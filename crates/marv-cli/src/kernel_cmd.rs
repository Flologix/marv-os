//! `marv kernel replay <file>...` (runtime-kernel phase 3): rebuilds the kernel's state from its event log
//! (`~/.marv/events/<yyyy-mm-dd>.jsonl`, `marv_session::kernel_events`) and prints it: the registry, the rules, the
//! states and the service grants (with the calls each ran). New in marv-rs.
//!
//! `marv kernel grants`: the person's service grants (`~/.marv/mesh/grants.json`), listed. It only reads: grants are
//! made, widened and revoked only in Marv desktop.
//!
//! Page-outs (runtime-kernel §11.4, `KernelEvent::Compacted`) are counted: how many, how many an overflow forced,
//! and the turns they saved to pages.
//! `marv kernel ps [--watch]` (runtime-kernel §20.6, O6): every Marv on this machine as the desktop's Activity tab
//! shows it, from each door's `marv/info` (`marv_session::live::collect`). TUI-free: it prints the table, and
//! `--watch` clears the screen and prints it again every 2 s. Only this person's doors (their 0700 mesh folder), and
//! no `/proc` read here: each Marv measures its own processes.
//!
//! `marv kernel log [file]...` (§20.6): the same fixed sentences as the desktop's timeline
//! (`marv_kernel::console::timeline`), one per line with its time (UTC). No message text: the log has none.
//!
//! Everything printed goes through `printable()`: a log is a file anyone with the user's rights could have edited.
//! And through `print_lines`: a reader that closes early (`marv kernel about | head -1`) ends the command quietly
//! with 0, where `println!` panicked (#69).

use std::fmt::Display;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use marv_core::printable;
use marv_kernel::{AgentKey, KernelEvent, KernelState, Subject, parse_log, replay};

/// How often `--watch` refreshes.
const WATCH_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

/// Writes `lines` to `out`, one per line, and flushes.
pub fn write_lines<L: Display>(out: &mut impl Write, lines: impl IntoIterator<Item = L>) -> io::Result<()> {
    for line in lines {
        writeln!(out, "{line}")?;
    }
    out.flush()
}

/// The exit code for what writing to stdout came to: a closed pipe is the reader having seen enough, not an error.
pub fn written(result: io::Result<()>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => 0,
        Err(e) => {
            // Not eprintln!, which panics on a closed stderr too.
            let _ = writeln!(io::stderr(), "marv: couldn't print: {e}");
            1
        }
    }
}

/// Prints `lines` on stdout (locked once); 0, or 1 if stdout failed other than by its reader closing.
pub fn print_lines<L: Display>(lines: impl IntoIterator<Item = L>) -> i32 {
    written(write_lines(&mut io::stdout().lock(), lines))
}

/// `marv kernel ps`'s lines for `activity`, each printable.
pub fn ps_lines(activity: &marv_kernel::activity::Activity) -> Vec<String> {
    if activity.rows.is_empty() {
        return vec!["No Marv is running here.".into()];
    }
    marv_kernel::activity::table(activity).iter().map(|l| printable(l)).collect()
}

/// `marv kernel ps [--watch]`.
pub async fn ps(watch: bool) -> i32 {
    let env: marv_mcp::Env = std::env::vars().collect();
    let Some(dir) = marv_mesh::discovery::mesh_dir(&env) else {
        eprintln!(
            "marv kernel ps needs Marv's runtime folder ($XDG_RUNTIME_DIR, or MARV_RUNTIME_DIR): no Marv is listed without it."
        );
        return 1;
    };
    loop {
        let activity = match marv_session::live::collect(&dir, marv_kernel::activity::DEFAULT_CAP).await {
            Ok(activity) => activity,
            Err(e) => {
                eprintln!("{}", printable(&e.to_string()));
                return 1;
            }
        };
        let mut out = io::stdout().lock();
        // Home and clear: a `top`-like refresh, no TUI.
        let cleared = if watch { out.write_all(b"\x1b[H\x1b[2J") } else { Ok(()) };
        let result = cleared.and_then(|()| write_lines(&mut out, ps_lines(&activity)));
        drop(out);
        // `--watch` piped somewhere that closed (`| head`) stops too.
        if !watch || result.is_err() {
            return written(result);
        }
        tokio::select! {
            _ = tokio::time::sleep(WATCH_EVERY) => {}
            _ = tokio::signal::ctrl_c() => return 0,
        }
    }
}

