//! The switchboard: one per Marv process, on the host, outside every sandbox.
//! New in marv-rs (docs/MESH.md).
//!
//! It owns the wiring table, hands out plugs, and routes every message. It is
//! the only part of the mesh that touches other processes or the network.
//! Every message goes through [`Switchboard::admit`], which applies the rules
//! in order and logs the outcome to the trajectory either way:
//!
//! 1. the receptacle exists and supports the mode;
//! 2. a wire allows it ([`Switchboard::authorize`], the one check: default deny, by name only);
//! 3. a message that must be text (a `send`, a Herdr pane's `.send`) is a JSON string or `{"text": "…"}` (#123);
//! 4. the message fits the wire's size limit (refused, never cut);
//! 5. the wire's rate limit has a token.
//!
//! The sender is the plug whose socket the message arrived on: identity comes
//! from the connection, never from the message.
//!
//! Serving: an MCP server (`marv_mcp::jsonrpc`) on each plug's socket. Each
//! wired receptacle is a tool in `tools/list`; `tools/call` routes; `watch`
//! wires get notifications (routing them comes last). v2 also listens on
//! `<runtime>/mesh/<session>.sock` ([`crate::discovery`]).
//!
//! Receptacles other than inboxes (v2: another Marv's inbox, a Herdr pane) are served by a [`Service`] offered with
//! them ([`Switchboard::offer_service`]). Ones that appear while Marv runs (a Marv started later, a new Herdr pane)
//! are found on demand by a [`Resolve`] ([`Switchboard::lookup`]). A wire to a service on another Marv is granted
//! with [`Switchboard::grant`], which asks the other end's user too (both ends approve, `Grant::remote`).
//!
//! Inboxes (v1): a queue per agent, at most `DEFAULT_INBOX_DEPTH` unread (the
//! oldest are kept, new ones refused, so a looping sender can't push out what
//! the reader hasn't seen). The agent's next step reads them with
//! [`Switchboard::drain_inbox`]; nothing is pushed into a running reply. A delivery rings [`Switchboard::mail`], so an
//! idle TUI with `mesh.wake` on can start a turn to read it (#78; the turn still drains it the same way).
//!
//! Shared, not locked by its users: the switchboard lives in an `Arc` and every method takes `&self`; `serve` and
//! `route` take `Arc<Self>` because they outlive the call (the socket loop, a `call` waiting for its answer). Its
//! tables sit behind one short synchronous lock, never held across an await, so serving never blocks wiring,
//! `mesh_list` or the next message.

use marv_fs::local_socket::StdListener as StdUnixListener;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use marv_fs::local_socket::{Listener as UnixListener, Stream as UnixStream};
use marv_mcp::jsonrpc::{Message, Request, Response, code, decode_line, encode_line, method};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

use crate::MeshError;
use crate::approval::WireRequest;
use crate::limits::{DEFAULT_INBOX_DEPTH, RateLimiter};
use crate::message::{Envelope, bad_text_body, text_body};
use crate::plug::{MODE_META, MODES_META, Plug, PlugId, Reachable};
use crate::receptacle::{Receptacle, ReceptacleKind, ReceptacleName};
use crate::wire::{Approver, Wire, WireMode, WireTable};

/// Receptacles on another Marv on this machine: `peer.<session>.<its receptacle>` ([`crate::discovery`]).
pub const PEER_PREFIX: &str = "peer.";

/// One JSON-RPC line, at most. Far above any wire's message limit; a client that sends more without a newline is
/// cut off rather than buffered without end.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

/// What happened to one message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    Delivered,
    Refused { reason: String },
}

/// One trajectory record (`type: "mesh"`): sender, receptacle, size, never the body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MeshRecord {
    pub at_ms: u64,
    pub id: Option<String>,
    pub sender: String,
    pub receptacle: String,
    pub mode: WireMode,
    pub size: usize,
    #[serde(flatten)]
    pub outcome: Outcome,
}

/// One unread inbox message as a wake decides on it ([`Switchboard::unread_messages`]): who, where, its hop and its
/// id. No field a body could go into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadMessage {
    pub id: String,
    pub sender: PlugId,
    pub receptacle: ReceptacleName,
    pub hop: Option<u32>,
    /// Its sending turn had used up its rounds (runtime-kernel live UX, [`crate::Envelope::rounds_out`]).
    pub rounds_out: bool,
    /// It's a woken turn's automatic report ([`crate::Envelope::report`], stamped, never read from text): it never wakes its reader.
    pub report: bool,
    /// The report answers a task ([`crate::Envelope::task`]).
    pub task: bool,
}

/// Where the switchboard logs every message (`marv-session`'s trajectory writer).
pub trait TrajectorySink: Send + Sync {
    fn mesh(&self, record: MeshRecord);
    /// A radio event (#124): joins, leaves, `lost`, `silent`, drops and refused frames.
    fn radio(&self, _record: crate::radio::RadioRecord) {}
    /// A radio message sent or received, delivered or refused (#124, phase 2): never its text.
    fn radio_frame(&self, _record: crate::radio::RadioFrameRecord) {}
}

/// A sink that keeps nothing (tests, and runs without trajectories).
pub struct NoTrajectory;
impl TrajectorySink for NoTrajectory {
    fn mesh(&self, _record: MeshRecord) {}
}

/// What serves a receptacle that isn't an inbox: another Marv's switchboard, a Herdr pane, a remote service. It runs
/// on the host, in Marv, never in a sandbox, and only gets messages [`Switchboard::admit`] let through.
#[async_trait]
pub trait Service: Send + Sync {
    /// Delivers one admitted message; a `call` returns its answer.
    async fn deliver(&self, envelope: &Envelope) -> crate::Result<Option<Value>>;

    /// For a receptacle on another Marv: asks that Marv's user to approve `from`'s wire, once this end's user did
    /// (`local`), and returns who approved there. `None` (the default): the receptacle is this process's own, and
    /// this end's approval is the only one.
    async fn approve_remote(&self, _from: &PlugId, _mode: WireMode, _local: Approver) -> Option<crate::Result<Approver>> {
        None
    }
}

/// Finds receptacles that appear while Marv runs (another Marv started later, a new Herdr pane) when something asks
/// for one by name.
#[async_trait]
pub trait Resolve: Send + Sync {
    async fn resolve(&self, name: &ReceptacleName) -> Option<(Receptacle, Arc<dyn Service>)>;
}

/// A session's live numbers, asked on demand ([`Switchboard::set_live`]).
pub type LiveSource = Arc<dyn Fn() -> Option<marv_kernel::metrics::Live> + Send + Sync>;

/// Told of each report-back ([`crate::message::is_report`]) that goes into the main agent's inbox, with its stamped
/// sender (never the body's claim): Marv desktop's Team view shows a member's latest ([`Switchboard::set_report_sink`]).
/// For the person's eyes; nothing reaches a model through it.
pub type ReportSink = Arc<dyn Fn(&PlugId, &str) + Send + Sync>;

pub struct Switchboard {
    /// This Marv's session id: message ids and the v2 discovery socket name.
    pub session: String,
    board: Mutex<Board>,
    /// Swapped by the session when its conversation (and so its trajectory file) changes.
    trajectory: Mutex<Arc<dyn TrajectorySink>>,
    /// The private 0700 folder holding plug sockets.
    runtime_dir: PathBuf,
    /// Whether this switchboard made `runtime_dir` for itself ([`Self::owning_folder`]): removed (when empty) on drop.
    owns_folder: bool,
    /// A plug was added while `serve` runs: it has a new socket to listen on.
    wake: tokio::sync::Notify,
    /// A message went into an inbox (#78): what an idle TUI waits on to start a turn (`mesh.wake`). One permit is
    /// kept when nobody waits, so a message that lands between two waits isn't missed.
    mail: tokio::sync::Notify,
    /// Asked, in order, for a receptacle nobody offered yet ([`Self::lookup`]).
    resolvers: Mutex<Vec<Arc<dyn Resolve>>>,
    /// The folder of the machine's switchboards, once this one's door is open (`discovery::open_door`): where
    /// `mesh_list` finds the other Marvs.
    peer_dir: Mutex<Option<PathBuf>>,
    /// The Herdr adapter, when Marv runs in a Herdr pane ([`Self::set_herdr`]): where `mesh_list` finds the agent
    /// panes it could be wired to.
    herdr: Mutex<Option<Arc<crate::herdr::HerdrAdapter>>>,
    /// The radio (#124), when this Marv has one ([`crate::radio::Radio::new`]): the door answers joins with it while
    /// it's on, and `mesh_list` lists who's on it.
    radio: Mutex<Option<Arc<crate::radio::Radio>>>,
    /// The person's rules (runtime-kernel 2b), when this Marv honors them ([`Self::set_rules`]): a desktop chat or the
    /// TUI. None (`marv -p`, tests, an SDK client that didn't ask): no rule ever wires anything here.
    rules: Mutex<Option<Arc<dyn marv_kernel::Rules>>>,
    /// The Marvs `mesh_list` shows (runtime-kernel 3): its own by default, a desktop's shared one for its chats
    /// ([`Self::set_registry`]). Seeing grants nothing: [`Self::authorize`] never reads it.
    registry: Mutex<Arc<crate::registry::MeshRegistry>>,
    /// The kernel's event log (runtime-kernel 3), where this Marv logs (a desktop chat, the TUI; never `marv -p`):
    /// every message admitted or refused (never its body), a rule's wire issued or ended, a wake.
    events: Mutex<Option<Arc<dyn marv_kernel::EventLog>>>,
    /// This Marv's chat card (runtime-kernel chat card, [`crate::card`]): set by its session at each turn's end from
    /// the person's typed messages, or by the desktop from a sleeping chat's saved session. Its door's `marv/info` and
    /// the registry read it. Cleaned when set.
    card: Mutex<Option<marv_kernel::Card>>,
    /// `mesh.cards` (runtime-kernel chat card): whether this Marv publishes its card and shows its model other
    /// Marvs' cards' words. Off until the person's Marv sets it from their config ([`Self::set_cards`]): a switchboard
    /// nobody configured (`marv -p`, an SDK client) neither shares nor shows any.
    cards: AtomicBool,
    /// This Marv's scheduler state as its door says it (a desktop chat's: sleeping, idle, busy, ...); None for a TUI.
    state: Mutex<Option<marv_kernel::AgentState>>,
    /// Its chat's role and `mesh.wake`, as its door says them (runtime-kernel coordination, [`PeerInfo::role`]).
    role: Mutex<Option<String>>,
    wake_word: Mutex<Option<crate::discovery::PeerWake>>,
    /// Until when its team's wakes are used up ([`PeerInfo::team_paused_until`]); set by the session's wake check.
    team_pause: Mutex<Option<u64>>,
    /// Runtime-kernel §20.2 (O2): what this Marv's session is doing (core, context, metrics, processes), asked when
    /// its door answers `marv/info`. Set by the session; none once it's gone (a sleeping chat).
    live: Mutex<Option<LiveSource>>,
    /// The person paused this chat (§15.3), as its door says it.
    paused: AtomicBool,
    /// Where this Marv's chat is read from for another Marv's `chat_read` (runtime-kernel §9.2,
    /// [`crate::chat_read`]): its session's host. None: its door refuses every read.
    chat_source: Mutex<Option<Arc<dyn crate::chat_read::ChatSource>>>,
    /// `chat_read`'s rate per reader (runtime-kernel §9.2): this Marv's own reads under `""`, each other Marv's reads
    /// of this one under its name. Each with when it was last used and when its refusal was last logged; pruned.
    reads: Mutex<HashMap<String, ReadBucket>>,
    /// How long a new wire to another Marv waits for that Marv's person (#201): [`crate::discovery::WIRE_ANSWER_WAIT`],
    /// shorter in tests ([`Self::set_wire_wait`]).
    wire_wait: Mutex<std::time::Duration>,
    /// This Marv's observed write set (runtime-kernel §14.4, [`marv_kernel::writes`]): each path its edit tools wrote
    /// in the last 30 minutes. Its door's `marv/info` and the registry read it; shown, never consulted by a check.
    writes: Mutex<marv_kernel::WriteSet>,
    /// The person's team boards (runtime-kernel §19), set with the rules where this Marv honors them
    /// ([`Self::set_boards`]). None: every board tool says no rule covers this chat.
    boards: Mutex<Option<Arc<marv_kernel::BoardStore>>>,
    /// Where this Marv's session works (runtime-kernel §14.1/§14.4), set by the session: a desktop worktree chat's
    /// worktree, which its door says as `work_dir` when it isn't its folder.
    work_dir: Mutex<Option<PathBuf>>,
    /// The person marked this chat private (runtime-kernel §9.2 R3): its door publishes no write set.
    private: AtomicBool,
    /// [`ReportSink`], when the host wants reports (Marv desktop).
    report_sink: Mutex<Option<ReportSink>>,
}

