//! A Claude Code chat's terminal view, seen by Marv OS: its turns, "needs you" and cost, from the hooks and status line
//! Marv adds to its `claude` (`marv_web::claude_code::terminal_settings`), which append JSON lines to an events file.
//! Each turn is metered as the chat view's are (`marv_web::harness_budget`: today's meters, Activity's cost per hour,
//! Settings → Spending), and the chat's state follows (working, needs you, idle).
//!
//! - The cost is the status line's `cost.total_cost_usd`, the session's running total: each rise is charged to the turn
//!   that's running (the first reading is the session's starting point, charged to nothing: a resumed session carries
//!   what it cost before).
//! - The status line's reading comes after the turn's end (seen live: `Stop`, then the cost), so a turn's meter
//!   closes at the first reading after its end, or [`SETTLE`] after it at most.
//! - A turn already running isn't stopped at a budget's limit: the agent runs its own loop (as in the chat view, the
//!   next turn meets the limit; in the terminal it starts anyway, and the person is told).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use marv_kernel::AgentState;
use serde_json::{Value, json};

/// How long a turn's meter stays open after its end at most, waiting for the status line's last cost.
pub const SETTLE: Duration = Duration::from_secs(10);

/// One line of the events file, as Marv reads it.
#[derive(Debug, PartialEq)]
pub enum Event {
    TurnStart,
    /// Its turn ended; where its transcript is (Claude Code's `Stop` hook says), for its reply.
    TurnEnd(Option<PathBuf>),
    /// Its turn ended with this reply (Codex's `notify`: `agent-turn-complete`, its `last-assistant-message`).
    TurnDone(Option<String>),
    NeedsYou,
    /// The session's cost so far (the status line's).
    Cost(f64),
}

pub fn parse(line: &str) -> Option<Event> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    if v["type"] == "agent-turn-complete" {
        return Some(Event::TurnDone(v["last-assistant-message"].as_str().map(str::to_string).filter(|r| !r.trim().is_empty())));
    }
    match v["hook_event_name"].as_str() {
        Some("UserPromptSubmit") => Some(Event::TurnStart),
        Some("Stop") => Some(Event::TurnEnd(v["transcript_path"].as_str().map(PathBuf::from))),
        // Its "waiting for your input" after a while at its prompt (`idle_prompt`) asks nothing: the turn is over.
        Some("Notification") if v["notification_type"] == "idle_prompt" => None,
        Some("Notification") => Some(Event::NeedsYou),
        Some(_) => None,
        None => v["cost"]["total_cost_usd"].as_f64().filter(|c| c.is_finite() && *c >= 0.0).map(Event::Cost),
    }
}

/// The running total's rises, turned into charges: the first reading is the baseline.
#[derive(Default)]
pub struct Costs {
    seen: Option<f64>,
}

impl Costs {
    /// The rise since the last reading (none for the first, or a reading that went down: a new session).
    pub fn rise(&mut self, total: f64) -> Option<f64> {
        let rise = self.seen.map(|seen| total - seen).filter(|d| *d > 0.0);
        self.seen = Some(total);
        rise
    }
}

/// Reads the events file from where it left off.
struct Tail {
    path: PathBuf,
    at: usize,
    partial: String,
}

impl Tail {
    fn next(&mut self) -> Vec<Event> {
        let Ok(bytes) = std::fs::read(&self.path) else { return Vec::new() };
        if bytes.len() < self.at {
            self.at = 0;
        }
        self.partial.push_str(&String::from_utf8_lossy(&bytes[self.at..]));
        self.at = bytes.len();
        let mut events = Vec::new();
        while let Some(end) = self.partial.find('\n') {
            let line: String = self.partial.drain(..=end).collect();
            events.extend(parse(&line));
        }
        events
    }
}

