# 03 — Store Layer

**Crates:** `reactive-stores`, `reactive-stores-macro`
**Depends on:** `reactive-core`
**Owns:** field-granular derived state, patches, keyed collections, and the
`#[derive(Store)]` / `#[derive(Patch)]` macros.

---

## 1. Why a store layer at all

A signal is a single value. A struct of signals gives field granularity but
changes the data's shape: every field becomes a handle, nesting becomes manual,
and plain Rust types cannot be passed around or serialized without conversion.

The store layer keeps the data shape and gives reactivity to paths:

```rust
#[derive(Store, Clone, Debug)]
struct AppState {
    user: User,
    todos: Vec<Todo>,
    theme: Theme,
}

let state = Store::new(AppState { ... });

// Reading a field tracks only that path.
state.user().name().with(|n| println!("{n}"));

// Writing a leaf marks that path and its ancestors, not siblings.
state.todos().at(0).done().set(true);
```

Solid achieves this with `Proxy` traps. Rust has no transparent proxy, so we use
the Leptos approach: a macro generates a **getter per field** returning a typed
handle (`Field<T>` or a `Subfield`) that carries a **path**. Each path maps to a
trigger node. Reads and writes go through the handle, which records the path.

This is a compile-time, monomorphized, zero-proxy design. The trade-off is that
dynamic shapes need an explicit dynamic store.

Code sketches in this document sometimes omit `cx` on reads/writes for brevity.
The real API takes `&Cx`: `field.set(cx, v)`, `field.read(cx)`,
`field.update(cx, |v| ...)`. There is no global graph.

## 2. Core types

```rust
pub struct Store<T> {
    inner: Arc<StoreInner<T>>,
}

struct StoreInner<T> {
    value: RwLock<T>,
    path: StorePath,           // usually ROOT
    triggers: Arc<TriggerMap>, // path -> NodeId
}
```

`Store<T>` is `Clone`. Cloning shares the data and triggers. A field handle is a
lighter path + reference:

```rust
pub struct Field<T> {
    owner: StoreHandle,        // shared store identity
    path: StorePath,
    _marker: PhantomData<T>,
}
```

`StorePath` is a small interned sequence of segments:

```rust
pub enum PathSegment {
    Field(&'static str),        // macro-generated, static for derived stores
    Index(usize),               // Vec / slice
    Key(Box<dyn Any + Send + Sync>), // HashMap/BTreeMap key
    Dynamic(Arc<str>),          // dynamic store
}
```

For macro-generated fields, `Field` is a static segment and the path can be
`SmallVec<[PathSegment; 4]>` or a precomputed interned ID. The macro can generate
a `const PATH_HASH` for a field chain, making trigger lookup a hash lookup or a
direct node handle stored in the field.

### 2.1 Trigger nodes

Each tracked path has a node:

- `value` trigger: notifies when the value at this path changes;
- `keys` trigger: notifies when the set of keys/indexes changes;
- `len` trigger: notifies when the length changes;
- optional `presence` trigger for `Option`/map membership.

Why separate: a list's row value can change without its length changing, and a
length change should not re-run every row's value binding. Solid's store
machinery makes the same distinction; Leptos models it with `StoreFieldTrigger`
and `KeyMap` (`reactive_stores/src/keyed.rs`, `path.rs`).

A write to a leaf:

1. Marks the leaf's `value` trigger dirty.
2. Marks ancestors' `value` triggers dirty (a parent struct value changed).
3. If the write changes the shape (insert/remove/key), marks the parent's
   `keys`/`len` triggers.
4. Does **not** mark siblings.

This is the "sibling isolation" property.

## 3. The derive macros

### 3.1 `#[derive(Store)]`

For a struct:

```rust
#[derive(Store)]
struct User {
    name: String,
    age: u32,
}
```

Generate:

```rust
trait UserStoreFields {
    fn name(&self) -> Field<String>;
    fn age(&self) -> Field<u32>;
}
```

The getters are implemented on `Store<User>`, `Field<User>`, and `Subfield<...>`
via a trait, so they compose at any depth. The macro also generates:

- `Store::new(value)`, `Store::from_parts(...)` for tests;
- a `path()` implementation for each field;
- `read()`, `write()`, `with()` convenience methods on `Field<T>`;
- an `update(|&mut T| ...)` that marks the path on drop.

For enums:

```rust
#[derive(Store)]
enum Status { Idle, Loading, Ready(Data), Failed(String) }
```

Generate variant accessors (`as_loading()`, `as_ready()`) returning
`Option<Field<...>>`; changing the variant marks the enum's value and tag
triggers.

