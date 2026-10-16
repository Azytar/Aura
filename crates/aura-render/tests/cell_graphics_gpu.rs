use aura_core::Terminal;
use aura_render::OffscreenRenderer;
use aura_text::Font;

fn gpu_available() -> bool {
    match OffscreenRenderer::new(1, 1) {
        Ok(_) => true,
        Err(error) => {
            eprintln!("skipped: Vulkan device unavailable: {error}");
            false
        }
    }
}

fn pixel(pixels: &[u8], width: u32, x: u32, y: u32) -> u8 {
    pixels[((y * width + x) * 4) as usize]
}

#[test]
fn every_braille_pattern_keeps_unicode_dot_order_across_sizes_and_spacing() {
    if !gpu_available() {
        return;
    }
    let text: String = (0x2800..=0x28ff)
        .map(|c| char::from_u32(c).unwrap())
        .collect();
    let mut terminal = Terminal::new(16, 16).unwrap();
    terminal.process(b"\x1b[?25l");
    terminal.process(text.as_bytes());
    let dots = [
        (0, 0),
        (0, 1),
        (0, 2),
        (1, 0),
        (1, 1),
        (1, 2),
        (0, 3),
        (1, 3),
    ];
    for family in ["DejaVu Sans Mono", "Noto Sans Mono"] {
        for size in [6.0, 9.0, 14.0, 18.0, 24.0, 36.0, 72.0] {
            for spacing in [0, 3] {
                let mut font = Font::load_with_family(family, size).unwrap();
                let (cw, ch) = (
                    font.metrics().width + spacing,
                    font.metrics().height + spacing,
                );
                let width = cw * 16 + 6;
                let mut renderer = OffscreenRenderer::new(width, ch * 16 + 10).unwrap();
                renderer.set_padding(3, 5);
                renderer.set_cell_spacing(spacing as i32, spacing as i32);
                renderer.set_glyph_offset(9, -7);
                let pixels = renderer.render(&mut terminal, &mut font).unwrap();
                for pattern in 0..256u32 {
                    for (bit, (x, y)) in dots.iter().enumerate() {
                        let px = 3 + pattern % 16 * cw + ((2 * x + 1) * cw / 4);
                        let py = 5 + pattern / 16 * ch + ((2 * y + 1) * ch / 8);
                        let brightness = pixel(&pixels, width, px, py);
                        assert_eq!(
                            brightness > 44,
                            pattern & (1 << bit) != 0,
                            "family={family}, size={size}, spacing={spacing}, pattern={pattern:#x}, bit={bit}, pixel={brightness}"
                        );
                    }
                    assert_eq!(
                        terminal
                            .viewport_cell((pattern / 16) as usize, (pattern % 16) as usize)
                            .unwrap()
                            .content,
                        0x2800 + pattern
                    );
                }
                assert_eq!(
                    font.fallback_count(),
                    0,
                    "cell graphics must bypass font lookup"
                );
                assert_eq!(pixel(&pixels, width, 0, 0), 24);
            }
        }
    }
}

#[test]
fn braille_centers_form_one_uniform_grid_across_cell_boundaries() {
    if !gpu_available() {
        return;
    }
    let mut terminal = Terminal::new(3, 3).unwrap();
    terminal.process(b"\x1b[?25l");
    terminal.process("⣿⣿⣿⣿⣿⣿⣿⣿⣿".as_bytes());
    for size in [9.0, 14.0, 18.0, 24.0] {
        let mut font = Font::load(None, size).unwrap();
        let cw = font.metrics().width + 1;
        let ch = font.metrics().height + 3;
        let width = cw * 3;
        let mut renderer = OffscreenRenderer::new(width, ch * 3).unwrap();
        renderer.set_cell_spacing(1, 3);
        let pixels = renderer.render(&mut terminal, &mut font).unwrap();
        for row in 0..12 {
            for col in 0..6 {
                let mut mass = 0.0;
                let mut center = [0.0; 2];
                for y in row * ch / 4..(row + 1) * ch / 4 {
                    for x in col * cw / 2..(col + 1) * cw / 2 {
                        let ink = (pixel(&pixels, width, x, y) - 24) as f64;
                        mass += ink;
                        center[0] += (x as f64 + 0.5) * ink;
                        center[1] += (y as f64 + 0.5) * ink;
                    }
                }
                assert!(mass > 0.0);
                let expected = [
                    (col as f64 + 0.5) * cw as f64 / 2.0,
                    (row as f64 + 0.5) * ch as f64 / 4.0,
                ];
                for axis in 0..2 {
                    assert!(
                        (center[axis] / mass - expected[axis]).abs() < 0.15,
                        "dot center drifts at size={size}, row={row}, col={col}, axis={axis}"
                    );
                }
            }
        }
    }
}

