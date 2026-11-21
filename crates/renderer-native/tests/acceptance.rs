//! Phase 5 acceptance: window-scale resize, paint-only zero layout, replay,
//! and a real wgpu headless draw.

use std::cell::RefCell;
use std::rc::Rc;

use reactive_core::{create_signal, Runtime, RuntimeHandle, WriteSignal};
use renderer_api::{Color, Length, ResolvedStyle, Size, TextStyle, Transform};
use renderer_native::{
    BackendError, BackendMetrics, FrameOutput, HeadlessBackend, PaintBackend, WgpuBackend,
    WindowApp, WindowDriver,
};
use renderer_null::{NullRenderer, SharedNullRenderer};
use scene::{Primitive, Rect, Scene};
use view::{column, text, text_fn, ViewTree};

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

struct DemoApp {
    runtime: RuntimeHandle,
    tree: ViewTree,
    root: reactive_core::NodeId,
    text_node: reactive_core::NodeId,
    label: WriteSignal<String>,
    frames: u64,
}

impl DemoApp {
    fn new() -> Self {
        let runtime = Runtime::new().into_handle();
        let renderer = Rc::new(RefCell::new(NullRenderer::new()));
        let tree = ViewTree::new(
            Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
            runtime.clone(),
        );
        let cx = runtime.cx();
        let (label, set_label) = create_signal(&cx, "hello".to_string());

        let mut root_style = style(Length::Percent(100.0), Length::Percent(100.0));
        root_style.background = Some(Color::new(0.1, 0.1, 0.12, 1.0));
        let mut text_style = style(Length::Auto, Length::Auto);
        text_style.text.color = Color::new(0.9, 0.9, 0.9, 1.0);

        let root = tree.mount(
            &cx,
            column(vec![
                text_fn(move |cx| label.get(cx)).style(text_style),
                text("side"),
            ])
            .style(root_style),
        );
        let text_node = tree.children(root)[0];
        Self {
            runtime,
            tree,
            root,
            text_node,
            label: set_label,
            frames: 0,
        }
    }

    fn set_label(&self, value: &str) {
        let cx = self.runtime.cx();
        self.label.set(&cx, value.to_string());
    }

    fn recolor(&self, color: Color) {
        let mut style = style(Length::Auto, Length::Auto);
        style.text.color = color;
        self.tree.set_style(self.text_node, style);
    }

    fn text_primitive(&self) -> Option<(Color, Rect)> {
        let scene = self.tree.build_scene();
        scene.ops().iter().find_map(|primitive| match primitive {
            Primitive::Text { color, rect, .. } => Some((*color, *rect)),
            _ => None,
        })
    }
}

impl Drop for DemoApp {
    fn drop(&mut self) {
        let cx = self.runtime.cx();
        self.tree.unmount(&cx, self.root);
    }
}

impl WindowApp for DemoApp {
    fn resize(&mut self, metrics: BackendMetrics) {
        self.tree
            .set_viewport(metrics.logical_width(), metrics.logical_height());
        self.tree.set_scale(metrics.scale);
    }

    fn frame(&mut self, _metrics: BackendMetrics) -> FrameOutput {
        let cx = self.runtime.cx();
        self.frames += 1;
        self.tree.frame(&cx, self.frames);
        let scene = self.tree.build_scene();
        let damage = self.tree.take_scene_damage();
        FrameOutput::new(scene, damage)
    }
}

fn assert_rect(actual: Option<layout::Rect>, x: f32, y: f32, width: f32, height: f32) {
    let actual = actual.expect("geometry");
    assert!((actual.x - x).abs() < 0.001, "{actual:?}");
    assert!((actual.y - y).abs() < 0.001, "{actual:?}");
    assert!((actual.width - width).abs() < 0.001, "{actual:?}");
    assert!((actual.height - height).abs() < 0.001, "{actual:?}");
}

