# 05 — Runtime and Frame Scheduler

**Crate:** `runtime`
**Depends on:** `reactive-core`, `view`, `renderer-api`, `platform`
**Owns:** the frame loop, queues, clock, frame phases, wake sources, idle
behavior, and the host boundary.

**Implementation status (2026-11-19).** The view/layout/text/scene frame phases
are implemented. The host boundary now has a `WindowDriver` and `PaintBackend`
contract in [`renderer-native`](../crates/renderer-native/src/lib.rs), with a
recording headless backend, a wgpu offscreen/surface backend, and a winit window
loop behind the `window` feature. Runtime async pumps and the host event queue
remain in the existing `runtime` crate; input dispatch is Phase 6.

---

## 1. Runtime responsibilities

The runtime is the only object that knows all of:

- the reactive graph and its flush;
- the view tree and its mount/dispose lifecycle;
- async continuations and timers;
- layout and scene building;
- the host window and GPU backend.

It does **not** own application state, the DOM, or a thread pool.

```rust
pub struct Runtime {
    pub graph: Graph,
    pub views: ViewArena,
    pub owners: OwnerArena,
    pub layout: LayoutEngine,
    pub scene: Scene,
    pub scheduler: Scheduler,
    pub spawner: Box<dyn Spawner>,
    pub clock: Clock,
    pub pending: VecDeque<Message>,
    pub frame_id: FrameId,
    pub mode: RuntimeMode,
    pub backend: Box<dyn PaintBackend>,
}
```

## 2. Frame phases

```
┌──────────────┐
│ 1. pump      │ drain continuations, timers, host events
├──────────────┤
│ 2. flush     │ promote writes → mark → compute heap → settle holds
├──────────────┤
│ 3. apply     │ render-effect applies → user-effect applies
├──────────────┤
│ 4. layout    │ resolve styles → taffy for dirty roots
├──────────────┤
│ 5. scene     │ re-emit dirty paint → replay clean ranges → batch
├──────────────┤
│ 6. present   │ submit to backend → schedule next wake
└──────────────┘
```

### 2.1 Phase 1 — pump

Inputs:

- `Message::Landed` from async flights;
- `Message::Timer` from the clock;
- `Message::HostEvent` from the platform (resize, scale, input, IME, a11y);
- `Message::Wake` from the host's event loop.

Work:

- Apply landed values to their nodes as **staged** values (never committed
  directly). A stale flight id is dropped.
- Apply timers; a timer callback may stage writes.
- Dispatch input events: hit test, focus, action/key dispatch, text input. Event
  handlers may stage writes.
- Update window metrics (size, scale, appearance) and mark layout dirty if
  needed.

No graph compute runs here. A write is just staged.

### 2.2 Phase 2 — flush

Work:

1. Promote staged signal writes (equality gate).
2. Mark subscribers dirty/check.
3. Run the compute heap (memos, effect computes, render-effect computes).
4. Settle holds: decide which transactions commit, which park.
5. Commit staged values for committed transactions.
6. If more writes were staged during compute (owned-write/internal), loop,
   bounded.

Output: the set of effect applies to run, the set of view nodes whose bindings
changed, and the set of disposed nodes.

No layout, no paint.

### 2.3 Phase 3 — apply

Work:

- Run render-effect applies in queue order. A binding writes its target:
  - text content;
  - an attribute/style field;
  - visibility;
  - a child-range replacement;
  - scroll offset (clamped).
- Mark the view node dirty according to the target:
  - text/attribute/color: paint dirty;
  - size/padding/flex/children: layout dirty + paint dirty;
  - visibility `none`: layout dirty; `hidden`: paint dirty.
- Run user-effect applies after render applies.
- Dispose view nodes whose owners were disposed in phase 2. Unlink bindings and
  queue their effects for disposal at the end of the phase.

Writes from applies are staged for the next flush (or rejected in owned scope).

### 2.4 Phase 4 — layout

