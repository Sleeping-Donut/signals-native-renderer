#![forbid(unsafe_code)]
//! The universal renderer contract.
//!
//! A backend implements [`Renderer`] for structure, properties, and events. It
//! may implement capability traits for text, images, scrolling, virtual lists,
//! accessibility, and animation. The contract intentionally has no layout or
//! paint primitives; a native backend uses its own scene, while a DOM backend
//! keeps its own layout.

use std::fmt;

/// Stable renderer node identity.
pub type NodeId = u64;

/// Stable listener identity.
pub type ListenerId = u64;

/// A borrowed element tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ElementTag<'a>(pub &'a str);

impl<'a> ElementTag<'a> {
    /// Construct a tag.
    #[must_use]
    pub const fn new(tag: &'a str) -> Self {
        Self(tag)
    }

    /// The tag name.
    #[must_use]
    pub const fn as_str(self) -> &'a str {
        self.0
    }
}

impl<'a> From<&'a str> for ElementTag<'a> {
    fn from(value: &'a str) -> Self {
        Self(value)
    }
}

/// A borrowed attribute name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AttrName<'a>(pub &'a str);

impl<'a> AttrName<'a> {
    /// Construct an attribute name.
    #[must_use]
    pub const fn new(name: &'a str) -> Self {
        Self(name)
    }

    /// The name.
    #[must_use]
    pub const fn as_str(self) -> &'a str {
        self.0
    }
}

impl<'a> From<&'a str> for AttrName<'a> {
    fn from(value: &'a str) -> Self {
        Self(value)
    }
}

/// A borrowed attribute value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AttrValue<'a>(pub &'a str);

impl<'a> AttrValue<'a> {
    /// Construct an attribute value.
    #[must_use]
    pub const fn new(value: &'a str) -> Self {
        Self(value)
    }

    /// The value.
    #[must_use]
    pub const fn as_str(self) -> &'a str {
        self.0
    }
}

impl<'a> From<&'a str> for AttrValue<'a> {
    fn from(value: &'a str) -> Self {
        Self(value)
    }
}

/// A renderer operation failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RendererError {
    /// The node does not exist.
    InvalidNode(NodeId),
    /// The child is not attached to the expected parent.
    NotChild,
    /// The backend does not support this operation.
    Unsupported(&'static str),
    /// The backend has been shut down.
    ShutDown,
    /// A backend-specific failure.
    Other(String),
}

impl fmt::Display for RendererError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidNode(id) => write!(f, "invalid renderer node {id}"),
            Self::NotChild => f.write_str("node is not a child of that parent"),
            Self::Unsupported(what) => write!(f, "renderer does not support {what}"),
            Self::ShutDown => f.write_str("renderer is shut down"),
            Self::Other(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for RendererError {}

/// Event kinds delivered by a host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventKind {
    /// A primary click.
    Click,
    /// Pointer pressed.
    PointerDown,
    /// Pointer released.
    PointerUp,
    /// Pointer moved.
    PointerMove,
    /// Key pressed.
    KeyDown,
    /// Key released.
    KeyUp,
    /// Text input.
    TextInput,
    /// Scroll gesture.
    Scroll,
}

/// A delivered event.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Event {
    /// Event kind.
    pub kind: EventKind,
    /// Logical x coordinate relative to the node.
    pub x: f32,
    /// Logical y coordinate relative to the node.
    pub y: f32,
}

impl Event {
    /// Construct an event.
    #[must_use]
    pub const fn new(kind: EventKind, x: f32, y: f32) -> Self {
        Self { kind, x, y }
    }
}

/// Keyboard modifiers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    /// Shift.
    pub shift: bool,
    /// Control.
    pub control: bool,
    /// Alt/Option.
    pub alt: bool,
    /// Command/Windows/Super.
    pub meta: bool,
}

