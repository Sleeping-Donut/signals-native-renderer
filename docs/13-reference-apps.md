# 13 — Reference Apps

**Owns:** the small Solid 2.0 programs we recreate as end-to-end sanity checks,
the behavior each one must preserve, and the frames to assert.

These are not feature demos. They are **parity checks**: small enough to port in
an afternoon, dense enough that a wrong reactive core or a wrong retained tree
shows up immediately. Port them in order; each one is a milestone gate.

## 1. Rules of use

- **Port behavior, not code.** The snippets are reproduced from Solid's docs and
  demos for reference. Solid is MIT, but the implementation must be written from
  the behavior, not transliterated line by line.
- **The acceptance checks are the spec.** If our API makes a check awkward, that
  is a design finding, not a reason to weaken the check. Record the finding in
  [12-decisions-and-risks.md](12-decisions-and-risks.md) Part F.
- **Run the Solid version when in doubt.** These are cheap to run with Vite and
  are the oracle for "what should happen when I click this".
- **Keep the frames.** The checks assert whole frames, not individual bindings.
  A locally correct binding that reveals in the wrong frame is still a failure
  (`F-1`).
- **Add the port to the app-level suite** ([11-testing-and-performance.md](11-testing-and-performance.md)
  §2.3). A demo that only exists in a doc rots.

## 2. Demo 1 — Counter: local vs shared state

**Source:** Solid's counter example. Two instances of one component; each has a
local signal, both share a module-level signal, and a derived value is read
inline.

```tsx
import { createMemo, createSignal } from "solid-js";
import { render } from "@solidjs/web";

const [sharedCount, setSharedCount] = createSignal(0);

function Counter() {
  const [localCounter, setLocalCount] = createSignal(0);
  const localDouble = () => localCounter() * 2;

  return (
    <div>
      <button onClick={() => setLocalCount((c) => (c = c + 1))}>
        local {localCounter()}
      </button>
      <button onClick={() => setSharedCount((c) => (c = c + 1))}>
        shared {sharedCount()}
      </button>
      <div>local double {localDouble()}</div>
    </div>
  );
}

function App() {
  return (
    <div>
      <Counter />
      <Counter />
    </div>
  );
}

render(() => <App />, document.getElementById("app")!);
```

`createMemo` is imported but unused. That is part of the point: `localDouble` is
a plain function, not a memo.

### 2.1 What it exercises

- **Split signals.** Two reads and two writes per instance; no combined handle.
- **Per-mount state.** `Counter` runs once per mount, so each instance owns its
  local signal. The shared signal lives above both.
- **Derived reads are not nodes by default.** `localDouble` recomputes on every
  read inside the render binding. Memoizing it would still be correct, but it
  would add a node and hide the distinction this demo teaches.
- **Write batching.** A click is one update; the button text and the double line
  change in one frame.
- **Static structure.** No conditional or list; the tree never rebuilds.

### 2.2 Expected frames

Initial: `local 0 / shared 0 / local double 0` in both instances.

| Action | Instance A | Instance B |
|---|---|---|
| Local +1 in A | `local 1`, double `2` | unchanged |
| Shared +1 in A | both shared text `1` | shared text `1` |
| Local +1 in B | unchanged | `local 1`, double `2` |

No action changes the other instance's local value, and no local action changes
the shared value.

### 2.3 Port sketch

```rust
#[derive(Clone, Copy)]
struct SharedCount {
    value: ReadSignal<u32>,
    bump: WriteSignal<u32>,
}

#[component]
fn Counter(shared: Prop<SharedCount>) -> impl View {
    let shared = shared.get_untracked(cx); // stable Copy handles
    let (local, set_local) = signal!(cx, 0);
    let local_double = memo!(cx, |cx, _prev| local.get(cx) * 2);

    view! {
        <Column gap=8.0 padding=12.0>
            <Button on_click=move |cx, _| set_local.update(cx, |c| *c += 1)>
                "local " {local}
            </Button>
            <Button on_click=move |cx, _| shared.bump.update(cx, |c| *c += 1)>
                "shared " {shared.value}
            </Button>
            <Text>"local double " {local_double}</Text>
        </Column>
    }
}

#[component]
fn App() -> impl View {
    let (shared, set_shared) = signal!(cx, 0);
    let shared = SharedCount { value: shared, bump: set_shared };

    view! {
        <Column gap=16.0 padding=16.0>
            <Counter shared=shared />
            <Counter shared=shared />
        </Column>
    }
}
```

