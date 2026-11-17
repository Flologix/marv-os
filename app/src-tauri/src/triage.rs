//! "Report a problem": the person's own coding agent looks into what went wrong with Marv OS, in a folder made for it
//! (`<app data>/triage/<time>/`), and helps write the report. The agent's chat runs there and sees only what's copied
//! in:
//!
//! - `PLAYBOOK.md`: what to do (ask what happened, read the evidence, separate a Marv OS bug from the agent's own or
//!   the computer's, draft an issue), and what not to (no fixes to Marv OS's files, nothing posted without the person).
//! - `marv-os.log` (and `marv-os.log.1`): the app's own log, which never holds a conversation and has the person's keys
//!   taken out ([`crate::applog`]).
//! - `reports/`: the latest hang reports ([`crate::hang_report`]).
//! - `about.md`: the version, the system, which agents are installed and whether the sandbox runs, and the chats as a
//!   list (title, agent, permission mode, whether its terminal runs): never their messages, keys or settings files.
//!
//! The folder is the person's to keep or delete; nothing here leaves the computer.

use std::path::{Path, PathBuf};

/// How many of the latest hang reports go in.
const REPORTS: usize = 5;

pub const TITLE: &str = "Report a problem";

/// What the agent is told to do: the same text for every agent, written into the folder as `PLAYBOOK.md`.
pub const PLAYBOOK: &str = "# Report a problem with Marv OS

You are helping the person find out what went wrong with Marv OS (a desktop app for coding agents) and, if it's a
bug, write a good report. This folder was made for that; it holds copies, so nothing here is Marv OS itself.

## What's here

- `about.md`: the Marv OS version, the system, the coding agents installed, whether Marv's sandbox runs, and the
  person's chats as a list (no messages).
- `marv-os.log` (and `marv-os.log.1`, the one before): Marv OS's own log of what went wrong or went unexpectedly.
  It holds no conversations, and keys are taken out.
- `reports/`: the latest reports from Marv OS's watchdog, when the window stopped responding.

## What to do

1. Ask the person what happened: what they did, what they expected, what they saw instead, and when (roughly).
2. Read `about.md` and the log around that time. Look for errors and warnings, and for what led up to them.
3. Say what you think happened, and how sure you are. Tell apart:
   - a Marv OS problem (the app, its terminals, its sandbox, its teams and mail),
   - a problem of the agent in the chat (Claude Code, Codex, OpenCode…) or of its sign-in,
   - something on the computer (a missing program, a full disk, the network).
4. If there's something the person can do now (a setting, a restart, installing a program), say it step by step.
5. If it's a Marv OS bug, draft the report in `REPORT.md`: a short title, what happened, steps to make it happen
   again, what was expected, the version and system from `about.md`, and the log lines that matter (only those).
   Read it through with the person before they send it.

## What not to do

- Don't change Marv OS's own files or settings, and don't restart it: say what to do and let the person do it.
- Don't post, send or upload anything: the person sends the report themselves.
- Don't put the person's messages, file contents, keys or paths outside this folder in the report unless they say so.
";

/// What's known about the computer and the app for `about.md`.
pub struct About {
    pub version: String,
    pub os: String,
    /// (agent, installed at, version), every agent Marv OS knows.
    pub agents: Vec<(String, Option<String>, Option<String>)>,
    pub sandbox: bool,
    /// One line per chat: title, agent, permission mode, folder kind, running or not.
    pub chats: Vec<String>,
}

fn about_text(about: &About) -> String {
    let mut out = format!(
        "# About this computer\n\n- Marv OS {}\n- System: {}\n- Marv's sandbox (bubblewrap): {}\n\n## Coding agents\n\n",
        about.version,
        about.os,
        if about.sandbox { "runs" } else { "not available" }
    );
    for (name, path, version) in &about.agents {
        match path {
            Some(path) => out.push_str(&format!("- {name}: {} ({path})\n", version.as_deref().unwrap_or("version unknown"))),
            None => out.push_str(&format!("- {name}: not installed\n")),
        }
    }
    out.push_str(&format!("\n## Chats ({})\n\n", about.chats.len()));
    for chat in &about.chats {
        out.push_str(&format!("- {chat}\n"));
    }
    out
}

