# Performance

For Unicode and style investigations, profile the parser/core independently
from the native terminal. Keep payload construction and history prefill outside
the workload timer, use the same retained-history capacity, and preserve exact
binary hashes and every repetition. CPU samples and allocation profiles explain
costs; instrumented runs must not be used as uninstrumented throughput results.
Native profiles include the graphics driver: llvmpipe executes shaders on the
CPU and cannot establish physical-GPU bottlenecks. Keep these profiles and all
new measurements outside the checkout.

For automated comparisons across Aura, Alacritty, Kitty, WezTerm and xterm,
see [the multi-terminal harness](TERMINAL_BENCHMARKS.md). Its reproducible
workloads generate private HTML/CSV/JSON/SVG artifacts outside Git checkouts.
`./tools/benchmark.sh` automates discovery and displays linear median bars in
the terminal and browser; `--nvim` adds a separate instrumented real-editor
fixture with exact input, status-line pixel witnesses and viewport/search checks.
Editor startup and edit clocks include editor and fixture overhead; these
samples remain separate from native helper clocks and core microbenchmarks.
`--current-session` reuses the desktop's X11/XWayland `DISPLAY` and driver
environment; the default still owns an Xvfb for software checks. Every key/chord
verifies focus atomically before delivery; this guard precedes the input clock,
while delivery/release overhead remains included. Keep the desktop idle and
unobscured. Light median bars and `overview.svg` use the same renderer for both
modes. Reports identify GL probe renderers and Vulkan device inventories,
including mixed or unknown results; they do not attest per-process GPU selection.
Do not pool desktop GPU and Xvfb/software samples into one campaign.
Desktop fixtures record their initial managed geometry, then withdraw/remap
only owned windows outside tiling rules and confirm the exact requested PTY
grid and client extent. Cell steps may be inferred from a nonmatching initial
grid with fractional tails, but the final assertions remain exact. Neovim
requires fresh resize acknowledgements before edit sampling. Output/input
workload clocks start after geometry preparation; startup-to-witness clocks
include that preparation and do not describe natural managed-window startup.
Failed warmups retain their causes and stop further rounds, rather than
repeating an invalid campaign or treating missing samples as zero.
The tables below are historical internal benchmarks, distinct from that harness.

Printable-output changes should be compared with separate baseline/candidate
executables and recorded hashes, retaining unfavorable repetitions. The core
workloads cover scalar parser output, terminal writes and bounded-history
scrolling; they do not measure presentation. Printable spans retain the public
scalar parser events. Differential tests compare spans with bytewise parsing and
scalar terminal writes, including malformed UTF-8, wide pairs, combining marks,
links, decoration, delayed wrap, character sets and insert mode. Recording scroll
is also compared with the previous cloning algorithm and checks buffer ownership.

Native comparisons additionally exercise render coalescing. `window.max_fps`
sets a render-call ceiling while the event loop continues to service input and
output. Record the effective setting when comparing builds; a changed rate is
not evidence of faster GPU presentation. Keep collected measurements private.

Background rendering selects full-screen shading for dense painted grids and
instanced cell quads for sparse ones. Both paths share one pipeline, avoiding
a separate first-use pipeline compilation when opening a dense TUI. Sparse
palette, selection and inverse/faint colors are resolved in the vertex stage;
default undecorated cells emit degenerate geometry. Empty and hidden text cells
also emit degenerate geometry instead of rasterizing a placeholder before discard.
The attachment is still cleared every frame.
Compare sparse cursor/selection traffic with dense colored TUI output; reduced
fragment work can trade against more vertex work. Exact readback tests cover
dense and sparse grids, erasure and custom palettes, including padding and
spacing. Keep software-renderer results separate from physical GPU results.

The `AURA_PERF` `parse` interval covers `Terminal::process`; refreshing host
query properties once before the bounded PTY drain is outside that interval.
Older builds refreshed those properties inside each read's parse interval, so
compare whole-workload time and CPU when evaluating that change.

