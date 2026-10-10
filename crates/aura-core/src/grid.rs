use std::collections::VecDeque;
use std::ops::Range;

/// Rows kept above the visible screen when nothing else is requested.
pub const DEFAULT_SCROLLBACK_ROWS: usize = 10_000;
type ReflowCell = (Cell, Option<String>, Option<u32>);

/// Bit flags describing terminal-cell presentation and wide-cell structure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(transparent)]
pub struct CellFlags(u32);

impl CellFlags {
    pub const WIDE_LEADING: Self = Self(1 << 0);
    pub const WIDE_SPACER: Self = Self(1 << 1);
    pub const BOLD: Self = Self(1 << 2);
    pub const ITALIC: Self = Self(1 << 3);
    pub const UNDERLINE: Self = Self(1 << 4);
    pub const STRIKETHROUGH: Self = Self(1 << 5);
    pub const INVERSE: Self = Self(1 << 6);

    const WIDE_MASK: u32 = Self::WIDE_LEADING.0 | Self::WIDE_SPACER.0;

    /// Creates flags from raw bits. Unknown bits are retained for forward compatibility.
    pub const fn from_bits_retain(bits: u32) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    fn without_wide(self) -> Self {
        Self(self.0 & !Self::WIDE_MASK)
    }
}

/// Semantic content and rendition attributes for one terminal column.
///
/// `content` stores the leading Unicode scalar; a row sidecar retains additional grapheme scalars.
/// Zero represents an empty cell. Color words remain opaque to avoid renderer coupling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct Cell {
    pub content: u32,
    pub fg: u32,
    pub bg: u32,
    pub flags: u32,
}

impl Cell {
    pub const EMPTY: Self = Self {
        content: 0,
        fg: 0,
        bg: 0,
        flags: 0,
    };

    pub const fn new(content: u32, fg: u32, bg: u32, flags: CellFlags) -> Self {
        Self {
            content,
            fg,
            bg,
            flags: flags.bits(),
        }
    }

    pub const fn flags(self) -> CellFlags {
        CellFlags::from_bits_retain(self.flags)
    }

    fn is_wide_leading(self) -> bool {
        self.flags().contains(CellFlags::WIDE_LEADING)
    }

    fn is_wide_spacer(self) -> bool {
        self.flags().contains(CellFlags::WIDE_SPACER)
    }
}

/// Geometric cursor position. Grid methods keep stored positions in bounds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    pub column: usize,
    pub row: usize,
}

/// One physical row and the metadata that links it to a logical line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    cells: Box<[Cell]>,
    graphemes: Vec<Option<String>>,
    hyperlinks: Vec<Option<u32>>,
    wrapped: bool,
    dirty: bool,
}

impl Row {
    fn new(width: usize) -> Self {
        Self {
            cells: vec![Cell::EMPTY; width].into_boxed_slice(),
            graphemes: vec![None; width],
            hyperlinks: vec![None; width],
            wrapped: false,
            dirty: true,
        }
    }

    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    pub fn grapheme_at(&self, column: usize) -> Option<String> {
        let cell = *self.cells.get(column)?;
        if cell.is_wide_spacer() {
            return Some(String::new());
        }
        if cell.content == 0 {
            return None;
        }
        self.graphemes[column]
            .clone()
            .or_else(|| char::from_u32(cell.content).map(|ch| ch.to_string()))
    }

    pub fn stored_grapheme_at(&self, column: usize) -> Option<&str> {
        self.graphemes.get(column)?.as_deref()
    }

    /// Whether this row continues the logical line from its preceding row.
    pub const fn is_wrapped(&self) -> bool {
        self.wrapped
    }

    pub const fn is_dirty(&self) -> bool {
        self.dirty
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridError {
    ZeroWidth,
    ZeroHeight,
    DimensionsTooLarge,
}

/// A fixed-size screen backed by circular physical rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grid {
    width: usize,
    height: usize,
    rows: Vec<Row>,
    base_row: usize,
    cursor: Cursor,
    scrollback: VecDeque<Row>,
    scrollback_limit: usize,
    viewport_offset: usize,
}

impl Grid {
    /// Creates a blank grid. Zero dimensions are rejected to keep all coordinates valid.
    pub fn new(width: usize, height: usize) -> Result<Self, GridError> {
        if width == 0 {
            return Err(GridError::ZeroWidth);
        }
        if height == 0 {
            return Err(GridError::ZeroHeight);
        }
        validate_dimensions(width, height)?;
        let mut grid = Self {
            width,
            height,
            rows: (0..height).map(|_| Row::new(width)).collect(),
            base_row: 0,
            cursor: Cursor::default(),
            scrollback: VecDeque::new(),
            scrollback_limit: DEFAULT_SCROLLBACK_ROWS,
            viewport_offset: 0,
        };
        grid.mark_all_dirty();
        Ok(grid)
    }

    pub const fn width(&self) -> usize {
        self.width
    }

    pub const fn height(&self) -> usize {
        self.height
    }

    pub const fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// Sets the cursor, clamping each coordinate to the visible grid.
    pub fn set_cursor(&mut self, column: usize, row: usize) {
        let old_row = self.cursor.row;
        self.cursor = Cursor {
            column: column.min(self.width - 1),
            row: row.min(self.height - 1),
        };
        self.row_mut(old_row).expect("stored cursor row").dirty = true;
        self.row_mut(self.cursor.row)
            .expect("clamped cursor row")
            .dirty = true;
    }

    pub fn move_cursor(&mut self, columns: isize, rows: isize) {
        let old_row = self.cursor.row;
        self.cursor.column = self
            .cursor
            .column
            .saturating_add_signed(columns)
            .min(self.width - 1);
        self.cursor.row = self
            .cursor
            .row
            .saturating_add_signed(rows)
            .min(self.height - 1);
        self.row_mut(old_row).expect("stored cursor row").dirty = true;
        self.row_mut(self.cursor.row)
            .expect("clamped cursor row")
            .dirty = true;
    }

    /// Maps a visible row to its physical row in the circular allocation.
    fn physical_row(&self, logical_row: usize) -> Option<usize> {
        (logical_row < self.height).then(|| (self.base_row + logical_row) % self.height)
    }

    fn row(&self, logical_row: usize) -> Option<&Row> {
        self.physical_row(logical_row)
            .map(|index| &self.rows[index])
    }

    fn row_mut(&mut self, logical_row: usize) -> Option<&mut Row> {
        let index = self.physical_row(logical_row)?;
        Some(&mut self.rows[index])
    }

