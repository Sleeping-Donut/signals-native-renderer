//! Phase 2 async tests: flights, holds, boundaries, key remounts, and
//! supersession.

use std::cell::{Cell, RefCell};
use std::future::{poll_fn, Future};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::pin::Pin;
use std::rc::Rc;
use std::task::Poll;

use reactive_core::{
    create_async_memo, create_derived_signal, create_signal, error_boundary, loading_boundary,
    AsyncError, LocalBoxFuture, ManualSpawner, ReaderClass, Runtime, Spawner,
};

type Fut<T> = Pin<Box<dyn Future<Output = Result<T, AsyncError>>>>;

fn delayed_with_counter<T: Clone + 'static>(
    value: T,
    polls: Rc<Cell<usize>>,
    ready_at: usize,
) -> Fut<T> {
    Box::pin(poll_fn(move |_| {
        let count = polls.get();
        polls.set(count + 1);
        if count < ready_at {
            Poll::Pending
        } else {
            Poll::Ready(Ok(value.clone()))
        }
    }))
}

#[test]
fn first_load_shows_fallback_then_content() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (page, _set_page) = create_signal(&cx, 1_u64);

    let polls = Rc::new(Cell::new(0));
    let data = create_async_memo(&cx, move |cx, _| {
        let page = page.get(cx);
        delayed_with_counter(format!("data-{page}"), Rc::clone(&polls), 1)
    });
    let boundary = loading_boundary(
        &cx,
        None,
        move |cx| data.get(cx),
        |_cx| "loading".to_string(),
    );

    assert_eq!(boundary.get(&cx), "loading");
    assert!(
        !boundary.is_pending(&cx),
        "first load is loading, not pending"
    );
    runtime.pump();
    cx.flush().unwrap();
    runtime.pump();
    cx.flush().unwrap();
    assert_eq!(boundary.get(&cx), "data-1");
}

#[test]
fn navigation_keeps_old_content_until_the_new_answer_is_ready() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (page, set_page) = create_signal(&cx, 1_u64);
    let requests = Rc::new(RefCell::new(Vec::new()));
    let requests_for_effect = Rc::clone(&requests);

    let data = create_async_memo(&cx, move |cx, _| {
        let page = page.get(cx);
        requests_for_effect.borrow_mut().push(page);
        delayed_with_counter(format!("data-{page}"), Rc::new(Cell::new(0)), 1)
    });
    let boundary = loading_boundary(
        &cx,
        None,
        move |cx| data.get(cx),
        |_cx| "loading".to_string(),
    );

    assert_eq!(boundary.get(&cx), "loading");
    runtime.pump();
    cx.flush().unwrap();
    // Still loading: the future needs a second poll.
    assert_eq!(boundary.get(&cx), "loading");

    runtime.pump();
    cx.flush().unwrap();
    assert_eq!(boundary.get(&cx), "data-1");

    set_page.set(&cx, 2);
    cx.flush().unwrap();
    // The new question is in flight; the old page stays whole.
    assert!(data.is_pending(&cx));
    assert_eq!(boundary.get(&cx), "data-1");

    runtime.pump();
    cx.flush().unwrap();
    // Page 2 has one more poll to go.
    assert_eq!(boundary.get(&cx), "data-1");

    runtime.pump();
    cx.flush().unwrap();
    assert_eq!(boundary.get(&cx), "data-2");
    assert_eq!(&*requests.borrow(), &[1, 2]);
}

#[test]
fn two_async_reads_do_not_show_a_partial_frame() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (page, set_page) = create_signal(&cx, 1_u64);

    let a = create_async_memo(&cx, move |cx, _| {
        let page = page.get(cx);
        delayed_with_counter(format!("a-{page}"), Rc::new(Cell::new(0)), 1)
    });
    let b = create_async_memo(&cx, move |cx, _| {
        let page = page.get(cx);
        let ready_at = if page == 1 { 0 } else { 2 };
        delayed_with_counter(format!("b-{page}"), Rc::new(Cell::new(0)), ready_at)
    });
    let boundary = loading_boundary(
        &cx,
        None,
        move |cx| format!("{} + {}", a.get(cx), b.get(cx)),
        |_cx| "loading".to_string(),
    );

    assert_eq!(boundary.get(&cx), "loading");
    // A can land first; B is still pending.
    runtime.pump();
    cx.flush().unwrap();
    assert_eq!(boundary.get(&cx), "loading");

    runtime.pump();
    cx.flush().unwrap();
    assert_eq!(boundary.get(&cx), "loading");

    runtime.pump();
    cx.flush().unwrap();
    assert_eq!(boundary.get(&cx), "a-1 + b-1");

    set_page.set(&cx, 2);
    cx.flush().unwrap();

    runtime.pump();
    cx.flush().unwrap();
    assert_eq!(
        boundary.get(&cx),
        "a-1 + b-1",
        "A2 is ready but B2 is not; the frame stays whole"
    );

    runtime.pump();
    cx.flush().unwrap();
    assert_eq!(boundary.get(&cx), "a-1 + b-1");

    runtime.pump();
    cx.flush().unwrap();
    assert_eq!(boundary.get(&cx), "a-2 + b-2");
}

