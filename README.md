> [!WARNING]
> **This is slop.** This repository is an AI-assisted proof of concept and design
> scratchpad, not production software. The code, APIs, and documentation are
> incomplete, experimental, and likely to change or be abandoned. Do not rely on
> any of it.

# signals-native-renderer

Design and planning documents for a signal-driven, retained-mode native UI
framework in Rust, informed by:

- **Solid 2.0** (`refs/solid`) — behavioral oracle for the reactive core and
  async rules.
- **Leptos** (`refs/leptos`) — a current Rust reactive graph and derive-macro
  store implementation.
- **GPUI** (`refs/zed/crates/gpui`) — retained view identity, paint caching,
  layout, input dispatch, and testing.

The reference repositories are git-ignored and may be re-cloned into `refs/` as
needed.

## Read in this order

1. [`PLAN.md`](PLAN.md) — scope, decisions, phases, milestones, risks, budgets.
2. [`ARCHITECTURE.md`](ARCHITECTURE.md) — system shape, crates, core types,
   frame lifecycle, invariants, reference map.
3. [`docs/`](docs/README.md) — one document per section:

   - [Glossary](docs/00-glossary.md)
   - [Reference repositories](docs/reference-repos.md)
   - [Reactive core](docs/01-reactive-core.md)
   - [Ownership and lifetimes](docs/02-ownership-and-lifetimes.md)
   - [Store layer](docs/03-store-layer.md)
   - [Async model](docs/04-async-model.md)
   - [Runtime and frame scheduler](docs/05-runtime-and-frame-scheduler.md)
   - [Renderer contract](docs/06-renderer-contract.md)
   - [Retained renderer](docs/07-retained-renderer.md)
   - [Layout and text](docs/08-layout-and-text.md)
   - [Input, focus, accessibility](docs/09-input-focus-accessibility.md)
   - [Macros and authoring](docs/10-macros-and-authoring.md)
   - [Testing and performance](docs/11-testing-and-performance.md)
   - [Decisions and risks](docs/12-decisions-and-risks.md)

[`PLAN-OUTLINE.md`](PLAN-OUTLINE.md) is the original brief and is kept for
provenance. Where it conflicts with `PLAN.md`, the newer documents win; the
deviations are recorded in
[docs/12-decisions-and-risks.md](docs/12-decisions-and-risks.md).

## License

This project is licensed under **GPL-2.0-or-later**. See [`LICENSE`](LICENSE) and
[`THIRD_PARTY.md`](THIRD_PARTY.md).

The reference repositories in `refs/` have their own licenses. Solid and Leptos
are MIT (compatible with GPLv2+); GPUI is Apache-2.0, which is compatible with
GPLv3 but not GPLv2. Because this project is GPL-2.0-or-later, a distributor may
choose GPLv3 and combine Apache-2.0 code, but we keep GPUI reference-only until
the project explicitly distributes under GPLv3-compatible terms and carries the
Apache notices. See [`docs/reference-repos.md`](docs/reference-repos.md).

## Development

Phase 0 is implemented as a Cargo workspace with a Nix flake. The flake pins the
toolchain; do not install Rust globally.

```sh
nix develop
cargo test --workspace
cargo bench -p reactive-core
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

Without an interactive shell:

```sh
nix develop -c cargo test --workspace
nix develop -c cargo bench -p reactive-core
```

Phase 0 covers the generational arena, `NodeId`/`Handle`, the runtime shell, the
manual spawner, and the benchmark harness. Phase 1 adds the reactive graph.

CI is path-filtered: changes to prose, docs, plans, and the README do not run the
Rust gate. Code, dependency, flake, toolchain, and workflow changes do.

## The one-paragraph summary

One reactive graph, one retained tree, one checkpoint per frame. State and
rendering share a single dependency graph; the retained tree gives stable
identity and incremental paint; the frame checkpoint drains async, promotes
writes, settles computations, applies bindings, lays out dirty subtrees, and
paints once. Third-party renderers get a small contract; the first-party
renderer bypasses it for speed.
