//! CPU font rasterization and bounded grayscale glyph atlas data.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use swash::scale::{Render, ScaleContext, Source, image::Content};
use swash::{
    FontRef,
    zeno::{Angle, Format, Transform},
};

const DEFAULT_ATLAS_SIZE: u32 = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellMetrics {
    pub width: u32,
    pub height: u32,
    pub ascent: i32,
    pub descent: u32,
    pub line_gap: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlyphInfo {
    pub uv_min: [f32; 2],
    pub uv_max: [f32; 2],
    pub size: [f32; 2],
    pub bearing: [f32; 2],
    pub advance: f32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlyphUpload {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct GlyphKey {
    glyph: u16,
    bold: bool,
    italic: bool,
}

/// Owns one font face and rasterizes each glyph key at most once.
pub struct Font {
    bytes: Vec<u8>,
    face_index: usize,
    size: f32,
    metrics: CellMetrics,
    atlas_size: u32,
    atlas: Vec<u8>,
    shelf_x: u32,
    shelf_y: u32,
    shelf_height: u32,
    glyphs: HashMap<GlyphKey, (u32, GlyphInfo)>,
    context: ScaleContext,
}

impl Font {
    pub fn load(path: Option<&Path>, size: f32) -> Result<Self, String> {
        if !size.is_finite() || !(4.0..=96.0).contains(&size) {
            return Err("font size must be between 4 and 96 pixels".into());
        }
        let path = path
            .map(Path::to_path_buf)
            .or_else(default_font_path)
            .ok_or_else(|| {
                "no monospace font found; pass --font or install a system monospace font"
                    .to_string()
            })?;
        let bytes =
            fs::read(&path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let face = FontRef::from_index(&bytes, 0)
            .ok_or_else(|| format!("invalid font: {}", path.display()))?;
        let raw = face.metrics(&[]);
        let scaled = raw.scale(size);
        let glyph_metrics = face.glyph_metrics(&[]).scale(size);
        let cell_width = glyph_metrics.advance_width(face.charmap().map('M'));
        if ['i', 'W', '0', ' '].into_iter().any(|ch| {
            (glyph_metrics.advance_width(face.charmap().map(ch)) - cell_width).abs() > 0.05
        }) {
            return Err(format!("font is not monospaced: {}", path.display()));
        }
        let ascent = scaled.ascent.ceil() as i32;
        let descent = scaled.descent.abs().ceil() as u32;
        let line_gap = scaled.leading.ceil().max(0.0) as u32;
        let metrics = CellMetrics {
            width: cell_width.ceil().max(1.0) as u32,
            height: (ascent.max(0) as u32 + descent + line_gap).max(1),
            ascent,
            descent,
            line_gap,
        };
        Ok(Self {
            bytes,
            face_index: 0,
            size,
            metrics,
            atlas_size: DEFAULT_ATLAS_SIZE,
            atlas: vec![0; (DEFAULT_ATLAS_SIZE * DEFAULT_ATLAS_SIZE) as usize],
            shelf_x: 1,
            shelf_y: 1,
            shelf_height: 0,
            glyphs: HashMap::new(),
            context: ScaleContext::new(),
        })
    }

    pub const fn metrics(&self) -> CellMetrics {
        self.metrics
    }
    pub const fn atlas_size(&self) -> u32 {
        self.atlas_size
    }
    pub fn atlas_pixels(&self) -> &[u8] {
        &self.atlas
    }

    /// Returns a stable atlas index and uploads only on the first lookup.
    pub fn glyph(
        &mut self,
        ch: char,
        bold: bool,
        italic: bool,
    ) -> Result<(u32, GlyphInfo, Option<GlyphUpload>), String> {
        let face =
            FontRef::from_index(&self.bytes, self.face_index).ok_or("invalid loaded font")?;
        let glyph_id = face.charmap().map(ch);
        let key = GlyphKey {
            glyph: glyph_id,
            bold,
            italic,
        };
        if let Some((index, info)) = self.glyphs.get(&key).copied() {
            return Ok((index, info, None));
        }

        let mut scaler = self
            .context
            .builder(face)
            .size(self.size)
            .hint(true)
            .build();
        let sources = [
            Source::Outline,
            Source::Bitmap(swash::scale::StrikeWith::BestFit),
        ];
        let mut image = swash::scale::image::Image::new();
        let mut renderer = Render::new(&sources);
        renderer.format(Format::Alpha);
        renderer.embolden(if bold { 0.45 } else { 0.0 });
        if italic {
            renderer.transform(Some(Transform::skew(
                Angle::from_degrees(-12.0),
                Angle::ZERO,
            )));
        }
        if !renderer.render_into(&mut scaler, glyph_id, &mut image) {
            let info = GlyphInfo {
                uv_min: [0.0; 2],
                uv_max: [0.0; 2],
                size: [0.0; 2],
                bearing: [0.0; 2],
                advance: self.metrics.width as f32,
            };
            let index = self.glyphs.len() as u32;
            self.glyphs.insert(key, (index, info));
            return Ok((index, info, None));
        }
        let placement = image.placement;
        let width = placement.width;
        let height = placement.height;
        let advance = face
            .glyph_metrics(&[])
            .scale(self.size)
            .advance_width(glyph_id);
        let index = self.glyphs.len() as u32;
        if width == 0 || height == 0 {
            let info = GlyphInfo {
                uv_min: [0.0; 2],
                uv_max: [0.0; 2],
                size: [0.0; 2],
                bearing: [0.0; 2],
                advance,
            };
            self.glyphs.insert(key, (index, info));
            return Ok((index, info, None));
        }
        let raster_bytes = (width as usize)
            .checked_mul(height as usize)
            .ok_or("rasterized glyph dimensions overflow")?;
        if image.content != Content::Mask || image.data.len() != raster_bytes {
            return Err("font rasterizer returned an unsupported glyph format".into());
        }
        let padded_width = width.checked_add(2).ok_or("glyph width overflow")?;
        let padded_height = height.checked_add(2).ok_or("glyph height overflow")?;
        let (x, y) = self
            .allocate(padded_width, padded_height)
            .ok_or("grayscale glyph atlas is full")?;
        let x = x + 1;
        let y = y + 1;
        for row in 0..height {
            let source = (row * width) as usize;
            let target = ((y + row) * self.atlas_size + x) as usize;
            self.atlas[target..target + width as usize]
                .copy_from_slice(&image.data[source..source + width as usize]);
        }
        let info = GlyphInfo {
            uv_min: [
                x as f32 / self.atlas_size as f32,
                y as f32 / self.atlas_size as f32,
            ],
            uv_max: [
                (x + width) as f32 / self.atlas_size as f32,
                (y + height) as f32 / self.atlas_size as f32,
            ],
            size: [width as f32, height as f32],
            bearing: [placement.left as f32, placement.top as f32],
            advance,
        };
        self.glyphs.insert(key, (index, info));
        Ok((
            index,
            info,
            Some(GlyphUpload {
                x,
                y,
                width,
                height,
                pixels: image.data,
            }),
        ))
    }

    fn allocate(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        if width > self.atlas_size || height > self.atlas_size {
            return None;
        }
        if self.shelf_x + width > self.atlas_size {
            self.shelf_x = 1;
            self.shelf_y = self.shelf_y.checked_add(self.shelf_height)?;
            self.shelf_height = 0;
        }
        if self.shelf_y + height > self.atlas_size {
            return None;
        }
        let result = (self.shelf_x, self.shelf_y);
        self.shelf_x += width;
        self.shelf_height = self.shelf_height.max(height);
        Some(result)
    }
}

fn default_font_path() -> Option<PathBuf> {
    [
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/truetype/liberation2/LiberationMono-Regular.ttf",
        "/usr/share/fonts/truetype/noto/NotoSansMono-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansMono-Regular.ttf",
        "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_font_metrics_and_glyph_cache_are_stable() {
        let mut font =
            Font::load(None, 14.0).expect("Linux test environment provides a monospace font");
        let metrics = font.metrics();
        assert!(metrics.width > 0 && metrics.height >= metrics.width);
        assert!(metrics.ascent > 0 && metrics.descent > 0);
        let (first_id, first, upload) = font.glyph('A', false, false).unwrap();
        assert!(first.size[0] > 0.0 && first.size[1] > 0.0);
        let upload = upload.expect("first glyph lookup rasterizes and uploads");
        assert_eq!(upload.pixels.len(), (upload.width * upload.height) as usize);
        assert!(upload.x + upload.width <= font.atlas_size());
        let (cached_id, cached, upload) = font.glyph('A', false, false).unwrap();
        assert_eq!(first_id, cached_id);
        assert_eq!(first, cached);
        assert!(upload.is_none());
    }
}
