//! Services: a chat that keeps working while the person is away, under the grant they made for it. New in marv-rs.
//!
//! A turn the person didn't start runs with automatic approval off. When it was started by a message on a wire a rule
//! the person made issued (and reads only such messages), and the chat has a [`ChatGrant`], the turn carries it: the
//! tool registry then runs what yolo would run (`marv_tools::ServiceGrant`), and nothing past it. Every request the
//! turn makes (its subagents' too) is charged to the grant's day ([`ChatGrant::spend`]); at the cap the grant pauses.
//!
//! Never in `marv -p` (one-shot sessions never carry one), never for a turn the radio or a hand-wired message started,
//! and never wider for a subagent: a granted turn's subagents run under the same grant.
//!
//! [`FileChatGrant`] is the desktop's: the person's `~/.marv/mesh/grants.json`, read again when it changes, and the
//! day's spend kept in a [`SpendBook`] (saved to `grant-spend.json`).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use marv_kernel::{AgentId, FileGrants, GrantDenied, GrantId, GrantStore, Spend, grant_for};

/// A chat's grant, as a session consults it. Implemented by the desktop ([`FileChatGrant`]); tests pass their own.
pub trait ChatGrant: Send + Sync {
    /// The chat's grant in force now (not revoked, not expired, under today's cap), if there is one.
    fn live(&self) -> Option<GrantId>;
    /// Whether the chat has a grant at all (live or not: expired or at its cap still counts).
    fn granted(&self) -> bool;
    /// Whether the grant is for work in `root`: inside the folder it was made for. A turn in any other folder never
    /// carries it.
    fn covers(&self, root: &std::path::Path) -> bool;
    /// A call ran under grant `grant` (as [`ChatGrant::live`] gave it) in turn `turn`: the tool's name and its audit
    /// key (`marv_tools::Tool::audit_key`: Marv's words, never the call's input).
    fn used(&self, grant: GrantId, tool: &str, audit_key: &str, turn: u32);
    /// One request of a granted turn: its cost in dollars when it has one (reported or estimated), else its tokens.
    fn spend(&self, cost_usd: Option<f64>, tokens: u64);
    /// Whether the chat's grant has expired (runtime-kernel §15.2: an unattended turn running under it is stopped;
    /// one at its spend cap only pauses at the next step). Default: never.
    fn expired(&self) -> bool {
        false
    }
}

/// A [`ChatGrant`] for one turn, as the tool registry sees it.
pub(crate) struct TurnGrant {
    pub(crate) grant: Arc<dyn ChatGrant>,
    pub(crate) turn: u32,
}

impl marv_tools::ServiceGrant for TurnGrant {
    fn live(&self) -> Option<u64> {
        self.grant.live().map(|id| id.0)
    }
    fn used(&self, grant: u64, tool: &str, audit_key: &str) {
        self.grant.used(GrantId(grant), tool, audit_key, self.turn);
    }
}

/// Why a grant stopped holding mid-task, for the person (a notice, a notification). Never reaches a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantPause {
    /// It reached today's spend cap.
    Spent(GrantId),
    /// It expired.
    Expired(GrantId),
}

/// What the person is told when a grant pauses: Marv's fixed words.
pub fn pause_text(pause: GrantPause) -> &'static str {
    match pause {
        GrantPause::Spent(_) => {
            "This chat reached today's spend cap for working while you're away: it asks you for everything until tomorrow, or until you raise the cap."
        }
        GrantPause::Expired(_) => {
            "This chat's permission to work while you're away has expired: it asks you for everything again."
        }
    }
}

/// What each grant spent today, in memory, saved after every charge. A spend file that can't be read counts every
/// grant as spent (the safe side), until the person fixes or removes it.
pub struct SpendBook {
    store: GrantStore,
    spent: Arc<Mutex<BTreeMap<GrantId, Spend>>>,
    unreadable: bool,
    saving: Arc<tokio::sync::Mutex<()>>,
    /// When each grant last ran a call, in this process (shown in the Background list).
    last_used: Mutex<BTreeMap<GrantId, u64>>,
}

impl SpendBook {
    pub fn load(store: GrantStore) -> Self {
        let (spent, unreadable) = match store.load_spend() {
            Ok(spent) => (spent, false),
            Err(_) => (BTreeMap::new(), true),
        };
        Self { store, spent: Arc::new(Mutex::new(spent)), unreadable, saving: Arc::default(), last_used: Mutex::default() }
    }

    /// `grant` ran a call at `now_ms`.
    pub fn mark_used(&self, grant: GrantId, now_ms: u64) {
        self.last_used.lock().unwrap_or_else(|e| e.into_inner()).insert(grant, now_ms);
    }

    /// When `grant` last ran a call in this process.
    pub fn last_used(&self, grant: GrantId) -> Option<u64> {
        self.last_used.lock().unwrap_or_else(|e| e.into_inner()).get(&grant).copied()
    }

