use std::fmt;
use std::sync::Arc;

use winit::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
};

use crate::{BackendError, WgpuBackend, WindowApp, WindowDriver};

/// A window-loop failure.
#[derive(Debug)]
pub enum WindowError {
    /// The event loop could not start.
    EventLoop(String),
    /// The window could not be created.
    Window(String),
    /// The graphics backend failed.
    Backend(BackendError),
}

impl fmt::Display for WindowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EventLoop(message) => write!(f, "event loop failed: {message}"),
            Self::Window(message) => write!(f, "window creation failed: {message}"),
            Self::Backend(error) => write!(f, "backend failed: {error}"),
        }
    }
}

impl std::error::Error for WindowError {}

/// Run a native window with a wgpu surface.
///
/// The app is resized once before the first frame and again on every physical
/// resize or scale-factor change. Redraws are event-driven.
pub fn run_windowed<A: WindowApp + 'static>(
    title: &str,
    width: u32,
    height: u32,
    app: A,
) -> Result<(), WindowError> {
    let event_loop = EventLoop::new().map_err(|error| WindowError::EventLoop(error.to_string()))?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let window = Arc::new(
        WindowBuilder::new()
            .with_title(title)
            .with_inner_size(LogicalSize::new(width as f64, height as f64))
            .build(&event_loop)
            .map_err(|error| WindowError::Window(error.to_string()))?,
    );
    let size = window.inner_size();
    let scale = window.scale_factor() as f32;
    let backend = WgpuBackend::new_surface(Arc::clone(&window), size.width, size.height, scale)
        .map_err(WindowError::Backend)?;
    let mut driver = WindowDriver::new(app, backend);
    window.request_redraw();

    event_loop
        .run(move |event, target| {
            if let Event::WindowEvent { event, .. } = event {
                match event {
                    WindowEvent::CloseRequested => target.exit(),
                    WindowEvent::Resized(size) => {
                        driver.resize(size.width, size.height, window.scale_factor() as f32);
                        window.request_redraw();
                    }
                    WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                        let size = window.inner_size();
                        driver.resize(size.width, size.height, scale_factor as f32);
                        window.request_redraw();
                    }
                    WindowEvent::RedrawRequested => match driver.redraw() {
                        Ok(_) => {}
                        Err(BackendError::Outdated | BackendError::Lost) => {
                            let _ = driver.backend_mut().recover();
                            window.request_redraw();
                        }
                        Err(error) => {
                            eprintln!("renderer-native: {error}");
                        }
                    },
                    _ => {}
                }
            }
        })
        .map_err(|error| WindowError::EventLoop(error.to_string()))
}
