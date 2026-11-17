//! Chats that keep running while you look at another one (#155). A chat's session lives here from its first open
//! until you delete or stop it, or the app quits: it stays on the mesh (other Marvs see it and a wired message wakes
//! it), and a turn keeps going. The page attaches to a chat while it shows it.
//!
//! Runtime-kernel 1 splits a chat in two. Its [`Resident`] record is cheap and kept while the app runs (from its first
//! open, or from the start for a chat a mesh rule covers): its folder, its mesh (switchboard, door, wires, inbox,
//! limiters), its "always" scopes, wake limit and turn hop, its approver and its events ([`Shared`], numbered across
//! loads). Its loaded session ([`LiveChat`]: the model, MCP servers, the history in memory) is what eviction drops: a
//! sleeping chat stays on the mesh under the same name, and its conversation and taint are in its session file.
//!
//! Every event gets a number (`seq`) and the last `BACKLOG` are kept whether a page is attached or not. A page
//! attaches with the last number it saved and gets what came after: the events of a turn that went on in the
//! background, a pending approval to answer, and anything sent in the moment between the page leaving and the detach.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use marv_web::protocol::ClientMessage;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;

/// Events kept per chat. Past this, the oldest go (a long unattended turn), and a page that missed them says so.
const BACKLOG: usize = 5000;
/// Chats kept loaded at once. Opening one more unloads the least recently shown chat that's idle and has nothing to
/// answer (it stays on the mesh, asleep); if every chat is busy or waiting, none is unloaded.
pub const MAX_LIVE: usize = 8;

/// Where a chat's events go while a page shows it: false once the page is gone.
pub type Sink = Box<dyn Fn(Value) -> bool + Send>;

/// A chat's state as the events tell it, and its page, if one is attached.
#[derive(Default)]
pub struct Shared {
    sink: Option<Sink>,
    /// The last events, numbered from 1.
    recent: VecDeque<Value>,
    seq: u64,
    /// The session's `ready` event: sent first to every page that attaches (never numbered or kept).
    ready: Option<Value>,
    busy: bool,
    /// Approvals asked and not answered yet, by id.
    waiting: BTreeSet<u64>,
    ended: bool,
    /// Which numbering the events have: a new one each time the app starts this chat's events afresh. A page whose
    /// saved number is from another (an earlier run of the app) gets every event kept, not those after its number.
    epoch: String,
    /// When the last event came (the harness plan, phase 2: an idle Claude Code chat sleeps a while after it).
    last: Option<Instant>,
}

impl Shared {
    /// This numbering's id, made on first use.
    pub(crate) fn epoch(&mut self) -> String {
        if self.epoch.is_empty() {
            self.epoch = uuid::Uuid::new_v4().to_string();
        }
        self.epoch.clone()
    }

    /// One event from the session: noted, numbered and kept, and sent to the page if one is attached.
    pub fn deliver(&mut self, mut event: Value) {
        self.last = Some(Instant::now());
        match event["type"].as_str() {
            Some("ready") => {
                event["epoch"] = json!(self.epoch());
                event["waiting"] = json!(self.waiting);
                self.ready = Some(event.clone());
                self.send(event);
                return;
            }
            Some("turn_start") => self.busy = true,
            Some("turn_end") => self.busy = false,
            Some("approval") => {
                if let Some(id) = event["id"].as_u64() {
                    self.waiting.insert(id);
                }
            }
            Some("approval_done") => {
                if let Some(id) = event["id"].as_u64() {
                    self.waiting.remove(&id);
                }
            }
            _ => {}
        }
        self.seq += 1;
        event["seq"] = json!(self.seq);
        if self.recent.len() == BACKLOG {
            self.recent.pop_front();
        }
        self.recent.push_back(event.clone());
        self.send(event);
    }

    /// To the attached page; false (and the page detached) if there's none or it's gone.
    fn send(&mut self, event: Value) -> bool {
        match &self.sink {
            Some(sink) if sink(event) => true,
            _ => {
                self.sink = None;
                false
            }
        }
    }

