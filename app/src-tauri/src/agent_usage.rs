//! What the coding agents spent, read from their own history on this computer: Claude Code's session files
//! (`~/.claude/projects/*/*.jsonl`), Codex's (`~/.codex/sessions/**/rollout-*.jsonl`) and OpenCode's database
//! (`~/.local/share/opencode/opencode.db`). It counts their work outside Marv OS too (a `claude` in another terminal),
//! and an agent Marv OS can't meter as it runs (Codex and OpenCode tell no cost to their terminal).
//!
//! - **Tokens** are the agents' own counts, each request once (Claude Code writes a reply's usage on every line of
//!   it; a resumed or forked session copies lines into a new file).
//! - **Cost** is what the same tokens cost at the provider's list prices ([`rate`]): an estimate, not a subscription's
//!   bill. OpenCode's own cost is used where it gives one; a model without a known price shows its tokens only.
//! - Read-only and private: nothing here leaves the computer or reaches a model. Each file is read again only when it
//!   changed ([`Scan`]).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;

const DAY_MS: u64 = 86_400_000;

/// One request's tokens, as an agent's history tells them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Request {
    pub agent: &'static str,
    pub model: String,
    pub at_ms: u64,
    /// Its id where the history gives one (counted once however many lines or files repeat it).
    pub key: Option<String>,
    /// Input not read from the cache.
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    /// Written to the cache for 5 minutes / for an hour (Claude's two prices; others write none or say no more).
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    /// The agent's own cost for it (OpenCode's), in micro-dollars.
    pub usd_micros: Option<u64>,
}

/// A model's list prices, in dollars per million tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rate {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
}

impl Rate {
    /// The usual cache prices for an input price: reads a tenth, 5-minute writes 1.25×, hour writes 2×.
    const fn standard(input: f64, output: f64) -> Rate {
        Rate { input, output, cache_read: input / 10.0, cache_write_5m: input * 1.25, cache_write_1h: input * 2.0 }
    }

    const fn read_at(self, cache_read: f64) -> Rate {
        Rate { cache_read, ..self }
    }

    fn usd_micros(&self, r: &Request) -> u64 {
        let usd = r.input as f64 * self.input
            + r.output as f64 * self.output
            + r.cache_read as f64 * self.cache_read
            + r.cache_write_5m as f64 * self.cache_write_5m
            + r.cache_write_1h as f64 * self.cache_write_1h;
        // Per million tokens, in micro-dollars: the two millions cancel.
        usd.round() as u64
    }
}

/// Anthropic's list prices for a Claude model id (`claude-opus-5-5`, `claude-sonnet-4-6-20260101`, `opus`), newest
/// first; `None` for a model it doesn't know (its tokens are shown, not priced).
pub fn rate(model: &str) -> Option<Rate> {
    let m = model.to_ascii_lowercase();
    let m = m.strip_prefix("anthropic/").unwrap_or(&m);
    // (id prefix, price): the first that matches, so a longer id comes before its family.
    const PRICES: &[(&str, Rate)] = &[
        ("claude-fable-5", Rate::standard(10.0, 50.0).read_at(0.25)),
        ("claude-mythos-5", Rate::standard(10.0, 50.0).read_at(0.25)),
        ("claude-opus-5-5", Rate::standard(4.0, 20.0).read_at(0.20)),
        ("claude-opus-5", Rate::standard(5.0, 25.0)),
        ("claude-opus-4-8", Rate::standard(5.0, 25.0)),
        ("claude-opus-4-7", Rate::standard(5.0, 25.0)),
        ("claude-opus-4-6", Rate::standard(5.0, 25.0)),
        ("claude-opus-4-5", Rate::standard(5.0, 25.0)),
        ("claude-opus-4", Rate::standard(15.0, 75.0)),
        ("claude-sonnet-5", Rate::standard(2.0, 10.0).read_at(0.20)),
        ("claude-sonnet-4", Rate::standard(3.0, 15.0)),
        ("claude-haiku-5-5", Rate::standard(0.10, 0.50)),
        ("claude-haiku-4-5", Rate::standard(1.0, 5.0)),
        ("claude-3-5-haiku", Rate::standard(0.80, 4.0)),
    ];
    PRICES.iter().find(|(prefix, _)| m.starts_with(prefix)).map(|(_, rate)| *rate)
}

