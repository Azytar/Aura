use std::collections::VecDeque;
use std::io;

const MAX_PENDING_BYTES: usize = 1024 * 1024;

#[derive(Default)]
pub struct Input {
    bytes: VecDeque<u8>,
}

impl Input {
    pub fn push(&mut self, bytes: &[u8]) {
        // Reject a whole input atom rather than cutting a UTF-8 or VT sequence.
        if bytes.len() > MAX_PENDING_BYTES - self.bytes.len() {
            eprintln!("aura: PTY input buffer full; input ignored");
        } else {
            self.bytes.extend(bytes);
        }
    }

    pub fn pending(&self) -> bool {
        !self.bytes.is_empty()
    }

    pub fn flush(&mut self, mut write: impl FnMut(&[u8]) -> io::Result<usize>) -> io::Result<()> {
        // Bound syscalls, including EINTR retries, so signals or tiny writes
        // cannot starve X11 events. Pending bytes keep EPOLLOUT armed.
        for _ in 0..64 {
            if self.bytes.is_empty() {
                break;
            }
            let (first, _) = self.bytes.as_slices();
            match write(first) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "PTY accepted no input",
                    ));
                }
                Ok(count) => {
                    self.bytes.drain(..count);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiny_successful_writes_also_yield_and_resume_in_order() {
        let mut input = Input::default();
        let payload = "é中".repeat(500).into_bytes();
        input.push(&payload);
        let mut output = Vec::new();
        input
            .flush(|bytes| {
                output.push(bytes[0]);
                Ok(1)
            })
            .unwrap();
        assert!(
            input.pending(),
            "one wake must not drain an arbitrarily slow writer"
        );
        while input.pending() {
            input
                .flush(|bytes| {
                    output.push(bytes[0]);
                    Ok(1)
                })
                .unwrap();
        }
        assert_eq!(output, payload);
    }

    #[test]
    fn repeated_interruptions_yield_without_dropping_input() {
        let mut input = Input::default();
        input.push(b"keep");
        let mut calls = 0;
        input
            .flush(|_| {
                calls += 1;
                assert!(
                    calls <= 64,
                    "flush starved the event loop on repeated EINTR"
                );
                Err(io::ErrorKind::Interrupted.into())
            })
            .unwrap();
        assert!(input.pending());
        let mut output = Vec::new();
        input
            .flush(|bytes| {
                output.extend_from_slice(bytes);
                Ok(bytes.len())
            })
            .unwrap();
        assert_eq!(output, b"keep");
    }

    #[test]
    fn fatal_write_errors_preserve_only_the_unaccepted_suffix() {
        for kind in [io::ErrorKind::WriteZero, io::ErrorKind::BrokenPipe] {
            let mut input = Input::default();
            input.push("aéZ".as_bytes());
            let mut output = Vec::new();
            let mut calls = 0;
            let error = input
                .flush(|bytes| {
                    calls += 1;
                    if calls == 1 {
                        output.extend_from_slice(&bytes[..2]);
                        Ok(2)
                    } else if kind == io::ErrorKind::WriteZero {
                        Ok(0)
                    } else {
                        Err(kind.into())
                    }
                })
                .unwrap_err();
            assert_eq!(error.kind(), kind);
            assert_eq!(calls, 2);
            input
                .flush(|bytes| {
                    output.extend_from_slice(bytes);
                    Ok(bytes.len())
                })
                .unwrap();
            assert_eq!(output, "aéZ".as_bytes());
            assert!(!input.pending());
        }
    }

    #[test]
    fn a_real_nonblocking_pty_keeps_a_large_utf8_paste_under_backpressure() {
        use std::path::Path;
        use std::time::{Duration, Instant};
        let payload = format!("\x1b[200~{}\x1b[201~", "aé中\r\n".repeat(16_384)).into_bytes();
        let command = format!(
            "stty raw -echo; printf READY; sleep 0.2; head -c {}",
            payload.len()
        );
        let mut pty = aura_pty::Pty::spawn_command(
            Path::new("/bin/sh"),
            &["-c", &command],
            aura_pty::PtySize::new(80, 24).unwrap(),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut buffer = [0u8; 16_384];
        let mut output = Vec::new();
        while output.len() < 5 && Instant::now() < deadline {
            if let Ok(count) = pty.read(&mut buffer) {
                output.extend_from_slice(&buffer[..count]);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(output, b"READY");
        output.clear();
        let mut input = Input::default();
        input.push(&payload);
        input.flush(|bytes| pty.write(bytes)).unwrap();
        assert!(
            input.pending(),
            "the sleeping reader must exert backpressure"
        );
        while output.len() < payload.len() && Instant::now() < deadline {
            input.flush(|bytes| pty.write(bytes)).unwrap();
            match pty.read(&mut buffer) {
                Ok(count) => output.extend_from_slice(&buffer[..count]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                other => panic!("PTY closed before paste was received: {other:?}"),
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(output, payload);
        assert!(!input.pending());
        assert!(pty.wait().unwrap().success());
    }

    #[test]
    fn partial_writes_and_backpressure_preserve_every_byte_in_order() {
        let mut input = Input::default();
        input.push("aé\x1b[200~paste".as_bytes());
        let mut output = Vec::new();
        let mut calls = 0;
        input
            .flush(|bytes| {
                calls += 1;
                match calls {
                    1 => Err(io::ErrorKind::Interrupted.into()),
                    2 => {
                        output.extend_from_slice(&bytes[..2]);
                        Ok(2)
                    }
                    _ => Err(io::ErrorKind::WouldBlock.into()),
                }
            })
            .unwrap();
        assert!(input.pending());
        input.push(b"\x1b[201~");
        input
            .flush(|bytes| {
                output.extend_from_slice(bytes);
                Ok(bytes.len())
            })
            .unwrap();
        assert_eq!(output, "aé\x1b[200~paste\x1b[201~".as_bytes());
        assert!(!input.pending());
    }

    #[test]
    fn input_is_bounded_and_zero_writes_are_errors() {
        let mut input = Input::default();
        input.push(b"keep");
        input.push(&vec![0; MAX_PENDING_BYTES]);
        assert_eq!(input.bytes.len(), 4);
        assert_eq!(
            input.flush(|_| Ok(0)).unwrap_err().kind(),
            io::ErrorKind::WriteZero
        );
        // A zero write is fatal, but no unaccepted bytes are removed. Only
        // WouldBlock indicates backpressure; retrying WriteZero on a writable
        // descriptor would keep waking the event loop without progress.
        assert_eq!(input.bytes.len(), 4);
        assert!(input.pending());
    }
}
