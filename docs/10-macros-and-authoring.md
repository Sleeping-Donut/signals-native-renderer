# 10 — Macros and Authoring

**Crates:** `macros` (proc macros), `prelude`
**Owns:** `view!`, `#[component]`, props, control-flow helpers, store derives,
diagnostics, and the authoring mental model.

---

## 1. The authoring model: mount once, bind forever

The most important thing to teach users:

> A component runs when it is mounted. Dynamic values are expressions inside the
> tree, not a reason to re-run the component.

This is different from React/Solid/Leptos/GPUI. It follows from the retained tree:
if the component re-ran, it would rebuild structure and lose identity. Instead,
the `view!` macro compiles dynamic expressions into **bindings**.

A reader coming from Solid expects:

```tsx
function Counter() {
  const [count, setCount] = createSignal(0);
  return <button onClick={() => setCount(count() + 1)}>{count()}</button>;
}
```

In our model:

```rust
#[component]
fn Counter() -> impl View {
    let count = create_signal(cx, 0);
    view! {
        <button on_click=move |cx, _| count.update(cx, |c| *c += 1)>
            {count}
        </button>
    }
}
```

- `count` is created once at mount.
- `on_click` is a listener registered once.
- `{count}` compiles to a text binding that subscribes to `count`.
- The button element is never rebuilt.

For users, the syntax looks similar; the semantics are more like Solid's compiled
output than its component function.

**Conventions in this document.** Reactive primitives use Solid/Leptos-style
`create_*` functions:

```rust
create_signal(cx, value)                 // plain source
create_derived_signal(cx, |cx, prev| ...)// writable derived
create_async_signal(cx, |cx| ...)        // writable derived, async
create_memo(cx, |cx, prev| ...)          // read-only derived, sync
create_async_memo(cx, |cx| ...)          // read-only derived, async
create_effect(cx, compute, apply)
create_render_effect(cx, compute, apply)
create_store(cx, value)
create_view_slot(cx, key, render)
```

They are thin wrappers over `Signal::new`, `Memo::new`, etc., and take `&Cx`.
The type constructors remain available for generic/library code; examples use the
`create_*` surface because it is easier to scan. All reactive operations take
`&Cx`; code sketches sometimes omit `cx` where the surrounding `view!` macro
supplies it, but the real API takes it.

Tags are native element kinds, not HTML. `div` is a generic box/stack and
`span` is inline text; they do not imply a DOM. The builder form uses
`ElementKind::Box`, `ElementKind::Text`, `ElementKind::Button`, etc.

## 2. `view!`

### 2.1 Syntax

```rust
view! {
    <div class="app" style:padding="8px">
        <Header title="Todos" />
        <Show when=move || !todos.is_empty() fallback=|| view! { <Empty /> }>
            <For each=todos key=|t| t.id children=|t| view! { <Row todo=t /> } />
        </Show>
        <button on_click=move |cx, _| add_todo(cx)>"Add"</button>
    </div>
}
```

- Tags map to `Element` builders; uppercase tags map to components.
- `attr=value` is an attribute.
- `style:name=value` is a style property.
- `class:name=bool` is a class toggle.
- `on_event=closure` is a listener.
- `{expr}` is a child expression (text or view).
- `if`/`for`/`match` are Rust control flow; the macro rewrites them into
  `Show`/`For`/`Switch` nodes to preserve identity.
- `..` spread of props is supported for components.

### 2.2 Expansion sketch

```rust
// view! { <button on_click=f>{count}</button> }
{
    let button = cx.element(ElementKind::Button);
    cx.set_style(button, Style::default());
    let text = cx.text("");
    cx.add_binding(text, create_render_effect(
        cx,
        move |cx| count.get(cx).to_string(),
        move |cx, s| cx.set_text(text, s),
    ));
    cx.insert(button, text, None);
    cx.on_click(button, f);
    button
}
```

The macro does not create a closure that returns a tree. It emits builder calls
in the current mount scope. Dynamic expressions become bindings; static values
are set once.

### 2.3 Reactivity of attributes

`<div title={name}>`:

