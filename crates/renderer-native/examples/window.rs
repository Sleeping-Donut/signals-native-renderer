//! Minimal native window: a filled background with one shaped text run.

use std::sync::Arc;

use renderer_api::{Color, Edges, TextStyle};
use renderer_native::{run_windowed, BackendMetrics, FrameOutput, WindowApp};
use scene::{Primitive, Rect, Scene};
use text::{MonospaceShaper, ShapeRequest, ShapedLayout, Shaper, TextSystem};

struct Demo {
    layout: Arc<ShapedLayout>,
}

impl Demo {
    fn new() -> Self {
        let style = TextStyle {
            color: Color::new(0.92, 0.92, 0.95, 1.0),
            font_size: 32.0,
            line_height: 40.0,
            weight: 500,
        };
        let mut shaper = MonospaceShaper::default();
        let layout = shaper.shape(ShapeRequest {
            text: "signals-native-renderer",
            style,
            max_width: None,
        });
        let _cache = TextSystem::new(Box::new(MonospaceShaper::default()));
        Self {
            layout: Arc::new(layout),
        }
    }
}

impl WindowApp for Demo {
    fn resize(&mut self, _metrics: BackendMetrics) {}

    fn frame(&mut self, metrics: BackendMetrics) -> FrameOutput {
        let mut scene = Scene::new();
        scene.push(Primitive::Fill {
            node: 1,
            rect: Rect::new(0.0, 0.0, metrics.logical_width(), metrics.logical_height()),
            color: Color::new(0.08, 0.09, 0.12, 1.0),
            radius: Edges::default(),
        });
        scene.push(Primitive::Text {
            node: 2,
            rect: Rect::new(32.0, 32.0, metrics.logical_width() - 64.0, 40.0),
            layout: Arc::clone(&self.layout),
            color: Color::new(0.92, 0.92, 0.95, 1.0),
        });
        FrameOutput::new(
            scene,
            vec![Rect::new(
                0.0,
                0.0,
                metrics.logical_width(),
                metrics.logical_height(),
            )],
        )
    }
}

fn main() {
    if let Err(error) = run_windowed("signals-native-renderer", 800, 600, Demo::new()) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
