//! Phase 2 reactive core: the sync graph plus async compute, flights, holds,
//! and loading boundaries.
//!
//! The public surface is split handles (`ReadSignal`/`WriteSignal`), read-only
//! `Memo`, cached `Derived`, loading boundaries, and effect handles. The graph
//! is single-threaded and arena-backed; every operation takes a borrowed [`Cx`].
//!
//! The scheduler follows the design docs: writes stage until [`Runtime::flush`],
//! values are equality-checked at promotion, invalidations propagate `Check`
//! marks, and a height-ordered heap runs compute passes before applies. Async
//! futures are polled by [`Runtime::pump`] and committed at the next flush; a
//! `LoadingBoundary` keeps its old value until its content can be computed
//! whole.

use std::any::{Any, TypeId};
use std::cell::{Cell, RefCell};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::ops::Deref;
use std::panic::{catch_unwind, panic_any, resume_unwind, AssertUnwindSafe};
use std::pin::Pin;
use std::rc::Rc;
use std::task::Poll;

use crate::{Arena, ManualSpawner, NodeId, Spawner, VirtualClock};

/// A boxed async error.
pub type AsyncError = Box<dyn Error + Send + Sync>;

type LocalBoxFuture<T> = Pin<Box<dyn Future<Output = T> + 'static>>;
type AsyncFuture = LocalBoxFuture<Result<Box<dyn Any>, AsyncError>>;
type FutureSlot = Rc<RefCell<Option<AsyncFuture>>>;
type CompletionQueue = Rc<RefCell<VecDeque<Completion>>>;

struct Completion {
    node: NodeId,
    flight: u64,
    result: Result<Box<dyn Any>, AsyncError>,
}
type AsyncComputeFn = Box<dyn for<'a> FnMut(&Cx<'a>, Option<&dyn Any>) -> AsyncFuture>;
type NodeRc = Rc<RefCell<Node>>;
type OwnerRc = Rc<RefCell<Owner>>;
type ComputeFn = Box<dyn for<'a> FnMut(&Cx<'a>, Option<&dyn Any>) -> Box<dyn Any>>;
type ApplyFn = Box<dyn for<'a> FnMut(&Cx<'a>, &dyn Any)>;
type CleanupSlot = Rc<RefCell<Option<Box<dyn FnOnce()>>>>;

struct NotReady;

struct AsyncFailure(String);

impl fmt::Display for AsyncFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn is_suspension(payload: &(dyn Any + Send)) -> bool {
    payload.is::<NotReady>() || payload.is::<AsyncFailure>()
}

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

/// How a read treats a node whose new answer is still in flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReaderClass {
    /// Return the committed value; a render effect sees the screen.
    Display,
    /// Join the hold: raise `NotReady` until the new answer is whole.
    Derive,
    /// Observe without joining the hold; used by `is_pending` probes.
    Observe,
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
        cx.rt.track(self.id);
        if cx.rt.pending_probe.get() {
            cx.rt.track_pending(self.id);
        }
        cx.rt.ensure_clean(self.id);
        if cx.rt.reader_class.get() == ReaderClass::Derive && cx.rt.is_pending_node(self.id) {
            cx.rt.raise_pending(self.id);
        }
        cx.rt.note_pending_read(self.id);
        let node = cx.rt.node(self.id).expect("signal was disposed");
        let node = node.borrow();
        if cx.rt.reader_class.get() == ReaderClass::Display {
            if let Some(guess) = node
                .guess
                .borrow()
                .as_ref()
                .and_then(|value| value.downcast_ref::<T>())
            {
                return f(guess);
            }
        }
        let value = node.value.borrow();
        let value = match value.as_ref().and_then(|value| value.downcast_ref::<T>()) {
            Some(value) => value,
            None => {
                drop(value);
                drop(node);
                cx.rt.raise_pending(self.id);
            }
        };
        f(value)
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
        cx.rt.track(self.id);
        if cx.rt.pending_probe.get() {
            cx.rt.track_pending(self.id);
        }
        cx.rt.ensure_clean(self.id);
        if cx.rt.reader_class.get() == ReaderClass::Derive && cx.rt.is_pending_node(self.id) {
            cx.rt.raise_pending(self.id);
        }
        cx.rt.note_pending_read(self.id);
        let node = cx.rt.node(self.id).expect("memo was disposed");
        let node = node.borrow();
        if cx.rt.reader_class.get() == ReaderClass::Display {
            if let Some(guess) = node
                .guess
                .borrow()
                .as_ref()
                .and_then(|value| value.downcast_ref::<T>())
            {
                return f(guess);
            }
        }
        let value = node.value.borrow();
        let value = match value.as_ref().and_then(|value| value.downcast_ref::<T>()) {
            Some(value) => value,
            None => {
                drop(value);
                drop(node);
                cx.rt.raise_pending(self.id);
            }
        };
        f(value)
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

/// A loading boundary. It shows its fallback on first load, keeps old content
/// while a disarmed refetch is pending, and remounts on a key change.
pub struct LoadingBoundary<T> {
    id: NodeId,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Clone for LoadingBoundary<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for LoadingBoundary<T> {}

impl<T> LoadingBoundary<T> {
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

impl<T: 'static> LoadingBoundary<T> {
    /// Whether the boundary or its content has been disposed.
    #[must_use]
    pub fn is_disposed(self, cx: &Cx<'_>) -> bool {
        cx.rt.is_disposed(self.id)
    }

    /// A tracked read of the boundary value.
    pub fn with<R>(self, cx: &Cx<'_>, f: impl FnOnce(&T) -> R) -> R {
        cx.rt.track(self.id);
        if cx.rt.pending_probe.get() {
            cx.rt.track_pending(self.id);
        }
        cx.rt.ensure_clean(self.id);
        cx.rt.note_pending_read(self.id);
        let node = cx.rt.node(self.id).expect("boundary was disposed");
        let node = node.borrow();
        let value = node.value.borrow();
        let value = match value.as_ref().and_then(|value| value.downcast_ref::<T>()) {
            Some(value) => value,
            None => {
                drop(value);
                drop(node);
                cx.rt.raise_pending(self.id);
            }
        };
        f(value)
    }
}

impl<T: Clone + 'static> LoadingBoundary<T> {
    /// A tracked read plus clone.
    pub fn get(self, cx: &Cx<'_>) -> T {
        self.with(cx, T::clone)
    }
}

