//! The dock's Git tab: the person's own lazygit, in a terminal of its own, in the folder they're looking at (a chat's
//! folder, or its worktree). Each folder is a project: its lazygit stays running while the person moves between that
//! folder's chats or tabs, and the next look attaches to it again, so it opens at once.
//!
//! - It's the person's tool on their own machine, like a terminal they opened: not sandboxed, not a model's.
//! - The page never names a path: the folder is the chat's (or a tree folder under the Marv folder), as the other dock
//!   tabs resolve it.
//! - At most [`MAX`] lazygits stay running; the least recently looked at goes first. A chat's agent is never ended
//!   to make room. All end with the app.
//! - The terminal itself is `portable-pty`'s (WezTerm's): the `unsafe` a pseudo-terminal needs stays in that crate.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use base64::Engine;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde_json::{Value, json};
use tauri::ipc::Channel;

/// How many lazygits stay running at once.
pub const MAX: usize = 6;

/// The program the Git tab runs.
pub const PROGRAM: &str = "lazygit";

/// Said when lazygit isn't installed.
pub const NOT_INSTALLED: &str = "The Git tab runs lazygit, which isn't installed. Install it \
     (https://github.com/jesseduffield/lazygit#installation) and open the tab again.";

/// Where a terminal's output goes: the page's channel while a Git tab shows it.
type Sink = Arc<Mutex<Option<Channel<Value>>>>;

/// A terminal's latest output, at most [`HISTORY`] bytes.
type History = Arc<Mutex<std::collections::VecDeque<u8>>>;

/// How much of a terminal's output is kept for a view that attaches later: an agent started with no page (a team's,
/// mail's) drew its screen into nothing, and the chat opened blank until the agent drew again (Claude Code didn't).
pub const HISTORY: usize = 512 * 1024;

struct Term {
    id: u64,
    master: Box<dyn MasterPty + Send>,
    /// Shared with its reader, which answers ConPTY's start-up question itself ([`answer_conpty_start`]).
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// What the person types, in order, for a thread that writes it ([`Terminals::write`]): a program not reading its
    /// input blocks that write, never the page or the other terminals. Ends with the terminal.
    typed: std::sync::mpsc::Sender<Vec<u8>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    out: Sink,
    /// Its latest output, replayed to a view that attaches (under `out`'s lock, so nothing is lost or doubled).
    history: History,
    used: Instant,
    /// What lives as long as it runs (a chat's mesh server).
    _keep: Option<Box<dyn std::any::Any + Send>>,
    /// Windows: the Job Object its program was put in ([`contain`]). Dropping it closes the job's only handle, and
    /// KILL_ON_JOB_CLOSE ends every process in it; Windows closes it too when Marv OS crashes, so no agent outlives it.
    #[cfg(windows)]
    job: Option<win32job::Job>,
    /// A chat's agent: its screen and output, for its state ([`crate::agent_detect`]), told each new size.
    output: Option<Arc<crate::terminal_events::Output>>,
}

impl Term {
    fn resize(&self, size: PtySize) {
        let _ = self.master.resize(size);
        if let Some(output) = &self.output {
            output.sized(size.cols, size.rows);
        }
    }
}

/// Windows: a new Job Object that ends everything in it when its last handle closes, with `child` in it. Its children,
/// and theirs, start inside it (a process can't leave a job it didn't ask to break away from). None: it couldn't be
/// made or `child` assigned (an old Windows that doesn't nest jobs, a child already gone); the failure is logged and
/// [`Term`]'s drop falls back to `taskkill /T`.
///
/// `child` already runs when portable-pty returns it, so a process it started before being assigned would be outside
/// the job; an agent (a Node or native CLI starting up) starts its first one well after the moment this takes.
#[cfg(windows)]
fn contain(child: &(dyn portable_pty::Child + Send + Sync), id: u64) -> Option<win32job::Job> {
    let handle = child.as_raw_handle()?;
    let made = win32job::Job::create_with_limit_info(win32job::ExtendedLimitInfo::new().limit_kill_on_job_close())
        .and_then(|job| job.assign_process(handle as isize).map(|()| job));
    match made {
        Ok(job) => Some(job),
        Err(e) => {
            crate::applog::warn("terminal", &format!("terminal {id} isn't in a job object ({e}); taskkill ends it"));
            None
        }
    }
}

/// Windows: its job ends the agent and every process under it. Without one (see [`contain`]), `taskkill /T /F`, off
/// this thread so the terminals' lock isn't held while it runs.
#[cfg(windows)]
impl Drop for Term {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }
        // Closed with the field, right after this.
        if self.job.is_some() {
            return;
        }
        match self.child.process_id() {
            Some(pid) => {
                std::thread::spawn(move || marv_sandbox::windows::end_tree(pid));
            }
            None => {
                let _ = self.child.kill();
            }
        }
    }
}

