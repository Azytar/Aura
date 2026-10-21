use std::collections::VecDeque;
use std::ops::Range;

/// Rows kept above the visible screen when nothing else is requested.
pub const DEFAULT_SCROLLBACK_ROWS: usize = 10_000;

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
    pub const FAINT: Self = Self(1 << 7);
    pub const HIDDEN: Self = Self(1 << 8);
    /// SGR 4:2 through 4:5 use three style bits in addition to UNDERLINE.
    pub const UNDERLINE_STYLE_MASK: Self = Self(7 << 9);

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

/// Canonical metadata prefix: trailing empty entries never retain storage.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Sidecar<T>(Vec<Option<T>>);

impl<T> Sidecar<T> {
    fn new() -> Self {
        Self(Vec::new())
    }
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    fn get(&self, column: usize) -> Option<&T> {
        self.0.get(column).and_then(Option::as_ref)
    }
    fn get_mut(&mut self, column: usize) -> Option<&mut T> {
        self.0.get_mut(column).and_then(Option::as_mut)
    }
    fn set(&mut self, column: usize, value: Option<T>) {
        if value.is_none() && column >= self.0.len() {
            return;
        }
        if value.is_some() && column >= self.0.len() {
            self.0.resize_with(column + 1, || None);
        }
        if let Some(slot) = self.0.get_mut(column) {
            *slot = value;
        }
        self.trim();
    }
    fn clear(&mut self) {
        self.0 = Vec::new();
    }
    fn trim(&mut self) {
        while self.0.last().is_some_and(Option::is_none) {
            self.0.pop();
        }
        if self.is_empty() {
            self.clear();
        }
    }
    fn clear_range(&mut self, range: Range<usize>) {
        let end = range.end.min(self.0.len());
        if range.start < end {
            for slot in &mut self.0[range.start..end] {
                *slot = None;
            }
            self.trim();
        }
    }
    fn is_clear_from(&self, column: usize) -> bool {
        column >= self.0.len()
    }
    fn truncate(&mut self, width: usize) {
        self.0.truncate(width);
        self.trim();
    }
    fn insert(&mut self, at: usize, amount: usize, width: usize) {
        self.truncate(width.saturating_sub(amount));
        if at < self.0.len() && amount != 0 {
            let old_len = self.0.len();
            self.0.resize_with(old_len + amount, || None);
            for index in (at..old_len).rev() {
                self.0[index + amount] = self.0[index].take();
            }
        }
        self.trim();
    }
    fn delete(&mut self, at: usize, amount: usize) {
        if at < self.0.len() {
            let end = at.saturating_add(amount).min(self.0.len());
            self.0.drain(at..end);
            self.trim();
        }
    }
    fn clone_prefix(&self, width: usize) -> Self
    where
        T: Clone,
    {
        let mut result = Self(self.0.iter().take(width).cloned().collect());
        result.trim();
        result
    }
}

/// One physical row and the metadata that links it to a logical line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    cells: Box<[Cell]>,
    graphemes: Sidecar<String>,
    hyperlinks: Sidecar<u32>,
    underline_colors: Sidecar<u32>,
    wrapped: bool,
    dirty: bool,
}

impl Row {
    /// Clear only the partners outside an overwritten range. Pairs wholly
    /// inside it are replaced together by the caller.
    fn clear_wide_edges(&mut self, start: usize, end: usize) {
        let left = if self.cells[start].is_wide_spacer() {
            start.checked_sub(1)
        } else {
            None
        };
        let right =
            (self.cells[end - 1].is_wide_leading() && end < self.cells.len()).then_some(end);
        for column in [left, right].into_iter().flatten() {
            self.cells[column] = Cell::EMPTY;
            self.graphemes.set(column, None);
            self.hyperlinks.set(column, None);
            self.underline_colors.set(column, None);
        }
    }

    pub fn underline_color_at(&self, column: usize) -> Option<u32> {
        self.underline_colors.get(column).copied()
    }

