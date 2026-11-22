//! Optimistic todos: the expected change shows at once, the save runs in the
//! background, and a rejected save is discarded without rollback code.
//!
//! Module layout:
//! - `model`: todo data and a pretend server with latency and a failure switch
//! - `store`: read view + actions-only writer over a store derived from `api.list()`
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
    use reactive_stores::OptimisticStore;

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

    /// The read side. It exposes the composed rows and the error text, never a
    /// field handle or a setter.
    pub struct Todos {
        todos: OptimisticStore<Vec<TodoRow>>,
        error: ReadSignal<String>,
    }

    impl Todos {
        /// The composed display rows: truth, live patches, and row guesses.
        #[must_use]
        pub fn rows(&self, cx: &Cx<'_>) -> Vec<TodoRow> {
            self.todos.list(|row| row.id).get(cx)
        }

        /// One composed row by id.
        #[must_use]
        pub fn row(&self, cx: &Cx<'_>, id: u64) -> Option<TodoRow> {
            self.rows(cx).into_iter().find(|row| row.id == id)
        }

        /// The last action error, empty when the last action succeeded.
        #[must_use]
        pub fn error(&self, cx: &Cx<'_>) -> String {
            self.error.get(cx)
        }
    }

    /// The write side. It exposes actions, not `set_todos`.
    pub struct TodoActions {
        runtime: RuntimeHandle,
        api: Rc<Api>,
        todos: OptimisticStore<Vec<TodoRow>>,
        set_error: WriteSignal<String>,
    }

    impl TodoActions {
        pub fn toggle(self: &Rc<Self>, id: u64) {
            let cx = self.runtime.cx();
            let todos = self.todos.list(|row| row.id);
            if !todos
                .get(&cx)
                .iter()
                .any(|row| row.id == id && !row.pending)
            {
                return;
            }
            let Some(row) = todos.at_key(&cx, &id) else {
                return;
            };
            let api = self.api.clone();
            let store = self.todos.clone();
            let set_error = self.set_error;
            let _ = self.runtime.action_with(
                move |rt| async move {
                    row.update(&rt.cx(), |row| {
                        row.done = !row.done;
                        row.pending = true;
                    });
                    api.toggle(id).await?;
                    refresh(&rt.cx(), |cx| {
                        let _ = store.get(cx);
                    });
                    Ok::<(), String>(())
                },
                move |cx, result| match result {
                    Ok(()) => set_error.set(cx, String::new()),
                    Err(error) => set_error.set(cx, error.clone()),
                },
            );
        }

        pub fn add(self: &Rc<Self>, text: String) {
            let api = self.api.clone();
            let todos = self.todos.list(|row| row.id);
            let store = self.todos.clone();
            let set_error = self.set_error;
            let id = self.api.next_id();
            let _ = self.runtime.action_with(
                move |rt| async move {
                    todos.push(
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
                        let _ = store.get(cx);
                    });
                    Ok::<(), String>(())
                },
                move |cx, result| match result {
                    Ok(()) => set_error.set(cx, String::new()),
                    Err(error) => set_error.set(cx, error.clone()),
                },
            );
        }

        pub fn fail_next(&self) {
            self.api.set_fail_next(true);
        }
    }

    /// Create the read side and the action-only write side.
    pub fn create_todos(
        cx: &Cx<'_>,
        runtime: RuntimeHandle,
        api: Rc<Api>,
    ) -> (Rc<Todos>, Rc<TodoActions>) {
        let api_for_load = api.clone();
        let todos = OptimisticStore::new(cx, Vec::new(), move || {
            let api = api_for_load.clone();
            async move {
                let list = api.list().await;
                Ok::<Vec<TodoRow>, reactive_core::AsyncError>(
                    list.into_iter().map(todo_row).collect(),
                )
            }
        });
        let (error, set_error) = create_signal(cx, String::new());
        (
            Rc::new(Todos {
                todos: todos.clone(),
                error,
            }),
            Rc::new(TodoActions {
                runtime,
                api,
                todos,
                set_error,
            }),
        )
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
    use crate::store::{create_todos, TodoActions, TodoRow, Todos};
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
        pub read: Rc<Todos>,
        pub actions: Rc<TodoActions>,
    }

    #[component]
    pub fn TodoRowView(cx: &Cx<'_>, props: TodoRowViewProps) -> View {
        let id = props.id.get(cx);
        button(
            vec![text_fn(move |cx| {
                props.read.row(cx, id).map_or_else(String::new, |row| {
                    let mark = if row.done { "☑" } else { "☐" };
                    if row.pending {
                        format!("{mark}  {}  (saving…)", row.text)
                    } else {
                        format!("{mark}  {}", row.text)
                    }
                })
            })
            .style(styles::base())],
            move |_cx: &Cx<'_>| props.actions.toggle(id),
        )
        .style(styles::todo_row())
        .hover_style(styles::HOVER)
        .focus_style(styles::FOCUS)
        .cursor(CursorIcon::Pointer)
    }

    #[macros::props]
    pub struct TodoFormProps {
        pub actions: Rc<TodoActions>,
    }

    #[component]
    pub fn TodoForm(cx: &Cx<'_>, props: TodoFormProps) -> View {
        let input = TextInputState::new(cx, "");
        let input_for_add = input.clone();

        view! {
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
                        props.actions.add(text);
                    }}
                />
            </Row>
        }
    }

    #[macros::props]
    pub struct FailButtonProps {
        pub actions: Rc<TodoActions>,
    }

    #[component]
    pub fn FailButton(cx: &Cx<'_>, props: FailButtonProps) -> View {
        view! {
            <ActionButton
                label={String::from("Fail the next save")}
                on_click={move |_cx: &Cx<'_>| props.actions.fail_next()}
            />
        }
    }

    #[macros::props]
    pub struct ErrorTextProps {
        pub read: Rc<Todos>,
    }

    #[component]
    pub fn ErrorText(cx: &Cx<'_>, props: ErrorTextProps) -> View {
        view! {
            <ErrorLine value={Prop::derived(cx, move |cx| props.read.error(cx))} />
        }
    }

    #[macros::props]
    pub struct ContentProps {
        pub read: Rc<Todos>,
        pub actions: Rc<TodoActions>,
    }

    #[component]
    pub fn Content(cx: &Cx<'_>, props: ContentProps) -> View {
        let read_for_rows = props.read.clone();
        let read_for_row = props.read.clone();
        let actions_for_row = props.actions.clone();

        view! {
            <Column style={styles::root()}>
                <TodoForm actions={props.actions.clone()} />
                <For
                    each={move |cx| read_for_rows.rows(cx)}
                    key={|row: &TodoRow| row.id}
                    children={move |cx, row: &TodoRow| view! {
                        <TodoRowView
                            id={row.id}
                            read={read_for_row.clone()}
                            actions={actions_for_row.clone()}
                        />
                    }}
                />
                <FailButton actions={props.actions.clone()} />
                <ErrorText read={props.read.clone()} />
            </Column>
        }
    }

    #[component]
    pub fn App(cx: &Cx<'_>) -> View {
        let runtime = cx.use_context::<RuntimeHandle>().expect("runtime context");
        let api = Rc::new(Api::new());
        let (read, actions) = create_todos(cx, runtime, api);

        view! {
            <Loading fallback={LoadingView}>
                <Content read={read.clone()} actions={actions.clone()} />
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
                Box::new(SharedNullRenderer::new(renderer.clone())),
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
