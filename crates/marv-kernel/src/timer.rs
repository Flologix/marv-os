//! Timers (runtime-kernel §15.4, decision I4): `~/.marv/kernel/timers.json`. New in marv-rs.
//!
//! - **Recurring timers are the person's:** made in the desktop, for one chat ([`TimerBy::Person`]). No tool, project
//!   or mesh message makes one: a [`TimerBy::Chat`] timer is always [`Repeat::Once`], and a file that says otherwise is
//!   refused whole.
//! - **One-shot self-timers under a grant** (`wake_me`): at most [`MAX_AHEAD_MS`] ahead, at most
//!   [`SELF_TIMERS_PER_DAY`] made a (UTC) day per chat, tied to the grant that was live when it was made: void if that
//!   grant ends. Their note is the chat's own text, delivered fenced, never as the person's words.
//! - **A missed fire runs once, late:** a timer due while Marv was closed fires once at the next look, marked late;
//!   several missed recurring fires collapse to one ([`Timer::after_fire`]).
//!
//! The same store rules as the grants ([`crate::grant_store`]): the user's own config folder only, versioned JSON with
//! unknown fields refused, 0600 in a 0700 folder, atomic, one change at a time across processes, ids never reused.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::grant::{DAY_MS, GrantId};
use crate::identity::AgentId;
use crate::store::{StoreError, locked, read_private_json, write_private_json};

/// The timers file's format version.
pub const TIMER_VERSION: u32 = 1;
/// How far ahead `wake_me` may set a timer: 6 hours.
pub const MAX_AHEAD_MS: u64 = 6 * 60 * 60 * 1000;
/// How many `wake_me` timers one chat may make a day.
pub const SELF_TIMERS_PER_DAY: u32 = 10;
/// The shortest `wake_me` delay, and the shortest recurring period (a timer is not a busy loop).
pub const MIN_AFTER_MS: u64 = 60 * 1000;
pub const MIN_EVERY_MS: u64 = 15 * 60 * 1000;
/// The longest recurring period: 31 days.
pub const MAX_EVERY_MS: u64 = 31 * DAY_MS;
/// The longest note a timer keeps (characters).
pub const MAX_NOTE: usize = 2000;
/// A fire this late (or later) is marked late: it was missed (Marv was closed, the chat was paused or busy).
pub const LATE_AFTER_MS: u64 = 2 * 60 * 1000;
/// A turn the person didn't start, running under the chat's grant, runs at most this long (wall time) before the
/// kernel stops it (§15.4).
pub const TURN_DEADLINE_MS: u64 = 30 * 60 * 1000;

/// Whether a timer may run with `live` the chat's grant in force now (review P2-3): every timer needs one, and a
/// chat's own timer only the very grant it was set under (a grant revoked and made again is a new grant: its old
/// timers are void).
pub fn may_fire(by: TimerBy, set_under: Option<GrantId>, live: Option<GrantId>) -> bool {
    match (by, live) {
        (_, None) => false,
        (TimerBy::Person, Some(_)) => true,
        (TimerBy::Chat, Some(live)) => set_under == Some(live),
    }
}

/// The deadline of an unattended turn (§15.4): only one that runs under a live grant has one. An ungranted woken turn
/// asks for everything, so it can't run away; stopping and pausing it would only silence a rule's chat.
pub fn turn_deadline(granted: bool) -> Option<u64> {
    granted.then_some(TURN_DEADLINE_MS)
}

/// `<config dir>/kernel/timers.json`.
pub fn timers_path(config_dir: &Path) -> PathBuf {
    config_dir.join("kernel").join("timers.json")
}

/// A timer's id: given by the store, never reused in one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TimerId(pub u64);

impl std::fmt::Display for TimerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "timer {}", self.0)
    }
}

/// Who made a timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimerBy {
    /// The person, in the desktop: once or recurring.
    Person,
    /// The chat itself (`wake_me`), under a grant: once only.
    Chat,
}

/// When a timer comes back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Repeat {
    Once,
    /// Every `every_ms` after its first time.
    Every {
        every_ms: u64,
    },
    /// Review P3-1: every day at `minute_of_day` (0..1440) local time in the IANA zone `tz` (the page's, "every
    /// morning at 9"), across DST: a time a spring-forward skips fires at the first valid instant after it, and a time
    /// a fall-back repeats fires at the earlier one. The person's only.
    Daily {
        minute_of_day: u16,
        tz: String,
    },
}

