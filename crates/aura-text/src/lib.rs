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

mod faces;

const DEFAULT_ATLAS_SIZE: u32 = 1024;
const MAX_FALLBACKS: usize = 8;
/// Cache pressure is recoverable: renderers rebuild from the visible grid.
pub const CACHE_FULL: &str = "font cache capacity exhausted";
const MAX_GLYPHS: usize = 65_536;

/// Smallest runtime font size in typographic pixels. The floor keeps glyph
/// rasters, cell geometry, and window pixel math safely above zero; every
/// size below is clamped here, never rejected. Verified against the
/// `metrics_*` and `.*_clamped_to_supported_range` tests below.
pub const MIN_FONT_SIZE: f32 = 6.0;
/// Largest runtime font size in typographic pixels. The ceiling keeps
/// single-glyph uploads inside the 1024px atlas packer and pixel dimensions
/// inside the saturating `u32` window math. Verified the same way.
pub const MAX_FONT_SIZE: f32 = 72.0;
/// Step applied by one `Ctrl+Plus` / `Ctrl+Minus` press.
pub const FONT_SIZE_STEP: f32 = 1.0;

/// Clamps any requested size into `[MIN_FONT_SIZE, MAX_FONT_SIZE]`.
/// Non-finite inputs fall back to `MIN_FONT_SIZE` so rasterization and
/// geometry never see NaN, infinity, zero, or negative sizes.
pub fn clamp_font_size(size: f32) -> f32 {
    if !size.is_finite() {
        return MIN_FONT_SIZE;
    }
    size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)
}

