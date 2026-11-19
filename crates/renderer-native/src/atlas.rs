use std::collections::HashMap;

use text::GlyphImage;

/// Atlas dimensions in texels.
pub const ATLAS_SIZE: u32 = 1024;

/// Identity of a rasterized glyph at a specific size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    /// Font identity.
    pub font_id: u64,
    /// Glyph id.
    pub glyph_id: u32,
    /// Font size bits.
    pub font_size: u32,
    /// Rasterization scale bits.
    pub raster_scale: u32,
}

impl GlyphKey {
    fn from_image(image: &GlyphImage) -> Self {
        Self {
            font_id: image.font_id,
            glyph_id: image.glyph_id,
            font_size: image.font_size.to_bits(),
            raster_scale: image.raster_scale.to_bits(),
        }
    }
}

/// A glyph's position in the atlas and its logical placement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AtlasEntry {
    /// UV rectangle: left, top, right, bottom.
    pub uv: [f32; 4],
    /// Logical width.
    pub width: f32,
    /// Logical height.
    pub height: f32,
    /// Logical left placement.
    pub left: f32,
    /// Logical top placement.
    pub top: f32,
}

/// Atlas counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AtlasStats {
    /// Glyphs already present.
    pub hits: u64,
    /// Glyphs packed this frame.
    pub misses: u64,
    /// Uploads forced because the atlas was full.
    pub resets: u64,
}

/// A CPU-side shelf-packed alpha atlas.
pub struct GlyphAtlas {
    width: u32,
    height: u32,
    data: Vec<u8>,
    entries: HashMap<GlyphKey, AtlasEntry>,
    next_x: u32,
    next_y: u32,
    row_height: u32,
    dirty: bool,
    stats: AtlasStats,
}

impl GlyphAtlas {
    /// Create an atlas with a reserved white texel for solid geometry.
    #[must_use]
    pub fn new() -> Self {
        let width = ATLAS_SIZE;
        let height = ATLAS_SIZE;
        let mut data = vec![0_u8; (width * height) as usize];
        data[0] = 255;
        Self {
            width,
            height,
            data,
            entries: HashMap::new(),
            next_x: 1,
            next_y: 0,
            row_height: 0,
            dirty: true,
            stats: AtlasStats::default(),
        }
    }

    /// Texture width.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Texture height.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Atlas bytes.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Whether the texture needs an upload.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Mark the atlas clean after upload.
    pub fn clear_dirty(&mut self) {
        self.dirty = false;
    }

    /// Number of cached glyphs.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the atlas is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Counters.
    #[must_use]
    pub fn stats(&self) -> AtlasStats {
        self.stats
    }

    /// UV of the reserved white texel.
    #[must_use]
    pub fn white_uv(&self) -> [f32; 2] {
        [0.5 / self.width as f32, 0.5 / self.height as f32]
    }

    /// Pack a glyph, returning its atlas entry.
    pub fn ensure(&mut self, image: &GlyphImage) -> Option<AtlasEntry> {
        let key = GlyphKey::from_image(image);
        if let Some(entry) = self.entries.get(&key) {
            self.stats.hits += 1;
            return Some(*entry);
        }
        if image.width == 0 || image.height == 0 || image.raster_scale <= 0.0 {
            return None;
        }
        let (width, height) = (image.width, image.height);
        if width + 2 > self.width || height + 2 > self.height {
            return None;
        }
        if self.next_x + width + 1 > self.width {
            self.next_x = 1;
            self.next_y += self.row_height + 1;
            self.row_height = 0;
        }
        if self.next_y + height + 1 > self.height {
            self.reset();
            self.stats.resets += 1;
        }
        let x = self.next_x;
        let y = self.next_y;
        let required = (width * height) as usize;
        if image.data.len() < required {
            return None;
        }
        for row in 0..height {
            let source = (row * width) as usize;
            let target = ((y + row) * self.width + x) as usize;
            self.data[target..target + width as usize]
                .copy_from_slice(&image.data[source..source + width as usize]);
        }
        self.next_x += width + 1;
        self.row_height = self.row_height.max(height);
        let scale = image.raster_scale;
        let entry = AtlasEntry {
            uv: [
                x as f32 / self.width as f32,
                y as f32 / self.height as f32,
                (x + width) as f32 / self.width as f32,
                (y + height) as f32 / self.height as f32,
            ],
            width: width as f32 / scale,
            height: height as f32 / scale,
            left: image.left as f32 / scale,
            top: image.top as f32 / scale,
        };
        self.entries.insert(key, entry);
        self.dirty = true;
        self.stats.misses += 1;
        Some(entry)
    }

    fn reset(&mut self) {
        self.data.fill(0);
        self.data[0] = 255;
        self.entries.clear();
        self.next_x = 1;
        self.next_y = 0;
        self.row_height = 0;
        self.dirty = true;
    }
}

impl Default for GlyphAtlas {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn image(glyph_id: u32, width: u32, height: u32) -> GlyphImage {
        GlyphImage {
            font_id: 7,
            glyph_id,
            font_size: 16.0,
            raster_scale: 2.0,
            left: 1,
            top: 3,
            width,
            height,
            data: Arc::from(vec![200_u8; (width * height) as usize]),
        }
    }

    #[test]
    fn packs_once_and_reuses_the_entry() {
        let mut atlas = GlyphAtlas::new();
        let glyph = image(5, 8, 10);
        let first = atlas.ensure(&glyph).expect("entry");
        let second = atlas.ensure(&glyph).expect("entry");
        assert_eq!(first, second);
        assert_eq!(atlas.len(), 1);
        assert_eq!(atlas.stats().misses, 1);
        assert_eq!(atlas.stats().hits, 1);
        assert!((first.width - 4.0).abs() < 0.001);
        assert!((first.height - 5.0).abs() < 0.001);
        assert!((first.left - 0.5).abs() < 0.001);
        assert!((first.top - 1.5).abs() < 0.001);
        assert_eq!(&atlas.data()[..2], &[255, 200]);
    }

    #[test]
    fn different_sizes_are_distinct_glyphs() {
        let mut atlas = GlyphAtlas::new();
        atlas.ensure(&image(1, 4, 4)).expect("a");
        let mut larger = image(1, 4, 4);
        larger.font_size = 32.0;
        let a = atlas.ensure(&larger).expect("b");
        assert_eq!(atlas.len(), 2);
        assert!(a.uv[0] > 0.0);
    }
}
