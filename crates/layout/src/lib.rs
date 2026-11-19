#![forbid(unsafe_code)]
//! Persistent layout tree for the native renderer.
//!
//! The view tree owns node identity. This crate keeps a long-lived taffy tree
//! keyed by [`LayoutId`], applies [`ResolvedStyle`](renderer_api::ResolvedStyle),
//! measures text through a host-supplied [`TextMeasure`], and copies snapped
//! geometry back after a layout pass.
//!
//! Layout is skipped entirely when nothing that affects layout changed. A
//! paint-only style change updates the stored style but does not dirty layout.

use std::collections::{HashMap, HashSet};

use renderer_api::{
    Align, Color, DefiniteLength, Display, FlexDirection, Justify, Length, Position, ResolvedStyle,
    Size, TextMeasureRequest, TextMetrics, TextStyle,
};
use taffy::{
    AlignItems, AvailableSpace, Dimension, Display as TaffyDisplay,
    FlexDirection as TaffyDirection, JustifyContent, LayoutInput, LayoutOutput, LengthPercentage,
    NodeId as TaffyId, Position as TaffyPosition, Rect as TaffyRect, Size as TaffySize, Style,
    TaffyTree,
};

/// Stable layout identity.
pub type LayoutId = u64;

/// A snapped rectangle in logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
}

impl Rect {
    /// Right edge.
    #[must_use]
    pub fn right(self) -> f32 {
        self.x + self.width
    }

    /// Bottom edge.
    #[must_use]
    pub fn bottom(self) -> f32 {
        self.y + self.height
    }
}

/// A text payload attached to a leaf layout node.
#[derive(Clone, Debug, PartialEq)]
pub struct TextContent {
    /// The text.
    pub text: String,
    /// Text metrics inputs.
    pub style: TextStyle,
}

/// Measures text for taffy's measure callback.
pub trait TextMeasure {
    /// Measure `request`. A width of `None` means an unconstrained line.
    fn measure(&mut self, request: &TextMeasureRequest) -> TextMetrics;
}

/// Result of [`LayoutEngine::compute`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayoutStats {
    /// Whether a layout pass ran.
    pub ran: bool,
    /// Number of roots computed.
    pub roots: usize,
    /// Number of nodes whose geometry was copied.
    pub nodes: usize,
}

#[derive(Debug)]
struct NodeContext {
    text: Option<TextContent>,
}

#[derive(Debug)]
struct NodeEntry {
    taffy_id: TaffyId,
    style: ResolvedStyle,
    text: Option<TextContent>,
    children: Vec<LayoutId>,
}

/// A persistent layout tree.
pub struct LayoutEngine {
    taffy: TaffyTree<NodeContext>,
    nodes: HashMap<LayoutId, NodeEntry>,
    roots: Vec<LayoutId>,
    geometry: HashMap<LayoutId, Rect>,
    changed: HashSet<LayoutId>,
    dirty_nodes: HashSet<LayoutId>,
    measure: Box<dyn TextMeasure>,
    viewport: Option<Size<f32>>,
    scale: f32,
    layout_runs: u64,
    next_layout: LayoutId,
}

impl LayoutEngine {
    /// Create an engine at a device scale factor (for example `2.0`).
    #[must_use]
    pub fn new(scale: f32, measure: Box<dyn TextMeasure>) -> Self {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let mut taffy = TaffyTree::new();
        taffy.disable_rounding();
        Self {
            taffy,
            nodes: HashMap::new(),
            roots: Vec::new(),
            geometry: HashMap::new(),
            changed: HashSet::new(),
            dirty_nodes: HashSet::new(),
            measure,
            viewport: None,
            scale,
            layout_runs: 0,
            next_layout: 1,
        }
    }

