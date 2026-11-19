#![forbid(unsafe_code)]
//! Scene primitives, retained paint ranges, replay, and damage.
//!
//! The view tree owns node identity and calls [`SceneBuilder::mark_dirty`] when a
//! node's paint changes. The builder records each node's primitive range in the
//! previous scene; a clean node replays that range instead of re-emitting its
//! subtree. A dirty node re-emits itself and its children.

use std::collections::{HashMap, HashSet};
use std::fmt::Debug;
use std::hash::Hash;
use std::ops::Range;
use std::sync::Arc;

use renderer_api::{Color, Edges, Transform};
use text::ShapedLayout;

/// Scene node identity.
pub type SceneNodeId = u64;

/// A logical rectangle.
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
    /// Construct a rectangle.
    #[must_use]
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

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

    /// Whether the rectangle has no area.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }

    /// Whether two rectangles overlap.
    #[must_use]
    pub fn intersects(self, other: Self) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }

    /// Whether a point lies inside the rectangle.
    #[must_use]
    pub fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.right() && y >= self.y && y <= self.bottom()
    }

    /// The overlap of two rectangles, if any.
    #[must_use]
    pub fn intersect(self, other: Self) -> Option<Self> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        if right <= x || bottom <= y {
            None
        } else {
            Some(Self::new(x, y, right - x, bottom - y))
        }
    }

    /// Apply a translation/scale transform.
    #[must_use]
    pub fn transformed(self, transform: Transform) -> Self {
        let x0 = transform.apply_x(self.x);
        let y0 = transform.apply_y(self.y);
        let x1 = transform.apply_x(self.right());
        let y1 = transform.apply_y(self.bottom());
        Self::new(x0.min(x1), y0.min(y1), (x1 - x0).abs(), (y1 - y0).abs())
    }

    /// The smallest rectangle containing both inputs.
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Self::new(x, y, right - x, bottom - y)
    }
}

/// A paint primitive.
#[derive(Clone, Debug, PartialEq)]
pub enum Primitive {
    /// A filled rectangle.
    Fill {
        /// Owning scene node.
        node: SceneNodeId,
        /// Painted rectangle.
        rect: Rect,
        /// Fill color.
        color: Color,
        /// Corner radii.
        radius: Edges<f32>,
        /// Ancestor clip in screen coordinates.
        clip: Option<Rect>,
    },
    /// A border.
    Border {
        /// Owning scene node.
        node: SceneNodeId,
        /// Outer rectangle.
        rect: Rect,
        /// Border widths.
        width: Edges<f32>,
        /// Border color.
        color: Color,
        /// Corner radii.
        radius: Edges<f32>,
        /// Ancestor clip in screen coordinates.
        clip: Option<Rect>,
    },
    /// A shaped text run.
    Text {
        /// Owning scene node.
        node: SceneNodeId,
        /// Layout origin and bounds.
        rect: Rect,
        /// Shaped layout shared with the text cache.
        layout: Arc<ShapedLayout>,
        /// Text color.
        color: Color,
        /// Ancestor clip in screen coordinates.
        clip: Option<Rect>,
        /// Paint transform. Text glyph positions are transformed by this.
        transform: Transform,
        /// Whether to draw a composition underline.
        underline: bool,
    },
    /// A scrollbar track and thumb.
    Scrollbar {
        /// Owning scene node.
        node: SceneNodeId,
        /// Track rectangle.
        track: Rect,
        /// Thumb rectangle.
        thumb: Rect,
        /// Track color.
        track_color: Color,
        /// Thumb color.
        thumb_color: Color,
        /// Corner radii.
        radius: Edges<f32>,
    },
    /// A tooltip overlay with shaped text and a background.
    Tooltip {
        /// Owning scene node.
        node: SceneNodeId,
        /// Outer tooltip rectangle.
        rect: Rect,
        /// Shaped tooltip text.
        layout: Arc<ShapedLayout>,
        /// Text color.
        color: Color,
        /// Background color.
        background: Color,
        /// Border color.
        border: Color,
        /// Inner padding.
        padding: Edges<f32>,
        /// Corner radii.
        radius: Edges<f32>,
        /// Ancestor clip in screen coordinates.
        clip: Option<Rect>,
    },
    /// A text caret.
    Caret {
        /// Owning scene node.
        node: SceneNodeId,
        /// Caret rectangle.
        rect: Rect,
        /// Caret color.
        color: Color,
        /// Ancestor clip in screen coordinates.
        clip: Option<Rect>,
    },
}

impl Primitive {
    /// The owning scene node.
    #[must_use]
    pub fn node(&self) -> SceneNodeId {
        match self {
            Self::Fill { node, .. }
            | Self::Border { node, .. }
            | Self::Text { node, .. }
            | Self::Scrollbar { node, .. }
            | Self::Tooltip { node, .. }
            | Self::Caret { node, .. } => *node,
        }
    }

    /// The painted bounds.
    #[must_use]
    pub fn bounds(&self) -> Rect {
        match self {
            Self::Fill { rect, .. }
            | Self::Border { rect, .. }
            | Self::Text { rect, .. }
            | Self::Tooltip { rect, .. }
            | Self::Caret { rect, .. } => *rect,
            Self::Scrollbar { track, thumb, .. } => track.union(*thumb),
        }
    }
}

/// An ordered list of paint primitives.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scene {
    ops: Vec<Primitive>,
}

impl Scene {
    /// Create an empty scene.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of primitives.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Whether the scene has no primitives.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// The primitives in paint order.
    #[must_use]
    pub fn ops(&self) -> &[Primitive] {
        &self.ops
    }

    /// Append a primitive.
    pub fn push(&mut self, primitive: Primitive) {
        self.ops.push(primitive);
    }

