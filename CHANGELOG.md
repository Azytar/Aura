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

### Fixed

- Configured cursor colors now reach every cursor shape. Focus loss shows a
  steady hollow cursor; focus and typing restart the configured blink interval.

- Configuration now supports validated imports, atomic live reload, custom
  bindings, shell arguments and child environment. Cursor timing, pointer
  policy and window identity have effective settings; standalone validation
  and a complete editable example expose errors before opening a window.

- Negotiated Kitty keyboard flags and modifyOtherKeys distinguish modified
  Enter and other shortcuts while preserving legacy text and Shift-only
  punctuation. Keyboard stacks are bounded and isolated per screen; repeat
  and release reports do not leak locally consumed shortcuts.

- Synchronized output batches (2026) defer presentation with a bounded host
  recovery timeout. Color, geometry, mode and terminal queries receive ordered
  replies; unknown modes and termcap names report unsupported capabilities.

- Mouse-aware applications receive button-free motion (1003), focus events
  (1004), SGR pixel coordinates (1016) and normal press-mode releases. Shift
  retains local selection. The X11 pointer changes with application mouse
  tracking, hides while typing and returns on motion.

- Unicode sextant blocks (U+1FB00–U+1FB3B), used by terminal logos such
  as Kilo Code, now fill their cell thirds directly instead of depending on
  font fallback. Adjacent filled thirds have no fractional-pixel seams.

- Keyboard traces are opt-in and compiled only in debug builds. Host timing
  reports are independently opt-in, retain bounded stage samples and omit
  stages not reached; ordinary rendering no longer samples frame clocks.
- Native rendering comparisons wait for presented witness pixels and actual
  PTY zoom/reset geometry rather than treating OSC titles as frame completion.

- Reflow walks borrowed cells one logical line at a time, reusing only row
  lengths as scratch storage even for a single very long wrapped paragraph.
  Unicode graphemes, wide links, blank lines and viewport/cursor mapping
  retain the previous resize behavior.

- Empty row metadata no longer reserves full-width grapheme/link tables.
  Cell shifts move owned graphemes, and fitting independent rows retain
  their cell buffers when only the terminal height changes.

- X11 window resizing preserves the previous frame in the overlapping area
  until Vulkan presents the replacement. Newly exposed strips use the
  configured background instead of flashing black.
- Resize and font zoom avoid deep copies of scrollback during reflow. Width
  changes reuse independent rows that still fit, preserving their graphemes,
  hyperlinks and viewport without rebuilding the complete history.
- Fonts missing common Return arrows such as `↵` can use an available
  DejaVu symbol fallback instead of displaying the primary missing-glyph box.
  Enter/Return marks and emoji presentation now have explicit pixel coverage.

- Combining marks arriving after a main-screen resize stay attached to the
  reflowed base cell instead of using its old coordinates.
- Insert-mode cell shifts retain hyperlink ownership on valid wide-cell
  continuation halves.
- Add bounded, seeded terminal/text properties, concrete failure replay,
  braille enumeration and independent GPU/native rendering checks, with a
  reproducible campaign report in `docs/TEST_CAMPAIGN.md`.

- Runtime font zoom keeps the window extent fixed, recalculates the grid and
  PTY dimensions, and rebuilds the font atlas once per input batch. Resize
  bursts reconcile the final geometry rather than every queued intermediate.
- X11 base/minimum size hints account for padding on both sides.
- Main-screen resize preserves the insertion point of full lines, avoids
  scrolling away prompts when unused rows shrink, retains the logical
  scrollback viewport anchor, and omits structural padding before wrapped
  wide glyphs. Alternate-screen content keeps its coordinates without reflow;
  leaving it restores the reflowed main-screen cursor.
- Integer font sizes are honoured. Unknown configuration keys, invalid types,
  unreadable files and out-of-range integers now produce startup errors rather
  than silently falling back to defaults or wrapping into valid values.
- `security.max_paste_bytes` is applied to incoming X11 pastes, including
  limits above the old fixed 64 KiB cap. Oversized pastes are rejected whole
  and their temporary properties are discarded. The upper bound leaves room
  for bracketed-paste markers in the input queue.
- The example configuration uses the portable default monospace family and
  keeps font offset/spacing options in the correct TOML section.

- Aborted compose sequences no longer swallow the cancelling key. A dead
  key followed by a character with no composed form (for example
  dead-acute then `c`) previously returned no input at all, losing a
  keystroke on every aborted sequence, including during password entry.
  The cancelling key is now encoded like any other key.
