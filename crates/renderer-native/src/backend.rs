use std::fmt;
use std::ops::Range;

use accesskit::ActionRequest;
use renderer_api::{KeyEvent, PointerEvent, WheelEvent};
use scene::{Rect, Scene};

use crate::AccessKitAdapter;

/// Physical and logical window metrics.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BackendMetrics {
    /// Physical pixel width.
    pub physical_width: u32,
    /// Physical pixel height.
    pub physical_height: u32,
    /// Device scale factor.
    pub scale: f32,
}

impl BackendMetrics {
    /// Construct metrics.
    #[must_use]
    pub const fn new(physical_width: u32, physical_height: u32, scale: f32) -> Self {
        Self {
            physical_width,
            physical_height,
            scale,
        }
    }

    /// Logical width in points.
    #[must_use]
    pub fn logical_width(self) -> f32 {
        self.physical_width as f32 / self.scale
    }

    /// Logical height in points.
    #[must_use]
    pub fn logical_height(self) -> f32 {
        self.physical_height as f32 / self.scale
    }
}

/// A backend failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendError {
    /// No suitable GPU adapter is available.
    NoAdapter,
    /// Device creation failed.
    Device(String),
    /// Surface creation or configuration failed.
    Surface(String),
    /// Surface acquisition timed out.
    Timeout,
    /// The surface needs reconfiguration.
    Outdated,
    /// The surface was lost.
    Lost,
    /// Out of memory.
    OutOfMemory,
    /// Another backend-specific failure.
    Other(String),
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoAdapter => f.write_str("no suitable GPU adapter"),
            Self::Device(message) => write!(f, "device creation failed: {message}"),
            Self::Surface(message) => write!(f, "surface failure: {message}"),
            Self::Timeout => f.write_str("surface acquisition timed out"),
            Self::Outdated => f.write_str("surface is outdated"),
            Self::Lost => f.write_str("surface was lost"),
            Self::OutOfMemory => f.write_str("out of memory"),
            Self::Other(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for BackendError {}

/// Counters from one backend frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameStats {
    /// Scene primitives submitted.
    pub primitives: usize,
    /// Fill rectangles.
    pub fill_quads: usize,
    /// Border edge rectangles.
    pub border_quads: usize,
    /// Glyph rectangles.
    pub glyph_quads: usize,
    /// Caret rectangles.
    pub caret_quads: usize,
    /// Composition underlines.
    pub underline_quads: usize,
    /// Glyph rectangles sourced from the atlas.
    pub atlas_glyphs: usize,
    /// Glyph rectangles rendered as placeholders.
    pub placeholder_glyphs: usize,
    /// Damage rectangles supplied by the scene builder.
    pub damage_rects: usize,
    /// Whether a surface present happened.
    pub presented: bool,
}

/// A native paint backend.
pub trait PaintBackend {
    /// Reconfigure for a physical size and scale.
    fn resize(&mut self, width: u32, height: u32, scale: f32);
    /// Render a scene. Presentation is separate so a surface frame can be held
    /// until [`PaintBackend::present`].
    fn render(&mut self, scene: &Scene, damage: &[Rect]) -> Result<FrameStats, BackendError>;
    /// Present the rendered frame.
    fn present(&mut self) -> Result<(), BackendError>;
    /// Current metrics.
    fn metrics(&self) -> BackendMetrics;
    /// Read a rendered pixel for headless verification.
    fn read_pixel(&self, _x: u32, _y: u32) -> Option<[u8; 4]> {
        None
    }
    /// Backend name.
    fn name(&self) -> &'static str;
}

/// One app frame: a scene and its damage.
#[derive(Clone, Debug, Default)]
pub struct FrameOutput {
    /// Scene to paint.
    pub scene: Scene,
    /// Damage rectangles.
    pub damage: Vec<Rect>,
}

impl FrameOutput {
    /// Construct a frame.
    #[must_use]
    pub fn new(scene: Scene, damage: Vec<Rect>) -> Self {
        Self { scene, damage }
    }
}

/// An application driven by a window loop.
pub trait WindowApp {
    /// The window was resized or its scale changed.
    fn resize(&mut self, metrics: BackendMetrics);
    /// Produce one frame.
    fn frame(&mut self, metrics: BackendMetrics) -> FrameOutput;
    /// Pointer moved in logical window coordinates.
    fn pointer_move(&mut self, _event: &PointerEvent) {}
    /// Primary pointer pressed.
    fn pointer_down(&mut self, _event: &PointerEvent) {}
    /// Primary pointer released.
    fn pointer_up(&mut self, _event: &PointerEvent) {}
    /// Key pressed. Return `true` when consumed.
    fn key_down(&mut self, _event: &KeyEvent) -> bool {
        false
    }
    /// Wheel/scroll. Return `true` when consumed or scrolled.
    fn wheel(&mut self, _event: &WheelEvent) -> bool {
        false
    }
    /// IME composition changed.
    fn ime_preedit(&mut self, _text: &str, _cursor: Option<Range<usize>>) {}
    /// IME committed text.
    fn ime_commit(&mut self, _text: &str) {}
    /// IME was disabled or cancelled.
    fn ime_cancel(&mut self) {}
    /// Current AccessKit tree update, if the app exposes accessibility.
    fn accesskit_update(&self) -> Option<accesskit::TreeUpdate> {
        None
    }
    /// Handle an AccessKit action request.
    fn accesskit_action(&mut self, _request: &ActionRequest) -> bool {
        false
    }
    /// Caret rectangle for IME candidate window placement.
    fn ime_cursor_area(&self) -> Option<Rect> {
        None
    }
}

