#![forbid(unsafe_code)]
//! Retained view tree for the native renderer.
//!
//! A component mounts once. Dynamic values become bindings (render effects that
//! write one renderer property), keyed lists reconcile by key and move existing
//! rows, and view slots replace one branch when a key changes. Structure is not
//! rebuilt per frame.

use std::any::{Any, TypeId};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::Hash;
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use layout::{LayoutEngine, LayoutId, LayoutStats, Rect, TextContent};
use reactive_core::{
    render_effect, EffectHandle, NodeId as ViewNodeId, OwnerHandle, ReadSignal, RuntimeHandle,
};
use renderer_api::{
    A11yRole, Color, CursorIcon, Display, Edges, ElementTag, FlexDirection, FrameInfo, Key,
    KeyEvent, Modifiers, NamedKey, NodeId as RendererNodeId, Overflow, PointerEvent, Renderer,
    ResolvedStyle, Size, TextMeasureRequest, TextMetrics, TextStyle, Transform, WheelEvent,
};
use scene::{PaintTree, Primitive, Scene, SceneBuilder, SceneStats};
use text::{MonospaceShaper, ShapedLayout, TextSystem};

/// Paint-only style overrides for hover and focus states.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StateStyle {
    /// Replacement background.
    pub background: Option<Color>,
    /// Replacement border color.
    pub border_color: Option<Color>,
    /// Replacement text color.
    pub text_color: Option<Color>,
}

impl StateStyle {
    /// An empty override set.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            background: None,
            border_color: None,
            text_color: None,
        }
    }

    /// Override the background.
    #[must_use]
    pub const fn background(mut self, color: Color) -> Self {
        self.background = Some(color);
        self
    }

    /// Override the border color.
    #[must_use]
    pub const fn border_color(mut self, color: Color) -> Self {
        self.border_color = Some(color);
        self
    }

    /// Override the text color.
    #[must_use]
    pub const fn text_color(mut self, color: Color) -> Self {
        self.text_color = Some(color);
        self
    }

    fn merge(self, other: Self) -> Self {
        Self {
            background: other.background.or(self.background),
            border_color: other.border_color.or(self.border_color),
            text_color: other.text_color.or(self.text_color),
        }
    }
}

/// A mounted view node.
struct ViewNode {
    renderer_id: RendererNodeId,
    layout_id: LayoutId,
    style: ResolvedStyle,
    text: Option<String>,
    parent: Option<ViewNodeId>,
    children: Vec<ViewNodeId>,
    bindings: Vec<EffectHandle>,
    owner: Option<OwnerHandle>,
    dirty: Cell<bool>,
    scroll_offset: Cell<(f32, f32)>,
    interactive: bool,
    focusable: bool,
    tab_index: Option<i32>,
    disabled: bool,
    a11y_role: A11yRole,
    a11y_label: Option<String>,
    a11y_value: Option<String>,
    cursor: Option<CursorIcon>,
    tooltip: Option<String>,
    hover_style: Option<StateStyle>,
    focus_style: Option<StateStyle>,
    view_listeners: Vec<ViewListener>,
    key_bindings: Vec<KeyBinding>,
    drag_start: Option<Rc<RefCell<DragStartFn>>>,
    drag_move: Option<Rc<RefCell<DragMoveFn>>>,
    drag_end: Option<Rc<RefCell<DragEndFn>>>,
    drop_target: Option<DropTarget>,
}

type DragStartFn = Box<dyn FnMut(&reactive_core::Cx<'_>) -> DragPayload>;
type DragMoveFn = Box<dyn FnMut(&reactive_core::Cx<'_>, &DragEvent)>;
type DragEndFn = Box<dyn FnMut(&reactive_core::Cx<'_>, &DragEnd)>;
type DropFn = Box<dyn FnMut(&reactive_core::Cx<'_>, &DragPayload) -> bool>;

struct DropTarget {
    type_id: TypeId,
    callback: Rc<RefCell<DropFn>>,
}

struct DragSession {
    source: ViewNodeId,
    payload: DragPayload,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ViewEvent {
    Click,
    PointerEnter,
    PointerLeave,
    Focus,
    Blur,
    Wheel,
}

/// Dispatch phase for listeners.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchPhase {
    /// Root-to-target.
    Capture,
    /// Target-to-root.
    Bubble,
}

/// Result of a listener.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchResult {
    /// Continue propagating.
    Continue,
    /// Stop propagation without suppressing default actions.
    Stop,
    /// Stop propagation and suppress default actions.
    Handled,
}

type ViewCallback = Rc<RefCell<Box<dyn FnMut(&reactive_core::Cx<'_>) -> DispatchResult>>>;

struct ViewListener {
    event: ViewEvent,
    phase: DispatchPhase,
    callback: ViewCallback,
}

#[derive(Clone)]
struct KeyBinding {
    stroke: KeyStroke,
    phase: DispatchPhase,
    callback: ViewCallback,
}

/// A shared text-input handler.
pub type TextInputHandle = Rc<RefCell<Box<dyn TextInputHandler>>>;

/// Composition state for IME input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Composition {
    /// The preedit text.
    pub text: String,
    /// Cursor/selection within the preedit text.
    pub cursor: Option<Range<usize>>,
}

/// A text-input handler owned by a focused view node.
pub trait TextInputHandler {
    /// Insert committed text.
    fn insert_text(&mut self, text: &str);
    /// Set the IME composition string.
    fn set_composition(&mut self, text: &str, cursor: Option<Range<usize>>);
    /// Commit the composition into the buffer.
    fn commit_composition(&mut self);
    /// Cancel the composition without changing the buffer.
    fn cancel_composition(&mut self);
    /// Delete backward.
    fn delete_backward(&mut self);
    /// Delete forward.
    fn delete_forward(&mut self);
    /// Current selection.
    fn selection(&self) -> Option<Range<usize>>;
    /// Replace the selection.
    fn set_selection(&mut self, range: Range<usize>);
    /// The text buffer.
    fn text(&self) -> &str;
    /// Caret rectangle in logical coordinates.
    fn caret_rect(&self) -> Rect;
    /// Set the caret rectangle.
    fn set_caret_rect(&mut self, rect: Rect);
    /// Whether a composition is active.
    fn is_composing(&self) -> bool {
        false
    }
    /// The current composition, if any.
    fn composition(&self) -> Option<Composition> {
        None
    }
    /// Move the caret left.
    fn move_left(&mut self, _extend: bool) {}
    /// Move the caret right.
    fn move_right(&mut self, _extend: bool) {}
    /// Move the caret home.
    fn move_home(&mut self, _extend: bool) {}
    /// Move the caret to the end.
    fn move_end(&mut self, _extend: bool) {}
}

/// A simple UTF-8 text buffer with selection and IME composition.
#[derive(Clone, Debug, PartialEq)]
pub struct TextInput {
    text: String,
    anchor: usize,
    caret: usize,
    composition: Option<Composition>,
    caret_rect: Rect,
}

impl TextInput {
    /// Create a text input with the caret at the end.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let end = text.len();
        Self {
            text,
            anchor: end,
            caret: end,
            composition: None,
            caret_rect: Rect::default(),
        }
    }

    /// The composition state.
    #[must_use]
    pub fn composition(&self) -> Option<&Composition> {
        self.composition.as_ref()
    }

    /// The current ordered selection.
    #[must_use]
    pub fn selection_range(&self) -> Range<usize> {
        self.anchor.min(self.caret)..self.anchor.max(self.caret)
    }

    /// Move the caret left, optionally extending the selection.
    pub fn move_left(&mut self, extend: bool) {
        let range = self.selection_range();
        if !extend && range.start != range.end {
            self.anchor = range.start;
            self.caret = range.start;
            return;
        }
        let mut caret = self.caret.min(self.text.len());
        if caret > 0 {
            caret -= 1;
            while caret > 0 && !self.text.is_char_boundary(caret) {
                caret -= 1;
            }
        }
        self.caret = caret;
        if !extend {
            self.anchor = caret;
        }
    }

    /// Move the caret right, optionally extending the selection.
    pub fn move_right(&mut self, extend: bool) {
        let range = self.selection_range();
        if !extend && range.start != range.end {
            self.anchor = range.end;
            self.caret = range.end;
            return;
        }
        let mut caret = self.caret.min(self.text.len());
        if caret < self.text.len() {
            caret += 1;
            while caret < self.text.len() && !self.text.is_char_boundary(caret) {
                caret += 1;
            }
        }
        self.caret = caret;
        if !extend {
            self.anchor = caret;
        }
    }

    /// Move to the start, optionally extending.
    pub fn move_home(&mut self, extend: bool) {
        self.caret = 0;
        if !extend {
            self.anchor = 0;
        }
    }

    /// Move to the end, optionally extending.
    pub fn move_end(&mut self, extend: bool) {
        self.caret = self.text.len();
        if !extend {
            self.anchor = self.caret;
        }
    }

    fn boundary(&self, mut index: usize) -> usize {
        index = index.min(self.text.len());
        while index > 0 && !self.text.is_char_boundary(index) {
            index -= 1;
        }
        index
    }

    fn normalize(&self, range: Range<usize>) -> Range<usize> {
        let start = self.boundary(range.start);
        let end = self.boundary(range.end.max(range.start));
        start..end
    }
}

impl Default for TextInput {
    fn default() -> Self {
        Self::new("")
    }
}

impl TextInputHandler for TextInput {
    fn insert_text(&mut self, text: &str) {
        let range = self.selection_range();
        self.text.replace_range(range.clone(), text);
        let caret = range.start + text.len();
        self.anchor = caret;
        self.caret = caret;
    }

    fn set_composition(&mut self, text: &str, cursor: Option<Range<usize>>) {
        if text.is_empty() {
            self.composition = None;
        } else {
            let max = text.len();
            let cursor = cursor.map(|range| {
                let start = range.start.min(max);
                let end = range.end.min(max).max(start);
                start..end
            });
            self.composition = Some(Composition {
                text: text.to_string(),
                cursor,
            });
        }
    }

    fn commit_composition(&mut self) {
        if let Some(composition) = self.composition.take() {
            self.insert_text(&composition.text);
        }
    }

    fn cancel_composition(&mut self) {
        self.composition = None;
    }

    fn delete_backward(&mut self) {
        let range = self.selection_range();
        if range.start != range.end {
            self.text.replace_range(range.clone(), "");
            self.anchor = range.start;
            self.caret = range.start;
            return;
        }
        if range.start == 0 {
            return;
        }
        let mut start = range.start - 1;
        while start > 0 && !self.text.is_char_boundary(start) {
            start -= 1;
        }
        self.text.replace_range(start..range.start, "");
        self.anchor = start;
        self.caret = start;
    }

    fn delete_forward(&mut self) {
        let range = self.selection_range();
        if range.start != range.end {
            self.text.replace_range(range.clone(), "");
            self.anchor = range.start;
            self.caret = range.start;
            return;
        }
        if range.end >= self.text.len() {
            return;
        }
        let mut end = range.end + 1;
        while end < self.text.len() && !self.text.is_char_boundary(end) {
            end += 1;
        }
        self.text.replace_range(range.end..end, "");
        self.anchor = range.start;
        self.caret = range.start;
    }

    fn selection(&self) -> Option<Range<usize>> {
        Some(self.selection_range())
    }

    fn set_selection(&mut self, range: Range<usize>) {
        let range = self.normalize(range);
        self.anchor = range.start;
        self.caret = range.end;
    }

    fn text(&self) -> &str {
        &self.text
    }

    fn caret_rect(&self) -> Rect {
        self.caret_rect
    }

    fn set_caret_rect(&mut self, rect: Rect) {
        self.caret_rect = rect;
    }

    fn is_composing(&self) -> bool {
        self.composition.is_some()
    }

    fn move_left(&mut self, extend: bool) {
        TextInput::move_left(self, extend);
    }

    fn move_right(&mut self, extend: bool) {
        TextInput::move_right(self, extend);
    }

    fn move_home(&mut self, extend: bool) {
        TextInput::move_home(self, extend);
    }

    fn move_end(&mut self, extend: bool) {
        TextInput::move_end(self, extend);
    }

    fn composition(&self) -> Option<Composition> {
        self.composition.clone()
    }
}

/// A type-erased drag payload.
#[derive(Clone)]
pub struct DragPayload {
    type_id: TypeId,
    value: Rc<dyn Any>,
}

impl DragPayload {
    /// Construct a payload from a concrete value.
    #[must_use]
    pub fn new<T: 'static>(value: T) -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            value: Rc::new(value),
        }
    }

    /// Whether the payload holds `T`.
    #[must_use]
    pub fn is<T: 'static>(&self) -> bool {
        self.type_id == TypeId::of::<T>()
    }

    /// Borrow the payload as `T`.
    #[must_use]
    pub fn get<T: 'static>(&self) -> Option<&T> {
        self.value.downcast_ref::<T>()
    }
}

impl fmt::Debug for DragPayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DragPayload")
            .field("type_id", &self.type_id)
            .finish()
    }
}

/// A drag move event.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DragEvent {
    /// Pointer x.
    pub x: f32,
    /// Pointer y.
    pub y: f32,
    /// Current drop target.
    pub target: Option<ViewNodeId>,
}

/// A drag completion event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DragEnd {
    /// Whether a drop target accepted the payload.
    pub dropped: bool,
    /// The target that accepted or was hovered.
    pub target: Option<ViewNodeId>,
}

/// A key plus modifiers used for bindings.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeyStroke {
    /// The key.
    pub key: Key,
    /// Active modifiers.
    pub modifiers: Modifiers,
}

/// A stable focus identity for a focusable view node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FocusHandle(ViewNodeId);

impl FocusHandle {
    /// The backing view node.
    #[must_use]
    pub const fn node(self) -> ViewNodeId {
        self.0
    }
}

