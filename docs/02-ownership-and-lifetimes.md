# 02 — Ownership and Lifetimes

**Crate:** `reactive-core` (module `ownership` in v1; extract to
`reactive-ownership` if it grows)
**Owns:** arenas, generational handles, the owner tree, cleanup, context, RAII
guards, and memory reclamation.

---

## 1. The problem

Rust makes lifetime errors compile errors, but a UI framework needs values that
outlive the function that created them and die when a subtree is removed. The
reference systems solve this differently:

- **Solid** uses JS garbage collection plus an owner tree for cleanup. A copied
  reference can keep a disposed computation reachable; the docs call the result
  a reclamation gap.
- **Leptos** uses `Owner` (an `Arc<RwLock<...>>` tree) plus a global `SlotMap`
  arena for values. The `Owner` drops arena entries on cleanup.
- **GPUI** uses `Entity<T>` handles with reference counts plus an `EntityMap`
  arena. The app owns state; handles are inert IDs and fail after release.

We choose the GPUI/Leptos half that is Rust-native: **arena + generational
handles + owner tree**, with a stricter rule than Solid: **disposed means
unreachable**. This turns the reclamation gap into an ordinary invalid-handle
error.

## 2. Arenas

### 2.1 Slot

```rust
struct Slot<T> {
    generation: u32,
    value: Option<T>,
}

pub struct Arena<T> {
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
    len: usize,
}
```

- `insert` pops a free index or pushes a new slot; generation is retained from
  the slot (bumped on removal).
- `remove(id)` checks `generation`; on success it takes the value, bumps the
  generation, and pushes the index to `free`.
- `get(id)` returns `None` on generation mismatch or empty slot.
- Generation wraps at `u32::MAX`; on wrap, skip the slot (do not reuse) to avoid
  ABA. In practice a slot would need 4 billion reuse cycles.

Arena operations are the only `unsafe`-eligible code, and v1 can implement them
with `Vec<Option<T>>` and safe indexing. Measure before adding `unsafe`.

### 2.2 Multiple arenas

Use separate arenas per type:

| Arena | Value | Why separate |
|---|---|---|
| `nodes` | `Node` | largest, hottest |
| `edges` | `Edge` | many, small, freed often |
| `owners` | `Owner` | tree shape differs |
| `views` | `ViewNode` | renderer-only |
| `text` | shaped lines | cache eviction differs |

A single heterogeneous `Box<dyn Any>` arena would simplify handles but adds a
downcast on every access and prevents type-specific free lists. Keep them
separate.

### 2.3 Handle validity

```rust
pub struct Handle<T> {
    id: NodeId,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Handle<T> {
    pub fn id(self) -> NodeId { self.id }
    pub fn is_alive(self, cx: &Cx) -> bool { cx.nodes.contains(self.id) }
}
```

Handles are `Copy`, `Eq`, `Hash`, and `'static`. They are not references and do
not keep anything alive. Resolving a dead handle is a recoverable error:

```rust
pub enum AccessError { Disposed, WrongKind }
```

Public convenience methods (`get`, `set`) panic in debug with a diagnostic and
return the last known/placeholder value in release. Internal renderer code uses
`try_get` and treats `Disposed` as "remove the binding".

## 3. The owner tree

### 3.1 Shape

```rust
pub struct Owner {
    pub id: OwnerId,
    pub parent: Option<OwnerId>,
    pub first_child: Option<OwnerId>,
    pub last_child: Option<OwnerId>,
    pub next_sibling: Option<OwnerId>,
    pub prev_sibling: Option<OwnerId>,

    pub cleanups: Vec<Cleanup>,
    pub context: TypeMap,
    pub nodes: Vec<NodeId>,       // graph nodes created in this scope
    pub views: Vec<ViewNodeId>,   // view nodes created in this scope
    pub disposed: bool,
}
```

- Every effect, memo, and component has an owner. Signals created inside a
  computation belong to the computation's owner.
- `Owner::new()` links to the current owner; `run_with_owner` sets and restores
  the current owner.
- A root has no parent and is disposed explicitly by the runtime or a window.

### 3.2 Re-run semantics

When an effect or memo recomputes:

1. Set the current owner to its owner.
2. Dispose the owner's **previous children** (depth-first) and run their
   cleanups. This drops signals, effects, and view nodes created by the previous
   pass.
