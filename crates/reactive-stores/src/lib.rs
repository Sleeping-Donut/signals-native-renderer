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

use reactive_core::{create_signal, Cx, OwnerHandle, ReadSignal, RuntimeHandle, WriteSignal};

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
    fn track(&self, cx: &Cx<'_>, path: &str);
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
    fn track(&self, cx: &Cx<'_>, path: &str) {
        let trigger = self.trigger(cx, path);
        trigger.0.get(cx);
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
    #[must_use]
    pub fn get(&self, cx: &Cx<'_>) -> T {
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
    pub fn set(&self, cx: &Cx<'_>, value: T) {
        (self.inner.write)(value);
        self.inner.store.note_write();
        self.inner.store.bump(cx, &self.inner.path);
    }

    fn set_shallow(&self, cx: &Cx<'_>, value: T) {
        (self.inner.write)(value);
        self.inner.store.note_write();
        self.inner.store.bump_shallow(cx, &self.inner.path);
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
                let index = rows
                    .iter()
                    .position(|row| key_fn_write(row) == key_for_write)
                    .expect("keyed row");
                rows[index] = value;
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
