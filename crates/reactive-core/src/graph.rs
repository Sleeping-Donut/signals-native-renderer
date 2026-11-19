//! Phase 1 reactive graph: signals, memos, effects, owners, and the flush loop.
//!
//! The public surface is split handles (`ReadSignal`/`WriteSignal`), read-only
//! `Memo`, cached `Derived`, and effect handles. The graph is single-threaded
//! and arena-backed; every operation takes a borrowed [`Cx`].
//!
//! The scheduler follows the design docs: writes stage until [`Runtime::flush`],
//! values are equality-checked at promotion, invalidations propagate `Check`
//! marks, and a height-ordered heap runs compute passes before applies.

use std::any::{Any, TypeId};
use std::cell::{Cell, RefCell};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;

use crate::{Arena, NodeId};

type NodeRc = Rc<RefCell<Node>>;
type OwnerRc = Rc<RefCell<Owner>>;
type ComputeFn = Box<dyn for<'a> FnMut(&Cx<'a>, Option<&dyn Any>) -> Box<dyn Any>>;
type ApplyFn = Box<dyn for<'a> FnMut(&Cx<'a>, &dyn Any)>;
type CleanupSlot = Rc<RefCell<Option<Box<dyn FnOnce()>>>>;

/// Dirty state of a computed node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DirtyState {
    /// Up to date.
    Clean,
    /// A dependency may have changed; verify before using.
    Check,
    /// Must recompute without verification.
    Dirty,
}

/// A flush failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlushError {
    /// `flush` was called from inside a compute or apply.
    Reentrant,
    /// The run-to-fixpoint limit was exceeded.
    LoopLimit,
}

impl fmt::Display for FlushError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reentrant => f.write_str("flush is not reentrant"),
            Self::LoopLimit => f.write_str("flush exceeded the run-to-fixpoint limit"),
        }
    }
}

impl std::error::Error for FlushError {}

/// Typed owner identity. Owners live in their own arena.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OwnerId {
    id: NodeId,
}

/// A scoped owner handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OwnerHandle {
    id: OwnerId,
}

impl OwnerHandle {
    /// Dispose this owner and every child, cleanup, and node inside it.
    pub fn dispose(self, cx: &Cx<'_>) {
        cx.rt.dispose_owner(self.id);
    }
}

/// A cleanup registered on an owner. Dropping it runs the cleanup early.
#[must_use = "dropping the handle runs the cleanup"]
pub struct CleanupHandle {
    slot: CleanupSlot,
}

impl Drop for CleanupHandle {
    fn drop(&mut self) {
        if let Some(cleanup) = self.slot.borrow_mut().take() {
            cleanup();
        }
    }
}

/// A signal read half.
pub struct ReadSignal<T> {
    id: NodeId,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Clone for ReadSignal<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for ReadSignal<T> {}

impl<T> ReadSignal<T> {
    fn new(id: NodeId) -> Self {
        Self {
            id,
            _marker: PhantomData,
        }
    }

    /// The graph node ID, for diagnostics and tests.
    #[must_use]
    pub const fn id(self) -> NodeId {
        self.id
    }
}

impl<T: 'static> ReadSignal<T> {
    /// Whether the node has been disposed.
    #[must_use]
    pub fn is_disposed(self, cx: &Cx<'_>) -> bool {
        cx.rt.is_disposed(self.id)
    }
}

impl<T: Clone + 'static> ReadSignal<T> {
    /// A tracked read plus clone.
    pub fn get(self, cx: &Cx<'_>) -> T {
        self.with(cx, T::clone)
    }
}

impl<T: 'static> ReadSignal<T> {
    /// A tracked read that borrows the value for the closure's lifetime.
    pub fn with<R>(self, cx: &Cx<'_>, f: impl FnOnce(&T) -> R) -> R {
        cx.rt.ensure_clean(self.id);
        cx.rt.track(self.id);
        let node = cx.rt.node(self.id).expect("signal was disposed");
        let node = node.borrow();
        let value = node.value.borrow();
        let value = value
            .as_ref()
            .and_then(|value| value.downcast_ref::<T>())
            .expect("signal value type");
        f(value)
    }

    /// Phase 1 is synchronous; a plain or computed value is never pending.
    #[must_use]
    pub fn is_pending(self, _cx: &Cx<'_>) -> bool {
        false
    }
}

/// A signal write half.
pub struct WriteSignal<T> {
    id: NodeId,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Clone for WriteSignal<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for WriteSignal<T> {}

impl<T> WriteSignal<T> {
    fn new(id: NodeId) -> Self {
        Self {
            id,
            _marker: PhantomData,
        }
    }

    /// The graph node ID, for diagnostics and tests.
    #[must_use]
    pub const fn id(self) -> NodeId {
        self.id
    }
}

impl<T: 'static> WriteSignal<T> {
    /// Whether the node has been disposed.
    #[must_use]
    pub fn is_disposed(self, cx: &Cx<'_>) -> bool {
        cx.rt.is_disposed(self.id)
    }

