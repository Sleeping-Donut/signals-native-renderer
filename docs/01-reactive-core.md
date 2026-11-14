# 01 — Reactive Core

**Crate:** `reactive-core`
**Depends on:** nothing (arena and ownership may be modules in v1)
**Owns:** node graph, dependency edges, dirty propagation, the compute heap,
signals, memos, effects, batching, and the read/write contract.

---

## 1. Goals

1. Fine-grained: a write wakes only the nodes that depend on it.
2. Glitch-free: no computation observes a mixture of old and new values within
   one flush.
3. Lazy: memos compute when needed or when scheduled, not eagerly on every
   write.
4. Predictable: one flush per frame, deterministic order, no hidden microtasks.
5. Solid 2.0–class semantics for batching, equality, ownership, and pending.
6. No `unsafe` in graph logic. The arena may use `unsafe` internally, isolated
   and Miri-tested.

## 2. What we port, and what we change

Solid 2.0's shipped core is an **alien-signals-style height heap with
DIRTY/CHECK marks** (`refs/solid/packages/signals/src/core/heap.ts`), not R3's
pull-verify traversal. R3 remains a useful benchmark and a source of ideas, but
it is not the target.

We adopt:

- intrusive dependency/subscriber edge lists (`graph.ts`);
- the height-indexed dirty heap and `markNode` propagation (`heap.ts`);
- staged writes promoted at flush (`F-10`);
- split effects: compute phase then apply phase (`signals.ts`);
- owner-scoped disposal and cleanup;
- the observable rules `F-1`, `F-2`, `F-3`, `F-4`, `F-5`, `F-9`, `F-10`, `F-12`
  for v1 (see [04-async-model.md](04-async-model.md)).

We do not adopt:

- JS object layout, `undefined` sentinels, property-mangling tricks;
- Proxy stores (see [03-store-layer.md](03-store-layer.md));
- microtask batching as the default (the host frame is the batch);
- reference-based resurrection of disposed computations (see
  [02-ownership-and-lifetimes.md](02-ownership-and-lifetimes.md)).

## 3. Node model

```rust
pub struct Node {
    pub id: NodeId,
    pub kind: NodeKind,
    pub owner: Option<OwnerId>,
    pub state: Cell<DirtyState>,
    pub height: Cell<u32>,
    pub flags: Cell<NodeFlags>,
    pub value: RefCell<ValueSlot>,
    pub deps: RefCell<EdgeList>,
    pub subs: RefCell<EdgeList>,
    pub async_state: RefCell<AsyncState>,
    pub cold: RefCell<Option<Box<ColdState>>>,
}

pub enum DirtyState { Clean, Check, Dirty }
```

`NodeFlags` includes at least:

| Flag | Meaning |
|---|---|
| `DISPOSED` | Slot is dead; a handle must not resolve. |
| `IN_HEAP` | Node is in the dirty heap. |
| `COMPUTING` | Compute pass in progress (cycle/debug guard). |
| `IS_EFFECT` | Apply phase exists. |
| `IS_RENDER` | Apply runs in the render queue. |
| `STAGED` | A value is staged for the next commit. |
| `HAS_ASYNC` | Cold async state exists. |
| `TRACKING` | Reads should create edges. |

Node kinds:

```rust
pub enum Compute<T> {
    Sync(Box<dyn FnMut(&Cx, Option<&T>) -> T>),
    Async(Box<dyn FnMut(&Cx) -> LocalBoxFuture<'static, Result<T, Error>>>),
}

pub enum NodeKind {
    Signal {
        // None for a plain writable source; Some for a derived writable signal.
        compute: Option<Compute<Box<dyn Any>>>,
    },
    Memo {
        compute: Compute<Box<dyn Any>>,
        equals: Equals,
    },
    Effect {
        compute: Box<dyn FnMut(&Cx) -> Box<dyn Any>>,
        apply: Box<dyn FnMut(&Cx, &dyn Any)>,
    },
    RenderEffect {
        apply: Box<dyn FnMut(&Cx, &dyn Any)>,
    },
}
```

Async is a **compute mode**, not a node kind. A `Signal` can be a plain source,
a derived writable signal, or an async derived writable signal; a `Memo` can be
sync or async. Nodes with an async compute have cold `AsyncState` (flight,
cancellation, staged landing); sync nodes never allocate or touch it. This keeps
the sync fast path clean while allowing any derived node to be async:

```rust
Memo::new(cx, |cx, prev| ...)          // sync derived
Memo::new_async(cx, |cx| ...)          // async derived
signal(cx, value)                      // plain source, split pair
derived_signal(cx, |cx, prev| ...)     // writable derived, split pair
async_derived_signal(cx, |cx| ...)             // writable derived, async, split pair
```

An **effect** in the graph sense has both halves. A **render effect** has only
an apply half because its compute result is the value it applies; the split still
matters because the compute runs in the heap and the apply runs after all
computes.

## 4. Edges

Edges are arena-allocated and intrusive. Each node keeps four cursor fields:

```rust
struct EdgeList { head: Option<EdgeId>, tail: Option<EdgeId> }

struct Edge {
    dep: NodeId,
    sub: NodeId,
    next_dep: Option<EdgeId>, prev_dep: Option<EdgeId>,
    next_sub: Option<EdgeId>, prev_sub: Option<EdgeId>,
}
```

Insertion order is preserved. Why:

- Nested effects depend on the outer effect running first (Leptos calls this out
  in `reactive_graph/src/graph/sets.rs`);
- Paint order and focus order in the view layer follow insertion order;
- Deterministic tests are much easier.

`link(dep, sub)` is idempotent within a pass: if the last dependency linked is
the same, or if the edge is already in the validated prefix of this pass, reuse
it. Solid's `link()` uses a per-pass generation stamp (`_depGen`/`_gen`) to avoid
an O(n) membership scan; we use the same idea with `Edge::validated_gen` and
`Node::dep_gen`.

`unlink(edge)` removes it from both lists in O(1). If the dependency's subscriber
list becomes empty and the node has `AUTO_DISPOSE`, it is queued for teardown at
the end of the pass (never during iteration).

## 5. Write path

### 5.1 Signal writes stage

```rust
pub fn set<T: 'static>(cx: &Cx, sig: Signal<T>, value: T) {
    let node = cx.node(sig.id());
    node.stage(value);
    cx.scheduler.batch().stage(node.id());
    cx.scheduler.schedule();
}
```

Rules:

- The value is stored in `pending_value`; the committed value is untouched.
- If a computation is currently running, this is a **write in owned scope** and
  is a debug error (see §9). The write is still staged in release to avoid
  corruption, and a diagnostic is emitted once.
- `set` twice before a flush replaces the staged value; only the last value
  matters unless a functional updater composes (`set(|v| v + 1)`), which reads
  the staged value for the writer's own composition channel.
- Equality is checked at promotion, not at `set`. This means a write that nets
  back to the committed value produces no dirty walk (`net to same value`).

### 5.2 Promotion at flush

At the start of a flush:

1. For each staged node in write order:
   - compare `pending` to `committed` with the node's equality;
   - if equal, drop the staging;
   - else move it to `committed`, set `state = Dirty`, and mark subscribers.
2. Clear the staged list.
3. Clear per-pass scratch state (`dep_gen` increments as each memo starts).

Marking:

```rust
fn mark(node: NodeId, new_state: DirtyState) {
    let n = &nodes[node];
    if n.state.get() >= new_state { return; }
    n.state.set(new_state);
    for edge in n.subs() {
        mark(edge.sub, DirtyState::Check);
    }
}
```

This is Solid's `markNode`. It is recursive in the reference; we implement it
iteratively with an explicit stack to avoid deep-tree stack overflow. The stack
holds `(NodeId, DirtyState)`; a node already at or above the requested state is
skipped. Effects are subscribers like any other node; they enter the heap and
their apply is queued after the compute phase.

### 5.3 Functional updaters and drafts

The writer's own composition channels see the staged value:

- `set(|prev| ...)` receives the staged value if one exists, else committed.
- A store draft sees the staged writes of the same tick (see
  [03-store-layer.md](03-store-layer.md)).
- Nothing else sees the write.

## 6. Read path

```rust
pub fn read<T: 'static>(cx: &Cx, sig: Signal<T>) -> T
where T: Clone
```

In practice we expose guard APIs to avoid clones:

```rust
pub fn with<T: 'static, R>(cx: &Cx, sig: Signal<T>, f: impl FnOnce(&T) -> R) -> R
pub fn with_memo<T: 'static, R>(cx: &Cx, memo: Memo<T>, f: impl FnOnce(&T) -> R) -> R
```