    pub fn row_at(&self, row: usize) -> Option<&Row> {
        self.row(row)
    }

    /// Returns the row currently shown at a viewport coordinate.
    pub fn viewport_row(&self, row: usize) -> Option<&Row> {
        if row >= self.height {
            return None;
        }
        let total = self.scrollback.len() + self.height;
        let end = total.saturating_sub(self.viewport_offset);
        let start = end.saturating_sub(self.height);
        let index = start + row;
        if index < self.scrollback.len() {
            self.scrollback.get(index)
        } else {
            self.row(index - self.scrollback.len())
        }
    }

    pub const fn viewport_offset(&self) -> usize {
        self.viewport_offset
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    /// Rows retained above the visible screen.
    pub const fn scrollback_limit(&self) -> usize {
        self.scrollback_limit
    }

    /// Changes how much history is retained, trimming immediately if the new
    /// limit is smaller than what is already stored. A zero limit disables
    /// scrollback entirely, which is what memory-constrained setups want.
    pub fn set_scrollback_limit(&mut self, limit: usize) {
        self.scrollback_limit = limit;
        while self.scrollback.len() > limit {
            self.scrollback.pop_front();
            self.viewport_offset = self.viewport_offset.saturating_sub(1);
        }
        self.set_viewport_offset(self.viewport_offset);
    }

    /// Moves the viewport into history without changing the live cursor or screen.
    pub fn set_viewport_offset(&mut self, offset: usize) {
        let clamped = offset.min(self.scrollback.len());
        if clamped != self.viewport_offset {
            self.viewport_offset = clamped;
            self.mark_all_dirty();
        }
    }

    /// Records rows displaced by full-screen output scroll before recycling them.
    pub fn scroll_up_recording(&mut self, count: usize) -> bool {
        let amount = count.min(self.height);
        if amount == 0 {
            return true;
        }
        for row in 0..amount {
            self.scrollback
                .push_back(self.row(row).expect("visible row").clone());
            if self.scrollback.len() > self.scrollback_limit {
                self.scrollback.pop_front();
                self.viewport_offset = self.viewport_offset.saturating_sub(1);
            }
        }
        if self.viewport_offset != 0 {
            self.viewport_offset = (self.viewport_offset + amount).min(self.scrollback.len());
        }
        self.scroll_up(0, self.height - 1, amount)
    }

    pub fn cell(&self, row: usize, column: usize) -> Option<Cell> {
        self.row(row)?.cells.get(column).copied()
    }

    /// Returns the full Unicode grapheme stored at a cell, preserving combining sequences.
    pub fn grapheme(&self, row: usize, column: usize) -> Option<String> {
        let cell = self.cell(row, column)?;
        if cell.is_wide_spacer() {
            return Some(String::new());
        }
        if cell.content == 0 {
            return None;
        }
        let row = self.row(row)?;
        if let Some(text) = row.graphemes[column].as_ref() {
            return Some(text.clone());
        }
        char::from_u32(cell.content).map(|ch| ch.to_string())
    }

    pub fn stored_grapheme(&self, row: usize, column: usize) -> Option<&str> {
        self.row(row)?.graphemes.get(column)?.as_deref()
    }

    pub fn hyperlink(&self, row: usize, column: usize) -> Option<u32> {
        self.row(row)?.hyperlinks.get(column).copied().flatten()
    }

    pub fn set_hyperlink(&mut self, row: usize, column: usize, link: Option<u32>) -> bool {
        if row >= self.height || column >= self.width {
            return false;
        }
        let target = self.row_mut(row).expect("validated row");
        if target.cells[column].content == 0 {
            return false;
        }
        target.hyperlinks[column] = link;
        if target.cells[column].is_wide_leading() {
            target.hyperlinks[column + 1] = link;
        }
        target.dirty = true;
        true
    }

    /// Stores one grapheme without making grid content depend on a font or GPU glyph ID.
    pub fn set_grapheme(&mut self, row: usize, column: usize, text: &str, cell: Cell) -> bool {
        let Some(ch) = text.chars().next() else {
            return false;
        };
        if row >= self.height
            || column >= self.width
            || cell.is_wide_leading()
            || cell.is_wide_spacer()
        {
            return false;
        }
        self.clear_cell_pair(row, column);
        let target = self.row_mut(row).expect("validated row");
        target.cells[column] = Cell {
            content: ch as u32,
            flags: cell.flags().without_wide().bits(),
            ..cell
        };
        target.graphemes[column] = multi_scalar_grapheme(text);
        target.hyperlinks[column] = None;
        target.dirty = true;
        true
    }

    /// Writes a normal cell. Wide structure can only be created with `set_wide_cell`.
    pub fn set_cell(&mut self, row: usize, column: usize, cell: Cell) -> bool {
        if column >= self.width
            || row >= self.height
            || cell.is_wide_leading()
            || cell.is_wide_spacer()
        {
            return false;
        }
        self.clear_cell_pair(row, column);
        let target = self.row_mut(row).expect("validated row");
        target.cells[column] = Cell {
            flags: cell.flags().without_wide().bits(),
            ..cell
        };
        target.graphemes[column] = None;
        target.hyperlinks[column] = None;
        target.dirty = true;
        true
    }

    /// Writes a leading cell and its spacer atomically, clearing any overlapped pairs.
    pub fn set_wide_cell(&mut self, row: usize, column: usize, leading: Cell) -> bool {
        self.set_wide_cell_inner(row, column, leading, None)
    }

    /// Writes a two-column grapheme and spacer atomically.
    pub fn set_wide_grapheme(
        &mut self,
        row: usize,
        column: usize,
        text: &str,
        leading: Cell,
    ) -> bool {
        self.set_wide_cell_inner(row, column, leading, multi_scalar_grapheme(text))
    }

    fn set_wide_cell_inner(
        &mut self,
        row: usize,
        column: usize,
        mut leading: Cell,
        grapheme: Option<String>,
    ) -> bool {
        if row >= self.height || column.checked_add(1).is_none_or(|end| end >= self.width) {
            return false;
        }
        self.clear_cell_pair(row, column);
        self.clear_cell_pair(row, column + 1);
        leading.flags = leading.flags().without_wide().bits() | CellFlags::WIDE_LEADING.bits();
        let spacer = Cell {
            content: 0,
            fg: leading.fg,
            bg: leading.bg,
            flags: CellFlags::WIDE_SPACER.bits(),
        };
        let target = self.row_mut(row).expect("validated row");
        target.cells[column] = leading;
        target.cells[column + 1] = spacer;
        target.graphemes[column] = grapheme;
        target.graphemes[column + 1] = None;
        target.hyperlinks[column] = None;
        target.hyperlinks[column + 1] = None;
        target.dirty = true;
        true
    }

    pub fn clear_cell(&mut self, row: usize, column: usize) -> bool {
        if row >= self.height || column >= self.width {
            return false;
        }
        self.clear_cell_pair(row, column);
        true
    }

    fn clear_cell_pair(&mut self, row: usize, column: usize) {
        let cell = self.row(row).expect("validated row").cells[column];
        let partner = if cell.is_wide_leading() {
            column.checked_add(1).filter(|&next| next < self.width)
        } else if cell.is_wide_spacer() {
            column.checked_sub(1)
        } else {
            None
        };
        let target = self.row_mut(row).expect("validated row");
        if target.cells[column] != Cell::EMPTY {
            target.cells[column] = Cell::EMPTY;
            target.graphemes[column] = None;
            target.hyperlinks[column] = None;
            target.dirty = true;
        }
        if let Some(partner) = partner {
            if target.cells[partner] != Cell::EMPTY {
                target.cells[partner] = Cell::EMPTY;
                target.graphemes[partner] = None;
                target.hyperlinks[partner] = None;
                target.dirty = true;
            }
        }
    }

    /// Writes as many ordinary cells as fit, returning the number written.
    pub fn write_range(&mut self, row: usize, start: usize, cells: &[Cell]) -> usize {
        if row >= self.height || start >= self.width {
            return 0;
        }
        let count = cells.len().min(self.width - start);
        let mut written = 0;
        for (offset, cell) in cells.iter().copied().take(count).enumerate() {
            if !self.set_cell(row, start + offset, cell) {
                break;
            }
            written += 1;
        }
        written
    }

    /// Inserts blank cells at a column, shifting the remaining row contents right.
    /// Row-local storage lets CSI insert operations avoid moving the rest of the screen.
    pub fn insert_cells(&mut self, row: usize, at: usize, count: usize) -> bool {
        if row >= self.height || at >= self.width {
            return false;
        }
        let amount = count.min(self.width - at);
        if amount == 0 {
            return true;
        }
        let width = self.width;
        let target = self.row_mut(row).expect("validated row");
        target.cells.copy_within(at..width - amount, at + amount);
        let shifted = target.graphemes[at..width - amount].to_vec();
        target.graphemes[at + amount..width].clone_from_slice(&shifted);
        target.cells[at..at + amount].fill(Cell::EMPTY);
        target.graphemes[at..at + amount].fill(None);
        let shifted_links = target.hyperlinks[at..width - amount].to_vec();
        target.hyperlinks[at + amount..width].clone_from_slice(&shifted_links);
        target.hyperlinks[at..at + amount].fill(None);
        repair_wide_pairs(&mut target.cells);
        clear_orphan_graphemes(&target.cells, &mut target.graphemes);
        clear_orphan_hyperlinks(&target.cells, &mut target.hyperlinks);
        target.dirty = true;
        true
    }

    /// Deletes cells at a column, shifting the remaining row contents left.
    /// Row-local storage lets CSI delete operations avoid moving the rest of the screen.
    pub fn delete_cells(&mut self, row: usize, at: usize, count: usize) -> bool {
        if row >= self.height || at >= self.width {
            return false;
        }
        let amount = count.min(self.width - at);
        if amount == 0 {
            return true;
        }
        let width = self.width;
        let target = self.row_mut(row).expect("validated row");
        target.cells.copy_within(at + amount..width, at);
        let shifted = target.graphemes[at + amount..width].to_vec();
        target.graphemes[at..width - amount].clone_from_slice(&shifted);
        target.cells[width - amount..].fill(Cell::EMPTY);
        target.graphemes[width - amount..].fill(None);
        let shifted_links = target.hyperlinks[at + amount..width].to_vec();
        target.hyperlinks[at..width - amount].clone_from_slice(&shifted_links);
        target.hyperlinks[width - amount..].fill(None);
        repair_wide_pairs(&mut target.cells);
        clear_orphan_graphemes(&target.cells, &mut target.graphemes);
        clear_orphan_hyperlinks(&target.cells, &mut target.hyperlinks);
        target.dirty = true;
        true
    }

    pub fn clear_row(&mut self, row: usize) -> bool {
        let Some(target) = self.row_mut(row) else {
            return false;
        };
        target.cells.fill(Cell::EMPTY);
        target.graphemes.fill(None);
        target.hyperlinks.fill(None);
        target.wrapped = false;
        target.dirty = true;
        true
    }

    /// Clears a half-open column range, clipping its end to the grid width.
    pub fn clear_range_in_row(&mut self, row: usize, columns: Range<usize>) -> bool {
        if row >= self.height || columns.start >= self.width || columns.start >= columns.end {
            return false;
        }
        for column in columns.start..columns.end.min(self.width) {
            self.clear_cell_pair(row, column);
        }
        if columns.start == 0 && columns.end >= self.width {
            self.row_mut(row).expect("validated row").wrapped = false;
        }
        true
    }

    pub fn clear_rect(&mut self, rows: Range<usize>, columns: Range<usize>) -> bool {
        if rows.start >= self.height
            || rows.start >= rows.end
            || columns.start >= self.width
            || columns.start >= columns.end
        {
            return false;
        }
        for row in rows.start..rows.end.min(self.height) {
            self.clear_range_in_row(row, columns.clone());
        }
        true
    }

    /// Clears cells and wrapped metadata without moving the cursor.
    pub fn clear_all(&mut self) {
        for row in &mut self.rows {
            row.cells.fill(Cell::EMPTY);
            row.graphemes.fill(None);
            row.hyperlinks.fill(None);
            row.wrapped = false;
            row.dirty = true;
        }
    }

    pub fn set_row_wrapped(&mut self, row: usize, wrapped: bool) -> bool {
        let Some(target) = self.row_mut(row) else {
            return false;
        };
        if target.wrapped != wrapped {
            target.wrapped = wrapped;
            target.dirty = true;
        }
        true
    }

    /// Scrolls an inclusive row region upward, replacing exposed rows with blanks.
    pub fn scroll_up(&mut self, top: usize, bottom: usize, count: usize) -> bool {
        self.scroll(top, bottom, count, true)
    }

    /// Scrolls an inclusive row region downward, replacing exposed rows with blanks.
    pub fn scroll_down(&mut self, top: usize, bottom: usize, count: usize) -> bool {
        self.scroll(top, bottom, count, false)
    }

    fn scroll(&mut self, top: usize, bottom: usize, count: usize, up: bool) -> bool {
        if top > bottom || bottom >= self.height || count == 0 {
            return top <= bottom && bottom < self.height;
        }
        let region_height = bottom - top + 1;
        let amount = count.min(region_height);
        if top == 0 && bottom + 1 == self.height {
            // Advancing the origin recycles only the rows exposed by the scroll.
            let shift = amount % self.height;
            if up {
                self.base_row = (self.base_row + shift) % self.height;
                for row in bottom + 1 - amount..=bottom {
                    self.reset_row(row);
                }
            } else {
                self.base_row = (self.base_row + self.height - shift) % self.height;
                for row in top..top + amount {
                    self.reset_row(row);
                }
                if top + amount <= bottom {
                    self.row_mut(top + amount).expect("valid region").wrapped = false;
                }
            }
            if amount == region_height {
                for logical in 0..self.height {
                    self.reset_row(logical);
                }
            }
            for logical in top..=bottom {
                self.row_mut(logical).expect("valid region").dirty = true;
            }
            return true;
        }
        for _ in 0..amount {
            if up {
                for row in top..bottom {
                    let a = self.physical_row(row).expect("valid region row");
                    let b = self.physical_row(row + 1).expect("valid region row");
                    self.rows.swap(a, b);
                }
                self.reset_row(bottom);
            } else {
                for row in (top + 1..=bottom).rev() {
                    let a = self.physical_row(row).expect("valid region row");
                    let b = self.physical_row(row - 1).expect("valid region row");
                    self.rows.swap(a, b);
                }
                self.reset_row(top);
            }
        }
        self.row_mut(top).expect("valid region").wrapped = false;
        if !up && top + amount <= bottom {
            self.row_mut(top + amount).expect("valid region").wrapped = false;
        }
        for logical in top..=bottom {
            self.row_mut(logical).expect("valid region").dirty = true;
        }
        true
    }

    fn reset_row(&mut self, logical_row: usize) {
        let row = self.row_mut(logical_row).expect("valid row");
        row.cells.fill(Cell::EMPTY);
        row.graphemes.fill(None);
        row.hyperlinks.fill(None);
        row.wrapped = false;
        row.dirty = true;
    }

    pub fn insert_rows(&mut self, at: usize, bottom: usize, count: usize) -> bool {
        if at > bottom || bottom >= self.height {
            return false;
        }
        let amount = count.min(bottom - at + 1);
        if amount == 0 {
            return true;
        }
        for _ in 0..amount {
            for row in (at + 1..=bottom).rev() {
                let a = self.physical_row(row).expect("valid region row");
                let b = self.physical_row(row - 1).expect("valid region row");
                self.rows.swap(a, b);
            }
            self.reset_row(at);
        }
        self.row_mut(at).expect("valid region").wrapped = false;
        if at + amount <= bottom {
            self.row_mut(at + amount).expect("valid region").wrapped = false;
        }
        for logical in at..=bottom {
            self.row_mut(logical).expect("valid region").dirty = true;
        }
        true
    }

    pub fn delete_rows(&mut self, at: usize, bottom: usize, count: usize) -> bool {
        if at > bottom || bottom >= self.height {
            return false;
        }
        let amount = count.min(bottom - at + 1);
        if amount == 0 {
            return true;
        }
        for _ in 0..amount {
            for row in at..bottom {
                let a = self.physical_row(row).expect("valid region row");
                let b = self.physical_row(row + 1).expect("valid region row");
                self.rows.swap(a, b);
            }
            self.reset_row(bottom);
        }
        self.row_mut(at).expect("valid region").wrapped = false;
        for logical in at..=bottom {
            self.row_mut(logical).expect("valid region").dirty = true;
        }
        true
    }

    /// Resizes without reflow: preserve the top-left intersection and discard the rest.
    pub fn resize(&mut self, new_width: usize, new_height: usize) -> Result<(), GridError> {
        if new_width == 0 {
            return Err(GridError::ZeroWidth);
        }
        if new_height == 0 {
            return Err(GridError::ZeroHeight);
        }
        validate_dimensions(new_width, new_height)?;

        let old_viewport_offset = self.viewport_offset;
        let old_cursor_row = self.cursor.row;
        let mut source: Vec<Row> = self.scrollback.iter().cloned().collect();
        source.extend((0..self.height).map(|row| self.row(row).expect("visible row").clone()));
        let cursor_source_row = self.scrollback.len() + old_cursor_row;
        let mut logical_lines: Vec<Vec<ReflowCell>> = Vec::new();
        let mut cursor_position = None;
        let mut source_line = 0;
        for (index, row) in source.iter().enumerate() {
            let source_offset;
            if logical_lines.is_empty() || !row.wrapped {
                logical_lines.push(Vec::new());
                source_line = logical_lines.len() - 1;
                source_offset = 0;
            } else {
                source_offset = logical_lines[source_line].len();
            }
            if index == cursor_source_row {
                cursor_position = Some((source_line, source_offset + self.cursor.column));
            }
            let continues = source.get(index + 1).is_some_and(|next| next.wrapped);
            let mut used = if continues {
                self.width
            } else {
                row.cells
                    .iter()
                    .rposition(|cell| cell.content != 0)
                    .map_or(0, |last| last + 1)
            };
            if index == cursor_source_row {
                used = used.max(self.cursor.column + 1);
            }
            if used > 0 && row.cells[used - 1].is_wide_leading() {
                used = (used + 1).min(self.width);
            }
            logical_lines[source_line].extend((0..used).map(|column| {
                (
                    row.cells[column],
                    row.graphemes[column].clone(),
                    row.hyperlinks[column],
                )
            }));
        }

        let mut reflowed = Vec::new();
        let mut new_cursor = None;
        for (line_index, line) in logical_lines.iter().enumerate() {
            let mut offset = 0;
            let mut continuation = false;
            if line.is_empty() {
                reflowed.push(Row::new(new_width));
                if cursor_position == Some((line_index, 0)) {
                    new_cursor = Some((reflowed.len() - 1, 0));
                }
            }
            while offset < line.len() {
                let mut row = Row::new(new_width);
                row.wrapped = continuation;
                let row_index = reflowed.len();
                let mut column = 0;
                while offset < line.len() && column < new_width {
                    let (cell, grapheme, hyperlink) = &line[offset];
                    if cell.is_wide_leading() {
                        if new_width == 1 {
                            row.cells[0] = Cell::new(
                                '\u{fffd}' as u32,
                                cell.fg,
                                cell.bg,
                                cell.flags().without_wide(),
                            );
                            row.graphemes[0] = Some("\u{fffd}".to_owned());
                            offset += 2;
                            column = 1;
                            continue;
                        }
                        if column + 1 >= new_width {
                            break;
                        }
                        row.cells[column] = *cell;
                        row.graphemes[column] = grapheme.clone();
                        row.hyperlinks[column] = *hyperlink;
                        if let Some((spacer, _, spacer_link)) = line.get(offset + 1) {
                            row.cells[column + 1] = *spacer;
                            row.hyperlinks[column + 1] = *spacer_link;
                        }
                        if cursor_position == Some((line_index, offset)) {
                            new_cursor = Some((row_index, column));
                        }
                        offset += 2;
                        column += 2;
                    } else {
                        row.cells[column] = *cell;
                        row.graphemes[column] = grapheme.clone();
                        row.hyperlinks[column] = *hyperlink;
                        if cursor_position == Some((line_index, offset)) {
                            new_cursor = Some((row_index, column));
                        }
                        offset += 1;
                        column += 1;
                    }
                }
                if offset < line.len() && column == 0 {
                    let (cell, grapheme, hyperlink) = &line[offset];
                    row.cells[0] = *cell;
                    row.graphemes[0] = grapheme.clone();
                    row.hyperlinks[0] = *hyperlink;
                    offset += 1;
                }
                reflowed.push(row);
                continuation = true;
            }
        }
        let desired_start = reflowed.len().saturating_sub(new_height);
        let start = new_cursor.map_or(desired_start, |(row, _)| desired_start.min(row));
        let mut scrollback: VecDeque<Row> = reflowed.drain(..start).collect();
        while scrollback.len() > self.scrollback_limit {
            scrollback.pop_front();
        }
        let cursor_row = new_cursor.map_or(0, |(row, _)| row.saturating_sub(start));
        let cursor_column = new_cursor
            .map_or(self.cursor.column.min(new_width - 1), |(_, column)| {
                column.min(new_width - 1)
            });
        let mut rows: Vec<Row> = reflowed.into_iter().take(new_height).collect();
        rows.resize_with(new_height, || Row::new(new_width));
        self.width = new_width;
        self.height = new_height;
        self.rows = rows;
        self.base_row = 0;
        self.scrollback = scrollback;
        self.viewport_offset = old_viewport_offset.min(self.scrollback.len());
        self.cursor.column = cursor_column;
        self.cursor.row = cursor_row.min(new_height - 1);
        self.mark_all_dirty();
        Ok(())
    }

    pub fn mark_row_dirty(&mut self, row: usize) -> bool {
        let Some(target) = self.row_mut(row) else {
            return false;
        };
        target.dirty = true;
        true
    }

    pub fn clear_row_dirty(&mut self, row: usize) -> bool {
        let Some(target) = self.row_mut(row) else {
            return false;
        };
        target.dirty = false;
        true
    }

    pub fn is_row_dirty(&self, row: usize) -> Option<bool> {
        self.row(row).map(Row::is_dirty)
    }

    pub fn mark_all_dirty(&mut self) {
        for row in &mut self.rows {
            row.dirty = true;
        }
    }

    /// Returns dirty visible row indices in order and consumes their dirty state.
    pub fn take_dirty_rows(&mut self) -> Vec<usize> {
        let dirty: Vec<_> = (0..self.height)
            .filter(|&row| self.row(row).is_some_and(Row::is_dirty))
            .collect();
        for &row in &dirty {
            self.row_mut(row).expect("visible row").dirty = false;
        }
        dirty
    }
}

fn validate_dimensions(width: usize, height: usize) -> Result<(), GridError> {
    let max_allocation = isize::MAX as usize;
    let cell_count = width
        .checked_mul(height)
        .ok_or(GridError::DimensionsTooLarge)?;
    let cell_row_bytes = width
        .checked_mul(std::mem::size_of::<Cell>())
        .ok_or(GridError::DimensionsTooLarge)?;
    let row_table_bytes = height
        .checked_mul(std::mem::size_of::<Row>())
        .ok_or(GridError::DimensionsTooLarge)?;
    if cell_count > max_allocation
        || cell_row_bytes > max_allocation
        || row_table_bytes > max_allocation
    {
        return Err(GridError::DimensionsTooLarge);
    }
    Ok(())
}

fn repair_wide_pairs(cells: &mut [Cell]) {
    for column in 0..cells.len() {
        let leading_valid = cells[column].is_wide_leading()
            && cells
                .get(column + 1)
                .is_some_and(|cell| cell.is_wide_spacer());
        let spacer_valid =
            cells[column].is_wide_spacer() && column > 0 && cells[column - 1].is_wide_leading();
        if (cells[column].is_wide_leading() && !leading_valid)
            || (cells[column].is_wide_spacer() && !spacer_valid)
        {
            cells[column] = Cell::EMPTY;
            // Invalid clipped pairs cannot retain semantic text without a leading cell.
        }
    }
}

fn clear_orphan_graphemes(cells: &[Cell], graphemes: &mut [Option<String>]) {
    for (cell, text) in cells.iter().zip(graphemes) {
        if cell.content == 0 {
            *text = None;
        }
    }
}

fn clear_orphan_hyperlinks(cells: &[Cell], hyperlinks: &mut [Option<u32>]) {
    for (cell, link) in cells.iter().zip(hyperlinks) {
        if cell.content == 0 {
            *link = None;
        }
    }
}

fn multi_scalar_grapheme(text: &str) -> Option<String> {
    (text.chars().count() > 1).then(|| text.to_owned())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveScreen {
    Main,
    Alternate,
}

/// Owns independent main and alternate screens and selects which one is active.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalGrid {
    main: Grid,
    alternate: Grid,
    active: ActiveScreen,
}

impl TerminalGrid {
    pub fn new(width: usize, height: usize) -> Result<Self, GridError> {
        Ok(Self {
            main: Grid::new(width, height)?,
            alternate: Grid::new(width, height)?,
            active: ActiveScreen::Main,
        })
    }

