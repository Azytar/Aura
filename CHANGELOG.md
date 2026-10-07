# Changelog

## 0.1.0 — technical release candidate

### Validated

- VT parser, grid, scrollback with reflow, Unicode graphemes and wide cells,
  PTY integration, 256-color and truecolor SGR, alternate screen.
- Interactive compatibility: bash (editing, history, completion), vim,
  neovim, tmux (including nested vim), htop, less.
- Mouse reporting (1000/1002/1006, SGR encoding), local selection
  (click, double-click word, triple-click line, drag), clipboard
  copy/paste, bracketed paste with `200~`/`201~` markers.
- OSC 0/2 titles, OSC 8 hyperlinks (state and cell association), OSC 52
  clipboard writes with bounded payloads, OSC 133 shell markers.
- Font shaping with script context, glyph cache, monochrome and color
  (RGBA) atlases, CJK/emoji/symbol fallback faces loaded on demand.
- Underline, strikethrough, and block/bar/underline cursor rendering.
- Vulkan 1.3 offscreen readback tests (backgrounds, glyphs, decorations,
  cursor shapes, truecolor, color emoji) passing in debug and release.
- Real X11 presentation on Mesa RADV with the full interactive matrix above.
- Heavy output (20k–30k lines) completes without corruption; idle renders
  only on cursor blink or real activity.

### Fixed with regression tests

- Glyph metadata table truncation on cache hits.
- Extended SGR color components leaking into rendition flags.
- Keyboard mapping going stale after layout switches (refetch on
  `MappingNotify`).

### Known limitations

- Coverage-guided fuzzing is not available (stable toolchain only);
  10k-iteration libFuzzer runs without instrumentation pass cleanly.
- Underline/strikethrough are solid bars; hyperlinks have no distinct style.
- External window-manager behavior (forced tiling sizes) is environment
  dependent and not asserted by tests.
