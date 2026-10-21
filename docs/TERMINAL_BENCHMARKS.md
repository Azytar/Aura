# Automated terminal comparisons

`tools/terminal-bench/bench.py` runs the same synthetic workloads sequentially in
several Linux/X11 emulators and creates a self-contained HTML dashboard, CSV,
JSON and standalone SVG charts. It uses Python 3.11+ and the standard library.
The harness is independent of Aura's internal core/font microbenchmarks.

The source, adapters and methodology can be versioned. Run directories, logs,
measurements and generated reports must remain outside every Git checkout.
The runner rejects checkout paths, including symlinks and worktrees, and refuses
to overwrite an existing run or report. It makes no network requests.

## Run

Build Aura first, then review `tools/terminal-bench/example.toml`. Relative
executable/font paths resolve against that file, not the current directory.
Available adapters are Aura, Alacritty, Kitty, WezTerm and xterm. Versions that
reject a configuration fail visibly; an adapter's existence is not a claim that
every version has been validated. Foot requires a Wayland harness and is not
included in this X11 comparison.

Runtime requirements:

- Python 3.11+, Linux `/proc`, Xlib, XTest and `xdotool`.
- Fontconfig's `fc-match` and the intended regular font file.
- Installed emulator binaries and their graphics/runtime dependencies.
- Xvfb for `--xvfb`; a dedicated existing X11 server for `--display`.
- `glxinfo` and `vulkaninfo` are optional renderer inventory tools. Missing tools
  remain explicit; a rendering declaration does not certify physical hardware.

```sh
python3 tools/terminal-bench/bench.py list \
  --config tools/terminal-bench/example.toml

python3 tools/terminal-bench/bench.py run \
  --config tools/terminal-bench/example.toml \
  --profile quick --xvfb --environment software \
  --output /path/outside-checkout/terminal-run-001 --baseline aura
```

Open `terminal-run-001/report/index.html`. Filter by emulator or cell geometry;
hover over bars for sample counts. Individual charts can be exported as SVG.
The detailed table includes units, p50/p95/p99, observed ranges and per-metric
changes relative to the chosen baseline. Each metric retains its own unit and
direction of improvement.

`quick` uses one warmup and three measured repetitions, 256 KiB output budgets,
16 samples for each input case, four resize pairs and 0.3 seconds of idle time.
It is a functional smoke profile, unsuitable for stable hardware rankings.
`standard` uses two warmups and seven measured repetitions, 8 MiB budgets,
100 input samples per case, twelve resize pairs and two idle seconds.
`--repetitions`, `--warmups`, `--seed` and `--timeout` are explicit overrides
retained in the run. Retain unfavorable repetitions and inspect raw trials.

For hardware work, use an otherwise idle **dedicated** X11 desktop:

```sh
python3 tools/terminal-bench/bench.py run \
  --config /path/to/comparison.toml --profile standard \
  --display :1 --environment hardware \
  --output /path/outside-checkout/terminal-run-002 --baseline aura-main
```

The runner changes focus to its own windows and injects the `a` key through
XTest. It does not change the keyboard map. Avoid locks, active modifiers and
other input on that display. Wrong input bytes fail the trial. Window-manager
constraints that prevent the requested geometry also fail visibly.

`--environment hardware` is a declaration, recorded beside GL/Vulkan inventory.
Check the actual renderer of each emulator. An inventory containing llvmpipe or
lavapipe must not be described as proof of physical GPU performance. Xvfb runs
are labeled separately in the dashboard, regardless of this declaration.

## What the clocks measure

| Metric | Clock boundary | Meaning |
| --- | --- | --- |
| Startup | process launch → raw helper handshake after cursor queries | PTY readiness, including the common Python helper; first visible frame is not measured |
| ASCII / Unicode / SGR / TUI output | first workload write → ordered DSR reply received by helper | bytes/s through PTY and terminal processing; not presented frames/s |
| Key delivery | XTest request → helper receives exactly `a` | host event delivery and PTY scheduling, including helper scheduling |
| Key delivery under output | same boundary while a separate synthetic child continuously writes | responsiveness with a saturated, backpressured PTY; no pixel timing in this case |
| Key to observed pixel | XTest request → new witness color read from the X11 drawable | upper bound to drawable update, including control transport, capture and 1 ms polling |
| Resize | XResizeWindow request → helper observes target PTY columns/rows | reflow, event handling and geometry notification; no presentation timestamp |
| CPU | differences in observed process-tree CPU counters | terminal tree after readiness, excluding helper descendants and the X server |
| RSS | sum of live tree resident pages, sampled every 10 ms | sampled process RSS, with shared pages counted per process; not PSS, VRAM or a guaranteed instantaneous peak |

Input-to-observed-pixel is a closed-loop test: one key and its witness complete
before the next sample. XGetImage can observe a drawable before the compositor
or display scans it out. Neither this metric nor DSR establishes input-to-photon
latency. The capture itself can synchronize server/driver work. Control/startup
and geometry waits have timeouts; a stalled native X server/driver can require
an external system watchdog.

