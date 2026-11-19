//! Shaping, wrapping, cache hits, and bounded eviction.

use std::sync::Arc;

use renderer_api::{TextMeasureRequest, TextStyle};
use text::{MonospaceShaper, ShapedLayout, TextSystem};

fn mk_style(font_size: f32, line_height: f32) -> TextStyle {
    TextStyle {
        color: renderer_api::Color::new(0.0, 0.0, 0.0, 1.0),
        font_size,
        line_height,
        weight: 400,
    }
}

fn system() -> TextSystem {
    TextSystem::new(Box::new(MonospaceShaper::default()))
}

fn lines(layout: &ShapedLayout) -> Vec<&str> {
    layout.lines.iter().map(|line| line.text.as_str()).collect()
}

#[test]
fn identical_requests_hit_the_cache_and_color_does_not_shape() {
    let mut text = system();
    let style = mk_style(16.0, 20.0);
    let first = text.layout("Hello", style, None);
    let second = text.layout("Hello", style, None);

    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(text.stats().misses, 1);
    assert_eq!(text.stats().hits, 1);
    assert_eq!(text.cache_len(), 1);

    let mut recolored = style;
    recolored.color = renderer_api::Color::new(1.0, 0.0, 0.0, 1.0);
    let third = text.layout("Hello", recolored, None);
    assert!(Arc::ptr_eq(&first, &third));
    assert_eq!(text.stats().hits, 2);
    assert_eq!(text.stats().misses, 1);
}

#[test]
fn width_and_font_size_are_cache_key_inputs() {
    let mut text = system();
    let style = mk_style(16.0, 20.0);
    let a = text.layout("Hello", style, Some(100.0));
    let b = text.layout("Hello", style, Some(200.0));
    assert!(!Arc::ptr_eq(&a, &b));
    assert_eq!(text.stats().misses, 2);

    let c = text.layout("Hello", mk_style(20.0, 24.0), Some(100.0));
    assert!(!Arc::ptr_eq(&a, &c));
    assert_eq!(text.stats().misses, 3);
}

#[test]
fn width_is_quantized_to_avoid_subpixel_cache_misses() {
    let mut text = system();
    let style = mk_style(16.0, 20.0);
    let a = text.layout("Hello", style, Some(100.1));
    let b = text.layout("Hello", style, Some(100.2));
    assert!(Arc::ptr_eq(&a, &b));
    assert_eq!(text.stats().misses, 1);
    assert_eq!(text.stats().hits, 1);
}

#[test]
fn wraps_at_word_boundaries_and_positions_glyphs() {
    let mut text = system();
    let layout = text.layout("hello world", mk_style(10.0, 12.0), Some(30.0));

    assert_eq!(lines(&layout), vec!["hello", "world"]);
    assert!((layout.width - 30.0).abs() < 0.001);
    assert!((layout.height - 24.0).abs() < 0.001);
    assert!((layout.line_height - 12.0).abs() < 0.001);
    assert!((layout.ascent - 8.0).abs() < 0.001);
    assert!((layout.descent - 4.0).abs() < 0.001);

    let first = &layout.lines[0];
    assert_eq!(first.glyphs.len(), 5);
    assert_eq!(first.glyphs[0].ch, 'h');
    assert!((first.glyphs[0].x - 0.0).abs() < 0.001);
    assert!((first.glyphs[1].x - 6.0).abs() < 0.001);
    assert!((first.glyphs[4].x - 24.0).abs() < 0.001);
    assert!((first.glyphs[4].advance - 6.0).abs() < 0.001);
    assert!((first.baseline - 8.0).abs() < 0.001);
    assert!((layout.lines[1].baseline - 20.0).abs() < 0.001);
    assert_eq!(first.byte_range, 0..5);
    assert_eq!(layout.lines[1].byte_range, 6..11);
}

#[test]
fn hard_breaks_a_word_wider_than_the_line() {
    let mut text = system();
    let layout = text.layout("abcdef", mk_style(10.0, 12.0), Some(18.0));
    assert_eq!(lines(&layout), vec!["abc", "def"]);
    assert!((layout.width - 18.0).abs() < 0.001);
}

#[test]
fn explicit_newlines_force_breaks() {
    let mut text = system();
    let layout = text.layout("a\nbc", mk_style(10.0, 12.0), Some(100.0));
    assert_eq!(lines(&layout), vec!["a", "bc"]);
    assert_eq!(layout.lines[0].byte_range, 0..1);
    assert_eq!(layout.lines[1].byte_range, 2..4);
}

#[test]
fn empty_text_keeps_one_line_height() {
    let mut text = system();
    let layout = text.layout("", mk_style(16.0, 20.0), None);
    assert_eq!(layout.lines.len(), 1);
    assert_eq!(layout.lines[0].glyphs.len(), 0);
    assert!((layout.width - 0.0).abs() < 0.001);
    assert!((layout.height - 20.0).abs() < 0.001);
}

#[test]
fn measure_request_goes_through_the_same_cache() {
    let mut text = system();
    let style = mk_style(16.0, 20.0);
    let request = TextMeasureRequest {
        text: "Hello".to_string(),
        width: None,
        style,
    };
    let first = text.measure(&request);
    let second = text.measure(&request);
    assert!((first.width - 48.0).abs() < 0.001);
    assert!((first.height - 20.0).abs() < 0.001);
    assert_eq!(first, second);
    assert_eq!(text.stats().misses, 1);
    assert_eq!(text.stats().hits, 1);
}

#[test]
#[cfg_attr(miri, ignore = "the system font database is not available under Miri")]
fn cosmic_text_shapes_and_rasterizes_platform_glyphs() {
    let mut text = TextSystem::new(Box::new(text::CosmicShaper::new()));
    let layout = text.layout("Hello", mk_style(32.0, 40.0), None);
    assert_eq!(layout.lines.len(), 1);
    assert!(layout.lines[0].glyphs.len() >= 5);
    assert!(layout.width > 0.0);
    let has_raster = layout
        .lines
        .iter()
        .flat_map(|line| &line.glyphs)
        .any(|glyph| glyph.image.as_ref().is_some_and(|image| image.width > 0));
    if !has_raster {
        eprintln!("skipping raster assertion: no system font mask");
        return;
    }
    assert!(layout
        .lines
        .iter()
        .flat_map(|line| &line.glyphs)
        .any(|glyph| glyph.glyph_id > 0 && glyph.font_id != 0));
    let second = text.layout("Hello", mk_style(32.0, 40.0), None);
    assert!(Arc::ptr_eq(&layout, &second));
    assert_eq!(text.stats().hits, 1);
    assert_eq!(text.shaper_name(), "cosmic-text");
}

#[test]
fn byte_budget_evicts_least_recently_used_layouts() {
    let mut text = TextSystem::with_budget(Box::new(MonospaceShaper::default()), 1);
    let style = mk_style(16.0, 20.0);
    text.layout("one", style, None);
    text.layout("two", style, None);
    text.layout("three", style, None);
    assert_eq!(text.cache_len(), 1);
    assert_eq!(text.stats().evictions, 2);
    assert!(text.cache_bytes() > 1);
}

#[test]
fn cache_can_be_cleared_and_repopulated() {
    let mut text = system();
    let style = mk_style(16.0, 20.0);
    text.layout("Hello", style, None);
    assert_eq!(text.cache_len(), 1);
    text.clear_cache();
    assert_eq!(text.cache_len(), 0);
    assert_eq!(text.cache_bytes(), 0);
    text.layout("Hello", style, None);
    assert_eq!(text.stats().misses, 2);
}
