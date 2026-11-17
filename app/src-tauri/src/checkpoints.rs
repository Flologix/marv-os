//! Each turn of a chat's agent, kept: the folder's files as they were before the turn and after it, so the person can
//! see what that turn changed (its own diff, not the branch's) and put the files back as they were before it. For every
//! agent alike (Marv, Claude Code, Codex, OpenCode…), in a git repository.
//!
//! - **A checkpoint** is a commit of the chat's folder (tracked and untracked files, `.gitignore` respected) made with
//!   a private index (`<data>/checkpoints/<chat>.bytes.index`), so the person's own index, branch, stash and history
//!   are never touched; it's kept from git's cleanup under `refs/marv/checkpoints/<chat>/`. It holds the files' bytes
//!   exactly as they are on disk ([`SETTINGS`]), so a restore gives back the same bytes.
//! - **A turn** ends when the chat's agent goes idle (its hooks where it has them, else its terminal going quiet:
//!   [`crate::terminal_events`]) and counts from the end of the turn before, or from the terminal's start. Only a turn that changed files is kept ([`KEEP`] at most); a turn
//!   that changed none leaves nothing.
//! - **Restore** puts the files back as they were before a turn (changed and deleted files back, files made since
//!   removed; ignored files left alone), itself kept as a turn, so it can be undone the same way.
//! - Nothing here reaches a model: the agent doesn't know about its checkpoints.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// The turns kept per chat; an older one's refs go.
pub const KEEP: usize = 50;
/// An untracked file bigger than this is left out of the checkpoints: `git add` copies each into the person's
/// `.git/objects`, so a dataset or a VM image in the folder was duplicated on the first turn and again whenever it
/// changed. Tracked files are kept whatever their size (they are in git already).
pub const FILE_MAX: u64 = 50 * 1024 * 1024;
/// At most this many files are named in the `add` command line (Windows' limit is 32k characters).
const LEFT_OUT_MAX: usize = 200;
/// A turn's diff is cut past this, for the page.
pub const DIFF_MAX: usize = 1024 * 1024;

/// One kept turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub n: u32,
    pub started_ms: u64,
    pub ended_ms: u64,
    /// The checkpoints' commits.
    pub before: String,
    pub after: String,
    /// Each file it changed: `A`, `M` or `D`, and its path in the repository.
    pub files: Vec<(String, String)>,
    pub added: u64,
    pub removed: u64,
    /// "Restored to before turn 4", for a restore; none for the agent's own turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// A checkpoint: its commit and tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub commit: String,
    pub tree: String,
}

/// Settings every checkpoint git call runs with, whatever the person's git config says:
/// - `core.longpaths`: Git for Windows refuses a path past 260 characters by default ("Filename too long"), and one
///   such file (a browser's saved page in the folder) failed every checkpoint of the chat. No effect elsewhere.
/// - `core.safecrlf=false` and `advice.addEmbeddedRepo=false`: with `core.autocrlf` on (Git for Windows' default),
///   every text file with LF endings printed a warning on each checkpoint, and every nested repository a page of
///   hints; kilobytes of them buried the one line that said what went wrong in the log.
/// - `core.autocrlf=false`, with [`NO_ATTRIBUTES`]: no line-ending conversion either way. Git for Windows turns
///   `autocrlf` on for everyone, and a restore then wrote CRLF into files that had LF: "put back as they were" gave back
///   every line changed.
const SETTINGS: [&str; 8] =
    ["-c", "core.longpaths=true", "-c", "core.safecrlf=false", "-c", "advice.addEmbeddedRepo=false", "-c", "core.autocrlf=false"];

/// The empty tree, as `GIT_ATTR_SOURCE`: the repository's `.gitattributes` (`text`, `eol=crlf`, filters) don't apply
/// to checkpoints, so a file goes in and comes back byte for byte. Git before 2.42 ignores the variable, and there a
/// `.gitattributes` with `text` rules still converts, as it always did.
const NO_ATTRIBUTES: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

fn run(dir: &Path, args: &[&str], index: Option<&Path>) -> Result<std::process::Output, String> {
    run_pathspecs(dir, args, index, true)
}

