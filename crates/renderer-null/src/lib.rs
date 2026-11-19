#![forbid(unsafe_code)]
//! In-process recording renderer.
//!
//! The null renderer maintains a shadow tree and a deterministic operation log.
//! Tests assert structure, properties, events, and frame hooks against it. It is
//! also the conformance target for every backend.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use renderer_api::{
    AttrName, AttrValue, ElementTag, Event, EventKind, FocusCapable, FrameInfo, ListenerId, NodeId,
    Renderer, RendererError, ResolvedStyle,
};

/// A recorded renderer operation.
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    /// Element created.
    CreateElement { id: NodeId, tag: String },
    /// Text created.
    CreateText { id: NodeId, text: String },
    /// Fragment created.
    CreateFragment { id: NodeId },
    /// Child inserted.
    Insert {
        parent: NodeId,
        child: NodeId,
        anchor: Option<NodeId>,
    },
    /// Child removed.
    Remove { parent: NodeId, child: NodeId },
    /// Text changed.
    SetText { id: NodeId, text: String },
    /// Attribute changed.
    SetAttr {
        id: NodeId,
        name: String,
        value: String,
    },
    /// Style changed.
    SetStyle { id: NodeId, style: ResolvedStyle },
    /// Listener added.
    Listen {
        id: NodeId,
        event: EventKind,
        listener: ListenerId,
    },
    /// Listener removed.
    Unlisten { id: NodeId, listener: ListenerId },
    /// Frame began.
    BeginFrame(FrameInfo),
    /// Frame ended.
    EndFrame,
}

#[derive(Debug)]
struct Node {
    tag: Option<String>,
    text: Option<String>,
    attrs: HashMap<String, String>,
    style: Option<ResolvedStyle>,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
    fragment: bool,
}

struct Listener {
    node: NodeId,
    event: EventKind,
    handler: Box<dyn FnMut(&Event)>,
}

/// A recording renderer with a shadow tree.
pub struct NullRenderer {
    nodes: HashMap<NodeId, Node>,
    listeners: HashMap<ListenerId, Listener>,
    ops: Vec<Op>,
    frames: Vec<FrameInfo>,
    next_node: NodeId,
    next_listener: ListenerId,
    focused: Option<NodeId>,
    shutdown: bool,
}

