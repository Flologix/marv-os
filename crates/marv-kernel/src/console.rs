//! The activity log as the person reads it: the console (runtime-kernel §20.3, decision O3; §20.6's `marv kernel log`).
//! New in marv-rs.
//!
//! The event log ([`crate::eventlog`]) holds no message text, so neither does this: each event kind has **fixed
//! sentences**, filled with names (mesh names, from the log's own `registered` events or the host's hints), rule and
//! grant numbers, counts and fixed words. Any name the log carries is cut and cleaned ([`crate::metrics::clean_name`]):
//! a log is a file anyone with the user's rights could have edited. Guard `the_log_view_has_no_message_text`.
//!
//! **"While you were away"** ([`away`]): one card sums an interval from counts alone: "3 chats ran 41 calls · 1 needs
//! you · $0.62 spent". The spend comes from the host (the log has none).
//!
//! **Notification history** ([`Notifications`], §20.5): the notifications a host showed (needs you, finished, paused),
//! kept in memory for the app's run, never on disk; each opens its chat.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::eventlog::{Delivery, KernelEvent, PolicyChange, ReadOutcome};
use crate::metrics::clean_name;
use crate::registry::AgentKey;
use crate::scheduler::AgentState;

/// Who each agent in a log is, by name.
#[derive(Debug, Clone, Default)]
pub struct Names {
    names: BTreeMap<AgentKey, String>,
}

impl Names {
    /// Names from the log's own `registered` events, then the host's `hints` (a chat id's mesh name, from Activity).
    pub fn from_log<'a>(events: impl IntoIterator<Item = &'a (u64, KernelEvent)>, hints: &[(String, String)]) -> Self {
        let mut names = BTreeMap::new();
        for (_, event) in events {
            if let KernelEvent::Registered { agent, mesh_name, .. } = event {
                names.insert(agent.clone(), clean_name(mesh_name));
            }
        }
        for (chat, name) in hints {
            if let Some(id) = crate::AgentId::new(chat).filter(|a| a.as_str() == chat) {
                names.insert(AgentKey::Chat(id), clean_name(name));
            }
        }
        Self { names }
    }

    /// An agent's name: its mesh name, or a short form of its key.
    pub fn of(&self, agent: &AgentKey) -> String {
        if let Some(name) = self.names.get(agent) {
            return name.clone();
        }
        match agent {
            AgentKey::Chat(id) => format!("chat {}", id.as_str().chars().take(8).collect::<String>()),
            AgentKey::Mesh(name) => clean_name(name),
        }
    }

    fn chat(&self, chat: &crate::AgentId) -> String {
        self.of(&AgentKey::Chat(chat.clone()))
    }
}

/// One line of the timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    pub at_ms: u64,
    /// The event kind, for filtering ("wake", "message", "grant", ...).
    pub kind: String,
    /// The chat a click opens, when the line is about one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
    /// The names it involves, for filtering by chat or team.
    pub agents: Vec<String>,
    /// The fixed sentence.
    pub text: String,
}

fn word<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value).ok().and_then(|v| v.as_str().map(|s| s.replace('_', " "))).unwrap_or_else(|| "?".into())
}

fn chat_of(agent: &AgentKey) -> Option<String> {
    match agent {
        AgentKey::Chat(id) => Some(id.as_str().to_string()),
        AgentKey::Mesh(_) => None,
    }
}

fn hop(hop: Option<u32>) -> String {
    hop.map_or(String::new(), |h| format!(", hop {h}"))
}

/// A plug or receptacle name from the log, shown as a sender: `peer-<marv>` is that Marv; anything else, cleaned.
fn sender(text: &str) -> String {
    clean_name(text.strip_prefix("peer-").unwrap_or(text))
}

/// A chat named by its id as some events carry it (a string): its name and the id a click opens, else the text
/// cleaned.
fn chat_name(chat: &str, names: &Names) -> (String, Option<String>) {
    match crate::AgentId::new(chat).filter(|a| a.as_str() == chat) {
        Some(id) => (names.chat(&id), Some(chat.to_string())),
        None => (clean_name(chat), None),
    }
}

