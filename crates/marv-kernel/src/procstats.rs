//! Process stats for the processes Marv started itself (runtime-kernel §20.9, "real processes, nested under their
//! agent"): CPU time, resident memory, start time and whether it's alive. New in marv-rs.
//!
//! **One portable layer.** Feature code never reads `/proc` itself: it asks [`stats`]. On Linux that reads
//! `/proc/<pid>/stat` and `/proc/<pid>/status`; on macOS (and the BSDs) it runs `ps -o rss=,time=,etime= -p <pid>`,
//! the same columns on both; anywhere else every answer is [`Stats::unknown`]. Chosen over the `sysinfo` and
//! `libproc` crates: no new dependency, no build-time bindgen (libproc's), no `unsafe` anywhere (CLAUDE.md), and the
//! `ps` path is the same program on Linux, so its parser is tested here too. A refresh is a few PIDs every couple of
//! seconds, so one `ps` per PID is cheap enough. The future `marv kernel serve` (#39) shares it.
//!
//! **Only Marv's own PIDs** (§20.9, guard `proc_reads_only_marvs_own_pids`). A PID is read only after the code that
//! spawned it said so ([`own`]): the MCP client when it starts a local server, the process manager when it starts a
//! dev server. A PID a peer, a model or a message names is never read: [`stats`] answers [`Refused`] for any PID not
//! in the set. A PID that ended and was reused by an unrelated process is caught by its start time: a process that
//! started after Marv took the PID as its own isn't the one it started ([`Refused`] too). A process's children (a
//! dev server's `node` under `npm`, under the sandbox) are summed into it ([`tree_stats`]): they are what Marv
//! started, one level down.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// What's known about one process. Every number is `None` where the platform doesn't say.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Stats {
    /// User + system CPU time, in milliseconds, since it started.
    pub cpu_ms: Option<u64>,
    /// Resident memory, in kilobytes.
    pub rss_kb: Option<u64>,
    /// When it started, ms since the Unix epoch.
    pub started_ms: Option<u64>,
    /// `None`: this platform can't tell.
    pub alive: Option<bool>,
}

impl Stats {
    /// A platform with no way to tell.
    pub const fn unknown() -> Self {
        Self { cpu_ms: None, rss_kb: None, started_ms: None, alive: None }
    }

    const fn gone() -> Self {
        Self { cpu_ms: None, rss_kb: None, started_ms: None, alive: Some(false) }
    }
}

/// [`stats`] for a PID Marv never said it started (or one reused since).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refused;

/// An own PID: when Marv took it (ms since the epoch), and on Linux its process's start time in clock ticks then.
#[derive(Debug, Clone, Copy)]
struct Owned {
    taken_ms: u64,
    start_ticks: Option<u64>,
}

fn owned() -> &'static Mutex<HashMap<u32, Owned>> {
    static OWN: OnceLock<Mutex<HashMap<u32, Owned>>> = OnceLock::new();
    OWN.get_or_init(Mutex::default)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// Marv just started `pid` (the spawn site calls this with the child's own id, never a number from elsewhere).
pub fn own(pid: u32) {
    own_started(pid, start_ticks(pid));
}

/// [`own`] with the start time (Linux clock ticks) the process had when Marv took it. For tests of the reuse check:
/// a PID now held by a process with another start time isn't the one Marv started.
#[doc(hidden)]
pub fn own_started(pid: u32, start_ticks: Option<u64>) {
    if pid > 1 {
        owned().lock().unwrap_or_else(|e| e.into_inner()).insert(pid, Owned { taken_ms: now_ms(), start_ticks });
    }
}

