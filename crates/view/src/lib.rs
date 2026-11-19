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

use reactive_core::{
    render_effect, EffectHandle, NodeId as ViewNodeId, OwnerHandle, ReadSignal, RuntimeHandle,
};
use renderer_api::{
    ElementTag, EventKind, FrameInfo, ListenerId, NodeId as RendererNodeId, Renderer,
};

/// A mounted view node.
#[derive(Debug)]
struct ViewNode {
    renderer_id: RendererNodeId,
    parent: Option<ViewNodeId>,
    children: Vec<ViewNodeId>,
    bindings: Vec<EffectHandle>,
    listeners: Vec<ListenerId>,
    owner: Option<OwnerHandle>,
    dirty: Cell<bool>,
}

#[derive(Clone)]
struct ViewTreeInner {
    renderer: Rc<RefCell<Box<dyn Renderer>>>,
    nodes: Rc<RefCell<reactive_core::Arena<ViewNode>>>,
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
        Self {
            inner: Rc::new(ViewTreeInner {
                renderer: Rc::new(RefCell::new(renderer)),
                nodes: Rc::new(RefCell::new(reactive_core::Arena::new())),
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

    /// Mount a view and return its node.
    pub fn mount(&self, cx: &reactive_core::Cx<'_>, view: View) -> ViewNodeId {
        view.mount(cx, self)
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
        self.alloc_node(renderer_id)
    }

    fn create_text(&self, text: &str) -> ViewNodeId {
        let renderer_id = self
            .inner
            .renderer
            .borrow_mut()
            .create_text(text)
            .expect("create text");
        self.alloc_node(renderer_id)
    }

    fn create_fragment(&self) -> ViewNodeId {
        let renderer_id = self
            .inner
            .renderer
            .borrow_mut()
            .create_fragment()
            .expect("create fragment");
        self.alloc_node(renderer_id)
    }

    fn alloc_node(&self, renderer_id: RendererNodeId) -> ViewNodeId {
        self.inner
            .nodes
            .borrow_mut()
            .insert(ViewNode {
                renderer_id,
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
        self.mark_dirty(parent);
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
            nodes
                .get(id)
                .map(|node| (node.children.clone(), node.renderer_id, node.parent))
        }) else {
            return;
        };
        let (children, renderer_id, parent) = node_data;
        if let Some(parent) = parent {
            self.mark_dirty(parent);
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
            }
        }

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
        let binding = render_effect(
            cx,
            move |cx| source.get(cx),
            move |_cx, value| {
                renderer
                    .borrow_mut()
                    .set_text(renderer_id, value)
                    .expect("set text");
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

/// Read a signal as a row value.
pub fn row_signal<T: Clone + 'static>(
    signal: ReadSignal<T>,
) -> impl Fn(&reactive_core::Cx<'_>) -> T + 'static {
    move |cx| signal.get(cx)
}
