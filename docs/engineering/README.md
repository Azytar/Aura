# Azytar Engineering Standard — Aura

This file records the compact engineering baseline for Aura. It complements the
architecture and benchmark documents; it does not duplicate their detailed
methodology.

## Stage status

| Stage | Status | Notes |
| --- | --- | --- |
| 0 — audit | complete | workspace, CI, tests, GPU boundaries, benchmarks and architecture inspected |
| 1 — Rust infrastructure | active | deterministic workspace CI and a separate GPU smoke gate are established |
| 2 — selective migration | not started | Python benchmark/X11 harnesses stay until an equivalent Rust path proves a concrete benefit |
| 3 — reliability | not started | expand only after current integration contracts are pinned |
| 4 — performance | active | first redraw-gating vertical is isolated and CI-verified |

## Audited baseline

Baseline inspected: `main` at
`5ecb2f65783b32980bd49caac778b9637d89a251`.

The workspace contains `aura-app`, `aura-config`, `aura-core`,
`aura-pty`, `aura-render` and `aura-text`. There is no current
`aura-platform` crate; X11 event/window ownership lives in `aura-app`.
`aura-render` receives only the raw connection required for Vulkan surface
creation.

Aura has no installer work in this engineering program. Packaging,
installation assistants and distribution automation remain out of scope.

## CI baseline

The deterministic job uses Rust 1.88.0 and checks:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --lib --bins --locked
cargo test -p aura-pty --test pty_terminal --locked
cargo test -p aura-app --test version_consistency --locked
cargo test --workspace --tests --no-run --locked
cargo build --release -p aura-app --locked
```

GPU assertions are a separate job. A Vulkan/font skip is not evidence of GPU
correctness, and a successful software-Vulkan run is not evidence about a
physical GPU or scanout. The owner hardware campaign remains separate.

The first CI run exposed a non-portable test oracle: it required a particular
CJK fallback bitmap to paint the logical wide-cell spacer column. Terminal
ownership only requires the `WIDE_LEADING`/`WIDE_SPACER` pair; a font may
draw a narrower bitmap inside that span. The test now retains the real
contracts: wide-cell ownership, visible rasterization, fresh rasterization
after resize, the following cell remaining intact, and an exact 12→24→12
round-trip.

## Existing campaigns

The Python X11, mutation, replay and terminal-comparison tooling is not treated
as migration debt by default. Several suites already provide independent
process/X11 oracles and preserved evidence. A Rust replacement is justified only
when it removes a demonstrated weakness such as arbitrary timing, duplicated
orchestration, unsafe cleanup, or an unnecessary dependency.

The terminal benchmark methodology in `docs/TERMINAL_BENCHMARKS.md` remains
authoritative. Host timings such as Vulkan `queue_present` return time are not
screen-presentation completion, and benchmark infrastructure must preserve that
distinction.

## Environment-dependent evidence

- Parser/grid/config and most library tests are deterministic and display-free.
- PTY integration requires Linux PTY/process semantics.
- X11 campaigns require an isolated X server or a deliberately selected session.
- Renderer pixel tests require a usable Vulkan implementation and suitable
  fonts; missing prerequisites must be reported explicitly.
- Physical-GPU latency, power-state and presentation evidence must be collected
  separately on controlled hardware.

## Stage 4 first vertical result

Normal key input previously marked the terminal invalid on every accepted key
even when the key changed no local pixels and the child had not produced output
yet. That could queue a render of unchanged/stale terminal content immediately
before the PTY response that would request the real frame.

The key path now invalidates only when typing itself changes visible local
state: clearing a selection, returning from scrollback, or restoring a hidden
blinking cursor. Otherwise the child output remains the event that requests the
next frame.

The regression uses a raw, non-echoing child on an isolated Xvfb display. It
verifies that unchanged typing produces no render calls while selection,
viewport restoration and blink restoration still repaint. The deterministic
workspace, offscreen Vulkan smoke and this X11 contract all pass on the branch.

This is evidence of removed redundant host-side frames. It is **not** a claim
about keyboard-to-screen latency, scanout, FPS or physical-GPU performance.

## Stage 4 Unicode core result

The Unicode candidate was extracted selectively from the experimental
presentation-latency branch: only `aura-core` grid/terminal changes and their
core benchmark were taken. Renderer, clipboard, PTY-reader-thread and unrelated
instrumentation changes were intentionally excluded.

Short multi-scalar graphemes now keep up to 12 bytes of their original UTF-8
inline in the row sidecar and fall back to `String` when they outgrow that
space. The leading scalar remains in `Cell`; no normalization is performed and
long clusters preserve their exact byte sequence. The associated row writer
also batches the supported ASCII-base-plus-diacritic path while preserving the
scalar fallback for unsupported or context-sensitive cases.

Correctness tests cover short inline clusters, Inline-to-Heap growth, long emoji
ZWJ text through grid/reflow, fragmented parsing, wide-cell and metadata
ownership. The deterministic workspace, release build, offscreen Vulkan smoke
and isolated X11 typing contract pass on this selectively extracted branch.

Existing measurements from the original experiment compared commit
`59aa1e9` with the candidate on the same non-Axiom host using alternating
baseline/candidate pairs after warm-up:

| CPU-only workload | Baseline | Candidate | Change |
| --- | ---: | ---: | ---: |
| Unicode, 1 MiB, no history | 5.53 ms | 4.39 ms | -20.6% |
| Unicode, 1 MiB, 10k history | 7.09 ms | 5.65 ms | -20.3% |
| UTF-8 terminal, 6144 bytes | 54.37 us | 31.55 us | -42.0% |
| Reflow with combining marks | 564.61 us | 325.77 us | -42.3% |

Those timings were **not reproduced by this CI run** and are CPU-core workload
evidence only. The CJK-only workload had no consistent improvement and was
sometimes slower, so this is not a blanket claim that all Unicode or native
terminal throughput improved. GPU presentation and physical input latency
remain unmeasured here.

## Next work

1. Keep the deterministic CI green and avoid folding hardware-sensitive gates
   into it without explicit semantics.
2. Audit the active presentation-latency branch before adopting any result.
3. Add shared Rust orchestration only where it removes duplicated command graphs;
   do not create an xtask framework merely for symmetry with Maverick.
4. Preserve the existing terminal benchmark harness until an alternative
   reproduces its environment capture, failure semantics and clock boundaries.
5. Start performance work only with before/after evidence under comparable
   display, driver, font, geometry and power conditions.