fn key(key: &AgentKey) -> String {
    printable(&key.to_string())
}

fn subject(subject: &Subject) -> String {
    match subject {
        Subject::Folder(scope) => printable(&scope.path().display().to_string()),
        Subject::Agent(id) => format!("chat {}", printable(id.as_str())),
    }
}

fn word<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| "?".into())
}

/// What `marv kernel replay` prints for `state` (rebuilt from `events` events, `skipped` lines skipped).
pub fn render(state: &KernelState, events: usize, skipped: usize) -> String {
    let mut out = Vec::new();
    out.push(format!("Registry ({}):", state.registry.len()));
    for e in state.registry.entries() {
        let at = e.state.map_or("no state".to_string(), |s| word(&s));
        out.push(format!(
            "- {}: {}, {}, {}, {at}",
            key(&e.id),
            printable(&e.mesh_name),
            word(&e.kind),
            printable(&e.folder.display().to_string())
        ));
    }
    out.push(format!("Rules ({}):", state.policies.len()));
    for (id, logged) in &state.policies {
        let r = &logged.rule;
        let way = if r.mutual { "each other" } else { "one way" };
        let until = match r.expires_at_ms {
            Some(ms) => format!("until {}", printable(&marv_session::time::iso(ms))),
            None => "until revoked".into(),
        };
        let expired = if logged.expired { ", expired" } else { "" };
        out.push(format!(
            "- {id}: {} -> {} ({}, {way}, {until}{expired}, by {}): {}",
            subject(&r.from),
            subject(&r.to),
            r.mode,
            word(&r.by),
            printable(&r.label)
        ));
    }
    out.push(format!("Grants ({}):", state.grants.len()));
    for logged in state.grants.values() {
        let expired = if logged.expired { ", expired" } else { "" };
        out.push(format!("- {}{expired}, {} calls ran", grant_line(&logged.grant, None), logged.used));
    }
    out.push(format!("States ({}):", state.states.len()));
    for (agent, s) in &state.states {
        out.push(format!("- {}: {}", key(agent), word(s)));
    }
    let t = &state.totals;
    out.push(format!(
        "{events} events: {} messages delivered, {} refused ({} with no wire), {} wire requests refused, {} wakes, {} wakes \
         skipped, {} rule wires issued, {} ended, {} calls run under a grant, {} grant pauses, {} chat reads served, {} \
         refused, {} compactions ({} forced by an overflow, {} turns saved to pages), {} interrupts ({} preempted), {} \
         signals, {} timers made, {} fired ({} late), {} void.{}",
        t.delivered,
        t.refused,
        t.refused_not_wired,
        t.wires_refused,
        t.wakes,
        t.wakes_skipped,
        t.issued,
        t.ended,
        t.granted_calls,
        t.grants_paused,
        t.reads_served,
        t.reads_refused,
        t.compactions,
        t.compactions_overflow,
        t.swap_turns,
        t.interrupts,
        t.preempted,
        t.signals,
        t.timers_made,
        t.timers_fired,
        t.timers_late,
        t.timers_void,
        if skipped > 0 { format!(" {skipped} lines skipped (not events of this version, or cut off).") } else { String::new() }
    ));
    out.join("\n")
}

fn dollars(micros: u64) -> String {
    format!("${:.2}", micros as f64 / 1e6)
}

