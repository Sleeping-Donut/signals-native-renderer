# 12 — Decisions, Risks, and Open Questions

This document is the ADR log and the risk register. It is append-only in spirit:
when a decision changes, add a new ADR that supersedes the old one; do not rewrite
history silently. The short table in [PLAN.md](../PLAN.md) §5 is the index.

---

## Part A — Architecture decisions

### ADR-001: Solid 2.0's rules are the oracle, not its API

**Status:** Accepted.

**Context.** Solid 2.0's `next` branch contains a large, heavily tested async
runtime. Its rules (`F-<n>`, `D-<n>`, `S-<n>`) describe observable behavior:
frame integrity, holds, boundaries, pending, flush visibility. Its API is shaped
by JavaScript (getters, proxies, microtasks, exceptions for control flow).

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

**Status:** Accepted.

**Context.** GPUI uses platform-specific backends (Metal, DirectX, Blade) for
maximum performance. We need cross-platform reach and a testable offscreen path.

**Decision.** Use `wgpu` for v1. Keep a `PaintBackend` trait so a platform-specific
backend can be added later. The null/software backend is always available for
tests.

**Consequences.**

- One shader path, WebGPU-ready.
- Possible driver quirks and a heavier dependency.
- Device loss must be handled; tests use a software backend.

**Alternatives.** Platform-specific backends (faster, three implementations);
`tiny-skia` CPU only (simple, slow for large scenes).

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

## Part B — Risk detail

The PLAN's risk table is the summary. This section states each risk's warning
signs and the decision that would change.

### R1: Async semantics exceed the schedule

- **Warning signs:** Phase 2 slips past 7 weeks; the rule tests for `F-3`/`F-5`
  keep finding new edge cases; `isPending` tests depend on reader placement.
- **Mitigation:** ship the subset in [04-async-model.md](04-async-model.md); cut
  `latest`/`isPending` to diagnostics if needed; defer actions and `affects`.
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

These are unresolved and should be decided in the named phase. Each has a
recommendation; the decision goes in this file when made.

### Q1. Component rebuild syntax (Phase 4)

Should `#[component(rebuild)]` be a first-class attribute, or only an explicit
`Rebuild` wrapper?

**Recommendation:** attribute, because it is discoverable and the macro already
knows the body. Keep `Rebuild` for manual composition.

### Q2. Props representation (Phase 4)

`Signal<T>` fields for reactive props, or a `Prop<T>` enum (`Static(T)` |
`Reactive(Signal<T>)`)?

**Recommendation:** `Signal<T>` for dynamic props and plain `T` for static props;
the macro chooses based on the argument type. Avoid a runtime enum.

### Q3. Async read style (Phase 2)

`Result<T, NotReady>` vs a `T` that throws to the boundary.

**Recommendation:** `Result` internally; the view binding converts `NotReady` to
a boundary/hold. This keeps Rust code explicit and testable.

### Q4. Boundary content lifetime (Phase 2)

Dispose content when showing a fallback, or keep it alive but hidden?

**Recommendation:** dispose in v1; add `KeepAlive` for state that matters. This
matches the retained tree's ownership model and avoids "hidden content holds
nothing" bookkeeping.

### Q5. Dynamic stores (Phase 7)

Ship `DynamicStore` in v1 or defer?

**Recommendation:** defer; `Signal<Value>` + rebuild covers config editors.

### Q6. Threading of background computation (Phase 2)

Can background tasks compute pure values and send them, or must all user code be
on the UI thread?

**Recommendation:** allow pure values; require `Send + 'static` for the result.
The graph stays UI-thread-affine.

### Q7. Renderer contract batching (Phase 3)

Do we need `begin_frame`/`end_frame` in the base trait?

**Recommendation:** keep them optional. The native path uses its own frame; a DOM
renderer can ignore them.

### Q8. Licence notices (Phase 1)

Per-module port notices or one `NOTICE` file?

**Recommendation:** one `NOTICE` plus a header comment in modules that port an
algorithm, naming the source repository and licence.

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