    /// A page shows the chat, having saved everything up to `after` of numbering `epoch`: its `ready` (with whether a
    /// turn is running, and this numbering's epoch), then what came after, then live events. A page with another
    /// epoch (saved before the app restarted) gets every event kept: its number means nothing here.
    pub fn attach(&mut self, sink: Sink, after: u64, epoch: Option<&str>) {
        let after = if epoch == Some(self.epoch().as_str()) { after } else { 0 };
        self.sink = Some(sink);
        if let Some(mut ready) = self.ready.clone() {
            ready["busy"] = json!(self.busy);
            // The approvals still waiting: a page's restored cards are live only for these.
            ready["waiting"] = json!(self.waiting);
            if !self.send(ready) {
                return;
            }
        }
        let first = self.seq + 1 - self.recent.len() as u64;
        let missed = first.saturating_sub(after + 1);
        if missed > 0 {
            let text = format!("{missed} earlier updates from while this chat was in the background aren't shown.");
            if !self.send(json!({"type": "notice", "text": text})) {
                return;
            }
        }
        let skip = after.saturating_sub(first - 1) as usize;
        let replay: Vec<Value> = self.recent.iter().skip(skip).cloned().collect();
        for event in replay {
            if !self.send(event) {
                return;
            }
        }
        if self.ended {
            self.send(json!({"type": "closed"}));
        }
    }

    /// Whether a page shows the chat now.
    pub fn shown(&self) -> bool {
        self.sink.is_some()
    }

    pub fn detach(&mut self) {
        self.sink = None;
    }

    /// The last event's number.
    #[cfg(test)]
    pub fn last_seq(&self) -> u64 {
        self.seq
    }

    /// Approvals waiting for the person (the session's, or its door's while it sleeps).
    pub fn waiting(&self) -> usize {
        self.waiting.len()
    }

    /// Its session was unloaded (it sleeps): the next page that attaches waits for the next load's `ready`, and no turn
    /// runs. The numbering goes on.
    pub fn unloaded(&mut self) {
        self.ready = None;
        self.busy = false;
        self.ended = false;
    }

    /// The session ended on its own (an error, or it closed).
    pub fn end(&mut self) {
        self.ended = true;
        self.busy = false;
        self.send(json!({"type": "closed"}));
    }

    pub fn idle(&self) -> bool {
        !self.busy && self.waiting.is_empty()
    }

    /// When the last event came, if any did.
    pub fn last(&self) -> Option<Instant> {
        self.last
    }
}

/// One loaded session: what eviction drops.
pub struct LiveChat {
    pub to_chat: mpsc::UnboundedSender<ClientMessage>,
    /// What the session was started with that can't change while it runs (switched-off servers, the page's reload
    /// count): a different key starts it again (it continues the same conversation).
    #[cfg_attr(not(test), allow(dead_code))]
    pub key: String,
    pub shared: Arc<Mutex<Shared>>,
    /// When a page last showed it.
    pub seen: Instant,
}

impl LiveChat {
    pub fn shared(&self) -> MutexGuard<'_, Shared> {
        lock(&self.shared)
    }

    pub fn running(&self) -> bool {
        !self.to_chat.is_closed() && !self.shared().ended
    }
}

