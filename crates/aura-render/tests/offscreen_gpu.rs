use aura_core::Terminal;
use aura_render::OffscreenRenderer;
use aura_text::Font;

/// Reports whether this host can create the offscreen Vulkan device the
/// assertions need. Machines without a GPU (or without the ICD loader) degrade
/// to a printed skip instead of a false failure, so `cargo test` really runs
/// these assertions wherever the hardware allows it.
fn require_gpu() -> bool {
    match OffscreenRenderer::new(1, 1) {
        Ok(_) => true,
        Err(error) => {
            eprintln!("Vulkan device unavailable: {error}");
            false
        }
    }
}

#[test]
fn renders_terminal_cells_and_reads_back_expected_colors() {
    if !require_gpu() {
        eprintln!("skipped: no Vulkan 1.3 graphics device available");
        return;
    }
    let mut font = Font::load(None, 18.0).expect("load system monospace font");
    let metrics = font.metrics();
    let mut terminal = Terminal::new(3, 2).expect("create terminal grid");
    terminal.process(b"\x1b[41mA\x1b[42mB\x1b[44mC\x1b[0m\r\n123");
    assert_eq!(
        terminal.viewport_cell(1, 0).unwrap().bg,
        aura_core::DEFAULT_COLOR
    );
    let width = metrics.width * 3;
    let height = metrics.height * 2;
    let mut renderer = OffscreenRenderer::new(width, height).expect("create offscreen renderer");
    let pixels = renderer
        .render(&mut terminal, &mut font)
        .expect("draw terminal and read back pixels");
    assert_eq!(pixels.len(), (width * height * 4) as usize);

    let pixel = |x: u32, y: u32| {
        let index = ((y * width + x) * 4) as usize;
        &pixels[index..index + 4]
    };
    assert_eq!(pixel(0, 0), &[205, 0, 0, 255]);
    assert_eq!(pixel(metrics.width, 0), &[0, 205, 0, 255]);
    assert_eq!(pixel(metrics.width * 2, 0), &[0, 0, 238, 255]);
    assert_eq!(
        pixel(0, metrics.height + metrics.height / 2),
        &[24, 24, 24, 255]
    );

    let first_cell = (0..metrics.height)
        .flat_map(|y| (0..metrics.width).map(move |x| (x, y)))
        .map(|(x, y)| pixel(x, y))
        .any(|rgba| rgba[0] > 150 && rgba[1] > 150 && rgba[2] > 150);
    assert!(
        first_cell,
        "the atlas sampled at least one bright A glyph pixel"
    );
}

#[test]
fn glyph_cache_hits_keep_later_table_entries() {
    if !require_gpu() {
        eprintln!("skipped: no Vulkan 1.3 graphics device available");
        return;
    }
    // "ABA" assigns ids 0,1 then hits id 0 again; a truncating table update
    // would drop entry 1 and leave the middle cell without sampled pixels.
    let mut font = Font::load(None, 18.0).expect("load system monospace font");
    let metrics = font.metrics();
    let mut terminal = Terminal::new(3, 1).expect("create terminal grid");
    terminal.process(b"ABA");
    let mut renderer = OffscreenRenderer::new(metrics.width * 3, metrics.height)
        .expect("create offscreen renderer");
    let pixels = renderer
        .render(&mut terminal, &mut font)
        .expect("draw terminal and read back pixels");
    let cell_bright = |column: u32| {
        (0..metrics.height).any(|y| {
            (0..metrics.width).any(|x| {
                let index = ((y * metrics.width * 3 + column * metrics.width + x) * 4) as usize;
                let rgba = &pixels[index..index + 4];
                rgba[0] > 150 && rgba[1] > 150 && rgba[2] > 150
            })
        })
    };
    assert!(cell_bright(0), "first A cell renders glyph pixels");
    assert!(
        cell_bright(1),
        "B cell survives the later A cache hit in the glyph table"
    );
    assert!(cell_bright(2), "second A cell renders glyph pixels");
}

