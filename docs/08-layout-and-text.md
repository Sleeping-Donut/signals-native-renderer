# 08 — Layout and Text

**Crates:** `layout`, `text` (may be modules of `renderer-native` in v0.1)
**Depends on:** `view` types; taffy; a platform text backend
**Owns:** style resolution, layout computation, text measurement/shaping,
text caching, images, and scroll geometry.

**Implementation status (2026-11-19).** The layout slice is in
[`crates/layout`](../crates/layout/src/lib.rs): a persistent taffy tree keyed by
`LayoutId`, `ResolvedStyle` translation, a `TextMeasure` trait, layout-dirty
tracking, pixel snapping at the device scale, and per-node geometry changes.
[`view`](../crates/view/src/lib.rs) mounts layout nodes and runs the pass after
bindings flush. Text shaping and caching are in
[`crates/text`](../crates/text/src/lib.rs): a pluggable `Shaper` trait, a
deterministic monospace backend, a real `CosmicShaper` (Harfrust shaping, system
font fallback, swash glyph masks), line wrapping, glyph positions, width
quantization, cache statistics, and an LRU byte budget. `view` feeds the cache
into layout and can be constructed with a platform shaper via
`ViewTree::new_with_shaper`. The scene builder and paint cache are in
[`crates/scene`](../crates/scene/src/lib.rs). The native wgpu backend samples a
shelf-packed alpha glyph atlas for real shaped text. The window loop and
surface backend are in [`crates/renderer-native`](../crates/renderer-native/src/lib.rs).

---

## 1. Layout model

We use **taffy** for flexbox/grid layout. It is the same engine GPUI uses
(`refs/zed/crates/gpui/src/taffy.rs`), and it is mature, pure Rust, and
incremental-friendly enough for our first version.

### 1.1 Style

The public style API is builder-based and Tailwind-like:

```rust
div()
    .flex()
    .flex_col()
    .gap_2()
    .p_4()
    .w_full()
    .bg(colors::neutral_900)
    .rounded_md()
    .child(text("Hello"))
```

Resolution produces a `ResolvedStyle`:

```rust
pub struct ResolvedStyle {
    pub display: Display,
    pub position: Position,
    pub size: Size<Length>,
    pub min_size: Size<Length>,
    pub max_size: Size<Length>,
    pub padding: Edges<DefiniteLength>,
    pub margin: Edges<Length>,
    pub border: Edges<AbsoluteLength>,
    pub gap: Size<DefiniteLength>,
    pub flex: FlexStyle,
    pub grid: Option<GridStyle>,
    pub overflow: Point<Overflow>,
    pub background: Option<Fill>,
    pub border_color: Option<Hsla>,
    pub corner_radii: Corners<AbsoluteLength>,
    pub shadow: Vec<BoxShadow>,
    pub text: TextStyle,
    pub opacity: f32,
    pub visibility: Visibility,
}
```

Lengths are resolved in this order:

1. percentages against the parent's definite size (or taffy's percentage
   resolution);
2. `rem` against the root font size;
3. `px` logical pixels;
4. `auto`.

`ResolvedStyle` is stored on the view node. The taffy style is derived from it
and cached; a style change updates only the changed taffy fields if possible,
otherwise it rebuilds the taffy style struct (taffy styles are plain structs, so
copying is cheap).

### 1.2 Persistent taffy tree

```rust
pub struct LayoutEngine {
    taffy: TaffyTree<NodeContext>,
    node_map: HashMap<ViewNodeId, taffy::NodeId>,
    dirty_roots: SmallVec<[ViewNodeId; 8]>,
    measure: TextMeasure,
}
```

- Each view node has a taffy node created at mount and removed at disposal.
- Style changes call `taffy.set_style(node, style)`.
- Child changes call `taffy.set_children(node, &children)`.
- A layout-dirty node is added to `dirty_roots`; the phase computes the union of
  its dirty subtree.

Unlike GPUI, which clears the layout engine every draw and rebuilds it
(`window.rs`: `self.layout_engine.as_mut().unwrap().clear()`), we keep it. This
avoids re-creating taffy nodes and re-resolving styles every frame.

### 1.3 Layout roots and dirty propagation

A layout root is:

- the window root, or
- a node marked `layout_root` (fixed size, `contain: layout`-like semantics).

When a style change affects size/position:

1. Mark the node `NEEDS_LAYOUT`.
2. Walk ancestors until a layout root; mark each `NEEDS_LAYOUT`.
3. Add the highest marked node to `dirty_roots`.

When a paint-only change occurs, do not mark layout.

When children change, mark `NEEDS_CHILDREN` + `NEEDS_LAYOUT` and add the parent.

### 1.4 Running layout

```
for root in dirty_roots:
    taffy.compute_layout(root, available_space)
    copy_geometry(root_subtree)
```

`compute_layout` is recursive over the root's subtree; taffy handles flex/grid
and calls our measure callback for text and images. After it returns, we walk the
subtree and copy `Layout` (position, size, content size, scroll size) to view
nodes, marking paint dirty where geometry changed.

