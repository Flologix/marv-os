//! The session's side of `chat_read` (runtime-kernel §9.2; `marv_mesh::chat_read`). New in marv-rs.
//!
//! - [`ChatReadTool`]: the main agent's tool, offered only where the session's host asks for it
//!   (`SessionInit::chat_read`: the TUI and Marv desktop's chats, which honor the person's rules). Never in `marv -p`,
//!   an SDK session, a subagent (its tools are the built-ins and its own plug's) or a pane. It is `Read`: under a
//!   covering read rule it doesn't ask, and without one it is refused before anything is read (see the deviation
//!   note in `marv_mesh::chat_read::read_chat`). What it returns is another chat's words: fenced, and the session
//!   counts as tainted (`Provenance::Mesh`), so its egress outside the mesh asks from then on.
//! - [`snapshot_of`]: what a read may see of a conversation, from the transcript the person saw: their typed
//!   messages, the assistant's replies, and tool entries by **name only** (never a tool's input or output, R1), plus
//!   the stored compaction summary (R4). Marv's own notices and a woken turn's line (`Role::System`) are left out, and
//!   so is everything only the model saw (mesh messages, attachments, notes). No model call.
//! - [`SessionSource`]: a loaded session as a `ChatSource` (the TUI's). While a turn runs (mk-60m.7) it serves the
//!   conversation as that turn found it ([`ReadCut`]: the transcript up to then, the summary then), marked mid-turn
//!   with when the last finished turn ended: never the turn in progress, its person message included. It used to
//!   refuse with `busy`, which left a coordinator unable to read a peer that was working.
//! - **No read-and-relay** (review P1): `chat_read` runs only in a turn the person started ([`NOT_STARTED_BY_YOU`]),
//!   and once a read is in the conversation (`TaintState::read_chat`, which lasts as the taint does) its `mesh_send`s
//!   lose a rule's no-ask (D2): they ask as tainted egress does, saying so ([`READ_CHAT_WARNING`]). Otherwise a send
//!   rule wider than a read rule would carry what was read past the read rule's folders.

use std::sync::Arc;

use async_trait::async_trait;
use marv_core::{ChatTurn, Provenance, ToolKind};
use marv_mesh::chat_read::{CHAT_READ, ChatSnapshot, ChatSource, ReadAsk, Turn, Unreadable};
use marv_tools::{Tool, ToolContext, ToolError, ToolOutput};
use serde_json::{Map, Value};

use crate::mesh::MeshHandle;
use crate::session::{MarvSession, WeakSession};
use crate::types::{Message, Role};

/// What `chat_read` says in a turn the person didn't start (a mesh wake, the radio).
pub const NOT_STARTED_BY_YOU: &str = "chat_read runs only in a turn you started";

/// What a mesh send's approval adds once the conversation read another chat (review P1(b)).
pub const READ_CHAT_WARNING: &str = "This conversation read another chat (chat_read): what it sends can carry that chat's words, so a rule's send asks you first.";

/// Where a conversation stood when its running turn started (mk-60m.7): what a mid-turn read serves.
#[derive(Debug, Clone)]
pub(crate) struct ReadCut {
    /// The transcript's length then (the client's, else the session's own).
    pub(crate) shown: usize,
    /// The session's own transcript's length then: the card's person messages.
    pub(crate) messages: usize,
    /// The compaction summary then (a compaction in the turn may replace it).
    pub(crate) summary: Option<String>,
    /// When the last finished turn ended (0: none has).
    pub(crate) ended_ms: u64,
}

/// "2026-10-25 14:03 UTC, 4 minutes ago": when a mid-turn chat's last finished turn ended, for the read's first line.
pub(crate) fn finished_words(ended_ms: u64, now_ms: u64) -> String {
    let iso = crate::time::iso(ended_ms);
    let minutes = now_ms.saturating_sub(ended_ms) / 60_000;
    let ago = match minutes {
        0 => "under a minute ago".to_string(),
        1 => "1 minute ago".to_string(),
        m if m < 120 => format!("{m} minutes ago"),
        m => format!("{} hours ago", m / 60),
    };
    format!("{} {} UTC, {ago}", &iso[..10], &iso[11..16])
}