- If `name` is a signal read, it becomes a binding.
- If it is a plain value, it is set once.
- The macro detects `Signal`/`Memo`/`Field` types syntactically where possible; for
  arbitrary expressions, it uses the `IntoReactive<T>` trait to decide at compile
  time.

To avoid ambiguity, the macro treats:

- `expr` whose type implements `ReactiveValue<T>` as a binding;
- a plain value as a constant;
- `move || expr` as a binding (the closure is the compute function).

Users can force a binding with `{move || ...}` or a constant with
`untrack(|| ...)`.

### 2.4 Conditional children

`if cond { a } else { b }` inside `view!` becomes a `Show` node:

```rust
let show = cx.conditional(
    move || cond,
    || { /* mount a */ },
    Some(|| { /* mount b */ }),
);
```

Only the active branch is mounted. The conditional node stays in place.

### 2.5 List children

`for item in items { row(item) }` becomes a `For` node. The macro requires a key
for a retained list; if omitted, it uses the item's index and emits a dev warning
(`UNKEYED_LIST`) because index keying destroys row state on reorder.

## 3. Components

### 3.1 `#[component]`

```rust
#[component]
fn Counter(initial: Prop<i32>, step: Prop<i32>) -> impl View {
    let count = create_signal(cx, initial.get_untracked(cx));
    view! { ... }
}
```

Generates:

```rust
struct CounterProps {
    initial: Prop<i32>,
    step: Prop<i32>,
}

fn Counter(props: CounterProps, cx: &mut Cx) -> ViewNodeId { ... }
```

The component is a mount function. It runs once. `impl View` is erased to
`ViewNodeId` by the macro; a component that returns an element or a fragment is
also valid.

### 3.2 Props

Props are uniform `Prop<T>` values. There is no bare `T` prop if the value may
change, because a bare `T` is a snapshot with no accessor and therefore no way to
be reactive:

```rust
pub enum Prop<T> {
    Static(T),
    Reactive(Signal<T>),
    Derived(Memo<T>),
}

impl<T: Clone> Prop<T> {
    pub fn get(&self, cx: &Cx) -> T;
    pub fn with<R>(&self, cx: &Cx, f: impl FnOnce(&T) -> R) -> R;
}
```

```rust
#[component]
fn Label(text: Prop<String>) -> impl View {
    view! { <span>{text}</span> }
}
```

Passing:

```rust
view! {
    <Label text=title />              // Signal<String> -> Prop::Reactive
    <Label text="Static" />           // literal -> Prop::Static("Static".into())
    <Label text=move || format!("{first} {last}") /> // closure -> Prop::Derived(memo)
}
```

The macro decides syntactically:

- a closure `move || ...` becomes a `Memo<T>` wrapped in `Prop::Derived`;
- an expression whose type is `Signal<T>` or `Memo<T>` becomes
  `Prop::Reactive`/`Prop::Derived`;
- any other expression becomes `Prop::Static`.

`Prop::Static` allocates no graph node and creates no edge. `Reactive` and
`Derived` subscribe normally. This is the Rust answer to Solid's prop getter:
reading `props.text` in Solid is always a lazy accessor; `Prop<T>` makes that
explicit without forcing a node for static values.

We do **not** use a blanket `impl<T> IntoProp<T> for T`: in Rust it conflicts
with `impl<T> IntoProp<T> for Signal<T>` (the compiler cannot rule out
`T = Signal<U>`). The syntactic rule avoids coherence problems and gives better
error messages.

#### Props design tradeoffs

| Design | Update granularity | Ergonomics | Cost |
|---|---|---|---|
| Plain `T` only | None; child must be replaced | Simplest signatures | Unusable for live props; footgun |
| `Signal<T>` only | Fine-grained | Uniform; macro wraps statics | Every static prop allocates a stored signal/node unless `Signal` has a constant variant |
| **`Prop<T>` enum (chosen)** | Fine-grained for `Reactive`/`Derived`; none for `Static` | Uniform signatures; macro chooses variant | One match per read; enum size |
| `Fn() -> T` derived closure | Fine-grained, uncached | Flexible | Recomputed at every read; no memoization |
| `RwSignal<T>`/`ReadSignal<T>` split | Fine-grained | Clear read/write split | More types; write access may be too broad |

