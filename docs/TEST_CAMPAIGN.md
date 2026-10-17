# Reproducible terminal campaign

This campaign explores bounded state machines and checks concrete rendering
oracles. It is not a proof of terminal conformance or absence of defects.
The recorded run started from `3181591` with a clean worktree. No changes
were committed or published. Evidence lives in
[`tests/campaign/evidence/2026-10-17`](../tests/campaign/evidence/2026-10-17).

## Running it

Run from the repository root with Rust, `glslc` and Vulkan 1.3 available.
The font required by the text and new GPU campaign is
`/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf`. Native tests also need
Python 3, X11, `xdotool`, `xdpyinfo`, `xwd`, ImageMagick `magick` and
`vulkaninfo`. Use a **dedicated** X server: existing keyboard suites change
its layout. Output defaults to `target/campaign/<profile>`.

```sh
python3 tests/campaign/run.py quick --gpu required
python3 tests/campaign/run.py extended --gpu required
python3 tests/campaign/run.py exhaustive --gpu required --triples
```

`--gpu off` explicitly excludes GPU/native stages. `--gpu local` reports
missing prerequisites (including the controlled text font) as omissions; `--gpu required` fails instead. A safety
deadline kills the stage's process group and records `INCOMPLETE`, never a
successful random budget. Each property uses a fixed case count, not time.
The default deadline is 600 seconds per stage; change `--safety-seconds` on
slower machines. Do not run native or mutation suites concurrently.

| Profile | Cases per property | Max actions | Initial/max generated grid | Max generated history | Shrink iterations |
| --- | ---: | ---: | --- | ---: | ---: |
| quick | 64 | 32 | 12×6 | 32 | 1,024 |
| extended | 2,048 | 96 | 24×12 | 128 | 4,096 |
| exhaustive | 4,096 | 128 | 32×16 | 256 | 8,192 |

There are three generated core properties and one text property. The focused
combining property always has three actions. Widths/heights can shrink to
one; zero resize requests must fail without any state change. History can
be zero. Selection coordinates clamp to the *current viewport*, an explicit
semantics that survives shrinking. Other invalid replay syntax fails loudly.
Main-screen reflow may discard content under storage limits: arbitrary
shrink/grow conservation is deliberately not asserted.

All profiles enumerate all 65,536 braille pairs. `--triples` additionally
checks all 16,777,216 triples, reusing one tiny core terminal. It does not
initialize Vulkan for each combination. The profile name describes its
budget; only these finite domains and the stated short partitions are
exhaustively enumerated.

Override budgets with `--cases`, `--actions`, `--width`, `--height`,
`--history`, `--shrink`, `--seed` (decimal or hexadecimal) and `--rng`
(`chacha` or `xorshift`). Absolute caps are 65,536 cases, 512 actions,
128×64 cells, 4,096 history rows and 65,536 shrink iterations. Individual
random byte actions have at most 32 bytes. Replay files are at most 512 KiB,
512 actions and 8,192 bytes per byte action. GPU image configurations are
fixed and bounded. Each failure overwrites a property/seed-specific artifact
instead of emitting a file for every shrink attempt.

Default seed: `1096110657` (`0x41555241`). Its encoding is four little-endian
u64 words `seed + i`, wrapping. ChaCha receives all 32 bytes; XorShift the
first 16. The manifest records the effective configuration, exact commands,
exit codes, durations, compiler, lockfile/font/source SHA-256 and base commit.
Tests read process environment; they never change global environment in
concurrent Rust tests. `proptest = 1.6.0` is pinned, development-only, with
`std` and without fork/timeouts. Cargo.lock pins transitive dependencies.

## Properties and their boundaries

- **State/fragmentation:** structured VT actions plus arbitrary bytes,
  Unicode, resize, viewport movement, history limits, selection and clearing.
  After every action: cursor/margins/dimensions, wide ownership, graphemes,
  hyperlinks, history/viewport, selection bounds and bounded external queues.
  Compare the entire private terminal state (both screens, parser, attributes,
  modes, title, replies, sidecars and effect queues) against byte-at-a-time
  and varying chunks with empty calls. Concatenate replies; drain queues at
  equivalent checkpoints. A common suffix visits alternate/main screens and
  requests replies to expose latent divergence. Existing deterministic
  screen-isolation and restoration tests remain in the workspace suite.
