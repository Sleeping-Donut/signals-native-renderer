# 09 — Input, Focus, and Accessibility

**Crate:** `platform` + `view` (input state) + `renderer-native` (hitboxes)
**Owns:** hit testing, event dispatch, actions/keybindings, focus, tab order,
gestures, IME, cursors/tooltips, and the accessibility tree.

**Implementation status (2026-11-19).** The retained input slice is in
[`crates/view`](../crates/view/src/lib.rs): layout-bounds hit testing, pointer
move/down/up and click bubbling, hover/pressed state, view click listeners,
focus handles and tab traversal, disabled-node skipping, focused/global key
bindings, focus fallback on removal, and an accessibility snapshot with
stable ids and press/focus actions. [`renderer-api`](../crates/renderer-api/src/lib.rs)
owns `Key`, `KeyEvent`, `Modifiers`, and `PointerEvent`;
[`renderer-native`](../crates/renderer-native/src/lib.rs) maps winit pointer and
keyboard events into `WindowDriver`. Content masks/transforms, capture/stop
results, drag/drop, scroll, IME, AccessKit protocol wiring, and cursor/tooltip
layers remain.

---

## 1. Input model

Input is a one-way flow into the graph:

```
platform event
   → runtime pump (phase 1)
   → hit test / focus route
   → listener callback
   → signal writes (staged)
   → flush / apply
   → bindings update the view
```

A listener never mutates the view tree directly. It stages signal writes or calls
a command API that stages them. This keeps input and rendering single-threaded
and ordered.

## 2. Hit testing

### 2.1 Hitboxes

Every interactive view node can own a `Hitbox`:

```rust
pub struct Hitbox {
    pub id: HitboxId,
    pub node: ViewNodeId,
    pub bounds: Bounds<Px>,          // layout bounds
    pub content_mask: ContentMask<Px>, // clip rect
    pub behavior: HitboxBehavior,    // BlockMouse | BlockMouseExceptScroll | Normal
    pub cursor: Option<Cursor>,
}
```

Hitboxes are collected during layout/paint and stored in paint order. Hit testing
walks them in reverse paint order (topmost first):

```rust
fn hit_test(point: Point<Px>) -> HitTest {
    let mut hits = Vec::new();
    for hitbox in hitboxes.iter().rev() {
        if hitbox.content_mask.contains(point)
            && hitbox.bounds.contains(point) {
            hits.push(hitbox.id);
            if hitbox.behavior == BlockMouse { break; }
        }
    }
    HitTest { hits, hover: hits.first().copied() }
}
```

`ContentMask` is the intersection of all ancestor clips; a point outside the mask
does not hit the node. This is the GPUI model (`window.rs` `Hitbox`,
`ContentMask`, `hit_test`) and it handles clipping, transforms, and overlays
correctly.

### 2.2 Transforms

If a node has a transform, the point is inverse-transformed into the node's local
space before the bounds test. Store the transform on the hitbox or resolve it
during hit testing. v1: support translation and scale; rotation later.

### 2.3 Overlays and layers

Overlays (menus, tooltips, popovers) are painted in a higher layer. Their
hitboxes are later in paint order, so they win. A modal overlay can add a
full-window blocker hitbox with `BlockMouse`.

## 3. Dispatch

### 3.1 Dispatch tree

The view tree itself is the dispatch tree; each interactive node has a stable
`ViewNodeId`. Dispatch is a walk from the target node to the root (bubble) and
from the root to the target (capture):

```rust
pub enum DispatchPhase { Capture, Bubble }

pub fn dispatch(event: &mut Event, target: ViewNodeId, cx: &mut Cx) {
    let path = path_to_root(target);
    for node in path.iter().rev() { node.capture(event, cx); }
    for node in path { node.bubble(event, cx); }
}
```

Because nodes are retained, the path is a parent walk; no per-frame dispatch tree
needs to be built. This is simpler than GPUI's `DispatchTree` (which exists
because its element tree is ephemeral) and a direct benefit of retained identity.

### 3.2 Listeners

```rust
div()
    .on_click(|cx, event| count.update(|c| *c += 1))
    .on_key_down(|cx, event| { ... })
```

The builder registers a listener on the view node:

```rust
pub struct Listener {
    pub event: EventKind,
    pub phase: DispatchPhase,
    pub callback: Box<dyn FnMut(&mut Cx, &Event) -> DispatchResult>,
}
```

`DispatchResult`:

- `Continue`: keep bubbling;
- `Stop`: stop propagation;
- `Handled`: stop and mark handled (for default action suppression).

Listeners are owned by the view node's owner and dropped with it. A removed node
cannot receive events.

### 3.3 Default actions