/// Milliseconds since the epoch of an RFC 3339 time (`2026-11-13T03:23:09.935Z`), to the second; `None` if it isn't
/// one. Agents write UTC (`Z`); an offset is applied.
pub fn parse_time(text: &str) -> Option<u64> {
    let b = text.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |from: usize, to: usize| text.get(from..to)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, s) = (num(0, 4)?, num(5, 7)?, num(8, 10)?, num(11, 13)?, num(14, 16)?, num(17, 19)?);
    // Howard Hinnant's days_from_civil.
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    let mut secs = days * 86_400 + h * 3600 + mi * 60 + s;
    let zone = text[19..].trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    if let Some(sign @ ('+' | '-')) = zone.chars().next() {
        let off = num_at(zone, 1)? * 3600 + num_at(zone, 4)? * 60;
        secs -= if sign == '+' { off } else { -off };
    }
    u64::try_from(secs).ok().map(|s| s * 1000)
}

fn num_at(text: &str, at: usize) -> Option<i64> {
    text.get(at..at + 2)?.parse().ok()
}

fn count(v: &serde_json::Value, key: &str) -> u64 {
    v.get(key).and_then(|n| n.as_u64()).unwrap_or(0)
}

/// Claude Code's requests in one session file: each reply line with a `usage`, keyed by its request and message ids.
pub fn claude_requests(text: &str) -> Vec<Request> {
    let mut out = Vec::new();
    for line in text.lines() {
        // Most lines are tool output: skip them before parsing.
        if !line.contains("\"usage\"") || !line.contains("\"assistant\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v["type"] != "assistant" {
            continue;
        }
        let message = &v["message"];
        let usage = &message["usage"];
        let model = message["model"].as_str().unwrap_or_default();
        if usage.is_null() || model.is_empty() || model == "<synthetic>" {
            continue;
        }
        let written = count(usage, "cache_creation_input_tokens");
        // The split by lifetime, where it's given (else all at the 5-minute price).
        let one_hour =
            usage["cache_creation"].get("ephemeral_1h_input_tokens").and_then(|n| n.as_u64()).unwrap_or(0).min(written);
        let key = match (v["requestId"].as_str(), message["id"].as_str()) {
            (None, None) => None,
            (r, m) => Some(format!("claude:{}:{}", r.unwrap_or(""), m.unwrap_or(""))),
        };
        out.push(Request {
            agent: "claude-code",
            model: model.to_string(),
            at_ms: v["timestamp"].as_str().and_then(parse_time).unwrap_or(0),
            key,
            input: count(usage, "input_tokens"),
            output: count(usage, "output_tokens"),
            cache_read: count(usage, "cache_read_input_tokens"),
            cache_write_5m: written - one_hour,
            cache_write_1h: one_hour,
            usd_micros: None,
        });
    }
    out
}

/// Codex's requests in one rollout file: its `token_usage_record` lines (one per response), or in an older Codex that
/// writes none, each `token_count` event's `last_token_usage`. Its input counts the cached part too, which is taken
/// out; its output counts the reasoning.
pub fn codex_requests(text: &str) -> Vec<Request> {
    let mut model = String::new();
    let (mut records, mut counts) = (Vec::new(), Vec::new());
    for line in text.lines() {
        let wanted = ["\"token_usage_record\"", "\"token_count\"", "\"turn_context\"", "\"thread_settings_applied\""];
        if !wanted.iter().any(|w| line.contains(w)) {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let payload = &v["payload"];
        let at_ms = v["timestamp"].as_str().and_then(parse_time).unwrap_or(0);
        let named = payload["model"].as_str().or_else(|| payload["thread_settings"]["model"].as_str());
        if let Some(m) = named.filter(|m| !m.is_empty()) {
            model = m.to_string();
        }
        let request = |usage: &serde_json::Value, key: Option<String>| {
            let input = count(usage, "input_tokens");
            let cached = count(usage, "cached_input_tokens").min(input);
            Request {
                agent: "codex",
                model: model.clone(),
                at_ms,
                key,
                input: input - cached,
                output: count(usage, "output_tokens"),
                cache_read: cached,
                cache_write_5m: count(usage, "cache_write_input_tokens"),
                ..Default::default()
            }
        };
        match (v["type"].as_str(), payload["type"].as_str()) {
            (Some("token_usage_record"), _) => {
                let key = payload["response_id"].as_str().map(|id| format!("codex:{id}"));
                records.push(request(&payload["usage"], key));
            }
            (Some("event_msg"), Some("token_count")) if !payload["info"]["last_token_usage"].is_null() => {
                counts.push(request(&payload["info"]["last_token_usage"], None));
            }
            _ => {}
        }
    }
    if records.is_empty() { counts } else { records }
}

/// OpenCode's requests: its assistant messages since `since_ms`, with the tokens and the cost it gives each.
fn opencode_requests(db: &Path, since_ms: u64) -> Result<Vec<Request>, String> {
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = rusqlite::Connection::open_with_flags(db, flags).map_err(|e| e.to_string())?;
    conn.busy_timeout(std::time::Duration::from_millis(500)).map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare("SELECT id, time_created, data FROM session_message WHERE type = 'assistant' AND time_created >= ?1")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([since_ms as i64], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?)))
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for (id, at, data) in rows.flatten() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) else { continue };
        let tokens = &v["tokens"];
        let model = match (v["model"]["providerID"].as_str(), v["model"]["id"].as_str()) {
            (Some(p), Some(m)) => format!("{p}/{m}"),
            (None, Some(m)) => m.to_string(),
            _ => continue,
        };
        // Its own word, $0 included (its free models).
        let cost = v["cost"].as_f64().filter(|c| *c >= 0.0).map(|c| (c * 1e6).round() as u64);
        out.push(Request {
            agent: "opencode",
            model,
            at_ms: at.max(0) as u64,
            key: Some(format!("opencode:{id}")),
            input: count(tokens, "input"),
            output: count(tokens, "output") + count(tokens, "reasoning"),
            cache_read: count(&tokens["cache"], "read"),
            cache_write_5m: count(&tokens["cache"], "write"),
            cache_write_1h: 0,
            usd_micros: cost,
        });
    }
    Ok(out)
}

