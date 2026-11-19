//! Phase 0 arena benchmark.
//!
//! `harness = false` in Cargo.toml keeps this a plain binary, so `cargo bench`
//! works on stable Rust without an external benchmark framework. It reports
//! insert, get, and remove throughput for a full 1M-slot fill and drain.

use std::hint::black_box;
use std::time::Instant;

use reactive_core::Arena;

const N: usize = 1_000_000;

fn per_op(ns: u128, ops: usize) -> f64 {
    ns as f64 / ops as f64
}

fn main() {
    let mut arena = Arena::with_capacity(N);

    let start = Instant::now();
    let handles: Vec<_> = (0..N).map(|i| arena.insert(i)).collect();
    let insert = start.elapsed();

    let start = Instant::now();
    let mut checksum = 0usize;
    for handle in &handles {
        checksum += *arena.get(handle.id()).expect("live handle");
    }
    let get = start.elapsed();

    let start = Instant::now();
    for handle in &handles {
        black_box(arena.remove(handle.id()));
    }
    let remove = start.elapsed();

    let start = Instant::now();
    let reused: Vec<_> = (0..N).map(|i| arena.insert(i)).collect();
    let reuse = start.elapsed();

    black_box(checksum);
    black_box(reused);

    println!("arena benchmark: {N} values");
    println!(
        "  insert      {:>10.1} ns/op  ({insert:?})",
        per_op(insert.as_nanos(), N)
    );
    println!(
        "  get         {:>10.1} ns/op  ({get:?})",
        per_op(get.as_nanos(), N)
    );
    println!(
        "  remove      {:>10.1} ns/op  ({remove:?})",
        per_op(remove.as_nanos(), N)
    );
    println!(
        "  reuse       {:>10.1} ns/op  ({reuse:?})",
        per_op(reuse.as_nanos(), N)
    );
}
