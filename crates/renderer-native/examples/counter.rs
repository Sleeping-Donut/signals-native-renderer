//! Two-instance counter reference app on the native window and wgpu backend.
//!
//! Each counter owns a local signal; both share a module-level signal. The
//! local double is a memo. Clicks and keyboard focus are handled by the view
//! tree, not by renderer-specific event hooks.

use std::cell::RefCell;
use std::rc::Rc;

use reactive_core::{create_memo, create_signal, ReadSignal, Runtime, RuntimeHandle, WriteSignal};
use renderer_api::{
    DefiniteLength, Edges, KeyEvent, Length, PointerEvent, ResolvedStyle, Size, TextStyle,
};
use renderer_native::{run_windowed, BackendMetrics, FrameOutput, WindowApp};
use renderer_null::{NullRenderer, SharedNullRenderer};
use view::{button, column, text_fn, View, ViewTree};

struct CounterApp {
    runtime: RuntimeHandle,
    tree: ViewTree,
    root: reactive_core::NodeId,
    frames: u64,
}

fn style() -> ResolvedStyle {
    ResolvedStyle {
        text: TextStyle {
            font_size: 20.0,
            line_height: 26.0,
            weight: 500,
            ..TextStyle::default()
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
    stack.background = Some(renderer_api::Color::new(0.16, 0.17, 0.2, 1.0));

    let mut button_style = style();
    button_style.size = Size {
        width: Length::Px(260.0),
        height: Length::Px(40.0),
    };
    button_style.background = Some(renderer_api::Color::new(0.24, 0.28, 0.36, 1.0));

    column(vec![
        button(
            vec![text_fn(move |cx| format!("local {}", local.get(cx)))],
            move |cx| set_local.update_with(cx, |value| value + 1),
        )
        .style(button_style),
        button(
            vec![text_fn(move |cx| format!("shared {}", shared.get(cx)))],
            move |cx| bump.update_with(cx, |value| value + 1),
        )
        .style(button_style),
        text_fn(move |cx| format!("local double {}", double.get(cx))).style(style()),
    ])
    .style(stack)
}

impl CounterApp {
    fn new() -> Self {
        let runtime = Runtime::new().into_handle();
        let renderer = Rc::new(RefCell::new(NullRenderer::new()));
        let tree = ViewTree::new(
            Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
            runtime.clone(),
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
        root_style.background = Some(renderer_api::Color::new(0.08, 0.09, 0.12, 1.0));

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
}

fn main() {
    let app = CounterApp::new();
    if let Err(error) = run_windowed("signals-native counter", 760, 360, app) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
