#![forbid(unsafe_code)]
//! Text shaping, line layout, and a bounded layout cache.
//!
//! The default [`MonospaceShaper`] is a deterministic, headless reference
//! shaper. A platform backend (parley/HarfBuzz/CoreText) can implement
//! [`Shaper`] without changing the cache, layout, or paint layers.

use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::sync::Arc;

use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache, Weight, Wrap};
use renderer_api::{TextMeasureRequest, TextMetrics, TextStyle};

/// A shaped glyph.
#[derive(Clone, Debug, PartialEq)]
pub struct Glyph {
    /// Source character.
    pub ch: char,
    /// Byte offset in the original text.
    pub cluster: usize,
    /// Horizontal offset from the line's start.
    pub x: f32,
    /// Vertical offset from the line's baseline.
    pub y: f32,
    /// Advance width.
    pub advance: f32,
    /// Shaped glyph id. Zero for the reference shaper.
    pub glyph_id: u32,
    /// Font identity for atlas keys.
    pub font_id: u64,
    /// Rasterized alpha mask, when a platform shaper produced one.
    pub image: Option<GlyphImage>,
}

/// A rasterized glyph mask in device pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct GlyphImage {
    /// Font identity.
    pub font_id: u64,
    /// Glyph id.
    pub glyph_id: u32,
    /// Font size used for rasterization.
    pub font_size: f32,
    /// Device scale used for rasterization.
    pub raster_scale: f32,
    /// Left placement relative to the glyph origin.
    pub left: i32,
    /// Top placement relative to the glyph origin.
    pub top: i32,
    /// Mask width.
    pub width: u32,
    /// Mask height.
    pub height: u32,
    /// 8-bit alpha mask.
    pub data: Arc<[u8]>,
}

impl GlyphImage {
    fn byte_size(&self) -> usize {
        std::mem::size_of::<Self>() + self.data.len()
    }
}

/// One shaped line.
#[derive(Clone, Debug, PartialEq)]
pub struct ShapedLine {
    /// Line text.
    pub text: String,
    /// Byte range in the original text.
    pub byte_range: Range<usize>,
    /// Glyphs in visual order.
    pub glyphs: Vec<Glyph>,
    /// Line width.
    pub width: f32,
    /// Baseline offset from the layout origin.
    pub baseline: f32,
}

/// A shaped paragraph or text block.
#[derive(Clone, Debug, PartialEq)]
pub struct ShapedLayout {
    /// Bounding width.
    pub width: f32,
    /// Bounding height.
    pub height: f32,
    /// Line height used for every line.
    pub line_height: f32,
    /// Ascent above the first baseline.
    pub ascent: f32,
    /// Descent below the last baseline.
    pub descent: f32,
    /// Shaped lines.
    pub lines: Vec<ShapedLine>,
}

impl ShapedLayout {
    /// Approximate heap bytes retained by this layout.
    #[must_use]
    pub fn byte_size(&self) -> usize {
        std::mem::size_of::<Self>()
            + self
                .lines
                .iter()
                .map(|line| {
                    std::mem::size_of::<ShapedLine>()
                        + line.text.len()
                        + line
                            .glyphs
                            .iter()
                            .map(|glyph| {
                                std::mem::size_of::<Glyph>()
                                    + glyph.image.as_ref().map_or(0, GlyphImage::byte_size)
                            })
                            .sum::<usize>()
                })
                .sum::<usize>()
    }
}

/// A shaping request.
#[derive(Clone, Copy, Debug)]
pub struct ShapeRequest<'a> {
    /// Source text.
    pub text: &'a str,
    /// Text style.
    pub style: TextStyle,
    /// Quantized wrapping width. `None` means no wrapping.
    pub max_width: Option<f32>,
}

/// A text shaping backend.
pub trait Shaper {
    /// Shape one text block.
    fn shape(&mut self, request: ShapeRequest<'_>) -> ShapedLayout;
    /// Backend name for diagnostics.
    fn name(&self) -> &'static str {
        "custom"
    }
}

/// A deterministic monospace shaper for headless runs and tests.
#[derive(Clone, Copy, Debug)]
pub struct MonospaceShaper {
    /// Advance width per character as a fraction of the font size.
    pub advance: f32,
}

impl Default for MonospaceShaper {
    fn default() -> Self {
        Self { advance: 0.6 }
    }
}