impl Default for NullRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl NullRenderer {
    /// Create an empty renderer.
    #[must_use]
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            listeners: HashMap::new(),
            ops: Vec::new(),
            frames: Vec::new(),
            next_node: 1,
            next_listener: 1,
            focused: None,
            shutdown: false,
        }
    }

    /// Recorded operations.
    #[must_use]
    pub fn ops(&self) -> &[Op] {
        &self.ops
    }

    /// Take and clear recorded operations.
    pub fn take_ops(&mut self) -> Vec<Op> {
        std::mem::take(&mut self.ops)
    }

    /// Clear recorded operations.
    pub fn clear_ops(&mut self) {
        self.ops.clear();
    }

    /// Text content of a node.
    #[must_use]
    pub fn text_of(&self, id: NodeId) -> Option<&str> {
        self.nodes.get(&id).and_then(|node| node.text.as_deref())
    }

    /// Element tag, if the node is an element.
    #[must_use]
    pub fn tag_of(&self, id: NodeId) -> Option<&str> {
        self.nodes.get(&id).and_then(|node| node.tag.as_deref())
    }

    /// Whether the node is a fragment.
    #[must_use]
    pub fn is_fragment(&self, id: NodeId) -> bool {
        self.nodes.get(&id).is_some_and(|node| node.fragment)
    }

    /// Children of a node.
    #[must_use]
    pub fn children_of(&self, id: NodeId) -> Option<&[NodeId]> {
        self.nodes.get(&id).map(|node| node.children.as_slice())
    }

    /// Attribute value.
    #[must_use]
    pub fn attr_of(&self, id: NodeId, name: &str) -> Option<&str> {
        self.nodes
            .get(&id)
            .and_then(|node| node.attrs.get(name))
            .map(String::as_str)
    }

    /// Style of a node.
    #[must_use]
    pub fn style_of(&self, id: NodeId) -> Option<&ResolvedStyle> {
        self.nodes.get(&id).and_then(|node| node.style.as_ref())
    }

    /// Begin/end frame records.
    #[must_use]
    pub fn frames(&self) -> &[FrameInfo] {
        &self.frames
    }

    /// Listener count.
    #[must_use]
    pub fn listener_count(&self) -> usize {
        self.listeners.len()
    }

    /// Clear recorded frame metadata without dropping its capacity.
    pub fn clear_frames(&mut self) {
        self.frames.clear();
    }

    /// Stop accepting new work.
    pub fn shutdown(&mut self) {
        self.shutdown = true;
    }

    /// Dispatch an event to a node's listeners. Returns the number called.
    pub fn dispatch(&mut self, node: NodeId, event: Event) -> usize {
        let ids: Vec<ListenerId> = self
            .listeners
            .iter()
            .filter(|(_, listener)| listener.node == node && listener.event == event.kind)
            .map(|(id, _)| *id)
            .collect();
        for id in &ids {
            if let Some(listener) = self.listeners.get_mut(id) {
                (listener.handler)(&event);
            }
        }
        ids.len()
    }

    /// Dispatch a click at the origin of a node.
    pub fn click(&mut self, node: NodeId) -> usize {
        self.dispatch(node, Event::new(EventKind::Click, 0.0, 0.0))
    }

    fn next_node(&mut self) -> NodeId {
        let id = self.next_node;
        self.next_node += 1;
        id
    }

    fn node(&self, id: NodeId) -> Result<&Node, RendererError> {
        if self.shutdown {
            return Err(RendererError::ShutDown);
        }
        self.nodes.get(&id).ok_or(RendererError::InvalidNode(id))
    }

    fn node_mut(&mut self, id: NodeId) -> Result<&mut Node, RendererError> {
        if self.shutdown {
            return Err(RendererError::ShutDown);
        }
        self.nodes
            .get_mut(&id)
            .ok_or(RendererError::InvalidNode(id))
    }

    fn detach(&mut self, child: NodeId) {
        if let Some(parent_id) = self.nodes.get(&child).and_then(|node| node.parent) {
            if let Some(parent) = self.nodes.get_mut(&parent_id) {
                parent.children.retain(|existing| *existing != child);
            }
            if let Some(node) = self.nodes.get_mut(&child) {
                node.parent = None;
            }
        }
    }

    fn is_ancestor(&self, ancestor: NodeId, mut node: NodeId) -> bool {
        loop {
            if node == ancestor {
                return true;
            }
            match self.nodes.get(&node).and_then(|node| node.parent) {
                Some(parent) => node = parent,
                None => return false,
            }
        }
    }
}

impl Renderer for NullRenderer {
    fn create_element(&mut self, tag: ElementTag<'_>) -> Result<NodeId, RendererError> {
        if self.shutdown {
            return Err(RendererError::ShutDown);
        }
        let id = self.next_node();
        let tag = tag.as_str().to_string();
        self.nodes.insert(
            id,
            Node {
                tag: Some(tag.clone()),
                text: None,
                attrs: HashMap::new(),
                style: None,
                parent: None,
                children: Vec::new(),
                fragment: false,
            },
        );
        self.ops.push(Op::CreateElement { id, tag });
        Ok(id)
    }

    fn create_text(&mut self, text: &str) -> Result<NodeId, RendererError> {
        if self.shutdown {
            return Err(RendererError::ShutDown);
        }
        let id = self.next_node();
        self.nodes.insert(
            id,
            Node {
                tag: None,
                text: Some(text.to_string()),
                attrs: HashMap::new(),
                style: None,
                parent: None,
                children: Vec::new(),
                fragment: false,
            },
        );
        self.ops.push(Op::CreateText {
            id,
            text: text.to_string(),
        });
        Ok(id)
    }

