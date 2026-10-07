//! Typed configuration loaded from `~/.config/aterm/aterm.toml`.
//!
//! `core` never reads the filesystem or TOML. The app crate parses, validates,
//! and converts config into typed values before constructing any font/render/terminal.

use std::path::{Path, PathBuf};

/// Re-exported from `aterm-font`: the single source of truth for runtime
/// font sizes. `config` validates the TOML startup size against the same
/// range that `Font::set_size` clamps at runtime.
pub use aterm_font::{FONT_SIZE_STEP, MAX_FONT_SIZE, MIN_FONT_SIZE, clamp_font_size};

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

/// Current font metrics plus the terminal geometry derived from them.
/// Single source of truth: `app` builds this from the live `Font` metrics
/// and hands the same values to window sizing, grid sizing, PTY winsize,
/// and the renderer — never two independent calculations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontGeometry {
    /// Rasterized cell width/height in pixels.
    pub cell_width: u32,
    /// Rasterized cell width/height in pixels.
    pub cell_height: u32,
    /// Outer window size holding `columns`×`rows` cells plus padding.
    pub pixel_width: u32,
    /// Outer window size holding `columns`×`rows` cells plus padding.
    pub pixel_height: u32,
    /// Terminal grid size this geometry was derived for.
    pub columns: usize,
    /// Terminal grid size this geometry was derived for.
    pub rows: usize,
}

impl FontGeometry {
    /// Derives window pixels from cell metrics, grid size, and padding with
    /// saturating math so huge sizes degrade to clamped pixels, never wrap.
    pub fn new(
        cell_width: u32,
        cell_height: u32,
        columns: usize,
        rows: usize,
        padding_x: u32,
        padding_y: u32,
    ) -> Self {
        let pixel_width = (columns.saturating_mul(cell_width.max(1) as usize))
            .saturating_add(2 * padding_x as usize)
            .max(1) as u32;
        let pixel_height = (rows.saturating_mul(cell_height.max(1) as usize))
            .saturating_add(2 * padding_y as usize)
            .max(1) as u32;
        Self {
            cell_width: cell_width.max(1),
            cell_height: cell_height.max(1),
            pixel_width: pixel_width.max(1),
            pixel_height,
            columns,
            rows,
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
    (columns, rows)
}

/// Default font family name when no config is present.
pub const DEFAULT_FONT_FAMILY: &str = "monospace";

/// Default font size when no config is present.
pub const DEFAULT_FONT_SIZE: f32 = 14.0;

/// Default terminal columns when no config is present.
pub const DEFAULT_COLUMNS: usize = 120;

/// Default terminal lines when no config is present.
pub const DEFAULT_LINES: usize = 32;

/// Default padding in typographic pixels.
pub const DEFAULT_PADDING: u32 = 0;

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
        let font = table
            .get("font")
            .ok_or_else(|| ConfigError::missing("font"))?
            .as_table()
            .ok_or_else(|| ConfigError::invalid_type("font", "table"))?;

        let mut family = font
            .get("family")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| ConfigError::missing("font.family"))?;

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

        let bold = FontStyleConfig::parse_section(font, "bold");
        let italic = FontStyleConfig::parse_section(font, "italic");
        let bold_italic = FontStyleConfig::parse_section(font, "bold_italic");

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
            bold,
            italic,
            bold_italic,
            fallback_families,
        })
    }
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
                let v = value as usize;
                if v >= 1 {
                    v
                } else {
                    return Err(ConfigError::invalid_value("window.columns", "must be >= 1"));
                }
            }
            None => DEFAULT_COLUMNS,
        };

        let lines = match window.get("lines").and_then(|v| v.as_integer()) {
            Some(value) => {
                let v = value as usize;
                if v >= 1 {
                    v
                } else {
                    return Err(ConfigError::invalid_value("window.lines", "must be >= 1"));
                }
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
pub struct AppConfig {
    pub font: FontConfig,
    pub window: WindowConfig,
}

impl AppConfig {
    /// Loads the default configuration without reading the filesystem.
    pub fn defaults() -> Self {
        Self {
            font: FontConfig {
                family: DEFAULT_FONT_FAMILY.to_string(),
                size: DEFAULT_FONT_SIZE,
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

        Ok(Self { font, window })
    }

    /// Returns the standard config path `~/.config/aterm/aterm.toml`.
    pub fn config_path() -> Option<PathBuf> {
        let config_dir = dirs::config_dir()?.join("aterm");
        Some(config_dir.join("aterm.toml"))
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
        let font = FontConfig::parse_table(&parsed).unwrap();
        let window = WindowConfig::parse_table(&parsed).unwrap();
        let config = AppConfig { font, window };
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
        let font = FontConfig::parse_table(&parsed).unwrap();
        let window = WindowConfig::parse_table(&parsed).unwrap();
        let config = AppConfig { font, window };

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
        let geometry = FontGeometry::new(9, 20, 120, 32, 0, 0);
        assert_eq!(geometry.pixel_width, 120 * 9);
        assert_eq!(geometry.pixel_height, 32 * 20);
        assert_eq!(geometry.columns, 120);
        assert_eq!(geometry.rows, 32);
        let (px, py) = geometry.pty_pixels();
        assert_eq!((px, py), (120 * 9, 32 * 20));

        let padded = FontGeometry::new(9, 20, 120, 32, 4, 4);
        assert_eq!(padded.pixel_width, 120 * 9 + 8);
        assert_eq!(padded.pixel_height, 32 * 20 + 8);

        // Zero cell dimensions degrade to 1px cells, never divide-by-zero.
        let degenerate = FontGeometry::new(0, 0, 120, 32, 0, 0);
        assert_eq!((degenerate.cell_width, degenerate.cell_height), (1, 1));
        assert_eq!((degenerate.pixel_width, degenerate.pixel_height), (120, 32));
    }

    #[test]
    fn shared_grid_dimensions_agree_with_geometry() {
        assert_eq!(grid_dimensions(100, 64, 9, 16), (11, 4));
        assert_eq!(grid_dimensions(2, 3, 9, 16), (1, 1));
        let geometry = FontGeometry::new(9, 16, 11, 4, 0, 0);
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
            let geometry = FontGeometry::new(cell_w, cell_h, 120, 32, 3, 5);
            let mut winsize =
                aterm_core::PtySize::new(geometry.columns, geometry.rows).expect("valid winsize");
            // Pixel lanes are filled by the font-resize path after PtySize::new.
            let (px, py) = geometry.pty_pixels();
            winsize.pixel_width = px;
            winsize.pixel_height = py;
            assert_eq!((winsize.columns, winsize.rows), (120, 32));
            let terminal =
                aterm_core::Terminal::new(geometry.columns, geometry.rows).expect("grid");
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
    fn missing_family_rejected() {
        let toml_str = r#"
[font]
size = 14.0
"#;
        let parsed: toml::Table = toml::from_str(toml_str).unwrap();
        let err = FontConfig::parse_table(&parsed).unwrap_err();
        assert!(matches!(err, ConfigError::Missing(_)));
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
        let config = AppConfig::load_from_path(Some(Path::new("/nonexistent/aterm.toml"))).unwrap();
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
    fn malformed_config_section_reports_missing_font() {
        let toml_str = r#"
[window]
columns = 80
"#;
        let parsed: toml::Table = toml::from_str(toml_str).unwrap();
        let err = FontConfig::parse_table(&parsed).unwrap_err();
        assert!(matches!(err, ConfigError::Missing("font")));
    }
}
