//! Meters: what each chat, folder, grant and core used, by (UTC) day (runtime-kernel §16.5, decision A5). New in
//! marv-rs.
//!
//! One [`TurnUsage`] per turn goes into the event log ([`crate::KernelEvent::Usage`]): its requests, tokens in, cached
//! and out, its cost (reported or estimated, or none for an unpriced core), wall time, the core, and whether a person
//! or a wake started it. **Numbers and ids only:** the chat's id, its folder, the core's `provider/model` name and
//! the grant's id are the only strings, and no field could hold a message's, a reply's or a tool's words. A plan core
//! also records the quota its provider's headers told ([`PlanQuota`]).
//!
//! [`Meters`] folds them, by day, into a meter per chat, per folder, per grant, per core and one for everything.
//! A turn's `id` is counted once however often a log is read again (another process's turns are read back from the
//! shared log; this one's own are in it too). Pure: time is passed in.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::grant::{GrantId, day_of};

/// Who started a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartedBy {
    /// The person typed (or ran a command that asks the model).
    Person,
    /// A wake: a mesh message, the radio. Nobody is watching it.
    Wake,
}

/// A plan's quota as its provider's headers last told it (numbers only).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanQuota {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_percent: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_at_ms: Option<u64>,
}

/// One turn's use, as logged. See the module doc: numbers and ids only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnUsage {
    /// The turn's own id (random): counted once however often the log is read.
    pub id: u64,
    /// The chat: a desktop chat's id, else the saved session's.
    pub chat: String,
    /// The folder the chat works in.
    pub folder: PathBuf,
    /// The chat's core, `provider/model`.
    pub core: String,
    /// The service grant the turn ran under, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<GrantId>,
    pub requests: u32,
    pub tokens_in: u64,
    pub cached: u64,
    pub out: u64,
    /// Millionths of a dollar; none when no request had a price (a plan's, a local model's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_micros: Option<u64>,
    /// Some of the cost was estimated from prices rather than reported.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub estimated: bool,
    /// The tokens of requests that had no price (what an unpriced budget counts).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unpriced_tokens: u64,
    pub wall_ms: u64,
    pub started_by: StartedBy,
    /// Other cores the turn's requests ran on (a subagent's type's, the small model), `provider/model`, each with
    /// its share. The meter per core is charged from these; the chat's core gets the rest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cores: Vec<CoreShare>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota: Option<PlanQuota>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// What a turn's requests on one other core used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreShare {
    pub core: String,
    pub requests: u32,
    pub tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_micros: Option<u64>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unpriced_tokens: u64,
}

/// Which meter.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MeterScope {
    /// Everything (the global budget's).
    All,
    Folder(PathBuf),
    Chat(String),
    Grant(GrantId),
    Core(String),
}

/// What a meter counted on one day.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Metered {
    pub turns: u64,
    pub requests: u64,
    pub tokens_in: u64,
    pub cached: u64,
    pub out: u64,
    /// Millionths of a dollar, priced requests only.
    pub usd_micros: u64,
    /// Tokens of unpriced requests.
    pub unpriced_tokens: u64,
    pub estimated: bool,
    pub wall_ms: u64,
}

impl Metered {
    fn add(&mut self, other: &Metered) {
        // Saturating throughout: a log anyone with the user's rights could edit holds any numbers.
        self.turns = self.turns.saturating_add(other.turns);
        self.requests = self.requests.saturating_add(other.requests);
        self.tokens_in = self.tokens_in.saturating_add(other.tokens_in);
        self.cached = self.cached.saturating_add(other.cached);
        self.out = self.out.saturating_add(other.out);
        self.usd_micros = self.usd_micros.saturating_add(other.usd_micros);
        self.unpriced_tokens = self.unpriced_tokens.saturating_add(other.unpriced_tokens);
        self.estimated |= other.estimated;
        self.wall_ms = self.wall_ms.saturating_add(other.wall_ms);
    }

    /// The whole turn as one meter's line.
    pub fn of(usage: &TurnUsage) -> Self {
        Self {
            turns: 1,
            requests: u64::from(usage.requests),
            tokens_in: usage.tokens_in,
            cached: usage.cached,
            out: usage.out,
            usd_micros: usage.cost_micros.unwrap_or(0),
            unpriced_tokens: usage.unpriced_tokens,
            estimated: usage.estimated,
            wall_ms: usage.wall_ms,
        }
    }
}

/// The meters, by day. See the module doc.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Meters {
    by: BTreeMap<(u64, MeterScope), Metered>,
    seen: BTreeSet<u64>,
    /// Each plan core's quota as last told, and when.
    quotas: BTreeMap<String, (u64, PlanQuota)>,
}

