use aterm_core::Terminal;
use aterm_font::Font;
use aterm_render::OffscreenRenderer;

#[test]
#[ignore = "requires an accessible Vulkan 1.3 graphics device"]
fn renders_terminal_cells_and_reads_back_expected_colors() {
    let mut font = Font::load(None, 18.0).expect("load system monospace font");
    let metrics = font.metrics();
    let mut terminal = Terminal::new(3, 2).expect("create terminal grid");
    terminal.process(b"\x1b[41mA\x1b[42mB\x1b[44mC\x1b[0m\r\n123");
    assert_eq!(
        terminal.viewport_cell(1, 0).unwrap().bg,
        aterm_core::DEFAULT_COLOR
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
#[ignore = "requires an accessible Vulkan 1.3 graphics device"]
fn glyph_cache_hits_keep_later_table_entries() {
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
#[ignore = "requires an accessible Vulkan 1.3 graphics device"]
fn underline_and_strikethrough_paint_foreground_rows() {
    // Spaces isolate decorations from glyph pixels: the bottom row of an
    // underlined cell and the middle row of a struck cell must carry the
    // foreground color while plain cells stay background.
    let mut font = Font::load(None, 18.0).expect("load system monospace font");
    let metrics = font.metrics();
    let height = metrics.height;
    let mut terminal = Terminal::new(3, 1).expect("create terminal grid");
    terminal.process(b"\x1b[4m \x1b[24m \x1b[9m ");
    let mut renderer = OffscreenRenderer::new(metrics.width * 3, height)
        .expect("create offscreen renderer");
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
#[ignore = "requires an accessible Vulkan 1.3 graphics device"]
fn color_emoji_samples_the_color_atlas() {
    // A color glyph must paint non-background texels through the RGBA path;
    // a monochrome-only pipeline would leave the cell background.
    let mut font = Font::load(None, 18.0).expect("load system monospace font");
    let metrics = font.metrics();
    if font.fallback_count() == 0 {
        eprintln!("no fallback faces installed; skipping color atlas check");
        return;
    }
    let (_, info, _) = font.glyph('😀', false, false).unwrap();
    if !info.color {
        eprintln!("emoji face has no color raster on this system");
        return;
    }
    let mut terminal = Terminal::new(1, 1).expect("create terminal grid");
    terminal.process("😀".as_bytes());
    let mut renderer = OffscreenRenderer::new(metrics.width, metrics.height)
        .expect("create offscreen renderer");
    let pixels = renderer
        .render(&mut terminal, &mut font)
        .expect("draw terminal and read back pixels");
    let painted = pixels.chunks_exact(4).any(|rgba| {
        rgba[0] != 24 || rgba[1] != 24 || rgba[2] != 24
    });
    assert!(
        painted,
        "color emoji cell must contain non-background pixels",
    );
}

#[test]
#[ignore = "requires an accessible Vulkan 1.3 graphics device"]
fn cursor_shapes_invert_expected_regions() {
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
#[ignore = "requires an accessible Vulkan 1.3 graphics device"]
fn inverse_and_truecolor_cells_use_deterministic_colors() {
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
#[ignore = "requires an accessible Vulkan 1.3 graphics device"]
fn mixed_fallback_frame_renders_all_atlases() {
    // ASCII (mask atlas, primary face), CJK (mask atlas, fallback face),
    // and emoji (color atlas) share one frame and one glyph table.
    let mut font = Font::load(None, 18.0).expect("load system monospace font");
    if font.fallback_count() == 0 {
        eprintln!("no fallback faces installed; skipping mixed frame check");
        return;
    }
    let metrics = font.metrics();
    let mut terminal = Terminal::new(5, 1).expect("create terminal grid");
    terminal.process("A界😀".as_bytes());
    let mut renderer = OffscreenRenderer::new(metrics.width * 5, metrics.height)
        .expect("create offscreen renderer");
    let pixels = renderer
        .render(&mut terminal, &mut font)
        .expect("draw terminal and read back pixels");
    let lit_in_column = |column: u32| {
        (0..metrics.height).any(|y| {
            (0..metrics.width).any(|x| {
                let index =
                    ((y * metrics.width * 5 + column * metrics.width + x) * 4) as usize;
                let rgba = &pixels[index..index + 4];
                rgba[0] > 60 || rgba[1] > 60 || rgba[2] > 60
            })
        })
    };
    assert!(lit_in_column(0), "ASCII cell renders from the mask atlas");
    assert!(lit_in_column(1), "CJK fallback cell renders from the mask atlas");
    assert!(
        lit_in_column(3),
        "emoji cell renders from the color atlas",
    );
}
