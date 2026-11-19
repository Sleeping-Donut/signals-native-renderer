//! A generational arena with a free list.
//!
//! One arena per value type (nodes, edges, owners, ...). Slots are never moved,
//! so an ID is a slot index plus the slot's generation. Removing a value bumps
//! the generation and pushes the index onto the free list; a stale ID can never
//! resolve to the reused slot.

use crate::{Handle, NodeId};

#[derive(Debug)]
struct Slot<T> {
    generation: u32,
    value: Option<T>,
}

/// A generational arena.
///
/// `insert` is O(1) amortized, `remove`/`get` are O(1), and the free list is a
/// `Vec<u32>` reused LIFO. The arena never shrinks its slot vector; a slot whose
/// generation reaches `u32::MAX` is left as a permanent hole rather than reused.
#[derive(Debug)]
pub struct Arena<T> {
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
    len: usize,
}

impl<T> Arena<T> {
    /// Create an empty arena.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            len: 0,
        }
    }

    /// Create an empty arena with space for at least `capacity` values.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            slots: Vec::with_capacity(capacity),
            free: Vec::new(),
            len: 0,
        }
    }

    /// Insert a value and return its typed handle.
    ///
    /// # Panics
    ///
    /// Panics if the slot index would exceed `u32::MAX` (an arena that large is
    /// not a real UI graph).
    pub fn insert(&mut self, value: T) -> Handle<T> {
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            debug_assert!(slot.value.is_none(), "free-list slot was occupied");
            slot.value = Some(value);
            self.len += 1;
            return Handle::from_id(NodeId::new(index, slot.generation));
        }

        let index = self.slots.len();
        assert!(
            index < u32::MAX as usize,
            "arena slot index exceeds u32::MAX"
        );
        self.slots.push(Slot {
            generation: 0,
            value: Some(value),
        });
        self.len += 1;
        Handle::from_id(NodeId::new(index as u32, 0))
    }

    /// Remove a value. Returns `None` for a stale, foreign, or already-removed ID.
    pub fn remove(&mut self, id: NodeId) -> Option<T> {
        if !id.is_valid() {
            return None;
        }

        let slot = self.slots.get_mut(id.index() as usize)?;
        if slot.generation != id.generation() {
            return None;
        }

        let value = slot.value.take()?;
        self.len -= 1;

        // Do not reuse a slot whose generation has wrapped: reusing it would
        // resurrect an old handle (ABA).
        if slot.generation != u32::MAX {
            slot.generation += 1;
            self.free.push(id.index());
        }

        Some(value)
    }

    /// Get a shared reference. Returns `None` for a stale or removed ID.
    #[must_use]
    pub fn get(&self, id: NodeId) -> Option<&T> {
        if !id.is_valid() {
            return None;
        }
        let slot = self.slots.get(id.index() as usize)?;
        if slot.generation != id.generation() {
            return None;
        }
        slot.value.as_ref()
    }

    /// Get a mutable reference. Returns `None` for a stale or removed ID.
    #[must_use]
    pub fn get_mut(&mut self, id: NodeId) -> Option<&mut T> {
        if !id.is_valid() {
            return None;
        }
        let slot = self.slots.get_mut(id.index() as usize)?;
        if slot.generation != id.generation() {
            return None;
        }
        slot.value.as_mut()
    }

    /// Whether the ID currently resolves.
    #[must_use]
    pub fn contains(&self, id: NodeId) -> bool {
        self.get(id).is_some()
    }

    /// Number of live values.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the arena holds no values.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Allocated slot capacity, including free slots.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.slots.capacity()
    }

    /// Number of reusable slots waiting on the free list.
    #[must_use]
    pub fn free_slots(&self) -> usize {
        self.free.len()
    }

    /// Remove every value, invalidating all handles. Slots remain allocated for
    /// reuse.
    pub fn clear(&mut self) {
        let Self { slots, free, len } = self;
        for (index, slot) in slots.iter_mut().enumerate() {
            if slot.value.take().is_none() {
                continue;
            }
            *len -= 1;
            if slot.generation != u32::MAX {
                slot.generation += 1;
                free.push(index as u32);
            }
        }
        debug_assert_eq!(*len, 0);
    }

    /// Iterate live values with their handles.
    pub fn iter(&self) -> impl Iterator<Item = (Handle<T>, &T)> + '_ {
        self.slots.iter().enumerate().filter_map(|(index, slot)| {
            slot.value.as_ref().map(|value| {
                (
                    Handle::from_id(NodeId::new(index as u32, slot.generation)),
                    value,
                )
            })
        })
    }
}

impl<T> Default for Arena<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_get_remove() {
        let mut arena = Arena::new();
        let handle = arena.insert("a");
        assert_eq!(arena.len(), 1);
        assert_eq!(arena.get(handle.id()), Some(&"a"));
        assert!(arena.contains(handle.id()));

        assert_eq!(arena.remove(handle.id()), Some("a"));
        assert_eq!(arena.len(), 0);
        assert_eq!(arena.get(handle.id()), None);
        assert_eq!(arena.remove(handle.id()), None);
    }

    #[test]
    fn free_list_reuses_the_slot_with_a_new_generation() {
        let mut arena = Arena::new();
        let first = arena.insert(1_u32);
        assert_eq!(arena.remove(first.id()), Some(1));
        assert_eq!(arena.free_slots(), 1);

        let second = arena.insert(2_u32);
        assert_eq!(second.id().index(), first.id().index());
        assert_ne!(second.id().generation(), first.id().generation());
        assert!(!arena.contains(first.id()));
        assert_eq!(arena.get(second.id()), Some(&2));
    }

    #[test]
    fn clear_invalidates_handles_and_keeps_capacity() {
        let mut arena = Arena::with_capacity(8);
        let handles: Vec<_> = (0..5).map(|i| arena.insert(i)).collect();
        let capacity = arena.capacity();

        arena.clear();
        assert!(arena.is_empty());
        for handle in handles {
            assert!(!arena.contains(handle.id()));
            assert_eq!(arena.remove(handle.id()), None);
        }
        assert_eq!(arena.capacity(), capacity);
        assert_eq!(arena.free_slots(), 5);
    }

    #[test]
    fn iter_yields_live_values_only() {
        let mut arena = Arena::new();
        let a = arena.insert("a");
        let b = arena.insert("b");
        let c = arena.insert("c");
        arena.remove(b.id());

        let values: Vec<_> = arena.iter().map(|(_, value)| *value).collect();
        assert_eq!(values, vec!["a", "c"]);
        let ids: Vec<_> = arena.iter().map(|(handle, _)| handle.id()).collect();
        assert!(ids.contains(&a.id()));
        assert!(ids.contains(&c.id()));
        assert!(!ids.contains(&b.id()));
    }

    #[test]
    fn invalid_id_never_resolves() {
        let arena = Arena::<u32>::new();
        assert_eq!(arena.get(NodeId::INVALID), None);
        assert!(!arena.contains(NodeId::INVALID));
    }
}