/// One grant: its chat, folder, expiry and cap (and today's spend, when known).
fn grant_line(g: &marv_kernel::Grant, spent: Option<marv_kernel::Spend>) -> String {
    let until = match g.expires_at_ms {
        Some(ms) => format!("until {}", printable(&marv_session::time::iso(ms))),
        None => "until revoked".into(),
    };
    let spend = match spent {
        Some(s) => format!("{} of {} today", dollars(s.usd_micros), dollars(g.cap.usd_micros)),
        None => format!("{} a day", dollars(g.cap.usd_micros)),
    };
    format!(
        "{}: chat {}, {}, {until}, {spend}: {}",
        g.id,
        printable(g.chat.as_str()),
        printable(&g.folder.path().display().to_string()),
        printable(&g.label)
    )
}

/// What `marv kernel grants` prints at `now_ms`.
pub fn render_grants(
    snapshot: &marv_kernel::GrantSnapshot,
    spent: &std::collections::BTreeMap<marv_kernel::GrantId, marv_kernel::Spend>,
    now_ms: u64,
) -> String {
    if snapshot.grants.is_empty() {
        return "No chat works while you're away: Marv desktop's Mesh panel can let one.".into();
    }
    let mut out = vec![format!("Chats that work while you're away ({}):", snapshot.grants.len())];
    for g in &snapshot.grants {
        let today = spent.get(&g.id).copied().unwrap_or_default().today(now_ms);
        let state = if !g.live_at(now_ms) {
            " [expired]"
        } else if today.over(&g.cap, now_ms) {
            " [paused: today's cap]"
        } else {
            ""
        };
        out.push(format!("- {}{state}", grant_line(g, Some(today))));
    }
    out.join("\n")
}

/// `marv kernel grants`.
pub fn grants(config_dir: &Path, home: &Path) -> i32 {
    let store = match marv_kernel::GrantStore::new(config_dir, home) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("marv kernel grants: {}", printable(&e.to_string()));
            return 1;
        }
    };
    let snapshot = match store.load() {
        Ok(snapshot) => snapshot,
        Err(e) => {
            eprintln!("marv kernel grants: {} (it counts as no grants)", printable(&e.to_string()));
            return 1;
        }
    };
    let spent = store.load_spend().unwrap_or_default();
    print_lines([render_grants(&snapshot, &spent, marv_session::time::now_ms())])
}

/// The events in `files`, in order.
pub fn read(files: &[PathBuf]) -> Result<(Vec<(u64, KernelEvent)>, usize), String> {
    let (mut all, mut skipped) = (Vec::new(), 0);
    for file in files {
        let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", printable(&file.display().to_string())))?;
        let (events, bad) = parse_log(&text);
        all.extend(events);
        skipped += bad;
    }
    Ok((all, skipped))
}

/// `marv kernel replay <file>...`.
pub fn replay_files(files: &[PathBuf]) -> i32 {
    match read(files) {
        Ok((events, skipped)) => {
            let n = events.len();
            print_lines([render(&replay(events), n, skipped)])
        }
        Err(e) => {
            eprintln!("marv kernel replay: {e}");
            1
        }
    }
}

/// One meter's line: "2 turns, 14 requests, 120k in (80k cached), 3.1k out, $0.42 (~ estimated), 12k unpriced tokens".
fn metered(m: &marv_kernel::Metered) -> String {
    use marv_session::usage::tokens;
    let mut line = format!(
        "{} turn{}, {} request{}, {} in ({} cached), {} out, {}",
        m.turns,
        if m.turns == 1 { "" } else { "s" },
        m.requests,
        if m.requests == 1 { "" } else { "s" },
        tokens(m.tokens_in),
        tokens(m.cached),
        tokens(m.out),
        marv_kernel::budget::dollars(m.usd_micros)
    );
    if m.estimated {
        line.push_str(" (partly estimated)");
    }
    if m.unpriced_tokens > 0 {
        line.push_str(&format!(", {} unpriced tokens", tokens(m.unpriced_tokens)));
    }
    line
}