/// The last reply's text in a Claude Code transcript (its `assistant` lines' text blocks), at most 4000 characters.
pub fn last_reply(transcript: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(transcript).ok()?;
    text.lines().rev().filter_map(|l| serde_json::from_str::<Value>(l).ok()).find_map(|v| {
        if v["type"] != "assistant" {
            return None;
        }
        let said: Vec<&str> = v["message"]["content"]
            .as_array()?
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect();
        let said = said.join("\n");
        (!said.trim().is_empty()).then(|| said.chars().take(4000).collect())
    })
}

/// What the watcher changes: the chat's state, and its meter.
pub trait Sink: Send + Sync + 'static {
    fn state(&self, state: AgentState);
    /// Its turn's reply, when its agent says where to read it.
    fn reply(&self, _text: String) {}
    fn begin(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>>;
    fn charge(&self, cost: f64);
    fn end(&self);
    /// Its hooks said a turn finished (for the person's notification).
    fn finished(&self) {}
}

/// Watches `path` until `stop`: every half second, its new lines into `sink`. The file goes when it stops. `output`:
/// the same terminal's screen, told what the hooks say and when a hooked turn runs (Claude Code is also seen from its
/// screen, [`crate::agent_detect`]: its hooks fire only on a turn, so a Claude that just started, or that showed a
/// question, said nothing at all).
pub async fn watch(path: PathBuf, stop: Arc<AtomicBool>, sink: Arc<dyn Sink>, output: Option<Arc<Output>>) {
    let turn = |on: bool| {
        if let Some(output) = &output {
            output.hooked_turn.store(on, Ordering::SeqCst);
        }
    };
    // The state the hooks say, told to the screen's reader too: the hooks win ([`crate::agent_detect`]).
    let say = |state: AgentState| {
        sink.state(state);
        if let Some(output) = &output {
            output.hook_said(state);
        }
    };
    let mut tail = Tail { path: path.clone(), at: 0, partial: String::new() };
    let mut costs = Costs::default();
    let mut open = false;
    let mut settling: Option<Instant> = None;
    loop {
        let stopping = stop.load(Ordering::SeqCst);
        for event in tail.next() {
            match event {
                Event::TurnStart => {
                    turn(true);
                    if open {
                        sink.end();
                    }
                    say(AgentState::Busy);
                    sink.begin().await;
                    (open, settling) = (true, None);
                }
                Event::NeedsYou => say(AgentState::Waiting),
                Event::TurnEnd(transcript) => {
                    turn(false);
                    if let Some(reply) = transcript.as_deref().and_then(last_reply) {
                        sink.reply(reply);
                    }
                    say(AgentState::Idle);
                    sink.finished();
                    settling = Some(Instant::now());
                }
                Event::TurnDone(reply) => {
                    turn(false);
                    if let Some(reply) = reply {
                        sink.reply(reply);
                    }
                    say(AgentState::Idle);
                    sink.finished();
                    if open {
                        sink.end();
                        open = false;
                    }
                }
                Event::Cost(total) => {
                    if let Some(rise) = costs.rise(total).filter(|_| open) {
                        sink.charge(rise);
                    }
                    // The turn's last cost came: its meter closes now.
                    if open && settling.is_some() {
                        sink.end();
                        (open, settling) = (false, None);
                    }
                }
            }
        }
        if open && (stopping || settling.is_some_and(|at| at.elapsed() >= SETTLE)) {
            sink.end();
            (open, settling) = (false, None);
        }
        if stopping {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let _ = std::fs::remove_file(&path);
}

/// What a chat's terminal shows and when it last drew, for its state ([`crate::agent_detect`]): fed by the terminal's
/// reader ([`crate::terminals`]), told by the hooks ([`watch`]).
#[derive(Default)]
pub struct Output {
    last: std::sync::Mutex<Option<Instant>>,
    /// A turn its hooks reported is running (Claude Code's): its screen can't end it, only the hooks can.
    hooked_turn: AtomicBool,
    /// What its hooks last said, and when.
    hook: std::sync::Mutex<Option<(AgentState, Instant)>>,
    /// Its screen once the terminal's size is known, and how many times it changed.
    screen: std::sync::Mutex<Option<(crate::agent_detect::screen::Screen, u64)>>,
}

impl Output {
    /// The terminal drew `bytes`.
    pub fn saw(&self, bytes: &[u8]) {
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        if let Some((screen, revision)) = self.screen.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            screen.feed(bytes);
            *revision += 1;
        }
    }

    /// The terminal's size, at its start and each resize.
    pub fn sized(&self, cols: u16, rows: u16) {
        let mut screen = self.screen.lock().unwrap_or_else(|e| e.into_inner());
        match screen.as_mut() {
            Some((screen, revision)) => {
                screen.resize(cols, rows);
                *revision += 1;
            }
            None => *screen = Some((crate::agent_detect::screen::Screen::new(cols, rows), 0)),
        }
    }

    /// Its visible screen as text now; None before its size is known.
    pub fn screen_text(&self) -> Option<String> {
        self.screen.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|(screen, _)| screen.text())
    }

    /// Its screen's revision: it changes with each output and resize.
    pub fn revision(&self) -> u64 {
        self.screen.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map_or(0, |(_, revision)| *revision)
    }

    /// `rules` read over its screen as it is now, and the screen's revision then.
    pub fn detect(&self, rules: &crate::agent_detect::Rules) -> (u64, crate::agent_detect::Detection) {
        // Copied out, the rules run after: the terminal's reader waits on this lock to feed the screen.
        let (revision, text, title, progress) = {
            let screen = self.screen.lock().unwrap_or_else(|e| e.into_inner());
            let Some((screen, revision)) = screen.as_ref() else { return (0, crate::agent_detect::Detection::FALLBACK) };
            (*revision, screen.text(), screen.title().to_string(), screen.progress().to_string())
        };
        let input = crate::agent_detect::DetectionInput { screen: &text, osc_title: &title, osc_progress: &progress };
        (revision, rules.detect(input))
    }

    /// It drew in the last [`QUIET`].
    pub fn drawing(&self, now: Instant) -> bool {
        self.last.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|at| now.saturating_duration_since(at) < QUIET)
    }

    /// What its hooks said.
    pub fn hooks(&self) -> crate::agent_detect::Hooks {
        crate::agent_detect::Hooks {
            turn: self.hooked_turn.load(Ordering::SeqCst),
            said: *self.hook.lock().unwrap_or_else(|e| e.into_inner()),
        }
    }

    fn hook_said(&self, state: AgentState) {
        *self.hook.lock().unwrap_or_else(|e| e.into_inner()) = Some((state, Instant::now()));
    }
}

