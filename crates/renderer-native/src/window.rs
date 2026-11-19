use std::fmt;
use std::sync::Arc;

use renderer_api::{
    CursorIcon, KeyEvent, Modifiers, NamedKey, PointerButton, PointerEvent, WheelEvent,
};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalPosition, LogicalSize},
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    keyboard::{Key as WinitKey, NamedKey as WinitNamedKey},
    window::{CursorIcon as WinitCursorIcon, Window},
};

use crate::{BackendError, PaintBackend, WgpuBackend, WindowApp, WindowDriver};

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

/// AccessKit events routed through the winit event loop.
#[derive(Debug)]
struct UserEvent(accesskit_winit::Event);

impl From<accesskit_winit::Event> for UserEvent {
    fn from(event: accesskit_winit::Event) -> Self {
        Self(event)
    }
}

struct WindowState<A: WindowApp> {
    window: Arc<Window>,
    driver: WindowDriver<A, WgpuBackend>,
    adapter: accesskit_winit::Adapter,
    cursor: (f32, f32),
    modifiers: Modifiers,
}

struct WindowRunner<A: WindowApp> {
    title: String,
    width: u32,
    height: u32,
    app: Option<A>,
    state: Option<WindowState<A>>,
    proxy: EventLoopProxy<UserEvent>,
    error: Option<WindowError>,
}

impl<A: WindowApp> WindowRunner<A> {
    fn fail(&mut self, event_loop: &ActiveEventLoop, error: WindowError) {
        if self.error.is_none() {
            self.error = Some(error);
        }
        event_loop.exit();
    }

    fn initialize(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let Some(app) = self.app.take() else {
            return;
        };
        let attributes = Window::default_attributes()
            .with_title(self.title.clone())
            .with_inner_size(LogicalSize::new(
                f64::from(self.width),
                f64::from(self.height),
            ))
            .with_visible(false);
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                self.fail(event_loop, WindowError::Window(error.to_string()));
                return;
            }
        };
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
        let backend =
            match WgpuBackend::new_surface(Arc::clone(&window), size.width, size.height, scale) {
                Ok(backend) => backend,
                Err(error) => {
                    self.fail(event_loop, WindowError::Backend(error));
                    return;
                }
            };
        let driver = WindowDriver::new(app, backend);
        let adapter = accesskit_winit::Adapter::with_event_loop_proxy(
            event_loop,
            &window,
            self.proxy.clone(),
        );
        window.set_ime_allowed(true);
        window.set_visible(true);
        window.request_redraw();
        self.state = Some(WindowState {
            window,
            driver,
            adapter,
            cursor: (0.0, 0.0),
            modifiers: Modifiers::NONE,
        });
    }

    fn handle_accesskit(&mut self, event_loop: &ActiveEventLoop, event: accesskit_winit::Event) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if state.window.id() != event.window_id {
            return;
        }
        if apply_accesskit_event(&mut state.driver, event.window_event) {
            state.window.request_redraw();
        }
        let _ = event_loop;
    }

    fn redraw(state: &mut WindowState<A>) {
        match state.driver.redraw() {
            Ok(_) => {
                if let Some(rect) = state.driver.ime_cursor_area() {
                    state.window.set_ime_cursor_area(
                        LogicalPosition::new(f64::from(rect.x), f64::from(rect.y)),
                        LogicalSize::new(
                            f64::from(rect.width.max(1.0)),
                            f64::from(rect.height.max(1.0)),
                        ),
                    );
                }
                if let Some(update) = state.driver.accesskit_update().cloned() {
                    state.adapter.update_if_active(|| update);
                }
            }
            Err(BackendError::Outdated | BackendError::Lost) => {
                let _ = state.driver.backend_mut().recover();
                state.window.request_redraw();
            }
            Err(error) => {
                eprintln!("renderer-native: {error}");
            }
        }
    }
}

