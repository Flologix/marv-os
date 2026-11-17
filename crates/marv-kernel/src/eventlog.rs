//! What happened in the kernel, in order (runtime-kernel phase 3, `docs/architecture/runtime-kernel.md` §5.4, D10).
//! New in marv-rs.
//!
//! A [`KernelEvent`] is one change the person's own Marvs (the desktop, the TUI; `marv mesh allow`/`revoke`) made or
//! saw: an agent entered or left the registry, a scheduler state changed, a message was admitted or refused, a message
//! woke an agent, a rule was made, revoked or found expired, a rule issued a wire. Each is appended through an
//! [`EventLog`] (marv-session's writer: `~/.marv/events/<yyyy-mm-dd>.jsonl`, 0600, never breaking a run), stamped with
//! its time ([`LoggedEvent`]), and [`replay`] rebuilds the [`KernelState`] (registry, rules, states) from them.
//!
//! **Never a message's words.** A [`KernelEvent::Message`] holds its id, its sender (the plug, and the agent when the
//! switchboard knows it), its receptacle, mode, hop, size and whether it was delivered: no body, no text, and not the
//! refusal's reason (Marv's own sentence, kept in the session's trajectory), only its category ([`Refusal`], a fixed
//! word; runtime-kernel live UX). A wire asked for and not made is a [`KernelEvent::WireRefused`] with a fixed reason,
//! and a message that didn't wake its idle reader a [`KernelEvent::WakeSkipped`]. The types hold no field a body could
//! go into. A rule's [`Policy`] is logged whole when it is made: the person's folders, chat ids and label, never a
//! model's words. A `chat_read` (runtime-kernel §9.2) is a [`KernelEvent::Read`]: who read whom, what was asked,
//! the reply's size and whether it was served or refused (with a fixed [`ReadRefusal`]): never the text served.
//!
//! A team board post (runtime-kernel §19.6) is a [`KernelEvent::BoardPost`]: the rule, who, the kind, the text's size
//! and how many paths; a clear is a [`KernelEvent::BoardCleared`] with a count. Never the text, never a path.
//!
//! Service grants are logged as they're made (whole, as stored), revoked, found expired and paused, and each call
//! one ran while the person was away as [`KernelEvent::GrantUsed`]: the tool's name and the call's audit key (Marv's
//! coarse words, `bash:net` and the like; never the command, path or anything else the model wrote).
//!
//! A page-out (runtime-kernel §11.4) is a [`KernelEvent::Compacted`]: which chat, what started it ([`CompactedBy`], a
//! fixed word), the overflow recovery's level, the context's size before and after, what the summary cost, and how
//! many turns went to the chat's saved page (its number): numbers only. The type has no field a summary, a turn or a
//! path could go into, so `marv kernel replay` can count page-outs without the log ever holding a word of a chat.
//!
//! Pure: no I/O, no clock (time is passed in).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::board::BoardKind;
use crate::budget::BudgetScope;
use crate::grant::{Grant, GrantId};
use crate::identity::{AgentId, AgentKind};
use crate::interrupt::{InterruptAction, Kind, SignalBy, SignalKind, Source};
use crate::meter::TurnUsage;
use crate::mode::Mode;
use crate::policy::{Policy, PolicyId, PolicySnapshot};
use crate::registry::{AgentKey, Entry, Registry};
use crate::scheduler::AgentState;
use crate::timer::{TimerBy, TimerId, TimerVoidReason};

/// The format version of a logged line.
pub const EVENT_VERSION: u32 = 1;

/// What became of a message. Never the refusal's reason (see the module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    Delivered,
    Refused,
}

/// Why a message was refused, as a category (runtime-kernel live UX): a fixed word, never the refusal's sentence
/// (Marv's, kept in the session's trajectory) nor a word of the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    /// No wire: no rule issued one and nobody granted one (a send no rule covers).
    NotWired,
    /// The rule that issued its wire was revoked or has expired.
    RuleEnded,
    /// Over the wire's size or rate limit.
    Limit,
    /// The receiver's inbox was full.
    InboxFull,
    /// No such receptacle (any more), or not in that mode.
    NoReceptacle,
    /// Anything else (a bad body, the other Marv gone).
    Other,
}

/// Why a wire asked for wasn't made (runtime-kernel live UX). No rule covered it (a rule would have issued it), so a
/// person was asked, or the other Marv was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireRefusal {
    /// This Marv's person said no (or couldn't be asked).
    Declined,
    /// The other Marv refused it (its person, or its door), or couldn't be reached.
    FarEnd,
    /// This Marv asked its person and nobody answered before the asking Marv's wait ran out (the card withdrawn).
    Unanswered,
}

/// What happened to a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyChange {
    /// The person made it (the desktop's Mesh panel, `marv mesh allow`): the event carries the rule as stored.
    Created,
    /// The person revoked it.
    Revoked,
    /// A Marv noticed it had expired.
    Expired,
}

/// Why a message that came in while its reader was idle didn't wake it (runtime-kernel live UX). A fixed word, never
/// the message's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeSkipReason {
    /// `mesh.wake` doesn't let its wire wake (off, or a wire the person approved by hand while only rules' wake).
    Off,
    /// A rule's wire at the hop limit ([`crate::RULE_HOP_LIMIT`]) or more: a person continues the chain.
    Hop,
    /// Its wire woke the reader as often as it may this minute.
    Limit,
    /// The rule wakes are paused until the person types.
    Paused,
    /// The sending turn, one its person started, had used up its rounds (marv-session's `MAX_LEAD_ROUNDS`).
    Rounds,
    /// A woken turn's automatic report back to the chat that sent it work (runtime-kernel coordination): a report never
    /// wakes; it's read at its reader's next step (`mesh_wait`) or turn.
    Report,
    /// A team's wire over its chat's wake budget (marv-session's `TEAM_WAKES_PER_HOUR`): the team's mail waits until
    /// the person writes in that chat or the hour's budget refills.
    Budget,
}

