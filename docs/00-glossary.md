# 00 — Glossary

Terms are defined once here. Section docs use them without re-definition.

## Graph

**Node** — an entry in the reactive graph: a signal, memo, effect, or render
effect. Identified by `NodeId`.

**Signal** — a source node with a settable value.

**Memo** — a derived node. Its value is computed from tracked reads and cached
until a dependency changes. It is a subscriber to its dependencies and a source
to its own subscribers. A memo may be sync or async; async is a compute mode, not
a different node type.

**Async compute** — a compute function that returns a future. Any derived signal
or memo may use one; a plain writable signal does not. Async nodes carry cold
flight/pending state; sync nodes do not.

**Effect** — a node with a *compute* half (tracked reads only) and an *apply*
half (side effects). The compute half runs in the graph; the apply half runs
after all computes settle.

**Render effect** — an effect whose apply half runs during the render phase and
whose writes target a view node or scene state. It is still an ordinary graph
node; "render" describes where it applies, not a second graph.

**Dependency** — an edge from a node that was read to the node that read it.

**Subscriber** — the target of a dependency edge; a node that must be checked
when the dependency changes.

**Height** — an integer maintained per node such that a subscriber's height is
greater than every dependency's height. Used to run the dirty heap in
topological order.

**Dirty** — a node whose value definitely changed.

**Check** — a node that may have changed because an upstream dependency may have
changed. It is verified by re-checking dependencies before recomputing.

**Clean** — a node whose value is known current.

**Flush** — the act of promoting staged writes, propagating dirtiness, running
the compute heap, and settling. One flush per frame, or an explicit manual flush
in tests and imperative code.

**Staged value** — a value written since the last flush. It is not visible to
readers until the flush that carries it (`F-10`).

**Tracked read** — a read performed while a computation is the current observer,
which creates a dependency edge.

**Untracked read** — a read that creates no edge.

## Ownership

**Owner** — a scope in the ownership tree. Owns cleanups, context values, and
child owners created while it was current.

**Disposal** — the act of tearing down an owner subtree: children first, then
cleanups in LIFO order, then the owner's values.

**Handle** — a `Copy` value identifying a graph node or view node. Valid only
while the slot's generation matches. A handle is a lookup key, not a reference.

**Reclamation** — returning a disposed node's memory to the arena free list.
Values are dropped at disposal; slots are reused with a bumped generation.

**No resurrection** — a disposed handle never becomes live again. This is a
deliberate difference from JavaScript implementations that can retain references
to disposed computations.

**Cleanup handle** — an RAII value that removes a cleanup when dropped.

**Effect handle** — a `Copy` capability over an effect with explicit `dispose`.
Dropping it does nothing; the owner or an explicit call disposes.

**Context** — a typed value attached to an owner and available to descendants.

## Async

**Spawner** — the host-supplied trait that schedules a local future. The
framework owns queues and wakeups, not threads.

**Flight** — one in-progress async computation of a node, identified by a
generation. A newer flight supersedes an older one; stale completions are
dropped.

**Continuation** — a completion posted to the runtime's queue, to be applied at
the next frame's pump phase.

**Hold** — a set of staged writes whose reveal waits for async work that on-screen
readers need (`F-2`).

**Transaction** — the bookkeeping for a hold: held nodes, stashed effect runs,
reader registrations, and the frame at which it commits.

**Reader** — a computation that displays a value. In the full Solid model only a
render effect is a reader; memos participate through their readers. In v0.1, our
`ReaderClass` distinguishes *derive*, *display*, and *observe*.

**Display** — a render effect that puts a value on screen.

**Boundary** — a view that catches pending or error state from its subtree.

**Loading boundary** — an *armed* boundary shows a fallback while content is
pending; once it has shown content it is *disarmed* and forwards pending state
into a hold (`F-4`, `F-5`).

**Error boundary** — a boundary that catches errors and shows an error fallback
(`F-12`).

**Pending** — a node whose observable value is not the final answer to the
current question (`F-9`). `isPending` is the view of this state.

**Loading** — a node with no committed value yet. Loading is not pending; it
suspends to a boundary or holds the mount.

**Latest** — the newest value a node has produced, including a held staged value.
`latest` is display-ahead: it may show a held change before the screen reveals
it (`F-6`). It arrives with the lane model in v0.2.

**Display-ahead** — a read that shows a held value or a verdict about it now,
without joining the hold. `isPending` is the v0.1 form; `latest` arrives with lanes
in v0.2 (`F-6`).

