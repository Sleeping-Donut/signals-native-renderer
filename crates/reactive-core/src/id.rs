//! Stable node identity and typed handles.

use core::fmt;
use core::hash::{Hash, Hasher};
use core::marker::PhantomData;

/// A stable graph node identity.
///
/// The generation is bumped when the slot is freed, so a stale ID never
/// resolves to a reused slot. `NodeId::INVALID` is the sentinel; arenas reject
/// it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId {
    index: u32,
    generation: u32,
}

impl NodeId {
    /// The invalid sentinel.
    pub const INVALID: Self = Self {
        index: u32::MAX,
        generation: 0,
    };

    /// Construct an ID from a slot index and generation.
    #[must_use]
    pub const fn new(index: u32, generation: u32) -> Self {
        Self { index, generation }
    }

    /// The slot index.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.index
    }

    /// The generation that makes this ID unique among reuses of the slot.
    #[must_use]
    pub const fn generation(self) -> u32 {
        self.generation
    }

    /// Whether this is not the invalid sentinel.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.index != u32::MAX
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NodeId")
            .field("index", &self.index)
            .field("generation", &self.generation)
            .finish()
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "node[{}#{}]", self.index, self.generation)
    }
}

/// A typed, `Copy` handle to a value stored in an arena.
///
/// The handle is inert: it does not keep the value alive and does not borrow
/// the arena. Resolve it through the arena that produced it.
pub struct Handle<T> {
    id: NodeId,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Handle<T> {
    /// Wrap an ID. The caller is responsible for using the matching arena.
    #[must_use]
    pub const fn from_id(id: NodeId) -> Self {
        Self {
            id,
            _marker: PhantomData,
        }
    }

    /// The invalid handle.
    #[must_use]
    pub const fn invalid() -> Self {
        Self::from_id(NodeId::INVALID)
    }

    /// The underlying ID.
    #[must_use]
    pub const fn id(self) -> NodeId {
        self.id
    }
}

impl<T> Clone for Handle<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Handle<T> {}

impl<T> PartialEq for Handle<T> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl<T> Eq for Handle<T> {}

impl<T> Hash for Handle<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl<T> fmt::Debug for Handle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Handle").field("id", &self.id).finish()
    }
}

/// Why a handle lookup failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessError {
    /// The slot was freed (or the handle is from a previous generation).
    Disposed,
    /// The slot is live but holds a different node kind.
    WrongKind,
}

impl fmt::Display for AccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disposed => f.write_str("node is disposed"),
            Self::WrongKind => f.write_str("node has a different kind"),
        }
    }
}

impl std::error::Error for AccessError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_node_is_not_valid() {
        assert!(!NodeId::INVALID.is_valid());
        assert!(NodeId::new(0, 0).is_valid());
    }

    #[test]
    fn handle_equality_uses_the_id() {
        let a = Handle::<u32>::from_id(NodeId::new(1, 2));
        let b = Handle::<u32>::from_id(NodeId::new(1, 2));
        let c = Handle::<u32>::from_id(NodeId::new(1, 3));
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
