# Cell graphics and font routing

Follow-up to the daily-use audit, starting from `760cf4751d7e1658b795a355d456b6d2b31f7bd4`.
Validated on Debian 13, Rust 1.88, Xvfb and Mesa llvmpipe. No physical GPU was
available for this session.

## Why Braille plots had gaps

The primary DejaVu Sans Mono, Liberation Mono and Noto Sans Mono faces did not
provide U+28FF. Aura correctly found a symbol fallback, but its dot layout was
designed for that font's bearings and advance, not Aura's primary cell metrics.
At 14 px, the first two faces produced a 9x17 cell: horizontal dot centers were
3.79 px apart inside a glyph and 5.21 px apart between cells. Vertical gaps were
3.74/3.26/3.74 px inside a glyph and 6.26 px between rows. Noto's taller cells
increased the row gap further. No clipped nonzero mask pixels were found in the
256-pattern probe; changing clipping or choosing another ordinary fallback
would not address the cause.

## Geometry belongs to the renderer

Both renderer paths now classify unadorned U+2800–U+28FF, U+2500–U+257F and
U+2580–U+259F as cell graphics. A high bit in the existing GPU glyph field tags
the codepoint; ordinary atlas indices remain bounded to 16 bits. The text shader
draws a cell-sized quad and computes coverage from the actual cell stride.
There are no additional draw calls, GPU buffers, dependencies or per-graphic
atlas entries. The replacement glyph reservation remains in the shared atlas
policy.

- Braille uses Unicode's eight dot bits on a uniform 2x4 lattice. Centers are
  separated by exactly half a cell width and a quarter of its height, including
  across cell boundaries. Circle coverage is antialiased.
- Box drawing retains each arm's light, heavy or double weight, including mixed
  junctions, double corners, dashed strokes, arcs and diagonals. Stroke placement
  is snapped for crisp light lines and an open double-line gap on even strides.
  Junction endpoints include the crossing stroke's width so odd strides do not
  leave a half-covered center pixel.
- Blocks cover the entire stride, including configured letter/line spacing.
  Fractional edges retain pixel coverage on odd dimensions. Shaded blocks use
  a screen-aligned 2x2 pattern at 25%, 50% and 75% density; quadrant masks retain
  their actual Unicode ordering.

Glyph offsets and synthetic font styles do not distort geometric characters.
Colors, inverse video, decorations, cursor behavior and selection still use the
existing passes. The original Unicode text stays in aura-core, so copying does
not return replacement characters or private encodings. A graphic with attached
combining marks follows the font shaping path rather than discarding the marks.

## Text faces and complete graphemes

Three style faces are discovered beside the primary font. Existing `font.bold`,
`font.italic` and `font.bold_italic` configurations now select their `family` and
`style`; an omitted family uses the loaded primary family. Explicit file paths
are accepted, and Italic/Oblique filenames are recognized. Invalid explicit
faces produce a startup diagnostic. Cell dimensions always use the primary
face. Font attributes prevent double emboldening or skewing a native style;
synthetic styling supplies a requested style when the selected face lacks it.

Previously multi-scalar text always shaped against the primary face and ignored
color raster results. Routing now selects one face covering the whole grapheme
before Rustybuzz shapes it, preserving joining context and emoji ligatures.
ZWJ/ZWNJ, variation selectors and emoji tags are shaping controls, not required
cmap glyphs. Emoji presentation prefers a complete color face; text presentation
keeps the primary face when it has coverage. Otherwise configured fallback
order is retained. If no face covers a cluster, its base character is retained
and unsupported pieces remain missing glyphs.

Shaped masks and RGBA images are composed with alpha-over and cached in the
appropriate atlas. COLR outlines are converted from Swash's premultiplied
channels to the straight alpha used by Vulkan's blend state; PNG emoji retain
their straight channels. Cache keys include the selected face, style, grapheme
and context. Raster input remains bounded to 256 bytes and 128 bytes of context
on each side, with at most 256 shaped glyphs, 1 MiB of intermediate images and a
512x512 final image. There remain eight fallback slots and three style slots.

## Verification and remaining limits

The full fmt/check/clippy/test/build checks in `DAILY_USE_AUDIT.md` passed.
182 runner cases passed, including 24 Vulkan readback tests. Four optional
FiraCode Nerd Font checks returned early because that font is absent. Installed
Noto CJK, Devanagari and color emoji assertions executed.

New regression coverage includes:

- All 256 Braille patterns at 6, 9, 14, 18, 24, 36 and 72 px with two font
  families, padding, glyph offsets and two spacings; weighted pixel centers
  verify the uniform lattice across cells.
- Every box drawing character, full blocks, complementary fractional halves,
  quadrant position/area, continuous light/heavy/double lines and two separately
  connected strokes in double corners from 9 through 72 px.