**Leptos comparison.** Leptos uses:

- `Signal<T>` — a type-erased enum of read signal, memo, derived closure, or
  stored value; `impl From<T> for Signal<T>` wraps a static value in
  `Signal::stored`;
- `MaybeSignal<T>` — `Static(T)` or a reactive variant, the closest analogue to
  `Prop<T>`; `impl From<T>` produces `Static` with no signal node;
- `MaybeProp<T>` — optional prop handling;
- `#[prop(into)]` — call-site conversion into the prop type;
- `Callback<In, Out>` and `Children` — events and slots.

Leptos' `MaybeSignal<T>` is exactly the "static or reactive" enum. Its
`#[prop(into)]` keeps call sites clean. Our `Prop<T>` is the same shape:
`Static(T)` is zero-node; `Reactive`/`Derived` carry handles. This avoids both
the bare-`T` footgun and the per-static-prop node cost of a uniform `Signal<T>`.

### 3.3 Prop identity

A reactive prop is a handle. If the parent passes a stable `Signal`/`Memo`, the
child's binding subscribes directly; the child is not replaced. If the parent
creates a new `Memo` each time it mounts, that is fine (mount happens once). If
the parent wants to change the prop later, it writes to the signal it passed; it
does not re-create the child. A `Static` prop never updates; that is explicit and
zero-cost.

This is the key ergonomic difference from GPUI: props are not re-supplied every
frame; they are `Prop<T>` values that live as long as the child.

### 3.4 Children

`children` is a `ViewFn` or a `Children` handle:

```rust
#[component]
fn Card(children: Children) -> impl View {
    view! { <div class="card">{children}</div> }
}
```

`Children` is a closure that mounts the child subtree when called. It is called
once during the parent's mount. For a child that can change structurally, pass a
`ViewFn` and wrap it in a `Show`/`ViewSlot`.

## 4. Control-flow helpers

### 4.1 `Show`

```rust
<Show when=move || cond fallback=|| view! { <Empty /> }>
    <Content />
</Show>
```

- `when` is a `Fn() -> bool` (or a `Memo<bool>`).
- `fallback` is optional.
- The child is a `ViewFn` and is mounted only when true.
- Flipping disposes one branch and mounts the other.
- `keep_alive=true` (v1.1) caches the inactive branch.

### 4.2 `For`

```rust
<For each=items key=|item| item.id children=|item| view! { <Row item=item /> } />
```

- `each` is `Memo<Vec<T>>` or a signal read.
- `key` returns a stable `Key`.
- `children` mounts a row given the item.
- The keyed diff is in [07-retained-renderer.md](07-retained-renderer.md).
- `each` can be a store's `iter_keyed()`; the row receives a `Field<T>`.

### 4.3 `Switch` / `Match`

```rust
<Switch>
    <Match when=move || state.is_loading()> <Spinner /> </Match>
    <Match when=move || state.is_error()> <ErrorView /> </Match>
    <Match when=move || true> <Content /> </Match>
</Switch>
```

Only the first matching branch is mounted. Branches are keyed by their position;
use explicit keys if the order can change.

### 4.4 `KeepAlive` (v1.1)

```rust
<KeepAlive when=move || tab.is_settings()>
    <Settings />
</KeepAlive>
```

Caches the subtree owner and view nodes when inactive; re-inserts on return. Use
for tabs, navigation stacks, and editors where local state matters.

## 5. Store derives

### 5.1 `#[derive(Store)]`

```rust
#[derive(Store, Clone, Debug, Default)]
struct AppState {
    user: User,
    todos: Vec<Todo>,
}

let state = create_store(cx, AppState::default());
view! {
    <span>{state.user().name()}</span>
    <For each=state.todos() key=|t| t.id children=|t| view! { <Row todo=t /> } />
}
```

Generated field methods return `Field<T>`; they are `Copy`/`Clone` handles.
See [03-store-layer.md](03-store-layer.md).

### 5.2 `#[derive(Patch)]`

```rust
#[derive(Store, Patch, Clone, PartialEq)]
struct Todo { id: u64, title: String, done: bool }

state.todos().at(0).patch(Todo { done: true, ..todo.snapshot() });
```

