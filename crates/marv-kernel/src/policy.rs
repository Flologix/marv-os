//! Rules: which of the person's agents may reach which (runtime-kernel phase 2b). New in marv-rs.
//!
//! A [`Policy`] says "agents under folder A (or chat A) may `send` (or `call`, or `read`) to agents under folder B (or
//! chat B)",
//! one way or, `mutual`, both, until it expires or is revoked. Only the person makes one (the desktop's Mesh panel,
//! `marv mesh allow`: later commits); they are kept in `~/.marv/mesh/policies.json` ([`crate::store`]).
//!
//! The mesh honors them where the session says so (a desktop chat, the TUI; never `marv -p`): a matching rule wires
//! two Marvs' main agents without the wire prompt (`marv_mesh::Switchboard::rule_grant`), and the wire admits only
//! while the rule is there and live (`Switchboard::authorize`).
//!
//! [`match_rule`] is pure: the subjects are absolute folder subtrees (component-wise, so `/a` covers `/a/b/c` but not
//! `/ab`) and chat ids; no globs, no prefixes. Default deny: no rule, a rule for another mode, a sibling folder, an
//! expired rule, a subagent or a shim plug all match nothing.
//!
//! `read` (runtime-kernel §9.2, `chat_read`): "may read each other's chats". Its own mode, off by default: a `send`
//! rule never allows a read and a `read` rule never wires a message (the mesh issues wires for `send` and `call` only).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::identity::{AgentId, AgentKind, AgentRef, FolderScope, ScopeError, is_canonical};
use crate::mode::Mode;

/// A rule's id: given by the store, never reused in one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PolicyId(pub u64);

impl std::fmt::Display for PolicyId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rule {}", self.0)
    }
}

/// One end of a rule.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Subject {
    /// Every Marv working in this subtree ("agents under ~/Projects/acme").
    Folder(FolderScope),
    /// One desktop chat.
    Agent(AgentId),
}

impl Subject {
    /// Whether `agent` is covered by this end. A folder covers an agent whose folder is inside it, compared
    /// component-wise and only for an absolute, canonical folder (one with `..` in it is never covered).
    pub fn covers(&self, agent: &AgentRef) -> bool {
        match self {
            Subject::Folder(scope) => is_canonical(&agent.folder) && scope.contains(&agent.folder),
            Subject::Agent(id) => agent.id.as_ref() == Some(id),
        }
    }

    /// The subtree, for a folder subject.
    pub fn scope(&self) -> Option<&FolderScope> {
        match self {
            Subject::Folder(scope) => Some(scope),
            Subject::Agent(_) => None,
        }
    }
}

/// Who made a rule: always the person, in one of Marv's own interfaces. There is no variant for a model, a mesh
/// message, a project config or yolo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleBy {
    /// The desktop's Mesh panel.
    Desktop,
    /// `marv mesh allow`, answered y on a terminal.
    Cli,
    /// The TUI.
    Tui,
}

/// One rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub id: PolicyId,
    pub from: Subject,
    pub to: Subject,
    /// `send`, `call` or `read`; never `watch` ([`Policy::check`]).
    pub mode: Mode,
    /// "May send to each other": the rule covers both directions.
    #[serde(default)]
    pub mutual: bool,
    /// Valid while `now < expires_at_ms`; none: until revoked.
    #[serde(default)]
    pub expires_at_ms: Option<u64>,
    pub by: RuleBy,
    pub created_at_ms: u64,
    /// What the person called it (shown in the UI).
    #[serde(default)]
    pub label: String,
}

/// Why a rule can't be kept.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuleError {
    #[error("{0}: rules cover send, call or read, never watch")]
    Mode(PolicyId),
    #[error("{0}: {1}")]
    Scope(PolicyId, ScopeError),
}