Some events have default behavior (scroll on wheel, text input, tab navigation).
Dispatch first gives listeners a chance; if unhandled, the default action runs.
For text input, the focused `TextInputHandler` is the default target.

## 4. Actions and key bindings

Actions are typed commands:

```rust
#[derive(Clone, PartialEq, Action)]
#[action(namespace = "app")]
struct Increment;

div().on_action::<Increment>(|cx, action| { ... });
```

Key bindings map keystrokes to actions in a context:

```rust
KeyBinding::new("cmd-+", Increment, Some("app"));
```

Dispatch:

1. Build the keystroke from the platform event (key + modifiers).
2. Find the deepest focusable node with a matching binding in its context chain.
3. Dispatch the action down the focus path (capture) and up (bubble).
4. If unhandled, run the global binding.

Actions are not signals; they are commands. A handler may stage writes. This
separates "what happened" from "what state changed", which makes keymaps
configurable and testable.

## 5. Focus

### 5.1 Focus handles

```rust
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct FocusHandle { id: FocusId }
```

- A focusable view node owns a `FocusHandle`.
- `window.focus(handle)` sets focus; `handle.is_focused(window)` reads it.
- Focus is stored per window, not per node, so a node can be remounted and
  re-focused.
- A focus ring is a binding: `focused = focus.is_focused()`; the style binding
  draws the ring.

### 5.2 Focus path and tab order

- The focus path is the chain from the root to the focused node.
- Tab order is the set of nodes with `tab_index` (explicit) or natural DOM-like
  order (tree order), grouped by `tab_stop` containers.
- `focus_next`/`focus_prev` walk the ordered list, skipping disabled/hidden
  nodes.
- A focused node removed from the tree transfers focus to the nearest surviving
  ancestor or the last focused sibling. GPUI has careful rules for this
  (`FocusHandle`, `focus_lost_restore_target`); we implement the simpler subset:
  parent, then previous sibling, then root.

### 5.3 Focus and lists

A keyed row keeps its `FocusHandle` across reorder because the row node is
stable. If the focused row is removed, focus moves to the list or the next row.
Do not rebuild the list on reorder; that is the whole point of keyed identity.

### 5.4 Focus and conditionals

`Show` disposes the inactive branch, so focus inside it is lost. Use `KeepAlive`
if focus must survive a tab switch. Document this; do not make every conditional
keep both branches alive by default.

## 6. Pointer gestures

### 6.1 Basic state

Per view node:

- hovered (from hit test)
- pressed (pointer down over it)
- active (pressed and still over it)
- dragging (pressed + moved beyond threshold)

State is stored in a `PointerState` keyed by `ViewNodeId` and cleared when the
node is removed. This is retained, so hover does not require a full tree walk
each frame; the hit test result updates a small set of nodes.

### 6.2 Click

A click is:

- pointer down on node A;
- pointer up on node A (or a descendant/ancestor per `click` rules);
- within the click threshold and time.

If down and up target different nodes, dispatch a `click` to their common
ancestor (or not at all, depending on the component). Keep the default
"down/up on same node" rule and let components opt into capture.

### 6.3 Drag and drop

- `on_drag_start`, `on_drag`, `on_drag_end` on a node.
- A drag session stores the source node, payload, current position, and the
  hovered drop target.
- Drop targets register `on_drop` and a payload type; the session validates types.
- A drag overlay is painted in a higher layer; it is a separate view subtree owned
  by the drag session, disposed on drop/cancel.

GPUI's `active_drag`, `DragMoveEvent`, and overlay prepaint are the reference
(`window.rs` `prepaint_deferred_draws`, `active_drag`).

### 6.4 Wheel/scroll

- The hit test finds the deepest scrollable ancestor under the pointer.
- The scroll offset updates; paint translates children.
- A `ScrollState` clamps to `[0, content - viewport]`.
- Scroll events can be captured by a child (e.g. a code editor) before the
  ancestor scrolls. Dispatch bubble from target to root.

## 7. Text input and IME

### 7.1 Focused input handler

When a text field is focused, the platform's IME is pointed at a handler:

```rust
pub trait TextInputHandler {
    fn insert_text(&mut self, text: &str);
    fn set_composition(&mut self, text: &str, cursor: Option<Range<usize>>);
    fn delete_backward(&mut self);
    fn delete_forward(&mut self);
    fn selection(&self) -> Option<Range<usize>>;
    fn set_selection(&mut self, range: Range<usize>);
    fn text(&self) -> &str;
    fn caret_rect(&self) -> Rect<Px>;
}
```

- The handler is owned by the focused view node's component.
- On blur, the platform's IME is reset.
- The composition string is painted as an overlay; the buffer is not mutated until
  commit.
- Caret position is computed from shaped clusters, not bytes.

### 7.2 Keyboard

