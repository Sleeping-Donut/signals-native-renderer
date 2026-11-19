# 12 — Decisions, Risks, and Open Questions

This document is the ADR log and the risk register. It is append-only in spirit:
when a decision changes, add a new ADR that supersedes the old one; do not rewrite
history silently. The short table in [PLAN.md](../PLAN.md) §5 is the index.

---

## Part A — Architecture decisions

### ADR-001: Solid 2.0's rules are the oracle, not its API

**Status:** Accepted.

**Context.** Solid 2.0's `main` branch (the 2.0 release line) contains a large,
heavily tested async runtime. Its rules (`F-<n>`, `D-<n>`, `S-<n>`) describe
observable behavior: frame integrity, holds, boundaries, pending, flush
visibility. Its API is shaped by JavaScript (getters, proxies, microtasks,
exceptions for control flow).

**Decision.** Port the behavioral rules that matter for a native UI, in a Rust
API. Use the rule IDs in our docs and tests so a future reader can trace a
semantic decision back to its source.

**Consequences.**

- We can validate against a real, maintained oracle rather than inventing
  semantics.
- We must explicitly document deviations (Result vs throw, no resurrection,
  mount-once, v1 subset).
- We are not bound to Solid's release schedule or API.

**Alternatives.** A "signals-like" core with no oracle (fast to build, impossible
to know when correct); an exact Solid API port (fights Rust, inherits JS
artifacts).

---

### ADR-002: Height-indexed dirty heap, not R3 pull-verify

**Status:** Accepted.

**Context.** The original outline proposed R3's push-pull, lazy two-phase
graph-coloring algorithm. Solid 2.0's shipped core uses a height-indexed heap
with DIRTY/CHECK marks (`refs/solid/packages/signals/src/core/heap.ts`), derived
from alien-signals. R3 is a separate engine with different trade-offs.

**Decision.** Implement the height heap. Keep R3 as a benchmark and a source of
ideas, not the target.

**Why.**

- It is the algorithm the behavioral oracle actually ships, so rule alignment is
  direct.
- It gives topological execution without recursion and without re-walking clean
  paths.
- It supports effects and render effects in the same scheduling structure.
- It is simpler to make deterministic than a pull-verify engine with a separate
  stack.

**Consequences.** We own height maintenance and `Check` verification. The heap is
not a proof of topology; `Check` is the correctness net. We should benchmark
against R3 and Alien Signals and be honest if another engine wins on a shape.

**Alternatives.** R3 (elegant, but not what the oracle ships); plain stack
(cheap, but order-sensitive); recursive height walk (simple, stack risk).

---

### ADR-003: Arena with generational handles; no resurrection

**Status:** Accepted.

**Context.** Solid relies on GC reachability: a disposed computation can be
revived by a copied reference. Leptos uses `Arc<RwLock<...>>` plus a slotmap.
GPUI uses an entity map with reference-counted handles.

**Decision.** Use arena slots with generations. A handle is a lookup key, not a
reference. Disposal drops values and invalidates the handle. A disposed handle
never resolves.

**Why.**

- No `Rc` cycles, no tracing GC, no resurrection ambiguity.
- Stable identity for view nodes and graph nodes.
- O(1) access, no lock per node.
- Stale handles fail deterministically, which is what a renderer wants.

**Consequences.**

- Public APIs must handle `Disposed` errors or panic in debug.
- Users cannot keep a computation alive by copying a handle; they must use an
  owner or an explicit `detach`.
- No compaction needed; free-list reuse bounds memory.
- This is a documented semantic deviation from Solid.

**Alternatives.** `Arc<RwLock>` per node (allocation, lock, poisoning, slow);
raw indices (ABA); leaked boxes (memory growth).

---

### ADR-004: Writes stage; the flush promotes

**Status:** Accepted.

**Context.** `F-10` says a write becomes visible at flush to every channel. This
is load-bearing for `latest`, `isPending`, and holds. Retrofitting it later would
touch every read path and every store.

**Decision.** A write stores a pending value. Readers see the committed value
until the flush. Functional updaters and drafts see the staged value for the
writer's own composition. Promotion applies equality and marks dirty.

**Consequences.**

- `set` then read in the same event handler returns the old value; users call
  `flush()` or `runtime.frame()` to see it.
- Tests must be explicit about flush boundaries.
- Async landings are staged, never committed directly.

**Alternatives.** Eager writes (simple, but breaks holds and tears frames).

---

### ADR-005: Async is Phase 2, not Phase 6

**Status:** Accepted.

**Context.** The outline placed async after the renderer. But flight state,
staged values, reader classes, and holds determine the node shape and the
scheduler. A renderer built before them would encode assumptions that later
break.

**Decision.** Build the reactive core and the async hold model before the
renderer. The renderer consumes a settled graph.

**Consequences.**

- The first demo is headless (null renderer), not visual.
- Phase 2 is the riskiest phase; it gets the most design attention and the
  earliest fuzzing.
- The renderer can be simpler because it never sees `NotReady` except at
  boundary edges.

