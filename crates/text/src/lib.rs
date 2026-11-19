#![forbid(unsafe_code)]
//! Text shaping, line layout, and a bounded layout cache.
//!
//! The default [`MonospaceShaper`] is a deterministic, headless reference
//! shaper. A platform backend (parley/HarfBuzz/CoreText) can implement
//! [`Shaper`] without changing the cache, layout, or paint layers.

use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::sync::Arc;

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
    /// Advance width.
    pub advance: f32,
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
                        + line.glyphs.len() * std::mem::size_of::<Glyph>()
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
                    advance: char_width,
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
