//! Taint: once web, MCP, mesh or image content enters a session's context, every exfiltration-capable call asks,
//! even in yolo, even after an "always".
//!
//! New in marv-rs (`docs/SECURITY.md`, "Provenance and taint"). Marv had no taint: yolo ran sandboxed bash and file
//! edits after a `web_fetch`, and remembered scopes ("don't ask again") answered network calls for the rest of the
//! session. The principle: an injected model must never hold private data, untrusted content and a way out together;
//! taint marks the moment the second arrives, so the third needs a person.
//!
//! What asks once tainted ([`TaintState::must_ask`]): `ToolKind::Egress` calls and any call that `uses_network`:
//! network bash, every MCP call (including `readOnlyHint` tools), web requests to a new URL prefix, mesh calls on
//! unapproved wires; and writes to protected paths (those ask regardless of taint).
//! A GitHub door read doesn't `use_network` (it only reaches GitHub's API, which shows nobody who read what): it
//! runs unasked, and its output taints.
//!
//! A subagent starts tainted if its parent is (its prompt was written by a tainted model), but inherits no network,
//! web or mesh approvals (`for_subagent`).

use std::collections::BTreeSet;
use std::sync::Mutex;

use marv_core::{Provenance, ToolKind};

/// A session's taint. Shared (`Arc`) between the session, its tool context and the approver.
#[derive(Debug, Default)]
pub struct TaintState {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    /// What tainted it, in order, each once.
    sources: Vec<Provenance>,
    /// URL prefixes the user approved while tainted (`https://docs.rs/`): a web request under one doesn't ask again.
    approved_prefixes: BTreeSet<String>,
    /// Another chat's words came in (`chat_read`, runtime-kernel §9.2): from here the conversation's mesh sends to the
    /// person's own Marvs lose their rule's no-ask (D2), so a read rule can't be widened by relaying what was read
    /// over a send rule. Lasts as the taint does (compaction, resume; saved with the session).
    read_chat: bool,
    /// The teams whose own chats it read (`chat_read` under a team's read rule): [`TaintState::read_confined_to`].
    read_teams: BTreeSet<String>,
}

