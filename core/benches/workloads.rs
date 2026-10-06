use std::hint::black_box;
use std::time::Instant;

use aterm_core::{Parser, Terminal};

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
}