impl Modifiers {
    /// No modifiers.
    pub const NONE: Self = Self {
        shift: false,
        control: false,
        alt: false,
        meta: false,
    };

    /// Construct modifiers.
    #[must_use]
    pub const fn new(shift: bool, control: bool, alt: bool, meta: bool) -> Self {
        Self {
            shift,
            control,
            alt,
            meta,
        }
    }
}

/// Named keyboard keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NamedKey {
    /// Enter/Return.
    Enter,
    /// Escape.
    Escape,
    /// Tab.
    Tab,
    /// Space.
    Space,
    /// Backspace.
    Backspace,
    /// Delete.
    Delete,
    /// Left arrow.
    ArrowLeft,
    /// Right arrow.
    ArrowRight,
    /// Up arrow.
    ArrowUp,
    /// Down arrow.
    ArrowDown,
    /// Home.
    Home,
    /// End.
    End,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Function key number.
    Function(u8),
}

/// A keyboard key.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// Printable character.
    Character(char),
    /// Named non-character key.
    Named(NamedKey),
}

/// A keyboard event.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyEvent {
    /// The key.
    pub key: Key,
    /// Active modifiers.
    pub modifiers: Modifiers,
    /// Whether this is an auto-repeat.
    pub repeat: bool,
    /// Text produced by the key, if any.
    pub text: Option<String>,
}

impl KeyEvent {
    /// Construct a key event.
    #[must_use]
    pub fn new(key: Key, modifiers: Modifiers) -> Self {
        Self {
            key,
            modifiers,
            repeat: false,
            text: None,
        }
    }
}

/// A pointer button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerButton {
    /// Primary button.
    Primary,
    /// Secondary button.
    Secondary,
    /// Middle button.
    Middle,
}

/// A pointer event in logical window coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PointerEvent {
    /// Logical x.
    pub x: f32,
    /// Logical y.
    pub y: f32,
    /// Button, if any.
    pub button: Option<PointerButton>,
}

impl PointerEvent {
    /// Construct a pointer event.
    #[must_use]
    pub const fn new(x: f32, y: f32, button: Option<PointerButton>) -> Self {
        Self { x, y, button }
    }
}

/// A wheel/scroll event in logical window coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WheelEvent {
    /// Logical x.
    pub x: f32,
    /// Logical y.
    pub y: f32,
    /// Horizontal delta.
    pub delta_x: f32,
    /// Vertical delta.
    pub delta_y: f32,
}

impl WheelEvent {
    /// Construct a wheel event.
    #[must_use]
    pub const fn new(x: f32, y: f32, delta_x: f32, delta_y: f32) -> Self {
        Self {
            x,
            y,
            delta_x,
            delta_y,
        }
    }
}

/// Frame metadata passed to a backend.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameInfo {
    /// Monotonic frame number.
    pub frame: u64,
}

/// Layout display mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Display {
    /// Flex layout.
    #[default]
    Flex,
    /// No layout.
    None,
}

/// Positioning mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Position {
    /// In flow.
    #[default]
    Relative,
    /// Out of flow.
    Absolute,
}

/// A flexible length.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Length {
    /// Automatic.
    #[default]
    Auto,
    /// Logical pixels.
    Px(f32),
    /// Percentage of the parent.
    Percent(f32),
}

/// A definite length.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DefiniteLength {
    /// Logical pixels.
    Px(f32),
    /// Percentage of the parent.
    Percent(f32),
}

impl Default for DefiniteLength {
    fn default() -> Self {
        Self::Px(0.0)
    }
}

/// A width/height pair.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size<T> {
    /// Width.
    pub width: T,
    /// Height.
    pub height: T,
}

/// Four edge values.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Edges<T> {
    /// Top.
    pub top: T,
    /// Right.
    pub right: T,
    /// Bottom.
    pub bottom: T,
    /// Left.
    pub left: T,
}

/// Flex direction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FlexDirection {
    /// Horizontal.
    #[default]
    Row,
    /// Vertical.
    Column,
}