The sustained core workloads write approximately 1 MiB of line-oriented ASCII,
wide/combining Unicode, mixed Han/kana/Hangul CJK, linked Unicode with colored
underline, or SGR text per iteration, eight times. A linked row-rewrite case
addresses the first visible row repeatedly, with a Han ideograph and 77 ASCII
cells (79 columns total), an OSC 8 link and colored underline; it avoids wrapping
and retains history while exercising replacement of occupied metadata. Each
terminal uses an 80x24 grid and either zero or 10,000 history rows, filled outside
the timer.
Payload construction is also outside the timer. Each iteration asserts that
the configured history length remains retained. These cases isolate processing
and recycling from PTY backpressure, font rasterization and GPU work.
The CJK case exercises both batched Han scalars and kana/Hangul fallback. The
linked case repeats one OSC 8 URL and resets its colored underline on each line,
so metadata work stays visible without growing a table of unique URLs. Compare
builds with the same updated harness and retain exact byte counts.

The UTF-8 row writer batches printable ASCII and the independent double-width
Han ranges U+3400–U+4DBF, U+4E00–U+9FFF and U+F900–U+FAFF. An ASCII base followed
by one U+0300–U+036F diacritic can remain in the same row batch, storing its exact
UTF-8 cluster. Additional marks, other scalars, existing clusters, graphics sets,
insert mode and row-edge handling retain scalar dispatch.
Differential tests compare complete terminal state and replies under fragmented
input; an exhaustive check verifies these Han widths and grapheme boundaries
against the Unicode libraries. ASCII/diacritic combinations are also compared
exhaustively with scalar writes, including REP and cursor-query replies. This
path does not replace contextual shaping.
Hyperlink and underline-color ranges overwrite occupied prefixes directly and
resize once for missing slots. Clearing a last value still releases storage;
ASCII and UTF-8 writers share the same range assignment.

Current core output labels report the exact byte counts for CSI, UTF-8 and
large-output fixtures; older approximate labels remain in the historical tables.
Font workloads report nanoseconds per iteration so warm cache lookups do not
round to zero at the former microsecond precision. Repeated-cluster cache hits
remain a warm-cache case, distinct from mixed/contextual shaping and native input.
Both scalar and repeated-cluster loops hide their receiver and inputs from the
optimizer so an inline lookup cannot collapse into repeated copies of one
constant result. Apply the same harness to both libraries when comparing builds;
older scalar results with a visible receiver are not directly comparable.

Scalar glyph lookup retains 128 bounded entries keyed by character and style
before face resolution. Collisions use the normal lookup; atlas, face, fallback
and size changes invalidate the entries. This is distinct from parsing Unicode
or shaping contextual text and does not establish native input latency.

Font cluster memoization retains one bounded key and shares invalidation with
its atlas. Differential tests compare it with full map lookup through context,
style, face, fallback and size changes. Warm repeated-cluster results should be
considered alongside mixed/contextual workloads and native runs.

Background-pass elision requires the attachment clear to match every effective
cell background, with no visible decorations, cursor or selection. Each acquired
image is still cleared. Validate complete RGBA images across colored-to-default
transitions, decoration states and cursor/selection changes.

The original table below records an earlier run of the benchmark binaries.
The resize follow-up at the end records a separate environment and comparison:

```sh
cargo bench --offline -p aura-core -p aura-text
```

Original environment: AMD Radeon Vega 3 Graphics (RADV RAVEN2), Mesa 26.2.4, Linux 7.2.7,
x86-64, `rustc 1.99.0` release profile. Numbers are from that earlier run of the
benchmarks, single run, no statistical treatment — they are a floor to compare
against after a change, not a certified measurement.

Run them yourself before trusting a comparison. If you change the benchmark,
change this file.

## What is measured

| Benchmark | Result | Notes |
| --- | --- | --- |
| ASCII parser, 4 KiB | 1.6 µs | ~2.6 GB/s through the VT state machine |
| CSI cursor movement, 9 KiB | 150 µs | 1000 `CSI H` sequences |
| UTF-8 terminal, 4 KiB | 690 µs | parse **and** grid write |
| SGR terminal, 3 KiB | 90 µs | 1000 colour changes |
| Terminal output, 250 KiB | 23.9 ms | sustained parsing and printing |
| Grid writes, 4800 cells | 87 µs | 18 ns per cell |
| Full-grid ring scroll, 1000 rows | 393 µs | no allocation: the ring rotates |
| Resize + reflow, 80x25 → 100x30 | 1.19 ms | reflow with cursor tracking |
| Resize + reflow, wide cells | 2.47 ms | 80x25 → 200x60 |
| Resize + reflow, combining marks | 4.71 ms | 200x60 → 80x25 |
| Scrollback page walk, 10k lines | 77.8 ms | 7.8 µs per row, includes row clone |
| Glyph cache hit | 200 ns | warm atlas |
| Shaped grapheme cache hit | 100 ns | warm atlas |
| Rasterize 94 ASCII glyphs | 22.3 ms | **includes loading the font file** |
| Shape + rasterize Arabic grapheme | 713 µs | **includes loading the font file** |
| Font metrics, 7 sizes | 3.20 ms total | **includes loading the font** |
| Runtime zoom 12 → 24 px with warmup | 17.8 ms | includes loading the font |
| Atlas growth, ASCII range, 14 px | 21.8 ms | includes loading the font |

