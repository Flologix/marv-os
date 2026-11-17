//! Kernel #41: a new launch never waits on a dead window. Each running desktop keeps a record,
//! `<data>/instances/<pid>.json` (its pid, its program's name and process start time, when it started, its version,
//! whether its watchdog runs), and touches it (mtime) every few seconds while its main thread answers the watchdog
//! (`watchdog`): a hidden window, or one whose page its own watchdog is reloading, still touches it. A new launch looks
//! at the others' records first:
//!
//! - **Gone**: the pid isn't running, or it's some other program or process now (a reused pid: its name or its start
//!   time differs): the record is removed.
//! - **Alive**: touched recently, or its watchdog is off (`MARV_WATCHDOG=off`: it never touches, so it's never taken
//!   over): nothing changes (two windows run side by side, as before).
//! - **Unresponsive**: running, but untouched for [`STALE`]: the launch watches its record a moment longer
//!   ([`CONFIRM`], a few touches' worth) in case it was only asleep; still untouched, the earlier window is closed
//!   (SIGTERM, which runs its own closing: stop notices, the normal exit; SIGKILL if it doesn't go) so its mesh doors
//!   and names are free, a hang report is written, and the person is told.
//!
//! The 2026-10-24 window had stopped accepting on its doors; the new launch's chats, named like the old one's, checked
//! their names with a blocking connect and hung with it (fixed in `marv_session::mesh_process`). Closing the old window
//! gives the new one its chats' names back.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Untouched this long while its process runs: the window's main thread stopped answering. Longer than its first
/// answer's wait, a reload's wait and two pings (`watchdog::Timings`), so a window that can help itself does.
pub const STALE: Duration = Duration::from_secs(90);
/// How long a launch watches a stale-looking record before closing its window (a laptop just woken: the window
/// touches it within one ping).
pub const CONFIRM: Duration = Duration::from_secs(8);
/// How long a closed window has after SIGTERM before SIGKILL: its closing waits up to 5 s for stops, 3 s for the turns
/// to end, then the normal exit.
pub const TERM_GRACE: Duration = Duration::from_secs(12);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub pid: u32,
    /// The program's file name (`marv-desktop`), so a reused pid isn't taken for a Marv.
    pub exe: String,
    pub started_ms: u64,
    pub version: String,
    /// Its process's start time (Linux: clock ticks after boot, `/proc/<pid>/stat`), so a reused pid running the same
    /// program isn't taken for it. None where unknown.
    #[serde(default)]
    pub start_ticks: Option<u64>,
    /// Whether its watchdog runs (and touches the record). A window without one is never taken over.
    #[serde(default = "yes")]
    pub watchdog: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Gone,
    Alive,
    Unresponsive,
}

/// What the launch needs from the machine (the tests give their own).
pub trait System {
    /// The wall clock (records' mtimes are wall times).
    fn now_ms(&self) -> u64;
    fn sleep(&self, d: Duration);
    fn alive(&self, pid: u32) -> bool;
    /// Whether `pid` runs a program named `exe`, started at `start_ticks` when that's known.
    fn is_marv(&self, pid: u32, exe: &str, start_ticks: Option<u64>) -> bool;
    /// Closes `pid` (SIGTERM, then SIGKILL): true once it's gone.
    fn terminate(&self, pid: u32) -> bool;
}

/// `record` (last touched at `touched_ms`) as of `now_ms`.
pub fn classify(record: &Record, touched_ms: u64, now_ms: u64, system: &dyn System) -> Owner {
    if !system.alive(record.pid) || !system.is_marv(record.pid, &record.exe, record.start_ticks) {
        return Owner::Gone;
    }
    if !record.watchdog {
        return Owner::Alive;
    }
    if now_ms.saturating_sub(touched_ms) > STALE.as_millis() as u64 { Owner::Unresponsive } else { Owner::Alive }
}

fn mtime_ms(path: &Path) -> Option<u64> {
    let m = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(m.duration_since(UNIX_EPOCH).ok()?.as_millis() as u64)
}

fn read(path: &Path) -> Option<Record> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// An earlier window this launch closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Closed {
    pub record: Record,
}

