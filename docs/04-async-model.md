# 04 — Async Model

**Crates:** `reactive-core` (async state, holds) + `runtime` (spawner, queues)
**Owns:** flights, continuations, staged values, holds, reader classes,
boundaries, `isPending`, `latest`, and the async subset of the Solid 2.0 rules.

---

## 1. Scope

Solid 2.0's async model is large: holds, transactions, boundaries, display-ahead
readers, lanes, optimistic guesses, declared lag, deferred sources, actions, and
a dozen error-recovery rules. Implementing all of it in v1 would dominate the
schedule.

We implement the **navigation and loading model** first, because it is what makes
a native app feel correct:

- A change that needs async to be shown keeps the old screen until it can be
  shown whole (`F-1`, `F-2`, `F-7`).
- A reader holds only while it is the display (`F-3`).
- An armed boundary catches pending work and shows its fallback (`F-4`).
- A disarmed boundary keeps content and forwards the pending state (`F-5`).
- `isPending` is about a new question; a re-ask is quiet (`F-9`).
- Writes become visible at flush (`F-10`).
- An error is an answer; pending is not an error (`F-12`).

We defer:

- `F-6` display-ahead/lane semantics beyond a minimal `latest`/`isPending`;
- `F-11` optimistic writes;
- `F-8` (withdrawn in the reference) and the deferred/lag rules;
- optimistic store overlays and lane-derived async.

The node and transaction shapes leave room for them; see §11.

## 2. Vocabulary mapped to Rust

| Term | Rust representation |
|---|---|
| Flight | `Flight { id: u64, node: NodeId, abort: AbortHandle }` |
| Continuation | `Message::Landed { node, flight_id, result }` in the runtime queue |
| Staged value | `Node.async.staged: Option<Staged>` |
| Held write | A staged write whose `txn` is not committed |
| Transaction | `Txn { id, nodes, readers, flights, status }` |
| Reader | A compute/render effect registered on a txn or boundary |
| Display | A render effect |
| Boundary | A `LoadingBoundary` or `ErrorBoundary` view node |

## 3. Spawner and continuations

### 3.1 The contract

```rust
pub trait Spawner {
    fn spawn_local(&self, fut: LocalBoxFuture<'static, ()>);
    fn wake_ui(&self);
}
```

- The runtime calls `spawn_local` for every async memo and action.
- The future must be `'static` and run on the UI thread's local executor. It may
  await I/O; the actual I/O happens wherever the host's reactor runs.
- `wake_ui` asks the host's event loop to run a frame soon. It is called when a
  continuation is posted, never polled.
- The framework does not own a thread pool, timers, or a reactor. Hosts provide:
  - `ManualSpawner` for tests (futures run only when the test pumps),
  - `WinitSpawner` / `PlatformSpawner` for desktop,
  - `WasmSpawner` for the browser.

This is the same separation Leptos makes with `any_spawner`
(`refs/leptos/any_spawner`), but the runtime owns the frame queue rather than
effects being tasks.

**Polling context.** A future cannot receive `&mut Cx` through the standard
`Future` API. The host's local executor therefore polls on the UI thread with an
`ExecutorContext` installed by the runtime (an `Rc` handle or a thread-local that
points at the runtime). `ScopedFuture` uses it to set the node's owner and
observer for the duration of each poll. This is the only thread-local in the
system, and it is an executor detail, not graph ownership: graphs remain owned by
`Runtime`, and multiple runtimes can still exist as long as only one is polling
on a thread at a time.

### 3.2 Continuation queue

The runtime has a `VecDeque<Message>` guarded by the UI thread's ownership. A
completion may be posted from a background thread; the host adapter wraps it in
a channel and wakes the UI thread. The frame's phase 1 drains the queue:

```rust
enum Message {
    Landed { node: NodeId, flight: u64, result: Result<Value, Error> },
    Timer { id: TimerId },
    HostEvent(HostEvent),
}
```

Ordering: messages are applied in arrival order. Two completions for the same
node are ordered by flight id; the older one is dropped before it can stage.

### 3.3 Flight lifecycle

```
Idle ──run──► Loading (no committed value)
              └─run again (new question)──► supersede: abort/drop old, new flight
Idle/Ready ──run (new question)──► Refreshing { previous }
Ready ──run (quiet re-ask)──► Refreshing { quiet: true, previous }
Refreshing ──land──► Holding { staged } ──txn commit──► Ready
Loading ──land──► Holding { staged } ──txn commit──► Ready
Loading/Refreshing ──reject──► Error { error, previous }
```

