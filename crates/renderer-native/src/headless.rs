use scene::{Primitive, Rect, Scene};

use crate::{BackendError, BackendMetrics, FrameStats, PaintBackend};

/// One recorded headless frame.
#[derive(Clone, Debug, PartialEq)]
pub struct HeadlessFrame {
    /// Metrics used for the frame.
    pub metrics: BackendMetrics,
    /// Backend counters.
    pub stats: FrameStats,
    /// Submitted scene.
    pub scene: Scene,
    /// Damage rectangles.
    pub damage: Vec<Rect>,
}

/// A recording backend for tests and headless runs.
#[derive(Clone, Debug, Default)]
pub struct HeadlessBackend {
    metrics: BackendMetrics,
    frames: Vec<HeadlessFrame>,
    resizes: Vec<BackendMetrics>,
    presents: u64,
}

impl HeadlessBackend {
    /// Create a headless backend.
    #[must_use]
    pub fn new(width: u32, height: u32, scale: f32) -> Self {
        Self {
            metrics: BackendMetrics::new(width, height, scale),
            frames: Vec::new(),
            resizes: Vec::new(),
            presents: 0,
        }
    }

    /// Recorded frames.
    #[must_use]
    pub fn frames(&self) -> &[HeadlessFrame] {
        &self.frames
    }

    /// The most recent frame.
    #[must_use]
    pub fn last(&self) -> Option<&HeadlessFrame> {
        self.frames.last()
    }

    /// Recorded resize metrics.
    #[must_use]
    pub fn resizes(&self) -> &[BackendMetrics] {
        &self.resizes
    }

    /// Number of presents.
    #[must_use]
    pub fn presents(&self) -> u64 {
        self.presents
    }
}

impl PaintBackend for HeadlessBackend {
    fn resize(&mut self, width: u32, height: u32, scale: f32) {
        self.metrics = BackendMetrics::new(width, height, scale);
        self.resizes.push(self.metrics);
    }

    fn render(&mut self, scene: &Scene, damage: &[Rect]) -> Result<FrameStats, BackendError> {
        let mut stats = FrameStats {
            primitives: scene.len(),
            damage_rects: damage.len(),
            ..FrameStats::default()
        };
        for primitive in scene.ops() {
            match primitive {
                Primitive::Fill { .. } => stats.fill_quads += 1,
                Primitive::Border { .. } => stats.border_quads += 1,
                Primitive::Text { layout, .. } => {
                    stats.glyph_quads += layout
                        .lines
                        .iter()
                        .map(|line| line.glyphs.len())
                        .sum::<usize>();
                }
            }
        }
        self.frames.push(HeadlessFrame {
            metrics: self.metrics,
            stats,
            scene: scene.clone(),
            damage: damage.to_vec(),
        });
        Ok(stats)
    }

    fn present(&mut self) -> Result<(), BackendError> {
        self.presents += 1;
        if let Some(frame) = self.frames.last_mut() {
            frame.stats.presented = true;
        }
        Ok(())
    }

    fn metrics(&self) -> BackendMetrics {
        self.metrics
    }

    fn name(&self) -> &'static str {
        "headless"
    }
}