    pub const fn active_screen(&self) -> ActiveScreen {
        self.active
    }

    pub fn select_screen(&mut self, screen: ActiveScreen) {
        self.active = screen;
        self.screen_mut(screen).mark_all_dirty();
    }

    pub fn active(&self) -> &Grid {
        self.screen(self.active)
    }

    pub fn active_mut(&mut self) -> &mut Grid {
        self.screen_mut(self.active)
    }

    pub fn screen(&self, screen: ActiveScreen) -> &Grid {
        match screen {
            ActiveScreen::Main => &self.main,
            ActiveScreen::Alternate => &self.alternate,
        }
    }

    pub fn screen_mut(&mut self, screen: ActiveScreen) -> &mut Grid {
        match screen {
            ActiveScreen::Main => &mut self.main,
            ActiveScreen::Alternate => &mut self.alternate,
        }
    }

    pub fn resize(&mut self, width: usize, height: usize) -> Result<(), GridError> {
        self.main.resize(width, height)?;
        self.alternate.resize(width, height)
    }

    /// Sets the scrollback limit on the main screen. The alternate screen
    /// never records history, so it is left untouched.
    pub fn set_scrollback_limit(&mut self, limit: usize) {
        self.main.set_scrollback_limit(limit);
    }

    /// Rows retained above the visible screen on the main screen.
    pub fn scrollback_limit(&self) -> usize {
        self.main.scrollback_limit()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(width: usize, height: usize) -> Grid {
        Grid::new(width, height).unwrap()
    }

    fn cell(ch: char) -> Cell {
        Cell::new(ch as u32, 7, 0, CellFlags::default())
    }

    fn put_rows(grid: &mut Grid, rows: &[&str]) {
        for (row, text) in rows.iter().enumerate() {
            for (column, ch) in text.chars().enumerate() {
                grid.set_cell(row, column, cell(ch));
            }
        }
    }

    fn row_text(grid: &Grid, row: usize) -> String {
        grid.row_at(row)
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

    #[test]
    fn scrollback_limit_is_configurable_and_trims_immediately() {
        let mut grid = grid(4, 2);
        assert_eq!(grid.scrollback_limit(), DEFAULT_SCROLLBACK_ROWS);
        for _ in 0..8 {
            grid.scroll_up_recording(1);
        }
        assert_eq!(grid.scrollback_len(), 8);
        // A smaller limit trims what is already stored, not just future rows.
        grid.set_scrollback_limit(3);
        assert_eq!(grid.scrollback_len(), 3);
        assert_eq!(grid.scrollback_limit(), 3);
        grid.set_viewport_offset(usize::MAX);
        assert_eq!(grid.viewport_offset(), 3);
        // Zero disables history entirely.
        grid.set_scrollback_limit(0);
        assert_eq!(grid.scrollback_len(), 0);
        assert_eq!(grid.viewport_offset(), 0);
        for _ in 0..5 {
            grid.scroll_up_recording(1);
        }
        assert_eq!(grid.scrollback_len(), 0);
    }

    #[test]
    fn resize_respects_the_configured_scrollback_limit() {
        let mut grid = grid(3, 2);
        grid.set_scrollback_limit(2);
        for _ in 0..6 {
            grid.scroll_up_recording(1);
        }
        grid.resize(4, 3).unwrap();
        assert!(
            grid.scrollback_len() <= 2,
            "resize kept {} rows",
            grid.scrollback_len()
        );
        assert!(grid.viewport_offset() <= grid.scrollback_len());
    }

    #[test]
    fn construction_rejects_zero_dimensions_and_initializes_rows_dirty() {
        assert_eq!(Grid::new(0, 1), Err(GridError::ZeroWidth));
        assert_eq!(Grid::new(1, 0), Err(GridError::ZeroHeight));
        assert_eq!(Grid::new(usize::MAX, 1), Err(GridError::DimensionsTooLarge));
        let mut grid = grid(4, 3);
        assert_eq!((grid.width(), grid.height()), (4, 3));
        assert_eq!(grid.take_dirty_rows(), vec![0, 1, 2]);
    }

    #[test]
    fn reads_and_writes_cells_and_clips_ranges() {
        let mut grid = grid(4, 2);
        assert!(grid.set_cell(1, 2, cell('x')));
        assert_eq!(grid.cell(1, 2), Some(cell('x')));
        assert_eq!(
            grid.write_range(0, 2, &[cell('a'), cell('b'), cell('c')]),
            2
        );
        assert_eq!(row_text(&grid, 0), "..ab");
        assert_eq!(grid.cell(2, 0), None);
        assert!(!grid.set_cell(0, 4, cell('!')));
    }

    #[test]
    fn cursor_moves_and_clamps_to_valid_coordinates() {
        let mut grid = grid(5, 4);
        grid.set_cursor(99, 99);
        assert_eq!(grid.cursor(), Cursor { column: 4, row: 3 });
        grid.move_cursor(-99, -99);
        assert_eq!(grid.cursor(), Cursor::default());
        grid.move_cursor(2, 1);
        assert_eq!(grid.cursor(), Cursor { column: 2, row: 1 });
    }

    #[test]
    fn logical_to_physical_mapping_wraps_and_full_scroll_recycles_rows() {
        let mut grid = grid(4, 3);
        put_rows(&mut grid, &["AAAA", "BBBB", "CCCC"]);
        let first_physical = grid.physical_row(0).unwrap();
        grid.scroll_up(0, 2, 1);
        assert_eq!(grid.base_row, (first_physical + 1) % 3);
        assert_eq!(
            [row_text(&grid, 0), row_text(&grid, 1), row_text(&grid, 2)],
            ["BBBB", "CCCC", "...."]
        );
    }

    #[test]
    fn row_reordering_marks_every_affected_visible_row_dirty() {
        let mut grid = grid(2, 4);
        grid.take_dirty_rows();
        grid.scroll_up(0, 3, 1);
        assert_eq!(grid.take_dirty_rows(), vec![0, 1, 2, 3]);
        grid.insert_rows(1, 3, 1);
        assert_eq!(grid.take_dirty_rows(), vec![1, 2, 3]);
        grid.delete_rows(0, 2, 1);
        assert_eq!(grid.take_dirty_rows(), vec![0, 1, 2]);
    }

    #[test]
    fn full_scroll_handles_small_4_by_3_and_5_by_4_grids() {
        for (width, height) in [(4, 3), (5, 4)] {
            let mut grid = grid(width, height);
            let rows: Vec<String> = (0..height)
                .map(|row| char::from(b'A' + row as u8).to_string().repeat(width))
                .collect();
            for (row, text) in rows.iter().enumerate() {
                grid.write_range(row, 0, &text.chars().map(cell).collect::<Vec<_>>());
            }
            grid.scroll_up(0, height - 1, 1);
            assert_eq!(row_text(&grid, 0), rows[1]);
            assert_eq!(row_text(&grid, height - 1), ".".repeat(width));
            grid.scroll_down(0, height - 1, 2);
            assert_eq!(row_text(&grid, 0), ".".repeat(width));
            assert_eq!(row_text(&grid, 2), rows[1]);
        }
    }

    #[test]
    fn scroll_supports_partial_regions_and_counts_larger_than_region() {
        let mut grid = grid(4, 5);
        put_rows(&mut grid, &["AAAA", "BBBB", "CCCC", "DDDD", "EEEE"]);
        assert!(grid.scroll_up(1, 3, 2));
        assert_eq!(
            [
                row_text(&grid, 0),
                row_text(&grid, 1),
                row_text(&grid, 2),
                row_text(&grid, 3),
                row_text(&grid, 4)
            ],
            ["AAAA", "DDDD", "....", "....", "EEEE"]
        );
        assert!(grid.scroll_down(1, 3, 9));
        assert_eq!(
            [row_text(&grid, 1), row_text(&grid, 2), row_text(&grid, 3)],
            ["....", "....", "...."]
        );
        assert!(!grid.scroll_up(0, 5, 1));
    }

    #[test]
    fn scroll_by_zero_is_a_valid_noop_for_valid_region() {
        let mut grid = grid(2, 2);
        assert!(grid.scroll_up(0, 1, 0));
        assert!(!grid.scroll_up(1, 0, 1));
    }

    #[test]
    fn clear_operations_preserve_cursor_and_repair_wide_pairs() {
        let mut grid = grid(5, 2);
        grid.set_cursor(3, 1);
        assert!(grid.set_wide_cell(0, 1, cell('界')));
        assert!(grid.clear_range_in_row(0, 2..3));
        assert_eq!(grid.cell(0, 1), Some(Cell::EMPTY));
        assert_eq!(grid.cell(0, 2), Some(Cell::EMPTY));
        put_rows(&mut grid, &["abcde", "12345"]);
        grid.set_row_wrapped(0, true);
        assert!(grid.clear_rect(0..1, 1..3));
        assert_eq!(row_text(&grid, 0), "a..de");
        assert!(grid.clear_row(0));
        assert!(!grid.row_at(0).unwrap().is_wrapped());
        grid.set_row_wrapped(1, true);
        grid.clear_all();
        assert!(!grid.row_at(1).unwrap().is_wrapped());
        assert_eq!(grid.cursor(), Cursor { column: 3, row: 1 });
    }

    #[test]
    fn clear_range_and_rect_clip_at_grid_edges() {
        let mut grid = grid(3, 2);
        put_rows(&mut grid, &["abc", "def"]);
        assert!(grid.clear_range_in_row(0, 1..99));
        assert_eq!(row_text(&grid, 0), "a..");
        assert!(grid.clear_rect(1..99, 0..99));
        assert_eq!(row_text(&grid, 1), "...");
    }

    #[test]
    fn row_insertion_and_deletion_are_region_bounded() {
        let mut grid = grid(2, 5);
        put_rows(&mut grid, &["AA", "BB", "CC", "DD", "EE"]);
        assert!(grid.insert_rows(1, 3, 1));
        assert_eq!(
            [
                row_text(&grid, 0),
                row_text(&grid, 1),
                row_text(&grid, 2),
                row_text(&grid, 3),
                row_text(&grid, 4)
            ],
            ["AA", "..", "BB", "CC", "EE"]
        );
        assert!(grid.delete_rows(1, 3, 2));
        assert_eq!(
            [
                row_text(&grid, 0),
                row_text(&grid, 1),
                row_text(&grid, 2),
                row_text(&grid, 3),
                row_text(&grid, 4)
            ],
            ["AA", "CC", "..", "..", "EE"]
        );
        assert_eq!((grid.width(), grid.height()), (2, 5));
    }

    #[test]
    fn cell_insertion_and_deletion_preserve_wide_pair_invariants() {
        let mut grid = grid(6, 1);
        grid.write_range(
            0,
            0,
            &[
                cell('A'),
                cell('B'),
                cell('C'),
                cell('D'),
                cell('E'),
                cell('F'),
            ],
        );
        assert!(grid.insert_cells(0, 1, 2));
        assert_eq!(row_text(&grid, 0), "A..BCD");
        assert!(grid.delete_cells(0, 1, 2));
        assert_eq!(row_text(&grid, 0), "ABCD..");
        assert!(grid.set_wide_cell(0, 3, cell('界')));
        assert!(grid.delete_cells(0, 4, 1));
        for column in 0..grid.width() {
            let cell = grid.cell(0, column).unwrap();
            if cell.is_wide_leading() {
                assert!(grid.cell(0, column + 1).unwrap().is_wide_spacer());
            }
            if cell.is_wide_spacer() {
                assert!(column > 0 && grid.cell(0, column - 1).unwrap().is_wide_leading());
            }
        }
    }

    #[test]
    fn resize_reflows_logical_rows_and_keeps_cursor_in_bounds() {
        let mut grid = grid(3, 2);
        put_rows(&mut grid, &["abc", "def"]);
        grid.set_cursor(2, 1);
        grid.resize(5, 3).unwrap();
        assert_eq!((grid.width(), grid.height()), (5, 3));
        assert_eq!(
            [row_text(&grid, 0), row_text(&grid, 1), row_text(&grid, 2)],
            ["abc..", "def..", "....."]
        );
        grid.resize(2, 1).unwrap();
        assert_eq!(row_text(&grid, 0), "f.");
        assert_eq!(grid.cursor(), Cursor { column: 0, row: 0 });
        assert!(!grid.take_dirty_rows().is_empty());
    }

    #[test]
    fn resize_rejects_zero_without_changing_grid() {
        let mut grid = grid(3, 2);
        let before = grid.clone();
        assert_eq!(grid.resize(0, 4), Err(GridError::ZeroWidth));
        assert_eq!(grid.resize(4, 0), Err(GridError::ZeroHeight));
        assert_eq!(grid, before);
    }

    #[test]
    fn dirty_tracking_is_row_scoped_and_consumed_in_visible_order() {
        let mut grid = grid(3, 3);
        assert_eq!(grid.take_dirty_rows(), vec![0, 1, 2]);
        assert!(grid.set_cell(2, 1, cell('x')));
        assert_eq!(grid.take_dirty_rows(), vec![2]);
        assert!(grid.take_dirty_rows().is_empty());
        grid.mark_row_dirty(0);
        assert_eq!(grid.is_row_dirty(0), Some(true));
        grid.clear_row_dirty(0);
        grid.mark_all_dirty();
        assert_eq!(grid.take_dirty_rows(), vec![0, 1, 2]);
    }

    #[test]
    fn cursor_motion_dirties_the_old_and_new_cursor_rows() {
        let mut grid = grid(3, 3);
        grid.take_dirty_rows();
        grid.set_cursor(1, 2);
        assert_eq!(grid.take_dirty_rows(), vec![0, 2]);
        grid.move_cursor(1, 0);
        assert_eq!(grid.take_dirty_rows(), vec![2]);
    }

    #[test]
    fn wide_cells_are_atomic_and_clearing_either_half_clears_both() {
        let mut grid = grid(5, 1);
        assert!(!grid.set_cell(0, 0, Cell::new(' ' as u32, 0, 0, CellFlags::WIDE_SPACER)));
        assert!(grid.set_wide_cell(0, 2, cell('界')));
        assert!(
            grid.cell(0, 2)
                .unwrap()
                .flags()
                .contains(CellFlags::WIDE_LEADING)
        );
        assert!(
            grid.cell(0, 3)
                .unwrap()
                .flags()
                .contains(CellFlags::WIDE_SPACER)
        );
        assert!(!grid.set_wide_cell(0, 4, cell('界')));
        assert!(grid.clear_cell(0, 3));
        assert_eq!(grid.cell(0, 2), Some(Cell::EMPTY));
        assert_eq!(grid.cell(0, 3), Some(Cell::EMPTY));
    }

    #[test]
    fn overwriting_one_wide_half_repairs_the_pair() {
        let mut grid = grid(4, 1);
        grid.set_wide_cell(0, 1, cell('界'));
        grid.set_cell(0, 2, cell('x'));
        assert_eq!(grid.cell(0, 1), Some(Cell::EMPTY));
        assert_eq!(grid.cell(0, 2), Some(cell('x')));
    }

    #[test]
    fn resizing_truncates_incomplete_wide_cells() {
        let mut grid = grid(4, 1);
        grid.set_wide_cell(0, 2, cell('界'));
        grid.resize(3, 1).unwrap();
        assert_eq!(row_text(&grid, 0), "...");
        assert!(
            grid.row_at(0)
                .unwrap()
                .cells()
                .iter()
                .all(|cell| !cell.is_wide_spacer() && !cell.is_wide_leading())
        );
    }

    #[test]
    fn wrapped_metadata_tracks_logical_line_continuation() {
        let mut grid = grid(3, 2);
        assert!(grid.set_row_wrapped(1, true));
        assert!(grid.row_at(1).unwrap().is_wrapped());
        grid.clear_row(1);
        assert!(!grid.row_at(1).unwrap().is_wrapped());
        grid.set_row_wrapped(0, true);
        grid.scroll_up(0, 1, 1);
        assert!(!grid.row_at(1).unwrap().is_wrapped());

        let mut grid = Grid::new(3, 4).unwrap();
        grid.set_row_wrapped(0, true);
        grid.set_row_wrapped(2, true);
        grid.scroll_down(0, 3, 1);
        assert!(!grid.row_at(1).unwrap().is_wrapped());
        grid.set_row_wrapped(2, true);
        grid.insert_rows(1, 3, 1);
        assert!(!grid.row_at(2).unwrap().is_wrapped());
    }

    #[test]
    fn main_and_alternate_screens_keep_independent_state() {
        let mut terminal = TerminalGrid::new(3, 2).unwrap();
        terminal.active_mut().set_cell(0, 0, cell('m'));
        terminal.active_mut().set_cursor(2, 1);
        terminal.select_screen(ActiveScreen::Alternate);
        assert_eq!(terminal.active().cell(0, 0), Some(Cell::EMPTY));
        terminal.active_mut().set_cell(0, 0, cell('a'));
        terminal.active_mut().set_cursor(1, 0);
        terminal.select_screen(ActiveScreen::Main);
        assert_eq!(terminal.active().cell(0, 0), Some(cell('m')));
        assert_eq!(terminal.active().cursor(), Cursor { column: 2, row: 1 });
        terminal.resize(4, 3).unwrap();
        assert_eq!(
            (
                terminal.screen(ActiveScreen::Alternate).width(),
                terminal.screen(ActiveScreen::Alternate).height()
            ),
            (4, 3)
        );
    }

    #[test]
    fn grapheme_storage_moves_with_cells_and_wide_pair_clears_atomically() {
        let mut grid = grid(5, 1);
        assert!(grid.set_grapheme(0, 0, "e\u{301}", cell('e')));
        assert_eq!(grid.grapheme(0, 0).as_deref(), Some("e\u{301}"));
        assert!(grid.set_wide_grapheme(0, 2, "界", cell('界')));
        assert_eq!(grid.grapheme(0, 2).as_deref(), Some("界"));
        grid.insert_cells(0, 0, 1);
        assert_eq!(grid.grapheme(0, 1).as_deref(), Some("e\u{301}"));
        assert_eq!(grid.grapheme(0, 3).as_deref(), Some("界"));
        grid.clear_cell(0, 4);
        assert_eq!(grid.cell(0, 3), Some(Cell::EMPTY));
        assert_eq!(grid.cell(0, 4), Some(Cell::EMPTY));
        assert_eq!(grid.grapheme(0, 3), None);
    }

    #[test]
    fn valid_operation_sequence_keeps_all_visible_rows_and_cursor_in_bounds() {
        let mut grid = grid(4, 3);
        for step in 0..100 {
            match step % 7 {
                0 => {
                    grid.scroll_up(0, 2, step % 5);
                }
                1 => {
                    grid.scroll_down(0, 2, step % 4);
                }
                2 => {
                    grid.insert_rows(1, 2, step % 3);
                }
                3 => {
                    grid.delete_rows(0, 2, step % 4);
                }
                4 => {
                    grid.clear_rect(0..3, 1..4);
                }
                5 => {
                    grid.set_cursor(step, step * 2);
                }
                _ => {
                    grid.resize(2 + step % 5, 1 + step % 4).unwrap();
                }
            }
            assert_eq!(grid.rows.len(), grid.height);
            assert!(grid.base_row < grid.height);
            assert!(grid.cursor.column < grid.width && grid.cursor.row < grid.height);
            for row in 0..grid.height {
                assert!(grid.row(row).is_some());
                for column in 0..grid.width {
                    let cell = grid.cell(row, column).unwrap();
                    if cell.is_wide_spacer() {
                        assert!(
                            column > 0 && grid.cell(row, column - 1).unwrap().is_wide_leading()
                        );
                    }
                    if cell.is_wide_leading() {
                        assert!(
                            column + 1 < grid.width
                                && grid.cell(row, column + 1).unwrap().is_wide_spacer()
                        );
                    }
                }
            }
        }
    }
}