- **Independent ASCII model:** ordinary nonwrapping writes, ECH, EL, ICH,
  DCH, SU, SD, IL and DL with explicit CUP, default attributes and full-screen
  margins. A small Vec-based model specifies row contents using insertion,
  removal and slicing. It does not model Unicode, reflow or partial margins;
  those are checked by invariants and existing examples, not an independent
  full VT emulator.
- **Combining/resize:** append a combining acute before versus after a resize
  and compare grid/cursor. This metamorphic property found a stale coordinate.
- **Text:** controlled DejaVu font at 12/17/23 px, styles and shaping context;
  repeated lookup IDs/metadata, no second upload, finite/bounded metrics/UVs,
  upload lengths and fallback/metadata caps. Cache pressure is recoverable;
  existing deterministic atlas-saturation/rebuild tests exercise the full path.
- **Unicode:** width and segmentation dependencies both report Unicode 17.0.0.
  Samples distinguish ASCII/NBSP/thin/ideographic spaces; combining sequences,
  CJK, emoji ZWJ/tone/flag/VS, Hangul Jamo, Arabic and Devanagari, malformed
  and truncated UTF-8. This checks storage/fragmentation and shaping contracts,
  not full bidi support or a Unicode conformance suite. Graphemes are capped
  at 256 UTF-8 bytes. Parser limits: CSI 128 bytes/32 parameters, strings
  4,096 bytes. Directed cases cross string/grapheme limits.
- **Short partitions:** all partitions of each directed input of at most
  eight bytes, including UTF-8/control sequences. Longer corpus entries use
  directed chunks; generated streams compare whole, byte and variable chunks.
  This does not enumerate every possible eight-byte input.

Production history defaults to 10,000 rows; the campaign uses smaller explicit
limits to reach eviction often. Font atlases are 1024×1024 grayscale and RGBA,
with at most 65,536 metadata entries and eight fallback faces. Existing tests
exercise saturation, visible replacement and recovery. No production
inspection APIs or unsafe blocks were added for this campaign.

## Stable replay

```sh
AURA_REPLAY="$PWD/tests/campaign/cases/combining-resize.case" \
  cargo test -p aura-core terminal::campaign::replay_cases --locked -- --nocapture
AURA_REPLAY="$PWD/tests/campaign/cases/insert-wide-link.case" \
  cargo test -p aura-core terminal::campaign::replay_cases --locked -- --nocapture
```

The header is `AURA-CASE 1`, followed by `property state|model|combining`
and `size WIDTH HEIGHT HISTORY`. Each subsequent line is an action:
`bytes HEX` (`-` means empty), `resize W H`, `view SIGNED_ROWS`, `limit N`,
`select ROW COL ROW COL`, `clear`, or `edit OP ROW COL N`.
Edit operations 0..8 are write/ECH/EL/ICH/DCH/SU/SD/IL/DL; counts are
`N+1`, writes use `a + N%26`, and edit coordinates wrap modulo the current
grid dimensions. Positive `view` scrolls up, negative down. Comments start
with `#`. Replay needs no random generation. Without `AURA_REPLAY`, the
replay test loads all tracked `.case` files in sorted order.

Failures report property, first failing action, seed/settings and save a
shrunk concrete case. The runner checks that encoding/decoding reproduces
the failure before reporting it. `proptest-regressions` retains its seeds;
manual further reductions are kept as concrete cases alongside the original
shrunk evidence. The two bug replays were run before and after their fixes.
The text format is `AURA-TEXT-1 SIZE_INDEX BOLD ITALIC UTF8_HEX` (indices
0..2, booleans 0/1); replay via `AURA_TEXT_REPLAY=path cargo test -p aura-text
campaign:: --locked -- --nocapture`.

## Rendering and evaluating the tests

The new CPU braille oracle reads Unicode dot names (assignments unchanged
since Unicode 3.0; fixture generated using Python Unicode 14.0) and the
paper diagram `14/25/36/78`. It checks eight on/off witnesses and connected
component counts for every pattern in nine configurations: sizes 17/18/23,
spacing (0,0)/(1,3)/(16,32), offsets -32/+32 at valid bounds. It does not
use the shader's bit table. Witness failures save actual, sparse expected
(witness corrections over actual pixels) and diff PPM images; sparse
expectations do not claim a full independently rasterized image.

