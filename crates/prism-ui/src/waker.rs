// ─────────────────────────────────────────────────────────────────────────
// waker.rs — wake the Wayland event loop from worker threads
//
// The main loop `poll()`s the Wayland fd and this pipe together, so a
// background thread (status worker, file search, IPC socket listener) can
// hand over new data without the UI busy-polling on a short timeout.
// ─────────────────────────────────────────────────────────────────────────

use std::os::fd::RawFd;
use std::sync::Arc;

struct Pipe {
    read: RawFd,
    write: RawFd,
}

impl Drop for Pipe {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.read);
            libc::close(self.write);
        }
    }
}

#[derive(Clone)]
pub struct Waker {
    pipe: Arc<Pipe>,
}

impl Waker {
    pub fn new() -> std::io::Result<Self> {
        let mut fds = [0 as libc::c_int; 2];
        let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { pipe: Arc::new(Pipe { read: fds[0], write: fds[1] }) })
    }

    /// Wake the event loop. Never blocks; if the pipe is already full a wake is pending anyway.
    pub fn wake(&self) {
        let b = [1u8];
        unsafe {
            libc::write(self.pipe.write, b.as_ptr() as *const libc::c_void, 1);
        }
    }

    pub fn fd(&self) -> RawFd {
        self.pipe.read
    }

    /// Consume pending wake bytes after poll() reported the pipe readable.
    pub fn drain(&self) {
        let mut buf = [0u8; 64];
        loop {
            let n = unsafe { libc::read(self.pipe.read, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n <= 0 {
                break;
            }
        }
    }
}
