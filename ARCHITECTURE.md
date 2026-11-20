# ARCHITECTURE

**Status:** Draft 1.
**Read with:** [`PLAN.md`](PLAN.md). Section detail lives in [`docs/`](docs/README.md).

This document is the map. It states the system's shape, the ownership of each
piece, the frame lifecycle, and the invariants everything else must preserve. It
does not repeat the algorithms in detail; each section doc owns its algorithm.

---

## 1. The shape in one picture

```
                         ┌─────────────────────────────────────────────┐
                         │                 runtime                     │
                         │  frame loop · queues · clock · wakeups      │
                         └───────────────┬─────────────────────────────┘
                                         │ drives
        ┌────────────────────────────────┼────────────────────────────────┐
        │                                │                                │
        ▼                                ▼                                ▼
┌───────────────┐              ┌──────────────────┐              ┌────────────────┐
│ reactive-core │◄────────────►│   view / tree    │─────────────►│ renderer-api   │
│ graph + owners│   bindings   │ retained nodes   │   commands   │ (3rd parties)  │
└───────┬───────┘              └────────┬─────────┘              └────────────────┘
        │                               │
        │ values                        │ layout+scene
        ▼                               ▼
┌───────────────┐              ┌──────────────────┐              ┌────────────────┐
│reactive-stores│              │  layout + text   │─────────────►│ renderer-native│
│ derive macros │              │ taffy + shaping  │              │ scene + wgpu   │
└───────────────┘              └──────────────────┘              └────────────────┘
```

Three ledgers must be kept in sync, and only one of them is authoritative:

- **The graph** is authoritative for values and invalidation.
- **The view tree** is authoritative for identity, structure, and which binding
  observes which node.
- **The scene** is derived output. It is never read back into the graph.

The rule that keeps this tractable: **a value flows graph → view binding → scene,
never scene → view → graph**. Input events are the only external writes; they
enter as ordinary signal writes.

---

## 2. The four models

### 2.1 Reactive graph

A directed acyclic graph of nodes (signals, memos, effects). Edges point from a
dependency to a subscriber. Each node has:

- a stable `NodeId` (index + generation);
- a value slot (committed, and optionally staged);
- a dirty state (`Clean`, `Check`, `Dirty`);
- a topological `height` used by the scheduler;
- intrusive dependency and subscriber edge lists;
- an owner (`OwnerId`) and optional async flight state.

The graph is owned by `Runtime`, not by thread-local globals. A `Cx` (context)
borrows the runtime for the duration of a call. Handles are `Copy` and carry a
generation; they do not borrow the graph.

### 2.2 Ownership tree

An arena-backed tree of owners, mirroring the lifetime of component scopes.
Effects, memos, signals created during a computation, context values, and
cleanup callbacks belong to the current owner. Re-running a computation disposes
its previous owner's children and cleanups first. Disposing an owner disposes
its subtree depth-first, children before self, cleanups LIFO.

The ownership tree is what gives Rust "GC by UI lifetime" without reference
cycles. It is not the same as the reactive graph: the graph can outlive an owner
only through handles, and those handles become invalid on disposal.

### 2.3 Retained view tree

A tree of `ViewNode`s with stable `NodeId`s:

- element nodes, text nodes, fragments, components, conditional nodes, list
  nodes, portals/overlays;
- persistent style, layout result, paint cache, and child order;
- a list of `Binding`s, each an effect that writes one dynamic property of the
  node (text, attribute, style field, visibility, child range).

Structure is built at mount. Dynamic values update through bindings. A component
may opt into whole-subtree rebuild when that is cheaper; the tree then replaces
the cached subtree under the same parent.

### 2.4 Scene and layout

- **Layout** is a persistent `TaffyTree` whose node IDs map 1:1 to view nodes.
  Style changes update taffy styles and mark layout dirty. Layout runs for
  layout-dirty roots; clean subtrees keep their previous geometry.