/// An error boundary. It runs a fallible content closure and renders the
/// fallback with the error value when it returns `Err`.
pub struct ErrorBoundary<T> {
    id: NodeId,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Clone for ErrorBoundary<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for ErrorBoundary<T> {}

impl<T> ErrorBoundary<T> {
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

impl<T: 'static> ErrorBoundary<T> {
    /// Whether the boundary or its content has been disposed.
    #[must_use]
    pub fn is_disposed(self, cx: &Cx<'_>) -> bool {
        cx.rt.is_disposed(self.id)
    }

    /// A tracked read of the fallback or recovered content.
    pub fn with<R>(self, cx: &Cx<'_>, f: impl FnOnce(&T) -> R) -> R {
        cx.rt.track(self.id);
        cx.rt.ensure_clean(self.id);
        let node = cx.rt.node(self.id).expect("error boundary was disposed");
        let node = node.borrow();
        let value = node.value.borrow();
        let value = match value.as_ref().and_then(|value| value.downcast_ref::<T>()) {
            Some(value) => value,
            None => {
                drop(value);
                drop(node);
                cx.rt.raise_pending(self.id);
            }
        };
        f(value)
    }
}

impl<T: Clone + 'static> ErrorBoundary<T> {
    /// A tracked read plus clone.
    pub fn get(self, cx: &Cx<'_>) -> T {
        self.with(cx, T::clone)
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

    /// Whether the effect has computed and applied at least once.
    #[must_use]
    pub fn is_ready(self, cx: &Cx<'_>) -> bool {
        if let Some(token) = cx.rt.effect_tokens.borrow().get(&self.id).copied() {
            cx.rt.track(token);
        }
        cx.rt.effect_ready(self.id)
    }
}

/// An owned graph runtime handle. Cloning is cheap and keeps the runtime
/// alive; use it in `'static` event handlers and detached tasks.
#[derive(Clone)]
pub struct RuntimeHandle {
    rt: Rc<Runtime>,
}

impl Runtime {
    /// Convert the runtime into an owned, clonable handle.
    #[must_use]
    pub fn into_handle(self) -> RuntimeHandle {
        RuntimeHandle { rt: Rc::new(self) }
    }
}

impl RuntimeHandle {
    /// Borrow the graph through a context.
    #[must_use]
    pub fn cx(&self) -> Cx<'_> {
        Cx { rt: &self.rt }
    }

    /// The underlying runtime.
    #[must_use]
    pub fn runtime(&self) -> &Runtime {
        &self.rt
    }
}

impl RuntimeHandle {
    /// Run an optimistic action: guesses show at once, committed writes land in
    /// one flush when the future settles, and guesses clear at settle.
    pub fn action<F, Fut, T, E>(&self, f: F) -> ActionHandle<T>
    where
        F: FnOnce(ActionCx) -> Fut + 'static,
        Fut: std::future::Future<Output = Result<T, E>> + 'static,
        T: Clone + 'static,
        E: ToString + 'static,
    {
        let runtime = self.clone();
        let (result, set_result) = create_signal_unchecked(&self.cx(), None::<Result<T, String>>);
        let action = ActionCx {
            runtime: self.clone(),
            guesses: Rc::new(RefCell::new(Vec::new())),
        };
        let guesses = Rc::clone(&action.guesses);
        self.spawner().spawn_local(Box::pin(async move {
            let outcome = f(action).await;
            let cx = runtime.cx();
            for id in guesses.borrow().iter().copied() {
                runtime.clear_guess_id(id);
            }
            set_result.set(&cx, Some(outcome.map_err(|error| error.to_string())));
        }));
        ActionHandle { result }
    }
}

impl Deref for RuntimeHandle {
    type Target = Runtime;

    fn deref(&self) -> &Self::Target {
        &self.rt
    }
}

