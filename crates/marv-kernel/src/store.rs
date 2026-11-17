//! The person's rules on disk: `~/.marv/mesh/policies.json` (runtime-kernel phase 2b, decision D4). New in marv-rs.
//!
//! - **Where:** `<config dir>/mesh/policies.json`, the config dir being the user's (`~/.marv`, or `MARV_CONFIG_DIR`
//!   from the real environment), given as an absolute path. A project's folder or its `.marv/config.json` is never a
//!   source of rules: nothing here takes a project root, and a relative config dir is refused.
//! - **Format:** versioned JSON (`version`, `generation`, `next_id`, `rules`); unknown fields refused everywhere, as
//!   for saved checkpoints. Every rule is checked when read ([`Policy::check`]: `send` or `call`, folders never home
//!   or above); a file that fails any check is refused whole, never half-used.
//! - **Writes:** 0600 in a 0700 folder, atomic (`marv_fs::write_private`), one read-change-write at a time across
//!   Marv processes (`marv_fs::with_file_lock`). Each change bumps the generation; ids are never reused.
//! - **Reads:** a file that others can read or write, or a symlink, is refused (rules are the person's).
//!
//! The mesh reads it through [`FileRules`] (re-read when the file changes). Nothing outside tests writes it yet (the
//! desktop's Mesh panel and `marv mesh allow` come later).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::mode::Mode;
use crate::policy::{Policy, PolicyId, PolicySnapshot, RuleBy, RuleError, Subject};

/// The file's format version.
pub const POLICY_VERSION: u32 = 1;

