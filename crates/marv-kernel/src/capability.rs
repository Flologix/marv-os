//! Capabilities: what a rule allows one agent, as an in-process record (runtime-kernel phase 2b). New in marv-rs.
//!
//! A [`Capability`] is an authorization, not a bearer secret (decision D9): opaque (its fields are private), never
//! serialized, never sent to another process. It names the rule and the generation of the rules it was issued under,
//! and is re-validated on use ([`Capability::still_valid`]): a revoked or changed rule set (a new generation) or a
//! passed expiry refuses it. [`Capability::delegate`] only narrows (the same mode, a subtree of its scope, an expiry no
//! later than its own), so a delegated capability can never re-delegate wider either.
//!
//! Not used by the mesh yet: a rule's wire is re-checked against the rules themselves (`Switchboard::authorize`). A
//! capability is for delegation (a later phase).

use crate::identity::{AgentRef, FolderScope};
use crate::mode::Mode;
use crate::policy::{Access, Denied, PolicyId, PolicySnapshot, match_rule};

/// How deep a chain a rule-issued capability's messages may wake: a message at this hop or more never wakes (the
/// radio's `MAX_WAKE_HOP`).
pub const RULE_HOP_LIMIT: u32 = 2;

/// Where a capability came from: a stored rule, under one generation of the rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Rule { id: PolicyId, generation: u64 },
}

/// What a rule allows one agent: `from` may reach `to` in `mode`, until `expires_at_ms`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability {
    via: Via,
    /// The holder's mesh name.
    from: String,
    /// The target's mesh name.
    to: String,
    mode: Mode,
    /// The rule's subtree on the target's side (a folder rule), for narrowing.
    scope: Option<FolderScope>,
    expires_at_ms: Option<u64>,
    hop_limit: u32,
    delegated: bool,
}

/// What a delegation may keep: the same mode, a scope inside the capability's, an expiry no later than its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Narrowing {
    pub mode: Mode,
    /// `None`: the capability's own scope.
    pub scope: Option<FolderScope>,
    /// `None`: the capability's own expiry.
    pub until_ms: Option<u64>,
}

impl Capability {
    /// The capability the rules give `access`, if one does ([`match_rule`]; default deny).
    pub fn issue(snapshot: &PolicySnapshot, access: &Access<'_>) -> Result<Self, Denied> {
        let matched = match_rule(snapshot, access)?;
        Ok(Self {
            via: Via::Rule { id: matched.rule.id, generation: snapshot.generation },
            from: access.from.mesh_name.clone(),
            to: access.to.mesh_name.clone(),
            mode: access.mode,
            scope: matched.rule.target(matched.direction).scope().cloned(),
            expires_at_ms: matched.rule.expires_at_ms,
            hop_limit: RULE_HOP_LIMIT,
            delegated: false,
        })
    }

    pub fn via(&self) -> Via {
        self.via
    }
    pub fn from(&self) -> &str {
        &self.from
    }
    pub fn to(&self) -> &str {
        &self.to
    }
    pub fn mode(&self) -> Mode {
        self.mode
    }
    pub fn scope(&self) -> Option<&FolderScope> {
        self.scope.as_ref()
    }
    pub fn expires_at_ms(&self) -> Option<u64> {
        self.expires_at_ms
    }
    pub fn hop_limit(&self) -> u32 {
        self.hop_limit
    }
    pub fn delegated(&self) -> bool {
        self.delegated
    }

    /// Whether it still holds under `snapshot` at `now_ms`: the rules are the generation it was issued under (any
    /// change, a revocation above all, makes a new one: the access must be matched again), its rule is still there,
    /// and it hasn't expired.
    pub fn still_valid(&self, snapshot: &PolicySnapshot, now_ms: u64) -> Result<(), Denied> {
        let Via::Rule { id, generation } = self.via;
        if snapshot.generation != generation || !snapshot.rules.iter().any(|r| r.id == id) {
            return Err(Denied::Revoked);
        }
        if self.expires_at_ms.is_some_and(|end| now_ms >= end) {
            return Err(Denied::Expired);
        }
        Ok(())
    }