#[test]
fn underline_and_strikethrough_paint_foreground_rows() {
    if !require_gpu() {
        eprintln!("skipped: no Vulkan 1.3 graphics device available");
        return;
    }
    // Spaces isolate decorations from glyph pixels: the bottom row of an
    // underlined cell and the middle row of a struck cell must carry the
    // foreground color while plain cells stay background.
    let mut font = Font::load(None, 18.0).expect("load system monospace font");
    let metrics = font.metrics();
    let height = metrics.height;
    let mut terminal = Terminal::new(3, 1).expect("create terminal grid");
    terminal.process(b"\x1b[4m \x1b[24m \x1b[9m ");
    let mut renderer =
        OffscreenRenderer::new(metrics.width * 3, height).expect("create offscreen renderer");
    let pixels = renderer
        .render(&mut terminal, &mut font)
        .expect("draw terminal and read back pixels");
    let pixel = |column: u32, y: u32| {
        let index = ((y * metrics.width * 3 + column * metrics.width) * 4) as usize;
        &pixels[index..index + 4]
    };
    let foreground = [216, 216, 216, 255];
    let background = [24, 24, 24, 255];
    assert_eq!(
        pixel(0, height - 1),
        &foreground,
        "underlined cell paints its bottom row",
    );
    assert_eq!(
        pixel(0, 0),
        &background,
        "underlined cell keeps its top row background",
    );
    assert_eq!(
        pixel(1, height - 1),
        &background,
        "plain cell has no underline row",
    );
    assert_eq!(
        pixel(2, height / 2),
        &foreground,
        "struck cell paints its middle row",
    );
    assert_eq!(
        pixel(2, height - 1),
        &background,
        "struck cell has no underline row",
    );
}

#[test]
fn color_emoji_samples_the_color_atlas() {
    if !require_gpu() {
        eprintln!("skipped: no Vulkan 1.3 graphics device available");
        return;
    }
    // A color glyph must paint non-background texels through the RGBA path;
    // a monochrome-only pipeline would leave the cell background. The grid
    // must be two cells wide: a 1x1 grid replaces any wide glyph with
    // U+FFFD, which would exercise the mask path instead of the color one.
    let mut font = Font::load(None, 18.0).expect("load system monospace font");
    let metrics = font.metrics();
    // Touch the emoji first: fallback faces load on demand, so checking
    // `fallback_count` before any miss would always skip on a cold font.
    let (_, info, _) = font.glyph('😀', false, false).unwrap();
    if font.fallback_count() == 0 {
        eprintln!("no fallback faces installed; skipping color atlas check");
        return;
    }
    if !info.color {
        eprintln!("emoji face has no color raster on this system");
        return;
    }
    let mut terminal = Terminal::new(2, 1).expect("create terminal grid");
    terminal.process("😀".as_bytes());
    // Terminal-cell ownership: leading cell + wide spacer.
    assert_eq!(terminal.viewport_cell(0, 0).unwrap().content, 0x1f600);
    assert!(
        terminal
            .viewport_cell(0, 1)
            .unwrap()
            .flags()
            .contains(aura_core::CellFlags::WIDE_SPACER)
    );
    let mut renderer = OffscreenRenderer::new(metrics.width * 2, metrics.height)
        .expect("create offscreen renderer");
    let pixels = renderer
        .render(&mut terminal, &mut font)
        .expect("draw terminal and read back pixels");
    let painted = pixels.as_chunks::<4>().0.iter().any(|rgba| {
        let spread = rgba[0].max(rgba[1]).max(rgba[2]) - rgba[0].min(rgba[1]).min(rgba[2]);
        spread > 40
    });
    assert!(
        painted,
        "color emoji cell must contain chromatic (non-gray) pixels",
    );
}

#[test]
fn cursor_shapes_invert_expected_regions() {
    if !require_gpu() {
        eprintln!("skipped: no Vulkan 1.3 graphics device available");
        return;
    }
    // A cursor on a space cell inverts exactly the shape region and leaves
    // the rest of the cell background.
    let mut font = Font::load(None, 18.0).expect("load system monospace font");
    let metrics = font.metrics();
    let (width, height) = (metrics.width, metrics.height);
    let foreground = [216, 216, 216, 255];
    let background = [24, 24, 24, 255];
    for (sequence, check) in [
        ("\x1b[2 q", "block"),
        ("\x1b[4 q", "underline"),
        ("\x1b[6 q", "bar"),
    ] {
        let mut terminal = Terminal::new(1, 1).expect("create terminal grid");
        terminal.process(b" ");
        terminal.process(b"\r");
        terminal.process(sequence.as_bytes());
        let mut renderer =
            OffscreenRenderer::new(width, height).expect("create offscreen renderer");
        renderer.set_show_cursor(true);
        let pixels = renderer
            .render(&mut terminal, &mut font)
            .expect("draw terminal and read back pixels");
        let pixel = |x: u32, y: u32| {
            let index = ((y * width + x) * 4) as usize;
            &pixels[index..index + 4]
        };
        match check {
            "block" => {
                assert_eq!(pixel(0, 0), &foreground, "block cursor inverts cell");
                assert_eq!(
                    pixel(width - 1, height - 1),
                    &foreground,
                    "block cursor covers the cell corner",
                );
            }
            "underline" => {
                assert_eq!(
                    pixel(0, height - 1),
                    &foreground,
                    "underline cursor paints the bottom row",
                );
                assert_eq!(
                    pixel(0, 0),
                    &background,
                    "underline cursor keeps the top row",
                );
            }
            "bar" => {
                assert_eq!(pixel(0, 0), &foreground, "bar cursor paints the left edge");
                assert_eq!(
                    pixel(width - 1, 0),
                    &background,
                    "bar cursor keeps the right edge",
                );
            }
            _ => unreachable!("cursor shape matrix covers block, underline, and bar"),
        }
    }
}

