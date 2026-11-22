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

mod optimistic {
    use std::cell::{Cell, RefCell};
    use std::future::poll_fn;
    use std::rc::Rc;
    use std::task::Poll;

    use reactive_core::{
        create_memo, loading_boundary, refresh, render_effect, Cx, Runtime, RuntimeHandle,
    };

    use crate::{app, store, AppStoreFields, Todo, UserStoreFields};

    fn delayed(polls: u32) -> impl std::future::Future<Output = ()> {
        let seen = Rc::new(Cell::new(0));
        poll_fn(move |_| {
            let count = seen.get();
            seen.set(count + 1);
            if count < polls {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
    }

    fn settle(runtime: &RuntimeHandle, cx: &Cx<'_>) {
        for _ in 0..8 {
            runtime.pump();
            cx.flush().unwrap();
        }
    }

    fn effect_of<T: Clone + 'static>(
        cx: &Cx<'_>,
        mut read: impl FnMut(&Cx<'_>) -> T + 'static,
    ) -> (Rc<RefCell<T>>, Rc<Cell<usize>>, reactive_core::EffectHandle) {
        let seen = Rc::new(RefCell::new(read(cx)));
        let runs = Rc::new(Cell::new(0));
        let seen_for_effect = Rc::clone(&seen);
        let runs_for_effect = Rc::clone(&runs);
        let mut read = read;
        let handle = render_effect(
            cx,
            move |cx| read(cx),
            move |_cx, value: &T| {
                *seen_for_effect.borrow_mut() = value.clone();
                runs_for_effect.set(runs_for_effect.get() + 1);
            },
        );
        (seen, runs, handle)
    }

    #[test]
    fn a_derived_store_loads_from_its_source() {
        let runtime = Runtime::new().into_handle();
        let cx = runtime.cx();
        let store = reactive_stores::OptimisticStore::new(&cx, Vec::<u32>::new(), || async {
            delayed(2).await;
            Ok::<Vec<u32>, reactive_core::AsyncError>(vec![1, 2, 3])
        });
        let store_for_boundary = store.clone();
        let boundary =
            loading_boundary(&cx, None, move |cx| store_for_boundary.get(cx).len(), |_| 0);
        assert_eq!(boundary.get(&cx), 0, "the first read is loading");

        settle(&runtime, &cx);
        assert_eq!(boundary.get(&cx), 3, "the source answer loads the store");
    }

    #[test]
    fn a_derived_list_commits_optimistically_and_refreshes() {
        let runtime = Runtime::new().into_handle();
        let cx = runtime.cx();
        let calls = Rc::new(Cell::new(0_u32));
        let calls_for_load = Rc::clone(&calls);
        let store = reactive_stores::OptimisticStore::new(&cx, Vec::<u32>::new(), move || {
            let calls = Rc::clone(&calls_for_load);
            async move {
                let call = calls.get();
                calls.set(call + 1);
                delayed(1).await;
                Ok::<Vec<u32>, reactive_core::AsyncError>(if call == 0 {
                    vec![1, 2]
                } else {
                    vec![1, 2, 3]
                })
            }
        });
        let list = store.list(|row| *row);
        settle(&runtime, &cx);
        assert_eq!(list.get(&cx), vec![1, 2]);

        let store_for_action = store.clone();
        let list_for_action = list.clone();
        let _handle = runtime.action(move |rt| async move {
            list_for_action.push(&rt.cx(), 3);
            delayed(1).await;
            refresh(&rt.cx(), |cx| {
                let _ = store_for_action.get(cx);
            });
            Ok::<(), String>(())
        });
        runtime.pump();
        cx.flush().unwrap();
        assert_eq!(list.get(&cx), vec![1, 2, 3], "the patch shows at once");

        settle(&runtime, &cx);
        assert_eq!(
            list.get(&cx),
            vec![1, 2, 3],
            "the success commit and the refresh agree on the truth"
        );
        assert!(
            calls.get() >= 2,
            "the closure probe re-asked the async source"
        );

        let list_for_failure = list.clone();
        let _handle = runtime.action(move |rt| async move {
            list_for_failure.push(&rt.cx(), 4);
            delayed(1).await;
            Err::<(), String>(String::from("rejected"))
        });
        runtime.pump();
        cx.flush().unwrap();
        assert_eq!(list.get(&cx), vec![1, 2, 3, 4]);
        settle(&runtime, &cx);
        assert_eq!(list.get(&cx), vec![1, 2, 3], "failure leaves the truth");
    }

    #[test]
    fn a_guess_shows_to_effects_and_reverts_on_failure() {
        let runtime = Runtime::new().into_handle();
        let cx = runtime.cx();
        let store = store!(&cx, app());
        let name = store.user().name();
        let (seen, _runs, _effect) = effect_of(&cx, {
            let name = name.clone();
            move |cx| name.get(cx)
        });
        cx.flush().unwrap();
        assert_eq!(*seen.borrow(), "Ada");
        let name_for_action = name.clone();
        let _handle = runtime.action(move |rt| async move {
            name_for_action.set(&rt.cx(), String::from("Grace"));
            delayed(2).await;
            Err::<(), String>(String::from("rejected"))
        });

        runtime.pump();
        cx.flush().unwrap();
        assert_eq!(
            *seen.borrow(),
            "Grace",
            "the guess is display state on the field's own path node"
        );

        settle(&runtime, &cx);
        assert_eq!(*seen.borrow(), "Ada", "a failed action drops its guess");
        assert_eq!(store.user().name().get(&cx), "Ada");
    }

    #[test]
    fn a_guess_is_display_only_for_derivations() {
        let runtime = Runtime::new().into_handle();
        let cx = runtime.cx();
        let store = store!(&cx, app());
        let name = store.user().name();
        let (seen, _runs, _effect) = effect_of(&cx, {
            let name = name.clone();
            move |cx| name.get(cx)
        });
        let name_for_memo = name.clone();
        let memo = create_memo(&cx, move |cx, _| name_for_memo.get(cx));
        cx.flush().unwrap();
        assert_eq!(memo.get(&cx), "Ada");
        let name_for_action = name.clone();
        let _handle = runtime.action(move |rt| async move {
            name_for_action.set(&rt.cx(), String::from("Grace"));
            delayed(2).await;
            Ok::<(), String>(())
        });

        runtime.pump();
        cx.flush().unwrap();
        assert_eq!(*seen.borrow(), "Grace");
        assert_eq!(
            memo.get(&cx),
            "Ada",
            "derivations keep the committed value while a guess is live"
        );

        settle(&runtime, &cx);
        assert_eq!(*seen.borrow(), "Grace");
        assert_eq!(
            memo.get(&cx),
            "Grace",
            "success writes the guess to the committed store"
        );
    }

    #[test]
    fn overlapping_actions_keep_their_own_paths() {
        let runtime = Runtime::new().into_handle();
        let cx = runtime.cx();
        let store = store!(&cx, app());
        let name = store.user().name();
        let age = store.user().age();
        let name_for_read = name.clone();
        let age_for_read = age.clone();
        let (seen, _runs, _effect) =
            effect_of(&cx, move |cx| (name_for_read.get(cx), age_for_read.get(cx)));
        cx.flush().unwrap();
        assert_eq!(*seen.borrow(), (String::from("Ada"), 30));
        let name_for_a = name.clone();
        let _a = runtime.action(move |rt| async move {
            name_for_a.set(&rt.cx(), String::from("Grace"));
            delayed(2).await;
            Err::<(), String>(String::from("a failed"))
        });
        let age_for_b = age.clone();
        let _b = runtime.action(move |rt| async move {
            age_for_b.set(&rt.cx(), 99);
            delayed(5).await;
            Ok::<(), String>(())
        });

        runtime.pump();
        cx.flush().unwrap();
        assert_eq!(*seen.borrow(), (String::from("Grace"), 99));

        for _ in 0..4 {
            runtime.pump();
            cx.flush().unwrap();
        }
        assert_eq!(
            *seen.borrow(),
            (String::from("Ada"), 99),
            "the failed action drops only its own path"
        );

        settle(&runtime, &cx);
        assert_eq!(*seen.borrow(), (String::from("Ada"), 99));
        assert_eq!(store.user().name().get(&cx), "Ada");
        assert_eq!(store.user().age().get(&cx), 99);
    }

    #[test]
    fn per_path_guesses_do_not_rerun_sibling_readers() {
        let runtime = Runtime::new().into_handle();
        let cx = runtime.cx();
        let store = store!(&cx, app());
        let name = store.user().name();
        let age = store.user().age();
        let name_for_effect = name.clone();
        let (_, name_runs, _name_effect) = effect_of(&cx, move |cx| name_for_effect.get(cx));
        let age_for_effect = age.clone();
        let (_, age_runs, _age_effect) = effect_of(&cx, move |cx| age_for_effect.get(cx));
        cx.flush().unwrap();
        let age_before = age_runs.get();
        let name_for_action = name.clone();
        let _handle = runtime.action(move |rt| async move {
            name_for_action.set(&rt.cx(), String::from("Grace"));
            delayed(2).await;
            Ok::<(), String>(())
        });

        runtime.pump();
        cx.flush().unwrap();
        assert!(name_runs.get() > 1, "the guessed path's reader reruns");
        assert_eq!(
            age_runs.get(),
            age_before,
            "a sibling path's reader does not rerun"
        );

        settle(&runtime, &cx);
    }

    #[test]
    fn list_patches_add_and_remove_rows() {
        let runtime = Runtime::new().into_handle();
        let cx = runtime.cx();
        let store = store!(&cx, app());
        let todos = store.todos().optimistic_list(|todo| todo.id);
        let todos_for_effect = todos.clone();
        let (seen, _runs, _effect) = effect_of(&cx, move |cx| todos_for_effect.get(cx).len());
        cx.flush().unwrap();
        assert_eq!(*seen.borrow(), 2);
        let todos_for_action = todos.clone();
        let added = Todo {
            id: 3,
            title: String::from("three"),
            done: false,
        };
        let _handle = runtime.action(move |rt| async move {
            todos_for_action.push(&rt.cx(), added);
            delayed(2).await;
            Ok::<(), String>(())
        });

        runtime.pump();
        cx.flush().unwrap();
        assert_eq!(*seen.borrow(), 3, "the patch shows at once");
        let ids: Vec<u32> = todos.get(&cx).iter().map(|todo| todo.id).collect();
        assert_eq!(ids, vec![1, 2, 3]);

        settle(&runtime, &cx);
        assert_eq!(*seen.borrow(), 3);
        assert_eq!(
            todos
                .get(&cx)
                .iter()
                .map(|todo| todo.id)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        let todos_for_remove = todos.clone();
        let _handle = runtime.action(move |rt| async move {
            todos_for_remove.remove(&rt.cx(), &1);
            delayed(2).await;
            Ok::<(), String>(())
        });
        runtime.pump();
        cx.flush().unwrap();
        assert_eq!(*seen.borrow(), 2);
        assert_eq!(
            todos
                .get(&cx)
                .iter()
                .map(|todo| todo.id)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        settle(&runtime, &cx);
        assert_eq!(
            todos
                .get(&cx)
                .iter()
                .map(|todo| todo.id)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[test]
    fn list_patch_failure_leaves_the_committed_rows() {
        let runtime = Runtime::new().into_handle();
        let cx = runtime.cx();
        let store = store!(&cx, app());
        let todos = store.todos().optimistic_list(|todo| todo.id);
        let todos_for_action = todos.clone();
        let _handle = runtime.action(move |rt| async move {
            todos_for_action.push(
                &rt.cx(),
                Todo {
                    id: 9,
                    title: String::from("nope"),
                    done: false,
                },
            );
            delayed(2).await;
            Err::<(), String>(String::from("rejected"))
        });
        runtime.pump();
        cx.flush().unwrap();
        assert_eq!(todos.len(&cx), 3);
        settle(&runtime, &cx);
        assert_eq!(todos.len(&cx), 2);
        assert_eq!(store.todos().get(&cx).len(), 2);
    }

    #[test]
    fn a_late_row_commit_does_not_resurrect_a_removed_row() {
        let runtime = Runtime::new().into_handle();
        let cx = runtime.cx();
        let store = store!(&cx, app());
        let todos = store.todos().optimistic_list(|todo| todo.id);
        let row = todos.at_key(&cx, &1).expect("row");
        let row_for_toggle = row.clone();
        let _toggle = runtime.action(move |rt| async move {
            row_for_toggle.update(&rt.cx(), |todo| todo.done = true);
            delayed(5).await;
            Ok::<(), String>(())
        });
        let todos_for_remove = todos.clone();
        let _remove = runtime.action(move |rt| async move {
            todos_for_remove.remove(&rt.cx(), &1);
            delayed(2).await;
            Ok::<(), String>(())
        });

        settle(&runtime, &cx);
        let ids: Vec<u32> = store.todos().get(&cx).iter().map(|todo| todo.id).collect();
        assert_eq!(ids, vec![2], "the removed row stays removed");
    }

    #[test]
    fn keyed_row_guess_follows_the_key_across_reorders() {
        let runtime = Runtime::new().into_handle();
        let cx = runtime.cx();
        let store = store!(&cx, app());
        let todos = store.todos().optimistic_list(|todo| todo.id);
        let row = todos.at_key(&cx, &1).expect("row");
        let todos_for_effect = todos.clone();
        let (seen, _runs, _effect) = effect_of(&cx, move |cx| todos_for_effect.get(cx));
        cx.flush().unwrap();
        let row_for_action = row.clone();
        let _handle = runtime.action(move |rt| async move {
            row_for_action.update(&rt.cx(), |todo| todo.done = true);
            delayed(4).await;
            Ok::<(), String>(())
        });
        runtime.pump();
        cx.flush().unwrap();
        assert!(seen.borrow().iter().any(|todo| todo.id == 1 && todo.done));

        // Reorder the committed list while the guess is live: the row guess
        // is keyed by id, so it still applies to row 1.
        let mut reordered = store.todos().get(&cx);
        reordered.reverse();
        store.todos().set(&cx, reordered);
        cx.flush().unwrap();
        let rows = seen.borrow().clone();
        assert!(rows.iter().any(|todo| todo.id == 1 && todo.done));
        assert_eq!(rows.first().map(|todo| todo.id), Some(2));

        settle(&runtime, &cx);
        assert!(store
            .todos()
            .get(&cx)
            .iter()
            .any(|todo| todo.id == 1 && todo.done));
    }
}