/// Cross-axis alignment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    /// Start.
    #[default]
    Start,
    /// Center.
    Center,
    /// End.
    End,
    /// Stretch.
    Stretch,
}

/// Main-axis distribution.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Justify {
    /// Start.
    #[default]
    Start,
    /// Center.
    Center,
    /// End.
    End,
    /// Space between.
    SpaceBetween,
}

/// Flex style fields.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FlexStyle {
    /// Direction.
    pub direction: FlexDirection,
    /// Grow factor.
    pub grow: f32,
    /// Shrink factor.
    pub shrink: f32,
    /// Basis.
    pub basis: Length,
    /// Cross-axis alignment.
    pub align_items: Align,
    /// Main-axis distribution.
    pub justify_content: Justify,
}

/// RGBA color in 0..=1.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Color {
    /// Red.
    pub r: f32,
    /// Green.
    pub g: f32,
    /// Blue.
    pub b: f32,
    /// Alpha.
    pub a: f32,
}

impl Color {
    /// Construct a color.
    #[must_use]
    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }
}

/// Text paint properties.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextStyle {
    /// Text color.
    pub color: Color,
    /// Logical font size.
    pub font_size: f32,
    /// Logical line height.
    pub line_height: f32,
    /// Numeric font weight.
    pub weight: u16,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            color: Color::new(0.0, 0.0, 0.0, 1.0),
            font_size: 16.0,
            line_height: 20.0,
            weight: 400,
        }
    }
}

/// A 2D translation and axis-aligned scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    /// X translation.
    pub translate_x: f32,
    /// Y translation.
    pub translate_y: f32,
    /// X scale.
    pub scale_x: f32,
    /// Y scale.
    pub scale_y: f32,
}

impl Transform {
    /// Identity transform.
    pub const IDENTITY: Self = Self {
        translate_x: 0.0,
        translate_y: 0.0,
        scale_x: 1.0,
        scale_y: 1.0,
    };

    /// Construct a transform.
    #[must_use]
    pub const fn new(translate_x: f32, translate_y: f32, scale_x: f32, scale_y: f32) -> Self {
        Self {
            translate_x,
            translate_y,
            scale_x,
            scale_y,
        }
    }

    /// A pure translation.
    #[must_use]
    pub const fn translation(x: f32, y: f32) -> Self {
        Self::new(x, y, 1.0, 1.0)
    }

    /// A uniform scale.
    #[must_use]
    pub const fn scale(scale: f32) -> Self {
        Self::new(0.0, 0.0, scale, scale)
    }

    /// Whether this is the identity transform.
    #[must_use]
    pub fn is_identity(self) -> bool {
        self == Self::IDENTITY
    }

    /// Apply to a point.
    #[must_use]
    pub fn apply_point(self, point: Point<f32>) -> Point<f32> {
        Point {
            x: point.x * self.scale_x + self.translate_x,
            y: point.y * self.scale_y + self.translate_y,
        }
    }

    /// Apply to an X coordinate.
    #[must_use]
    pub fn apply_x(self, x: f32) -> f32 {
        x * self.scale_x + self.translate_x
    }

    /// Apply to a Y coordinate.
    #[must_use]
    pub fn apply_y(self, y: f32) -> f32 {
        y * self.scale_y + self.translate_y
    }

    /// Compose a local transform around `origin` after this parent transform.
    #[must_use]
    pub fn compose(self, local: Self, origin: Point<f32>) -> Self {
        Self {
            translate_x: (origin.x - origin.x * local.scale_x + local.translate_x) * self.scale_x
                + self.translate_x,
            translate_y: (origin.y - origin.y * local.scale_y + local.translate_y) * self.scale_y
                + self.translate_y,
            scale_x: local.scale_x * self.scale_x,
            scale_y: local.scale_y * self.scale_y,
        }
    }