**Alternatives.** Renderer-first (faster visible progress, high rewrite risk).

---

### ADR-006: Retained nodes with bindings, plus rebuild escape hatch

**Status:** Accepted.

**Context.** GPUI retains views but rebuilds elements every frame and caches
per-view paint ranges. A fully retained element tree with one binding per
property is the finest-grained option but makes components "build once, bind
forever" and complicates structure. A virtual DOM is the familiar middle ground
but pays a diff tax and loses identity.

**Decision.** Retain view nodes and structure, build them at mount, and bind
dynamic properties with effects. Provide `Rebuild` for subtrees where binding is
more expensive than re-mounting.

**Consequences.**

- One signal write updates one text run; no subtree re-render.
- Components run once; the mental model must be taught.
- `Show`/`For` are the only structural mutations, and they are explicit.
- Keyed lists are required for row state.
- The renderer can cache layout and paint per node.

**Alternatives.** GPUI per-view rebuild (coarse, proven); virtual DOM (diff tax,
no stable node identity); fully immediate mode (no retained state).

---

### ADR-007: Full layout on layout-dirty frames; incremental later

**Status:** Accepted, with a revisit trigger.

**Context.** Taffy does not expose a general incremental layout API. GPUI clears
and rebuilds its taffy tree every draw. Retaining the taffy tree and skipping
layout on paint-only frames is a large win; true dirty-subtree layout is research.

**Decision.** v1 keeps a persistent taffy tree, runs layout only when
layout-dirty, and uses layout roots (fixed-size/contained subtrees) as the
incremental unit. Do not promise dirty-subtree flex/grid layout in v1.

**Consequences.**

- Paint-only updates are cheap.
- A layout change may relayout a large subtree.
- If layout exceeds 30% of frame time in the demo, prototype dirty-subtree
  layout before Phase 8.

**Alternatives.** Full layout every frame (GPUI, simpler, slower); custom layout
engine (too large).

---

### ADR-008: wgpu for the first-party backend

**Status:** Accepted, with a native-backend escape hatch.

**Context.** GPUI uses platform-specific backends (Metal, DirectX, Blade) for
maximum performance. We need cross-platform reach and a testable offscreen path.
"wgpu vs native" is not "portable API vs Metal/Vulkan"; wgpu is a portable front
end that uses Metal, Vulkan, or DX12 underneath. The real trade-off is control
and overhead, not API availability.

| Option | Pros | Cons |
|---|---|---|
| **wgpu (chosen)** | One API, one shader language (WGSL), Metal/Vulkan/DX12/WebGPU, offscreen testing, faster to first pixel | Runtime validation/command encoding, less control over present/partial damage, driver quirks, heavier dependency |
| GPUI-style native backends | Lowest overhead, platform features (IOSurface, partial present, display link), best latency | 3× implementation, shaders, testing, unsafe/platform code; large team commitment |
| CPU (`tiny-skia`) | Simple, deterministic, no GPU risk | Slow for large scenes; not the flagship |

**Decision.** Use `wgpu` for v1 behind a `PaintBackend` trait. The scene,
paint-cache, and batching layers are backend-agnostic, so a native Metal/Vulkan/
DX12 backend is an additive change, not a rewrite. The null/software backend is
always available for tests.

**Consequences.**

- One shader path, WebGPU-ready, tests on any machine.
- Possible driver quirks and a heavier dependency; device loss must be handled.
- Retained scene/command partial updates work. GPU damage scissoring works if we
  render into an offscreen texture we own across frames. True partial *present*
  (dirty rects) is not portable through wgpu; it needs a native backend.
- A platform-specific fast path is justified only if profiling shows wgpu
  overhead above ~10–15% of frame time or a required platform feature is missing.

**Alternatives.** Platform-specific backends from day one (premature); CPU only
(simple, slow for large scenes).

**Revisit trigger.** If the demo misses the frame budget because of command
encoding/present overhead, add a Metal backend on macOS first while keeping wgpu
for other targets.

---

### ADR-009: Derive-macro stores, not proxy emulation

**Status:** Accepted.

**Context.** Solid's stores use Proxy traps for transparent field access. Rust
has no safe transparent proxy. Leptos uses generated field getters with paths.

**Decision.** Adopt the Leptos-style derive macro: `#[derive(Store)]` generates
typed field getters; `#[derive(Patch)]` diffs. Provide a `DynamicStore` for
schema-less data.

**Consequences.**

- Compile-time checked paths and field names.
- No runtime interception overhead.
- Dynamic shapes are explicit and slower.
- Macro complexity and compile time.

**Alternatives.** Unsafe proxy (unsound or fragile); a single `Signal<Value>`
(coarse); snapshot + memo (simple, re-renders whole subtree).

---

### ADR-010: Rename `renderer-gpui` → `renderer-native`, `universal` → `renderer-api`

**Status:** Accepted.

**Context.** The outline's names imply a dependency on GPUI and use a Solid-ism
for the contract. Neither is accurate.

