//! Store-backed counter. No manual notify: field writes mark their own paths.

use std::cell::RefCell;
use std::rc::Rc;

use reactive_core::{Cx, Runtime, RuntimeHandle};
use renderer_api::{
    Color, CursorIcon, DefiniteLength, Edges, KeyEvent, Length, PointerEvent, ResolvedStyle, Size,
    TextStyle,
};
use renderer_native::{run_windowed, BackendMetrics, FrameOutput, WindowApp};
use renderer_null::{NullRenderer, SharedNullRenderer};
use view::{View, ViewTree};

mod signals {
    use super::*;
    use macros::view;
    use reactive_core::memo;
    use reactive_stores::store;

    #[derive(Clone, Debug, PartialEq, reactive_stores::Store)]
    pub struct CounterState {
        pub count: u32,
    }

    pub fn view(cx: &Cx<'_>) -> View {
        let store = store!(cx, CounterState { count: 0 });
        let store_for_double = store.clone();
        let double = memo!(cx, move |cx, _| store_for_double.count().get(cx) * 2);
        let count_for_text = store.clone();
        let count_for_button = store;
        view! {
            <Column style={styles::root()}>
                <Button
                    style={styles::button()}
                    hover_style={styles::HOVER}
                    focus_style={styles::FOCUS}
                    cursor={CursorIcon::Pointer}
                    on_click={move |cx| count_for_button.count().update(cx, |count| *count += 1)}
                >
                    <Text style={styles::label()}>
                        {format!("count {}", count_for_text.count().get(cx))}
                    </Text>
                </Button>
                <Text style={styles::label()}>{format!("double {}", double.get(cx))}</Text>
            </Column>
        }
    }
}

mod styles {
    use super::*;
    use view::StateStyle;

    pub fn label() -> ResolvedStyle {
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

    pub fn root() -> ResolvedStyle {
        let mut style = label();
        style.flex.direction = renderer_api::FlexDirection::Column;
        style.gap = Size {
            width: DefiniteLength::Px(0.0),
            height: DefiniteLength::Px(10.0),
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
        let mut style = label();
        style.size = Size {
            width: Length::Px(220.0),
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

struct StoreCounterApp {
    runtime: RuntimeHandle,
    tree: ViewTree,
    root: reactive_core::NodeId,
    frames: u64,
}

impl StoreCounterApp {
    fn new() -> Self {
        let runtime = Runtime::new().into_handle();
        let renderer = Rc::new(RefCell::new(NullRenderer::new()));
        let tree = ViewTree::new_with_shaper(
            Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
            runtime.clone(),
            Box::new(text::CosmicShaper::new()),
        );
        let cx = runtime.cx();
        let root = tree.mount(&cx, signals::view(&cx));
        Self {
            runtime,
            tree,
            root,
            frames: 0,
        }
    }
}

impl Drop for StoreCounterApp {
    fn drop(&mut self) {
        let cx = self.runtime.cx();
        self.tree.unmount(&cx, self.root);
    }
}

impl WindowApp for StoreCounterApp {
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

fn main() {
    if let Err(error) = run_windowed("store counter", 420, 220, StoreCounterApp::new()) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