CPU counters have the host's scheduler-tick resolution, recorded in the
manifest. Short idle trials can report zero CPU ticks without proving that the
process consumes no CPU. The recorder samples the tree while waiting for control
messages; its own CPU and synthetic writer CPU are outside the terminal scope.
X server/compositor CPU, GPU energy and VRAM are not measured. These scoped
figures do not establish total system efficiency.

## Workloads and correctness boundaries

Every repetition starts a fresh emulator and raw helper with echo disabled.
Font/config paths and child arguments are explicit; personal dotfiles and
terminal servers are avoided. Diagnostics are disabled. The intended regular
face is hashed and checked against Fontconfig in an isolated configuration.
Library versions and fallback rendering can still differ across emulators.

- ASCII scrolls short full lines; Unicode includes wide CJK and combining text.
- SGR changes truecolor attributes. TUI updates rewrite addressed colored rows.
- Payload construction happens before the output timer; actual byte counts are
  recorded because complete UTF-8/ANSI blocks may exceed the requested budget.
- An ordered cursor query closes each output interval. Wide/combining cursor
  geometry is checked before timing, but this is not a complete VT conformance
  or glyph-image correctness suite.
- Before resize, the helper clears existing history and fills the configured
  capacity plus the visible rows, outside the resize timer. Eighty by twenty-four
  becomes 73x22 and returns in the default fixture. History capacity is configured
  equally; generic terminals do not expose a verified retained-row count.
- Each ordinary input sample must change an independent solid-color cell in the
  drawable. Stale frames and unchanged OSC titles cannot complete that sample.
- The loaded-input producer is a separate child, bounded by a timer and the
  number of input samples. PTY backpressure bounds queued output. Producer exit
  before a sample fails the trial; the producer is stopped and output drained
  before the terminal is closed.

Existing Aura core properties and GPU pixel comparisons remain the appropriate
checks for Aura's content preservation and rendering correctness. Run them before
accepting an optimization based on this dashboard.

## Fair comparisons and statistics

The default policy groups only equal measured cell dimensions, on the same run,
font hash, configured grid/history and rendering environment. A complete
comparison requires at least two available emulators. Missing required binaries
or failed trials produce a nonzero exit status; optional missing binaries remain
explicit omissions. Matching font
names and requested sizes alone does not guarantee equal cell dimensions.

The sample DejaVu/14 px configuration applies an explicit one-pixel horizontal
spacing adjustment to Alacritty and Kitty to match Aura's observed cell stride.
It does not alter font size. Recheck the recorded geometry on another version or
DPI. Aura accepts nonnegative spacing; Alacritty supports signed offsets; Kitty
uses `modify_font`. Nonzero adjustments for other adapters are rejected. xterm
may belong to a separate geometry cohort. `--geometry-policy grid` explicitly
allows comparison of equal character grids with different pixel areas; the
report labels that weaker condition.

Initial fractional-cell remainder pixels are normalized to the PTY text extent
before measured work. Text is pinned top-left and the resulting client extent
is checked. Requested grids and each resize acknowledgement must match exactly.

Execution is sequential. Each round shuffles emulator order with a fixed seed;
the exact order is retained. Warmups are retained but excluded from summaries.
All raw samples remain available, including failed and interrupted trial records.
Failed trials contribute no timing values. Missing binaries and incompatible
geometries remain missing, rather than becoming zero bars.

Percentiles use nearest rank across retained samples of successful measured
trials. Key/resize trials contain multiple samples; startup/output trials contain
one each. The manifest preserves trial boundaries. Bars show p50 and the observed
min–max range, **not a confidence interval**. A partial series has a distinct
status/color and no relative baseline delta. There is no combined performance
score or automatic noisy timing gate.

To compare Aura builds, add two entries with `kind = "aura"`, distinct IDs and
absolute paths to independently built binaries. Identical hashes for the same
kind are rejected to catch accidental reuse of a shared build target. Preserve
both binaries and their build/source references. An optional `source_ref` per
entry records a declared commit or build reference without assuming that the
current checkout produced an existing binary. Executable hashes do not hash
the full dependency closure or prove identical driver/library environments.

## Artifacts, automation and regression checks

`run.json` records effective settings, font/helper/harness/executable hashes,
versions, execution order, geometry, correctness checks, phase resources and raw
samples. GL/Vulkan inventory and each terminal's launch configuration/logs stay
beside it. The harness sources are snapshotted into the private run and the
helper executes from that snapshot. Binary and primary font hashes are checked
before and after every trial; rebuilding a binary mid-campaign invalidates that
trial. Executable hashing also warms its filesystem cache, so startup measures
a cached executable rather than a certified cold boot. Completed trials are atomically saved after each execution. An
interrupted active trial remains identified in the manifest.

Reports can be regenerated without launching any emulator, into a new directory:

```sh
python3 tools/terminal-bench/bench.py report \
  /path/outside-checkout/terminal-run-001/run.json \
  --output /path/outside-checkout/terminal-report-002 --baseline alacritty

PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover \
  -s tools/terminal-bench/tests -v
```

The GitHub workflow runs harness unit/failure tests only. It does not run
performance campaigns or upload logs, reports, screenshots or measurements.
Hardware campaigns belong on controlled local runners, with artifacts retained
in a private directory outside the checkout. Keep workload/method changes under
review and collect a new baseline after changing them.