**Decision.** Use `renderer-native` for the first-party renderer and
`renderer-api` for the contract. Keep the old names in prose as "formerly" so
readers of the outline can map them.

**Consequences.** Clearer crate names; a small documentation mapping cost.

---

### ADR-011: Single UI thread

**Status:** Accepted.

**Context.** Both reference systems are UI-thread-affine. Multi-threaded reactive
graphs need atomics, locks, or message passing on every edge.

**Decision.** The graph, view tree, layout, and scene live on one thread. Async
may run elsewhere; results return as continuations. `Spawner` is the only
concurrency boundary.

**Consequences.**

- No `Send`/`Sync` bounds on UI state.
- Fast paths are lock-free in the single-threaded sense.
- Background computation must send plain values, not graph handles.

**Alternatives.** Multi-threaded graph (complex, slower for UI-sized graphs);
actor model (message overhead, hard to preserve frame atomicity).

---

### ADR-012: DAG by write discipline, debug-only cycle checks

**Status:** Accepted.

**Context.** A reactive graph is acyclic only if computations do not write the
signals they read. Solid 2.0 forbids writes under owned scope. A per-edge DFS is
too expensive for a hot path that may add thousands of edges during mount.

**Decision.** Reject writes during compute in dev; allow explicit
`owned_write`/`defer_write` escapes. Run a bounded cycle walk in debug when an
edge is added while the subscriber is computing. Release relies on the discipline
and the flush round limit.

**Consequences.**

- Cycles are almost always caught at the source (the write).
- Debug builds pay a small check; release does not.
- A `defer_write` feedback loop is caught by the round limit, not by topology.

**Alternatives.** Always-on 3-color DFS (correct, slow); no check (latent
cycles).

---

### ADR-013: GPL-2.0-or-later, with GPUI kept reference-only

**Status:** Accepted.

**Context.** The project is being published as free software. The references have
different licenses: Solid MIT, Leptos MIT, GPUI Apache-2.0.

**Decision.** License the project GPL-2.0-or-later. Keep GPUI as an architectural
reference for v1; do not copy its code yet. Ported MIT code keeps its notice.

**Consequences.** The project can use Solid/Leptos code and ideas with
attribution. GPUI is Apache-2.0: compatible with GPLv3, not GPLv2-only. Because
the project is GPL-2.0-or-later, a distributor may choose GPLv3 and combine
Apache-2.0 code, but the combined distribution must carry the Apache notices and
must not be made under GPLv2-only. Until we explicitly take that step, GPUI stays
reference-only. The scene/paint/input designs in this plan are independent
descriptions and can be implemented from scratch.

---

### ADR-014: Async is a compute mode, not a node type

**Status:** Accepted. Supersedes the first-draft `AsyncMemo<T>` split.

**Context.** The first pass separated `Memo<T>` and `AsyncMemo<T>`. That is
colored at the type level: a helper taking `Memo<T>` cannot accept an async memo,
and it loses Solid's writable derived signal shape (`createSignal(fn)`), which is
a signal with a compute function, not a read-only memo.

**Decision.** Async is a property of the compute function:

- Writable sources are split: `create_signal()` / `create_derived_signal()` / `create_async_derived_signal()`
  return `(ReadSignal<T>, WriteSignal<T>)`. There is no combined public signal
  and no `RwSignal`.
- `Memo<T>` is read-only derived; its compute may be sync or async.
- Both halves use `get`/`is_pending`/`latest`. A plain signal never suspends.
- Async state is cold (`Option<Box<AsyncState>>`); sync nodes do not pay for it.
- Constructors differ (`create_memo` / `create_async_memo`,
  `create_signal` / `create_derived_signal` / `create_async_derived_signal`)
  because Rust has no overloading, but the read half is always `ReadSignal<T>`
  and the write half is always `WriteSignal<T>`. A helper taking `ReadSignal<T>`
  or `Memo<T>` does not care how it was built.
- Do not add a normalizing `new` trait. Sync/async blanket impls overlap; a
  `Sync`/`Async` or `Compute` wrapper is still visible at the call site and only
  hides the mode. Keep the explicit constructors; a `Compute` wrapper may exist
  for macro-generated code.
- `AsyncMemo<T>` is not a distinct type; at most a readability alias.

**Consequences.** Writable derived signals are preserved. A derived node can be
async without changing its consumer's type. The sync fast path stays clean via
cold async state. The read API remains colorless (`get`) with an `is_pending`
verdict; there is no defaulting read.

---

## Part B — Risk detail

The PLAN's risk table is the summary. This section states each risk's warning
signs and the decision that would change.

### R1: Async semantics exceed the schedule

- **Warning signs:** Phase 2 slips past 7 weeks; the rule tests for `F-3`/`F-5`
  keep finding new edge cases; `isPending` tests depend on reader placement.
- **Mitigation:** ship the subset in [04-async-model.md](04-async-model.md); cut
  `isPending` to a diagnostic if needed; defer actions, `latest`, `affects`, and
  full `on` to v2.
