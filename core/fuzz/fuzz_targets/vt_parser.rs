#![no_main]

use aterm_core::Terminal;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: &[u8]| {
    let mut contiguous = Terminal::new(80, 24).expect("constant dimensions are valid");
    let mut chunked = Terminal::new(80, 24).expect("constant dimensions are valid");
    contiguous.process(input);
    for chunk in input.chunks(31) {
        chunked.process(chunk);
    }
    assert_eq!(contiguous.active_grid(), chunked.active_grid());
    assert_eq!(contiguous.cursor(), chunked.cursor());
    assert_eq!(contiguous.modes(), chunked.modes());
    assert_eq!(contiguous.title(), chunked.title());
});
