# 11 — Testing and Performance

**Owns:** the test strategy, conformance suites, fuzzers, differential tests,
benchmarks, CI gates, and devtools.

---

## 1. The pyramid

```
                 ┌──────────────────────┐
                 │  App-level scenario  │  few, slow, realistic
                 ├──────────────────────┤
                 │  Differential / fuzz │  random, invariant-based
                 ├──────────────────────┤
                 │  Renderer conformance│  every backend
                 ├──────────────────────┤
                 │  Reactive rule suite │  semantics oracle
                 ├──────────────────────┤
                 │       Unit tests     │  many, fast
                 └──────────────────────┘
```

The reactive core and renderer are tested separately, then integrated through a
headless runtime and the null renderer.

## 2. Reactive core conformance

The oracle is Solid 2.0's rule documents:

- `refs/solid/packages/signals/docs/RULES-FUNDAMENTALS.md` (`F-<n>`)
- `RULES-DERIVED.md` (`D-<n>`)
- `SCENARIOS.md` (`S-<n>`)
- `SPEC-ASYNC-SEMANTICS.md` (`A<nn>`, history and mechanism)

We do not port every rule. The v1 subset is defined in
[04-async-model.md](04-async-model.md). For each adopted rule, write both a
**positive** test (the expected frame) and a **negative** test (the rejected
temptation). Example:

```rust
#[test]
fn f2_navigation_keeps_old_screen_until_new_is_ready() {
    let mut rt = TestRuntime::new();
    rt.mount(nav_app());
    rt.click("details");
    // fetch is in flight
    assert_frame(&rt, |f| f.text("page") == "A" && f.text("loading") == "");
    rt.land("details-data");
    assert_frame(&rt, |f| f.text("page") == "B" && f.text("title") == "Details");
}
```

### 2.1 Core graph tests

| Area | Tests |
|---|---|
| Read/write | signal reads, write visibility before/after flush, untrack |
| Equality | default, custom, never; no notify on equal |
| Memo | lazy/eager, dependency change, diamond, nested |
| Effects | compute/apply order, cleanup, rerun, disposal |
| Ownership | child disposal, LIFO cleanup, context |
| Heap | ordering, height adjustment, run-to-fixpoint bound |
| Cycles | write-in-compute rejected; debug cycle error |
| Handles | stale handle fails; slot reuse doesn't alias |
| Arenas | random insert/remove; Miri |

### 2.2 Rule-to-test map

Keep a checked-in table (or generated doc) with one row per adopted rule:

```
F-1  tests/rules/f1_whole_frame.rs
F-2  tests/rules/f2_hold.rs
F-3  tests/rules/f3_display_reader.rs
F-4  tests/rules/f4_armed_boundary.rs
F-5  tests/rules/f5_disarmed_boundary.rs
F-9  tests/rules/f9_pending.rs
F-10 tests/rules/f10_flush.rs
F-12 tests/rules/f12_error.rs
```

A CI check fails if a rule in the v1 subset has no test. This prevents the
"documented but unverified" drift that the reference docs warn about.

### 2.3 App-level reference demos

The rule suite checks the graph in isolation. The app-level suite checks that the
graph, bounds, and retained tree agree on a whole frame. The programs to port
and the exact frames to assert are in
[13-reference-apps.md](13-reference-apps.md). Port them in milestone order:
counter (M1/M3), states and cities (M2/M4), then the later candidates.

## 3. Differential tests

Two implementations of the same observable:

### 3.1 Reactive full recompute

For a random DAG and a random operation sequence:

- run the incremental graph;
- from the final inputs, recompute every memo from scratch in topological order;
- assert values and errors match.