#[test]
fn inverse_and_truecolor_cells_use_deterministic_colors() {
    if !require_gpu() {
        eprintln!("skipped: no Vulkan 1.3 graphics device available");
        return;
    }
    let mut font = Font::load(None, 18.0).expect("load system monospace font");
    let metrics = font.metrics();
    let mut terminal = Terminal::new(2, 1).expect("create terminal grid");
    terminal.process(b"\x1b[7m \x1b[27m\x1b[38;2;255;0;0mA");
    let mut renderer = OffscreenRenderer::new(metrics.width * 2, metrics.height)
        .expect("create offscreen renderer");
    let pixels = renderer
        .render(&mut terminal, &mut font)
        .expect("draw terminal and read back pixels");
    let pixel = |column: u32, x: u32, y: u32| {
        let index = ((y * metrics.width * 2 + column * metrics.width + x) * 4) as usize;
        &pixels[index..index + 4]
    };
    // Inverse swaps the default palette: background carries the foreground.
    assert_eq!(
        pixel(0, 0, 0),
        &[216, 216, 216, 255],
        "inverse cell background uses the foreground color",
    );
    // The truecolor A must show red-dominant glyph pixels somewhere.
    let red = (0..metrics.height).any(|y| {
        (0..metrics.width).any(|x| {
            let rgba = pixel(1, x, y);
            rgba[0] > 150 && rgba[1] < 100 && rgba[2] < 100
        })
    });
    assert!(red, "truecolor cell renders red glyph pixels");
}

#[test]
fn mixed_fallback_frame_renders_all_atlases() {
    if !require_gpu() {
        eprintln!("skipped: no Vulkan 1.3 graphics device available");
        return;
    }
    // ASCII (mask atlas, primary face), CJK (mask atlas, fallback face),
    // and emoji (color atlas) share one frame and one glyph table.
    // `A界😀` occupies exactly five terminal columns (1+2+2).
    let mut font = Font::load(None, 18.0).expect("load system monospace font");
    // Touch the fallback glyphs first: fallback faces load on demand, so a
    // cold `fallback_count` check would skip even when CJK/emoji exist.
    let _ = font.glyph('界', false, false);
    let _ = font.glyph('😀', false, false);
    if font.fallback_count() == 0 {
        eprintln!("no fallback faces installed; skipping mixed frame check");
        return;
    }
    let metrics = font.metrics();
    let mut terminal = Terminal::new(5, 1).expect("create terminal grid");
    terminal.process("A界😀".as_bytes());
    assert_eq!(terminal.viewport_cell(0, 1).unwrap().content, 0x754c);
    assert_eq!(terminal.viewport_cell(0, 3).unwrap().content, 0x1f600);
    let mut renderer = OffscreenRenderer::new(metrics.width * 5, metrics.height)
        .expect("create offscreen renderer");
    let pixels = renderer
        .render(&mut terminal, &mut font)
        .expect("draw terminal and read back pixels");
    let lit_in_column = |column: u32| {
        (0..metrics.height).any(|y| {
            (0..metrics.width).any(|x| {
                let index = ((y * metrics.width * 5 + column * metrics.width + x) * 4) as usize;
                let rgba = &pixels[index..index + 4];
                rgba[0] > 60 || rgba[1] > 60 || rgba[2] > 60
            })
        })
    };
    assert!(lit_in_column(0), "ASCII cell renders from the mask atlas");
    assert!(
        lit_in_column(1),
        "CJK fallback cell renders from the mask atlas"
    );
    assert!(lit_in_column(3), "emoji cell renders from the color atlas",);
}

