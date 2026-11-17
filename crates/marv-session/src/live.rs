//! What a session says about itself to the Activity table (runtime-kernel §20.2 and §20.9, decisions O2 and O10).
//! New in marv-rs.
//!
//! [`LiveMeter`] watches the turn's events (`run_turn`'s emit) and the approvals (a [`MeteredApprover`] around the
//! client's): the loop timers (model / tools / waiting for you), the requests (tokens/s, cache hit %, $/h) and the
//! retries (throttles), in a [`marv_kernel::metrics::Meter`]. [`crate::session::MarvSession::live`] adds the core, the
//! context used ÷ budget, the spend, the running subagents' kinds and the real processes (local MCP servers and the
//! process tool's, measured through [`marv_kernel::procstats`], which reads only PIDs Marv started).
//!
//! A session with a mesh puts this on its door's `marv/info` (`live`), so a terminal Marv and a desktop chat appear
//! in one table (`marv kernel ps`, the desktop's Activity). **For the person only:** no tool reads it and `mesh_list`
//! is unchanged (guard `activity_never_reaches_a_model`). It holds no message text: names, kinds and numbers.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use marv_agent::LoopEvent;
use marv_kernel::metrics::{ChildProcess, Meter, Metrics, Phase, ProcessKind, RateLimit, SubagentRow};
use marv_kernel::procstats::Sampler;
use marv_tools::{ApprovalRequest, Approver, Decision};

use crate::session::SessionEvent;

/// A subagent's kind as Activity shows it: an agent file's name (a short identifier), else "subagent". The model
/// picks it, so anything else (words, a sentence) is never shown.
pub fn subagent_kind(kind: &str) -> String {
    let ok = !kind.is_empty() && kind.len() <= 32 && kind.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok { kind.to_string() } else { "subagent".into() }
}

#[derive(Default)]
struct Inner {
    meter: Meter,
    /// Running subagents by their `agent` call's id: kind, and how many of its tool calls run.
    subagents: BTreeMap<String, (String, u32)>,
    /// A harness chat's (Claude Code's, an ACP agent's): what its `ready` named it, and its last context used ÷ window.
    harness: Option<String>,
    context: Option<marv_kernel::metrics::Context>,
}

/// How long a sample of the real processes is reused: a door asked twice a second reads them once.
pub const SAMPLE_EVERY_MS: u64 = 1_000;

/// The real processes' last sample (when, and the rows), its sampler, and whether a refresh is running.
#[derive(Default)]
struct Processes {
    at_ms: u64,
    rows: Vec<ChildProcess>,
    sampler: Sampler,
}

/// One session's live numbers. Cheap to share.
#[derive(Default)]
pub struct LiveMeter {
    inner: Mutex<Inner>,
    /// Review P2-3: sampled off the meter's lock, at most every [`SAMPLE_EVERY_MS`], on a blocking thread (`ps`).
    processes: Arc<Mutex<Processes>>,
    refreshing: Arc<std::sync::atomic::AtomicBool>,
    /// The door whose state this session says (`SessionInit::door_state`: a terminal Marv's), if any.
    door: Mutex<Option<crate::mesh::MeshHandle>>,
    /// A chat's terminal agent's own process (its program's name, PID): shown under its row with its tree's CPU and
    /// memory (Claude Code, Codex… report no numbers of their own but their cost).
    agent: Mutex<Option<(String, u32)>>,
}

/// The scheduler word a phase is, for a door that says its own state.
pub fn door_word(phase: Phase) -> marv_kernel::AgentState {
    match phase {
        Phase::Idle => marv_kernel::AgentState::Idle,
        Phase::You => marv_kernel::AgentState::Waiting,
        Phase::Model | Phase::Tools => marv_kernel::AgentState::Busy,
    }
}

fn micros(cost: Option<f64>) -> u64 {
    cost.filter(|c| c.is_finite() && *c > 0.0).map_or(0, |c| (c * 1e6).round() as u64)
}

impl LiveMeter {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Says this session's state on `door` from now on (idle now, if nothing runs).
    pub fn set_door(&self, door: crate::mesh::MeshHandle) {
        *self.door.lock().unwrap_or_else(|e| e.into_inner()) = Some(door);
        self.report();
    }

