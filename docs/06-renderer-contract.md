# 06 — Renderer Contract

**Crate:** `renderer-api` (the outline called this crate `universal`)
**Depends on:** nothing (only `std`)
**Owns:** the object-safe contract third-party renderers implement, capability
traits, the null renderer's interface, and the conformance suite.

**Implementation status (2026-11-19).** The contract is implemented in
[`renderer-api`](../crates/renderer-api/src/lib.rs) and the recording backend in
[`renderer-null`](../crates/renderer-null/src/lib.rs). The conformance suite and
headless counter live in
[`renderer-null/tests`](../crates/renderer-null/tests/conformance.rs).

---

## 1. Purpose and non-purpose

The contract exists so a third-party backend (a DOM renderer, a software
rasterizer, a test recorder, an embedded display) can be driven by the same view
layer without reimplementing the reactive runtime.

It is **not**:

- the hot path of the first-party renderer;
- a scene graph API;
- a layout engine API;
- a promise that every backend supports every feature.

The first-party `renderer-native` bypasses this contract and talks to its own
arena, layout, and scene. If it implements the contract, that implementation is
an adapter for tests and third-party tooling, never the path used by the app.

This is the same "universal contract, specialized flagship" split Solid makes
between `solid-js/universal` and `solid-js/web`.

## 2. Design constraints

1. **Object-safe.** The runtime must be able to hold `Box<dyn Renderer>` for
   third-party backends.
2. **Stable node handles.** `NodeId` is a `u64` chosen by the renderer and valid
   until the node is removed or the renderer is dropped.
3. **No layout or paint primitives in the base trait.** A DOM renderer has its
   own layout; a native renderer has ours. The base contract is structure +
   properties + events.
4. **No assumptions about synchronization.** A backend may be immediate (DOM) or
   retained (native).
5. **Errors are values.** Every operation returns `Result`; the runtime can
   replace a subtree on failure.
6. **Batching is optional.** A backend that benefits from begin/end frame can
   override the hooks; a simple backend ignores them.

## 3. Base trait

```rust
pub type NodeId = u64;

pub trait Renderer {
    fn create_element(&mut self, tag: ElementTag<'_>) -> Result<NodeId, RendererError>;
    fn create_text(&mut self, text: &str) -> Result<NodeId, RendererError>;
    fn create_fragment(&mut self) -> Result<NodeId, RendererError>;

    fn insert(
        &mut self,
        parent: NodeId,
        child: NodeId,
        anchor: Option<NodeId>,
    ) -> Result<(), RendererError>;

    fn remove(&mut self, parent: NodeId, child: NodeId) -> Result<(), RendererError>;

    fn set_text(&mut self, node: NodeId, text: &str) -> Result<(), RendererError>;

    fn set_attr(
        &mut self,
        node: NodeId,
        name: AttrName<'_>,
        value: AttrValue<'_>,
    ) -> Result<(), RendererError>;

    fn set_style(&mut self, node: NodeId, style: &ResolvedStyle)
        -> Result<(), RendererError>;

    fn listen(
        &mut self,
        node: NodeId,
        event: EventKind,
        handler: Box<dyn FnMut(&Event)>,
    ) -> Result<ListenerId, RendererError>;

    fn unlisten(&mut self, node: NodeId, listener: ListenerId)
        -> Result<(), RendererError>;

    /// Optional frame hooks. Defaults are no-ops.
    fn begin_frame(&mut self, info: &FrameInfo) -> Result<(), RendererError> { Ok(()) }
    fn end_frame(&mut self) -> Result<(), RendererError> { Ok(()) }
}
```

`ElementTag` is a borrowed string or a static tag; a backend may intern it.
`AttrName`/`AttrValue` are borrowed to avoid allocation; a backend that needs
ownership copies. `ResolvedStyle` is the full resolved style struct; see §4.

### 3.1 Why `insert` takes an anchor

The view layer owns sibling order; the renderer must insert before an existing
sibling for O(1) keyed moves. `anchor = None` means append. This mirrors the DOM
`insertBefore` and is enough for all structural operations.

### 3.2 Removal semantics

`remove(parent, child)` removes the child subtree from the parent. The renderer
may keep the nodes alive if the view layer holds handles for a later re-insert;
the contract does not require immediate destruction. `renderer-native` keeps
removed nodes in a graveyard until the frame ends so focus/scroll state can be
transferred.