    /// The current scale factor.
    #[must_use]
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Set the scale factor. A change invalidates all geometry.
    pub fn set_scale(&mut self, scale: f32) {
        if scale.is_finite() && scale > 0.0 && scale != self.scale {
            self.scale = scale;
            self.invalidate_all();
        }
    }

    /// Number of layout passes that ran.
    #[must_use]
    pub fn layout_runs(&self) -> u64 {
        self.layout_runs
    }

    /// Nodes awaiting layout.
    #[must_use]
    pub fn dirty_nodes(&self) -> usize {
        self.dirty_nodes.len()
    }

    /// Create a leaf or container and return its layout identity.
    pub fn create_node(&mut self, style: ResolvedStyle, text: Option<TextContent>) -> LayoutId {
        let taffy_style = to_taffy_style(&style, self.scale);
        let taffy_id = self
            .taffy
            .new_leaf_with_context(taffy_style, NodeContext { text: text.clone() })
            .expect("create taffy leaf");
        let id = self.next_layout;
        self.next_layout += 1;
        self.nodes.insert(
            id,
            NodeEntry {
                taffy_id,
                style,
                text,
                children: Vec::new(),
            },
        );
        id
    }

    /// Replace a node's children. Identical child lists are a no-op.
    pub fn set_children(&mut self, id: LayoutId, children: &[LayoutId]) {
        let Some(entry) = self.nodes.get(&id) else {
            return;
        };
        if entry.children == children {
            return;
        }
        let taffy_id = entry.taffy_id;
        let taffy_children: Vec<TaffyId> = children
            .iter()
            .filter_map(|child| self.nodes.get(child).map(|child| child.taffy_id))
            .collect();
        self.taffy
            .set_children(taffy_id, &taffy_children)
            .expect("set taffy children");
        self.nodes.get_mut(&id).expect("layout node").children = children.to_vec();
        self.mark_dirty(id);
    }

    /// Replace a node's style.
    ///
    /// Only layout-affecting fields dirty layout. Color, opacity, background,
    /// radius, and other paint-only fields update the stored style but leave
    /// geometry untouched.
    pub fn set_style(&mut self, id: LayoutId, style: ResolvedStyle) {
        let Some(entry) = self.nodes.get(&id) else {
            return;
        };
        if entry.style == style {
            return;
        }
        let layout_changed = layout_style_differs(&entry.style, &style);
        let taffy_id = entry.taffy_id;
        if layout_changed {
            let taffy_style = to_taffy_style(&style, self.scale);
            self.taffy
                .set_style(taffy_id, taffy_style)
                .expect("set taffy style");
            if let Some(text) = self.nodes.get_mut(&id).expect("layout node").text.as_mut() {
                text.style = style.text;
            }
            if let Some(context) = self.taffy.get_node_context_mut(taffy_id) {
                if let Some(text) = context.text.as_mut() {
                    text.style = style.text;
                }
            }
        }
        self.nodes.get_mut(&id).expect("layout node").style = style;
        if layout_changed {
            self.mark_dirty(id);
        }
    }

    /// Replace a node's text. A change dirties layout because text is measured.
    pub fn set_text(&mut self, id: LayoutId, text: Option<TextContent>) {
        let Some(entry) = self.nodes.get(&id) else {
            return;
        };
        if entry.text == text {
            return;
        }
        let taffy_id = entry.taffy_id;
        if let Some(context) = self.taffy.get_node_context_mut(taffy_id) {
            context.text = text.clone();
        }
        self.taffy.mark_dirty(taffy_id).expect("dirty text node");
        self.nodes.get_mut(&id).expect("layout node").text = text;
        self.mark_dirty(id);
    }

    /// Mark a node as a layout root.
    pub fn add_root(&mut self, id: LayoutId) {
        if self.nodes.contains_key(&id) && !self.roots.contains(&id) {
            self.roots.push(id);
            self.mark_dirty(id);
        }
    }

    /// Remove a layout root.
    pub fn remove_root(&mut self, id: LayoutId) {
        self.roots.retain(|root| *root != id);
    }

