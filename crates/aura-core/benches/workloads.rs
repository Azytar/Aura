use std::hint::black_box;
use std::time::Instant;

use aura_core::{Cell, Grid, Parser, Terminal};

fn measure(name: &str, iterations: usize, mut operation: impl FnMut()) {
    let start = Instant::now();
    for _ in 0..iterations {
        operation();
    }
    let elapsed = start.elapsed();
    println!(
        "{name}: {:.1} ns/iteration",
        elapsed.as_nanos() as f64 / iterations as f64
    );
}

fn main() {
    let ascii = vec![b'x'; 4096];
    measure("ASCII parser (4 KiB)", 2_000, || {
        let mut parser = Parser::new();
        parser.feed(black_box(&ascii), |_| {});
    });

    let cursor = b"\x1b[12;40H".repeat(1_000);
    measure("CSI cursor movement (9 KiB)", 200, || {
        let mut terminal = Terminal::new(120, 40).expect("fixed dimensions are valid");
        black_box(terminal.process(black_box(&cursor)));
    });

    let utf8 = "界e\u{301}".repeat(1024).into_bytes();
    measure("UTF-8 terminal (4 KiB)", 500, || {
        let mut terminal = Terminal::new(80, 25).expect("fixed dimensions are valid");
        black_box(terminal.process(black_box(&utf8)));
    });

    let sgr = b"\x1b[38;2;12;34;56mhello\x1b[0m".repeat(128);
    measure("SGR terminal (3 KiB)", 500, || {
        let mut terminal = Terminal::new(80, 25).expect("fixed dimensions are valid");
        black_box(terminal.process(black_box(&sgr)));
    });

    let large_output = vec![b'x'; 250_000];
    measure("terminal output (250 KiB)", 4, || {
        let mut terminal = Terminal::new(120, 40).expect("fixed dimensions are valid");
        black_box(terminal.process(black_box(&large_output)));
    });

    // Match the native harness's line-oriented traffic without PTY or GPU
    // work. Populate history before timing so steady-state row reuse is
    // measured separately from terminal construction and initial allocation.
    for history in [0, 10_000] {
        for (name, block) in [
            ("ASCII", format!("{}\r\n", "x".repeat(79))),
            ("Unicode", format!("{}\r\n", "ab界e\u{301}".repeat(15))),
            (
                "SGR",
                "\x1b[38;2;12;171;52mcolored\x1b[0m plain\r\n".to_owned(),
            ),
        ] {
            let payload = block
                .repeat((1usize << 20).div_ceil(block.len()))
                .into_bytes();
            let mut terminal = Terminal::new(80, 24).unwrap();
            terminal.set_scrollback_limit(history);
            for _ in 0..history + 24 {
                terminal.process(b"prefill\r\n");
            }
            measure(
                &format!(
                    "sustained {name} terminal ({} bytes, history={history})",
                    payload.len()
                ),
                8,
                || {
                    black_box(terminal.process(black_box(&payload)));
                    assert_eq!(terminal.active_grid().scrollback_len(), history);
                },
            );
        }
    }

    let mut grid = Grid::new(120, 40).expect("fixed dimensions are valid");
    let cell = Cell {
        content: b'x' as u32,
        fg: 0,
        bg: 0,
        flags: 0,
    };
    measure("grid writes (4,800 cells)", 200, || {
        for row in 0..40 {
            for column in 0..120 {
                black_box(grid.set_cell(row, column, cell));
            }
        }
    });

    let mut grid = Grid::new(120, 40).expect("fixed dimensions are valid");
    measure("full-grid ring scroll (1,000 rows)", 100, || {
        for _ in 0..1_000 {
            black_box(grid.scroll_up(0, 39, 1));
        }
    });

    let wrapped = b"terminal reflow measurement ".repeat(256);
    measure("resize and reflow (80x25 to 100x30)", 25, || {
        let mut terminal = Terminal::new(80, 25).expect("fixed dimensions are valid");
        terminal.process(black_box(&wrapped));
        terminal.resize(100, 30).expect("valid resized dimensions");
        black_box(&terminal);
        terminal.resize(80, 25).expect("valid restored dimensions");
        black_box(&terminal);
    });

    let wide = "界x".repeat(4096).into_bytes();
    measure(
        "resize and reflow with wide cells (80x25 to 200x60)",
        10,
        || {
            let mut terminal = Terminal::new(80, 25).expect("fixed dimensions are valid");
            terminal.process(black_box(&wide));
            terminal.resize(200, 60).expect("valid resized dimensions");
            black_box(&terminal);
            terminal.resize(80, 25).expect("valid restored dimensions");
            black_box(&terminal);
        },
    );

    let combining = "e\u{301}x".repeat(4096).into_bytes();
    measure(
        "resize and reflow with combining marks (200x60 to 80x25)",
        10,
        || {
            let mut terminal = Terminal::new(200, 60).expect("fixed dimensions are valid");
            terminal.process(black_box(&combining));
            terminal.resize(80, 25).expect("valid resized dimensions");
            black_box(&terminal);
            terminal.resize(200, 60).expect("valid restored dimensions");
            black_box(&terminal);
        },
    );

    measure("scrollback traversal (10k lines, page walk)", 5, || {
        let mut terminal = Terminal::new(80, 25).expect("fixed dimensions are valid");
        let block = vec![b'y'; 80 * 25];
        for _ in 0..400 {
            terminal.process(black_box(&block));
        }
        for _ in 0..40 {
            terminal.scroll_view_up(25);
            black_box(terminal.viewport_offset());
            terminal.scroll_view_down(25);
            black_box(terminal.viewport_offset());
        }
    });

    // Populate outside the timer: interactive zoom pays for resize, not for
    // printing the history again. Short tree-like lines avoid eviction when
    // the width changes, so repeated round trips keep the same working set.
    for history in [0, 1_000, 10_000] {
        for (width, height) in [(79, 25), (80, 24)] {
            let mut terminal = Terminal::new(80, 25).expect("fixed dimensions are valid");
            terminal.set_scrollback_limit(history);
            for line in 0..history + 25 {
                terminal.process(format!("├── directory-{line:05}/file-e\u{301}界\r\n").as_bytes());
            }
            measure(
                &format!("history resize round trip ({history} rows, 80x25 to {width}x{height})"),
                20,
                || {
                    terminal
                        .resize(width, height)
                        .expect("valid resized dimensions");
                    terminal.resize(80, 25).expect("valid restored dimensions");
                    black_box(&terminal);
                },
            );
        }
    }

    // Fill outside the timer and leave headroom for the narrower geometry.
    // CRLF fixes the insertion point before round trips, including the single
    // continuous paragraph case; no history may be lost or added on restore.
    for history in [1_000, 10_000] {
        for continuous in [false, true] {
            let mut terminal = Terminal::new(80, 25).unwrap();
            terminal.set_scrollback_limit(history * 2 + 64);
            if continuous {
                for _ in 0..history + 24 {
                    terminal.process(&[b'x'; 80]);
                }
                terminal.process(b"\r\n");
            } else {
                for _ in 0..(history + 24) / 2 {
                    terminal.process(&[b'x'; 160]);
                    terminal.process(b"\r\n");
                }
            }
            let retained = terminal.active_grid().scrollback_len();
            assert_eq!(retained, history);
            measure(
                &format!("wrapped history round trip ({history} rows, continuous={continuous})"),
                8,
                || {
                    terminal.resize(73, 22).unwrap();
                    terminal.resize(80, 25).unwrap();
                    assert_eq!(terminal.active_grid().scrollback_len(), retained);
                    black_box(&terminal);
                },
            );
        }
    }

    // Font-size-to-geometry conversion must stay cheap because it runs on
    // every startup and any future font-change path.
    let sizes = [10.0, 12.0, 14.0, 16.0, 18.0, 20.0, 24.0];
    measure("font metrics to cell/pixel geometry", sizes.len(), || {
        for &size in &sizes {
            let font = aura_text::Font::load_with_family("monospace", size)
                .expect("system monospace is available");
            let metrics = font.metrics();
            let cols = 120usize;
            let rows = 32usize;
            let pixel_width = (cols * metrics.width as usize).max(1) as u32;
            let pixel_height = (rows * metrics.height as usize).max(1) as u32;
            black_box((pixel_width, pixel_height, metrics.width, metrics.height));
        }
    });
}
