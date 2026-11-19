#![forbid(unsafe_code)]
//! Phase 2 reactive core: the sync graph plus async compute, flights, holds,
//! and loading boundaries. Generational arena slots and typed handles remain the
//! storage foundation.
//!
//! The arena is deliberately safe code. The design allows `unsafe` inside the
//! arena later if measurements justify it, behind Miri; nothing else in the
//! crate will be allowed to use it.

mod arena;
mod graph;
mod id;

pub use arena::Arena;
pub use graph::{
    create_async_derived_signal, create_async_memo, create_derived, create_derived_signal,
    create_memo, create_memo_unchecked, create_memo_with, create_signal, create_signal_unchecked,
    create_signal_with, effect, error_boundary, loading_boundary, render_effect, AsyncError,
    CleanupHandle, Cx, Derived, DirtyState, EffectHandle, ErrorBoundary, FlushError,
    LoadingBoundary, Memo, OwnerHandle, OwnerId, ReadSignal, Runtime, WriteSignal,
};
pub use id::{AccessError, Handle, NodeId};