    /// Remove a node and its subtree.
    pub fn remove_node(&mut self, id: LayoutId) {
        self.remove_root(id);
        let Some(entry) = self.nodes.remove(&id) else {
            return;
        };
        self.dirty_nodes.remove(&id);
        self.geometry.remove(&id);
        self.changed.remove(&id);
        for child in entry.children {
            self.remove_node(child);
        }
        self.taffy
            .remove(entry.taffy_id)
            .expect("remove taffy node");
        self.invalidate_all();
    }

    /// Snapped geometry for a node after the last layout pass.
    #[must_use]
    pub fn geometry(&self, id: LayoutId) -> Option<Rect> {
        self.geometry.get(&id).copied()
    }

    /// Nodes whose geometry changed in the last layout pass.
    #[must_use]
    pub fn geometry_changes(&self) -> &HashSet<LayoutId> {
        &self.changed
    }

    /// Mark every mounted node dirty.
    pub fn invalidate_all(&mut self) {
        self.dirty_nodes.extend(self.nodes.keys().copied());
    }

    /// Run layout when needed.
    ///
    /// Returns without touching taffy when no layout-relevant change occurred
    /// since the previous pass and the viewport is unchanged. Paint-only style
    /// changes therefore cost zero layout.
    pub fn compute(&mut self, viewport: Size<f32>) -> LayoutStats {
        let viewport_changed = self.viewport != Some(viewport);
        if self.dirty_nodes.is_empty() && !viewport_changed {
            self.changed.clear();
            return LayoutStats::default();
        }

        self.viewport = Some(viewport);
        self.layout_runs += 1;
        self.changed.clear();

        let roots = self.roots.clone();
        let mut nodes = 0;
        let scale = self.scale;
        let available = TaffySize {
            width: AvailableSpace::Definite(viewport.width),
            height: AvailableSpace::Definite(viewport.height),
        };

        for root in roots.iter().copied() {
            let Some(entry) = self.nodes.get(&root) else {
                continue;
            };
            let taffy_id = entry.taffy_id;
            // A root percentage resolves against the window viewport, not an
            // absent parent.
            if matches!(entry.style.size.width, Length::Percent(_))
                || matches!(entry.style.size.height, Length::Percent(_))
            {
                let mut resolved = entry.style;
                if let Length::Percent(percent) = entry.style.size.width {
                    resolved.size.width = Length::Px(viewport.width * percent / 100.0);
                }
                if let Length::Percent(percent) = entry.style.size.height {
                    resolved.size.height = Length::Px(viewport.height * percent / 100.0);
                }
                let resolved = to_taffy_style(&resolved, scale);
                self.taffy
                    .set_style(taffy_id, resolved)
                    .expect("resolve root style");
            }
            let measure = &mut self.measure;
            if self
                .taffy
                .compute_layout_with_measure(taffy_id, available, |input, _, context, style| {
                    measure_leaf(input, context, style, Some(&mut **measure))
                })
                .is_err()
            {
                continue;
            }
            copy_geometry(
                &self.taffy,
                &self.nodes,
                root,
                0.0,
                0.0,
                scale,
                &mut self.geometry,
                &mut self.changed,
                &mut nodes,
            );
        }

        self.dirty_nodes.clear();
        LayoutStats {
            ran: true,
            roots: roots.len(),
            nodes,
        }
    }

    fn mark_dirty(&mut self, id: LayoutId) {
        if self.nodes.contains_key(&id) {
            self.dirty_nodes.insert(id);
        }
    }
}

