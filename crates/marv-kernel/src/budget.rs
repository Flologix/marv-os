//! Budgets, nested: global ⊇ folder ⊇ chat ⊇ grant (runtime-kernel §16.6–§16.7, decisions A6, A7; §8.5). New in
//! marv-rs.
//!
//! Each is a cap a (UTC) day, dollars where the requests have a price and tokens where they don't ([`SpendCap`], as a
//! grant's), and each is optional: **only the person sets one** (Marv desktop's Usage view, or `marv kernel budget`
//! at a terminal, which asks y/N first), in `~/.marv/mesh/budgets.json` ([`BudgetStore`]): the user's own config
//! folder, never a project's; no tool, mesh message, project config, environment variable or flag sets, widens or
//! removes one ([`BudgetBy`] has one variant). By default none is set: only a grant's own cap (§10) is on.
//!
//! A request is charged to every level above it (the meters, [`crate::meter`]) and runs only if each that is set has
//! room ([`check`]): a folder budget covers every chat in the folder or below it. At 80% of one the person is warned
//! ([`WARN_PERCENT`]). What happens at a limit is the session's: a person's turn asks before its next request, an
//! unattended one pauses and the person is notified; never a silent move to another core.
//!
//! A budgets file that can't be read (refused by the store's checks) counts as reached ([`Reached::unreadable`]):
//! the safe side, as an unreadable grant-spend file is.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::grant::SpendCap;
use crate::identity::FolderScope;
use crate::store::{StoreError, locked, read_private_json, write_private_json};

/// The budgets file's format version.
pub const BUDGET_VERSION: u32 = 1;

/// The share of a budget at which the person is warned, in percent.
pub const WARN_PERCENT: u64 = 80;

/// `<config dir>/mesh/budgets.json`.
pub fn budgets_path(config_dir: &Path) -> PathBuf {
    config_dir.join("mesh").join("budgets.json")
}

/// What a budget covers.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetScope {
    /// Everything this person's Marvs spend.
    Global,
    /// Every chat working in the folder or below it.
    Folder(FolderScope),
    /// One chat (a desktop chat's id).
    Chat(String),
}

impl BudgetScope {
    /// The level's word for the person ("today's budget for acme-api").
    pub fn label(&self) -> String {
        match self {
            BudgetScope::Global => "all of Marv".into(),
            BudgetScope::Folder(f) => {
                f.path().file_name().map_or_else(|| f.path().display().to_string(), |n| n.to_string_lossy().into())
            }
            BudgetScope::Chat(_) => "this chat".into(),
        }
    }
}

/// Who set a budget: always the person. There is no variant for a model, a mesh message, a project or a flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetBy {
    /// The person: the desktop's Usage view, or `marv kernel budget` after its y/N.
    Person,
}

/// One budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    pub scope: BudgetScope,
    pub cap: SpendCap,
    pub by: BudgetBy,
    pub set_at_ms: u64,
}

/// The budgets as one version of the file had them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BudgetSnapshot {
    pub generation: u64,
    pub budgets: Vec<Budget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BudgetFile {
    version: u32,
    generation: u64,
    budgets: Vec<Budget>,
}

impl Default for BudgetFile {
    fn default() -> Self {
        Self { version: BUDGET_VERSION, generation: 0, budgets: Vec::new() }
    }
}

/// What a level spent today, as the meters count it: dollars of priced requests, tokens of unpriced ones.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Spent {
    pub usd_micros: u64,
    pub tokens: u64,
}

/// A budget at (or past) its limit, or past its warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reached {
    /// `None`: the budgets file couldn't be read.
    pub scope: Option<BudgetScope>,
    pub cap: SpendCap,
    pub spent: Spent,
}

impl Reached {
    /// The budgets file couldn't be read: counts as a limit reached.
    pub fn unreadable() -> Self {
        Self { scope: None, cap: SpendCap { usd_micros: 0, tokens: 0 }, spent: Spent::default() }
    }

