# Architecture

AURA is a workspace of small crates with one strict rule: a crate may only
depend on a crate below it in this diagram, and nothing depends on the window
system except the top two.

```
                  ┌──────────────┐
                  │  aura-app    │  window, event loop, clipboard, PTY pump
                  └───┬───┬───┬──┘
                      │   │   │
        ┌─────────────┘   │   └──────────────┐
        │                 │                  │
┌───────▼───────┐ ┌───────▼────────┐ ┌───────▼────────┐
│ aura-platform │ │  aura-render   │ │  aura-config   │
│ (X11 today)   │ │ cells+glyphs   │ │ TOML + geometry│
└───┬───────────┘ └───┬────────────┘ └───────┬────────┘
    │                 │                      │
    │         ┌───────▼────────┐             │
    │         │   aura-text    │◄────────────┘
    │         │ shaping/raster │
    │         └───────┬────────┘
    │                 │
┌───▼─────────────────▼──┐
│       aura-core        │  parser, grid, terminal state. No OS, no GPU, no I/O.
└───────────┬───────────┘
            │ (probe)
        ┌───▼────┐
        │aura-pty│  openpty, setsid, TIOCSCTTY, TIOCSWINSZ
        └────────┘
```

`aura-core` never calls `libc`, never touches the filesystem, and never knows
a font or a GPU exists. That is what makes the parser, grid, scrollback, and
reflow testable without a display, and it is why `aura-pty` exists as a
separate crate: every pseudoterminal syscall lives in one auditable place.

## Crates

### `aura-core`

VT state. Three modules:

- `parser` — an incremental VT/ANSI state machine. It owns no terminal and no
  I/O: `feed(bytes, emit)` accepts any chunking and produces the same event
  stream, which the fuzz target asserts directly. Sequence lengths are bounded
  (128 bytes per CSI, 4 KiB per string) and a malformed sequence is dropped,
  never allowed to stall the parser.
- `grid` — cells, wide-cell pairs, graphemes, hyperlinks, scrollback, dirty
  rows. Rows are stored in a ring, so scrolling rotates an index instead of
  copying rows. Resize reflows logical lines (a row knows whether it continues
  the previous one) and re-derives the cursor position from the reflow.
- `terminal` — cursor, modes, SGR, DEC private modes, OSC, selection. The
  grid holds rows; the terminal holds semantics.

`Terminal::process(&[u8]) -> Vec<u8>` is the whole input contract: bytes in,
replies out (for `DSR`-style queries). It never spawns anything and never
executes anything the child sends.

### `aura-pty`

Linux pseudoterminal ownership. The only place `openpty`, `setsid`,
`TIOCSCTTY`, and `TIOCSWINSZ` are called.

- The child becomes a session leader with the slave as its controlling
  terminal, so job control, `stty`, and window-size queries work.
- `pre_exec` runs only async-signal-safe syscalls before `exec`.
- The master is nonblocking and close-on-exec.
- `Drop` and `close` both reap the child: closing the master hangs the session
  up, and if the child is still alive its whole process group is killed. This
  is what keeps a closed window from leaving an orphaned shell.

### `aura-text`

Font discovery, shaping, rasterization, and the glyph atlas. Built on `rustybuzz`
for shaping and `swash` for rasterization.

- The glyph cache is keyed on face, size, glyph identity, bold, and italic, so
  each size rasterizes independently and a runtime font change cannot reuse a
  stale raster.
- The atlas is shelf-packed on the CPU: glyph bitmaps are uploaded to a Vulkan
  image by the renderer, but the packing decision and the atlas contents live
  here.
- Monochrome (R8) and colour (RGBA8, emoji) bitmaps use two atlases because
  they need different image formats.
- Fallback faces load on the first primary miss, so a plain session never pays
  for CJK or emoji.

### `aura-render`

Terminal drawing on top of Vulkan. It owns no window-system types beyond a raw
connection used to create a surface.

- Backgrounds, glyphs, and decorations are three separate passes: one
  fullscreen triangle for backgrounds, one instanced quad per cell for glyphs.
  One draw call per kind, not one per character.
- Cell state reaches the GPU as an SSBO of `GpuCell`; glyph rectangles are a
  second SSBO. Push constants carry the grid geometry.
- Vulkan 1.3 dynamic rendering and synchronization2 are requested explicitly;
  there is no compatibility path pretending they are optional.
- Frames are bounded at two in flight, each with its own command pool, fence,
  and mapped buffers.
- The offscreen renderer is the same code path without a surface, rendering
  into an image and reading it back. The GPU tests go through it, which is why
  they can assert on actual pixel values.

### `aura-config`

TOML discovery, validation, and geometry.

- Every section is optional. A configuration that only sets colours, or only
  the window, loads with everything else at its defaults. A missing file yields
  the defaults, with no warning, because AURA is fully usable with no file.
- `FontGeometry` is the single source of truth for window pixels, grid size,
  PTY winsize, and renderer size. Two independent calculations of "how big is a
  cell" is the bug this type exists to prevent.
- `Palette` is data only. It does not know about Vulkan, so the renderer keeps
  full control of its own texture encoding.

### `aura-app`

The binary. Owns the X11 connection, the epoll loop, the PTY pump, keyboard
and mouse translation, and the clipboard. This is where AURA is allowed to
depend on everything.

## Event loop

One `epoll` with three sources:

| Token | Source | Effect |
| --- | --- | --- |
| 1 | X11 connection | key, button, motion, configure, expose, selection, client message |
| 2 | PTY master | read to exhaustion, feed the terminal, write any replies back |
| 3 | `timerfd`, 500 ms | cursor blink phase change |

A frame is produced only when something is marked invalid: PTY output, a
resize, an Expose, a title change, selection changes, a font-size change, or a
cursor blink transition. Without one of those, the loop blocks in `epoll_wait`
and the GPU stays idle.

The window is mapped only after the PTY, terminal, renderer, and first frame
exist, so the window can never accept a keystroke it has no child to deliver.

## Threading

There are no worker threads. One thread runs the event loop, the parser, and
the renderer, which is why no allocation in the frame path needs to be shared
or synchronised.

## Invariants worth preserving

1. `aura-core` has no `libc`, no file I/O, and no GPU dependency.
2. A chunk boundary never changes the event stream the parser produces. The
   fuzz target asserts this over random input.
3. A wide cell's two halves are created and cleared together; no operation can
   leave a spacer without its leading cell.
4. Colour words stored in cells stay opaque: the grid never learns what a
   font or a GPU glyph is.
5. The scalar values in a cell come from the PTY, not from AURA, and are never
   interpreted as a command.
6. Every `unsafe` block carries a `SAFETY` comment, and
   `unsafe_op_in_unsafe_fn` is denied workspace-wide.
7. The renderer's push-constant block is declared identically in all three
   shader stages and mirrored by a `size_of`/`offset_of` test in Rust. Layout
   drift between stages has already caused one bug; do not let it happen
   again.

## What is not here

- No worker threads, and no async runtime.
- No dynamic linking against the font or GUI stack beyond what the system
  already provides; Vulkan is loaded at runtime.
- No telemetry, no network access, no external services.
- No terminal multiplexer, tabs, or splits. AURA is one terminal.
