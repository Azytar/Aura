//! Window-system and renderer-independent terminal state.

mod grid;

pub use grid::{ActiveScreen, Cell, CellFlags, Cursor, Grid, GridError, Row, TerminalGrid};
