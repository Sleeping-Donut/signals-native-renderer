//! Lane reads: `latest`, display-only guesses, quiet `refresh`, and merge.

use std::cell::{Cell, RefCell};
use std::future::poll_fn;
use std::pin::Pin;
use std::rc::Rc;
use std::task::Poll;

use reactive_core::{
    clear_guess, create_async_memo, create_memo, create_signal, guess, is_pending, latest,
    loading_boundary, refresh, render_effect, AsyncError, LatestError, OptimisticSignal, Runtime,
};

type Fut<T> = Pin<Box<dyn std::future::Future<Output = Result<T, AsyncError>>>>;

fn delayed<T: Clone + 'static>(value: T) -> Fut<T> {
    let polls = Rc::new(Cell::new(0));
    Box::pin(poll_fn(move |_| {
        let count = polls.get();
        polls.set(count + 1);
        if count < 2 {
            Poll::Pending
        } else {
            Poll::Ready(Ok(value.clone()))
        }
    }))
}

#[test]
fn latest_reads_the_staged_answer_during_a_hold() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (page, set_page) = create_signal(&cx, 0_u32);
    let data = create_async_memo(&cx, move |cx, _: Option<&u32>| delayed(page.get(cx)));
    let boundary = loading_boundary(&cx, None, move |cx| data.get(cx), |_| 0);
    let _ = boundary.get(&cx);
    for _ in 0..6 {
        runtime.pump();
        cx.flush().unwrap();
    }
    assert_eq!(data.get(&cx), 0);

    let derived = create_memo(&cx, move |cx, _: Option<&u32>| {
        latest(cx, data).unwrap_or_default()
    });

    set_page.set(&cx, 1);
    cx.flush().unwrap();
    assert!(is_pending(&cx, |cx| data.get(cx)));
    assert_eq!(latest(&cx, data), Ok(0), "nothing has landed yet");
    assert_eq!(data.get(&cx), 0, "display reads stay committed");

    for _ in 0..6 {
        runtime.pump();
        if latest(&cx, data) == Ok(1) {
            break;
        }
    }
    assert_eq!(latest(&cx, data), Ok(1), "the staged answer is visible");
    assert_eq!(data.get(&cx), 0, "display still sees the committed value");
    assert_eq!(
        derived.get(&cx),
        0,
        "a derivation over latest sees the screen, not the staged value"
    );

    cx.flush().unwrap();
    assert_eq!(data.get(&cx), 1);
    assert_eq!(latest(&cx, data), Ok(1));
    assert_eq!(derived.get(&cx), 1);
}

#[test]
fn latest_reports_loading_before_the_first_value() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let data = create_async_memo(&cx, |_cx, _: Option<&u32>| delayed(1_u32));
    assert_eq!(latest(&cx, data), Err(LatestError::Loading));
}

#[test]
fn guesses_are_display_only_and_clear() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (count, _set_count) = create_signal(&cx, 1_u32);
    let double = create_memo(&cx, move |cx, _| count.get(cx) * 2);
    let seen: Rc<RefCell<Vec<(u32, u32)>>> = Rc::new(RefCell::new(Vec::new()));
    let seen_for_effect = Rc::clone(&seen);
    let effect = render_effect(
        &cx,
        move |cx| (count.get(cx), double.get(cx)),
        move |_cx, pair: &(u32, u32)| seen_for_effect.borrow_mut().push(*pair),
    );
    cx.flush().unwrap();
    assert_eq!(seen.borrow().last().copied(), Some((1, 2)));

    guess(&cx, &count, 5_u32);
    cx.flush().unwrap();
    assert_eq!(
        seen.borrow().last().copied(),
        Some((5, 2)),
        "display sees the guess; derivations keep the committed value"
    );
    assert!(is_pending(&cx, |cx| count.get(cx)));

    clear_guess(&cx, &count);
    cx.flush().unwrap();
    assert_eq!(seen.borrow().last().copied(), Some((1, 2)));
    assert!(!is_pending(&cx, |cx| count.get(cx)));

    effect.dispose(&cx);
}

