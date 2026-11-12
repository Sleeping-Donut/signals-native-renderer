# Documentation

Start with [`../PLAN.md`](../PLAN.md) for scope and sequencing, then
[`../ARCHITECTURE.md`](../ARCHITECTURE.md) for the system shape. The documents
below own one section each. They are intended to be read while implementing, not
just once.

| Doc | Owns |
|---|---|
| [00-glossary.md](00-glossary.md) | Terms shared across all docs. |
| [reference-repos.md](reference-repos.md) | Exact refs/ commits, clone commands, licences. |
| [01-reactive-core.md](01-reactive-core.md) | Signals, memos, effects, the height heap, read/write paths. |
| [02-ownership-and-lifetimes.md](02-ownership-and-lifetimes.md) | Arenas, handles, owners, cleanup, reclamation. |
| [03-store-layer.md](03-store-layer.md) | Field-granular stores, patches, keyed collections. |
| [04-async-model.md](04-async-model.md) | Spawner, flights, holds, boundaries, pending. |
| [05-runtime-and-frame-scheduler.md](05-runtime-and-frame-scheduler.md) | The six frame phases, wakeups, idle, reentrancy. |
| [06-renderer-contract.md](06-renderer-contract.md) | The renderer API (universal contract) and conformance. |
| [07-retained-renderer.md](07-retained-renderer.md) | The retained view tree, bindings, lists, paint cache. |
| [08-layout-and-text.md](08-layout-and-text.md) | Taffy integration, text shaping, caching. |
| [09-input-focus-accessibility.md](09-input-focus-accessibility.md) | Hit testing, dispatch, focus, IME, AccessKit. |
| [10-macros-and-authoring.md](10-macros-and-authoring.md) | `view!`, components, props, diagnostics. |
| [11-testing-and-performance.md](11-testing-and-performance.md) | Conformance suites, fuzzers, benchmarks. |
| [12-decisions-and-risks.md](12-decisions-and-risks.md) | ADR log, risk detail, unresolved questions. |

## Conventions

- **Must / should / may** follow RFC 2119.
- Code sketches are illustrative. Signatures will change; the stated invariants
  will not without an ADR.
- Each doc ends with *Tests* and *Open questions*. If an implementation changes a
  stated rule, update the doc in the same change.
- Cross-references use the rule IDs from the reference material: `F-<n>` and
  `D-<n>` refer to Solid 2.0's fundamentals and derived rules
  (`refs/solid/packages/signals/docs/RULES-FUNDAMENTALS.md`). We adopt a subset;
  the doc says which.
- Reference paths are relative to `refs/` and are not part of this repository.
