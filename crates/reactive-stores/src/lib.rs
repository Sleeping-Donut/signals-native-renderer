#![forbid(unsafe_code)]
//! Field-granular stores, patches, and keyed collections.
//!
//! A [`Store<T>`] owns one value and a trigger per accessed path. Field handles
//! read and write through typed accessor closures; a write marks only that path
//! and its ancestors, leaving siblings untouched.

use std::cell::{Cell, RefCell};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Debug;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use reactive_core::{
    create_async_memo, create_signal, latest, refresh, render_effect, AsyncError, Cx, Memo,
    NodeHandle, NodeId, OwnerHandle, ReadSignal, RuntimeHandle, WriteSignal,
};

pub use reactive_stores_macro::{Patch, Store};

type Trigger = (ReadSignal<u64>, WriteSignal<u64>);
type ReadFn<T> = Rc<dyn Fn() -> T>;
type WriteFn<T> = Rc<dyn Fn(T)>;

/// Store diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StoreDiagnostics {
    /// Field reads.
    pub reads: u64,
    /// Field writes.
    pub writes: u64,
    /// Interned trigger paths.
    pub triggers: usize,
    /// Live store handles sharing this value.
    pub handles: usize,
}

trait StoreAccess {
    /// Track `path` and return the node that carries its guesses.
    fn track(&self, cx: &Cx<'_>, path: &str) -> NodeId;
    fn bump(&self, cx: &Cx<'_>, path: &str);
    fn bump_shallow(&self, cx: &Cx<'_>, path: &str);
    fn note_read(&self);
    fn note_write(&self);
    fn trigger_count(&self) -> usize;
    fn reads(&self) -> u64;
    fn writes(&self) -> u64;
}

struct StoreInner<T> {
    value: RefCell<T>,
    triggers: RefCell<HashMap<String, Trigger>>,
    owner: OwnerHandle,
    reads: Cell<u64>,
    writes: Cell<u64>,
}

impl<T> StoreAccess for StoreInner<T> {
    fn track(&self, cx: &Cx<'_>, path: &str) -> NodeId {
        let trigger = self.trigger(cx, path);
        trigger.0.get(cx);
        trigger.0.node_id()
    }

    fn bump(&self, cx: &Cx<'_>, path: &str) {
        let ancestors = ancestors(path);
        let keys: Vec<String> = self.triggers.borrow().keys().cloned().collect();
        for key in keys {
            let descendant = key == path
                || key.starts_with(&format!("{path}."))
                || key.starts_with(&format!("{path}["))
                || key.starts_with(&format!("{path}#"));
            if descendant || ancestors.contains(&key) {
                if let Some(trigger) = self.triggers.borrow().get(&key) {
                    trigger.1.update_with(cx, |value| value.wrapping_add(1));
                }
            }
        }
    }

    fn bump_shallow(&self, cx: &Cx<'_>, path: &str) {
        for ancestor in ancestors(path) {
            if let Some(trigger) = self.triggers.borrow().get(&ancestor) {
                trigger.1.update_with(cx, |value| value.wrapping_add(1));
            }
        }
    }

    fn note_read(&self) {
        self.reads.set(self.reads.get() + 1);
    }

    fn note_write(&self) {
        self.writes.set(self.writes.get() + 1);
    }

    fn trigger_count(&self) -> usize {
        self.triggers.borrow().len()
    }

    fn reads(&self) -> u64 {
        self.reads.get()
    }

    fn writes(&self) -> u64 {
        self.writes.get()
    }
}

impl<T> StoreInner<T> {
    fn trigger(&self, cx: &Cx<'_>, path: &str) -> Trigger {
        if let Some(trigger) = self.triggers.borrow().get(path) {
            return *trigger;
        }
        let trigger = cx.with_owner(self.owner, |cx| create_signal(cx, 0_u64));
        self.triggers.borrow_mut().insert(path.to_string(), trigger);
        trigger
    }
}

fn ancestors(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = path.to_string();
    loop {
        out.push(current.clone());
        if let Some(index) = current.rfind(['.', '[', '#']) {
            current.truncate(index);
        } else {
            break;
        }
    }
    if !out.iter().any(String::is_empty) {
        out.push(String::new());
    }
    out
}

/// A shared store value.
pub struct Store<T> {
    inner: Rc<StoreInner<T>>,
}

impl<T> Clone for Store<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Rc::clone(&self.inner),
        }
    }
}

