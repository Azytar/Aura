# AURA Terminal

A GPU-accelerated terminal emulator for Linux. It has its own VT parser, grid,
scrollback, Unicode handling, font shaping and rasterization, glyph atlas, and
a Vulkan renderer.

## Status

Version 0.1.0: the terminal core, text stack, PTY supervision, and Vulkan
renderer are implemented and tested, including offscreen GPU readback
assertions that run as part of `cargo test` on machines with a usable Vulkan
device.

See `CHANGELOG.md` for the validated scope and known limitations,
`docs/ARCHITECTURE.md` for how the crates fit together, and
`docs/PERFORMANCE.md` for measured numbers.

## Requirements

- Linux with a Vulkan 1.3 device exposing `dynamicRendering` and
  `synchronization2` (for example Mesa RADV/ANV).
- A monospace font. Resolution order: explicit path, family name search in
  `/usr/share/fonts` and `/usr/local/share/fonts`, then well-known system
  paths (`DejaVuSansMono`, `LiberationMono`, `NotoSansMono`). CJK, emoji,
  and symbol fallback faces load on demand from standard Noto paths when
  present.
- X11 with XKB; development libraries `libxkbcommon`, `libxkbcommon-x11`,
  and `libxcb-xkb` are required to link the keyboard backend.
- Build tools: `cargo` (1.88+), `glslc` for shader compilation.

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
configuration is provided in `example/config.toml`.

## Configuration

The TOML file is read once at startup; it is never hot-reloaded. Every
section is optional, so a file that only sets the colours, or only the window,
loads with everything else at its defaults. Unknown keys, invalid types and
out-of-range values are reported at startup instead of silently loading defaults.
Supported keys:

- `font.family`, `font.size` (startup size, 6-72 px)
- `font.bold`, `font.italic`, `font.bold_italic` (optional per-style faces)
- `font.fallback.families` (ordered fallback family names)
- `font.offset_x`, `font.offset_y`, `font.line_spacing`, `font.letter_spacing`
- `window.columns`, `window.lines`, `window.padding_x`, `window.padding_y`
- `colors.theme` (`dark`, `light`, or `custom`) with optional
  `foreground`, `background`, `cursor`, `selection`, and a 16-entry `palette`.
  Colours accept `#rrggbb`, `rrggbb`, `0xrrggbb`, `#rgb`, or an integer.
- `cursor.style` (`block`, `underline`, `bar`) and `cursor.blink`
- `scrollback.lines` (0 disables history, up to 1000000)
- `security.osc52` (`write-only` or `disabled`) and `security.max_paste_bytes`
  (1-1048564 bytes; oversized pastes are rejected whole).

`example/config.toml` documents every key with its default. AURA never answers
an OSC 52 *read* request, so the clipboard cannot be extracted by something
running inside the terminal.

`font.size` is the configured startup size. `Ctrl+Plus` / `Ctrl+Minus`
change the size during the session (fresh rasterization, rebuilt atlas,
grid and PTY dimensions updated); `Ctrl+0` restores the configured startup
size. Zoom keeps the outer window size fixed and recalculates the visible rows
and columns. The TOML value itself never changes at runtime.

Braille, box drawing and block elements are drawn from cell geometry, so plots
and TUI borders remain aligned across font changes and configured spacing.
Real style faces and complete-grapheme fallback retain the primary grid metrics.
See [Cell graphics and font routing](docs/CELL_GRAPHICS.md) for behavior,
regression coverage and limits.

## Local install

```sh
install -Dm755 target/release/aura /usr/local/bin/aura
install -Dm644 aura.desktop /usr/share/applications/aura.desktop
```

## Key bindings

- `Ctrl+Plus` / `Ctrl+Minus` change the runtime font size by 1.0 px
  (clamped to 6-72 px); `Ctrl+0` restores the configured `font.size`.