The available space for a root:

- window root: the viewport size;
- layout-boundary node: its resolved size (definite) or the parent's available
  space if auto.

### 1.5 Incremental layout (later)

Taffy does not offer a general "recompute only this subtree" API. Options:

1. **Layout roots**: fixed-size components recompute independently. This is the
   v0.1 optimization and covers most app structure.
2. **Measure cache**: text/image measurement is cached; a re-layout of unchanged
   text is cheap.
3. **Dirty subtree layout**: for a node whose size is independent of its children
   (e.g. a fixed-size container), compute only its subtree and propagate its size
   up if it changed. This requires a custom layout pass or taffy's low-level API;
   prototype in Phase 8.
4. **Full layout**: always correct, O(n) per layout-dirty frame. Acceptable for
   v0.1.

Do not promise incremental flex/grid layout in v0.1. Measure the demo: if layout is
< 20% of frame time, the complexity is not justified.

### 1.6 Pixel snapping

Follow GPUI's approach:

- snap absolute lengths (borders, padding, gaps, explicit sizes) to device pixels
  before layout;
- after layout, snap node edges to device pixels, with midpoints rounded toward
  zero;
- return logical pixels to view nodes by dividing by scale factor.

This avoids blurry 1px borders at 1.5×/2× and keeps adjacent edges closed.

### 1.7 Overflow and scroll

- `overflow: visible|hidden|scroll|auto` maps to taffy overflow and to a
  `ContentMask` on paint.
- `scroll` and `auto` create a `ScrollState` on the view node: offset, viewport
  size, content size.
- Scrolling changes only the offset; children are laid out once and translated
  by `-offset` during paint. It does not re-run layout.
- Scrollbars are painted by the renderer, not the layout engine. A scrollbar
  consumes no layout space in v0.1 (overlay style); later, `scrollbar-gutter` can
  reserve space.
- Virtual lists use the content size to compute the visible range.

## 2. Text

Text is the hardest part of a native UI framework and the most visible. Treat it
as a first-class subsystem, not a paint detail.

### 2.1 Text model

```rust
pub struct TextStyle {
    pub font: FontId,
    pub size: Px,
    pub weight: FontWeight,
    pub style: FontStyle,
    pub line_height: LineHeight,
    pub letter_spacing: Px,
    pub color: Hsla,
    pub decoration: Option<TextDecoration>,
    pub features: FontFeatures,
}

pub struct TextNode {
    pub content: String,
    pub style: TextStyle,
    pub wrap: TextWrap,          // NoWrap | Wrap | Ellipsis
    pub align: TextAlign,
    pub runs: Vec<TextRun>,      // optional styled spans
}
```

A text node's intrinsic size:

- with `NoWrap`: the shaped advance width and line height;
- with `Wrap`: the min/max content width for the available width; height depends
  on the number of lines.

Text measurement is called from taffy's measure callback. It must not run
bindings or mutate the graph.

### 2.2 Shaping

Use a shaping engine that supports:

- font fallback per grapheme cluster (including emoji and CJK);
- bidi;
- ligatures and kerning;
- variable fonts and font features;
- line breaking (Unicode UAX #14) and grapheme clusters.

Candidates: `parley` + `swash` + `fontique` (the Druid/Xilem lineage),
`cosmic-text`, or a direct HarfBuzz binding. GPUI's `TextSystem` wraps platform
APIs (CoreText, DirectWrite, fontconfig) and shapes with a platform shaper; that
is the fastest path on each OS but triples the implementation.

Recommendation for v0.1:

- Use **parley/fontique/swash** for cross-platform shaping and fallback.
- Keep a `TextSystem` trait so a platform-specific backend can replace it later.
- Store shaped output in a cache keyed by `(text, style, available_width,
  shape_options)`.

### 2.3 Caching

```rust
pub struct ShapedLine {
    pub text: Arc<str>,
    pub runs: Vec<ShapedRun>,       // glyphs + positions + font ids
    pub width: Px,
    pub height: Px,
    pub baseline: Px,
    pub ascent: Px,
    pub descent: Px,
    pub line_breaks: Vec<usize>,    // byte offsets
}

pub struct TextCache {
    lines: Lru<(TextKey, Px), Arc<ShapedLine>>,
    layouts: Lru<LayoutKey, Arc<TextLayout>>,
}
```

- `TextKey` hashes the content and style.
- `Px` width is quantized to device pixels or a small step (e.g. 0.5 px) to avoid
  cache misses on subpixel jitter.
- An LRU with a byte budget (e.g. 32 MB) bounds memory.
- Reuse line layouts across frames; a clean text node replays its paint range and
  does not shape at all.

GPUI's `TextSystem` has a line layout cache and a `LineWrapper` for incremental
wrapping (`refs/zed/crates/gpui/src/text_system.rs`). That is the reference for
cache keys and eviction.

### 2.4 Paint

Text is painted as glyph runs (atlas sprites) or as a path in the backend. The
scene primitive is a `GlyphRun`:

```rust
pub struct GlyphRun {
    pub font: FontId,
    pub size: Px,
    pub color: Hsla,
    pub origin: Point<Px>,
    pub glyphs: Arc<[Glyph]>,
    pub bounds: Bounds<Px>,
}
```

The backend owns the glyph atlas. The scene builder emits runs in paint order;
the backend batches by font/atlas/texture.

Selection and caret are separate overlay primitives; do not rebuild the text
layout to paint them.

### 2.5 Editing and IME

Text editing is a higher-level component:

- `TextEdit` holds a `TextBuffer` (rope or `String` + cursor/selection), a
  `FocusHandle`, and a shaped layout.
- It registers an IME input handler with the platform.
- Composition text is painted as an underline overlay; the buffer is not
  committed until composition ends.
- Cursor motion uses shaped cluster boundaries, not byte offsets.

IME is platform-specific and belongs in `platform`, with a trait:

```rust
pub trait TextInputHandler {
    fn insert_text(&mut self, text: &str);
    fn set_composition(&mut self, text: &str, cursor: Option<Range<usize>>);
    fn delete_backward(&mut self);
    fn delete_forward(&mut self);
    fn selection(&self) -> Option<Range<usize>>;
    fn set_selection(&mut self, range: Range<usize>);
}
```

## 3. Images and SVG

- `Image` holds an `ImageId`, decoded pixels, and an intrinsic size.
- The backend uploads to a texture; the scene primitive references the texture
  and a sampling mode.
- `ObjectFit` (fill/contain/cover/scale-down/none) computes the painted rect
  without affecting layout (unless the image has an intrinsic size in layout).
- SVG: rasterize at the needed size, cache by `(asset, size, scale)`. GPUI has
  `svg_renderer.rs`; we can use `resvg`/`usvg` for v0.1.

## 4. Style resolution details

- **Inheritance**: text properties (font, color, line height) inherit; layout and
  paint properties do not.
- **Refinement**: builder calls produce a `StyleRefinement` (all fields optional)
  that is applied over the parent's style. GPUI uses `refineable`; we can hand
  write the merge or use a small derive.
- **Resolution timing**: at the start of layout for dirty nodes, not at binding
  time. A binding sets a refinement; resolution combines it with ancestors.
- **Caching**: a node's resolved style is cached with a generation. If neither
  its refinement nor any inherited style changed, reuse it. Inherited changes
  mark the subtree style-dirty.

## 5. Performance

| Operation | Target |
|---|---|
| Style resolve, 1k nodes | < 1 ms |
| Taffy layout, 10k simple nodes | < 20 ms |
| Text shape, 1k short lines | < 5 ms cold, ≈0 warm |
| Text measure, cache hit | < 100 ns |
| Paint emit, 10k primitives | < 5 ms |
| Scene replay, clean subtree | O(primitives replayed), no shaping |

Measure with `criterion` and a frame profiler. The biggest risks are text shaping
and layout of deep trees; both have caches and layout roots as mitigations.

## 6. Tests

- **Layout correctness**: compare selected cases against a browser/taffy
  reference: flex grow/shrink, gaps, padding, borders, min/max, absolute
  positioning, grid tracks.
- **Pixel snapping**: 1px border at 1×, 1.5×, 2×; adjacent edges meet exactly.
- **Dirty layout**: a paint-only change runs zero layout; a padding change runs
  layout for the subtree; a text change with auto width runs layout.
- **Text**: fallback (Latin + CJK + emoji), bidi, ligatures, wrapping, ellipsis,
  line height, letter spacing.
- **Cache**: same text/style/width hits; different width misses; eviction under
  budget.
- **IME**: composition insert/cancel/commit; cursor at cluster boundaries.
- **Images**: object fit math; SVG raster cache.
- **Fuzz**: random style trees; layout terminates; no NaN/negative sizes.

## 7. Open questions

1. **Text engine.** parley vs cosmic-text vs platform. Prototype both on the
   demo's text and pick by shaping quality and frame time.
2. **Persistent taffy styles.** Does taffy support cheap style updates, or should
   we rebuild the style struct on every change? Benchmark.
3. **Incremental layout.** Is a custom dirty-subtree pass worth it, or are layout
   roots enough? Decide after Phase 5 measurements.
4. **Text default width.** `auto` (content-sized) vs `100%` (parent). For UI
   labels, content-sized is natural but makes text changes layout changes. Maybe
   default text to content-sized and let containers stretch it.
5. **Subpixel text.** Do we support LCD subpixel antialiasing, or grayscale only?
   Grayscale is simpler and works with transparency; subpixel is sharper on
   legacy displays.
6. **Font loading.** Bundle a default font, or rely on system fonts? For
   deterministic tests, bundle a test font; for apps, system fonts with fallback.
