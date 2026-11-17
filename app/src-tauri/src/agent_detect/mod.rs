//! Every agent chat's state (working, needs you, idle) from what its terminal shows, not only Claude Code's from its
//! hooks. Ported from herdr 0.9.5 (<https://github.com/herdrdev/herdr>, Apache-2.0: its licence is `HERDR-LICENSE`
//! beside this file): the rule engine from `src/detect/manifest.rs` ([`manifest`]), the agents' rules from
//! `src/detect/manifests/*.toml` (copied unchanged into `manifests/`, embedded, never updated over the air), and the
//! debounce from `src/pane/agent_detection.rs` (here, [`Detector`]). Marv's changes are noted where they are.
//!
//! - **The screen:** herdr reads Ghostty's visible screen; Marv keeps one of its own per chat terminal
//!   ([`screen::Screen`], fed the same output the page's terminal gets), plus the last OSC title and progress. The
//!   rules need the screen, not the output stream: the agents redraw in place. It's a small model (visible rows
//!   only, as herdr reads them; no colours), not a second terminal: an escape it doesn't know is skipped, so a rare
//!   screen may read slightly off, and the output's activity still stands behind it.
//! - **Agents with rules:** Claude Code, Codex, OpenCode, Gemini CLI, Pi, Cursor Agent, Antigravity. Marv and Crush
//!   have none and are seen from their output alone, as before (drawing: working; quiet for
//!   [`QUIET`](crate::terminal_events::QUIET): idle).
//! - **What a reading means:** a blocking rule (an approval, a trust or update question) is "needs you"
//!   (`AgentState::Waiting`); a working rule is working (`Busy`); an idle rule plainly on screen (a prompt box, an idle
//!   title) is idle. No rule (herdr's idle fallback): the output decides, working while it draws (output activity is
//!   work: a reply streaming, a spinner) and idle once it's quiet. A rule that skips (a transcript viewer, a model
//!   picker) changes nothing.
//! - **Debounce (herdr's):** nothing for [`STARTUP_GRACE`] after the terminal starts (an agent's first frames say
//!   nothing yet); working to an idle that isn't plainly on screen needs [`PENDING_IDLE_CONFIRMATIONS`] more readings
//!   [`RECHECK`] apart (at most [`PENDING_IDLE_CAP`]), so a frame between two spinner frames isn't a turn's end.
//! - **Hooks win.** An agent whose hooks report (Claude Code's turns and questions, Codex's turn ends:
//!   [`crate::terminal_events::watch`]) is told by them first: for [`HOOK_HOLD`] after a hook spoke the screen says
//!   nothing (the screen may still show the frame before). During a turn the hooks reported, the screen may only
//!   say what it plainly sees of working or needs you (a question the hooks didn't report; the person answered one,
//!   and it works again); the turn's end is the hooks' to say. Outside a hooked turn (an agent just started, one with
//!   no hooks at all) the screen decides.

mod manifest;
pub mod screen;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use marv_kernel::AgentState;

pub use manifest::{Detection, DetectionInput, Rules, State, rules_for};

/// How often a chat's screen is looked at (herdr's `AGENT_PENDING_IDLE_RECHECK`).
pub const RECHECK: Duration = Duration::from_millis(100);
/// How many more readings an idle needs after working before it's said (herdr's).
pub const PENDING_IDLE_CONFIRMATIONS: u8 = 3;
/// The longest an idle waits for those readings (herdr's `AGENT_PENDING_IDLE_CAP`).
pub const PENDING_IDLE_CAP: Duration = Duration::from_millis(700);
/// After the terminal starts, how long its screen says nothing (herdr's `AGENT_STARTUP_GRACE_WINDOW`).
pub const STARTUP_GRACE: Duration = Duration::from_secs(3);
/// After a hook spoke, how long the screen says nothing. Marv's: herdr has no hooks beside its screen rules.
pub const HOOK_HOLD: Duration = Duration::from_secs(2);
/// A changing screen is read again at most this often, unless an idle is being confirmed: a spinner redraws ten
/// times a second, and each reading runs every rule.
const REREAD: Duration = Duration::from_millis(250);

/// An idle after working, waiting for its confirmations (herdr's `PendingIdleConfirmation`).
#[derive(Debug, Default)]
struct PendingIdle {
    started_at: Option<Instant>,
    confirmations: u8,
}

impl PendingIdle {
    fn active(&self) -> bool {
        self.started_at.is_some()
    }

