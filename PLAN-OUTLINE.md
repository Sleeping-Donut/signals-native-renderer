# Architecture Plan: A Signal-Driven Retained-Mode UI Framework in Rust

## 1. Vision and Goals

Build a native Rust UI framework that combines:

- **Fine-grained reactive state** using a SolidJS 2.0–compatible reactive core (R3-style push-pull engine).
- **Retained-mode rendering** inspired by GPUI: persistent view identities, explicit invalidation, incremental paint.
- **Renderer portability**: a universal contract for third-party backends, with a specialized first-party GPUI-inspired renderer that bypasses generic dispatch for maximum performance.
- **Correctness by construction**: the reactive core is validated against SolidJS 2.0's fundamental rules as a portable behavioral specification.

The system is **one reactive graph, one retained tree, one checkpoint per frame**. There is no dual reactivity model, no diff/reconcile tax, and no hidden background executor.

---

## 2. Architectural Principles

| Principle | Consequence |
|---|---|
| **One dependency graph** | State and rendering share the same reactive core. The renderer is a sink, not a parallel system. |
| **Explicit over automatic at the boundary** | The reactive graph is automatic *internally*; the renderer consumes it via scheduled effects at a frame checkpoint. |
| **Retained identity, incremental paint** | UI nodes persist across frames with stable generational keys; only dirty nodes are updated. |
| **DAG invariant** | The reactive graph is acyclic by construction (3-color DFS validation). Disposal follows reverse topological order. |
| **Universal contract, specialized flagship** | A minimal universal trait enables third-party renderers; the first-party renderer uses direct, monomorphized paths. |
| **The frame is the checkpoint** | Batched writes are flushed, continuations drained, dirty nodes propagated, and paint emitted once per frame. |

---

## 3. Crate Structure

```
your-framework/
├── crates/
│   ├── reactive-core/          # R3-style engine + Solid 2.0 rules as test oracle
│   ├── reactive-stores/        # #[derive(Store)] macro layer
│   ├── reactive-ownership/     # Arena, owner tree, RAII guards (may merge with core)
│   ├── universal/              # UniversalRenderer trait + capability sub-traits
│   ├── runtime/                # Frame scheduler, checkpoint, async bridge
│   ├── renderer-gpui/          # First-party GPUI-inspired retained renderer
│   ├── renderer-null/          # Headless/test renderer
│   └── prelude/                # Re-exports for ergonomic use
└── examples/
```

Layering:

```
reactive-core ──► reactive-stores ──┐
       │                            ├──► runtime ──► renderer-gpui
       └──► reactive-ownership ─────┘         │
                                              └──► universal ◄── renderer-null
```

`reactive-core` has **no dependency** on rendering. `renderer-gpui` has no dependency on `universal` unless implementing the optional adapter.

---

## 4. Layer 1 — Reactive Core (`reactive-core`)

### 4.1 Algorithmic Basis