impl<T: Clone + 'static> Store<T> {
    /// Create a store owned by the current reactive owner.
    #[must_use]
    pub fn new(cx: &Cx<'_>, value: T) -> Self {
        let owner = cx.create_owner();
        Self {
            inner: Rc::new(StoreInner {
                value: RefCell::new(value),
                triggers: RefCell::new(HashMap::new()),
                owner,
                reads: Cell::new(0),
                writes: Cell::new(0),
            }),
        }
    }

    /// The root field handle.
    ///
    /// Useful for a store that is one value (a list, a record) rather than a
    /// struct with named fields.
    #[must_use]
    pub fn root(&self) -> Field<T> {
        self.project("", |value| value.clone(), |value, next| *value = next)
    }

    /// Read the root value.
    #[must_use]
    pub fn get(&self, cx: &Cx<'_>) -> T {
        self.root().get(cx)
    }

    /// Write the root value.
    pub fn set(&self, cx: &Cx<'_>, value: T) {
        self.root().set(cx, value);
    }

    /// Read the whole value and track the root path.
    #[must_use]
    pub fn snapshot(&self, cx: &Cx<'_>) -> T {
        self.inner.track(cx, "");
        self.inner.note_read();
        self.inner.value.borrow().clone()
    }

    /// A field handle for one root field.
    pub fn project<U: 'static>(
        &self,
        key: &'static str,
        read: impl Fn(&T) -> U + 'static,
        write: impl Fn(&mut T, U) + 'static,
    ) -> Field<U> {
        let inner = Rc::clone(&self.inner);
        let read = Rc::new(read);
        let write = Rc::new(write);
        let read_inner = Rc::clone(&inner);
        let write_inner = Rc::clone(&inner);
        Field {
            inner: Rc::new(FieldInner {
                store: inner,
                path: key.to_string(),
                read: Rc::new(move || read(&read_inner.value.borrow())),
                write: Rc::new(move |value| {
                    write(&mut write_inner.value.borrow_mut(), value);
                }),
            }),
        }
    }

    /// Diagnostics for this store.
    #[must_use]
    pub fn diagnostics(&self) -> StoreDiagnostics {
        StoreDiagnostics {
            reads: self.inner.reads(),
            writes: self.inner.writes(),
            triggers: self.inner.trigger_count(),
            handles: Rc::strong_count(&self.inner),
        }
    }
}

/// A store derived from an async source.
///
/// The source is the truth: its first answer loads the store, and
/// [`OptimisticStore::refresh`] re-asks it. Writes to the store are ordinary
/// per-path guesses, so a success commits optimistically while the refresh
/// catches up with the server. Reads join the source, so the first load is a
/// loading state rather than an empty seed.
pub struct OptimisticStore<T: 'static> {
    store: Store<T>,
    source: Memo<T>,
    initialized: Rc<std::cell::Cell<bool>>,
}

impl<T> Clone for OptimisticStore<T> {
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            source: self.source,
            initialized: Rc::clone(&self.initialized),
        }
    }
}

impl<T: Clone + PartialEq + 'static> OptimisticStore<T> {
    /// Create a store whose truth comes from `load`.
    ///
    /// `seed` is the placeholder used before the first answer. The first read
    /// of the store joins the source, so a loading boundary sees a loading
    /// state instead of the seed.
    pub fn new<F, Fut>(cx: &Cx<'_>, seed: T, mut load: F) -> Self
    where
        F: FnMut() -> Fut + 'static,
        Fut: std::future::Future<Output = Result<T, AsyncError>> + 'static,
    {
        let source = create_async_memo(cx, move |_cx: &Cx<'_>, _previous: Option<&T>| {
            Box::pin(load())
        });
        let store = Store::new(cx, seed);
        let initialized = Rc::new(std::cell::Cell::new(false));
        let store_for_effect = store.clone();
        let initialized_for_effect = Rc::clone(&initialized);
        let _effect = render_effect(
            cx,
            move |cx| latest(cx, source).ok(),
            move |cx, value: &Option<T>| {
                if let Some(value) = value {
                    store_for_effect.set(cx, value.clone());
                    initialized_for_effect.set(true);
                }
            },
        );
        Self {
            store,
            source,
            initialized,
        }
    }

    /// The display value: the store once the source has landed, otherwise the
    /// source's current answer.
    ///
    /// For a list, use [`OptimisticStore::list`] so per-key patches and row
    /// guesses compose; this is the root value alone.
    #[must_use]
    pub fn get(&self, cx: &Cx<'_>) -> T {
        let value = self.source.get(cx);
        if self.initialized.get() {
            self.store.get(cx)
        } else {
            value
        }
    }

    /// Ask the source again. The store keeps its optimistic value until the
    /// new answer lands.
    pub fn refresh(&self, cx: &Cx<'_>) {
        refresh(cx, self.source);
    }

    /// The root field handle.
    #[must_use]
    pub fn root(&self) -> Field<T> {
        self.store.root()
    }

    /// Whether the first answer has landed.
    ///
    /// Tracks the source and the store without suspending, so a loading
    /// boundary can gate content on it.
    #[must_use]
    pub fn is_loading(&self, cx: &Cx<'_>) -> bool {
        let _ = latest(cx, self.source);
        let _ = self.store.root().get(cx);
        !self.initialized.get()
    }
}