    /// What `grant` spent on `now_ms`'s day.
    pub fn get(&self, grant: GrantId, now_ms: u64) -> Spend {
        if self.unreadable {
            return Spend { day: marv_kernel::day_of(now_ms), usd_micros: u64::MAX, tokens: u64::MAX };
        }
        self.spent.lock().unwrap_or_else(|e| e.into_inner()).get(&grant).copied().unwrap_or_default().today(now_ms)
    }

    /// Charges one request and saves (in the background, when there's a runtime). The spend after it. A book whose
    /// file couldn't be read never saves (it would replace what the person may still fix).
    pub fn charge(&self, grant: GrantId, now_ms: u64, cost_usd: Option<f64>, tokens: u64) -> Spend {
        let after = {
            let mut spent = self.spent.lock().unwrap_or_else(|e| e.into_inner());
            let entry = spent.entry(grant).or_default();
            entry.charge(now_ms, cost_usd, tokens);
            let after = *entry;
            // Only today's lines are kept.
            spent.retain(|_, s| s.day == after.day);
            after
        };
        if !self.unreadable
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let (store, saving, spent) = (self.store.clone(), self.saving.clone(), self.spent.clone());
            runtime.spawn(async move {
                // The book as it is once this save's turn comes, so a later save never loses to an earlier one.
                let _one = saving.lock().await;
                let all = spent.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let _ = store.save_spend(&all).await;
            });
        }
        if self.unreadable { Spend { usd_micros: u64::MAX, tokens: u64::MAX, ..after } } else { after }
    }
}

/// The desktop's [`ChatGrant`] for one chat: the person's grants file and the day's spend.
pub struct FileChatGrant {
    grants: Arc<FileGrants>,
    book: Arc<SpendBook>,
    chat: AgentId,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    on_pause: Option<Arc<dyn Fn(GrantPause) + Send + Sync>>,
    /// The kernel's event log: each call the grant ran (tool name and scope key), and its pauses.
    events: Option<Arc<dyn marv_kernel::EventLog>>,
    /// The pauses already told, so each is told once.
    told: Mutex<Vec<GrantPause>>,
}

impl FileChatGrant {
    pub fn new(grants: Arc<FileGrants>, book: Arc<SpendBook>, chat: AgentId) -> Self {
        Self { grants, book, chat, clock: Arc::new(crate::time::now_ms), on_pause: None, events: None, told: Mutex::default() }
    }

    /// Logs to the kernel's event log: `GrantUsed` for each call it ran, `GrantPaused` at the cap.
    pub fn events(mut self, log: Arc<dyn marv_kernel::EventLog>) -> Self {
        self.events = Some(log);
        self
    }

    /// Called once per grant and reason when it pauses mid-task (to tell the person).
    pub fn on_pause(mut self, f: Arc<dyn Fn(GrantPause) + Send + Sync>) -> Self {
        self.on_pause = Some(f);
        self
    }

    /// The time it decides by (tests).
    pub fn with_clock(mut self, clock: Arc<dyn Fn() -> u64 + Send + Sync>) -> Self {
        self.clock = clock;
        self
    }

    pub fn chat(&self) -> &AgentId {
        &self.chat
    }

    fn tell(&self, pause: GrantPause) {
        let mut told = self.told.lock().unwrap_or_else(|e| e.into_inner());
        if told.contains(&pause) {
            return;
        }
        told.push(pause);
        drop(told);
        if let (GrantPause::Spent(id), Some(log)) = (pause, &self.events) {
            log.append(
                (self.clock)(),
                marv_kernel::KernelEvent::GrantPaused { id, reason: marv_kernel::GrantPauseReason::Spend },
            );
        }
        if let Some(f) = &self.on_pause {
            f(pause);
        }
    }
}

impl ChatGrant for FileChatGrant {
    fn live(&self) -> Option<GrantId> {
        let snapshot = self.grants.snapshot();
        let now = (self.clock)();
        match grant_for(&snapshot, &self.chat, now, |id| self.book.get(id, now)) {
            Ok(grant) => Some(grant.id),
            Err(denied) => {
                if let Some(grant) = snapshot.for_chat(&self.chat) {
                    match denied {
                        GrantDenied::Spent => self.tell(GrantPause::Spent(grant.id)),
                        GrantDenied::Expired => self.tell(GrantPause::Expired(grant.id)),
                        GrantDenied::NoGrant => {}
                    }
                }
                None
            }
        }
    }

    fn granted(&self) -> bool {
        self.grants.snapshot().for_chat(&self.chat).is_some()
    }

    fn expired(&self) -> bool {
        let now = (self.clock)();
        self.grants.snapshot().for_chat(&self.chat).is_some_and(|g| !g.live_at(now))
    }

    fn covers(&self, root: &std::path::Path) -> bool {
        self.grants.snapshot().for_chat(&self.chat).is_some_and(|g| g.folder.contains(root))
    }