Work:

- Resolve the style of dirty view nodes (cascade inherited text style, rem,
  scale).
- Update taffy styles and child lists for structural changes.
- For each layout-dirty root, compute layout. A root is the window root or the
  nearest layout boundary (a node with fixed size and `contain: layout`-like
  semantics).
- Copy geometry back to view nodes; mark paint dirty for nodes whose bounds
  changed.
- Text measurement may run inside taffy's measure callback.

If nothing is layout-dirty, skip entirely.

### 2.5 Phase 5 — scene

Work:

- Traverse the view tree in paint order.
- For a dirty node: emit its primitives into the scene, store `PaintRange`.
- For a clean node: replay the previous frame's `PaintRange` into the new scene.
- Apply clipping/content masks and transforms.
- Sort primitives by z/order and batch by kind for the backend.
- Compute damage rects if the backend supports partial present.

If nothing is paint-dirty, skip scene rebuild and re-present the previous scene
(only if the window needs a present for another reason).

### 2.6 Phase 6 — present

Work:

- Submit the scene to the backend.
- Present (vsync or immediate).
- Schedule the next frame only if:
  - animations are active;
  - a timer is due;
  - a continuation was posted during the frame;
  - the host requested a redraw.
- Otherwise clear `dirty` and sleep.

## 3. Scheduling and coalescing

- Multiple writes in one phase produce one flush and one frame.
- A frame request while a frame is running sets `requested_again`; the loop runs
  another frame after the current one if work remains.
- `request_frame()` is idempotent; it calls `spawner.wake_ui()`.
- The host may call `runtime.frame(cause)` directly (vsync, test, or manual).
- `RuntimeMode`:
  - `Live`: vsync/event-driven.
  - `Continuous`: run frames continuously (benchmarks, animations).
  - `Manual`: only explicit `frame()` calls (tests).
  - `Headless`: no platform present; scene is recorded.

## 4. Clock and timers

```rust
pub struct Clock {
    now: Instant,
    frame: FrameId,
}

pub fn set_timeout(cx: &Cx, dur: Duration, f: impl FnOnce() + 'static) -> TimerId;
pub fn set_interval(cx: &Cx, dur: Duration, f: impl FnMut() + 'static) -> TimerId;
```

- Timers are owned by the runtime, not the graph. They post `Message::Timer` at
  the frame boundary; the callback stages writes.
- Tests use a `VirtualClock`; `advance(Duration)` fires due timers in order.
- Animation frames use the same clock: an animation requests a frame each tick
  and stages a value; the value drives a binding.

## 5. Idle behavior

The runtime is idle when all of:

- the continuation queue is empty;
- the graph heap is empty and no staged writes exist;
- no layout/paint work is pending;
- no animation is active;
- no timer is due.

Idle means the host event loop blocks. The runtime does not poll. This is the
"0% idle CPU" property both GPUI and Solid's microtask model aim for, made
explicit by the host loop.

A diagnostic `idle_counters` tracks wake reasons so a regression (e.g. a timer
that fires every frame) is visible in tests.

## 6. Reentrancy and deferred work

| Situation | Rule |
|---|---|
| Write during compute | Dev error; release stages for next flush. |
| Write during apply | Allowed only for `owned_write` signals or through `defer_write`; otherwise dev error. |
| `flush()` inside compute/apply | No-op with dev warning. |
| Mount a view node during apply | Allowed for structural bindings (`Show`, `For`) when the parent is stable; new render-effect bindings initialize synchronously, so layout sees a complete subtree. |
| Dispose a view node during apply | Deferred to the end of phase 3, so no iteration or dispatch observes a freed node. |
| Layout mutation during layout | Not allowed; a measure callback must be pure. |
| Paint mutation during paint | Not allowed; paint reads resolved state. |
| AccessKit update during paint | Not allowed; the a11y tree is built in apply/layout, and paint only reads resolved a11y state. |

Deferred queues:

```rust
struct Deferred {
    dispose: Vec<ViewNodeId>,
    writes: Vec<PendingWrite>,
}
```

## 7. Host integration

The host implements:

```rust
pub trait Host {
    fn window_size(&self) -> Size<Pixels>;
    fn scale_factor(&self) -> f32;
    fn request_redraw(&self);
    fn set_cursor(&self, cursor: Cursor);
    fn set_ime_text(&self, text: &str);
    fn notify_a11y(&self, update: A11yUpdate);
    fn present(&mut self, scene: &Scene);
}
```

Desktop:

- `winit` event loop; `winit::Window` for the surface.
- `wgpu::Surface` for presentation.
- Platform text system (CoreText, DirectWrite, fontconfig).
- AccessKit adapter per platform.

Web (later):

- Canvas + `wgpu` WebGPU; input from DOM events on the canvas.
- Accessibility via ARIA/AT; a DOM mirror for screen readers.

Headless:

- No window; `HeadlessHost` records frames and provides a null surface.
- `render_to_image()` uses a software rasterizer for golden tests.

## 8. Error handling at the runtime level

- A panic in user code unwinds to the runtime's guard, which records the current
  owner/node, marks the graph halted, and presents an error placeholder. The
  process may continue in a degraded state for diagnostics, but no further writes
  are processed until `reset_error_halt()`.
- A backend error (device lost) is reported to the host; the runtime attempts one
  device recreation, then surfaces a fatal error.
- A renderer contract error replaces the offending subtree with an error node and
  reports once; it does not halt the graph.

## 9. API

```rust
impl Runtime {
    pub fn new(options: RuntimeOptions) -> Self;
    pub fn mount(&mut self, root: impl Component) -> ViewNodeId;
    pub fn frame(&mut self, cause: FrameCause) -> FrameStats;
    pub fn request_frame(&mut self);
    pub fn run(&mut self) -> !;                 // host loop, desktop
    pub fn flush(&mut self);                    // explicit, tests/imperative
    pub fn set_mode(&mut self, mode: RuntimeMode);
    pub fn set_spawner(&mut self, spawner: Box<dyn Spawner>);
    pub fn stats(&self) -> &RuntimeStats;
}
```

`FrameStats` reports phase durations, node counts, dirty counts, layout count,
paint primitive count, and allocations. It is cheap enough to keep in release
(no per-node tracing) and is the basis of the benchmark harness.

## 10. Tests

- Phase order: a fake backend records the order of `flush`, `apply`, `layout`,
  `scene`, `present`.
- No work on idle: a 60-frame idle loop produces zero allocs and zero backend
  calls.
- Coalescing: 100 writes in one event handler produce one frame.
- Reentrancy: a write in apply is staged; a `flush` in apply is a no-op; a
  disposal in apply is deferred.
- Frame cause: input/async/animation/resize are reported correctly.
- Timer: virtual clock fires in order, no drift accumulation.
- Backend loss: simulated device loss recovers once and reports.
- Soak: 1M frames with random events; no unbounded queue growth.

## 11. Open questions

1. Should `Runtime::run` own the winit loop, or should the host own the loop and
   call `frame`? Owning it is simpler; the host-owned loop is more flexible for
   embedding. (Leaning: provide both; `run` is a convenience wrapper.)
2. Do we need a separate "layout frame" vs "paint frame" concept? A paint-only
   frame can present without layout; the backend still needs a scene. Keep one
   frame function with phase skips.
3. How do we handle multiple windows? One runtime per window, or one graph shared
   across windows? (Leaning: one graph + one runtime, multiple window hosts; a
   view node belongs to one window.)
4. Is `FrameStats` always on? Keep the counters; the per-node traces are
   feature-gated.
5. How should animations integrate with the reactive graph? An animation is a
   `ReadSignal<f32>`/`WriteSignal<f32>` pair driven by a timer; a binding reads it. No special node kind.