- C1 single-byte controls `IND` (0x84), `NEL` (0x85) and `HTS` (0x88) are
  now honoured exactly like their `ESC D`, `ESC E` and `ESC H` spellings
  instead of being silently dropped.
- Scrollback shortcuts (`Shift+PgUp`/`Shift+PgDn`,
  `Ctrl+Shift+Home`/`Ctrl+Shift+End`) no longer swallow keys on the
  alternate screen. They now reach the fullscreen application as modified
  navigation keys; previously `vim`, `less` and similar programs never
  received them.
- Mouse drags no longer extend the local selection while mouse tracking
  is active. Press-mode tracking used to highlight locally at the same
  time as the application received the press, naming different lines.
- PTY writes yield after a bounded number of attempts, including repeated
  interruptions and tiny partial writes, so pending input cannot starve X11.
  `WouldBlock` retains the unwritten suffix for the next writable wake;
  `WriteZero` and `BrokenPipe` propagate as errors without retry loops or
  removing unaccepted bytes. A zero write is no longer mistaken for
  transient backpressure.
- Interrupted PTY reads are retried instead of aborting the event loop.
- A cancelling dead key can begin a fresh Compose sequence; for example,
  dead-acute then dead-grave then `a` now produces `à` with the matching
  Compose rules instead of dropping the second accent.
- C1 `RI` (0x8d) now scrolls like `ESC M`. C1 controls and sequence
  introducers interrupt unfinished ESC/CSI sequences instead of being lost.
  UTF-8 continuation bytes and control-string payloads keep their lexical
  interpretation.
- A wide glyph ending at the right margin leaves the cursor on the final
  occupied column. Cursor reports and backspace now agree with the cell
  pair, including when replacing it with narrow text.
- Leaving the alternate screen clears its selection instead of highlighting
  unrelated main-screen text. Stale local drags and autoscroll stop when
  screen changes or mouse tracking invalidate their selection.
- A hard reset returns the viewport to the cleared live screen instead of
  continuing to display old scrollback.

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

### Added

- Colour theming: `colors.theme` selects a complete `dark` or `light`
  palette, or `custom` to build one from overrides. `foreground`,
  `background`, `cursor`, `selection`, and a 16-entry ANSI `palette` are
  accepted; colours may be `#rrggbb`, `rrggbb`, `0xrrggbb`, `#rgb`, or an
  integer. The palette is passed to the renderer instead of being hardcoded,
  so a theme takes effect without re-rasterizing anything.
- `cursor.style` and `cursor.blink` seed the terminal's cursor before any
  application overrides them with DECSCUSR.
- `scrollback.lines` configures how much history is retained, trimming any
  existing history immediately when the limit drops.
- `security.osc52` chooses between honouring OSC 52 writes (`write-only`,
  the default) and ignoring OSC 52 entirely (`disabled`); plus
  `security.max_paste_bytes`, which bounds a single paste before it is
  forwarded to the child.
- Every configuration section is now optional. `[font]` used to be
  mandatory, so a configuration that only set colours or the window failed
  to parse and silently lost all of its other settings.
- `Terminal::set_modes` and `Terminal::set_scrollback_limit` on the core, so
  configuration can seed terminal state without reaching into private fields.
- Selection details: a double or triple click followed by a drag keeps
  extending by whole words or lines, dragging past an edge scrolls the
  view on a repeating tick while the pointer stays outside (faster the
  further out it is), releasing the button publishes the selection as the
  X11 PRIMARY selection, and `Shift+Insert` or a middle click pastes it
  back. A keystroke clears the local selection instead of leaving a stale
  highlight.

### Fixed

- The selection highlight rendered brown instead of blue. `0x00406090` was
  written into the shader as if it were already in the shader's packed BGR
  channel order, so it came out as rgb(144,96,64). The selection colour now
  travels with the palette as a push constant and is packed on the CPU, like
  every other colour.
- The offscreen renderer ignored the terminal's selection entirely (it
  always passed `u32::MAX`), so the swapchain path's selection behaviour was
  never exercised by tests. The offscreen target now mirrors it.
- Selections were anchored to viewport rows, so scrolling after selecting
  made the highlight and the copied text name different lines. Selections
  now carry absolute buffer line numbers: they follow their content while
  the viewport scrolls, clamp when history is trimmed, and are dropped on
  reflow because a resize renumbers lines.

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