    /// Replace the value. It becomes visible at the next flush.
    pub fn set(self, cx: &Cx<'_>, value: T) {
        cx.rt.stage(self.id, Box::new(value));
    }

    /// Replace the value with one computed from the previous effective value.
    ///
    /// This is the Solid-style updater; it compares at flush.
    pub fn update_with(self, cx: &Cx<'_>, f: impl FnOnce(&T) -> T) {
        let node = cx.rt.node(self.id).expect("signal was disposed");
        let previous = take_effective(&node);
        let next = {
            let value = previous.downcast_ref::<T>().expect("signal value type");
            f(value)
        };
        cx.rt.stage(self.id, Box::new(next));
    }

    /// Mutate in place. This bypasses equality and always notifies at flush.
    pub fn update(self, cx: &Cx<'_>, f: impl FnOnce(&mut T)) {
        let node = cx.rt.node(self.id).expect("signal was disposed");
        let mut value = take_effective(&node);
        {
            let value = value
                .as_mut()
                .downcast_mut::<T>()
                .expect("signal value type");
            f(value);
        }
        cx.rt.stage(self.id, value);
    }
}

/// A read-only, cached derived value.
pub struct Memo<T> {
    id: NodeId,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Clone for Memo<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Memo<T> {}

impl<T> Memo<T> {
    fn new(id: NodeId) -> Self {
        Self {
            id,
            _marker: PhantomData,
        }
    }

    /// The graph node ID, for diagnostics and tests.
    #[must_use]
    pub const fn id(self) -> NodeId {
        self.id
    }
}

impl<T: 'static> Memo<T> {
    /// Whether the node has been disposed.
    #[must_use]
    pub fn is_disposed(self, cx: &Cx<'_>) -> bool {
        cx.rt.is_disposed(self.id)
    }

    /// A tracked read that borrows the cached value.
    pub fn with<R>(self, cx: &Cx<'_>, f: impl FnOnce(&T) -> R) -> R {
        cx.rt.ensure_clean(self.id);
        cx.rt.track(self.id);
        let node = cx.rt.node(self.id).expect("memo was disposed");
        let node = node.borrow();
        let value = node.value.borrow();
        let value = value
            .as_ref()
            .and_then(|value| value.downcast_ref::<T>())
            .expect("memo value type");
        f(value)
    }

    /// Phase 1 is synchronous.
    #[must_use]
    pub fn is_pending(self, _cx: &Cx<'_>) -> bool {
        false
    }
}

impl<T: Clone + 'static> Memo<T> {
    /// A tracked read plus clone.
    pub fn get(self, cx: &Cx<'_>) -> T {
        self.with(cx, T::clone)
    }
}

/// A read-only, uncached derived value. Every read runs the closure.
pub struct Derived<T> {
    compute: Box<dyn Fn(&Cx<'_>) -> T>,
}

impl<T> Derived<T> {
    /// Run the derivation. Reads inside it are tracked by the caller.
    pub fn get(&self, cx: &Cx<'_>) -> T {
        (self.compute)(cx)
    }
}

/// A public effect handle. Dropping it does not dispose the effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EffectHandle {
    id: NodeId,
}

impl EffectHandle {
    /// The graph node ID, for diagnostics and tests.
    #[must_use]
    pub const fn id(self) -> NodeId {
        self.id
    }

    /// Stop the effect. Idempotent.
    pub fn dispose(self, cx: &Cx<'_>) {
        cx.rt.dispose_node(self.id);
    }

    /// Whether the effect has been disposed.
    #[must_use]
    pub fn is_disposed(self, cx: &Cx<'_>) -> bool {
        cx.rt.is_disposed(self.id)
    }
}

/// Borrowed graph context.
#[derive(Clone, Copy)]
pub struct Cx<'a> {
    rt: &'a Runtime,
}

impl<'a> Cx<'a> {
    /// Enter a child owner for the duration of the closure.
    pub fn with_owner<R>(self, owner: OwnerHandle, f: impl FnOnce(&Cx<'a>) -> R) -> R {
        let previous = self.rt.current_owner.replace(owner.id);
        let result = f(&self);
        self.rt.current_owner.set(previous);
        result
    }

    /// Create a child owner of the current owner.
    pub fn create_owner(self) -> OwnerHandle {
        let parent = self.rt.current_owner.get();
        OwnerHandle {
            id: self.rt.alloc_owner(Some(parent)),
        }
    }

    /// The current owner.
    #[must_use]
    pub fn owner(self) -> OwnerHandle {
        OwnerHandle {
            id: self.rt.current_owner.get(),
        }
    }

    /// Provide a typed context value on the current owner.
    pub fn provide_context<T: 'static>(self, value: T) {
        let owner = self.rt.owner(self.rt.current_owner.get());
        owner
            .borrow_mut()
            .context
            .insert(TypeId::of::<T>(), Box::new(value));
    }