/// The process's start time in clock ticks since boot (Linux: `/proc/<pid>/stat` field 22); `None` elsewhere. Exact,
/// so unlike a wall-clock comparison it isn't fooled by the clock stepping.
fn start_ticks(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        linux::parse_stat(&stat).map(|(_, _, start)| start)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

/// `pid` ended (or Marv let it go): it's no longer read.
pub fn release(pid: u32) {
    owned().lock().unwrap_or_else(|e| e.into_inner()).remove(&pid);
}

/// Whether `pid` is one Marv started and still holds.
pub fn is_own(pid: u32) -> bool {
    owned().lock().unwrap_or_else(|e| e.into_inner()).contains_key(&pid)
}

/// A start time this much after the PID was taken is another process's (clock ticks and `ps` round to a second).
const START_SLACK_MS: u64 = 2_000;

/// What's known about `pid`, one of Marv's own ([`own`]); [`Refused`] for any other PID.
pub fn stats(pid: u32) -> Result<Stats, Refused> {
    let Some(taken) = owned().lock().unwrap_or_else(|e| e.into_inner()).get(&pid).copied() else {
        return Err(Refused);
    };
    // A reused PID: on Linux, its start time in ticks isn't the one recorded (exact); elsewhere it started after
    // Marv took the PID (wall clock, with slack).
    if let Some(ticks) = taken.start_ticks {
        if start_ticks(pid).is_some_and(|now| now != ticks) {
            return Err(Refused);
        }
        return Ok(read(pid));
    }
    let stats = read(pid);
    if stats.started_ms.is_some_and(|started| started > taken.taken_ms + START_SLACK_MS) {
        return Err(Refused);
    }
    Ok(stats)
}

/// [`stats`] for `pid` with its descendants' CPU and memory added (Linux; elsewhere `pid`'s own).
pub fn tree_stats(pid: u32) -> Result<Stats, Refused> {
    let mut total = stats(pid)?;
    if total.alive != Some(true) {
        return Ok(total);
    }
    for child in descendants(pid) {
        let s = read(child);
        if s.alive == Some(true) {
            total.cpu_ms = add(total.cpu_ms, s.cpu_ms);
            total.rss_kb = add(total.rss_kb, s.rss_kb);
        }
    }
    Ok(total)
}

fn add(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
    }
}

/// CPU % between two samples of one process: its CPU time's growth over the wall time between them (100 = one core).
pub fn cpu_percent(before: (u64, Stats), after: (u64, Stats)) -> Option<u32> {
    let (t0, s0) = before;
    let (t1, s1) = after;
    let wall = t1.checked_sub(t0).filter(|w| *w > 0)?;
    let used = s1.cpu_ms?.checked_sub(s0.cpu_ms?)?;
    Some(((used as f64 / wall as f64) * 100.0).round() as u32)
}

/// Remembers each PID's last sample, so a refresh can give CPU %.
#[derive(Debug, Default)]
pub struct Sampler {
    last: HashMap<u32, (u64, Stats)>,
}

/// One process as a refresh sees it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Sample {
    pub rss_kb: Option<u64>,
    /// Since the previous sample; `None` on the first.
    pub cpu_pct: Option<u32>,
    pub alive: Option<bool>,
}

impl Sampler {
    /// `pid` (one of Marv's own, with its descendants) now; [`Refused`] for any other.
    pub fn sample(&mut self, pid: u32, now_ms: u64) -> Result<Sample, Refused> {
        let stats = tree_stats(pid)?;
        let cpu_pct = self.last.get(&pid).and_then(|before| cpu_percent(*before, (now_ms, stats)));
        self.last.insert(pid, (now_ms, stats));
        Ok(Sample { rss_kb: stats.rss_kb, cpu_pct, alive: stats.alive })
    }

    /// Forgets the PIDs not in `keep`.
    pub fn retain(&mut self, keep: &[u32]) {
        self.last.retain(|pid, _| keep.contains(pid));
    }
}

#[cfg(target_os = "linux")]
fn read(pid: u32) -> Stats {
    linux::read(pid)
}

