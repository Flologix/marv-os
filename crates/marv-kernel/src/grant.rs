//! Service grants: the person's one-time permission for one chat to keep working while they're away. New in marv-rs.
//!
//! A turn the person didn't start (a rule's message woke the chat) runs with automatic approval off: every call that
//! would ask, asks, and waits. A [`Grant`] lets that one chat, in such a turn, run what yolo would run without asking
//! (sandboxed commands without network, edits outside `.git`, worktree subagents) and nothing past it. Everything else
//! still waits for the person.
//!
//! - **Per chat:** a grant names one chat id, never a folder: a folder's future chats would inherit power nobody
//!   looked at.
//! - **Made only by the person**, in the desktop ([`GrantBy`] has no other variant): no tool, mesh message, project
//!   config, env var or flag creates, widens or extends one.
//! - **Limits on every one:** an expiry (none: until revoked), and a spend cap a day ([`SpendCap`], $2 by default,
//!   tokens for turns that have no price). At the cap it pauses until the next (UTC) day or the person raises it.
//!
//! [`grant_for`] is pure: time and the day's spend are passed in. The file is [`crate::grant_store`].

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::identity::{AgentId, FolderScope, ScopeError};

/// A grant's id: given by the store, never reused in one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GrantId(pub u64);

impl std::fmt::Display for GrantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "grant {}", self.0)
    }
}

/// Who made a grant: always the person, in the desktop. There is no variant for a model, a mesh message, a project
/// config, the TUI or a flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantBy {
    /// The desktop: the rule dialog's "keep working while I'm away", or the chat's own switch.
    Desktop,
}

/// The default spend cap a day: $2.
pub const DEFAULT_DAILY_USD_MICROS: u64 = 2_000_000;
/// The default cap a day for turns that have no price (a plan's, a local model's): tokens, in and out.
pub const DEFAULT_DAILY_TOKENS: u64 = 1_000_000;
/// One day, for the spend's day (UTC).
pub const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// What a grant may spend a day: dollars where the turns have a price, tokens where they don't. Either reached
/// pauses the grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpendCap {
    /// Millionths of a dollar.
    pub usd_micros: u64,
    pub tokens: u64,
}

impl Default for SpendCap {
    fn default() -> Self {
        Self { usd_micros: DEFAULT_DAILY_USD_MICROS, tokens: DEFAULT_DAILY_TOKENS }
    }
}

/// One grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub id: GrantId,
    /// The one chat it covers.
    pub chat: AgentId,
    /// The chat's folder when the grant was made (shown in the Background list; never what the grant covers).
    pub folder: FolderScope,
    /// Valid while `now < expires_at_ms`; none: until revoked.
    #[serde(default)]
    pub expires_at_ms: Option<u64>,
    #[serde(default)]
    pub cap: SpendCap,
    pub by: GrantBy,
    pub created_at_ms: u64,
    /// What the person sees it as ("acme-api works while you're away").
    #[serde(default)]
    pub label: String,
}

impl Grant {
    /// What a stored grant must be: its folder never the home folder or above it.
    pub fn check(&self, home: &Path) -> Result<(), ScopeError> {
        self.folder.check_home(home)
    }

    /// Whether it is in force at `now_ms` (expiry is exclusive).
    pub fn live_at(&self, now_ms: u64) -> bool {
        self.expires_at_ms.is_none_or(|end| now_ms < end)
    }
}

/// The grants as one version of the file had them; every change bumps the generation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GrantSnapshot {
    pub generation: u64,
    pub grants: Vec<Grant>,
}

impl GrantSnapshot {
    /// The newest grant for `chat`, live or not.
    pub fn for_chat(&self, chat: &AgentId) -> Option<&Grant> {
        self.grants.iter().filter(|g| &g.chat == chat).max_by_key(|g| (g.created_at_ms, g.id))
    }
}

/// Runtime-kernel §14.1 (decision P1): whether a chat being granted to work in `folder` starts in its own git
/// worktree by default: when another granted chat works there too (in `others`: the folders of the other chats
/// granted with it and of those already granted). Two folders share files when one contains the other. The person's
/// own chats, which they drive, stay in the folder as today: this is asked only for a grant.
pub fn worktree_by_default<'a>(folder: &Path, others: impl IntoIterator<Item = &'a Path>) -> bool {
    others.into_iter().any(|other| other.starts_with(folder) || folder.starts_with(other))
}

