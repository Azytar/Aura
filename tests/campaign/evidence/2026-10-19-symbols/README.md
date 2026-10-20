# Return symbols and emoji evidence

See [the rendering report](../../../../docs/CELL_GRAPHICS.md#return-symbols-and-emoji-follow-up).

- `fallback-*-before.log`: expected assertion failures with the old fallback
  set. The primary font returned .notdef for U+21B5. These are not passes.
- `fallback-*-after.log`, `reference-results.json`: the same Fira Code and
  JetBrains Mono regressions pass after restoring the new fallback group.
- `font-sources.json`: Google Fonts commit, fixture paths and SHA-256. The font
  files and their licenses remain outside Aura's checkout; no fonts were
  installed or vendored. These are reference versions, not the user's files.
- `gpu-*.log`: exact hinted-bitmap reference checks for `↵` at 12/18/24 px,
  including adjacent-cell protection, with each reference primary font.
- `native-*.log`: cached/fresh-process comparisons containing Return symbols
  and emoji, including font zoom/reset. Fira Code and JetBrains Mono ran six
  sizes (9/12/17/18/23/36); controlled DejaVu ran 17/18.
- `verification`: all five ordered workspace checks exited zero. The full
  suite includes text glyph coverage/cache tests, Return-symbol bitmap tests,
  emoji color atlas/ligature tests and previous resize/history regressions.
- `msrv.log`, `resize-final.log`: Rust 1.88 and final live resize regression.
- `summary.json`: scope, sources, executable checksum and omissions.

Four optional FiraCode Nerd Font assertions still skipped. Fetching reference
Fira Code outside the installed font directories does not validate those Nerd
Font-specific assertions. No live Alacritty comparison or physical-GPU test
was available. SHA256SUMS records checksums of the retained evidence files.
