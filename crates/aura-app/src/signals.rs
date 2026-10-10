use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

pub struct Termination {
    fd: OwnedFd,
    previous: libc::sigset_t,
}

impl Termination {
    pub fn new() -> io::Result<Self> {
        // SAFETY: both sets are writable stack storage. Block before creating
        // Vulkan threads so all threads route termination to this signalfd.
        let (fd, previous) = unsafe {
            let mut mask = std::mem::zeroed();
            let mut previous = std::mem::zeroed();
            libc::sigemptyset(&mut mask);
            for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
                libc::sigaddset(&mut mask, signal);
            }
            let error = libc::pthread_sigmask(libc::SIG_BLOCK, &mask, &mut previous);
            if error != 0 {
                return Err(io::Error::from_raw_os_error(error));
            }
            let fd = libc::signalfd(-1, &mask, libc::SFD_CLOEXEC | libc::SFD_NONBLOCK);
            if fd == -1 {
                let error = io::Error::last_os_error();
                libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut());
                return Err(error);
            }
            (OwnedFd::from_raw_fd(fd), previous)
        };
        Ok(Self { fd, previous })
    }

    pub fn fd(&self) -> i32 {
        self.fd.as_raw_fd()
    }
}

impl Drop for Termination {
    fn drop(&mut self) {
        // SAFETY: previous is the calling thread's saved mask. Consume pending
        // termination before unblocking it, after the PTY child has been reaped.
        unsafe {
            let mut info: libc::signalfd_siginfo = std::mem::zeroed();
            while libc::read(
                self.fd(),
                (&mut info as *mut libc::signalfd_siginfo).cast(),
                size_of::<libc::signalfd_siginfo>(),
            ) > 0
            {}
            libc::pthread_sigmask(libc::SIG_SETMASK, &self.previous, std::ptr::null_mut());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    #[test]
    fn the_shell_does_not_inherit_the_event_loops_blocked_signals() {
        let _termination = Termination::new().unwrap();
        let mut pty = aura_pty::Pty::spawn_command(
            std::path::Path::new("/bin/sh"),
            &["-c", "kill -INT $$; exit 99"],
            aura_pty::PtySize::new(10, 2).unwrap(),
        )
        .unwrap();
        assert_eq!(pty.wait().unwrap().signal(), Some(libc::SIGINT));
    }
}
