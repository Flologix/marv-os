//! What the Activity table measures per agent (runtime-kernel §20.9, decision O10). New in marv-rs.
//!
//! A task manager's numbers, mapped to an agent:
//! - **CPU %** is the share of the last minute ([`WINDOW_MS`]) spent in model calls, and the split user / system /
//!   iowait is **model / tools / waiting for you**: blocked on an approval is I/O wait, the slow device is the person.
//!   [`Meter`] keeps the phases as spans; nested approvals and parallel tool calls count once.
//! - **Clock speed** is output tokens per second over the streamed requests of the last minute.
//! - **Cache hit rate** is the prompt cache's: cached ÷ prompt tokens over the last ten minutes ([`POWER_WINDOW_MS`]).
//! - **Power draw** is $ per hour over the last ten minutes.
//! - **Running hot**: retries, and of those throttles (a 429), over the last ten minutes, and the provider's rate-limit
//!   headroom when its headers said ([`RateLimit`]).
//!
//! Pure: time is passed in (ms), no clock, no I/O. A session feeds it from its turn's events; `marv/info` carries the
//! [`Metrics`] snapshot (fixed shapes, integers only) to `marv kernel ps` and the desktop's Activity tab. For the person
//! only: nothing here reaches a model (guard `activity_never_reaches_a_model`).

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// The CPU window: the last minute.
pub const WINDOW_MS: u64 = 60_000;
/// The power window ($/h, cache hit %, throttles): the last ten minutes.
pub const POWER_WINDOW_MS: u64 = 10 * 60_000;
/// Requests kept for the power window (an agent makes far fewer in ten minutes).
const MAX_REQUESTS: usize = 512;

/// What an agent is doing right now, as the meter splits time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// No turn.
    Idle,
    /// A request to the model is streaming (or about to be sent).
    Model,
    /// A tool call runs.
    Tools,
    /// An approval waits for the person.
    You,
}

/// A provider's rate limit, from its response headers when it sends them: the limit and what's left, per minute
/// (OpenAI's `x-ratelimit-*`, Anthropic's `anthropic-ratelimit-*`, OpenRouter's `x-ratelimit-*`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests_limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests_left: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_left: Option<u64>,
}

impl RateLimit {
    /// How much of the limit is used, in %: the busier of requests and tokens. `None` when the headers said neither.
    pub fn used_pct(&self) -> Option<u8> {
        let used = |limit: Option<u64>, left: Option<u64>| {
            let limit = limit.filter(|l| *l > 0)?;
            let left = left?.min(limit);
            Some((((limit - left) as f64 / limit as f64) * 100.0).round() as u8)
        };
        match (used(self.requests_limit, self.requests_left), used(self.tokens_limit, self.tokens_left)) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }
}

/// One request's numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Request {
    at_ms: u64,
    prompt: u64,
    cached: u64,
    completion: u64,
    /// How long it streamed (since its model phase began); 0 for one this agent didn't stream (a subagent's).
    stream_ms: u64,
}

/// One agent's numbers over its last minute and ten minutes ([`Meter::snapshot`]). Integers only, so it compares and
/// serializes exactly; `marv/info` carries it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metrics {
    /// Share of the last minute in model calls, tools and waiting for the person (each 0–100; together ≤ 100).
    pub model_pct: u8,
    pub tools_pct: u8,
    pub you_pct: u8,
    /// Output tokens per second over the last minute's requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_per_s: Option<u32>,
    /// Prompt-cache hit %, over the last ten minutes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_hit_pct: Option<u8>,
    /// $ per hour over the last ten minutes, in micro-dollars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usd_micros_per_hour: Option<u64>,
    /// Requests in the last minute.
    #[serde(default)]
    pub requests_per_min: u32,
    /// Retries in the last ten minutes, and of those, throttles (the provider's 429).
    #[serde(default)]
    pub retries: u32,
    #[serde(default)]
    pub throttles: u32,
    /// The provider's rate limit as its last response's headers said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<RateLimit>,
}