    /// Read the nearest context value of type `T`, walking up owners.
    #[must_use]
    pub fn use_context<T: Clone + 'static>(self) -> Option<T> {
        let mut current = Some(self.rt.current_owner.get());
        while let Some(id) = current {
            let owner = self.rt.owner(id);
            let owner = owner.borrow();
            if let Some(value) = owner
                .context
                .get(&TypeId::of::<T>())
                .and_then(|value| value.downcast_ref::<T>())
            {
                return Some(value.clone());
            }
            current = owner.parent;
        }
        None
    }

    /// Register a cleanup on the current owner.
    pub fn on_cleanup<F: FnOnce() + 'static>(self, cleanup: F) -> CleanupHandle {
        let slot: CleanupSlot = Rc::new(RefCell::new(Some(Box::new(cleanup))));
        let owner = self.rt.owner(self.rt.current_owner.get());
        owner.borrow_mut().cleanups.push(Rc::clone(&slot));
        CleanupHandle { slot }
    }

    /// Run a closure with dependency tracking disabled.
    pub fn untrack<R>(self, f: impl FnOnce() -> R) -> R {
        let previous = self.rt.tracking.replace(false);
        let result = f();
        self.rt.tracking.set(previous);
        result
    }

    /// Flush staged writes and run pending computes/applies.
    pub fn flush(self) -> Result<(), FlushError> {
        self.rt.flush()
    }

    /// Run a closure, then flush.
    pub fn flush_with<R>(self, f: impl FnOnce() -> R) -> Result<R, FlushError> {
        let value = f();
        self.rt.flush()?;
        Ok(value)
    }
}

/// The reactive graph runtime.
pub struct Runtime {
    nodes: RefCell<Arena<NodeRc>>,
    owners: RefCell<Arena<OwnerRc>>,
    root: OwnerId,
    current_owner: Cell<OwnerId>,
    observer: Cell<Option<NodeId>>,
    tracking: Cell<bool>,
    revision: Cell<u64>,
    heap: RefCell<BinaryHeap<Reverse<HeapEntry>>>,
    seq: Cell<u64>,
    staged: RefCell<Vec<NodeId>>,
    render_applies: RefCell<Vec<NodeId>>,
    user_applies: RefCell<Vec<NodeId>>,
    in_flush: Cell<bool>,
    dirty_visits: Cell<u64>,
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

impl Runtime {
    /// Create a graph with a root owner.
    #[must_use]
    pub fn new() -> Self {
        let mut owners = Arena::new();
        let root = owners
            .insert(Rc::new(RefCell::new(Owner {
                parent: None,
                children: Vec::new(),
                cleanups: Vec::new(),
                context: HashMap::new(),
                nodes: Vec::new(),
                disposed: false,
            })))
            .id();
        let root = OwnerId { id: root };
        Self {
            nodes: RefCell::new(Arena::new()),
            owners: RefCell::new(owners),
            root,
            current_owner: Cell::new(root),
            observer: Cell::new(None),
            tracking: Cell::new(true),
            revision: Cell::new(0),
            heap: RefCell::new(BinaryHeap::new()),
            seq: Cell::new(0),
            staged: RefCell::new(Vec::new()),
            render_applies: RefCell::new(Vec::new()),
            user_applies: RefCell::new(Vec::new()),
            in_flush: Cell::new(false),
            dirty_visits: Cell::new(0),
        }
    }

    /// Borrow the graph through a context.
    #[must_use]
    pub fn cx(&self) -> Cx<'_> {
        Cx { rt: self }
    }

    /// The root owner.
    #[must_use]
    pub const fn root_owner(&self) -> OwnerHandle {
        OwnerHandle { id: self.root }
    }

    /// Number of nodes newly marked `Check` since the last reset. Diagnostic
    /// counter for invalidation-path tests.
    #[must_use]
    pub fn dirty_visits(&self) -> u64 {
        self.dirty_visits.get()
    }

    /// Reset the invalidation-path counter.
    pub fn reset_dirty_visits(&self) {
        self.dirty_visits.set(0);
    }

    fn node(&self, id: NodeId) -> Option<NodeRc> {
        self.nodes.borrow().get(id).cloned()
    }

    fn owner(&self, id: OwnerId) -> OwnerRc {
        self.owners
            .borrow()
            .get(id.id)
            .cloned()
            .expect("owner was disposed")
    }

    fn is_disposed(&self, id: NodeId) -> bool {
        self.node(id)
            .is_none_or(|node| node.borrow().disposed.get())
    }