Port the **R3 push-pull, lazy two-phase graph-coloring algorithm** (milomg's R3, descendant of Reactively and Alien Signals). This provides:

- **Push phase**: sources mark downstream nodes dirty.
- **Pull phase**: sinks lazily evaluate in topological order.
- **Glitch-freedom**: no effect observes an inconsistent intermediate state.
- **Minimal recomputation**: only nodes on dirty paths are evaluated.

### 4.2 Node Representation

```
ReactiveNode {
    kind: Signal | Memo | Effect,
    value: Option<Box<dyn Any>>,       // interior-mutability cell
    subscribers: BTreeMap<SubscriberId, Subscription>,
    dependencies: BTreeMap<DependencyId, Subscription>,
    dirty: DirtyState,                 // Clean | Check | Dirty
    generation: u32,                   // for stale-handle detection
}
```

Nodes live in a **generational arena** (`SlotMap`-style). Handles are `Copy` `(index, generation)` pairs. No `Rc` cycles are possible because the graph is a DAG.

### 4.3 Behavioral Contract (SolidJS 2.0 Rules)

The core is tested against the **fundamental rules** (`F-<n>`) from SolidJS 2.0's `packages/signals/docs`:

- **F-batching**: reads after a write within a batch observe the pre-write value until flush.
- **F-glitch-freedom**: effects never observe inconsistent derived state.
- **F-disposal-order**: disposal follows reverse topological order.
- **F-async-hold**: async computeds hold previous values until settlement.

JavaScript-specific derived rules (Proxy store invariants, GC reachability) are **explicitly out of scope**; Rust-specific derived rules are authored alongside the port.

### 4.4 Async Model

No async runtime is required. Async computations:

1. Register a continuation when they begin awaiting.
2. Return a "held" previous value (per F-async-hold).
3. Enqueue the continuation into the runtime's checkpoint queue on completion.

The **frame boundary is the checkpoint** where continuations drain and batched writes flush.

---

## 5. Layer 2 — Store Layer (`reactive-stores`)

### 5.1 Mechanism

Macro-generated field getters (the Leptos `reactive_stores` pattern):

```rust
#[derive(Store)]
struct AppState {
    user: User,
    todos: Vec<Todo>,
    theme: Theme,
}
```

The macro generates per-field getters returning reactive handles. Reading `state.todos()` subscribes only to `todos`; changing `state.user()` does not notify effects watching `todos`.

### 5.2 Properties

- **Sibling isolation**: field-granular tracking without runtime Proxy interception.
- **Nested composition**: nested `#[derive(Store)]` structs compose recursively.
- **Patch semantics**: `#[derive(Patch)]` enables wholesale replacement with per-field dirty detection.
- **Collections**: keyed field iteration preserves per-element granularity.

### 5.3 Trade-off

Compile-time macro dependency and slightly reduced flexibility for dynamically-shaped state. For flat state, plain signals suffice; stores are opt-in.

---

## 6. Layer 3 — Ownership, Lifetimes, Cleanup (`reactive-ownership`)

### 6.1 The Three-Layer Stack

**Layer 1 — Generational arena + `Copy` handles.**
All reactive nodes and retained UI nodes live in arenas. Handles are `(index, generation)` pairs. Removal bumps generation; stale handles cannot alias reused slots. This solves ABA and gives stable identity without borrowing the graph.

**Layer 2 — Owner tree for scope-based disposal.**
Every effect and computed has an `Owner`. When an effect re-runs:

1. Its previous owner is cleared.
2. Cleanup functions run (depth-first).
3. Child effects are cancelled.
4. Arena-stored values created during the previous run are dropped and recreated lazily.

Because the graph is a DAG, disposal follows **reverse topological order** with no circular cleanup risk.

**Layer 3 — RAII subscription guards.**
Subscriptions are held by guards that unsubscribe on drop. Retained UI nodes store their subscription guards in their node record; removing a node drops the guards, unsubscribing cleanly with no ghost dirty writes.

### 6.2 The Reclamation Gap

The ACM port of SolidJS to Rust established that **disposal does not establish unreachability**—user code may retain copied handles, and a disposed computation can be resurrected by a later read. Without a tracing GC, arena slots are not reclaimed automatically.

**Mitigation strategies** (configurable per scope):

| Strategy | When to use | Cost |
|---|---|---|
| **Bounded growth** | Short-lived scopes; owner tree drops arena values on re-run | Arena grows with longest-lived owner's churn |
| **Periodic compaction** | Long-lived documents with high churn | Requires rewriting `Copy` handles' indices; handles must not escape runtime |
| **Explicit handle release** | APIs where users manage lifetimes | Shifts burden to user; documented contract |

This gap is a **permanent design constraint**, not a bug to be hidden.

---

## 7. Layer 4 — Universal Renderer Contract (`universal`)

### 7.1 The Universal Trait

A minimal contract for third-party renderer authors, analogous to SolidJS's `createRenderer`:

```rust
pub trait UniversalRenderer {
    type Node;
    type Error;

    fn create_element(&mut self, tag: &str) -> Result<Self::Node, Self::Error>;
    fn create_text(&mut self, text: &str) -> Result<Self::Node, Self::Error>;
    fn insert(&mut self, parent: &Self::Node, child: Self::Node, anchor: Option<&Self::Node>);
    fn remove(&mut self, parent: &Self::Node, child: &Self::Node);
    fn set_attribute(&mut self, node: &Self::Node, name: &str, value: &str);
    fn set_text(&mut self, node: &Self::Node, text: &str);
    // Intentionally minimal; no layout or paint primitives
}
```

### 7.2 Capability Sub-Traits

Following Iced's model, optional capabilities extend the base:

```rust
pub trait TextRenderer: UniversalRenderer { /* shaping, measurement */ }
pub trait ImageRenderer: UniversalRenderer { /* image handles, sampling */ }
pub trait ScrollRenderer: UniversalRenderer { /* scroll offsets, virtualization */ }
```

Third-party renderers implement the base plus any capabilities they support. The runtime dispatches on capability presence at construction time, not per-frame.

### 7.3 The Bypass Principle

Just as `solid-js/web` bypasses `solid-js/universal` for DOM-specific optimizations, **first-party renderers may bypass the universal trait entirely**. The universal crate is the *contract*; the GPUI-inspired renderer is the *flagship implementation* that proves the performance ceiling. It may optionally implement `UniversalRenderer` as an adapter, but its internal path is direct.

---

## 8. Layer 5 — First-Party Retained Renderer (`renderer-gpui`)

### 8.1 Structure

A persistent tree of UI nodes with stable generational identities, parallel to the reactive graph's arena:

```
UiNode {
    id: NodeId,                        // generational key
    kind: Element | Text | Container,
    style: ResolvedStyle,
    layout: Option<LayoutResult>,      // retained across frames
    subscriptions: Vec<SubscriptionGuard>,
    children: SlotMap<NodeId, UiNode>,
    dirty: DirtyFlags,
}
```

### 8.2 Signal-Driven Invalidation

Each UI node **subscribes to the signals it reads** during its render function. Subscription guards are stored on the node. When a signal changes:

1. The push phase marks subscribed nodes dirty.
2. At the frame checkpoint, the renderer walks dirty nodes only.
3. Each dirty node re-runs its render function, re-subscribes, and updates its style/layout/paint state.

**No whole-tree rebuild.** Only dirty nodes re-execute.

### 8.3 Direct Interaction Path

The GPUI-inspired renderer does **not** route through `UniversalRenderer`. It uses:

- Direct arena access for node mutation.
- Monomorphized render functions per element type.
- Retained layout state (no re-layout for unchanged subtrees).
- A GPU command buffer built from dirty paint regions only.

### 8.4 GPUI-Inspired Choices Preserved

- **Explicit view identity** via generational handles.
- **Selective invalidation**: nothing repaints unless a subscribed signal changes.
- **Frame-loop predictability**: one flush, one paint per frame.
- **Zero idle CPU**: no background polling; the graph sleeps until the next signal write.

### 8.5 GPUI-Inspired Choices Replaced

- **Manual `notify()`** → replaced by automatic signal subscription.
- **Imperative element tree rebuild** → replaced by dirty-node incremental updates.
- **`Entity<T>` state** → replaced by the reactive graph; `Store` derives provide the ergonomics.

---

## 9. Layer 6 — Frame Scheduler (`runtime`)

### 9.1 The Checkpoint

The runtime owns the frame loop and the continuation queue:

```
Frame {
    1. Drain continuation queue (async settlements).
    2. Flush batched signal writes.
    3. Propagate dirty marks through the reactive graph (push phase).
    4. For each dirty UI node: pull value, re-run render, re-subscribe.
    5. Layout dirty subtrees.
    6. Paint dirty regions.
    7. Present.
}
```

Steps 1–4 are the reactive checkpoint; steps 5–7 are the render checkpoint. Both occur once per frame.

### 9.2 Async Bridge

Async computations enqueue continuations rather than polling futures. The runtime drains the queue at step 1. No executor, no background threads required for the reactive core itself.

### 9.3 Idle Behavior

When no signal is written and no continuation is pending, the frame loop sleeps. The runtime wakes on:

- An external event (input, timer) that writes a signal.
- A continuation completion.

This preserves GPUI's **0% idle CPU** property.

---

## 10. Testing and Correctness Contract

### 10.1 Reactive Core Tests

- **Fundamental rule suite**: port SolidJS 2.0's `F-<n>` rules as Rust test cases. These are the immutable correctness oracle.
- **Scenario tests**: port `S-<n>` scenarios as fuzz targets.
- **DAG invariant fuzzing**: generate random dependency graphs; assert no cycles are accepted.

### 10.2 Store Tests

- Sibling isolation: changing field A does not notify effects watching field B.
- Patch semantics: wholesale replacement only dirties changed fields.
- Nested composition: nested stores compose without field aliasing.

### 10.3 Renderer Tests

- **Null renderer**: headless backend implementing `UniversalRenderer` for CI.
- **Dirty-node isolation**: changing a signal only repaints subscribed nodes.
- **Reclamation scenarios**: assert arena growth bounds under documented workloads.

### 10.4 Integration Tests

- End-to-end: signal write → frame flush → correct paint output in null renderer.
- Async hold: pending async computation holds previous value until settlement.

---

## 11. Phased Implementation Roadmap

### Phase 1 — Reactive Core
- Generational arena, node types, push-pull propagation.
- DAG validation (3-color DFS).
- Batching and flush semantics.
- Port and pass `F-<n>` fundamental rules.
- **Milestone**: headless reactive graph passes rule suite.

### Phase 2 — Ownership and Cleanup
- Owner tree, scope-based disposal.
- RAII subscription guards.
- Reclamation strategy (bounded growth initially).
- **Milestone**: create/dispose cycles show bounded arena growth for documented workloads.

### Phase 3 — Store Layer
- `#[derive(Store)]` macro.
- Sibling isolation and nested composition.
- Keyed collection iteration.
- **Milestone**: field-granular notification verified by tests.

### Phase 4 — Universal Contract and Null Renderer
- `UniversalRenderer` trait and capability sub-traits.
- `renderer-null` implementation.
- End-to-end signal → render integration.
- **Milestone**: full pipeline works on null renderer.

### Phase 5 — GPUI-Inspired Renderer
- Retained UI node tree with generational identities.
- Signal-driven node subscription.
- Incremental layout and paint.
- GPU pipeline integration.
- **Milestone**: interactive demo with zero idle CPU and dirty-node isolation.

### Phase 6 — Async Integration
- Continuation queue and frame checkpoint.
- Async computeds with hold semantics.
- **Milestone**: async data fetch drives UI updates without an executor.

### Phase 7 — Hardening
- Compaction strategy.
- Explicit handle release API.
- Performance benchmarks against GPUI baseline.
- **Milestone**: production-readiness report.

---

## 12. Open Problems and Trade-offs

| Problem | Status | Plan |
|---|---|---|
| Reclamation gap (arena growth) | Permanent constraint | Three documented strategies; user chooses per workload |
| Compaction requires handle rewriting | Solvable if handles don't escape runtime | Restrict public API to opaque handles |
| Store macro complexity | Accepted cost | Compile-time macro; opt-in for flat state |
| Async hold semantics in Rust | Partially solved by ACM port | Port continuation model; validate against `F-async-hold` |
| Universal trait expressiveness | Tunable | Start minimal; extend via capability sub-traits |
| Direct-path vs universal-path drift | Design risk | Shared test suite; universal path is a thin adapter over direct path |

---

## 13. Summary

This architecture delivers:

- **A reactive core** ported from R3's algorithm and validated against SolidJS 2.0's fundamental rules.
- **A store layer** using macro-generated field getters for Proxy-equivalent granularity in Rust.
- **An ownership stack** of generational arenas, owner trees, and RAII guards that handles dynamic DAG mutation deterministically.
- **A universal renderer contract** for third-party backends, following the SolidJS `createRenderer` pattern.
- **A GPUI-inspired first-party renderer** that bypasses the universal trait for performance, proving the ceiling.
- **A frame scheduler** that serves as the single checkpoint where reactive settlement and rendering meet.

The system is **one graph, one tree, one checkpoint**—explicit at the boundary, automatic internally, and portable by design.
