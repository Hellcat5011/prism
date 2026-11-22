//! Settings > Calendar > Calendar Accounts: online accounts whose calendars sync both ways, kept
//! by GNOME Online Accounts (GOA) and read and written through evolution-data-server.
//!
//! * Accounts…: lists GOA's calendar accounts; one can have its calendar turned off or on, or
//!   be removed. CalDAV accounts (iCloud, Fastmail, Nextcloud, any CalDAV server) are added
//!   here: the server is asked first whether the name and password work (PROPFIND), then GOA
//!   keeps the account and its password. Google and Microsoft sign in in the browser, which only
//!   gnome-online-accounts-gtk can start (GOA's sign-in is part of its GTK dialogs); Prism opens
//!   it, offering to install it first.
//! * Calendars…: every calendar evolution-data-server has: shown or hidden in Prism
//!   (`calendar_hidden`), and which one new events go into (`calendar_default`).
//!
//! Each runs on a thread and asks through prompt cards (`prompt.rs`), one after another.

use std::collections::HashMap;

use prism_common::{eds, Config};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedObjectPath, Value};

use crate::prompt::{self, Choice, Field, Request, Session, Verdict};

const GOA: &str = "org.gnome.OnlineAccounts";
const GOA_PATH: &str = "/org/gnome/OnlineAccounts";
const GOA_MANAGER: &str = "/org/gnome/OnlineAccounts/Manager";
const ACCOUNT_IFACE: &str = "org.gnome.OnlineAccounts.Account";
const GOA_GTK: &str = "/usr/bin/gnome-online-accounts-gtk";
const ICLOUD: &str = "https://caldav.icloud.com";

/// GOA providers with calendars.
const CALENDAR_PROVIDERS: &[&str] = &["google", "ms_graph", "webdav", "owncloud", "exchange"];

struct Account {
    path: OwnedObjectPath,
    provider: String,
    provider_name: String,
    name: String,
    calendar_off: bool,
    needs_sign_in: bool,
}

/// Opens the Accounts… flow.
pub fn accounts() {
    std::thread::spawn(|| {
        if let Err(e) = accounts_flow() {
            tell("Calendar accounts", &e);
        }
    });
}

/// Opens the Calendars… flow.
pub fn calendars() {
    std::thread::spawn(|| {
        if let Err(e) = calendars_flow() {
            tell("Calendars", &e);
        }
    });
}

fn ask(req: Request) -> Result<Session, String> {
    prompt::ask(req).ok_or_else(|| "cannot show a prompt".to_string())
}

fn request(title: &str, message: &str, detail: &str, icon: &'static str, ok: &str) -> Request {
    Request { title: title.into(), message: message.into(), detail: detail.into(), icon, fields: Vec::new(), ok_label: ok.into(), choices: Vec::new() }
}

/// A card with a message and OK.
fn tell(title: &str, message: &str) {
    if let Some(s) = prompt::ask(request(title, message, "", "dot", "OK")) {
        let _ = s.answers.recv();
        s.verdict(Verdict::Done);
    }
}

/// A list to pick from; the index, or None when closed.
fn pick(title: &str, message: &str, detail: &str, ok: &str, choices: Vec<Choice>) -> Result<Option<usize>, String> {
    let s = ask(Request { choices, ..request(title, message, detail, "dot", ok) })?;
    let answer = s.answers.recv().ok().flatten();
    s.verdict(Verdict::Done);
    Ok(answer.and_then(|a| a.first().and_then(|i| i.parse().ok())))
}

fn choice(title: impl Into<String>, detail: impl Into<String>, icon: &'static str) -> Choice {
    Choice { title: title.into(), detail: detail.into(), icon }
}

/// Tell prismd to read the calendars again.
fn refresh() {
    let _ = prism_common::ipc::request(&prism_common::ipc::IpcCommand::CalendarRefresh);
}

// ── GNOME Online Accounts ────────────────────────────────────────────────

fn goa_available() -> bool {
    std::path::Path::new("/usr/share/dbus-1/services/org.gnome.OnlineAccounts.service").exists()
}

fn list_accounts(conn: &Connection) -> Result<Vec<Account>, String> {
    let om = zbus::blocking::fdo::ObjectManagerProxy::builder(conn)
        .destination(GOA)
        .and_then(|b| b.path(GOA_PATH))
        .and_then(|b| b.build())
        .map_err(|e| format!("GNOME Online Accounts: {e}"))?;
    let objects = om.get_managed_objects().map_err(|e| format!("GNOME Online Accounts: {e}"))?;
    let mut out = Vec::new();
    for (path, ifaces) in objects {
        let Some(props) = ifaces.iter().find(|(n, _)| n.as_str() == ACCOUNT_IFACE).map(|(_, p)| p) else { continue };
        let text = |k: &str| props.get(k).and_then(|v| String::try_from(v.try_clone().ok()?).ok()).unwrap_or_default();
        let flag = |k: &str| props.get(k).and_then(|v| bool::try_from(v).ok()).unwrap_or(false);
        let provider = text("ProviderType");
        if !CALENDAR_PROVIDERS.contains(&provider.as_str()) {
            continue;
        }
        out.push(Account { path, provider_name: text("ProviderName"), name: text("PresentationIdentity"), calendar_off: flag("CalendarDisabled"), needs_sign_in: flag("AttentionNeeded"), provider });
    }
    out.sort_by(|a, b| (&a.provider_name, &a.name).cmp(&(&b.provider_name, &b.name)));
    Ok(out)
}

