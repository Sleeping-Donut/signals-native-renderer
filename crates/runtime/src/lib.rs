#![forbid(unsafe_code)]
//! Runtime shell for Phase 0.
//!
//! The full frame loop (pump, flush, apply, layout, scene, present) arrives with
//! the graph in Phase 1. Phase 0 fixes the two host boundaries the graph will
//! need: a frame clock and an explicit local spawner. [`ManualSpawner`] is the
//! deterministic test executor.

mod runtime;
mod spawner;

pub use runtime::{FrameId, Runtime};
pub use spawner::{LocalBoxFuture, ManualSpawner, Spawner};