This catches missed dirtiness, bad heights, and stale caches. It does not catch
scheduling (effects' order), which has separate tests.

### 3.2 Renderer full rebuild vs incremental

For a random view tree and a random edit:

- clone the tree and do a full rebuild/mount into a fresh scene;
- apply the edit incrementally to the original;
- assert the scenes are identical (primitives, bounds, order, text).

This is invariant I9 and the most valuable renderer test. It catches paint cache
bugs, dirty propagation misses, and stale layout.

### 3.3 Scene golden files

Serialize the scene to a stable text format and compare to a checked-in golden:

```
quad bounds=(0,0,100,40) color=#1e1e1e radius=4
text "Hello" origin=(8,12) font=Inter:14 color=#fff
...
```

Golden files are brittle if they include timestamps/IDs; keep IDs out or
normalize them. Use golden files for visual regressions, not for every test.

## 4. Fuzzing

### 4.1 Graph fuzzer

Generate:

- nodes: signals, memos, effects, async memos;
- edges: random read patterns within a valid scope;
- operations: set, flush, land, cancel, dispose, mount, unmount.

Invariants:

- no panic, no use-after-free (Miri on a subset);
- every frame is internally consistent (`F-1`);
- no disposed handle resolves;
- arena live counts return to baseline after all owners are disposed;
- run-to-fixpoint terminates within the bound.

Use `proptest` or `arbitrary`; shrink to a minimal sequence. Store failures as
regression tests.

### 4.2 Renderer tree fuzzer

Port the idea of GPUI's `randomized_element_tree.rs`
(`refs/zed/crates/gpui/src/randomized_element_tree.rs`):

- generate a tree with random topology (wide, deep, mixed, narrow);
- random element counts, entity density, handler density, styles;
- random edits: insert, remove, move, restyle, set text;
- assert differential equivalence and invariants I7–I9;
- run the same tree through the backend and the null renderer and compare
  logical output.

This is the best defense against retained-tree structural bugs.

### 4.3 Input fuzzer

Random pointer/key sequences against a tree with focusables and scroll
containers. Assert:

- no panic;
- focus is always a live node or `None`;
- scroll offsets stay in bounds;
- dispatch terminates (no infinite bubble);
- a11y tree IDs remain stable for surviving nodes.

## 5. Renderer conformance

`renderer-conformance` is parameterized over a backend:

```rust
fn run_suite<F: RendererFactory>(factory: F) {
    structure_tests(factory);
    text_tests(factory);
    attr_tests(factory);
    style_tests(factory);
    event_tests(factory);
    error_tests(factory);
    lifecycle_tests(factory);
    capability_tests(factory);
}
```

The null renderer is the reference implementation. A third-party renderer is
"supported" only when the suite passes.

## 6. Determinism

- All tests run with `ManualSpawner` and `VirtualClock`.
- No test sleeps; `advance(Duration)` fires timers.
- Frames are explicit: `rt.frame(FrameCause::Manual)`.
- Random seeds are printed on failure and accepted as env vars (`SEED`, like
  GPUI's test harness).
- Hash maps use deterministic hashers (`FxHashMap` or a seeded hasher) in code
  that affects iteration order; tests must not depend on hash iteration order.

## 7. Performance measurement

### 7.1 Microbenchmarks

Use `criterion`:

| Bench | What |
|---|---|
| `signal_read` | 1 signal, 1 reader, hot loop |
| `signal_write_1` / `_1k` | write + mark, 1/1k subscribers |
| `memo_diamond` | two paths converge; verify glitch-free |
| `memo_chain_100` | deep chain, one write |
| `effect_apply` | compute + apply overhead |
| `owner_create_dispose` | 1k owners, nested |
| `arena_churn` | insert/remove 1M nodes |

### 7.2 Renderer benchmarks

| Bench | What |
|---|---|
| `mount_10k_rows` | keyed list mount, simple text |
| `update_one_row` | one field change |
| `reorder_10k` | move one row from top to bottom |
| `paint_only` | color change on one node |
| `layout_full` | 10k nodes, flex |
| `text_shape_1k` | 1k short labels cold/warm |
| `scene_replay` | clean subtree replay |
| `input_hit_test` | 1k hitboxes |

### 7.3 Frame benchmarks

`FrameStats` from the runtime:

- phase durations;
- dirty node counts;
- layout node count;
- paint primitive count;
- allocations;
- wake reason.

Track p50/p95/p99 over a scripted scenario. A regression >10% fails the perf CI
job (with noise handling; run benchmark pairs interleaved).

### 7.4 Budgets

From [PLAN.md](../PLAN.md) §8. Add a machine-readable budget file and assert in
CI on a dedicated runner. Local runs report, CI enforces.

## 8. Leak and soak tests

- **Arena leak**: mount/unmount a component 100k times; live node/owner/edge
  counts return to baseline; free lists bounded.
- **Binding leak**: remove view nodes; their effects are disposed; no effect
  fires after removal.
- **Listener leak**: remove nodes with listeners; listener count returns to zero.
- **Text cache**: churn 1M distinct labels; cache respects its byte budget.
- **24h soak**: random user-like activity; memory flat after warm-up; no frame
  time creep; no queue growth.
- **GPUI-style leak detection**: optionally track every handle and assert all are
  released at test end (`refs/zed/crates/gpui/src/app/entity_map.rs` has a
  `leak-detection` feature).

## 9. CI gates

| Gate | Blocks |
|---|---|
| `cargo test` (unit + integration) | merge |
| Reactive rule suite (v1 subset) | merge |
| Renderer conformance (null + native adapter) | merge |
| Differential/fuzz smoke (fixed seed, 1000 cases) | merge |
| Miri on `reactive-core` | merge |
| `cargo clippy -- -D warnings` | merge |
| `cargo fmt --check` | merge |
| Benchmarks (nightly, interleaved) | regression >10% |
| Soak (weekly) | leak/creep |
| Visual goldens (nightly) | unexpected diff |

Feature matrix:

- `std` only (no platform) — core tests.
- `runtime` + `renderer-null` — headless integration.
- `renderer-native` + `wgpu` — desktop.
- `wasm` — compile check (later).

## 10. Observability and devtools

- **Graph inspector**: live node count, edges, dirty states, owner tree. Read-only
  snapshot; do not hold locks across UI.
- **Dirty trace**: for a given node, why it recomputed (which dependency changed).
  Dev builds only.
- **Frame profiler**: phase timings, dirty counts, paint ops, damage.
- **Scene debugger**: overlay bounds, hitboxes, dirty regions; dump scene to
  JSON for a golden.
- **Attribution** (later): map a signal write to the bindings and scene primitives
  it changed. Solid's `attribution` module is the reference; it is large, so
  start with a simple stack trace in dev.

Diagnostics must be pay-for-use: compiled out or behind a feature in release.

## 11. References

| Topic | Reference |
|---|---|
| Reactive rules | `refs/solid/packages/signals/docs/RULES-FUNDAMENTALS.md` |
| Rule index / scenarios | `RULES-INDEX.md`, `SCENARIOS.md` |
| Async state | `INTERNALS-ASYNC-STATE.md`, `SPEC-ASYNC-SEMANTICS.md` |
| Leptos tests | `refs/leptos/reactive_graph/tests/*` |
| Store tests | `refs/leptos/reactive_stores/tests/*` |
| GPUI test harness | `refs/zed/crates/gpui/src/app/test_context.rs`, `test.rs` |
| Randomized tree | `refs/zed/crates/gpui/src/randomized_element_tree.rs` |
| Leak detection | `refs/zed/crates/gpui/src/app/entity_map.rs` |
| Visual tests | `refs/zed/crates/gpui/src/app/visual_test_context.rs` |

## 12. Open questions

1. **Fuzz corpus storage.** Check failing seeds into the repo as regression
   tests, or store a corpus directory? (Leaning: both; seeds are small.)
2. **Golden scene format.** Text is stable and diffable; binary is faster. Use
   text for tests, binary for benchmarks.
3. **Miri scope.** Miri is slow; run the arena/edge suites, not the full renderer.
4. **Perf CI hardware.** A dedicated runner is needed for stable numbers; until
   then, report and gate on relative changes with wide thresholds.
5. **Visual diff threshold.** Pixel-perfect goldens fail on font/GPU changes;
   use a perceptual diff with a small tolerance and a platform-specific font
   bundle.
6. **Accessibility testing.** AccessKit has a test adapter; use it to assert the
   tree without a screen reader.
