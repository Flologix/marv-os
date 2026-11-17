//! Wires and the wiring table: who may reach which receptacle, how. New in
//! marv-rs (docs/MESH.md, "Words" and "Rules").
//!
//! A [`Wire`] is a grant from one plug to one receptacle, in one mode. The
//! [`WireTable`] holds them; every decision goes through `Switchboard::authorize` (runtime-kernel 2b), which asks the
//! table and nothing else:
//!
//! - **Default deny.** An empty table allows nothing; nothing is wired until the
//!   user wires it.
//! - **By name only.** A wire matches on the exact plug id, the exact
//!   receptacle name and the exact mode. No wildcards, no prefixes, no hosts or
//!   ports; a `call` wire grants no `send`, a `send` wire no `watch`.
//! - **No self-wiring.** A plug can't hold a wire to a receptacle it owns (its
//!   own inbox), and only the user grants: [`Grant`] has no agent approver, so
//!   there's no way for a plug to wire itself (rule 5, no ambient authority).
//! - **Cross-process wires (v2) need both ends**: a wire to another Marv's
//!   receptacle counts only once that Marv's user approved it too.
//! - Yolo never wires anything: a grant always names a user approval.

use serde::{Deserialize, Serialize};

use crate::MeshError;
use crate::limits::WireLimits;
use crate::plug::PlugId;
use crate::receptacle::ReceptacleName;

/// How a wire may be used: `call`, `send` or `watch`. The type lives in `marv-kernel` (runtime-kernel 2b), where rules
/// use it too; same serde names, same words.
pub use marv_kernel::Mode as WireMode;

/// Who approved a wire. Every variant is the user; there is deliberately no
/// variant for an agent or for yolo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Approver {
    /// An approval in the TUI.
    Tui,
    /// `marv mesh wire a -> b:inbox`.
    Cli,
    /// Headless `--approve mesh:<receptacle>`.
    Headless,
    /// A rule the person made and stored (runtime-kernel 2b, `marv_kernel::Policy`): still the user, as a rule is
    /// only ever the person's (made in Marv's own UI, never by a model, a message, a project config or yolo).
    ///
    /// Made only by the switchboard's own rule check (`Switchboard::rule_grant`), which matched a live rule in the
    /// person's file to both ends' identities. It can't come from outside: it is never deserialized (a peer's
    /// `marv/wire` can't claim it), and only marv-mesh builds a [`RuleGrant`]. The wire it grants admits only while its
    /// rule is still there and live (`Switchboard::authorize`).
    #[serde(skip_deserializing)]
    Policy(RuleGrant),
    /// The other Marv's own rule answered for its person (seen from this end). Only ever a wire's remote approver: a
    /// rule-issued approval crosses to another process as this word alone ([`Approver::on_the_wire`]), never as a
    /// [`RuleGrant`], so it can't be mistaken for this end's rule. Claiming it gives a peer nothing a peer saying `tui`
    /// wouldn't: this end decides for itself either way.
    PeerRule,
}

impl Approver {
    /// Whether a wire with this approver was issued by a rule here (the wake limits by hop apply to those only, D3).
    pub fn is_policy(&self) -> bool {
        matches!(self, Approver::Policy(_))
    }

    /// How this approval is told to another Marv (`marv/wire`): a rule here is [`Approver::PeerRule`] there.
    pub fn on_the_wire(self) -> Approver {
        match self {
            Approver::Policy(_) => Approver::PeerRule,
            other => other,
        }
    }
}

/// Which rule, under which generation of the rules, a rule-issued wire came from. Its fields are private and its
/// constructor is the crate's: no other crate can make one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RuleGrant {
    id: marv_kernel::PolicyId,
    generation: u64,
}

impl RuleGrant {
    /// Only `Switchboard::rule_grant` (and this crate's tests) make one.
    pub(crate) fn new(id: marv_kernel::PolicyId, generation: u64) -> Self {
        Self { id, generation }
    }

    pub fn id(&self) -> marv_kernel::PolicyId {
        self.id
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// The user's approval of a wire: this end's, and for a cross-process wire the other end's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub local: Approver,
    /// The other Marv's approval (v2). Required when [`Wire::cross_process`].
    pub remote: Option<Approver>,
}

impl Grant {
    pub fn local(by: Approver) -> Self {
        Self { local: by, remote: None }
    }
}

/// A grant from one plug to one receptacle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wire {
    pub from: PlugId,
    pub to: ReceptacleName,
    pub mode: WireMode,
    pub grant: Grant,
    pub limits: WireLimits,
    /// The receptacle is on another Marv's switchboard (v2) or the AgentMesh (v3).
    pub cross_process: bool,
}