#[cfg(unix)]
impl Drop for Term {
    fn drop(&mut self) {
        use nix::sys::signal::{Signal, kill, killpg};
        use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
        // Already ended and reaped: its number may be another process's now.
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }
        let Some(pid) = self.child.process_id() else {
            let _ = self.child.kill();
            return;
        };
        let pid = nix::unistd::Pid::from_raw(pid as i32);
        // It leads its own session: everything in it ends with it (a pager or an editor it started; Codex's
        // code-mode host, in a process group of its own).
        let session = move |signal| {
            session_members(pid).into_iter().for_each(|p| {
                let _ = kill(p, signal);
            })
        };
        // The members now, for where the session can't be looked up once its leader is gone (macOS finds them by the
        // leader's terminal).
        let known = session_members(pid);
        let _ = killpg(pid, Signal::SIGHUP);
        session(Signal::SIGHUP);
        // Waited for on a thread of its own, killed if the hangup didn't end it: waiting here held the terminals' lock,
        // and a Codex that never finished ending (one of its threads stuck on a helper's pipe) froze every terminal and
        // the Merge that closed it.
        std::thread::spawn(move || {
            let until = std::time::Instant::now() + KILL_AFTER;
            let mut reaped = false;
            loop {
                reaped = reaped || !matches!(waitpid(pid, Some(WaitPidFlag::WNOHANG)), Ok(WaitStatus::StillAlive));
                // The shell dies of the hangup, but a `nohup` child (anything that ignores SIGHUP) stays in the
                // session: it is killed too, not only when the leader itself hangs on. (A process that left the
                // session with `setsid` is detached on purpose, and survives by design.)
                if reaped && session_left(pid, &known).is_empty() {
                    return;
                }
                if std::time::Instant::now() >= until {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            // The leader's number may be another process's once it is reaped: only signalled while it isn't.
            if !reaped {
                let _ = kill(pid, Signal::SIGKILL);
                let _ = killpg(pid, Signal::SIGKILL);
            }
            session_left(pid, &known).into_iter().for_each(|p| {
                let _ = kill(p, Signal::SIGKILL);
            });
            if !reaped {
                let _ = waitpid(pid, None);
            }
        });
    }
}

/// How long a closed terminal's processes get to end on the hangup before the rest are killed.
#[cfg(unix)]
const KILL_AFTER: std::time::Duration = std::time::Duration::from_secs(1);

/// The processes still in the session `leader` led (not the leader), whether or not the leader is gone: on Linux the
/// session id outlives its leader for as long as a member runs (and no process can take the number meanwhile); on
/// macOS the tty lookup needs the leader, so the members `known` from before are checked instead.
#[cfg(unix)]
fn session_left(leader: nix::unistd::Pid, known: &[nix::unistd::Pid]) -> Vec<nix::unistd::Pid> {
    let mut left: Vec<nix::unistd::Pid> = session_members(leader).into_iter().filter(|p| *p != leader).collect();
    if cfg!(target_os = "macos") {
        for p in known {
            if *p != leader && !left.contains(p) && nix::sys::signal::kill(*p, None).is_ok() {
                left.push(*p);
            }
        }
    }
    left
}

/// The processes in session `leader` (from `/proc`; none where there's no `/proc`), the leader too.
#[cfg(all(unix, not(target_os = "macos")))]
fn session_members(leader: nix::unistd::Pid) -> Vec<nix::unistd::Pid> {
    let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
    dir.flatten()
        .filter_map(|entry| {
            let pid: i32 = entry.file_name().to_str()?.parse().ok()?;
            let stat = std::fs::read_to_string(entry.path().join("stat")).ok()?;
            // After the command's closing parenthesis: state, ppid, pgrp, session.
            let session: i32 = stat.rsplit_once(')')?.1.split_whitespace().nth(3)?.parse().ok()?;
            (session == leader.as_raw()).then(|| nix::unistd::Pid::from_raw(pid))
        })
        .collect()
}

/// macOS has no `/proc`, and its `ps` doesn't print a usable session id (`sess` is a kernel address or 0): the
/// processes on the terminal `leader` leads, from `ps` (every process of the session has the terminal, until it starts
/// a session of its own), the leader too.
#[cfg(target_os = "macos")]
fn session_members(leader: nix::unistd::Pid) -> Vec<nix::unistd::Pid> {
    let Ok(out) = std::process::Command::new("/bin/ps").args(["-axo", "pid=,tty="]).output() else { return Vec::new() };
    let text = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<(i32, &str)> = text
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((fields.next()?.parse().ok()?, fields.next()?))
        })
        .collect();
    // "??" is no terminal.
    let Some(tty) = rows.iter().find(|(pid, _)| *pid == leader.as_raw()).map(|(_, tty)| *tty).filter(|t| *t != "??") else {
        return Vec::new();
    };
    rows.iter().filter(|(_, t)| *t == tty).map(|(pid, _)| nix::unistd::Pid::from_raw(*pid)).collect()
}

/// The running terminals, by what they show: `git:<folder>` (a folder's lazygit), `chat:<id>` (a chat's agent).
#[derive(Default)]
pub struct Terminals {
    terms: Mutex<HashMap<String, Term>>,
    next: Mutex<u64>,
    /// Per terminal key, a lock a start holds from its check to its terminal, so two overlapping starts don't both
    /// set up (see [`Terminals::start_lock`]).
    starts: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// Per terminal key, the start that owns it now ([`Terminals::claim`]).
    owners: Mutex<HashMap<String, u64>>,
}

fn size(cols: u16, rows: u16) -> PtySize {
    PtySize { rows: rows.max(2), cols: cols.max(10), pixel_width: 0, pixel_height: 0 }
}

/// The lazygit the Git tab runs: the person's own (on PATH), else the one the Windows installer brings, next to
/// marv-desktop.exe. None: neither.
pub fn lazygit() -> Option<std::path::PathBuf> {
    marv_session::paths::on_path(PROGRAM).or_else(|| {
        let exe = std::env::current_exe().ok()?;
        let bundled = exe.parent()?.join(if cfg!(windows) { "lazygit.exe" } else { PROGRAM });
        bundled.is_file().then_some(bundled)
    })
}

impl Terminals {
    /// Shows `folder`'s lazygit on `out` at this size: the running one (redrawn for this view), else a new one. Its
    /// id, for [`write`](Self::write) and [`resize`](Self::resize).
    pub fn open(&self, folder: &Path, cols: u16, rows: u16, out: Channel<Value>) -> Result<u64, String> {
        let program = lazygit();
        if program.is_none() && !self.running(&git_key(folder)) {
            return Err(NOT_INSTALLED.into());
        }
        let command = Launch {
            argv: vec![program.map_or_else(|| PROGRAM.into(), std::path::PathBuf::into_os_string)],
            path: None,
            withheld: Vec::new(),
            cwd: folder.to_path_buf(),
            keep: None,
            env: Vec::new(),
        };
        self.open_with(git_key(folder), command, cols, rows, Some(out), None, None)
    }

    /// The lock for starting terminal `key`: held across the whole start, so a second start waits and then finds the
    /// first one's terminal running.
    pub fn start_lock(&self, key: &str) -> Arc<Mutex<()>> {
        self.starts.lock().unwrap_or_else(|e| e.into_inner()).entry(key.to_string()).or_default().clone()
    }

