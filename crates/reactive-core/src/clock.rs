//! Deterministic virtual clock for async tests and manual frames.

use std::cell::Cell;
use std::future::Future;
use std::rc::Rc;
use std::task::Poll;

/// A millisecond clock advanced explicitly by the host or test.
#[derive(Clone, Debug, Default)]
pub struct VirtualClock {
    now: Rc<Cell<u64>>,
}

impl VirtualClock {
    /// Create a clock at zero.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Current virtual time in milliseconds.
    #[must_use]
    pub fn now(&self) -> u64 {
        self.now.get()
    }

    /// Advance the clock.
    pub fn advance(&self, millis: u64) {
        self.now.set(self.now.get().saturating_add(millis));
    }

    /// A future that completes once the clock reaches `millis` from now.
    pub fn sleep(&self, millis: u64) -> impl Future<Output = ()> {
        let clock = self.clone();
        let deadline = self.now().saturating_add(millis);
        std::future::poll_fn(move |_| {
            if clock.now() >= deadline {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
    }
}