Notes:

- `{local}`, `{shared.value}`, and `{local_double}` are text bindings, not
  component re-runs.
- `local_double` is a `memo!`, so it is cached and notifies only when the value
  changes. The Solid original used an uncached getter; this port deliberately
  exercises the separate-derived-value authoring. If we want the uncached
  shape, keep a plain function and read it in a binding:
  `{move |cx| local_double(cx, local)}`.
- Passing a `Copy` bundle of handles as a prop avoids context plumbing and makes
  ownership obvious: both counters receive the same handles, and each owns its
  `local`. A `SharedCount` context or a small `create_counter` helper is equally
  valid; the check is behavior, not wiring.

### 2.4 Acceptance checks

1. `counter_local_isolation` — click local in A; A's local text and double
   change; B's local text and double do not; both shared texts do not.
2. `counter_shared_propagation` — click shared in A; both shared texts change in
   one frame; both local values and doubles do not.
3. `counter_no_rebuild` — the operation log shows text updates only, no
   structure create/destroy after mount.
4. `counter_two_mounts` — disposal of one instance leaves the other's local and
   the shared value intact.

**Milestone:** M1 headless (signals and bindings), M3 on the null renderer
(operation log), M4–M5 natively.

## 3. Demo 2 — States and cities: the async system

**Source:** Solid's "states" example. An async memo feeds a writable derived
signal, which feeds another async memo, which feeds a second writable derived
signal. A `Loading` boundary owns the first load; `isPending` marks a refetch
without falling back.

```tsx
import { createMemo, createSignal, For, isPending, Loading } from "solid-js";
import { render } from "@solidjs/web";

const CITIES: Record<string, string[]> = {
  California: ["Los Angeles", "San Francisco", "San Diego"],
  Florida: ["Miami", "Orlando", "Tampa"],
  "New York": ["New York City", "Buffalo", "Rochester"],
  Texas: ["Houston", "Dallas", "Austin"],
};

// Stand-ins for network requests.
const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
const getStates = async () => (await sleep(500), Object.keys(CITIES));
const getCities = async (state: string) => (await sleep(500), CITIES[state] ?? []);

function App() {
  // Async memos read like plain values, even when one depends on another.
  const states = createMemo(getStates);
  const [state, setState] = createSignal(() => states()[0]);
  const cities = createMemo(() => getCities(state()));
  // A writable signal derived from the list: picking a new state resets the
  // city on its own, with no effect to keep them in sync.
  const [city, setCity] = createSignal(() => cities()[0]);

  return (
    <Loading fallback={<p>Loading…</p>}>
      <select onChange={(e) => setState(e.currentTarget.value)}>
        <For each={states()}>{(name) => <option>{name}</option>}</For>
      </select>{" "}
      <select disabled={isPending(cities)} onChange={(e) => setCity(e.currentTarget.value)}>
        <For each={cities()}>{(name) => <option>{name}</option>}</For>
      </select>
      <p>
        Selection: {city()}, {state()}
      </p>
    </Loading>
  );
}

render(() => <App />, document.getElementById("app")!);
```

### 3.1 What it exercises

- **Async is a compute mode.** `states` and `cities` are memos whose compute is
  async. There is no `AsyncMemo` type; they read like memo values.
- **A sync derived signal can be pending.** `state` and `city` have synchronous
  computes but read async values, so they suspend on first load and replay at
  commit. This is the clearest small proof that pending belongs to the node, not
  to a special node kind.
- **Writable derived signals.** `createSignal(fn)` is Solid 2.0's function-form
  signal ("writable memo"): a user write lands immediately, and a source change
  re-runs the function, which decides what to keep. Here both functions ignore
  the previous value, so a new list resets the selection. There is no effect
  syncing `state` and `city`.
- **Chained async questions.** `cities` reads `state`, so its first run cannot
  start until `states` has answered. The first load is sequential, not parallel.