impl Repeat {
    /// Whether it comes back at all.
    pub fn recurs(&self) -> bool {
        !matches!(self, Repeat::Once)
    }
}

/// The first `minute_of_day` in zone `tz` strictly after `after_ms`. None: an unknown zone or minute, or past the end
/// of time.
pub fn next_daily(minute_of_day: u16, tz: &str, after_ms: u64) -> Option<u64> {
    use chrono::{Duration, LocalResult, NaiveTime, TimeZone};
    if minute_of_day >= 24 * 60 {
        return None;
    }
    let tz: chrono_tz::Tz = tz.parse().ok()?;
    let after = chrono::DateTime::from_timestamp_millis(i64::try_from(after_ms).ok()?)?.with_timezone(&tz);
    let at = NaiveTime::from_hms_opt(u32::from(minute_of_day / 60), u32::from(minute_of_day % 60), 0)?;
    let mut day = after.date_naive();
    // Today's, or the next days' (two are enough but for a zone that skips a whole day).
    for _ in 0..4 {
        let mut local = day.and_time(at);
        // A gap (spring forward): the first valid minute after it.
        let instant = loop {
            match tz.from_local_datetime(&local) {
                LocalResult::Single(t) => break Some(t),
                LocalResult::Ambiguous(earlier, _) => break Some(earlier),
                LocalResult::None => {
                    local = local.checked_add_signed(Duration::minutes(1))?;
                    if local.date() != day && local.time() > at {
                        break None;
                    }
                }
            }
        };
        if let Some(instant) = instant
            && instant > after
        {
            return u64::try_from(instant.timestamp_millis()).ok();
        }
        day = day.succ_opt()?;
    }
    None
}

/// One timer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timer {
    pub id: TimerId,
    pub chat: AgentId,
    pub by: TimerBy,
    pub repeat: Repeat,
    /// When it next fires.
    pub at_ms: u64,
    /// The person's words (a recurring timer's "check CI"), or the chat's own note (`wake_me`, fenced when it fires).
    pub note: String,
    /// A `wake_me` timer's grant: void once it isn't the chat's live grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<GrantId>,
    /// A `wake_me` timer's mesh hop: the turn that set it was at this hop, and the turn it wakes starts there (so a
    /// timer never resets a chain's hop count). The person's: 0.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub hop: u32,
    pub created_at_ms: u64,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl Timer {
    /// What a stored timer must be: only the person's recur; a chat's has its grant.
    pub fn check(&self) -> Result<(), String> {
        match (self.by, &self.repeat) {
            (TimerBy::Chat, Repeat::Every { .. } | Repeat::Daily { .. }) => {
                Err(format!("{} recurs but wasn't made by the person", self.id))
            }
            (_, Repeat::Daily { minute_of_day, tz }) if next_daily(*minute_of_day, tz, 0).is_none() => {
                Err(format!("{} is daily at a time or zone Marv doesn't know", self.id))
            }
            (TimerBy::Chat, Repeat::Once) if self.grant.is_none() => {
                Err(format!("{} was set by a chat without a grant", self.id))
            }
            (_, Repeat::Every { every_ms }) if *every_ms < MIN_EVERY_MS => Err(format!("{} recurs too often", self.id)),
            (_, Repeat::Every { every_ms }) if *every_ms > MAX_EVERY_MS => {
                Err(format!("{} recurs less than once a month", self.id))
            }
            _ if self.note.chars().count() > MAX_NOTE => Err(format!("{}'s note is too long", self.id)),
            _ => Ok(()),
        }
    }

    pub fn due(&self, now_ms: u64) -> bool {
        self.at_ms <= now_ms
    }

    /// Whether a fire at `now_ms` is late (missed while Marv was closed, or held while the chat was busy or paused).
    pub fn late(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.at_ms) >= LATE_AFTER_MS
    }

    /// After a fire at `now_ms`: none (a one-shot is done), or the next time, past `now_ms` (every missed fire
    /// collapses into the one that ran).
    pub fn after_fire(&self, now_ms: u64) -> Option<u64> {
        match &self.repeat {
            Repeat::Once => None,
            Repeat::Daily { minute_of_day, tz } => next_daily(*minute_of_day, tz, now_ms.max(self.at_ms)),
            &Repeat::Every { every_ms } => {
                let every = every_ms.clamp(MIN_EVERY_MS, MAX_EVERY_MS);
                let missed = now_ms.saturating_sub(self.at_ms) / every;
                // Checked: a time past u64 (a planted at_ms near the end) means it never comes again.
                missed.checked_add(1).and_then(|n| n.checked_mul(every)).and_then(|d| self.at_ms.checked_add(d))
            }
        }
    }
}

