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
aura --doctor
```

Configuration is optional. When `~/.config/aura/config.toml` is absent, the
documented defaults are used and the terminal is fully functional. An example
configuration is provided in `example/config.toml`.

## Configuration

Every section is optional. Unknown keys, invalid types and out-of-range values
are errors. Fonts, colors, padding, cursor behavior, bindings and other runtime
settings reload when the file or an import changes; an invalid edit keeps the
active configuration. Ctrl+Shift+R retries a reload.

Use `aura --print-config` for the documented example, `--config-file PATH` to
select a file, and `--check-config` to validate it without opening X11. The
[configuration guide](docs/CONFIGURATION.md) covers imports, custom bindings,
shell arguments, child environment and reload behavior. `example/config.toml`
lists every setting and its default. OSC 52 reads are never answered.

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
- `Ctrl+Shift+R` reload configuration; `[keyboard.bindings]` customizes shortcuts.
- Local mouse selection: click and drag, double click for a word, triple
  click for a line; dragging past an edge scrolls the view. Releasing a
  selection publishes it as the X11 PRIMARY selection; `Shift+Insert` or a
  middle click pastes that selection.
- `Shift+PageUp` / `Shift+PageDown` scrollback, `Ctrl+Shift+Home` top,
  `Ctrl+Shift+End` bottom.

Mouse-aware applications can request hover motion, focus changes and pixel
coordinates. Shift gives local selection back to Aura. Enhanced keyboard
sequences are negotiated by the application; ordinary shell input keeps its
legacy encoding. The pointer hides while typing and returns on motion;
`[mouse]` and `[cursor]` customize that behavior and cursor timing.

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
  `aura --doctor` reports which displays are visible.
- **No IME.** Composing text through an input method is not implemented, so
  CJK input relies on the input method's own commit path reaching the X
  keyboard events.
- **No input-to-present latency instrumentation.** Opt-in host stage timings
  do not establish when an image reaches the screen.
- **No subpixel antialiasing.** Hyperlink text has no distinct automatic style.
- **No fontconfig discovery.** Fonts are found in XDG/user and standard system
  directories; fonts outside them need an explicit path. Fontconfig aliases and
  rules are not evaluated.
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