impl Wire {
    pub fn new(from: PlugId, to: ReceptacleName, mode: WireMode, grant: Grant) -> Self {
        let cross_process = to.is_remote();
        Self { from, to, mode, grant, limits: WireLimits::default(), cross_process }
    }

    /// Whether both ends that must approve have.
    pub fn approved(&self) -> bool {
        !self.cross_process || self.grant.remote.is_some()
    }

    /// The wire is to a receptacle its own plug owns (an agent's own inbox).
    fn is_self(&self, owner: Option<&PlugId>) -> bool {
        owner == Some(&self.from) || self.to.inbox_owner() == Some(self.from.as_str())
    }
}

/// The switchboard's wiring table.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireTable {
    wires: Vec<Wire>,
}

impl WireTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a wire the user approved. `owner` is the receptacle's owner, when it
    /// has one. A wire for the same plug, receptacle and mode is replaced (new limits).
    pub fn add(&mut self, wire: Wire, owner: Option<&PlugId>) -> crate::Result<()> {
        if wire.is_self(owner) {
            return Err(MeshError::SelfWire(format!("{} -> {}", wire.from, wire.to)));
        }
        self.remove(&wire.from, &wire.to, wire.mode);
        self.wires.push(wire);
        Ok(())
    }

    /// Unwires; true when there was such a wire.
    pub fn remove(&mut self, from: &PlugId, to: &ReceptacleName, mode: WireMode) -> bool {
        let before = self.wires.len();
        self.wires.retain(|w| !(&w.from == from && &w.to == to && w.mode == mode));
        self.wires.len() != before
    }

    /// Removes every wire from or to an agent (it ended).
    pub fn unplug(&mut self, agent: &PlugId) {
        self.wires.retain(|w| &w.from != agent && w.to.inbox_owner() != Some(agent.as_str()));
    }

    /// The wire that allows `from` to use `receptacle` in `mode`, if any. Crate-only: decisions go through
    /// `Switchboard::authorize`.
    pub(crate) fn find(&self, from: &PlugId, receptacle: &ReceptacleName, mode: WireMode) -> Option<&Wire> {
        self.wires.iter().find(|w| &w.from == from && &w.to == receptacle && w.mode == mode && w.approved() && !w.is_self(None))
    }

    /// Whether a wire allows it. Default deny. On a snapshot ([`crate::Switchboard::table`]) this is a reading, for
    /// tests and display: the switchboard decides through `Switchboard::authorize`, and a guard keeps every other
    /// crate's code from deciding with this.
    pub fn allows(&self, from: &PlugId, receptacle: &ReceptacleName, mode: WireMode) -> bool {
        self.find(from, receptacle, mode).is_some()
    }

    /// [`Self::find`], as the error the model reads when there's no wire. Called by `Switchboard::authorize`; public
    /// for the same reading as [`Self::allows`].
    pub fn check(&self, from: &PlugId, receptacle: &ReceptacleName, mode: WireMode) -> crate::Result<&Wire> {
        self.find(from, receptacle, mode).ok_or_else(|| MeshError::NotWired {
            from: from.to_string(),
            receptacle: receptacle.to_string(),
            mode,
        })
    }

    /// Every usable wire from a plug (`mesh_list`, `tools/list`). Crate-only, like [`Self::find`].
    pub(crate) fn wires_from<'a>(&'a self, from: &'a PlugId) -> impl Iterator<Item = &'a Wire> + 'a {
        self.wires.iter().filter(move |w| &w.from == from && w.approved())
    }

    pub fn all(&self) -> &[Wire] {
        &self.wires
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> PlugId {
        PlugId::new(s).unwrap()
    }
    fn name(s: &str) -> ReceptacleName {
        ReceptacleName::new(s).unwrap()
    }
    fn wire(from: &str, to: &str, mode: WireMode) -> Wire {
        Wire::new(id(from), name(to), mode, Grant::local(Approver::Tui))
    }

    #[test]
    fn default_deny() {
        let table = WireTable::new();
        for mode in [WireMode::Call, WireMode::Send, WireMode::Watch] {
            assert!(!table.allows(&id("worker-1"), &name("agent.parent.inbox"), mode));
        }
        let err = table.check(&id("worker-1"), &name("agent.parent.inbox"), WireMode::Send).unwrap_err();
        assert_eq!(
            err.to_string(),
            "not wired: worker-1 has no send wire to agent.parent.inbox (the user wires it: /mesh wire, or marv mesh wire)"
        );
    }

    #[test]
    fn a_wire_allows_exactly_its_plug_receptacle_and_mode() {
        let mut table = WireTable::new();
        table.add(wire("worker-1", "agent.parent.inbox", WireMode::Send), Some(&id("parent"))).unwrap();
        assert!(table.allows(&id("worker-1"), &name("agent.parent.inbox"), WireMode::Send));
        // Another mode, another plug, a look-alike name: all denied.
        assert!(!table.allows(&id("worker-1"), &name("agent.parent.inbox"), WireMode::Call));
        assert!(!table.allows(&id("worker-1"), &name("agent.parent.inbox"), WireMode::Watch));
        assert!(!table.allows(&id("worker-2"), &name("agent.parent.inbox"), WireMode::Send));
        assert!(!table.allows(&id("worker-1"), &name("agent.parent.inbox2"), WireMode::Send));
        assert!(!table.allows(&id("worker-1"), &name("agent.parent"), WireMode::Send));
        // Wires are one-way.
        assert!(!table.allows(&id("parent"), &name("agent.worker-1.inbox"), WireMode::Send));
    }

    #[test]
    fn no_self_wiring() {
        let mut table = WireTable::new();
        let err = table.add(wire("worker-1", "agent.worker-1.inbox", WireMode::Send), None).unwrap_err();
        assert!(matches!(err, MeshError::SelfWire(_)));
        // Also refused when ownership comes from the receptacle, not its name.
        assert!(table.add(wire("dev", "process.dev", WireMode::Call), Some(&id("dev"))).is_err());
        assert!(!table.allows(&id("worker-1"), &name("agent.worker-1.inbox"), WireMode::Send));
        assert!(table.all().is_empty());
    }

    #[test]
    fn cross_process_wires_need_both_ends() {
        let mut table = WireTable::new();
        table.add(wire("parent", "mesh.remote.weather", WireMode::Call), None).unwrap();
        assert!(!table.allows(&id("parent"), &name("mesh.remote.weather"), WireMode::Call));
        let both = Wire::new(
            id("parent"),
            name("mesh.remote.weather"),
            WireMode::Call,
            Grant { local: Approver::Cli, remote: Some(Approver::Tui) },
        );
        table.add(both, None).unwrap();
        assert!(table.allows(&id("parent"), &name("mesh.remote.weather"), WireMode::Call));
        assert_eq!(table.all().len(), 1, "replaced, not duplicated");
    }

    #[test]
    fn unwiring_and_unplugging() {
        let mut table = WireTable::new();
        table.add(wire("worker-1", "agent.parent.inbox", WireMode::Send), None).unwrap();
        table.add(wire("parent", "agent.worker-1.inbox", WireMode::Send), None).unwrap();
        table.add(wire("worker-2", "agent.parent.inbox", WireMode::Send), None).unwrap();
        assert!(table.remove(&id("worker-2"), &name("agent.parent.inbox"), WireMode::Send));
        assert!(!table.remove(&id("worker-2"), &name("agent.parent.inbox"), WireMode::Send));
        table.unplug(&id("worker-1"));
        assert!(table.all().is_empty());
    }

    #[test]
    fn a_policy_approver_is_never_read_from_outside() {
        let by = Approver::Policy(RuleGrant::new(marv_kernel::PolicyId(3), 9));
        assert!(by.is_policy() && !Approver::Tui.is_policy());
        assert_eq!(serde_json::to_value(by).unwrap(), serde_json::json!({"policy": {"id": 3, "generation": 9}}));
        // What a peer's `marv/wire` sends as `by` (a string), or anything shaped like it, never parses as one.
        for claimed in [serde_json::json!("policy"), serde_json::json!({"policy": {"id": 3, "generation": 9}})] {
            assert!(serde_json::from_value::<Approver>(claimed.clone()).is_err(), "{claimed}");
        }
        assert_eq!(serde_json::from_value::<Approver>(serde_json::json!("tui")).unwrap(), Approver::Tui);
    }

    #[test]
    fn modes_parse() {
        assert_eq!("watch".parse::<WireMode>().unwrap(), WireMode::Watch);
        assert!("all".parse::<WireMode>().is_err());
    }
}
