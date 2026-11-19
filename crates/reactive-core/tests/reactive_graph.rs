//! Phase 1 graph tests: glitch freedom, batching, equality, effects, owners,
//! cleanup, context, and path-isolated invalidation.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use reactive_core::{
    create_derived, create_derived_signal, create_memo, create_signal, create_signal_with, effect,
    render_effect, Runtime,
};

#[test]
fn signal_writes_stage_until_flush() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (read, write) = create_signal(&cx, 1);

    assert_eq!(read.get(&cx), 1);
    write.set(&cx, 2);
    assert_eq!(
        read.get(&cx),
        1,
        "staged writes are not visible before flush"
    );

    cx.flush().unwrap();
    assert_eq!(read.get(&cx), 2);
}

#[test]
fn diamond_is_glitch_free_and_computes_in_height_order() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (a, set_a) = create_signal(&cx, 1);

    let b_runs = Rc::new(Cell::new(0));
    let c_runs = Rc::new(Cell::new(0));
    let d_observations = Rc::new(RefCell::new(Vec::new()));

    let b_runs_for_b = Rc::clone(&b_runs);
    let b = create_memo(&cx, move |cx, _| {
        b_runs_for_b.set(b_runs_for_b.get() + 1);
        a.get(cx) * 2
    });

    let c_runs_for_c = Rc::clone(&c_runs);
    let c = create_memo(&cx, move |cx, _| {
        c_runs_for_c.set(c_runs_for_c.get() + 1);
        a.get(cx) + 1
    });

    let observations = Rc::clone(&d_observations);
    let d = create_memo(&cx, move |cx, _| {
        let value = b.get(cx) + c.get(cx);
        observations.borrow_mut().push(value);
        value
    });

    assert_eq!(d.get(&cx), 4);
    assert_eq!(b_runs.get(), 1);
    assert_eq!(c_runs.get(), 1);

    set_a.set(&cx, 2);
    cx.flush().unwrap();

    assert_eq!(d.get(&cx), 7);
    assert_eq!(b_runs.get(), 2);
    assert_eq!(c_runs.get(), 2);
    assert_eq!(&*d_observations.borrow(), &[4, 7]);
}

#[test]
fn batched_writes_compute_once_and_equality_short_circuits() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (a, set_a) = create_signal(&cx, 1);
    let runs = Rc::new(Cell::new(0));

    let runs_for_memo = Rc::clone(&runs);
    let memo = create_memo(&cx, move |cx, _| {
        runs_for_memo.set(runs_for_memo.get() + 1);
        a.get(cx) * 10
    });

    assert_eq!(memo.get(&cx), 10);
    assert_eq!(runs.get(), 1);

    set_a.set(&cx, 2);
    set_a.set(&cx, 3);
    cx.flush().unwrap();
    assert_eq!(memo.get(&cx), 30);
    assert_eq!(runs.get(), 2, "two writes in one tick recompute once");

    set_a.set(&cx, 3);
    cx.flush().unwrap();
    assert_eq!(memo.get(&cx), 30);
    assert_eq!(runs.get(), 2, "an equal write does not invalidate");
}

#[test]
fn updates_follow_set_and_update_with_and_update() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (value, write) = create_signal(&cx, 1);
    let runs = Rc::new(Cell::new(0));

    let runs_for_memo = Rc::clone(&runs);
    let memo = create_memo(&cx, move |cx, _| {
        runs_for_memo.set(runs_for_memo.get() + 1);
        value.get(cx)
    });
    assert_eq!(memo.get(&cx), 1);
    assert_eq!(runs.get(), 1);

    write.update_with(&cx, |value| value + 1);
    cx.flush().unwrap();
    assert_eq!(memo.get(&cx), 2);
    assert_eq!(runs.get(), 2);

    write.update(&cx, |value| *value += 1);
    cx.flush().unwrap();
    assert_eq!(memo.get(&cx), 3);
    assert_eq!(runs.get(), 3);

    // update is in-place and always notifies, even for a no-op mutation.
    write.update(&cx, |_value| {});
    cx.flush().unwrap();
    assert_eq!(memo.get(&cx), 3);
    assert_eq!(runs.get(), 4, "update always notifies");
}