impl<T: Clone + PartialEq + 'static> OptimisticStore<Vec<T>> {
    /// A keyed list view over this derived store.
    #[must_use]
    pub fn list<K, F>(&self, key: F) -> OptimisticList<T, K>
    where
        K: Clone + Eq + Hash + 'static,
        F: Fn(&T) -> K + 'static,
    {
        OptimisticList {
            list: self.store.root(),
            key: Rc::new(key),
            source: Some(self.source),
            initialized: Some(Rc::clone(&self.initialized)),
        }
    }
}

/// A field handle whose writes are per-action guesses.
///
/// The guess lives on the field's own path node, so a read inside an action
/// dirties exactly the readers of that field. Settling the action drops only
/// its guesses; a success also writes the guessed value to the committed
/// store, a failure discards it.
pub struct OptimisticField<T> {
    field: Field<T>,
}

impl<T> Clone for OptimisticField<T> {
    fn clone(&self) -> Self {
        Self {
            field: self.field.clone(),
        }
    }
}

impl<T: Clone + 'static> OptimisticField<T> {
    /// Read the display value (the guess inside an action, else truth).
    #[must_use]
    pub fn get(&self, cx: &Cx<'_>) -> T {
        self.field.get(cx)
    }

    /// Guess inside an action, write truth outside one.
    pub fn set(&self, cx: &Cx<'_>, value: T) {
        self.field.set(cx, value);
    }

    /// Update the display value.
    pub fn update(&self, cx: &Cx<'_>, f: impl FnOnce(&mut T)) {
        self.field.update(cx, f);
    }

    /// Write truth. Inside an action it lands when the action settles.
    pub fn commit(&self, cx: &Cx<'_>, value: T) {
        self.field.commit(cx, value);
    }

    /// The underlying field handle.
    #[must_use]
    pub fn field(&self) -> &Field<T> {
        &self.field
    }
}

/// A keyed list whose writes are per-action patches.
///
/// A structural change (`push`/`remove`) is a patch on the list's own path
/// node; a row write through [`OptimisticList::at_key`] is a guess on that
/// row's keyed path. Disjoint rows and disjoint keys do not entangle, and a
/// read composes truth, live patches, and live row guesses per key.
pub struct OptimisticList<T, K> {
    list: Field<Vec<T>>,
    key: Rc<dyn Fn(&T) -> K>,
    source: Option<Memo<Vec<T>>>,
    initialized: Option<Rc<Cell<bool>>>,
}

impl<T, K> Clone for OptimisticList<T, K> {
    fn clone(&self) -> Self {
        Self {
            list: self.list.clone(),
            key: Rc::clone(&self.key),
            source: self.source,
            initialized: self.initialized.as_ref().map(Rc::clone),
        }
    }
}

/// One action's structural edit to a keyed list.
#[derive(Clone)]
struct ListPatch<T, K> {
    added: Vec<T>,
    removed: Vec<K>,
}

impl<T: Clone + 'static, K: Clone + Eq + Hash + 'static> OptimisticList<T, K> {
    /// The composed display rows: truth, this list's live patches, and live
    /// row guesses, per key. A derivation sees truth only.
    #[must_use]
    pub fn get(&self, cx: &Cx<'_>) -> Vec<T> {
        let mut rows = if let Some(source) = self.source {
            let value = source.get(cx);
            if self.initialized.as_ref().is_some_and(|ready| ready.get()) {
                self.list.get(cx)
            } else {
                value
            }
        } else {
            self.list.get(cx)
        };
        if !cx.is_display() {
            return rows;
        }
        let node = self.list.node(cx);
        for patch in cx.guess_values::<ListPatch<T, K>, _>(&node) {
            rows.retain(|row| !patch.removed.iter().any(|key| *key == (self.key)(row)));
            rows.extend(patch.added);
        }
        for row in &mut rows {
            let key = (self.key)(row);
            let node = self.list.inner.store.track(cx, &self.row_path(&key));
            if let Some(guess) = cx.guess_value::<T, _>(&node) {
                *row = guess;
            }
        }
        rows
    }

