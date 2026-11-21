//! `is_pending` is observable state, not a diagnostic snapshot.

use std::cell::{Cell, RefCell};
use std::future::poll_fn;
use std::pin::Pin;
use std::rc::Rc;
use std::task::Poll;

use reactive_core::{
    create_async_memo, create_memo, create_signal, is_pending, loading_boundary, render_effect,
    AsyncError, Runtime,
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
fn pending_status_reruns_readers_on_flight_start_and_commit() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (page, set_page) = create_signal(&cx, 0_u32);
    let data = create_async_memo(&cx, move |cx, _: Option<&u32>| {
        let page = page.get(cx);
        delayed(page)
    });
    let pending = create_memo(&cx, move |cx, _| is_pending(cx, move |cx| data.get(cx)));
    let boundary = loading_boundary(&cx, None, move |cx| data.get(cx), |_| 0);

    assert_eq!(boundary.get(&cx), 0);
    for _ in 0..6 {
        runtime.pump();
        cx.flush().unwrap();
    }
    assert_eq!(boundary.get(&cx), 0);
    assert!(!pending.get(&cx));

    set_page.set(&cx, 1);
    cx.flush().unwrap();
    assert!(is_pending(&cx, |cx| data.get(cx)));
    assert!(
        pending.get(&cx),
        "readers of is_pending must rerun when a flight starts"
    );

    for _ in 0..6 {
        runtime.pump();
        cx.flush().unwrap();
    }
    assert!(!is_pending(&cx, |cx| data.get(cx)));
    assert!(
        !pending.get(&cx),
        "readers of is_pending must rerun when the result commits"
    );
}

#[test]
fn display_pass_shows_the_verdict_during_a_hold() {
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
    assert_eq!(boundary.get(&cx), 0, "the initial answer is committed");

    let seen: Rc<RefCell<Vec<(u32, bool)>>> = Rc::new(RefCell::new(Vec::new()));
    let seen_for_effect = Rc::clone(&seen);
    let effect = render_effect(
        &cx,
        move |cx| (data.get(cx), is_pending(cx, move |cx| data.get(cx))),
        move |_cx, (value, pending): &(u32, bool)| {
            seen_for_effect.borrow_mut().push((*value, *pending));
        },
    );
    cx.flush().unwrap();
    assert_eq!(
        seen.borrow().last().copied(),
        Some((0, false)),
        "initial read is committed and idle"
    );

    seen.borrow_mut().clear();
    set_page.set(&cx, 1);
    cx.flush().unwrap();
    assert_eq!(
        seen.borrow().as_slice(),
        &[(0, true)],
        "a display pass applies the old value with the pending verdict during the hold"
    );

    for _ in 0..6 {
        runtime.pump();
        cx.flush().unwrap();
    }
    assert_eq!(
        seen.borrow().last().copied(),
        Some((1, false)),
        "the committed value follows once the flight lands"
    );

    effect.dispose(&cx);
}
