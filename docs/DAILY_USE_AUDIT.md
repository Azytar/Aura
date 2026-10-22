# Daily use audit

Audited against `910c0fbd546459fba40e19fc46841e7596a5c9e9` on Debian 13,
Rust 1.88.0, Xvfb and Mesa llvmpipe (Vulkan 1.4.305). The real application,
its PTY child and GPU readback were exercised; this is not a claim of complete
xterm compatibility or validation on physical GPUs.

## Causes and decisions

| Area | Root cause | Correction and rationale |
| --- | --- | --- |
| Caps Lock, Shift and layouts | The app selected only two slots from the core X11 mapping. It ignored XKB key types, lock semantics, groups and AltGr. | Use libxkbcommon's X11 map and the modifiers/group from each KeyPress snapshot. Reload on XKB map changes. This preserves queued-event semantics and delegates layout rules to the system instead of inventing case tables. Compose handles dead keys without emitting incomplete characters. |
| Navigation and shortcuts | Modifier bytes for special keys were missing; zoom scanned all levels of a key and could consume a different shifted symbol. The keypad-add keysym was also wrong. | Encode xterm navigation/function-key modifiers, backtab, control bytes and application keypad mode. Match zoom only against the resolved symbol. AltGr-consumed modifiers do not become Ctrl/Alt shortcuts. SGR mouse release keeps the original button code. |
| ncurses redraws | CSI X was ignored and erasure/insert/delete/scroll operations generated default-background blanks. htop's captured output uses X, K and scroll regions, so stale text and incorrect colored gaps were possible. | Implement ECH and background-color erasure throughout the terminal layer. Preserve wide-cell ownership and remove erased grapheme/link sidecars. Add SU/SD, REP, insert mode and programmable tabs. Keep glyph-independent semantics in aura-core. |
| VT dispatch and localized titles | Unrecognized CSI intermediates could fall through to ordinary commands. OSC/DCS treated a raw 0x9C continuation inside UTF-8, such as Ü, as ST. | Separate CSI namespaces and retain lexical UTF-8 state across control-string fragments. Standalone C1 ST still terminates strings; UTF-8 payload stays out of the grid. |
| Narrow glyphs and resize | Text coordinates were normalized by the grid but expanded across the whole window viewport, including padding and incomplete cells. Background lookup used font strides, so the two passes disagreed. | Use a viewport with exact cell pixel dimensions and explicit padding. Apply configured spacing consistently to render, resize and mouse coordinates. Clear gutters with the palette background. The same rasterized i remains byte-identical after a fractional-cell resize. |
| Cursor and overhang | Bar and underline cursors inverted the whole glyph like block cursors. Italic/accent overhang could repaint neighboring cells. | Invert glyph foreground only for a block cursor; clip glyph fragments to their owning cell or wide pair. GPU tests compare pixels outside thin cursor stripes. |
| Font discovery | Recursive search returned the first matching filename, potentially Bold/Italic before Regular. System fallback paths assumed Arch, and configured fallback families were unused. | Rank family matches deterministically, preferring the regular face. Recognize Debian and Arch Noto paths. Lazily load configured fallbacks before system candidates, bounded to eight faces. |
| Long-session glyph cache | A full atlas permanently cached new glyphs as empty images. Shaped cache keys could also outgrow the fixed GPU metadata table. Font invalidation forgot that the Vulkan image already had a sampled layout. | Return recoverable cache pressure without caching failed allocations. Cap metadata at 65,536 entries. Reserve a visible replacement glyph, rebuild the visible grid once, and replace excess glyphs only when even that working set exceeds the atlas. Keep image initialization separate from upload invalidation so barriers synchronize existing sampled images. Both renderer paths share the rasterization policy. |
| PTY and shutdown | Synchronous writes propagated WouldBlock; unbounded read draining could starve keyboard events. XCB replies could queue events while emptying the socket. SIGTERM bypassed child cleanup. | Queue partial writes and watch EPOLLOUT only while pending. Bound pending bytes to 1 MiB, rejecting a whole atom on overflow rather than splitting UTF-8/VT. Read at most 256 KiB per wake. Check the XCB queue before epoll. Own event-loop descriptors with RAII and route termination through signalfd, blocked before GPU threads start. Reset the child's signal mask before exec and reap it on shutdown. |
| Large windows | Window-derived dimensions could exceed fixed GPU SSBO limits; negative config dimensions cast to huge unsigned values. | Share a 512-column/256-row budget between geometry, config and renderer. Reject invalid configured sizes and clamp large live windows, keeping surplus pixels as background. This avoids unbounded GPU allocations and an otherwise fatal resize. |