#[test]
fn window_resize_is_correct_at_one_one_and_a_half_and_two_x() {
    let app = DemoApp::new();
    let backend = HeadlessBackend::new(300, 200, 1.0);
    let mut driver = WindowDriver::new(app, backend);

    for (physical_width, physical_height, scale) in
        [(300, 200, 1.0_f32), (450, 300, 1.5), (600, 400, 2.0)]
    {
        driver.resize(physical_width, physical_height, scale);
        driver.redraw().expect("redraw");
        let metrics = driver.metrics();
        assert!((metrics.logical_width() - 300.0).abs() < 0.001);
        assert!((metrics.logical_height() - 200.0).abs() < 0.001);

        let app = driver.app();
        assert_rect(app.tree.layout_rect(app.root), 0.0, 0.0, 300.0, 200.0);
        let frame = driver.backend().last().expect("frame");
        assert_eq!(frame.metrics.physical_width, physical_width);
        assert_eq!(frame.metrics.physical_height, physical_height);
        assert!((frame.metrics.scale - scale).abs() < 0.001);
        assert_eq!(frame.stats.fill_quads, 1);
        assert!(frame.stats.glyph_quads > 0);
        assert!(frame.stats.presented);
    }

    let resizes = driver.backend().resizes();
    assert_eq!(resizes.len(), 3);
    assert_eq!(driver.frames(), 3);
}

#[test]
fn paint_only_recolor_is_zero_layout_and_replays_through_the_backend() {
    let app = DemoApp::new();
    let backend = HeadlessBackend::new(300, 200, 1.0);
    let mut driver = WindowDriver::new(app, backend);
    driver.redraw().expect("first frame");
    let misses = driver.app().tree.text_cache_stats().misses;

    driver.app().recolor(Color::new(1.0, 0.0, 0.0, 1.0));
    driver.redraw().expect("recolor frame");

    assert!(
        !driver.app().tree.last_layout().ran,
        "recolor is paint-only"
    );
    let frame = driver.backend().last().expect("frame");
    assert!(!frame.damage.is_empty(), "recolor produces damage");
    assert!(driver.app().tree.scene_stats().replayed >= 1);
    let text = driver.app().text_primitive().expect("text primitive");
    assert_eq!(text.0, Color::new(1.0, 0.0, 0.0, 1.0));
    assert_eq!(
        driver.app().tree.text_cache_stats().misses,
        misses,
        "recolor must not create a new text layout"
    );
}

#[test]
fn layout_change_updates_the_scene_through_the_backend() {
    let app = DemoApp::new();
    let backend = HeadlessBackend::new(300, 200, 1.0);
    let mut driver = WindowDriver::new(app, backend);
    driver.redraw().expect("first frame");
    let before = driver.app().text_primitive().expect("text").1;

    driver.app().set_label("hello world");
    driver.redraw().expect("layout frame");

    assert!(driver.app().tree.last_layout().ran);
    let after = driver.app().text_primitive().expect("text").1;
    assert!(after.width > before.width);
    assert!(!driver.backend().last().expect("frame").damage.is_empty());
}

#[test]
#[cfg_attr(miri, ignore = "wgpu uses platform graphics drivers")]
fn wgpu_headless_backend_draws_a_red_rect() {
    let mut backend = match WgpuBackend::new_headless(64, 64, 1.0) {
        Ok(backend) => backend,
        Err(BackendError::NoAdapter) => {
            eprintln!("skipping wgpu test: no adapter");
            return;
        }
        Err(error) => panic!("wgpu init failed: {error}"),
    };
    eprintln!("wgpu adapter: {}", backend.adapter_info());

    let mut scene = Scene::new();
    scene.push(Primitive::Fill {
        node: 1,
        rect: Rect::new(0.0, 0.0, 32.0, 32.0),
        color: Color::new(1.0, 0.0, 0.0, 1.0),
        radius: Default::default(),
        clip: None,
    });
    let stats = backend
        .render(&scene, &[Rect::new(0.0, 0.0, 32.0, 32.0)])
        .unwrap();
    backend.present().unwrap();
    assert_eq!(stats.fill_quads, 1);
    assert_eq!(stats.damage_rects, 1);

    let red = backend.read_pixel(16, 16).expect("red pixel");
    let background = backend.read_pixel(60, 60).expect("background pixel");
    assert!(red[0] > 200, "red channel {red:?}");
    assert!(red[1] < 50, "green channel {red:?}");
    assert!(red[2] < 50, "blue channel {red:?}");
    assert!(red[3] > 200, "alpha channel {red:?}");
    assert!(background[0] < 100, "background {background:?}");
}