#[test]
fn refresh_is_a_quiet_re_ask() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (page, set_page) = create_signal(&cx, 0_u32);
    let data = create_async_memo(&cx, move |cx, _: Option<&u32>| delayed(page.get(cx)));
    let boundary = loading_boundary(&cx, None, move |cx| data.get(cx), |_| 0);
    let _ = boundary.get(&cx);
    for _ in 0..6 {
        runtime.pump();
        cx.flush().unwrap();
    }

    let seen: Rc<RefCell<Vec<u32>>> = Rc::new(RefCell::new(Vec::new()));
    let seen_for_effect = Rc::clone(&seen);
    let effect = render_effect(
        &cx,
        move |cx| data.get(cx),
        move |_cx, value: &u32| seen_for_effect.borrow_mut().push(*value),
    );
    cx.flush().unwrap();
    let notified = seen.borrow().len();

    refresh(&cx, data);
    assert!(!is_pending(&cx, |cx| data.get(cx)), "a re-ask is quiet");
    cx.flush().unwrap();
    assert!(!is_pending(&cx, |cx| data.get(cx)));
    for _ in 0..6 {
        runtime.pump();
    }
    cx.flush().unwrap();
    assert_eq!(data.get(&cx), 0);
    assert_eq!(
        seen.borrow().len(),
        notified,
        "an equal re-ask does not notify readers"
    );

    set_page.set(&cx, 1);
    cx.flush().unwrap();
    assert!(
        is_pending(&cx, |cx| data.get(cx)),
        "a changed dependency is a new question"
    );

    effect.dispose(&cx);
}

#[test]
fn results_ready_in_one_flush_commit_together() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (page, set_page) = create_signal(&cx, 0_u32);
    let left = create_async_memo(&cx, move |cx, _: Option<&u32>| delayed(page.get(cx) * 10));
    let right = create_async_memo(&cx, move |cx, _: Option<&u32>| delayed(page.get(cx) * 100));
    let boundary = loading_boundary(
        &cx,
        None,
        move |cx| (left.get(cx), right.get(cx)),
        |_| (0, 0),
    );
    let _ = boundary.get(&cx);
    for _ in 0..6 {
        runtime.pump();
        cx.flush().unwrap();
    }

    let seen: Rc<RefCell<Vec<(u32, u32)>>> = Rc::new(RefCell::new(Vec::new()));
    let seen_for_effect = Rc::clone(&seen);
    let effect = render_effect(
        &cx,
        move |cx| (left.get(cx), right.get(cx)),
        move |_cx, pair: &(u32, u32)| seen_for_effect.borrow_mut().push(*pair),
    );
    cx.flush().unwrap();
    assert_eq!(seen.borrow().as_slice(), &[(0, 0)]);

    set_page.set(&cx, 1);
    cx.flush().unwrap();
    for _ in 0..6 {
        runtime.pump();
    }
    cx.flush().unwrap();

    assert_eq!(
        seen.borrow().as_slice(),
        &[(0, 0), (10, 100)],
        "results that land before one flush are applied as one merged update"
    );

    effect.dispose(&cx);
}

#[test]
fn actions_guess_then_commit_after_await() {
    let runtime = Runtime::new().into_handle();
    let cx = runtime.cx();
    let (count, set_count) = create_signal(&cx, 1_u32);
    let seen: Rc<RefCell<Vec<u32>>> = Rc::new(RefCell::new(Vec::new()));
    let seen_for_effect = Rc::clone(&seen);
    let effect = render_effect(
        &cx,
        move |cx| count.get(cx),
        move |_cx, value: &u32| seen_for_effect.borrow_mut().push(*value),
    );
    cx.flush().unwrap();
    assert_eq!(seen.borrow().as_slice(), &[1]);
    let runtime_for_action = runtime.clone();
    let handle = runtime.action(move || {
        let rt = runtime_for_action.clone();
        async move {
            let cx = rt.cx();
            guess(&cx, &count, 5_u32);
            set_count.set(&rt.cx(), 7_u32); // buffered before the await
            if delayed(()).await.is_err() {
                return Err(String::from("async error"));
            }
            set_count.set(&rt.cx(), 9_u32); // buffered after the await
            Ok::<(), String>(())
        }
    });

    runtime.pump();
    cx.flush().unwrap();
    assert_eq!(
        seen.borrow().last().copied(),
        Some(5),
        "the guess shows during the await"
    );
    assert_eq!(
        count.committed(&cx),
        Some(1),
        "committed writes are held until the action settles"
    );
    assert!(is_pending(&cx, |cx| count.get(cx)));

    for _ in 0..4 {
        runtime.pump();
        cx.flush().unwrap();
    }
    assert_eq!(
        count.committed(&cx),
        Some(9),
        "writes after await land at settle"
    );
    assert_eq!(seen.borrow().last().copied(), Some(9));
    assert!(!is_pending(&cx, |cx| count.get(cx)));
    assert_eq!(
        handle.result().get(&cx),
        Some(Ok(())),
        "the action reports completion"
    );

    effect.dispose(&cx);
}

