//! Native window-driver input routing into the retained view tree.

use std::cell::RefCell;
use std::rc::Rc;

use reactive_core::{create_signal, ReadSignal, Runtime, RuntimeHandle};
use renderer_api::{
    Key, KeyEvent, Length, Modifiers, Overflow, PointerButton, PointerEvent, ResolvedStyle, Size,
    TextStyle, WheelEvent,
};
use renderer_native::{BackendMetrics, FrameOutput, HeadlessBackend, WindowApp, WindowDriver};
use renderer_null::{NullRenderer, SharedNullRenderer};
use view::{button_text, column, text_fn, KeyStroke, ViewTree};

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

struct InputApp {
    runtime: RuntimeHandle,
    tree: ViewTree,
    root: reactive_core::NodeId,
    count: ReadSignal<u32>,
    frames: u64,
}

impl InputApp {
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
            column(vec![
                text_fn(move |cx| format!("count {}", count.get(cx))),
                button_text("+", move |cx| {
                    set_count.update_with(cx, |value| value + 1);
                })
                .on_key(KeyStroke::character('x'), move |cx| {
                    set_count.update_with(cx, |value| value + 1);
                })
                .style(px(80.0, 30.0)),
            ])
            .style(px(200.0, 200.0)),
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

impl Drop for InputApp {
    fn drop(&mut self) {
        let cx = self.runtime.cx();
        self.tree.unmount(&cx, self.root);
    }
}

impl WindowApp for InputApp {
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

    fn pointer_move(&mut self, event: &PointerEvent) {
        self.tree.pointer_move(event);
    }

    fn pointer_down(&mut self, event: &PointerEvent) {
        self.tree.pointer_down(event);
    }

    fn pointer_up(&mut self, event: &PointerEvent) {
        self.tree.pointer_up(event);
    }

    fn key_down(&mut self, event: &KeyEvent) -> bool {
        self.tree.key_down(event)
    }

    fn wheel(&mut self, event: &WheelEvent) -> bool {
        self.tree.wheel(event)
    }
}

struct ScrollApp {
    runtime: RuntimeHandle,
    tree: ViewTree,
    root: reactive_core::NodeId,
    frames: u64,
}

impl ScrollApp {
    fn new() -> Self {
        let runtime = Runtime::new().into_handle();
        let renderer = Rc::new(RefCell::new(NullRenderer::new()));
        let tree = ViewTree::new(
            Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
            runtime.clone(),
        );
        let cx = runtime.cx();
        let mut style = px(100.0, 100.0);
        style.overflow = Overflow::Scroll;
        let root = tree.mount(
            &cx,
            column(vec![column(vec![]).style(px(100.0, 300.0))]).style(style),
        );
        Self {
            runtime,
            tree,
            root,
            frames: 0,
        }
    }
}

impl Drop for ScrollApp {
    fn drop(&mut self) {
        let cx = self.runtime.cx();
        self.tree.unmount(&cx, self.root);
    }
}

impl WindowApp for ScrollApp {
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

    fn wheel(&mut self, event: &WheelEvent) -> bool {
        self.tree.wheel(event)
    }
}

#[test]
fn window_driver_routes_wheel_to_scroll_state() {
    let backend = HeadlessBackend::new(200, 200, 1.0);
    let mut driver = WindowDriver::new(ScrollApp::new(), backend);
    driver.redraw().expect("initial frame");

    assert!(driver.wheel(&WheelEvent::new(50.0, 50.0, 0.0, 40.0)));
    let state = driver
        .app()
        .tree
        .scroll_state(driver.app().root)
        .expect("scroll state");
    assert_eq!(state.offset_y, 40.0);
}

#[test]
fn window_driver_routes_pointer_and_key_events() {
    let backend = HeadlessBackend::new(200.0 as u32, 200.0 as u32, 1.0);
    let mut driver = WindowDriver::new(InputApp::new(), backend);
    driver.redraw().expect("initial frame");

    let button = driver.app().tree.children(driver.app().root)[1];
    let rect = driver
        .app()
        .tree
        .layout_rect(button)
        .expect("button geometry");
    let point = PointerEvent::new(rect.x + 5.0, rect.y + 5.0, None);

    driver.pointer_move(&point);
    driver.pointer_down(&PointerEvent::new(
        point.x,
        point.y,
        Some(PointerButton::Primary),
    ));
    driver.pointer_up(&PointerEvent::new(
        point.x,
        point.y,
        Some(PointerButton::Primary),
    ));
    driver.redraw().expect("click frame");

    {
        let cx = driver.app().runtime.cx();
        assert_eq!(driver.app().count.get(&cx), 1);
    }
    assert!(driver.app().tree.is_focused(button));

    let key = KeyEvent::new(Key::Character('x'), Modifiers::NONE);
    assert!(driver.key_down(&key));
    driver.redraw().expect("key frame");
    {
        let cx = driver.app().runtime.cx();
        assert_eq!(driver.app().count.get(&cx), 2);
    }
    assert!(driver.last_stats().presented);
}
