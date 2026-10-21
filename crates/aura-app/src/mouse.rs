use crate::cell_position;
use aura_core::{MouseTracking, Terminal};
use aura_text::CellMetrics;
use x11rb::protocol::xproto::KeyButMask;

/// Shift always gives selection to the terminal, including in a mouse-aware TUI.
pub(crate) fn mouse_modes(terminal: &Terminal, state: KeyButMask) -> aura_core::Modes {
    let mut modes = terminal.modes();
    if state.contains(KeyButMask::SHIFT) || terminal.viewport_offset() != 0 {
        modes.mouse_tracking = MouseTracking::Off;
    }
    modes
}

#[derive(Clone, Copy)]
pub(crate) struct MouseEvent {
    pub button: u8,
    pub x: i16,
    pub y: i16,
    pub state: KeyButMask,
    pub released: bool,
    pub motion: bool,
}

pub(crate) fn mouse_report(
    modes: aura_core::Modes,
    event: MouseEvent,
    metrics: CellMetrics,
    padding: [u32; 2],
    grid: [usize; 2],
) -> Option<Vec<u8>> {
    let MouseEvent {
        button,
        x: pixel_x,
        y: pixel_y,
        state,
        released,
        motion,
    } = event;
    if modes.mouse_tracking == MouseTracking::Off
        || (released && button >= 4)
        || (motion
            && (modes.mouse_tracking == MouseTracking::Press
                || (modes.mouse_tracking == MouseTracking::Drag && button == 0)))
    {
        return None;
    }
    let mut code = match button {
        1 => 0u32,
        2 => 1,
        3 => 2,
        4 => 64,
        5 => 65,
        6 => 66,
        7 => 67,
        0 if motion && modes.mouse_tracking == MouseTracking::Motion => 3,
        _ => return None,
    };
    let sgr = modes.mouse_sgr || modes.mouse_pixel;
    if released && !sgr {
        code = 3;
    } else if motion {
        code += 32;
    }
    if state.contains(KeyButMask::SHIFT) {
        code += 4;
    }
    if state.contains(KeyButMask::MOD1) {
        code += 8;
    }
    if state.contains(KeyButMask::CONTROL) {
        code += 16;
    }
    let (column, row) = if modes.mouse_pixel {
        (
            (i64::from(pixel_x) - i64::from(padding[0])).max(0) as usize,
            (i64::from(pixel_y) - i64::from(padding[1])).max(0) as usize,
        )
    } else {
        cell_position(pixel_x, pixel_y, metrics, padding)
    };
    let bounds = if modes.mouse_pixel {
        [
            grid[0].saturating_mul(metrics.width as usize),
            grid[1].saturating_mul(metrics.height as usize),
        ]
    } else {
        grid
    };
    let x = column.min(bounds[0].saturating_sub(1)) + 1;
    let y = row.min(bounds[1].saturating_sub(1)) + 1;
    if sgr {
        Some(
            format!(
                "\x1b[<{code};{x};{}{suffix}",
                y,
                suffix = if released { 'm' } else { 'M' }
            )
            .into_bytes(),
        )
    } else if code <= 223 && x <= 223 && y <= 223 {
        Some(vec![
            0x1b,
            b'[',
            b'M',
            (code + 32) as u8,
            (x + 32) as u8,
            (y + 32) as u8,
        ])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hover_pixels_releases_and_selection_override() {
        let mut terminal = Terminal::new(10, 3).unwrap();
        terminal.process(b"\x1b[?1003;1006h");
        let metrics = CellMetrics {
            width: 8,
            height: 16,
            ascent: 12,
            descent: 3,
            line_gap: 1,
        };
        let hover = MouseEvent {
            button: 0,
            x: 22,
            y: 38,
            state: KeyButMask::default(),
            released: false,
            motion: true,
        };
        let outside = MouseEvent {
            x: i16::MAX,
            y: i16::MAX,
            ..hover
        };
        assert_eq!(
            mouse_report(terminal.modes(), hover, metrics, [4, 6], [10, 3]).unwrap(),
            b"\x1b[<35;3;3M"
        );
        assert_eq!(
            mouse_report(terminal.modes(), outside, metrics, [4, 6], [10, 3]).unwrap(),
            b"\x1b[<35;10;3M"
        );
        terminal.process(b"\x1b[?1016h");
        assert_eq!(
            mouse_report(terminal.modes(), hover, metrics, [4, 6], [10, 3]).unwrap(),
            b"\x1b[<35;19;33M"
        );
        assert_eq!(
            mouse_report(terminal.modes(), outside, metrics, [4, 6], [10, 3]).unwrap(),
            b"\x1b[<35;80;48M"
        );
        assert!(
            mouse_report(
                mouse_modes(&terminal, KeyButMask::SHIFT),
                hover,
                metrics,
                [4, 6],
                [10, 3]
            )
            .is_none()
        );
        terminal.process(b"\x1b[?1003l\x1b[?1000h");
        assert!(mouse_report(terminal.modes(), hover, metrics, [4, 6], [10, 3]).is_none());
        let release = MouseEvent {
            button: 1,
            released: true,
            motion: false,
            ..hover
        };
        assert_eq!(
            mouse_report(terminal.modes(), release, metrics, [4, 6], [10, 3]).unwrap(),
            b"\x1b[<0;19;33m"
        );
    }
}