/// `<config dir>/mesh/policies.json`.
pub fn policies_path(config_dir: &Path) -> PathBuf {
    config_dir.join("mesh").join("policies.json")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFile {
    version: u32,
    generation: u64,
    next_id: u64,
    rules: Vec<Policy>,
}

impl Default for PolicyFile {
    fn default() -> Self {
        Self { version: POLICY_VERSION, generation: 0, next_id: 1, rules: Vec::new() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    #[error("the config folder {0} isn't an absolute path: rules are read only from the user's own config folder")]
    NotAbsolute(String),
    #[error("{0}: {1}")]
    Io(String, String),
    #[error("{0} is a symlink or can be read or changed by others: not reading it")]
    Unsafe(String),
    #[error("{0} is version {1}, this Marv reads version {2}")]
    Version(String, u32, u32),
    #[error("{0} isn't a file Marv can read: {1}")]
    Parse(String, String),
    #[error("{0}: {1}")]
    Rule(String, RuleError),
    #[error("{0}")]
    Lock(String),
    #[error("there's no {0}")]
    NoSuchRule(PolicyId),
}

/// A rule the person is making: the store gives it its id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRule {
    pub from: Subject,
    pub to: Subject,
    pub mode: Mode,
    pub mutual: bool,
    pub expires_at_ms: Option<u64>,
    pub by: RuleBy,
    pub created_at_ms: u64,
    pub label: String,
}

/// The rules file of one user.
#[derive(Debug, Clone)]
pub struct PolicyStore {
    path: PathBuf,
    home: PathBuf,
}

impl PolicyStore {
    /// The store in `config_dir` (absolute: the user's config folder). `home` is what rule folders may not be, or be
    /// above.
    pub fn new(config_dir: &Path, home: &Path) -> Result<Self, StoreError> {
        if !config_dir.is_absolute() {
            return Err(StoreError::NotAbsolute(config_dir.display().to_string()));
        }
        Ok(Self { path: policies_path(config_dir), home: home.to_path_buf() })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn shown(&self) -> String {
        self.path.display().to_string()
    }

    /// The rules now. No file: none (generation 0). A file that fails any check is an error, never part of it.
    pub fn load(&self) -> Result<PolicySnapshot, StoreError> {
        let file = self.read()?;
        Ok(PolicySnapshot { generation: file.generation, rules: file.rules })
    }

    fn read(&self) -> Result<PolicyFile, StoreError> {
        let Some(file) = read_private_json::<PolicyFile>(&self.path, POLICY_VERSION)? else {
            return Ok(PolicyFile::default());
        };
        let mut ids = std::collections::HashSet::new();
        for rule in &file.rules {
            rule.check(&self.home).map_err(|e| StoreError::Rule(self.shown(), e))?;
            if !ids.insert(rule.id) || rule.id.0 >= file.next_id {
                return Err(StoreError::Parse(self.shown(), format!("{} is listed twice or past next_id", rule.id)));
            }
        }
        Ok(file)
    }

    /// One change under the file lock: read, change, bump the generation, write (0600, atomic).
    async fn change<T: Send>(
        &self,
        edit: impl FnOnce(&mut PolicyFile) -> Result<T, StoreError> + Send,
    ) -> Result<(T, PolicySnapshot), StoreError> {
        locked(&self.path, || async {
            let mut file = self.read()?;
            let out = edit(&mut file)?;
            file.generation += 1;
            for rule in &file.rules {
                rule.check(&self.home).map_err(|e| StoreError::Rule(self.shown(), e))?;
            }
            write_private_json(&self.path, &file).await?;
            Ok((out, PolicySnapshot { generation: file.generation, rules: file.rules }))
        })
        .await
    }

    /// Adds a rule the person made; its id is the next one, never one used before.
    pub async fn add(&self, rule: NewRule) -> Result<(PolicyId, PolicySnapshot), StoreError> {
        self.change(|file| {
            let id = PolicyId(file.next_id);
            file.next_id += 1;
            let NewRule { from, to, mode, mutual, expires_at_ms, by, created_at_ms, label } = rule;
            file.rules.push(Policy { id, from, to, mode, mutual, expires_at_ms, by, created_at_ms, label });
            Ok(id)
        })
        .await
    }

    /// Revokes a rule: it goes, and the generation moves on (a capability issued under it no longer holds). Its team
    /// board (runtime-kernel §19.2) is deleted with it, under the board's lock: a post that races the revoke either
    /// lands before and is deleted, or re-reads the rules under that lock and is refused. Boards left by rules that
    /// are gone go too.
    pub async fn revoke(&self, id: PolicyId) -> Result<PolicySnapshot, StoreError> {
        let snapshot = self
            .change(|file| {
                let before = file.rules.len();
                file.rules.retain(|r| r.id != id);
                if file.rules.len() == before { Err(StoreError::NoSuchRule(id)) } else { Ok(()) }
            })
            .await
            .map(|((), snapshot)| snapshot)?;
        if let Some(config_dir) = self.path.parent().and_then(Path::parent) {
            crate::board::delete_board(config_dir, id).await?;
            // And any other board whose rule is gone (one a crash left behind).
            crate::board::sweep_boards(config_dir, &snapshot, None).await?;
        }
        Ok(snapshot)
    }
}

/// Reads one of the person's private JSON files: none when it isn't there. Refused when it's a symlink or not a plain
/// file, when others can read or write it, when it's another `version` (checked first, so a newer file says so rather
/// than failing on a field this Marv doesn't know), or when it doesn't parse (unknown fields included).
pub(crate) fn read_private_json<T: serde::de::DeserializeOwned>(path: &Path, expected: u32) -> Result<Option<T>, StoreError> {
    let shown = || path.display().to_string();
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(StoreError::Io(shown(), e.to_string())),
    };
    if !meta.is_file() || marv_fs::mode::mode_of(&meta.permissions()) & 0o077 != 0 {
        return Err(StoreError::Unsafe(shown()));
    }
    let text = std::fs::read_to_string(path).map_err(|e| StoreError::Io(shown(), e.to_string()))?;
    let version = serde_json::from_str::<serde_json::Value>(&text)
        .map_err(|e| StoreError::Parse(shown(), e.to_string()))?
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| StoreError::Parse(shown(), "no version".into()))?;
    if version != u64::from(expected) {
        return Err(StoreError::Version(shown(), u32::try_from(version).unwrap_or(u32::MAX), expected));
    }
    serde_json::from_str(&text).map(Some).map_err(|e| StoreError::Parse(shown(), e.to_string()))
}

/// Writes one of the person's private JSON files: 0600 in a 0700 folder, atomic.
pub(crate) async fn write_private_json<T: Serialize>(path: &Path, value: &T) -> Result<(), StoreError> {
    let shown = || path.display().to_string();
    let text = serde_json::to_string_pretty(value).map_err(|e| StoreError::Parse(shown(), e.to_string()))?;
    marv_fs::write_private(path, &text).await.map_err(|e| StoreError::Io(shown(), e.to_string()))
}

/// Runs `work` holding the file's cross-process lock (one read-change-write at a time across Marv processes).
pub(crate) async fn locked<T, F, Fut>(path: &Path, work: F) -> Result<T, StoreError>
where
    F: FnOnce() -> Fut + Send,
    Fut: std::future::Future<Output = Result<T, StoreError>> + Send,
{
    marv_fs::with_file_lock(path, marv_fs::LockOptions::default(), work).await.map_err(|e| StoreError::Lock(e.to_string()))?
}

/// The rules as the mesh consults them: a snapshot, cheap to ask for on every message.
pub trait Rules: Send + Sync {
    fn snapshot(&self) -> std::sync::Arc<PolicySnapshot>;
}

/// [`Rules`] from the person's file: read again only when it changed (its inode, mode, length, or modified or change
/// time: a `chmod` that makes it readable by others changes only the last two, and must refuse it at once), so a rule
/// made or revoked in the desktop or with `marv mesh allow` reaches every running Marv at its next message. A file
/// that's refused ([`PolicyStore::load`]: unsafe, unreadable, invalid) counts as no rules at all, never as part of
/// them; [`FileRules::problem`] says why.
pub struct FileRules {
    store: PolicyStore,
    cache: std::sync::Mutex<Cached>,
}

/// What identifies one state of the file: inode, mode, length, modified time and change time (seconds, nanoseconds).
type Stamp = marv_fs::mode::FileStamp;

#[derive(Default)]
struct Cached {
    loaded: bool,
    stamp: Option<Stamp>,
    snapshot: std::sync::Arc<PolicySnapshot>,
    problem: Option<String>,
}

impl FileRules {
    pub fn new(store: PolicyStore) -> Self {
        Self { store, cache: std::sync::Mutex::new(Cached::default()) }
    }

    /// Why the file isn't used, if it isn't (shown to the person, never to a model).
    pub fn problem(&self) -> Option<String> {
        self.snapshot();
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).problem.clone()
    }
}