#[test]
fn font_resize_repaints_ascii_cells_with_fresh_rasterization() {
    if !require_gpu() {
        eprintln!("skipped: no Vulkan 1.3 graphics device available");
        return;
    }
    // Ordinary narrow glyphs only: 12 -> 24 re-rasterizes (bright pixel
    // coverage grows with the cell) and returning to 12 reproduces the
    // original frame byte-for-byte — no size-24 atlas/cache may leak.
    let mut font = Font::load(None, 12.0).expect("load system monospace font");
    let mut terminal = Terminal::new(3, 1).expect("create terminal grid");
    terminal.process(b"ABC");

    let render = |font: &mut Font, terminal: &mut Terminal, columns: u32| {
        let metrics = font.metrics();
        let mut renderer = OffscreenRenderer::new(metrics.width * columns, metrics.height)
            .expect("create offscreen renderer");
        renderer
            .render(terminal, font)
            .expect("draw terminal and read back pixels")
    };
    let bright_in_cell = |pixels: &[u8], width: u32, height: u32, cell: u32| {
        let frame_width = width * 3;
        (0..height).any(|y| {
            (0..width).any(|x| {
                let index = ((y * frame_width + cell * width + x) * 4) as usize;
                let rgba = &pixels[index..index + 4];
                rgba[0] > 100 && rgba[1] > 100 && rgba[2] > 100
            })
        })
    };

    let small = font.metrics();
    let frame12 = render(&mut font, &mut terminal, 3);
    for cell in 0..3 {
        assert!(
            bright_in_cell(&frame12, small.width, small.height, cell),
            "ASCII cell {cell} renders at 12px"
        );
    }
    let (_, info12, _) = font.glyph('A', false, false).unwrap();

    assert!(font.set_size(24.0).unwrap(), "size changes 12 -> 24");
    let (_, info24, upload) = font.glyph('A', false, false).unwrap();
    assert!(
        upload.is_some(),
        "24px glyph is freshly rasterized, not scaled from the 12px texture"
    );
    assert!(
        info24.size[0] > info12.size[0] && info24.size[1] > info12.size[1],
        "24px raster is physically larger: {:?} vs {:?}",
        info12.size,
        info24.size
    );
    let large = font.metrics();
    let frame24 = render(&mut font, &mut terminal, 3);
    assert_eq!(
        frame24.len(),
        (large.width * 3 * large.height * 4) as usize,
        "frame follows the new cell geometry"
    );
    let lit = |pixels: &[u8], width: u32, height: u32| {
        (0..height)
            .flat_map(|y| (0..width * 3).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let index = ((y * width * 3 + x) * 4) as usize;
                let rgba = &pixels[index..index + 4];
                rgba[0] > 100 && rgba[1] > 100 && rgba[2] > 100
            })
            .count()
    };
    let lit12 = lit(&frame12, small.width, small.height);
    let lit24 = lit(&frame24, large.width, large.height);
    assert!(
        lit24 > lit12,
        "bigger rasterization paints more glyph pixels: 12px={lit12} 24px={lit24}"
    );
    for cell in 0..3 {
        assert!(
            bright_in_cell(&frame24, large.width, large.height, cell),
            "ASCII cell {cell} renders at 24px"
        );
    }

    // Return trip: back at 12 the frame must match the original exactly.
    assert!(font.set_size(12.0).unwrap(), "size returns to 12");
    let frame12b = render(&mut font, &mut terminal, 3);
    assert_eq!(
        frame12b, frame12,
        "12 -> 24 -> 12 must reproduce the original frame with no stale glyphs"
    );
}