/// Why a timer was void (a fixed word).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimerVoidReason {
    /// Its chat's grant ended (revoked, expired, replaced) or there's none.
    GrantEnded,
    /// The grant is at today's spend cap: this fire doesn't run.
    Spent,
    /// Its chat is gone, or can't load.
    NoChat,
    /// Its chat isn't on the mesh (no runtime folder): nothing here can run its timers.
    NotOnMesh,
    /// The person removed it.
    Removed,
}

/// Why `wake_me` was refused (shown to the model as the tool's error).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WakeMeRefused {
    #[error("wake_me needs a background grant: the person hasn't let this chat work while they're away")]
    NoGrant,
    #[error("wake_me can set a timer at most 6 hours ahead")]
    TooFar,
    #[error("wake_me needs at least a minute")]
    TooSoon,
    #[error("this chat has set its 10 timers for today")]
    DailyLimit,
    #[error("wake_me needs a note for when it fires")]
    NoNote,
    #[error("wake_me's note is at most 2000 characters")]
    NoteTooLong,
    #[error("couldn't save the timer: {0}")]
    Store(String),
}

/// The timers as one version of the file had them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TimerSnapshot {
    pub generation: u64,
    pub timers: Vec<Timer>,
}

impl TimerSnapshot {
    /// The timers due at `now_ms`, earliest first.
    pub fn due(&self, now_ms: u64) -> Vec<&Timer> {
        let mut due: Vec<&Timer> = self.timers.iter().filter(|t| t.due(now_ms)).collect();
        due.sort_by_key(|t| (t.at_ms, t.id));
        due
    }

    pub fn for_chat<'a>(&'a self, chat: &'a AgentId) -> impl Iterator<Item = &'a Timer> + 'a {
        self.timers.iter().filter(move |t| &t.chat == chat)
    }
}

/// When one chat made a `wake_me` timer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SelfMade {
    chat: AgentId,
    made_ms: u64,
}

/// Whether a timer made at `made_ms` counts against the limit at `now_ms`: within the last 24 hours, or "later" than
/// now (a clock that went back never frees the limit).
fn counts(made_ms: u64, now_ms: u64) -> bool {
    made_ms > now_ms || now_ms - made_ms < DAY_MS
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TimerFile {
    version: u32,
    generation: u64,
    next_id: u64,
    timers: Vec<Timer>,
    /// `wake_me` timers made in the last 24 hours (fired or not): the limit counts what was made, in a rolling window.
    #[serde(default)]
    self_made: Vec<SelfMade>,
}

impl Default for TimerFile {
    fn default() -> Self {
        Self { version: TIMER_VERSION, generation: 0, next_id: 1, timers: Vec::new(), self_made: Vec::new() }
    }
}

/// A timer the person is making in the desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonTimer {
    pub chat: AgentId,
    pub repeat: Repeat,
    pub at_ms: u64,
    pub note: String,
}

/// One user's timers file.
#[derive(Debug, Clone)]
pub struct TimerStore {
    path: PathBuf,
}