### 3.3 Event delivery

The renderer decides how events reach a listener. On the web, it installs DOM
listeners. On native, it may build a hit-test tree and dispatch. The contract
only requires:

- the handler is called while the node is attached;
- `unlisten` (or dropping the listener token) guarantees no further calls;
- event coordinates are in logical pixels relative to the node;
- the handler may stage signal writes but must not re-enter the renderer.

The last rule keeps dispatch single-threaded and avoids reentrant tree mutation.

## 4. Style in the contract

A full style object in the universal trait would force every backend to
understand our layout properties. We accept that cost in v0.1 for a simple,
object-safe call: `ResolvedStyle` is a plain struct of layout and paint fields.
`renderer-api` owns the wire types; the view layer converts its internal `Style`
into them.

```rust
pub struct ResolvedStyle {
    pub display: Display,
    pub position: Position,
    pub size: Size<Length>,
    pub padding: Edges<DefiniteLength>,
    pub gap: Size<DefiniteLength>,
    pub flex: FlexStyle,
    pub background: Option<Color>,
    pub border: Edges<AbsoluteLength>,
    pub border_color: Option<Color>,
    pub radius: Corners<AbsoluteLength>,
    pub opacity: f32,
    pub text: TextStyle,
}
```

- A DOM renderer maps the fields to CSS.
- A native renderer maps them to taffy and paint.
- A backend that does not support a field may ignore it and report the omission
  once in diagnostics.

The wire types are intentionally a small duplicate of the view types so
`renderer-api` stays dependency-free. Conversions from the internal `Style` live
in `view`.

A later diffing adapter can add a narrower `set_style_prop` method without
changing the base contract. Do not add per-field methods now: they multiply the
trait and the conformance matrix.

## 5. Capability traits

A renderer implements the base plus any capabilities it supports. The runtime
queries capabilities once at construction and stores a `CapabilitySet`; it does
not check per frame.

```rust
pub trait TextCapable: Renderer {
    fn measure_text(&mut self, req: &TextMeasureRequest) -> Result<TextMetrics, RendererError>;
    fn shape_text(&mut self, req: &TextShapeRequest) -> Result<ShapedText, RendererError>;
}

pub trait ImageCapable: Renderer {
    fn create_image(&mut self, data: ImageData) -> Result<ImageId, RendererError>;
    fn destroy_image(&mut self, id: ImageId);
}

pub trait ScrollCapable: Renderer {
    fn set_scroll_offset(&mut self, node: NodeId, offset: Point<Px>) -> Result<(), RendererError>;
    fn scroll_offset(&self, node: NodeId) -> Point<Px>;
    fn set_scroll_handler(&mut self, node: NodeId, handler: Box<dyn FnMut(ScrollEvent)>);
}

pub trait VirtualListCapable: Renderer {
    fn set_visible_range(&mut self, node: NodeId, range: Range<usize>);
}

pub trait A11yCapable: Renderer {
    fn set_a11y_role(&mut self, node: NodeId, role: A11yRole);
    fn set_a11y_label(&mut self, node: NodeId, label: &str);
    fn update_a11y(&mut self, update: &A11yUpdate);
}

pub trait AnimationCapable: Renderer {
    fn request_animation_frame(&mut self);
}
```

Why capabilities instead of one giant trait:

- A minimal DOM renderer should not be forced to implement image atlases or
  virtual lists.
- The runtime can degrade gracefully: no text capability means the shared text
  system measures; no scroll capability means the view layer implements scroll
  by clipping/offsetting.
- The first-party renderer implements the capabilities natively; a third-party
  renderer advertises what it can do.

This follows Iced's renderer/capability split, adapted to a retained tree.

## 6. Null renderer

`renderer-null` implements `Renderer` and records operations:

```rust
pub enum Op {
    CreateElement { id: NodeId, tag: String },
    CreateText { id: NodeId, text: String },
    Insert { parent: NodeId, child: NodeId, anchor: Option<NodeId> },
    Remove { parent: NodeId, child: NodeId },
    SetText { id: NodeId, text: String },
    SetAttr { id: NodeId, name: String, value: String },
    SetStyle { id: NodeId, style: ResolvedStyle },
    Listen { id: NodeId, event: EventKind },
    Unlisten { id: NodeId, event: EventKind },
}
```

It also maintains a shadow tree so tests can assert structure:

```rust
let null = NullRenderer::new();
let mut rt = Runtime::new_headless(Box::new(null));
rt.mount(counter_app);
rt.frame(FrameCause::Manual);
assert_eq!(rt.renderer().text_of("count"), "0");
rt.click("inc");
assert_eq!(rt.renderer().text_of("count"), "1");
```

The null renderer is the reference for the conformance suite and the fastest CI
target.

## 7. Conformance suite

Every renderer must pass `renderer-conformance`:

1. **Structure**: create parent/children, insert at head/middle/tail, remove,
   re-insert; assert parent/child links and order.
2. **Text**: create text, set text, empty text, long text; assert value.
3. **Attributes**: set, overwrite, remove (empty value semantics are renderer
   defined; document them).
4. **Style**: set layout and paint props; assert the renderer's resolved style
   matches the request (or that it reports unsupported).
5. **Events**: listen, dispatch, unlisten; no callback after unlisten; handler
   can stage a write.
6. **Errors**: invalid node id, create after shutdown, duplicate remove; every
   error is returned, not panicked.
7. **Frame hooks**: begin/end called once per frame when enabled; no-op
   otherwise.
8. **Lifecycle**: dropping the renderer releases nodes and listeners; no leaked
   listeners.
9. **Capabilities**: a capability the renderer does not advertise must not be
   called by the runtime.

The suite is parameterized over a `RendererFactory`, so a third-party backend
runs the same tests.

## 8. Relationship to the first-party renderer

`renderer-native` has two layers:

- `native::Tree` — the retained `ViewNode` arena, bindings, layout, scene.
- `native::Adapter` — an optional `Renderer` implementation that forwards
  operations to `Tree` for conformance tests.

The app path uses `Tree` directly:

```
view layer ──binding──► native::Tree::set_text(node, text)
                       native::Tree::set_style(node, style)
                       native::Tree::insert(parent, child, anchor)
```

The adapter path is:

```
view layer ──Renderer trait──► native::Adapter ──► native::Tree
```

The adapter adds virtual dispatch and handle translation. It exists to prove the
contract and to let tooling drive the native renderer, not for production.

## 9. Relationship to the view layer

The view layer is generic over a `Backend`:

```rust
pub trait Backend {
    type Node: Copy;
    fn create_element(&mut self, tag: Tag) -> Self::Node;
    fn create_text(&mut self, text: &str) -> Self::Node;
    fn insert(&mut self, parent: Self::Node, child: Self::Node, anchor: Option<Self::Node>);
    fn remove(&mut self, parent: Self::Node, child: Self::Node);
    fn set_text(&mut self, node: Self::Node, text: &str);
    fn set_style(&mut self, node: Self::Node, style: &ResolvedStyle);
    // ...
}
```

- `impl Backend for dyn Renderer` wraps the universal contract.
- `impl Backend for native::Tree` is the direct path.
- `impl Backend for NullRenderer` is the test path.

The view layer's control flow (`Show`, `For`) is written once against `Backend`
and monomorphized for the native path. This is the same pattern as Tachys'
`Renderer` trait, but with a retained tree instead of a DOM.

## 10. Testing the contract itself

- A `MockRenderer` that returns errors on demand; assert the runtime replaces the
  subtree and reports once, without panicking.
- A `PanicRenderer` that panics on `set_text`; assert the runtime catches at the
  contract boundary and halts or reports.
- A `SlowRenderer` that sleeps; assert the runtime does not hold locks across
  calls.
- A `DropCounter` renderer; assert all nodes/listeners are released.

## 11. Open questions

1. **Full style vs property diffs.** Full `ResolvedStyle` is simple; a diffing
   API is faster for DOM. Could the runtime compute a diff and call a narrower
   method? (Leaning: full struct in v0.1, diff adapter later.)
2. **Text capability ownership.** Does the renderer own shaping, or does the
   shared text system? For native, shared; for DOM, the browser. The capability
   reports which.
3. **Event model.** Node-relative coordinates and a generic `Event` are enough
   for v0.1. A richer event enum (pointer, wheel, key, IME) will be needed; keep it
   in `renderer-api` so all backends agree.
4. **Frame hooks.** Do we need `begin_frame`/`end_frame` for batching, or is
   per-op batching enough? (Leaning: keep the hooks optional; the native path
   uses its own frame.)
5. **Stable NodeId.** `u64` is opaque; a backend may use a pointer. Document that
   `0` is never a valid node.
