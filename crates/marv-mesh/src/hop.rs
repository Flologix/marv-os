//! How deep in a chain of agents waking agents a message is (runtime-kernel 2b). New in marv-rs.
//!
//! The person's own turn starts at hop 0. A turn a message woke runs one hop past the deepest message it read, and what
//! it sends carries that hop. A person's turn stays at 0 through the replies it waits for (`mesh_wait`: a stamped
//! report, or mail sent at hop 0 from a turn its own person started), but reading any other outside message (one with
//! a higher hop, or none at all: a shim's) raises it to 1 (never higher: hop 2 wakes no rule's peers, so its own conversation is limited by its rounds) from then on, so what it sends afterwards isn't
//! task-eligible (docs/SECURITY.md). The radio has done this since #124 (`radio::MAX_WAKE_HOP`); runtime-kernel 2b gives inbox
//! messages a hop too ([`crate::Envelope::hop`]), so a rule-issued wire's chain can stop waking at hop 2.
//!
//! **Set by Marv, never by a model.** The switchboard stamps a message from one of Marv's own plugs (the main agent,
//! a subagent) with that agent's [`TurnHop`], registered by the session ([`crate::Switchboard::set_turn_hop`]); a hop
//! in the request (`_meta`, a tool argument) is ignored there, as a sandboxed command can speak on its plug's socket.
//! Only a peer Marv's plug (`peer-<session>`, made by this Marv's door for a pid-confirmed Marv and reachable only from
//! that Marv's host process) carries its hop in `tools/call`'s `_meta` ([`HOP_META`]), as its `PeerLink` forwards it.
//! A shim plug (`marv mesh plug`: Claude Code, opencode) has no hop at all.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// `tools/call`'s `_meta` key for a message's hop, across a `PeerLink` (beside [`crate::plug::MODE_META`]).
pub const HOP_META: &str = "marv/hop";
/// `tools/call`'s `_meta` key saying the sending turn used up its rounds ([`TurnHop::rounds_out`], runtime-kernel live
/// UX), across a `PeerLink`. It can only stop a wake, never start one.
pub const ROUNDS_META: &str = "marv/rounds_out";
/// `tools/call`'s `_meta` key saying the message is a woken turn's automatic report ([`TurnHop::arm_report`]), across a
/// `PeerLink`: read from a peer Marv's plug only, like the hop. A peer that sends no key sent no report.
pub const REPORT_META: &str = "marv/report";
/// `tools/call`'s `_meta` key saying the report answers a task: its reporting turn was itself a task turn
/// ([`TurnHop::arm_report`]). Read from a peer Marv's plug only, like the report flag, and only with it.
pub const TASK_META: &str = "marv/task";
/// marv-rs: `tools/call`'s `_meta` key saying the sending chat had read nothing from outside when it sent
/// ([`TurnHop::clean`]): read from a peer Marv's plug only, like the hop. A peer that sends no key sent tainted mail.
pub const CLEAN_META: &str = "marv/clean";

/// The hop of the turn running now: 0 for a turn the person started, one past the deepest message read for a turn a
/// message started. What the turn sends carries it. (The radio's `TurnHop`, generalized: marv-session re-exports it.)
///
/// Runtime-kernel live UX: a person's turn stays at hop 0 (1 once it reads outside mail) however many replies it reads mid-turn (`mesh_wait`), so it
/// also counts its **rounds**: mid-turn inbox reads that brought messages over a rule's wire. Past a cap (marv-session's
/// `MAX_LEAD_ROUNDS`) the turn is [`Self::rounds_out`]: what it sends still arrives, but wakes nobody, until the
/// person's next message starts a new turn ([`Self::new_turn`]).
#[derive(Debug, Default)]
pub struct TurnHop {
    hop: AtomicU32,
    rounds: AtomicU32,
    rounds_out: AtomicBool,
    /// A team coordinator's turn woken only by task reports from its team ([`Self::root_in_team`]): the team rule's
    /// id, so only a send over a wire that rule issued goes out at hop 0.
    team_rooted: std::sync::Mutex<Option<marv_kernel::PolicyId>>,
    /// The automatic reports Marv's code is about to send, with whether each answers a task ([`Self::arm_report`]).
    report: std::sync::Mutex<Vec<(String, bool)>>,
    /// marv-rs: the chat's taint, for [`Self::clean`]. Registered by the session; none: never clean.
    taint: std::sync::Mutex<Option<std::sync::Arc<marv_sandbox::TaintState>>>,
}