#[test]
#[cfg_attr(miri, ignore = "timing test is not meaningful under Miri")]
fn clean_headless_frames_fit_a_60hz_budget() {
    let app = DemoApp::new();
    let backend = HeadlessBackend::new(300, 200, 1.0);
    let mut driver = WindowDriver::new(app, backend);
    driver.redraw().expect("warmup");
    let start = std::time::Instant::now();
    for _ in 0..120 {
        driver.redraw().expect("frame");
    }
    let per_frame = start.elapsed() / 120;
    eprintln!("headless frame time: {per_frame:?}");
    assert!(
        per_frame < std::time::Duration::from_millis(16),
        "headless frame time {per_frame:?} exceeds a 60 Hz budget"
    );
    assert_eq!(driver.frames(), 121);
}

#[test]
#[cfg_attr(miri, ignore = "wgpu uses platform graphics drivers")]
fn wgpu_clips_fills_to_the_content_mask() {
    let mut backend = match WgpuBackend::new_headless(64, 64, 1.0) {
        Ok(backend) => backend,
        Err(BackendError::NoAdapter) => {
            eprintln!("skipping wgpu clip test: no adapter");
            return;
        }
        Err(error) => panic!("wgpu init failed: {error}"),
    };
    let mut scene = Scene::new();
    scene.push(Primitive::Fill {
        node: 1,
        rect: Rect::new(0.0, 0.0, 64.0, 64.0),
        color: Color::new(1.0, 0.0, 0.0, 1.0),
        radius: Default::default(),
        clip: Some(Rect::new(0.0, 0.0, 32.0, 64.0)),
    });
    backend.render(&scene, &[]).expect("clip render");
    backend.present().expect("present");

    let visible = backend.read_pixel(16, 32).expect("visible pixel");
    let clipped = backend.read_pixel(48, 32).expect("clipped pixel");
    assert!(visible[0] > 200, "visible {visible:?}");
    assert!(clipped[0] < 100, "clipped {clipped:?}");
}

#[test]
#[cfg_attr(miri, ignore = "wgpu and system fonts are not available under Miri")]
fn wgpu_glyph_atlas_renders_platform_text() {
    let mut text = text::TextSystem::new(Box::new(text::CosmicShaper::new()));
    let style = TextStyle {
        color: Color::new(0.95, 0.95, 0.95, 1.0),
        font_size: 32.0,
        line_height: 40.0,
        weight: 400,
    };
    let layout = text.layout("A", style, None);
    if !layout
        .lines
        .iter()
        .flat_map(|line| &line.glyphs)
        .any(|glyph| glyph.image.is_some())
    {
        eprintln!("skipping glyph atlas test: no system font mask");
        return;
    }
    let mut backend = match WgpuBackend::new_headless(96, 96, 1.0) {
        Ok(backend) => backend,
        Err(BackendError::NoAdapter) => {
            eprintln!("skipping glyph atlas test: no adapter");
            return;
        }
        Err(error) => panic!("wgpu init failed: {error}"),
    };
    let mut scene = Scene::new();
    scene.push(Primitive::Text {
        node: 1,
        rect: Rect::new(8.0, 8.0, 80.0, 48.0),
        layout,
        color: style.color,
        clip: None,
        transform: Transform::IDENTITY,
        underline: false,
    });
    backend.render(&scene, &[]).expect("glyph render");
    backend.present().expect("present");
    assert!(
        backend.atlas_len() > 0,
        "real glyphs must populate the atlas"
    );
    let (_, _, pixels) = backend.read_image().expect("readback");
    let bright = pixels
        .chunks_exact(4)
        .filter(|pixel| pixel[0] > 150)
        .count();
    assert!(bright > 5, "expected glyph pixels, got {bright}");
    eprintln!(
        "glyph atlas: {} glyphs, {} bright pixels at 1x",
        backend.atlas_len(),
        bright
    );
    assert!(backend.atlas_stats().misses > 0);

    for (width, height, scale) in [(144, 96, 1.5_f32), (192, 128, 2.0)] {
        backend.resize(width, height, scale);
        backend.render(&scene, &[]).expect("glyph render");
        backend.present().expect("present");
        let (_, _, pixels) = backend.read_image().expect("readback");
        let bright = pixels
            .chunks_exact(4)
            .filter(|pixel| pixel[0] > 150)
            .count();
        assert!(
            bright > 5,
            "expected glyph pixels at {scale}x, got {bright}"
        );
        eprintln!("glyph atlas: {bright} bright pixels at {scale}x");
    }
}