/// One agent's loop timers and request meters. Fed in order with the times things happened.
#[derive(Debug, Clone)]
pub struct Meter {
    in_turn: bool,
    tools: u32,
    waiting: u32,
    phase: Phase,
    since_ms: u64,
    /// When the current model phase began (a request's stream time).
    model_since_ms: Option<u64>,
    /// Closed spans of the last minute (start, end, phase), oldest first; Idle spans aren't kept.
    spans: VecDeque<(u64, u64, Phase)>,
    requests: VecDeque<Request>,
    /// Retries of the power window (at, throttled).
    retries: VecDeque<(u64, bool)>,
    rate_limit: Option<RateLimit>,
    /// What requests cost, in the power window (at, micro-dollars): the main agent's, its subagents' and its summaries'.
    charges: VecDeque<(u64, u64)>,
    /// Today's spend (the UTC day, micro-dollars) by this meter: this load of the session, never a restored total.
    today: (u64, u64),
}

impl Default for Meter {
    fn default() -> Self {
        Self::new()
    }
}

impl Meter {
    pub fn new() -> Self {
        Self {
            in_turn: false,
            tools: 0,
            waiting: 0,
            phase: Phase::Idle,
            since_ms: 0,
            model_since_ms: None,
            spans: VecDeque::new(),
            requests: VecDeque::new(),
            retries: VecDeque::new(),
            rate_limit: None,
            charges: VecDeque::new(),
            today: (0, 0),
        }
    }