Existing GPU tests additionally assert box continuity, blocks/fractions/
quadrants, colors/inverse, cursor/selection/decorations, padding, spacing,
wide overwrites, atlas recovery and font rebuilds. Cached-atlas comparisons
allocate a new renderer **and Font** for the reference. Native X11 tests
compare dirty-row rendering against a separate fresh process, then repeat
after font zoom/reset. An independent required blue background pixel prevents
blank frames in both paths from vacuously passing. Frames use the same
controlled font/driver and compare exactly: no tolerance was increased.
Failures save expected/actual/diff PPMs.

The initial native harness exposed two harness defects: an invalid conservation
assumption when zoom discarded occupied rows, and black captures of occluded
Xvfb windows. The final fixture has spare rows/columns, raises windows before
capture and checks content. These were not attributed to Aura.

```sh
python3 tests/campaign/mutations.py target/campaign/mutations
```

This temporarily swaps braille dots, retains an invalid wide half, and omits
dirty-row updates. It requires specific assertion failures (not compiler
failures), restores exact saved source bytes in `finally` and reruns the
original tests. Run only with a clean/stable editing session and dedicated
X11. All three were detected and their restored tests passed. Shader and
renderer source have no residual mutation diff.

## Instrumented fuzzing

The existing separate fuzz workspace was extended; no second fuzz framework
was introduced. The target compares both screens, observable modes/cursor,
replies, queues, selection and links, with interleaved resize/viewport/selection
and a common continuation. Input is capped at 8,192 bytes, grid 16×8 and
history 32. The first bytes choose dimensions/chunks but remain terminal input,
so the original corpus is still useful.

```sh
mkdir -p target/campaign/fuzz-corpus
cp crates/aura-core/fuzz/corpus/vt_parser/* target/campaign/fuzz-corpus/
cargo +nightly-2026-10-10 fuzz run --fuzz-dir crates/aura-core/fuzz \
  vt_parser target/campaign/fuzz-corpus -- \
  -runs=10000 -max_len=8192 -timeout=5 -seed=1096110657
cargo fmt --manifest-path crates/aura-core/fuzz/Cargo.toml -- --check
cargo +nightly-2026-10-10 check \
  --manifest-path crates/aura-core/fuzz/Cargo.toml --locked
```

The retained log shows libFuzzer coverage counters/PC tables; the binary
symbol evidence also confirms AddressSanitizer instrumentation. 10,000 expanded-target runs completed in seven seconds;
this is coverage-guided fuzzing, not a mathematical result. A preceding
10,000-run pilot used the original target and is identified separately.

## Recorded results (2026-10-17)

Environment: Linux x86_64, Rust 1.96.0, MSRV check with 1.88.0,
nightly-2026-10-10 and cargo-fuzz 0.12.0. Tools/libraries were restored to
`/workspace/.aura-env` using the saved environment setup, without system
configuration changes. Vulkan used **software** llvmpipe, Mesa 25.0.7,
LLVM 19.1.7, API 1.4.305; no instance validation layers were installed.
X11 ran on our dedicated Xvfb `:99`. The environment evidence includes font
inventory, compiler details, driver and primary-font checksum.

The pilot core quick run took about 0.19 s and braille pairs 0.18 s;
a separate initial triple measurement took 60.92 s. These established the
budgets above. Final serial core profiles include all six campaign tests:

| Profile | Core seconds | Text seconds | GPU seconds | Native seconds | Total stage seconds | Result |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| quick | 1.12 | 0.42 | 10.22 | 2.62 | 14.51 | complete |
| extended | 42.05 | 11.04 | 10.22 | 4.35 | 67.83 | complete |
| exhaustive + triples | 142.53 | 21.51 | 13.28 | 7.27 | 184.86 | complete |

Stage durations include Cargo startup/build/lock waits and Vulkan setup, not
just property evaluation. Final exhaustive core evaluation: combining 3.67 s,
ASCII model 55.08 s, state/fragmentation 33.60 s and triples 50.08 s. Native
profiles compared 2/4/6 sizes respectively. The GPU stage ran all 27 tests in
`campaign_gpu`, `cell_graphics_gpu` and `offscreen_gpu`, without GPU skips.
`narrow_glyph_pixels_and_backgrounds_survive_padding_spacing_and_residue`
passed; no new residue defect was reproduced or attributed to this change.