Only changed fields notify.

### 5.3 Dynamic stores

`DynamicStore` is opt-in and not macro-generated. Use it for plugins/config, not
app state.

## 6. Diagnostics

Authoring mistakes are common in a mount-once model. The framework should catch
them early:

| Code | When | Severity |
|---|---|---|
| `STRICT_READ_IN_MOUNT` | A signal read at component-body top level without `untrack`/binding. | Warning (dev). |
| `WRITE_IN_COMPUTE` | A write during a compute pass. | Error (dev), staged (release). |
| `WRITE_IN_APPLY` | A write during an apply without `owned_write`. | Error (dev). |
| `DISPOSED_HANDLE` | Reading/writing a disposed handle. | Error (dev), no-op (release). |
| `UNKEYED_LIST` | `For` without a key. | Warning (dev). |
| `DUPLICATE_KEY` | Two rows with the same key. | Error (dev). |
| `MOUNTED_DURING_APPLY` | A structural mount during apply. | Warning; deferred. |
| `LEAKED_EFFECT` | A detached effect with no owner. | Info (dev), optional. |
| `A11Y_NO_LABEL` | An interactive node without a label. | Warning (dev, a11y enabled). |
| `CONTEXT_MISSING` | `expect_context` found nothing. | Error. |

Diagnostics carry the owner path and source location (from `#[track_caller]` and
macro spans). In release, only the ones that affect correctness remain, and they
log once.

### 6.1 The strict-read rule

Reading a signal at the top level of a mount body is almost always a mistake: it
takes a snapshot and will not update. The macro cannot always distinguish a
deliberate snapshot from a mistake, so it warns unless the read is:

- inside `untrack(...)`;
- inside a binding (`{...}` child, reactive attribute, closure);
- inside a memo/effect compute.

This mirrors Solid 2.0's strict top-level access rule
(`documentation/solid-2.0/01-reactivity-batching-effects.md`).

## 7. Escape hatches

### 7.1 `untrack`

```rust
let initial = untrack(cx, || count.get(cx));
```

Explicit snapshot. Use for initial values, one-time setup, and measuring.

### 7.2 View slots (and `Rebuild` sugar)

```rust
// A stable host whose active child is replaced when `key` changes.
create_view_slot(
    cx,
    key,
    move |cx| view! { <ExpensiveForm state=state /> },
)
```

`ViewSlot` is a stable host node that owns one active child subtree. When the key
changes, the old child owner is disposed and a new subtree is mounted under the
same host. `Rebuild::new(closure)` is sugar for a `ViewSlot` whose key is the
closure's tracked dependencies: it re-evaluates the closure and replaces the
subtree.

**It is not the base of the control-flow components.** This is the important
distinction:

- Solid's `<Show>`, `<Switch>`, and `<For>` are specialized control-flow
  primitives. They do not go through `<Dynamic>`/`dynamic()`. Solid's
  `dynamic()` is specifically for switching **which component or tag** is
  rendered at a call site.
- Leptos has no `Dynamic` component; its control-flow components are specialized
  Tachys views, and dynamic erasure is handled by `AnyView`/`Either`.
- Our `Show`, `Switch`, and `For` are specialized nodes too. They are the public
  API and do not require a `ViewSlot`.
- `ViewSlot` is a sibling primitive for the generic case: render an arbitrary
  view-returning closure, swap a component type, or host a plugin UI whose whole
  subtree is chosen at runtime. A Solid-style `dynamic()` helper would be a thin
  wrapper over it; it is not the mechanism behind `Show`/`Switch`.

**Why not only control-flow components?** Because control flow encodes a finite
shape: a boolean, a branch set, a list. It cannot express "render whatever view
this closure returns" or "switch between arbitrary component types" without a
generic host. A `ViewSlot` is that host. It is also what `Rebuild` uses.

**Why not just `AnyView`/`Either`?** They are type-erasure tools, not lifecycle
hosts:

| Mechanism | What it does | What it lacks |
|---|---|---|
| `AnyView` | boxes/erases a concrete view type | no stable host/key; allocating a new box does not define when/whether to replace the mounted subtree |
| `Either<A, B>` | compile-time two-branch view type | fixed alternatives; cannot hold an arbitrary runtime view without erasure |
| `ViewSlot` | stable host node + keyed child replacement | an extra host node; child state is lost on replacement |

`ViewSlot` can hold an `AnyView`, an `Either`, or a concrete view type; the
erasure choice is orthogonal. The slot supplies the identity, key, ownership, and
replacement semantics that `AnyView`/`Either` do not carry. Tachys-style
control-flow views hide that replacement inside the view's own state; in a
retained tree the host node is the explicit equivalent of the DOM marker.

**None of this is renderer-specific.** Control-flow nodes live in the shared
`view` crate over a `Backend`/`ViewTree` interface; a renderer only implements
node creation, insertion/removal, layout, and paint. Solid can keep `Show`/`For`
in the `solid` package because they return renderer-owned `Element` values and let
the renderer's insert path mount them; Tachys keeps its control flow generic over
`Renderer`. We do the same: one control-flow implementation, many backends. A
third-party renderer does not reimplement `Show`/`For`/`ViewSlot`; it implements
the low-level operations they call.

**Use control flow first.** `Show`/`Switch`/`For` express the common shapes and
should be the default API. Reach for a `ViewSlot` for dynamic component/view
switching, plugin UIs, or whole-subtree replacement.

**Semantics (v1):**

- The host node is stable; its active child subtree is replaced.
- The old child owner, view nodes, and bindings are disposed.
- The new child is mounted under the same host, preserving host identity and
  siblings.
- State captured outside the closure survives. Local bindings created inside the
  closure reset on replacement. Hoist state that must survive.
- Replacement is structural: layout and paint dirty. Focus/scroll inside is lost
  unless state is hoisted or `KeepAlive` is used.
- Replacement runs in apply; first-run bindings initialize synchronously, so
  layout sees a complete subtree.

**When to use it:**

- generated/plugin UIs whose structure is a function of state;
- switching between component types at runtime;
- cheap leaf components where binding every property is more code than replacing
  the leaf;
- migrating code that is naturally "render this tree from this state".

**When not to use it:** for value-only changes, for keyed lists, or as a default.
Use bindings for values, `For` for lists, and `Show`/`Switch` for branches.

**Options considered and rejected as core semantics:**

| Semantics | Why not |
|---|---|
| `#[component(rebuild)]` attribute | Ambiguous whether `let` bindings are hoisted (state survives) or re-created (state resets). |
| Replace the component node itself | Loses sibling position/focus and complicates parent references. |
| Reconcile keyed children across rebuilds | That is a virtual DOM; it reintroduces the diff tax and duplicate identity rules. |
| Keep both branches alive | That is `KeepAlive`, a separate feature. |

Compose `ViewSlot` with `CachedView`/`Memo` to keep expensive children alive
across replacements (a shallow rebuild). If a subtree needs row identity, use
`For` inside it; `ViewSlot` is not a substitute for keyed lists.

### 7.3 `RenderOnce`

```rust
#[derive(IntoElement)]
struct Badge { text: String }
impl RenderOnce for Badge {
    fn render(self, cx: &mut Cx) -> impl View { view! { <span>{self.text}</span> } }
}
```

A stateless component that is consumed at mount. It has no identity and no
bindings of its own; its output is inlined into the parent. This is the Rust
equivalent of a "function component that is just markup".

### 7.4 `RawElement`

For custom paint or layout:

```rust
RawElement::new(|cx, bounds| {
    // emit primitives directly
})
```

It participates in layout as a leaf (or a container if it manages children) and
paints through the scene API. Use for charts, canvases, and custom editors.

## 8. Example: a todo app

