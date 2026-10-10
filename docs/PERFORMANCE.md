# Performance

Every number below was produced on this machine by the benchmark binaries in
this repository:

```sh
cargo bench --offline -p aura-core -p aura-text
```

Hardware: AMD Radeon Vega 3 Graphics (RADV RAVEN2), Mesa 26.2.4, Linux 7.2.7,
x86-64, `rustc 1.99.0` release profile. Numbers are from the current
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
- **Resize reflows the logical lines once.** The reflow cost shows up in the
  resize figures; after it, only dirty rows are repainted.

## Reproducing

```sh
cargo build --release
cargo bench --offline -p aura-core -p aura-text
```

`cargo bench` also runs the crates' unit tests, so the output includes the test
summary; the timing lines are the ones starting with a benchmark name.
