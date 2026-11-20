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

use reactive_core::{ReadSignal, Runtime, RuntimeHandle, WriteSignal};
use renderer_api::{
    Color, CursorIcon, DefiniteLength, Edges, KeyEvent, Length, PointerEvent, ResolvedStyle, Size,
    TextStyle,
};
use renderer_native::{run_windowed, BackendMetrics, FrameOutput, WindowApp};
use renderer_null::{NullRenderer, SharedNullRenderer};
use view::ViewTree;

mod signals {
    use super::styles;
    use super::*;
    use macros::view;
    use reactive_core::{memo, signal};
    use view::View;

    pub fn counter(
        cx: &reactive_core::Cx<'_>,
        shared: ReadSignal<u32>,
        bump: WriteSignal<u32>,
    ) -> View {
        let (local, set_local) = signal!(cx, 0_u32);
        let double = memo!(cx, move |cx, _| local.get(cx) * 2);

        view! {
            <Column style={styles::stack()}>
                <Button
                    style={styles::button()}
                    hover_style={styles::HOVER}
                    focus_style={styles::FOCUS}
                    cursor={CursorIcon::Pointer}
                    on_click={move |cx| set_local.update_with(cx, |value| value + 1)}
                >
                    <Text style={styles::button_label()}>{format!("local {}", local.get(cx))}</Text>
                </Button>
                <Button
                    style={styles::button()}
                    hover_style={styles::HOVER}
                    focus_style={styles::FOCUS}
                    cursor={CursorIcon::Pointer}
                    on_click={move |cx| bump.update_with(cx, |value| value + 1)}
                >
                    <Text style={styles::button_label()}>{format!("shared {}", shared.get(cx))}</Text>
                </Button>
                <Text style={styles::base()}>{format!("local double {}", double.get(cx))}</Text>
            </Column>
        }
    }

    pub fn root(
        cx: &reactive_core::Cx<'_>,
        shared: ReadSignal<u32>,
        bump: WriteSignal<u32>,
    ) -> View {
        view! {
            <Row style={styles::root()}>
                <Embed expr={counter(cx, shared, bump)} />
                <Embed expr={counter(cx, shared, bump)} />
            </Row>
        }
    }
}

mod styles {
    use super::*;
    use view::StateStyle;

    pub fn text() -> TextStyle {
        TextStyle {
            color: Color::new(0.92, 0.92, 0.95, 1.0),
            font_size: 20.0,
            line_height: 26.0,
            weight: 400,
        }
    }

    pub fn button_label() -> ResolvedStyle {
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

    pub fn base() -> ResolvedStyle {
        ResolvedStyle {
            text: text(),
            ..ResolvedStyle::default()
        }
    }

    pub fn stack() -> ResolvedStyle {
        let mut style = base();
        style.flex.direction = renderer_api::FlexDirection::Column;
        style.gap = Size {
            width: DefiniteLength::Px(0.0),
            height: DefiniteLength::Px(8.0),
        };
        style.padding = Edges {
            top: DefiniteLength::Px(12.0),
            right: DefiniteLength::Px(12.0),
            bottom: DefiniteLength::Px(12.0),
            left: DefiniteLength::Px(12.0),
        };
        style.background = Some(Color::new(0.16, 0.17, 0.2, 1.0));
        style
    }

    pub fn root() -> ResolvedStyle {
        let mut style = base();
        style.flex.direction = renderer_api::FlexDirection::Row;
        style.gap = Size {
            width: DefiniteLength::Px(16.0),
            height: DefiniteLength::Px(0.0),
        };
        style.padding = Edges {
            top: DefiniteLength::Px(16.0),
            right: DefiniteLength::Px(16.0),
            bottom: DefiniteLength::Px(16.0),
            left: DefiniteLength::Px(16.0),
        };
        style.background = Some(Color::new(0.08, 0.09, 0.12, 1.0));
        style
    }

    pub fn button() -> ResolvedStyle {
        let mut style = base();
        style.size = Size {
            width: Length::Px(260.0),
            height: Length::Px(40.0),
        };
        style.padding = Edges {
            top: DefiniteLength::Px(8.0),
            right: DefiniteLength::Px(10.0),
            bottom: DefiniteLength::Px(8.0),
            left: DefiniteLength::Px(10.0),
        };
        style.background = Some(Color::new(0.24, 0.28, 0.36, 1.0));
        style.border = Edges {
            top: 1.0,
            right: 1.0,
            bottom: 1.0,
            left: 1.0,
        };
        style.border_color = Some(Color::new(0.36, 0.42, 0.54, 1.0));
        style
    }

    pub const HOVER: StateStyle = StateStyle::new().background(Color::new(0.3, 0.36, 0.46, 1.0));
    pub const FOCUS: StateStyle = StateStyle::new()
        .background(Color::new(0.2, 0.42, 0.62, 1.0))
        .border_color(Color::new(0.62, 0.82, 1.0, 1.0));
}

mod app {
    use super::*;
    use reactive_core::create_signal;
    use renderer_native::{PaintBackend, WgpuBackend};

    pub struct CounterApp {
        runtime: RuntimeHandle,
        tree: ViewTree,
        root: reactive_core::NodeId,
        frames: u64,
    }

    impl CounterApp {
        pub fn new() -> Self {
            let runtime = Runtime::new().into_handle();
            let renderer = Rc::new(RefCell::new(NullRenderer::new()));
            let tree = ViewTree::new_with_shaper(
                Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
                runtime.clone(),
                Box::new(text::CosmicShaper::new()),
            );
            let cx = runtime.cx();
            let (shared, bump) = create_signal(&cx, 0_u32);
            let root = tree.mount(&cx, crate::signals::root(&cx, shared, bump));
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

        fn cursor_icon(&self) -> CursorIcon {
            self.tree.cursor().unwrap_or(CursorIcon::Default)
        }

        fn accesskit_update(&self) -> Option<accesskit::TreeUpdate> {
            self.tree.accesskit_tree()
        }

        fn accesskit_action(&mut self, request: &accesskit::ActionRequest) -> bool {
            self.tree.dispatch_accesskit_action(request)
        }
    }

    pub fn write_snapshot(path: &str) {
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
}

fn main() {
    if let Ok(path) = std::env::var("COUNTER_SNAPSHOT") {
        app::write_snapshot(&path);
        return;
    }
    if let Err(error) = run_windowed("signals-native counter", 760, 360, app::CounterApp::new()) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