    /// Append a copy of a range from another scene.
    pub fn replay_from(&mut self, source: &Self, range: Range<usize>) {
        let start = range.start.min(source.ops.len());
        let end = range.end.min(source.ops.len()).max(start);
        self.ops.extend_from_slice(&source.ops[start..end]);
    }

    /// Remove every primitive without dropping capacity.
    pub fn clear(&mut self) {
        self.ops.clear();
    }
}

/// A node range captured from the previous scene.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PaintCache {
    /// Primitive range in the previous scene.
    pub range: Range<usize>,
    /// Node bounds when the range was captured.
    pub bounds: Rect,
    /// Whether the range is valid.
    pub valid: bool,
}

/// Paint build counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SceneStats {
    /// Nodes whose previous range was replayed.
    pub replayed: usize,
    /// Nodes that re-emitted themselves and their children.
    pub rebuilt: usize,
    /// Primitives in the resulting scene.
    pub primitives: usize,
    /// Damage rectangles.
    pub damage_rects: usize,
}

/// The tree the scene builder paints.
pub trait PaintTree {
    /// Node identity.
    type Node: Copy + Eq + Hash + Debug;

    /// Parent of a node.
    fn parent(&self, node: Self::Node) -> Option<Self::Node>;
    /// Children in paint order.
    fn children(&self, node: Self::Node) -> Vec<Self::Node>;
    /// Node bounds.
    fn bounds(&self, node: Self::Node) -> Rect;
    /// Emit the node's own primitives.
    fn paint(&self, node: Self::Node, out: &mut Vec<Primitive>);
}

/// A builder that replays clean paint ranges and rebuilds dirty subtrees.
pub struct SceneBuilder<N> {
    previous: Scene,
    current: Scene,
    caches: HashMap<N, PaintCache>,
    dirty: HashSet<N>,
    damage: Vec<Rect>,
    stats: SceneStats,
}

impl<N: Copy + Eq + Hash + Debug> Default for SceneBuilder<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N: Copy + Eq + Hash + Debug> SceneBuilder<N> {
    /// Create an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self {
            previous: Scene::new(),
            current: Scene::new(),
            caches: HashMap::new(),
            dirty: HashSet::new(),
            damage: Vec::new(),
            stats: SceneStats::default(),
        }
    }

    /// The scene from the most recent build.
    #[must_use]
    pub fn scene(&self) -> &Scene {
        &self.previous
    }

    /// Counters from the most recent build.
    #[must_use]
    pub fn stats(&self) -> SceneStats {
        self.stats
    }

    /// Current damage rectangles.
    #[must_use]
    pub fn damage(&self) -> &[Rect] {
        &self.damage
    }

    /// Take and clear damage rectangles.
    pub fn take_damage(&mut self) -> Vec<Rect> {
        std::mem::take(&mut self.damage)
    }

    /// The cached paint range for a node.
    #[must_use]
    pub fn cache(&self, node: N) -> Option<&PaintCache> {
        self.caches.get(&node)
    }

    /// Drop every cached range. The next build rebuilds the full tree.
    pub fn clear_caches(&mut self) {
        self.caches.clear();
    }

    /// Drop a removed node's cached range and dirty flag.
    pub fn remove(&mut self, node: N) {
        self.caches.remove(&node);
        self.dirty.remove(&node);
    }

    /// Mark a node dirty and propagate the dirty flag to its ancestors.
    ///
    /// Only the explicitly dirty node contributes damage; ancestors are marked
    /// dirty so a cached parent range is not replayed over a changed child.
    pub fn mark_dirty<T>(&mut self, tree: &T, node: N)
    where
        T: PaintTree<Node = N>,
    {
        self.add_damage(tree.bounds(node));
        let mut current = Some(node);
        while let Some(node) = current {
            self.dirty.insert(node);
            current = tree.parent(node);
        }
    }

    /// Build the scene for the given roots.
    pub fn build<T>(&mut self, tree: &T, roots: &[N]) -> &Scene
    where
        T: PaintTree<Node = N>,
    {
        self.current.clear();
        self.stats = SceneStats::default();

        if self.caches.is_empty() {
            for root in roots {
                self.add_damage(tree.bounds(*root));
            }
        }

        for root in roots {
            self.build_node(tree, *root);
        }

        std::mem::swap(&mut self.previous, &mut self.current);
        self.dirty.clear();
        self.stats.primitives = self.previous.len();
        self.stats.damage_rects = self.damage.len();
        &self.previous
    }

    fn build_node<T>(&mut self, tree: &T, node: N)
    where
        T: PaintTree<Node = N>,
    {
        if let Some(cache) = self.caches.get(&node).cloned() {
            if cache.valid && !self.dirty.contains(&node) {
                let start = self.current.len();
                self.current.replay_from(&self.previous, cache.range);
                let end = self.current.len();
                if let Some(cache) = self.caches.get_mut(&node) {
                    cache.range = start..end;
                }
                self.stats.replayed += 1;
                return;
            }
        }

        let start = self.current.len();
        tree.paint(node, self.current.ops_mut());
        for child in tree.children(node) {
            self.build_node(tree, child);
        }
        let end = self.current.len();
        self.caches.insert(
            node,
            PaintCache {
                range: start..end,
                bounds: tree.bounds(node),
                valid: true,
            },
        );
        self.stats.rebuilt += 1;
    }

    fn add_damage(&mut self, rect: Rect) {
        if rect.is_empty() {
            return;
        }
        for existing in &mut self.damage {
            if existing.intersects(rect) {
                *existing = existing.union(rect);
                return;
            }
        }
        self.damage.push(rect);
    }
}

impl Scene {
    fn ops_mut(&mut self) -> &mut Vec<Primitive> {
        &mut self.ops
    }
}