    fn clear(&mut self) {
        *self = PendingIdle::default();
    }

    /// Whether to hold the idle back this reading.
    fn hold(&mut self, now: Instant) -> bool {
        let Some(started_at) = self.started_at else {
            self.started_at = Some(now);
            self.confirmations = 0;
            return true;
        };
        if now.duration_since(started_at) >= PENDING_IDLE_CAP {
            self.clear();
            return false;
        }
        self.confirmations = self.confirmations.saturating_add(1);
        if self.confirmations >= PENDING_IDLE_CONFIRMATIONS {
            self.clear();
            return false;
        }
        true
    }
}

/// What the hooks last said, for one reading.
#[derive(Debug, Clone, Copy, Default)]
pub struct Hooks {
    /// A turn they reported is running (between Claude Code's `UserPromptSubmit` and `Stop`).
    pub turn: bool,
    /// The state they last said, and when.
    pub said: Option<(AgentState, Instant)>,
}

/// One chat's screen readings turned into its state: what to tell the chat, when it changes.
#[derive(Debug)]
pub struct Detector {
    grace_until: Instant,
    pending: PendingIdle,
    /// What this detector last said, and when.
    said: Option<(AgentState, Instant)>,
}

impl Detector {
    /// For a terminal that started at `started`.
    pub fn new(started: Instant) -> Self {
        Detector { grace_until: started + STARTUP_GRACE, pending: PendingIdle::default(), said: None }
    }

    /// One reading: the screen's `detection` (none: the agent has no rules), whether its output is `drawing`, and
    /// what its hooks said. The state to tell the chat, if it changed.
    pub fn observe(&mut self, detection: Option<Detection>, drawing: bool, hooks: Hooks, now: Instant) -> Option<AgentState> {
        if now < self.grace_until {
            self.pending.clear();
            return None;
        }
        let from_output = (if drawing { AgentState::Busy } else { AgentState::Idle }, false);
        // The state, and whether the screen plainly shows it.
        let next = match detection {
            Some(d) if d.skip_state_update => None,
            Some(d) if d.rule.is_some() => match d.state {
                State::Blocked => Some((AgentState::Waiting, true)),
                State::Working => Some((AgentState::Busy, true)),
                State::Idle if d.visible_idle => Some((AgentState::Idle, true)),
                State::Idle => Some(from_output),
                State::Unknown => None,
            },
            _ => Some(from_output),
        };
        let Some((next, plain)) = next else {
            self.pending.clear();
            return None;
        };
        if !screen_may_speak(next, plain, hooks, now) {
            self.pending.clear();
            return None;
        }
        // The chat's state now: whichever of the hooks and this detector spoke last.
        let current = match (self.said, hooks.said) {
            (Some(own), Some(hook)) => Some(if hook.1 > own.1 { hook.0 } else { own.0 }),
            (own, hook) => own.or(hook).map(|(state, _)| state),
        };
        if current == Some(AgentState::Busy) && next == AgentState::Idle && !plain {
            if self.pending.hold(now) {
                return None;
            }
        } else {
            self.pending.clear();
        }
        if current == Some(next) {
            return None;
        }
        self.said = Some((next, now));
        Some(next)
    }

    /// An idle is being confirmed: the screen is read on every [`RECHECK`].
    pub fn confirming(&self) -> bool {
        self.pending.active()
    }
}

/// Whether the screen's `next` (`plain`: shown by a rule, not inferred) may be said over what the hooks said.
fn screen_may_speak(next: AgentState, plain: bool, hooks: Hooks, now: Instant) -> bool {
    if hooks.said.is_some_and(|(_, at)| now.saturating_duration_since(at) < HOOK_HOLD) {
        return false;
    }
    !hooks.turn || (plain && matches!(next, AgentState::Busy | AgentState::Waiting))
}

