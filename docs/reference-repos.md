# Reference Repositories

The design docs cite files under `refs/`. That directory is git-ignored so the
main repository stays small. This page records the exact state the docs were
written against and how to restore it.

## Recorded state

| Repo | Path | Ref | Commit | Date |
|---|---|---|---|---|
| Solid | `refs/solid` | `main` (2.0 release) | `6888e261cee7967d3e4d6c04a5a61fad255aff1c` | 2026-11-18 |
| Leptos | `refs/leptos` | default branch | `1434587899126c03a649828df98a363d6bd601ab` | 2026-11-12 |
| Zed (GPUI) | `refs/zed` | default branch, sparse | `498af674abe1f054869a4d77b5a96ed8a3f12008` | 2026-11-12 |

The Solid references were originally drafted against `next` at
`f4d2bc2c2a516f5332843b9e7172caa8a4f4634a` (2026-11-12). Solid 2.0 has since
released and development moved to `main`; the current state above is the 2.0
release line. If a cited file has moved, search both the release commit and the
older `next` commit before concluding the design note is stale.

## Clone commands

```sh
mkdir -p refs

# Solid 2.0 main branch (the behavioral oracle)
git clone --branch main --depth 1 \
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

| Repo | License | Compatibility note |
|---|---|---|
| This project | GPL-2.0-or-later | Recipients may choose GPLv2 or GPLv3-or-later |
| Solid | MIT | Compatible with GPLv2-or-later |
| Leptos | MIT | Compatible with GPLv2-or-later |
| GPUI | Apache-2.0 | Compatible with GPLv3, **not** GPLv2-only |

MIT code may be incorporated into a GPLv2-or-later work with attribution.
Apache-2.0 is compatible with GPLv3, not GPLv2-only. So:

- Use Solid and Leptos as both references and, where useful, as sources for
  ported algorithms; keep the MIT notice.
- Keep GPUI reference-only for now. Reimplement its ideas independently; do not
  copy code, comments, or other expressive content unless you are prepared to
  distribute the combined work under GPLv3 with the Apache notices.
- If you copy GPUI code, the distribution must be under GPLv3 terms (the
  GPL-2.0-or-later grant allows that choice) and carry the Apache-2.0 notices.
  Do not distribute the combined work under GPLv2-only.

The implementation should still be independent. Where an algorithm is ported,
add a module header naming the source file, commit, and license, and keep a
top-level `THIRD_PARTY.md` (see ADR Q8 in
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