- **Scene** is a frame-local list of paint primitives with stable ranges per view
  node. Clean nodes replay their previous range; dirty nodes re-emit. The scene
  is sorted into batches for the GPU backend.

Scene and layout are caches. Dropping either must be recoverable from the view
tree and styles alone; this is the invariant that keeps caching honest.

---

## 3. Crates and dependency rules

```
reactive-core
  ├── reactive-ownership      (may be a module of core in v0.1)
  ├── reactive-stores         (depends on core; macro crate separate)
  ├── renderer-api            (no dependency on core)
  ├── view                    (core + renderer-api)
  ├── layout                  (view types only; taffy)
  ├── text                    (layout types only; platform text trait)
  ├── runtime                 (core + view + renderer-api + platform)
  ├── renderer-null           (renderer-api)
  ├── renderer-native         (view + layout + text + platform + wgpu)
  ├── components              (view + reactive-core; Select, ListBox, Menu, …)
  ├── platform                (winit/window/input/accessibility)
  └── prelude                 (re-exports)
```

Hard rules:

1. `reactive-core` **must not** depend on any rendering, view, or platform crate.
2. `renderer-api` **must not** depend on `reactive-core`. A third-party renderer
   should be implementable without the graph.
3. `view` may depend on `renderer-api` and `reactive-core`; it must not depend on
   a concrete platform, GPU backend, or layout engine. `view` defines `Style`,
   `LayoutState`, and `ViewNodeId`; `layout` depends on those types and on taffy,
   never the reverse. This keeps the retained tree independent of the layout
   implementation.
4. `renderer-native` may bypass `renderer-api` internally. If it implements the
   universal contract, that implementation is an adapter, never the hot path.
5. `runtime` is the only crate that owns the frame loop and the host boundary.
6. `components` is a higher layer than the element vocabulary. `Select`,
   `ListBox`, `Menu`, `Dialog`, `ScrollView`, and similar widgets are composed
   from `view` nodes and `reactive-core`; they should not add an `ElementKind`
   or a `renderer-api` capability unless a new native primitive is unavoidable.
   The element set stays small and backend-stable.

Workspace layout:

```
crates/
├── reactive-core/          # graph, edges, heap, signals, memos, effects
│   └── ownership/          # arena, owners, cleanup, context (module in v0.1)
├── reactive-stores/        # Store, Field, Patch, keyed collections
├── reactive-stores-macro/  # #[derive(Store)], #[derive(Patch)]
├── renderer-api/           # object-safe Renderer + capabilities
├── view/                   # ViewNode, bindings, Show/For, mount lifecycle
├── layout/                 # style resolution + taffy integration
├── text/                   # shaping, measurement, line cache
├── runtime/                # frame loop, queues, spawner, clock
├── platform/               # window, input, IME, AccessKit adapters
├── renderer-native/        # retained tree backend, scene, wgpu paint
├── renderer-null/          # recording renderer + conformance target
├── components/             # Select, ListBox, Menu, Dialog, ScrollView
├── macros/                 # view!, #[component]
└── prelude/                # re-exports
examples/
└── demo/                   # the app used for milestones and benchmarks
```

Optional split if compile times demand it: move `layout` and `text` under
`renderer-native` as modules first; split them out only when a third-party
renderer needs them.

Optional split if compile times demand it: move `layout` and `text` under
`renderer-native` as modules first; split them out only when a third-party
renderer needs them.

---

## 4. Core types (shape, not final signatures)

### 4.1 Handles and arenas

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId {
    index: u32,
    generation: u32,
}

pub struct Arena<T> {
    slots: Vec<Slot<T>>,       // value + generation + occupied
    free: Vec<u32>,            // free indices
}

pub struct Handle<T> {
    id: NodeId,
    _marker: PhantomData<fn() -> T>,
}
```

`Handle<T>` is `Copy` and `'static`. Resolving a handle checks the generation;
a disposed or reused slot returns `None` (or an error in checked APIs). This is
the "no resurrection" rule.