/// [`run`], with `literal` off for the one call that writes its pathspecs as `:(literal)…` / `:(exclude,literal)…`
/// itself (an exclude is magic, which `GIT_LITERAL_PATHSPECS` would turn into a file name).
fn run_pathspecs(dir: &Path, args: &[&str], index: Option<&Path>, literal: bool) -> Result<std::process::Output, String> {
    let mut cmd = marv_core::process::command("git");
    cmd.current_dir(dir).args(SETTINGS).args(args).env("GIT_ATTR_SOURCE", NO_ATTRIBUTES);
    // A fixed name for the checkpoints' commits (the person's git may have none set), and no hooks or pager. Paths
    // are literal: a file named `[draft]*.md` is that file, not a pattern (restore passes the diff's paths back).
    for (k, v) in [
        ("GIT_AUTHOR_NAME", "Marv OS"),
        ("GIT_AUTHOR_EMAIL", "checkpoints@marv.local"),
        ("GIT_COMMITTER_NAME", "Marv OS"),
        ("GIT_COMMITTER_EMAIL", "checkpoints@marv.local"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_OPTIONAL_LOCKS", "0"),
    ] {
        cmd.env(k, v);
    }
    if literal {
        cmd.env("GIT_LITERAL_PATHSPECS", "1");
    }
    if let Some(index) = index {
        cmd.env("GIT_INDEX_FILE", index);
    }
    cmd.output().map_err(|e| format!("git: {e}"))
}

/// What went wrong, from git's stderr: its `error:` and `fatal:` lines when it has any, else all of it.
fn gist(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let errors: Vec<&str> = text.lines().filter(|l| l.starts_with("error:") || l.starts_with("fatal:")).collect();
    if errors.is_empty() { text.trim().to_string() } else { errors.join("\n") }
}

fn git(dir: &Path, args: &[&str], index: Option<&Path>) -> Result<String, String> {
    let out = run(dir, args, index)?;
    if !out.status.success() {
        return Err(gist(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// The top of the git repository `dir` is in, and `dir` inside it as git writes it (`""` at the top, else
/// `sub/dir/`); `None` outside one. Asked of git rather than worked out from the paths: on Windows `dir` may be
/// `\\?\C:\…` with backslashes while git answers `C:/…`.
fn repo_place(dir: &Path) -> Option<(PathBuf, String)> {
    let out = git(dir, &["rev-parse", "--show-toplevel", "--show-prefix"], None).ok()?;
    let mut lines = out.lines();
    let top = PathBuf::from(lines.next()?);
    Some((top, lines.next().unwrap_or("").to_string()))
}

/// The files `git add` skipped already told of, per chat, so a file that can't be read isn't logged on every turn.
fn first_time_skipped(chat: &str, what: &str) -> bool {
    static TOLD: Mutex<Option<std::collections::HashSet<(String, String)>>> = Mutex::new(None);
    let mut told = TOLD.lock().unwrap_or_else(|e| e.into_inner());
    told.get_or_insert_with(Default::default).insert((chat.to_string(), what.to_string()))
}

/// The ref a chat's turn checkpoint is kept under: `refs/marv/checkpoints/<chat>/<n>-before`.
fn ref_name(chat: &str, n: u32, side: &str) -> String {
    format!("refs/marv/checkpoints/{chat}/{n}-{side}")
}

/// `git diff --name-status -z` between two commits inside `folder`: each file's status letter and path. `-z` gives
/// paths as they are (git quotes `café.txt` as `"caf\303\251.txt"` otherwise, and a restore then couldn't find it).
/// No renames: a rename is a deletion and an addition, each one path.
fn name_status(top: &Path, from: &str, to: &str, folder: &str) -> Result<Vec<(String, String)>, String> {
    let out = run(top, &["diff", "--name-status", "--no-renames", "-z", from, to, "--", folder], None)?;
    if !out.status.success() {
        return Err(gist(&out.stderr));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut fields = text.split('\0');
    let mut files = Vec::new();
    while let (Some(status), Some(path)) = (fields.next(), fields.next()) {
        files.push((status.chars().take(1).collect(), path.to_string()));
    }
    Ok(files)
}

/// A chat's turns in one repository.
pub struct Book {
    pub chat: String,
    /// The repository's top folder.
    pub top: PathBuf,
    /// The chat's folder in it, as git writes it (`""` when it is the top, else `sub/dir/`): only its files are
    /// checkpointed. Not the whole repository: a chat in `~/Projects/app` inside a repository at `~/Projects` (or a
    /// home folder kept in git) read every other project's files on each turn, and failed on any of them it couldn't
    /// index (a nested repository with no commit yet), so its own files were never kept.
    pub prefix: String,
    /// `<data>/checkpoints`.
    pub dir: PathBuf,
}

impl Book {
    /// `.bytes.index`: not the old `.index`, which began as a copy of the person's index and so held their files as
    /// git had converted them (LF for CRLF on Windows), not as they were on disk.
    fn index(&self) -> PathBuf {
        self.dir.join(format!("{}.bytes.index", self.chat))
    }

    /// The chat's folder as a pathspec: every diff and restore stays inside it, also for a turn kept before
    /// checkpoints were per folder (whose commits hold the whole repository).
    fn folder(&self) -> &str {
        if self.prefix.is_empty() { "." } else { &self.prefix }
    }

    fn file(&self) -> PathBuf {
        self.dir.join(format!("{}.json", self.chat))
    }

    pub fn turns(&self) -> Vec<Turn> {
        std::fs::read_to_string(self.file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    fn save(&self, turns: &[Turn]) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let temp = self.file().with_extension("json.tmp");
        std::fs::write(&temp, serde_json::to_string_pretty(turns).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        std::fs::rename(&temp, self.file()).map_err(|e| e.to_string())
    }

    /// The chat's folder now, as a commit, from the chat's own index. Not started from a copy of the person's index (as
    /// it once was, for speed): that holds their files as git converted them, and a restore from it wrote back other
    /// bytes. So a chat's first checkpoint reads its whole folder once; later ones only what changed.
    /// Callers hold the chat's lock ([`locked`], or the [`Recorder`]'s thread).
    pub fn snapshot(&self) -> Result<Snapshot, String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let index = self.index();
        // A lock git left by our private index is stale: only this chat's git touches that index, one call at a time
        // under the chat's lock. Git for Windows leaves one when it can't delete it (an antivirus or the search
        // indexer still has it open, and with no terminal it can't ask to retry), and from then on every checkpoint
        // of the chat failed with "index.lock: File exists" until the person found and removed it. The old index's
        // files go too (see [`Book::index`]).
        for stale in [".bytes.index.lock", ".index", ".index.lock"] {
            let _ = std::fs::remove_file(self.dir.join(format!("{}{stale}", self.chat)));
        }
        // `--ignore-errors`: a file git can't index (unreadable, a nested repository with no commit yet) is left out
        // of the checkpoint, said once in the log, instead of failing it, so one odd file doesn't cost the chat every
        // turn. git still exits 1 then; 128 is a real failure.
        // Untracked files past [`FILE_MAX`] stay out: excluded from the add, since adding them is the copy.
        let big = self.untracked_big_files();
        let folder = format!(":(literal){}", self.folder());
        let excludes: Vec<String> = big.iter().map(|p| format!(":(exclude,literal){p}")).collect();
        let mut args = vec!["add", "--all", "--ignore-errors", "--", folder.as_str()];
        args.extend(excludes.iter().map(String::as_str));
        let out = run_pathspecs(&self.top, &args, Some(&index), false)?;
        match out.status.code() {
            Some(0) => {}
            Some(1) => {
                let skipped = gist(&out.stderr);
                if first_time_skipped(&self.chat, &skipped) {
                    crate::applog::warn("checkpoints", &format!("left out of this chat's checkpoints: {skipped}"));
                }
            }
            _ => return Err(gist(&out.stderr)),
        }
        let tree = git(&self.top, &["write-tree"], Some(&index))?;
        let commit = git(&self.top, &["commit-tree", &tree, "-m", "Marv OS checkpoint"], None)?;
        Ok(Snapshot { commit, tree })
    }

    /// The chat's folder's files the person's git doesn't track (ignored ones aside) that are bigger than [`FILE_MAX`],
    /// as git writes their paths; at most [`LEFT_OUT_MAX`], the rest are kept as before. Said once in the log.
    fn untracked_big_files(&self) -> Vec<String> {
        let Ok(out) = run(&self.top, &["ls-files", "--others", "--exclude-standard", "-z", "--", self.folder()], None) else {
            return Vec::new();
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let big: Vec<String> = text
            .split('\0')
            .filter(|p| !p.is_empty())
            .filter(|p| std::fs::symlink_metadata(self.top.join(p)).is_ok_and(|m| m.is_file() && m.len() > FILE_MAX))
            .take(LEFT_OUT_MAX)
            .map(str::to_string)
            .collect();
        if let Some(first) = big.first()
            && first_time_skipped(&self.chat, &format!("big:{first}"))
        {
            crate::applog::warn(
                "checkpoints",
                &format!(
                    "{} untracked file(s) over {} MB are left out of this chat's checkpoints, e.g. {first}",
                    big.len(),
                    FILE_MAX >> 20
                ),
            );
        }
        big
    }

    /// Keeps the turn from `before` to `after` if it changed files ([`KEEP`] at most); the turn kept.
    pub fn keep(
        &self,
        before: &Snapshot,
        after: &Snapshot,
        started_ms: u64,
        ended_ms: u64,
        label: Option<String>,
    ) -> Result<Option<Turn>, String> {
        if before.tree == after.tree {
            return Ok(None);
        }
        let files = name_status(&self.top, &before.commit, &after.commit, self.folder())?;
        let (mut added, mut removed) = (0, 0);
        for line in
            git(&self.top, &["diff", "--numstat", "--no-renames", &before.commit, &after.commit, "--", self.folder()], None)?
                .lines()
        {
            let mut cols = line.split('\t');
            added += cols.next().and_then(|n| n.parse::<u64>().ok()).unwrap_or(0);
            removed += cols.next().and_then(|n| n.parse::<u64>().ok()).unwrap_or(0);
        }
        let mut turns = self.turns();
        let n = turns.last().map_or(1, |t| t.n + 1);
        git(&self.top, &["update-ref", &ref_name(&self.chat, n, "before"), &before.commit], None)?;
        git(&self.top, &["update-ref", &ref_name(&self.chat, n, "after"), &after.commit], None)?;
        let turn = Turn {
            n,
            started_ms,
            ended_ms,
            before: before.commit.clone(),
            after: after.commit.clone(),
            files,
            added,
            removed,
            label,
        };
        turns.push(turn.clone());
        while turns.len() > KEEP {
            let old = turns.remove(0);
            for side in ["before", "after"] {
                let _ = git(&self.top, &["update-ref", "-d", &ref_name(&self.chat, old.n, side)], None);
            }
        }
        self.save(&turns)?;
        Ok(Some(turn))
    }

    /// Turn `n`'s diff, cut at [`DIFF_MAX`].
    pub fn diff(&self, n: u32) -> Result<String, String> {
        let turn = self.turns().into_iter().find(|t| t.n == n).ok_or("That turn isn't kept any more.")?;
        let mut text =
            git(&self.top, &["diff", "--no-renames", "--no-color", &turn.before, &turn.after, "--", self.folder()], None)?;
        if text.len() > DIFF_MAX {
            let mut cut = DIFF_MAX;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
            text.push_str("\n… (cut here: the rest of this turn's diff is too long to show)\n");
        }
        Ok(text)
    }

    /// Puts the files back as they were before turn `n`, kept as a turn of its own; what it did.
    pub fn restore(&self, n: u32, now_ms: u64) -> Result<String, String> {
        let turn = self.turns().into_iter().find(|t| t.n == n).ok_or("That turn isn't kept any more.")?;
        let now = self.snapshot()?;
        let changed = name_status(&self.top, &turn.before, &now.commit, self.folder())?;
        let mut put_back = Vec::new();
        let mut made = Vec::new();
        for (status, path) in changed {
            if status == "A" { made.push(path) } else { put_back.push(path) }
        }
        // Changed and deleted files first: if that fails, nothing was removed yet, so the folder isn't left half-way
        // with its new files gone and the old ones not back.
        let mut done = 0;
        for chunk in put_back.chunks(200) {
            let mut args = vec!["restore", "--worktree", "--source", turn.before.as_str(), "--"];
            args.extend(chunk.iter().map(String::as_str));
            git(&self.top, &args, None).map_err(|e| {
                format!("Couldn't put the files back ({e}). {done} of {} were put back and none removed; the rest of the folder is as it was.", put_back.len())
            })?;
            done += chunk.len();
        }
        // Made since: not in the checkpoint, so they go (inside the repository only).
        let (mut removed, mut stuck) = (0, Vec::new());
        for path in &made {
            let file = self.top.join(path);
            if !file.starts_with(&self.top) {
                continue;
            }
            match std::fs::remove_file(&file) {
                Ok(()) => removed += 1,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => stuck.push(format!("{path} ({e})")),
            }
        }
        let after = self.snapshot()?;
        self.keep(&now, &after, now_ms, now_ms, Some(format!("Restored to before turn {n}")))?;
        if !stuck.is_empty() {
            return Err(format!(
                "{} put back and {removed} removed, but {} new file(s) couldn't be removed: {}.",
                put_back.len(),
                stuck.len(),
                stuck.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
            ));
        }
        Ok(format!(
            "The files are back as they were before turn {n}: {} put back, {removed} removed. This restore is kept as a turn, so it can be undone the same way.",
            put_back.len()
        ))
    }
}

/// One chat's turns as its terminal runs: [`Recorder::began`] when its agent starts working, [`Recorder::idle`] when it
/// stops. The git work runs on a thread of its own, in the order they came, never on the caller's.
pub struct Recorder {
    jobs: std::sync::mpsc::Sender<Job>,
    working: Mutex<bool>,
    /// Told once the first checkpoint is taken ([`Recorder::wait_first`]).
    #[cfg(test)]
    first: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

enum Job {
    Began,
    Idle,
}

/// Each chat's lock, so two terminals of one chat (or a restore) never run git on its index at once.
fn chat_lock(chat: &str) -> Arc<Mutex<()>> {
    static LOCKS: Mutex<Option<HashMap<String, Arc<Mutex<()>>>>> = Mutex::new(None);
    let mut locks = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    locks.get_or_insert_with(HashMap::new).entry(chat.to_string()).or_default().clone()
}

impl Recorder {
    /// For chat `chat` working in `dir`; `None` outside a git repository. Its first checkpoint is taken first thing
    /// on its own thread, as the terminal starts: a turn counts from the end of the one before (or from there). Never
    /// before the terminal starts: a checkpoint of a big folder from scratch (a Unity project's 1.2 GB of assets, its
    /// private index new) took most of a minute, the chat's terminal black all the while (seen 2026-11-17). Turns told
    /// meanwhile wait for it. Its thread ends with it.
    pub fn new(chat: &str, dir: &Path, data: &Path, now: fn() -> u64) -> Option<Recorder> {
        let book = book_for(chat, dir, data)?;
        let (jobs, inbox) = std::sync::mpsc::channel::<Job>();
        let (taken, first) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            let mut started = now();
            let mut last = match locked(&book, |b| b.snapshot()) {
                Ok(snap) => Some(snap),
                Err(e) => {
                    crate::applog::warn("checkpoints", &format!("no first checkpoint: {e}"));
                    None
                }
            };
            let _ = taken.send(());
            for job in inbox {
                let lock = chat_lock(&book.chat);
                let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
                match job {
                    Job::Began => started = now(),
                    Job::Idle => match book.snapshot() {
                        Ok(after) => {
                            if let Some(before) = &last
                                && let Err(e) = book.keep(before, &after, started, now(), None)
                            {
                                crate::applog::warn("checkpoints", &format!("a turn wasn't kept: {e}"));
                            }
                            last = Some(after);
                        }
                        Err(e) => crate::applog::warn("checkpoints", &format!("no checkpoint after a turn: {e}")),
                    },
                }
            }
        });
        #[cfg(not(test))]
        drop(first);
        Some(Recorder {
            jobs,
            working: Mutex::new(false),
            #[cfg(test)]
            first: Mutex::new(Some(first)),
        })
    }

    /// Waits until its first checkpoint is taken (or failed).
    #[cfg(test)]
    pub fn wait_first(&self) {
        if let Some(first) = self.first.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = first.recv();
        }
    }

    fn turn(&self, working: bool, job: Job) {
        let mut was = self.working.lock().unwrap_or_else(|e| e.into_inner());
        if *was != working {
            *was = working;
            let _ = self.jobs.send(job);
        }
    }

    pub fn began(&self) {
        self.turn(true, Job::Began);
    }

    pub fn idle(&self) {
        self.turn(false, Job::Idle);
    }
}

/// Chat `chat`'s turns where it works (`dir`), for the page.
pub fn book_for(chat: &str, dir: &Path, data: &Path) -> Option<Book> {
    let (top, prefix) = repo_place(dir)?;
    Some(Book { chat: chat.to_string(), top, prefix, dir: data.join("checkpoints") })
}

/// Chat `chat` is deleted: its refs under `refs/marv/checkpoints/<chat>/` (which keep every checkpoint's files in the
/// repository's objects for good) and its two files in `<data>/checkpoints` go. `dir` is a folder of the repository the
/// chat worked in (its own, or the main folder if a worktree is already removed); a repository that is gone is fine.
pub fn forget(chat: &str, dir: &Path, data: &Path) {
    let checkpoints = data.join("checkpoints");
    for name in
        [format!("{chat}.bytes.index"), format!("{chat}.bytes.index.lock"), format!("{chat}.index"), format!("{chat}.json")]
    {
        let _ = std::fs::remove_file(checkpoints.join(name));
    }
    let existing = dir.ancestors().find(|d| d.is_dir());
    let Some((top, _)) = existing.and_then(repo_place) else { return };
    let prefix = format!("refs/marv/checkpoints/{chat}/");
    let Ok(listed) = git(&top, &["for-each-ref", "--format=%(refname)", &prefix], None) else { return };
    let commands: String = listed.lines().filter(|r| r.starts_with(&prefix)).map(|r| format!("delete {r}\n")).collect();
    if commands.is_empty() {
        return;
    }
    // One transaction: `update-ref --stdin` takes the refs' names as lines.
    let child = marv_core::process::command("git")
        .current_dir(&top)
        .args(SETTINGS)
        .args(["update-ref", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn();
    if let Ok(mut child) = child {
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(commands.as_bytes());
        }
        if let Ok(out) = child.wait_with_output()
            && !out.status.success()
        {
            crate::applog::warn(
                "checkpoints",
                &format!("a deleted chat's checkpoint refs weren't removed: {}", gist(&out.stderr)),
            );
        }
    }
}

/// Runs `f` on a chat's book holding its lock (a restore never races a turn's checkpoint).
pub fn locked<T>(book: &Book, f: impl FnOnce(&Book) -> T) -> T {
    let lock = chat_lock(&book.chat);
    let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
    f(book)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"], None).unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "build/\n").unwrap();
        git(dir.path(), &["add", "."], None).unwrap();
        git(dir.path(), &["commit", "-q", "-m", "first"], None).unwrap();
        dir
    }

    /// A turn's changes are kept with their own diff, the person's index and branch untouched; a turn that changed
    /// nothing leaves nothing; restore puts the files back (a new file removed, ignored files left) and is a turn too.
    #[test]
    fn a_turn_is_kept_with_its_own_diff_and_restored_without_touching_the_persons_git() {
        let project = repo();
        let data = tempfile::tempdir().unwrap();
        let book = book_for("chat1", project.path(), data.path()).unwrap();
        let status = || git(project.path(), &["status", "--porcelain"], None).unwrap();
        let head = || git(project.path(), &["rev-parse", "HEAD"], None).unwrap();
        let (head0, status0) = (head(), status());

        let before = book.snapshot().unwrap();
        assert!(book.keep(&before, &book.snapshot().unwrap(), 1, 2, None).unwrap().is_none(), "nothing changed");
        std::fs::write(project.path().join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(project.path().join("new.txt"), "fresh\n").unwrap();
        std::fs::create_dir_all(project.path().join("build")).unwrap();
        std::fs::write(project.path().join("build/out.bin"), "ignored").unwrap();
        let turn = book.keep(&before, &book.snapshot().unwrap(), 1, 2, None).unwrap().unwrap();
        assert_eq!(turn.n, 1);
        assert_eq!(turn.files, vec![("M".to_string(), "a.txt".to_string()), ("A".to_string(), "new.txt".to_string())]);
        assert_eq!((turn.added, turn.removed), (2, 0));
        assert!(book.diff(1).unwrap().contains("+two"));
        assert_eq!(head(), head0, "no commit on the person's branch");
        assert!(
            !git(project.path(), &["diff", "--cached", "--name-only"], None).unwrap().contains("a.txt"),
            "their index untouched"
        );

        let said = book.restore(1, 3).unwrap();
        assert!(said.contains("1 put back, 1 removed"), "{said}");
        assert_eq!(std::fs::read_to_string(project.path().join("a.txt")).unwrap(), "one\n");
        assert!(!project.path().join("new.txt").exists());
        assert!(project.path().join("build/out.bin").exists(), "ignored files are left alone");
        assert_eq!(status(), status0);
        let turns = book.turns();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[1].label.as_deref(), Some("Restored to before turn 1"));
        // Kept from git's cleanup under the chat's refs.
        let refs = git(project.path(), &["for-each-ref", "--format=%(refname)", "refs/marv/checkpoints/chat1"], None).unwrap();
        assert_eq!(refs.lines().count(), 4);
        // And the restore can be undone the same way.
        book.restore(2, 4).unwrap();
        assert_eq!(std::fs::read_to_string(project.path().join("a.txt")).unwrap(), "one\ntwo\n");
        assert!(project.path().join("new.txt").exists());
    }

    /// The terminal's turns, told as they happen: one that changed files is kept, a busy spell that changed none isn't.
    #[test]
    fn a_terminals_turns_are_kept_in_order() {
        let project = repo();
        let data = tempfile::tempdir().unwrap();
        let recorder = Recorder::new("c", project.path(), data.path(), || 7).unwrap();
        recorder.wait_first();
        recorder.began();
        recorder.began();
        std::fs::write(project.path().join("a.txt"), "changed\n").unwrap();
        recorder.idle();
        recorder.began();
        recorder.idle();
        drop(recorder);
        let book = book_for("c", project.path(), data.path()).unwrap();
        let mut turns = Vec::new();
        for _ in 0..100 {
            turns = book.turns();
            if !turns.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        let turns = if turns.is_empty() { turns } else { book.turns() };
        assert_eq!(turns.len(), 1, "{turns:?}");
        assert_eq!(turns[0].files, vec![("M".to_string(), "a.txt".to_string())]);
    }

    /// A chat in a folder of a bigger repository (`~/Projects/app` in a repository at `~/Projects`): its turns are its
    /// folder's, and what git can't index elsewhere in the repository (a nested repository with no commit, which failed
    /// every checkpoint of such a chat) doesn't matter. Restore works from the folder too.
    #[test]
    fn a_chat_in_a_folder_of_a_bigger_repository_keeps_only_its_folder() {
        let project = repo();
        let app = project.path().join("app");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("main.rs"), "fn main() {}\n").unwrap();
        let empty = project.path().join("other/empty");
        std::fs::create_dir_all(&empty).unwrap();
        git(&empty, &["init", "-q"], None).unwrap();
        let data = tempfile::tempdir().unwrap();
        let book = book_for("c", &app, data.path()).unwrap();
        assert_eq!(book.prefix, "app/");

        let before = book.snapshot().unwrap();
        std::fs::write(app.join("main.rs"), "fn main() { run() }\n").unwrap();
        std::fs::write(project.path().join("a.txt"), "someone else's change\n").unwrap();
        let turn = book.keep(&before, &book.snapshot().unwrap(), 1, 2, None).unwrap().unwrap();
        assert_eq!(turn.files, vec![("M".to_string(), "app/main.rs".to_string())]);

        book.restore(1, 3).unwrap();
        assert_eq!(std::fs::read_to_string(app.join("main.rs")).unwrap(), "fn main() {}\n");
        assert_eq!(
            std::fs::read_to_string(project.path().join("a.txt")).unwrap(),
            "someone else's change\n",
            "outside the folder: left"
        );
    }

    /// A file git can't index inside the chat's own folder is left out of its checkpoints, not the end of them.
    #[test]
    fn what_git_cant_index_is_left_out_not_fatal() {
        let project = repo();
        // A nested repository with no commit yet: "does not have a commit checked out".
        std::fs::create_dir_all(project.path().join("x")).unwrap();
        git(&project.path().join("x"), &["init", "-q"], None).unwrap();
        let data = tempfile::tempdir().unwrap();
        let book = book_for("c", project.path(), data.path()).unwrap();
        let before = book.snapshot().unwrap();
        std::fs::write(project.path().join("a.txt"), "two\n").unwrap();
        let turn = book.keep(&before, &book.snapshot().unwrap(), 1, 2, None).unwrap().unwrap();
        assert_eq!(turn.files, vec![("M".to_string(), "a.txt".to_string())]);
    }

    /// A lock git left on the chat's private index (Git for Windows when something still had it open) doesn't stop
    /// every checkpoint after it.
    #[test]
    fn a_stale_lock_on_the_private_index_is_cleared() {
        let project = repo();
        let data = tempfile::tempdir().unwrap();
        let book = book_for("c", project.path(), data.path()).unwrap();
        std::fs::create_dir_all(&book.dir).unwrap();
        for lock in ["c.bytes.index.lock", "c.index.lock"] {
            std::fs::write(book.dir.join(lock), "").unwrap();
        }
        book.snapshot().unwrap();
        assert!(!book.dir.join("c.bytes.index.lock").exists());
        assert!(!book.dir.join("c.index.lock").exists(), "the old index's lock goes too");
    }

    /// A restore gives back the bytes that were there: LF, CRLF and mixed endings alike, whatever `core.autocrlf` (on
    /// for everyone with Git for Windows) and the repository's `.gitattributes` say.
    #[test]
    fn a_restore_gives_back_the_same_bytes() {
        let project = repo();
        git(project.path(), &["config", "core.autocrlf", "true"], None).unwrap();
        std::fs::write(project.path().join(".gitattributes"), "* text=auto eol=crlf\n").unwrap();
        let files: [(&str, &[u8]); 3] =
            [("lf.txt", b"one\ntwo\n"), ("crlf.txt", b"one\r\ntwo\r\n"), ("mixed.txt", b"one\ntwo\r\n")];
        for (name, bytes) in files {
            std::fs::write(project.path().join(name), bytes).unwrap();
        }
        let data = tempfile::tempdir().unwrap();
        let book = book_for("c", project.path(), data.path()).unwrap();
        let before = book.snapshot().unwrap();
        for (name, _) in files {
            std::fs::write(project.path().join(name), "changed\n").unwrap();
        }
        book.keep(&before, &book.snapshot().unwrap(), 1, 2, None).unwrap().unwrap();
        book.restore(1, 3).unwrap();
        for (name, bytes) in files {
            assert_eq!(std::fs::read(project.path().join(name)).unwrap(), bytes, "{name}");
        }
    }

    /// Git for Windows refuses a path past 260 characters unless `core.longpaths` is on; one such file failed every
    /// checkpoint of the chat.
    #[cfg(windows)]
    #[test]
    fn a_path_past_windows_260_characters_is_kept() {
        let project = repo();
        let deep = project.path().join("L".repeat(120)).join("M".repeat(120));
        std::fs::create_dir_all(&deep).unwrap();
        let data = tempfile::tempdir().unwrap();
        let book = book_for("c", project.path(), data.path()).unwrap();
        let before = book.snapshot().unwrap();
        std::fs::write(deep.join(format!("{}.download", "N".repeat(40))), "page\n").unwrap();
        let turn = book.keep(&before, &book.snapshot().unwrap(), 1, 2, None).unwrap().unwrap();
        assert_eq!(turn.files.len(), 1, "{:?}", turn.files);
        assert!(turn.files[0].1.ends_with(".download"));
    }

    #[test]
    fn outside_a_repository_there_are_no_checkpoints() {
        let plain = tempfile::tempdir().unwrap();
        assert!(book_for("c", plain.path(), plain.path()).is_none());
        assert!(Recorder::new("c", plain.path(), plain.path(), || 0).is_none());
    }

    /// Only the last [`KEEP`] turns are kept; an older one's refs go with it.
    #[test]
    fn old_turns_go_with_their_refs() {
        let project = repo();
        let data = tempfile::tempdir().unwrap();
        let book = book_for("c", project.path(), data.path()).unwrap();
        let mut before = book.snapshot().unwrap();
        for i in 0..(KEEP + 2) {
            std::fs::write(project.path().join("a.txt"), format!("{i}\n")).unwrap();
            let after = book.snapshot().unwrap();
            book.keep(&before, &after, 0, 0, None).unwrap();
            before = after;
        }
        let turns = book.turns();
        assert_eq!((turns.len(), turns[0].n), (KEEP, 3));
        let refs = git(project.path(), &["for-each-ref", "--format=%(refname)", "refs/marv/checkpoints/c"], None).unwrap();
        assert_eq!(refs.lines().count(), KEEP * 2);
    }

    /// Paths git would quote (non-ASCII, a space) restore like any other, and a failing step doesn't leave new files
    /// deleted with the old ones not back.
    #[test]
    fn a_restore_handles_names_git_would_quote() {
        let project = repo();
        let names = ["Übersicht ä.md", "with space.txt"];
        std::fs::write(project.path().join(names[0]), "alt\n").unwrap();
        let data = tempfile::tempdir().unwrap();
        let book = book_for("c", project.path(), data.path()).unwrap();
        let before = book.snapshot().unwrap();
        std::fs::write(project.path().join(names[0]), "neu\n").unwrap();
        std::fs::write(project.path().join(names[1]), "new\n").unwrap();
        let turn = book.keep(&before, &book.snapshot().unwrap(), 1, 2, None).unwrap().unwrap();
        assert!(turn.files.contains(&("M".to_string(), names[0].to_string())), "{:?}", turn.files);
        assert!(turn.files.contains(&("A".to_string(), names[1].to_string())), "{:?}", turn.files);
        book.restore(1, 3).unwrap();
        assert_eq!(std::fs::read_to_string(project.path().join(names[0])).unwrap(), "alt\n");
        assert!(!project.path().join(names[1]).exists());
        assert_eq!(book.turns().last().unwrap().label.as_deref(), Some("Restored to before turn 1"));
    }

    /// Deleting a chat drops its refs and files; a missing repository or folder is fine.
    #[test]
    fn forgetting_a_chat_drops_its_refs_and_files() {
        let project = repo();
        let data = tempfile::tempdir().unwrap();
        let book = book_for("c", project.path(), data.path()).unwrap();
        let before = book.snapshot().unwrap();
        std::fs::write(project.path().join("a.txt"), "two\n").unwrap();
        book.keep(&before, &book.snapshot().unwrap(), 1, 2, None).unwrap().unwrap();
        let refs = |chat: &str| {
            git(project.path(), &["for-each-ref", "--format=%(refname)", &format!("refs/marv/checkpoints/{chat}")], None).unwrap()
        };
        assert_eq!(refs("c").lines().count(), 2);
        forget("c", project.path(), data.path());
        assert!(refs("c").is_empty());
        assert!(!book.file().exists() && !book.index().exists());
        forget("c", &project.path().join("gone/deeper"), data.path());
        forget("c", Path::new("/nonexistent-marv-test/x"), &data.path().join("none"));
    }

    /// An untracked file past [`FILE_MAX`] isn't copied into the repository's objects; a tracked one still is.
    #[test]
    fn a_big_untracked_file_is_left_out_of_the_checkpoint() {
        let project = repo();
        let big = std::fs::File::create(project.path().join("image.bin")).unwrap();
        big.set_len(FILE_MAX + 1).unwrap();
        std::fs::write(project.path().join("small.txt"), "s\n").unwrap();
        let data = tempfile::tempdir().unwrap();
        let book = book_for("c", project.path(), data.path()).unwrap();
        let snap = book.snapshot().unwrap();
        let tree = git(project.path(), &["ls-tree", "-r", "--name-only", &snap.tree], None).unwrap();
        assert!(tree.contains("small.txt") && tree.contains("a.txt"), "{tree}");
        assert!(!tree.contains("image.bin"), "{tree}");
    }
}