/// Looks at every other window's record in `dir` (never `own`'s, nor the pids in `skip`: this process's launcher):
/// removes the gone ones' and closes the unresponsive ones. What it closed.
pub fn take_over(dir: &Path, own: u32, skip: &[u32], system: &dyn System) -> Vec<Closed> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut closed = Vec::new();
    for path in entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")) {
        let Some(record) = read(&path) else {
            // Half-written or not ours: nothing a live window would keep.
            let _ = std::fs::remove_file(&path);
            continue;
        };
        if record.pid == own || skip.contains(&record.pid) {
            continue;
        }
        let touched = mtime_ms(&path).unwrap_or(0);
        match classify(&record, touched, system.now_ms(), system) {
            Owner::Alive => {}
            Owner::Gone => {
                let _ = std::fs::remove_file(&path);
            }
            Owner::Unresponsive => {
                if confirmed_stale(&path, touched, system) && system.terminate(record.pid) {
                    let _ = std::fs::remove_file(&path);
                    closed.push(Closed { record });
                }
            }
        }
    }
    closed
}

/// Whether the record stays untouched (and its process running) for [`CONFIRM`].
fn confirmed_stale(path: &Path, touched: u64, system: &dyn System) -> bool {
    let step = Duration::from_millis(500);
    let mut waited = Duration::ZERO;
    while waited < CONFIRM {
        system.sleep(step);
        waited += step;
        match mtime_ms(path) {
            Some(t) if t > touched => return false,
            None => return false, // removed: it closed on its own
            Some(_) => {}
        }
    }
    true
}

/// This window's own record, removed by [`Own::remove`] as the app exits (Tauri's state is never dropped).
#[derive(Debug)]
pub struct Own {
    path: PathBuf,
}

