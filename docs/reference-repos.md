# Reference Repositories

The design docs cite files under `refs/`. That directory is git-ignored so the
main repository stays small. This page records the exact state the docs were
written against and how to restore it.

## Recorded state

| Repo | Path | Ref | Commit | Date |
|---|---|---|---|---|
| Solid | `refs/solid` | `next` | `f4d2bc2c2a516f5332843b9e7172caa8a4f4634a` | 2026-11-12 |
| Leptos | `refs/leptos` | default branch | `1434587899126c03a649828df98a363d6bd601ab` | 2026-11-12 |
| Zed (GPUI) | `refs/zed` | default branch, sparse | `498af674abe1f054869a4d77b5a96ed8a3f12008` | 2026-11-12 |

If a reference file cited in a doc has moved, search the recorded commit before
concluding the design note is stale.

## Clone commands

```sh
mkdir -p refs

# Solid 2.0 next branch (the behavioral oracle)
git clone --branch next --depth 1 \
  https://github.com/solidjs/solid.git refs/solid

# Leptos (Rust reactive graph + stores)
git clone --depth 1 \
  https://github.com/leptos-rs/leptos.git refs/leptos

# Zed, sparse to the GPUI crates
git clone --filter=blob:none --sparse \
  https://github.com/zed-industries/zed.git refs/zed
cd refs/zed
git sparse-checkout set crates/gpui crates/gpui_macros crates/gpui_platform \
  crates/gpui_macos crates/gpui_linux crates/gpui_windows crates/gpui_web
```

The docs primarily cite:

- `refs/solid/packages/signals/`
- `refs/solid/packages/universal/`
- `refs/solid/documentation/solid-2.0/`
- `refs/leptos/reactive_graph/`
- `refs/leptos/reactive_stores/`
- `refs/leptos/reactive_stores_macro/`
- `refs/leptos/tachys/`
- `refs/zed/crates/gpui/`

## Licenses and compatibility

| Repo | License | Compatible with GPL-2.0-only? |
|---|---|---|
| This project | GPL-2.0-only | — |
| Solid | MIT | Yes |
| Leptos | MIT | Yes |
| GPUI | Apache-2.0 | **No** |

MIT code may be incorporated into a GPLv2 work with attribution. Apache-2.0 is
compatible with GPLv3, not GPLv2. So:

- Use Solid and Leptos as both references and, where useful, as sources for
  ported algorithms; keep the MIT notice.
- Use GPUI as an **architectural reference only**. Reimplement its ideas
  independently; do not copy code, comments, or other expressive content.
- If you later need to copy GPUI code, relicense the project to GPL-3.0-or-later
  (or GPL-2.0-or-later, which lets recipients choose GPLv3) or obtain permission
  from the copyright holder first.

The implementation should still be independent. Where an algorithm is ported,
add a module header naming the source file, commit, and license, and keep a
top-level `NOTICE`/`THIRD_PARTY.md` (see ADR Q8 in
[12-decisions-and-risks.md](12-decisions-and-risks.md)).

## Why these three

- **Solid 2.0** is the only reference with a written, test-backed specification of
  async hold/pending semantics. It is the oracle, not a code template.
- **Leptos** proves the reactive graph and store macro are viable in Rust, and
  documents the costs of an `Arc<RwLock>` node model.
- **GPUI** proves retained view identity, per-view paint caching, scene replay,
  taffy layout, and AccessKit integration at application scale.

No other repository is required for v1. If one is added, record its commit here
first.