#[test]
fn overlapping_actions_keep_their_own_guesses() {
    let runtime = Runtime::new().into_handle();
    let cx = runtime.cx();
    let (count, _set_count) = create_signal(&cx, 1_u32);
    let seen: Rc<RefCell<Vec<u32>>> = Rc::new(RefCell::new(Vec::new()));
    let seen_for_effect = Rc::clone(&seen);
    let effect = render_effect(
        &cx,
        move |cx| count.get(cx),
        move |_cx, value: &u32| seen_for_effect.borrow_mut().push(*value),
    );
    cx.flush().unwrap();

    let runtime_a = runtime.clone();
    let _a = runtime.action(move || {
        let rt = runtime_a.clone();
        async move {
            guess(&rt.cx(), &count, 5_u32);
            Ok::<(), String>(())
        }
    });
    let runtime_b = runtime.clone();
    let _b = runtime.action(move || {
        let rt = runtime_b.clone();
        async move {
            guess(&rt.cx(), &count, 6_u32);
            if delayed(()).await.is_err() {
                return Err(String::from("async error"));
            }
            Ok::<(), String>(())
        }
    });

    runtime.pump();
    cx.flush().unwrap();
    assert_eq!(
        seen.borrow().last().copied(),
        Some(6),
        "A settling must not clear B's guess"
    );

    for _ in 0..4 {
        runtime.pump();
        cx.flush().unwrap();
    }
    assert_eq!(count.committed(&cx), Some(1));
    assert_eq!(seen.borrow().last().copied(), Some(1));

    effect.dispose(&cx);
}

#[test]
fn actions_report_errors_and_clear_guesses() {
    let runtime = Runtime::new().into_handle();
    let cx = runtime.cx();
    let (count, _set_count) = create_signal(&cx, 1_u32);
    let runtime_for_action = runtime.clone();
    let handle = runtime.action(move || {
        let rt = runtime_for_action.clone();
        async move {
            guess(&rt.cx(), &count, 5_u32);
            if delayed(()).await.is_err() {
                return Err(String::from("async error"));
            }
            Err::<(), String>(String::from("nope"))
        }
    });

    runtime.pump();
    cx.flush().unwrap();
    assert!(is_pending(&cx, |cx| count.get(cx)));

    for _ in 0..3 {
        runtime.pump();
        cx.flush().unwrap();
    }
    assert_eq!(
        count.committed(&cx),
        Some(1),
        "a failed action reveals no committed write"
    );
    assert!(!is_pending(&cx, |cx| count.get(cx)));
    assert_eq!(handle.result().get(&cx), Some(Err(String::from("nope"))));
}

#[test]
fn on_rearms_the_boundary_without_a_key() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (page, set_page) = create_signal(&cx, 0_u32);
    let data = create_async_memo(&cx, move |cx, _: Option<&u32>| delayed(page.get(cx) * 10));
    let boundary =
        loading_boundary(&cx, None, move |cx| data.get(cx), |_| 999_u32).on(&cx, move |cx| {
            let _ = page.get(cx);
        });
    let _ = boundary.get(&cx);
    for _ in 0..6 {
        runtime.pump();
        cx.flush().unwrap();
    }
    assert_eq!(boundary.get(&cx), 0);

    set_page.set(&cx, 1);
    cx.flush().unwrap();
    assert_eq!(
        boundary.get(&cx),
        999,
        "on re-arms the boundary and shows the fallback"
    );

    for _ in 0..6 {
        runtime.pump();
        cx.flush().unwrap();
    }
    assert_eq!(
        boundary.get(&cx),
        10,
        "the new content replaces the fallback"
    );
}

#[test]
fn without_on_a_disarmed_boundary_keeps_content() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (page, set_page) = create_signal(&cx, 0_u32);
    let data = create_async_memo(&cx, move |cx, _: Option<&u32>| delayed(page.get(cx) * 10));
    let boundary = loading_boundary(&cx, None, move |cx| data.get(cx), |_| 999_u32);
    let _ = boundary.get(&cx);
    for _ in 0..6 {
        runtime.pump();
        cx.flush().unwrap();
    }
    assert_eq!(boundary.get(&cx), 0);

    set_page.set(&cx, 1);
    cx.flush().unwrap();
    assert_eq!(
        boundary.get(&cx),
        0,
        "a disarmed boundary forwards and keeps the old content"
    );

    for _ in 0..6 {
        runtime.pump();
        cx.flush().unwrap();
    }
    assert_eq!(boundary.get(&cx), 10);
}

