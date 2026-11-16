//! Live reload of `config.json`: an inotify watch on the config directory. Each time the
//! file is written or replaced (Config::save renames a temp file over it) the new config is
//! parsed and handed to `on_change`. Invalid JSON keeps the previous config.

use std::ffi::{CString, OsStr};
use std::os::unix::ffi::OsStrExt;
use std::thread;
use std::time::Duration;

use prism_common::Config;

pub fn start(initial: &Config, on_change: impl Fn(Config) + Send + 'static) {
    let path = Config::config_path();
    let (Some(dir), Some(name)) = (path.parent().map(|d| d.to_path_buf()), path.file_name().map(|n| n.to_os_string())) else {
        return;
    };
    let mut last = serde_json::to_value(initial).ok();

    thread::spawn(move || {
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
        if fd < 0 {
            eprintln!("[prismd/config] inotify_init1 failed: {}", std::io::Error::last_os_error());
            return;
        }
        let _ = std::fs::create_dir_all(&dir);
        let Ok(cdir) = CString::new(dir.as_os_str().as_bytes()) else { return };
        if unsafe { libc::inotify_add_watch(fd, cdir.as_ptr(), libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO) } < 0 {
            eprintln!("[prismd/config] cannot watch {}: {}", dir.display(), std::io::Error::last_os_error());
            return;
        }
        eprintln!("[prismd/config] watching {}", path.display());

        let mut buf = [0u8; 4096];
        loop {
            let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n <= 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                eprintln!("[prismd/config] inotify read failed; live reload stopped");
                return;
            }
            if !touches(&buf[..n as usize], &name) {
                continue;
            }
            // Coalesce a burst of writes (e.g. a slider dragged in Settings).
            thread::sleep(Duration::from_millis(100));
            match Config::try_load() {
                Ok(cfg) => {
                    let value = serde_json::to_value(&cfg).ok();
                    if value.is_some() && value == last {
                        continue;
                    }
                    last = value;
                    eprintln!("[prismd/config] config.json changed; applying");
                    on_change(cfg);
                }
                Err(e) => eprintln!("[prismd/config] keeping previous config, parse failed: {e}"),
            }
        }
    });
}

/// True if any inotify event in `buf` names `file`.
fn touches(buf: &[u8], file: &OsStr) -> bool {
    let header = std::mem::size_of::<libc::inotify_event>();
    let mut off = 0;
    while off + header <= buf.len() {
        let ev = unsafe { std::ptr::read_unaligned(buf.as_ptr().add(off) as *const libc::inotify_event) };
        let start = off + header;
        let end = (start + ev.len as usize).min(buf.len());
        let raw = &buf[start..end];
        let name = &raw[..raw.iter().position(|&b| b == 0).unwrap_or(raw.len())];
        if name == file.as_bytes() {
            return true;
        }
        off = end;
    }
    false
}