/// The conversation's stored compaction summary, if it was compacted: what `compacted_history` put first.
pub(crate) fn summary_of(conversation: &[ChatTurn]) -> Option<String> {
    match conversation.first()? {
        ChatTurn::User { text } => text.strip_prefix(marv_agent::compact::COMPACTED_PREFIX).map(|s| s.trim().to_string()),
        _ => None,
    }
}

/// What a read may see of a conversation (module doc): from its transcript and the model's conversation (for the
/// compaction summary only), with its card.
pub fn snapshot_of(transcript: &[Message], conversation: &[ChatTurn], card: marv_kernel::Card) -> ChatSnapshot {
    let turns = transcript
        .iter()
        .filter_map(|m| match m.role {
            Role::User if !m.text.trim().is_empty() => Some(Turn::Person(m.text.clone())),
            Role::Assistant if !m.text.trim().is_empty() => Some(Turn::Assistant(m.text.clone())),
            Role::Tool => Some(Turn::Tool(m.text.clone())),
            _ => None,
        })
        .collect();
    ChatSnapshot { card: Some(card), summary: summary_of(conversation), turns, mid_turn: None }
}

/// A saved conversation as a read may see it (a sleeping desktop chat's, from its file).
pub fn snapshot_of_saved(saved: &crate::sessions::SavedSession) -> ChatSnapshot {
    snapshot_of(&saved.transcript, &saved.conversation, crate::sessions::card_of(saved))
}

/// A loaded session as a [`ChatSource`] (the TUI's): read from memory; mid-turn, as the turn found it. `notice` shows the
/// person a served read's line (`marv_mesh::chat_read::notice_line`). Holds the session weakly: the switchboard it
/// is set on belongs to the session.
pub struct SessionSource {
    pub session: WeakSession,
    pub notice: Arc<dyn Fn(String) + Send + Sync>,
}

#[async_trait]
impl ChatSource for SessionSource {
    async fn snapshot(&self) -> Result<ChatSnapshot, Unreadable> {
        let session = self.session.upgrade().ok_or(Unreadable::Unavailable)?;
        session.chat_snapshot()
    }

    fn read_by(&self, reader: &str, ask: ReadAsk) {
        (self.notice)(marv_mesh::chat_read::notice_line(reader, ask));
    }
}

/// `chat_read` for the main agent (module doc).
pub struct ChatReadTool {
    pub mesh: MeshHandle,
}

#[async_trait]
impl Tool for ChatReadTool {
    type Input = Map<String, Value>;
    fn name(&self) -> &str {
        CHAT_READ
    }
    fn description(&self) -> &str {
        marv_mesh::chat_read::DESCRIPTION
    }
    fn schema(&self) -> Value {
        marv_mesh::chat_read::spec().parameters
    }
    /// Read-only and inbound: under a read rule it doesn't ask (and without one nothing is read).
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn provenance(&self) -> Provenance {
        Provenance::Mesh
    }
    fn checkpoint(&self) -> bool {
        false
    }
    fn check(&self, input: &Self::Input) -> Result<(), String> {
        marv_mesh::chat_read::parse_input(input).map(drop)
    }
    fn label(&self, input: &Self::Input) -> String {
        input.get("chat").and_then(Value::as_str).unwrap_or(CHAT_READ).to_string()
    }
    async fn run(&self, input: &Self::Input, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        // Review P1(a): only in a turn the person started. A turn a mesh message or the radio woke (automatic approval
        // off) could be another agent asking for a read to relay: refused before anything is asked or read.
        if !ctx.mode.auto_approval() {
            return Ok(ToolOutput::error(NOT_STARTED_BY_YOU));
        }
        let (chat, ask) = marv_mesh::chat_read::parse_input(input).map_err(ToolError::new)?;
        match marv_mesh::chat_read::read_chat_under(&self.mesh, &chat, ask).await {
            Ok((text, rule)) => {
                // Another chat's words: the session is tainted from here (Provenance::Mesh), like any inbox message,
                // and flagged (review P1(b)): its rule sends to the person's own Marvs ask from now on, so what was
                // read can't be relayed past the read rule's folders without the person. A teammate's, read under its
                // team's read rule, may still go to that team (`TaintState::read_confined_to`): the person put
                // those chats together, and anywhere else still asks.
                match crate::teams::team_of_rule_label(&rule) {
                    Some(team) => ctx.taint.mark_read_team(team),
                    None => ctx.taint.mark_read_chat(),
                };
                let mut out = ToolOutput::ok(text, "read");
                out.provenance = Provenance::Mesh;
                Ok(out)
            }
            // Marv's own sentence: no taint.
            Err(err) => Ok(ToolOutput::error(err.text)),
        }
    }
}