/// Why a grant paused (a fixed word).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantPauseReason {
    /// It reached today's spend cap.
    Spend,
}

/// Longest tool name or scope key a [`KernelEvent::GrantUsed`] keeps; the rest is cut.
pub const GRANT_USED_MAX: usize = 120;

/// What a `chat_read` asked for (runtime-kernel §9.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadWhat {
    /// The chat's card, its compaction summary if it has one, and its last 3 person and assistant turns.
    Summary,
    /// Its last `n` person and assistant turns.
    Tail,
}

/// What became of a `chat_read` (runtime-kernel §9.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadOutcome {
    Served,
    Refused,
}

/// Why a `chat_read` was refused (runtime-kernel §9.2): a fixed word, never a word of either chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadRefusal {
    /// No live `read` rule the person made covers the reader reading the target.
    NoRule,
    /// The person marked the target chat private.
    Private,
    /// The target was mid-turn (or mid-approval).
    Busy,
    /// The reader's person said no.
    Declined,
    /// Not even the newest turn fits the reply's size cap.
    TooLarge,
    /// The reader read as often as it may this minute.
    Limit,
    /// The target couldn't be reached, didn't answer as itself, or can't be read now (a terminal Marv that isn't
    /// running, a chat with no conversation).
    Unavailable,
}

/// What started a page-out (runtime-kernel §11.4): a fixed word, never the person's `/compact` focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactedBy {
    /// The context reached the chat's context budget.
    Budget,
    /// The provider said the request didn't fit (overflow recovery).
    Overflow,
    /// The person asked (`/compact`).
    Person,
}

/// One kernel event. Every one names the agent it is about, or (a rule, a grant) the rule or grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum KernelEvent {
    /// An agent entered the registry as its Marv's own (a desktop chat with a door, a TUI).
    Registered { agent: AgentKey, mesh_name: String, folder: PathBuf, kind: AgentKind },
    /// It left (stopped, deleted, gone off the mesh).
    Unregistered { agent: AgentKey },
    /// Its scheduler state changed (`from`: none when it had none).
    State { agent: AgentKey, from: Option<AgentState>, to: AgentState },
    /// A message `agent`'s switchboard admitted or refused (each end of a cross-process message logs its own). `from`
    /// is the sending plug (`parent`, `worker-N`, `peer-<marv>`, a shim's), `from_agent` who that is when the
    /// switchboard knows it (its own main agent, a pid-confirmed peer), `to` the receptacle. Never the body.
    Message {
        agent: AgentKey,
        id: String,
        from: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_agent: Option<AgentKey>,
        to: String,
        mode: Mode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hop: Option<u32>,
        size: usize,
        outcome: Delivery,
        /// A refused message's category (runtime-kernel live UX); none when delivered, and in older logs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refusal: Option<Refusal>,
    },
    /// A wire `agent`'s switchboard was asked for and didn't make (runtime-kernel live UX): `from` the plug that would
    /// have used it (and who that is, when known), `to` the receptacle, and why. Never a word of a message.
    WireRefused {
        agent: AgentKey,
        from: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_agent: Option<AgentKey>,
        to: String,
        mode: Mode,
        reason: WireRefusal,
    },
    /// A message woke `agent`: `by` the plug it came from, at its hop.
    Wake {
        agent: AgentKey,
        by: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hop: Option<u32>,
    },
    /// Messages that came in while `agent` was idle and didn't wake it (runtime-kernel live UX): `count` of them, `by`
    /// the plug they came from, the first one's hop, and why. Logged once per message, when the person is told it
    /// waits. Never the body.
    WakeSkipped {
        agent: AgentKey,
        by: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hop: Option<u32>,
        reason: WakeSkipReason,
        count: usize,
    },
    /// A rule was made (`rule`: as stored), revoked or found expired.
    Policy {
        id: PolicyId,
        change: PolicyChange,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rule: Option<Policy>,
    },
    /// A rule wired (`issued`) `agent`'s plug `from` to `to`, or a rule's wire was refused because the rule ended.
    Capability { agent: AgentKey, issued: bool, from: String, to: String, rule: PolicyId, generation: u64 },
    /// The person made a service grant, or changed its cap (as stored: the chat, its folder, expiry, cap and label;
    /// never a model's words).
    GrantMade { grant: Grant },
    /// The person revoked it (or a newer grant for the chat replaced it).
    GrantRevoked { id: GrantId },
    /// A Marv noticed it had expired.
    GrantExpired { id: GrantId },
    /// It paused (today's spend cap).
    GrantPaused { id: GrantId, reason: GrantPauseReason },
    /// A call it ran while the person was away: the tool's name and its audit key in `scope_key` (`marv_tools`'
    /// `Tool::audit_key`: the tool's name, or `bash:` and its flags), never the input.
    GrantUsed { id: GrantId, chat: AgentId, tool: String, scope_key: String, turn: u32 },
    /// A chat booted (runtime-kernel §18.1, B1): its first request of a conversation (a new chat, `/clear`, a resume, a
    /// `/model`), once the system prompt and the tool specs it sends are fixed. `shell`: what runs it; `core`: the
    /// provider and model; `prompt_hash`/`tools_hash`: SHA-256 of the system prompt's bytes and of the tool specs in
    /// order (two boots with the same hashes have the same cacheable prefix); `devices`: its device set (§17.1), by
    /// name. Hashes and names only: never the prompt, a spec's words or a message.
    Booted {
        agent: AgentKey,
        shell: AgentKind,
        core: String,
        prompt_hash: String,
        tools_hash: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        devices: Vec<String>,
        resumed: bool,
    },
    /// A `chat_read` (runtime-kernel §9.2): `reader` read (or was refused) `target`'s chat. `n`: the turns a tail
    /// asked for; `size`: the reply's bytes (0 when refused). Logged once, by the end that decided it (the target's
    /// host for what it served or refused, the reader's for a read it refused before asking). Never the text.
    Read {
        reader: AgentKey,
        target: AgentKey,
        what: ReadWhat,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        n: Option<u32>,
        size: usize,
        outcome: ReadOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<ReadRefusal>,
    },
    /// A page-out (runtime-kernel §11.4): `chat`'s conversation was compacted. `page`: the saved page's number, when
    /// one was written; `swap_turns`: the turns it holds; `level`: an overflow recovery's level; `tokens_before`: the
    /// context the last request used (none before any request), `tokens_after`: the compacted history's estimate;
    /// `cost_usd_micros`: what the summary cost, when known. Numbers and a fixed word only: never the summary or a
    /// turn's text.
    Compacted {
        chat: AgentKey,
        by: CompactedBy,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        level: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tokens_before: Option<u64>,
        tokens_after: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost_usd_micros: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        page: Option<u32>,
        swap_turns: u64,
    },
    /// One turn's use (runtime-kernel §16.5): numbers and ids only ([`TurnUsage`]), never a word of the turn.
    Usage { usage: TurnUsage },
    /// The person switched a chat's core (`/model`, the desktop's picker; runtime-kernel §16.8): `from` and `to` as
    /// `provider/model`, and whether the conversation was paged out first to fit the new core's window.
    CoreSwitched {
        chat: String,
        from: String,
        to: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        paged_out: bool,
    },
    /// A chat's turn met a budget at its limit (runtime-kernel §16.7): `scope` none when the budgets file couldn't be
    /// read; `unattended` when nobody was there to ask (it paused), else the person was asked.
    BudgetReached {
        chat: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<BudgetScope>,
        unattended: bool,
    },
    /// The person set (`cap`) or removed a budget (runtime-kernel §16.6).
    BudgetSet {
        scope: BudgetScope,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cap: Option<crate::grant::SpendCap>,
    },
    /// Runtime-kernel §14.4: `a` wrote a path `b` also wrote in the last 30 minutes ([`crate::writes`]); they now
    /// share `paths_count` paths. Logged by `a`'s Marv when the count grows. Who and how many: never a path, never a
    /// file's content.
    WriteOverlap { a: AgentKey, b: AgentKey, paths_count: usize },
    /// Runtime-kernel §19.6 (T7): an entry went on `rule`'s team board. `by` the chat that posted it (none: the person,
    /// from the desktop), its kind, `size` its text's bytes and `paths_count` the paths it named. Never the text,
    /// never a path.
    BoardPost {
        rule: PolicyId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by: Option<AgentKey>,
        kind: BoardKind,
        size: usize,
        paths_count: usize,
    },
    /// `count` entries were cleared from `rule`'s board: `by` the chat that cleared its own (none: the person).
    BoardCleared {
        rule: PolicyId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by: Option<AgentKey>,
        count: usize,
    },
    /// An interrupt reached `chat` (runtime-kernel §15.5): where from, what, and what it did. Fixed words only.
    Interrupt { chat: AgentKey, source: Source, kind: Kind, action: InterruptAction },
    /// The person signalled `to` (§15.3): always the person ([`SignalBy`]).
    Signal { to: AgentKey, kind: SignalKind, by: SignalBy },
    /// A timer was made (§15.4): by whom, for which chat, whether it recurs and when it first fires. Never its note.
    TimerMade { id: TimerId, chat: AgentId, by: TimerBy, recurring: bool, fires_at_ms: u64 },
    /// A timer fired; `late`: it was missed (Marv closed, the chat busy or paused) and ran once now.
    TimerFired { id: TimerId, chat: AgentId, late: bool },
    /// A timer couldn't run (its grant ended, the spend cap, its chat gone) or the person removed it.
    TimerVoid { id: TimerId, chat: AgentId, reason: TimerVoidReason },
}

