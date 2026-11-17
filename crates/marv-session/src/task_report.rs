//! Task wakes and report-back (runtime-kernel coordination). New in marv-rs.
//!
//! - A turn a mesh message woke ([`crate::mesh::WakeBy`]) is a **task turn** when the chat's role
//!   (`crate::roles`) takes tasks from the message's sender: its chat name (`peer-<name>` is the link a peer Marv's
//!   messages arrive on) or the role that chat published on its door (`marv_mesh::discovery::PeerInfo::role`). A
//!   report ([`marv_mesh::message::looks_like_report`]) never starts one. The turn reads [`crate::mesh::task_wake_note`] and
//!   runs in `CallMode::Task` (`marv_tools::floor::task_auto_runs`).
//! - **Every** woken inbox turn, task or not, sends its sender a report when it ends, built without a model call
//!   from how the turn ended (`crate::radio::report_of`'s shape): done, didn't act, failed (stopped, declined, the step
//!   limit, an error), and once, when the turn first asks its person something, "waiting for its user". It goes into
//!   the sender's main inbox over a wire that exists or a rule the person made issues (never one a person would have to
//!   approve: then it isn't sent, and the transcript says so).
//! - A report never wakes its reader (`crate::mesh::WakeSkip::Report`), so two chats can't keep each other going:
//!   the sender's `mesh_wait`, or its next turn, reads it. Except over a team's wire (`crate::mesh::TEAM_WAKES_PER_HOUR`),
//!   where it wakes an idle coordinator within the chat's wake budget; a turn a report woke sends no report back.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use marv_mesh::approval::WireRequest;
use marv_mesh::plug::PlugClient;
use marv_mesh::{ReceptacleName, WireMode};
use marv_tools::{ApprovalRequest, Approver, Decision};

use crate::mesh::{MeshHandle, WakeBy, parent_id};

/// Who woke an inbox turn, and whether it's a task turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxTurn {
    pub by: WakeBy,
    /// The peer Marv that sent it (`peer-<name>`'s name); None for a message from a local plug (a subagent, a shim):
    /// no report goes to those.
    pub sender_chat: Option<String>,
    /// The chat's role takes tasks from that sender.
    pub task: bool,
    /// A report woke it (a team's wire lets a stamped report wake an idle chat, `crate::mesh::decide_wake_as`): it
    /// sends none back, so two chats can't answer each other's reports until the team budget runs out.
    pub report: bool,
    /// That report answers a task (its sender's turn was a task turn, `marv_mesh::Envelope::task`).
    pub report_task: bool,
}

/// Who woke an inbox turn and whether it's a task turn for a chat with `role` (runtime-kernel coordination's
/// decision): a hop-0 message over a rule's wire, from a sender the role names by the role it publishes, or by chat
/// name only when it's a desktop chat (a stable name). `MarvSession`'s, and a harness chat's (the harness plan, phase 3).
pub async fn inbox_turn_for(mesh: &MeshHandle, by: WakeBy, role: Option<&crate::roles::Role>) -> InboxTurn {
    let sender_chat = marv_mesh::discovery::peer_session(&by.sender).map(String::from);
    let task = match (role, &sender_chat) {
        (Some(role), Some(chat)) if !role.takes_tasks_from.is_empty() && crate::mesh::task_eligible(mesh, &by) => {
            let by_name =
                crate::mesh::desktop_chat_name(mesh, &by).is_some_and(|name| &name == chat && role.takes_tasks_from(chat, None));
            by_name || crate::mesh::sender_role(mesh, chat).await.is_some_and(|r| role.takes_tasks_from.iter().any(|t| t == &r))
        }
        _ => false,
    };
    let (report, report_task) = crate::mesh::oldest_report(mesh, &by);
    InboxTurn { by, sender_chat, task, report, report_task }
}

/// The harness plan, phase 3: the report a harness chat's woken inbox turn sends its sender when it ends, as
/// `MarvSession`'s (`end_report`, `send_report`): `reason` is the page's turn-end word ("end", "max_steps",
/// "interrupted", "error"), `calls` the tools it ran, `reply` its last reply (quoted only as [`may_quote`] allows; a
/// harness reads no chat through Marv). None: no report is due (a report woke it, or no peer sent it). Some: the
/// transcript's line, sent or why not.
pub async fn harness_report(
    mesh: &MeshHandle,
    inbox: &InboxTurn,
    reason: &str,
    calls: usize,
    reply: Option<&str>,
) -> Option<String> {
    use marv_core::StopReason;
    if inbox.report {
        return None;
    }
    let peer = inbox.sender_chat.clone()?;
    let reason = match reason {
        "end" => crate::session::TurnEndReason::Done(StopReason::End),
        "max_steps" => crate::session::TurnEndReason::Done(StopReason::MaxSteps),
        "interrupted" => crate::session::TurnEndReason::Interrupted,
        _ => crate::session::TurnEndReason::Done(StopReason::Error),
    };
    let me = mesh.session.clone();
    let kind = std::sync::Mutex::new(None);
    let build = |rule_wire: bool| {
        let quote = may_quote(inbox.task, rule_wire, false);
        let (k, text) = end_report(reason, &me, inbox.task, calls, reply.filter(|_| quote), None, 0, false);
        *kind.lock().unwrap_or_else(|e| e.into_inner()) = Some(k);
        report_text(&me, k, &text)
    };
    let sent = tokio::time::timeout(REPORT_WAIT, send_report(mesh, &peer, inbox.task, build)).await;
    let word = kind.lock().unwrap_or_else(|e| e.into_inner()).map_or("", |k| k.word());
    Some(match sent {
        Ok(Ok(())) => format!("Report → {peer} ({word})"),
        Ok(Err(why)) => format!("The report to {peer} wasn't sent: {why}"),
        Err(_) => format!("The report to {peer} wasn't sent: wiring it took too long"),
    })
}

