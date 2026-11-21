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
- Damage scissoring: offscreen targets render with `LoadOp::Load` and one
  scissor per damage rect; window surfaces render into a persistent scene
  texture and blit it. A scissored frame is byte-identical to a full render
  (`wgpu_incremental_damage_matches_a_full_render`), and the demo snapshots are
  unchanged across a hundred incremental frames.

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

`crates/reactive-core/tests/soak.rs` is split into two non-CI tools:

- `fuzz_churn_is_bounded` — rapid deterministic fuzz/churn for agents. It
  creates and disposes owners with random signals, memos, render effects, and
  nested owners, then proves live node/owner counts stay fixed. Run:

```bash
scripts/soak-fuzz.sh
```

Measured on the M3 Pro:

```text
fuzz churn: 100000 rounds in 0.217s (461182 rounds/s),
nodes=0, owners=1, rss=Some(2336) KB
```

The counts are constant from the first assertion to the last.

- `soak_monitor` — optional manual wall-clock monitor. Script:

```bash
scripts/soak-monitor.sh 24 3600
```

This detaches the test, logs hourly node/owner/RSS counts, and records a final
pass/fail in `.soak/soak-*.log`. It is not a CI job and is not a blocker.
Recommended before a release, after scheduler/arena changes, or when memory
growth is suspected.

## Release-mode budget measurements

Measured with `scripts/perf-budgets.sh` on the M3 Pro (release build,
10,000-row keyed list):

| Measurement | Result | Budget | Status |
|---|---|---|---|
| Mount + first 10k layout | 130.0 ms | < 150 ms | met |
| Single-row update frame | 0.07 ms | < 1 ms | met |
| Paint-only frame | 0.037 ms | < 2 ms | met |
| 10k reorder | 92.0 ms | — | recorded |

A change that stays inside a **fixed-size node** recomputes only that subtree:
the updated row copied one layout node and ran zero full-root passes. The bound
requires definite width and height with no flex shrink, which is the common row
shape. Auto-sized ancestors still force a full pass, so content that changes its
container's size keeps the old behavior.

```text
perf budgets: mount+layout 130.348 ms, single-row update 0.067 ms
(layout nodes 1 roots 0), paint-only 0.038 ms, 10k reorder 92.413 ms
```

## Conformance and CI

- `renderer-null` conformance covers structure, text, styles, events, errors,
  frame hooks, and operation-log recording.
- The full workspace tests and clippy run on Ubuntu and macOS in CI.
- Miri covers reactive core, stores, view counter, macro, and async tests;
  GPU/font timing tests are ignored under Miri.

## Known limits

- The manual 24-hour monitor has not been run to completion in this session; it
  is available for a human to leave running. The rapid fuzz run is the automated
  bounded-growth evidence.
- Fixed-size layout boundaries isolate local updates; general incremental
  layout remains a future optimization.
- Damage scissoring reports rasterized scene area in `FrameStats`; GPU-time
  deltas still need a timestamp-query harness.
- Enum/dynamic stores, automatic `Loading` detection, and the component/props
  macro are implemented for v0.1.
