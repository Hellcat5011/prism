//! Share plugin (`kdeconnect.share.request`): files in both directions, text and URLs from the
//! phone. Received files go to `phone_download_dir` (default `~/Downloads`) as `<name>.part`
//! until complete; name clashes get " (1)", " (2)", ...

use crate::core::{Core, Shared};
use crate::notify::{self, Note};
use crate::packet::Packet;
use prism_common::phone::{PhoneReply, PhoneTransfer, TransferDirection};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[derive(Default)]
pub struct ShareState {
    next: u64,
    cancel: Vec<(u64, Arc<AtomicBool>)>,
}

/// Keep at most this many finished transfers in the view.
const KEEP_DONE: usize = 5;

fn expand(dir: &str) -> PathBuf {
    match dir.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rest),
        None if dir == "~" || dir.is_empty() => PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(if dir.is_empty() { "Downloads" } else { "" }),
        None => PathBuf::from(dir),
    }
}

/// A file name from the phone, safe to create in the download folder.
pub fn sanitize(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = base.chars().map(|c| if c.is_control() { '_' } else { c }).collect();
    let clean = clean.trim().trim_start_matches('.').to_string();
    if clean.is_empty() { "received-file".into() } else { clean.chars().take(200).collect() }
}

/// `dir/name`, or `dir/name (1).ext` and so on when taken.
pub fn free_path(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() && !dir.join(format!("{name}.part")).exists() {
        return p;
    }
    let (stem, ext) = match name.rfind('.').filter(|i| *i > 0) {
        Some(i) => (&name[..i], &name[i..]),
        None => (name, ""),
    };
    (1..).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|p| !p.exists() && !Path::new(&format!("{}.part", p.display())).exists()).unwrap()
}

