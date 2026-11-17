//! The visible OS's data model (runtime-kernel §20, decisions O1, O2 and O6). New in marv-rs.
//!
//! **One table across apps.** Every agent on this machine is a row: the desktop's chats and the terminal Marvs alike,
//! each from its own door's `marv/info` (kernel state, never a UI's guess), with its subagents and real processes
//! nested under it. The rows are built by whoever lists the doors (`marv_session::live::collect`); this module holds
//! the shapes, the state words, the grouping (by folder or by team) and the table `marv kernel ps` prints.
//!
//! **For the person only** (§20.8): names, states, counts and numbers. A card's title is the person's own typing and
//! appears only in a chat's row. Nothing here is ever offered to a model.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::identity::{AgentId, AgentKind, AgentRef};
use crate::metrics::{ChildProcess, Live, Phase, SubagentRow};
use crate::policy::Policy;
use crate::scheduler::AgentState;

/// The scheduler's cap K on the desktop (§5.3): what "6 of 8 loaded" counts against.
pub const DEFAULT_CAP: usize = 8;

/// What kind of agent a row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowKind {
    /// A Marv desktop chat.
    Chat,
    /// A Marv in a terminal.
    Terminal,
}

/// One agent on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRow {
    /// Its mesh name.
    pub name: String,
    pub folder: PathBuf,
    pub kind: RowKind,
    /// A desktop chat's id, only for a chat of the process listing (its door's credential pid is that process's): what
    /// the desktop's buttons signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<AgentState>,
    /// Its chat card's title (the person's own words), when its Marv publishes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active_ms: Option<u64>,
    #[serde(default)]
    pub live: Live,
}

/// A rule's chats (§15.3's team): "Stop the team" is on its header.
#[derive(Debug)]
pub struct Team<'a> {
    pub rule: &'a Policy,
    pub rows: Vec<&'a AgentRow>,
}

impl AgentRow {
    /// Who this row is, as a rule sees it.
    pub fn agent(&self) -> AgentRef {
        let id = self.chat.as_deref().and_then(AgentId::new);
        AgentRef {
            mesh_name: self.name.clone(),
            kind: if id.is_some() { AgentKind::Chat } else { AgentKind::Terminal },
            id,
            folder: self.folder.clone(),
        }
    }
}

/// Activity's state words (§20.1): sleeping, loading, idle, busy, waiting for you, paused.
pub fn state_word(row: &AgentRow) -> &'static str {
    if row.live.paused {
        return "paused";
    }
    match row.state {
        Some(AgentState::Sleeping) => "sleeping",
        Some(AgentState::Loading) => "loading",
        Some(AgentState::Idle) => "idle",
        Some(AgentState::Busy) => "busy",
        Some(AgentState::Waiting) => "waiting for you",
        None => "running",
    }
}

/// One model's line in the system bar (§20.9's per-core graphs).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelBar {
    pub model: String,
    pub agents: usize,
    pub tokens_per_s: u32,
    pub usd_micros_per_hour: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_hit_pct: Option<u8>,
    /// The rate limit's use, % (the busiest agent's headers), when the provider says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_pct: Option<u8>,
    /// Throttles (429s) in the last ten minutes: "running hot" where no headers come.
    pub throttles: u32,
}

/// The bar under the table (§20.9).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemBar {
    /// Loaded sessions, and the cap K.
    pub loaded: usize,
    pub cap: usize,
    pub models: Vec<ModelBar>,
    /// The agents' spend today (each load's, the UTC day), micro-dollars.
    pub spend_usd_micros: u64,
}

/// The whole table.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activity {
    pub rows: Vec<AgentRow>,
    pub bar: SystemBar,
}

/// A row is loaded when its state says so; a terminal Marv (no scheduler) is always loaded.
fn loaded(row: &AgentRow) -> bool {
    row.state.is_none_or(|s| s.loaded())
}