    /// A new start of terminal `key` takes it over: its number, for [`owns`](Self::owns). An older terminal of the key
    /// that ends later (a restart's hangup takes a moment, its kill a second) is no longer its owner.
    pub fn claim(&self, key: &str) -> u64 {
        let mut next = self.next.lock().unwrap_or_else(|e| e.into_inner());
        *next += 1;
        self.owners.lock().unwrap_or_else(|e| e.into_inner()).insert(key.to_string(), *next);
        *next
    }

    /// Whether start `generation` ([`claim`](Self::claim)) is the latest of terminal `key`.
    pub fn owns(&self, key: &str, generation: u64) -> bool {
        self.owners.lock().unwrap_or_else(|e| e.into_inner()).get(key).is_none_or(|g| *g == generation)
    }

    /// Terminal `id`'s key (`chat:<id>`, `git:<folder>`).
    pub fn key_of(&self, id: u64) -> Option<String> {
        self.terms.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(_, t)| t.id == id).map(|(k, _)| k.clone())
    }

    /// The process terminal `key` runs (its agent), while it runs.
    pub fn pid(&self, key: &str) -> Option<u32> {
        self.terms.lock().unwrap_or_else(|e| e.into_inner()).get(key)?.child.process_id()
    }

    /// Whether terminal `key` runs.
    pub fn running(&self, key: &str) -> bool {
        let mut terms = self.terms.lock().unwrap_or_else(|e| e.into_inner());
        terms.retain(|_, t| t.child.try_wait().ok().flatten().is_none());
        terms.contains_key(key)
    }

    /// Shows terminal `key` on `out`: the running one (redrawn for this view), else `command` started for it, and
    /// `ended` called once it exits (on its reader's thread).
    #[allow(clippy::too_many_arguments)] // What runs, how it's shown, and what's told when it draws and ends.
    pub fn open_with(
        &self,
        key: String,
        command: Launch,
        cols: u16,
        rows: u16,
        out: Option<Channel<Value>>,
        ended: Option<Box<dyn FnOnce() + Send>>,
        output: Option<Arc<crate::terminal_events::Output>>,
    ) -> Result<u64, String> {
        let mut terms = self.terms.lock().unwrap_or_else(|e| e.into_inner());
        terms.retain(|_, t| t.child.try_wait().ok().flatten().is_none());
        if let Some(term) = terms.get_mut(&key) {
            if let Some(out) = out {
                // What it drew so far first (under the output's lock: the reader can't slip a chunk in between).
                let mut sink = term.out.lock().unwrap_or_else(|e| e.into_inner());
                let history: Vec<u8> = term.history.lock().unwrap_or_else(|e| e.into_inner()).iter().copied().collect();
                if !history.is_empty() {
                    let _ = out.send(json!({ "data": base64::engine::general_purpose::STANDARD.encode(&history) }));
                }
                *sink = Some(out);
            }
            term.used = Instant::now();
            // A size change makes it draw the whole screen again for the new view.
            term.resize(size(cols, rows.saturating_sub(1)));
            term.resize(size(cols, rows));
            return Ok(term.id);
        }
        make_room(&mut terms, &key);
        let id = {
            let mut next = self.next.lock().unwrap_or_else(|e| e.into_inner());
            *next += 1;
            *next
        };
        let term = spawn(id, command, cols, rows, Arc::new(Mutex::new(out)), ended, output)?;
        terms.insert(key, term);
        Ok(id)
    }

    fn with<T>(&self, id: u64, f: impl FnOnce(&mut Term) -> T) -> Option<T> {
        let mut terms = self.terms.lock().unwrap_or_else(|e| e.into_inner());
        terms.values_mut().find(|t| t.id == id).map(f)
    }

    /// What the person typed (or pasted) into terminal `id`, queued for the terminal's own writer thread: in the order
    /// given, and never blocking here (the terminals' lock is only held to queue it). A bracketed paste gets its
    /// platform's line breaks ([`for_pty`]).
    pub fn write(&self, id: u64, data: &str) -> Result<(), String> {
        let data = for_pty(data);
        self.with(id, |t| t.typed.send(data.as_bytes().to_vec()).map_err(|e| e.to_string()))
            .unwrap_or_else(|| Err("This folder's lazygit has ended: open the Git tab again.".into()))
    }

    /// Writes `data` into terminal `key` (team mail typed into a chat's agent). False: it isn't running.
    pub fn write_key(&self, key: &str, data: &str) -> bool {
        // Written with the terminals' lock released: a program not reading its input blocks only this call.
        let writer = self.terms.lock().unwrap_or_else(|e| e.into_inner()).get(key).map(|t| t.writer.clone());
        writer.is_some_and(|w| write_to(&w, data.as_bytes()).is_ok())
    }

    /// The text of terminal `key`'s latest output (its last `bytes`, escape sequences left out): what it shows now,
    /// roughly, for a question waiting on the person.
    pub fn tail(&self, key: &str, bytes: usize) -> Option<String> {
        let terms = self.terms.lock().unwrap_or_else(|e| e.into_inner());
        let history = terms.get(key)?.history.lock().unwrap_or_else(|e| e.into_inner());
        let from = history.len().saturating_sub(bytes);
        let raw: Vec<u8> = history.iter().skip(from).copied().collect();
        Some(plain_text(&raw))
    }

    /// What terminal `key` shows now (its screen model's visible rows), for a terminal that keeps one (a chat's agent);
    /// None for one that doesn't, or before its size is known.
    pub fn screen(&self, key: &str) -> Option<String> {
        let output = self.terms.lock().unwrap_or_else(|e| e.into_inner()).get(key)?.output.clone()?;
        output.screen_text()
    }

    /// The Git tab's new size.
    pub fn resize(&self, id: u64, cols: u16, rows: u16) {
        self.with(id, |t| t.resize(size(cols, rows)));
    }

    /// The Git tab stopped showing terminal `id`: it keeps running, its output unseen until the next look.
    pub fn detach(&self, id: u64) {
        self.with(id, |t| *t.out.lock().unwrap_or_else(|e| e.into_inner()) = None);
    }

    /// Ends terminal `key` if it runs.
    pub fn close_key(&self, key: &str) {
        self.terms.lock().unwrap_or_else(|e| e.into_inner()).remove(key);
    }

    /// Ends the agent terminals of chats `ids` that run, each with its whole session as [`close_key`] does: a stopped
    /// team's members. Pausing alone left their agents and what those started running (seen 2026-11-17). The chats
    /// whose terminal ran.
    pub fn close_chats(&self, ids: &[String]) -> Vec<String> {
        let mut terms = self.terms.lock().unwrap_or_else(|e| e.into_inner());
        terms.retain(|_, t| t.child.try_wait().ok().flatten().is_none());
        ids.iter().filter(|id| terms.remove(&format!("chat:{id}")).is_some()).cloned().collect()
    }

    /// The app quits: every terminal ends now, with its whole session ([`Term`]'s drop), and this waits (up to 2 s in
    /// all) for the agents to be gone. Managed state isn't dropped at process exit, so without this a dev server an
    /// agent started kept running after Marv OS closed.
    pub fn close_all(&self) {
        let terms: Vec<(String, Term)> = self.terms.lock().unwrap_or_else(|e| e.into_inner()).drain().collect();
        {
            // Their `ended` callbacks run as the readers see them end: none of them is a chat's to clear now.
            let mut owners = self.owners.lock().unwrap_or_else(|e| e.into_inner());
            terms.iter().for_each(|(key, _)| {
                owners.insert(key.clone(), u64::MAX);
            });
        }
        #[cfg(unix)]
        let pids: Vec<(nix::unistd::Pid, Vec<nix::unistd::Pid>)> = terms
            .iter()
            .filter_map(|(_, t)| t.child.process_id())
            .map(|p| nix::unistd::Pid::from_raw(p as i32))
            .map(|p| (p, session_members(p)))
            .collect();
        drop(terms);
        // Their hangup is sent; the kill follows a second later from a thread that would die with the process. Reaped
        // (gone to `kill`) once it has run.
        #[cfg(unix)]
        {
            let until = Instant::now() + std::time::Duration::from_secs(2);
            // Their sessions' other members too: a `nohup` child outlives the hangup and is killed a second later; the
            // process would otherwise end first and leave it running.
            let running = |(leader, known): &(nix::unistd::Pid, Vec<nix::unistd::Pid>)| {
                nix::sys::signal::kill(*leader, None).is_ok() || !session_left(*leader, known).is_empty()
            };
            while Instant::now() < until && pids.iter().any(running) {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }

    /// Ends terminal `id` (the person asked for a fresh one).
    pub fn close(&self, id: u64) {
        self.terms.lock().unwrap_or_else(|e| e.into_inner()).retain(|_, t| t.id != id);
    }
}

/// Where a bracketed paste starts and ends (xterm's `term.paste` and team mail send them: the agent takes one paste).
const PASTE_START: &str = "\x1b[200~";
const PASTE_END: &str = "\x1b[201~";

/// How long after a typed message's paste its Enter goes in: an Enter in the same write reached the agent while it was
/// still taking the paste, and became a newline in it (herdr waits 300 ms for every agent; Marv already waited 400).
pub const SUBMIT_DELAY: std::time::Duration = std::time::Duration::from_millis(400);

/// Right arrow: the key that ends a paste for Codex on Windows ([`submission`]).
const PASTE_BOUNDARY: &str = "\x1b[C";

/// Pasted `text` with its platform's line breaks (`windows`: the platform, a parameter so the tests run anywhere): on
/// Windows every one CRLF, as herdr's `prepare_paste_text_for_pty_platform` (Windows console programs read a paste's
/// lines by CRLF); elsewhere as it is. A lone CR counts as a line break too: xterm.js turns every pasted line break
/// into one before the page sends it.
pub fn paste_line_breaks(text: &str, windows: bool) -> std::borrow::Cow<'_, str> {
    if !windows || !text.contains(['\r', '\n']) {
        return text.into();
    }
    text.replace("\r\n", "\n").replace('\r', "\n").replace('\n', "\r\n").into()
}