/// Picks one kind of meter.
type MeterKind = fn(&marv_kernel::MeterScope) -> bool;

/// What `marv kernel usage` prints: per day (newest first), everything, then each chat, folder, grant and core, and
/// the budgets set with today's spend against them. Every name through `printable()`.
pub fn render_usage(
    meters: &marv_kernel::Meters,
    budgets: Option<&marv_kernel::BudgetSnapshot>,
    now_ms: u64,
    spent: impl Fn(&marv_kernel::BudgetScope) -> marv_kernel::Spent,
) -> String {
    use marv_kernel::MeterScope;
    let mut out = Vec::new();
    let today = marv_kernel::day_of(now_ms);
    let days: Vec<u64> = meters.days().into_iter().rev().collect();
    if days.is_empty() {
        out.push("No usage logged yet.".to_string());
    }
    for day in days {
        let date = marv_session::time::iso_date(day * marv_kernel::DAY_MS);
        out.push(format!(
            "{date}{}: {}",
            if day == today { " (today)" } else { "" },
            metered(&meters.get(day, &MeterScope::All))
        ));
        let kinds: [(&str, MeterKind); 4] = [
            ("Folders", |s| matches!(s, MeterScope::Folder(_))),
            ("Chats", |s| matches!(s, MeterScope::Chat(_))),
            ("Grants", |s| matches!(s, MeterScope::Grant(_))),
            ("Cores", |s| matches!(s, MeterScope::Core(_))),
        ];
        for (title, kind) in kinds {
            let rows = meters.on_day(day, kind);
            if rows.is_empty() {
                continue;
            }
            out.push(format!("  {title}:"));
            for (scope, m) in rows {
                let name = match scope {
                    MeterScope::Folder(f) => f.display().to_string(),
                    MeterScope::Chat(c) | MeterScope::Core(c) => c,
                    MeterScope::Grant(g) => g.to_string(),
                    MeterScope::All => continue,
                };
                out.push(format!("  - {}: {}", printable(&name), metered(&m)));
            }
        }
    }
    let quotas: Vec<String> = meters
        .quotas()
        .map(|(core, (at, q))| {
            let mut parts = Vec::new();
            if let Some(r) = q.remaining {
                parts.push(format!("{r} left"));
            }
            if let Some(p) = q.used_percent {
                parts.push(format!("{p}% used"));
            }
            if let Some(reset) = q.reset_at_ms {
                parts.push(format!("resets {}", marv_session::time::iso(reset)));
            }
            format!("- {}: {} (as of {})", printable(core), parts.join(", "), marv_session::time::iso(*at))
        })
        .collect();
    if !quotas.is_empty() {
        out.push("Plan quotas:".into());
        out.extend(quotas);
    }
    match budgets {
        Some(b) if !b.budgets.is_empty() => {
            out.push("Budgets (today):".into());
            for budget in &b.budgets {
                let s = spent(&budget.scope);
                out.push(format!(
                    "- {}: {} of {} ({}%)",
                    printable(&budget_name(&budget.scope)),
                    marv_kernel::budget::dollars(s.usd_micros),
                    marv_kernel::budget::dollars(budget.cap.usd_micros),
                    marv_kernel::budget::percent(&s, &budget.cap)
                ));
            }
        }
        Some(_) => out.push("No budgets set (`marv kernel budget <dollars>`, or Marv desktop's Usage view).".into()),
        None => out.push("The budgets file can't be read: every budget counts as reached until it's fixed.".into()),
    }
    out.join("\n")
}

fn budget_name(scope: &marv_kernel::BudgetScope) -> String {
    match scope {
        marv_kernel::BudgetScope::Global => "everything".into(),
        marv_kernel::BudgetScope::Folder(f) => f.path().display().to_string(),
        marv_kernel::BudgetScope::Chat(c) => format!("chat {c}"),
    }
}

