//! The person's service grants on disk: `~/.marv/mesh/grants.json`, beside `policies.json`, and what each spent today
//! in `~/.marv/mesh/grant-spend.json`. New in marv-rs.
//!
//! The same store code as the rules ([`crate::store`]): only the user's own config folder (absolute; never a
//! project's), versioned JSON with unknown fields refused, 0600 in a 0700 folder, atomic, one read-change-write at a
//! time across Marv processes, a generation bumped by every change, ids never reused, and a symlink or a file others
//! can read or write refused whole (it then counts as no grants at all).
//!
//! Only the desktop writes `grants.json`, from the person's own clicks. The spend file only ever counts up within a
//! day: what it holds can pause a grant, never widen one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::grant::{Grant, GrantBy, GrantId, GrantSnapshot, Spend, SpendCap};
use crate::identity::{AgentId, FolderScope};
use crate::store::{StoreError, locked, read_private_json, write_private_json};

/// The grants file's format version.
pub const GRANT_VERSION: u32 = 1;

/// `<config dir>/mesh/grants.json`.
pub fn grants_path(config_dir: &Path) -> PathBuf {
    config_dir.join("mesh").join("grants.json")
}

/// `<config dir>/mesh/grant-spend.json`.
pub fn spend_path(config_dir: &Path) -> PathBuf {
    config_dir.join("mesh").join("grant-spend.json")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantFile {
    version: u32,
    generation: u64,
    next_id: u64,
    grants: Vec<Grant>,
}

impl Default for GrantFile {
    fn default() -> Self {
        Self { version: GRANT_VERSION, generation: 0, next_id: 1, grants: Vec::new() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpendLine {
    grant: GrantId,
    day: u64,
    usd_micros: u64,
    tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpendFile {
    version: u32,
    spend: Vec<SpendLine>,
}

/// A grant the person is making: the store gives it its id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewGrant {
    pub chat: AgentId,
    pub folder: FolderScope,
    pub expires_at_ms: Option<u64>,
    pub cap: SpendCap,
    pub by: GrantBy,
    pub created_at_ms: u64,
    pub label: String,
}

/// What a change to the grants did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantChange {
    pub snapshot: GrantSnapshot,
    /// Grants the change took away (a new grant for a chat replaces its old one).
    pub replaced: Vec<GrantId>,
}

/// One user's grants file (and its spend file).
#[derive(Debug, Clone)]
pub struct GrantStore {
    path: PathBuf,
    spend_path: PathBuf,
    home: PathBuf,
}

impl GrantStore {
    /// The store in `config_dir` (absolute: the user's config folder). `home` is what a grant's folder may not be, or
    /// be above.
    pub fn new(config_dir: &Path, home: &Path) -> Result<Self, StoreError> {
        if !config_dir.is_absolute() {
            return Err(StoreError::NotAbsolute(config_dir.display().to_string()));
        }
        Ok(Self { path: grants_path(config_dir), spend_path: spend_path(config_dir), home: home.to_path_buf() })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn spend_path(&self) -> &Path {
        &self.spend_path
    }

    fn shown(&self) -> String {
        self.path.display().to_string()
    }

    /// The grants now. No file: none (generation 0). A file that fails any check is an error, never part of it.
    pub fn load(&self) -> Result<GrantSnapshot, StoreError> {
        let file = self.read()?;
        Ok(GrantSnapshot { generation: file.generation, grants: file.grants })
    }

    fn read(&self) -> Result<GrantFile, StoreError> {
        let Some(file) = read_private_json::<GrantFile>(&self.path, GRANT_VERSION)? else {
            return Ok(GrantFile::default());
        };
        let mut ids = std::collections::HashSet::new();
        for grant in &file.grants {
            grant.check(&self.home).map_err(|e| StoreError::Parse(self.shown(), e.to_string()))?;
            if !ids.insert(grant.id) || grant.id.0 >= file.next_id {
                return Err(StoreError::Parse(self.shown(), format!("{} is listed twice or past next_id", grant.id)));
            }
        }
        Ok(file)
    }

    async fn change<T: Send>(
        &self,
        edit: impl FnOnce(&mut GrantFile) -> Result<T, StoreError> + Send,
    ) -> Result<(T, GrantSnapshot), StoreError> {
        locked(&self.path, || async {
            let mut file = self.read()?;
            let out = edit(&mut file)?;
            file.generation += 1;
            for grant in &file.grants {
                grant.check(&self.home).map_err(|e| StoreError::Parse(self.shown(), e.to_string()))?;
            }
            write_private_json(&self.path, &file).await?;
            Ok((out, GrantSnapshot { generation: file.generation, grants: file.grants }))
        })
        .await
    }

    /// Adds a grant the person made, replacing any the chat had (one grant per chat).
    pub async fn add(&self, grant: NewGrant) -> Result<(GrantId, GrantChange), StoreError> {
        let ((id, replaced), snapshot) = self
            .change(|file| {
                let id = GrantId(file.next_id);
                file.next_id += 1;
                let NewGrant { chat, folder, expires_at_ms, cap, by, created_at_ms, label } = grant;
                let replaced: Vec<GrantId> = file.grants.iter().filter(|g| g.chat == chat).map(|g| g.id).collect();
                file.grants.retain(|g| g.chat != chat);
                file.grants.push(Grant { id, chat, folder, expires_at_ms, cap, by, created_at_ms, label });
                Ok((id, replaced))
            })
            .await?;
        Ok((id, GrantChange { snapshot, replaced }))
    }

    /// Revokes a grant: it goes, and the generation moves on (the chat's next unattended call asks).
    pub async fn revoke(&self, id: GrantId) -> Result<GrantSnapshot, StoreError> {
        self.change(|file| {
            let before = file.grants.len();
            file.grants.retain(|g| g.id != id);
            if file.grants.len() == before { Err(StoreError::Parse(self.shown(), format!("there's no {id}"))) } else { Ok(()) }
        })
        .await
        .map(|((), snapshot)| snapshot)
    }

    /// Sets a grant's spend cap (the person raising it after a pause, or lowering it).
    pub async fn set_cap(&self, id: GrantId, cap: SpendCap) -> Result<GrantSnapshot, StoreError> {
        self.change(|file| match file.grants.iter_mut().find(|g| g.id == id) {
            Some(grant) => {
                grant.cap = cap;
                Ok(())
            }
            None => Err(StoreError::Parse(self.shown(), format!("there's no {id}"))),
        })
        .await
        .map(|((), snapshot)| snapshot)
    }

    /// What each grant spent, as last saved. No file: nothing. A refused file is an error (the caller then treats
    /// every grant as spent for the day, never as unspent).
    pub fn load_spend(&self) -> Result<BTreeMap<GrantId, Spend>, StoreError> {
        let file = read_private_json::<SpendFile>(&self.spend_path, GRANT_VERSION)?;
        Ok(file
            .map(|f| {
                f.spend.into_iter().map(|l| (l.grant, Spend { day: l.day, usd_micros: l.usd_micros, tokens: l.tokens })).collect()
            })
            .unwrap_or_default())
    }

    /// Saves what each grant spent (the caller's in-memory book, whole), under the file lock.
    pub async fn save_spend(&self, spend: &BTreeMap<GrantId, Spend>) -> Result<(), StoreError> {
        let file = SpendFile {
            version: GRANT_VERSION,
            spend: spend
                .iter()
                .map(|(grant, s)| SpendLine { grant: *grant, day: s.day, usd_micros: s.usd_micros, tokens: s.tokens })
                .collect(),
        };
        locked(&self.spend_path, || async { write_private_json(&self.spend_path, &file).await }).await
    }
}

/// [`GrantStore::load`] as the session consults it: read again only when the file changed (inode, mode, length,
/// modified or change time), so a grant revoked in the desktop holds at the chat's next call. A file that's refused
/// counts as no grants at all; [`FileGrants::problem`] says why.
pub struct FileGrants {
    store: GrantStore,
    cache: std::sync::Mutex<Cached>,
}

type Stamp = marv_fs::mode::FileStamp;

#[derive(Default)]
struct Cached {
    loaded: bool,
    stamp: Option<Stamp>,
    snapshot: std::sync::Arc<GrantSnapshot>,
    problem: Option<String>,
}

impl FileGrants {
    pub fn new(store: GrantStore) -> Self {
        Self { store, cache: std::sync::Mutex::new(Cached::default()) }
    }

    pub fn store(&self) -> &GrantStore {
        &self.store
    }

    /// The grants now.
    pub fn snapshot(&self) -> std::sync::Arc<GrantSnapshot> {
        let stamp = std::fs::symlink_metadata(self.store.path()).ok().map(|m| marv_fs::mode::stamp(&m));
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if !cache.loaded || cache.stamp != stamp {
            let (snapshot, problem) = match self.store.load() {
                Ok(snapshot) => (snapshot, None),
                Err(e) => (GrantSnapshot::default(), Some(e.to_string())),
            };
            *cache = Cached { loaded: true, stamp, snapshot: std::sync::Arc::new(snapshot), problem };
        }
        cache.snapshot.clone()
    }

    /// Why the file isn't used, if it isn't (shown to the person, never to a model).
    pub fn problem(&self) -> Option<String> {
        self.snapshot();
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).problem.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grant::tests::chat_id;
    use crate::grant::{DAY_MS, grant_for};
    use crate::policy::tests::{HOME, scope};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn store(dir: &Path) -> GrantStore {
        GrantStore::new(dir, Path::new(HOME)).unwrap()
    }

    fn new_grant(chat: u8) -> NewGrant {
        NewGrant {
            chat: chat_id(chat),
            folder: scope("/home/me/acme"),
            expires_at_ms: None,
            cap: SpendCap::default(),
            by: GrantBy::Desktop,
            created_at_ms: 5,
            label: "acme".into(),
        }
    }

    fn write(path: &Path, text: &str, mode: u32) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        #[cfg(not(unix))]
        let _ = mode;
    }

    #[tokio::test]
    async fn grants_round_trip_privately_one_per_chat() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(tmp.path());
        assert_eq!(store.path(), tmp.path().join("mesh/grants.json"));
        assert_eq!(store.load().unwrap(), GrantSnapshot::default());
        let (first, c1) = store.add(new_grant(1)).await.unwrap();
        let (second, c2) = store.add(new_grant(2)).await.unwrap();
        assert_eq!((first, second, c1.snapshot.generation, c2.snapshot.generation), (GrantId(1), GrantId(2), 1, 2));
        let (third, c3) = store.add(new_grant(1)).await.unwrap();
        assert_eq!((third, c3.replaced.as_slice()), (GrantId(3), [first].as_slice()), "a chat's new grant replaces its old");
        assert_eq!(c3.snapshot.grants.len(), 2);
        #[cfg(unix)]
        assert_eq!(std::fs::metadata(store.path()).unwrap().permissions().mode() & 0o777, 0o600);
        let raised = store.set_cap(third, SpendCap { usd_micros: 5_000_000, tokens: 1 }).await.unwrap();
        assert_eq!(raised.for_chat(&chat_id(1)).unwrap().cap.usd_micros, 5_000_000);
        let s = store.revoke(third).await.unwrap();
        assert_eq!((s.generation, s.grants.len()), (5, 1));
        assert!(store.revoke(third).await.is_err());
        assert!(grant_for(&s, &chat_id(1), 0, |_| Spend::default()).is_err(), "revoked: gone");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_grants_follow_every_change_and_a_refused_file_is_no_grants() {
        let tmp = tempfile::tempdir().unwrap();
        let grants = FileGrants::new(store(tmp.path()));
        assert!(grants.snapshot().grants.is_empty());
        let (id, _) = store(tmp.path()).add(new_grant(1)).await.unwrap();
        assert_eq!(grants.snapshot().grants.len(), 1, "a grant made by another process is seen at once");
        store(tmp.path()).revoke(id).await.unwrap();
        assert!(grants.snapshot().grants.is_empty(), "revoked: gone at the next look");
        store(tmp.path()).add(new_grant(1)).await.unwrap();
        std::fs::set_permissions(store(tmp.path()).path(), std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(grants.snapshot().grants.is_empty(), "a file others can read is no grants at all");
        assert!(grants.problem().unwrap().contains("can be read or changed by others"));
    }

    #[cfg(unix)]
    #[test]
    fn unknown_fields_other_makers_symlinks_and_home_are_refused_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(tmp.path());
        let grant = r#"{"id": 1, "chat": "00000001-0000-4000-8000-000000000000", "folder": "/home/me/acme", "by": "desktop", "created_at_ms": 1}"#;
        let file = |g: &str, extra: &str| format!(r#"{{"version": 1, "generation": 1, "next_id": 9, "grants": [{g}]{extra}}}"#);
        write(store.path(), &file(grant, ""), 0o600);
        assert_eq!(store.load().unwrap().grants[0].cap, SpendCap::default(), "the control: the default cap");
        let cases = [
            file(grant, r#", "allow_all": true"#),
            file(&grant.replace(r#""by": "desktop""#, r#""by": "model""#), ""),
            file(&grant.replace(r#""by": "desktop""#, r#""by": "desktop", "unlimited": true"#), ""),
            file(&grant.replace("/home/me/acme", "/home/me"), ""),
            file(&grant.replace("/home/me/acme", "/home/me/../x"), ""),
            file(&grant.replace(r#""chat": "00000001-0000-4000-8000-000000000000""#, r#""folder_all": "/home/me""#), ""),
            file(&format!("{grant}, {grant}"), ""),
            r#"{"version": 2, "generation": 1, "next_id": 1, "grants": []}"#.to_string(),
        ];
        for text in cases {
            write(store.path(), &text, 0o600);
            assert!(store.load().is_err(), "{text}");
        }
        write(store.path(), &file(grant, ""), 0o640);
        assert!(matches!(store.load(), Err(StoreError::Unsafe(_))));
        std::fs::remove_file(store.path()).unwrap();
        let elsewhere = tmp.path().join("elsewhere.json");
        write(&elsewhere, &file(grant, ""), 0o600);
        std::os::unix::fs::symlink(&elsewhere, store.path()).unwrap();
        assert!(matches!(store.load(), Err(StoreError::Unsafe(_))));
        assert!(matches!(GrantStore::new(Path::new(".marv"), Path::new(HOME)), Err(StoreError::NotAbsolute(_))));
    }

    #[tokio::test]
    async fn spend_round_trips_privately() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(tmp.path());
        assert!(store.load_spend().unwrap().is_empty());
        let mut book = BTreeMap::new();
        let mut spend = Spend::default();
        spend.charge(3 * DAY_MS, Some(0.25), 0);
        book.insert(GrantId(4), spend);
        store.save_spend(&book).await.unwrap();
        assert_eq!(store.load_spend().unwrap(), book);
        #[cfg(unix)]
        assert_eq!(std::fs::metadata(store.spend_path()).unwrap().permissions().mode() & 0o777, 0o600);
        write(store.spend_path(), r#"{"version": 1, "spend": [], "reset": true}"#, 0o600);
        assert!(store.load_spend().is_err());
    }
}
