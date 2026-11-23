//! The update notice: Marv OS says when a newer release is out, with a link to the download page. Nothing is
//! downloaded or installed here; the person updates as they installed.
//!
//! - **The check:** at most once a day, GitHub's `releases/latest` page of the releases repository is asked for its
//!   redirect (not followed), which names the latest release's tag (`os-v1.0.1`). Prereleases are never "latest", so
//!   a release candidate isn't offered. No identifier, no usage data: GitHub sees the request and the IP, like anyone
//!   opening the releases page.
//! - **Off:** Settings › About, `MARV_NO_UPDATE_CHECK` (any value) or `CI` in the environment, and debug builds (their
//!   version is the workspace's, not a release's).
//! - The last answer is kept in the app's data folder (`update-check.json`), so a launch within the day asks nothing.

use std::path::{Path, PathBuf};
use std::time::Duration;

use semver::Version;
use serde::{Deserialize, Serialize};

/// The latest release's page; its redirect names the tag.
pub const LATEST_URL: &str = "https://github.com/Flologix/marv-os-releases/releases/latest";
/// Where the bar's Download button goes: the site picks the right file for the person's system.
pub const DOWNLOAD_URL: &str = "https://www.flologixai.com/marv-os";
/// A day between checks.
const FRESH_SECS: u64 = 24 * 60 * 60;
const TIMEOUT: Duration = Duration::from_secs(10);

/// A newer release, for the page's bar.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Update {
    pub version: String,
    /// Its release notes.
    pub notes_url: String,
    pub download_url: String,
}

/// Whether the check may run: not with `MARV_NO_UPDATE_CHECK` or `CI` set, nor in a debug build.
pub fn enabled(setting_on: bool, env: impl Fn(&str) -> Option<String>, debug: bool) -> bool {
    setting_on && !debug && env("MARV_NO_UPDATE_CHECK").is_none() && env("CI").is_none()
}

/// The version a release tag names (`os-v1.0.1`, or the URL ending in it). `None` for anything else.
pub fn tag_version(location: &str) -> Option<Version> {
    let tag = location.trim_end_matches('/').rsplit('/').next()?;
    Version::parse(tag.strip_prefix("os-v")?).ok()
}

/// `latest` when it's newer than `current` (this build's version). A build whose version doesn't parse is offered
/// nothing.
pub fn newer(current: &str, latest: &Version) -> Option<Update> {
    let current = Version::parse(current).ok()?;
    (latest > &current).then(|| Update {
        version: latest.to_string(),
        notes_url: format!("https://github.com/Flologix/marv-os-releases/releases/tag/os-v{latest}"),
        download_url: DOWNLOAD_URL.to_string(),
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Checked {
    /// Seconds since the epoch.
    at: u64,
    latest: String,
}

fn cache_path(data: &Path) -> PathBuf {
    data.join("update-check.json")
}

fn load(data: &Path) -> Option<Checked> {
    serde_json::from_str(&std::fs::read_to_string(cache_path(data)).ok()?).ok()
}

fn save(data: &Path, checked: &Checked) {
    if let Ok(text) = serde_json::to_string(checked) {
        let _ = std::fs::create_dir_all(data);
        let _ = std::fs::write(cache_path(data), text);
    }
}

/// The latest release's version from GitHub: the `releases/latest` redirect's tag. `None` on any failure.
async fn fetch_latest(url: &str, current: &str) -> Option<Version> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(TIMEOUT)
        .connect_timeout(TIMEOUT)
        .user_agent(format!("Marv-OS/{current}"))
        .build()
        .ok()?;
    let response = client.get(url).send().await.ok()?;
    let location = response.headers().get(reqwest::header::LOCATION)?.to_str().ok()?;
    tag_version(location)
}

/// The latest release's version: the kept answer while it's under a day old, else asked again (and kept). A failed
/// check isn't kept, so the next one asks again.
pub async fn latest(data: &Path, url: &str, current: &str, now: u64) -> Option<Version> {
    if let Some(checked) = load(data).filter(|c| now.saturating_sub(c.at) < FRESH_SECS) {
        return Version::parse(&checked.latest).ok();
    }
    let latest = fetch_latest(url, current).await?;
    save(data, &Checked { at: now, latest: latest.to_string() });
    Some(latest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_tag_names_its_version() {
        let v = |s: &str| tag_version(s).map(|v| v.to_string());
        assert_eq!(v("https://github.com/Flologix/marv-os-releases/releases/tag/os-v1.0.1").as_deref(), Some("1.0.1"));
        assert_eq!(v("os-v1.2.0-rc.1").as_deref(), Some("1.2.0-rc.1"));
        assert_eq!(v("https://github.com/Flologix/marv-os-releases/releases/tag/os-v1.0.1/").as_deref(), Some("1.0.1"));
        // No release yet (GitHub sends the list) or another tag: nothing.
        assert_eq!(v("https://github.com/Flologix/marv-os-releases/releases"), None);
        assert_eq!(v("https://github.com/Flologix/marv-os-releases/releases/tag/v0.16.5"), None);
    }

    #[test]
    fn only_a_newer_release_is_offered() {
        let latest = Version::parse("1.0.1").unwrap();
        let update = newer("1.0.0", &latest).unwrap();
        assert_eq!(update.version, "1.0.1");
        assert_eq!(update.notes_url, "https://github.com/Flologix/marv-os-releases/releases/tag/os-v1.0.1");
        assert!(newer("1.0.1", &latest).is_none());
        assert!(newer("1.1.0", &latest).is_none());
        // A release candidate is offered its release.
        assert!(newer("1.0.1-rc.1", &latest).is_some());
        assert!(newer("not a version", &latest).is_none());
    }

    #[test]
    fn off_by_setting_environment_or_debug_build() {
        let none = |_: &str| None;
        assert!(enabled(true, none, false));
        assert!(!enabled(false, none, false));
        assert!(!enabled(true, none, true));
        assert!(!enabled(true, |k: &str| (k == "MARV_NO_UPDATE_CHECK").then(|| "1".into()), false));
        assert!(!enabled(true, |k: &str| (k == "CI").then(|| "true".into()), false));
    }

    #[tokio::test]
    async fn a_fresh_answer_is_used_without_asking() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &Checked { at: 1_000, latest: "1.0.3".into() });
        // The URL can't answer: only the kept answer can give 1.0.3.
        let got = latest(dir.path(), "http://127.0.0.1:9/never", "1.0.0", 1_000 + FRESH_SECS - 1).await;
        assert_eq!(got.map(|v| v.to_string()).as_deref(), Some("1.0.3"));
        // A day later it asks again, and a failed check gives nothing.
        assert_eq!(latest(dir.path(), "http://127.0.0.1:9/never", "1.0.0", 1_000 + FRESH_SECS).await, None);
    }
}
