//! Optimistic todos: the expected change shows at once, the save runs in the
//! background, and a rejected save is discarded without rollback code.
//!
//! Module layout:
//! - `model`: todo data and a pretend server with latency and a failure switch
//! - `store`: optimistic store derived from `api.list()` (per-row guesses and add patches)
//! - `styles`
//! - `basecomponents`: `ActionButton`, `IconButton`, `TextLine`, `ErrorLine`
//! - `uicomponents`: `App`, `Content`, `TodoRowView`, `LoadingView`
//! - `app`: window lifecycle and `main`
//!
//! Set `TODOS_SNAPSHOT=/path/to/frame.rgba` to render one offscreen frame at
//! 1520x720 (2x) without opening a window.

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

use reactive_core::RuntimeHandle;
use renderer_api::{KeyEvent, PointerEvent};
use renderer_native::{run_windowed, BackendMetrics, FrameOutput, WindowApp};
use renderer_null::{NullRenderer, SharedNullRenderer};
use view::ViewTree;

mod model {
    use std::cell::{Cell, RefCell};
    use std::future::poll_fn;
    use std::task::Poll;
    use std::time::{Duration, Instant};

    pub const LOAD_DELAY_MS: u64 = 200;
    pub const SAVE_DELAY_MS: u64 = 1000;

    #[derive(Clone, Debug, PartialEq)]
    pub struct Todo {
        pub id: u64,
        pub text: String,
        pub done: bool,
    }

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

    /// The pretend server. Saves take a second, and one can be made to fail.
    #[derive(Default)]
    pub struct Api {
        saved: RefCell<Vec<Todo>>,
        fail_next: Cell<bool>,
        next_id: Cell<u64>,
    }

    impl Api {
        pub fn new() -> Self {
            Self {
                saved: RefCell::new(vec![
                    Todo {
                        id: 1,
                        text: String::from("Read the docs"),
                        done: true,
                    },
                    Todo {
                        id: 2,
                        text: String::from("Try optimistic actions"),
                        done: false,
                    },
                ]),
                fail_next: Cell::new(false),
                next_id: Cell::new(3),
            }
        }

        pub fn next_id(&self) -> u64 {
            let id = self.next_id.get();
            self.next_id.set(id + 1);
            id
        }

        pub fn set_fail_next(&self, fail: bool) {
            self.fail_next.set(fail);
        }

        fn take_failure(&self) -> Option<String> {
            self.fail_next
                .replace(false)
                .then(|| String::from("The server rejected that change."))
        }

        pub async fn list(&self) -> Vec<Todo> {
            sleep(LOAD_DELAY_MS).await;
            self.saved.borrow().clone()
        }

        pub async fn toggle(&self, id: u64) -> Result<(), String> {
            sleep(SAVE_DELAY_MS).await;
            if let Some(error) = self.take_failure() {
                return Err(error);
            }
            if let Some(todo) = self
                .saved
                .borrow_mut()
                .iter_mut()
                .find(|todo| todo.id == id)
            {
                todo.done = !todo.done;
            }
            Ok(())
        }

        pub async fn add(&self, todo: Todo) -> Result<(), String> {
            sleep(SAVE_DELAY_MS).await;
            if let Some(error) = self.take_failure() {
                return Err(error);
            }
            self.saved.borrow_mut().push(todo);
            Ok(())
        }
    }
}

mod store {
    use std::rc::Rc;

    use reactive_core::{create_signal, refresh, Cx, ReadSignal, RuntimeHandle, WriteSignal};
    use reactive_stores::{OptimisticList, OptimisticStore};

    use crate::model::{Api, Todo};

    /// A row as the user sees it: derived data plus an in-flight guess.
    #[derive(Clone, Debug, PartialEq)]
    pub struct TodoRow {
        pub id: u64,
        pub text: String,
        pub done: bool,
        pub pending: bool,
    }

    fn todo_row(todo: Todo) -> TodoRow {
        TodoRow {
            id: todo.id,
            text: todo.text,
            done: todo.done,
            pending: false,
        }
    }

    /// Optimistic rows derived from the API. The store's truth is `api.list()`;
    /// writes are per-key guesses and list patches, and `refresh` re-asks the
    /// source after a save.
    pub struct TodosStore {
        runtime: RuntimeHandle,
        api: Rc<Api>,
        todos: OptimisticStore<Vec<TodoRow>>,
        error: ReadSignal<String>,
        set_error: WriteSignal<String>,
    }