impl KernelEvent {
    /// [`KernelEvent::GrantUsed`], its tool name and scope key cut to [`GRANT_USED_MAX`] characters.
    pub fn grant_used(id: GrantId, chat: AgentId, tool: &str, scope_key: &str, turn: u32) -> Self {
        let cut = |s: &str| s.chars().take(GRANT_USED_MAX).collect::<String>();
        KernelEvent::GrantUsed { id, chat, tool: cut(tool), scope_key: cut(scope_key), turn }
    }
}

/// One logged line: the format version, the time, and the event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoggedEvent {
    pub v: u32,
    pub at_ms: u64,
    #[serde(flatten)]
    pub event: KernelEvent,
}

impl LoggedEvent {
    pub fn new(at_ms: u64, event: KernelEvent) -> Self {
        Self { v: EVENT_VERSION, at_ms, event }
    }

    /// One JSON line, without the newline.
    pub fn line(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Where kernel events go. Appending never fails the caller and never blocks it on I/O: a writer that can't keep up
/// or can't write drops events (and says so once).
pub trait EventLog: Send + Sync {
    fn append(&self, at_ms: u64, event: KernelEvent);
}

/// A log that keeps nothing.
pub struct NoEvents;
impl EventLog for NoEvents {
    fn append(&self, _at_ms: u64, _event: KernelEvent) {}
}

/// A rule as the log knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedRule {
    pub rule: Policy,
    /// A Marv noticed it had expired.
    pub expired: bool,
}

/// How many of each event the log held (for the record; not part of the state compared).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Totals {
    pub delivered: u64,
    pub refused: u64,
    /// Of those, refused for having no wire (runtime-kernel live UX).
    pub refused_not_wired: u64,
    /// Wires asked for and not made (runtime-kernel live UX).
    pub wires_refused: u64,
    pub wakes: u64,
    /// Messages that didn't wake an idle reader (runtime-kernel live UX).
    pub wakes_skipped: u64,
    pub issued: u64,
    pub ended: u64,
    /// Calls service grants ran while the person was away.
    pub granted_calls: u64,
    /// Times a grant paused at its spend cap.
    pub grants_paused: u64,
    /// `chat_read`s served and refused (runtime-kernel §9.2).
    pub reads_served: u64,
    pub reads_refused: u64,
    /// Page-outs (runtime-kernel §11.4), of them those an overflow forced, and the turns they saved to pages.
    pub compactions: u64,
    pub compactions_overflow: u64,
    pub swap_turns: u64,
    /// Turns metered (runtime-kernel §16.5), core switches (§16.8), budgets met at their limit (§16.7).
    pub usage_turns: u64,
    pub core_switches: u64,
    pub budget_limits: u64,
    /// Write overlaps logged (runtime-kernel §14.4).
    pub write_overlaps: u64,
    /// Team board posts, and entries cleared (runtime-kernel §19).
    pub board_posts: u64,
    pub board_cleared: u64,
    /// Interrupts (runtime-kernel §15.5), and of those, the ones that stopped a turn.
    pub interrupts: u64,
    pub preempted: u64,
    pub signals: u64,
    pub timers_made: u64,
    pub timers_fired: u64,
    /// Of those, fired late (missed and run once).
    pub timers_late: u64,
    pub timers_void: u64,
    /// Chats booted (runtime-kernel §18.1).
    pub boots: u64,
}