/// A name for the folder from the time: `2026-11-13-0342`, unique under `triage`.
fn folder_name(now_ms: u64) -> String {
    marv_session::time::iso(now_ms)[..16].replace(['T', ':'], "-")
}

/// Makes the folder for a problem report under `data`: the playbook, the log, the latest hang reports and
/// `about.md`. Its path.
pub fn prepare(data: &Path, logs: Option<&Path>, about: &About, now_ms: u64) -> Result<PathBuf, String> {
    let parent = data.join("triage");
    let mut dir = parent.join(folder_name(now_ms));
    let mut n = 1;
    while dir.exists() {
        n += 1;
        dir = parent.join(format!("{}-{n}", folder_name(now_ms)));
    }
    let made = |e: std::io::Error| format!("The folder for the report couldn't be made: {e}");
    std::fs::create_dir_all(&dir).map_err(made)?;
    marv_fs::mode::set_mode(&dir, 0o700).map_err(made)?;
    std::fs::write(dir.join("PLAYBOOK.md"), PLAYBOOK).map_err(made)?;
    std::fs::write(dir.join("about.md"), about_text(about)).map_err(made)?;
    if let Some(logs) = logs {
        for name in [crate::applog::FILE.to_string(), format!("{}.1", crate::applog::FILE)] {
            if logs.join(&name).is_file() {
                std::fs::copy(logs.join(&name), dir.join(&name)).map_err(made)?;
            }
        }
    }
    let mut reports: Vec<PathBuf> = std::fs::read_dir(crate::hang_report::dir(data))
        .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect())
        .unwrap_or_default();
    // Named by their time: the latest last.
    reports.sort();
    if !reports.is_empty() {
        std::fs::create_dir_all(dir.join("reports")).map_err(made)?;
        for report in reports.iter().rev().take(REPORTS) {
            if let Some(name) = report.file_name() {
                std::fs::copy(report, dir.join("reports").join(name)).map_err(made)?;
            }
        }
    }
    Ok(dir)
}

/// What's typed into the agent's terminal to start it off.
pub const FIRST_MESSAGE: &str = "Read PLAYBOOK.md in this folder and follow it: start by asking me what went wrong with Marv OS.";

#[cfg(test)]
mod tests {
    use super::*;

    /// The folder holds the playbook, the log and the latest reports, and `about.md` lists the chats without their
    /// messages; a second report the same minute gets a folder of its own.
    #[test]
    fn a_report_folder_has_the_evidence_and_nothing_of_the_conversations() {
        let data = tempfile::tempdir().unwrap();
        let logs = tempfile::tempdir().unwrap();
        std::fs::write(logs.path().join(crate::applog::FILE), "12:00 ERROR terminal: claude wouldn't start\n").unwrap();
        let reports = crate::hang_report::dir(data.path());
        std::fs::create_dir_all(&reports).unwrap();
        for t in 1..=7 {
            std::fs::write(reports.join(format!("hang-{t}.json")), "{}").unwrap();
        }
        let about = About {
            version: "0.16.3".into(),
            os: "linux".into(),
            agents: vec![("Claude Code".into(), Some("/bin/claude".into()), Some("2.1.0".into())), ("Pi".into(), None, None)],
            sandbox: true,
            chats: vec!["\"Fix the build\" · Claude Code · Supervised · a project folder · running".into()],
        };
        let dir = prepare(data.path(), Some(logs.path()), &about, 1_794_540_189_000).unwrap();
        assert!(dir.starts_with(data.path().join("triage")));
        assert!(std::fs::read_to_string(dir.join("PLAYBOOK.md")).unwrap().contains("Don't post"));
        assert!(std::fs::read_to_string(dir.join(crate::applog::FILE)).unwrap().contains("wouldn't start"));
        let copied: Vec<String> =
            std::fs::read_dir(dir.join("reports")).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        assert_eq!(copied.len(), REPORTS);
        assert!(copied.contains(&"hang-7.json".to_string()) && !copied.contains(&"hang-1.json".to_string()));
        let text = std::fs::read_to_string(dir.join("about.md")).unwrap();
        assert!(text.contains("Marv OS 0.16.3") && text.contains("Pi: not installed") && text.contains("Fix the build"));
        let again = prepare(data.path(), None, &about, 1_794_540_189_000).unwrap();
        assert_ne!(again, dir);
        #[cfg(unix)] // Windows has no mode bits
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700, "private");
        }
    }
}