/// What the page sends a terminal, ready for its pty: a bracketed paste (the whole of one write, as xterm sends it)
/// with this platform's [`paste_line_breaks`]. Typed keys pass as they are: a typed Enter is a CR, a ctrl+J a newline.
/// With [`submission`], the one place pasted text is converted.
fn for_pty(data: &str) -> std::borrow::Cow<'_, str> {
    pasted_for(data, cfg!(windows))
}

fn pasted_for(data: &str, windows: bool) -> std::borrow::Cow<'_, str> {
    match data.strip_prefix(PASTE_START).and_then(|d| d.strip_suffix(PASTE_END)) {
        Some(body) if windows => format!("{PASTE_START}{}{PASTE_END}", paste_line_breaks(body, true)).into(),
        _ => data.into(),
    }
}

/// A message typed into an agent's terminal (team mail, a timer): `paste` written first, Enter `enter_after` later.
#[derive(Debug, PartialEq, Eq)]
pub struct Submission {
    pub paste: String,
    pub enter_after: std::time::Duration,
}

/// How `text` is typed into agent `agent`'s terminal (`windows`: the platform, a parameter so the tests run anywhere):
/// pasted, bracketed so its lines stay one message, with the platform's line breaks, Enter [`SUBMIT_DELAY`] later.
///
/// Codex on Windows (herdr #3187, #3961): its Windows input reader doesn't see bracketed paste, takes the text as a
/// "paste burst", and while that burst is buffered turns a following Enter into a newline. The burst flushes after an
/// idle timeout, so a wait sized to the text is a guess that fails when ConPTY delivers a long paste late (herdr tried
/// that first). Codex flushes the burst at once on a key that isn't a character, so a right arrow after the paste marks
/// its end however long it is or however slowly it arrives, and the Enter then submits. Elsewhere Codex sees the
/// bracketed paste, and herdr sends nothing more.
pub fn submission(text: &str, agent: &str, windows: bool) -> Submission {
    let mut paste = format!("{PASTE_START}{}{PASTE_END}", paste_line_breaks(text, windows));
    if windows && agent == "codex" {
        paste.push_str(PASTE_BOUNDARY);
    }
    Submission { paste, enter_after: SUBMIT_DELAY }
}

