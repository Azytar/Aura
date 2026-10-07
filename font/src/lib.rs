//! CPU font rasterization and bounded grayscale glyph atlas data.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use swash::scale::{Render, ScaleContext, Source, image::Content};
use swash::{
    FontRef,
    zeno::{Angle, Format, Transform},
};
use unicode_script::{Script, UnicodeScript};

const DEFAULT_ATLAS_SIZE: u32 = 1024;
const MAX_FALLBACKS: usize = 3;

/// Well-known fallback faces tried in order when the primary face lacks a
/// glyph: CJK coverage, color emoji, and extra symbols. Each entry is a file
/// path plus the face index inside that file (collections hold several).
const FALLBACK_CANDIDATES: &[(&str, usize)] = &[
    ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 0),
    ("/usr/share/fonts/noto/NotoColorEmoji.ttf", 0),
    ("/usr/share/fonts/noto/NotoSansSymbols2-Regular.ttf", 0),
];

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
    /// True when the atlas pixels are RGBA8 (color emoji) rather than a
    /// monochrome mask; the renderer samples a separate color atlas.
    pub color: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlyphUpload {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    /// True when `pixels` holds `width * height` RGBA8 bytes for the color
    /// atlas instead of `width * height` mask bytes.
    pub color: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct GlyphKey {
    /// 0 selects the primary face, 1..=MAX_FALLBACKS a fallback face.
    face: u8,
    identity: GlyphIdentity,
    bold: bool,
    italic: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum GlyphIdentity {
    Glyph(u16),
    Text {
        grapheme: String,
        pre_context: String,
        post_context: String,
    },
}

struct PositionedMask {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

struct FaceData {
    bytes: Vec<u8>,
    index: usize,
}

/// Owns one primary font face plus fallback faces and rasterizes each glyph
/// key at most once. Cell metrics always come from the primary face; terminal
/// semantics never depend on which face drew a glyph.
pub struct Font {
    bytes: Vec<u8>,
    face_index: usize,
    fallbacks: Vec<FaceData>,
    size: f32,
    metrics: CellMetrics,
    atlas_size: u32,
    atlas: Vec<u8>,
    color_atlas: Vec<u8>,
    shelf_x: u32,
    shelf_y: u32,
    shelf_height: u32,
    color_shelf_x: u32,
    color_shelf_y: u32,
    color_shelf_height: u32,
    glyphs: HashMap<GlyphKey, (u32, GlyphInfo)>,
    context: ScaleContext,
}

impl Font {
    pub fn requires_contextual_shaping(ch: char) -> bool {
        matches!(
            ch.script(),
            Script::Arabic
                | Script::Syriac
                | Script::Thaana
                | Script::Nko
                | Script::Mongolian
                | Script::Bengali
                | Script::Devanagari
                | Script::Gujarati
                | Script::Gurmukhi
                | Script::Kannada
                | Script::Malayalam
                | Script::Oriya
                | Script::Sinhala
                | Script::Tamil
                | Script::Telugu
                | Script::Myanmar
                | Script::Khmer
                | Script::Lao
                | Script::Thai
                | Script::Hangul
        )
    }

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
            fallbacks: load_fallbacks(),
            size,
            metrics,
            atlas_size: DEFAULT_ATLAS_SIZE,
            atlas: vec![0; (DEFAULT_ATLAS_SIZE * DEFAULT_ATLAS_SIZE) as usize],
            color_atlas: vec![
                0;
                (DEFAULT_ATLAS_SIZE * DEFAULT_ATLAS_SIZE * 4) as usize
            ],
            shelf_x: 1,
            shelf_y: 1,
            shelf_height: 0,
            color_shelf_x: 1,
            color_shelf_y: 1,
            color_shelf_height: 0,
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
    pub fn color_atlas_pixels(&self) -> &[u8] {
        &self.color_atlas
    }
    /// Number of fallback faces loaded after the primary face.
    pub fn fallback_count(&self) -> usize {
        self.fallbacks.len()
    }

    /// Returns a stable atlas index and uploads only on the first lookup.
    /// Glyphs missing from the primary face fall back to the loaded fallback
    /// faces; glyphs missing everywhere resolve to an empty entry.
    pub fn glyph(
        &mut self,
        ch: char,
        bold: bool,
        italic: bool,
    ) -> Result<(u32, GlyphInfo, Option<GlyphUpload>), String> {
        let (face_id, glyph_id) = self.resolve_face(ch);
        let key = GlyphKey {
            face: face_id,
            identity: GlyphIdentity::Glyph(glyph_id),
            bold,
            italic,
        };
        if let Some((index, info)) = self.glyphs.get(&key).copied() {
            return Ok((index, info, None));
        }

        let face_bytes: &[u8] = if face_id == 0 {
            &self.bytes
        } else {
            &self.fallbacks[face_id as usize - 1].bytes
        };
        let loaded_index = if face_id == 0 {
            self.face_index
        } else {
            self.fallbacks[face_id as usize - 1].index
        };
        let face =
            FontRef::from_index(face_bytes, loaded_index).ok_or("invalid loaded font")?;
        let mut scaler = self
            .context
            .builder(face)
            .size(self.size)
            .hint(true)
            .build();
        let sources = [
            Source::Outline,
            Source::Bitmap(swash::scale::StrikeWith::BestFit),
            Source::ColorBitmap(swash::scale::StrikeWith::BestFit),
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
                color: false,
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
                color: false,
            };
            self.glyphs.insert(key, (index, info));
            return Ok((index, info, None));
        }
        let color = image.content == Content::Color;
        let bytes_per_pixel = if color { 4 } else { 1 };
        let raster_bytes = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(bytes_per_pixel))
            .ok_or("rasterized glyph dimensions overflow")?;
        if (!color && image.content != Content::Mask) || image.data.len() != raster_bytes
        {
            return Err("font rasterizer returned an unsupported glyph format".into());
        }
        let padded_width = width.checked_add(2).ok_or("glyph width overflow")?;
        let padded_height = height.checked_add(2).ok_or("glyph height overflow")?;
        // A full atlas degrades the glyph to an empty entry instead of
        // failing the frame: atlas capacity stays bounded while the session
        // keeps running.
        let Some((x, y)) = self.allocate_in(padded_width, padded_height, color) else {
            let info = GlyphInfo {
                uv_min: [0.0; 2],
                uv_max: [0.0; 2],
                size: [0.0; 2],
                bearing: [0.0; 2],
                advance,
                color,
            };
            self.glyphs.insert(key, (index, info));
            return Ok((index, info, None));
        };
        let x = x + 1;
        let y = y + 1;
        let row_bytes = width as usize * bytes_per_pixel;
        let atlas = if color {
            &mut self.color_atlas
        } else {
            &mut self.atlas
        };
        for row in 0..height {
            let source = (row as usize) * row_bytes;
            let target = ((y + row) * self.atlas_size + x) as usize * bytes_per_pixel;
            atlas[target..target + row_bytes]
                .copy_from_slice(&image.data[source..source + row_bytes]);
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
            color,
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
                color,
            }),
        ))
    }

    /// Selects the first face whose charmap contains `ch`: the primary face,
    /// then fallbacks in load order. Returns face 0 with id 0 when no face
    /// provides the character.
    fn resolve_face(&self, ch: char) -> (u8, u16) {
        if let Some(face) = FontRef::from_index(&self.bytes, self.face_index)
            && face.charmap().map(ch) != 0
        {
            return (0, face.charmap().map(ch));
        }
        for (position, fallback) in self.fallbacks.iter().enumerate() {
            if position >= MAX_FALLBACKS {
                break;
            }
            if let Some(face) = FontRef::from_index(&fallback.bytes, fallback.index) {
                let glyph_id = face.charmap().map(ch);
                if glyph_id != 0 {
                    return (position as u8 + 1, glyph_id);
                }
            }
        }
        (0, 0)
    }

    /// Shapes and rasterizes one bounded grapheme as a single atlas entry.
    pub fn glyph_text(
        &mut self,
        text: &str,
        bold: bool,
        italic: bool,
    ) -> Result<(u32, GlyphInfo, Option<GlyphUpload>), String> {
        self.glyph_text_with_context(text, "", "", bold, italic)
    }

    /// Shapes one grapheme with adjacent run context for joining scripts.
    pub fn glyph_text_with_context(
        &mut self,
        text: &str,
        pre_context: &str,
        post_context: &str,
        bold: bool,
        italic: bool,
    ) -> Result<(u32, GlyphInfo, Option<GlyphUpload>), String> {
        if text.len() > 256 || text.is_empty() {
            return Err("grapheme raster input must contain 1 to 256 bytes".into());
        }
        if pre_context.len() > 128 || post_context.len() > 128 {
            return Err("shaping context exceeds the 128-byte limit".into());
        }
        if text.chars().count() == 1 && pre_context.is_empty() && post_context.is_empty() {
            return self.glyph(text.chars().next().expect("nonempty text"), bold, italic);
        }
        let key = GlyphKey {
            face: 0,
            identity: GlyphIdentity::Text {
                grapheme: text.to_owned(),
                pre_context: pre_context.to_owned(),
                post_context: post_context.to_owned(),
            },
            bold,
            italic,
        };
        if let Some((index, info)) = self.glyphs.get(&key).copied() {
            return Ok((index, info, None));
        }

        let face = rustybuzz::Face::from_slice(&self.bytes, self.face_index as u32)
            .ok_or("invalid loaded font for shaping")?;
        let scale = self.size / face.units_per_em() as f32;
        let mut buffer = rustybuzz::UnicodeBuffer::new();
        buffer.push_str(text);
        buffer.set_pre_context(pre_context);
        buffer.set_post_context(post_context);
        buffer.guess_segment_properties();
        let shaped = rustybuzz::shape(&face, &[], buffer);
        let infos = shaped.glyph_infos();
        let positions = shaped.glyph_positions();
        let font_face =
            FontRef::from_index(&self.bytes, self.face_index).ok_or("invalid loaded font")?;
        let mut scaler = self
            .context
            .builder(font_face)
            .size(self.size)
            .hint(true)
            .build();
        let sources = [
            Source::Outline,
            Source::Bitmap(swash::scale::StrikeWith::BestFit),
            Source::ColorBitmap(swash::scale::StrikeWith::BestFit),
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
        let mut masks = Vec::with_capacity(infos.len());
        let mut pen_x = 0.0f32;
        let mut pen_y = 0.0f32;
        let mut advance = 0.0f32;
        let mut bounds = None::<(i32, i32, i32, i32)>;
        for (info, position) in infos.iter().zip(positions) {
            let glyph_id = info.glyph_id as u16;
            if renderer.render_into(&mut scaler, glyph_id, &mut image)
                && image.content == Content::Mask
            {
                let placement = image.placement;
                let width = placement.width;
                let height = placement.height;
                let expected = (width as usize)
                    .checked_mul(height as usize)
                    .ok_or("shaped glyph dimensions overflow")?;
                if image.data.len() != expected {
                    return Err("font rasterizer returned an unsupported glyph mask".into());
                }
                if width > 0 && height > 0 {
                    let x =
                        (pen_x + position.x_offset as f32 * scale).round() as i32 + placement.left;
                    let y =
                        -(pen_y + position.y_offset as f32 * scale).round() as i32 - placement.top;
                    let right = x
                        .checked_add(width as i32)
                        .ok_or("shaped glyph bounds overflow")?;
                    let bottom = y
                        .checked_add(height as i32)
                        .ok_or("shaped glyph bounds overflow")?;
                    bounds = Some(match bounds {
                        Some((left, top, old_right, old_bottom)) => (
                            left.min(x),
                            top.min(y),
                            old_right.max(right),
                            old_bottom.max(bottom),
                        ),
                        None => (x, y, right, bottom),
                    });
                    masks.push(PositionedMask {
                        x,
                        y,
                        width,
                        height,
                        pixels: image.data.clone(),
                    });
                }
            }
            pen_x += position.x_advance as f32 * scale;
            pen_y += position.y_advance as f32 * scale;
            advance += position.x_advance as f32 * scale;
        }
        let index = self.glyphs.len() as u32;
        let Some((left, top, right, bottom)) = bounds else {
            let info = GlyphInfo {
                uv_min: [0.0; 2],
                uv_max: [0.0; 2],
                size: [0.0; 2],
                bearing: [0.0; 2],
                advance,
                color: false,
            };
            self.glyphs.insert(key, (index, info));
            return Ok((index, info, None));
        };
        let width = u32::try_from(right - left).map_err(|_| "shaped glyph width overflow")?;
        let height = u32::try_from(bottom - top).map_err(|_| "shaped glyph height overflow")?;
        if width > 512 || height > 512 {
            return Err("shaped grapheme exceeds the 512-pixel raster limit".into());
        }
        let mut pixels = vec![0u8; (width * height) as usize];
        for mask in masks {
            for y in 0..mask.height {
                for x in 0..mask.width {
                    let source = (y * mask.width + x) as usize;
                    let target_x = (mask.x - left) as u32 + x;
                    let target_y = (mask.y - top) as u32 + y;
                    let target = (target_y * width + target_x) as usize;
                    pixels[target] = pixels[target].max(mask.pixels[source]);
                }
            }
        }
        let padded_width = width.checked_add(2).ok_or("glyph width overflow")?;
        let padded_height = height.checked_add(2).ok_or("glyph height overflow")?;
        // A full atlas degrades the cluster to an empty entry instead of
        // failing the frame; see `glyph` for the capacity policy.
        let Some((atlas_x, atlas_y)) = self.allocate_in(padded_width, padded_height, false)
        else {
            let info = GlyphInfo {
                uv_min: [0.0; 2],
                uv_max: [0.0; 2],
                size: [0.0; 2],
                bearing: [0.0; 2],
                advance,
                color: false,
            };
            self.glyphs.insert(key, (index, info));
            return Ok((index, info, None));
        };
        let x = atlas_x + 1;
        let y = atlas_y + 1;
        for row in 0..height {
            let source = (row * width) as usize;
            let target = ((y + row) * self.atlas_size + x) as usize;
            self.atlas[target..target + width as usize]
                .copy_from_slice(&pixels[source..source + width as usize]);
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
            bearing: [left as f32, -top as f32],
            advance,
            color: false,
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
                pixels,
                color: false,
            }),
        ))
    }

    fn allocate_in(&mut self, width: u32, height: u32, color: bool) -> Option<(u32, u32)> {        if width > self.atlas_size || height > self.atlas_size {
            return None;
        }
        let (shelf_x, shelf_y, shelf_height) = if color {
            (
                &mut self.color_shelf_x,
                &mut self.color_shelf_y,
                &mut self.color_shelf_height,
            )
        } else {
            (
                &mut self.shelf_x,
                &mut self.shelf_y,
                &mut self.shelf_height,
            )
        };
        if *shelf_x + width > self.atlas_size {
            *shelf_x = 1;
            *shelf_y = shelf_y.checked_add(*shelf_height)?;
            *shelf_height = 0;
        }
        if *shelf_y + height > self.atlas_size {
            return None;
        }
        let result = (*shelf_x, *shelf_y);
        *shelf_x += width;
        *shelf_height = (*shelf_height).max(height);
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

/// Loads existing fallback faces from well-known system paths, in priority
/// order and bounded by `MAX_FALLBACKS`.
fn load_fallbacks() -> Vec<FaceData> {
    let mut faces = Vec::new();
    for (path, index) in FALLBACK_CANDIDATES {
        if faces.len() >= MAX_FALLBACKS {
            break;
        }
        let candidate = PathBuf::from(path);
        let Ok(bytes) = fs::read(&candidate) else {
            continue;
        };
        if FontRef::from_index(&bytes, *index).is_some() {
            faces.push(FaceData {
                bytes,
                index: *index,
            });
        }
    }
    faces
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

    #[test]
    fn shaped_grapheme_uses_one_composited_cached_atlas_entry() {
        let mut font =
            Font::load(None, 14.0).expect("Linux test environment provides a monospace font");
        let (first_id, first, upload) = font.glyph_text("e\u{301}", false, false).unwrap();
        assert!(first.size[0] > 0.0 && first.size[1] > 0.0);
        let upload = upload.expect("first cluster lookup produces one atlas upload");
        assert_eq!(upload.pixels.len(), (upload.width * upload.height) as usize);
        let (cached_id, cached, upload) = font.glyph_text("e\u{301}", false, false).unwrap();
        assert_eq!(cached_id, first_id);
        assert_eq!(cached, first);
        assert!(upload.is_none());
    }

    #[test]
    fn contextual_script_shaping_is_enabled_without_shaping_latin_grid_runs() {
        assert!(Font::requires_contextual_shaping('م'));
        assert!(Font::requires_contextual_shaping('न'));
        assert!(!Font::requires_contextual_shaping('A'));
        assert!(!Font::requires_contextual_shaping('漢'));
    }

    #[test]
    fn fallback_faces_cover_cjk_and_symbols_without_touching_primary_metrics() {
        let mut font =
            Font::load(None, 14.0).expect("Linux test environment provides a monospace font");
        if font.fallback_count() == 0 {
            eprintln!("no fallback faces installed; skipping CJK coverage check");
            return;
        }
        let metrics = font.metrics();
        let (id, info, upload) = font.glyph('界', false, false).unwrap();
        assert!(info.size[0] > 0.0 && info.size[1] > 0.0);
        assert!(!info.color);
        let upload = upload.expect("first CJK lookup rasterizes and uploads");
        assert!(!upload.color);
        assert_eq!(upload.pixels.len(), (upload.width * upload.height) as usize);
        // A cache hit reuses the stable index without a second upload.
        let (cached_id, cached, upload) = font.glyph('界', false, false).unwrap();
        assert_eq!(cached_id, id);
        assert_eq!(cached, info);
        assert!(upload.is_none());
        // Primary metrics never depend on fallback faces.
        assert_eq!(font.metrics(), metrics);
    }

    #[test]
    fn color_emoji_resolves_to_rgba_uploads_when_present() {
        let mut font =
            Font::load(None, 14.0).expect("Linux test environment provides a monospace font");
        if font.fallback_count() == 0 {
            eprintln!("no fallback faces installed; skipping emoji coverage check");
            return;
        }
        let (_, info, upload) = font.glyph('😀', false, false).unwrap();
        let Some(upload) = upload else {
            eprintln!("emoji face has no raster for U+1F600 on this system");
            return;
        };
        assert!(info.color, "emoji raster must be flagged for the color atlas");
        assert!(upload.color);
        assert_eq!(
            upload.pixels.len(),
            (upload.width * upload.height * 4) as usize
        );
    }
}
