//! Frame clock and runtime shell.

use crate::Spawner;

/// Monotonic frame identifier.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FrameId(u64);

impl FrameId {
    /// The frame before the first tick.
    pub const ZERO: Self = Self(0);

    /// The raw frame number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

/// The runtime shell.
///
/// Phase 0 owns only the frame clock and the host's local spawner. Phase 1 adds
/// the graph arenas and the frame phases around them.
pub struct Runtime<S: Spawner> {
    frame: FrameId,
    spawner: S,
}

impl<S: Spawner> Runtime<S> {
    /// Create a runtime with the host's local spawner.
    pub fn new(spawner: S) -> Self {
        Self {
            frame: FrameId::ZERO,
            spawner,
        }
    }

    /// The current frame.
    #[must_use]
    pub const fn frame(&self) -> FrameId {
        self.frame
    }

    /// The host spawner.
    #[must_use]
    pub fn spawner(&self) -> &S {
        &self.spawner
    }

    /// Advance to the next frame. Phase 1 will surround this with pump, flush,
    /// apply, layout, scene, and present.
    pub fn advance_frame(&mut self) -> FrameId {
        self.frame = self.frame.next();
        self.frame
    }
}

impl<S: Spawner> std::fmt::Debug for Runtime<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime")
            .field("frame", &self.frame)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ManualSpawner;

    #[test]
    fn frames_advance_monotonically() {
        let mut runtime = Runtime::new(ManualSpawner::new());
        assert_eq!(runtime.frame(), FrameId::ZERO);
        assert_eq!(runtime.advance_frame().get(), 1);
        assert_eq!(runtime.advance_frame().get(), 2);
    }

    #[test]
    fn runtime_exposes_the_spawner() {
        let runtime = Runtime::new(ManualSpawner::new());
        runtime.spawner().wake_ui();
        assert!(runtime.spawner().take_wake());
    }
}
