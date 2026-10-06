use crate::{
    ActiveScreen, Cell, CellFlags, Cursor, Grid, GridError, Parser, ParserEvent, TerminalGrid,
};

/// Renderer-defined foreground/background default palette entry.
pub const DEFAULT_COLOR: u32 = 0;
/// Color words with this tag store an xterm 256-color palette index in the low byte.
pub const INDEXED_COLOR_TAG: u32 = 0x0100_0000;
/// Color words with this tag store packed `0xRRGGBB` in the low 24 bits.
pub const RGB_COLOR_TAG: u32 = 0x0200_0000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseTracking {
    #[default]
    Off,
    Press,
    Drag,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Modes {
    pub application_cursor: bool,
    pub cursor_visible: bool,
    pub auto_wrap: bool,
    pub origin: bool,
    pub mouse_tracking: MouseTracking,
    pub mouse_sgr: bool,
    pub bracketed_paste: bool,
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            application_cursor: false,
            cursor_visible: true,
            auto_wrap: true,
            origin: false,
            mouse_tracking: MouseTracking::Off,
            mouse_sgr: false,
            bracketed_paste: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rendition {
    fg: u32,
    bg: u32,
    flags: u32,
}

impl Default for Rendition {
    fn default() -> Self {
        Self {
            fg: DEFAULT_COLOR,
            bg: DEFAULT_COLOR,
            flags: 0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct SavedCursor {
    cursor: Cursor,
    rendition: Rendition,
    pending_wrap: bool,
}

/// Terminal semantics layered over the independent main and alternate grids.
pub struct Terminal {
    grid: TerminalGrid,
    parser: Parser,
    modes: Modes,
    mouse_press_enabled: bool,
    mouse_drag_enabled: bool,
    rendition: Rendition,
    saved_cursor: Option<SavedCursor>,
    alternate_saved_cursor: Option<SavedCursor>,
    alternate_1049_active: bool,
    top_margin: usize,
    bottom_margin: usize,
    pending_wrap: bool,
    main_pending_wrap: bool,
    title: String,
    bell_count: u64,
    g0_dec_graphics: bool,
    g1_dec_graphics: bool,
    use_g1: bool,
}

impl Terminal {
    pub fn new(width: usize, height: usize) -> Result<Self, GridError> {
        Ok(Self {
            grid: TerminalGrid::new(width, height)?,
            parser: Parser::new(),
            modes: Modes::default(),
            mouse_press_enabled: false,
            mouse_drag_enabled: false,
            rendition: Rendition::default(),
            saved_cursor: None,
            alternate_saved_cursor: None,
            alternate_1049_active: false,
            top_margin: 0,
            bottom_margin: height - 1,
            pending_wrap: false,
            main_pending_wrap: false,
            title: String::new(),
            bell_count: 0,
            g0_dec_graphics: false,
            g1_dec_graphics: false,
            use_g1: false,
        })
    }

    pub const fn modes(&self) -> Modes {
        self.modes
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub const fn bell_count(&self) -> u64 {
        self.bell_count
    }

    pub fn cursor(&self) -> Cursor {
        self.grid.active().cursor()
    }

    pub fn grid(&self) -> &TerminalGrid {
        &self.grid
    }

    pub fn active_grid(&self) -> &Grid {
        self.grid.active()
    }

    /// Consumes the active screen's row damage after its contents have been uploaded.
    pub fn take_dirty_rows(&mut self) -> Vec<usize> {
        self.grid.active_mut().take_dirty_rows()
    }

    pub fn scroll_region(&self) -> (usize, usize) {
        (self.top_margin, self.bottom_margin)
    }

    pub fn is_alternate_screen(&self) -> bool {
        self.grid.active_screen() == ActiveScreen::Alternate
    }

    /// Consumes a PTY byte stream and returns terminal replies for the child process.
    pub fn process(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut replies = Vec::new();
        let mut parser = std::mem::take(&mut self.parser);
        parser.feed(bytes, |event| self.dispatch(event, &mut replies));
        self.parser = parser;
        replies
    }

    pub fn resize(&mut self, width: usize, height: usize) -> Result<(), GridError> {
        self.grid.resize(width, height)?;
        self.top_margin = 0;
        self.bottom_margin = height - 1;
        self.pending_wrap = false;
        self.main_pending_wrap = false;
        Ok(())
    }

    fn dispatch(&mut self, event: ParserEvent, replies: &mut Vec<u8>) {
        match event {
            ParserEvent::Print(ch) => {
                let ch = if self.use_g1 {
                    if self.g1_dec_graphics {
                        dec_graphic(ch)
                    } else {
                        ch
                    }
                } else if self.g0_dec_graphics {
                    dec_graphic(ch)
                } else {
                    ch
                };
                self.put_char(ch);
            }
            ParserEvent::Control(control) => self.control(control),
            ParserEvent::Escape {
                intermediates,
                intermediate_len,
                final_byte,
            } => self.escape(
                &intermediates[..intermediate_len as usize],
                final_byte,
                replies,
            ),
            ParserEvent::Csi(csi) => self.csi(csi, replies),
            ParserEvent::Osc(bytes) => self.osc(&bytes),
        }
    }

    fn control(&mut self, control: u8) {
        match control {
            0x07 => self.bell_count = self.bell_count.saturating_add(1),
            0x08 => {
                self.pending_wrap = false;
                let cursor = self.cursor();
                self.set_cursor(cursor.column.saturating_sub(1), cursor.row);
            }
            0x09 => {
                self.pending_wrap = false;
                let cursor = self.cursor();
                let next_tab = (cursor.column / 8 + 1) * 8;
                self.set_cursor(next_tab.min(self.active_grid().width() - 1), cursor.row);
            }
            0x0a..=0x0c => self.index(),
            0x0d => {
                self.pending_wrap = false;
                let row = self.cursor().row;
                self.set_cursor(0, row);
            }
            0x0e => self.use_g1 = true,
            0x0f => self.use_g1 = false,
            _ => {}
        }
    }

    fn escape(&mut self, intermediates: &[u8], final_byte: u8, replies: &mut Vec<u8>) {
        if intermediates == b"(" {
            self.g0_dec_graphics = final_byte == b'0';
            return;
        }
        if intermediates == b")" {
            self.g1_dec_graphics = final_byte == b'0';
            return;
        }
        if !intermediates.is_empty() {
            return;
        }
        match final_byte {
            b'7' => self.saved_cursor = Some(self.save_cursor()),
            b'8' => {
                if let Some(saved) = self.saved_cursor {
                    self.restore_cursor(saved);
                }
            }
            b'D' => self.index(),
            b'E' => {
                self.carriage_return();
                self.index();
            }
            b'M' => self.reverse_index(),
            b'Z' => replies.extend_from_slice(b"\x1b[?1;2c"),
            b'c' => self.reset(),
            _ => {}
        }
    }

    fn csi(&mut self, csi: crate::Csi, replies: &mut Vec<u8>) {
        if csi.private == Some(b'?') {
            match csi.final_byte {
                b'h' => self.set_dec_modes(&csi, true),
                b'l' => self.set_dec_modes(&csi, false),
                b'n' if csi.param(0, 0) == 6 => self.report_cursor(replies, true),
                _ => {}
            }
            return;
        }
        match csi.final_byte {
            b'A' => self.move_vertical(param_count(&csi, 0), true),
            b'B' | b'e' => self.move_vertical(param_count(&csi, 0), false),
            b'C' | b'a' => self.move_horizontal(param_count(&csi, 0) as isize),
            b'D' => self.move_horizontal(-(param_count(&csi, 0) as isize)),
            b'E' => {
                self.move_vertical(param_count(&csi, 0), false);
                let row = self.cursor().row;
                self.set_cursor(0, row);
            }
            b'F' => {
                self.move_vertical(param_count(&csi, 0), true);
                let row = self.cursor().row;
                self.set_cursor(0, row);
            }
            b'G' | b'`' => {
                let col = one_based(csi.param(0, 1));
                let row = self.cursor().row;
                self.set_cursor(col, row);
            }
            b'd' => {
                let row = one_based(csi.param(0, 1));
                let target = if self.modes.origin {
                    self.top_margin + row
                } else {
                    row
                };
                let col = self.cursor().column;
                self.position(col, target);
            }
            b'H' | b'f' => {
                let row = one_based(csi.param(0, 1));
                let col = one_based(csi.param(1, 1));
                let target = if self.modes.origin {
                    self.top_margin + row
                } else {
                    row
                };
                self.position(col, target);
            }
            b'J' => self.erase_display(csi.param(0, 0)),
            b'K' => self.erase_line(csi.param(0, 0)),
            b'@' => {
                let cursor = self.cursor();
                self.active_grid_mut().insert_cells(
                    cursor.row,
                    cursor.column,
                    param_count(&csi, 0),
                );
                self.pending_wrap = false;
            }
            b'P' => {
                let cursor = self.cursor();
                self.active_grid_mut().delete_cells(
                    cursor.row,
                    cursor.column,
                    param_count(&csi, 0),
                );
                self.pending_wrap = false;
            }
            b'L' => {
                let cursor = self.cursor();
                if (self.top_margin..=self.bottom_margin).contains(&cursor.row) {
                    let bottom = self.bottom_margin;
                    self.active_grid_mut()
                        .insert_rows(cursor.row, bottom, param_count(&csi, 0));
                }
            }
            b'M' => {
                let cursor = self.cursor();
                if (self.top_margin..=self.bottom_margin).contains(&cursor.row) {
                    let bottom = self.bottom_margin;
                    self.active_grid_mut()
                        .delete_rows(cursor.row, bottom, param_count(&csi, 0));
                }
            }
            b'r' => self.set_scroll_region(&csi),
            b'm' => self.sgr(&csi),
            b's' => self.saved_cursor = Some(self.save_cursor()),
            b'u' => {
                if let Some(saved) = self.saved_cursor {
                    self.restore_cursor(saved);
                }
            }
            b'c' if csi.param(0, 0) == 0 => replies.extend_from_slice(b"\x1b[?1;2c"),
            b'n' if csi.param(0, 0) == 5 => replies.extend_from_slice(b"\x1b[0n"),
            b'n' if csi.param(0, 0) == 6 => self.report_cursor(replies, false),
            _ => {}
        }
    }

    fn put_char(&mut self, ch: char) {
        if self.pending_wrap && self.modes.auto_wrap {
            let row = self.cursor().row;
            let top = self.top_margin;
            let bottom = self.bottom_margin;
            if row == bottom {
                self.active_grid_mut().scroll_up(top, bottom, 1);
                self.set_cursor(0, row);
            } else if row + 1 < self.active_grid().height() {
                self.set_cursor(0, row + 1);
            }
            let continuation = self.cursor().row;
            self.active_grid_mut().set_row_wrapped(continuation, true);
        }
        self.pending_wrap = false;
        let cursor = self.cursor();
        let cell = Cell::new(
            ch as u32,
            self.rendition.fg,
            self.rendition.bg,
            CellFlags::from_bits_retain(self.rendition.flags),
        );
        self.active_grid_mut()
            .set_cell(cursor.row, cursor.column, cell);
        if cursor.column + 1 >= self.active_grid().width() {
            if self.modes.auto_wrap {
                self.pending_wrap = true;
            }
        } else {
            self.set_cursor(cursor.column + 1, cursor.row);
        }
    }

    fn index(&mut self) {
        self.pending_wrap = false;
        let cursor = self.cursor();
        let top = self.top_margin;
        let bottom = self.bottom_margin;
        if cursor.row == bottom {
            self.active_grid_mut().scroll_up(top, bottom, 1);
        } else if cursor.row + 1 < self.active_grid().height() {
            self.set_cursor(cursor.column, cursor.row + 1);
            let row = self.cursor().row;
            self.active_grid_mut().set_row_wrapped(row, false);
        }
    }

    fn reverse_index(&mut self) {
        self.pending_wrap = false;
        let cursor = self.cursor();
        let top = self.top_margin;
        if cursor.row == top {
            let bottom = self.bottom_margin;
            self.active_grid_mut().scroll_down(top, bottom, 1);
        } else if cursor.row > 0 {
            self.set_cursor(cursor.column, cursor.row - 1);
        }
    }

    fn carriage_return(&mut self) {
        self.pending_wrap = false;
        let row = self.cursor().row;
        self.set_cursor(0, row);
    }

    fn set_cursor(&mut self, column: usize, row: usize) {
        self.pending_wrap = false;
        self.grid.active_mut().set_cursor(column, row);
    }

    fn position(&mut self, column: usize, row: usize) {
        let (top, bottom) = self.vertical_bounds();
        let row = row.clamp(top, bottom);
        self.set_cursor(column.min(self.active_grid().width() - 1), row);
    }

    fn move_vertical(&mut self, amount: usize, up: bool) {
        let cursor = self.cursor();
        let (top, bottom) = self.vertical_bounds();
        let row = if up {
            cursor.row.saturating_sub(amount).max(top)
        } else {
            cursor.row.saturating_add(amount).min(bottom)
        };
        self.set_cursor(cursor.column, row);
    }

    fn move_horizontal(&mut self, amount: isize) {
        let cursor = self.cursor();
        let column = cursor
            .column
            .saturating_add_signed(amount)
            .min(self.active_grid().width() - 1);
        self.set_cursor(column, cursor.row);
    }

    fn vertical_bounds(&self) -> (usize, usize) {
        if self.modes.origin {
            (self.top_margin, self.bottom_margin)
        } else {
            (0, self.active_grid().height() - 1)
        }
    }

    fn erase_display(&mut self, mode: u16) {
        let cursor = self.cursor();
        let width = self.active_grid().width();
        let height = self.active_grid().height();
        match mode {
            0 => {
                self.active_grid_mut()
                    .clear_range_in_row(cursor.row, cursor.column..width);
                self.active_grid_mut()
                    .clear_rect(cursor.row + 1..height, 0..width);
            }
            1 => {
                self.active_grid_mut().clear_rect(0..cursor.row, 0..width);
                self.active_grid_mut()
                    .clear_range_in_row(cursor.row, 0..cursor.column + 1);
            }
            2 => self.active_grid_mut().clear_all(),
            _ => {}
        }
    }

    fn erase_line(&mut self, mode: u16) {
        let cursor = self.cursor();
        let width = self.active_grid().width();
        match mode {
            0 => {
                self.active_grid_mut()
                    .clear_range_in_row(cursor.row, cursor.column..width);
            }
            1 => {
                self.active_grid_mut()
                    .clear_range_in_row(cursor.row, 0..cursor.column + 1);
            }
            2 => {
                self.active_grid_mut().clear_row(cursor.row);
            }
            _ => {}
        }
    }

    fn set_scroll_region(&mut self, csi: &crate::Csi) {
        let height = self.active_grid().height();
        let top = one_based(csi.param(0, 1));
        let bottom = if csi.param_count < 2 || !csi.present[1] {
            height - 1
        } else {
            one_based(csi.param(1, height as u16))
        };
        if top < bottom && bottom < height {
            self.top_margin = top;
            self.bottom_margin = bottom;
            self.home_cursor();
        }
    }

    fn home_cursor(&mut self) {
        let row = if self.modes.origin {
            self.top_margin
        } else {
            0
        };
        self.set_cursor(0, row);
    }

    fn set_dec_modes(&mut self, csi: &crate::Csi, enabled: bool) {
        for index in 0..csi.param_count as usize {
            if !csi.present[index] {
                continue;
            }
            match csi.params[index] {
                1 => self.modes.application_cursor = enabled,
                6 => {
                    self.modes.origin = enabled;
                    self.home_cursor();
                }
                7 => {
                    self.modes.auto_wrap = enabled;
                    self.pending_wrap = false;
                }
                25 => {
                    if self.modes.cursor_visible != enabled {
                        self.modes.cursor_visible = enabled;
                        let row = self.cursor().row;
                        self.active_grid_mut().mark_row_dirty(row);
                    }
                }
                47 | 1047 => self.switch_alternate(enabled, false),
                1000 => {
                    self.mouse_press_enabled = enabled;
                    self.update_mouse_tracking();
                }
                1002 => {
                    self.mouse_drag_enabled = enabled;
                    self.update_mouse_tracking();
                }
                1006 => self.modes.mouse_sgr = enabled,
                1049 => self.switch_alternate(enabled, true),
                2004 => self.modes.bracketed_paste = enabled,
                _ => {}
            }
        }
    }

    fn switch_alternate(&mut self, enabled: bool, save_main: bool) {
        if enabled {
            if self.is_alternate_screen() {
                return;
            }
            if save_main {
                self.alternate_saved_cursor = Some(self.save_cursor());
                self.alternate_1049_active = true;
            }
            self.main_pending_wrap = self.pending_wrap;
            self.grid.select_screen(ActiveScreen::Alternate);
            self.active_grid_mut().clear_all();
            self.home_cursor();
            self.pending_wrap = false;
        } else if self.is_alternate_screen() {
            self.grid.select_screen(ActiveScreen::Main);
            self.pending_wrap = self.main_pending_wrap;
            if save_main && self.alternate_1049_active {
                if let Some(saved) = self.alternate_saved_cursor.take() {
                    self.restore_cursor(saved);
                }
                self.alternate_1049_active = false;
            }
        }
    }

    fn sgr(&mut self, csi: &crate::Csi) {
        if csi.param_count == 0 {
            self.rendition = Rendition::default();
            return;
        }
        let mut index = 0;
        while index < csi.param_count as usize {
            let code = csi.param(index, 0);
            index += 1;
            match code {
                0 => self.rendition = Rendition::default(),
                1 => self.rendition.flags |= CellFlags::BOLD.bits(),
                3 => self.rendition.flags |= CellFlags::ITALIC.bits(),
                4 => self.rendition.flags |= CellFlags::UNDERLINE.bits(),
                7 => self.rendition.flags |= CellFlags::INVERSE.bits(),
                9 => self.rendition.flags |= CellFlags::STRIKETHROUGH.bits(),
                22 => self.rendition.flags &= !CellFlags::BOLD.bits(),
                23 => self.rendition.flags &= !CellFlags::ITALIC.bits(),
                24 => self.rendition.flags &= !CellFlags::UNDERLINE.bits(),
                27 => self.rendition.flags &= !CellFlags::INVERSE.bits(),
                29 => self.rendition.flags &= !CellFlags::STRIKETHROUGH.bits(),
                30..=37 => self.rendition.fg = indexed_color((code - 30) as u8),
                40..=47 => self.rendition.bg = indexed_color((code - 40) as u8),
                90..=97 => self.rendition.fg = indexed_color((code - 90 + 8) as u8),
                100..=107 => self.rendition.bg = indexed_color((code - 100 + 8) as u8),
                39 => self.rendition.fg = DEFAULT_COLOR,
                49 => self.rendition.bg = DEFAULT_COLOR,
                38 | 48 => {
                    let is_foreground = code == 38;
                    let color = match csi.param(index, u16::MAX) {
                        5 if index + 1 < csi.param_count as usize => {
                            index += 1;
                            Some(indexed_color(csi.param(index, 0).min(255) as u8))
                        }
                        2 if index + 3 < csi.param_count as usize => {
                            let r = csi.param(index + 1, 0).min(255) as u32;
                            let g = csi.param(index + 2, 0).min(255) as u32;
                            let b = csi.param(index + 3, 0).min(255) as u32;
                            index += 3;
                            Some(RGB_COLOR_TAG | (r << 16) | (g << 8) | b)
                        }
                        _ => {
                            index = csi.param_count as usize;
                            None
                        }
                    };
                    if let Some(color) = color {
                        if is_foreground {
                            self.rendition.fg = color;
                        } else {
                            self.rendition.bg = color;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn save_cursor(&self) -> SavedCursor {
        SavedCursor {
            cursor: self.cursor(),
            rendition: self.rendition,
            pending_wrap: self.pending_wrap,
        }
    }

    fn restore_cursor(&mut self, saved: SavedCursor) {
        self.grid
            .active_mut()
            .set_cursor(saved.cursor.column, saved.cursor.row);
        self.rendition = saved.rendition;
        self.pending_wrap = saved.pending_wrap;
    }

    fn report_cursor(&self, replies: &mut Vec<u8>, private: bool) {
        let cursor = self.cursor();
        let row = if self.modes.origin {
            cursor.row.saturating_sub(self.top_margin)
        } else {
            cursor.row
        };
        if private {
            replies
                .extend_from_slice(format!("\x1b[?{};{}R", row + 1, cursor.column + 1).as_bytes());
        } else {
            replies
                .extend_from_slice(format!("\x1b[{};{}R", row + 1, cursor.column + 1).as_bytes());
        }
    }

    fn osc(&mut self, bytes: &[u8]) {
        let Some(separator) = bytes.iter().position(|&byte| byte == b';') else {
            return;
        };
        let Ok(command) = std::str::from_utf8(&bytes[..separator]) else {
            return;
        };
        if command == "0" || command == "2" {
            let title = String::from_utf8_lossy(&bytes[separator + 1..]);
            self.title.clear();
            self.title.push_str(&title);
        }
    }

    fn reset(&mut self) {
        self.grid.select_screen(ActiveScreen::Main);
        self.grid.screen_mut(ActiveScreen::Main).clear_all();
        self.grid.screen_mut(ActiveScreen::Alternate).clear_all();
        self.grid.screen_mut(ActiveScreen::Main).set_cursor(0, 0);
        self.grid
            .screen_mut(ActiveScreen::Alternate)
            .set_cursor(0, 0);
        self.modes = Modes::default();
        self.mouse_press_enabled = false;
        self.mouse_drag_enabled = false;
        self.rendition = Rendition::default();
        self.saved_cursor = None;
        self.alternate_saved_cursor = None;
        self.alternate_1049_active = false;
        self.top_margin = 0;
        self.bottom_margin = self.active_grid().height() - 1;
        self.pending_wrap = false;
        self.main_pending_wrap = false;
        self.title.clear();
        self.g0_dec_graphics = false;
        self.g1_dec_graphics = false;
        self.use_g1 = false;
    }

    fn active_grid_mut(&mut self) -> &mut Grid {
        self.grid.active_mut()
    }

    fn update_mouse_tracking(&mut self) {
        self.modes.mouse_tracking = if self.mouse_drag_enabled {
            MouseTracking::Drag
        } else if self.mouse_press_enabled {
            MouseTracking::Press
        } else {
            MouseTracking::Off
        };
    }
}

fn param_count(csi: &crate::Csi, index: usize) -> usize {
    csi.param(index, 1).max(1) as usize
}

fn one_based(value: u16) -> usize {
    value.max(1) as usize - 1
}

fn indexed_color(index: u8) -> u32 {
    INDEXED_COLOR_TAG | index as u32
}

fn dec_graphic(ch: char) -> char {
    match ch {
        '`' => '◆',
        'a' => '▒',
        'f' => '°',
        'g' => '±',
        'j' => '┘',
        'k' => '┐',
        'l' => '┌',
        'm' => '└',
        'n' => '┼',
        'o' => '⎺',
        'p' => '⎻',
        'q' => '─',
        'r' => '⎼',
        's' => '⎽',
        't' => '├',
        'u' => '┤',
        'v' => '┴',
        'w' => '┬',
        'x' => '│',
        'y' => '≤',
        'z' => '≥',
        '{' => 'π',
        '|' => '≠',
        '}' => '£',
        '~' => '·',
        _ => ch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal(width: usize, height: usize) -> Terminal {
        Terminal::new(width, height).unwrap()
    }

    fn line(terminal: &Terminal, row: usize) -> String {
        terminal
            .active_grid()
            .row_at(row)
            .unwrap()
            .cells()
            .iter()
            .map(|cell| {
                if cell.content == 0 {
                    '.'
                } else {
                    char::from_u32(cell.content).unwrap_or('?')
                }
            })
            .collect()
    }

    fn feed(terminal: &mut Terminal, input: &[u8]) -> Vec<u8> {
        terminal.process(input)
    }

    #[test]
    fn printable_text_utf8_controls_and_wide_grid_state() {
        let mut terminal = terminal(8, 3);
        feed(&mut terminal, b"A\xe2");
        feed(&mut terminal, b"\x82\xac\x07\tB\rC\nD");
        assert_eq!(line(&terminal, 0), "C€.....B");
        assert_eq!(line(&terminal, 1), ".D......");
        assert_eq!(terminal.bell_count(), 1);
        assert_eq!(terminal.cursor(), Cursor { column: 2, row: 1 });
    }

    #[test]
    fn right_margin_delays_wrap_until_next_printable() {
        let mut terminal = terminal(3, 2);
        feed(&mut terminal, b"ABC");
        assert_eq!(line(&terminal, 0), "ABC");
        assert_eq!(terminal.cursor(), Cursor { column: 2, row: 0 });
        assert!(!terminal.active_grid().row_at(0).unwrap().is_wrapped());
        feed(&mut terminal, b"D");
        assert_eq!(line(&terminal, 1), "D..");
        assert!(terminal.active_grid().row_at(1).unwrap().is_wrapped());
    }

    #[test]
    fn csi_can_be_split_across_reads_and_deca_wm_disables_wrapping() {
        let mut terminal = terminal(3, 2);
        feed(&mut terminal, b"\x1b[");
        feed(&mut terminal, b"2;");
        feed(&mut terminal, b"2HXY");
        assert_eq!(line(&terminal, 1), ".XY");
        feed(&mut terminal, b"\x1b[?7l\x1b[1;1HABCDEF");
        assert_eq!(line(&terminal, 0), "ABF");
        assert_eq!(terminal.cursor(), Cursor { column: 2, row: 0 });
        assert!(!terminal.modes().auto_wrap);
    }

    #[test]
    fn carriage_return_line_feed_and_backspace_have_distinct_semantics() {
        let mut terminal = terminal(5, 2);
        feed(&mut terminal, b"abc\rZ\nQ\x08X");
        assert_eq!(line(&terminal, 0), "Zbc..");
        assert_eq!(line(&terminal, 1), ".X...");
    }

    #[test]
    fn cursor_csi_and_erase_sequences_obey_bounds() {
        let mut terminal = terminal(5, 3);
        feed(&mut terminal, b"ABCDE\x1b[2;3HXY\x1b[1A\x1b[99C!");
        assert_eq!(line(&terminal, 1), "..XY.");
        assert_eq!(line(&terminal, 0), "ABCD!");
        assert_eq!(terminal.cursor(), Cursor { column: 4, row: 0 });
        feed(&mut terminal, b"\x1b[2J");
        assert_eq!(
            [line(&terminal, 0), line(&terminal, 1), line(&terminal, 2)],
            [".....", ".....", "....."]
        );
    }

    #[test]
    fn ich_dch_il_dl_and_erase_line_mutate_only_requested_regions() {
        let mut terminal = terminal(5, 4);
        feed(&mut terminal, b"ABCDE\x1b[1;3H\x1b[2@\x1b[1P");
        assert_eq!(line(&terminal, 0), "AB.C.");
        feed(
            &mut terminal,
            b"\x1b[2;1H22222\x1b[3;1H33333\x1b[2;1H\x1b[L",
        );
        assert_eq!(line(&terminal, 1), ".....");
        assert_eq!(line(&terminal, 2), "22222");
        feed(&mut terminal, b"\x1b[M\x1b[K");
        assert_eq!(line(&terminal, 1), ".....");
    }

    #[test]
    fn sgr_supports_attributes_ansi_256_and_truecolor() {
        let mut terminal = terminal(8, 1);
        feed(
            &mut terminal,
            b"\x1b[1;3;4;7;9;31;48;5;200mA\x1b[38;2;1;2;3mB\x1b[22;23;24;27;29;39;49mC",
        );
        let row = terminal.active_grid().row_at(0).unwrap();
        let a = row.cells()[0];
        let b = row.cells()[1];
        let c = row.cells()[2];
        assert_eq!(a.fg, INDEXED_COLOR_TAG | 1);
        assert_eq!(a.bg, INDEXED_COLOR_TAG | 200);
        assert_ne!(a.flags & CellFlags::BOLD.bits(), 0);
        assert_ne!(a.flags & CellFlags::INVERSE.bits(), 0);
        assert_eq!(b.fg, RGB_COLOR_TAG | 0x01_02_03);
        assert_eq!(c.fg, DEFAULT_COLOR);
        assert_eq!(c.bg, DEFAULT_COLOR);
        assert_eq!(c.flags, 0);
    }

    #[test]
    fn incomplete_extended_color_does_not_become_unrelated_sgr_attributes() {
        let mut terminal = terminal(2, 1);
        feed(&mut terminal, b"\x1b[38;2;1mX");
        let cell = terminal.active_grid().cell(0, 0).unwrap();
        assert_eq!(cell.fg, DEFAULT_COLOR);
        assert_eq!(cell.flags, 0);
    }

    #[test]
    fn origin_mode_and_scroll_margins_bound_cursor_and_scrolling() {
        let mut terminal = terminal(3, 4);
        feed(
            &mut terminal,
            b"111\r\n222\r\n333\r\n444\x1b[2;3r\x1b[?6h\x1b[99;1H",
        );
        assert_eq!(terminal.cursor().row, 2);
        assert_eq!(terminal.scroll_region(), (1, 2));
        feed(&mut terminal, b"\nX");
        assert_eq!(line(&terminal, 0), "111");
        assert_eq!(line(&terminal, 3), "444");
    }

    #[test]
    fn save_restore_cursor_preserves_position_and_rendition() {
        let mut terminal = terminal(5, 2);
        feed(
            &mut terminal,
            b"\x1b[31m\x1b[2;3H\x1b7\x1b[0m\x1b[1;1H\x1b8X",
        );
        assert_eq!(terminal.cursor(), Cursor { column: 3, row: 1 });
        assert_eq!(
            terminal.active_grid().cell(1, 2).unwrap().fg,
            INDEXED_COLOR_TAG | 1
        );
    }

    #[test]
    fn alternate_screen_1049_keeps_main_cells_and_cursor_isolated() {
        let mut terminal = terminal(4, 2);
        feed(&mut terminal, b"MAIN\x1b[2;2H\x1b[?1049hALT");
        assert!(terminal.is_alternate_screen());
        assert_eq!(line(&terminal, 0), "ALT.");
        feed(&mut terminal, b"\x1b[?1049l");
        assert!(!terminal.is_alternate_screen());
        assert_eq!(line(&terminal, 0), "MAIN");
        assert_eq!(terminal.cursor(), Cursor { column: 1, row: 1 });
    }

    #[test]
    fn alternate_switch_does_not_reuse_main_pending_wrap_state() {
        let mut terminal = terminal(4, 2);
        feed(&mut terminal, b"ABCD\x1b[?47hX\x1b[?47lE");
        assert_eq!(line(&terminal, 0), "ABCD");
        assert_eq!(line(&terminal, 1), "E...");
    }

    #[test]
    fn dec_modes_mouse_bracketed_paste_and_cursor_visibility_are_observable() {
        let mut terminal = terminal(4, 2);
        feed(&mut terminal, b"\x1b[?1;25;1002;1006;2004h");
        let modes = terminal.modes();
        assert!(
            modes.application_cursor
                && modes.cursor_visible
                && modes.mouse_sgr
                && modes.bracketed_paste
        );
        assert_eq!(modes.mouse_tracking, MouseTracking::Drag);
        feed(&mut terminal, b"\x1b[?1000h\x1b[?1002h\x1b[?1002l");
        assert_eq!(terminal.modes().mouse_tracking, MouseTracking::Press);
        feed(&mut terminal, b"\x1b[?1;25;1000;1002;1006;2004l");
        assert!(!terminal.modes().application_cursor);
        assert!(!terminal.modes().cursor_visible);
        assert_eq!(terminal.modes().mouse_tracking, MouseTracking::Off);
        assert!(!terminal.modes().mouse_sgr);
        assert!(!terminal.modes().bracketed_paste);
    }

    #[test]
    fn osc_title_da_dsr_and_cpr_produce_expected_state_and_replies() {
        let mut terminal = terminal(8, 3);
        feed(&mut terminal, b"\x1b]0;hello title\x1b\\\x1b[2;4H");
        assert_eq!(terminal.title(), "hello title");
        assert_eq!(
            feed(&mut terminal, b"\x1b[c\x1b[5n\x1b[6n"),
            b"\x1b[?1;2c\x1b[0n\x1b[2;4R"
        );
        assert_eq!(feed(&mut terminal, b"\x1bZ"), b"\x1b[?1;2c");
    }

    #[test]
    fn malformed_and_unknown_sequences_are_ignored_safely() {
        let mut terminal = terminal(5, 2);
        feed(
            &mut terminal,
            b"A\x1b[1;;999999999999999999999mB\x1b]garbage\x1b\\C\x1bPignored\x1b\\D",
        );
        assert_eq!(line(&terminal, 0), "ABCD.");
    }

    #[test]
    fn dec_special_graphics_and_control_strings_work() {
        let mut terminal = terminal(4, 1);
        feed(&mut terminal, b"\x1b(0q\x1b(B!");
        assert_eq!(line(&terminal, 0), "─!..");
        feed(&mut terminal, b"\x07\x1b[?25l");
        assert_eq!(terminal.bell_count(), 1);
        assert!(!terminal.modes().cursor_visible);
    }

    #[test]
    fn editor_multiplexer_and_monitor_redraw_sequences_smoke() {
        let cases: [(&[u8], ActiveScreen, &str); 4] = [
            (
                b"\x1b[?1049h\x1b[?25l\x1b[2J\x1b[H\x1b[32mVIM\x1b[0m",
                ActiveScreen::Alternate,
                "VIM",
            ),
            (
                b"\x1b[?1;1000;1006;2004h\x1b[2J\x1b[Htmux",
                ActiveScreen::Main,
                "tmux",
            ),
            (
                b"\x1b[?1049h\x1b[?25l\x1b[2J\x1b[1;1Hhtop",
                ActiveScreen::Alternate,
                "htop",
            ),
            (
                b"\x1b[2J\x1b[1;1Hless\x1b[2;4r\x1b[4;1Htail",
                ActiveScreen::Main,
                "less",
            ),
        ];
        for (sequence, screen, expected) in cases {
            let mut terminal = terminal(12, 5);
            feed(&mut terminal, sequence);
            assert_eq!(terminal.grid().active_screen(), screen);
            assert!(line(&terminal, 0).starts_with(expected));
        }
    }
}
