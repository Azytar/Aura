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
| 3+ | not started | correctness and measurement baselines come before performance changes |

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