    fn alloc_owner(&self, parent: Option<OwnerId>) -> OwnerId {
        let owner = Rc::new(RefCell::new(Owner {
            parent,
            children: Vec::new(),
            cleanups: Vec::new(),
            context: HashMap::new(),
            nodes: Vec::new(),
            disposed: false,
        }));
        let id = OwnerId {
            id: self.owners.borrow_mut().insert(owner).id(),
        };
        if let Some(parent) = parent {
            self.owner(parent).borrow_mut().children.push(id);
        }
        id
    }

    fn alloc_node(
        &self,
        kind: NodeKind,
        compute: Option<ComputeFn>,
        apply: Option<ApplyFn>,
        equals: Equals,
    ) -> NodeId {
        let owner = self.current_owner.get();
        let node = Rc::new(RefCell::new(Node::new(kind, compute, apply, equals)));
        let id = self.nodes.borrow_mut().insert(node).id();
        self.owner(owner).borrow_mut().nodes.push(id);
        id
    }

    fn stage(&self, id: NodeId, value: Box<dyn Any>) {
        let Some(node) = self.node(id) else {
            return;
        };
        let push = {
            let node = node.borrow();
            *node.staged.borrow_mut() = Some(value);
            !node.staged_flag.replace(true)
        };
        if push {
            self.staged.borrow_mut().push(id);
        }
    }

    fn track(&self, dep: NodeId) {
        if !self.tracking.get() {
            return;
        }
        let Some(sub) = self.observer.get() else {
            return;
        };
        if dep == sub {
            return;
        }
        let Some(sub_node) = self.node(sub) else {
            return;
        };
        if sub_node.borrow().deps.borrow().contains(&dep) {
            return;
        }

        let Some(dep_node) = self.node(dep) else {
            return;
        };
        let dep_height = dep_node.borrow().height.get();
        let new_height = dep_height.saturating_add(1);
        let reheap = {
            let sub = sub_node.borrow_mut();
            sub.deps.borrow_mut().push(dep);
            if new_height > sub.height.get() {
                sub.height.set(new_height);
                sub.in_heap.get()
            } else {
                false
            }
        };
        dep_node.borrow_mut().subs.borrow_mut().push(sub);
        if reheap {
            self.reheap(sub);
        }
    }

    fn schedule(&self, id: NodeId) {
        let Some(node) = self.node(id) else {
            return;
        };
        let node = node.borrow();
        if node.disposed.get() || node.in_heap.get() {
            return;
        }
        node.in_heap.set(true);
        let entry = HeapEntry {
            height: node.height.get(),
            seq: self.next_seq(),
            id,
        };
        drop(node);
        self.heap.borrow_mut().push(Reverse(entry));
    }

    fn reheap(&self, id: NodeId) {
        let Some(node) = self.node(id) else {
            return;
        };
        let node = node.borrow();
        if !node.in_heap.get() {
            return;
        }
        let entry = HeapEntry {
            height: node.height.get(),
            seq: self.next_seq(),
            id,
        };
        drop(node);
        self.heap.borrow_mut().push(Reverse(entry));
    }

    fn next_seq(&self) -> u64 {
        let seq = self.seq.get() + 1;
        self.seq.set(seq);
        seq
    }

    fn mark_check(&self, start: NodeId) {
        let mut stack = vec![start];
        while let Some(id) = stack.pop() {
            let Some(node) = self.node(id) else {
                continue;
            };
            let (state, disposed, subs) = {
                let node = node.borrow();
                let state = node.state.get();
                let disposed = node.disposed.get();
                let subs = node.subs.borrow().clone();
                (state, disposed, subs)
            };
            if disposed || state >= DirtyState::Check {
                continue;
            }
            node.borrow_mut().state.set(DirtyState::Check);
            self.dirty_visits.set(self.dirty_visits.get() + 1);
            if node.borrow().kind.has_compute() {
                self.schedule(id);
            }
            stack.extend(subs);
        }
    }

    fn mark_subs(&self, id: NodeId) {
        let Some(node) = self.node(id) else {
            return;
        };
        let subs = node.borrow().subs.borrow().clone();
        for sub in subs {
            self.mark_check(sub);
        }
    }

    fn unlink_all_deps(&self, id: NodeId) {
        let Some(node) = self.node(id) else {
            return;
        };
        let deps = std::mem::take(&mut *node.borrow().deps.borrow_mut());
        for dep in deps {
            if let Some(dep_node) = self.node(dep) {
                dep_node
                    .borrow_mut()
                    .subs
                    .borrow_mut()
                    .retain(|sub| *sub != id);
            }
        }
        let old_height = {
            let node = node.borrow();
            let old = node.height.get();
            node.height.set(0);
            old
        };
        if old_height != 0 && node.borrow().in_heap.get() {
            self.reheap(id);
        }
    }