    fn report(&self) {
        let phase = self.phase();
        if let Some(door) = self.door.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            door.set_state(Some(door_word(phase)));
        }
    }

    /// One of the turn's events, at `now` (wall ms).
    pub fn observe(&self, event: &SessionEvent, now: u64) {
        self.note(event, now);
        if matches!(
            event,
            SessionEvent::TurnStart { .. }
                | SessionEvent::TurnEnd { .. }
                | SessionEvent::Loop(LoopEvent::ToolStart { .. } | LoopEvent::ToolEnd { .. })
        ) {
            self.report();
        }
    }

    fn note(&self, event: &SessionEvent, now: u64) {
        let mut inner = self.lock();
        let Inner { meter, subagents, .. } = &mut *inner;
        match event {
            SessionEvent::TurnStart { .. } => meter.turn_start(now),
            SessionEvent::TurnEnd { .. } => {
                meter.turn_end(now);
                subagents.clear();
            }
            SessionEvent::Loop(event) => match event {
                LoopEvent::ToolStart { call, .. } => {
                    meter.tool_start(now);
                    if call.name == "agent" {
                        let kind = subagent_kind(&marv_agent::agent::agent_args(&call.arguments).kind);
                        subagents.insert(call.id.clone(), (kind, 0));
                    }
                }
                LoopEvent::ToolEnd { call, .. } => {
                    meter.tool_end(now);
                    subagents.remove(&call.id);
                }
                LoopEvent::Usage(usage, end) => {
                    // Its cost is charged where the totals count it (`MarvSession::count`, with their estimate).
                    meter.request(now, usage.prompt_tokens, usage.cached_tokens.unwrap_or(0), usage.completion_tokens, true);
                    if let Some(limit) = end.rate_limit {
                        meter.rate_limit(rate_limit(limit));
                    }
                }
                LoopEvent::Retry { cause, .. } => meter.retry(now, *cause == marv_core::RetryCause::Throttled),
                _ => {}
            },
            SessionEvent::Subagent { call_id, event } => {
                if let Some((_, tools)) = subagents.get_mut(call_id) {
                    match event {
                        LoopEvent::ToolStart { .. } => *tools += 1,
                        LoopEvent::ToolEnd { .. } => *tools = tools.saturating_sub(1),
                        _ => {}
                    }
                }
                match event {
                    LoopEvent::Retry { cause, .. } => meter.retry(now, *cause == marv_core::RetryCause::Throttled),
                    // Review P3-4: a subagent's requests count for the cache (their cost is charged by `count`).
                    LoopEvent::Usage(usage, _) => {
                        meter.request(now, usage.prompt_tokens, usage.cached_tokens.unwrap_or(0), usage.completion_tokens, false)
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// What a request cost (USD, reported or estimated as the totals do: review P3-4), for $/h and today's spend.
    pub fn charge(&self, now: u64, cost: Option<f64>) {
        self.lock().meter.charge(now, micros(cost));
    }

    /// Today's spend by this load (micro-dollars).
    pub fn spent_today(&self, now: u64) -> u64 {
        self.lock().meter.spent_today(now)
    }

    pub fn wait_start(&self, now: u64) {
        self.lock().meter.wait_start(now);
        self.report();
    }

    pub fn wait_end(&self, now: u64) {
        self.lock().meter.wait_end(now);
        self.report();
    }

    /// The numbers at `now`.
    pub fn metrics(&self, now: u64) -> Metrics {
        self.lock().meter.snapshot(now)
    }

    pub fn phase(&self) -> Phase {
        self.lock().meter.phase()
    }

    /// The running subagents: kind and what it's doing.
    pub fn subagents(&self) -> Vec<SubagentRow> {
        self.lock()
            .subagents
            .values()
            .map(|(kind, tools)| SubagentRow { kind: kind.clone(), phase: if *tools > 0 { Phase::Tools } else { Phase::Model } })
            .collect()
    }

    /// `procs` (kind, name, PID: each one Marv started) as last measured: the cached rows, refreshed in the background
    /// (a blocking thread: `ps` on macOS) when older than [`SAMPLE_EVERY_MS`]. A PID that isn't Marv's own is left out.
    /// The first answer is empty, or this call's own sample where there's no runtime.
    pub fn processes(&self, procs: Vec<(ProcessKind, String, u32)>, now: u64) -> Vec<ChildProcess> {
        use std::sync::atomic::Ordering;
        let (at, rows) = {
            let p = self.processes.lock().unwrap_or_else(|e| e.into_inner());
            (p.at_ms, p.rows.clone())
        };
        let stale = now.saturating_sub(at) >= SAMPLE_EVERY_MS && (!procs.is_empty() || !rows.is_empty());
        if stale && !self.refreshing.swap(true, Ordering::SeqCst) {
            let (cache, refreshing) = (self.processes.clone(), self.refreshing.clone());
            let job = move || {
                let mut p = cache.lock().unwrap_or_else(|e| e.into_inner());
                p.rows = sample(&mut p.sampler, &procs, now);
                p.at_ms = now;
                refreshing.store(false, Ordering::SeqCst);
            };
            match tokio::runtime::Handle::try_current() {
                Ok(runtime) => drop(runtime.spawn_blocking(job)),
                Err(_) => {
                    job();
                    return self.processes.lock().unwrap_or_else(|e| e.into_inner()).rows.clone();
                }
            }
        }
        rows
    }
}

/// One sample of `procs` (each one Marv started; any other is refused and left out).
fn sample(sampler: &mut Sampler, procs: &[(ProcessKind, String, u32)], now: u64) -> Vec<ChildProcess> {
    let rows = procs
        .iter()
        .filter_map(|(kind, name, pid)| {
            let sample = sampler.sample(*pid, now).ok()?;
            Some(ChildProcess {
                kind: *kind,
                name: marv_kernel::metrics::clean_name(name),
                pid: *pid,
                rss_kb: sample.rss_kb,
                cpu_pct: sample.cpu_pct,
                alive: sample.alive,
            })
        })
        .collect();
    let pids: Vec<u32> = procs.iter().map(|(_, _, pid)| *pid).collect();
    sampler.retain(&pids);
    rows
}

fn rate_limit(limit: marv_core::RateLimit) -> RateLimit {
    RateLimit {
        requests_limit: limit.requests_limit,
        requests_left: limit.requests_left,
        tokens_limit: limit.tokens_limit,
        tokens_left: limit.tokens_left,
    }
}

impl LiveMeter {
    /// One event of a chat another harness runs (the page's events, as `marv_web::claude_code` and `marv_web::acp`
    /// send them), at `now`: its turns, tools and approvals for the loop timers, its `usage` for the cache, $/h and
    /// today's spend. A harness reports usage once per turn, not per request, so it never counts for tokens/s.
    pub fn observe_harness(&self, event: &serde_json::Value, now: u64) {
        let mut inner = self.lock();
        let n = |k: &str| event[k].as_u64().unwrap_or(0);
        match event["type"].as_str() {
            // A `ready` that names no agent (a chat's page attaching to its terminal) keeps the one it had.
            Some("ready") => {
                if let Some(model) = event["model"].as_str() {
                    inner.harness = Some(harness_core(model));
                }
            }
            Some("turn_start") => inner.meter.turn_start(now),
            Some("turn_end") => inner.meter.turn_end(now),
            Some("tool_start") => inner.meter.tool_start(now),
            Some("tool_end") => inner.meter.tool_end(now),
            Some("approval") => inner.meter.wait_start(now),
            Some("approval_done") => inner.meter.wait_end(now),
            Some("usage") => {
                inner.meter.request(now, n("promptTokens"), n("cachedTokens"), n("completionTokens"), false);
                inner.meter.charge(now, micros(event["cost"].as_f64()));
                if let Some(budget) = event["contextLength"].as_u64().filter(|w| *w > 0) {
                    inner.context = Some(marv_kernel::metrics::Context { used: n("context"), budget });
                }
            }
            _ => {}
        }
    }

    /// A harness chat's door's `live` (runtime-kernel §20.2): its agent as the core, then its numbers, and its terminal
    /// agent's process ([`LiveMeter::set_agent_process`]). None until its first `ready`. Kept while it sleeps, so
    /// today's spend stays on its row.
    pub fn harness_live(&self, now: u64) -> Option<marv_kernel::metrics::Live> {
        let agent = self.agent.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let procs = agent.map(|(name, pid)| vec![(marv_kernel::metrics::ProcessKind::Process, name, pid)]).unwrap_or_default();
        let processes = self.processes(procs, now);
        let inner = self.lock();
        let core = inner.harness.clone()?;
        Some(marv_kernel::metrics::Live {
            core: Some(core),
            context: inner.context,
            metrics: Some(inner.meter.snapshot(now)),
            spend_usd_micros: Some(inner.meter.spent_today(now)),
            processes,
            ..Default::default()
        })
    }

    /// The chat's terminal agent's process from now on (None: it ended).
    pub fn set_agent_process(&self, process: Option<(String, u32)>) {
        *self.agent.lock().unwrap_or_else(|e| e.into_inner()) = process;
    }
}

/// A harness's name as Activity's model column shows it: one clean short line (it comes from the runner, but stays
/// within what any peer's core may be).
pub fn harness_core(name: &str) -> String {
    marv_kernel::metrics::clean_name(name)
}

/// The client's approver, timed: while it waits for the person, the agent is "waiting for you" (§20.9's iowait).
pub struct MeteredApprover {
    pub inner: Arc<dyn Approver>,
    pub meter: Arc<LiveMeter>,
}

/// Ends the wait however the approval ends (answered, or the turn stopped and the future dropped).
struct Waiting(Arc<LiveMeter>);

impl Drop for Waiting {
    fn drop(&mut self) {
        self.0.wait_end(crate::time::now_ms());
    }
}

#[async_trait]
impl Approver for MeteredApprover {
    async fn approve(&self, request: ApprovalRequest) -> Decision {
        self.meter.wait_start(crate::time::now_ms());
        let _waiting = Waiting(self.meter.clone());
        self.inner.approve(request).await
    }
}

/// One Activity row from a door's `marv/info` (already cut and cleaned on receipt: `PeerInfo::received`).
///
/// `credential_pid`: the pid of the process on the door, from its socket's peer credentials (review P3-3), never
/// `info.pid` (the door's own word). A row carries its chat id (what the desktop's buttons signal) only when that pid
/// is this process's: a chat of this app's, never another process's claim to be one.
pub fn row_of(found: &marv_mesh::discovery::LivePeer) -> marv_kernel::activity::AgentRow {
    use marv_kernel::activity::{AgentRow, RowKind};
    let peer = &found.peer;
    let info = &peer.info;
    let mine = found.credential_pid == Some(std::process::id());
    AgentRow {
        name: peer.session.clone(),
        // Shown through `printable()` (the terminal) or as text (the desktop), never as markup.
        folder: std::path::PathBuf::from(&info.cwd),
        kind: if info.chat.is_some() { RowKind::Chat } else { RowKind::Terminal },
        chat: info.chat.clone().filter(|c| mine && marv_kernel::AgentId::new(c).is_some_and(|a| a.as_str() == c)),
        pid: found.credential_pid.unwrap_or(0),
        state: info.state,
        title: info.card.as_ref().map(|c| c.title.clone()).filter(|t| !t.is_empty()),
        last_active_ms: info.card.as_ref().map(|c| c.last_active_ms).filter(|ms| *ms > 0),
        live: info.live.clone().unwrap_or_default(),
    }
}

/// Every Marv on this machine as Activity's table (runtime-kernel §20.2): each door in this person's mesh folder
/// (0700, theirs) asked for its `marv/info`. A desktop's chats each have a door, so they're rows too.
pub async fn collect(dir: &std::path::Path, cap: usize) -> marv_mesh::Result<marv_kernel::activity::Activity> {
    let peers = marv_mesh::discovery::list_live(dir).await?;
    Ok(marv_kernel::activity::Activity::from_rows(peers.iter().map(row_of).collect(), cap))
}

/// The kernel's events from `from_ms` on, from the day logs that cover it (`~/.marv/events/<yyyy-mm-dd>.jsonl`, at
/// most the last 7 days), oldest first. Lines that don't parse are skipped.
pub fn read_events(config_dir: &std::path::Path, from_ms: u64, now_ms: u64) -> Vec<(u64, marv_kernel::KernelEvent)> {
    let from_ms = from_ms.max(now_ms.saturating_sub(7 * marv_kernel::DAY_MS));
    let mut days = Vec::new();
    let mut day = from_ms - from_ms % marv_kernel::DAY_MS;
    while day <= now_ms {
        days.push(crate::kernel_events::events_path(config_dir, day));
        day += marv_kernel::DAY_MS;
    }
    let mut events = Vec::new();
    for path in days {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        events.extend(marv_kernel::parse_log(&text).0.into_iter().filter(|(at, _)| *at >= from_ms));
    }
    events
}

/// The timeline (runtime-kernel §20.3) from `from_ms`: fixed sentences, no message text. `hints`: (chat id, mesh
/// name) from Activity, for chats registered before the logs read.
pub fn timeline(
    config_dir: &std::path::Path,
    from_ms: u64,
    now_ms: u64,
    hints: &[(String, String)],
) -> Vec<marv_kernel::console::Line> {
    let events = read_events(config_dir, from_ms, now_ms);
    let names = marv_kernel::console::Names::from_log(&events, hints);
    marv_kernel::console::timeline(&events, &names)
}

/// "While you were away" for `[from_ms, now_ms)`, from the log's counts.
pub fn away(config_dir: &std::path::Path, from_ms: u64, now_ms: u64) -> marv_kernel::console::Away {
    marv_kernel::console::away(&read_events(config_dir, from_ms, now_ms), from_ms, now_ms)
}

/// "While you were away" across a person's absence (runtime-kernel §20.3; review P2-1, P3-10): when they left, and
/// today's spend then (every Marv's, from `collect`); when they're back after [`marv_kernel::console::AWAY_AFTER_MS`] or
/// more, the card from the log's counts with the spend that went by. Kept by the host (the desktop), computed here.
#[derive(Debug, Default)]
pub struct AwayTracker {
    left: Mutex<Option<(u64, u64)>>,
}

impl AwayTracker {
    /// The person left at `now`; `spent_today`: today's spend then. A second leave keeps the first.
    pub fn leave(&self, now: u64, spent_today: u64) {
        let mut left = self.left.lock().unwrap_or_else(|e| e.into_inner());
        if left.is_none() {
            *left = Some((now, spent_today));
        }
    }

    /// The person is back at `now`, today's spend now `spent_today`: the card, if they were away long enough and
    /// anything happened. A new UTC day meanwhile counts today's spend alone.
    pub fn back(&self, config_dir: &std::path::Path, now: u64, spent_today: u64) -> Option<marv_kernel::console::Away> {
        let (at, then) = self.left.lock().unwrap_or_else(|e| e.into_inner()).take()?;
        if now.saturating_sub(at) < marv_kernel::console::AWAY_AFTER_MS {
            return None;
        }
        let spent =
            if marv_kernel::day_of(at) == marv_kernel::day_of(now) { spent_today.saturating_sub(then) } else { spent_today };
        let card = marv_kernel::console::Away { spent_usd_micros: Some(spent), ..away(config_dir, at, now) };
        (!card.is_empty() || spent > 0).then_some(card)
    }
}

/// Permissions by chat (runtime-kernel §20.4; review P3-10): `chats` (id, title, folder) the host knows, the timers
/// from their file in `config_dir`, and the host's rules and grants (with today's spend). Show and revoke only. The
/// rules come from the host, which reads the rules file already for its own Mesh panel: this module never opens it,
/// so the rules are read only where they're honored or shown by the person's own tools (guard
/// `where_rules_arent_honored_a_matching_rule_changes_nothing`).
pub fn permissions(
    config_dir: &std::path::Path,
    chats: &[(String, String, std::path::PathBuf)],
    rules: &[marv_kernel::Policy],
    grants: &[marv_kernel::Grant],
    spend: &std::collections::BTreeMap<marv_kernel::GrantId, marv_kernel::Spend>,
    now: u64,
    utc_offset_ms: i64,
) -> Vec<marv_kernel::permissions::ChatPermissions> {
    let known: Vec<marv_kernel::permissions::KnownChat> = chats
        .iter()
        .filter_map(|(id, title, folder)| {
            let id = marv_kernel::AgentId::new(id).filter(|a| a.as_str() == id)?;
            Some(marv_kernel::permissions::KnownChat { id, title: title.clone(), folder: folder.clone() })
        })
        .collect();
    let timers = marv_kernel::TimerStore::new(config_dir).ok().and_then(|s| s.load().ok()).map(|s| s.timers).unwrap_or_default();
    marv_kernel::permissions::by_chat(&known, rules, grants, spend, &timers, now, utc_offset_ms)
}

/// About this Marv (runtime-kernel §20.7): the version, the limits in force and where each store lives. No
/// conversation text. `cap`: the host's K.
pub fn about(config_dir: &std::path::Path, cap: usize) -> marv_kernel::about::About {
    use marv_kernel::about::{About, Store};
    let store = |what: &str, path: std::path::PathBuf| Store { what: what.into(), path: path.display().to_string() };
    let mut stores = vec![
        store("rules", marv_kernel::policies_path(config_dir)),
        store("grants", marv_kernel::grants_path(config_dir)),
        store("grant spend", marv_kernel::spend_path(config_dir)),
        store("timers", marv_kernel::timers_path(config_dir)),
        store("event log", crate::kernel_events::events_dir(config_dir)),
        store("sessions", config_dir.join("sessions")),
    ];
    let env: marv_mcp::Env = std::env::vars().collect();
    if let Some(dir) = marv_mesh::discovery::mesh_dir(&env) {
        stores.push(store("doors (the mesh)", dir));
    }
    About {
        version: env!("CARGO_PKG_VERSION").into(),
        limits: marv_kernel::about::limits(cap, crate::mesh::WAKES_PER_MINUTE, crate::config::DEFAULT_COMPACT_TOKENS),
        stores,
        tool_abi: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marv_core::ToolCall;

    fn call(id: &str, name: &str, args: &str) -> ToolCall {
        ToolCall { id: id.into(), name: name.into(), arguments: args.into() }
    }

    #[test]
    fn a_ready_that_names_no_agent_keeps_the_row() {
        use serde_json::json;
        let m = LiveMeter::default();
        m.observe_harness(&json!({"type": "ready", "model": "Codex"}), 0);
        // A chat's page attaching to its terminal says `ready` with no model: its row stays (it went blank in Activity).
        m.observe_harness(&json!({"type": "ready", "harness": "codex", "terminal": true}), 1_000);
        assert_eq!(m.harness_live(2_000).and_then(|l| l.core).as_deref(), Some("Codex"));
    }

    #[test]
    fn a_harness_chats_events_fill_its_row() {
        use serde_json::json;
        let m = LiveMeter::default();
        // Nothing until its runner says which agent it is.
        assert_eq!(m.harness_live(0), None);
        m.observe_harness(&json!({"type": "ready", "model": "Claude\u{202E}Code"}), 0);
        m.observe_harness(&json!({"type": "turn_start"}), 1_000);
        m.observe_harness(&json!({"type": "tool_start", "id": "t1"}), 2_000);
        assert_eq!(m.phase(), Phase::Tools);
        m.observe_harness(&json!({"type": "approval", "id": 1}), 3_000);
        assert_eq!(m.phase(), Phase::You);
        m.observe_harness(&json!({"type": "approval_done", "id": 1}), 4_000);
        m.observe_harness(&json!({"type": "tool_end", "id": "t1"}), 5_000);
        let usage = json!({"type": "usage", "promptTokens": 1000, "cachedTokens": 800, "completionTokens": 50,
            "cost": 0.12, "context": 1000, "contextLength": 200_000});
        m.observe_harness(&usage, 6_000);
        m.observe_harness(&json!({"type": "turn_end", "reason": "end"}), 6_000);
        let live = m.harness_live(10_000).unwrap();
        // The name is cleaned as any peer's core is.
        assert_eq!(live.core.as_deref(), Some("Claude Code"));
        assert_eq!(live.spend_usd_micros, Some(120_000));
        assert_eq!(live.context, Some(marv_kernel::metrics::Context { used: 1000, budget: 200_000 }));
        let metrics = live.metrics.unwrap();
        assert!(metrics.usd_micros_per_hour.unwrap_or(0) > 0);
        assert_eq!(metrics.cache_hit_pct, Some(80));
        // A turn's usage is no request's stream: no tokens/s claimed.
        assert!(metrics.tokens_per_s.unwrap_or(0) == 0);
        assert_eq!(m.phase(), Phase::Idle);
    }

    #[test]
    fn the_turns_events_drive_the_meter() {
        let m = LiveMeter::default();
        m.observe(&SessionEvent::TurnStart { turn: "t".into() }, 0);
        let start = |c: ToolCall| SessionEvent::Loop(LoopEvent::ToolStart { call: c, label: String::new() });
        m.observe(&start(call("a", "agent", r#"{"type":"explore","prompt":"secret words"}"#)), 20_000);
        assert_eq!(m.phase(), Phase::Tools);
        let subs = m.subagents();
        assert_eq!((subs.len(), subs[0].kind.as_str(), subs[0].phase), (1, "explore", Phase::Model));
        m.observe(
            &SessionEvent::Subagent {
                call_id: "a".into(),
                event: LoopEvent::ToolStart { call: call("x", "read_file", "{}"), label: String::new() },
            },
            25_000,
        );
        assert_eq!(m.subagents()[0].phase, Phase::Tools);
        let end = SessionEvent::Loop(LoopEvent::ToolEnd {
            call: call("a", "agent", "{}"),
            run: marv_agent::agent::ToolRun::new(marv_core::ToolResult::default(), "agent"),
        });
        m.observe(&end, 40_000);
        assert!(m.subagents().is_empty());
        m.observe(
            &SessionEvent::TurnEnd { turn: "t".into(), reason: crate::session::TurnEndReason::Done(marv_core::StopReason::End) },
            60_000,
        );
        let s = m.metrics(60_000);
        assert_eq!((s.model_pct, s.tools_pct), (67, 33));
        assert_eq!(m.phase(), Phase::Idle);
    }

    /// "While you were away" (review P2-1, P3-10): nothing under ten minutes; after, the spend that went by today, and
    /// a new UTC day counts today's alone.
    #[test]
    fn away_counts_the_spend_that_went_by() {
        let dir = tempfile::tempdir().unwrap();
        let t = AwayTracker::default();
        let day = 30 * marv_kernel::DAY_MS;
        t.leave(day + 1000, 500);
        t.leave(day + 2000, 900);
        assert_eq!(t.back(dir.path(), day + 1000 + 60_000, 800), None, "a minute away");
        t.leave(day + 1000, 500);
        let card = t.back(dir.path(), day + 1000 + marv_kernel::console::AWAY_AFTER_MS, 800).unwrap();
        assert_eq!(card.spent_usd_micros, Some(300));
        t.leave(day + marv_kernel::DAY_MS - 1000, 5_000);
        let card = t.back(dir.path(), day + marv_kernel::DAY_MS + marv_kernel::console::AWAY_AFTER_MS, 40).unwrap();
        assert_eq!(card.spent_usd_micros, Some(40), "a new day: today's alone");
        assert_eq!(t.back(dir.path(), day * 2, 0), None, "back without leaving");
    }

    #[test]
    fn a_subagent_kind_is_a_name_or_nothing() {
        assert_eq!(subagent_kind("explore"), "explore");
        assert_eq!(subagent_kind("look at the secret plan"), "subagent");
        assert_eq!(subagent_kind(&"x".repeat(40)), "subagent");
    }

    #[tokio::test]
    async fn waiting_for_the_person_is_timed_even_when_the_turn_stops() {
        struct Never;
        #[async_trait]
        impl Approver for Never {
            async fn approve(&self, _: ApprovalRequest) -> Decision {
                std::future::pending().await
            }
        }
        let meter = Arc::new(LiveMeter::default());
        meter.observe(&SessionEvent::TurnStart { turn: "t".into() }, crate::time::now_ms());
        let approver = MeteredApprover { inner: Arc::new(Never), meter: meter.clone() };
        let ask = approver.approve(crate::runner::step_limit_request(1, 1));
        let _ = tokio::time::timeout(std::time::Duration::from_millis(20), ask).await;
        // Dropped mid-wait (the turn stopped): it's no longer waiting.
        assert_eq!(meter.phase(), Phase::Model);
    }
}