    /// What it's doing now.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    fn resolve(&self) -> Phase {
        if self.waiting > 0 {
            Phase::You
        } else if self.tools > 0 {
            Phase::Tools
        } else if self.in_turn {
            Phase::Model
        } else {
            Phase::Idle
        }
    }

    /// Closes the running span at `now` and starts the next phase.
    fn update(&mut self, now: u64) {
        let next = self.resolve();
        if next == self.phase {
            return;
        }
        if self.phase != Phase::Idle && now > self.since_ms {
            self.spans.push_back((self.since_ms, now, self.phase));
        }
        if next == Phase::Model {
            self.model_since_ms = Some(now);
        }
        self.phase = next;
        self.since_ms = now;
        self.trim(now);
    }

    fn trim(&mut self, now: u64) {
        let cpu_from = now.saturating_sub(WINDOW_MS);
        while self.spans.front().is_some_and(|(_, end, _)| *end <= cpu_from) {
            self.spans.pop_front();
        }
        let power_from = now.saturating_sub(POWER_WINDOW_MS);
        while self.requests.front().is_some_and(|r| r.at_ms < power_from) || self.requests.len() > MAX_REQUESTS {
            self.requests.pop_front();
        }
        while self.retries.front().is_some_and(|(at, _)| *at < power_from) || self.retries.len() > MAX_REQUESTS {
            self.retries.pop_front();
        }
        while self.charges.front().is_some_and(|(at, _)| *at < power_from) || self.charges.len() > MAX_REQUESTS {
            self.charges.pop_front();
        }
    }

    /// A turn started: its first request.
    pub fn turn_start(&mut self, now: u64) {
        self.in_turn = true;
        self.update(now);
    }

    /// The turn ended (however): nothing runs or waits any more.
    pub fn turn_end(&mut self, now: u64) {
        self.in_turn = false;
        self.tools = 0;
        self.waiting = 0;
        self.update(now);
    }

    /// A tool call started (calls in parallel each start).
    pub fn tool_start(&mut self, now: u64) {
        self.tools += 1;
        self.update(now);
    }

    /// A tool call ended (or was answered without running).
    pub fn tool_end(&mut self, now: u64) {
        self.tools = self.tools.saturating_sub(1);
        self.update(now);
    }

    /// An approval is asked of the person.
    pub fn wait_start(&mut self, now: u64) {
        self.waiting += 1;
        self.update(now);
    }

    /// The person answered it (or the turn stopped).
    pub fn wait_end(&mut self, now: u64) {
        self.waiting = self.waiting.saturating_sub(1);
        self.update(now);
    }

    /// A request's usage arrived: its tokens. `streamed`: this agent streamed it (its stream time counts for tokens/s);
    /// a subagent's request counts for the cache only.
    pub fn request(&mut self, now: u64, prompt: u64, cached: u64, completion: u64, streamed: bool) {
        let stream_ms = if streamed { self.model_since_ms.map_or(0, |since| now.saturating_sub(since)) } else { 0 };
        let completion = if streamed { completion } else { 0 };
        self.requests.push_back(Request { at_ms: now, prompt, cached: cached.min(prompt), completion, stream_ms });
        // The next request (a retry, the next step with no tool call) streams from here.
        if self.phase == Phase::Model {
            self.model_since_ms = Some(now);
        }
        self.trim(now);
    }

    /// What a request cost (micro-dollars: reported, or estimated from prices as the totals are): $/h and today's
    /// spend.
    pub fn charge(&mut self, now: u64, usd_micros: u64) {
        if usd_micros == 0 {
            return;
        }
        self.charges.push_back((now, usd_micros));
        let day = crate::day_of(now);
        self.today = if self.today.0 == day { (day, self.today.1.saturating_add(usd_micros)) } else { (day, usd_micros) };
        self.trim(now);
    }

    /// Today's spend by this meter (0 on a new UTC day).
    pub fn spent_today(&self, now: u64) -> u64 {
        if self.today.0 == crate::day_of(now) { self.today.1 } else { 0 }
    }

    /// The provider is retrying; `throttled`: it answered 429.
    pub fn retry(&mut self, now: u64, throttled: bool) {
        self.retries.push_back((now, throttled));
        self.trim(now);
    }

    /// The provider's last response's rate-limit headers.
    pub fn rate_limit(&mut self, limit: RateLimit) {
        self.rate_limit = Some(limit);
    }

    /// The numbers at `now`.
    pub fn snapshot(&self, now: u64) -> Metrics {
        let from = now.saturating_sub(WINDOW_MS);
        let span = now - from;
        let mut spent = [0u64; 3];
        let open = (self.phase != Phase::Idle).then_some((self.since_ms, now, self.phase));
        for (start, end, phase) in self.spans.iter().copied().chain(open) {
            let (start, end) = (start.max(from), end.min(now));
            if end <= start {
                continue;
            }
            match phase {
                Phase::Model => spent[0] += end - start,
                Phase::Tools => spent[1] += end - start,
                Phase::You => spent[2] += end - start,
                Phase::Idle => {}
            }
        }
        let pct = |ms: u64| if span == 0 { 0 } else { ((ms as f64 / span as f64) * 100.0).round().min(100.0) as u8 };
        let minute: Vec<&Request> = self.requests.iter().filter(|r| r.at_ms >= from).collect();
        let (out, ms) =
            minute.iter().fold((0u64, 0u64), |(t, m), r| (t.saturating_add(r.completion), m.saturating_add(r.stream_ms)));
        let tokens_per_s = (ms > 0).then(|| ((out as f64 * 1000.0) / ms as f64).round() as u32);
        let power_from = now.saturating_sub(POWER_WINDOW_MS);
        let power: Vec<&Request> = self.requests.iter().filter(|r| r.at_ms >= power_from).collect();
        let (prompt, cached) =
            power.iter().fold((0u64, 0u64), |(p, c), r| (p.saturating_add(r.prompt), c.saturating_add(r.cached)));
        let cache_hit_pct = (prompt > 0).then(|| ((cached as f64 / prompt as f64) * 100.0).round() as u8);
        let charged: Vec<&(u64, u64)> = self.charges.iter().filter(|(at, _)| *at >= power_from).collect();
        let usd: u64 = charged.iter().map(|(_, c)| *c).fold(0, u64::saturating_add);
        // Over the window, or since the first charge when that's sooner (a new agent isn't averaged over ten minutes
        // it didn't exist), at least a minute so one request doesn't read as a huge rate.
        let usd_micros_per_hour = charged.first().filter(|_| usd > 0).map(|(first, _)| {
            let over = now.saturating_sub(*first).max(WINDOW_MS);
            ((usd as f64) * 3_600_000.0 / over as f64).round() as u64
        });
        let retries = self.retries.iter().filter(|(at, _)| *at >= power_from);
        let (retries, throttles) = retries.fold((0u32, 0u32), |(r, t), (_, throttled)| (r + 1, t + u32::from(*throttled)));
        Metrics {
            model_pct: pct(spent[0]),
            tools_pct: pct(spent[1]),
            you_pct: pct(spent[2]),
            tokens_per_s,
            cache_hit_pct,
            usd_micros_per_hour,
            requests_per_min: minute.len() as u32,
            retries,
            throttles,
            rate_limit: self.rate_limit,
        }
    }
}

/// How full an agent's context is (§11.2): the last request's tokens out of its budget (the compaction threshold,
/// or the window).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    pub used: u64,
    pub budget: u64,
}

