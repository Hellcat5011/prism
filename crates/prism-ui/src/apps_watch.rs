//! Notices apps being installed or removed: one thread watches the application directories with
//! inotify and, once a package manager has finished writing, refreshes the icon index and bumps
//! a generation that the launcher's list and the dock follow.

use crate::waker::Waker;
use prism_common::apps::application_search_paths;
use std::os::unix::ffi::OsStrExt;
use std::sync::{Condvar, Mutex, Once};
use std::time::Duration;

/// An install writes many files: wait for this much quiet before looking at the result.
const QUIET_MS: i32 = 500;
/// How often directories that did not exist yet are looked for.
const RESCAN_MS: i32 = 30_000;

static GENERATION: Mutex<u64> = Mutex::new(0);
static CHANGED: Condvar = Condvar::new();
static START: Once = Once::new();

/// Changes whenever the set of installed apps may have changed.
pub fn generation() -> u64 {
    *GENERATION.lock().unwrap()
}

/// Block until the generation differs from `seen`.
pub fn wait_for_change(seen: u64) {
    let mut generation = GENERATION.lock().unwrap();
    while *generation == seen {
        generation = CHANGED.wait(generation).unwrap();
    }
}

/// Whether an event for the file `name` concerns an app (also a temporary name of one).
fn is_app_file(name: &[u8]) -> bool {
    name.windows(8).any(|w| w == b".desktop")
}

/// Whether the raw inotify records in `buf` mention an app file.
fn mentions_app(buf: &[u8]) -> bool {
    let header = std::mem::size_of::<libc::inotify_event>();
    let mut off = 0;
    while off + header <= buf.len() {
        // The buffer is byte-aligned: read the record's length without forming a reference.
        let event = unsafe { std::ptr::read_unaligned(buf.as_ptr().add(off) as *const libc::inotify_event) };
        let end = (off + header + event.len as usize).min(buf.len());
        if is_app_file(&buf[off + header..end]) {
            return true;
        }
        off = end;
    }
    false
}

/// Watch every application directory that exists now (adding one twice changes nothing).
fn add_watches(fd: i32) {
    const MASK: u32 = libc::IN_CREATE | libc::IN_DELETE | libc::IN_MOVED_TO | libc::IN_MOVED_FROM | libc::IN_CLOSE_WRITE;
    for dir in application_search_paths() {
        if let Ok(path) = std::ffi::CString::new(dir.as_os_str().as_bytes()) {
            unsafe { libc::inotify_add_watch(fd, path.as_ptr(), MASK) };
        }
    }
}

/// Wait up to `timeout_ms` for events; true when any of those read mention an app.
fn read_events(fd: i32, timeout_ms: i32) -> Option<bool> {
    let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    if unsafe { libc::poll(&mut pfd, 1, timeout_ms) } <= 0 {
        return None;
    }
    let mut buf = [0u8; 8192];
    let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    Some(n > 0 && mentions_app(&buf[..n as usize]))
}

pub fn start(waker: Waker) {
    START.call_once(|| {
        let spawned = std::thread::Builder::new().name("prism-apps-watch".into()).spawn(move || {
            let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
            if fd < 0 {
                eprintln!("[prism-ui] cannot watch for installed apps: {}", std::io::Error::last_os_error());
                return;
            }
            loop {
                add_watches(fd);
                if read_events(fd, RESCAN_MS) != Some(true) {
                    continue;
                }
                while read_events(fd, QUIET_MS).is_some() {}
                // Icons arrive with the app; the index has to know them before the list is read.
                crate::icons::refresh();
                *GENERATION.lock().unwrap() += 1;
                CHANGED.notify_all();
                waker.wake();
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        if let Err(e) = spawned {
            eprintln!("[prism-ui] cannot watch for installed apps: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &[u8]) -> Vec<u8> {
        let event = libc::inotify_event { wd: 1, mask: libc::IN_CREATE, cookie: 0, len: name.len() as u32 };
        let mut out = unsafe { std::slice::from_raw_parts(&event as *const _ as *const u8, std::mem::size_of::<libc::inotify_event>()) }.to_vec();
        out.extend_from_slice(name);
        out
    }

    #[test]
    fn only_app_files_count() {
        assert!(is_app_file(b"firefox.desktop\0\0"));
        assert!(is_app_file(b"firefox.desktop.Xa12"));
        assert!(!is_app_file(b"mimeinfo.cache"));
    }

    #[test]
    fn every_record_of_a_batch_is_read() {
        let mut batch = record(b"mimeinfo.cache\0\0");
        assert!(!mentions_app(&batch));
        batch.extend(record(b"kitty.desktop\0\0\0"));
        assert!(mentions_app(&batch));
    }

    #[test]
    fn a_cut_off_record_is_not_read_past_the_buffer() {
        let batch = record(b"kitty.desktop\0\0\0");
        assert!(!mentions_app(&batch[..4]));
    }
}
