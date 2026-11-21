# Changelog

## Unreleased

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