/// The UTC day `now_ms` is in.
pub fn day_of(now_ms: u64) -> u64 {
    now_ms / DAY_MS
}

/// What a grant spent on one day.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spend {
    pub day: u64,
    pub usd_micros: u64,
    pub tokens: u64,
}

impl Spend {
    /// What was spent on `now_ms`'s day (nothing, if this is an earlier day's). A clock that went back (to an earlier
    /// day than this spend's) never drops it: the later day's spend still counts.
    pub fn today(&self, now_ms: u64) -> Spend {
        if self.day >= day_of(now_ms) { *self } else { Spend { day: day_of(now_ms), ..Spend::default() } }
    }

    /// Adds one request: its cost when it has one, else its tokens. A cost that isn't a finite, non-negative number
    /// counts its tokens instead.
    pub fn charge(&mut self, now_ms: u64, cost_usd: Option<f64>, tokens: u64) {
        *self = self.today(now_ms);
        match cost_usd.filter(|c| c.is_finite() && *c >= 0.0) {
            Some(cost) => self.usd_micros = self.usd_micros.saturating_add((cost * 1_000_000.0).ceil() as u64),
            None => self.tokens = self.tokens.saturating_add(tokens),
        }
    }

    /// Whether `cap` is reached on `now_ms`'s day.
    pub fn over(&self, cap: &SpendCap, now_ms: u64) -> bool {
        let today = self.today(now_ms);
        today.usd_micros >= cap.usd_micros || today.tokens >= cap.tokens
    }
}

/// Why a chat's unattended turn has no grant to run under. Default deny: [`GrantDenied::NoGrant`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GrantDenied {
    #[error("no grant covers this chat")]
    NoGrant,
    #[error("the grant has expired")]
    Expired,
    #[error("the grant reached its spend cap for today")]
    Spent,
}

