//! The team board: shared memory between a rule's chats (runtime-kernel §19, M7b, decisions T1–T7). New in marv-rs.
//!
//! - **One board per rule** (T1), at `<config dir>/mesh/boards/<rule id>.json`, beside `policies.json`, with the
//!   rules' store code ([`crate::store`]): versioned JSON with unknown fields refused, 0600 in a 0700 folder, atomic,
//!   one read-change-write at a time across Marv processes (`marv_fs::with_file_lock` on the board's file), and a
//!   symlink or a file others can read or write refused whole.
//! - An entry is `{ id, by, kind, text, paths, at_ms }`. `by` is stamped by whoever calls the store (the mesh: the
//!   chat's own mesh name; the desktop: the person), never taken from a model's input. `kind` is `claim`, `note` or
//!   `done`; `text` one line of at most [`MAX_TEXT_CHARS`] characters; `paths` at most [`MAX_PATHS`] workspace paths.
//! - **Limits** (T2): an entry expires [`ENTRY_TTL_MS`] after it was posted (dropped at the next change, and never
//!   read). An author holds at most [`MAX_CLAIMS`] live claims (a sixth is refused; a `done` closes one) and
//!   [`MAX_NOTES`] notes and dones (the eleventh drops the author's own oldest). A board keeps [`MAX_ENTRIES`]: when
//!   it's full a post drops the poster's own oldest entry, and one who has none there is refused. Nobody's post ever
//!   pushes another author's entry off.
//! - **Text** is one line: control characters, line and paragraph separators, bidi controls and other format
//!   characters (Cf) are refused, in the text and in paths.
//! - **Lifetime:** the rule's. A post re-reads the rules file under the board's lock and refuses when its rule is
//!   gone or has expired, and [`crate::PolicyStore::revoke`] deletes the board under that same lock, so a post racing
//!   a revoke can't bring a revoked rule's board back.
//! - What the text *is* (another chat's words: Outside, fenced and tainting when read, §19.3) is the reader's
//!   business (`marv_mesh::board`); the store keeps it as given.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::policy::PolicyId;
use crate::store::{PolicyStore, StoreError, locked, read_private_json, write_private_json};

/// The board file's format version.
pub const BOARD_VERSION: u32 = 1;
/// An entry's text, at most (characters).
pub const MAX_TEXT_CHARS: usize = 280;
/// Entries a board keeps, at most: the oldest go first.
pub const MAX_ENTRIES: usize = 50;
/// Live claims one author may hold on a board.
pub const MAX_CLAIMS: usize = 5;
/// Live notes and dones one author may hold on a board: past it, the author's own oldest goes.
pub const MAX_NOTES: usize = 10;
/// How long an entry lives: 24 hours.
pub const ENTRY_TTL_MS: u64 = 24 * 60 * 60 * 1000;
/// Paths one entry may name, and a path's length, at most.
pub const MAX_PATHS: usize = 10;
pub const MAX_PATH_CHARS: usize = 512;

/// `<config dir>/mesh/boards`.
pub fn boards_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("mesh").join("boards")
}

/// `<config dir>/mesh/boards/<rule id>.json`.
pub fn board_path(config_dir: &Path, rule: PolicyId) -> PathBuf {
    boards_dir(config_dir).join(format!("{}.json", rule.0))
}

/// What an entry says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoardKind {
    /// "I'm working on".
    Claim,
    Note,
    Done,
}

impl BoardKind {
    pub fn word(self) -> &'static str {
        match self {
            BoardKind::Claim => "claim",
            BoardKind::Note => "note",
            BoardKind::Done => "done",
        }
    }
}

impl std::str::FromStr for BoardKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "claim" => Ok(BoardKind::Claim),
            "note" => Ok(BoardKind::Note),
            "done" => Ok(BoardKind::Done),
            other => Err(format!("\"{other}\" isn't a board entry's kind: claim, note or done")),
        }
    }
}

/// Who posted an entry: a chat, by the mesh name its Marv stamped, or the person (the desktop).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum BoardBy {
    Chat(String),
    Person,
}

