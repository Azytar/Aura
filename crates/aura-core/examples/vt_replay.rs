use std::fs::File;
use std::io::{self, Read};

use aura_core::{CellFlags, Terminal};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let source = args.next().unwrap_or_else(|| "-".to_owned());
    let columns = args.next().map_or(Ok(80), |value| value.parse())?;
    let rows = args.next().map_or(Ok(24), |value| value.parse())?;
    if args.next().is_some() {
        return Err("usage: vt_replay [input-file|-] [columns rows]".into());
    }
    let mut input = Vec::new();
    if source == "-" {
        io::stdin().read_to_end(&mut input)?;
    } else {
        File::open(source)?.read_to_end(&mut input)?;
    }
    let mut terminal = Terminal::new(columns, rows)
        .map_err(|error| io::Error::other(format!("terminal geometry: {error:?}")))?;
    let replies = terminal.process(&input);
    println!("Terminal {columns}x{rows} title={:?}", terminal.title());
    for row in 0..rows {
        let mut line = String::new();
        for column in 0..columns {
            let Some(cell) = terminal.viewport_cell(row, column) else {
                continue;
            };
            if cell.flags().contains(CellFlags::WIDE_SPACER) {
                continue;
            }
            if let Some(grapheme) = terminal.viewport_grapheme(row, column) {
                line.push_str(&grapheme);
            } else {
                line.push(' ');
            }
        }
        let wrapped = terminal
            .active_grid()
            .viewport_row(row)
            .is_some_and(|visible| visible.is_wrapped());
        println!("{}{}", if wrapped { "↪" } else { " " }, line);
    }
    let cursor = terminal.cursor();
    eprintln!(
        "cursor=({}, {}) alternate={} replies={:?}",
        cursor.column,
        cursor.row,
        terminal.is_alternate_screen(),
        String::from_utf8_lossy(&replies)
    );
    Ok(())
}