/// What a report says happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportKind {
    Done,
    DidntAct,
    Failed,
    Waiting,
}

impl ReportKind {
    pub fn word(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::DidntAct => "didn't act",
            Self::Failed => "failed",
            Self::Waiting => "waiting for its user",
        }
    }
}

/// A report's whole text: [`marv_mesh::message::REPORT_PREFIX`], the reporting Marv, the kind, then what happened.
pub fn report_text(me: &str, kind: ReportKind, text: &str) -> String {
    format!("{}{} ({}): {}", marv_mesh::message::REPORT_PREFIX, marv_core::printable(me), kind.word(), text)
}

/// The end-of-turn report from how the turn ended: `crate::radio::report_for`'s text, its kind narrowed. A turn that
/// ended normally is `done` when it was a task turn that ran a tool, `didn't act` otherwise (a task turn that ran
/// nothing, or a woken turn whose role doesn't take tasks from the sender: it only told its person).
///
/// The report goes out unasked, so it carries the model's words (the reply's first paragraph) only when `reply` is
/// given: the caller passes it for a task turn whose wire back a rule issued, in a conversation that read no other
/// chat ([`may_quote`]); otherwise the report is Marv's fixed words (the kind, the files changed, a declined tool).
#[allow(clippy::too_many_arguments)] // The turn's facts, as `radio::report_for` takes them, plus the task's two.
pub fn end_report(
    reason: crate::session::TurnEndReason,
    me: &str,
    task: bool,
    calls: usize,
    reply: Option<&str>,
    declined: Option<&str>,
    changed: usize,
    person_joined: bool,
) -> (ReportKind, String) {
    let reply = reply.filter(|_| task);
    let (kind, text) = crate::radio::report_for(reason, me, reply, declined, changed, person_joined);
    let kind = match kind {
        marv_mesh::radio::Kind::Done if task && calls > 0 => ReportKind::Done,
        marv_mesh::radio::Kind::Done if task => ReportKind::DidntAct,
        marv_mesh::radio::Kind::Done => {
            return (
                ReportKind::DidntAct,
                format!("{me}'s role doesn't take tasks from you, so it told its user what arrived. {text}"),
            );
        }
        _ => ReportKind::Failed,
    };
    (kind, text)
}

/// Whether a report may quote the model's reply (P1-3 of the security review): a task turn, back over a wire a rule
/// the person made issued, from a conversation that didn't read another chat. Anything else gets fixed words only.
pub fn may_quote(task: bool, rule_wire: bool, read_chat: bool) -> bool {
    task && rule_wire && !read_chat
}

/// Sends the report `text` makes (given whether the wire back is a rule's) from this chat's main agent to Marv
/// `peer`'s main inbox: over a wire that exists, or one a rule the person made issues now. Never asks anyone: without
/// either, it isn't sent (the error says why).
pub async fn send_report(mesh: &MeshHandle, peer: &str, task: bool, text: impl FnOnce(bool) -> String) -> Result<(), String> {
    let inbox = ReceptacleName::new(format!("agent.{}.inbox", marv_mesh::PARENT_AGENT)).map_err(|e| e.to_string())?;
    let receptacle = marv_mesh::discovery::peer_receptacle(peer, &inbox).map_err(|e| e.to_string())?;
    let me = parent_id();
    if mesh.authorize(&me, &receptacle, WireMode::Send).is_err() {
        if mesh.lookup(&receptacle).await.is_none() {
            return Err(format!("{peer} isn't on the mesh any more"));
        }
        let request = WireRequest { from: me.clone(), to: receptacle.clone(), mode: WireMode::Send, asked_by: me.clone() };
        let Some(by) = mesh.rule_for(&request).await else {
            return Err(format!("there's no wire back to {peer}, and no rule of your user's makes one"));
        };
        mesh.grant(request, by).await.map_err(|e| e.to_string())?;
    }
    let rule_wire = mesh.authorize(&me, &receptacle, WireMode::Send).is_ok_and(|w| w.grant.local.is_policy());
    let text = text(rule_wire);
    // Marv's own code stamps this message as a report; nothing a model sends, or text that merely starts like one, is.
    // `task`: the reporting turn was a task turn, so the report answers a hop-0 task (a team coordinator roots on those).
    // The stamp is taken back if the send fails or this future is dropped (a timeout): none is left armed.
    let mut armed = ArmedReport { hop: mesh.turn_hop(&me), text: text.clone(), sent: false };
    if let Some(hop) = &armed.hop {
        hop.arm_report(&text, task);
    }
    let plug = mesh.plug(me).map_err(|e| e.to_string())?;
    let mut client = PlugClient::connect(&plug.host_socket).await.map_err(|e| e.to_string())?;
    let sent = client.send(&receptacle, &text).await.map_err(|e| e.to_string());
    armed.sent = sent.is_ok();
    sent
}

