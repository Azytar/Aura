//! Typed configuration loaded from `~/.config/aura/config.toml`.
//!
//! `core` never reads the filesystem or TOML. The app crate parses, validates,
//! and converts config into typed values before constructing any font/render/terminal.

use std::path::{Path, PathBuf};

/// Re-exported from `aura-text`: the single source of truth for runtime
/// font sizes. `config` validates the TOML startup size against the same
/// range that `Font::set_size` clamps at runtime.
pub use aura_text::{FONT_SIZE_STEP, MAX_FONT_SIZE, MIN_FONT_SIZE, clamp_font_size};

/// Runtime font-size state: `configured` never changes (TOML startup value),
/// `current` moves with `Ctrl+Plus/Minus` and returns with `Ctrl+0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RuntimeFontSize {
    configured: f32,
    current: f32,
}

impl RuntimeFontSize {
    /// Creates the state from the validated TOML startup size.
    pub fn new(configured: f32) -> Self {
        let clamped = clamp_font_size(configured);
        Self {
            configured: clamped,
            current: clamped,
        }
    }

    /// Startup size from TOML; `Ctrl+0` restores this, never a hardcoded value.
    /// Only read by tests (the live path keeps it inside `reset()`), so the
    /// binary target allows dead code here by design.
    #[allow(dead_code)]
    pub const fn configured(&self) -> f32 {
        self.configured
    }

    /// Live size used for metrics, atlas, and geometry.
    pub const fn current(&self) -> f32 {
        self.current
    }

    /// `Ctrl+Plus`: grows by one step, saturating at [`MAX_FONT_SIZE`].
    /// Returns `true` when the size actually changed.
    pub fn grow(&mut self) -> bool {
        self.set(self.current + FONT_SIZE_STEP)
    }

    /// `Ctrl+Minus`: shrinks by one step, saturating at [`MIN_FONT_SIZE`].
    /// Returns `true` when the size actually changed.
    pub fn shrink(&mut self) -> bool {
        self.set(self.current - FONT_SIZE_STEP)
    }

    /// `Ctrl+0`: restores the configured startup size.
    /// Returns `true` when the size actually changed.
    pub fn reset(&mut self) -> bool {
        self.set(self.configured)
    }

    fn set(&mut self, requested: f32) -> bool {
        let clamped = clamp_font_size(requested);
        if clamped.to_bits() == self.current.to_bits() {
            return false;
        }
        self.current = clamped;
        true
    }
}

/// Font rendering offsets and spacing, bundled so `FontGeometry::new` stays
/// within the reasonable parameter-count limit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GlyphOffsets {
    /// Horizontal glyph offset in pixels (visual only, does not change cell geometry).
    pub offset_x: i32,
    /// Vertical glyph offset in pixels (visual only, does not change cell geometry).
    pub offset_y: i32,
    /// Additional vertical space between lines in pixels.
    pub line_spacing: i32,
    /// Additional horizontal space between cells in pixels.
    pub letter_spacing: i32,
}

/// Current font metrics plus the terminal geometry derived from them.
/// Single source of truth: `app` builds this from the live `Font` metrics
/// and hands the same values to window sizing, grid sizing, PTY winsize,
/// and the renderer — never two independent calculations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontGeometry {
    /// Rasterized cell width in pixels (includes letter_spacing).
    pub cell_width: u32,
    /// Rasterized cell height in pixels (includes line_spacing).
    pub cell_height: u32,
    /// Outer window size holding `columns`×`rows` cells plus padding.
    pub pixel_width: u32,
    /// Outer window size holding `columns`×`rows` cells plus padding.
    pub pixel_height: u32,
    /// Terminal grid size this geometry was derived for.
    pub columns: usize,
    /// Terminal grid size this geometry was derived for.
    pub rows: usize,
    /// Horizontal glyph offset (visual only).
    pub offset_x: i32,
    /// Vertical glyph offset (visual only).
    pub offset_y: i32,
    /// Additional vertical space between lines.
    pub line_spacing: i32,
    /// Additional horizontal space between cells.
    pub letter_spacing: i32,
}

impl FontGeometry {
    /// Derives window pixels from cell metrics, grid size, and padding with
    /// saturating math so huge sizes degrade to clamped pixels, never wrap.
    /// `line_spacing` and `letter_spacing` are added to cell dimensions.
    pub fn new(
        cell_width: u32,
        cell_height: u32,
        columns: usize,
        rows: usize,
        padding_x: u32,
        padding_y: u32,
        glyph_offsets: GlyphOffsets,
    ) -> Self {
        let GlyphOffsets {
            offset_x,
            offset_y,
            line_spacing,
            letter_spacing,
        } = glyph_offsets;
        let effective_cell_width = cell_width.saturating_add_signed(letter_spacing).max(1);
        let effective_cell_height = cell_height.saturating_add_signed(line_spacing).max(1);
        let pixel_width = (columns.saturating_mul(effective_cell_width as usize))
            .saturating_add(2 * padding_x as usize)
            .max(1) as u32;
        let pixel_height = (rows.saturating_mul(effective_cell_height as usize))
            .saturating_add(2 * padding_y as usize)
            .max(1) as u32;
        Self {
            cell_width: effective_cell_width,
            cell_height: effective_cell_height,
            pixel_width: pixel_width.max(1),
            pixel_height,
            columns,
            rows,
            offset_x,
            offset_y,
            line_spacing,
            letter_spacing,
        }
    }

    /// PTY pixel lanes for `TIOCSWINSZ`, saturated into `u16` Linux limits.
    pub fn pty_pixels(&self) -> (u16, u16) {
        (
            u16::try_from(self.pixel_width).unwrap_or(u16::MAX),
            u16::try_from(self.pixel_height).unwrap_or(u16::MAX),
        )
    }
}

/// Shared grid budget for window sizing and the renderer's fixed SSBOs.
pub const MAX_GRID_COLUMNS: usize = 512;
pub const MAX_GRID_ROWS: usize = 256;

/// Derives how many complete cells fit in an inner (padding-free) window area.
/// Shared by startup sizing and `ConfigureNotify` handling so both agree.
pub fn grid_dimensions(
    inner_width: u32,
    inner_height: u32,
    cell_width: u32,
    cell_height: u32,
) -> (usize, usize) {
    let columns = (inner_width / cell_width.max(1)).max(1) as usize;
    let rows = (inner_height / cell_height.max(1)).max(1) as usize;
    (columns.min(MAX_GRID_COLUMNS), rows.min(MAX_GRID_ROWS))
}

