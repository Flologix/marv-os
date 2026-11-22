//! The chat dock's data (kernel #53): a chat's changes (the git diff of the folder it works in, or of its worktree's
//! branch since it started) and which files in it are in a chat's write set (runtime-kernel §14.4). The page only
//! renders what this says; nothing here writes.

use std::path::Path;

use serde::Serialize;

/// The most files and diff lines the dock is sent; past them it says how many more there were.
const MAX_FILES: usize = 200;
const MAX_LINES: usize = 4000;
/// A diff line longer than this is cut (a minified file's one line).
const MAX_LINE_CHARS: usize = 400;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiffLine {
    /// "add", "del", "ctx", or "gap" (a hunk's header, its text the `@@` line's trailing context).
    pub kind: &'static str,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileChange {
    /// Relative to the chat's folder.
    pub path: String,
    /// "modified", "added", "deleted", "renamed", "binary" or "untracked".
    pub status: &'static str,
    pub added: u32,
    pub removed: u32,
    pub lines: Vec<DiffLine>,
    /// Lines left out past the dock's limit.
    pub more: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct Changes {
    /// What the diff is against: "HEAD" (uncommitted changes), or the worktree's base hash.
    pub against: String,
    pub files: Vec<FileChange>,
    /// Files left out past the dock's limit.
    pub more_files: u32,
    /// Why there is no diff (not a git repository, git missing), if there isn't one.
    pub problem: Option<String>,
}

/// Git in `dir`, read-only and without anything a repository's own config could run: no fsmonitor hook, no optional
/// index lock (and the diff below asks for no external diff or textconv driver). Its stdout when it succeeds.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = marv_core::process::command("git")
        .args(["-c", "core.fsmonitor=false", "-c", "core.quotePath=false", "--no-pager"])
        .args(args)
        .current_dir(dir)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GIT_EXTERNAL_DIFF")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound if dir.is_dir() => crate::checkpoints::GIT_MISSING.to_string(),
            _ => format!("git didn't run: {e}"),
        })?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let first = err.lines().next().unwrap_or("git failed").trim();
        return Err(if first.contains("not a git repository") {
            "This folder isn't a git repository.".into()
        } else {
            first.to_string()
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The changes under `root`: against `base` (a worktree chat's starting commit: its branch's work, committed or not),
/// else against HEAD (what isn't committed yet), plus the untracked files.
pub fn changes(root: &Path, base: Option<&str>) -> Changes {
    let against = base.filter(|b| !b.is_empty() && b.chars().all(|c| c.is_ascii_hexdigit())).unwrap_or("HEAD").to_string();
    // The prefixes said outright: a person's diff.mnemonicPrefix or diff.noPrefix would change what parse_diff reads.
    let args = [
        "diff",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        "--relative",
        "-M",
        &against,
        "--",
        ".",
    ];
    let diff = match git(root, &args) {
        Ok(text) => text,
        Err(problem) => return Changes { against, problem: Some(problem), ..Changes::default() },
    };
    let mut files = parse_diff(&diff);
    if let Ok(list) = git(root, &["ls-files", "--others", "--exclude-standard", "-z", "--", "."]) {
        files.extend(list.split('\0').filter(|p| !p.is_empty()).map(|path| FileChange {
            path: path.to_string(),
            status: "untracked",
            added: 0,
            removed: 0,
            lines: Vec::new(),
            more: 0,
        }));
    }
    let more_files = files.len().saturating_sub(MAX_FILES) as u32;
    files.truncate(MAX_FILES);
    Changes { against, files, more_files, problem: None }
}

/// A unified diff (`git diff`) as files and lines, at most [`MAX_LINES`] lines in all (the rest counted per file).
pub fn parse_diff(text: &str) -> Vec<FileChange> {
    let mut files: Vec<FileChange> = Vec::new();
    let mut budget = MAX_LINES;
    let mut in_hunk = false;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            // `a/x b/y`: the b side, until a `+++` or rename line says better.
            let path = rest.split(" b/").last().unwrap_or(rest).to_string();
            files.push(FileChange { path, status: "modified", added: 0, removed: 0, lines: Vec::new(), more: 0 });
            in_hunk = false;
            continue;
        }
        let Some(file) = files.last_mut() else { continue };
        if !in_hunk {
            if line.starts_with("new file mode") {
                file.status = "added";
            } else if line.starts_with("deleted file mode") {
                file.status = "deleted";
            } else if let Some(to) = line.strip_prefix("rename to ") {
                file.status = "renamed";
                file.path = to.to_string();
            } else if line.starts_with("Binary files") {
                file.status = "binary";
            } else if let Some(path) = line.strip_prefix("+++ b/") {
                file.path = path.to_string();
            }
        }
        if let Some(header) = line.strip_prefix("@@") {
            in_hunk = true;
            let context = header.split_once("@@").map_or("", |(_, rest)| rest).trim();
            push(file, &mut budget, "gap", context);
            continue;
        }
        if !in_hunk {
            continue;
        }
        match line.as_bytes().first() {
            Some(b'+') => {
                file.added += 1;
                push(file, &mut budget, "add", &line[1..]);
            }
            Some(b'-') => {
                file.removed += 1;
                push(file, &mut budget, "del", &line[1..]);
            }
            Some(b' ') => push(file, &mut budget, "ctx", &line[1..]),
            _ => {} // "\ No newline at end of file"
        }
    }
    files
}