**Foreign and opaque values.** The macro only tracks fields it understands:
primitives, `String`, collections with adapters, and nested types that derive
`Store`. Any other field is a **leaf**: a read tracks the path, a write through
the field marks it, but mutation *inside* the value is invisible. A `RefCell`, a
channel sender, an actor handle, or a third-party object that manages its own
state must not be held expecting fine-grained updates. Either keep it out of the
store, or treat it as a leaf and write it back through the field (`field.set`)
after mutating it. Solid 2.0's equivalent note is the class instance that runs
methods on the store proxy; Rust has no proxy, so the boundary is the type, not
the syntax.

### 3.2 `#[derive(Patch)]`

`patch(new_value)` replaces a value but only notifies changed fields:

```rust
#[derive(Store, Patch)]
struct Todo { title: String, done: bool }

store.patch(Todo { title: "new".into(), done: true });
```

The derive compares each field (`PartialEq`) and marks only the changed paths.
For `Vec`, the default patch is positional; a keyed form uses a key function:

```rust
store.todos().patch_keyed(new_rows, |t| t.id);
```

Keyed patch:

1. Build a map of old keys → indices.
2. For each new row: if key exists, patch in place; else insert.
3. Remove missing keys.
4. Mark the list's `keys`/`len` triggers; mark only changed rows.

This is the `reconcile` idea from Solid (`documentation/solid-2.0/04-stores.md`,
`packages/signals/src/store/reconcile.ts`) expressed as a Rust trait.

### 3.3 Draft updates

Solid's store setters take a draft callback. Rust cannot hand out a mutable proxy
cheaply without unsafe or a full clone. Two supported forms:

```rust
// Leaf write: no clone.
store.user().name().set("Ada".into());

// Whole-value update: clone-modify-swap, with optional patch.
store.user().update(|user| { user.age += 1; });
```

`update` clones `T` (requires `T: Clone`), applies the closure, then diffs or
replaces. For large values, prefer leaf writes or `patch`. A future `Draft<T>`
guard can mutate in place behind a `RwLock` write guard and mark on drop; that is
the closest safe analogue to Solid's draft, and it is the recommended v2
addition.

Why not hand out `&mut T` directly? Because a long-lived guard blocks all reads
of the store while the user holds it, and a panic or early return must still mark
the right paths. A guard with a recorded set of writes is safer. For v1,
clone-modify-swap is predictable.

## 4. Keyed collections

```rust
pub trait KeyedCollection {
    type Key: Clone + Eq + Hash;
    type Value;
    fn key(&self, index: usize) -> Self::Key;
    fn at_key(&self, key: &Self::Key) -> Option<&Self::Value>;
}
```

Generated for `Vec<T>`, `HashMap<K, V>`, `BTreeMap<K, V>`, and slices.

Handles:

```rust
state.todos().len()                 // Field<usize>, tracks keys/len
state.todos().at(3)                 // Field<Todo>, tracks index 3
state.todos().at_key(&id)           // Option<Field<Todo>>
state.todos().iter_keyed()          // iterator of (Key, Field<Todo>)
```

The view layer's `For` uses `iter_keyed()` to mount rows and a keyed diff to
reorder. The store does not own the view rows; it only exposes stable keys and
per-key fields.

Reordering a `Vec` is the hard case: indices change, so `at(3)` would point at a
different element after a move. Two modes:

- **Index mode** (`Vec` as a list): `at(i)` tracks positional index. Reordering
  notifies the affected index paths. Use when position is the identity.
- **Keyed mode** (`KeyedList<K, V>` or `Vec<T>` with a key function): row handles
  are keyed by `K`; reordering moves nodes but does not change which key each
  handle points at. This is what `For` uses by default when a key is supplied.

Provide a `KeyedVec<T, K>` wrapper for apps that want stable keys with `Vec`
storage. It keeps a `HashMap<K, usize>` index and a generation per key so a
removed key's handle cannot alias a reused index.

## 5. Dynamic stores

For JSON-like or schema-less data, the macro path is unavailable. Provide:

```rust
pub struct DynamicStore {
    value: RwLock<Value>,
    paths: RwLock<PathInterner>,
    nodes: RwLock<Vec<NodeId>>,          // indexed by PathId
}

pub struct PathInterner {
    by_key: HashMap<PathKey, PathId>,    // canonical path -> dense id
    entries: Vec<PathEntry>,             // PathId -> segments + hash
}

impl DynamicStore {
    pub fn get(&self, path: &[PathSegment]) -> DynamicField;
    pub fn set(&self, path: &[PathSegment], value: Value);
}
```

**What "interned path map" means.** A dynamic path is a runtime sequence such as
`["users", Key(42), "name"]`. Hashing that sequence on every read/write is
expensive and allocates if you build a `Vec` each time. An interner assigns each
unique, canonical path a dense integer:

```
["users", Key(42), "name"]
  → canonical PathKey (interned segments)
  → PathId(17)                 // u32, dense
  → nodes[17] = NodeId(...)    // trigger node
```