impl MonospaceShaper {
    /// Create a shaper with a custom advance fraction.
    #[must_use]
    pub const fn new(advance: f32) -> Self {
        Self { advance }
    }
}

impl Shaper for MonospaceShaper {
    fn shape(&mut self, request: ShapeRequest<'_>) -> ShapedLayout {
        let style = request.style;
        let font_size = style.font_size.max(0.0);
        let line_height = style.line_height.max(font_size);
        let char_width = font_size * self.advance;
        let ascent = font_size * 0.8;
        let descent = (line_height - ascent).max(0.0);
        let ranges = line_ranges(request.text, request.max_width, char_width);
        let mut lines = Vec::with_capacity(ranges.len());
        let mut width = 0.0_f32;

        for (line_index, range) in ranges.into_iter().enumerate() {
            let text = &request.text[range.clone()];
            let mut glyphs = Vec::with_capacity(text.len());
            for (index, (offset, ch)) in text.char_indices().enumerate() {
                let x = index as f32 * char_width;
                glyphs.push(Glyph {
                    ch,
                    cluster: range.start + offset,
                    x,
                    y: 0.0,
                    advance: char_width,
                    glyph_id: ch as u32,
                    font_id: 0,
                    image: None,
                });
            }
            let line_width = glyphs.len() as f32 * char_width;
            width = width.max(line_width);
            lines.push(ShapedLine {
                text: text.to_string(),
                byte_range: range,
                glyphs,
                width: line_width,
                baseline: line_index as f32 * line_height + ascent,
            });
        }

        ShapedLayout {
            width,
            height: lines.len() as f32 * line_height,
            line_height,
            ascent,
            descent,
            lines,
        }
    }

    fn name(&self) -> &'static str {
        "monospace"
    }
}

#[allow(clippy::single_range_in_vec_init)]
fn line_ranges(text: &str, max_width: Option<f32>, char_width: f32) -> Vec<Range<usize>> {
    let Some(max_width) = max_width else {
        return vec![0..text.len()];
    };
    if max_width <= 0.0 || char_width <= 0.0 {
        return vec![0..text.len()];
    }

    let mut ranges = Vec::new();
    let mut offset = 0;
    for paragraph in text.split('\n') {
        wrap_paragraph(paragraph, offset, max_width, char_width, &mut ranges);
        offset += paragraph.len() + 1;
    }
    if text.ends_with('\n') {
        ranges.push(text.len()..text.len());
    }
    if ranges.is_empty() {
        ranges.push(0..0);
    }
    ranges
}

fn wrap_paragraph(
    paragraph: &str,
    offset: usize,
    max_width: f32,
    char_width: f32,
    out: &mut Vec<Range<usize>>,
) {
    let mut line_start = offset;
    let mut line_width = 0.0_f32;
    let mut word_start = offset;
    let mut last_space = offset;

    for (relative, ch) in paragraph.char_indices() {
        let absolute = offset + relative;
        let is_space = ch.is_whitespace();
        if line_width + char_width > max_width + 0.01 && absolute > line_start && !is_space {
            let break_at = if last_space > line_start {
                last_space
            } else if word_start > line_start {
                word_start
            } else {
                absolute
            };
            out.push(line_start..break_at);
            line_start = if break_at == last_space {
                word_start
            } else {
                break_at
            };
            line_width =
                paragraph[line_start - offset..relative].chars().count() as f32 * char_width;
            word_start = absolute;
        }
        line_width += char_width;
        if is_space {
            last_space = absolute;
            word_start = absolute + ch.len_utf8();
        }
    }

    out.push(line_start..offset + paragraph.len());
}

/// A platform shaping backend backed by cosmic-text (Harfrust + font fallback)
/// and swash glyph rasterization.
pub struct CosmicShaper {
    font_system: FontSystem,
    swash: SwashCache,
}

impl Default for CosmicShaper {
    fn default() -> Self {
        Self::new()
    }
}

impl CosmicShaper {
    /// Create a shaper with the system font database.
    #[must_use]
    pub fn new() -> Self {
        Self {
            font_system: FontSystem::new(),
            swash: SwashCache::new(),
        }
    }

    /// The underlying font system.
    #[must_use]
    pub fn font_system(&self) -> &FontSystem {
        &self.font_system
    }
}

const RASTER_SCALE: f32 = 2.0;

