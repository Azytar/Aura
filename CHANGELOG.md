# Changelog

All notable changes to this project are documented in this file.

This project uses [Semantic Versioning 2.0.0](https://semver.org/): given
`MAJOR.MINOR.PATCH`, `PATCH` covers compatible bug fixes, `MINOR` covers
new backward-compatible functionality, and `MAJOR` covers breaking public
API or behavior changes. While the version is `0.y.z` the project is in
initial development and the public API is not considered stable.

Release rules going forward:

- `0.1.0` is the initial release.
- `0.1.x` receives compatible bug fixes only.
- `0.2.0` is the next step for compatible feature additions (for example a
  TOML configuration file or font configuration).
- `1.0.0` marks a stable public API.

A version represents a release, not a commit; versions are not incremented
per commit.

## [Unreleased]

### Added

- Typed TOML configuration loaded once at startup from `~/.config/aterm/aterm.toml`.
- Configurable font family and size with validation (6-72 px).
- Runtime font zoom: `Ctrl+Plus` / `Ctrl+Minus` adjust the live size by
  1.0 px (clamped to 6-72 px) with fresh rasterization, atlas rebuild, and
  window/PTY geometry updates; `Ctrl+0` restores the configured startup
  size. The TOML file remains startup-only and is never hot-reloaded.
- Centralized font-size limits shared by config validation and runtime
  clamping, plus a single-source `FontGeometry` for window, grid, PTY, and
  renderer dimensions.
- Size-aware glyph cache key so each font size rasterizes independently.
- Atlas invalidation on font geometry changes, preventing stale CPU/GPU metadata.
- Configurable terminal columns, lines, and padding.
- Fallback family names loaded on demand when the primary face lacks a glyph.
- Per-style face overrides parsed from config (bold, italic, bold-italic).
- Benchmarks for font metrics and glyph cache behavior across sizes.

## [0.1.0] - 2026-10-06

Initial technical release: a GPU terminal emulator with its own VT stack,
Vulkan 1.3 renderer, and X11 frontend, validated as described below.

### Added

- PTY integration with shell spawning and window-size propagation.
- Own VT parser (UTF-8, CSI, OSC, DCS) with bounded intermediate state.
- Terminal grid with scrollback, reflow on resize, wide cells, graphemes,
  and combining marks.
- Unicode shaping with script context and per-glyph rasterization.
- Font fallback faces loaded on demand (CJK, emoji, symbols) and separate
  monochrome (R8) and color (RGBA8) glyph atlases.
- Vulkan 1.3 renderer: swapchain presentation for X11 plus a deterministic
  offscreen readback target used by the test suite.
- X11 frontend: window, title, keyboard translation with keymap refresh,
  mouse reporting (1000/1002/1006, SGR encoding), local selection
  (click, double-click word, triple-click line, drag), clipboard
  copy/paste, and bracketed paste markers.
- OSC 0/2 window titles, OSC 8 hyperlinks, OSC 52 clipboard writes, OSC 133
  shell markers.
- Underline, strikethrough, inverse, and block/bar/underline cursor
  rendering driven by terminal state (including DECSCUSR styles).
- Minimal CLI: `--help`, `--version`, `--font PATH`, `--font-size PIXELS`.
- Benchmarks for parser, grid, scroll, reflow, glyph cache, shaping, and
  rasterization workloads; VT parser fuzz corpus and target.

### Fixed

- Glyph metadata table truncation on cache hits, which left later cells
  without sampled pixels.
- Extended SGR color parameters leaking the trailing component into the
  rendition (for example `38;2;r;g;0` resetting all attributes).
- Missing color-bitmap font source, which left color emoji unrasterized.
- X11 keyboard mapping going stale after layout switches; the mapping is
  now refetched on `MappingNotify`.

### Security

- Terminal input is treated as untrusted: OSC/DCS strings are length
  bounded, CSI parameters are fixed-count with saturating arithmetic,
  clipboard payloads are size bounded in both directions, hyperlink and
  shell-marker state is capacity bounded, and glyph atlas uploads are
  dimension and bounds validated before reaching the GPU.

## Known limitations

- Coverage-guided fuzzing is not available in this environment (stable
  toolchain only); plain libFuzzer mutation runs pass without crashes.
- Underline and strikethrough render as solid bars; hyperlink text has no
  distinct style yet.
- Forced window-manager tiling sizes and live keyboard-layout switching
  depend on the running session and are not asserted by automated tests.