impl std::fmt::Debug for RuntimeHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeHandle").finish_non_exhaustive()
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

    /// Whether dependency tracking is currently enabled.
    #[must_use]
    pub fn is_tracking(self) -> bool {
        self.rt.tracking.get()
    }

    /// The current reader class.
    #[must_use]
    pub fn reader_class(self) -> ReaderClass {
        self.rt.reader_class.get()
    }

    /// Run a closure with a specific reader class. Restores on unwind.
    pub fn with_reader_class<R>(self, class: ReaderClass, f: impl FnOnce() -> R) -> R {
        let previous = self.rt.reader_class.replace(class);
        let result = catch_unwind(AssertUnwindSafe(f));
        self.rt.reader_class.set(previous);
        match result {
            Ok(value) => value,
            Err(payload) => resume_unwind(payload),
        }
    }

    /// The runtime's deterministic virtual clock.
    #[must_use]
    pub fn clock(self) -> VirtualClock {
        self.rt.clock.clone()
    }

    /// Advance the virtual clock and request a pump.
    pub fn advance_time(self, millis: u64) {
        self.rt.clock.advance(millis);
        self.rt.spawner.wake_ui();
    }

    /// Flush staged writes and run pending computes/applies.
    pub fn flush(self) -> Result<(), FlushError> {
        self.rt.flush()
    }

    /// Poll in-flight async futures once. Ready results commit at the next flush.
    pub fn pump(self) {
        self.rt.pump();
    }

    /// Number of queued local tasks, when the host can report it.
    #[must_use]
    pub fn pending_tasks(self) -> usize {
        self.rt.spawner.pending()
    }

    /// Number of live graph nodes.
    #[must_use]
    pub fn live_nodes(self) -> usize {
        self.rt.nodes.borrow().len()
    }

    /// Number of live owners.
    #[must_use]
    pub fn live_owners(self) -> usize {
        self.rt.owners.borrow().len()
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
    async_ready: RefCell<Vec<NodeId>>,
    completions: CompletionQueue,
    spawner: Box<dyn Spawner>,
    in_flush: Cell<bool>,
    reader_class: Cell<ReaderClass>,
    clock: VirtualClock,
    dirty_visits: Cell<u64>,
    pending_tokens: RefCell<HashMap<NodeId, NodeId>>,
    affected: RefCell<HashSet<NodeId>>,
    refresh_nodes: RefCell<HashSet<NodeId>>,
    affects_token: Cell<Option<NodeId>>,
    effect_tokens: RefCell<HashMap<NodeId, NodeId>>,
    pending_probe: Cell<bool>,
    pending_probe_found: Cell<bool>,
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
            async_ready: RefCell::new(Vec::new()),
            completions: Rc::new(RefCell::new(VecDeque::new())),
            spawner: Box::new(ManualSpawner::new()),
            in_flush: Cell::new(false),
            reader_class: Cell::new(ReaderClass::Display),
            clock: VirtualClock::new(),
            dirty_visits: Cell::new(0),
            pending_tokens: RefCell::new(HashMap::new()),
            affected: RefCell::new(HashSet::new()),
            refresh_nodes: RefCell::new(HashSet::new()),
            affects_token: Cell::new(None),
            effect_tokens: RefCell::new(HashMap::new()),
            pending_probe: Cell::new(false),
            pending_probe_found: Cell::new(false),
        }
    }

    /// Create a graph whose async flights run on `spawner`.
    pub fn with_spawner(spawner: impl Spawner + 'static) -> Self {
        let mut runtime = Self::new();
        runtime.spawner = Box::new(spawner);
        runtime
    }

    /// The host spawner.
    #[must_use]
    pub fn spawner(&self) -> &dyn Spawner {
        self.spawner.as_ref()
    }

    /// The runtime's deterministic virtual clock.
    #[must_use]
    pub fn clock(&self) -> VirtualClock {
        self.clock.clone()
    }

    /// Advance the virtual clock without pumping. Hosts and tests call
    /// [`pump`](Self::pump) afterwards.
    pub fn advance_time(&self, millis: u64) {
        self.clock.advance(millis);
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

    fn raise_pending(&self, id: NodeId) -> ! {
        let error = self
            .node(id)
            .and_then(|node| node.borrow().async_error.borrow().clone());
        match error {
            Some(error) => panic_any(AsyncFailure(error)),
            None => panic_any(NotReady),
        }
    }

    fn is_pending_node(&self, start: NodeId) -> bool {
        let mut stack = vec![start];
        let mut seen = Vec::new();
        while let Some(id) = stack.pop() {
            if seen.contains(&id) {
                continue;
            }
            seen.push(id);
            let Some(node) = self.node(id) else {
                continue;
            };
            if self.affected.borrow().contains(&id) {
                return true;
            }
            let node = node.borrow();
            if node.guess.borrow().is_some() {
                return true;
            }
            let has_value = node.value.borrow().is_some();
            if node.in_flight.get() && has_value && !node.quiet.get() {
                return true;
            }
            if node.async_result.borrow().is_some() && has_value {
                return true;
            }
            stack.extend(node.deps.borrow().iter().copied());
        }
        false
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

    fn is_pending_probe<T>(&self, read: impl FnOnce(&Cx<'_>) -> T) -> bool {
        self.track(self.affects_token());
        let previous_probe = self.pending_probe.replace(true);
        let previous_found = self.pending_probe_found.replace(false);
        let previous_class = self.reader_class.replace(ReaderClass::Display);
        let result = catch_unwind(AssertUnwindSafe(|| read(&self.cx())));
        self.reader_class.set(previous_class);
        let found = self.pending_probe_found.get();
        self.pending_probe.set(previous_probe);
        self.pending_probe_found.set(previous_found);
        match result {
            Ok(_) => found,
            // A suspended read with no committed value is loading, not a new
            // question. Pending is the verdict that an old value is being
            // replaced by a newer answer.
            Err(payload) if is_suspension(&*payload) => found,
            Err(payload) => resume_unwind(payload),
        }
    }

    fn note_pending_read(&self, id: NodeId) {
        if self.pending_probe.get() && self.is_pending_node(id) {
            self.pending_probe_found.set(true);
        }
    }

    fn track_pending(&self, id: NodeId) {
        if let Some(token) = self.pending_tokens.borrow().get(&id).copied() {
            self.track(token);
        }
    }

    fn register_effect_token(&self, cx: &Cx<'_>, id: NodeId) {
        let (token, _) = create_signal(cx, 0_u64);
        self.effect_tokens.borrow_mut().insert(id, token.id());
    }

    fn effect_ready(&self, id: NodeId) -> bool {
        self.node(id).is_some_and(|node| {
            let node = node.borrow();
            node.computed.get() && node.value.borrow().is_some()
        })
    }

    fn bump_effect(&self, id: NodeId) {
        let Some(token) = self.effect_tokens.borrow().get(&id).copied() else {
            return;
        };
        let Some(node) = self.node(token) else {
            return;
        };
        let next = node
            .borrow()
            .value
            .borrow()
            .as_ref()
            .and_then(|value| value.downcast_ref::<u64>())
            .copied()
            .unwrap_or(0)
            .wrapping_add(1);
        self.stage(token, Box::new(next));
    }

    fn bump_pending(&self, id: NodeId) {
        let Some(token) = self.pending_tokens.borrow().get(&id).copied() else {
            return;
        };
        let Some(node) = self.node(token) else {
            return;
        };
        let next = node
            .borrow()
            .value
            .borrow()
            .as_ref()
            .and_then(|value| value.downcast_ref::<u64>())
            .copied()
            .unwrap_or(0)
            .wrapping_add(1);
        self.stage(token, Box::new(next));
    }

    fn clear_guess_id(&self, id: NodeId) {
        if let Some(node) = self.node(id) {
            node.borrow().guess.borrow_mut().take();
        }
        self.mark_display_changed(id);
        self.bump_affects();
    }

    fn mark_display_changed(&self, id: NodeId) {
        let revision = self.revision.get() + 1;
        self.revision.set(revision);
        if let Some(node) = self.node(id) {
            node.borrow().last_changed.set(revision);
        }
        self.mark_subs(id);
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
            DirtyState::Clean => {
                if !node.borrow().computed.get()
                    && node.borrow().value.borrow().is_none()
                    && !node.borrow().in_flight.get()
                    && node.borrow().async_error.borrow().is_none()
                {
                    self.recompute(id);
                }
            }
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
        let kind = node.borrow().kind;
        if kind.is_async() {
            self.recompute_async(id);
        } else if matches!(kind, NodeKind::Boundary) {
            self.recompute_boundary(id);
        } else if matches!(kind, NodeKind::ErrorBoundary) {
            self.recompute_error_boundary(id);
        } else {
            self.recompute_sync(id);
        }
    }

    fn recompute_sync(&self, id: NodeId) {
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

        let is_effect = node.borrow().kind.is_effect();
        let previous_observer = self.observer.replace(Some(id));
        let previous_tracking = self.tracking.replace(true);
        let previous_class = self.reader_class.replace(if is_effect {
            ReaderClass::Display
        } else {
            ReaderClass::Derive
        });
        let result = catch_unwind(AssertUnwindSafe(|| {
            compute(&self.cx(), old.as_ref().map(|value| &**value as &dyn Any))
        }));
        self.reader_class.set(previous_class);
        self.observer.set(previous_observer);
        self.tracking.set(previous_tracking);

        node.borrow_mut().compute.borrow_mut().replace(compute);

        let new = match result {
            Ok(new) => new,
            Err(payload) => {
                node.borrow_mut().computing.set(false);
                if is_suspension(&*payload) {
                    let node = node.borrow_mut();
                    node.state.set(DirtyState::Clean);
                    node.computed.set(old.is_some());
                    if let Some(old) = old {
                        node.value.borrow_mut().replace(old);
                    }
                    node.last_compute_at.set(self.revision.get());
                } else {
                    resume_unwind(payload);
                }
                return;
            }
        };

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

    fn recompute_async(&self, id: NodeId) {
        let Some(node) = self.node(id) else {
            return;
        };
        {
            let node = node.borrow();
            if node.disposed.get() || node.computing.get() {
                return;
            }
        }

        node.borrow_mut().computing.set(true);
        node.borrow_mut().async_error.borrow_mut().take();

        // Supersede any existing flight. Cancelling the token stops the task;
        // taking the shared slot drops the innermost future immediately, so a
        // teardown guard runs exactly once.
        if node.borrow().in_flight.get() {
            if let Some(cancel) = node.borrow_mut().cancel.borrow_mut().take() {
                cancel.set(true);
            }
            if let Some(slot) = node.borrow_mut().future_slot.borrow_mut().take() {
                slot.borrow_mut().take();
            }
            node.borrow_mut().in_flight.set(false);
            let flight = node.borrow().flight.get();
            node.borrow_mut().flight.set(flight + 1);
        }

        let quiet = self.refresh_nodes.borrow_mut().remove(&id);
        node.borrow().quiet.set(quiet);
        self.unlink_all_deps(id);
        let (mut compute, old) = {
            let node = node.borrow();
            let compute = node
                .async_compute
                .borrow_mut()
                .take()
                .expect("async node has a compute closure");
            let old = node.value.borrow_mut().take();
            (compute, old)
        };

        let previous_observer = self.observer.replace(Some(id));
        let previous_tracking = self.tracking.replace(true);
        let previous_class = self.reader_class.replace(ReaderClass::Derive);
        let result = catch_unwind(AssertUnwindSafe(|| {
            compute(&self.cx(), old.as_ref().map(|value| &**value as &dyn Any))
        }));
        self.reader_class.set(previous_class);
        self.observer.set(previous_observer);
        self.tracking.set(previous_tracking);

        node.borrow_mut()
            .async_compute
            .borrow_mut()
            .replace(compute);

        match result {
            Ok(future) => {
                let flight = node.borrow().flight.get() + 1;
                let slot: FutureSlot = Rc::new(RefCell::new(Some(future)));
                let cancel = Rc::new(Cell::new(false));
                let node_ref = node.borrow();
                node_ref.flight.set(flight);
                node_ref.future_flight.set(flight);
                node_ref.future_slot.borrow_mut().replace(Rc::clone(&slot));
                node_ref.cancel.borrow_mut().replace(Rc::clone(&cancel));
                node_ref.in_flight.set(true);
                node_ref.computed.set(old.is_some());
                node_ref.state.set(DirtyState::Clean);
                node_ref.last_compute_at.set(self.revision.get());
                node_ref.computing.set(false);
                if let Some(old) = old {
                    node_ref.value.borrow_mut().replace(old);
                }
                drop(node_ref);
                self.spawn_flight(id, flight, slot, cancel);
                // Pending is observable state: readers of `is_pending`
                // (disabled controls, held display text) must re-run when a
                // flight starts, not only when it commits.
                self.bump_pending(id);
            }
            Err(payload) => {
                node.borrow_mut().computing.set(false);
                if is_suspension(&*payload) {
                    let node = node.borrow();
                    node.state.set(DirtyState::Clean);
                    node.computed.set(old.is_some());
                    node.in_flight.set(false);
                    node.last_compute_at.set(self.revision.get());
                    if let Some(old) = old {
                        node.value.borrow_mut().replace(old);
                    }
                } else {
                    resume_unwind(payload);
                }
            }
        }
    }

    fn recompute_boundary(&self, id: NodeId) {
        let Some(node) = self.node(id) else {
            return;
        };
        {
            let node = node.borrow();
            if node.disposed.get() || node.computing.get() {
                return;
            }
        }

        node.borrow_mut().computing.set(true);
        self.unlink_all_deps(id);

        let fns = node
            .borrow_mut()
            .boundary
            .borrow_mut()
            .take()
            .expect("boundary node has functions");
        let old = node.borrow_mut().value.borrow_mut().take();

        let previous_observer = self.observer.replace(Some(id));
        let previous_tracking = self.tracking.replace(true);
        let previous_class = self.reader_class.replace(ReaderClass::Derive);

        let key_now = fns.key.map(|key| key.get(&self.cx()));
        let key_changed = key_now != *fns.current_key.borrow();
        if key_changed {
            *fns.current_key.borrow_mut() = key_now;
            fns.armed.set(true);
            fns.has_content.set(false);
        }

        let mut fallback_value = None;
        if fns.armed.get() || !fns.has_content.get() || old.is_none() {
            fallback_value = Some(catch_unwind(AssertUnwindSafe(|| {
                (fns.fallback)(&self.cx())
            })));
        }

        let content_result = catch_unwind(AssertUnwindSafe(|| (fns.content)(&self.cx())));

        self.reader_class.set(previous_class);
        self.observer.set(previous_observer);
        self.tracking.set(previous_tracking);

        let mut value = old;
        let mut changed = false;

        match content_result {
            Ok(content) => {
                value = Some(content);
                fns.has_content.set(true);
                fns.armed.set(false);
                changed = true;
            }
            Err(payload) => {
                if is_suspension(&*payload) {
                    if !fns.has_content.get() {
                        match fallback_value {
                            Some(Ok(fallback)) => {
                                value = Some(fallback);
                                changed = true;
                            }
                            Some(Err(payload)) if !is_suspension(&*payload) => {
                                resume_unwind(payload);
                            }
                            _ => {}
                        }
                    }
                } else {
                    resume_unwind(payload);
                }
            }
        }

        node.borrow_mut().boundary.borrow_mut().replace(fns);
        let node = node.borrow();
        node.computing.set(false);
        node.computed.set(true);
        node.state.set(DirtyState::Clean);
        node.last_compute_at.set(self.revision.get());
        if let Some(value) = value {
            node.value.borrow_mut().replace(value);
        }
        if changed {
            node.last_changed.set(self.revision.get());
        }
        drop(node);
        if changed {
            self.mark_subs(id);
        }
    }

    fn recompute_error_boundary(&self, id: NodeId) {
        let Some(node) = self.node(id) else {
            return;
        };
        {
            let node = node.borrow();
            if node.disposed.get() || node.computing.get() {
                return;
            }
        }

        node.borrow_mut().computing.set(true);
        self.unlink_all_deps(id);

        let fns = node
            .borrow_mut()
            .error_boundary
            .borrow_mut()
            .take()
            .expect("error boundary node has functions");
        let old = node.borrow_mut().value.borrow_mut().take();

        let previous_observer = self.observer.replace(Some(id));
        let previous_tracking = self.tracking.replace(true);
        let content_result = catch_unwind(AssertUnwindSafe(|| (fns.content)(&self.cx())));

        let value = match content_result {
            Ok(Ok(content)) => Some(content),
            Ok(Err(error)) => Some((fns.fallback)(&self.cx(), &*error)),
            Err(payload) => {
                if is_suspension(&*payload) {
                    old
                } else {
                    self.observer.set(previous_observer);
                    self.tracking.set(previous_tracking);
                    node.borrow_mut().error_boundary.borrow_mut().replace(fns);
                    node.borrow_mut().computing.set(false);
                    resume_unwind(payload);
                }
            }
        };

        self.observer.set(previous_observer);
        self.tracking.set(previous_tracking);

        node.borrow_mut().error_boundary.borrow_mut().replace(fns);
        let node = node.borrow();
        node.computing.set(false);
        node.computed.set(true);
        node.state.set(DirtyState::Clean);
        node.last_compute_at.set(self.revision.get());
        if let Some(value) = value {
            node.value.borrow_mut().replace(value);
        }
        node.last_changed.set(self.revision.get());
        drop(node);
        self.mark_subs(id);
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
        let applied = value.is_some();
        node.borrow_mut().apply.borrow_mut().replace(apply);
        *node.borrow_mut().value.borrow_mut() = value;
        if applied {
            self.bump_effect(id);
        }
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
        if let Some(cancel) = node.borrow_mut().cancel.borrow_mut().take() {
            cancel.set(true);
        }
        if let Some(slot) = node.borrow_mut().future_slot.borrow_mut().take() {
            slot.borrow_mut().take();
        }
        node.borrow_mut().in_flight.set(false);
        node.borrow_mut().value.borrow_mut().take();
        node.borrow_mut().staged.borrow_mut().take();
        node.borrow_mut().compute.borrow_mut().take();
        node.borrow_mut().apply.borrow_mut().take();
        self.nodes.borrow_mut().remove(id);
    }

    fn dispose_owner(&self, id: OwnerId) {
        let Some(owner) = self.owners.borrow().get(id.id).cloned() else {
            return;
        };
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
        if id != self.root {
            self.owners.borrow_mut().remove(id.id);
        }
    }

    /// Flush staged writes, run computes, then applies, until stable.
    /// Poll every in-flight async future once.
    ///
    /// The host spawner is pumped first; completed flights are posted to the
    /// runtime's completion queue and drained here. A ready result is held until
    /// the next [`flush`](Self::flush).
    pub fn pump(&self) {
        self.spawner.pump();
        let completions = std::mem::take(&mut *self.completions.borrow_mut());
        for completion in completions {
            let Some(node) = self.node(completion.node) else {
                continue;
            };
            if node.borrow().disposed.get()
                || node.borrow().future_flight.get() != completion.flight
                || !node.borrow().in_flight.get()
            {
                continue;
            }
            node.borrow_mut().in_flight.set(false);
            node.borrow_mut()
                .async_result
                .borrow_mut()
                .replace(completion.result);
            self.async_ready.borrow_mut().push(completion.node);
        }
    }

    fn spawn_flight(&self, id: NodeId, flight: u64, slot: FutureSlot, cancel: Rc<Cell<bool>>) {
        let completions = Rc::clone(&self.completions);
        let task = std::future::poll_fn(move |context| {
            if cancel.get() {
                return Poll::Ready(());
            }
            let mut slot = slot.borrow_mut();
            let Some(future) = slot.as_mut() else {
                return Poll::Ready(());
            };
            match future.as_mut().poll(context) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(result) => {
                    slot.take();
                    completions.borrow_mut().push_back(Completion {
                        node: id,
                        flight,
                        result,
                    });
                    Poll::Ready(())
                }
            }
        });
        self.spawner.spawn_local(Box::pin(task));
    }

    fn commit_async(&self) {
        let ready = std::mem::take(&mut *self.async_ready.borrow_mut());
        if ready.is_empty() {
            return;
        }
        let revision = self.revision.get() + 1;
        self.revision.set(revision);

        for id in ready {
            let Some(node) = self.node(id) else {
                continue;
            };
            if node.borrow().disposed.get() {
                continue;
            }
            let result = node.borrow_mut().async_result.borrow_mut().take();
            let Some(result) = result else {
                continue;
            };

            match result {
                Ok(new_value) => {
                    let changed = {
                        let node_ref = node.borrow();
                        let committed = node_ref.value.borrow();
                        match committed.as_ref() {
                            None => true,
                            Some(old) => !node_ref.equals.eq(&**old, &*new_value),
                        }
                    };
                    let quiet = node.borrow().quiet.get();
                    let node_ref = node.borrow();
                    node_ref.quiet.set(false);
                    node_ref.value.borrow_mut().replace(new_value);
                    node_ref.async_error.borrow_mut().take();
                    node_ref.computed.set(true);
                    node_ref.state.set(DirtyState::Clean);
                    if changed {
                        node_ref.last_changed.set(revision);
                    }
                    drop(node_ref);
                    if changed {
                        self.mark_subs(id);
                    }
                    if changed || !quiet {
                        self.bump_pending(id);
                    }
                }
                Err(error) => {
                    let node_ref = node.borrow();
                    node_ref.quiet.set(false);
                    node_ref.async_error.borrow_mut().replace(error.to_string());
                    node_ref.value.borrow_mut().take();
                    node_ref.computed.set(false);
                    node_ref.state.set(DirtyState::Clean);
                    node_ref.last_changed.set(revision);
                    drop(node_ref);
                    self.mark_subs(id);
                    self.bump_pending(id);
                }
            }
        }
    }

    /// Flush staged writes, commit ready async results, run computes, then
    /// applies, until stable.
    pub fn flush(&self) -> Result<(), FlushError> {
        if self.in_flush.replace(true) {
            return Err(FlushError::Reentrant);
        }
        let mut rounds = 0;
        loop {
            self.commit_async();
            let has_work = !self.staged.borrow().is_empty()
                || !self.heap.borrow().is_empty()
                || !self.render_applies.borrow().is_empty()
                || !self.user_applies.borrow().is_empty()
                || !self.async_ready.borrow().is_empty();
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
        let affected = std::mem::take(&mut *self.affected.borrow_mut());
        if !affected.is_empty() {
            self.bump_affects();
        }
        self.in_flush.set(false);
        Ok(())
    }

    fn latest_value<T: Clone + 'static>(&self, id: NodeId) -> Result<T, LatestError> {
        let Some(node_ref) = self.node(id) else {
            return Err(LatestError::Loading);
        };
        let node = node_ref.borrow();
        if let Some(guess) = node
            .guess
            .borrow()
            .as_ref()
            .and_then(|value| value.downcast_ref::<T>())
        {
            return Ok(guess.clone());
        }
        if let Some(Ok(value)) = node.async_result.borrow().as_ref() {
            if let Some(value) = value.downcast_ref::<T>() {
                return Ok(value.clone());
            }
        }
        let committed = node
            .value
            .borrow()
            .as_ref()
            .and_then(|value| value.downcast_ref::<T>())
            .cloned();
        committed.ok_or(LatestError::Loading)
    }

    fn committed_value<T: Clone + 'static>(&self, id: NodeId) -> Result<T, LatestError> {
        let Some(node_ref) = self.node(id) else {
            return Err(LatestError::Loading);
        };
        let node = node_ref.borrow();
        let committed = node
            .value
            .borrow()
            .as_ref()
            .and_then(|value| value.downcast_ref::<T>())
            .cloned();
        committed.ok_or(LatestError::Loading)
    }

    fn affects_token(&self) -> NodeId {
        if let Some(id) = self.affects_token.get() {
            return id;
        }
        let (read, _) = create_signal(&self.cx(), 0_u64);
        self.affects_token.set(Some(read.id()));
        read.id()
    }

    fn bump_affects(&self) {
        let id = self.affects_token();
        let Some(node) = self.node(id) else {
            return;
        };
        let next = node
            .borrow()
            .value
            .borrow()
            .as_ref()
            .and_then(|value| value.downcast_ref::<u64>())
            .copied()
            .unwrap_or(0)
            .wrapping_add(1);
        self.stage(id, Box::new(next));
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

/// Whether any computation read by `read` has a newer answer in flight.
///
/// The closure is evaluated in a pending-probe context: async reads with a
/// committed value report their old value but mark the probe; reads with no
/// committed value suspend and count as pending. This accepts any expression,
/// not a specific signal type.
pub fn is_pending<T>(cx: &Cx<'_>, read: impl FnOnce(&Cx<'_>) -> T) -> bool {
    cx.rt.is_pending_probe(read)
}

/// A display-ahead read that could not produce a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LatestError {
    /// The node has never produced a value.
    Loading,
}

/// A node handle usable by the lane APIs.
pub trait NodeHandle {
    /// The graph node behind this handle.
    fn node_id(&self) -> NodeId;
}

impl<T> NodeHandle for ReadSignal<T> {
    fn node_id(&self) -> NodeId {
        self.id
    }
}

impl<T> NodeHandle for WriteSignal<T> {
    fn node_id(&self) -> NodeId {
        self.id
    }
}

impl<T> NodeHandle for Memo<T> {
    fn node_id(&self) -> NodeId {
        self.id
    }
}

/// Read the newest answer a lane can show.
///
/// In a display pass this prefers a guess, then a landed-but-uncommitted async
/// answer, then the committed value. In a derive pass it returns only the
/// committed value, so derivations cannot tear against the screen.
pub fn latest<T: Clone + 'static>(cx: &Cx<'_>, memo: Memo<T>) -> Result<T, LatestError> {
    cx.rt.track(memo.id());
    cx.rt.ensure_clean(memo.id());
    if cx.rt.reader_class.get() == ReaderClass::Derive {
        cx.rt.committed_value(memo.id())
    } else {
        cx.rt.latest_value(memo.id())
    }
}

