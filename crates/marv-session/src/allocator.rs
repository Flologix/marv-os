//! The allocator's session side (runtime-kernel §16, decisions A5–A7): the meters, one `Usage` event per turn, and
//! the person's nested budgets as a turn meets them. New in marv-rs.
//!
//! A [`MeterBook`] is one per process (the desktop's, shared by its chats; the TUI's): the day's meters
//! ([`marv_kernel::Meters`]), read back from the kernel's event log (so another of the person's Marvs' turns count
//! too) plus this process's turns in flight. Each turn gets a [`TurnMeter`] when it starts: every request (its
//! subagents', its summaries' too) is charged to it as it comes back, and when the turn ends it is one
//! [`KernelEvent::Usage`] in the log: numbers and ids only, never a word of the turn.
//!
//! **Budgets** (`~/.marv/mesh/budgets.json`, [`marv_kernel::budget`]): before a turn's first request and before
//! each next one (the main agent's and its subagents'), every budget over the chat that is set is checked against
//! today's meters and the turns in flight ([`TurnMeter::gate`]):
//! - at 80% of one, the person is told once a day ([`BudgetNotice::Warning`]);
//! - at the limit, **a person's turn asks** ("acme reached its $10.00 budget for today: continue this turn?"): yes
//!   covers this turn only, no stops it; the question is `must_ask`, so no "always", yolo or pre-approval answers it,
//!   and with no one to ask (`marv -p --approve`) it stops;
//! - **an unattended turn** (a wake) pauses: it stops before the request, the person is notified, and the next turn
//!   meets the same check;
//! - **never a silent move to another core**: nothing here changes the turn's provider or model.
//!
//! A budgets file that can't be read counts as a limit reached (the safe side).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use marv_kernel::{
    BudgetCheck, BudgetScope, CoreShare, EventLog, FileBudgets, KernelEvent, MeterScope, Meters, PlanQuota, Reached, Spent,
    StartedBy, TurnUsage, day_of,
};
use marv_tools::{ApprovalRequest, Approver, Decision, Preview};

/// What the person is told about a budget (Marv's words, the person's numbers). Never reaches a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetNotice {
    /// A budget passed 80% today (once a day each).
    Warning(String),
    /// A person's turn met a limit and was asked; `continued`: they said go on (this turn only).
    Asked { text: String, continued: bool },
    /// An unattended turn met a limit and paused.
    Paused(String),
    /// A limit where no one can be asked (`marv -p`, a server): the run stopped.
    Stopped(String),
}

impl BudgetNotice {
    pub fn text(&self) -> String {
        match self {
            BudgetNotice::Warning(t) => format!("{t}."),
            BudgetNotice::Asked { text, continued: true } => format!("{text}: continuing this turn, as you said."),
            BudgetNotice::Asked { text, continued: false } => format!("{text}: stopped, as you said."),
            BudgetNotice::Stopped(t) => format!("{t}: stopped (no one here can say go on)."),
            BudgetNotice::Paused(t) => {
                format!("{t}: this chat's work while you're away paused. It goes on tomorrow, or when you raise the budget.")
            }
        }
    }
}

pub type OnBudget = Arc<dyn Fn(BudgetNotice) + Send + Sync>;

#[derive(Default)]
struct Book {
    meters: Meters,
    /// This process's turns not yet logged, by id.
    pending: BTreeMap<u64, TurnUsage>,
    /// How far each day's log file was read.
    read: BTreeMap<PathBuf, u64>,
    /// Warnings told, by day and budget (`None`: the unreadable file).
    warned: BTreeSet<(u64, Option<BudgetScope>)>,
}

/// The process's meters and the person's budgets. See the module doc.
pub struct MeterBook {
    /// The user's config folder: where the event log is read back from. None: this process's turns only.
    config_dir: Option<PathBuf>,
    events: Option<Arc<dyn EventLog>>,
    budgets: Option<Arc<FileBudgets>>,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    book: Mutex<Book>,
    /// The budgets file's store couldn't be made (a relative config folder): every budget counts as reached.
    refused: bool,
    /// The event log's writer, when this book made it (`for_person`): flushed before a one-shot run exits.
    writer: Option<Arc<crate::kernel_events::EventWriter>>,
}