/// The files the agents' histories are in, changed since `since_ms`.
fn history_files(home: &Path, since_ms: u64) -> Vec<(&'static str, PathBuf)> {
    let mut found = Vec::new();
    let claude_dir = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| home.join(".claude"));
    let codex_dir = std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".codex"));
    walk(&claude_dir.join("projects"), 2, since_ms, &mut |p| found.push(("claude-code", p)));
    walk(&codex_dir.join("sessions"), 4, since_ms, &mut |p| found.push(("codex", p)));
    walk(&codex_dir.join("archived_sessions"), 4, since_ms, &mut |p| found.push(("codex", p)));
    found
}

/// Every `.jsonl` below `dir` (at most `depth` folders down) changed since `since_ms`.
fn walk(dir: &Path, depth: u32, since_ms: u64, each: &mut dyn FnMut(PathBuf)) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        let path = entry.path();
        if kind.is_dir() && depth > 1 {
            walk(&path, depth - 1, since_ms, each);
        } else if kind.is_file() && path.extension().is_some_and(|e| e == "jsonl") && modified_ms(&path) >= since_ms {
            each(path);
        }
    }
}

fn modified_ms(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as u64)
}

/// Each history file's requests as last read, kept until the file changes (its size and time).
#[derive(Default)]
struct Scan {
    files: HashMap<PathBuf, ((u64, u64), Vec<Request>)>,
}

impl Scan {
    fn requests(&mut self, agent: &'static str, path: &Path) -> &[Request] {
        let stamp = std::fs::metadata(path).map(|m| (m.len(), modified_ms(path))).unwrap_or_default();
        let stale = self.files.get(path).is_none_or(|(seen, _)| *seen != stamp);
        if stale {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            let requests = if agent == "codex" { codex_requests(&text) } else { claude_requests(&text) };
            self.files.insert(path.to_path_buf(), (stamp, requests));
        }
        &self.files[path].1
    }
}

static SCAN: Mutex<Option<Scan>> = Mutex::new(None);

/// One agent and model's usage over the days asked for.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageLine {
    pub agent: String,
    pub model: String,
    pub requests: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// What it cost at list prices (or as the agent says); `None`: no price known for the model.
    pub usd_micros: Option<u64>,
    /// Some of its requests had no known price (the cost counts only the others).
    pub partly_priced: bool,
}

/// The agents' usage since a day: by agent and model, and by day.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentUsage {
    pub since_ms: u64,
    pub lines: Vec<UsageLine>,
    /// (UTC day start in ms, priced cost that day in micro-dollars), oldest first, every day of the window.
    pub days: Vec<(u64, u64)>,
    /// What couldn't be read (OpenCode's database busy or of an older layout), for the person.
    pub problems: Vec<String>,
}