- **A disarmed boundary keeps content.** After the first load, changing the state
  starts a refetch; the boundary does not fall back, the old lists and selection
  text stay, and the city control is disabled by `isPending`.
- **The declared inconsistency of pending.** While the new cities are in flight,
  the frame shows the old committed values (the old lists and the old selection
  text) beside the verdict that a change is pending (the city control is
  disabled). That is expected: display-ahead applies to the **verdict**, not to
  the values. Do not "fix" it by clearing the city, falling back, or showing the
  new state text early.
- **`For` over async values.** Both lists render from memos that may be pending;
  neither list is a `Vec` rebuilt by a component re-run.

### 3.2 Expected timeline (no `on`, 500 ms each)

`t = 0`: the boundary is armed, both memos are pending, fallback shows
`Loading…`.

`t = 500 ms`: `states` lands, `state` derives `California`, `cities` starts.
Content is still not ready because the content pass reads `cities` pending while
the boundary has not shown content yet; fallback stays.

`t = 1000 ms`: `cities` lands, `city` derives `Los Angeles`. Content appears:
states list, California's cities, `Selection: Los Angeles, California`.

Choose Florida:

`t = 1000 ms + click`: the write lands in the graph, and `cities` starts a new
flight. The frame is held: the display keeps committed values, so the lists and
`Selection: Los Angeles, California` stay put. The verdict is display-ahead:
`isPending(cities)` is true, so the city control is disabled. No fallback: the
boundary is disarmed (`F-5`).

`t = +500 ms`: `cities` lands with Florida's list, `city` re-derives to `Miami`.
Frame: Florida's cities, city control enabled, `Selection: Miami, Florida`.

Choosing a city is immediate and has no further effect: the derived signal does
not re-run until `cities` changes.

**A control detail.** Solid's `<select>` is uncontrolled, so the state picker
visually shows Florida as soon as the user picks it, even though the graph's
displayed `state` is still California until the commit. If our select is
controlled by `state`, it will show California until the commit and then jump.
That difference is in the widget, not the graph: assert the `<p>` and the lists,
and decide separately whether a control may show a local selection ahead of its
bound value.

### 3.3 Port sketch

```rust
let states = Memo::new_async(cx, |_cx| async move { get_states().await });

let (state, set_state) = derived_signal(cx, move |cx, _prev| {
    states.get(cx).first().cloned().unwrap_or_default()
});

let cities = Memo::new_async(cx, move |cx| {
    let state_name = state.get(cx).clone();   // sync prefix; owns before await
    async move { get_cities(&state_name).await }
});

let (city, set_city) = derived_signal(cx, move |cx, _prev| {
    cities.get(cx).first().cloned().unwrap_or_default()
});

view! {
    <LoadingBoundary fallback=|| view! {
        <Column padding=16.0>
            <Text>"Loading…"</Text>
        </Column>
    }>
        <Column gap=12.0 padding=16.0>
            <Select
                items=states
                selected=state
                on_select=move |cx, name| set_state.set(cx, name)
            />
            <Select
                items=cities
                selected=city
                disabled=move |cx| is_pending(cx, cities)
                on_select=move |cx, name| set_city.set(cx, name)
            />
            <Text>
                "Selection: " {city} ", " {state}
            </Text>
        </Column>
    </LoadingBoundary>
}
```

Notes:

- `Select` is a **component-library** widget (a `components` crate built on the
  element vocabulary), not a core element like `Column`. If the component layer
  does not exist yet, the fallback is two `For` lists of selectable rows; the
  reactive wiring is identical.
- `derived_signal` is the right constructor for `state` and `city`: they are
  writable, their compute is synchronous, and pending comes from the async read.
  `async_derived_signal` would also work but would misrepresent the compute.
- `cities`'s closure must read `state` **before** returning the future. A `&Cx`
  cannot cross an `await`; the owned `String` is what the future needs.
- `set_state` and `set_city` are the write halves. The demo never calls them from
  an effect; the resets are consequences of the derived computes.
- If you add `on={state.get(cx)}` to the boundary, the expected frames change:
  the fallback would show on every state change. That is the `on` distinction in
  [04-async-model.md](04-async-model.md) §6.1; keep this demo without it.