```rust
#[derive(Store, Patch, Clone, Default)]
struct AppState {
    draft: String,
    todos: Vec<Todo>,
}

#[component]
fn App(state: Store<AppState>) -> impl View {
    let filter = create_signal(cx, Filter::All);

    let visible = create_memo(cx, move |cx| {
        let f = filter.get(cx);
        state.todos().read(cx).iter()
            .filter(|t| f.matches(t))
            .cloned()
            .collect::<Vec<_>>()
    });

    view! {
        <div class="app">
            <h1>"Todos"</h1>
            <input
                value=state.draft()
                on_input=move |cx, text| state.draft().set(cx, text)
                on_key_down=move |cx, key| if key.is_enter() { add_todo(cx, state) }
            />
            <button on_click=move |cx, _| add_todo(cx, state)>"Add"</button>

            <div class="filters">
                <FilterButton label="All" selected=filter value=Filter::All />
                <FilterButton label="Active" selected=filter value=Filter::Active />
                <FilterButton label="Done" selected=filter value=Filter::Done />
            </div>

            <Show when=move |cx| !visible.get(cx).is_empty()
                  fallback=|| view! { <p class="empty">"Nothing here"</p> }>
                <For each=visible key=|t| t.id children=|t| view! {
                    <TodoRow todo=t />
                } />
            </Show>
        </div>
    }
}

fn add_todo(cx: &mut Cx, state: Store<AppState>) {
    let title = state.draft().get(cx);
    if title.is_empty() { return; }
    state.draft().set(cx, String::new());
    let id = next_id(cx);
    state.todos().push(cx, Todo { id, title, done: false });
}
```

What runs when the user types:

1. `on_input` stages `state.draft().set(cx, text)`.
2. Flush marks the draft path and the `input.value` binding.
3. Apply sets the input's value and the caret.
4. The list does not re-run; the draft is not in `visible`.

What runs when a todo is toggled:

1. `todo.done().set(cx, !done)` marks the row's done path and the `visible` memo.
2. The row's checkbox binding updates.
3. `visible` may remove the row from the filtered list; the keyed `For` diff
   removes it (or keeps it if the filter still matches).

## 9. Trade-offs

| Choice | Benefit | Cost |
|---|---|---|
| Mount once | no structural diff, stable identity, cheap updates | components do not re-run; users must use bindings |
| `view!` proc macro | concise, compile-time wiring | macro complexity, error messages |
| Props as signals | fine-grained updates without re-render | prop identity must be stable |
| Keyed `For` | row state survives reorder | keys required; duplicate-key checks |
| `ViewSlot` | simple for dynamic forms | loses subtree state |
| Strict-read warnings | catches a common mistake | some false positives; needs `untrack` |

## 10. Tests

- Macro expansion: `trybuild` or `cargo expand` golden tests for tags, attrs,
  bindings, conditionals, lists, spreads.
- Compile-fail: missing key, wrong prop type, non-view child, non-`Send` future
  in an action.
- Runtime: the todo example runs; typing updates only the draft binding; toggling
  updates only the row and the filter memo.
- Diagnostics: each code in §6 fires on a minimal example and not on the
  corresponding correct example.
- Hygiene: macros work in modules, with generics, and with `use` aliases.
- Performance: `view!` output allocates once at mount, zero per update for a
  static subtree.

## 11. Open questions

1. **Syntax.** How close to JSX should `view!` be? A tag syntax is familiar but
   requires a proc macro parser; a builder API is simpler but more verbose. We
   can ship both: `view!` for apps, builders for libraries.
2. **Reactive attribute detection.** The v1 decision is syntactic: closures are
   memos, handles pass through, other expressions become static (`Prop::Static`
   or a constant signal). The open part is how much type information the macro
   can use without hurting error messages. Do not add a blanket `IntoSignal`
   trait; Rust coherence rejects it.
3. **Props defaults.** `Prop<T>` is the v1 decision: `Static`, `Reactive`, or
   `Derived`; no bare `T` for values that may change. The open part is whether
   `Prop<T>` should also support optional/`Option` convenience or a separate
   `MaybeProp<T>`.
4. **`For` key ergonomics.** Requiring a key is correct but annoying. Can the
   macro infer `id` when the item implements a `Keyed` trait? Yes, with an
   explicit override.
5. **Component state.** A per-mount `struct` vs closure captures. `ViewSlot`
   requires state to be captured outside its replacement closure; the macro can
   choose closure captures for simple components and a state struct for
   `ViewSlot` and for exposing methods.
6. **Error messages.** Proc macros are notoriously bad at spans. Budget time for
   `syn::Error` spans and `trybuild` tests.