    /// Row count as displayed.
    #[must_use]
    pub fn len(&self, cx: &Cx<'_>) -> usize {
        self.get(cx).len()
    }

    /// Whether the list is empty as displayed.
    #[must_use]
    pub fn is_empty(&self, cx: &Cx<'_>) -> bool {
        self.len(cx) == 0
    }

    /// A stable keyed row handle.
    ///
    /// Returns `None` when no committed row carries `key`; a row that exists
    /// only in a structural guess is list-level until it lands.
    #[must_use]
    pub fn at_key(&self, _cx: &Cx<'_>, key: &K) -> Option<OptimisticField<T>> {
        if !(self.list.inner.read)()
            .iter()
            .any(|row| (self.key)(row) == *key)
        {
            return None;
        }
        let list = self.list.clone();
        let key_fn = Rc::clone(&self.key);
        let keyed = list.keyed(move |row| key_fn(row));
        keyed.at_key(key).map(|field| field.optimistic())
    }

    /// Append a row. Inside an action it is a patch; outside it is truth.
    pub fn push(&self, cx: &Cx<'_>, row: T) {
        if self.stage_patch(
            cx,
            ListPatch {
                added: vec![row.clone()],
                removed: Vec::new(),
            },
        ) {
            let list = self.list.clone();
            let key = Rc::clone(&self.key);
            let row_key = key(&row);
            cx.defer_write(move |cx| {
                let mut rows = (list.inner.read)();
                if !rows.iter().any(|candidate| key(candidate) == row_key) {
                    rows.push(row);
                }
                list.set(cx, rows);
            });
            return;
        }
        let mut rows = (self.list.inner.read)();
        if !rows
            .iter()
            .any(|candidate| (self.key)(candidate) == (self.key)(&row))
        {
            rows.push(row);
        }
        self.list.set(cx, rows);
    }

    /// Remove the row with `key`. Inside an action it is a patch.
    pub fn remove(&self, cx: &Cx<'_>, key: &K) {
        if self.stage_patch(
            cx,
            ListPatch {
                added: Vec::new(),
                removed: vec![key.clone()],
            },
        ) {
            let list = self.list.clone();
            let key_fn = Rc::clone(&self.key);
            let row_key = key.clone();
            cx.defer_write(move |cx| {
                let mut rows = (list.inner.read)();
                rows.retain(|row| key_fn(row) != row_key);
                list.set(cx, rows);
            });
            return;
        }
        let mut rows = (self.list.inner.read)();
        rows.retain(|row| (self.key)(row) != *key);
        self.list.set(cx, rows);
    }

    /// Whether `key` has a live guess (a row write or a structural patch).
    #[must_use]
    pub fn is_pending(&self, cx: &Cx<'_>, key: &K) -> bool {
        let node = self.list.node(cx);
        for patch in cx.guess_values::<ListPatch<T, K>, _>(&node) {
            if patch.removed.iter().any(|removed| removed == key)
                || patch.added.iter().any(|row| (self.key)(row) == *key)
            {
                return true;
            }
        }
        let row_node = self.list.inner.store.track(cx, &self.row_path(key));
        cx.guess_value::<T, _>(&row_node).is_some()
    }

    /// Write truth. Inside an action it lands when the action settles.
    pub fn commit(&self, cx: &Cx<'_>, rows: Vec<T>) {
        self.list.commit(cx, rows);
    }

    fn stage_patch(&self, cx: &Cx<'_>, patch: ListPatch<T, K>) -> bool {
        if !cx.in_action() {
            return false;
        }
        let node = self.list.node(cx);
        cx.stage_guess(&node, patch)
    }

    fn row_path(&self, key: &K) -> String {
        format!("{}#{}", self.list.path(), hash_key(key))
    }
}

struct FieldInner<T> {
    store: Rc<dyn StoreAccess>,
    path: String,
    read: ReadFn<T>,
    write: WriteFn<T>,
}

/// A typed handle to one store path.
pub struct Field<T> {
    inner: Rc<FieldInner<T>>,
}

impl<T> Clone for Field<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Rc::clone(&self.inner),
        }
    }
}

/// A read-only view of a store field.
pub struct ReadField<T> {
    field: Field<T>,
}

impl<T> Clone for ReadField<T> {
    fn clone(&self) -> Self {
        Self {
            field: self.field.clone(),
        }
    }
}

/// A write-only view of a store field.
pub struct WriteField<T> {
    field: Field<T>,
}