    fn ensure_clean(&self, id: NodeId) {
        let Some(node) = self.node(id) else {
            return;
        };
        if node.borrow().disposed.get() {
            return;
        }
        let state = node.borrow().state.get();
        match state {
            DirtyState::Clean => {}
            DirtyState::Dirty => self.recompute(id),
            DirtyState::Check => {
                if !node.borrow().computed.get() {
                    self.recompute(id);
                    return;
                }
                let deps = node.borrow().deps.borrow().clone();
                for dep in deps {
                    if !self.is_disposed(dep) {
                        self.ensure_clean(dep);
                    }
                }
                let changed = {
                    let node = node.borrow();
                    let last_compute_at = node.last_compute_at.get();
                    let deps = node.deps.borrow().clone();
                    deps.iter().any(|dep| {
                        self.node(*dep)
                            .is_some_and(|dep| dep.borrow().last_changed.get() > last_compute_at)
                    })
                };
                if changed {
                    self.recompute(id);
                } else {
                    node.borrow_mut().state.set(DirtyState::Clean);
                }
            }
        }
    }

    fn recompute(&self, id: NodeId) {
        let Some(node) = self.node(id) else {
            return;
        };
        {
            let node = node.borrow();
            if node.disposed.get() || node.computing.get() {
                return;
            }
            if node.compute.borrow().is_none() {
                node.state.set(DirtyState::Clean);
                return;
            }
        }

        node.borrow_mut().computing.set(true);
        self.unlink_all_deps(id);
        let (mut compute, old) = {
            let node = node.borrow();
            let compute = node
                .compute
                .borrow_mut()
                .take()
                .expect("computed node has a compute closure");
            let old = node.value.borrow_mut().take();
            (compute, old)
        };

        let previous_observer = self.observer.replace(Some(id));
        let previous_tracking = self.tracking.replace(true);
        let new = compute(&self.cx(), old.as_ref().map(|value| &**value as &dyn Any));
        self.observer.set(previous_observer);
        self.tracking.set(previous_tracking);

        node.borrow_mut().compute.borrow_mut().replace(compute);

        let kind = node.borrow().kind;
        if kind.is_effect() {
            let node = node.borrow_mut();
            node.value.borrow_mut().replace(new);
            node.computing.set(false);
            node.computed.set(true);
            node.state.set(DirtyState::Clean);
            node.last_compute_at.set(self.revision.get());
            drop(node);
            self.enqueue_apply(id);
            return;
        }

        let changed = {
            let node = node.borrow();
            match old.as_ref() {
                None => true,
                Some(old) => !node.equals.eq(&**old, &*new),
            }
        };
        let node = node.borrow_mut();
        node.value.borrow_mut().replace(new);
        node.computing.set(false);
        node.computed.set(true);
        node.state.set(DirtyState::Clean);
        node.last_compute_at.set(self.revision.get());
        if changed {
            node.last_changed.set(self.revision.get());
        }
        drop(node);
        if changed {
            self.mark_subs(id);
        }
    }

    fn enqueue_apply(&self, id: NodeId) {
        let Some(node) = self.node(id) else {
            return;
        };
        if node.borrow().apply_queued.replace(true) {
            return;
        }
        let render = node.borrow().kind.is_render_effect();
        if render {
            self.render_applies.borrow_mut().push(id);
        } else {
            self.user_applies.borrow_mut().push(id);
        }
    }

    fn dequeue_apply(&self, id: NodeId) {
        self.render_applies
            .borrow_mut()
            .retain(|queued| *queued != id);
        self.user_applies
            .borrow_mut()
            .retain(|queued| *queued != id);
        if let Some(node) = self.node(id) {
            node.borrow().apply_queued.set(false);
        }
    }

    fn apply_effect(&self, id: NodeId) {
        let Some(node) = self.node(id) else {
            return;
        };
        if node.borrow().disposed.get() {
            return;
        }
        node.borrow().apply_queued.set(false);
        let (apply, value) = {
            let node = node.borrow();
            let apply = node.apply.borrow_mut().take();
            let value = node.value.borrow_mut().take();
            (apply, value)
        };
        let Some(mut apply) = apply else {
            return;
        };
        let previous_observer = self.observer.replace(None);
        let previous_tracking = self.tracking.replace(false);
        if let Some(value) = value.as_ref() {
            apply(&self.cx(), &**value);
        }
        self.observer.set(previous_observer);
        self.tracking.set(previous_tracking);
        node.borrow_mut().apply.borrow_mut().replace(apply);
        *node.borrow_mut().value.borrow_mut() = value;
    }

    fn run_applies(&self) {
        let render = std::mem::take(&mut *self.render_applies.borrow_mut());
        let user = std::mem::take(&mut *self.user_applies.borrow_mut());
        for id in render {
            self.apply_effect(id);
        }
        for id in user {
            self.apply_effect(id);
        }
    }