- **Course change:** if the txn model cannot express a case without lanes, stop
  and add the lane abstraction rather than piling special cases.

### R2: Retained structural updates

- **Warning signs:** keyed list reorder loses focus or duplicates nodes; `Show`
  flip leaks owners; the differential fuzzer finds scene mismatches after moves.
- **Mitigation:** restrict v1 structure to `Show` + keyed `For`; no arbitrary
  re-parenting except portals; use `Rebuild` for pathological cases.
- **Course change:** if keyed reorder is not stable by Phase 4 exit, use
  per-view rebuild for that subtree and keep the retained model for static
  structure.

### R3: Incremental layout

- **Warning signs:** layout > 30% of frame time; demo feels sluggish on resize;
  text measurement dominates.
- **Mitigation:** layout roots; measure cache; text cache; avoid re-layout on
  paint-only changes.
- **Course change:** implement a dirty-subtree layout pass or switch to a
  layout engine with incremental support.

### R4: Text shaping

- **Warning signs:** > 5 ms/frame shaping; font fallback artifacts; IME cursor
  misalignment.
- **Mitigation:** parley/fontique; LRU shaped-line cache; grayscale AA;
  cluster-based caret.
- **Course change:** platform text backend per OS (CoreText/DirectWrite) if
  cross-platform shaping is not good enough.

### R5: Store macro complexity

- **Warning signs:** macro errors are unreadable; compile time grows; enum/patch
  cases explode.
- **Mitigation:** v1 subset: plain structs, `Vec`, `Option`, `HashMap`; defer
  enums and projections.
- **Course change:** ship a hand-written `Store` for the demo and defer the
  derive.

### R6: wgpu/platform instability

- **Warning signs:** device loss on a target; surface format issues; WebGPU
  differences.
- **Mitigation:** null/software backend; platform matrix in CI; capability
  checks.
- **Course change:** platform-specific backend for the problematic target.

### R7: Unsafe arena soundness

- **Warning signs:** Miri failures; aliasing bugs under stacked borrows; free-list
  reuse races (none in single-thread, but logic bugs).
- **Mitigation:** safe indices first; isolate `unsafe`; Miri; property tests.
- **Course change:** keep the safe implementation if `unsafe` does not buy a
  measured win.

### R8: API churn

- **Warning signs:** third-party renderer authors blocked; examples break every
  week.
- **Mitigation:** freeze `renderer-api` after Phase 3; keep app APIs unstable
  until M6.
- **Course change:** publish a `0.x` with a compatibility policy earlier.

---

## Part C — Open questions

These were open in Draft 1. Most are now resolved; the remaining engineering
questions are flagged.

### Q1. Component rebuild (resolved for v1)

**Decision:** no special rebuild primitive. The generic primitive is a
`ViewSlot`: a stable host node whose one active child is replaced when its key
changes. `Rebuild::new(closure)` is sugar over a `ViewSlot` keyed on the
closure's tracked reads.

Why: Solid and Leptos already express this with control flow. Solid's `<Show>`,
`<Switch>`, and `<For>` are specialized primitives; Solid's `dynamic()` is
specifically for switching which component/tag is rendered, not the base of the
control-flow components. Leptos has no `Dynamic` component; its control-flow
components are specialized Tachys views, with `AnyView`/`Either` for erasure.
Neither needs a generic slot to implement `Show`/`Switch`/`For`.

Our `Show`/`Switch`/`For` are specialized too and should be the default API. A
`ViewSlot` is the sibling primitive for the cases control flow cannot express:
arbitrary view-returning closures, runtime component/view switching, and plugin
UIs. A Solid-style `dynamic()` helper would be a thin wrapper over `ViewSlot`.

Semantics:

- old child owner, view nodes, and bindings are disposed;
- new child mounts under the same stable host; host/sibling identity survives;
- state captured outside the closure survives; locals inside reset;
- replacement is structural (layout + paint dirty);
- focus/scroll is lost unless state is hoisted or `KeepAlive` is used.

When useful: generated/plugin UIs, runtime component switching, cheap leaves, and
code naturally written as "render this tree from this state". Not for value
changes, keyed lists, or as a default.

### Q2. Props representation (resolved for v1)

**Decision:** all props are `Prop<T>`:

```rust
pub enum Prop<T> {
    Static(T),
    Reactive(ReadSignal<T>),
    Derived(Memo<T>),
}
```

There is no bare `T` prop if the value may change. A bare `T` is a snapshot with
no accessor; it cannot be reactive. `Prop::Static` allocates no node and creates
no edge; `Reactive`/`Derived` subscribe normally. The `view!` macro maps
closures to `Derived`, handles to `Reactive`, and other expressions to `Static`.

**Leptos comparison:** Leptos has `Signal<T>` (type-erased enum of read signal,
memo, derived closure, or stored value; `From<T>` uses `stored`),
`MaybeSignal<T>` (`Static(T)` or reactive; no node for static), `MaybeProp<T>`,
`#[prop(into)]`, `Callback`, and `Children`. `MaybeSignal<T>` is exactly the
`Prop<T>` shape. Our choice avoids both the bare-`T` footgun and the per-static
node cost of making everything `Signal<T>`.

