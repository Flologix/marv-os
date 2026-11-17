//! Interrupts and signals (runtime-kernel §15, decisions I1–I3). New in marv-rs.
//!
//! Every event that starts, stops or changes a chat's turn is an [`Interrupt`]: a [`Source`] and a [`Kind`],
//! delivered to the chat through the kernel (the desktop's resident record) and nowhere else. [`handling`] is the one
//! decision of what it does to the chat now:
//! - **preempt** (at the next safe point: the stream is cancelled, a running call finishes or is killed): only the
//!   person's stop or close, a grant revoked or expired in an unattended turn, the turn's deadline (§15.4: the kernel
//!   stopping an unattended turn), and shutdown. Nothing else ever stops a running turn.
//! - **at the next step boundary**: mail and radio on the turn's own wires (read between steps, as before), the
//!   person's "send now", a pause (the person's, or a grant's spend cap): the current step finishes, cache-safe.
//! - **queued as the next turn**, in [`Source`] order: person > kernel > mail > radio > timer ([`InterruptQueue`]).
//!   A person's message always goes ahead of any queued wake, and a timer never jumps a person.
//!
//! A [`Signal`] is what one chat can be sent: stop, pause, resume, close. Only the person sends one ([`SignalBy`] has no
//! other variant): a peer can send only mail (and, under a rule, the wake that mail causes). There's no `kill(2)`
//! between chats, so a hijacked chat can't take its team down.
//!
//! Pure: no I/O, no clock. The words here are fixed (they go into the event log, which never holds text).

use serde::{Deserialize, Serialize};

/// Where an interrupt comes from. The order is the queue's priority: the person first, timers last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Person,
    Kernel,
    Mail,
    Radio,
    Timer,
}

/// What an interrupt is (a fixed word).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// The person's message (plain Enter: the next turn).
    Message,
    /// The person's "send now": into the running turn at its next step.
    SendNow,
    /// Esc, or a stop from the Mesh panel.
    Stop,
    /// The person's pause (the Mesh panel, stop-the-team): no next step, no wakes or timers until resumed.
    Pause,
    Resume,
    /// The person closed (stopped) the chat: it leaves the mesh.
    Close,
    /// An answer to an approval.
    Approval,
    /// A grant revoked, expired, or at its spend cap.
    GrantRevoked,
    GrantExpired,
    SpendCap,
    /// The unattended turn's deadline (§15.4).
    Deadline,
    /// The chat's session unloaded (the scheduler's eviction, §5.3).
    Unload,
    Shutdown,
    /// A message on a wire (mail).
    Wire,
    /// A radio message.
    Radio,
    /// A timer fired.
    Timer,
}

/// What became of an interrupt (the event log's `action`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterruptAction {
    /// The running turn was stopped for it.
    Preempted,
    /// It reaches the running turn at its next step boundary.
    AtStep,
    /// It waits for the next turn, in priority order.
    Queued,
    /// It had nothing to act on (a stop with no turn running) or couldn't run (a timer whose grant ended).
    Dropped,
}

/// What a chat is doing when an interrupt comes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnNow {
    Idle,
    /// A turn the person started.
    Person,
    /// A turn the person didn't start (a wake, a timer): automatic approval off unless a grant holds.
    Unattended,
}

/// One interrupt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Interrupt {
    pub source: Source,
    pub kind: Kind,
}

impl Interrupt {
    pub fn new(source: Source, kind: Kind) -> Self {
        Self { source, kind }
    }
}

/// Whether `interrupt` stops the running turn `now` (I2). Only the person's stop or close, the kernel's revoke,
/// expiry or deadline of an unattended turn, and shutdown. A person's own turn is never stopped by its grant ending
/// (it doesn't run under it) nor by a deadline (that's for turns nobody watches).
pub fn preempts(interrupt: Interrupt, now: TurnNow) -> bool {
    if now == TurnNow::Idle {
        return false;
    }
    match (interrupt.source, interrupt.kind) {
        (Source::Person, Kind::Stop | Kind::Close) => true,
        (Source::Kernel, Kind::Shutdown) => true,
        (Source::Kernel, Kind::GrantRevoked | Kind::GrantExpired | Kind::Deadline) => now == TurnNow::Unattended,
        _ => false,
    }
}

/// What `interrupt` does to a chat that is doing `now` (I2), as the event log records it.
pub fn handling(interrupt: Interrupt, now: TurnNow) -> InterruptAction {
    if preempts(interrupt, now) {
        return InterruptAction::Preempted;
    }
    let running = now != TurnNow::Idle;
    match interrupt.kind {
        // Things that start a turn: the next one, in priority order. Mail and radio on a running turn's own wires
        // are read between its steps.
        Kind::Wire | Kind::Radio if running => InterruptAction::AtStep,
        Kind::Message | Kind::Wire | Kind::Radio | Kind::Timer => InterruptAction::Queued,
        Kind::SendNow | Kind::Pause | Kind::SpendCap if running => InterruptAction::AtStep,
        // A pause or resume with nothing running still holds (or releases) the chat's wakes and timers.
        Kind::Pause | Kind::Resume => InterruptAction::Queued,
        Kind::Approval if running => InterruptAction::AtStep,
        // Nothing running to act on (a stop, a send-now with no turn: it goes back to the composer).
        _ => InterruptAction::Dropped,
    }
}

/// The interrupts waiting to start a chat's next turn, each with what it carries (`T`). [`InterruptQueue::pop`] takes
/// the highest-priority [`Source`], oldest first within one.
#[derive(Debug, Clone)]
pub struct InterruptQueue<T> {
    items: Vec<(Source, u64, T)>,
    next: u64,
}

impl<T> Default for InterruptQueue<T> {
    fn default() -> Self {
        Self { items: Vec::new(), next: 0 }
    }
}

