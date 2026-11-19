//! Surface-path smoke run: opens a window, renders real glyphs, resizes to
//! several physical sizes, prints metrics, and exits.
//!
//! Run with: cargo run -p renderer-native --example window_smoke --features window

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use renderer_api::{Color, Edges, TextStyle, Transform};
use renderer_native::{PaintBackend, WgpuBackend};
use scene::{Primitive, Rect, Scene};
use text::{CosmicShaper, ShapeRequest, ShapedLayout, Shaper};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalSize},
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::Window,
};

struct Smoke {
    backend: WgpuBackend,
    layout: Arc<ShapedLayout>,
    frame: u64,
    resize_index: usize,
    last_frame: Instant,
    samples: Arc<Mutex<Vec<f64>>>,
}

const SIZES: [(u32, u32); 3] = [(400, 300), (600, 450), (800, 600)];

struct SmokeState {
    window: Arc<Window>,
    smoke: Smoke,
}

struct SmokeRunner {
    state: Option<SmokeState>,
    samples: Arc<Mutex<Vec<f64>>>,
    target_frames: u64,
    started: Instant,
}

impl ApplicationHandler for SmokeRunner {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("signals-native renderer smoke")
            .with_inner_size(LogicalSize::new(400.0, 300.0));
        let window = Arc::new(event_loop.create_window(attributes).expect("window"));
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
        let smoke = Smoke {
            backend,
            layout,
            frame: 0,
            resize_index: 0,
            last_frame: Instant::now(),
            samples: Arc::clone(&self.samples),
        };
        window.request_redraw();
        self.state = Some(SmokeState { window, smoke });
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                state.smoke.backend.resize(
                    size.width,
                    size.height,
                    state.window.scale_factor() as f32,
                );
                state.window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                let size = state.window.inner_size();
                state
                    .smoke
                    .backend
                    .resize(size.width, size.height, scale_factor as f32);
                state.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                let now = Instant::now();
                state
                    .smoke
                    .samples
                    .lock()
                    .unwrap()
                    .push(now.duration_since(state.smoke.last_frame).as_secs_f64() * 1000.0);
                state.smoke.last_frame = now;
                let metrics = state.smoke.backend.metrics();
                let scene = state.smoke.scene(metrics);
                if let Err(error) = state.smoke.backend.render(&scene, &[]) {
                    eprintln!("render failed: {error}");
                    event_loop.exit();
                    return;
                }
                if let Err(error) = state.smoke.backend.present() {
                    eprintln!("present failed: {error}");
                    event_loop.exit();
                    return;
                }
                state.smoke.frame += 1;
                if state.smoke.frame <= 3 || state.smoke.frame % 30 == 0 {
                    eprintln!(
                        "frame {}: {}x{} scale {:.2} logical {:.0}x{:.0} atlas {}",
                        state.smoke.frame,
                        metrics.physical_width,
                        metrics.physical_height,
                        metrics.scale,
                        metrics.logical_width(),
                        metrics.logical_height(),
                        state.smoke.backend.atlas_len()
                    );
                }

                if state.smoke.frame % 2 == 0 && state.smoke.resize_index + 1 < SIZES.len() {
                    state.smoke.resize_index += 1;
                    let (width, height) = SIZES[state.smoke.resize_index];
                    let _ = state
                        .window
                        .request_inner_size(PhysicalSize::new(width, height));
                }
                if state.smoke.frame >= self.target_frames
                    || self.started.elapsed() > Duration::from_secs(30)
                {
                    event_loop.exit();
                } else {
                    state.window.request_redraw();
                }
            }
            _ => {}
        }
    }
}

impl Smoke {
    fn scene(&self, metrics: renderer_native::BackendMetrics) -> Scene {
        let mut scene = Scene::new();
        scene.push(Primitive::Fill {
            node: 1,
            rect: Rect::new(0.0, 0.0, metrics.logical_width(), metrics.logical_height()),
            color: Color::new(0.08, 0.09, 0.12, 1.0),
            radius: Edges::default(),
            clip: None,
        });
        scene.push(Primitive::Text {
            node: 2,
            rect: Rect::new(24.0, 24.0, metrics.logical_width() - 48.0, 48.0),
            layout: Arc::clone(&self.layout),
            color: Color::new(0.92, 0.92, 0.95, 1.0),
            clip: None,
            transform: Transform::IDENTITY,
            underline: false,
        });
        scene
    }
}

fn main() {
    let target_frames = std::env::var("SMOKE_FRAMES")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(240);
    let samples = Arc::new(Mutex::new(Vec::<f64>::new()));
    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut runner = SmokeRunner {
        state: None,
        samples: Arc::clone(&samples),
        target_frames,
        started: Instant::now(),
    };
    event_loop.run_app(&mut runner).expect("event loop run");

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
