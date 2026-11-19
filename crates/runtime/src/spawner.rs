//! Local spawner contract and the deterministic manual executor.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

/// A future that runs on the UI thread.
///
/// The graph is thread-affine; async graph work must stay on the local
/// executor. Background threads return through continuations instead.
pub type LocalBoxFuture = Pin<Box<dyn Future<Output = ()> + 'static>>;

/// The host's local executor boundary.
///
/// The runtime never owns a thread pool. A host provides a spawner; tests use
/// [`ManualSpawner`].
pub trait Spawner {
    /// Queue a future for the UI thread.
    fn spawn_local(&self, future: LocalBoxFuture);

    /// Ask the host event loop to run a frame soon.
    fn wake_ui(&self);
}

/// A deterministic executor for tests.
///
/// `pump` polls every queued future once. A future that returns `Pending` is
/// re-queued; a future that returns `Ready` is dropped. Futures spawned during
/// a pump are appended and run on a later pump, not invented immediately.
#[derive(Default)]
pub struct ManualSpawner {
    queue: RefCell<VecDeque<LocalBoxFuture>>,
    woke: Cell<bool>,
}

impl ManualSpawner {
    /// Create an empty manual executor.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of futures waiting for the next pump.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.queue.borrow().len()
    }

    /// Poll the current queue once. Returns how many futures completed.
    ///
    /// Pending futures are kept in the queue for the next call.
    pub fn pump(&self) -> usize {
        // Take the queue so a future may spawn while it is polled without
        // re-entering the RefCell borrow.
        let mut tasks = std::mem::take(&mut *self.queue.borrow_mut());
        let mut requeue = VecDeque::new();
        let mut completed = 0;

        while let Some(mut future) = tasks.pop_front() {
            let waker = Waker::noop();
            let mut context = Context::from_waker(waker);
            match future.as_mut().poll(&mut context) {
                Poll::Ready(()) => completed += 1,
                Poll::Pending => requeue.push_back(future),
            }
        }

        let mut queue = self.queue.borrow_mut();
        for future in requeue {
            queue.push_back(future);
        }
        completed
    }

    /// Pump until a pump completes nothing or the queue is empty.
    ///
    /// This is for tests with self-waking futures; a future that repeatedly
    /// makes progress and spawns a replacement can keep this running.
    pub fn run_until_stalled(&self) -> usize {
        let mut completed = 0;
        loop {
            let done = self.pump();
            completed += done;
            if done == 0 {
                break;
            }
        }
        completed
    }

    /// Take and clear the wake flag.
    pub fn take_wake(&self) -> bool {
        self.woke.replace(false)
    }
}

impl Spawner for ManualSpawner {
    fn spawn_local(&self, future: LocalBoxFuture) {
        self.queue.borrow_mut().push_back(future);
    }

    fn wake_ui(&self) {
        self.woke.set(true);
    }
}

impl<S: Spawner + ?Sized> Spawner for std::rc::Rc<S> {
    fn spawn_local(&self, future: LocalBoxFuture) {
        (**self).spawn_local(future);
    }

    fn wake_ui(&self) {
        (**self).wake_ui();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn pump_completes_a_ready_future() {
        let spawner = ManualSpawner::new();
        let state = Rc::new(Cell::new(0_u32));
        let task_state = Rc::clone(&state);

        spawner.spawn_local(Box::pin(async move {
            task_state.set(task_state.get() + 1);
        }));

        assert_eq!(spawner.pending(), 1);
        assert_eq!(spawner.pump(), 1);
        assert_eq!(state.get(), 1);
        assert_eq!(spawner.pending(), 0);
    }

    #[test]
    fn pending_future_is_requeued_and_runs_on_the_next_pump() {
        let spawner = ManualSpawner::new();
        let polls = Rc::new(Cell::new(0_u32));
        let task_polls = Rc::clone(&polls);

        spawner.spawn_local(Box::pin(std::future::poll_fn(move |_cx| {
            let count = task_polls.get();
            task_polls.set(count + 1);
            if count == 0 {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })));

        assert_eq!(spawner.pump(), 0);
        assert_eq!(polls.get(), 1);
        assert_eq!(spawner.pending(), 1);

        assert_eq!(spawner.pump(), 1);
        assert_eq!(polls.get(), 2);
        assert_eq!(spawner.pending(), 0);
    }

    #[test]
    fn spawn_during_pump_runs_on_the_next_pump() {
        let spawner = Rc::new(ManualSpawner::new());
        let state = Rc::new(Cell::new(0_u32));
        let task_spawner = Rc::clone(&spawner);
        let first_state = Rc::clone(&state);
        let second_state = Rc::clone(&state);

        spawner.spawn_local(Box::pin(async move {
            first_state.set(1);
            task_spawner.spawn_local(Box::pin(async move {
                second_state.set(2);
            }));
        }));

        assert_eq!(spawner.pump(), 1);
        assert_eq!(state.get(), 1);
        assert_eq!(spawner.pending(), 1);

        assert_eq!(spawner.pump(), 1);
        assert_eq!(state.get(), 2);
    }

    #[test]
    fn wake_flag_is_taken_once() {
        let spawner = ManualSpawner::new();
        assert!(!spawner.take_wake());
        spawner.wake_ui();
        assert!(spawner.take_wake());
        assert!(!spawner.take_wake());
    }
}
