//! Local spawner boundary and the deterministic manual executor.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

/// A future that runs on the UI thread.
pub type LocalBoxFuture = Pin<Box<dyn Future<Output = ()> + 'static>>;

/// The host's local executor boundary.
///
/// The graph calls `spawn_local` for async flights. A host implementation may
/// rely on its own event loop; [`ManualSpawner`] is the deterministic fallback
/// used by the runtime and tests.
pub trait Spawner {
    /// Queue a future for the UI thread.
    fn spawn_local(&self, future: LocalBoxFuture);

    /// Ask the host event loop to run a frame soon.
    fn wake_ui(&self);

    /// Poll queued tasks once. Hosts with their own loop may leave this as a
    /// no-op; [`ManualSpawner`] overrides it.
    fn pump(&self) {}
}

/// A deterministic executor for tests and the default graph runtime.
///
/// `pump` polls every queued task once. A task that returns `Pending` is
/// re-queued; a task that returns `Ready` is dropped. Tasks spawned during a
/// pump run on a later pump.
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

    /// Number of tasks waiting for the next pump.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.queue.borrow().len()
    }

    /// Poll the current queue once. Returns how many tasks completed.
    pub fn pump(&self) -> usize {
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

    /// Pump until a pump completes nothing.
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

    fn pump(&self) {
        let _ = ManualSpawner::pump(self);
    }
}

impl<S: Spawner + ?Sized> Spawner for std::rc::Rc<S> {
    fn spawn_local(&self, future: LocalBoxFuture) {
        (**self).spawn_local(future);
    }

    fn wake_ui(&self) {
        (**self).wake_ui();
    }

    fn pump(&self) {
        (**self).pump();
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
    fn pending_future_is_requeued() {
        let spawner = ManualSpawner::new();
        let polls = Rc::new(Cell::new(0_u32));
        let task_polls = Rc::clone(&polls);

        spawner.spawn_local(Box::pin(std::future::poll_fn(move |_| {
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
    fn wake_flag_is_taken_once() {
        let spawner = ManualSpawner::new();
        assert!(!spawner.take_wake());
        spawner.wake_ui();
        assert!(spawner.take_wake());
        assert!(!spawner.take_wake());
    }
}