impl MeterBook {
    /// `config_dir`: read the day's log back from there; `events`: where each turn's `Usage` goes; `budgets`: the
    /// person's budgets (none: no budget is checked).
    pub fn new(config_dir: Option<PathBuf>, events: Option<Arc<dyn EventLog>>, budgets: Option<Arc<FileBudgets>>) -> Self {
        Self {
            config_dir,
            events,
            budgets,
            clock: Arc::new(crate::time::now_ms),
            book: Mutex::default(),
            refused: false,
            writer: None,
        }
    }

    /// Waits for the Usage events queued so far to be written (a one-shot run, before it exits).
    pub async fn flush(&self) {
        if let Some(writer) = &self.writer {
            writer.flush().await;
        }
    }

    /// The person's own: the day's log in `config_dir` read back and written to, and their budgets there. A budgets
    /// store that can't be made fails closed (every budget counts as reached), never open.
    pub fn for_person(config_dir: &Path, home: &Path) -> Self {
        let writer = crate::kernel_events::EventWriter::new(config_dir).ok();
        let events = writer.clone().map(|l| l as Arc<dyn EventLog>);
        let book = match marv_kernel::BudgetStore::new(config_dir, home) {
            Ok(store) => Self::new(Some(config_dir.to_path_buf()), events, Some(Arc::new(FileBudgets::new(store)))),
            Err(_) => Self { refused: true, ..Self::new(Some(config_dir.to_path_buf()), events, None) },
        };
        Self { writer, ..book }
    }

    /// The time it decides by (tests).
    pub fn with_clock(mut self, clock: Arc<dyn Fn() -> u64 + Send + Sync>) -> Self {
        self.clock = clock;
        self
    }