Read algorithm:

1. Resolve the handle. If disposed, return `Err(Disposed)`.
2. If the node is a signal:
   - return committed value (never staged; `F-10`);
   - if an observer is current and tracking is on, link dep → observer.
3. If the node is a memo:
   - if `state == Dirty` or `Check`, ensure it is up to date:
     - `Dirty`: recompute now (pull);
     - `Check`: verify dependencies; if any changed, recompute; else become
       `Clean`.
   - link if tracked;
   - return committed value.
4. If the node is an effect, reading it is an error (effects have no value) unless
   the read is diagnostic.

### 6.1 Pull verification

The heap already runs in height order, but a pull can happen outside the heap
(a user reads a memo from an event handler). `ensure_clean` is:

```rust
fn ensure_clean(node: NodeId) {
    match state {
        Clean => {}
        Dirty => recompute(node),
        Check => {
            for dep in deps(node) {
                ensure_clean(dep);
                if state == Dirty { break; }
            }
            if state == Check { state = Clean; }
            else { recompute(node); }
        }
    }
}
```

This is the lazy half of the push-pull model. It guarantees a read never returns
a stale memo even if the node was not in the heap.

### 6.2 Tracking

`Cx` holds `observer: Option<NodeId>` and `tracking: bool`.

- `read` links only if `tracking` and a non-disposed observer exist.
- `untrack(f)` saves the observer, sets `tracking = false`, runs `f`, restores.
- `tracking = true` is the default inside a compute pass.
- Effect apply halves run untracked. This is essential: an apply that reads a
  signal must not subscribe to it (Solid 2.0's split effects; Leptos'
  `with_observer_untracked`).

## 7. Compute heap

### 7.1 Why a heap

A simple queue runs in insertion order, which can execute a subscriber before
its dependency and force a pull/recompute cascade. A recursive height walk is
correct but risks stack overflow and does not coalesce duplicate spawns well.

The heap is indexed by `height` (Solid's `Heap` with `_heap: Vec<Option<Node>>`
plus per-height linked lists):

```rust
struct Heap {
    levels: Vec<Option<NodeId>>,
    min: u32,
    max: u32,
    marked: bool,
}
```