### 3.4 Acceptance checks

1. `states_first_load_is_sequential` — fallback at 0 and 500 ms; content only
   after both requests land; assert no frame contains a partial list or an empty
   selection.
2. `states_initial_selection` — first content frame has California and Los
   Angeles selected.
3. `states_pending_keeps_committed_lists` — after choosing Florida, before the
   refetch lands: the displayed state and city are still California and Los
   Angeles, the lists are still California's, the city control is disabled,
   `is_pending(cities)` is true, and there is no fallback. The graph's `state`
   value is Florida; only the display is held. This is the v1 verdict-only
   display-ahead check ([04-async-model.md](04-async-model.md) §7.1).
4. `states_derived_reset` — when Florida's cities land, city becomes Miami with
   no effect and no extra compute of the city node.
5. `states_manual_city_survives_until_source_change` — choosing Orlando persists
   across unrelated frames; changing the state resets it.
6. `states_async_is_compute_mode` — the type-level test asserts `states` and
   `cities` are `Memo`, `state` and `city` are read/write pairs, and no public
   async node type exists.
7. `states_frame_integrity` — every frame is a legal pairing of committed values;
   the only "mixed" frame is the declared pending one (new state, old city).
8. `states_cancel_supersession` — choose Florida, then Texas before Florida
   lands; Florida's answer never commits; the selection resets from Texas's
   list.

**Milestone:** M2 headless (holds, boundaries, pending), M4 on the null renderer
(lists and bindings), M5–M6 natively (controls and keyboard).

## 4. Porting notes

| Solid 2.0 | Ours |
|---|---|
| `createSignal(v)` | `signal!(cx, v)` → `(ReadSignal<T>, WriteSignal<T>)` |
| `createSignal(fn)` (writable memo) | `derived_signal(cx, \|cx, prev\| ...)`; not a thunk value |
| `createMemo(syncFn)` | `Memo::new(cx, ...)` |
| `createMemo(asyncFn)` | `Memo::new_async(cx, ...)` |
| `isPending(x)` | `is_pending(cx, x)` (v1 subset) |
| `<Loading>` | `<LoadingBoundary>`; first load catches, later loads forward |
| `<For each=...>` | `<For each=... key=...>`; key is explicit |
| `disabled={expr}` | `disabled=...` reactive attribute |
| `onClick` | `on_click` |
| plain function read | per-read derived expression; no node unless memoized |
| `<select>/<option>` | `Select`/`ListBox` in the `components` crate, not a core element |

Rust-specific traps:

- **A closure is a value.** `signal(cx, || 42)` is a signal holding a closure,
  not a derived signal. Derivation is `derived_signal`. The macro escape hatch is
  `signal!(cx, (|| 42))`.
- **No function overloads.** Solid's `createSignal(fn)` and `createSignal(value)`
  are one API by overloading; ours are different constructors by design.
- **No effect is a feature.** If a demo needs an effect to keep two values in
  sync, the derived-signal model is being used wrong; fix the data flow first.
- **Whole frames.** A check that passes by looking only at the changed binding is
  not a check. Snapshot the frame.

## 5. Adding a demo

1. Run the Solid version and write the expected frames down first.
2. Name the behaviors it exercises; map each to a rule or invariant in the docs.
3. Port it with the smallest view vocabulary that preserves behavior. Do not add
   widgets just for the demo.
4. Add the acceptance checks to the app-level suite. Include one negative check
   (the tempting wrong behavior).
5. Link it from this page and from the milestone it gates.

## 6. Later candidates

- **Keyed list reorder** — stable identity, focus and state preserved, only moved
  rows are touched. Gates M4.
- **Error boundary** — a failing async compute replaces the subtree; a domain
  `Result` stays a value. Gates M2/M3.
- **Navigation with `on`** — the product-page example from
  [04-async-model.md](04-async-model.md) §6.1; only after the v2 `on` work.
- **Actions and optimism** — write, pessimistic wait, optimistic reveal,
  revert. v2.
- **Store form** — field-granular updates, nested paths, patch only changed
  fields. Gates M7.
- **Focus and keyboard** — tab order, IME, accessibility tree. Gates M6.