/// One accessibility node.
#[derive(Clone, Debug, PartialEq)]
pub struct A11yNode {
    /// Stable node id.
    pub id: u64,
    /// Role.
    pub role: A11yRole,
    /// Label.
    pub label: Option<String>,
    /// Value.
    pub value: Option<String>,
    /// Disabled state.
    pub disabled: bool,
    /// Whether the node can receive focus.
    pub focusable: bool,
    /// Whether the node currently has focus.
    pub focused: bool,
    /// Snapped logical bounds.
    pub bounds: Rect,
    /// Child a11y ids.
    pub children: Vec<u64>,
}

/// A snapshot of the accessibility tree.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct A11yTree {
    /// Root ids.
    pub roots: Vec<u64>,
    /// Nodes with child ids referencing this list.
    pub nodes: Vec<A11yNode>,
}

/// Scroll viewport, content, and clamped offset in logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScrollState {
    /// Horizontal offset.
    pub offset_x: f32,
    /// Vertical offset.
    pub offset_y: f32,
    /// Viewport width.
    pub viewport_width: f32,
    /// Viewport height.
    pub viewport_height: f32,
    /// Content width.
    pub content_width: f32,
    /// Content height.
    pub content_height: f32,
}

impl ScrollState {
    /// Maximum horizontal offset.
    #[must_use]
    pub fn max_x(self) -> f32 {
        (self.content_width - self.viewport_width).max(0.0)
    }

    /// Maximum vertical offset.
    #[must_use]
    pub fn max_y(self) -> f32 {
        (self.content_height - self.viewport_height).max(0.0)
    }

    /// Whether any content can scroll.
    #[must_use]
    pub fn is_scrollable(self) -> bool {
        self.max_x() > 0.0 || self.max_y() > 0.0
    }

    /// Clamp both offsets into range.
    #[must_use]
    pub fn clamped(mut self) -> Self {
        self.offset_x = self.offset_x.clamp(0.0, self.max_x());
        self.offset_y = self.offset_y.clamp(0.0, self.max_y());
        self
    }

    /// Set a clamped offset.
    pub fn set_offset(&mut self, x: f32, y: f32) {
        self.offset_x = x;
        self.offset_y = y;
        *self = self.clamped();
    }

    /// Add a clamped delta.
    pub fn scroll_by(&mut self, x: f32, y: f32) {
        self.set_offset(self.offset_x + x, self.offset_y + y);
    }
}

impl KeyStroke {
    /// Construct a key stroke.
    #[must_use]
    pub fn new(key: Key, modifiers: Modifiers) -> Self {
        Self { key, modifiers }
    }

    /// A printable character without modifiers.
    #[must_use]
    pub fn character(ch: char) -> Self {
        Self::new(Key::Character(ch), Modifiers::NONE)
    }

    /// A named key without modifiers.
    #[must_use]
    pub fn named(key: NamedKey) -> Self {
        Self::new(Key::Named(key), Modifiers::NONE)
    }
}

struct ViewTreeInner {
    renderer: Rc<RefCell<Box<dyn Renderer>>>,
    nodes: Rc<RefCell<reactive_core::Arena<ViewNode>>>,
    layout: RefCell<LayoutEngine>,
    text: Rc<RefCell<TextSystem>>,
    scene: RefCell<SceneBuilder<ViewNodeId>>,
    roots: RefCell<Vec<ViewNodeId>>,
    paint_dirty: RefCell<HashSet<ViewNodeId>>,
    pending_layout_children: RefCell<HashSet<ViewNodeId>>,
    viewport: Cell<Size<f32>>,
    last_layout: Cell<LayoutStats>,
    focused: RefCell<Option<ViewNodeId>>,
    hovered: RefCell<Option<ViewNodeId>>,
    hover_started: Cell<Option<Instant>>,
    tooltip: RefCell<Option<ViewNodeId>>,
    tooltip_delay: Cell<Duration>,
    pressed: RefCell<Option<ViewNodeId>>,
    press_position: Cell<(f32, f32)>,
    drag: RefCell<Option<DragSession>>,
    drag_target: RefCell<Option<ViewNodeId>>,
    text_inputs: RefCell<HashMap<ViewNodeId, TextInputHandle>>,
    global_keys: RefCell<Vec<KeyBinding>>,
    last_dirty: Cell<usize>,
    runtime: RuntimeHandle,
}

/// A shared retained view tree.
#[derive(Clone)]
pub struct ViewTree {
    inner: Rc<ViewTreeInner>,
}

impl ViewTree {
    /// Create a tree over a renderer and runtime.
    #[must_use]
    pub fn new(renderer: Box<dyn Renderer>, runtime: RuntimeHandle) -> Self {
        Self::new_with_shaper(renderer, runtime, Box::new(MonospaceShaper::default()))
    }

    /// Create a tree with a platform shaping backend.
    #[must_use]
    pub fn new_with_shaper(
        renderer: Box<dyn Renderer>,
        runtime: RuntimeHandle,
        shaper: Box<dyn text::Shaper>,
    ) -> Self {
        let text = Rc::new(RefCell::new(TextSystem::new(shaper)));
        Self {
            inner: Rc::new(ViewTreeInner {
                renderer: Rc::new(RefCell::new(renderer)),
                nodes: Rc::new(RefCell::new(reactive_core::Arena::new())),
                layout: RefCell::new(LayoutEngine::new(
                    1.0,
                    Box::new(CachedTextMeasure {
                        text: Rc::clone(&text),
                    }),
                )),
                text,
                scene: RefCell::new(SceneBuilder::new()),
                roots: RefCell::new(Vec::new()),
                paint_dirty: RefCell::new(HashSet::new()),
                pending_layout_children: RefCell::new(HashSet::new()),
                viewport: Cell::new(Size {
                    width: 800.0,
                    height: 600.0,
                }),
                last_layout: Cell::new(LayoutStats::default()),
                focused: RefCell::new(None),
                hovered: RefCell::new(None),
                hover_started: Cell::new(None),
                tooltip: RefCell::new(None),
                tooltip_delay: Cell::new(Duration::from_millis(500)),
                pressed: RefCell::new(None),
                press_position: Cell::new((0.0, 0.0)),
                drag: RefCell::new(None),
                drag_target: RefCell::new(None),
                text_inputs: RefCell::new(HashMap::new()),
                global_keys: RefCell::new(Vec::new()),
                last_dirty: Cell::new(0),
                runtime,
            }),
        }
    }

    /// The runtime handle used by listeners and slots.
    #[must_use]
    pub fn runtime(&self) -> &RuntimeHandle {
        &self.inner.runtime
    }

    /// Set the logical viewport used by the next frame's layout pass.
    pub fn set_viewport(&self, width: f32, height: f32) {
        self.inner.viewport.set(Size { width, height });
    }

    /// Set the device scale factor used for layout pixel snapping.
    pub fn set_scale(&self, scale: f32) {
        self.inner.layout.borrow_mut().set_scale(scale);
    }

    /// Layout statistics from the last frame.
    #[must_use]
    pub fn last_layout(&self) -> LayoutStats {
        self.inner.last_layout.get()
    }

    /// Text shaping cache counters.
    #[must_use]
    pub fn text_cache_stats(&self) -> text::CacheStats {
        self.inner.text.borrow().stats()
    }

    /// Number of shaped layouts currently cached.
    #[must_use]
    pub fn text_cache_len(&self) -> usize {
        self.inner.text.borrow().cache_len()
    }

    /// Build the scene and return a snapshot of it.
    ///
    /// Call after [`ViewTree::frame`] so layout geometry is current.
    pub fn build_scene(&self) -> Scene {
        let roots = self.inner.roots.borrow().clone();
        let dirty = self
            .inner
            .paint_dirty
            .borrow()
            .iter()
            .copied()
            .collect::<Vec<_>>();
        let mut builder = self.inner.scene.borrow_mut();
        let scene = {
            let tree = ViewPaintTree { tree: self };
            for node in dirty {
                builder.mark_dirty(&tree, node);
            }
            builder.build(&tree, &roots).clone()
        };
        self.inner.paint_dirty.borrow_mut().clear();
        let mut scene = scene;
        self.paint_scrollbars(&mut scene);
        self.paint_tooltip(&mut scene);
        scene
    }

    fn paint_scrollbars(&self, out: &mut Scene) {
        let scrollables = {
            let nodes = self.inner.nodes.borrow();
            nodes
                .iter()
                .filter(|(_, node)| is_scrollable(node.style.overflow))
                .map(|(id, _)| id.id())
                .collect::<Vec<_>>()
        };
        for id in scrollables {
            let Some(state) = self.scroll_state(id) else {
                continue;
            };
            let (Some(renderer_id), Some(rect)) = (self.renderer_id(id), self.layout_rect(id))
            else {
                continue;
            };
            let viewport = scene::Rect::new(rect.x, rect.y, rect.width, rect.height)
                .transformed(self.world_transform(id));
            let max_y = state.max_y();
            let max_x = state.max_x();
            if max_y > 0.5 {
                let thickness = 6.0;
                let margin = 2.0;
                let track = scene::Rect::new(
                    viewport.x + viewport.width - thickness - margin,
                    viewport.y + margin,
                    thickness,
                    (viewport.height - margin * 2.0).max(1.0),
                );
                let ratio = (state.viewport_height / state.content_height).clamp(0.0, 1.0);
                let thumb_height = (track.height * ratio).clamp(16.0, track.height);
                let travel = track.height - thumb_height;
                let position = if max_y > 0.0 {
                    state.offset_y / max_y
                } else {
                    0.0
                };
                out.push(Primitive::Scrollbar {
                    node: renderer_id,
                    track,
                    thumb: scene::Rect::new(
                        track.x,
                        track.y + travel * position,
                        track.width,
                        thumb_height,
                    ),
                    track_color: Color::new(0.0, 0.0, 0.0, 0.18),
                    thumb_color: Color::new(0.45, 0.45, 0.5, 0.7),
                    radius: Edges {
                        top: 3.0,
                        right: 3.0,
                        bottom: 3.0,
                        left: 3.0,
                    },
                });
            }
            if max_x > 0.5 {
                let thickness = 6.0;
                let margin = 2.0;
                let track = scene::Rect::new(
                    viewport.x + margin,
                    viewport.y + viewport.height - thickness - margin,
                    (viewport.width - margin * 2.0).max(1.0),
                    thickness,
                );
                let ratio = (state.viewport_width / state.content_width).clamp(0.0, 1.0);
                let thumb_width = (track.width * ratio).clamp(16.0, track.width);
                let travel = track.width - thumb_width;
                let position = if max_x > 0.0 {
                    state.offset_x / max_x
                } else {
                    0.0
                };
                out.push(Primitive::Scrollbar {
                    node: renderer_id,
                    track,
                    thumb: scene::Rect::new(
                        track.x + travel * position,
                        track.y,
                        thumb_width,
                        track.height,
                    ),
                    track_color: Color::new(0.0, 0.0, 0.0, 0.18),
                    thumb_color: Color::new(0.45, 0.45, 0.5, 0.7),
                    radius: Edges {
                        top: 3.0,
                        right: 3.0,
                        bottom: 3.0,
                        left: 3.0,
                    },
                });
            }
        }
    }

    fn paint_tooltip(&self, out: &mut Scene) {
        let Some(node) = self.tooltip_visible() else {
            return;
        };
        if self.hovered() != Some(node) {
            return;
        }
        let (Some(renderer_id), Some(rect)) = (self.renderer_id(node), self.layout_rect(node))
        else {
            return;
        };
        let Some(text) = self
            .inner
            .nodes
            .borrow()
            .get(node)
            .and_then(|node| node.tooltip.clone())
        else {
            return;
        };
        if text.is_empty() {
            return;
        }
        let style = TextStyle::default();
        let layout = self
            .inner
            .text
            .borrow_mut()
            .layout(&text, style, Some(320.0));
        let anchor = scene::Rect::new(rect.x, rect.y, rect.width, rect.height)
            .transformed(self.world_transform(node));
        let padding = Edges {
            top: 4.0,
            right: 6.0,
            bottom: 4.0,
            left: 6.0,
        };
        let width = layout.width + padding.left + padding.right;
        let height = layout.height.max(style.line_height) + padding.top + padding.bottom;
        let viewport = self.inner.viewport.get();
        let mut x = anchor.x;
        let mut y = anchor.y + anchor.height + 4.0;
        if x + width > viewport.width {
            x = (viewport.width - width - 4.0).max(4.0);
        }
        if y + height > viewport.height {
            y = (anchor.y - height - 4.0).max(4.0);
        }
        out.push(Primitive::Tooltip {
            node: renderer_id,
            rect: scene::Rect::new(x, y, width, height),
            layout,
            color: renderer_api::Color::new(0.96, 0.96, 0.98, 1.0),
            background: renderer_api::Color::new(0.12, 0.12, 0.15, 0.97),
            border: renderer_api::Color::new(0.35, 0.35, 0.42, 1.0),
            padding,
            radius: Edges {
                top: 4.0,
                right: 4.0,
                bottom: 4.0,
                left: 4.0,
            },
            clip: None,
        });
    }

    /// Scene build counters from the last [`ViewTree::build_scene`].
    #[must_use]
    pub fn scene_stats(&self) -> SceneStats {
        self.inner.scene.borrow().stats()
    }

    /// Take the damage rectangles from the last scene build.
    pub fn take_scene_damage(&self) -> Vec<scene::Rect> {
        self.inner.scene.borrow_mut().take_damage()
    }

    /// Snapped geometry for a view node, in logical pixels.
    #[must_use]
    pub fn layout_rect(&self, id: ViewNodeId) -> Option<Rect> {
        let layout_id = self
            .inner
            .nodes
            .borrow()
            .get(id)
            .map(|node| node.layout_id)?;
        self.inner.layout.borrow().geometry(layout_id)
    }