/// `marv kernel usage [--week]`.
pub fn usage(config_dir: &Path, week: bool) -> i32 {
    let now = marv_session::time::now_ms();
    let meters = marv_session::allocator::meters_from_log(config_dir, now, if week { 7 } else { 1 });
    let home = marv_session::paths::home_dir();
    let budgets = marv_kernel::BudgetStore::new(config_dir, &home).ok().and_then(|s| s.load().ok());
    let book = marv_session::allocator::MeterBook::new(Some(config_dir.to_path_buf()), None, None);
    book.refresh();
    print_lines([render_usage(&meters, budgets.as_ref(), now, |scope| book.spent(scope))])
}

/// Parses `10`, `2.50`, `$3` as millionths of a dollar.
pub fn parse_dollars(text: &str) -> Option<u64> {
    let usd: f64 = text.trim().trim_start_matches('$').parse().ok()?;
    marv_kernel::budget::dollar_cap(usd).map(|cap| cap.usd_micros)
}

/// `marv kernel budget [<dollars>|off] [--folder <dir> | --chat <id>]`: lists, or sets after the person's y on a
/// terminal (no terminal: refused, so a script or an agent's command can't set one).
pub async fn budget(config_dir: &Path, home: &Path, amount: Option<&str>, folder: Option<&str>, chat: Option<&str>) -> i32 {
    let store = match marv_kernel::BudgetStore::new(config_dir, home) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("marv kernel budget: {}", printable(&e.to_string()));
            return 1;
        }
    };
    let Some(amount) = amount else {
        return usage(config_dir, false);
    };
    let scope = match (folder, chat) {
        (Some(folder), _) => {
            let path = std::fs::canonicalize(folder).unwrap_or_else(|_| PathBuf::from(folder));
            match marv_kernel::FolderScope::new(&path, home) {
                Ok(scope) => marv_kernel::BudgetScope::Folder(scope),
                Err(e) => {
                    eprintln!("marv kernel budget: {}", printable(&e.to_string()));
                    return 1;
                }
            }
        }
        (None, Some(chat)) => match marv_kernel::AgentId::new(chat) {
            Some(id) => marv_kernel::BudgetScope::Chat(id.as_str().to_string()),
            None => {
                eprintln!("marv kernel budget: --chat takes a desktop chat's id");
                return 1;
            }
        },
        (None, None) => marv_kernel::BudgetScope::Global,
    };
    let cap = if amount == "off" {
        None
    } else {
        match parse_dollars(amount) {
            // A budget in dollars; unpriced requests (a plan's, a local model's) count against a token cap as large as
            // a grant's default.
            Some(micros) => Some(marv_kernel::SpendCap { usd_micros: micros, tokens: u64::MAX }),
            None => {
                eprintln!("marv kernel budget: {} isn't an amount of dollars (or off)", printable(amount));
                return 1;
            }
        }
    };
    let what = match &cap {
        Some(cap) => {
            format!("Set a budget of {} a day for {}?", marv_kernel::budget::dollars(cap.usd_micros), budget_name(&scope))
        }
        None => format!("Remove the budget for {}?", budget_name(&scope)),
    };
    match crate::mesh_cmd::confirm_on_tty(&what) {
        Some(true) => {}
        Some(false) => {
            eprintln!("Nothing changed.");
            return 1;
        }
        None => {
            eprintln!("marv kernel budget: a budget is set only by you, at a terminal (or in Marv desktop)");
            return 1;
        }
    }
    match store.set(scope.clone(), cap, marv_session::time::now_ms()).await {
        Ok(_) => {
            if let Ok(log) = marv_session::kernel_events::EventWriter::new(config_dir) {
                marv_kernel::EventLog::append(log.as_ref(), marv_session::time::now_ms(), KernelEvent::BudgetSet { scope, cap });
                log.flush().await;
            }
            print_lines(["Done."])
        }
        Err(e) => {
            eprintln!("marv kernel budget: {}", printable(&e.to_string()));
            1
        }
    }
}

