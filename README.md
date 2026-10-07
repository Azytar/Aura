# aterm

A GPU terminal emulator with its own VT parser, grid, scrollback, shaper
integration, glyph atlas, and Vulkan 1.3 renderer with an X11 frontend.

## Status

Technical release candidate: core, renderer, and application are tested,
including real X11 presentation and offscreen GPU readback validation.
See `CHANGELOG.md` for the validated scope and known limitations.

## Requirements

- Linux with X11 (`DISPLAY` set) and a Vulkan 1.3 device with
  `dynamicRendering` and `synchronization2` (e.g. Mesa RADV/ANV).
- A monospace font. Resolution order: `--font PATH`, then well-known system
  paths (`DejaVuSansMono`, `LiberationMono`, `NotoSansMono`). CJK, emoji,
  and symbol fallback faces load on demand from standard Noto paths when
  present; otherwise those glyphs render as blank cells.
- Build tools: `cargo` (1.85+), `glslc` for shader compilation.

The terminal advertises `TERM=xterm-256color` with `COLORTERM=truecolor`.
No custom terminfo entry is required: the implemented set (SGR truecolor,
SGR mouse reporting, bracketed paste, alternate screen, DECSCUSR cursor
styles) is covered by `xterm-256color` and validated against
bash, vim, neovim, tmux, htop, and less.

## Build

```sh
cargo build --release
```

The `aterm` binary lands at `target/release/aterm` (about 3.5 MB,
dynamically linked against `libxcb`, `libc`, `libm`; Vulkan is loaded at
runtime with `dlopen`). Shaders are compiled at build time and embedded in
the binary; no runtime data files are needed.

## Run

```sh
./target/release/aterm [--font PATH] [--font-size PIXELS]
aterm --help
aterm --version
```

## Local install

```sh
install -Dm755 target/release/aterm /usr/local/bin/aterm
install -Dm644 aterm.desktop /usr/share/applications/aterm.desktop
```

## Key bindings

- `Ctrl+Shift+C` copy selection, `Ctrl+Shift+V` paste.
- `Shift+PageUp` / `Shift+PageDown` scrollback, `Ctrl+Shift+Home` top,
  `Ctrl+Shift+End` bottom.
- Mouse clicks report to applications that enable tracking (1000/1002/1006);
  otherwise they drive local selection (click, double-click word,
  triple-click line, drag).

## Layout

- `core`: VT parser, terminal state, grid, scrollback, Unicode, PTY.
- `font`: shaping (rustybuzz), rasterization (swash), glyph cache with
  primary plus on-demand fallback faces, monochrome and color atlases.
- `render`: Vulkan 1.3 renderer (swapchain presentation plus deterministic
  offscreen readback used by tests).
- `app`: X11 event loop, PTY pump, keyboard/mouse translation, clipboard.

## Known limitations

- No coverage-guided fuzzing: only stable toolchains are available, so the
  `core/fuzz` target runs as plain libFuzzer mutation runs (10k, no crashes).
- Underline and strikethrough render as solid bars; hyperlink text has no
  distinct style yet.
- No font configuration file: primary via `--font`, fallbacks auto-detected.
