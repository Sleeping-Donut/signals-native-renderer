# Changelog

## Unreleased

- v0.2 work started: Phase 9/10 roadmap in PLAN, the held-frame `is_pending`
  verdict prototype, and the damage-presentation design.
- `MaybeProp<T>` (the caller may omit the prop). `#[prop(optional)]` now stores
  the field as `MaybeProp<T>`; `#[prop(default = expr)]` keeps `Prop<T>`.
- Damage scissoring: offscreen targets accumulate with `LoadOp::Load` and
  per-damage scissor rects; window surfaces render into a persistent scene
  texture and blit it. `FrameStats::scissored_area` reports rasterized area.
- `affects(cx, node)` declares pending without a value write; the node and its
  derivations report pending until the current flush commits.
- `latest(memo)`, display-only `guess`/`clear_guess`, and quiet `refresh`.
  Async results ready at one flush commit together, and equal async answers no
  longer notify readers.
- `RuntimeHandle::action`/`action_with` with a poll-scoped action: writes after
  `await` stay in the action without an `ActionCx` argument. `OptimisticSignal`
  guesses, plain writes are buffered until settle, and guesses are stored per
  action. `ActionHandle::result` reports completion; the todos demo uses the
  ambient form.
- Per-key store optimism: `Field::optimistic` stages a guess on the field's own
  path node, and `Field::optimistic_list` stages keyed `push`/`remove` patches
  plus `list#hash(key)` row guesses. Reads compose truth, live patches, and row
  guesses; success commits the delta against truth, so overlapping actions do
  not clear or clobber each other.
- Props treat `Rc<T>`/`Arc<T>` and store handles (`Store`, `Field`,
  `ReadField`, `WriteField`, `OptimisticStore`, `OptimisticList`,
  `DynamicStore`) as raw: passing a shared store or action struct no longer
  needs `#[prop(raw)]`.
- `OptimisticStore` derives a store from an async source
  (`OptimisticStore::new(cx, seed, || api.list())`). `refresh(cx, |cx| ...)`
  re-asks the async nodes a probe read touches, matching `is_pending`'s shape.
  The todos demo derives its rows from the API and uses `<Loading>` for the
  first load instead of copying data in from a spawned task.
- `LoadingBoundary::on(cx, closure)` re-arms a loaded boundary when a tracked
  dependency changes, showing the fallback for the new question.
- Renderer `NodeId` contract frozen: opaque non-zero ids, `INVALID_NODE_ID`
  reserved, conformance covered by renderer-null.
- `Field::split` / `read_only` / `write_only` expose `ReadField`/`WriteField`
  capability views for store APIs that need a read-only or write-only handle.
- Seeded fuzz for transaction merge and quiet re-asks
  (`merge_fuzz_random_arrival_orders`, `reask_fuzz_is_quiet`).

## v0.1.1 — 2026-11-21

Second pre-release. Authoring ergonomics, logical control flow, an optimistic
reference app, and the single-row layout budget.

- Experimental `view_builder!` fluent authoring front-end.
- `Prop<T>` stores `ReadSignal`/`Memo` handles instead of boxed closures;
  `From<T>`, `From<ReadSignal<T>>`, and `From<Memo<T>>` are supported.
- Component props use `#[macros::props]` builders. Plain field types are wrapped
  as `Prop<T>` automatically, while view and capability types (`View`,
  `Callback`, `CallbackWith<T>`, `WriteSignal<T>`, …) stay as declared.
  `#[prop(raw)]` is only needed for aliases and custom types. Optional/default
  fields and automatic `view!` prop conversion are supported.
- `Callback` (no payload) and `CallbackWith<T>` (one payload) prop types.
  `CallbackWith<T>` accepts `Fn(&Cx, T)` and converts from `WriteSignal<T>`;
  `CallbackWith::set(write)` spells that conversion out at the call site.
- `view::control` with logical `Show`, `For`, `Loading`, `LoadingWhen`,
  `Repeat`, `Switch`, and `Match` components. `view!` emits the imported names,
  and control flow takes no `style` attribute.
- `TextInputState`, a shared editable buffer whose view repaints from the
  handler and can be cleared programmatically.
- Layout boundaries: a text or subtree change inside a fixed-size node
  recomputes only that node's subtree instead of every layout root. The
  single-row update budget now measures 0.07 ms (target < 1 ms).
- Optimistic todos reference app (`examples/todos.rs`).
- Logical fragments and control-flow views mount no layout node, so their
  children participate in the surrounding element's layout.
- `FlexWrap` on `FlexStyle`, so a container can wrap rows onto multiple lines.
- Component children and the `view::select` selectable-list component.
- Automatic view-aware `Loading` no longer needs a `ready` probe.

## v0.1.0 — 2026-11-20

First pre-release. Phases 0–8 are implemented.

### Reactive core

- Signals, writable derived signals, memos, async memos, split effects,
  render effects, batching, owners, cleanups, and disposal.
- Async flights, holds, loading/error boundaries, pending verdicts, and a
  deterministic virtual clock.
- `signal!` and `memo!` macros, including derive/async forms.
- `is_pending(cx, |cx| ...)` closure probe.

### Stores

- `Store<T>`, `Field<T>`, nested projection, keyed handles, `patch_keyed`.
- `#[derive(Store)]` for structs and enum variants.
- `#[derive(Patch)]` with field-granular notification.
- Dynamic stores (`DynamicValue`), `Draft` guards, strict reads, diagnostics.
- Native store-backed counter example.

### View, layout, text, paint

- Retained view tree, keyed lists, view slots, bindings, focus identity.
- Taffy layout, cosmic-text shaping/cache, scene paint replay and damage.
- wgpu surface/offscreen backend, glyph atlas, native winit loop.
- Visible M3 Pro 2× evidence at ~60 fps; renderer-scale 1×/1.5×/2× runs.

### Input and accessibility

- Pointer/key/wheel/IME dispatch, focus/tab order, drag/drop.
- Hover/focus state styles, cursors, tooltips, scroll chaining/scrollbars.
- AccessKit tree generation, `accesskit_winit` OS adapter, action routing.
- Automatic view-aware `Loading` and component/props tags.

### Async demo

- Native states-and-cities app with chained async memos, held selection,
  pending-disabled city rows, derived resets, and supersession.

### Testing

- Workspace tests and clippy on Ubuntu/macOS; Miri on core, stores, view.
- Rapid deterministic churn fuzz (not CI) and optional manual soak monitor.