    fn run_heap(&self) {
        loop {
            let entry = {
                let mut heap = self.heap.borrow_mut();
                loop {
                    let Some(Reverse(entry)) = heap.pop() else {
                        return;
                    };
                    let Some(node) = self.node(entry.id) else {
                        continue;
                    };
                    let height = node.borrow().height.get();
                    let in_heap = node.borrow().in_heap.get();
                    let disposed = node.borrow().disposed.get();
                    if disposed || !in_heap || entry.height != height {
                        continue;
                    }
                    node.borrow().in_heap.set(false);
                    break entry;
                }
            };
            self.ensure_clean(entry.id);
        }
    }

    fn promote_staged(&self) {
        let staged = std::mem::take(&mut *self.staged.borrow_mut());
        if staged.is_empty() {
            return;
        }
        let revision = self.revision.get() + 1;
        self.revision.set(revision);
        for id in staged {
            let Some(node) = self.node(id) else {
                continue;
            };
            node.borrow().staged_flag.set(false);
            let new = node.borrow_mut().staged.borrow_mut().take();
            let Some(new) = new else {
                continue;
            };
            if node.borrow().disposed.get() {
                continue;
            }
            let changed = {
                let node = node.borrow();
                let value = node.value.borrow();
                match value.as_ref() {
                    None => true,
                    Some(old) => !node.equals.eq(&**old, &*new),
                }
            };
            if changed {
                let node = node.borrow_mut();
                node.value.borrow_mut().replace(new);
                node.computed.set(true);
                node.state.set(DirtyState::Clean);
                node.last_changed.set(revision);
                drop(node);
                self.mark_subs(id);
            }
        }
    }

    fn dispose_node(&self, id: NodeId) {
        let Some(node) = self.node(id) else {
            return;
        };
        if node.borrow().disposed.get() {
            return;
        }
        self.unlink_all_deps(id);
        let subs = node.borrow().subs.borrow().clone();
        for sub in subs {
            if let Some(sub_node) = self.node(sub) {
                sub_node
                    .borrow_mut()
                    .deps
                    .borrow_mut()
                    .retain(|dep| *dep != id);
            }
        }
        node.borrow_mut().disposed.set(true);
        node.borrow_mut().state.set(DirtyState::Clean);
        node.borrow_mut().in_heap.set(false);
        node.borrow_mut().value.borrow_mut().take();
        node.borrow_mut().staged.borrow_mut().take();
        node.borrow_mut().compute.borrow_mut().take();
        node.borrow_mut().apply.borrow_mut().take();
        self.nodes.borrow_mut().remove(id);
    }

    fn dispose_owner(&self, id: OwnerId) {
        let owner = self.owner(id);
        if owner.borrow().disposed {
            return;
        }
        owner.borrow_mut().disposed = true;
        let children = owner.borrow().children.clone();
        for child in children {
            self.dispose_owner(child);
        }
        let cleanups = {
            let mut owner = owner.borrow_mut();
            std::mem::take(&mut owner.cleanups)
        };
        for slot in cleanups.into_iter().rev() {
            if let Some(cleanup) = slot.borrow_mut().take() {
                cleanup();
            }
        }
        let nodes = {
            let mut owner = owner.borrow_mut();
            std::mem::take(&mut owner.nodes)
        };
        for node in nodes {
            self.dispose_node(node);
        }
        let parent = owner.borrow().parent;
        if let Some(parent) = parent {
            self.owner(parent)
                .borrow_mut()
                .children
                .retain(|child| *child != id);
        }
    }

    /// Flush staged writes, run computes, then applies, until stable.
    pub fn flush(&self) -> Result<(), FlushError> {
        if self.in_flush.replace(true) {
            return Err(FlushError::Reentrant);
        }
        let mut rounds = 0;
        loop {
            let has_work = !self.staged.borrow().is_empty()
                || !self.heap.borrow().is_empty()
                || !self.render_applies.borrow().is_empty()
                || !self.user_applies.borrow().is_empty();
            if !has_work {
                break;
            }
            rounds += 1;
            if rounds > 10_000 {
                self.in_flush.set(false);
                return Err(FlushError::LoopLimit);
            }
            self.promote_staged();
            self.run_heap();
            self.run_applies();
        }
        self.in_flush.set(false);
        Ok(())
    }
}

/// Create a plain signal with `PartialEq` equality.
pub fn create_signal<T: PartialEq + 'static>(
    cx: &Cx<'_>,
    value: T,
) -> (ReadSignal<T>, WriteSignal<T>) {
    create_signal_impl(cx, value, Equals::Value(any_eq::<T>))
}

/// Create a plain signal that always notifies.
pub fn create_signal_unchecked<T: 'static>(
    cx: &Cx<'_>,
    value: T,
) -> (ReadSignal<T>, WriteSignal<T>) {
    create_signal_impl(cx, value, Equals::Always)
}