    pub fn now(&self) -> u64 {
        (self.clock)()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Book> {
        self.book.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Reads what the day's log gained since the last read (whole lines only: a line being written waits).
    pub fn refresh(&self) {
        let Some(dir) = &self.config_dir else { return };
        let path = crate::kernel_events::events_path(dir, self.now());
        let from = self.lock().read.get(&path).copied().unwrap_or(0);
        let Some((text, upto)) = read_from(&path, from) else { return };
        let (events, _) = marv_kernel::parse_log(&text);
        let mut book = self.lock();
        for (at, event) in events {
            if let KernelEvent::Usage { usage } = event {
                book.meters.record(at, &usage);
            }
        }
        book.read.insert(path, upto);
    }

    /// The meters as they are now (today's log read back, this process's finished turns), for the Usage view.
    pub fn meters(&self) -> Meters {
        self.refresh();
        self.lock().meters.clone()
    }

    /// What `scope` spent today: the meters and the turns in flight.
    pub fn spent(&self, scope: &BudgetScope) -> Spent {
        let day = day_of(self.now());
        let book = self.lock();
        let logged = match scope {
            BudgetScope::Global => book.meters.get(day, &MeterScope::All),
            BudgetScope::Folder(f) => book.meters.folder_within(day, f.path()),
            BudgetScope::Chat(c) => book.meters.get(day, &MeterScope::Chat(c.clone())),
        };
        let mut spent = Spent { usd_micros: logged.usd_micros, tokens: logged.unpriced_tokens };
        for turn in book.pending.values() {
            let covered = match scope {
                BudgetScope::Global => true,
                BudgetScope::Folder(f) => f.contains(&turn.folder),
                BudgetScope::Chat(c) => &turn.chat == c,
            };
            if covered {
                spent.usd_micros = spent.usd_micros.saturating_add(turn.cost_micros.unwrap_or(0));
                spent.tokens = spent.tokens.saturating_add(turn.unpriced_tokens);
            }
        }
        spent
    }

    /// Every budget over `chat` in `folder`, now.
    pub fn check(&self, chat: &str, folder: &Path) -> BudgetCheck {
        if self.refused {
            return BudgetCheck { limits: vec![Reached::unreadable()], warnings: Vec::new() };
        }
        let Some(budgets) = &self.budgets else { return BudgetCheck::default() };
        match budgets.snapshot() {
            Ok(snapshot) => {
                self.refresh();
                marv_kernel::budget::check(&snapshot, chat, folder, |scope| self.spent(scope))
            }
            Err(_) => BudgetCheck { limits: vec![Reached::unreadable()], warnings: Vec::new() },
        }
    }

    /// Appends a kernel event (a core switch) to the log, when there is one.
    pub fn log(&self, event: KernelEvent) {
        let event = match event {
            KernelEvent::CoreSwitched { chat, from, to, paged_out } => {
                KernelEvent::CoreSwitched { chat, from: core_label(&from), to: core_label(&to), paged_out }
            }
            other => other,
        };
        if let Some(events) = &self.events {
            events.append(self.now(), event);
        }
    }

    /// Starts a turn's meter.
    pub fn begin(self: &Arc<Self>, turn: TurnStart) -> Arc<TurnMeter> {
        let id = random_id();
        let usage = TurnUsage {
            id,
            chat: turn.chat.clone(),
            folder: turn.folder.clone(),
            core: core_label(&turn.core),
            grant: turn.grant,
            requests: 0,
            tokens_in: 0,
            cached: 0,
            out: 0,
            cost_micros: None,
            estimated: false,
            unpriced_tokens: 0,
            wall_ms: 0,
            started_by: turn.started_by,
            cores: Vec::new(),
            quota: None,
        };
        self.lock().pending.insert(id, usage);
        Arc::new(TurnMeter {
            book: self.clone(),
            id,
            chat: turn.chat,
            folder: turn.folder,
            unattended: turn.started_by == StartedBy::Wake,
            started_ms: self.now(),
            allowed: Mutex::default(),
            done: Mutex::new(false),
            asking: tokio::sync::Mutex::new(()),
            refused: std::sync::atomic::AtomicBool::new(false),
        })
    }
}

/// What a turn's meter starts with.
pub struct TurnStart {
    pub chat: String,
    pub folder: PathBuf,
    /// The chat's core, `provider/model`.
    pub core: String,
    pub grant: Option<marv_kernel::GrantId>,
    pub started_by: StartedBy,
}

/// One request, as it's charged.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Charge {
    pub tokens_in: u64,
    pub cached: u64,
    pub out: u64,
    /// Its cost in dollars (reported, or estimated: `estimated`), or none.
    pub cost_usd: Option<f64>,
    pub estimated: bool,
}

/// One turn's meter. See the module doc.
pub struct TurnMeter {
    book: Arc<MeterBook>,
    id: u64,
    chat: String,
    folder: PathBuf,
    unattended: bool,
    started_ms: u64,
    /// Budgets the person said to go past, this turn only (`None`: the unreadable file).
    allowed: Mutex<Vec<Option<BudgetScope>>>,
    done: Mutex<bool>,
    /// One question at a time: parallel subagents meeting the same limit get one question between them.
    asking: tokio::sync::Mutex<()>,
    /// The person said no (or the turn paused): every later request of the turn stops without asking again.
    refused: std::sync::atomic::AtomicBool,
}

impl TurnMeter {
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Charges one request on `core` (`provider/model`).
    pub fn charge(&self, core: &str, charge: Charge) {
        let core = core_label(core);
        let core = core.as_str();
        let micros = charge.cost_usd.filter(|c| c.is_finite() && *c >= 0.0).map(|c| (c * 1_000_000.0).ceil() as u64);
        let mut book = self.book.lock();
        let Some(turn) = book.pending.get_mut(&self.id) else { return };
        let tokens = charge.tokens_in.saturating_add(charge.out);
        turn.requests = turn.requests.saturating_add(1);
        turn.tokens_in = turn.tokens_in.saturating_add(charge.tokens_in);
        turn.cached = turn.cached.saturating_add(charge.cached);
        turn.out = turn.out.saturating_add(charge.out);
        match micros {
            Some(m) => {
                turn.cost_micros = Some(turn.cost_micros.unwrap_or(0).saturating_add(m));
                turn.estimated |= charge.estimated;
            }
            None => turn.unpriced_tokens = turn.unpriced_tokens.saturating_add(tokens),
        }
        if core != turn.core {
            let share = match turn.cores.iter_mut().find(|s| s.core == core) {
                Some(share) => share,
                None => {
                    turn.cores.push(CoreShare {
                        core: core.into(),
                        requests: 0,
                        tokens: 0,
                        cost_micros: None,
                        unpriced_tokens: 0,
                    });
                    turn.cores.last_mut().expect("just pushed")
                }
            };
            share.requests = share.requests.saturating_add(1);
            share.tokens = share.tokens.saturating_add(tokens);
            match micros {
                Some(m) => share.cost_micros = Some(share.cost_micros.unwrap_or(0).saturating_add(m)),
                None => share.unpriced_tokens = share.unpriced_tokens.saturating_add(tokens),
            }
        }
    }

