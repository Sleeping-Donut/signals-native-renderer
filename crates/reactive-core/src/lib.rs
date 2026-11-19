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
mod graph;
mod id;

pub use arena::Arena;
pub use graph::{
    create_derived, create_derived_signal, create_memo, create_memo_unchecked, create_memo_with,
    create_signal, create_signal_unchecked, create_signal_with, effect, render_effect,
    CleanupHandle, Cx, Derived, DirtyState, EffectHandle, FlushError, Memo, OwnerHandle, OwnerId,
    ReadSignal, Runtime, WriteSignal,
};
pub use id::{AccessError, Handle, NodeId};
