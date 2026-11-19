//! Layout correctness, invalidation, text measurement, and pixel snapping.

use layout::{LayoutEngine, MonospaceText, Rect, TextContent};
use renderer_api::{
    Align, Color, DefiniteLength, Display, Edges, FlexDirection, Justify, Length, Position,
    ResolvedStyle, Size, TextMetrics, TextStyle,
};

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

fn with_direction(mut style: ResolvedStyle, direction: FlexDirection) -> ResolvedStyle {
    style.flex.direction = direction;
    style
}

fn with_background(mut style: ResolvedStyle, color: Color) -> ResolvedStyle {
    style.background = Some(color);
    style
}

fn engine(scale: f32) -> LayoutEngine {
    LayoutEngine::new(scale, Box::new(MonospaceText::default()))
}

fn close(a: f32, b: f32) {
    assert!((a - b).abs() < 0.001, "expected {b}, got {a}");
}

fn assert_rect(actual: Option<Rect>, x: f32, y: f32, width: f32, height: f32) {
    let actual = actual.expect("geometry");
    close(actual.x, x);
    close(actual.y, y);
    close(actual.width, width);
    close(actual.height, height);
}

#[test]
fn flex_row_grows_and_respects_padding_gap_and_percent() {
    let mut layout = engine(1.0);

    let mut root_style = with_direction(px(200.0, 100.0), FlexDirection::Row);
    root_style.gap = Size {
        width: DefiniteLength::Px(10.0),
        height: DefiniteLength::Px(0.0),
    };
    let root = layout.create_node(root_style, None);

    let mut a = px(50.0, 20.0);
    a.flex.grow = 1.0;
    let a = layout.create_node(a, None);
    let mut b = px(50.0, 20.0);
    b.flex.grow = 1.0;
    let b = layout.create_node(b, None);

    layout.set_children(root, &[a, b]);
    layout.add_root(root);

    let stats = layout.compute(Size {
        width: 200.0,
        height: 100.0,
    });
    assert!(stats.ran);
    assert_eq!(stats.nodes, 3);

    // 200 - 10 gap = 190; each basis 50 leaves 90 free, 45 each.
    assert_rect(layout.geometry(a), 0.0, 0.0, 95.0, 20.0);
    assert_rect(layout.geometry(b), 105.0, 0.0, 95.0, 20.0);

    // Padding and percent resolve inside the remaining content box.
    let mut padded = with_direction(px(100.0, 100.0), FlexDirection::Column);
    padded.padding = Edges {
        top: DefiniteLength::Px(2.0),
        right: DefiniteLength::Px(2.0),
        bottom: DefiniteLength::Px(2.0),
        left: DefiniteLength::Px(2.0),
    };
    padded.gap = Size {
        width: DefiniteLength::Px(0.0),
        height: DefiniteLength::Px(4.0),
    };
    let padded = layout.create_node(padded, None);
    let first = layout.create_node(style(Length::Percent(50.0), Length::Px(10.0)), None);
    let second = layout.create_node(style(Length::Percent(100.0), Length::Px(10.0)), None);
    layout.set_children(padded, &[first, second]);
    layout.add_root(padded);

    layout.compute(Size {
        width: 100.0,
        height: 100.0,
    });
    assert_rect(layout.geometry(first), 2.0, 2.0, 48.0, 10.0);
    assert_rect(layout.geometry(second), 2.0, 16.0, 96.0, 10.0);
}

#[test]
fn absolute_children_do_not_consume_flex_space() {
    let mut layout = engine(1.0);
    let root = layout.create_node(with_direction(px(200.0, 100.0), FlexDirection::Row), None);
    let before = layout.create_node(px(50.0, 20.0), None);
    let mut overlay = px(10.0, 10.0);
    overlay.position = Position::Absolute;
    let overlay = layout.create_node(overlay, None);
    let after = layout.create_node(px(50.0, 20.0), None);

    layout.set_children(root, &[before, overlay, after]);
    layout.add_root(root);
    layout.compute(Size {
        width: 200.0,
        height: 100.0,
    });

    assert_rect(layout.geometry(before), 0.0, 0.0, 50.0, 20.0);
    assert_rect(layout.geometry(after), 50.0, 0.0, 50.0, 20.0);
    assert_rect(layout.geometry(overlay), 0.0, 0.0, 10.0, 10.0);
}