fn measure_leaf(
    input: LayoutInput,
    context: Option<&mut NodeContext>,
    style: &Style,
    measure: Option<&mut dyn TextMeasure>,
) -> LayoutOutput {
    let text = context.as_ref().and_then(|context| context.text.as_ref());
    taffy::compute_leaf_layout(
        input,
        style,
        |_, _| 0.0,
        |known_dimensions, available_space| {
            let Some(text) = text else {
                return known_or_zero(known_dimensions);
            };
            let Some(measure) = measure else {
                return known_or_zero(known_dimensions);
            };
            let width = known_dimensions.width.or(match available_space.width {
                AvailableSpace::Definite(width) => Some(width),
                AvailableSpace::MinContent | AvailableSpace::MaxContent => None,
            });
            let request = TextMeasureRequest {
                text: text.text.clone(),
                width,
                style: text.style,
            };
            let metrics = measure.measure(&request);
            TaffySize {
                width: known_dimensions.width.unwrap_or(metrics.width),
                height: known_dimensions.height.unwrap_or(metrics.height),
            }
        },
    )
}

fn known_or_zero(size: TaffySize<Option<f32>>) -> TaffySize<f32> {
    TaffySize {
        width: size.width.unwrap_or(0.0),
        height: size.height.unwrap_or(0.0),
    }
}

#[allow(clippy::too_many_arguments)]
fn copy_geometry(
    taffy: &TaffyTree<NodeContext>,
    nodes: &HashMap<LayoutId, NodeEntry>,
    id: LayoutId,
    origin_x: f32,
    origin_y: f32,
    scale: f32,
    geometry: &mut HashMap<LayoutId, Rect>,
    changed: &mut HashSet<LayoutId>,
    count: &mut usize,
) {
    let Some(entry) = nodes.get(&id) else {
        return;
    };
    let Ok(layout) = taffy.layout(entry.taffy_id) else {
        return;
    };
    let x = origin_x + layout.location.x;
    let y = origin_y + layout.location.y;
    let rect = snap_rect(x, y, layout.size.width, layout.size.height, scale);
    *count += 1;
    if geometry.get(&id) != Some(&rect) {
        changed.insert(id);
    }
    geometry.insert(id, rect);
    for child in entry.children.iter().copied() {
        copy_geometry(taffy, nodes, child, x, y, scale, geometry, changed, count);
    }
}

/// Snap one logical coordinate to the device pixel grid.
#[must_use]
pub fn snap(value: f32, scale: f32) -> f32 {
    if !value.is_finite() || !scale.is_finite() || scale <= 0.0 {
        return value;
    }
    (value * scale).round() / scale
}

fn snap_rect(x: f32, y: f32, width: f32, height: f32, scale: f32) -> Rect {
    if !scale.is_finite() || scale <= 0.0 {
        return Rect {
            x,
            y,
            width,
            height,
        };
    }
    let left = (x * scale).round();
    let top = (y * scale).round();
    let right = ((x + width) * scale).round();
    let bottom = ((y + height) * scale).round();
    Rect {
        x: left / scale,
        y: top / scale,
        width: (right - left) / scale,
        height: (bottom - top) / scale,
    }
}

fn to_taffy_style(style: &ResolvedStyle, scale: f32) -> Style {
    Style {
        display: match style.display {
            Display::Flex => TaffyDisplay::Flex,
            Display::None => TaffyDisplay::None,
        },
        position: match style.position {
            Position::Relative => TaffyPosition::Relative,
            Position::Absolute => TaffyPosition::Absolute,
        },
        size: TaffySize {
            width: dimension(style.size.width, scale),
            height: dimension(style.size.height, scale),
        },
        padding: TaffyRect {
            left: definite(style.padding.left, scale),
            right: definite(style.padding.right, scale),
            top: definite(style.padding.top, scale),
            bottom: definite(style.padding.bottom, scale),
        },
        border: TaffyRect {
            left: LengthPercentage::length(snap(style.border.left, scale)),
            right: LengthPercentage::length(snap(style.border.right, scale)),
            top: LengthPercentage::length(snap(style.border.top, scale)),
            bottom: LengthPercentage::length(snap(style.border.bottom, scale)),
        },
        gap: TaffySize {
            width: definite(style.gap.width, scale),
            height: definite(style.gap.height, scale),
        },
        flex_direction: match style.flex.direction {
            FlexDirection::Row => TaffyDirection::Row,
            FlexDirection::Column => TaffyDirection::Column,
        },
        flex_grow: style.flex.grow,
        flex_shrink: style.flex.shrink,
        flex_basis: dimension(style.flex.basis, scale),
        align_items: match style.flex.align_items {
            Align::Start => AlignItems::START,
            Align::Center => AlignItems::CENTER,
            Align::End => AlignItems::END,
            Align::Stretch => AlignItems::STRETCH,
        },
        justify_content: match style.flex.justify_content {
            Justify::Start => JustifyContent::START,
            Justify::Center => JustifyContent::CENTER,
            Justify::End => JustifyContent::END,
            Justify::SpaceBetween => JustifyContent::SPACE_BETWEEN,
        },
        ..Style::default()
    }
}

