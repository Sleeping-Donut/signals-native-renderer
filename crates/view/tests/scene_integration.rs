//! Retained tree to scene integration: paint primitives, replay, damage.

use std::cell::RefCell;
use std::rc::Rc;

use reactive_core::{create_signal, Runtime, RuntimeHandle};
use renderer_api::{Color, Length, ResolvedStyle, Size, TextStyle};
use renderer_null::{NullRenderer, SharedNullRenderer};
use scene::Primitive;
use view::{column, text_fn, ViewTree};

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

fn setup() -> (RuntimeHandle, ViewTree, Rc<RefCell<NullRenderer>>) {
    let runtime = Runtime::new().into_handle();
    let renderer = Rc::new(RefCell::new(NullRenderer::new()));
    let tree = ViewTree::new(
        Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
        runtime.clone(),
    );
    (runtime, tree, renderer)
}

fn text_color(color: Color) -> ResolvedStyle {
    let mut style = style(Length::Auto, Length::Auto);
    style.text.color = color;
    style
}

#[test]
fn scene_contains_fills_and_shaped_text() {
    let (runtime, tree, _renderer) = setup();
    let cx = runtime.cx();
    let mut root_style = style(Length::Px(200.0), Length::Px(100.0));
    root_style.background = Some(Color::new(0.1, 0.1, 0.1, 1.0));
    let mut label_style = text_color(Color::new(0.2, 0.4, 0.8, 1.0));
    label_style.background = Some(Color::new(0.0, 0.0, 0.0, 0.5));

    let root = tree.mount(
        &cx,
        column(vec![text_fn(|_| "Hi".to_string()).style(label_style)]).style(root_style),
    );
    tree.set_viewport(200.0, 100.0);
    tree.frame(&cx, 0);
    let scene = tree.build_scene();

    assert_eq!(scene.len(), 3);
    assert!(matches!(
        scene.ops()[0],
        Primitive::Fill { color, .. } if color == Color::new(0.1, 0.1, 0.1, 1.0)
    ));
    assert!(matches!(
        scene.ops()[1],
        Primitive::Fill { color, .. } if color == Color::new(0.0, 0.0, 0.0, 0.5)
    ));
    let Primitive::Text { layout, color, .. } = &scene.ops()[2] else {
        panic!("expected text primitive");
    };
    assert_eq!(*color, Color::new(0.2, 0.4, 0.8, 1.0));
    assert_eq!(layout.lines.len(), 1);
    assert_eq!(layout.lines[0].text, "Hi");
    assert!((layout.width - 19.2).abs() < 0.001);
    assert_eq!(tree.scene_stats().primitives, 3);

    tree.unmount(&cx, root);
}

#[test]
fn paint_only_recolor_keeps_layout_and_rebuilds_the_scene() {
    let (runtime, tree, _renderer) = setup();
    let cx = runtime.cx();
    let root = tree.mount(
        &cx,
        column(vec![text_fn(|_| "Hi".to_string())])
            .style(style(Length::Px(200.0), Length::Px(100.0))),
    );
    tree.set_viewport(200.0, 100.0);
    tree.frame(&cx, 0);
    tree.build_scene();
    tree.take_scene_damage();
    let text_node = tree.children(root)[0];

    tree.set_style(text_node, text_color(Color::new(1.0, 0.0, 0.0, 1.0)));
    tree.frame(&cx, 1);
    assert!(!tree.last_layout().ran, "recolor is paint-only");

    let scene = tree.build_scene();
    let Primitive::Text { color, .. } = scene
        .ops()
        .iter()
        .find(|op| matches!(op, Primitive::Text { .. }))
        .expect("text primitive")
    else {
        unreachable!()
    };
    assert_eq!(*color, Color::new(1.0, 0.0, 0.0, 1.0));
    assert!(!tree.take_scene_damage().is_empty());

    tree.unmount(&cx, root);
}

#[test]
fn clean_scene_build_replays_without_shaping() {
    let (runtime, tree, _renderer) = setup();
    let cx = runtime.cx();
    let root = tree.mount(
        &cx,
        column(vec![text_fn(|_| "stable".to_string())])
            .style(style(Length::Px(200.0), Length::Px(100.0))),
    );
    tree.set_viewport(200.0, 100.0);
    tree.frame(&cx, 0);

    tree.build_scene();
    let hits = tree.text_cache_stats().hits;
    let cache_len = tree.text_cache_len();
    let damaged = tree.take_scene_damage();
    assert!(!damaged.is_empty());

    let scene = tree.build_scene();
    assert_eq!(tree.scene_stats().rebuilt, 0);
    assert!(tree.scene_stats().replayed >= 1);
    assert_eq!(tree.text_cache_stats().hits, hits);
    assert_eq!(tree.text_cache_len(), cache_len);
    assert_eq!(scene.len(), 1);

    tree.unmount(&cx, root);
}

#[test]
fn layout_change_updates_scene_geometry_and_damage() {
    let (runtime, tree, _renderer) = setup();
    let cx = runtime.cx();
    let (value, set_value) = create_signal(&cx, "a".to_string());
    let root = tree.mount(
        &cx,
        column(vec![text_fn(move |cx| value.get(cx))])
            .style(style(Length::Px(200.0), Length::Px(100.0))),
    );
    tree.set_viewport(200.0, 100.0);
    tree.frame(&cx, 0);
    tree.build_scene();
    tree.take_scene_damage();

    set_value.set(&cx, "aa".to_string());
    tree.frame(&cx, 1);
    assert!(tree.last_layout().ran);
    let scene = tree.build_scene();
    let Primitive::Text { rect, .. } = scene
        .ops()
        .iter()
        .find(|op| matches!(op, Primitive::Text { .. }))
        .expect("text primitive")
    else {
        unreachable!()
    };
    assert!((rect.width - 19.0).abs() < 0.001, "{rect:?}");
    assert!(!tree.take_scene_damage().is_empty());

    tree.unmount(&cx, root);
}