#[test]
fn paint_only_style_changes_run_zero_layout() {
    let mut layout = engine(1.0);
    let root = layout.create_node(px(100.0, 100.0), None);
    let child = layout.create_node(px(10.0, 10.0), None);
    layout.set_children(root, &[child]);
    layout.add_root(root);

    layout.compute(Size {
        width: 100.0,
        height: 100.0,
    });
    let runs = layout.layout_runs();
    assert_eq!(runs, 1);

    // Background is paint-only.
    layout.set_style(
        child,
        with_background(px(10.0, 10.0), Color::new(1.0, 0.0, 0.0, 1.0)),
    );
    let stats = layout.compute(Size {
        width: 100.0,
        height: 100.0,
    });
    assert!(!stats.ran);
    assert_eq!(layout.layout_runs(), runs);
    assert!(layout.geometry_changes().is_empty());
    assert_rect(layout.geometry(child), 0.0, 0.0, 10.0, 10.0);

    // A layout change runs again and reports only the changed subtree.
    layout.set_style(child, px(20.0, 10.0));
    let stats = layout.compute(Size {
        width: 100.0,
        height: 100.0,
    });
    assert!(stats.ran);
    assert_eq!(layout.layout_runs(), runs + 1);
    assert!(layout.geometry_changes().contains(&child));
    assert!(!layout.geometry_changes().contains(&root));
    assert_rect(layout.geometry(child), 0.0, 0.0, 20.0, 10.0);
}

#[test]
fn text_measurement_drives_auto_size_and_repaints_do_not_relayout() {
    let mut layout = engine(1.0);
    let root = layout.create_node(with_direction(px(200.0, 100.0), FlexDirection::Row), None);
    let text_style = TextStyle {
        color: Color::new(0.0, 0.0, 0.0, 1.0),
        font_size: 16.0,
        line_height: 20.0,
        weight: 400,
    };
    let text = layout.create_node(
        style(Length::Auto, Length::Auto),
        Some(TextContent {
            text: "Hello".to_string(),
            style: text_style,
        }),
    );
    layout.set_children(root, &[text]);
    layout.add_root(root);

    layout.compute(Size {
        width: 200.0,
        height: 100.0,
    });
    // 5 chars * 16px * 0.6 advance = 48; line height 20.
    assert_rect(layout.geometry(text), 0.0, 0.0, 48.0, 20.0);

    layout.set_text(
        text,
        Some(TextContent {
            text: "Hello!!".to_string(),
            style: text_style,
        }),
    );
    layout.compute(Size {
        width: 200.0,
        height: 100.0,
    });
    close(layout.geometry(text).unwrap().width, 67.0);

    // Recolor text: paint only, even though text nodes are measured.
    let runs = layout.layout_runs();
    let mut recolored = style(Length::Auto, Length::Auto);
    recolored.text.color = Color::new(1.0, 0.0, 0.0, 1.0);
    layout.set_style(text, recolored);
    let stats = layout.compute(Size {
        width: 200.0,
        height: 100.0,
    });
    assert!(!stats.ran);
    assert_eq!(layout.layout_runs(), runs);

    // A font-size change is layout-relevant and must remeasure the text.
    let mut larger = style(Length::Auto, Length::Auto);
    larger.text.font_size = 32.0;
    larger.text.line_height = 40.0;
    layout.set_style(text, larger);
    let stats = layout.compute(Size {
        width: 200.0,
        height: 100.0,
    });
    assert!(stats.ran);
    let rect = layout.geometry(text).expect("geometry");
    close(rect.width, 134.0);
    close(rect.height, 40.0);
}