The XKB dependency adds two Rust crates (`xkbcommon` and `xkeysym`) and native
XKB link libraries. It replaces the incomplete manual keyboard table; no
window toolkit or OS dependency was introduced into aura-core. The declared
minimum Rust version is now 1.88, matching the let-chains already used by the
repository.

## Verification

After rebuilding workspace artifacts, all of the following completed:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked -- --nocapture
cargo build --release --locked
python3 tests/x11_daily_use.py target/release/aura
bash tests/e2e_shell_smoke.sh target/release/aura
./target/release/aura --doctor
```

For current pointer-theme and history-selection regressions, also run
`python3 tests/x11_pointer_selection.py target/release/aura` on a dedicated
X11 display. It checks exact themed cursor images and selection drags at a
screen edge without padding, including stationary scrolling and wheel events.

- 166 runner cases passed, including 15 Vulkan readback tests. Four optional
  FiraCode Nerd Font checks return early because that font is absent; those
  four checks are not claimed as executed coverage. CJK and color emoji
  assertions ran with installed Noto fonts.
- A real nonblocking PTY received every byte of a 131,084-byte UTF-8 bracketed
  paste while its reader initially slept, exercising partial writes and
  backpressure.
- 2,048 seeded mixed TUI operations, main/alternate-screen changes and resizes
  produced identical state with complete versus byte-at-a-time input. Wide
  leading/spacer pairs remained valid on both screens.
- Native X11 checks cover Caps/Shift combinations, Ctrl+Left, backtab, F6,
  live German layout/AltGr, US international dead keys, colored i glyphs,
  padding/spacing, fractional-cell resize and SIGTERM child reaping.
- Actual htop ran through Setup, resize, font zoom, a 6,000-pixel-wide window
  and clean quit. Actual yes output remained interruptible with Ctrl+C.
  Interactive bash completed the original title round-trip and framebuffer
  smoke test. These native checks require neither PRoot nor tini.
- A captured real htop VT stream reproduced the same 24 text rows as pyte
  0.8.2. This comparison covers text state; the independent GPU tests cover
  colors and geometry. The original stream's final text also matched, so this
  comparison alone does not demonstrate the original erasure defect.
- cargo-fuzz 0.12.0 with Rust nightly 2026-10-10 and AddressSanitizer completed
  10,000 runs (`max_len=8192`, timeout 5 s, seed 1692331294) without a crash,
  sanitizer finding or chunking assertion failure. New permanent corpus seeds
  cover UTF-8 control strings and ncurses erasure/scroll/tab operations.

Run the X11 suite only on a dedicated test display: it changes the server's
keyboard layout. Code 77 means a skipped suite, not a successful validation.
Use a separate CARGO_TARGET_DIR for each checkout when checking intermediate
commits; artifacts must not be shared between distinct copies of one package.

## Limits and follow-up

- `sudo pacman -Syu` was not executed as a system upgrade: this host is Debian.
  A deterministic PTY fixture verifies the exact uppercase S, no-echo password
  input, carriage-return progress, background erasure and a UTF-8 OSC title.
  An actual Arch upgrade and its system-specific hooks still need validation.
- Physical GPU drivers, real window managers, keyboard hardware and sustained
  multi-hour interactive sessions were not available in this cloud display.
  The automated scenarios substantiate the listed fixes, not every desktop.
- The keyboard backend provides XKB and Compose. Full input-method integration
  (XIM/IBus) and native Wayland remain outside this backend.
- The drawing follow-up resolved configured style faces, whole-grapheme fallback,
  complex color emoji and font-dependent Braille/block/box geometry. See
  [Cell graphics and font routing](CELL_GRAPHICS.md) for causes and verification.
- The atlas remains bounded. A visible set larger than its capacity uses
  replacement glyphs; pending PTY input beyond 1 MiB is rejected atomically
  with a diagnostic. These are explicit resource limits, not silent cache
  corruption or unlimited allocation.
- This task creates local commits only. Cloud draft saving does not publish the
  environment, and restoration of local-only commits in a fresh task has not
  been independently verified. A Git bundle is retained outside the checkout.

Session evidence is in `/workspace/aura-audit`: baseline regression failures,
VT capture, reference comparison, check/clippy/test/build logs, fuzz log and
native X11/shell results. Reusable installation and startup instructions are
in `/workspace/aura-env` and in the saved environment draft.