    fn used(&self, grant: GrantId, tool: &str, audit_key: &str, turn: u32) {
        let now = (self.clock)();
        self.book.mark_used(grant, now);
        if let Some(log) = &self.events {
            log.append(now, marv_kernel::KernelEvent::grant_used(grant, self.chat.clone(), tool, audit_key, turn));
        }
    }

    fn spend(&self, cost_usd: Option<f64>, tokens: u64) {
        let snapshot = self.grants.snapshot();
        let Some(grant) = snapshot.for_chat(&self.chat) else { return };
        let now = (self.clock)();
        let after = self.book.charge(grant.id, now, cost_usd, tokens);
        if grant.live_at(now) && after.over(&grant.cap, now) {
            self.tell(GrantPause::Spent(grant.id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marv_kernel::{FolderScope, GrantBy, NewGrant, SpendCap};
    use std::path::Path;

    fn chat() -> AgentId {
        AgentId::new("00000001-0000-4000-8000-000000000000").unwrap()
    }

    async fn granted(dir: &Path, home: &Path, expires_at_ms: Option<u64>) -> (GrantStore, GrantId) {
        let store = GrantStore::new(dir, home).unwrap();
        let grant = NewGrant {
            chat: chat(),
            folder: FolderScope::new(&home.join("acme"), home).unwrap(),
            expires_at_ms,
            cap: SpendCap { usd_micros: 1_000_000, tokens: 100 },
            by: GrantBy::Desktop,
            created_at_ms: 1,
            label: "acme".into(),
        };
        let (id, _) = store.add(grant).await.unwrap();
        (store, id)
    }

    #[tokio::test]
    async fn a_grant_holds_until_its_cap_or_expiry_and_each_pause_is_told_once() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let (store, id) = granted(&tmp.path().join(".marv"), &home, Some(10_000)).await;
        let now = Arc::new(std::sync::atomic::AtomicU64::new(5));
        let told = Arc::new(Mutex::new(Vec::new()));
        let clock = now.clone();
        let sink = told.clone();
        let grant =
            FileChatGrant::new(Arc::new(FileGrants::new(store.clone())), Arc::new(SpendBook::load(store.clone())), chat())
                .with_clock(Arc::new(move || clock.load(std::sync::atomic::Ordering::SeqCst)))
                .on_pause(Arc::new(move |p| sink.lock().unwrap().push(p)));
        assert_eq!(grant.live(), Some(id));
        grant.spend(Some(0.4), 0);
        grant.spend(None, 99);
        assert_eq!(grant.live(), Some(id), "under both caps");
        grant.spend(Some(0.6), 0);
        assert_eq!(grant.live(), None, "at $1: paused");
        assert_eq!(grant.live(), None);
        assert_eq!(told.lock().unwrap().clone(), [GrantPause::Spent(id)], "told once");
        // Saved, so a restart doesn't forget it.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(SpendBook::load(store.clone()).get(id, 5).usd_micros, 1_000_000);

        now.store(marv_kernel::DAY_MS + 5, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(grant.live(), None, "the next day it has expired");
        assert_eq!(told.lock().unwrap().last(), Some(&GrantPause::Expired(id)));
    }

    #[derive(Default)]
    struct Log(Mutex<Vec<marv_kernel::KernelEvent>>);
    impl marv_kernel::EventLog for Log {
        fn append(&self, _at_ms: u64, event: marv_kernel::KernelEvent) {
            self.0.lock().unwrap().push(event);
        }
    }

    #[tokio::test]
    async fn each_granted_call_and_pause_is_logged_without_the_input() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let (store, id) = granted(&tmp.path().join(".marv"), &home, None).await;
        let log = Arc::new(Log::default());
        let grant = FileChatGrant::new(Arc::new(FileGrants::new(store.clone())), Arc::new(SpendBook::load(store)), chat())
            .events(log.clone());
        grant.used(id, "bash", "bash:", 3);
        grant.spend(Some(1.0), 0);
        grant.spend(Some(1.0), 0);
        let events = log.0.lock().unwrap().clone();
        assert_eq!(
            events,
            [
                marv_kernel::KernelEvent::grant_used(id, chat(), "bash", "bash:", 3),
                marv_kernel::KernelEvent::GrantPaused { id, reason: marv_kernel::GrantPauseReason::Spend },
            ],
            "the pause once"
        );
    }

    #[tokio::test]
    async fn an_unreadable_spend_file_counts_every_grant_as_spent() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let (store, _) = granted(&tmp.path().join(".marv"), &home, None).await;
        std::fs::write(store.spend_path(), "not json").unwrap();
        let grant =
            FileChatGrant::new(Arc::new(FileGrants::new(store.clone())), Arc::new(SpendBook::load(store.clone())), chat());
        assert_eq!(grant.live(), None);
        // ... and the unreadable file is left as it was, for the person to fix.
        grant.spend(Some(0.5), 0);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(std::fs::read_to_string(store.spend_path()).unwrap(), "not json");
    }
}