/// Create a plain signal with a custom comparator.
pub fn create_signal_with<T: 'static>(
    cx: &Cx<'_>,
    value: T,
    equals: impl Fn(&T, &T) -> bool + 'static,
) -> (ReadSignal<T>, WriteSignal<T>) {
    create_signal_impl(cx, value, custom_equals(equals))
}

fn create_signal_impl<T: 'static>(
    cx: &Cx<'_>,
    value: T,
    equals: Equals,
) -> (ReadSignal<T>, WriteSignal<T>) {
    let id = cx.rt.alloc_node(NodeKind::Signal, None, None, equals);
    let node = cx.rt.node(id).expect("new signal");
    node.borrow_mut()
        .value
        .borrow_mut()
        .replace(Box::new(value));
    node.borrow().computed.set(true);
    (ReadSignal::new(id), WriteSignal::new(id))
}

/// Create a writable derived signal.
pub fn create_derived_signal<T: PartialEq + 'static>(
    cx: &Cx<'_>,
    mut compute: impl FnMut(&Cx<'_>, Option<&T>) -> T + 'static,
) -> (ReadSignal<T>, WriteSignal<T>) {
    let compute: ComputeFn = Box::new(move |cx, previous| {
        let previous = previous.and_then(|value| value.downcast_ref::<T>());
        Box::new(compute(cx, previous))
    });
    let id = cx.rt.alloc_node(
        NodeKind::DerivedSignal,
        Some(compute),
        None,
        Equals::Value(any_eq::<T>),
    );
    cx.rt
        .node(id)
        .expect("new signal")
        .borrow_mut()
        .state
        .set(DirtyState::Dirty);
    (ReadSignal::new(id), WriteSignal::new(id))
}

/// Create a cached read-only memo.
pub fn create_memo<T: PartialEq + 'static>(
    cx: &Cx<'_>,
    mut compute: impl FnMut(&Cx<'_>, Option<&T>) -> T + 'static,
) -> Memo<T> {
    create_memo_impl(
        cx,
        move |cx, previous| compute(cx, previous),
        Equals::Value(any_eq::<T>),
    )
}

/// Create a memo that always notifies.
pub fn create_memo_unchecked<T: 'static>(
    cx: &Cx<'_>,
    mut compute: impl FnMut(&Cx<'_>, Option<&T>) -> T + 'static,
) -> Memo<T> {
    create_memo_impl(
        cx,
        move |cx, previous| compute(cx, previous),
        Equals::Always,
    )
}

/// Create a memo with a custom comparator.
pub fn create_memo_with<T: 'static>(
    cx: &Cx<'_>,
    mut compute: impl FnMut(&Cx<'_>, Option<&T>) -> T + 'static,
    equals: impl Fn(&T, &T) -> bool + 'static,
) -> Memo<T> {
    create_memo_impl(
        cx,
        move |cx, previous| compute(cx, previous),
        custom_equals(equals),
    )
}

fn create_memo_impl<T: 'static>(
    cx: &Cx<'_>,
    mut compute: impl FnMut(&Cx<'_>, Option<&T>) -> T + 'static,
    equals: Equals,
) -> Memo<T> {
    let compute: ComputeFn = Box::new(move |cx, previous| {
        let previous = previous.and_then(|value| value.downcast_ref::<T>());
        Box::new(compute(cx, previous))
    });
    let id = cx
        .rt
        .alloc_node(NodeKind::Memo, Some(compute), None, equals);
    cx.rt
        .node(id)
        .expect("new memo")
        .borrow_mut()
        .state
        .set(DirtyState::Dirty);
    Memo::new(id)
}

/// Create an uncached read-only derivation.
pub fn create_derived<T: 'static>(
    _cx: &Cx<'_>,
    compute: impl Fn(&Cx<'_>) -> T + 'static,
) -> Derived<T> {
    Derived {
        compute: Box::new(compute),
    }
}