    /// What the person reads: "acme-api reached its $10.00 budget for today" (Marv's words, the person's numbers).
    pub fn text(&self) -> String {
        let Some(scope) = &self.scope else {
            return "Your budgets file (~/.marv/mesh/budgets.json) can't be read, so every budget counts as reached".into();
        };
        let what = scope.label();
        if self.spent.usd_micros >= self.cap.usd_micros {
            format!("{what} reached its {} budget for today", dollars(self.cap.usd_micros))
        } else {
            format!("{what} reached its {}-token budget for today", self.cap.tokens)
        }
    }

    /// The warning's words: "acme-api has used 82% of its $10.00 budget for today".
    pub fn warning_text(&self) -> String {
        let what = self.scope.as_ref().map_or_else(|| "Marv".to_string(), BudgetScope::label);
        format!("{what} has used {}% of its {} budget for today", percent(&self.spent, &self.cap), dollars(self.cap.usd_micros))
    }
}

/// The largest budget, in dollars a day.
pub const MAX_BUDGET_USD: f64 = 100_000.0;

/// A budget of `usd` dollars a day, as the desktop and `marv kernel budget` both set it: more than $0 and at most
/// [`MAX_BUDGET_USD`]. Dollars only: an unpriced core's tokens are metered but meet no limit (the token cap is open).
pub fn dollar_cap(usd: f64) -> Option<SpendCap> {
    (usd.is_finite() && usd > 0.0 && usd <= MAX_BUDGET_USD)
        .then(|| SpendCap { usd_micros: (usd * 1_000_000.0).round() as u64, tokens: u64::MAX })
}

/// `$10.00`.
pub fn dollars(micros: u64) -> String {
    format!("${}.{:02}", micros / 1_000_000, (micros % 1_000_000) / 10_000)
}

/// The larger share of the cap spent, in whole percent.
pub fn percent(spent: &Spent, cap: &SpendCap) -> u64 {
    let share = |s: u64, c: u64| s.saturating_mul(100).checked_div(c).unwrap_or(100);
    share(spent.usd_micros, cap.usd_micros).max(share(spent.tokens, cap.tokens))
}

/// What [`check`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BudgetCheck {
    /// Budgets at their limit, outermost first: the request may not run without the person.
    pub limits: Vec<Reached>,
    /// Budgets past [`WARN_PERCENT`] and not at the limit.
    pub warnings: Vec<Reached>,
}

/// The budgets over `chat` (working in `folder`), outermost first: global, the folders it is in (widest first), the
/// chat's own.
pub fn covering<'a>(snapshot: &'a BudgetSnapshot, chat: &str, folder: &Path) -> Vec<&'a Budget> {
    let mut out: Vec<&Budget> = snapshot.budgets.iter().filter(|b| b.scope == BudgetScope::Global).collect();
    let mut folders: Vec<&Budget> =
        snapshot.budgets.iter().filter(|b| matches!(&b.scope, BudgetScope::Folder(f) if f.contains(folder))).collect();
    folders.sort_by_key(|b| match &b.scope {
        BudgetScope::Folder(f) => f.path().components().count(),
        _ => 0,
    });
    out.extend(folders);
    out.extend(snapshot.budgets.iter().filter(|b| matches!(&b.scope, BudgetScope::Chat(c) if c == chat)));
    out
}

/// Whether a request for `chat` in `folder` may run: every budget over it that is set, against what its level spent
/// today (`spent`). Pure.
pub fn check(snapshot: &BudgetSnapshot, chat: &str, folder: &Path, spent: impl Fn(&BudgetScope) -> Spent) -> BudgetCheck {
    let mut out = BudgetCheck::default();
    for budget in covering(snapshot, chat, folder) {
        let s = spent(&budget.scope);
        let reached = Reached { scope: Some(budget.scope.clone()), cap: budget.cap, spent: s };
        if s.usd_micros >= budget.cap.usd_micros || s.tokens >= budget.cap.tokens {
            out.limits.push(reached);
        } else if percent(&s, &budget.cap) >= WARN_PERCENT {
            out.warnings.push(reached);
        }
    }
    out
}

