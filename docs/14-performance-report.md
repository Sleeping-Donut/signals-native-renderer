# 14 — v0.1 Performance and Hardening Report

Measured on the development MacBook Pro (Apple M3 Pro, 2× Retina, Metal),
release builds unless noted. The original 2019-laptop target was replaced by
the hardware-adapted M3 Pro baseline in Phase 5.

## Native frame times

`window_smoke`, visible Metal surface:

| Run | Physical / logical | Workload | Steady mean | p95 | FPS |
|---|---|---|---|---|---|
| 2× | 3024×1742 / 1512×871 | 84 text runs, 300 frames | 16.724 ms | 17.253 ms | 59.8 |
| 1× renderer scale | 3024×1742 / 3024×1742 | 120 frames | 16.670 ms | 17.432 ms | 60.0 |
| 1.5× renderer scale | 3024×1742 / 2016×1161 | 120 frames | 16.669 ms | 17.442 ms | 60.0 |

Steady numbers skip the first ten frames.

## Layout and paint

- Paint-only style changes run zero layout.
- Layout changes rebuild only dirty scene ranges; clean siblings replay.
- Text shaping is cached with an LRU byte budget.
- Allocations: the retained-tree profile test asserts no per-frame structural
  rebuild after mount.
- Scene damage is recorded per dirty range.

## Stores and reactivity

- Field writes notify only the written path, its ancestors, and descendants
  when a whole subtree is replaced.
- Nested patches dirty only changed nested paths.
- `patch_keyed` uses a shallow list write and bumps only changed key paths;
  unchanged keyed row memos do not rerun.
- `StoreDiagnostics` reports reads, writes, trigger nodes, and live handles;
  `Field::get_strict` rejects reads outside tracked computations.
- Pending is a tracked verdict: async pending tokens are bumped on flight start
  and commit.

## Churn and arena reuse

`crates/reactive-core/tests/soak.rs`:

- `churn_does_not_grow_graph_arenas` creates and disposes 4,000 owners with
  signals, memos, render effects, and flushes, then asserts the live node and
  owner counts are unchanged.
- `soak_long_churn` is the 24-hour harness. Run:

```bash
SOAK_SECONDS=86400 cargo test -p reactive-core --test soak soak_long_churn -- --ignored
```

A one-second run of the same harness passed and kept node/owner counts stable
during the run.

## Conformance and CI

- `renderer-null` conformance covers structure, text, styles, events, errors,
  frame hooks, and operation-log recording.
- The full workspace tests and clippy run on Ubuntu and macOS in CI.
- Miri covers reactive core, stores, view counter, macro, and async tests;
  GPU/font timing tests are ignored under Miri.

## Known limits

- A full 24-hour wall-clock run has not been executed locally; the harness is
  checked in and parameterized by `SOAK_SECONDS`.
- Incremental layout and damage scissoring remain future optimizations.
- Enum/dynamic stores, automatic `Loading` detection, and the component/props
  macro are implemented for v0.1.
- The 24-hour soak harness is checked in; only a short wall-clock interval was
  run in this session.