- `PathKey` is a canonical sequence of interned segments. Segment strings and
  keys are interned once; equality and hashing are cheap.
- `by_key` is consulted only on the first access (or when a new segment
  combination appears). After that, a field handle can cache its `PathId` and
  reads/writes are an index into `nodes`.
- `nodes` is a dense `Vec<NodeId>`, so node lookup is O(1) and cache-friendly.
- Prefixes can share structure: a trie is an optional refinement for enumerating
  children or invalidating a subtree.

**Costs.** The interner is a hash table plus canonical segment storage; it grows
with the number of distinct paths. It must be bounded or reference-counted by
live `DynamicField` handles, otherwise a long-lived app that generates arbitrary
paths leaks. Keys must be `Hash + Eq + 'static` or be interned to `u32`/`Arc<str>`.
The first access pays canonicalization; subsequent accesses are cheap.

**Alternatives:** a per-access `HashMap<Vec<PathSegment>, NodeId>` (simple, but
allocates and hashes a long key on every access); a trie (good for prefixes and
subtree invalidation, more pointer chasing); string paths (easy, slow); static
macro-generated paths (fastest, but not dynamic).

**v1 decision:** the static `#[derive(Store)]` path is v1; `DynamicStore` with
interning is v2 unless a real app needs it. Static stores avoid the question
entirely: the macro generates a const field index/path, so there is no runtime
interning or hash lookup.

**Leptos comparison.** Leptos is already closer to this than Solid's Proxy
stores:

- `StorePath` is a `Vec<StorePathSegment>`; a segment is a per-store interned
  `usize` for a field or key.
- Each `ArcStore` has a `TriggerMap` (`FxHashMap<StorePath, StoreFieldTrigger>`)
  holding `this` and `children` triggers per path.
- Keyed fields map a key to a stable segment via `FieldKeys`, so a keyed path is
  stable across reorder.
- Writes collect the path's triggers plus ancestors' `children` triggers and
  notify from the root down (order matters for `Option::unwrap`/`Show`).
- `Patch`/`reconcile` provide keyed diffing.

So Leptos has runtime paths and a path→trigger map, but:

- segments are field/key indices, not arbitrary runtime values;
- the full path is a `Vec<usize>` hashed on lookup, not a dense `PathId`;
- the store is typed (`Store<T>`, `Field<T>`), not a dynamic `Value` tree;
- there is no ergonomic arbitrary-path API; the macro is the intended surface.

Our `DynamicStore` is the same idea with full-path interning, a dense node array,
and untyped values for plugins/config. It is an evolution of Leptos' path model,
not a replacement for the typed store. If we build it, reuse the segment
interning and trigger/ancestor ordering rules from Leptos rather than inventing
a second path system.

**Fallback design.** If full-path interning or untyped values prove too complex
during the prototype, fall back to the Leptos shape: `StorePath` of interned
`usize` segments and a `HashMap<StorePath, Trigger>` per store. That is simpler
and already proven; it gives up the dense `PathId` fast path and arbitrary key
types, but it keeps the dynamic API and the typed store unchanged.

- Reads/writes go through `DynamicField`, which resolves the path each time.
- Cost: hash lookup (first) or `PathId` index (after) + type check; no
  compile-time field checking.
- Use for plugins, inspectors, and configuration editors, not for app state.

## 6. Async derived stores and projections (v2)

Solid's `createProjection` / `createStore(fn)` are the store analogue of
`Memo`/writable derived signal. The key idea: a derive function receives a draft
and mutates it; the result is reconciled into the store, preserving keyed
identity.

In Rust:

```rust
pub struct Projection<T> { /* store + derive memo */ }

impl<T: Patch + Clone> Projection<T> {
    pub fn new(derive: impl FnMut(&mut T) + 'static, seed: T) -> Self;
}
```

- The derive runs as a memo computation; writes to the draft are applied to a
  shadow store at the end of the pass.
- A keyed return value is reconciled with `Patch` semantics.
- Async derives are `create_async_memo` with a seed, returning `Memo<Store<T>>`;
  reads before the first answer see the seed or are pending, depending on options.
- Do not catch `NotReady` inside a derive: it is how the projection learns to
  retry (Solid #3073; the same rule applies here).

This is deferred to v2 because it depends on the full async hold model. The
store layer in v1 supports `Memo<Vec<T>>` and explicit `LoadingBoundary` at
the view level, which covers most list/loading cases.

## 7. Interaction with the graph

- A store leaf is a graph signal. Path triggers are ordinary nodes with a
  `Signal`-like value (the leaf value) or a structural value (keys/len).
- A read of `Field<T>::with(|v| ...)` links the caller to the path's value node.
- A read of `len()` links to the len node; a read of keys links to the keys node.
- A write goes through `RwLock<T>` + the path's node. The write stages on the
  node; the store's in-memory value is updated at flush (or immediately with
  `write_now` for internal code). This keeps `F-10` true for stores as well:
  a store read before the flush sees the committed value.
