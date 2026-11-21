//! The states-and-cities demo written without the authoring macros.
//!
//! Same UI and module layout as `states.rs`, so the two files can be read side
//! by side. This one spells out what the macros expand to:
//!
//! | `states.rs` | this file |
//! | --- | --- |
//! | `signal!(cx, value)` | `create_signal(cx, value)` |
//! | `signal!(derive cx, f)` | `create_derived_signal(cx, f)` |
//! | `memo!(cx, f)` | `create_memo(cx, f)` |
//! | `memo!(async cx, f)` | `create_async_memo(cx, f)` |
//! | `#[macros::props]` | a struct with `Prop<T>` / `CallbackWith<T>` fields |
//! | `#[component]` | a function taking `&Cx` plus a props struct |
//! | `view! { <Column style={s}> }` | `view::column(vec![...]).style(s)` |
//! | `<Panel>…</Panel>` | `panel(cx, PanelProps { …, children })` |
//! | `<Loading>` / `<Text>` | `view::control::Loading` / `view::text_fn` |
//!
//! Behaviour is identical to `states.rs`; only the authoring surface differs.

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

use reactive_core::RuntimeHandle;
use renderer_api::{KeyEvent, PointerEvent};
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
    use renderer_api::{
        Color, DefiniteLength, Edges, FlexDirection, Length, ResolvedStyle, Size, TextStyle,
    };

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
        let mut style = text();
        style.text.font_size = 16.0;
        style.text.line_height = 20.0;
        style
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
        style.flex.direction = FlexDirection::Column;
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
        style.flex.direction = FlexDirection::Column;
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
}

mod basecomponents {
    use reactive_core::Cx;
    use view::{column, select, text_fn, CallbackWith, Prop, View};

    use crate::styles;

    /// `#[macros::props]` would turn `title: String` into `Prop<String>`; here
    /// the field type is written out and `children` is a plain `View`.
    pub struct PanelProps {
        pub title: Prop<String>,
        pub children: View,
    }

    /// `#[component]` would return `Panel(&cx, PanelProps::builder()…)`; here
    /// the caller builds the struct directly.
    pub fn panel(cx: &Cx<'_>, props: PanelProps) -> View {
        column(vec![
            text_line(cx, TextLineProps { value: props.title }),
            props.children,
        ])
        .style(styles::panel())
    }

    pub struct TextLineProps {
        pub value: Prop<String>,
    }

    pub fn text_line(_cx: &Cx<'_>, props: TextLineProps) -> View {
        // `view! { <Text>{value.get(cx)}</Text> }` expands to `text_fn`.
        text_fn(move |cx| props.value.get(cx)).style(styles::text())
    }

    pub struct SelectListProps {
        pub items: Prop<Vec<String>>,
        pub selected: Prop<String>,
        pub on_select: CallbackWith<String>,
        pub disabled: Prop<bool>,
    }

    pub fn select_list(_cx: &Cx<'_>, props: SelectListProps) -> View {
        select(
            move |cx| props.items.get(cx),
            |name: &String| name.clone(),
            |name: &String| name.clone(),
            move |cx| props.selected.get(cx),
            move |cx, item| props.on_select.call(cx, item),
            styles::row(),
            move |cx| props.disabled.get(cx),
        )
    }

    pub fn loading_view(_cx: &Cx<'_>) -> View {
        column(vec![text_line(
            _cx,
            TextLineProps {
                value: Prop::static_value(String::from("Loading…")),
            },
        )])
        .style(styles::loading_root())
    }
}

mod uicomponents {
    use reactive_core::{create_async_memo, create_derived_signal, create_memo, is_pending, Cx};
    use view::control::Loading;
    use view::{column, row, CallbackWith, Prop, View};

    use crate::basecomponents::{
        loading_view, panel, select_list, text_line, PanelProps, SelectListProps, TextLineProps,
    };
    use crate::model;
    use crate::styles;

    pub struct ContentProps {
        pub states: Prop<Vec<String>>,
        pub cities: Prop<Vec<String>>,
        pub state: Prop<String>,
        pub city: Prop<String>,
        pub on_state: CallbackWith<String>,
        pub on_city: CallbackWith<String>,
    }