/// One reader's `chat_read` rate (runtime-kernel §9.2).
struct ReadBucket {
    limiter: RateLimiter,
    used_ms: u64,
    logged_ms: Option<u64>,
}

/// How long an unused read bucket is kept (it is full again well before), and how often a reader's rate refusal is
/// logged.
const READ_BUCKET_KEPT_MS: u64 = 60_000;

/// What changes: one short lock, never held across an await.
#[derive(Default)]
struct Board {
    table: WireTable,
    receptacles: HashMap<ReceptacleName, Receptacle>,
    plugs: HashMap<PlugId, Plug>,
    limiters: HashMap<(PlugId, ReceptacleName, WireMode), RateLimiter>,
    seq: u64,
    /// Unread messages per agent, oldest first.
    inboxes: HashMap<PlugId, VecDeque<Envelope>>,
    /// Unread radio messages (#124), oldest first: the main agent's alone, drained at the inbox's points.
    radio_inbox: VecDeque<crate::radio::RadioMessage>,
    /// Sockets bound by `plug` that `serve` hasn't picked up yet.
    unserved: Vec<(PlugId, StdUnixListener)>,
    /// Stops a plug's listener and its connections when it's unplugged.
    stops: HashMap<PlugId, CancellationToken>,
    /// The last `worker-N` handed out ([`Switchboard::plug_next`]); never reused within a process.
    workers: u64,
    /// What serves each receptacle that isn't an inbox.
    services: HashMap<ReceptacleName, Arc<dyn Service>>,
    /// Who each plug is, as a rule would see it (runtime-kernel 2b): the main agent's from its own session, a peer's
    /// from its pid-confirmed `marv/info`. Kept for the plug's life. Data only: nothing is admitted because of it.
    identities: HashMap<PlugId, marv_kernel::AgentRef>,
    /// The hop of the turn each of Marv's own plugs speaks for ([`crate::hop`]), registered by the session.
    hops: HashMap<PlugId, Arc<crate::hop::TurnHop>>,
    /// The plugs this Marv's door made for pid-confirmed peer Marvs (`peer-<session>`): the only ones whose
    /// `tools/call` may carry a hop.
    peers: std::collections::HashSet<PlugId>,
    /// The unread messages already logged as a skipped wake (runtime-kernel live UX): kept with the inbox, so a session
    /// reloaded on the same switchboard (a desktop chat waking from sleep) doesn't log them again.
    skip_logged: std::collections::HashSet<String>,
}

/// A refusal's category for the kernel's event log (runtime-kernel live UX): a fixed word, never the error's text.
fn refusal(err: &MeshError) -> marv_kernel::Refusal {
    match err {
        MeshError::NotWired { .. } | MeshError::SelfWire(_) => marv_kernel::Refusal::NotWired,
        MeshError::RuleEnded { .. } => marv_kernel::Refusal::RuleEnded,
        MeshError::TooLarge { .. } | MeshError::RateLimited { .. } => marv_kernel::Refusal::Limit,
        MeshError::InboxFull(_) => marv_kernel::Refusal::InboxFull,
        MeshError::NoReceptacle(_) | MeshError::NotServed { .. } => marv_kernel::Refusal::NoReceptacle,
        _ => marv_kernel::Refusal::Other,
    }
}

/// What a `tools/call` claimed about its message in `_meta` (a peer Marv's link forwards both): read from a peer plug
/// only ([`Board::hop_of`]).
#[derive(Debug, Clone, Copy, Default)]
struct Claim {
    hop: Option<u32>,
    rounds_out: bool,
    report: bool,
    task: bool,
    clean: bool,
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn io(context: &str, err: std::io::Error) -> MeshError {
    MeshError::Io(format!("{context}: {err}"))
}

impl Switchboard {
    pub fn new(session: impl Into<String>, runtime_dir: PathBuf, trajectory: Arc<dyn TrajectorySink>) -> Self {
        Self {
            session: session.into(),
            board: Mutex::default(),
            trajectory: Mutex::new(trajectory),
            runtime_dir,
            owns_folder: false,
            wake: tokio::sync::Notify::new(),
            mail: tokio::sync::Notify::new(),
            resolvers: Mutex::default(),
            peer_dir: Mutex::default(),
            herdr: Mutex::default(),
            radio: Mutex::default(),
            rules: Mutex::default(),
            registry: Mutex::new(crate::registry::MeshRegistry::new()),
            events: Mutex::default(),
            card: Mutex::default(),
            cards: AtomicBool::new(false),
            state: Mutex::default(),
            role: Mutex::default(),
            wake_word: Mutex::default(),
            team_pause: Mutex::default(),
            live: Mutex::default(),
            paused: AtomicBool::new(false),
            chat_source: Mutex::default(),
            reads: Mutex::default(),
            wire_wait: Mutex::new(crate::discovery::WIRE_ANSWER_WAIT),
            writes: Mutex::default(),
            boards: Mutex::default(),
            work_dir: Mutex::default(),
            private: AtomicBool::new(false),
            report_sink: Mutex::default(),
        }
    }

    /// The host's [`ReportSink`] from now on.
    pub fn set_report_sink(&self, sink: ReportSink) {
        *self.report_sink.lock().unwrap_or_else(|e| e.into_inner()) = Some(sink);
    }

    /// Runtime-kernel §14.4: this Marv's edit tools wrote `path` (absolute, its real path) just now. Its registry entry
    /// takes it and an overlap with another live chat's writes is logged; where the other Marvs' doors are known, a
    /// stale scan of them is redone in the background first, so a terminal Marv sees the others' latest writes too.
    pub fn note_write(self: &Arc<Self>, path: &Path) {
        let now = now_ms();
        self.writes.lock().unwrap_or_else(|e| e.into_inner()).record(path, now);
        self.registry().noted_write(self, self.peer_dir());
    }

    /// The paths this Marv wrote in the last 30 minutes, newest first (its own view: the desktop's overlaps).
    pub fn recent_writes(&self) -> Vec<marv_kernel::RecentWrite> {
        self.writes.lock().unwrap_or_else(|e| e.into_inner()).recent(now_ms())
    }

    /// The write set as its door says it to other Marvs (runtime-kernel §14.4): only with `mesh.cards` on, never for a
    /// private chat.
    pub fn published_writes(&self) -> Vec<marv_kernel::RecentWrite> {
        if !self.cards_on() || self.private.load(Ordering::SeqCst) {
            return Vec::new();
        }
        self.recent_writes()
    }

    /// Where this Marv's session works from now on (its root).
    pub fn set_work_dir(&self, dir: PathBuf) {
        *self.work_dir.lock().unwrap_or_else(|e| e.into_inner()) = Some(dir);
    }

    /// The work folder as the door says it: none when it is the door's folder `cwd`.
    pub fn published_work_dir(&self, cwd: &Path) -> Option<String> {
        let dir = self.work_dir.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
        (dir != cwd).then(|| dir.display().to_string())
    }

    /// The person marked this chat private, or not (its door then publishes no write set).
    pub fn set_private(&self, private: bool) {
        self.private.store(private, Ordering::SeqCst);
    }

    /// Where this Marv's chat is read from for `chat_read` (runtime-kernel §9.2): set by its session's host (the TUI,
    /// a desktop chat), never for `marv -p`.
    pub fn set_chat_source(&self, source: Arc<dyn crate::chat_read::ChatSource>) {
        *self.chat_source.lock().unwrap_or_else(|e| e.into_inner()) = Some(source);
    }

    pub fn chat_source(&self) -> Option<Arc<dyn crate::chat_read::ChatSource>> {
        self.chat_source.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Takes one read's token for `reader` (`""`: this Marv's own reads), taken before anything else is checked, so
    /// refused reads count too ([`crate::chat_read::read_limits`]). `Err(log)` when it read as often as it may:
    /// `log` is true once a minute per reader, so a reader past its rate can't flood the event log either. Buckets
    /// unused for a minute (full again by then) are dropped.
    pub(crate) fn take_read(&self, reader: &str) -> Result<(), bool> {
        let now = now_ms();
        let mut reads = self.reads.lock().unwrap_or_else(|e| e.into_inner());
        reads.retain(|_, b| now.saturating_sub(b.used_ms) < READ_BUCKET_KEPT_MS);
        let bucket = reads.entry(reader.to_string()).or_insert_with(|| ReadBucket {
            limiter: RateLimiter::new(crate::chat_read::read_limits(), now),
            used_ms: now,
            logged_ms: None,
        });
        bucket.used_ms = now;
        if bucket.limiter.take(crate::chat_read::CHAT_READ, now).is_ok() {
            return Ok(());
        }
        let log = bucket.logged_ms.is_none_or(|at| now.saturating_sub(at) >= READ_BUCKET_KEPT_MS);
        if log {
            bucket.logged_ms = Some(now);
        }
        Err(log)
    }

    /// Whether a live `read` rule the person made covers `reader` reading `target`'s chat (runtime-kernel §9.2,
    /// `chat_read`): only where this Marv honors rules. The rules' one other reader here besides [`Self::rule_grant`]
    /// and [`Self::authorize`]; it wires nothing.
    pub(crate) fn read_rule_covers(&self, reader: &marv_kernel::AgentRef, target: &marv_kernel::AgentRef) -> bool {
        self.read_rule_label(reader, target).is_some()
    }

    /// The label of the read rule that covers `reader` reading `target` (what the person called it), if one does.
    pub(crate) fn read_rule_label(&self, reader: &marv_kernel::AgentRef, target: &marv_kernel::AgentRef) -> Option<String> {
        let rules = self.rules_now()?;
        let access = marv_kernel::Access { from: reader, to: target, mode: marv_kernel::Mode::Read, now_ms: now_ms() };
        marv_kernel::match_rule(&rules, &access).ok().map(|m| m.rule.label.clone())
    }

    /// Logs a kernel event as it is (runtime-kernel 3), where this Marv logs.
    pub(crate) fn log_kernel(&self, event: marv_kernel::KernelEvent) {
        self.event(now_ms(), |_| event);
    }

    /// This Marv's chat card from now on (runtime-kernel chat card): cut and cleaned here whoever built it.
    pub fn set_card(&self, card: marv_kernel::Card) {
        *self.card.lock().unwrap_or_else(|e| e.into_inner()) = Some(crate::card::received(card));
    }

    /// Sets the card only when there's none yet (a sleeping chat's, read from its saved session, never over the card
    /// a turn that ended meanwhile set).
    pub fn set_card_if_none(&self, card: marv_kernel::Card) {
        let mut slot = self.card.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Some(crate::card::received(card));
        }
    }

    /// `mesh.cards` from the person's config (on unless they turned it off): publish this Marv's card, and show its
    /// model other Marvs' cards' words.
    pub fn set_cards(&self, on: bool) {
        self.cards.store(on, Ordering::SeqCst);
    }

    /// Whether `mesh.cards` is on here ([`Self::set_cards`]).
    pub fn cards_on(&self) -> bool {
        self.cards.load(Ordering::SeqCst)
    }

    /// The card as published: none while `mesh.cards` is off.
    pub fn card(&self) -> Option<marv_kernel::Card> {
        if !self.cards_on() {
            return None;
        }
        self.card.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// This Marv's scheduler state, as its door says it (the desktop's registry sets it with the entry's).
    pub fn set_state(&self, state: Option<marv_kernel::AgentState>) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = state;
    }

