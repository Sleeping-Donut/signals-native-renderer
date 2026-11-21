use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;

use reactive_core::{create_memo, Cx, Runtime};
use reactive_stores::{store, DynamicValue, Store, StoreDiagnostics, StorePatch};

#[derive(Clone, Debug, PartialEq, reactive_stores::Store)]
enum Status {
    Idle,
    Ready(u32),
    Pair(u32, u32),
}

#[derive(Clone, Debug, PartialEq, reactive_stores::Store, reactive_stores::Patch)]
struct User {
    name: String,
    age: u32,
}

#[derive(Clone, Debug, PartialEq, reactive_stores::Store, reactive_stores::Patch)]
struct Todo {
    id: u32,
    title: String,
    done: bool,
}

#[derive(Clone, Debug, PartialEq, reactive_stores::Store, reactive_stores::Patch)]
struct App {
    count: u32,
    name: String,
    user: User,
    todos: Vec<Todo>,
}

fn app() -> App {
    App {
        count: 0,
        name: "app".to_string(),
        user: User {
            name: "Ada".to_string(),
            age: 30,
        },
        todos: vec![
            Todo {
                id: 1,
                title: "one".to_string(),
                done: false,
            },
            Todo {
                id: 2,
                title: "two".to_string(),
                done: false,
            },
        ],
    }
}

fn counter<T: Clone + PartialEq + 'static>(
    cx: &Cx<'_>,
    read: impl Fn(&Cx<'_>) -> T + 'static,
) -> (reactive_core::Memo<T>, Rc<Cell<usize>>) {
    let runs = Rc::new(Cell::new(0));
    let runs_for_memo = Rc::clone(&runs);
    let memo = create_memo(cx, move |cx, _| {
        runs_for_memo.set(runs_for_memo.get() + 1);
        read(cx)
    });
    (memo, runs)
}

#[test]
fn field_writes_are_sibling_isolated() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let store = store!(&cx, app());
    let store_for_count = store.clone();
    let (count, count_runs) = counter(&cx, move |cx| store_for_count.count().get(cx));
    let store_for_name = store.clone();
    let (name, name_runs) = counter(&cx, move |cx| store_for_name.name().get(cx));

    assert_eq!(count.get(&cx), 0);
    assert_eq!(name.get(&cx), "app");
    assert_eq!(count_runs.get(), 1);
    assert_eq!(name_runs.get(), 1);

    store.count().set(&cx, 1);
    cx.flush().unwrap();
    assert_eq!(count.get(&cx), 1);
    assert_eq!(name.get(&cx), "app");
    assert_eq!(count_runs.get(), 2);
    assert_eq!(name_runs.get(), 1, "sibling field must not rerun");

    let clone = store.clone();
    let diagnostics: StoreDiagnostics = store.diagnostics();
    assert!(diagnostics.writes >= 1);
    assert!(diagnostics.reads >= 2);
    assert!(diagnostics.handles >= 2);
    assert_eq!(clone.count().get(&cx), 1);

    let strict = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cx.untrack(|| store.count().get_strict(&cx))
    }));
    assert!(
        strict.is_err(),
        "strict reads must reject untracked contexts"
    );
}

#[test]
fn nested_fields_compose_and_patch_changes_only_dirty_fields() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let store = Store::new(&cx, app());
    let store_for_name = store.clone();
    let (user_name, name_runs) = counter(&cx, move |cx| store_for_name.user().name().get(cx));
    let store_for_age = store.clone();
    let (user_age, age_runs) = counter(&cx, move |cx| store_for_age.user().age().get(cx));

    assert_eq!(user_name.get(&cx), "Ada");
    assert_eq!(user_age.get(&cx), 30);

    store.user().name().set(&cx, "Grace".to_string());
    cx.flush().unwrap();
    assert_eq!(user_name.get(&cx), "Grace");
    assert_eq!(name_runs.get(), 2);
    assert_eq!(age_runs.get(), 1, "sibling nested field must not rerun");

    let mut next = app().user;
    next.name = "Katherine".to_string();
    store.user().patch(&cx, next);
    cx.flush().unwrap();
    assert_eq!(user_name.get(&cx), "Katherine");
    assert_eq!(user_age.get(&cx), 30);
    assert_eq!(name_runs.get(), 3);
    assert_eq!(age_runs.get(), 1);
}

#[test]
fn keyed_patch_marks_only_changed_keys() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let store = Store::new(&cx, app());
    let todos = store.todos();
    let keyed = todos.keyed(|todo: &Todo| todo.id);
    let one = keyed.at_key(&1).expect("row 1");
    let two = keyed.at_key(&2).expect("row 2");
    let one_for_count = one.clone();
    let (one_title, one_runs) = counter(&cx, move |cx| one_for_count.title().get(cx));
    let two_for_count = two.clone();
    let (two_title, two_runs) = counter(&cx, move |cx| two_for_count.title().get(cx));
    assert_eq!(one_title.get(&cx), "one");
    assert_eq!(two_title.get(&cx), "two");

    let mut next = app().todos;
    next[0].title = "one changed".to_string();
    todos.patch_keyed(&cx, next, |todo: &Todo| todo.id);
    cx.flush().unwrap();

    assert_eq!(one_title.get(&cx), "one changed");
    assert_eq!(two_title.get(&cx), "two");
    assert_eq!(one_runs.get(), 2);
    assert_eq!(two_runs.get(), 1, "unchanged keyed row must not rerun");
}

