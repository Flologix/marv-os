//! Marv OS's own log: what went wrong or went unexpectedly (a panic, a failed command, an agent's terminal that
//! wouldn't start, an error on the page), for the person to send with a problem report (Settings › About › Logs).
//!
//! - One file, `marv-os.log` in the platform's log folder (Linux `~/.local/share/ai.flologix.marv/logs`, Windows
//!   `%LOCALAPPDATA%\ai.flologix.marv\logs`, macOS `~/Library/Logs/ai.flologix.marv`), kept under [`MAX_BYTES`]: past
//!   it, the file becomes `marv-os.log.1` (the one before is dropped) and a new one starts.
//! - What goes in is Marv's own words about what happened, never a conversation: no messages, no files' contents.
//!   Each line is `printable()` and on one line, and the person's keys are taken out ([`Log::scrub`]) in case an error
//!   quoted one.
//! - Every line also goes to stderr, as the app's `eprintln!`s did.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Past this the log starts over (the previous one kept as `.1`): two files at most, for a report.
pub const MAX_BYTES: u64 = 5 * 1024 * 1024;
/// The log's file name.
pub const FILE: &str = "marv-os.log";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn word(self) -> &'static str {
        match self {
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }

    /// The page's word for it (`"error"`, `"warn"`; anything else is info).
    pub fn from_word(word: &str) -> Self {
        match word {
            "error" => Level::Error,
            "warn" => Level::Warn,
            _ => Level::Info,
        }
    }
}

struct Log {
    path: PathBuf,
    file: Mutex<Option<std::fs::File>>,
    secrets: marv_core::Secrets,
}

static LOG: OnceLock<Log> = OnceLock::new();

impl Log {
    /// `text` without the person's keys: the ones Marv was given (`secrets`), and anything shaped like a provider's
    /// key or a bearer token, which an error may quote.
    fn scrub(&self, text: &str) -> String {
        let mut text = self.secrets.redact(text).unwrap_or_else(|| text.to_string());
        for prefix in ["sk-or-", "sk-ant-", "sk-proj-", "sk-", "ghp_", "gho_", "github_pat_", "xoxb-", "xoxp-", "Bearer "] {
            let mut from = 0;
            while let Some(at) = text[from..].find(prefix).map(|i| i + from) {
                let start = at + prefix.len();
                let end = text[start..]
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'))
                    .map_or(text.len(), |i| i + start);
                from = if end - start >= 12 {
                    text.replace_range(start..end, "[redacted]");
                    start + "[redacted]".len()
                } else {
                    start
                };
            }
        }
        text
    }

    fn write(&self, line: &str) {
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        // Its size on disk, so a log left large by an earlier run rolls over too.
        if std::fs::metadata(&self.path).is_ok_and(|m| m.len() > MAX_BYTES) {
            *file = None;
            let _ = std::fs::rename(&self.path, self.path.with_extension("log.1"));
        }
        if file.is_none() {
            *file = std::fs::OpenOptions::new().create(true).append(true).open(&self.path).ok();
        }
        if let Some(f) = file.as_mut() {
            let _ = writeln!(f, "{line}");
        }
    }
}

/// Starts the log in `dir`, with the person's keys to keep out of it, and says what's running (version, system),
/// which every report needs. Errors are said on stderr only: the app runs without a log.
pub fn init(dir: &Path, secrets: marv_core::Secrets, version: &str) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("marv-desktop: no log ({}: {e})", dir.display());
        return;
    }
    let _ = LOG.set(Log { path: dir.join(FILE), file: Mutex::new(None), secrets });
    let desktop = ["XDG_CURRENT_DESKTOP", "XDG_SESSION_TYPE", "GDK_BACKEND"]
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| format!("{k}={v}")))
        .collect::<Vec<_>>()
        .join(" ");
    info(
        "start",
        &format!(
            "Marv OS {version} on {} {} (pid {}){}",
            std::env::consts::OS,
            std::env::consts::ARCH,
            std::process::id(),
            if desktop.is_empty() { String::new() } else { format!(", {desktop}") }
        ),
    );
    // A panic is the most useful line a report can have: where, and what it said.
    let before = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        let what = panic
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| panic.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        let at = panic.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        error("panic", &format!("{what} (at {at})"));
        before(panic);
    }));
}

/// One line: when (UTC), how bad, which part of the app, and what happened.
pub fn write(level: Level, area: &str, text: &str) {
    let one_line = marv_core::printable(text).replace(['\n', '\r'], " ⏎ ");
    let text = LOG.get().map_or(one_line.clone(), |log| log.scrub(&one_line));
    eprintln!("marv-desktop: {area}: {text}");
    if let Some(log) = LOG.get() {
        let area = marv_core::printable(area);
        log.write(&format!("{} {:<5} {area}: {text}", marv_session::time::iso(marv_session::time::now_ms()), level.word()));
    }
}

pub fn info(area: &str, text: &str) {
    write(Level::Info, area, text);
}

pub fn warn(area: &str, text: &str) {
    write(Level::Warn, area, text);
}

pub fn error(area: &str, text: &str) {
    write(Level::Error, area, text);
}

/// The log's folder, for Settings' "Open the logs folder".
pub fn dir() -> Option<PathBuf> {
    LOG.get().and_then(|l| l.path.parent().map(Path::to_path_buf))
}

/// The log's last `lines` lines (the previous file's end first when this one is short), for "Copy the log".
pub fn recent(lines: usize) -> String {
    let Some(log) = LOG.get() else { return String::new() };
    let read = |p: &Path| std::fs::read_to_string(p).unwrap_or_default();
    let now = read(&log.path);
    let mut all: Vec<&str> = now.lines().collect();
    let before;
    if all.len() < lines {
        before = read(&log.path.with_extension("log.1"));
        let mut older: Vec<&str> = before.lines().collect();
        older.extend(all);
        all = older;
    }
    all[all.len().saturating_sub(lines)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The log never keeps a key: the ones Marv was given, and ones an error quotes in a provider's or GitHub's shape.
    #[test]
    fn the_log_keeps_no_keys() {
        let log = Log {
            path: PathBuf::from("/nowhere"),
            file: Mutex::new(None),
            secrets: marv_core::Secrets::new(["my-own-zai-key-0123456789".to_string()]),
        };
        let line = log.scrub(
            "401 from OpenRouter with sk-or-v1-abcdef0123456789abcdef, my-own-zai-key-0123456789, Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.x, ghp_ABCDEFGHIJKLMNOPQRSTUVWX",
        );
        for key in ["abcdef0123456789abcdef", "my-own-zai-key", "eyJhbGciOiJIUzI1NiJ9", "ABCDEFGHIJKLMNOP"] {
            assert!(!line.contains(key), "{key} in {line}");
        }
        assert!(line.starts_with("401 from OpenRouter with sk-or-[redacted]"), "{line}");
        assert_eq!(log.scrub("sk-short and a task-list"), "sk-short and a task-list", "words aren't keys");
    }

    /// Past its size the log starts over, the previous one kept as `.1`, and the copy reads across both.
    #[test]
    fn the_log_rolls_over_and_reads_across_files() {
        let dir = tempfile::tempdir().unwrap();
        let log = Log { path: dir.path().join(FILE), file: Mutex::new(None), secrets: Default::default() };
        std::fs::write(&log.path, "x".repeat(MAX_BYTES as usize + 1) + "\nold last\n").unwrap();
        log.write("first after");
        assert!(std::fs::read_to_string(dir.path().join("marv-os.log.1")).unwrap().ends_with("old last\n"));
        assert_eq!(std::fs::read_to_string(&log.path).unwrap(), "first after\n");
    }
}
