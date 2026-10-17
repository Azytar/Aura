# Campaign evidence

See [the report](../../../../docs/TEST_CAMPAIGN.md) for commands, scope,
budgets, results and limitations. `SHA256SUMS` records checksums of the retained files.

- `final-{quick,extended,exhaustive}`: successful manifests and all stage logs.
- `verification`: the five ordered workspace commands and their statuses.
- `integration`: five existing native suites, commands/statuses/logs.
- `environment`: compiler/toolchain, font inventory/checksum, Vulkan driver,
  sanitizer/coverage symbols and libFuzzer resource defaults.
- `campaign-*-before.log`: failures before the two production fixes;
  `original-shrinks-*`: original proptest local reductions. Smaller stable
  regression inputs are in `../../cases`.
- `campaign-after-fix.log` and final profiles: successful replay/properties.
- `mutations`: three deliberate assertion failures, restored successes,
  checksums and gzip-compressed expected/actual/diff PPM witnesses. The
  geometry expected image has sparse corrected witness pixels; it is not
  a full independent rasterization. Unpack with `gzip -dk IMAGE.ppm.gz`.
- `campaign-fuzz.log.gz`: original-target pilot; `campaign-fuzz-expanded.log.gz`:
  first expanded-target run; `campaign-fuzz-final.log.gz`: final instrumented
  10,000-run target with all 16 curated seeds. `final-fuzz-corpus.tar.gz` retains
  its resulting coverage corpus. Corpus order, runtime feedback and toolchain
  can affect libFuzzer evolution; concrete regressions need none of them.
- `prerequisites`, `local-without-vulkan`, `required-without-vulkan`: intentional
  missing-driver tests. Exit 101/1 are expected negative outcomes. Local
  omissions are not successful GPU validation.
- `font-prerequisites`: temporary missing-font checks (local skip / required
  error), exact restoration and passing baseline reruns.
- `replay-controls`: text replay example and intentional rejection of
  oversized inputs.
- `xorshift-controls`: successful alternate RNG/seed/budget check.
- `timeout-controls`: intentionally interrupted (124), **incomplete** campaign.

The four FiraCode assertions skipped in the full workspace log are recorded
omissions. GPU-only profiles have no skips. No formal proof was attempted.
Logs preserve execution-time paths/line numbers, including those before
formatting. Reports preserve the code hashes at each profile's start.