    impl TodosStore {
        pub fn new(cx: &Cx<'_>, runtime: RuntimeHandle, api: Rc<Api>) -> Rc<Self> {
            let api_for_load = Rc::clone(&api);
            let todos = OptimisticStore::new(cx, Vec::new(), move || {
                let api = Rc::clone(&api_for_load);
                async move {
                    let list = api.list().await;
                    Ok::<Vec<TodoRow>, reactive_core::AsyncError>(
                        list.into_iter().map(todo_row).collect(),
                    )
                }
            });
            let (error, set_error) = create_signal(cx, String::new());

            Rc::new(Self {
                runtime,
                api,
                todos,
                error,
                set_error,
            })
        }

        pub fn rows(&self) -> OptimisticList<TodoRow, u64> {
            self.todos.list(|row| row.id)
        }

        pub fn error(&self) -> ReadSignal<String> {
            self.error
        }

        pub fn set_fail_next(self: &Rc<Self>, _cx: &Cx<'_>, fail: bool) {
            self.api.set_fail_next(fail);
        }

        pub fn toggle(self: &Rc<Self>, cx: &Cx<'_>, id: u64) {
            if !self
                .rows()
                .get(cx)
                .iter()
                .any(|row| row.id == id && !row.pending)
            {
                return;
            }
            let Some(row) = self.rows().at_key(cx, &id) else {
                return;
            };
            let api = Rc::clone(&self.api);
            let todos = self.todos.clone();
            let set_error = self.set_error;
            let runtime_for_action = self.runtime.clone();

            let _ = self.runtime.action_with(
                move || {
                    let rt = runtime_for_action.clone();
                    async move {
                        row.update(&rt.cx(), |row| {
                            row.done = !row.done;
                            row.pending = true;
                        });

                        api.toggle(id).await?;
                        refresh(&rt.cx(), |cx| {
                            let _ = todos.get(cx);
                        });
                        Ok::<(), String>(())
                    }
                },
                move |cx, result| match result {
                    Ok(()) => set_error.set(cx, String::new()),
                    Err(message) => set_error.set(cx, message.clone()),
                },
            );
        }

        pub fn add(self: &Rc<Self>, _cx: &Cx<'_>, text: String) {
            let api = Rc::clone(&self.api);
            let rows = self.rows();
            let todos = self.todos.clone();
            let set_error = self.set_error;
            let id = self.api.next_id();
            let runtime_for_action = self.runtime.clone();

            let _ = self.runtime.action_with(
                move || {
                    let rt = runtime_for_action.clone();
                    async move {
                        rows.push(
                            &rt.cx(),
                            TodoRow {
                                id,
                                text: text.clone(),
                                done: false,
                                pending: true,
                            },
                        );

                        api.add(Todo {
                            id,
                            text,
                            done: false,
                        })
                        .await?;

                        refresh(&rt.cx(), |cx| {
                            let _ = todos.get(cx);
                        });
                        Ok::<(), String>(())
                    }
                },
                move |cx, result| match result {
                    Ok(()) => set_error.set(cx, String::new()),
                    Err(message) => set_error.set(cx, message.clone()),
                },
            );
        }
    }
}

mod styles {
    use renderer_api::{
        Color, DefiniteLength, Edges, FlexDirection, Length, ResolvedStyle, Size, TextStyle,
    };
    use view::StateStyle;

    pub fn text() -> TextStyle {
        TextStyle {
            color: Color::new(0.92, 0.92, 0.95, 1.0),
            font_size: 18.0,
            line_height: 24.0,
            weight: 400,
        }
    }

    pub fn base() -> ResolvedStyle {
        ResolvedStyle {
            text: text(),
            ..ResolvedStyle::default()
        }
    }

    pub fn root() -> ResolvedStyle {
        let mut style = base();
        style.flex.direction = FlexDirection::Column;
        style.gap = Size {
            width: DefiniteLength::Px(0.0),
            height: DefiniteLength::Px(10.0),
        };
        style.padding = Edges {
            top: DefiniteLength::Px(20.0),
            right: DefiniteLength::Px(20.0),
            bottom: DefiniteLength::Px(20.0),
            left: DefiniteLength::Px(20.0),
        };
        style.background = Some(Color::new(0.08, 0.09, 0.12, 1.0));
        style
    }

    pub fn form() -> ResolvedStyle {
        let mut style = base();
        style.flex.direction = FlexDirection::Row;
        style.gap = Size {
            width: DefiniteLength::Px(8.0),
            height: DefiniteLength::Px(0.0),
        };
        style
    }

    pub fn todo_row() -> ResolvedStyle {
        let mut style = base();
        style.flex.direction = FlexDirection::Row;
        style.flex.align_items = renderer_api::Align::Center;
        style.gap = Size {
            width: DefiniteLength::Px(8.0),
            height: DefiniteLength::Px(0.0),
        };
        style
    }

