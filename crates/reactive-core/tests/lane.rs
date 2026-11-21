//! Lane reads: `latest`, display-only guesses, quiet `refresh`, and merge.

use std::cell::{Cell, RefCell};
use std::future::poll_fn;
use std::pin::Pin;
use std::rc::Rc;
use std::task::Poll;

use reactive_core::{
    clear_guess, create_async_memo, create_memo, create_signal, guess, is_pending, latest,
    loading_boundary, refresh, render_effect, AsyncError, LatestError, Runtime,
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