impl TimerStore {
    /// The store in `config_dir` (absolute: the user's config folder).
    pub fn new(config_dir: &Path) -> Result<Self, StoreError> {
        if !config_dir.is_absolute() {
            return Err(StoreError::NotAbsolute(config_dir.display().to_string()));
        }
        Ok(Self { path: timers_path(config_dir) })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn shown(&self) -> String {
        self.path.display().to_string()
    }

    /// The timers now. No file: none. A file that fails any check is an error, never part of it.
    pub fn load(&self) -> Result<TimerSnapshot, StoreError> {
        let file = self.read()?;
        Ok(TimerSnapshot { generation: file.generation, timers: file.timers })
    }

    fn read(&self) -> Result<TimerFile, StoreError> {
        let Some(file) = read_private_json::<TimerFile>(&self.path, TIMER_VERSION)? else {
            return Ok(TimerFile::default());
        };
        let mut ids = std::collections::HashSet::new();
        for timer in &file.timers {
            timer.check().map_err(|e| StoreError::Parse(self.shown(), e))?;
            if !ids.insert(timer.id) || timer.id.0 >= file.next_id {
                return Err(StoreError::Parse(self.shown(), format!("{} is listed twice or past next_id", timer.id)));
            }
        }
        Ok(file)
    }

    async fn change<T: Send>(
        &self,
        edit: impl FnOnce(&mut TimerFile) -> Result<T, StoreError> + Send,
    ) -> Result<(T, TimerSnapshot), StoreError> {
        locked(&self.path, || async {
            let mut file = self.read()?;
            let out = edit(&mut file)?;
            file.generation += 1;
            for timer in &file.timers {
                timer.check().map_err(|e| StoreError::Parse(self.shown(), e))?;
            }
            write_private_json(&self.path, &file).await?;
            Ok((out, TimerSnapshot { generation: file.generation, timers: file.timers }))
        })
        .await
    }

    /// The person's timer (the desktop's own click; once or recurring).
    /// A daily timer's first time is its next local occurrence after `now_ms` (the given `at_ms` is ignored).
    pub async fn add_person(&self, timer: PersonTimer, now_ms: u64) -> Result<(Timer, TimerSnapshot), StoreError> {
        let shown = self.shown();
        let at_ms = match &timer.repeat {
            Repeat::Daily { minute_of_day, tz } => next_daily(*minute_of_day, tz, now_ms)
                .ok_or_else(|| StoreError::Parse(shown.clone(), "that time or zone isn't one Marv knows".into()))?,
            _ => timer.at_ms,
        };
        self.change(|file| {
            let timer = Timer {
                id: TimerId(file.next_id),
                chat: timer.chat,
                by: TimerBy::Person,
                repeat: timer.repeat,
                at_ms,
                note: timer.note,
                grant: None,
                hop: 0,
                created_at_ms: now_ms,
            };
            timer.check().map_err(|e| StoreError::Parse(shown, e))?;
            file.next_id += 1;
            file.timers.push(timer.clone());
            Ok(timer)
        })
        .await
    }

    /// `wake_me`: a one-shot timer `after_ms` from `now_ms` for `chat`, under its live `grant` (the caller checked it
    /// is). Inside the limits or refused: never more than [`MAX_AHEAD_MS`] ahead, never more than
    /// [`SELF_TIMERS_PER_DAY`] made today.
    #[allow(clippy::too_many_arguments)]
    pub async fn add_self(
        &self,
        chat: &AgentId,
        grant: GrantId,
        hop: u32,
        after_ms: u64,
        note: &str,
        now_ms: u64,
    ) -> Result<Timer, WakeMeRefused> {
        if after_ms > MAX_AHEAD_MS {
            return Err(WakeMeRefused::TooFar);
        }
        if after_ms < MIN_AFTER_MS {
            return Err(WakeMeRefused::TooSoon);
        }
        let note = note.trim().to_string();
        if note.is_empty() {
            return Err(WakeMeRefused::NoNote);
        }
        if note.chars().count() > MAX_NOTE {
            return Err(WakeMeRefused::NoteTooLong);
        }
        let made = self
            .change(|file| {
                file.self_made.retain(|m| counts(m.made_ms, now_ms));
                let count = file.self_made.iter().filter(|m| &m.chat == chat).count();
                if count >= SELF_TIMERS_PER_DAY as usize {
                    return Ok(Err(WakeMeRefused::DailyLimit));
                }
                file.self_made.push(SelfMade { chat: chat.clone(), made_ms: now_ms });
                let timer = Timer {
                    id: TimerId(file.next_id),
                    chat: chat.clone(),
                    by: TimerBy::Chat,
                    repeat: Repeat::Once,
                    at_ms: now_ms + after_ms,
                    note,
                    grant: Some(grant),
                    hop,
                    created_at_ms: now_ms,
                };
                file.next_id += 1;
                file.timers.push(timer.clone());
                Ok(Ok(timer))
            })
            .await
            .map_err(|e| WakeMeRefused::Store(e.to_string()))?;
        made.0
    }

    /// Timer `id` fired at `now_ms`: a one-shot goes, a recurring one moves past `now_ms`. The timer as it was, if it
    /// was there and due (checked under the lock: two looks never both fire one time).
    pub async fn fired(&self, id: TimerId, now_ms: u64) -> Result<Option<Timer>, StoreError> {
        self.change(|file| {
            let Some(at) = file.timers.iter().position(|t| t.id == id && t.at_ms <= now_ms) else { return Ok(None) };
            let timer = file.timers[at].clone();
            match timer.after_fire(now_ms) {
                Some(next) => file.timers[at].at_ms = next,
                None => {
                    file.timers.remove(at);
                }
            }
            Ok(Some(timer))
        })
        .await
        .map(|(t, _)| t)
    }

    /// Removes timer `id` (the person's click, or a void one-shot). Whether it was there.
    pub async fn remove(&self, id: TimerId) -> Result<(bool, TimerSnapshot), StoreError> {
        self.change(|file| {
            let before = file.timers.len();
            file.timers.retain(|t| t.id != id);
            Ok(file.timers.len() != before)
        })
        .await
    }

    /// The chat's `wake_me` timers made in the last 24 hours (for the person, and tests).
    pub fn made_today(&self, chat: &AgentId, now_ms: u64) -> u32 {
        self.read().map_or(0, |f| f.self_made.iter().filter(|m| &m.chat == chat && counts(m.made_ms, now_ms)).count() as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grant::tests::chat_id;

    fn timer(by: TimerBy, repeat: Repeat) -> Timer {
        Timer {
            id: TimerId(1),
            chat: chat_id(1),
            by,
            repeat,
            at_ms: 1_000,
            note: "check CI".into(),
            grant: Some(GrantId(1)),
            hop: 0,
            created_at_ms: 0,
        }
    }

    #[test]
    fn a_missed_recurring_timer_fires_once_late_and_moves_past_now() {
        let every = 60 * 60 * 1000;
        let t = Timer { at_ms: 10 * every, ..timer(TimerBy::Person, Repeat::Every { every_ms: every }) };
        assert!(!t.due(10 * every - 1));
        assert!(t.due(10 * every) && !t.late(10 * every));
        // Closed for five and a half periods: one fire, late, and the next is the first one after now.
        let now = 15 * every + every / 2;
        assert!(t.late(now));
        assert_eq!(t.after_fire(now), Some(16 * every));
        assert_eq!(timer(TimerBy::Chat, Repeat::Once).after_fire(now), None);
        // Never past the end of time, and never more than a month apart.
        let end = Timer { at_ms: u64::MAX - 5, ..timer(TimerBy::Person, Repeat::Every { every_ms: MAX_EVERY_MS }) };
        assert_eq!(end.after_fire(u64::MAX), None);
        assert!(timer(TimerBy::Person, Repeat::Every { every_ms: MAX_EVERY_MS + 1 }).check().is_err());
    }

    /// Review P3-1: 9:00 in Berlin stays 9:00 across both DST changes; a time the spring-forward skips fires at the
    /// first valid minute after it, and a repeated one at its earlier instant.
    #[test]
    fn a_daily_timer_keeps_its_local_time_across_dst() {
        use chrono::TimeZone;
        let berlin: chrono_tz::Tz = "Europe/Berlin".parse().unwrap();
        let ms = |y, m, d, h, min| berlin.with_ymd_and_hms(y, m, d, h, min, 0).earliest().unwrap().timestamp_millis() as u64;
        // The day before the 2026 spring-forward (29 March), after 9:00: the next is 9:00 on the 29th, 23 h later.
        let next = next_daily(9 * 60, "Europe/Berlin", ms(2026, 3, 28, 10, 0)).unwrap();
        assert_eq!(next, ms(2026, 3, 29, 9, 0));
        // And across the fall-back (25 October): 9:00 local again.
        assert_eq!(next_daily(9 * 60, "Europe/Berlin", ms(2026, 10, 24, 10, 0)), Some(ms(2026, 10, 25, 9, 0)));
        // 2:30 doesn't exist on 29 March: the first valid minute after it, 3:00 CEST.
        let gap = next_daily(2 * 60 + 30, "Europe/Berlin", ms(2026, 3, 28, 12, 0)).unwrap();
        assert_eq!(gap, ms(2026, 3, 29, 3, 0));
        // 2:30 happens twice on 25 October: the earlier.
        let twice = next_daily(2 * 60 + 30, "Europe/Berlin", ms(2026, 10, 24, 12, 0)).unwrap();
        assert_eq!(twice, berlin.with_ymd_and_hms(2026, 10, 25, 2, 30, 0).earliest().unwrap().timestamp_millis() as u64);
        assert!(next_daily(9 * 60, "Mars/Olympus", 0).is_none());
        assert!(next_daily(24 * 60, "Europe/Berlin", 0).is_none());
        let daily = Repeat::Daily { minute_of_day: 9 * 60, tz: "Europe/Berlin".into() };
        let t = Timer { at_ms: next, ..timer(TimerBy::Person, daily.clone()) };
        assert!(t.check().is_ok());
        assert_eq!(t.after_fire(next + 3 * DAY_MS), Some(ms(2026, 4, 2, 9, 0)), "missed days collapse into one");
        assert!(timer(TimerBy::Chat, daily).check().is_err(), "the person's only");
    }

    #[test]
    fn only_the_persons_timers_recur() {
        assert!(timer(TimerBy::Person, Repeat::Every { every_ms: MIN_EVERY_MS }).check().is_ok());
        assert!(timer(TimerBy::Chat, Repeat::Every { every_ms: MIN_EVERY_MS }).check().is_err());
        assert!(Timer { grant: None, ..timer(TimerBy::Chat, Repeat::Once) }.check().is_err(), "a chat's needs its grant");
        assert!(timer(TimerBy::Person, Repeat::Every { every_ms: 1_000 }).check().is_err(), "not a busy loop");
    }

    #[tokio::test]
    async fn wake_me_stays_inside_its_limits_and_the_file_round_trips() {
        let d = tempfile::tempdir().unwrap();
        let store = TimerStore::new(d.path()).unwrap();
        let chat = chat_id(1);
        let now = 3 * DAY_MS + 5;
        assert_eq!(store.add_self(&chat, GrantId(1), 0, MAX_AHEAD_MS + 1, "x", now).await, Err(WakeMeRefused::TooFar));
        assert_eq!(store.add_self(&chat, GrantId(1), 0, 1, "x", now).await, Err(WakeMeRefused::TooSoon));
        assert_eq!(store.add_self(&chat, GrantId(1), 0, MIN_AFTER_MS, "  ", now).await, Err(WakeMeRefused::NoNote));
        for _ in 0..SELF_TIMERS_PER_DAY {
            store.add_self(&chat, GrantId(1), 0, MAX_AHEAD_MS, "check the run", now).await.unwrap();
        }
        assert_eq!(store.add_self(&chat, GrantId(1), 0, MIN_AFTER_MS, "one more", now).await, Err(WakeMeRefused::DailyLimit));
        assert!(store.add_self(&chat_id(2), GrantId(2), 0, MIN_AFTER_MS, "another chat", now).await.is_ok());
        assert_eq!(
            store.add_self(&chat, GrantId(1), 0, MIN_AFTER_MS, "next morning", now + DAY_MS - 1).await,
            Err(WakeMeRefused::DailyLimit),
            "a rolling 24 hours, not the calendar day"
        );
        assert!(store.add_self(&chat, GrantId(1), 0, MIN_AFTER_MS, "a day later", now + DAY_MS).await.is_ok());
        let long = "x".repeat(MAX_NOTE + 1);
        assert_eq!(store.add_self(&chat_id(3), GrantId(3), 0, MIN_AFTER_MS, &long, now).await, Err(WakeMeRefused::NoteTooLong));
        let snapshot = store.load().unwrap();
        assert_eq!(snapshot.timers.len(), SELF_TIMERS_PER_DAY as usize + 2);
        assert!(snapshot.timers.iter().all(|t| t.by == TimerBy::Chat && t.repeat == Repeat::Once));
        // A fired one-shot goes.
        let first = snapshot.timers[0].id;
        assert!(store.fired(first, now + MAX_AHEAD_MS - 1).await.unwrap().is_none(), "not due yet: not fired");
        assert!(store.fired(first, now + MAX_AHEAD_MS).await.unwrap().is_some());
        assert!(store.fired(first, now + MAX_AHEAD_MS).await.unwrap().is_none(), "once");
        assert!(store.load().unwrap().timers.iter().all(|t| t.id != first));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(store.path()).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[tokio::test]
    async fn a_file_with_a_chat_made_recurring_timer_is_refused_whole() {
        let d = tempfile::tempdir().unwrap();
        let store = TimerStore::new(d.path()).unwrap();
        let (person, _) = store
            .add_person(
                PersonTimer { chat: chat_id(1), repeat: Repeat::Every { every_ms: DAY_MS }, at_ms: 9, note: "CI".into() },
                1,
            )
            .await
            .unwrap();
        assert_eq!(person.by, TimerBy::Person);
        let text = std::fs::read_to_string(store.path()).unwrap().replace("\"person\"", "\"chat\"");
        std::fs::write(store.path(), text).unwrap();
        assert!(store.load().is_err());
        assert!(TimerStore::new(Path::new("relative")).is_err());
    }
}