impl Rules for FileRules {
    fn snapshot(&self) -> std::sync::Arc<PolicySnapshot> {
        let stamp = std::fs::symlink_metadata(self.store.path()).ok().map(|m| marv_fs::mode::stamp(&m));
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        // Read once at first, then whenever the file changed, appeared or went.
        if !cache.loaded || cache.stamp != stamp {
            let (snapshot, problem) = match self.store.load() {
                Ok(snapshot) => (snapshot, None),
                Err(e) => (PolicySnapshot::default(), Some(e.to_string())),
            };
            *cache = Cached { loaded: true, stamp, snapshot: std::sync::Arc::new(snapshot), problem };
        }
        cache.snapshot.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::tests::{HOME, folder};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn store(dir: &Path) -> PolicyStore {
        PolicyStore::new(dir, Path::new(HOME)).unwrap()
    }

    fn new_rule() -> NewRule {
        NewRule {
            from: folder("/home/me/acme"),
            to: folder("/home/me/acme"),
            mode: Mode::Send,
            mutual: false,
            expires_at_ms: None,
            by: RuleBy::Desktop,
            created_at_ms: 5,
            label: "Acme".into(),
        }
    }

    #[cfg(unix)]
    fn mode_of(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[tokio::test]
    async fn rules_round_trip_privately_and_each_change_bumps_the_generation() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(tmp.path());
        assert_eq!(store.path(), tmp.path().join("mesh/policies.json"));
        assert_eq!(store.load().unwrap(), PolicySnapshot::default(), "no file: no rules");
        let (first, s1) = store.add(new_rule()).await.unwrap();
        let (second, s2) = store.add(new_rule()).await.unwrap();
        assert_eq!((first, second, s1.generation, s2.generation), (PolicyId(1), PolicyId(2), 1, 2));
        assert_eq!(store.load().unwrap(), s2);
        #[cfg(unix)]
        {
            assert_eq!(mode_of(store.path()), 0o600);
            assert_eq!(mode_of(&tmp.path().join("mesh")), 0o700);
        }
        let s3 = store.revoke(first).await.unwrap();
        assert_eq!((s3.generation, s3.rules.len()), (3, 1));
        assert_eq!(store.revoke(first).await, Err(StoreError::NoSuchRule(first)));
        assert_eq!(store.load().unwrap().generation, 3, "a refused change writes nothing");
        let (third, _) = store.add(new_rule()).await.unwrap();
        assert_eq!(third, PolicyId(3), "ids are never reused");
        let names: Vec<String> =
            std::fs::read_dir(tmp.path().join("mesh")).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        assert_eq!(names, ["policies.json"], "no temp or lock files left");
    }

    #[tokio::test]
    async fn two_writers_never_lose_a_rule() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(tmp.path());
        // Polled together: each waits its turn on the lock, and none loses another's rule.
        let adds = (0..8).map(|_| store.add(new_rule()));
        for added in futures::future::join_all(adds).await {
            added.unwrap();
        }
        let s = store.load().unwrap();
        assert_eq!((s.generation, s.rules.len()), (8, 8));
    }