/// Ask an async memo to run its question again without reporting pending.
///
/// This is the quiet re-ask path: `is_pending` stays false while the flight is
/// out, and an equal answer does not notify readers.
pub fn refresh<T>(cx: &Cx<'_>, memo: Memo<T>) {
    let id = memo.id();
    cx.rt.refresh_nodes.borrow_mut().insert(id);
    if let Some(node) = cx.rt.node(id) {
        node.borrow().state.set(DirtyState::Dirty);
    }
    cx.rt.schedule(id);
}

/// Show `value` in display passes without committing it.
///
/// Derivations keep reading the committed value. The guess marks the node
/// pending and lasts until [`clear_guess`].
pub fn guess<T: Clone + 'static, H: NodeHandle>(cx: &Cx<'_>, node: &H, value: T) {
    let id = node.node_id();
    if let Some(node) = cx.rt.node(id) {
        node.borrow().guess.borrow_mut().replace(Box::new(value));
    }
    cx.rt.mark_display_changed(id);
    cx.rt.bump_affects();
}

/// Remove a guess and return display reads to the committed value.
pub fn clear_guess<H: NodeHandle>(cx: &Cx<'_>, node: &H) {
    cx.rt.clear_guess_id(node.node_id());
}

/// The capability an action uses to guess and commit writes.
#[derive(Clone)]
pub struct ActionCx {
    runtime: RuntimeHandle,
    guesses: Rc<RefCell<Vec<NodeId>>>,
}