    fn create_fragment(&mut self) -> Result<NodeId, RendererError> {
        if self.shutdown {
            return Err(RendererError::ShutDown);
        }
        let id = self.next_node();
        self.nodes.insert(
            id,
            Node {
                tag: None,
                text: None,
                attrs: HashMap::new(),
                style: None,
                parent: None,
                children: Vec::new(),
                fragment: true,
            },
        );
        self.ops.push(Op::CreateFragment { id });
        Ok(id)
    }

    fn insert(
        &mut self,
        parent: NodeId,
        child: NodeId,
        anchor: Option<NodeId>,
    ) -> Result<(), RendererError> {
        self.node(parent)?;
        self.node(child)?;
        if parent == child {
            return Err(RendererError::NotChild);
        }
        if self.is_ancestor(child, parent) {
            return Err(RendererError::NotChild);
        }
        if let Some(anchor) = anchor {
            if !self
                .nodes
                .get(&parent)
                .is_some_and(|node| node.children.contains(&anchor))
            {
                return Err(RendererError::NotChild);
            }
        }

        self.detach(child);
        let parent_node = self
            .nodes
            .get_mut(&parent)
            .ok_or(RendererError::InvalidNode(parent))?;
        let index = match anchor {
            Some(anchor) => parent_node
                .children
                .iter()
                .position(|existing| *existing == anchor)
                .ok_or(RendererError::NotChild)?,
            None => parent_node.children.len(),
        };
        parent_node.children.insert(index, child);
        if let Some(child_node) = self.nodes.get_mut(&child) {
            child_node.parent = Some(parent);
        }
        self.ops.push(Op::Insert {
            parent,
            child,
            anchor,
        });
        Ok(())
    }

    fn remove(&mut self, parent: NodeId, child: NodeId) -> Result<(), RendererError> {
        self.node(parent)?;
        self.node(child)?;
        if self
            .focused
            .is_some_and(|focus| focus == child || self.is_ancestor(child, focus))
        {
            self.focused = None;
        }
        let parent_node = self
            .nodes
            .get_mut(&parent)
            .ok_or(RendererError::InvalidNode(parent))?;
        let index = parent_node
            .children
            .iter()
            .position(|existing| *existing == child)
            .ok_or(RendererError::NotChild)?;
        parent_node.children.remove(index);
        if let Some(child_node) = self.nodes.get_mut(&child) {
            child_node.parent = None;
        }
        self.ops.push(Op::Remove { parent, child });
        Ok(())
    }

    fn set_text(&mut self, node: NodeId, text: &str) -> Result<(), RendererError> {
        let node_ref = self.node_mut(node)?;
        node_ref.text = Some(text.to_string());
        self.ops.push(Op::SetText {
            id: node,
            text: text.to_string(),
        });
        Ok(())
    }

    fn set_attr(
        &mut self,
        node: NodeId,
        name: AttrName<'_>,
        value: AttrValue<'_>,
    ) -> Result<(), RendererError> {
        let name = name.as_str().to_string();
        let value = value.as_str().to_string();
        self.node_mut(node)?
            .attrs
            .insert(name.clone(), value.clone());
        self.ops.push(Op::SetAttr {
            id: node,
            name,
            value,
        });
        Ok(())
    }

    fn set_style(&mut self, node: NodeId, style: &ResolvedStyle) -> Result<(), RendererError> {
        self.node_mut(node)?.style = Some(*style);
        self.ops.push(Op::SetStyle {
            id: node,
            style: *style,
        });
        Ok(())
    }

    fn listen(
        &mut self,
        node: NodeId,
        event: EventKind,
        handler: Box<dyn FnMut(&Event)>,
    ) -> Result<ListenerId, RendererError> {
        self.node(node)?;
        let id = self.next_listener;
        self.next_listener += 1;
        self.listeners.insert(
            id,
            Listener {
                node,
                event,
                handler,
            },
        );
        self.ops.push(Op::Listen {
            id: node,
            event,
            listener: id,
        });
        Ok(id)
    }

    fn unlisten(&mut self, node: NodeId, listener: ListenerId) -> Result<(), RendererError> {
        self.node(node)?;
        match self.listeners.remove(&listener) {
            Some(existing) if existing.node == node => {
                self.ops.push(Op::Unlisten { id: node, listener });
                Ok(())
            }
            Some(existing) => {
                self.listeners.insert(listener, existing);
                Err(RendererError::NotChild)
            }
            None => Err(RendererError::NotChild),
        }
    }