fn push(file: &mut FileChange, budget: &mut usize, kind: &'static str, text: &str) {
    if *budget == 0 {
        file.more += 1;
        return;
    }
    *budget -= 1;
    let text = if text.chars().count() > MAX_LINE_CHARS {
        text.chars().take(MAX_LINE_CHARS).collect::<String>() + "…"
    } else {
        text.to_string()
    };
    file.lines.push(DiffLine { kind, text });
}

/// A file in a chat's folder that a chat (or another Marv) wrote in the last 30 minutes: the dock marks it with who.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteMark {
    /// Relative to the chat's folder.
    pub path: String,
    /// The writer's title (a desktop chat) or mesh name (another Marv).
    pub by: String,
    /// The desktop chat that wrote it, when it is one.
    pub chat: Option<String>,
    /// This chat wrote it.
    pub mine: bool,
    pub at_ms: u64,
}

/// The marks under `root` (a real path) for chat `me`, from each desktop chat's write set (`sets`: chat id, its
/// title, its writes as real paths). Paths outside the folder are left out; newest first.
pub fn marks(root: &Path, me: &str, sets: &[(String, String, Vec<marv_kernel::RecentWrite>)]) -> Vec<WriteMark> {
    let mut out: Vec<WriteMark> = sets
        .iter()
        .flat_map(|(id, title, writes)| {
            writes.iter().filter_map(move |w| {
                let rel = w.path.strip_prefix(root).ok()?.to_string_lossy().into_owned();
                (!rel.is_empty()).then(|| WriteMark {
                    path: rel,
                    by: if title.is_empty() { "a chat".into() } else { title.clone() },
                    chat: Some(id.clone()),
                    mine: id == me,
                    at_ms: w.at_ms,
                })
            })
        })
        .collect();
    out.sort_by(|a, b| b.at_ms.cmp(&a.at_ms).then_with(|| a.path.cmp(&b.path)));
    out
}

/// The most log lines the dock asks for (the process tool's own cap is higher; the dock shows a tail).
pub const LOG_LINES: usize = 200;

/// One background process a chat started with the process tool (mk-60m.11), for the dock's Terminal tab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProcessView {
    pub name: String,
    pub command: String,
    /// "running" or "exited".
    pub state: &'static str,
    pub exit_code: Option<i32>,
    /// What the person opens in their browser: `http://localhost:<port>` for each forwarded port (or, with network,
    /// each of the host's own ports it listens on), as the process tool tells the model. None once it exited.
    pub urls: Vec<String>,
    pub network: bool,
    /// What wasn't forwarded, and why.
    pub notes: Vec<String>,
    /// When it started (ms since the epoch): also which start this is, for Stop (`same_start`).
    pub started_at: u64,
}

/// The session's processes as the dock shows them; every string the process or the model wrote made printable.
pub fn processes(list: &[marv_session::processes::ProcessInfo]) -> Vec<ProcessView> {
    use marv_session::processes::ProcessState;
    list.iter()
        .map(|p| {
            let running = p.state == ProcessState::Running;
            ProcessView {
                name: marv_core::printable(&p.name),
                command: marv_core::printable(&p.command),
                state: if running { "running" } else { "exited" },
                exit_code: p.exit_code,
                urls: if running { p.ports.iter().map(|port| format!("http://localhost:{port}")).collect() } else { Vec::new() },
                network: p.network,
                notes: p.notes.iter().map(|n| marv_core::printable(n)).collect(),
                started_at: p.started_at,
            }
        })
        .collect()
}