#[test]
fn blocks_cover_full_strides_and_fractional_halves_are_complementary() {
    if !gpu_available() {
        return;
    }
    let mut font = Font::load(None, 14.0).unwrap();
    let cw = font.metrics().width + 2;
    let ch = font.metrics().height + 2;
    let width = cw * 5;
    let mut terminal = Terminal::new(5, 1).unwrap();
    terminal.process("\x1b[?25l█▀▄▌▐".as_bytes());
    let mut renderer = OffscreenRenderer::new(width, ch).unwrap();
    renderer.set_cell_spacing(2, 2);
    renderer.set_glyph_offset(-8, 12);
    let pixels = renderer.render(&mut terminal, &mut font).unwrap();
    for y in 0..ch {
        for x in 0..cw {
            assert_eq!(pixel(&pixels, width, x, y), 216);
            for (a, b) in [(1, 2), (3, 4)] {
                let sum = u16::from(pixel(&pixels, width, a * cw + x, y))
                    + u16::from(pixel(&pixels, width, b * cw + x, y));
                assert!(
                    (239..=241).contains(&sum),
                    "half-block area is lost at ({x},{y}): {sum}"
                );
            }
        }
    }
}

#[test]
fn light_heavy_and_double_lines_join_without_dim_center_pixels() {
    if !gpu_available() {
        return;
    }
    let mut font = Font::load(None, 14.0).unwrap();
    let (cw, ch) = (font.metrics().width, font.metrics().height);
    let width = cw * 3;
    for text in ["───", "━━━", "═══"] {
        let mut terminal = Terminal::new(3, 1).unwrap();
        terminal.process(b"\x1b[?25l");
        terminal.process(text.as_bytes());
        let mut renderer = OffscreenRenderer::new(width, ch).unwrap();
        let pixels = renderer.render(&mut terminal, &mut font).unwrap();
        for y in 0..ch {
            for x in 1..width {
                assert_eq!(
                    pixel(&pixels, width, x, y),
                    pixel(&pixels, width, 0, y),
                    "line {text} has a seam at ({x},{y})"
                );
            }
        }
    }
    for text in ["│\r\n│\r\n│", "┃\r\n┃\r\n┃", "║\r\n║\r\n║"] {
        let mut terminal = Terminal::new(1, 3).unwrap();
        terminal.process(b"\x1b[?25l");
        terminal.process(text.as_bytes());
        let mut renderer = OffscreenRenderer::new(cw, ch * 3).unwrap();
        let pixels = renderer.render(&mut terminal, &mut font).unwrap();
        for x in 0..cw {
            for y in 1..ch * 3 {
                assert_eq!(
                    pixel(&pixels, cw, x, y),
                    pixel(&pixels, cw, x, 0),
                    "vertical seam at ({x},{y})"
                );
            }
        }
    }
}

#[test]
fn graphics_ignore_font_style_but_keep_sgr_colors_and_inverse() {
    if !gpu_available() {
        return;
    }
    let mut font = Font::load(None, 18.0).unwrap();
    let (cw, ch) = (font.metrics().width, font.metrics().height);
    let width = cw * 8;
    let mut terminal = Terminal::new(8, 1).unwrap();
    terminal.process("\x1b[?25l⣿█─╬\x1b[1;3m⣿█─╬".as_bytes());
    let mut renderer = OffscreenRenderer::new(width, ch).unwrap();
    let pixels = renderer.render(&mut terminal, &mut font).unwrap();
    for y in 0..ch {
        for x in 0..cw * 4 {
            assert_eq!(
                pixel(&pixels, width, x, y),
                pixel(&pixels, width, x + cw * 4, y)
            );
        }
    }
    terminal.process("\r\x1b[0;31m█\x1b[7m█".as_bytes());
    let pixels = renderer.render(&mut terminal, &mut font).unwrap();
    assert_eq!(pixel(&pixels, width, 0, 0), 205);
    assert_eq!(pixel(&pixels, width, cw, 0), 24);
}