    pub fn state(&self) -> Option<marv_kernel::AgentState> {
        *self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// This Marv's chat's role (runtime-kernel coordination), as its door says it: fixed for the chat's life. Only a
    /// plain role name is kept.
    pub fn set_role(&self, role: Option<String>) {
        *self.role.lock().unwrap_or_else(|e| e.into_inner()) = role.filter(|r| crate::discovery::valid_role_name(r));
    }

    pub fn role(&self) -> Option<String> {
        self.role.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// This Marv's `mesh.wake`, as its door says it.
    pub fn set_wake(&self, wake: Option<crate::discovery::PeerWake>) {
        *self.wake_word.lock().unwrap_or_else(|e| e.into_inner()) = wake;
    }

    pub fn wake(&self) -> Option<crate::discovery::PeerWake> {
        *self.wake_word.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Publishes until when (epoch ms) a team wire's message won't wake this Marv, its team wake budget being used
    /// up (none: it would). Told by the session's wake check; read from its door by a sender, to say who must act.
    pub fn set_team_pause(&self, until: Option<u64>) {
        *self.team_pause.lock().unwrap_or_else(|e| e.into_inner()) = until;
    }

    pub fn team_pause(&self) -> Option<u64> {
        *self.team_pause.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The label of the rule that issued the wire `sender` -> `receptacle` (what the person called it), when a rule did
    /// and it's still live: how the session tells a team's wire (`Team <name>`) from any other. It grants nothing.
    pub fn wire_rule_label(&self, sender: &PlugId, receptacle: &ReceptacleName) -> Option<String> {
        self.wire_rule(sender, receptacle).map(|(_, label)| label)
    }

    /// The id and label of the rule that issued the wire `sender` -> `receptacle` ([`Self::wire_rule_label`]).
    pub fn wire_rule(&self, sender: &PlugId, receptacle: &ReceptacleName) -> Option<(marv_kernel::PolicyId, String)> {
        let wire = self.authorize(sender, receptacle, WireMode::Send).ok()?;
        let Approver::Policy(grant) = wire.grant.local else { return None };
        let rules = self.rules_now()?;
        rules.rules.iter().find(|r| r.id == grant.id()).map(|r| (r.id, r.label.clone()))
    }

    /// The door's `marv/info` of the Marv whose main inbox is `receptacle`, when it answers as itself (as
    /// [`Self::rule_for`] asks it). Never a grant: a sender's note of what its message will do.
    pub async fn door_info(&self, receptacle: &ReceptacleName) -> Option<crate::discovery::PeerInfo> {
        let (session, remote) = crate::discovery::parse_peer(receptacle)?;
        remote.inbox_owner()?;
        let socket = crate::discovery::session_socket(&self.peer_dir()?, &session)?;
        let ask = async { crate::discovery::DoorClient::connect(&socket).await?.info().await };
        let info = tokio::time::timeout(crate::discovery::DOOR_TIMEOUT, ask).await.ok()?.ok()?;
        (info.name == session).then_some(info)
    }

    /// Where this Marv's live numbers come from (runtime-kernel §20.2): its session sets it.
    pub fn set_live(&self, source: LiveSource) {
        *self.live.lock().unwrap_or_else(|e| e.into_inner()) = Some(source);
    }

    /// The person paused (or resumed) this chat: its door says so.
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
    }

    /// What its door says in `marv/info`'s `live`: the session's numbers (none when no session is loaded), with the
    /// pause. For the person's views only: never `mesh_list`, never a model.
    pub fn live(&self) -> Option<marv_kernel::metrics::Live> {
        let source = self.live.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let paused = self.paused.load(Ordering::SeqCst);
        let live = source.and_then(|f| f());
        match live {
            Some(mut live) => {
                live.paused = paused;
                Some(live)
            }
            None if paused => Some(marv_kernel::metrics::Live { paused, ..Default::default() }),
            None => None,
        }
    }

    /// Lists the Marvs from `registry` from now on (runtime-kernel 3): a desktop gives all its chats one, which it
    /// fills with them itself. Seeing grants nothing: no check reads it.
    pub fn set_registry(&self, registry: Arc<crate::registry::MeshRegistry>) {
        *self.registry.lock().unwrap_or_else(|e| e.into_inner()) = registry;
    }

    /// The Marvs `mesh_list` shows.
    pub fn registry(&self) -> Arc<crate::registry::MeshRegistry> {
        self.registry.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Logs kernel events to `log` from now on (runtime-kernel 3): where the person's own Marv runs (a desktop chat,
    /// the TUI), never `marv -p`.
    pub fn set_events(&self, log: Arc<dyn marv_kernel::EventLog>) {
        *self.events.lock().unwrap_or_else(|e| e.into_inner()) = Some(log);
    }

    pub(crate) fn event(&self, at_ms: u64, event: impl FnOnce(marv_kernel::AgentKey) -> marv_kernel::KernelEvent) {
        let log = self.events.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(log) = log {
            log.append(at_ms, event(self.own_key()));
        }
    }

    /// Who this switchboard is, as the kernel's events name it: its main agent's chat id, or its mesh name.
    pub fn own_key(&self) -> marv_kernel::AgentKey {
        match self.identity(&PlugId::new(crate::PARENT_AGENT).expect("a valid plug id")).and_then(|me| me.id) {
            Some(id) => marv_kernel::AgentKey::Chat(id),
            None => marv_kernel::AgentKey::Mesh(self.session.clone()),
        }
    }

    /// Logs that this Marv's chat booted (runtime-kernel §18.1, B1): `core` (provider and model), the hashes of the
    /// system prompt and tool specs it sends, its device set by name, and whether it resumed a saved conversation. A
    /// desktop chat (it has a chat id) is shell `Chat`, anything else `Terminal`. Never a prompt's or a message's words.
    pub fn log_booted(&self, core: &str, prompt_hash: &str, tools_hash: &str, devices: Vec<String>, resumed: bool) {
        self.event(now_ms(), |agent| {
            let shell = match agent {
                marv_kernel::AgentKey::Chat(_) => marv_kernel::AgentKind::Chat,
                marv_kernel::AgentKey::Mesh(_) => marv_kernel::AgentKind::Terminal,
            };
            marv_kernel::KernelEvent::Booted {
                agent,
                shell,
                core: core.to_string(),
                prompt_hash: prompt_hash.to_string(),
                tools_hash: tools_hash.to_string(),
                devices,
                resumed,
            }
        });
    }

    /// Logs that a message from `sender` at `hop` woke this Marv (runtime-kernel 3; the hop from [`Self::unread_hop`]
    /// before the woken turn read it). Call it when the woken turn has started.
    pub fn log_wake(&self, sender: &PlugId, hop: Option<u32>) {
        self.event(now_ms(), |agent| marv_kernel::KernelEvent::Wake { agent, by: sender.to_string(), hop });
    }

    /// Logs that `count` messages from `sender` (the first at `hop`) came in while this Marv was idle and didn't wake it,
    /// and why (runtime-kernel live UX). Call it once per message, when the person is told they wait. Never a body.
    pub fn log_wake_skipped(&self, sender: &PlugId, hop: Option<u32>, reason: marv_kernel::WakeSkipReason, count: usize) {
        self.event(now_ms(), |agent| marv_kernel::KernelEvent::WakeSkipped { agent, by: sender.to_string(), hop, reason, count });
    }

    /// The hop of the oldest unread message from `sender` on `receptacle` in `agent`'s inbox (for [`Self::log_wake`]).
    pub fn unread_hop(&self, agent: &PlugId, sender: &PlugId, receptacle: &ReceptacleName) -> Option<u32> {
        self.board().inboxes.get(agent)?.iter().find(|e| &e.sender == sender && &e.receptacle == receptacle).and_then(|e| e.hop)
    }

    /// Honors the person's rules from now on (runtime-kernel 2b): a rule they made may wire this Marv's main agent to
    /// another Marv's ([`Self::rule_grant`]), and a wire a rule made admits only while the rule lasts
    /// ([`Self::authorize`]). Set by the session for a desktop chat or the TUI, never for `marv -p`.
    pub fn set_rules(&self, rules: Arc<dyn marv_kernel::Rules>) {
        *self.rules.lock().unwrap_or_else(|e| e.into_inner()) = Some(rules);
    }

    /// The rules as they are now, when this Marv honors any. Read outside the board's lock (it may read the file).
    pub(crate) fn rules_now(&self) -> Option<Arc<marv_kernel::PolicySnapshot>> {
        let rules = self.rules.lock().unwrap_or_else(|e| e.into_inner()).clone();
        rules.map(|r| r.snapshot())
    }

    /// How long a new wire to another Marv waits for that Marv's person, on both ends (#201).
    pub fn wire_wait(&self) -> std::time::Duration {
        *self.wire_wait.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Changes [`Self::wire_wait`] (tests: a short wait instead of minutes). Set it before the door opens.
    pub fn set_wire_wait(&self, wait: std::time::Duration) {
        *self.wire_wait.lock().unwrap_or_else(|e| e.into_inner()) = wait;
    }

    /// The person's team boards from now on (runtime-kernel §19): set with [`Self::set_rules`], never for `marv -p`.
    pub fn set_boards(&self, boards: Arc<marv_kernel::BoardStore>) {
        *self.boards.lock().unwrap_or_else(|e| e.into_inner()) = Some(boards);
    }

    pub fn boards(&self) -> Option<Arc<marv_kernel::BoardStore>> {
        self.boards.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The rules whose team board this Marv's main agent may use (runtime-kernel §19.1): the live rules the person
    /// made that cover it at either end, in any mode, newest first, each with its label and whether this chat could
    /// `mesh_send` under it (a `send` rule, and on its sending side unless it's mutual: the only case a post runs
    /// unasked, D2's shape). Only where this Marv honors rules and knows who its main agent is; none otherwise. It
    /// wires nothing and admits nothing.
    pub fn board_rules(&self) -> Vec<crate::board::BoardRule> {
        let (Some(rules), Some(me)) =
            (self.rules_now(), self.identity(&PlugId::new(crate::PARENT_AGENT).expect("a valid plug id")))
        else {
            return Vec::new();
        };
        if !matches!(me.kind, marv_kernel::AgentKind::Chat | marv_kernel::AgentKind::Terminal) {
            return Vec::new();
        }
        let now = now_ms();
        let mut covering: Vec<&marv_kernel::Policy> = rules
            .rules
            .iter()
            .filter(|r| r.mode != marv_kernel::Mode::Watch && r.live_at(now) && (r.from.covers(&me) || r.to.covers(&me)))
            .collect();
        covering.sort_by_key(|r| std::cmp::Reverse((r.created_at_ms, r.id)));
        covering
            .into_iter()
            .map(|r| crate::board::BoardRule {
                id: r.id,
                label: r.label.clone(),
                sends: r.mode == marv_kernel::Mode::Send && (r.mutual || r.from.covers(&me)),
            })
            .collect()
    }

    /// Marks `runtime_dir` as made for this switchboard alone (`marv_session::mesh::start`'s `p/<pid>-<hex>`): dropped,
    /// the switchboard removes it once its sockets are gone, so empty plug folders don't pile up. Never a folder
    /// someone else gave it.
    pub fn owning_folder(mut self) -> Self {
        self.owns_folder = true;
        self
    }

    /// This Marv's radio (#124; set by [`crate::radio::Radio::new`]).
    pub fn set_radio(&self, radio: Arc<crate::radio::Radio>) {
        *self.radio.lock().unwrap_or_else(|e| e.into_inner()) = Some(radio);
    }
    pub fn radio(&self) -> Option<Arc<crate::radio::Radio>> {
        self.radio.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Logs a radio event to the current trajectory.
    pub fn log_radio(&self, record: crate::radio::RadioRecord) {
        let sink = self.trajectory.lock().unwrap_or_else(|e| e.into_inner()).clone();
        sink.radio(record);
    }

    /// Logs a radio message (never its text) to the current trajectory.
    pub fn log_radio_frame(&self, record: crate::radio::RadioFrameRecord) {
        let sink = self.trajectory.lock().unwrap_or_else(|e| e.into_inner()).clone();
        sink.radio_frame(record);
    }

    /// A radio message for the main agent (#124): queued, unless `depth` wait unread already (the oldest are kept,
    /// the new one refused, as an inbox does). Rings [`Self::mail`].
    pub fn deliver_radio(&self, message: crate::radio::RadioMessage, depth: usize) -> bool {
        {
            let mut board = self.board();
            if board.radio_inbox.len() >= depth {
                return false;
            }
            board.radio_inbox.push_back(message);
        }
        self.mail.notify_one();
        true
    }

    /// The unread radio messages, oldest first, without taking them: what decides a radio wake (#124).
    pub fn unread_radio(&self) -> Vec<crate::radio::RadioMessage> {
        self.board().radio_inbox.iter().cloned().collect()
    }

    /// The unread radio messages for the main agent's next read (oldest first).
    pub fn drain_radio(&self) -> Vec<crate::radio::RadioMessage> {
        self.board().radio_inbox.drain(..).collect()
    }

    /// The unread radio messages but requests, for a read between steps (#124, phase 3): a request is read only at a
    /// turn's start, so "the turn that handles it" is well defined; it waits for the next turn.
    pub fn drain_radio_but_requests(&self) -> Vec<crate::radio::RadioMessage> {
        let mut board = self.board();
        let (requests, rest): (Vec<_>, Vec<_>) = board.radio_inbox.drain(..).partition(|m| m.kind == crate::radio::Kind::Request);
        board.radio_inbox.extend(requests);
        rest
    }

    /// Takes the unread `ack`, `done` and `failed` that answer request `id` (`radio_tx`'s wait, #124).
    pub fn take_radio_replies(&self, id: &str) -> Vec<crate::radio::RadioMessage> {
        let mut board = self.board();
        let (replies, rest): (Vec<_>, Vec<_>) =
            board.radio_inbox.drain(..).partition(|m| m.kind.is_reply() && m.reply_to.as_deref() == Some(id));
        board.radio_inbox.extend(rest);
        replies
    }

    /// Where the other Marvs' doors are (set when this one's door opens).
    pub fn set_peer_dir(&self, dir: PathBuf) {
        if let Some(herdr) = self.herdr() {
            herdr.set_mesh_dir(dir.clone());
        }
        *self.peer_dir.lock().unwrap_or_else(|e| e.into_inner()) = Some(dir);
    }
    pub fn peer_dir(&self) -> Option<PathBuf> {
        self.peer_dir.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Offers Herdr's agent panes: found on demand by name ([`crate::herdr::HerdrPanes`]) and listed by `mesh_list`.
    pub fn set_herdr(&self, adapter: Arc<crate::herdr::HerdrAdapter>) {
        // #124: a pane send looks for a Marv on the radio in the pane, in this switchboard's mesh folder.
        if let Some(dir) = self.peer_dir() {
            adapter.set_mesh_dir(dir);
        }
        self.add_resolver(Arc::new(crate::herdr::HerdrPanes(adapter.clone())));
        *self.herdr.lock().unwrap_or_else(|e| e.into_inner()) = Some(adapter);
    }
    pub fn herdr(&self) -> Option<Arc<crate::herdr::HerdrAdapter>> {
        self.herdr.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn board(&self) -> MutexGuard<'_, Board> {
        self.board.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn log(&self, record: MeshRecord) {
        let sink = self.trajectory.lock().unwrap_or_else(|e| e.into_inner()).clone();
        sink.mesh(record);
    }

    /// Where every message is logged from now on (the session's current trajectory).
    pub fn set_trajectory(&self, trajectory: Arc<dyn TrajectorySink>) {
        *self.trajectory.lock().unwrap_or_else(|e| e.into_inner()) = trajectory;
    }

    /// A snapshot of the wiring table.
    pub fn table(&self) -> WireTable {
        self.board().table.clone()
    }

    /// Offers a receptacle (an agent's inbox, a Herdr pane, a remote service).
    pub fn offer(&self, receptacle: Receptacle) {
        self.board().receptacles.insert(receptacle.name.clone(), receptacle);
    }

    pub fn receptacle(&self, name: &ReceptacleName) -> Option<Receptacle> {
        self.board().receptacles.get(name).cloned()
    }

    /// Offers a receptacle served by `service` (another Marv's inbox, a Herdr pane).
    pub fn offer_service(&self, receptacle: Receptacle, service: Arc<dyn Service>) {
        let mut board = self.board();
        board.services.insert(receptacle.name.clone(), service);
        board.receptacles.insert(receptacle.name.clone(), receptacle);
    }

    /// Asks `resolver` for receptacles nobody offered yet ([`Self::lookup`]).
    pub fn add_resolver(&self, resolver: Arc<dyn Resolve>) {
        self.resolvers.lock().unwrap_or_else(|e| e.into_inner()).push(resolver);
    }

    /// A receptacle by name: one already offered, else the first resolver that finds it (then offered from now on).
    pub async fn lookup(&self, name: &ReceptacleName) -> Option<Receptacle> {
        if let Some(r) = self.receptacle(name) {
            return Some(r);
        }
        let resolvers = self.resolvers.lock().unwrap_or_else(|e| e.into_inner()).clone();
        for resolver in resolvers {
            if let Some((receptacle, service)) = resolver.resolve(name).await {
                self.offer_service(receptacle.clone(), service);
                return Some(receptacle);
            }
        }
        None
    }

    /// The agents plugged in here, by name.
    pub fn agents(&self) -> Vec<PlugId> {
        let mut agents: Vec<PlugId> = self.board().plugs.keys().cloned().collect();
        agents.sort();
        agents
    }

    pub fn has_plug(&self, id: &PlugId) -> bool {
        self.board().plugs.contains_key(id)
    }

    /// Records who `plug` is (runtime-kernel 2b): the main agent's [`marv_kernel::AgentRef`] from its session, a peer
    /// plug's from that Marv's pid-confirmed `marv/info`. Kept until the plug is unplugged. **Data only:**
    /// [`Self::authorize`] doesn't read it, so it never admits or wires anything.
    pub fn set_identity(&self, plug: PlugId, who: marv_kernel::AgentRef) {
        self.board().identities.insert(plug, who);
    }

    /// The hop of the turn `plug` speaks for (runtime-kernel 2b): the session registers its main agent's, and a
    /// subagent's plug shares its parent's. Every message from `plug` is stamped with it ([`Envelope::hop`]); a hop
    /// in the request itself is ignored. Kept until the plug is unplugged.
    pub fn set_turn_hop(&self, plug: PlugId, hop: Arc<crate::hop::TurnHop>) {
        self.board().hops.insert(plug, hop);
    }

    /// The turn hop registered for `plug`, if any.
    pub fn turn_hop(&self, plug: &PlugId) -> Option<Arc<crate::hop::TurnHop>> {
        self.board().hops.get(plug).cloned()
    }

    /// A plug for a pid-confirmed peer Marv (the door's `marv/wire`): [`Self::plug`], and its `tools/call` may carry
    /// the hop its `PeerLink` forwards.
    pub(crate) fn plug_peer(&self, id: PlugId) -> crate::Result<Plug> {
        let plug = self.plug(id.clone())?;
        self.board().peers.insert(id);
        Ok(plug)
    }

    /// Who `plug` is, when known ([`Self::set_identity`]).
    pub fn identity(&self, plug: &PlugId) -> Option<marv_kernel::AgentRef> {
        self.board().identities.get(plug).cloned()
    }

    /// The private runtime folder, made 0700 (it holds every plug's socket).
    fn runtime_folder(&self) -> crate::Result<()> {
        let dir = &self.runtime_dir;
        std::fs::create_dir_all(dir).map_err(|e| io(&format!("can't make {}", dir.display()), e))?;
        let meta = std::fs::symlink_metadata(dir).map_err(|e| io(&dir.display().to_string(), e))?;
        if !meta.is_dir() {
            return Err(MeshError::Io(format!("{} isn't a folder", dir.display())));
        }
        marv_fs::mode::set_mode(dir, 0o700).map_err(|e| io(&dir.display().to_string(), e))
    }

    /// A plug for a new agent and its inbox; its socket (0600) is bound at once, so a sandbox can mount it before
    /// `serve` picks it up. The agent starts with no wires. Plugging an agent that has a plug returns that one.
    pub fn plug(&self, id: PlugId) -> crate::Result<Plug> {
        if let Some(plug) = self.board().plugs.get(&id) {
            return Ok(plug.clone());
        }
        self.runtime_folder()?;
        let host_socket = self.runtime_dir.join(format!("{}.sock", id.as_str()));
        // A socket left by an earlier plug of this name (the folder is this session's own): bind would fail on it.
        let _ = std::fs::remove_file(&host_socket);
        marv_sandbox::runtime::check_socket_path(&host_socket).map_err(MeshError::Io)?;
        let listener = StdUnixListener::bind_private(&host_socket)
            .map_err(|e| io(&format!("can't listen on {}", host_socket.display()), e))?;
        let plug = Plug { host_socket, id: id.clone() };
        {
            let mut board = self.board();
            let inbox = Receptacle::inbox(id.clone());
            board.receptacles.insert(inbox.name.clone(), inbox);
            board.plugs.insert(id.clone(), plug.clone());
            board.inboxes.entry(id.clone()).or_default();
            board.stops.insert(id.clone(), CancellationToken::new());
            board.unserved.push((id, listener));
        }
        self.wake.notify_one();
        Ok(plug)
    }

    /// A plug named `<prefix>-<n>` with the next unused n (`worker-1`, `worker-2`, …): a subagent's.
    pub fn plug_next(&self, prefix: &str) -> crate::Result<Plug> {
        let id = {
            let mut board = self.board();
            loop {
                board.workers += 1;
                let id = PlugId::new(format!("{prefix}-{}", board.workers))?;
                if !board.plugs.contains_key(&id) {
                    break id;
                }
            }
        };
        self.plug(id)
    }

    /// Adds a wire the user approved (see [`crate::approval`]). A wire to another Marv's receptacle (`peer.*`, or
    /// a peer or remote kind) is always cross-process, whoever built it: it counts only once the other end's
    /// approval is in `Grant::remote`, so a one-ended grant can't reach another process.
    pub fn wire(&self, mut wire: Wire) -> crate::Result<()> {
        let mut board = self.board();
        let receptacle = board.receptacles.get(&wire.to);
        let owner = receptacle.and_then(|r| r.owner.clone());
        let elsewhere = receptacle.is_some_and(|r| matches!(r.kind, ReceptacleKind::Peer { .. } | ReceptacleKind::Remote { .. }));
        if elsewhere || wire.to.as_str().starts_with(PEER_PREFIX) {
            wire.cross_process = true;
        }
        let issued = match wire.grant.local {
            Approver::Policy(grant) => Some((grant, wire.from.to_string(), wire.to.to_string())),
            _ => None,
        };
        board.table.add(wire, owner.as_ref())?;
        drop(board);
        // Runtime-kernel 3: a rule issued this wire.
        if let Some((grant, from, to)) = issued {
            self.event(now_ms(), |agent| marv_kernel::KernelEvent::Capability {
                agent,
                issued: true,
                from,
                to,
                rule: grant.id(),
                generation: grant.generation(),
            });
        }
        Ok(())
    }

    /// Grants a wire a person on this end approved (`by`). For a receptacle on another Marv, that Marv's user is asked
    /// as well ([`Service::approve_remote`]), and the wire is recorded only if they approve too.
    pub async fn grant(&self, request: WireRequest, by: Approver) -> crate::Result<()> {
        let service = self.board().services.get(&request.to).cloned();
        let remote = match service {
            Some(service) => service.approve_remote(&request.from, request.mode, by).await.transpose()?,
            None => None,
        };
        let mut wire = request.grant(by);
        if let Some(remote) = remote {
            wire.cross_process = true;
            wire.grant.remote = Some(remote);
        }
        self.wire(wire)
    }

    /// The approval a rule gives `request` (runtime-kernel 2b), if one does: `from` and `to` are both ends'
    /// identities, as this Marv knows them (its own main agent's from the session; another Marv's from its
    /// pid-confirmed or owner-checked `marv/info`). The only place a [`RuleGrant`](crate::wire::RuleGrant) is made.
    ///
    /// Only for what rules may cover (D8): this Marv honors rules ([`Self::set_rules`]), the wire is to a Marv's main
    /// agent's inbox (`agent.parent.inbox`, here or another Marv's), in `send` or `call`, and both ends are desktop
    /// chats or terminal Marvs (`marv_kernel::match_rule` refuses subagents and shim plugs). Anything else, or no
    /// live rule: `None`, and the person is asked as before.
    pub(crate) fn rule_grant(
        &self,
        request: &WireRequest,
        from: &marv_kernel::AgentRef,
        to: &marv_kernel::AgentRef,
    ) -> Option<Approver> {
        let main_inbox = |r: &ReceptacleName| r.inbox_owner() == Some(crate::PARENT_AGENT);
        let to_main = match crate::discovery::parse_peer(&request.to) {
            Some((_, remote)) => main_inbox(&remote),
            None => main_inbox(&request.to),
        };
        if !to_main || !matches!(request.mode, WireMode::Send | WireMode::Call) {
            return None;
        }
        let rules = self.rules_now()?;
        let access = marv_kernel::Access { from, to, mode: request.mode, now_ms: now_ms() };
        let matched = marv_kernel::match_rule(&rules, &access).ok()?;
        Some(Approver::Policy(crate::wire::RuleGrant::new(matched.rule.id, rules.generation)))
    }

    /// [`Self::rule_grant`] for a wire this Marv's main agent asks for (`mesh_send`/`mesh_call` with no wire yet):
    /// to another Marv's main inbox, whose identity comes from that Marv's own `marv/info`, asked of its door in
    /// this user's private mesh folder. `None` for anything else, or when the far Marv doesn't answer as itself in
    /// [`crate::discovery::DOOR_TIMEOUT`].
    pub async fn rule_for(&self, request: &WireRequest) -> Option<Approver> {
        if request.from.as_str() != crate::PARENT_AGENT || self.rules_now().is_none_or(|r| r.rules.is_empty()) {
            return None;
        }
        let me = self.identity(&request.from)?;
        let (session, remote) = crate::discovery::parse_peer(&request.to)?;
        if remote.inbox_owner() != Some(crate::PARENT_AGENT) {
            return None;
        }
        let socket = crate::discovery::session_socket(&self.peer_dir()?, &session)?;
        let ask = async { crate::discovery::DoorClient::connect(&socket).await?.info().await };
        let info = tokio::time::timeout(crate::discovery::DOOR_TIMEOUT, ask).await.ok()?.ok()?;
        if info.name != session {
            return None;
        }
        self.rule_grant(request, &me, &crate::discovery::agent_ref(&info))
    }

    /// The rule that would wire `from`'s sends to Marv `peer`'s main inbox, if one covers it (runtime-kernel live UX),
    /// for `mesh_list` to say so. From what the listing already has (`peer`'s `marv/info`, as found): it grants
    /// nothing, and the send itself asks [`Self::rule_for`], which asks that Marv's door again.
    pub fn rule_covers(&self, from: &PlugId, peer: &crate::discovery::PeerSwitchboard) -> Option<marv_kernel::PolicyId> {
        // As `rule_for` checks: the door must answer as the Marv it's named for (review P3), else the send would ask.
        if from.as_str() != crate::PARENT_AGENT || peer.session == self.session || peer.info.name != peer.session {
            return None;
        }
        let me = self.identity(from)?;
        let to = crate::discovery::peer_receptacle(&peer.session, &ReceptacleName::new("agent.parent.inbox").ok()?).ok()?;
        let request = WireRequest { from: from.clone(), to, mode: WireMode::Send, asked_by: from.clone() };
        match self.rule_grant(&request, &me, &crate::discovery::agent_ref(&peer.info))? {
            Approver::Policy(grant) => Some(grant.id()),
            _ => None,
        }
    }

    /// Runtime-kernel §14.4: whether a live rule the person made lets `peer`'s main agent send to this Marv's main
    /// agent `me` (a mutual rule does): where `mesh_list` may show text that flows from `peer` (its "editing:" line).
    /// Shown only: it wires nothing.
    pub fn rule_covers_from(&self, me: &PlugId, peer: &crate::discovery::PeerSwitchboard) -> bool {
        if me.as_str() != crate::PARENT_AGENT || peer.session == self.session || peer.info.name != peer.session {
            return false;
        }
        let (Some(rules), Some(mine)) = (self.rules_now(), self.identity(me)) else { return false };
        let theirs = crate::discovery::agent_ref(&peer.info);
        let access = marv_kernel::Access { from: &theirs, to: &mine, mode: marv_kernel::Mode::Send, now_ms: now_ms() };
        marv_kernel::match_rule(&rules, &access).is_ok()
    }

    pub fn unwire(&self, from: &PlugId, to: &ReceptacleName, mode: WireMode) -> bool {
        let mut board = self.board();
        board.limiters.remove(&(from.clone(), to.clone(), mode));
        board.table.remove(from, to, mode)
    }

    /// An agent ended: its plug, inbox, wires, socket and connections go.
    pub fn unplug(&self, id: &PlugId) {
        let plug = {
            let mut board = self.board();
            board.table.unplug(id);
            board.receptacles.remove(&ReceptacleName::inbox(id));
            board.limiters.retain(|(from, to, _), _| from != id && to.inbox_owner() != Some(id.as_str()));
            board.inboxes.remove(id);
            board.identities.remove(id);
            board.hops.remove(id);
            board.peers.remove(id);
            board.unserved.retain(|(p, _)| p != id);
            if let Some(stop) = board.stops.remove(id) {
                stop.cancel();
            }
            board.plugs.remove(id)
        };
        if let Some(plug) = plug {
            let _ = std::fs::remove_file(&plug.host_socket);
        }
    }

    /// **The one check.** Every "may `from` use `receptacle` in `mode`?" goes through here (runtime-kernel 2b, §4.3):
    /// admitting a message ([`Self::admit`]), listing what a plug may reach, whether a message may wake its reader,
    /// and whether a tool call needs a wire first. It answers from the person's wires alone, exactly as the table
    /// always has: default deny, by exact plug, receptacle and mode, never a wire to the plug's own receptacle, and a
    /// cross-process wire only once both ends approved. The wire that allows it is returned.
    ///
    /// A wire a rule made ([`Approver::Policy`], runtime-kernel 2b) is the same wire, and it admits only while its
    /// rule is still in the person's file and hasn't expired: revoking a rule ends what it wired at the next message
    /// ([`MeshError::RuleEnded`]). Rules never authorize anything by themselves here: a rule only ever adds a wire
    /// to the table, through [`Self::rule_grant`]. Not logged here: [`Self::admit`] logs every message it decides.
    pub fn authorize(&self, from: &PlugId, receptacle: &ReceptacleName, mode: WireMode) -> crate::Result<Wire> {
        let rules = self.rules_now();
        self.board().authorize(from, receptacle, mode, rules.as_deref(), now_ms()).cloned()
    }

    /// What `mesh_list` shows a plug: only what it's wired to, each wire as [`Self::authorize`] allows it.
    pub fn reachable(&self, from: &PlugId) -> Vec<Reachable> {
        let rules = self.rules_now();
        let now = now_ms();
        let board = self.board();
        let mut out: Vec<Reachable> = Vec::new();
        for w in board.table.wires_from(from) {
            if board.authorize(from, &w.to, w.mode, rules.as_deref(), now).is_err() {
                continue;
            }
            let Some(r) = board.receptacles.get(&w.to) else { continue };
            match out.iter_mut().find(|x| x.receptacle == w.to) {
                Some(x) => x.modes.push(w.mode),
                None => out.push(Reachable { receptacle: w.to.clone(), modes: vec![w.mode], description: r.description.clone() }),
            }
        }
        out
    }

    /// The policy check every message goes through (module docs), logged either way.
    pub fn admit(
        &self,
        from: &PlugId,
        receptacle: &ReceptacleName,
        mode: WireMode,
        body: Value,
        now_ms: u64,
    ) -> crate::Result<Envelope> {
        self.admit_claiming(from, receptacle, mode, body, now_ms, Claim::default())
    }

    /// [`Self::admit`], with the hop the request claimed (`tools/call`'s `_meta`): believed from a peer plug only.
    fn admit_claiming(
        &self,
        from: &PlugId,
        receptacle: &ReceptacleName,
        mode: WireMode,
        body: Value,
        now_ms: u64,
        claimed: Claim,
    ) -> crate::Result<Envelope> {
        let rules = self.rules_now();
        let (mut envelope, result) = {
            let mut board = self.board();
            board.seq += 1;
            let id = format!("{}:{}", self.session, board.seq);
            let mut envelope = Envelope::new(id, from.clone(), receptacle.clone(), mode, body, now_ms);
            envelope.hop = board.hop_of(from, claimed.hop);
            envelope.rounds_out = board.rounds_out_of(from, claimed.rounds_out);
            envelope.clean = board.clean_of(from, claimed.clean);
            if let Some(task) = board.report_of(from, claimed.report, claimed.task, &envelope.text()) {
                (envelope.report, envelope.task) = (true, task);
            }
            let result = board.check(&envelope, rules.as_deref());
            (envelope, result)
        };
        // A team coordinator's turn rooted only in its team's stamped reports ([`crate::hop::TurnHop::root_in_team`])
        // sends over a wire that same team rule issued at hop 0, so the worker takes the rework as a task
        // (docs/SECURITY.md). Only a plug of Marv's own has such a turn; the rooted rule is a team's by construction.
        if result.is_ok()
            && envelope.hop != Some(0)
            && let Some(rooted) = self.turn_hop(from).and_then(|h| h.team_rooted())
            && self.wire_rule(from, receptacle).is_some_and(|(id, _)| id == rooted)
        {
            envelope.hop = Some(0);
        }
        // Logged outside the lock: the sink is the session's trajectory writer.
        self.log_event(&envelope, result.as_ref().err());
        if let Err(MeshError::RuleEnded { from, receptacle, rule }) = &result {
            let generation = rules.as_ref().map_or(0, |r| r.generation);
            let (from, to, rule) = (from.clone(), receptacle.clone(), *rule);
            self.event(now_ms, |agent| marv_kernel::KernelEvent::Capability { agent, issued: false, from, to, rule, generation });
        }
        self.log(MeshRecord {
            at_ms: now_ms,
            id: result.is_ok().then(|| envelope.id.clone()),
            sender: from.to_string(),
            receptacle: receptacle.to_string(),
            mode,
            size: envelope.size,
            outcome: match &result {
                Ok(()) => Outcome::Delivered,
                Err(e) => Outcome::Refused { reason: e.to_string() },
            },
        });
        result.map(|()| envelope)
    }

    /// Who `plug` is, as the kernel's events name it, when this switchboard knows (its main agent, a confirmed peer).
    fn agent_key(&self, plug: &PlugId) -> Option<marv_kernel::AgentKey> {
        self.identity(plug).map(|me| match me.id {
            Some(id) => marv_kernel::AgentKey::Chat(id),
            None => marv_kernel::AgentKey::Mesh(me.mesh_name),
        })
    }

    /// The kernel's event for a message this switchboard decided (runtime-kernel 3): who sent it (and who that is,
    /// when known), where to, how, how big and whether it got through, and if not, the refusal's category
    /// (runtime-kernel live UX). Never the body, never the refusal's sentence.
    fn log_event(&self, envelope: &Envelope, refused: Option<&MeshError>) {
        let from_agent = self.agent_key(&envelope.sender);
        self.event(envelope.at_ms, |agent| marv_kernel::KernelEvent::Message {
            agent,
            id: envelope.id.clone(),
            from: envelope.sender.to_string(),
            from_agent,
            to: envelope.receptacle.to_string(),
            mode: envelope.mode,
            hop: envelope.hop,
            size: envelope.size,
            outcome: if refused.is_none() { marv_kernel::Delivery::Delivered } else { marv_kernel::Delivery::Refused },
            refusal: refused.map(refusal),
        });
    }

    /// Logs that a wire `request` asked for wasn't made, and why (runtime-kernel live UX): this Marv's person said no
    /// (the mesh tools' wire prompt, or the door's for another Marv's request), or the other Marv refused. Ids and a
    /// fixed reason only.
    pub fn log_wire_refused(&self, request: &crate::approval::WireRequest, reason: marv_kernel::WireRefusal) {
        let from_agent = self.agent_key(&request.from);
        let (from, to, mode) = (request.from.to_string(), request.to.to_string(), request.mode);
        self.event(now_ms(), |agent| marv_kernel::KernelEvent::WireRefused { agent, from, from_agent, to, mode, reason });
    }

    /// Delivers an admitted message: into an inbox (v1), or to the receptacle's [`Service`] (v2: a peer switchboard,
    /// a Herdr pane). `call` waits for the response. A message that can't be delivered after all (its inbox filled
    /// since `admit`, its agent ended, the other Marv went away) is logged again, refused, under the same id.
    pub async fn route(self: Arc<Self>, envelope: Envelope) -> crate::Result<Option<Value>> {
        enum Next {
            Done(crate::Result<Option<Value>>),
            Serve(Arc<dyn Service>),
        }
        // A report delivered to the main agent's inbox, told to the host once the board is unlocked.
        let mut report: Option<(PlugId, String)> = None;
        let next = {
            let mut board = self.board();
            let kind = board.receptacles.get(&envelope.receptacle).map(|r| (r.kind.clone(), r.owner.clone()));
            match kind {
                None => Next::Done(Err(MeshError::NoReceptacle(envelope.receptacle.to_string()))),
                Some((ReceptacleKind::Inbox, Some(owner))) if envelope.mode == WireMode::Send => {
                    Next::Done(match board.inboxes.get_mut(&owner) {
                        Some(queue) if queue.len() < DEFAULT_INBOX_DEPTH => {
                            queue.push_back(envelope.clone());
                            self.mail.notify_one();
                            let text = envelope.text();
                            if owner.as_str() == crate::PARENT_AGENT && envelope.report {
                                report = Some((envelope.sender.clone(), text));
                            }
                            Ok(None)
                        }
                        Some(_) => Err(MeshError::InboxFull(envelope.receptacle.to_string())),
                        None => Err(MeshError::NoReceptacle(envelope.receptacle.to_string())),
                    })
                }
                // `watch` on inboxes is still to come.
                Some((ReceptacleKind::Inbox, _)) => {
                    Next::Done(Err(MeshError::NotServed { receptacle: envelope.receptacle.to_string(), mode: envelope.mode }))
                }
                Some(_) => match board.services.get(&envelope.receptacle) {
                    Some(service) => Next::Serve(service.clone()),
                    None => {
                        Next::Done(Err(MeshError::NotServed { receptacle: envelope.receptacle.to_string(), mode: envelope.mode }))
                    }
                },
            }
        };
        if let Some((sender, text)) = report {
            let sink = self.report_sink.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(sink) = sink {
                sink(&sender, &text);
            }
        }
        // A service is awaited outside the lock: it may talk to another process.
        let result = match next {
            Next::Done(result) => result,
            Next::Serve(service) => service.deliver(&envelope).await,
        };
        if let Err(err) = &result {
            self.log_event(&envelope, Some(err));
            self.log(MeshRecord {
                at_ms: envelope.at_ms,
                id: Some(envelope.id.clone()),
                sender: envelope.sender.to_string(),
                receptacle: envelope.receptacle.to_string(),
                mode: envelope.mode,
                size: envelope.size,
                outcome: Outcome::Refused { reason: err.to_string() },
            });
        }
        result
    }

    /// One message from `from`: [`Self::admit`], then [`Self::route`]. What `tools/call` runs.
    pub async fn deliver(
        self: &Arc<Self>,
        from: &PlugId,
        receptacle: &str,
        mode: WireMode,
        body: Value,
    ) -> crate::Result<Option<Value>> {
        self.deliver_claiming(from, receptacle, mode, body, Claim::default()).await
    }

    async fn deliver_claiming(
        self: &Arc<Self>,
        from: &PlugId,
        receptacle: &str,
        mode: WireMode,
        body: Value,
        claimed: Claim,
    ) -> crate::Result<Option<Value>> {
        let receptacle = ReceptacleName::new(receptacle)?;
        let envelope = self.admit_claiming(from, &receptacle, mode, body, now_ms(), claimed)?;
        Arc::clone(self).route(envelope).await
    }

    /// One MCP request from `from`'s socket: `initialize`, `ping`, `tools/list` (= [`Self::reachable`]),
    /// `tools/call` (= [`Self::deliver`]). The mode rides in `_meta` ([`MODE_META`], default `call`). A refusal is
    /// a tool result with `isError`, so the agent reads it; the request's own fields are never echoed back.
    pub async fn handle(self: &Arc<Self>, from: &PlugId, request: Request) -> Response {
        let id = request.id.clone();
        match request.method.as_str() {
            method::INITIALIZE => Response::ok(
                id,
                json!({
                    "protocolVersion": marv_mcp::jsonrpc::PROTOCOL_VERSION,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "marv-switchboard", "version": env!("CARGO_PKG_VERSION")},
                }),
            ),
            method::PING => Response::ok(id, json!({})),
            method::TOOLS_LIST => {
                let tools: Vec<Value> = self
                    .reachable(from)
                    .into_iter()
                    .map(|r| {
                        let schema = self.receptacle(&r.receptacle).map_or_else(|| json!({"type": "object"}), |x| x.input_schema);
                        json!({
                            "name": r.receptacle,
                            "description": r.description,
                            "inputSchema": schema,
                            "_meta": {MODES_META: r.modes},
                        })
                    })
                    .collect();
                Response::ok(id, json!({"tools": tools}))
            }
            method::TOOLS_CALL => {
                let params = request.params.unwrap_or(Value::Null);
                let Some(name) = params.get("name").and_then(Value::as_str) else {
                    return Response::err(id, code::INVALID_PARAMS, "tools/call needs a name");
                };
                let mode = match params.get("_meta").and_then(|m| m.get(MODE_META)) {
                    None => Ok(WireMode::Call),
                    Some(m) => serde_json::from_value::<WireMode>(m.clone()).map_err(|_| ()),
                };
                let Ok(mode) = mode else {
                    return Response::err(id, code::INVALID_PARAMS, "the mode is call, send or watch");
                };
                let body = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                // A claimed hop (and rounds used up): believed from a peer Marv's plug only (`Board::hop_of`).
                let meta = params.get("_meta");
                let claimed = Claim {
                    hop: meta
                        .and_then(|m| m.get(crate::hop::HOP_META))
                        .and_then(Value::as_u64)
                        .and_then(|h| u32::try_from(h).ok()),
                    rounds_out: meta.and_then(|m| m.get(crate::hop::ROUNDS_META)).and_then(Value::as_bool).unwrap_or(false),
                    report: meta.and_then(|m| m.get(crate::hop::REPORT_META)).and_then(Value::as_bool).unwrap_or(false),
                    task: meta.and_then(|m| m.get(crate::hop::TASK_META)).and_then(Value::as_bool).unwrap_or(false),
                    clean: meta.and_then(|m| m.get(crate::hop::CLEAN_META)).and_then(Value::as_bool).unwrap_or(false),
                };
                let (text, is_error) = match self.deliver_claiming(from, name, mode, body, claimed).await {
                    Ok(Some(Value::String(answer))) => (answer, false),
                    Ok(Some(answer)) => (answer.to_string(), false),
                    Ok(None) => (format!("Delivered to {name}."), false),
                    Err(err) => (err.to_string(), true),
                };
                Response::ok(id, json!({"content": [{"type": "text", "text": text}], "isError": is_error}))
            }
            _ => Response::err(id, code::METHOD_NOT_FOUND, "the switchboard doesn't know that method"),
        }
    }

    /// Listens on every plug's socket (0600, in the 0700 runtime folder) until cancelled, including plugs added
    /// later. Each connection's task holds its own clone of the `Arc`; who sent a message is the plug whose socket it
    /// arrived on.
    pub async fn serve(self: Arc<Self>, cancel: CancellationToken) -> crate::Result<()> {
        loop {
            let unserved = std::mem::take(&mut self.board().unserved);
            for (id, listener) in unserved {
                let stop = self.board().stops.get(&id).cloned().unwrap_or_default();
                let listener = listener.into_tokio().map_err(|e| io("listener", e))?;
                tokio::spawn(Arc::clone(&self).accept(id, listener, stop, cancel.clone()));
            }
            tokio::select! {
                _ = self.wake.notified() => {}
                _ = cancel.cancelled() => return Ok(()),
            }
        }
    }

    async fn accept(self: Arc<Self>, id: PlugId, listener: UnixListener, stop: CancellationToken, cancel: CancellationToken) {
        loop {
            tokio::select! {
                _ = stop.cancelled() => return,
                _ = cancel.cancelled() => return,
                conn = listener.accept() => match conn {
                    Ok((stream, _)) => {
                        tokio::spawn(Arc::clone(&self).connection(id.clone(), stream, stop.clone(), cancel.clone()));
                    }
                    // Out of file descriptors and the like: try again shortly rather than spin.
                    Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
                },
            }
        }
    }

    /// One client on a plug's socket: newline-delimited JSON-RPC, one request at a time.
    async fn connection(self: Arc<Self>, id: PlugId, stream: UnixStream, stop: CancellationToken, cancel: CancellationToken) {
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        loop {
            let mut line = Vec::new();
            let mut limited = (&mut read).take(MAX_LINE_BYTES as u64 + 1);
            let n = tokio::select! {
                _ = stop.cancelled() => return,
                _ = cancel.cancelled() => return,
                n = limited.read_until(b'\n', &mut line) => n,
            };
            match n {
                Ok(0) | Err(_) => return,
                Ok(_) if line.len() > MAX_LINE_BYTES => return,
                Ok(_) => {}
            }
            let Ok(text) = std::str::from_utf8(&line) else { return };
            // Notifications (`initialized`) and anything unparsable get no answer.
            let Ok(Message::Request(request)) = decode_line(text) else { continue };
            let response = self.handle(&id, request).await;
            if write.write_all(encode_line(&Message::Response(response)).as_bytes()).await.is_err() {
                return;
            }
        }
    }

    /// Resolves once a message has gone into an inbox since the last call (#78). Any agent's inbox: the caller reads
    /// [`Self::unread`] for the one it cares about.
    pub async fn mail(&self) {
        self.mail.notified().await;
    }

    /// The wires of an agent's unread messages (sender, receptacle), oldest first, without taking them: they're
    /// still drained, fenced, by its next turn (#78).
    pub fn unread(&self, agent: &PlugId) -> Vec<(PlugId, ReceptacleName)> {
        self.board()
            .inboxes
            .get(agent)
            .map(|q| q.iter().map(|e| (e.sender.clone(), e.receptacle.clone())).collect())
            .unwrap_or_default()
    }

    /// [`Self::unread`], with each message's hop (runtime-kernel 2b): what decides whether a rule-issued wire's message
    /// may wake its reader.
    pub fn unread_hops(&self, agent: &PlugId) -> Vec<(PlugId, ReceptacleName, Option<u32>)> {
        self.board()
            .inboxes
            .get(agent)
            .map(|q| q.iter().map(|e| (e.sender.clone(), e.receptacle.clone(), e.hop)).collect())
            .unwrap_or_default()
    }

    /// [`Self::unread_hops`] with each message's id (runtime-kernel live UX): what tells the person, once per message,
    /// that mail waits and why it didn't wake Marv. Never a body.
    pub fn unread_messages(&self, agent: &PlugId) -> Vec<UnreadMessage> {
        self.board()
            .inboxes
            .get(agent)
            .map(|q| {
                q.iter()
                    .map(|e| UnreadMessage {
                        id: e.id.clone(),
                        sender: e.sender.clone(),
                        receptacle: e.receptacle.clone(),
                        hop: e.hop,
                        rounds_out: e.rounds_out,
                        report: e.report,
                        task: e.task,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Of `ids` (unread messages in `agent`'s inbox), the ones not logged as a skipped wake yet, now marked logged
    /// (runtime-kernel live UX). Ids no longer unread are forgotten.
    pub fn first_skip_logs(&self, agent: &PlugId, ids: &[String]) -> Vec<String> {
        let mut board = self.board();
        let unread: std::collections::HashSet<String> =
            board.inboxes.get(agent).map(|q| q.iter().map(|e| e.id.clone()).collect()).unwrap_or_default();
        board.skip_logged.retain(|id| unread.contains(id));
        ids.iter().filter(|id| unread.contains(*id) && board.skip_logged.insert((*id).clone())).cloned().collect()
    }

    /// Takes the unread messages with these ids from `agent`'s inbox (oldest first), leaving the rest unread
    /// (runtime-kernel live UX: a turn a rule's message woke reads only rules' wires).
    pub fn take_inbox(&self, agent: &PlugId, ids: &[String]) -> Vec<Envelope> {
        let mut board = self.board();
        let Some(queue) = board.inboxes.get_mut(agent) else { return Vec::new() };
        let (taken, kept): (Vec<Envelope>, Vec<Envelope>) = queue.drain(..).partition(|e| ids.contains(&e.id));
        queue.extend(kept);
        taken
    }

    /// The inbox's unread messages for an agent's next turn (oldest first).
    pub fn drain_inbox(&self, agent: &PlugId) -> Vec<Envelope> {
        self.board().inboxes.get_mut(agent).map(|q| q.drain(..).collect()).unwrap_or_default()
    }
}

impl Drop for Switchboard {
    fn drop(&mut self) {
        for plug in self.board().plugs.values() {
            let _ = std::fs::remove_file(&plug.host_socket);
        }
        if self.owns_folder {
            // Only when empty: anything else in it is left as it is.
            let _ = std::fs::remove_dir(&self.runtime_dir);
        }
    }
}

/// Whether a message must be text (#123): every `send` (an inbox, another Marv's inbox) and a Herdr pane's `.send`,
/// whose text is typed into the pane. Their text is a JSON string or `{"text": "…"}`; anything else is refused here,
/// before it's queued, forwarded or typed, never passed on as JSON.
fn takes_text(r: &Receptacle, e: &Envelope) -> bool {
    e.mode == WireMode::Send
        || (r.kind == ReceptacleKind::HerdrPane
            && crate::herdr::parse_pane_receptacle(&e.receptacle).is_some_and(|(_, a)| a == crate::herdr::PaneAction::Send))
}

impl Board {
    /// A message's hop ([`crate::hop`]): Marv's own plug's registered turn hop (whatever the request claimed), a peer
    /// Marv's forwarded hop, or none (a shim plug, or a plug nobody registered a turn for).
    /// marv-rs: whether the sender had read nothing from outside: Marv's own main plug's turn says
    /// ([`TurnHop::clean`]; a subagent's plug shares the turn's hop but not its taint, so its mail is never clean), a
    /// peer Marv's forwarded flag, never anything else (a shim's mail is never clean).
    fn clean_of(&self, from: &PlugId, claimed: bool) -> bool {
        match self.hops.get(from) {
            Some(hop) => from.as_str() == crate::PARENT_AGENT && hop.clean(),
            None => claimed && self.peers.contains(from),
        }
    }

    fn hop_of(&self, from: &PlugId, claimed: Option<u32>) -> Option<u32> {
        match self.hops.get(from) {
            Some(hop) => Some(hop.get()),
            None if self.peers.contains(from) => claimed,
            None => None,
        }
    }

    /// Whether a message is a woken turn's automatic report: Marv's own plug's only when its session armed exactly this
    /// text ([`crate::hop::TurnHop::arm_report`]), a peer Marv's forwarded flag, never anything else.
    /// `Some(task)` for a report (whether it answers a task), the same way.
    fn report_of(&self, from: &PlugId, claimed: bool, claimed_task: bool, text: &str) -> Option<bool> {
        match self.hops.get(from) {
            Some(hop) => hop.take_report(text),
            None => (claimed && self.peers.contains(from)).then_some(claimed_task),
        }
    }

    /// Whether a message's sending turn used up its rounds (runtime-kernel live UX), as [`Self::hop_of`] decides a hop.
    fn rounds_out_of(&self, from: &PlugId, claimed: bool) -> bool {
        match self.hops.get(from) {
            Some(hop) => hop.rounds_out(),
            None => claimed && self.peers.contains(from),
        }
    }

    /// [`Switchboard::authorize`] under the board's lock: the only caller of the wire table's lookups. `rules`: the
    /// person's rules now, when this Marv honors them; a wire a rule made needs its rule there and live at `now_ms`
    /// (rules are only ever added or revoked, never changed, so the id is enough). With no rules honored, a rule's
    /// wire admits nothing.
    fn authorize(
        &self,
        from: &PlugId,
        receptacle: &ReceptacleName,
        mode: WireMode,
        rules: Option<&marv_kernel::PolicySnapshot>,
        now_ms: u64,
    ) -> crate::Result<&Wire> {
        let wire = self.table.check(from, receptacle, mode)?;
        if let Approver::Policy(grant) = wire.grant.local {
            let live = rules.is_some_and(|s| s.rules.iter().any(|r| r.id == grant.id() && r.live_at(now_ms)));
            if !live {
                return Err(MeshError::RuleEnded {
                    from: from.to_string(),
                    receptacle: receptacle.to_string(),
                    rule: grant.id(),
                });
            }
        }
        Ok(wire)
    }

    fn check(&mut self, e: &Envelope, rules: Option<&marv_kernel::PolicySnapshot>) -> crate::Result<()> {
        let r = self.receptacles.get(&e.receptacle).ok_or_else(|| MeshError::NoReceptacle(e.receptacle.to_string()))?;
        let not_wired = || MeshError::NotWired { from: e.sender.to_string(), receptacle: e.receptacle.to_string(), mode: e.mode };
        if !r.supports(e.mode) {
            return Err(not_wired());
        }
        let full = r.kind == ReceptacleKind::Inbox
            && r.owner.as_ref().and_then(|o| self.inboxes.get(o)).is_some_and(|q| q.len() >= DEFAULT_INBOX_DEPTH);
        let wire = self.authorize(&e.sender, &e.receptacle, e.mode, rules, e.at_ms)?;
        let limits = wire.limits;
        if takes_text(r, e) && text_body(&e.body).is_none() {
            return Err(bad_text_body(e.receptacle.as_str()));
        }
        limits.check_size(e.receptacle.as_str(), e.size)?;
        if full {
            return Err(MeshError::InboxFull(e.receptacle.to_string()));
        }
        self.limiters
            .entry((e.sender.clone(), e.receptacle.clone(), e.mode))
            .or_insert_with(|| RateLimiter::new(limits, e.at_ms))
            .take(e.receptacle.as_str(), e.at_ms)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::limits::WireLimits;
    use crate::plug::PlugClient;
    use crate::wire::{Approver, Grant};
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    #[derive(Default)]
    pub(crate) struct Log(pub Mutex<Vec<MeshRecord>>);
    impl TrajectorySink for Log {
        fn mesh(&self, record: MeshRecord) {
            self.0.lock().unwrap().push(record);
        }
    }

    /// A private runtime folder, removed on drop. Short, so socket paths stay under the 108-byte limit.
    pub(crate) struct Runtime(pub PathBuf);
    impl Runtime {
        pub(crate) fn new() -> Self {
            static N: AtomicUsize = AtomicUsize::new(0);
            Self(std::env::temp_dir().join(format!("mm-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst))))
        }
    }
    impl Drop for Runtime {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn id(s: &str) -> PlugId {
        PlugId::new(s).unwrap()
    }

    fn board() -> (Switchboard, Arc<Log>, Runtime) {
        let log = Arc::new(Log::default());
        let runtime = Runtime::new();
        let sb = Switchboard::new("s1", runtime.0.clone(), log.clone());
        sb.plug(id("parent")).unwrap();
        sb.plug(id("worker-1")).unwrap();
        (sb, log, runtime)
    }

    fn tui(from: &str, to: &ReceptacleName) -> Wire {
        Wire::new(id(from), to.clone(), WireMode::Send, Grant::local(Approver::Tui))
    }

    #[test]
    fn shared_without_a_lock_around_it() {
        fn shared<T: Send + Sync>(_: &T) {}
        let (sb, _, _rt) = board();
        let sb = Arc::new(sb);
        shared(&sb);
        let inbox = ReceptacleName::inbox(&id("parent"));
        let other = sb.clone();
        std::thread::spawn(move || {
            other.wire(Wire::new(id("worker-1"), inbox, WireMode::Send, Grant::local(Approver::Tui))).unwrap();
        })
        .join()
        .unwrap();
        assert_eq!(sb.reachable(&id("worker-1")).len(), 1);
    }

    #[test]
    fn unwired_messages_are_refused_and_logged() {
        let (sb, log, _rt) = board();
        let inbox = ReceptacleName::inbox(&id("parent"));
        let err = sb.admit(&id("worker-1"), &inbox, WireMode::Send, Value::from("hi"), 0).unwrap_err();
        assert!(matches!(err, MeshError::NotWired { .. }));
        let records = log.0.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert!(matches!(records[0].outcome, Outcome::Refused { .. }));
        assert_eq!(records[0].size, 4);
        assert_eq!(records[0].id, None);
    }

    #[test]
    fn a_wired_message_is_admitted_as_mesh_text() {
        let (sb, log, _rt) = board();
        let inbox = ReceptacleName::inbox(&id("parent"));
        sb.wire(Wire::new(id("worker-1"), inbox.clone(), WireMode::Send, Grant::local(Approver::Tui))).unwrap();
        let e = sb.admit(&id("worker-1"), &inbox, WireMode::Send, Value::from("done"), 5).unwrap();
        assert_eq!(e.provenance, marv_core::Provenance::Mesh);
        assert_eq!(e.id, "s1:1");
        assert_eq!(log.0.lock().unwrap()[0].outcome, Outcome::Delivered);
        // An inbox takes no calls, wired or not.
        assert!(sb.admit(&id("worker-1"), &inbox, WireMode::Call, Value::Null, 5).is_err());
        assert_eq!(sb.reachable(&id("worker-1")).len(), 1);
        assert!(sb.reachable(&id("parent")).is_empty());
    }

    #[test]
    fn agents_cant_wire_their_own_inbox() {
        let (sb, _, _rt) = board();
        let own = ReceptacleName::inbox(&id("worker-1"));
        assert!(matches!(
            sb.wire(Wire::new(id("worker-1"), own, WireMode::Send, Grant::local(Approver::Cli))),
            Err(MeshError::SelfWire(_))
        ));
    }

    #[test]
    fn per_wire_limits_apply() {
        let (sb, _, _rt) = board();
        let inbox = ReceptacleName::inbox(&id("parent"));
        let mut w = Wire::new(id("worker-1"), inbox.clone(), WireMode::Send, Grant::local(Approver::Tui));
        w.limits = WireLimits { max_message_bytes: 8, per_minute: 1, burst: 1 };
        sb.wire(w).unwrap();
        assert!(matches!(
            sb.admit(&id("worker-1"), &inbox, WireMode::Send, Value::from("x".repeat(20)), 0),
            Err(MeshError::TooLarge { .. })
        ));
        sb.admit(&id("worker-1"), &inbox, WireMode::Send, Value::from("a"), 0).unwrap();
        assert!(matches!(
            sb.admit(&id("worker-1"), &inbox, WireMode::Send, Value::from("b"), 1),
            Err(MeshError::RateLimited { .. })
        ));
    }

    #[test]
    fn unplugging_removes_wires_both_ways_and_the_socket() {
        let (sb, _, _rt) = board();
        let to_parent = ReceptacleName::inbox(&id("parent"));
        let to_worker = ReceptacleName::inbox(&id("worker-1"));
        sb.wire(tui("worker-1", &to_parent)).unwrap();
        sb.wire(tui("parent", &to_worker)).unwrap();
        let socket = sb.plug(id("worker-1")).unwrap().host_socket;
        assert!(socket.exists());
        sb.unplug(&id("worker-1"));
        assert!(sb.table().all().is_empty());
        assert!(sb.receptacle(&to_worker).is_none());
        assert!(!socket.exists());
    }

    #[cfg(unix)]
    #[test]
    fn plugs_get_private_sockets_and_workers_are_numbered() {
        use std::os::unix::fs::PermissionsExt;
        let (sb, _, rt) = board();
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&rt.0), 0o700);
        let w = sb.plug_next("worker").unwrap();
        // worker-1 is taken already: the next free number.
        assert_eq!(w.id.as_str(), "worker-2");
        assert_eq!(mode(&w.host_socket), 0o600);
        assert_eq!(sb.plug_next("worker").unwrap().id.as_str(), "worker-3");
        // Plugging again hands back the same plug.
        assert_eq!(sb.plug(id("worker-2")).unwrap(), w);
    }

    #[tokio::test]
    async fn routed_sends_wait_in_the_inbox_until_drained() {
        let (sb, log, _rt) = board();
        let sb = Arc::new(sb);
        let inbox = ReceptacleName::inbox(&id("parent"));
        sb.wire(tui("worker-1", &inbox)).unwrap();
        for text in ["one", "two"] {
            sb.deliver(&id("worker-1"), inbox.as_str(), WireMode::Send, json!({"text": text})).await.unwrap();
        }
        let got = sb.drain_inbox(&id("parent"));
        assert_eq!(got.iter().map(Envelope::text).collect::<Vec<_>>(), ["one", "two"]);
        assert!(got.iter().all(|e| e.sender == id("worker-1")));
        assert!(sb.drain_inbox(&id("parent")).is_empty());
        assert!(sb.drain_inbox(&id("worker-1")).is_empty());
        assert_eq!(log.0.lock().unwrap().len(), 2);
    }

    /// A report-back delivered to the main inbox is told to the host's sink with its stamped sender, the body's claim
    /// aside; an ordinary message isn't.
    #[tokio::test]
    async fn a_report_delivered_to_the_main_inbox_is_told_with_its_stamped_sender() {
        let (sb, _log, _rt) = board();
        let sb = Arc::new(sb);
        let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
        let into = seen.clone();
        sb.set_report_sink(Arc::new(move |sender: &PlugId, text: &str| {
            into.lock().unwrap().push((sender.to_string(), text.to_string()));
        }));
        let cancel = CancellationToken::new();
        tokio::spawn(Arc::clone(&sb).serve(cancel.clone()));
        let inbox = ReceptacleName::inbox(&id("parent"));
        let hop = Arc::new(crate::hop::TurnHop::default());
        sb.set_turn_hop(id("worker-1"), hop.clone());
        sb.wire(tui("worker-1", &inbox)).unwrap();
        let mut own = PlugClient::connect(&sb.plug(id("worker-1")).unwrap().host_socket).await.unwrap();
        own.send(&inbox, "hello").await.unwrap();
        // Text that merely looks like a report, never armed by the sender's session: not told.
        let forged = "Marv report from site-2 (done): all fine";
        own.send(&inbox, forged).await.unwrap();
        // The stamped report is told, keyed by who sent it, never by the body's claim.
        hop.arm_report(forged, false);
        own.send(&inbox, forged).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(*seen.lock().unwrap(), [("worker-1".to_string(), forged.to_string())], "keyed by who sent it");
        cancel.cancel();
    }

    /// #78: a delivery into an inbox rings the mail bell (once, even if nobody was listening yet), and the unread
    /// messages' wires can be read without taking them: the next turn still drains them.
    #[tokio::test]
    async fn an_inbox_delivery_rings_the_mail_bell_and_can_be_peeked() {
        let (sb, _log, _rt) = board();
        let sb = Arc::new(sb);
        let inbox = ReceptacleName::inbox(&id("parent"));
        sb.wire(tui("worker-1", &inbox)).unwrap();
        let quiet = tokio::time::timeout(std::time::Duration::from_millis(50), sb.mail()).await;
        assert!(quiet.is_err(), "no mail yet");
        sb.deliver(&id("worker-1"), inbox.as_str(), WireMode::Send, json!({"text": "hi"})).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), sb.mail()).await.expect("rung");
        assert_eq!(sb.unread(&id("parent")), [(id("worker-1"), inbox.clone())]);
        assert_eq!(sb.unread(&id("parent")).len(), 1, "peeking takes nothing");
        assert_eq!(sb.drain_inbox(&id("parent")).len(), 1);
        assert!(sb.unread(&id("parent")).is_empty());
        assert!(sb.unread(&id("nobody")).is_empty());
    }

    /// #123: an inbox send is text. Anything else is refused at admission (logged), never queued as JSON.
    #[tokio::test]
    async fn an_inbox_send_that_isnt_text_is_refused_not_queued() {
        let (sb, log, _rt) = board();
        let sb = Arc::new(sb);
        let inbox = ReceptacleName::inbox(&id("parent"));
        sb.wire(tui("worker-1", &inbox)).unwrap();
        let err = sb.deliver(&id("worker-1"), inbox.as_str(), WireMode::Send, json!({"prompt": "go"})).await.unwrap_err();
        assert!(err.to_string().contains(r#"expected {"text": "..."}"#), "{err}");
        assert!(matches!(log.0.lock().unwrap().last().unwrap().outcome, Outcome::Refused { .. }));
        assert!(sb.drain_inbox(&id("parent")).is_empty());
        sb.deliver(&id("worker-1"), inbox.as_str(), WireMode::Send, json!({"text": "go"})).await.unwrap();
        assert_eq!(sb.drain_inbox(&id("parent"))[0].text(), "go");
    }

    #[tokio::test]
    async fn a_full_inbox_keeps_the_oldest_and_refuses_new_ones() {
        let (sb, log, _rt) = board();
        let sb = Arc::new(sb);
        let inbox = ReceptacleName::inbox(&id("parent"));
        let mut w = tui("worker-1", &inbox);
        w.limits.burst = 1000;
        sb.wire(w).unwrap();
        for i in 0..DEFAULT_INBOX_DEPTH {
            sb.deliver(&id("worker-1"), inbox.as_str(), WireMode::Send, Value::from(i.to_string())).await.unwrap();
        }
        let err = sb.deliver(&id("worker-1"), inbox.as_str(), WireMode::Send, Value::from("late")).await.unwrap_err();
        assert!(matches!(err, MeshError::InboxFull(_)));
        assert!(matches!(log.0.lock().unwrap().last().unwrap().outcome, Outcome::Refused { .. }));
        let kept = sb.drain_inbox(&id("parent"));
        assert_eq!(kept.len(), DEFAULT_INBOX_DEPTH);
        assert_eq!(kept[0].text(), "0");
    }

    #[tokio::test]
    async fn over_the_socket_identity_is_the_socket_and_only_wires_count() {
        let (sb, log, _rt) = board();
        let sb = Arc::new(sb);
        let cancel = CancellationToken::new();
        tokio::spawn(Arc::clone(&sb).serve(cancel.clone()));
        let to_parent = ReceptacleName::inbox(&id("parent"));
        let worker = sb.plug(id("worker-1")).unwrap();
        let mut client = PlugClient::connect(&worker.host_socket).await.unwrap();
        // Nothing wired: nothing listed, and a send is refused (and logged).
        assert!(client.list().await.unwrap().is_empty());
        let err = client.send(&to_parent, "hi").await.unwrap_err();
        assert!(err.to_string().starts_with("not wired: worker-1 has no send wire to agent.parent.inbox"), "{err}");
        assert!(sb.drain_inbox(&id("parent")).is_empty());
        // Wired by the user: listed, delivered, and the sender is the socket's plug.
        sb.wire(tui("worker-1", &to_parent)).unwrap();
        let listed = client.list().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!((listed[0].receptacle.clone(), listed[0].modes.clone()), (to_parent.clone(), vec![WireMode::Send]));
        client.send(&to_parent, "done").await.unwrap();
        let got = sb.drain_inbox(&id("parent"));
        assert_eq!((got[0].sender.as_str(), got[0].text().as_str()), ("worker-1", "done"));
        // A plug added after serve started is served too.
        let late = sb.plug(id("worker-9")).unwrap();
        let mut late = PlugClient::connect(&late.host_socket).await.unwrap();
        assert!(late.send(&to_parent, "x").await.is_err());
        let outcomes: Vec<bool> = log.0.lock().unwrap().iter().map(|r| r.outcome == Outcome::Delivered).collect();
        assert_eq!(outcomes, [false, true, false]);
        // Unplugged: its connections close.
        sb.unplug(&id("worker-1"));
        assert!(client.list().await.is_err());
        cancel.cancel();
    }

    /// Runtime-kernel 2b: a message's hop is Marv's to set. A plug of Marv's own is stamped with its registered turn's
    /// hop whatever the request claims (a sandboxed command can speak on its plug's socket); only a peer Marv's plug
    /// carries its forwarded hop; a shim's (or an unregistered plug's) has none.
    #[tokio::test]
    async fn the_hop_is_the_turns_never_the_requests() {
        let (sb, _log, _rt) = board();
        let sb = Arc::new(sb);
        let cancel = CancellationToken::new();
        tokio::spawn(Arc::clone(&sb).serve(cancel.clone()));
        let inbox = ReceptacleName::inbox(&id("parent"));
        let hop = Arc::new(crate::hop::TurnHop::default());
        hop.set(2);
        sb.set_turn_hop(id("worker-1"), hop.clone());
        let shim = sb.plug(id("claude")).unwrap();
        let peer = sb.plug_peer(id("peer-alpha")).unwrap();
        for from in ["worker-1", "claude", "peer-alpha"] {
            sb.wire(tui(from, &inbox)).unwrap();
        }
        let worker = sb.plug(id("worker-1")).unwrap();
        let mut client = PlugClient::connect(&worker.host_socket).await.unwrap();
        client.forward(&inbox, WireMode::Send, json!({"text": "claims 0"}), Some(0), false, None, false).await.unwrap();
        client.send(&inbox, "claims nothing").await.unwrap();
        hop.set(0);
        client.send(&inbox, "the person's turn").await.unwrap();
        let mut shim = PlugClient::connect(&shim.host_socket).await.unwrap();
        shim.forward(&inbox, WireMode::Send, json!({"text": "a shim claims 0"}), Some(0), false, None, false).await.unwrap();
        let mut peer = PlugClient::connect(&peer.host_socket).await.unwrap();
        peer.forward(&inbox, WireMode::Send, json!({"text": "from alpha at 1"}), Some(1), false, None, false).await.unwrap();
        peer.send(&inbox, "from an older alpha").await.unwrap();
        let hops: Vec<(String, Option<u32>)> =
            sb.drain_inbox(&id("parent")).iter().map(|e| (e.sender.to_string(), e.hop)).collect();
        assert_eq!(
            hops,
            [
                ("worker-1".to_string(), Some(2)),
                ("worker-1".to_string(), Some(2)),
                ("worker-1".to_string(), Some(0)),
                ("claude".to_string(), None),
                ("peer-alpha".to_string(), Some(1)),
                ("peer-alpha".to_string(), None),
            ]
        );
        // Unplugged, its turn's hop goes with it.
        sb.unplug(&id("worker-1"));
        assert!(sb.turn_hop(&id("worker-1")).is_none());
        cancel.cancel();
    }

    /// A message is a report only when the sending Marv's code stamped it: Marv's own plug's text that its session armed
    /// (once), a peer Marv's forwarded flag. A claim on a plug's socket, a shim's, or text that looks like one isn't.
    #[tokio::test]
    async fn a_report_is_stamped_by_the_sending_marv_never_read_from_text_or_a_claim() {
        let (sb, _log, _rt) = board();
        let sb = Arc::new(sb);
        let cancel = CancellationToken::new();
        tokio::spawn(Arc::clone(&sb).serve(cancel.clone()));
        let inbox = ReceptacleName::inbox(&id("parent"));
        let hop = Arc::new(crate::hop::TurnHop::default());
        sb.set_turn_hop(id("worker-1"), hop.clone());
        for from in ["worker-1", "claude", "peer-alpha"] {
            sb.wire(tui(from, &inbox)).unwrap();
        }
        let like = format!("{}worker-1 (done): x", crate::message::REPORT_PREFIX);
        let mut own = PlugClient::connect(&sb.plug(id("worker-1")).unwrap().host_socket).await.unwrap();
        own.send(&inbox, &like).await.unwrap();
        own.forward(&inbox, WireMode::Send, json!({"text": "claims"}), None, false, Some(false), false).await.unwrap();
        hop.arm_report(&like, false);
        own.send(&inbox, &like).await.unwrap();
        own.send(&inbox, &like).await.unwrap();
        let mut shim = PlugClient::connect(&sb.plug(id("claude")).unwrap().host_socket).await.unwrap();
        shim.forward(&inbox, WireMode::Send, json!({"text": like}), None, false, Some(false), false).await.unwrap();
        let mut peer = PlugClient::connect(&sb.plug_peer(id("peer-alpha")).unwrap().host_socket).await.unwrap();
        peer.forward(&inbox, WireMode::Send, json!({"text": "from alpha"}), Some(0), false, Some(false), false).await.unwrap();
        peer.send(&inbox, &like).await.unwrap();
        let flags: Vec<bool> = sb.drain_inbox(&id("parent")).iter().map(|e| e.report).collect();
        assert_eq!(flags, [false, false, true, false, false, true, false]);
        cancel.cancel();
    }

    /// marv-rs: mail is clean only when the sending Marv's code says so: Marv's own main plug while its chat's taint
    /// is clear (not a subagent's plug sharing its hop), a peer Marv's forwarded flag. A claim on Marv's own plug's socket, a shim's, or a tainted chat's isn't.
    #[tokio::test]
    async fn mail_is_clean_only_from_an_untainted_chat_or_a_peers_stamp() {
        let (sb, _log, _rt) = board();
        let sb = Arc::new(sb);
        let cancel = CancellationToken::new();
        tokio::spawn(Arc::clone(&sb).serve(cancel.clone()));
        sb.plug(id("lead")).unwrap();
        let inbox = ReceptacleName::inbox(&id("lead"));
        let hop = Arc::new(crate::hop::TurnHop::default());
        sb.set_turn_hop(id("parent"), hop.clone());
        sb.set_turn_hop(id("worker-1"), hop.clone());
        for from in ["parent", "worker-1", "claude", "peer-alpha"] {
            sb.wire(tui(from, &inbox)).unwrap();
        }
        let mut own = PlugClient::connect(&sb.plug(id("parent")).unwrap().host_socket).await.unwrap();
        let mut sub = PlugClient::connect(&sb.plug(id("worker-1")).unwrap().host_socket).await.unwrap();
        own.send(&inbox, "no taint registered").await.unwrap();
        let taint = Arc::new(marv_sandbox::TaintState::default());
        hop.watch_taint(taint.clone());
        own.send(&inbox, "clean").await.unwrap();
        sub.send(&inbox, "a subagent shares the hop, not the taint").await.unwrap();
        taint.mark(marv_core::Provenance::Web);
        own.send(&inbox, "read a web page").await.unwrap();
        own.forward(&inbox, WireMode::Send, json!({"text": "claims clean"}), None, false, None, true).await.unwrap();
        let mut shim = PlugClient::connect(&sb.plug(id("claude")).unwrap().host_socket).await.unwrap();
        shim.forward(&inbox, WireMode::Send, json!({"text": "a shim claims"}), None, false, None, true).await.unwrap();
        let mut peer = PlugClient::connect(&sb.plug_peer(id("peer-alpha")).unwrap().host_socket).await.unwrap();
        peer.forward(&inbox, WireMode::Send, json!({"text": "stamped"}), Some(0), false, None, true).await.unwrap();
        peer.send(&inbox, "unstamped").await.unwrap();
        let clean: Vec<bool> = sb.drain_inbox(&id("lead")).iter().map(|e| e.clean).collect();
        assert_eq!(clean, [false, true, false, false, false, false, true, false]);
        cancel.cancel();
    }
}