/// Whether `name` is still the start the person saw (`started_at`, which tells two starts of one name apart: a new
/// start comes after the old one was stopped, so later): true to stop it, false when it's gone, an error when a new
/// start of that name replaced it.
pub fn same_start(list: &[marv_session::processes::ProcessInfo], name: &str, started_at: u64) -> Result<bool, String> {
    match list.iter().find(|p| p.name == name) {
        None => Ok(false),
        Some(p) if p.started_at == started_at => Ok(true),
        Some(_) => Err(format!(
            "{} was started again since you asked to stop it: nothing was stopped. Look again, and stop the new one if you mean to.",
            marv_core::printable(name)
        )),
    }
}

/// A process's log tail for the dock: what the process printed, escape sequences and controls removed (`\r` too, so a
/// progress bar's redraws don't pile up on one line).
pub fn log_tail(text: &str) -> String {
    marv_core::printable(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const DIFF: &str = "diff --git a/src/auth/redirect.rs b/src/auth/redirect.rs
index 1111111..2222222 100644
--- a/src/auth/redirect.rs
+++ b/src/auth/redirect.rs
@@ -12,4 +12,5 @@ fn handle(req: Request) {
   let session = reset(req);
-  let next = req.param(\"next\");
+  let next = req.param(\"next\");
+  let session = reset(req);
   redirect(next.or(\"/\"))
\\ No newline at end of file
diff --git a/docs/auth.md b/docs/auth.md
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/docs/auth.md
@@ -0,0 +1 @@
+# Auth
diff --git a/old.txt b/new.txt
similarity index 100%
rename from old.txt
rename to new.txt
diff --git a/logo.png b/logo.png
index 4444444..5555555 100644
Binary files a/logo.png and b/logo.png differ
";

    #[test]
    fn a_diff_is_read_as_files_their_status_counts_and_lines() {
        let files = parse_diff(DIFF);
        let summary: Vec<(&str, &str, u32, u32)> =
            files.iter().map(|f| (f.path.as_str(), f.status, f.added, f.removed)).collect();
        assert_eq!(
            summary,
            vec![
                ("src/auth/redirect.rs", "modified", 2, 1),
                ("docs/auth.md", "added", 1, 0),
                ("new.txt", "renamed", 0, 0),
                ("logo.png", "binary", 0, 0)
            ]
        );
        let kinds: Vec<&str> = files[0].lines.iter().map(|l| l.kind).collect();
        assert_eq!(kinds, ["gap", "ctx", "del", "add", "add", "ctx"]);
        assert_eq!(files[0].lines[0].text, "fn handle(req: Request) {");
        assert_eq!(files[0].lines[3].text, "  let next = req.param(\"next\");");
    }

    #[test]
    fn a_long_diff_stops_at_the_limit_and_counts_the_rest() {
        let mut text = String::from("diff --git a/big b/big\n--- a/big\n+++ b/big\n@@ -0,0 +1,5000 @@\n");
        for i in 0..5000 {
            text.push_str(&format!("+line {i}\n"));
        }
        let files = parse_diff(&text);
        assert_eq!(files[0].lines.len(), MAX_LINES);
        assert_eq!(files[0].added, 5000);
        assert_eq!(files[0].more as usize, 5001 - MAX_LINES, "the hunk header counts as a line");
    }

    #[test]
    fn a_header_lookalike_inside_a_hunk_is_a_line_not_a_status() {
        let text = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-new file mode 100644\n+deleted file mode 100644\n";
        let files = parse_diff(text);
        assert_eq!(files[0].status, "modified");
        assert_eq!((files[0].added, files[0].removed), (1, 1));
    }

    #[test]
    fn write_marks_keep_only_paths_in_the_folder_relative_and_say_whose() {
        let root = PathBuf::from("/w/acme");
        let w = |p: &str, at| marv_kernel::RecentWrite { path: PathBuf::from(p), at_ms: at };
        let sets = vec![
            ("c1".to_string(), "fix login".to_string(), vec![w("/w/acme/src/a.rs", 5), w("/w/other/b.rs", 6)]),
            ("c2".to_string(), String::new(), vec![w("/w/acme/docs/auth.md", 9), w("/w/acme", 10)]),
        ];
        let got = marks(&root, "c1", &sets);
        let short: Vec<(&str, &str, bool)> = got.iter().map(|m| (m.path.as_str(), m.by.as_str(), m.mine)).collect();
        assert_eq!(short, vec![("docs/auth.md", "a chat", false), ("src/a.rs", "fix login", true)]);
    }

    #[test]
    fn changes_outside_a_repository_say_why() {
        let dir = std::env::temp_dir().join(format!("marv-dock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let got = changes(&dir, None);
        let _ = std::fs::remove_dir_all(&dir);
        if got.problem.is_none() {
            return; // the temp folder sits inside a repository on this machine: nothing to assert
        }
        assert!(got.files.is_empty());
        assert_eq!(got.against, "HEAD");
    }

    #[test]
    fn a_repositorys_changes_are_read_whatever_its_diff_settings() {
        let dir = std::env::temp_dir().join(format!("marv-dock-repo-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let run = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        run(&["init", "-q"]);
        // A person's settings that change what `git diff` prints: the dock asks for its own prefixes.
        run(&["config", "diff.mnemonicPrefix", "true"]);
        run(&["config", "diff.noprefix", "true"]);
        std::fs::write(dir.join("src/a.txt"), "one\ntwo\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "first"]);
        std::fs::write(dir.join("src/a.txt"), "one\nthree\n").unwrap();
        std::fs::write(dir.join("src/new.txt"), "x\n").unwrap();
        let got = changes(&dir.join("src"), None);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(got.problem, None);
        let short: Vec<(&str, &str, u32, u32)> =
            got.files.iter().map(|f| (f.path.as_str(), f.status, f.added, f.removed)).collect();
        assert_eq!(short, vec![("a.txt", "modified", 1, 1), ("new.txt", "untracked", 0, 0)], "relative to the chat's folder");
    }

    #[test]
    fn a_base_that_isnt_a_hash_falls_back_to_head() {
        let dir = std::env::temp_dir();
        assert_eq!(changes(&dir, Some("--output=/tmp/x")).against, "HEAD");
    }

    fn info(name: &str, running: bool, ports: Vec<u16>) -> marv_session::processes::ProcessInfo {
        use marv_session::processes::ProcessState;
        marv_session::processes::ProcessInfo {
            name: name.into(),
            command: "bun run dev\u{1b}]52;c;eA==\u{7}".into(),
            network: false,
            state: if running { ProcessState::Running } else { ProcessState::Exited },
            exit_code: (!running).then_some(1),
            ports,
            notes: vec!["port 4789 is in use\u{1b}[2J".into()],
            started_at: 7,
        }
    }

    #[test]
    fn processes_get_their_forwarded_urls_while_running_and_printable_text() {
        let got = processes(&[info("web", true, vec![4789, 5173]), info("old", false, vec![3000])]);
        assert_eq!(got[0].urls, ["http://localhost:4789", "http://localhost:5173"]);
        assert_eq!((got[0].state, got[0].exit_code), ("running", None));
        assert_eq!(got[0].command, "bun run dev", "an OSC 52 in the command never reaches the page");
        assert_eq!(got[0].notes, ["port 4789 is in use"]);
        assert_eq!((got[1].state, got[1].exit_code), ("exited", Some(1)));
        assert!(got[1].urls.is_empty(), "an exited process has nothing to open");
    }

    #[test]
    fn a_stop_goes_only_to_the_start_the_person_saw() {
        let list = [info("web", true, vec![4789])];
        assert_eq!(same_start(&list, "web", 7), Ok(true));
        assert_eq!(same_start(&list, "gone", 7), Ok(false));
        let replaced = same_start(&list, "web", 6).unwrap_err();
        assert!(replaced.contains("started again") && replaced.contains("nothing was stopped"), "{replaced}");
    }

    #[test]
    fn a_log_tail_drops_escapes_and_carriage_returns() {
        assert_eq!(log_tail("\u{1b}[32mready\u{1b}[0m in 80ms\n10%\r50%\r100%"), "ready in 80ms\n10%50%100%");
    }
}