    pub fn input() -> ResolvedStyle {
        let mut style = base();
        style.size = Size {
            width: Length::Px(300.0),
            height: Length::Px(36.0),
        };
        style.padding = Edges {
            top: DefiniteLength::Px(6.0),
            right: DefiniteLength::Px(10.0),
            bottom: DefiniteLength::Px(6.0),
            left: DefiniteLength::Px(10.0),
        };
        style.background = Some(Color::new(0.16, 0.17, 0.21, 1.0));
        style.border = Edges {
            top: 1.0,
            right: 1.0,
            bottom: 1.0,
            left: 1.0,
        };
        style.border_color = Some(Color::new(0.32, 0.36, 0.44, 1.0));
        style
    }

    fn button(width: Length) -> ResolvedStyle {
        let mut style = base();
        style.size = Size {
            width,
            height: Length::Px(36.0),
        };
        style.padding = Edges {
            top: DefiniteLength::Px(6.0),
            right: DefiniteLength::Px(10.0),
            bottom: DefiniteLength::Px(6.0),
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

    pub fn action_button() -> ResolvedStyle {
        button(Length::Auto)
    }

    pub fn icon_button() -> ResolvedStyle {
        button(Length::Px(36.0))
    }

    pub fn button_label() -> ResolvedStyle {
        let mut style = base();
        style.text.font_size = 16.0;
        style.text.line_height = 20.0;
        style
    }

    pub fn error() -> ResolvedStyle {
        let mut style = base();
        style.text.font_size = 16.0;
        style.text.line_height = 20.0;
        style.text.color = Color::new(1.0, 0.52, 0.52, 1.0);
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
    use view::{text_fn, Callback, View};

    use crate::styles;

    #[macros::props]
    pub struct ActionButtonProps {
        pub label: String,
        pub on_click: Callback,
    }

    #[component]
    pub fn ActionButton(_cx: &Cx<'_>, props: ActionButtonProps) -> View {
        view! {
            <Button
                style={styles::action_button()}
                hover_style={styles::HOVER}
                focus_style={styles::FOCUS}
                cursor={CursorIcon::Pointer}
                on_click={move |cx: &Cx<'_>| props.on_click.call(cx)}
            >
                <Text style={styles::button_label()}>{props.label.get(cx)}</Text>
            </Button>
        }
    }

    #[macros::props]
    pub struct TextLineProps {
        pub value: String,
    }

    #[component]
    pub fn TextLine(_cx: &Cx<'_>, props: TextLineProps) -> View {
        text_fn(move |cx| props.value.get(cx)).style(styles::base())
    }

    #[macros::props]
    pub struct ErrorLineProps {
        pub value: String,
    }

    #[component]
    pub fn ErrorLine(_cx: &Cx<'_>, props: ErrorLineProps) -> View {
        text_fn(move |cx| props.value.get(cx)).style(styles::error())
    }
}

mod uicomponents {
    use std::rc::Rc;

    use macros::{component, view};
    use reactive_core::{Cx, RuntimeHandle};
    use renderer_api::CursorIcon;
    use view::control::{For, Loading};
    use view::{button, text_fn, Prop, TextInputState, View};

    use crate::basecomponents::{
        ActionButton, ActionButtonProps, ErrorLine, ErrorLineProps, TextLine, TextLineProps,
    };
    use crate::model::Api;
    use crate::store::{TodoRow, TodosStore};
    use crate::styles;

    #[component]
    pub fn LoadingView(cx: &Cx<'_>) -> View {
        view! {
            <Column style={styles::root()}>
                <TextLine value={String::from("Loading todos…")} />
            </Column>
        }
    }

    #[macros::props]
    pub struct TodoRowViewProps {
        pub id: u64,
        #[prop(raw)]
        pub store: Rc<TodosStore>,
    }

    #[component]
    pub fn TodoRowView(cx: &Cx<'_>, props: TodoRowViewProps) -> View {
        let store = props.store;
        let id = props.id.get(cx);
        // The keyed list composes truth, live patches, and row guesses, so a
        // binding sees the optimistic value while a derivation sees truth.
        let rows = store.rows();
        let rows_for_label = rows.clone();
        let rows_for_text = rows.clone();
        let store_for_click = Rc::clone(&store);

        let toggle = button(
            vec![text_fn(move |cx| {
                let done = rows_for_label
                    .get(cx)
                    .iter()
                    .any(|row| row.id == id && row.done);
                if done {
                    String::from("☑")
                } else {
                    String::from("☐")
                }
            })
            .style(styles::button_label())],
            move |cx: &Cx<'_>| store_for_click.toggle(cx, id),
        )
        .style(styles::icon_button())
        .hover_style(styles::HOVER)
        .focus_style(styles::FOCUS)
        .cursor(CursorIcon::Pointer);

        view! {
            <Row style={styles::todo_row()}>
                <Embed expr={toggle} />
                <Embed expr={
                    text_fn(move |cx| {
                        rows_for_text
                            .get(cx)
                            .iter()
                            .find(|row| row.id == id)
                            .map_or_else(String::new, |row| {
                                if row.pending {
                                    format!("{}  (saving…)", row.text)
                                } else {
                                    row.text.clone()
                                }
                            })
                    })
                    .style(styles::base())
                } />
            </Row>
        }
    }

    #[macros::props]
    pub struct ContentProps {
        #[prop(raw)]
        pub store: Rc<TodosStore>,
    }

    #[component]
    pub fn Content(cx: &Cx<'_>, props: ContentProps) -> View {
        let store = props.store;
        let input = TextInputState::new(cx, "");

        let store_for_add = Rc::clone(&store);
        let input_for_add = input.clone();
        let store_for_fail = Rc::clone(&store);
        let store_for_rows = Rc::clone(&store);
        let store_for_children = Rc::clone(&store);
        let store_for_error = Rc::clone(&store);

        view! {
            <Column style={styles::root()}>
                <Row style={styles::form()}>
                    <TextLine value={String::from("New todo")} />
                    <Embed expr={input.view(styles::input())} />
                    <ActionButton
                        label={String::from("Add")}
                        on_click={move |cx: &Cx<'_>| {
                            let text = input_for_add.text().trim().to_string();
                            if text.is_empty() {
                                return;
                            }
                            input_for_add.clear(cx);
                            store_for_add.add(cx, text);
                        }}
                    />
                </Row>
                <For
                    each={move |cx: &Cx<'_>| store_for_rows.rows().get(cx)}
                    key={|row: &TodoRow| row.id}
                    children={move |cx: &Cx<'_>, row: &TodoRow| {
                        let id = row.id;
                        let store = Rc::clone(&store_for_children);
                        view! {
                            <TodoRowView id={id} store={store} />
                        }
                    }}
                />
                <ActionButton
                    label={String::from("Fail the next save")}
                    on_click={move |cx: &Cx<'_>| {
                        store_for_fail.set_fail_next(cx, true);
                    }}
                />
                <ErrorLine
                    value={Prop::derived(cx, move |cx| store_for_error.error().get(cx))}
                />
            </Column>
        }
    }

    #[component]
    pub fn App(cx: &Cx<'_>) -> View {
        let runtime = cx.use_context::<RuntimeHandle>().expect("runtime context");
        let api = Rc::new(Api::new());
        let store = TodosStore::new(cx, runtime, api);

        view! {
            <Loading fallback={LoadingView}>
                <Content store={Rc::clone(&store)} />
            </Loading>
        }
    }
}

mod app {
    use super::*;

    pub struct TodosApp {
        runtime: RuntimeHandle,
        tree: ViewTree,
        root: reactive_core::NodeId,
        frames: u64,
    }

    impl TodosApp {
        pub fn new() -> Self {
            let runtime = reactive_core::Runtime::new().into_handle();
            let renderer = Rc::new(RefCell::new(NullRenderer::new()));
            let tree = ViewTree::new_with_shaper(
                Box::new(SharedNullRenderer::new(Rc::clone(&renderer))),
                runtime.clone(),
                Box::new(text::CosmicShaper::new()),
            );
            let cx = runtime.cx();
            cx.provide_context(runtime.clone());
            let root = tree.mount(&cx, crate::uicomponents::App(&cx));
            Self {
                runtime,
                tree,
                root,
                frames: 0,
            }
        }
    }

    impl Drop for TodosApp {
        fn drop(&mut self) {
            let cx = self.runtime.cx();
            self.tree.unmount(&cx, self.root);
        }
    }

    impl WindowApp for TodosApp {
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
            self.runtime.cx().pending_tasks() > 0 || self.tree.dirty_views() > 0
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
        let mut app = TodosApp::new();
        app.resize(metrics);
        let mut output = app.frame(metrics);
        for _ in 0..100 {
            std::thread::sleep(std::time::Duration::from_millis(16));
            output = app.frame(metrics);
        }
        save_frame(&output, path);
    }

    fn save_frame(output: &FrameOutput, path: &str) {
        use renderer_native::PaintBackend;
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
    if let Ok(path) = std::env::var("TODOS_SNAPSHOT") {
        app::write_snapshot(&path);
        return;
    }
    if let Err(error) = run_windowed("optimistic todos", 760, 360, app::TodosApp::new()) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
