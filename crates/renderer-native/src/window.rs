use std::fmt;
use std::sync::Arc;

use renderer_api::{KeyEvent, Modifiers, NamedKey, PointerButton, PointerEvent, WheelEvent};
use winit::{
    dpi::{LogicalPosition, LogicalSize},
    event::{ElementState, Event, MouseButton, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    keyboard::{Key as WinitKey, NamedKey as WinitNamedKey},
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
    window.set_ime_allowed(true);
    let mut cursor = (0.0_f32, 0.0_f32);
    let mut modifiers = Modifiers::NONE;
    window.request_redraw();

    event_loop
        .run(move |event, target| {
            if let Event::WindowEvent { event, .. } = event {
                match event {
                    WindowEvent::CloseRequested => target.exit(),
                    WindowEvent::CursorMoved { position, .. } => {
                        let scale = window.scale_factor() as f32;
                        cursor = (position.x as f32 / scale, position.y as f32 / scale);
                        driver.pointer_move(&PointerEvent::new(cursor.0, cursor.1, None));
                    }
                    WindowEvent::MouseInput { state, button, .. } => {
                        let button = match button {
                            MouseButton::Left => Some(PointerButton::Primary),
                            MouseButton::Right => Some(PointerButton::Secondary),
                            MouseButton::Middle => Some(PointerButton::Middle),
                            _ => None,
                        };
                        if let Some(button) = button {
                            let event = PointerEvent::new(cursor.0, cursor.1, Some(button));
                            match state {
                                ElementState::Pressed => driver.pointer_down(&event),
                                ElementState::Released => driver.pointer_up(&event),
                            }
                        }
                    }
                    WindowEvent::MouseWheel { delta, .. } => {
                        let (delta_x, delta_y) = match delta {
                            winit::event::MouseScrollDelta::LineDelta(x, y) => (x * 20.0, y * 20.0),
                            winit::event::MouseScrollDelta::PixelDelta(position) => {
                                (position.x as f32, position.y as f32)
                            }
                        };
                        driver.wheel(&WheelEvent::new(cursor.0, cursor.1, delta_x, delta_y));
                    }
                    WindowEvent::Ime(ime) => match ime {
                        winit::event::Ime::Preedit(text, cursor) => {
                            driver.ime_preedit(&text, cursor.map(|(start, end)| start..end));
                        }
                        winit::event::Ime::Commit(text) => {
                            driver.ime_commit(&text);
                        }
                        winit::event::Ime::Disabled => driver.ime_cancel(),
                        winit::event::Ime::Enabled => {}
                    },
                    WindowEvent::ModifiersChanged(state) => {
                        let state = state.state();
                        modifiers = Modifiers::new(
                            state.shift_key(),
                            state.control_key(),
                            state.alt_key(),
                            state.super_key(),
                        );
                    }
                    WindowEvent::KeyboardInput { event, .. }
                        if event.state == ElementState::Pressed =>
                    {
                        if let Some(key) = map_key(&event.logical_key) {
                            let key_event = KeyEvent {
                                key,
                                modifiers,
                                repeat: event.repeat,
                                text: event.text.as_ref().map(ToString::to_string),
                            };
                            driver.key_down(&key_event);
                        }
                    }
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
                        Ok(_) => {
                            if let Some(rect) = driver.ime_cursor_area() {
                                window.set_ime_cursor_area(
                                    LogicalPosition::new(f64::from(rect.x), f64::from(rect.y)),
                                    LogicalSize::new(
                                        f64::from(rect.width.max(1.0)),
                                        f64::from(rect.height.max(1.0)),
                                    ),
                                );
                            }
                        }
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

fn map_key(key: &WinitKey) -> Option<renderer_api::Key> {
    match key {
        WinitKey::Character(text) => text.chars().next().map(renderer_api::Key::Character),
        WinitKey::Named(named) => map_named(*named).map(renderer_api::Key::Named),
        _ => None,
    }
}

fn map_named(named: WinitNamedKey) -> Option<NamedKey> {
    Some(match named {
        WinitNamedKey::Enter => NamedKey::Enter,
        WinitNamedKey::Escape => NamedKey::Escape,
        WinitNamedKey::Tab => NamedKey::Tab,
        WinitNamedKey::Space => NamedKey::Space,
        WinitNamedKey::Backspace => NamedKey::Backspace,
        WinitNamedKey::Delete => NamedKey::Delete,
        WinitNamedKey::ArrowLeft => NamedKey::ArrowLeft,
        WinitNamedKey::ArrowRight => NamedKey::ArrowRight,
        WinitNamedKey::ArrowUp => NamedKey::ArrowUp,
        WinitNamedKey::ArrowDown => NamedKey::ArrowDown,
        WinitNamedKey::Home => NamedKey::Home,
        WinitNamedKey::End => NamedKey::End,
        WinitNamedKey::PageUp => NamedKey::PageUp,
        WinitNamedKey::PageDown => NamedKey::PageDown,
        WinitNamedKey::F1 => NamedKey::Function(1),
        WinitNamedKey::F2 => NamedKey::Function(2),
        WinitNamedKey::F3 => NamedKey::Function(3),
        WinitNamedKey::F4 => NamedKey::Function(4),
        WinitNamedKey::F5 => NamedKey::Function(5),
        WinitNamedKey::F6 => NamedKey::Function(6),
        WinitNamedKey::F7 => NamedKey::Function(7),
        WinitNamedKey::F8 => NamedKey::Function(8),
        WinitNamedKey::F9 => NamedKey::Function(9),
        WinitNamedKey::F10 => NamedKey::Function(10),
        WinitNamedKey::F11 => NamedKey::Function(11),
        WinitNamedKey::F12 => NamedKey::Function(12),
        _ => return None,
    })
}
