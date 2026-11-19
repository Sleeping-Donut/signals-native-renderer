# PLAN

**Status:** Draft 2 — review decisions folded in (v1/v2 scope, async reads, props, rebuild, license).
**Supersedes:** `PLAN-OUTLINE.md` (kept as the original brief).
**Companion docs:** [`ARCHITECTURE.md`](ARCHITECTURE.md) and [`docs/`](docs/README.md).

---

## 1. What we are building

A native Rust UI framework with three properties that are usually bought separately:

1. **Fine-grained reactive state.** A Solid 2.0–class reactive graph: signals, memos,
   split effects, owners, async derivations, and question-scoped pending. The
   behavioral oracle is Solid 2.0's documented rules (`F-<n>` / `D-<n>` /
   `S-<n>`), not its JavaScript API.
2. **Retained rendering.** Stable view and element identities across frames,
   explicit invalidation, incremental binding updates, retained layout and paint
   caches, and a frame that does only the work its dirty set requires.
3. **Renderer portability.** A small object-safe contract for third-party
   backends, plus a first-party renderer that deliberately bypasses the contract
   on its hot path.

The system is **one reactive graph, one retained tree, one checkpoint per frame**.
There is no virtual-DOM diff, no second reactivity model in the renderer, and no
hidden background executor owned by the framework.

---

## 2. Principles

| # | Principle | Consequence |
|---|---|---|
| P1 | **One graph** | Application state and rendered output share one dependency graph. The renderer is a sink of that graph, not a parallel notification system. |
| P2 | **Explicit at the boundary** | Automatic tracking inside a computation; writes, frame flushes, and layout/paint passes happen at named boundaries. |
| P3 | **Retained identity** | Views, elements, text, and layout nodes keep stable generational IDs. A missing ID is a bug, not a reason to rebuild the world. |
| P4 | **Mount once, bind forever** | Component structure is built at mount; dynamic values are fine-grained bindings. Whole-subtree re-render is an explicit escape hatch, not the default. |
| P5 | **The frame is the checkpoint** | Async continuations drain, writes promote, computations settle, effect applies run, layout runs, paint emits: once per frame, in that order. |
| P6 | **No resurrection** | A disposed handle never becomes live again. This removes the reclamation gap that a JavaScript port inherits from GC reachability. |
| P7 | **Universal contract, specialized flagship** | Third-party renderers get a small trait; the first-party renderer uses direct monomorphized paths. Both share one conformance suite. |
| P8 | **No hidden executor** | Async work is spawned through a `Spawner` supplied by the host. The framework owns queues and wakeups, not threads or timers. |

---

## 3. Scope

### 3.1 In scope for v1

- Reactive core with signals, memos, split effects, render effects, batching, and
  ownership.
- Generational arena and opaque `Copy` handles.
- Async compute mode for derived signals and memos, with flight generations,
  held values, basic `isPending`, and frame-level holds.
- `LoadingBoundary` and `ErrorBoundary`, with the subset of Solid's hold model
  described in [04-async-model.md](docs/04-async-model.md): first load, held
  navigation, disarmed forwarding, and errors. A `key`-triggered boundary remount
  substitutes for `on` in v1.
- Derived stores with per-field and per-key granularity.
- Universal renderer contract, null renderer, and a conformance suite.
- Retained native renderer: element tree, bindings, keyed lists, conditionals,
  layout, paint, input, focus.
- Taffy layout and text shaping/measurement with a bounded cache.
- wgpu paint backend (desktop first; web later).
- Dev diagnostics: owner/cleanup diagnostics, write-in-computation errors, dirty
  tracing, and a live graph inspector.
- Deterministic test runtime and a headless scene renderer.

### 3.2 Explicitly out of scope for v1

- Optimistic writes (`F-11`), actions, `latest`, `affects`, and full
  display-ahead/lane semantics (`F-6`). The node shape leaves room for them; the
  machinery lands in v2.
- Full `LoadingBoundary::on` re-arm semantics. v1 uses a `key` that remounts the
  boundary, which covers navigation and reload without the re-arm edge cases
  (`F-4`/`F-5`, `D-15`/`D-16`).
- SSR, hydration, and server functions. The renderer contract must not assume a
  DOM, but no server target is built.
- Multi-window collaboration, presence, and CRDT state.
- Hot reload of Rust code (subsecond-style). The architecture supports it; it is
  not a deliverable.
- A visual designer or a general-purpose CSS engine.
- Full WAI-ARIA authoring surface; AccessKit integration covers the common roles
  first.

### 3.3 Non-goals that are often assumed

- **Not a GPUI fork.** GPUI is a reference for identity, caching, input dispatch,
  and scene composition. We do not depend on `gpui` and do not copy its
  immediate-mode element lifecycle.
