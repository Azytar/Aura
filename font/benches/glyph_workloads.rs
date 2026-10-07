use std::hint::black_box;
use std::time::Instant;

use aterm_font::Font;

fn measure(name: &str, iterations: usize, mut operation: impl FnMut()) {
    let start = Instant::now();
    for _ in 0..iterations {
        operation();
    }
    println!(
        "{name}: {:.1} us/iteration",
        start.elapsed().as_secs_f64() * 1_000_000.0 / iterations as f64
    );
}

fn main() {
    let mut cached = Font::load(None, 16.0).expect("system monospace font is available");
    cached
        .glyph('A', false, false)
        .expect("rasterize cache seed");
    measure("glyph cache hit", 50_000, || {
        black_box(
            cached
                .glyph(black_box('A'), false, false)
                .expect("cached glyph"),
        );
    });

    let mut shaped_cache = Font::load(None, 16.0).expect("system monospace font is available");
    shaped_cache
        .glyph_text("e\u{301}", false, false)
        .expect("shape cache seed");
    measure("shaped grapheme cache hit", 20_000, || {
        black_box(
            shaped_cache
                .glyph_text("e\u{301}", false, false)
                .expect("cached shaped glyph"),
        );
    });

    measure("rasterize 94 distinct ASCII glyphs", 12, || {
        let mut font = Font::load(None, 16.0).expect("system monospace font is available");
        for ch in (b'!'..=b'~').map(char::from) {
            black_box(font.glyph(ch, false, false).expect("rasterize glyph"));
        }
    });

    measure("shape and rasterize Arabic grapheme", 24, || {
        let mut font = Font::load(None, 18.0).expect("system monospace font is available");
        black_box(
            font.glyph_text_with_context("م", "سل", "ن", false, false)
                .expect("shape contextual Arabic glyph"),
        );
    });

    // Each distinct font size must produce its own metrics and atlas; the
    // workload below measures the cost of switching sizes so accidental
    // atlas reuse is visible in the numbers.
    let sizes = [10.0, 12.0, 14.0, 16.0, 18.0, 20.0, 24.0];
    measure(
        "load and rasterize A at each tested size",
        sizes.len(),
        || {
            for &size in &sizes {
                let mut font = Font::load(None, size).expect("system monospace font is available");
                black_box(
                    font.glyph('A', false, false)
                        .expect("rasterize glyph at tested size"),
                );
            }
        },
    );

    measure("font metrics computation per size", sizes.len(), || {
        for &size in &sizes {
            let font = Font::load(None, size).expect("system monospace font is available");
            let metrics = font.metrics();
            black_box(metrics);
        }
    });

    // Runtime zoom cost: in-place `set_size` (metrics + cache/atlas clear)
    // followed by a realistic glyph warmup, both directions.
    measure("font resize 12->24 with ASCII warmup", 12, || {
        let mut font = Font::load(None, 12.0).expect("system monospace font is available");
        black_box(font.set_size(black_box(24.0)).expect("resize to 24"));
        for ch in (b'A'..=b'Z').map(char::from) {
            black_box(font.glyph(ch, false, false).expect("warm glyph at 24"));
        }
        black_box(font.glyph('界', false, false).expect("warm CJK at 24"));
    });

    measure("font resize 24->12 with ASCII warmup", 12, || {
        let mut font = Font::load(None, 24.0).expect("system monospace font is available");
        black_box(font.set_size(black_box(12.0)).expect("resize to 12"));
        for ch in (b'A'..=b'Z').map(char::from) {
            black_box(font.glyph(ch, false, false).expect("warm glyph at 12"));
        }
        black_box(font.glyph('界', false, false).expect("warm CJK at 12"));
    });

    measure("atlas growth across ASCII range at 14pt", 1, || {
        let mut font = Font::load(None, 14.0).expect("system monospace font is available");
        let mut glyph_count = 0u32;
        for ch in (b' '..=b'~').map(char::from) {
            if let Ok((_, _, upload)) = font.glyph(ch, false, false) {
                if upload.is_some() {
                    glyph_count += 1;
                }
            }
        }
        black_box(glyph_count);
    });
}