**Lane** — the world a display-ahead read opens: the screen plus what it shows
ahead. A lane holds on its own async separately from the hold it reads. Since
Solid's 2026-11-12 amendment, an async pass that reads ahead asks the screen's
question as a sync pass would, so a lane can hold on both the frame's question
and its own. Lanes and `latest` are v0.2; v0.1 ships only the `isPending` probe.

**Question** — the inputs a node's value answers. An input change asks a new
question; `refresh()` re-asks the same one quietly (`F-9`).

**Re-ask** — a request for the same question while a flight is in progress. It is
quiet: `isPending` stays false, and the fresh value reveals silently.

**Declared lag** — a source that intentionally keeps its previous answer while
refetching (`createDeferred`). It lags but never leads.

**Optimistic write / guess** — a tentative value shown while a hold is open
(`F-11`). Not in v0.1.

## View and rendering

**View node** — an entry in the retained view tree: element, text, fragment,
component, conditional, list, or overlay.

**Mount** — the one-time construction of a component's view subtree.

**View slot** — a stable host node whose one active child subtree is replaced
when its key changes. `Rebuild` is sugar for a slot keyed on a closure's tracked
reads. Used for runtime component/view switching and whole-subtree replacement;
`Show`/`Switch`/`For` are separate specialized control-flow nodes.

**Binding** — an effect that writes one dynamic property of a view node. It
subscribes to the signals it reads and updates the view/scene when they change.

**Prop** — a value passed from a parent to a child, represented as `Prop<T>`
(`Static(T)`, `Reactive(ReadSignal<T>)`, or `Derived(Memo<T>)`). There are no bare
`T` props for values that may change. `Static` allocates no graph node.

**Style** — a resolved layout+paint description attached to a view node. The
public style API is builder-based; resolution is a separate step.

**Layout** — geometry produced by the layout engine (taffy). Retained on the view
node.

**Scene** — a frame-local, ordered list of paint primitives.

**Paint range** — a `Range<usize>` into the scene, cached per view node. Clean
nodes replay their range.

**Damage** — the union of changed regions; used by the backend to decide how much
to rasterize/present. Optional in v0.1.

**Keyed list** — a list whose rows are identified by a key, so reordering
preserves row state and bindings.

**Virtualization** — rendering only visible rows of a large list. Orthogonal to
keyed identity; a virtualized list still needs stable keys for visible rows.

## Renderer contract

**Renderer API** — the small, object-safe trait set a third-party backend
implements (`renderer-api` crate; formerly "universal").

**Capability** — an optional trait a renderer may implement (text, images,
scrolling, virtualization, accessibility, animation).

**Null renderer** — a headless renderer that records operations and emits a
deterministic log. Used for conformance and CI.

**Native renderer** — the first-party renderer (`renderer-native`; formerly
`renderer-gpui`). It bypasses the universal contract on its hot path.

**Conformance suite** — the shared tests every renderer must pass.

## Frame and runtime

**Runtime** — the owner of the graph, view tree, layout, scene, queues, clock,
and frame loop.

**Cx** — the per-call context that borrows the runtime and tracks the current
owner, observer, and frame phase. All reactive operations take `&Cx`.

**Signal** — an internal full node handle. Public code gets the split pair from
`create_signal()`: `ReadSignal<T>` (get/with) and `WriteSignal<T>`; pending is
read with `is_pending(cx, |cx| ...)`
(set/update). There is no combined public signal and no `RwSignal`.

**Core constructor** — reactive constructors use one `create_*` family:
`create_signal`, `create_derived_signal`, `create_async_derived_signal`,
`create_memo`, `create_async_memo`, `create_lazy_memo`, and `create_derived`.
`create_derived` is the uncached read-only handle; `_signal` means a writable
pair. `Store::new` and `ViewSlot::new` are single-type constructors.

**`create_*` helper** — custom primitive/abstraction wrappers such as
`create_resource`, `create_theme`, and `create_counter`. The prefix is shared
with the reactive constructors; the distinction is the return type and
documentation, not the naming.

**Phase** — one of pump, flush, apply, layout, scene, present.

**Checkpoint** — the point at the end of a flush where staged values commit and
the frame's visible state is decided.

**Idle** — no work pending; the runtime sleeps until a wake source fires.

**Wake source** — host input, timer, async completion, animation request, or
explicit `request_frame`.

**Frame cause** — why a frame is running: input, async, animation, resize,
manual, or initial. Diagnostics and tests use it.