#[test]
#[cfg_attr(miri, ignore = "wgpu uses platform graphics drivers")]
fn wgpu_backend_drives_resizes_through_the_window_loop() {
    let backend = match WgpuBackend::new_headless(300, 200, 1.0) {
        Ok(backend) => backend,
        Err(BackendError::NoAdapter) => {
            eprintln!("skipping wgpu driver test: no adapter");
            return;
        }
        Err(error) => panic!("wgpu init failed: {error}"),
    };
    let mut driver = WindowDriver::new(DemoApp::new(), backend);

    for (width, height, scale) in [(300, 200, 1.0_f32), (450, 300, 1.5), (600, 400, 2.0)] {
        driver.resize(width, height, scale);
        let stats = driver.redraw().expect("wgpu frame");
        assert!(stats.fill_quads >= 1);
        assert!(stats.presented);
        let pixel = driver.backend().read_pixel(250, 150).expect("pixel");
        assert!(
            pixel[0] > 75 && pixel[0] < 140,
            "background pixel {pixel:?} at {width}x{height}@{scale}"
        );
    }
    assert_eq!(driver.frames(), 3);
}

#[test]
fn headless_backend_records_no_extra_work_for_clean_frames() {
    let app = DemoApp::new();
    let backend = HeadlessBackend::new(300, 200, 1.0);
    let mut driver = WindowDriver::new(app, backend);
    driver.redraw().expect("first frame");
    let before = driver.app().tree.scene_stats();

    driver.redraw().expect("clean frame");
    let after = driver.app().tree.scene_stats();
    assert_eq!(after.rebuilt, 0);
    assert!(after.replayed >= 1);
    assert!(before.primitives == after.primitives);
}

#[test]
#[cfg_attr(miri, ignore = "wgpu uses platform graphics drivers")]
fn wgpu_incremental_damage_matches_a_full_render() {
    let mut backend = match WgpuBackend::new_headless(64, 64, 1.0) {
        Ok(backend) => backend,
        Err(BackendError::NoAdapter) => {
            eprintln!("skipping wgpu damage test: no adapter");
            return;
        }
        Err(error) => panic!("wgpu init failed: {error}"),
    };
    let blue = Color::new(0.0, 0.0, 1.0, 1.0);
    let red = Color::new(1.0, 0.0, 0.0, 1.0);
    let green = Color::new(0.0, 1.0, 0.0, 1.0);
    let fill = |node, rect, color| Primitive::Fill {
        node,
        rect,
        color,
        radius: Default::default(),
        clip: None,
    };

    let mut first = Scene::new();
    first.push(fill(1, Rect::new(0.0, 0.0, 64.0, 64.0), blue));
    first.push(fill(2, Rect::new(8.0, 8.0, 16.0, 16.0), red));
    let first_stats = backend
        .render(&first, &[Rect::new(0.0, 0.0, 64.0, 64.0)])
        .expect("first render");
    backend.present().expect("first present");
    assert_eq!(first_stats.scissored_area, 64 * 64, "first frame is full");

    let mut second = Scene::new();
    second.push(fill(1, Rect::new(0.0, 0.0, 64.0, 64.0), blue));
    second.push(fill(2, Rect::new(32.0, 32.0, 16.0, 16.0), green));
    let second_stats = backend
        .render(
            &second,
            &[
                Rect::new(8.0, 8.0, 16.0, 16.0),
                Rect::new(32.0, 32.0, 16.0, 16.0),
            ],
        )
        .expect("incremental render");
    backend.present().expect("incremental present");
    assert_eq!(
        second_stats.scissored_area,
        16 * 16 * 2,
        "only damage pixels are rasterized"
    );
    let (_, _, incremental) = backend.read_image().expect("incremental readback");

    let mut fresh = match WgpuBackend::new_headless(64, 64, 1.0) {
        Ok(backend) => backend,
        Err(BackendError::NoAdapter) => return,
        Err(error) => panic!("wgpu init failed: {error}"),
    };
    fresh.render(&second, &[]).expect("full render");
    fresh.present().expect("full present");
    let (_, _, full) = fresh.read_image().expect("full readback");

    assert_eq!(
        incremental, full,
        "a scissored frame must match the same scene rendered from scratch"
    );
}