fn dimension(length: Length, scale: f32) -> Dimension {
    match length {
        Length::Auto => Dimension::auto(),
        Length::Px(value) => Dimension::length(snap(value, scale)),
        Length::Percent(value) => Dimension::percent(value / 100.0),
    }
}

fn definite(length: DefiniteLength, scale: f32) -> LengthPercentage {
    match length {
        DefiniteLength::Px(value) => LengthPercentage::length(snap(value, scale)),
        DefiniteLength::Percent(value) => LengthPercentage::percent(value / 100.0),
    }
}

/// Equality over fields that can change layout.
///
/// Text color is deliberately excluded: recoloring text must not re-run layout.
#[must_use]
pub fn layout_style_differs(a: &ResolvedStyle, b: &ResolvedStyle) -> bool {
    a.display != b.display
        || a.position != b.position
        || a.size != b.size
        || a.padding != b.padding
        || a.gap != b.gap
        || a.flex != b.flex
        || a.border != b.border
        || text_layout_differs(a.text, b.text)
}

fn text_layout_differs(a: TextStyle, b: TextStyle) -> bool {
    a.font_size != b.font_size || a.line_height != b.line_height || a.weight != b.weight
}

/// A deterministic monospace measurer for tests and headless runs.
#[derive(Clone, Copy, Debug)]
pub struct MonospaceText {
    /// Advance width per character as a fraction of the font size.
    pub advance: f32,
}

impl Default for MonospaceText {
    fn default() -> Self {
        Self { advance: 0.6 }
    }
}

impl MonospaceText {
    /// Construct a measurer.
    #[must_use]
    pub const fn new(advance: f32) -> Self {
        Self { advance }
    }
}

impl TextMeasure for MonospaceText {
    fn measure(&mut self, request: &TextMeasureRequest) -> TextMetrics {
        let char_width = request.style.font_size * self.advance;
        let line_height = request.style.line_height.max(request.style.font_size);
        let full_width = request.text.chars().count() as f32 * char_width;
        match request.width {
            Some(max_width) if max_width > 0.0 && full_width > max_width => {
                let per_line = (max_width / char_width).floor().max(1.0);
                let lines = (request.text.chars().count() as f32 / per_line)
                    .ceil()
                    .max(1.0);
                TextMetrics {
                    width: max_width,
                    height: lines * line_height,
                }
            }
            _ => TextMetrics {
                width: full_width,
                height: line_height,
            },
        }
    }
}

/// Convenience constructor for a transparent style with the given size.
#[must_use]
pub fn fixed_size(width: f32, height: f32) -> ResolvedStyle {
    ResolvedStyle {
        size: Size {
            width: Length::Px(width),
            height: Length::Px(height),
        },
        text: TextStyle {
            color: Color::new(0.0, 0.0, 0.0, 1.0),
            ..TextStyle::default()
        },
        ..ResolvedStyle::default()
    }
}