- `Ctrl+Shift+C` copy selection, `Ctrl+Shift+V` paste.
- Local mouse selection: click and drag, double click for a word, triple
  click for a line; dragging past an edge scrolls the view. Releasing a
  selection publishes it as the X11 PRIMARY selection; `Shift+Insert` or a
  middle click pastes that selection.
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

## Testing

```sh
cargo test --locked   # Includes GPU readback assertions
./tests/e2e_shell_smoke.sh target/release/aura
# Dedicated X11 display: checks resize/zoom geometry and pixel round trips.
python3 tests/x11_resize.py target/release/aura
```

`cargo test` runs the GPU readback suite as part of the normal run. It prints a
skip reason on a machine without a usable Vulkan device rather than being
`#[ignore]`d, because a skipped assertion is how the push-constant layout bug
in `docs/CHANGELOG` went unnoticed.

`tests/e2e_shell_smoke.sh` drives the real binary on a live X display and
checks the milestone end to end: typed keys reach `/bin/bash`, output comes back
through the parser into the Vulkan framebuffer, and closing the window leaves
no orphaned child. It exits 77 and explains itself when there is no display.

Its input path is `xdotool type`, which synthesizes characters by rebinding
scratch keycodes and restoring them afterwards. That restore can deliver
`MappingNotify` ahead of the synthetic `KeyPress` events, so occasionally the
character bash receives is not the one intended. AURA's `MappingNotify`
handling was checked separately against a real layout switch (`setxkbmap`
`us`<->`de`, five consecutive sends, all correct), and the script re-sends with
a `Ctrl+C` when a skew is detected.

## Known limitations

- **X11 only.** The window backend is X11; there is no Wayland frontend yet.
  `aura doctor` reports which displays are visible.
- **No IME.** Composing text through an input method is not implemented, so
  CJK input relies on the input method's own commit path reaching the X
  keyboard events.
- **No configurable key bindings.** The shortcuts are fixed in
  `crates/aura-app/src/main.rs`; `[keyboard]` does not exist yet.
- **No input-to-present latency instrumentation.** Only aggregate frame timing
  is reported on stderr.
- **No subpixel antialiasing**, and underline and strikethrough render as solid
  bars. Hyperlink text has no distinct style.
- **No fontconfig discovery.** Fonts are found by scanning standard
  directories, so a font outside them needs an explicit path.
- Font configuration is startup-only; live reload is not implemented.
- OSC 8 hyperlinks are parsed and stored, and OSC 52 writes are honoured, but
  nothing opens a link on click.

## Daily use audit

See [the daily use audit](docs/DAILY_USE_AUDIT.md) for keyboard, ncurses,
rendering and PTY fixes, regression coverage and remaining validation limits.
The X11 regression suite changes the keyboard map; run it on a dedicated
virtual display:

```sh
python3 tests/x11_daily_use.py target/release/aura
```

See [the reproducible test campaign](docs/TEST_CAMPAIGN.md) for seeded
properties, concrete replay, braille enumeration, GPU checks and instrumented
nightly fuzzing, including measured budgets and validation limits.

## Optional diagnostics

Diagnostics are silent by default. `AURA_DEBUG_KEYBOARD=1` enables map, reload
and key-event traces only in debug builds; release builds omit those traces.
`AURA_PERF=1` independently enables bounded host timing reports in debug or
release. Both variables accept only `0` (off) or `1` (on), with invalid values
rejected before X11 or PTY startup. Keyboard traces may include key identities;
performance reports include neither keys nor PTY contents.

Performance reports appear about every five seconds while the event loop
acts. Each stage retains its latest 256 samples: `n` counts all interval
samples, `retained` counts stored samples, p50/p95/p99 use nearest rank, and
`max` covers the entire interval. Unreached stages produce no samples.
`render_calls/s` counts calls, not confirmed presented frames. These are host
intervals without extra GPU waits; `present_call` returning does not establish
when the image reached the screen.
