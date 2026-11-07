# Terminal implementation references

Reviewed on 2026-11-06. These are primary source repositories and protocol
specifications, not third-party performance rankings. Their design choices are
not evidence that the same optimization will help Aura on a physical GPU.

## Existing terminals

| Project | Primary reference | Useful lesson for Aura |
| --- | --- | --- |
| Alacritty | [event.rs](https://github.com/alacritty/alacritty/blob/master/alacritty/src/event.rs) and [window_context.rs](https://github.com/alacritty/alacritty/blob/master/alacritty/src/window_context.rs) | A frame event can bypass normal batching for input responsiveness, while damage/dirty state prevents unnecessary redraw requests. Aura should keep PTY/input service independent from render-call throttling. |
| xterm | [XTerm Control Sequences](https://www.invisible-island.net/xterm/ctlseqs/ctlseqs.html) | A behavioral reference for escape parsing, OSC/CSI state, and error recovery. It is not a GPU performance blueprint. |
| Ghostty | [architecture](https://ghostty.org/docs/about) and [GPU features](https://ghostty.org/docs/features) | A shared terminal core behind native macOS (Swift/AppKit) and Linux (GTK) frontends; Metal on macOS and OpenGL on Linux. A reference for **separating host integration from core semantics**. |
| iTerm2 | [open-source macOS implementation](https://github.com/gnachman/iTerm2) | A substantial macOS-only terminal whose implementation includes a Metal-based renderer. This is independent of Apple's built-in Terminal.app; do not confuse the two projects. |

### Unicode throughput reference details

- Alacritty's [PTY loop](https://github.com/alacritty/alacritty/blob/master/alacritty_terminal/src/event_loop.rs)
  reads and parses in a separate thread with a 1 MiB pending-buffer bound and
  at most 65,535 bytes processed per terminal lock. That architecture may
  reduce rendering-related backpressure, but it costs synchronization and does
  not by itself explain native throughput. Aura currently has one host loop;
  measure its parse-versus-PTY intervals before changing ownership.
- Alacritty's [VTE parser](https://docs.rs/vte/latest/src/vte/lib.rs.html)
  uses `memchr` to find escape boundaries rapidly, with UTF-8 dispatch over
  the intervening text. Aura already batches ordinary text spans, so replacing
  the parser blindly would discard its existing fast path and risk malformed
  UTF-8 or split-sequence regressions.
- Alacritty's [cell extras](https://github.com/alacritty/alacritty/blob/master/alacritty_terminal/src/term/cell.rs)
  retain combining characters in a bounded nine-character `ArrayVec`, allocated
  only when extra cell attributes are needed. The first extra allocation is
  **not** free; inline capacity avoids further per-character allocations.
- xterm's [manual](https://www.invisible-island.net/xterm/manpage/xterm.html)
  documents `combiningChars` (default 2, 0–5) and `precompose` (default on).
  The former reserves combining storage, while the latter can fold a base and
  accent into an NFC character. Aura should retain its original Unicode text
  for selection and clipboard rather than apply unconditional normalization.
- xterm's [author notes](https://www.invisible-island.net/xterm/xterm-paste64.html)
  document skipping zero-width checks for Western codepoints below U+0300.
  Its [FAQ](https://www.invisible-island.net/xterm/xterm.faq.html) notes that
  `XCopyArea` scrolling can be slower on some Xorg configurations. Neither is
  evidence that direct X11 surface copies would speed Aura's Vulkan backend.

For Aura's current surface renderer, the first optimization is to retain static
background coverage by dirty row. Cursor blink and selection-only changes must
not redo work for unchanged cells. A full-scanning implementation remains as a
reference in the offscreen path and differential tests. See
[Performance](PERFORMANCE.md) for the CPU microbenchmark and its limits.

## Wayland is a distinct host backend

Native Wayland support should share Aura's parser, grid, shaping and Vulkan
cell rendering, but implement window/event/clipboard integration separately
from XCB. The relevant protocol contracts are:

- [xdg-shell](https://wayland.app/protocols/xdg-shell): toplevel surfaces,
  configure acknowledgements, fullscreen and resize semantics.
- [Wayland data devices](https://wayland.freedesktop.org/docs/book/Protocol.html):
  focus-scoped clipboard selection and transfer through data offers/sources,
  **not** X11 atoms or `INCR`. Preserve the large-copy regression coverage.
- [presentation-time](https://wayland.app/protocols/presentation-time):
  per-commit presentation feedback and discarded-frame reporting. Measure this
  separately from CPU render-call time.
- [xdg-decoration](https://wayland.app/protocols/xdg-decoration-unstable-v1):
  negotiate window decoration where supported; keep a compatible fallback.

Before implementation, define a narrow host interface for window lifecycle,
input, clipboard and frame scheduling. Keep the current X11 backend functioning
throughout and test Wayland under a real compositor; an XWayland window does
not validate native Wayland behavior. Do not port X11 clipboard limits or
selection ownership assumptions directly to the Wayland data device protocol.