#[test]
fn every_box_drawing_character_paints_and_quadrants_keep_their_area() {
    if !gpu_available() {
        return;
    }
    let mut font = Font::load(None, 18.0).unwrap();
    let (cw, ch) = (font.metrics().width, font.metrics().height);
    let width = cw * 16;
    let mut terminal = Terminal::new(16, 8).unwrap();
    terminal.process(b"\x1b[?25l");
    let boxes: String = (0x2500..=0x257f)
        .map(|c| char::from_u32(c).unwrap())
        .collect();
    terminal.process(boxes.as_bytes());
    let mut renderer = OffscreenRenderer::new(width, ch * 8).unwrap();
    let pixels = renderer.render(&mut terminal, &mut font).unwrap();
    for i in 0..128 {
        assert!(
            (0..ch).any(|y| (0..cw).any(|x| pixel(
                &pixels,
                width,
                i % 16 * cw + x,
                i / 16 * ch + y
            ) > 44)),
            "box drawing U+{:04X} disappeared",
            0x2500 + i
        );
    }
    let mut terminal = Terminal::new(10, 1).unwrap();
    terminal.process(b"\x1b[?25l");
    let quadrants: String = (0x2596..=0x259f)
        .map(|c| char::from_u32(c).unwrap())
        .collect();
    terminal.process(quadrants.as_bytes());
    let width = cw * 10;
    let mut renderer = OffscreenRenderer::new(width, ch).unwrap();
    let pixels = renderer.render(&mut terminal, &mut font).unwrap();
    for (i, mask) in [4u32, 8, 1, 13, 9, 7, 11, 2, 6, 14].into_iter().enumerate() {
        let ink: u32 = (0..ch)
            .flat_map(|y| (0..cw).map(move |x| (x, y)))
            .map(|(x, y)| u32::from(pixel(&pixels, width, i as u32 * cw + x, y)) - 24)
            .sum();
        let expected = cw * ch * 192 * mask.count_ones() / 4;
        assert!(
            ink.abs_diff(expected) <= cw * ch,
            "quadrant mask {mask} area changed"
        );
        for quadrant in 0..4 {
            let x = i as u32 * cw + (quadrant % 2 * 2 + 1) * cw / 4;
            let y = (quadrant / 2 * 2 + 1) * ch / 4;
            assert_eq!(
                pixel(&pixels, width, x, y) > 44,
                mask & (1 << quadrant) != 0
            );
        }
    }
}

#[test]
fn double_corners_preserve_two_separate_connected_strokes() {
    if !gpu_available() {
        return;
    }
    for size in [9.0, 14.0, 18.0, 24.0, 36.0, 72.0] {
        let mut font = Font::load(None, size).unwrap();
        let (cw, ch) = (font.metrics().width, font.metrics().height);
        for character in ['╔', '╗', '╚', '╝'] {
            let mut terminal = Terminal::new(1, 1).unwrap();
            terminal.process(format!("\x1b[?25l{character}").as_bytes());
            let mut renderer = OffscreenRenderer::new(cw, ch).unwrap();
            let pixels = renderer.render(&mut terminal, &mut font).unwrap();
            let mut unseen: Vec<bool> = pixels
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| p[0] > 44)
                .collect();
            let mut components = Vec::new();
            for start in 0..unseen.len() {
                if !unseen[start] {
                    continue;
                }
                unseen[start] = false;
                let mut stack = vec![start];
                let mut count = 0;
                while let Some(index) = stack.pop() {
                    count += 1;
                    let x = index as u32 % cw;
                    let y = index as u32 / cw;
                    for (nx, ny) in [
                        (x.wrapping_sub(1), y),
                        (x + 1, y),
                        (x, y.wrapping_sub(1)),
                        (x, y + 1),
                    ] {
                        if nx >= cw || ny >= ch {
                            continue;
                        }
                        let next = (ny * cw + nx) as usize;
                        if unseen[next] {
                            unseen[next] = false;
                            stack.push(next);
                        }
                    }
                }
                components.push(count);
            }
            assert_eq!(
                components.len(),
                2,
                "{character} at size={size} has broken strokes or a closed double-line gap: {components:?}"
            );
            assert!(components.iter().all(|&count| count > cw as usize / 2));
        }
    }
}

#[test]
fn graphical_cells_survive_zoom_and_selection_preserves_original_text() {
    if !gpu_available() {
        return;
    }
    let mut font = Font::load(None, 14.0).unwrap();
    let mut terminal = Terminal::new(4, 1).unwrap();
    terminal.process("\x1b[?25l⣇█╬i".as_bytes());
    let mut renderer = OffscreenRenderer::new(160, 80).unwrap();
    let original = renderer.render(&mut terminal, &mut font).unwrap();
    font.set_size(24.0).unwrap();
    renderer.invalidate_atlas();
    renderer.render(&mut terminal, &mut font).unwrap();
    font.set_size(14.0).unwrap();
    renderer.invalidate_atlas();
    assert_eq!(renderer.render(&mut terminal, &mut font).unwrap(), original);
    terminal.set_selection(
        aura_core::SelectionPoint { line: 0, column: 0 },
        aura_core::SelectionPoint { line: 0, column: 3 },
    );
    renderer.render(&mut terminal, &mut font).unwrap();
    assert_eq!(terminal.selection_text().as_deref(), Some("⣇█╬i"));
    terminal.clear_selection();
    assert_eq!(renderer.render(&mut terminal, &mut font).unwrap(), original);
}