/// A grant as the log knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedGrant {
    pub grant: Grant,
    /// A Marv noticed it had expired.
    pub expired: bool,
    /// Calls it ran.
    pub used: u64,
}

/// What the events say the kernel is: the registry (own agents, with their states), the rules not revoked (and which
/// expired), every agent's last state, and totals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KernelState {
    pub registry: Registry,
    pub policies: BTreeMap<PolicyId, LoggedRule>,
    pub states: BTreeMap<AgentKey, AgentState>,
    /// The grants not revoked, with the calls each ran.
    pub grants: BTreeMap<GrantId, LoggedGrant>,
    pub totals: Totals,
}

impl KernelState {
    /// Applies one event.
    pub fn apply(&mut self, event: &KernelEvent) {
        match event {
            KernelEvent::Registered { agent, mesh_name, folder, kind } => {
                let mut entry = Entry::new(agent.clone(), mesh_name.clone(), folder.clone(), *kind);
                entry.state = self.states.get(agent).copied();
                self.registry.upsert(entry);
            }
            KernelEvent::Unregistered { agent } => {
                self.registry.remove(agent);
                self.states.remove(agent);
            }
            KernelEvent::State { agent, to, .. } => {
                self.states.insert(agent.clone(), *to);
                self.registry.set_state(agent, Some(*to));
            }
            KernelEvent::Message { outcome, refusal, .. } => match outcome {
                Delivery::Delivered => self.totals.delivered = self.totals.delivered.saturating_add(1),
                Delivery::Refused => {
                    self.totals.refused = self.totals.refused.saturating_add(1);
                    if *refusal == Some(Refusal::NotWired) {
                        self.totals.refused_not_wired = self.totals.refused_not_wired.saturating_add(1);
                    }
                }
            },
            KernelEvent::WireRefused { .. } => self.totals.wires_refused = self.totals.wires_refused.saturating_add(1),
            KernelEvent::Wake { .. } => self.totals.wakes = self.totals.wakes.saturating_add(1),
            KernelEvent::WakeSkipped { count, .. } => {
                self.totals.wakes_skipped = self.totals.wakes_skipped.saturating_add(*count as u64)
            }
            KernelEvent::Policy { id, change, rule } => match change {
                PolicyChange::Created => {
                    if let Some(rule) = rule.as_ref().filter(|r| r.id == *id) {
                        self.policies.insert(*id, LoggedRule { rule: rule.clone(), expired: false });
                    }
                }
                PolicyChange::Revoked => {
                    self.policies.remove(id);
                }
                PolicyChange::Expired => {
                    if let Some(logged) = self.policies.get_mut(id) {
                        logged.expired = true;
                    }
                }
            },
            KernelEvent::Capability { issued: true, .. } => self.totals.issued = self.totals.issued.saturating_add(1),
            KernelEvent::Capability { issued: false, .. } => self.totals.ended = self.totals.ended.saturating_add(1),
            // Logged again when the person changes it (its cap): the calls it ran are kept.
            KernelEvent::GrantMade { grant } => {
                let used = self.grants.get(&grant.id).map_or(0, |g| g.used);
                self.grants.insert(grant.id, LoggedGrant { grant: grant.clone(), expired: false, used });
            }
            KernelEvent::GrantRevoked { id } => {
                self.grants.remove(id);
            }
            KernelEvent::GrantExpired { id } => {
                if let Some(logged) = self.grants.get_mut(id) {
                    logged.expired = true;
                }
            }
            KernelEvent::GrantPaused { .. } => self.totals.grants_paused = self.totals.grants_paused.saturating_add(1),
            KernelEvent::GrantUsed { id, .. } => {
                self.totals.granted_calls = self.totals.granted_calls.saturating_add(1);
                if let Some(logged) = self.grants.get_mut(id) {
                    logged.used = logged.used.saturating_add(1);
                }
            }
            KernelEvent::Read { outcome: ReadOutcome::Served, .. } => {
                self.totals.reads_served = self.totals.reads_served.saturating_add(1)
            }
            KernelEvent::Read { outcome: ReadOutcome::Refused, .. } => {
                self.totals.reads_refused = self.totals.reads_refused.saturating_add(1)
            }
            KernelEvent::Compacted { by, swap_turns, .. } => {
                self.totals.compactions = self.totals.compactions.saturating_add(1);
                if *by == CompactedBy::Overflow {
                    self.totals.compactions_overflow = self.totals.compactions_overflow.saturating_add(1);
                }
                self.totals.swap_turns = self.totals.swap_turns.saturating_add(*swap_turns);
            }
            KernelEvent::Usage { .. } => self.totals.usage_turns = self.totals.usage_turns.saturating_add(1),
            KernelEvent::CoreSwitched { .. } => self.totals.core_switches = self.totals.core_switches.saturating_add(1),
            KernelEvent::BudgetReached { .. } => self.totals.budget_limits = self.totals.budget_limits.saturating_add(1),
            KernelEvent::BudgetSet { .. } => {}
            KernelEvent::WriteOverlap { .. } => self.totals.write_overlaps = self.totals.write_overlaps.saturating_add(1),
            KernelEvent::BoardPost { .. } => self.totals.board_posts = self.totals.board_posts.saturating_add(1),
            KernelEvent::BoardCleared { count, .. } => {
                self.totals.board_cleared = self.totals.board_cleared.saturating_add(*count as u64)
            }
            KernelEvent::Interrupt { action, .. } => {
                self.totals.interrupts = self.totals.interrupts.saturating_add(1);
                if *action == InterruptAction::Preempted {
                    self.totals.preempted = self.totals.preempted.saturating_add(1);
                }
            }
            KernelEvent::Signal { .. } => self.totals.signals = self.totals.signals.saturating_add(1),
            KernelEvent::TimerMade { .. } => self.totals.timers_made = self.totals.timers_made.saturating_add(1),
            KernelEvent::TimerFired { late, .. } => {
                self.totals.timers_fired = self.totals.timers_fired.saturating_add(1);
                if *late {
                    self.totals.timers_late = self.totals.timers_late.saturating_add(1);
                }
            }
            KernelEvent::TimerVoid { .. } => self.totals.timers_void = self.totals.timers_void.saturating_add(1),
            KernelEvent::Booted { .. } => self.totals.boots = self.totals.boots.saturating_add(1),
        }
    }