impl<T> Clone for WriteField<T> {
    fn clone(&self) -> Self {
        Self {
            field: self.field.clone(),
        }
    }
}

impl<T: Clone + 'static> Field<T> {
    /// A handle whose writes are per-action guesses.
    #[must_use]
    pub fn optimistic(&self) -> OptimisticField<T> {
        OptimisticField {
            field: self.clone(),
        }
    }

    /// A handle that can only read this field.
    #[must_use]
    pub fn read_only(&self) -> ReadField<T> {
        ReadField {
            field: self.clone(),
        }
    }

    /// A handle that can only write this field.
    #[must_use]
    pub fn write_only(&self) -> WriteField<T> {
        WriteField {
            field: self.clone(),
        }
    }

    /// Split the field into read and write capabilities.
    #[must_use]
    pub fn split(&self) -> (ReadField<T>, WriteField<T>) {
        (self.read_only(), self.write_only())
    }
}

impl<T: Clone + 'static> ReadField<T> {
    /// Read the value at this path.
    #[must_use]
    pub fn get(&self, cx: &Cx<'_>) -> T {
        self.field.get(cx)
    }

    /// Read the value, requiring an active tracked computation.
    #[must_use]
    pub fn get_strict(&self, cx: &Cx<'_>) -> T {
        self.field.get_strict(cx)
    }

    /// Read the value at this path without cloning.
    pub fn with<R>(&self, cx: &Cx<'_>, f: impl FnOnce(&T) -> R) -> R {
        self.field.with(cx, f)
    }
}

impl<T: Clone + 'static> WriteField<T> {
    /// Replace the value at this path.
    pub fn set(&self, cx: &Cx<'_>, value: T) {
        self.field.set(cx, value);
    }

    /// Update through a clone-modify-swap.
    pub fn update(&self, cx: &Cx<'_>, f: impl FnOnce(&mut T)) {
        self.field.update(cx, f);
    }
}