fn account_proxy<'a>(conn: &Connection, a: &'a Account) -> Result<Proxy<'a>, String> {
    Proxy::new(conn, GOA, a.path.as_ref(), ACCOUNT_IFACE).map_err(|e| e.to_string())
}

fn accounts_flow() -> Result<(), String> {
    if !goa_available() || !eds::available() {
        tell("Calendar accounts", "Online calendars need GNOME Online Accounts and evolution-data-server. Install them with: sudo pacman -S gnome-online-accounts evolution-data-server");
        return Ok(());
    }
    let conn = Connection::session().map_err(|e| e.to_string())?;
    loop {
        let list = list_accounts(&conn)?;
        let mut choices: Vec<Choice> = list
            .iter()
            .map(|a| {
                let state = if a.needs_sign_in {
                    "needs signing in again"
                } else if a.calendar_off {
                    "calendar off"
                } else {
                    "calendar on"
                };
                choice(&a.name, format!("{} \u{b7} {state}", a.provider_name), "check")
            })
            .collect();
        let adds = [("Add Google account", "Signs in in your browser"), ("Add Microsoft account", "Outlook.com and Microsoft 365, signs in in your browser"), ("Add iCloud account", "Needs an app-specific password from your Apple account"), ("Add CalDAV account", "Fastmail, Nextcloud, mailbox.org or any CalDAV server")];
        choices.extend(adds.iter().map(|(t, d)| choice(*t, *d, "plus")));
        let message = if list.is_empty() { "No calendar accounts yet. Their calendars sync both ways: events you add from the Calendar widget go to the account too." } else { "Pick an account to change it, or add one." };
        let Some(i) = pick("Calendar accounts", message, "", "Open", choices)? else { return Ok(()) };
        match i.checked_sub(list.len()) {
            None => account_actions(&conn, &list[i])?,
            Some(0) => return browser_sign_in("Google"),
            Some(1) => return browser_sign_in("Microsoft"),
            Some(2) => add_caldav(true)?,
            Some(_) => add_caldav(false)?,
        }
    }
}

fn account_actions(conn: &Connection, a: &Account) -> Result<(), String> {
    let mut choices = vec![choice(if a.calendar_off { "Turn calendar on" } else { "Turn calendar off" }, "Its events show in Prism and new ones can go to it", "check"), choice("Remove account", "From this computer only; nothing is deleted on the server", "trash")];
    if a.needs_sign_in && matches!(a.provider.as_str(), "google" | "ms_graph") {
        choices.insert(0, choice("Sign in again", "Opens GNOME Online Accounts", "key"));
    }
    let Some(i) = pick(&a.name, &a.provider_name, "", "Do it", choices)? else { return Ok(()) };
    let i = if a.needs_sign_in && matches!(a.provider.as_str(), "google" | "ms_graph") { i } else { i + 1 };
    let p = account_proxy(conn, a)?;
    match i {
        0 => return browser_sign_in(if a.provider == "google" { "Google" } else { "Microsoft" }),
        1 => {
            p.set_property("CalendarDisabled", !a.calendar_off).map_err(|e| format!("changing the account: {e}"))?;
        }
        _ => {
            let s = ask(request("Remove account", &format!("Remove {}? Its calendars leave Prism and this computer; nothing is deleted on the server.", a.name), "", "trash", "Remove"))?;
            if s.answers.recv().ok().flatten().is_none() {
                return Ok(());
            }
            let r = p.call::<_, _, ()>("Remove", &()).map_err(|e| format!("removing the account: {e}"));
            s.verdict(if r.is_ok() { Verdict::Done } else { Verdict::Cancel });
            r?;
        }
    }
    refresh();
    Ok(())
}