impl std::fmt::Display for BoardBy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BoardBy::Chat(name) => f.write_str(name),
            BoardBy::Person => f.write_str("the user"),
        }
    }
}

/// One entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardEntry {
    pub id: u64,
    pub by: BoardBy,
    pub kind: BoardKind,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    pub at_ms: u64,
}

impl BoardEntry {
    /// Whether it still lives at `now_ms` (expiry is exclusive, as a rule's).
    pub fn live_at(&self, now_ms: u64) -> bool {
        now_ms < self.at_ms.saturating_add(ENTRY_TTL_MS)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BoardFile {
    version: u32,
    rule: PolicyId,
    next_id: u64,
    entries: Vec<BoardEntry>,
}

impl BoardFile {
    fn new(rule: PolicyId) -> Self {
        Self { version: BOARD_VERSION, rule, next_id: 1, entries: Vec::new() }
    }
}

/// A post: the store gives it its id and time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEntry {
    pub by: BoardBy,
    pub kind: BoardKind,
    pub text: String,
    pub paths: Vec<String>,
}

/// What a post did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Posted {
    /// The entry as kept.
    pub entry: BoardEntry,
    /// How many of the poster's own older entries it pushed off (its note cap, or a full board).
    pub dropped: usize,
    /// The claim a `done` closed: the poster's own claim naming one of its paths, else its oldest.
    pub closed: Option<u64>,
}

/// What to clear.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Clear {
    /// Everything (the person).
    All,
    /// Everything one author posted (a model clears only its own).
    By(BoardBy),
    /// One entry (the person); with `Some(by)`, only when that author posted it.
    Entry(u64, Option<BoardBy>),
}

/// Why a post or a clear wasn't made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BoardError {
    /// The post breaks a limit, or isn't one line of text: Marv's sentence, for the model or the person.
    #[error("{0}")]
    Refused(String),
    /// The rule is gone (revoked) or has expired: the board went with it.
    #[error("{0} is gone or has expired: its board went with it")]
    NoRule(PolicyId),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// What a one-line entry may not hold: control characters (Cc), the line and paragraph separators (Zl, Zp), the bidi
/// controls and the other format characters (Cf: soft hyphen, zero-width characters, the word joiner and invisible
/// operators, the BOM, interlinear annotation and the tag characters).
pub fn not_one_line(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{2028}' | '\u{2029}'
                | '\u{00ad}'
                | '\u{0600}'..='\u{0605}'
                | '\u{061c}'
                | '\u{06dd}'
                | '\u{070f}'
                | '\u{08e2}'
                | '\u{180e}'
                | '\u{200b}'..='\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206f}'
                | '\u{feff}'
                | '\u{fff9}'..='\u{fffb}'
                | '\u{110bd}'
                | '\u{110cd}'
                | '\u{13430}'..='\u{1343f}'
                | '\u{1bca0}'..='\u{1bca3}'
                | '\u{1d173}'..='\u{1d17a}'
                | '\u{e0001}'
                | '\u{e0020}'..='\u{e007f}'
        )
}

/// A post's text as the board keeps it: trimmed, one line, 1..=[`MAX_TEXT_CHARS`] characters, no control characters.
pub fn check_text(text: &str) -> Result<String, BoardError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(BoardError::Refused("A board entry needs text: one line of what you're on or did.".into()));
    }
    if text.chars().any(not_one_line) {
        return Err(BoardError::Refused(
            "A board entry is one line: no line breaks, tabs, control, bidi or other invisible format characters.".into(),
        ));
    }
    let n = text.chars().count();
    if n > MAX_TEXT_CHARS {
        return Err(BoardError::Refused(format!(
            "A board entry is at most {MAX_TEXT_CHARS} characters (this one is {n}): say it shorter."
        )));
    }
    Ok(text.to_string())
}

