mod keyboard;
mod protocol;
mod rendition;
pub use protocol::HostProperties;

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
    Motion,
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

/// A point in the retained buffer. `line` is an absolute line number that
/// keeps naming the same content while the viewport scrolls; it only loses
/// meaning when trimming drops that content. `column` indexes a cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionPoint {
    pub line: u64,
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
    pub mouse_pixel: bool,
    pub focus_reporting: bool,
    pub synchronized_output: bool,
    pub keyboard_flags: u8,
    pub modify_other_keys: u8,
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
            mouse_pixel: false,
            focus_reporting: false,
            synchronized_output: false,
            keyboard_flags: 0,
            modify_other_keys: 0,
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
    underline_color: u32,
    flags: u32,
}

impl Default for Rendition {
    fn default() -> Self {
        Self {
            fg: DEFAULT_COLOR,
            bg: DEFAULT_COLOR,
            underline_color: DEFAULT_COLOR,
            flags: 0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
struct SavedCursor {
    cursor: Cursor,
    rendition: Rendition,
    pending_wrap: bool,
}

/// Terminal semantics layered over the independent main and alternate grids.
#[cfg_attr(test, derive(Clone, Debug, PartialEq, Eq))]
pub struct Terminal {
    host_properties: Option<HostProperties>,
    keyboard_stacks: [Vec<u8>; 2],
    grid: TerminalGrid,
    parser: Parser,
    modes: Modes,
    mouse_press_enabled: bool,
    mouse_drag_enabled: bool,
    mouse_motion_enabled: bool,
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
            host_properties: None,
            keyboard_stacks: [vec![0], vec![0]],
            grid: TerminalGrid::new(width, height)?,
            parser: Parser::new(),
            modes: Modes::default(),
            mouse_press_enabled: false,
            mouse_drag_enabled: false,
            mouse_motion_enabled: false,
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

    /// Xterm focus reports are opt-in and contain no pointer coordinates.
    pub fn focus_report(&self, focused: bool) -> Option<&'static [u8]> {
        self.modes
            .focus_reporting
            .then_some(if focused { b"\x1b[I" } else { b"\x1b[O" })
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

    pub fn viewport_underline_color(&self, row: usize, column: usize) -> Option<u32> {
        self.grid
            .active()
            .viewport_row(row)?
            .underline_color_at(column)
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

    /// Absolute line number shown at a viewport row of the active screen.
    pub fn viewport_line(&self, row: usize) -> Option<u64> {
        self.active_grid().viewport_line(row)
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
        self.normalized_selection()
    }

    /// Selects the cell rectangle between two buffer points.
    pub fn set_selection(&mut self, anchor: SelectionPoint, focus: SelectionPoint) {
        let grid = self.grid.active();
        let width = grid.width();
        let first = grid.first_line();
        let last = grid.last_line();
        let clamp = |point: SelectionPoint| SelectionPoint {
            line: point.line.clamp(first, last),
            column: point.column.min(width - 1),
        };
        self.store_selection(clamp(anchor), clamp(focus));
    }

    /// Selects whole words: the start endpoint grows left and the end endpoint
    /// grows right to the bounds of the same-class run (alphanumeric and `_`
    /// versus everything else) under each one.
    pub fn set_selection_words(&mut self, anchor: SelectionPoint, focus: SelectionPoint) {
        let (anchor_start, _) = self.word_bounds(anchor);
        let (_, focus_end) = self.word_bounds(focus);
        self.set_selection(
            SelectionPoint {
                line: anchor.line,
                column: anchor_start,
            },
            SelectionPoint {
                line: focus.line,
                column: focus_end,
            },
        );
    }

    /// Selects whole lines: each endpoint grows to the full row width.
    pub fn set_selection_lines(&mut self, anchor: SelectionPoint, focus: SelectionPoint) {
        let last = self.active_grid().width() - 1;
        self.set_selection(
            SelectionPoint {
                line: anchor.line,
                column: 0,
            },
            SelectionPoint {
                line: focus.line,
                column: last,
            },
        );
    }

    /// Bounds of the contiguous same-class run at `point`. Uses absolute lines
    /// so double-clicking a word still names the same cells after scrolling.
    fn word_bounds(&self, point: SelectionPoint) -> (usize, usize) {
        let grid = self.grid.active();
        let width = grid.width();
        let line = point.line.clamp(grid.first_line(), grid.last_line());
        let column = point.column.min(width - 1);
        let word_class = |column: usize| {
            grid.row_at_line(line)
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
        while end + 1 < width && word_class(end + 1) == class {
            end += 1;
        }
        (start, end)
    }

    fn store_selection(&mut self, anchor: SelectionPoint, focus: SelectionPoint) {
        let old = self.selection.replace((anchor, focus));
        for (start, end) in [old, Some((anchor, focus))].into_iter().flatten() {
            self.mark_line_range_dirty(start.line.min(end.line), start.line.max(end.line));
        }
    }

    /// Marks the visible rows touching `start..=end`; lines outside the
    /// viewport have no pixels to repaint.
    fn mark_line_range_dirty(&mut self, start: u64, end: u64) {
        let (from, to) = {
            let grid = self.grid.active();
            let (Some(first), Some(last)) =
                (grid.viewport_line(0), grid.viewport_line(grid.height() - 1))
            else {
                return;
            };
            (start.max(first), end.min(last))
        };
        for line in from..=to {
            if let Some(row) = self.grid.active().line_viewport(line) {
                self.grid.active_mut().mark_row_dirty(row);
            }
        }
    }

    pub fn clear_selection(&mut self) {
        if let Some((start, end)) = self.selection.take() {
            self.mark_line_range_dirty(start.line.min(end.line), start.line.max(end.line));
        }
    }

    /// The selection clamped to the lines still retained. A selection whose
    /// lines were trimmed away entirely reports `None`.
    fn normalized_selection(&self) -> Option<(SelectionPoint, SelectionPoint)> {
        let (anchor, focus) = self.selection?;
        let (mut start, mut end) = if (anchor.line, anchor.column) <= (focus.line, focus.column) {
            (anchor, focus)
        } else {
            (focus, anchor)
        };
        let grid = self.grid.active();
        let first = grid.first_line();
        let last = grid.last_line();
        if end.line < first || start.line > last {
            return None;
        }
        if start.line < first {
            start = SelectionPoint {
                line: first,
                column: 0,
            };
        }
        if end.line > last {
            end = SelectionPoint {
                line: last,
                column: grid.width() - 1,
            };
        }
        Some((start, end))
    }

    pub fn selection_text(&self) -> Option<String> {
        let (start, end) = self.normalized_selection()?;
        let grid = self.grid.active();
        let mut output = String::new();
        for line in start.line..=end.line {
            let row = grid.row_at_line(line)?;
            let first = if line == start.line { start.column } else { 0 };
            let last = if line == end.line {
                end.column
            } else {
                row.cells().len() - 1
            };
            let mut text = String::new();
            for column in first..=last {
                if let Some(grapheme) = row.grapheme_at(column) {
                    text.push_str(&grapheme);
                } else {
                    text.push(' ');
                }
            }
            output.push_str(text.trim_end());
            if line < end.line
                && grid
                    .row_at_line(line + 1)
                    .is_some_and(|next| !next.is_wrapped())
            {
                output.push('\n');
            }
        }
        Some(output)
    }

    /// Linear range over the visible grid for the renderer's selection
    /// highlight: the visible intersection of the selection, or `None` when
    /// none of it is on screen.
    pub fn selection_cell_range(&self) -> Option<(u32, u32)> {
        let (start, end) = self.normalized_selection()?;
        let grid = self.grid.active();
        let width = grid.width() as u32;
        let first_visible = grid.viewport_line(0)?;
        let last_visible = grid.viewport_line(grid.height() - 1)?;
        if end.line < first_visible || start.line > last_visible {
            return None;
        }
        let top_line = start.line.max(first_visible);
        let bottom_line = end.line.min(last_visible);
        let top_row = grid.line_viewport(top_line)? as u32;
        let bottom_row = grid.line_viewport(bottom_line)? as u32;
        let start_column = if top_line == start.line {
            start.column as u32
        } else {
            0
        };
        let end_column = if bottom_line == end.line {
            end.column as u32
        } else {
            width - 1
        };
        Some((
            top_row * width + start_column,
            bottom_row * width + end_column,
        ))
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
        parser.feed_spans(bytes, |input| match input {
            crate::parser::Input::Ascii(text) => self.put_ascii(text, &mut replies),
            crate::parser::Input::Text(text) => self.put_text(text),
            crate::parser::Input::Event(event) => self.dispatch(event, &mut replies),
        });
        self.parser = parser;
        replies
    }

    pub fn resize(&mut self, width: usize, height: usize) -> Result<(), GridError> {
        // Same-size resizes (font zoom) must leave scroll margins, pending
        // wrap, tabs, and selection alone; only a geometry change invalidates
        // them. Grid::resize already no-ops on identical dimensions.
        if self.grid.active().width() == width && self.grid.active().height() == height {
            return Ok(());
        }
        let main_wrap = if self.is_alternate_screen() {
            self.main_pending_wrap
        } else {
            self.pending_wrap
        };
        let alternate = self.is_alternate_screen();
        self.grid.resize(width, height)?;
        // A delayed-wrap cursor names the final occupied cell. After reflow
        // it must advance to the insertion point unless still at the margin.
        let main = self.grid.screen_mut(ActiveScreen::Main);
        let cursor = main.cursor();
        if main_wrap && cursor.column + 1 < width {
            main.set_cursor(cursor.column + 1, cursor.row);
        }
        let main_wrap = main_wrap && cursor.column + 1 >= width;
        if let Some(saved) = &mut self.alternate_saved_cursor {
            saved.cursor = main.cursor();
            saved.pending_wrap = main_wrap;
        }
        self.main_pending_wrap = main_wrap;
        // Reflow rewrites logical lines, so a selection made against the old
        // geometry no longer names the same cells; drop it instead of keeping
        // a highlight whose text would silently change.
        self.clear_selection();
        let old_width = self.tabs.len();
        self.tabs.resize(width, false);
        for col in old_width..width {
            self.tabs[col] = col != 0 && col % 8 == 0;
        }
        self.top_margin = 0;
        self.bottom_margin = height - 1;
        self.pending_wrap = !alternate && main_wrap;
        // Resize is not a VT control and must not detach the next combining
        // scalar. Reflow moved the last printed grapheme along with the cursor.
        if self.last_printed.is_some() {
            let cursor = self.cursor();
            let position = if alternate {
                self.last_printed.map(|(_, row, column)| (row, column))
            } else if self.pending_wrap {
                Some((cursor.row, cursor.column))
            } else if cursor.column != 0 {
                Some((cursor.row, cursor.column - 1))
            } else if cursor.row != 0 {
                Some((cursor.row - 1, width - 1))
            } else {
                None
            };
            self.last_printed = position.and_then(|(row, column)| {
                let cell = self.active_grid().cell(row, column)?;
                let column = if cell.flags().contains(CellFlags::WIDE_SPACER) {
                    column.checked_sub(1)?
                } else if cell.content == 0 {
                    return None;
                } else {
                    column
                };
                Some((self.grid.active_screen(), row, column))
            });
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
            ParserEvent::DcsQuery(bytes) => self.protocol_dcs(&bytes, replies),
            ParserEvent::Osc(bytes) => {
                if !self.protocol_osc(&bytes, replies) {
                    self.osc(&bytes);
                }
            }
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
            // C1 single-byte forms of the ECMA-48 controls the parser
            // already emits as `Control`: IND (ESC D), NEL (ESC E) and HTS
            // (ESC H) behave exactly like their two-byte spellings.
            0x84 => self.index(),
            0x85 => {
                self.carriage_return();
                self.index();
            }
            0x8d => self.reverse_index(),
            0x88 => {
                let column = self.cursor().column;
                self.tabs[column] = true;
            }
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
        // Colon syntax belongs to SGR; it must not flatten into cursor or mode parameters.
        if csi.subparams.iter().any(|sub| *sub) && (csi.final_byte != b'm' || csi.private.is_some())
        {
            return;
        }
        if self.keyboard_csi(&csi, replies) || self.protocol_csi(&csi, replies) {
            return;
        }
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

    /// Batch independent scalars within a row; joins, wrapping and terminal
    /// modes retain scalar dispatch before attempting another span.
    fn put_text(&mut self, mut text: &str) {
        let graphics = if self.use_g1 {
            self.g1_dec_graphics
        } else {
            self.g0_dec_graphics
        };
        while let Some(ch) = text.chars().next() {
            if crate::grid::plain_text_width(ch).is_some()
                && !graphics
                && !self.insert_mode
                && !self.pending_wrap
                && !self.last_printed.is_some_and(|(_, row, column)| {
                    self.active_grid().stored_grapheme(row, column).is_some()
                })
            {
                let cursor = self.cursor();
                let cell = Cell::new(
                    0,
                    self.rendition.fg,
                    self.rendition.bg,
                    CellFlags::from_bits_retain(self.rendition.flags),
                );
                let link = self.current_hyperlink;
                let underline = (self.rendition.underline_color != DEFAULT_COLOR)
                    .then_some(self.rendition.underline_color);
                if let Some(written) = self.active_grid_mut().write_plain_text(
                    cursor.row,
                    cursor.column,
                    text,
                    cell,
                    link,
                    underline,
                ) {
                    self.last_printed =
                        Some((self.grid.active_screen(), cursor.row, written.last_column));
                    self.preceding_char = Some(written.last_char);
                    self.active_grid_mut()
                        .advance_printed_cursor(written.end_column);
                    self.pending_wrap =
                        self.modes.auto_wrap && written.end_column >= self.active_grid().width();
                    text = &text[written.bytes..];
                    continue;
                }
            }
            text = &text[ch.len_utf8()..];
            let ch = if graphics { dec_graphic(ch) } else { ch };
            self.put_char(ch);
            self.preceding_char = Some(ch);
        }
    }

    fn put_ascii(&mut self, mut text: &[u8], replies: &mut Vec<u8>) {
        if self.insert_mode
            || if self.use_g1 {
                self.g1_dec_graphics
            } else {
                self.g0_dec_graphics
            }
        {
            for &byte in text {
                self.dispatch(ParserEvent::Print(byte as char), replies);
            }
            return;
        }
        while let Some((&first, rest)) = text.split_first() {
            // Only delayed wraps and stored clusters need the scalar path.
            // Ordinary runs, including short runs between Unicode scalars,
            // can write their first column with the rest of the row span.
            if self.pending_wrap
                || self.last_printed.is_some_and(|(_, row, column)| {
                    self.active_grid().stored_grapheme(row, column).is_some()
                })
            {
                self.put_char(first as char);
                self.preceding_char = Some(first as char);
                text = rest;
                continue;
            }
            let cursor = self.cursor();
            let count = text.len().min(self.active_grid().width() - cursor.column);
            let cell = Cell::new(
                0,
                self.rendition.fg,
                self.rendition.bg,
                CellFlags::from_bits_retain(self.rendition.flags),
            );
            let link = self.current_hyperlink;
            let underline = (self.rendition.underline_color != DEFAULT_COLOR)
                .then_some(self.rendition.underline_color);
            self.active_grid_mut().write_ascii(
                cursor.row,
                cursor.column,
                &text[..count],
                cell,
                link,
                underline,
            );
            let end = cursor.column + count;
            self.last_printed = Some((self.grid.active_screen(), cursor.row, end - 1));
            self.preceding_char = Some(text[count - 1] as char);
            self.active_grid_mut().advance_printed_cursor(end);
            self.pending_wrap = self.modes.auto_wrap && end >= self.active_grid().width();
            text = &text[count..];
        }
    }

    fn put_char(&mut self, ch: char) {
        if ch.is_ascii() {
            let previous = self.last_printed.and_then(|(screen, row, column)| {
                (screen == self.grid.active_screen())
                    .then(|| self.active_grid().stored_grapheme(row, column))
                    .flatten()
            });
            if previous.is_some_and(|text| text.len() >= 256) {
                return;
            }
            // ASCII starts a new cluster after an ordinary scalar or one of
            // these extending marks; other stored clusters use the full rules.
            if previous.is_none_or(|text| {
                text.chars()
                    .next_back()
                    .is_some_and(|last| ('\u{300}'..='\u{36f}').contains(&last))
            }) {
                self.write_single(ch);
                return;
            }
        }
        // Combining Diacritical Marks extend an ASCII base without changing
        // its width. Keep the owned cluster in place instead of allocating,
        // segmenting and measuring a fresh copy for every appended accent.
        if ('\u{300}'..='\u{36f}').contains(&ch)
            && let Some((screen, row, column)) = self.last_printed
            && screen == self.grid.active_screen()
            && let Some(cell) = self.active_grid().cell(row, column)
            && (0x20..=0x7e).contains(&cell.content)
            && !cell.flags().contains(CellFlags::WIDE_LEADING)
        {
            self.active_grid_mut()
                .append_ascii_diacritic(row, column, ch);
            return;
        }
        self.put_char_scalar(ch);
    }

    fn put_char_scalar(&mut self, ch: char) {
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
                    let cell = self
                        .active_grid()
                        .cell(row, column)
                        .expect("last cell remains visible");
                    let width = UnicodeWidthStr::width(combined.as_str());
                    if (width == 2 && cell.flags().contains(CellFlags::WIDE_LEADING))
                        || (width <= 1 && !cell.flags().contains(CellFlags::WIDE_LEADING))
                    {
                        self.active_grid_mut()
                            .extend_grapheme(row, column, combined);
                        return;
                    }
                    if width == 2 && column + 1 < self.active_grid().width() {
                        let link = self.active_grid().hyperlink(row, column);
                        let underline = self
                            .active_grid()
                            .row_at(row)
                            .and_then(|row| row.underline_color_at(column));
                        self.active_grid_mut()
                            .set_wide_grapheme(row, column, &combined, cell);
                        self.active_grid_mut()
                            .set_underline_color(row, column, underline);
                        if let Some(link) = link {
                            self.active_grid_mut()
                                .set_hyperlink(row, column, Some(link));
                        }
                        self.set_cursor(column + 2, row);
                        self.pending_wrap =
                            self.modes.auto_wrap && column + 2 >= self.active_grid().width();
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
        let underline = self.rendition.underline_color;
        if underline != DEFAULT_COLOR {
            self.active_grid_mut()
                .set_underline_color(cursor.row, cursor.column, Some(underline));
        }
        self.last_printed = Some((self.grid.active_screen(), cursor.row, cursor.column));
        // Even with delayed wrap the cursor names the last occupied column,
        // not the leading half. DSR, backspace and relative moves use it.
        self.active_grid_mut()
            .advance_printed_cursor(cursor.column + 2);
        self.pending_wrap = self.modes.auto_wrap && cursor.column + 2 >= self.active_grid().width();
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
        let underline = self.rendition.underline_color;
        if underline != DEFAULT_COLOR {
            self.active_grid_mut()
                .set_underline_color(cursor.row, cursor.column, Some(underline));
        }
        self.last_printed = Some((self.grid.active_screen(), cursor.row, cursor.column));
        if cursor.column + 1 >= self.active_grid().width() {
            if self.modes.auto_wrap {
                self.pending_wrap = true;
            }
        } else {
            self.active_grid_mut()
                .advance_printed_cursor(cursor.column + 1);
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
                1003 => {
                    self.mouse_motion_enabled = enabled;
                    self.update_mouse_tracking();
                }
                1004 => self.modes.focus_reporting = enabled,
                1006 => self.modes.mouse_sgr = enabled,
                1016 => self.modes.mouse_pixel = enabled,
                1049 => self.switch_alternate(enabled, true),
                2004 => self.modes.bracketed_paste = enabled,
                2026 => self.modes.synchronized_output = enabled,
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
            self.clear_selection();
            self.grid.select_screen(ActiveScreen::Main);
            self.pending_wrap = self.main_pending_wrap;
            if save_main && self.alternate_1049_active {
                if let Some(saved) = self.alternate_saved_cursor.take() {
                    self.restore_cursor(saved);
                }
                self.alternate_1049_active = false;
            }
        }
        let screen = usize::from(self.is_alternate_screen());
        self.modes.keyboard_flags = *self.keyboard_stacks[screen]
            .last()
            .expect("keyboard stack base");
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
        self.jump_to_bottom();
        self.grid.screen_mut(ActiveScreen::Main).clear_all();
        self.grid.screen_mut(ActiveScreen::Alternate).clear_all();
        self.grid.screen_mut(ActiveScreen::Main).set_cursor(0, 0);
        self.grid
            .screen_mut(ActiveScreen::Alternate)
            .set_cursor(0, 0);
        self.modes = Modes::default();
        self.keyboard_stacks = [vec![0], vec![0]];
        self.mouse_press_enabled = false;
        self.mouse_drag_enabled = false;
        self.mouse_motion_enabled = false;
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
        self.modes.mouse_tracking = if self.mouse_motion_enabled {
            MouseTracking::Motion
        } else if self.mouse_drag_enabled {
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
    fn dispatch_scalar(terminal: &mut Terminal, event: ParserEvent, replies: &mut Vec<u8>) {
        if let ParserEvent::Print(ch) = event {
            let graphics = if terminal.use_g1 {
                terminal.g1_dec_graphics
            } else {
                terminal.g0_dec_graphics
            };
            let ch = if graphics { dec_graphic(ch) } else { ch };
            terminal.put_char_scalar(ch);
            terminal.preceding_char = Some(ch);
        } else {
            terminal.dispatch(event, replies);
        }
    }

    #[test]
    fn plain_text_han_ranges_have_independent_double_width_scalars() {
        for range in [0x3400..=0x4dbf, 0x4e00..=0x9fff, 0xf900..=0xfaff] {
            for value in range {
                let ch = char::from_u32(value).unwrap();
                assert_eq!(crate::grid::plain_text_width(ch), Some(2));
                assert_eq!(UnicodeWidthChar::width(ch), Some(2), "U+{value:04X}");
                assert_eq!(
                    format!("a{ch}a").graphemes(true).count(),
                    3,
                    "U+{value:04X}"
                );
            }
        }
    }

    #[test]
    fn utf8_row_runs_match_scalar_writes_through_chunking_and_wide_edges() {
        fn scalar(t: &mut Terminal, bytes: &[u8]) -> Vec<u8> {
            let mut replies = Vec::new();
            let mut parser = std::mem::take(&mut t.parser);
            parser.feed(bytes, |event| dispatch_scalar(t, event, &mut replies));
            t.parser = parser;
            replies
        }
        let capped = format!("界e{}abcdefgh漢字", "\u{301}".repeat(127));
        let scenarios = [
            "\x1b[38;2;12;34;56;48;5;4;4:3;58:5:2m\x1b]8;;https://example.org\x1b\\界abcdefghijklmnop漢字e\u{301}ab界\r\n漢字abcdefghijklmnopqrstuv\x1b[3b\x1b[6n",
            "界\u{fe0f}ab漢\u{200d}a字🇵🇪👩‍💻क्षمَرْحَبًاabcdefghijklmnop漢字",
            "\u{3400}\u{4dbf}\u{4e00}\u{9fff}\u{f900}\u{faff}abcdefgh漢字",
            "\x1b[?7l界abcdef漢字abcdefghijklmnop\x1b[?7h界abcdefghijklmnop漢字",
            "界abc漢字\r\x1b[4h漢abcdefghijklmnop\x1b[4l字abcdefghijklmnop\x1b(0界lqk漢abcdefghijklmnop\x1b(B字abcdefghijklmnop",
            "\x1b[?1049h界abcdefghijklmnop漢字\x1b[?1049l界abcdefghijklmnop漢字\r\n界abcdefghijklmnop漢字",
            capped.as_str(),
        ];
        for (width, height) in [(1, 1), (2, 2), (7, 3), (17, 4)] {
            for chunk_size in [1, 2, 3, 7, 64, 4096] {
                for input in scenarios {
                    let mut actual = terminal(width, height);
                    actual.set_scrollback_limit(8);
                    let mut expected = actual.clone();
                    for _ in 0..height + 10 {
                        assert_eq!(
                            actual.process(b"prefill\r\n"),
                            scalar(&mut expected, b"prefill\r\n")
                        );
                    }
                    actual.scroll_view_up(3);
                    expected.scroll_view_up(3);
                    for chunk in input.as_bytes().chunks(chunk_size) {
                        assert_eq!(actual.process(chunk), scalar(&mut expected, chunk));
                        assert_eq!(
                            actual, expected,
                            "{width}x{height}, chunk={chunk_size}, input={input:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn ascii_diacritics_match_segmentation_and_width_rules() {
        for base in b' '..=b'~' {
            for mark in 0x300..=0x36f {
                let mark = char::from_u32(mark).unwrap();
                let input = format!("{}{}", base as char, mark);
                let mut actual = terminal(4, 2);
                actual.process(input.as_bytes());
                let mut expected = terminal(4, 2);
                Parser::new().feed(input.as_bytes(), |event| {
                    dispatch_scalar(&mut expected, event, &mut Vec::new())
                });
                assert_eq!(actual, expected, "base={base}, mark={mark:?}");
            }
        }
    }

    #[test]
    fn ascii_spans_match_scalar_dispatch_across_modes_and_metadata() {
        use proptest::prelude::*;
        use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};

        let operations: &[&[u8]] = &[
            b"abcdefghijklmnopqrstuvwxyz0123456789",
            b" ",
            b"xyz",
            b"\r\n",
            b"\x08",
            b"\t",
            b"\x1b[H",
            b"\x1b[2;3H",
            b"\x1b[?7l",
            b"\x1b[?7h",
            b"\x1b[4h",
            b"\x1b[4l",
            b"\x1b(0lqkx\x1b(B",
            b"\x1b)0\x0elqkx\x0f",
            b"\x1b[2;3r",
            b"\x1b[r",
            b"\x1b[?1049h",
            b"\x1b[?1049l",
            b"\x1b[3b",
            b"\x1b[38;2;12;34;56;48;5;4;4:3;58:5:2m",
            b"\x1b[0m",
            b"\x1b]8;;https://example.org\x1b\\",
            b"\x1b]8;;\x1b\\",
            "界e\u{301}❤\u{fe0f}🇵🇪👩‍💻".as_bytes(),
            b"\x1b[2J",
            b"\x1b[3J",
            b"\x1b[6n",
            b"\xf0\x9f",
            b"\xa7\x91",
            b"\xe2\x1b[31m",
            b"\x1b[@",
            b"\x1b[P",
            b"\x1b[S",
            b"\x1b[T",
            b"\x1bc",
        ];
        let strategy = (
            1usize..20,
            1usize..6,
            0usize..33,
            prop::collection::vec((0usize..operations.len(), 1usize..40), 1..160),
        );
        let mut runner = TestRunner::new_with_rng(
            Config {
                cases: 512,
                failure_persistence: None,
                ..Config::default()
            },
            TestRng::from_seed(RngAlgorithm::ChaCha, &[0xa7; 32]),
        );
        runner
            .run(&strategy, |(width, height, history, steps)| {
                let mut spans = terminal(width, height);
                spans.set_scrollback_limit(history);
                let mut scalar = spans.clone();
                for (index, chunk_size) in steps {
                    for chunk in operations[index].chunks(chunk_size) {
                        let actual = spans.process(chunk);
                        let mut expected = Vec::new();
                        let mut parser = std::mem::take(&mut scalar.parser);
                        parser.feed(chunk, |event| {
                            dispatch_scalar(&mut scalar, event, &mut expected)
                        });
                        scalar.parser = parser;
                        prop_assert_eq!(actual, expected);
                        prop_assert_eq!(&spans, &scalar);
                    }
                }
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn ascii_after_a_full_grapheme_preserves_scalar_limit_behavior() {
        for cluster in [
            format!("e{}", "\u{301}".repeat(127)),
            format!("e{}\u{20dd}", "\u{301}".repeat(126)),
        ] {
            assert!((255..=256).contains(&cluster.len()));
            let mut spans = terminal(8, 3);
            let mut scalar = spans.clone();
            let bytes = format!("{cluster}abcdefgh");
            spans.process(bytes.as_bytes());
            let mut parser = std::mem::take(&mut scalar.parser);
            parser.feed(bytes.as_bytes(), |event| {
                dispatch_scalar(&mut scalar, event, &mut Vec::new())
            });
            scalar.parser = parser;
            assert_eq!(spans, scalar);
        }
    }

    #[test]
    fn long_history_resize_preserves_unicode_links_and_viewport() {
        let mut t = terminal(80, 25);
        t.set_scrollback_limit(10_000);
        feed(&mut t, b"\x1b]8;;https://example.org\x1b\\");
        for line in 0..10_025 {
            feed(&mut t, format!("├── {line:05}/ e\u{301}界\r\n").as_bytes());
        }
        feed(&mut t, b"\x1b]8;;\x1b\\");
        t.scroll_view_up(5_000);
        let before = t.grid.clone();
        let cursor = t.cursor();
        // These lines fit both widths, so no extra row or history eviction is
        // needed. A round trip must preserve sidecars and the viewport anchor.
        t.resize(79, 25).unwrap();
        t.resize(80, 25).unwrap();
        for screen in [ActiveScreen::Main, ActiveScreen::Alternate] {
            let actual = t.grid.screen(screen);
            let expected = before.screen(screen);
            assert_eq!(actual.first_line(), expected.first_line());
            assert_eq!(actual.last_line(), expected.last_line());
            assert_eq!(actual.viewport_offset(), expected.viewport_offset());
            for line in expected.first_line()..=expected.last_line() {
                assert!(
                    actual.row_at_line(line) == expected.row_at_line(line),
                    "history resize changed row or sidecars at {line}"
                );
            }
        }
        assert_eq!(t.cursor(), cursor);
    }

    #[test]
    fn insert_mode_keeps_existing_wide_hyperlink_ownership() {
        let mut t = terminal(3, 1);
        feed(
            &mut t,
            "\x1b]8;;https://example.org\x1b\\\x1b[4h界A".as_bytes(),
        );
        assert_eq!(
            t.active_grid().hyperlink(0, 0),
            t.active_grid().hyperlink(0, 1)
        );
        assert!(t.active_grid().hyperlink(0, 1).is_some());
    }

    #[test]
    fn combining_mark_after_resize_follows_its_reflowed_base() {
        let mut t = terminal(12, 6);
        feed(&mut t, b"aa");
        t.resize(1, 6).unwrap();
        feed(&mut t, "\u{301}".as_bytes());
        assert_eq!(t.active_grid().grapheme(1, 0).as_deref(), Some("a\u{301}"));
    }

    #[test]
    fn a_scrolled_view_keeps_its_logical_anchor_when_width_changes() {
        let mut t = terminal(4, 2);
        feed(&mut t, b"abcdefghijklmnop\r\n");
        t.scrollback_top();
        assert_eq!(t.viewport_cell(0, 0).unwrap().content, 'a' as u32);
        t.resize(2, 2).unwrap();
        assert_eq!(t.viewport_cell(0, 0).unwrap().content, 'a' as u32);
        t.resize(8, 2).unwrap();
        assert_eq!(t.viewport_cell(0, 0).unwrap().content, 'a' as u32);
    }

    #[test]
    fn shrinking_unused_rows_keeps_the_prompt_and_cursor_in_place() {
        let mut t = terminal(8, 10);
        feed(&mut t, b"output\r\nprompt");
        t.resize(8, 5).unwrap();
        assert_eq!(line(&t, 0), "output..");
        assert_eq!(line(&t, 1), "prompt..");
        assert_eq!(t.cursor(), Cursor { column: 6, row: 1 });
        assert_eq!(t.scrollback_len(), 0);
    }

    #[test]
    fn widening_a_full_line_preserves_the_insertion_point() {
        for text in ["ABCD", "AB界"] {
            let mut t = terminal(4, 3);
            feed(&mut t, text.as_bytes());
            t.resize(8, 3).unwrap();
            assert_eq!(t.cursor(), Cursor { column: 4, row: 0 });
            feed(&mut t, b"X");
            assert_eq!(t.active_grid().cell(0, 4).unwrap().content, 'X' as u32);
            assert_eq!(
                t.active_grid().cell(0, 3).unwrap().content,
                if text == "ABCD" { 'D' as u32 } else { 0 }
            );
        }
    }

    #[test]
    fn reflow_does_not_insert_a_space_before_a_wrapped_wide_character() {
        let mut t = terminal(3, 3);
        feed(&mut t, "AB界X".as_bytes());
        t.resize(8, 3).unwrap();
        assert_eq!(line(&t, 0), "AB界.X...");
        assert_eq!(t.cursor(), Cursor { column: 5, row: 0 });
    }

    #[test]
    fn alternate_resize_preserves_coordinates_and_the_reflowed_main_cursor() {
        let mut t = terminal(6, 4);
        feed(&mut t, b"abcdefghi\x1b[?1049h123456\r\nXYZ");
        t.resize(4, 4).unwrap();
        assert_eq!(line(&t, 0), "1234");
        assert_eq!(line(&t, 1), "XYZ.");
        assert_eq!(t.scrollback_len(), 0);
        feed(&mut t, b"\x1b[?1049lX");
        assert_eq!(line(&t, 0), "abcd");
        assert_eq!(line(&t, 1), "efgh");
        assert_eq!(line(&t, 2), "iX..");
    }

    #[test]
    fn clear_removes_wide_cells_links_and_graphemes_and_homes_the_cursor() {
        let mut t = terminal(8, 3);
        feed(
            &mut t,
            "\x1b]8;;https://example.org\x1b\\界e\u{301}\x1b]8;;\x1b\\\r\nold\r\nold\r\n"
                .as_bytes(),
        );
        feed(&mut t, b"\x1b[H\x1b[2J\x1b[3J");
        assert_eq!(t.cursor(), Cursor { column: 0, row: 0 });
        assert_eq!(t.scrollback_len(), 0);
        for row in 0..3 {
            for column in 0..8 {
                let cell = t.viewport_cell(row, column).unwrap();
                assert_eq!(cell.content, 0);
                assert!(!cell.flags().contains(CellFlags::WIDE_LEADING));
                assert!(!cell.flags().contains(CellFlags::WIDE_SPACER));
                assert!(t.viewport_stored_grapheme(row, column).is_none());
                assert!(t.hyperlink_at(row, column).is_none());
            }
        }
        feed(&mut t, b"X");
        assert_eq!(line(&t, 0), "X.......");
    }

    #[test]
    fn c1_reverse_index_matches_escape_at_the_scroll_margin() {
        for sequence in [b"\x8d".as_slice(), b"\x1bM"] {
            let mut t = terminal(4, 3);
            feed(&mut t, b"AAAA\r\nBBBB\r\nCCCC\x1b[1;3H");
            feed(&mut t, sequence);
            assert_eq!(line(&t, 0), "....");
            assert_eq!(line(&t, 1), "AAAA");
            assert_eq!(t.cursor(), Cursor { column: 2, row: 0 });
        }
    }

    #[test]
    fn c1_interrupts_unfinished_escape_sequences_without_losing_text() {
        for prefix in [b"\x1b[12;".as_slice(), b"\x1b(", b"\x1b"] {
            let mut t = terminal(4, 3);
            feed(&mut t, b"A");
            feed(&mut t, prefix);
            feed(&mut t, b"\x85B");
            assert_eq!(line(&t, 0), "A...");
            assert_eq!(line(&t, 1), "B...");
            assert_eq!(t.cursor(), Cursor { column: 1, row: 1 });
        }
    }

    #[test]
    fn wide_glyph_at_right_margin_reports_and_moves_from_the_last_column() {
        for prefix in ["AB界", "AB❤\u{fe0f}"] {
            let mut t = terminal(4, 2);
            feed(&mut t, prefix.as_bytes());
            assert_eq!(t.cursor(), Cursor { column: 3, row: 0 });
            assert_eq!(feed(&mut t, b"\x1b[6n"), b"\x1b[1;4R");
            feed(&mut t, b"\x08X");
            assert_eq!(line(&t, 0), "ABX.");
            assert_eq!(line(&t, 1), "....");
        }
    }

    #[test]
    fn leaving_alternate_screen_discards_its_selection() {
        let mut t = terminal(5, 2);
        feed(&mut t, b"main\x1b[?1049halt");
        let point = SelectionPoint {
            line: t.viewport_line(0).unwrap(),
            column: 0,
        };
        t.set_selection(point, point);
        assert_eq!(t.selection_text().as_deref(), Some("a"));
        feed(&mut t, b"\x1b[?1049l");
        assert!(t.selection().is_none());
        assert_eq!(line(&t, 0), "main.");
        assert_eq!(t.cursor(), Cursor { column: 4, row: 0 });
    }

    #[test]
    fn reset_shows_the_cleared_live_screen_instead_of_old_scrollback() {
        let mut t = terminal(5, 2);
        feed(&mut t, b"old\r\nold\r\nold\r\n");
        t.scrollback_top();
        assert!(t.viewport_offset() > 0);
        feed(&mut t, b"\x1bc");
        assert_eq!(t.viewport_offset(), 0);
        assert_eq!(t.cursor(), Cursor { column: 0, row: 0 });
        for row in 0..2 {
            assert!((0..5).all(|column| t.viewport_cell(row, column).unwrap().content == 0));
        }
        assert_eq!(t.modes(), Modes::default());
    }

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
            SelectionPoint { line: 0, column: 0 },
            SelectionPoint { line: 1, column: 3 },
        );
        assert_eq!(terminal.selection_text().as_deref(), Some("café\nnext"));
        assert_eq!(terminal.active_grid().cell(0, 0), before);
        assert_eq!(terminal.selection_cell_range(), Some((0, 11)));
        terminal.clear_selection();
        assert!(terminal.selection().is_none());
    }

    #[test]
    fn selection_keeps_naming_the_same_lines_while_the_viewport_scrolls() {
        let mut terminal = terminal(6, 2);
        feed(&mut terminal, b"aa\r\nbb\r\ncc\r\ndd");
        // Live view shows cc/dd; aa/bb are in scrollback.
        assert_eq!(terminal.viewport_line(0), Some(2));
        terminal.set_selection(
            SelectionPoint { line: 2, column: 0 },
            SelectionPoint { line: 2, column: 1 },
        );
        assert_eq!(terminal.selection_text().as_deref(), Some("cc"));
        assert_eq!(terminal.selection_cell_range(), Some((0, 1)));
        terminal.scroll_view_up(1);
        assert_eq!(terminal.viewport_line(0), Some(1));
        assert_eq!(terminal.selection_text().as_deref(), Some("cc"));
        assert_eq!(terminal.selection_cell_range(), Some((6, 7)));
        terminal.scroll_view_up(1);
        assert_eq!(terminal.selection_text().as_deref(), Some("cc"));
        assert_eq!(terminal.selection_cell_range(), None);
        terminal.scroll_view_down(2);
        assert_eq!(terminal.selection_cell_range(), Some((0, 1)));
    }

    #[test]
    fn trimming_scrollback_clamps_or_drops_an_anchored_selection() {
        let mut terminal = terminal(4, 2);
        terminal.set_scrollback_limit(1);
        feed(&mut terminal, b"aa\r\nbb\r\ncc");
        // Scrollback [aa], screen [bb, cc].
        terminal.set_selection(
            SelectionPoint { line: 0, column: 0 },
            SelectionPoint { line: 0, column: 1 },
        );
        assert_eq!(terminal.selection_text().as_deref(), Some("aa"));
        // One scroll drops aa; a selection of it cannot survive.
        feed(&mut terminal, b"\r\ndd");
        assert_eq!(terminal.active_grid().first_line(), 1);
        assert!(terminal.selection().is_none());
        assert_eq!(terminal.selection_text(), None);
        // A selection on a retained line survives even when it leaves the view.
        terminal.set_selection(
            SelectionPoint { line: 2, column: 0 },
            SelectionPoint { line: 2, column: 1 },
        );
        feed(&mut terminal, b"\r\nee");
        assert_eq!(terminal.active_grid().first_line(), 2);
        assert_eq!(terminal.selection_text().as_deref(), Some("cc"));
        assert_eq!(terminal.selection_cell_range(), None);
        // Trimming the selected line itself drops the selection.
        feed(&mut terminal, b"\r\nff");
        assert_eq!(terminal.active_grid().first_line(), 3);
        assert!(terminal.selection().is_none());
    }

    #[test]
    fn word_and_line_selection_extend_endpoints_over_the_buffer() {
        let mut terminal = terminal(10, 2);
        feed(&mut terminal, b"foo bar\r\nbaz");
        terminal.set_selection_words(
            SelectionPoint { line: 0, column: 1 },
            SelectionPoint { line: 0, column: 5 },
        );
        assert_eq!(terminal.selection_text().as_deref(), Some("foo bar"));
        terminal.set_selection_words(
            SelectionPoint { line: 1, column: 1 },
            SelectionPoint { line: 1, column: 1 },
        );
        assert_eq!(terminal.selection_text().as_deref(), Some("baz"));
        terminal.set_selection_lines(
            SelectionPoint { line: 0, column: 2 },
            SelectionPoint { line: 1, column: 1 },
        );
        assert_eq!(terminal.selection_text().as_deref(), Some("foo bar\nbaz"));
    }

    #[test]
    fn selection_cell_range_returns_only_the_visible_intersection() {
        let mut terminal = terminal(4, 2);
        feed(&mut terminal, b"aa\r\nbb\r\ncc\r\ndd");
        terminal.set_selection(
            SelectionPoint { line: 0, column: 0 },
            SelectionPoint { line: 3, column: 1 },
        );
        assert_eq!(terminal.selection_text().as_deref(), Some("aa\nbb\ncc\ndd"));
        // Live view shows lines 2..=3: the highlight starts at its own row 0.
        assert_eq!(terminal.selection_cell_range(), Some((0, 5)));
    }

    #[test]
    fn resize_drops_the_selection_instead_of_keeping_stale_cells() {
        let mut terminal = terminal(8, 2);
        feed(&mut terminal, b"hello");
        terminal.set_selection(
            SelectionPoint { line: 0, column: 0 },
            SelectionPoint { line: 0, column: 4 },
        );
        assert_eq!(terminal.selection_text().as_deref(), Some("hello"));
        terminal.resize(6, 2).unwrap();
        assert!(terminal.selection().is_none());
    }

    #[test]
    fn font_size_resize_preserves_state_without_clearing() {
        // Simulates the font-zoom path: same columns×rows, pixel geometry
        // changes underneath. Content, modes, cursor, alternate screen,
        // scroll region, pending wrap, and scrollback must all survive.
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
        // A scroll region plus a pending wrap: filling the last row to the
        // margin arms the wrap without moving the cursor.
        feed(&mut terminal, b"\x1b[1;2r\x1b[3;1H0123456789");
        assert_eq!(terminal.scroll_region(), (0, 1));
        assert!(terminal.pending_wrap);
        let cursor_before = terminal.cursor();
        terminal.resize(10, 3).unwrap();
        assert_eq!(terminal.cursor(), cursor_before);
        assert_eq!(terminal.modes(), modes_before);
        assert_eq!(terminal.scrollback_len(), scrollback_before);
        assert_eq!(terminal.scroll_region(), (0, 1));
        assert!(terminal.pending_wrap);
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
    fn c1_single_byte_controls_match_their_escape_spellings() {
        // The parser emits 0x84/0x85/0x88 as `Control`; they must behave
        // exactly like ESC D (IND), ESC E (NEL) and ESC H (HTS).
        let mut index = terminal(4, 3);
        feed(&mut index, b"\x84");
        assert_eq!(index.cursor(), Cursor { column: 0, row: 1 });
        feed(&mut index, b"\x84\x84\x84");
        assert_eq!(index.cursor(), Cursor { column: 0, row: 2 });

        let mut next_line = terminal(4, 3);
        feed(&mut next_line, b"AB\x85CD");
        assert_eq!(line(&next_line, 0), "AB..");
        assert_eq!(line(&next_line, 1), "CD..");
        assert_eq!(next_line.cursor(), Cursor { column: 2, row: 1 });

        let mut tabs = terminal(8, 1);
        feed(&mut tabs, b"\x1b[3g");
        feed(&mut tabs, b"\x1b[1;4H\x88\r\tX");
        assert_eq!(tabs.active_grid().cell(0, 3).unwrap().content, 'X' as u32);
        assert_eq!(tabs.cursor().column, 4);
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
    fn motion_and_focus_modes_are_independent_and_resettable() {
        let mut terminal = terminal(10, 3);
        terminal.process(b"\x1b[?1000;1002;1003;1004;1016h");
        assert_eq!(terminal.modes().mouse_tracking, MouseTracking::Motion);
        assert_eq!(terminal.focus_report(true), Some(&b"\x1b[I"[..]));
        assert_eq!(terminal.focus_report(false), Some(&b"\x1b[O"[..]));
        assert!(terminal.modes().mouse_pixel);
        terminal.process(b"\x1b[?1003l");
        assert_eq!(terminal.modes().mouse_tracking, MouseTracking::Drag);
        terminal.process(b"\x1b[?1002l");
        assert_eq!(terminal.modes().mouse_tracking, MouseTracking::Press);
        terminal.process(b"\x1bc");
        assert_eq!(terminal.modes().mouse_tracking, MouseTracking::Off);
        assert!(!terminal.modes().mouse_pixel);
        assert!(terminal.focus_report(true).is_none());
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

#[cfg(test)]
#[path = "terminal/campaign.rs"]
mod campaign;