    /// A narrower capability for `to` (a delegate acting for the holder): never a new mode, never a wider scope,
    /// never a later expiry. Asking for more is refused, not clamped.
    pub fn delegate(&self, to: &AgentRef, narrowing: Narrowing) -> Result<Capability, Denied> {
        if narrowing.mode != self.mode {
            return Err(Denied::Wider("mode"));
        }
        let scope = match (&self.scope, narrowing.scope) {
            (own, None) => own.clone(),
            (Some(own), Some(asked)) if own.contains_scope(&asked) => Some(asked),
            _ => return Err(Denied::Wider("scope")),
        };
        let expires_at_ms = match (self.expires_at_ms, narrowing.until_ms) {
            (own, None) => own,
            (None, Some(asked)) => Some(asked),
            (Some(own), Some(asked)) if asked <= own => Some(asked),
            _ => return Err(Denied::Wider("expiry")),
        };
        Ok(Capability {
            via: self.via,
            from: to.mesh_name.clone(),
            to: self.to.clone(),
            mode: self.mode,
            scope,
            expires_at_ms,
            hop_limit: self.hop_limit,
            delegated: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::tests::{chat, folder, rule, scope, snapshot};

    fn issued(expires: Option<u64>) -> (PolicySnapshot, Capability) {
        let mut r = rule(7, folder("/home/me/a"), folder("/home/me/b"), Mode::Send);
        r.expires_at_ms = expires;
        let s = snapshot(vec![r]);
        let (from, to) = (chat("alpha", "/home/me/a/x"), chat("bravo", "/home/me/b/y"));
        let cap = Capability::issue(&s, &Access { from: &from, to: &to, mode: Mode::Send, now_ms: 0 }).unwrap();
        (s, cap)
    }

    #[test]
    fn issued_from_the_matching_rule_with_its_scope_and_expiry() {
        let (_, cap) = issued(Some(500));
        assert_eq!(cap.via(), Via::Rule { id: PolicyId(7), generation: 1 });
        assert_eq!((cap.from(), cap.to(), cap.mode()), ("alpha", "bravo", Mode::Send));
        assert_eq!(cap.scope(), Some(&scope("/home/me/b")));
        assert_eq!((cap.expires_at_ms(), cap.hop_limit(), cap.delegated()), (Some(500), RULE_HOP_LIMIT, false));
        let (from, to) = (chat("alpha", "/home/me/a/x"), chat("bravo", "/home/me/b/y"));
        let none = Capability::issue(&PolicySnapshot::default(), &Access { from: &from, to: &to, mode: Mode::Send, now_ms: 0 });
        assert_eq!(none, Err(Denied::NoRule), "default deny");
    }

    #[test]
    fn revocation_bumps_the_generation_and_the_next_check_fails() {
        let (mut s, cap) = issued(None);
        assert_eq!(cap.still_valid(&s, 1), Ok(()));
        // Revoked: the rule goes and the generation moves on.
        s.rules.clear();
        s.generation += 1;
        assert_eq!(cap.still_valid(&s, 1), Err(Denied::Revoked));
        // Any other change (another rule made) is a new generation too: matched again, never trusted as it was.
        let (mut s, cap) = issued(None);
        s.rules.push(rule(8, folder("/home/me/c"), folder("/home/me/d"), Mode::Send));
        s.generation += 1;
        assert_eq!(cap.still_valid(&s, 1), Err(Denied::Revoked));
    }

    #[test]
    fn still_valid_until_its_expiry() {
        let (s, cap) = issued(Some(1_000));
        assert_eq!(cap.still_valid(&s, 999), Ok(()));
        assert_eq!(cap.still_valid(&s, 1_000), Err(Denied::Expired));
        assert_eq!(cap.still_valid(&s, 1_001), Err(Denied::Expired));
    }

    #[test]
    fn delegation_only_narrows() {
        let (s, cap) = issued(Some(1_000));
        let worker = chat("worker", "/home/me/a/x");
        let same = Narrowing { mode: Mode::Send, scope: None, until_ms: None };
        let d = cap.delegate(&worker, same.clone()).unwrap();
        assert_eq!(
            (d.from(), d.to(), d.scope(), d.expires_at_ms(), d.delegated()),
            ("worker", "bravo", cap.scope(), Some(1_000), true)
        );
        assert_eq!(d.still_valid(&s, 10), Ok(()), "same rule, same generation");
        // Never a new mode, a wider or sibling scope, or a later expiry.
        assert_eq!(cap.delegate(&worker, Narrowing { mode: Mode::Call, ..same.clone() }), Err(Denied::Wider("mode")));
        for wider in ["/home/me/x", "/home/me/bc"] {
            let asked = Narrowing { scope: Some(scope(wider)), ..same.clone() };
            assert_eq!(cap.delegate(&worker, asked), Err(Denied::Wider("scope")), "{wider}");
        }
        assert_eq!(cap.delegate(&worker, Narrowing { until_ms: Some(1_001), ..same.clone() }), Err(Denied::Wider("expiry")));
        // Narrower is fine.
        let narrow = Narrowing { mode: Mode::Send, scope: Some(scope("/home/me/b/y")), until_ms: Some(900) };
        let d = cap.delegate(&worker, narrow).unwrap();
        assert_eq!((d.scope(), d.expires_at_ms()), (Some(&scope("/home/me/b/y")), Some(900)));
        // And what was delegated can't re-delegate wider than itself.
        let sub = chat("sub", "/home/me/a/x");
        assert_eq!(d.delegate(&sub, Narrowing { scope: Some(scope("/home/me/b")), ..same.clone() }), Err(Denied::Wider("scope")));
        assert_eq!(d.delegate(&sub, Narrowing { until_ms: Some(1_000), ..same.clone() }), Err(Denied::Wider("expiry")));
        assert_eq!(d.delegate(&sub, Narrowing { until_ms: None, ..same.clone() }).unwrap().expires_at_ms(), Some(900));
        // One with no expiry may hand out a dated one.
        let (_, open) = issued(None);
        assert_eq!(open.delegate(&worker, Narrowing { until_ms: Some(5), ..same }).unwrap().expires_at_ms(), Some(5));
    }

    #[test]
    fn a_chat_rule_has_no_scope_to_narrow_into() {
        let (a, b) = (chat("alpha", "/home/me/a"), chat("bravo", "/home/me/b"));
        let s = snapshot(vec![rule(
            1,
            crate::Subject::Agent(a.id.clone().unwrap()),
            crate::Subject::Agent(b.id.clone().unwrap()),
            Mode::Send,
        )]);
        let cap = Capability::issue(&s, &Access { from: &a, to: &b, mode: Mode::Send, now_ms: 0 }).unwrap();
        assert_eq!(cap.scope(), None);
        let asked = Narrowing { mode: Mode::Send, scope: Some(scope("/home/me/b")), until_ms: None };
        assert_eq!(cap.delegate(&a, asked), Err(Denied::Wider("scope")));
    }
}