**Cost:** one match per prop read and a slightly larger enum. That is acceptable;
prop reads are not the hottest loop, and the alternative is a silent snapshot bug
or a node per static prop.

### Q3. Async read style (resolved for v1)

**Decision:** `get()` is the public read.

- `get() -> T` is the only public value read. Inside a tracked computation, a
  not-ready value suspends via a private `NotReady` token caught at the
  computation boundary; the scheduler holds/replays the reader.
- `is_pending(cx) -> bool` is the verdict; it never suspends.
- There is no public `try_get`, `try_read`, or `try`, and no defaulting read.
  `try` is a reserved Rust keyword; a default would claim a value the graph does
  not have; a public `Result` read colors every helper.
- `Option<T>`/`Result<T, ReadError>` are rejected as the public shape: they
  conflate pending/error/absence and force propagation through pure helpers.
- `panic = "abort"` builds cannot use the unwind path; they use a
  boundary/domain state or an internal runtime fallback, not a public API.
- User code must not catch the private suspension token; `D-42` still applies.

This is the Rust translation of Solid throwing `NotReadyError`: the throw is a
graph control token, not an application error. Implementation paths (private
panic payload and the internal `panic = "abort"` fallback) are in
[04-async-model.md](04-async-model.md) §3.6.

### Q4. Boundary content lifetime and `on` (resolved for v1)

**Decision:** v1 disposes content when the fallback is shown; `KeepAlive` is the
opt-in for state that must survive. Solid keeps content alive but off screen;
that buys state preservation at the cost of "hidden readers hold nothing"
bookkeeping. For a first native version, disposal is simpler and matches the
retained tree's owner model.

**`on`:** deferred to v2. v1 uses a `key` memo that remounts the boundary. A key
change produces a fresh armed boundary, which shows the fallback on a new load
and then content. This covers navigation/reload.

Full `on` is not a simple reset: it must arm without remounting, show the
fallback with the frame of the change that armed it, avoid showing it when the
same data is read outside the boundary, collect readers that were already
forwarded, and interact with lanes when the `on` read is display-ahead. Those are
`D-15`/`D-16`; implementing a partial version produces wrong-frame fallbacks and
stranded holds. It belongs with the v2 lane/hold work.

### Q5. Dynamic stores and the interned path map (resolved for v1)

**Decision:** the static `#[derive(Store)]` path is v1. `DynamicStore` with an
interned path map is v2 unless a real v1 app needs it.

An interned path map is:

```
path ["users", Key(42), "name"]
  → canonical PathKey (interned segments)
  → PathId(17)                 // u32, dense
  → nodes[17] = NodeId(...)    // trigger node
```

A `HashMap<PathKey, PathId>` is consulted on first access; after that, a read or
write uses the integer `PathId` and a dense `Vec<NodeId>`. Benefits: no repeated
long-path hashing, stable node identity, cheap equality, shared prefixes, and no
duplicate strings. Costs: a canonicalization step on first access, an intern
table that must be bounded/evicted, and a requirement that keys are
`Hash + Eq + 'static` (or interned to `u32`). The alternatives are a per-access
`HashMap<Vec<Segment>, Node>` (simple, slow, allocates), a trie (good prefixes,
pointer chasing), or string paths (easy, slow). Static stores avoid the question
entirely: the macro generates a const field index/path.

Leptos already uses a runtime path map (`StorePath` of interned `usize` segments
into a `TriggerMap`), but hashes the full `Vec` per lookup and is typed. Our
dynamic store adds full-path interning, a dense `PathId` vector, and untyped
values for plugins/config. It is an evolution of Leptos' model, not a different
one; reuse its segment interning and ancestor-trigger ordering.

**Fallback:** if full-path interning or untyped values prove too complex, use the
Leptos shape directly (`StorePath` + `HashMap<StorePath, Trigger>`). It gives up
the dense fast path and arbitrary key types but keeps the dynamic API and is
already proven.

### Q6. Threading of background computation (resolved for v1)

**Decision:** background work is allowed. The user is not misunderstanding: you
can spawn a thread or a `tokio` task from the UI thread. The rule is that the
**graph, view tree, layout, and scene stay on the UI thread**. A background task
must compute a plain `Send + 'static` value and return it through a continuation;
it must not retain or mutate handles.

The runtime provides `Spawner::spawn_local` for UI-local futures and an optional
`BackgroundSpawner::spawn` for `Send` futures. The host can implement the latter
with tokio, rayon, or its own pool; the framework never owns threads. If the host
provides no background spawner, user code may use its own executor and send the
result back through a `UiHandle`/channel. The UI-local future must not block on
CPU or blocking I/O directly, or it will stall the frame; await a background task
instead.

Cancellation is cooperative. When the memo re-runs or is disposed, the old
future is dropped/aborted and its generation is superseded; a late result is
discarded. A raw thread cannot be killed, so long-running work must check the
flight's `CancellationToken` and stop early. The framework guarantees the stale
result is ignored; it cannot guarantee the thread stops without cooperation.

