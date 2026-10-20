# Resize and history evidence

See [the report](../../../../docs/PERFORMANCE.md#resize-and-long-history-zoom-follow-up-2026-10-19)
for the environment, workload, commands and limitations.

- `core-before.log` / `core-after.log`: release core benchmarks. History is
  populated outside the added resize timers; numbers are per round trip.
- `native-before.log` / `native-after.log`: all eight samples per history size
  and actual PTY dimensions. The endpoint is SIGWINCH, not GPU presentation.
- `visual-before.log` / `visual-after.log`: the same X11 resize test fails on
  the baseline's intermediate erase and passes with the fix. Baseline exit 1
  in `visual-results.json` is an expected negative, not a successful test.
- `overlap-{expected,actual,diff}.ppm.gz`: baseline visual failure. Expected is
  the old frame, actual is the resized drawable cropped to the old overlap,
  diff is the absolute RGB difference. Decompress with `gzip -dk FILE.ppm.gz`.
- `core-exhaustive.log`: 107 core tests, including 4,096 cases per generated
  property, the 3,000-case full-reflow comparison and 10,000-row regression.
- `verification`: the five ordered workspace commands and their statuses.
  Four optional FiraCode assertions print skips; those are omissions.
- `msrv.log`, `fuzz.log.gz`: Rust 1.88 check and 10,000 instrumented nightly
  libFuzzer runs on the expanded existing target, both completed.
- `x11-daily.log`, `x11-cached-fresh.log`: real X11 workflows and independent
  cached/fresh-process frame comparisons, no internal skips.
- `summary.json`: commands, source/font/binary checksums and scope.

An earlier native comparison reused a baseline executable through a shared
Cargo target directory and was discarded. The final pair verified different
executable hashes and the corrected executable passed the visual regression
before collecting timings. Use separate target directories when reproducing.
`SHA256SUMS` records checksums of these retained evidence files.
