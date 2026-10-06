//! Window-system and renderer-independent terminal state.

mod grid;
mod parser;
mod pty;
mod terminal;

pub use grid::{ActiveScreen, Cell, CellFlags, Cursor, Grid, GridError, Row, TerminalGrid};
pub use parser::{Csi, Event as ParserEvent, Parser};
pub use pty::{Pty, PtyError, PtySize};
pub use terminal::{
    DEFAULT_COLOR, INDEXED_COLOR_TAG, Modes, MouseTracking, RGB_COLOR_TAG, Terminal,
};