#[test]
fn pixel_snapping_is_correct_at_fractional_scales() {
    let mut layout = engine(1.5);
    let root = layout.create_node(px(10.0, 10.0), None);
    let one_px = layout.create_node(px(1.0, 1.0), None);
    layout.set_children(root, &[one_px]);
    layout.add_root(root);
    layout.compute(Size {
        width: 10.0,
        height: 10.0,
    });
    let rect = layout.geometry(one_px).expect("geometry");
    close(rect.width, 2.0 / 1.5);
    close(rect.height, 2.0 / 1.5);

    // Adjacent edges meet exactly after snapping.
    let mut layout = engine(1.5);
    let root = layout.create_node(with_direction(px(100.0, 20.0), FlexDirection::Row), None);
    let first = layout.create_node(px(10.4, 20.0), None);
    let second = layout.create_node(px(10.4, 20.0), None);
    layout.set_children(root, &[first, second]);
    layout.add_root(root);
    layout.compute(Size {
        width: 100.0,
        height: 20.0,
    });
    let a = layout.geometry(first).expect("first");
    let b = layout.geometry(second).expect("second");
    close(a.right(), b.x);
    close(a.width, 10.6667);
}

#[test]
fn viewport_resize_relayouts_percent_sizes_at_every_scale() {
    for scale in [1.0, 1.5, 2.0] {
        let mut layout = engine(scale);
        let root = layout.create_node(style(Length::Percent(100.0), Length::Px(50.0)), None);
        layout.add_root(root);

        layout.compute(Size {
            width: 300.0,
            height: 200.0,
        });
        let rect = layout.geometry(root).expect("geometry");
        close(rect.width, 300.0);
        close(rect.height, 50.0);

        layout.compute(Size {
            width: 600.0,
            height: 200.0,
        });
        let rect = layout.geometry(root).expect("geometry");
        close(rect.width, 600.0);

        // A scale change invalidates and re-snaps.
        let runs = layout.layout_runs();
        layout.set_scale(if scale == 1.0 { 2.0 } else { 1.0 });
        let stats = layout.compute(Size {
            width: 600.0,
            height: 200.0,
        });
        assert!(stats.ran);
        assert_eq!(layout.layout_runs(), runs + 1);
    }
}

#[test]
fn remove_node_drops_geometry_and_dirty_state() {
    let mut layout = engine(1.0);
    let root = layout.create_node(px(100.0, 100.0), None);
    let child = layout.create_node(px(10.0, 10.0), None);
    layout.set_children(root, &[child]);
    layout.add_root(root);
    layout.compute(Size {
        width: 100.0,
        height: 100.0,
    });
    assert!(layout.geometry(child).is_some());

    layout.remove_node(child);
    assert!(layout.geometry(child).is_none());
    assert_eq!(layout.dirty_nodes(), 1);
    layout.compute(Size {
        width: 100.0,
        height: 100.0,
    });
    assert_eq!(layout.dirty_nodes(), 0);
}

#[test]
fn display_none_lays_out_to_zero_without_measuring_text() {
    struct PanicMeasure;
    impl layout::TextMeasure for PanicMeasure {
        fn measure(&mut self, _request: &renderer_api::TextMeasureRequest) -> TextMetrics {
            panic!("hidden text must not be measured");
        }
    }

    let mut layout = LayoutEngine::new(1.0, Box::new(PanicMeasure));
    let root = layout.create_node(px(100.0, 100.0), None);
    let mut hidden = px(10.0, 10.0);
    hidden.display = Display::None;
    let hidden = layout.create_node(
        hidden,
        Some(TextContent {
            text: "not measured".to_string(),
            style: TextStyle::default(),
        }),
    );
    layout.set_children(root, &[hidden]);
    layout.add_root(root);
    layout.compute(Size {
        width: 100.0,
        height: 100.0,
    });
    let rect = layout.geometry(hidden).expect("geometry");
    close(rect.width, 0.0);
    close(rect.height, 0.0);
}

#[test]
fn alignment_and_justify_are_translated() {
    let mut layout = engine(1.0);
    let mut root_style = with_direction(px(100.0, 100.0), FlexDirection::Row);
    root_style.flex.align_items = Align::Center;
    root_style.flex.justify_content = Justify::End;
    let root = layout.create_node(root_style, None);
    let child = layout.create_node(px(20.0, 10.0), None);
    layout.set_children(root, &[child]);
    layout.add_root(root);
    layout.compute(Size {
        width: 100.0,
        height: 100.0,
    });
    assert_rect(layout.geometry(child), 80.0, 45.0, 20.0, 10.0);
}