impl<T> InterruptQueue<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, source: Source, item: T) {
        self.items.push((source, self.next, item));
        self.next += 1;
    }

    /// The next to run: the highest-priority source (person > kernel > mail > radio > timer), oldest first.
    pub fn pop(&mut self) -> Option<(Source, T)> {
        let at = self.items.iter().enumerate().min_by_key(|(_, (source, seq, _))| (*source, *seq)).map(|(i, _)| i)?;
        let (source, _, item) = self.items.remove(at);
        Some((source, item))
    }

    /// The source [`InterruptQueue::pop`] would take next.
    pub fn peek(&self) -> Option<Source> {
        self.items.iter().map(|(source, seq, _)| (*source, *seq)).min().map(|(source, _)| source)
    }

    /// What waits, in no particular order.
    pub fn iter(&self) -> impl Iterator<Item = (Source, &T)> {
        self.items.iter().map(|(source, _, item)| (*source, item))
    }

    /// Whether something from `source` waits.
    pub fn has(&self, source: Source) -> bool {
        self.items.iter().any(|(s, _, _)| *s == source)
    }

    /// Drops what `keep` says no to.
    pub fn retain(&mut self, mut keep: impl FnMut(Source, &T) -> bool) {
        self.items.retain(|(source, _, item)| keep(*source, item));
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// A signal one chat can be sent (I3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalKind {
    /// Stop the running turn (as Esc).
    Stop,
    /// No next step; no wakes or timers until resumed.
    Pause,
    Resume,
    /// Stop the chat: its session ends and it leaves the mesh until it's opened again.
    Close,
}

impl SignalKind {
    /// The interrupt it is.
    pub fn interrupt(self) -> Interrupt {
        let kind = match self {
            SignalKind::Stop => Kind::Stop,
            SignalKind::Pause => Kind::Pause,
            SignalKind::Resume => Kind::Resume,
            SignalKind::Close => Kind::Close,
        };
        Interrupt::new(Source::Person, kind)
    }
}

/// Who sent a signal: always the person (the desktop's Mesh panel). There is no variant for a peer, a mesh message,
/// a model, a project or a flag: a peer can send only mail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalBy {
    Person,
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCES: [Source; 5] = [Source::Person, Source::Kernel, Source::Mail, Source::Radio, Source::Timer];
    const KINDS: [Kind; 16] = [
        Kind::Message,
        Kind::SendNow,
        Kind::Stop,
        Kind::Pause,
        Kind::Resume,
        Kind::Close,
        Kind::Approval,
        Kind::GrantRevoked,
        Kind::GrantExpired,
        Kind::SpendCap,
        Kind::Deadline,
        Kind::Unload,
        Kind::Shutdown,
        Kind::Wire,
        Kind::Radio,
        Kind::Timer,
    ];

    #[test]
    fn the_queue_runs_person_kernel_mail_radio_timer_oldest_first() {
        let mut q = InterruptQueue::new();
        q.push(Source::Timer, "t1");
        q.push(Source::Mail, "m1");
        q.push(Source::Radio, "r1");
        q.push(Source::Mail, "m2");
        q.push(Source::Person, "p1");
        q.push(Source::Kernel, "k1");
        q.push(Source::Person, "p2");
        assert_eq!(q.peek(), Some(Source::Person));
        let order: Vec<&str> = std::iter::from_fn(|| q.pop().map(|(_, t)| t)).collect();
        assert_eq!(order, ["p1", "p2", "k1", "m1", "m2", "r1", "t1"]);
        assert!(q.is_empty() && q.pop().is_none());
    }

    #[test]
    fn only_stop_revoke_deadline_and_shutdown_preempt() {
        for now in [TurnNow::Idle, TurnNow::Person, TurnNow::Unattended] {
            for source in SOURCES {
                for kind in KINDS {
                    let i = Interrupt::new(source, kind);
                    let expected = now != TurnNow::Idle
                        && matches!(
                            (source, kind, now),
                            (Source::Person, Kind::Stop | Kind::Close, _)
                                | (Source::Kernel, Kind::Shutdown, _)
                                | (Source::Kernel, Kind::GrantRevoked | Kind::GrantExpired | Kind::Deadline, TurnNow::Unattended)
                        );
                    assert_eq!(preempts(i, now), expected, "{source:?} {kind:?} {now:?}");
                }
            }
        }
        // Mail, radio and timers never stop a turn: they wait for a step or the next turn.
        let wire = Interrupt::new(Source::Mail, Kind::Wire);
        assert_eq!(handling(wire, TurnNow::Unattended), InterruptAction::AtStep);
        assert_eq!(handling(wire, TurnNow::Idle), InterruptAction::Queued);
        assert_eq!(handling(Interrupt::new(Source::Timer, Kind::Timer), TurnNow::Person), InterruptAction::Queued);
        assert_eq!(handling(Interrupt::new(Source::Person, Kind::Stop), TurnNow::Idle), InterruptAction::Dropped);
        assert_eq!(handling(Interrupt::new(Source::Person, Kind::Pause), TurnNow::Person), InterruptAction::AtStep);
    }

    #[test]
    fn a_signal_is_only_ever_the_persons() {
        assert_eq!(serde_json::from_value::<SignalBy>(serde_json::json!("person")).unwrap(), SignalBy::Person);
        for by in ["peer", "mesh", "model", "chat", "project", "rule", "cli"] {
            assert!(serde_json::from_value::<SignalBy>(serde_json::json!(by)).is_err(), "{by}");
        }
        for kind in [SignalKind::Stop, SignalKind::Pause, SignalKind::Resume, SignalKind::Close] {
            assert_eq!(kind.interrupt().source, Source::Person);
        }
    }
}