/// Claim a free name by creating its `.part` file: (final path, part path, open part file).
/// Exclusive, so transfers running at the same time never share a file.
fn reserve(dir: &Path, name: &str) -> std::io::Result<(PathBuf, PathBuf, std::fs::File)> {
    loop {
        let target = free_path(dir, name);
        let part = PathBuf::from(format!("{}.part", target.display()));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&part) {
            Ok(f) => return Ok((target, part, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

fn add_transfer(core: &mut Core, id: &str, t: PhoneTransfer) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    core.plugins.share.cancel.push((t.id, flag.clone()));
    if let Some(d) = core.devices.get_mut(id) {
        d.view.transfers.push(t);
        let done: Vec<u64> = d.view.transfers.iter().filter(|t| t.done || t.error.is_some()).map(|t| t.id).collect();
        if done.len() > KEEP_DONE {
            let drop: Vec<u64> = done[..done.len() - KEEP_DONE].to_vec();
            d.view.transfers.retain(|t| !drop.contains(&t.id));
        }
    }
    core.publish();
    flag
}

fn update(shared: &Shared, id: &str, tid: u64, f: impl FnOnce(&mut PhoneTransfer)) {
    let mut core = shared.lock().unwrap();
    if let Some(t) = core.devices.get_mut(id).and_then(|d| d.view.transfers.iter_mut().find(|t| t.id == tid)) {
        f(t);
    }
    core.publish();
}

/// Progress callback: updates the view at most every 250 ms; false when cancelled.
fn progress(shared: Shared, id: String, tid: u64, cancel: Arc<AtomicBool>) -> impl FnMut(u64) -> bool {
    let start = Instant::now();
    let mut last = Instant::now();
    move |bytes| {
        if last.elapsed().as_millis() >= 250 {
            last = Instant::now();
            let speed = (bytes as f64 / start.elapsed().as_secs_f64().max(0.001)) as u64;
            update(&shared, &id, tid, |t| {
                t.bytes = bytes;
                t.speed = speed;
            });
        }
        !cancel.load(Ordering::SeqCst)
    }
}

pub fn handle(core: &mut Core, id: &str, p: &Packet) {
    let shared = core.shared();
    if p.kind == "kdeconnect.share.request.update" {
        return;
    }
    let phone = core.devices.get(id).map(|d| d.view.name.clone()).unwrap_or_default();
    if let Some(url) = p.str("url") {
        // Never opened automatically: offer it.
        notify::show(Note {
            key: format!("url:{url}"),
            app: phone.clone(),
            summary: format!("Link from {phone}"),
            body: url.to_string(),
            actions: vec![("open".into(), "Open".into())],
            icon: "web-browser".into(),
            urgency: 1,
        });
        return;
    }
    if let Some(text) = p.str("text") {
        crate::plugins::clipboard::handle_shared_text(core, text);
        notify::show(Note { key: format!("text:{id}"), app: phone.clone(), summary: format!("Text from {phone}"), body: format!("Copied to the clipboard: {}", text.chars().take(200).collect::<String>()), actions: Vec::new(), icon: "edit-paste".into(), urgency: 1 });
        return;
    }
    let (Some(port), Some(shared)) = (p.payload_port(), shared.as_ref()) else { return };
    let name = sanitize(p.str("filename").unwrap_or("received-file"));
    let size = p.payload_size.unwrap_or(-1);
    let dir = expand(&core.cfg.phone_download_dir);
    let open_after = core.cfg.phone_open_received;
    core.plugins.share.next += 1;
    let tid = core.plugins.share.next;
    let cancel = add_transfer(core, id, PhoneTransfer { id: tid, direction: TransferDirection::Incoming, name: name.clone(), total: size.max(0) as u64, ..Default::default() });
    let (shared, id) = (shared.clone(), id.to_string());
    std::thread::spawn(move || {
        let result = (|| -> Result<PathBuf, String> {
            std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            let (target, part, mut f) = reserve(&dir, &name).map_err(|e| format!("{}: {e}", dir.display()))?;
            let r = crate::payload::download(&shared, &id, port, size, &mut f, progress(shared.clone(), id.clone(), tid, cancel));
            drop(f);
            match r {
                Ok(_) => {
                    std::fs::rename(&part, &target).map_err(|e| e.to_string())?;
                    Ok(target)
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&part);
                    Err(e.to_string())
                }
            }
        })();
        match result {
            Ok(path) => {
                let ps = path.to_string_lossy().into_owned();
                update(&shared, &id, tid, |t| {
                    t.done = true;
                    t.bytes = t.total.max(t.bytes);
                    t.path = Some(ps.clone());
                });
                notify::show(Note {
                    key: format!("file:{ps}"),
                    app: phone.clone(),
                    summary: format!("Received {}", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
                    body: format!("From {phone}, saved in {}", path.parent().map(|p| p.display().to_string()).unwrap_or_default()),
                    actions: vec![("open".into(), "Open".into()), ("folder".into(), "Show in folder".into())],
                    icon: "document-save".into(),
                    urgency: 1,
                });
                if open_after {
                    open(&ps);
                }
            }
            Err(e) => {
                update(&shared, &id, tid, |t| t.error = Some(e.clone()));
                notify::show(Note { key: format!("fail:{tid}"), app: phone.clone(), summary: format!("Could not receive {name}"), body: e, actions: Vec::new(), icon: "dialog-error".into(), urgency: 1 });
            }
        }
    });
}

pub fn open(target: &str) {
    let _ = prism_common::apps::app_command("xdg-open", &[target.to_string()]).spawn();
}

/// `prism-ctl phone send <files>` and the Send Files tile.
pub fn send_files(shared: &Shared, device: Option<String>, paths: Vec<String>) -> PhoneReply {
    let id = {
        let core = shared.lock().unwrap();
        match core.resolve(&device) {
            Ok(id) if core.devices[&id].link.is_some() && core.devices[&id].view.paired => id,
            Ok(_) => return PhoneReply::Error { message: "the phone is not connected".into() },
            Err(e) => return PhoneReply::Error { message: e },
        }
    };
    let files: Vec<PathBuf> = paths.iter().map(PathBuf::from).filter(|p| p.is_file()).collect();
    if files.is_empty() {
        return PhoneReply::Error { message: "no readable files given".into() };
    }
    let total: u64 = files.iter().filter_map(|p| p.metadata().ok()).map(|m| m.len()).sum();
    let count = files.len();
    let shared = shared.clone();
    std::thread::spawn(move || {
        // One at a time, as KDE Connect does.
        for path in files {
            send_one(&shared, &id, &path, count, total);
        }
    });
    PhoneReply::Ok { message: Some(format!("sending {count} file(s)")) }
}

fn send_one(shared: &Shared, id: &str, path: &Path, count: usize, total: u64) {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    let Ok(meta) = path.metadata() else { return };
    let size = meta.len();
    let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64).unwrap_or(0);
    let (tid, cancel) = {
        let mut core = shared.lock().unwrap();
        core.plugins.share.next += 1;
        let tid = core.plugins.share.next;
        let cancel = add_transfer(&mut core, id, PhoneTransfer { id: tid, direction: TransferDirection::Outgoing, name: name.clone(), total: size, ..Default::default() });
        (tid, cancel)
    };
    let (done_tx, done_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let file = path.to_path_buf();
    let (sh, idc, cc) = (shared.clone(), id.to_string(), cancel.clone());
    let port = crate::payload::serve(shared, id, move |t| {
        let mut f = std::fs::File::open(&file)?;
        let r = crate::payload::pump(t, &mut f, progress(sh, idc, tid, cc)).map(|_| ());
        let _ = done_tx.send(r.as_ref().map(|_| ()).map_err(|e| e.to_string()));
        r
    });
    let port = match port {
        Ok(p) => p,
        Err(e) => {
            update(shared, id, tid, |t| t.error = Some(e.to_string()));
            return;
        }
    };
    let mut p = Packet::new("kdeconnect.share.request", json!({ "filename": name, "lastModified": mtime, "creationTime": mtime, "numberOfFiles": count, "totalPayloadSize": total }));
    p.payload_size = Some(size as i64);
    p.payload_transfer_info = Some(json!({ "port": port }));
    shared.lock().unwrap().send(id, p);
    let r = done_rx.recv_timeout(std::time::Duration::from_secs(3600)).unwrap_or_else(|_| Err("the phone did not fetch the file".into()));
    update(shared, id, tid, |t| match r {
        Ok(()) => {
            t.done = true;
            t.bytes = t.total;
        }
        Err(e) => t.error = Some(e),
    });
}

pub fn cancel(core: &mut Core, tid: u64) -> Result<Option<String>, String> {
    let flag = core.plugins.share.cancel.iter().find(|(t, _)| *t == tid).map(|(_, f)| f.clone()).ok_or("no such transfer")?;
    flag.store(true, Ordering::SeqCst);
    Ok(None)
}

pub fn send_text(core: &mut Core, id: &str, text: Option<&str>, url: Option<&str>) -> Result<Option<String>, String> {
    let body = match (text, url) {
        (_, Some(u)) => json!({ "url": u }),
        (Some(t), _) => json!({ "text": t }),
        _ => return Err("nothing to send".into()),
    };
    core.send(id, Packet::new("kdeconnect.share.request", body));
    Ok(None)
}

/// Actions on our file and link notifications.
pub fn desktop_action(key: &str, action: &str) {
    if let Some(path) = key.strip_prefix("file:") {
        match action {
            "folder" => open(&Path::new(path).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()),
            _ => open(path),
        }
    } else if let Some(url) = key.strip_prefix("url:") {
        if url.starts_with("http://") || url.starts_with("https://") {
            open(url);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_safe_and_unique() {
        assert_eq!(sanitize("../../etc/passwd"), "passwd");
        assert_eq!(sanitize(".bashrc"), "bashrc");
        assert_eq!(sanitize("a\nb.txt"), "a_b.txt");
        assert_eq!(sanitize(""), "received-file");
        let dir = std::env::temp_dir().join(format!("prism-share-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(free_path(&dir, "a.txt"), dir.join("a.txt"));
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        assert_eq!(free_path(&dir, "a.txt"), dir.join("a (1).txt"));
        std::fs::write(dir.join("noext"), "x").unwrap();
        assert_eq!(free_path(&dir, "noext"), dir.join("noext (1)"));
        std::fs::write(dir.join("a (1).txt.part"), "x").unwrap();
        assert_eq!(free_path(&dir, "a.txt"), dir.join("a (2).txt"));
        let (first, _, _) = reserve(&dir, "b.txt").unwrap();
        let (second, _, _) = reserve(&dir, "b.txt").unwrap();
        assert_ne!(first, second);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