/// `marv kernel log`'s lines for `events`: "HH:MM:SS  <sentence>", each printable.
pub fn log_lines(events: &[(u64, KernelEvent)]) -> Vec<String> {
    let names = marv_kernel::console::Names::from_log(events, &[]);
    marv_kernel::console::timeline(events, &names)
        .into_iter()
        .map(|line| printable(&format!("{}  {}", &marv_session::time::iso(line.at_ms)[11..19], line.text)))
        .collect()
}

/// `marv kernel log <file>...`.
pub fn log_files(files: &[PathBuf]) -> i32 {
    match read(files) {
        Ok((events, _)) => {
            let mut lines = log_lines(&events);
            if lines.is_empty() {
                lines.push("Nothing in the kernel's log yet.".into());
            }
            print_lines(lines)
        }
        Err(e) => {
            eprintln!("marv kernel log: {e}");
            1
        }
    }
}

/// The default when no file is given: today's log in the user's config folder.
pub fn todays_log(config_dir: &Path) -> PathBuf {
    marv_session::kernel_events::events_path(config_dir, marv_session::time::now_ms())
}

#[cfg(test)]
mod tests {
    use super::*;
    use marv_kernel::{AgentId, AgentKind, AgentState, FolderScope, LoggedEvent, Mode, Policy, PolicyChange, PolicyId, RuleBy};

    #[test]
    fn a_closed_reader_is_not_an_error_but_other_write_failures_are() {
        let mut out = Vec::new();
        assert!(write_lines(&mut out, ["one", "two"]).is_ok());
        assert_eq!(out, b"one\ntwo\n");
        assert_eq!(written(Ok(())), 0);
        assert_eq!(written(Err(io::ErrorKind::BrokenPipe.into())), 0);
        assert_eq!(written(Err(io::ErrorKind::StorageFull.into())), 1);
    }

    #[test]
    fn usage_prints_each_meter_and_the_budgets_printably() {
        let mut meters = marv_kernel::Meters::default();
        let usage = marv_kernel::TurnUsage {
            id: 1,
            chat: "c\x1b[2J1".into(),
            folder: "/home/me/acme".into(),
            core: "zai/glm-4.6".into(),
            grant: None,
            requests: 3,
            tokens_in: 12_000,
            cached: 8_000,
            out: 500,
            cost_micros: Some(420_000),
            estimated: true,
            unpriced_tokens: 0,
            wall_ms: 10,
            started_by: marv_kernel::StartedBy::Person,
            cores: Vec::new(),
            quota: Some(marv_kernel::PlanQuota { remaining: Some(40), used_percent: None, reset_at_ms: None }),
        };
        meters.record(5, &usage);
        let budgets = marv_kernel::BudgetSnapshot {
            generation: 1,
            budgets: vec![marv_kernel::Budget {
                scope: marv_kernel::BudgetScope::Global,
                cap: marv_kernel::SpendCap { usd_micros: 1_000_000, tokens: u64::MAX },
                by: marv_kernel::BudgetBy::Person,
                set_at_ms: 1,
            }],
        };
        let shown = render_usage(&meters, Some(&budgets), 5, |_| marv_kernel::Spent { usd_micros: 420_000, tokens: 0 });
        assert!(
            shown.starts_with("1970-01-01 (today): 1 turn, 3 requests, 12k in (8k cached), 500 out, $0.42 (partly estimated)"),
            "{shown}"
        );
        assert!(shown.contains("  - /home/me/acme: 1 turn"), "{shown}");
        assert!(shown.contains("- zai/glm-4.6: 40 left"), "{shown}");
        assert!(shown.contains("- everything: $0.42 of $1.00 (42%)"), "{shown}");
        assert!(!shown.contains('\x1b'), "printable: {shown}");
        assert_eq!(parse_dollars("2.50"), Some(2_500_000));
        assert_eq!(parse_dollars("$3"), Some(3_000_000));
        assert_eq!(parse_dollars("-1"), None);
        assert_eq!(parse_dollars("lots"), None);
    }