/// Adds the requests up by agent and model and by day, each request once.
pub fn total(requests: impl IntoIterator<Item = Request>, since_ms: u64, now_ms: u64) -> AgentUsage {
    let first_day = since_ms / DAY_MS;
    let mut days: Vec<(u64, u64)> = (first_day..=now_ms / DAY_MS).map(|d| (d * DAY_MS, 0)).collect();
    let mut seen = HashSet::new();
    let mut lines: Vec<UsageLine> = Vec::new();
    for r in requests {
        if r.at_ms < since_ms || r.key.as_ref().is_some_and(|k| !seen.insert(k.clone())) {
            continue;
        }
        let cost = r.usd_micros.or_else(|| rate(&r.model).map(|rate| rate.usd_micros(&r)));
        let at = match lines.iter().position(|l| l.agent == r.agent && l.model == r.model) {
            Some(i) => i,
            None => {
                lines.push(UsageLine { agent: r.agent.to_string(), model: r.model.clone(), ..Default::default() });
                lines.len() - 1
            }
        };
        let line = &mut lines[at];
        line.requests += 1;
        line.input += r.input;
        line.output += r.output;
        line.cache_read += r.cache_read;
        line.cache_write += r.cache_write_5m + r.cache_write_1h;
        match cost {
            Some(c) => {
                line.usd_micros = Some(line.usd_micros.unwrap_or(0) + c);
                if let Some(day) = days.get_mut((r.at_ms / DAY_MS).saturating_sub(first_day) as usize) {
                    day.1 += c;
                }
            }
            None => line.partly_priced = true,
        }
    }
    for line in &mut lines {
        // Only "partly" when something was priced: none priced is `usd_micros: None`.
        line.partly_priced &= line.usd_micros.is_some();
    }
    lines.sort_by(|a, b| b.usd_micros.unwrap_or(0).cmp(&a.usd_micros.unwrap_or(0)).then(b.requests.cmp(&a.requests)));
    AgentUsage { since_ms, lines, days, problems: Vec::new() }
}

