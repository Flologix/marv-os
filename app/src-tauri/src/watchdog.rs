//! Kernel #41: the window's liveness. A desktop window hung for days once (2026-10-24): its WebKit web process had
//! died, the window said "not responding", and nothing noticed.
//!
//! **The rule: a watchdog never stops a person's running turns over a false positive.** When in doubt it writes a
//! report and keeps running. What it does:
//!
//! - Every [`Timings::every`] the app pings the page (a script round-trip through the web view that answers
//!   `document.visibilityState`) and, separately, the app's GTK main thread (`run_on_main_thread`). Silence is
//!   measured from the **oldest unanswered** ping, so a late tick under load never counts as silence.
//! - **The main thread late** is not a page hang: nothing is reloaded. After [`Timings::main_report`] a report is
//!   written; after [`Timings::main_exit`] the app exits, but only when no chat has a turn running (the driver checks).
//!   Meanwhile its instance record isn't touched, so a new launch the person starts can take the window over.
//! - **The page silent** while visible, past [`Timings::timeout`] ([`Timings::first`] before its first answer), and
//!   corroborated (on Linux: WebKitGTK says the web process isn't responsive), or **the web process gone**, or WebKit
//!   saying it's unresponsive for the timeout (also while hidden): the page is reloaded, again with backoff
//!   ([`Timings::after_reload`] doubling up to [`Timings::backoff_max`]) while it doesn't come back. Never an exit.
//!   Silence that WebKit doesn't corroborate is only reported. While the window is hidden or minimized, silence isn't
//!   counted at all (WebKit may throttle it).
//! - After [`Timings::healthy_reset`] healthy, the backoff starts over.
//!
//! [`Heartbeat`] is the state machine alone, its clock given (milliseconds of a monotonic clock, which doesn't run
//! while the machine sleeps, so a resume never looks like a hang). The app drives it from a task (`lib.rs`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// What made the watchdog act (in a hang report).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fired {
    /// The page didn't answer a ping for [`Timings::timeout`] while visible.
    HeartbeatTimeout,
    /// The web view's web process ended (crashed, killed, out of memory).
    WebProcessGone,
    /// WebKitGTK said the web process stopped responding.
    WebProcessUnresponsive,
    /// The app's main thread didn't run a probe for [`Timings::main_report`].
    MainThreadStuck,
    /// A new launch found an earlier window that stopped responding, and closed it (`instance`).
    StaleInstance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timings {
    /// How often the page and the main thread are pinged.
    pub every: Duration,
    /// No answer for this long (visible): the page is silent. The main thread late this long: it's the main thread.
    pub timeout: Duration,
    /// Before the page's first answer (it's still loading at start).
    pub first: Duration,
    /// After a reload, how long before another; doubled each time, up to [`Timings::backoff_max`].
    pub after_reload: Duration,
    pub backoff_max: Duration,
    /// Healthy this long: the backoff starts over.
    pub healthy_reset: Duration,
    /// The main thread stuck this long: a report.
    pub main_report: Duration,
    /// The main thread stuck this long: the app exits if no turn runs.
    pub main_exit: Duration,
    /// Whether the platform can say the web process is unresponsive (WebKitGTK); without it, silence is enough.
    pub corroborate: bool,
    /// Silent (visible) this long without WebKit agreeing: reloaded anyway. WebKitGTK only notices an unresponsive
    /// process when it sends it something that wants a reply (input, a reload): a SIGSTOPped web process stayed
    /// "responsive" in the manual check. A reload never touches a chat's turn, so this is safe to do late.
    pub silent_reload: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Self {
            every: Duration::from_secs(3),
            timeout: Duration::from_secs(20),
            first: Duration::from_secs(45),
            after_reload: Duration::from_secs(30),
            backoff_max: Duration::from_secs(600),
            healthy_reset: Duration::from_secs(600),
            main_report: Duration::from_secs(120),
            main_exit: Duration::from_secs(600),
            corroborate: cfg!(target_os = "linux"),
            silent_reload: Duration::from_secs(120),
        }
    }
}