    #[test]
    fn replay_prints_the_registry_rules_and_states_printably() {
        let a = AgentKey::Chat(AgentId::new("554963ee-fd8e-4bec-a7dc-49051f090acc").unwrap());
        let scope = FolderScope::new(Path::new("/home/me/acme"), Path::new("/home/me")).unwrap();
        let rule = Policy {
            id: PolicyId(3),
            from: Subject::Folder(scope.clone()),
            to: Subject::Folder(scope),
            mode: Mode::Send,
            mutual: true,
            expires_at_ms: None,
            by: RuleBy::Cli,
            created_at_ms: 1,
            label: "Acme\x1b[2J talks".into(),
        };
        let events = [
            KernelEvent::Registered {
                agent: a.clone(),
                mesh_name: "acme-554963ee".into(),
                folder: "/home/me/acme".into(),
                kind: AgentKind::Chat,
            },
            KernelEvent::State { agent: a.clone(), from: None, to: AgentState::Sleeping },
            KernelEvent::Policy { id: PolicyId(3), change: PolicyChange::Created, rule: Some(rule) },
            KernelEvent::Wake { agent: a.clone(), by: "peer-web-0001".into(), hop: Some(1) },
            KernelEvent::WakeSkipped {
                agent: a,
                by: "peer-web-0001".into(),
                hop: Some(2),
                reason: marv_kernel::WakeSkipReason::Hop,
                count: 3,
            },
            KernelEvent::WireRefused {
                agent: AgentKey::Mesh("acme-554963ee".into()),
                from: "parent".into(),
                from_agent: None,
                to: "peer.web-0001.agent.parent.inbox".into(),
                mode: Mode::Send,
                reason: marv_kernel::WireRefusal::Declined,
            },
            KernelEvent::Read {
                reader: AgentKey::Mesh("acme-554963ee".into()),
                target: AgentKey::Mesh("web-0001".into()),
                what: marv_kernel::ReadWhat::Summary,
                n: None,
                size: 120,
                outcome: marv_kernel::ReadOutcome::Served,
                reason: None,
            },
            KernelEvent::Compacted {
                chat: AgentKey::Mesh("acme-554963ee".into()),
                by: marv_kernel::CompactedBy::Overflow,
                level: Some(1),
                tokens_before: Some(42_000),
                tokens_after: 9_000,
                cost_usd_micros: Some(30_000),
                page: Some(1),
                swap_turns: 118,
            },
        ];
        let dir = std::env::temp_dir().join(format!("marv-replay-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("2026-10-17.jsonl");
        let text: String = events.iter().map(|e| LoggedEvent::new(5, e.clone()).line() + "\n").collect();
        std::fs::write(&file, text + "{\"v\":1,").unwrap();
        let (events, skipped) = read(std::slice::from_ref(&file)).unwrap();
        let shown = render(&replay(events), 8, skipped);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            shown,
            "Registry (1):\n\
             - chat 554963ee-fd8e-4bec-a7dc-49051f090acc: acme-554963ee, chat, /home/me/acme, sleeping\n\
             Rules (1):\n\
             - rule 3: /home/me/acme -> /home/me/acme (send, each other, until revoked, by cli): Acme talks\n\
             Grants (0):\n\
             States (1):\n\
             - chat 554963ee-fd8e-4bec-a7dc-49051f090acc: sleeping\n\
             8 events: 0 messages delivered, 0 refused (0 with no wire), 1 wire requests refused, 1 wakes, 3 wakes skipped, 0 rule wires issued, 0 ended, 0 calls run under a grant, 0 grant pauses, 1 chat reads served, 0 refused, 1 compactions (1 forced by an overflow, 118 turns saved to pages), 0 interrupts (0 preempted), 0 signals, 0 timers made, 0 fired (0 late), 0 void. 1 lines skipped (not events of this version, or cut off)."
        );
        assert!(read(&[dir.join("missing.jsonl")]).is_err());
    }

    fn grant() -> marv_kernel::Grant {
        marv_kernel::Grant {
            id: marv_kernel::GrantId(2),
            chat: AgentId::new("554963ee-fd8e-4bec-a7dc-49051f090acc").unwrap(),
            folder: FolderScope::new(Path::new("/home/me/acme"), Path::new("/home/me")).unwrap(),
            expires_at_ms: None,
            cap: marv_kernel::SpendCap::default(),
            by: marv_kernel::GrantBy::Desktop,
            created_at_ms: 1,
            label: "acme\x1b[2J works".into(),
        }
    }

    #[test]
    fn replay_counts_the_calls_each_grant_ran() {
        let g = grant();
        let events = vec![
            (1, KernelEvent::GrantMade { grant: g.clone() }),
            (2, KernelEvent::grant_used(g.id, g.chat.clone(), "write_file", "files", 1)),
            (3, KernelEvent::grant_used(g.id, g.chat.clone(), "bash", "bash", 1)),
            (4, KernelEvent::GrantPaused { id: g.id, reason: marv_kernel::GrantPauseReason::Spend }),
        ];
        let shown = render(&replay(events), 4, 0);
        assert!(
            shown.contains(
                "Grants (1):\n- grant 2: chat 554963ee-fd8e-4bec-a7dc-49051f090acc, /home/me/acme, until revoked, $2.00 a day: acme works, 2 calls ran"
            ),
            "{shown}"
        );
        assert!(shown.contains("2 calls run under a grant, 1 grant pauses, 0 chat reads served"), "{shown}");
    }

    #[test]
    fn log_prints_the_timeline_with_times_and_no_text() {
        let a = AgentKey::Chat(AgentId::new("554963ee-fd8e-4bec-a7dc-49051f090acc").unwrap());
        let events = vec![
            (
                1_000,
                KernelEvent::Registered {
                    agent: a.clone(),
                    mesh_name: "acme\x1b[2J".into(),
                    folder: "/p".into(),
                    kind: AgentKind::Chat,
                },
            ),
            (61_000, KernelEvent::Wake { agent: a, by: "peer-infra".into(), hop: None }),
        ];
        let lines = log_lines(&events);
        assert_eq!(lines[1], "00:01:01  acme [2J was woken by infra");
        assert!(lines.iter().all(|l| !l.contains('\x1b')));
    }

    #[test]
    fn ps_says_when_nothing_runs() {
        assert_eq!(ps_lines(&marv_kernel::activity::Activity::default()), ["No Marv is running here."]);
    }

    #[test]
    fn grants_lists_spend_and_pauses_printably() {
        let g = grant();
        let snapshot = marv_kernel::GrantSnapshot { generation: 1, grants: vec![g.clone()] };
        let now = 3 * marv_kernel::DAY_MS;
        let mut spend = marv_kernel::Spend::default();
        spend.charge(now, Some(0.4), 0);
        let spent: std::collections::BTreeMap<_, _> = [(g.id, spend)].into_iter().collect();
        assert_eq!(
            render_grants(&snapshot, &spent, now),
            "Chats that work while you're away (1):\n- grant 2: chat 554963ee-fd8e-4bec-a7dc-49051f090acc, /home/me/acme, until revoked, $0.40 of $2.00 today: acme works"
        );
        spend.charge(now, Some(1.6), 0);
        let spent: std::collections::BTreeMap<_, _> = [(g.id, spend)].into_iter().collect();
        assert!(render_grants(&snapshot, &spent, now).ends_with("[paused: today's cap]"));
        assert!(render_grants(&marv_kernel::GrantSnapshot::default(), &spent, now).starts_with("No chat works"));
    }
}