#[test]
fn font_resize_rerasterizes_wide_cells_with_terminal_cell_ownership() {
    if !require_gpu() {
        eprintln!("skipped: no Vulkan 1.3 graphics device available");
        return;
    }
    // `A界B` occupies four terminal columns: A=1, 界=2 (leading + spacer),
    // B=1. Placement is asserted from terminal-cell ownership, never from
    // a one-character-per-column assumption.
    let mut font = Font::load(None, 12.0).expect("load system monospace font");
    let mut terminal = Terminal::new(4, 1).expect("create terminal grid");
    terminal.process("A界B".as_bytes());
    assert_eq!(terminal.viewport_cell(0, 0).unwrap().content, 'A' as u32);
    let flags = |column: usize| terminal.viewport_cell(0, column).unwrap().flags();
    assert!(flags(1).contains(aura_core::CellFlags::WIDE_LEADING));
    assert!(flags(2).contains(aura_core::CellFlags::WIDE_SPACER));
    assert_eq!(terminal.viewport_cell(0, 3).unwrap().content, 'B' as u32);

    let render = |font: &mut Font, terminal: &mut Terminal| {
        let metrics = font.metrics();
        let mut renderer = OffscreenRenderer::new(metrics.width * 4, metrics.height)
            .expect("create offscreen renderer");
        renderer
            .render(terminal, font)
            .expect("draw terminal and read back pixels")
    };
    let bright_in_cell = |pixels: &[u8], width: u32, height: u32, cell: u32| {
        let frame_width = width * 4;
        (0..height).any(|y| {
            (0..width).any(|x| {
                let index = ((y * frame_width + cell * width + x) * 4) as usize;
                let rgba = &pixels[index..index + 4];
                rgba[0] > 100 && rgba[1] > 100 && rgba[2] > 100
            })
        })
    };

    let small = font.metrics();
    let frame12 = render(&mut font, &mut terminal);
    assert!(
        bright_in_cell(&frame12, small.width, small.height, 0),
        "A at 12px"
    );
    assert!(
        bright_in_cell(&frame12, small.width, small.height, 1),
        "界 leading cell at 12px"
    );
    assert!(
        bright_in_cell(&frame12, small.width, small.height, 2),
        "界 is not clipped: its raster spans into the spacer column at 12px"
    );
    assert!(
        bright_in_cell(&frame12, small.width, small.height, 3),
        "B keeps its column after the wide pair at 12px"
    );

    assert!(font.set_size(24.0).unwrap(), "size changes 12 -> 24");
    let (_, cjk24, cjk_upload) = font.glyph('界', false, false).unwrap();
    assert!(
        cjk_upload.is_some(),
        "the fallback CJK face re-rasterizes at 24px"
    );
    assert!(cjk24.size[0] > 0.0 && cjk24.size[1] > 0.0);
    let large = font.metrics();
    let frame24 = render(&mut font, &mut terminal);
    assert_eq!(frame24.len(), (large.width * 4 * large.height * 4) as usize);
    assert!(
        bright_in_cell(&frame24, large.width, large.height, 0),
        "A at 24px"
    );
    assert!(
        bright_in_cell(&frame24, large.width, large.height, 1),
        "界 leading cell at 24px"
    );
    assert!(
        bright_in_cell(&frame24, large.width, large.height, 2),
        "界 is not clipped: its raster spans into the spacer column at 24px"
    );
    assert!(
        bright_in_cell(&frame24, large.width, large.height, 3),
        "B keeps its column after the wide pair at 24px"
    );

    assert!(font.set_size(12.0).unwrap(), "size returns to 12");
    let frame12b = render(&mut font, &mut terminal);
    assert_eq!(
        frame12b, frame12,
        "12 -> 24 -> 12 reproduces the original wide-cell frame exactly"
    );
}

