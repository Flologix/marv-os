//! Which agents are loaded (runtime-kernel phase 1, `docs/architecture/runtime-kernel.md` §5.3). New in marv-rs.
//!
//! A desktop chat is always *resident* while the app runs (its mesh door, wires and inbox stay); its *session* (the
//! model, its MCP servers, the history in memory) is loaded only while it's needed. At most `cap` (K, the desktop's
//! `MAX_LIVE`) sessions are loaded at once for anything the scheduler starts on its own: a message that wakes a
//! sleeping chat waits in a queue, in arrival order, until a slot frees. A person opening a chat is never made to
//! wait (decision D12): it's admitted even over K, and the next chat that can go is evicted as soon as one can.
//!
//! Eviction takes the least recently *active* loaded agent (shown, its turn ended, woken), and never one that is
//! loading, running a turn, waiting for an answer, or shown on a page.
//!
//! A pure state machine: no Tokio, no clock (time is passed in), no I/O. The caller does what it's told: unload what
//! [`Admit::RunAfterEvicting`] or [`Action::Evict`] names, load what [`Action::Start`] names, and report each state
//! change back through [`Scheduler::update`] and [`Scheduler::attach`].

use std::collections::{BTreeMap, VecDeque};

use crate::AgentId;

/// What an agent asks the scheduler for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Show (and so load) the agent. `by_person`: a person opened it (admitted even over the cap); otherwise it's
    /// treated like a wake.
    Open { by_person: bool },
    /// A message woke it (`by`: who, for the caller's own records).
    Wake { by: String },
}

/// The scheduler's answer to a [`Request`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admit {
    /// Load it now (or it's loaded already).
    Run,
    /// Unload this agent's session first (it's now [`AgentState::Sleeping`]), then load the requester.
    RunAfterEvicting(AgentId),
    /// Every slot is taken by an agent that can't go: it starts when one frees ([`Action::Start`]), in arrival order.
    /// `position` 0 is next.
    Queued { position: usize },
}

/// Where an agent's session is. Everything but `Sleeping` takes a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    /// Resident only: no session loaded.
    Sleeping,
    /// Admitted, its session being built.
    Loading,
    /// Loaded, nothing running.
    Idle,
    /// Running a turn.
    Busy,
    /// An approval waits for the person.
    Waiting,
}

impl AgentState {
    pub fn loaded(self) -> bool {
        self != Self::Sleeping
    }
}

/// What the caller must do after a state change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Unload this agent's session (it's now [`AgentState::Sleeping`]).
    Evict(AgentId),
    /// A queued request's turn: load this agent (it's now [`AgentState::Loading`]) and wake it. Also given for a queued
    /// agent that got loaded meanwhile (a person opened it): it then needs only its wake.
    Start(AgentId),
}

#[derive(Debug, Clone)]
struct Entry {
    state: AgentState,
    /// A page shows it.
    attached: bool,
    /// When it was last active (shown, its state changed, woken): eviction takes the oldest.
    active_ms: u64,
}

/// The run queue and the cap. See the module doc.
#[derive(Debug, Clone)]
pub struct Scheduler {
    cap: usize,
    agents: BTreeMap<AgentId, Entry>,
    /// Requests waiting for a slot, oldest first; an agent is in it at most once.
    queue: VecDeque<AgentId>,
}

