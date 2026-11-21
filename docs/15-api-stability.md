# 15 — v0.1 API Stability

This is the first pre-release. The following surfaces are stable enough for
in-tree apps and examples.

## Stable for v0.1

- `reactive-core`
  - `create_signal`, `create_derived_signal`, `create_memo`, `create_async_memo`
  - `signal!`, `memo!`
  - `effect`, `render_effect`, `Runtime`, `RuntimeHandle`, `Cx`
  - `loading_boundary`, `error_boundary`, `is_pending`
  - `OwnerHandle`, cleanup, disposal
- `reactive-stores`
  - `Store`, `Field`, `StorePatch`, `Draft`, `DynamicValue`
  - `#[derive(Store)]`, `#[derive(Patch)]`, `store!`
  - keyed list handles and `patch_keyed`
- `renderer-api`
  - pointer/key/wheel/IME event types
  - resolved styles, transforms, overflow, a11y roles, cursor icons
- `view`
  - `View`, builders, `Prop` (static/signal/memo), `view!`, `StateStyle`
  - focus/tab, input dispatch, scroll, drag/drop, text input
  - AccessKit snapshot/action methods
- `renderer-native`
  - `WindowApp`, `WindowDriver`, `PaintBackend`, `WgpuBackend`,
    `HeadlessBackend`, `run_windowed`

## Experimental / may change

- `async_derived_signal` details and staged multi-node transaction semantics.
- Component tag conventions, slots, and the component/props/`Props` builder
  macros.
- Automatic `Loading` internals (effect readiness tokens, subtree collection).
- Store enum tuple-variant ergonomics, dynamic-store schema helpers.
- Damage scissoring, incremental layout, and partial presentation.
- `view_builder!` fluent syntax; `view!` remains the recommended form.

## Compatibility notes

- All crates are `0.1.0`; no semver guarantees before v1.
- Public APIs take `&Cx`; there is no global runtime.
- Reads use `get`; pending is checked with `is_pending(cx, |cx| ...)`.
- No `RwSignal`, combined public signal handle, or `AsyncMemo` type.