- Font zoom round trips, color/inverse behavior and copying original graphics.
- Exact bitmap comparison of native style faces and complete emoji ligatures:
  woman technologist, family, skin tone, Spanish flag and emoji heart. Some
  installed emoji designs are monochrome even in an RGBA font; comparisons use
  the font's actual pixels rather than assuming every emoji is chromatic.
- Whole-grapheme Devanagari fallback with context, overlapping mask/color alpha,
  COLR alpha conversion and recovery after a full color atlas.
- Native swapchain pixels for all 256 Braille patterns, continuous borders and
  full blocks before/after a fractional resize. Existing Caps/Shift/AltGr/Compose,
  individual i, password/progress fixture, htop, yes and shell checks still pass.

The implementation cannot resolve eight distinct dots in a cell with fewer
than two horizontal or four vertical pixels. Strong negative spacing can
create that limit; antialiasing does not create missing physical pixels.
Other symbol ranges, terminal image protocols, absent font coverage, font
collection face selection and full input-method integration are separate work.
Proportional fallback overhang remains clipped to its owning cell or wide pair
to protect neighboring TUI content; not every complex script fits that budget.
Physical GPU drivers and real desktop scaling still need platform validation.
The pacman check remains a PTY protocol fixture, not an actual Arch upgrade.

Session logs and a GPU-rendered preview are in `/workspace/aura-audit/drawing-*`.

## Return symbols and emoji follow-up

`⏎` (U+23CE) and `↵` (U+21B5) are text symbols. `↩︎` requests text
presentation; `↩️` requests emoji presentation. They are distinct from the CR
byte generated by pressing Enter.

The missing `↵` was reproduced with Liberation Mono and with Fira Code and
JetBrains Mono reference fonts. None of those primary fixtures covers U+21B5;
the old CJK/color-emoji/Symbols2 fallback set also missed it, so Aura returned
the primary .notdef glyph. The fallback set now includes available DejaVu
Sans Mono (or DejaVu Sans when Mono is absent) after the existing specialist
fonts. Configured priority, the eight-face bound and primary cell metrics
are preserved. The fix needs a covering font installed; it does not bundle
fonts or add Fontconfig discovery.

The regression fails before this addition and passes afterward for both
reference families. Tests require actual cmap coverage before checking ink,
so a visible missing-glyph box cannot satisfy them. Vulkan readback at
12/18/24 px is compared with an independent Font's hinted bitmap and keeps
the adjacent blue cell intact. One channel unit is allowed for UNORM blend
rounding. A fixed brightness threshold would be wrong: small antialiased
Return marks need not have fully opaque pixels.

`😀`, woman technologist, skin tone, Spanish flag, emoji heart and `↩️`
exercise the color atlas; text presentation remains separately covered.
The native cached/fresh-process fixture includes `⏎`, `↵`, smiley and emoji
Return and passes font zoom/reset at 17/18 px with DejaVu, Fira Code and
JetBrains Mono. These are controlled reference files, not a claim about every
version installed on a user's machine. Alacritty is unavailable here, so no
live side-by-side comparison was performed.

Reference sources, checksums, before/after logs and verification results are
in [`tests/campaign/evidence/2026-10-19-symbols`](../tests/campaign/evidence/2026-10-19-symbols).
Fonts were read from the Google Fonts Git repository into the workspace;
they were neither installed into the system nor added to Aura's repository.
To check an exact font file on a dedicated X11 display:

```sh
AURA_REQUIRE_GPU=1 AURA_TEST_SYMBOL_FONT=/path/to/font.ttf \
  cargo test -p aura-text return_arrow_uses_a_real_fallback --locked -- --nocapture
AURA_REQUIRE_GPU=1 AURA_TEST_SYMBOL_FONT=/path/to/font.ttf \
  cargo test -p aura-render --test offscreen_gpu return_symbols --locked -- --nocapture
AURA_REQUIRE_GPU=1 AURA_TEST_SYMBOL_FONT=/path/to/font.ttf \
  python3 tests/x11_campaign.py target/release/aura
```

The first command expects a primary font missing `↵`, to exercise fallback.
Without the override, GPU tests check the four text forms against controlled
DejaVu. With an override they specifically check `↵`; the native fixture
also checks the other characters and shared cache/zoom behavior. Missing
required fonts/Vulkan are errors in required mode, and local skips remain
reported omissions.

## Sextant blocks

U+1FB00–U+1FB3B use a 2x3 rectangle grid, with Unicode subcells numbered
`12/34/56`. These are distinct from the 2x4 circular dots of Braille. Terminal
logos can mix sextants with full and half blocks; all three use the actual
cell stride, including letter/line spacing, without font fallback or glyph
offsets. Attached combining marks still use the text shaping path.

The sextant range omits space, full block, left half and right half because
those shapes already have earlier Unicode encodings. Filled subcell areas
are added before blending, preserving contiguous edges even when a cell's
height is not divisible by three. Colors and inverse video use the same
existing cell passes. Tests use independently named Unicode sextants to
check every pixel, and `tests/x11_sextants.py` checks native initial frames,
color updates and exact comparison with fresh rendering.
