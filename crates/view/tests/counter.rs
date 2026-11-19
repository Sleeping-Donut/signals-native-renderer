//! Counter reference app on the retained view tree.

use std::cell::RefCell;
use std::rc::Rc;

use reactive_core::{create_memo, create_signal, ReadSignal, Runtime, RuntimeHandle, WriteSignal};
use renderer_api::{
    Color, CursorIcon, FlexDirection, Key, KeyEvent, Length, Modifiers, NamedKey, PointerEvent,
    ResolvedStyle, Size, TextStyle,
};
use renderer_null::{NullRenderer, SharedNullRenderer};
use view::{button, column, text_fn, StateStyle, View, ViewTree};

fn button_style() -> ResolvedStyle {
    ResolvedStyle {
        size: Size {
            width: Length::Px(160.0),
            height: Length::Px(32.0),
        },
        text: TextStyle {
            color: Color::new(0.92, 0.92, 0.95, 1.0),
            font_size: 16.0,
            line_height: 20.0,
            weight: 500,
        },
        background: Some(Color::new(0.24, 0.28, 0.36, 1.0)),
        border: renderer_api::Edges {
            top: 1.0,
            right: 1.0,
            bottom: 1.0,
            left: 1.0,
        },
        border_color: Some(Color::new(0.36, 0.42, 0.54, 1.0)),
        ..ResolvedStyle::default()
    }
}

const HOVER: StateStyle = StateStyle::new().background(Color::new(0.3, 0.36, 0.46, 1.0));
const FOCUS: StateStyle = StateStyle::new()
    .background(Color::new(0.2, 0.42, 0.62, 1.0))
    .border_color(Color::new(0.62, 0.82, 1.0, 1.0));

fn counter(cx: &reactive_core::Cx<'_>, shared: ReadSignal<u32>, bump: WriteSignal<u32>) -> View {
    let (local, set_local) = create_signal(cx, 0_u32);
    let double = create_memo(cx, move |cx, _| local.get(cx) * 2);
    let mut stack = ResolvedStyle {
        flex: renderer_api::FlexStyle {
            direction: FlexDirection::Column,
            ..renderer_api::FlexStyle::default()
        },
        ..ResolvedStyle::default()
    };
    stack.gap = Size {
        width: renderer_api::DefiniteLength::Px(0.0),
        height: renderer_api::DefiniteLength::Px(6.0),
    };
    column(vec![
        button(
            vec![text_fn(move |cx| format!("local {}", local.get(cx)))],
            move |cx| set_local.update_with(cx, |value| value + 1),
        )
        .style(button_style())
        .hover_style(HOVER)
        .focus_style(FOCUS)
        .cursor(CursorIcon::Pointer),
        button(
            vec![text_fn(move |cx| format!("shared {}", shared.get(cx)))],
            move |cx| bump.update_with(cx, |value| value + 1),
        )
        .style(button_style())
        .hover_style(HOVER)
        .focus_style(FOCUS)
        .cursor(CursorIcon::Pointer),
        text_fn(move |cx| format!("local double {}", double.get(cx))),
    ])
    .style(stack)
}

fn setup() -> (
    RuntimeHandle,
    ViewTree,
    Rc<RefCell<NullRenderer>>,
    ReadSignal<u32>,
    WriteSignal<u32>,
) {
    let runtime = Runtime::new().into_handle();
    let renderer = Rc::new(RefCell::new(NullRenderer::new()));
    let tree = ViewTree::new(
        Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
        runtime.clone(),
    );
    let cx = runtime.cx();
    let (shared, bump) = create_signal(&cx, 0_u32);
    (runtime, tree, renderer, shared, bump)
}

fn text(
    renderer: &Rc<RefCell<NullRenderer>>,
    tree: &ViewTree,
    node: reactive_core::NodeId,
) -> Option<String> {
    let id = tree.renderer_id(node)?;
    renderer.borrow().text_of(id).map(str::to_string)
}

#[test]
fn counter_local_shared_keyboard_and_accesskit() {
    let (runtime, tree, renderer, shared, bump) = setup();
    let cx = runtime.cx();
    let root = tree.mount(
        &cx,
        column(vec![counter(&cx, shared, bump), counter(&cx, shared, bump)]),
    );
    tree.set_viewport(400.0, 200.0);
    tree.frame(&cx, 0);

    let counters = tree.children(root);
    let first = counters[0];
    let second = counters[1];
    let first_children = tree.children(first);
    let second_children = tree.children(second);
    let (first_local, first_shared) = (first_children[0], first_children[1]);
    let (second_local, _second_shared) = (second_children[0], second_children[1]);
    let first_local_text = tree.children(first_local)[0];
    let first_double_text = first_children[2];
    let second_local_text = tree.children(second_local)[0];

    assert_eq!(
        text(&renderer, &tree, first_local_text).as_deref(),
        Some("local 0")
    );
    assert_eq!(
        text(&renderer, &tree, first_double_text).as_deref(),
        Some("local double 0")
    );

    assert!(tree.click_node(first_local));
    tree.frame(&cx, 1);
    assert_eq!(
        text(&renderer, &tree, first_local_text).as_deref(),
        Some("local 1")
    );
    assert_eq!(
        text(&renderer, &tree, first_double_text).as_deref(),
        Some("local double 2")
    );
    assert_eq!(
        text(&renderer, &tree, second_local_text).as_deref(),
        Some("local 0")
    );

    tree.set_focus(Some(first_shared));
    let enter = KeyEvent::new(Key::Named(NamedKey::Enter), Modifiers::NONE);
    assert!(tree.key_down(&enter));
    tree.frame(&cx, 2);
    assert_eq!(
        text(&renderer, &tree, tree.children(first_shared)[0]).as_deref(),
        Some("shared 1")
    );
    assert_eq!(
        text(&renderer, &tree, tree.children(second_children[1])[0]).as_deref(),
        Some("shared 1")
    );

    let space = KeyEvent::new(
        Key::Named(NamedKey::Space),
        Modifiers {
            shift: true,
            ..Modifiers::NONE
        },
    );
    assert!(tree.key_down(&space));
    tree.frame(&cx, 3);
    assert_eq!(
        text(&renderer, &tree, tree.children(first_shared)[0]).as_deref(),
        Some("shared 2")
    );

    let update = tree.accesskit_tree().expect("accesskit tree");
    assert_eq!(
        update.focus,
        accesskit::NodeId(tree.renderer_id(first_shared).unwrap())
    );
    let focused = update
        .nodes
        .iter()
        .find_map(|(id, node)| (*id == update.focus).then_some(node))
        .expect("focused node");
    assert_eq!(focused.role(), accesskit::Role::Button);
    assert_eq!(focused.label(), Some("shared 2"));

    let scene = tree.build_scene();
    let focus_fill = scene.ops().iter().any(|primitive| {
        matches!(
            primitive,
            scene::Primitive::Fill { node, color, .. }
                if *node == tree.renderer_id(first_shared).unwrap()
                    && *color == Color::new(0.2, 0.42, 0.62, 1.0)
        )
    });
    assert!(focus_fill, "focused button paints its focus background");

    let rect = tree.layout_rect(first_local).expect("local button");
    tree.pointer_move(&PointerEvent::new(rect.x + 4.0, rect.y + 4.0, None));
    assert_eq!(tree.cursor(), Some(CursorIcon::Pointer));

    tree.unmount(&cx, root);
}