impl<A: WindowApp> ApplicationHandler<UserEvent> for WindowRunner<A> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.initialize(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        self.handle_accesskit(event_loop, event.0);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if state.window.id() != window_id {
            return;
        }
        state.adapter.process_event(&state.window, &event);
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::CursorMoved { position, .. } => {
                let scale = state.window.scale_factor() as f32;
                state.cursor = (position.x as f32 / scale, position.y as f32 / scale);
                state
                    .driver
                    .pointer_move(&PointerEvent::new(state.cursor.0, state.cursor.1, None));
                state
                    .window
                    .set_cursor(map_cursor(state.driver.cursor_icon()));
            }
            WindowEvent::MouseInput {
                state: element,
                button,
                ..
            } => {
                let button = match button {
                    MouseButton::Left => Some(PointerButton::Primary),
                    MouseButton::Right => Some(PointerButton::Secondary),
                    MouseButton::Middle => Some(PointerButton::Middle),
                    _ => None,
                };
                if let Some(button) = button {
                    let event = PointerEvent::new(state.cursor.0, state.cursor.1, Some(button));
                    match element {
                        ElementState::Pressed => state.driver.pointer_down(&event),
                        ElementState::Released => state.driver.pointer_up(&event),
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
                state.driver.wheel(&WheelEvent::new(
                    state.cursor.0,
                    state.cursor.1,
                    delta_x,
                    delta_y,
                ));
            }
            WindowEvent::Ime(ime) => match ime {
                winit::event::Ime::Preedit(text, cursor) => {
                    state
                        .driver
                        .ime_preedit(&text, cursor.map(|(start, end)| start..end));
                }
                winit::event::Ime::Commit(text) => {
                    state.driver.ime_commit(&text);
                }
                winit::event::Ime::Disabled => state.driver.ime_cancel(),
                winit::event::Ime::Enabled => {}
            },
            WindowEvent::ModifiersChanged(modifiers) => {
                let modifiers = modifiers.state();
                state.modifiers = Modifiers::new(
                    modifiers.shift_key(),
                    modifiers.control_key(),
                    modifiers.alt_key(),
                    modifiers.super_key(),
                );
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if let Some(key) = map_key(&event.logical_key) {
                    let key_event = KeyEvent {
                        key,
                        modifiers: state.modifiers,
                        repeat: event.repeat,
                        text: event.text.as_ref().map(ToString::to_string),
                    };
                    state.driver.key_down(&key_event);
                }
            }
            WindowEvent::Resized(size) => {
                state
                    .driver
                    .resize(size.width, size.height, state.window.scale_factor() as f32);
                state.window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                let size = state.window.inner_size();
                state
                    .driver
                    .resize(size.width, size.height, scale_factor as f32);
                state.window.request_redraw();
            }
            WindowEvent::RedrawRequested => Self::redraw(state),
            _ => {}
        }
    }
}

/// Handle one AccessKit platform event. Returns whether a redraw is needed.
fn apply_accesskit_event<A: WindowApp, B: PaintBackend>(
    driver: &mut WindowDriver<A, B>,
    event: accesskit_winit::WindowEvent,
) -> bool {
    match event {
        accesskit_winit::WindowEvent::InitialTreeRequested => true,
        accesskit_winit::WindowEvent::ActionRequested(request) => driver.accesskit_action(&request),
        accesskit_winit::WindowEvent::AccessibilityDeactivated => false,
    }
}

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
    let mut builder = EventLoop::<UserEvent>::with_user_event();
    let event_loop = builder
        .build()
        .map_err(|error| WindowError::EventLoop(error.to_string()))?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let mut runner = WindowRunner {
        title: title.to_string(),
        width,
        height,
        app: Some(app),
        state: None,
        proxy,
        error: None,
    };
    event_loop
        .run_app(&mut runner)
        .map_err(|error| WindowError::EventLoop(error.to_string()))?;
    if let Some(error) = runner.error {
        return Err(error);
    }
    Ok(())
}

