//! Depth effect worker (port of QML DepthService): keeps a foreground mask for the current
//! wallpaper and publishes its path in `~/.cache/prism/depth-mask.txt`, which the desktop
//! widgets watch. Masks come from `scripts/wallpaper-depth.py` (cached by the script).
//!
//! The wallpaper picker in prism-ui sets wallpapers directly, so the current wallpaper is
//! polled (one small file read every 2 s) instead of being pushed here.

use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use prism_common::depth::{self, DepthParams};
use prism_common::Config;

enum Msg {
    Config(Box<Config>),
    /// Generate for the current wallpaper even when automatic generation is off.
    Generate,
}

pub struct Depth {
    tx: mpsc::Sender<Msg>,
}

impl Depth {
    pub fn start(cfg: &Config) -> Depth {
        let (tx, rx) = mpsc::channel();
        let cfg = cfg.clone();
        thread::spawn(move || run(cfg, rx));
        Depth { tx }
    }

    pub fn update_config(&self, cfg: &Config) {
        let _ = self.tx.send(Msg::Config(Box::new(cfg.clone())));
    }

    pub fn generate(&self) {
        let _ = self.tx.send(Msg::Generate);
    }
}

/// What a published mask was made from.
#[derive(Clone, PartialEq)]
struct Key {
    wallpaper: PathBuf,
    params: DepthParams,
}

fn expand_home(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rest),
        None => PathBuf::from(p),
    }
}

fn run(mut cfg: Config, rx: mpsc::Receiver<Msg>) {
    let mut published: Option<Key> = None;
    let mut failed: Option<Key> = None;
    // Background pre-generation of the whole wallpaper folder, with what it was started for.
    let mut pregen: Option<(Child, PathBuf, DepthParams)> = None;
    loop {
        let mut force = false;
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Msg::Config(c)) => cfg = *c,
            Ok(Msg::Generate) => force = true,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        // Coalesce queued messages.
        while let Ok(m) = rx.try_recv() {
            match m {
                Msg::Config(c) => cfg = *c,
                Msg::Generate => force = true,
            }
        }

        if !cfg.depth_enabled {
            if published.take().is_some() || depth::current_mask().is_some() {
                eprintln!("[prismd/depth] off");
            }
            depth::publish_mask(None);
            stop_pregen(&mut pregen);
            failed = None;
            continue;
        }

        let params = DepthParams::from_config(&cfg);
        let Some(wallpaper) = prism_common::wallpaper::get_current_wallpaper() else { continue };
        let key = Key { wallpaper, params: params.clone() };
        let stale = published.as_ref() != Some(&key);
        if stale && published.as_ref().is_some_and(|p| p.wallpaper != key.wallpaper) {
            // The old mask belongs to another picture: drop it right away.
            depth::publish_mask(None);
        }
        let wanted = force || (cfg.depth_auto_generate && stale && failed.as_ref() != Some(&key));
        if wanted {
            eprintln!("[prismd/depth] generating mask for {}", key.wallpaper.display());
            match depth::generate_mask(&key.wallpaper, &params) {
                Ok(mask) => {
                    eprintln!("[prismd/depth] mask ready: {}", mask.display());
                    depth::publish_mask(Some(&mask));
                    published = Some(key.clone());
                    failed = None;
                }
                Err(e) => {
                    eprintln!("[prismd/depth] {e}");
                    failed = Some(key.clone());
                }
            }
        } else if stale && published.is_some() && !cfg.depth_auto_generate {
            depth::publish_mask(None);
            published = None;
        }

        // Pre-generate the rest of the folder once the current wallpaper has its mask.
        let dir = expand_home(&cfg.wallpaper_dir);
        let want_pregen = cfg.depth_pregenerate && published.is_some() && dir.is_dir();
        let running_for = pregen.as_mut().and_then(|(c, d, p)| match c.try_wait() {
            Ok(None) => Some((d.clone(), p.clone())),
            _ => None,
        });
        match (want_pregen, running_for) {
            (true, Some((d, p))) if d == dir && p == params => {}
            (true, _) if !pregen_done(&pregen, &dir, &params) => {
                stop_pregen(&mut pregen);
                if let Some(mut c) = depth::pregenerate_command(&dir, &params) {
                    c.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
                    match c.spawn() {
                        Ok(child) => {
                            eprintln!("[prismd/depth] pre-generating masks in {}", dir.display());
                            pregen = Some((child, dir, params));
                        }
                        Err(e) => eprintln!("[prismd/depth] pre-generation failed to start: {e}"),
                    }
                }
            }
            (false, Some(_)) => stop_pregen(&mut pregen),
            _ => {}
        }
    }
}

/// True if a finished pre-generation already covered this folder and settings.
fn pregen_done(pregen: &Option<(Child, PathBuf, DepthParams)>, dir: &Path, params: &DepthParams) -> bool {
    pregen.as_ref().is_some_and(|(_, d, p)| d == dir && p == params)
}

fn stop_pregen(pregen: &mut Option<(Child, PathBuf, DepthParams)>) {
    if let Some((mut c, _, _)) = pregen.take() {
        if let Ok(None) = c.try_wait() {
            let _ = c.kill();
        }
        let _ = c.wait();
    }
}