#[test]
fn font_resize_repaints_color_emoji_from_fresh_color_atlas() {
    if !require_gpu() {
        eprintln!("skipped: no Vulkan 1.3 graphics device available");
        return;
    }
    // `A😀B` occupies four terminal columns (A=1, 😀=2, B=1). The emoji
    // must come from the RGBA atlas at BOTH sizes — a 12px atlas reused
    // at 24px would paint chromatic pixels at the wrong scale or not at all.
    let mut font = Font::load(None, 12.0).expect("load system monospace font");
    let (_, info, _) = font.glyph('😀', false, false).unwrap();
    if font.fallback_count() == 0 {
        eprintln!("no fallback faces installed; skipping emoji resize check");
        return;
    }
    if !info.color {
        eprintln!("emoji face has no color raster on this system");
        return;
    }
    let mut terminal = Terminal::new(4, 1).expect("create terminal grid");
    terminal.process("A😀B".as_bytes());
    assert_eq!(terminal.viewport_cell(0, 0).unwrap().content, 'A' as u32);
    assert_eq!(terminal.viewport_cell(0, 1).unwrap().content, 0x1f600);
    assert!(
        terminal
            .viewport_cell(0, 2)
            .unwrap()
            .flags()
            .contains(aura_core::CellFlags::WIDE_SPACER)
    );
    assert_eq!(terminal.viewport_cell(0, 3).unwrap().content, 'B' as u32);

    let render = |font: &mut Font, terminal: &mut Terminal| {
        let metrics = font.metrics();
        let mut renderer = OffscreenRenderer::new(metrics.width * 4, metrics.height)
            .expect("create offscreen renderer");
        renderer
            .render(terminal, font)
            .expect("draw terminal and read back pixels")
    };
    // Chromatic pixels anywhere in the emoji's two-column span (columns 1-2).
    let emoji_chroma = |pixels: &[u8], width: u32, height: u32| {
        let frame_width = width * 4;
        (0..height).any(|y| {
            (width..width * 3).any(|x| {
                let index = ((y * frame_width + x) * 4) as usize;
                let rgba = &pixels[index..index + 4];
                let spread = rgba[0].max(rgba[1]).max(rgba[2]) - rgba[0].min(rgba[1]).min(rgba[2]);
                spread > 40
            })
        })
    };

    let small = font.metrics();
    let frame12 = render(&mut font, &mut terminal);
    assert!(
        emoji_chroma(&frame12, small.width, small.height),
        "emoji paints chromatic pixels from the color atlas at 12px"
    );

    assert!(font.set_size(24.0).unwrap(), "size changes 12 -> 24");
    let (_, emoji24, emoji_upload) = font.glyph('😀', false, false).unwrap();
    assert!(emoji_upload.is_some(), "color glyph re-rasterizes at 24px");
    assert!(
        emoji24.color,
        "still served by the color atlas after resize"
    );
    let large = font.metrics();
    let frame24 = render(&mut font, &mut terminal);
    assert!(
        emoji_chroma(&frame24, large.width, large.height),
        "emoji paints chromatic pixels from a fresh color atlas at 24px"
    );

    assert!(font.set_size(12.0).unwrap(), "size returns to 12");
    let frame12b = render(&mut font, &mut terminal);
    assert_eq!(
        frame12b, frame12,
        "12 -> 24 -> 12 reproduces the original emoji frame exactly"
    );
}

#[test]
fn font_resize_keeps_combining_grapheme_in_one_cell() {
    if !require_gpu() {
        eprintln!("skipped: no Vulkan 1.3 graphics device available");
        return;
    }
    // `e` + U+0301 is a single terminal cell at every size; the composed
    // glyph must re-rasterize with the rest of the font, not vanish or
    // split when the size changes.
    let mut font = Font::load(None, 12.0).expect("load system monospace font");
    let mut terminal = Terminal::new(2, 1).expect("create terminal grid");
    terminal.process("e\u{301}B".as_bytes());
    assert_eq!(
        terminal.active_grid().grapheme(0, 0).as_deref(),
        Some("e\u{301}"),
        "combining sequence stays one semantic cell"
    );
    assert_eq!(terminal.viewport_cell(0, 1).unwrap().content, 'B' as u32);

    let render = |font: &mut Font, terminal: &mut Terminal| {
        let metrics = font.metrics();
        let mut renderer = OffscreenRenderer::new(metrics.width * 2, metrics.height)
            .expect("create offscreen renderer");
        renderer
            .render(terminal, font)
            .expect("draw terminal and read back pixels")
    };
    let bright_in_cell = |pixels: &[u8], width: u32, height: u32, cell: u32| {
        let frame_width = width * 2;
        (0..height).any(|y| {
            (0..width).any(|x| {
                let index = ((y * frame_width + cell * width + x) * 4) as usize;
                let rgba = &pixels[index..index + 4];
                rgba[0] > 100 && rgba[1] > 100 && rgba[2] > 100
            })
        })
    };

    let small = font.metrics();
    let frame12 = render(&mut font, &mut terminal);
    assert!(
        bright_in_cell(&frame12, small.width, small.height, 0),
        "composed é renders at 12px"
    );
    assert!(
        bright_in_cell(&frame12, small.width, small.height, 1),
        "B renders at 12px"
    );

    assert!(font.set_size(24.0).unwrap(), "size changes 12 -> 24");
    let large = font.metrics();
    let frame24 = render(&mut font, &mut terminal);
    assert!(
        bright_in_cell(&frame24, large.width, large.height, 0),
        "composed é renders at 24px"
    );
    assert!(
        bright_in_cell(&frame24, large.width, large.height, 1),
        "B renders at 24px"
    );
    assert_eq!(
        terminal.active_grid().grapheme(0, 0).as_deref(),
        Some("e\u{301}"),
        "grapheme sidecar survives the resize"
    );

    assert!(font.set_size(12.0).unwrap(), "size returns to 12");
    let frame12b = render(&mut font, &mut terminal);
    assert_eq!(
        frame12b, frame12,
        "12 -> 24 -> 12 reproduces the original combining-glyph frame exactly"
    );
}

