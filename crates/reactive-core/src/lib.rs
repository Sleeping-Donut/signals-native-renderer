#![forbid(unsafe_code)]
//! Phase 2 reactive core: the sync graph plus async compute, flights, holds,
//! and loading boundaries. Generational arena slots and typed handles remain the
//! storage foundation.
//!
//! The arena is deliberately safe code. The design allows `unsafe` inside the
//! arena later if measurements justify it, behind Miri; nothing else in the
//! crate will be allowed to use it.

mod arena;
mod clock;
mod graph;
mod id;
mod spawner;

pub use arena::Arena;
pub use clock::VirtualClock;
pub use graph::{
    affects, clear_guess, create_async_derived_signal, create_async_memo, create_derived,
    create_derived_signal, create_memo, create_memo_unchecked, create_memo_with, create_signal,
    create_signal_unchecked, create_signal_with, effect, error_boundary, guess, is_pending, latest,
    loading_boundary, refresh, render_effect, ActionCx, ActionHandle, Affects, AsyncError,
    CleanupHandle, Cx, Derived, DirtyState, EffectHandle, ErrorBoundary, FlushError, LatestError,
    LoadingBoundary, Memo, NodeHandle, OptimisticSignal, OwnerHandle, OwnerId, ReadSignal,
    ReaderClass, Runtime, RuntimeHandle, WriteSignal,
};
pub use id::{AccessError, Handle, NodeId};
pub use spawner::{LocalBoxFuture, ManualSpawner, Spawner};

/// Create a plain signal pair.
///
/// `signal!(cx, value)` is `create_signal(cx, value)`.
#[macro_export]
macro_rules! signal {
    (derive $cx:expr, $compute:expr $(,)?) => {
        $crate::create_derived_signal($cx, $compute)
    };
    (async $cx:expr, $compute:expr $(,)?) => {
        $crate::create_async_derived_signal($cx, $compute)
    };
    ($cx:expr, $value:expr $(,)?) => {
        $crate::create_signal($cx, $value)
    };
}

/// Create a memo.
///
/// `memo!(cx, compute)` is `create_memo(cx, compute)`. Pass a third
/// argument to use a custom comparator: `memo!(cx, compute, equals)`.
#[macro_export]
macro_rules! memo {
    (async $cx:expr, $compute:expr $(,)?) => {
        $crate::create_async_memo($cx, $compute)
    };
    ($cx:expr, $compute:expr $(,)?) => {
        $crate::create_memo($cx, $compute)
    };
    ($cx:expr, $compute:expr, $equals:expr $(,)?) => {
        $crate::create_memo_with($cx, $compute, $equals)
    };
}