/// Create an effect that computes during the heap and applies after it.
pub fn effect<T: 'static>(
    cx: &Cx<'_>,
    mut compute: impl FnMut(&Cx<'_>) -> T + 'static,
    mut apply: impl FnMut(&Cx<'_>, &T) + 'static,
) -> EffectHandle {
    let compute: ComputeFn = Box::new(move |cx, _previous| Box::new(compute(cx)));
    let apply: ApplyFn = Box::new(move |cx, value| {
        let value = value.downcast_ref::<T>().expect("effect value type");
        apply(cx, value);
    });
    let id = cx.rt.alloc_node(
        NodeKind::Effect { render: false },
        Some(compute),
        Some(apply),
        Equals::Always,
    );
    cx.rt
        .node(id)
        .expect("new effect")
        .borrow_mut()
        .state
        .set(DirtyState::Dirty);
    cx.rt.schedule(id);
    EffectHandle { id }
}

/// Create a render effect. Its first compute and apply run synchronously.
pub fn render_effect<T: 'static>(
    cx: &Cx<'_>,
    mut compute: impl FnMut(&Cx<'_>) -> T + 'static,
    mut apply: impl FnMut(&Cx<'_>, &T) + 'static,
) -> EffectHandle {
    let compute: ComputeFn = Box::new(move |cx, _previous| Box::new(compute(cx)));
    let apply: ApplyFn = Box::new(move |cx, value| {
        let value = value.downcast_ref::<T>().expect("render effect value type");
        apply(cx, value);
    });
    let id = cx.rt.alloc_node(
        NodeKind::Effect { render: true },
        Some(compute),
        Some(apply),
        Equals::Always,
    );
    cx.rt
        .node(id)
        .expect("new render effect")
        .borrow_mut()
        .state
        .set(DirtyState::Dirty);
    cx.rt.recompute(id);
    cx.rt.dequeue_apply(id);
    cx.rt.apply_effect(id);
    EffectHandle { id }
}

fn take_effective(node: &NodeRc) -> Box<dyn Any> {
    let node = node.borrow_mut();
    let staged = node.staged.borrow_mut().take();
    if let Some(value) = staged {
        value
    } else {
        node.value.borrow_mut().take().expect("signal has a value")
    }
}

fn any_eq<T: PartialEq + 'static>(a: &dyn Any, b: &dyn Any) -> bool {
    match (a.downcast_ref::<T>(), b.downcast_ref::<T>()) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

fn custom_equals<T: 'static>(equals: impl Fn(&T, &T) -> bool + 'static) -> Equals {
    Equals::Custom(Box::new(move |a, b| {
        let a = a.downcast_ref::<T>().expect("comparator type");
        let b = b.downcast_ref::<T>().expect("comparator type");
        equals(a, b)
    }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NodeKind {
    Signal,
    DerivedSignal,
    Memo,
    Effect { render: bool },
}

impl NodeKind {
    fn has_compute(self) -> bool {
        !matches!(self, Self::Signal)
    }

    fn is_effect(self) -> bool {
        matches!(self, Self::Effect { .. })
    }

    fn is_render_effect(self) -> bool {
        matches!(self, Self::Effect { render: true })
    }
}

type EqualsFn = fn(&dyn Any, &dyn Any) -> bool;
type CustomEqualsFn = Box<dyn Fn(&dyn Any, &dyn Any) -> bool>;

enum Equals {
    Value(EqualsFn),
    Always,
    Custom(CustomEqualsFn),
}

impl Equals {
    fn eq(&self, a: &dyn Any, b: &dyn Any) -> bool {
        match self {
            Self::Value(eq) => eq(a, b),
            Self::Always => false,
            Self::Custom(eq) => eq(a, b),
        }
    }
}

struct Node {
    kind: NodeKind,
    state: Cell<DirtyState>,
    height: Cell<u32>,
    in_heap: Cell<bool>,
    disposed: Cell<bool>,
    computed: Cell<bool>,
    computing: Cell<bool>,
    value: RefCell<Option<Box<dyn Any>>>,
    staged: RefCell<Option<Box<dyn Any>>>,
    staged_flag: Cell<bool>,
    deps: RefCell<Vec<NodeId>>,
    subs: RefCell<Vec<NodeId>>,
    compute: RefCell<Option<ComputeFn>>,
    apply: RefCell<Option<ApplyFn>>,
    equals: Equals,
    last_changed: Cell<u64>,
    last_compute_at: Cell<u64>,
    apply_queued: Cell<bool>,
}

impl Node {
    fn new(
        kind: NodeKind,
        compute: Option<ComputeFn>,
        apply: Option<ApplyFn>,
        equals: Equals,
    ) -> Self {
        let has_compute = kind.has_compute();
        Self {
            kind,
            state: Cell::new(if has_compute {
                DirtyState::Dirty
            } else {
                DirtyState::Clean
            }),
            height: Cell::new(0),
            in_heap: Cell::new(false),
            disposed: Cell::new(false),
            computed: Cell::new(!has_compute),
            computing: Cell::new(false),
            value: RefCell::new(None),
            staged: RefCell::new(None),
            staged_flag: Cell::new(false),
            deps: RefCell::new(Vec::new()),
            subs: RefCell::new(Vec::new()),
            compute: RefCell::new(compute),
            apply: RefCell::new(apply),
            equals,
            last_changed: Cell::new(0),
            last_compute_at: Cell::new(0),
            apply_queued: Cell::new(false),
        }
    }
}

struct Owner {
    parent: Option<OwnerId>,
    children: Vec<OwnerId>,
    cleanups: Vec<CleanupSlot>,
    context: HashMap<TypeId, Box<dyn Any>>,
    nodes: Vec<NodeId>,
    disposed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct HeapEntry {
    height: u32,
    seq: u64,
    id: NodeId,
}