Two defects were reproduced with saved replays **before** repair:

1. `aa`, resize 12×6 → 1×6, combining acute: `last_printed` retained the old
   column, losing the mark. Resize now remaps the grapheme anchor using the
   reflowed insertion cursor, respecting wide continuations and fixed alternate
   coordinates. A permanent unit test and metamorphic property pass afterward.
2. OSC 8 link + insert mode + `界A` in 3×1: shifting cleared the link on the
   valid wide spacer because its scalar is zero. Cleanup now distinguishes
   structural spacers from empty cells. The permanent unit test and sidecar
   invariants pass afterward.

The original proptest local reductions and further reduced concrete replays
are retained; “minimal” filenames mean the reduction reached by this budget,
not a proof of global minimality.

The exact ordered AGENTS.md commands all exited 0:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked -- --nocapture
cargo build --release --locked
```

The full test command reports four optional FiraCode Nerd Font tests as
successful Rust test functions after printing `skipping`; their assertions
were **omitted**, not validated. All other available font/GPU assertions ran.
The separate fuzz workspace format/check and the final Rust 1.88 workspace
check also passed. A preliminary Clippy check caught an OR-pattern style
warning in the new model, corrected to a range without suppression; the
separate fuzz target needed formatting. Neither was a product failure.

The final instrumented fuzz run included all 16 curated seeds and completed
10,000 runs in six seconds, no assertion/sanitizer failure, libFuzzer feedback `cov=2621` / `ft=8046`, 736 corpus entries, peak reported RSS 457 MiB.
The short run only grew its length frontier to 171 bytes despite the 8,192-byte
input cap; directed long-input tests supply complementary limit coverage.
The earlier expanded-target seven-second run and original-target pilot logs
are retained separately. These coverage numbers describe libFuzzer feedback,
not a project coverage percentage.

Executed sequentially on `:99`, all exited 0 without internal skips:

```sh
./tests/e2e_shell_smoke.sh target/release/aura
python3 tests/x11_daily_use.py target/release/aura
python3 tests/x11_input_regressions.py target/release/aura
python3 tests/x11_shell_workflows.py target/release/aura
python3 tests/x11_resize.py target/release/aura
```

These exercise real PTY/shell round trips, Bash editing and synthetic no-echo
input, clear/reset framebuffers, vi save/resize, less paging/search/resize,
htop setup/resize/zoom, Compose/modifiers/repeats, mouse tracking transitions,
selection, swapchain graphics, resize bursts and child cleanup. The workspace
also ran its real PTY integration test. No account credential was entered.

Three deliberate mutations produced assertion failures (101, 101, 1), and
the corresponding restored tests exited 0. Their result file includes exact
source checksums; all three still match after restoration. There is no shader
or production-renderer diff. Additional harness checks confirmed local
Vulkan absence yields `COMPLETE_WITH_OMISSIONS`, required absence fails,
XorShift/seed-zero/budget overrides work, a one-second deadline yields
`INCOMPLETE` (124), and oversized core/text replay reads are bounded before
decoding and rejected. Temporary nonexistent-font paths also verified local
omissions and required errors in both text and GPU tests, followed by exact
restoration and successful reruns. Those intentional negatives are not product failures or
completed random campaigns.

Remaining limits: physical GPUs and other drivers/desktops, validation-layer
runs, FiraCode, full Unicode/bidi conformance and longer fuzz/multi-hour
sessions. `tmux` is absent; `axiom` has no account here; no SSH test server or
credentials were provisioned. Those interactive cases remain unvalidated,
not terminal defects. No formal verification was performed. Deterministic
examples, generated properties, finite enumeration and integration evidence
are distinguished above; none guarantees infallibility.

The evidence directory has an index and SHA-256 list. Logs retain the paths
and source line numbers used at execution time. Harmless later formatting
of the fuzz target, bounded replay reads and prerequisite/deadline handling explain
source hashes that differ between profile snapshots; terminal fixes and GPU
assertions did not change during the three final profiles. Raw early harness
captures remain outside the checkout; the curated evidence includes only
failures with meaningful oracles and the final successful logs.
