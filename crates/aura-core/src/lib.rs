//! Window-system and renderer-independent terminal state.

mod grid;
mod parser;
mod terminal;

pub use grid::{ActiveScreen, Cell, CellFlags, Cursor, Grid, GridError, Row, TerminalGrid};
pub use parser::{Csi, Event as ParserEvent, Parser};
pub use terminal::{
    CursorStyle, DEFAULT_COLOR, INDEXED_COLOR_TAG, Modes, MouseTracking, Osc52Write, RGB_COLOR_TAG,
    SelectionPoint, ShellMarker, Terminal,
};