impl Meters {
    /// Counts a turn logged at `at_ms` on its day. `false` (and nothing counted) when its id was counted already.
    pub fn record(&mut self, at_ms: u64, usage: &TurnUsage) -> bool {
        if !self.seen.insert(usage.id) {
            return false;
        }
        let day = day_of(at_ms);
        let whole = Metered::of(usage);
        let mut add = |scope: MeterScope, m: &Metered| self.by.entry((day, scope)).or_default().add(m);
        add(MeterScope::All, &whole);
        add(MeterScope::Folder(usage.folder.clone()), &whole);
        add(MeterScope::Chat(usage.chat.clone()), &whole);
        if let Some(grant) = usage.grant {
            add(MeterScope::Grant(grant), &whole);
        }
        // Per core: each other core its share, the chat's core the rest.
        let mut rest = whole;
        for share in &usage.cores {
            let part = Metered {
                turns: 0,
                requests: u64::from(share.requests),
                tokens_in: share.tokens,
                usd_micros: share.cost_micros.unwrap_or(0),
                unpriced_tokens: share.unpriced_tokens,
                ..Metered::default()
            };
            rest.requests = rest.requests.saturating_sub(part.requests);
            rest.tokens_in = rest.tokens_in.saturating_sub(part.tokens_in);
            rest.usd_micros = rest.usd_micros.saturating_sub(part.usd_micros);
            rest.unpriced_tokens = rest.unpriced_tokens.saturating_sub(part.unpriced_tokens);
            add(MeterScope::Core(share.core.clone()), &part);
        }
        add(MeterScope::Core(usage.core.clone()), &rest);
        if let Some(quota) = usage.quota {
            let newer = self.quotas.get(&usage.core).is_none_or(|(at, _)| *at <= at_ms);
            if newer {
                self.quotas.insert(usage.core.clone(), (at_ms, quota));
            }
        }
        true
    }

    /// Whether the turn `id` is counted.
    pub fn has(&self, id: u64) -> bool {
        self.seen.contains(&id)
    }

    /// One meter on `day`.
    pub fn get(&self, day: u64, scope: &MeterScope) -> Metered {
        self.by.get(&(day, scope.clone())).copied().unwrap_or_default()
    }

    /// Every chat folder inside `folder` (it, or below it) on `day`, summed: a folder's budget covers its subfolders'
    /// chats.
    pub fn folder_within(&self, day: u64, folder: &Path) -> Metered {
        let mut sum = Metered::default();
        for ((d, scope), m) in &self.by {
            if let (true, MeterScope::Folder(f)) = (*d == day, scope)
                && f.starts_with(folder)
            {
                sum.add(m);
            }
        }
        sum
    }

    /// The meters of one kind on `day` (`kind` picks: chats, folders, grants or cores), largest spend first.
    pub fn on_day(&self, day: u64, kind: fn(&MeterScope) -> bool) -> Vec<(MeterScope, Metered)> {
        let mut rows: Vec<(MeterScope, Metered)> =
            self.by.iter().filter(|((d, s), _)| *d == day && kind(s)).map(|((_, s), m)| (s.clone(), *m)).collect();
        rows.sort_by_key(|r| std::cmp::Reverse((r.1.usd_micros, r.1.unpriced_tokens)));
        rows
    }

    /// The days with anything counted, oldest first.
    pub fn days(&self) -> Vec<u64> {
        let days: BTreeSet<u64> = self.by.keys().map(|(d, _)| *d).collect();
        days.into_iter().collect()
    }

    /// A plan core's quota as last told (when, and what).
    pub fn quota(&self, core: &str) -> Option<(u64, PlanQuota)> {
        self.quotas.get(core).copied()
    }

    /// Every plan core's last quota.
    pub fn quotas(&self) -> impl Iterator<Item = (&String, &(u64, PlanQuota))> {
        self.quotas.iter()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::grant::DAY_MS;

    pub(crate) fn turn(id: u64, chat: &str, folder: &str, cost: Option<u64>, tokens: u64) -> TurnUsage {
        TurnUsage {
            id,
            chat: chat.into(),
            folder: folder.into(),
            core: "openrouter/big".into(),
            grant: None,
            requests: 2,
            tokens_in: tokens,
            cached: 0,
            out: 10,
            cost_micros: cost,
            estimated: false,
            unpriced_tokens: if cost.is_none() { tokens + 10 } else { 0 },
            wall_ms: 100,
            started_by: StartedBy::Person,
            cores: Vec::new(),
            quota: None,
        }
    }

    #[test]
    fn a_turn_is_counted_once_on_every_meter_of_its_day() {
        let mut meters = Meters::default();
        let mut t = turn(1, "a", "/p/acme/api", Some(1_000), 500);
        t.grant = Some(GrantId(7));
        t.cores = vec![CoreShare { core: "zai/glm".into(), requests: 1, tokens: 200, cost_micros: None, unpriced_tokens: 210 }];
        assert!(meters.record(5, &t));
        assert!(!meters.record(5, &t), "the same turn read again");
        meters.record(6, &turn(2, "b", "/p/acme/web", Some(500), 100));
        meters.record(DAY_MS + 1, &turn(3, "a", "/p/acme/api", Some(9_000), 100));
        assert_eq!(meters.get(0, &MeterScope::All).usd_micros, 1_500);
        assert_eq!(meters.get(0, &MeterScope::Chat("a".into())).turns, 1);
        assert_eq!(meters.get(0, &MeterScope::Grant(GrantId(7))).usd_micros, 1_000);
        assert_eq!(meters.folder_within(0, Path::new("/p/acme")).usd_micros, 1_500, "the folder holds both chats");
        assert_eq!(meters.folder_within(0, Path::new("/p/acme/api")).usd_micros, 1_000);
        assert_eq!(meters.folder_within(0, Path::new("/p/acm")).usd_micros, 0, "a path prefix isn't a folder");
        assert_eq!(meters.get(0, &MeterScope::Core("zai/glm".into())).unpriced_tokens, 210);
        assert_eq!(meters.get(0, &MeterScope::Core("openrouter/big".into())).requests, 1 + 2, "the rest is the chat's");
        assert_eq!(meters.get(1, &MeterScope::All).usd_micros, 9_000, "the next day starts again");
        assert_eq!(meters.days(), [0, 1]);
    }
}