impl TurnHop {
    pub fn get(&self) -> u32 {
        self.hop.load(Ordering::SeqCst)
    }
    pub fn set(&self, hop: u32) {
        self.hop.store(hop, Ordering::SeqCst);
    }
    /// At least `hop` from now on (a turn a message started read a message from deeper in a chain).
    pub fn raise(&self, hop: u32) {
        self.hop.fetch_max(hop, Ordering::SeqCst);
    }
    /// marv-rs: the chat's taint, which [`Self::clean`] reads at each send.
    pub fn watch_taint(&self, taint: std::sync::Arc<marv_sandbox::TaintState>) {
        *self.taint.lock().unwrap_or_else(|e| e.into_inner()) = Some(taint);
    }
    /// marv-rs: the chat has read nothing from outside (no web page, MCP output, tainted mail…), so what it sends now
    /// carries no outside text: a teammate reading it is reading the team's own work, as a subagent's report is. False
    /// when the session registered no taint.
    pub fn clean(&self) -> bool {
        self.taint.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|t| !t.is_tainted())
    }

    /// A new turn at `hop`: its rounds start again.
    pub fn new_turn(&self, hop: u32) {
        self.set(hop);
        self.rounds.store(0, Ordering::SeqCst);
        self.rounds_out.store(false, Ordering::SeqCst);
        self.unroot();
        self.report.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
    /// This turn was woken by a team's task report alone (set by the session, from [`crate::Envelope::report`] and
    /// [`crate::Envelope::task`] over the wire team rule `rule` issued, never from text): its sends over a wire that
    /// same rule issued go out at hop 0 ([`Self::team_rooted`]), so a team's rework reaches its worker as a task, as the
    /// first send did. Ends with [`Self::unroot`], with the turn's end and with every new turn.
    pub fn root_in_team(&self, rule: marv_kernel::PolicyId) {
        *self.team_rooted.lock().unwrap_or_else(|e| e.into_inner()) = Some(rule);
    }
    /// The turn read something besides a team's task reports (other mail, a radio message, a chat, the board): it is no
    /// longer rooted.
    pub fn unroot(&self) {
        *self.team_rooted.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
    /// The team rule this turn is still rooted in ([`Self::root_in_team`]).
    pub fn team_rooted(&self) -> Option<marv_kernel::PolicyId> {
        *self.team_rooted.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// One more round this turn; past `max`, the turn's sends stop waking anyone ([`Self::rounds_out`]).
    pub fn count_round(&self, max: u32) {
        if self.rounds.fetch_add(1, Ordering::SeqCst).saturating_add(1) >= max {
            self.rounds_out.store(true, Ordering::SeqCst);
        }
    }
    /// Marv's own code is about to send the automatic report `text` (`marv-session`'s `task_report::send_report`): the
    /// next message from this plug with exactly that text is stamped [`crate::Envelope::report`]. A model's `mesh_send`
    /// and a sandboxed command on the plug's socket can't arm it, and the stamp is used up by one message.
    ///
    /// `task`: the reporting turn was itself a task turn (`InboxTurn::task`), so the report answers a hop-0 task: the
    /// message is also stamped [`crate::Envelope::task`], from Marv's code alone.
    pub fn arm_report(&self, text: &str, task: bool) {
        self.report.lock().unwrap_or_else(|e| e.into_inner()).push((text.to_string(), task));
    }
    /// Takes back an armed report that was not sent (the send failed or timed out).
    pub fn disarm_report(&self, text: &str) {
        let mut armed = self.report.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = armed.iter().position(|(t, _)| t == text) {
            armed.remove(i);
        }
    }
    /// Takes the armed report if `text` is it: `Some(task)` (what the switchboard stamps a message with).
    pub(crate) fn take_report(&self, text: &str) -> Option<bool> {
        let mut armed = self.report.lock().unwrap_or_else(|e| e.into_inner());
        armed.iter().position(|(t, _)| t == text).map(|i| armed.remove(i).1)
    }
    /// The rounds counted this turn.
    pub fn rounds(&self) -> u32 {
        self.rounds.load(Ordering::SeqCst)
    }
    /// Whether this turn used up its rounds: its messages carry [`crate::Envelope::rounds_out`].
    pub fn rounds_out(&self) -> bool {
        self.rounds_out.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turns_hop_is_set_and_only_rises_when_raised() {
        let hop = TurnHop::default();
        assert_eq!(hop.get(), 0);
        hop.raise(2);
        hop.raise(1);
        assert_eq!(hop.get(), 2);
        hop.set(0);
        assert_eq!(hop.get(), 0, "a person typing resets the chain");
    }

    /// Runtime-kernel live UX: rounds count up to the cap, then the turn is out of them until a new turn.
    #[test]
    fn a_turns_rounds_run_out_at_the_cap_and_a_new_turn_resets_them() {
        let hop = TurnHop::default();
        for _ in 0..4 {
            hop.count_round(5);
        }
        assert!(!hop.rounds_out());
        hop.count_round(5);
        assert!(hop.rounds_out() && hop.rounds() == 5);
        hop.root_in_team(marv_kernel::PolicyId(1));
        hop.new_turn(0);
        assert!(!hop.rounds_out() && hop.rounds() == 0 && hop.get() == 0 && hop.team_rooted().is_none());
    }
}