fn font_key(font_id: cosmic_text::fontdb::ID) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    font_id.hash(&mut hasher);
    hasher.finish()
}

impl Shaper for CosmicShaper {
    fn shape(&mut self, request: ShapeRequest<'_>) -> ShapedLayout {
        let font_size = request.style.font_size.max(1.0);
        let line_height = request.style.line_height.max(font_size);
        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(font_size, line_height));
        buffer.set_size(&mut self.font_system, request.max_width, None);
        buffer.set_wrap(&mut self.font_system, Wrap::WordOrGlyph);
        let attrs = Attrs::new()
            .family(Family::SansSerif)
            .weight(Weight(request.style.weight));
        buffer.set_text(
            &mut self.font_system,
            request.text,
            &attrs,
            Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(&mut self.font_system, false);

        let mut lines = Vec::new();
        let mut width = 0.0_f32;
        let mut height = 0.0_f32;
        let mut ascent = 0.0_f32;
        let mut descent = 0.0_f32;

        for run in buffer.layout_runs() {
            let start = run.glyphs.first().map_or(0, |glyph| glyph.start);
            let end = run.glyphs.last().map_or(start, |glyph| glyph.end);
            let line_text = run.text.get(start..end).unwrap_or("");
            let mut glyphs = Vec::with_capacity(run.glyphs.len());
            for glyph in run.glyphs {
                let ch = run.text[glyph.start..glyph.end]
                    .chars()
                    .next()
                    .unwrap_or('\u{fffd}');
                let physical = glyph.physical((0.0, 0.0), RASTER_SCALE);
                let image = self
                    .swash
                    .get_image(&mut self.font_system, physical.cache_key)
                    .as_ref()
                    .and_then(|image| {
                        if image.placement.width == 0 || image.placement.height == 0 {
                            return None;
                        }
                        let data = match image.content {
                            cosmic_text::SwashContent::Mask => image.data.clone(),
                            cosmic_text::SwashContent::SubpixelMask
                            | cosmic_text::SwashContent::Color => image
                                .data
                                .chunks_exact(4)
                                .map(|pixel| pixel[3].max(pixel[0]))
                                .collect(),
                        };
                        Some(GlyphImage {
                            font_id: font_key(glyph.font_id),
                            glyph_id: u32::from(glyph.glyph_id),
                            font_size: glyph.font_size,
                            raster_scale: RASTER_SCALE,
                            left: image.placement.left,
                            top: image.placement.top,
                            width: image.placement.width,
                            height: image.placement.height,
                            data: Arc::from(data),
                        })
                    });
                glyphs.push(Glyph {
                    ch,
                    cluster: glyph.start,
                    x: glyph.x,
                    y: glyph.y,
                    advance: glyph.w,
                    glyph_id: u32::from(glyph.glyph_id),
                    font_id: font_key(glyph.font_id),
                    image,
                });
            }
            width = width.max(run.line_w);
            height = height.max(run.line_top + run.line_height);
            ascent = ascent.max(run.line_y - run.line_top);
            descent = descent.max((run.line_top + run.line_height) - run.line_y);
            lines.push(ShapedLine {
                text: line_text.to_string(),
                byte_range: start..end,
                glyphs,
                width: run.line_w,
                baseline: run.line_y,
            });
        }

        if lines.is_empty() {
            lines.push(ShapedLine {
                text: String::new(),
                byte_range: 0..0,
                glyphs: Vec::new(),
                width: 0.0,
                baseline: font_size * 0.8,
            });
            height = line_height;
            ascent = font_size * 0.8;
            descent = line_height - ascent;
        }

        ShapedLayout {
            width,
            height,
            line_height,
            ascent,
            descent,
            lines,
        }
    }

    fn name(&self) -> &'static str {
        "cosmic-text"
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct TextKey {
    text: Arc<str>,
    font_size: u32,
    line_height: u32,
    weight: u16,
    width: Option<i64>,
}

/// Cache counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// Cache hits.
    pub hits: u64,
    /// Cache misses.
    pub misses: u64,
    /// Entries evicted under the byte budget.
    pub evictions: u64,
}

struct CacheEntry {
    layout: Arc<ShapedLayout>,
    bytes: usize,
}