/// A post's paths as the board keeps them: at most [`MAX_PATHS`], each non-empty, at most [`MAX_PATH_CHARS`], no
/// control characters; duplicates dropped.
pub fn check_paths(paths: &[String]) -> Result<Vec<String>, BoardError> {
    if paths.len() > MAX_PATHS {
        return Err(BoardError::Refused(format!("A board entry names at most {MAX_PATHS} paths.")));
    }
    let mut out: Vec<String> = Vec::new();
    for path in paths {
        let path = path.trim();
        if path.is_empty() || path.chars().count() > MAX_PATH_CHARS || path.chars().any(not_one_line) {
            return Err(BoardError::Refused(format!(
                "A board entry's paths are workspace paths of 1 to {MAX_PATH_CHARS} characters, one line each."
            )));
        }
        if !out.iter().any(|p| p == path) {
            out.push(path.to_string());
        }
    }
    Ok(out)
}

/// The person's boards, one per rule (module doc).
#[derive(Debug, Clone)]
pub struct BoardStore {
    config_dir: PathBuf,
    rules: PolicyStore,
}

impl BoardStore {
    /// The boards in `config_dir` (absolute: the user's config folder), for the rules in its `policies.json`.
    pub fn new(config_dir: &Path, home: &Path) -> Result<Self, StoreError> {
        let rules = PolicyStore::new(config_dir, home)?;
        Ok(Self { config_dir: config_dir.to_path_buf(), rules })
    }

    /// `rule`'s board file.
    pub fn path(&self, rule: PolicyId) -> PathBuf {
        board_path(&self.config_dir, rule)
    }

    fn read_file(&self, rule: PolicyId) -> Result<BoardFile, StoreError> {
        let path = self.path(rule);
        let shown = || path.display().to_string();
        let Some(file) = read_private_json::<BoardFile>(&path, BOARD_VERSION)? else { return Ok(BoardFile::new(rule)) };
        if file.rule != rule {
            return Err(StoreError::Parse(shown(), format!("it's {}'s board, not {rule}'s", file.rule)));
        }
        let mut ids = std::collections::HashSet::new();
        for entry in &file.entries {
            if !ids.insert(entry.id) || entry.id >= file.next_id {
                return Err(StoreError::Parse(shown(), format!("entry {} is listed twice or past next_id", entry.id)));
            }
            if entry.text.chars().count() > MAX_TEXT_CHARS || entry.paths.len() > MAX_PATHS {
                return Err(StoreError::Parse(shown(), format!("entry {} is over the board's limits", entry.id)));
            }
        }
        if file.entries.len() > MAX_ENTRIES {
            return Err(StoreError::Parse(shown(), format!("it holds over {MAX_ENTRIES} entries")));
        }
        Ok(file)
    }

    /// `rule`'s live entries at `now_ms`, oldest first. No file: none. A file that fails any check is an error.
    pub fn read(&self, rule: PolicyId, now_ms: u64) -> Result<Vec<BoardEntry>, StoreError> {
        Ok(self.read_file(rule)?.entries.into_iter().filter(|e| e.live_at(now_ms)).collect())
    }

    /// Whether `rule` is still in the person's file and live at `now_ms`, read now (under the board's lock).
    fn rule_live(&self, rule: PolicyId, now_ms: u64) -> Result<bool, StoreError> {
        Ok(self.rules.load()?.rules.iter().any(|r| r.id == rule && r.live_at(now_ms)))
    }

    /// Posts an entry to `rule`'s board at `now_ms` (module doc: the limits, the rule re-read under the lock). The
    /// entry as kept, what it pushed off and the claim it closed ([`Posted`]).
    pub async fn post(&self, rule: PolicyId, entry: NewEntry, now_ms: u64) -> Result<Posted, BoardError> {
        let text = check_text(&entry.text)?;
        let paths = check_paths(&entry.paths)?;
        let path = self.path(rule);
        locked(&path, || async { Ok(self.post_locked(rule, entry, text, paths, now_ms).await) }).await?
    }

