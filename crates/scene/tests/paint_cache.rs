//! Paint caching, replay, damage, and full-rebuild equivalence.

use std::collections::HashMap;
use std::sync::Arc;

use renderer_api::{Color, Edges, TextStyle, Transform};
use scene::{PaintTree, Primitive, Rect, SceneBuilder};
use text::{MonospaceShaper, ShapedLayout, TextSystem};

#[derive(Clone)]
struct Node {
    parent: Option<u64>,
    children: Vec<u64>,
    bounds: Rect,
    color: Color,
    text_color: Color,
    layout: Option<Arc<ShapedLayout>>,
}

#[derive(Clone, Default)]
struct Tree {
    nodes: HashMap<u64, Node>,
}

impl Tree {
    fn new() -> Self {
        let mut tree = Self::default();
        tree.insert(1, None, vec![2], Rect::new(0.0, 0.0, 100.0, 100.0));
        tree.insert(2, Some(1), vec![3], Rect::new(0.0, 0.0, 100.0, 50.0));
        tree.insert(3, Some(2), vec![], Rect::new(0.0, 0.0, 50.0, 20.0));
        tree
    }

    fn insert(&mut self, id: u64, parent: Option<u64>, children: Vec<u64>, bounds: Rect) {
        self.nodes.insert(
            id,
            Node {
                parent,
                children,
                bounds,
                color: Color::new(0.2, 0.2, 0.2, 1.0),
                text_color: Color::new(0.0, 0.0, 0.0, 1.0),
                layout: None,
            },
        );
    }

    fn set_color(&mut self, id: u64, color: Color) {
        self.nodes.get_mut(&id).expect("node").color = color;
    }

    fn set_layout(&mut self, id: u64, layout: Option<Arc<ShapedLayout>>) {
        self.nodes.get_mut(&id).expect("node").layout = layout;
    }
}

impl PaintTree for Tree {
    type Node = u64;

    fn parent(&self, node: Self::Node) -> Option<Self::Node> {
        self.nodes.get(&node).and_then(|node| node.parent)
    }

    fn children(&self, node: Self::Node) -> Vec<Self::Node> {
        self.nodes
            .get(&node)
            .map(|node| node.children.clone())
            .unwrap_or_default()
    }

    fn bounds(&self, node: Self::Node) -> Rect {
        self.nodes
            .get(&node)
            .map(|node| node.bounds)
            .unwrap_or_default()
    }

    fn paint(&self, node: Self::Node, out: &mut Vec<Primitive>) {
        let Some(data) = self.nodes.get(&node) else {
            return;
        };
        out.push(Primitive::Fill {
            node,
            rect: data.bounds,
            color: data.color,
            radius: Edges::default(),
            clip: None,
        });
        if let Some(layout) = &data.layout {
            out.push(Primitive::Text {
                node,
                rect: data.bounds,
                layout: Arc::clone(layout),
                color: data.text_color,
                clip: None,
                transform: Transform::IDENTITY,
                underline: false,
            });
        }
    }
}

fn red() -> Color {
    Color::new(1.0, 0.0, 0.0, 1.0)
}

#[test]
fn first_build_emits_every_node_and_later_frames_replay() {
    let tree = Tree::new();
    let mut builder = SceneBuilder::new();

    builder.build(&tree, &[1]);
    assert_eq!(builder.scene().len(), 3);
    assert_eq!(builder.stats().rebuilt, 3);
    assert_eq!(builder.stats().replayed, 0);
    assert_eq!(builder.stats().primitives, 3);
    assert_eq!(builder.stats().damage_rects, 1);
    builder.take_damage();

    builder.build(&tree, &[1]);
    assert_eq!(builder.scene().len(), 3);
    assert_eq!(builder.stats().rebuilt, 0);
    assert_eq!(
        builder.stats().replayed,
        1,
        "the root range replays the subtree"
    );
    assert!(builder.damage().is_empty());
}

