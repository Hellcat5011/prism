//! The display side of a video wallpaper: starts the `prism-video` decoder for one screen, shares
//! frame memory with it and says which frame is due. The caller shows frames with wl_shm buffers
//! made straight from that memory, so a frame is never copied here.

use std::collections::VecDeque;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

/// Frames in the shared memory.
pub const SLOTS: u32 = 4;
/// Further behind than this after a pause, playback carries on from where it is.
const LATE_LIMIT: Duration = Duration::from_millis(250);
const RECORD: usize = 12;

/// The decoder: next to this program, else on the PATH.
pub fn helper() -> PathBuf {
    std::env::current_exe().ok().and_then(|e| Some(e.parent()?.join("prism-video"))).filter(|p| p.is_file()).unwrap_or_else(|| "prism-video".into())
}

/// One picture from `video`, written as a PPM file.
pub fn extract_still(video: &Path, out: &Path) -> Result<(), String> {
    let status = Command::new(helper()).arg("still").arg(video).arg(out).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => Err("this video cannot be played".into()),
        Err(e) => Err(format!("prism-video: {e}")),
    }
}

pub struct VideoStream {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    memory: OwnedFd,
    pub width: i32,
    pub height: i32,
    partial: Vec<u8>,
    /// Frames decoded and not shown yet: (slot, time in ms).
    queue: VecDeque<(u32, u64)>,
    /// The moment time 0 of the video would have played.
    start: Option<Instant>,
    alive: bool,
}

impl VideoStream {
    /// Start decoding `file` for a screen of `width` by `height` pixels.
    pub fn open(file: &Path, width: i32, height: i32) -> Option<VideoStream> {
        if width <= 0 || height <= 0 {
            return None;
        }
        let size = SLOTS as i64 * width as i64 * height as i64 * 4;
        let memory = unsafe {
            let fd = libc::memfd_create(c"prism-video".as_ptr(), libc::MFD_CLOEXEC);
            if fd < 0 {
                return None;
            }
            let owned = OwnedFd::from_raw_fd(fd);
            if libc::ftruncate(fd, size) < 0 {
                return None;
            }
            owned
        };
        const CHILD_FD: i32 = 3;
        let shared = memory.as_raw_fd();
        let mut cmd = Command::new(helper());
        cmd.arg("play").arg(file).arg(width.to_string()).arg(height.to_string()).arg(CHILD_FD.to_string());
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        unsafe {
            cmd.pre_exec(move || {
                // The decoder ends with whoever started it, also when that one is killed.
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                if libc::dup2(shared, CHILD_FD) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn().ok()?;
        let (stdin, stdout) = (child.stdin.take()?, child.stdout.take()?);
        unsafe { libc::fcntl(stdout.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };
        Some(VideoStream { child, stdin, stdout, memory, width, height, partial: Vec::new(), queue: VecDeque::new(), start: None, alive: true })
    }

    /// The shared memory, for a wl_shm pool of `pool_size` bytes.
    pub fn memory(&self) -> &OwnedFd {
        &self.memory
    }

    pub fn pool_size(&self) -> i32 {
        SLOTS as i32 * self.frame_size()
    }

    pub fn frame_size(&self) -> i32 {
        self.width * self.height * 4
    }

    /// Readable when the decoder has announced frames.
    pub fn fd(&self) -> RawFd {
        self.stdout.as_raw_fd()
    }

    /// Read what the decoder announced. False once the decoder has stopped.
    pub fn pump(&mut self) -> bool {
        let mut buf = [0u8; RECORD * 8];
        loop {
            let n = unsafe { libc::read(self.stdout.as_raw_fd(), buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            match n {
                0 => self.alive = false,
                n if n > 0 => {
                    self.partial.extend_from_slice(&buf[..n as usize]);
                    continue;
                }
                _ => {}
            }
            break;
        }
        let whole = self.partial.len() / RECORD * RECORD;
        for r in self.partial[..whole].chunks_exact(RECORD) {
            let slot = u32::from_le_bytes([r[0], r[1], r[2], r[3]]);
            let ms = u64::from_le_bytes([r[4], r[5], r[6], r[7], r[8], r[9], r[10], r[11]]);
            self.queue.push_back((slot, ms));
        }
        self.partial.drain(..whole);
        self.alive
    }

    /// Time until the next frame is due; None while no frame is waiting.
    pub fn due_in(&self, now: Instant) -> Option<Duration> {
        let (_, ms) = *self.queue.front()?;
        Some(self.start.map_or(Duration::ZERO, |s| (s + Duration::from_millis(ms)).saturating_duration_since(now)))
    }

    /// The newest frame that is due, if any. Older ones go back to the decoder unseen.
    pub fn take_due(&mut self, now: Instant) -> Option<u32> {
        let (_, first) = *self.queue.front()?;
        let start = *self.start.get_or_insert_with(|| now - Duration::from_millis(first));
        // Paused (the wallpaper was covered): carry on from here, without racing to catch up.
        if now.saturating_duration_since(start + Duration::from_millis(first)) > LATE_LIMIT {
            self.start = Some(now - Duration::from_millis(first));
        }
        let start = self.start.unwrap_or(start);
        let mut shown = None;
        while let Some(&(slot, ms)) = self.queue.front() {
            if start + Duration::from_millis(ms) > now {
                break;
            }
            self.queue.pop_front();
            if let Some(old) = shown.replace(slot) {
                self.release(old);
            }
        }
        shown
    }

    /// The compositor is done with `slot`: the decoder may write it again.
    pub fn release(&mut self, slot: u32) {
        let _ = self.stdin.write_all(&slot.to_le_bytes());
    }
}

impl Drop for VideoStream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