/// A chat kept while the app runs, loaded or asleep (runtime-kernel 1, D11): everything that must survive an unload.
pub struct Resident {
    /// Its folder, as its mesh knows it (a chat whose folder moved gets a new record).
    pub folder: std::path::PathBuf,
    /// What every load of its session shares: mesh handle, "always" scopes, turn hop, approver, wake limit.
    pub keep: marv_web::host::Resident,
    /// Its switchboard and open door (None: not on the mesh). Held for its drop: with the record, it leaves the mesh.
    #[allow(dead_code)]
    pub mesh: Option<marv_session::mesh_process::Mesh>,
    pub shared: Arc<Mutex<Shared>>,
    /// Where its events go (the approver's and every load's): into `shared`.
    pub out: mpsc::UnboundedSender<Value>,
    /// The loaded session, if any.
    pub loaded: Option<LiveChat>,
    /// Which load `loaded` is (a load that ended late is told apart from the current one), and whether it said `ready`.
    pub load: u64,
    pub ready: bool,
    /// The session key of its last load (switched-off servers, the page's reload count): a load for a wake reuses it.
    pub key: String,
    /// Opened by the person this run (D13: such a chat keeps its door while it sleeps; one only a rule brought up
    /// leaves the mesh when no rule covers it any more).
    pub opened: bool,
    /// A wake the scheduler admitted or queued, run once the session is loaded (runtime-kernel 1).
    pub pending: Option<Pending>,
    /// Runtime-kernel §15.2: what waits to start its next turn, in priority order (the person's messages, fired
    /// timers; mail waits in its inbox and is looked at between them, `chats::Chats::look`).
    pub queue: marv_kernel::InterruptQueue<Queued>,
    /// The person paused it (§15.3, the Mesh panel, stop-the-team) or its unattended turn hit its deadline (§15.4):
    /// no wakes and no timers until the person resumes it. The person's own messages still run.
    pub paused: bool,
    /// An unattended turn (a wake, a timer) runs now; `true`: it started under the chat's live grant.
    pub unattended: Option<bool>,
    /// The person sent a message into the running unattended turn (review P3-7): it's theirs now.
    pub attended: Arc<std::sync::atomic::AtomicBool>,
    /// Why a wake couldn't load it, as last told to the person (told again only when it changes).
    pub cant_wake: Option<String>,
    /// A load waits for the last one's session to finish closing (its last save), so it resumes everything.
    pub gate: Arc<tokio::sync::Mutex<()>>,
    /// Wakes its watcher (a load said `ready`).
    pub kick: Arc<tokio::sync::Notify>,
    /// Ends its watcher and event pump when the record goes.
    pub stop: tokio_util::sync::CancellationToken,
    /// Run by another harness (Claude Code; the harness plan, phase 2): no mesh, no Marv session. Timers wake it
    /// through its messages ([`crate::chats`]' `look_harness`), and it sleeps when idle and unshown a while.
    pub harness: bool,
    /// A harness chat's meter (its events', for Activity's row): its door's `live` reads it.
    pub meter: Option<Arc<marv_session::live::LiveMeter>>,
    /// When its unattended turn (a timer's) started, for the deadline of a harness chat (Marv's own turns keep theirs).
    pub woke: Option<Instant>,
    /// Phase 3: the inbox turn a harness chat runs now (mail woke it), and what it did so far: its report's facts.
    pub inbox: Option<HarnessInbox>,
}

/// A harness chat's mail-woken turn: who woke it, the tools it ran, its last reply (`marv_session::task_report`).
#[derive(Debug, Clone)]
pub struct HarnessInbox {
    pub turn: marv_session::task_report::InboxTurn,
    pub calls: usize,
    pub reply: Option<String>,
}

/// A wake the scheduler admitted for a sleeping chat: mail on a wire, or a fired timer.
#[derive(Debug, Clone)]
pub enum Pending {
    Mail(marv_session::mesh::WakeBy),
    Timer(marv_session::timers::TimerWake),
}

/// What waits in a resident's interrupt queue.
#[derive(Debug, Clone)]
pub enum Queued {
    /// The person's message (plain Enter).
    Person(String),
    Timer(marv_session::timers::TimerWake),
}

impl Drop for Resident {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

impl Resident {
    /// Pauses or resumes the chat (runtime-kernel §15.3), and says so on its door, so every Activity table (this
    /// app's, another's, `marv kernel ps`) shows it paused (§20.2).
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        if let Some(mesh) = &self.mesh {
            mesh.handle.set_paused(paused);
        }
    }

    pub fn shared(&self) -> MutexGuard<'_, Shared> {
        lock(&self.shared)
    }

    /// Its session is loaded and hasn't ended.
    pub fn running(&self) -> bool {
        self.loaded.as_ref().is_some_and(LiveChat::running)
    }

    /// A message came that would wake it, but it can't load (`why`: its folder is gone, say): the person is told once
    /// in its events, and the message waits in its inbox.
    pub fn refuse_wake(&mut self, why: &str) {
        if self.cant_wake.as_deref() == Some(why) {
            return;
        }
        self.cant_wake = Some(why.to_string());
        let text = format!(
            "A message on the mesh came for this chat while it slept, but it can't wake: {}. It waits in its inbox.",
            marv_core::printable(why.trim_end_matches('.'))
        );
        let _ = self.out.send(serde_json::json!({"type": "notice", "text": text}));
    }

    /// The person's next queued message, if one waits (it goes ahead of everything else).
    pub fn take_person(&mut self) -> Option<String> {
        if self.queue.peek() != Some(marv_kernel::InterruptSource::Person) {
            return None;
        }
        match self.queue.pop() {
            Some((_, Queued::Person(text))) => Some(text),
            _ => None,
        }
    }

    /// The next fired timer, once nothing ahead of it waits in the queue.
    pub fn take_timer(&mut self) -> Option<marv_session::timers::TimerWake> {
        if self.queue.peek() != Some(marv_kernel::InterruptSource::Timer) {
            return None;
        }
        match self.queue.pop() {
            Some((_, Queued::Timer(timer))) => Some(timer),
            _ => None,
        }
    }

    /// Unloads its session (it closes, saving): the record, its door and everything in `keep` stay.
    pub fn unload(&mut self) {
        self.loaded = None;
        self.ready = false;
        self.shared().unloaded();
    }
}