### Q7. Batching (resolved for v1)

**Reactive write batching.** The choice is frame checkpoint (chosen), microtask,
explicit `batch` scopes, or immediate writes. Frame batching gives deterministic
one-flush/one-paint semantics and is required for holds; its cost is that
read-after-write needs `flush(fn)`, and non-frame contexts need an explicit
flush. Microtask batching is a JS event-loop artifact; immediate writes tear and
lose glitch freedom; a separate `batch` API is mostly redundant when the tick is
already the batch. v1 keeps `flush(fn)` as the imperative escape hatch and does
not add `batch`.

**Renderer contract batching.** The base trait is per-operation; `begin_frame`/
`end_frame` are optional. Per-operation is simplest for DOM-like backends and
conformance; frame hooks let a native/GPU backend coalesce command encoding.
Requiring frame hooks would burden simple backends; forbidding them would force
one call per primitive. Optional is the right split. The native path does not use
the contract at all; it has its own frame.

### Q8. License notices (resolved)

**Decision:** the project is **GPL-2.0-or-later** (see `LICENSE`). Use one
`THIRD_PARTY.md` for reference attributions and a module header for any ported MIT
code.

**Compatibility note:** GPUI is Apache-2.0, compatible with GPLv3 but not
GPLv2-only. Keep it reference-only for v1. If GPUI code is incorporated, the
combined distribution must be under GPLv3 terms (allowed by the “or later” grant)
and carry the Apache notices. Solid and Leptos are MIT and compatible; keep
their notices when porting code.

### Q9. Text default width (Phase 5)

Content-sized text by default, or fill the parent?

**Recommendation:** content-sized for labels, with a `wrap` opt-in. Container
stretching is a style concern.

### Q10. Focus restoration (Phase 6)

Parent/sibling fallback, or restore to the last focused node when it remounts?

**Recommendation:** parent/sibling fallback in v1; `KeepAlive` for exact restore.

---

## Part D — What would make us change course

- **If the height heap cannot preserve `F-1` under async holds** after two
  focused attempts, consider a two-phase commit that snapshots all display reads
  and replays them, rather than committing node-by-node.
- **If the retained tree cannot keep keyed reorder stable**, adopt GPUI's
  per-view ephemeral element tree for list rows and keep the retained tree for
  static structure.
- **If mount-once is too surprising for users**, add a `render` component mode
  (rebuild by default for a component) even if it is coarser. The architecture
  supports both; the default is a product decision.
- **If incremental layout is required for the demo**, either add layout roots
  aggressively or switch to a layout engine with incremental support.
- **If third-party renderers cannot implement the contract without layout**,
  add a `MeasureCapability` and a shared text system to the contract, rather than
  forcing every backend to own shaping.

---

## Part E — Mapping from `PLAN-OUTLINE.md`

The outline is the original brief. This table records where the detailed plan
keeps, refines, or rejects each claim, so a reader can move between the two
without guessing.

| Outline section / claim | Where it landed | What changed |
|---|---|---|
| §4.1 R3 push-pull, lazy two-phase | ADR-002, [01-reactive-core.md](01-reactive-core.md) §7 | The shipped oracle uses a height-indexed heap with DIRTY/CHECK, not R3. R3 stays a benchmark. |
| §4.2 generational arena, `Copy` handles | ADR-003, [02-ownership-and-lifetimes.md](02-ownership-and-lifetimes.md) | Kept, with a stronger rule: disposed handles never resolve, so no resurrection. |
| §4.3 Solid 2.0 `F-<n>` rules | ADR-001, [04-async-model.md](04-async-model.md) §9 | Adopted as the oracle, but only a v1 subset. Optimistic/lane rules are deferred. |
| §4.4 "No async runtime is required" | [04-async-model.md](04-async-model.md) §3 | Rust async needs an executor and an I/O driver. The framework owns queues and wakeups, not the executor; the host supplies a `Spawner`. |
| §5 `#[derive(Store)]` field getters | [03-store-layer.md](03-store-layer.md) | Kept, with explicit `Patch`, keyed collections, and a dynamic-store escape hatch. |
| §6.1 owner tree, RAII guards | [02-ownership-and-lifetimes.md](02-ownership-and-lifetimes.md) | Kept; expanded cleanup order, context, and view ownership. |
| §6.2 reclamation gap | ADR-003, [02-ownership-and-lifetimes.md](02-ownership-and-lifetimes.md) §5 | Eliminated by no-resurrection + free-list reuse. No compaction in v1. |
| §7 universal trait and capabilities | [06-renderer-contract.md](06-renderer-contract.md) | Made object-safe with `u64` node IDs; added style, events, frame hooks; renamed `renderer-api`. |
| §8.1 persistent `UiNode` tree | [07-retained-renderer.md](07-retained-renderer.md) | Kept, but the node tree is not a second reactive system: dynamic properties are bindings. |
| §8.2 "each UI node subscribes to the signals it reads during its render function" | [07-retained-renderer.md](07-retained-renderer.md) §3 | Refined: the *binding* subscribes, not the node's whole render function. Components mount once. |
| §8.3 direct interaction path | [07-retained-renderer.md](07-retained-renderer.md) §1 | Kept as Model C: direct native tree, with a universal adapter only for conformance. |
| §8.4 GPUI choices preserved | [07-retained-renderer.md](07-retained-renderer.md), [09-input-focus-accessibility.md](09-input-focus-accessibility.md) | Kept identity, selective invalidation, one frame, zero idle CPU. |
| §8.5 "imperative element tree rebuild" replaced | [07-retained-renderer.md](07-retained-renderer.md) | Replaced by mount-once structure + bindings; `Rebuild` remains an explicit escape hatch. |
| §9 frame checkpoint | [05-runtime-and-frame-scheduler.md](05-runtime-and-frame-scheduler.md) | Expanded to six phases; async pump first, compute/apply split, layout/scene/present separated. |
| §9.2 async bridge | [04-async-model.md](04-async-model.md) §3–5 | Continuation queue kept; flight generations, staged landings, and holds specified. |
| §10 testing | [11-testing-and-performance.md](11-testing-and-performance.md) | Added differential scene tests, randomized tree fuzzing, Miri, leak/soak, and CI gates. |
| §11 roadmap | [PLAN.md](../PLAN.md) §6 | Async moved from Phase 6 to Phase 2; stores moved later; renderer split into structure and pixels. |
| §12 open problems | Part C above | Reclamation is resolved by design; async subset, store persistence, and incremental layout remain open. |
| Crate names `renderer-gpui`, `universal` | ADR-010 | Renamed `renderer-native`, `renderer-api`. |


