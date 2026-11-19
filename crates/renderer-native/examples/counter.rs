//! Two-instance counter reference app on the native window and wgpu backend.
//!
//! Each counter owns a local signal; both share a module-level signal. The
//! local double is a memo. Clicks and keyboard focus are handled by the view
//! tree, not by renderer-specific event hooks.
//!
//! Set `COUNTER_SNAPSHOT=/path/to/frame.rgba` to render one offscreen frame at
//! 1520x720 (2x) without opening a window.

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

use reactive_core::{create_memo, create_signal, ReadSignal, Runtime, RuntimeHandle, WriteSignal};
use renderer_api::{
    Color, CursorIcon, DefiniteLength, Edges, KeyEvent, Length, PointerEvent, ResolvedStyle, Size,
    TextStyle,
};
use renderer_native::{
    run_windowed, BackendMetrics, FrameOutput, PaintBackend, WgpuBackend, WindowApp,
};
use renderer_null::{NullRenderer, SharedNullRenderer};
use text::CosmicShaper;
use view::{button, column, text_fn, StateStyle, View, ViewTree};

struct CounterApp {
    runtime: RuntimeHandle,
    tree: ViewTree,
    root: reactive_core::NodeId,
    frames: u64,
}

fn style() -> ResolvedStyle {
    ResolvedStyle {
        text: TextStyle {
            color: Color::new(0.92, 0.92, 0.95, 1.0),
            font_size: 20.0,
            line_height: 26.0,
            weight: 400,
        },
        ..ResolvedStyle::default()
    }
}

fn label_style() -> ResolvedStyle {
    ResolvedStyle {
        text: TextStyle {
            color: Color::new(0.92, 0.92, 0.95, 1.0),
            font_size: 16.0,
            line_height: 20.0,
            weight: 400,
        },
        ..ResolvedStyle::default()
    }
}

fn counter(cx: &reactive_core::Cx<'_>, shared: ReadSignal<u32>, bump: WriteSignal<u32>) -> View {
    let (local, set_local) = create_signal(cx, 0_u32);
    let double = create_memo(cx, move |cx, _| local.get(cx) * 2);

    let mut stack = style();
    stack.flex.direction = renderer_api::FlexDirection::Column;
    stack.gap = Size {
        width: DefiniteLength::Px(0.0),
        height: DefiniteLength::Px(8.0),
    };
    stack.padding = Edges {
        top: DefiniteLength::Px(12.0),
        right: DefiniteLength::Px(12.0),
        bottom: DefiniteLength::Px(12.0),
        left: DefiniteLength::Px(12.0),
    };
    stack.background = Some(Color::new(0.16, 0.17, 0.2, 1.0));

    let mut button_style = style();
    button_style.size = Size {
        width: Length::Px(260.0),
        height: Length::Px(40.0),
    };
    button_style.background = Some(Color::new(0.24, 0.28, 0.36, 1.0));
    button_style.padding = Edges {
        top: DefiniteLength::Px(8.0),
        right: DefiniteLength::Px(10.0),
        bottom: DefiniteLength::Px(8.0),
        left: DefiniteLength::Px(10.0),
    };
    button_style.border = Edges {
        top: 1.0,
        right: 1.0,
        bottom: 1.0,
        left: 1.0,
    };
    button_style.border_color = Some(Color::new(0.36, 0.42, 0.54, 1.0));

    let hover_style = StateStyle::new().background(Color::new(0.3, 0.36, 0.46, 1.0));
    let focus_style = StateStyle::new()
        .background(Color::new(0.2, 0.42, 0.62, 1.0))
        .border_color(Color::new(0.62, 0.82, 1.0, 1.0));

    column(vec![
        button(
            vec![text_fn(move |cx| format!("local {}", local.get(cx))).style(label_style())],
            move |cx| set_local.update_with(cx, |value| value + 1),
        )
        .style(button_style)
        .hover_style(hover_style)
        .focus_style(focus_style)
        .cursor(CursorIcon::Pointer),
        button(
            vec![text_fn(move |cx| format!("shared {}", shared.get(cx))).style(label_style())],
            move |cx| bump.update_with(cx, |value| value + 1),
        )
        .style(button_style)
        .hover_style(hover_style)
        .focus_style(focus_style)
        .cursor(CursorIcon::Pointer),
        text_fn(move |cx| format!("local double {}", double.get(cx))).style(style()),
    ])
    .style(stack)
}

impl CounterApp {
    fn new() -> Self {
        let runtime = Runtime::new().into_handle();
        let renderer = Rc::new(RefCell::new(NullRenderer::new()));
        let tree = ViewTree::new_with_shaper(
            Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
            runtime.clone(),
            Box::new(CosmicShaper::new()),
        );
        let cx = runtime.cx();
        let (shared, bump) = create_signal(&cx, 0_u32);

        let mut root_style = style();
        root_style.flex.direction = renderer_api::FlexDirection::Row;
        root_style.gap = Size {
            width: DefiniteLength::Px(16.0),
            height: DefiniteLength::Px(0.0),
        };
        root_style.padding = Edges {
            top: DefiniteLength::Px(16.0),
            right: DefiniteLength::Px(16.0),
            bottom: DefiniteLength::Px(16.0),
            left: DefiniteLength::Px(16.0),
        };
        root_style.background = Some(Color::new(0.08, 0.09, 0.12, 1.0));

        let root = tree.mount(
            &cx,
            column(vec![counter(&cx, shared, bump), counter(&cx, shared, bump)]).style(root_style),
        );
        Self {
            runtime,
            tree,
            root,
            frames: 0,
        }
    }
}

impl Drop for CounterApp {
    fn drop(&mut self) {
        let cx = self.runtime.cx();
        self.tree.unmount(&cx, self.root);
    }
}

impl WindowApp for CounterApp {
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

    fn accesskit_update(&self) -> Option<accesskit::TreeUpdate> {
        self.tree.accesskit_tree()
    }

    fn accesskit_action(&mut self, request: &accesskit::ActionRequest) -> bool {
        self.tree.dispatch_accesskit_action(request)
    }
}

fn write_snapshot(path: &str) {
    let metrics = BackendMetrics::new(1520, 720, 2.0);
    let mut app = CounterApp::new();
    app.resize(metrics);
    let output = app.frame(metrics);
    let mut backend = WgpuBackend::new_headless(
        metrics.physical_width,
        metrics.physical_height,
        metrics.scale,
    )
    .expect("headless wgpu");
    backend
        .render(&output.scene, &output.damage)
        .expect("render");
    backend.present().expect("present");
    let (width, height, pixels) = backend.read_image().expect("readback");
    let mut file = std::fs::File::create(path).expect("snapshot file");
    file.write_all(&pixels).expect("snapshot pixels");
    eprintln!("snapshot: {width}x{height}");
}

fn main() {
    if let Ok(path) = std::env::var("COUNTER_SNAPSHOT") {
        write_snapshot(&path);
        return;
    }
    let app = CounterApp::new();
    if let Err(error) = run_windowed("signals-native counter", 760, 360, app) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