pub fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

/// The chat to end so another can start: past `MAX_LIVE`, the least recently shown one that no page shows, isn't
/// running a turn and has nothing waiting for an answer. #155's rule over loaded sessions alone; the desktop now picks
/// over resident records (`chats`), with the same filter.
#[cfg(test)]
pub fn to_evict(live: &HashMap<String, LiveChat>, keep: &str) -> Option<String> {
    if live.len() < MAX_LIVE {
        return None;
    }
    live.iter()
        .filter(|(id, chat)| id.as_str() != keep && chat.shared().sink.is_none() && chat.shared().idle())
        .min_by_key(|(_, chat)| chat.seen)
        .map(|(id, _)| id.clone())
}

/// What the sidebar shows for a chat that's loaded, or asleep on the mesh.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveState {
    pub id: String,
    pub busy: bool,
    pub waiting: usize,
    /// Its session is unloaded, its door open: a message on a wire wakes it (runtime-kernel 1).
    pub asleep: bool,
    /// Why the last message that would have woken it couldn't (its folder is gone).
    pub cant_wake: Option<String>,
    /// Paused by the person or its deadline (runtime-kernel §15): no wakes or timers until resumed.
    pub paused: bool,
}

/// The loaded chats, the sleeping ones on the mesh or run by another harness, and any other with an approval waiting
/// (its door's).
pub fn states(residents: &HashMap<String, Resident>) -> Vec<LiveState> {
    residents
        .iter()
        .filter_map(|(id, chat)| {
            // Before its events' lock: `running` takes it too.
            let running = chat.running();
            let shared = chat.shared();
            (running || chat.keep.mesh.is_some() || chat.harness || shared.waiting() > 0 || chat.paused).then(|| LiveState {
                id: id.clone(),
                busy: running && shared.busy,
                waiting: shared.waiting.len(),
                asleep: !running && chat.loaded.is_none(),
                cant_wake: chat.cant_wake.clone(),
                paused: chat.paused,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    type Got = Arc<Mutex<Vec<Value>>>;

    /// A page: what it got, and whether it's still there.
    fn page() -> (Sink, Got, Arc<Mutex<bool>>) {
        let got = Arc::new(Mutex::new(Vec::new()));
        let open = Arc::new(Mutex::new(true));
        let (g, o) = (got.clone(), open.clone());
        let sink: Sink = Box::new(move |e| {
            if !*o.lock().unwrap() {
                return false;
            }
            g.lock().unwrap().push(e);
            true
        });
        (sink, got, open)
    }

    fn types(got: &Got) -> Vec<String> {
        got.lock().unwrap().iter().map(|e| e["type"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn a_page_that_comes_back_gets_what_came_after_its_last_event_and_a_waiting_approval_is_counted() {
        let mut chat = Shared::default();
        let (sink, got, open) = page();
        chat.attach(sink, 0, None);
        chat.deliver(json!({"type": "ready", "sessionId": "s"}));
        chat.deliver(json!({"type": "turn_start"}));
        chat.deliver(json!({"type": "text_delta", "text": "a"}));
        // The page saved up to 2 and left; 3 went to it as it went (lost there), 4 and 5 found it gone.
        chat.deliver(json!({"type": "text_delta", "text": "b"}));
        *open.lock().unwrap() = false;
        chat.deliver(json!({"type": "text_delta", "text": "c"}));
        chat.deliver(json!({"type": "approval", "id": 7}));
        assert!(chat.sink.is_none());
        assert_eq!((chat.busy, chat.waiting.len()), (true, 1));
        assert_eq!(types(&got), ["ready", "turn_start", "text_delta", "text_delta"]);

        let (sink, again, _) = page();
        let epoch = chat.epoch();
        chat.attach(sink, 2, Some(&epoch));
        assert_eq!(types(&again), ["ready", "text_delta", "text_delta", "approval"]);
        let again = again.lock().unwrap();
        assert_eq!(again[0]["busy"], true);
        assert_eq!(again[0]["waiting"], json!([7]));
        assert_eq!((again[1]["seq"].as_u64(), again[1]["text"].as_str()), (Some(3), Some("b")));
        drop(again);

        chat.deliver(json!({"type": "approval_done", "id": 7}));
        chat.deliver(json!({"type": "turn_end", "reason": "done"}));
        assert!(chat.idle());
    }

    #[test]
    fn a_ready_while_away_is_sent_once_and_a_new_page_gets_everything() {
        let mut chat = Shared::default();
        chat.deliver(json!({"type": "ready"}));
        chat.deliver(json!({"type": "notice", "text": "woken"}));
        let (sink, got, _) = page();
        chat.attach(sink, 0, None);
        assert_eq!(types(&got), ["ready", "notice"]);
        let (sink, up_to_date, _) = page();
        let epoch = chat.epoch();
        chat.attach(sink, 1, Some(&epoch));
        assert_eq!(types(&up_to_date), ["ready"]);
    }

    #[test]
    fn past_the_backlog_the_page_is_told_how_many_it_missed() {
        let mut chat = Shared::default();
        for i in 0..BACKLOG + 3 {
            chat.deliver(json!({"type": "text_delta", "text": i.to_string()}));
        }
        let (sink, got, _) = page();
        let epoch = chat.epoch();
        chat.attach(sink, 1, Some(&epoch));
        let got = got.lock().unwrap();
        assert_eq!(got.len(), BACKLOG + 1);
        assert!(got[0]["text"].as_str().unwrap().starts_with("2 earlier updates"), "{}", got[0]);
        assert_eq!(got[1]["text"], "3");
    }

    /// After the app restarts, a chat's numbering starts again at 1, while the page saved a number from the last run:
    /// a page with another epoch (or none) gets every event kept, and the `ready` it gets says the new epoch.
    #[test]
    fn a_page_from_an_earlier_run_gets_every_event() {
        let mut chat = Shared::default();
        chat.deliver(json!({"type": "ready"}));
        chat.deliver(json!({"type": "text_delta", "text": "new run"}));
        for epoch in [Some("an-earlier-run"), None] {
            let (sink, got, _) = page();
            chat.attach(sink, 41, epoch);
            assert_eq!(types(&got), ["ready", "text_delta"], "{epoch:?}");
            assert_eq!(got.lock().unwrap()[0]["epoch"], chat.epoch());
        }
    }

    #[test]
    fn an_ended_chat_tells_the_page_that_attaches() {
        let mut chat = Shared::default();
        chat.end();
        let (sink, got, _) = page();
        chat.attach(sink, 0, None);
        assert_eq!(types(&got), ["closed"]);
    }

    #[test]
    fn eviction_picks_the_oldest_idle_unshown_chat_and_never_a_busy_or_waiting_one() {
        let start = Instant::now();
        let mut live = HashMap::new();
        let mut senders = Vec::new();
        for i in 0..MAX_LIVE {
            let (to_chat, from) = mpsc::unbounded_channel();
            senders.push(from);
            let chat = LiveChat {
                to_chat,
                key: String::new(),
                shared: Arc::default(),
                seen: start + std::time::Duration::from_secs(i as u64),
            };
            live.insert(format!("c{i}"), chat);
        }
        // c0 is running a turn, c1 waits for an approval, c2 is on screen: c3 is the oldest that can go.
        live["c0"].shared().busy = true;
        live["c1"].shared().waiting.insert(1);
        live["c2"].shared().attach(Box::new(|_| true), 0, None);
        assert_eq!(to_evict(&live, "new").as_deref(), Some("c3"));
        assert_eq!(to_evict(&live, "c3").as_deref(), Some("c4"));
        live.remove("c7");
        assert_eq!(to_evict(&live, "new"), None, "under the cap, nothing ends");
    }
}
