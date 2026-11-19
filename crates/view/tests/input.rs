//! Phase 6 input, focus, key binding, and accessibility behavior.

use std::cell::RefCell;
use std::rc::Rc;

use reactive_core::{create_signal, Runtime, RuntimeHandle};
use renderer_api::{
    A11yRole, Key, KeyEvent, Length, Modifiers, NamedKey, PointerButton, PointerEvent, Position,
    ResolvedStyle, Size, TextStyle,
};
use renderer_null::{NullRenderer, SharedNullRenderer};
use view::{button_text, column, keyed_list, text_fn, FocusHandle, KeyStroke, ViewTree};

fn px(width: f32, height: f32) -> ResolvedStyle {
    ResolvedStyle {
        size: Size {
            width: Length::Px(width),
            height: Length::Px(height),
        },
        text: TextStyle {
            color: renderer_api::Color::new(0.0, 0.0, 0.0, 1.0),
            font_size: 16.0,
            line_height: 20.0,
            weight: 400,
        },
        ..ResolvedStyle::default()
    }
}

fn absolute(width: f32, height: f32) -> ResolvedStyle {
    let mut style = px(width, height);
    style.position = Position::Absolute;
    style
}

fn setup() -> (RuntimeHandle, ViewTree, Rc<RefCell<NullRenderer>>) {
    let runtime = Runtime::new().into_handle();
    let renderer = Rc::new(RefCell::new(NullRenderer::new()));
    let tree = ViewTree::new(
        Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
        runtime.clone(),
    );
    (runtime, tree, renderer)
}

fn pointer(x: f32, y: f32) -> PointerEvent {
    PointerEvent::new(x, y, Some(PointerButton::Primary))
}

#[test]
fn topmost_hitbox_receives_the_click() {
    let (runtime, tree, _renderer) = setup();
    let cx = runtime.cx();
    let (a, set_a) = create_signal(&cx, 0_u32);
    let (b, set_b) = create_signal(&cx, 0_u32);
    let root = tree.mount(
        &cx,
        column(vec![
            button_text("a", move |cx| set_a.update_with(cx, |v| v + 1))
                .style(absolute(100.0, 50.0)),
            button_text("b", move |cx| set_b.update_with(cx, |v| v + 1))
                .style(absolute(100.0, 50.0)),
        ])
        .style(px(200.0, 200.0)),
    );
    tree.set_viewport(200.0, 200.0);
    tree.frame(&cx, 0);

    let children = tree.children(root);
    let event = pointer(10.0, 10.0);
    assert_eq!(tree.hit_test(10.0, 10.0), Some(children[1]));
    assert_eq!(tree.pointer_move(&event), Some(children[1]));
    assert_eq!(tree.hovered(), Some(children[1]));
    assert_eq!(tree.pointer_down(&event), Some(children[1]));
    assert!(tree.pointer_up(&event));
    tree.frame(&cx, 1);

    assert_eq!(a.get(&cx), 0);
    assert_eq!(b.get(&cx), 1);
    assert!(tree.is_focused(children[1]));

    tree.unmount(&cx, root);
}

#[test]
fn tab_order_skips_disabled_nodes_and_wraps() {
    let (runtime, tree, _renderer) = setup();
    let cx = runtime.cx();
    let root = tree.mount(
        &cx,
        column(vec![
            button_text("one", |_| {}).style(px(100.0, 20.0)),
            button_text("two", |_| {})
                .disabled(true)
                .style(px(100.0, 20.0)),
            button_text("three", |_| {}).style(px(100.0, 20.0)),
        ])
        .style(px(200.0, 200.0)),
    );
    tree.set_viewport(200.0, 200.0);
    tree.frame(&cx, 0);
    let children = tree.children(root);

    assert_eq!(tree.focus_next(), Some(children[0]));
    assert_eq!(tree.focus_next(), Some(children[2]));
    assert_eq!(tree.focus_next(), Some(children[0]));
    assert_eq!(tree.focus_prev(), Some(children[2]));

    let tab = KeyEvent::new(Key::Named(NamedKey::Tab), Modifiers::NONE);
    assert!(tree.key_down(&tab));
    assert_eq!(tree.focused(), Some(children[0]));
    let shift_tab = KeyEvent::new(
        Key::Named(NamedKey::Tab),
        Modifiers::new(true, false, false, false),
    );
    assert!(tree.key_down(&shift_tab));
    assert_eq!(tree.focused(), Some(children[2]));

    tree.unmount(&cx, root);
}

