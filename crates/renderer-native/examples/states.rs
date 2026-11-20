//! States and cities: async memos, a loading boundary, and writable derived
//! selection signals on the native window.

use std::cell::RefCell;
use std::future::poll_fn;
use std::io::Write;
use std::rc::Rc;
use std::task::Poll;
use std::time::{Duration, Instant};

use reactive_core::{
    create_async_memo, create_derived_signal, loading_boundary, Memo, ReadSignal, Runtime,
    RuntimeHandle, WriteSignal,
};
use renderer_api::{
    Color, CursorIcon, DefiniteLength, Edges, KeyEvent, Length, PointerEvent, ResolvedStyle, Size,
    TextStyle,
};
use renderer_native::{
    run_windowed, BackendMetrics, FrameOutput, PaintBackend, WgpuBackend, WindowApp,
};
use renderer_null::{NullRenderer, SharedNullRenderer};
use text::CosmicShaper;
use view::{button, column, keyed_list, row, text, text_fn, view_slot, StateStyle, View, ViewTree};

const STATES: [&str; 4] = ["California", "Florida", "New York", "Texas"];
const CITIES: [(&str, [&str; 3]); 4] = [
    ("California", ["Los Angeles", "San Francisco", "San Diego"]),
    ("Florida", ["Miami", "Orlando", "Tampa"]),
    ("New York", ["New York City", "Buffalo", "Rochester"]),
    ("Texas", ["Houston", "Dallas", "Austin"]),
];
const DELAY_MS: u64 = 500;

struct StatesApp {
    runtime: RuntimeHandle,
    tree: ViewTree,
    root: reactive_core::NodeId,
    states: Memo<Vec<String>>,
    cities: Memo<Vec<String>>,
    frames: u64,
}

fn sleep(millis: u64) -> impl std::future::Future<Output = ()> {
    let deadline = Instant::now() + Duration::from_millis(millis);
    poll_fn(move |_| {
        if Instant::now() >= deadline {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
}

fn cities_for(state: &str) -> Vec<String> {
    CITIES
        .iter()
        .find(|(name, _)| *name == state)
        .map(|(_, cities)| cities.iter().map(|city| (*city).to_string()).collect())
        .unwrap_or_default()
}

fn panel_style() -> ResolvedStyle {
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

fn text_style() -> ResolvedStyle {
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

fn row_style() -> ResolvedStyle {
    ResolvedStyle {
        size: Size {
            width: Length::Px(238.0),
            height: Length::Px(28.0),
        },
        text: TextStyle {
            color: Color::new(0.92, 0.92, 0.95, 1.0),
            font_size: 16.0,
            line_height: 20.0,
            weight: 400,
        },
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

const HOVER: StateStyle = StateStyle::new().background(Color::new(0.3, 0.36, 0.46, 1.0));
const FOCUS: StateStyle = StateStyle::new()
    .background(Color::new(0.2, 0.42, 0.62, 1.0))
    .border_color(Color::new(0.62, 0.82, 1.0, 1.0));

fn loading_view() -> View {
    column(vec![text("Loading…")]).style(ResolvedStyle {
        padding: Edges {
            top: DefiniteLength::Px(16.0),
            right: DefiniteLength::Px(16.0),
            bottom: DefiniteLength::Px(16.0),
            left: DefiniteLength::Px(16.0),
        },
        ..ResolvedStyle::default()
    })
}

fn content_view(
    states: Memo<Vec<String>>,
    cities: Memo<Vec<String>>,
    state: ReadSignal<String>,
    city: ReadSignal<String>,
    set_state: WriteSignal<String>,
    set_city: WriteSignal<String>,
) -> View {
    let state_rows = keyed_list(
        move |cx| states.get(cx),
        |name| name.clone(),
        move |_cx, name| {
            let label = name.clone();
            let target = name.clone();
            button(
                vec![text_fn(move |cx| {
                    let marker = if state.get(cx) == label {
                        "▶ "
                    } else {
                        "   "
                    };
                    format!("{marker}{label}")
                })],
                move |cx| set_state.set(cx, target.clone()),
            )
            .style(row_style())
            .hover_style(HOVER)
            .focus_style(FOCUS)
            .cursor(CursorIcon::Pointer)
        },
    );

    let city_rows = keyed_list(
        move |cx| cities.get(cx),
        |name| name.clone(),
        move |_cx, name| {
            let label = name.clone();
            let target = name.clone();
            button(
                vec![text_fn(move |cx| {
                    let marker = if city.get(cx) == label { "▶ " } else { "   " };
                    format!("{marker}{label}")
                })],
                move |cx| set_city.set(cx, target.clone()),
            )
            .style(row_style())
            .hover_style(HOVER)
            .focus_style(FOCUS)
            .cursor(CursorIcon::Pointer)
            .disabled_when(move |cx| cities.is_pending(cx))
        },
    );

    let mut root = ResolvedStyle {
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
    root.flex.direction = renderer_api::FlexDirection::Column;

    column(vec![
        row(vec![
            column(vec![text("States").style(text_style()), state_rows]).style(panel_style()),
            column(vec![text("Cities").style(text_style()), city_rows]).style(panel_style()),
        ])
        .style(ResolvedStyle {
            gap: Size {
                width: DefiniteLength::Px(16.0),
                height: DefiniteLength::Px(0.0),
            },
            ..ResolvedStyle::default()
        }),
        text_fn(move |cx| format!("Selection: {}, {}", city.get(cx), state.get(cx)))
            .style(text_style()),
    ])
    .style(root)
}

impl StatesApp {
    fn new() -> Self {
        let runtime = Runtime::new().into_handle();
        let renderer = Rc::new(RefCell::new(NullRenderer::new()));
        let tree = ViewTree::new_with_shaper(
            Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
            runtime.clone(),
            Box::new(CosmicShaper::new()),
        );
        let cx = runtime.cx();

        let states = create_async_memo(&cx, |_cx, _: Option<&Vec<String>>| {
            Box::pin(async move {
                sleep(DELAY_MS).await;
                Ok(STATES.iter().map(|state| (*state).to_string()).collect())
            })
        });
        let (state, set_state) = create_derived_signal(&cx, move |cx, _| {
            states.get(cx).first().cloned().unwrap_or_default()
        });
        let cities = create_async_memo(&cx, move |cx, _: Option<&Vec<String>>| {
            let state_name = state.get(cx);
            Box::pin(async move {
                sleep(DELAY_MS).await;
                Ok(cities_for(&state_name))
            })
        });
        let (city, set_city) = create_derived_signal(&cx, move |cx, _| {
            cities.get(cx).first().cloned().unwrap_or_default()
        });
        let ready = loading_boundary(
            &cx,
            None,
            move |cx| {
                let _ = states.get(cx);
                let _ = cities.get(cx);
                true
            },
            |_| false,
        );

        let app = view_slot(
            move |cx| ready.get(cx),
            move |cx| {
                if ready.get(cx) {
                    content_view(states, cities, state, city, set_state, set_city)
                } else {
                    loading_view()
                }
            },
        );
        let root = tree.mount(&cx, app);
        Self {
            runtime,
            tree,
            root,
            states,
            cities,
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
        let cx = self.runtime.cx();
        self.states.is_pending(&cx) || self.cities.is_pending(&cx)
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

fn main() {
    if let Ok(path) = std::env::var("STATES_SNAPSHOT") {
        write_snapshot(&path);
        return;
    }
    if let Err(error) = run_windowed("states and cities", 760, 360, StatesApp::new()) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
