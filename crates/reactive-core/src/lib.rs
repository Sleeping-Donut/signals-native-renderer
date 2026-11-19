#![forbid(unsafe_code)]
//! Reactive core foundations.
//!
//! Phase 0 owns the storage primitives the graph and the retained tree share:
//! generational arena slots, stable [`NodeId`]s, and typed [`Handle`]s. The graph
//! itself (signals, memos, effects, owners) starts in Phase 1.
//!
//! The arena is deliberately safe code. The design allows `unsafe` inside the
//! arena later if measurements justify it, behind Miri; nothing else in the
//! crate will be allowed to use it.

mod arena;
mod id;

pub use arena::Arena;
pub use id::{AccessError, Handle, NodeId};
