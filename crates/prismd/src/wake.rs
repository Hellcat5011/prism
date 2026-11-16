//! Channels that can wake a thread blocked in poll(2) on a Wayland connection.
//!
//! The idle and gamma threads each own a Wayland event queue. They block in `poll` on the
//! connection fd plus the read end of a pipe; `Sender::send` queues a message and writes a
//! byte to the pipe so the thread wakes and drains its `Receiver`.

use std::os::fd::{AsFd, AsRawFd, RawFd};
use std::sync::{mpsc, Arc};

use wayland_client::EventQueue;

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

pub struct Sender<T> {
    tx: mpsc::Sender<T>,
    pipe: Arc<Pipe>,
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Sender { tx: self.tx.clone(), pipe: Arc::clone(&self.pipe) }
    }
}

impl<T> Sender<T> {
    pub fn send(&self, msg: T) {
        if self.tx.send(msg).is_ok() {
            let b = 1u8;
            unsafe {
                libc::write(self.pipe.write, &b as *const u8 as *const libc::c_void, 1);
            }
        }
    }
}

pub struct Receiver<T> {
    rx: mpsc::Receiver<T>,
    pipe: Arc<Pipe>,
}

impl<T> Receiver<T> {
    /// Empty the wake pipe and return every queued message.
    pub fn drain(&self) -> Vec<T> {
        let mut buf = [0u8; 64];
        while unsafe { libc::read(self.pipe.read, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) } > 0 {}
        self.rx.try_iter().collect()
    }

    pub fn fd(&self) -> RawFd {
        self.pipe.read
    }
}

pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let mut fds = [0 as RawFd; 2];
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) };
    assert!(rc == 0, "pipe2 failed: {}", std::io::Error::last_os_error());
    let pipe = Arc::new(Pipe { read: fds[0], write: fds[1] });
    let (tx, rx) = mpsc::channel();
    (Sender { tx, pipe: Arc::clone(&pipe) }, Receiver { rx, pipe })
}

/// Flush, wait until the Wayland connection or `wake_fd` is readable (or `timeout_ms`
/// passes; -1 waits forever), read and dispatch events. Errors mean the connection is gone.
pub fn dispatch<S>(queue: &mut EventQueue<S>, state: &mut S, wake_fd: RawFd, timeout_ms: i32) -> Result<(), String> {
    queue.dispatch_pending(state).map_err(|e| e.to_string())?;
    queue.flush().map_err(|e| e.to_string())?;
    if let Some(guard) = queue.prepare_read() {
        let mut fds = [
            libc::pollfd { fd: guard.connection_fd().as_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: wake_fd, events: libc::POLLIN, revents: 0 },
        ];
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout_ms) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() != std::io::ErrorKind::Interrupted {
                return Err(err.to_string());
            }
        } else if fds[0].revents & (libc::POLLERR | libc::POLLHUP) != 0 {
            return Err("Wayland connection closed".into());
        } else if fds[0].revents & libc::POLLIN != 0 {
            match guard.read() {
                Ok(_) => {}
                Err(wayland_client::backend::WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.to_string()),
            }
        }
    }
    queue.dispatch_pending(state).map_err(|e| e.to_string())?;
    Ok(())
}
