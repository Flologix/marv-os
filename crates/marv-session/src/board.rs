//! The session's side of the team board (runtime-kernel §19; `marv_mesh::board`). New in marv-rs.
//!
//! [`BoardTool`]: `board_read`, `board_post`, `board_clear` for the main agent of every session with a mesh, always
//! (three constant specs after the mesh's, the prompt cache). Where no rule covers the chat (or the host doesn't
//! honor rules: `marv -p`, an SDK session) each says `marv_mesh::board::NO_RULE` and does nothing. Subagents don't get
//! them: a board entry is the chat's, by its mesh name.
//!
//! - `board_read` is `Read`. What it returns is other chats' words: fenced, and the session counts as tainted
//!   (`Provenance::Mesh`) when any entry came back.
//! - `board_post` writes the rule's own board: where this chat could `mesh_send` under that rule (a `send` rule, on its
//!   sending side unless mutual), like that send (D2) it doesn't ask, tainted or not, in a person's turn or an
//!   unattended one. Under a read or call rule, or on the receiving end of a one-way rule, each post asks a person
//!   (`once`, [`NOT_A_SENDER_WARNING`]). **And after a `chat_read`** (T4, the read-and-relay rule from #25's review): once
//!   the conversation read another chat (`TaintState::read_chat`), every post asks a person, `once` (no "always", no
//!   yolo, no service grant answers it), saying why ([`READ_CHAT_POST_WARNING`]), so a board can't relay what a read
//!   rule didn't allow.
//! - `board_clear { mine: true }` removes only this chat's own entries: it doesn't ask.

use async_trait::async_trait;
use marv_core::{Provenance, ToolKind};
use marv_mesh::board::{BOARD_CLEAR, BOARD_POST, BOARD_READ, board_specs, kind_of};
use marv_tools::{Preview, Tool, ToolContext, ToolError, ToolOutput};
use serde_json::{Map, Value};

use crate::mesh::MeshHandle;

/// What a post's approval says once the conversation read another chat (T4).
pub const READ_CHAT_POST_WARNING: &str = "This conversation read another chat (chat_read): a board post can carry that chat's words to every chat under the rule, so each post asks you first.";

/// What a post's approval says where this chat couldn't `mesh_send` under the board's rule (a read or call rule, the
/// receiving end of a one-way rule).
pub const NOT_A_SENDER_WARNING: &str = "The rule this board belongs to doesn't let this chat message the others (it's a read or call rule, or this chat is on its receiving end): a post reaches every chat under it, so each post asks you first.";

/// The three board tools, in `board_specs()` order.
pub fn tools(mesh: &MeshHandle) -> Vec<BoardTool> {
    [BOARD_READ, BOARD_POST, BOARD_CLEAR].into_iter().map(|name| BoardTool { name, mesh: mesh.clone() }).collect()
}

/// One board tool for the main agent (module doc).
pub struct BoardTool {
    pub name: &'static str,
    pub mesh: MeshHandle,
}