fn delayed_after<T: Clone + 'static>(value: T, polls: u32) -> Fut<T> {
    let count = Rc::new(Cell::new(0));
    Box::pin(poll_fn(move |_| {
        let polls_done = count.get();
        count.set(polls_done + 1);
        if polls_done < polls {
            Poll::Pending
        } else {
            Poll::Ready(Ok(value.clone()))
        }
    }))
}

fn next_random(state: &mut u64, bound: u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    (*state >> 33) % bound.max(1)
}

#[test]
fn merge_fuzz_random_arrival_orders() {
    let mut checks = 0_u64;
    for seed in 0..32_u64 {
        let mut state = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let left_polls = next_random(&mut state, 4) as u32;
        let right_polls = next_random(&mut state, 4) as u32;
        let runtime = Runtime::new();
        let cx = runtime.cx();
        let (page, set_page) = create_signal(&cx, 0_u32);
        let left = create_async_memo(&cx, move |cx, _: Option<&u32>| {
            delayed_after(page.get(cx) * 10, left_polls)
        });
        let right = create_async_memo(&cx, move |cx, _: Option<&u32>| {
            delayed_after(page.get(cx) * 100, right_polls)
        });
        let boundary = loading_boundary(
            &cx,
            None,
            move |cx| (left.get(cx), right.get(cx)),
            |_| (0_u32, 0_u32),
        );
        let _ = boundary.get(&cx);
        for _ in 0..8 {
            runtime.pump();
            cx.flush().unwrap();
        }

        let seen: Rc<RefCell<Vec<(u32, u32)>>> = Rc::new(RefCell::new(Vec::new()));
        let seen_for_effect = Rc::clone(&seen);
        let effect = render_effect(
            &cx,
            move |cx| (left.get(cx), right.get(cx)),
            move |_cx, pair: &(u32, u32)| seen_for_effect.borrow_mut().push(*pair),
        );
        cx.flush().unwrap();
        seen.borrow_mut().clear();

        let mut page_value = 0_u32;
        for _ in 0..40 {
            if next_random(&mut state, 5) == 0 {
                page_value += 1;
                set_page.set(&cx, page_value);
            }
            let pumps = next_random(&mut state, 4);
            for _ in 0..pumps {
                runtime.pump();
            }

            let before = (left.get(&cx), right.get(&cx));
            let mark = seen.borrow().len();
            cx.flush().unwrap();
            let after = (left.get(&cx), right.get(&cx));
            let applied: Vec<(u32, u32)> = seen.borrow()[mark..].to_vec();

            assert!(
                after.0 >= before.0 && after.1 >= before.1,
                "seed {seed}: values regressed from {before:?} to {after:?}"
            );
            if before != after {
                checks += 1;
                assert_eq!(
                    applied.last().copied(),
                    Some(after),
                    "seed {seed}: the settled apply must match committed values, got {applied:?}"
                );
                if before.0 != after.0 && before.1 != after.1 {
                    assert_eq!(
                        applied.len(),
                        1,
                        "seed {seed}: two ready results must merge into one apply, got {applied:?}"
                    );
                }
            }
        }
        effect.dispose(&cx);
    }
    eprintln!("merge fuzz: 32 seeds x 40 flushes, {checks} value-changing flushes, 0 violations");
}

#[test]
fn reask_fuzz_is_quiet() {
    let mut checks = 0_u64;
    for seed in 0..32_u64 {
        let mut state = seed.wrapping_mul(0x2545_f491_4f6c_dd1d);
        let polls = next_random(&mut state, 4) as u32;
        let runtime = Runtime::new();
        let cx = runtime.cx();
        let (page, set_page) = create_signal(&cx, 0_u32);
        let data = create_async_memo(&cx, move |cx, _: Option<&u32>| {
            delayed_after(page.get(cx) * 10, polls)
        });
        let boundary = loading_boundary(&cx, None, move |cx| data.get(cx), |_| 0_u32);
        let _ = boundary.get(&cx);
        for _ in 0..8 {
            runtime.pump();
            cx.flush().unwrap();
        }

        let seen: Rc<RefCell<Vec<u32>>> = Rc::new(RefCell::new(Vec::new()));
        let seen_for_effect = Rc::clone(&seen);
        let effect = render_effect(
            &cx,
            move |cx| data.get(cx),
            move |_cx, value: &u32| seen_for_effect.borrow_mut().push(*value),
        );
        cx.flush().unwrap();

        let mut page_value = 0_u32;
        for _ in 0..30 {
            if next_random(&mut state, 2) == 0 {
                refresh(&cx, data);
                assert!(
                    !is_pending(&cx, |cx| data.get(cx)),
                    "seed {seed}: a re-ask must be quiet"
                );
                let mark = seen.borrow().len();
                for _ in 0..(polls + 2) {
                    runtime.pump();
                }
                cx.flush().unwrap();
                assert_eq!(
                    seen.borrow().len(),
                    mark,
                    "seed {seed}: an equal re-ask must not notify readers"
                );
                checks += 1;
            } else {
                page_value += 1;
                set_page.set(&cx, page_value);
                cx.flush().unwrap();
                assert!(
                    is_pending(&cx, |cx| data.get(cx)),
                    "seed {seed}: a dependency change is a new question"
                );
                for _ in 0..(polls + 2) {
                    runtime.pump();
                }
                cx.flush().unwrap();
                checks += 1;
            }
        }
        effect.dispose(&cx);
    }
    eprintln!("re-ask fuzz: 32 seeds x 30 steps, {checks} checks, 0 violations");
}

