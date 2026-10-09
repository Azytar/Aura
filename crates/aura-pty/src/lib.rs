//! Linux pseudoterminal ownership: allocation of the master/slave pair,
//! supervision of the child process, and propagation of window size.
//!
//! This crate is the only place that performs the `openpty`, `setsid`,
//! `TIOCSCTTY`, and `TIOCSWINSZ` calls. It deals in raw descriptors and
//! byte buffers; it never interprets the bytes the child writes, and it never
//! executes anything the child sends.
//!
//! # Lifecycle
//!
//! [`Pty::close`] and [`Drop`] both make sure the child is reaped: the master
//! descriptor is closed first (which hangs the session up), and if the child
//! is still alive its whole process group is killed. Closing the terminal must
//! never leave an orphaned shell behind.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};

pub type PtyError = io::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PtySize {
    pub columns: u16,
    pub rows: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

impl PtySize {
    pub fn new(columns: usize, rows: usize) -> io::Result<Self> {
        if columns == 0 || rows == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "PTY dimensions must be nonzero",
            ));
        }
        Ok(Self {
            columns: u16::try_from(columns).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "PTY width exceeds Linux limit")
            })?,
            rows: u16::try_from(rows).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "PTY height exceeds Linux limit",
                )
            })?,
            pixel_width: 0,
            pixel_height: 0,
        })
    }

    fn to_winsize(self) -> io::Result<libc::winsize> {
        if self.columns == 0 || self.rows == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "PTY dimensions must be nonzero",
            ));
        }
        Ok(libc::winsize {
            ws_row: self.rows,
            ws_col: self.columns,
            ws_xpixel: self.pixel_width,
            ws_ypixel: self.pixel_height,
        })
    }
}

/// A child process attached to a real Unix pseudoterminal master.
pub struct Pty {
    master: Option<File>,
    child: Child,
}

impl Pty {
    /// Starts the configured interactive shell, falling back to bash and then sh.
    pub fn spawn_shell(size: PtySize) -> io::Result<Self> {
        let shell = configured_shell();
        let name = shell.file_name().unwrap_or_else(|| OsStr::new("sh"));
        let login_name = format!("-{}", name.to_string_lossy());
        Self::spawn(&shell, &[], Some(&login_name), size)
    }

    /// Starts a command with stdin, stdout, and stderr attached to the PTY slave.
    pub fn spawn_command(program: &Path, args: &[&str], size: PtySize) -> io::Result<Self> {
        Self::spawn(program, args, None, size)
    }

    fn spawn(
        program: &Path,
        args: &[&str],
        argv0: Option<&str>,
        size: PtySize,
    ) -> io::Result<Self> {
        let winsize = size.to_winsize()?;
        let mut master_fd = -1;
        let mut slave_fd = -1;
        // A real PTY pair makes tty checks, job control, and window-size queries work in shells.
        let result = unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null(),
                &winsize,
            )
        };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        // Each successful openpty descriptor is now owned and closed on every error path.
        let master = unsafe { OwnedFd::from_raw_fd(master_fd) };
        let slave = unsafe { OwnedFd::from_raw_fd(slave_fd) };
        set_cloexec(master.as_raw_fd())?;
        set_cloexec(slave.as_raw_fd())?;
        set_nonblocking(master.as_raw_fd())?;

        let stdin = File::from(duplicate_fd(slave.as_raw_fd())?);
        let stdout = File::from(duplicate_fd(slave.as_raw_fd())?);
        let stderr = File::from(duplicate_fd(slave.as_raw_fd())?);
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::from(stdin))
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor");
        if let Some(argv0) = argv0 {
            command.arg0(argv0);
        }
        // The child only performs async-signal-safe syscalls before exec; fd 0 is
        // already the PTY slave because Command configured its stdio handles.
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn()?;
        drop(slave);
        Ok(Self {
            master: Some(File::from(master)),
            child,
        })
    }

    pub fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.master_ref()?.read(buffer)
    }

    /// Writes some bytes to the child. A nonblocking master may return `WouldBlock`.
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.master_ref()?.write(bytes)
    }

    pub fn resize(&self, size: PtySize) -> io::Result<()> {
        let winsize = size.to_winsize()?;
        // TIOCSWINSZ sends SIGWINCH to the foreground process group of this PTY.
        let result =
            unsafe { libc::ioctl(self.master_ref()?.as_raw_fd(), libc::TIOCSWINSZ, &winsize) };
        if result == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn master_fd(&self) -> io::Result<RawFd> {
        Ok(self.master_ref()?.as_raw_fd())
    }

    pub fn child_id(&self) -> u32 {
        self.child.id()
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child.wait()
    }

    /// Closes the PTY and terminates/reaps the child if it did not exit on hangup.
    pub fn close(&mut self) -> io::Result<ExitStatus> {
        self.master.take();
        if let Some(status) = self.child.try_wait()? {
            return Ok(status);
        }
        let process_group = -(self.child.id() as libc::pid_t);
        // The child is a session leader, so its process group owns all PTY jobs.
        let result = unsafe { libc::kill(process_group, libc::SIGKILL) };
        if result == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        self.child.wait()
    }

    fn master_ref(&self) -> io::Result<&File> {
        self.master
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "PTY master is closed"))
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.master.take();
        if self.child.try_wait().ok().flatten().is_none() {
            let process_group = -(self.child.id() as libc::pid_t);
            // Closing the master sends hangup; kill the remaining session to avoid orphaned jobs.
            let _ = unsafe { libc::kill(process_group, libc::SIGKILL) };
            let _ = self.child.wait();
        }
    }
}

fn duplicate_fd(fd: i32) -> io::Result<OwnedFd> {
    // F_DUPFD_CLOEXEC prevents inherited slave descriptors from keeping the PTY open.
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate == -1 {
        Err(io::Error::last_os_error())
    } else {
        // fcntl returned a new descriptor owned by this call.
        Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
    }
}

fn set_cloexec(fd: i32) -> io::Result<()> {
    // Preserve descriptor flags while ensuring children cannot inherit these handles.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn set_nonblocking(fd: i32) -> io::Result<()> {
    // Event loops need reads to return promptly when the PTY has been drained.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn configured_shell() -> PathBuf {
    if let Some(shell) = std::env::var_os("SHELL") {
        let shell = PathBuf::from(shell);
        if executable(&shell) {
            return shell;
        }
    }
    [Path::new("/bin/bash"), Path::new("/bin/sh")]
        .into_iter()
        .find(|path| executable(path))
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/bin/sh"))
}

fn executable(path: &Path) -> bool {
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // Reject unusable SHELL values before asking Command to exec them.
    unsafe { libc::access(path.as_ptr(), libc::X_OK) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pty_size_rejects_zero_and_values_outside_linux_winsize() {
        assert!(PtySize::new(0, 24).is_err());
        assert!(PtySize::new(80, 0).is_err());
        assert!(PtySize::new(usize::MAX, 24).is_err());
        assert_eq!(PtySize::new(80, 24).unwrap().columns, 80);
        assert!(
            PtySize {
                columns: 0,
                rows: 24,
                pixel_width: 0,
                pixel_height: 0,
            }
            .to_winsize()
            .is_err()
        );
    }
}