impl TaintState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records text from `source` entering the context. Returns true when this call tainted a clean session.
    pub fn mark(&self, source: Provenance) -> bool {
        if !source.taints() {
            return false;
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let was_clean = inner.sources.is_empty();
        if !inner.sources.contains(&source) {
            inner.sources.push(source);
        }
        was_clean
    }

    pub fn is_tainted(&self) -> bool {
        !self.inner.lock().unwrap_or_else(|e| e.into_inner()).sources.is_empty()
    }

    /// What tainted the session, first first (for the status bar and the approval's warning).
    pub fn sources(&self) -> Vec<Provenance> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).sources.clone()
    }

    /// Records that another chat's words (`chat_read`) entered the context: tainted as a mesh message, and flagged
    /// ([`Self::read_chat`]).
    pub fn mark_read_chat(&self) -> bool {
        let tainted = self.mark(Provenance::Mesh);
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).read_chat = true;
        tainted
    }

    /// Whether the conversation holds another chat's words (`chat_read`).
    pub fn read_chat(&self) -> bool {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).read_chat
    }

    /// Records that a teammate's words (`chat_read` under team `team`'s read rule) entered the context: tainted as a mesh
    /// message, but not flagged as [`Self::read_chat`]: sends inside that one team still run as its rule lets them
    /// ([`Self::read_confined_to`]).
    pub fn mark_read_team(&self, team: &str) -> bool {
        let tainted = self.mark(Provenance::Mesh);
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).read_teams.insert(team.to_string());
        tainted
    }

    /// The teams whose chats the conversation read.
    pub fn read_teams(&self) -> Vec<String> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).read_teams.iter().cloned().collect()
    }

    /// A resumed conversation's teams read (after [`Self::restore`], which clears them).
    pub fn restore_read_teams(&self, teams: &[String]) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).read_teams = teams.iter().cloned().collect();
    }

    /// Whether what the conversation read may go, without the person, to a chat over team `team`'s wire (None: not a
    /// team's): nothing read, or only chats of that same team. Another chat's words (a read outside a team), or two
    /// teams', ask: no team can carry what another team's chat said.
    pub fn read_confined_to(&self, team: Option<&str>) -> bool {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.read_chat {
            return false;
        }
        match (inner.read_teams.len(), team) {
            (0, _) => true,
            (1, Some(team)) => inner.read_teams.contains(team),
            _ => false,
        }
    }

    /// A resumed conversation's `read_chat` flag (after [`Self::restore`], which clears it).
    pub fn restore_read_chat(&self, read: bool) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).read_chat = read;
    }

    /// Whether a call must ask a person now, whatever yolo or remembered scopes say.
    pub fn must_ask(&self, kind: ToolKind, uses_network: bool) -> bool {
        self.is_tainted() && (kind == ToolKind::Egress || uses_network)
    }

    /// Remembers a web prefix the user approved after taint.
    pub fn approve_prefix(&self, prefix: &str) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).approved_prefixes.insert(prefix.to_string());
    }

    /// The prefixes approved after taint, sorted (for `/permissions`).
    pub fn approved_prefixes(&self) -> Vec<String> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).approved_prefixes.iter().cloned().collect()
    }

    /// Whether a URL is under a prefix approved after taint.
    pub fn prefix_approved(&self, url: &str) -> bool {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).approved_prefixes.iter().any(|p| url.starts_with(p.as_str()))
    }

    /// A subagent's state: tainted like its parent, with no approvals.
    /// A resumed conversation's taint: exactly these sources, in order, and no approved prefixes (approvals were the
    /// last process's). The conversation still holds the outside text that tainted it, so a resume must restore this.
    pub fn restore(&self, sources: &[Provenance]) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.sources.clear();
        inner.approved_prefixes.clear();
        inner.read_chat = false;
        inner.read_teams.clear();
        for source in sources.iter().copied().filter(|s| s.taints()) {
            if !inner.sources.contains(&source) {
                inner.sources.push(source);
            }
        }
    }

    /// What tainted `child` (a subagent) taints this one too: its report can carry that text back.
    pub fn absorb(&self, child: &TaintState) {
        for source in child.sources() {
            self.mark(source);
        }
        if child.read_chat() {
            self.mark_read_chat();
        }
    }

    pub fn for_subagent(&self) -> TaintState {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        TaintState {
            inner: Mutex::new(Inner {
                sources: inner.sources.clone(),
                approved_prefixes: BTreeSet::new(),
                read_chat: inner.read_chat,
                read_teams: inner.read_teams.clone(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn restore_sets_exactly_the_saved_sources_and_absorb_carries_a_childs_up() {
        let t = TaintState::new();
        t.mark(Provenance::Mcp);
        t.approve_prefix("https://docs.rs/");
        t.restore(&[Provenance::Web, Provenance::User, Provenance::Web]);
        assert_eq!(t.sources(), [Provenance::Web], "only tainting sources, once each");
        assert!(!t.prefix_approved("https://docs.rs/serde"), "approvals were the last process's");
        let parent = TaintState::new();
        let child = parent.for_subagent();
        child.mark(Provenance::Mesh);
        assert!(!parent.is_tainted());
        parent.absorb(&child);
        assert_eq!(parent.sources(), [Provenance::Mesh]);
    }

    /// Runtime-kernel §9.2: a chat read taints as mesh text and flags the conversation; the flag goes with a subagent
    /// and back, and a resume sets it as saved.
    #[test]
    fn a_chat_read_taints_and_is_flagged() {
        let t = TaintState::new();
        assert!(!t.read_chat());
        assert!(t.mark_read_chat());
        assert_eq!((t.sources(), t.read_chat()), (vec![Provenance::Mesh], true));
        assert!(t.for_subagent().read_chat());
        t.restore(&[Provenance::Mesh]);
        assert!(!t.read_chat(), "restore sets exactly what was saved");
        t.restore_read_chat(true);
        assert!(t.read_chat());
        let parent = TaintState::new();
        let child = parent.for_subagent();
        child.mark_read_chat();
        parent.absorb(&child);
        assert!(parent.read_chat());
    }

    use super::*;

    #[test]
    fn only_outside_sources_taint() {
        let taint = TaintState::new();
        assert!(!taint.mark(Provenance::User));
        assert!(!taint.mark(Provenance::Project));
        assert!(!taint.is_tainted());
        assert!(taint.mark(Provenance::Web));
        assert!(!taint.mark(Provenance::Mcp)); // already tainted
        assert_eq!(taint.sources(), vec![Provenance::Web, Provenance::Mcp]);
    }

    #[test]
    fn egress_asks_once_tainted() {
        let taint = TaintState::new();
        assert!(!taint.must_ask(ToolKind::Egress, true));
        taint.mark(Provenance::Mesh);
        assert!(taint.must_ask(ToolKind::Egress, false));
        assert!(taint.must_ask(ToolKind::Execute, true)); // network bash
        assert!(!taint.must_ask(ToolKind::Write, false));
    }

    #[test]
    fn subagents_inherit_taint_but_not_approvals() {
        let taint = TaintState::new();
        taint.mark(Provenance::Web);
        taint.approve_prefix("https://docs.rs/");
        assert!(taint.prefix_approved("https://docs.rs/regex"));
        let child = taint.for_subagent();
        assert!(child.is_tainted());
        assert!(!child.prefix_approved("https://docs.rs/regex"));
    }

    /// A teammate's read (desktop teams) leaves that one team's sends unasked; another team, a chat outside any team,
    /// or a second team's read ask; a restore clears it.
    #[test]
    fn a_read_stays_inside_its_one_team() {
        let t = TaintState::default();
        assert!(t.read_confined_to(None) && t.read_confined_to(Some("web")));
        t.mark_read_team("web");
        assert!(t.is_tainted() && !t.read_chat());
        assert!(t.read_confined_to(Some("web")));
        assert!(!t.read_confined_to(Some("api")) && !t.read_confined_to(None));
        t.mark_read_team("api");
        assert!(!t.read_confined_to(Some("web")) && !t.read_confined_to(Some("api")), "two teams' words go nowhere unasked");
        t.restore(&[]);
        assert!(t.read_teams().is_empty() && t.read_confined_to(Some("web")));
        t.restore_read_teams(&["web".to_string()]);
        t.mark_read_chat();
        assert!(!t.read_confined_to(Some("web")), "another chat's words ask everywhere");
    }
}
