//! The desktop's chats as the runtime kernel sees them (runtime-kernel 1): each one a [`Resident`] record kept while
//! the app runs, with its session loaded or not. A record keeps the chat on the mesh while it sleeps; its watcher wakes
//! the loaded session when a wired message comes. Free of Tauri: the app gives it a [`Host`] (how to build a chat's
//! spec, put it on the mesh, save its session id and spawn work), and the tests give it theirs.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use marv_web::approver::WebApprover;
use marv_web::host::{ChatSpec, check_wake, check_wake_waiting, run_chat, run_person, run_timer, run_wake_then};
use marv_web::protocol::ClientMessage;
use serde_json::Value;
#[cfg(test)]
use serde_json::json;
use tokio::sync::mpsc;

use marv_kernel::{
    Action, Admit, AgentId, AgentKey, AgentState, Interrupt, InterruptKind, InterruptSource, KernelEvent, Request, Scheduler,
    SignalBy, SignalKind, TimerStore, TimerVoidReason, TurnNow, handling,
};

use crate::background::{self, LiveChat, MAX_LIVE, Pending, Queued, Resident, Shared, Sink};

/// Said when something would load a chat that's open in its terminal.
pub const IN_TERMINAL: &str = "This chat is open in its terminal: close the terminal to chat here again.";

pub type Task = Pin<Box<dyn Future<Output = ()> + Send>>;

/// A harness chat's numbers on its door's `live` (runtime-kernel §20.2), for Activity.
fn show_meter(mesh: &marv_session::mesh::MeshHandle, meter: &Arc<marv_session::live::LiveMeter>) {
    let weak = Arc::downgrade(meter);
    mesh.set_live(Arc::new(move || weak.upgrade()?.harness_live(marv_session::time::now_ms())));
}

/// What the chats need from the app.
pub trait Host: Send + Sync + 'static {
    /// How to load chat `id` now, as its record says (folder, saved session, last settings): `resident` is filled in
    /// by the caller. Err: why it can't load (its folder is gone, no key), told to the person.
    fn spec(&self, id: &str) -> Result<ChatSpec, String>;
    /// Puts chat `id` (in `folder`) on the mesh, its door's wire requests asked through `approver` (None: no mesh).
    fn start_mesh(
        &self,
        id: &str,
        folder: &Path,
        approver: &Arc<WebApprover>,
        out: &mpsc::UnboundedSender<Value>,
    ) -> Option<marv_session::mesh_process::Mesh>;
    /// Its session's id, from a load's `ready`: reopening it, or a wake, continues it.
    fn session_saved(&self, id: &str, session: &str);
    /// Types `text` into chat `id`'s terminal as the person's message (pasted, then Enter): team mail for an agent that
    /// runs in its terminal. False: no terminal of it runs.
    fn type_into(&self, _id: &str, _text: &str) -> bool {
        false
    }
    /// Starts chat `id`'s agent in its terminal with no page showing it (inside the outer sandbox: nobody watches), for
    /// mail to an agent Marv can't run headless (Codex, OpenCode…). Err: why it can't.
    fn start_terminal(&self, _id: &str) -> Result<(), String> {
        Err("No terminals here.".into())
    }
    /// What a team member's agent is told first in its terminal (its role and team), for an agent that can't be given
    /// it at start as Claude Code is. None: no role, or it was given at start.
    fn terminal_brief(&self, _id: &str) -> Option<String> {
        None
    }
    /// Chat `id`'s spec for its terminal (no Marv key or model needed): its role and team, for its reports.
    fn terminal_spec(&self, id: &str) -> Result<ChatSpec, String> {
        self.spec(id)
    }
    /// A question chat `id`'s terminal shows that only the person can answer (its folder trust, a login): what to tell
    /// them. None: none (or no terminal).
    fn terminal_question(&self, _id: &str) -> Option<String> {
        None
    }
    /// Stops the turn running in chat `id`'s terminal (Escape, as the person would): an unattended turn past its
    /// deadline, or whose grant ended.
    fn interrupt_terminal(&self, _id: &str) {}
    /// Whether chat `id`'s agent runs only in its terminal (Marv can't run it headless): mail starts its terminal.
    fn terminal_only(&self, _id: &str) -> bool {
        false
    }
    fn spawn(&self, task: Task);
    /// One of chat `id`'s events, for the person's notifications (an approval, a turn's end); `shown`: a page showed
    /// the chat when it came. Called off the chats' lock; never blocks.
    fn notify(&self, _id: &str, _event: &Value, _shown: bool) {}
    /// Chat `id`'s service grant, as its sessions consult it ([`marv_session::grants::ChatGrant`]); a pause is told
    /// on `out`. None: the chat can never carry one (tests, no config folder).
    fn grant(&self, _id: &str, _out: &mpsc::UnboundedSender<Value>) -> Option<Arc<dyn marv_session::grants::ChatGrant>> {
        None
    }
    /// Chat `id`'s card from its saved session (runtime-kernel chat card: its person messages, no model), so a chat
    /// that sleeps from the start still says what it's about. None: no saved session.
    fn saved_card(&self, _id: &str) -> CardTask {
        Box::pin(async { None })
    }
    /// The person's `mesh.wake` (mk-60m.8; `~/.marv/config.json`, shared with `marv`, set in Settings): which wires
    /// may wake a chat. Read at each look, so a change applies at once. The default (a host that doesn't say): the
    /// config's own default, only rules' wires.
    fn mesh_wake(&self) -> marv_session::config::MeshWake {
        marv_session::config::MeshWake::Rules
    }
    /// Whether the person marked chat `id` private (runtime-kernel §9.2, R3: other chats can't read it). A chat with
    /// no record is private: nothing is served for it.
    fn private(&self, id: &str) -> bool;
    /// A notice for chat `id` the desktop has for it as it loads (runtime-kernel §14.1: its worktree was gone, so it
    /// works in its folder again), told in the chat once. None by default.
    fn take_notice(&self, _id: &str) -> Option<String> {
        None
    }
    /// Chat `id`'s saved conversation as a `chat_read` may see it (`marv_session::chat_read::snapshot_of_saved`), for
    /// a chat whose session isn't loaded. None: no saved session.
    fn saved_snapshot(&self, _id: &str) -> SnapshotTask {
        Box::pin(async { None })
    }
    /// Whether chat `id` is run by another harness (Claude Code; the harness plan, phase 2): no mesh, woken by timers
    /// through its messages, asleep when idle a while.
    fn harness(&self, _id: &str) -> bool {
        false
    }
    /// Runs one load of a chat: [`run_chat`] (tests give a harness chat a stand-in).
    fn run(&self, spec: ChatSpec, incoming: mpsc::UnboundedReceiver<ClientMessage>, out: mpsc::UnboundedSender<Value>) -> Task {
        Box::pin(run_chat(spec, incoming, out))
    }
    /// Chat `id`'s `wake_me` timers under `grant` (runtime-kernel §15.4). None: the chat isn't offered the tool.
    fn timers(
        &self,
        _id: &str,
        _grant: &Arc<dyn marv_session::grants::ChatGrant>,
    ) -> Option<Arc<dyn marv_session::timers::SelfTimers>> {
        None
    }
}

/// A saved chat's `chat_read` snapshot, read off the app's thread.
pub type SnapshotTask = Pin<Box<dyn Future<Output = Option<marv_mesh::chat_read::ChatSnapshot>> + Send>>;

/// A resident chat as other chats' `chat_read` reads it (runtime-kernel §9.2): never when the person marked it
/// private; from its loaded session (mid-turn: as of its last finished turn, never the turn in progress), else from
/// its saved session, so a sleeping chat is read without waking it. A served read is a quiet notice in the chat.
struct ResidentSource {
    id: String,
    host: Arc<dyn Host>,
    session: Weak<Mutex<Option<marv_session::MarvSession>>>,
    out: mpsc::UnboundedSender<Value>,
}

#[async_trait::async_trait]
impl marv_mesh::chat_read::ChatSource for ResidentSource {
    async fn snapshot(&self) -> Result<marv_mesh::chat_read::ChatSnapshot, marv_mesh::chat_read::Unreadable> {
        use marv_mesh::chat_read::Unreadable;
        if self.host.private(&self.id) {
            return Err(Unreadable::Private);
        }
        // While a load or an unload is under way the slot can lag the session by a moment: a read then serves the
        // saved session, as of its last save (a turn's end saves it), which is at most that stale. Never newer text
        // than the person saw. A loaded session mid-turn serves its last finished turn; a chat unloads only idle, so a
        // saved file holds a turn in progress only when Marv quit during it (#202's cut-off turn): that turn is served
        // as saved, without a mid-turn line, as the chat shows it when it opens again.
        let loaded = self.session.upgrade().and_then(|slot| slot.lock().unwrap_or_else(|e| e.into_inner()).clone());
        match loaded {
            Some(session) => session.chat_snapshot(),
            None => self.host.saved_snapshot(&self.id).await.ok_or(Unreadable::Unavailable),
        }
    }

    fn read_by(&self, reader: &str, ask: marv_mesh::chat_read::ReadAsk) {
        let line = marv_mesh::chat_read::notice_line(reader, ask);
        let _ = self.out.send(serde_json::json!({"type": "notice", "text": line}));
    }
}

/// A saved chat's card, read off the app's thread.
pub type CardTask = Pin<Box<dyn Future<Output = Option<marv_kernel::Card>> + Send>>;

struct Inner {
    residents: HashMap<String, Resident>,
    /// Which sessions are loaded, the cap K and the run queue (runtime-kernel 1).
    sched: Scheduler,
    /// Stopped by the person this run: not brought back by a rule until opened again.
    stopped: HashSet<String>,
    /// Harness chats open in their own terminal ([`Chats::to_terminal`]): the agent's own interface runs their
    /// conversation, so nothing loads them (a page, mail, a timer) until it closes.
    in_terminal: HashSet<String>,
    /// What each open terminal's agent is doing, from its hooks ([`Chats::terminal_state`]): shown in place of the
    /// record's own state (which is asleep: no session loaded).
    terminal_states: HashMap<String, AgentState>,
    /// Chats whose door is lent to their terminal's Marv ([`Chats::to_terminal`]): it runs the chat's switchboard under
    /// the chat's name meanwhile, and the door comes back when it ends.
    lent: HashSet<String>,
    /// Terminal chats whose agent was told its team brief (with its first mail).
    briefed: HashSet<String>,
    /// Terminal chats Marv OS started for mail, and whether their agent has drawn yet: it's ready for the mail once it
    /// drew and went quiet (an output-watched terminal says idle before its agent has drawn anything).
    starting: HashMap<String, bool>,
    /// Terminal chats whose agent was typed a teammate's mail, and whether its turn on it started (it went busy): its
    /// report goes back when it's idle again ([`Chats::terminal_state`]).
    mail_turn: HashMap<String, bool>,
    /// What the person's views (the team's first task, "Tell the team") typed for a terminal chat whose agent wasn't
    /// ready yet: typed in, in order, once it is.
    to_type: HashMap<String, Vec<String>>,
    /// Terminal chats whose question for the person (`Host::terminal_question`) they were told about.
    told_question: HashSet<String>,
    /// Terminal chats showing a card for mail that doesn't wake them ([`Chats::mail_waits`]): cleared once it's read.
    mail_shown: HashSet<String>,
    /// Terminal chats asking the person now (a permission prompt, a question only they can answer), and the id their
    /// "needs you" notification goes by: withdrawn when it's answered.
    terminal_ask: HashMap<String, u64>,
    next_ask: u64,
    /// Terminal chats running a timer's turn typed into them, and whether it started (they went busy): unattended
    /// until it ends (its deadline and grant held over it by `tick_harness`).
    timer_turn: HashMap<String, bool>,
}

pub struct Chats {
    inner: Mutex<Inner>,
    /// Who's on the mesh, for every chat's `mesh_list` (runtime-kernel 3): the chats with a door, entered here as the
    /// desktop's own (authoritative, with their scheduler state), and the other Marvs from the door scan. Seeing
    /// grants nothing.
    registry: Arc<marv_mesh::registry::MeshRegistry>,
    /// The kernel's event log (runtime-kernel 3), once the app gives one ([`Chats::set_events`]): every chat's
    /// messages, rule wires and wakes, and the registry's entries and state changes.
    events: Mutex<Option<Arc<dyn marv_kernel::EventLog>>>,
    host: Arc<dyn Host>,
    me: Weak<Chats>,
    /// The scheduler's clock starts here.
    epoch: Instant,
    /// `mesh.cards` from the person's config (runtime-kernel chat card), as the app last read it ([`Chats::set_cards`]):
    /// off until then. Every chat's switchboard follows it.
    cards: std::sync::atomic::AtomicBool,
    /// The person's timers (`~/.marv/kernel/timers.json`, runtime-kernel §15.4), once the app gives them
    /// ([`Chats::set_timers`]); [`Chats::tick`] fires what's due.
    timers: Mutex<Option<TimerStore>>,
    /// How long an unattended turn may run (§15.4): [`marv_kernel::TURN_DEADLINE_MS`]; tests shorten it.
    deadline_ms: std::sync::atomic::AtomicU64,
    /// [`HARNESS_IDLE_MS`]; tests shorten it.
    harness_idle_ms: std::sync::atomic::AtomicU64,
}

/// What the person is told when an unattended turn ran into its deadline (Marv's words).
pub const DEADLINE_TEXT: &str = "Paused: this turn ran 30 minutes while you were away, so Marv stopped it. Mail and timers wait until you resume the chat (the Mesh panel) or write to it.";

/// What the person is told when a paused chat runs again (their resume, or their message).
pub const RESUMED_TEXT: &str = "Resumed: mail and timers can wake this chat again.";

/// Said in a chat the person paused (§15.3).
const PAUSED_TEXT: &str =
    "Paused from the Mesh panel: this turn ends after its current step, and mail and timers wait until you resume the chat.";
/// Said in a harness chat paused mid-turn (H1): its agent can't stop between steps, so its turn was stopped.
const HARNESS_PAUSED_TEXT: &str = "Paused from the Mesh panel: its agent can't stop between steps, so this turn was stopped. Mail and timers wait until you resume the chat.";

/// Said to a harness chat under the mail that woke it (phase 3).
/// A terminal agent's report when Marv OS can't read its reply (it's on the agent's own screen).
pub const TERMINAL_REPLY: &str = "Its turn on your message ended; its answer is in its own terminal (Marv OS can't read it). Ask it with mesh_send for the details, or open its chat.";

pub const HARNESS_MAIL_NOTE: &str = "Marv: that is mail from other agents on the mesh, not from the person. To answer, send with mesh_send (mesh_list shows the inbox names).";

/// How long a harness chat (Claude Code) stays loaded once idle with no page showing it: then it sleeps, its process
/// ends, and a message, an open or a timer resumes its conversation (the harness plan, phase 2).
pub const HARNESS_IDLE_MS: u64 = 10 * 60 * 1000;

/// How often a watcher looks at its inbox without being told (mail that came during a turn is unread after it).
const LOOK_AGAIN: Duration = Duration::from_secs(2);

/// The wall clock, for the event log and timers.
fn wall_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// What chat `resident` is doing, as an interrupt sees it.
fn turn_now(resident: &Resident) -> TurnNow {
    let busy = resident.keep.session().is_some_and(|s| s.busy()) || !resident.shared().idle();
    match (busy, resident.unattended) {
        (false, _) => TurnNow::Idle,
        (true, Some(_)) => TurnNow::Unattended,
        (true, None) => TurnNow::Person,
    }
}

/// The scheduler's name for a chat (a chat's id is a UUID; anything else is never scheduled, nor loaded for a wake).
fn agent(id: &str) -> Option<AgentId> {
    AgentId::new(id).filter(|a| a.as_str() == id)
}

impl Chats {
    /// The desktop's chats: at most [`MAX_LIVE`] loaded for what a wake starts (K, D15).
    pub fn new(host: Arc<dyn Host>) -> Arc<Self> {
        Self::with_cap(host, MAX_LIVE)
    }

    pub fn with_cap(host: Arc<dyn Host>, cap: usize) -> Arc<Self> {
        let inner = Inner {
            residents: HashMap::new(),
            sched: Scheduler::new(cap),
            stopped: HashSet::new(),
            in_terminal: HashSet::new(),
            terminal_states: HashMap::new(),
            lent: HashSet::new(),
            briefed: HashSet::new(),
            starting: HashMap::new(),
            mail_turn: HashMap::new(),
            to_type: HashMap::new(),
            told_question: HashSet::new(),
            mail_shown: HashSet::new(),
            terminal_ask: HashMap::new(),
            next_ask: 0,
            timer_turn: HashMap::new(),
        };
        let registry = marv_mesh::registry::MeshRegistry::new();
        Arc::new_cyclic(|me| Self {
            inner: Mutex::new(inner),
            registry,
            events: Mutex::default(),
            host,
            me: me.clone(),
            epoch: Instant::now(),
            cards: std::sync::atomic::AtomicBool::new(false),
            timers: Mutex::default(),
            deadline_ms: std::sync::atomic::AtomicU64::new(marv_kernel::TURN_DEADLINE_MS),
            harness_idle_ms: std::sync::atomic::AtomicU64::new(HARNESS_IDLE_MS),
        })
    }

    /// Fires the person's timers from `store` from now on ([`Chats::tick`]).
    pub fn set_timers(&self, store: TimerStore) {
        *self.timers.lock().unwrap_or_else(|e| e.into_inner()) = Some(store);
    }

    /// The deadline of every unattended turn from now on (tests).
    #[cfg(test)]
    pub fn set_deadline(&self, ms: u64) {
        self.deadline_ms.store(ms, std::sync::atomic::Ordering::SeqCst);
    }

    /// How long an idle, unshown harness chat stays loaded from now on (tests).
    #[cfg(all(test, unix))] // its tests run a bash stand-in for claude
    pub fn set_harness_idle(&self, ms: u64) {
        self.harness_idle_ms.store(ms, std::sync::atomic::Ordering::SeqCst);
    }