    async fn post_locked(
        &self,
        rule: PolicyId,
        entry: NewEntry,
        text: String,
        paths: Vec<String>,
        now_ms: u64,
    ) -> Result<Posted, BoardError> {
        let path = self.path(rule);
        if !self.rule_live(rule, now_ms)? {
            // Gone or expired: its board goes with it (we hold its lock).
            remove_if_there(&path)?;
            return Err(BoardError::NoRule(rule));
        }
        let mut file = self.read_file(rule)?;
        file.entries.retain(|e| e.live_at(now_ms));
        let own = |e: &BoardEntry| e.by == entry.by;
        let mut dropped = 0;
        let mut closed = None;
        match entry.kind {
            BoardKind::Claim => {
                let claims = file.entries.iter().filter(|e| own(e) && e.kind == BoardKind::Claim).count();
                if claims >= MAX_CLAIMS {
                    return Err(BoardError::Refused(format!(
                        "You hold {MAX_CLAIMS} claims on this board already, the most a chat may: post done (it closes \
                         your claim naming the same paths, else your oldest) or clear yours (board_clear), then claim \
                         again."
                    )));
                }
            }
            BoardKind::Note | BoardKind::Done => {
                if entry.kind == BoardKind::Done {
                    let mine = file.entries.iter().filter(|e| own(e) && e.kind == BoardKind::Claim);
                    let matching = mine.clone().find(|e| e.paths.iter().any(|p| paths.contains(p)));
                    if let Some(claim) = matching.or_else(|| mine.clone().next()).map(|e| e.id) {
                        file.entries.retain(|e| e.id != claim);
                        closed = Some(claim);
                    }
                }
                let notes = file.entries.iter().filter(|e| own(e) && e.kind != BoardKind::Claim).count();
                if notes >= MAX_NOTES
                    && let Some(oldest) = file.entries.iter().find(|e| own(e) && e.kind != BoardKind::Claim).map(|e| e.id)
                {
                    file.entries.retain(|e| e.id != oldest);
                    dropped += 1;
                }
            }
        }
        if file.entries.len() >= MAX_ENTRIES {
            match file.entries.iter().find(|e| own(e)).map(|e| e.id) {
                Some(oldest) => {
                    file.entries.retain(|e| e.id != oldest);
                    dropped += 1;
                }
                None => {
                    return Err(BoardError::Refused(format!(
                        "This board is full ({MAX_ENTRIES} entries, none of them yours): a post never pushes another \
                         chat's entry off. Entries go after 24 hours, or the user clears them."
                    )));
                }
            }
        }
        let kept = BoardEntry { id: file.next_id, by: entry.by, kind: entry.kind, text, paths, at_ms: now_ms };
        file.next_id += 1;
        file.entries.push(kept.clone());
        write_private_json(&path, &file).await?;
        Ok(Posted { entry: kept, dropped, closed })
    }

    /// Clears entries from `rule`'s board: how many went. Expired ones go too (not counted). Allowed whatever the
    /// rule's state: clearing only ever removes.
    pub async fn clear(&self, rule: PolicyId, what: Clear, now_ms: u64) -> Result<usize, BoardError> {
        let path = self.path(rule);
        let count = locked(&path, || async {
            if std::fs::symlink_metadata(&path).is_err() {
                return Ok(0);
            }
            let mut file = self.read_file(rule)?;
            file.entries.retain(|e| e.live_at(now_ms));
            let before = file.entries.len();
            file.entries.retain(|e| match &what {
                Clear::All => false,
                Clear::By(by) => &e.by != by,
                Clear::Entry(id, by) => e.id != *id || by.as_ref().is_some_and(|by| &e.by != by),
            });
            let count = before - file.entries.len();
            write_private_json(&path, &file).await?;
            Ok(count)
        })
        .await?;
        Ok(count)
    }

    /// Deletes `rule`'s board (its rule was revoked), under the board's lock. No board: nothing to do.
    pub async fn delete(&self, rule: PolicyId) -> Result<(), StoreError> {
        delete_board(&self.config_dir, rule).await
    }
}

/// Removes a board's file, if it's there (the caller holds its lock).
fn remove_if_there(path: &Path) -> Result<(), StoreError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(StoreError::Io(path.display().to_string(), e.to_string())),
    }
}

