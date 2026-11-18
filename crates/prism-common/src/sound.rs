//! Notification sounds: Prism's built-in chime or a file the user chose, played through
//! whichever command line player the system has.
//!
//! An idle output needs time to wake, and whatever plays meanwhile is lost: a Bluetooth speaker
//! has to bring back its link and open its amplifier. So on an idle output the sound is played
//! as one stream that begins with a lead-in: a tone too low to hear, which wakes the output.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

static CHIME: &[u8] = include_bytes!("../../../data/notification.wav");
const CHIME_FILE: &str = "prism-notification.wav";

/// A long file chosen as the sound is cut off after this.
const LONGEST: Duration = Duration::from_secs(10);
/// How long a sleeping output gets to wake before the sound itself begins.
const BLUETOOTH_LEAD: Duration = Duration::from_millis(1200);
const WIRED_LEAD: Duration = Duration::from_millis(300);
const RATE: u32 = 44_100;
/// The lead-in: below hearing, about -32 dBFS so that an amplifier notices it.
const LEAD_HZ: f64 = 20.0;
const LEAD_PEAK: f64 = 800.0;
const LEAD_FADE_IN_MS: f64 = 150.0;
const LEAD_FADE_OUT_MS: f64 = 40.0;

const PLAYERS: [(&str, &[&str]); 3] = [("pw-play", &["--media-role=Notification"]), ("paplay", &["--property=media.role=event"]), ("aplay", &["-q"])];

/// A sound's name for lists: "message-new-instant.oga" is "Message New Instant".
pub fn label(path: &str) -> String {
    let stem = std::path::Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    stem.split(['-', '_', ' '])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
}

/// The built-in chime as a file the players can open.
fn chime_path() -> Option<PathBuf> {
    let dir = runtime_dir();
    let path = dir.join(CHIME_FILE);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() == CHIME.len() as u64) {
        return Some(path);
    }
    let tmp = dir.join(format!("prism-notification.{}.tmp", std::process::id()));
    std::fs::write(&tmp, CHIME).ok()?;
    std::fs::rename(&tmp, &path).ok()?;
    Some(path)
}

/// The tone that wakes an output, fading in and out so that it never clicks.
fn lead_in(lead: Duration) -> Vec<i16> {
    let count = (lead.as_secs_f64() * RATE as f64) as usize;
    (0..count)
        .map(|i| {
            let t = i as f64 / RATE as f64;
            let left = (count - i) as f64 / RATE as f64;
            let envelope = (t * 1000.0 / LEAD_FADE_IN_MS).min(left * 1000.0 / LEAD_FADE_OUT_MS).min(1.0);
            (LEAD_PEAK * envelope * (std::f64::consts::TAU * LEAD_HZ * t).sin()) as i16
        })
        .collect()
}