/// What a terminal runs: `argv` in `cwd`; its environment this process's less `withheld`, or (`path` set: inside the
/// outer sandbox, whose `bwrap` sets the rest) cleared to that `PATH`.
pub struct Launch {
    pub argv: Vec<std::ffi::OsString>,
    pub path: Option<String>,
    pub withheld: Vec<String>,
    pub cwd: PathBuf,
    /// Kept for as long as it runs, dropped when it ends (a chat's mesh server).
    pub keep: Option<Box<dyn std::any::Any + Send>>,
    /// Variables set for it (outside the sandbox: inside, `bwrap`'s own carry them).
    pub env: Vec<(String, String)>,
}

/// Before terminal `key` starts: a lazygit makes room among the lazygits (at most [`MAX`], the least recently seen
/// ends first); a chat's agent never ends to make room (ending one cut a Marv off in the middle of its turn).
fn make_room(terms: &mut HashMap<String, Term>, key: &str) {
    if !key.starts_with("git:") {
        return;
    }
    while terms.keys().filter(|k| k.starts_with("git:")).count() >= MAX {
        let oldest = terms.iter().filter(|(k, _)| k.starts_with("git:")).min_by_key(|(_, t)| t.used).map(|(k, _)| k.clone());
        let Some(oldest) = oldest else { break };
        terms.remove(&oldest);
    }
}

/// `raw` terminal output as text: escape sequences (CSI, OSC) dropped, cursor moves as spaces.
pub fn plain_text(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI: parameters, then a final byte; a cursor move or erase reads as a space between words.
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
                out.push(' ');
            }
            // OSC: up to BEL or ESC \.
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' || (c == '\x1b' && chars.peek() == Some(&'\\')) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// A folder's lazygit key, from the folder's standard form: on Windows `c:\proj` and `C:\Proj` are one folder, one
/// lazygit.
fn git_key(folder: &Path) -> String {
    format!("git:{}", marv_session::paths::normalize_folder(folder).display())
}

/// `launch` on a new pseudo-terminal, its output read on a thread of its own into `out` (base64 chunks, then `exit`
/// when it ends, and `ended` called).
/// `data` into a terminal, flushed.
fn write_to(writer: &Mutex<Box<dyn Write + Send>>, data: &[u8]) -> std::io::Result<()> {
    let mut writer = writer.lock().unwrap_or_else(|e| e.into_inner());
    writer.write_all(data).and_then(|_| writer.flush())
}

/// The cursor-position question (DSR, `ESC [ 6 n`).
const CURSOR_QUESTION: &[u8] = b"[6n";

/// Windows: ConPTY starts by asking the terminal where its cursor is (portable-pty opens it with
/// `PSEUDOCONSOLE_INHERIT_CURSOR`) and holds the program until it's answered. Only the page's terminal answered, and
/// only a terminal shown as it started: a team's chats start unseen, and the replay when one is opened came before the
/// page could send anything, so their agents waited forever on a black screen (3 of 3, 2026-11-14). The first question
/// is always ConPTY's (nothing else runs before it's answered), so it's answered here: row 1, column 1, a new terminal's
/// cursor; and taken out of the output, so the page never answers it as well (a second answer would be typed into the
/// agent). Later questions are the agent's own and go to the page as ever.
///
/// Whether to keep looking (not once it's answered, nor after a few seconds of output without one: elsewhere, and on a
/// ConPTY that doesn't ask), and the output without the question when it was in `chunk`.
fn answer_conpty_start(chunk: &[u8], writer: &Mutex<Box<dyn Write + Send>>, started: Instant) -> (bool, Option<Vec<u8>>) {
    if !cfg!(windows) {
        return (false, None);
    }
    match chunk.windows(CURSOR_QUESTION.len()).position(|w| w == CURSOR_QUESTION) {
        Some(at) => {
            let _ = write_to(writer, b"[1;1R");
            (false, Some([&chunk[..at], &chunk[at + CURSOR_QUESTION.len()..]].concat()))
        }
        None => (started.elapsed() < std::time::Duration::from_secs(5), None),
    }
}

