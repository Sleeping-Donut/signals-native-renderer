//! Phase 6 input, focus, key binding, and accessibility behavior.

use std::cell::RefCell;
use std::rc::Rc;

use reactive_core::{create_signal, Runtime, RuntimeHandle};
use renderer_api::{
    A11yRole, Key, KeyEvent, Length, Modifiers, NamedKey, Overflow, PointerButton, PointerEvent,
    Position, ResolvedStyle, Size, TextStyle, Transform,
};
use renderer_null::{NullRenderer, SharedNullRenderer};
use view::{
    button_text, column, keyed_list, text_fn, DispatchPhase, DispatchResult, FocusHandle,
    KeyStroke, ViewTree,
};

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

fn clipped(width: f32, height: f32) -> ResolvedStyle {
    let mut style = px(width, height);
    style.overflow = Overflow::Hidden;
    style
}

fn transformed(transform: Transform) -> ResolvedStyle {
    let mut style = px(100.0, 100.0);
    style.transform = transform;
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
fn click_dispatch_runs_capture_then_bubble() {
    let (runtime, tree, _renderer) = setup();
    let cx = runtime.cx();
    let order = Rc::new(RefCell::new(Vec::new()));
    let order_root_capture = Rc::clone(&order);
    let order_child_capture = Rc::clone(&order);
    let order_child_bubble = Rc::clone(&order);
    let order_root_bubble = Rc::clone(&order);
    let child = column(vec![])
        .on_click_capture(move |_| order_child_capture.borrow_mut().push("child-capture"))
        .on_click(move |_| order_child_bubble.borrow_mut().push("child-bubble"));
    let root = tree.mount(
        &cx,
        column(vec![child])
            .on_click_capture(move |_| order_root_capture.borrow_mut().push("root-capture"))
            .on_click(move |_| order_root_bubble.borrow_mut().push("root-bubble"))
            .style(px(100.0, 100.0)),
    );
    tree.set_viewport(100.0, 100.0);
    tree.frame(&cx, 0);
    let child = tree.children(root)[0];
    assert!(tree.click_node(child));
    assert_eq!(
        &*order.borrow(),
        &[
            "root-capture",
            "child-capture",
            "child-bubble",
            "root-bubble"
        ]
    );

    tree.unmount(&cx, root);
}

#[test]
fn capture_stop_prevents_bubble() {
    let (runtime, tree, _renderer) = setup();
    let cx = runtime.cx();
    let child_capture = Rc::new(RefCell::new(false));
    let child_bubble = Rc::new(RefCell::new(false));
    let capture_flag = Rc::clone(&child_capture);
    let bubble_flag = Rc::clone(&child_bubble);
    let child = column(vec![])
        .on_click_capture(move |_| *capture_flag.borrow_mut() = true)
        .on_click(move |_| *bubble_flag.borrow_mut() = true);
    let root = tree.mount(
        &cx,
        column(vec![child])
            .on_click_filter(DispatchPhase::Capture, |_| DispatchResult::Stop)
            .style(px(100.0, 100.0)),
    );
    tree.set_viewport(100.0, 100.0);
    tree.frame(&cx, 0);
    let child = tree.children(root)[0];
    tree.click_node(child);
    assert!(!*child_capture.borrow());
    assert!(!*child_bubble.borrow());

    tree.unmount(&cx, root);
}

#[test]
fn handled_key_binding_suppresses_tab_default() {
    let (runtime, tree, _renderer) = setup();
    let cx = runtime.cx();
    let first = button_text("first", |_| {})
        .on_key_filter(
            KeyStroke::named(NamedKey::Tab),
            DispatchPhase::Bubble,
            |_| DispatchResult::Handled,
        )
        .style(px(100.0, 20.0));
    let root = tree.mount(
        &cx,
        column(vec![
            first,
            button_text("second", |_| {}).style(px(100.0, 20.0)),
        ])
        .style(px(200.0, 200.0)),
    );
    tree.set_viewport(200.0, 200.0);
    tree.frame(&cx, 0);
    let buttons = tree.children(root);
    tree.set_focus(Some(buttons[0]));

    let tab = KeyEvent::new(Key::Named(NamedKey::Tab), Modifiers::NONE);
    assert!(tree.key_down(&tab));
    assert_eq!(tree.focused(), Some(buttons[0]), "Handled suppresses Tab");

    tree.unmount(&cx, root);
}

#[test]
fn stopped_key_binding_does_not_suppress_tab_default() {
    let (runtime, tree, _renderer) = setup();
    let cx = runtime.cx();
    let first = button_text("first", |_| {})
        .on_key_filter(
            KeyStroke::named(NamedKey::Tab),
            DispatchPhase::Bubble,
            |_| DispatchResult::Stop,
        )
        .style(px(100.0, 20.0));
    let root = tree.mount(
        &cx,
        column(vec![
            first,
            button_text("second", |_| {}).style(px(100.0, 20.0)),
        ])
        .style(px(200.0, 200.0)),
    );
    tree.set_viewport(200.0, 200.0);
    tree.frame(&cx, 0);
    let buttons = tree.children(root);
    tree.set_focus(Some(buttons[0]));

    let tab = KeyEvent::new(Key::Named(NamedKey::Tab), Modifiers::NONE);
    tree.key_down(&tab);
    assert_eq!(tree.focused(), Some(buttons[1]), "Stop leaves Tab default");

    tree.unmount(&cx, root);
}

#[test]
fn key_dispatch_runs_capture_then_bubble() {
    let (runtime, tree, _renderer) = setup();
    let cx = runtime.cx();
    let order = Rc::new(RefCell::new(Vec::new()));
    let capture = Rc::clone(&order);
    let bubble = Rc::clone(&order);
    let child = button_text("child", |_| {})
        .on_key_capture(KeyStroke::character('x'), move |_| {
            capture.borrow_mut().push("capture");
        })
        .on_key(KeyStroke::character('x'), move |_| {
            bubble.borrow_mut().push("bubble");
        })
        .style(px(100.0, 20.0));
    let root = tree.mount(&cx, column(vec![child]).style(px(200.0, 200.0)));
    tree.set_viewport(200.0, 200.0);
    tree.frame(&cx, 0);
    let child = tree.children(root)[0];
    tree.set_focus(Some(child));

    let key = KeyEvent::new(Key::Character('x'), Modifiers::NONE);
    assert!(tree.key_down(&key));
    assert_eq!(&*order.borrow(), &["capture", "bubble"]);

    tree.unmount(&cx, root);
}

#[test]
fn clipped_hitboxes_ignore_points_outside_the_content_mask() {
    let (runtime, tree, _renderer) = setup();
    let cx = runtime.cx();
    let (count, set_count) = create_signal(&cx, 0_u32);
    let container = {
        let mut style = clipped(100.0, 100.0);
        style.background = Some(renderer_api::Color::new(0.2, 0.2, 0.2, 1.0));
        view::column(vec![button_text("inside", move |cx| {
            set_count.update_with(cx, |value| value + 1);
        })
        .style({
            let mut style = absolute(150.0, 150.0);
            style.background = Some(renderer_api::Color::new(1.0, 0.0, 0.0, 1.0));
            style
        })])
        .style(style)
    };
    let root = tree.mount(&cx, view::column(vec![container]).style(px(200.0, 200.0)));
    tree.set_viewport(200.0, 200.0);
    tree.frame(&cx, 0);

    let button = tree.children(tree.children(root)[0])[0];
    assert_eq!(tree.hit_test(50.0, 50.0), Some(button));
    assert_eq!(tree.hit_test(120.0, 80.0), None);
    tree.pointer_down(&pointer(120.0, 80.0));
    tree.pointer_up(&pointer(120.0, 80.0));
    tree.frame(&cx, 1);
    assert_eq!(count.get(&cx), 0);

    let scene = tree.build_scene();
    let button_renderer = tree.renderer_id(button).unwrap();
    let fill = scene
        .ops()
        .iter()
        .find_map(|primitive| match primitive {
            scene::Primitive::Fill {
                node, rect, clip, ..
            } if *node == button_renderer => Some((*rect, *clip)),
            _ => None,
        })
        .expect("button fill");
    assert_eq!(fill.0, scene::Rect::new(0.0, 0.0, 150.0, 150.0));
    assert_eq!(fill.1, Some(scene::Rect::new(0.0, 0.0, 100.0, 100.0)));

    tree.unmount(&cx, root);
}

#[test]
fn transformed_pointer_coordinates_hit_and_paint_in_world_space() {
    let (runtime, tree, _renderer) = setup();
    let cx = runtime.cx();
    let (count, set_count) = create_signal(&cx, 0_u32);
    let mut button_style = px(50.0, 50.0);
    button_style.background = Some(renderer_api::Color::new(0.0, 1.0, 0.0, 1.0));
    let container = view::column(vec![button_text("scaled", move |cx| {
        set_count.update_with(cx, |value| value + 1);
    })
    .style(button_style)])
    .style(transformed(Transform::new(10.0, 10.0, 2.0, 2.0)));
    let root = tree.mount(&cx, view::column(vec![container]).style(px(300.0, 300.0)));
    tree.set_viewport(300.0, 300.0);
    tree.frame(&cx, 0);

    let button = tree.children(tree.children(root)[0])[0];
    assert_eq!(tree.hit_test(5.0, 5.0), None);
    assert_eq!(tree.hit_test(60.0, 60.0), Some(button));
    tree.pointer_down(&pointer(60.0, 60.0));
    tree.pointer_up(&pointer(60.0, 60.0));
    tree.frame(&cx, 1);
    assert_eq!(count.get(&cx), 1);

    let scene = tree.build_scene();
    let button_renderer = tree.renderer_id(button).unwrap();
    let rect = scene
        .ops()
        .iter()
        .find_map(|primitive| match primitive {
            scene::Primitive::Fill { node, rect, .. } if *node == button_renderer => Some(*rect),
            _ => None,
        })
        .expect("button fill");
    assert_eq!(rect, scene::Rect::new(10.0, 10.0, 100.0, 100.0));

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