/// The person's budgets file. Same rules as the rules' and grants' files ([`crate::store`]).
#[derive(Debug, Clone)]
pub struct BudgetStore {
    path: PathBuf,
    home: PathBuf,
}

impl BudgetStore {
    /// The store in `config_dir` (absolute: the user's config folder).
    pub fn new(config_dir: &Path, home: &Path) -> Result<Self, StoreError> {
        if !config_dir.is_absolute() {
            return Err(StoreError::NotAbsolute(config_dir.display().to_string()));
        }
        Ok(Self { path: budgets_path(config_dir), home: home.to_path_buf() })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn check_file(&self, file: &BudgetFile) -> Result<(), StoreError> {
        let shown = self.path.display().to_string();
        let mut scopes = std::collections::HashSet::new();
        for budget in &file.budgets {
            if let BudgetScope::Folder(f) = &budget.scope {
                f.check_home(&self.home).map_err(|e| StoreError::Parse(shown.clone(), e.to_string()))?;
            }
            if !scopes.insert(budget.scope.clone()) {
                return Err(StoreError::Parse(shown, "a budget is listed twice".into()));
            }
        }
        Ok(())
    }

    /// The budgets now. No file: none. A file that fails any check is an error.
    pub fn load(&self) -> Result<BudgetSnapshot, StoreError> {
        let file = read_private_json::<BudgetFile>(&self.path, BUDGET_VERSION)?.unwrap_or_default();
        self.check_file(&file)?;
        Ok(BudgetSnapshot { generation: file.generation, budgets: file.budgets })
    }

    /// Sets (or, with `None`, removes) the budget for `scope`: the person's own act.
    pub async fn set(&self, scope: BudgetScope, cap: Option<SpendCap>, now_ms: u64) -> Result<BudgetSnapshot, StoreError> {
        locked(&self.path, || async {
            let mut file = read_private_json::<BudgetFile>(&self.path, BUDGET_VERSION)?.unwrap_or_default();
            file.budgets.retain(|b| b.scope != scope);
            if let Some(cap) = cap {
                file.budgets.push(Budget { scope, cap, by: BudgetBy::Person, set_at_ms: now_ms });
            }
            file.generation += 1;
            self.check_file(&file)?;
            write_private_json(&self.path, &file).await?;
            Ok(BudgetSnapshot { generation: file.generation, budgets: file.budgets })
        })
        .await
    }
}

/// [`BudgetStore::load`] as a session consults it: read again only when the file changed. A refused file is
/// [`FileBudgets::snapshot`]'s `Err` (its reason, for the person).
pub struct FileBudgets {
    store: BudgetStore,
    cache: std::sync::Mutex<Cached>,
}

/// Read once, the file's stamp then, and what it held (or why it was refused).
type Cached = (bool, Option<marv_fs::mode::FileStamp>, Result<std::sync::Arc<BudgetSnapshot>, String>);

impl FileBudgets {
    pub fn new(store: BudgetStore) -> Self {
        Self { store, cache: std::sync::Mutex::new((false, None, Ok(std::sync::Arc::default()))) }
    }

    pub fn store(&self) -> &BudgetStore {
        &self.store
    }