    /// The plan core's quota as its last response's headers told it.
    pub fn quota(&self, quota: PlanQuota) {
        if let Some(turn) = self.book.lock().pending.get_mut(&self.id) {
            turn.quota = Some(quota);
        }
    }

    /// Whether the next request may run (see the module doc). `approver`: who asks the person in their turn (none: no
    /// one to ask, so a limit stops it); `stop` ends a wait for the answer. Notices go to `notify`.
    pub async fn gate(
        &self,
        approver: Option<&Arc<dyn Approver>>,
        stop: &tokio_util::sync::CancellationToken,
        notify: &OnBudget,
    ) -> bool {
        use std::sync::atomic::Ordering;
        if self.refused.load(Ordering::SeqCst) {
            return false;
        }
        let _one = self.asking.lock().await;
        if self.refused.load(Ordering::SeqCst) {
            return false;
        }
        let ok = self.gate_once(approver, stop, notify).await;
        if !ok {
            self.refused.store(true, Ordering::SeqCst);
        }
        ok
    }

    async fn gate_once(
        &self,
        approver: Option<&Arc<dyn Approver>>,
        stop: &tokio_util::sync::CancellationToken,
        notify: &OnBudget,
    ) -> bool {
        let check = self.book.check(&self.chat, &self.folder);
        let day = day_of(self.book.now());
        for warning in &check.warnings {
            let first = self.book.lock().warned.insert((day, warning.scope.clone()));
            if first {
                notify(BudgetNotice::Warning(warning.warning_text()));
            }
        }
        let new: Vec<Reached> = {
            let allowed = self.allowed.lock().unwrap_or_else(|e| e.into_inner());
            check.limits.into_iter().filter(|r| !allowed.contains(&r.scope)).collect()
        };
        let Some(first) = new.first() else { return true };
        let text = first.text();
        let log = |unattended: bool| {
            if let Some(events) = &self.book.events {
                for reached in &new {
                    events.append(
                        self.book.now(),
                        KernelEvent::BudgetReached { chat: self.chat.clone(), scope: reached.scope.clone(), unattended },
                    );
                }
            }
        };
        if self.unattended {
            log(true);
            notify(BudgetNotice::Paused(text));
            return false;
        }
        log(false);
        let Some(approver) = approver else {
            notify(BudgetNotice::Stopped(text));
            return false;
        };
        let answer = tokio::select! {
            d = approver.approve(budget_request(&text)) => d,
            _ = stop.cancelled() => Decision::No,
        };
        let continued = matches!(answer, Decision::Yes | Decision::Always);
        notify(BudgetNotice::Asked { text, continued });
        if continued {
            self.allowed.lock().unwrap_or_else(|e| e.into_inner()).extend(new.into_iter().map(|r| r.scope));
        }
        continued
    }

