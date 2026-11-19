#![forbid(unsafe_code)]
//! Native paint backend and window loop.
//!
//! [`PaintBackend`] consumes a [`scene::Scene`] and presents it. [`WindowDriver`]
//! owns the application/backend pair and applies resize and redraw steps. The
//! wgpu implementation renders scene primitives to an offscreen target or a
//! window surface; [`HeadlessBackend`] records the same steps for tests.

mod accesskit_adapter;
mod atlas;
mod backend;
mod headless;
mod wgpu;
#[cfg(feature = "window")]
mod window;

pub use accesskit_adapter::AccessKitAdapter;
pub use atlas::{AtlasEntry, AtlasStats, GlyphAtlas, GlyphKey, ATLAS_SIZE};
pub use backend::{
    BackendError, BackendMetrics, FrameOutput, FrameStats, PaintBackend, WindowApp, WindowDriver,
};
pub use headless::{HeadlessBackend, HeadlessFrame};
pub use wgpu::WgpuBackend;
#[cfg(feature = "window")]
pub use window::{run_windowed, WindowError};

/// Whether a shaped glyph should produce paint geometry.
///
/// Spaces, tabs, and line breaks carry an advance but no mask. Drawing a
/// placeholder for them produces black rectangles between words.
pub(crate) fn is_paintable_glyph(glyph: &text::Glyph) -> bool {
    !glyph.ch.is_whitespace()
}

#[cfg(test)]
mod tests {
    use super::is_paintable_glyph;
    use text::Glyph;

    fn glyph(ch: char) -> Glyph {
        Glyph {
            ch,
            cluster: 0,
            x: 0.0,
            y: 0.0,
            advance: 4.0,
            glyph_id: 0,
            font_id: 0,
            image: None,
        }
    }

    #[test]
    fn whitespace_glyphs_are_not_painted() {
        assert!(is_paintable_glyph(&glyph('a')));
        assert!(is_paintable_glyph(&glyph('0')));
        assert!(!is_paintable_glyph(&glyph(' ')));
        assert!(!is_paintable_glyph(&glyph('\t')));
        assert!(!is_paintable_glyph(&glyph('\n')));
    }
}