impl MarvSession {
    /// This conversation as another chat's `chat_read` may see it ([`snapshot_of`]). While a turn runs (mk-60m.7):
    /// as that turn found it ([`ReadCut`]), marked mid-turn, never the turn in progress. (A compaction or an undo
    /// keeps the session busy too, but adds no turn: read as it is.)
    pub fn chat_snapshot(&self) -> Result<ChatSnapshot, Unreadable> {
        // The transcript is read without the session's lock: a turn that starts (or ends) meanwhile is seen by the
        // count changing, and the read looks again, so a turn's partial words are never served as finished.
        // The card is taken inside the loop too: a turn that starts after it can't put its person message in "now".
        let (cut, transcript, conversation, card) = loop {
            let (before, cut) = self.read_cut();
            let (transcript, conversation) = self.read_view();
            let card = self.card_within(cut.as_ref().map_or(usize::MAX, |c| c.messages));
            if self.read_cut().0 == before {
                break (cut, transcript, conversation, card);
            }
        };
        let Some(cut) = cut else {
            return Ok(snapshot_of(&transcript, &conversation, card));
        };
        let shown = &transcript[..cut.shown.min(transcript.len())];
        let mut snapshot = snapshot_of(shown, &[], card);
        snapshot.summary = cut.summary;
        let finished = (cut.ended_ms > 0).then(|| finished_words(cut.ended_ms, crate::time::now_ms()));
        snapshot.mid_turn = Some(marv_mesh::chat_read::MidTurn { finished });
        Ok(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_is_the_persons_and_assistants_words_and_tool_names_only() {
        let mut tool = Message::new(3, Role::Tool, "read_file");
        tool.tool = Some(crate::types::ToolStatus {
            label: "secrets/.env".into(),
            status: crate::types::ToolState::Done,
            summary: Some("API_KEY=sk-live-0000".into()),
            steps: None,
            diff: None,
        });
        let transcript = vec![
            Message::new(1, Role::User, "fix the login"),
            Message::new(2, Role::System, "Woken by a message from worker-1 on agent.parent.inbox"),
            tool,
            Message::new(4, Role::Assistant, "done"),
            Message::new(5, Role::Assistant, "  "),
        ];
        let conversation = marv_agent::compact::compacted_history("We built it.", None, false);
        let card = marv_kernel::Card {
            title: "fix the login".into(),
            now: "fix the login".into(),
            last_active_ms: 1,
            ..Default::default()
        };
        let snapshot = snapshot_of(&transcript, &conversation, card);
        assert_eq!(
            snapshot.turns,
            [Turn::Person("fix the login".into()), Turn::Tool("read_file".into()), Turn::Assistant("done".into())]
        );
        assert_eq!(snapshot.summary.as_deref(), Some("We built it."));
        let served = marv_mesh::chat_read::reply(&snapshot, ReadAsk::tail(5)).unwrap();
        for leak in ["secrets/.env", "sk-live", "Woken by"] {
            assert!(!served.contains(leak), "{leak}: {served}");
        }
        assert_eq!(summary_of(&[ChatTurn::User { text: "hi".into() }]), None, "not compacted: no summary");
    }

    #[test]
    fn a_mid_turn_reads_finished_time_is_utc_and_how_long_ago() {
        let ended = 1_791_878_709_042; // 2026-10-13T08:05:09Z
        assert_eq!(finished_words(ended, ended + 30_000), "2026-10-13 08:05 UTC, under a minute ago");
        assert_eq!(finished_words(ended, ended + 60_000), "2026-10-13 08:05 UTC, 1 minute ago");
        assert_eq!(finished_words(ended, ended + 47 * 60_000), "2026-10-13 08:05 UTC, 47 minutes ago");
        assert_eq!(finished_words(ended, ended + 5 * 3_600_000), "2026-10-13 08:05 UTC, 5 hours ago");
        assert_eq!(finished_words(ended, 0), "2026-10-13 08:05 UTC, under a minute ago", "a clock behind: no panic");
    }
}