fn map_cursor(icon: CursorIcon) -> WinitCursorIcon {
    match icon {
        CursorIcon::Default => WinitCursorIcon::Default,
        CursorIcon::Pointer => WinitCursorIcon::Pointer,
        CursorIcon::Text => WinitCursorIcon::Text,
        CursorIcon::Grab => WinitCursorIcon::Grab,
        CursorIcon::Grabbing => WinitCursorIcon::Grabbing,
        CursorIcon::Crosshair => WinitCursorIcon::Crosshair,
        CursorIcon::Move => WinitCursorIcon::Move,
        CursorIcon::NotAllowed => WinitCursorIcon::NotAllowed,
        CursorIcon::ResizeHorizontal => WinitCursorIcon::EwResize,
        CursorIcon::ResizeVertical => WinitCursorIcon::NsResize,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    use accesskit::{Action, ActionRequest, TreeId};
    use reactive_core::{create_signal, ReadSignal, Runtime, RuntimeHandle};
    use renderer_api::{Length, ResolvedStyle, Size, TextStyle};
    use renderer_null::{NullRenderer, SharedNullRenderer};
    use view::{button_text, column, ViewTree};

    use crate::HeadlessBackend;

    struct TestApp {
        runtime: RuntimeHandle,
        tree: ViewTree,
        root: reactive_core::NodeId,
        count: ReadSignal<u32>,
        frames: u64,
    }

    impl TestApp {
        fn new() -> Self {
            let runtime = Runtime::new().into_handle();
            let renderer = Rc::new(RefCell::new(NullRenderer::new()));
            let tree = ViewTree::new(
                Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
                runtime.clone(),
            );
            let cx = runtime.cx();
            let (count, set_count) = create_signal(&cx, 0_u32);
            let root = tree.mount(
                &cx,
                column(vec![button_text("+", move |cx| {
                    set_count.set(cx, count.get(cx) + 1)
                })
                .style(px(80.0, 30.0))])
                .style(px(200.0, 100.0)),
            );
            Self {
                runtime,
                tree,
                root,
                count,
                frames: 0,
            }
        }
    }

    impl Drop for TestApp {
        fn drop(&mut self) {
            let cx = self.runtime.cx();
            self.tree.unmount(&cx, self.root);
        }
    }

    fn px(width: f32, height: f32) -> ResolvedStyle {
        ResolvedStyle {
            size: Size {
                width: Length::Px(width),
                height: Length::Px(height),
            },
            text: TextStyle::default(),
            ..ResolvedStyle::default()
        }
    }

    impl WindowApp for TestApp {
        fn resize(&mut self, metrics: crate::BackendMetrics) {
            self.tree
                .set_viewport(metrics.logical_width(), metrics.logical_height());
            self.tree.set_scale(metrics.scale);
        }

        fn frame(&mut self, _metrics: crate::BackendMetrics) -> crate::FrameOutput {
            let cx = self.runtime.cx();
            self.frames += 1;
            self.tree.frame(&cx, self.frames);
            let scene = self.tree.build_scene();
            let damage = self.tree.take_scene_damage();
            crate::FrameOutput::new(scene, damage)
        }

        fn accesskit_update(&self) -> Option<accesskit::TreeUpdate> {
            self.tree.accesskit_tree()
        }

        fn accesskit_action(&mut self, request: &ActionRequest) -> bool {
            self.tree.dispatch_accesskit_action(request)
        }
    }

    #[test]
    fn accesskit_platform_events_route_to_driver_actions() {
        let backend = HeadlessBackend::new(200, 100, 1.0);
        let mut driver = WindowDriver::new(TestApp::new(), backend);
        driver.redraw().expect("initial frame");
        let update = driver.accesskit_update().expect("update");
        let button = update
            .nodes
            .iter()
            .find_map(|(id, node)| {
                (node.role() == accesskit::Role::Button && node.label() == Some("+")).then_some(*id)
            })
            .expect("button");

        assert!(apply_accesskit_event(
            &mut driver,
            accesskit_winit::WindowEvent::InitialTreeRequested,
        ));
        assert!(apply_accesskit_event(
            &mut driver,
            accesskit_winit::WindowEvent::ActionRequested(ActionRequest {
                action: Action::Click,
                target_tree: TreeId::ROOT,
                target_node: button,
                data: None,
            }),
        ));
        driver.redraw().expect("action frame");
        {
            let cx = driver.app().runtime.cx();
            assert_eq!(driver.app().count.get(&cx), 1);
        }
        assert!(!apply_accesskit_event(
            &mut driver,
            accesskit_winit::WindowEvent::AccessibilityDeactivated,
        ));
    }
}