impl<T: Clone + 'static> Field<T> {
    /// Read the value at this path.
    ///
    /// In a display pass a live guess for this action shadows the committed
    /// value; derivations keep the committed value. Either way the path's own
    /// node is tracked, so a guess dirties exactly the readers of this field.
    #[must_use]
    pub fn get(&self, cx: &Cx<'_>) -> T {
        let node = self.node(cx);
        self.inner.store.note_read();
        if cx.is_display() {
            if let Some(guess) = cx.guess_value::<T, _>(&node) {
                return guess;
            }
        }
        (self.inner.read)()
    }

    /// The node a read of this path tracks.
    fn node(&self, cx: &Cx<'_>) -> NodeId {
        self.inner.store.track(cx, &self.inner.path)
    }

    /// Read the committed value, ignoring guesses and pending state.
    #[must_use]
    pub fn committed(&self, cx: &Cx<'_>) -> T {
        self.inner.store.track(cx, &self.inner.path);
        self.inner.store.note_read();
        (self.inner.read)()
    }

    /// Read the value, requiring an active tracked computation.
    ///
    /// Use this at effect/template edges where a silent untracked read would be
    /// a bug. Normal helpers use [`Field::get`].
    #[must_use]
    pub fn get_strict(&self, cx: &Cx<'_>) -> T {
        assert!(
            cx.is_tracking(),
            "strict store read outside a tracked computation at {}",
            self.inner.path
        );
        self.get(cx)
    }

    /// Read the value at this path without cloning.
    pub fn with<R>(&self, cx: &Cx<'_>, f: impl FnOnce(&T) -> R) -> R {
        let value = self.get(cx);
        f(&value)
    }

    /// Replace the value at this path.
    ///
    /// Inside an action this is a guess: the display shows it, the committed
    /// value is untouched, and the write lands when the action succeeds.
    pub fn set(&self, cx: &Cx<'_>, value: T) {
        if self.stage_guess(cx, &value) {
            return;
        }
        self.write_committed(cx, value, false);
    }

    /// Stage a committed write. Inside an action it lands at settle; outside
    /// it behaves like [`Field::set`].
    pub fn commit(&self, cx: &Cx<'_>, value: T) {
        if self.defer_commit(cx, value.clone()) {
            return;
        }
        self.write_committed(cx, value, false);
    }

    fn set_shallow(&self, cx: &Cx<'_>, value: T) {
        if self.stage_guess(cx, &value) {
            return;
        }
        self.write_committed(cx, value, true);
    }

    fn stage_guess(&self, cx: &Cx<'_>, value: &T) -> bool {
        if !cx.in_action() {
            return false;
        }
        let node = self.node(cx);
        if !cx.stage_guess(&node, value.clone()) {
            return false;
        }
        self.inner.store.note_write();
        self.defer_commit(cx, value.clone());
        true
    }

    fn defer_commit(&self, cx: &Cx<'_>, value: T) -> bool {
        let write = Rc::clone(&self.inner.write);
        let store = Rc::clone(&self.inner.store);
        let path = self.inner.path.clone();
        cx.defer_write(move |cx| {
            write(value);
            store.note_write();
            store.bump(cx, &path);
        })
    }

    fn write_committed(&self, cx: &Cx<'_>, value: T, shallow: bool) {
        (self.inner.write)(value);
        self.inner.store.note_write();
        if shallow {
            self.inner.store.bump_shallow(cx, &self.inner.path);
        } else {
            self.inner.store.bump(cx, &self.inner.path);
        }
    }

    /// Update through a clone-modify-swap.
    pub fn update(&self, cx: &Cx<'_>, f: impl FnOnce(&mut T)) {
        let mut value = self.get(cx);
        f(&mut value);
        self.set(cx, value);
    }

    /// Project one child field.
    pub fn project<U: 'static>(
        &self,
        key: &'static str,
        read: impl Fn(&T) -> U + 'static,
        write: impl Fn(&mut T, U) + 'static,
    ) -> Field<U> {
        let parent_read = Rc::clone(&self.inner.read);
        let parent_write = Rc::clone(&self.inner.write);
        let read = Rc::new(read);
        let write = Rc::new(write);
        let read_for_child = Rc::clone(&parent_read);
        let read_for_write = Rc::clone(&parent_read);
        let write_for_child = Rc::clone(&parent_write);
        let read_child = Rc::clone(&read);
        let write_child = Rc::clone(&write);
        Field {
            inner: Rc::new(FieldInner {
                store: Rc::clone(&self.inner.store),
                path: format!("{}.{}", self.inner.path, key),
                read: Rc::new(move || read_child(&read_for_child())),
                write: Rc::new(move |value| {
                    let mut parent_value = (read_for_write)();
                    write_child(&mut parent_value, value);
                    (write_for_child)(parent_value);
                }),
            }),
        }
    }

    /// Start a draft update. The write lands when the guard drops.
    #[must_use]
    pub fn draft(&self, runtime: &RuntimeHandle) -> Draft<T>
    where
        T: Clone,
    {
        let value = self.get(&runtime.cx());
        Draft {
            field: self.clone(),
            runtime: runtime.clone(),
            value,
        }
    }

    /// The store path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.inner.path
    }

    fn project_with<U: 'static>(
        &self,
        path: String,
        read: impl Fn(&T) -> U + 'static,
        write: impl Fn(&mut T, U) + 'static,
    ) -> Field<U> {
        let parent_read = Rc::clone(&self.inner.read);
        let parent_write = Rc::clone(&self.inner.write);
        let read = Rc::new(read);
        let write = Rc::new(write);
        let read_for_child = Rc::clone(&parent_read);
        let read_for_write = Rc::clone(&parent_read);
        let write_for_child = Rc::clone(&parent_write);
        let read_child = Rc::clone(&read);
        let write_child = Rc::clone(&write);
        Field {
            inner: Rc::new(FieldInner {
                store: Rc::clone(&self.inner.store),
                path,
                read: Rc::new(move || read_child(&read_for_child())),
                write: Rc::new(move |value| {
                    let mut parent_value = (read_for_write)();
                    write_child(&mut parent_value, value);
                    (write_for_child)(parent_value);
                }),
            }),
        }
    }
}