/// One event's sentence, or `None` for the kinds the timeline leaves out (every busy/idle change, a capability's
/// bookkeeping). `GrantUsed` lines are merged by [`timeline`].
pub fn sentence(at_ms: u64, event: &KernelEvent, names: &Names) -> Option<Line> {
    let line = |kind: &str, chat: Option<String>, agents: Vec<String>, text: String| {
        Some(Line { at_ms, kind: kind.into(), chat, agents, text })
    };
    match event {
        KernelEvent::Registered { agent, kind, .. } => {
            let n = names.of(agent);
            line("registered", chat_of(agent), vec![n.clone()], format!("{n} came on the mesh ({})", word(kind)))
        }
        KernelEvent::Unregistered { agent } => {
            let n = names.of(agent);
            line("registered", chat_of(agent), vec![n.clone()], format!("{n} left the mesh"))
        }
        KernelEvent::State { agent, to, .. } => {
            let n = names.of(agent);
            let text = match to {
                AgentState::Waiting => format!("{n} needs you"),
                AgentState::Sleeping => format!("{n} went to sleep"),
                _ => return None,
            };
            line("state", chat_of(agent), vec![n], text)
        }
        KernelEvent::Message { agent, from, from_agent, to, hop: h, outcome, refusal, .. } => {
            let me = names.of(agent);
            let from = from_agent.as_ref().map_or_else(|| sender(from), |a| names.of(a));
            let to_name = sender(to.split('.').nth(1).unwrap_or(to));
            let text = match outcome {
                Delivery::Delivered => format!("{from} sent {me} a message{}", hop(*h)),
                Delivery::Refused => {
                    let why = refusal.map_or("refused".into(), |r| word(&r));
                    format!("a message from {from} to {to_name} was refused: {why}")
                }
            };
            line("message", chat_of(agent), vec![me, from], text)
        }
        KernelEvent::WireRefused { agent, from, from_agent, reason, .. } => {
            let me = names.of(agent);
            let from = from_agent.as_ref().map_or_else(|| sender(from), |a| names.of(a));
            line(
                "message",
                chat_of(agent),
                vec![me.clone(), from.clone()],
                format!("{from} asked to wire to {me}: refused ({})", word(reason)),
            )
        }
        KernelEvent::Wake { agent, by, hop: h } => {
            let me = names.of(agent);
            let by_name = sender(by);
            line("wake", chat_of(agent), vec![me.clone(), by_name.clone()], format!("{me} was woken by {by_name}{}", hop(*h)))
        }
        KernelEvent::WakeSkipped { agent, by, reason, count, .. } => {
            let me = names.of(agent);
            let by_name = sender(by);
            let s = if *count == 1 { "" } else { "s" };
            line(
                "wake",
                chat_of(agent),
                vec![me.clone(), by_name.clone()],
                format!("{count} message{s} from {by_name} didn't wake {me} ({})", word(reason)),
            )
        }
        KernelEvent::Policy { id, change, rule } => {
            let text = match change {
                PolicyChange::Created => {
                    let mode = rule.as_ref().map_or_else(String::new, |r| format!(" ({})", r.mode));
                    format!("you made {id}{mode}")
                }
                PolicyChange::Revoked => format!("you revoked {id}"),
                PolicyChange::Expired => format!("{id} expired"),
            };
            line("rule", None, vec![], text)
        }
        KernelEvent::Capability { .. } => None,
        KernelEvent::GrantMade { grant } => {
            let n = names.chat(&grant.chat);
            line(
                "grant",
                Some(grant.chat.as_str().into()),
                vec![n.clone()],
                format!("you let {n} work while you're away ({})", grant.id),
            )
        }
        KernelEvent::GrantRevoked { id } => line("grant", None, vec![], format!("you revoked {id}")),
        KernelEvent::GrantExpired { id } => line("grant", None, vec![], format!("{id} expired")),
        KernelEvent::GrantPaused { id, .. } => line("grant", None, vec![], format!("{id} paused: today's spend cap")),
        KernelEvent::GrantUsed { id, chat, .. } => {
            let n = names.chat(chat);
            line("grant", Some(chat.as_str().into()), vec![n.clone()], format!("{id} on {n} ran 1 call while you were away"))
        }
        KernelEvent::Read { reader, target, outcome, reason, .. } => {
            let (r, t) = (names.of(reader), names.of(target));
            let text = match outcome {
                ReadOutcome::Served => format!("{r} read {t}'s chat"),
                ReadOutcome::Refused => {
                    format!("{r} couldn't read {t}'s chat: {}", reason.map_or("refused".into(), |w| word(&w)))
                }
            };
            line("read", chat_of(target), vec![r, t], text)
        }
        KernelEvent::Interrupt { chat, source, kind, action } => {
            let n = names.of(chat);
            line(
                "interrupt",
                chat_of(chat),
                vec![n.clone()],
                format!("{n}: {} from {} ({})", word(kind), word(source), word(action)),
            )
        }
        KernelEvent::Signal { to, kind, .. } => {
            let n = names.of(to);
            let what = match kind {
                crate::SignalKind::Stop => "stopped",
                crate::SignalKind::Pause => "paused",
                crate::SignalKind::Resume => "resumed",
                crate::SignalKind::Close => "closed",
            };
            line("signal", chat_of(to), vec![n.clone()], format!("you {what} {n}"))
        }
        KernelEvent::TimerMade { id, chat, by, recurring, .. } => {
            let n = names.chat(chat);
            let who = match by {
                crate::TimerBy::Person => "you set".to_string(),
                crate::TimerBy::Chat => format!("{n} set"),
            };
            let every = if *recurring { " (repeating)" } else { "" };
            line("timer", Some(chat.as_str().into()), vec![n.clone()], format!("{who} {id} for {n}{every}"))
        }
        KernelEvent::TimerFired { id, chat, late } => {
            let n = names.chat(chat);
            let late = if *late { " (late: missed while Marv was closed)" } else { "" };
            line("timer", Some(chat.as_str().into()), vec![n.clone()], format!("{n} was woken by {id}{late}"))
        }
        KernelEvent::TimerVoid { id, chat, reason } => {
            let n = names.chat(chat);
            line("timer", Some(chat.as_str().into()), vec![n.clone()], format!("{id} for {n} didn't run: {}", word(reason)))
        }
        // The release merge's other kernels (memory §11.4, allocator §16, write sets §14.4, board §19): fixed words and
        // numbers, never a summary, a path, a board entry's text or a turn's words.
        KernelEvent::Compacted { chat, by, swap_turns, .. } => {
            let n = names.of(chat);
            let saved = if *swap_turns > 0 { format!(", {swap_turns} turns saved to a page") } else { String::new() };
            line("compacted", chat_of(chat), vec![n.clone()], format!("{n}'s conversation was compacted ({}){saved}", word(by)))
        }
        // One per turn: bookkeeping, as busy/idle changes are.
        KernelEvent::Usage { .. } => None,
        KernelEvent::Booted { agent, core, resumed, .. } => {
            let n = names.of(agent);
            let how = if *resumed { "resumed" } else { "started" };
            line("boot", chat_of(agent), vec![n.clone()], format!("{n} {how} a conversation on {}", clean_name(core)))
        }
        KernelEvent::CoreSwitched { chat, to, paged_out, .. } => {
            let (n, id) = chat_name(chat, names);
            let paged = if *paged_out { " (paged out first to fit)" } else { "" };
            line("core", id, vec![n.clone()], format!("you switched {n} to {}{paged}", clean_name(to)))
        }
        KernelEvent::BudgetReached { chat, unattended, .. } => {
            let (n, id) = chat_name(chat, names);
            let what = if *unattended { "it paused" } else { "you were asked" };
            line("budget", id, vec![n.clone()], format!("{n} reached a budget's limit: {what}"))
        }
        KernelEvent::BudgetSet { cap, .. } => {
            line("budget", None, vec![], if cap.is_some() { "you set a budget".into() } else { "you removed a budget".into() })
        }
        KernelEvent::WriteOverlap { a, b, paths_count } => {
            let (an, bn) = (names.of(a), names.of(b));
            let s = if *paths_count == 1 { "" } else { "s" };
            line(
                "writes",
                chat_of(a),
                vec![an.clone(), bn.clone()],
                format!("{an} and {bn} edited the same {paths_count} file{s}"),
            )
        }
        KernelEvent::BoardPost { rule, by, kind, .. } => {
            let who = by.as_ref().map_or_else(|| "you".to_string(), |a| names.of(a));
            let agents = by.as_ref().map(|a| vec![names.of(a)]).unwrap_or_default();
            line("board", by.as_ref().and_then(chat_of), agents, format!("{who} posted a {} on {rule}'s board", word(kind)))
        }
        KernelEvent::BoardCleared { rule, by, count } => {
            let who = by.as_ref().map_or_else(|| "you".to_string(), |a| names.of(a));
            let agents = by.as_ref().map(|a| vec![names.of(a)]).unwrap_or_default();
            let s = if *count == 1 { "y" } else { "ies" };
            line("board", by.as_ref().and_then(chat_of), agents, format!("{who} cleared {count} entr{s} from {rule}'s board"))
        }
    }
}