    pub fn content(cx: &Cx<'_>, props: ContentProps) -> View {
        let state_for_display = props.state.clone();
        let city_for_display = props.city.clone();
        let cities_for_pending = props.cities.clone();
        // `memo!(cx, move |cx, _| …)` expands to `create_memo`.
        let selection = create_memo(cx, move |cx, _: Option<&String>| {
            format!(
                "Selection: {}, {}",
                city_for_display.get(cx),
                state_for_display.get(cx)
            )
        });
        // `Prop::derived(cx, …)` in `states.rs` is this memo plus `.into()`.
        let cities_disabled = create_memo(cx, move |cx, _: Option<&bool>| {
            let cities = cities_for_pending.clone();
            is_pending(cx, move |cx| cities.get(cx))
        });

        column(vec![
            row(vec![
                panel(
                    cx,
                    PanelProps {
                        title: Prop::static_value(String::from("States")),
                        children: select_list(
                            cx,
                            SelectListProps {
                                items: props.states,
                                selected: props.state,
                                on_select: props.on_state,
                                disabled: Prop::static_value(false),
                            },
                        ),
                    },
                ),
                panel(
                    cx,
                    PanelProps {
                        title: Prop::static_value(String::from("Cities")),
                        children: select_list(
                            cx,
                            SelectListProps {
                                items: props.cities,
                                selected: props.city,
                                on_select: props.on_city,
                                disabled: cities_disabled.into(),
                            },
                        ),
                    },
                ),
            ])
            .style(styles::row_container()),
            text_line(
                cx,
                TextLineProps {
                    value: selection.into(),
                },
            ),
        ])
        .style(styles::root())
    }

    pub fn app_root(cx: &Cx<'_>) -> View {
        // `memo!(async cx, …)` expands to `create_async_memo`.
        let states = create_async_memo(cx, |_cx, _: Option<&Vec<String>>| {
            Box::pin(async {
                model::sleep(model::DELAY_MS).await;
                Ok(model::STATES
                    .iter()
                    .map(|state| (*state).to_string())
                    .collect())
            })
        });
        // `signal!(derive cx, …)` expands to `create_derived_signal`.
        let (state, set_state) = create_derived_signal(cx, move |cx, _| {
            states.get(cx).first().cloned().unwrap_or_default()
        });
        let cities = create_async_memo(cx, move |cx, _: Option<&Vec<String>>| {
            let state_name = state.get(cx);
            Box::pin(async move {
                model::sleep(model::DELAY_MS).await;
                Ok(model::cities_for(&state_name))
            })
        });
        let (city, set_city) = create_derived_signal(cx, move |cx, _| {
            cities.get(cx).first().cloned().unwrap_or_default()
        });

        // The `<Loading>` tag expands to `view::control::Loading`.
        Loading(loading_view, move |cx| {
            content(
                cx,
                ContentProps {
                    states: states.into(),
                    cities: cities.into(),
                    state: state.into(),
                    city: city.into(),
                    on_state: CallbackWith::set(set_state),
                    on_city: CallbackWith::set(set_city),
                },
            )
        })
    }
}

mod app {
    use super::*;

    pub struct StatesApp {
        runtime: RuntimeHandle,
        tree: ViewTree,
        root: reactive_core::NodeId,
        frames: u64,
    }

    impl StatesApp {
        pub fn new() -> Self {
            let runtime = reactive_core::Runtime::new().into_handle();
            let renderer = Rc::new(RefCell::new(NullRenderer::new()));
            let tree = ViewTree::new_with_shaper(
                Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
                runtime.clone(),
                Box::new(text::CosmicShaper::new()),
            );
            let cx = runtime.cx();
            let root = tree.mount(&cx, crate::uicomponents::app_root(&cx));
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

        fn cursor_icon(&self) -> renderer_api::CursorIcon {
            self.tree.cursor().unwrap_or_default()
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
        use renderer_native::PaintBackend;
        let metrics = BackendMetrics::new(1520, 720, 2.0);
        let mut app = StatesApp::new();
        app.resize(metrics);
        let mut output = app.frame(metrics);
        for _ in 0..100 {
            std::thread::sleep(std::time::Duration::from_millis(16));
            output = app.frame(metrics);
        }
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
    if let Ok(path) = std::env::var("STATES_MANUAL_SNAPSHOT") {
        app::write_snapshot(&path);
        return;
    }
    if let Err(error) = run_windowed(
        "states and cities (manual)",
        760,
        360,
        app::StatesApp::new(),
    ) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