- **Not a Leptos port.** Leptos is a reference for a production Rust reactive
  graph and store macro. Its `Arc<RwLock>` node shape and executor model are
  explicitly not adopted.
- **Not Solid-compatible at the API level.** We port behavior, not `createSignal`
  call shapes. Rust naming and ownership rules win where they conflict.

---

## 4. Reference inputs and how they are used

All reference repos are read-only under `refs/` (git-ignored). The project itself
is licensed **GPL-2.0-or-later** (see `LICENSE`). Solid and Leptos are MIT and
GPLv2-compatible. GPUI is Apache-2.0: compatible with GPLv3, not GPLv2. A
GPL-2.0-or-later distributor may choose GPLv3 and combine Apache-2.0 code, but
this plan keeps GPUI reference-only until the project explicitly distributes the
affected code under GPLv3-compatible terms with Apache notices. Where an
algorithm is ported from an MIT source, record the source file and license in the
module header.

| Reference | What we take | What we do not take |
|---|---|---|
| `refs/solid/packages/signals` | Behavioral oracle: `docs/RULES-FUNDAMENTALS.md`, `RULES-DERIVED.md`, `SCENARIOS.md`; the height-heap scheduler in `src/core/heap.ts`; link/unlink edge lists in `src/core/graph.ts`; the staged-write and hold model in `src/core/core.ts` and `scheduler.ts`. | JS object layout, Proxy stores, microtask scheduling, byte-size tricks. |
| `refs/solid/packages/universal` | Shape of a minimal renderer contract (`RendererOptions`, `createRenderer`). | Its implicit mutable-DOM assumptions and array reconciliation. |
| `refs/solid/documentation/solid-2.0` | The 2.0 semantics: split effects, flush batching, strict reads, stores, async boundaries. | JS-specific migration rules. |
| `refs/leptos/reactive_graph` | A current Rust graph: `ReactiveNode` states, observer tracking, owner cleanup, `RenderEffect`, `AsyncDerived`, `SourceSet` ordering constraints. | `Arc<RwLock>` per node, subscriber-set cloning on every mark, executor-driven effects. |
| `refs/leptos/reactive_stores` + `_macro` | The field-Getter store design: `StorePath`, `Field`, `Subfield`, keyed access, `Patch`, per-path triggers. | Its arena-per-value indirection where a direct path node is cheaper. |
| `refs/leptos/tachys` | How dynamic bindings become render effects (`reactive_graph/{property,style,class,bind}.rs`); a no-diff renderer architecture. | Its DOM-specific builder traits. |
| `refs/zed/crates/gpui` | Retained view identity (`EntityId`), per-view prepaint/paint cache ranges, scene replay, taffy integration, hitbox/content-mask input dispatch, focus/action dispatch, AccessKit integration, deterministic test harness, randomized element-tree fuzzer. | Immediate-mode element rebuild as the default; `cx.notify()` as the only invalidation channel; per-frame full layout. |

---

## 5. Key decisions

Each decision is expanded in [docs/12-decisions-and-risks.md](docs/12-decisions-and-risks.md).
The largest deviations from `PLAN-OUTLINE.md` are D2 (height heap, not R3), D3
(no resurrection), D5 (async before the renderer), and D6 (retained nodes with
bindings). A section-by-section mapping from the outline is in
[docs/12-decisions-and-risks.md](docs/12-decisions-and-risks.md) Part E.