#[cfg(target_os = "linux")]
fn descendants(pid: u32) -> Vec<u32> {
    linux::descendants(pid)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn read(pid: u32) -> Stats {
    ps::read(pid)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn descendants(_pid: u32) -> Vec<u32> {
    Vec::new()
}

#[cfg(not(unix))]
fn read(_pid: u32) -> Stats {
    Stats::unknown()
}

#[cfg(not(unix))]
fn descendants(_pid: u32) -> Vec<u32> {
    Vec::new()
}

#[cfg(target_os = "linux")]
mod linux {
    use super::Stats;

    /// USER_HZ: the unit of `/proc/<pid>/stat`'s times. 100 on every architecture Linux ships for user space (the
    /// kernel's HZ is hidden behind it), so it isn't asked of `sysconf` (which would need libc).
    const TICKS_PER_S: u64 = 100;

    /// utime, stime and starttime (ticks) from `/proc/<pid>/stat`. The command name in parentheses may hold spaces
    /// and parentheses, so fields are counted from the last `)`.
    pub(super) fn parse_stat(text: &str) -> Option<(u64, u64, u64)> {
        let rest = &text[text.rfind(')')? + 1..];
        let fields: Vec<&str> = rest.split_whitespace().collect();
        // After the name: state is field 3, so field n is fields[n - 3].
        let field = |n: usize| fields.get(n - 3)?.parse::<u64>().ok();
        Some((field(14)?, field(15)?, field(22)?))
    }

    /// `VmRSS` from `/proc/<pid>/status`, in kB.
    pub(super) fn parse_rss(text: &str) -> Option<u64> {
        let line = text.lines().find(|l| l.starts_with("VmRSS:"))?;
        line.split_whitespace().nth(1)?.parse().ok()
    }

    fn boot_ms() -> Option<u64> {
        let text = std::fs::read_to_string("/proc/stat").ok()?;
        let line = text.lines().find(|l| l.starts_with("btime "))?;
        line.split_whitespace().nth(1)?.parse::<u64>().ok().map(|s| s * 1000)
    }

    pub(super) fn read(pid: u32) -> Stats {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { return Stats::gone() };
        // A zombie has ended: its numbers are what it left.
        let zombie = stat.rfind(')').is_some_and(|i| stat[i + 1..].trim_start().starts_with('Z'));
        let times = parse_stat(&stat);
        let rss = std::fs::read_to_string(format!("/proc/{pid}/status")).ok().and_then(|s| parse_rss(&s));
        Stats {
            cpu_ms: times.map(|(u, s, _)| (u + s) * 1000 / TICKS_PER_S),
            rss_kb: rss,
            started_ms: times.and_then(|(_, _, start)| Some(boot_ms()? + start * 1000 / TICKS_PER_S)),
            alive: Some(!zombie),
        }
    }

    /// Every process under `pid`, from each of its threads' `children` list.
    pub(super) fn descendants(pid: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut queue = vec![pid];
        while let Some(p) = queue.pop() {
            let Ok(tasks) = std::fs::read_dir(format!("/proc/{p}/task")) else { continue };
            for task in tasks.flatten() {
                let Ok(text) = std::fs::read_to_string(task.path().join("children")) else { continue };
                for child in text.split_whitespace().filter_map(|c| c.parse::<u32>().ok()) {
                    if !out.contains(&child) && out.len() < 256 {
                        out.push(child);
                        queue.push(child);
                    }
                }
            }
        }
        out
    }
}

/// `ps -o rss=,time=,etime= -p <pid>`: macOS's and the BSDs' way (and Linux procps', so it's tested there too).
#[cfg(unix)]
#[cfg_attr(target_os = "linux", allow(dead_code))]
mod ps {
    use super::Stats;

    /// `[[dd-]hh:]mm:ss[.cc]` in ms.
    pub(super) fn duration_ms(text: &str) -> Option<u64> {
        let (days, clock) = match text.split_once('-') {
            Some((d, rest)) => (d.parse::<u64>().ok()?, rest),
            None => (0, text),
        };
        let mut secs = 0f64;
        for part in clock.split(':') {
            secs = secs * 60.0 + part.parse::<f64>().ok()?;
        }
        Some(days * 86_400_000 + (secs * 1000.0).round() as u64)
    }

    /// One `rss= time= etime=` line, read at `now_ms`.
    pub(super) fn parse(line: &str, now_ms: u64) -> Option<Stats> {
        let mut parts = line.split_whitespace();
        let rss = parts.next()?.parse::<u64>().ok()?;
        let cpu = duration_ms(parts.next()?)?;
        let elapsed = duration_ms(parts.next()?)?;
        Some(Stats { cpu_ms: Some(cpu), rss_kb: Some(rss), started_ms: now_ms.checked_sub(elapsed), alive: Some(true) })
    }

    pub(super) fn read(pid: u32) -> Stats {
        // The system's own ps, by its absolute path, with a clean environment: never one a project or a PATH entry
        // planted (review P2-2).
        let out = marv_core::process::command("/bin/ps")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .args(["-o", "rss=,time=,etime=", "-p", &pid.to_string()])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output();
        let Ok(out) = out else { return Stats::unknown() };
        let text = String::from_utf8_lossy(&out.stdout);
        match text.lines().find(|l| !l.trim().is_empty()) {
            Some(line) => parse(line, super::now_ms()).unwrap_or(Stats::unknown()),
            // ps prints nothing (and fails) for a PID that's gone.
            None => Stats::gone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guard's core (`proc_reads_only_marvs_own_pids` in tests/guards): a PID Marv never took is refused, even
    /// one that exists (this test's own process, its parent, init); one it took is read until it's let go.
    #[test]
    fn only_own_pids_are_read() {
        let me = std::process::id();
        assert_eq!(stats(me), Err(Refused));
        assert_eq!(stats(1), Err(Refused));
        own(1);
        assert_eq!(stats(1), Err(Refused), "pid 1 is never Marv's");
        let mut child = std::process::Command::new("sleep").arg("5").spawn().unwrap();
        let pid = child.id();
        assert_eq!(stats(pid), Err(Refused));
        own(pid);
        let s = stats(pid).unwrap();
        if cfg!(unix) {
            assert_eq!(s.alive, Some(true));
            assert!(s.rss_kb.is_some_and(|kb| kb > 0), "{s:?}");
        }
        release(pid);
        assert_eq!(stats(pid), Err(Refused));
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn a_reused_pid_is_not_marvs() {
        let mut child = std::process::Command::new("sleep").arg("5").spawn().unwrap();
        let pid = child.id();
        // Taken "long ago": the process now under that PID started after, so it's another's.
        owned().lock().unwrap().insert(pid, Owned { taken_ms: 1_000, start_ticks: None });
        if cfg!(unix) {
            assert_eq!(stats(pid), Err(Refused));
        }
        // Linux: taken just now, but from a process with another start tick (the wall clock can't tell).
        if let Some(ticks) = start_ticks(pid) {
            own_started(pid, Some(ticks.saturating_sub(1)));
            assert_eq!(stats(pid), Err(Refused));
            own(pid);
            assert!(stats(pid).is_ok());
        }
        release(pid);
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn cpu_percent_is_cpu_time_over_wall_time() {
        let at = |cpu| Stats { cpu_ms: Some(cpu), ..Stats::default() };
        assert_eq!(cpu_percent((0, at(100)), (1000, at(600))), Some(50));
        assert_eq!(cpu_percent((0, at(0)), (1000, at(2000))), Some(200), "two cores");
        assert_eq!(cpu_percent((5, at(0)), (5, at(10))), None, "no wall time");
        assert_eq!(cpu_percent((0, Stats::unknown()), (1000, at(10))), None);
    }

    #[cfg(unix)]
    #[test]
    fn ps_lines_parse() {
        assert_eq!(ps::duration_ms("01:02"), Some(62_000));
        assert_eq!(ps::duration_ms("1:00:00"), Some(3_600_000));
        assert_eq!(ps::duration_ms("2-00:00:01"), Some(2 * 86_400_000 + 1000));
        assert_eq!(ps::duration_ms("0:01.50"), Some(1500), "macOS gives hundredths");
        let s = ps::parse("  4520   0:00.12     05:00", 1_000_000).unwrap();
        assert_eq!((s.rss_kb, s.cpu_ms, s.started_ms), (Some(4520), Some(120), Some(700_000)));
        assert_eq!(ps::parse("garbage", 0), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_stat_parses_with_odd_names() {
        let line = "42 (a) b (c) S 1 42 42 0 -1 4194304 100 0 0 0 7 3 0 0 20 0 1 0 555 1000 200";
        assert_eq!(linux::parse_stat(line), Some((7, 3, 555)));
        assert_eq!(linux::parse_rss("Name:\tx\nVmRSS:\t  1234 kB\n"), Some(1234));
        assert_eq!(linux::parse_rss("Name:\tx\n"), None);
    }

    /// The `ps` path agrees with `/proc` on Linux (procps' columns are the same as macOS's).
    #[cfg(target_os = "linux")]
    #[test]
    fn ps_and_proc_agree() {
        let mut child = std::process::Command::new("sleep").arg("5").spawn().unwrap();
        let pid = child.id();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let (a, b) = (linux::read(pid), ps::read(pid));
        if b.alive.is_some() {
            assert_eq!(a.alive, b.alive);
            let (sa, sb) = (a.started_ms.unwrap(), b.started_ms.unwrap());
            assert!(sa.abs_diff(sb) < 3_000, "{sa} vs {sb}");
        }
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(!linux::read(pid).alive.unwrap_or(false), "gone or a zombie until reaped");
    }
}