#[test]
fn selection_uses_the_configured_palette_colour() {
    // Regression guard: the selection colour used to be a hand-swizzled
    // constant in the shader, so it rendered as brown rgb(144,96,64) instead of
    // the intended slate blue, and the configured palette was ignored.
    if !require_gpu() {
        return;
    }
    let mut font = Font::load(None, 18.0).expect("load system monospace font");
    let metrics = font.metrics();
    let (width, height) = (metrics.width * 3, metrics.height);
    let mut terminal = Terminal::new(3, 1).expect("create terminal grid");
    terminal.process(b"   ");
    terminal.set_selection(
        aura_core::SelectionPoint { line: 0, column: 1 },
        aura_core::SelectionPoint { line: 0, column: 1 },
    );
    let mut renderer = OffscreenRenderer::new(width, height).expect("create offscreen renderer");
    renderer.set_palette(aura_config::Palette::DARK);
    let pixels = renderer
        .render(&mut terminal, &mut font)
        .expect("draw terminal and read back pixels");
    let pixel = |x: u32, y: u32| {
        let index = ((y * width + x) * 4) as usize;
        [
            pixels[index],
            pixels[index + 1],
            pixels[index + 2],
            pixels[index + 3],
        ]
    };
    assert_eq!(
        pixel(metrics.width, 0),
        [0x40, 0x60, 0x90, 255],
        "selection cell must use the palette selection colour rgb(64,96,144)"
    );
    assert_eq!(
        pixel(0, 0),
        [0x18, 0x18, 0x18, 255],
        "unselected cells keep the plain background"
    );
}

#[test]
fn narrow_glyph_pixels_and_backgrounds_survive_padding_spacing_and_residue() {
    if !require_gpu() {
        return;
    }
    let mut font = Font::load(None, 18.0).unwrap();
    let metrics = font.metrics();
    let (cw, ch) = (metrics.width + 2, metrics.height + 3);
    let mut terminal = Terminal::new(3, 1).unwrap();
    terminal.process(b"\x1b[?25l\x1b[41mi\x1b[42mi\x1b[44mi");
    let mut first = OffscreenRenderer::new(cw * 3 + 6, ch + 10).unwrap();
    first.set_padding(3, 5);
    first.set_cell_spacing(2, 3);
    let a = first.render(&mut terminal, &mut font).unwrap();
    let mut resized = OffscreenRenderer::new(cw * 3 + 13, ch + 21).unwrap();
    resized.set_padding(3, 5);
    resized.set_cell_spacing(2, 3);
    let b = resized.render(&mut terminal, &mut font).unwrap();
    let pixel = |data: &[u8], width: u32, x: u32, y: u32| -> [u8; 4] {
        let offset = ((y * width + x) * 4) as usize;
        data[offset..offset + 4].try_into().unwrap()
    };
    assert_eq!(pixel(&b, cw * 3 + 13, 0, 0), [24, 24, 24, 255]);
    assert_eq!(pixel(&b, cw * 3 + 13, cw * 3 + 12, 5), [24, 24, 24, 255]);
    for y in 0..ch {
        for x in 0..cw * 3 {
            assert_eq!(
                pixel(&a, cw * 3 + 6, x + 3, y + 5),
                pixel(&b, cw * 3 + 13, x + 3, y + 5)
            );
        }
    }
    for (col, color) in [[205, 0, 0, 255], [0, 205, 0, 255], [0, 0, 238, 255]]
        .iter()
        .enumerate()
    {
        assert_eq!(
            pixel(&b, cw * 3 + 13, 3 + col as u32 * cw, 5 + ch - 1),
            *color
        );
    }
    assert!(
        (0..ch).any(|y| (0..cw).any(|x| {
            let rgba = pixel(&b, cw * 3 + 13, x + 3, y + 5);
            rgba[1] > 100 && rgba[2] > 100
        })),
        "i has visible glyph pixels"
    );
}