/// A mono 16-bit WAV file.
fn wav_file(samples: &[i16]) -> Vec<u8> {
    let data = samples.len() as u32 * 2;
    let mut wav = Vec::with_capacity(44 + data as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&(RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data.to_le_bytes());
    samples.iter().for_each(|v| wav.extend_from_slice(&v.to_le_bytes()));
    wav
}

fn samples_of(pcm: &[u8]) -> Vec<i16> {
    pcm.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect()
}

/// `file` as samples at `RATE`, through ffmpeg.
fn decode(file: &std::path::Path) -> Option<Vec<i16>> {
    let seconds = LONGEST.as_secs().to_string();
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-t", &seconds, "-i"])
        .arg(file)
        .args(["-ac", "1", "-ar", &RATE.to_string(), "-f", "s16le", "-"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    (out.status.success() && !out.stdout.is_empty()).then(|| samples_of(&out.stdout))
}

/// The sound with its lead-in, as a file. Kept until another sound or lead-in replaces it.
fn with_lead_in(custom: &str, lead: Duration) -> Option<PathBuf> {
    let chosen = sound_file(custom)?;
    let mut hash = DefaultHasher::new();
    (&chosen, lead.as_millis()).hash(&mut hash);
    std::fs::metadata(&chosen).ok()?.modified().ok()?.hash(&mut hash);
    let dir = runtime_dir();
    let name = format!("prism-sound-{:016x}.wav", hash.finish());
    let path = dir.join(&name);
    if path.is_file() {
        return Some(path);
    }
    let sound = if custom.trim().is_empty() || chosen.file_name() == Some(CHIME_FILE.as_ref()) { samples_of(&CHIME[44..]) } else { decode(&chosen)? };
    let mut samples = lead_in(lead);
    samples.extend(sound);
    for old in std::fs::read_dir(&dir).ok()?.flatten() {
        if old.file_name().to_string_lossy().starts_with("prism-sound-") {
            let _ = std::fs::remove_file(old.path());
        }
    }
    let tmp = dir.join(format!("{name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, wav_file(&samples)).ok()?;
    std::fs::rename(&tmp, &path).ok()?;
    Some(path)
}

fn pactl(args: &[&str]) -> Option<String> {
    let out = Command::new("pactl").args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The state of sink `name` in the output of `pactl list short sinks`.
fn sink_state(list: &str, name: &str) -> Option<String> {
    list.lines().find_map(|l| {
        let fields: Vec<&str> = l.split('\t').collect();
        (fields.get(1) == Some(&name)).then(|| fields.last().map(|s| s.trim().to_string())).flatten()
    })
}

/// The device address in a Bluetooth sink's name, as BlueZ writes it in object paths.
fn bluetooth_address(sink: &str) -> Option<&str> {
    sink.strip_prefix("bluez_output.")?.split('.').next().filter(|a| !a.is_empty())
}

/// Whether an output in `state` needs waking before a sound. Something already playing keeps
/// any output awake. Otherwise a Bluetooth speaker's amplifier has muted, whatever its link does.
fn needs_waking(state: Option<&str>, bluetooth: bool) -> bool {
    match state {
        Some("RUNNING") => false,
        Some("SUSPENDED") => true,
        _ => bluetooth,
    }
}

/// How long the default output needs to wake; zero when it can play at once.
fn wake_time() -> Duration {
    let Some(sink) = pactl(&["get-default-sink"]).map(|s| s.trim().to_string()) else { return Duration::ZERO };
    let state = pactl(&["list", "short", "sinks"]).and_then(|l| sink_state(&l, &sink));
    let bluetooth = bluetooth_address(&sink).is_some();
    match (needs_waking(state.as_deref(), bluetooth), bluetooth) {
        (false, _) => Duration::ZERO,
        (true, true) => BLUETOOTH_LEAD,
        (true, false) => WIRED_LEAD,
    }
}

/// `custom` with a leading `~/` expanded.
pub fn expand(custom: &str, home: &str) -> PathBuf {
    match custom.strip_prefix("~/") {
        Some(rest) => PathBuf::from(home).join(rest),
        None => PathBuf::from(custom),
    }
}

/// The file to play: the chosen one while it exists, else the built-in chime.
fn sound_file(custom: &str) -> Option<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let chosen = expand(custom.trim(), &home);
    if !custom.trim().is_empty() && chosen.is_file() {
        return Some(chosen);
    }
    chime_path()
}

/// Run one player to the end of the sound; true when it played.
fn run_player(program: &str, args: &[&str], file: &PathBuf) -> bool {
    let spawned = Command::new(program).args(args).arg(file).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    let Ok(mut child) = spawned else { return false };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if start.elapsed() >= LONGEST + BLUETOOTH_LEAD => {
                let _ = child.kill();
                let _ = child.wait();
                return true;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return false,
        }
    }
}

/// Play the notification sound in the background. `custom` is the chosen file, empty for the
/// built-in chime. Returns how long after now the sound itself begins.
pub fn play(custom: &str) -> Duration {
    let lead = wake_time();
    let custom = custom.to_string();
    std::thread::spawn(move || {
        let padded = (!lead.is_zero()).then(|| with_lead_in(&custom, lead)).flatten();
        let Some(file) = padded.or_else(|| sound_file(&custom)) else { return };
        if !PLAYERS.iter().any(|(program, args)| run_player(program, args, &file)) {
            eprintln!("[prism] cannot play {}", file.display());
        }
    });
    lead
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chime_is_a_short_wav_file() {
        assert_eq!(&CHIME[..4], b"RIFF");
        assert_eq!(&CHIME[8..12], b"WAVE");
        assert!(CHIME.len() < 128 * 1024);
    }

    #[test]
    fn the_lead_in_is_low_and_never_clicks() {
        let lead = lead_in(BLUETOOTH_LEAD);
        assert_eq!(lead.len(), (BLUETOOTH_LEAD.as_secs_f64() * RATE as f64) as usize);
        assert!(lead.iter().all(|v| (*v as f64).abs() <= LEAD_PEAK));
        assert!(lead.iter().map(|v| v.abs()).max().unwrap() as f64 > LEAD_PEAK * 0.9);
        assert!(lead[0].abs() < 2 && lead[lead.len() - 1].abs() < 2);
        // One step between samples stays tiny: a 20 Hz tone has no audible edge.
        assert!(lead.windows(2).all(|w| (w[1] as i32 - w[0] as i32).abs() < 6));
    }

    #[test]
    fn the_chime_can_follow_a_lead_in() {
        assert_eq!(u32::from_le_bytes([CHIME[24], CHIME[25], CHIME[26], CHIME[27]]), RATE);
        assert_eq!(u16::from_le_bytes([CHIME[22], CHIME[23]]), 1);
        assert_eq!(&CHIME[36..40], b"data");
        let wav = wav_file(&samples_of(&CHIME[44..]));
        assert_eq!(wav, CHIME);
    }

    #[test]
    fn idle_bluetooth_speakers_are_woken_and_busy_outputs_are_not() {
        assert!(!needs_waking(Some("RUNNING"), true));
        assert!(needs_waking(Some("IDLE"), true));
        assert!(needs_waking(Some("SUSPENDED"), true));
        assert!(needs_waking(Some("SUSPENDED"), false));
        assert!(!needs_waking(Some("IDLE"), false));
        assert!(!needs_waking(None, false));
    }

    #[test]
    fn bluetooth_outputs_are_recognised_by_name() {
        assert_eq!(bluetooth_address("bluez_output.34_00_00_5C_F4_DD.1"), Some("34_00_00_5C_F4_DD"));
        assert_eq!(bluetooth_address("alsa_output.pci-0000_2d_00.4.analog-stereo"), None);
    }

    #[test]
    fn sink_states_are_read_from_pactl() {
        let list = "51\talsa_output.hdmi\tPipeWire\ts32le 2ch 48000Hz\tSUSPENDED\n70\tbluez_output.x.1\tPipeWire\ts16le 2ch 44100Hz\tRUNNING\n";
        assert_eq!(sink_state(list, "bluez_output.x.1").as_deref(), Some("RUNNING"));
        assert_eq!(sink_state(list, "alsa_output.hdmi").as_deref(), Some("SUSPENDED"));
        assert_eq!(sink_state(list, "other"), None);
    }

    #[test]
    fn sounds_are_named_after_their_files() {
        assert_eq!(label("/usr/share/sounds/message-new-instant.oga"), "Message New Instant");
        assert_eq!(label("~/Music/my_cool sound.wav"), "My Cool Sound");
    }

    #[test]
    fn paths_under_home_expand() {
        assert_eq!(expand("~/a.ogg", "/home/u"), PathBuf::from("/home/u/a.ogg"));
        assert_eq!(expand("/x/a.ogg", "/home/u"), PathBuf::from("/x/a.ogg"));
    }
}