The font benchmarks deliberately include `Font::load`, which reads and parses
the font file. Treat the per-glyph figures as an upper bound; the steady-state
cost of a glyph miss is the cache-hit number above it, minus the shaping and
rasterization.

## What is deliberately not measured here

The measurements that matter most for a terminal are not in the table because
they need a live display and a stable comparison harness:

- **Idle CPU.** AURA renders only when something changes: PTY output, window
  resize, Expose, OSC title change, or a cursor blink. The blob timer drives
  blinking, so a steady cursor with no output produces no frames at all. Verify
  with `top` on an idle window, not by reading this file.
- **Input-to-present latency.** Not instrumented. Adding a timestamp at key
  press and comparing it against the presentation timestamp per frame would let
  you measure the p50/p95/p99 the specification asks for; that work is not
  done yet.
- **Memory under load.** Not instrumented. `scrollback.lines` is the dominant
  term: each retained row stores a full line of cells plus its grapheme and
  hyperlink sidecars.
- **Comparison against Kitty or Alacritty.** Not performed. Any comparison
  needs identical hardware, identical font and size, identical configuration,
  and the same workload, and none of that has been set up. AURA makes no
  performance claim against other terminals.

## Design decisions the numbers support

- **The grid scrolls by rotating a ring, not by copying.** `full-grid ring
  scroll` at 393 µs for a thousand rows is O(1) in rows moved; the one-off cost
  is resetting the rows that were exposed.
- **Scrollback grows by appending displaced rows and is bounded.**
  `scrollback.lines` trims the front and never reallocates the live rows.
- **Rasterization is per glyph per size, and the cache is keyed on it.** Two
  cache hits at 100-200 ns is what keeps a full screen repaint affordable:
  glyph work is paid once per size, not once per frame.
- **Resize reflows logical lines when required.** Independent rows that still
  fit can reuse their buffers; full reflow borrows its source data. Afterward,
  only dirty rows are repainted.

## Reproducing

```sh
cargo build --release
cargo bench --offline -p aura-core -p aura-text
```

`cargo bench` also runs the crates' unit tests, so the output includes the test
summary; the timing lines are the ones starting with a benchmark name.

## Resize and long-history zoom follow-up (2026-10-19)

This comparison used Intel Xeon Platinum 8370C, Linux x86_64, Rust 1.96.0,
DejaVu Sans Mono, dedicated Xvfb `:99` and software Vulkan llvmpipe (Mesa
25.0.7, LLVM 19.1.7). It is a separate environment from the original table.
The baseline was commit `29c9056`; the final code is the working-tree fix.
Raw logs, compiler/environment details and binary/source checksums are in
[`tests/performance/evidence/2026-10-19`](../tests/performance/evidence/2026-10-19).

The old reflow cloned all physical rows, then copied their owned graphemes
into logical lines, then copied them again into output rows. The new full
path borrows old rows and grapheme slices until the replacement owns them.
For width-only changes, independent unwrapped rows that still fit can resize
their buffers in place, preserving their owned graphemes and links. Wrapped
lines, clipping, changed row counts, decorated tails and cases needing
viewport redistribution retain the full path. No history limit was reduced,
no content was dropped to improve timings, and no background threads or
new dependencies were introduced.

### Core workload

The added benchmark populates tree-like UTF-8 lines **outside** the timer,
then measures 20 resize/restore pairs on the same terminal. Each line contains
a combining mark and a wide scalar. Unlike the older resize workloads, these
figures exclude terminal construction and initial output parsing.

| History rows | Round trip | Before | After |
| ---: | --- | ---: | ---: |
| 0 | 80×25 → 79×25 → 80×25 | 0.135 ms | 0.096 ms |
| 1,000 | 80×25 → 79×25 → 80×25 | 5.068 ms | 1.299 ms |
| 10,000 | 80×25 → 79×25 → 80×25 | 91.911 ms | 19.860 ms |
| 10,000 | 80×25 → 80×24 → 80×25 | 99.552 ms | 65.252 ms |