---

## Part F — Remaining open questions (audit)

This is the consolidated audit of the `Open questions` sections in the section
docs. Items marked **resolved** are no longer open; they are listed in Part C or
the ADRs. The lists below are the items that still need a decision, a prototype,
or a measurement.

### Resolved / no longer open

- Async is a compute mode, not a separate `AsyncMemo` type.
- Signals are split by default: `create_signal()` returns
  `(ReadSignal<T>, WriteSignal<T>)`; no `RwSignal`.
- Public reads are `get` + `is_pending`; no `try`/`try_get`/`get_or`.
- Props are `Prop<T>` (`Static`, `Reactive(ReadSignal)`, `Derived(Memo)`).
- `ViewSlot` is a sibling of specialized `Show`/`Switch`/`For`; no rebuild
  primitive.
- `on`, actions, `latest`, `affects`, optimism, and lanes are v2.
- Background work is allowed; the graph stays UI-thread-affine; cancellation is
  cooperative.
- Frame batching is the write batch; `flush(fn)` is the imperative escape hatch.
- License is GPL-2.0-or-later; GPUI is reference-only for v1.
- `DynamicStore` is v2 with a Leptos-style fallback.
- Async code uses owned context capabilities: `&Cx` for sync, read-only
  `CxHandle` for async compute, `ActionCx` for action writes, `TaskCx` for
  detached mainline work. Reads after `await` are tracked.
- `Derived<T>` (no macro; `create_derived`) is the uncached read-only derivation
  (Solid function form, Leptos `Signal::derive`). It is sync-only; async and
  `is_pending` need a memo node.
- Components take `cx: &mut Cx` as their first parameter. `#[component]` handles
  props, children, prop classification, and the owner scope; it does not inject a
  hidden `cx`.
- Reactive constructors use one `create_*` family: `create_signal`,
  `create_derived_signal`, `create_async_derived_signal`, `create_memo`,
  `create_async_memo`, `create_lazy_memo`, `create_derived`. Single-type
  constructors keep `Type::new` (`Store::new`, `ViewSlot::new`). Macros
  (`signal!`, `memo!`) sit on top.

### Leaning, needs confirmation (recommendation exists)

