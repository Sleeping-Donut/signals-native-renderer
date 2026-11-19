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
    Display, Edges, ElementTag, EventKind, FlexDirection, FrameInfo, ListenerId,
    NodeId as RendererNodeId, Renderer, ResolvedStyle, Size, TextMeasureRequest, TextMetrics,
    TextStyle,
};
use scene::{PaintTree, Primitive, Scene, SceneBuilder, SceneStats};
use text::{MonospaceShaper, TextSystem};

/// A mounted view node.
#[derive(Debug)]
struct ViewNode {
    renderer_id: RendererNodeId,
    layout_id: LayoutId,
    style: ResolvedStyle,
    text: Option<String>,
    parent: Option<ViewNodeId>,
    children: Vec<ViewNodeId>,
    bindings: Vec<EffectHandle>,
    listeners: Vec<ListenerId>,
    owner: Option<OwnerHandle>,
    dirty: Cell<bool>,
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

    /// Set the retained focus identity. The renderer focus system arrives in
    /// Phase 6; the view tree preserves this identity across reorders.
    pub fn set_focus(&self, focus: Option<ViewNodeId>) {
        *self.inner.focused.borrow_mut() = focus;
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
        self.alloc_node(renderer_id, default_style(tag), None)
    }

    fn create_text(&self, text: &str) -> ViewNodeId {
        let renderer_id = self
            .inner
            .renderer
            .borrow_mut()
            .create_text(text)
            .expect("create text");
        let style = default_text_style();
        self.alloc_node(
            renderer_id,
            style,
            Some(TextContent {
                text: text.to_string(),
                style: style.text,
            }),
        )
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
                listeners: Vec::new(),
                owner: None,
                dirty: Cell::new(false),
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

    fn add_listener(&self, view: ViewNodeId, listener: ListenerId) {
        if let Some(node) = self.inner.nodes.borrow_mut().get_mut(view) {
            node.listeners.push(listener);
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
            self.set_focus(None);
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
        let node = self.inner.nodes.borrow_mut().remove(id);
        if let Some(node) = node {
            for listener in node.listeners {
                let parent_renderer = node
                    .parent
                    .and_then(|parent| {
                        self.inner
                            .nodes
                            .borrow()
                            .get(parent)
                            .map(|parent| parent.renderer_id)
                    })
                    .unwrap_or(node.renderer_id);
                let _ = self
                    .inner
                    .renderer
                    .borrow_mut()
                    .unlisten(parent_renderer, listener);
            }
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
        let renderer_id = tree.renderer_id(id).expect("button");
        let runtime = tree.runtime().clone();
        let listener = tree
            .inner
            .renderer
            .borrow_mut()
            .listen(
                renderer_id,
                EventKind::Click,
                Box::new(move |_event| {
                    let cx = runtime.cx();
                    on_click(&cx);
                }),
            )
            .expect("listen");
        tree.add_listener(id, listener);
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