    fn write(store: &PolicyStore, text: &str, mode: u32) {
        std::fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        std::fs::write(store.path(), text).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(mode)).unwrap();
        #[cfg(not(unix))]
        let _ = mode;
    }

    const RULE: &str = r#"{"id": 1, "from": {"folder": "/home/me/a"}, "to": {"folder": "/home/me/b"}, "mode": "send", "by": "cli", "created_at_ms": 1}"#;

    #[test]
    fn a_hand_written_file_is_read_when_it_passes_every_check() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(tmp.path());
        write(&store, &format!(r#"{{"version": 1, "generation": 4, "next_id": 2, "rules": [{RULE}]}}"#), 0o600);
        let s = store.load().unwrap();
        assert_eq!((s.generation, s.rules.len(), s.rules[0].mutual, s.rules[0].label.as_str()), (4, 1, false, ""));
    }

    #[test]
    fn unknown_fields_bad_rules_and_other_versions_are_refused_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(tmp.path());
        let file =
            |rules: &str, extra: &str| format!(r#"{{"version": 1, "generation": 1, "next_id": 9, "rules": [{rules}]{extra}}}"#);
        let cases = [
            file(RULE, r#", "allow_all": true"#),
            file(&RULE.replace(r#""by": "cli""#, r#""by": "cli", "include_everyone": true"#), ""),
            file(&RULE.replace(r#""by": "cli""#, r#""by": "model""#), ""),
            file(&RULE.replace("send", "watch"), ""),
            file(&RULE.replace("/home/me/b", "/home/me"), ""),
            file(&RULE.replace("/home/me/b", "/home"), ""),
            file(&RULE.replace(r#"{"folder": "/home/me/b"}"#, r#"{"glob": "/home/me/*"}"#), ""),
            file(&format!("{RULE}, {RULE}"), ""),
            r#"{"version": 2, "generation": 1, "next_id": 1, "rules": [], "new": 1}"#.to_string(),
            "not json".to_string(),
        ];
        for text in cases {
            write(&store, &text, 0o600);
            assert!(store.load().is_err(), "{text}");
        }
        write(&store, r#"{"version": 2, "generation": 1, "next_id": 1, "rules": [], "new": 1}"#, 0o600);
        assert!(matches!(store.load(), Err(StoreError::Version(_, 2, 1))));
    }

    #[cfg(unix)]
    #[test]
    fn a_file_others_can_read_or_a_symlink_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(tmp.path());
        let good = r#"{"version": 1, "generation": 1, "next_id": 1, "rules": []}"#;
        for mode in [0o644, 0o620, 0o604] {
            write(&store, good, mode);
            assert!(matches!(store.load(), Err(StoreError::Unsafe(_))), "{mode:o}");
        }
        std::fs::remove_file(store.path()).unwrap();
        let elsewhere = tmp.path().join("elsewhere.json");
        std::fs::write(&elsewhere, good).unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&elsewhere, store.path()).unwrap();
        assert!(matches!(store.load(), Err(StoreError::Unsafe(_))));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_rules_follow_every_change_and_a_refused_file_is_no_rules() {
        let tmp = tempfile::tempdir().unwrap();
        let rules = FileRules::new(store(tmp.path()));
        assert!(rules.snapshot().rules.is_empty(), "no file: no rules");
        let (id, _) = store(tmp.path()).add(new_rule()).await.unwrap();
        assert_eq!(rules.snapshot().rules.len(), 1, "a rule made by another process is seen at once");
        store(tmp.path()).revoke(id).await.unwrap();
        assert!(rules.snapshot().rules.is_empty(), "revoked: gone at the next look");
        store(tmp.path()).add(new_rule()).await.unwrap();
        assert_eq!(rules.snapshot().rules.len(), 1);
        std::fs::set_permissions(store(tmp.path()).path(), std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(rules.snapshot().rules.is_empty(), "a file others can read is no rules at all");
        assert!(rules.problem().unwrap().contains("can be read or changed by others"));
        std::fs::remove_file(store(tmp.path()).path()).unwrap();
        assert!(rules.snapshot().rules.is_empty() && rules.problem().is_none(), "removed: no rules, no problem");
    }

    #[test]
    fn only_an_absolute_config_folder_holds_rules() {
        assert!(matches!(PolicyStore::new(Path::new(".marv"), Path::new(HOME)), Err(StoreError::NotAbsolute(_))));
        assert_eq!(policies_path(Path::new("/home/me/.marv")), Path::new("/home/me/.marv/mesh/policies.json"));
    }
}