#[test]
fn effects_run_after_computes_and_applies_run_in_creation_order() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (a, set_a) = create_signal(&cx, 0);
    let order = Rc::new(RefCell::new(Vec::new()));

    let order_for_first = Rc::clone(&order);
    let _first = effect(
        &cx,
        move |cx| a.get(cx),
        move |_cx, value| order_for_first.borrow_mut().push(format!("first:{value}")),
    );
    let order_for_second = Rc::clone(&order);
    let _second = effect(
        &cx,
        move |cx| a.get(cx),
        move |_cx, value| {
            order_for_second
                .borrow_mut()
                .push(format!("second:{value}"))
        },
    );

    assert!(order.borrow().is_empty(), "effects run at flush");
    cx.flush().unwrap();
    assert_eq!(&*order.borrow(), &["first:0", "second:0"]);

    set_a.set(&cx, 1);
    cx.flush().unwrap();
    assert_eq!(
        &*order.borrow(),
        &["first:0", "second:0", "first:1", "second:1"]
    );
}

#[test]
fn render_effect_runs_synchronously_once_at_creation() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (a, set_a) = create_signal(&cx, 0);
    let applies = Rc::new(RefCell::new(Vec::new()));

    let applies_for_effect = Rc::clone(&applies);
    let _handle = render_effect(
        &cx,
        move |cx| a.get(cx),
        move |_cx, value| applies_for_effect.borrow_mut().push(*value),
    );
    assert_eq!(
        &*applies.borrow(),
        &[0],
        "render effect applies at creation"
    );

    set_a.set(&cx, 1);
    cx.flush().unwrap();
    assert_eq!(&*applies.borrow(), &[0, 1]);
}

#[test]
fn effect_apply_can_stage_a_write_and_another_round_settles_it() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (a, set_a) = create_signal(&cx, 0);
    let (b, set_b) = create_signal(&cx, 0);

    let _handle = effect(
        &cx,
        move |cx| a.get(cx),
        move |cx, value| {
            if *value > 0 {
                set_b.set(cx, value * 2);
            }
        },
    );

    cx.flush().unwrap();
    assert_eq!(b.get(&cx), 0);
    set_a.set(&cx, 2);
    cx.flush().unwrap();
    assert_eq!(b.get(&cx), 4);
}

#[test]
fn untracked_reads_do_not_create_a_dependency() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (a, set_a) = create_signal(&cx, 1);
    let runs = Rc::new(Cell::new(0));

    let runs_for_memo = Rc::clone(&runs);
    let memo = create_memo(&cx, move |cx, _| {
        runs_for_memo.set(runs_for_memo.get() + 1);
        cx.untrack(|| a.get(cx))
    });
    assert_eq!(memo.get(&cx), 1);
    assert_eq!(runs.get(), 1);

    set_a.set(&cx, 2);
    cx.flush().unwrap();
    assert_eq!(memo.get(&cx), 1, "the untracked read kept the old value");
    assert_eq!(runs.get(), 1, "no dependency edge was created");
}

#[test]
fn effect_dispose_stops_future_applies() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (a, set_a) = create_signal(&cx, 0);
    let applies = Rc::new(Cell::new(0));

    let applies_for_effect = Rc::clone(&applies);
    let handle = effect(
        &cx,
        move |cx| a.get(cx),
        move |_cx, _value| applies_for_effect.set(applies_for_effect.get() + 1),
    );
    cx.flush().unwrap();
    assert_eq!(applies.get(), 1);

    handle.dispose(&cx);
    set_a.set(&cx, 1);
    cx.flush().unwrap();
    assert_eq!(applies.get(), 1);
}