/// Drives an application and a backend through resize and redraw steps.
pub struct WindowDriver<A, B> {
    app: A,
    backend: B,
    metrics: BackendMetrics,
    frames: u64,
    last_stats: FrameStats,
    last_scene_len: usize,
    accesskit: AccessKitAdapter,
}

impl<A: WindowApp, B: PaintBackend> WindowDriver<A, B> {
    /// Create a driver and deliver the initial metrics to the app.
    pub fn new(mut app: A, backend: B) -> Self {
        let metrics = backend.metrics();
        app.resize(metrics);
        Self {
            app,
            backend,
            metrics,
            frames: 0,
            last_stats: FrameStats::default(),
            last_scene_len: 0,
            accesskit: AccessKitAdapter::new(),
        }
    }

    /// Current window metrics.
    #[must_use]
    pub fn metrics(&self) -> BackendMetrics {
        self.metrics
    }

    /// Number of completed frames.
    #[must_use]
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Statistics from the last frame.
    #[must_use]
    pub fn last_stats(&self) -> FrameStats {
        self.last_stats
    }

    /// Primitive count from the last frame.
    #[must_use]
    pub fn last_scene_len(&self) -> usize {
        self.last_scene_len
    }

    /// Resize the backend and notify the app.
    pub fn resize(&mut self, width: u32, height: u32, scale: f32) {
        self.metrics = BackendMetrics::new(width, height, scale);
        self.backend.resize(width, height, scale);
        self.app.resize(self.metrics);
    }

    /// Route a pointer move to the app.
    pub fn pointer_move(&mut self, event: &PointerEvent) {
        self.app.pointer_move(event);
    }

    /// Route a pointer press to the app.
    pub fn pointer_down(&mut self, event: &PointerEvent) {
        self.app.pointer_down(event);
    }

    /// Route a pointer release to the app.
    pub fn pointer_up(&mut self, event: &PointerEvent) {
        self.app.pointer_up(event);
    }

    /// Route a key press to the app.
    pub fn key_down(&mut self, event: &KeyEvent) -> bool {
        self.app.key_down(event)
    }

    /// Route a wheel event to the app.
    pub fn wheel(&mut self, event: &WheelEvent) -> bool {
        self.app.wheel(event)
    }

    /// Route an IME composition update to the app.
    pub fn ime_preedit(&mut self, text: &str, cursor: Option<Range<usize>>) {
        self.app.ime_preedit(text, cursor);
    }

    /// Route committed IME text to the app.
    pub fn ime_commit(&mut self, text: &str) {
        self.app.ime_commit(text);
    }

    /// Route IME cancellation to the app.
    pub fn ime_cancel(&mut self) {
        self.app.ime_cancel();
    }

    /// The latest AccessKit tree update applied by the driver.
    #[must_use]
    pub fn accesskit_update(&self) -> Option<&accesskit::TreeUpdate> {
        self.accesskit.last_update()
    }

    /// Number of AccessKit updates applied.
    #[must_use]
    pub fn accesskit_update_count(&self) -> u64 {
        self.accesskit.update_count()
    }

    /// Route an AccessKit action into the app.
    pub fn accesskit_action(&mut self, request: &ActionRequest) -> bool {
        self.app.accesskit_action(request)
    }

    /// The platform AccessKit adapter boundary.
    #[must_use]
    pub fn accesskit_adapter(&self) -> &AccessKitAdapter {
        &self.accesskit
    }

    /// Caret rectangle for IME candidate window placement.
    #[must_use]
    pub fn ime_cursor_area(&self) -> Option<Rect> {
        self.app.ime_cursor_area()
    }

    /// Run one app frame and present it.
    pub fn redraw(&mut self) -> Result<FrameStats, BackendError> {
        let output = self.app.frame(self.metrics);
        self.last_scene_len = output.scene.len();
        self.last_stats = self.backend.render(&output.scene, &output.damage)?;
        self.backend.present()?;
        self.last_stats.presented = true;
        if let Some(update) = self.app.accesskit_update() {
            self.accesskit.apply(update);
        }
        self.frames += 1;
        Ok(self.last_stats)
    }

    /// The application.
    #[must_use]
    pub fn app(&self) -> &A {
        &self.app
    }

    /// The application, mutably.
    pub fn app_mut(&mut self) -> &mut A {
        &mut self.app
    }

    /// The backend.
    #[must_use]
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// The backend, mutably.
    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }
}