impl Own {
    /// Writes this process's record in `dir`; `watchdog`: whether this window's watchdog runs (touching it).
    pub fn register(dir: &Path, version: &str, watchdog: bool) -> std::io::Result<Own> {
        std::fs::create_dir_all(dir)?;
        let pid = std::process::id();
        let exe = std::env::current_exe().ok().and_then(|p| program_name(&p)).unwrap_or_default();
        let started_ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        let record = Record { pid, exe, started_ms, version: version.to_string(), start_ticks: start_ticks(pid), watchdog };
        let path = dir.join(format!("{pid}.json"));
        let tmp = dir.join(format!("{pid}.json.tmp"));
        std::fs::write(&tmp, serde_json::to_vec(&record).map_err(std::io::Error::other)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(Own { path })
    }

    /// "Still answering": a new launch leaves this window alone.
    pub fn touch(&self) {
        if let Ok(file) = std::fs::OpenOptions::new().write(true).open(&self.path) {
            let _ = file.set_modified(SystemTime::now());
        }
    }

    pub fn remove(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A program's file name, without the " (deleted)" Linux adds to a replaced binary's `/proc/<pid>/exe`.
fn program_name(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_string_lossy().to_string();
    Some(name.trim_end_matches(" (deleted)").to_string())
}

/// When `pid` started (Linux: field 22 of `/proc/<pid>/stat`, read after the command's closing parenthesis, which
/// may itself contain spaces). None elsewhere.
pub fn start_ticks(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let rest = &stat[stat.rfind(')')? + 1..];
        // After ")": state is field 3, so starttime (22) is the 20th.
        rest.split_whitespace().nth(19)?.parse().ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

/// The real machine.
pub struct Machine;

impl System for Machine {
    fn now_ms(&self) -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
    }

    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }

    #[cfg(windows)]
    fn alive(&self, pid: u32) -> bool {
        marv_sandbox::windows::alive(pid)
    }

    #[cfg(unix)]
    fn alive(&self, pid: u32) -> bool {
        let Ok(pid) = i32::try_from(pid) else { return false };
        // Signal 0: whether the process exists (EPERM: it does, someone else's).
        match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
            Ok(()) => true,
            Err(e) => e == nix::errno::Errno::EPERM,
        }
    }

    fn is_marv(&self, pid: u32, exe: &str, start: Option<u64>) -> bool {
        if exe.is_empty() {
            return false;
        }
        if let (Some(recorded), Some(now)) = (start, start_ticks(pid))
            && recorded != now
        {
            return false;
        }
        #[cfg(target_os = "linux")]
        {
            std::fs::read_link(format!("/proc/{pid}/exe")).ok().and_then(|p| program_name(&p)).is_some_and(|n| n == exe)
        }
        #[cfg(windows)]
        {
            marv_sandbox::windows::image_name(pid).and_then(|n| program_name(Path::new(&n))).is_some_and(|n| n == exe)
        }
        #[cfg(all(unix, not(target_os = "linux")))]
        {
            marv_core::process::command("ps")
                .args(["-p", &pid.to_string(), "-o", "comm="])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .and_then(|c| program_name(Path::new(&c)))
                .is_some_and(|n| n == exe)
        }
    }

    /// Windows: asked to close, then ended (`taskkill`, then `taskkill /F`).
    #[cfg(windows)]
    fn terminate(&self, pid: u32) -> bool {
        let gone_within = |d: Duration| {
            let until = std::time::Instant::now() + d;
            while std::time::Instant::now() < until {
                if !self.alive(pid) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            !self.alive(pid)
        };
        marv_sandbox::windows::ask_to_close(pid);
        if gone_within(TERM_GRACE) {
            return true;
        }
        marv_sandbox::windows::end_tree(pid);
        gone_within(Duration::from_secs(3))
    }

    #[cfg(unix)]
    fn terminate(&self, pid: u32) -> bool {
        use nix::sys::signal::{Signal, kill};
        let Ok(raw) = i32::try_from(pid) else { return false };
        let target = nix::unistd::Pid::from_raw(raw);
        let gone_within = |d: Duration| {
            let until = std::time::Instant::now() + d;
            while std::time::Instant::now() < until {
                if !self.alive(pid) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            !self.alive(pid)
        };
        // A stopped process takes SIGTERM only once continued. Its SIGTERM handler closes the normal way (stop
        // notices, rules revoked, the event log flushed), which can take several seconds.
        let _ = kill(target, Signal::SIGTERM);
        let _ = kill(target, Signal::SIGCONT);
        if gone_within(TERM_GRACE) {
            return true;
        }
        let _ = kill(target, Signal::SIGKILL);
        gone_within(Duration::from_secs(3))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::{BTreeMap, BTreeSet};

    /// A machine whose clock moves only by `sleep`, with the processes the test says run.
    struct Fake {
        now: Cell<u64>,
        running: RefCell<BTreeMap<u32, String>>,
        /// Start times the fake knows (pids not listed: unknown).
        starts: BTreeMap<u32, u64>,
        terminated: RefCell<BTreeSet<u32>>,
        /// Called on each sleep (a window touching its record meanwhile).
        on_sleep: Box<dyn Fn(u64)>,
    }

    impl Fake {
        fn new(now: u64, running: &[(u32, &str)]) -> Self {
            Fake {
                now: Cell::new(now),
                running: RefCell::new(running.iter().map(|(p, e)| (*p, e.to_string())).collect()),
                starts: BTreeMap::new(),
                terminated: RefCell::default(),
                on_sleep: Box::new(|_| {}),
            }
        }
    }

    impl System for Fake {
        fn now_ms(&self) -> u64 {
            self.now.get()
        }
        fn sleep(&self, d: Duration) {
            self.now.set(self.now.get() + d.as_millis() as u64);
            (self.on_sleep)(self.now.get());
        }
        fn alive(&self, pid: u32) -> bool {
            self.running.borrow().contains_key(&pid)
        }
        fn is_marv(&self, pid: u32, exe: &str, start: Option<u64>) -> bool {
            let same_start = match (start, self.starts.get(&pid)) {
                (Some(a), Some(b)) => a == *b,
                _ => true,
            };
            same_start && self.running.borrow().get(&pid).is_some_and(|e| e == exe)
        }
        fn terminate(&self, pid: u32) -> bool {
            self.terminated.borrow_mut().insert(pid);
            self.running.borrow_mut().remove(&pid);
            true
        }
    }

    struct Dir(PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn dir() -> Dir {
        let d = std::env::temp_dir().join(format!("marv-inst-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }

    /// A record for `pid`, last touched at `touched_ms` (wall).
    fn record(dir: &Path, pid: u32, exe: &str, touched_ms: u64) -> PathBuf {
        let path = dir.join(format!("{pid}.json"));
        let r = Record { pid, exe: exe.into(), started_ms: 1, version: "0.10.0".into(), start_ticks: None, watchdog: true };
        std::fs::write(&path, serde_json::to_vec(&r).unwrap()).unwrap();
        set_touched(&path, touched_ms);
        path
    }

    fn set_touched(path: &Path, ms: u64) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(UNIX_EPOCH + Duration::from_millis(ms)).unwrap();
    }

    const NOW: u64 = 1_800_000_000_000;

    #[test]
    fn owners_are_gone_alive_or_unresponsive() {
        let sys = Fake::new(NOW, &[(10, "marv-desktop"), (11, "bash")]);
        let r = |pid| Record {
            pid,
            exe: "marv-desktop".into(),
            started_ms: 0,
            version: String::new(),
            start_ticks: None,
            watchdog: true,
        };
        assert_eq!(classify(&r(9), NOW, NOW, &sys), Owner::Gone, "not running");
        assert_eq!(classify(&r(11), NOW, NOW, &sys), Owner::Gone, "a reused pid: another program");
        assert_eq!(classify(&r(10), NOW - 5_000, NOW, &sys), Owner::Alive);
        assert_eq!(classify(&r(10), NOW - STALE.as_millis() as u64, NOW, &sys), Owner::Alive, "at the edge");
        assert_eq!(classify(&r(10), NOW - 91_000, NOW, &sys), Owner::Unresponsive);
        // P1-1: a window whose watchdog is off never touches its record: never unresponsive, never closed.
        let off = Record { watchdog: false, ..r(10) };
        assert_eq!(classify(&off, 0, NOW, &sys), Owner::Alive);
        assert_eq!(classify(&Record { watchdog: false, ..r(9) }, 0, NOW, &sys), Owner::Gone, "but gone when gone");
    }

    /// P3-1: a reused pid running the same program, started at another time, is not the window that wrote the record.
    #[test]
    fn a_reused_pid_with_another_start_time_is_gone() {
        let mut sys = Fake::new(NOW, &[(10, "marv-desktop")]);
        sys.starts.insert(10, 777);
        let r = |start| Record {
            pid: 10,
            exe: "marv-desktop".into(),
            started_ms: 0,
            version: String::new(),
            start_ticks: start,
            watchdog: true,
        };
        assert_eq!(classify(&r(Some(776)), NOW, NOW, &sys), Owner::Gone);
        assert_eq!(classify(&r(Some(777)), NOW, NOW, &sys), Owner::Alive);
        assert_eq!(classify(&r(None), NOW, NOW, &sys), Owner::Alive, "unknown: the name decides");
        // An older record without the newer fields still reads (watchdog on, no start time).
        let old: Record = serde_json::from_str(r#"{"pid":1,"exe":"m","started_ms":2,"version":"v"}"#).unwrap();
        assert!(old.watchdog && old.start_ticks.is_none());
    }

    /// The real start time: this process's is known and stable; a child's differs from a later one's.
    #[cfg(target_os = "linux")]
    #[test]
    fn start_ticks_come_from_proc() {
        let me = std::process::id();
        assert!(start_ticks(me).is_some());
        assert_eq!(start_ticks(me), start_ticks(me));
        let ticks = start_ticks(me);
        assert!(Machine.is_marv(me, &program_name(&std::env::current_exe().unwrap()).unwrap(), ticks));
        assert!(!Machine.is_marv(me, &program_name(&std::env::current_exe().unwrap()).unwrap(), ticks.map(|t| t + 1)));
    }

    #[test]
    fn a_launch_removes_dead_records_closes_hung_windows_and_leaves_live_ones() {
        let d = dir();
        let sys = Fake::new(NOW, &[(20, "marv-desktop"), (21, "marv-desktop"), (22, "firefox")]);
        let dead = record(&d.0, 19, "marv-desktop", NOW - 1000);
        let live = record(&d.0, 20, "marv-desktop", NOW - 2000);
        let hung = record(&d.0, 21, "marv-desktop", NOW - 3_600_000);
        let reused = record(&d.0, 22, "marv-desktop", NOW - 3_600_000);
        std::fs::write(d.0.join("junk.json"), "{").unwrap();
        let closed = take_over(&d.0, 99, &[], &sys);
        assert_eq!(closed.iter().map(|c| c.record.pid).collect::<Vec<_>>(), [21]);
        assert_eq!(*sys.terminated.borrow(), BTreeSet::from([21]), "only the hung Marv is closed, never firefox");
        assert!(!dead.exists() && !hung.exists() && !reused.exists() && !d.0.join("junk.json").exists());
        assert!(live.exists());
        assert!(sys.now_ms() - NOW >= CONFIRM.as_millis() as u64, "watched a while before closing");
    }

    #[test]
    fn a_window_that_touches_its_record_meanwhile_was_only_asleep() {
        let d = dir();
        let mut sys = Fake::new(NOW, &[(30, "marv-desktop")]);
        let path = record(&d.0, 30, "marv-desktop", NOW - 3_600_000);
        let touched = path.clone();
        // It wakes 2 s into the watch and touches its record.
        sys.on_sleep = Box::new(move |now| {
            if now >= NOW + 2000 {
                set_touched(&touched, now);
            }
        });
        assert!(take_over(&d.0, 99, &[], &sys).is_empty());
        assert!(sys.terminated.borrow().is_empty() && path.exists());
    }

    #[test]
    fn its_own_record_and_its_launcher_are_never_looked_at() {
        let d = dir();
        let sys = Fake::new(NOW, &[(40, "marv-desktop"), (41, "marv-desktop")]);
        record(&d.0, 40, "marv-desktop", NOW - 3_600_000);
        record(&d.0, 41, "marv-desktop", NOW - 3_600_000);
        assert!(take_over(&d.0, 40, &[41], &sys).is_empty());
        assert!(sys.terminated.borrow().is_empty());
    }

    #[test]
    fn a_missing_folder_is_no_others() {
        let sys = Fake::new(NOW, &[]);
        assert!(take_over(Path::new("/nonexistent/marv-instances"), 1, &[], &sys).is_empty());
    }

    #[test]
    fn its_own_record_is_written_touched_and_removed() {
        let d = dir();
        let own = Own::register(&d.0, "0.10.0", true).unwrap();
        let path = d.0.join(format!("{}.json", std::process::id()));
        let r = read(&path).unwrap();
        assert_eq!((r.pid, r.version.as_str()), (std::process::id(), "0.10.0"));
        assert!(!r.exe.is_empty());
        // The real machine knows this process for what it is.
        assert!(r.watchdog);
        assert!(Machine.alive(r.pid) && Machine.is_marv(r.pid, &r.exe, r.start_ticks));
        assert!(!Machine.is_marv(r.pid, "something-else", r.start_ticks));
        let now = Machine.now_ms();
        set_touched(&path, now - 3_600_000);
        own.touch();
        assert!(mtime_ms(&path).unwrap() > now - 60_000);
        own.remove();
        assert!(!path.exists());
    }

    /// P3-5: a record goes stale only after the window's own watchdog had its full say: its first answer's wait,
    /// a reload's wait and two pings.
    #[test]
    fn stale_outlasts_the_windows_own_watchdog() {
        let t = crate::watchdog::Timings::default();
        assert!(STALE > t.first + t.after_reload + 2 * t.every, "{STALE:?}");
        assert!(STALE > t.timeout, "a main thread late past the timeout stops touching first");
    }

    #[test]
    fn a_replaced_binary_keeps_its_name() {
        assert_eq!(program_name(Path::new("/usr/bin/marv-desktop (deleted)")).as_deref(), Some("marv-desktop"));
    }

    /// The real machine closes a stopped process (what a hung window can be): SIGTERM once continued, else SIGKILL.
    #[cfg(unix)] // SIGSTOP and `sleep`
    #[test]
    fn the_machine_closes_a_stopped_process() {
        let mut child = std::process::Command::new("sleep").arg("60").spawn().unwrap();
        let pid = child.id();
        let raw = nix::unistd::Pid::from_raw(pid as i32);
        nix::sys::signal::kill(raw, nix::sys::signal::Signal::SIGSTOP).unwrap();
        // Reaped by a thread, so "gone" means gone (a zombie still answers signal 0).
        let reaper = std::thread::spawn(move || child.wait());
        assert!(Machine.terminate(pid));
        assert!(reaper.join().unwrap().is_ok());
    }
}