    fn new(width: usize) -> Self {
        Self {
            cells: vec![Cell::EMPTY; width].into_boxed_slice(),
            graphemes: Sidecar::new(),
            hyperlinks: Sidecar::new(),
            underline_colors: Sidecar::new(),
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
        self.graphemes
            .get(column)
            .cloned()
            .or_else(|| char::from_u32(cell.content).map(|ch| ch.to_string()))
    }

    pub fn stored_grapheme_at(&self, column: usize) -> Option<&str> {
        self.graphemes.get(column).map(String::as_str)
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
///
/// Rows are numbered from the first line the grid ever held. `first_line`
/// advances whenever old rows are dropped, so a line number keeps identifying
/// the same content while the viewport scrolls and only loses meaning once
/// that content itself is trimmed away.
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
    first_line: u64,
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
            first_line: 0,
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
        if logical_row >= self.height {
            return None;
        }
        let remaining = self.height - self.base_row;
        Some(if logical_row >= remaining {
            logical_row - remaining
        } else {
            self.base_row + logical_row
        })
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

    /// Absolute number of the earliest retained line. Dropping old history
    /// advances this origin instead of renumbering the surviving lines.
    pub const fn first_line(&self) -> u64 {
        self.first_line
    }

    /// Absolute number of the last retained line.
    pub fn last_line(&self) -> u64 {
        self.first_line + (self.scrollback.len() + self.height) as u64 - 1
    }

    /// Returns a row by its absolute line number.
    pub fn row_at_line(&self, line: u64) -> Option<&Row> {
        let index = usize::try_from(line.checked_sub(self.first_line)?).ok()?;
        if index < self.scrollback.len() {
            self.scrollback.get(index)
        } else {
            self.row(index - self.scrollback.len())
        }
    }

    /// Absolute line number shown at a viewport row.
    pub fn viewport_line(&self, row: usize) -> Option<u64> {
        if row >= self.height {
            return None;
        }
        let total = self.scrollback.len() + self.height;
        let end = total.saturating_sub(self.viewport_offset);
        let start = end.saturating_sub(self.height);
        Some(self.first_line + (start + row) as u64)
    }

    /// Viewport row currently showing an absolute line, when it is visible.
    pub fn line_viewport(&self, line: u64) -> Option<usize> {
        let index = usize::try_from(line.checked_sub(self.first_line)?).ok()?;
        let total = self.scrollback.len() + self.height;
        let end = total.saturating_sub(self.viewport_offset);
        let start = end.saturating_sub(self.height);
        (index >= start && index < end).then_some(index - start)
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
            self.first_line += 1;
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
            if self.scrollback_limit == 0 {
                self.first_line += 1;
                continue;
            }
            let replacement = if self.scrollback.len() == self.scrollback_limit {
                self.first_line += 1;
                self.viewport_offset = self.viewport_offset.saturating_sub(1);
                self.scrollback.pop_front().expect("full nonempty history")
            } else {
                Row::new(self.width)
            };
            let physical = self.physical_row(row).expect("visible row");
            // Transfer owned cells and metadata into history. Once full,
            // recycle the evicted row instead of allocating and cloning it.
            let retained = std::mem::replace(&mut self.rows[physical], replacement);
            self.scrollback.push_back(retained);
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
        if let Some(text) = row.graphemes.get(column) {
            return Some(text.clone());
        }
        char::from_u32(cell.content).map(|ch| ch.to_string())
    }

    pub fn stored_grapheme(&self, row: usize, column: usize) -> Option<&str> {
        self.row(row)?.graphemes.get(column).map(String::as_str)
    }

    /// Retains a validated extension whose leading scalar and cell width did
    /// not change. Rendition and both halves' metadata remain owned in place.
    pub(crate) fn extend_grapheme(&mut self, row: usize, column: usize, text: String) {
        let target = self.row_mut(row).expect("last cell remains visible");
        debug_assert_eq!(
            text.chars().next().map(u32::from),
            Some(target.cells[column].content)
        );
        target.graphemes.set(column, Some(text));
        target.dirty = true;
    }

    pub(crate) fn append_ascii_diacritic(&mut self, row: usize, column: usize, ch: char) {
        let target = self.row_mut(row).expect("last cell remains visible");
        if let Some(text) = target.graphemes.get_mut(column) {
            if text.len() + ch.len_utf8() <= 256 {
                text.push(ch);
                target.dirty = true;
            }
        } else {
            let base = char::from_u32(target.cells[column].content).expect("ASCII base");
            let mut text = String::with_capacity(base.len_utf8() + ch.len_utf8());
            text.push(base);
            text.push(ch);
            target.graphemes.set(column, Some(text));
            target.dirty = true;
        }
    }

    /// Applies to a printed cell and both halves of a wide character.
    pub fn set_underline_color(&mut self, row: usize, column: usize, color: Option<u32>) -> bool {
        if row >= self.height || column >= self.width {
            return false;
        }
        let target = self.row_mut(row).expect("validated row");
        if target.cells[column].content == 0 {
            return false;
        }
        target.underline_colors.set(column, color);
        if target.cells[column].is_wide_leading() {
            target.underline_colors.set(column + 1, color);
        }
        target.dirty = true;
        true
    }

    pub fn hyperlink(&self, row: usize, column: usize) -> Option<u32> {
        self.row(row)?.hyperlinks.get(column).copied()
    }

    pub fn set_hyperlink(&mut self, row: usize, column: usize, link: Option<u32>) -> bool {
        if row >= self.height || column >= self.width {
            return false;
        }
        let target = self.row_mut(row).expect("validated row");
        if target.cells[column].content == 0 {
            return false;
        }
        target.hyperlinks.set(column, link);
        if target.cells[column].is_wide_leading() {
            target.hyperlinks.set(column + 1, link);
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
        let target = self.row_mut(row).expect("validated row");
        target.clear_wide_edges(column, column + 1);
        target.cells[column] = Cell {
            content: ch as u32,
            flags: cell.flags().without_wide().bits(),
            ..cell
        };
        target.graphemes.set(column, multi_scalar_grapheme(text));
        target.hyperlinks.set(column, None);
        target.underline_colors.set(column, None);
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
        let target = self.row_mut(row).expect("validated row");
        target.clear_wide_edges(column, column + 1);
        target.cells[column] = Cell {
            flags: cell.flags().without_wide().bits(),
            ..cell
        };
        target.graphemes.set(column, None);
        target.hyperlinks.set(column, None);
        target.underline_colors.set(column, None);
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
        leading.flags = leading.flags().without_wide().bits() | CellFlags::WIDE_LEADING.bits();
        let spacer = Cell {
            content: 0,
            fg: leading.fg,
            bg: leading.bg,
            flags: leading.flags().without_wide().bits() | CellFlags::WIDE_SPACER.bits(),
        };
        let target = self.row_mut(row).expect("validated row");
        target.clear_wide_edges(column, column + 2);
        target.cells[column] = leading;
        target.cells[column + 1] = spacer;
        target.graphemes.set(column, grapheme);
        target.graphemes.set(column + 1, None);
        target.hyperlinks.set(column, None);
        target.underline_colors.set(column, None);
        target.hyperlinks.set(column + 1, None);
        target.underline_colors.set(column + 1, None);
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
            target.graphemes.set(column, None);
            target.hyperlinks.set(column, None);
            target.underline_colors.set(column, None);
            target.dirty = true;
        }
        if let Some(partner) = partner
            && target.cells[partner] != Cell::EMPTY
        {
            target.cells[partner] = Cell::EMPTY;
            target.graphemes.set(partner, None);
            target.hyperlinks.set(partner, None);
            target.underline_colors.set(partner, None);
            target.dirty = true;
        }
    }

    /// Writes a validated printable ASCII run, clearing wide pairs at its edges.
    pub(crate) fn write_ascii(
        &mut self,
        row: usize,
        start: usize,
        bytes: &[u8],
        cell: Cell,
        link: Option<u32>,
        underline: Option<u32>,
    ) {
        debug_assert!(!bytes.is_empty() && start + bytes.len() <= self.width);
        debug_assert!(bytes.iter().all(|byte| (0x20..=0x7e).contains(byte)));
        let end = start + bytes.len();
        let target = self.row_mut(row).expect("validated row");
        target.clear_wide_edges(start, end);
        target.graphemes.clear_range(start..end);
        target.hyperlinks.clear_range(start..end);
        target.underline_colors.clear_range(start..end);
        for (target, &byte) in target.cells[start..end].iter_mut().zip(bytes) {
            *target = Cell {
                content: u32::from(byte),
                ..cell
            };
        }
        if link.is_some() {
            for column in start..end {
                target.hyperlinks.set(column, link);
            }
        }
        if underline.is_some() {
            for column in start..end {
                target.underline_colors.set(column, underline);
            }
        }
        target.dirty = true;
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
        target.cells[at..at + amount].fill(Cell::EMPTY);
        target.graphemes.insert(at, amount, width);
        target.hyperlinks.insert(at, amount, width);
        target.underline_colors.insert(at, amount, width);
        repair_wide_pairs(&mut target.cells);
        clear_orphan_graphemes(&target.cells, &mut target.graphemes);
        clear_orphan_metadata(&target.cells, &mut target.hyperlinks);
        clear_orphan_metadata(&target.cells, &mut target.underline_colors);
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
        target.cells[width - amount..].fill(Cell::EMPTY);
        target.graphemes.delete(at, amount);
        target.hyperlinks.delete(at, amount);
        target.underline_colors.delete(at, amount);
        repair_wide_pairs(&mut target.cells);
        clear_orphan_graphemes(&target.cells, &mut target.graphemes);
        clear_orphan_metadata(&target.cells, &mut target.hyperlinks);
        clear_orphan_metadata(&target.cells, &mut target.underline_colors);
        target.dirty = true;
        true
    }

    pub fn clear_row(&mut self, row: usize) -> bool {
        let Some(target) = self.row_mut(row) else {
            return false;
        };
        target.cells.fill(Cell::EMPTY);
        target.graphemes.clear();
        target.hyperlinks.clear();
        target.underline_colors.clear();
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

    /// Erases with terminal rendition, including both halves of any wide cell
    /// intersecting the range. Sidecar text and links cannot survive erasure.
    pub fn erase_range_in_row(
        &mut self,
        row: usize,
        columns: Range<usize>,
        mut blank: Cell,
    ) -> bool {
        if row >= self.height || columns.start >= self.width || columns.start >= columns.end {
            return false;
        }
        let width = self.width;
        let target = self.row_mut(row).expect("validated row");
        let mut start = columns.start;
        let mut end = columns.end.min(width);
        if target.cells[start].is_wide_spacer() {
            start = start.saturating_sub(1);
        }
        if target.cells[end - 1].is_wide_leading() {
            end = (end + 1).min(width);
        }
        blank.content = 0;
        blank.flags = 0;
        target.cells[start..end].fill(blank);
        target.graphemes.clear_range(start..end);
        target.hyperlinks.clear_range(start..end);
        target.underline_colors.clear_range(start..end);
        if start == 0 && end == width {
            target.wrapped = false;
        }
        target.dirty = true;
        true
    }

    /// Discards saved rows and returns the viewport to the live screen.
    pub fn clear_scrollback(&mut self) {
        self.first_line += self.scrollback.len() as u64;
        self.scrollback.clear();
        self.set_viewport_offset(0);
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
            row.graphemes.clear();
            row.hyperlinks.clear();
            row.underline_colors.clear();
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
        row.graphemes.clear();
        row.hyperlinks.clear();
        row.underline_colors.clear();
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

    /// Reflows logical main-screen lines while retaining the cursor and history.
    pub fn resize(&mut self, new_width: usize, new_height: usize) -> Result<(), GridError> {
        // Same-size resizes (font zoom) must not reflow: rebuilding logical
        // lines here would reorder scrollback and move the cursor even though
        // no cell changed size. The caller still repaints via dirty state.
        if new_width == self.width && new_height == self.height {
            return Ok(());
        }
        if new_width == 0 {
            return Err(GridError::ZeroWidth);
        }
        if new_height == 0 {
            return Err(GridError::ZeroHeight);
        }
        validate_dimensions(new_width, new_height)?;

        if self.resize_fitting_rows(new_width, new_height) {
            return Ok(());
        }
        self.resize_reflow(new_width, new_height)
    }

    // Independent lines that fit need no semantic reconstruction. Reject
    // decorated tails as full reflow intentionally trims those differently.
    fn resize_fitting_rows(&mut self, width: usize, height: usize) -> bool {
        if self.cursor.column >= width
            || !self.scrollback.iter().chain(&self.rows).all(|row| {
                let mut used = row
                    .cells
                    .iter()
                    .rposition(|cell| cell.content != 0)
                    .map_or(0, |last| last + 1);
                if used > 0 && row.cells[used - 1].is_wide_leading() {
                    used = (used + 1).min(self.width);
                }
                !row.wrapped
                    && used <= width
                    && row.cells[used..].iter().all(|cell| *cell == Cell::EMPTY)
                    && row.graphemes.is_clear_from(used)
                    && row.hyperlinks.is_clear_from(used)
                    && row.underline_colors.is_clear_from(used)
            })
        {
            return false;
        }

        let old_history = self.scrollback.len();
        let cursor_index = old_history + self.cursor.row;
        let mut retained = old_history + self.height;
        while retained > cursor_index + 1 {
            let row = self.row(retained - old_history - 1).expect("visible tail");
            if row.cells.iter().any(|cell| *cell != Cell::EMPTY) {
                break;
            }
            retained -= 1;
        }
        let start = retained.saturating_sub(height).min(cursor_index);
        let resize_row = |row: &mut Row| {
            if row.cells.len() != width {
                let mut cells = std::mem::take(&mut row.cells).into_vec();
                cells.resize(width, Cell::EMPTY);
                row.cells = cells.into_boxed_slice();
                row.graphemes.truncate(width);
                row.hyperlinks.truncate(width);
                row.underline_colors.truncate(width);
            }
            row.dirty = true;
        };
        // Preserve the direct width-only traversal, including the ring origin.
        if height == self.height && start == old_history && retained == old_history + self.height {
            for row in self.scrollback.iter_mut().chain(&mut self.rows) {
                resize_row(row);
            }
            self.width = width;
            return true;
        }
        self.rows.rotate_left(self.base_row);
        let mut all: Vec<Row> = self
            .scrollback
            .drain(..)
            .chain(self.rows.drain(..))
            .take(retained)
            .collect();
        for row in &mut all {
            resize_row(row);
        }
        let mut visible = all.split_off(start);
        visible.truncate(height);
        visible.resize_with(height, || Row::new(width));
        let discarded = all.len().saturating_sub(self.scrollback_limit);
        self.first_line += discarded as u64;
        self.scrollback = all.into_iter().skip(discarded).collect();
        let anchor = old_history.saturating_sub(self.viewport_offset);
        self.viewport_offset = if self.viewport_offset == 0 {
            0
        } else {
            start.saturating_sub(anchor).min(self.scrollback.len())
        };
        self.rows = visible;
        self.base_row = 0;
        self.width = width;
        self.height = height;
        self.cursor.row = cursor_index.saturating_sub(start).min(height - 1);
        true
    }

    fn resize_reflow(&mut self, new_width: usize, new_height: usize) -> Result<(), GridError> {
        let old_viewport_offset = self.viewport_offset;
        let old_cursor_row = self.cursor.row;
        // Borrow the old rows until the replacement is complete. Cloning the
        // history here duplicates every cell buffer and grapheme per zoom.
        let mut source: Vec<&Row> = self.scrollback.iter().collect();
        source.extend((0..self.height).map(|row| self.row(row).expect("visible row")));
        let cursor_source_row = self.scrollback.len() + old_cursor_row;
        // Empty rows below the cursor are spare screen space, not history.
        // Drop them before choosing the new viewport so shrinking a mostly
        // empty window does not scroll the prompt up unnecessarily.
        while source.len() > cursor_source_row + 1
            && source.last().is_some_and(|row| {
                !row.wrapped && row.cells.iter().all(|cell| *cell == Cell::EMPTY)
            })
        {
            source.pop();
        }
        let viewport_source_row = self.scrollback.len().saturating_sub(old_viewport_offset);
        let mut reflowed = Vec::new();
        let mut new_cursor = None;
        let mut new_viewport_row = None;
        // Retain only used lengths, not a second copy of every cell and its
        // borrowed metadata. Reuse this scratch allocation across logical lines.
        let mut lengths = Vec::new();
        let mut group_start = 0;
        while group_start < source.len() {
            let mut group_end = group_start + 1;
            while group_end < source.len() && source[group_end].wrapped {
                group_end += 1;
            }
            lengths.clear();
            let mut logical_len = 0;
            let mut cursor_position = None;
            let mut viewport_position = None;
            for (index, row) in source.iter().enumerate().take(group_end).skip(group_start) {
                if old_viewport_offset != 0 && index == viewport_source_row {
                    viewport_position = Some(logical_len);
                }
                if index == cursor_source_row {
                    cursor_position = Some(logical_len + self.cursor.column);
                }
                let continues = index + 1 < group_end;
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
                // Structural padding before a wrapped wide cell is not text.
                if continues
                    && used == self.width
                    && row.cells[used - 1] == Cell::EMPTY
                    && source[index + 1].cells[0].is_wide_leading()
                {
                    used -= 1;
                }
                if used > 0 && row.cells[used - 1].is_wide_leading() {
                    used = (used + 1).min(self.width);
                }
                lengths.push(used);
                logical_len += used;
            }
            let mut line = source[group_start..group_end]
                .iter()
                .zip(&lengths)
                .flat_map(|(row, &used)| {
                    (0..used).map(move |column| {
                        (
                            row.cells[column],
                            row.graphemes.get(column).map(String::as_str),
                            row.hyperlinks.get(column).copied(),
                            row.underline_colors.get(column).copied(),
                        )
                    })
                })
                .peekable();
            let mut offset = 0;
            let mut continuation = false;
            if logical_len == 0 {
                reflowed.push(Row::new(new_width));
                if viewport_position == Some(0) {
                    new_viewport_row = Some(reflowed.len() - 1);
                }
                if cursor_position == Some(0) {
                    new_cursor = Some((reflowed.len() - 1, 0));
                }
            }
            while offset < logical_len {
                let row_start = offset;
                let mut row = Row::new(new_width);
                row.wrapped = continuation;
                let row_index = reflowed.len();
                let mut column = 0;
                while offset < logical_len && column < new_width {
                    let (cell, grapheme, hyperlink, underline_color) =
                        *line.peek().expect("remaining logical cell");
                    if cell.is_wide_leading() {
                        if new_width == 1 {
                            row.cells[0] = Cell::new(
                                '\u{fffd}' as u32,
                                cell.fg,
                                cell.bg,
                                cell.flags().without_wide(),
                            );
                            row.graphemes.set(0, Some("\u{fffd}".to_owned()));
                            row.underline_colors.set(0, underline_color);
                            if cursor_position
                                .is_some_and(|pos| (offset..offset + 2).contains(&pos))
                            {
                                new_cursor = Some((row_index, 0));
                            }
                            line.next();
                            line.next();
                            offset += 2;
                            column = 1;
                            continue;
                        }
                        if column + 1 >= new_width {
                            break;
                        }
                        row.cells[column] = cell;
                        row.graphemes.set(column, grapheme.map(str::to_owned));
                        row.hyperlinks.set(column, hyperlink);
                        row.underline_colors.set(column, underline_color);
                        line.next();
                        if let Some((spacer, _, spacer_link, spacer_color)) = line.next() {
                            row.cells[column + 1] = spacer;
                            row.hyperlinks.set(column + 1, spacer_link);
                            row.underline_colors.set(column + 1, spacer_color);
                        }
                        if let Some(pos) = cursor_position
                            && (offset..offset + 2).contains(&pos)
                        {
                            new_cursor = Some((row_index, column + pos - offset));
                        }
                        offset += 2;
                        column += 2;
                    } else {
                        row.cells[column] = cell;
                        row.graphemes.set(column, grapheme.map(str::to_owned));
                        row.hyperlinks.set(column, hyperlink);
                        row.underline_colors.set(column, underline_color);
                        if cursor_position == Some(offset) {
                            new_cursor = Some((row_index, column));
                        }
                        line.next();
                        offset += 1;
                        column += 1;
                    }
                }
                if offset < logical_len && column == 0 {
                    let (cell, grapheme, hyperlink, underline_color) =
                        *line.peek().expect("remaining logical cell");
                    row.cells[0] = cell;
                    row.graphemes.set(0, grapheme.map(str::to_owned));
                    row.hyperlinks.set(0, hyperlink);
                    row.underline_colors.set(0, underline_color);
                    line.next();
                    offset += 1;
                }
                if viewport_position.is_some_and(|pos| (row_start..offset).contains(&pos)) {
                    new_viewport_row = Some(row_index);
                }
                reflowed.push(row);
                continuation = true;
            }
            group_start = group_end;
        }
        let desired_start = reflowed.len().saturating_sub(new_height);
        let start = new_cursor.map_or(desired_start, |(row, _)| desired_start.min(row));
        let mut scrollback: VecDeque<Row> = reflowed.drain(..start).collect();
        // Reflow rebuilds the buffer, so line numbers are only guaranteed
        // monotonic: advance the origin by whatever the new limit discards.
        self.first_line += scrollback.len().saturating_sub(self.scrollback_limit) as u64;
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
        // Anchor the same logical content, not the old number of physical
        // rows: wrapping changes that distance whenever the width changes.
        self.viewport_offset = new_viewport_row
            .map_or(old_viewport_offset, |row| start.saturating_sub(row))
            .min(self.scrollback.len());
        self.cursor.column = cursor_column;
        self.cursor.row = cursor_row.min(new_height - 1);
        self.mark_all_dirty();
        Ok(())
    }

    fn resize_fixed(&mut self, width: usize, height: usize) -> Result<(), GridError> {
        validate_dimensions(width, height)?;
        if (width, height) == (self.width, self.height) {
            return Ok(());
        }
        let mut rows: Vec<Row> = (0..height).map(|_| Row::new(width)).collect();
        for (index, target) in rows.iter_mut().enumerate().take(self.height) {
            let source = self.row(index).expect("existing visible row");
            let count = width.min(self.width);
            target.cells[..count].copy_from_slice(&source.cells[..count]);
            target.graphemes = source.graphemes.clone_prefix(count);
            target.hyperlinks = source.hyperlinks.clone_prefix(count);
            target.underline_colors = source.underline_colors.clone_prefix(count);
            repair_wide_pairs(&mut target.cells);
            for column in 0..width {
                if target.cells[column].content == 0 && !target.cells[column].is_wide_spacer() {
                    target.graphemes.set(column, None);
                    target.hyperlinks.set(column, None);
                    target.underline_colors.set(column, None);
                }
            }
        }
        self.rows = rows;
        self.base_row = 0;
        self.width = width;
        self.height = height;
        self.cursor = Cursor {
            column: self.cursor.column.min(width - 1),
            row: self.cursor.row.min(height - 1),
        };
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn assert_sidecar_ownership(&self, links: &std::collections::HashMap<u32, String>) {
        for row in self.rows.iter().chain(&self.scrollback) {
            for (column, cell) in row.cells.iter().enumerate() {
                if cell.content == 0 && !cell.is_wide_spacer() {
                    assert!(row.graphemes.get(column).is_none(), "orphan grapheme");
                    assert!(row.hyperlinks.get(column).is_none(), "orphan hyperlink");
                    assert!(
                        row.underline_colors.get(column).is_none(),
                        "orphan underline color"
                    );
                }
                if let Some(id) = row.hyperlinks.get(column) {
                    assert!(links.contains_key(id), "unknown hyperlink id");
                }
                if cell.is_wide_leading() && column + 1 < self.width {
                    assert_eq!(
                        row.hyperlinks.get(column).copied(),
                        row.hyperlinks.get(column + 1).copied(),
                        "split wide hyperlink"
                    );
                    assert_eq!(
                        row.underline_colors.get(column),
                        row.underline_colors.get(column + 1),
                        "split wide underline color"
                    );
                    assert_eq!(
                        (cell.fg, cell.bg),
                        (row.cells[column + 1].fg, row.cells[column + 1].bg),
                        "split wide rendition"
                    );
                }
            }
        }
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

fn clear_orphan_graphemes(cells: &[Cell], graphemes: &mut Sidecar<String>) {
    for (cell, text) in cells.iter().zip(&mut graphemes.0) {
        if cell.content == 0 {
            *text = None;
        }
    }
    graphemes.trim();
}

fn clear_orphan_metadata(cells: &[Cell], metadata: &mut Sidecar<u32>) {
    for (cell, link) in cells.iter().zip(&mut metadata.0) {
        // Wide spacers have no scalar but still belong to the leading
        // cell's metadata. Only a genuinely empty cell loses ownership.
        if cell.content == 0 && !cell.is_wide_spacer() {
            *link = None;
        }
    }
    metadata.trim();
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
        self.alternate.resize_fixed(width, height)
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
    #[test]
    fn recording_scroll_moves_rows_and_matches_cloning_reference() {
        for limit in [0, 1, 2, 4, 16] {
            for amount in 0..=5 {
                let mut grid = Grid::new(8, 4).unwrap();
                grid.set_scrollback_limit(limit);
                for round in 0..12 {
                    for row in 0..4 {
                        grid.set_grapheme(row, 1, "e\u{301}", Cell::EMPTY);
                        grid.set_wide_cell(
                            row,
                            3,
                            Cell::new('界' as u32, round, 4, CellFlags::default()),
                        );
                        grid.set_hyperlink(row, 3, Some(round));
                        grid.set_underline_color(row, 3, Some(round));
                    }
                    grid.set_viewport_offset(round as usize);
                    let retained = grid.row(0).unwrap().cells.as_ptr();
                    let mut expected = grid.clone();
                    let count = amount.min(expected.height);
                    for row in 0..count {
                        expected
                            .scrollback
                            .push_back(expected.row(row).unwrap().clone());
                        if expected.scrollback.len() > limit {
                            expected.scrollback.pop_front();
                            expected.first_line += 1;
                            expected.viewport_offset = expected.viewport_offset.saturating_sub(1);
                        }
                    }
                    if expected.viewport_offset != 0 {
                        expected.viewport_offset =
                            (expected.viewport_offset + count).min(expected.scrollback.len());
                    }
                    expected.scroll_up(0, 3, count);
                    grid.scroll_up_recording(amount);
                    assert_eq!(
                        grid, expected,
                        "limit={limit}, amount={amount}, round={round}"
                    );
                    if count != 0 && count <= limit {
                        assert_eq!(
                            grid.scrollback[grid.scrollback.len() - count]
                                .cells
                                .as_ptr(),
                            retained
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn logical_line_reflow_preserves_blank_lines_cursor_and_viewport() {
        let mut terminal = crate::Terminal::new(6, 3).unwrap();
        terminal.set_scrollback_limit(64);
        terminal.process(b"abcdefgh\r\n\r\nijklmnop\r\nqrstuvwx");
        terminal.scroll_view_up(3);
        terminal.resize(4, 3).unwrap();
        fn rows(grid: &Grid) -> (Vec<String>, Vec<bool>) {
            let all = grid
                .scrollback
                .iter()
                .chain((0..grid.height).map(|i| grid.row(i).unwrap()));
            all.map(|row| {
                (
                    (0..row.cells.len())
                        .filter_map(|i| row.grapheme_at(i))
                        .collect(),
                    row.wrapped,
                )
            })
            .unzip()
        }
        assert_eq!(
            rows(terminal.active_grid()),
            (
                ["abcd", "efgh", "", "ijkl", "mnop", "qrst", "uvwx", ""]
                    .map(String::from)
                    .to_vec(),
                vec![false, true, false, false, true, false, true, true]
            )
        );
        assert_eq!(terminal.cursor(), Cursor { column: 0, row: 2 });
        assert_eq!(terminal.viewport_offset(), 4);
        terminal.resize(10, 4).unwrap();
        assert_eq!(
            rows(terminal.active_grid()).0,
            ["abcdefgh", "", "ijklmnop", "qrstuvwx"]
        );
        assert_eq!(terminal.cursor(), Cursor { column: 8, row: 3 });
        assert_eq!(terminal.viewport_offset(), 0);
        assert!(terminal.active_grid().scrollback.is_empty());
    }

    #[test]
    fn continuous_logical_line_keeps_graphemes_and_wide_links() {
        let mut terminal = crate::Terminal::new(8, 3).unwrap();
        terminal.set_scrollback_limit(128);
        let input = "ab界e\u{301}".repeat(40);
        terminal.process(b"\x1b]8;;https://example.org\x1b\\");
        terminal.process(input.as_bytes());
        let link = terminal.active_grid().hyperlink(0, 0).unwrap();
        for width in [8, 5, 7, 3, 11, 8] {
            terminal.resize(width, 3).unwrap();
            let grid = terminal.active_grid();
            let mut text = String::new();
            for row in grid
                .scrollback
                .iter()
                .chain((0..grid.height).map(|i| grid.row(i).unwrap()))
            {
                for (column, cell) in row.cells.iter().enumerate() {
                    if cell.content != 0 || cell.is_wide_spacer() {
                        assert_eq!(row.hyperlinks.get(column), Some(&link));
                    }
                    if cell.is_wide_leading() {
                        assert!(row.cells[column + 1].is_wide_spacer());
                        assert_eq!(row.hyperlinks.get(column + 1), Some(&link));
                    }
                    if cell.is_wide_spacer() {
                        assert!(column > 0 && row.cells[column - 1].is_wide_leading());
                    }
                    if let Some(grapheme) = row.grapheme_at(column) {
                        text.push_str(&grapheme);
                    }
                }
            }
            assert_eq!(text, input, "width={width}");
        }
    }

    #[test]
    fn clearing_the_last_underline_color_releases_storage() {
        let mut grid = Grid::new(8, 2).unwrap();
        grid.set_cell(0, 7, Cell::new('X' as u32, 0, 0, CellFlags::UNDERLINE));
        grid.set_underline_color(0, 7, Some(0x02010203));
        assert!(grid.rows[0].underline_colors.0.capacity() > 0);
        grid.clear_cell(0, 7);
        assert_eq!(grid.rows[0].underline_colors.0.capacity(), 0);
        grid.set_cell(0, 7, Cell::new('X' as u32, 0, 0, CellFlags::UNDERLINE));
        grid.set_underline_color(0, 7, Some(0x02010203));
        grid.resize_fixed(7, 2).unwrap();
        assert_eq!(grid.rows[0].underline_colors.0.capacity(), 0);
        grid.set_wide_cell(0, 2, Cell::new('界' as u32, 0, 0, CellFlags::UNDERLINE));
        assert!(grid.set_underline_color(0, 2, Some(0x02010203)));
        assert!(!grid.set_underline_color(0, 3, None));
        assert_eq!(
            grid.rows[0].underline_color_at(2),
            grid.rows[0].underline_color_at(3)
        );
    }

    #[test]
    fn ascii_history_has_no_sidecar_allocations() {
        let mut terminal = crate::Terminal::new(8, 4).unwrap();
        terminal.set_scrollback_limit(64);
        terminal.process(b"abc\r\n".repeat(70).as_slice());
        let grid = terminal.active_grid();
        assert_eq!(grid.scrollback.len(), 64);
        for row in grid.scrollback.iter().chain(&grid.rows) {
            assert_eq!(row.graphemes.0.capacity(), 0);
            assert_eq!(row.hyperlinks.0.capacity(), 0);
            assert_eq!(row.underline_colors.0.capacity(), 0);
        }
    }

    #[test]
    fn sidecar_releases_last_value() {
        let mut values = Sidecar::new();
        values.set(7, Some(String::from("e\u{301}")));
        values.set(7, None);
        assert_eq!(values.0.capacity(), 0);
        values.set(7, Some(String::from("x")));
        values.truncate(7);
        assert_eq!(values.0.capacity(), 0);
        values.set(7, Some(String::from("x")));
        values.clear_range(0..8);
        assert_eq!(values.0.capacity(), 0);
    }

    #[test]
    fn sidecar_operations_match_dense_model() {
        use proptest::prelude::*;
        use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
        let mut runner = TestRunner::new_with_rng(
            Config {
                cases: 1024,
                failure_persistence: None,
                ..Config::default()
            },
            TestRng::from_seed(RngAlgorithm::ChaCha, &[0xa7; 32]),
        );
        let operations =
            proptest::collection::vec((0u8..7, 0usize..32, 0usize..33, any::<bool>()), 1..128);
        runner
            .run(&operations, |operations| {
                let mut sparse = Sidecar::new();
                let mut dense = vec![None; 32];
                for (op, at, count, present) in operations {
                    let amount = count.min(32 - at);
                    match op {
                        0 => {
                            let v = present.then(|| format!("value-{at}"));
                            sparse.set(at, v.clone());
                            dense[at] = v;
                        }
                        1 => {
                            sparse.clear_range(at..at + amount);
                            dense[at..at + amount].fill(None);
                        }
                        2 => {
                            sparse.insert(at, amount, 32);
                            for i in (at..32 - amount).rev() {
                                dense[i + amount] = dense[i].take();
                            }
                            dense[at..at + amount].fill(None);
                        }
                        3 => {
                            sparse.delete(at, amount);
                            dense.drain(at..at + amount);
                            dense.resize(32, None);
                        }
                        4 => {
                            sparse.truncate(at);
                            dense[at..].fill(None);
                        }
                        5 => {
                            sparse = sparse.clone_prefix(at);
                            dense[at..].fill(None);
                        }
                        _ => {
                            sparse.clear();
                            dense.fill(None);
                        }
                    }
                    for (i, value) in dense.iter().enumerate() {
                        prop_assert_eq!(sparse.get(i), value.as_ref());
                    }
                    prop_assert!(sparse.0.last().is_none_or(Option::is_some));
                    prop_assert_eq!(
                        sparse.is_clear_from(at),
                        dense[at..].iter().all(Option::is_none)
                    );
                    if dense.iter().all(Option::is_none) {
                        prop_assert_eq!(sparse.0.capacity(), 0);
                    }
                }
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn fitting_height_resize_keeps_owned_row_buffers() {
        let mut terminal = crate::Terminal::new(8, 4).unwrap();
        terminal.set_scrollback_limit(64);
        terminal.process(b"a\r\nb\r\nc\r\nd\r\ne\r\nf\r\n");
        let grid = terminal.active_grid();
        let buffers: Vec<_> = grid
            .scrollback
            .iter()
            .chain((0..grid.height).map(|i| grid.row(i).unwrap()))
            .filter(|row| row.cells.iter().any(|cell| *cell != Cell::EMPTY))
            .map(|row| row.cells.as_ptr())
            .collect();
        terminal.resize(8, 3).unwrap();
        let grid = terminal.active_grid();
        let retained: Vec<_> = grid
            .scrollback
            .iter()
            .chain(&grid.rows)
            .filter(|row| row.cells.iter().any(|cell| *cell != Cell::EMPTY))
            .map(|row| row.cells.as_ptr())
            .collect();
        assert_eq!(buffers, retained);
    }

    #[test]
    fn fitting_row_resize_matches_full_reflow() {
        let samples = [
            "a\r\nb\r\n",
            "e\u{301}界\r\n",
            "\x1b[44mabc\x1b[K\r\n",
            "long logical line that wraps ",
            "\x1b]8;;https://example.org\x1b\\界e\u{301}\r\n",
        ];
        for width in [1, 2, 3, 8, 80] {
            for height in [1, 3] {
                for history in [0, 4] {
                    for sample in samples {
                        for clear_below in [false, true] {
                            let mut terminal = crate::Terminal::new(width, height).unwrap();
                            terminal.set_scrollback_limit(history);
                            terminal.process(sample.repeat(10).as_bytes());
                            if clear_below {
                                terminal.process(b"\x1b[1;1H\x1b[J");
                            }
                            terminal.scroll_view_up(2);
                            for new_width in [1, 2, 3, 7, 8, 79, 80, 90] {
                                for new_height in [1, 2, 3, height, height + 1, height + 3] {
                                    // Identical geometry intentionally preserves
                                    // pending wrap and is not a reflow operation.
                                    if (new_width, new_height) == (width, height) {
                                        continue;
                                    }
                                    let mut actual = terminal.active_grid().clone();
                                    let mut reference = actual.clone();
                                    // The full path canonicalizes circular rows.
                                    // Compare logical storage, not its ring origin.
                                    actual.resize(new_width, new_height).unwrap();
                                    reference.resize_reflow(new_width, new_height).unwrap();
                                    actual.rows.rotate_left(actual.base_row);
                                    actual.base_row = 0;
                                    assert_eq!(
                                        actual, reference,
                                        "{width}x{height} -> {new_width}x{new_height}, history={history}, clear={clear_below}, sample={sample:?}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

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
