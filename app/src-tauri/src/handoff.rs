//! Switching a chat's agent (Claude Code to Codex, Marv to OpenCode…) without losing the conversation: the agent
//! taking over is given what was said, read from the outgoing agent's own history.
//!
//! - **What it's given** is a budgeted selection, not a summary a model wrote: the person's first request, then the
//!   latest messages back to [`BUDGET`] characters, each whole (one longer than a third of the budget is cut in the
//!   middle, and says so), in order, with who said it. It says how many messages were left out, and where the whole
//!   conversation is (`handoff-full.md`), for the agent to read parts of as it needs them.
//! - **Where:** beside the repository's own files (`git rev-parse --git-path`, so never in the working tree, a commit or
//!   a diff, and inside the outer sandbox's view of the folder), else in the app's data folder.
//! - **Switching back** continues that agent's own conversation (the chat keeps each agent's session), given what
//!   happened since in the same way.
//! - The outgoing agent's reasoning, tool calls and their output aren't carried: only what the person and the agent
//!   said to each other.
//!
//! Read from: Marv's saved session, Claude Code's session file, Codex's latest rollout in the folder, OpenCode's latest
//! session in the folder. Another agent's history isn't read; the one taking over is told so.

use std::path::{Path, PathBuf};

/// The selection's size, in characters (about 10k tokens).
pub const BUDGET: usize = 40_000;

/// Who said a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Who {
    Person,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Said {
    pub who: Who,
    pub text: String,
}

fn said(who: Who, text: &str) -> Option<Said> {
    let text = text.trim();
    (!text.is_empty()).then(|| Said { who, text: text.to_string() })
}

/// Text an agent's harness puts in the person's turn (Claude Code's slash commands and reminders, Codex's environment
/// and instructions): not something the person said.
fn harness_text(text: &str) -> bool {
    let t = text.trim_start();
    [
        "<command-",
        "<local-command",
        "<system-reminder",
        "<environment_context",
        "<user_instructions",
        "<permissions",
        "# AGENTS.md",
        "Caveat: The messages below",
        "[Request interrupted",
    ]
    .iter()
    .any(|p| t.starts_with(p))
}

/// Claude Code's session file: the person's messages (not tool results) and its replies' text, a reply written over
/// several lines put together.
pub fn claude_conversation(text: &str) -> Vec<Said> {
    let mut out: Vec<Said> = Vec::new();
    let mut last_reply: Option<String> = None;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v["isSidechain"] == true || v["isMeta"] == true {
            continue;
        }
        let content = &v["message"]["content"];
        let texts: Vec<&str> = match content {
            serde_json::Value::String(s) => vec![s.as_str()],
            serde_json::Value::Array(blocks) => {
                blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect()
            }
            _ => Vec::new(),
        };
        let joined = texts.join("\n");
        match v["type"].as_str() {
            Some("user") if !joined.trim().is_empty() && !harness_text(&joined) => {
                out.extend(said(Who::Person, &joined));
                last_reply = None;
            }
            Some("assistant") if !joined.trim().is_empty() => {
                let id = v["message"]["id"].as_str().map(str::to_string);
                match out.last_mut() {
                    Some(prev) if prev.who == Who::Agent && id.is_some() && id == last_reply => {
                        prev.text = format!("{}\n{}", prev.text, joined.trim());
                    }
                    _ => out.extend(said(Who::Agent, &joined)),
                }
                last_reply = id;
            }
            _ => {}
        }
    }
    out
}

/// A Codex rollout: the person's messages and its replies (`response_item` messages), without what Codex adds.
pub fn codex_conversation(text: &str) -> Vec<Said> {
    let mut out = Vec::new();
    for line in text.lines() {
        if !line.contains("\"response_item\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let p = &v["payload"];
        if p["type"] != "message" {
            continue;
        }
        let who = match p["role"].as_str() {
            Some("user") => Who::Person,
            Some("assistant") => Who::Agent,
            _ => continue,
        };
        let text: Vec<&str> = p["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| matches!(c["type"].as_str(), Some("input_text" | "output_text" | "text")))
            .filter_map(|c| c["text"].as_str())
            .collect();
        let text = text.join("\n");
        if who == Who::Person && harness_text(&text) {
            continue;
        }
        out.extend(said(who, &text));
    }
    out
}

/// Marv's saved session: what the person saw (`transcript`).
pub fn marv_conversation(text: &str) -> Vec<Said> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return Vec::new() };
    v["transcript"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let who = match m["role"].as_str() {
                Some("user") => Who::Person,
                Some("assistant") => Who::Agent,
                _ => return None,
            };
            said(who, m["text"].as_str().unwrap_or_default())
        })
        .collect()
}