These are means from individual runs, not statistically stable guarantees.
The height-change case still needs full redistribution and benefits only
from avoiding deep copies. Work is still proportional to stored history;
this does not promise constant-time resizing of arbitrarily large buffers.

### Native key delivery through resize

`tests/x11_zoom_latency.py` generates the history, waits for readiness, then
alternates Ctrl+Plus/Ctrl+Minus eight times, measuring the elapsed time until
the child receives SIGWINCH with its new PTY dimensions. The outer window
stays at 640×425; the column counts alternate between 80 and 71. Each operation
finishes before the next key is sent. History generation is not timed.

| History rows | Median before | Median after | Max before | Max after |
| ---: | ---: | ---: | ---: | ---: |
| 0 | 7.290 ms | 5.878 ms | 11.930 ms | 11.077 ms |
| 1,000 | 14.020 ms | 6.343 ms | 17.725 ms | 8.875 ms |
| 10,000 | 57.628 ms | 19.298 ms | 99.712 ms | 24.440 ms |

This measures X11 key delivery, font metrics, terminal resize and PTY
notification. It **excludes GPU completion/presentation** and is not a
full input-to-photon measurement. The samples are few, scheduler/driver
variation remains, and no absolute timing threshold is used as a test.
The final pair ran sequentially without our other builds/tests running.

A preliminary native comparison accidentally shared a Cargo target directory
with the baseline and executed the old binary. It was discarded. The final
comparison verified distinct binary hashes and passed the resize-frame
regression before measurement. Reproduce with separate target directories:

```sh
# Use a dedicated X11 display with Vulkan and DejaVu available.
git worktree add --detach /tmp/aura-resize-baseline 29c9056
CARGO_TARGET_DIR=/tmp/aura-resize-baseline-target cargo build --release --locked \
  --manifest-path /tmp/aura-resize-baseline/Cargo.toml
cargo build --release --locked
python3 tests/x11_zoom_latency.py /tmp/aura-resize-baseline-target/release/aura
python3 tests/x11_zoom_latency.py target/release/aura
cargo bench --offline -p aura-core --bench workloads
```

### Flicker and correctness

The resize flash was separately reproduced: X11's default ForgetGravity
cleared the old window image, and the window background was always black.
Aura now requests NorthWest bit gravity and resolves the configured background
through the X11 colormap. X11 preserves the overlapping old frame and paints
new strips in the configured color until Vulkan presents the new geometry.

The X11 regression pauses **only Aura**, resizes the still-running X server's
window, and compares the intermediate pixels. Both expansion and shrinking
preserve their overlap; exposed strips have the expected configured color.
It then resumes Aura and verifies zoom reset and ordinary resize bursts.
Failures retain XWD captures under `AURA_ARTIFACTS` or `target/resize-evidence`.
This checks the otherwise brief intermediate erase rather than only the final
settled image. It passed with the fix and fails against the baseline.

The new core differential test checks 3,000 geometry/content/history cases
against full reflow, including Unicode, links, colored erasure, wrapped lines,
viewport offsets and spare rows. The identical-size no-op is outside this
comparison because it intentionally preserves state rather than reflowing.
A 10,000-row regression separately checks logical rows, graphemes, hyperlinks,
cursor and viewport after a width round trip that needs no eviction. Physical
ring order can change legitimately and is not a content invariant.

The ordered workspace fmt/check/Clippy (`-D warnings`)/test/release sequence
passed, as did the core exhaustive profile (4,096 cases per generated
property), X11 resize, daily-use and cached/fresh native GPU suites. Four
optional FiraCode tests still skip because that font is unavailable; those
assertions were not validated. Physical GPUs, other compositors/drivers and
input-to-present latency remain unmeasured.

### Wrapped-history resize methodology

The core benchmark also fills retained history outside the timer, using either
separate 160-column paragraphs or a single continuous paragraph built from
80-character blocks. Each workload ends with CRLF to fix the insertion point.
For 1,000 and 10,000 retained rows, capacity is `history * 2 + 64`; eight
round trips run from 80x25 through 73x22 and back, asserting unchanged
restored history length. These are host-side core resize timings, not GPU
presentation or input latency measurements.
