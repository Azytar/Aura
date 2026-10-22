# AGENTS.md

AURA Terminal: a GPU-accelerated terminal emulator for Linux. Six-crate Rust workspace, X11 window backend, Vulkan 1.3 renderer. Rust 2024 edition, MSRV 1.88. The benchmark harness has unit-test CI; run workspace verification yourself.

## Build and verify

- `cargo build --release` produces `target/release/aura`. `glslc` must be on `PATH`: `crates/aura-render/build.rs` compiles `shaders/*` at build time (`--target-env=vulkan1.3`) and fails the build without it.
- Ordered verification sequence (from `docs/DAILY_USE_AUDIT.md`):
  `cargo fmt --all -- --check` → `cargo check --workspace --all-targets --all-features --locked` → `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` → `cargo test --workspace --all-targets --all-features --locked -- --nocapture` → `cargo build --release --locked`
- Focused tests: `cargo test -p aura-core` (parser/grid/scrollback; no display or GPU needed), `cargo test -p aura-pty`, `cargo test -p aura-render`, `cargo test -p aura-config`.
- The offscreen Vulkan readback tests (`crates/aura-render/tests/offscreen_gpu.rs`) print a skip and pass when no usable Vulkan device exists. This is deliberate — an `#[ignore]`d test once hid a shader bug — do not convert them to `#[ignore]`. They also need a system monospace font (DejaVu / Liberation / Noto).
- Cross-emulator harness tests: `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tools/terminal-bench/tests -v`. Native runs default to an owned Xvfb; the Bash launcher can explicitly use `--current-session` (X11/XWayland, focus-checked input). See `docs/TERMINAL_BENCHMARKS.md`. Artifacts must stay outside every checkout.
- Benchmarks: `cargo bench --offline -p aura-core -p aura-text`. If you change a benchmark, update `docs/PERFORMANCE.md`.
- Live end-to-end (needs X11, `xdotool`, `xwd`, ImageMagick `magick`): `./tests/e2e_shell_smoke.sh target/release/aura`. Exit 77 means "no display": a skip, not a pass. Its `xdotool type` input path is inherently racy and the script retries; that is not a terminal bug.
- X11 regression suite (needs `htop`, `setxkbmap`, `xprop`, etc.): `python3 tests/x11_daily_use.py target/release/aura`. It changes the server keyboard layout, so run it only on a dedicated virtual display. Exit 77 means skipped.

## Versioning

`crates/aura-app/tests/version_consistency.rs` fails unless all six `crates/*/Cargo.toml` versions, `CHANGELOG.md` (a `## [<version>]` heading), and `README.md` carry the same version. Bump them together. Other changes go under `[Unreleased]` in `CHANGELOG.md`; release-line rules sit at the top of that file (0.1.x = bug fixes only).

## Architecture

- Strict layering (`docs/ARCHITECTURE.md`): `aura-app` → `{aura-render, aura-config}` → `aura-text` → `aura-core`; `aura-pty` sits below `aura-core`. A crate may only depend on crates below it. X11 code lives in `aura-app` (the diagram's `aura-platform` crate does not exist).
- `aura-core` must stay free of libc, file I/O, and any font/GPU knowledge so the parser is testable without a display; every PTY syscall lives in `aura-pty`.
- Parser invariant: any chunking of the same bytes produces the same event stream (the fuzz target asserts this). A wide cell's two halves are always created and cleared together.
- Shader push-constant layouts are declared in all shader stages and mirrored by `size_of`/`offset_of!` assertions in `crates/aura-render/src/lib.rs`. Layout drift between shader and Rust has already caused a bug; keep them in sync.
- No worker threads and no async runtime: one thread runs the event loop, parser, and renderer.
- Every `unsafe` block carries a `SAFETY` comment; `unsafe_op_in_unsafe_fn` is denied workspace-wide.

## Conventions and gotchas

- Config defaults to `~/.config/aura/config.toml`; imports and runtime settings support validated live reload (see `docs/CONFIGURATION.md`). `example/config.toml` documents every key and is loaded by a test in `crates/aura-config/src/lib.rs`; keep it parseable. (Root `examples/` is an empty leftover.)
- CLI flags include `--help`, `--version`, `--doctor`, `--config-file PATH`, `--check-config` and `--print-config`; config validation must work without X11.
- `crates/aura-core/fuzz` is a separate cargo workspace with its own lockfile and is not touched by root `cargo` commands; it needs cargo-fuzz plus a nightly toolchain.
- Commit messages follow the existing `type(scope): summary` style (e.g. `fix(app): ...`).
