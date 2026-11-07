//! Optional, bounded background PTY reads. The UI alone parses and modifies
//! terminal state; this thread only reads bytes while the UI might render.
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

pub const READ_CHUNK_BYTES: usize = 64 * 1024;
pub const DRAIN_BUDGET_BYTES: usize = 256 * 1024;
const MAX_PENDING_CHUNKS: usize = 16;

pub enum ReadEvent {
    Bytes(Vec<u8>),
    Closed,
    Error(io::Error),
}

pub struct Reader {
    receiver: Option<Receiver<ReadEvent>>,
    wake: OwnedFd,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Reader {
    /// The supplied descriptor must be nonblocking. `Pty::clone_reader`
    /// provides a duplicate of AURA's nonblocking PTY master.
    pub fn start(file: File) -> io::Result<Self> {
        // SAFETY: F_GETFL only reads descriptor flags and does not borrow memory.
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        if flags == -1 {
            return Err(io::Error::last_os_error());
        }
        if flags & libc::O_NONBLOCK == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "background PTY reader requires a nonblocking file",
            ));
        }
        // SAFETY: eventfd creates an independent descriptor; it is wrapped in
        // OwnedFd immediately and never outlives its owners.
        let fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        if fd == -1 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful eventfd returned an owned descriptor.
        let wake = unsafe { OwnedFd::from_raw_fd(fd) };
        let notify = wake.try_clone()?;
        let stop = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = mpsc::sync_channel(MAX_PENDING_CHUNKS);
        let stop_worker = Arc::clone(&stop);
        let worker = thread::Builder::new()
            .name("aura-pty-reader".to_owned())
            .spawn(move || {
                let mut file = file;
                let mut buffer = [0u8; READ_CHUNK_BYTES];
                while !stop_worker.load(Ordering::Acquire) {
                    let event = match file.read(&mut buffer) {
                        Ok(0) => ReadEvent::Closed,
                        Ok(length) => ReadEvent::Bytes(buffer[..length].to_vec()),
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            let mut pollfd = libc::pollfd {
                                fd: file.as_raw_fd(),
                                events: libc::POLLIN,
                                revents: 0,
                            };
                            // SAFETY: pollfd is a valid writable single entry;
                            // the timeout makes shutdown bounded when idle.
                            unsafe { libc::poll(&mut pollfd, 1, 100) };
                            continue;
                        }
                        Err(error) if error.raw_os_error() == Some(libc::EIO) => ReadEvent::Closed,
                        Err(error) => ReadEvent::Error(error),
                    };
                    let finished = !matches!(event, ReadEvent::Bytes(_));
                    if sender.send(event).is_err() {
                        break;
                    }
                    let _ = signal(notify.as_raw_fd());
                    if finished {
                        break;
                    }
                }
            })?;
        Ok(Self {
            receiver: Some(receiver),
            wake,
            stop,
            worker: Some(worker),
        })
    }

    pub fn wake_fd(&self) -> RawFd {
        self.wake.as_raw_fd()
    }

    pub fn clear_wakeup(&self) -> io::Result<()> {
        let mut value = 0u64;
        // SAFETY: value is writable for eight bytes and wake is owned.
        let count = unsafe {
            libc::read(
                self.wake.as_raw_fd(),
                (&mut value as *mut u64).cast(),
                size_of::<u64>(),
            )
        };
        if count == size_of::<u64>() as isize {
            return Ok(());
        }
        if count == -1 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                return Ok(());
            }
            return Err(error);
        }
        Err(io::Error::other("short PTY reader eventfd read"))
    }

    pub fn try_next(&self) -> Option<ReadEvent> {
        self.receiver.as_ref()?.try_recv().ok()
    }

    pub fn wake_again(&self) -> io::Result<()> {
        signal(self.wake.as_raw_fd())
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Unblocks a producer waiting on a full bounded channel.
        self.receiver.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn signal(fd: RawFd) -> io::Result<()> {
    let value = 1u64;
    // SAFETY: the pointer is valid for eight readable bytes; fd is an
    // eventfd opened nonblocking and owned by the caller.
    let count = unsafe { libc::write(fd, (&value as *const u64).cast(), size_of::<u64>()) };
    if count == size_of::<u64>() as isize {
        return Ok(());
    }
    if count == -1 {
        let error = io::Error::last_os_error();
        // A saturated eventfd is already readable, so the wake is not lost.
        if error.kind() == io::ErrorKind::WouldBlock {
            return Ok(());
        }
        return Err(error);
    }
    Err(io::Error::other("short PTY reader eventfd write"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn threaded_reader_delivers_bytes_in_order_and_reports_eof() {
        let mut fds = [-1; 2];
        // SAFETY: fds is writable for two descriptors; successful descriptors
        // become owned immediately below.
        assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
        // SAFETY: both descriptors were returned by the successful pipe2.
        let read = unsafe { File::from_raw_fd(fds[0]) };
        // SAFETY: the write descriptor is distinct and owned by this scope.
        let mut write = unsafe { File::from_raw_fd(fds[1]) };
        // SAFETY: F_SETFL changes file status flags on the owned pipe read end.
        assert_eq!(
            unsafe { libc::fcntl(read.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) },
            0
        );
        let reader = Reader::start(read).unwrap();
        let expected = "ab界e\u{301}".repeat(2_000).into_bytes();
        write.write_all(&expected).unwrap();
        drop(write);

        let mut actual = Vec::new();
        let mut closed = false;
        for _ in 0..100 {
            let mut pollfd = libc::pollfd {
                fd: reader.wake_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: pollfd points to a valid entry and the fd remains owned.
            assert!(unsafe { libc::poll(&mut pollfd, 1, 100) } >= 0);
            reader.clear_wakeup().unwrap();
            while let Some(event) = reader.try_next() {
                match event {
                    ReadEvent::Bytes(bytes) => actual.extend_from_slice(&bytes),
                    ReadEvent::Closed => closed = true,
                    ReadEvent::Error(error) => panic!("PTY read failed: {error}"),
                }
            }
            if closed {
                break;
            }
        }
        assert!(closed);
        assert_eq!(actual, expected);
    }

    #[test]
    fn reader_shutdown_does_not_wait_for_an_idle_pty() {
        let mut fds = [-1; 2];
        // SAFETY: the array has room for both returned owned descriptors.
        assert_eq!(
            unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK) },
            0
        );
        // SAFETY: pipe2 initialized each descriptor and transfers ownership.
        let read = unsafe { File::from_raw_fd(fds[0]) };
        // SAFETY: a distinct descriptor becomes owned by this local.
        let _write = unsafe { File::from_raw_fd(fds[1]) };
        let reader = Reader::start(read).unwrap();
        let started = std::time::Instant::now();
        drop(reader);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn reader_rejects_blocking_descriptors() {
        let mut fds = [-1; 2];
        // SAFETY: the array has room for both returned owned descriptors.
        assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
        // SAFETY: each returned descriptor has a unique new owner.
        let read = unsafe { File::from_raw_fd(fds[0]) };
        let _write = unsafe { File::from_raw_fd(fds[1]) };
        assert_eq!(
            Reader::start(read).err().unwrap().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