    /// Inverse-map a screen point into local coordinates.
    #[must_use]
    pub fn inverse_point(self, point: Point<f32>) -> Option<Point<f32>> {
        if self.scale_x == 0.0 || self.scale_y == 0.0 {
            return None;
        }
        Some(Point {
            x: (point.x - self.translate_x) / self.scale_x,
            y: (point.y - self.translate_y) / self.scale_y,
        })
    }
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// Content overflow behavior.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Overflow {
    /// Content paints outside the node.
    #[default]
    Visible,
    /// Content is clipped to the node's bounds.
    Hidden,
    /// Content is clipped and scrollable.
    Scroll,
    /// Content is clipped and scrollable when it overflows.
    Auto,
}

/// The resolved style sent to a backend.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ResolvedStyle {
    /// Display mode.
    pub display: Display,
    /// Positioning.
    pub position: Position,
    /// Size.
    pub size: Size<Length>,
    /// Padding.
    pub padding: Edges<DefiniteLength>,
    /// Gap.
    pub gap: Size<DefiniteLength>,
    /// Flex properties.
    pub flex: FlexStyle,
    /// Background.
    pub background: Option<Color>,
    /// Border widths.
    pub border: Edges<f32>,
    /// Border color.
    pub border_color: Option<Color>,
    /// Corner radii.
    pub radius: Edges<f32>,
    /// Opacity.
    pub opacity: f32,
    /// Text properties.
    pub text: TextStyle,
    /// Paint/hit transform applied to this node's subtree.
    pub transform: Transform,
    /// Whether children are clipped to this node's bounds.
    pub overflow: Overflow,
}

/// A logical point.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point<T> {
    /// X.
    pub x: T,
    /// Y.
    pub y: T,
}

/// Text measurement request.
#[derive(Clone, Debug, PartialEq)]
pub struct TextMeasureRequest {
    /// Text.
    pub text: String,
    /// Available width.
    pub width: Option<f32>,
    /// Style.
    pub style: TextStyle,
}

/// Text metrics.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TextMetrics {
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
}

/// Text shaping request.
#[derive(Clone, Debug, PartialEq)]
pub struct TextShapeRequest {
    /// Text.
    pub text: String,
    /// Style.
    pub style: TextStyle,
}

/// Shaped text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShapedText {
    /// Glyph runs, opaque to the contract.
    pub runs: Vec<u32>,
    /// Metrics.
    pub metrics: TextMetrics,
}

/// Image data handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ImageId(pub u64);

/// Raw image bytes.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageData {
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
    /// RGBA bytes.
    pub rgba: Vec<u8>,
}

/// Scroll event.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScrollEvent {
    /// X offset.
    pub x: f32,
    /// Y offset.
    pub y: f32,
}

/// Accessibility role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum A11yRole {
    /// Generic container.
    Generic,
    /// Button.
    Button,
    /// Text.
    Text,
    /// Text input.
    TextInput,
}

/// Accessibility update.
#[derive(Clone, Debug, PartialEq)]
pub struct A11yUpdate {
    /// Role update.
    pub role: Option<A11yRole>,
    /// Label update.
    pub label: Option<String>,
}

/// Capabilities a backend advertises once at construction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CapabilitySet {
    /// Text measurement/shaping.
    pub text: bool,
    /// Images.
    pub images: bool,
    /// Scrolling.
    pub scroll: bool,
    /// Virtual lists.
    pub virtual_list: bool,
    /// Accessibility.
    pub a11y: bool,
    /// Animation frames.
    pub animation: bool,
}

/// The object-safe renderer contract.
pub trait Renderer {
    // Structure.