- Key events go to the focused node first, then bubble.
- A text field consumes printable keys and editing keys; unhandled keys bubble to
  shortcuts.
- Tab is handled by the focus system unless a text field consumes it (multi-line
  editors may want tab insertion).

## 8. Accessibility

### 8.1 Model

Use AccessKit, the same library GPUI uses
(`refs/zed/crates/gpui/src/window/a11y.rs`):

- Each view node can have an a11y role and properties.
- The accessibility tree is built during the layout/apply phases, not paint.
- Node IDs derive from `ViewNodeId` plus a stable path hash. Because view nodes
  are retained, IDs are stable across frames and reorder.
- An update is sent only when something changed; the runtime tracks a dirty a11y
  set.

```rust
pub struct A11yNode {
    pub role: A11yRole,
    pub label: Option<String>,
    pub value: Option<String>,
    pub description: Option<String>,
    pub disabled: bool,
    pub focusable: bool,
    pub bounds: Rect<Px>,
    pub children: Vec<ViewNodeId>,
}
```

### 8.2 Stable IDs

The biggest a11y bug in immediate-mode frameworks is unstable node IDs: a screen
reader sees a different node after a re-render. Our retained tree fixes this by
construction:

- `A11yNodeId = hash(ViewNodeId, path)`; `ViewNodeId` is stable.
- The path hash uses stable keys (component IDs and keyed-row keys), never sibling
  indices; otherwise a reorder would change the ID.
- A keyed row keeps its ID across reorder.
- A conditional branch gets a new ID when remounted; that is correct (it is a
  different subtree).
- A rebuild replaces IDs; keep the rebuilt subtree small or use `KeepAlive` for
  content with a11y state.

### 8.3 Actions

AccessKit actions (press, focus, set value, scroll) map to the same commands as
input:

- `press` → the node's `on_click`/action;
- `focus` → focus the node's `FocusHandle`;
- `set_value` → the node's value setter (for sliders/inputs);
- `scroll` → the nearest scroll ancestor.

Handlers receive an `A11yAction` and stage writes like any other event.

### 8.4 Testing

- Build a tree with labels/roles; assert the AccessKit tree structure.
- Focus a node; assert the a11y focus event.
- Reorder a keyed list; assert the focused row's a11y ID is unchanged.
- Toggle a conditional; assert the branch's IDs are removed and new ones added,
  not reused.

## 9. Cursors and tooltips

- A cursor request is a per-node `Cursor` resolved by the deepest hitbox under
  the pointer. The platform cursor is set once per frame if it changed.
- Tooltips are deferred overlays: a hover timer triggers a tooltip request; the
  overlay is painted in a higher layer. Tooltip content is a normal view subtree,
  owned by the tooltip manager.
- Do not allocate a tooltip subtree per hover if the content is static; cache it
  by key.

## 10. Event ordering and reentrancy

- Input dispatch runs in phase 1. A listener stages writes; it does not flush.
- If a listener disposes the node it is running on, the dispatch path is
  snapshotted at the start; later listeners on detached nodes are skipped.
- If a listener opens an action/async hold, the writes are staged in that
  transaction; the dispatch returns and the frame continues.
- Platform callbacks (resize, scale, theme) are events too; they stage signals
  and mark layout.

## 11. Tests

- Hit testing: overlapping nodes, clipping, transforms, `BlockMouse`.
- Dispatch: capture/bubble order; `Stop`; default actions.
- Actions: keybinding resolution by context; global vs focused.
- Focus: tab order; focus transfers on removal; focus survives list reorder.
- Pointer: click thresholds; drag start/end; drop type validation.
- Wheel: deepest scrollable ancestor; clamp at edges.
- IME: composition commit/cancel; caret at cluster boundaries.
- A11y: roles/labels/values; stable IDs across reorder; actions.
- Fuzz: random pointer paths and key sequences; no panic, no stuck focus.

## 12. Open questions

1. **Dispatch tree vs parent walk.** Parent walk is simpler with a retained tree;
   are there cases where a separate dispatch tree is needed (e.g. portals)?
   Resolve by storing the logical parent separately from the paint parent.
2. **Focus restoration.** How much state should survive a remount? Parent/sibling
   fallback is the v1 rule; `KeepAlive` is the opt-in.
3. **Gesture library.** Do we implement double-click, long-press, pinch in v1, or
   only click/drag/wheel? Start with the basics, add gestures as a layer.
4. **A11y tree build cost.** Build incrementally from dirty nodes or rebuild the
   whole tree when a11y is active? Start with a full build when active, measure.
5. **Platform IME differences.** macOS/Windows/Linux composition models differ;
   keep the trait minimal and put quirks in the platform adapter.
6. **Cursor and tooltip layering.** Are they separate layers or part of the
   overlay layer? Separate makes hit testing predictable.