/// The timeline from a log, oldest first: one fixed sentence per event, a grant's calls in one turn merged into one
/// line ("grant 2 on acme-api ran 12 calls while you were away").
pub fn timeline(events: &[(u64, KernelEvent)], names: &Names) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    let mut run: Option<(crate::GrantId, u32, usize, u64)> = None;
    for (at, event) in events {
        if let KernelEvent::GrantUsed { id, chat, turn, .. } = event {
            match &mut run {
                Some((rid, rturn, idx, count)) if rid == id && rturn == turn => {
                    *count += 1;
                    let n = names.chat(chat);
                    out[*idx].text = format!("{id} on {n} ran {count} calls while you were away");
                    out[*idx].at_ms = *at;
                }
                _ => {
                    if let Some(line) = sentence(*at, event, names) {
                        out.push(line);
                        run = Some((*id, *turn, out.len() - 1, 1));
                    }
                }
            }
            continue;
        }
        if let Some(line) = sentence(*at, event, names) {
            out.push(line);
        }
    }
    out
}

/// What happened between two times, from counts alone (§20.3's "while you were away").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Away {
    pub from_ms: u64,
    pub to_ms: u64,
    /// Chats that ran: woken, or ran calls under a grant, or a timer fired.
    pub chats: usize,
    /// Calls run under a grant.
    pub calls: u64,
    pub wakes: u64,
    /// Agents whose last state in the interval is waiting for the person.
    pub needs_you: usize,
    /// Grants that paused at their cap.
    pub paused: u64,
    /// The spend over the interval, from the host (the log has none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spent_usd_micros: Option<u64>,
}