#[test]
fn focused_key_binding_and_global_binding_dispatch() {
    let (runtime, tree, _renderer) = setup();
    let cx = runtime.cx();
    let (count, set_count) = create_signal(&cx, 0_u32);
    let (global, set_global) = create_signal(&cx, 0_u32);
    let root = tree.mount(
        &cx,
        column(vec![text_fn(move |cx| count.get(cx).to_string())])
            .on_key(KeyStroke::character('x'), move |cx| {
                set_count.update_with(cx, |v| v + 1);
            })
            .style(px(200.0, 200.0)),
    );
    let handle: FocusHandle = tree.focus_handle(root).expect("focusable");
    tree.set_viewport(200.0, 200.0);
    tree.frame(&cx, 0);
    tree.focus(handle);

    let x = KeyEvent::new(Key::Character('x'), Modifiers::NONE);
    assert!(tree.key_down(&x));
    tree.frame(&cx, 1);
    assert_eq!(count.get(&cx), 1);

    tree.bind_global_key(KeyStroke::character('g'), move |cx| {
        set_global.update_with(cx, |v| v + 1);
    });
    let g = KeyEvent::new(Key::Character('g'), Modifiers::NONE);
    assert!(tree.key_down(&g));
    tree.frame(&cx, 2);
    assert_eq!(global.get(&cx), 1);

    tree.unmount(&cx, root);
}

#[test]
fn a11y_tree_reports_roles_labels_and_focus() {
    let (runtime, tree, _renderer) = setup();
    let cx = runtime.cx();
    let root = tree.mount(
        &cx,
        column(vec![button_text("Save", |_| {}).style(px(80.0, 20.0))])
            .a11y_label("Actions")
            .style(px(200.0, 200.0)),
    );
    tree.set_viewport(200.0, 200.0);
    tree.frame(&cx, 0);
    let button = tree.children(root)[0];
    tree.set_focus(Some(button));

    let a11y = tree.a11y_tree();
    assert_eq!(a11y.roots.len(), 1);
    let root_node = a11y
        .nodes
        .iter()
        .find(|node| node.id == a11y.roots[0])
        .expect("root");
    assert_eq!(root_node.role, A11yRole::Generic);
    assert_eq!(root_node.label.as_deref(), Some("Actions"));
    let button_node = a11y
        .nodes
        .iter()
        .find(|node| node.role == A11yRole::Button)
        .expect("button");
    assert_eq!(button_node.label.as_deref(), Some("Save"));
    assert!(button_node.focusable);
    assert!(button_node.focused);

    tree.unmount(&cx, root);
}

#[test]
fn a11y_actions_press_and_focus() {
    let (runtime, tree, _renderer) = setup();
    let cx = runtime.cx();
    let (count, set_count) = create_signal(&cx, 0_u32);
    let root = tree.mount(
        &cx,
        column(vec![button_text("press", move |cx| {
            set_count.update_with(cx, |value| value + 1);
        })])
        .style(px(200.0, 200.0)),
    );
    tree.set_viewport(200.0, 200.0);
    tree.frame(&cx, 0);
    let button = tree
        .a11y_tree()
        .nodes
        .into_iter()
        .find(|node| node.role == A11yRole::Button)
        .expect("button");
    assert!(tree.a11y_activate(button.id));
    tree.frame(&cx, 1);
    assert_eq!(count.get(&cx), 1);
    assert!(tree.a11y_focus(button.id));
    assert!(tree.is_focused(tree.children(root)[0]));
    assert!(!tree.a11y_focus(999_999));

    tree.unmount(&cx, root);
}

#[test]
fn removing_focused_row_moves_focus_to_previous_focusable_sibling() {
    let (runtime, tree, _renderer) = setup();
    let cx = runtime.cx();
    let (order, set_order) = create_signal(&cx, vec![0_u32, 1, 2]);
    let root = tree.mount(
        &cx,
        keyed_list(
            move |cx| order.get(cx),
            |index| *index,
            |_cx, index| button_text(format!("row {index}"), |_| {}),
        ),
    );
    tree.set_viewport(200.0, 200.0);
    tree.frame(&cx, 0);
    let rows = tree.children(root);
    tree.set_focus(Some(rows[1]));

    set_order.set(&cx, vec![0, 2]);
    tree.frame(&cx, 1);

    assert_eq!(tree.focused(), Some(rows[0]));

    tree.unmount(&cx, root);
}

#[test]
fn keyed_row_a11y_id_and_focus_survive_reorder() {
    let (runtime, tree, _renderer) = setup();
    let cx = runtime.cx();
    let (order, set_order) = create_signal(&cx, vec![0_u32, 1, 2]);
    let root = tree.mount(
        &cx,
        keyed_list(
            move |cx| order.get(cx),
            |index| *index,
            |_cx, index| button_text(format!("row {index}"), |_| {}),
        ),
    );
    tree.set_viewport(200.0, 200.0);
    tree.frame(&cx, 0);

    let row_one = tree.children(root)[1];
    let before = tree
        .a11y_tree()
        .nodes
        .into_iter()
        .find(|node| node.label.as_deref() == Some("row 1"))
        .expect("row one");
    assert_eq!(before.id, tree.renderer_id(row_one).unwrap());
    tree.set_focus(Some(row_one));

    set_order.set(&cx, vec![2, 1, 0]);
    tree.frame(&cx, 1);

    let after = tree
        .a11y_tree()
        .nodes
        .into_iter()
        .find(|node| node.label.as_deref() == Some("row 1"))
        .expect("row one after reorder");
    assert_eq!(after.id, before.id);
    assert!(after.focused);
    assert!(tree.is_focused(row_one));

    tree.unmount(&cx, root);
}