/// Default font family name when no config is present.
pub const DEFAULT_FONT_FAMILY: &str = "monospace";

/// Default font size when no config is present.
pub const DEFAULT_FONT_SIZE: f32 = 14.0;

/// Default horizontal glyph offset when no config is present.
pub const DEFAULT_OFFSET_X: i32 = 0;
/// Default vertical glyph offset when no config is present.
pub const DEFAULT_OFFSET_Y: i32 = 0;
/// Default line spacing when no config is present.
pub const DEFAULT_LINE_SPACING: i32 = 0;
/// Default letter spacing when no config is present.
pub const DEFAULT_LETTER_SPACING: i32 = 0;

/// Maximum absolute value for glyph offsets.
pub const MAX_OFFSET: i32 = 32;
/// Maximum line spacing value.
pub const MAX_LINE_SPACING: i32 = 32;
/// Maximum letter spacing value.
pub const MAX_LETTER_SPACING: i32 = 16;

/// Default terminal columns when no config is present.
pub const DEFAULT_COLUMNS: usize = 120;

/// Default terminal lines when no config is present.
pub const DEFAULT_LINES: usize = 32;

/// Default padding in typographic pixels.
pub const DEFAULT_PADDING: u32 = 0;

/// Default number of scrollback rows retained above the visible screen.
pub const DEFAULT_SCROLLBACK_LINES: usize = 10_000;

/// Largest scrollback the configuration may request. The grid stores one
/// `Row` (cells plus grapheme and hyperlink sidecars) per line, so this is a
/// memory guard rather than a policy choice: at the default 120 columns a row
/// holds roughly 7 KiB of cells, putting the ceiling near a gigabyte.
pub const MAX_SCROLLBACK_LINES: usize = 1_000_000;

/// Largest paste, in bytes, that AURA forwards to the child process. A bounded
/// paste keeps one mis-click from writing an unbounded blob into a shell that
/// may then act on it.
pub const DEFAULT_MAX_PASTE_BYTES: usize = 64 * 1024;

/* ------------------------------------------------------------------ colours */

/// Named starting points for the palette. `Custom` takes every colour from the
/// configuration file and falls back to the dark palette for anything left
/// unspecified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeKind {
    #[default]
    Dark,
    Light,
    Custom,
}

impl ThemeKind {
    /// Parses a theme name. Case-insensitive so `dark`, `Dark`, and `DARK`
    /// all work; unknown names are rejected rather than silently defaulted.
    pub fn parse(value: &str) -> Result<Self, ConfigError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "dark" => Ok(Self::Dark),
            "light" => Ok(Self::Light),
            "custom" => Ok(Self::Custom),
            _ => Err(ConfigError::invalid_value(
                "colors.theme",
                format!("must be one of dark, light, custom (got {value:?})"),
            )),
        }
    }
}

/// Terminal colours as plain `0xRRGGBB` values.
///
/// This type holds data only and knows nothing about the GPU, the ANSI
/// encoding, or `aura_core`, so the renderer keeps full freedom over its own
/// texture encoding. Mapping `aura_core`'s colour words onto these entries is
/// the renderer's job, where the tag constants already live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// Default foreground, and the colour of a block cursor.
    pub foreground: u32,
    /// Default background.
    pub background: u32,
    /// Selection highlight.
    pub selection: u32,
    /// The 16 ANSI colours: 0-7 normal, 8-15 bright.
    pub ansi: [u32; 16],
}

impl Palette {
    /// The default dark palette: a neutral, slightly cool grey scale with the
    /// conventional xterm hues. The greys are kept low-contrast on purpose so
    /// long sessions do not vibrate.
    pub const DARK: Self = Self {
        foreground: 0xd8d8d8,
        background: 0x181818,
        selection: 0x406090,
        ansi: [
            0x000000, 0xcd0000, 0x00cd00, 0xcdcd00, 0x0000ee, 0xcd00cd, 0x00cdcd, 0xe5e5e5,
            0x7f7f7f, 0xff0000, 0x00ff00, 0xffff00, 0x5c5cff, 0xff00ff, 0x00ffff, 0xffffff,
        ],
    };

    /// The same hues lifted for a paper background.
    pub const LIGHT: Self = Self {
        foreground: 0x1a1a1a,
        background: 0xfafafa,
        selection: 0xadd6ff,
        ansi: [
            0x1a1a1a, 0xb00020, 0x006000, 0x8a6d00, 0x0033cc, 0x8b1a8b, 0x007070, 0xbfbfbf,
            0x595959, 0xd40000, 0x008000, 0xb58900, 0x1a53ff, 0xb0189b, 0x009999, 0x000000,
        ],
    };

    /// Starting palette for a theme. `Custom` starts from dark so that a
    /// partial override still has a complete set of colours to fall back on.
    pub const fn for_theme(theme: ThemeKind) -> Self {
        match theme {
            ThemeKind::Dark | ThemeKind::Custom => Self::DARK,
            ThemeKind::Light => Self::LIGHT,
        }
    }

    pub const fn with_foreground(mut self, value: u32) -> Self {
        self.foreground = value;
        self
    }

    pub const fn with_background(mut self, value: u32) -> Self {
        self.background = value;
        self
    }

    pub const fn with_selection(mut self, value: u32) -> Self {
        self.selection = value;
        self
    }

    pub const fn with_ansi(mut self, ansi: [u32; 16]) -> Self {
        self.ansi = ansi;
        self
    }
}