#[test]
fn optimistic_signal_guesses_inside_actions() {
    let runtime = Runtime::new().into_handle();
    let cx = runtime.cx();
    let todos = OptimisticSignal::new(&cx, vec![1_u32]);
    let seen: Rc<RefCell<Vec<Vec<u32>>>> = Rc::new(RefCell::new(Vec::new()));
    let seen_for_effect = Rc::clone(&seen);
    let effect = render_effect(
        &cx,
        move |cx| todos.get(cx),
        move |_cx, rows: &Vec<u32>| seen_for_effect.borrow_mut().push(rows.clone()),
    );
    cx.flush().unwrap();

    let runtime_for_action = runtime.clone();
    let handle = runtime.action(move || {
        let rt = runtime_for_action.clone();
        async move {
            todos.update_with(&rt.cx(), |rows| rows.push(2));
            if delayed(()).await.is_err() {
                return Err(String::from("async error"));
            }
            todos.commit(&rt.cx(), vec![1, 2, 3]);
            Ok::<(), String>(())
        }
    });

    runtime.pump();
    cx.flush().unwrap();
    assert_eq!(
        seen.borrow().last().cloned(),
        Some(vec![1, 2]),
        "the optimistic handle shows the guess during the action"
    );
    assert_eq!(
        todos.read().committed(&cx),
        Some(vec![1]),
        "the commit is held until settle"
    );
    assert!(is_pending(&cx, |cx| todos.get(cx)));

    for _ in 0..4 {
        runtime.pump();
        cx.flush().unwrap();
    }
    assert_eq!(todos.read().committed(&cx), Some(vec![1, 2, 3]));
    assert_eq!(seen.borrow().last().cloned(), Some(vec![1, 2, 3]));
    assert!(!is_pending(&cx, |cx| todos.get(cx)));
    assert_eq!(handle.result().get(&cx), Some(Ok(())));

    effect.dispose(&cx);
}

#[test]
fn optimistic_signal_failure_reverts_the_guess() {
    let runtime = Runtime::new().into_handle();
    let cx = runtime.cx();
    let todos = OptimisticSignal::new(&cx, vec![1_u32]);
    let runtime_for_action = runtime.clone();
    let handle = runtime.action(move || {
        let rt = runtime_for_action.clone();
        async move {
            todos.update_with(&rt.cx(), |rows| rows.push(2));
            if delayed(()).await.is_err() {
                return Err(String::from("async error"));
            }
            Err::<(), String>(String::from("rejected"))
        }
    });

    runtime.pump();
    cx.flush().unwrap();
    assert_eq!(todos.get(&cx), vec![1, 2]);

    for _ in 0..4 {
        runtime.pump();
        cx.flush().unwrap();
    }
    assert_eq!(todos.get(&cx), vec![1]);
    assert_eq!(todos.read().committed(&cx), Some(vec![1]));
    assert_eq!(
        handle.result().get(&cx),
        Some(Err(String::from("rejected")))
    );
}

#[test]
fn action_settle_callback_writes_outside_the_scope() {
    let runtime = Runtime::new().into_handle();
    let cx = runtime.cx();
    let (error, set_error) = create_signal(&cx, String::new());
    let runtime_for_action = runtime.clone();
    let _handle = runtime.action_with(
        move || {
            let _rt = runtime_for_action.clone();
            async move {
                if delayed(()).await.is_err() {
                    return Err(String::from("async error"));
                }
                Err::<(), String>(String::from("rejected"))
            }
        },
        move |cx, result| {
            if let Err(message) = result {
                set_error.set(cx, message.clone());
            }
        },
    );

    runtime.pump();
    for _ in 0..4 {
        runtime.pump();
        cx.flush().unwrap();
    }
    assert_eq!(
        error.get(&cx),
        "rejected",
        "the settle callback runs mainline after the action scope"
    );
}