#[test]
fn key_change_remounts_to_the_fallback() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (key, set_key) = create_signal(&cx, 1_u64);
    let polls = Rc::new(Cell::new(0));
    let data = create_async_memo(&cx, move |cx, _| {
        let key = key.get(cx);
        delayed_with_counter(format!("data-{key}"), Rc::clone(&polls), 1)
    });
    let boundary = loading_boundary(
        &cx,
        Some(key),
        move |cx| data.get(cx),
        |_cx| "loading".to_string(),
    );

    assert_eq!(boundary.get(&cx), "loading");
    runtime.pump();
    cx.flush().unwrap();
    runtime.pump();
    cx.flush().unwrap();
    assert_eq!(boundary.get(&cx), "data-1");

    set_key.set(&cx, 2);
    cx.flush().unwrap();
    assert_eq!(
        boundary.get(&cx),
        "loading",
        "a key change arms the boundary again"
    );

    runtime.pump();
    cx.flush().unwrap();
    runtime.pump();
    cx.flush().unwrap();
    assert_eq!(boundary.get(&cx), "data-2");
}

#[test]
fn superseded_flights_are_dropped_and_teardown_runs_once() {
    #[derive(Clone)]
    struct Teardown(Rc<Cell<usize>>);
    impl Drop for Teardown {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (page, set_page) = create_signal(&cx, 1_u64);
    let teardowns = Rc::new(Cell::new(0));
    let requests = Rc::new(RefCell::new(Vec::new()));
    let requests_for_effect = Rc::clone(&requests);

    let data = create_async_memo(&cx, {
        let teardowns = Rc::clone(&teardowns);
        move |cx, _| {
            let page = page.get(cx);
            requests_for_effect.borrow_mut().push(page);
            let teardown = Teardown(Rc::clone(&teardowns));
            let polls = Rc::new(Cell::new(0));
            Box::pin(async move {
                let _teardown = teardown;
                let value = poll_fn(move |_| {
                    let count = polls.get();
                    polls.set(count + 1);
                    if count < 1 {
                        Poll::Pending
                    } else {
                        Poll::Ready(format!("data-{page}"))
                    }
                })
                .await;
                Ok(value)
            })
        }
    });
    let boundary = loading_boundary(
        &cx,
        None,
        move |cx| data.get(cx),
        |_cx| "loading".to_string(),
    );

    assert_eq!(boundary.get(&cx), "loading");
    runtime.pump();
    cx.flush().unwrap();
    assert_eq!(teardowns.get(), 0);

    set_page.set(&cx, 2);
    cx.flush().unwrap();
    assert_eq!(
        teardowns.get(),
        1,
        "the superseded future was dropped exactly once"
    );

    runtime.pump();
    cx.flush().unwrap();
    runtime.pump();
    cx.flush().unwrap();
    assert_eq!(boundary.get(&cx), "data-2");
    assert_eq!(&*requests.borrow(), &[1, 2]);
}

#[test]
fn sync_derived_over_async_reads_holds_without_a_new_constructor() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (page, set_page) = create_signal(&cx, 1_u64);
    let data = create_async_memo(&cx, move |cx, _| {
        let page = page.get(cx);
        delayed_with_counter(format!("data-{page}"), Rc::new(Cell::new(0)), 1)
    });

    // A synchronous writable derived signal over an async memo: no async
    // constructor and no separate node kind.
    let (state, _set_state) = create_derived_signal(&cx, move |cx, _| data.get(cx));

    let boundary = loading_boundary(
        &cx,
        None,
        move |cx| state.get(cx),
        |_cx| "loading".to_string(),
    );

    assert_eq!(boundary.get(&cx), "loading");
    runtime.pump();
    cx.flush().unwrap();
    runtime.pump();
    cx.flush().unwrap();
    assert_eq!(boundary.get(&cx), "data-1");

    set_page.set(&cx, 2);
    cx.flush().unwrap();
    assert!(state.is_pending(&cx), "the sync derivation is pending");
    assert_eq!(boundary.get(&cx), "data-1");

    runtime.pump();
    cx.flush().unwrap();
    runtime.pump();
    cx.flush().unwrap();
    assert_eq!(boundary.get(&cx), "data-2");
}