/// The grant `chat`'s unattended turn runs under at `now_ms`, if any: pure, no I/O. `spent` is what each grant spent
/// (any day; only `now_ms`'s counts). The newest grant for the chat decides: expired or at its cap ⇒ none.
pub fn grant_for<'a>(
    snapshot: &'a GrantSnapshot,
    chat: &AgentId,
    now_ms: u64,
    spent: impl Fn(GrantId) -> Spend,
) -> Result<&'a Grant, GrantDenied> {
    let grant = snapshot.for_chat(chat).ok_or(GrantDenied::NoGrant)?;
    if !grant.live_at(now_ms) {
        return Err(GrantDenied::Expired);
    }
    if spent(grant.id).over(&grant.cap, now_ms) {
        return Err(GrantDenied::Spent);
    }
    Ok(grant)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::policy::tests::{HOME, scope};

    pub(crate) fn chat_id(n: u8) -> AgentId {
        AgentId::new(&format!("{n:08x}-0000-4000-8000-000000000000")).unwrap()
    }

    pub(crate) fn grant(id: u64, chat: u8) -> Grant {
        Grant {
            id: GrantId(id),
            chat: chat_id(chat),
            folder: scope("/home/me/acme"),
            expires_at_ms: None,
            cap: SpendCap::default(),
            by: GrantBy::Desktop,
            created_at_ms: id,
            label: String::new(),
        }
    }

    fn snapshot(grants: Vec<Grant>) -> GrantSnapshot {
        GrantSnapshot { generation: 1, grants }
    }

    #[test]
    fn a_grant_names_one_chat_and_default_deny() {
        let s = snapshot(vec![grant(1, 1)]);
        let none = |_| Spend::default();
        assert_eq!(grant_for(&s, &chat_id(1), 0, none).map(|g| g.id), Ok(GrantId(1)));
        assert_eq!(grant_for(&s, &chat_id(2), 0, none).map(|g| g.id), Err(GrantDenied::NoGrant), "another chat");
        assert_eq!(grant_for(&GrantSnapshot::default(), &chat_id(1), 0, none).map(|g| g.id), Err(GrantDenied::NoGrant));
    }

    #[test]
    fn expiry_is_exclusive_and_the_newest_grant_decides() {
        let mut g = grant(1, 1);
        g.expires_at_ms = Some(1_000);
        let s = snapshot(vec![g.clone()]);
        let none = |_| Spend::default();
        assert!(grant_for(&s, &chat_id(1), 999, none).is_ok());
        assert_eq!(grant_for(&s, &chat_id(1), 1_000, none).map(|g| g.id), Err(GrantDenied::Expired));
        // A newer, expired grant for the chat isn't rescued by an older live one.
        let mut newer = grant(2, 1);
        newer.expires_at_ms = Some(10);
        let s = snapshot(vec![grant(1, 1), newer]);
        assert_eq!(grant_for(&s, &chat_id(1), 20, none).map(|g| g.id), Err(GrantDenied::Expired));
    }

    #[test]
    fn spend_counts_dollars_or_tokens_by_the_day_and_the_cap_pauses() {
        let now = 5 * DAY_MS + 10;
        let mut spend = Spend::default();
        spend.charge(now, Some(1.5), 999_999);
        assert_eq!((spend.day, spend.usd_micros, spend.tokens), (5, 1_500_000, 0), "a priced request counts its cost");
        assert!(!spend.over(&SpendCap::default(), now));
        spend.charge(now, Some(0.5), 0);
        assert!(spend.over(&SpendCap::default(), now), "$2 reached");
        assert!(!spend.over(&SpendCap::default(), now + DAY_MS), "the next day starts again");
        let mut plan = Spend::default();
        plan.charge(now, None, DEFAULT_DAILY_TOKENS);
        assert!(plan.over(&SpendCap::default(), now), "a priceless turn counts tokens");
        let mut odd = Spend::default();
        odd.charge(now, Some(f64::NAN), 7);
        odd.charge(now, Some(-1.0), 3);
        assert_eq!((odd.usd_micros, odd.tokens), (0, 10), "a cost that isn't one counts the tokens");
        let mut back = Spend::default();
        back.charge(now, Some(2.0), 0);
        assert!(back.over(&SpendCap::default(), now - DAY_MS), "a clock gone back a day keeps today's spend");
        back.charge(now - DAY_MS, Some(0.1), 0);
        assert_eq!((back.day, back.usd_micros), (5, 2_100_000), "and charges onto it");

        let s = snapshot(vec![grant(1, 1)]);
        let spent = |_| Spend { day: 5, usd_micros: DEFAULT_DAILY_USD_MICROS, tokens: 0 };
        assert_eq!(grant_for(&s, &chat_id(1), now, spent).map(|g| g.id), Err(GrantDenied::Spent));
        assert!(grant_for(&s, &chat_id(1), now + DAY_MS, spent).is_ok(), "yesterday's spend doesn't count");
    }

    #[test]
    fn a_stored_grant_is_never_home_or_above_and_only_the_desktop_makes_one() {
        assert!(grant(1, 1).check(Path::new(HOME)).is_ok());
        let read: FolderScope = serde_json::from_value(serde_json::json!("/home")).unwrap();
        assert!(Grant { folder: read, ..grant(1, 1) }.check(Path::new(HOME)).is_err());
        for by in ["model", "mesh", "project", "tui", "cli", "env"] {
            assert!(serde_json::from_value::<GrantBy>(serde_json::json!(by)).is_err(), "{by}");
        }
    }

    /// P1: two or more granted chats sharing a folder (the same, or one inside the other) default to worktrees; a
    /// granted chat alone in its folder, or beside a sibling folder, doesn't.
    #[test]
    fn granted_chats_sharing_a_folder_default_to_worktrees() {
        let p = Path::new;
        assert!(!worktree_by_default(p("/home/me/acme"), []), "alone");
        assert!(worktree_by_default(p("/home/me/acme"), [p("/home/me/acme")]));
        assert!(worktree_by_default(p("/home/me/acme/api"), [p("/home/me/acme")]), "inside another's folder");
        assert!(worktree_by_default(p("/home/me/acme"), [p("/home/me/web"), p("/home/me/acme/api")]));
        assert!(!worktree_by_default(p("/home/me/acme"), [p("/home/me/acme-web"), p("/home/me/web")]), "component-wise");
    }
}