Rules:

- A new flight increments `generation`. A completion whose generation is not the
  current one is discarded (`StaleFlight`).
- A run cancels the previous flight if the executor supports abort. If not, the
  previous future is dropped when the node is disposed or the generation changes;
  its completion is still discarded.
- A flight is owned by its node; disposing the node aborts it.
- The compute closure is called on the UI thread to record dependencies and
  produce a future. The future runs on the local executor; if the host runs it on
  the UI thread, user code inside it must not touch the graph except by staging
  writes. The landing is applied at the next pump, never from inside the future.

### 3.4 The async memo API

```rust
pub struct AsyncMemo<T> { /* NodeId */ }

impl<T: 'static> AsyncMemo<T> {
    pub fn new(
        cx: &Cx,
        compute: impl FnMut(&Cx) -> LocalBoxFuture<'static, Result<T, Error>> + 'static,
    ) -> Self;

    pub fn get(&self, cx: &Cx) -> Result<T, AsyncReadError>
    where T: Clone;
}
```

Alternatives considered:

- **Result style (chosen):** `get` returns `Result<T, AsyncReadError>` where
  `NotReady` is a distinct variant. The view layer's binding interprets
  `NotReady` by suspending/holding or by using `latest`.
- **Boundary style (rejected):** `get` returns `T`, and reading before ready
  panics/throws a `NotReady` error that only boundaries catch. This matches
  Solid's read-throw model, but it makes ordinary application code awkward in
  Rust and hides the control flow.

The Result style keeps control flow explicit, which is the Rust-native
translation of Solid's "throw to the graph, not to the caller" rule (`D-42`).

**Polling context.** The future is polled through a `ScopedFuture` wrapper that
sets the node's owner and observer before each poll and restores them after. This
is how reads after an `await` are tracked: without it, only reads in the
synchronous prefix would create dependencies. Leptos uses exactly this wrapper
(`refs/leptos/reactive_graph/src/computed/async_derived/mod.rs`,
`ScopedFuture::poll`). Actions, by contrast, poll untracked: they are commands,
not derivations, and should not re-run because a signal they read changed.

A `Resource<T>` convenience wraps `AsyncMemo<T>` with a `set_reload` trigger and
a `refresh()` method.

## 4. Reader classes

Every compute pass has a class, set by what created it:

```rust
pub enum ReaderClass {
    Derive,    // a memo or a user-effect compute
    Display,   // a render effect / binding
    Observe,   // isPending/latest probe
}
```

The class determines how a read of a pending/staged node is served:

| Node state | Derive | Display | Observe |
|---|---|---|---|
| Committed, clean | committed | committed | committed |
| Staged new value, txn open | join txn: stage result | serve committed; register replay | serve committed / verdict |
| Loading (no committed) | propagate `NotReady` | `NotReady` → boundary or hold | `NotReady` (loading) |
| Error | propagate error | boundary or halt | `isPending` false, `latest` errors |
| Flight in progress, same question | committed (quiet) | committed | `isPending` false |
| Flight in progress, new question | committed + pending flag | committed + register replay | `isPending` true |

Why `Display` gets the committed value: a render effect is the screen. If it
recomputed with a staged value, part of the screen would show the future while
the rest shows the present (`F-1`). It is served the present and re-run at the
commit. This is Solid's `F-3`/`A29` distinction, simplified to two classes plus
the observe probe.

Why `Derive` joins: a memo's value is shared by every reader; if it computed from
the staged world, its readers would see a future value without the screen
changing. So the pass joins the transaction and its result commits with it
(`F-2`, `D-1`).

### 4.1 Creation-time passes

A render effect created during apply (a `Show` branch, a new list row) runs its
first compute synchronously so the subtree is initialized before layout. It is a
`Display` reader: it sees committed values.

If that first compute reads a `Loading` node outside an armed boundary, the mount
is aborted and retried when the node lands. The parent structural binding keeps a
placeholder in its place. v1 requires async reads in newly mounted content to be
under an armed `LoadingBoundary`; mounting arbitrary async content into an
already-visible frame without a boundary is a documented limitation. The full
Solid model treats this as "born held" and ties the mount to the hold
(`A29`); we defer that until the lane model exists.

