use super::{CellFlags, DEFAULT_COLOR, RGB_COLOR_TAG, Rendition, Terminal, indexed_color};
use crate::Csi;

impl Terminal {
    pub(super) fn sgr(&mut self, csi: &Csi) {
        if csi.param_count == 0 {
            self.rendition = Rendition::default();
            return;
        }
        let count = usize::from(csi.param_count);
        let mut index = 0;
        while index < count {
            let start = index;
            let code = csi.param(index, 0);
            index += 1;
            while index < count && csi.subparams[index] {
                index += 1;
            }
            if index > start + 1 {
                match code {
                    4 if index == start + 2 => self.underline_style(csi.param(start + 1, 1)),
                    38 | 48 | 58 => {
                        let values = &csi.params[start + 1..index];
                        let color = match values {
                            [5, value] => Some(indexed_color((*value).min(255) as u8)),
                            [2, r, g, b] | [2, 0, r, g, b] => Some(rgb(*r, *g, *b)),
                            _ => None,
                        };
                        self.rendition_color(code, color);
                    }
                    _ => {}
                }
                continue;
            }
            match code {
                0 => self.rendition = Rendition::default(),
                1 => self.rendition.flags |= CellFlags::BOLD.bits(),
                2 => self.rendition.flags |= CellFlags::FAINT.bits(),
                3 => self.rendition.flags |= CellFlags::ITALIC.bits(),
                4 => self.underline_style(1),
                7 => self.rendition.flags |= CellFlags::INVERSE.bits(),
                8 => self.rendition.flags |= CellFlags::HIDDEN.bits(),
                9 => self.rendition.flags |= CellFlags::STRIKETHROUGH.bits(),
                21 => self.underline_style(2),
                22 => self.rendition.flags &= !(CellFlags::BOLD.bits() | CellFlags::FAINT.bits()),
                23 => self.rendition.flags &= !CellFlags::ITALIC.bits(),
                24 => self.underline_style(0),
                27 => self.rendition.flags &= !CellFlags::INVERSE.bits(),
                28 => self.rendition.flags &= !CellFlags::HIDDEN.bits(),
                29 => self.rendition.flags &= !CellFlags::STRIKETHROUGH.bits(),
                30..=37 => self.rendition.fg = indexed_color((code - 30) as u8),
                40..=47 => self.rendition.bg = indexed_color((code - 40) as u8),
                90..=97 => self.rendition.fg = indexed_color((code - 90 + 8) as u8),
                100..=107 => self.rendition.bg = indexed_color((code - 100 + 8) as u8),
                39 => self.rendition.fg = DEFAULT_COLOR,
                49 => self.rendition.bg = DEFAULT_COLOR,
                59 => self.rendition.underline_color = DEFAULT_COLOR,
                38 | 48 | 58 => {
                    let available = count - index;
                    let needed = match csi.param(index, u16::MAX) {
                        5 => 2,
                        2 => 4,
                        _ => usize::MAX,
                    };
                    // Incomplete extended colors consume the tail, rather than
                    // accidentally interpreting a component as a fresh attribute.
                    if available < needed || csi.subparams[index..index + needed].iter().any(|v| *v)
                    {
                        break;
                    }
                    let color = if needed == 2 {
                        indexed_color(csi.param(index + 1, 0).min(255) as u8)
                    } else {
                        rgb(
                            csi.param(index + 1, 0),
                            csi.param(index + 2, 0),
                            csi.param(index + 3, 0),
                        )
                    };
                    index += needed;
                    self.rendition_color(code, Some(color));
                }
                _ => {}
            }
        }
    }

    fn underline_style(&mut self, style: u16) {
        if style > 5 {
            return;
        }
        self.rendition.flags &=
            !(CellFlags::UNDERLINE.bits() | CellFlags::UNDERLINE_STYLE_MASK.bits());
        if style > 0 {
            self.rendition.flags |= CellFlags::UNDERLINE.bits() | (u32::from(style - 1) << 9);
        }
    }