impl<T: Clone + 'static> Field<Vec<T>> {
    /// Number of elements.
    #[must_use]
    pub fn len(&self, cx: &Cx<'_>) -> usize {
        self.get(cx).len()
    }

    /// Whether the list is empty.
    #[must_use]
    pub fn is_empty(&self, cx: &Cx<'_>) -> bool {
        self.len(cx) == 0
    }

    /// A positional element handle.
    pub fn at(&self, index: usize) -> Field<T> {
        self.project_with(
            format!("{}[{}]", self.path(), index),
            move |list| list[index].clone(),
            move |list, value| list[index] = value,
        )
    }

    /// A keyed list whose writes are per-action patches.
    ///
    /// Row handles are keyed by `key`, so a guess follows its row across
    /// reorders; a read composes truth, live structural patches, and live row
    /// guesses per key.
    #[must_use]
    pub fn optimistic_list<K, F>(&self, key: F) -> OptimisticList<T, K>
    where
        K: Clone + Eq + Hash + 'static,
        F: Fn(&T) -> K + 'static,
    {
        OptimisticList {
            list: self.clone(),
            key: Rc::new(key),
            source: None,
            initialized: None,
        }
    }

    /// Keyed access. The key function is stored per operation; handles are
    /// keyed by the hash of the key so reordering does not retarget them.
    pub fn keyed<K, F>(&self, key: F) -> KeyedField<T, K>
    where
        K: Clone + Eq + Hash + 'static,
        F: Fn(&T) -> K + 'static,
    {
        KeyedField {
            list: self.clone(),
            key: Rc::new(key),
        }
    }

    /// Patch a keyed list, marking only changed keys and the list shape.
    pub fn patch_keyed<K, F>(&self, cx: &Cx<'_>, next: Vec<T>, key: F)
    where
        T: PartialEq,
        K: Clone + Eq + Hash + Debug + 'static,
        F: Fn(&T) -> K + 'static,
    {
        let old = self.get(cx);
        let old_by_key: HashMap<K, usize> = old
            .iter()
            .enumerate()
            .map(|(index, item)| (key(item), index))
            .collect();
        let old_keys: Vec<K> = old.iter().map(&key).collect();
        let next_keys: Vec<K> = next.iter().map(&key).collect();
        let order_changed = old_keys != next_keys;
        let mut changed = false;
        let mut changed_keys = Vec::new();
        let mut updated = Vec::with_capacity(next.len());
        for item in next {
            let item_key = key(&item);
            match old_by_key.get(&item_key) {
                Some(index) if old[*index] == item => {}
                Some(_) => {
                    changed = true;
                    changed_keys.push(item_key.clone());
                }
                None => {
                    changed = true;
                    changed_keys.push(item_key.clone());
                }
            }
            updated.push(item);
        }
        if !changed && !order_changed && old.len() == updated.len() {
            return;
        }
        self.set_shallow(cx, updated);
        if !changed_keys.is_empty() {
            let key = Rc::new(key);
            let keyed = self.keyed(move |item| key(item));
            for item_key in changed_keys {
                keyed.at_key_value(&item_key).update(cx, |_| ());
            }
        }
    }
}

/// A list field with stable key-based handles.
pub struct KeyedField<T, K> {
    list: Field<Vec<T>>,
    key: Rc<dyn Fn(&T) -> K>,
}

impl<T, K> Clone for KeyedField<T, K> {
    fn clone(&self) -> Self {
        Self {
            list: self.list.clone(),
            key: Rc::clone(&self.key),
        }
    }
}

impl<T: Clone + 'static, K: Clone + Eq + Hash + 'static> KeyedField<T, K> {
    /// The number of rows.
    #[must_use]
    pub fn len(&self, cx: &Cx<'_>) -> usize {
        self.list.len(cx)
    }

    /// A stable keyed row handle.
    pub fn at_key(&self, key: &K) -> Option<Field<T>> {
        let list = self.list.clone();
        let key_value = key.clone();
        let key_fn = self.key.clone();
        let key_fn_write = Rc::clone(&key_fn);
        let key_for_read = key_value.clone();
        let key_for_write = key_value.clone();
        let hash = hash_key(&key_value);
        Some(list.project_with(
            format!("{}#{}", list.path(), hash),
            move |rows| {
                rows.iter()
                    .find(|row| key_fn(row) == key_for_read)
                    .cloned()
                    .expect("keyed row")
            },
            move |rows, value| {
                // A row removed by another action is not resurrected by a
                // late settle; its write is simply dropped.
                if let Some(index) = rows
                    .iter()
                    .position(|row| key_fn_write(row) == key_for_write)
                {
                    rows[index] = value;
                }
            },
        ))
    }

    fn at_key_value(&self, key: &K) -> Field<T> {
        self.at_key(key).expect("keyed row")
    }

    /// Iterate keys and row handles.
    #[must_use]
    pub fn iter_keyed(&self, cx: &Cx<'_>) -> Vec<(K, Field<T>)> {
        self.list
            .get(cx)
            .iter()
            .map(|row| {
                let key = (self.key)(row);
                (key.clone(), self.at_key_value(&key))
            })
            .collect()
    }
}

/// A clone-modify-swap draft. Dropping the guard writes the value back.
pub struct Draft<T: Clone + 'static> {
    field: Field<T>,
    runtime: RuntimeHandle,
    value: T,
}

impl<T: Clone + 'static> std::ops::Deref for Draft<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<T: Clone + 'static> std::ops::DerefMut for Draft<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.value
    }
}

