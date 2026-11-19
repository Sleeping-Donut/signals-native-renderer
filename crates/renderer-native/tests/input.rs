//! Native window-driver input routing into the retained view tree.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use reactive_core::{create_signal, ReadSignal, Runtime, RuntimeHandle};
use renderer_api::{
    CursorIcon, Key, KeyEvent, Length, Modifiers, Overflow, PointerButton, PointerEvent,
    ResolvedStyle, Size, TextStyle, WheelEvent,
};
use renderer_native::{BackendMetrics, FrameOutput, HeadlessBackend, WindowApp, WindowDriver};
use renderer_null::{NullRenderer, SharedNullRenderer};
use view::{button_text, column, text_fn, KeyStroke, TextInput, TextInputHandler, ViewTree};

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

    fn accesskit_update(&self) -> Option<accesskit::TreeUpdate> {
        self.tree.accesskit_tree()
    }

    fn accesskit_action(&mut self, request: &accesskit::ActionRequest) -> bool {
        self.tree.dispatch_accesskit_action(request)
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

struct HoverApp {
    runtime: RuntimeHandle,
    tree: ViewTree,
    root: reactive_core::NodeId,
    frames: u64,
}

impl HoverApp {
    fn new() -> Self {
        let runtime = Runtime::new().into_handle();
        let renderer = Rc::new(RefCell::new(NullRenderer::new()));
        let tree = ViewTree::new(
            Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
            runtime.clone(),
        );
        let cx = runtime.cx();
        let root = tree.mount(
            &cx,
            button_text("hover", |_| {})
                .style(px(80.0, 30.0))
                .cursor(CursorIcon::Pointer)
                .tooltip("Save file"),
        );
        Self {
            runtime,
            tree,
            root,
            frames: 0,
        }
    }
}

impl Drop for HoverApp {
    fn drop(&mut self) {
        let cx = self.runtime.cx();
        self.tree.unmount(&cx, self.root);
    }
}

impl WindowApp for HoverApp {
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

    fn cursor_icon(&self) -> CursorIcon {
        self.tree.cursor().unwrap_or(CursorIcon::Default)
    }
}

struct TextApp {
    runtime: RuntimeHandle,
    tree: ViewTree,
    root: reactive_core::NodeId,
    input: Rc<RefCell<Box<dyn TextInputHandler>>>,
    frames: u64,
}

impl TextApp {
    fn new() -> Self {
        let runtime = Runtime::new().into_handle();
        let renderer = Rc::new(RefCell::new(NullRenderer::new()));
        let tree = ViewTree::new(
            Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
            runtime.clone(),
        );
        let cx = runtime.cx();
        let root = tree.mount(&cx, text_fn(|_| "input".to_string()).style(px(120.0, 30.0)));
        let input = Rc::new(RefCell::new(
            Box::new(TextInput::new("")) as Box<dyn TextInputHandler>
        ));
        tree.register_text_input(root, Rc::clone(&input));
        tree.set_focus(Some(root));
        Self {
            runtime,
            tree,
            root,
            input,
            frames: 0,
        }
    }
}

impl Drop for TextApp {
    fn drop(&mut self) {
        let cx = self.runtime.cx();
        self.tree.unmount(&cx, self.root);
    }
}

impl WindowApp for TextApp {
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

    fn key_down(&mut self, event: &KeyEvent) -> bool {
        self.tree.key_down(event)
    }

    fn ime_preedit(&mut self, text: &str, cursor: Option<Range<usize>>) {
        self.tree.ime_preedit(text, cursor);
    }

    fn ime_commit(&mut self, text: &str) {
        self.tree.ime_commit(text);
    }

    fn ime_cancel(&mut self) {
        self.tree.ime_cancel();
    }

    fn accesskit_update(&self) -> Option<accesskit::TreeUpdate> {
        self.tree.accesskit_tree()
    }

    fn ime_cursor_area(&self) -> Option<scene::Rect> {
        self.tree
            .ime_cursor_area()
            .map(|rect| scene::Rect::new(rect.x, rect.y, rect.width, rect.height))
    }
}

#[test]
fn window_driver_exposes_accesskit_update() {
    let backend = HeadlessBackend::new(200, 100, 1.0);
    let mut driver = WindowDriver::new(TextApp::new(), backend);
    driver.redraw().expect("initial frame");
    let update = driver.accesskit_update().expect("accesskit update");
    let root_id = accesskit::NodeId(
        driver
            .app()
            .tree
            .renderer_id(driver.app().root)
            .expect("renderer id"),
    );
    assert_eq!(update.focus, root_id);
    let node = update
        .nodes
        .iter()
        .find_map(|(id, node)| (*id == root_id).then_some(node))
        .expect("root node");
    assert_eq!(node.role(), accesskit::Role::TextInput);
    assert_eq!(driver.accesskit_update_count(), 1);
}

#[test]
fn window_driver_exposes_ime_cursor_area() {
    let backend = HeadlessBackend::new(200, 100, 1.0);
    let mut driver = WindowDriver::new(TextApp::new(), backend);
    driver.redraw().expect("initial frame");
    let area = driver.ime_cursor_area().expect("caret area");
    assert!(area.width > 0.0 && area.height > 0.0);
}

#[test]
fn window_driver_routes_accesskit_press_and_focus_actions() {
    let backend = HeadlessBackend::new(200, 200, 1.0);
    let mut driver = WindowDriver::new(InputApp::new(), backend);
    driver.redraw().expect("initial frame");
    let update = driver.accesskit_update().expect("update");
    let button_id = update
        .nodes
        .iter()
        .find_map(|(id, node)| {
            (node.role() == accesskit::Role::Button && node.label() == Some("+")).then_some(*id)
        })
        .expect("button node");

    let focus = accesskit::ActionRequest {
        action: accesskit::Action::Focus,
        target_tree: accesskit::TreeId::ROOT,
        target_node: button_id,
        data: None,
    };
    assert!(driver.accesskit_action(&focus));
    assert!(driver
        .app()
        .tree
        .is_focused(driver.app().tree.children(driver.app().root)[1]));

    let click = accesskit::ActionRequest {
        action: accesskit::Action::Click,
        target_tree: accesskit::TreeId::ROOT,
        target_node: button_id,
        data: None,
    };
    assert!(driver.accesskit_action(&click));
    driver.redraw().expect("action frame");
    {
        let cx = driver.app().runtime.cx();
        assert_eq!(driver.app().count.get(&cx), 1);
    }
}

#[test]
fn window_driver_maps_hover_cursor_and_tooltip_layer() {
    let backend = HeadlessBackend::new(200, 100, 1.0);
    let mut driver = WindowDriver::new(HoverApp::new(), backend);
    driver.redraw().expect("initial frame");
    assert_eq!(driver.cursor_icon(), CursorIcon::Default);

    driver.pointer_move(&PointerEvent::new(10.0, 10.0, None));
    assert_eq!(driver.cursor_icon(), CursorIcon::Pointer);

    driver
        .app()
        .tree
        .set_tooltip_delay(Duration::from_millis(0));
    driver.redraw().expect("tooltip frame");
    let frame = driver.backend().last().expect("frame");
    assert!(
        frame
            .scene
            .ops()
            .iter()
            .any(|primitive| matches!(primitive, scene::Primitive::Tooltip { .. })),
        "tooltip primitive should be in the submitted scene"
    );
    assert!(frame.stats.tooltip_quads >= 1);

    driver.pointer_move(&PointerEvent::new(500.0, 500.0, None));
    assert_eq!(driver.cursor_icon(), CursorIcon::Default);
    driver.redraw().expect("cleared frame");
    let frame = driver.backend().last().expect("frame");
    assert!(!frame
        .scene
        .ops()
        .iter()
        .any(|primitive| matches!(primitive, scene::Primitive::Tooltip { .. })));
}

#[test]
fn window_driver_routes_ime_preedit_commit_and_cancel() {
    let backend = HeadlessBackend::new(200, 100, 1.0);
    let mut driver = WindowDriver::new(TextApp::new(), backend);
    driver.redraw().expect("initial frame");

    driver.ime_preedit("に", Some(0..3));
    assert!(driver.app().input.borrow().is_composing());

    driver.ime_commit("日本");
    assert_eq!(driver.app().input.borrow().text(), "日本");
    assert!(!driver.app().input.borrow().is_composing());

    driver.ime_preedit("x", None);
    driver.ime_cancel();
    assert!(!driver.app().input.borrow().is_composing());
    assert_eq!(driver.app().input.borrow().text(), "日本");

    let key = KeyEvent::new(Key::Character('a'), Modifiers::NONE);
    assert!(driver.key_down(&key));
    assert_eq!(driver.app().input.borrow().text(), "日本a");
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