/// The agents' usage over the last `days` days (today included), from their histories in `home`.
pub fn read(home: &Path, days: u64, now_ms: u64) -> AgentUsage {
    let since_ms = (now_ms / DAY_MS).saturating_sub(days.saturating_sub(1)) * DAY_MS;
    let mut requests = Vec::new();
    {
        let mut scan = SCAN.lock().unwrap_or_else(|e| e.into_inner());
        let scan = scan.get_or_insert_with(Scan::default);
        let files = history_files(home, since_ms);
        // Forget files no longer in the window (or gone).
        let keep: HashSet<&PathBuf> = files.iter().map(|(_, p)| p).collect();
        scan.files.retain(|p, _| keep.contains(p));
        for (agent, path) in &files {
            requests.extend(scan.requests(agent, path).iter().filter(|r| r.at_ms >= since_ms).cloned());
        }
    }
    let mut problems = Vec::new();
    let data = std::env::var_os("OPENCODE_DATA_DIR").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share/opencode"));
    let db = data.join("opencode.db");
    if db.is_file() {
        match opencode_requests(&db, since_ms) {
            Ok(found) => requests.extend(found),
            Err(e) => problems.push(format!("OpenCode's history couldn't be read: {e}")),
        }
    }
    let mut usage = total(requests, since_ms, now_ms);
    usage.problems = problems;
    usage
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_read_to_the_second_and_offsets_applied() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time("2026-11-13T03:23:09.935Z"), Some(1_794_540_189_000));
        assert_eq!(parse_time("2026-11-13T05:23:09+02:00"), parse_time("2026-11-13T03:23:09Z"));
        assert_eq!(parse_time("yesterday"), None);
    }

    /// Claude Code writes a reply's usage on each of its lines (text, then a tool call): one request. Its cache writes
    /// are priced by lifetime.
    #[test]
    fn a_claude_reply_on_two_lines_is_one_request_priced_by_cache_lifetime() {
        let line = r#"{"type":"assistant","requestId":"req_1","timestamp":"2026-11-13T03:23:09.935Z","message":{"id":"msg_1","model":"claude-opus-5-5","usage":{"input_tokens":2,"cache_creation_input_tokens":1000,"cache_read_input_tokens":30000,"output_tokens":246,"cache_creation":{"ephemeral_1h_input_tokens":1000,"ephemeral_5m_input_tokens":0}}}}"#;
        let text = format!("{line}\n{line}\n{{\"type\":\"user\",\"message\":{{\"content\":\"usage\"}}}}\n");
        let found = claude_requests(&text);
        assert_eq!(found.len(), 2, "both lines are read; the key makes them one");
        assert_eq!((found[0].cache_write_1h, found[0].cache_write_5m), (1000, 0));
        let usage = total(found, 0, 1_794_540_189_000);
        assert_eq!(usage.lines.len(), 1);
        let l = &usage.lines[0];
        assert_eq!((l.requests, l.input, l.output, l.cache_read, l.cache_write), (1, 2, 246, 30000, 1000));
        // 2×$4 + 246×$20 + 30000×$0.20 + 1000×$8, per million.
        assert_eq!(l.usd_micros, Some(8 + 4920 + 6000 + 8000));
        assert_eq!(usage.days.last().unwrap().1, l.usd_micros.unwrap());
    }

    /// Codex counts the cached input inside its input; Marv OS shows the two apart. A model without a known price is
    /// shown with its tokens and no cost.
    #[test]
    fn a_codex_rollouts_cached_input_is_taken_out_and_an_unknown_model_is_unpriced() {
        let text = [
            r#"{"timestamp":"2026-11-11T17:10:00.995Z","type":"event_msg","payload":{"type":"thread_settings_applied","thread_settings":{"model":"gpt-6.1-sol"}}}"#,
            r#"{"timestamp":"2026-11-11T17:10:07.203Z","type":"token_usage_record","payload":{"response_id":"resp_1","usage":{"input_tokens":15938,"cached_input_tokens":12032,"cache_write_input_tokens":0,"output_tokens":92}}}"#,
            r#"{"timestamp":"2026-11-11T17:10:07.443Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":15938,"cached_input_tokens":12032,"output_tokens":92}}}}"#,
        ]
        .join("\n");
        let found = codex_requests(&text);
        assert_eq!(found.len(), 1, "the record, not the event that repeats it");
        assert_eq!((found[0].model.as_str(), found[0].input, found[0].cache_read), ("gpt-6.1-sol", 3906, 12032));
        let older = codex_requests(&text.lines().filter(|l| !l.contains("token_usage_record")).collect::<Vec<_>>().join("\n"));
        assert_eq!(older.len(), 1, "an older Codex: its token_count events");
        let usage = total(found, 0, 1_794_540_189_000);
        assert_eq!((usage.lines[0].usd_micros, usage.lines[0].partly_priced), (None, false));
    }

    #[test]
    fn claude_prices_go_by_the_longest_matching_family() {
        assert_eq!(rate("claude-opus-5-5").unwrap().input, 4.0);
        assert_eq!(rate("claude-opus-5").unwrap().input, 5.0);
        assert_eq!(rate("claude-opus-4-1-20250805").unwrap().input, 15.0);
        assert_eq!(rate("claude-sonnet-4-6").unwrap().cache_write_1h, 6.0);
        assert_eq!(rate("anthropic/claude-haiku-4-5").unwrap().output, 5.0);
        assert!(rate("gpt-6.1-sol").is_none() && rate("big-pickle").is_none());
    }

    /// Requests before the window are left out; every day of it has its line, even a day with nothing.
    #[test]
    fn the_window_has_every_day_and_nothing_before_it() {
        let day = |d: u64| Request {
            agent: "claude-code",
            model: "claude-haiku-4-5".into(),
            at_ms: d * DAY_MS + 5,
            output: 1_000_000,
            ..Default::default()
        };
        let usage = total([day(9), day(10), day(12)], 10 * DAY_MS, 12 * DAY_MS + 1);
        assert_eq!(usage.days, vec![(10 * DAY_MS, 5_000_000), (11 * DAY_MS, 0), (12 * DAY_MS, 5_000_000)]);
        assert_eq!(usage.lines[0].requests, 2);
    }

    /// This computer's real histories, for a look by hand: `cargo test -p marv-desktop agent_usage -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn this_computers_last_week() {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
        let started = std::time::Instant::now();
        let usage = read(&marv_session::paths::home_dir(), 7, now);
        let first = started.elapsed();
        let started = std::time::Instant::now();
        read(&marv_session::paths::home_dir(), 7, now);
        println!("first read {first:?}, again {:?}", started.elapsed());
        for l in &usage.lines {
            println!(
                "{:12} {:28} {:6} req  in {:>10} out {:>9} cache {:>12}/{:>10}  {:?}",
                l.agent,
                l.model,
                l.requests,
                l.input,
                l.output,
                l.cache_read,
                l.cache_write,
                l.usd_micros.map(|u| u as f64 / 1e6)
            );
        }
        println!("{:?}\n{:?}", usage.days, usage.problems);
    }
}
