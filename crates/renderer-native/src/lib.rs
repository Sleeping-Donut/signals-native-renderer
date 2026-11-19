#![forbid(unsafe_code)]
//! Native paint backend and window loop.
//!
//! [`PaintBackend`] consumes a [`scene::Scene`] and presents it. [`WindowDriver`]
//! owns the application/backend pair and applies resize and redraw steps. The
//! wgpu implementation renders scene primitives to an offscreen target or a
//! window surface; [`HeadlessBackend`] records the same steps for tests.

mod backend;
mod headless;
mod wgpu;
#[cfg(feature = "window")]
mod window;

pub use backend::{
    BackendError, BackendMetrics, FrameOutput, FrameStats, PaintBackend, WindowApp, WindowDriver,
};
pub use headless::{HeadlessBackend, HeadlessFrame};
pub use wgpu::WgpuBackend;
#[cfg(feature = "window")]
pub use window::{run_windowed, WindowError};