impl ActionCx {
    /// Show a display-only guess for the duration of the action.
    pub fn guess<T: Clone + 'static, H: NodeHandle>(&self, node: &H, value: T) {
        guess(&self.runtime.cx(), node, value);
        let id = node.node_id();
        let mut guesses = self.guesses.borrow_mut();
        if !guesses.contains(&id) {
            guesses.push(id);
        }
    }

    /// Stage a committed write. It becomes visible at the next flush.
    pub fn set<T: 'static>(&self, signal: WriteSignal<T>, value: T) {
        signal.set(&self.runtime.cx(), value);
    }

    /// Stage a committed update computed from the previous value.
    pub fn update_with<T: 'static>(&self, signal: WriteSignal<T>, f: impl FnOnce(&T) -> T) {
        signal.update_with(&self.runtime.cx(), f);
    }

    /// Read the committed value of a signal without tracking or guesses.
    #[must_use]
    pub fn get<T: Clone + 'static>(&self, signal: ReadSignal<T>) -> T {
        self.runtime
            .committed_value(signal.id())
            .expect("signal has a committed value")
    }
}

/// The completion signal of an action.
pub struct ActionHandle<T> {
    result: ReadSignal<Option<Result<T, String>>>,
}

impl<T: Clone + 'static> ActionHandle<T> {
    /// The action's outcome, or `None` while it is still running.
    #[must_use]
    pub fn result(&self) -> ReadSignal<Option<Result<T, String>>> {
        self.result
    }
}

