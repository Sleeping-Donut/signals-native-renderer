//! Randomized property tests for the generational arena.
//!
//! No external proptest dependency: the generator is a deterministic xorshift,
//! so failures reproduce from the seed alone. The model is a simple live-value
//! vector; every operation checks the arena against it.

use std::time::{Duration, Instant};

use reactive_core::{Arena, NodeId};

struct Rng(u64);

impl Rng {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, upper: usize) -> usize {
        assert!(upper > 0);
        (self.next() as usize) % upper
    }
}

#[test]
fn arena_matches_a_live_value_model() {
    let mut arena = Arena::new();
    let mut live: Vec<(NodeId, u64)> = Vec::new();
    let mut dead: Vec<NodeId> = Vec::new();
    let mut rng = Rng::new(0x5eed_1234_abcd_9876);

    for step in 0..50_000 {
        match rng.next() % 100 {
            0..=44 => {
                let value = rng.next();
                let handle = arena.insert(value);
                assert!(arena.contains(handle.id()));
                assert_eq!(arena.get(handle.id()), Some(&value));
                assert_eq!(arena.get_mut(handle.id()).map(|value| *value), Some(value));
                live.push((handle.id(), value));
            }
            45..=79 if !live.is_empty() => {
                let index = rng.below(live.len());
                let (id, value) = live.swap_remove(index);
                assert_eq!(arena.remove(id), Some(value));
                assert!(arena.get(id).is_none());
                assert_eq!(arena.remove(id), None);
                dead.push(id);
            }
            80..=94 if !live.is_empty() => {
                let index = rng.below(live.len());
                let (id, value) = live[index];
                assert_eq!(arena.get(id), Some(&value));
            }
            95..=98 if !dead.is_empty() => {
                let index = rng.below(dead.len());
                let id = dead[index];
                assert!(arena.get(id).is_none());
                assert_eq!(arena.remove(id), None);
            }
            _ => {
                arena.clear();
                for (id, _) in live.drain(..) {
                    assert!(arena.get(id).is_none());
                }
                dead.clear();
            }
        }

        assert_eq!(arena.len(), live.len(), "length mismatch at step {step}");
        assert_eq!(arena.is_empty(), live.is_empty());
    }

    // Every dead handle stays dead; every live handle still resolves.
    for id in dead {
        assert!(!arena.contains(id));
    }
    for (id, value) in live {
        assert_eq!(arena.get(id), Some(&value));
    }
}

#[test]
fn reclaimed_slots_never_resurrect_old_handles() {
    let mut arena = Arena::with_capacity(4);
    let mut old = Vec::new();

    for round in 0..1_000 {
        let handle = arena.insert(round);
        let id = handle.id();
        assert_eq!(arena.remove(id), Some(round));
        assert!(!arena.contains(id));
        old.push(id);
    }

    // The arena reused one slot a thousand times; only the newest generation
    // could ever be live, and it was removed above.
    for id in old {
        assert!(!arena.contains(id));
    }
    assert!(arena.is_empty());
}

#[test]
#[cfg_attr(miri, ignore = "timing is meaningless under Miri")]
fn ten_thousand_nodes_allocate_and_free_in_bounded_time() {
    const N: usize = 10_000;

    let start = Instant::now();
    let mut arena = Arena::with_capacity(N);
    let handles: Vec<_> = (0..N).map(|i| arena.insert(i)).collect();
    assert_eq!(arena.len(), N);

    let mut sum = 0usize;
    for handle in &handles {
        sum += *arena.get(handle.id()).expect("live handle");
    }
    assert_eq!(sum, (0..N).sum::<usize>());

    for handle in handles {
        assert!(arena.remove(handle.id()).is_some());
    }
    assert!(arena.is_empty());
    assert_eq!(arena.free_slots(), N);
    let first_pass = start.elapsed();

    // Reuse every slot; this is the path that must stay O(1) with no realloc.
    let start = Instant::now();
    let handles: Vec<_> = (0..N).map(|i| arena.insert(i)).collect();
    assert_eq!(arena.len(), N);
    for handle in handles {
        assert!(arena.remove(handle.id()).is_some());
    }
    let reuse_pass = start.elapsed();

    println!("10k insert+remove: {first_pass:?}; slot reuse: {reuse_pass:?}");
    assert!(
        first_pass < Duration::from_secs(5),
        "10k allocate/free took {first_pass:?}"
    );
    assert!(
        reuse_pass < Duration::from_secs(5),
        "10k reuse took {reuse_pass:?}"
    );
}
