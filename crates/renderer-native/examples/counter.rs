//! Ten-instance counter reference app on the native window and wgpu backend.
//!
//! Module layout:
//! - `app`: window lifecycle and `main`
//! - `uicomponents`: `App` and `Counter`
//! - `basecomponents`: reusable `ActionButton` and `TextLine`
//! - `styles`: shared style builders
//!
//! Set `COUNTER_SNAPSHOT=/path/to/frame.rgba` to render one offscreen frame at
//! 1520x720 (2x) without opening a window.

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

use reactive_core::RuntimeHandle;
use renderer_api::{KeyEvent, PointerEvent};
use renderer_native::{run_windowed, BackendMetrics, FrameOutput, WindowApp};
use renderer_null::{NullRenderer, SharedNullRenderer};
use view::ViewTree;

mod styles {
    use renderer_api::{
        Color, DefiniteLength, Edges, FlexDirection, FlexWrap, Length, ResolvedStyle, Size,
        TextStyle,
    };
    use view::StateStyle;

    pub fn text() -> TextStyle {
        TextStyle {
            color: Color::new(0.92, 0.92, 0.95, 1.0),
            font_size: 16.0,
            line_height: 20.0,
            weight: 400,
        }
    }

    pub fn base() -> ResolvedStyle {
        ResolvedStyle {
            text: text(),
            ..ResolvedStyle::default()
        }
    }

    pub fn counter() -> ResolvedStyle {
        let mut style = base();
        style.flex.direction = FlexDirection::Row;
        style.gap = Size {
            width: DefiniteLength::Px(8.0),
            height: DefiniteLength::Px(0.0),
        };
        style.padding = Edges {
            top: DefiniteLength::Px(8.0),
            right: DefiniteLength::Px(10.0),
            bottom: DefiniteLength::Px(8.0),
            left: DefiniteLength::Px(10.0),
        };
        style.background = Some(Color::new(0.16, 0.17, 0.2, 1.0));
        style
    }

    pub fn page() -> ResolvedStyle {
        let mut style = base();
        style.flex.direction = FlexDirection::Column;
        style.padding = Edges {
            top: DefiniteLength::Px(16.0),
            right: DefiniteLength::Px(16.0),
            bottom: DefiniteLength::Px(16.0),
            left: DefiniteLength::Px(16.0),
        };
        style.background = Some(Color::new(0.08, 0.09, 0.12, 1.0));
        style
    }

    /// The control-flow view is logical, so the counters wrap in this row.
    pub fn grid() -> ResolvedStyle {
        let mut style = base();
        style.flex.direction = FlexDirection::Row;
        style.flex.wrap = FlexWrap::Wrap;
        style.gap = Size {
            width: DefiniteLength::Px(16.0),
            height: DefiniteLength::Px(8.0),
        };
        style
    }