- Structural writes (insert/remove) update the store's container immediately but
  stage the node changes; a reader before flush sees the old structure. This
  requires a committed snapshot or a copy-on-write container. For `Vec`, a
  clone-on-write at flush is acceptable; for large lists, use an immutable
  `im::Vector` or a persistent vector for cheap snapshots. This is a design
  decision for Phase 7; see below.

### 7.1 The structural snapshot problem

If a write mutates the `Vec` in place, a read between the write and the flush
sees the new structure even though `F-10` says it should see the old. Options:

1. **Copy-on-write**: clone the container at write time, mutate the clone, swap
   at flush. Cost: O(n) per structural write. Fine for small lists, poor for
   10k rows.
2. **Persistent collections**: use `im::Vector`, `rpds`, or an arena-backed
   list so a snapshot is O(1). Cost: dependency and API differences.
3. **Stage operation log**: record insert/remove operations, apply at flush,
   and serve reads from the old container until then. Cost: reads must replay
   the log or the view sees stale structure; complexity.
4. **Relax `F-10` for structural writes**: document that structure changes
   immediately (but values stage). This is a real divergence from Solid and
   would surprise users.

Recommendation: use `im::Vector` or an internal persistent vector for large
lists, and copy-on-write for small `Vec`s. This preserves `F-10` without a
special case. Decide in Phase 7 with a benchmark; until then, use
copy-on-write and document the O(n) cost.

## 8. API sketch

```rust
#[derive(Store, Patch, Clone, Debug, Default)]
struct AppState {
    user: User,
    todos: Vec<Todo>,
    theme: Theme,
}

let state = Store::new(AppState::default());

// Field access
state.user().name().set("Ada".into());
let name = state.user().name().get();       // Clone
state.todos().at(0).done().set(true);
state.todos().push(Todo::default());        // structural write, staged

// Iteration
for (key, todo) in state.todos().iter_keyed(|t| t.id) {
    // key is stable; todo is a Field<Todo>
}

// Whole-value patch
state.patch(AppState { theme: Theme::Dark, ..state.snapshot() });

// Dynamic
let dyn_store = DynamicStore::new(json!({ "a": 1 }));
dyn_store.get(&["a".into()]).set(json!(2));
```

## 9. Trade-offs and alternatives

| Option | Granularity | Ergonomics | Cost |
|---|---|---|---|
| Struct of signals | field | manual, shape changes | none |
| `#[derive(Store)]` (chosen) | field / key | good, macro | compile time, path bookkeeping |
| Proxy emulation | arbitrary | closest to Solid | unsafe or runtime interception, dynamic overhead |
| Snapshot + memo | whole value | simplest | recompute whole subtree |
| Immer-style draft | field | good | clone or persistent container |

The store layer is opt-in. Flat state can use a `create_signal()` pair directly; a single
struct with a handful of fields does not need a store.

## 10. Tests

- Sibling isolation: write `user.name`, assert a binding on `user.age` does not
  run.
- Nested composition: `store.a().b().c()` tracks the leaf path; writing `a.b`
  notifies `c` but not `a.d`.
- Keyed patch: insert/remove/reorder; only changed keys run their bindings;
  row state (focus, scroll) survives a reorder.
- Keyed handle safety: removing a key and reinserting it gives a new generation;
  an old handle does not alias the new row.
- Structural `F-10`: read between write and flush sees the old structure; after
  flush sees the new.
- Draft/update: `update` marks only the changed leaf paths when using `Patch`.
- Dynamic store: path interning, concurrent reads, missing paths.
- Macro hygiene: modules, generics, visibility, `Option`, `Vec`, `HashMap`,
  enums.
- Compile-fail tests: wrong field, wrong type, non-`Patch` patch.

## 11. Open questions

1. **Persistent vector vs copy-on-write.** Decide in Phase 7 with a 10k-row
   benchmark. If we add `im`, it becomes a public dependency; if we hide it, we
   own the implementation.
2. **Draft API.** Is clone-modify-swap enough for v1, or do we need a `Draft`
   guard? (Leaning: clone-modify-swap for v1, draft in v2.)
3. **Field API naming.** `state.user().name()` vs `state.user.name()`. The
   method-call form is Rust-idiomatic and composes with `Field`; keep it.
4. **Dynamic store in v1.** Probably defer; a config editor can use a `create_signal()` pair
   and rebuild bindings.
5. **`Patch` for arbitrary types.** Require `PartialEq`; provide a manual
   `Patch` trait for types that cannot derive it.
6. **Async projections.** v2, with the hold model. v1 uses async memos over
   `Vec<T>`.