impl Policy {
    /// What a stored rule must be before it is matched: a `send`, `call` or `read` mode, and folder subjects that aren't the
    /// home folder or above it.
    pub fn check(&self, home: &Path) -> Result<(), RuleError> {
        if self.mode == Mode::Watch {
            return Err(RuleError::Mode(self.id));
        }
        for scope in [self.from.scope(), self.to.scope()].into_iter().flatten() {
            scope.check_home(home).map_err(|e| RuleError::Scope(self.id, e))?;
        }
        Ok(())
    }

    /// Whether the rule is in force at `now_ms` (expiry is exclusive: at `expires_at_ms` it has expired).
    pub fn live_at(&self, now_ms: u64) -> bool {
        self.expires_at_ms.is_none_or(|end| now_ms < end)
    }

    /// Whether the rule covers `from` reaching `to` (either way when `mutual`), whatever its mode and expiry.
    fn covers(&self, from: &AgentRef, to: &AgentRef) -> Option<Direction> {
        if self.from.covers(from) && self.to.covers(to) {
            Some(Direction::Forward)
        } else if self.mutual && self.to.covers(from) && self.from.covers(to) {
            Some(Direction::Back)
        } else {
            None
        }
    }

    /// The subject `to` was covered by, for a match in `direction`.
    pub fn target(&self, direction: Direction) -> &Subject {
        match direction {
            Direction::Forward => &self.to,
            Direction::Back => &self.from,
        }
    }
}

/// Which way a rule matched: as written, or (`mutual`) the other way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Back,
}

/// The rules as one version of the file had them. Every change to the file (a rule made or revoked) bumps the
/// generation, so a capability issued under an older one is re-checked ([`crate::Capability::still_valid`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PolicySnapshot {
    pub generation: u64,
    pub rules: Vec<Policy>,
}

/// One question for the rules: may `from` reach `to` in `mode` at `now_ms`?
#[derive(Debug, Clone, Copy)]
pub struct Access<'a> {
    pub from: &'a AgentRef,
    pub to: &'a AgentRef,
    pub mode: Mode,
    pub now_ms: u64,
}

/// Why the rules don't allow an access. Default deny: [`Denied::NoRule`] when nothing matches.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Denied {
    #[error("no rule allows it")]
    NoRule,
    #[error("the rule that allowed it has expired")]
    Expired,
    #[error("the rule that allowed it was changed or revoked")]
    Revoked,
    #[error("an agent can't be given a rule to itself")]
    SelfWire,
    /// Rules never cover this end or mode (a subagent, a shim plug, `watch`).
    #[error("rules don't cover {0}")]
    NotCoverable(&'static str),
    /// A delegation asked for more than the capability holds.
    #[error("a delegated capability can't be wider than its own ({0})")]
    Wider(&'static str),
}

/// A rule that matched, and which way.
#[derive(Debug, Clone, Copy)]
pub struct Matched<'a> {
    pub rule: &'a Policy,
    pub direction: Direction,
}