/// Google and Microsoft: GOA's own dialog (gnome-online-accounts-gtk), installed first if the
/// user agrees.
fn browser_sign_in(provider: &str) -> Result<(), String> {
    if !std::path::Path::new(GOA_GTK).exists() {
        if !std::path::Path::new("/usr/bin/pacman").exists() {
            tell("Calendar accounts", &format!("{provider} accounts are added with gnome-online-accounts-gtk. Install it with your package manager, then try again."));
            return Ok(());
        }
        let s = ask(request(
            "Calendar accounts",
            &format!("Signing in to {provider} needs gnome-online-accounts-gtk (about 180 KiB; it uses what GNOME Online Accounts already installed). Install it now?"),
            "Installed with pacman; your password is asked for.",
            "plus",
            "Install",
        ))?;
        if s.answers.recv().ok().flatten().is_none() {
            return Ok(());
        }
        s.verdict(Verdict::Done);
        let ok = std::process::Command::new("pkexec").args(["/usr/bin/pacman", "-S", "--needed", "--noconfirm", "gnome-online-accounts-gtk"]).status().is_ok_and(|st| st.success());
        if !ok || !std::path::Path::new(GOA_GTK).exists() {
            tell("Calendar accounts", "gnome-online-accounts-gtk was not installed. Install it with: sudo pacman -S gnome-online-accounts-gtk");
            return Ok(());
        }
    }
    let child = prism_common::apps::app_command(GOA_GTK, &[]).spawn().map_err(|e| format!("gnome-online-accounts-gtk: {e}"))?;
    crate::reap(child);
    tell("Calendar accounts", &format!("Choose {provider} in the window that opened and sign in. Its calendars appear in Prism a minute after, and in Settings > Calendar Accounts."));
    Ok(())
}

/// curl's config syntax for one quoted value.
fn curl_quote(s: &str) -> String {
    let mut q = String::from('"');
    for c in s.chars() {
        match c {
            '\\' => q.push_str("\\\\"),
            '"' => q.push_str("\\\""),
            '\n' | '\r' => {}
            c => q.push(c),
        }
    }
    q.push('"');
    q
}

/// The HTTP status of a CalDAV PROPFIND on `url` with the name and password (given to curl on
/// its standard input, never on its command line).
fn propfind(url: &str, user: &str, password: &str) -> Result<u16, String> {
    use std::io::Write;
    let body = r#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:"><d:prop><d:current-user-principal/></d:prop></d:propfind>"#;
    let mut child = std::process::Command::new("curl")
        .args(["-s", "-o", "/dev/null", "-w", "%{http_code}", "-X", "PROPFIND", "-H", "Depth: 0", "-H", "Content-Type: application/xml", "--data", body, "--max-time", "20", "-L", "--proto", "=https,http", "--proto-redir", "=https", "-K", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = writeln!(stdin, "url = {}", curl_quote(url));
        let _ = writeln!(stdin, "user = {}", curl_quote(&format!("{user}:{password}")));
    }
    let out = child.wait_with_output().map_err(|e| format!("curl: {e}"))?;
    let code: u16 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0);
    if code == 0 {
        return Err("The server could not be reached. Check the address and your connection.".into());
    }
    Ok(code)
}

/// "nextcloud.example.com" -> "https://nextcloud.example.com".
fn server_url(s: &str) -> Option<String> {
    let s = s.trim().trim_end_matches('/');
    if s.is_empty() || s.contains(char::is_whitespace) {
        return None;
    }
    if s.starts_with("https://") || s.starts_with("http://") {
        Some(s.to_string())
    } else {
        Some(format!("https://{s}"))
    }
}

fn add_caldav(icloud: bool) -> Result<(), String> {
    let (title, message, detail) = if icloud {
        ("Add iCloud account", "Your Apple Account address and an app-specific password.", "Make the password at account.apple.com > Sign-In and Security > App-Specific Passwords. It is kept in your keyring by GNOME Online Accounts.")
    } else {
        ("Add CalDAV account", "The server's address, and your name and password there.", "Fastmail: caldav.fastmail.com. Nextcloud: your server's address. The password is kept in your keyring by GNOME Online Accounts.")
    };
    let mut fields = Vec::new();
    if !icloud {
        fields.push(Field { label: "Server".into(), secret: false, value: String::new() });
    }
    fields.push(Field { label: if icloud { "Apple Account (email)".into() } else { "User name".into() }, secret: false, value: String::new() });
    fields.push(Field { label: if icloud { "App-specific password".into() } else { "Password".into() }, secret: true, value: String::new() });
    let s = ask(Request { fields, ..request(title, message, detail, "key", "Add") })?;
    loop {
        let Some(a) = s.answers.recv().ok().flatten() else { return Ok(()) };
        let (server, user, password) = if icloud { (ICLOUD.to_string(), a[0].trim().to_string(), a[1].clone()) } else { (a[0].clone(), a[1].trim().to_string(), a[2].clone()) };
        let Some(url) = server_url(&server) else {
            s.verdict(Verdict::Retry("That is not a server address.".into()));
            continue;
        };
        if user.is_empty() || password.is_empty() {
            s.verdict(Verdict::Retry("Fill in the name and the password.".into()));
            continue;
        }
        // The address as given, else its /.well-known/caldav.
        let mut code = match propfind(&url, &user, &password) {
            Ok(c) => c,
            Err(e) => {
                s.verdict(Verdict::Retry(e));
                continue;
            }
        };
        let mut caldav = url.clone();
        if !(200..300).contains(&code) && code != 401 && code != 403 {
            let wk = format!("{url}/.well-known/caldav");
            if let Ok(c) = propfind(&wk, &user, &password) {
                if (200..300).contains(&c) || c == 401 || c == 403 {
                    code = c;
                    caldav = wk;
                }
            }
        }
        match code {
            200..=299 => {}
            401 | 403 => {
                s.verdict(Verdict::Retry(if icloud { "Apple did not accept these. Use an app-specific password, not your Apple Account password.".into() } else { "The server did not accept the name and password.".into() }));
                continue;
            }
            c => {
                s.verdict(Verdict::Retry(format!("That does not look like a CalDAV server (it answered {c}).")));
                continue;
            }
        }
        match add_webdav_account(&url, &caldav, &user, &password) {
            Ok(()) => {
                s.verdict(Verdict::Done);
                refresh();
                tell("Calendar accounts", "Added. Its calendars appear in Prism within a minute; pick which ones show under Calendars\u{2026}.");
                return Ok(());
            }
            Err(e) => {
                s.verdict(Verdict::Retry(e));
            }
        }
    }
}

