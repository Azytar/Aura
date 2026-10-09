# AURA Terminal

A GPU-accelerated terminal emulator for Linux. It has its own VT parser, grid,
scrollback, Unicode handling, font shaping and rasterization, glyph atlas, and
a Vulkan renderer.

## Status

The terminal core, text stack, PTY supervision, and Vulkan renderer are
implemented and tested, including offscreen GPU readback assertions that run
as part of `cargo test` on machines with a usable Vulkan device.

See `CHANGELOG.md` for the validated scope and known limitations, and
`docs/ARCHITECTURE.md` for how the crates fit together.

## Requirements

- Linux with a Vulkan 1.3 device exposing `dynamicRendering` and
  `synchronization2` (for example Mesa RADV/ANV).
- A monospace font. Resolution order: explicit path, family name search in
  `/usr/share/fonts` and `/usr/local/share/fonts`, then well-known system
  paths (`DejaVuSansMono`, `LiberationMono`, `NotoSansMono`). CJK, emoji,
  and symbol fallback faces load on demand from standard Noto paths when
  present.
- Build tools: `cargo` (1.85+), `glslc` for shader compilation.

The terminal advertises `TERM=xterm-256color` with `COLORTERM=truecolor`. No
custom terminfo entry is required: the implemented set (SGR truecolor, SGR
mouse reporting, bracketed paste, alternate screen, DECSCUSR cursor styles)
is covered by `xterm-256color`.

## Build

```sh
cargo build --release
```

The `aura` binary lands at `target/release/aura`. Shaders are compiled at build
time and embedded in the binary; no runtime data files are needed.

## Run

```sh
./target/release/aura
aura --help
aura --version
aura doctor
```

Configuration is optional. When `~/.config/aura/config.toml` is absent, the
documented defaults are used and the terminal is fully functional. An example
configuration is provided in `examples/aura.toml`.

## Configuration

The TOML file is read once at startup; it is never hot-reloaded. Supported
keys:

- `font.family`, `font.size` (startup size, 6-72 px)
- `font.bold`, `font.italic`, `font.bold_italic` (optional per-style faces)
- `font.fallback.families` (ordered fallback family names)
- `window.columns`, `window.lines`, `window.padding_x`, `window.padding_y`

`font.size` is the configured startup size. `Ctrl+Plus` / `Ctrl+Minus`
change the size during the session (fresh rasterization, rebuilt atlas,
window and PTY geometry updated); `Ctrl+0` restores the configured startup
size. The TOML value itself never changes at runtime.

## Local install

```sh
install -Dm755 target/release/aura /usr/local/bin/aura
install -Dm644 aura.desktop /usr/share/applications/aura.desktop
```

## Key bindings

- `Ctrl+Plus` / `Ctrl+Minus` change the runtime font size by 1.0 px
  (clamped to 6-72 px); `Ctrl+0` restores the configured `font.size`.
- `Ctrl+Shift+C` copy selection, `Ctrl+Shift+V` paste.
- `Shift+PageUp` / `Shift+PageDown` scrollback, `Ctrl+Shift+Home` top,
  `Ctrl+Shift+End` bottom.

## Layout

- `crates/aura-core`: VT parser, terminal state, grid, scrollback, Unicode.
- `crates/aura-pty`: Linux pseudoterminal allocation and child supervision.
- `crates/aura-text`: font discovery, shaping, rasterization, glyph cache,
  and atlas management.
- `crates/aura-render`: terminal cell, glyph, and decoration drawing on top
  of the Vulkan backend.
- `crates/aura-config`: TOML configuration discovery, validation, and
  terminal geometry.
- `crates/aura-app`: window event loop, PTY pump, keyboard and mouse
  translation, clipboard.
- `shaders/`: GLSL 460 sources compiled by `crates/aura-render/build.rs`.

## Known limitations

- No coverage-guided fuzzing: only stable toolchains are available, so the
  `crates/aura-core/fuzz` target runs as plain libFuzzer mutation runs.
- Underline and strikethrough render as solid bars; hyperlink text has no
  distinct style yet.
- Font configuration is startup-only; live reload and DPI-aware scaling are
  not implemented.
