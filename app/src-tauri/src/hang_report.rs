//! Kernel #41: a small local report each time the watchdog acts (`watchdog`, `instance`), in the app's data folder
//! (`<data>/reports/hang-<ms>.json`, 0600). Settings › About counts them and opens the folder. Nothing is uploaded
//! (crash reports the person opts into come later, #46). The newest [`KEEP`] are kept.
//!
//! What a report holds is fixed by [`HangReport`]: the version, the time, what fired, the uptime and counts. Never a
//! message's text, a project's path, a chat's title or a key: the test below fails if a field is added unseen.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::watchdog::Fired;

/// How many reports are kept (the oldest go).
pub const KEEP: usize = 50;

/// What the watchdog did about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Acted {
    /// The page was reloaded.
    Reloaded,
    /// Only noted: the app kept running as it was.
    Reported,
    /// The app exited (nothing was running).
    Exited,
    /// A new launch closed this earlier window.
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HangReport {
    /// The window's version (the closed window's, for a stale one).
    pub version: String,
    /// When it fired (ms since the epoch).
    pub time_ms: u64,
    pub fired: Fired,
    pub acted: Acted,
    /// How long the window had run.
    pub uptime_s: u64,
    /// The chats saved in the app.
    pub chats: usize,
    /// Page reloads the watchdog made this run.
    pub reloads: u32,
    /// `linux`, `macos`.
    pub os: String,
}

pub fn dir(data: &Path) -> PathBuf {
    data.join("reports")
}

/// Writes `report` in `<data>/reports/`.
pub fn write(data: &Path, report: &HangReport) -> std::io::Result<PathBuf> {
    use marv_fs::mode::CreateMode;
    let dir = dir(data);
    std::fs::DirBuilder::new().recursive(true).create_mode(0o700).create(&dir)?;
    let mut path = dir.join(format!("hang-{}.json", report.time_ms));
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("hang-{}-{n}.json", report.time_ms));
        n += 1;
    }
    let text = serde_json::to_vec_pretty(report).map_err(std::io::Error::other)?;
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).create_mode(0o600).open(&path)?;
    std::io::Write::write_all(&mut file, &text)?;
    prune(&dir);
    Ok(path)
}

/// `hang-<ms>[-n].json` → (ms, n), for ordering.
fn order(name: &str) -> Option<(u64, u64)> {
    let stem = name.strip_prefix("hang-")?.strip_suffix(".json")?;
    let (ms, n) = stem.split_once('-').unwrap_or((stem, "0"));
    Some((ms.parse().ok()?, n.parse().ok()?))
}

/// Removes all but the newest [`KEEP`] reports.
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut reports: Vec<((u64, u64), PathBuf)> =
        entries.flatten().filter_map(|e| Some((order(&e.file_name().to_string_lossy())?, e.path()))).collect();
    if reports.len() <= KEEP {
        return;
    }
    reports.sort();
    for (_, path) in &reports[..reports.len() - KEEP] {
        let _ = std::fs::remove_file(path);
    }
}

/// How many reports there are.
pub fn count(data: &Path) -> usize {
    std::fs::read_dir(dir(data)).map_or(0, |entries| {
        entries
            .flatten()
            .filter(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                name.starts_with("hang-") && name.ends_with(".json")
            })
            .count()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn report(time_ms: u64) -> HangReport {
        HangReport {
            version: "0.10.0".into(),
            time_ms,
            fired: Fired::HeartbeatTimeout,
            acted: Acted::Reloaded,
            uptime_s: 3600,
            chats: 4,
            reloads: 1,
            os: std::env::consts::OS.into(),
        }
    }

    #[test]
    fn reports_are_private_files_counted_in_the_data_folder() {
        let data = std::env::temp_dir().join(format!("marv-reports-{}", uuid::Uuid::new_v4().simple()));
        assert_eq!(count(&data), 0);
        let a = write(&data, &report(5)).unwrap();
        let b = write(&data, &report(5)).unwrap();
        assert_ne!(a, b, "same millisecond: two files");
        std::fs::write(dir(&data).join("notes.txt"), "").unwrap();
        assert_eq!(count(&data), 2);
        #[cfg(unix)] // Windows has no mode bits
        {
            assert_eq!(std::fs::metadata(&a).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(std::fs::metadata(dir(&data)).unwrap().permissions().mode() & 0o777, 0o700);
        }
        let back: HangReport = serde_json::from_slice(&std::fs::read(&a).unwrap()).unwrap();
        assert_eq!(back, report(5));
        let _ = std::fs::remove_dir_all(&data);
    }

    /// P3-7: the newest 50 are kept, ordered by their time (not by name: 9 < 10).
    #[test]
    fn only_the_newest_reports_are_kept() {
        let data = std::env::temp_dir().join(format!("marv-reports-{}", uuid::Uuid::new_v4().simple()));
        for ms in 1..=(KEEP as u64 + 5) {
            write(&data, &report(ms)).unwrap();
        }
        assert_eq!(count(&data), KEEP);
        for gone in 1..=5 {
            assert!(!dir(&data).join(format!("hang-{gone}.json")).exists(), "{gone}");
        }
        assert!(dir(&data).join("hang-6.json").exists() && dir(&data).join("hang-55.json").exists());
        assert_eq!(order("hang-12-3.json"), Some((12, 3)));
        assert_eq!(order("notes.json"), None);
        let _ = std::fs::remove_dir_all(&data);
    }

    /// A report holds these fields and nothing else: no text, paths or keys. Adding one means looking at it here.
    #[test]
    fn a_report_holds_only_versions_times_and_counts() {
        let value = serde_json::to_value(report(1)).unwrap();
        let mut keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["acted", "chats", "fired", "os", "reloads", "time_ms", "uptime_s", "version"]);
        assert_eq!(value["fired"], "heartbeat_timeout");
        assert_eq!(serde_json::to_value(Fired::StaleInstance).unwrap(), "stale_instance");
        assert_eq!(serde_json::to_value(Fired::WebProcessGone).unwrap(), "web_process_gone");
        assert_eq!(serde_json::to_value(Fired::MainThreadStuck).unwrap(), "main_thread_stuck");
        assert_eq!(value["acted"], "reloaded");
    }
}