/// The person must be gone this long for the card.
pub const AWAY_AFTER_MS: u64 = 10 * 60_000;

/// [`Away`] for `[from_ms, to_ms)`.
pub fn away(events: &[(u64, KernelEvent)], from_ms: u64, to_ms: u64) -> Away {
    let mut ran: BTreeSet<String> = BTreeSet::new();
    let mut last_state: BTreeMap<AgentKey, AgentState> = BTreeMap::new();
    let mut card = Away { from_ms, to_ms, ..Away::default() };
    for (at, event) in events.iter().filter(|(at, _)| (from_ms..to_ms).contains(at)) {
        let _ = at;
        match event {
            KernelEvent::Wake { agent, .. } => {
                card.wakes += 1;
                ran.insert(format!("{agent}"));
            }
            KernelEvent::GrantUsed { chat, .. } => {
                card.calls += 1;
                ran.insert(format!("{}", AgentKey::Chat(chat.clone())));
            }
            KernelEvent::TimerFired { chat, .. } => {
                ran.insert(format!("{}", AgentKey::Chat(chat.clone())));
            }
            KernelEvent::GrantPaused { .. } => card.paused += 1,
            KernelEvent::State { agent, to, .. } => {
                last_state.insert(agent.clone(), *to);
            }
            _ => {}
        }
    }
    card.chats = ran.len();
    card.needs_you = last_state.values().filter(|s| **s == AgentState::Waiting).count();
    card
}

impl Away {
    /// Whether anything happened worth a card.
    pub fn is_empty(&self) -> bool {
        self.chats == 0 && self.calls == 0 && self.needs_you == 0 && self.paused == 0
    }

    /// The card's fixed words: "3 chats ran 41 calls · 1 needs you · $0.62 spent".
    pub fn text(&self) -> String {
        let plural = |n: u64, one: &str, many: &str| if n == 1 { one.to_string() } else { many.to_string() };
        let mut parts = vec![format!(
            "{} {} ran {} {}",
            self.chats,
            plural(self.chats as u64, "chat", "chats"),
            self.calls,
            plural(self.calls, "call", "calls")
        )];
        if self.needs_you > 0 {
            parts.push(format!("{} {} you", self.needs_you, plural(self.needs_you as u64, "needs", "need")));
        }
        if self.paused > 0 {
            parts.push(format!("{} paused at {} cap", self.paused, plural(self.paused, "its", "their")));
        }
        if let Some(spent) = self.spent_usd_micros {
            parts.push(format!("${:.2} spent", spent as f64 / 1e6));
        }
        parts.join(" · ")
    }
}

