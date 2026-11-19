//! Surface-path smoke run: opens a window, renders real glyphs, resizes to
//! several physical sizes, prints metrics, and exits.
//!
//! Run with: cargo run -p renderer-native --example window_smoke --features window

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use renderer_api::{Color, Edges, TextStyle};
use renderer_native::{PaintBackend, WgpuBackend};
use scene::{Primitive, Rect, Scene};
use text::{CosmicShaper, ShapeRequest, ShapedLayout, Shaper};
use winit::{
    dpi::{LogicalSize, PhysicalSize},
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
};

struct Smoke {
    backend: WgpuBackend,
    layout: Arc<ShapedLayout>,
    frame: u64,
    resize_index: usize,
    started: Instant,
    last_frame: Instant,
    samples: Arc<Mutex<Vec<f64>>>,
}

const SIZES: [(u32, u32); 3] = [(400, 300), (600, 450), (800, 600)];

impl Smoke {
    fn scene(&self, metrics: renderer_native::BackendMetrics) -> Scene {
        let mut scene = Scene::new();
        scene.push(Primitive::Fill {
            node: 1,
            rect: Rect::new(0.0, 0.0, metrics.logical_width(), metrics.logical_height()),
            color: Color::new(0.08, 0.09, 0.12, 1.0),
            radius: Edges::default(),
        });
        scene.push(Primitive::Text {
            node: 2,
            rect: Rect::new(24.0, 24.0, metrics.logical_width() - 48.0, 48.0),
            layout: Arc::clone(&self.layout),
            color: Color::new(0.92, 0.92, 0.95, 1.0),
        });
        scene
    }
}

fn main() {
    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let window = Arc::new(
        WindowBuilder::new()
            .with_title("signals-native renderer smoke")
            .with_inner_size(LogicalSize::new(400.0, 300.0))
            .build(&event_loop)
            .expect("window"),
    );
    let size = window.inner_size();
    let scale = window.scale_factor() as f32;
    let backend = WgpuBackend::new_surface(Arc::clone(&window), size.width, size.height, scale)
        .expect("wgpu surface");
    eprintln!("surface backend: {}", backend.adapter_info());

    let mut shaper = CosmicShaper::new();
    let layout = Arc::new(shaper.shape(ShapeRequest {
        text: "signals-native-renderer",
        style: TextStyle {
            color: Color::new(0.92, 0.92, 0.95, 1.0),
            font_size: 28.0,
            line_height: 34.0,
            weight: 500,
        },
        max_width: None,
    }));
    let samples = Arc::new(Mutex::new(Vec::<f64>::new()));
    let target_frames = std::env::var("SMOKE_FRAMES")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(240);
    let mut smoke = Smoke {
        backend,
        layout,
        frame: 0,
        resize_index: 0,
        started: Instant::now(),
        last_frame: Instant::now(),
        samples: Arc::clone(&samples),
    };
    window.request_redraw();

    event_loop
        .run(move |event, target| {
            if let Event::WindowEvent { event, .. } = event {
                match event {
                    WindowEvent::CloseRequested => target.exit(),
                    WindowEvent::Resized(size) => {
                        smoke
                            .backend
                            .resize(size.width, size.height, window.scale_factor() as f32);
                        window.request_redraw();
                    }
                    WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                        let size = window.inner_size();
                        smoke
                            .backend
                            .resize(size.width, size.height, scale_factor as f32);
                        window.request_redraw();
                    }
                    WindowEvent::RedrawRequested => {
                        let now = Instant::now();
                        smoke
                            .samples
                            .lock()
                            .unwrap()
                            .push(now.duration_since(smoke.last_frame).as_secs_f64() * 1000.0);
                        smoke.last_frame = now;
                        let metrics = smoke.backend.metrics();
                        let scene = smoke.scene(metrics);
                        if let Err(error) = smoke.backend.render(&scene, &[]) {
                            eprintln!("render failed: {error}");
                            target.exit();
                            return;
                        }
                        if let Err(error) = smoke.backend.present() {
                            eprintln!("present failed: {error}");
                            target.exit();
                            return;
                        }
                        smoke.frame += 1;
                        if smoke.frame <= 3 || smoke.frame % 30 == 0 {
                            eprintln!(
                                "frame {}: {}x{} scale {:.2} logical {:.0}x{:.0} atlas {}",
                                smoke.frame,
                                metrics.physical_width,
                                metrics.physical_height,
                                metrics.scale,
                                metrics.logical_width(),
                                metrics.logical_height(),
                                smoke.backend.atlas_len()
                            );
                        }

                        if smoke.frame % 2 == 0 && smoke.resize_index + 1 < SIZES.len() {
                            smoke.resize_index += 1;
                            let (width, height) = SIZES[smoke.resize_index];
                            let _ = window.request_inner_size(PhysicalSize::new(width, height));
                        }
                        if smoke.frame >= target_frames
                            || smoke.started.elapsed() > Duration::from_secs(30)
                        {
                            target.exit();
                        } else {
                            window.request_redraw();
                        }
                    }
                    _ => {}
                }
            }
        })
        .expect("event loop run");

    let mut samples = samples.lock().unwrap().clone();
    samples.retain(|sample| *sample > 0.0);
    let steady: Vec<f64> = samples.iter().skip(10).copied().collect();
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if !samples.is_empty() {
        let sum: f64 = samples.iter().sum();
        let avg = sum / samples.len() as f64;
        let p50 = samples[samples.len() / 2];
        let p95 = samples[((samples.len() as f64 * 0.95) as usize).min(samples.len() - 1)];
        let max = samples[samples.len() - 1];
        eprintln!(
            "frame time: avg {avg:.3} ms, p50 {p50:.3} ms, p95 {p95:.3} ms, max {max:.3} ms, {:.1} fps ({} samples)",
            1000.0 / avg,
            samples.len()
        );
        if !steady.is_empty() {
            let mut steady = steady;
            steady.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let sum: f64 = steady.iter().sum();
            let avg = sum / steady.len() as f64;
            let p95 = steady[((steady.len() as f64 * 0.95) as usize).min(steady.len() - 1)];
            eprintln!(
                "steady frame time (skip 10): avg {avg:.3} ms, p95 {p95:.3} ms, max {:.3} ms, {:.1} fps ({} samples)",
                steady[steady.len() - 1],
                1000.0 / avg,
                steady.len()
            );
        }
    }
}