| ID | Decision | Why | Rejected alternative |
|---|---|---|---|
| D1 | Solid 2.0's **rules** are the oracle; its API is not. | The rules capture the hard semantics (frame integrity, holds, pending) without JS constraints. | A loose "signals-like" core with no conformance target. |
| D2 | Use a **height-indexed dirty heap** with DIRTY/CHECK marks, not R3's pull-verify traversal. | This is what current Solid 2.0 actually ships (`core/heap.ts`); it gives topological execution without recursion and without re-walking clean paths. | R3's two-phase graph coloring; Alien Signals' plain stack. Both remain benchmarks. |
| D3 | **Arena + generational `Copy` handles**; no resurrection. | Fast reads, stable identity, safe disposal, no `Rc` cycles, and stale handles fail deterministically. | `Arc<RwLock<Node>>` (allocation and lock per node); raw indices (ABA). |
| D4 | **Writes stage; the flush promotes.** Staged writes are invisible to readers until the frame checkpoint. | `F-10` is load-bearing for async holds and for `latest`/`isPending`. Retrofitting it later would touch every read path. | Eager writes with a "dirty" notification. |
| D5 | **Async is Phase 2, not Phase 6.** | Flight state, staged values, and reader classes determine the node shape and scheduler. Building the renderer first would force a core rewrite. | Outline order (renderer before async). |
| D6 | **Retained element tree with per-node bindings**, plus a `ViewSlot` for structural replacement. | Matches the fine-grained graph: one signal write updates one text run, not a whole component subtree. Retains a GPUI-like subtree replacement as the escape hatch, without making it a second renderer model. | GPUI's fully ephemeral element tree (coarse); a virtual DOM (diff tax). |
| D7 | **Taffy full layout on layout-dirty frames; incremental layout is a later optimization.** | Correctness first; paint-only frames can skip layout entirely, which is the common case. | Promising incremental flex/grid layout in v1. |
| D8 | **wgpu** for the first-party paint backend. | Cross-platform, testable offscreen, one shader path. | Metal/D3D/Blade per platform in v1. |
| D9 | **Derive-macro stores** (Leptos-style), not a proxy emulation. | Compile-time paths are fast and Rust-idiomatic; dynamic stores are an explicit opt-in. | A `Deref`-based fake proxy with runtime interception. |
| D10 | Rename **`renderer-gpui` → `renderer-native`**; rename **`universal` → `renderer-api`**. | The renderer does not use GPUI, and "universal" is a Solid-ism. Layer names stay recognisable in prose. | Keeping misleading crate names. |
| D11 | **Single UI thread**; async may run elsewhere, but state mutation and rendering are thread-affine. | Removes atomic and lock costs, matches both reference systems, and keeps frame determinism. | A multi-threaded graph with `Send` nodes. |
| D12 | **DAG by write discipline**, with debug-only cycle checks. | Writes inside computations are rejected, which removes the only common cycle source. A per-edge 3-color DFS is too expensive for release. | Always-on cycle validation on every edge insert. |

---

## 6. Phased roadmap

Phases are ordered by dependency, not by user-visible excitement. Each phase has
an exit criterion that can be tested in CI.

### Phase 0 — Foundations (1–2 weeks)

**Build:** workspace layout, `reactive-core` skeleton, generational arena,
`NodeId`/`Handle`, `Runtime` shell, test harness, `ManualSpawner`, benchmark
harness.

**Exit:** arena create/remove/reuse passes property tests; a 10k-node arena
allocates and frees in bounded time; `cargo test` and `cargo bench` run on macOS
and Linux.

**Status (2026-11-19): implemented.** The workspace, arena, handles, runtime
shell, manual spawner, property tests, and arena benchmark live in
[`crates/`](crates/). The flake and CI matrix cover macOS and Linux:
`nix develop -c cargo test --workspace` and
`nix develop -c cargo bench -p reactive-core`.

### Phase 1 — Reactive core (3–4 weeks)

**Build:** signals, memos, split effects, render effects, dependency edges,
height heap, dirty/check propagation, equality, batching, flush, `untrack`,
context, owners, cleanup, disposal.

**Exit:**
- Ported graph tests pass: glitch freedom, diamond, batched reads, equality
  short-circuit, disposal order, nested owners, effect ordering.
- A write to one of 10k signals touches only its dependent path (asserted by a
  dirty-visit counter).
- No `unsafe` outside the arena; Miri passes on the core test suite.

**Status (2026-11-19): implemented.** The graph lives in
[`crates/reactive-core/src/graph.rs`](crates/reactive-core/src/graph.rs) with
integration tests in
[`crates/reactive-core/tests/reactive_graph.rs`](crates/reactive-core/tests/reactive_graph.rs).
The graph uses `#![forbid(unsafe_code)]`. Miri passes the core tests; the two
scale/timing tests are ignored under Miri because they measure wall-clock time.

### Phase 2 — Async and holds (3–5 weeks)

**Build:** `Spawner`/`BackgroundSpawner` traits, continuation queue, flight
generations, async compute mode for derived signals and memos, basic `isPending`,
`LoadingBoundary` (first load, disarmed forwarding, `key` remount),
`ErrorBoundary`, staged holds, reader classes (derive / display / observe),
deterministic virtual clock. Actions, `latest`, `affects`, and full `on` are v2.

**Exit:**
- A navigation test: click → page B data loads → old page stays on screen →
  page B appears whole.
- A first-load test suspends to the nearest boundary; a disarmed boundary
  forwards and keeps content.
- A `key` change remounts the boundary and shows the fallback on a fresh load.
- Held async never shows a partial frame: a snapshot assertion on every frame.
- Stale flight completions are dropped; cancellation runs teardown exactly once.

**Status (2026-11-19): implemented.** Async memos and writable derived signals,
flight generations, cancellation, `is_pending`, `LoadingBoundary` (first load,
disarmed hold, key remount), `ErrorBoundary`, `ReaderClass`, and the
deterministic virtual clock are implemented in
[`crates/reactive-core`](crates/reactive-core/src/graph.rs). Flights run through
a host `Spawner`; `ManualSpawner` is the default and the runtime crate re-exports
the contract ([`graph_spawner.rs`](crates/runtime/tests/graph_spawner.rs)). The
navigation, no-partial-frame, stale-flight, virtual-clock, reader-class, and
spawner tests are in
[`async_graph.rs`](crates/reactive-core/tests/async_graph.rs). The full multi-node
transaction model remains a v2 refinement.