impl Timings {
    /// The defaults, with `MARV_WATCHDOG_TIMEOUT_S` (for trying the watchdog by hand) shortening the timeouts. None
    /// when `MARV_WATCHDOG=off`, and in a debug build unless `MARV_WATCHDOG=on`.
    pub fn from_env(get: impl Fn(&str) -> Option<String>, debug: bool) -> Option<Self> {
        let set = get("MARV_WATCHDOG");
        if set.as_deref().is_some_and(|v| v == "off" || v == "0") {
            return None;
        }
        if debug && !set.as_deref().is_some_and(|v| v == "on" || v == "1") {
            return None;
        }
        let mut t = Self::default();
        if let Some(secs) = get("MARV_WATCHDOG_TIMEOUT_S").and_then(|v| v.parse::<u64>().ok()).filter(|s| *s >= 2) {
            let s = Duration::from_secs(secs);
            t.timeout = s;
            t.first = s;
            t.after_reload = s.max(Duration::from_secs(10));
            t.every = Duration::from_secs((secs / 4).clamp(1, 3));
            t.main_report = s * 3;
            t.main_exit = s * 6;
            t.silent_reload = s * 6;
        }
        Some(t)
    }
}

/// What the driver does after a [`Heartbeat::tick`] (it pings both every tick regardless).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Nothing,
    /// Reload the page (a report is written).
    Reload(Fired),
    /// Write a report and keep running.
    Report(Fired),
    /// Exit the normal way if no chat has a turn running; otherwise keep running.
    ExitIfIdle(Fired),
}

#[derive(Debug)]
pub struct Heartbeat {
    t: Timings,
    /// The oldest page ping not answered yet.
    page_pending: Option<u64>,
    /// The oldest main-thread probe not run yet.
    main_pending: Option<u64>,
    answered_once: bool,
    visible: bool,
    gone: bool,
    unresponsive_since: Option<u64>,
    /// When the last reload was, and the earliest the next may be.
    reloaded_at: Option<u64>,
    next_reload_at: u64,
    level: u32,
    reloads: u32,
    healthy_since: Option<u64>,
    main_reported: bool,
    main_exit_asked: bool,
    silence_reported: bool,
}

fn ms(d: Duration) -> u64 {
    d.as_millis() as u64
}

impl Heartbeat {
    pub fn new(t: Timings) -> Self {
        Self {
            t,
            page_pending: None,
            main_pending: None,
            answered_once: false,
            visible: true,
            gone: false,
            unresponsive_since: None,
            reloaded_at: None,
            next_reload_at: 0,
            level: 0,
            reloads: 0,
            healthy_since: None,
            main_reported: false,
            main_exit_asked: false,
            silence_reported: false,
        }
    }

    /// Reloads so far this run.
    pub fn reloads(&self) -> u32 {
        self.reloads
    }

    pub fn page_pinged(&mut self, now: u64) {
        self.page_pending.get_or_insert(now);
    }

    /// The page answered, saying whether it's visible (its web process evidently runs: one that died was replaced).
    pub fn page_answered(&mut self, visible: bool) {
        self.page_pending = None;
        self.gone = false;
        self.answered_once = true;
        self.visible = visible;
        self.silence_reported = false;
    }

    pub fn main_probed(&mut self, now: u64) {
        self.main_pending.get_or_insert(now);
    }

    pub fn main_answered(&mut self) {
        self.main_pending = None;
        self.main_reported = false;
        self.main_exit_asked = false;
    }

    pub fn web_process_gone(&mut self) {
        self.gone = true;
    }

    /// WebKitGTK's `is-web-process-responsive`, as it changes.
    pub fn web_responsive(&mut self, responsive: bool, now: u64) {
        if responsive {
            self.unresponsive_since = None;
        } else {
            self.unresponsive_since.get_or_insert(now);
        }
    }

    fn main_late(&self, now: u64) -> u64 {
        self.main_pending.map_or(0, |p| now.saturating_sub(p))
    }

    /// The main thread answers (its record may be touched: the window can still help itself, a hidden one included).
    pub fn alive(&self, now: u64) -> bool {
        self.main_late(now) <= ms(self.t.timeout)
    }

