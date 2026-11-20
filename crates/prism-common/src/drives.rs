//! Removable drives over UDisks2 (system bus), shared by prismd's drive notifications
//! (`drives.rs`) and prism-ui's Drives tray icon: which filesystems sit on removable drives,
//! mounting them, and ejecting a drive (unmount everything on it, then power it off).
//!
//! UDisks decides what a user may do (polkit: mounting removable media is allowed for the
//! active session). Nothing UDisks marks as system or ignored is listed.

use std::collections::HashMap;

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

pub const UDISKS: &str = "org.freedesktop.UDisks2";
const ROOT: &str = "/org/freedesktop/UDisks2";
const BLOCK: &str = "org.freedesktop.UDisks2.Block";
const FS: &str = "org.freedesktop.UDisks2.Filesystem";
const DRIVE: &str = "org.freedesktop.UDisks2.Drive";

/// A filesystem on a removable drive.
#[derive(Clone, Debug, PartialEq)]
pub struct Volume {
    /// UDisks block device object path.
    pub block: String,
    /// UDisks drive object path.
    pub drive: String,
    pub label: String,
    pub size: u64,
    pub mount: Option<String>,
}

impl Volume {
    /// "Label (16 GB)".
    pub fn title(&self) -> String {
        let s = human_size(self.size);
        if s.is_empty() { self.label.clone() } else { format!("{} ({s})", self.label) }
    }
}

pub fn human_size(n: u64) -> String {
    match n {
        0 => String::new(),
        n if n < 1 << 30 => format!("{:.0} MB", n as f64 / (1u64 << 20) as f64),
        n if n < 1 << 40 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
        n => format!("{:.1} TB", n as f64 / (1u64 << 40) as f64),
    }
}

type Ifaces = HashMap<String, HashMap<String, OwnedValue>>;

fn get<T: TryFrom<OwnedValue>>(props: Option<&HashMap<String, OwnedValue>>, k: &str) -> Option<T> {
    props?.get(k).and_then(|v| T::try_from(v.try_clone().ok()?).ok())
}

/// D-Bus byte strings (mount points) end with a NUL.
fn bytes_str(b: &[u8]) -> String {
    String::from_utf8_lossy(b.strip_suffix(&[0]).unwrap_or(b)).into_owned()
}

fn objects(conn: &Connection) -> Result<HashMap<OwnedObjectPath, Ifaces>, String> {
    let om = Proxy::new(conn, UDISKS, ROOT, "org.freedesktop.DBus.ObjectManager").map_err(|e| e.to_string())?;
    om.call("GetManagedObjects", &()).map_err(|e| e.to_string())
}

/// The volume at `block` if it is a filesystem on a removable drive.
fn volume(objects: &HashMap<OwnedObjectPath, Ifaces>, block: &str, ifaces: &Ifaces) -> Option<Volume> {
    let fs = ifaces.get(FS)?;
    let b = ifaces.get(BLOCK);
    let flag = |k: &str| get::<bool>(b, k).unwrap_or(false);
    if flag("HintSystem") || flag("HintIgnore") {
        return None;
    }
    let drive: OwnedObjectPath = get(b, "Drive")?;
    let d = objects.iter().find(|(p, _)| p.as_str() == drive.as_str()).and_then(|(_, i)| i.get(DRIVE));
    d?;
    let dflag = |k: &str| get::<bool>(d, k).unwrap_or(false);
    let bus: String = get(d, "ConnectionBus").unwrap_or_default();
    if !(dflag("Removable") || dflag("MediaRemovable") || matches!(bus.as_str(), "usb" | "sdio" | "ieee1394") || flag("HintAuto")) {
        return None;
    }
    let mut label: String = get(b, "IdLabel").unwrap_or_default();
    if label.trim().is_empty() {
        let vendor: String = get(d, "Vendor").unwrap_or_default();
        let model: String = get(d, "Model").unwrap_or_default();
        label = format!("{} {}", vendor.trim(), model.trim()).trim().to_string();
    }
    if label.is_empty() {
        label = "Removable drive".into();
    }
    let points: Vec<Vec<u8>> = get(Some(fs), "MountPoints").unwrap_or_default();
    Some(Volume { block: block.to_string(), drive: drive.to_string(), label, size: get(b, "Size").unwrap_or(0), mount: points.first().map(|p| bytes_str(p)) })
}

/// Every filesystem on a removable drive, by device path.
pub fn list(conn: &Connection) -> Result<Vec<Volume>, String> {
    let objs = objects(conn)?;
    let mut out: Vec<Volume> = objs.iter().filter_map(|(p, i)| volume(&objs, p.as_str(), i)).collect();
    out.sort_by(|a, b| a.block.cmp(&b.block));
    Ok(out)
}

/// The volume at `block`, if it is one we handle.
pub fn find(conn: &Connection, block: &str) -> Option<Volume> {
    let objs = objects(conn).ok()?;
    objs.iter().find(|(p, _)| p.as_str() == block).and_then(|(p, i)| volume(&objs, p.as_str(), i))
}

fn method_error(e: zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(_, Some(msg), _) => msg,
        e => e.to_string(),
    }
}

/// Mount `v` (if it is not yet) and return where.
pub fn mount(conn: &Connection, v: &Volume) -> Result<String, String> {
    if let Some(m) = &v.mount {
        return Ok(m.clone());
    }
    let opts: HashMap<&str, Value> = HashMap::new();
    let reply = conn.call_method(Some(UDISKS), v.block.as_str(), Some(FS), "Mount", &(opts,)).map_err(method_error)?;
    reply.body().deserialize::<String>().map_err(|e| e.to_string())
}

/// Unmount every filesystem on `drive`, then power it off (or eject the medium).
pub fn eject(conn: &Connection, drive: &str) -> Result<(), String> {
    let opts: HashMap<&str, Value> = HashMap::new();
    for v in list(conn)?.into_iter().filter(|v| v.drive == drive && v.mount.is_some()) {
        conn.call_method(Some(UDISKS), v.block.as_str(), Some(FS), "Unmount", &(&opts,)).map_err(method_error)?;
    }
    let p = Proxy::new(conn, UDISKS, drive, DRIVE).map_err(|e| e.to_string())?;
    let can_off = p.get_property::<bool>("CanPowerOff").unwrap_or(false);
    let ejectable = p.get_property::<bool>("Ejectable").unwrap_or(false);
    if can_off || ejectable {
        let method = if can_off { "PowerOff" } else { "Eject" };
        conn.call_method(Some(UDISKS), drive, Some(DRIVE), method, &(&opts,)).map_err(method_error)?;
    }
    Ok(())
}