#[test]
fn nested_owners_dispose_children_then_cleanups_lifo() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let order = Rc::new(RefCell::new(Vec::new()));
    let mut cleanups = Vec::new();

    let parent = cx.create_owner();
    let order_for_parent = Rc::clone(&order);
    cx.with_owner(parent, |cx| {
        cleanups.push(cx.on_cleanup({
            let order = Rc::clone(&order_for_parent);
            move || order.borrow_mut().push("parent-1")
        }));
        cleanups.push(cx.on_cleanup({
            let order = Rc::clone(&order_for_parent);
            move || order.borrow_mut().push("parent-2")
        }));

        let child = cx.create_owner();
        let order_for_child = Rc::clone(&order);
        cx.with_owner(child, |cx| {
            cleanups.push(cx.on_cleanup(move || order_for_child.borrow_mut().push("child")));
        });
    });

    parent.dispose(&cx);
    assert_eq!(&*order.borrow(), &["child", "parent-2", "parent-1"]);
}

#[test]
fn cleanup_handle_runs_early_and_only_once() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let runs = Rc::new(Cell::new(0));
    let owner = cx.create_owner();

    let handle = cx.with_owner(owner, |cx| {
        let runs = Rc::clone(&runs);
        cx.on_cleanup(move || runs.set(runs.get() + 1))
    });
    assert_eq!(runs.get(), 0);
    drop(handle);
    assert_eq!(runs.get(), 1);

    owner.dispose(&cx);
    assert_eq!(runs.get(), 1, "owner disposal does not rerun the cleanup");
}

#[test]
fn context_walks_nested_owners() {
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Theme(u32);

    let runtime = Runtime::new();
    let cx = runtime.cx();
    let parent = cx.create_owner();
    let child = cx.with_owner(parent, |cx| {
        cx.provide_context(Theme(7));
        cx.create_owner()
    });

    assert_eq!(cx.use_context::<Theme>(), None);
    let theme = cx.with_owner(child, |cx| cx.use_context::<Theme>());
    assert_eq!(theme, Some(Theme(7)));
}

#[test]
#[cfg_attr(miri, ignore = "scale test; run natively")]
fn a_write_touches_only_its_dependent_path_in_a_10k_graph() {
    const N: usize = 10_000;

    let runtime = Runtime::new();
    let cx = runtime.cx();
    let mut signals = Vec::with_capacity(N);
    let mut memos = Vec::with_capacity(N);
    let mut runs = Vec::with_capacity(N);

    for index in 0..N {
        let (read, write) = create_signal(&cx, index as i32);
        let counter = Rc::new(Cell::new(0));
        let counter_for_memo = Rc::clone(&counter);
        let memo = create_memo(&cx, move |cx, _| {
            counter_for_memo.set(counter_for_memo.get() + 1);
            read.get(cx)
        });
        let _ = memo.get(&cx);
        counter.set(0);
        signals.push((read, write));
        memos.push(memo);
        runs.push(counter);
    }

    runtime.reset_dirty_visits();
    signals[0].1.set(&cx, 9_999);
    cx.flush().unwrap();

    assert_eq!(
        runtime.dirty_visits(),
        1,
        "only the written signal's dependent path was marked"
    );
    assert_eq!(runs[0].get(), 1, "the written path recomputed once");
    for (index, counter) in runs.iter().enumerate().skip(1) {
        assert_eq!(counter.get(), 0, "unrelated path {index} recomputed");
    }
    assert_eq!(memos[0].get(&cx), 9_999);
}