    fn begin_frame(&mut self, info: &FrameInfo) -> Result<(), RendererError> {
        self.frames.push(*info);
        self.ops.push(Op::BeginFrame(*info));
        Ok(())
    }

    fn end_frame(&mut self) -> Result<(), RendererError> {
        self.ops.push(Op::EndFrame);
        Ok(())
    }

    fn focus_capable(&mut self) -> Option<&mut dyn FocusCapable> {
        Some(self)
    }
}

impl FocusCapable for NullRenderer {
    fn set_focus(&mut self, node: Option<NodeId>) -> Result<(), RendererError> {
        if let Some(node) = node {
            self.node(node)?;
        }
        self.focused = node;
        Ok(())
    }

    fn focused(&self) -> Option<NodeId> {
        self.focused
    }
}

/// A renderer proxy that shares its recording state with the test.
///
/// A [`ViewTree`](../view/index.html) owns a `Box<dyn Renderer>`; this wrapper
/// lets a test keep the `Rc<RefCell<NullRenderer>>` for assertions.
#[derive(Clone)]
pub struct SharedNullRenderer(pub Rc<RefCell<NullRenderer>>);

impl SharedNullRenderer {
    /// Wrap a shared null renderer.
    #[must_use]
    pub fn new(renderer: Rc<RefCell<NullRenderer>>) -> Self {
        Self(renderer)
    }
}

impl Renderer for SharedNullRenderer {
    fn create_element(&mut self, tag: ElementTag<'_>) -> Result<NodeId, RendererError> {
        self.0.borrow_mut().create_element(tag)
    }
    fn create_text(&mut self, text: &str) -> Result<NodeId, RendererError> {
        self.0.borrow_mut().create_text(text)
    }
    fn create_fragment(&mut self) -> Result<NodeId, RendererError> {
        self.0.borrow_mut().create_fragment()
    }
    fn insert(
        &mut self,
        parent: NodeId,
        child: NodeId,
        anchor: Option<NodeId>,
    ) -> Result<(), RendererError> {
        self.0.borrow_mut().insert(parent, child, anchor)
    }
    fn remove(&mut self, parent: NodeId, child: NodeId) -> Result<(), RendererError> {
        self.0.borrow_mut().remove(parent, child)
    }
    fn set_text(&mut self, node: NodeId, text: &str) -> Result<(), RendererError> {
        self.0.borrow_mut().set_text(node, text)
    }
    fn set_attr(
        &mut self,
        node: NodeId,
        name: AttrName<'_>,
        value: AttrValue<'_>,
    ) -> Result<(), RendererError> {
        self.0.borrow_mut().set_attr(node, name, value)
    }
    fn set_style(&mut self, node: NodeId, style: &ResolvedStyle) -> Result<(), RendererError> {
        self.0.borrow_mut().set_style(node, style)
    }
    fn listen(
        &mut self,
        node: NodeId,
        event: EventKind,
        handler: Box<dyn FnMut(&Event)>,
    ) -> Result<ListenerId, RendererError> {
        self.0.borrow_mut().listen(node, event, handler)
    }
    fn unlisten(&mut self, node: NodeId, listener: ListenerId) -> Result<(), RendererError> {
        self.0.borrow_mut().unlisten(node, listener)
    }
    fn begin_frame(&mut self, info: &FrameInfo) -> Result<(), RendererError> {
        self.0.borrow_mut().begin_frame(info)
    }
    fn end_frame(&mut self) -> Result<(), RendererError> {
        self.0.borrow_mut().end_frame()
    }

    fn focus_capable(&mut self) -> Option<&mut dyn FocusCapable> {
        Some(self)
    }
}

impl FocusCapable for SharedNullRenderer {
    fn set_focus(&mut self, node: Option<NodeId>) -> Result<(), RendererError> {
        self.0.borrow_mut().set_focus(node)
    }

    fn focused(&self) -> Option<NodeId> {
        self.0.borrow().focused()
    }
}