    /// One look, every [`Timings::every`].
    pub fn tick(&mut self, now: u64) -> Action {
        // The main thread late: never the page's fault, never a reload.
        let main_late = self.main_late(now);
        if main_late > ms(self.t.timeout) {
            self.healthy_since = None;
            if main_late > ms(self.t.main_exit) && !self.main_exit_asked {
                self.main_exit_asked = true;
                return Action::ExitIfIdle(Fired::MainThreadStuck);
            }
            if main_late > ms(self.t.main_report) && !self.main_reported {
                self.main_reported = true;
                return Action::Report(Fired::MainThreadStuck);
            }
            return Action::Nothing;
        }
        let limit = if self.answered_once { self.t.timeout } else { self.t.first };
        let silent = self.visible && self.page_pending.is_some_and(|p| now.saturating_sub(p) > ms(limit));
        let unresponsive = self.unresponsive_since.is_some_and(|u| now.saturating_sub(u) > ms(self.t.timeout));
        let long_silent = self.visible && self.page_pending.is_some_and(|p| now.saturating_sub(p) > ms(self.t.silent_reload));
        let corroborated = !self.t.corroborate || self.unresponsive_since.is_some() || long_silent;
        let why = if self.gone {
            Some(Fired::WebProcessGone)
        } else if unresponsive {
            Some(Fired::WebProcessUnresponsive)
        } else if silent && corroborated {
            Some(Fired::HeartbeatTimeout)
        } else {
            None
        };
        let Some(why) = why else {
            if silent {
                // Silent, but WebKit says its process answers: not sure enough to touch the page.
                self.healthy_since = None;
                if !self.silence_reported {
                    self.silence_reported = true;
                    return Action::Report(Fired::HeartbeatTimeout);
                }
                return Action::Nothing;
            }
            let fresh = self.page_pending.is_none_or(|p| now.saturating_sub(p) <= ms(self.t.timeout));
            if fresh && self.answered_once {
                let since = *self.healthy_since.get_or_insert(now);
                if now.saturating_sub(since) >= ms(self.t.healthy_reset) {
                    self.level = 0;
                }
            }
            return Action::Nothing;
        };
        self.healthy_since = None;
        if now < self.next_reload_at {
            return Action::Nothing;
        }
        self.level += 1;
        self.reloads += 1;
        let backoff = self.t.after_reload.saturating_mul(1 << (self.level - 1).min(16)).min(self.t.backoff_max);
        self.next_reload_at = now + ms(backoff);
        self.reloaded_at = Some(now);
        // The new page is measured afresh.
        self.gone = false;
        self.page_pending = None;
        self.unresponsive_since = None;
        Action::Reload(why)
    }
}

/// What a chat stopped as the app was closed (a takeover's SIGTERM, a logout) is told when it next loads.
pub const STOPPED_TEXT: &str = "Marv was closed while this chat's turn ran, so the turn was stopped. The conversation is kept up to there: write to continue.";

/// What the person is told when a new launch closed an earlier window that had stopped responding.
pub const TAKEN_OVER_TEXT: &str = "An earlier Marv window stopped responding; it was closed.";

/// What a desktop notification says when the page is reloaded again (the window itself can't say it).
pub const RELOADING_TEXT: &str = "Marv's window stopped responding again; reloading it. Your chats keep running.";

/// Notices for chats kept across a restart (`<data>/notices.json`): the stop notices, said in each chat when it next
/// loads in the next launch.
fn notices_file(data: &Path) -> PathBuf {
    data.join("notices.json")
}

/// One read-change-write of the notices file at a time in this process.
static NOTICES: Mutex<()> = Mutex::new(());

/// Adds `notices` to the saved ones (written whole to a temp file, then renamed over: never half a file).
pub fn save_notices(data: &Path, notices: &HashMap<String, String>) -> std::io::Result<()> {
    if notices.is_empty() {
        return Ok(());
    }
    let _held = NOTICES.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = read_notices(data);
    all.extend(notices.iter().map(|(k, v)| (k.clone(), v.clone())));
    std::fs::create_dir_all(data)?;
    let text = serde_json::to_vec(&all).map_err(std::io::Error::other)?;
    let tmp = data.join(format!("notices.json.{}.{}.tmp", std::process::id(), uuid::Uuid::new_v4().simple()));
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(&text)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, notices_file(data)).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