View nodes use a distinct `ViewNodeId` with the same index + generation shape.
The graph and view arenas are separate; mixing their IDs is a type error, not a
runtime lookup.

### 4.2 Graph node

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
    pub cold: RefCell<Option<Box<ColdData>>>,
}

pub enum NodeKind {
    Signal,
    Memo { compute: Box<dyn FnMut(Option<&dyn Any>) -> Box<dyn Any>> },
    Effect { compute: Box<dyn FnMut() -> Box<dyn Any>>,
             apply: Box<dyn FnMut(&dyn Any)> },
    RenderEffect { apply: Box<dyn FnMut(&dyn Any)> },
}
```

Notes:

- `value` is `Option<Box<dyn Any>>` in v0.1. A later optimization can add typed
  slots or `SmallBox` without changing the handle API.
- `cold` holds async and diagnostic data that most nodes never use, mirroring
  Solid's node extension (`refs/solid/packages/signals/docs/NODE-SHAPE.md`).
- Do not put rarely-used fields on the hot literal. Measure before adding.

### 4.3 Edge

Edges live in their own arena so unlink is O(1) and iteration preserves creation
order (Leptos calls out order sensitivity for nested effects in
`reactive_graph/src/graph/sets.rs`).

```rust
pub struct Edge {
    pub dep: NodeId,
    pub sub: NodeId,
    pub next_dep: Option<EdgeId>,
    pub prev_dep: Option<EdgeId>,
    pub next_sub: Option<EdgeId>,
    pub prev_sub: Option<EdgeId>,
    pub generation: u32,
}
```

Each node stores `deps_head/deps_tail` and `subs_head/subs_tail`. The lists are
intrusive, stable, and allocation-free after the first edges.

### 4.4 Owner

```rust
pub struct Owner {
    pub id: OwnerId,
    pub parent: Option<OwnerId>,
    pub first_child: Option<OwnerId>,
    pub next_sibling: Option<OwnerId>,
    pub cleanups: Vec<Cleanup>,
    pub context: TypeMap,
    pub disposed: bool,
}
```

The current owner is a field on `Cx`, not a thread-local, so tests and multiple
runtimes are isolated. `run_with_owner` restores the previous owner on drop.

### 4.5 View node

```rust
pub struct ViewNode {
    pub id: ViewNodeId,                // distinct arena from graph NodeId
    pub parent: Option<ViewNodeId>,
    pub first_child: Option<ViewNodeId>,
    pub last_child: Option<ViewNodeId>,
    pub next_sibling: Option<ViewNodeId>,
    pub prev_sibling: Option<ViewNodeId>,
    pub kind: ViewKind,
    pub style: Style,
    pub layout: LayoutState,
    pub paint: PaintCache,
    pub bindings: SmallVec<[Binding; 4]>,
    pub flags: ViewFlags,
}

pub struct Binding {
    pub effect: EffectHandle,
    pub target: BindingTarget,   // Text | Prop(PropId) | Style(StyleField) | Visibility | ChildRange
}
```

Sibling links rather than `Vec<NodeId>`: O(1) insert/remove, stable order, and no
O(n) splice on keyed reorder. Iteration is cache-friendly enough because nodes
are arena-resident; a later "flattened sibling vec" optimization is possible.

### 4.6 Component

A component is a function run once under an owner, returning a `ViewNode`
subtree. It may create signals, memos, and bindings. It is not re-run on every
change; structural replacement is an explicit `ViewSlot`. Props are
`Prop<T>` values (`Static`, `Reactive`, `Derived`), not bare `T` for values that
may change.

```rust
pub trait Component: 'static {
    type Props: 'static;
    fn mount(props: Self::Props, cx: &mut Cx) -> ViewNodeId;
}
```

The `#[component]` macro generates the props struct and the mount shim. A
`ViewSlot` (or its `Rebuild` sugar) replaces a subtree when needed; there is no
special rebuild component mode. Control-flow nodes (`Show`, `Switch`, `For`) are
specialized and do not require a `ViewSlot`; a Solid-style `dynamic()`
component-switch helper would be a thin wrapper over one.