impl Activity {
    /// The table from `rows`, sorted by folder then name, with the system bar summed from them.
    pub fn from_rows(mut rows: Vec<AgentRow>, cap: usize) -> Self {
        rows.sort_by(|a, b| (&a.folder, &a.name).cmp(&(&b.folder, &b.name)));
        let mut models: BTreeMap<String, ModelBar> = BTreeMap::new();
        let mut cached: BTreeMap<String, (u32, u32)> = BTreeMap::new();
        for row in &rows {
            let (Some(core), Some(m)) = (&row.live.core, &row.live.metrics) else { continue };
            let bar = models.entry(core.clone()).or_insert_with(|| ModelBar { model: core.clone(), ..ModelBar::default() });
            // A peer's numbers are its say: summed without overflowing (review P3-2).
            bar.agents = bar.agents.saturating_add(1);
            bar.tokens_per_s = bar.tokens_per_s.saturating_add(m.tokens_per_s.unwrap_or(0));
            bar.usd_micros_per_hour = bar.usd_micros_per_hour.saturating_add(m.usd_micros_per_hour.unwrap_or(0));
            bar.throttles = bar.throttles.saturating_add(m.throttles);
            if let Some(used) = m.rate_limit.and_then(|r| r.used_pct()) {
                bar.rate_limit_pct = Some(bar.rate_limit_pct.map_or(used, |p| p.max(used)));
            }
            if let Some(hit) = m.cache_hit_pct {
                let c = cached.entry(core.clone()).or_default();
                *c = (c.0.saturating_add(u32::from(hit.min(100))), c.1.saturating_add(1));
            }
        }
        for (core, (sum, n)) in cached {
            if let Some(bar) = models.get_mut(&core) {
                bar.cache_hit_pct = Some((sum / n.max(1)).min(100) as u8);
            }
        }
        let bar = SystemBar {
            // K is the desktop's scheduler's: only its chats count against it (review P3-6).
            loaded: rows.iter().filter(|r| r.kind == RowKind::Chat && loaded(r)).count(),
            cap,
            models: models.into_values().collect(),
            spend_usd_micros: rows.iter().filter_map(|r| r.live.spend_usd_micros).fold(0u64, u64::saturating_add),
        };
        Self { rows, bar }
    }

    /// The rows grouped by team (§20.1): one team per rule live at `now`, the rows it covers (either end), in rule
    /// order. A row in no team isn't listed; a row may be in several.
    pub fn by_team<'a>(&'a self, rules: &'a [Policy], now_ms: u64) -> Vec<Team<'a>> {
        rules
            .iter()
            .filter(|r| r.live_at(now_ms))
            .map(|rule| Team {
                rule,
                rows: self.rows.iter().filter(|row| rule.from.covers(&row.agent()) || rule.to.covers(&row.agent())).collect(),
            })
            .filter(|t| !t.rows.is_empty())
            .collect()
    }

    /// The rows grouped by folder, in order.
    pub fn by_folder(&self) -> Vec<(PathBuf, Vec<&AgentRow>)> {
        let mut groups: BTreeMap<PathBuf, Vec<&AgentRow>> = BTreeMap::new();
        for row in &self.rows {
            groups.entry(row.folder.clone()).or_default().push(row);
        }
        groups.into_iter().collect()
    }
}

/// A team as the desktop's Activity shows it: its rule, its label, and its rows' names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamView {
    pub rule: u64,
    pub label: String,
    pub names: Vec<String>,
}

/// What the desktop's Activity tab renders (§20.1): the table, with the pauses the host knows (a desktop chat's
/// pause lives in the desktop), and the teams.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct View {
    pub activity: Activity,
    pub teams: Vec<TeamView>,
}

/// [`View`] from `activity`, marking the chats in `paused` paused and grouping by the live `rules`.
pub fn view(mut activity: Activity, paused: &[String], rules: &[Policy], now_ms: u64) -> View {
    for row in &mut activity.rows {
        if row.chat.as_ref().is_some_and(|c| paused.contains(c)) {
            row.live.paused = true;
        }
    }
    let teams = activity
        .by_team(rules, now_ms)
        .into_iter()
        .map(|t| TeamView {
            rule: t.rule.id.0,
            // The person's words, but from a file anyone with their rights can edit: one clean line (review P3-7).
            label: crate::metrics::clean_name(&t.rule.label),
            names: t.rows.iter().map(|r| r.name.clone()).collect(),
        })
        .collect();
    View { activity, teams }
}

fn dollars(micros: u64) -> String {
    format!("${:.2}", micros as f64 / 1e6)
}

fn phase_word(phase: Phase) -> &'static str {
    match phase {
        Phase::Idle => "idle",
        Phase::Model => "model",
        Phase::Tools => "tools",
        Phase::You => "waiting for you",
    }
}

fn memory(kb: Option<u64>) -> String {
    match kb {
        Some(kb) if kb >= 1024 => format!("{} MB", kb / 1024),
        Some(kb) => format!("{kb} KB"),
        None => "—".into(),
    }
}