impl<T: Clone + 'static> Drop for Draft<T> {
    fn drop(&mut self) {
        let cx = self.runtime.cx();
        self.field.set(&cx, self.value.clone());
    }
}

/// A schema-less value for dynamic stores.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum DynamicValue {
    /// Null.
    #[default]
    Null,
    /// Boolean.
    Bool(bool),
    /// Number.
    Number(f64),
    /// String.
    String(String),
    /// List.
    List(Vec<DynamicValue>),
    /// Map.
    Map(BTreeMap<String, DynamicValue>),
}

impl DynamicValue {
    fn child(&self, key: &str) -> DynamicValue {
        match self {
            Self::Map(map) => map.get(key).cloned().unwrap_or_default(),
            _ => DynamicValue::Null,
        }
    }

    fn set_child(&mut self, key: &str, value: DynamicValue) {
        if !matches!(self, Self::Map(_)) {
            *self = Self::Map(BTreeMap::new());
        }
        if let Self::Map(map) = self {
            map.insert(key.to_string(), value);
        }
    }

    fn element(&self, index: usize) -> DynamicValue {
        match self {
            Self::List(list) => list.get(index).cloned().unwrap_or_default(),
            _ => DynamicValue::Null,
        }
    }

    fn set_element(&mut self, index: usize, value: DynamicValue) {
        if !matches!(self, Self::List(_)) {
            *self = Self::List(Vec::new());
        }
        if let Self::List(list) = self {
            if list.len() <= index {
                list.resize(index + 1, DynamicValue::Null);
            }
            list[index] = value;
        }
    }
}

impl Store<DynamicValue> {
    fn project_dynamic(
        &self,
        path: String,
        read: impl Fn(&DynamicValue) -> DynamicValue + 'static,
        write: impl Fn(&mut DynamicValue, DynamicValue) + 'static,
    ) -> Field<DynamicValue> {
        let inner = Rc::clone(&self.inner);
        let read = Rc::new(read);
        let write = Rc::new(write);
        let read_inner = Rc::clone(&inner);
        let write_inner = Rc::clone(&inner);
        Field {
            inner: Rc::new(FieldInner {
                store: inner,
                path,
                read: Rc::new(move || read(&read_inner.value.borrow())),
                write: Rc::new(move |value| {
                    write(&mut write_inner.value.borrow_mut(), value);
                }),
            }),
        }
    }

    /// A dynamic map field.
    #[must_use]
    pub fn field(&self, key: &str) -> Field<DynamicValue> {
        let key_for_read = key.to_string();
        let key_for_write = key.to_string();
        self.project_dynamic(
            key.to_string(),
            move |value| value.child(&key_for_read),
            move |value, next| value.set_child(&key_for_write, next),
        )
    }

    /// A dynamic list element.
    #[must_use]
    pub fn index(&self, index: usize) -> Field<DynamicValue> {
        self.project_dynamic(
            format!("[{index}]"),
            move |value| value.element(index),
            move |value, next| value.set_element(index, next),
        )
    }
}

impl Field<DynamicValue> {
    fn project_dynamic(
        &self,
        path: String,
        read: impl Fn(&DynamicValue) -> DynamicValue + 'static,
        write: impl Fn(&mut DynamicValue, DynamicValue) + 'static,
    ) -> Field<DynamicValue> {
        self.project_with(path, read, write)
    }

    /// A dynamic map field below this field.
    #[must_use]
    pub fn field(&self, key: &str) -> Field<DynamicValue> {
        let key_for_read = key.to_string();
        let key_for_write = key.to_string();
        self.project_dynamic(
            format!("{}.{}", self.path(), key),
            move |value| value.child(&key_for_read),
            move |value, next| value.set_child(&key_for_write, next),
        )
    }

    /// A dynamic list element below this field.
    #[must_use]
    pub fn index(&self, index: usize) -> Field<DynamicValue> {
        self.project_dynamic(
            format!("{}[{}]", self.path(), index),
            move |value| value.element(index),
            move |value, next| value.set_element(index, next),
        )
    }
}

/// A patch operation for a store value.
pub trait StorePatch<T> {
    /// Replace `next`, marking only changed field paths.
    fn patch(&self, cx: &Cx<'_>, next: T);
}

fn hash_key<K: Hash>(key: &K) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

/// Create a [`Store`].
///
/// `store!(cx, value)` is `Store::new(cx, value)`.
#[macro_export]
macro_rules! store {
    ($cx:expr, $value:expr $(,)?) => {
        $crate::Store::new($cx, $value)
    };
}
