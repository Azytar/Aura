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

- Workspace restructured into the AURA crate layout: `crates/aura-core`,
  `crates/aura-pty`, `crates/aura-text`, `crates/aura-render`,
  `crates/aura-config`, and `crates/aura-app`, with shaders under `shaders/`
  and the binary renamed to `aura`.
- `aura-pty` as an independent crate: the only place that calls `openpty`,
  `setsid`, `TIOCSCTTY`, and `TIOCSWINSZ`. `aura-core` no longer owns any
  file-descriptor logic, so terminal state has no OS dependencies left.
- `aura-config` as an independent crate: TOML discovery under
  `~/.config/aura/config.toml`, validation, and terminal geometry.

### Fixed

- Background push-constant layout drift: `background.frag` declared its
  push-constant block without `offset_x`/`offset_y`, so every field after
  `ascent` was read two `u32` slots earlier than the Rust `PushGrid` and the
  vertex stages wrote it. The fragment stage read `cursor_visible` from
  `offset_y` (always 0, so the cursor cell was never inverted), and treated
  the real cursor index as `selection_start`. Any cursor in cell 0 or 1
  painted that cell with the selection background colour, and the block
  cursor never appeared over blank cells.
- The offscreen GPU readback suite is no longer `#[ignore]`d. It now prints a
  skip reason when no Vulkan device is reachable, so `cargo test` really runs
  those assertions on capable hosts instead of silently hiding them.

### Changed

- Configuration moved from `~/.config/aterm/aterm.toml` to
  `~/.config/aura/config.toml`; the example file is `examples/aura.toml`.

## [0.1.0] - 2026-10-06

- Typed TOML configuration loaded once at startup from `~/.config/aura/config.toml`.
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
- Minimal CLI: `--help`, `--version`.
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