    /// The world transform for a node, composed from its ancestors.
    #[must_use]
    pub fn world_transform(&self, id: ViewNodeId) -> Transform {
        let nodes = self.inner.nodes.borrow();
        let mut chain = Vec::new();
        let mut current = Some(id);
        while let Some(node) = current {
            let Some(data) = nodes.get(node) else {
                break;
            };
            chain.push(node);
            current = data.parent;
        }
        chain.reverse();
        let mut world = Transform::IDENTITY;
        for node in chain {
            let Some(data) = nodes.get(node) else {
                continue;
            };
            let origin = self
                .inner
                .layout
                .borrow()
                .geometry(data.layout_id)
                .unwrap_or_default();
            let mut local = data.style.transform;
            if node != id && is_scrollable(data.style.overflow) {
                if let Some(scroll) = self.scroll_state(node) {
                    local.translate_x -= scroll.offset_x * local.scale_x;
                    local.translate_y -= scroll.offset_y * local.scale_y;
                }
            }
            world = world.compose(
                local,
                renderer_api::Point {
                    x: origin.x,
                    y: origin.y,
                },
            );
        }
        world
    }

    /// The intersection of all ancestor clips, in screen coordinates.
    #[must_use]
    pub fn content_mask(&self, id: ViewNodeId) -> Option<Rect> {
        let nodes = self.inner.nodes.borrow();
        let mut current = nodes.get(id).and_then(|data| data.parent);
        let mut mask: Option<Rect> = None;
        while let Some(node) = current {
            let Some(data) = nodes.get(node) else {
                break;
            };
            if data.style.overflow != Overflow::Visible {
                if let Some(rect) = self.inner.layout.borrow().geometry(data.layout_id) {
                    let clip = transform_rect(self.world_transform(node), rect);
                    mask = Some(mask.map_or(clip, |existing| intersect_rect(existing, clip)));
                }
            }
            current = data.parent;
        }
        mask
    }

    /// The scroll state for a scrollable node.
    #[must_use]
    pub fn scroll_state(&self, id: ViewNodeId) -> Option<ScrollState> {
        let (overflow, rect, offset) = {
            let nodes = self.inner.nodes.borrow();
            let node = nodes.get(id)?;
            (
                node.style.overflow,
                self.layout_rect(id)?,
                node.scroll_offset.get(),
            )
        };
        if !is_scrollable(overflow) {
            return None;
        }
        let (content_width, content_height) = self.scroll_content_size(id, rect);
        Some(
            ScrollState {
                offset_x: offset.0,
                offset_y: offset.1,
                viewport_width: rect.width,
                viewport_height: rect.height,
                content_width,
                content_height,
            }
            .clamped(),
        )
    }

    /// Set a clamped scroll offset.
    pub fn set_scroll_offset(&self, id: ViewNodeId, x: f32, y: f32) -> Option<ScrollState> {
        let mut state = self.scroll_state(id)?;
        state.set_offset(x, y);
        if let Some(node) = self.inner.nodes.borrow().get(id) {
            node.scroll_offset.set((state.offset_x, state.offset_y));
        }
        self.mark_paint_dirty(id);
        Some(state)
    }

    /// Scroll a node by a clamped delta.
    pub fn scroll_by(&self, id: ViewNodeId, x: f32, y: f32) -> Option<ScrollState> {
        let mut state = self.scroll_state(id)?;
        state.scroll_by(x, y);
        if let Some(node) = self.inner.nodes.borrow().get(id) {
            node.scroll_offset.set((state.offset_x, state.offset_y));
        }
        self.mark_paint_dirty(id);
        Some(state)
    }

    /// The deepest scrollable node under a point.
    #[must_use]
    pub fn deepest_scrollable(&self, x: f32, y: f32) -> Option<ViewNodeId> {
        let roots = self.inner.roots.borrow().clone();
        for root in roots.iter().rev() {
            if let Some(hit) = self.scrollable_node(*root, x, y) {
                return Some(hit);
            }
        }
        None
    }

    fn scrollable_node(&self, node: ViewNodeId, x: f32, y: f32) -> Option<ViewNodeId> {
        let (children, scrollable) = {
            let nodes = self.inner.nodes.borrow();
            let data = nodes.get(node)?;
            (data.children.clone(), is_scrollable(data.style.overflow))
        };
        for child in children.iter().rev() {
            if let Some(hit) = self.scrollable_node(*child, x, y) {
                return Some(hit);
            }
        }
        if scrollable {
            if let Some(rect) = self.layout_rect(node) {
                let world = self.world_transform(node);
                let local = world.inverse_point(renderer_api::Point { x, y })?;
                let inside = contains_point(rect, local.x, local.y);
                let unmasked = self
                    .content_mask(node)
                    .is_none_or(|mask| contains_point(mask, x, y));
                if inside && unmasked {
                    return Some(node);
                }
            }
        }
        None
    }

    fn scroll_content_size(&self, id: ViewNodeId, viewport: Rect) -> (f32, f32) {
        let mut width = viewport.width;
        let mut height = viewport.height;
        for child in self.children(id) {
            if let Some(rect) = self.layout_rect(child) {
                width = width.max(rect.right() - viewport.x);
                height = height.max(rect.bottom() - viewport.y);
            }
        }
        (width, height)
    }

    /// Route a wheel event to the deepest scrollable ancestor and then either a
    /// listener or the default scroll action.
    pub fn wheel(&self, event: &WheelEvent) -> bool {
        let mut target = self.deepest_scrollable(event.x, event.y);
        let mut delta_x = event.delta_x;
        let mut delta_y = event.delta_y;
        let mut scrolled = false;
        let mut hops = 0;
        while let Some(node) = target {
            if self.dispatch_view_event(node, ViewEvent::Wheel) {
                return true;
            }
            if let (Some(before), Some(after)) = (
                self.scroll_state(node),
                self.scroll_by(node, delta_x, delta_y),
            ) {
                delta_x -= after.offset_x - before.offset_x;
                delta_y -= after.offset_y - before.offset_y;
                scrolled = true;
            }
            if delta_x.abs() < 0.01 && delta_y.abs() < 0.01 {
                break;
            }
            hops += 1;
            if hops > 64 {
                break;
            }
            target = self.scrollable_ancestor(node);
        }
        scrolled
    }

    fn scrollable_ancestor(&self, id: ViewNodeId) -> Option<ViewNodeId> {
        let mut current = self
            .inner
            .nodes
            .borrow()
            .get(id)
            .and_then(|node| node.parent);
        while let Some(node) = current {
            let nodes = self.inner.nodes.borrow();
            let data = nodes.get(node)?;
            if is_scrollable(data.style.overflow) {
                return Some(node);
            }
            current = data.parent;
        }
        None
    }

    /// Apply a resolved style to a mounted node.
    pub fn set_style(&self, id: ViewNodeId, style: ResolvedStyle) {
        let (renderer_id, layout_id) = {
            let nodes = self.inner.nodes.borrow();
            let Some(node) = nodes.get(id) else {
                return;
            };
            (node.renderer_id, node.layout_id)
        };
        self.inner
            .renderer
            .borrow_mut()
            .set_style(renderer_id, &style)
            .expect("set style");
        self.inner.layout.borrow_mut().set_style(layout_id, style);
        if let Some(node) = self.inner.nodes.borrow_mut().get_mut(id) {
            node.style = style;
        }
        self.mark_dirty(id);
        self.mark_paint_dirty(id);
    }

    /// Replace the text of a text node.
    pub fn set_view_text(&self, id: ViewNodeId, text: String) {
        let (layout_id, style) = {
            let nodes = self.inner.nodes.borrow();
            let Some(node) = nodes.get(id) else {
                return;
            };
            (node.layout_id, node.style.text)
        };
        self.inner.layout.borrow_mut().set_text(
            layout_id,
            Some(TextContent {
                text: text.clone(),
                style,
            }),
        );
        if let Some(node) = self.inner.nodes.borrow_mut().get_mut(id) {
            node.text = Some(text);
        }
        self.mark_paint_dirty(id);
    }

    /// Set the retained focus identity. Focus survives keyed reorders because
    /// the node identity is retained.
    pub fn set_focus(&self, focus: Option<ViewNodeId>) {
        let previous = *self.inner.focused.borrow();
        if previous == focus {
            return;
        }
        *self.inner.focused.borrow_mut() = focus;
        if let Some(previous) = previous {
            self.mark_paint_dirty(previous);
        }
        if let Some(focus) = focus {
            self.mark_paint_dirty(focus);
        }
        if let Some(previous) = previous {
            self.dispatch_view_event(previous, ViewEvent::Blur);
        }
        if let Some(focus) = focus {
            self.dispatch_view_event(focus, ViewEvent::Focus);
        }
        let renderer_id = focus.and_then(|id| self.renderer_id(id));
        let mut renderer = self.inner.renderer.borrow_mut();
        if let Some(capability) = renderer.focus_capable() {
            let _ = capability.set_focus(renderer_id);
        }
    }

    /// The retained focus identity.
    #[must_use]
    pub fn focused(&self) -> Option<ViewNodeId> {
        *self.inner.focused.borrow()
    }

    /// A focus handle for a focusable node.
    #[must_use]
    pub fn focus_handle(&self, id: ViewNodeId) -> Option<FocusHandle> {
        self.inner
            .nodes
            .borrow()
            .get(id)
            .filter(|node| node.focusable && !node.disabled)
            .map(|_| FocusHandle(id))
    }

    /// Focus a handle.
    pub fn focus(&self, handle: FocusHandle) {
        self.set_focus(Some(handle.node()));
    }

    /// Whether a node is focused.
    #[must_use]
    pub fn is_focused(&self, id: ViewNodeId) -> bool {
        self.focused() == Some(id)
    }

    /// Register a text-input handler on a view node.
    pub fn register_text_input(&self, node: ViewNodeId, handler: TextInputHandle) {
        self.inner.text_inputs.borrow_mut().insert(node, handler);
        self.set_focusable(node, true);
        self.set_interactive(node);
        self.set_a11y(node, Some(A11yRole::TextInput), None, None);
    }

    /// The text-input handler on a node.
    #[must_use]
    pub fn text_input(&self, node: ViewNodeId) -> Option<TextInputHandle> {
        self.inner.text_inputs.borrow().get(&node).cloned()
    }

    /// The text-input handler for the focused node.
    #[must_use]
    pub fn focused_text_input(&self) -> Option<TextInputHandle> {
        let node = self.focused()?;
        self.text_input(node)
    }

    /// The focused input's caret rectangle for IME cursor placement.
    #[must_use]
    pub fn ime_cursor_area(&self) -> Option<Rect> {
        let input = self.focused_text_input()?;
        let rect = input.borrow().caret_rect();
        (rect.width > 0.0 && rect.height > 0.0).then_some(rect)
    }

    /// Set the focused input's IME composition.
    pub fn ime_preedit(&self, text: &str, cursor: Option<Range<usize>>) -> bool {
        let Some(input) = self.focused_text_input() else {
            return false;
        };
        input.borrow_mut().set_composition(text, cursor);
        self.mark_paint_dirty(self.focused().unwrap_or(ViewNodeId::INVALID));
        true
    }

    /// Commit IME text into the focused input.
    pub fn ime_commit(&self, text: &str) -> bool {
        let Some(input) = self.focused_text_input() else {
            return false;
        };
        {
            let mut input = input.borrow_mut();
            input.cancel_composition();
            input.insert_text(text);
        }
        self.mark_paint_dirty(self.focused().unwrap_or(ViewNodeId::INVALID));
        true
    }

    /// Cancel the focused input's IME composition.
    pub fn ime_cancel(&self) -> bool {
        let Some(input) = self.focused_text_input() else {
            return false;
        };
        input.borrow_mut().cancel_composition();
        self.mark_paint_dirty(self.focused().unwrap_or(ViewNodeId::INVALID));
        true
    }

    /// Insert text into the focused input.
    pub fn insert_text(&self, text: &str) -> bool {
        let Some(input) = self.focused_text_input() else {
            return false;
        };
        input.borrow_mut().insert_text(text);
        self.mark_paint_dirty(self.focused().unwrap_or(ViewNodeId::INVALID));
        true
    }

    /// Delete backward in the focused input.
    pub fn delete_backward(&self) -> bool {
        let Some(input) = self.focused_text_input() else {
            return false;
        };
        input.borrow_mut().delete_backward();
        self.mark_paint_dirty(self.focused().unwrap_or(ViewNodeId::INVALID));
        true
    }

    /// Delete forward in the focused input.
    pub fn delete_forward(&self) -> bool {
        let Some(input) = self.focused_text_input() else {
            return false;
        };
        input.borrow_mut().delete_forward();
        self.mark_paint_dirty(self.focused().unwrap_or(ViewNodeId::INVALID));
        true
    }

    fn cancel_text_composition(&self) -> bool {
        let Some(input) = self.focused_text_input() else {
            return false;
        };
        let mut input = input.borrow_mut();
        if input.is_composing() {
            input.cancel_composition();
            true
        } else {
            false
        }
    }

    fn handle_text_input_key(&self, event: &KeyEvent) -> bool {
        let Some(input) = self.focused_text_input() else {
            return false;
        };
        let mut input = input.borrow_mut();
        match &event.key {
            Key::Named(NamedKey::Backspace) => {
                input.delete_backward();
                true
            }
            Key::Named(NamedKey::Delete) => {
                input.delete_forward();
                true
            }
            Key::Named(NamedKey::ArrowLeft) => {
                input.move_left(event.modifiers.shift);
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                input.move_right(event.modifiers.shift);
                true
            }
            Key::Named(NamedKey::Home) => {
                input.move_home(event.modifiers.shift);
                true
            }
            Key::Named(NamedKey::End) => {
                input.move_end(event.modifiers.shift);
                true
            }
            Key::Named(NamedKey::Enter) => {
                input.insert_text(
                    "
",
                );
                true
            }
            Key::Character(ch) if !event.modifiers.control && !event.modifiers.meta => {
                if let Some(text) = &event.text {
                    input.insert_text(text);
                } else {
                    input.insert_text(&ch.to_string());
                }
                true
            }
            Key::Named(NamedKey::Space) if !event.modifiers.control && !event.modifiers.meta => {
                input.insert_text(event.text.as_deref().unwrap_or(" "));
                true
            }
            _ => false,
        }
    }