| Area | Question | Current leaning |
|---|---|---|
| Core equality | Require `T: PartialEq` by default? | Yes; `create_signal_unchecked`/custom comparator escape |
| Signal update | Should `update` compare or always notify? | Always notify; `set` compares; `update_with` if needed |
| Memo reads | Guards or clones? | Both: guards internally, `Clone` convenience for app code |
| Memo lifetime | Eager vs lazy default; autodisposal | Eager default; `create_lazy_memo` = lazy + autodispose (Solid); defer if autodisposal is not in v1 |
| Props variants | Keep `Reactive`/`Derived` or one read handle? | Collapse; both implement `Read<T>` |
| Effects | Public `EffectHandle` or owner-only? | Both: handles for bindings, owner-scoped for app effects |
| Ownership | Public `OwnerId`? | No; opaque `Scope`, IDs for diagnostics |
| Cleanup panic | Halt or report/continue? | Report and continue; halt only for uncaught compute/apply errors |
| Boundary content | Dispose on fallback or keep alive? | Dispose in v1; `KeepAlive` opt-in |
| First load | Write while a first load is out | Held behind the load; a memo in between changes nothing |
| Store draft | Clone-modify-swap or `Draft` guard? | Clone-modify-swap v1; draft v2 |
| Store patch | Require `PartialEq`? | Yes; manual `Patch` trait for other types |
| Renderer style | Full `ResolvedStyle` or property diffs? | Full struct v1; diff adapter later |
| Text ownership | Renderer or shared text system? | Shared; capability reports which |
| Frame hooks | Required or optional? | Optional |
| Runtime loop | Runtime owns winit or host owns loop? | Both: `run` convenience, host-owned `frame` |
| Frame phases | Separate layout/paint frames? | One frame with phase skips |
| Windows | One runtime per window? | One graph/runtime, multiple window hosts |
| Child order | Linked list or cached Vec? | Links first; measure |
| Focus restore | Parent/sibling or exact remount? | Parent/sibling; `KeepAlive` for exact |
| Text width | Content-sized or fill parent? | Content-sized with `wrap` opt-in |
| A11y build | Full or incremental? | Full when active first; measure |
| Fuzz corpus | Seeds or corpus dir? | Both; seeds are small |

### Genuinely open — needs a prototype or measurement

| Phase | Question | Why it is still open | Next step |
|---|---|---|---|
| 1 | Equality default and comparator API | Affects every constructor and generic bound | Decide before core API freeze |
| 1 | Memo read representation (guard vs clone vs both) | Borrow conflicts vs ergonomics | Prototype both on the demo |
| 1 | EffectHandle visibility | Public API surface vs binding lifetime | Decide before prelude freeze |
| 2 | Colorless suspension implementation | Private panic payload vs custom unwinder; `panic = "abort"` | Prototype Path A/B; measure |
| 2 | Transaction merge criterion | Sharing a reader vs sharing a node | Fuzz independent-widget shapes |
| 2 | Re-ask detection | Explicit `refresh()` vs hashing tracked inputs | Decide with async rule tests |
| 2 | `is_pending` in a held frame | Verdict-only display-ahead: allow value-identical Display applies, or build a verdict lane | Prototype with the states demo (13-reference-apps.md) |
| 2 | `on` full semantics | Needs full boundary/txn/lane model | v2; keyed remount in v1 |
| 3 | Stable renderer `NodeId` contract | `u64` opaque; `0` invalid; backend pointer option | Freeze in `renderer-api` |
| 3 | Event model richness | Generic `Event` vs typed pointer/key/IME | Prototype with real input |
| 4 | Component-local state representation | Per-mount struct vs closure captures | Decide with macro design |
| 4 | Child order representation | Linked list vs cached Vec | Benchmark mount/reorder |
| 4 | `KeepAlive` scope | v1 or v1.1? Tabs/navigation need it | Decide before M4 |
| 4 | Paint roots / damage layering | Window vs layer roots | Prototype overlays/tooltips |
| 5 | Text engine | parley vs cosmic-text vs platform | Prototype on demo text |
| 5 | Persistent taffy style updates | Rebuild vs patch style structs | Benchmark |
| 5 | Incremental layout | Layout roots vs dirty-subtree pass | Measure Phase 5 frame time |
| 5 | Text intrinsic sizing/default width | Layout invalidation vs UI expectations | Decide with text prototype |
| 5 | Subpixel text | Grayscale vs LCD subpixel | Platform/quality study |
| 5 | Font loading | Bundle vs system vs both | Decide with test font |
| 6 | Dispatch tree vs parent walk | Portals and logical parents | Prototype portal |
| 6 | Gesture scope | Click/drag/wheel vs double/pinch/long-press | Decide v1 scope |
| 6 | A11y tree build cost | Full vs incremental when active | Measure with AccessKit |
| 6 | Cursor/tooltip layers | Separate vs shared overlay layer | Hit-test prototype |
| 7 | Store persistent vector vs copy-on-write | O(n) writes vs dependency/API | 10k-row benchmark |
| 7 | Store field read/write split | Apply `ReadField`/`WriteField` like signals? | Prototype before store API freeze |
| 8 | `view!` syntax strictness | Tree syntax vs builder vs both | Macro prototype |
| 8 | Reactive attribute detection | Macro type inference vs explicit syntax | `trybuild` prototype |
| 8 | Optional props | `Prop<Option<T>>` vs `MaybeProp<T>` | Decide with props macro |
| 8 | `For` key inference | Explicit key vs `Keyed` trait | Prototype macro |
| 8 | Macro error spans | Proc-macro diagnostics quality | Budget time; `trybuild` |
| 8 | `view_builder!` | Fluent macro front-end | Stretch only; not scheduled |

### Deferred (not open for v1)

- `on` full re-arm, actions, `latest`, `affects`, optimism, lanes.
- `DynamicStore` unless a v1 app needs it.
- Store projections / async derived stores.
- `KeepAlive` if not in v1.
- `view_builder!`.
- Incremental layout beyond layout roots.
- True partial present (native backend).
- SSR/hydration, server functions, hot reload.