/// The newest Codex rollout started in `dir` (Codex continues "the latest in this folder").
fn codex_rollout(home: &Path, dir: &Path) -> Option<PathBuf> {
    let root = std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".codex")).join("sessions");
    let mut files = Vec::new();
    collect(&root, 4, &mut files);
    files.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    let want = dir.display().to_string();
    files.into_iter().map(|(_, p)| p).find(|p| {
        let first = std::fs::read_to_string(p).ok().and_then(|t| t.lines().next().map(str::to_string)).unwrap_or_default();
        serde_json::from_str::<serde_json::Value>(&first).is_ok_and(|v| v["payload"]["cwd"].as_str() == Some(want.as_str()))
    })
}

fn collect(dir: &Path, depth: u32, out: &mut Vec<(std::time::SystemTime, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() && depth > 1 {
            collect(&path, depth - 1, out);
        } else if path.extension().is_some_and(|x| x == "jsonl")
            && let Ok(t) = e.metadata().and_then(|m| m.modified())
        {
            out.push((t, path));
        }
    }
}

/// OpenCode's latest session in `dir`, from its database.
fn opencode_conversation(home: &Path, dir: &Path) -> Result<Vec<Said>, String> {
    let data = std::env::var_os("OPENCODE_DATA_DIR").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share/opencode"));
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = rusqlite::Connection::open_with_flags(data.join("opencode.db"), flags).map_err(|e| e.to_string())?;
    let session: String = conn
        .query_row(
            "SELECT id FROM session_v2 WHERE directory = ?1 ORDER BY time_updated DESC LIMIT 1",
            [dir.display().to_string()],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare("SELECT type, data FROM session_message WHERE session_id = ?1 AND type IN ('user', 'assistant') ORDER BY seq")
        .map_err(|e| e.to_string())?;
    let rows = stmt.query_map([session], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for (kind, data) in rows.flatten() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) else { continue };
        let text = match kind.as_str() {
            "user" => v["text"].as_str().unwrap_or_default().to_string(),
            _ => v["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|c| c["type"] == "text")
                .filter_map(|c| c["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        };
        out.extend(said(if kind == "user" { Who::Person } else { Who::Agent }, &text));
    }
    Ok(out)
}

/// What chat's agent `agent` said with the person in `dir` (its session `session` where it keeps one by id); `None`:
/// Marv OS can't read this agent's history.
pub fn conversation(agent: &str, session: Option<&str>, dir: &Path, home: &Path, marv_dir: &Path) -> Option<Vec<Said>> {
    let read = |p: &Path| std::fs::read_to_string(p).ok();
    match agent {
        marv_web::terminal_agents::MARV => {
            let id = session?;
            let file = marv_session::sessions::SessionStore::new(marv_dir.join("sessions")).file(dir, id);
            Some(marv_conversation(&read(&file)?))
        }
        "claude-code" => {
            let id = session?;
            let config = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| home.join(".claude"));
            let file = std::fs::read_dir(config.join("projects"))
                .ok()?
                .flatten()
                .map(|e| e.path().join(format!("{id}.jsonl")))
                .find(|p| p.is_file())?;
            Some(claude_conversation(&read(&file)?))
        }
        "codex" => Some(codex_conversation(&read(&codex_rollout(home, dir)?)?)),
        "opencode" => opencode_conversation(home, dir).ok(),
        _ => None,
    }
}

/// `text` cut to `max` characters in the middle, saying so.
fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let half = max / 2;
    let head: String = text.chars().take(half).collect();
    let tail: String = text.chars().rev().take(half).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{head}\n\n[… the middle of this message is left out here; it's whole in the full conversation …]\n\n{tail}")
}

/// The budgeted selection: the first request, then the latest messages back to `budget` characters; how many
/// messages between were left out.
pub fn select(messages: &[Said], budget: usize) -> (Vec<Said>, usize) {
    let longest = budget / 3;
    let fit = |m: &Said| Said { who: m.who, text: cut(&m.text, longest) };
    let first = messages.iter().position(|m| m.who == Who::Person);
    let mut used = first.map_or(0, |i| fit(&messages[i]).text.len());
    let mut latest = Vec::new();
    for (i, m) in messages.iter().enumerate().rev() {
        if Some(i) == first {
            break;
        }
        let m = fit(m);
        if used + m.text.len() > budget && !latest.is_empty() {
            break;
        }
        used += m.text.len();
        latest.push((i, m));
    }
    latest.reverse();
    let start = latest.first().map_or(messages.len(), |(i, _)| *i);
    let mut out: Vec<Said> = first.filter(|i| *i < start).map(|i| fit(&messages[i])).into_iter().collect();
    let left_out = start.saturating_sub(first.map_or(0, |i| i + 1));
    out.extend(latest.into_iter().map(|(_, m)| m));
    (out, left_out)
}

fn render(messages: &[Said], from: &str) -> String {
    let mut out = String::new();
    for m in messages {
        let who = if m.who == Who::Person { "The person" } else { from };
        out.push_str(&format!("### {who}\n\n{}\n\n", m.text));
    }
    out
}

/// The handoff's text for the agent taking over from `from`, and the whole conversation's.
pub fn handoff_text(from: &str, messages: Option<&[Said]>, full_path: &Path) -> (String, String) {
    let Some(messages) = messages.filter(|m| !m.is_empty()) else {
        let text = format!(
            "# Taking over this chat from {from}\n\nThe person was working in this folder with {from}, whose conversation Marv OS can't read. Ask the person where things stand before you change anything.\n"
        );
        return (text, String::new());
    };
    let (picked, left_out) = select(messages, BUDGET);
    let mut text = format!(
        "# Taking over this chat from {from}\n\nThe person was working in this folder with {from}; you're continuing that work. Below is what they said to each other, in order: the person's first request, then the latest messages."
    );
    if left_out > 0 {
        text.push_str(&format!(
            " {left_out} messages in between are left out here. The whole conversation is in `{}`: read the parts you need (it can be long).",
            full_path.display()
        ));
    }
    text.push_str(&format!(
        " {from}'s tool calls and their output aren't included: look at the files themselves for what was done.\n\n## The conversation\n\n"
    ));
    text.push_str(&render(&picked, from));
    let full = format!("# The whole conversation with {from}\n\n{}", render(messages, from));
    (text, full)
}

/// Where a chat's handoff files go: beside the repository's files when `dir` is in one (never in the working tree),
/// else `<data>/handoffs`.
pub fn place(dir: &Path, data: &Path, chat: &str) -> PathBuf {
    let in_git = marv_core::process::command("git")
        .current_dir(dir)
        .args(["rev-parse", "--path-format=absolute", "--git-path", "marv-handoff"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()));
    in_git.unwrap_or_else(|| data.join("handoffs")).join(chat)
}

/// Writes chat `chat`'s handoff (in [`place`]); the selection's path.
pub fn write(dir: &Path, data: &Path, chat: &str, from: &str, messages: Option<&[Said]>) -> Result<PathBuf, String> {
    let folder = place(dir, data, chat);
    std::fs::create_dir_all(&folder).map_err(|e| format!("The handoff couldn't be written: {e}"))?;
    let (path, full_path) = (folder.join("handoff.md"), folder.join("handoff-full.md"));
    let (text, full) = handoff_text(from, messages, &full_path);
    std::fs::write(&path, text).map_err(|e| format!("The handoff couldn't be written: {e}"))?;
    if !full.is_empty() {
        std::fs::write(&full_path, full).map_err(|e| format!("The handoff couldn't be written: {e}"))?;
    }
    Ok(path)
}

/// What's typed into the new agent's terminal.
pub fn first_message(from: &str, path: &Path) -> String {
    format!(
        "You're taking over this chat from {from}. Read {} first (the conversation so far), then tell me in a few sentences where things stand and what you'd do next.",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(who: Who, text: &str) -> Said {
        Said { who, text: text.into() }
    }

    /// The first request and the latest messages, whole and in order, within the budget; the count of those left out.
    #[test]
    fn the_selection_keeps_the_first_request_and_the_latest_messages() {
        let mut all = vec![m(Who::Person, "Build a pomodoro timer")];
        for i in 0..20 {
            all.push(m(Who::Agent, &format!("reply {i} {}", "x".repeat(90))));
            all.push(m(Who::Person, &format!("ask {i}")));
        }
        let (picked, left_out) = select(&all, 1000);
        assert_eq!(picked[0].text, "Build a pomodoro timer");
        assert_eq!(picked.last().unwrap().text, "ask 19");
        assert_eq!(picked.len() + left_out, all.len());
        assert!(left_out > 0);
        let used: usize = picked.iter().map(|s| s.text.len()).sum();
        assert!(used <= 1000, "{used}");
        // A short conversation goes whole.
        let (whole, none) = select(&all[..3], BUDGET);
        assert_eq!((whole.len(), none), (3, 0));
        // One message past a third of the budget is cut in the middle, and says so.
        let (cut_one, _) = select(&[m(Who::Person, &"y".repeat(5000))], 3000);
        assert!(cut_one[0].text.contains("left out here") && cut_one[0].text.len() < 1300);
    }

    #[test]
    fn claude_code_conversations_are_read_without_tool_results_or_its_own_text() {
        let text = [
            r#"{"type":"user","message":{"role":"user","content":"Fix the login bug"}}"#,
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"Looking."}]}}"#,
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash"}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"ok"}]}}"#,
            r#"{"type":"assistant","message":{"id":"m2","content":[{"type":"text","text":"Fixed it."}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":"<command-name>/model</command-name>"}}"#,
            r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"meta"}}"#,
        ]
        .join("\n");
        let said = claude_conversation(&text);
        assert_eq!(said, vec![m(Who::Person, "Fix the login bug"), m(Who::Agent, "Looking."), m(Who::Agent, "Fixed it.")]);
    }

    #[test]
    fn codex_and_marv_conversations_are_read() {
        let codex = [
            r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"You are Codex"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>cwd</environment_context>"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Fix biggest()"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done."}]}}"#,
        ]
        .join("\n");
        assert_eq!(codex_conversation(&codex), vec![m(Who::Person, "Fix biggest()"), m(Who::Agent, "Done.")]);
        let marv =
            r#"{"transcript":[{"role":"user","text":"hi"},{"role":"notice","text":"x"},{"role":"assistant","text":"hello"}]}"#;
        assert_eq!(marv_conversation(marv), vec![m(Who::Person, "hi"), m(Who::Agent, "hello")]);
    }

    /// In a repository the handoff is beside its files, never in the working tree (nothing for `git status`).
    #[test]
    fn the_handoff_is_kept_out_of_the_working_tree() {
        let project = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| std::process::Command::new("git").current_dir(project.path()).args(args).output().unwrap();
        git(&["init", "-q"]);
        let data = tempfile::tempdir().unwrap();
        let said = vec![m(Who::Person, "Build it"), m(Who::Agent, "Built.")];
        let path = write(project.path(), data.path(), "c1", "Claude Code", Some(&said)).unwrap();
        assert!(path.starts_with(project.path().join(".git")), "{path:?}");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("from Claude Code") && text.contains("### The person\n\nBuild it") && text.contains("Built."));
        assert!(String::from_utf8_lossy(&git(&["status", "--porcelain"]).stdout).trim().is_empty());
        let plain = tempfile::tempdir().unwrap();
        let unread = write(plain.path(), data.path(), "c2", "Pi", None).unwrap();
        assert!(unread.starts_with(data.path().join("handoffs")));
        assert!(std::fs::read_to_string(unread).unwrap().contains("can't read"));
    }

    /// A real Claude Code session on this computer, for a look by hand:
    /// `HANDOFF_SESSION=<id> cargo test -p marv-desktop handoff -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn a_real_claude_session() {
        let Ok(id) = std::env::var("HANDOFF_SESSION") else { return };
        let home = marv_session::paths::home_dir();
        let said = conversation("claude-code", Some(&id), &home, &home, &home.join(".marv")).unwrap();
        let (picked, left_out) = select(&said, BUDGET);
        let size: usize = picked.iter().map(|s| s.text.len()).sum();
        println!("{} messages, {} picked ({size} chars), {left_out} left out", said.len(), picked.len());
        println!("first: {}", picked[0].text.chars().take(80).collect::<String>());
        println!("last: {}", picked.last().unwrap().text.chars().take(120).collect::<String>());
    }
}