    /// The currently hovered interactive node.
    #[must_use]
    pub fn hovered(&self) -> Option<ViewNodeId> {
        *self.inner.hovered.borrow()
    }

    /// The node pressed by the current pointer gesture.
    #[must_use]
    pub fn pressed(&self) -> Option<ViewNodeId> {
        *self.inner.pressed.borrow()
    }

    /// Test a logical point against the retained interactive nodes.
    #[must_use]
    pub fn hit_test(&self, x: f32, y: f32) -> Option<ViewNodeId> {
        let roots = self.inner.roots.borrow().clone();
        for root in roots.iter().rev() {
            if let Some(hit) = self.hit_test_node(*root, x, y) {
                return Some(hit);
            }
        }
        None
    }

    fn hit_test_node(&self, node: ViewNodeId, x: f32, y: f32) -> Option<ViewNodeId> {
        let (children, interactive, disabled) = {
            let nodes = self.inner.nodes.borrow();
            let node = nodes.get(node)?;
            (node.children.clone(), node.interactive, node.disabled)
        };
        for child in children.iter().rev() {
            if let Some(hit) = self.hit_test_node(*child, x, y) {
                return Some(hit);
            }
        }
        if interactive && !disabled {
            if let Some(rect) = self.layout_rect(node) {
                let world = self.world_transform(node);
                let local = world.inverse_point(renderer_api::Point { x, y })?;
                let inside = local.x >= rect.x
                    && local.x <= rect.right()
                    && local.y >= rect.y
                    && local.y <= rect.bottom();
                let unmasked = self
                    .content_mask(node)
                    .is_none_or(|mask| contains_point(mask, x, y));
                if inside && unmasked {
                    return Some(node);
                }
            }
        }
        None
    }

    /// Update hover state and return the topmost hit.
    pub fn pointer_move(&self, event: &PointerEvent) -> Option<ViewNodeId> {
        self.pointer_move_at(event, Instant::now())
    }

    /// Update hover state with an explicit clock for tooltip timing.
    pub fn pointer_move_at(&self, event: &PointerEvent, now: Instant) -> Option<ViewNodeId> {
        let hit = self.hit_test(event.x, event.y);
        let previous = *self.inner.hovered.borrow();
        if previous != hit {
            if let Some(previous) = previous {
                self.dispatch_view_event(previous, ViewEvent::PointerLeave);
            }
            if let Some(hit) = hit {
                self.dispatch_view_event(hit, ViewEvent::PointerEnter);
            }
            *self.inner.hovered.borrow_mut() = hit;
            *self.inner.tooltip.borrow_mut() = None;
            self.inner.hover_started.set(hit.map(|_| now));
            if let Some(previous) = previous {
                self.mark_paint_dirty(previous);
            }
            if let Some(hit) = hit {
                self.mark_paint_dirty(hit);
            }
        } else if hit.is_some() && self.inner.hover_started.get().is_none() {
            self.inner.hover_started.set(Some(now));
        }
        self.update_drag(event);
        hit
    }

    /// Whether `id` is the hovered node or an ancestor of it.
    #[must_use]
    pub fn is_hovered_within(&self, id: ViewNodeId) -> bool {
        self.is_within(self.hovered(), id)
    }

    /// Whether `id` is the focused node or an ancestor of it.
    #[must_use]
    pub fn is_focused_within(&self, id: ViewNodeId) -> bool {
        self.is_within(self.focused(), id)
    }

    fn is_within(&self, target: Option<ViewNodeId>, ancestor: ViewNodeId) -> bool {
        let mut current = target;
        while let Some(node) = current {
            if node == ancestor {
                return true;
            }
            current = self
                .inner
                .nodes
                .borrow()
                .get(node)
                .and_then(|node| node.parent);
        }
        false
    }

    fn state_style(&self, id: ViewNodeId) -> StateStyle {
        let (hover_style, focus_style) = {
            let nodes = self.inner.nodes.borrow();
            let Some(node) = nodes.get(id) else {
                return StateStyle::default();
            };
            (node.hover_style, node.focus_style)
        };
        let mut style = StateStyle::default();
        if self.is_hovered_within(id) {
            if let Some(hover) = hover_style {
                style = style.merge(hover);
            }
        }
        if self.is_focused_within(id) {
            if let Some(focus) = focus_style {
                style = style.merge(focus);
            }
        }
        style
    }

    /// The cursor requested by the hovered node or its nearest ancestor.
    #[must_use]
    pub fn cursor(&self) -> Option<CursorIcon> {
        let mut current = self.hovered();
        while let Some(id) = current {
            let nodes = self.inner.nodes.borrow();
            let node = nodes.get(id)?;
            if let Some(cursor) = node.cursor {
                return Some(cursor);
            }
            current = node.parent;
        }
        None
    }

    /// Set the tooltip delay. Defaults to 500ms.
    pub fn set_tooltip_delay(&self, delay: Duration) {
        self.inner.tooltip_delay.set(delay);
    }

    /// The node whose tooltip is currently visible.
    #[must_use]
    pub fn tooltip_visible(&self) -> Option<ViewNodeId> {
        *self.inner.tooltip.borrow()
    }

    /// Reveal the hovered node's tooltip once its delay has elapsed.
    pub fn update_tooltip_at(&self, now: Instant) -> bool {
        let Some(hovered) = self.hovered() else {
            return false;
        };
        if self.tooltip_visible() == Some(hovered) {
            return false;
        }
        let has_tooltip = self.inner.nodes.borrow().get(hovered).is_some_and(|node| {
            node.tooltip
                .as_ref()
                .is_some_and(|tooltip| !tooltip.is_empty())
        });
        if !has_tooltip {
            return false;
        }
        let Some(started) = self.inner.hover_started.get() else {
            return false;
        };
        if now.saturating_duration_since(started) < self.inner.tooltip_delay.get() {
            return false;
        }
        *self.inner.tooltip.borrow_mut() = Some(hovered);
        self.mark_paint_dirty(hovered);
        true
    }

    /// Pointer press. Focuses the hit node when it is focusable.
    pub fn pointer_down(&self, event: &PointerEvent) -> Option<ViewNodeId> {
        let hit = self.hit_test(event.x, event.y);
        *self.inner.pressed.borrow_mut() = hit;
        self.inner.press_position.set((event.x, event.y));
        if let Some(hit) = hit {
            let focusable = self
                .inner
                .nodes
                .borrow()
                .get(hit)
                .is_some_and(|node| node.focusable && !node.disabled);
            if focusable {
                self.set_focus(Some(hit));
            }
        }
        hit
    }

    /// Pointer release. Completes a drag, or dispatches a click when press and
    /// release share a node.
    pub fn pointer_up(&self, event: &PointerEvent) -> bool {
        if let Some(session) = self.inner.drag.borrow_mut().take() {
            let target = self.deepest_drop_target(event.x, event.y);
            let accepted = target.is_some_and(|target| {
                let drop = {
                    let nodes = self.inner.nodes.borrow();
                    nodes.get(target).and_then(|node| {
                        node.drop_target
                            .as_ref()
                            .map(|drop| (drop.type_id, Rc::clone(&drop.callback)))
                    })
                };
                let Some((type_id, callback)) = drop else {
                    return false;
                };
                if type_id != session.payload.type_id {
                    return false;
                }
                let cx = self.inner.runtime.cx();
                let accepted = (callback.borrow_mut())(&cx, &session.payload);
                accepted
            });
            let cx = self.inner.runtime.cx();
            if let Some(callback) = self
                .inner
                .nodes
                .borrow()
                .get(session.source)
                .and_then(|node| node.drag_end.as_ref().map(Rc::clone))
            {
                (callback.borrow_mut())(
                    &cx,
                    &DragEnd {
                        dropped: accepted,
                        target,
                    },
                );
            }
            *self.inner.drag_target.borrow_mut() = None;
            *self.inner.pressed.borrow_mut() = None;
            return true;
        }

        let hit = self.hit_test(event.x, event.y);
        let pressed = *self.inner.pressed.borrow();
        *self.inner.pressed.borrow_mut() = None;
        if pressed.is_some() && pressed == hit {
            if let Some(node) = hit {
                return self.click_node(node);
            }
        }
        false
    }

    /// The current drag source, if a drag is active.
    #[must_use]
    pub fn active_drag(&self) -> Option<ViewNodeId> {
        self.inner.drag.borrow().as_ref().map(|drag| drag.source)
    }

    /// The current drop target, if any.
    #[must_use]
    pub fn drag_target(&self) -> Option<ViewNodeId> {
        *self.inner.drag_target.borrow()
    }

    fn update_drag(&self, event: &PointerEvent) {
        if self.inner.drag.borrow().is_none() {
            let Some(source) = *self.inner.pressed.borrow() else {
                return;
            };
            let (start_x, start_y) = self.inner.press_position.get();
            let dx = event.x - start_x;
            let dy = event.y - start_y;
            if dx * dx + dy * dy < 16.0 {
                return;
            }
            let callback = {
                let nodes = self.inner.nodes.borrow();
                nodes
                    .get(source)
                    .and_then(|node| node.drag_start.as_ref().map(Rc::clone))
            };
            let Some(callback) = callback else {
                return;
            };
            let cx = self.inner.runtime.cx();
            let payload = (callback.borrow_mut())(&cx);
            *self.inner.drag.borrow_mut() = Some(DragSession { source, payload });
        }

        let target = self.deepest_drop_target(event.x, event.y);
        *self.inner.drag_target.borrow_mut() = target;
        let source = self.inner.drag.borrow().as_ref().map(|drag| drag.source);
        if let Some(source) = source {
            let callback = {
                let nodes = self.inner.nodes.borrow();
                nodes
                    .get(source)
                    .and_then(|node| node.drag_move.as_ref().map(Rc::clone))
            };
            if let Some(callback) = callback {
                let cx = self.inner.runtime.cx();
                (callback.borrow_mut())(
                    &cx,
                    &DragEvent {
                        x: event.x,
                        y: event.y,
                        target,
                    },
                );
            }
        }
    }

    fn cancel_drag(&self) -> bool {
        let Some(session) = self.inner.drag.borrow_mut().take() else {
            return false;
        };
        let callback = {
            let nodes = self.inner.nodes.borrow();
            nodes
                .get(session.source)
                .and_then(|node| node.drag_end.as_ref().map(Rc::clone))
        };
        if let Some(callback) = callback {
            let cx = self.inner.runtime.cx();
            (callback.borrow_mut())(
                &cx,
                &DragEnd {
                    dropped: false,
                    target: None,
                },
            );
        }
        *self.inner.drag_target.borrow_mut() = None;
        *self.inner.pressed.borrow_mut() = None;
        true
    }

    fn deepest_drop_target(&self, x: f32, y: f32) -> Option<ViewNodeId> {
        let roots = self.inner.roots.borrow().clone();
        for root in roots.iter().rev() {
            if let Some(hit) = self.drop_target_node(*root, x, y) {
                return Some(hit);
            }
        }
        None
    }

    fn drop_target_node(&self, node: ViewNodeId, x: f32, y: f32) -> Option<ViewNodeId> {
        let (children, has_target) = {
            let nodes = self.inner.nodes.borrow();
            let data = nodes.get(node)?;
            (data.children.clone(), data.drop_target.is_some())
        };
        for child in children.iter().rev() {
            if let Some(hit) = self.drop_target_node(*child, x, y) {
                return Some(hit);
            }
        }
        if has_target {
            if let Some(rect) = self.layout_rect(node) {
                let world = self.world_transform(node);
                let local = world.inverse_point(renderer_api::Point { x, y })?;
                let unmasked = self
                    .content_mask(node)
                    .is_none_or(|mask| contains_point(mask, x, y));
                if contains_point(rect, local.x, local.y) && unmasked {
                    return Some(node);
                }
            }
        }
        None
    }

    /// Dispatch a click to a node and its ancestors.
    pub fn click_node(&self, node: ViewNodeId) -> bool {
        let disabled = self
            .inner
            .nodes
            .borrow()
            .get(node)
            .is_some_and(|node| node.disabled);
        if disabled {
            return false;
        }
        let focusable = self
            .inner
            .nodes
            .borrow()
            .get(node)
            .is_some_and(|node| node.focusable);
        if focusable {
            self.set_focus(Some(node));
        }
        self.dispatch_view_event(node, ViewEvent::Click);
        true
    }

    /// Move focus to the next tab stop.
    pub fn focus_next(&self) -> Option<ViewNodeId> {
        self.focus_step(true)
    }

    /// Move focus to the previous tab stop.
    pub fn focus_prev(&self) -> Option<ViewNodeId> {
        self.focus_step(false)
    }

    fn focus_step(&self, forward: bool) -> Option<ViewNodeId> {
        let order = self.tab_order();
        if order.is_empty() {
            return None;
        }
        let current = self.focused();
        let index = current
            .and_then(|current| order.iter().position(|node| *node == current))
            .unwrap_or(if forward { order.len() - 1 } else { 0 });
        let next = if forward {
            (index + 1) % order.len()
        } else if index == 0 {
            order.len() - 1
        } else {
            index - 1
        };
        let node = order[next];
        self.set_focus(Some(node));
        Some(node)
    }

