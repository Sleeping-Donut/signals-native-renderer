//! View tree to layout integration: mount, style, frame, geometry.

use std::cell::RefCell;
use std::rc::Rc;

use layout::Rect;
use reactive_core::{create_signal, Runtime, RuntimeHandle};
use renderer_api::{Color, DefiniteLength, FlexDirection, Length, ResolvedStyle, Size, TextStyle};
use renderer_null::{NullRenderer, SharedNullRenderer};
use view::{column, row, text_fn, ViewTree};

fn style(width: Length, height: Length) -> ResolvedStyle {
    ResolvedStyle {
        size: Size { width, height },
        text: TextStyle {
            color: Color::new(0.0, 0.0, 0.0, 1.0),
            font_size: 16.0,
            line_height: 20.0,
            weight: 400,
        },
        ..ResolvedStyle::default()
    }
}

fn px(width: f32, height: f32) -> ResolvedStyle {
    style(Length::Px(width), Length::Px(height))
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

fn assert_rect(actual: Option<Rect>, x: f32, y: f32, width: f32, height: f32) {
    let actual = actual.expect("geometry");
    assert!((actual.x - x).abs() < 0.001, "x: {actual:?}");
    assert!((actual.y - y).abs() < 0.001, "y: {actual:?}");
    assert!((actual.width - width).abs() < 0.001, "width: {actual:?}");
    assert!((actual.height - height).abs() < 0.001, "height: {actual:?}");
}

#[test]
fn children_layout_through_the_retained_tree() {
    let (runtime, tree, _renderer) = setup();
    let cx = runtime.cx();
    let mut root_style = px(200.0, 100.0);
    root_style.flex.direction = FlexDirection::Row;
    root_style.gap = Size {
        width: DefiniteLength::Px(10.0),
        height: DefiniteLength::Px(0.0),
    };
    let mut child_style = px(50.0, 20.0);
    child_style.flex.grow = 1.0;

    let root = tree.mount(
        &cx,
        row(vec![
            column(vec![]).style(child_style),
            column(vec![]).style(child_style),
        ])
        .style(root_style),
    );
    tree.set_viewport(200.0, 100.0);
    tree.frame(&cx, 0);
    assert!(tree.last_layout().ran);

    let children = tree.children(root);
    assert_eq!(children.len(), 2);
    assert_rect(tree.layout_rect(root), 0.0, 0.0, 200.0, 100.0);
    assert_rect(tree.layout_rect(children[0]), 0.0, 0.0, 95.0, 20.0);
    assert_rect(tree.layout_rect(children[1]), 105.0, 0.0, 95.0, 20.0);

    tree.unmount(&cx, root);
}

#[test]
fn text_binding_updates_layout_but_recolor_does_not() {
    let (runtime, tree, _renderer) = setup();
    let cx = runtime.cx();
    let (value, set_value) = create_signal(&cx, "Hello".to_string());
    let root = tree.mount(
        &cx,
        column(vec![text_fn(move |cx| value.get(cx))]).style(px(200.0, 100.0)),
    );
    tree.set_viewport(200.0, 100.0);
    tree.frame(&cx, 0);
    let text_node = tree.children(root)[0];
    assert_rect(tree.layout_rect(text_node), 0.0, 0.0, 48.0, 20.0);

    set_value.set(&cx, "Hello!!".to_string());
    tree.frame(&cx, 1);
    assert_rect(tree.layout_rect(text_node), 0.0, 0.0, 67.0, 20.0);
    assert!(tree.last_layout().ran);

    // A text color change is paint-only.
    let mut recolored = style(Length::Auto, Length::Auto);
    recolored.text.color = Color::new(1.0, 0.0, 0.0, 1.0);
    tree.set_style(text_node, recolored);
    tree.frame(&cx, 2);
    assert!(!tree.last_layout().ran);

    tree.unmount(&cx, root);
}

#[test]
fn viewport_resize_and_scale_resnap_geometry() {
    let (runtime, tree, _renderer) = setup();
    let cx = runtime.cx();
    let root = tree.mount(
        &cx,
        column(vec![column(vec![]).style(px(1.0, 1.0))])
            .style(style(Length::Percent(100.0), Length::Px(50.0))),
    );
    tree.set_viewport(300.0, 200.0);
    tree.frame(&cx, 0);
    assert_rect(tree.layout_rect(root), 0.0, 0.0, 300.0, 50.0);

    tree.set_viewport(600.0, 200.0);
    tree.frame(&cx, 1);
    assert_rect(tree.layout_rect(root), 0.0, 0.0, 600.0, 50.0);

    let child = tree.children(root)[0];
    tree.frame(&cx, 2);
    assert!(!tree.last_layout().ran);

    tree.set_scale(1.5);
    tree.frame(&cx, 3);
    assert!(tree.last_layout().ran);
    let rect = tree.layout_rect(child).expect("child geometry");
    assert!((rect.width - 2.0 / 1.5).abs() < 0.001, "{rect:?}");

    tree.unmount(&cx, root);
}