    /// The rules part of a live snapshot, as the log would have it: each rule, expired when it has (by `now_ms`).
    pub fn rules_of(snapshot: &PolicySnapshot, now_ms: u64) -> BTreeMap<PolicyId, LoggedRule> {
        snapshot.rules.iter().map(|r| (r.id, LoggedRule { rule: r.clone(), expired: !r.live_at(now_ms) })).collect()
    }
}

/// The state the events, in order, leave (the time isn't needed: events are applied in the order given).
pub fn replay(events: impl IntoIterator<Item = (u64, KernelEvent)>) -> KernelState {
    let mut state = KernelState::default();
    for (_at_ms, event) in events {
        state.apply(&event);
    }
    state
}

/// The events in a log's text, in order, and how many lines were skipped (a half-written last line after a crash, a
/// line of another version, or one that isn't an event).
pub fn parse_log(text: &str) -> (Vec<(u64, KernelEvent)>, usize) {
    let (mut events, mut skipped) = (Vec::new(), 0);
    let parse = |text: &str| serde_json::from_str::<LoggedEvent>(text).ok().filter(|l| l.v == EVENT_VERSION);
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        // A half-written line (a crash, or two writers racing) with an event appended right after it: the junk up to
        // the event's own start is dropped, not the event.
        match parse(line) {
            Some(logged) => events.push((logged.at_ms, logged.event)),
            None => {
                skipped += 1;
                if let Some(logged) = line.rfind("{\"v\":").filter(|at| *at > 0).and_then(|at| parse(&line[at..])) {
                    events.push((logged.at_ms, logged.event));
                }
            }
        }
    }
    (events, skipped)
}

/// Rules whose expiry a Marv has noticed, so each is logged once ([`PolicyChange::Expired`]).
#[derive(Debug, Clone, Default)]
pub struct Expiries {
    told: std::collections::BTreeSet<PolicyId>,
}

