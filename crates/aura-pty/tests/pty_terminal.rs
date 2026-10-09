use std::io;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use aura_core::Terminal;
use aura_pty::{Pty, PtySize};

#[test]
fn real_pty_output_and_input_reach_terminal_state() {
    let size = PtySize::new(40, 5).unwrap();
    let mut pty = Pty::spawn_command(
        Path::new("/bin/sh"),
        &[
            "-c",
            "printf '\\033[31mPTY_OUT\\033[0m\\n'; read line; stty size; printf '\\033[32m%s\\033[0m\\n' \"$line\"",
        ],
        size,
    )
    .expect("the Linux PTY should spawn /bin/sh");
    let mut terminal = Terminal::new(size.columns as usize, size.rows as usize).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut sent_input = false;
    let mut buffer = [0_u8; 1024];

    while Instant::now() < deadline {
        match pty.read(&mut buffer) {
            Ok(length) if length > 0 => {
                let replies = terminal.process(&buffer[..length]);
                if !replies.is_empty() {
                    let _ = pty.write(&replies);
                }
            }
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if !sent_input && row_text(&terminal, 0).starts_with("PTY_OUT") {
                    let new_size = PtySize::new(44, 5).unwrap();
                    pty.resize(new_size).unwrap();
                    terminal
                        .resize(new_size.columns as usize, new_size.rows as usize)
                        .unwrap();
                    pty.write(b"PTY_IN\n")
                        .expect("small PTY input fits in one write");
                    sent_input = true;
                }
                if pty.try_wait().unwrap().is_some() && sent_input {
                    match pty.read(&mut buffer) {
                        Ok(length) if length > 0 => {
                            terminal.process(&buffer[..length]);
                            continue;
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                        Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                        _ => {}
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
            Err(error) => panic!("PTY read failed: {error}"),
        }
        if sent_input && row_text(&terminal, 3).starts_with("PTY_IN") {
            break;
        }
    }

    assert!(
        sent_input,
        "shell did not produce its initial marker in time"
    );
    assert!(row_text(&terminal, 0).starts_with("PTY_OUT"));
    assert!(row_text(&terminal, 2).starts_with("5 44"));
    assert!(row_text(&terminal, 3).starts_with("PTY_IN"));
    assert_eq!(
        terminal.active_grid().cell(3, 0).unwrap().fg,
        0x0100_0000 | 2
    );
    assert!(pty.wait().unwrap().success());
    assert!(pty.close().unwrap().success());
}

fn row_text(terminal: &Terminal, row: usize) -> String {
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