---

## 5. Frame lifecycle

The runtime exposes one function, conceptually:

```rust
pub fn frame(&mut self, cx: &mut Cx, cause: FrameCause) {
    self.pump_async(cx);       // 1. continuations, timers, host events
    self.flush_reactive(cx);   // 2. promote writes, mark, settle
    self.apply_effects(cx);    // 3. user effects and render bindings
    self.layout(cx);           // 4. only layout-dirty roots
    self.build_scene(cx);      // 5. patch/replay paint ranges
    self.present(cx);          // 6. hand scene to the backend
}
```

### 5.1 Phase details

| Phase | Work | Can write signals? | Can read staged values? |
|---|---|---|---|
| 1. Pump async | Drain continuation queue; promote completed flights to staged values; dispatch host events. | Yes (event handlers) | No |
| 2. Flush reactive | Promote staged writes; propagate dirty/check; run memo compute heap; decide holds; commit or park transactions. | No (writes are queued for next flush) | Compute passes see committed + staged as rules dictate |
| 3. Apply effects | Run effect apply halves in queue order; render bindings write view/scene state. | Queued for next flush (debug error in owned scope) | Reads see the committed frame |
| 4. Layout | Resolve style; compute taffy layout for dirty roots; update geometry. | No | N/A |
| 5. Scene | Re-emit dirty node paint; replay clean ranges; sort/batch. | No | N/A |
| 6. Present | Submit GPU commands; schedule next wake if dirty/animated. | No | N/A |

### 5.2 Why this order

- Async completions must become staged values before the graph runs, otherwise a
  completion could land mid-settlement and tear the frame.
- All compute passes must finish before any effect apply, because an apply may
  read a memo that another apply also reads. Solid 2.0 split effects for exactly
  this reason (`documentation/solid-2.0/01-reactivity-batching-effects.md`).
- Layout must follow all style bindings; paint must follow all layout.
- No phase may observe an output of a later phase.

### 5.3 Reentrancy rules

- A write during phase 2 is a bug: `debug_assert` + dev diagnostic; in release it
  is staged for the next frame.
- A write during phase 3 is allowed only outside an owned scope (event handlers
  and effect apply bodies are owned scopes; internal state uses
  `owned_write`-equivalent permit).
- `flush()` is not reentrant. Calling it from an effect apply is a no-op with a
  dev warning. Tests use `runtime.frame(FrameCause::Manual)`.
- Destroying a view node during phase 3 is deferred to the end of the phase, so
  no binding iterates a disposed node.

### 5.4 Idle and wakeups

The runtime sleeps when:

- no continuation is ready;
- no staged write or dirty node exists;
- no animation is active;
- no layout/paint work is pending.

Wake sources: host input, timer, async completion, animation frame request, and
explicit `request_frame()`. The host's event loop owns the actual wait. The
runtime never polls.

---

## 6. Threading

- The graph, view tree, layout, and scene are **thread-affine to the UI thread**.
- Handles are `Copy` but not `Send` in v0.1. A background task may return a plain
  value; the continuation is posted to the UI queue, and the value becomes a
  staged write there.
- `Spawner` is a trait implemented by the host:

```rust
pub trait Spawner {
    fn spawn_local(&self, fut: LocalBoxFuture<'static, ()>);
    fn wake_ui(&self);
}

/// Optional; host-provided thread pool / tokio / rayon.
pub trait BackgroundSpawner {
    fn spawn(&self, fut: BoxFuture<'static, ()>) -> TaskHandle;
}
```