/// A node that can be declared pending.
pub trait Affects {
    /// The graph node behind this handle.
    fn node_id(&self) -> NodeId;
}

impl<T> Affects for ReadSignal<T> {
    fn node_id(&self) -> NodeId {
        self.id
    }
}

impl<T> Affects for WriteSignal<T> {
    fn node_id(&self) -> NodeId {
        self.id
    }
}

impl<T> Affects for Memo<T> {
    fn node_id(&self) -> NodeId {
        self.id
    }
}

/// Declare that `node` is changing without writing a value.
///
/// The node and any derivation read from it report pending until the current
/// flush commits. Use it for data that is about to be replaced by a result
/// that is not a value write.
pub fn affects<T: Affects>(cx: &Cx<'_>, node: &T) {
    let id = node.node_id();
    cx.rt.affected.borrow_mut().insert(id);
    cx.rt.bump_affects();
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

/// Create an async cached memo.
pub fn create_async_memo<T, F>(cx: &Cx<'_>, mut compute: F) -> Memo<T>
where
    T: PartialEq + 'static,
    F: FnMut(&Cx<'_>, Option<&T>) -> Pin<Box<dyn Future<Output = Result<T, AsyncError>>>> + 'static,
{
    let async_compute: AsyncComputeFn = Box::new(move |cx, previous| {
        let previous = previous.and_then(|value| value.downcast_ref::<T>());
        let future = compute(cx, previous);
        Box::pin(async move { future.await.map(|value| Box::new(value) as Box<dyn Any>) })
    });
    let id = cx
        .rt
        .alloc_node(NodeKind::AsyncMemo, None, None, Equals::Value(any_eq::<T>));
    let node = cx.rt.node(id).expect("new async memo");
    node.borrow_mut()
        .async_compute
        .borrow_mut()
        .replace(async_compute);
    node.borrow_mut().state.set(DirtyState::Dirty);
    let (pending, _) = create_signal(cx, 0_u64);
    cx.rt.pending_tokens.borrow_mut().insert(id, pending.id());
    Memo::new(id)
}

/// Create an async writable derived signal.
pub fn create_async_derived_signal<T, F>(
    cx: &Cx<'_>,
    mut compute: F,
) -> (ReadSignal<T>, WriteSignal<T>)
where
    T: PartialEq + 'static,
    F: FnMut(&Cx<'_>, Option<&T>) -> Pin<Box<dyn Future<Output = Result<T, AsyncError>>>> + 'static,
{
    let async_compute: AsyncComputeFn = Box::new(move |cx, previous| {
        let previous = previous.and_then(|value| value.downcast_ref::<T>());
        let future = compute(cx, previous);
        Box::pin(async move { future.await.map(|value| Box::new(value) as Box<dyn Any>) })
    });
    let id = cx.rt.alloc_node(
        NodeKind::AsyncDerivedSignal,
        None,
        None,
        Equals::Value(any_eq::<T>),
    );
    let node = cx.rt.node(id).expect("new async derived signal");
    node.borrow_mut()
        .async_compute
        .borrow_mut()
        .replace(async_compute);
    node.borrow_mut().state.set(DirtyState::Dirty);
    let (pending, _) = create_signal(cx, 0_u64);
    cx.rt.pending_tokens.borrow_mut().insert(id, pending.id());
    (ReadSignal::new(id), WriteSignal::new(id))
}

/// Create a loading boundary.
///
/// `key` is an optional `u64` signal; a key change remounts the boundary and
/// shows the fallback for the new load. Content is evaluated as one unit: if any
/// read inside it is pending, the boundary keeps its old content (disarmed) or
/// its fallback (first load/key remount).
pub fn loading_boundary<T>(
    cx: &Cx<'_>,
    key: Option<ReadSignal<u64>>,
    content: impl Fn(&Cx<'_>) -> T + 'static,
    fallback: impl Fn(&Cx<'_>) -> T + 'static,
) -> LoadingBoundary<T>
where
    T: 'static,
{
    let boundary = BoundaryFns {
        content: Box::new(move |cx| Box::new(content(cx))),
        fallback: Box::new(move |cx| Box::new(fallback(cx))),
        key,
        armed: Cell::new(true),
        current_key: RefCell::new(None),
        has_content: Cell::new(false),
    };
    let id = cx
        .rt
        .alloc_node(NodeKind::Boundary, None, None, Equals::Always);
    let node = cx.rt.node(id).expect("new boundary");
    node.borrow_mut().boundary.borrow_mut().replace(boundary);
    node.borrow_mut().state.set(DirtyState::Dirty);
    LoadingBoundary::new(id)
}

/// Create an error boundary.
pub fn error_boundary<T, E>(
    cx: &Cx<'_>,
    content: impl Fn(&Cx<'_>) -> Result<T, E> + 'static,
    fallback: impl Fn(&Cx<'_>, &E) -> T + 'static,
) -> ErrorBoundary<T>
where
    T: 'static,
    E: 'static,
{
    let content = Box::new(move |cx: &Cx<'_>| {
        content(cx)
            .map(|value| Box::new(value) as Box<dyn Any>)
            .map_err(|error| Box::new(error) as Box<dyn Any>)
    });
    let fallback = Box::new(move |cx: &Cx<'_>, error: &dyn Any| {
        let error = error.downcast_ref::<E>().expect("error boundary type");
        Box::new(fallback(cx, error)) as Box<dyn Any>
    });
    let id = cx
        .rt
        .alloc_node(NodeKind::ErrorBoundary, None, None, Equals::Always);
    let node = cx.rt.node(id).expect("new error boundary");
    node.borrow_mut()
        .error_boundary
        .borrow_mut()
        .replace(ErrorBoundaryFns { content, fallback });
    node.borrow_mut().state.set(DirtyState::Dirty);
    ErrorBoundary::new(id)
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
    cx.rt.register_effect_token(cx, id);
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
    cx.rt.register_effect_token(cx, id);
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
    AsyncMemo,
    AsyncDerivedSignal,
    Boundary,
    ErrorBoundary,
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

    fn is_async(self) -> bool {
        matches!(self, Self::AsyncMemo | Self::AsyncDerivedSignal)
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
    async_compute: RefCell<Option<AsyncComputeFn>>,
    future_slot: RefCell<Option<FutureSlot>>,
    cancel: RefCell<Option<Rc<Cell<bool>>>>,
    flight: Cell<u64>,
    future_flight: Cell<u64>,
    in_flight: Cell<bool>,
    quiet: Cell<bool>,
    guess: RefCell<Option<Box<dyn Any>>>,
    async_result: RefCell<Option<Result<Box<dyn Any>, AsyncError>>>,
    async_error: RefCell<Option<String>>,
    boundary: RefCell<Option<BoundaryFns>>,
    error_boundary: RefCell<Option<ErrorBoundaryFns>>,
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
            async_compute: RefCell::new(None),
            future_slot: RefCell::new(None),
            cancel: RefCell::new(None),
            flight: Cell::new(0),
            future_flight: Cell::new(0),
            in_flight: Cell::new(false),
            quiet: Cell::new(false),
            guess: RefCell::new(None),
            async_result: RefCell::new(None),
            async_error: RefCell::new(None),
            boundary: RefCell::new(None),
            error_boundary: RefCell::new(None),
            apply: RefCell::new(apply),
            equals,
            last_changed: Cell::new(0),
            last_compute_at: Cell::new(0),
            apply_queued: Cell::new(false),
        }
    }
}

struct BoundaryFns {
    content: BoundaryFn,
    fallback: BoundaryFn,
    key: Option<ReadSignal<u64>>,
    armed: Cell<bool>,
    current_key: RefCell<Option<u64>>,
    has_content: Cell<bool>,
}

type BoundaryFn = Box<dyn Fn(&Cx<'_>) -> Box<dyn Any>>;

struct ErrorBoundaryFns {
    content: ErrorContentFn,
    fallback: ErrorFallbackFn,
}

type ErrorContentFn = Box<dyn Fn(&Cx<'_>) -> Result<Box<dyn Any>, Box<dyn Any>>>;
type ErrorFallbackFn = Box<dyn Fn(&Cx<'_>, &dyn Any) -> Box<dyn Any>>;

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