impl Expiries {
    /// The rules in `snapshot` that have expired by `now_ms` and weren't noticed yet.
    pub fn newly_expired(&mut self, snapshot: &PolicySnapshot, now_ms: u64) -> Vec<PolicyId> {
        let expired: Vec<PolicyId> =
            snapshot.rules.iter().filter(|r| !r.live_at(now_ms) && !self.told.contains(&r.id)).map(|r| r.id).collect();
        self.told.extend(expired.iter().copied());
        expired
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{AgentId, FolderScope};
    use crate::policy::{RuleBy, Subject};
    use std::path::Path;

    fn chat(n: u8) -> AgentKey {
        AgentKey::Chat(AgentId::new(&format!("{n:02x}000000-0000-4000-8000-000000000000")).unwrap())
    }

    fn rule(id: u64, expires: Option<u64>) -> Policy {
        let scope = FolderScope::new(Path::new("/home/me/acme"), Path::new("/home/me")).unwrap();
        Policy {
            id: PolicyId(id),
            from: Subject::Folder(scope.clone()),
            to: Subject::Folder(scope),
            mode: Mode::Send,
            mutual: true,
            expires_at_ms: expires,
            by: RuleBy::Desktop,
            created_at_ms: 1,
            label: "Acme".into(),
        }
    }

    fn registered(agent: AgentKey, name: &str) -> KernelEvent {
        KernelEvent::Registered { agent, mesh_name: name.into(), folder: "/home/me/acme/api".into(), kind: AgentKind::Chat }
    }

    #[test]
    fn board_events_count_and_carry_no_text_or_paths() {
        let events = vec![
            (
                1,
                KernelEvent::BoardPost { rule: PolicyId(3), by: Some(chat(1)), kind: BoardKind::Claim, size: 12, paths_count: 2 },
            ),
            (2, KernelEvent::BoardPost { rule: PolicyId(3), by: None, kind: BoardKind::Note, size: 3, paths_count: 0 }),
            (3, KernelEvent::BoardCleared { rule: PolicyId(3), by: Some(chat(1)), count: 1 }),
        ];
        let state = replay(events.clone());
        assert_eq!((state.totals.board_posts, state.totals.board_cleared), (2, 1));
        let lines: String = events.iter().map(|(at, e)| LoggedEvent::new(*at, e.clone()).line() + "\n").collect();
        assert_eq!(parse_log(&lines), (events, 0));
        let planted = r#"{"v":1,"at_ms":1,"event":"board_post","rule":3,"kind":"note","size":2,"paths_count":0,"text":"hi"}"#;
        assert_eq!(parse_log(planted).1, 1, "a field the format doesn't have");
    }

    #[test]
    fn replay_counts_grants_and_the_calls_they_ran_never_their_input() {
        let g = crate::grant::tests::grant(4, 1);
        let chat = g.chat.clone();
        let events = vec![
            (1, KernelEvent::GrantMade { grant: g.clone() }),
            (2, KernelEvent::grant_used(g.id, chat.clone(), "write_file", "files", 1)),
            (3, KernelEvent::grant_used(g.id, chat.clone(), "bash", &"x".repeat(500), 1)),
            (4, KernelEvent::GrantPaused { id: g.id, reason: GrantPauseReason::Spend }),
            (5, KernelEvent::GrantExpired { id: g.id }),
        ];
        let state = replay(events.clone());
        let logged = &state.grants[&g.id];
        assert_eq!((logged.used, logged.expired, state.totals.granted_calls, state.totals.grants_paused), (2, true, 2, 1));
        let KernelEvent::GrantUsed { scope_key, .. } = &events[2].1 else { panic!() };
        assert_eq!(scope_key.chars().count(), GRANT_USED_MAX, "cut");
        // Round trip, and a used line has no field an input could go into.
        let lines: String = events.iter().map(|(at, e)| LoggedEvent::new(*at, e.clone()).line() + "\n").collect();
        let (parsed, skipped) = parse_log(&lines);
        assert_eq!((parsed, skipped), (events, 0));
        let planted = r#"{"v":1,"at_ms":1,"event":"grant_used","id":4,"chat":"01000000-0000-4000-8000-000000000000","tool":"bash","scope_key":"bash","turn":1,"input":"rm -rf ~"}"#;
        assert_eq!(parse_log(planted).1, 1, "a field the format doesn't have");
        // A half-written line with the next event appended to it: the junk goes, the event stays.
        let next = LoggedEvent::new(9, KernelEvent::GrantExpired { id: g.id }).line();
        let (parsed, skipped) = parse_log(&format!("{{\"v\":1,\"at_ms\":8,\"ev{next}\n"));
        assert_eq!((parsed, skipped), (vec![(9, KernelEvent::GrantExpired { id: g.id })], 1));
        let revoked = replay(vec![(1, KernelEvent::GrantMade { grant: g.clone() }), (2, KernelEvent::GrantRevoked { id: g.id })]);
        assert!(revoked.grants.is_empty());
    }

    #[test]
    fn interrupts_signals_and_timers_are_counted_and_carry_no_text() {
        use crate::interrupt::{InterruptAction, Kind, SignalBy, SignalKind, Source};
        use crate::timer::{TimerBy, TimerId, TimerVoidReason};
        let a = chat(1);
        let AgentKey::Chat(id) = a.clone() else { panic!() };
        let events = vec![
            (
                1,
                KernelEvent::Interrupt {
                    chat: a.clone(),
                    source: Source::Mail,
                    kind: Kind::Wire,
                    action: InterruptAction::Queued,
                },
            ),
            (
                2,
                KernelEvent::Interrupt {
                    chat: a.clone(),
                    source: Source::Kernel,
                    kind: Kind::Deadline,
                    action: InterruptAction::Preempted,
                },
            ),
            (3, KernelEvent::Signal { to: a.clone(), kind: SignalKind::Pause, by: SignalBy::Person }),
            (4, KernelEvent::TimerMade { id: TimerId(1), chat: id.clone(), by: TimerBy::Chat, recurring: false, fires_at_ms: 9 }),
            (5, KernelEvent::TimerFired { id: TimerId(1), chat: id.clone(), late: true }),
            (6, KernelEvent::TimerVoid { id: TimerId(2), chat: id.clone(), reason: TimerVoidReason::GrantEnded }),
        ];
        let t = replay(events.clone()).totals;
        assert_eq!((t.interrupts, t.preempted, t.signals), (2, 1, 1));
        assert_eq!((t.timers_made, t.timers_fired, t.timers_late, t.timers_void), (1, 1, 1, 1));
        let lines: String = events.iter().map(|(at, e)| LoggedEvent::new(*at, e.clone()).line() + "\n").collect();
        assert_eq!(parse_log(&lines), (events, 0));
        let chat_id = id.as_str();
        for planted in [
            format!(
                r#"{{"v":1,"at_ms":1,"event":"interrupt","chat":{{"chat":"{chat_id}"}},"source":"mail","kind":"wire","action":"queued","text":"rm -rf ~"}}"#
            ),
            format!(
                r#"{{"v":1,"at_ms":1,"event":"timer_made","id":1,"chat":"{chat_id}","by":"chat","recurring":false,"fires_at_ms":9,"note":"rm -rf ~"}}"#
            ),
            format!(r#"{{"v":1,"at_ms":1,"event":"signal","to":{{"chat":"{chat_id}"}},"kind":"stop","by":"peer"}}"#),
        ] {
            assert_eq!(parse_log(&planted).1, 1, "{planted}");
        }
    }

    #[test]
    fn replay_rebuilds_the_registry_rules_and_states() {
        let (a, b) = (chat(1), chat(2));
        let events = vec![
            (1, registered(a.clone(), "api-01000000")),
            (2, KernelEvent::State { agent: a.clone(), from: None, to: AgentState::Sleeping }),
            (3, registered(b.clone(), "web-02000000")),
            (4, KernelEvent::Policy { id: PolicyId(1), change: PolicyChange::Created, rule: Some(rule(1, None)) }),
            (5, KernelEvent::Policy { id: PolicyId(2), change: PolicyChange::Created, rule: Some(rule(2, Some(9))) }),
            (6, KernelEvent::State { agent: a.clone(), from: Some(AgentState::Sleeping), to: AgentState::Loading }),
            (7, KernelEvent::State { agent: a.clone(), from: Some(AgentState::Loading), to: AgentState::Idle }),
            (
                8,
                KernelEvent::Message {
                    agent: a.clone(),
                    id: "api-01000000:1".into(),
                    from: "parent".into(),
                    from_agent: Some(a.clone()),
                    to: "peer.web-02000000.agent.parent.inbox".into(),
                    mode: Mode::Send,
                    hop: Some(0),
                    size: 5,
                    outcome: Delivery::Delivered,
                    refusal: None,
                },
            ),
            (9, KernelEvent::Wake { agent: b.clone(), by: "peer-api-01000000".into(), hop: Some(0) }),
            (10, KernelEvent::Policy { id: PolicyId(2), change: PolicyChange::Expired, rule: None }),
            (11, KernelEvent::Policy { id: PolicyId(1), change: PolicyChange::Revoked, rule: None }),
            (12, KernelEvent::Unregistered { agent: b.clone() }),
            (
                13,
                KernelEvent::WakeSkipped {
                    agent: a.clone(),
                    by: "peer-web-02000000".into(),
                    hop: Some(2),
                    reason: WakeSkipReason::Hop,
                    count: 2,
                },
            ),
            (
                14,
                KernelEvent::Message {
                    agent: a.clone(),
                    id: "api-01000000:2".into(),
                    from: "worker-1".into(),
                    from_agent: None,
                    to: "agent.parent.inbox".into(),
                    mode: Mode::Send,
                    hop: None,
                    size: 3,
                    outcome: Delivery::Refused,
                    refusal: Some(Refusal::NotWired),
                },
            ),
            (
                15,
                KernelEvent::WireRefused {
                    agent: a.clone(),
                    from: "parent".into(),
                    from_agent: Some(a.clone()),
                    to: "peer.web-02000000.agent.parent.inbox".into(),
                    mode: Mode::Send,
                    reason: WireRefusal::Declined,
                },
            ),
        ];
        let state = replay(events.clone());
        assert_eq!(state.registry.len(), 1);
        assert_eq!(state.registry.get(&a).unwrap().state, Some(AgentState::Idle));
        assert_eq!(state.states, BTreeMap::from([(a.clone(), AgentState::Idle)]));
        assert_eq!(state.policies.keys().copied().collect::<Vec<_>>(), [PolicyId(2)]);
        assert!(state.policies[&PolicyId(2)].expired);
        assert_eq!((state.totals.delivered, state.totals.wakes, state.totals.wakes_skipped), (1, 1, 2));
        let t = &state.totals;
        assert_eq!((t.refused, t.refused_not_wired, t.wires_refused), (1, 1, 1));
        state.registry.consistent().unwrap();

        // Through the log's text and back: the same state.
        let text: String =
            events.iter().map(|(t, e)| format!("{}\n", LoggedEvent::new(*t, e.clone()).line())).collect::<String>()
                + "{\"v\":1,\"at";
        let (parsed, skipped) = parse_log(&text);
        assert_eq!((parsed.len(), skipped), (events.len(), 1), "a half-written last line is skipped");
        assert_eq!(replay(parsed), state);

        // A registration after the state keeps the state.
        let again = replay(vec![
            (1, KernelEvent::State { agent: a.clone(), from: None, to: AgentState::Busy }),
            (2, registered(a.clone(), "api-01000000")),
        ]);
        assert_eq!(again.registry.get(&a).unwrap().state, Some(AgentState::Busy));
    }

    #[test]
    fn a_message_line_has_its_fields_and_no_room_for_words() {
        let a = chat(1);
        let line = LoggedEvent::new(
            42,
            KernelEvent::Message {
                agent: a,
                id: "x:1".into(),
                from: "worker-1".into(),
                from_agent: None,
                to: "agent.parent.inbox".into(),
                mode: Mode::Send,
                hop: None,
                size: 11,
                outcome: Delivery::Refused,
                refusal: None,
            },
        )
        .line();
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        let mut keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, ["agent", "at_ms", "event", "from", "id", "mode", "outcome", "size", "to", "v"]);
        assert_eq!(value["agent"]["chat"], "01000000-0000-4000-8000-000000000000");
        // A line with a field the format doesn't have (a body, a text) isn't an event.
        let smuggled = line.replace("\"size\":11", "\"size\":11,\"text\":\"secret\"");
        assert_eq!(parse_log(&smuggled), (Vec::new(), 1));
    }

    /// Runtime-kernel live UX: a skipped wake is who, the hop, a fixed reason and a count; no room for words.
    #[test]
    fn a_skipped_wake_line_has_a_reason_word_and_no_room_for_words() {
        let event =
            KernelEvent::WakeSkipped { agent: chat(1), by: "peer-b".into(), hop: Some(2), reason: WakeSkipReason::Hop, count: 1 };
        let line = LoggedEvent::new(7, event).line();
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        let mut keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, ["agent", "at_ms", "by", "count", "event", "hop", "reason", "v"]);
        assert_eq!((value["event"].as_str(), value["reason"].as_str()), (Some("wake_skipped"), Some("hop")));
        let smuggled = line.replace("\"count\":1", "\"count\":1,\"text\":\"secret\"");
        assert_eq!(parse_log(&smuggled), (Vec::new(), 1));
        let worded = line.replace("\"reason\":\"hop\"", "\"reason\":\"the user said\"");
        assert_ne!(worded, line);
        assert_eq!(parse_log(&worded), (Vec::new(), 1), "a reason is a fixed word");
    }