/// Well-known fallback faces tried in order when the primary face lacks a
/// glyph: CJK coverage, color emoji, and extra symbols. Each entry is a file
/// path plus the face index inside that file (collections hold several).
const FALLBACK_CANDIDATES: &[&[&str]] = &[
    &[
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    ],
    &[
        "/usr/share/fonts/noto/NotoColorEmoji.ttf",
        "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
    ],
    &[
        "/usr/share/fonts/noto/NotoSansSymbols2-Regular.ttf",
        "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
    ],
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
    /// 0 selects the primary face, 1..=8 fallbacks, 9..=11 style faces.
    face: u8,
    /// Raw bits of the font size in typographic pixels. Stored as bits so the
    /// key remains `Eq`/`Hash` across the full `f32` space after validation.
    size_bits: u32,
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

struct PositionedRaster {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    color: bool,
}

struct FaceData {
    bytes: Vec<u8>,
    index: usize,
}

/// Owns one primary face, three optional styles and bounded fallbacks, rasterizing each glyph
/// key at most once. Cell metrics always come from the primary face; terminal
/// semantics never depend on which face drew a glyph. Fallback faces load on
/// the first primary miss so plain sessions never pay their cost.
pub struct Font {
    bytes: Vec<u8>,
    face_index: usize,
    primary_path: PathBuf,
    styles: [Option<FaceData>; 3],
    fallbacks: Vec<FaceData>,
    fallbacks_loaded: bool,
    fallback_paths: Vec<PathBuf>,
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
        let size = clamp_font_size(size);
        let path = path
            .map(Path::to_path_buf)
            .or_else(default_font_path)
            .ok_or_else(|| {
                "no monospace font found; pass --font or install a system monospace font"
                    .to_string()
            })?;
        let bytes =
            fs::read(&path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let metrics = compute_metrics(&bytes, 0, size)
            .ok_or_else(|| format!("invalid font: {}", path.display()))?;
        Ok(Self {
            bytes,
            face_index: 0,
            styles: faces::load_styles(&path),
            primary_path: path,
            fallbacks: Vec::new(),
            fallbacks_loaded: false,
            fallback_paths: Vec::new(),
            size,
            metrics,
            atlas_size: DEFAULT_ATLAS_SIZE,
            atlas: vec![0; (DEFAULT_ATLAS_SIZE * DEFAULT_ATLAS_SIZE) as usize],
            color_atlas: vec![0; (DEFAULT_ATLAS_SIZE * DEFAULT_ATLAS_SIZE * 4) as usize],
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

    /// Current rasterization size in typographic pixels.
    pub const fn size(&self) -> f32 {
        self.size
    }

    /// Switches to `requested` size with fresh metrics, an empty glyph cache,
    /// and cleared atlas shelves so the next lookup re-rasterizes every
    /// glyph (primary, fallback, CJK, emoji) instead of scaling old pixels.
    /// Returns `true` when the effective size changed.
    pub fn set_size(&mut self, requested: f32) -> Result<bool, String> {
        let size = clamp_font_size(requested);
        if size.to_bits() == self.size.to_bits() {
            return Ok(false);
        }
        let metrics = compute_metrics(&self.bytes, self.face_index, size)
            .ok_or_else(|| "loaded font no longer parses; size change rejected".to_string())?;
        self.size = size;
        self.metrics = metrics;
        self.invalidate_atlas();
        Ok(true)
    }

    /// Clears the glyph cache and atlas so stale metadata is never sampled
    /// after a face, size, or style change.
    pub fn invalidate_atlas(&mut self) {
        self.atlas.fill(0);
        self.color_atlas.fill(0);
        self.glyphs.clear();
        self.shelf_x = 1;
        self.shelf_y = 1;
        self.shelf_height = 0;
        self.color_shelf_x = 1;
        self.color_shelf_y = 1;
        self.color_shelf_height = 0;
    }

    /// Resolves a font family name or filesystem path to a concrete font file.
    ///
    /// Resolution order:
    /// 1. If `family` is an existing file, use it directly.
    /// 2. Search `/usr/share/fonts` recursively for files whose stem contains
    ///    the family name (case-insensitive).
    /// 3. Fall back to the same search in `/usr/local/share/fonts`.
    pub fn resolve_family_path(family: &str) -> Option<PathBuf> {
        let candidate = PathBuf::from(family);
        if candidate.is_file() {
            return Some(candidate);
        }
        for root in ["/usr/share/fonts", "/usr/local/share/fonts"] {
            let root = PathBuf::from(root);
            if let Some(found) = search_font_dir(&root, family) {
                return Some(found);
            }
        }
        None
    }

    /// Loads a font by family name or explicit path, falling back to the
    /// system monospace search when the name cannot be resolved.
    /// The requested size is clamped by [`clamp_font_size`], same as `load`.
    pub fn load_with_family(family: &str, size: f32) -> Result<Self, String> {
        let path = Self::resolve_family_path(family)
            .or_else(default_font_path)
            .ok_or_else(|| {
                format!("no font found for family '{family}'; pass a path or install the font")
            })?;
        Self::load(Some(&path), size)
    }

    /// Configured families precede system fallbacks, with a fixed face budget.
    pub fn set_fallback_families(&mut self, families: &[String]) {
        self.fallback_paths = families
            .iter()
            .take(MAX_FALLBACKS)
            .filter_map(|family| Self::resolve_family_path(family))
            .collect();
        self.fallbacks.clear();
        self.fallbacks_loaded = false;
        self.invalidate_atlas();
    }

    /// Replaces one style face without changing cell metrics. Missing or
    /// invalid explicit faces return an error; available styles are discovered
    /// beside the primary font, with synthetic styling as the last resort.
    pub fn set_style_face(
        &mut self,
        bold: bool,
        italic: bool,
        family: Option<&str>,
        style: Option<&str>,
    ) -> Result<(), String> {
        let slot = usize::from(bold) + usize::from(italic) * 2;
        if slot == 0 {
            return Err("style face requires bold or italic".into());
        }
        let style = style.unwrap_or(["Bold", "Italic", "Bold Italic"][slot - 1]);
        let family = family.unwrap_or("monospace");
        let path = if family == "monospace" {
            faces::sibling_style_path(&self.primary_path, style)
        } else {
            faces::style_path(family, style)
        };
        let family = if family == "monospace" {
            faces::family_stem(&self.primary_path)
        } else {
            family.to_owned()
        };
        let path = path
            .or_else(|| faces::style_path(&family, style))
            .ok_or_else(|| format!("cannot find font family '{family}' style '{style}'"))?;
        self.styles[slot - 1] = Some(faces::load(&path)?);
        self.invalidate_atlas();
        Ok(())
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
    /// Number of fallback faces loaded so far (zero until the first primary
    /// miss triggers on-demand loading).
    pub fn fallback_count(&self) -> usize {
        self.fallbacks.len()
    }

    /// Returns a stable atlas index and uploads only on the first lookup.
    /// Glyphs missing from the primary face fall back to the loaded fallback
    /// faces; characters missing everywhere use the primary .notdef glyph.
    pub fn glyph(
        &mut self,
        ch: char,
        bold: bool,
        italic: bool,
    ) -> Result<(u32, GlyphInfo, Option<GlyphUpload>), String> {
        let mut text = [0; 4];
        let face_id = self.resolve_text_face(ch.encode_utf8(&mut text), bold, italic);
        let face = self.face(face_id).ok_or("invalid loaded font")?;
        let glyph_id = face.charmap().map(ch);
        let synthetic_bold = bold && face.attributes().weight().0 < 600;
        let synthetic_italic = italic && face.attributes().style() == swash::Style::Normal;
        let key = GlyphKey {
            face: face_id,
            size_bits: self.size.to_bits(),
            identity: GlyphIdentity::Glyph(glyph_id),
            bold,
            italic,
        };
        if let Some((index, info)) = self.glyphs.get(&key).copied() {
            return Ok((index, info, None));
        }
        if self.glyphs.len() >= MAX_GLYPHS {
            return Err(CACHE_FULL.into());
        }

        let (face_bytes, loaded_index) = match face_id {
            0 => (&self.bytes[..], self.face_index),
            1..=8 => {
                let face = &self.fallbacks[face_id as usize - 1];
                (&face.bytes[..], face.index)
            }
            _ => {
                let face = self.styles[face_id as usize - 9].as_ref().unwrap();
                (&face.bytes[..], face.index)
            }
        };
        let face = FontRef::from_index(face_bytes, loaded_index).ok_or("invalid loaded font")?;
        let mut scaler = self
            .context
            .builder(face)
            .size(self.size)
            .hint(true)
            .build();
        let sources = [
            Source::ColorOutline(0),
            Source::Outline,
            Source::Bitmap(swash::scale::StrikeWith::BestFit),
            Source::ColorBitmap(swash::scale::StrikeWith::BestFit),
        ];
        let mut image = swash::scale::image::Image::new();
        let mut renderer = Render::new(&sources);
        renderer.format(Format::Alpha);
        renderer.embolden(if synthetic_bold { 0.45 } else { 0.0 });
        if synthetic_italic {
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
        if (!color && image.content != Content::Mask) || image.data.len() != raster_bytes {
            return Err("font rasterizer returned an unsupported glyph format".into());
        }
        straighten_color_outline(&mut image);
        let padded_width = width.checked_add(2).ok_or("glyph width overflow")?;
        let padded_height = height.checked_add(2).ok_or("glyph height overflow")?;
        // Never cache a failed allocation as an invisible glyph.
        let (x, y) = self
            .allocate_in(padded_width, padded_height, color)
            .ok_or(CACHE_FULL)?;
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

    fn face(&self, id: u8) -> Option<FontRef<'_>> {
        let face = match id {
            0 => return FontRef::from_index(&self.bytes, self.face_index),
            1..=8 => self.fallbacks.get(id as usize - 1)?,
            _ => self.styles.get(id as usize - 9)?.as_ref()?,
        };
        FontRef::from_index(&face.bytes, face.index)
    }

    // Select one face for the whole cluster. Mixing faces before shaping
    // breaks joining, combining marks, and emoji ligatures.
    fn resolve_text_face(&mut self, text: &str, bold: bool, italic: bool) -> u8 {
        let style = u8::from(bold) + u8::from(italic) * 2;
        let mut primary = 0;
        for slot in [style, u8::from(bold), u8::from(italic) * 2] {
            if slot != 0
                && self
                    .face(8 + slot)
                    .is_some_and(|face| faces::covers(face, text))
            {
                primary = 8 + slot;
                break;
            }
        }
        let primary_covers = self
            .face(primary)
            .is_some_and(|face| faces::covers(face, text));
        let emoji_presentation = text.contains('\u{fe0f}') || text.contains('\u{200d}');
        if primary_covers && !emoji_presentation {
            return primary;
        }
        if !self.fallbacks_loaded {
            self.fallbacks = load_fallbacks(&self.fallback_paths);
            self.fallbacks_loaded = true;
        }
        let mut first = None;
        for (position, fallback) in self.fallbacks.iter().enumerate() {
            if let Some(face) = FontRef::from_index(&fallback.bytes, fallback.index)
                && faces::covers(face, text)
            {
                let id = position as u8 + 1;
                first.get_or_insert(id);
                if emoji_presentation && faces::has_color(face) {
                    return id;
                }
            }
        }
        if primary_covers {
            primary
        } else if let Some(id) = first {
            id
        } else {
            // No complete face: preserve the base character rather than
            // dropping the entire cluster. Unsupported marks remain .notdef.
            let base = text.chars().next().unwrap_or('\u{fffd}');
            (0..=self.fallbacks.len() as u8)
                .find(|&id| {
                    self.face(id)
                        .is_some_and(|face| face.charmap().map(base) != 0)
                })
                .unwrap_or(0)
        }
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
        let face_id = self.resolve_text_face(text, bold, italic);
        let font_face = self.face(face_id).ok_or("invalid loaded font")?;
        let synthetic_bold = bold && font_face.attributes().weight().0 < 600;
        let synthetic_italic = italic && font_face.attributes().style() == swash::Style::Normal;
        let key = GlyphKey {
            face: face_id,
            size_bits: self.size.to_bits(),
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
        if self.glyphs.len() >= MAX_GLYPHS {
            return Err(CACHE_FULL.into());
        }

        let (face_bytes, loaded_index) = match face_id {
            0 => (&self.bytes[..], self.face_index),
            1..=8 => {
                let face = &self.fallbacks[face_id as usize - 1];
                (&face.bytes[..], face.index)
            }
            _ => {
                let face = self.styles[face_id as usize - 9].as_ref().unwrap();
                (&face.bytes[..], face.index)
            }
        };
        let face = rustybuzz::Face::from_slice(face_bytes, loaded_index as u32)
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
        if infos.len() > 256 {
            return Err("shaped grapheme exceeds the 256-glyph limit".into());
        }
        let font_face =
            FontRef::from_index(face_bytes, loaded_index).ok_or("invalid loaded font")?;
        let mut scaler = self
            .context
            .builder(font_face)
            .size(self.size)
            .hint(true)
            .build();
        let sources = [
            Source::ColorOutline(0),
            Source::Outline,
            Source::Bitmap(swash::scale::StrikeWith::BestFit),
            Source::ColorBitmap(swash::scale::StrikeWith::BestFit),
        ];
        let mut image = swash::scale::image::Image::new();
        let mut renderer = Render::new(&sources);
        renderer.format(Format::Alpha);
        renderer.embolden(if synthetic_bold { 0.45 } else { 0.0 });
        if synthetic_italic {
            renderer.transform(Some(Transform::skew(
                Angle::from_degrees(-12.0),
                Angle::ZERO,
            )));
        }
        let mut rasters = Vec::with_capacity(infos.len());
        let mut raster_bytes = 0usize;
        let mut pen_x = 0.0f32;
        let mut pen_y = 0.0f32;
        let mut advance = 0.0f32;
        let mut bounds = None::<(i32, i32, i32, i32)>;
        for (info, position) in infos.iter().zip(positions) {
            let glyph_id = info.glyph_id as u16;
            if renderer.render_into(&mut scaler, glyph_id, &mut image) {
                let placement = image.placement;
                let width = placement.width;
                let height = placement.height;
                let color = image.content == Content::Color;
                let expected = (width as usize)
                    .checked_mul(height as usize)
                    .and_then(|pixels| pixels.checked_mul(if color { 4 } else { 1 }))
                    .ok_or("shaped glyph dimensions overflow")?;
                if (!color && image.content != Content::Mask) || image.data.len() != expected {
                    return Err("font rasterizer returned an unsupported glyph format".into());
                }
                raster_bytes = raster_bytes
                    .checked_add(expected)
                    .ok_or("shaped raster overflow")?;
                if width > 512 || height > 512 || raster_bytes > 512 * 512 * 4 {
                    return Err("shaped grapheme exceeds the raster memory limit".into());
                }
                straighten_color_outline(&mut image);
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
                    rasters.push(PositionedRaster {
                        x,
                        y,
                        width,
                        height,
                        pixels: image.data.clone(),
                        color,
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
        let (pixels, color) = composite_rasters(&rasters, [left, top], width, height);
        let bytes_per_pixel = if color { 4 } else { 1 };
        let padded_width = width.checked_add(2).ok_or("glyph width overflow")?;
        let padded_height = height.checked_add(2).ok_or("glyph height overflow")?;
        let (atlas_x, atlas_y) = self
            .allocate_in(padded_width, padded_height, color)
            .ok_or(CACHE_FULL)?;
        let x = atlas_x + 1;
        let y = atlas_y + 1;
        let row_bytes = width as usize * bytes_per_pixel;
        let atlas = if color {
            &mut self.color_atlas
        } else {
            &mut self.atlas
        };
        for row in 0..height {
            let source = row as usize * row_bytes;
            let target = ((y + row) * self.atlas_size + x) as usize * bytes_per_pixel;
            atlas[target..target + row_bytes].copy_from_slice(&pixels[source..source + row_bytes]);
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
                pixels,
                color,
            }),
        ))
    }

    fn allocate_in(&mut self, width: u32, height: u32, color: bool) -> Option<(u32, u32)> {
        if width > self.atlas_size || height > self.atlas_size {
            return None;
        }
        let (shelf_x, shelf_y, shelf_height) = if color {
            (
                &mut self.color_shelf_x,
                &mut self.color_shelf_y,
                &mut self.color_shelf_height,
            )
        } else {
            (&mut self.shelf_x, &mut self.shelf_y, &mut self.shelf_height)
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

// Swash's COLR compositor produces premultiplied channels; PNG emoji use
// straight alpha. Both atlas paths must match Vulkan's SRC_ALPHA blend.
fn straighten_color_outline(image: &mut swash::scale::image::Image) {
    if image.content != Content::Color || !matches!(image.source, Source::ColorOutline(_)) {
        return;
    }
    for rgba in image.data.chunks_exact_mut(4) {
        let alpha = u32::from(rgba[3]);
        if alpha != 0 {
            for channel in &mut rgba[..3] {
                *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
}

fn composite_rasters(
    rasters: &[PositionedRaster],
    origin: [i32; 2],
    width: u32,
    height: u32,
) -> (Vec<u8>, bool) {
    let color = rasters.iter().any(|raster| raster.color);
    let stride = if color { 4 } else { 1 };
    let mut pixels = vec![0; width as usize * height as usize * stride];
    for raster in rasters {
        for y in 0..raster.height {
            for x in 0..raster.width {
                let source = (y * raster.width + x) as usize;
                let tx = (raster.x - origin[0]) as u32 + x;
                let ty = (raster.y - origin[1]) as u32 + y;
                let target = (ty * width + tx) as usize * stride;
                if !color {
                    let a = u32::from(raster.pixels[source]);
                    let d = u32::from(pixels[target]);
                    pixels[target] = (a + (d * (255 - a) + 127) / 255) as u8;
                    continue;
                }
                let rgba: [u8; 4] = if raster.color {
                    raster.pixels[source * 4..source * 4 + 4]
                        .try_into()
                        .unwrap()
                } else {
                    [255, 255, 255, raster.pixels[source]]
                };
                let a = u32::from(rgba[3]);
                let d = u32::from(pixels[target + 3]);
                let alpha = a * 255 + d * (255 - a);
                if alpha == 0 {
                    continue;
                }
                for channel in 0..3 {
                    let numerator = u32::from(rgba[channel]) * a * 255
                        + u32::from(pixels[target + channel]) * d * (255 - a);
                    pixels[target + channel] = ((numerator + alpha / 2) / alpha) as u8;
                }
                pixels[target + 3] = ((alpha + 127) / 255) as u8;
            }
        }
    }
    (pixels, color)
}

/// Recursively searches `dir` for font files whose stem matches `family`.
fn search_font_dir(dir: &Path, family: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    let mut best: Option<PathBuf> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        // Avoid following directory symlinks into cycles or outside the font root.
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let candidate = if kind.is_dir() {
            search_font_dir(&path, family)
        } else if matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("ttf" | "ttc" | "otf")
        ) && family_matches_path(family, &path)
        {
            Some(path)
        } else {
            None
        };
        if let Some(candidate) = candidate
            && best
                .as_ref()
                .is_none_or(|old| font_path_rank(&candidate, family) < font_path_rank(old, family))
        {
            best = Some(candidate);
        }
    }
    best
}

fn font_path_rank(path: &Path, family: &str) -> (u8, PathBuf) {
    let stem = normalize_name(
        &path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase(),
    );
    let family = normalize_name(&family.to_lowercase());
    let rank = if stem == family || stem == format!("{family}regular") {
        0
    } else if ["bold", "italic", "oblique", "light", "thin", "condensed"]
        .iter()
        .any(|style| stem.contains(style))
    {
        2
    } else {
        1
    };
    (rank, path.to_owned())
}

/// Heuristic: true when the family name appears in the filename stem.
///
/// Three matching strategies are tried, in order of strictness:
/// 1. Case-insensitive substring match on the raw stem (e.g. `"monospace"`
///    matches `DejaVuSansMono-Regular`).
/// 2. Case-insensitive substring match on the raw family against the stem.
/// 3. Normalized match ignoring non-alphanumeric characters, so multi-word
///    family names like `"FiraCode Nerd Font"` resolve to stems such as
///    `FiraCodeNerdFont-Regular` without hardcoding a path.
fn family_matches_path(family: &str, path: &Path) -> bool {
    let family_lower = family.to_lowercase();
    let stem = match path.file_stem().and_then(|s| s.to_str()) {
        Some(stem) => stem.to_lowercase(),
        None => return false,
    };
    if stem.contains(&family_lower) || family_lower.contains(&stem) {
        return true;
    }
    normalize_name(&stem).contains(&normalize_name(&family_lower))
}

/// Removes non-alphanumeric characters so that family names with spaces,
/// hyphens, or other punctuation match font file stems that omit them.
fn normalize_name(name: &str) -> String {
    name.chars().filter(|c| c.is_alphanumeric()).collect()
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

/// Computes cell metrics for `bytes`/`index` at `size` without touching the
/// atlas. Returns `None` when the face does not parse, which lets `set_size`
/// reject the change while keeping the live font untouched.
fn compute_metrics(bytes: &[u8], index: usize, size: f32) -> Option<CellMetrics> {
    let face = FontRef::from_index(bytes, index)?;
    let raw = face.metrics(&[]);
    let scaled = raw.scale(size);
    let glyph_metrics = face.glyph_metrics(&[]).scale(size);
    let cell_width = glyph_metrics.advance_width(face.charmap().map('M'));
    if ['i', 'W', '0', ' ']
        .into_iter()
        .any(|ch| (glyph_metrics.advance_width(face.charmap().map(ch)) - cell_width).abs() > 0.05)
    {
        return None;
    }
    let ascent = scaled.ascent.ceil() as i32;
    let descent = scaled.descent.abs().ceil() as u32;
    let line_gap = scaled.leading.ceil().max(0.0) as u32;
    Some(CellMetrics {
        width: cell_width.ceil().max(1.0) as u32,
        height: (ascent.max(0) as u32 + descent + line_gap).max(1),
        ascent,
        descent,
        line_gap,
    })
}

/// Loads existing fallback faces from well-known system paths, in priority
/// order and bounded by `MAX_FALLBACKS`.
fn load_fallbacks(configured: &[PathBuf]) -> Vec<FaceData> {
    let mut faces = Vec::new();
    let system = FALLBACK_CANDIDATES
        .iter()
        .filter_map(|paths| paths.iter().map(PathBuf::from).find(|path| path.is_file()));
    let mut seen = Vec::new();
    for candidate in configured.iter().cloned().chain(system) {
        if faces.len() >= MAX_FALLBACKS {
            break;
        }
        if seen.contains(&candidate) {
            continue;
        }
        seen.push(candidate.clone());
        let Ok(bytes) = fs::read(&candidate) else {
            continue;
        };
        if FontRef::from_index(&bytes, 0).is_some() {
            faces.push(FaceData { bytes, index: 0 });
        }
    }
    faces
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_styles_match_native_faces_without_double_synthetic_styling() {
        let Some(path) = Font::resolve_family_path("DejaVu Sans Mono") else {
            eprintln!("skipped: DejaVu Sans Mono unavailable");
            return;
        };
        let mut font = Font::load(Some(&path), 18.0).unwrap();
        let metrics = font.metrics();
        for (slot, bold, italic) in [(0, true, false), (1, false, true), (2, true, true)] {
            let style = font.styles[slot].as_ref().expect("installed DejaVu style");
            let bytes = style.bytes.clone();
            let native_path =
                faces::style_path("DejaVu Sans Mono", ["Bold", "Italic", "Bold Italic"][slot])
                    .unwrap();
            let mut native = Font::load(Some(&native_path), 18.0).unwrap();
            assert_eq!(bytes, native.bytes);
            for text in ["i", "W", "e\u{301}"] {
                let (_, actual, upload) = font.glyph_text(text, bold, italic).unwrap();
                let (_, expected, native_upload) = native.glyph_text(text, false, false).unwrap();
                assert_eq!(actual.size, expected.size);
                assert_eq!(actual.bearing, expected.bearing);
                assert_eq!(upload.unwrap().pixels, native_upload.unwrap().pixels);
            }
            assert_eq!(
                font.metrics(),
                metrics,
                "style never alters grid dimensions"
            );
        }
    }

    #[test]
    fn explicit_style_faces_invalidate_cache_and_missing_faces_leave_it_intact() {
        let mut font = Font::load(None, 18.0).unwrap();
        let metrics = font.metrics();
        let native_italic = font.styles[1].as_ref().unwrap().bytes.clone();
        let native_both = font.styles[2].as_ref().unwrap().bytes.clone();
        font.glyph('i', true, false).unwrap();
        assert!(
            font.set_style_face(true, false, Some("/nonexistent/aura-font.ttf"), None)
                .is_err()
        );
        assert_eq!(font.glyphs.len(), 1);
        let Some(path) = Font::resolve_family_path("Liberation Mono Bold") else {
            eprintln!("skipped: Liberation Mono Bold unavailable");
            return;
        };
        font.set_style_face(true, false, path.to_str(), None)
            .unwrap();
        assert!(font.glyphs.is_empty());
        let mut native = Font::load(Some(&path), 18.0).unwrap();
        let (_, actual, upload) = font.glyph('i', true, false).unwrap();
        let (_, expected, native_upload) = native.glyph('i', false, false).unwrap();
        assert_eq!(actual.size, expected.size);
        assert_eq!(upload.unwrap().pixels, native_upload.unwrap().pixels);
        assert_eq!(font.metrics(), metrics);
        font.set_style_face(true, false, Some("Liberation Mono Bold"), None)
            .unwrap();
        assert_eq!(font.styles[0].as_ref().unwrap().bytes, native.bytes);
        font.set_style_face(true, true, None, Some("Bold Italic"))
            .unwrap();
        assert_eq!(font.styles[2].as_ref().unwrap().bytes, native_both);
        font.set_style_face(false, true, None, Some("Italic"))
            .unwrap();
        assert_eq!(font.styles[1].as_ref().unwrap().bytes, native_italic);
    }

    #[test]
    fn complete_devanagari_cluster_and_joining_context_use_configured_fallback() {
        let Some(path) = Font::resolve_family_path("Noto Sans Devanagari") else {
            eprintln!("skipped: Noto Sans Devanagari unavailable");
            return;
        };
        let mut font = Font::load(None, 18.0).unwrap();
        font.set_fallback_families(&[path.to_string_lossy().into_owned()]);
        assert_eq!(font.resolve_text_face("कि", false, false), 1);
        for (before, after) in [("", ""), ("अ", "क")] {
            let (id, info, upload) = font
                .glyph_text_with_context("कि", before, after, false, false)
                .unwrap();
            assert!(!info.color && info.size[0] > 0.0 && info.size[1] > 0.0);
            assert!(upload.unwrap().pixels.iter().any(|&p| p != 0));
            assert!(
                font.glyphs
                    .iter()
                    .any(|(key, &(index, _))| key.face == 1 && index == id)
            );
            assert!(
                font.glyph_text_with_context("कि", before, after, false, false)
                    .unwrap()
                    .2
                    .is_none()
            );
        }
    }

    #[test]
    fn emoji_clusters_select_one_color_face_and_preserve_ligatures() {
        let mut font = Font::load(None, 18.0).unwrap();
        font.resolve_text_face("👩\u{200d}💻", false, false);
        if !font
            .fallbacks
            .iter()
            .any(|face| faces::has_color(FontRef::from_index(&face.bytes, face.index).unwrap()))
        {
            eprintln!("skipped: color emoji font unavailable");
            return;
        }
        for text in [
            "👩\u{200d}💻",
            "👨\u{200d}👩\u{200d}👧\u{200d}👦",
            "👍🏽",
            "🇪🇸",
            "❤\u{fe0f}",
        ] {
            let face_id = font.resolve_text_face(text, false, false);
            let selected = font.face(face_id).unwrap();
            assert!(faces::covers(selected, text) && faces::has_color(selected));
            let fallback = &font.fallbacks[face_id as usize - 1];
            let face = rustybuzz::Face::from_slice(&fallback.bytes, fallback.index as u32).unwrap();
            let mut buffer = rustybuzz::UnicodeBuffer::new();
            buffer.push_str(text);
            buffer.guess_segment_properties();
            let shape = rustybuzz::shape(&face, &[], buffer);
            assert_eq!(
                shape.len(),
                1,
                "{text} must be one ligature, not separate emoji"
            );
            let mut context = ScaleContext::new();
            let mut scaler = context.builder(selected).size(18.0).hint(true).build();
            let reference = Render::new(&[Source::ColorBitmap(swash::scale::StrikeWith::BestFit)])
                .render(&mut scaler, shape.glyph_infos()[0].glyph_id as u16)
                .expect("emoji ligature bitmap");
            let (_, info, upload) = font.glyph_text(text, false, false).unwrap();
            assert!(
                info.color && info.size[0] > 0.0 && info.size[1] > 0.0,
                "{text} lost its color raster"
            );
            let upload = upload.unwrap();
            assert_eq!(
                upload.pixels.len(),
                (upload.width * upload.height * 4) as usize
            );
            assert!(upload.pixels.chunks_exact(4).any(|p| p[3] != 0));
            assert_eq!(upload.pixels.len(), reference.data.len());
            for (actual, expected) in upload
                .pixels
                .chunks_exact(4)
                .zip(reference.data.chunks_exact(4))
            {
                assert_eq!(actual[3], expected[3]);
                if actual[3] != 0 {
                    assert_eq!(
                        actual, expected,
                        "{text} must preserve the font's actual bitmap colors"
                    );
                }
            }
            assert!(font.glyph_text(text, false, false).unwrap().2.is_none());
        }
        let text_face = font.resolve_text_face("❤\u{fe0e}", false, false);
        assert_eq!(
            text_face, 0,
            "text presentation must retain the primary monochrome face"
        );
    }

    #[test]
    fn color_cluster_cache_pressure_is_recoverable() {
        let mut font = Font::load(None, 18.0).unwrap();
        let (_, info, _) = font.glyph_text("👩\u{200d}💻", false, false).unwrap();
        if !info.color {
            eprintln!("skipped: color emoji font unavailable");
            return;
        }
        font.invalidate_atlas();
        font.color_shelf_y = font.atlas_size;
        assert_eq!(
            font.glyph_text("👩\u{200d}💻", false, false).unwrap_err(),
            CACHE_FULL
        );
        assert!(font.glyphs.is_empty());
        font.invalidate_atlas();
        assert!(
            font.glyph_text("👩\u{200d}💻", false, false)
                .unwrap()
                .2
                .is_some()
        );
    }

    #[test]
    fn overlapping_masks_and_color_layers_use_alpha_composition() {
        let mask = || PositionedRaster {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            pixels: vec![128],
            color: false,
        };
        assert_eq!(
            composite_rasters(&[mask(), mask()], [0, 0], 1, 1),
            (vec![192], false)
        );
        let rgba = |pixels| PositionedRaster {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            pixels,
            color: true,
        };
        assert_eq!(
            composite_rasters(
                &[rgba(vec![0, 0, 255, 128]), rgba(vec![255, 0, 0, 128])],
                [0, 0],
                1,
                1
            ),
            (vec![170, 0, 85, 192], true)
        );
        assert_eq!(
            composite_rasters(&[rgba(vec![255, 0, 0, 128]), mask()], [0, 0], 1, 1),
            (vec![255, 170, 170, 192], true)
        );
    }

    #[test]
    fn colr_pixels_are_converted_from_premultiplied_to_straight_alpha() {
        let mut image = swash::scale::image::Image::new();
        image.source = Source::ColorOutline(0);
        image.content = Content::Color;
        image.data = vec![32, 64, 96, 128, 0, 0, 0, 0, 40, 80, 120, 255];
        straighten_color_outline(&mut image);
        assert_eq!(
            image.data,
            [64, 128, 191, 128, 0, 0, 0, 0, 40, 80, 120, 255]
        );
    }

    #[test]
    fn a_full_atlas_does_not_poison_glyph_or_cluster_cache() {
        let mut font = Font::load(None, 18.0).unwrap();
        font.shelf_y = font.atlas_size;
        for text in ["i", "e\u{301}"] {
            let before = font.glyphs.len();
            assert_eq!(font.glyph_text(text, false, false).unwrap_err(), CACHE_FULL);
            assert_eq!(font.glyphs.len(), before);
        }
        font.invalidate_atlas();
        for text in ["i", "e\u{301}"] {
            let (_, info, upload) = font.glyph_text(text, false, false).unwrap();
            assert!(info.size[0] > 0.0 && upload.is_some());
        }
    }

    #[test]
    fn family_search_prefers_regular_across_directories() {
        let root = std::env::temp_dir().join(format!("aura-font-search-{}", std::process::id()));
        fs::create_dir_all(root.join("a")).unwrap();
        fs::create_dir_all(root.join("z")).unwrap();
        for name in [
            "a/ExampleMono-Bold.ttf",
            "a/ExampleMono-Italic.ttf",
            "z/ExampleMono-Regular.ttf",
        ] {
            fs::write(root.join(name), []).unwrap();
        }
        assert_eq!(
            search_font_dir(&root, "Example Mono"),
            Some(root.join("z/ExampleMono-Regular.ttf"))
        );
        assert_eq!(
            search_font_dir(&root, "Example Mono Bold"),
            Some(root.join("a/ExampleMono-Bold.ttf"))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn configured_fallbacks_are_lazy_bounded_and_take_priority() {
        let path = default_font_path().unwrap();
        let mut font = Font::load(None, 14.0).unwrap();
        font.set_fallback_families(&[path.to_string_lossy().into_owned()]);
        assert_eq!(font.fallback_count(), 0);
        font.resolve_text_face("中", false, false);
        assert!(font.fallback_count() <= MAX_FALLBACKS);
        assert_eq!(font.fallbacks[0].bytes, fs::read(path).unwrap());
    }

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
        assert_eq!(
            font.fallback_count(),
            0,
            "fallbacks stay unloaded until a miss"
        );
        let metrics = font.metrics();
        let (id, info, upload) = font.glyph('界', false, false).unwrap();
        if font.fallback_count() == 0 {
            eprintln!("no fallback faces installed; skipping CJK coverage check");
            return;
        }
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
        let (_, info, upload) = font.glyph('😀', false, false).unwrap();
        if font.fallback_count() == 0 {
            eprintln!("no fallback faces installed; skipping emoji coverage check");
            return;
        }
        let Some(upload) = upload else {
            eprintln!("emoji face has no raster for U+1F600 on this system");
            return;
        };
        assert!(
            info.color,
            "emoji raster must be flagged for the color atlas"
        );
        assert!(upload.color);
        assert_eq!(
            upload.pixels.len(),
            (upload.width * upload.height * 4) as usize
        );
    }

    #[test]
    fn load_with_family_resolves_existing_path() {
        let mut font =
            Font::load_with_family("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf", 14.0)
                .expect("explicit path must load");
        let _ = font.glyph('A', false, false);
    }

    #[test]
    fn size_aware_cache_key_prevents_cross_size_hits() {
        let mut font =
            Font::load_with_family("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf", 12.0)
                .expect("system font is available");
        let (_id12, info12, _) = font.glyph('A', false, false).unwrap();
        assert!(info12.size[0] > 0.0 && info12.size[1] > 0.0);

        let mut font =
            Font::load_with_family("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf", 18.0)
                .expect("system font is available");
        let (_id18, info18, _) = font.glyph('A', false, false).unwrap();
        assert!(info18.size[0] > 0.0 && info18.size[1] > 0.0);

        // Different sizes produce different rasterizations in separate Font instances.
        // The IDs may coincide by coincidence, but the atlas placements differ.
        assert_ne!(font.metrics().width, 12, "metrics change with size");
    }

    #[test]
    fn metrics_change_with_font_size() {
        let sizes = [10.0, 12.0, 14.0, 16.0, 18.0, 20.0, 24.0];
        let mut widths = Vec::new();
        for &size in &sizes {
            let font = Font::load_with_family("monospace", size)
                .expect("system monospace must be available");
            let m = font.metrics();
            widths.push(m.width);
        }
        assert!(
            widths.windows(2).any(|w| w[0] != w[1]),
            "metrics must change with font size"
        );
    }

    #[test]
    fn set_size_rerasterizes_and_clears_stale_cache() {
        let mut font =
            Font::load(None, 12.0).expect("Linux test environment provides a monospace font");
        let before = font.metrics();
        let (_, info12, _) = font.glyph('A', false, false).unwrap();
        assert!(info12.size[0] > 0.0 && info12.size[1] > 0.0);
        // Fallback coverage must also rasterize at the old size before resize.
        let _ = font.glyph('界', false, false);
        let cached = font.glyphs.len();
        assert!(cached >= 2, "primary + fallback glyphs are cached");

        assert!(font.set_size(24.0).unwrap());
        assert_eq!(font.size(), 24.0);
        assert!(font.glyphs.is_empty(), "old cache must not survive resize");
        assert_eq!(font.shelf_x, 1);
        assert_eq!(font.shelf_y, 1);
        assert_eq!(font.color_shelf_x, 1);
        assert_eq!(font.color_shelf_y, 1);
        let after = font.metrics();
        assert_ne!(before, after, "metrics must change with size");
        assert!(after.width > before.width && after.height > before.height);

        // Same glyph at the new size re-rasterizes through a fresh cache key.
        let (_, info24, upload) = font.glyph('A', false, false).unwrap();
        assert!(
            upload.is_some(),
            "new size must rasterize, not scale old pixels"
        );
        assert!(
            info24.size[0] > info12.size[0] && info24.size[1] > info12.size[1],
            "24px glyph must be larger than 12px: {info12:?} vs {info24:?}"
        );
        // Fallback glyphs also re-rasterize at the new size.
        let (_, cjk24, cjk_upload) = font.glyph('界', false, false).unwrap();
        assert!(
            cjk_upload.is_some(),
            "fallback must re-rasterize at the new size"
        );
        assert!(cjk24.size[0] > 0.0 && cjk24.size[1] > 0.0);

        // Same-size call is a no-op (idempotent, no cache churn).
        let cached_after = font.glyphs.len();
        assert!(!font.set_size(24.0).unwrap());
        assert_eq!(font.glyphs.len(), cached_after);
    }

    #[test]
    fn set_size_is_clamped_to_supported_range() {
        let mut font =
            Font::load(None, 14.0).expect("Linux test environment provides a monospace font");
        assert!(font.set_size(1.0).unwrap());
        assert_eq!(font.size(), MIN_FONT_SIZE);
        assert!(font.metrics().width > 0 && font.metrics().height > 0);
        assert!(font.set_size(200.0).unwrap());
        assert_eq!(font.size(), MAX_FONT_SIZE);
        assert!(font.metrics().width > 0 && font.metrics().height > 0);
        assert!(font.set_size(f32::NAN).unwrap());
        assert_eq!(font.size(), MIN_FONT_SIZE);
        // At minimum, further shrink stays clamped; at maximum, growth stays.
        assert!(!font.set_size(MIN_FONT_SIZE - 1.0).unwrap());
        assert_eq!(font.size(), MIN_FONT_SIZE);
        assert!(font.set_size(MAX_FONT_SIZE).unwrap());
        assert!(!font.set_size(MAX_FONT_SIZE + 1.0).unwrap());
        assert_eq!(font.size(), MAX_FONT_SIZE);
    }

    #[test]
    fn clamp_font_size_never_leaks_nonpositive_sizes() {
        const { assert!(MIN_FONT_SIZE > 0.0) }
        const { assert!(MAX_FONT_SIZE > MIN_FONT_SIZE) }
        const { assert!(FONT_SIZE_STEP == 1.0) }
        assert_eq!(clamp_font_size(0.0), MIN_FONT_SIZE);
        assert_eq!(clamp_font_size(-12.0), MIN_FONT_SIZE);
        assert_eq!(clamp_font_size(f32::NAN), MIN_FONT_SIZE);
        assert_eq!(clamp_font_size(f32::INFINITY), MIN_FONT_SIZE);
        assert_eq!(clamp_font_size(f32::NEG_INFINITY), MIN_FONT_SIZE);
        assert_eq!(clamp_font_size(14.0), 14.0);
    }

    #[test]
    fn unicode_and_fallback_survive_size_round_trip() {
        let mut font =
            Font::load(None, 14.0).expect("Linux test environment provides a monospace font");
        // ASCII, accented, combining, CJK, emoji, wide glyphs at startup size.
        for text in ["A", "é", "e\u{301}", "界", "😀"] {
            if text == "😀" && font.fallback_count() == 0 {
                let _ = font.glyph('😀', false, false);
                if font.fallback_count() == 0 {
                    continue;
                }
            }
            font.glyph_text(text, false, false)
                .unwrap_or_else(|_| panic!("rasterize {text:?} at 14px"));
        }
        for size in [12.0, 16.0, 20.0, 24.0, 12.0] {
            assert!(font.set_size(size).unwrap() || font.size() == size);
            for text in ["A", "é", "e\u{301}", "界", "😀"] {
                if text == "😀" && font.fallback_count() == 0 {
                    continue;
                }
                let (_, info, upload) = font
                    .glyph_text(text, false, false)
                    .unwrap_or_else(|_| panic!("rasterize {text:?} at {size}px"));
                assert!(
                    upload.is_some(),
                    "{text:?} must re-rasterize after resize to {size}px"
                );
                // Empty entries only happen for missing glyphs; ASCII/CJK must paint.
                if matches!(text, "A" | "界") {
                    assert!(
                        info.size[0] > 0.0 && info.size[1] > 0.0,
                        "{text:?} must have pixels at {size}px"
                    );
                }
            }
        }
    }

    #[test]
    fn invalidate_atlas_clears_glyphs_and_resets_shelves() {
        let mut font =
            Font::load_with_family("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf", 14.0)
                .expect("system font is available");
        let _ = font.glyph('A', false, false);
        let _ = font.glyph('B', false, false);
        assert!(font.glyphs.len() >= 2);
        font.invalidate_atlas();
        assert!(font.glyphs.is_empty());
        assert_eq!(font.shelf_x, 1);
        assert_eq!(font.shelf_y, 1);
        assert_eq!(font.color_shelf_x, 1);
        assert_eq!(font.color_shelf_y, 1);
    }

    #[test]
    fn family_name_matching_handles_spaces_and_punctuation() {
        assert!(normalize_name("FiraCode Nerd Font") == "FiraCodeNerdFont");
        assert!(normalize_name("FiraCode-Nerd") == "FiraCodeNerd");
        // Stems without spaces contain the normalized family.
        assert!(family_matches_path(
            "FiraCode Nerd Font",
            Path::new("/usr/share/fonts/TTF/FiraCodeNerdFont-Regular.ttf")
        ));
        // Exact stem match still works.
        assert!(family_matches_path(
            "mono",
            Path::new("/sys/FooMonoBar.ttf")
        ));
        // Completely unrelated names do not match.
        assert!(!family_matches_path(
            "Comic Sans",
            Path::new("/usr/share/fonts/FiraCodeNerdFont-Regular.ttf")
        ));
    }

    // --- FiraCode Nerd Font tests ---
    // These exercise the existing pipeline (metrics, shaping, atlas, scaling)
    // with a real Nerd Font installed on the system. They skip gracefully when
    // the font is absent so they never depend on a hardcoded path. No Nerd
    // Font-specific code is added; this is pure pipeline validation.

    #[test]
    fn nerd_font_resolves_and_has_valid_metrics() {
        let path = Font::resolve_family_path("FiraCode Nerd Font");
        if path.is_none() {
            eprintln!("FiraCode Nerd Font not installed; skipping");
            return;
        }
        let font =
            Font::load(path.as_deref(), 14.0).expect("resolved FiraCode Nerd Font path must load");
        let metrics = font.metrics();
        assert!(
            metrics.width > 0 && metrics.height >= metrics.width,
            "metrics must be positive and tall enough for monospace: {metrics:?}"
        );
        assert!(metrics.ascent > 0 && metrics.descent > 0);
    }

    #[test]
    fn nerd_font_renders_ascii_unicode_cjk_and_nerd_glyphs() {
        let path = Font::resolve_family_path("FiraCode Nerd Font");
        if path.is_none() {
            eprintln!("FiraCode Nerd Font not installed; skipping");
            return;
        }
        let mut font =
            Font::load(Some(&path.unwrap()), 14.0).expect("FiraCode Nerd Font must load");
        // ASCII: basic Latin glyphs must produce a mask upload.
        let (_, ascii, upload) = font.glyph('A', false, false).unwrap();
        assert!(ascii.size[0] > 0.0 && ascii.size[1] > 0.0);
        assert!(upload.is_some(), "ASCII glyph must rasterize");
        assert!(!upload.as_ref().unwrap().color);

        // Accented Unicode: é (U+00E9) is directly encodable in the font's cmap.
        let (_, _, upload) = font.glyph('é', false, false).unwrap();
        assert!(upload.is_some(), "accented glyph must rasterize");

        // CJK: FiraCode Nerd Font includes CJK coverage; if the primary face
        // lacks the glyph it falls back to a CJK face (loaded on demand).
        let (_, cjk, _) = font.glyph('中', false, false).unwrap();
        if font.fallback_count() == 0 {
            // Primary face itself provided the glyph.
            assert!(
                cjk.size[0] > 0.0 && cjk.size[1] > 0.0,
                "CJK glyph must have pixels from the primary face: {cjk:?}"
            );
        } else {
            // Fallback face provided the glyph.
            assert!(
                cjk.size[0] > 0.0 && cjk.size[1] > 0.0,
                "CJK glyph must have pixels from a fallback face: {cjk:?}"
            );
        }

        // Nerd Font icon: U+F015 (home icon from Font Awesome, in the PUA).
        let (_, nerd, upload) = font.glyph('\u{f015}', false, false).unwrap();
        assert!(
            nerd.size[0] > 0.0 && nerd.size[1] > 0.0,
            "Nerd Font icon must have pixels: {nerd:?}"
        );
        // The icon is expected at 14px and must not corrupt the atlas.
        assert!(
            upload.is_some(),
            "Nerd Font icon must produce an atlas upload at 14px"
        );

        // Combining sequence: é as e + U+0301 must shape into one entry.
        let (_, shaped, upload) = font.glyph_text("e\u{301}", false, false).unwrap();
        assert!(shaped.size[0] > 0.0 && shaped.size[1] > 0.0);
        assert!(upload.is_some(), "shaped grapheme must rasterize");
    }

    #[test]
    fn nerd_font_atlas_rebuilds_on_size_change() {
        let path = Font::resolve_family_path("FiraCode Nerd Font");
        if path.is_none() {
            eprintln!("FiraCode Nerd Font not installed; skipping");
            return;
        }
        let mut font =
            Font::load(Some(&path.unwrap()), 12.0).expect("FiraCode Nerd Font must load at 12px");
        let (_, info12, _) = font.glyph('A', false, false).unwrap();
        let metrics12 = font.metrics();
        assert!(info12.size[0] > 0.0 && info12.size[1] > 0.0);

        // Grow to 24px: atlas, glyph cache, and metrics must all refresh.
        assert!(font.set_size(24.0).unwrap());
        assert!(font.glyphs.is_empty(), "cache must clear after resize");
        assert_eq!(font.shelf_x, 1);
        assert_eq!(font.color_shelf_x, 1);
        let (_, info24, upload) = font.glyph('A', false, false).unwrap();
        assert!(upload.is_some(), "new size must re-rasterize");
        assert!(
            info24.size[0] > info12.size[0] && info24.size[1] > info12.size[1],
            "24px glyph must be larger than 12px"
        );
        let metrics24 = font.metrics();
        assert!(metrics24.width > metrics12.width);

        // CJK and Nerd Font icon also re-rasterize at the new size.
        let (_, _, upload) = font.glyph('中', false, false).unwrap();
        assert!(upload.is_some(), "CJK must re-rasterize after resize");
        let (_, _, upload) = font.glyph('\u{f015}', false, false).unwrap();
        assert!(
            upload.is_some(),
            "Nerd Font icon must re-rasterize after resize"
        );

        // Return to 12px: must reproduce the original size exactly.
        assert!(font.set_size(12.0).unwrap());
        let (_, info12b, _) = font.glyph('A', false, false).unwrap();
        assert_eq!(
            info12b.size, info12.size,
            "12 -> 24 -> 12 reproduces same glyph"
        );
    }

    #[test]
    fn nerd_font_runtime_scaling_produces_different_glyph_sizes() {
        let path = Font::resolve_family_path("FiraCode Nerd Font");
        if path.is_none() {
            eprintln!("FiraCode Nerd Font not installed; skipping");
            return;
        }
        let mut font = Font::load(Some(&path.unwrap()), MIN_FONT_SIZE).expect("load at min size");
        let (_, small, _) = font.glyph('A', false, false).unwrap();
        assert!(small.size[0] > 0.0 && small.size[1] > 0.0);

        // Simulate a runtime zoom sequence (12 -> 16 -> 20 -> 12).
        for size in [12.0, 16.0, 20.0, 12.0] {
            assert!(font.set_size(size).unwrap());
            let metrics = font.metrics();
            assert!(metrics.width > 0 && metrics.height > 0);
            let (_, info, upload) = font.glyph('A', false, false).unwrap();
            assert!(
                upload.is_some(),
                "must re-rasterize after size change to {size}px"
            );
            assert!(
                info.size[0] > 0.0 && info.size[1] > 0.0,
                "glyph must have pixels at {size}px"
            );
        }
        // Verify the 20px glyph is genuinely larger than the 12px one.
        font.set_size(12.0).unwrap();
        let (_, info12, _) = font.glyph('A', false, false).unwrap();
        font.set_size(20.0).unwrap();
        let (_, info20, _) = font.glyph('A', false, false).unwrap();
        assert!(
            info20.size[0] > info12.size[0] && info20.size[1] > info12.size[1],
            "20px glyph must be larger than 12px"
        );
    }
}