#[test]
fn draft_writes_on_drop() {
    let runtime = Runtime::new().into_handle();
    let cx = runtime.cx();
    let store = Store::new(&cx, app());
    {
        let mut draft = store.user().draft(&runtime);
        draft.age += 1;
        assert_eq!(draft.age, 31);
    }
    cx.flush().unwrap();
    assert_eq!(store.user().age().get(&cx), 31);
}

#[test]
fn keyed_rows_survive_reorder() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let store = Store::new(&cx, app());
    let todos = store.todos();
    let keyed = todos.keyed(|todo: &Todo| todo.id);
    let one = keyed.at_key(&1).expect("row 1");
    let one_title = one.title();

    let mut next = app().todos;
    next.reverse();
    todos.patch_keyed(&cx, next, |todo: &Todo| todo.id);
    cx.flush().unwrap();

    assert_eq!(one_title.get(&cx), "one");
    assert_eq!(todos.get(&cx)[0].id, 2);
    assert_eq!(todos.get(&cx)[1].id, 1);
}

#[test]
fn top_level_patch_marks_only_changed_fields() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let store = Store::new(&cx, app());
    let store_for_count = store.clone();
    let (count, count_runs) = counter(&cx, move |cx| store_for_count.count().get(cx));
    let store_for_name = store.clone();
    let (name, name_runs) = counter(&cx, move |cx| store_for_name.name().get(cx));
    assert_eq!(count.get(&cx), 0);
    assert_eq!(name.get(&cx), "app");

    let mut next = app();
    next.count = 7;
    store.patch(&cx, next);
    cx.flush().unwrap();

    assert_eq!(count.get(&cx), 7);
    assert_eq!(name.get(&cx), "app");
    assert_eq!(count_runs.get(), 2);
    assert_eq!(name_runs.get(), 1);
}

#[test]
fn enum_variants_are_addressable_fields() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let store = Store::new(&cx, Status::Idle);
    let idle = store.is_idle();
    let ready = store.as_ready();
    assert!(idle.get(&cx));
    assert_eq!(ready.get(&cx), None);

    ready.set(&cx, Some(7));
    cx.flush().unwrap();
    assert_eq!(store.snapshot(&cx), Status::Ready(7));
    assert!(!idle.get(&cx));
    assert_eq!(ready.get(&cx), Some(7));

    let pair = store.as_pair();
    pair.set(&cx, Some((1, 2)));
    cx.flush().unwrap();
    assert_eq!(store.snapshot(&cx), Status::Pair(1, 2));
    assert_eq!(pair.get(&cx), Some((1, 2)));
}

#[test]
fn dynamic_store_supports_maps_lists_and_path_isolation() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let store = Store::new(&cx, DynamicValue::Map(BTreeMap::new()));
    let name = store.field("name");
    let score = store.field("score");
    let name_for_count = name.clone();
    let (name_value, name_runs) = counter(&cx, move |cx| name_for_count.get(cx));
    let score_for_count = score.clone();
    let (score_value, score_runs) = counter(&cx, move |cx| score_for_count.get(cx));
    assert_eq!(name_value.get(&cx), DynamicValue::Null);
    assert_eq!(score_value.get(&cx), DynamicValue::Null);

    name.set(&cx, DynamicValue::String("Ada".to_string()));
    cx.flush().unwrap();
    assert_eq!(name_value.get(&cx), DynamicValue::String("Ada".to_string()));
    assert_eq!(score_value.get(&cx), DynamicValue::Null);
    assert_eq!(name_runs.get(), 2);
    assert_eq!(score_runs.get(), 1);

    store
        .field("tags")
        .index(1)
        .set(&cx, DynamicValue::String("math".to_string()));
    cx.flush().unwrap();
    assert_eq!(
        store.field("tags").index(1).get(&cx),
        DynamicValue::String("math".to_string())
    );
    assert_eq!(store.field("tags").index(0).get(&cx), DynamicValue::Null);
}

#[test]
fn fields_split_into_read_and_write_capabilities() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let store = store!(&cx, app());
    let name = store.user().name();
    let (read, write) = name.split();
    assert_eq!(read.get(&cx), "Ada");

    write.set(&cx, String::from("Grace"));
    cx.flush().unwrap();
    assert_eq!(read.get(&cx), "Grace");

    name.write_only().update(&cx, |name| name.push('!'));
    cx.flush().unwrap();
    assert_eq!(name.read_only().get(&cx), "Grace!");
}