#[test]
fn error_boundary_renders_the_error_fallback_and_recovers() {
    #[derive(Debug)]
    struct Boom;

    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (fails, set_fails) = create_signal(&cx, true);
    let boundary = error_boundary(
        &cx,
        move |cx| {
            if fails.get(cx) {
                Err(Boom)
            } else {
                Ok("content".to_string())
            }
        },
        |_cx, error: &Boom| format!("error:{error:?}"),
    );

    assert_eq!(boundary.get(&cx), "error:Boom");
    set_fails.set(&cx, false);
    cx.flush().unwrap();
    assert_eq!(boundary.get(&cx), "content");
}

#[test]
fn virtual_clock_drives_async_without_poll_counters() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let clock = cx.clock();
    let data = create_async_memo(&cx, move |_cx, _| {
        let clock = clock.clone();
        Box::pin(async move {
            clock.sleep(100).await;
            Ok("ready".to_string())
        })
    });
    let boundary = loading_boundary(
        &cx,
        None,
        move |cx| data.get(cx),
        |_cx| "loading".to_string(),
    );

    assert_eq!(boundary.get(&cx), "loading");
    runtime.pump();
    cx.flush().unwrap();
    assert_eq!(boundary.get(&cx), "loading");

    runtime.advance_time(99);
    runtime.pump();
    cx.flush().unwrap();
    assert_eq!(boundary.get(&cx), "loading");

    runtime.advance_time(1);
    runtime.pump();
    cx.flush().unwrap();
    assert_eq!(boundary.get(&cx), "ready");
}

#[test]
fn runtime_uses_the_configured_spawner_for_flights() {
    struct CountingSpawner {
        inner: ManualSpawner,
        spawned: Rc<Cell<usize>>,
    }

    impl Spawner for CountingSpawner {
        fn spawn_local(&self, future: LocalBoxFuture) {
            self.spawned.set(self.spawned.get() + 1);
            self.inner.spawn_local(future);
        }

        fn wake_ui(&self) {
            self.inner.wake_ui();
        }

        fn pump(&self) {
            self.inner.pump();
        }
    }

    let spawned = Rc::new(Cell::new(0));
    let runtime = Runtime::with_spawner(CountingSpawner {
        inner: ManualSpawner::new(),
        spawned: Rc::clone(&spawned),
    });
    let cx = runtime.cx();
    let data = create_async_memo(&cx, move |_cx, _| {
        delayed_with_counter("spawned".to_string(), Rc::new(Cell::new(0)), 0)
    });
    let boundary = loading_boundary(
        &cx,
        None,
        move |cx| data.get(cx),
        |_cx| "loading".to_string(),
    );

    assert_eq!(boundary.get(&cx), "loading");
    assert_eq!(spawned.get(), 1, "the graph spawned a task on the host");
    runtime.pump();
    cx.flush().unwrap();
    assert_eq!(boundary.get(&cx), "spawned");
}

#[test]
fn reader_classes_separate_display_and_derive_reads() {
    let runtime = Runtime::new();
    let cx = runtime.cx();
    let (page, set_page) = create_signal(&cx, 1_u64);
    let data = create_async_memo(&cx, move |cx, _| {
        let page = page.get(cx);
        delayed_with_counter(format!("data-{page}"), Rc::new(Cell::new(0)), 1)
    });

    // Start the first flight with a display read; there is no committed value
    // yet, so the read suspends. The boundary normally catches this.
    let _ = catch_unwind(AssertUnwindSafe(|| {
        cx.with_reader_class(ReaderClass::Display, || data.get(&cx))
    }));
    runtime.pump();
    cx.flush().unwrap();
    runtime.pump();
    cx.flush().unwrap();
    assert_eq!(
        cx.with_reader_class(ReaderClass::Display, || data.get(&cx)),
        "data-1"
    );

    set_page.set(&cx, 2);
    cx.flush().unwrap();

    assert_eq!(
        cx.with_reader_class(ReaderClass::Display, || data.get(&cx)),
        "data-1",
        "display reads keep the committed value"
    );
    let derive = catch_unwind(AssertUnwindSafe(|| {
        cx.with_reader_class(ReaderClass::Derive, || data.get(&cx))
    }));
    assert!(derive.is_err(), "derive reads join the hold");

    runtime.pump();
    cx.flush().unwrap();
    runtime.pump();
    cx.flush().unwrap();
    assert_eq!(
        cx.with_reader_class(ReaderClass::Derive, || data.get(&cx)),
        "data-2"
    );
}