/// What a notification was about (§10.3's three kinds).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteKind {
    NeedsYou,
    Finished,
    Paused,
}

/// A notification the host showed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub at_ms: u64,
    pub kind: NoteKind,
    pub chat: String,
    /// The chat's name as the notification said it.
    pub name: String,
}

/// The most notifications kept for the app's run.
pub const MAX_NOTES: usize = 200;

/// Notification history (§20.5): in memory for the app's run, never on disk, newest last, at most [`MAX_NOTES`].
#[derive(Debug, Default)]
pub struct Notifications {
    notes: std::sync::Mutex<VecDeque<Note>>,
}

impl Notifications {
    pub fn push(&self, note: Note) {
        let mut notes = self.notes.lock().unwrap_or_else(|e| e.into_inner());
        notes.push_back(Note { name: clean_name(&note.name), ..note });
        while notes.len() > MAX_NOTES {
            notes.pop_front();
        }
    }

    /// Newest first.
    pub fn list(&self) -> Vec<Note> {
        self.notes.lock().unwrap_or_else(|e| e.into_inner()).iter().rev().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventlog::{Refusal, WakeSkipReason};
    use crate::{AgentId, AgentKind, GrantId, Mode, PolicyId};

    const API: &str = "554963ee-fd8e-4bec-a7dc-49051f090acc";
    const WEB: &str = "11111111-fd8e-4bec-a7dc-49051f090acc";

    fn chat(id: &str) -> AgentKey {
        AgentKey::Chat(AgentId::new(id).unwrap())
    }

    fn log() -> Vec<(u64, KernelEvent)> {
        vec![
            (
                1,
                KernelEvent::Registered {
                    agent: chat(API),
                    mesh_name: "acme-api".into(),
                    folder: "/p/api".into(),
                    kind: AgentKind::Chat,
                },
            ),
            (
                2,
                KernelEvent::Registered {
                    agent: chat(WEB),
                    mesh_name: "acme-web".into(),
                    folder: "/p/web".into(),
                    kind: AgentKind::Chat,
                },
            ),
            (3, KernelEvent::Wake { agent: chat(WEB), by: "peer-acme-api".into(), hop: Some(1) }),
            (4, KernelEvent::grant_used(GrantId(2), AgentId::new(API).unwrap(), "write_file", "write:src", 4)),
            (5, KernelEvent::grant_used(GrantId(2), AgentId::new(API).unwrap(), "bash", "bash:cargo test", 4)),
            (6, KernelEvent::grant_used(GrantId(2), AgentId::new(API).unwrap(), "bash", "bash:ls", 4)),
            (7, KernelEvent::GrantPaused { id: GrantId(2), reason: crate::GrantPauseReason::Spend }),
            (
                8,
                KernelEvent::Message {
                    agent: chat(WEB),
                    id: "m1".into(),
                    from: "peer-infra".into(),
                    from_agent: None,
                    to: "agent.parent.inbox".into(),
                    mode: Mode::Send,
                    hop: None,
                    size: 512,
                    outcome: Delivery::Refused,
                    refusal: Some(Refusal::NotWired),
                },
            ),
            (9, KernelEvent::State { agent: chat(WEB), from: Some(AgentState::Busy), to: AgentState::Waiting }),
            (10, KernelEvent::State { agent: chat(API), from: Some(AgentState::Idle), to: AgentState::Busy }),
            (
                11,
                KernelEvent::WakeSkipped {
                    agent: chat(API),
                    by: "peer-acme-web".into(),
                    hop: Some(2),
                    reason: WakeSkipReason::Hop,
                    count: 2,
                },
            ),
            (12, KernelEvent::Policy { id: PolicyId(3), change: PolicyChange::Revoked, rule: None }),
        ]
    }

    #[test]
    fn the_timeline_is_fixed_sentences() {
        let events = log();
        let names = Names::from_log(&events, &[]);
        let lines: Vec<String> = timeline(&events, &names).into_iter().map(|l| l.text).collect();
        assert_eq!(
            lines,
            [
                "acme-api came on the mesh (chat)",
                "acme-web came on the mesh (chat)",
                "acme-web was woken by acme-api, hop 1",
                "grant 2 on acme-api ran 3 calls while you were away",
                "grant 2 paused: today's spend cap",
                "a message from infra to parent was refused: not wired",
                "acme-web needs you",
                "2 messages from acme-web didn't wake acme-api (hop)",
                "you revoked rule 3",
            ]
        );
        let wake = &timeline(&events, &names)[2];
        assert_eq!((wake.kind.as_str(), wake.chat.as_deref()), ("wake", Some(WEB)));
    }

    /// The release merge's kernels in the timeline: fixed words and numbers only (a model name cleaned), and a turn's
    /// usage left out as bookkeeping.
    #[test]
    fn the_other_kernels_events_are_fixed_sentences() {
        let mut events = log();
        events.extend([
            (
                20,
                KernelEvent::Compacted {
                    chat: chat(API),
                    by: crate::CompactedBy::Overflow,
                    level: Some(1),
                    tokens_before: Some(42_000),
                    tokens_after: 9_000,
                    cost_usd_micros: None,
                    page: Some(1),
                    swap_turns: 12,
                },
            ),
            (
                21,
                KernelEvent::CoreSwitched { chat: API.into(), from: "a/b".into(), to: "zai/glm\x1b[2J".into(), paged_out: true },
            ),
            (22, KernelEvent::BudgetReached { chat: WEB.into(), scope: None, unattended: true }),
            (23, KernelEvent::WriteOverlap { a: chat(API), b: chat(WEB), paths_count: 2 }),
            (
                24,
                KernelEvent::BoardPost {
                    rule: PolicyId(3),
                    by: Some(chat(WEB)),
                    kind: crate::BoardKind::Note,
                    size: 9,
                    paths_count: 0,
                },
            ),
            (25, KernelEvent::BoardCleared { rule: PolicyId(3), by: None, count: 1 }),
        ]);
        let names = Names::from_log(&events, &[]);
        let lines: Vec<String> = timeline(&events[12..], &names).into_iter().map(|l| l.text).collect();
        assert_eq!(lines[0], "acme-api's conversation was compacted (overflow), 12 turns saved to a page");
        assert!(lines[1].starts_with("you switched acme-api to zai/glm") && !lines[1].contains('\x1b'), "{}", lines[1]);
        assert!(lines[1].ends_with(" (paged out first to fit)"), "{}", lines[1]);
        assert_eq!(
            lines[2..],
            [
                "acme-web reached a budget's limit: it paused",
                "acme-api and acme-web edited the same 2 files",
                "acme-web posted a note on rule 3's board",
                "you cleared 1 entry from rule 3's board",
            ]
        );
    }

    #[test]
    fn names_from_hints_and_hostile_names_are_cleaned() {
        let events = vec![(1, KernelEvent::Wake { agent: chat(API), by: "peer-\x1b]52;c;x\x07evil".into(), hop: None })];
        let names = Names::from_log(&events, &[(API.into(), "acme-api".into())]);
        let line = &timeline(&events, &names)[0];
        assert!(line.text.starts_with("acme-api was woken by ") && !line.text.contains('\x1b'), "{}", line.text);
        assert_eq!(Names::default().of(&chat(API)), "chat 554963ee");
    }

    #[test]
    fn away_sums_the_interval_from_counts() {
        let events = log();
        let card = away(&events, 3, 12);
        assert_eq!((card.chats, card.calls, card.wakes, card.needs_you, card.paused), (2, 3, 1, 1, 1));
        let card = Away { spent_usd_micros: Some(620_000), ..card };
        assert_eq!(card.text(), "2 chats ran 3 calls · 1 needs you · 1 paused at its cap · $0.62 spent");
        assert!(away(&events, 100, 200).is_empty());
    }

    #[test]
    fn notifications_are_kept_newest_first_and_capped() {
        let n = Notifications::default();
        for i in 0..(MAX_NOTES as u64 + 5) {
            n.push(Note { at_ms: i, kind: NoteKind::Finished, chat: API.into(), name: "acme\napi".into() });
        }
        let list = n.list();
        assert_eq!((list.len(), list[0].at_ms, list[0].name.as_str()), (MAX_NOTES, MAX_NOTES as u64 + 4, "acme api"));
    }
}
