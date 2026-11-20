//! The per-user runtime directory for sockets, locks and short-lived files.
//!
//! `$XDG_RUNTIME_DIR` (private to the user, on tmpfs) when it is set. Without it the fallback is
//! a private `0700` directory under `~/.cache/prism/runtime`, never a predictable name in the
//! shared `/tmp`, where another user could create the path first or plant a symlink.

use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::OnceLock;

/// The runtime directory (see the module docs).
pub fn dir() -> PathBuf {
    if let Some(d) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
        if d.is_dir() {
            return d;
        }
    }
    static FALLBACK: OnceLock<PathBuf> = OnceLock::new();
    FALLBACK.get_or_init(fallback).clone()
}

fn fallback() -> PathBuf {
    let uid = unsafe { libc::getuid() };
    let d = match std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        Some(home) => PathBuf::from(home).join(".cache/prism/runtime"),
        None => std::env::temp_dir().join(format!("prism-runtime-{uid}")),
    };
    let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&d);
    match std::fs::symlink_metadata(&d) {
        Ok(m) if m.is_dir() && m.uid() == uid => {
            if m.mode() & 0o077 != 0 {
                let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
            }
        }
        _ => eprintln!("[prism] {} is not a directory owned by this user; set XDG_RUNTIME_DIR", d.display()),
    }
    d
}
