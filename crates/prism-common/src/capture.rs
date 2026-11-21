//! Screen recording job, shared by prism-ui (which builds it in the capture overlay), prismd
//! (which runs prism-rec with it) and prism-rec. See SCREEN_CAPTURE_PLAN.md.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecordTarget {
    /// A whole output, by wl_output name ("DP-2").
    Output { name: String },
    /// A window, by its ext-foreign-toplevel identifier (title for messages).
    Window { id: String, title: String },
    /// Part of an output, in that output's buffer pixels.
    Region { output: String, x: i32, y: i32, w: i32, h: i32 },
}

/// Screen sharing for the ScreenCast portal: `prism-rec --cast '<CastJob JSON>'`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct CastJob {
    /// Output name (wl_output name), or
    pub output: Option<String>,
    /// window identifier (ext-foreign-toplevel).
    pub window: Option<String>,
    pub cursor: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct RecordJob {
    pub target: RecordTarget,
    /// Frame rate cap.
    pub fps: u32,
    pub cursor: bool,
    pub audio_desktop: bool,
    pub audio_mic: bool,
    /// "auto", "h264", "hevc", "av1".
    pub codec: String,
    /// "low", "medium", "high", "lossless".
    pub quality: String,
    /// Output file (decided by prismd: folder + Recording_<time>.<container>). For a replay
    /// buffer: its folder and extension (saved clips are Replay_<time>.<ext> there).
    pub path: String,
    /// Replay buffer: keep this many seconds in memory and save them on SIGUSR1 (0 = record).
    #[serde(default)]
    pub replay_secs: u32,
}

/// Recording state as prismd reports it (`prism-ctl screenshot status`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct RecordStatus {
    pub recording: bool,
    pub path: String,
    /// Unix milliseconds when recording started.
    pub started_ms: i64,
    pub audio: bool,
    /// What is recorded (the indicator keeps out of the picture when it can).
    #[serde(default)]
    pub target: Option<RecordTarget>,
}

/// Local time as `YYYY-MM-DD_HH-MM-SS` (file names).
pub fn timestamp() -> String {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!("{:04}-{:02}-{:02}_{:02}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_round_trips() {
        let j = RecordJob {
            target: RecordTarget::Region { output: "DP-2".into(), x: 10, y: 20, w: 640, h: 360 },
            fps: 60,
            cursor: false,
            audio_desktop: true,
            audio_mic: false,
            codec: "auto".into(),
            quality: "medium".into(),
            path: "/tmp/a.mkv".into(),
            replay_secs: 0,
        };
        let s = serde_json::to_string(&j).unwrap();
        assert!(s.contains(r#""kind":"region""#));
        assert_eq!(serde_json::from_str::<RecordJob>(&s).unwrap(), j);
    }
}
