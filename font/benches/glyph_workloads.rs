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
}