    fn tab_order(&self) -> Vec<ViewNodeId> {
        let roots = self.inner.roots.borrow().clone();
        let mut natural = Vec::new();
        for root in roots {
            self.collect_focusable(root, &mut natural);
        }
        let mut explicit = natural
            .iter()
            .enumerate()
            .filter_map(|(order, node)| {
                self.inner
                    .nodes
                    .borrow()
                    .get(*node)
                    .and_then(|data| data.tab_index)
                    .map(|index| (index, order, *node))
            })
            .collect::<Vec<_>>();
        explicit.sort_by_key(|(index, order, _)| (*index, *order));
        let mut ordered = explicit
            .into_iter()
            .map(|(_, _, node)| node)
            .collect::<Vec<_>>();
        for node in natural {
            let explicit = self
                .inner
                .nodes
                .borrow()
                .get(node)
                .is_some_and(|data| data.tab_index.is_some());
            if !explicit {
                ordered.push(node);
            }
        }
        ordered
    }

    fn collect_focusable(&self, node: ViewNodeId, out: &mut Vec<ViewNodeId>) {
        let (children, focusable, disabled) = {
            let nodes = self.inner.nodes.borrow();
            let Some(data) = nodes.get(node) else {
                return;
            };
            (data.children.clone(), data.focusable, data.disabled)
        };
        if focusable && !disabled {
            out.push(node);
        }
        for child in children {
            self.collect_focusable(child, out);
        }
    }

    /// Dispatch a key event to the focused path, then global bindings, then
    /// default Tab traversal.
    pub fn key_down(&self, event: &KeyEvent) -> bool {
        if event.key == Key::Named(NamedKey::Escape) {
            if self.cancel_text_composition() {
                return true;
            }
            if self.cancel_drag() {
                return true;
            }
        }
        if self.handle_text_input_key(event) {
            return true;
        }
        let cx = self.inner.runtime.cx();
        let mut path = Vec::new();
        let mut current = self.focused();
        while let Some(node) = current {
            path.push(node);
            current = self
                .inner
                .nodes
                .borrow()
                .get(node)
                .and_then(|data| data.parent);
        }

        let mut stopped = false;
        let mut invoked = false;
        for phase in [DispatchPhase::Capture, DispatchPhase::Bubble] {
            let iter: Box<dyn Iterator<Item = &ViewNodeId>> = if phase == DispatchPhase::Capture {
                Box::new(path.iter().rev())
            } else {
                Box::new(path.iter())
            };
            for node in iter {
                let bindings = {
                    let nodes = self.inner.nodes.borrow();
                    nodes.get(*node).map(|data| {
                        data.key_bindings
                            .iter()
                            .filter(|binding| {
                                binding.phase == phase
                                    && binding.stroke.key == event.key
                                    && binding.stroke.modifiers == event.modifiers
                            })
                            .map(|binding| Rc::clone(&binding.callback))
                            .collect::<Vec<_>>()
                    })
                };
                if let Some(bindings) = bindings {
                    for callback in bindings {
                        invoked = true;
                        match (callback.borrow_mut())(&cx) {
                            DispatchResult::Continue => {}
                            DispatchResult::Stop => {
                                stopped = true;
                                break;
                            }
                            DispatchResult::Handled => return true,
                        }
                    }
                }
                if stopped {
                    break;
                }
            }
            if stopped {
                break;
            }
        }

        let globals = self.inner.global_keys.borrow().clone();
        for binding in globals {
            if binding.stroke.key == event.key && binding.stroke.modifiers == event.modifiers {
                invoked = true;
                match (binding.callback.borrow_mut())(&cx) {
                    DispatchResult::Continue => {}
                    DispatchResult::Stop => break,
                    DispatchResult::Handled => return true,
                }
            }
        }

        if matches!(event.key, Key::Named(NamedKey::Enter | NamedKey::Space))
            && !event.modifiers.control
            && !event.modifiers.meta
            && !event.modifiers.alt
        {
            if let Some(focused) = self.focused() {
                let activatable = self
                    .inner
                    .nodes
                    .borrow()
                    .get(focused)
                    .is_some_and(|node| node.interactive && !node.disabled);
                if activatable {
                    return self.click_node(focused);
                }
            }
        }
        if event.key == Key::Named(NamedKey::Tab) {
            if event.modifiers.shift {
                self.focus_prev();
            } else {
                self.focus_next();
            }
            return true;
        }
        invoked
    }