fn read_notices(data: &Path) -> HashMap<String, String> {
    std::fs::read(notices_file(data)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// The saved notices, removed from disk (they're now the app's to tell).
pub fn take_notices(data: &Path) -> HashMap<String, String> {
    let _held = NOTICES.lock().unwrap_or_else(|e| e.into_inner());
    let all = read_notices(data);
    let _ = std::fs::remove_file(notices_file(data));
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 1000;

    fn t(corroborate: bool) -> Timings {
        Timings { corroborate, ..Timings::default() }
    }

    /// A driver tick: both pinged, then the look.
    fn tick(hb: &mut Heartbeat, now: u64) -> Action {
        hb.page_pinged(now);
        hb.main_probed(now);
        hb.tick(now)
    }

    /// Pings answered at once for `secs` seconds from `from`, every 3 s; every action seen.
    fn healthy(hb: &mut Heartbeat, from: u64, secs: u64) -> Vec<Action> {
        let mut seen = Vec::new();
        let mut now = from;
        while now < from + secs * S {
            let a = tick(hb, now);
            if a != Action::Nothing {
                seen.push(a);
            }
            hb.page_answered(true);
            hb.main_answered();
            now += 3 * S;
        }
        seen
    }

    #[test]
    fn an_answering_page_and_main_thread_do_nothing() {
        let mut hb = Heartbeat::new(t(true));
        assert!(healthy(&mut hb, 0, 600).is_empty());
        assert!(hb.alive(600 * S));
        assert_eq!(hb.reloads(), 0);
    }

    /// P2-1: silence counts from the oldest unanswered ping. Ticks that come late (a loaded machine) never make a
    /// gap into silence: the ping wasn't sent either.
    #[test]
    fn late_ticks_are_not_silence() {
        let mut hb = Heartbeat::new(t(false));
        healthy(&mut hb, 0, 30);
        // The next tick comes 40 s late; its ping is answered right after.
        assert_eq!(tick(&mut hb, 70 * S), Action::Nothing);
        hb.page_answered(true);
        hb.main_answered();
        assert_eq!(tick(&mut hb, 73 * S), Action::Nothing);
    }

    #[test]
    fn a_silent_visible_page_is_reloaded_only_when_webkit_agrees() {
        let mut hb = Heartbeat::new(t(true));
        healthy(&mut hb, 0, 30);
        // The page stops answering; the main thread doesn't.
        let mut now = 30 * S;
        let mut seen = Vec::new();
        while now < 90 * S {
            let a = tick(&mut hb, now);
            hb.main_answered();
            if a != Action::Nothing {
                seen.push((now / S, a));
            }
            now += 3 * S;
        }
        assert_eq!(seen, [(51, Action::Report(Fired::HeartbeatTimeout))], "WebKit says it answers: only a report");
        // WebKit agrees: reloaded.
        let mut agrees = Heartbeat::new(t(true));
        healthy(&mut agrees, 0, 30);
        tick(&mut agrees, 30 * S);
        agrees.main_answered();
        agrees.web_responsive(false, 31 * S);
        assert_eq!(tick(&mut agrees, 51 * S), Action::Reload(Fired::HeartbeatTimeout));
        assert_eq!(agrees.reloads(), 1);
        // WebKit never agrees, but the page stays silent for 2 minutes: reloaded then (a reload stops no turn).
        while now <= 160 * S {
            let a = tick(&mut hb, now);
            hb.main_answered();
            if a != Action::Nothing {
                assert_eq!((now / S, a), (153, Action::Reload(Fired::HeartbeatTimeout)));
                break;
            }
            now += 3 * S;
        }
        assert_eq!(hb.reloads(), 1);
    }

    #[test]
    fn without_a_webkit_signal_silence_is_enough_for_a_reload() {
        let mut hb = Heartbeat::new(t(false));
        healthy(&mut hb, 0, 30);
        let mut now = 30 * S;
        loop {
            let a = tick(&mut hb, now);
            hb.main_answered();
            if a != Action::Nothing {
                assert_eq!((now / S, a), (51, Action::Reload(Fired::HeartbeatTimeout)));
                break;
            }
            now += 3 * S;
        }
    }

    /// P2-3: a hidden page's silence isn't counted; a gone or unresponsive web process still is.
    #[test]
    fn a_hidden_page_is_judged_only_by_webkit() {
        let mut hb = Heartbeat::new(t(false));
        hb.page_pinged(0);
        hb.page_answered(false);
        for i in 1..200 {
            assert_eq!(tick(&mut hb, i * 3 * S), Action::Nothing, "hidden: silence isn't counted");
            hb.main_answered();
            assert!(hb.alive(i * 3 * S), "and its record is touched: never taken over");
        }
        hb.web_responsive(false, 600 * S);
        assert_eq!(tick(&mut hb, 610 * S), Action::Nothing, "unresponsive, not yet for the timeout");
        hb.main_answered();
        assert_eq!(tick(&mut hb, 621 * S), Action::Reload(Fired::WebProcessUnresponsive));
    }

    #[test]
    fn the_first_answer_gets_the_longer_wait() {
        let mut hb = Heartbeat::new(t(false));
        for now in [0, 15 * S, 30 * S, 45 * S] {
            assert_eq!(tick(&mut hb, now), Action::Nothing);
            hb.main_answered();
        }
        assert_eq!(tick(&mut hb, 46 * S), Action::Reload(Fired::HeartbeatTimeout));
    }

    /// P2-4/P2-5: a dead web process is reloaded at once, again with backoff while it stays dead, never an exit; after
    /// 10 minutes healthy the backoff starts over.
    #[test]
    fn a_dying_page_is_reloaded_with_backoff_and_never_exits() {
        let mut hb = Heartbeat::new(t(true));
        healthy(&mut hb, 0, 10);
        let mut reloads = Vec::new();
        let mut now = 10 * S;
        while now < 1600 * S {
            hb.web_process_gone();
            let a = tick(&mut hb, now);
            hb.main_answered();
            match a {
                Action::Reload(Fired::WebProcessGone) => reloads.push(now / S - 10),
                Action::Nothing => {}
                other => panic!("never {other:?}"),
            }
            now += S;
        }
        // 0, then 30, 60, 120, 240, 480 s apart, then 600 (the cap).
        assert_eq!(reloads, [0, 30, 90, 210, 450, 930, 1530]);
        // Healthy for 10 minutes (the reloaded page answers): the next reload comes at once and the one after 30 s
        // later again.
        hb.page_answered(true);
        let back = healthy(&mut hb, now + 600 * S, 610);
        assert!(back.is_empty());
        let now = now + 1300 * S;
        hb.web_process_gone();
        assert_eq!(tick(&mut hb, now), Action::Reload(Fired::WebProcessGone));
        hb.main_answered();
        hb.web_process_gone();
        assert_eq!(tick(&mut hb, now + 29 * S), Action::Nothing);
        hb.main_answered();
        assert_eq!(tick(&mut hb, now + 30 * S), Action::Reload(Fired::WebProcessGone));
    }

    /// P2-2: the main thread late is never a page hang: no reload (even with the page silent), a report after 2
    /// minutes, then an exit only if idle (asked once), and its record isn't touched meanwhile.
    #[test]
    fn a_stuck_main_thread_is_reported_never_reloaded() {
        let mut hb = Heartbeat::new(t(false));
        healthy(&mut hb, 0, 30);
        let mut seen = Vec::new();
        let mut now = 30 * S;
        while now < 700 * S {
            // The page dies once the main thread is already late.
            if now >= 60 * S {
                hb.web_process_gone();
            }
            let a = tick(&mut hb, now);
            if a != Action::Nothing {
                seen.push((now / S, a));
            }
            now += 3 * S;
        }
        assert_eq!(
            seen,
            [(153, Action::Report(Fired::MainThreadStuck)), (633, Action::ExitIfIdle(Fired::MainThreadStuck))],
            "no reload while the main thread is late"
        );
        assert!(!hb.alive(now));
        // It comes back: a page that died meanwhile is reloaded now.
        hb.main_answered();
        assert!(hb.alive(now));
        assert_eq!(tick(&mut hb, now), Action::Reload(Fired::WebProcessGone));
    }

    #[test]
    fn the_environment_turns_it_off_or_shortens_it() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
        };
        assert_eq!(Timings::from_env(env(&[]), false), Some(Timings::default()));
        assert_eq!(Timings::from_env(env(&[("MARV_WATCHDOG", "off")]), false), None);
        // P3-2: off in a debug build unless asked for.
        assert_eq!(Timings::from_env(env(&[]), true), None);
        assert_eq!(Timings::from_env(env(&[("MARV_WATCHDOG", "on")]), true), Some(Timings::default()));
        let t = Timings::from_env(env(&[("MARV_WATCHDOG_TIMEOUT_S", "8")]), false).unwrap();
        assert_eq!((t.timeout, t.first, t.every), (Duration::from_secs(8), Duration::from_secs(8), Duration::from_secs(2)));
        assert_eq!(Timings::from_env(env(&[("MARV_WATCHDOG_TIMEOUT_S", "x")]), false), Some(Timings::default()));
    }

    #[test]
    fn notices_survive_a_restart_once() {
        let data = std::env::temp_dir().join(format!("marv-notices-{}", uuid::Uuid::new_v4().simple()));
        assert!(take_notices(&data).is_empty());
        save_notices(&data, &HashMap::new()).unwrap();
        assert!(!notices_file(&data).exists(), "nothing to save: no file");
        save_notices(&data, &HashMap::from([("a".to_string(), STOPPED_TEXT.to_string())])).unwrap();
        save_notices(&data, &HashMap::from([("b".to_string(), "x".to_string())])).unwrap();
        let leftovers = std::fs::read_dir(&data).unwrap().count();
        assert_eq!(leftovers, 1, "no temp files left");
        let got = take_notices(&data);
        assert_eq!((got.len(), got["a"].as_str()), (2, STOPPED_TEXT));
        assert!(take_notices(&data).is_empty(), "told once");
        let _ = std::fs::remove_dir_all(&data);
    }
}