    /// One kernel event, stamped with the wall clock.
    fn log(&self, event: KernelEvent) {
        if let Some(log) = self.events.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            log.append(wall_ms(), event);
        }
    }

    /// An interrupt chat `id` got (runtime-kernel §15.5): source, kind and action, never text.
    fn log_interrupt(&self, id: &str, interrupt: Interrupt, action: marv_kernel::InterruptAction) {
        if let Some(agent) = agent(id) {
            self.log(KernelEvent::Interrupt {
                chat: AgentKey::Chat(agent),
                source: interrupt.source,
                kind: interrupt.kind,
                action,
            });
        }
    }

    /// Who's on the mesh, as the chats' `mesh_list` reads it.
    #[cfg(test)]
    pub fn registry(&self) -> &Arc<marv_mesh::registry::MeshRegistry> {
        &self.registry
    }

    /// Whether chat `id`'s session is loaded now (shown on a page, or running in the background).
    pub fn is_loaded(&self, id: &str) -> bool {
        self.inner().residents.get(id).is_some_and(|r| r.loaded.is_some())
    }

    /// The person marked chat `id` private, or not: its door publishes no write set (runtime-kernel §14.4).
    pub fn set_private(&self, id: &str, private: bool) {
        if let Some(mesh) = self.inner().residents.get(id).and_then(|r| r.keep.mesh.clone()) {
            mesh.set_private(private);
        }
    }

    /// The chats with a record here (the desktop's own, by id).
    pub fn resident_ids(&self) -> std::collections::BTreeSet<String> {
        self.inner().residents.keys().cloned().collect()
    }

    /// Runtime-kernel §14.4: each desktop chat's own write set (the paths its edit tools wrote in the last 30 minutes,
    /// as real paths), for the dock's file marks (kernel #53). The person's own view: a private chat's too.
    pub fn write_sets(&self) -> Vec<(String, Vec<marv_kernel::RecentWrite>)> {
        self.inner()
            .residents
            .iter()
            .filter_map(|(id, r)| r.keep.mesh.as_ref().map(|mesh| (id.clone(), mesh.recent_writes())))
            .filter(|(_, writes)| !writes.is_empty())
            .collect()
    }

    /// Runtime-kernel §14.4: each chat on the mesh whose edit tools wrote, in the last 30 minutes, a path another live
    /// chat (or another Marv) also wrote, with those others and the shared paths. Shown, never scheduled.
    pub fn write_overlaps(&self) -> Vec<(String, Vec<marv_kernel::Overlap>)> {
        let names: Vec<(String, String)> =
            self.inner().residents.iter().filter_map(|(id, r)| r.keep.mesh_name.clone().map(|name| (id.clone(), name))).collect();
        let mut out: Vec<(String, Vec<marv_kernel::Overlap>)> = names
            .into_iter()
            .map(|(id, name)| (id, self.registry.overlaps_of(&name)))
            .filter(|(_, overlaps)| !overlaps.is_empty())
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Logs the kernel's events to `log` from now on (runtime-kernel 3): chats put on the mesh after this log theirs.
    pub fn set_events(&self, log: Arc<dyn marv_kernel::EventLog>) {
        self.registry.set_events(log.clone());
        *self.events.lock().unwrap_or_else(|e| e.into_inner()) = Some(log);
    }

    /// Drops chat `id`'s record (its door closes with it) and its registry entry.
    fn drop_resident(&self, inner: &mut Inner, id: &str) -> Option<Resident> {
        let resident = inner.residents.remove(id)?;
        if let Some(name) = &resident.keep.mesh_name {
            self.registry.unregister_own(name);
        }
        Some(resident)
    }

    /// The registry's states for the chats, as the scheduler has them now.
    fn publish_states(&self, inner: &Inner) {
        for (id, resident) in &inner.residents {
            if let (Some(name), Some(agent)) = (&resident.keep.mesh_name, agent(id)) {
                let terminal =
                    inner.in_terminal.contains(id).then(|| inner.terminal_states.get(id).copied().unwrap_or(AgentState::Idle));
                self.registry.set_state(name, terminal.unwrap_or_else(|| inner.sched.state(&agent)));
            }
        }
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn now(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    /// Chat `id`'s record, made (and put on the mesh) if it has none. Needs a Tokio runtime.
    fn resident<'a>(&self, inner: &'a mut Inner, id: &str, folder: &Path) -> &'a mut Resident {
        if inner.residents.get(id).is_some_and(|r| r.folder != folder && r.loaded.is_none()) {
            // Its folder moved while it slept: a new record, on the mesh from the new folder.
            self.drop_resident(inner, id);
        }
        inner.residents.entry(id.to_string()).or_insert_with(|| self.make_resident(id, folder))
    }

    /// Puts chat `id`'s switchboard `mesh` to work for its record: cards, privacy, the main inbox, the event log, the
    /// registry, its saved card and its `chat_read` source. At its record's making, and again when a terminal Marv
    /// gives the chat's door back ([`Chats::terminal_closed`]).
    fn join_mesh(
        &self,
        id: &str,
        mesh: &marv_session::mesh_process::Mesh,
        session: &Arc<Mutex<Option<marv_session::MarvSession>>>,
        out: &mpsc::UnboundedSender<Value>,
    ) {
        mesh.handle.set_cards(self.cards.load(std::sync::atomic::Ordering::SeqCst));
        // Runtime-kernel §14.4: a private chat's door publishes no write set.
        mesh.handle.set_private(self.host.private(id));
        // The main agent's inbox is there from the start, loaded or not: a message to a sleeping chat waits in it.
        let _ = mesh.handle.plug(marv_session::mesh::parent_id());
        if let Some(log) = self.events.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            mesh.handle.set_events(log);
        }
        if mesh.register(&self.registry) {
            self.registry.set_state(&mesh.name, AgentState::Sleeping);
        }
        // Runtime-kernel chat card: until a turn ends here, its card is its saved session's. A turn that ended
        // meanwhile set a newer one, which this never replaces. The registry reads it from the switchboard.
        let (saved, handle) = (self.host.saved_card(id), mesh.handle.clone());
        self.host.spawn(Box::pin(async move {
            if let Some(card) = saved.await {
                handle.set_card_if_none(card);
            }
        }));

        mesh.handle.set_chat_source(Arc::new(ResidentSource {
            id: id.to_string(),
            host: self.host.clone(),
            session: Arc::downgrade(session),
            out: out.clone(),
        }));
    }

    fn make_resident(&self, id: &str, folder: &Path) -> Resident {
        let (out, events) = mpsc::unbounded_channel::<Value>();
        let approver = Arc::new(WebApprover::new(out.clone()));
        let mesh = self.host.start_mesh(id, folder, &approver, &out);
        let session: Arc<Mutex<Option<marv_session::MarvSession>>> = Arc::default();
        if let Some(mesh) = &mesh {
            self.join_mesh(id, mesh, &session, &out);
        }
        let keep = marv_web::host::Resident {
            mesh: mesh.as_ref().map(|m| m.handle.clone()),
            mesh_name: mesh.as_ref().map(|m| m.name.clone()),
            always: Default::default(),
            turn_hop: Default::default(),
            file_versions: Default::default(),
            approver,
            wake_limit: Default::default(),
            session,
            woken: Default::default(),
            harness_calls: Default::default(),
        };
        let mut resident = Resident {
            folder: folder.to_path_buf(),
            keep,
            mesh,
            shared: Arc::new(Mutex::new(Shared::default())),
            out,
            loaded: None,
            load: 0,
            ready: false,
            key: format!("|{}", 0),
            opened: false,
            pending: None,
            cant_wake: None,
            queue: Default::default(),
            paused: false,
            unattended: None,
            attended: Arc::default(),
            gate: Arc::default(),
            kick: Arc::default(),
            stop: Default::default(),
            harness: self.host.harness(id),
            meter: None,
            woke: None,
            inbox: None,
        };
        // A harness chat's numbers for Activity (runtime-kernel §20.2): its own events metered, on its door's `live`.
        if resident.harness {
            let meter = Arc::new(marv_session::live::LiveMeter::default());
            if let Some(mesh) = &resident.keep.mesh {
                show_meter(mesh, &meter);
            }
            resident.meter = Some(meter);
        }
        self.host.spawn(Box::pin(pump(self.me.clone(), id.to_string(), events, resident.stop.clone())));
        if resident.harness {
            let mesh = resident.keep.mesh.clone();
            let watch = watch_harness(self.me.clone(), id.to_string(), mesh, resident.kick.clone(), resident.stop.clone());
            self.host.spawn(Box::pin(watch));
        } else if let Some(mesh) = resident.keep.mesh.clone() {
            let watch = watch(self.me.clone(), id.to_string(), mesh, resident.kick.clone(), resident.stop.clone());
            self.host.spawn(Box::pin(watch));
        }
        resident
    }

    /// Loads chat `id`'s session (its record must exist), continuing its conversation with its last settings.
    fn load(&self, inner: &mut Inner, id: &str, key: String) -> Result<(), String> {
        if inner.in_terminal.contains(id) {
            return Err(IN_TERMINAL.into());
        }
        let mut spec = self.host.spec(id)?;
        let resident = inner.residents.get_mut(id).ok_or("That chat is gone.")?;
        spec.resident = Some(resident.keep.clone());
        spec.grant = self.host.grant(id, &resident.out);
        if let Some(line) = self.host.take_notice(id) {
            let _ = resident.out.send(serde_json::json!({"type": "notice", "text": line}));
        }
        spec.timers = spec.grant.as_ref().and_then(|grant| self.host.timers(id, grant));
        let (to_chat, from_page) = mpsc::unbounded_channel();
        resident.load += 1;
        resident.ready = false;
        resident.key = key.clone();
        resident.shared().unloaded();
        resident.loaded = Some(LiveChat { to_chat, key, shared: resident.shared.clone(), seen: Instant::now() });
        let (gate, load, me, id) = (resident.gate.clone(), resident.load, self.me.clone(), id.to_string());
        let run = self.host.run(spec, from_page, resident.out.clone());
        self.host.spawn(Box::pin(async move {
            // The last load's session closes (its last save) before this one resumes it.
            let _turn = gate.lock_owned().await;
            run.await;
            if let Some(chats) = me.upgrade() {
                chats.ended(&id, load);
            }
        }));
        Ok(())
    }

    /// Does what the scheduler says: unloads what it evicted, loads what a queued wake may now start. A wake that
    /// can't load (its folder is gone) is refused and told, and its slot goes to the next.
    fn apply(&self, inner: &mut Inner, mut actions: Vec<Action>) {
        while !actions.is_empty() {
            for action in std::mem::take(&mut actions) {
                match action {
                    Action::Evict(victim) => {
                        if let Some(resident) = inner.residents.get_mut(victim.as_str()) {
                            resident.unload();
                        }
                    }
                    Action::Start(next) => {
                        let id = next.as_str();
                        let Some(resident) = inner.residents.get_mut(id) else {
                            actions.extend(inner.sched.update(&next, AgentState::Sleeping, self.now()));
                            continue;
                        };
                        if resident.loaded.is_some() {
                            // Shown meanwhile: its watcher takes the wake once the session is idle.
                            resident.kick.notify_one();
                            continue;
                        }
                        let key = resident.key.clone();
                        if let Err(why) = self.load(inner, id, key) {
                            if let Some(resident) = inner.residents.get_mut(id) {
                                resident.pending = None;
                                resident.refuse_wake(&why);
                            }
                            actions.extend(inner.sched.update(&next, AgentState::Sleeping, self.now()));
                        }
                    }
                }
            }
        }
        self.publish_states(inner);
    }

    /// A load's session ended. If it's still the current load, it ended on its own (an error, or it closed): the page
    /// is told, and its slot frees. An unload already let it go.
    fn ended(&self, id: &str, load: u64) {
        let mut inner = self.inner();
        if let Some(resident) = inner.residents.get_mut(id)
            && resident.load == load
            && resident.loaded.is_some()
        {
            resident.loaded = None;
            resident.ready = false;
            resident.shared().end();
            if let Some(agent) = agent(id) {
                let actions = inner.sched.update(&agent, AgentState::Sleeping, self.now());
                self.apply(&mut inner, actions);
            }
        }
    }

    /// One event from chat `id`'s session or approver: kept and shown ([`Shared::deliver`]), and the scheduler told
    /// when its loaded session changes state (ready, a turn, an approval) or its page comes or goes.
    fn event(&self, id: &str, event: Value) {
        let ready = event["type"] == "ready";
        let turn_end = event["type"] == "turn_end";
        let kind = event["type"].as_str().map(str::to_string);
        let text = event["text"].as_str().map(str::to_string);
        let reason = event["reason"].as_str().unwrap_or("end").to_string();
        // A Claude Code chat's new session is saved once its first turn ends (`marv_web::claude_code`).
        if (ready || event["type"] == "session")
            && let Some(session) = event["sessionId"].as_str()
        {
            self.host.session_saved(id, session);
        }
        // The page gets it first; the notifier after, off the lock.
        let tell = matches!(
            event["type"].as_str(),
            Some("approval" | "approval_done" | "turn_end" | "paused" | "timer_void" | "budget")
        )
        .then(|| event.clone());
        let mut inner = self.inner();
        let Some(resident) = inner.residents.get_mut(id) else { return };
        let shown = resident.shared().shown();
        if let Some(meter) = &resident.meter {
            meter.observe_harness(&event, marv_session::time::now_ms());
        }
        resident.shared().deliver(event);
        if ready && resident.loaded.is_some() {
            resident.ready = true;
            resident.kick.notify_one();
        }
        let mut report = None;
        if resident.harness {
            if let Some(inbox) = resident.inbox.as_mut() {
                match kind.as_deref() {
                    Some("tool_start") => inbox.calls += 1,
                    Some("assistant") => inbox.reply = text.clone(),
                    _ => {}
                }
            }
            if turn_end {
                // Its woken turn is over: its report goes back (phase 3), and the next queued wake, if any, may run.
                report = resident
                    .inbox
                    .take()
                    .zip(resident.keep.mesh.clone())
                    .map(|(inbox, mesh)| (inbox, mesh, resident.out.clone()));
                (resident.unattended, resident.woke) = (None, None);
                resident.kick.notify_one();
            }
        }
        self.sync(&mut inner, id);
        drop(inner);
        if let Some((inbox, mesh, out)) = report {
            self.host.spawn(Box::pin(async move {
                let reply = inbox.reply.as_deref();
                if let Some(line) =
                    marv_session::task_report::harness_report(&mesh, &inbox.turn, &reason, inbox.calls, reply).await
                {
                    let _ = out.send(serde_json::json!({"type": "notice", "text": marv_core::printable(&line)}));
                }
            }));
        }
        if let Some(event) = tell {
            self.host.notify(id, &event, shown);
        }
    }

    /// Tells the scheduler chat `id`'s state as its events say (once its load is ready), and whether a page shows it.
    /// A wake that waits to run on it counts as a turn: a chat loaded for a wake isn't evicted before the wake runs.
    fn sync(&self, inner: &mut Inner, id: &str) {
        let Some(resident) = inner.residents.get(id) else { return };
        let now = resident.ready.then(|| {
            let shared = resident.shared();
            let state = if shared.waiting() > 0 {
                AgentState::Waiting
            } else if !shared.idle() || resident.pending.is_some() {
                AgentState::Busy
            } else {
                AgentState::Idle
            };
            (state, shared.shown())
        });
        if let (Some((state, shown)), Some(agent)) = (now, agent(id)) {
            let mut actions = Vec::new();
            if inner.sched.attached(&agent) != shown {
                actions.extend(inner.sched.attach(&agent, shown, self.now()));
            }
            if inner.sched.state(&agent) != state {
                actions.extend(inner.sched.update(&agent, state, self.now()));
            }
            self.apply(inner, actions);
        }
    }

    /// The person opens chat `id` (in `folder`) on a page: shown again if its session runs with the same `key` (the
    /// page's settings applied), else loaded, continuing its conversation. `seen`: the last event the page saved, and
    /// the numbering it's in (`background::Shared::attach`). The person never waits for a slot (D12): over K, the
    /// next chat that can go is unloaded as soon as one can. Needs a Tokio runtime.
    // Loads the chat's session for a page (before every chat was its agent's terminal: `attach`); kept, with its
    // tests, for the headless loads wakes still make until team mail goes to the terminals.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn open(
        &self,
        id: &str,
        folder: &Path,
        key: String,
        configure: ClientMessage,
        sink: Sink,
        (after, epoch): (u64, Option<&str>),
    ) -> Result<(), String> {
        let mut inner = self.inner();
        let now = self.now();
        inner.stopped.remove(id);
        let resident = self.resident(&mut inner, id, folder);
        resident.opened = true;
        let agent = agent(id);
        if let Some(loaded) = resident.loaded.as_mut().filter(|l| l.key == key && l.running()) {
            loaded.seen = Instant::now();
            let _ = loaded.to_chat.send(configure);
            resident.shared().attach(sink, after, epoch);
            if let Some(agent) = &agent {
                inner.sched.request(agent.clone(), Request::Open { by_person: true }, now);
                let actions = inner.sched.attach(agent, true, now);
                self.apply(&mut inner, actions);
            }
            return Ok(());
        }
        // Its servers changed (or it ended): loaded again below, continuing the conversation.
        let was_loaded = resident.loaded.is_some();
        resident.unload();
        if let Some(agent) = &agent {
            if was_loaded {
                let actions = inner.sched.update(agent, AgentState::Sleeping, now);
                self.apply(&mut inner, actions);
            }
            if let Admit::RunAfterEvicting(victim) = inner.sched.request(agent.clone(), Request::Open { by_person: true }, now)
                && let Some(victim) = inner.residents.get_mut(victim.as_str())
            {
                victim.unload();
            }
        }
        if let Err(why) = self.load(&mut inner, id, key) {
            if let Some(agent) = &agent {
                let actions = inner.sched.update(agent, AgentState::Sleeping, now);
                self.apply(&mut inner, actions);
            }
            return Err(why);
        }
        let resident = inner.residents.get_mut(id).ok_or("That chat is gone.")?;
        resident.cant_wake = None;
        resident.shared().attach(sink, after, epoch);
        if let Some(agent) = &agent {
            let actions = inner.sched.attach(agent, true, now);
            self.apply(&mut inner, actions);
        }
        Ok(())
    }

    /// A page shows chat `id`, whose agent runs in its terminal (Marv OS's chats are the agents' own terminals,
    /// `marv_web::terminal_agents`): its record is made (its mesh door, its cards) and the page attached to its events,
    /// with no session loaded. `ready` says which agent runs it. Its terminal is opened apart (`chat_terminal_open`).
    pub fn attach(&self, id: &str, folder: &Path, harness: &str, sink: Sink, (after, epoch): (u64, Option<&str>)) {
        let mut inner = self.inner();
        let now = self.now();
        inner.stopped.remove(id);
        let resident = self.resident(&mut inner, id, folder);
        resident.opened = true;
        resident.shared().attach(sink, after, epoch);
        let mesh = resident.keep.mesh_name.clone();
        let _ = resident
            .out
            .send(serde_json::json!({"type": "ready", "harness": harness, "mesh": mesh, "terminal": true, "commands": []}));
        if let Some(agent) = agent(id) {
            let actions = inner.sched.attach(&agent, true, now);
            self.apply(&mut inner, actions);
        }
    }

    /// A message from the page. A sleeping chat takes only answers to its door's wire requests.
    ///
    /// Runtime-kernel §15.2: the person's message goes into the chat's interrupt queue and its watcher runs it, ahead of
    /// any wake or timer waiting (a wake that started a moment before runs first; the message is then the next turn).
    pub fn message(&self, id: &str, message: ClientMessage) -> Result<(), String> {
        let mut inner = self.inner();
        let resident = inner.residents.get_mut(id).ok_or("This chat isn't open.")?;
        let now = turn_now(resident);
        match (&resident.loaded, message) {
            (Some(_), ClientMessage::Send { text }) if resident.keep.mesh.is_some() && !resident.harness => {
                let text = text.trim().to_string();
                if text.is_empty() {
                    return Ok(());
                }
                resident.queue.push(InterruptSource::Person, Queued::Person(text));
                resident.kick.notify_one();
                drop(inner);
                if now != TurnNow::Idle {
                    let interrupt = Interrupt::new(InterruptSource::Person, InterruptKind::Message);
                    self.log_interrupt(id, interrupt, handling(interrupt, now));
                }
                Ok(())
            }
            (Some(loaded), message) => {
                // A harness chat's person typing lifts the rule wakes' pause, as a Marv chat's does (`run_person`), and
                // their turn reads the unread mail, fenced under their words, as a Marv chat's turn reads it at its start.
                let message = match message {
                    ClientMessage::Send { text } if resident.harness => {
                        resident.keep.wake_limit.lock().unwrap_or_else(|e| e.into_inner()).person_typed();
                        resident.keep.turn_hop.new_turn(0);
                        let mail = resident.keep.mesh.as_ref().and_then(|m| marv_session::mesh::harness_inbox(m));
                        ClientMessage::Send { text: mail.map_or_else(|| text.clone(), |mail| format!("{text}\n\n{mail}")) }
                    }
                    message => message,
                };
                // Review P3-7: the person sending into an unattended turn makes it theirs: no deadline, no stop for
                // its grant's end.
                if matches!(message, ClientMessage::SendNow { .. }) && resident.unattended.is_some() {
                    resident.unattended = None;
                    resident.attended.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                let kind = match &message {
                    ClientMessage::SendNow { .. } => Some(InterruptKind::SendNow),
                    ClientMessage::Interrupt => Some(InterruptKind::Stop),
                    _ => None,
                };
                let sent = loaded.to_chat.send(message).map_err(|_| "This chat has closed.".to_string());
                drop(inner);
                if let Some(kind) = kind.filter(|_| now != TurnNow::Idle) {
                    let interrupt = Interrupt::new(InterruptSource::Person, kind);
                    self.log_interrupt(id, interrupt, handling(interrupt, now));
                }
                sent
            }
            (None, ClientMessage::Approval { id, decision }) => {
                resident.keep.approver.answer(id, decision);
                Ok(())
            }
            (None, _) => Err("This chat isn't open.".into()),
        }
    }

    /// The page stopped showing the chat: it keeps running (on the mesh, its turn going on) and keeps what happens.
    /// Over K (a person's open), it may now be the one unloaded.
    pub fn close(&self, id: &str) {
        let mut inner = self.inner();
        let Some(resident) = inner.residents.get_mut(id) else { return };
        resident.shared().detach();
        if let Some(loaded) = resident.loaded.as_mut() {
            loaded.seen = Instant::now();
        }
        if let Some(agent) = agent(id) {
            let actions = inner.sched.attach(&agent, false, self.now());
            self.apply(&mut inner, actions);
        }
    }

    /// Chat `id`'s loaded session, if it has one (the dock's processes: mk-60m.11). None while it sleeps or isn't open.
    pub fn session(&self, id: &str) -> Option<marv_session::MarvSession> {
        self.with_resident(id, |r| r.keep.session()).flatten()
    }

    /// The person stopped the chat (or deleted it): its session ends and it leaves the mesh until it's opened again.
    pub fn stop(&self, id: &str) {
        let mut inner = self.inner();
        self.drop_resident(&mut inner, id);
        inner.stopped.insert(id.to_string());
        if let Some(agent) = agent(id) {
            let actions = inner.sched.remove(&agent, self.now());
            self.apply(&mut inner, actions);
        }
    }

    /// Kernel #41: the chats with a turn running now (the person's or an unattended one), for the watchdog, which
    /// stops them through [`Chats::stop`] with a notice before the app exits.
    pub fn busy_ids(&self) -> Vec<String> {
        let inner = self.inner();
        // A terminal agent is working or asking by what its terminal says (its session isn't loaded).
        let terminal = |id: &String| {
            inner.in_terminal.contains(id)
                && matches!(inner.terminal_states.get(id), Some(AgentState::Busy | AgentState::Waiting))
        };
        let mut ids: Vec<String> = inner
            .residents
            .iter()
            .filter(|(id, r)| turn_now(r) != TurnNow::Idle || terminal(id))
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        ids
    }

    /// Kernel #41, as the app is closed (SIGTERM): stops every chat with a running turn through [`Chats::stop`]. The
    /// ids stopped, and their sessions, so the caller can wait a moment for their turns to end.
    pub fn stop_running(&self) -> (Vec<String>, Vec<marv_session::MarvSession>) {
        let ids = self.busy_ids();
        let sessions = {
            let inner = self.inner();
            ids.iter().filter_map(|id| inner.residents.get(id).and_then(|r| r.keep.session())).collect()
        };
        for id in &ids {
            self.stop(id);
        }
        (ids, sessions)
    }

    /// How many chats have a record now (loaded or asleep).
    #[cfg(test)]
    pub fn resident_count(&self) -> usize {
        self.inner().residents.len()
    }

    /// For the sidebar.
    pub fn states(&self) -> Vec<background::LiveState> {
        let inner = self.inner();
        let mut states = background::states(&inner.residents);
        // A chat whose agent runs in its terminal is listed even when nothing else would list it: a terminal Marv has
        // lent the chat's door away (no mesh), isn't a harness and has no session loaded, so it was left out, and the
        // page said "not running" beside a Marv that ran.
        for id in &inner.in_terminal {
            if let Some(r) = inner.residents.get(id)
                && !states.iter().any(|s| &s.id == id)
            {
                states.push(background::LiveState {
                    id: id.clone(),
                    busy: false,
                    waiting: 0,
                    asleep: false,
                    cant_wake: None,
                    paused: r.paused,
                });
            }
        }
        for state in &mut states {
            if inner.in_terminal.contains(&state.id) {
                let now = inner.terminal_states.get(&state.id).copied().unwrap_or(AgentState::Idle);
                state.asleep = false;
                state.busy = now == AgentState::Busy;
                state.waiting = state.waiting.max(usize::from(now == AgentState::Waiting));
            }
        }
        states
    }

    /// D13: every chat a mesh rule covers keeps a door while the app runs, opened this run or not. `covered`: those
    /// chats now (id and folder, checked by the caller). A record only a rule brought up, asleep with nothing waiting,
    /// leaves the mesh once no rule covers it. Needs a Tokio runtime.
    /// `mesh.cards` as the person's config says it now (runtime-kernel chat card): every chat publishes its card and
    /// shows its model covered Marvs' cards' words only while it's on. The app reads the config off this lock (at
    /// start and on each rules look) and gives the answer here.
    pub fn set_cards(&self, on: bool) {
        self.cards.store(on, std::sync::atomic::Ordering::SeqCst);
        for resident in self.inner().residents.values() {
            if let Some(mesh) = &resident.keep.mesh {
                mesh.set_cards(on);
            }
        }
    }

    pub fn sync_rules(&self, covered: &[(String, PathBuf)]) {
        let mut inner = self.inner();
        for (id, folder) in covered {
            if !inner.stopped.contains(id) && !inner.residents.contains_key(id) {
                self.resident(&mut inner, id, folder);
            }
        }
        let keep: HashSet<&String> = covered.iter().map(|(id, _)| id).collect();
        let gone: Vec<String> = inner
            .residents
            .iter()
            .filter(|(id, r)| {
                !(r.opened
                    || keep.contains(id)
                    || r.loaded.is_some()
                    || r.pending.is_some()
                    || !r.queue.is_empty()
                    || r.shared().waiting() > 0)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in gone {
            self.drop_resident(&mut inner, &id);
            if let Some(agent) = agent(&id) {
                let actions = inner.sched.remove(&agent, self.now());
                self.apply(&mut inner, actions);
            }
        }
    }

    /// One look at chat `id`'s interrupt queue and inbox (runtime-kernel §15.2: person > kernel > mail > radio > timer).
    /// Loaded and idle with nothing to answer: the person's queued message runs first; then (unless the chat is
    /// paused) a wake the scheduler admitted, a message on a wire that may wake it (as `watch_mesh` did), or a fired
    /// timer. Asleep: within the same wake rules ([`check_wake`]: the wire, the per-wire and rule limits, the hop),
    /// or for a fired timer, the wake asks the scheduler for a slot; the session loads (its last settings, its saved
    /// conversation and taint) and the wake runs then, or it waits its turn in the queue. Every woken turn runs with
    /// automatic approval off ([`run_wake_then`]) and under the unattended deadline ([`Chats::unattended`]).
    async fn look(&self, id: &str) {
        let (keep, out, loading, pending, paused, timer_waits) = {
            let inner = self.inner();
            let Some(r) = inner.residents.get(id) else { return };
            let timer_waits = r.queue.has(InterruptSource::Timer);
            (r.keep.clone(), r.out.clone(), r.loaded.is_some(), r.pending.is_some(), r.paused, timer_waits)
        };
        // Every chat is its agent's terminal (the desktop): mail and timers start it, rather than a session no one sees.
        // (Its door lent to its terminal's Marv, there's no mesh here: its mail is Marv's own.)
        if self.host.terminal_only(id) {
            if let Some(mesh) = keep.mesh.as_ref()
                && !keep.approver.waiting()
            {
                self.mail_to_terminal(id, mesh, &keep, &out, paused).await;
            }
            self.timer_to_terminal(id).await;
            return;
        }
        let Some(mesh) = keep.mesh.as_ref() else { return };
        if keep.approver.waiting() {
            return;
        }
        if let Some(session) = keep.session() {
            if session.busy() {
                return;
            }
            // The person first: a message they sent always runs before any queued wake or timer.
            if let Some(text) = self.with_resident(id, |r| r.take_person()).flatten() {
                // The person writing to a paused chat resumes it (as `DEADLINE_TEXT` says).
                if self.with_resident(id, |r| {
                    let was = r.paused;
                    r.set_paused(false);
                    was
                }) == Some(true)
                {
                    let _ = out.send(serde_json::json!({"type": "notice", "text": RESUMED_TEXT}));
                }
                run_person(&session, &text, &out, &keep.wake_limit).await;
                self.again(id);
                return;
            }
            if paused {
                return;
            }
            let admitted = self.with_resident(id, |r| r.pending.take()).flatten();
            let wake = admitted
                .or_else(|| check_wake(mesh, &keep.wake_limit, &out, self.host.mesh_wake()).map(Pending::Mail))
                .or_else(|| self.with_resident(id, |r| r.take_timer()).flatten().map(Pending::Timer));
            if let Some(wake) = wake {
                self.unattended(id, &session, &keep, &out, wake).await;
                self.again(id);
            }
            return;
        }
        // Loading, or queued for a slot: the wake runs once it's loaded.
        let unread = !mesh.unread(&marv_session::mesh::parent_id()).is_empty();
        if loading || pending || paused || !(unread || timer_waits) {
            return;
        }
        // Refused before it's counted: a chat that can't load (its folder is gone) isn't charged a wake for it.
        if let Err(why) = self.host.spec(id) {
            self.with_resident(id, |r| r.refuse_wake(&why));
            return;
        }
        let by = if unread { check_wake(mesh, &keep.wake_limit, &out, self.host.mesh_wake()).map(Pending::Mail) } else { None };
        let Some(by) = by.or_else(|| self.with_resident(id, |r| r.take_timer()).flatten().map(Pending::Timer)) else { return };
        let Some(agent) = agent(id) else { return };
        let mut inner = self.inner();
        let now = self.now();
        let Some(resident) = inner.residents.get_mut(id) else { return };
        resident.cant_wake = None;
        let by_name = match &by {
            Pending::Mail(by) => by.sender.to_string(),
            Pending::Timer(_) => "timer".to_string(),
        };
        resident.pending = Some(by);
        let admit = inner.sched.request(agent.clone(), Request::Wake { by: by_name }, now);
        let start = match admit {
            Admit::Run => true,
            Admit::RunAfterEvicting(victim) => {
                if let Some(victim) = inner.residents.get_mut(victim.as_str()) {
                    victim.unload();
                }
                true
            }
            // Its turn comes when a slot frees (`Action::Start`), in arrival order.
            Admit::Queued { .. } => false,
        };
        if start {
            self.apply(&mut inner, vec![Action::Start(agent)]);
        }
    }

    /// The harness plan, phases 2 and 3: a look at a chat another harness runs (Claude Code). Mail on its inbox (within
    /// the same wake rules as a Marv chat's: [`check_wake`]) and fired timers wake it; the person's messages go
    /// straight to it. Loaded, ready and idle with nothing to answer: mail first, then the next timer, starts its turn,
    /// sent to the harness as a message after Marv's line for the transcript (mail fenced, as other agents' words).
    /// Asleep with mail or a timer waiting: it asks the scheduler for a slot and loads (the harness resumes its
    /// conversation), and the wake runs once the load is ready. Its turn runs under the same deadline as Marv's
    /// ([`Chats::tick`]).
    async fn look_harness(&self, id: &str) {
        let (keep, out, folder, loaded, ready, idle, pending, paused, timer_waits, running) = {
            let inner = self.inner();
            let Some(r) = inner.residents.get(id) else { return };
            let idle = r.shared().idle();
            (
                r.keep.clone(),
                r.out.clone(),
                r.folder.clone(),
                r.loaded.is_some(),
                r.ready,
                idle,
                r.pending.is_some(),
                r.paused,
                r.queue.has(InterruptSource::Timer),
                r.unattended.is_some(),
            )
        };
        let unread = keep.mesh.as_ref().is_some_and(|m| !m.unread(&marv_session::mesh::parent_id()).is_empty());
        if paused || running || keep.approver.waiting() {
            return;
        }
        // Its agent runs in its terminal: mail is typed in once it's idle (never over a turn or a question).
        let terminal = {
            let inner = self.inner();
            let not_ready = inner.starting.get(id).is_some_and(|drawn| !drawn);
            inner.in_terminal.contains(id).then(|| (inner.terminal_states.get(id).copied(), not_ready))
        };
        if let Some((state, not_ready)) = terminal {
            if !unread {
                self.mail_read(id, &out);
                self.timer_to_terminal(id).await;
                return;
            }
            if not_ready || !matches!(state, Some(AgentState::Idle)) {
                return;
            }
            let Some(mesh) = keep.mesh.as_ref() else { return };
            if self.waits_on_person(id) {
                return;
            }
            let (wake, waiting) = check_wake_waiting(mesh, &keep.wake_limit, &out, self.host.mesh_wake());
            self.mail_waits(id, mesh, &out, &waiting);
            let Some(by) = wake else { return };
            self.mail_read(id, &out);
            // As a headless chat's mail turn (`harness_mail`): whether it's a task, its report back when it ends.
            let role = self.member_role(id);
            let turn = marv_session::task_report::inbox_turn_for(mesh, by.clone(), role.as_ref()).await;
            if let Some(hop) = mesh.turn_hop(&marv_session::mesh::parent_id()) {
                hop.new_turn(0);
            }
            let Some(note) = marv_session::mesh::harness_inbox(mesh) else { return };
            mesh.log_wake(&by.sender, None);
            let first = self.inner().briefed.insert(id.to_string());
            let brief = if first { self.host.terminal_brief(id) } else { None };
            let text = match brief {
                Some(brief) => format!("[Marv OS: how this chat works]\n{brief}\n\n---\n\n{note}\n\n{HARNESS_MAIL_NOTE}"),
                None => format!("{note}\n\n{HARNESS_MAIL_NOTE}"),
            };
            {
                let mut inner = self.inner();
                inner.starting.remove(id);
                inner.mail_turn.insert(id.to_string(), false);
                if let Some(r) = inner.residents.get_mut(id) {
                    r.inbox = Some(background::HarnessInbox { turn, calls: 1, reply: None });
                    let _ = r.out.send(serde_json::json!({"type": "notice", "text": by.line()}));
                }
            }
            if !self.host.type_into(id, &text) {
                let _ =
                    out.send(serde_json::json!({"type": "notice", "text": "Mail came, but this chat's terminal isn't running."}));
            }
            return;
        }
        if loaded {
            if !ready || !idle {
                return;
            }
            let admitted = self.with_resident(id, |r| r.pending.take()).flatten();
            let mail = match &admitted {
                Some(Pending::Mail(by)) => Some(by.clone()),
                Some(Pending::Timer(_)) => None,
                None if unread => keep.mesh.as_ref().and_then(|m| check_wake(m, &keep.wake_limit, &out, self.host.mesh_wake())),
                None => None,
            };
            if let (Some(by), Some(mesh)) = (mail, keep.mesh.as_ref()) {
                self.harness_mail(id, mesh, by).await;
                self.again(id);
                return;
            }
            let next = match admitted {
                Some(Pending::Timer(timer)) => Some(timer),
                _ => self.with_resident(id, |r| r.take_timer()).flatten(),
            };
            let Some(timer) = next else { return };
            let live = self.harness_grant(id, &folder);
            let Some(timer) = self.start_timer(id, live, &keep, timer).await else {
                self.again(id);
                return;
            };
            self.with_resident(id, |r| {
                let Some(loaded) = &r.loaded else { return };
                (r.unattended, r.woke) = (Some(live.is_some()), Some(Instant::now()));
                r.attended.store(false, std::sync::atomic::Ordering::SeqCst);
                let _ = r.out.send(serde_json::json!({"type": "notice", "text": timer.line()}));
                r.keep.woken.store(true, std::sync::atomic::Ordering::SeqCst);
                let _ = loaded.to_chat.send(ClientMessage::Send { text: timer.for_model() });
            });
            self.again(id);
            return;
        }
        if pending || !(timer_waits || unread) {
            return;
        }
        // Its agent runs in its terminal: mail and timers start the terminal, and go in once the agent is ready.
        if !unread && timer_waits && self.host.terminal_only(id) {
            self.timer_to_terminal(id).await;
            return;
        }
        if unread && self.host.terminal_only(id) {
            self.inner().starting.insert(id.to_string(), false);
            if let Err(why) = self.host.start_terminal(id) {
                self.inner().starting.remove(id);
                self.with_resident(id, |r| r.refuse_wake(&why));
            }
            return;
        }
        // Refused before it's counted: a chat that can't load (its folder is gone, no `claude`) keeps its mail and timer.
        if let Err(why) = self.host.spec(id) {
            self.with_resident(id, |r| r.refuse_wake(&why));
            return;
        }
        let mail = match (&keep.mesh, unread) {
            (Some(mesh), true) => check_wake(mesh, &keep.wake_limit, &out, self.host.mesh_wake()).map(Pending::Mail),
            _ => None,
        };
        let Some(agent) = agent(id) else { return };
        let mut inner = self.inner();
        let now = self.now();
        let Some(resident) = inner.residents.get_mut(id) else { return };
        let Some(wake) = mail.or_else(|| resident.take_timer().map(Pending::Timer)) else { return };
        let by = match &wake {
            Pending::Mail(by) => by.sender.to_string(),
            Pending::Timer(_) => "timer".to_string(),
        };
        resident.cant_wake = None;
        resident.pending = Some(wake);
        let start = match inner.sched.request(agent.clone(), Request::Wake { by }, now) {
            Admit::Run => true,
            Admit::RunAfterEvicting(victim) => {
                if let Some(victim) = inner.residents.get_mut(victim.as_str()) {
                    victim.unload();
                }
                true
            }
            Admit::Queued { .. } => false,
        };
        if start {
            self.apply(&mut inner, vec![Action::Start(agent)]);
        }
    }

    /// Phase 3: mail woke harness chat `id` (by `by`): its unread inbox, fenced as other agents' words (the same note a
    /// Marv chat's model reads), goes to the harness as its turn's message, after Marv's line for the transcript.
    /// Whether it's a task turn (its role takes tasks from the sender) is decided first, from the unread message, as for
    /// a Marv chat; the turn's report goes back when it ends ([`Chats::event`]).
    async fn harness_mail(&self, id: &str, mesh: &marv_session::mesh::MeshHandle, by: marv_session::mesh::WakeBy) {
        let role = self.member_role(id);
        let turn = marv_session::task_report::inbox_turn_for(mesh, by.clone(), role.as_ref()).await;
        if let Some(hop) = mesh.turn_hop(&marv_session::mesh::parent_id()) {
            hop.new_turn(0);
        }
        let Some(note) = marv_session::mesh::harness_inbox(mesh) else { return };
        mesh.log_wake(&by.sender, None);
        self.with_resident(id, |r| {
            let Some(loaded) = &r.loaded else { return };
            r.inbox = Some(background::HarnessInbox { turn, calls: 0, reply: None });
            (r.unattended, r.woke) = (Some(false), Some(Instant::now()));
            r.attended.store(false, std::sync::atomic::Ordering::SeqCst);
            let _ = r.out.send(serde_json::json!({"type": "notice", "text": by.line()}));
            let text = format!("{note}\n\n{HARNESS_MAIL_NOTE}");
            r.keep.woken.store(true, std::sync::atomic::Ordering::SeqCst);
            let _ = loaded.to_chat.send(ClientMessage::Send { text });
        });
    }

    /// Mail for a Marv chat on the desktop, whose agent is the `marv` in its terminal: its terminal starts (taking the
    /// chat's door: its mesh name) and the mail, taken from the door's inbox first so none is lost with it, is typed in
    /// once Marv is ready. Its terminal already running has the door and its mail itself.
    async fn mail_to_terminal(
        &self,
        id: &str,
        mesh: &marv_session::mesh::MeshHandle,
        keep: &marv_web::host::Resident,
        out: &mpsc::UnboundedSender<Value>,
        paused: bool,
    ) {
        if paused || self.inner().in_terminal.contains(id) || mesh.unread(&marv_session::mesh::parent_id()).is_empty() {
            return;
        }
        let Some(by) = check_wake(mesh, &keep.wake_limit, out, self.host.mesh_wake()) else { return };
        let Some(note) = marv_session::mesh::harness_inbox(mesh) else { return };
        mesh.log_wake(&by.sender, None);
        let _ = out.send(serde_json::json!({"type": "notice", "text": by.line()}));
        if let Err(why) = self.host.start_terminal(id) {
            self.with_resident(id, |r| r.refuse_wake(&why));
            return;
        }
        self.type_when_ready(id, format!("{note}\n\n{HARNESS_MAIL_NOTE}"), true);
    }

    /// A timer due for chat `id`, whose agent runs in its terminal: typed in once the agent is idle and ready, as an
    /// unattended turn (its deadline and its grant hold over it); its terminal starts for it if it isn't running
    /// (inside the outer sandbox: nobody watches). The person first: never over their turn or a question.
    async fn timer_to_terminal(&self, id: &str) {
        let (waits, running, ready, folder, keep) = {
            let inner = self.inner();
            let Some(r) = inner.residents.get(id) else { return };
            let ready = matches!(inner.terminal_states.get(id), Some(AgentState::Idle))
                && !inner.starting.get(id).is_some_and(|drawn| !drawn)
                && !inner.timer_turn.contains_key(id)
                && !inner.mail_turn.contains_key(id);
            (r.queue.has(InterruptSource::Timer), inner.in_terminal.contains(id), ready, r.folder.clone(), r.keep.clone())
        };
        if !waits || keep.approver.waiting() || self.with_resident(id, |r| r.paused).unwrap_or(true) {
            return;
        }
        if !running {
            self.inner().starting.insert(id.to_string(), false);
            if let Err(why) = self.host.start_terminal(id) {
                self.inner().starting.remove(id);
                self.with_resident(id, |r| r.refuse_wake(&why));
            }
            return;
        }
        if !ready || self.waits_on_person(id) {
            return;
        }
        let Some(timer) = self.with_resident(id, |r| r.take_timer()).flatten() else { return };
        let live = self.harness_grant(id, &folder);
        let Some(timer) = self.start_timer(id, live, &keep, timer).await else { return };
        {
            let mut inner = self.inner();
            inner.timer_turn.insert(id.to_string(), false);
            if let Some(r) = inner.residents.get_mut(id) {
                (r.unattended, r.woke) = (Some(live.is_some()), Some(Instant::now()));
                r.attended.store(false, std::sync::atomic::Ordering::SeqCst);
                let _ = r.out.send(serde_json::json!({"type": "notice", "text": timer.line()}));
            }
        }
        self.host.type_into(id, &timer.for_model());
    }

    /// Chat `id`'s role as a team member (whose mesh tasks it takes), for its reports. None: no role.
    fn member_role(&self, id: &str) -> Option<marv_session::roles::Role> {
        let spec = self.host.spec(id).or_else(|_| self.host.terminal_spec(id)).ok()?;
        let roster = spec.team.as_ref().map(|(_, roster)| roster.as_slice());
        let home = marv_session::paths::home_dir();
        let name = spec.role.as_deref()?;
        marv_session::teams::harness_role(&spec.root, &home, &spec.config_dir, name, spec.writes.as_deref(), roster).ok()
    }

    /// Chat `id`'s live grant as a harness chat's timer turn carries it: held now and covering its folder.
    fn harness_grant(&self, id: &str, folder: &Path) -> Option<marv_kernel::GrantId> {
        let (quiet, _drop) = mpsc::unbounded_channel();
        self.host.grant(id, &quiet).filter(|g| g.covers(folder)).and_then(|g| g.live())
    }

    /// Chat `id`'s terminal agent asks the person now (`asking`: its permission prompt, a question only they can
    /// answer), or stopped: its "needs you" notification, or that notification withdrawn, as a chat view's approval's
    /// (an approval with no tool named: the question is on its screen). The events to tell, off the lock.
    fn terminal_asking(inner: &mut Inner, id: &str, asking: bool) -> Vec<(Value, bool)> {
        let shown = inner.residents.get(id).is_some_and(|r| r.shared().shown());
        let had = inner.terminal_ask.get(id).copied();
        match (asking, had) {
            (true, None) => {
                // Ids of their own, far from a session's approval ids.
                inner.next_ask += 1;
                let ask = (1 << 40) + inner.next_ask;
                inner.terminal_ask.insert(id.to_string(), ask);
                vec![(serde_json::json!({"type": "approval", "id": ask, "tool": ""}), shown)]
            }
            (false, Some(ask)) => {
                inner.terminal_ask.remove(id);
                vec![(serde_json::json!({"type": "approval_done", "id": ask}), shown)]
            }
            _ => Vec::new(),
        }
    }

    /// Chat `id`'s terminal agent finished a turn (its hooks said: Claude Code's `Stop`, Codex's `notify`): told as a
    /// chat view's turn end is (a notification when the person isn't looking; a team all idle, one for the team).
    pub fn terminal_finished(&self, id: &str) {
        let shown = {
            let inner = self.inner();
            if !inner.in_terminal.contains(id) {
                return;
            }
            inner.residents.get(id).is_some_and(|r| r.shared().shown())
        };
        self.host.notify(id, &serde_json::json!({"type": "turn_end", "reason": "end"}), shown);
    }

    /// The person sent a line to chat `id`'s terminal agent: as their message in a chat view, it starts their own turn
    /// (hop 0: what the agent sends from it may start a teammate's task turn) and gives its team wakes back (mail had
    /// stopped waking it at its team's budget "until you write in this chat"). A terminal Marv keeps its own.
    /// Mail in terminal chat `id` that doesn't start a turn (a wire its person approved by hand, say): a card over its
    /// terminal says who it's from, to deliver it with a click, and its sender is told it waits (a "waiting for its
    /// user" report), so it doesn't sit in `mesh_wait` for a reply that can't come. Before, the only word of it was a
    /// notice on a page the terminal hides. Reports never get a card: they're read at the agent's next mesh call.
    fn mail_waits(
        &self,
        id: &str,
        mesh: &marv_session::mesh::MeshHandle,
        out: &tokio::sync::mpsc::UnboundedSender<Value>,
        waiting: &[marv_session::mesh::Waiting],
    ) {
        let shown: Vec<_> = waiting.iter().filter(|w| w.why != marv_session::mesh::WakeSkip::Report).collect();
        if shown.is_empty() {
            return;
        }
        self.inner().mail_shown.insert(id.to_string());
        for w in shown {
            let from = marv_mesh::discovery::peer_session(&w.by.sender).map_or_else(|| w.by.sender.to_string(), String::from);
            let _ = out.send(serde_json::json!({"type": "mail_waiting", "from": marv_core::printable(&from), "count": w.count}));
            let (mesh, by, out) = (mesh.clone(), w.by.clone(), out.clone());
            tokio::spawn(async move {
                if let Some(line) = marv_session::task_report::waiting_report(&mesh, &by).await {
                    let _ = out.send(serde_json::json!({"type": "notice", "text": line}));
                }
            });
        }
    }

    /// Chat `id`'s waiting mail was read (delivered, or taken by a mesh call): its card goes.
    fn mail_read(&self, id: &str, out: &tokio::sync::mpsc::UnboundedSender<Value>) {
        if self.inner().mail_shown.remove(id) {
            let _ = out.send(serde_json::json!({"type": "mail_waiting", "clear": true}));
        }
    }

    /// The person delivers terminal chat `id`'s waiting mail (its card's button): typed in as a woken turn's would be,
    /// fenced as another agent's words. Their click is their go-ahead to show it, not to act on it.
    pub fn deliver_mail(&self, id: &str) -> Result<(), String> {
        let (keep, out) = {
            let inner = self.inner();
            let r = inner.residents.get(id).ok_or("This chat isn't open.")?;
            (r.keep.clone(), r.out.clone())
        };
        let _ = out.send(serde_json::json!({"type": "mail_waiting", "clear": true}));
        self.inner().mail_shown.remove(id);
        if !matches!(self.inner().terminal_states.get(id), Some(AgentState::Idle)) {
            return Err("Its agent is busy: deliver the message once it's done.".into());
        }
        let mesh = keep.mesh.as_ref().ok_or("This chat isn't on the mesh.")?;
        keep.wake_limit.lock().unwrap_or_else(|e| e.into_inner()).person_typed();
        keep.turn_hop.new_turn(0);
        let note = marv_session::mesh::harness_inbox(mesh).ok_or("No message is waiting any more.")?;
        if !self.host.type_into(id, &format!("{note}\n\n{HARNESS_MAIL_NOTE}")) {
            return Err("This chat's terminal isn't running.".into());
        }
        Ok(())
    }

    pub fn person_typed(&self, id: &str) {
        let inner = self.inner();
        if inner.lent.contains(id) {
            return;
        }
        if let Some(r) = inner.residents.get(id) {
            r.keep.wake_limit.lock().unwrap_or_else(|e| e.into_inner()).person_typed();
            r.keep.turn_hop.new_turn(0);
        }
    }

    /// Chat `id`'s terminal agent started (`agent`: its name, as Activity's model column shows it; `pid`: its process):
    /// its row in Activity shows it, its process and its numbers from now on. None: its terminal ended.
    pub fn terminal_meter(&self, id: &str, agent: Option<(&str, u32)>) {
        let inner = self.inner();
        let Some(r) = inner.residents.get(id) else { return };
        let Some(meter) = r.meter.clone() else { return };
        if let Some((name, _)) = agent {
            // Its door shows the terminal's numbers again: a headless load in between (a timer's, the page's) put its
            // session's on the door, which show nothing once it unloaded.
            if let Some(mesh) = &r.keep.mesh {
                show_meter(mesh, &meter);
            }
            meter.observe_harness(&serde_json::json!({"type": "ready", "model": name}), marv_session::time::now_ms());
        }
        meter.set_agent_process(agent.map(|(name, pid)| (name.to_string(), pid)));
    }

    /// Chat `id`'s terminal agent reported what its turn cost (USD: Claude Code's status line): on its Activity row.
    pub fn terminal_cost(&self, id: &str, cost: f64) {
        if let Some(meter) = self.inner().residents.get(id).and_then(|r| r.meter.clone()) {
            meter.observe_harness(&serde_json::json!({"type": "usage", "cost": cost}), marv_session::time::now_ms());
        }
    }

    /// The harness chat `id` moves to its own terminal: its session ends (the agent keeps the conversation, which the
    /// terminal resumes) and nothing loads it again until [`Chats::terminal_closed`]. Refused while it works or waits.
    /// `lend_door`: the terminal's agent is Marv, which takes the chat's mesh name and runs the chat's switchboard
    /// itself (it approves its own wires and wakes on its own mail): the chat's door closes while it runs.
    pub fn to_terminal(&self, id: &str, folder: &Path, lend_door: bool) -> Result<(), String> {
        let mut inner = self.inner();
        // Its record is made here if the page hasn't attached yet (the terminal and the page's `attach` race on open):
        // the terminal needs its mesh door and approver either way. Any chat's agent runs in its terminal, Marv's too.
        let r = self.resident(&mut inner, id, folder);
        // A headless session in the middle of a turn (a timer's, from before the terminals) is let finish.
        let busy = r.loaded.is_some() && (!(r.ready && r.shared().idle()) || r.unattended.is_some());
        if busy {
            return Err("It's working in the background (a teammate's message, a timer): wait for its turn to end, or stop it in Activity, then open it again.".into());
        }
        // What waited to load it headless goes to its terminal instead: a timer back in its queue (mail stays in its
        // inbox), typed in once the agent is ready.
        if let Some(Pending::Timer(timer)) = r.pending.take() {
            r.queue.push(InterruptSource::Timer, Queued::Timer(timer));
        }
        r.unload();
        if lend_door {
            if let Some(mesh) = r.mesh.take() {
                self.registry.unregister_own(&mesh.name);
            }
            r.keep.mesh = None;
            inner.lent.insert(id.to_string());
        }
        inner.in_terminal.insert(id.to_string());
        if let Some(agent) = agent(id) {
            let actions = inner.sched.update(&agent, AgentState::Sleeping, self.now());
            self.apply(&mut inner, actions);
        }
        self.publish_states(&inner);
        Ok(())
    }

    /// A terminal Marv ended: the chat's door opens again under its name (off the lock: the name may take a moment to
    /// be free, `mesh_process::start` waits for it).
    fn take_door_back(&self, id: &str) {
        let parts = {
            let mut inner = self.inner();
            if !inner.lent.remove(id) {
                return;
            }
            inner.residents.get(id).map(|r| (r.folder.clone(), r.keep.approver.clone(), r.out.clone(), r.keep.session.clone()))
        };
        let Some((folder, approver, out, session)) = parts else { return };
        let mesh = self.host.start_mesh(id, &folder, &approver, &out);
        if let Some(mesh) = &mesh {
            self.join_mesh(id, mesh, &session, &out);
        }
        let mut inner = self.inner();
        if let Some(r) = inner.residents.get_mut(id) {
            r.keep.mesh = mesh.as_ref().map(|m| m.handle.clone());
            r.mesh = mesh;
        }
    }

    /// What a harness chat's terminal needs of its record: the parts that outlive its sessions (its mesh door, approver,
    /// "always" scopes), and where its events go (its page, a notice or a card).
    pub fn terminal_parts(&self, id: &str) -> Option<(marv_web::host::Resident, mpsc::UnboundedSender<Value>)> {
        self.inner().residents.get(id).map(|r| (r.keep.clone(), r.out.clone()))
    }

    /// A harness chat's terminal turn started (`Busy`), ended (`Idle`) or needs the person (`Waiting`): its state in
    /// Activity, the sidebar and `mesh_list`, as a loaded chat's would be (kept beside the scheduler, which holds the
    /// chat asleep and counts no slot for it). Ignored once the terminal closed.
    pub fn terminal_state(&self, id: &str, state: AgentState) {
        let notices = {
            let mut inner = self.inner();
            let was = inner.terminal_states.get(id).copied();
            if !inner.in_terminal.contains(id) || was == Some(state) {
                Vec::new()
            } else {
                Self::terminal_asking(&mut inner, id, state == AgentState::Waiting)
            }
        };
        for (event, shown) in notices {
            self.host.notify(id, &event, shown);
        }
        let mut inner = self.inner();
        if !inner.in_terminal.contains(id) {
            return;
        }
        let was = inner.terminal_states.insert(id.to_string(), state);
        // Activity's numbers for its row: a turn while it works, "waiting for you" while it asks.
        if was != Some(state)
            && let Some(meter) = inner.residents.get(id).and_then(|r| r.meter.clone())
        {
            let now = marv_session::time::now_ms();
            if was == Some(AgentState::Waiting) {
                meter.observe_harness(&serde_json::json!({"type": "approval_done"}), now);
            }
            match state {
                AgentState::Busy if was != Some(AgentState::Waiting) => {
                    meter.observe_harness(&serde_json::json!({"type": "turn_start"}), now)
                }
                AgentState::Waiting => meter.observe_harness(&serde_json::json!({"type": "approval"}), now),
                AgentState::Idle | AgentState::Sleeping if matches!(was, Some(AgentState::Busy | AgentState::Waiting)) => {
                    meter.observe_harness(&serde_json::json!({"type": "turn_end"}), now)
                }
                _ => {}
            }
        }
        if state == AgentState::Busy
            && let Some(drawn) = inner.starting.get_mut(id)
        {
            *drawn = true;
        }
        if state == AgentState::Busy
            && let Some(started) = inner.mail_turn.get_mut(id)
        {
            *started = true;
        }
        if state == AgentState::Busy
            && let Some(started) = inner.timer_turn.get_mut(id)
        {
            *started = true;
        }
        // A timer's turn over: no longer unattended (its deadline and grant are done with it).
        if state == AgentState::Idle && inner.timer_turn.get(id) == Some(&true) {
            inner.timer_turn.remove(id);
            if let Some(r) = inner.residents.get_mut(id) {
                (r.unattended, r.woke) = (None, None);
            }
        }
        // Its turn on a teammate's mail is over: the report goes back, as a headless chat's does at its turn's end.
        let report = (state == AgentState::Idle && inner.mail_turn.get(id) == Some(&true))
            .then(|| {
                inner.mail_turn.remove(id);
                let r = inner.residents.get_mut(id)?;
                Some((r.inbox.take()?, r.keep.mesh.clone()?, r.out.clone()))
            })
            .flatten();
        if let Some((inbox, mesh, out)) = report {
            self.host.spawn(Box::pin(async move {
                let reply = inbox.reply.unwrap_or_else(|| TERMINAL_REPLY.to_string());
                if let Some(line) =
                    marv_session::task_report::harness_report(&mesh, &inbox.turn, "end", inbox.calls, Some(&reply)).await
                {
                    let _ = out.send(serde_json::json!({"type": "notice", "text": marv_core::printable(&line)}));
                }
            }));
        }
        // Idle and ready: what the person typed for it while it started goes in first (after this lock: a question on
        // its screen keeps it waiting).
        let typed = (state == AgentState::Idle && !inner.starting.get(id).is_some_and(|drawn| !drawn))
            .then(|| {
                inner.starting.remove(id);
                inner.to_type.remove(id)
            })
            .flatten();
        // Idle again: mail that waited for its turn to end is looked at now.
        if state == AgentState::Idle
            && let Some(r) = inner.residents.get(id)
        {
            r.kick.notify_one();
        }
        self.publish_states(&inner);
        drop(inner);
        if let Some(texts) = typed {
            if self.waits_on_person(id) {
                self.inner().to_type.insert(id.to_string(), texts);
            } else {
                for text in texts {
                    self.host.type_into(id, &text);
                }
            }
        }
    }

    /// Whether chat `id`'s terminal waits on a question only the person can answer: then nothing is typed in (it would
    /// answer it), and they're told once in the chat.
    fn waits_on_person(&self, id: &str) -> bool {
        let Some(question) = self.host.terminal_question(id) else {
            let notices = {
                let mut inner = self.inner();
                if inner.told_question.remove(id) { Self::terminal_asking(&mut inner, id, false) } else { Vec::new() }
            };
            for (event, shown) in notices {
                self.host.notify(id, &event, shown);
            }
            return false;
        };
        let notices = {
            let mut inner = self.inner();
            if inner.told_question.insert(id.to_string()) {
                if let Some(r) = inner.residents.get(id) {
                    let _ = r.out.send(serde_json::json!({"type": "notice", "text": format!("Waiting for you: {question}")}));
                }
                // A team stalls on it (Claude Code's folder trust before its first task): the person is told.
                Self::terminal_asking(&mut inner, id, true)
            } else {
                Vec::new()
            }
        };
        for (event, shown) in notices {
            self.host.notify(id, &event, shown);
        }
        true
    }

    /// The person's message for chat `id`'s terminal agent (a team's first task, "Tell the team"): typed in now if it's
    /// running and ready, else once it is (`starting`: its terminal was just started for it).
    pub fn type_when_ready(&self, id: &str, text: String, starting: bool) {
        let ready = {
            let mut inner = self.inner();
            if starting {
                inner.starting.entry(id.to_string()).or_insert(false);
            }
            let not_ready = inner.starting.get(id).is_some_and(|drawn| !drawn);
            let idle = matches!(inner.terminal_states.get(id), Some(AgentState::Idle));
            if not_ready || !idle {
                inner.to_type.entry(id.to_string()).or_default().push(text.clone());
                false
            } else {
                true
            }
        };
        if ready {
            if self.waits_on_person(id) {
                self.inner().to_type.entry(id.to_string()).or_default().push(text);
            } else {
                self.host.type_into(id, &text);
            }
        }
    }

    /// A terminal agent's reply at its turn's end, when Marv OS can read it (Claude Code's, from its transcript): the
    /// report on a teammate's mail quotes it.
    pub fn terminal_reply(&self, id: &str, text: String) {
        if let Some(inbox) = self.inner().residents.get_mut(id).and_then(|r| r.inbox.as_mut()) {
            inbox.reply = Some(text);
        }
    }

    /// The harness chat's terminal closed: the chat loads again at once if a page shows it (it continues the same
    /// conversation), else at the next look or wake.
    ///
    /// Callable from any thread: a terminal's reader is a plain std thread (no Tokio runtime), and giving the door back
    /// starts a switchboard and its watchers with `tokio::spawn`, which panicked there ("there is no reactor running",
    /// every `/exit`, restart or agent switch, seen 2026-11-17) and left the chat without its Activity row and door.
    pub fn terminal_closed(&self, id: &str) {
        let handle = tauri::async_runtime::handle();
        let _runtime = tokio::runtime::Handle::try_current().is_err().then(|| handle.inner().enter());
        self.take_door_back(id);
        let mut inner = self.inner();
        inner.in_terminal.remove(id);
        inner.terminal_states.remove(id);
        inner.briefed.remove(id);
        inner.starting.remove(id);
        inner.mail_turn.remove(id);
        inner.to_type.remove(id);
        inner.timer_turn.remove(id);
        inner.told_question.remove(id);
        let shown = inner.residents.get(id).filter(|r| r.loaded.is_none() && r.shared().shown()).map(|r| r.key.clone());
        if let Some(key) = shown
            && let Err(why) = self.load(&mut inner, id, key)
            && let Some(r) = inner.residents.get(id)
        {
            let _ = r.out.send(serde_json::json!({"type": "notice", "text": why}));
        }
        self.publish_states(&inner);
    }

    /// The harness plan, phase 2, at each kernel tick: a harness chat's timer turn past its deadline, or whose grant
    /// ended, is stopped and the chat paused, as Marv's are; a harness chat idle with no page on it for
    /// [`HARNESS_IDLE_MS`] sleeps (its process ends; the harness keeps its conversation, resumed on the next load).
    fn tick_harness(&self) {
        let deadline = Duration::from_millis(self.deadline_ms.load(std::sync::atomic::Ordering::SeqCst));
        let idle_for = Duration::from_millis(self.harness_idle_ms.load(std::sync::atomic::Ordering::SeqCst));
        let inner = self.inner();
        let running: Vec<(String, PathBuf, bool)> = inner
            .residents
            .iter()
            .filter(|(id, r)| {
                (r.harness || inner.in_terminal.contains(*id))
                    && r.unattended == Some(true)
                    && !r.attended.load(std::sync::atomic::Ordering::SeqCst)
            })
            .map(|(id, r)| (id.clone(), r.folder.clone(), r.woke.is_some_and(|w| w.elapsed() >= deadline)))
            .collect();
        drop(inner);
        // Off the lock: the app's grant reads its files.
        let stop: Vec<(String, InterruptKind)> = running
            .into_iter()
            .filter_map(|(id, folder, late)| match (late, self.harness_grant(&id, &folder)) {
                (true, _) => Some((id, InterruptKind::Deadline)),
                (false, None) => Some((id, InterruptKind::GrantRevoked)),
                _ => None,
            })
            .collect();
        let mut inner = self.inner();
        for (id, kind) in stop {
            let in_terminal = inner.in_terminal.contains(&id);
            let Some(r) = inner.residents.get_mut(&id) else { continue };
            if in_terminal {
                // A turn typed into its terminal (a timer's): stopped there, as the person would.
                self.host.interrupt_terminal(&id);
            } else if let Some(loaded) = &r.loaded {
                let _ = loaded.to_chat.send(ClientMessage::Interrupt);
            }
            r.keep.approver.cancel_all();
            (r.unattended, r.woke) = (None, None);
            if kind == InterruptKind::Deadline {
                r.set_paused(true);
                let _ = r.out.send(serde_json::json!({"type": "notice", "text": DEADLINE_TEXT}));
                let _ = r.out.send(serde_json::json!({"type": "paused", "reason": "deadline"}));
            }
            let interrupt = Interrupt::new(InterruptSource::Kernel, kind);
            self.log_interrupt(&id, interrupt, handling(interrupt, TurnNow::Unattended));
        }
        let sleepy: Vec<String> = inner
            .residents
            .iter()
            .filter(|(_, r)| r.harness && r.ready && r.unattended.is_none() && r.pending.is_none() && r.queue.is_empty())
            .filter(|(_, r)| {
                let Some(loaded) = &r.loaded else { return false };
                let shared = r.shared();
                let since = shared.last().map_or(loaded.seen, |last| last.max(loaded.seen));
                !shared.shown() && shared.idle() && since.elapsed() >= idle_for
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in sleepy {
            if let Some(r) = inner.residents.get_mut(&id) {
                r.unload();
            }
            if let Some(agent) = agent(&id) {
                let actions = inner.sched.update(&agent, AgentState::Sleeping, self.now());
                self.apply(&mut inner, actions);
            }
        }
        self.publish_states(&inner);
    }

    /// After a turn the watcher ran: the scheduler told (a wake that found nothing to read didn't tell it), and one
    /// more look at once, for what queued meanwhile.
    fn again(&self, id: &str) {
        let mut inner = self.inner();
        self.sync(&mut inner, id);
        if let Some(r) = inner.residents.get(id) {
            r.kick.notify_one();
        }
    }

    /// Runs an unattended turn (a wake, a timer) under its deadline (runtime-kernel §15.4): past it, the kernel stops
    /// the turn (a preempt), pauses the chat and tells the person ("paused: ran 30 min").
    async fn unattended(
        &self,
        id: &str,
        session: &marv_session::MarvSession,
        keep: &marv_web::host::Resident,
        out: &mpsc::UnboundedSender<Value>,
        wake: Pending,
    ) {
        let wake = match wake {
            Pending::Timer(timer) => match self.start_timer(id, session.live_grant(), keep, timer).await {
                Some(timer) => Pending::Timer(timer),
                None => return,
            },
            mail => mail,
        };
        let granted = session.grant_live();
        self.with_resident(id, |r| r.unattended = Some(granted));
        // Only a turn under the chat's grant has a deadline (an ungranted one asks for everything; pausing it would
        // only silence a rule's chat with nothing to resume it).
        let deadline = marv_kernel::turn_deadline(granted)
            .map(|_| Duration::from_millis(self.deadline_ms.load(std::sync::atomic::Ordering::SeqCst)));
        let run = async {
            match &wake {
                Pending::Mail(by) => {
                    let Some(mesh) = keep.mesh.as_ref() else { return };
                    let hop = marv_session::mesh::wake_hop(mesh, by);
                    run_wake_then(session, by, out, self.host.mesh_wake(), || mesh.log_wake(&by.sender, hop)).await;
                }
                Pending::Timer(timer) => {
                    run_timer(session, timer, out).await;
                }
            }
        };
        let attended = self.with_resident(id, |r| {
            r.attended.store(false, std::sync::atomic::Ordering::SeqCst);
            r.attended.clone()
        });
        let attended = attended.unwrap_or_default();
        let outcome = match deadline {
            Some(deadline) => {
                let grace = marv_web::host::DEADLINE_GRACE;
                marv_web::host::with_deadline(session, &keep.approver, deadline, grace, &attended, run).await
            }
            None => {
                run.await;
                marv_web::host::Deadline::Ended
            }
        };
        if outcome == marv_web::host::Deadline::Stuck {
            // A turn that won't end after its stop: the session is let go (it closes, saving) so the chat is free.
            self.with_resident(id, |r| r.unload());
        }
        if outcome != marv_web::host::Deadline::Ended {
            self.with_resident(id, |r| r.set_paused(true));
            let _ = out.send(serde_json::json!({"type": "notice", "text": DEADLINE_TEXT}));
            let _ = out.send(serde_json::json!({"type": "paused", "reason": "deadline"}));
            let interrupt = Interrupt::new(InterruptSource::Kernel, InterruptKind::Deadline);
            self.log_interrupt(id, interrupt, handling(interrupt, TurnNow::Unattended));
        }
        self.with_resident(id, |r| r.unattended = None);
    }

    /// The person's signal to chat `id` (runtime-kernel §15.3, the Mesh panel): stop its turn, pause it (no next
    /// step; no wakes or timers until resumed), resume it, or close it (it leaves the mesh until opened again). Only
    /// the person signals: a peer can send only mail, and nothing on the mesh reaches this.
    pub fn signal(&self, id: &str, kind: SignalKind) -> Result<(), String> {
        let interrupt = kind.interrupt();
        let to = agent(id).map(AgentKey::Chat);
        let action = {
            let mut inner = self.inner();
            let resident = inner.residents.get_mut(id).ok_or("That chat isn't running.")?;
            let now = turn_now(resident);
            let action = handling(interrupt, now);
            let session = resident.keep.session();
            // A harness chat has no Marv session to stop between steps: its runner stops the turn (H1).
            let harness_turn = resident.harness.then(|| resident.loaded.as_ref().map(|l| l.to_chat.clone())).flatten();
            let text = match kind {
                SignalKind::Stop => {
                    if let Some(session) = &session {
                        session.interrupt();
                    }
                    if let Some(to_chat) = &harness_turn {
                        let _ = to_chat.send(ClientMessage::Interrupt);
                    }
                    resident.keep.approver.cancel_all();
                    None
                }
                SignalKind::Pause => {
                    resident.set_paused(true);
                    if let Some(session) = &session {
                        session.halt_at_step();
                    }
                    match &harness_turn {
                        Some(to_chat) => {
                            let busy = !resident.shared().idle();
                            if busy {
                                let _ = to_chat.send(ClientMessage::Interrupt);
                                resident.keep.approver.cancel_all();
                            }
                            Some(if busy { HARNESS_PAUSED_TEXT } else { PAUSED_TEXT })
                        }
                        None => Some(PAUSED_TEXT),
                    }
                }
                SignalKind::Resume => {
                    resident.set_paused(false);
                    if let Some(session) = &session {
                        session.resume_steps();
                    }
                    resident.kick.notify_one();
                    Some(RESUMED_TEXT)
                }
                SignalKind::Close => None,
            };
            if let Some(text) = text {
                let _ = resident.out.send(serde_json::json!({"type": "notice", "text": text}));
            }
            action
        };
        if kind == SignalKind::Close {
            self.stop(id);
        }
        if let Some(to) = to {
            self.log(KernelEvent::Signal { to, kind, by: SignalBy::Person });
        }
        self.log_interrupt(id, interrupt, action);
        Ok(())
    }

    /// "Resume the team" (§15.3): every one of `ids` that's paused runs again. How many were paused.
    pub fn resume_team(&self, ids: &[String]) -> usize {
        let paused: Vec<&String> = ids.iter().filter(|id| self.with_resident(id, |r| r.paused) == Some(true)).collect();
        paused.iter().filter(|id| self.signal(id, SignalKind::Resume).is_ok()).count()
    }

    /// "Stop the team" (§15.3): every one of `ids` (the chats a rule covers) is paused and its turn stopped. They stay
    /// on the mesh, paused, until the person resumes them. How many were running here.
    pub fn stop_team(&self, ids: &[String]) -> usize {
        let mut stopped = 0;
        for id in ids {
            if self.signal(id, SignalKind::Pause).is_ok() {
                let _ = self.signal(id, SignalKind::Stop);
                stopped += 1;
            }
        }
        stopped
    }

    /// The kernel's look (runtime-kernel §15, the app's timer every few seconds): an unattended turn whose grant was
    /// revoked or expired mid-turn is stopped (a preempt), and the timers due at `now_ms` fire.
    pub async fn tick(&self, now_ms: u64) {
        self.tick_harness();
        let running: Vec<(String, marv_session::MarvSession, Arc<WebApprover>)> = self
            .inner()
            .residents
            .iter()
            .filter(|(_, r)| r.unattended == Some(true))
            .filter_map(|(id, r)| Some((id.clone(), r.keep.session()?, r.keep.approver.clone())))
            .collect();
        for (id, session, approver) in running {
            let kind = if !session.has_grant() {
                InterruptKind::GrantRevoked
            } else if session.grant_expired() {
                InterruptKind::GrantExpired
            } else {
                continue;
            };
            if session.busy() {
                session.interrupt();
                approver.cancel_all();
                let interrupt = Interrupt::new(InterruptSource::Kernel, kind);
                self.log_interrupt(&id, interrupt, handling(interrupt, TurnNow::Unattended));
            }
        }
        let Some(store) = self.timers.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return };
        let Ok(snapshot) = store.load() else { return };
        for timer in snapshot.due(now_ms) {
            self.fire(&store, timer.clone(), now_ms).await;
        }
    }

    /// One due timer: void when it can't run (its chat gone, its grant ended or at its cap: told once, a one-shot
    /// removed, a recurring one moved to its next time), else into its chat's queue (after the person, after mail),
    /// marked fired (late when it was missed) and logged.
    async fn fire(&self, store: &TimerStore, timer: marv_kernel::Timer, now_ms: u64) {
        let id = timer.chat.as_str().to_string();
        if self.inner().stopped.contains(&id) {
            // The person stopped it this run: the timer waits until they open it again, then fires (late).
            return;
        }
        // A chat that runs in its terminal (the desktop's): its terminal's spec (an agent Marv can't run headless has
        // no other), and the timer is typed in there (`timer_to_terminal`).
        let spec = if self.host.terminal_only(&id) { self.host.terminal_spec(&id) } else { self.host.spec(&id) };
        let (quiet, _drop) = mpsc::unbounded_channel();
        let grant = self.host.grant(&id, &quiet);
        let live = grant.as_ref().and_then(|g| g.live());
        let void = match (&spec, timer.by, live) {
            (Err(_), ..) => Some(TimerVoidReason::NoChat),
            (_, _, None) if grant.as_ref().is_some_and(|g| g.granted() && !g.expired()) => Some(TimerVoidReason::Spent),
            (_, _, None) => Some(TimerVoidReason::GrantEnded),
            (_, by, live) if !marv_kernel::may_fire(by, timer.grant, live) => Some(TimerVoidReason::GrantEnded),
            _ => None,
        };
        if let Some(reason) = void {
            self.void_timer(store, &timer.chat, timer.id, timer.repeat, reason, now_ms).await;
            return;
        }
        let Ok(spec) = spec else { return };
        let interrupt = Interrupt::new(InterruptSource::Timer, InterruptKind::Timer);
        let (action, no_mesh) = {
            let mut inner = self.inner();
            // Its door lent to its terminal's Marv: on the mesh all the same, the timer runs there.
            let lent = inner.lent.contains(&id);
            let resident = self.resident(&mut inner, &id, &spec.root);
            // Review P2-1/P2-2: while the chat is paused, or this timer already waits (queued, or admitted to load),
            // it stays due in the file: nothing more is queued, and a missed recurring fire collapses into one. The
            // file is changed only when its turn starts.
            let waiting = resident.queue.iter().any(|(_, q)| matches!(q, Queued::Timer(t) if t.id == timer.id))
                || matches!(&resident.pending, Some(Pending::Timer(t)) if t.id == timer.id);
            if resident.keep.mesh.is_none() && !resident.harness && !lent {
                (None, true)
            } else if resident.paused || waiting {
                (None, false)
            } else {
                let action = handling(interrupt, turn_now(resident));
                resident.queue.push(InterruptSource::Timer, Queued::Timer(marv_session::timers::TimerWake::of(&timer)));
                resident.kick.notify_one();
                (Some(action), false)
            }
        };
        if no_mesh {
            // Nothing here can run it (its watcher is the mesh's); a record only it brought up goes again.
            {
                let mut inner = self.inner();
                if inner.residents.get(&id).is_some_and(|r| !r.opened && r.loaded.is_none() && r.queue.is_empty()) {
                    self.drop_resident(&mut inner, &id);
                }
            }
            self.void_timer(store, &timer.chat, timer.id, timer.repeat, TimerVoidReason::NotOnMesh, now_ms).await;
            return;
        }
        if let Some(action) = action {
            self.log_interrupt(&id, interrupt, action);
        }
    }

    /// A timer that can't run: a one-shot removed, a recurring one moved to its next time; logged and told once.
    async fn void_timer(
        &self,
        store: &TimerStore,
        chat: &AgentId,
        timer: marv_kernel::TimerId,
        repeat: marv_kernel::Repeat,
        reason: TimerVoidReason,
        now_ms: u64,
    ) {
        let gone = match repeat {
            marv_kernel::Repeat::Once => store.remove(timer).await.map(|(gone, _)| gone).unwrap_or(false),
            _ => store.fired(timer, now_ms).await.ok().flatten().is_some(),
        };
        if gone {
            self.tell_void(chat, timer, reason);
        }
    }

    /// A void timer, logged and told in its chat (Marv's fixed words).
    fn tell_void(&self, chat: &AgentId, timer: marv_kernel::TimerId, reason: TimerVoidReason) {
        let id = chat.as_str();
        self.log(KernelEvent::TimerVoid { id: timer, chat: chat.clone(), reason });
        self.log_interrupt(
            id,
            Interrupt::new(InterruptSource::Timer, InterruptKind::Timer),
            marv_kernel::InterruptAction::Dropped,
        );
        let text = match reason {
            TimerVoidReason::Spent => "A timer for this chat didn't run: its grant is at today's spend cap.",
            TimerVoidReason::NotOnMesh => {
                "A timer for this chat didn't run: the chat isn't on the mesh here, so nothing can wake it."
            }
            _ => "A timer for this chat didn't run: it needs the chat's permission to work while you're away, which has ended.",
        };
        if let Some(r) = self.inner().residents.get(id) {
            let _ = r.out.send(serde_json::json!({"type": "notice", "text": text}));
            let _ = r.out.send(serde_json::json!({"type": "timer_void"}));
        }
    }

    /// Just before a timer's turn (review P2-2, P2-3, P2-4): the chat's grant must hold now (for its own timer, the
    /// very grant it was set under), the rule wakes' overall limit must have room, and the file must still have it due;
    /// only then is it marked fired (a one-shot goes, a recurring one moves on) and logged. None: it doesn't run now
    /// (void, told; or it waits, due, for the limit).
    async fn start_timer(
        &self,
        id: &str,
        live: Option<marv_kernel::GrantId>,
        keep: &marv_web::host::Resident,
        timer: marv_session::timers::TimerWake,
    ) -> Option<marv_session::timers::TimerWake> {
        let now = wall_ms();
        let store = self.timers.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let chat = agent(id)?;
        let ended = !marv_kernel::may_fire(timer.by, timer.grant, live);
        if ended {
            let repeat = store
                .as_ref()
                .and_then(|s| s.load().ok())
                .and_then(|s| s.timers.into_iter().find(|t| t.id == timer.id))
                .map_or(marv_kernel::Repeat::Once, |t| t.repeat);
            // Whether or not the file still had it (a look may have voided it already), it doesn't run: told once here.
            let in_file = store.as_ref().is_some_and(|s| s.load().is_ok_and(|s| s.timers.iter().any(|t| t.id == timer.id)));
            match (&store, in_file) {
                (Some(store), true) => self.void_timer(store, &chat, timer.id, repeat, TimerVoidReason::GrantEnded, now).await,
                _ => self.tell_void(&chat, timer.id, TimerVoidReason::GrantEnded),
            }
            return None;
        }
        if !keep.wake_limit.lock().unwrap_or_else(|e| e.into_inner()).take_timer_wake(now) {
            // Rule wakes are paused until the person types: it stays due in the file and fires then.
            return None;
        }
        if let Some(store) = &store
            && !matches!(store.fired(timer.id, now).await, Ok(Some(_)))
        {
            return None;
        }
        let timer = timer.starting(now);
        self.log(KernelEvent::TimerFired { id: timer.id, chat, late: timer.late });
        Some(timer)
    }

    /// The person revoked chat `id`'s grant: an unattended turn running under it stops now (§15.2).
    pub fn grant_revoked(&self, id: &str) {
        let running = self
            .inner()
            .residents
            .get(id)
            .filter(|r| r.unattended == Some(true))
            .and_then(|r| Some((r.keep.session()?, r.keep.approver.clone())));
        if let Some((session, approver)) = running.filter(|(s, _)| s.busy()) {
            session.interrupt();
            approver.cancel_all();
            let interrupt = Interrupt::new(InterruptSource::Kernel, InterruptKind::GrantRevoked);
            self.log_interrupt(id, interrupt, handling(interrupt, TurnNow::Unattended));
        }
    }

    pub(crate) fn with_resident<T>(&self, id: &str, f: impl FnOnce(&mut Resident) -> T) -> Option<T> {
        self.inner().residents.get_mut(id).map(f)
    }
}

/// A record's events, into its [`Shared`], until the record goes.
async fn pump(
    chats: Weak<Chats>,
    id: String,
    mut events: mpsc::UnboundedReceiver<Value>,
    stop: tokio_util::sync::CancellationToken,
) {
    loop {
        let event = tokio::select! {
            event = events.recv() => event,
            _ = stop.cancelled() => None,
        };
        let (Some(event), Some(chats)) = (event, chats.upgrade()) else { return };
        chats.event(&id, event);
    }
}

/// A record's watcher: looks at its inbox when mail comes, when a load is ready, and now and then.
async fn watch(
    chats: Weak<Chats>,
    id: String,
    mesh: marv_session::mesh::MeshHandle,
    kick: Arc<tokio::sync::Notify>,
    stop: tokio_util::sync::CancellationToken,
) {
    loop {
        tokio::select! {
            _ = mesh.mail() => {}
            _ = kick.notified() => {}
            _ = tokio::time::sleep(LOOK_AGAIN) => {}
            _ = stop.cancelled() => return,
        }
        let Some(chats) = chats.upgrade() else { return };
        chats.look(&id).await;
    }
}

/// A harness chat's watcher: looks when mail comes (phase 3: it's on the mesh), a load is ready or a turn ends, when
/// kicked, and now and then.
async fn watch_harness(
    chats: Weak<Chats>,
    id: String,
    mesh: Option<marv_session::mesh::MeshHandle>,
    kick: Arc<tokio::sync::Notify>,
    stop: tokio_util::sync::CancellationToken,
) {
    loop {
        tokio::select! {
            _ = async { match &mesh { Some(mesh) => mesh.mail().await, None => std::future::pending().await } } => {}
            _ = kick.notified() => {}
            _ = tokio::time::sleep(LOOK_AGAIN) => {}
            _ = stop.cancelled() => return,
        }
        let Some(chats) = chats.upgrade() else { return };
        chats.look_harness(&id).await;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use marv_guards::{Scripted, TempDir, say};
    use marv_kernel::TimerBy;
    use marv_mesh::approval::WireRequest;
    use marv_mesh::wire::{Approver, WireMode};
    use marv_mesh::{PlugId, ReceptacleName};

    pub const A: &str = "554963ee-fd8e-4bec-a7dc-49051f090ac0";
    pub const B: &str = "6b1d0c2a-1111-4bec-a7dc-49051f090ac1";
    pub const C: &str = "7c2e1d3b-2222-4bec-a7dc-49051f090ac2";

    /// The chats' app, for tests: folders and Marv's files in a temp folder, a scripted model, its own mesh folder.
    pub struct TestHost {
        pub dir: TempDir,
        pub model: Arc<Scripted>,
        pub sessions: Mutex<HashMap<String, String>>,
        pub yolo: bool,
        /// Saved chats' cards, as the app reads them from their saved sessions.
        pub cards: Mutex<HashMap<String, marv_kernel::Card>>,
        /// The chats the person marked private (runtime-kernel §9.2).
        pub private: Mutex<HashSet<String>>,
        /// Every chat's grant (runtime-kernel §15.4's timers), when a test gives one.
        pub grant: Mutex<Option<Arc<dyn marv_session::grants::ChatGrant>>>,
        /// The person's `mesh.wake` (mk-60m.8): every wire unless a test says otherwise.
        pub wake: Mutex<marv_session::config::MeshWake>,
        /// Chats Claude Code runs (the harness plan), each with its stand-in `claude`.
        pub claude: Mutex<HashMap<String, PathBuf>>,
        /// The person's meters and budgets (runtime-kernel §16), when a test gives them.
        pub meters: Mutex<Option<Arc<marv_session::allocator::MeterBook>>>,
        /// What was typed into chats' terminals (team mail), by chat.
        pub typed: Mutex<Vec<(String, String)>>,
        /// The notifications the person would get (chat, event type).
        pub notified: Mutex<Vec<(String, String)>>,
        /// Every chat is its agent's terminal (the desktop's way), when a test says so; the terminals it started.
        pub terminals: Mutex<Option<Vec<String>>>,
        /// The terminal turns stopped (an unattended one past its deadline, or whose grant ended).
        pub interrupted: Mutex<Vec<String>>,
    }

    impl TestHost {
        pub fn new(tag: &str, replies: Vec<Vec<marv_core::ProviderEvent>>, yolo: bool) -> Arc<Self> {
            let dir = TempDir::new(tag);
            for id in [A, B, C] {
                std::fs::create_dir_all(dir.path().join(&id[..4])).unwrap();
            }
            Arc::new(Self {
                dir,
                model: Scripted::new(replies),
                sessions: Mutex::default(),
                yolo,
                cards: Mutex::default(),
                private: Mutex::default(),
                grant: Mutex::default(),
                wake: Mutex::new(marv_session::config::MeshWake::On),
                claude: Mutex::default(),
                meters: Mutex::default(),
                typed: Mutex::default(),
                notified: Mutex::default(),
                terminals: Mutex::default(),
                interrupted: Mutex::default(),
            })
        }

        pub fn folder(&self, id: &str) -> PathBuf {
            self.dir.path().join(&id[..4])
        }

        pub fn env(&self) -> marv_mcp::Env {
            // 0.8.0: the runtime folder (`$XDG_RUNTIME_DIR/marv`) is made only inside an existing XDG_RUNTIME_DIR.
            std::fs::create_dir_all(self.dir.path().join("r")).unwrap();
            [("XDG_RUNTIME_DIR".to_string(), self.dir.path().join("r").display().to_string())].into_iter().collect()
        }
    }

    impl Host for TestHost {
        fn notify(&self, id: &str, event: &Value, _shown: bool) {
            self.notified.lock().unwrap().push((id.to_string(), event["type"].as_str().unwrap_or("").to_string()));
        }

        fn type_into(&self, id: &str, text: &str) -> bool {
            self.typed.lock().unwrap().push((id.to_string(), text.to_string()));
            true
        }

        fn terminal_only(&self, _id: &str) -> bool {
            self.terminals.lock().unwrap().is_some()
        }

        fn interrupt_terminal(&self, id: &str) {
            self.interrupted.lock().unwrap().push(id.to_string());
        }

        fn start_terminal(&self, id: &str) -> Result<(), String> {
            self.terminals.lock().unwrap().get_or_insert_with(Vec::new).push(id.to_string());
            Ok(())
        }

        fn spec(&self, id: &str) -> Result<ChatSpec, String> {
            let root = self.folder(id);
            if !root.is_dir() {
                return Err(format!("This chat's folder: {} isn't there.", root.display()));
            }
            let harness = self.harness(id);
            Ok(ChatSpec {
                root,
                config_dir: self.dir.path().join("c"),
                api_key: String::new(),
                zai_api_key: None,
                kimi_api_key: None,
                chatgpt_auth: None,
                model: "test/model".into(),
                max_steps: 10,
                yolo: self.yolo,
                resume: self.sessions.lock().unwrap().get(id).cloned().map(marv_session::ResumeTarget::Id),
                processes: false,
                sources: Vec::new(),
                mesh: !harness,
                mcp_off: Vec::new(),
                devices_off: Vec::new(),
                mesh_name: None,
                chat_id: Some(id.to_string()),
                rules: false,
                manage_mcp: false,
                host_commands: false,
                kernel_events: None,
                timers: None,
                delegates: None,
                role: None,
                team: None,
                team_writes: Vec::new(),
                team_peers: Vec::new(),
                choices: false,
                writes: None,
                harness: if harness { marv_web::host::Harness::ClaudeCode } else { Default::default() },
                harness_sandbox: false,
                agent_model: None,
                resident: None,
                provider: Some(marv_provider::ProviderOption::Custom(self.model.clone())),
                grant: None,
                trajectories: false,
                meters: self.meters.lock().unwrap().clone(),
            })
        }

        fn start_mesh(
            &self,
            id: &str,
            folder: &Path,
            approver: &Arc<WebApprover>,
            out: &mpsc::UnboundedSender<Value>,
        ) -> Option<marv_session::mesh_process::Mesh> {
            let name = marv_kernel::mesh_name_for(folder, &marv_kernel::AgentId::new(id).unwrap());
            let mut mesh = marv_session::mesh_process::start(&self.env(), folder, &[], Some(&name)).unwrap()?;
            mesh.chat = Some(id.to_string());
            marv_web::host::open_mesh_door(&mut mesh, approver, out);
            Some(mesh)
        }

        fn saved_card(&self, id: &str) -> CardTask {
            let card = self.cards.lock().unwrap().get(id).cloned();
            Box::pin(async move { card })
        }

        fn private(&self, id: &str) -> bool {
            self.private.lock().unwrap().contains(id)
        }

        fn grant(&self, _id: &str, _out: &mpsc::UnboundedSender<Value>) -> Option<Arc<dyn marv_session::grants::ChatGrant>> {
            self.grant.lock().unwrap().clone()
        }

        fn mesh_wake(&self) -> marv_session::config::MeshWake {
            *self.wake.lock().unwrap()
        }

        fn session_saved(&self, id: &str, session: &str) {
            self.sessions.lock().unwrap().insert(id.to_string(), session.to_string());
        }

        fn spawn(&self, task: Task) {
            tokio::spawn(task);
        }

        fn harness(&self, id: &str) -> bool {
            self.claude.lock().unwrap().contains_key(id)
        }

        fn run(
            &self,
            spec: ChatSpec,
            incoming: mpsc::UnboundedReceiver<ClientMessage>,
            out: mpsc::UnboundedSender<Value>,
        ) -> Task {
            let id = spec.chat_id.clone().unwrap_or_default();
            match self.claude.lock().unwrap().get(&id).cloned() {
                Some(bin) => Box::pin(marv_web::claude_code::run_chat_with(bin, spec, incoming, out)),
                None => Box::pin(run_chat(spec, incoming, out)),
            }
        }
    }

    /// A stand-in `claude` for chat `id` (the harness plan): each start's arguments and every message it reads go to
    /// its log; each message gets "done" and the turn's end. Its log's path.
    #[cfg(unix)] // a bash script stands in for the agent
    pub fn fake_claude(host: &TestHost, id: &str) -> PathBuf {
        let log = host.dir.path().join(format!("claude-{}.log", &id[..4]));
        let script = format!(
            r#"#!/usr/bin/env bash
echo "START $*" >> '{log}'
while read -r line; do
  case "$line" in
    *'"type":"user"'*)
      printf '%s\n' "$line" >> '{log}'
      echo '{{"type":"assistant","parent_tool_use_id":null,"message":{{"content":[{{"type":"text","text":"done"}}]}}}}'
      echo '{{"type":"result","subtype":"success","total_cost_usd":0.01,"usage":{{}}}}' ;;
  esac
done
"#,
            log = log.display()
        );
        let bin = host.dir.path().join(format!("claude-{}", &id[..4]));
        std::fs::write(&bin, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        host.claude.lock().unwrap().insert(id.to_string(), bin);
        log
    }

    /// A stand-in `claude` whose turns run until they're interrupted (H1): each message is logged and gets no reply;
    /// an interrupt request is logged as `INTERRUPT` and ends the turn as Claude Code does. Its log's path.
    #[cfg(unix)] // a bash script stands in for the agent
    pub fn slow_claude(host: &TestHost, id: &str) -> PathBuf {
        let log = host.dir.path().join(format!("slow-{}.log", &id[..4]));
        let script = format!(
            r#"#!/usr/bin/env bash
while read -r line; do
  case "$line" in
    *'"subtype":"interrupt"'*)
      echo INTERRUPT >> '{log}'
      echo '{{"type":"result","subtype":"error_during_execution","errors":["[ede_diagnostic] result_type=user"],"usage":{{}}}}' ;;
    *'"type":"user"'*)
      echo TURN >> '{log}' ;;
  esac
done
"#,
            log = log.display()
        );
        let bin = host.dir.path().join(format!("slow-{}", &id[..4]));
        std::fs::write(&bin, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        host.claude.lock().unwrap().insert(id.to_string(), bin);
        log
    }

    pub type Got = Arc<Mutex<Vec<Value>>>;

    /// A page that keeps what it's sent.
    pub fn page() -> (Sink, Got) {
        let got: Got = Arc::default();
        let g = got.clone();
        let sink: Sink = Box::new(move |e| {
            g.lock().unwrap().push(e);
            true
        });
        (sink, got)
    }

    pub fn count(got: &Got, kind: &str) -> usize {
        got.lock().unwrap().iter().filter(|e| e["type"] == kind).count()
    }

    /// Waits (at most 20 s) for `done`.
    pub async fn until(what: &str, mut done: impl FnMut() -> bool) {
        for _ in 0..1000 {
            if done() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("timed out waiting for {what}");
    }

    pub fn configure() -> ClientMessage {
        ClientMessage::Configure { model: None, max_steps: None, yolo: None, context_budget: None }
    }

    /// Opens as a page of this run would: with the chat's own epoch, so `after` counts.
    pub fn open(chats: &Chats, host: &TestHost, id: &str, sink: Sink, after: u64) -> Result<(), String> {
        let epoch = chats.inner().residents.get(id).map(|r| r.shared().epoch());
        chats.open(id, &host.folder(id), "|0".into(), configure(), sink, (after, epoch.as_deref()))
    }

    /// A worker plug on `mesh`, wired by the person to send to the main agent's inbox.
    pub async fn wired_worker(mesh: &marv_session::mesh::MeshHandle) -> PlugId {
        let worker = mesh.plug_next("worker").unwrap();
        let inbox = ReceptacleName::new("agent.parent.inbox").unwrap();
        let request = WireRequest { from: worker.id.clone(), to: inbox, mode: WireMode::Send, asked_by: worker.id.clone() };
        mesh.grant(request, Approver::Tui).await.unwrap();
        worker.id
    }

    pub async fn send_from(mesh: &marv_session::mesh::MeshHandle, worker: &PlugId, text: &str) {
        mesh.deliver(worker, "agent.parent.inbox", WireMode::Send, json!({ "text": text })).await.unwrap();
    }

    /// Everything that must survive an unload, as it is now: the session's (from its file once reloaded) and the
    /// record's.
    #[derive(Debug, PartialEq)]
    struct State {
        id: String,
        conversation: Vec<marv_core::ChatTurn>,
        first_message: String,
        tainted: Vec<marv_core::Provenance>,
        plan: Option<Vec<marv_tools::types::PlanItem>>,
        totals: marv_session::usage::Totals,
        wires: Vec<String>,
        unread: Vec<(PlugId, ReceptacleName)>,
        always: bool,
        wake_limit: String,
        hop: u32,
        hop_registered: bool,
        mesh_name: Option<String>,
    }

    fn state(keep: &marv_web::host::Resident, workers: &[&PlugId]) -> State {
        let session = keep.session().expect("loaded");
        let mesh = keep.mesh.as_ref().unwrap();
        let inbox = ReceptacleName::new("agent.parent.inbox").unwrap();
        let parent = marv_session::mesh::parent_id();
        State {
            id: session.id(),
            conversation: session.conversation(),
            first_message: session.first_message(),
            tainted: session.tainted(),
            plan: session.plan(),
            totals: session.usage().totals,
            wires: workers.iter().map(|w| format!("{:?}", mesh.authorize(w, &inbox, WireMode::Send))).collect(),
            unread: mesh.unread(&parent),
            always: keep.always.contains("bash:echo kept"),
            // Without the messages already told to wait (runtime-kernel live UX): one that came while asleep is told
            // too, which isn't state the reload must keep.
            wake_limit: format!("{:?}", keep.wake_limit.lock().unwrap()).split(", told_waiting").next().unwrap().to_string(),
            hop: keep.turn_hop.get(),
            hop_registered: mesh.turn_hop(&parent).is_some_and(|h| Arc::ptr_eq(&h, &keep.turn_hop)),
            mesh_name: keep.mesh_name.clone(),
        }
    }

    fn loaded(chats: &Chats) -> usize {
        chats.inner().residents.values().filter(|r| r.loaded.is_some()).count()
    }

    fn ms() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
    }

    /// The harness plan, phase 1: a Claude Code chat's new session exists only once its first turn ends, so it's
    /// saved from a `session` event then, as a Marv chat's is from `ready`; a reopen resumes it.
    #[tokio::test]
    async fn a_session_event_saves_the_chats_session_like_ready() {
        let host = TestHost::new("session-event", vec![], true);
        let chats = Chats::with_cap(host.clone(), 1);
        chats.event(A, json!({"type": "session", "sessionId": "claude-session-1"}));
        assert_eq!(host.sessions.lock().unwrap().get(A).map(String::as_str), Some("claude-session-1"));
        chats.event(B, json!({"type": "notice", "sessionId": "not-a-save"}));
        assert!(host.sessions.lock().unwrap().get(B).is_none(), "only ready and session save one");
    }

    /// mk-60m.11: the dock reaches a chat's processes through its loaded session; none for a chat that isn't loaded.
    #[tokio::test]
    async fn the_dock_reads_a_loaded_chats_session_only() {
        let host = TestHost::new("dock-session", vec![say("Hello.")], true);
        let chats = Chats::with_cap(host.clone(), 1);
        assert!(chats.session(A).is_none());
        let (sink, _got) = page();
        open(&chats, &host, A, sink, 0).unwrap();
        until("ready", || chats.with_resident(A, |r| r.ready).unwrap()).await;
        until("published", || chats.session(A).is_some()).await;
        let session = chats.session(A).unwrap();
        assert!(crate::dock::processes(&session.processes()).is_empty());
        assert!(session.process_logs("web", 10).is_err(), "no process door here: an error, never a panic");
        assert!(chats.session(B).is_none());
        chats.stop(A);
        assert!(chats.session(A).is_none());
    }

    /// Runtime-kernel 1 (§5.3 tests): a chat evicted and loaded again is the same chat, field by field: its
    /// conversation, first message, taint, plan and totals come back from its session file; its wires, unread inbox,
    /// "always" scopes, wake counts and turn hop never left its record. A wake limit hit before and while it sleeps is
    /// still hit after it's loaded. Then a message on another wire wakes it while it sleeps: the scheduler loads it
    /// (evicting the idle one, K = 1), and the woken turn reads everything that waited.
    #[tokio::test]
    async fn a_wake_after_unload_restores_identical_state() {
        let host = TestHost::new("state", vec![say("Hello."), say("Read m1."), say("Read the rest.")], true);
        let chats = Chats::with_cap(host.clone(), 1);
        let (sink, got) = page();
        open(&chats, &host, A, sink, 0).unwrap();
        until("ready", || chats.with_resident(A, |r| r.ready).unwrap()).await;
        chats.message(A, ClientMessage::Send { text: "hi".into() }).unwrap();
        until("the person's turn", || count(&got, "usage") == 1).await;
        let keep = chats.with_resident(A, |r| r.keep.clone()).unwrap();
        let mesh = keep.mesh.clone().unwrap();
        let (one, two) = (wired_worker(&mesh).await, wired_worker(&mesh).await);
        // A wake while it's loaded: the woken turn reads m1 and taints the session.
        send_from(&mesh, &one, "m1").await;
        until("the woken turn", || count(&got, "usage") == 2).await;
        // worker-1 has woken it 3 times this minute: its next message waits, told once.
        let by =
            marv_session::mesh::WakeBy { sender: one.clone(), receptacle: ReceptacleName::new("agent.parent.inbox").unwrap() };
        for _ in 0..2 {
            let candidate = marv_session::mesh::WakeCandidate {
                id: "taken".into(),
                by: by.clone(),
                hop: None,
                rule_issued: false,
                rounds_out: false,
                report: false,
                team: false,
            };
            let check = marv_session::mesh::decide_wake([candidate], &mut keep.wake_limit.lock().unwrap(), ms());
            assert!(check.wake.is_some());
        }
        send_from(&mesh, &one, "m2").await;
        until("the limit's notice", || {
            got.lock().unwrap().iter().any(|e| e["text"].as_str().is_some_and(|t| t.contains("times a minute already")))
        })
        .await;
        keep.always.add("bash:echo kept");
        // What's saved is what the session says (its first message is read from its last save).
        keep.session().unwrap().flush().await;
        let before = state(&keep, &[&one, &two]);
        assert_eq!(before.tainted, [marv_core::Provenance::Mesh]);
        assert_eq!(before.unread.len(), 1);

        // Evicted: the person opens B with A idle and unshown (K = 1).
        chats.close(A);
        let (sink, _b) = page();
        open(&chats, &host, B, sink, 0).unwrap();
        until("A unloaded", || keep.session().is_none()).await;
        until("B ready", || chats.with_resident(B, |r| r.ready).unwrap()).await;
        chats.close(B);
        // While it sleeps, worker-1's limit holds: no load for it.
        send_from(&mesh, &one, "m2b").await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(chats.with_resident(A, |r| r.loaded.is_none()).unwrap(), "a limited wire doesn't wake a sleeping chat");
        assert_eq!(mesh.unread(&marv_session::mesh::parent_id()).len(), 2);

        // Loaded again by the person: identical, but for the message that came while it slept.
        let seq = chats.with_resident(A, |r| r.shared().last_seq()).unwrap();
        let (sink, again) = page();
        open(&chats, &host, A, sink, seq).unwrap();
        until("A ready", || chats.with_resident(A, |r| r.ready).unwrap()).await;
        until("published", || keep.session().is_some()).await;
        let after = state(&keep, &[&one, &two]);
        let expected = State { unread: [before.unread.clone(), vec![(one.clone(), by.receptacle.clone())]].concat(), ..before };
        assert_eq!(after, expected);
        assert!(chats.with_resident(B, |r| r.loaded.is_none()).unwrap(), "B made room (K = 1)");
        assert!(again.lock().unwrap().iter().filter_map(|e| e["seq"].as_u64()).all(|n| n > seq), "numbered on");

        // Asleep again, and a message on worker-2's wire wakes it: loaded by the scheduler, B evicted for it.
        chats.close(A);
        let (sink, _b) = page();
        open(&chats, &host, B, sink, 0).unwrap();
        until("A unloaded", || keep.session().is_none()).await;
        chats.close(B);
        until("B idle", || chats.inner().sched.state(&agent(B).unwrap()) == AgentState::Idle).await;
        send_from(&mesh, &two, "m3").await;
        until("the wake's turn", || host.model.requests.lock().unwrap().len() == 3).await;
        until("its reply", || {
            chats.with_resident(A, |r| r.shared().idle()).unwrap() && keep.session().is_some_and(|s| !s.busy())
        })
        .await;
        let woken = format!("{:?}", host.model.requests.lock().unwrap()[2].0);
        for text in ["m2", "m2b", "m3"] {
            assert!(woken.contains(text), "{text} was read: {woken}");
        }
        assert!(mesh.unread(&marv_session::mesh::parent_id()).is_empty());
        assert!(chats.with_resident(B, |r| r.loaded.is_none()).unwrap() && loaded(&chats) == 1, "K held");
        assert_eq!(keep.session().unwrap().tainted(), [marv_core::Provenance::Mesh]);
    }

    /// mk-60m.8: the desktop's wake honours the person's `mesh.wake`, as the TUI's does. A message on a wire the person
    /// approved by hand (not a rule's): with wake off it never wakes the chat (it waits, and the page is told why);
    /// with the default (rules' wires only) it still doesn't; with wake on it does, and the woken turn reads it.
    #[tokio::test]
    async fn a_wake_honours_the_persons_mesh_wake() {
        use marv_session::config::MeshWake;
        let host = TestHost::new("wake-setting", vec![say("Hello."), say("Read m1.")], true);
        *host.wake.lock().unwrap() = MeshWake::Off;
        let chats = Chats::with_cap(host.clone(), 2);
        let (sink, got) = page();
        open(&chats, &host, A, sink, 0).unwrap();
        until("ready", || chats.with_resident(A, |r| r.ready).unwrap()).await;
        chats.message(A, ClientMessage::Send { text: "hi".into() }).unwrap();
        until("the person's turn", || count(&got, "usage") == 1).await;
        until("idle", || chats.with_resident(A, |r| r.shared().idle()).unwrap()).await;
        let keep = chats.with_resident(A, |r| r.keep.clone()).unwrap();
        let mesh = keep.mesh.clone().unwrap();
        let worker = wired_worker(&mesh).await;
        send_from(&mesh, &worker, "m1").await;
        for wake in [MeshWake::Off, MeshWake::Rules] {
            *host.wake.lock().unwrap() = wake;
            chats.look(A).await;
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert_eq!(host.model.requests.lock().unwrap().len(), 1, "{wake:?}: no woken turn");
            assert_eq!(mesh.unread(&marv_session::mesh::parent_id()).len(), 1, "{wake:?}: m1 waits");
        }
        assert!(
            got.lock().unwrap().iter().any(|e| e["text"].as_str().is_some_and(|t| t.contains("mesh wake is off"))),
            "the page is told why it waits"
        );
        *host.wake.lock().unwrap() = MeshWake::On;
        chats.look(A).await;
        until("the woken turn", || count(&got, "usage") == 2).await;
        assert!(format!("{:?}", host.model.requests.lock().unwrap()[1].0).contains("m1"), "the woken turn read m1");
        assert!(mesh.unread(&marv_session::mesh::parent_id()).is_empty());
    }

    /// Wakes that find every slot taken by a chat that can't go wait in the queue and run in the order they came; the
    /// cap holds throughout. B waits for an answer (never evicted); A then C get a message while asleep.
    #[tokio::test]
    async fn queued_wakes_run_in_arrival_order_and_the_cap_holds() {
        let host = TestHost::new("queue", vec![say("A read it."), say("C read it.")], true);
        let chats = Chats::with_cap(host.clone(), 1);
        let (sink, _b) = page();
        open(&chats, &host, B, sink, 0).unwrap();
        until("B ready", || chats.with_resident(B, |r| r.ready).unwrap()).await;
        chats.close(B);
        // B asks its person something: it can't be unloaded until it's answered.
        let approver = chats.with_resident(B, |r| r.keep.approver.clone()).unwrap();
        let asking = tokio::spawn(async move {
            let request = marv_tools::ApprovalRequest {
                tool: "bash".into(),
                label: "x".into(),
                preview: Default::default(),
                scope: marv_tools::types::Scope { key: "bash:x".into(), description: "x".into() },
                agent: None,
                network: false,
                once: true,
                must_ask: false,
                persists: false,
                protected: None,
                door: None,
                reason: false,
            };
            marv_tools::Approver::approve(approver.as_ref(), request).await
        });
        until("B waiting", || chats.inner().sched.state(&agent(B).unwrap()) == AgentState::Waiting).await;
        chats.sync_rules(&[(A.to_string(), host.folder(A)), (C.to_string(), host.folder(C))]);
        let max = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let watching = {
            let (chats, max) = (chats.clone(), max.clone());
            tokio::spawn(async move {
                loop {
                    max.fetch_max(loaded(&chats), std::sync::atomic::Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
        };
        for id in [A, C] {
            let mesh = chats.with_resident(id, |r| r.keep.mesh.clone().unwrap()).unwrap();
            let worker = wired_worker(&mesh).await;
            send_from(&mesh, &worker, &format!("for {}", &id[..4])).await;
            until("queued", || chats.with_resident(id, |r| r.pending.is_some()).unwrap()).await;
        }
        assert_eq!(chats.inner().sched.queued(), [agent(A).unwrap(), agent(C).unwrap()]);
        assert_eq!(loaded(&chats), 1);
        // B's approval is answered: its slot goes to A, then (once A's woken turn is over) to C.
        let id = until_approval(&chats).await;
        chats.message(B, ClientMessage::Approval { id, decision: marv_tools::Decision::Yes }).unwrap();
        asking.await.unwrap();
        until("both woken", || host.model.requests.lock().unwrap().len() == 2).await;
        let order: Vec<String> = host.model.requests.lock().unwrap().iter().map(|(h, _)| format!("{h:?}")).collect();
        assert!(order[0].contains("for 554"), "A first: {}", order[0]);
        assert!(order[1].contains("for 7c2e"), "then C: {}", order[1]);
        watching.abort();
        assert_eq!(max.load(std::sync::atomic::Ordering::SeqCst), 1, "never past K = 1");
    }

    /// mk-60m.8 review: a sleeping chat's wake waits for a slot; meanwhile the person turns mesh wake off. When the
    /// slot frees and the chat loads, the wake decided earlier doesn't run: no model request, the message waits, and
    /// the chat's page is told why (`WAKE_OFF_NOTE`).
    ///
    /// Proven: dropped `run_wake_then`'s `!wake.any()` check → this test failed (no notice).
    #[tokio::test]
    async fn a_queued_wake_doesnt_run_once_mesh_wake_is_turned_off() {
        use marv_session::config::MeshWake;
        let host = TestHost::new("queue-off", vec![say("A read it.")], true);
        let chats = Chats::with_cap(host.clone(), 1);
        let (sink, _b) = page();
        open(&chats, &host, B, sink, 0).unwrap();
        until("B ready", || chats.with_resident(B, |r| r.ready).unwrap()).await;
        chats.close(B);
        // B asks its person something: it can't be unloaded until it's answered, so A's wake waits for the slot.
        let approver = chats.with_resident(B, |r| r.keep.approver.clone()).unwrap();
        let asking = tokio::spawn(async move {
            let request = marv_tools::ApprovalRequest {
                tool: "bash".into(),
                label: "x".into(),
                preview: Default::default(),
                scope: marv_tools::types::Scope { key: "bash:x".into(), description: "x".into() },
                agent: None,
                network: false,
                once: true,
                must_ask: false,
                persists: false,
                protected: None,
                door: None,
                reason: false,
            };
            marv_tools::Approver::approve(approver.as_ref(), request).await
        });
        until("B waiting", || chats.inner().sched.state(&agent(B).unwrap()) == AgentState::Waiting).await;
        chats.sync_rules(&[(A.to_string(), host.folder(A))]);
        let mesh = chats.with_resident(A, |r| r.keep.mesh.clone().unwrap()).unwrap();
        let worker = wired_worker(&mesh).await;
        send_from(&mesh, &worker, "for A").await;
        until("queued", || chats.with_resident(A, |r| r.pending.is_some()).unwrap()).await;
        *host.wake.lock().unwrap() = MeshWake::Off;
        let (sink, a_page) = page();
        chats.with_resident(A, |r| r.shared().attach(sink, 0, None));
        let id = until_approval(&chats).await;
        chats.message(B, ClientMessage::Approval { id, decision: marv_tools::Decision::Yes }).unwrap();
        asking.await.unwrap();
        until("the page is told", || a_page.lock().unwrap().iter().any(|e| e["text"] == marv_web::host::WAKE_OFF_NOTE)).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(host.model.requests.lock().unwrap().is_empty(), "no model request");
        assert_eq!(mesh.unread(&marv_session::mesh::parent_id()).len(), 1, "the message waits");
    }

    async fn until_approval(chats: &Chats) -> u64 {
        let (sink, got) = page();
        chats.with_resident(B, |r| r.shared().attach(sink, 0, None));
        until("B's approval", || count(&got, "approval") == 1).await;
        let id = got.lock().unwrap().iter().find(|e| e["type"] == "approval").unwrap()["id"].as_u64().unwrap();
        chats.with_resident(B, |r| r.shared().detach());
        id
    }

    /// A sleeping chat whose folder was removed: the wake is refused before it's counted, the person is told once in
    /// its events, and the message waits in its inbox. No session loads.
    /// Runtime-kernel 3: the desktop fills the registry with its chats (authoritative, with their scheduler state);
    /// every chat's `mesh_list` reads it, and a chat that leaves the mesh leaves it.
    #[tokio::test]
    async fn the_desktop_fills_the_registry_with_its_chats() {
        let host = TestHost::new("reg", vec![say("Hello.")], true);
        let chats = Chats::with_cap(host.clone(), 2);
        chats.sync_rules(&[(B.to_string(), host.folder(B))]);
        let (sink, _got) = page();
        open(&chats, &host, A, sink, 0).unwrap();
        until("ready", || chats.with_resident(A, |r| r.ready).unwrap()).await;
        let name = |id: &str| marv_kernel::mesh_name_for(&host.folder(id), &agent(id).unwrap());
        let key = |id: &str| marv_kernel::AgentKey::Chat(agent(id).unwrap());
        until("A idle in the registry", || {
            chats.registry().snapshot().get(&key(A)).is_some_and(|e| e.state == Some(AgentState::Idle))
        })
        .await;
        let snap = chats.registry().snapshot();
        let b = snap.get(&key(B)).unwrap();
        assert_eq!((b.mesh_name.clone(), b.state, b.kind), (name(B), Some(AgentState::Sleeping), marv_kernel::AgentKind::Chat));
        assert_eq!(b.folder, host.folder(B));
        assert!(b.can(&marv_kernel::Reach::Inbox("parent".into())));
        // Each chat lists from it: A's mesh_list sees B (asleep, its door open), as a scan would.
        let mesh = chats.with_resident(A, |r| r.keep.mesh.clone().unwrap()).unwrap();
        assert!(Arc::ptr_eq(&mesh.registry(), chats.registry()));
        let dir = mesh.peer_dir().unwrap();
        let listed = mesh.registry().peers(&dir).await;
        assert!(listed.iter().all(|p| p.info.live.is_none()), "the registry never carries live numbers");
        assert_eq!(listed, scanned_for_models(&dir).await);
        assert_eq!(listed.iter().map(|p| p.session.clone()).collect::<Vec<_>>(), {
            let mut names = vec![name(A), name(B)];
            names.sort();
            names
        });
        chats.stop(B);
        assert!(chats.registry().snapshot().get(&key(B)).is_none(), "stopped: off the mesh, out of the registry");
    }

    /// A door scan as `mesh_list` would see it: the doors' `live` numbers (runtime-kernel §20.2) are the person's
    /// views', never the registry's (§20.8), so they're cleared before comparing.
    async fn scanned_for_models(dir: &std::path::Path) -> Vec<marv_mesh::discovery::PeerSwitchboard> {
        let mut scanned = marv_mesh::discovery::list(dir).await.unwrap();
        for p in &mut scanned {
            p.info.live = None;
        }
        scanned
    }

    /// Runtime-kernel chat card: a sleeping chat's card comes from its saved session, a loaded one's from its turns
    /// (the person's typed words, never the model's reply); both are in the registry, and another chat's `mesh_list`
    /// shows them, as a scan of the doors would.
    #[tokio::test]
    async fn a_chats_card_is_in_the_registry_awake_or_asleep() {
        let host = TestHost::new("card", vec![say("MODEL REPLY: done.")], true);
        let saved = marv_kernel::Card {
            title: "Set up the Rust API skeleton".into(),
            now: "add auth middleware".into(),
            last_active_ms: 3,
            ..Default::default()
        };
        host.cards.lock().unwrap().insert(B.into(), saved.clone());
        let chats = Chats::with_cap(host.clone(), 2);
        chats.set_cards(true);
        chats.sync_rules(&[(B.to_string(), host.folder(B))]);
        let (sink, _got) = page();
        open(&chats, &host, A, sink, 0).unwrap();
        until("ready", || chats.with_resident(A, |r| r.ready).unwrap()).await;
        let key = |id: &str| marv_kernel::AgentKey::Chat(agent(id).unwrap());
        let card = |id: &str| chats.registry().snapshot().get(&key(id)).and_then(|e| e.card.clone());
        until("B's saved card", || card(B).as_ref() == Some(&saved)).await;
        assert_eq!(chats.registry().snapshot().get(&key(B)).unwrap().state, Some(AgentState::Sleeping));

        chats.message(A, ClientMessage::Send { text: "fix the\nlogin test".into() }).unwrap();
        until("A's card", || card(A).is_some_and(|c| c.title == "fix the login test")).await;
        let a = card(A).unwrap();
        assert_eq!(a.now, "fix the login test");
        assert!(a.last_active_ms > 0 && !format!("{a:?}").contains("MODEL REPLY"));

        let mesh = chats.with_resident(A, |r| r.keep.mesh.clone().unwrap()).unwrap();
        let dir = mesh.peer_dir().unwrap();
        let listed = mesh.registry().peers(&dir).await;
        assert!(listed.iter().all(|p| p.info.live.is_none()), "the registry never carries live numbers");
        assert_eq!(listed, scanned_for_models(&dir).await, "as a scan of the doors");
        let mut w = marv_mesh::tools::Wireable::new(mesh.session.clone(), marv_session::mesh::parent_id());
        w.peers = listed;
        w.cards = mesh.cards_on();
        let unruled = marv_mesh::tools::list_answer(&[], &w);
        assert!(unruled.text.contains("(sleeping): ") && !unruled.text.contains("Rust API"), "no rule: {}", unruled.text);
        let b = marv_kernel::mesh_name_for(&host.folder(B), &agent(B).unwrap());
        w.covered = vec![(b, marv_kernel::PolicyId(1))];
        let answer = marv_mesh::tools::list_answer(&[], &w);
        assert!(
            answer.text.contains("{\"title\":\"Set up the Rust API skeleton\",\"now\":\"add auth middleware\"}"),
            "{}",
            answer.text
        );
        // Card words are fenced, not tainting (decided 2026-10-19, option B: `CARD_WORDS_TAINT`).
        assert!(!answer.from_mesh);
        // `mesh.cards` turned off in the config: no chat publishes its card any more.
        chats.set_cards(false);
        assert!(card(A).is_none() && card(B).is_none());
    }

    /// Runtime-kernel §9.2: under a read rule one chat reads another without a turn there: a loaded chat from its
    /// session (the person's words and the assistant's, never a tool's output), with a quiet notice in it; a chat the
    /// person marked private refuses; a sleeping one is never woken by a read.
    #[tokio::test]
    async fn a_chat_reads_another_under_a_read_rule_and_a_private_one_refuses() {
        let host = TestHost::new("read", vec![say("MODEL REPLY: the login test passes now.")], true);
        let config = host.dir.path().join("c");
        let chats = Chats::with_cap(host.clone(), 2);
        chats.set_cards(true);
        chats.sync_rules(&[(B.to_string(), host.folder(B))]);
        let (sink, got) = page();
        open(&chats, &host, A, sink, 0).unwrap();
        until("ready", || chats.with_resident(A, |r| r.ready).unwrap()).await;
        chats.message(A, ClientMessage::Send { text: "fix the login test".into() }).unwrap();
        until("the turn", || count(&got, "turn_end") >= 1).await;
        let session = chats.with_resident(A, |r| r.keep.session().unwrap()).unwrap();
        until("idle", || !session.busy()).await;

        let home = Path::new("/nonexistent-home");
        let store = marv_kernel::PolicyStore::new(&config, home).unwrap();
        let scope = marv_kernel::FolderScope::new(host.dir.path(), home).unwrap();
        let rule = |mode| marv_kernel::NewRule {
            from: marv_kernel::Subject::Folder(scope.clone()),
            to: marv_kernel::Subject::Folder(scope.clone()),
            mode,
            mutual: true,
            expires_at_ms: None,
            by: marv_kernel::RuleBy::Desktop,
            created_at_ms: 1,
            label: String::new(),
        };
        store.add(rule(marv_kernel::Mode::Send)).await.unwrap();
        let mesh = |id: &str| chats.with_resident(id, |r| r.keep.mesh.clone().unwrap()).unwrap();
        let (a, b) = (mesh(A), mesh(B));
        for m in [&a, &b] {
            m.set_rules(Arc::new(marv_kernel::FileRules::new(store.clone())));
        }
        let name = |id: &str| marv_kernel::mesh_name_for(&host.folder(id), &agent(id).unwrap());
        let name_a = name(A);
        let read = |ask| marv_mesh::chat_read::read_chat(&b, &name_a, ask);
        let refused = read(marv_mesh::chat_read::ReadAsk::tail(5)).await.unwrap_err();
        assert_eq!(refused.reason, marv_kernel::ReadRefusal::NoRule, "a send rule doesn't allow reads");

        store.add(rule(marv_kernel::Mode::Read)).await.unwrap();
        let text = read(marv_mesh::chat_read::ReadAsk::tail(5)).await.unwrap();
        assert!(text.contains(r#"{"person":"fix the login test"}"#), "{text}");
        assert!(text.contains("MODEL REPLY: the login test passes now."), "{text}");
        let line = format!("Read by {} · last 5 turns", name(B));
        until("the notice", || got.lock().unwrap().iter().any(|e| e["type"] == "notice" && e["text"] == line.as_str())).await;

        host.private.lock().unwrap().insert(A.to_string());
        let refused = read(marv_mesh::chat_read::ReadAsk::summary()).await.unwrap_err();
        assert_eq!(refused.reason, marv_kernel::ReadRefusal::Private);

        // B sleeps: read from its saved session (none here), and never loaded for it.
        let refused = marv_mesh::chat_read::read_chat(&a, &name(B), marv_mesh::chat_read::ReadAsk::summary()).await;
        assert_eq!(refused.unwrap_err().reason, marv_kernel::ReadRefusal::Unavailable);
        assert!(chats.with_resident(B, |r| r.loaded.is_none()).unwrap(), "a read never wakes it");
    }

    /// Runtime-kernel 3 (§5.4): a scripted set of chats on the mesh (two doors, a rule made and one found expired,
    /// sends delivered and refused, a wake, loads and an eviction under K = 1, a revoke, a chat stopped) logs its
    /// events; replaying the log rebuilds exactly the live kernel state: the registry (the desktop's own chats and their
    /// states), the rules and the states.
    #[tokio::test]
    async fn replaying_the_event_log_rebuilds_the_live_kernel_state() {
        let host = TestHost::new("replay", vec![say("Read it.")], true);
        let config = host.dir.path().join("c");
        let log = marv_session::kernel_events::EventWriter::new(&config).unwrap();
        let chats = Chats::with_cap(host.clone(), 1);
        chats.set_events(log.clone());
        let store = marv_kernel::PolicyStore::new(&config, Path::new("/nonexistent-home")).unwrap();
        let scope = marv_kernel::FolderScope::new(host.dir.path(), Path::new("/nonexistent-home")).unwrap();
        let rule = |expires_at_ms| marv_kernel::NewRule {
            from: marv_kernel::Subject::Folder(scope.clone()),
            to: marv_kernel::Subject::Folder(scope.clone()),
            mode: marv_kernel::Mode::Send,
            mutual: true,
            expires_at_ms,
            by: marv_kernel::RuleBy::Desktop,
            created_at_ms: ms(),
            label: "Test chats".into(),
        };
        // Two rules, as the Mesh panel makes them; one already past its expiry, noticed as the app's rules look does.
        let (kept, snapshot) = store.add(rule(None)).await.unwrap();
        marv_session::kernel_events::log_created(log.as_ref(), kept, &snapshot);
        let (old, snapshot) = store.add(rule(Some(1))).await.unwrap();
        marv_session::kernel_events::log_created(log.as_ref(), old, &snapshot);
        let expiries = Mutex::new(marv_kernel::Expiries::default());
        marv_session::kernel_events::notice_expiries(&expiries, &snapshot, ms(), log.as_ref());

        // A and B on the mesh (B asleep, kept by the rule), A opened and loaded.
        chats.sync_rules(&[(A.to_string(), host.folder(A)), (B.to_string(), host.folder(B))]);
        let (sink, got) = page();
        open(&chats, &host, A, sink, 0).unwrap();
        until("A ready", || chats.with_resident(A, |r| r.ready).unwrap()).await;
        // A wired send wakes A; an unwired one is refused.
        let mesh = chats.with_resident(A, |r| r.keep.mesh.clone().unwrap()).unwrap();
        let worker = wired_worker(&mesh).await;
        send_from(&mesh, &worker, "wake up").await;
        until("the woken turn", || count(&got, "usage") == 1).await;
        let stranger = mesh.plug_next("worker").unwrap();
        assert!(mesh.deliver(&stranger.id, "agent.parent.inbox", WireMode::Send, json!({"text": "no"})).await.is_err());
        // B opened: under K = 1, A (idle, its page closed) is evicted.
        chats.close(A);
        let (sink, _b) = page();
        open(&chats, &host, B, sink, 0).unwrap();
        until("B ready, A asleep", || {
            chats.with_resident(B, |r| r.ready).unwrap() && chats.inner().sched.state(&agent(A).unwrap()) == AgentState::Sleeping
        })
        .await;
        // The rule is revoked; C comes and goes.
        store.revoke(kept).await.unwrap();
        marv_session::kernel_events::log_revoked(log.as_ref(), kept);
        chats.sync_rules(&[(C.to_string(), host.folder(C))]);
        chats.stop(C);
        until("idle", || chats.inner().sched.state(&agent(B).unwrap()) == AgentState::Idle).await;
        log.flush().await;

        // The live kernel state.
        let live_registry = chats.registry.own_view();
        let live_rules = marv_kernel::KernelState::rules_of(&store.load().unwrap(), ms());
        let live_states: std::collections::BTreeMap<marv_kernel::AgentKey, AgentState> = {
            let inner = chats.inner();
            inner
                .residents
                .keys()
                .filter_map(|id| agent(id))
                .map(|a| (marv_kernel::AgentKey::Chat(a.clone()), inner.sched.state(&a)))
                .collect()
        };
        // ... and what the log says.
        let mut events = Vec::new();
        let mut files: Vec<PathBuf> = std::fs::read_dir(config.join("events")).unwrap().flatten().map(|e| e.path()).collect();
        files.sort();
        for file in files {
            let (more, skipped) = marv_kernel::parse_log(&std::fs::read_to_string(file).unwrap());
            assert_eq!(skipped, 0);
            events.extend(more);
        }
        let replayed = marv_kernel::replay(events);
        assert_eq!(replayed.registry, live_registry);
        assert_eq!(replayed.policies, live_rules);
        assert_eq!(replayed.states, live_states);
        assert_eq!(live_registry.len(), 2, "A and B; C left");
        assert!(replayed.policies[&old].expired && !replayed.policies.contains_key(&kept));
        assert_eq!(live_states.values().copied().collect::<Vec<_>>().len(), 2);
        assert_eq!((replayed.totals.delivered, replayed.totals.refused, replayed.totals.wakes), (1, 1, 1));
    }

    #[tokio::test]
    async fn a_wake_for_a_chat_whose_folder_is_gone_is_refused_and_told() {
        let host = TestHost::new("gone", vec![say("never")], true);
        let chats = Chats::with_cap(host.clone(), 1);
        chats.sync_rules(&[(A.to_string(), host.folder(A))]);
        let (sink, got) = page();
        chats.with_resident(A, |r| r.shared().attach(sink, 0, None));
        std::fs::remove_dir_all(host.folder(A)).unwrap();
        let mesh = chats.with_resident(A, |r| r.keep.mesh.clone().unwrap()).unwrap();
        let worker = wired_worker(&mesh).await;
        send_from(&mesh, &worker, "hello?").await;
        until("the notice", || count(&got, "notice") == 1).await;
        let text = got.lock().unwrap()[0]["text"].as_str().unwrap().to_string();
        assert!(text.contains("can't wake") && text.contains("isn't there"), "{text}");
        send_from(&mesh, &worker, "still there?").await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(count(&got, "notice"), 1, "told once");
        assert_eq!(mesh.unread(&marv_session::mesh::parent_id()).len(), 2);
        assert_eq!(loaded(&chats), 0);
        assert!(host.model.requests.lock().unwrap().is_empty());
        let limit = chats.with_resident(A, |r| format!("{:?}", r.keep.wake_limit.lock().unwrap())).unwrap();
        assert!(!limit.contains("worker"), "no wake was charged: {limit}");
    }

    /// The eviction filter is #155's, unchanged (§5.3 "must not change"): over the same chats the scheduler picks what
    /// `background::to_evict` picks: the least recently active idle chat no page shows, never a busy, waiting or shown
    /// one.
    #[test]
    fn the_scheduler_evicts_what_155_evicted() {
        use crate::background::{LiveChat, to_evict};
        let start = Instant::now();
        let mut live = HashMap::new();
        let mut sched = Scheduler::new(MAX_LIVE);
        let mut keep_alive = Vec::new();
        for i in 0..MAX_LIVE {
            let (to_chat, from) = mpsc::unbounded_channel();
            keep_alive.push(from);
            let id = format!("{i:08x}-0000-4000-8000-000000000000");
            let seen = start + Duration::from_secs(i as u64);
            live.insert(id.clone(), LiveChat { to_chat, key: String::new(), shared: Arc::default(), seen });
            sched.request(agent(&id).unwrap(), Request::Open { by_person: true }, 0);
            sched.update(&agent(&id).unwrap(), AgentState::Idle, i as u64);
        }
        let id = |i: usize| format!("{i:08x}-0000-4000-8000-000000000000");
        live[&id(0)].shared().deliver(json!({"type": "turn_start"}));
        sched.update(&agent(&id(0)).unwrap(), AgentState::Busy, 0);
        live[&id(1)].shared().deliver(json!({"type": "approval", "id": 1}));
        sched.update(&agent(&id(1)).unwrap(), AgentState::Waiting, 1);
        live[&id(2)].shared().attach(Box::new(|_| true), 0, None);
        sched.attach(&agent(&id(2)).unwrap(), true, 2);
        assert_eq!(to_evict(&live, "new"), Some(id(3)));
        assert_eq!(sched.victim(None).map(|a| a.to_string()), Some(id(3)));
    }

    /// D13: a chat a rule covers is on the mesh from the start (door open, no session), opened this run or not; once no
    /// rule covers it, one only a rule brought up leaves; a chat the person stopped isn't brought back.
    #[tokio::test]
    async fn a_rule_covered_chat_keeps_a_door_without_ever_being_opened() {
        let host = TestHost::new("rules", vec![say("Hello.")], true);
        let chats = Chats::new(host.clone());
        chats.sync_rules(&[(B.to_string(), host.folder(B))]);
        let keep = chats.with_resident(B, |r| (r.opened, r.loaded.is_none(), r.keep.clone())).unwrap();
        assert!(!keep.0 && keep.1, "brought up asleep");
        let name = keep.2.mesh_name.clone().unwrap();
        let socket = host.dir.path().join("r/marv/mesh").join(format!("{name}.sock"));
        assert!(socket.exists());
        let mesh = keep.2.mesh.clone().unwrap();
        let worker = wired_worker(&mesh).await;
        send_from(&mesh, &worker, "anyone there?").await;
        assert_eq!(mesh.unread(&marv_session::mesh::parent_id()).len(), 1, "its inbox takes the message");
        // Opened by the person: it stays when the rule goes.
        let (sink, _got) = page();
        open(&chats, &host, A, sink, 0).unwrap();
        chats.sync_rules(&[(A.to_string(), host.folder(A))]);
        assert!(chats.with_resident(B, |_| ()).is_none(), "no rule covers it now: it left the mesh");
        assert!(!socket.exists());
        chats.sync_rules(&[]);
        assert!(chats.with_resident(A, |_| ()).is_some(), "opened this run: it keeps its door");
        chats.stop(A);
        chats.sync_rules(&[(A.to_string(), host.folder(A))]);
        assert!(chats.with_resident(A, |_| ()).is_none(), "the person stopped it: no rule brings it back this run");
    }

    /// D12: a person opening a chat never waits for a slot. Over K (every loaded chat waiting for an answer), the open
    /// is admitted past it, a wake that comes meanwhile queues, and once a chat can go it's unloaded, back to K. The
    /// sidebar shows the unloaded chat asleep, on the mesh.
    #[tokio::test]
    async fn a_person_opens_over_k_and_the_next_chat_that_can_go_is_unloaded() {
        let host = TestHost::new("person", vec![say("for C")], true);
        let chats = Chats::with_cap(host.clone(), 1);
        let (sink, _a) = page();
        open(&chats, &host, A, sink, 0).unwrap();
        until("A ready", || chats.with_resident(A, |r| r.ready).unwrap()).await;
        chats.close(A);
        let approver = chats.with_resident(A, |r| r.keep.approver.clone()).unwrap();
        let asking = tokio::spawn(async move {
            let request = marv_tools::ApprovalRequest {
                tool: "bash".into(),
                label: "x".into(),
                preview: Default::default(),
                scope: marv_tools::types::Scope { key: "bash:x".into(), description: "x".into() },
                agent: None,
                network: false,
                once: true,
                must_ask: false,
                persists: false,
                protected: None,
                door: None,
                reason: false,
            };
            marv_tools::Approver::approve(approver.as_ref(), request).await
        });
        until("A waiting", || chats.inner().sched.state(&agent(A).unwrap()) == AgentState::Waiting).await;
        let (sink, _b) = page();
        open(&chats, &host, B, sink, 0).unwrap();
        assert_eq!(loaded(&chats), 2, "the person didn't wait");
        until("B ready", || chats.with_resident(B, |r| r.ready).unwrap()).await;
        // A wake for a sleeping chat meanwhile: K is hard for it.
        chats.sync_rules(&[(C.to_string(), host.folder(C))]);
        let mesh = chats.with_resident(C, |r| r.keep.mesh.clone().unwrap()).unwrap();
        let worker = wired_worker(&mesh).await;
        send_from(&mesh, &worker, "for C").await;
        until("C queued", || chats.with_resident(C, |r| r.pending.is_some()).unwrap()).await;
        assert_eq!(loaded(&chats), 2);
        // A's approval is answered: A can go, so it's unloaded, back to K; B is shown, so C still waits.
        let (sink, got) = page();
        chats.with_resident(A, |r| r.shared().attach(sink, 0, None));
        until("A's approval", || count(&got, "approval") == 1).await;
        let id = got.lock().unwrap().iter().find(|e| e["type"] == "approval").unwrap()["id"].as_u64().unwrap();
        chats.with_resident(A, |r| r.shared().detach());
        chats.message(A, ClientMessage::Approval { id, decision: marv_tools::Decision::No }).unwrap();
        asking.await.unwrap();
        until("A unloaded", || chats.with_resident(A, |r| r.loaded.is_none()).unwrap()).await;
        assert_eq!(loaded(&chats), 1);
        assert!(chats.with_resident(C, |r| r.loaded.is_none()).unwrap(), "B is shown: C waits");
        let states = chats.states();
        let a = states.iter().find(|s| s.id == A).unwrap();
        assert!(a.asleep && !a.busy && a.waiting == 0, "the sidebar: asleep, on the mesh");
        // B's page goes: C's wake takes the slot.
        chats.close(B);
        until("C's woken turn", || host.model.requests.lock().unwrap().len() == 1).await;
        assert_eq!(loaded(&chats), 1);
    }

    // ---- runtime-kernel §15: interrupts, signals, timers ----

    /// The kernel's events, kept.
    #[derive(Default)]
    pub struct Kept(pub Mutex<Vec<KernelEvent>>);
    impl marv_kernel::EventLog for Kept {
        fn append(&self, _at_ms: u64, event: KernelEvent) {
            self.0.lock().unwrap().push(event);
        }
    }

    /// A grant that holds (or doesn't), for timers.
    pub struct Live(pub Mutex<Option<marv_kernel::GrantId>>, pub std::sync::atomic::AtomicBool);
    impl Live {
        pub fn new(grant: Option<u64>) -> Arc<Self> {
            Arc::new(Self(Mutex::new(grant.map(marv_kernel::GrantId)), Default::default()))
        }
        /// The grant is still in the file, but expired.
        pub fn expire(&self) {
            *self.0.lock().unwrap() = None;
            self.1.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        pub fn set(&self, grant: Option<u64>) {
            *self.0.lock().unwrap() = grant.map(marv_kernel::GrantId);
        }
    }
    impl marv_session::grants::ChatGrant for Live {
        fn live(&self) -> Option<marv_kernel::GrantId> {
            *self.0.lock().unwrap()
        }
        fn granted(&self) -> bool {
            self.0.lock().unwrap().is_some() || self.1.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn expired(&self) -> bool {
            self.1.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn covers(&self, _root: &Path) -> bool {
            true
        }
        fn used(&self, _grant: marv_kernel::GrantId, _tool: &str, _audit_key: &str, _turn: u32) {}
        fn spend(&self, _cost_usd: Option<f64>, _tokens: u64) {}
    }

    fn last_user(request: &[marv_core::ChatTurn]) -> String {
        request
            .iter()
            .rev()
            .find_map(|t| match t {
                marv_core::ChatTurn::User { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    fn timer(by: TimerBy, note: &str) -> marv_session::timers::TimerWake {
        marv_session::timers::TimerWake {
            id: marv_kernel::TimerId(1),
            by,
            note: note.into(),
            late: false,
            grant: Some(marv_kernel::GrantId(1)),
            hop: 0,
            at_ms: 0,
        }
    }

    /// Kernel #41: the watchdog finds the chats with a turn running (here one waiting for an approval) and stops them
    /// through the person's stop path: the session ends, the chat leaves the mesh, and nothing is busy any more. An idle
    /// chat isn't listed.
    #[tokio::test]
    async fn the_watchdog_finds_running_turns_and_stops_them_the_normal_way() {
        let write =
            marv_guards::use_tools(vec![marv_guards::tool_call("w1", "write_file", json!({"path": "a.txt", "content": "x"}))]);
        let host = TestHost::new("watchdog", vec![write, say("Wrote.")], false);
        let chats = Chats::new(host.clone());
        let (sink, got) = page();
        open(&chats, &host, A, sink, 0).unwrap();
        let (sink_b, _) = page();
        open(&chats, &host, B, sink_b, 0).unwrap();
        until("ready", || chats.with_resident(A, |r| r.ready).unwrap() && chats.with_resident(B, |r| r.ready).unwrap()).await;
        assert!(chats.busy_ids().is_empty());
        chats.message(A, ClientMessage::Send { text: "go".into() }).unwrap();
        until("the approval", || count(&got, "approval") == 1).await;
        assert_eq!(chats.busy_ids(), [A], "B is idle");
        assert_eq!(chats.resident_count(), 2);
        let (stopped, sessions) = chats.stop_running();
        assert_eq!((stopped.as_slice(), sessions.len()), ([A.to_string()].as_slice(), 1));
        let session = sessions.into_iter().next().unwrap();
        assert!(chats.busy_ids().is_empty());
        assert!(!chats.is_loaded(A) && chats.is_loaded(B));
        assert_eq!(chats.resident_count(), 1);
        // Its turn ends with it (the approval it waited for is gone), before the test's folder goes.
        until("A's turn ended", || !session.busy()).await;
        drop(session);
        chats.stop(B);
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    /// §15.2: what waits while a turn runs goes in priority order once it ends: the person's message (sent while the
    /// turn waited for an approval) runs before a fired timer that was queued first.
    #[tokio::test]
    async fn the_persons_message_runs_before_a_queued_timer() {
        let write =
            marv_guards::use_tools(vec![marv_guards::tool_call("w1", "write_file", json!({"path": "a.txt", "content": "x"}))]);
        let host = TestHost::new("order", vec![write, say("Wrote."), say("Person."), say("Timer.")], false);
        *host.grant.lock().unwrap() = Some(Live::new(Some(1)));
        let chats = Chats::new(host.clone());
        let (sink, got) = page();
        open(&chats, &host, A, sink, 0).unwrap();
        until("ready", || chats.with_resident(A, |r| r.ready).unwrap()).await;
        chats.message(A, ClientMessage::Send { text: "go".into() }).unwrap();
        until("the approval", || count(&got, "approval") == 1).await;
        chats.with_resident(A, |r| r.queue.push(InterruptSource::Timer, Queued::Timer(timer(TimerBy::Person, "check CI"))));
        chats.message(A, ClientMessage::Send { text: "and then this".into() }).unwrap();
        let id = got.lock().unwrap().iter().find(|e| e["type"] == "approval").unwrap()["id"].as_u64().unwrap();
        chats.message(A, ClientMessage::Approval { id, decision: marv_tools::Decision::Yes }).unwrap();
        until("three turns", || host.model.requests.lock().unwrap().len() == 4).await;
        let requests = host.model.requests.lock().unwrap().clone();
        assert_eq!(last_user(&requests[2].0), "and then this");
        assert!(last_user(&requests[3].0).contains("check CI"), "{}", last_user(&requests[3].0));
        assert!(last_user(&requests[3].0).starts_with(marv_session::timers::PERSON_TIMER_LINE));
    }

    /// §15.4 and §15.3: an ungranted woken turn has no deadline (it asks; nothing pauses it); a granted one that runs
    /// past its deadline is stopped and the chat paused (told, and in the log as a preempt); while it's paused a timer
    /// waits; the person writing to it resumes it, and the timer runs after their message.
    #[tokio::test]
    async fn only_a_granted_turn_stops_at_its_deadline_and_the_persons_message_resumes_the_chat() {
        let write =
            marv_guards::use_tools(vec![marv_guards::tool_call("w1", "write_file", json!({"path": "a.txt", "content": "x"}))]);
        // A command with the network: past yolo's set, so it asks under a grant too.
        let fetch = marv_guards::use_tools(vec![marv_guards::tool_call(
            "f1",
            "bash",
            json!({"command": "curl https://example.invalid/ci", "network": true}),
        )]);
        let replies = vec![say("Hi."), write, fetch, say("Person."), say("Timer.")];
        let host = TestHost::new("deadline", replies, false);
        let grant = Live::new(None);
        *host.grant.lock().unwrap() = Some(grant.clone());
        let chats = Chats::new(host.clone());
        let kept = Arc::new(Kept::default());
        chats.set_events(kept.clone());
        chats.set_deadline(300);
        let (sink, got) = page();
        open(&chats, &host, A, sink, 0).unwrap();
        until("ready", || chats.with_resident(A, |r| r.ready).unwrap()).await;
        chats.message(A, ClientMessage::Send { text: "hi".into() }).unwrap();
        until("the person's turn", || count(&got, "turn_end") == 1).await;
        // Ungranted: a woken turn's call asks and nobody answers; past the deadline it's still waiting, and not paused.
        let mesh = chats.with_resident(A, |r| r.keep.mesh.clone()).flatten().unwrap();
        let worker = wired_worker(&mesh).await;
        send_from(&mesh, &worker, "write it").await;
        let queue = |note: &str| {
            let note = note.to_string();
            chats.with_resident(A, move |r| {
                r.queue.push(InterruptSource::Timer, Queued::Timer(timer(TimerBy::Person, &note)));
                r.kick.notify_one();
            });
        };
        until("its approval", || count(&got, "approval") == 1).await;
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert_eq!(count(&got, "paused"), 0);
        assert!(!chats.with_resident(A, |r| r.paused).unwrap());
        chats.signal(A, SignalKind::Stop).unwrap();
        until("its end", || count(&got, "turn_end") == 2).await;

        // Granted: its networked command asks, and the deadline stops it and pauses the chat.
        grant.set(Some(1));
        queue("fetch CI");
        until("paused", || count(&got, "paused") == 1).await;
        until("its end", || count(&got, "turn_end") == 3).await;
        assert!(chats.with_resident(A, |r| r.paused).unwrap());
        assert!(got.lock().unwrap().iter().any(|e| e["text"] == DEADLINE_TEXT));
        assert!(kept.0.lock().unwrap().iter().any(|e| matches!(
            e,
            KernelEvent::Interrupt { kind: InterruptKind::Deadline, action: marv_kernel::InterruptAction::Preempted, .. }
        )));
        // Paused: the next timer waits.
        queue("again");
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(host.model.requests.lock().unwrap().len(), 3, "held while paused");
        // The person writes: resumed, their message first, then the timer.
        chats.message(A, ClientMessage::Send { text: "back".into() }).unwrap();
        until("both turns", || host.model.requests.lock().unwrap().len() == 5).await;
        assert!(!chats.with_resident(A, |r| r.paused).unwrap());
        assert!(got.lock().unwrap().iter().any(|e| e["text"] == RESUMED_TEXT));
        let requests = host.model.requests.lock().unwrap().clone();
        assert_eq!(last_user(&requests[3].0), "back");
        assert!(last_user(&requests[4].0).contains("again"));
    }

    /// §15.3: "resume the team" resumes the paused chats it's given, and the person's signal is logged as theirs.
    #[tokio::test]
    async fn resume_the_team_resumes_its_paused_chats() {
        let host = TestHost::new("team", vec![say("Hi.")], false);
        let chats = Chats::new(host.clone());
        let kept = Arc::new(Kept::default());
        chats.set_events(kept.clone());
        let (sink, _got) = page();
        open(&chats, &host, A, sink, 0).unwrap();
        until("ready", || chats.with_resident(A, |r| r.ready).unwrap()).await;
        // §20.2: its door says the pause too, so a terminal's `marv kernel ps` and another app's Activity show it.
        let door_paused =
            || chats.with_resident(A, |r| r.mesh.as_ref().map(|m| m.handle.live().is_some_and(|l| l.paused))).unwrap();
        assert_eq!(door_paused(), Some(false));
        assert_eq!(chats.stop_team(&[A.to_string(), B.to_string()]), 1, "B isn't running here");
        assert!(chats.with_resident(A, |r| r.paused).unwrap());
        assert_eq!(door_paused(), Some(true));
        assert_eq!(chats.resume_team(&[A.to_string(), B.to_string()]), 1);
        assert!(!chats.with_resident(A, |r| r.paused).unwrap());
        assert_eq!(door_paused(), Some(false));
        assert!(
            kept.0
                .lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, KernelEvent::Signal { kind: SignalKind::Resume, by: SignalBy::Person, .. }))
        );
    }

    /// §15.4: a due timer for a chat without a live grant is void (removed, told, logged); with one, it fires: a
    /// sleeping chat is loaded for it and the turn reads the note; a one-shot is gone after.
    #[tokio::test]
    async fn a_due_timer_fires_only_under_a_live_grant() {
        let host = TestHost::new("tick", vec![say("Checked.")], false);
        let grant = Live::new(None);
        *host.grant.lock().unwrap() = Some(grant.clone());
        let chats = Chats::new(host.clone());
        let kept = Arc::new(Kept::default());
        chats.set_events(kept.clone());
        // Opened once, so it has its settings and a record; then not shown.
        let (sink, _got) = page();
        open(&chats, &host, A, sink, 0).unwrap();
        until("ready", || chats.with_resident(A, |r| r.ready).unwrap()).await;
        chats.close(A);
        let store = TimerStore::new(&host.dir.path().join("c")).unwrap();
        chats.set_timers(store.clone());
        let chat = AgentId::new(A).unwrap();
        let person = |note: &str| marv_kernel::PersonTimer {
            chat: chat.clone(),
            repeat: marv_kernel::Repeat::Once,
            at_ms: 1_000,
            note: note.into(),
        };
        store.add_person(person("no grant"), 1).await.unwrap();
        chats.tick(5_000).await;
        assert!(store.load().unwrap().timers.is_empty(), "void, and removed");
        assert!(
            kept.0
                .lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, KernelEvent::TimerVoid { reason: TimerVoidReason::GrantEnded, .. }))
        );
        assert!(host.model.requests.lock().unwrap().is_empty());

        grant.set(Some(1));
        store.add_person(person("check CI"), 1).await.unwrap();
        chats.tick(1_000 + 10 * 60_000).await;
        until("the timer's turn", || host.model.requests.lock().unwrap().len() == 1).await;
        assert!(last_user(&host.model.requests.lock().unwrap()[0].0).contains("check CI"));
        assert!(store.load().unwrap().timers.is_empty(), "a one-shot is done");
        assert!(kept.0.lock().unwrap().iter().any(|e| matches!(e, KernelEvent::TimerFired { late: true, .. })));
    }

    /// **The harness plan, phase 2: Claude Code chats sleep and a timer wakes one, continuing its conversation.** Two
    /// Claude Code chats each have a conversation. Idle, the unshown one sleeps (its `claude` ends) and the shown one
    /// stays; the person's timer for the sleeping one loads it, resuming its saved session, and its note is the turn's
    /// message. The other chat isn't touched.
    ///
    /// Proven: `fire` voiding a timer for a chat with no mesh, as before (`NotOnMesh`) → failed (no wake); the load
    /// for the wake starting a new session (`resume: None`) → failed (no `--resume`); `tick_harness` not putting idle
    /// chats to sleep → failed (A still loaded).
    /// A Claude Code chat open in its terminal (`to_terminal`) has its session ended and isn't loaded again by a page
    /// or a message (one `claude` per conversation) until the terminal closes; a working one isn't moved.
    #[cfg(unix)] // a bash script stands in for the agent
    #[tokio::test(flavor = "multi_thread")]
    async fn a_chat_in_its_terminal_isnt_loaded_until_the_terminal_closes() {
        let host = TestHost::new("cc-term", Vec::new(), false);
        let log = fake_claude(&host, A);
        let chats = Chats::new(host.clone());
        let (sink, _got) = page();
        open(&chats, &host, A, sink, 0).unwrap();
        until("ready", || chats.with_resident(A, |r| r.ready).unwrap()).await;
        chats.message(A, ClientMessage::Send { text: "hello".into() }).unwrap();
        let started = || std::fs::read_to_string(&log).unwrap_or_default();
        until("its claude", || started().contains("hello")).await;
        until("idle", || chats.with_resident(A, |r| r.shared().idle()).unwrap()).await;
        chats.to_terminal(A, &host.folder(A), false).unwrap();
        assert!(!chats.is_loaded(A), "its session ended for the terminal");
        let err = open(&chats, &host, A, page().0, 0).err().unwrap_or_default();
        assert!(!chats.is_loaded(A) && err.contains("open in its terminal"), "a page doesn't load it: {err}");
        let _ = chats.message(A, ClientMessage::Send { text: "again".into() });
        assert!(!chats.is_loaded(A), "nor a message");
        assert_eq!(started().matches("START").count(), 1);
        chats.terminal_closed(A);
        until("loaded again for the page that shows it", || chats.is_loaded(A)).await;
    }

    /// Any chat's agent opens in its terminal, a Marv chat's too, and before a page has attached to it (the terminal
    /// and the page's `attach` race when a chat opens): its record is made, with its door and approver, then.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_marv_chat_opens_in_its_terminal_before_its_page_attaches() {
        let host = TestHost::new("term-marv", Vec::new(), false);
        let chats = Chats::new(host.clone());
        assert!(chats.terminal_parts(B).is_none(), "no record yet");
        chats.to_terminal(B, &host.folder(B), false).unwrap();
        assert!(chats.terminal_parts(B).is_some(), "its record, for the terminal's door and cards");
        chats.attach(B, &host.folder(B), "marv", page().0, (0, None));
        assert!(!chats.is_loaded(B), "the page attaches without loading a session");
        chats.terminal_closed(B);
    }

    /// A chat's terminal Marv is the chat on the mesh: the chat's door closes while it runs (so the terminal's Marv can
    /// take the chat's name) and opens again, under the same name, when it ends.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_terminal_marv_borrows_the_chats_door_and_gives_it_back() {
        let host = TestHost::new("term-door", Vec::new(), false);
        let chats = Chats::new(host.clone());
        chats.attach(A, &host.folder(A), "marv", page().0, (0, None));
        let name = chats.with_resident(A, |r| r.keep.mesh_name.clone()).flatten().expect("on the mesh");
        let door = chats.with_resident(A, |r| r.mesh.is_some()).unwrap();
        assert!(door, "its door is open");
        chats.to_terminal(A, &host.folder(A), true).unwrap();
        assert!(chats.with_resident(A, |r| r.mesh.is_none() && r.keep.mesh.is_none()).unwrap(), "lent to the terminal");
        chats.terminal_closed(A);
        assert!(chats.with_resident(A, |r| r.mesh.is_some() && r.keep.mesh.is_some()).unwrap(), "back");
        assert_eq!(
            chats.with_resident(A, |r| r.keep.mesh_name.clone()).flatten().as_deref(),
            Some(name.as_str()),
            "under its name"
        );
    }

    /// A terminal's reader is a plain std thread with no Tokio runtime: its `ended` callback closing the chat's terminal
    /// there must still give the door back (it panicked at `tokio::spawn` before; seen 2026-11-17).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_terminal_ending_on_a_plain_thread_gives_the_door_back() {
        let host = TestHost::new("term-thread", Vec::new(), false);
        let chats = Chats::new(host.clone());
        chats.attach(A, &host.folder(A), "marv", page().0, (0, None));
        chats.to_terminal(A, &host.folder(A), true).unwrap();
        let on = chats.clone();
        std::thread::spawn(move || on.terminal_closed(A)).join().expect("terminal_closed panicked off the runtime");
        assert!(chats.with_resident(A, |r| r.mesh.is_some() && r.keep.mesh.is_some()).unwrap(), "its door is back");
    }

    /// A Marv chat open in its terminal (its door lent to the terminal's Marv) is running to the page: idle, then working
    /// as its terminal says, and gone from the list once the terminal closes. It was left out of the list ("not
    /// running") while it ran.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_terminal_marvs_chat_shows_running() {
        let host = TestHost::new("term-state", Vec::new(), false);
        let chats = Chats::new(host.clone());
        chats.to_terminal(A, &host.folder(A), true).unwrap();
        let state = |chats: &Chats| chats.states().into_iter().find(|s| s.id == A);
        let now = state(&chats).expect("listed while its terminal runs");
        assert!(!now.asleep && !now.busy, "idle, not asleep or working");
        chats.terminal_state(A, AgentState::Busy);
        assert!(state(&chats).is_some_and(|s| s.busy), "working");
        chats.terminal_closed(A);
        assert!(state(&chats).is_none_or(|s| !s.busy), "not working once closed");
    }

    /// Team mail for an agent in its terminal is typed into it once it's idle: never over its turn, fenced as another
    /// agent's words, and taken from the inbox only when it goes in.
    #[cfg(unix)] // a bash script stands in for the agent
    #[tokio::test(flavor = "multi_thread")]
    async fn mail_for_a_terminal_agent_is_typed_in_once_its_idle() {
        let host = TestHost::new("term-mail", Vec::new(), false);
        fake_claude(&host, A);
        let chats = Chats::new(host.clone());
        chats.attach(A, &host.folder(A), "claude-code", page().0, (0, None));
        chats.to_terminal(A, &host.folder(A), false).unwrap();
        chats.terminal_state(A, AgentState::Busy);
        let mesh = chats.with_resident(A, |r| r.keep.mesh.clone()).flatten().expect("on the mesh");
        let worker = wired_worker(&mesh).await;
        send_from(&mesh, &worker, "the lead says: run the tests").await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(host.typed.lock().unwrap().is_empty(), "never over its turn");
        assert!(!mesh.unread(&marv_session::mesh::parent_id()).is_empty(), "the mail waits in its inbox");
        chats.terminal_state(A, AgentState::Idle);
        until("typed in", || !host.typed.lock().unwrap().is_empty()).await;
        let (id, text) = host.typed.lock().unwrap()[0].clone();
        assert_eq!(id, A);
        assert!(text.contains("run the tests") && text.contains(marv_mesh::message::FENCE_TAG), "fenced: {text}");
        assert!(mesh.unread(&marv_session::mesh::parent_id()).is_empty(), "taken from the inbox as it went in");
        assert!(text.contains(HARNESS_MAIL_NOTE), "told how to answer: {text}");
        assert!(chats.with_resident(A, |r| r.inbox.is_some()).unwrap(), "its report waits for the turn");
        chats.terminal_reply(A, "All 42 pass.".into());
        chats.terminal_state(A, AgentState::Idle);
        assert!(chats.with_resident(A, |r| r.inbox.is_some()).unwrap(), "not before its turn on the mail started");
        chats.terminal_state(A, AgentState::Busy);
        chats.terminal_state(A, AgentState::Idle);
        assert!(chats.with_resident(A, |r| r.inbox.is_none()).unwrap(), "its turn ended: the report went");
        chats.terminal_closed(A);
    }

    /// Mail on a wire the person approved by hand doesn't start a turn in a terminal chat (mesh wake: rules only): a
    /// card over its terminal says who it's from (before, a notice on a page the terminal hides), nothing is typed
    /// until the person delivers it, and then it goes in fenced and the card goes.
    #[cfg(unix)] // a bash script stands in for the agent
    #[tokio::test(flavor = "multi_thread")]
    async fn mail_that_doesnt_wake_a_terminal_chat_shows_a_card_to_deliver_it() {
        let host = TestHost::new("term-mail-card", Vec::new(), false);
        *host.wake.lock().unwrap() = marv_session::config::MeshWake::Rules;
        fake_claude(&host, A);
        let chats = Chats::new(host.clone());
        let (sink, got) = page();
        chats.attach(A, &host.folder(A), "claude-code", sink, (0, None));
        chats.to_terminal(A, &host.folder(A), false).unwrap();
        chats.terminal_state(A, AgentState::Idle);
        let mesh = chats.with_resident(A, |r| r.keep.mesh.clone()).flatten().expect("on the mesh");
        let worker = wired_worker(&mesh).await;
        send_from(&mesh, &worker, "please run the probe").await;
        until("a card", || count(&got, "mail_waiting") == 1).await;
        let card = got.lock().unwrap().iter().find(|e| e["type"] == "mail_waiting").cloned().unwrap();
        assert_eq!(card["count"], 1);
        assert!(card["from"].as_str().unwrap().contains("worker"), "{card}");
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(host.typed.lock().unwrap().is_empty(), "not typed in: the wire doesn't wake it");
        assert_eq!(count(&got, "mail_waiting"), 1, "one card per message");

        chats.deliver_mail(A).unwrap();
        let (id, text) = host.typed.lock().unwrap()[0].clone();
        assert_eq!(id, A);
        assert!(text.contains("please run the probe") && text.contains(marv_mesh::message::FENCE_TAG), "fenced: {text}");
        until("the card goes", || got.lock().unwrap().iter().any(|e| e["type"] == "mail_waiting" && e["clear"] == true)).await;
        assert!(chats.deliver_mail(A).is_err(), "nothing left to deliver");
        chats.terminal_closed(A);
    }

    /// A wire request to a terminal chat's door (another Marv's first message) is answered from the card over its
    /// terminal: the person's yes reaches the door's approver, and the card goes.
    #[cfg(unix)] // a bash script stands in for the agent
    #[tokio::test(flavor = "multi_thread")]
    async fn a_wire_card_over_a_terminal_chat_is_answered() {
        let host = TestHost::new("term-wire", Vec::new(), false);
        fake_claude(&host, A);
        let chats = Chats::new(host.clone());
        let (sink, got) = page();
        chats.attach(A, &host.folder(A), "claude-code", sink, (0, None));
        chats.to_terminal(A, &host.folder(A), false).unwrap();
        chats.terminal_state(A, AgentState::Idle);
        let approver = chats.with_resident(A, |r| r.keep.approver.clone()).unwrap();
        let mesh = chats.with_resident(A, |r| r.keep.mesh.clone()).flatten().expect("on the mesh");
        let request = WireRequest {
            from: PlugId::new("peer-flologix-30c5e40a").unwrap(),
            to: ReceptacleName::new("agent.parent.inbox").unwrap(),
            mode: WireMode::Send,
            asked_by: PlugId::new("peer-flologix-30c5e40a").unwrap(),
        };
        let _ = &mesh;
        let asking = tokio::spawn(async move {
            marv_tools::Approver::approve(approver.as_ref(), marv_session::mesh::wire_approval(&request)).await
        });
        until("the card", || count(&got, "approval") == 1).await;
        let id = got.lock().unwrap().iter().find(|e| e["type"] == "approval").unwrap()["id"].as_u64().unwrap();
        chats.message(A, ClientMessage::Approval { id, decision: marv_tools::Decision::Yes }).unwrap();
        let answered = tokio::time::timeout(Duration::from_secs(5), asking).await.expect("answered").unwrap();
        assert_eq!(answered, marv_tools::Decision::Yes);
        chats.terminal_closed(A);
    }

    /// A line the person sends to a chat's terminal agent is their turn, as in a chat view: hop 0 again (a turn woken by
    /// a report was deeper, and a task it handed on from there wouldn't start a teammate's task turn).
    #[cfg(unix)] // a bash script stands in for the agent
    #[tokio::test(flavor = "multi_thread")]
    async fn the_person_typing_in_a_terminal_starts_their_own_turn() {
        let host = TestHost::new("term-typed", Vec::new(), false);
        fake_claude(&host, A);
        let chats = Chats::new(host.clone());
        chats.attach(A, &host.folder(A), "claude-code", page().0, (0, None));
        chats.to_terminal(A, &host.folder(A), false).unwrap();
        let hop = chats.with_resident(A, |r| r.keep.turn_hop.clone()).unwrap();
        hop.new_turn(2);
        chats.person_typed(A);
        assert_eq!(hop.get(), 0);
        chats.terminal_closed(A);
    }

    /// A terminal agent's turn end and its questions reach the person's notifications as a chat view's do: "needs you"
    /// when it asks, withdrawn when it stops, and "finished" when its hooks say a turn ended (never for its screen
    /// drawing and going quiet).
    #[cfg(unix)] // a bash script stands in for the agent
    #[tokio::test(flavor = "multi_thread")]
    async fn a_terminal_agents_questions_and_turn_ends_are_notified() {
        let host = TestHost::new("term-notify", Vec::new(), false);
        fake_claude(&host, A);
        let chats = Chats::new(host.clone());
        chats.attach(A, &host.folder(A), "claude-code", page().0, (0, None));
        chats.to_terminal(A, &host.folder(A), false).unwrap();
        let told = || host.notified.lock().unwrap().iter().map(|(_, kind)| kind.clone()).collect::<Vec<_>>();
        chats.terminal_state(A, AgentState::Busy);
        chats.terminal_state(A, AgentState::Idle);
        assert!(told().is_empty(), "drawing and going quiet isn't a turn's end");
        chats.terminal_state(A, AgentState::Busy);
        chats.terminal_state(A, AgentState::Waiting);
        chats.terminal_state(A, AgentState::Waiting);
        assert_eq!(told(), ["approval"], "once while it asks");
        chats.terminal_state(A, AgentState::Busy);
        assert_eq!(told(), ["approval", "approval_done"], "withdrawn once answered");
        chats.terminal_state(A, AgentState::Idle);
        chats.terminal_finished(A);
        assert_eq!(told(), ["approval", "approval_done", "turn_end"]);
        assert!(chats.busy_ids().is_empty());
        chats.terminal_state(A, AgentState::Busy);
        assert_eq!(chats.busy_ids(), [A.to_string()], "a terminal agent at work is busy for its team's idle notice");
        chats.terminal_closed(A);
    }

    /// A terminal agent's row in Activity: its agent and its process (named, with its PID) once its terminal starts,
    /// its process gone when it ends.
    #[cfg(unix)] // a bash script stands in for the agent
    #[tokio::test(flavor = "multi_thread")]
    async fn a_terminal_agent_shows_on_its_chats_activity_row() {
        let host = TestHost::new("term-row", Vec::new(), false);
        fake_claude(&host, A);
        let chats = Chats::new(host.clone());
        chats.attach(A, &host.folder(A), "claude-code", page().0, (0, None));
        chats.to_terminal(A, &host.folder(A), false).unwrap();
        let meter = chats.with_resident(A, |r| r.meter.clone()).flatten().expect("a harness chat's meter");
        assert_eq!(meter.harness_live(1), None, "nothing until its agent starts");
        chats.terminal_meter(A, Some(("Claude Code", std::process::id())));
        chats.terminal_cost(A, 0.25);
        let live = meter.harness_live(marv_session::time::now_ms()).unwrap();
        assert_eq!(live.core.as_deref(), Some("Claude Code"));
        assert_eq!(live.spend_usd_micros, Some(250_000));
        chats.terminal_meter(A, None);
        chats.terminal_closed(A);
    }

    /// Mail to a Marv chat whose terminal isn't running starts it, and goes in once Marv has drawn and gone quiet: taken
    /// from the chat's door first, so none is lost when the door goes to the terminal's Marv.
    #[tokio::test(flavor = "multi_thread")]
    async fn mail_to_a_marv_chat_starts_its_terminal_and_goes_in_once_ready() {
        let host = TestHost::new("term-marv-mail", Vec::new(), false);
        *host.terminals.lock().unwrap() = Some(Vec::new());
        let chats = Chats::new(host.clone());
        chats.attach(B, &host.folder(B), "marv", page().0, (0, None));
        let mesh = chats.with_resident(B, |r| r.keep.mesh.clone()).flatten().expect("on the mesh");
        let worker = wired_worker(&mesh).await;
        send_from(&mesh, &worker, "the lead says: write the README").await;
        until("its terminal started", || host.terminals.lock().unwrap().as_ref().is_some_and(|t| t.contains(&B.to_string())))
            .await;
        assert!(mesh.unread(&marv_session::mesh::parent_id()).is_empty(), "taken from the door before it's lent");
        // What `start_terminal` does on the desktop: the door goes to the terminal's Marv.
        chats.to_terminal(B, &host.folder(B), true).unwrap();
        chats.terminal_state(B, AgentState::Idle);
        assert!(host.typed.lock().unwrap().is_empty(), "not before Marv has drawn");
        chats.terminal_state(B, AgentState::Busy);
        chats.terminal_state(B, AgentState::Idle);
        let typed = host.typed.lock().unwrap().clone();
        assert!(typed.len() == 1 && typed[0].0 == B && typed[0].1.contains("write the README"), "{typed:?}");
        chats.terminal_closed(B);
    }

    /// A timer due for a chat whose agent runs in its terminal starts the terminal if it isn't running, and is typed in
    /// once the agent has drawn and gone quiet, as an unattended turn (its deadline and grant over it) until it ends.
    #[cfg(unix)] // a bash script stands in for the agent
    #[tokio::test(flavor = "multi_thread")]
    async fn a_timer_for_a_terminal_chat_is_typed_into_its_terminal() {
        let host = TestHost::new("term-timer", Vec::new(), false);
        fake_claude(&host, A);
        *host.terminals.lock().unwrap() = Some(Vec::new());
        *host.grant.lock().unwrap() = Some(Live::new(Some(1)));
        let chats = Chats::new(host.clone());
        let store = TimerStore::new(&host.dir.path().join("c")).unwrap();
        chats.set_timers(store.clone());
        chats.attach(A, &host.folder(A), "claude-code", page().0, (0, None));
        let timer = marv_kernel::PersonTimer {
            chat: AgentId::new(A).unwrap(),
            repeat: marv_kernel::Repeat::Once,
            at_ms: 1_000,
            note: "check CI".into(),
        };
        store.add_person(timer, 1).await.unwrap();
        chats.tick(wall_ms()).await;
        until("its terminal started for the timer", || {
            host.terminals.lock().unwrap().as_ref().is_some_and(|t| t.contains(&A.to_string()))
        })
        .await;
        assert!(!chats.is_loaded(A), "no headless session for it");
        // What `start_terminal` does on the desktop, then the agent draws and goes quiet.
        chats.to_terminal(A, &host.folder(A), false).unwrap();
        chats.terminal_state(A, AgentState::Busy);
        chats.terminal_state(A, AgentState::Idle);
        until("typed in", || !host.typed.lock().unwrap().is_empty()).await;
        let typed = host.typed.lock().unwrap().clone();
        assert!(typed[0].0 == A && typed[0].1.contains("check CI"), "{typed:?}");
        assert_eq!(chats.with_resident(A, |r| r.unattended).unwrap(), Some(true), "an unattended turn, under its grant");
        // Past its deadline: stopped in its terminal, as the person would, and paused.
        chats.set_deadline(0);
        chats.tick(wall_ms()).await;
        assert_eq!(*host.interrupted.lock().unwrap(), [A.to_string()], "stopped in its terminal");
        chats.set_deadline(marv_kernel::TURN_DEADLINE_MS);
        chats.terminal_state(A, AgentState::Busy);
        chats.terminal_state(A, AgentState::Idle);
        assert_eq!(chats.with_resident(A, |r| r.unattended).unwrap(), None, "its turn ended");
        assert!(store.load().unwrap().timers.is_empty(), "a one-shot timer, done");
        chats.terminal_closed(A);
    }

    /// The terminal's mesh server outlives the chat's ending session. That session removes `harness-mcp.sock` as it
    /// goes, which can be after `to_terminal` returns (the person saw "bwrap: Can't find source path
    /// …/harness-mcp.sock" in the release build), so the terminal serves on a name of its own.
    #[cfg(unix)] // a bash script stands in for the agent
    #[tokio::test(flavor = "multi_thread")]
    async fn a_terminals_mesh_socket_survives_the_chats_ending_session() {
        let host = TestHost::new("cc-term-mesh", Vec::new(), false);
        let log = fake_claude(&host, A);
        let chats = Chats::new(host.clone());
        let (sink, _got) = page();
        open(&chats, &host, A, sink, 0).unwrap();
        until("ready", || chats.with_resident(A, |r| r.ready).unwrap()).await;
        chats.message(A, ClientMessage::Send { text: "hi".into() }).unwrap();
        until("its claude", || std::fs::read_to_string(&log).unwrap_or_default().contains("hi")).await;
        until("idle", || chats.with_resident(A, |r| r.shared().idle()).unwrap()).await;
        chats.to_terminal(A, &host.folder(A), false).unwrap();
        let mut spec = host.spec(A).unwrap();
        let (keep, _out) = chats.terminal_parts(A).unwrap();
        spec.resident = Some(keep.clone());
        let own = marv_web::claude_code::mesh_server_at(&spec, &keep.approver, "harness-term-test.sock").expect("on the mesh");
        assert_ne!(own.socket.file_name().unwrap(), "harness-mcp.sock");
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert!(own.socket.exists(), "the terminal's own socket is still there once the chat's session has gone");
    }

    #[cfg(unix)] // a bash script stands in for the agent
    #[tokio::test]
    async fn an_idle_claude_code_chat_sleeps_and_its_timer_wakes_it_into_the_same_conversation() {
        let host = TestHost::new("cc-wake", Vec::new(), false);
        let (log_a, log_b) = (fake_claude(&host, A), fake_claude(&host, B));
        *host.grant.lock().unwrap() = Some(Live::new(Some(1)));
        let chats = Chats::new(host.clone());
        let store = TimerStore::new(&host.dir.path().join("c")).unwrap();
        chats.set_timers(store.clone());
        let read = |log: &PathBuf| std::fs::read_to_string(log).unwrap_or_default();
        let (sink_b, _got_b) = page();
        for (id, sink) in [(A, page().0), (B, sink_b)] {
            open(&chats, &host, id, sink, 0).unwrap();
            until("ready", || chats.with_resident(id, |r| r.ready).unwrap()).await;
            chats.message(id, ClientMessage::Send { text: "remember the number 7".into() }).unwrap();
            until("its session saved", || host.sessions.lock().unwrap().contains_key(id)).await;
            until("idle", || chats.with_resident(id, |r| r.shared().idle()).unwrap()).await;
        }
        let session_a = host.sessions.lock().unwrap()[A].clone();
        assert!(read(&log_a).contains(&format!("--session-id {session_a}")), "{}", read(&log_a));
        chats.close(A);

        chats.set_harness_idle(0);
        chats.tick(wall_ms()).await;
        assert!(!chats.is_loaded(A), "idle and unshown: asleep");
        assert!(chats.is_loaded(B), "on screen: stays");
        assert!(chats.states().iter().any(|s| s.id == A && s.asleep), "the sidebar shows it asleep");
        chats.set_harness_idle(HARNESS_IDLE_MS);

        let timer = marv_kernel::PersonTimer {
            chat: AgentId::new(A).unwrap(),
            repeat: marv_kernel::Repeat::Once,
            at_ms: 1_000,
            note: "check CI".into(),
        };
        store.add_person(timer, 1).await.unwrap();
        chats.tick(wall_ms()).await;
        until("the timer's turn", || read(&log_a).contains("check CI")).await;
        let log = read(&log_a);
        assert_eq!(log.matches("START").count(), 2, "{log}");
        assert!(
            log.lines().filter(|l| l.starts_with("START")).nth(1).unwrap().contains(&format!("--resume {session_a}")),
            "{log}"
        );
        until("its turn ends", || chats.with_resident(A, |r| r.unattended.is_none() && r.shared().idle()).unwrap()).await;
        assert!(store.load().unwrap().timers.is_empty(), "a one-shot is done");
        assert_eq!(read(&log_b).matches("START").count(), 1, "the other chat isn't touched");
    }

    /// **The harness plan, phase 3: mail on the mesh wakes a Claude Code chat**, loaded or asleep. It starts with its
    /// mesh tools (`--mcp-config` naming `__mesh-mcp`), and a teammate's message reaches it as its turn's message,
    /// fenced as another agent's words with Marv's note under it, never as the person's text. Asleep, the mail loads
    /// it and resumes its conversation.
    ///
    /// Proven: `look_harness` without the mail branch → failed (no turn); `harness_mail` sending the message's raw text
    /// → failed (no fence); `mesh_server` not calling `harness_turn_hop` → failed (no hop, so no report stamp).
    #[cfg(unix)] // a bash script stands in for the agent
    #[tokio::test]
    async fn mail_on_the_mesh_wakes_a_claude_code_chat_fenced_loaded_or_asleep() {
        let host = TestHost::new("cc-mail", Vec::new(), false);
        let log = fake_claude(&host, A);
        let chats = Chats::new(host.clone());
        let read = || std::fs::read_to_string(&log).unwrap_or_default();
        open(&chats, &host, A, page().0, 0).unwrap();
        until("ready", || chats.with_resident(A, |r| r.ready).unwrap()).await;
        chats.message(A, ClientMessage::Send { text: "hi".into() }).unwrap();
        until("its session saved", || host.sessions.lock().unwrap().contains_key(A)).await;
        until("idle", || chats.with_resident(A, |r| r.shared().idle()).unwrap()).await;
        assert!(read().lines().next().unwrap().contains("__mesh-mcp"), "its mesh tools: {}", read());
        let mesh = chats.with_resident(A, |r| r.keep.mesh.clone()).flatten().expect("on the mesh");
        // Its plug has a turn hop (no taint on it): its sends carry a hop, and Marv's reports from it are stamped.
        let hop = mesh.turn_hop(&marv_session::mesh::parent_id()).expect("a turn hop for the harness's plug");
        assert!(!hop.clean(), "never clean");
        let worker = wired_worker(&mesh).await;

        send_from(&mesh, &worker, "the lead says: run the tests").await;
        until("the mail's turn", || read().contains("run the tests")).await;
        let line = read().lines().find(|l| l.contains("run the tests")).unwrap().to_string();
        assert!(line.contains(marv_mesh::message::FENCE_TAG), "fenced as mesh data: {line}");
        assert!(line.contains("not from the person"), "{line}");
        until("its turn ends", || chats.with_resident(A, |r| r.unattended.is_none() && r.shared().idle()).unwrap()).await;

        chats.close(A);
        chats.set_harness_idle(0);
        chats.tick(wall_ms()).await;
        assert!(!chats.is_loaded(A), "asleep");
        chats.set_harness_idle(HARNESS_IDLE_MS);
        send_from(&mesh, &worker, "the lead says: now the docs").await;
        until("woken by the mail", || read().contains("now the docs")).await;
        let session = host.sessions.lock().unwrap()[A].clone();
        assert!(read().lines().filter(|l| l.starts_with("START")).nth(1).unwrap().contains(&format!("--resume {session}")));
    }

    /// **H1: Stop and Pause stop a Claude Code chat's running turn**, as they do a Marv chat's, and "Stop the team"
    /// stops it too. Its agent can't stop between steps, so Pause stops the turn now and says so. Proven: `signal`
    /// without the harness interrupt → failed (no INTERRUPT, still busy).
    #[cfg(unix)] // a bash script stands in for the agent
    #[tokio::test]
    async fn stop_and_pause_stop_a_claude_code_chats_turn() {
        let host = TestHost::new("cc-stop", Vec::new(), false);
        let log = slow_claude(&host, A);
        let chats = Chats::new(host.clone());
        let (sink, got) = page();
        open(&chats, &host, A, sink, 0).unwrap();
        until("ready", || chats.with_resident(A, |r| r.ready).unwrap()).await;
        let read = || std::fs::read_to_string(&log).unwrap_or_default();
        let busy = || chats.with_resident(A, |r| !r.shared().idle()).unwrap();

        chats.message(A, ClientMessage::Send { text: "a long job".into() }).unwrap();
        until("its turn runs", || read().contains("TURN") && busy()).await;
        chats.signal(A, SignalKind::Stop).unwrap();
        until("stopped", || read().matches("INTERRUPT").count() == 1 && !busy()).await;

        chats.message(A, ClientMessage::Send { text: "another".into() }).unwrap();
        until("its next turn runs", || read().matches("TURN").count() == 2 && busy()).await;
        chats.signal(A, SignalKind::Pause).unwrap();
        until("paused and stopped", || read().matches("INTERRUPT").count() == 2 && !busy()).await;
        assert_eq!(chats.with_resident(A, |r| r.paused), Some(true));
        let said = got.lock().unwrap().iter().any(|e| e["text"].as_str() == Some(HARNESS_PAUSED_TEXT));
        assert!(said, "the person is told the turn was stopped, not that it ends after a step");
        chats.signal(A, SignalKind::Resume).unwrap();

        chats.message(A, ClientMessage::Send { text: "a third".into() }).unwrap();
        until("its third turn runs", || read().matches("TURN").count() == 3 && busy()).await;
        assert_eq!(chats.stop_team(&[A.to_string()]), 1);
        until("the team stopped it", || read().matches("INTERRUPT").count() >= 3 && !busy()).await;
    }

    /// **H2: a Claude Code chat's turns meet the person's budgets.** Its first turn's cost is in today's meters; past
    /// its chat budget the next turn asks before anything goes to Claude Code, and "no" leaves the message unsent.
    /// Proven: the runner sending without `HarnessBudget::begin` → failed (no card, a second turn ran).
    #[cfg(unix)] // a bash script stands in for the agent
    #[tokio::test]
    async fn a_claude_code_chats_turn_asks_at_its_budget() {
        use marv_kernel::{BudgetScope, BudgetStore, FileBudgets, SpendCap};
        let host = TestHost::new("cc-budget", Vec::new(), false);
        let store = BudgetStore::new(&host.dir.path().join("budgets"), &host.dir.path().join("home")).unwrap();
        let cap = SpendCap { usd_micros: 5_000, tokens: 1_000_000_000 };
        store.set(BudgetScope::Chat(A.into()), Some(cap), 1).await.unwrap();
        let book = marv_session::allocator::MeterBook::new(None, None, Some(Arc::new(FileBudgets::new(store))));
        *host.meters.lock().unwrap() = Some(Arc::new(book));
        let log = fake_claude(&host, A);
        let chats = Chats::new(host.clone());
        let (sink, got) = page();
        open(&chats, &host, A, sink, 0).unwrap();
        until("ready", || chats.with_resident(A, |r| r.ready).unwrap()).await;
        let read = || std::fs::read_to_string(&log).unwrap_or_default();
        let turns = || read().lines().filter(|l| l.contains(r#""type":"user""#)).count();

        chats.message(A, ClientMessage::Send { text: "first".into() }).unwrap();
        until("its first turn", || turns() == 1 && chats.with_resident(A, |r| r.shared().idle()).unwrap()).await;
        let spent = host.meters.lock().unwrap().as_ref().unwrap().spent(&BudgetScope::Chat(A.into())).usd_micros;
        assert_eq!(spent, 10_000, "the stand-in's $0.01 is today's spend");

        chats.message(A, ClientMessage::Send { text: "second".into() }).unwrap();
        let card = || got.lock().unwrap().iter().rev().find(|e| e["type"] == "approval").and_then(|e| e["id"].as_u64());
        until("the budget's card", || card().is_some()).await;
        chats.message(A, ClientMessage::Approval { id: card().unwrap(), decision: marv_tools::Decision::No }).unwrap();
        until("unsent", || got.lock().unwrap().iter().any(|e| e["type"] == "unsent" && e["text"] == "second")).await;
        until("idle", || chats.with_resident(A, |r| r.shared().idle()).unwrap()).await;
        assert_eq!(turns(), 1, "nothing went to Claude Code past the budget");
    }

    /// **A Claude Code chat is a row in Activity, with its numbers.** Its door's `marv/info` carries what its own events
    /// metered: its agent as the model, today's spend and $/h from its turns' cost, so the desktop's Activity and
    /// `marv kernel ps` list it beside the Marvs. Proven: not observing its events → failed (no spend); not setting
    /// its door's `live` → failed (no `live`).
    #[cfg(unix)] // a bash script stands in for the agent
    #[tokio::test]
    async fn a_claude_code_chat_is_a_row_in_activity_with_its_spend() {
        let host = TestHost::new("cc-activity", Vec::new(), false);
        fake_claude(&host, A);
        let chats = Chats::new(host.clone());
        open(&chats, &host, A, page().0, 0).unwrap();
        until("ready", || chats.with_resident(A, |r| r.ready).unwrap()).await;
        chats.message(A, ClientMessage::Send { text: "hi".into() }).unwrap();
        until("its session saved", || host.sessions.lock().unwrap().contains_key(A)).await;
        until("idle", || chats.with_resident(A, |r| r.shared().idle()).unwrap()).await;
        let mesh = chats.with_resident(A, |r| r.keep.mesh.clone()).flatten().expect("on the mesh");
        let table = marv_session::live::collect(&mesh.peer_dir().unwrap(), 8).await.unwrap();
        let row = table.rows.iter().find(|r| r.chat.as_deref() == Some(A)).expect("its row");
        assert_eq!(row.live.core.as_deref(), Some("Claude Code"));
        // The stand-in's turn cost $0.01.
        assert_eq!(row.live.spend_usd_micros, Some(10_000));
        assert!(row.live.metrics.as_ref().and_then(|m| m.usd_micros_per_hour).unwrap_or(0) > 0);
        let bar = table.bar.models.iter().find(|m| m.model == "Claude Code").expect("its model's line in the bar");
        assert_eq!(bar.agents, 1);
        assert_eq!(table.bar.spend_usd_micros, 10_000);
    }

    /// Review P2-1, P2-2, P2-3: a due timer for a paused chat stays due in the file and is queued at most once, however
    /// many looks pass; it's marked fired only when its turn starts; and a chat's own timer set under one grant is
    /// void under another (checked again just before it runs).
    #[tokio::test]
    async fn a_timer_waits_due_while_paused_and_runs_only_under_its_own_grant() {
        let host = TestHost::new("held", vec![say("Hi."), say("Ran.")], false);
        let grant = Live::new(Some(1));
        *host.grant.lock().unwrap() = Some(grant.clone());
        let chats = Chats::new(host.clone());
        let kept = Arc::new(Kept::default());
        chats.set_events(kept.clone());
        let (sink, got) = page();
        open(&chats, &host, A, sink, 0).unwrap();
        until("ready", || chats.with_resident(A, |r| r.ready).unwrap()).await;
        let store = TimerStore::new(&host.dir.path().join("c")).unwrap();
        chats.set_timers(store.clone());
        let chat = AgentId::new(A).unwrap();
        chats.signal(A, SignalKind::Pause).unwrap();
        let every = marv_kernel::Repeat::Every { every_ms: 15 * 60_000 };
        let (made, _) = store
            .add_person(marv_kernel::PersonTimer { chat: chat.clone(), repeat: every, at_ms: 1_000, note: "check CI".into() }, 1)
            .await
            .unwrap();
        for n in 0..5u64 {
            chats.tick(1_000 + n * 15 * 60_000).await;
        }
        assert_eq!(chats.with_resident(A, |r| r.queue.len()).unwrap(), 0, "nothing queued while paused");
        assert_eq!(store.load().unwrap().timers[0].at_ms, made.at_ms, "still due in the file");
        // Resumed: one fire for all the missed ones, and the file moves on only once its turn starts.
        chats.signal(A, SignalKind::Resume).unwrap();
        for _ in 0..3 {
            chats.tick(1_000 + 5 * 15 * 60_000).await;
        }
        until("the timer's turn", || host.model.requests.lock().unwrap().len() == 1).await;
        until("its end", || count(&got, "turn_end") == 1).await;
        assert!(store.load().unwrap().timers[0].at_ms > wall_ms(), "moved past now, once");
        assert_eq!(kept.0.lock().unwrap().iter().filter(|e| matches!(e, KernelEvent::TimerFired { .. })).count(), 1);
        store.remove(made.id).await.unwrap();

        // A chat's timer from grant 1, with grant 2 the live one now: void at fire, and at its start too.
        let mine = store.add_self(&chat, marv_kernel::GrantId(1), 0, 60_000, "check the run", 1).await.unwrap();
        grant.set(Some(2));
        chats.tick(mine.at_ms).await;
        assert!(store.load().unwrap().timers.is_empty(), "void under another grant");
        chats.with_resident(A, |r| {
            r.queue.push(InterruptSource::Timer, Queued::Timer(timer(TimerBy::Chat, "queued under grant 1")));
            r.kick.notify_one();
        });
        until("void at its start", || {
            kept.0
                .lock()
                .unwrap()
                .iter()
                .filter(|e| matches!(e, KernelEvent::TimerVoid { reason: TimerVoidReason::GrantEnded, .. }))
                .count()
                == 2
        })
        .await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(host.model.requests.lock().unwrap().len(), 1, "it never ran");
    }

    /// Review P3-10 (§15.2): a granted unattended turn is stopped by the kernel's tick when its grant is revoked, and
    /// when it expires; each is a preempt in the log. A person's own turn in the same chat never is.
    #[tokio::test]
    async fn the_tick_stops_an_unattended_turn_whose_grant_was_revoked_or_expired() {
        for expire in [false, true] {
            let net = || {
                marv_guards::use_tools(vec![marv_guards::tool_call(
                    "n1",
                    "bash",
                    json!({"command": "curl https://example.invalid", "network": true}),
                )])
            };
            let host = TestHost::new("revoke", vec![net(), net()], false);
            let grant = Live::new(Some(1));
            *host.grant.lock().unwrap() = Some(grant.clone());
            let chats = Chats::new(host.clone());
            let kept = Arc::new(Kept::default());
            chats.set_events(kept.clone());
            let (sink, got) = page();
            open(&chats, &host, A, sink, 0).unwrap();
            until("ready", || chats.with_resident(A, |r| r.ready).unwrap()).await;
            chats.with_resident(A, |r| {
                r.queue.push(InterruptSource::Timer, Queued::Timer(timer(TimerBy::Person, "check")));
                r.kick.notify_one();
            });
            until("its approval", || count(&got, "approval") == 1).await;
            if expire {
                grant.expire();
            } else {
                grant.set(None);
            }
            chats.tick(wall_ms()).await;
            until("its end", || count(&got, "turn_end") == 1).await;
            let kind = if expire { InterruptKind::GrantExpired } else { InterruptKind::GrantRevoked };
            assert!(kept.0.lock().unwrap().iter().any(|e| matches!(
                e,
                KernelEvent::Interrupt { kind: k, action: marv_kernel::InterruptAction::Preempted, .. } if *k == kind
            )));
            // The person's own turn in the chat: the tick leaves it alone.
            chats.message(A, ClientMessage::Send { text: "run it".into() }).unwrap();
            until("the person's approval", || count(&got, "approval") == 2).await;
            chats.tick(wall_ms()).await;
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert_eq!(count(&got, "turn_end"), 1, "the person's turn runs on");
            chats.signal(A, SignalKind::Stop).unwrap();
        }
    }

    /// Review P3-7: the person sending into an unattended turn makes it theirs: its deadline doesn't stop or pause it.
    #[tokio::test]
    async fn sending_now_into_an_unattended_turn_makes_it_the_persons() {
        let net = marv_guards::use_tools(vec![marv_guards::tool_call(
            "n1",
            "bash",
            json!({"command": "curl https://example.invalid", "network": true}),
        )]);
        let host = TestHost::new("attended", vec![net, say("OK.")], false);
        *host.grant.lock().unwrap() = Some(Live::new(Some(1)));
        let chats = Chats::new(host.clone());
        chats.set_deadline(300);
        let (sink, got) = page();
        open(&chats, &host, A, sink, 0).unwrap();
        until("ready", || chats.with_resident(A, |r| r.ready).unwrap()).await;
        chats.with_resident(A, |r| {
            r.queue.push(InterruptSource::Timer, Queued::Timer(timer(TimerBy::Person, "check")));
            r.kick.notify_one();
        });
        until("its approval", || count(&got, "approval") == 1).await;
        chats.message(A, ClientMessage::SendNow { text: "I'm here".into() }).unwrap();
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert_eq!(count(&got, "paused"), 0, "the person's turn now");
        assert!(!chats.with_resident(A, |r| r.paused).unwrap());
        let id = got.lock().unwrap().iter().find(|e| e["type"] == "approval").unwrap()["id"].as_u64().unwrap();
        chats.message(A, ClientMessage::Approval { id, decision: marv_tools::Decision::No }).unwrap();
        until("its end", || count(&got, "turn_end") == 1).await;
    }
}