fn child_line(prefix: &str, name: &str, state: &str, mem: &str, cpu: &str) -> String {
    format!("{:<17}{:<16}{:>7}{:>6}", format!("{prefix}{name}"), state, mem, cpu)
}

fn process_line(last: bool, p: &ChildProcess) -> String {
    let kind = match p.kind {
        crate::metrics::ProcessKind::Mcp => "mcp: ",
        crate::metrics::ProcessKind::Process => "",
    };
    let state = match p.alive {
        Some(false) => "exited",
        _ => "running",
    };
    let cpu = p.cpu_pct.map_or("—".into(), |c| format!("{c}%"));
    child_line(if last { "  └ " } else { "  ├ " }, &format!("{kind}{}", p.name), state, &memory(p.rss_kb), &cpu)
        + "   (real process)"
}

fn subagent_line(last: bool, s: &SubagentRow) -> String {
    child_line(if last { "  └ " } else { "  ├ " }, &s.kind, phase_word(s.phase), "", "") + "   (subagent)"
}

/// What `marv kernel ps` prints (§20.6, the table in §20.9): one line per agent, its subagents and real processes
/// indented under it, then the system bar. The words are fixed; names were cut and cleaned on receipt.
pub fn table(activity: &Activity) -> Vec<String> {
    let mut out = vec![format!(
        "{:<17}{:<16}{:>7}{:>6}  {:<16}{:>7}{:>7}  MODEL",
        "AGENT", "STATE", "CTX", "CPU", "model/tools/you", "$/h", "CACHE"
    )];
    for row in &activity.rows {
        let live = &row.live;
        let ctx = live.context.and_then(|c| c.pct()).map_or("—".into(), |p| format!("{p}%"));
        let (cpu, split, per_hour, cache) = match &live.metrics {
            Some(m) => (
                format!("{}%", m.model_pct),
                format!("{:>3} /{:>3} /{:>3}", m.model_pct, m.tools_pct, m.you_pct),
                m.usd_micros_per_hour.map_or("—".into(), dollars),
                m.cache_hit_pct.map_or("—".into(), |c| format!("{c}%")),
            ),
            None => ("—".into(), String::new(), "—".into(), "—".into()),
        };
        let core = live.core.clone().unwrap_or_else(|| "—".into());
        out.push(format!("{:<17}{:<16}{ctx:>7}{cpu:>6}  {split:<16}{per_hour:>7}{cache:>7}  {core}", row.name, state_word(row)));
        let children = live.subagents.len() + live.processes.len();
        let mut n = 0;
        for s in &live.subagents {
            n += 1;
            out.push(subagent_line(n == children, s));
        }
        for p in &live.processes {
            n += 1;
            out.push(process_line(n == children, p));
        }
    }
    out.push("─".repeat(78));
    let bar = &activity.bar;
    let mut parts = vec![format!("Loaded {} of {}", bar.loaded, bar.cap)];
    for m in &bar.models {
        let mut line =
            format!("{}: {} agent{}, {} tok/s", m.model, m.agents, if m.agents == 1 { "" } else { "s" }, m.tokens_per_s);
        match m.rate_limit_pct {
            Some(pct) => line.push_str(&format!(", rate limit {pct}% used")),
            None if m.throttles > 0 => line.push_str(&format!(", running hot ({} throttled)", m.throttles)),
            None => {}
        }
        if m.usd_micros_per_hour > 0 {
            line.push_str(&format!(", {}/h", dollars(m.usd_micros_per_hour)));
        }
        parts.push(line);
    }
    parts.push(format!("today {}", dollars(bar.spend_usd_micros)));
    out.push(parts.join(" · "));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::{Context, Metrics, ProcessKind, RateLimit};

    fn row(name: &str, folder: &str, state: Option<AgentState>, live: Live) -> AgentRow {
        AgentRow {
            name: name.into(),
            folder: folder.into(),
            kind: if state.is_some() { RowKind::Chat } else { RowKind::Terminal },
            chat: None,
            pid: 7,
            state,
            title: None,
            last_active_ms: None,
            live,
        }
    }

    fn busy() -> Live {
        Live {
            core: Some("glm-5.3".into()),
            context: Some(Context { used: 62, budget: 100 }),
            metrics: Some(Metrics {
                model_pct: 58,
                tools_pct: 13,
                you_pct: 0,
                tokens_per_s: Some(48),
                cache_hit_pct: Some(84),
                usd_micros_per_hour: Some(1_100_000),
                rate_limit: Some(RateLimit { requests_limit: Some(100), requests_left: Some(69), ..RateLimit::default() }),
                ..Metrics::default()
            }),
            spend_usd_micros: Some(3_800_000),
            processes: vec![ChildProcess {
                kind: ProcessKind::Mcp,
                name: "linear".into(),
                pid: 9,
                rss_kb: Some(44 * 1024),
                cpu_pct: Some(0),
                alive: Some(true),
            }],
            subagents: vec![SubagentRow { kind: "explore".into(), phase: Phase::Tools }],
            ..Live::default()
        }
    }

    #[test]
    fn the_table_shows_every_agent_its_children_and_the_bar() {
        let rows = vec![
            row("infra", "/p/infra", Some(AgentState::Sleeping), Live::default()),
            row("acme-api", "/p/acme", Some(AgentState::Busy), busy()),
            row("term-1a2b", "/p/acme", None, Live { paused: false, ..Live::default() }),
        ];
        let activity = Activity::from_rows(rows, DEFAULT_CAP);
        let lines = table(&activity);
        assert!(lines[0].starts_with("AGENT"));
        assert!(lines[1].starts_with("acme-api") && lines[1].contains("busy") && lines[1].contains("62%"), "{}", lines[1]);
        assert!(lines[1].contains("$1.10") && lines[1].contains("84%") && lines[1].ends_with("glm-5.3"), "{}", lines[1]);
        assert!(lines[2].starts_with("  ├ explore") && lines[2].contains("tools"), "{}", lines[2]);
        assert!(lines[3].starts_with("  └ mcp: linear") && lines[3].contains("44 MB"), "{}", lines[3]);
        assert!(lines[4].starts_with("term-1a2b") && lines[4].contains("running"), "{}", lines[4]);
        assert!(lines[5].starts_with("infra") && lines[5].contains("sleeping"));
        let bar = lines.last().unwrap();
        assert!(bar.starts_with("Loaded 1 of 8"), "only desktop chats count against K: {bar}");
        assert!(
            bar.contains("glm-5.3: 1 agent, 48 tok/s, rate limit 31% used, $1.10/h") && bar.ends_with("today $3.80"),
            "{bar}"
        );
    }

    #[test]
    fn state_words_and_folders() {
        let mut r = row("a", "/p", Some(AgentState::Waiting), Live::default());
        assert_eq!(state_word(&r), "waiting for you");
        r.live.paused = true;
        assert_eq!(state_word(&r), "paused");
        let activity =
            Activity::from_rows(vec![r, row("b", "/q", None, Live::default()), row("c", "/p", None, Live::default())], 8);
        let groups = activity.by_folder();
        assert_eq!(
            groups.iter().map(|(f, rows)| (f.display().to_string(), rows.len())).collect::<Vec<_>>(),
            [("/p".to_string(), 2), ("/q".to_string(), 1)]
        );
    }

    #[test]
    fn teams_are_the_rows_a_live_rule_covers() {
        use crate::identity::FolderScope;
        use crate::policy::{PolicyId, RuleBy, Subject};
        let home = std::path::Path::new("/home/u");
        let rule = |id, folder: &str, expires| Policy {
            id: PolicyId(id),
            from: Subject::Folder(FolderScope::new(std::path::Path::new(folder), home).unwrap()),
            to: Subject::Folder(FolderScope::new(std::path::Path::new(folder), home).unwrap()),
            mode: crate::mode::Mode::Send,
            mutual: true,
            expires_at_ms: expires,
            by: RuleBy::Desktop,
            created_at_ms: 0,
            label: "acme".into(),
        };
        let activity = Activity::from_rows(
            vec![row("a", "/p/acme/api", None, Live::default()), row("b", "/p/infra", None, Live::default())],
            8,
        );
        let rules = [rule(3, "/p/acme", None), rule(4, "/p/infra", Some(5))];
        let teams = activity.by_team(&rules, 10);
        assert_eq!(teams.len(), 1, "rule 4 expired");
        assert_eq!((teams[0].rule.id.0, teams[0].rows.len(), teams[0].rows[0].name.as_str()), (3, 1, "a"));
    }

    #[test]
    fn throttles_show_as_running_hot_without_headers() {
        let mut live = busy();
        if let Some(m) = live.metrics.as_mut() {
            m.rate_limit = None;
            m.throttles = 3;
        }
        let lines = table(&Activity::from_rows(vec![row("a", "/p", None, live)], 8));
        assert!(lines.last().unwrap().contains("running hot (3 throttled)"));
    }
}