/// The rule that allows `access`, if any: pure, no I/O. The newest live rule that covers both ends in the access's
/// mode wins. Default deny: none matches ⇒ [`Denied::NoRule`] ([`Denied::Expired`] when only expired rules would have).
pub fn match_rule<'a>(snapshot: &'a PolicySnapshot, access: &Access<'_>) -> Result<Matched<'a>, Denied> {
    for end in [access.from, access.to] {
        match end.kind {
            AgentKind::Chat | AgentKind::Terminal => {}
            AgentKind::Subagent => return Err(Denied::NotCoverable("a subagent")),
            AgentKind::Shim => return Err(Denied::NotCoverable("a shim plug")),
        }
    }
    if access.mode == Mode::Watch {
        return Err(Denied::NotCoverable("watch"));
    }
    if access.from.same_agent(access.to) {
        return Err(Denied::SelfWire);
    }
    let mut expired = false;
    let mut best: Option<Matched<'a>> = None;
    for rule in snapshot.rules.iter().filter(|r| r.mode == access.mode) {
        let Some(direction) = rule.covers(access.from, access.to) else { continue };
        if !rule.live_at(access.now_ms) {
            expired = true;
            continue;
        }
        let newer = best.is_none_or(|b| (rule.created_at_ms, rule.id) > (b.rule.created_at_ms, b.rule.id));
        if newer {
            best = Some(Matched { rule, direction });
        }
    }
    best.ok_or(if expired { Denied::Expired } else { Denied::NoRule })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::PathBuf;

    pub(crate) const HOME: &str = "/home/me";

    pub(crate) fn scope(path: &str) -> FolderScope {
        FolderScope::new(Path::new(path), Path::new(HOME)).unwrap()
    }

    pub(crate) fn chat(name: &str, folder: &str) -> AgentRef {
        // A UUID-shaped id per name, so two chats never share one.
        let hex: String = name.bytes().map(|b| format!("{b:02x}")).collect::<String>();
        let id = format!("{:0<8}-0000-4000-8000-000000000000", &hex[..hex.len().min(8)]);
        AgentRef { id: AgentId::new(&id), mesh_name: name.into(), folder: PathBuf::from(folder), kind: AgentKind::Chat }
    }

    pub(crate) fn rule(id: u64, from: Subject, to: Subject, mode: Mode) -> Policy {
        Policy {
            id: PolicyId(id),
            from,
            to,
            mode,
            mutual: false,
            expires_at_ms: None,
            by: RuleBy::Desktop,
            created_at_ms: id,
            label: String::new(),
        }
    }

    pub(crate) fn folder(path: &str) -> Subject {
        Subject::Folder(scope(path))
    }

    pub(crate) fn snapshot(rules: Vec<Policy>) -> PolicySnapshot {
        PolicySnapshot { generation: 1, rules }
    }

    fn may(s: &PolicySnapshot, from: &AgentRef, to: &AgentRef, mode: Mode, now_ms: u64) -> Result<PolicyId, Denied> {
        match_rule(s, &Access { from, to, mode, now_ms }).map(|m| m.rule.id)
    }

    #[test]
    fn a_folder_rule_covers_its_whole_subtree_and_nothing_beside_it() {
        let s = snapshot(vec![rule(1, folder("/home/me/a"), folder("/home/me/a"), Mode::Send)]);
        let top = chat("top", "/home/me/a");
        let deep = chat("deep", "/home/me/a/b/c");
        let lookalike = chat("lookalike", "/home/me/ab");
        let sibling = chat("sibling", "/home/me/b");
        assert_eq!(may(&s, &top, &deep, Mode::Send, 0), Ok(PolicyId(1)), "/a covers /a/b/c");
        assert_eq!(may(&s, &deep, &top, Mode::Send, 0), Ok(PolicyId(1)), "both ends inside: either way");
        assert_eq!(may(&s, &top, &lookalike, Mode::Send, 0), Err(Denied::NoRule), "/a doesn't cover /ab");
        assert_eq!(may(&s, &lookalike, &top, Mode::Send, 0), Err(Denied::NoRule));
        assert_eq!(may(&s, &top, &sibling, Mode::Send, 0), Err(Denied::NoRule), "a sibling folder");
        let dotted = chat("dotted", "/home/me/a/../b");
        assert_eq!(may(&s, &top, &dotted, Mode::Send, 0), Err(Denied::NoRule), "a folder with .. is never covered");
    }

    #[test]
    fn a_rule_is_one_way_unless_mutual_and_works_across_spaces() {
        // Two spaces: ~/Work/acme and ~/Personal/notes.
        let acme = chat("acme", "/home/me/Work/acme/api");
        let notes = chat("notes", "/home/me/Personal/notes");
        let one_way = snapshot(vec![rule(1, folder("/home/me/Work/acme"), folder("/home/me/Personal"), Mode::Send)]);
        assert_eq!(may(&one_way, &acme, &notes, Mode::Send, 0), Ok(PolicyId(1)), "from and to in different spaces");
        assert_eq!(may(&one_way, &notes, &acme, Mode::Send, 0), Err(Denied::NoRule), "directed");
        let mut both = rule(2, folder("/home/me/Work/acme"), folder("/home/me/Personal"), Mode::Send);
        both.mutual = true;
        let mutual = snapshot(vec![both]);
        assert_eq!(may(&mutual, &acme, &notes, Mode::Send, 0), Ok(PolicyId(2)));
        assert_eq!(may(&mutual, &notes, &acme, Mode::Send, 0), Ok(PolicyId(2)), "mutual: the other way too");
    }

    #[test]
    fn a_chat_rule_names_one_chat() {
        let a = chat("alpha", "/home/me/x");
        let b = chat("bravo", "/home/me/y");
        let other = chat("charlie", "/home/me/y");
        let s = snapshot(vec![rule(1, Subject::Agent(a.id.clone().unwrap()), Subject::Agent(b.id.clone().unwrap()), Mode::Send)]);
        assert_eq!(may(&s, &a, &b, Mode::Send, 0), Ok(PolicyId(1)));
        assert_eq!(may(&s, &a, &other, Mode::Send, 0), Err(Denied::NoRule), "another chat in the same folder");
        let terminal = AgentRef { id: None, kind: AgentKind::Terminal, ..a.clone() };
        let terminal = AgentRef { mesh_name: "api-3f9a".into(), ..terminal };
        assert_eq!(may(&s, &terminal, &b, Mode::Send, 0), Err(Denied::NoRule), "a terminal Marv has no chat id");
    }

    #[test]
    fn expiry_is_exclusive() {
        let a = chat("alpha", "/home/me/a/x");
        let b = chat("bravo", "/home/me/a/y");
        let mut r = rule(1, folder("/home/me/a"), folder("/home/me/a"), Mode::Send);
        r.expires_at_ms = Some(1_000);
        let s = snapshot(vec![r]);
        assert_eq!(may(&s, &a, &b, Mode::Send, 999), Ok(PolicyId(1)), "just before");
        assert_eq!(may(&s, &a, &b, Mode::Send, 1_000), Err(Denied::Expired), "at");
        assert_eq!(may(&s, &a, &b, Mode::Send, 1_001), Err(Denied::Expired), "after");
    }

    #[test]
    fn default_deny() {
        let a = chat("alpha", "/home/me/a/x");
        let b = chat("bravo", "/home/me/a/y");
        assert_eq!(may(&PolicySnapshot::default(), &a, &b, Mode::Send, 0), Err(Denied::NoRule), "empty store");
        let send_only = snapshot(vec![rule(1, folder("/home/me/a"), folder("/home/me/a"), Mode::Send)]);
        assert_eq!(may(&send_only, &a, &b, Mode::Call, 0), Err(Denied::NoRule), "a rule for another mode");
        let sibling = snapshot(vec![rule(1, folder("/home/me/a"), folder("/home/me/b"), Mode::Send)]);
        assert_eq!(may(&sibling, &a, &b, Mode::Send, 0), Err(Denied::NoRule), "a rule for a sibling folder");
        assert_eq!(may(&send_only, &a, &a, Mode::Send, 0), Err(Denied::SelfWire), "never to itself");
        assert_eq!(may(&send_only, &a, &b, Mode::Watch, 0), Err(Denied::NotCoverable("watch")));
        let worker = AgentRef { kind: AgentKind::Subagent, mesh_name: "worker-1".into(), id: None, ..a.clone() };
        assert!(matches!(may(&send_only, &worker, &b, Mode::Send, 0), Err(Denied::NotCoverable(_))), "a subagent");
        let shim = AgentRef { kind: AgentKind::Shim, mesh_name: "claude".into(), id: None, ..b.clone() };
        assert!(matches!(may(&send_only, &a, &shim, Mode::Send, 0), Err(Denied::NotCoverable(_))), "a shim plug");
    }

    /// Runtime-kernel §9.2 (R2): `read` is its own mode. A `send` rule (even mutual) never allows a read, and a `read`
    /// rule never a send; a read rule matches only within its folders, either way only when mutual.
    #[test]
    fn read_is_its_own_mode_never_implied_by_send() {
        let a = chat("alpha", "/home/me/a/x");
        let b = chat("bravo", "/home/me/a/y");
        let outside = chat("other", "/home/me/b");
        let mut send = rule(1, folder("/home/me/a"), folder("/home/me/a"), Mode::Send);
        send.mutual = true;
        assert_eq!(may(&snapshot(vec![send.clone()]), &a, &b, Mode::Read, 0), Err(Denied::NoRule), "send doesn't imply read");
        let read = rule(2, folder("/home/me/a/x"), folder("/home/me/a/y"), Mode::Read);
        let s = snapshot(vec![send, read]);
        assert_eq!(may(&s, &a, &b, Mode::Read, 0), Ok(PolicyId(2)));
        assert_eq!(may(&s, &b, &a, Mode::Read, 0), Err(Denied::NoRule), "one way unless mutual");
        assert_eq!(may(&s, &a, &outside, Mode::Read, 0), Err(Denied::NoRule), "only within its folders");
        assert_eq!(may(&s, &a, &b, Mode::Send, 0), Ok(PolicyId(1)), "the send rule still sends");
        let only_read = snapshot(vec![rule(3, folder("/home/me/a"), folder("/home/me/a"), Mode::Read)]);
        assert_eq!(may(&only_read, &a, &b, Mode::Send, 0), Err(Denied::NoRule), "read never implies send");
    }

    #[test]
    fn the_newest_live_rule_wins() {
        let a = chat("alpha", "/home/me/a/x");
        let b = chat("bravo", "/home/me/a/y");
        let mut old = rule(1, folder("/home/me/a"), folder("/home/me/a"), Mode::Send);
        old.created_at_ms = 10;
        let mut new = rule(2, folder("/home/me/a/x"), folder("/home/me/a"), Mode::Send);
        new.created_at_ms = 20;
        assert_eq!(may(&snapshot(vec![new.clone(), old.clone()]), &a, &b, Mode::Send, 0), Ok(PolicyId(2)));
        new.expires_at_ms = Some(5);
        assert_eq!(may(&snapshot(vec![new, old]), &a, &b, Mode::Send, 6), Ok(PolicyId(1)), "an expired one doesn't");
    }

    #[test]
    fn a_stored_rule_is_send_call_or_read_and_never_home_or_above() {
        let home = Path::new(HOME);
        assert!(rule(1, folder("/home/me/a"), folder("/home/me/b"), Mode::Send).check(home).is_ok());
        assert!(rule(1, folder("/home/me/a"), folder("/home/me/b"), Mode::Call).check(home).is_ok());
        assert!(rule(1, folder("/home/me/a"), folder("/home/me/b"), Mode::Read).check(home).is_ok());
        assert_eq!(
            rule(1, folder("/home/me/a"), folder("/home/me/b"), Mode::Watch).check(home),
            Err(RuleError::Mode(PolicyId(1)))
        );
        // A subject read from a file was only checked for being canonical.
        let read: Subject = serde_json::from_value(serde_json::json!({"folder": "/home"})).unwrap();
        assert!(matches!(rule(1, folder("/home/me/a"), read, Mode::Send).check(home), Err(RuleError::Scope(..))));
        assert!(serde_json::from_value::<Subject>(serde_json::json!({"folder": "/home/me/../x"})).is_err());
        assert!(serde_json::from_value::<Subject>(serde_json::json!({"agent": "../../etc"})).is_err());
        assert!(serde_json::from_value::<Subject>(serde_json::json!({"glob": "/home/me/*"})).is_err());
    }
}