/// Deletes `rule`'s board under its lock ([`BoardStore::delete`]; [`crate::PolicyStore::revoke`] calls it). Always
/// locked, so a post holding the lock finishes first; no board is nothing to do.
pub(crate) async fn delete_board(config_dir: &Path, rule: PolicyId) -> Result<(), StoreError> {
    if !boards_dir(config_dir).exists() {
        // No board was ever made here (a post makes the folder under the lock, after reading the rules).
        return Ok(());
    }
    let path = board_path(config_dir, rule);
    locked(&path, || async { remove_if_there(&path) }).await
}

/// The rule ids that have a board file in `config_dir`.
fn board_ids(config_dir: &Path) -> Vec<PolicyId> {
    let Ok(dir) = std::fs::read_dir(boards_dir(config_dir)) else { return Vec::new() };
    dir.filter_map(|e| e.ok()?.file_name().to_str()?.strip_suffix(".json")?.parse::<u64>().ok().map(PolicyId)).collect()
}

/// Deletes every board whose rule isn't in `rules` and live at `now_ms` (`None`: only those whose rule is gone),
/// each under its lock.
pub(crate) async fn sweep_boards(
    config_dir: &Path,
    rules: &crate::policy::PolicySnapshot,
    now_ms: Option<u64>,
) -> Result<usize, StoreError> {
    let mut deleted = 0;
    for id in board_ids(config_dir) {
        let keep = rules.rules.iter().any(|r| r.id == id && now_ms.is_none_or(|now| r.live_at(now)));
        if !keep {
            delete_board(config_dir, id).await?;
            deleted += 1;
        }
    }
    Ok(deleted)
}