    /// Register a window-level key binding.
    pub fn bind_global_key(
        &self,
        stroke: KeyStroke,
        callback: impl Fn(&reactive_core::Cx<'_>) + 'static,
    ) {
        self.inner.global_keys.borrow_mut().push(KeyBinding {
            stroke,
            phase: DispatchPhase::Bubble,
            callback: Rc::new(RefCell::new(Box::new(move |cx| {
                callback(cx);
                DispatchResult::Continue
            }))),
        });
    }

    /// Build an accessibility snapshot.
    #[must_use]
    pub fn a11y_tree(&self) -> A11yTree {
        let roots = self.inner.roots.borrow().clone();
        let mut tree = A11yTree::default();
        for root in roots {
            let id = self.a11y_visit(root, &mut tree.nodes);
            tree.roots.push(id);
        }
        tree
    }

    /// Dispatch an AccessKit action into the retained input methods.
    pub fn dispatch_accesskit_action(&self, request: &accesskit::ActionRequest) -> bool {
        if request.target_tree != accesskit::TreeId::ROOT {
            return false;
        }
        let Some(node) = self
            .inner
            .nodes
            .borrow()
            .iter()
            .find(|(_, node)| node.renderer_id == request.target_node.0)
            .map(|(handle, _)| handle.id())
        else {
            return false;
        };
        match request.action {
            accesskit::Action::Focus => {
                let focusable = self
                    .inner
                    .nodes
                    .borrow()
                    .get(node)
                    .is_some_and(|node| node.focusable && !node.disabled);
                if focusable {
                    self.set_focus(Some(node));
                }
                focusable
            }
            accesskit::Action::Blur => {
                if self.focused() == Some(node) {
                    self.set_focus(None);
                    true
                } else {
                    false
                }
            }
            accesskit::Action::Click => self.click_node(node),
            accesskit::Action::SetValue => {
                let Some(input) = self.text_input(node) else {
                    return false;
                };
                let Some(accesskit::ActionData::Value(value)) = &request.data else {
                    return false;
                };
                let mut input = input.borrow_mut();
                let len = input.text().len();
                input.set_selection(0..len);
                input.insert_text(value);
                self.mark_paint_dirty(node);
                true
            }
            accesskit::Action::SetTextSelection => {
                let Some(input) = self.text_input(node) else {
                    return false;
                };
                let Some(accesskit::ActionData::SetTextSelection(selection)) = &request.data else {
                    return false;
                };
                let mut input = input.borrow_mut();
                let text = input.text().to_string();
                let start = char_to_byte(&text, selection.anchor.character_index);
                let end = char_to_byte(&text, selection.focus.character_index);
                input.set_selection(start..end);
                self.mark_paint_dirty(node);
                true
            }
            _ => false,
        }
    }

    /// Build an AccessKit tree update from the current accessibility snapshot.
    #[must_use]
    pub fn accesskit_tree(&self) -> Option<accesskit::TreeUpdate> {
        let snapshot = self.a11y_tree();
        if snapshot.nodes.is_empty() {
            return None;
        }
        let (root, synthetic) = if snapshot.roots.len() == 1 {
            (snapshot.roots[0], None)
        } else {
            let mut root = accesskit::Node::new(accesskit::Role::GenericContainer);
            root.set_children(
                snapshot
                    .roots
                    .iter()
                    .map(|id| accesskit::NodeId(*id))
                    .collect::<Vec<_>>(),
            );
            (0, Some(root))
        };
        let mut nodes = snapshot
            .nodes
            .into_iter()
            .map(|node| {
                let mut output = accesskit::Node::new(match node.role {
                    A11yRole::Generic => accesskit::Role::GenericContainer,
                    A11yRole::Button => accesskit::Role::Button,
                    A11yRole::Text => accesskit::Role::Label,
                    A11yRole::TextInput => accesskit::Role::TextInput,
                });
                if let Some(label) = node.label {
                    output.set_label(label);
                }
                if let Some(value) = node.value {
                    output.set_value(value);
                }
                if node.disabled {
                    output.set_disabled();
                }
                output.set_bounds(accesskit::Rect {
                    x0: f64::from(node.bounds.x),
                    y0: f64::from(node.bounds.y),
                    x1: f64::from(node.bounds.right()),
                    y1: f64::from(node.bounds.bottom()),
                });
                if !node.children.is_empty() {
                    output.set_children(
                        node.children
                            .into_iter()
                            .map(accesskit::NodeId)
                            .collect::<Vec<_>>(),
                    );
                }
                (accesskit::NodeId(node.id), output)
            })
            .collect::<Vec<_>>();
        if let Some(root) = synthetic {
            nodes.push((accesskit::NodeId(0), root));
        }
        let root_id = accesskit::NodeId(root);
        let focus = self
            .focused()
            .and_then(|node| self.renderer_id(node))
            .map_or(root_id, accesskit::NodeId);
        Some(accesskit::TreeUpdate {
            nodes,
            tree: Some(accesskit::TreeInfo::new(root_id)),
            focus,
            tree_id: accesskit::TreeId::ROOT,
        })
    }

    fn a11y_visit(&self, node: ViewNodeId, out: &mut Vec<A11yNode>) -> u64 {
        let (children, renderer_id, role, label, value, disabled, focusable, bounds) = {
            let nodes = self.inner.nodes.borrow();
            let Some(data) = nodes.get(node) else {
                return 0;
            };
            let bounds = self.layout_rect(node).unwrap_or_default();
            let label = data.a11y_label.clone().or_else(|| {
                if data.a11y_role == A11yRole::Button {
                    data.children.iter().find_map(|child| {
                        nodes
                            .get(*child)
                            .and_then(|child| child.text.as_deref())
                            .map(str::to_string)
                    })
                } else if data.a11y_role == A11yRole::Text {
                    data.text.clone()
                } else {
                    None
                }
            });
            (
                data.children.clone(),
                data.renderer_id,
                data.a11y_role,
                label,
                data.a11y_value.clone(),
                data.disabled,
                data.focusable,
                bounds,
            )
        };
        let mut child_ids = Vec::with_capacity(children.len());
        for child in children {
            let id = self.a11y_visit(child, out);
            if id != 0 {
                child_ids.push(id);
            }
        }
        out.push(A11yNode {
            id: renderer_id,
            role,
            label,
            value,
            disabled,
            focusable,
            focused: self.focused() == Some(node),
            bounds,
            children: child_ids,
        });
        renderer_id
    }

    fn dispatch_view_event(&self, node: ViewNodeId, event: ViewEvent) -> bool {
        let cx = self.inner.runtime.cx();
        let mut path = Vec::new();
        let mut current = Some(node);
        while let Some(node) = current {
            path.push(node);
            current = self
                .inner
                .nodes
                .borrow()
                .get(node)
                .and_then(|data| data.parent);
        }

        let mut stopped = false;
        for phase in [DispatchPhase::Capture, DispatchPhase::Bubble] {
            let iter: Box<dyn Iterator<Item = &ViewNodeId>> = if phase == DispatchPhase::Capture {
                Box::new(path.iter().rev())
            } else {
                Box::new(path.iter())
            };
            for node in iter {
                let callbacks = {
                    let nodes = self.inner.nodes.borrow();
                    nodes.get(*node).map(|data| {
                        data.view_listeners
                            .iter()
                            .filter(|listener| listener.event == event && listener.phase == phase)
                            .map(|listener| Rc::clone(&listener.callback))
                            .collect::<Vec<_>>()
                    })
                };
                if let Some(callbacks) = callbacks {
                    for callback in callbacks {
                        match (callback.borrow_mut())(&cx) {
                            DispatchResult::Continue => {}
                            DispatchResult::Stop => {
                                stopped = true;
                                break;
                            }
                            DispatchResult::Handled => return true,
                        }
                    }
                }
                if stopped {
                    break;
                }
            }
            if stopped {
                break;
            }
        }
        false
    }

    /// Activate an accessibility node by its stable id.
    pub fn a11y_activate(&self, id: u64) -> bool {
        let node = self
            .inner
            .nodes
            .borrow()
            .iter()
            .find(|(_, node)| node.renderer_id == id)
            .map(|(handle, _)| handle.id());
        node.is_some_and(|node| self.click_node(node))
    }

    /// Focus an accessibility node by its stable id.
    pub fn a11y_focus(&self, id: u64) -> bool {
        let node = self
            .inner
            .nodes
            .borrow()
            .iter()
            .find(|(_, node)| node.renderer_id == id && node.focusable && !node.disabled)
            .map(|(handle, _)| handle.id());
        if let Some(node) = node {
            self.set_focus(Some(node));
            true
        } else {
            false
        }
    }

    fn focus_fallback(&self, removed: ViewNodeId) -> Option<ViewNodeId> {
        let nodes = self.inner.nodes.borrow();
        let mut current = nodes.get(removed).and_then(|node| node.parent);
        while let Some(node) = current {
            let Some(data) = nodes.get(node) else {
                break;
            };
            if data.focusable && !data.disabled {
                return Some(node);
            }
            current = data.parent;
        }
        let parent = nodes.get(removed).and_then(|node| node.parent)?;
        let siblings = nodes.get(parent).map(|node| node.children.clone())?;
        let index = siblings.iter().position(|node| *node == removed)?;
        siblings[..index]
            .iter()
            .rev()
            .find_map(|sibling| self.focusable_in_subtree(*sibling, &nodes))
    }

    fn focusable_in_subtree(
        &self,
        node: ViewNodeId,
        nodes: &reactive_core::Arena<ViewNode>,
    ) -> Option<ViewNodeId> {
        let data = nodes.get(node)?;
        if data.focusable && !data.disabled {
            return Some(node);
        }
        for child in data.children.iter().rev() {
            if let Some(found) = self.focusable_in_subtree(*child, nodes) {
                return Some(found);
            }
        }
        None
    }

    fn is_descendant(&self, node: ViewNodeId, ancestor: ViewNodeId) -> bool {
        let nodes = self.inner.nodes.borrow();
        let mut current = Some(node);
        while let Some(id) = current {
            if id == ancestor {
                return true;
            }
            current = nodes.get(id).and_then(|node| node.parent);
        }
        false
    }

    /// Mount a view as a window root and return its node.
    pub fn mount(&self, cx: &reactive_core::Cx<'_>, view: View) -> ViewNodeId {
        let id = view.mount(cx, self);
        if let Some(layout_id) = self.inner.nodes.borrow().get(id).map(|node| node.layout_id) {
            self.inner.layout.borrow_mut().add_root(layout_id);
        }
        self.inner.roots.borrow_mut().push(id);
        self.mark_paint_dirty(id);
        id
    }

    /// Remove a mounted subtree, its listeners, bindings, and owner.
    pub fn unmount(&self, cx: &reactive_core::Cx<'_>, id: ViewNodeId) {
        self.remove_view(cx, id);
    }

    /// Run one frame: begin, flush the graph, end.
    pub fn frame(&self, cx: &reactive_core::Cx<'_>, frame: u64) {
        {
            let mut renderer = self.inner.renderer.borrow_mut();
            renderer
                .begin_frame(&FrameInfo { frame })
                .expect("begin frame");
        }
        cx.flush().expect("flush");
        self.flush_layout_children();
        let stats = self
            .inner
            .layout
            .borrow_mut()
            .compute(self.inner.viewport.get());
        self.inner.last_layout.set(stats);
        if stats.ran {
            let changes = self.inner.layout.borrow().geometry_changes().clone();
            if !changes.is_empty() {
                let ids = {
                    let nodes = self.inner.nodes.borrow();
                    nodes
                        .iter()
                        .filter(|(_, node)| changes.contains(&node.layout_id))
                        .map(|(id, _)| id.id())
                        .collect::<Vec<_>>()
                };
                for id in ids {
                    self.mark_paint_dirty(id);
                }
            }
        }
        self.update_tooltip_at(Instant::now());
        self.clear_dirty();
        self.inner
            .renderer
            .borrow_mut()
            .end_frame()
            .expect("end frame");
    }

    /// Renderer node behind a view node.
    #[must_use]
    pub fn renderer_id(&self, id: ViewNodeId) -> Option<RendererNodeId> {
        self.inner
            .nodes
            .borrow()
            .get(id)
            .map(|node| node.renderer_id)
    }

    /// View children.
    #[must_use]
    pub fn children(&self, id: ViewNodeId) -> Vec<ViewNodeId> {
        self.inner
            .nodes
            .borrow()
            .get(id)
            .map(|node| node.children.clone())
            .unwrap_or_default()
    }

    /// Number of structurally dirty nodes.
    #[must_use]
    pub fn dirty_views(&self) -> usize {
        self.inner
            .nodes
            .borrow()
            .iter()
            .filter(|(_, node)| node.dirty.get())
            .count()
    }

    /// Structural dirty nodes observed at the end of the last frame.
    #[must_use]
    pub fn last_frame_dirty(&self) -> usize {
        self.inner.last_dirty.get()
    }

    fn mark_paint_dirty(&self, id: ViewNodeId) {
        let mut current = Some(id);
        while let Some(node) = current {
            self.inner.paint_dirty.borrow_mut().insert(node);
            current = self
                .inner
                .nodes
                .borrow()
                .get(node)
                .and_then(|node| node.parent);
        }
    }

    fn mark_dirty(&self, id: ViewNodeId) {
        if let Some(node) = self.inner.nodes.borrow().get(id) {
            node.dirty.set(true);
        }
    }

    fn clear_dirty(&self) {
        let nodes = self.inner.nodes.borrow();
        let dirty = nodes.iter().filter(|(_, node)| node.dirty.get()).count();
        self.inner.last_dirty.set(dirty);
        for (_, node) in nodes.iter() {
            node.dirty.set(false);
        }
    }

    fn create_element(&self, tag: &str) -> ViewNodeId {
        let renderer_id = self
            .inner
            .renderer
            .borrow_mut()
            .create_element(ElementTag::new(tag))
            .expect("create element");
        let id = self.alloc_node(renderer_id, default_style(tag), None);
        let role = match tag {
            "Button" => A11yRole::Button,
            _ => A11yRole::Generic,
        };
        self.set_a11y(id, Some(role), None, None);
        id
    }

    fn create_text(&self, text: &str) -> ViewNodeId {
        let renderer_id = self
            .inner
            .renderer
            .borrow_mut()
            .create_text(text)
            .expect("create text");
        let style = default_text_style();
        let id = self.alloc_node(
            renderer_id,
            style,
            Some(TextContent {
                text: text.to_string(),
                style: style.text,
            }),
        );
        self.set_a11y(id, Some(A11yRole::Text), None, None);
        id
    }

    fn create_fragment(&self) -> ViewNodeId {
        let renderer_id = self
            .inner
            .renderer
            .borrow_mut()
            .create_fragment()
            .expect("create fragment");
        self.alloc_node(renderer_id, default_style("Fragment"), None)
    }

    fn alloc_node(
        &self,
        renderer_id: RendererNodeId,
        style: ResolvedStyle,
        text: Option<TextContent>,
    ) -> ViewNodeId {
        let text_value = text.as_ref().map(|text| text.text.clone());
        let layout_id = self.inner.layout.borrow_mut().create_node(style, text);
        self.inner
            .nodes
            .borrow_mut()
            .insert(ViewNode {
                renderer_id,
                layout_id,
                style,
                text: text_value,
                parent: None,
                children: Vec::new(),
                bindings: Vec::new(),
                owner: None,
                dirty: Cell::new(false),
                scroll_offset: Cell::new((0.0, 0.0)),
                interactive: false,
                focusable: false,
                tab_index: None,
                disabled: false,
                a11y_role: A11yRole::Generic,
                a11y_label: None,
                a11y_value: None,
                cursor: None,
                tooltip: None,
                hover_style: None,
                focus_style: None,
                view_listeners: Vec::new(),
                key_bindings: Vec::new(),
                drag_start: None,
                drag_move: None,
                drag_end: None,
                drop_target: None,
            })
            .id()
    }

    fn insert_child(&self, parent: ViewNodeId, child: ViewNodeId, anchor: Option<ViewNodeId>) {
        let (parent_renderer, child_renderer, anchor_renderer, old_parent) = {
            let nodes = self.inner.nodes.borrow();
            let parent = nodes.get(parent).expect("parent view node");
            let child = nodes.get(child).expect("child view node");
            let parent_renderer = parent.renderer_id;
            let child_renderer = child.renderer_id;
            let anchor_renderer =
                anchor.map(|anchor| nodes.get(anchor).expect("anchor view node").renderer_id);
            (
                parent_renderer,
                child_renderer,
                anchor_renderer,
                child.parent,
            )
        };

        if let Some(old_parent) = old_parent {
            if old_parent == parent {
                // Reordering inside the same parent: remove the old slot first.
                if let Some(parent_node) = self.inner.nodes.borrow_mut().get_mut(parent) {
                    parent_node.children.retain(|existing| *existing != child);
                }
            } else if let Some(parent_node) = self.inner.nodes.borrow_mut().get_mut(old_parent) {
                parent_node.children.retain(|existing| *existing != child);
            }
        }

        let index = {
            let nodes = self.inner.nodes.borrow();
            let parent = nodes.get(parent).expect("parent view node");
            match anchor {
                Some(anchor) => parent
                    .children
                    .iter()
                    .position(|existing| *existing == anchor)
                    .unwrap_or(parent.children.len()),
                None => parent.children.len(),
            }
        };

        self.inner
            .renderer
            .borrow_mut()
            .insert(parent_renderer, child_renderer, anchor_renderer)
            .expect("insert child");

        {
            let mut nodes = self.inner.nodes.borrow_mut();
            if let Some(parent_node) = nodes.get_mut(parent) {
                parent_node.children.insert(index, child);
            }
            if let Some(child_node) = nodes.get_mut(child) {
                child_node.parent = Some(parent);
            }
        }
        self.inner
            .pending_layout_children
            .borrow_mut()
            .insert(parent);
        if let Some(old_parent) = old_parent {
            if old_parent != parent {
                self.inner
                    .pending_layout_children
                    .borrow_mut()
                    .insert(old_parent);
            }
        }
        self.mark_dirty(parent);
        self.mark_paint_dirty(parent);
        if let Some(old_parent) = old_parent {
            if old_parent != parent {
                self.mark_paint_dirty(old_parent);
            }
        }
    }

    fn flush_layout_children(&self) {
        let pending = std::mem::take(&mut *self.inner.pending_layout_children.borrow_mut());
        for parent in pending {
            self.sync_layout_children(parent);
        }
    }

    fn sync_layout_children(&self, parent: ViewNodeId) {
        let (layout_id, children) = {
            let nodes = self.inner.nodes.borrow();
            let Some(node) = nodes.get(parent) else {
                return;
            };
            let children = node
                .children
                .iter()
                .filter_map(|child| nodes.get(*child).map(|child| child.layout_id))
                .collect::<Vec<_>>();
            (node.layout_id, children)
        };
        self.inner
            .layout
            .borrow_mut()
            .set_children(layout_id, &children);
    }

    fn add_binding(&self, view: ViewNodeId, binding: EffectHandle) {
        if let Some(node) = self.inner.nodes.borrow_mut().get_mut(view) {
            node.bindings.push(binding);
        }
    }

    fn add_view_listener(
        &self,
        view: ViewNodeId,
        event: ViewEvent,
        phase: DispatchPhase,
        callback: Box<dyn FnMut(&reactive_core::Cx<'_>) -> DispatchResult>,
    ) {
        if let Some(node) = self.inner.nodes.borrow_mut().get_mut(view) {
            node.interactive = true;
            node.view_listeners.push(ViewListener {
                event,
                phase,
                callback: Rc::new(RefCell::new(callback)),
            });
        }
    }

    fn add_key_binding(
        &self,
        view: ViewNodeId,
        stroke: KeyStroke,
        phase: DispatchPhase,
        callback: Box<dyn FnMut(&reactive_core::Cx<'_>) -> DispatchResult>,
    ) {
        if let Some(node) = self.inner.nodes.borrow_mut().get_mut(view) {
            node.focusable = true;
            node.key_bindings.push(KeyBinding {
                stroke,
                phase,
                callback: Rc::new(RefCell::new(callback)),
            });
        }
    }

    fn set_interactive(&self, view: ViewNodeId) {
        if let Some(node) = self.inner.nodes.borrow_mut().get_mut(view) {
            node.interactive = true;
        }
    }

    fn set_focusable(&self, view: ViewNodeId, focusable: bool) {
        if let Some(node) = self.inner.nodes.borrow_mut().get_mut(view) {
            node.focusable = focusable;
        }
    }

    fn set_tab_index(&self, view: ViewNodeId, index: Option<i32>) {
        if let Some(node) = self.inner.nodes.borrow_mut().get_mut(view) {
            node.tab_index = index;
        }
    }

    /// Set or clear a node's pointer cursor.
    pub fn set_cursor(&self, view: ViewNodeId, cursor: Option<CursorIcon>) {
        if let Some(node) = self.inner.nodes.borrow_mut().get_mut(view) {
            node.cursor = cursor;
        }
    }

    /// Set or clear a node's hover tooltip.
    pub fn set_tooltip(&self, view: ViewNodeId, tooltip: Option<String>) {
        if let Some(node) = self.inner.nodes.borrow_mut().get_mut(view) {
            node.tooltip = tooltip;
        }
    }

    /// Set or clear a node's hover state style.
    pub fn set_hover_style(&self, view: ViewNodeId, style: Option<StateStyle>) {
        if let Some(node) = self.inner.nodes.borrow_mut().get_mut(view) {
            node.hover_style = style;
        }
    }

    /// Set or clear a node's focus state style.
    pub fn set_focus_style(&self, view: ViewNodeId, style: Option<StateStyle>) {
        if let Some(node) = self.inner.nodes.borrow_mut().get_mut(view) {
            node.focus_style = style;
        }
    }

    fn set_disabled(&self, view: ViewNodeId, disabled: bool) {
        if let Some(node) = self.inner.nodes.borrow_mut().get_mut(view) {
            node.disabled = disabled;
        }
    }

    fn set_a11y(
        &self,
        view: ViewNodeId,
        role: Option<A11yRole>,
        label: Option<String>,
        value: Option<String>,
    ) {
        if let Some(node) = self.inner.nodes.borrow_mut().get_mut(view) {
            if let Some(role) = role {
                node.a11y_role = role;
            }
            if label.is_some() {
                node.a11y_label = label;
            }
            if value.is_some() {
                node.a11y_value = value;
            }
        }
    }

    fn set_drag_start(&self, view: ViewNodeId, callback: DragStartFn) {
        if let Some(node) = self.inner.nodes.borrow_mut().get_mut(view) {
            node.interactive = true;
            node.drag_start = Some(Rc::new(RefCell::new(callback)));
        }
    }

    fn set_drag_move(&self, view: ViewNodeId, callback: DragMoveFn) {
        if let Some(node) = self.inner.nodes.borrow_mut().get_mut(view) {
            node.drag_move = Some(Rc::new(RefCell::new(callback)));
        }
    }

    fn set_drag_end(&self, view: ViewNodeId, callback: DragEndFn) {
        if let Some(node) = self.inner.nodes.borrow_mut().get_mut(view) {
            node.drag_end = Some(Rc::new(RefCell::new(callback)));
        }
    }

    fn set_drop_target(&self, view: ViewNodeId, type_id: TypeId, callback: DropFn) {
        if let Some(node) = self.inner.nodes.borrow_mut().get_mut(view) {
            node.interactive = true;
            node.drop_target = Some(DropTarget {
                type_id,
                callback: Rc::new(RefCell::new(callback)),
            });
        }
    }

    fn set_owner(&self, view: ViewNodeId, owner: OwnerHandle) {
        if let Some(node) = self.inner.nodes.borrow_mut().get_mut(view) {
            node.owner = Some(owner);
        }
    }

    fn remove_view(&self, cx: &reactive_core::Cx<'_>, id: ViewNodeId) {
        self.inner.roots.borrow_mut().retain(|root| *root != id);
        if self
            .inner
            .focused
            .borrow()
            .is_some_and(|focus| self.is_descendant(focus, id))
        {
            let fallback = self.focus_fallback(id);
            self.set_focus(fallback);
        }
        if self
            .inner
            .drag
            .borrow()
            .as_ref()
            .is_some_and(|drag| self.is_descendant(drag.source, id))
        {
            self.cancel_drag();
        }
        if self
            .inner
            .drag_target
            .borrow()
            .is_some_and(|target| target == id)
        {
            *self.inner.drag_target.borrow_mut() = None;
        }
        let Some(node_data) = ({
            let nodes = self.inner.nodes.borrow();
            nodes.get(id).map(|node| {
                (
                    node.children.clone(),
                    node.renderer_id,
                    node.layout_id,
                    node.parent,
                )
            })
        }) else {
            return;
        };
        let (children, renderer_id, layout_id, parent) = node_data;
        if let Some(parent) = parent {
            self.mark_dirty(parent);
            self.mark_paint_dirty(parent);
        }

        for child in children {
            self.remove_view(cx, child);
        }

        if let Some(parent) = parent {
            if let Some(parent_node) = self.inner.nodes.borrow_mut().get_mut(parent) {
                parent_node.children.retain(|existing| *existing != id);
                let parent_renderer = parent_node.renderer_id;
                let _ = self
                    .inner
                    .renderer
                    .borrow_mut()
                    .remove(parent_renderer, renderer_id);
                self.inner
                    .pending_layout_children
                    .borrow_mut()
                    .insert(parent);
            }
        }

        self.inner.layout.borrow_mut().remove_node(layout_id);
        self.inner.scene.borrow_mut().remove(id);
        self.inner.text_inputs.borrow_mut().remove(&id);
        if self
            .inner
            .hovered
            .borrow()
            .is_some_and(|hovered| hovered == id)
        {
            *self.inner.hovered.borrow_mut() = None;
            *self.inner.tooltip.borrow_mut() = None;
        }
        if self
            .inner
            .pressed
            .borrow()
            .is_some_and(|pressed| pressed == id)
        {
            *self.inner.pressed.borrow_mut() = None;
        }
        let node = self.inner.nodes.borrow_mut().remove(id);
        if let Some(node) = node {
            for binding in node.bindings {
                binding.dispose(cx);
            }
            if let Some(owner) = node.owner {
                owner.dispose(cx);
            }
        }
    }
}

fn is_scrollable(overflow: Overflow) -> bool {
    matches!(overflow, Overflow::Scroll | Overflow::Auto)
}

fn char_to_byte(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map_or(text.len(), |(index, _)| index)
}

fn contains_point(rect: Rect, x: f32, y: f32) -> bool {
    x >= rect.x && x <= rect.right() && y >= rect.y && y <= rect.bottom()
}

fn intersect_rect(a: Rect, b: Rect) -> Rect {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = a.right().min(b.right());
    let bottom = a.bottom().min(b.bottom());
    Rect {
        x,
        y,
        width: (right - x).max(0.0),
        height: (bottom - y).max(0.0),
    }
}

fn transform_rect(transform: Transform, rect: Rect) -> Rect {
    let a = transform.apply_point(renderer_api::Point {
        x: rect.x,
        y: rect.y,
    });
    let b = transform.apply_point(renderer_api::Point {
        x: rect.right(),
        y: rect.bottom(),
    });
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    Rect {
        x,
        y,
        width: (a.x.max(b.x) - x).abs(),
        height: (a.y.max(b.y) - y).abs(),
    }
}

fn default_style(tag: &str) -> ResolvedStyle {
    let mut style = ResolvedStyle {
        display: Display::Flex,
        ..ResolvedStyle::default()
    };
    style.flex.direction = match tag {
        "Row" | "Button" => FlexDirection::Row,
        _ => FlexDirection::Column,
    };
    style
}

fn default_text_style() -> ResolvedStyle {
    ResolvedStyle {
        display: Display::Flex,
        text: TextStyle::default(),
        ..ResolvedStyle::default()
    }
}

/// A mountable view.
pub struct View {
    mount: MountFn,
}

type MountFn = Box<dyn FnOnce(&reactive_core::Cx<'_>, &ViewTree) -> ViewNodeId>;

impl View {
    /// Mount once into a tree.
    pub fn mount(self, cx: &reactive_core::Cx<'_>, tree: &ViewTree) -> ViewNodeId {
        (self.mount)(cx, tree)
    }

    /// Apply a resolved style to the mounted root of this view.
    #[must_use]
    pub fn style(self, style: ResolvedStyle) -> Self {
        let mount = self.mount;
        view(move |cx, tree| {
            let id = mount(cx, tree);
            tree.set_style(id, style);
            id
        })
    }

    /// Make this view focusable or not.
    #[must_use]
    pub fn focusable(self, focusable: bool) -> Self {
        let mount = self.mount;
        view(move |cx, tree| {
            let id = mount(cx, tree);
            tree.set_focusable(id, focusable);
            id
        })
    }

    /// Set an explicit tab index.
    #[must_use]
    pub fn tab_index(self, index: i32) -> Self {
        let mount = self.mount;
        view(move |cx, tree| {
            let id = mount(cx, tree);
            tree.set_tab_index(id, Some(index));
            tree.set_focusable(id, true);
            id
        })
    }

    /// Set the pointer cursor for this view.
    #[must_use]
    pub fn cursor(self, cursor: CursorIcon) -> Self {
        let mount = self.mount;
        view(move |cx, tree| {
            let id = mount(cx, tree);
            tree.set_cursor(id, Some(cursor));
            id
        })
    }

    /// Set a tooltip shown after hovering this view.
    #[must_use]
    pub fn tooltip(self, tooltip: impl Into<String>) -> Self {
        let tooltip = tooltip.into();
        let mount = self.mount;
        view(move |cx, tree| {
            let id = mount(cx, tree);
            tree.set_tooltip(id, Some(tooltip));
            id
        })
    }

    /// Set paint overrides while this view or a descendant is hovered.
    #[must_use]
    pub fn hover_style(self, style: StateStyle) -> Self {
        let mount = self.mount;
        view(move |cx, tree| {
            let id = mount(cx, tree);
            tree.set_hover_style(id, Some(style));
            id
        })
    }

    /// Set paint overrides while this view or a descendant has focus.
    #[must_use]
    pub fn focus_style(self, style: StateStyle) -> Self {
        let mount = self.mount;
        view(move |cx, tree| {
            let id = mount(cx, tree);
            tree.set_focus_style(id, Some(style));
            id
        })
    }

    /// Disable pointer and keyboard interaction.
    #[must_use]
    pub fn disabled(self, disabled: bool) -> Self {
        let mount = self.mount;
        view(move |cx, tree| {
            let id = mount(cx, tree);
            tree.set_disabled(id, disabled);
            id
        })
    }

    /// Set the accessibility label.
    #[must_use]
    pub fn a11y_label(self, label: impl Into<String>) -> Self {
        let label = label.into();
        let mount = self.mount;
        view(move |cx, tree| {
            let id = mount(cx, tree);
            tree.set_a11y(id, None, Some(label.clone()), None);
            id
        })
    }

    /// Set the accessibility role.
    #[must_use]
    pub fn a11y_role(self, role: A11yRole) -> Self {
        let mount = self.mount;
        view(move |cx, tree| {
            let id = mount(cx, tree);
            tree.set_a11y(id, Some(role), None, None);
            id
        })
    }

    /// Set the accessibility value.
    #[must_use]
    pub fn a11y_value(self, value: impl Into<String>) -> Self {
        let value = value.into();
        let mount = self.mount;
        view(move |cx, tree| {
            let id = mount(cx, tree);
            tree.set_a11y(id, None, None, Some(value.clone()));
            id
        })
    }

    /// Add a click listener to the mounted root.
    #[must_use]
    pub fn on_click(self, handler: impl Fn(&reactive_core::Cx<'_>) + 'static) -> Self {
        self.on_click_filter(DispatchPhase::Bubble, move |cx| {
            handler(cx);
            DispatchResult::Continue
        })
    }

    /// Start a drag from the mounted root when the pointer leaves its click
    /// threshold. The returned payload determines drop-target compatibility.
    #[must_use]
    pub fn on_drag_start(
        self,
        handler: impl Fn(&reactive_core::Cx<'_>) -> DragPayload + 'static,
    ) -> Self {
        let mount = self.mount;
        view(move |cx, tree| {
            let id = mount(cx, tree);
            tree.set_drag_start(id, Box::new(move |cx| handler(cx)));
            id
        })
    }

    /// Receive drag move events on the source.
    #[must_use]
    pub fn on_drag(self, handler: impl Fn(&reactive_core::Cx<'_>, &DragEvent) + 'static) -> Self {
        let mount = self.mount;
        view(move |cx, tree| {
            let id = mount(cx, tree);
            tree.set_drag_move(id, Box::new(move |cx, event| handler(cx, event)));
            id
        })
    }

    /// Receive the drag completion event on the source.
    #[must_use]
    pub fn on_drag_end(self, handler: impl Fn(&reactive_core::Cx<'_>, &DragEnd) + 'static) -> Self {
        let mount = self.mount;
        view(move |cx, tree| {
            let id = mount(cx, tree);
            tree.set_drag_end(id, Box::new(move |cx, event| handler(cx, event)));
            id
        })
    }

    /// Accept a typed drag payload. The handler returns whether it accepted.
    #[must_use]
    pub fn on_drop<T: 'static>(
        self,
        handler: impl Fn(&reactive_core::Cx<'_>, &T) -> bool + 'static,
    ) -> Self {
        let mount = self.mount;
        view(move |cx, tree| {
            let id = mount(cx, tree);
            tree.set_drop_target(
                id,
                TypeId::of::<T>(),
                Box::new(move |cx, payload| match payload.get::<T>() {
                    Some(value) => handler(cx, value),
                    None => false,
                }),
            );
            id
        })
    }

    /// Add a wheel listener to the mounted root.
    #[must_use]
    pub fn on_wheel(self, handler: impl Fn(&reactive_core::Cx<'_>) + 'static) -> Self {
        self.on_wheel_filter(DispatchPhase::Bubble, move |cx| {
            handler(cx);
            DispatchResult::Continue
        })
    }

    /// Add a capture-phase wheel listener.
    #[must_use]
    pub fn on_wheel_capture(self, handler: impl Fn(&reactive_core::Cx<'_>) + 'static) -> Self {
        self.on_wheel_filter(DispatchPhase::Capture, move |cx| {
            handler(cx);
            DispatchResult::Continue
        })
    }

    /// Add a wheel listener with an explicit phase and result.
    #[must_use]
    pub fn on_wheel_filter(
        self,
        phase: DispatchPhase,
        handler: impl Fn(&reactive_core::Cx<'_>) -> DispatchResult + 'static,
    ) -> Self {
        let mount = self.mount;
        view(move |cx, tree| {
            let id = mount(cx, tree);
            tree.add_view_listener(id, ViewEvent::Wheel, phase, Box::new(move |cx| handler(cx)));
            id
        })
    }

    /// Add a capture-phase click listener to the mounted root.
    #[must_use]
    pub fn on_click_capture(self, handler: impl Fn(&reactive_core::Cx<'_>) + 'static) -> Self {
        self.on_click_filter(DispatchPhase::Capture, move |cx| {
            handler(cx);
            DispatchResult::Continue
        })
    }

    /// Add a click listener with an explicit phase and result.
    #[must_use]
    pub fn on_click_filter(
        self,
        phase: DispatchPhase,
        handler: impl Fn(&reactive_core::Cx<'_>) -> DispatchResult + 'static,
    ) -> Self {
        let mount = self.mount;
        view(move |cx, tree| {
            let id = mount(cx, tree);
            tree.add_view_listener(id, ViewEvent::Click, phase, Box::new(move |cx| handler(cx)));
            id
        })
    }

    /// Add a key binding to the mounted root.
    #[must_use]
    pub fn on_key(
        self,
        stroke: KeyStroke,
        handler: impl Fn(&reactive_core::Cx<'_>) + 'static,
    ) -> Self {
        self.on_key_filter(stroke, DispatchPhase::Bubble, move |cx| {
            handler(cx);
            DispatchResult::Continue
        })
    }

    /// Add a capture-phase key binding.
    #[must_use]
    pub fn on_key_capture(
        self,
        stroke: KeyStroke,
        handler: impl Fn(&reactive_core::Cx<'_>) + 'static,
    ) -> Self {
        self.on_key_filter(stroke, DispatchPhase::Capture, move |cx| {
            handler(cx);
            DispatchResult::Continue
        })
    }

    /// Add a key binding with an explicit phase and result.
    #[must_use]
    pub fn on_key_filter(
        self,
        stroke: KeyStroke,
        phase: DispatchPhase,
        handler: impl Fn(&reactive_core::Cx<'_>) -> DispatchResult + 'static,
    ) -> Self {
        let mount = self.mount;
        view(move |cx, tree| {
            let id = mount(cx, tree);
            tree.add_key_binding(id, stroke.clone(), phase, Box::new(move |cx| handler(cx)));
            id
        })
    }
}

fn view<F>(mount: F) -> View
where
    F: FnOnce(&reactive_core::Cx<'_>, &ViewTree) -> ViewNodeId + 'static,
{
    View {
        mount: Box::new(mount),
    }
}

/// A dynamic text source.
pub struct TextSource(Box<dyn Fn(&reactive_core::Cx<'_>) -> String>);

impl TextSource {
    fn get(&self, cx: &reactive_core::Cx<'_>) -> String {
        (self.0)(cx)
    }
}

impl From<&str> for TextSource {
    fn from(value: &str) -> Self {
        let value = value.to_string();
        Self(Box::new(move |_cx| value.clone()))
    }
}

impl From<String> for TextSource {
    fn from(value: String) -> Self {
        Self(Box::new(move |_cx| value.clone()))
    }
}

/// Create a text view.
pub fn text(source: impl Into<TextSource>) -> View {
    let source = source.into();
    view(move |cx, tree| {
        let id = tree.create_text("");
        let renderer_id = tree.renderer_id(id).expect("text node");
        let renderer = Rc::clone(&tree.inner.renderer);
        let tree_for_layout = tree.clone();
        let binding = render_effect(
            cx,
            move |cx| source.get(cx),
            move |_cx, value| {
                let text = value.clone();
                renderer
                    .borrow_mut()
                    .set_text(renderer_id, text.as_str())
                    .expect("set text");
                tree_for_layout.set_view_text(id, text);
            },
        );
        tree.add_binding(id, binding);
        id
    })
}

/// Create a text view from a closure.
pub fn text_fn(f: impl Fn(&reactive_core::Cx<'_>) -> String + 'static) -> View {
    let source = TextSource(Box::new(f));
    text(source)
}

/// Create a column.
pub fn column(children: Vec<View>) -> View {
    container("Column", children)
}

/// Create a row.
pub fn row(children: Vec<View>) -> View {
    container("Row", children)
}

fn container(tag: &'static str, children: Vec<View>) -> View {
    view(move |cx, tree| {
        let id = tree.create_element(tag);
        for child in children {
            let child_id = child.mount(cx, tree);
            tree.insert_child(id, child_id, None);
        }
        id
    })
}

/// Create a button with child views.
pub fn button(children: Vec<View>, on_click: impl Fn(&reactive_core::Cx<'_>) + 'static) -> View {
    view(move |cx, tree| {
        let id = tree.create_element("Button");
        for child in children {
            let child_id = child.mount(cx, tree);
            tree.insert_child(id, child_id, None);
        }
        tree.set_interactive(id);
        tree.set_focusable(id, true);
        tree.set_a11y(id, Some(A11yRole::Button), None, None);
        tree.add_view_listener(
            id,
            ViewEvent::Click,
            DispatchPhase::Bubble,
            Box::new(move |cx| {
                on_click(cx);
                DispatchResult::Continue
            }),
        );
        id
    })
}

/// Create a text button.
pub fn button_text(
    label: impl Into<TextSource>,
    on_click: impl Fn(&reactive_core::Cx<'_>) + 'static,
) -> View {
    button(vec![text(label)], on_click)
}

/// A conditionally rendered branch.
pub fn show<W, T, F>(when: W, then: T, fallback: F) -> View
where
    W: Fn(&reactive_core::Cx<'_>) -> bool + Clone + 'static,
    T: Fn(&reactive_core::Cx<'_>) -> View + 'static,
    F: Fn(&reactive_core::Cx<'_>) -> View + 'static,
{
    let when_key = when.clone();
    view_slot(
        move |cx| when_key(cx),
        move |cx| {
            if when(cx) {
                then(cx)
            } else {
                fallback(cx)
            }
        },
    )
}

/// A dynamic branch keyed by a value. A key change replaces the branch.
pub fn view_slot<K, F, B>(key: F, build: B) -> View
where
    K: Eq + Clone + 'static,
    F: Fn(&reactive_core::Cx<'_>) -> K + 'static,
    B: Fn(&reactive_core::Cx<'_>) -> View + 'static,
{
    view(move |cx, tree| {
        let id = tree.create_fragment();
        let state: Rc<RefCell<Option<(K, ViewNodeId, OwnerHandle)>>> = Rc::new(RefCell::new(None));
        let tree_for_effect = tree.clone();
        let state_for_effect = Rc::clone(&state);
        let binding = render_effect(
            cx,
            move |cx| key(cx),
            move |cx, key| {
                let mut state = state_for_effect.borrow_mut();
                if state.as_ref().is_some_and(|(current, _, _)| current == key) {
                    return;
                }
                if let Some((_, old, _)) = state.take() {
                    tree_for_effect.remove_view(cx, old);
                }
                let owner = cx.create_owner();
                let branch = cx.with_owner(owner, |cx| build(cx).mount(cx, &tree_for_effect));
                tree_for_effect.set_owner(branch, owner);
                tree_for_effect.insert_child(id, branch, None);
                *state = Some((key.clone(), branch, owner));
            },
        );
        tree.add_binding(id, binding);
        id
    })
}

/// A keyed list. Existing keys keep their mounted view and owner when reordered.
pub fn keyed_list<T, K, E, G, R>(each: E, key: G, row: R) -> View
where
    T: 'static,
    K: Eq + Hash + Clone + 'static,
    E: Fn(&reactive_core::Cx<'_>) -> Vec<T> + 'static,
    G: Fn(&T) -> K + 'static,
    R: for<'a> Fn(&reactive_core::Cx<'a>, &T) -> View + 'static,
{
    view(move |cx, tree| {
        let id = tree.create_element("List");
        let state: Rc<RefCell<HashMap<K, RowEntry>>> = Rc::new(RefCell::new(HashMap::new()));
        let tree_for_effect = tree.clone();
        let state_for_effect = Rc::clone(&state);
        let binding = render_effect(
            cx,
            move |cx| each(cx),
            move |cx, items| {
                reconcile(
                    cx,
                    &tree_for_effect,
                    id,
                    &state_for_effect,
                    &key,
                    &row,
                    items,
                );
            },
        );
        tree.add_binding(id, binding);
        id
    })
}

struct RowEntry {
    view: ViewNodeId,
    owner: OwnerHandle,
}

fn reconcile<T, K, G, R>(
    cx: &reactive_core::Cx<'_>,
    tree: &ViewTree,
    list: ViewNodeId,
    state: &Rc<RefCell<HashMap<K, RowEntry>>>,
    key: &G,
    row: &R,
    items: &[T],
) where
    K: Eq + Hash + Clone,
    G: Fn(&T) -> K,
    R: for<'a> Fn(&reactive_core::Cx<'a>, &T) -> View,
{
    let old = std::mem::take(&mut *state.borrow_mut());
    let mut next = HashMap::with_capacity(items.len());

    for item in items {
        let item_key = key(item);
        if let Some(entry) = old.get(&item_key) {
            next.insert(
                item_key,
                RowEntry {
                    view: entry.view,
                    owner: entry.owner,
                },
            );
            continue;
        }

        if next.contains_key(&item_key) {
            // Duplicate key: the earlier row wins.
            continue;
        }
        let owner = cx.create_owner();
        let view = cx.with_owner(owner, |cx| row(cx, item).mount(cx, tree));
        tree.set_owner(view, owner);
        tree.insert_child(list, view, None);
        next.insert(item_key, RowEntry { view, owner });
    }

    let kept: HashSet<ViewNodeId> = next.values().map(|entry| entry.view).collect();
    for (_, entry) in old {
        if !kept.contains(&entry.view) {
            tree.remove_view(cx, entry.view);
        }
    }

    for item in items {
        let item_key = key(item);
        if let Some(entry) = next.get(&item_key) {
            tree.insert_child(list, entry.view, None);
        }
    }

    *state.borrow_mut() = next;
}

fn caret_rect_for_layout(layout: &ShapedLayout, rect: scene::Rect, caret: usize) -> scene::Rect {
    let mut line_index = layout.lines.len().saturating_sub(1);
    for (index, line) in layout.lines.iter().enumerate() {
        if caret <= line.byte_range.end {
            line_index = index;
            break;
        }
    }
    let Some(line) = layout.lines.get(line_index) else {
        return scene::Rect::new(rect.x, rect.y, 1.0, rect.height.max(1.0));
    };
    let mut x = line.glyphs.first().map_or(0.0, |glyph| glyph.x);
    for glyph in &line.glyphs {
        if caret >= glyph.cluster + glyph.ch.len_utf8() {
            x = glyph.x + glyph.advance;
        }
    }
    let top = rect.y + line.baseline - layout.ascent;
    scene::Rect::new(rect.x + x, top, 1.0, layout.line_height.max(1.0))
}

struct ViewPaintTree<'a> {
    tree: &'a ViewTree,
}

impl PaintTree for ViewPaintTree<'_> {
    type Node = ViewNodeId;

    fn parent(&self, node: Self::Node) -> Option<Self::Node> {
        self.tree
            .inner
            .nodes
            .borrow()
            .get(node)
            .and_then(|node| node.parent)
    }

    fn children(&self, node: Self::Node) -> Vec<Self::Node> {
        self.tree.children(node)
    }

    fn bounds(&self, node: Self::Node) -> scene::Rect {
        let Some(rect) = self.tree.layout_rect(node) else {
            return scene::Rect::default();
        };
        let rect = scene::Rect::new(rect.x, rect.y, rect.width, rect.height);
        rect.transformed(self.tree.world_transform(node))
    }

    fn paint(&self, node: Self::Node, out: &mut Vec<Primitive>) {
        let Some((renderer_id, style, view_text, rect)) = ({
            let nodes = self.tree.inner.nodes.borrow();
            nodes.get(node).map(|view| {
                (
                    view.renderer_id,
                    view.style,
                    view.text.clone(),
                    self.tree.layout_rect(node).unwrap_or_default(),
                )
            })
        }) else {
            return;
        };
        let text_input = self.tree.text_input(node);
        let (text, caret, composing) = match &text_input {
            Some(input) => {
                let input = input.borrow();
                let base = input.text().to_string();
                match input.composition() {
                    Some(composition) => {
                        let caret_byte = input
                            .selection()
                            .map_or(base.len(), |selection| selection.end)
                            .min(base.len());
                        let mut text = base;
                        text.insert_str(caret_byte, &composition.text);
                        let caret = caret_byte
                            + composition
                                .cursor
                                .as_ref()
                                .map_or(composition.text.len(), |cursor| cursor.end);
                        (Some(text), Some(caret), true)
                    }
                    None => (
                        Some(base),
                        input.selection().map(|selection| selection.end),
                        false,
                    ),
                }
            }
            None => (view_text, None, false),
        };
        let mut style = style;
        let state = self.tree.state_style(node);
        if let Some(background) = state.background {
            style.background = Some(background);
        }
        if let Some(border_color) = state.border_color {
            style.border_color = Some(border_color);
        }
        if let Some(text_color) = state.text_color {
            style.text.color = text_color;
        }
        let layout_rect = scene::Rect::new(rect.x, rect.y, rect.width, rect.height);
        let transform = self.tree.world_transform(node);
        let paint_rect = layout_rect.transformed(transform);
        let clip = self
            .tree
            .content_mask(node)
            .map(|mask| scene::Rect::new(mask.x, mask.y, mask.width, mask.height));
        if let Some(color) = style.background {
            out.push(Primitive::Fill {
                node: renderer_id,
                rect: paint_rect,
                color,
                radius: style.radius,
                clip,
            });
        }
        if style.border != Edges::default() {
            if let Some(color) = style.border_color {
                out.push(Primitive::Border {
                    node: renderer_id,
                    rect: paint_rect,
                    width: style.border,
                    color,
                    radius: style.radius,
                    clip,
                });
            }
        }
        if let Some(text) = text {
            if !text.is_empty() || text_input.is_some() {
                // Layout geometry is pixel-snapped; give shaping a half-pixel
                // of slack so a line at the exact snapped width does not wrap.
                let layout = self.tree.inner.text.borrow_mut().layout(
                    &text,
                    style.text,
                    Some(rect.width.max(1.0) + 0.5),
                );
                let caret_rect =
                    caret.map(|caret| caret_rect_for_layout(&layout, layout_rect, caret));
                out.push(Primitive::Text {
                    node: renderer_id,
                    rect: layout_rect,
                    layout,
                    color: style.text.color,
                    clip,
                    transform,
                    underline: composing,
                });
                if let (Some(input), Some(caret_rect)) = (&text_input, caret_rect) {
                    if self.tree.is_focused(node) {
                        let caret_rect = caret_rect.transformed(transform);
                        input.borrow_mut().set_caret_rect(Rect {
                            x: caret_rect.x,
                            y: caret_rect.y,
                            width: caret_rect.width,
                            height: caret_rect.height,
                        });
                        out.push(Primitive::Caret {
                            node: renderer_id,
                            rect: caret_rect,
                            color: style.text.color,
                            clip,
                        });
                    }
                }
            }
        }
    }
}

struct CachedTextMeasure {
    text: Rc<RefCell<TextSystem>>,
}

impl layout::TextMeasure for CachedTextMeasure {
    fn measure(&mut self, request: &TextMeasureRequest) -> TextMetrics {
        self.text.borrow_mut().measure(request)
    }
}

/// Read a signal as a row value.
pub fn row_signal<T: Clone + 'static>(
    signal: ReadSignal<T>,
) -> impl Fn(&reactive_core::Cx<'_>) -> T + 'static {
    move |cx| signal.get(cx)
}
