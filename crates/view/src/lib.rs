#![forbid(unsafe_code)]
//! Retained view tree for the native renderer.
//!
//! A component mounts once. Dynamic values become bindings (render effects that
//! write one renderer property), keyed lists reconcile by key and move existing
//! rows, and view slots replace one branch when a key changes. Structure is not
//! rebuilt per frame.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::rc::Rc;

use layout::{LayoutEngine, LayoutId, LayoutStats, Rect, TextContent};
use reactive_core::{
    render_effect, EffectHandle, NodeId as ViewNodeId, OwnerHandle, ReadSignal, RuntimeHandle,
};
use renderer_api::{
    A11yRole, Display, Edges, ElementTag, FlexDirection, FrameInfo, Key, KeyEvent, Modifiers,
    NamedKey, NodeId as RendererNodeId, PointerEvent, Renderer, ResolvedStyle, Size,
    TextMeasureRequest, TextMetrics, TextStyle,
};
use scene::{PaintTree, Primitive, Scene, SceneBuilder, SceneStats};
use text::{MonospaceShaper, TextSystem};

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
    interactive: bool,
    focusable: bool,
    tab_index: Option<i32>,
    disabled: bool,
    a11y_role: A11yRole,
    a11y_label: Option<String>,
    a11y_value: Option<String>,
    view_listeners: Vec<ViewListener>,
    key_bindings: Vec<KeyBinding>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ViewEvent {
    Click,
    PointerEnter,
    PointerLeave,
    Focus,
    Blur,
}