/// Reads chat terminal `output`'s screen every [`RECHECK`] until `stop`, telling `sink` each change of state. `agent`:
/// its Marv id, for its rules.
pub async fn watch(
    agent: &str,
    output: Arc<crate::terminal_events::Output>,
    stop: Arc<AtomicBool>,
    sink: Arc<dyn crate::terminal_events::Sink>,
) {
    let rules = rules_for(agent);
    let mut detector = Detector::new(Instant::now());
    // The last reading, of which revision of the screen, and when.
    let mut read: Option<(u64, Detection, Instant)> = None;
    while !stop.load(Ordering::SeqCst) {
        let now = Instant::now();
        let detection = rules.map(|rules| {
            let fresh = read.is_some_and(|(revision, _, at)| {
                revision == output.revision() || (!detector.confirming() && now.duration_since(at) < REREAD)
            });
            if !fresh {
                let (revision, detection) = output.detect(rules);
                read = Some((revision, detection, now));
            }
            read.map_or(Detection::FALLBACK, |(_, d, _)| d)
        });
        if let Some(state) = detector.observe(detection, output.drawing(now), output.hooks(), now) {
            sink.state(state);
        }
        tokio::time::sleep(RECHECK).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(state: State, visible: bool) -> Option<Detection> {
        Some(Detection {
            state,
            rule: Some("r"),
            skip_state_update: false,
            visible_idle: visible && state == State::Idle,
            visible_blocker: visible && state == State::Blocked,
            visible_working: visible && state == State::Working,
        })
    }

    const NO_RULE: Option<Detection> = Some(Detection::FALLBACK);
    const QUIET_HOOKS: Hooks = Hooks { turn: false, said: None };

    /// After the grace, readings `at` milliseconds from the start.
    fn at(start: Instant, ms: u64) -> Instant {
        start + STARTUP_GRACE + Duration::from_millis(ms)
    }

    #[test]
    fn nothing_is_said_during_the_startup_grace() {
        let start = Instant::now();
        let mut d = Detector::new(start);
        assert_eq!(d.observe(rule(State::Blocked, true), false, QUIET_HOOKS, start + Duration::from_secs(1)), None);
        assert_eq!(d.observe(rule(State::Blocked, true), false, QUIET_HOOKS, at(start, 0)), Some(AgentState::Waiting));
        assert_eq!(d.observe(rule(State::Blocked, true), false, QUIET_HOOKS, at(start, 100)), None, "said once");
    }

    /// Working to an inferred idle (no rule, the output quiet) waits for three more readings; a plainly shown idle
    /// (a prompt box) doesn't; working again in between starts over.
    #[test]
    fn an_idle_after_working_is_confirmed_first() {
        let start = Instant::now();
        let mut d = Detector::new(start);
        assert_eq!(d.observe(rule(State::Working, true), true, QUIET_HOOKS, at(start, 0)), Some(AgentState::Busy));
        assert_eq!(d.observe(NO_RULE, false, QUIET_HOOKS, at(start, 100)), None);
        assert_eq!(d.observe(NO_RULE, false, QUIET_HOOKS, at(start, 200)), None);
        assert_eq!(d.observe(rule(State::Working, true), true, QUIET_HOOKS, at(start, 300)), None, "still working");
        assert!(!d.confirming(), "working again clears the pending idle");
        for ms in [400, 500, 600] {
            assert_eq!(d.observe(NO_RULE, false, QUIET_HOOKS, at(start, ms)), None, "{ms}");
            assert!(d.confirming());
        }
        assert_eq!(d.observe(NO_RULE, false, QUIET_HOOKS, at(start, 700)), Some(AgentState::Idle));
        // Working again, then its prompt box: idle at once.
        assert_eq!(d.observe(rule(State::Working, true), true, QUIET_HOOKS, at(start, 800)), Some(AgentState::Busy));
        assert_eq!(d.observe(rule(State::Idle, true), false, QUIET_HOOKS, at(start, 900)), Some(AgentState::Idle));
    }

    #[test]
    fn the_pending_idle_gives_up_waiting_at_its_cap() {
        let start = Instant::now();
        let mut d = Detector::new(start);
        d.observe(NO_RULE, true, QUIET_HOOKS, at(start, 0));
        assert_eq!(d.observe(NO_RULE, false, QUIET_HOOKS, at(start, 10)), None);
        assert_eq!(d.observe(NO_RULE, false, QUIET_HOOKS, at(start, 10 + 700)), Some(AgentState::Idle));
    }

    /// No rules (Marv, Crush), or none matching: drawing is working, quiet is idle; a skipping rule changes nothing.
    #[test]
    fn output_decides_when_no_rule_does() {
        let start = Instant::now();
        let mut d = Detector::new(start);
        assert_eq!(d.observe(None, true, QUIET_HOOKS, at(start, 0)), Some(AgentState::Busy));
        let skip = Some(Detection { skip_state_update: true, ..rule(State::Unknown, false).unwrap() });
        assert_eq!(d.observe(skip, false, QUIET_HOOKS, at(start, 100)), None);
        assert!(!d.confirming());
        let mut d = Detector::new(start);
        assert_eq!(d.observe(None, false, QUIET_HOOKS, at(start, 0)), Some(AgentState::Idle));
        // A blocking rule beats output: a question being drawn still needs the person.
        assert_eq!(d.observe(rule(State::Blocked, true), true, QUIET_HOOKS, at(start, 100)), Some(AgentState::Waiting));
    }

    /// Hooks win: the screen is silent for a moment after a hook spoke, and during a hooked turn it may only say
    /// working or needs you from a rule (not idle, nor anything inferred from output).
    #[test]
    fn hooks_win_over_the_screen() {
        let start = Instant::now();
        let mut d = Detector::new(start);
        let hook_at = at(start, 0);
        let turn = Hooks { turn: true, said: Some((AgentState::Busy, hook_at)) };
        // Just after the hook said working, the screen's stale idle prompt says nothing.
        assert_eq!(d.observe(rule(State::Idle, true), false, turn, at(start, 500)), None);
        let later = |ms: u64| at(start, 2000 + ms);
        // Within the turn: an idle or an inferred state from the screen is ignored...
        assert_eq!(d.observe(rule(State::Idle, true), false, turn, later(0)), None);
        assert_eq!(d.observe(NO_RULE, false, turn, later(100)), None);
        // ...a question it plainly shows is said...
        assert_eq!(d.observe(rule(State::Blocked, true), false, turn, later(200)), Some(AgentState::Waiting));
        // ...and so is working again once the person answered it.
        assert_eq!(d.observe(rule(State::Working, true), true, turn, later(300)), Some(AgentState::Busy));
        // The hooks said the turn ended (idle): the screen's last working frame doesn't undo that...
        let ended_at = later(400);
        let ended = Hooks { turn: false, said: Some((AgentState::Idle, ended_at)) };
        assert_eq!(d.observe(rule(State::Working, true), true, ended, later(500)), None);
        // ...and once the hold is over, an idle screen agrees with the hooks: nothing to say.
        assert_eq!(d.observe(rule(State::Idle, true), false, ended, ended_at + HOOK_HOLD), None);
        // A question appearing later, outside any turn (Claude Code's folder trust at start), is the screen's to say.
        assert_eq!(
            d.observe(rule(State::Blocked, true), false, ended, ended_at + HOOK_HOLD + RECHECK),
            Some(AgentState::Waiting)
        );
    }

    /// The whole loop on a real screen: Codex's approval prompt drawn into its terminal is "needs you" once the grace
    /// is over.
    #[tokio::test]
    async fn a_codex_approval_on_screen_needs_the_person() {
        use std::sync::Mutex;
        #[derive(Default)]
        struct Seen(Mutex<Vec<AgentState>>);
        impl crate::terminal_events::Sink for Seen {
            fn state(&self, state: AgentState) {
                self.0.lock().unwrap().push(state);
            }
            fn begin(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
                Box::pin(async {})
            }
            fn charge(&self, _: f64) {}
            fn end(&self) {}
        }
        let output = Arc::new(crate::terminal_events::Output::default());
        output.sized(80, 12);
        let prompt = "• I'll remove the build folder.\r\n\r\n  Would you like to run the following command?\r\n\r\n  \
                      $ rm -rf build\r\n\r\n› 1. Yes, proceed (y)\r\n  2. No, and tell Codex what to do differently \
                      (esc)\r\n\r\n  Press enter to confirm or esc to cancel";
        output.saw(prompt.as_bytes());
        let (seen, stop) = (Arc::new(Seen::default()), Arc::new(AtomicBool::new(false)));
        let started = Instant::now();
        let task = tokio::spawn(watch("codex", output, stop.clone(), seen.clone()));
        tokio::time::sleep(STARTUP_GRACE / 2).await;
        assert!(seen.0.lock().unwrap().is_empty(), "nothing during the grace");
        // A loaded test machine may be slow to compile the rules: up to ten seconds.
        while seen.0.lock().unwrap().is_empty() && started.elapsed() < Duration::from_secs(10) {
            tokio::time::sleep(RECHECK).await;
        }
        tokio::time::sleep(RECHECK * 3).await;
        stop.store(true, Ordering::SeqCst);
        task.await.unwrap();
        assert_eq!(*seen.0.lock().unwrap(), [AgentState::Waiting]);
    }
}
