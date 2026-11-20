//! States and cities: async memos, a loading boundary, and writable derived
//! selection signals on the native window.

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;
use std::time::Duration;

use reactive_core::{
    is_pending, memo, signal, Memo, ReadSignal, Runtime, RuntimeHandle, WriteSignal,
};
use renderer_api::{
    Color, CursorIcon, DefiniteLength, Edges, KeyEvent, Length, PointerEvent, ResolvedStyle, Size,
    TextStyle,
};
use renderer_native::{run_windowed, BackendMetrics, FrameOutput, WindowApp};
use renderer_null::{NullRenderer, SharedNullRenderer};
use view::ViewTree;

mod model {
    use std::future::poll_fn;
    use std::task::Poll;
    use std::time::{Duration, Instant};

    pub const STATES: [&str; 4] = ["California", "Florida", "New York", "Texas"];
    pub const CITIES: [(&str, [&str; 3]); 4] = [
        ("California", ["Los Angeles", "San Francisco", "San Diego"]),
        ("Florida", ["Miami", "Orlando", "Tampa"]),
        ("New York", ["New York City", "Buffalo", "Rochester"]),
        ("Texas", ["Houston", "Dallas", "Austin"]),
    ];
    pub const DELAY_MS: u64 = 500;

    pub fn sleep(millis: u64) -> impl std::future::Future<Output = ()> {
        let deadline = Instant::now() + Duration::from_millis(millis);
        poll_fn(move |_| {
            if Instant::now() >= deadline {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
    }

    pub fn cities_for(state: &str) -> Vec<String> {
        CITIES
            .iter()
            .find(|(name, _)| *name == state)
            .map(|(_, cities)| cities.iter().map(|city| (*city).to_string()).collect())
            .unwrap_or_default()
    }
}

mod styles {
    use super::*;
    use view::StateStyle;

    pub fn text() -> ResolvedStyle {
        ResolvedStyle {
            text: TextStyle {
                color: Color::new(0.92, 0.92, 0.95, 1.0),
                font_size: 18.0,
                line_height: 24.0,
                weight: 400,
            },
            ..ResolvedStyle::default()
        }
    }

    pub fn row_label() -> ResolvedStyle {
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

    pub fn panel() -> ResolvedStyle {
        let mut style = ResolvedStyle {
            size: Size {
                width: Length::Px(260.0),
                height: Length::Auto,
            },
            padding: Edges {
                top: DefiniteLength::Px(10.0),
                right: DefiniteLength::Px(10.0),
                bottom: DefiniteLength::Px(10.0),
                left: DefiniteLength::Px(10.0),
            },
            gap: Size {
                width: DefiniteLength::Px(0.0),
                height: DefiniteLength::Px(6.0),
            },
            background: Some(Color::new(0.16, 0.17, 0.2, 1.0)),
            ..ResolvedStyle::default()
        };
        style.flex.direction = renderer_api::FlexDirection::Column;
        style
    }

    pub fn row() -> ResolvedStyle {
        ResolvedStyle {
            size: Size {
                width: Length::Px(238.0),
                height: Length::Px(28.0),
            },
            text: row_label().text,
            padding: Edges {
                top: DefiniteLength::Px(3.0),
                right: DefiniteLength::Px(8.0),
                bottom: DefiniteLength::Px(3.0),
                left: DefiniteLength::Px(8.0),
            },
            background: Some(Color::new(0.24, 0.28, 0.36, 1.0)),
            border: Edges {
                top: 1.0,
                right: 1.0,
                bottom: 1.0,
                left: 1.0,
            },
            border_color: Some(Color::new(0.36, 0.42, 0.54, 1.0)),
            ..ResolvedStyle::default()
        }
    }

    pub fn root() -> ResolvedStyle {
        let mut style = ResolvedStyle {
            gap: Size {
                width: DefiniteLength::Px(0.0),
                height: DefiniteLength::Px(12.0),
            },
            padding: Edges {
                top: DefiniteLength::Px(16.0),
                right: DefiniteLength::Px(16.0),
                bottom: DefiniteLength::Px(16.0),
                left: DefiniteLength::Px(16.0),
            },
            background: Some(Color::new(0.08, 0.09, 0.12, 1.0)),
            ..ResolvedStyle::default()
        };
        style.flex.direction = renderer_api::FlexDirection::Column;
        style
    }

    pub fn row_container() -> ResolvedStyle {
        ResolvedStyle {
            gap: Size {
                width: DefiniteLength::Px(16.0),
                height: DefiniteLength::Px(0.0),
            },
            ..ResolvedStyle::default()
        }
    }

    pub fn loading_root() -> ResolvedStyle {
        ResolvedStyle {
            padding: Edges {
                top: DefiniteLength::Px(16.0),
                right: DefiniteLength::Px(16.0),
                bottom: DefiniteLength::Px(16.0),
                left: DefiniteLength::Px(16.0),
            },
            ..ResolvedStyle::default()
        }
    }

    pub const HOVER: StateStyle = StateStyle::new().background(Color::new(0.3, 0.36, 0.46, 1.0));
    pub const FOCUS: StateStyle = StateStyle::new()
        .background(Color::new(0.2, 0.42, 0.62, 1.0))
        .border_color(Color::new(0.62, 0.82, 1.0, 1.0));
}

mod signals {
    use super::styles;
    use super::*;
    use macros::view;
    use reactive_core::memo;
    use view::View;

    fn state_row(name: &str, state: ReadSignal<String>, set_state: WriteSignal<String>) -> View {
        let label = name.to_string();
        let label_for_text = label.clone();
        let target = label.clone();
        view! {
            <Button
                style={styles::row()}
                hover_style={styles::HOVER}
                focus_style={styles::FOCUS}
                cursor={CursorIcon::Pointer}
                on_click={move |cx| set_state.set(cx, target.clone())}
            >
                <Text style={styles::row_label()}>
                    {format!(
                        "{}{}",
                        if state.get(cx) == label_for_text { "▶ " } else { "   " },
                        label
                    )}
                </Text>
            </Button>
        }
    }

    fn city_row(
        name: &str,
        city: ReadSignal<String>,
        cities: Memo<Vec<String>>,
        set_city: WriteSignal<String>,
    ) -> View {
        let label = name.to_string();
        let label_for_text = label.clone();
        let target = label.clone();
        view! {
            <Button
                style={styles::row()}
                hover_style={styles::HOVER}
                focus_style={styles::FOCUS}
                cursor={CursorIcon::Pointer}
                on_click={move |cx| set_city.set(cx, target.clone())}
                disabled_when={move |cx| is_pending(cx, move |cx| cities.get(cx))}
            >
                <Text style={styles::row_label()}>
                    {format!(
                        "{}{}",
                        if city.get(cx) == label_for_text { "▶ " } else { "   " },
                        label
                    )}
                </Text>
            </Button>
        }
    }

    pub fn content_view(
        cx: &reactive_core::Cx<'_>,
        states: Memo<Vec<String>>,
        cities: Memo<Vec<String>>,
        state: ReadSignal<String>,
        city: ReadSignal<String>,
        set_state: WriteSignal<String>,
        set_city: WriteSignal<String>,
    ) -> View {
        let selection = memo!(cx, move |cx, _previous: Option<&String>| {
            format!("Selection: {}, {}", city.get(cx), state.get(cx))
        });

        view! {
            <Column style={styles::root()}>
                <Row style={styles::row_container()}>
                    <Column style={styles::panel()}>
                        <Text style={styles::text()}>"States"</Text>
                        <For
                            each={move |cx| states.get(cx)}
                            key={|name: &String| name.clone()}
                            children={move |_cx, name: &String| {
                                state_row(name, state, set_state)
                            }}
                        />
                    </Column>
                    <Column style={styles::panel()}>
                        <Text style={styles::text()}>"Cities"</Text>
                        <For
                            each={move |cx| cities.get(cx)}
                            key={|name: &String| name.clone()}
                            children={move |_cx, name: &String| {
                                city_row(name, city, cities, set_city)
                            }}
                        />
                    </Column>
                </Row>
                <Text style={styles::text()}>{selection.get(cx)}</Text>
            </Column>
        }
    }

    pub fn app_view(
        states: Memo<Vec<String>>,
        cities: Memo<Vec<String>>,
        state: ReadSignal<String>,
        city: ReadSignal<String>,
        set_state: WriteSignal<String>,
        set_city: WriteSignal<String>,
    ) -> View {
        view! {
            <Loading
                ready={move |cx| {
                    let _ = states.get(cx);
                    let _ = cities.get(cx);
                    true
                }}
                fallback={|_cx| loading_view()}
            >
                <Embed expr={content_view(cx, states, cities, state, city, set_state, set_city)} />
            </Loading>
        }
    }

    pub fn loading_view() -> View {
        view! {
            <Column style={styles::loading_root()}>
                <Text style={styles::text()}>"Loading…"</Text>
            </Column>
        }
    }
}

mod app {
    use super::model;
    use super::signals;
    use super::*;
    use renderer_native::{PaintBackend, WgpuBackend};

    pub struct StatesApp {
        runtime: RuntimeHandle,
        tree: ViewTree,
        root: reactive_core::NodeId,
        frames: u64,
    }

    impl StatesApp {
        pub fn new() -> Self {
            let runtime = Runtime::new().into_handle();
            let renderer = Rc::new(RefCell::new(NullRenderer::new()));
            let tree = ViewTree::new_with_shaper(
                Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
                runtime.clone(),
                Box::new(text::CosmicShaper::new()),
            );
            let cx = runtime.cx();

            let states = memo!(async &cx, |_cx, _: Option<&Vec<String>>| {
                Box::pin(async move {
                    model::sleep(model::DELAY_MS).await;
                    Ok(model::STATES
                        .iter()
                        .map(|state| (*state).to_string())
                        .collect())
                })
            });
            let (state, set_state) = signal!(derive & cx, move |cx, _| {
                states.get(cx).first().cloned().unwrap_or_default()
            });
            let cities = memo!(async &cx, move |cx, _: Option<&Vec<String>>| {
                let state_name = state.get(cx);
                Box::pin(async move {
                    model::sleep(model::DELAY_MS).await;
                    Ok(model::cities_for(&state_name))
                })
            });
            let (city, set_city) = signal!(derive & cx, move |cx, _| {
                cities.get(cx).first().cloned().unwrap_or_default()
            });
            let root = tree.mount(
                &cx,
                signals::app_view(states, cities, state, city, set_state, set_city),
            );
            Self {
                runtime,
                tree,
                root,
                frames: 0,
            }
        }
    }

    impl Drop for StatesApp {
        fn drop(&mut self) {
            let cx = self.runtime.cx();
            self.tree.unmount(&cx, self.root);
        }
    }

    impl WindowApp for StatesApp {
        fn resize(&mut self, metrics: BackendMetrics) {
            self.tree
                .set_viewport(metrics.logical_width(), metrics.logical_height());
            self.tree.set_scale(metrics.scale);
        }

        fn frame(&mut self, _metrics: BackendMetrics) -> FrameOutput {
            let cx = self.runtime.cx();
            let _ = cx.flush();
            cx.pump();
            let _ = cx.flush();
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

        fn wants_frame(&self) -> bool {
            self.runtime.cx().pending_tasks() > 0
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
        let mut app = StatesApp::new();
        app.resize(metrics);
        let mut output = app.frame(metrics);
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(16));
            output = app.frame(metrics);
        }
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
    if let Ok(path) = std::env::var("STATES_SNAPSHOT") {
        app::write_snapshot(&path);
        return;
    }
    if let Err(error) = run_windowed("states and cities", 760, 360, app::StatesApp::new()) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