type ViewCallback = Rc<RefCell<Box<dyn FnMut(&reactive_core::Cx<'_>)>>>;

struct ViewListener {
    event: ViewEvent,
    callback: ViewCallback,
}

#[derive(Clone)]
struct KeyBinding {
    stroke: KeyStroke,
    callback: ViewCallback,
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
    pressed: RefCell<Option<ViewNodeId>>,
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
                pressed: RefCell::new(None),
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
        scene
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
                if x >= rect.x && x <= rect.right() && y >= rect.y && y <= rect.bottom() {
                    return Some(node);
                }
            }
        }
        None
    }

    /// Update hover state and return the topmost hit.
    pub fn pointer_move(&self, event: &PointerEvent) -> Option<ViewNodeId> {
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
        }
        hit
    }

    /// Pointer press. Focuses the hit node when it is focusable.
    pub fn pointer_down(&self, event: &PointerEvent) -> Option<ViewNodeId> {
        let hit = self.hit_test(event.x, event.y);
        *self.inner.pressed.borrow_mut() = hit;
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

    /// Pointer release. Dispatches a click when press and release share a node.
    pub fn pointer_up(&self, event: &PointerEvent) -> bool {
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
        self.dispatch_view_event(node, ViewEvent::Click)
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
        let cx = self.inner.runtime.cx();
        let mut current = self.focused();
        while let Some(node) = current {
            let bindings = {
                let nodes = self.inner.nodes.borrow();
                nodes.get(node).map(|data| {
                    data.key_bindings
                        .iter()
                        .filter(|binding| {
                            binding.stroke.key == event.key
                                && binding.stroke.modifiers == event.modifiers
                        })
                        .map(|binding| Rc::clone(&binding.callback))
                        .collect::<Vec<_>>()
                })
            };
            if let Some(bindings) = bindings {
                if !bindings.is_empty() {
                    for callback in bindings {
                        (callback.borrow_mut())(&cx);
                    }
                    return true;
                }
            }
            current = self
                .inner
                .nodes
                .borrow()
                .get(node)
                .and_then(|node| node.parent);
        }

        let globals = self.inner.global_keys.borrow().clone();
        for binding in globals {
            if binding.stroke.key == event.key && binding.stroke.modifiers == event.modifiers {
                (binding.callback.borrow_mut())(&cx);
                return true;
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
        false
    }

    /// Register a window-level key binding.
    pub fn bind_global_key(
        &self,
        stroke: KeyStroke,
        callback: impl Fn(&reactive_core::Cx<'_>) + 'static,
    ) {
        self.inner.global_keys.borrow_mut().push(KeyBinding {
            stroke,
            callback: Rc::new(RefCell::new(Box::new(callback))),
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
        let mut current = Some(node);
        let mut handled = false;
        while let Some(node) = current {
            let callbacks = {
                let nodes = self.inner.nodes.borrow();
                nodes.get(node).map(|data| {
                    data.view_listeners
                        .iter()
                        .filter(|listener| listener.event == event)
                        .map(|listener| Rc::clone(&listener.callback))
                        .collect::<Vec<_>>()
                })
            };
            if let Some(callbacks) = callbacks {
                for callback in callbacks {
                    (callback.borrow_mut())(&cx);
                    handled = true;
                }
            }
            current = self
                .inner
                .nodes
                .borrow()
                .get(node)
                .and_then(|data| data.parent);
        }
        handled
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
                interactive: false,
                focusable: false,
                tab_index: None,
                disabled: false,
                a11y_role: A11yRole::Generic,
                a11y_label: None,
                a11y_value: None,
                view_listeners: Vec::new(),
                key_bindings: Vec::new(),
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
        callback: Box<dyn FnMut(&reactive_core::Cx<'_>)>,
    ) {
        if let Some(node) = self.inner.nodes.borrow_mut().get_mut(view) {
            node.interactive = true;
            node.view_listeners.push(ViewListener {
                event,
                callback: Rc::new(RefCell::new(callback)),
            });
        }
    }

    fn add_key_binding(
        &self,
        view: ViewNodeId,
        stroke: KeyStroke,
        callback: Box<dyn FnMut(&reactive_core::Cx<'_>)>,
    ) {
        if let Some(node) = self.inner.nodes.borrow_mut().get_mut(view) {
            node.focusable = true;
            node.key_bindings.push(KeyBinding {
                stroke,
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
        if self
            .inner
            .hovered
            .borrow()
            .is_some_and(|hovered| hovered == id)
        {
            *self.inner.hovered.borrow_mut() = None;
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
        let mount = self.mount;
        view(move |cx, tree| {
            let id = mount(cx, tree);
            tree.add_view_listener(id, ViewEvent::Click, Box::new(move |cx| handler(cx)));
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
        let mount = self.mount;
        view(move |cx, tree| {
            let id = mount(cx, tree);
            tree.add_key_binding(id, stroke.clone(), Box::new(move |cx| handler(cx)));
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
        tree.add_view_listener(id, ViewEvent::Click, Box::new(move |cx| on_click(cx)));
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
        self.tree
            .layout_rect(node)
            .map(|rect| scene::Rect::new(rect.x, rect.y, rect.width, rect.height))
            .unwrap_or_default()
    }

    fn paint(&self, node: Self::Node, out: &mut Vec<Primitive>) {
        let Some((renderer_id, style, text, rect)) = ({
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
        let rect = scene::Rect::new(rect.x, rect.y, rect.width, rect.height);
        if let Some(color) = style.background {
            out.push(Primitive::Fill {
                node: renderer_id,
                rect,
                color,
                radius: style.radius,
            });
        }
        if style.border != Edges::default() {
            if let Some(color) = style.border_color {
                out.push(Primitive::Border {
                    node: renderer_id,
                    rect,
                    width: style.border,
                    color,
                    radius: style.radius,
                });
            }
        }
        if let Some(text) = text {
            if !text.is_empty() {
                // Layout geometry is pixel-snapped; give shaping a half-pixel
                // of slack so a line at the exact snapped width does not wrap.
                let layout = self.tree.inner.text.borrow_mut().layout(
                    &text,
                    style.text,
                    Some(rect.width.max(1.0) + 0.5),
                );
                out.push(Primitive::Text {
                    node: renderer_id,
                    rect,
                    layout,
                    color: style.text.color,
                });
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