    /// The budgets now, or why the file is refused.
    pub fn snapshot(&self) -> Result<std::sync::Arc<BudgetSnapshot>, String> {
        let stamp = std::fs::symlink_metadata(self.store.path()).ok().map(|m| marv_fs::mode::stamp(&m));
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if !cache.0 || cache.1 != stamp {
            let loaded = self.store.load().map(std::sync::Arc::new).map_err(|e| e.to_string());
            *cache = (true, stamp, loaded);
        }
        cache.2.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(path: &str) -> FolderScope {
        serde_json::from_value(serde_json::json!(path)).unwrap()
    }

    fn budget(scope: BudgetScope, usd: u64) -> Budget {
        Budget { scope, cap: SpendCap { usd_micros: usd, tokens: 1_000_000 }, by: BudgetBy::Person, set_at_ms: 1 }
    }

    #[test]
    fn every_budget_over_a_chat_is_checked_outermost_first_and_warns_at_80_percent() {
        let snapshot = BudgetSnapshot {
            generation: 1,
            budgets: vec![
                budget(BudgetScope::Chat("c1".into()), 1_000_000),
                budget(BudgetScope::Folder(folder("/home/me/acme/api")), 5_000_000),
                budget(BudgetScope::Folder(folder("/home/me/acme")), 8_000_000),
                budget(BudgetScope::Global, 10_000_000),
                budget(BudgetScope::Folder(folder("/home/me/other")), 1),
                budget(BudgetScope::Chat("c2".into()), 1),
            ],
        };
        let order: Vec<BudgetScope> =
            covering(&snapshot, "c1", Path::new("/home/me/acme/api/src")).into_iter().map(|b| b.scope.clone()).collect();
        assert_eq!(
            order,
            [
                BudgetScope::Global,
                BudgetScope::Folder(folder("/home/me/acme")),
                BudgetScope::Folder(folder("/home/me/acme/api")),
                BudgetScope::Chat("c1".into())
            ]
        );
        let spent = |usd: u64| move |_: &BudgetScope| Spent { usd_micros: usd, tokens: 0 };
        let quiet = check(&snapshot, "c1", Path::new("/home/me/acme/api"), spent(100_000));
        assert_eq!(quiet, BudgetCheck::default());
        let warned = check(&snapshot, "c1", Path::new("/home/me/acme/api"), spent(850_000));
        assert_eq!((warned.limits.len(), warned.warnings.len()), (0, 1), "the chat's $1 at 85%");
        let at = check(&snapshot, "c1", Path::new("/home/me/acme/api"), spent(8_000_000));
        let limits: Vec<_> = at.limits.iter().map(|r| r.scope.clone().unwrap()).collect();
        assert_eq!(limits.len(), 3, "the acme folder, its api folder and the chat: {limits:?}");
        assert_eq!(at.warnings.len(), 1, "global at 80%");
        assert_eq!(at.limits[0].text(), "acme reached its $8.00 budget for today");
        assert_eq!(at.warnings[0].warning_text(), "all of Marv has used 80% of its $10.00 budget for today");
        assert!(check(&BudgetSnapshot::default(), "c1", Path::new("/x"), spent(u64::MAX)).limits.is_empty(), "none set");
    }

    #[tokio::test]
    async fn budgets_round_trip_privately_and_only_the_person_sets_one() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(home.join("acme")).unwrap();
        let store = BudgetStore::new(&tmp.path().join(".marv"), &home).unwrap();
        assert!(BudgetStore::new(Path::new("rel"), &home).is_err(), "never a relative (project) folder");
        let cap = SpendCap { usd_micros: 10_000_000, tokens: 1_000 };
        store.set(BudgetScope::Global, Some(cap), 5).await.unwrap();
        let acme = FolderScope::new(&home.join("acme"), &home).unwrap();
        let snap = store.set(BudgetScope::Folder(acme.clone()), Some(cap), 6).await.unwrap();
        assert_eq!(snap.budgets.len(), 2);
        assert_eq!(store.load().unwrap().budgets, snap.budgets);
        let snap = store.set(BudgetScope::Global, None, 7).await.unwrap();
        assert_eq!(snap.budgets.iter().map(|b| &b.scope).collect::<Vec<_>>(), [&BudgetScope::Folder(acme)]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(store.path()).unwrap().permissions().mode() & 0o777, 0o600);
        }
        for by in ["model", "mesh", "project", "env", "flag"] {
            assert!(serde_json::from_value::<BudgetBy>(serde_json::json!(by)).is_err(), "{by}");
        }
        // A file that fails a check is refused whole (and counts as reached where it's used).
        std::fs::write(store.path(), "{\"version\":1,\"generation\":1,\"budgets\":[],\"extra\":1}").unwrap();
        assert!(store.load().is_err());
        assert!(FileBudgets::new(store).snapshot().is_err());
    }
}