    pub fn button() -> ResolvedStyle {
        let mut style = base();
        style.size = Size {
            width: Length::Px(100.0),
            height: Length::Px(32.0),
        };
        style.padding = Edges {
            top: DefiniteLength::Px(4.0),
            right: DefiniteLength::Px(8.0),
            bottom: DefiniteLength::Px(4.0),
            left: DefiniteLength::Px(8.0),
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

    pub fn button_label() -> ResolvedStyle {
        let mut style = base();
        style.text.font_size = 14.0;
        style.text.line_height = 18.0;
        style
    }

    pub const HOVER: StateStyle = StateStyle::new().background(Color::new(0.3, 0.36, 0.46, 1.0));
    pub const FOCUS: StateStyle = StateStyle::new()
        .background(Color::new(0.2, 0.42, 0.62, 1.0))
        .border_color(Color::new(0.62, 0.82, 1.0, 1.0));
}

mod basecomponents {
    use macros::{component, view};
    use reactive_core::Cx;
    use renderer_api::CursorIcon;
    use view::{Callback, View};

    use crate::styles;

    #[macros::props]
    pub struct ActionButtonProps {
        pub label: String,
        pub on_click: Callback,
    }

    #[component]
    pub fn ActionButton(_cx: &Cx<'_>, props: ActionButtonProps) -> View {
        let label = props.label;
        let on_click = props.on_click;
        view! {
            <Button
                style={styles::button()}
                hover_style={styles::HOVER}
                focus_style={styles::FOCUS}
                cursor={CursorIcon::Pointer}
                on_click={move |cx: &Cx<'_>| on_click.call(cx)}
            >
                <Text style={styles::button_label()}>{label.get(cx)}</Text>
            </Button>
        }
    }

    #[macros::props]
    pub struct TextLineProps {
        pub value: String,
    }

    #[component]
    pub fn TextLine(_cx: &Cx<'_>, props: TextLineProps) -> View {
        let value = props.value;
        view! { <Text style={styles::base()}>{value.get(cx)}</Text> }
    }
}

mod uicomponents {
    use macros::{component, view};
    use reactive_core::{memo, signal, Cx};
    use view::control::Repeat;
    use view::{CallbackWith, Prop, View};

    use crate::basecomponents::{ActionButton, ActionButtonProps, TextLine, TextLineProps};
    use crate::styles;

    #[macros::props]
    pub struct CounterProps {
        pub shared: u32,
        pub increment: CallbackWith<u32>,
    }

    #[component]
    pub fn Counter(cx: &Cx<'_>, props: CounterProps) -> View {
        let (local, set_local) = signal!(cx, 0_u32);
        let double = memo!(cx, move |cx, _| local.get(cx) * 2);

        view! {
            <Row style={styles::counter()}>
                <ActionButton
                    label={Prop::derived(cx, move |cx| {
                        format!("local {}", local.get(cx))
                    })}
                    on_click={move |cx: &Cx<'_>| {
                        set_local.update_with(cx, |value| value + 1)
                    }}
                />
                <ActionButton
                    label={Prop::derived(cx, move |cx| {
                        format!("shared {}", props.shared.get(cx))
                    })}
                    on_click={move |cx: &Cx<'_>| {
                        props.increment.call(cx, 1)
                    }}
                />
                <TextLine value={Prop::derived(cx, move |cx| {
                    format!("local double {}", double.get(cx))
                })} />
            </Row>
        }
    }

    #[component]
    pub fn App(cx: &Cx<'_>) -> View {
        let (shared, bump) = signal!(cx, 0_u32);
        let increment: CallbackWith<u32> =
            (move |cx: &Cx<'_>, amount: u32| bump.update_with(cx, |value| value + amount)).into();

        view! {
            <Column style={styles::page()}>
                <Row style={styles::grid()}>
                    <Repeat count={10}>
                        <Counter shared={shared} increment={increment.clone()} />
                    </Repeat>
                </Row>
            </Column>
        }
    }
}

mod app {
    use super::*;

    pub struct CounterApp {
        pub runtime: RuntimeHandle,
        pub tree: ViewTree,
        pub root: reactive_core::NodeId,
        pub frames: u64,
    }

    impl CounterApp {
        pub fn new() -> Self {
            let runtime = reactive_core::Runtime::new().into_handle();
            let renderer = Rc::new(RefCell::new(NullRenderer::new()));
            let tree = ViewTree::new_with_shaper(
                Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
                runtime.clone(),
                Box::new(text::CosmicShaper::new()),
            );
            let cx = runtime.cx();
            let root = tree.mount(&cx, crate::uicomponents::App(&cx));
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

        fn cursor_icon(&self) -> renderer_api::CursorIcon {
            self.tree.cursor().unwrap_or_default()
        }

        fn accesskit_update(&self) -> Option<accesskit::TreeUpdate> {
            self.tree.accesskit_tree()
        }

        fn accesskit_action(&mut self, request: &accesskit::ActionRequest) -> bool {
            self.tree.dispatch_accesskit_action(request)
        }
    }

    pub fn write_snapshot(path: &str) {
        use renderer_native::PaintBackend;
        let metrics = BackendMetrics::new(1520, 720, 2.0);
        let mut app = CounterApp::new();
        app.resize(metrics);
        let output = app.frame(metrics);
        let mut backend =
            renderer_native::WgpuBackend::new_headless(1520, 720, 2.0).expect("headless wgpu");
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