## 5. Holds and transactions

### 5.1 Opening a hold

A hold opens when a **display** pass cannot be completed with committed values:

1. The flush promotes writes and marks dirty.
2. A render effect computes. It reads a node that is `Loading` (no committed
   value) or a node whose new question is in flight.
3. The read returns `NotReady` (for `Loading`) or a `CommittedPending` marker
   (for a new question with a committed value).
4. The scheduler catches this at the effect:
   - If the effect is under an armed `LoadingBoundary`, the boundary records the
     reader and shows its fallback; the reader does not join a transaction
     (`F-4`). This applies to both `NotReady` and `CommittedPending`: an armed
     boundary catches anything pending under it, including a change whose old
     value is still displayable.
   - Otherwise, the scheduler opens or joins a transaction:
     - keep the effect's previously applied value (do not apply the new one);
     - register the effect as a reader on the txn;
     - record the flight(s) it waits on;
     - hold the flush's staged writes in the txn. Membership is the tick's
       (Solid's `D-4`): a write in the same synchronous update is part of the
       same change, even if it is not a direct dependency of this effect.
5. The frame continues: other independent changes commit. The txn stays parked.

`CommittedPending` is not an error. A display reader served a committed value
while a new question is in flight registers on the txn and continues to display
the committed value. The old frame remains intact.

### 5.2 Committing

A transaction commits when:

- every flight it is waiting on has landed (success or error);
- every derive reader that joined has recomputed under the landed values;
- no new flight was opened by those recomputes.

At commit:

1. Move each held node's staged value to committed.
2. Apply the parked effect values (the frame is now whole).
3. Run the effect applies that were stashed (in creation order).
4. Resume normal flushing; if the commit staged more writes, loop.
5. Drop the txn.

If a landed value is equal to the committed value (same question), the reader
recomputes and may produce an equal result; equality suppresses the apply. The
txn still completes, but no frame is shown as changed.

### 5.3 Supersession

If a new question starts before the previous flight lands:

- the old flight is superseded (generation bump);
- the old txn remains waiting only on flights that are still relevant; a txn
  with no live flights and no readers is dropped;
- a late landing from the old flight is discarded, never staged.

If a write to the same node arrives while a txn holds it:

- it joins the txn if the txn is still open (`F-2`'s write-joins rule);
- otherwise it starts a new txn.

### 5.4 Nested holds

A derive reader that joins a txn may itself start new async work (an async memo
that reads a held value). That new flight belongs to the same txn. This is how a
navigation cascade settles as one frame. The txn keeps a set of flights, not a
single "root" flight.

A derive that joins must not publish its value to mainline readers until commit.
Its node is marked `HELD`; reads from display readers get the committed value and
replay at commit; reads from other derives join the same txn.

### 5.5 Transaction identity

```rust
pub struct TxnId(u64);

pub struct Txn {
    pub id: TxnId,
    pub opened_at: FrameId,
    pub nodes: Vec<NodeId>,          // held staged values
    pub readers: Vec<ReaderId>,      // effects/derives to replay
    pub flights: Vec<FlightRef>,     // what we wait on
    pub quiet: bool,                 // all flights are re-asks
    pub status: TxnStatus,           // Open | ReadyToCommit | Committed | Dropped
}
```

Merging: two txns opened in one flush merge if their readers share a node. This
prevents two halves of one screen from revealing separately. Merge is union of
nodes/readers/flights; the older id survives.

## 6. Boundaries

### 6.1 LoadingBoundary

```rust
pub struct LoadingBoundary {
    armed: bool,
    on: Option<Memo<()>>,      // re-arm trigger (v1.1)
    content: ViewNodeId,
    fallback: ViewNodeId,
}
```

Semantics:

- **Armed** (fresh): a `NotReady` from a display reader under it is caught. The
  boundary shows its fallback and records the reader. No transaction is opened
  for that read. If the boundary itself is mounted by a held change, it appears
  with the hold's commit (the fallback is part of the held frame).
- **Disarmed** (has shown content): a `NotReady` is forwarded to the enclosing
  transaction. The boundary keeps showing content, registers as a display reader,
  and replays at commit. This is `F-5`.
- **`on` re-arm** (v1.1): when the `on` memo changes, the boundary arms again.
  If something under it is pending, it shows the fallback with the change's
  frame; otherwise the arm is a no-op.
- **Nested boundaries**: the nearest armed boundary catches. A disarmed inner
  boundary forwards to the outer boundary/transaction.

v1 simplification: content behind a fallback is **disposed** when the fallback is
shown, and rebuilt on reveal. Solid keeps it alive but off screen. Disposing is
simpler and avoids "hidden content holds nothing" bookkeeping; it loses scroll
position and local state. Provide a `KeepAlive` wrapper later if needed.

### 6.2 ErrorBoundary

```rust
pub struct ErrorBoundary {
    content: ViewNodeId,
    fallback: ViewNodeId,     // fallback receives the error
}
```

- A compute error under the boundary sends it to the fallback. Readers under the
  fallback hold nothing.
- The error fallback stays until a new answer replaces the errored value. A
  successful retry re-renders content.
- `NotReady` is never routed here; pending is not an error.
- An error that escapes every boundary is a root error: report, freeze the
  graph in a halted state, ignore later writes until `reset_error_halt()`.

### 6.3 Boundary placement rules

- A boundary's `on` memo is evaluated outside the boundary; its reads hold like a
  plain reader.
- A verdict about content that may have no committed value belongs inside the
  boundary that owns the content.
- A boundary mounted *inside* a held frame appears with the commit, not before.

These are direct translations of `F-4`/`F-5`/`F-9`.

## 7. Verdicts: `isPending` and `latest`

### 7.1 `isPending`

```rust
pub fn is_pending<T>(cx: &Cx, memo: AsyncMemo<T>) -> bool
```

Returns true iff a **new question** has no committed answer:

- a dependency changed and the new flight has not landed;
- a landed value is staged in an open txn;
- an `affects()` mark is live.

Returns false when:

- a re-ask (`refresh`) is in flight;
- the node is loading (no committed value) — that is loading, not pending;
- the node is idle or errored with a committed previous value and no new question.

v1 implements the first two causes and `affects` as a simple mark API. The
"landed value staged" case is the txn's `staged` flag.

### 7.2 `latest`

```rust
pub fn latest<T: Clone>(cx: &Cx, memo: AsyncMemo<T>) -> Result<T, LatestError>
```

- If a value is staged (held), return the staged value.
- Else return the committed value.
- Else return `Err(Loading)` (no value has ever been produced).

`latest` is a display-ahead read: it does not join the txn and does not make the
frame wait. Using it inside a derived memo is unsupported in v1 (it would derive
from a future value without a lane; the result is a normal memo that recomputes
at commit, which can tear against the screen). Restrict it to render bindings and
diagnostics.

### 7.3 `affects`

```rust
pub fn affects<T>(cx: &Cx, node: &T);
```

Declares "this data is changing" without a value write. It marks the node and
its derivations pending until the next commit. Used for optimistic UI and for
manually declared reloads. In v1 it is a simple counter with a commit-time
release; it does not propagate through the full mark-sentinel machinery.

## 8. Actions (v1.1)

An action is an explicit hold around a future:

```rust
pub fn action<F, T>(cx: &Cx, f: F) -> ActionHandle<T>
where F: FnOnce(ActionCx) -> LocalBoxFuture<'static, Result<T, Error>>;
```

- Opening an action opens a txn and sets `action_depth += 1`.
- Writes inside the action (before or after awaits) stage into the txn if the
  target node is already in it; new nodes join on first write.
- The txn commits when the future resolves and all flights it started have
  landed.
- `flush()` inside an action is refused (dev error); the action's writes are not
  visible until it settles.

Rust has no ambient async context, so a write after an `await` is associated with
the action only if it goes through the `ActionCx` (which carries the txn id) or
the node is already stamped. We require `ActionCx` for writes; this is stricter
than Solid and avoids the "bare await loses the transaction" bug. A plain
`Signal::set` from a detached task is a mainline write.

This is a deliberate Rust-native improvement: the transaction is a capability,
not ambient thread state.

## 9. Rule subset and what each buys

| Rule | v1? | Mechanism |
|---|---|---|
| `F-1` screen is whole | Yes | Display reads committed; txn commit is atomic. |
| `F-2` async changes are held | Yes | `NotReady`/`CommittedPending` opens a txn. |
| `F-3` reader holds while display | Partial | Display readers register per txn; disposed readers are removed. No "fallback hidden reader" edge cases. |
| `F-4` armed boundary catches | Yes | First-load fallback; no txn for caught readers. |
| `F-5` disarmed boundary forwards | Yes | Keeps content; reader joins txn. |
| `F-6` display-ahead lanes | Minimal | `latest`/`isPending` only; no derived lanes. |
| `F-7` nothing reveals ahead | Yes by construction | Only txn commit reveals; `latest` is explicit. |
| `F-9` pending semantics | Subset | New question vs re-ask; no store-wide marks. |
| `F-10` flush visibility | Yes | Staged writes. |
| `F-11` optimistic | No | Deferred. |
| `F-12` errors | Yes | ErrorBoundary; pending is not an error. |

### 9.1 Documented deviations from Solid 2.0

- **Reads are `Result`, not throws.** `NotReady` is a value the view layer handles;
  it is not an exception thrown through user code. See `D-42` for the rule we are
  translating.
- **Content is disposed on fallback entry.** Solid keeps it alive but off screen.
  Use `KeepAlive` (v1.1) for state that must survive.
- **No born-held mounts outside a boundary.** A newly mounted subtree that reads
  loading async must be under an armed boundary. The full model ties the mount to
  the hold (`A29`); we defer it.
- **`latest` is display-only.** It is not allowed in derived state in v1 because
  there is no lane to keep the derivation consistent.
- **No optimistic writes or lanes.** The node shape reserves space; the
  machinery is not present.
- **Actions require `ActionCx`.** There is no ambient async context in Rust, so a
  write after `await` joins an action only through the action's capability.

## 10. Testing

Deterministic executor:

```rust
#[test]
fn navigation_holds_old_page() {
    let mut rt = TestRuntime::new();
    rt.mount(page_a);
    rt.click("next");
    // page B's fetch is now in flight
    assert_scene!(rt, "page A still visible");
    rt.land(page_b_data);
    assert_scene!(rt, "page B whole");
}
```

Required tests:

- First load with and without an armed boundary.
- Navigation: click, fetch, old page stays, new page appears whole.
- Disarmed boundary keeps content during a refetch.
- Nested boundaries: nearest armed catches; disarmed forwards.
- Supersession: two quick fetches; only the last lands.
- Equal landing: no repaint.
- Error: rejected fetch shows error fallback; retry replaces it.
- Cancellation: dispose mid-flight; no landing, no panic.
- `isPending`: true for a new question, false for a re-ask, false for loading.
- `latest`: shows staged, errors before first value.
- Action: writes before/after await commit together; `flush` inside is refused.
- Fuzz: random graph of async memos, writes, and boundaries; assert every frame
  is a snapshot consistent with some committed txn state.

## 11. Future: lanes and optimism

The node shape reserves:

- `staged` with a `TxnId` (already used);
- `guess`/`override` value + source (unused);
- `lane` id on a txn (unused);
- `reader_class` can grow `DisplayAhead` without changing the enum's call sites.

When we add `F-6`/`F-11`:

1. A `latest`/guess read enters a lane txn whose world is "screen + guesses".
2. A lane holds on its own async separately from the parent txn.
3. A guess is a displayed value over a hold; it reverts at commit or on a
   differing truth.
4. The store layer gets per-key guesses and rebase.

Do not start this until the v1 navigation model is stable and the conformance
suite is green. The reference implementation
(`refs/solid/packages/signals/src/core/{lanes,optimistic,guesses}.ts`) shows how
much machinery is involved; it is not a weekend addition.

## 12. Open questions

1. **Result vs throw.** We chose `Result` reads. Does this make boundary
   composition too explicit? Could a `?`-based view binding hide the plumbing
   while keeping the semantics?
2. **Boundary content lifetime.** v1 disposes on fallback; Solid keeps alive.
   Which is the better default for a native app with scroll/focus state?
3. **Action writes after await.** `ActionCx` is required. Is that too
   restrictive for library code that takes a plain `Cx`?
4. **`affects` in v1.** Is a simple mark enough, or should it be omitted until
   the full verdict machinery exists?
5. **Merging transactions.** Sharing a reader is our merge criterion. Is sharing
   a *node* enough, or does that over-merge independent widgets?
6. **Re-ask detection.** We use `refresh()` as the only quiet path. Should an
   `AsyncMemo` that recomputes with equal tracked inputs be automatically quiet?
   (Solid infers this from the question; we could hash dependency values.)