/// An armed report stamp ([`marv_mesh::hop::TurnHop::arm_report`]) that is taken back unless the report was sent.
struct ArmedReport {
    hop: Option<Arc<marv_mesh::hop::TurnHop>>,
    text: String,
    sent: bool,
}

impl Drop for ArmedReport {
    fn drop(&mut self) {
        if !self.sent
            && let Some(hop) = &self.hop
        {
            hop.disarm_report(&self.text);
        }
    }
}

/// The client's approver in a woken inbox turn: the first question it asks the person also tells the sender the turn
/// is waiting for them (once), so the sender's `mesh_wait` doesn't sit out its whole timeout.
pub struct WaitingReporter {
    pub inner: Arc<dyn Approver>,
    pub mesh: MeshHandle,
    pub peer: String,
    pub me: String,
    /// The waiting turn is a task turn: its report answers a task.
    pub task: bool,
    pub told: AtomicBool,
}

#[async_trait]
impl Approver for WaitingReporter {
    async fn approve(&self, request: ApprovalRequest) -> Decision {
        if !self.told.swap(true, Ordering::SeqCst) {
            let what = format!("it asked its user to approve {} and waits for the answer", marv_core::printable(&request.tool));
            let text = report_text(&self.me, ReportKind::Waiting, &what);
            let (mesh, peer, task) = (self.mesh.clone(), self.peer.clone(), self.task);
            tokio::spawn(async move {
                let _ = tokio::time::timeout(REPORT_WAIT, send_report(&mesh, &peer, task, |_| text)).await;
            });
        }
        self.inner.approve(request).await
    }
}

/// Told to the sender of a message that waits in a chat that doesn't start a turn for it (a wire its person approved
/// by hand, mesh wake off): a "waiting for its user" report, so the sender's `mesh_wait` doesn't sit out its whole
/// timeout for a reply that can't come until the person delivers the message. None: no peer chat sent it.
pub async fn waiting_report(mesh: &MeshHandle, by: &crate::mesh::WakeBy) -> Option<String> {
    let peer = marv_mesh::discovery::peer_session(&by.sender)?.to_string();
    let me = mesh.session.clone();
    let text = report_text(&me, ReportKind::Waiting, MAIL_WAITS);
    Some(match tokio::time::timeout(REPORT_WAIT, send_report(mesh, &peer, false, |_| text)).await {
        Ok(Ok(())) => format!("Report → {peer} (waiting for its user)"),
        Ok(Err(why)) => format!("The report to {peer} wasn't sent: {why}"),
        Err(_) => format!("The report to {peer} wasn't sent: wiring it took too long"),
    })
}

/// [`waiting_report`]'s words.
pub const MAIL_WAITS: &str = "your message doesn't start a turn here (this wire doesn't wake this chat), so it waits until this chat's user delivers it; don't wait on a reply";

/// The longest a report's wiring may take (a rule's wire to another Marv waits for that Marv's door).
pub const REPORT_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::TurnEndReason;
    use marv_core::StopReason;

    #[test]
    fn the_end_report_says_done_only_for_a_task_turn_that_acted() {
        let done = TurnEndReason::Done(StopReason::End);
        let reply = Some("Changed styles.css: the header is blue.\n\nDetails follow.");
        let (kind, text) = end_report(done, "front-1", true, 3, reply, None, 1, false);
        assert_eq!((kind, text.as_str()), (ReportKind::Done, "Changed styles.css: the header is blue. (1 file changed)"));
        assert_eq!(end_report(done, "front-1", true, 0, reply, None, 0, false).0, ReportKind::DidntAct);
        let (kind, text) = end_report(done, "front-1", false, 2, reply, None, 0, false);
        assert_eq!(kind, ReportKind::DidntAct);
        assert!(text.starts_with("front-1's role doesn't take tasks from you"), "{text}");
        let (kind, text) = end_report(TurnEndReason::Done(StopReason::MaxSteps), "front-1", true, 9, None, None, 0, false);
        assert_eq!((kind, text.as_str()), (ReportKind::Failed, "the turn ended: the step limit"));
        let full = report_text("front-1", ReportKind::Failed, &text);
        assert!(marv_mesh::message::looks_like_report(&full));
        assert_eq!(full, "Marv report from front-1 (failed): the turn ended: the step limit");
    }
}