### Phase 3 — Renderer contract and null renderer (2 weeks)

**Build:** `renderer-api`, capability sub-traits, `renderer-null`, operation
recording, frame begin/end, conformance tests, binding adapters.

**Exit:** a headless counter app renders, updates only the changed text
operation, and produces a deterministic operation log; the same app runs on the
in-process scene recorder.

**Status (2026-11-19): implemented.** [`renderer-api`](crates/renderer-api/src/lib.rs)
defines the object-safe contract and capability traits;
[`renderer-null`](crates/renderer-null/src/lib.rs) maintains a shadow tree and
deterministic operation log. Conformance tests cover structure, text,
attributes, style, events, errors, frame hooks, and the log
([conformance.rs](crates/renderer-null/tests/conformance.rs)). The headless
counter renders two instances, updates only the changed text operations, and
preserves per-instance state
([headless_counter.rs](crates/renderer-null/tests/headless_counter.rs)).

### Phase 4 — Retained native renderer: structure (4–6 weeks)

**Build:** `ViewNode` arena, child lists, keyed lists, conditionals, dynamic view
slots, binding effects, `view!` macro (first cut), dirty flags, invalidation,
root/window lifecycle.

**Exit:** a 10k-row keyed list mounts, reorders, and updates only affected rows;
structural changes preserve sibling state and focus; no per-frame tree rebuild in
the allocation profile.

**Status (2026-11-19): implemented.** [`view`](crates/view/src/lib.rs) has the
retained `ViewTree`, child lists, keyed lists, `show`/`view_slot`, dirty/
invalidation tracking, text/button binding effects, mount/unmount/frame
lifecycle, retained focus identity, and a first `view!` macro in
[`macros`](crates/macros/src/lib.rs). Renderer-level focus is delivered through
the `FocusCapable` capability and survives reorder. Tests cover a two-counter
builder tree, a 10k-row reorder with unchanged text bindings and preserved row
state/focus, a branch slot, the macro, and an allocation profile
([retained_tree.rs](crates/view/tests/retained_tree.rs),
[view_macro.rs](crates/view/tests/view_macro.rs),
[allocation_profile.rs](crates/view/tests/allocation_profile.rs)).

### Phase 5 — Layout, paint, platform (4–6 weeks)

**Build:** taffy integration with persistent node mapping, style resolution,
text measurement and shaping, scene builder, paint cache/replay, wgpu backend,
window loop, scale factor and pixel snapping.

**Exit:** the demo app runs at 60 fps on a 2019 laptop; a paint-only change
(color/text) does zero layout; a layout change repaints only dirty subtrees;
resize is correct at 1×/1.5×/2×.

