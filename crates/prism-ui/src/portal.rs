//! xdg-desktop-portal FileChooser: the system file dialog, for "Browse…" buttons.
//! Blocking; call from a worker thread.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use zbus::blocking::{Connection, MessageIterator};
use zbus::message::Type as MsgType;
use zbus::zvariant::{OwnedValue, Value};
use zbus::MatchRule;

/// Ask the user for a folder. Ok(None) when the dialog was cancelled.
pub fn pick_folder(title: &str, start: Option<&Path>) -> Result<Option<PathBuf>, String> {
    Ok(open_file(title, start, true, false, None, None)?.into_iter().next())
}

/// Ask the user for an audio file. Ok(None) when the dialog was cancelled.
pub fn pick_audio(title: &str, start: Option<&Path>) -> Result<Option<PathBuf>, String> {
    Ok(open_file(title, start, false, false, Some(("Audio", "audio/*")), None)?.into_iter().next())
}

/// Ask the user for a video file. Ok(None) when the dialog was cancelled.
pub fn pick_video(title: &str, start: Option<&Path>) -> Result<Option<PathBuf>, String> {
    Ok(open_file(title, start, false, false, Some(("Videos", "video/*")), None)?.into_iter().next())
}

/// Pick files to open (several allowed); empty when cancelled.
pub fn pick_files(title: &str) -> Result<Vec<PathBuf>, String> {
    open_file(title, None, false, true, None, None)
}

/// Pick a Prism backup to import. Ok(None) when cancelled.
pub fn pick_backup(title: &str) -> Result<Option<PathBuf>, String> {
    Ok(open_file(title, None, false, false, Some(("Prism backups", "application/gzip")), None)?.into_iter().next())
}

/// Ask where to save a file, suggesting `name`. Ok(None) when cancelled.
pub fn save_file(title: &str, name: &str) -> Result<Option<PathBuf>, String> {
    Ok(open_file(title, None, false, false, None, Some(name))?.into_iter().next())
}

/// OpenFile, or SaveFile when `save_as` names the suggested file.
fn open_file(title: &str, start: Option<&Path>, directory: bool, multiple: bool, filter: Option<(&str, &str)>, save_as: Option<&str>) -> Result<Vec<PathBuf>, String> {
    let e = |e: zbus::Error| e.to_string();
    let conn = Connection::session().map_err(e)?;
    let token = format!(
        "prism{}_{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
    );
    let sender = conn.unique_name().ok_or("no D-Bus unique name")?.as_str().trim_start_matches(':').replace('.', "_");
    let request_path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");

    // Subscribe to the Response signal before calling, so a fast reply is not missed.
    let rule = MatchRule::builder()
        .msg_type(MsgType::Signal)
        .interface("org.freedesktop.portal.Request")
        .and_then(|b| b.member("Response"))
        .and_then(|b| b.path(request_path.as_str()))
        .map_err(e)?
        .build();
    let mut responses = MessageIterator::for_match_rule(rule, &conn, Some(4)).map_err(e)?;

    let mut opts: HashMap<&str, Value> = HashMap::new();
    opts.insert("handle_token", Value::from(token.as_str()));
    if let Some(name) = save_as {
        opts.insert("current_name", Value::from(name));
    } else {
        opts.insert("directory", Value::from(directory));
        opts.insert("multiple", Value::from(multiple));
    }
    opts.insert("modal", Value::from(true));
    if let Some((name, mime)) = filter {
        // A list of (name, [(1 = MIME type, pattern)]).
        opts.insert("filters", Value::new(vec![(name.to_string(), vec![(1u32, mime.to_string())])]));
    }
    if let Some(dir) = start.filter(|d| d.is_dir()) {
        // current_folder is a NUL-terminated byte string.
        let mut bytes = std::os::unix::ffi::OsStrExt::as_bytes(dir.as_os_str()).to_vec();
        bytes.push(0);
        opts.insert("current_folder", Value::from(bytes));
    }
    conn.call_method(
        Some("org.freedesktop.portal.Desktop"),
        "/org/freedesktop/portal/desktop",
        Some("org.freedesktop.portal.FileChooser"),
        if save_as.is_some() { "SaveFile" } else { "OpenFile" },
        &("", title, opts),
    )
    .map_err(e)?;

    let msg = responses.next().ok_or("portal closed without a response")?.map_err(e)?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = msg.body().deserialize().map_err(e)?;
    if code != 0 {
        return Ok(Vec::new()); // 1 = cancelled, 2 = other
    }
    let uris: Vec<String> = results
        .get("uris")
        .and_then(|v| Vec::<String>::try_from(v.try_clone().ok()?).ok())
        .unwrap_or_default();
    Ok(uris.iter().filter_map(|u| uri_to_path(u)).collect())
}

/// file:// URI to a path, decoding %XX escapes.
fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(PathBuf::from(<std::ffi::OsString as std::os::unix::ffi::OsStringExt>::from_vec(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_file_uris() {
        assert_eq!(uri_to_path("file:///home/me/My%20Walls"), Some(PathBuf::from("/home/me/My Walls")));
        assert_eq!(uri_to_path("file:///a/b%"), Some(PathBuf::from("/a/b%")));
        assert_eq!(uri_to_path("https://x"), None);
    }
}