#[test]
fn custom_comparator_controls_notification() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (value, write) = create_signal_with(&cx, 1, |a, b| a % 10 == b % 10);
    let runs = Rc::new(Cell::new(0));

    let runs_for_memo = Rc::clone(&runs);
    let memo = create_memo(&cx, move |cx, _| {
        runs_for_memo.set(runs_for_memo.get() + 1);
        value.get(cx)
    });
    assert_eq!(memo.get(&cx), 1);
    assert_eq!(runs.get(), 1);

    write.set(&cx, 11);
    cx.flush().unwrap();
    assert_eq!(memo.get(&cx), 1, "11 is equal to 1 under the comparator");
    assert_eq!(runs.get(), 1);

    write.set(&cx, 12);
    cx.flush().unwrap();
    assert_eq!(memo.get(&cx), 12);
    assert_eq!(runs.get(), 2);
}

#[test]
fn writable_derived_signal_recomputes_from_deps_but_accepts_a_write() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (base, set_base) = create_signal(&cx, 2);
    let (value, write) = create_derived_signal(&cx, move |cx, _| base.get(cx) * 2);

    assert_eq!(value.get(&cx), 4);

    write.set(&cx, 100);
    cx.flush().unwrap();
    assert_eq!(value.get(&cx), 100, "a write overrides until a dep changes");

    set_base.set(&cx, 3);
    cx.flush().unwrap();
    assert_eq!(value.get(&cx), 6, "a dep change recomputes over the write");
}

#[test]
fn uncached_derived_runs_every_read_without_a_node() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (base, set_base) = create_signal(&cx, 2);
    let runs = Rc::new(Cell::new(0));

    let runs_for_derived = Rc::clone(&runs);
    let derived = create_derived(&cx, move |cx| {
        runs_for_derived.set(runs_for_derived.get() + 1);
        base.get(cx) * 3
    });

    assert_eq!(derived.get(&cx), 6);
    derived.get(&cx);
    assert_eq!(runs.get(), 2, "uncached reads rerun the closure");

    set_base.set(&cx, 4);
    cx.flush().unwrap();
    assert_eq!(derived.get(&cx), 12);
    assert_eq!(runs.get(), 3);
}

#[test]
fn render_effects_apply_before_user_effects() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (a, set_a) = create_signal(&cx, 0);
    let order = Rc::new(RefCell::new(Vec::new()));

    let order_for_user = Rc::clone(&order);
    let _user = effect(
        &cx,
        move |cx| a.get(cx),
        move |_cx, value| order_for_user.borrow_mut().push(format!("user:{value}")),
    );
    let order_for_render = Rc::clone(&order);
    let _render = render_effect(
        &cx,
        move |cx| a.get(cx),
        move |_cx, value| {
            order_for_render
                .borrow_mut()
                .push(format!("render:{value}"))
        },
    );

    assert_eq!(&*order.borrow(), &["render:0"]);
    cx.flush().unwrap();
    assert_eq!(&*order.borrow(), &["render:0", "user:0"]);

    set_a.set(&cx, 1);
    cx.flush().unwrap();
    assert_eq!(
        &*order.borrow(),
        &["render:0", "user:0", "render:1", "user:1"]
    );
}

#[test]
fn owner_disposal_disposes_nodes_and_stops_effects() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let owner = cx.create_owner();
    let (read, write, applies) = cx.with_owner(owner, |cx| {
        let (read, write) = create_signal(cx, 0);
        let applies = Rc::new(Cell::new(0));
        let applies_for_effect = Rc::clone(&applies);
        let _effect = effect(
            cx,
            move |cx| read.get(cx),
            move |_cx, _value| applies_for_effect.set(applies_for_effect.get() + 1),
        );
        (read, write, applies)
    });
    cx.flush().unwrap();
    assert_eq!(applies.get(), 1);
    assert!(!read.is_disposed(&cx));

    owner.dispose(&cx);
    assert!(read.is_disposed(&cx));
    assert!(write.is_disposed(&cx));

    // The handles are stale, but writing through a disposed handle is a no-op;
    // flush must not revive the effect.
    write.set(&cx, 1);
    cx.flush().unwrap();
    assert_eq!(applies.get(), 1);
}
