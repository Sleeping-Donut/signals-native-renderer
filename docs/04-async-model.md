# 04 — Async Model

**Crates:** `reactive-core` (async state, holds) + `runtime` (spawner, queues)
**Owns:** flights, continuations, staged values, holds, reader classes,
boundaries, `isPending`, and the v1 async subset of the Solid 2.0 rules.
`latest`, actions, `affects`, and full `on` are v2.

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

- `F-6` display-ahead/lane semantics, including `latest`;
- `F-11` optimistic writes;
- actions and `affects` (they rely on the hold/verdict machinery we are not
  building yet);
- full `LoadingBoundary::on` re-arm (v1 uses a `key` remount; see §6.1);
- `F-8` (withdrawn in the reference) and the deferred/lag rules;
- optimistic store overlays and lane-derived async.

`isPending` stays in v1 in its basic form (a new question is in flight or a
landed answer is held). It does not need optimism or lanes.

The node and transaction shapes leave room for the deferred pieces; see §11.

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

/// Optional: the host may provide a thread pool / tokio / rayon.
pub trait BackgroundSpawner {
    fn spawn(&self, fut: BoxFuture<'static, ()>) -> TaskHandle;
}
```

- The runtime calls `spawn_local` for every async memo (and, in v2, action).
- The future must be `'static` and run on the UI thread's local executor. It may
  `await` I/O; the actual I/O happens wherever the host's reactor or background
  executor runs.
- `spawn_background` (the optional trait) is for CPU work, blocking I/O, or
  parallelism. Its future must be `Send + 'static`, and its result must come back
  through a continuation; it must not touch the graph.
- `wake_ui` asks the host's event loop to run a frame soon. It is called when a
  continuation is posted, never polled.
- If the host has no background spawner, user code may use its own executor
  (`tokio::spawn`, `std::thread::spawn`, rayon) and send the result back through
  a `UiHandle`/channel. The framework never owns threads.
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

### 3.4 Async is a compute mode, not a node type

The first draft made `AsyncMemo<T>` a separate type. That is colored: it says a
memo is either sync or async, and a helper that takes `Memo<T>` cannot accept the
async one. It also loses Solid's writable derived signal shape, because it
separates "async" from "signal".

The correct model is orthogonal:

- **`ReadSignal<T>` / `WriteSignal<T>`** — the split view of a writable source:
  plain (`signal`), derived writable (`derived_signal`, Solid's
  `createSignal(fn)`), or async derived writable (`async_derived_signal`). A plain
  source has no compute and is always ready.
- **`Memo<T>`** — read-only derived. Its compute may be synchronous or
  asynchronous.
- **Async is a property of the compute function**, not the node kind. Any
  derived signal or memo can be async.
- The read halves (`ReadSignal`, `Memo`) use the same public read API (`get`,
  `is_pending`, `with`); both can be pending. A plain signal simply never is.
- The async state is cold: an optional `AsyncState` on the node. Sync nodes never
  allocate or touch it; the read fast path checks one flag/niche.

Constructors differ because Rust has no overloading, but they return the same
types:

```rust
Memo::new(cx, |cx, prev| ...)          // sync
Memo::new_async(cx, |cx| ...)          // async
let (read, write) = signal(cx, value);// plain source
let (read, write) = derived_signal(cx, |cx, prev| ...); // writable derived
let (read, write) = async_derived_signal(cx, |cx| ...);         // writable derived, async
```

A helper taking `Memo<T>` does not care how it was built. `AsyncMemo<T>` is not a
distinct type or read contract; keep it, if at all, as a type alias for
readability.

`async_derived_signal` is not an "async node type." It is a **writable derived
signal whose compute happens to be async**, and it returns the same
`(ReadSignal<T>, WriteSignal<T>)` pair as a sync derived signal. If you only need
a read-only async derivation, use `Memo::new_async` instead; there is no write
half.

**Async lifecycle.** The constructor stores the compute closure and returns
immediately with the handles; it does not block or `.await`.

1. The derive run calls the closure under the node's owner/observer to get a
   future. The synchronous prefix records tracked reads.
2. The runtime wraps the future in `ScopedFuture` (owner/observer restored on
   each poll) and hands it to the host's UI-local `Spawner`.
3. The future is polled by the local executor. A read of the signal before the
   result is ready suspends to a `LoadingBoundary` or holds the mounting change.
4. On completion, the result is posted as a continuation. At the next frame's
   pump, it becomes a staged value; the transaction/boundary decides when it
   reveals.
5. If a dependency changes first, the old flight's generation is bumped, its
   cancellation token is signalled, and a new run starts. The old completion is
   discarded.
6. An `Err` result is an error answer: `ErrorBoundary` catches it. Pending is
   not an error.

The compute closure should read reactive dependencies synchronously before
returning the future, or carry an owned context handle; a `&Cx` cannot be held
across an `await` in a `'static` future. The example below reads nothing after
the await, so it is fine.

**Rejected: one `SignalFactory<T>` trait for all constructors.** A trait with
impls for `T`, `FnOnce() -> T`, and `FnOnce() -> Future<Output = T>` cannot be
written as stated:

- the impls overlap (`T` can itself be a closure or a future);
- `FnOnce` consumes the factory, so it cannot recompute when dependencies
  change; a reactive compute must be `FnMut(&Cx, Option<&T>)` (or `FnMut(&Cx)`
  for async);
- it conflates a plain source, a derived writable signal, and an async derived
  signal, which have different write/recompute/pending semantics;
- it has no `&Cx`, owner, flight, cancellation, or equality, so it cannot
  allocate or drive a real node;
- the async branch needs `LocalBoxFuture`/`ScopedFuture` and a host executor,
  not a bare `tokio::spawn`.

A wrapper (`Value(v)`, `Sync(f)`, `Async(f)`) or a `Compute` enum can make one
call shape work, but the wrapper is still visible and the constructors are still
semantically distinct. Keep the explicit constructors.

**No internal `Source` enum.** An enum with
`Box<dyn FnMut(&Cx, Option<&T>) -> T>` / `Box<dyn FnMut(&Cx) -> LocalBoxFuture>`
would centralize setup but introduce type erasure and dynamic dispatch on every
recompute. Instead, each explicit constructor stores its own compute type and
shares private generic setup helpers where useful. A `signal!`/`memo!` macro can
dispatch at compile time directly to those constructors, so it does not need a
runtime enum either.

Optional macro sugar:

```rust
signal!(cx, 42);                         // create_signal
signal!(cx, |cx, prev| ...);             // create_derived_signal
signal!(cx, async |cx| ...);             // create_async_derived_signal
memo!(cx, |cx, prev| ...);               // create_memo
memo!(cx, async |cx| ...);               // create_async_memo
```

The async closure is the **public** syntax. The macro lowers it to the
constructor shape:

```rust
signal!(cx, async |cx| { ... })
// expands to
create_async_derived_signal(cx, move |cx| async move { ... })
```

Why lower it? The constructor stores `FnMut(&Cx) -> LocalBoxFuture<'static,
Result<T, Error>>`; `async |...|` implements `AsyncFnMut`, not that bound
directly. The `move`/`async move` pair makes captures owned and `'static`, which
is required to store the compute. If the constructor instead took an
`AsyncFnMut` bound, the async closure could be passed directly, but storing and
calling it repeatedly (associated future, object safety, HRTB) is harder. The
lowering is an implementation detail; users write `async |cx| ...`.

A `macro_rules!` front-end is enough for the common forms; a proc macro is more
robust for typed/pattern closure arguments and async-closure syntax. The macro is
optional sugar, not the runtime mechanism.

```rust
pub struct Memo<T> { /* NodeId */ }
pub struct ReadSignal<T> { /* NodeId + read capability */ }
pub struct WriteSignal<T> { /* NodeId + write capability */ }

impl<T: 'static> Memo<T> {
    pub fn new(
        cx: &Cx,
        compute: impl FnMut(&Cx, Option<&T>) -> T + 'static,
    ) -> Self;

    pub fn new_async(
        cx: &Cx,
        compute: impl FnMut(&Cx) -> LocalBoxFuture<'static, Result<T, Error>> + 'static,
    ) -> Self;

    /// Read the value. Inside a tracked computation, a not-ready value suspends
    /// the computation; the scheduler holds/replays it. Outside a tracked
    /// computation, check `is_pending`; a not-ready `get` is a diagnostic, not
    /// a value.
    pub fn get(&self, cx: &Cx) -> T;

    /// Verdict only: is a new question still in flight or staged?
    pub fn is_pending(&self, cx: &Cx) -> bool;

    pub fn with<R>(&self, cx: &Cx, f: impl FnOnce(&T) -> R) -> R;
}

impl<T: 'static> ReadSignal<T> {
    pub fn get(&self, cx: &Cx) -> T;
    pub fn is_pending(&self, cx: &Cx) -> bool;
    pub fn with<R>(&self, cx: &Cx, f: impl FnOnce(&T) -> R) -> R;
}

impl<T: 'static> WriteSignal<T> {
    pub fn set(&self, cx: &Cx, value: T);
    pub fn update(&self, cx: &Cx, f: impl FnOnce(&mut T));
}
```

A plain `signal()` pair never suspends and `read.get` always returns a value. A
derived writable signal may suspend if its compute is async; writes through
`write.update` set the `prev` value for the next compute, matching Solid's
function-form signal. There is no combined public signal and no `RwSignal`.

**`get` is the public read.** There is no `try_get`, `try_read`, or `try` method:

- `try` is a reserved Rust keyword; an inherent method cannot use that name.
- More importantly, a public `Result` read colors every helper. A function that
  reads a signal would have to return `Result<T, ReadError>` or unwrap, and
  `ReadError` would leak through layers that have nothing to do with loading.
- The scheduler uses a private `NotReady` control token internally. It is not a
  public error and should never be converted into one.
- `is_pending` is the verdict. Boundaries handle loading and error UI; helpers
  take plain values.
- There is no defaulting read (`get_or`). A default would lie about the graph:
  it would return a value the graph does not have. Use `is_pending` and a
  boundary/domain state instead.

Solid has no defaulting read either. Its closest tools are `latest(x)` for the
newest produced value, `isPending(x)` for the verdict, and
`createDeferred`/`loadingValue` for stale/default UI. Those are truthful about
the state; a `get_or` default is not.

**Coloring and composition.** If every read returned `Result`, then every
function that reads a signal either returns `Result<T, ReadError>` or unwraps.
`?` flattens the types but not the signatures: `fn label(user: &User) -> String`
becomes `fn label(user: &User) -> Result<String, ReadError>`. Do that through five
layers and the code is colored. `get()` avoids that: it suspends at the
computation boundary; a helper stays a plain function.

Push reads to the edges: read in a binding/memo and pass plain values down. This
is good style regardless. `with`/`read_with` closures localize a read when you
want to avoid clones. At an imperative edge, check `is_pending`; do not invent a
default.

**Where `.await` appears.** `.await` is confined to async producers and explicit
waits:

- inside an async memo/signal compute, for the actual I/O or work
  (`fetch_user(id).await`, `sleep(...).await`, joining tasks);
- inside actions and background tasks;
- in imperative code that explicitly waits (`until(...).await`,
  `resolve(...).await`, awaiting a task handle).

It does **not** appear in view/binding/render code or in ordinary signal/memo
reads. `get(cx)` is synchronous; it may suspend the current computation through
the graph, but it is not an `async fn`. When dependencies change, the async
compute runs again and its internal `.await`s run again; consumers still read
synchronously.

**`?` is for your errors, not `NotReady`.** In

```rust
let user = fetch_user(id).await?;
```

`fetch_user` returns `Future<Output = Result<User, FetchError>>`. `.await` gives
you `Result<User, FetchError>`, and `?` unwraps `Ok(User)` or returns
`Err(FetchError)` from the async block. The async block returns
`Result<User, AppError>`, so `?` converts via `From<FetchError> for AppError`.
That error becomes the memo's error state and is caught by an `ErrorBoundary` —
it is not `NotReady`.

`NotReady` is produced only by **reactive reads** (`get`, store reads), and it is
control flow, not an application error:

```rust
#[derive(Debug, thiserror::Error)]
enum AppError {
    #[error(transparent)]
    Fetch(#[from] FetchError),
    // ...
}

let user = Memo::new_async(cx, {
    let user_id = props.user_id;
    move |cx| async move {
        let id = user_id.get(cx);          // sync tracked read; no ?
        let user = fetch_user(id).await?;  // ? is FetchError -> AppError
        Ok(user)
    }
});
```

Rules that keep them separate:

- Use `get(cx)` for reactive dependencies. It suspends the computation on
  `NotReady`; it never returns a value that `?` could unwrap.
- Do not default a pending value. `is_pending` tells you whether a new question
  is in flight; a boundary or a domain state carries the loading/error UI. A
  default would be a value the graph does not have.
- `LoadingBoundary` catches pending; `ErrorBoundary` catches the memo's `Err`.
  A 404 from `fetch_user` is an `Err`, so it is an error, not loading.
- If a 404 is domain absence rather than failure, return a domain value instead:

```rust
match fetch_user(id).await {
    Ok(user) => Ok(ProfileState::Ready(user)),
    Err(FetchError::NotFound) => Ok(ProfileState::Missing),
    Err(e) => Err(AppError::from(e)),
}
```

- If you need to read another async node inside an async compute, either
  `get(cx)` it (the graph holds the outer compute until it is ready) or await its
  readiness explicitly (`until`/`resolve`).

**Examples at the edges.** Reads belong in bindings, memos, and render effects;
helpers take plain values. (The sketches pass `&Cx` into compute closures so
reads are tracked; constructor names are illustrative.)

```rust
// 1. Colorless in a memo: the memo stays pending, its readers hold.
let label = Memo::new(cx, move |cx| {
    let user = user.get(cx);          // suspends if not ready
    format!("{} ({})", user.name, user.role)
});

// 2. Colorless in a render binding, under a boundary.
let binding = render_effect(
    cx,
    move |cx| user.get(cx).name.clone(),
    move |cx, name| cx.set_text(name_node, name),
);

// 3. A helper takes a value; it never reads a signal and never colors.
fn greeting(user: &User) -> String {
    format!("Hi {}", user.name)
}

let text = Memo::new(cx, move |cx| greeting(&user.get(cx)));

// 4. Imperative edge: read only when the value is ready.
if !count.is_pending(cx) {
    log_click(count.get(cx));
}

// 5. Verdict only; no value read.
if !user.is_pending(cx) {
    let user = user.get(cx);
    open_profile(cx, user.id);
}
```

Notice what does **not** happen: `greeting` never returns `Result`; the memo
never returns `Result`; only the async producer's `?` names `FetchError`. That is
the coloring discipline.

**The same component with a prop and a local signal:**

```rust
#[derive(Clone)]
struct ProfileCardProps {
    user_id: Prop<UserId>,
}

fn profile_ui(
    user: &User,
    id: UserId,
    show_details: ReadSignal<bool>,
    set_show_details: WriteSignal<bool>,
) -> View {
    view! {
        <Column gap=8>
            <Text>{id}</Text>
            <Text>{user.name.clone()}</Text>
            <Button on_click=move |cx, _| set_show_details.update(cx, |v| *v = !*v)>
                "Details"
            </Button>
            <Show when=move || show_details.get(cx)>
                <Text>{user.bio.clone()}</Text>
            </Show>
        </Column>
    }
}

#[component]
fn ProfileCard(props: ProfileCardProps) -> impl View {
    let (show_details, set_show_details) = signal(cx, false);

    let user = Memo::new_async(cx, {
        let user_id = props.user_id;
        move |cx| {
            let id = user_id.get(cx);
            async move { fetch_user(id).await }
        }
    });

    view! {
        <LoadingBoundary fallback=|| view! { <ProfileSkeleton /> }>
            <ViewSlot
                key=move |cx| props.user_id.get(cx)
                render=move |cx| {
                    let user = user.get(cx);          // suspends if not ready
                    profile_ui(&user, props.user_id.get(cx), show_details, set_show_details)
                }
            />
        </LoadingBoundary>
    }
}
```

No `Result` in `profile_ui` or in the slot render. If `user` is not ready, the
graph suspends the render and the boundary shows the skeleton. When the user
lands, the slot renders the profile.

**Without `view!`:**

```rust
fn profile_card(cx: &mut Cx, props: ProfileCardProps) -> ViewNodeId {
    let (show_details, set_show_details) = signal(cx, false);

    let user = Memo::new_async(cx, {
        let user_id = props.user_id;
        move |cx| {
            let id = user_id.get(cx);
            async move { fetch_user(id).await }
        }
    });

    let root = cx.element(ElementKind::Column);

    let title = cx.text("");
    cx.bind_text(title, move |cx| props.user_id.get(cx).to_string());
    cx.insert(root, title, None);

    let slot = ViewSlot::new(
        cx,
        move |cx| props.user_id.get(cx),
        move |cx| {
            let user = user.get(cx);              // suspends if not ready
            profile_ui_builder(
                cx,
                &user,
                props.user_id.get(cx),
                show_details,
            )
        },
    );
    cx.insert(root, slot, None);

    let toggle = cx.element(ElementKind::Button);
    cx.set_text(toggle, "Details");
    cx.on_click(toggle, move |cx, _| {
        set_show_details.update(cx, |v| *v = !*v);
    });
    cx.insert(root, toggle, None);

    root
}
```

`profile_ui_builder` is the same content without `view!`:

```rust
fn profile_ui_builder(
    cx: &mut Cx,
    user: &User,
    id: UserId,
    show_details: ReadSignal<bool>,
) -> ViewNodeId {
    let column = cx.element(ElementKind::Column);

    let id_text = cx.text(&id.to_string());
    cx.insert(column, id_text, None);

    let name = cx.text(&user.name);
    cx.insert(column, name, None);

    let bio = cx.text(&user.bio);
    let details = cx.conditional(
        move |cx| show_details.get(cx),
        move |_cx| bio,
        None,
    );
    cx.insert(column, details, None);

    column
}
```

`ViewSlot` is the host; `profile_ui` is the content. The slot re-runs the async
read when `user_id` changes and replaces the child subtree. `profile_ui` could
be a component, but a component mounts once; without a keyed host it would not
re-mount for a new user. `ElementKind::Column` is the native vertical stack;
`view!`'s `<Column>` lowers to it. There is no DOM here.

### 3.5 Cancellation and background threads

A flight is cancelled by dropping its future (and, where supported, aborting the
task). That is **cooperative** for background work:

- dropping an async future stops it at the next `await`; it does not kill a
  thread that is running blocking/CPU code;
- `BackgroundSpawner::spawn` returns a `TaskHandle`; `abort`/`cancel` is
  cooperative and executor-specific;
- a raw `std::thread` cannot be safely killed. The work must check a
  `CancellationToken`/`AtomicBool` or stop on a channel message.

The compute closure receives a `FlightCtx`:

```rust
pub struct FlightCtx {
    cancel: CancellationToken,
}

impl FlightCtx {
    pub fn is_cancelled(&self) -> bool;
    pub fn token(&self) -> CancellationToken;
}
```

Typical pattern:

```rust
Memo::new_async(cx, |cx| {
    let ctx = cx.flight_ctx();
    let bg = cx.background_spawner();       // Clone + 'static handle
    async move {
        let token = ctx.token();
        let task = bg.spawn(async move {
            // long CPU / blocking work
            for chunk in work {
                if token.is_cancelled() { return Err(Cancelled); }
                process(chunk);
            }
            Ok(value)
        });
        task.await?
    }
})
```

When a dependency changes while the flight is running:

1. the memo re-runs and bumps the flight generation;
2. the old future is dropped/aborted and its token is cancelled;
3. the old background work should observe the token and stop early;
4. if it finishes anyway, the landing is discarded because its generation is
   stale.

The framework guarantees (4) regardless of cooperation; (3) is the user's
responsibility. This is why blocking work belongs in `spawn_background`, not
`spawn_local`: a UI-local future that blocks cannot be cancelled until it yields.

### 3.6 Implementing the colorless suspension

`get() -> T` needs a way to stop a synchronous computation without changing its
signature. These are the implementation paths worth prototyping.

#### Path A — private panic payload (recommended default)

```rust
struct NotReady(());
#[cold]
fn suspend() -> ! {
    std::panic::resume_unwind(Box::new(NotReady(())));
}
```

- Each computation runs under `catch_unwind(AssertUnwindSafe(|| compute()))`.
- If the payload is `NotReady`, the scheduler marks the node pending, registers
  the reader/flight, and parks or shows a boundary fallback.
- If the payload is anything else, rethrow it (user panic).
- A panic hook checks `PanicHookInfo::payload().is::<NotReady>()` and suppresses
  the default report, so a normal first load does not print a panic.
- On unwind, Rust runs destructors, so RAII guards clean up. The scheduler must
  still dispose the partial owner tree created by the failed pass.
- `Cx` sets `suspended = true` before unwinding. If a computation returns normally
  while `suspended` is set, someone swallowed the token; emit
  `CAUGHT_REACTIVE_READ` and treat the value as pending. That makes the failure
  loud instead of silently publishing a partial value.
- Catch at each computation, not only at the render effect, so a memo can be
  marked pending and its reader can re-suspend cleanly.
- `panic = "abort"` cannot use this path; `get()` is required.
- Unwinding across `extern "C"` is undefined/aborts. User computations must not
  cross FFI before a suspension; document this.

Costs and risks: a landing pad per computation (measure; likely small), panic
machinery on the suspension path (microseconds, only on first load/hold), a panic
hook that must cooperate, and the fact that a user `catch_unwind` can intercept
the token. The `suspended` flag turns interception into a diagnostic. This is the
closest to Solid's throw model and the only path that is truly colorless.

#### Path B — internal `Result` fallback (advanced, not public API)

For `panic = "abort"` builds, the computation runtime can carry a `Result`
internally: the scheduler knows a pass suspended, and the public read stays
`get`/`is_pending`. There is no public `try` method. This path is an
implementation fallback for targets without unwinding, not the authoring API;
library code still receives plain values, and boundaries still own loading/error
UI.

- Safe, portable, works with `panic = "abort"`, no panic hooks.
- Not colorless for computations that need to suspend; those targets must use
  a boundary/domain state or enable unwinding.
- The public surface is unchanged: `get`, `is_pending`, `with`.

#### Path C — custom `setjmp`/`longjmp`

Rejected. It is platform-specific, unsafe, skips Rust destructors, and interacts
badly with async and `Drop`. Do not use.

#### Path D — sentinel value + pending flag

`get()` returns `T::default()` (or `MaybeUninit`) and sets `Cx.suspended`. The
caller may use the bogus value before the scheduler notices; side effects,
indexing, and logic run on garbage. Rejected as unsound or requiring a sandbox
for every computation.

#### Path E — CPS / `with` closures

`signal.with(|v| ...)` avoids suspension for the closure body, but you cannot
early-return from the enclosing function, and async/hold logic still has to stop.
Useful for library APIs that want to avoid clones, not a general colorless read.

#### Path F — macro-only lowering

The `view!`/`#[component]` macros can lower `get()` at a binding boundary to the
internal suspension/Result path for targets without unwinding. This helps
bindings but not arbitrary helper functions; a helper that calls `get()` still
needs a suspension mechanism. It is a complement to Path A/B, not a replacement.

#### Recommendation

Implement Path A first (colorless `get`) where unwinding is available. Use the
internal Path B only for `panic = "abort"` targets. The public API stays
`get` / `is_pending` / `with`; there is no public `try` method. Prototype both on
the same navigation/hold tests, measure the `catch_unwind` overhead, and keep the
`suspended` flag diagnostic. If unwinding is unacceptable on a target, that
target uses a boundary/domain state or enables unwinding; no public semantics
change.

## 4. Reader classes

Every compute pass has a class, set by what created it:

```rust
pub enum ReaderClass {
    Derive,    // a memo or a user-effect compute
    Display,   // a render effect / binding
    Observe,   // isPending probe (latest arrives with lanes in v2)
}
```

The class determines how a read of a pending/staged node is served:

| Node state | Derive | Display | Observe |
|---|---|---|---|
| Committed, clean | committed | committed | committed |
| Staged new value, txn open | join txn: stage result | serve committed; register replay | serve committed / verdict |
| Loading (no committed) | propagate `NotReady` | `NotReady` → boundary or hold | `NotReady` (loading) |
| Error | propagate error | boundary or halt | `isPending` false |
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
    key: Option<Memo<Key>>,    // v1: changing the key remounts the boundary
    on: Option<Memo<()>>,      // v2: dependency-list re-arm
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
- **`key` remount (v1)**: if the key memo changes, the boundary is disposed and
  remounted under the same parent. The new boundary is fresh/armed, so a load
  shows the fallback, then content. This covers the common navigation/reload
  shape without the full `on` semantics. State inside the content is lost.
- **`on` re-arm (v2)**: when the `on` memo changes, the boundary arms again
  without remounting. If something under it is pending, it shows the fallback
  with the change's frame; otherwise the arm is a no-op. This requires the full
  boundary/txn interaction (`F-4`/`F-5`, `D-15`/`D-16`), so it is deferred.
- **Nested boundaries**: the nearest armed boundary catches. A disarmed inner
  boundary forwards to the outer boundary/transaction.

Why `on` is v2: it is not just "reset a bool". It must identify the frame of the
change that armed it, keep the fallback off screen when the same data is read
outside the boundary, collect pending readers that were already forwarded, and
interact with lanes when the `on` read is display-ahead. Those are the cases in
`D-15`/`D-16`; implementing the simple version would produce a boundary that
sometimes shows a fallback with the wrong frame or strands a hold. The `key`
remount gives the same observable behavior for the common case with far less
machinery.

**What `on` is for.** A boundary that has shown content normally stays disarmed
during a refetch: the old content remains on screen until the new data lands.
That is right for "same page, refreshing data". It is wrong when the dependency
that defines the page changed: showing product A's page while product B loads is
stale content. `on` is a dependency list that says "when any of these reads
change, this is a new question, so show the fallback for the new content."

In Solid:

```jsx
<Loading on={productId()} fallback={<Spinner />}>
  <ProductDetails id={productId()} />
</Loading>
```

- The value of `on` is never compared; only what it reads matters.
- It takes an expression, not a function: `on={productId()}`. Passing
  `on={() => productId()}` reads nothing and never notifies.
- The fallback follows the frame of the change. In a navigation where the shell
  reads `product(id)` outside and comments read inside, the fallback appears with
  the new shell (`[A] -> [B + spinner] -> [B + comments]`), not beside the old
  page.
- If the same data is also read outside the boundary, the frame may already be
  waiting on it and the fallback can never be shown; that is a structural
  diagnostic (`LOADING_ON_OUTSIDE_HOLD`), not a bug in `on`.
- Children are not re-created in Solid; they stay alive behind the fallback.
  Our v1 disposes content, so a `key` remount is equivalent in observable
  behavior for the common case.

**Leptos equivalent.** Leptos does not have an `on` dependency list. Its coarse
version is the choice between `<Suspense>` and `<Transition>`:

- `<Suspense>` shows the fallback whenever a resource under it is pending,
  including after the first load.
- `<Transition>` keeps the old content after the first load and never falls back
  for later changes.
- To force a fallback for a specific change, you key/remount the branch (or use
  the router's route-keyed view). That is exactly our v1 `key` remount.

Solid's `on` is the granular middle: keep the boundary disarmed for refetches,
but re-arm it for a named dependency without remounting children. That is why it
is v2 for us: it needs the fuller boundary/txn model, while `Suspense` vs
`Transition` and keyed remount cover the common cases in v1.

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

## 7. Verdicts: `isPending` (v1), `latest` and `affects` (v2)

### 7.1 `isPending`

```rust
pub fn is_pending<T>(cx: &Cx, memo: Memo<T>) -> bool
```

Returns true iff a **new question** has no committed answer:

- a dependency changed and the new flight has not landed;
- a landed value is staged in an open txn.

Returns false when:

- a re-ask (`refresh`) is in flight;
- the node is loading (no committed value) — that is loading, not pending;
- the node is idle or errored with a committed previous value and no new question.

`affects()` marks are v2; v1 implements the two flight/staging causes. The
"landed value staged" case is the txn's `staged` flag.

### 7.2 `latest` (v2)

```rust
pub fn latest<T: Clone>(cx: &Cx, memo: Memo<T>) -> Result<T, LatestError>
```

- If a value is staged (held), return the staged value.
- Else return the committed value.
- Else return `Err(Loading)` (no value has ever been produced).

`latest` is a display-ahead read: it does not join the txn and does not make the
frame wait. It belongs with the lane model because a derived value over `latest`
needs a lane to stay consistent. Without lanes, using it in a memo can tear
against the screen, so v1 does not expose it. It returns with the v2 lane work.

### 7.3 `affects` (v2)

```rust
pub fn affects<T>(cx: &Cx, node: &T);
```

Declares "this data is changing" without a value write. It marks the node and
its derivations pending until the next commit. It is the declared-pending half of
the optimistic/verdict model, so it moves with that model to v2.

## 8. Actions (v2)

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
| `F-6` display-ahead lanes | No (v2) | `latest` and lanes are deferred. |
| `F-7` nothing reveals ahead | Yes by construction | Only txn commit reveals; there is no v1 display-ahead read. |
| `F-9` pending semantics | Subset | New question vs re-ask; basic `isPending`; no `affects`/store-wide marks. |
| `F-10` flush visibility | Yes | Staged writes. |
| `F-11` optimistic | No (v2) | Deferred with actions/affects. |
| `F-12` errors | Yes | ErrorBoundary; pending is not an error. |

### 9.1 Documented deviations from Solid 2.0

- **The public read is `get`.** `get()` suspends inside a tracked computation via
  a private `NotReady` token; there is no public `Result`/`try` read and no
  defaulting read. `is_pending` is the verdict. This is not a throw-to-user-code
  channel; `D-42` still applies.
- **Content is disposed on fallback entry.** Solid keeps it alive but off screen.
  Use `KeepAlive` (v1.1) for state that must survive.
- **No born-held mounts outside a boundary.** A newly mounted subtree that reads
  loading async must be under an armed boundary. The full model ties the mount to
  the hold (`A29`); we defer it.
- **No `latest` in v1.** It arrives with the lane model in v2.
- **No optimistic writes, actions, or `affects` in v1.** The node shape reserves
  space; the machinery is v2.
- **Actions require `ActionCx` (v2).** There is no ambient async context in Rust,
  so a write after `await` joins an action only through the action's capability.

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
- `get`: returns `NotReady` before first value; `get` suspends inside a
  computation and resumes at commit; `get` outside a tracked computation errors.
- Fuzz: random graph of async memos, writes, and boundaries; assert every frame
  is a snapshot consistent with some committed txn state.

## 11. v2: lanes, optimism, actions, `latest`, `on`

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
5. Actions, `affects`, and full `on` arrive with the same verdict/hold
   machinery; they are not independent features.

Do not start this until the v1 navigation model is stable and the conformance
suite is green. The reference implementation
(`refs/solid/packages/signals/src/core/{lanes,optimistic,guesses}.ts`) shows how
much machinery is involved; it is not a weekend addition.

## 12. Open questions

1. **Colorless read mechanism.** We chose the private-unwind `get()`; there is
   no public try/default read. The open part is whether the unwind should be a
   real panic payload or a custom `ControlFlow`-based unwinder; `panic = "abort"`
   forces the internal fallback.
2. **Boundary content lifetime.** v1 disposes on fallback; Solid keeps alive.
   Which is the better default for a native app with scroll/focus state?
3. **Action writes after await (v2).** `ActionCx` is required. Is that too
   restrictive for library code that takes a plain `Cx`?
4. **`affects` (v2).** It should move with the verdict machinery; confirm the
   arity and store coverage at that point.
5. **Merging transactions.** Sharing a reader is our merge criterion. Is sharing
   a *node* enough, or does that over-merge independent widgets?
6. **Re-ask detection.** We use `refresh()` as the only quiet path. Should an
   `Memo` that recomputes with equal tracked inputs be automatically quiet?
   (Solid infers this from the question; we could hash dependency values.)