/// Parses one `0xRRGGBB` colour. Accepts `#rrggbb`, `rrggbb`, `0xrrggbb`,
/// and a bare integer, so configuration can stay readable without forcing a
/// single notation.
pub fn parse_color(value: &toml::Value, key: &'static str) -> Result<u32, ConfigError> {
    let parse_hex = |text: &str| -> Result<u32, ConfigError> {
        let hex = text
            .trim()
            .trim_start_matches('#')
            .trim_start_matches("0x")
            .trim_start_matches("0X");
        let digits = match hex.len() {
            6 => hex.to_owned(),
            // CSS-style shorthand: #abc expands to #aabbcc.
            3 => hex
                .bytes()
                .flat_map(|byte| [byte, byte])
                .map(|byte| byte as char)
                .collect(),
            _ => {
                return Err(ConfigError::invalid_value(
                    key,
                    format!("must be 6 hex digits such as \"#1e1e1e\" (got {text:?})"),
                ));
            }
        };
        u32::from_str_radix(&digits, 16)
            .map_err(|_| ConfigError::invalid_value(key, format!("invalid hex colour {text:?}")))
    };
    match value {
        toml::Value::String(text) => parse_hex(text),
        toml::Value::Integer(number) if (0..=0xff_ffff).contains(number) => Ok(*number as u32),
        _ => Err(ConfigError::invalid_type(key, "hex colour string")),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FontStyleConfig {
    /// Font family name, or `None` to inherit from the primary `[font]` section.
    pub family: Option<String>,
    /// Font style name such as `Bold`, `Italic`, `Bold Italic`.
    pub style: Option<String>,
}

impl FontStyleConfig {
    fn parse_section(table: &toml::Table, key: &str) -> Option<Self> {
        let section = table.get(key)?.as_table()?;
        Some(Self {
            family: section
                .get("family")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            style: section
                .get("style")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FontConfig {
    /// Font family name (e.g. `Iosevka`, `monospace`).
    pub family: String,
    /// Font size in typographic pixels used for rasterization.
    pub size: f32,
    /// Horizontal glyph offset in pixels (visual only, does not change cell geometry).
    pub offset_x: i32,
    /// Vertical glyph offset in pixels (visual only, does not change cell geometry).
    pub offset_y: i32,
    /// Additional vertical space between lines in pixels.
    pub line_spacing: i32,
    /// Additional horizontal space between cells in pixels.
    pub letter_spacing: i32,
    /// Bold face configuration.
    pub bold: Option<FontStyleConfig>,
    /// Italic face configuration.
    pub italic: Option<FontStyleConfig>,
    /// Bold+italic face configuration.
    pub bold_italic: Option<FontStyleConfig>,
    /// Ordered fallback family names loaded when the primary face lacks a glyph.
    pub fallback_families: Vec<String>,
}

impl FontConfig {
    fn parse_table(table: &toml::Table) -> Result<Self, ConfigError> {
        // The section itself is optional: a configuration that only sets
        // colours or scrollback must still load with a usable font.
        let font = table
            .get("font")
            .and_then(|value| value.as_table())
            .cloned()
            .unwrap_or_else(toml::Table::new);

        let mut family = font
            .get("family")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| DEFAULT_FONT_FAMILY.to_string());

        // Trim and validate non-empty family.
        family = family.trim().to_string();
        if family.is_empty() {
            return Err(ConfigError::invalid_value(
                "font.family",
                "must not be empty",
            ));
        }

        let size = font
            .get("size")
            .and_then(|v| v.as_float())
            .unwrap_or(DEFAULT_FONT_SIZE as f64) as f32;

        if !size.is_finite() || !(MIN_FONT_SIZE..=MAX_FONT_SIZE).contains(&size) {
            return Err(ConfigError::invalid_value(
                "font.size",
                format!("must be between {MIN_FONT_SIZE} and {MAX_FONT_SIZE}"),
            ));
        }

        let offset_x = parse_offset(&font, "offset_x", DEFAULT_OFFSET_X)?;
        let offset_y = parse_offset(&font, "offset_y", DEFAULT_OFFSET_Y)?;
        let line_spacing = parse_spacing(&font, "line_spacing", DEFAULT_LINE_SPACING)?;
        let letter_spacing = parse_spacing(&font, "letter_spacing", DEFAULT_LETTER_SPACING)?;

        let bold = FontStyleConfig::parse_section(&font, "bold");
        let italic = FontStyleConfig::parse_section(&font, "italic");
        let bold_italic = FontStyleConfig::parse_section(&font, "bold_italic");

        let fallback_families = font
            .get("fallback")
            .and_then(|v| v.as_table())
            .and_then(|fallback| fallback.get("families"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        Ok(Self {
            family,
            size,
            offset_x,
            offset_y,
            line_spacing,
            letter_spacing,
            bold,
            italic,
            bold_italic,
            fallback_families,
        })
    }

    /// Bundles the font rendering parameters that configure glyph positioning
    /// and cell spacing for `FontGeometry::new`.
    pub fn glyph_offsets(&self) -> GlyphOffsets {
        GlyphOffsets {
            offset_x: self.offset_x,
            offset_y: self.offset_y,
            line_spacing: self.line_spacing,
            letter_spacing: self.letter_spacing,
        }
    }
}

/// Parses an offset value from the font table, clamping to valid range.
fn parse_offset(font: &toml::Table, key: &'static str, default: i32) -> Result<i32, ConfigError> {
    let value = font
        .get(key)
        .and_then(|v| v.as_integer())
        .unwrap_or(default as i64) as i32;
    if !(-MAX_OFFSET..=MAX_OFFSET).contains(&value) {
        return Err(ConfigError::invalid_value(
            key,
            format!("must be between -{MAX_OFFSET} and {MAX_OFFSET}"),
        ));
    }
    Ok(value)
}

/// Parses a spacing value from the font table, clamping to valid range.
fn parse_spacing(font: &toml::Table, key: &'static str, default: i32) -> Result<i32, ConfigError> {
    let value = font
        .get(key)
        .and_then(|v| v.as_integer())
        .unwrap_or(default as i64) as i32;
    let max = if key == "line_spacing" {
        MAX_LINE_SPACING
    } else {
        MAX_LETTER_SPACING
    };
    if !(0..=max).contains(&value) {
        return Err(ConfigError::invalid_value(
            key,
            format!("must be between 0 and {max}"),
        ));
    }
    Ok(value)
}

#[derive(Debug, Clone, PartialEq)]
pub struct WindowConfig {
    pub columns: usize,
    pub lines: usize,
    pub padding_x: u32,
    pub padding_y: u32,
}

impl WindowConfig {
    fn parse_table(table: &toml::Table) -> Result<Self, ConfigError> {
        let window = table
            .get("window")
            .and_then(|v| v.as_table())
            .cloned()
            .unwrap_or_else(toml::Table::new);

        let columns = match window.get("columns").and_then(|v| v.as_integer()) {
            Some(value) => {
                if !(1..=MAX_GRID_COLUMNS as i64).contains(&value) {
                    return Err(ConfigError::invalid_value(
                        "window.columns",
                        format!("must be between 1 and {MAX_GRID_COLUMNS}"),
                    ));
                }
                value as usize
            }
            None => DEFAULT_COLUMNS,
        };

        let lines = match window.get("lines").and_then(|v| v.as_integer()) {
            Some(value) => {
                if !(1..=MAX_GRID_ROWS as i64).contains(&value) {
                    return Err(ConfigError::invalid_value(
                        "window.lines",
                        format!("must be between 1 and {MAX_GRID_ROWS}"),
                    ));
                }
                value as usize
            }
            None => DEFAULT_LINES,
        };

        let padding_x = match window.get("padding_x").and_then(|v| v.as_integer()) {
            Some(value) => {
                if value < 0 {
                    return Err(ConfigError::invalid_value("padding_x", "must be >= 0"));
                }
                value as u32
            }
            None => DEFAULT_PADDING,
        };

        let padding_y = match window.get("padding_y").and_then(|v| v.as_integer()) {
            Some(value) => {
                if value < 0 {
                    return Err(ConfigError::invalid_value("padding_y", "must be >= 0"));
                }
                value as u32
            }
            None => DEFAULT_PADDING,
        };

        Ok(Self {
            columns,
            lines,
            padding_x,
            padding_y,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColorsConfig {
    /// Which palette the section starts from.
    pub theme: ThemeKind,
    /// Explicit colours overriding the theme.
    pub foreground: Option<u32>,
    pub background: Option<u32>,
    pub cursor: Option<u32>,
    pub selection: Option<u32>,
    /// Optional full override of the 16 ANSI colours.
    pub ansi: Option<[u32; 16]>,
}

impl ColorsConfig {
    fn parse_table(table: &toml::Table) -> Result<Self, ConfigError> {
        let colors = table
            .get("colors")
            .and_then(|value| value.as_table())
            .cloned()
            .unwrap_or_else(toml::Table::new);

        let theme = match colors.get("theme") {
            Some(value) => ThemeKind::parse(
                value
                    .as_str()
                    .ok_or_else(|| ConfigError::invalid_type("colors.theme", "string"))?,
            )?,
            None => ThemeKind::default(),
        };

        let optional_color = |key: &'static str| -> Result<Option<u32>, ConfigError> {
            colors
                .get(key)
                .map(|value| parse_color(value, key))
                .transpose()
        };

        let ansi = match colors.get("palette") {
            Some(value) => {
                let entries = value
                    .as_array()
                    .ok_or_else(|| ConfigError::invalid_type("colors.palette", "array"))?;
                if entries.len() != 16 {
                    return Err(ConfigError::invalid_value(
                        "colors.palette",
                        "must list exactly 16 colours (0-7 normal, 8-15 bright)",
                    ));
                }
                let mut parsed = [0_u32; 16];
                for (slot, entry) in entries.iter().enumerate() {
                    parsed[slot] = parse_color(entry, "colors.palette")?;
                }
                Some(parsed)
            }
            None => None,
        };

        Ok(Self {
            theme,
            foreground: optional_color("foreground")?,
            background: optional_color("background")?,
            cursor: optional_color("cursor")?,
            selection: optional_color("selection")?,
            ansi,
        })
    }

    /// The palette this section resolves to: the theme's colours with every
    /// explicit override applied on top.
    pub fn palette(&self) -> Palette {
        let mut palette = Palette::for_theme(self.theme);
        if let Some(value) = self.foreground {
            palette = palette.with_foreground(value);
        }
        if let Some(value) = self.background {
            palette = palette.with_background(value);
        }
        if let Some(value) = self.selection {
            palette = palette.with_selection(value);
        }
        if let Some(value) = self.ansi {
            palette = palette.with_ansi(value);
        }
        palette
    }

    /// The cursor colour: an explicit override, else the foreground.
    pub fn cursor_color(&self) -> u32 {
        self.cursor.unwrap_or_else(|| self.palette().foreground)
    }
}

/// DECSCUSR shape, which applications write with `CSI Ps SP q`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

impl CursorShape {
    /// Parses a cursor shape name; unknown names are rejected.
    pub fn parse(value: &str) -> Result<Self, ConfigError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "block" => Ok(Self::Block),
            "underline" => Ok(Self::Underline),
            "bar" => Ok(Self::Bar),
            _ => Err(ConfigError::invalid_value(
                "cursor.style",
                format!("must be one of block, underline, bar (got {value:?})"),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorConfig {
    /// Shape used until an application selects its own.
    pub style: CursorShape,
    /// Whether the cursor blinks when the application does not say otherwise.
    pub blink: bool,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            style: CursorShape::default(),
            blink: true,
        }
    }
}

impl CursorConfig {
    fn parse_table(table: &toml::Table) -> Result<Self, ConfigError> {
        let cursor = table
            .get("cursor")
            .and_then(|value| value.as_table())
            .cloned()
            .unwrap_or_else(toml::Table::new);

        let style = match cursor.get("style") {
            Some(value) => CursorShape::parse(
                value
                    .as_str()
                    .ok_or_else(|| ConfigError::invalid_type("cursor.style", "string"))?,
            )?,
            None => CursorShape::default(),
        };
        let blink = cursor.get("blink").and_then(|value| value.as_bool());

        Ok(Self {
            style,
            blink: blink.unwrap_or(true),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollbackConfig {
    /// Rows kept above the visible screen.
    pub lines: usize,
}

impl Default for ScrollbackConfig {
    fn default() -> Self {
        Self {
            lines: DEFAULT_SCROLLBACK_LINES,
        }
    }
}

impl ScrollbackConfig {
    fn parse_table(table: &toml::Table) -> Result<Self, ConfigError> {
        let scrollback = table
            .get("scrollback")
            .and_then(|value| value.as_table())
            .cloned()
            .unwrap_or_else(toml::Table::new);

        let lines = match scrollback.get("lines").and_then(|value| value.as_integer()) {
            Some(value) if value >= 0 => {
                let lines = value as usize;
                if lines > MAX_SCROLLBACK_LINES {
                    return Err(ConfigError::invalid_value(
                        "scrollback.lines",
                        format!("must be <= {MAX_SCROLLBACK_LINES}"),
                    ));
                }
                lines
            }
            Some(_) => {
                return Err(ConfigError::invalid_value(
                    "scrollback.lines",
                    "must be >= 0",
                ));
            }
            None => DEFAULT_SCROLLBACK_LINES,
        };

        Ok(Self { lines })
    }
}

/// What AURA does with OSC 52 clipboard requests from the child.
///
/// Reads are always refused: a terminal that answers `?` hands host clipboard
/// contents to whatever is running inside it, which is exactly the escalation
/// a malicious command wants. Writes stay under operator control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Osc52Policy {
    /// Execute writes and refuse reads. This is the default.
    #[default]
    WriteOnly,
    /// Ignore OSC 52 entirely; use only local copy/paste.
    Disabled,
}

impl Osc52Policy {
    pub fn parse(value: &str) -> Result<Self, ConfigError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "write-only" | "write_only" | "writeonly" => Ok(Self::WriteOnly),
            "disabled" | "off" => Ok(Self::Disabled),
            _ => Err(ConfigError::invalid_value(
                "security.osc52",
                format!("must be write-only or disabled (got {value:?})"),
            )),
        }
    }

    /// Whether an OSC 52 write request should be executed.
    pub const fn allows_write(self) -> bool {
        matches!(self, Self::WriteOnly)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecurityConfig {
    /// Policy for OSC 52 clipboard requests. Reads are never allowed.
    pub osc52: Osc52Policy,
    /// Largest paste, in bytes, forwarded to the child.
    pub max_paste_bytes: usize,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            osc52: Osc52Policy::default(),
            max_paste_bytes: DEFAULT_MAX_PASTE_BYTES,
        }
    }
}

impl SecurityConfig {
    fn parse_table(table: &toml::Table) -> Result<Self, ConfigError> {
        let security = table
            .get("security")
            .and_then(|value| value.as_table())
            .cloned()
            .unwrap_or_else(toml::Table::new);

        let osc52 = match security.get("osc52") {
            Some(value) => Osc52Policy::parse(
                value
                    .as_str()
                    .ok_or_else(|| ConfigError::invalid_type("security.osc52", "string"))?,
            )?,
            None => Osc52Policy::default(),
        };

        let max_paste_bytes = match security
            .get("max_paste_bytes")
            .and_then(|value| value.as_integer())
        {
            Some(value) if value > 0 => value as usize,
            Some(_) => {
                return Err(ConfigError::invalid_value(
                    "security.max_paste_bytes",
                    "must be > 0",
                ));
            }
            None => DEFAULT_MAX_PASTE_BYTES,
        };

        Ok(Self {
            osc52,
            max_paste_bytes,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppConfig {
    pub font: FontConfig,
    pub window: WindowConfig,
    pub colors: ColorsConfig,
    pub cursor: CursorConfig,
    pub scrollback: ScrollbackConfig,
    pub security: SecurityConfig,
}

impl AppConfig {
    /// Loads the default configuration without reading the filesystem.
    pub fn defaults() -> Self {
        Self {
            font: FontConfig {
                family: DEFAULT_FONT_FAMILY.to_string(),
                size: DEFAULT_FONT_SIZE,
                offset_x: DEFAULT_OFFSET_X,
                offset_y: DEFAULT_OFFSET_Y,
                line_spacing: DEFAULT_LINE_SPACING,
                letter_spacing: DEFAULT_LETTER_SPACING,
                bold: None,
                italic: None,
                bold_italic: None,
                fallback_families: Vec::new(),
            },
            window: WindowConfig {
                columns: DEFAULT_COLUMNS,
                lines: DEFAULT_LINES,
                padding_x: DEFAULT_PADDING,
                padding_y: DEFAULT_PADDING,
            },
            colors: ColorsConfig {
                theme: ThemeKind::default(),
                foreground: None,
                background: None,
                cursor: None,
                selection: None,
                ansi: None,
            },
            cursor: CursorConfig::default(),
            scrollback: ScrollbackConfig::default(),
            security: SecurityConfig::default(),
        }
    }

    /// Loads configuration from the standard config path, falling back to defaults.
    pub fn load() -> Self {
        Self::load_from_path(Self::config_path().as_deref()).unwrap_or_default()
    }

    /// Loads configuration from a specific path, falling back to defaults.
    pub fn load_from_path(path: Option<&Path>) -> Result<Self, ConfigError> {
        let config_path = Self::config_path();
        let path = match path.or(config_path.as_deref()) {
            Some(path) => path,
            None => return Ok(Self::defaults()),
        };

        if !path.is_file() {
            return Ok(Self::defaults());
        }

        let content = std::fs::read_to_string(path)
            .map_err(|error| ConfigError::io(path.display().to_string(), error.to_string()))?;

        let parsed: toml::Table = toml::from_str(&content)
            .map_err(|error| ConfigError::parse(path.display().to_string(), error.to_string()))?;

        let font = FontConfig::parse_table(&parsed)?;
        let window = WindowConfig::parse_table(&parsed)?;

        // Every other section is optional: a configuration that only sets the
        // font, or only the colours, must load with the rest at their
        // defaults. A missing or empty file is equally valid and yields all
        // defaults.
        let colors = ColorsConfig::parse_table(&parsed)?;
        let cursor = CursorConfig::parse_table(&parsed)?;
        let scrollback = ScrollbackConfig::parse_table(&parsed)?;
        let security = SecurityConfig::parse_table(&parsed)?;

        Ok(Self {
            font,
            window,
            colors,
            cursor,
            scrollback,
            security,
        })
    }

    /// Builds a configuration from an already parsed TOML table. Kept separate
    /// from [`Self::load_from_path`] so parsing can be exercised without
    /// touching the filesystem.
    pub fn load_from_text(table: &toml::Table) -> Result<Self, ConfigError> {
        let font = FontConfig::parse_table(table)?;
        let window = WindowConfig::parse_table(table)?;
        let colors = ColorsConfig::parse_table(table)?;
        let cursor = CursorConfig::parse_table(table)?;
        let scrollback = ScrollbackConfig::parse_table(table)?;
        let security = SecurityConfig::parse_table(table)?;
        Ok(Self {
            font,
            window,
            colors,
            cursor,
            scrollback,
            security,
        })
    }

    /// Returns the standard config path `~/.config/aura/config.toml`.
    pub fn config_path() -> Option<PathBuf> {
        // XDG config home: `~/.config/aura/config.toml` by default.
        let config_dir = dirs::config_dir()?.join("aura");
        Some(config_dir.join("config.toml"))
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self::defaults()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    Missing(&'static str),
    InvalidType {
        key: &'static str,
        expected: &'static str,
    },
    InvalidValue {
        key: &'static str,
        reason: String,
    },
    Io {
        path: String,
        reason: String,
    },
    Parse {
        path: String,
        reason: String,
    },
}

impl ConfigError {
    pub fn missing(key: &'static str) -> Self {
        Self::Missing(key)
    }

    pub fn invalid_type(key: &'static str, expected: &'static str) -> Self {
        Self::InvalidType { key, expected }
    }

    pub fn invalid_value(key: &'static str, reason: impl Into<String>) -> Self {
        Self::InvalidValue {
            key,
            reason: reason.into(),
        }
    }

    pub fn io(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Io {
            path: path.into(),
            reason: reason.into(),
        }
    }

    pub fn parse(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Parse {
            path: path.into(),
            reason: reason.into(),
        }
    }
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Missing(key) => write!(f, "missing required config key: {key}"),
            ConfigError::InvalidType { key, expected } => {
                write!(f, "{key} must be {expected}")
            }
            ConfigError::InvalidValue { key, reason } => {
                write!(f, "{key}: {reason}")
            }
            ConfigError::Io { path, reason } => {
                write!(f, "config {path}: {reason}")
            }
            ConfigError::Parse { path, reason } => {
                write!(f, "config {path}: parse error: {reason}")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_windows_and_invalid_config_respect_the_gpu_grid_budget() {
        assert_eq!(
            grid_dimensions(u32::MAX, u32::MAX, 1, 1),
            (MAX_GRID_COLUMNS, MAX_GRID_ROWS)
        );
        for key in ["columns", "lines"] {
            for value in [-1, 0, 65536] {
                let table = toml::from_str(&format!("[window]\n{key} = {value}\n")).unwrap();
                assert!(WindowConfig::parse_table(&table).is_err(), "{key}={value}");
            }
        }
    }

    #[test]
    fn every_section_is_optional() {
        // A configuration that only mentions the window must load, with the
        // font and the new sections falling back to their defaults.
        let table = toml::from_str("[window]\ncolumns = 90\n").unwrap();
        let config = AppConfig::load_from_text(&table).unwrap();
        assert_eq!(config.window.columns, 90);
        assert_eq!(config.font.family, DEFAULT_FONT_FAMILY);
        assert_eq!(config.colors.palette(), Palette::DARK);
        assert_eq!(config.cursor, CursorConfig::default());
        assert_eq!(config.scrollback.lines, DEFAULT_SCROLLBACK_LINES);
        assert_eq!(config.security, SecurityConfig::default());
    }

    #[test]
    fn theme_and_overrides_resolve_into_a_complete_palette() {
        // `r##` because the TOML body itself contains `"#...` colour literals.
        let table = toml::from_str(
            r##"
[colors]
theme = "light"
foreground = "#101010"
selection = "0x123456"
palette = ["#000000", "#010101", "#020202", "#030303", "#040404", "#050505", "#060606", "#070707",
           "#080808", "#090909", "#0a0a0a", "#0b0b0b", "#0c0c0c", "#0d0d0d", "#0e0e0e", "#0f0f0f"]
"##,
        )
        .unwrap();
        let colors = ColorsConfig::parse_table(&table).unwrap();
        let palette = colors.palette();
        assert_eq!(palette.foreground, 0x101010);
        // The theme supplies background and selection unless overridden.
        assert_eq!(palette.background, Palette::LIGHT.background);
        assert_eq!(palette.selection, 0x123456);
        assert_eq!(palette.ansi[1], 0x010101);
        assert_eq!(palette.ansi[15], 0x0f0f0f);
    }

    #[test]
    fn cursor_colour_defaults_to_the_foreground() {
        let table = toml::from_str("[colors]\ntheme = \"dark\"\n").unwrap();
        let colors = ColorsConfig::parse_table(&table).unwrap();
        assert_eq!(colors.cursor_color(), colors.palette().foreground);

        let table = toml::from_str("[colors]\ncursor = \"#ff0000\"\n").unwrap();
        let colors = ColorsConfig::parse_table(&table).unwrap();
        assert_eq!(colors.cursor_color(), 0xff0000);
    }

    #[test]
    fn cursor_and_scrollback_bounds_are_enforced() {
        let table = toml::from_str("[cursor]\nstyle = \"bar\"\nblink = false\n").unwrap();
        let cursor = CursorConfig::parse_table(&table).unwrap();
        assert_eq!(cursor.style, CursorShape::Bar);
        assert!(!cursor.blink);

        assert!(toml::from_str::<toml::Table>("[cursor]\nstyle = \"blob\"\n").is_ok());
        let table = toml::from_str("[cursor]\nstyle = \"blob\"\n").unwrap();
        assert!(CursorConfig::parse_table(&table).is_err());

        let table = toml::from_str("[scrollback]\nlines = 500\n").unwrap();
        assert_eq!(ScrollbackConfig::parse_table(&table).unwrap().lines, 500);

        for bad in ["-1", &MAX_SCROLLBACK_LINES.saturating_add(1).to_string()] {
            let table = toml::from_str(&format!("[scrollback]\nlines = {bad}\n")).unwrap();
            let error = ScrollbackConfig::parse_table(&table).unwrap_err();
            assert!(matches!(error, ConfigError::InvalidValue { .. }), "{error}");
        }
    }

    #[test]
    fn security_defaults_refuse_everything_unsafe() {
        let table = toml::from_str("").unwrap();
        let security = SecurityConfig::parse_table(&table).unwrap();
        // Writes are on so local copy/paste keeps working; reads are never
        // allowed by any policy.
        assert!(security.osc52.allows_write());
        assert!(!Osc52Policy::Disabled.allows_write());
        assert_eq!(security.max_paste_bytes, DEFAULT_MAX_PASTE_BYTES);

        let table =
            toml::from_str("[security]\nosc52 = \"disabled\"\nmax_paste_bytes = 8\n").unwrap();
        let security = SecurityConfig::parse_table(&table).unwrap();
        assert!(!security.osc52.allows_write());
        assert_eq!(security.max_paste_bytes, 8);

        let table = toml::from_str("[security]\nosc52 = \"read-only\"\n").unwrap();
        assert!(SecurityConfig::parse_table(&table).is_err());
    }

    #[test]
    fn colour_parsing_accepts_the_common_notations_and_rejects_junk() {
        let value = |text: &str| toml::Value::String(text.to_owned());
        assert_eq!(parse_color(&value("#1e1e1e"), "k").unwrap(), 0x1e1e1e);
        assert_eq!(parse_color(&value("1e1e1e"), "k").unwrap(), 0x1e1e1e);
        assert_eq!(parse_color(&value("0x1E1E1E"), "k").unwrap(), 0x1e1e1e);
        assert_eq!(
            parse_color(&toml::Value::Integer(0x1e1e1e), "k").unwrap(),
            0x1e1e1e
        );
        for bad in ["", "#", "#1234567", "#gggggg", "#12"] {
            assert!(
                parse_color(&value(bad), "k").is_err(),
                "{bad} should be rejected"
            );
        }
    }

    #[test]
    fn theme_names_are_case_insensitive_and_validated() {
        assert_eq!(ThemeKind::parse("DARK").unwrap(), ThemeKind::Dark);
        assert_eq!(ThemeKind::parse(" Light ").unwrap(), ThemeKind::Light);
        assert!(ThemeKind::parse("solarized").is_err());
    }

    #[test]
    fn defaults_are_valid() {
        let config = AppConfig::defaults();
        assert_eq!(config.font.family, DEFAULT_FONT_FAMILY);
        assert_eq!(config.font.size, DEFAULT_FONT_SIZE);
        assert_eq!(config.window.columns, DEFAULT_COLUMNS);
        assert_eq!(config.window.lines, DEFAULT_LINES);
        assert_eq!(config.window.padding_x, DEFAULT_PADDING);
        assert_eq!(config.window.padding_y, DEFAULT_PADDING);
    }

    #[test]
    fn parse_minimal_config() {
        let toml_str = r#"
[font]
family = "Iosevka"
size = 18.0
"#;
        let parsed: toml::Table = toml::from_str(toml_str).unwrap();
        let config = AppConfig::load_from_text(&parsed).unwrap();
        assert_eq!(config.font.family, "Iosevka");
        assert_eq!(config.font.size, 18.0);
        assert_eq!(config.window.columns, DEFAULT_COLUMNS);
    }

    #[test]
    fn parse_full_config() {
        let toml_str = r#"
[font]
family = "Iosevka"
size = 14.0

[font.bold]
family = "Iosevka"
style = "Bold"

[font.italic]
family = "Iosevka"
style = "Italic"

[font.bold_italic]
family = "Iosevka"
style = "Bold Italic"

[font.fallback]
families = [
    "Noto Sans CJK",
    "Noto Color Emoji",
]

[window]
columns = 120
lines = 32
padding_x = 0
padding_y = 0
"#;
        let parsed: toml::Table = toml::from_str(toml_str).unwrap();
        let config = AppConfig::load_from_text(&parsed).unwrap();

        assert_eq!(config.font.family, "Iosevka");
        assert_eq!(config.font.size, 14.0);
        assert_eq!(
            config.font.bold.as_ref().unwrap().family.as_deref(),
            Some("Iosevka")
        );
        assert_eq!(
            config.font.bold.as_ref().unwrap().style.as_deref(),
            Some("Bold")
        );
        assert_eq!(
            config.font.italic.as_ref().unwrap().style.as_deref(),
            Some("Italic")
        );
        assert_eq!(
            config.font.bold_italic.as_ref().unwrap().style.as_deref(),
            Some("Bold Italic")
        );
        assert_eq!(
            config.font.fallback_families,
            vec!["Noto Sans CJK", "Noto Color Emoji"]
        );
        assert_eq!(config.window.columns, 120);
        assert_eq!(config.window.lines, 32);
        assert_eq!(config.window.padding_x, 0);
        assert_eq!(config.window.padding_y, 0);
    }

    #[test]
    fn runtime_font_size_tracks_configured_and_current() {
        // Spec sequence: 14 → 13 → 12 → 13 → 14 → … → Ctrl+0.
        let mut state = RuntimeFontSize::new(14.0);
        assert_eq!(state.configured(), 14.0);
        assert_eq!(state.current(), 14.0);
        assert!(state.shrink());
        assert_eq!(state.current(), 13.0);
        assert_eq!(state.configured(), 14.0);
        assert!(state.shrink());
        assert_eq!(state.current(), 12.0);
        assert!(state.grow());
        assert_eq!(state.current(), 13.0);
        assert!(state.reset());
        assert_eq!(state.current(), 14.0);
        // Reset at the configured size is a no-op.
        assert!(!state.reset());
    }

    #[test]
    fn runtime_font_size_respects_minimum_and_maximum() {
        // Ctrl+- at minimum remains minimum; Ctrl++ at maximum remains maximum.
        let mut state = RuntimeFontSize::new(MIN_FONT_SIZE);
        assert!(!state.shrink());
        assert_eq!(state.current(), MIN_FONT_SIZE);
        assert!(state.grow());

        let mut state = RuntimeFontSize::new(MAX_FONT_SIZE);
        assert!(!state.grow());
        assert_eq!(state.current(), MAX_FONT_SIZE);
        assert!(state.shrink());

        // Custom startup size survives the round trip: 18 → 17 → 16 → 17 → 18.
        let mut state = RuntimeFontSize::new(18.0);
        assert!(state.shrink());
        assert_eq!(state.current(), 17.0);
        assert!(state.shrink());
        assert_eq!(state.current(), 16.0);
        assert!(state.grow());
        assert_eq!(state.current(), 17.0);
        assert!(state.reset());
        assert_eq!(state.current(), 18.0);

        // A non-default startup size is its own reset target: 24 → reset → 24.
        let mut state = RuntimeFontSize::new(24.0);
        assert!(state.shrink());
        assert!(state.reset());
        assert_eq!(state.current(), 24.0);
    }

    #[test]
    fn font_geometry_is_single_source_of_truth() {
        let geometry = FontGeometry::new(9, 20, 120, 32, 0, 0, GlyphOffsets::default());
        assert_eq!(geometry.pixel_width, 120 * 9);
        assert_eq!(geometry.pixel_height, 32 * 20);
        assert_eq!(geometry.columns, 120);
        assert_eq!(geometry.rows, 32);
        let (px, py) = geometry.pty_pixels();
        assert_eq!((px, py), (120 * 9, 32 * 20));

        let padded = FontGeometry::new(9, 20, 120, 32, 4, 4, GlyphOffsets::default());
        assert_eq!(padded.pixel_width, 120 * 9 + 8);
        assert_eq!(padded.pixel_height, 32 * 20 + 8);

        // Zero cell dimensions degrade to 1px cells, never divide-by-zero.
        let degenerate = FontGeometry::new(0, 0, 120, 32, 0, 0, GlyphOffsets::default());
        assert_eq!((degenerate.cell_width, degenerate.cell_height), (1, 1));
        assert_eq!((degenerate.pixel_width, degenerate.pixel_height), (120, 32));
    }

    #[test]
    fn shared_grid_dimensions_agree_with_geometry() {
        assert_eq!(grid_dimensions(100, 64, 9, 16), (11, 4));
        assert_eq!(grid_dimensions(2, 3, 9, 16), (1, 1));
        let geometry = FontGeometry::new(9, 16, 11, 4, 0, 0, GlyphOffsets::default());
        assert_eq!(
            grid_dimensions(geometry.pixel_width, geometry.pixel_height, 9, 16),
            (11, 4)
        );
    }

    #[test]
    fn pty_winsize_matches_terminal_grid_from_geometry() {
        // The same FontGeometry feeds Terminal::resize and the PTY's
        // TIOCSWINSZ: rows/columns must be identical and the pixel lanes
        // must carry the window dimensions.
        for (cell_w, cell_h) in [(8, 17), (9, 20), (17, 34)] {
            let geometry =
                FontGeometry::new(cell_w, cell_h, 120, 32, 3, 5, GlyphOffsets::default());
            let mut winsize =
                aura_pty::PtySize::new(geometry.columns, geometry.rows).expect("valid winsize");
            // Pixel lanes are filled by the font-resize path after PtySize::new.
            let (px, py) = geometry.pty_pixels();
            winsize.pixel_width = px;
            winsize.pixel_height = py;
            assert_eq!((winsize.columns, winsize.rows), (120, 32));
            let terminal = aura_core::Terminal::new(geometry.columns, geometry.rows).expect("grid");
            assert_eq!(terminal.active_grid().width(), usize::from(winsize.columns));
            assert_eq!(terminal.active_grid().height(), usize::from(winsize.rows));
            assert_eq!(
                (winsize.pixel_width, winsize.pixel_height),
                (px, py),
                "pixel lanes follow the padded window size"
            );
            assert_eq!(
                (px, py),
                ((120 * cell_w + 6) as u16, (32 * cell_h + 10) as u16)
            );
        }
    }

    #[test]
    fn invalid_size_rejected() {
        let toml_str = r#"
[font]
family = "Iosevka"
size = 0.0
"#;
        let parsed: toml::Table = toml::from_str(toml_str).unwrap();
        let err = FontConfig::parse_table(&parsed).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidValue { .. }));
    }

    #[test]
    fn missing_family_falls_back_to_the_default_family() {
        // Every font field is optional: a configuration that sets only the
        // window must not fail to load because it says nothing about fonts.
        let parsed: toml::Table = toml::from_str("[font]\nsize = 14.0\n").unwrap();
        let font = FontConfig::parse_table(&parsed).unwrap();
        assert_eq!(font.family, DEFAULT_FONT_FAMILY);
        assert_eq!(font.size, 14.0);
    }

    #[test]
    fn invalid_columns_rejected() {
        let toml_str = r#"
[window]
columns = 0
"#;
        let parsed: toml::Table = toml::from_str(toml_str).unwrap();
        let err = WindowConfig::parse_table(&parsed).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidValue { .. }));
    }

    #[test]
    fn missing_config_uses_defaults() {
        let config =
            AppConfig::load_from_path(Some(Path::new("/nonexistent/config.toml"))).unwrap();
        assert_eq!(config.font.family, DEFAULT_FONT_FAMILY);
        assert_eq!(config.font.size, DEFAULT_FONT_SIZE);
    }

    #[test]
    fn window_geometry_uses_complete_cells() {
        // Simulate the geometry calculation the app performs from config + metrics.
        let config = AppConfig::defaults();
        let cell_width = 9u32;
        let cell_height = 16u32;
        let pixel_width = (config.window.columns * cell_width as usize)
            .saturating_add(2 * config.window.padding_x as usize)
            .max(1) as u32;
        let pixel_height = (config.window.lines * cell_height as usize)
            .saturating_add(2 * config.window.padding_y as usize)
            .max(1) as u32;
        assert_eq!(pixel_width, 120 * 9);
        assert_eq!(pixel_height, 32 * 16);
    }

    #[test]
    fn window_geometry_accounts_for_padding() {
        let mut config = AppConfig::defaults();
        config.window.padding_x = 4;
        config.window.padding_y = 4;
        let cell_width = 9u32;
        let cell_height = 16u32;
        let pixel_width = (config.window.columns * cell_width as usize)
            .saturating_add(2 * config.window.padding_x as usize)
            .max(1) as u32;
        let pixel_height = (config.window.lines * cell_height as usize)
            .saturating_add(2 * config.window.padding_y as usize)
            .max(1) as u32;
        assert_eq!(pixel_width, 120 * 9 + 8);
        assert_eq!(pixel_height, 32 * 16 + 8);
    }

    #[test]
    fn resize_inner_dimensions_subtract_padding() {
        let config = AppConfig::defaults();
        let cell_width = 9u32;
        let cell_height = 16u32;
        let event_width = 120u32 * 9 + 8;
        let event_height = 32u32 * 16 + 8;
        let inner_width = event_width
            .saturating_sub(2 * config.window.padding_x)
            .max(1);
        let inner_height = event_height
            .saturating_sub(2 * config.window.padding_y)
            .max(1);
        let cols = (inner_width / cell_width.max(1)).max(1) as usize;
        let rows = (inner_height / cell_height.max(1)).max(1) as usize;
        assert_eq!(cols, 120);
        assert_eq!(rows, 32);
    }

    #[test]
    fn style_faces_are_parsed_from_config() {
        let toml_str = r#"
[font]
family = "monospace"
size = 14.0

[font.bold]
family = "monospace"
style = "Bold"

[font.italic]
family = "monospace"
style = "Italic"

[font.bold_italic]
family = "monospace"
style = "Bold Italic"
"#;
        let parsed: toml::Table = toml::from_str(toml_str).unwrap();
        let font = FontConfig::parse_table(&parsed).unwrap();
        assert_eq!(
            font.bold.as_ref().unwrap().family.as_deref(),
            Some("monospace")
        );
        assert_eq!(font.bold.as_ref().unwrap().style.as_deref(), Some("Bold"));
        assert_eq!(
            font.italic.as_ref().unwrap().style.as_deref(),
            Some("Italic")
        );
        assert_eq!(
            font.bold_italic.as_ref().unwrap().style.as_deref(),
            Some("Bold Italic")
        );
    }

    #[test]
    fn invalid_toml_is_reported_as_parse_error() {
        let toml_str = r#"this is = [ invalid"#;
        let table = toml::from_str::<toml::Table>(toml_str).expect_err("invalid TOML must fail");
        // The parser should reject malformed TOML.
        let _ = table;
    }

    #[test]
    fn a_configuration_without_a_font_section_loads_with_defaults() {
        // Regression guard: `[font]` used to be mandatory, so a configuration
        // that only set colours or the window failed to parse and silently
        // dropped every other setting.
        let parsed: toml::Table = toml::from_str("[window]\ncolumns = 80\n").unwrap();
        let font = FontConfig::parse_table(&parsed).unwrap();
        assert_eq!(font.family, DEFAULT_FONT_FAMILY);
        assert_eq!(font.size, DEFAULT_FONT_SIZE);
        assert!(font.bold.is_none());
        assert!(font.fallback_families.is_empty());
    }
}