**Status (2026-11-19): layout, platform text, scene, and native backend implemented.** The
[`layout`](crates/layout/src/lib.rs) crate owns a persistent
[`taffy`](https://crates.io/crates/taffy) tree, maps logical styles to taffy
styles, tracks layout-dirty nodes, and copies snapped geometry (1×/1.5×/2×)
back to the tree. Paint-only style changes run zero layout, and geometry changes
are reported per node. [`view`](crates/view/src/lib.rs) mounts layout nodes,
defers child-list sync to the frame (keeping 10k-row reorders linear), and
exposes `set_viewport`, `set_scale`, `set_style`, and `layout_rect`.
[`text`](crates/text/src/lib.rs) shapes and wraps text behind a `Shaper` trait,
with a deterministic monospace backend for tests and a real `CosmicShaper`
(Harfrust shaping, system font fallback, swash glyph masks), width quantization,
cache counters, and an LRU byte budget. [`scene`](crates/scene/src/lib.rs) builds fill/border/text
primitives, records per-node paint ranges, replays clean subtrees, rebuilds dirty
paths, and reports damage. `view` integrates both: text measurement is cached,
and `build_scene` emits retained primitives with replay counters.

Tests cover flex grow/gap/padding/percent/absolute layout, text auto-size and
wrapping, paint-only no-layout, pixel snapping, resize/scale, cache hits and
eviction, paint replay versus full rebuild, damage, and scene construction
([layout.rs](crates/layout/tests/layout.rs),
[shaping.rs](crates/text/tests/shaping.rs),
[paint_cache.rs](crates/scene/tests/paint_cache.rs),
[layout_integration.rs](crates/view/tests/layout_integration.rs),
[scene_integration.rs](crates/view/tests/scene_integration.rs)).

The [`renderer-native`](crates/renderer-native/src/lib.rs) crate adds the
`PaintBackend`/`WindowDriver` contract, a recording `HeadlessBackend`, a wgpu
offscreen and surface backend, and a winit window loop behind the `window`
feature. Fills, borders, and text are batched into one vertex stream. Real shaped text is
rasterized into a shelf-packed alpha glyph atlas and sampled by the wgpu
pipeline; monospace/no-image glyphs retain a placeholder fallback. The offscreen
backend supports pixel readback. Tests in
[acceptance.rs](crates/renderer-native/tests/acceptance.rs) drive the real window
driver at 1×/1.5×/2×, assert paint-only zero layout and dirty-subtree replay
through the backend, check a 60 Hz CPU frame budget, verify a wgpu-drawn red
pixel, and render real platform glyphs at 1×/1.5×/2× (`Metal Apple M3 Pro`
locally). The `window` feature, [`examples/window.rs`](crates/renderer-native/examples/window.rs),
and [`examples/window_smoke.rs`](crates/renderer-native/examples/window_smoke.rs)
compile. The smoke run opened a Metal surface window, rendered real glyphs
(atlas populated with 12 glyphs), resized the surface 800×600 → 600×450 →
800×600 at the display's 2× scale, and exited cleanly.

The smoke example now reports per-frame timings. On the development MacBook Pro
(M3 Pro, built-in Retina display at 2×, Metal) a release run of 300 frames gave:
mean 16.798 ms / 59.5 fps with startup included, and a steady mean of
16.672 ms / 60.0 fps after skipping the first ten frames (p95 17.309 ms, steady max
20.629 ms). This is hardware evidence on an M3 Pro, not the 2019 target laptop.

Remaining Phase 5 work: a visible-window run on displays configured at 1×/1.5×,
hardware 60 fps acceptance on the target laptop, and optional damage
scissoring/partial present. The development machine exposes only a 2× Retina
display (`backingScaleFactor=2.0` for every available display mode), so the
visible 1×/1.5× run needs different hardware or a manual display configuration.
The glyph atlas, platform shaping, window loop, and offscreen/surface wgpu paths
are implemented.

### Phase 6 — Input, focus, accessibility (3–4 weeks)

**Build:** hitboxes and content masks, mouse/keyboard/text dispatch, focus
handles and tab stops, actions and key bindings, hover/drag state, AccessKit
tree, cursor/tooltip layers.

**Exit:** keyboard-only navigation works; screen reader reads a labelled form;
click/drag/scroll are correct under transforms and clipping; focus survives
list reorder.

**Status (2026-11-19): input/focus/a11y slice implemented.** [`view`](crates/view/src/lib.rs)
now has hit testing over layout bounds, pointer move/down/up and click
dispatch, hover/pressed state, view-level click listeners, focus handles and
tab traversal, disabled-node skipping, focused/global key bindings, focus
fallback when a focused row is removed, and an accessibility snapshot with
stable renderer ids, roles, labels, values, bounds, focus state, and
press/focus actions. [`renderer-api`](crates/renderer-api/src/lib.rs) owns the
key/modifier/pointer event types. [`renderer-native`](crates/renderer-native/src/lib.rs)
routes winit pointer and keyboard events through `WindowDriver` into the app;
[`examples/counter.rs`](crates/renderer-native/examples/counter.rs) is a native
two-instance counter with clickable buttons and key bindings.

Tests cover topmost hit testing, click dispatch, tab order and disabled nodes,
focused/global key bindings, a11y roles/labels/focus, a11y press/focus actions,
stable a11y ids across keyed reorder, focus preservation across reorder, focus
fallback on row removal, native window-driver pointer/key routing,
content-mask clipping and translation/scale transforms in hit testing and scene
building, capture/bubble dispatch with `Continue`/`Stop`/`Handled` results and
default-action suppression, and scroll state with clipped viewports, clamped
offsets, deepest-scrollable wheel routing, painted/hit-test translation, and
typed drag/drop sessions with start/move/end callbacks and payload validation,
and focused text-input/IME handlers with composition, commit/cancel, deletion,
selection, caret state, native winit IME routing, and AccessKit `TreeUpdate`
generation with roles, labels, values, bounds, disabled state, focus, and stable
keyed-row ids. The native driver applies updates through an `AccessKitAdapter`
boundary, and AccessKit press/focus/value/selection actions route back into the
retained input methods
([input.rs](crates/view/tests/input.rs),
[renderer-native/tests/input.rs](crates/renderer-native/tests/input.rs),
[acceptance.rs](crates/renderer-native/tests/acceptance.rs)). IME composition
text paints with an underline, focused carets paint as scene primitives, caret
arrow/Home/End movement supports Shift selection extension, and the caret
rectangle feeds the winit IME cursor area. Hovered nodes request pointer cursors
with ancestor inheritance, and delayed tooltips paint through a scene tooltip
primitive that the native backend renders and the window loop applies through
`set_cursor_icon`. Remaining Phase 6 work: a concrete OS AccessKit adapter,
richer hover/focus styling, and scroll-edge chaining/scrollbars.

### Phase 7 — Stores and authoring (3–4 weeks)

**Build:** `#[derive(Store)]`, `Field`/`Subfield`, `Patch`, keyed collections,
`store!`/`view!` ergonomics, props as signals, diagnostics for strict reads and
store aliasing.

**Exit:** field-granular notification verified; nested stores compose; keyed list
patch only dirties changed keys; the example app uses no manual `notify`.

### Phase 8 — Hardening and performance (ongoing)

**Build:** incremental layout experiments, damage regions, text cache eviction,
arena compaction only if measurements demand it, fuzzers, benchmarks, devtools.

**Exit:** published performance report; no unbounded growth in a 24-hour churn
test; third-party null renderer conformance suite is green.

### Stretch goals (not scheduled)

- **`view_builder!`** — a second proc-macro front-end that parses fluent
  builder-chain syntax (`Row.gap(8).child(Text.value(label))`) and emits the
  same direct `cx.element` / component-mount calls as `view!`. It is a syntax
  front-end, not a runtime builder, so runtime cost should match `view!` when the
  emitter is shared. It is explicitly unimportant and not in any phase: it adds a
  second syntax, documentation, and weaker IDE/type diagnostics inside the macro.
  Only pursue if fluent syntax is strongly requested.

### Milestones

| Milestone | Phase | Demo |
|---|---|---|
| M1 "Graph" | 1 | Headless reactive graph passes the ported core suite. |
| M2 "Navigation" | 2 | Async page swap with hold, boundary, and no torn frames. |
| M3 "Contract" | 3 | Same app runs on null renderer and records operations. |
| M4 "Tree" | 4 | Keyed list with stable identity and binding-level updates. |
| M5 "Pixels" | 5 | Native window with layout, text, and paint. |
| M6 "Interaction" | 6 | Keyboard, mouse, IME, and accessibility on the demo. |
| M7 "Stores" | 7 | Nested app state with field-granular updates. |
| M8 "Proof" | 8 | Benchmarks, fuzzers, and a 24-hour soak. |

The reference programs for M1–M7, with the expected frames and acceptance
checks, are in [docs/13-reference-apps.md](docs/13-reference-apps.md). The
counter is the M1/M3 app; states and cities is the M2/M4 app.

---

## 7. Risk register

| Risk | Likelihood | Impact | Mitigation | Trigger to change course |
|---|---|---|---|---|
| Hold/async semantics are too large for v1 | High | High | Ship a defined subset (`F-1,2,3,4,5,9,10,12`) and gate optimistic lanes/actions/`latest`. Keep the node shape extensible. | If Phase 2 exceeds 7 weeks, cut `isPending` to a read-only diagnostic and ship navigation holds only. |
| Retained tree structural updates are harder than expected | High | High | Start with keyed list + `Show` only; disallow arbitrary re-parenting; keep cached-rebuild escape hatch. | If keyed list reorder is not stable by Phase 4 exit, fall back to GPUI-style per-view rebuild for that subtree. |
| Incremental layout is not achievable with taffy | Medium | Medium | Treat full layout on layout-dirty frames as v1; add layout boundaries and measure caching later. | If layout exceeds 30% of frame time in the demo, add a dirty-subtree layout prototype before Phase 8. |
| Text shaping dominates frame time | Medium | Medium | Cache shaped lines by content+font+width; keep a platform text system boundary; profile early. | If shaping > 5 ms/frame, move to a retained line layout with incremental reflow. |
| Store macro complexity delays app work | Medium | Medium | Ship flat `create_signal()` pairs first; stores are opt-in. | If derive macro exceeds 3 weeks, ship a `#[derive(Store)]` subset (plain fields + Vec) and defer enums/patch. |
| wgpu backend blocks on platform issues | Low | Medium | Null renderer and scene recorder are first-class; a CPU rasterizer test backend keeps CI green. | If wgpu is unstable on a target, add a `tiny-skia` software backend for that platform. |
| Unsafe arena code has soundness bugs | Medium | High | Safe indices first; isolate `unsafe` in `arena`; Miri; no `unsafe` in graph logic. | Any Miri failure blocks release until fixed or the `unsafe` is removed. |
| Public API churn | High | Medium | Mark everything `#[doc(hidden)]` or `unstable` until Phase 6; semver only after M6. | If third-party renderer authors need stability earlier, freeze `renderer-api` after Phase 3. |

---

## 8. Performance budgets (v1 targets)

Measured on a 2021-class laptop, release build, 1600×1000 window.

| Metric | Target | Notes |
|---|---|---|
| Idle CPU | < 0.5% | No frame unless dirty or animating. |
| Signal read | < 20 ns | Hot path, single-threaded. |
| Signal write + dirty walk | < 100 ns for 1 dependent | 1k dependents < 20 µs. |
| Memo recompute overhead | < 100 ns + user fn | Excludes user code. |
| 10k-row mount | < 150 ms | Including layout and text measure, text simple. |
| 10k-row single-row update | < 1 ms | Binding + layout if needed. |
| Paint-only update | < 2 ms | No layout, scene patch only. |
| Full layout of 10k nodes | < 20 ms | Taffy, simple flex. |
| Frame time, demo | < 8 ms p95 | At 60 Hz. |
| Allocations per paint-only frame | 0 in steady state | Arena reuse; no per-frame element tree. |

### 8.1 Resource model (estimates, not measurements)

These are the expected ranges for the architecture as specified, before the
optimizations listed at the end. They are ±30–50%; GPU driver memory is the
largest platform-specific term and is often outside our control.

**Framework heap.** A useful first-order model:

```
heap ≈ 2 MB
     + graph_nodes        × (180–250 B + value size)
     + edges              × 56 B
     + owners             × (120–180 B)
     + view_nodes         × (700–1100 B)
     + extra bindings     × 30 B          # first four are inline
     + taffy_nodes        × (200–400 B)
     + scene_primitives   × (64–128 B)    # double-buffered
     + text_cache                         # bounded: 10–32 MB
```

The view-node term dominates because it includes a resolved `Style`, layout
state, paint cache, and up to four inline bindings. A non-virtualized row with
six elements costs roughly **6–12 KB** all-in (view nodes, graph nodes, taffy
nodes, bindings, scene). That is the number to remember for list sizing.

**Process RSS.** Add the platform baseline:

| Scenario | Framework heap | Process RSS (incl. GPU/driver/fonts) |
|---|---:|---:|
| Empty window | < 2 MB | 40–90 MB |
| Small app (1 screen, < 1k view nodes) | 1–4 MB | 60–120 MB |
| 10k-row table, not virtualized | 60–120 MB | 150–300 MB |
| 100k-row table, virtualized | 10–40 MB + data | 100–250 MB |
| 100k-row table, not virtualized | 0.6–1.2 GB | 0.8–1.5 GB |

The base is dominated by the GPU driver, swapchain, and font/text system, not by
the reactive core. An empty window should not be blamed on the framework.

**GPU memory.** Roughly:

```
driver + device       20–60 MB
pipelines/shaders      5–15 MB
swapchain              2–3 × width × height × 4 B
glyph atlas + buffers  2–20 MB
```

At 1600×1000 the swapchain is 13–19 MB; at 4K it is 66–100 MB. On unified-memory
platforms this appears in RSS; on discrete GPUs it is VRAM.

**CPU.** Idle app CPU is 0.0–0.1% (the host event loop may show 0.1–0.3%). A
small interaction costs 0.5–3 ms end-to-end; a paint-only frame 0.3–2 ms; a
10k-node layout 10–20 ms. Signal read is 10–30 ns, a write with one dependent
50–200 ns, a binding apply 0.2–1 µs. User code dominates normal actions.

**GPU activity.** Idle: zero draw calls and no app GPU work. v1 presents the
whole scene on any dirty frame, so a small change redraws the full window; that
is avoidable overdraw, not idle load. Damage regions/scissor are a v2
optimization. Text atlases upload only new glyphs. Continuous animation or a
blinking caret wakes frames deliberately; without them the app is GPU-idle.

**Levers if the numbers are too high.** In order of impact: virtualize long
lists (bounds view nodes); intern/share resolved styles (`Arc<ResolvedStyle>` or
a `StyleId` table) to shrink view nodes; use shallow stores for record-granularity
data; reduce inline bindings; add damage regions for GPU overdraw.

---

## 9. Decisions from the open questions

These were open in Draft 1 and are now resolved; the detailed rationale is in
[docs/12-decisions-and-risks.md](docs/12-decisions-and-risks.md) Part C and the
section docs.

1. **Component rebuild.** No special rebuild primitive. A `ViewSlot` replaces a
   subtree under a stable host; `Rebuild` is sugar over it. Solid uses
   `Show`/`Switch`/`dynamic()`/function children; Leptos uses specialized control
   flow and `AnyView`/`Either`. Neither needs a generic control-flow base. See
   [docs/10-macros-and-authoring.md](docs/10-macros-and-authoring.md) §7.2.
2. **Props representation.** All props are `Prop<T>` (`Static(T)`,
   `Reactive(ReadSignal<T>)`, `Derived(Memo<T>)`). No bare-`T` props for values that
   may change; static props allocate no node. See
   [docs/10-macros-and-authoring.md](docs/10-macros-and-authoring.md) §3.2.
3. **Async read type.** `get() -> T` is the only public value read; inside a
   tracked computation it suspends on `NotReady`, implemented by a private token
   caught at the computation boundary. `is_pending` is the verdict. There is no
   public `try`/`Result` read and no defaulting read; a default would lie about
   the graph. `Option`/`Result` are rejected for the public shape because they
   color every helper. `panic = "abort"` builds use a boundary/domain state or an
   internal fallback. Implementation paths are in
   [docs/04-async-model.md](docs/04-async-model.md) §3.6.
4. **Boundary scope.** v1 implements first-load, held navigation, disarmed
   forwarding, errors, and a `key` that remounts the boundary. Full `on` re-arm
   and `latest` are v2. See
   [docs/04-async-model.md](docs/04-async-model.md) §6.1 and §7.2.
5. **Dynamic stores.** The static macro path is v1; `DynamicStore` with an
   interned path map is v2 unless a real app needs it in v1. If the full
   interner is too complex, fall back to the Leptos `StorePath`/`TriggerMap`
   design. See [docs/03-store-layer.md](docs/03-store-layer.md) §5.
6. **Threading.** Background work is allowed. The graph and view tree remain
   UI-thread-affine; results return as continuations. The host supplies a
   background spawner in addition to the UI-local one. Cancellation is
   cooperative: the framework drops/aborts the flight and ignores stale results;
   long-running threads must check the flight's cancellation token. See
   [docs/04-async-model.md](docs/04-async-model.md) §3.1 and §3.5.
7. **Renderer contract batching.** `begin_frame`/`end_frame` stay optional; the
   native path uses its own frame. No required per-backend batching API.
8. **License and attribution.** GPL-2.0-or-later; one `THIRD_PARTY.md` plus module
   headers for ported MIT code. GPUI is reference-only because Apache-2.0 is not
   GPLv2-compatible. See `LICENSE` and
   [docs/reference-repos.md](docs/reference-repos.md).

### 9.1 Remaining open questions

The section docs still contain leaning decisions and genuinely open questions.
They are consolidated in
[docs/12-decisions-and-risks.md](docs/12-decisions-and-risks.md) Part F,
grouped into resolved, leaning, genuinely open (with next step), and deferred.
The headline open items are: equality/comparator API, memo read representation,
colorless suspension implementation, transaction merge criterion, re-ask
detection, child-order representation, text engine, incremental layout, dispatch
tree vs parent walk, and store field split.

---

## 10. Documentation map

| Document | Contents |
|---|---|
| [ARCHITECTURE.md](ARCHITECTURE.md) | System overview, crate graph, core types, frame lifecycle, invariants. |
| [docs/00-glossary.md](docs/00-glossary.md) | Terms used across the docs. |
| [docs/01-reactive-core.md](docs/01-reactive-core.md) | Node/edge model, height heap, read/write path, effects. |
| [docs/02-ownership-and-lifetimes.md](docs/02-ownership-and-lifetimes.md) | Arena, handles, owners, cleanup, reclamation. |
| [docs/03-store-layer.md](docs/03-store-layer.md) | Derived stores, field paths, patches, keyed collections. |
| [docs/04-async-model.md](docs/04-async-model.md) | Spawner, flights, holds, boundaries, `isPending`. |
| [docs/05-runtime-and-frame-scheduler.md](docs/05-runtime-and-frame-scheduler.md) | Runtime ownership, frame phases, wakeups, idle. |
| [docs/06-renderer-contract.md](docs/06-renderer-contract.md) | Universal traits, capabilities, conformance. |
| [docs/07-retained-renderer.md](docs/07-retained-renderer.md) | Retained tree, bindings, lists, paint cache. |
| [docs/08-layout-and-text.md](docs/08-layout-and-text.md) | Taffy, dirty layout, text shaping and caching. |
| [docs/09-input-focus-accessibility.md](docs/09-input-focus-accessibility.md) | Hit testing, dispatch, focus, IME, AccessKit. |
| [docs/10-macros-and-authoring.md](docs/10-macros-and-authoring.md) | `view!`, components, props, control flow, diagnostics. |
| [docs/11-testing-and-performance.md](docs/11-testing-and-performance.md) | Test pyramid, conformance, fuzzers, benchmarks. |
| [docs/12-decisions-and-risks.md](docs/12-decisions-and-risks.md) | ADR index, risk detail, open questions. |

---

## 11. Definition of done for v1

- The demo app exercises state, async navigation, a 10k-row list, text input,
  keyboard navigation, and accessibility.
- The reactive core passes the ported fundamental and derived rule suites for the
  v1 subset, plus the fuzz corpus.
- The renderer passes incremental-vs-full scene equivalence, leak checks, and
  the randomized tree fuzzer.
- Idle CPU and frame budgets in §8 are met on the reference machine.
- A third-party renderer can be implemented from `docs/06-renderer-contract.md`
  alone and passes the null-renderer conformance suite.
- Every `unsafe` block has a safety comment and a Miri-covered test.