    /// Create an element.
    fn create_element(&mut self, tag: ElementTag<'_>) -> Result<NodeId, RendererError>;
    /// Create a text node.
    fn create_text(&mut self, text: &str) -> Result<NodeId, RendererError>;
    /// Create a fragment.
    fn create_fragment(&mut self) -> Result<NodeId, RendererError>;
    /// Insert a child before `anchor`, or append when `None`.
    fn insert(
        &mut self,
        parent: NodeId,
        child: NodeId,
        anchor: Option<NodeId>,
    ) -> Result<(), RendererError>;
    /// Remove a child from a parent.
    fn remove(&mut self, parent: NodeId, child: NodeId) -> Result<(), RendererError>;

    // Properties.

    /// Set text content.
    fn set_text(&mut self, node: NodeId, text: &str) -> Result<(), RendererError>;
    /// Set an attribute.
    fn set_attr(
        &mut self,
        node: NodeId,
        name: AttrName<'_>,
        value: AttrValue<'_>,
    ) -> Result<(), RendererError>;
    /// Set the resolved style.
    fn set_style(&mut self, node: NodeId, style: &ResolvedStyle) -> Result<(), RendererError>;

    // Events.

    /// Register an event listener.
    fn listen(
        &mut self,
        node: NodeId,
        event: EventKind,
        handler: Box<dyn FnMut(&Event)>,
    ) -> Result<ListenerId, RendererError>;
    /// Remove a listener.
    fn unlisten(&mut self, node: NodeId, listener: ListenerId) -> Result<(), RendererError>;

    // Optional frame hooks.

    /// Begin a frame.
    fn begin_frame(&mut self, _info: &FrameInfo) -> Result<(), RendererError> {
        Ok(())
    }
    /// End a frame.
    fn end_frame(&mut self) -> Result<(), RendererError> {
        Ok(())
    }

    /// Optional focus capability. A backend that does not support focus returns
    /// `None`; the view layer keeps its own retained focus identity either way.
    fn focus_capable(&mut self) -> Option<&mut dyn FocusCapable> {
        None
    }
}

/// Focus capability.
pub trait FocusCapable: Renderer {
    /// Set or clear the focused node.
    fn set_focus(&mut self, node: Option<NodeId>) -> Result<(), RendererError>;
    /// The focused node.
    fn focused(&self) -> Option<NodeId>;
}

/// Text capability.
pub trait TextCapable: Renderer {
    /// Measure text.
    fn measure_text(&mut self, req: &TextMeasureRequest) -> Result<TextMetrics, RendererError>;
    /// Shape text.
    fn shape_text(&mut self, req: &TextShapeRequest) -> Result<ShapedText, RendererError>;
}

/// Image capability.
pub trait ImageCapable: Renderer {
    /// Create an image.
    fn create_image(&mut self, data: ImageData) -> Result<ImageId, RendererError>;
    /// Destroy an image.
    fn destroy_image(&mut self, id: ImageId);
}

/// Scroll capability.
pub trait ScrollCapable: Renderer {
    /// Set scroll offset.
    fn set_scroll_offset(&mut self, node: NodeId, offset: Point<f32>) -> Result<(), RendererError>;
    /// Read scroll offset.
    fn scroll_offset(&self, node: NodeId) -> Point<f32>;
    /// Set a scroll handler.
    fn set_scroll_handler(&mut self, node: NodeId, handler: Box<dyn FnMut(ScrollEvent)>);
}

/// Virtual-list capability.
pub trait VirtualListCapable: Renderer {
    /// Set the visible range.
    fn set_visible_range(&mut self, node: NodeId, range: std::ops::Range<usize>);
}

/// Accessibility capability.
pub trait A11yCapable: Renderer {
    /// Set a role.
    fn set_a11y_role(&mut self, node: NodeId, role: A11yRole);
    /// Set a label.
    fn set_a11y_label(&mut self, node: NodeId, label: &str);
    /// Apply an update.
    fn update_a11y(&mut self, update: &A11yUpdate);
}

/// Animation capability.
pub trait AnimationCapable: Renderer {
    /// Request an animation frame.
    fn request_animation_frame(&mut self);
}
