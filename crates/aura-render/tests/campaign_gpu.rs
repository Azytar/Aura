use aura_core::Terminal;
use aura_render::OffscreenRenderer;
use aura_text::Font;
use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn required() -> bool {
    env::var("AURA_REQUIRE_GPU").as_deref() == Ok("1")
}
fn font(size: f32) -> Option<Font> {
    let path = Path::new("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf");
    match Font::load(Some(path), size) {
        Ok(font) => Some(font),
        Err(error) => {
            assert!(!required(), "required font {}: {error}", path.display());
            eprintln!("SKIP font {}: {error}", path.display());
            None
        }
    }
}
fn renderer(w: u32, h: u32) -> Option<OffscreenRenderer> {
    match OffscreenRenderer::new(w, h) {
        Ok(r) => Some(r),
        Err(error) => {
            assert!(!required(), "required Vulkan: {error}");
            eprintln!("SKIP Vulkan: {error}");
            None
        }
    }
}
fn ppm(path: &Path, w: u32, h: u32, rgba: &[u8]) {
    let mut data = format!("P6\n{w} {h}\n255\n").into_bytes();
    for pixel in rgba.chunks_exact(4) {
        data.extend_from_slice(&pixel[..3]);
    }
    fs::write(path, data).unwrap();
}
fn evidence(name: &str, w: u32, h: u32, expected: &[u8], actual: &[u8]) {
    let root =
        PathBuf::from(env::var("AURA_ARTIFACTS").unwrap_or_else(|_| "target/campaign".into()));
    fs::create_dir_all(&root).unwrap();
    ppm(&root.join(format!("{name}-expected.ppm")), w, h, expected);
    ppm(&root.join(format!("{name}-actual.ppm")), w, h, actual);
    let diff: Vec<_> = expected
        .iter()
        .zip(actual)
        .map(|(a, b)| a.abs_diff(*b))
        .collect();
    ppm(&root.join(format!("{name}-diff.ppm")), w, h, &diff);
}

#[test]
fn unicode_named_braille_witnesses_and_components() {
    let assignments: Vec<_> = include_str!("../../../tests/campaign/braille-dots.txt")
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| {
            let mut parts = line.split_whitespace();
            let scalar = u32::from_str_radix(parts.next().unwrap(), 16).unwrap();
            (scalar, parts.next().unwrap().to_owned())
        })
        .collect();
    assert_eq!(assignments.len(), 256);
    let text: String = assignments
        .iter()
        .map(|(c, _)| char::from_u32(*c).unwrap())
        .collect();
    for size in [17.0, 18.0, 23.0] {
        for (letters, lines, offset) in [(0, 0, -32), (1, 3, 32), (16, 32, -32)] {
            let Some(mut font) = font(size) else {
                return;
            };
            let (cw, ch) = (
                font.metrics().width + letters,
                font.metrics().height + lines,
            );
            let (w, h) = (cw * 16, ch * 16);
            let Some(mut renderer) = renderer(w, h) else {
                return;
            };
            renderer.set_cell_spacing(letters as i32, lines as i32);
            renderer.set_glyph_offset(offset, offset);
            let mut t = Terminal::new(16, 16).unwrap();
            t.process(b"\x1b[?25l");
            t.process(text.as_bytes());
            let pixels = renderer.render(&mut t, &mut font).unwrap();
            let mut expected = pixels.clone();
            let mut failures = Vec::new();
            for (index, (_, dots)) in assignments.iter().enumerate() {
                // Read Unicode's dot names, then place them using the named
                // four-row paper diagram 14/25/36/78, not shader bit indices.
                for (row, label) in ["14", "25", "36", "78"].iter().enumerate() {
                    for (col, digit) in label.chars().enumerate() {
                        let x = (index as u32 % 16) * cw + (2 * col as u32 + 1) * cw / 4;
                        let y = (index as u32 / 16) * ch + (2 * row as u32 + 1) * ch / 8;
                        let at = ((y * w + x) * 4) as usize;
                        let on = dots.contains(digit);
                        expected[at..at + 3].fill(if on { 216 } else { 24 });
                        if (pixels[at] > 128) != on {
                            failures.push(format!("pattern={index} dot={digit}"));
                        }
                    }
                }
                // Connected-component topology is independent of dot-center
                // sampling and detects unexpected extra bridges or dots.
                let mut ink: Vec<bool> = (0..cw * ch)
                    .map(|p| {
                        let x = index as u32 % 16 * cw + p % cw;
                        let y = index as u32 / 16 * ch + p / cw;
                        pixels[((y * w + x) * 4) as usize] > 128
                    })
                    .collect();
                let mut components = 0;
                for start in 0..ink.len() {
                    if !ink[start] {
                        continue;
                    }
                    components += 1;
                    ink[start] = false;
                    let mut stack = vec![start];
                    while let Some(p) = stack.pop() {
                        let x = p % cw as usize;
                        let y = p / cw as usize;
                        for (nx, ny) in [
                            (x.wrapping_sub(1), y),
                            (x + 1, y),
                            (x, y.wrapping_sub(1)),
                            (x, y + 1),
                        ] {
                            if nx < cw as usize && ny < ch as usize {
                                let q = ny * cw as usize + nx;
                                if ink[q] {
                                    ink[q] = false;
                                    stack.push(q);
                                }
                            }
                        }
                    }
                }
                let count = if dots == "0" { 0 } else { dots.len() };
                if components != count {
                    failures.push(format!(
                        "pattern={index} components={components} expected={count}"
                    ));
                }
            }
            if !failures.is_empty() {
                evidence("braille-witnesses", w, h, &expected, &pixels);
                panic!(
                    "size={size} cell={cw}x{ch} offsets={offset}: {}",
                    failures.join(",")
                );
            }
        }
    }
    eprintln!(
        "COMPLETE GPU Unicode witnesses: 256 patterns x 9 cell/spacing/offset configurations"
    );
}

#[test]
fn cached_atlas_render_matches_a_new_renderer_and_font() {
    for size in [17.0, 23.0] {
        let Some(mut font) = font(size) else {
            return;
        };
        let (w, h) = (font.metrics().width * 8 + 8, font.metrics().height * 3 + 10);
        let Some(mut cached) = renderer(w, h) else {
            return;
        };
        cached.set_padding(4, 5);
        let initial = b"\x1b[?25l\x1b[31mABCD\r\n\x1b[44m12345678";
        let updates = "\x1b[1;2H界\x1b[2;3H\x1b[3X\x1b[0m\x1b[3;1H⣇█╬i\x1b[4 q\x1b[?25h".as_bytes();
        let mut t = Terminal::new(8, 3).unwrap();
        t.process(initial);
        cached.render(&mut t, &mut font).unwrap();
        t.process(updates);
        let actual = cached.render(&mut t, &mut font).unwrap();
        // These are distinct allocations: no grid, glyph table, atlas image,
        // layout flag or Font cache can be inherited by the reference.
        let mut fresh_font = Font::load(
            Some(Path::new(
                "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
            )),
            size,
        )
        .unwrap();
        let mut fresh = OffscreenRenderer::new(w, h).unwrap();
        fresh.set_padding(4, 5);
        let mut reference = Terminal::new(8, 3).unwrap();
        reference.process(initial);
        reference.process(updates);
        let expected = fresh.render(&mut reference, &mut fresh_font).unwrap();
        if actual != expected {
            evidence("atlas-cached-fresh", w, h, &expected, &actual);
            panic!("cached/fresh pixels differ at size={size}");
        }
    }
    eprintln!("COMPLETE GPU atlas cached/fresh comparisons: 2 controlled-font sizes");
}