/// A shaping system with a bounded layout cache.
pub struct TextSystem {
    shaper: Box<dyn Shaper>,
    cache: HashMap<TextKey, CacheEntry>,
    lru: VecDeque<TextKey>,
    used_bytes: usize,
    max_bytes: usize,
    width_step: f32,
    stats: CacheStats,
}

impl TextSystem {
    /// Create a system with an 8 MB cache.
    #[must_use]
    pub fn new(shaper: Box<dyn Shaper>) -> Self {
        Self::with_budget(shaper, 8 * 1024 * 1024)
    }

    /// Create a system with a byte budget.
    #[must_use]
    pub fn with_budget(shaper: Box<dyn Shaper>, max_bytes: usize) -> Self {
        Self {
            shaper,
            cache: HashMap::new(),
            lru: VecDeque::new(),
            used_bytes: 0,
            max_bytes,
            width_step: 0.5,
            stats: CacheStats::default(),
        }
    }

    /// The shaping backend name.
    #[must_use]
    pub fn shaper_name(&self) -> &'static str {
        self.shaper.name()
    }

    /// Cache counters.
    #[must_use]
    pub fn stats(&self) -> CacheStats {
        self.stats
    }

    /// Number of cached layouts.
    #[must_use]
    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }

    /// Approximate cache bytes in use.
    #[must_use]
    pub fn cache_bytes(&self) -> usize {
        self.used_bytes
    }

    /// Set the cache byte budget.
    pub fn set_max_bytes(&mut self, max_bytes: usize) {
        self.max_bytes = max_bytes;
        self.evict(None);
    }

    /// Set the width quantization step.
    pub fn set_width_step(&mut self, step: f32) {
        if step.is_finite() && step > 0.0 {
            self.width_step = step;
        }
    }

    /// Drop every cached layout.
    pub fn clear_cache(&mut self) {
        self.cache.clear();
        self.lru.clear();
        self.used_bytes = 0;
    }

    /// Shape or reuse a layout.
    pub fn layout(
        &mut self,
        text: &str,
        style: TextStyle,
        max_width: Option<f32>,
    ) -> Arc<ShapedLayout> {
        let key = self.key(text, style, max_width);
        if let Some(layout) = self.cache.get(&key).map(|entry| Arc::clone(&entry.layout)) {
            self.stats.hits += 1;
            self.touch(&key);
            return layout;
        }

        self.stats.misses += 1;
        let shape_width = key.width.map(|width| width as f32 * self.width_step);
        let layout = Arc::new(self.shaper.shape(ShapeRequest {
            text,
            style,
            max_width: shape_width,
        }));
        let bytes = layout.byte_size();
        self.cache.insert(
            key.clone(),
            CacheEntry {
                layout: Arc::clone(&layout),
                bytes,
            },
        );
        self.lru.push_back(key.clone());
        self.used_bytes += bytes;
        self.evict(Some(&key));
        layout
    }

    /// Measure through the cache. This is the adapter used by layout.
    pub fn measure(&mut self, request: &TextMeasureRequest) -> TextMetrics {
        let layout = self.layout(&request.text, request.style, request.width);
        TextMetrics {
            width: layout.width,
            height: layout.height,
        }
    }

    fn key(&self, text: &str, style: TextStyle, max_width: Option<f32>) -> TextKey {
        TextKey {
            text: Arc::from(text),
            font_size: style.font_size.to_bits(),
            line_height: style.line_height.to_bits(),
            weight: style.weight,
            width: max_width.map(|width| (width / self.width_step).ceil() as i64),
        }
    }

    fn touch(&mut self, key: &TextKey) {
        if let Some(index) = self.lru.iter().position(|existing| existing == key) {
            self.lru.remove(index);
        }
        self.lru.push_back(key.clone());
    }

    fn evict(&mut self, keep: Option<&TextKey>) {
        while self.used_bytes > self.max_bytes && self.cache.len() > 1 {
            let Some(old) = self.lru.pop_front() else {
                break;
            };
            if keep.is_some_and(|keep| keep == &old) {
                self.lru.push_back(old);
                continue;
            }
            if let Some(entry) = self.cache.remove(&old) {
                self.used_bytes = self.used_bytes.saturating_sub(entry.bytes);
                self.stats.evictions += 1;
            }
        }
    }
}

impl Default for TextSystem {
    fn default() -> Self {
        Self::new(Box::new(MonospaceShaper::default()))
    }
}
