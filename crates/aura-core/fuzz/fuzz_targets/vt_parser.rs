#![no_main]
use aura_core::{SelectionPoint, Terminal};
use libfuzzer_sys::fuzz_target;

fn same(left: &mut Terminal, right: &mut Terminal) {
    assert_eq!(left.grid(), right.grid());
    assert_eq!(left.cursor(), right.cursor());
    assert_eq!(left.modes(), right.modes());
    assert_eq!(left.scroll_region(), right.scroll_region());
    assert_eq!(left.title(), right.title());
    assert_eq!(left.bell_count(), right.bell_count());
    assert_eq!(left.selection(), right.selection());
    assert_eq!(left.selection_text(), right.selection_text());
    assert_eq!(
        left.take_clipboard_requests(),
        right.take_clipboard_requests()
    );
    assert_eq!(left.take_shell_markers(), right.take_shell_markers());
    for row in 0..left.active_grid().height() {
        for column in 0..left.active_grid().width() {
            assert_eq!(
                left.hyperlink_at(row, column),
                right.hyperlink_at(row, column)
            );
        }
    }
}

fuzz_target!(|input: &[u8]| {
    if input.len() < 4 || input.len() > 8192 {
        return;
    }
    let width = 1 + usize::from(input[0] % 16);
    let height = 1 + usize::from(input[1] % 8);
    let chunk = 1 + usize::from(input[2] % 31);
    let mut whole = Terminal::new(width, height).unwrap();
    let mut split = Terminal::new(width, height).unwrap();
    whole.set_scrollback_limit(32);
    split.set_scrollback_limit(32);
    let middle = input.len() / 2;
    for part in [&input[..middle], &input[middle..]] {
        let expected = whole.process(part);
        let mut actual = Vec::new();
        for piece in part.chunks(chunk) {
            actual.extend(split.process(piece));
            split.process(&[]);
        }
        assert_eq!(expected, actual);
        same(&mut whole, &mut split);
        let new_width = 1 + usize::from(input[3] % 16);
        whole.resize(new_width, height).unwrap();
        split.resize(new_width, height).unwrap();
        for terminal in [&mut whole, &mut split] {
            terminal.scroll_view_up(3);
            let line = terminal.viewport_line(0).unwrap();
            terminal.set_selection(
                SelectionPoint { line, column: 0 },
                SelectionPoint {
                    line,
                    column: new_width - 1,
                },
            );
        }
        same(&mut whole, &mut split);
    }
    // Equal visible state is insufficient: continue pending parser and VT
    // state identically, visiting both screen buffers and external effects.
    for suffix in [b"\x18\x1b[6n".as_slice(), b"\x1b[?1049hX\x1b[?1049l\x1b[6n"] {
        assert_eq!(whole.process(suffix), split.process(suffix));
        same(&mut whole, &mut split);
    }
});