/// How long a terminal stays quiet before its agent counts as idle.
pub const QUIET: Duration = Duration::from_secs(3);

/// The desktop's sink: the chat's state in [`crate::chats::Chats`], and its meter ([`marv_web::harness_budget`]).
pub struct ChatSink {
    pub chats: Arc<crate::chats::Chats>,
    pub id: String,
    pub budget: Arc<marv_web::harness_budget::HarnessBudget>,
    /// Its turns' checkpoints ([`crate::checkpoints`]), in a git repository.
    pub turns: Option<Arc<crate::checkpoints::Recorder>>,
    /// Delegates' turns, for the chat that asked ([`crate::delegates`]).
    pub delegations: Arc<crate::delegates::Delegations>,
}

impl Sink for ChatSink {
    fn state(&self, state: AgentState) {
        self.chats.terminal_state(&self.id, state);
        self.delegations.state(&self.id, state);
        // A turn runs from its agent working to its going idle; asking the person is still the turn.
        match (&self.turns, state) {
            (Some(turns), AgentState::Busy) => turns.began(),
            (Some(turns), AgentState::Idle) => turns.idle(),
            _ => {}
        }
    }
    fn reply(&self, text: String) {
        self.delegations.reply(&self.id, &text);
        self.chats.terminal_reply(&self.id, text);
    }
    fn begin(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            // Metered as a person's turn; at a limit the card asks, but the turn already runs in the terminal.
            let _ = self.budget.begin(&tokio_util::sync::CancellationToken::new()).await;
        })
    }
    fn charge(&self, cost: f64) {
        self.budget.observe(&json!({"type": "usage", "cost": cost}));
        self.chats.terminal_cost(&self.id, cost);
    }
    fn end(&self) {
        self.budget.observe(&json!({"type": "turn_end"}));
    }
    fn finished(&self) {
        self.chats.terminal_finished(&self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A question the person already answered (Claude Code's folder trust) is gone from the screen once the agent
    /// redraws, though it's still in the output's last bytes: read from the screen, it no longer holds a team's first
    /// task (it was held forever, the lead idle without its task).
    #[test]
    fn an_answered_question_is_gone_from_the_screen_but_not_the_raw_output() {
        let output = Output::default();
        output.sized(80, 24);
        let asked = "Do you trust the files in this folder?\r\n\r\n > 1. Yes, I trust this folder\r\n   2. No, exit\r\n";
        output.saw(asked.as_bytes());
        let screen = output.screen_text().unwrap();
        assert!(marv_web::terminal_agents::waiting_question(&screen).is_some(), "asked: {screen}");
        // Answered: the agent clears the screen and draws its prompt box.
        let redrawn = "\x1b[2J\x1b[H\u{256d}\u{2500}\u{2500}\u{2500}\u{256e}\r\n\u{2502} > \u{2502}\r\n\u{2570}\u{2500}\u{2500}\u{2500}\u{256f}\r\n";
        output.saw(redrawn.as_bytes());
        let screen = output.screen_text().unwrap();
        assert_eq!(marv_web::terminal_agents::waiting_question(&screen), None, "answered: {screen}");
        // The raw output still has it: what held the task before.
        let raw = format!("{asked}{redrawn}");
        assert!(marv_web::terminal_agents::waiting_question(&raw).is_some());
    }

    #[test]
    fn hook_and_status_lines_are_read_and_others_skipped() {
        assert_eq!(parse(r#"{"hook_event_name":"UserPromptSubmit","prompt":"hi"}"#), Some(Event::TurnStart));
        assert_eq!(parse(r#"{"hook_event_name":"Stop"}"#), Some(Event::TurnEnd(None)));
        assert_eq!(
            parse(r#"{"hook_event_name":"Stop","transcript_path":"/t.jsonl"}"#),
            Some(Event::TurnEnd(Some("/t.jsonl".into())))
        );
        assert_eq!(parse(r#"{"hook_event_name":"Notification","message":"needs you"}"#), Some(Event::NeedsYou));
        assert_eq!(parse(r#"{"cost":{"total_cost_usd":0.5},"model":{}}"#), Some(Event::Cost(0.5)));
        assert_eq!(parse(r#"{"hook_event_name":"PreToolUse"}"#), None);
        assert_eq!(parse("not json"), None);
        // Claude Code at its prompt a while isn't asking anything; a permission prompt is.
        let idle = r#"{"hook_event_name":"Notification","notification_type":"idle_prompt","message":"Claude is waiting for your input"}"#;
        assert_eq!(parse(idle), None);
        assert_eq!(parse(r#"{"hook_event_name":"Notification","notification_type":"permission_prompt"}"#), Some(Event::NeedsYou));
    }

    /// Codex's `notify` line (`agent-turn-complete`): its turn's end, with its last reply (an empty one is none).
    #[test]
    fn a_codex_turns_end_and_reply_are_read() {
        let line = r#"{"type":"agent-turn-complete","thread-id":"t","turn-id":"u","cwd":"/w","client":"codex-tui","input-messages":["hi"],"last-assistant-message":"Done: codex.txt"}"#;
        assert_eq!(parse(line), Some(Event::TurnDone(Some("Done: codex.txt".into()))));
        assert_eq!(parse(r#"{"type":"agent-turn-complete","last-assistant-message":" "}"#), Some(Event::TurnDone(None)));
    }

    #[test]
    fn the_last_reply_is_read_from_a_claude_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let lines = [
            r#"{"type":"user","message":{"content":"run the tests"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Running them."},{"type":"tool_use","name":"Bash"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"All 42 pass."}]}}"#,
            r#"{"type":"system","subtype":"x"}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();
        assert_eq!(last_reply(&path).as_deref(), Some("All 42 pass."));
        assert_eq!(last_reply(&dir.path().join("none.jsonl")), None);
    }

    #[derive(Default)]
    struct Seen(Mutex<Vec<String>>);
    impl Sink for Seen {
        fn state(&self, state: AgentState) {
            self.0.lock().unwrap().push(format!("{state:?}"));
        }
        fn begin(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
            self.0.lock().unwrap().push("begin".into());
            Box::pin(async {})
        }
        fn charge(&self, cost: f64) {
            self.0.lock().unwrap().push(format!("charge {cost:.2}"));
        }
        fn end(&self) {
            self.0.lock().unwrap().push("end".into());
        }
    }

    /// A resumed session's starting total is charged to nothing; each rise in a turn is charged to it, the first
    /// reading after its end included (then its meter closes); the file goes when the terminal stops.
    #[tokio::test]
    async fn a_terminals_turns_are_metered_from_its_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let lines = [
            r#"{"cost":{"total_cost_usd":1.00}}"#,
            r#"{"hook_event_name":"UserPromptSubmit"}"#,
            r#"{"cost":{"total_cost_usd":1.25}}"#,
            r#"{"hook_event_name":"Notification"}"#,
            r#"{"cost":{"total_cost_usd":1.40}}"#,
            r#"{"hook_event_name":"Stop"}"#,
            r#"{"cost":{"total_cost_usd":1.50}}"#,
            r#"{"cost":{"total_cost_usd":1.60}}"#,
        ];
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        let seen = Arc::new(Seen::default());
        let stop = Arc::new(AtomicBool::new(false));
        let task = tokio::spawn(watch(path.clone(), stop.clone(), seen.clone(), None));
        tokio::time::sleep(Duration::from_millis(1200)).await;
        stop.store(true, Ordering::SeqCst);
        task.await.unwrap();
        // The reading after the end is the turn's last (charged, then the meter closes); a later rise has no turn.
        assert_eq!(
            *seen.0.lock().unwrap(),
            ["Busy", "begin", "charge 0.25", "Waiting", "charge 0.15", "Idle", "charge 0.10", "end"]
        );
        assert!(!path.exists(), "the events file goes with the terminal");
    }

    /// What the hooks say reaches the terminal's screen reader too (so the screen holds back: the hooks win), and
    /// so does a turn they reported running.
    #[tokio::test]
    async fn the_screen_reader_hears_the_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(&path, "{\"hook_event_name\":\"UserPromptSubmit\"}\n{\"hook_event_name\":\"Notification\"}\n").unwrap();
        let output = Arc::new(Output::default());
        let stop = Arc::new(AtomicBool::new(false));
        let task = tokio::spawn(watch(path.clone(), stop.clone(), Arc::new(Seen::default()), Some(output.clone())));
        tokio::time::sleep(Duration::from_millis(300)).await;
        let hooks = output.hooks();
        assert!(hooks.turn);
        assert_eq!(hooks.said.map(|(state, _)| state), Some(AgentState::Waiting));
        std::fs::write(&path, "{\"hook_event_name\":\"UserPromptSubmit\"}\n{\"hook_event_name\":\"Notification\"}\n{\"hook_event_name\":\"Stop\"}\n")
            .unwrap();
        tokio::time::sleep(Duration::from_millis(800)).await;
        stop.store(true, Ordering::SeqCst);
        task.await.unwrap();
        let hooks = output.hooks();
        assert!(!hooks.turn);
        assert_eq!(hooks.said.map(|(state, _)| state), Some(AgentState::Idle));
    }
}