    fn rendition_color(&mut self, code: u16, color: Option<u32>) {
        if let Some(color) = color {
            match code {
                38 => self.rendition.fg = color,
                48 => self.rendition.bg = color,
                58 => self.rendition.underline_color = color,
                _ => unreachable!(),
            }
        }
    }
}

fn rgb(r: u16, g: u16, b: u16) -> u32 {
    RGB_COLOR_TAG
        | (u32::from(r.min(255)) << 16)
        | (u32::from(g.min(255)) << 8)
        | u32::from(b.min(255))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colon_colors_and_attributes_survive_bytewise_input_and_reset() {
        let bytes = "\x1b[4:3;58:2::1:2:3;38:2:4:5:6;2;8m界e\u{301}\x1b[22;28;24;59mX".as_bytes();
        let mut whole = Terminal::new(8, 2).unwrap();
        whole.process(bytes);
        let mut split = Terminal::new(8, 2).unwrap();
        for byte in bytes {
            split.process(&[*byte]);
        }
        for row in 0..2 {
            for col in 0..8 {
                assert_eq!(whole.viewport_cell(row, col), split.viewport_cell(row, col));
                assert_eq!(
                    whole.viewport_grapheme(row, col),
                    split.viewport_grapheme(row, col)
                );
                assert_eq!(
                    whole.viewport_underline_color(row, col),
                    split.viewport_underline_color(row, col)
                );
            }
        }
        for col in 0..3 {
            assert_eq!(whole.viewport_underline_color(0, col), Some(rgb(1, 2, 3)));
            let cell = whole.viewport_cell(0, col).unwrap();
            assert!(cell.flags().contains(CellFlags::HIDDEN));
            assert!(cell.flags().contains(CellFlags::FAINT));
            assert_eq!(cell.fg, rgb(4, 5, 6));
            assert_eq!((cell.flags >> 9) & 7, 2);
        }
        assert_eq!(whole.viewport_cell(0, 3).unwrap().flags, 0);
        assert_eq!(whole.viewport_underline_color(0, 3), None);
        whole.process(b"\x1b[2:3H");
        assert_eq!(
            whole.cursor().column,
            4,
            "non-SGR subparameters must not move the cursor"
        );
    }

    #[test]
    fn underline_metadata_follows_shifts_wide_pairs_and_borrowed_reflow() {
        let mut terminal = Terminal::new(8, 3).unwrap();
        terminal.process(b"\x1b[4;58;5;12m");
        terminal.process("A界B".as_bytes());
        terminal.process(b"\r\x1b[@");
        assert_eq!(terminal.viewport_underline_color(0, 0), None);
        for col in 1..5 {
            assert_eq!(
                terminal.viewport_underline_color(0, col),
                Some(indexed_color(12))
            );
        }
        terminal.process(b"\x1b[P");
        for col in 0..4 {
            assert_eq!(
                terminal.viewport_underline_color(0, col),
                Some(indexed_color(12))
            );
        }
        terminal.process(b"\x1b[2J\x1b[H");
        terminal.process("ab界e\u{301}".repeat(30).as_bytes());
        for width in [5, 7, 3, 11, 8] {
            terminal.resize(width, 3).unwrap();
            let grid = terminal.active_grid();
            for id in grid.first_line()..=grid.last_line() {
                let Some(line) = grid.row_at_line(id) else {
                    continue;
                };
                for (column, cell) in line.cells().iter().enumerate() {
                    if cell.content != 0 || cell.flags().contains(CellFlags::WIDE_SPACER) {
                        assert_eq!(line.underline_color_at(column), Some(indexed_color(12)));
                    }
                }
            }
        }
        terminal.process(b"\x1b[2J");
        for row in 0..3 {
            for col in 0..8 {
                assert_eq!(terminal.viewport_underline_color(row, col), None);
            }
        }
    }

    #[test]
    fn unsupported_subgroups_do_not_become_separate_attributes() {
        let mut terminal = Terminal::new(8, 1).unwrap();
        terminal.process(b"\x1b[4:99;38:2:7:1:2:3;9:2mX");
        let cell = terminal.viewport_cell(0, 0).unwrap();
        assert_eq!(cell.flags, 0);
        assert_eq!(cell.fg, DEFAULT_COLOR);
    }
}
