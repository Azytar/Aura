use crate::{
    ActiveScreen, Cell, CellFlags, Cursor, Grid, GridError, Parser, ParserEvent, TerminalGrid,
};
use std::collections::HashMap;
use std::collections::VecDeque;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Osc52Write {
    pub selection: Vec<u8>,
    pub base64: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellMarker {
    PromptStart,
    CommandStart,
    OutputStart,
    CommandFinished(Option<i32>),
}

/// DECSCUSR cursor shape selected with `CSI Ps SP q`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorStyle {
    /// Blinking or steady full-cell block (Ps 0, 1, 2).
    #[default]
    Block,
    /// Blinking or steady underline bar (Ps 3, 4).
    Underline,
    /// Blinking or steady vertical bar (Ps 5, 6).
    Bar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionPoint {
    pub row: usize,
    pub column: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Modes {
    pub application_cursor: bool,
    pub application_keypad: bool,
    pub cursor_visible: bool,
    pub auto_wrap: bool,
    pub origin: bool,
    pub mouse_tracking: MouseTracking,
    pub mouse_sgr: bool,
    pub bracketed_paste: bool,
    pub cursor_style: CursorStyle,
    pub cursor_blink: bool,
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            application_cursor: false,
            application_keypad: false,
            cursor_visible: true,
            auto_wrap: true,
            origin: false,
            mouse_tracking: MouseTracking::Off,
            mouse_sgr: false,
            bracketed_paste: false,
            cursor_style: CursorStyle::Block,
            cursor_blink: true,
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
    last_printed: Option<(ActiveScreen, usize, usize)>,
    preceding_char: Option<char>,
    tabs: Vec<bool>,
    insert_mode: bool,
    current_hyperlink: Option<u32>,
    hyperlinks: HashMap<u32, String>,
    next_hyperlink: u32,
    clipboard_requests: VecDeque<Osc52Write>,
    shell_markers: VecDeque<ShellMarker>,
    selection: Option<(SelectionPoint, SelectionPoint)>,
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
            last_printed: None,
            preceding_char: None,
            tabs: (0..width).map(|col| col != 0 && col % 8 == 0).collect(),
            insert_mode: false,
            current_hyperlink: None,
            hyperlinks: HashMap::new(),
            next_hyperlink: 1,
            clipboard_requests: VecDeque::new(),
            shell_markers: VecDeque::new(),
            selection: None,
        })
    }

    pub const fn modes(&self) -> Modes {
        self.modes
    }

    /// Replaces the whole mode set.
    ///
    /// Called once at startup to seed the configuration's cursor style and
    /// blink preference. Anything the application later emits (DECSCUSR,
    /// DECPrivateModeSet) overrides it by construction, because those arrive
    /// after the seed. This is deliberately not a per-field setter: a half
    /// applied mode set is a state the terminal would never produce itself.
    pub const fn set_modes(&mut self, modes: Modes) {
        self.modes = modes;
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn hyperlink_at(&self, row: usize, column: usize) -> Option<&str> {
        let id = self.active_grid().hyperlink(row, column)?;
        self.hyperlinks.get(&id).map(String::as_str)
    }

    /// Returns pending OSC 52 writes. Reads are denied so child processes cannot inspect host data.
    pub fn take_clipboard_requests(&mut self) -> Vec<Osc52Write> {
        self.clipboard_requests.drain(..).collect()
    }

    pub fn take_shell_markers(&mut self) -> Vec<ShellMarker> {
        self.shell_markers.drain(..).collect()
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

    pub fn viewport_cell(&self, row: usize, column: usize) -> Option<Cell> {
        self.grid
            .active()
            .viewport_row(row)?
            .cells()
            .get(column)
            .copied()
    }

    pub fn viewport_grapheme(&self, row: usize, column: usize) -> Option<String> {
        self.grid.active().viewport_row(row)?.grapheme_at(column)
    }

    pub fn viewport_stored_grapheme(&self, row: usize, column: usize) -> Option<&str> {
        self.grid
            .active()
            .viewport_row(row)?
            .stored_grapheme_at(column)
    }

    pub fn viewport_shaping_context(&self, row: usize, column: usize) -> (String, String) {
        let Some(visible_row) = self.grid.active().viewport_row(row) else {
            return (String::new(), String::new());
        };
        if column >= visible_row.cells().len() {
            return (String::new(), String::new());
        }
        if visible_row.cells()[column].content == 0
            || visible_row.cells()[column]
                .flags()
                .contains(CellFlags::WIDE_SPACER)
        {
            return (String::new(), String::new());
        }
        let mut before = Vec::new();
        let mut before_bytes = 0;
        for index in (0..column.min(visible_row.cells().len())).rev() {
            let cell = visible_row.cells()[index];
            if cell.content == 0 && !cell.flags().contains(CellFlags::WIDE_SPACER) {
                break;
            }
            if cell.flags().contains(CellFlags::WIDE_SPACER) {
                continue;
            }
            let Some(text) = visible_row.grapheme_at(index) else {
                break;
            };
            if before_bytes + text.len() > 128 {
                break;
            }
            before_bytes += text.len();
            before.push(text);
        }
        before.reverse();
        let pre_context = before.concat();
        let mut post_context = String::new();
        let mut post_bytes = 0;
        for index in column.saturating_add(1)..visible_row.cells().len() {
            let cell = visible_row.cells()[index];
            if cell.content == 0 && !cell.flags().contains(CellFlags::WIDE_SPACER) {
                break;
            }
            if cell.flags().contains(CellFlags::WIDE_SPACER) {
                continue;
            }
            let Some(text) = visible_row.grapheme_at(index) else {
                break;
            };
            if post_bytes + text.len() > 128 {
                break;
            }
            post_bytes += text.len();
            post_context.push_str(&text);
        }
        (pre_context, post_context)
    }

    pub fn scrollback_len(&self) -> usize {
        self.grid.screen(ActiveScreen::Main).scrollback_len()
    }

    /// Rows retained above the visible screen on the main screen.
    pub fn scrollback_limit(&self) -> usize {
        self.grid.scrollback_limit()
    }

    /// Sets how much history survives above the visible screen. Applied only
    /// to the main screen: the alternate screen never records scrollback.
    pub fn set_scrollback_limit(&mut self, limit: usize) {
        self.grid.set_scrollback_limit(limit);
    }

    pub fn viewport_offset(&self) -> usize {
        if self.is_alternate_screen() {
            0
        } else {
            self.grid.active().viewport_offset()
        }
    }

    pub fn page_up(&mut self) {
        if self.is_alternate_screen() {
            return;
        }
        let amount = self.active_grid().height().saturating_sub(1).max(1);
        self.scroll_view_up(amount);
    }

    pub fn page_down(&mut self) {
        if self.is_alternate_screen() {
            return;
        }
        let amount = self.active_grid().height().saturating_sub(1).max(1);
        self.scroll_view_down(amount);
    }

    pub fn scroll_view_up(&mut self, rows: usize) {
        if !self.is_alternate_screen() && rows > 0 {
            let grid = self.grid.active_mut();
            grid.set_viewport_offset(grid.viewport_offset().saturating_add(rows));
        }
    }

    pub fn scroll_view_down(&mut self, rows: usize) {
        if !self.is_alternate_screen() && rows > 0 {
            let grid = self.grid.active_mut();
            grid.set_viewport_offset(grid.viewport_offset().saturating_sub(rows));
        }
    }

    pub fn scrollback_top(&mut self) {
        if !self.is_alternate_screen() {
            self.grid.active_mut().set_viewport_offset(usize::MAX);
        }
    }

    pub fn jump_to_bottom(&mut self) {
        if !self.is_alternate_screen() {
            self.grid.active_mut().set_viewport_offset(0);
        }
    }

    pub fn selection(&self) -> Option<(SelectionPoint, SelectionPoint)> {
        self.selection
    }

    pub fn set_selection(&mut self, anchor: SelectionPoint, focus: SelectionPoint) {
        let width = self.active_grid().width();
        let height = self.active_grid().height();
        let clamp = |point: SelectionPoint| SelectionPoint {
            row: point.row.min(height - 1),
            column: point.column.min(width - 1),
        };
        let next = (clamp(anchor), clamp(focus));
        let old = self.selection;
        self.selection = Some(next);
        for (start, end) in [old, Some(next)].into_iter().flatten() {
            for row in start.row.min(end.row)..=start.row.max(end.row) {
                self.grid.active_mut().mark_row_dirty(row);
            }
        }
    }

    pub fn select_line(&mut self, row: usize) {
        let last = self.active_grid().width() - 1;
        self.set_selection(
            SelectionPoint { row, column: 0 },
            SelectionPoint { row, column: last },
        );
    }

    pub fn select_word(&mut self, point: SelectionPoint) {
        let row = point.row.min(self.active_grid().height() - 1);
        let column = point.column.min(self.active_grid().width() - 1);
        let word_class = |column| {
            self.grid
                .active()
                .viewport_row(row)
                .and_then(|row| row.grapheme_at(column))
                .and_then(|text| text.chars().next())
                .is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
        };
        let class = word_class(column);
        let mut start = column;
        while start > 0 && word_class(start - 1) == class {
            start -= 1;
        }
        let mut end = column;
        while end + 1 < self.active_grid().width() && word_class(end + 1) == class {
            end += 1;
        }
        self.set_selection(
            SelectionPoint { row, column: start },
            SelectionPoint { row, column: end },
        );
    }

    pub fn clear_selection(&mut self) {
        if let Some((start, end)) = self.selection.take() {
            for row in start.row.min(end.row)..=start.row.max(end.row) {
                self.grid.active_mut().mark_row_dirty(row);
            }
        }
    }

    pub fn selection_text(&self) -> Option<String> {
        let (anchor, focus) = self.selection?;
        let (start, end) = if (anchor.row, anchor.column) <= (focus.row, focus.column) {
            (anchor, focus)
        } else {
            (focus, anchor)
        };
        let mut output = String::new();
        for row_index in start.row..=end.row {
            let row = self.grid.active().viewport_row(row_index)?;
            let first = if row_index == start.row {
                start.column
            } else {
                0
            };
            let last = if row_index == end.row {
                end.column
            } else {
                row.cells().len() - 1
            };
            let mut line = String::new();
            for column in first..=last {
                if let Some(grapheme) = row.grapheme_at(column) {
                    line.push_str(&grapheme);
                } else {
                    line.push(' ');
                }
            }
            output.push_str(line.trim_end());
            if row_index < end.row
                && self
                    .grid
                    .active()
                    .viewport_row(row_index + 1)
                    .is_some_and(|next| !next.is_wrapped())
            {
                output.push('\n');
            }
        }
        Some(output)
    }

    pub fn selection_cell_range(&self) -> Option<(u32, u32)> {
        let (anchor, focus) = self.selection?;
        let width = self.active_grid().width();
        let start = anchor.row * width + anchor.column;
        let end = focus.row * width + focus.column;
        Some((start.min(end) as u32, start.max(end) as u32))
    }

    pub fn encode_paste(&self, bytes: &[u8]) -> Vec<u8> {
        if !self.modes.bracketed_paste {
            return bytes.to_vec();
        }
        let mut output = Vec::with_capacity(bytes.len() + 12);
        output.extend_from_slice(b"\x1b[200~");
        output.extend_from_slice(bytes);
        output.extend_from_slice(b"\x1b[201~");
        output
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
        let old_width = self.tabs.len();
        self.tabs.resize(width, false);
        for col in old_width..width {
            self.tabs[col] = col != 0 && col % 8 == 0;
        }
        if let Some((anchor, focus)) = self.selection {
            let clamp = |point: SelectionPoint| SelectionPoint {
                row: point.row.min(height - 1),
                column: point.column.min(width - 1),
            };
            self.selection = Some((clamp(anchor), clamp(focus)));
        }
        self.top_margin = 0;
        self.bottom_margin = height - 1;
        self.pending_wrap = false;
        // `pending_wrap` belongs to the active screen and is invalidated by the
        // new geometry. `main_pending_wrap` is the saved pending-wrap state of
        // the main screen: when resizing from the alternate screen it must be
        // preserved, but on the main screen it is stale and reset.
        if !self.is_alternate_screen() {
            self.main_pending_wrap = false;
        }
        Ok(())
    }

    fn dispatch(&mut self, event: ParserEvent, replies: &mut Vec<u8>) {
        if !matches!(event, ParserEvent::Print(_)) {
            self.last_printed = None;
        }
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
                self.preceding_char = Some(ch);
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
                self.tab(1, false);
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
            b'=' => self.modes.application_keypad = true,
            b'>' => self.modes.application_keypad = false,
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
            b'H' => {
                let col = self.cursor().column;
                self.tabs[col] = true;
            }
            b'Z' => replies.extend_from_slice(b"\x1b[?1;2c"),
            b'c' => self.reset(),
            _ => {}
        }
    }

    fn csi(&mut self, csi: crate::Csi, replies: &mut Vec<u8>) {
        if csi.intermediate_len != 0 {
            if csi.private.is_none()
                && csi.final_byte == b'q'
                && csi.intermediate_len == 1
                && csi.intermediates[0] == b' '
            {
                self.set_cursor_style(csi.param(0, 0));
            }
            return;
        }
        if csi.private == Some(b'>') {
            if csi.final_byte == b'c' && csi.param(0, 0) == 0 {
                replies.extend_from_slice(b"\x1b[>0;1;0c");
            }
            return;
        }
        if csi.private.is_some() && csi.private != Some(b'?') {
            return;
        }
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
            b'h' | b'l' => {
                for index in 0..csi.param_count as usize {
                    if csi.param(index, 0) == 4 {
                        self.insert_mode = csi.final_byte == b'h';
                    }
                }
            }
            b'I' => self.tab(param_count(&csi, 0), false),
            b'Z' => self.tab(param_count(&csi, 0), true),
            b'g' => match csi.param(0, 0) {
                0 => {
                    let col = self.cursor().column;
                    self.tabs[col] = false;
                }
                3 => self.tabs.fill(false),
                _ => {}
            },
            b'b' => {
                if let Some(ch) = self.preceding_char {
                    for _ in 0..param_count(&csi, 0) {
                        self.put_char(ch);
                    }
                }
            }
            b'X' => {
                let cursor = self.cursor();
                self.erase_cells(
                    cursor.row,
                    cursor.column..cursor.column.saturating_add(param_count(&csi, 0)),
                );
            }
            b'S' => self.scroll_up(self.top_margin, self.bottom_margin, param_count(&csi, 0)),
            b'T' if csi.param_count <= 1 => {
                self.scroll_down(self.top_margin, self.bottom_margin, param_count(&csi, 0))
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
                self.erase_cells(
                    cursor.row,
                    cursor.column..cursor.column.saturating_add(param_count(&csi, 0)),
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
                let width = self.active_grid().width();
                let amount = param_count(&csi, 0).min(width - cursor.column);
                self.erase_cells(cursor.row, width - amount..width);
                self.pending_wrap = false;
            }
            b'L' => {
                let cursor = self.cursor();
                if (self.top_margin..=self.bottom_margin).contains(&cursor.row) {
                    let bottom = self.bottom_margin;
                    self.active_grid_mut()
                        .insert_rows(cursor.row, bottom, param_count(&csi, 0));
                    let amount = param_count(&csi, 0).min(bottom - cursor.row + 1);
                    self.erase_rows(cursor.row..cursor.row + amount);
                }
            }
            b'M' => {
                let cursor = self.cursor();
                if (self.top_margin..=self.bottom_margin).contains(&cursor.row) {
                    let bottom = self.bottom_margin;
                    self.active_grid_mut()
                        .delete_rows(cursor.row, bottom, param_count(&csi, 0));
                    let amount = param_count(&csi, 0).min(bottom - cursor.row + 1);
                    self.erase_rows(bottom + 1 - amount..bottom + 1);
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
        let scalar_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if let Some((screen, row, column)) = self.last_printed
            && screen == self.grid.active_screen()
            && let Some(cell) = self.active_grid().cell(row, column)
        {
            let stored = self.active_grid().stored_grapheme(row, column);
            let previous_scalar = char::from_u32(cell.content);
            let regional_indicator = ('\u{1f1e6}'..='\u{1f1ff}').contains(&ch);
            let extends = scalar_width == 0
                || ('\u{1f3fb}'..='\u{1f3ff}').contains(&ch)
                || stored.is_some()
                || (regional_indicator
                    && previous_scalar
                        .is_some_and(|previous| ('\u{1f1e6}'..='\u{1f1ff}').contains(&previous)))
                || (is_hangul_jamo(ch) && previous_scalar.is_some_and(is_hangul_jamo));
            if extends {
                let mut combined = stored.map_or_else(
                    || previous_scalar.map_or_else(String::new, |previous| previous.to_string()),
                    str::to_owned,
                );
                if combined.len() + ch.len_utf8() <= 256 {
                    combined.push(ch);
                } else {
                    return;
                }
                if combined.graphemes(true).count() == 1 {
                    let mut cell = self
                        .active_grid()
                        .cell(row, column)
                        .expect("last cell remains visible");
                    let width = UnicodeWidthStr::width(combined.as_str());
                    if width == 2 && cell.flags().contains(CellFlags::WIDE_LEADING) {
                        let link = self.active_grid().hyperlink(row, column);
                        self.active_grid_mut()
                            .set_wide_grapheme(row, column, &combined, cell);
                        if let Some(link) = link {
                            self.active_grid_mut()
                                .set_hyperlink(row, column, Some(link));
                        }
                        return;
                    }
                    if width == 2 && column + 1 < self.active_grid().width() {
                        let link = self.active_grid().hyperlink(row, column);
                        self.active_grid_mut()
                            .set_wide_grapheme(row, column, &combined, cell);
                        if let Some(link) = link {
                            self.active_grid_mut()
                                .set_hyperlink(row, column, Some(link));
                        }
                        self.set_cursor(column + 2, row);
                        self.pending_wrap =
                            self.modes.auto_wrap && column + 2 >= self.active_grid().width();
                        return;
                    }
                    if width <= 1 && !cell.flags().contains(CellFlags::WIDE_LEADING) {
                        let link = self.active_grid().hyperlink(row, column);
                        cell.content = combined.chars().next().expect("nonempty grapheme") as u32;
                        self.active_grid_mut()
                            .set_grapheme(row, column, &combined, cell);
                        if let Some(link) = link {
                            self.active_grid_mut()
                                .set_hyperlink(row, column, Some(link));
                        }
                        return;
                    }
                }
            }
        }
        let width = scalar_width;
        if width == 0 {
            return;
        }
        if width > 2 {
            return;
        }
        if width == 2 && self.active_grid().width() == 1 {
            self.write_single('\u{fffd}');
            return;
        }
        if width == 2 && self.cursor().column + 1 >= self.active_grid().width() {
            if self.modes.auto_wrap {
                self.pending_wrap = true;
            } else {
                self.write_single('\u{fffd}');
                return;
            }
        }
        if width == 2 {
            self.put_wide_char(ch);
        } else {
            self.write_single(ch);
        }
    }

    fn put_wide_char(&mut self, ch: char) {
        if self.pending_wrap && self.modes.auto_wrap {
            let row = self.cursor().row;
            let top = self.top_margin;
            let bottom = self.bottom_margin;
            if row == bottom {
                self.scroll_up(top, bottom, 1);
                self.set_cursor(0, row);
            } else if row + 1 < self.active_grid().height() {
                self.set_cursor(0, row + 1);
            }
            let continuation = self.cursor().row;
            self.active_grid_mut().set_row_wrapped(continuation, true);
        }
        self.pending_wrap = false;
        let cursor = self.cursor();
        if self.insert_mode {
            self.active_grid_mut()
                .insert_cells(cursor.row, cursor.column, 2);
        }
        let cell = Cell::new(
            ch as u32,
            self.rendition.fg,
            self.rendition.bg,
            CellFlags::from_bits_retain(self.rendition.flags),
        );
        if !self
            .active_grid_mut()
            .set_wide_cell(cursor.row, cursor.column, cell)
        {
            return;
        }
        if let Some(id) = self.current_hyperlink {
            self.active_grid_mut()
                .set_hyperlink(cursor.row, cursor.column, Some(id));
        }
        self.last_printed = Some((self.grid.active_screen(), cursor.row, cursor.column));
        if cursor.column + 2 >= self.active_grid().width() {
            if self.modes.auto_wrap {
                self.pending_wrap = true;
            }
        } else {
            self.set_cursor(cursor.column + 2, cursor.row);
        }
    }

    fn write_single(&mut self, ch: char) {
        if self.pending_wrap && self.modes.auto_wrap {
            let row = self.cursor().row;
            if row == self.bottom_margin {
                let (top, bottom) = (self.top_margin, self.bottom_margin);
                self.scroll_up(top, bottom, 1);
                self.set_cursor(0, row);
            } else if row + 1 < self.active_grid().height() {
                self.set_cursor(0, row + 1);
            }
            let continuation = self.cursor().row;
            self.active_grid_mut().set_row_wrapped(continuation, true);
        }
        self.pending_wrap = false;
        let cursor = self.cursor();
        if self.insert_mode {
            self.active_grid_mut()
                .insert_cells(cursor.row, cursor.column, 1);
        }
        let cell = Cell::new(
            ch as u32,
            self.rendition.fg,
            self.rendition.bg,
            CellFlags::from_bits_retain(self.rendition.flags),
        );
        self.active_grid_mut()
            .set_cell(cursor.row, cursor.column, cell);
        if let Some(id) = self.current_hyperlink {
            self.active_grid_mut()
                .set_hyperlink(cursor.row, cursor.column, Some(id));
        }
        self.last_printed = Some((self.grid.active_screen(), cursor.row, cursor.column));
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
            self.scroll_up(top, bottom, 1);
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
            self.scroll_down(top, bottom, 1);
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

    fn erase_cells(&mut self, row: usize, columns: std::ops::Range<usize>) {
        // Erase operations use the current colors, never the glyph's attributes.
        let blank = Cell::new(
            0,
            self.rendition.fg,
            self.rendition.bg,
            CellFlags::default(),
        );
        self.active_grid_mut()
            .erase_range_in_row(row, columns, blank);
    }

    fn erase_rows(&mut self, rows: std::ops::Range<usize>) {
        let width = self.active_grid().width();
        for row in rows {
            self.erase_cells(row, 0..width);
        }
    }

    fn erase_display(&mut self, mode: u16) {
        let cursor = self.cursor();
        let width = self.active_grid().width();
        let height = self.active_grid().height();
        match mode {
            0 => {
                self.erase_cells(cursor.row, cursor.column..width);
                self.erase_rows(cursor.row + 1..height);
            }
            1 => {
                self.erase_rows(0..cursor.row);
                self.erase_cells(cursor.row, 0..cursor.column + 1);
            }
            2 => self.erase_rows(0..height),
            3 => self.active_grid_mut().clear_scrollback(),
            _ => {}
        }
    }

    fn erase_line(&mut self, mode: u16) {
        let cursor = self.cursor();
        let width = self.active_grid().width();
        match mode {
            0 => self.erase_cells(cursor.row, cursor.column..width),
            1 => self.erase_cells(cursor.row, 0..cursor.column + 1),
            2 => self.erase_cells(cursor.row, 0..width),
            _ => {}
        }
    }

    fn tab(&mut self, count: usize, backwards: bool) {
        let cursor = self.cursor();
        let mut col = cursor.column;
        for _ in 0..count.min(self.tabs.len()) {
            col = if backwards {
                (0..col).rev().find(|&pos| self.tabs[pos]).unwrap_or(0)
            } else {
                (col + 1..self.tabs.len())
                    .find(|&pos| self.tabs[pos])
                    .unwrap_or(self.tabs.len() - 1)
            };
        }
        self.set_cursor(col, cursor.row);
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

    /// Applies DECSCUSR (`CSI Ps SP q`); unknown values keep the prior style.
    fn set_cursor_style(&mut self, style: u16) {
        let (shape, blink) = match style {
            0 | 1 => (CursorStyle::Block, true),
            2 => (CursorStyle::Block, false),
            3 => (CursorStyle::Underline, true),
            4 => (CursorStyle::Underline, false),
            5 => (CursorStyle::Bar, true),
            6 => (CursorStyle::Bar, false),
            _ => return,
        };
        self.modes.cursor_style = shape;
        self.modes.cursor_blink = blink;
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
            self.clear_selection();
            if save_main {
                self.alternate_saved_cursor = Some(self.save_cursor());
                self.alternate_1049_active = true;
            }
            self.main_pending_wrap = self.pending_wrap;
            self.grid.select_screen(ActiveScreen::Alternate);
            self.erase_rows(0..self.active_grid().height());
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
                            index += 2;
                            Some(indexed_color(csi.param(index - 1, 0).min(255) as u8))
                        }
                        2 if index + 3 < csi.param_count as usize => {
                            let r = csi.param(index + 1, 0).min(255) as u32;
                            let g = csi.param(index + 2, 0).min(255) as u32;
                            let b = csi.param(index + 3, 0).min(255) as u32;
                            index += 4;
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
        } else if command == "8" {
            let rest = &bytes[separator + 1..];
            let Some(parameter_end) = rest.iter().position(|&byte| byte == b';') else {
                return;
            };
            let uri = &rest[parameter_end + 1..];
            if uri.is_empty() {
                self.current_hyperlink = None;
            } else if uri.len() <= 2048
                && let Ok(uri) = std::str::from_utf8(uri)
                && self.hyperlinks.len() < 4096
            {
                let id = self.next_hyperlink;
                self.next_hyperlink = self.next_hyperlink.saturating_add(1).max(1);
                self.hyperlinks.insert(id, uri.to_owned());
                self.current_hyperlink = Some(id);
            }
        } else if command == "52" {
            let rest = &bytes[separator + 1..];
            let Some(data_separator) = rest.iter().position(|&byte| byte == b';') else {
                return;
            };
            let selection = &rest[..data_separator];
            let payload = &rest[data_separator + 1..];
            if payload != b"?"
                && payload.len() <= 4096
                && selection.len() == 1
                && matches!(selection[0], b'c' | b'p' | b's' | b'0'..=b'7')
                && self.clipboard_requests.len() < 16
            {
                self.clipboard_requests.push_back(Osc52Write {
                    selection: selection.to_vec(),
                    base64: payload.to_vec(),
                });
            }
        } else if command == "133" {
            let marker = match bytes.get(separator + 1) {
                Some(b'A') => Some(ShellMarker::PromptStart),
                Some(b'B') => Some(ShellMarker::CommandStart),
                Some(b'C') => Some(ShellMarker::OutputStart),
                Some(b'D') => {
                    let status = bytes
                        .get(separator + 2..)
                        .and_then(|rest| rest.strip_prefix(b";"))
                        .and_then(|status| std::str::from_utf8(status).ok())
                        .and_then(|status| status.parse().ok());
                    Some(ShellMarker::CommandFinished(status))
                }
                _ => None,
            };
            if let Some(marker) = marker
                && self.shell_markers.len() < 1024
            {
                self.shell_markers.push_back(marker);
            }
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
        self.last_printed = None;
        self.preceding_char = None;
        self.tabs = (0..self.active_grid().width())
            .map(|col| col != 0 && col % 8 == 0)
            .collect();
        self.insert_mode = false;
        self.current_hyperlink = None;
        self.selection = None;
    }

    fn active_grid_mut(&mut self) -> &mut Grid {
        self.grid.active_mut()
    }

    fn scroll_up(&mut self, top: usize, bottom: usize, count: usize) {
        let full_screen = top == 0 && bottom + 1 == self.active_grid().height();
        if full_screen && !self.is_alternate_screen() {
            self.active_grid_mut().scroll_up_recording(count);
        } else {
            self.active_grid_mut().scroll_up(top, bottom, count);
        }
        let amount = count.min(bottom - top + 1);
        self.erase_rows(bottom + 1 - amount..bottom + 1);
    }

    fn scroll_down(&mut self, top: usize, bottom: usize, count: usize) {
        self.active_grid_mut().scroll_down(top, bottom, count);
        self.erase_rows(top..top + count.min(bottom - top + 1));
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

fn is_hangul_jamo(ch: char) -> bool {
    matches!(ch as u32, 0x1100..=0x11ff | 0xa960..=0xa97f | 0xd7b0..=0xd7ff)
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
    #[test]
    fn mixed_tui_updates_resize_and_chunking_preserve_wide_cell_ownership() {
        let mut whole = terminal(40, 12);
        let mut split = terminal(40, 12);
        let updates = [
            "i界e\u{301}",
            "\x1b[44m\x1b[2X",
            "\x1b[3@",
            "\x1b[2P",
            "\x1b[2S",
            "\x1b[T",
            "\x1b[2;8r",
            "\x1b[?6h\x1b[4;5H",
            "\x1b[?6l\x1b[r",
            "\x1b[?1049h",
            "\x1b[?1049l",
            "\x1b[4hAB界\x1b[4l",
            "\x1b[2L",
            "\x1b[M",
            "\r\n\t",
            "\x1b[2J",
            "\x1b[3b",
            "\x1b[3g\x1bH\x1b[2I\x1b[Z",
            "\x1b]0;Ü title\x07",
        ];
        let mut seed = 0x7f4a_7c15u32;
        for step in 0..2048 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let update = updates[seed as usize % updates.len()].as_bytes();
            whole.process(update);
            for byte in update {
                split.process(&[*byte]);
            }
            if step % 31 == 0 {
                let width = 2 + seed as usize % 60;
                let height = 1 + (seed >> 8) as usize % 20;
                whole.resize(width, height).unwrap();
                split.resize(width, height).unwrap();
            }
            assert_eq!(whole.active_grid(), split.active_grid(), "step {step}");
            assert_eq!(whole.cursor(), split.cursor());
            for screen in [ActiveScreen::Main, ActiveScreen::Alternate] {
                let grid = whole.grid.screen(screen);
                for row in 0..grid.height() {
                    for column in 0..grid.width() {
                        let cell = grid.cell(row, column).unwrap();
                        if cell.flags().contains(CellFlags::WIDE_LEADING) {
                            assert!(
                                column + 1 < grid.width()
                                    && grid
                                        .cell(row, column + 1)
                                        .unwrap()
                                        .flags()
                                        .contains(CellFlags::WIDE_SPACER),
                                "orphan lead at step {step}"
                            );
                        }
                        if cell.flags().contains(CellFlags::WIDE_SPACER) {
                            assert!(
                                column > 0
                                    && grid
                                        .cell(row, column - 1)
                                        .unwrap()
                                        .flags()
                                        .contains(CellFlags::WIDE_LEADING),
                                "orphan spacer at step {step}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn ncurses_erasure_and_insert_delete_keep_the_active_background() {
        let mut t = terminal(8, 4);
        feed(&mut t, b"\x1b[42m\x1b[2J\x1b[1;1Habcd\x1b[1;2H\x1b[2X");
        assert_eq!(line(&t, 0), "a..d....");
        for row in 0..4 {
            for col in 0..8 {
                assert_eq!(t.active_grid().cell(row, col).unwrap().bg, indexed_color(2));
            }
        }
        feed(&mut t, b"\x1b[3;1Hxyz\x1b[3;2H\x1b[@\x1b[P\x1b[4;1H\n");
        assert_eq!(t.active_grid().cell(3, 0).unwrap().bg, indexed_color(2));
        feed(&mut t, b"\x1b[2;1H\x1b[L\x1b[M\x1b[1;1H\x1b[T");
        assert_eq!(t.active_grid().cell(0, 7).unwrap().bg, indexed_color(2));
    }

    #[test]
    fn ncurses_repeat_scroll_and_tabs_obey_terminal_geometry() {
        let mut t = terminal(20, 4);
        feed(&mut t, b"i\x1b[3b\x1b[2;1Hsecond\x1b[3;1Hthird\x1b[S");
        assert!(line(&t, 0).starts_with("second"));
        feed(&mut t, b"\x1b[T\x1b[1;1H\x1b[3g\x1b[5G\x1bH\r\tX");
        assert_eq!(t.active_grid().cell(0, 4).unwrap().content, 'X' as u32);
        feed(&mut t, b"\x1b[Z");
        assert_eq!(t.cursor().column, 4);
        feed(&mut t, b"\x1b[1;1H\x1b[4hZ\x1b[4l");
        assert_eq!(t.active_grid().cell(0, 5).unwrap().content, 'X' as u32);
        let mut repeated = terminal(8, 2);
        feed(&mut repeated, b"i\x1b[3b");
        assert_eq!(line(&repeated, 0), "iiii....");
    }

    #[test]
    fn unsupported_csi_prefixes_do_not_change_rendition_or_cursor() {
        let mut t = terminal(5, 2);
        feed(&mut t, b"\x1b[31m\x1b[2$H\x1b[0$mX");
        assert_eq!(t.active_grid().cell(0, 0).unwrap().fg, indexed_color(1));
        assert_eq!(feed(&mut t, b"\x1b[>c"), b"\x1b[>0;1;0c");
    }
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
    fn scrollback_limit_follows_configuration_and_alternate_screen_is_unaffected() {
        let mut terminal = terminal(4, 2);
        terminal.set_scrollback_limit(2);
        assert_eq!(terminal.scrollback_limit(), 2);
        for _ in 0..6 {
            feed(&mut terminal, b"line\r\n");
        }
        assert_eq!(terminal.scrollback_len(), 2);

        // The alternate screen holds no history, so entering it changes
        // nothing about how much the main screen keeps.
        feed(&mut terminal, b"\x1b[?1049h");
        assert!(terminal.is_alternate_screen());
        feed(&mut terminal, b"alt\r\n");
        assert_eq!(terminal.scrollback_len(), 2);
        feed(&mut terminal, b"\x1b[?1049l");
        assert!(!terminal.is_alternate_screen());
        assert_eq!(terminal.scrollback_limit(), 2);
    }

    #[test]
    fn configured_modes_are_seeded_then_application_wins() {
        let mut terminal = terminal(6, 2);
        terminal.set_modes(crate::Modes {
            cursor_style: CursorStyle::Bar,
            cursor_blink: false,
            ..terminal.modes()
        });
        assert_eq!(terminal.modes().cursor_style, CursorStyle::Bar);
        // DECSCUSR from the application overrides the seeded preference.
        feed(&mut terminal, b"\x1b[4 q");
        assert_eq!(terminal.modes().cursor_style, CursorStyle::Underline);
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
    fn combining_and_zwj_sequences_remain_single_semantic_cells() {
        let mut terminal = terminal(5, 2);
        feed(&mut terminal, "e\u{301}👩\u{200d}🚀".as_bytes());
        assert_eq!(
            terminal.active_grid().grapheme(0, 0).as_deref(),
            Some("e\u{301}")
        );
        assert_eq!(
            terminal.active_grid().grapheme(0, 1).as_deref(),
            Some("👩\u{200d}🚀")
        );
        assert!(
            terminal
                .active_grid()
                .cell(0, 1)
                .unwrap()
                .flags()
                .contains(CellFlags::WIDE_LEADING)
        );
        assert!(
            terminal
                .active_grid()
                .cell(0, 2)
                .unwrap()
                .flags()
                .contains(CellFlags::WIDE_SPACER)
        );
        assert_eq!(terminal.cursor(), Cursor { column: 3, row: 0 });
    }

    #[test]
    fn regional_indicator_skin_tone_and_hangul_clusters_stay_atomic() {
        let mut terminal = terminal(8, 2);
        feed(&mut terminal, "🇵🇪👍🏽가".as_bytes());
        assert_eq!(terminal.active_grid().grapheme(0, 0).as_deref(), Some("🇵🇪"));
        assert_eq!(terminal.active_grid().grapheme(0, 2).as_deref(), Some("👍🏽"));
        assert_eq!(terminal.active_grid().grapheme(0, 4).as_deref(), Some("가"));
        for column in [0, 2, 4] {
            assert!(
                terminal
                    .active_grid()
                    .cell(0, column)
                    .unwrap()
                    .flags()
                    .contains(CellFlags::WIDE_LEADING)
            );
            assert!(
                terminal
                    .active_grid()
                    .cell(0, column + 1)
                    .unwrap()
                    .flags()
                    .contains(CellFlags::WIDE_SPACER)
            );
        }
    }

    #[test]
    fn shaping_context_stays_with_the_same_unbroken_text_run() {
        let mut terminal = terminal(8, 1);
        feed(&mut terminal, "سلام".as_bytes());
        assert_eq!(
            terminal.viewport_shaping_context(0, 1),
            ("س".to_owned(), "ام".to_owned())
        );
        assert_eq!(
            terminal.viewport_shaping_context(0, 4),
            (String::new(), String::new())
        );
    }

    #[test]
    fn grapheme_storage_has_a_fixed_per_cell_byte_limit() {
        let mut terminal = terminal(4, 1);
        let mut input = "a".to_owned();
        input.push_str(&"\u{301}".repeat(1000));
        feed(&mut terminal, input.as_bytes());
        assert!(terminal.active_grid().grapheme(0, 0).unwrap().len() <= 256);
    }

    #[test]
    fn wide_character_wraps_before_last_column_and_preserves_pair_invariant() {
        let mut terminal = terminal(3, 2);
        feed(&mut terminal, "AB界".as_bytes());
        assert_eq!(line(&terminal, 0), "AB.");
        assert_eq!(line(&terminal, 1), "界..");
        assert!(terminal.active_grid().row_at(1).unwrap().is_wrapped());
        assert!(
            terminal
                .active_grid()
                .cell(1, 1)
                .unwrap()
                .flags()
                .contains(CellFlags::WIDE_SPACER)
        );
    }

    #[test]
    fn narrow_grids_count_terminal_cell_widths_not_characters() {
        // `A界😀B` is four characters but six terminal columns (1+2+2+1
        // per the same `unicode-width` table `put_char` uses). Column
        // ownership must follow cell widths, not character counts.
        assert_eq!(UnicodeWidthStr::width("A界😀B"), 6);

        // Six columns hold the whole sequence: leading/spacer pairs for
        // the wide glyphs and B in the last column.
        let mut fitting = terminal(6, 1);
        feed(&mut fitting, "A界😀B".as_bytes());
        let row = fitting.active_grid().row_at(0).unwrap();
        assert_eq!(row.cells()[0].content, 'A' as u32);
        assert!(row.cells()[1].flags().contains(CellFlags::WIDE_LEADING));
        assert!(row.cells()[2].flags().contains(CellFlags::WIDE_SPACER));
        assert!(row.cells()[3].flags().contains(CellFlags::WIDE_LEADING));
        assert!(row.cells()[4].flags().contains(CellFlags::WIDE_SPACER));
        assert_eq!(row.cells()[5].content, 'B' as u32);
        assert!(!row.is_wrapped());
        assert_eq!(fitting.cursor(), Cursor { column: 5, row: 0 });
        assert_eq!(fitting.scrollback_len(), 0);

        // One column short: `A界😀` fills the row exactly (1+2+2 = 5),
        // `B` wraps, and the single-row terminal scrolls the full row
        // into scrollback instead of dropping it silently.
        let mut narrow = terminal(5, 1);
        feed(&mut narrow, "A界😀B".as_bytes());
        let row = narrow.active_grid().row_at(0).unwrap();
        assert_eq!(row.cells()[0].content, 'B' as u32);
        assert!(
            row.cells()[1..]
                .iter()
                .all(|cell| cell.content == 0 && cell.flags() == CellFlags::default()),
            "only the wrapped B remains on the fresh row"
        );
        assert!(row.is_wrapped());
        assert_eq!(narrow.scrollback_len(), 1);
        assert_eq!(narrow.cursor(), Cursor { column: 1, row: 0 });
    }

    #[test]
    fn full_screen_output_builds_scrollback_and_viewport_does_not_move_live_cursor() {
        let mut terminal = terminal(4, 2);
        feed(&mut terminal, b"1\r\n2\r\n3\r\n4");
        assert_eq!(terminal.scrollback_len(), 2);
        assert_eq!(terminal.viewport_offset(), 0);
        let live_cursor = terminal.cursor();
        terminal.page_up();
        assert_eq!(terminal.viewport_offset(), 1);
        assert_eq!(terminal.viewport_cell(0, 0).unwrap().content, b'2' as u32);
        assert_eq!(terminal.viewport_cell(1, 0).unwrap().content, b'3' as u32);
        assert_eq!(terminal.cursor(), live_cursor);
        feed(&mut terminal, b"\r\n5");
        assert_eq!(terminal.viewport_cell(0, 0).unwrap().content, b'2' as u32);
        terminal.page_up();
        assert_eq!(terminal.viewport_cell(0, 0).unwrap().content, b'1' as u32);
        terminal.page_down();
        assert_eq!(terminal.viewport_offset(), 2);
        terminal.jump_to_bottom();
        assert_eq!(terminal.viewport_offset(), 0);
    }

    #[test]
    fn scrollback_line_and_home_end_controls_clamp_to_available_history() {
        let mut terminal = terminal(4, 2);
        feed(&mut terminal, b"1\r\n2\r\n3\r\n4\r\n5");
        terminal.scroll_view_up(1);
        assert_eq!(terminal.viewport_cell(0, 0).unwrap().content, b'3' as u32);
        terminal.scroll_view_up(usize::MAX);
        assert_eq!(terminal.viewport_cell(0, 0).unwrap().content, b'1' as u32);
        terminal.scroll_view_down(1);
        assert_eq!(terminal.viewport_cell(0, 0).unwrap().content, b'2' as u32);
        terminal.jump_to_bottom();
        assert_eq!(terminal.viewport_offset(), 0);
        terminal.scrollback_top();
        assert!(terminal.viewport_offset() > 0);
        terminal.jump_to_bottom();
        feed(&mut terminal, b"\x1b[?1049h");
        terminal.scrollback_top();
        assert_eq!(terminal.viewport_offset(), 0);
    }

    #[test]
    fn alternate_screen_scrolling_never_adds_to_main_scrollback() {
        let mut terminal = terminal(3, 2);
        feed(&mut terminal, b"1\r\n2\r\n3");
        let history = terminal.scrollback_len();
        feed(&mut terminal, b"\x1b[?1049hA\r\nB\r\nC\x1b[?1049l");
        assert_eq!(terminal.scrollback_len(), history);
    }

    #[test]
    fn osc8_links_are_cell_semantics_and_close_for_following_text() {
        let mut terminal = terminal(12, 1);
        feed(
            &mut terminal,
            b"\x1b]8;id=x;https://example.test\x1b\\link\x1b]8;;\x1b\\plain",
        );
        for column in 0..4 {
            assert_eq!(
                terminal.hyperlink_at(0, column),
                Some("https://example.test")
            );
        }
        assert_eq!(terminal.hyperlink_at(0, 4), None);
        assert_eq!(
            terminal.active_grid().cell(0, 0).unwrap().content,
            b'l' as u32
        );
    }

    #[test]
    fn osc52_allows_bounded_writes_but_never_clipboard_reads() {
        let mut terminal = terminal(8, 2);
        feed(&mut terminal, b"\x1b]52;c;aGVsbG8=\x1b\\\x1b]52;c;?\x1b\\");
        assert_eq!(
            terminal.take_clipboard_requests(),
            vec![Osc52Write {
                selection: b"c".to_vec(),
                base64: b"aGVsbG8=".to_vec(),
            }]
        );
        let oversized = [b"\x1b]52;c;".as_slice(), &vec![b'A'; 4100], b"\x1b\\"].concat();
        feed(&mut terminal, &oversized);
        assert!(terminal.take_clipboard_requests().is_empty());
    }

    #[test]
    fn paste_bytes_follow_bracketed_paste_mode() {
        let mut terminal = terminal(8, 2);
        assert_eq!(terminal.encode_paste(b"a\nb"), b"a\nb");
        feed(&mut terminal, b"\x1b[?2004h");
        assert_eq!(terminal.encode_paste(b"a\nb"), b"\x1b[200~a\nb\x1b[201~");
    }

    #[test]
    fn osc133_emits_bounded_shell_integration_markers() {
        let mut terminal = terminal(8, 2);
        feed(
            &mut terminal,
            b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\\x1b]133;C\x1b\\\x1b]133;D;7\x1b\\",
        );
        assert_eq!(
            terminal.take_shell_markers(),
            vec![
                ShellMarker::PromptStart,
                ShellMarker::CommandStart,
                ShellMarker::OutputStart,
                ShellMarker::CommandFinished(Some(7)),
            ]
        );
    }

    #[test]
    fn selection_is_external_to_cells_and_extracts_graphemes_and_logical_lines() {
        let mut terminal = terminal(8, 2);
        feed(&mut terminal, "café\r\nnext".as_bytes());
        let before = terminal.active_grid().cell(0, 0);
        terminal.set_selection(
            SelectionPoint { row: 0, column: 0 },
            SelectionPoint { row: 1, column: 3 },
        );
        assert_eq!(terminal.selection_text().as_deref(), Some("café\nnext"));
        assert_eq!(terminal.active_grid().cell(0, 0), before);
        assert_eq!(terminal.selection_cell_range(), Some((0, 11)));
        terminal.clear_selection();
        assert!(terminal.selection().is_none());
    }

    #[test]
    fn font_size_resize_preserves_state_without_clearing() {
        // Simulates the font-zoom path: same columns×rows, pixel geometry
        // changes underneath. Content, modes, cursor, alternate screen,
        // and scrollback must all survive.
        let mut terminal = terminal(10, 3);
        feed(&mut terminal, b"hello\x1b[1mB\x1b[0m\r\nworld");
        feed(&mut terminal, b"\x1b[?1049hALT");
        assert!(terminal.is_alternate_screen());
        feed(&mut terminal, b"\x1b[?1049l");
        assert!(!terminal.is_alternate_screen());
        let scrollback_before = terminal.scrollback_len();
        let modes_before = terminal.modes();
        // Wide + combining state must survive a same-size resize.
        feed(&mut terminal, "界e\u{301}".as_bytes());
        let cursor_before = terminal.cursor();
        terminal.resize(10, 3).unwrap();
        assert_eq!(terminal.cursor(), cursor_before);
        assert_eq!(terminal.modes(), modes_before);
        assert_eq!(terminal.scrollback_len(), scrollback_before);
        assert!(!terminal.is_alternate_screen());
        // Grid still honors wide-leading/spacer pairing after resize.
        let row = terminal.active_grid().row_at(cursor_before.row).unwrap();
        assert!(row.cells().iter().any(|cell| cell.content != 0));
    }

    #[test]
    fn resize_does_not_join_independent_rows_and_reflows_wrapped_text() {
        let mut independent = terminal(10, 3);
        feed(&mut independent, b"AAAAAAAAAA\r\nBBBB");
        independent.resize(20, 3).unwrap();
        assert_eq!(line(&independent, 0), "AAAAAAAAAA..........");
        assert_eq!(&line(&independent, 1)[..4], "BBBB");

        let mut wrapped = terminal(10, 3);
        feed(&mut wrapped, b"AAAAAAAAAAA");
        assert!(wrapped.active_grid().row_at(1).unwrap().is_wrapped());
        wrapped.resize(20, 3).unwrap();
        assert_eq!(&line(&wrapped, 0)[..11], "AAAAAAAAAAA");
        assert!(!wrapped.active_grid().row_at(1).unwrap().is_wrapped());
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
    fn extended_color_components_are_fully_consumed() {
        // A trailing zero component must not reset the rendition, and a
        // 256-palette index matching an SGR code must not set its flag.
        let mut terminal = terminal(3, 1);
        feed(&mut terminal, b"\x1b[38;2;255;0;0mA\x1b[38;5;9mB");
        let row = terminal.active_grid().row_at(0).unwrap();
        let a = row.cells()[0];
        let b = row.cells()[1];
        assert_eq!(a.fg, RGB_COLOR_TAG | 0xff_00_00);
        assert_eq!(a.flags, 0);
        assert_eq!(b.fg, INDEXED_COLOR_TAG | 9);
        assert_eq!(b.flags, 0);
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
    fn resize_while_alternate_preserves_main_pending_wrap() {
        // Regression test: resize while on the alternate screen must not
        // destroy `main_pending_wrap`, which holds the main screen's saved
        // pending-wrap state. Before the fix, resize unconditionally cleared
        // it, so exiting the alternate screen lost the wrap and overwrote the
        // right-margin character instead of advancing to the next line.
        let mut terminal = terminal(4, 2);
        // Fill the main screen row to the right margin, creating a real
        // pending-wrap condition (pending_wrap == true).
        feed(&mut terminal, b"ABCD");
        assert_eq!(terminal.cursor(), Cursor { column: 3, row: 0 });
        // Enter alternate screen: saves main_pending_wrap and resets
        // pending_wrap. Mode 47 does not save/restore the main cursor, so the
        // main screen cursor stays at (3, 0).
        feed(&mut terminal, b"\x1b[?47h");
        assert!(terminal.is_alternate_screen());
        assert_eq!(terminal.cursor(), Cursor { column: 0, row: 0 });
        // Resize while on the alternate screen: main_pending_wrap must survive.
        terminal.resize(4, 2).unwrap();
        assert!(terminal.is_alternate_screen());
        // Exit alternate screen: restores pending_wrap from main_pending_wrap.
        feed(&mut terminal, b"\x1b[?47l");
        assert!(!terminal.is_alternate_screen());
        assert_eq!(terminal.cursor(), Cursor { column: 3, row: 0 });
        // If main_pending_wrap was preserved, pending_wrap is still true, so the
        // next printable wraps to the next line instead of overwriting the
        // character at the right margin.
        feed(&mut terminal, b"E");
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
    fn cursor_style_follows_decscusr_and_survives_screen_switches() {
        let mut terminal = terminal(4, 2);
        assert_eq!(terminal.modes().cursor_style, CursorStyle::Block);
        assert!(terminal.modes().cursor_blink);
        feed(&mut terminal, b"\x1b[4 q");
        assert_eq!(terminal.modes().cursor_style, CursorStyle::Underline);
        assert!(!terminal.modes().cursor_blink);
        feed(&mut terminal, b"\x1b[5 q");
        assert_eq!(terminal.modes().cursor_style, CursorStyle::Bar);
        assert!(terminal.modes().cursor_blink);
        feed(&mut terminal, b"\x1b[2 q");
        assert_eq!(terminal.modes().cursor_style, CursorStyle::Block);
        assert!(!terminal.modes().cursor_blink);
        // Unknown Ps values and a missing space intermediate keep the style.
        feed(&mut terminal, b"\x1b[9 q\x1b[3q");
        assert_eq!(terminal.modes().cursor_style, CursorStyle::Block);
        // The style is terminal-global: alternate screen entry and exit keep it.
        feed(&mut terminal, b"\x1b[3 q\x1b[?1049h\x1b[?1049l");
        assert_eq!(terminal.modes().cursor_style, CursorStyle::Underline);
        // DECTCEM visibility stays independent of the shape.
        feed(&mut terminal, b"\x1b[?25l");
        assert!(!terminal.modes().cursor_visible);
        assert_eq!(terminal.modes().cursor_style, CursorStyle::Underline);
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

    #[test]
    fn hostile_output_keeps_terminal_invariants() {
        // Long OSC/DCS, invalid UTF-8, overlong CSI, rapid mode toggles,
        // wide-cell abuse, and control aborts must neither panic nor corrupt
        // grid state, chunked or contiguous.
        let mut hostile = Vec::new();
        hostile.extend_from_slice(b"\x1b]0;");
        hostile.extend(std::iter::repeat_n(b'A', 5000));
        hostile.extend_from_slice(b"\x07\x1bP");
        hostile.extend(std::iter::repeat_n(b'B', 3000));
        hostile.extend_from_slice(b"\x1b\\\xff\xfe\x80 invalid \xc3(");
        hostile.extend_from_slice(b"\x1b[");
        for _ in 0..40 {
            hostile.extend_from_slice(b"1;");
        }
        hostile.extend_from_slice(b"H\x1b[?1049h\x1b[?1000h\x1b[?1049l\x1b[?1000l");
        hostile.extend_from_slice(&"界界é́́".as_bytes().repeat(200));
        hostile.extend_from_slice(b"\x1b[5;20r\x1b[r\x18\x1a");
        let mut contiguous = terminal(80, 24);
        let mut chunked = terminal(80, 24);
        contiguous.process(&hostile);
        for chunk in hostile.chunks(7) {
            chunked.process(chunk);
        }
        assert_eq!(contiguous.active_grid(), chunked.active_grid());
        assert_eq!(contiguous.cursor(), chunked.cursor());
        let grid = contiguous.active_grid();
        let cursor = contiguous.cursor();
        assert!(cursor.column < grid.width() && cursor.row < grid.height());
        for row in 0..grid.height() {
            for column in 0..grid.width() {
                let cell = grid.cell(row, column).unwrap();
                if cell.flags().contains(CellFlags::WIDE_SPACER) {
                    assert!(
                        column > 0
                            && grid
                                .cell(row, column - 1)
                                .unwrap()
                                .flags()
                                .contains(CellFlags::WIDE_LEADING)
                    );
                }
                if cell.flags().contains(CellFlags::WIDE_LEADING) {
                    assert!(
                        column + 1 < grid.width()
                            && grid
                                .cell(row, column + 1)
                                .unwrap()
                                .flags()
                                .contains(CellFlags::WIDE_SPACER)
                    );
                }
            }
        }
    }
}