`insert(node)` adds it at its height. `run()` processes `min..=max` in order.
Within a level, insertion order is preserved. If a node's height is stale, it is
removed and reinserted at the corrected height before it runs
(`adjust_height`, from Solid's `adjustHeight`).

### 7.2 Height assignment

When a node links a dependency during a compute pass:

```rust
node.height = max(node.height, dep.height + 1);
```

When a node's height changes while it is already in the heap, remove and
reinsert it (and reinsert its subscribers if their invariant is now violated).
Solid's `insertIntoHeap` reuses `IN_HEAP`/`IN_HEAP_HEIGHT` flags to avoid
duplicates.

Heights are a heuristic for order, not a proof of topology. `Check` verification
is the correctness net: if a node runs too early, its `Check` state forces it to
ensure dependencies are clean before recompute.

### 7.3 Effect phases

The heap runs compute passes only.

- For a memo: compute and stage the new value (commit at end of pass).
- For an effect: compute the value, then enqueue the apply.
- For a render effect: compute, then enqueue the apply in the render queue.

After the heap drains:

1. Commit staged values and trim dependencies of unfinished/unchanged passes.
2. Run render-effect applies in queue order.
3. Run user-effect applies in queue order.

This ordering is the Solid 2.0 `EFFECT_RENDER`, `EFFECT_USER` split and is
required for boundaries and async holds.

### 7.4 Run-to-fixpoint

An effect apply may stage a write. The flush loop continues until no staged
writes or dirty nodes remain, with a safety bound (default 10,000 rounds) that
raises `PotentialInfiniteLoop` with the last staged node's name. In dev, a write
inside an owned scope is already an error, so a loop usually indicates a
legitimate feedback loop in event-driven code.

## 8. Effects

### 8.1 API

```rust
pub fn effect(cx: &Cx, compute: impl FnMut(&Cx) -> T + 'static,
              apply: impl FnMut(&Cx, &T) + 'static) -> EffectHandle
where T: 'static;

pub fn render_effect(cx: &Cx, compute: impl FnMut(&Cx) -> T + 'static,
                     apply: impl FnMut(&Cx, &T) + 'static) -> EffectHandle
where T: 'static;
```

`EffectHandle` is an RAII guard: dropping it disposes the effect and unlinks its
edges. View nodes store their `EffectHandle`s; removing a node drops the guards.

### 8.2 First run

- `effect` runs its compute on the next flush, not at creation, unless
  `immediate` is requested. This matches Solid's split effect and Leptos'
  `Effect` (it notifies once at creation to run on the next tick).
- `render_effect` runs compute and apply synchronously at creation, so a view
  node has its initial value before the first paint. Subsequent runs enqueue
  their applies in the render queue. This is used by bindings that must exist
  before the first frame.

### 8.3 Cleanup

An effect's apply may return a cleanup. The cleanup runs:

- before the next apply,
- on effect disposal,
- on owner disposal.

Cleanups run in LIFO order within an owner, children before self.

### 8.4 Effect ordering

Compute passes run in heap order (height, then insertion). Applies run in the
order they were enqueued. A parent binding created before a child binding applies
before it. If an apply for parent and child both write the same view node, the
child wins by creation order. This is deterministic and documented; it is not a
general dependency guarantee.

## 9. Write discipline and cycles

### 9.1 The rule

A write to a signal inside a compute pass is a bug. It is rejected because:

- it creates an edge from the written signal to the writing computation, which
  can create a cycle;
- it makes the value of a computation depend on its own output;
- it breaks the topological heap's assumptions.

We allow writes in:

- event handlers (outside any computation),
- effect apply halves, but only for signals marked `owned_write` (internal
  state) or via `cx.defer_write` which stages the write for the next flush;
- `on_settled` callbacks, which run after the frame is decided.

The check is:

```rust
if cx.in_compute() && !signal.permits_owned_write() {
    dev_error!(WriteInOwnedScope, owner_path(cx));
    // release: stage and continue
}
```

### 9.2 Cycle detection

In debug builds, when `link(dep, sub)` is about to add an edge and `sub` is
currently `COMPUTING`, walk `sub`'s dependency ancestors; if `dep` is reachable,
report `DependencyCycle` with both owner paths. The walk is bounded (e.g. 64
nodes) and only runs in debug.

In release, the write discipline and the acyclic nature of reads make cycles
impossible through normal APIs. A cycle can only be introduced by a
`defer_write` feedback loop; the flush round limit catches it.

### 9.3 Why not 3-color DFS always

A per-edge DFS is O(V+E) on every link, which is too expensive for a hot path
that may add thousands of edges during a mount. The write discipline gives
acyclicity by construction; the debug walk gives a precise error when a bug
slips through.

## 10. Batching and flush

```rust
pub fn flush(cx: &mut Cx) -> Result<(), FlushError>;
```

- `flush` is idempotent: a second call with no work is a no-op.
- A flush is not reentrant. Calling it inside a compute/apply is a dev error.
- `flush(fn)` runs `fn` in a synchronous scope then flushes; the callback's
  return value is preserved.
- The runtime calls `flush` once per frame, at phase 2.
- Tests call `runtime.flush()` or `runtime.frame(FrameCause::Manual)`.

Flush algorithm:

```
loop {
    if no staged writes and heap empty and no queued applies: break
    promote staged writes
    mark dirty
    run heap (computes)
    settle holds (async model)
    run render applies
    run user applies
}
```

The async settle step is a no-op when async is not compiled or no flights are
outstanding. Its details live in [04-async-model.md](04-async-model.md).

### 10.1 Why frame batching

| Model | Latency / determinism | Cost |
|---|---|---|
| Immediate write | Every write marks and may recompute; no glitch freedom across a multi-write change. | Simple reads, but tears and re-runs. |
| Microtask batching (Solid 2.0) | Coalesces within a JS event-loop turn; effect timing follows microtasks. | Good for JS, but an executor-turn concept in Rust; can flush at surprising points. |
| **Frame checkpoint (chosen)** | One flush and one paint per frame; async holds and layout share the same boundary. | Read-after-write needs `flush(fn)`; non-frame contexts need an explicit flush. |
| Explicit `batch(fn)` | Groups writes and flushes at the end. | Largely redundant when the tick is already the batch; nested-scope rules. |
| Per-signal synchronous flush | Immediate visibility. | No cross-write consistency; expensive. |

Frame batching is the only model that makes `F-1` (the screen is whole), async
holds, and one paint per frame fall out of the same checkpoint. It also makes
tests deterministic. The price is that imperative code must call `flush(fn)` (or
run a frame) when it needs to read its own write. There is no separate `batch`
API in v1: the tick is the batch.

## 11. Equality

```rust
pub enum Equals {
    Always,                         // never consider equal (always notify)
    Never,                          // consider all values equal (never notify)
    PartialEq,                      // T: PartialEq
    Custom(Box<dyn Fn(&dyn Any, &dyn Any) -> bool>),
}
```

Default for `T: PartialEq` is `PartialEq`; for non-`PartialEq` types, use
`Equals::Always` (notify every write) or wrap in a newtype with a custom
comparator. The default is chosen at the constructor via a trait bound:

```rust
let (read, write) = signal(cx, value);          // requires T: PartialEq
let (read, write) = signal_unchecked(cx, value);// no equality, always notify
let (read, write) = signal_with(cx, value, equals); // custom
```

A comparator panic is a compute error: the value is replaced by the error, and
the error boundary / root error hook handles it (Solid's A3 rule).

## 12. API surface (v1)

```rust
// Sources — split by default
let (count, set_count) = signal(cx, v);                 // ReadSignal<T>, WriteSignal<T>
let (value, set_value) = derived_signal(cx, |cx, prev| ...); // writable derived
let (value, set_value) = async_derived_signal(cx, |cx| ...);         // writable derived, async
count.get(cx) / count.is_pending(cx)
set_count.set(cx, v) / set_count.update(cx, |v| ...)

// Derivations (read-only)
Memo::new(cx, |cx, prev| ...) -> Memo<T>         // sync
Memo::new_async(cx, |cx| ...) -> Memo<T>         // async
Memo::new_lazy(cx, |cx, prev| ...) -> Memo<T>
memo.get(cx) / memo.is_pending(cx)
memo.with(cx, |v| ...)

// Effects
effect(cx, compute, apply) -> EffectHandle
render_effect(cx, compute, apply) -> EffectHandle
effect_once(cx, apply) -> EffectHandle      // no compute value; runs once
on_cleanup(cx, f)

// Control
untrack(cx, f)
flush(cx)
flush(cx, f)                                 // run f, then flush synchronously
```

There is no combined public signal handle and no `RwSignal`. `signal()`,
`derived_signal()`, and `async_derived_signal()` return a `(ReadSignal<T>,
WriteSignal<T>)` pair. `Signal<T>` in the node-model sections is the internal
full node handle; application and library code uses the halves.

Every reactive operation takes `&Cx`. The context resolves the runtime and
current owner; there is no global graph. The `view!` macro has `cx` in scope, so
application code rarely writes it. `Prop::Static` needs no `cx`. Helper
functions take plain values, so they do not thread `cx` through.

`ReadSignal<T>` and `WriteSignal<T>` are differentiated by capability, not by
sync/async. Async is a compute mode: `Memo::new_async` and `async_derived_signal` return
the same `ReadSignal`/`WriteSignal`/`Memo` types as their sync counterparts.
`AsyncMemo<T>` is not a separate type.

**Split is the default.** There is no combined public signal and no `RwSignal`.
Every writable source is created as a pair:

```rust
let (read, write) = signal(cx, 0);
// read:  ReadSignal<i32>   — get/with/is_pending, no set
// write: WriteSignal<i32>  — set/update, no get
```

`ReadSignal`/`WriteSignal` are `Copy` views over the same node: no allocation, no
runtime cost, no separate graph. `Memo<T>` and `Prop<T>` are read-only. Use
`ReadSignal<T>` in a component's public API and keep `WriteSignal<T>` private to
the primitive; the compiler then prevents downstream writes. The cost is two
handles to thread and tuple returns; the benefit is least privilege and explicit
API boundaries. Solid (`[get, set]`) and Leptos (`signal() ->
(ReadSignal, WriteSignal)`) both default to the split shape.

```rust
#[derive(Clone, Copy)]
struct CounterPrimitive {
    pub count: ReadSignal<i32>,
    increment: WriteSignal<i32>,
}

fn create_counter(cx: &Cx, initial: i32) -> CounterPrimitive {
    let (count, increment) = signal(cx, initial);
    CounterPrimitive { count, increment }
}

impl CounterPrimitive {
    fn increment(&self, cx: &Cx) {
        self.increment.update(cx, |c| *c += 1);
    }
}
```

`ReadSignal` and `Memo` are both read-only handles, but they point at different
node kinds:

| | `ReadSignal<T>` | `Memo<T>` |
|---|---|---|
| Write capability | None on this handle; the paired `WriteSignal` may exist | None, ever |
| Node | Read half of a signal node (plain or derived writable) | Derived memo node |
| Caching | The source value; plain signals do not recompute | Computed value cached; recomputes on dependency change |
| Equality | Signal equality on writes | Memo equality; equal results do not notify downstream |
| Async | Can be the read half of an async derived signal | Can be an async memo |
| Consumer view | `get`/`is_pending`/`with` | `get`/`is_pending`/`with` |

At the consumer boundary they are equivalent accessors: both implement the same
`Read<T>` behavior. The difference is the underlying node's caching/equality and
whether a write half exists. Props can therefore take either; `Prop<T>` uses a
read handle, not a node kind.

`Memo::new_lazy` defers the first computation until first read and disposes when
unobserved, matching Solid's `lazy` option. This is useful for expensive
derivations in branches that may never mount.

## 13. Performance notes

- Node literal: keep under ~40 fields; put async/diagnostic data behind a cold
  pointer. Solid measured a creation cliff past the V8 in-object limit; Rust does
  not have that exact cliff, but cache lines still matter.
- Edge lists: no allocation after the first few edges (`SmallVec` inline for
  `EdgeList` cursors only; edges themselves are arena-allocated).
- Dirty walk: iterative, no `Vec` allocation in the common case; reuse a scratch
  stack in the scheduler.
- Heights: `u32`, capped; a height above 65,535 is clamped and the node is
  verified by `Check` pull.
- Read fast path: resolve handle + committed value + link. Avoid `RefCell`
  double-borrow by storing the value in a `Cell` for `Copy` types later.
- Do not clone `Vec<Subscriber>` on mark (Leptos does this); the intrusive list
  iterates in place. If a subscriber removes itself during iteration, the walk
  restarts from the saved next edge cursor.

## 14. Tests

Port these from the reference material:

| Test family | Source | What it proves |
|---|---|---|
| Graph basics | Solid `packages/signals/tests` (selected), Leptos `reactive_graph/tests/signal.rs`, `memo.rs`, `effect.rs` | read/write, equality, nested memos, untrack. |
| Glitch freedom | Solid fundamentals `F-1`; a diamond with two paths to one effect | the effect sees a consistent pair. |
| Batching | `F-10` | reads before flush see the old value; after flush see the new. |
| Equality | Leptos `hash_eq_contract.rs`, Solid `createMemo.test.ts` | equal values do not notify. |
| Disposal | Solid `owner.ts` tests, Leptos `cleanup.rs` | child-before-parent, LIFO cleanups. |
| Ordering | Leptos `effect.rs` | nested effects run outer-first. |
| Cycle guard | new | write in compute is rejected; debug cycle walk. |
| Heap stress | new | 100k nodes, random writes, no stack overflow, bounded time. |
| Miri | new | arena + edge unlink under stacked borrows. |

Property tests:

- random DAGs: after any sequence of writes and flushes, every memo's value equals
  recomputing from scratch;
- random dispose/read sequences: no disposed handle resolves, no use-after-free;
- write coalescing: N writes to one signal produce ≤1 recompute of each dependent
  per flush.

## 15. Open questions

1. Should `signal()` require `T: PartialEq` by default, or should equality be
   explicit? (Leaning: `PartialEq` default, `signal_unchecked` escape.)
2. Do we expose `Memo<T>` values by guard or by clone? Guards are faster but can
   conflict with writes; clone is simpler for users. (Leaning: both, with guards
   for internal code and `Clone` convenience for app code.)
3. Is `flush(fn)` enough for imperative bursts, or do we need a separate batch
   scope? (Leaning: no separate `batch`; the tick is the batch, as in Solid 2.0.)
4. Do effects need a visible `EffectHandle` in the public API, or is owner-scoped
   disposal enough? Renderer bindings need explicit handles; app effects can be
   owner-scoped. (Leaning: both.)
5. How do we expose diagnostics without paying for them in release? A `diagnostics`
   feature that compiles owner paths and node names; default on in dev, off in
   release.