fn spawn(
    id: u64,
    launch: Launch,
    cols: u16,
    rows: u16,
    out: Sink,
    ended: Option<Box<dyn FnOnce() + Send>>,
    output: Option<Arc<crate::terminal_events::Output>>,
) -> Result<Term, String> {
    let pair = native_pty_system().openpty(size(cols, rows)).map_err(|e| format!("Couldn't open a terminal: {e}"))?;
    let program = launch.argv.first().ok_or("Nothing to run.")?.to_string_lossy().into_owned();
    let mut command = CommandBuilder::from_argv(launch.argv.clone());
    command.cwd(&launch.cwd);
    match &launch.path {
        Some(path) => {
            command.env_clear();
            command.env("PATH", path);
        }
        None => {
            // The host's environment, not the AppImage's (`marv_core::process::host_env_fixes`): an agent's
            // `python3` found the image's PYTHONHOME and died without its standard library.
            for (name, value) in marv_core::process::host_env_fixes() {
                match value {
                    Some(value) => command.env(name, value),
                    None => command.env_remove(name),
                }
            }
            for name in &launch.withheld {
                command.env_remove(name);
            }
            command.env("TERM", "xterm-256color");
            command.env("COLORTERM", "truecolor");
            for (key, value) in &launch.env {
                command.env(key, value);
            }
        }
    }
    let child = pair.slave.spawn_command(command).map_err(|e| {
        crate::applog::error("terminal", &format!("couldn't start {program} in {}: {e}", launch.cwd.display()));
        format!("Couldn't start {program}: {e}")
    })?;
    let name = std::path::Path::new(&program).file_name().map_or(program.clone(), |n| n.to_string_lossy().into_owned());
    crate::applog::info(
        "terminal",
        &format!("started {name} (terminal {id}{})", if launch.path.is_some() { ", sandboxed" } else { "" }),
    );
    #[cfg(windows)]
    let job = contain(child.as_ref(), id);
    let started = Instant::now();
    // Started here: Activity may show its CPU and memory (a PID Marv didn't start is never read).
    if let Some(pid) = child.process_id() {
        marv_kernel::procstats::own(pid);
    }
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = Arc::new(Mutex::new(pair.master.take_writer().map_err(|e| e.to_string())?));
    let answer = writer.clone();
    let (typed, typing) = std::sync::mpsc::channel::<Vec<u8>>();
    let typist = writer.clone();
    std::thread::spawn(move || {
        while let Ok(data) = typing.recv() {
            if write_to(&typist, &data).is_err() {
                break;
            }
        }
    });
    let sink = out.clone();
    let history: History = Arc::default();
    let kept = history.clone();
    let watched = output.clone();
    if let Some(output) = &output {
        let size = size(cols, rows);
        output.sized(size.cols, size.rows);
    }
    std::thread::spawn(move || {
        let mut buf = [0u8; 16 * 1024];
        let mut starting = true;
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let answered;
                    let mut chunk = &buf[..n];
                    if starting {
                        (starting, answered) = answer_conpty_start(chunk, &answer, started);
                        if let Some(rest) = &answered {
                            chunk = rest;
                        }
                        if chunk.is_empty() {
                            continue;
                        }
                    }
                    if let Some(output) = &output {
                        output.saw(chunk);
                    }
                    let data = base64::engine::general_purpose::STANDARD.encode(chunk);
                    let sink = sink.lock().unwrap_or_else(|e| e.into_inner());
                    {
                        let mut kept = kept.lock().unwrap_or_else(|e| e.into_inner());
                        kept.extend(chunk);
                        let over = kept.len().saturating_sub(HISTORY);
                        kept.drain(..over);
                    }
                    if let Some(out) = sink.as_ref() {
                        let _ = out.send(json!({ "data": data }));
                    }
                }
            }
        }
        // An agent that ended within seconds of starting most likely failed to start: what it last printed says why
        // (a missing login, a bad flag). Only then: later output is the person's conversation.
        let ran = started.elapsed();
        if ran < std::time::Duration::from_secs(10) {
            let kept = kept.lock().unwrap_or_else(|e| e.into_inner());
            let last: Vec<u8> = kept.iter().skip(kept.len().saturating_sub(600)).copied().collect();
            let tail = String::from_utf8_lossy(&last).into_owned();
            let tail = strip_ansi(&tail);
            crate::applog::warn(
                "terminal",
                &format!("{name} (terminal {id}) ended after {} ms; it last printed: {}", ran.as_millis(), tail.trim()),
            );
        } else {
            crate::applog::info("terminal", &format!("{name} (terminal {id}) ended after {} s", ran.as_secs()));
        }
        // It ended (the person quit it): the tab says so and offers a new one.
        if let Some(out) = sink.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let _ = out.send(json!({ "exit": true }));
        }
        if let Some(ended) = ended {
            ended();
        }
    });
    Ok(Term {
        id,
        master: pair.master,
        writer,
        typed,
        child,
        out,
        history,
        used: Instant::now(),
        _keep: launch.keep,
        #[cfg(windows)]
        job,
        output: watched,
    })
}