- `spawn_local` is for UI-affine futures (async derived nodes). It must not block.
- `BackgroundSpawner` is for `Send` work. The result returns through a
  continuation; the background future never touches a graph handle.
- If the host provides no background spawner, user code may use its own executor
  and send the result back through a `UiHandle`/channel.

- `ManualSpawner` for tests runs futures only when `pump` is called; this makes
  async tests deterministic.
- A multi-threaded graph is an explicit non-goal (see PLAN D11). Background
  computation is not a multi-threaded graph.

The only thread-local in the design is the executor context used while polling a
local future. It carries a runtime pointer so `ScopedFuture` can set the node's
owner and observer; it does not own the graph or make state global.

---

## 7. Error model

Errors fall into four classes, each with one channel:

1. **User computation error.** A memo/effect compute returns `Err` or panics.
   Memos store the error as their value; readers either propagate it or an
   `ErrorBoundary` catches it. Effects log at the boundary and skip the apply
   unless an error handler is provided.
2. **Reactive misuse.** Write in computation, read of a disposed handle, stale
   owner, or dependency cycle. These are bugs, not recoverable state. Debug:
   panic with a diagnostic code and owner path. Release: stage/ignore and log
   once (`REACTIVITY_HALTED` for an uncaught error that escapes the root).
3. **Renderer contract error.** A third-party renderer returns `Err`; the
   runtime replaces the subtree with an error placeholder and reports once. It
   does not unwind the graph.
4. **Host/platform error.** Window creation, GPU device loss, font load. The
   runtime surfaces to the host; the app decides whether to retry or exit.

No `catch_unwind` around user code in the hot path. Panics are bugs; a panic
hook records the current owner and node for diagnostics.

---

## 8. Identity and lifetimes

| Entity | Identity | Lifetime | Stale access |
|---|---|---|---|
| Graph node | `NodeId(index, gen)` | Until disposed or owner disposed | `None` / `Disposed` error |
| Owner | `OwnerId` | Subtree scope | `None` |
| View node | `NodeId` in view arena | Until removed from tree | `None` |
| Element state (scroll, focus) | `ViewNodeId` | View node | Dropped with node |
| Async flight | `(NodeId, u64 generation)` | Until completion or supersede | Dropped |
| Binding | `EffectHandle` | View node | Disposed with node |

**Key rule:** a handle is never evidence that the node is alive. It is a lookup
key. This is why no compaction is needed and why memory is reclaimed on
disposal. It also means a user cannot "resurrect" a disposed computation by
reading a retained handle, unlike a JS implementation where a copied reference
can keep a node reachable.

---

## 9. Invariants

These are asserted in debug builds and tested in CI. A change that violates one
is a design change, not a bug fix.

| ID | Invariant |
|---|---|
| I1 | A frame never shows a value beside a stale derivation of it (`F-1`). |
| I2 | A write is invisible to every reader until the flush that carries it (`F-10`). |
| I3 | A node's dependency edges are exactly the reads of its last committed pass; an unfinished pass's edges are trimmed at commit. |
| I4 | A dirty node's height is greater than every dependency's height in the same heap run; a stale height is corrected before execution. |
| I5 | Disposing an owner disposes children before self and runs cleanups LIFO. |
| I6 | A disposed handle resolves to `None`; a slot's generation never repeats. |
| I7 | Every view node has exactly one parent, except the root; sibling links are acyclic and consistent in both directions. |
| I8 | A binding's effect is disposed when its view node is removed, and never runs after disposal. |
| I9 | A clean view node's paint range replays exactly the primitives a full rebuild would produce with the same inputs. |
| I10 | Layout runs only when a style or structural change marks a layout-dirty root; paint-only changes never run layout. |
| I11 | The scene is a pure function of the view tree, styles, layout, and text caches; it is never an input to the graph. |
| I12 | No framework-owned thread polls; every wake is caused by input, a timer, an async completion, or an explicit frame request. |

---

## 10. Reference mapping