    /// The turn ended: its `Usage` goes into the meters and the event log (once; a turn with no request logs none).
    pub fn finish(&self) {
        if std::mem::replace(&mut *self.done.lock().unwrap_or_else(|e| e.into_inner()), true) {
            return;
        }
        let now = self.book.now();
        let usage = {
            let mut book = self.book.lock();
            let Some(mut usage) = book.pending.remove(&self.id) else { return };
            usage.wall_ms = now.saturating_sub(self.started_ms);
            if usage.requests == 0 {
                return;
            }
            book.meters.record(now, &usage);
            usage
        };
        if let Some(events) = &self.book.events {
            events.append(now, KernelEvent::Usage { usage });
        }
    }
}

impl Drop for TurnMeter {
    fn drop(&mut self) {
        self.finish();
    }
}

/// The question a person's turn gets at a limit: `must_ask` and `once`, so nothing but the person answers it.
pub fn budget_request(text: &str) -> ApprovalRequest {
    ApprovalRequest {
        reason: false,
        tool: "budget".into(),
        label: "budget".into(),
        preview: Preview {
            title: format!("{text}. Continue this turn?"),
            note: Some(
                "yes covers this turn only; to raise the budget, use Marv desktop's Usage view or `marv kernel budget`; no stops here"
                    .into(),
            ),
            ..Preview::default()
        },
        scope: marv_tools::Scope { key: "budget".into(), description: "a budget's limit".into() },
        agent: None,
        network: false,
        once: true,
        must_ask: true,
        // Not a write later conversations load (#220's flag): `must_ask` and `once` already keep it the person's.
        persists: false,
        protected: None,
        door: None,
    }
}

/// The text after byte `from` up to the last whole line, and where that ends. None: nothing new (or no file).
fn read_from(path: &Path, from: u64) -> Option<(String, u64)> {
    use marv_fs::mode::NoFollow;
    use std::io::{Read, Seek, SeekFrom};
    // Never through a symlink, and only a file others can't write (the writer makes it 0600): what it says is spend.
    let mut file = std::fs::OpenOptions::new().read(true).no_follow().open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || marv_fs::mode::mode_of(&meta.permissions()) & 0o022 != 0 {
        return None;
    }
    let len = meta.len();
    if len <= from {
        return None;
    }
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut bytes = Vec::new();
    file.take(len - from).read_to_end(&mut bytes).ok()?;
    let end = bytes.iter().rposition(|b| *b == b'\n')? + 1;
    bytes.truncate(end);
    Some((String::from_utf8_lossy(&bytes).into_owned(), from + end as u64))
}

fn random_id() -> u64 {
    use std::hash::BuildHasher;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    std::collections::hash_map::RandomState::new().hash_one(nanos)
}

/// A core's name as the meters keep it: `provider/model` in the characters model ids use (letters, digits, `._:/@+-`),
/// at most 120 of them; anything else is `_`. A model name is config text, but what reaches the event log stays
/// a name, never a sentence.
pub fn core_label(name: &str) -> String {
    name.chars().take(120).map(|c| if c.is_ascii_alphanumeric() || "._:/@+-".contains(c) { c } else { '_' }).collect()
}

/// `MARV_METER_CHAT` (runtime-kernel §16.5: set by the Herdr door for a worker it spawns) from the real environment:
/// the chat its spend is charged to, only when it's a chat id (`AgentId::new`).
pub fn meter_chat_from_env(env: &crate::config::Env) -> Option<String> {
    env.get("MARV_METER_CHAT").and_then(|v| marv_kernel::AgentId::new(v.trim())).map(|id| id.as_str().to_string())
}

/// The meters in the log files for the `days` days up to `now_ms` (`marv kernel usage`, the Usage view's week).
pub fn meters_from_log(config_dir: &Path, now_ms: u64, days: u64) -> Meters {
    let mut meters = Meters::default();
    for back in (0..days).rev() {
        let at = now_ms.saturating_sub(back * marv_kernel::DAY_MS);
        let Some((text, _)) = read_from(&crate::kernel_events::events_path(config_dir, at), 0) else { continue };
        for (at, event) in marv_kernel::parse_log(&text).0 {
            if let KernelEvent::Usage { usage } = event {
                meters.record(at, &usage);
            }
        }
    }
    meters
}

#[cfg(test)]
mod tests {
    use super::*;
    use marv_kernel::{BudgetStore, SpendCap};
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Default)]
    struct Log(Mutex<Vec<KernelEvent>>);
    impl EventLog for Log {
        fn append(&self, _at: u64, event: KernelEvent) {
            self.0.lock().unwrap().push(event);
        }
    }

    struct Answer(Decision, Mutex<Vec<ApprovalRequest>>);
    #[async_trait::async_trait]
    impl Approver for Answer {
        async fn approve(&self, request: ApprovalRequest) -> Decision {
            self.1.lock().unwrap().push(request);
            self.0.clone()
        }
    }

    fn start(chat: &str, wake: bool) -> TurnStart {
        TurnStart {
            chat: chat.into(),
            folder: "/home/me/acme".into(),
            core: "openrouter/big".into(),
            grant: None,
            started_by: if wake { StartedBy::Wake } else { StartedBy::Person },
        }
    }

    fn dollars(usd: f64) -> Charge {
        Charge { tokens_in: 100, cached: 0, out: 10, cost_usd: Some(usd), estimated: false }
    }

    #[test]
    fn a_turn_is_one_usage_event_with_its_requests_and_cores() {
        let log = Arc::new(Log::default());
        let book = Arc::new(MeterBook::new(None, Some(log.clone()), None).with_clock(Arc::new(|| 5)));
        let turn = book.begin(start("c1", false));
        turn.charge("openrouter/big", dollars(0.5));
        turn.charge("zai/glm", Charge { tokens_in: 50, out: 5, ..Charge::default() });
        assert_eq!(book.spent(&BudgetScope::Global), Spent { usd_micros: 500_000, tokens: 55 }, "in flight counts");
        turn.finish();
        turn.finish();
        let events = log.0.lock().unwrap().clone();
        assert_eq!(events.len(), 1, "one per turn");
        let KernelEvent::Usage { usage } = &events[0] else { panic!("{events:?}") };
        assert_eq!((usage.requests, usage.cost_micros, usage.unpriced_tokens), (2, Some(500_000), 55));
        assert_eq!(
            usage.cores,
            [CoreShare { core: "zai/glm".into(), requests: 1, tokens: 55, cost_micros: None, unpriced_tokens: 55 }]
        );
        assert_eq!(book.spent(&BudgetScope::Chat("c1".into())).usd_micros, 500_000, "counted once, after it ended too");
        drop(book.begin(start("c2", false)));
        assert_eq!(log.0.lock().unwrap().len(), 1, "a turn with no request logs nothing");
    }

    #[tokio::test]
    async fn a_limit_asks_a_person_once_a_turn_and_pauses_a_wake() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let store = BudgetStore::new(&tmp.path().join(".marv"), &home).unwrap();
        store.set(BudgetScope::Chat("c1".into()), Some(SpendCap { usd_micros: 1_000_000, tokens: 1_000_000 }), 1).await.unwrap();
        let now = Arc::new(AtomicU64::new(5));
        let clock = now.clone();
        let log = Arc::new(Log::default());
        let book = Arc::new(
            MeterBook::new(None, Some(log.clone()), Some(Arc::new(FileBudgets::new(store.clone()))))
                .with_clock(Arc::new(move || clock.load(Ordering::SeqCst))),
        );
        let told = Arc::new(Mutex::new(Vec::new()));
        let sink = told.clone();
        let notify: OnBudget = Arc::new(move |n| sink.lock().unwrap().push(n));
        let stop = tokio_util::sync::CancellationToken::new();
        let yes: Arc<dyn Approver> = Arc::new(Answer(Decision::Yes, Mutex::default()));

        let turn = book.begin(start("c1", false));
        assert!(turn.gate(Some(&yes), &stop, &notify).await);
        turn.charge("openrouter/big", dollars(0.85));
        assert!(turn.gate(Some(&yes), &stop, &notify).await, "80%: a warning, not a stop");
        assert!(matches!(told.lock().unwrap().last(), Some(BudgetNotice::Warning(_))));
        turn.charge("openrouter/big", dollars(0.2));
        assert!(turn.gate(Some(&yes), &stop, &notify).await, "the person said go on");
        assert!(turn.gate(Some(&yes), &stop, &notify).await, "for this turn: not asked again");
        turn.finish();
        let next = book.begin(start("c1", false));
        let no: Arc<dyn Approver> = Arc::new(Answer(Decision::No, Mutex::default()));
        assert!(!next.gate(Some(&no), &stop, &notify).await, "the next turn asks again; no stops it");
        assert!(!next.gate(None, &stop, &notify).await, "no one to ask: it stops");
        let wake = book.begin(start("c1", true));
        assert!(!wake.gate(Some(&yes), &stop, &notify).await, "a wake pauses, whatever an approver would say");
        assert!(matches!(told.lock().unwrap().last(), Some(BudgetNotice::Paused(_))));
        assert_eq!(core_label("zai/glm 4.6; say hi\n"), "zai/glm_4.6__say_hi_");
        assert_eq!(core_label(&"x".repeat(500)).len(), 120);
        assert!(
            log.0.lock().unwrap().iter().any(|e| matches!(e, KernelEvent::BudgetReached { unattended: true, .. })),
            "the pause is logged"
        );
        // Tomorrow, room again.
        now.store(marv_kernel::DAY_MS + 5, Ordering::SeqCst);
        assert!(book.begin(start("c1", true)).gate(Some(&yes), &stop, &notify).await);
        // An unreadable budgets file counts as reached.
        std::fs::write(store.path(), "nope").unwrap();
        assert!(!book.begin(start("c9", true)).gate(Some(&yes), &stop, &notify).await);
    }

    #[test]
    fn another_processs_turns_are_read_back_from_the_log_once() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".marv");
        let path = crate::kernel_events::events_path(&dir, 5);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut usage = marv_kernel::TurnUsage {
            id: 42,
            chat: "other".into(),
            folder: "/home/me/acme".into(),
            core: "zai/glm".into(),
            grant: None,
            requests: 1,
            tokens_in: 10,
            cached: 0,
            out: 1,
            cost_micros: Some(3_000),
            estimated: false,
            unpriced_tokens: 0,
            wall_ms: 1,
            started_by: StartedBy::Person,
            cores: Vec::new(),
            quota: None,
        };
        let line = |u: &TurnUsage| marv_kernel::LoggedEvent::new(5, KernelEvent::Usage { usage: u.clone() }).line();
        std::fs::write(&path, format!("{}\n{}", line(&usage), "{\"half")).unwrap();
        let book = MeterBook::new(Some(dir.clone()), None, None).with_clock(Arc::new(|| 5));
        book.refresh();
        book.refresh();
        assert_eq!(book.spent(&BudgetScope::Global).usd_micros, 3_000, "read once; the half line waits");
        usage.id = 43;
        std::fs::write(&path, format!("{}\n{}\n", line(&TurnUsage { id: 42, ..usage.clone() }), line(&usage))).unwrap();
        book.refresh();
        assert_eq!(book.spent(&BudgetScope::Global).usd_micros, 6_000);
        assert_eq!(meters_from_log(&dir, 5, 7).get(0, &MeterScope::All).usd_micros, 6_000);
    }
}