/// `text` without terminal escape sequences (colours, cursor moves), for the log.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI: parameters up to a final letter.
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: up to BEL or ESC \.
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' || (c == '\u{1b}' && chars.peek() == Some(&'\\')) {
                        chars.next_if_eq(&'\\');
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A restart's new start owns the chat's terminal: the old one's end (a second later) is no longer its to report.
    #[test]
    fn an_older_start_does_not_own_the_key_after_a_newer_one() {
        let terminals = Terminals::default();
        let old = terminals.claim("chat:a");
        assert!(terminals.owns("chat:a", old));
        let new = terminals.claim("chat:a");
        assert!(!terminals.owns("chat:a", old));
        assert!(terminals.owns("chat:a", new));
        assert!(terminals.owns("chat:b", 1), "an unclaimed key has no newer owner");
    }

    /// A restart (the permission card's Restart it): the chat's terminal is closed and opened again, and the second
    /// open starts the agent afresh rather than attaching to the one just closed.
    #[cfg(unix)]
    #[test]
    fn a_restart_starts_the_agent_again() {
        let dir = tempfile::tempdir().unwrap();
        let terminals = Terminals::default();
        let launch = || Launch {
            argv: vec!["sleep".into(), "30".into()],
            path: None,
            withheld: Vec::new(),
            cwd: dir.path().into(),
            keep: None,
            env: Vec::new(),
        };
        let first = terminals.open_with("chat:a".into(), launch(), 80, 24, None, None, None).unwrap();
        let before = terminals.pid("chat:a").unwrap();
        terminals.close_key("chat:a");
        let second = terminals.open_with("chat:a".into(), launch(), 80, 24, None, None, None).unwrap();
        assert_ne!(first, second, "a new terminal");
        assert!(terminals.running("chat:a"));
        assert_ne!(terminals.pid("chat:a"), Some(before), "a new agent process");
        terminals.close_key("chat:a");
    }

    /// A program on a terminal of its own: it sees a terminal of the size given, in its folder, and what's typed reaches
    /// it. (No channel here: its output is read straight off the terminal.)
    #[test]
    fn a_program_runs_on_its_own_terminal_in_its_folder() {
        let dir = tempfile::tempdir().unwrap();
        let pair = native_pty_system().openpty(size(91, 27)).unwrap();
        let mut command = CommandBuilder::new("sh");
        command.args(["-c", "pwd; stty size; test -t 0 && echo tty; read line; echo got:$line"]);
        command.cwd(dir.path());
        let mut child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let mut writer = pair.master.take_writer().unwrap();
        writer.write_all(b"hello\n").unwrap();
        child.wait().unwrap();
        let mut seen = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            seen.extend_from_slice(&buf[..n]);
            if String::from_utf8_lossy(&seen).contains("got:hello") {
                break;
            }
        }
        let seen = String::from_utf8_lossy(&seen);
        let real = std::fs::canonicalize(dir.path()).unwrap();
        assert!(seen.contains(&*real.to_string_lossy()), "{seen}");
        assert!(seen.contains("27 91"), "{seen}");
        assert!(seen.contains("tty") && seen.contains("got:hello"), "{seen}");
    }

    /// Lazygits make room for a new one, the least recently seen first; a chat's agent never does, however many run.
    #[test]
    fn only_lazygits_are_ended_to_make_room() {
        let dir = tempfile::tempdir().unwrap();
        let terminals = Terminals::default();
        let sleep = |key: &str| {
            let launch = Launch {
                argv: vec!["sleep".into(), "30".into()],
                path: None,
                withheld: Vec::new(),
                cwd: dir.path().into(),
                keep: None,
                env: Vec::new(),
            };
            let mut terms = terminals.terms.lock().unwrap();
            let term = spawn(terms.len() as u64 + 1, launch, 80, 24, Arc::new(Mutex::new(None)), None, None).unwrap();
            terms.insert(key.to_string(), term);
        };
        for i in 0..8 {
            sleep(&format!("chat:{i}"));
        }
        for i in 0..MAX {
            sleep(&format!("git:/f{i}"));
        }
        make_room(&mut terminals.terms.lock().unwrap(), "chat:new");
        assert_eq!(terminals.terms.lock().unwrap().len(), 8 + MAX, "a chat's agent makes no room");
        make_room(&mut terminals.terms.lock().unwrap(), "git:/new");
        assert!((0..8).all(|i| terminals.running(&format!("chat:{i}"))), "every chat's agent still runs");
        assert!(!terminals.running("git:/f0"), "the oldest lazygit made room");
    }

    /// Closing a terminal doesn't wait for its program: one that ignores the hangup (and a child of it in a process
    /// group of its own, as Codex's code-mode host is) is killed a moment later, while the terminals go on.
    #[cfg(unix)] // process groups and sessions; Windows closes the tree with its Job Object
    #[test]
    fn closing_a_terminal_ends_its_whole_session_without_waiting() {
        let dir = tempfile::tempdir().unwrap();
        let terminals = Terminals::default();
        let pids = dir.path().join("pids");
        // `set -m`: its background child gets a process group of its own, in the same session.
        let script = format!(
            "trap '' HUP; set -m; sh -c 'trap \\\"\\\" HUP; echo $$ > {0}.child; exec sleep 30' & echo $$ > {0}; wait",
            pids.display()
        );
        let launch = Launch {
            argv: vec!["sh".into(), "-c".into(), script.into()],
            path: None,
            withheld: Vec::new(),
            cwd: dir.path().into(),
            keep: None,
            env: Vec::new(),
        };
        let term = spawn(1, launch, 80, 24, Arc::new(Mutex::new(None)), None, None).unwrap();
        terminals.terms.lock().unwrap().insert("chat:a".into(), term);
        let read = |p: &std::path::Path| std::fs::read_to_string(p).ok().and_then(|s| s.trim().parse::<i32>().ok());
        let child_file = pids.with_extension("child");
        let start = Instant::now();
        while (read(&pids).is_none() || read(&child_file).is_none()) && start.elapsed().as_secs() < 5 {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let (leader, child) = (read(&pids).expect("it started"), read(&child_file).expect("its child started"));
        let closing = Instant::now();
        terminals.close_key("chat:a");
        assert!(closing.elapsed().as_millis() < 500, "closing waited for the program: {:?}", closing.elapsed());
        let alive = |pid: i32| {
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
                && !std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| s.contains(") Z "))
        };
        let start = Instant::now();
        while (alive(leader) || alive(child)) && start.elapsed().as_secs() < 5 {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(!alive(leader), "the program ignoring its hangup was killed");
        assert!(!alive(child), "its child in a group of its own was killed");
    }

    /// A `nohup`'d child (it ignores the hangup) outlives its shell, which dies of the hangup, so no leader is left to
    /// escalate for: it is killed all the same, on closing one terminal and on quitting (`close_all`). One that left
    /// the session with `setsid` is detached on purpose and isn't touched (not tested: it would outlive the test).
    #[cfg(unix)]
    #[test]
    fn a_nohup_child_doesnt_outlive_its_closed_terminal() {
        for quit in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let terminals = Terminals::default();
            let pids = dir.path().join("pids");
            let script = format!("nohup sh -c 'echo $$ > {0}; exec sleep 300' >/dev/null 2>&1 & exec sleep 300", pids.display());
            let launch = Launch {
                argv: vec!["sh".into(), "-c".into(), script.into()],
                path: None,
                withheld: Vec::new(),
                cwd: dir.path().into(),
                keep: None,
                env: Vec::new(),
            };
            let term = spawn(1, launch, 80, 24, Arc::new(Mutex::new(None)), None, None).unwrap();
            terminals.terms.lock().unwrap().insert("chat:a".into(), term);
            let read = |p: &std::path::Path| std::fs::read_to_string(p).ok().and_then(|s| s.trim().parse::<i32>().ok());
            let start = Instant::now();
            while read(&pids).is_none() && start.elapsed().as_secs() < 5 {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            let child = read(&pids).expect("the nohup child started");
            let alive = |pid: i32| {
                nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
                    && !std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| s.contains(") Z "))
            };
            assert!(alive(child));
            if quit {
                terminals.close_all();
            } else {
                terminals.close_key("chat:a");
            }
            let start = Instant::now();
            while alive(child) && start.elapsed().as_millis() < 2500 {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            if alive(child) {
                let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(child), nix::sys::signal::Signal::SIGKILL);
            }
            assert!(!alive(child), "the nohup child was killed (quit: {quit})");
        }
    }

    /// "Stop team" ends its members' agents with their whole sessions (a `nohup`'d child too), and only theirs: another
    /// chat's agent runs on, and a member with no terminal running isn't counted.
    #[cfg(unix)]
    #[test]
    fn closing_a_teams_chats_ends_their_whole_sessions_only() {
        let dir = tempfile::tempdir().unwrap();
        let terminals = Terminals::default();
        let start = |key: &str, pids: &std::path::Path| {
            let script = format!("nohup sh -c 'echo $$ > {0}; exec sleep 300' >/dev/null 2>&1 & exec sleep 300", pids.display());
            let launch = Launch {
                argv: vec!["sh".into(), "-c".into(), script.into()],
                path: None,
                withheld: Vec::new(),
                cwd: dir.path().into(),
                keep: None,
                env: Vec::new(),
            };
            let term = spawn(1, launch, 80, 24, Arc::new(Mutex::new(None)), None, None).unwrap();
            terminals.terms.lock().unwrap().insert(key.into(), term);
        };
        let (member, other) = (dir.path().join("member"), dir.path().join("other"));
        start("chat:a", &member);
        start("chat:b", &other);
        let read = |p: &std::path::Path| std::fs::read_to_string(p).ok().and_then(|s| s.trim().parse::<i32>().ok());
        let started = Instant::now();
        while (read(&member).is_none() || read(&other).is_none()) && started.elapsed().as_secs() < 5 {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let (child, others) = (read(&member).expect("its nohup child started"), read(&other).expect("the other started"));
        let alive = |pid: i32| {
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
                && !std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| s.contains(") Z "))
        };
        assert_eq!(terminals.close_chats(&["a".into(), "gone".into()]), vec!["a".to_string()]);
        let waited = Instant::now();
        while alive(child) && waited.elapsed().as_millis() < 2500 {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let killed = !alive(child);
        if !killed {
            let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(child), nix::sys::signal::Signal::SIGKILL);
        }
        assert!(killed, "the member's nohup child was killed");
        assert!(!terminals.running("chat:a"));
        assert!(terminals.running("chat:b") && alive(others), "another chat's agent runs on");
        terminals.close_all();
    }

    /// A terminal started with no view (a team's member, mail's) runs, and keeps what it drew, at most [`HISTORY`], for
    /// the view that attaches later. On Windows this is also ConPTY's start-up question answered without a page
    /// ([`answer_conpty_start`]): unanswered, the program never started (a team's three chats, black). And the question
    /// isn't left in what the page will be shown, or the page would answer it again into the agent.
    #[test]
    fn a_terminal_with_no_view_keeps_its_latest_output() {
        let dir = tempfile::tempdir().unwrap();
        let argv: Vec<std::ffi::OsString> = if cfg!(windows) {
            vec!["cmd".into(), "/c".into(), "echo drawn before anyone looked & ping -n 6 127.0.0.1 >nul".into()]
        } else {
            vec!["sh".into(), "-c".into(), "printf 'drawn before anyone looked'; sleep 5".into()]
        };
        let launch = Launch { argv, path: None, withheld: Vec::new(), cwd: dir.path().into(), keep: None, env: Vec::new() };
        let term = spawn(1, launch, 80, 24, Arc::new(Mutex::new(None)), None, None).unwrap();
        let end = Instant::now() + std::time::Duration::from_secs(5);
        let kept = || String::from_utf8_lossy(&term.history.lock().unwrap().iter().copied().collect::<Vec<u8>>()).into_owned();
        while Instant::now() < end && !kept().contains("drawn before anyone looked") {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(kept().contains("drawn before anyone looked"), "{}", kept());
        assert!(!kept().contains("\x1b[6n"), "ConPTY's question isn't passed on to the page");
    }

    /// Pasted line breaks are CRLF on Windows (from the page's CRs, a message's LFs, or CRLFs), as they are elsewhere.
    #[test]
    fn pastes_get_windows_line_breaks_on_windows_only() {
        assert_eq!(paste_line_breaks("one\ntwo\r\nthree\rfour", true), "one\r\ntwo\r\nthree\r\nfour");
        assert_eq!(paste_line_breaks("one\ntwo\r\nthree\rfour", false), "one\ntwo\r\nthree\rfour");
        assert_eq!(paste_line_breaks("one line", true), "one line");
        // The page's bracketed paste is converted; typed keys never are.
        assert_eq!(pasted_for("\x1b[200~a\rb\x1b[201~", true), "\x1b[200~a\r\nb\x1b[201~");
        assert_eq!(pasted_for("\x1b[200~a\rb\x1b[201~", false), "\x1b[200~a\rb\x1b[201~");
        assert_eq!(pasted_for("\r", true), "\r");
        assert_eq!(pasted_for("\n", true), "\n");
    }

    /// A message is pasted, then Enter a moment later; Codex on Windows gets a right arrow after the paste to end its
    /// paste burst, so the Enter submits instead of becoming a newline, however long the message.
    #[test]
    fn a_typed_message_is_pasted_then_entered_and_codex_on_windows_gets_a_boundary() {
        let long = "line\n".repeat(2000);
        let codex = submission(&long, "codex", true);
        assert!(codex.paste.starts_with("\x1b[200~line\r\n"));
        assert!(codex.paste.ends_with("\x1b[201~\x1b[C"), "the paste ends with the boundary key");
        assert_eq!(codex.enter_after, SUBMIT_DELAY);
        assert_eq!(submission("hi\nthere", "codex", false).paste, "\x1b[200~hi\nthere\x1b[201~");
        assert_eq!(submission("hi\nthere", "claude", true).paste, "\x1b[200~hi\r\nthere\x1b[201~");
        assert!(SUBMIT_DELAY >= std::time::Duration::from_millis(300), "at least herdr's wait");
    }

    #[test]
    fn lazygit_is_the_persons_own_when_its_on_the_path() {
        let own = marv_session::paths::on_path(PROGRAM);
        if own.is_some() {
            assert_eq!(lazygit(), own);
        }
    }

    /// Live, by hand (`--ignored`): the real lazygit draws its panels on the terminal, in a git folder.
    #[test]
    #[ignore]
    fn lazygit_draws_on_its_terminal() {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git").args(["init", "-q"]).current_dir(dir.path()).status().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        let pair = native_pty_system().openpty(size(100, 30)).unwrap();
        let mut command = CommandBuilder::new(PROGRAM);
        command.cwd(dir.path());
        command.env("TERM", "xterm-256color");
        let mut child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        let mut seen = Vec::new();
        let end = Instant::now() + std::time::Duration::from_secs(8);
        while Instant::now() < end && !String::from_utf8_lossy(&seen).contains("a.txt") {
            if let Ok(chunk) = rx.recv_timeout(std::time::Duration::from_millis(200)) {
                seen.extend(chunk);
            }
        }
        let _ = child.kill();
        let seen = String::from_utf8_lossy(&seen);
        assert!(seen.contains("Status") && seen.contains("a.txt"), "{}", &seen[..seen.len().min(2000)]);
    }
}