    /// Runtime-kernel live UX: a refused wire and a refusal's category are ids and fixed words; no room for words.
    #[test]
    fn a_refused_wire_and_a_refusal_are_fixed_words() {
        let event = KernelEvent::WireRefused {
            agent: chat(1),
            from: "parent".into(),
            from_agent: None,
            to: "peer.b.agent.parent.inbox".into(),
            mode: Mode::Send,
            reason: WireRefusal::FarEnd,
        };
        let line = LoggedEvent::new(7, event).line();
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        let mut keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, ["agent", "at_ms", "event", "from", "mode", "reason", "to", "v"]);
        assert_eq!((value["event"].as_str(), value["reason"].as_str()), (Some("wire_refused"), Some("far_end")));
        assert_eq!(parse_log(&line.replace("\"far_end\"", "\"they said: hi\"")), (Vec::new(), 1));
        let refused = KernelEvent::Message {
            agent: chat(1),
            id: "x:2".into(),
            from: "worker-1".into(),
            from_agent: None,
            to: "agent.parent.inbox".into(),
            mode: Mode::Send,
            hop: None,
            size: 4,
            outcome: Delivery::Refused,
            refusal: Some(Refusal::NotWired),
        };
        let line = LoggedEvent::new(8, refused).line();
        assert!(line.contains("\"refusal\":\"not_wired\""), "{line}");
        assert_eq!(parse_log(&line.replace("\"not_wired\"", "\"no wire from worker-1: hi\"")), (Vec::new(), 1));
    }

    /// Runtime-kernel §9.2: a read is who read whom, what, its size and a fixed outcome; no room for the text served.
    #[test]
    fn a_read_line_has_its_fields_and_no_room_for_words() {
        let served = KernelEvent::Read {
            reader: chat(1),
            target: AgentKey::Mesh("api-3f9a".into()),
            what: ReadWhat::Tail,
            n: Some(5),
            size: 812,
            outcome: ReadOutcome::Served,
            reason: None,
        };
        let line = LoggedEvent::new(9, served.clone()).line();
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        let mut keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, ["at_ms", "event", "n", "outcome", "reader", "size", "target", "v", "what"]);
        assert_eq!((value["event"].as_str(), value["what"].as_str()), (Some("read"), Some("tail")));
        assert_eq!(parse_log(&line.replace("\"size\":812", "\"size\":812,\"text\":\"secret\"")), (Vec::new(), 1));
        let refused = KernelEvent::Read {
            reader: chat(1),
            target: chat(2),
            what: ReadWhat::Summary,
            n: None,
            size: 0,
            outcome: ReadOutcome::Refused,
            reason: Some(ReadRefusal::Private),
        };
        let line2 = LoggedEvent::new(10, refused.clone()).line();
        assert!(line2.contains("\"reason\":\"private\""), "{line2}");
        assert_eq!(parse_log(&line2.replace("\"private\"", "\"they typed: hi\"")), (Vec::new(), 1));
        let state = replay(vec![(9, served), (10, refused.clone()), (11, refused)]);
        assert_eq!((state.totals.reads_served, state.totals.reads_refused), (1, 2));
    }

    /// Runtime-kernel §11.4: a page-out line is numbers and a fixed word, with no room for a summary or a turn.
    #[test]
    fn a_compacted_line_has_sizes_and_no_room_for_words() {
        let compacted = KernelEvent::Compacted {
            chat: chat(1),
            by: CompactedBy::Overflow,
            level: Some(1),
            tokens_before: Some(42_000),
            tokens_after: 9_000,
            cost_usd_micros: Some(30_000),
            page: Some(2),
            swap_turns: 118,
        };
        let line = LoggedEvent::new(9, compacted.clone()).line();
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        let mut keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "at_ms",
                "by",
                "chat",
                "cost_usd_micros",
                "event",
                "level",
                "page",
                "swap_turns",
                "tokens_after",
                "tokens_before",
                "v"
            ]
        );
        assert_eq!((value["event"].as_str(), value["by"].as_str()), (Some("compacted"), Some("overflow")));
        assert_eq!(parse_log(&line.replace("\"swap_turns\":118", "\"swap_turns\":118,\"summary\":\"secret\"")), (Vec::new(), 1));
        assert_eq!(parse_log(&line.replace("\"overflow\"", "\"keep the auth notes\"")), (Vec::new(), 1));
        let person = KernelEvent::Compacted {
            chat: chat(2),
            by: CompactedBy::Person,
            level: None,
            tokens_before: None,
            tokens_after: 3_000,
            cost_usd_micros: None,
            page: None,
            swap_turns: 0,
        };
        let state = replay(vec![(9, compacted.clone()), (10, person), (11, compacted)]);
        let t = &state.totals;
        assert_eq!((t.compactions, t.compactions_overflow, t.swap_turns), (3, 2, 236));
    }

    /// Runtime-kernel §14.4: a write overlap is who and how many; no room for a path or a file's content.
    #[test]
    fn a_write_overlap_line_is_who_and_how_many() {
        let overlap = KernelEvent::WriteOverlap { a: chat(1), b: AgentKey::Mesh("api-3f9a".into()), paths_count: 2 };
        let line = LoggedEvent::new(9, overlap.clone()).line();
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        let mut keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, ["a", "at_ms", "b", "event", "paths_count", "v"]);
        assert_eq!(value["event"].as_str(), Some("write_overlap"));
        assert_eq!(parse_log(&line.replace("\"paths_count\":2", "\"paths_count\":2,\"paths\":[\"/w/a.rs\"]")), (Vec::new(), 1));
        assert_eq!(replay(vec![(9, overlap.clone()), (10, overlap)]).totals.write_overlaps, 2);
    }

    #[test]
    fn each_expiry_is_noticed_once() {
        let snapshot = PolicySnapshot { generation: 2, rules: vec![rule(1, Some(10)), rule(2, None), rule(3, Some(20))] };
        let mut seen = Expiries::default();
        assert!(seen.newly_expired(&snapshot, 9).is_empty());
        assert_eq!(seen.newly_expired(&snapshot, 10), [PolicyId(1)], "expiry is exclusive");
        assert!(seen.newly_expired(&snapshot, 15).is_empty());
        assert_eq!(seen.newly_expired(&snapshot, 30), [PolicyId(3)]);
        let rules = KernelState::rules_of(&snapshot, 30);
        assert_eq!(rules.values().filter(|r| r.expired).count(), 2);
    }
}