#[test]
fn thin_cursors_preserve_the_glyph_outside_the_cursor_stripe() {
    if !require_gpu() {
        return;
    }
    let mut font = Font::load(None, 18.0).unwrap();
    let m = font.metrics();
    let mut terminal = Terminal::new(1, 1).unwrap();
    terminal.process(b"i\r\x1b[?25l");
    let mut renderer = OffscreenRenderer::new(m.width, m.height).unwrap();
    renderer.set_show_cursor(true);
    let hidden = renderer.render(&mut terminal, &mut font).unwrap();
    let thick = (m.height >> 4).max(1) * 2;
    for (sequence, bar) in [
        (b"\x1b[?25h\x1b[3 q".as_slice(), false),
        (b"\x1b[5 q".as_slice(), true),
    ] {
        terminal.process(sequence);
        let visible = renderer.render(&mut terminal, &mut font).unwrap();
        assert_ne!(hidden, visible, "cursor must paint a stripe");
        for y in 0..m.height {
            for x in 0..m.width {
                if (bar && x >= thick) || (!bar && y < m.height - thick) {
                    let offset = ((y * m.width + x) * 4) as usize;
                    assert_eq!(
                        hidden[offset..offset + 4],
                        visible[offset..offset + 4],
                        "thin cursor changed glyph at ({x}, {y})"
                    );
                }
            }
        }
    }
}

#[test]
fn a_long_sessions_full_atlas_is_rebuilt_for_visible_glyphs() {
    if !require_gpu() {
        return;
    }
    let mut font = Font::load(None, 72.0).unwrap();
    let mut exhausted = false;
    for scalar in 0x2500..=0x2bff {
        if let Err(error) = font.glyph(char::from_u32(scalar).unwrap(), false, false) {
            assert_eq!(error, aura_text::CACHE_FULL);
            exhausted = true;
            break;
        }
    }
    assert!(exhausted, "fixture must fill the atlas before drawing i");
    let m = font.metrics();
    let mut terminal = Terminal::new(1, 1).unwrap();
    terminal.process(b"i\x1b[?25l");
    let mut renderer = OffscreenRenderer::new(m.width, m.height).unwrap();
    let pixels = renderer.render(&mut terminal, &mut font).unwrap();
    assert!(
        pixels
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[0] > 150 && p[1] > 150 && p[2] > 150),
        "i must recover after atlas exhaustion"
    );
    let (_, info, _) = font.glyph('i', false, false).unwrap();
    assert!(info.size[0] > 0.0 && info.size[1] > 0.0);
    font.set_size(71.0).unwrap();
    renderer.invalidate_atlas();
    assert!(
        renderer
            .render(&mut terminal, &mut font)
            .unwrap()
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[0] > 150)
    );
}

#[test]
fn complete_emoji_clusters_reach_the_color_shader_and_preserve_cell_ownership() {
    if !require_gpu() {
        return;
    }
    let mut font = Font::load(None, 18.0).unwrap();
    if !font
        .glyph_text("👩\u{200d}💻", false, false)
        .unwrap()
        .1
        .color
    {
        eprintln!("skipped: color emoji font unavailable");
        return;
    }
    let (cw, ch) = (font.metrics().width, font.metrics().height);
    for text in ["👩\u{200d}💻", "👍🏽", "🇪🇸", "❤\u{fe0f}"] {
        let mut terminal = Terminal::new(3, 1).unwrap();
        terminal.process(b"\x1b[?25l");
        terminal.process(text.as_bytes());
        terminal.process(b"\x1b[44m ");
        assert!(
            terminal
                .viewport_cell(0, 1)
                .unwrap()
                .flags()
                .contains(aura_core::CellFlags::WIDE_SPACER)
        );
        let width = cw * 3;
        let mut renderer = OffscreenRenderer::new(width, ch).unwrap();
        let pixels = renderer.render(&mut terminal, &mut font).unwrap();
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .take((cw * 2) as usize)
                .any(|p| p[0] != p[1])
                || (0..ch).any(|y| (0..cw * 2).any(|x| {
                    let offset = ((y * width + x) * 4) as usize;
                    let p = &pixels[offset..offset + 4];
                    p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2]) > 20
                })),
            "{text} did not reach the color shader"
        );
        for y in 0..ch {
            for x in cw * 2..width {
                let offset = ((y * width + x) * 4) as usize;
                assert_eq!(
                    &pixels[offset..offset + 4],
                    [0, 0, 238, 255],
                    "{text} painted its neighbor"
                );
            }
        }
    }
}