impl Scheduler {
    /// A scheduler that loads at most `cap` agents for what it starts itself (at least 1).
    pub fn new(cap: usize) -> Self {
        Self { cap: cap.max(1), agents: BTreeMap::new(), queue: VecDeque::new() }
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    /// An agent's state ([`AgentState::Sleeping`] for one the scheduler doesn't know).
    pub fn state(&self, id: &AgentId) -> AgentState {
        self.agents.get(id).map_or(AgentState::Sleeping, |e| e.state)
    }

    /// Whether a page shows it.
    pub fn attached(&self, id: &AgentId) -> bool {
        self.agents.get(id).is_some_and(|e| e.attached)
    }

    /// How many agents take a slot now.
    pub fn loaded(&self) -> usize {
        self.agents.values().filter(|e| e.state.loaded()).count()
    }

    /// The queued agents, next first.
    pub fn queued(&self) -> Vec<AgentId> {
        self.queue.iter().cloned().collect()
    }

    fn entry(&mut self, id: &AgentId, now_ms: u64) -> &mut Entry {
        self.agents.entry(id.clone()).or_insert(Entry { state: AgentState::Sleeping, attached: false, active_ms: now_ms })
    }

    /// The agent eviction takes now, other than `except`: loaded and idle, not shown, least recently active (ties: the
    /// smaller id, so it's deterministic). Never one loading, busy, waiting or attached.
    pub fn victim(&self, except: Option<&AgentId>) -> Option<AgentId> {
        self.agents
            .iter()
            .filter(|(id, e)| Some(*id) != except && e.state == AgentState::Idle && !e.attached)
            .min_by_key(|(id, e)| (e.active_ms, (*id).clone()))
            .map(|(id, _)| id.clone())
    }

    /// Asks for `id` to be loaded. See [`Admit`]. The scheduler records the outcome: a `Run` agent is `Loading` (or
    /// stays as it is, if loaded), a victim is `Sleeping`.
    pub fn request(&mut self, id: AgentId, request: Request, now_ms: u64) -> Admit {
        let person = matches!(request, Request::Open { by_person: true });
        let loaded = self.state(&id).loaded();
        if person {
            // Shown now: whatever wake it had queued is the loaded session's to take.
            self.queue.retain(|q| q != &id);
        }
        if loaded {
            self.entry(&id, now_ms).active_ms = now_ms;
            return Admit::Run;
        }
        if !person {
            if let Some(position) = self.queue.iter().position(|q| q == &id) {
                return Admit::Queued { position };
            }
            // Never ahead of a wake that came first.
            if !self.queue.is_empty() {
                self.queue.push_back(id);
                return Admit::Queued { position: self.queue.len() - 1 };
            }
        }
        let admit = if self.loaded() < self.cap {
            Admit::Run
        } else if let Some(victim) = self.victim(Some(&id)) {
            // Over the cap only after a person's open, and then nothing could go (see `rebalance`): so here it's
            // exactly at the cap, and one out makes room for one in.
            self.entry(&victim, now_ms).state = AgentState::Sleeping;
            Admit::RunAfterEvicting(victim)
        } else if person {
            // D12: the person is never made to wait; the next chat that can go is evicted once one can.
            Admit::Run
        } else {
            self.queue.push_back(id);
            return Admit::Queued { position: self.queue.len() - 1 };
        };
        let entry = self.entry(&id, now_ms);
        entry.state = AgentState::Loading;
        entry.active_ms = now_ms;
        admit
    }

    /// `id`'s session is now in `state` (it loaded, a turn started or ended, an approval came or was answered, it
    /// ended). Counts as activity. Returns what that frees: evictions (back under the cap) and queued starts.
    pub fn update(&mut self, id: &AgentId, state: AgentState, now_ms: u64) -> Vec<Action> {
        let entry = self.entry(id, now_ms);
        entry.state = state;
        entry.active_ms = now_ms;
        self.rebalance(now_ms)
    }

    /// A page started (`true`) or stopped showing `id`. Counts as activity.
    pub fn attach(&mut self, id: &AgentId, attached: bool, now_ms: u64) -> Vec<Action> {
        let entry = self.entry(id, now_ms);
        entry.attached = attached;
        entry.active_ms = now_ms;
        self.rebalance(now_ms)
    }

    /// `id` is gone (deleted, or stopped by the person): it takes no slot and its queued request is dropped.
    pub fn remove(&mut self, id: &AgentId, now_ms: u64) -> Vec<Action> {
        self.agents.remove(id);
        self.queue.retain(|q| q != id);
        self.rebalance(now_ms)
    }

    /// Back under the cap if a person's open took it over (evicting what can go), then queued requests in order while
    /// there's a slot, or an idle agent to make one.
    fn rebalance(&mut self, now_ms: u64) -> Vec<Action> {
        let mut actions = Vec::new();
        while self.loaded() > self.cap {
            let Some(victim) = self.victim(None) else { break };
            self.entry(&victim, now_ms).state = AgentState::Sleeping;
            actions.push(Action::Evict(victim));
        }
        while let Some(next) = self.queue.front().cloned() {
            if self.state(&next).loaded() {
                self.queue.pop_front();
                actions.push(Action::Start(next));
                continue;
            }
            if self.loaded() >= self.cap {
                if self.loaded() > self.cap {
                    break;
                }
                let Some(victim) = self.victim(Some(&next)) else { break };
                self.entry(&victim, now_ms).state = AgentState::Sleeping;
                actions.push(Action::Evict(victim));
            }
            self.queue.pop_front();
            let entry = self.entry(&next, now_ms);
            entry.state = AgentState::Loading;
            entry.active_ms = now_ms;
            actions.push(Action::Start(next));
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: usize) -> AgentId {
        AgentId::new(&format!("{n:08x}-0000-4000-8000-000000000000")).unwrap()
    }

    fn wake() -> Request {
        Request::Wake { by: "peer".into() }
    }

    const PERSON: Request = Request::Open { by_person: true };

    /// `n` agents loaded and idle, unshown, active at 1, 2, 3…
    fn full(cap: usize, n: usize) -> Scheduler {
        let mut s = Scheduler::new(cap);
        for i in 0..n {
            s.agents.insert(id(i), Entry { state: AgentState::Idle, attached: false, active_ms: i as u64 + 1 });
        }
        s
    }

    #[test]
    fn under_the_cap_everything_runs_and_a_loaded_agent_just_runs() {
        let mut s = Scheduler::new(2);
        assert_eq!(s.request(id(0), wake(), 1), Admit::Run);
        assert_eq!(s.state(&id(0)), AgentState::Loading);
        assert_eq!(s.request(id(1), PERSON, 2), Admit::Run);
        assert_eq!(s.request(id(0), wake(), 3), Admit::Run, "already loaded: no second slot");
        assert_eq!(s.loaded(), 2);
    }

    #[test]
    fn eviction_takes_the_least_recently_active_idle_unshown_agent_never_a_busy_waiting_loading_or_shown_one() {
        let mut s = full(6, 6);
        // 0 runs a turn, 1 waits for an answer, 2 is shown, 3 is loading: 4 is the oldest that can go.
        s.update(&id(0), AgentState::Busy, 0);
        s.update(&id(1), AgentState::Waiting, 0);
        s.attach(&id(2), true, 0);
        s.update(&id(3), AgentState::Loading, 0);
        for i in 0..4 {
            s.agents.get_mut(&id(i)).unwrap().active_ms = 0;
        }
        assert_eq!(s.victim(None), Some(id(4)));
        assert_eq!(s.request(id(9), wake(), 100), Admit::RunAfterEvicting(id(4)));
        assert_eq!(s.state(&id(4)), AgentState::Sleeping);
        // Activity, not when it was loaded: 5 was woken (busy, then its turn ended) after the others went idle.
        s.update(&id(5), AgentState::Busy, 101);
        s.update(&id(5), AgentState::Idle, 102);
        s.update(&id(0), AgentState::Idle, 50);
        assert_eq!(s.victim(None), Some(id(0)), "0's turn ended before 5's");
        // A page leaving counts as activity too (it was shown then).
        s.attach(&id(2), false, 200);
        s.update(&id(1), AgentState::Idle, 150);
        assert_eq!(s.victim(None), Some(id(0)));
        s.update(&id(0), AgentState::Busy, 300);
        assert_eq!(s.victim(None), Some(id(5)));
    }

    #[test]
    fn queued_wakes_run_in_arrival_order_as_slots_free() {
        let mut s = full(2, 2);
        s.update(&id(0), AgentState::Busy, 10);
        s.update(&id(1), AgentState::Busy, 10);
        assert_eq!(s.request(id(7), wake(), 11), Admit::Queued { position: 0 });
        assert_eq!(s.request(id(5), wake(), 12), Admit::Queued { position: 1 });
        assert_eq!(s.request(id(6), wake(), 13), Admit::Queued { position: 2 });
        assert_eq!(s.request(id(7), wake(), 14), Admit::Queued { position: 0 }, "queued once");
        assert_eq!(s.queued(), [id(7), id(5), id(6)]);
        // 0's turn ends: it's idle and unshown, so it makes room for the first wake.
        assert_eq!(s.update(&id(0), AgentState::Idle, 20), [Action::Evict(id(0)), Action::Start(id(7))]);
        assert_eq!(s.state(&id(7)), AgentState::Loading);
        // Nothing else can go (7 loading, 1 busy).
        assert_eq!(s.update(&id(7), AgentState::Busy, 21), []);
        // A wake that comes now waits behind the queue even though a slot might free.
        assert_eq!(s.request(id(8), wake(), 22), Admit::Queued { position: 2 });
        assert_eq!(s.remove(&id(1), 23), [Action::Start(id(5))]);
        assert_eq!(s.update(&id(5), AgentState::Idle, 24), [Action::Evict(id(5)), Action::Start(id(6))]);
        assert_eq!(s.update(&id(6), AgentState::Idle, 25), [Action::Evict(id(6)), Action::Start(id(8))]);
        assert!(s.queued().is_empty());
        assert!(s.loaded() <= 2);
    }

    #[test]
    fn a_person_is_admitted_over_the_cap_and_the_next_agent_that_can_go_is_evicted_then() {
        let mut s = full(2, 2);
        s.update(&id(0), AgentState::Busy, 10);
        s.update(&id(1), AgentState::Waiting, 10);
        assert_eq!(s.request(id(3), PERSON, 11), Admit::Run);
        assert_eq!(s.loaded(), 3, "the person never waits (D12)");
        s.attach(&id(3), true, 11);
        // A wake now queues: the cap is hard for what the scheduler starts.
        assert_eq!(s.request(id(4), wake(), 12), Admit::Queued { position: 0 });
        // 1's approval is answered: it can go, which brings the count back to the cap, and no further.
        assert_eq!(s.update(&id(1), AgentState::Idle, 13), [Action::Evict(id(1))]);
        assert_eq!(s.loaded(), 2);
        // The person's chat loads, is shown, then left: now it can go, and the queued wake takes its slot.
        assert_eq!(s.update(&id(3), AgentState::Idle, 14), [], "shown: it stays");
        assert_eq!(s.attach(&id(3), false, 15), [Action::Evict(id(3)), Action::Start(id(4))]);
        // With room, a person's open of an agent that was queued takes it out of the queue.
        let mut s = full(1, 1);
        s.update(&id(0), AgentState::Busy, 1);
        assert_eq!(s.request(id(2), wake(), 2), Admit::Queued { position: 0 });
        assert_eq!(s.request(id(2), PERSON, 3), Admit::Run);
        assert!(s.queued().is_empty());
    }

    #[test]
    fn a_queued_agent_a_person_loaded_meanwhile_is_started_without_a_second_slot() {
        let mut s = full(1, 1);
        s.update(&id(0), AgentState::Busy, 1);
        assert_eq!(s.request(id(2), wake(), 2), Admit::Queued { position: 0 });
        // Loaded some other way (shown on a page by the caller's own bookkeeping): the queue hands it its wake, no load.
        s.attach(&id(2), true, 3);
        assert_eq!(s.update(&id(2), AgentState::Idle, 3), [Action::Start(id(2))]);
        assert!(s.queued().is_empty());
        assert_eq!(s.loaded(), 2);
    }

    /// The cap holds against a randomized run of opens, wakes, turn starts and ends, approvals, pages coming and going
    /// and removals: a load the scheduler starts (a wake's `Run`, an [`Action::Start`]) never takes the count past K;
    /// over K only after a person's open, and then only while nothing can be evicted; every eviction is of an idle,
    /// unshown agent; queued wakes start in arrival order.
    #[test]
    fn the_cap_holds_for_what_the_scheduler_starts_in_a_randomized_run() {
        struct Rng(u64);
        impl Rng {
            fn next(&mut self, n: u64) -> u64 {
                self.0 ^= self.0 << 13;
                self.0 ^= self.0 >> 7;
                self.0 ^= self.0 << 17;
                self.0 % n
            }
        }
        for seed in 1..=200u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let cap = 1 + rng.next(4) as usize;
            let agents = cap + 1 + rng.next(6) as usize;
            let mut s = Scheduler::new(cap);
            let mut arrivals: Vec<AgentId> = Vec::new();
            let mut started: Vec<AgentId> = Vec::new();
            for step in 0..400u64 {
                let now = step;
                let who = id(rng.next(agents as u64) as usize);
                let before = s.loaded();
                let queued_before = s.queued();
                let check_actions = |s: &Scheduler, actions: &[Action], started: &mut Vec<AgentId>| {
                    for action in actions {
                        match action {
                            Action::Evict(v) => assert_eq!(s.state(v), AgentState::Sleeping),
                            Action::Start(a) => started.push(a.clone()),
                        }
                    }
                };
                match rng.next(7) {
                    0 => {
                        let admit = s.request(who.clone(), PERSON, now);
                        if let Admit::RunAfterEvicting(v) = &admit {
                            assert_eq!(before, cap);
                            assert!(!s.attached(v));
                        }
                    }
                    1 | 2 => {
                        let was_loaded = s.state(&who).loaded();
                        let admit = s.request(who.clone(), wake(), now);
                        match admit {
                            Admit::Run if !was_loaded => assert!(s.loaded() <= cap, "seed {seed}: a wake took K past"),
                            Admit::RunAfterEvicting(_) => assert!(s.loaded() <= cap),
                            Admit::Queued { .. } => {
                                if !queued_before.contains(&who) {
                                    arrivals.push(who.clone());
                                }
                            }
                            Admit::Run => {}
                        }
                    }
                    3 if s.state(&who).loaded() => {
                        let state = [AgentState::Idle, AgentState::Busy, AgentState::Waiting][rng.next(3) as usize];
                        let actions = s.update(&who, state, now);
                        check_actions(&s, &actions, &mut started);
                    }
                    4 if s.state(&who).loaded() => {
                        let actions = s.attach(&who, rng.next(2) == 0, now);
                        check_actions(&s, &actions, &mut started);
                    }
                    5 if s.state(&who).loaded() => {
                        let actions = s.update(&who, AgentState::Sleeping, now);
                        check_actions(&s, &actions, &mut started);
                    }
                    6 if rng.next(5) == 0 => {
                        let actions = s.remove(&who, now);
                        arrivals.retain(|a| a != &who);
                        check_actions(&s, &actions, &mut started);
                    }
                    _ => {}
                }
                // Never over K while something could go; a person's open is the only way past it.
                if s.loaded() > cap {
                    assert!(s.victim(None).is_none(), "seed {seed} step {step}: over K with an idle unshown agent");
                }
                // Queued wakes start in the order they arrived (person opens may take one out of the queue).
                for a in started.drain(..) {
                    let position = arrivals.iter().position(|x| x == &a).expect("started what was queued");
                    assert!(
                        arrivals[..position].iter().all(|x| !s.queued().contains(x)),
                        "seed {seed}: {a} started ahead of an earlier wake"
                    );
                    arrivals.remove(position);
                }
                arrivals.retain(|a| s.queued().contains(a));
            }
        }
    }
}
