use std::collections::VecDeque;

use accesskit::{ActionRequest, TreeUpdate};

/// Platform-adapter boundary for AccessKit updates and action requests.
///
/// A platform backend receives [`TreeUpdate`] values from the window driver and
/// forwards action requests back through [`ActionRequest`]. The adapter keeps
/// the latest tree for tests and headless hosts.
#[derive(Default)]
pub struct AccessKitAdapter {
    update: Option<TreeUpdate>,
    updates: u64,
    actions: VecDeque<ActionRequest>,
}

impl AccessKitAdapter {
    /// Create an empty adapter.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply a tree update.
    pub fn apply(&mut self, update: TreeUpdate) {
        self.update = Some(update);
        self.updates += 1;
    }

    /// The most recent tree update.
    #[must_use]
    pub fn last_update(&self) -> Option<&TreeUpdate> {
        self.update.as_ref()
    }

    /// Number of applied updates.
    #[must_use]
    pub fn update_count(&self) -> u64 {
        self.updates
    }

    /// Queue an action request from the platform adapter.
    pub fn push_action(&mut self, request: ActionRequest) {
        self.actions.push_back(request);
    }

    /// Drain queued action requests.
    pub fn drain_actions(&mut self) -> Vec<ActionRequest> {
        self.actions.drain(..).collect()
    }
}