impl Context {
    /// Used ÷ budget in %; `None` with no budget.
    pub fn pct(&self) -> Option<u8> {
        (self.budget > 0).then(|| ((self.used as f64 / self.budget as f64) * 100.0).round().min(255.0) as u8)
    }

    /// The memory-pressure colour (§20.9): by budget, not window. Amber past 60% (attention fades), red from 90%.
    pub fn pressure(&self) -> Pressure {
        match self.pct() {
            Some(p) if p >= 90 => Pressure::Red,
            Some(p) if p > 60 => Pressure::Amber,
            _ => Pressure::Green,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pressure {
    Green,
    Amber,
    Red,
}

/// What kind of real process an agent owns (§20.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessKind {
    /// A background process from the process tool (a dev server).
    Process,
    /// A local MCP server Marv started.
    Mcp,
}

/// A real OS process an agent owns, measured by its own Marv ([`crate::procstats`]: only PIDs it started).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildProcess {
    pub kind: ProcessKind,
    /// The MCP server's configured name, or the process's name (cut and cleaned on receipt).
    pub name: String,
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss_kb: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_pct: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alive: Option<bool>,
}

/// A subagent running under an agent: its kind (an agent file's name), never its task's words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentRow {
    pub kind: String,
    pub phase: Phase,
}

/// What a Marv says about itself beyond its card and state (§20.2, O2): its core, context, metrics, spend and real
/// processes. Fixed shapes; `marv/info` carries it as `live`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Live {
    /// The model (the core, §16).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<Context>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<Metrics>,
    /// Today's spend (the UTC day) by this load of the session, micro-dollars: never a resumed session's lifetime total.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spend_usd_micros: Option<u64>,
    /// The person paused it (§15.3).
    #[serde(default)]
    pub paused: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subagents: Vec<SubagentRow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub processes: Vec<ChildProcess>,
}

/// The most of each list a received [`Live`] keeps, and the longest name.
pub const MAX_LIVE_ROWS: usize = 32;
pub const MAX_LIVE_NAME: usize = 48;