/// A GOA WebDAV account with only its calendars on.
fn add_webdav_account(url: &str, caldav: &str, user: &str, password: &str) -> Result<(), String> {
    let conn = Connection::session().map_err(|e| e.to_string())?;
    let p = Proxy::new(&conn, GOA, GOA_MANAGER, "org.gnome.OnlineAccounts.Manager").map_err(|e| e.to_string())?;
    let host = url.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or(url);
    let presentation = if host == "caldav.icloud.com" { user.to_string() } else { format!("{user} ({host})") };
    let mut credentials: HashMap<&str, Value> = HashMap::new();
    credentials.insert("password", Value::from(password));
    let details: HashMap<&str, &str> = [("Uri", url), ("CalDavUri", caldav), ("CalendarEnabled", "true"), ("ContactsEnabled", "false"), ("FilesEnabled", "false")].into_iter().collect();
    let _: OwnedObjectPath = p.call("AddAccount", &("webdav", user, presentation.as_str(), credentials, details)).map_err(|e| format!("GNOME Online Accounts did not add it: {e}"))?;
    Ok(())
}

// ── Calendars ────────────────────────────────────────────────────────────

fn calendars_flow() -> Result<(), String> {
    if !eds::available() {
        tell("Calendars", "Online calendars need evolution-data-server. Install it with: sudo pacman -S evolution-data-server gnome-online-accounts");
        return Ok(());
    }
    let client = eds::Client::new()?;
    loop {
        let cals = client.calendars()?;
        let cfg = Config::load();
        let default = cals.iter().position(|c| c.uid == cfg.calendar_default);
        let choices: Vec<Choice> = cals
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let shown = !cfg.calendar_hidden.contains(&c.uid);
                let mut detail = if c.account.is_empty() { "On this computer".to_string() } else { c.account.clone() };
                detail.push_str(if shown { " \u{b7} shown" } else { " \u{b7} hidden" });
                if default == Some(i) {
                    detail.push_str(" \u{b7} new events go here");
                }
                choice(&c.name, detail, if shown { "check" } else { "dot" })
            })
            .collect();
        let Some(i) = pick("Calendars", "Pick a calendar to show or hide it in Prism, or to put new events in it.", "", "Open", choices)? else { return Ok(()) };
        let c = &cals[i];
        let shown = !cfg.calendar_hidden.contains(&c.uid);
        let Some(a) = pick(&c.name, if c.account.is_empty() { "On this computer" } else { &c.account }, "", "Do it", vec![choice(if shown { "Hide in Prism" } else { "Show in Prism" }, "Its events on the Calendar widget and in reminders", "check"), choice("Put new events here", "The calendar the event card picks first", "plus")])? else { continue };
        let mut cfg = Config::load();
        if a == 0 {
            if shown {
                cfg.calendar_hidden.push(c.uid.clone());
            } else {
                cfg.calendar_hidden.retain(|u| *u != c.uid);
            }
        } else {
            cfg.calendar_default = c.uid.clone();
        }
        cfg.save().map_err(|e| format!("saving: {e}"))?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_addresses() {
        assert_eq!(server_url("caldav.fastmail.com").as_deref(), Some("https://caldav.fastmail.com"));
        assert_eq!(server_url("https://cloud.example.com/remote.php/dav/").as_deref(), Some("https://cloud.example.com/remote.php/dav"));
        assert_eq!(server_url(" "), None);
    }
}