impl BoardStore {
    /// Deletes the boards of rules that are gone or have expired by `now_ms` (the desktop's rules look calls it): how
    /// many went.
    pub async fn sweep(&self, now_ms: u64) -> Result<usize, StoreError> {
        let rules = self.rules.load()?;
        sweep_boards(&self.config_dir, &rules, Some(now_ms)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::Mode;
    use crate::policy::tests::{HOME, scope};
    use crate::policy::{RuleBy, Subject};
    use crate::store::NewRule;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    const HOUR: u64 = 60 * 60 * 1000;

    async fn with_rule(dir: &Path, expires_at_ms: Option<u64>) -> (BoardStore, PolicyId) {
        let rules = PolicyStore::new(dir, Path::new(HOME)).unwrap();
        let rule = NewRule {
            from: Subject::Folder(scope("/home/me/acme")),
            to: Subject::Folder(scope("/home/me/acme")),
            mode: Mode::Send,
            mutual: true,
            expires_at_ms,
            by: RuleBy::Desktop,
            created_at_ms: 1,
            label: "acme".into(),
        };
        let (id, _) = rules.add(rule).await.unwrap();
        (BoardStore::new(dir, Path::new(HOME)).unwrap(), id)
    }

    fn post(by: &str, kind: BoardKind, text: &str) -> NewEntry {
        NewEntry { by: BoardBy::Chat(by.into()), kind, text: text.into(), paths: Vec::new() }
    }

    #[tokio::test]
    async fn posts_round_trip_privately_with_stamped_ids_and_times() {
        let tmp = tempfile::tempdir().unwrap();
        let (boards, rule) = with_rule(tmp.path(), None).await;
        assert_eq!(boards.path(rule), tmp.path().join(format!("mesh/boards/{}.json", rule.0)));
        assert!(boards.read(rule, 0).unwrap().is_empty());
        let mut entry = post("alpha", BoardKind::Claim, "  I'm on src/api  ");
        entry.paths = vec!["src/api".into(), "src/api".into()];
        let Posted { entry: kept, dropped, .. } = boards.post(rule, entry, 10).await.unwrap();
        assert_eq!((kept.id, kept.at_ms, kept.text.as_str(), kept.paths.len(), dropped), (1, 10, "I'm on src/api", 1, 0));
        boards.post(rule, post("beta", BoardKind::Note, "tests are green on main"), 20).await.unwrap();
        let read = boards.read(rule, 30).unwrap();
        assert_eq!(read.iter().map(|e| e.id).collect::<Vec<_>>(), [1, 2]);
        #[cfg(unix)]
        {
            let mode = std::fs::metadata(boards.path(rule)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[tokio::test]
    async fn the_limits_hold() {
        let tmp = tempfile::tempdir().unwrap();
        let (boards, rule) = with_rule(tmp.path(), None).await;
        // 280 characters (not bytes) is the most; one line only, nothing invisible.
        let long = "é".repeat(MAX_TEXT_CHARS);
        assert!(boards.post(rule, post("alpha", BoardKind::Note, &long), 1).await.is_ok());
        let over = "x".repeat(MAX_TEXT_CHARS + 1);
        assert!(matches!(boards.post(rule, post("alpha", BoardKind::Note, &over), 1).await, Err(BoardError::Refused(_))));
        for text in [
            "",
            "   ",
            "two\nlines",
            "a\ttab",
            "a\u{2028}b",
            "a\u{2029}b",
            "rm\u{202e}gpj",
            "a\u{2066}b",
            "zero\u{200b}width",
            "a\u{feff}b",
            "tag\u{e0041}",
        ] {
            assert!(
                matches!(boards.post(rule, post("alpha", BoardKind::Note, text), 1).await, Err(BoardError::Refused(_))),
                "{text:?}"
            );
        }
        let mut bidi_path = post("alpha", BoardKind::Note, "ok");
        bidi_path.paths = vec!["src/\u{202e}sj.exe".into()];
        assert!(matches!(boards.post(rule, bidi_path, 1).await, Err(BoardError::Refused(_))));
        // Five live claims per author; another author's are its own.
        for n in 0..MAX_CLAIMS {
            boards.post(rule, post("alpha", BoardKind::Claim, &format!("claim {n}")), 2).await.unwrap();
        }
        let refused = boards.post(rule, post("alpha", BoardKind::Claim, "one more"), 3).await.unwrap_err();
        assert!(refused.to_string().contains("post done"), "{refused}");
        boards.post(rule, post("beta", BoardKind::Claim, "beta's claim"), 3).await.unwrap();
        // A done closes the author's oldest claim (or the one naming its paths), never another author's.
        let done = boards.post(rule, post("alpha", BoardKind::Done, "claim 0 done"), 3).await.unwrap();
        let read = boards.read(rule, 3).unwrap();
        assert_eq!(read.iter().find(|e| Some(e.id) == done.closed).map(|e| e.text.as_str()), None);
        assert!(!read.iter().any(|e| e.text == "claim 0") && read.iter().any(|e| e.text == "beta's claim"));
        boards.post(rule, post("alpha", BoardKind::Claim, "claim again"), 3).await.unwrap();
        // Ten notes and dones per author: the eleventh drops the author's own oldest.
        for n in 0..MAX_NOTES {
            boards.post(rule, post("beta", BoardKind::Note, &format!("note {n}")), 4).await.unwrap();
        }
        let over = boards.post(rule, post("beta", BoardKind::Note, "note 10"), 4).await.unwrap();
        assert_eq!(over.dropped, 1);
        let read = boards.read(rule, 4).unwrap();
        let beta_notes: Vec<&str> = read
            .iter()
            .filter(|e| e.by == BoardBy::Chat("beta".into()) && e.kind == BoardKind::Note)
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!((beta_notes.len(), beta_notes[0]), (MAX_NOTES, "note 1"));
        // 50 entries: when full, a post drops the poster's own oldest; one with nothing there is refused, so nobody
        // pushes another's entry off.
        for chat in ["c1", "c2", "c3", "c4"] {
            for n in 0..MAX_NOTES {
                let _ = boards.post(rule, post(chat, BoardKind::Note, &format!("{chat} {n}")), 5).await;
            }
        }
        let read = boards.read(rule, 5).unwrap();
        assert_eq!(read.len(), MAX_ENTRIES, "full");
        let before: Vec<u64> = read.iter().filter(|e| e.by != BoardBy::Chat("c3".into())).map(|e| e.id).collect();
        assert!(matches!(boards.post(rule, post("newcomer", BoardKind::Note, "hi"), 6).await, Err(BoardError::Refused(_))));
        let pushed = boards.post(rule, post("c3", BoardKind::Note, "c3 again"), 6).await.unwrap();
        assert_eq!(pushed.dropped, 1);
        let after: Vec<u64> =
            boards.read(rule, 6).unwrap().iter().filter(|e| e.by != BoardBy::Chat("c3".into())).map(|e| e.id).collect();
        assert_eq!(before, after, "nobody else's entry went");
        // 24 hours: an entry is gone at its expiry, and its claim no longer counts.
        assert!(boards.read(rule, 6 + ENTRY_TTL_MS).unwrap().is_empty());
        for n in 0..MAX_CLAIMS {
            boards.post(rule, post("alpha", BoardKind::Claim, &format!("again {n}")), 6 + ENTRY_TTL_MS).await.unwrap();
        }
        assert_eq!(boards.read(rule, 7 + ENTRY_TTL_MS).unwrap().len(), MAX_CLAIMS, "only today's");
    }

    #[tokio::test]
    async fn a_done_closes_the_claim_naming_its_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let (boards, rule) = with_rule(tmp.path(), None).await;
        let claim = |text: &str, path: &str| NewEntry {
            by: BoardBy::Chat("alpha".into()),
            kind: BoardKind::Claim,
            text: text.into(),
            paths: vec![path.into()],
        };
        boards.post(rule, claim("api", "src/api"), 1).await.unwrap();
        let web = boards.post(rule, claim("web", "src/web"), 1).await.unwrap().entry.id;
        let done = NewEntry {
            by: BoardBy::Chat("alpha".into()),
            kind: BoardKind::Done,
            text: "web done".into(),
            paths: vec!["src/web".into()],
        };
        assert_eq!(boards.post(rule, done, 2).await.unwrap().closed, Some(web));
        let left: Vec<String> = boards.read(rule, 2).unwrap().into_iter().map(|e| e.text).collect();
        assert_eq!(left, ["api", "web done"]);
    }

    #[tokio::test]
    async fn expired_rules_boards_go_at_the_next_post_or_sweep() {
        let tmp = tempfile::tempdir().unwrap();
        let (boards, rule) = with_rule(tmp.path(), Some(10 * HOUR)).await;
        boards.post(rule, post("alpha", BoardKind::Note, "hi"), HOUR).await.unwrap();
        assert!(boards.post(rule, post("alpha", BoardKind::Note, "late"), 10 * HOUR).await.is_err());
        assert!(!boards.path(rule).exists(), "the expired rule's board went at the post");
        let (_, live) = with_rule(tmp.path(), None).await;
        let (_, expiring) = with_rule(tmp.path(), Some(5 * HOUR)).await;
        boards.post(live, post("alpha", BoardKind::Note, "stays"), HOUR).await.unwrap();
        boards.post(expiring, post("alpha", BoardKind::Note, "goes"), HOUR).await.unwrap();
        assert_eq!(boards.sweep(6 * HOUR).await.unwrap(), 1);
        assert!(boards.path(live).exists() && !boards.path(expiring).exists());
        // A board left by a rule that's gone goes at the next revoke of any rule.
        std::fs::copy(boards.path(live), boards.path(PolicyId(77))).unwrap();
        let (_, other) = with_rule(tmp.path(), None).await;
        PolicyStore::new(tmp.path(), Path::new(HOME)).unwrap().revoke(other).await.unwrap();
        assert!(!boards.path(PolicyId(77)).exists() && boards.path(live).exists());
    }

    #[tokio::test]
    async fn clears_take_what_was_asked_and_only_that() {
        let tmp = tempfile::tempdir().unwrap();
        let (boards, rule) = with_rule(tmp.path(), None).await;
        assert_eq!(boards.clear(rule, Clear::All, 1).await.unwrap(), 0, "no board: nothing");
        assert!(!boards.path(rule).exists(), "a clear doesn't make a board");
        for (by, text) in [("alpha", "a1"), ("beta", "b1"), ("alpha", "a2")] {
            boards.post(rule, post(by, BoardKind::Note, text), 1).await.unwrap();
        }
        assert_eq!(boards.clear(rule, Clear::By(BoardBy::Chat("alpha".into())), 2).await.unwrap(), 2);
        assert_eq!(boards.clear(rule, Clear::Entry(2, Some(BoardBy::Chat("alpha".into()))), 2).await.unwrap(), 0);
        assert_eq!(boards.clear(rule, Clear::Entry(2, None), 2).await.unwrap(), 1);
        assert!(boards.read(rule, 3).unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_board_lives_as_long_as_its_rule() {
        let tmp = tempfile::tempdir().unwrap();
        let (boards, rule) = with_rule(tmp.path(), Some(10 * HOUR)).await;
        boards.post(rule, post("alpha", BoardKind::Note, "hi"), HOUR).await.unwrap();
        assert!(matches!(boards.post(rule, post("alpha", BoardKind::Note, "late"), 10 * HOUR).await, Err(BoardError::NoRule(_))));
        assert!(matches!(boards.post(PolicyId(99), post("alpha", BoardKind::Note, "x"), 1).await, Err(BoardError::NoRule(_))));
        assert!(!boards.path(PolicyId(99)).exists(), "no rule: no board made");
        PolicyStore::new(tmp.path(), Path::new(HOME)).unwrap().revoke(rule).await.unwrap();
        assert!(!boards.path(rule).exists(), "revoking the rule deleted its board");
    }

    #[cfg(unix)]
    #[test]
    fn unknown_fields_other_rules_and_unsafe_files_are_refused_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let boards = BoardStore::new(tmp.path(), Path::new(HOME)).unwrap();
        let path = boards.path(PolicyId(1));
        let entry = r#"{"id": 1, "by": {"chat": "alpha"}, "kind": "note", "text": "hi", "at_ms": 1}"#;
        let file = |rule: u64, e: &str, extra: &str| {
            format!(r#"{{"version": 1, "rule": {rule}, "next_id": 2, "entries": [{e}]{extra}}}"#)
        };
        let write = |text: &str, mode: u32| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        write(&file(1, entry, ""), 0o600);
        assert_eq!(boards.read(PolicyId(1), 2).unwrap().len(), 1, "the control");
        for text in [
            file(1, entry, r#", "pinned": true"#),
            file(2, entry, ""),
            file(1, &entry.replace(r#""kind": "note""#, r#""kind": "order""#), ""),
            file(1, &entry.replace(r#""at_ms": 1"#, r#""at_ms": 1, "from": "beta""#), ""),
            file(1, &format!("{entry}, {entry}"), ""),
            file(1, &entry.replace("\"hi\"", &format!("\"{}\"", "x".repeat(MAX_TEXT_CHARS + 1))), ""),
        ] {
            write(&text, 0o600);
            assert!(boards.read(PolicyId(1), 2).is_err(), "{text}");
        }
        write(&file(1, entry, ""), 0o644);
        assert!(matches!(boards.read(PolicyId(1), 2), Err(StoreError::Unsafe(_))));
    }

    #[tokio::test]
    async fn concurrent_posts_from_many_writers_all_land() {
        let tmp = tempfile::tempdir().unwrap();
        let (boards, rule) = with_rule(tmp.path(), None).await;
        let mut tasks = Vec::new();
        for n in 0..20 {
            let boards = BoardStore::new(tmp.path(), Path::new(HOME)).unwrap();
            tasks.push(tokio::spawn(async move {
                boards.post(rule, post(&format!("chat{n}"), BoardKind::Note, &format!("n{n}")), 1).await.unwrap()
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        let read = boards.read(rule, 2).unwrap();
        assert_eq!(read.len(), 20);
        let mut ids: Vec<u64> = read.iter().map(|e| e.id).collect();
        ids.dedup();
        assert_eq!(ids.len(), 20, "every post got its own id");
    }
}