/// One printable line of at most [`MAX_LIVE_NAME`] characters: what a peer's name or kind is shown as.
pub fn clean_name(text: &str) -> String {
    let mut out: String = text
        .chars()
        .map(|c| if c.is_control() || is_bidi(c) { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if out.chars().count() > MAX_LIVE_NAME {
        out = out.chars().take(MAX_LIVE_NAME - 1).collect::<String>() + "…";
    }
    out
}

fn is_bidi(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

impl Live {
    /// Another Marv's `live` as it may be kept and shown: lists capped, names cut and cleaned (never trusted as sent).
    pub fn received(mut self) -> Self {
        self.core = self.core.map(|c| clean_name(&c));
        self.subagents.truncate(MAX_LIVE_ROWS);
        self.processes.truncate(MAX_LIVE_ROWS);
        for s in &mut self.subagents {
            s.kind = clean_name(&s.kind);
        }
        for p in &mut self.processes {
            p.name = clean_name(&p.name);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_minute_splits_into_model_tools_and_you() {
        let mut m = Meter::new();
        m.turn_start(0);
        m.tool_start(10_000); // 10 s model
        m.wait_start(15_000); // 5 s tools
        m.wait_end(35_000); // 20 s waiting for the person
        m.tool_end(40_000); // 5 s tools
        m.turn_end(50_000); // 10 s model
        let s = m.snapshot(60_000);
        assert_eq!((s.model_pct, s.tools_pct, s.you_pct), (33, 17, 33));
        // A minute later it's all gone.
        assert_eq!(m.snapshot(120_000).model_pct, 0);
        // The running phase counts up to now.
        let mut m = Meter::new();
        m.turn_start(30_000);
        assert_eq!(m.snapshot(60_000).model_pct, 50);
        assert_eq!(m.phase(), Phase::Model);
    }

    #[test]
    fn parallel_calls_and_nested_approvals_count_once() {
        let mut m = Meter::new();
        m.turn_start(0);
        m.tool_start(0);
        m.tool_start(0);
        m.wait_start(10_000);
        m.wait_start(10_000);
        m.wait_end(20_000);
        assert_eq!(m.phase(), Phase::You);
        m.wait_end(30_000);
        m.tool_end(40_000);
        assert_eq!(m.phase(), Phase::Tools);
        m.tool_end(60_000);
        let s = m.snapshot(60_000);
        assert_eq!((s.model_pct, s.tools_pct, s.you_pct), (0, 67, 33));
        // A turn that ends mid-call (stopped) leaves nothing running.
        m.tool_start(60_000);
        m.turn_end(61_000);
        assert_eq!(m.phase(), Phase::Idle);
    }

    #[test]
    fn requests_give_speed_cache_and_power() {
        let mut m = Meter::new();
        m.turn_start(0);
        m.request(2_000, 1000, 800, 100, true); // 100 tokens in 2 s
        m.charge(2_000, 10_000);
        m.request(4_000, 1000, 1000, 100, true); // another 100 in 2 s
        m.charge(4_000, 10_000);
        // A subagent's request: the cache counts it, tokens/s doesn't.
        m.request(5_000, 0, 0, 5_000, false);
        let s = m.snapshot(60_000);
        assert_eq!(s.tokens_per_s, Some(50));
        assert_eq!(s.cache_hit_pct, Some(90));
        assert_eq!(s.requests_per_min, 3, "the subagent's too");
        // $0.02 over the first minute (at least a minute): $1.20 an hour.
        assert_eq!(s.usd_micros_per_hour, Some(1_200_000));
        // Ten minutes later the power window is empty.
        let later = m.snapshot(4_000 + POWER_WINDOW_MS + 1);
        assert_eq!((later.cache_hit_pct, later.usd_micros_per_hour, later.tokens_per_s), (None, None, None));
        // No price: no $/h, the rest still counts.
        let mut m = Meter::new();
        m.request(1000, 10, 0, 0, true);
        m.charge(1000, 0);
        assert_eq!((m.snapshot(1000).usd_micros_per_hour, m.snapshot(1000).cache_hit_pct), (None, Some(0)));
        // Today's spend starts again on a new UTC day.
        let mut m = Meter::new();
        m.charge(crate::DAY_MS - 10, 300);
        m.charge(crate::DAY_MS - 5, 200);
        assert_eq!(m.spent_today(crate::DAY_MS - 1), 500);
        assert_eq!(m.spent_today(crate::DAY_MS + 1), 0);
        m.charge(crate::DAY_MS + 2, 7);
        assert_eq!(m.spent_today(crate::DAY_MS + 3), 7);
    }

    #[test]
    fn throttles_and_the_rate_limit() {
        let mut m = Meter::new();
        m.retry(1000, false);
        m.retry(2000, true);
        m.rate_limit(RateLimit { requests_limit: Some(100), requests_left: Some(69), ..RateLimit::default() });
        let s = m.snapshot(3000);
        assert_eq!((s.retries, s.throttles), (2, 1));
        assert_eq!(s.rate_limit.unwrap().used_pct(), Some(31));
        let both = RateLimit { requests_limit: Some(10), requests_left: Some(9), tokens_limit: Some(100), tokens_left: Some(20) };
        assert_eq!(both.used_pct(), Some(80), "the busier of the two");
        assert_eq!(RateLimit::default().used_pct(), None);
        assert_eq!(m.snapshot(2000 + POWER_WINDOW_MS + 1).retries, 0);
    }

    #[test]
    fn context_pressure_is_by_budget() {
        let c = |used| Context { used, budget: 100 };
        assert_eq!((c(50).pressure(), c(61).pressure(), c(95).pressure()), (Pressure::Green, Pressure::Amber, Pressure::Red));
        assert_eq!(Context { used: 5, budget: 0 }.pct(), None);
    }

    #[test]
    fn a_received_live_is_cut_and_cleaned() {
        let live = Live {
            core: Some("\x1b[31mglm\u{202E}".into()),
            processes: (0..40)
                .map(|i| ChildProcess {
                    kind: ProcessKind::Mcp,
                    name: format!("{}\n{i}", "x".repeat(100)),
                    pid: i,
                    rss_kb: None,
                    cpu_pct: None,
                    alive: None,
                })
                .collect(),
            ..Live::default()
        }
        .received();
        assert_eq!(live.core.as_deref(), Some("[31mglm"));
        assert_eq!(live.processes.len(), MAX_LIVE_ROWS);
        assert!(live.processes[0].name.chars().count() <= MAX_LIVE_NAME && !live.processes[0].name.contains('\n'));
        // An older Marv's info (none of it) parses as nothing.
        assert_eq!(serde_json::from_str::<Live>("{}").unwrap(), Live::default());
    }
}