This table is the quickest way to find the upstream design when implementing a
module. Paths are relative to `refs/`.

| Our concept | Solid 2.0 | Leptos | GPUI |
|---|---|---|---|
| Node + edge graph | `packages/signals/src/core/types.ts`, `graph.ts`, `core.ts` | `reactive_graph/src/graph/{node,source,subscriber,sets}.rs` | — |
| Dirty heap | `packages/signals/src/core/heap.ts` | — (uses dirty flags + `update_if_necessary`) | — |
| Owner tree / cleanup | `packages/signals/src/core/owner.ts` | `reactive_graph/src/owner.rs` | Entity lifecycle (`app/entity_map.rs`) |
| Effect split | `packages/signals/src/signals.ts` (`createEffect`) | `reactive_graph/src/effect/{effect,render_effect}.rs` | `Render`/`cx.notify` |
| Async memo / flights | `packages/signals/src/core/async.ts` | `reactive_graph/src/computed/async_derived/*` | `cx.spawn` / `Task` |
| Boundaries | `packages/signals/src/boundaries.ts` | `leptos/src/suspense_component.rs`, `error_boundary.rs` | — |
| Store paths | `packages/signals/src/store/*` | `reactive_stores/src/{field,keyed,path,patch}.rs` | — |
| Derive macros | — | `reactive_stores_macro/src/lib.rs` | `gpui_macros/src/derive_render.rs` |
| Universal renderer | `packages/universal/src/universal.ts` | `tachys/src/renderer/mod.rs` | — |
| Retained view identity | — | — | `crates/gpui/src/view.rs`, `app/entity_map.rs` |
| Per-view cache | — | — | `crates/gpui/src/view.rs` (`ViewElementState`, `reuse_prepaint/paint`) |
| Scene / replay | — | — | `crates/gpui/src/scene.rs` (`replay`, `batches`) |
| Layout | — | — | `crates/gpui/src/taffy.rs` |
| Text | — | — | `crates/gpui/src/text_system.rs` |
| Hit testing / dispatch | — | — | `crates/gpui/src/window.rs` (`Hitbox`, `hit_test`), `key_dispatch.rs` |
| Accessibility | — | — | `crates/gpui/src/window/a11y.rs` |
| Test harness | — | — | `crates/gpui/src/app/test_context.rs`, `test.rs`, `randomized_element_tree.rs` |

---

## 11. Naming

Working names used in these docs:

- **Runtime** — the object that owns the graph, view tree, layout, scene, and
  frame loop.
- **Cx** — the per-call context that borrows the runtime and the current owner.
- **Node** — a reactive graph node unless qualified as **view node**.
- **Owner** — a lifetime scope in the ownership tree.
- **Binding** — an effect that writes one dynamic property of a view node.
- **Mount** — the one-time construction of a component's subtree.
- **View slot** — a stable host whose one child subtree is replaced when its key
  changes. `Rebuild` is sugar over it. Control-flow nodes are separate,
  specialized primitives.
- **Hold** — a staged change whose reveal waits for async work.
- **Boundary** — a `LoadingBoundary` or `ErrorBoundary` view.
- **Frame** — one pass through the six phases.

Crate names:

- `reactive-core`, `reactive-stores`, `reactive-stores-macro`
- `renderer-api` (formerly "universal")
- `view`, `layout`, `text`, `runtime`, `platform`
- `renderer-native` (formerly `renderer-gpui`), `renderer-null`
- `prelude`

---

## 12. What this architecture deliberately does not do

- It does not rebuild the element tree every frame.
- It does not diff a virtual tree.
- It does not make the renderer a second reactive system.
- It does not hide an executor, thread pool, or timer service.
- It does not promise that a disposed handle can come back.
- It does not use a global thread-local graph; multiple runtimes can exist in one
  process (important for tests and embedded use).
- It does not require `Send`/`Sync` for UI state.
- It does not implement optimistic lanes in v0.1.