3. Reset the owner's `context`? **No.** Context provided by an ancestor is
   inherited; context provided by the computation itself is re-provided each run.
   A per-run context map is cleared, but the parent chain remains.
4. Run the compute function. New children register under the same owner.
5. On completion, restore the previous owner.

This is the key difference from a plain arena: re-running a computation is also
a disposal of everything it created last time. It gives the "GC by UI lifetime"
property Leptos describes in `reactive_graph/src/owner.rs`.

### 3.3 Disposal order

`dispose(owner)`:

```
for child in children (in order):
    dispose(child)
run cleanups in reverse registration order (LIFO)
drop owner context
drop owner's node/view references (which may trigger their own disposal)
mark disposed, bump generation, return slot to free list
```

Why children first: a child effect may depend on a value the parent created; the
parent's cleanup should not run while the child still exists. Why LIFO: a cleanup
registered later may depend on a resource created earlier. This matches Solid
2.0's unwind order in `documentation/solid-2.0/02-signals-derived-ownership.md`.

### 3.4 Cleanup registration

```rust
pub fn on_cleanup(cx: &Cx, f: impl FnOnce() + 'static) -> CleanupHandle
```

`CleanupHandle` removes the cleanup when dropped early (e.g. a resource that was
already released). Cleanup callbacks may not panic; a panic is caught, reported,
and the remaining cleanups still run (Solid's "cleanup throw continues" rule).

## 4. Subscription guards

Bindings and user effects are stored as guards, not raw IDs:

```rust
#[must_use = "dropping the guard disposes the effect"]
pub struct EffectHandle {
    id: NodeId,
    owner: Option<OwnerId>,
}

impl Drop for EffectHandle {
    fn drop(&mut self) { /* dispose effect, unlink edges, run cleanup */ }
}
```

- A view node stores `SmallVec<[EffectHandle; 4]>` for its bindings. Removing the
  view node drops the guards; the effects disappear before the node slot is
  freed.
- A user effect can be `detach()`ed (leaked intentionally) or stored in an
  owner. Detaching is explicit, matching Solid's `runWithOwner(null, ...)`.
- Dropping a guard during a flush removes the effect from the heap and queues
  edges for unlink at the end of the pass, never during iteration.

## 5. Reclamation

### 5.1 What is reclaimed

- The node's value and closures are dropped on removal.
- Its edges are unlinked and their slots returned to the edge free list.
- Its owner, if still alive, removes it from `owner.nodes`.
- Its slot is reusable immediately with a new generation.

There is no separate "unreachable but not reclaimed" set. Unlike Solid, a copied
handle does not keep the value alive; it only keeps an index that will fail to
resolve after disposal.

### 5.2 What can still grow

- **Live nodes**: unavoidable.
- **Free slots**: bounded by the high-water mark of concurrent live nodes. The
  free list reuses them; it does not shrink automatically.
- **Owner children lists**: pruned on reallocation (Leptos does this in
  `Owner::register` by retaining only live arena entries). We do the same, with
  an amortized `retain` when a `Vec` is about to grow.
- **Diagnostics**: if enabled, names and source locations are retained. Disable
  in release or cap the ring.

A 24-hour churn test is the acceptance check: live nodes/owners/edges return to a
baseline after the app goes idle, and free slots stabilize.

### 5.3 Why no compaction

Compaction would require rewriting every `NodeId` held by users, bindings, and
edges. Since slots are reused through a free list and values are dropped at
disposal, compaction buys memory only when the high-water mark is much larger
than the live set and the allocator cannot reuse the slots. Measure that case
before building it. If needed, compaction can be a `runtime.shrink()` at a safe
point (no user-held handles; only opaque handles), which is exactly the
constraint the original outline identified.

### 5.4 The "resurrection" difference

In Solid, a memo created under a disposed owner can sometimes be revived by a
later read because the JS object is still reachable. In Rust we make this an
error:

```rust
match memo.try_get(cx) {
    Ok(v) => ...,
    Err(AccessError::Disposed) => // binding is stale; remove it
}
```

This is a semantic improvement for a native framework: disposed means disposed.
It also removes the need for a tracing GC. Document it prominently for users
coming from Solid.

## 6. Context

```rust
pub fn provide_context<T: 'static>(cx: &mut Cx, value: T);
pub fn use_context<T: Clone + 'static>(cx: &Cx) -> Option<T>;
pub fn expect_context<T: Clone + 'static>(cx: &Cx) -> T; // panics/errors if absent
```

- Context is stored on the owner and searched up the parent chain.
- A default-less context that is missing returns `None`; `expect_context` is the
  asserting form. We do not throw a special `ContextNotFoundError` by default;
  an optional error type can wrap it for app code.
- Context values may be signals/memos; reading them inside a binding tracks them
  normally.
- Context is per-runtime and per-owner, not a global. This makes tests and
  multiple windows isolate cleanly.

## 7. Interaction with async

- A flight is owned by the node's owner. Disposing the owner cancels the flight's
  registration; the future may still run on the host, but its completion is
  dropped (generation mismatch) and never applied.
- A node that is pending because of a flight is not "kept alive" by the flight.
  Disposal is allowed; the flight becomes inert.
- A boundary owner owns its fallback/content subtrees; switching to the fallback
  disposes the content's bindings but may keep the content's *state* if the
  design says so. In our v1, content is disposed on fallback entry and rebuilt on
  reveal; this is simpler and predictable. (Solid keeps it alive but off screen;
  see [04-async-model.md](04-async-model.md) for the trade.)
- `on_cleanup` runs on cancellation if the async body registered it; the
  framework does not run user futures to completion on disposal.

## 8. View-tree ownership

The view tree follows the same owner tree:

- A component mount creates an owner.
- Every view node created during the mount is registered on that owner.
- Removing a component disposes its owner, which drops the view nodes and their
  binding guards.
- A keyed list row has its own owner. Reordering rows does not dispose owners;
  removing a row does.
- A conditional (`Show`) owns its active branch. Flipping the condition disposes
  the old branch owner and mounts the new one. If the branch should be kept
  alive (e.g. a tab that must preserve scroll), use a keyed list of persistent
  branches or a `KeepAlive` wrapper that moves the owner instead of disposing.

## 9. API

```rust
// Roots
let root = Owner::new_root(cx);
let handle = root.run(|cx| { ... });

// Scopes
let owner = Owner::new(cx);            // child of current
cx.with_owner(owner, |cx| { ... });    // set current for the closure
owner.dispose(cx);

// Cleanup
let c = on_cleanup(cx, || { ... });
drop(c);                                // early removal

// Guards
let effect = effect(cx, compute, apply);
effect.detach();                        // intentional leak
// drop(effect) disposes

// Context
provide_context(cx, Theme::Dark);
let theme = use_context::<Theme>(cx);
```

## 10. Tests

- **Arena**: insert/remove/reuse; stale handle fails; generation wraps safely;
  10M operations under Miri.
- **Owner order**: parent/child/sibling disposal order; cleanups LIFO; a cleanup
  registered by a child runs before the parent's.
- **Re-run**: an effect that creates a nested effect and a signal each run leaves
  no live children from previous runs; the arena live count is stable across
  10k runs.
- **Disposal during flush**: dropping a guard inside an apply defers unlink to
  the pass end; no iterator invalidation.
- **Panic in cleanup**: remaining cleanups still run; the panic is reported.
- **Context**: nearest ancestor wins; missing context is `None`; context is not
  shared between runtimes.
- **Async cancellation**: disposing a pending node drops its completion; a late
  landing does not touch the graph.
- **Leak soak**: 24h churn with screenshots/alloc snapshots; live counts return
  to baseline.

## 11. Open questions

1. Should `Owner` be `Send`? No in v1 (UI thread). A background owner would need
   a separate graph or a message boundary.
2. Do we expose `OwnerId` publicly? Prefer opaque `Scope` handles; `OwnerId` for
   diagnostics only.
3. Should disposing a component keep its scroll/focus state for a later remount?
   GPUI keeps element state by `GlobalElementId` across frames; our retained
   tree keeps it while the node lives. For remount, a `KeepAlive` cache is the
   explicit answer.
4. How aggressively should `Owner::nodes` prune? Leptos prunes at capacity; we
   can do the same, plus an explicit `compact()` for long-lived roots.
5. Is a panic in a cleanup a halt or a report? Solid continues; we continue and
   report, with `REACTIVITY_HALTED` only for an uncaught compute/apply error.