#[test]
fn paint_change_rebuilds_the_dirty_path_and_replays_siblings() {
    let mut tree = Tree::new();
    tree.insert(4, Some(2), vec![], Rect::new(0.0, 25.0, 50.0, 20.0));
    tree.nodes.get_mut(&2).unwrap().children = vec![3, 4];
    let mut builder = SceneBuilder::new();
    builder.build(&tree, &[1]);
    builder.take_damage();

    tree.set_color(3, red());
    builder.mark_dirty(&tree, 3);
    builder.build(&tree, &[1]);

    assert_eq!(builder.stats().rebuilt, 3, "root, parent, and leaf re-emit");
    assert_eq!(builder.stats().replayed, 1, "the clean sibling replays");
    assert_eq!(builder.scene().len(), 4);
    assert!(matches!(
        builder.scene().ops()[2],
        Primitive::Fill { node: 3, color, .. } if color == red()
    ));
    assert_eq!(builder.damage().len(), 1);
    assert_eq!(builder.damage()[0], Rect::new(0.0, 0.0, 50.0, 20.0));
}

#[test]
fn incremental_and_full_builds_are_identical() {
    let mut tree = Tree::new();
    let mut incremental = SceneBuilder::new();
    incremental.build(&tree, &[1]);

    tree.set_color(3, red());
    incremental.mark_dirty(&tree, 3);
    incremental.build(&tree, &[1]);

    let mut full = SceneBuilder::new();
    let full_scene = full.build(&tree, &[1]).clone();
    assert_eq!(incremental.scene(), &full_scene);
}

#[test]
fn ranges_are_updated_when_a_node_emits_more_primitives() {
    let mut text = TextSystem::new(Box::new(MonospaceShaper::default()));
    let layout = text.layout(
        "hi",
        TextStyle {
            color: Color::new(0.0, 0.0, 0.0, 1.0),
            font_size: 16.0,
            line_height: 20.0,
            weight: 400,
        },
        None,
    );
    let mut tree = Tree::new();
    let mut builder = SceneBuilder::new();
    builder.build(&tree, &[1]);

    tree.set_layout(3, Some(Arc::clone(&layout)));
    builder.mark_dirty(&tree, 3);
    builder.build(&tree, &[1]);
    assert_eq!(builder.scene().len(), 4);
    assert!(builder
        .scene()
        .ops()
        .iter()
        .any(|op| matches!(op, Primitive::Text { node: 3, .. })));

    // A clean frame must replay the corrected range, not the old one.
    builder.build(&tree, &[1]);
    assert_eq!(builder.stats().rebuilt, 0);
    assert_eq!(builder.stats().replayed, 1);
    assert_eq!(builder.scene().len(), 4);

    let mut full = SceneBuilder::new();
    assert_eq!(builder.scene(), full.build(&tree, &[1]));
}

#[test]
fn structural_change_rebuilds_the_parent_and_follows_child_order() {
    let mut tree = Tree::new();
    tree.insert(4, Some(2), vec![], Rect::new(0.0, 25.0, 50.0, 20.0));
    tree.nodes.get_mut(&2).unwrap().children = vec![4, 3];

    let mut builder = SceneBuilder::new();
    builder.build(&tree, &[1]);
    builder.take_damage();

    tree.nodes.get_mut(&2).unwrap().children = vec![3, 4];
    builder.mark_dirty(&tree, 2);
    builder.build(&tree, &[1]);

    let nodes: Vec<u64> = builder.scene().ops().iter().map(Primitive::node).collect();
    assert_eq!(nodes, vec![1, 2, 3, 4]);
    assert_eq!(builder.stats().rebuilt, 2, "root and parent re-emit");
    assert_eq!(builder.stats().replayed, 2, "both leaves replay");
}

#[test]
fn clear_caches_forces_a_full_rebuild() {
    let tree = Tree::new();
    let mut builder = SceneBuilder::new();
    builder.build(&tree, &[1]);
    builder.clear_caches();
    builder.build(&tree, &[1]);
    assert_eq!(builder.scene().len(), 3);
    assert_eq!(builder.stats().rebuilt, 3);
    assert_eq!(builder.stats().replayed, 0);
}