#[async_trait]
impl Tool for BoardTool {
    type Input = Map<String, Value>;
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        match self.name {
            BOARD_READ => marv_mesh::board::READ_DESCRIPTION,
            BOARD_POST => marv_mesh::board::POST_DESCRIPTION,
            _ => marv_mesh::board::CLEAR_DESCRIPTION,
        }
    }
    fn schema(&self) -> Value {
        board_specs().into_iter().find(|s| s.name == self.name).map(|s| s.parameters).unwrap_or(Value::Null)
    }
    fn kind(&self) -> ToolKind {
        kind_of(self.name).unwrap_or(ToolKind::Write)
    }
    /// Marv's own words (an empty board, "Posted", a clear's count, a refusal), as the mesh tools declare: #220's
    /// `run_tool` raises every result to the declared source, so declaring Mesh would taint an empty read. A read
    /// with entries (other chats' words) sets `Provenance::Mesh` on its result itself, and taints.
    fn provenance(&self) -> Provenance {
        Provenance::Marv
    }
    /// The board is the rule's, outside the project: no checkpoint.
    fn checkpoint(&self) -> bool {
        false
    }
    fn check(&self, input: &Self::Input) -> Result<(), String> {
        marv_mesh::board::check(self.name, input)
    }
    fn label(&self, input: &Self::Input) -> String {
        match self.name {
            BOARD_POST => input.get("kind").and_then(Value::as_str).unwrap_or(BOARD_POST).to_string(),
            _ => self.name.to_string(),
        }
    }
    /// A post asks only after a `chat_read` (T4); a read and a clear of one's own never do.
    fn needs_approval(&self, _input: &Self::Input) -> Option<bool> {
        (self.name != BOARD_POST).then_some(false)
    }
    async fn preview(&self, input: &Self::Input, ctx: &ToolContext) -> Result<Preview, ToolError> {
        let text = input.get("text").and_then(Value::as_str).unwrap_or_default().to_string();
        let warning = if self.name != BOARD_POST {
            None
        } else if !self.read_lets_post(input, ctx) {
            Some(READ_CHAT_POST_WARNING.to_string())
        } else if !marv_mesh::board::post_runs_unasked(&self.mesh, input) {
            Some(NOT_A_SENDER_WARNING.to_string())
        } else {
            None
        };
        Ok(Preview { title: "Post to the team board".into(), text: Some(text), warning, ..Preview::default() })
    }
    /// A post asks every time once the conversation read another chat.
    fn once(&self, _input: &Self::Input, _ctx: &ToolContext) -> bool {
        true
    }
    /// T4 (D2's shape): a post runs unasked, tainted or not, in any turn, only to the board of a rule this chat could
    /// `mesh_send` under (a `send` rule, on its sending side unless mutual), and only while the conversation hasn't
    /// read another chat. Otherwise (a read or call rule, the receiving end of a one-way rule, after a `chat_read`) it
    /// asks a person every time: never yolo or a grant.
    fn own_marvs_only(&self, input: &Self::Input, ctx: &ToolContext) -> bool {
        self.name == BOARD_POST && self.read_lets_post(input, ctx) && marv_mesh::board::post_runs_unasked(&self.mesh, input)
    }
    async fn run(&self, input: &Self::Input, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let result = match self.name {
            BOARD_READ => {
                let rule = input.get("rule").and_then(Value::as_u64).map(marv_kernel::PolicyId);
                marv_mesh::board::read(&self.mesh, rule).await.map(|read| {
                    let mut out = ToolOutput::ok(read.text, format!("{} entries", read.entries));
                    // Other chats' words: the session is tainted from here (Provenance::Mesh), as by mail.
                    if read.entries > 0 {
                        out.provenance = Provenance::Mesh;
                    }
                    out
                })
            }
            BOARD_POST => match marv_mesh::board::parse_post(input) {
                Ok(post) => marv_mesh::board::post(&self.mesh, post).await.map(|t| ToolOutput::ok(t, "posted")),
                Err(e) => Err(e),
            },
            _ => match marv_mesh::board::parse_clear(input) {
                Ok(rule) => marv_mesh::board::clear(&self.mesh, rule).await.map(|t| ToolOutput::ok(t, "cleared")),
                Err(e) => Err(e),
            },
        };
        // A refusal is Marv's own sentence: no taint.
        Ok(result.unwrap_or_else(ToolOutput::error))
    }
}

/// `/board` (the TUI): each covering rule's board as the person sees it, one entry a line, every word of it through
/// `printable` (other chats wrote it).
pub async fn person_view(mesh: &MeshHandle) -> String {
    let rules = mesh.board_rules();
    let Some(store) = mesh.boards().filter(|_| !rules.is_empty()) else {
        return "No rule covers this Marv, so it has no team board. A rule you make (the desktop's Mesh panel, or marv \
                mesh allow) gives the chats under it one."
            .into();
    };
    let now = crate::time::now_ms();
    let mut out = Vec::new();
    for rule in rules.into_iter().map(|r| r.id) {
        match store.read(rule, now) {
            Ok(entries) if entries.is_empty() => out.push(format!("Team board of {rule}: empty.")),
            Ok(entries) => {
                out.push(format!("Team board of {rule} ({} entries):", entries.len()));
                for e in entries {
                    let paths = if e.paths.is_empty() { String::new() } else { format!(" [{}]", e.paths.join(", ")) };
                    let line =
                        format!("- {} {}: {}{paths} ({})", e.by, e.kind.word(), e.text, marv_mesh::board::age(e.at_ms, now));
                    out.push(marv_core::printable(&line));
                }
            }
            Err(e) => out.push(marv_core::printable(&format!("Team board of {rule} can't be read: {e}"))),
        }
    }
    out.join("\n")
}

impl BoardTool {
    /// Whether what this conversation read (`chat_read`) may go on this post's board without the person: nothing read,
    /// or only chats of the team that board belongs to (`TaintState::read_confined_to`).
    fn read_lets_post(&self, input: &Map<String, Value>, ctx: &ToolContext) -> bool {
        let team = marv_mesh::board::post_rule_label(&self.mesh, input)
            .and_then(|label| crate::teams::team_of_rule_label(&label).map(str::to_string));
        ctx.taint.read_confined_to(team.as_deref())
    }
}
