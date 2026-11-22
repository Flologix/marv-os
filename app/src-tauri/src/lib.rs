//! Marv desktop. The user picks a root folder once; the window shows its tree, and a chat runs Marv on any folder
//! in it (Marv reads and changes what's below that folder, and asks before anything else). Chats are driven by
//! `marv_web::host`, the same code marv-web runs for a browser: the page sends the same messages and gets the same
//! events, through Tauri commands and a channel instead of a WebSocket.

mod agent_detect;
mod agent_usage;
mod applog;
mod apps;
mod background;
mod chats;
mod checkpoints;
mod delegates;
pub mod dmabuf;
mod dock;
mod handoff;
mod hang_report;
mod instance;
pub mod login_env;
mod marv_config;
mod notify;
mod plan;
mod store;
mod teams;
mod terminal_events;
mod terminals;
mod tree;
mod triage;
mod watchdog;
mod worktrees;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use marv_web::host::ChatSpec;
use marv_web::protocol::{ClientMessage, is_model_id};
use serde::Serialize;
use serde_json::Value;
use store::{Chat, Store};
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State};
use tokio::sync::mpsc;

struct Desktop {
    store: Store,
    /// The chats kept while the app runs (#155, runtime-kernel 1): on the mesh, their sessions loaded or asleep.
    live: std::sync::Arc<chats::Chats>,
    /// The folder named on the command line (`marv-desktop <dir>`), until the page takes it.
    launch: Mutex<Option<String>>,
    /// MCP servers looked up for adding, by name: what the person saw is exactly what gets written.
    proposals: Mutex<HashMap<String, Value>>,
    /// Mesh rules made "until Marv quits": revoked when the app exits (and expiring in a day, should it not).
    session_rules: Mutex<Vec<marv_kernel::PolicyId>>,
    /// The kernel's event log (runtime-kernel 3, `~/.marv/events/`): rules made, revoked and found expired here, and
    /// the chats' events. None when the config folder can't hold one (it isn't absolute).
    events: Option<std::sync::Arc<marv_session::kernel_events::EventWriter>>,
    /// The rules whose expiry this app has logged.
    expiries: Mutex<marv_kernel::Expiries>,
    /// #159: the ChatGPT sign-in waiting for the browser, if one is (one at a time; the page can cancel it).
    sign_in: Mutex<Option<tokio_util::sync::CancellationToken>>,
    /// Tells the person when a chat they aren't looking at needs them or is done.
    notifier: notify::Notifier,
    /// Runtime-kernel §20.3: when the person left, for "while you were away" (computed in `marv_session::live`).
    away: marv_session::live::AwayTracker,
    /// The person's service grants (`~/.marv/mesh/grants.json`) and what each spent today: a chat granted here
    /// keeps working while they're away, within yolo's set. None when the config folder can't hold them.
    grants: Option<Grants>,
    /// Grants made "until Marv quits": revoked when the app exits (and expiring in a day, should it not).
    session_grants: Mutex<Vec<marv_kernel::GrantId>>,
    /// Notices for chats, told in the chat as it next loads (runtime-kernel §14.1: its worktree was gone).
    notices: Mutex<std::collections::HashMap<String, String>>,
    /// The provider in `~/.marv/config.json` when each Marv chat's terminal started with no model of its own: Marv
    /// reads its config only at start, so one set up later (OpenRouter after Ollama) didn't reach it, and nothing said so.
    started_on: Mutex<std::collections::HashMap<String, Option<String>>>,
    /// The Team view's reports (`teams::TeamReports`): each chat's latest report-back per stamped sender, this run.
    reports: teams::TeamReports,
    /// The grants whose expiry this app has logged.
    grant_expiries: Mutex<std::collections::BTreeSet<marv_kernel::GrantId>>,
    /// Runtime-kernel §16: the meters every chat's turns are charged to, and the person's budgets
    /// (`~/.marv/mesh/budgets.json`). None when the config folder can't hold them.
    meters: Option<std::sync::Arc<marv_session::allocator::MeterBook>>,
    /// Kernel #41: the window's watchdog, this window's instance record, and what this launch did at start.
    watch: Watch,
}

/// Kernel #41: the page's and main thread's heartbeat (`watchdog`), this window's record for later launches
/// (`instance`), and the hang reports in the app's data folder (`hang_report`).
struct Watch {
    data: PathBuf,
    started: std::time::Instant,
    /// None: the watchdog is off (`MARV_WATCHDOG=off`, or a debug build).
    heartbeat: Mutex<Option<watchdog::Heartbeat>>,
    instance: Option<instance::Own>,
    /// For the page once: this launch closed an earlier window that had stopped responding.
    notice: Mutex<Option<String>>,
    /// The exit's cleanup (P3-3): 0 not started, 1 running, 2 done. It runs once (the normal exit, or the fallback
    /// when the main thread is stuck), and the fallback never cuts a cleanup that's running short.
    exit: std::sync::atomic::AtomicU8,
    /// The app is closing (SIGTERM): the watchdog stops acting.
    closing: std::sync::atomic::AtomicBool,
}

impl Watch {
    /// The watchdog's clock: monotonic milliseconds since start (not running while the machine sleeps).
    fn now(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// `f` on the heartbeat, if the watchdog runs (never held across an await).
    fn heartbeat(&self, f: impl FnOnce(&mut watchdog::Heartbeat)) {
        if let Some(hb) = self.heartbeat.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            f(hb);
        }
    }
}

/// Kernel #41: what this launch runs, from the environment and the build (`watchdog::Timings::from_env`). Without a
/// watchdog there's no takeover of other windows either, and this window's record says so (never taken over).
struct WatchPlan {
    timings: Option<watchdog::Timings>,
}

impl WatchPlan {
    fn from_env(get: impl Fn(&str) -> Option<String>, debug: bool) -> Self {
        Self { timings: watchdog::Timings::from_env(get, debug) }
    }
}

/// Kernel #41, at start: with the watchdog on, earlier windows that stopped responding are closed first (their mesh
/// names freed, a report each); then this window's own record, saying whether its watchdog runs. The notice for the
/// person, if a window was closed, and the record.
fn start_instances(
    data: &Path,
    plan: &WatchPlan,
    chats: usize,
    system: &dyn instance::System,
    skip: &[u32],
) -> (Option<String>, Option<instance::Own>) {
    let dir = data.join("instances");
    let notice =
        if plan.timings.is_some() { take_over_hung_windows(data, chats, system, std::process::id(), skip) } else { None };
    (notice, instance::Own::register(&dir, VERSION, plan.timings.is_some()).ok())
}

/// The grants file, as every chat's grant reads it, and the day's spend.
struct Grants {
    file: std::sync::Arc<marv_kernel::FileGrants>,
    spend: std::sync::Arc<marv_session::grants::SpendBook>,
}

impl Desktop {
    fn event_log(&self) -> Option<&dyn marv_kernel::EventLog> {
        self.events.as_deref().map(|l| l as &dyn marv_kernel::EventLog)
    }
}

/// The CLI's own folder (~/.marv): memory, MCP servers, logins and saved sessions shared with `marv`.
fn marv_dir() -> PathBuf {
    let env: marv_session::config::Env = std::env::vars().collect();
    marv_session::config::default_config_dir(&env, &marv_session::paths::home_dir())
}

impl Desktop {
    fn root(&self) -> Result<PathBuf, String> {
        self.store.settings().root.ok_or_else(|| "Pick your Marv folder first.".to_string())
    }

    /// The chats, with any saved relative to the root (before folders were full paths) converted and saved back.
    fn chats(&self) -> Vec<Chat> {
        let mut chats = self.store.chats();
        if let Ok(root) = self.root()
            && chats.iter().any(|c| !c.folder.starts_with('/'))
        {
            for chat in chats.iter_mut().filter(|c| !c.folder.starts_with('/')) {
                chat.folder = root.join(&chat.folder).to_string_lossy().trim_end_matches('/').to_string();
            }
            let _ = self.store.modify_chats(|saved| {
                for chat in saved.iter_mut().filter(|c| !c.folder.starts_with('/')) {
                    chat.folder = root.join(&chat.folder).to_string_lossy().trim_end_matches('/').to_string();
                }
            });
        }
        chats
    }

    /// `rel` (from the page, relative to the top folder) as a full path: a chat's folder, in its one standard form (on
    /// Windows each folder's on-disk casing, so a chat made on `Proj` and one on `proj` share their folder's lazygit).
    fn full(&self, rel: &str) -> Result<String, String> {
        let path = marv_session::paths::normalize_folder(&tree::resolve(&self.root()?, rel)?);
        Ok(path.to_string_lossy().trim_end_matches('/').to_string())
    }

    /// The pasted key, else `OPENROUTER_API_KEY`, else the one `marv`'s setup saved.
    fn api_key(&self) -> Option<String> {
        saved_key(self.store.settings().api_key, "OPENROUTER_API_KEY", "apiKey")
    }

    /// #159: the Z.ai Coding Plan key: pasted here, else `ZAI_API_KEY`, else `marv`'s own config.
    fn zai_key(&self) -> Option<String> {
        saved_key(self.store.settings().zai_api_key, "ZAI_API_KEY", "zaiApiKey")
    }

    /// #159: the Kimi Code plan key: pasted here, else `KIMI_API_KEY`, else `marv`'s own config.
    fn kimi_key(&self) -> Option<String> {
        saved_key(self.store.settings().kimi_api_key, "KIMI_API_KEY", "kimiApiKey")
    }

    /// #159: the ChatGPT sign-in, shared with `marv` (~/.marv/chatgpt.json).
    fn chatgpt(&self) -> marv_provider::chatgpt::AuthStore {
        marv_provider::chatgpt::AuthStore::new(&marv_dir())
    }

    /// #159: whether a plan may be used here: its switch is on (`marv_provider::offered`), or it's already the chosen
    /// provider (as the CLI keeps a config that names it).
    fn plan_allowed(&self, plan: &str) -> bool {
        let offered = match plan {
            "zai" => marv_provider::offered::ZAI_CODING_PLAN,
            "kimi" => true,
            "chatgpt" => marv_provider::offered::CHATGPT_SIGN_IN,
            _ => false,
        };
        offered || self.store.settings().provider.as_deref() == Some(plan)
    }

    /// Whether the chosen provider has what it needs (a key, a sign-in; Ollama nothing).
    fn ready(&self) -> bool {
        match self.store.settings().provider.as_deref() {
            Some("ollama") => true,
            Some("zai") => self.zai_key().is_some(),
            Some("kimi") => self.kimi_key().is_some(),
            Some("chatgpt") => self.chatgpt().signed_in(),
            _ => self.api_key().is_some(),
        }
    }
}

/// A key pasted here, else the environment's, else the one `marv`'s setup saved under `field`.
fn saved_key(pasted: Option<String>, env: &str, field: &str) -> Option<String> {
    if let Some(key) = pasted.filter(|k| !k.trim().is_empty()) {
        return Some(key);
    }
    if let Ok(key) = std::env::var(env)
        && !key.trim().is_empty()
    {
        return Some(key.trim().to_string());
    }
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(marv_dir().join("config.json")).ok()?).ok()?;
    saved.get(field)?.as_str().map(str::to_string)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    root: Option<String>,
    root_name: Option<String>,
    has_key: bool,
    /// "openrouter", "ollama", "zai", "kimi" or "chatgpt" (#159), once chosen.
    provider: Option<String>,
    /// #159: the chosen provider has what it needs.
    ready: bool,
    /// #159: onboarding offers the Z.ai Coding Plan / the Kimi Code plan / Sign in with ChatGPT
    /// (`marv_provider::offered`).
    offer_zai: bool,
    offer_kimi: bool,
    offer_chatgpt: bool,
    has_zai_key: bool,
    has_kimi_key: bool,
    /// #159: who is signed in with ChatGPT, if anyone.
    chatgpt_account: Option<String>,
    spaces: Vec<store::Space>,
    active: Option<String>,
    /// The home folder (real path), where Home chats run.
    home: String,
    /// Signed in to a Marv account here (plan.json), free or paid.
    signed_in: bool,
    /// The first run's "Sign in to Marv" step was answered.
    account_asked: bool,
    /// Marv's outer sandbox can run here (Linux with bubblewrap; not on Windows yet): the page says only what is true.
    sandbox: bool,
}

fn info(desktop: &Desktop) -> AppInfo {
    let settings = desktop.store.settings();
    let root = settings.root.clone();
    AppInfo {
        root_name: root.as_ref().and_then(|r| r.file_name()).map(|n| n.to_string_lossy().into_owned()),
        root: root.map(|r| r.to_string_lossy().into_owned()),
        has_key: desktop.api_key().is_some(),
        provider: settings.provider.clone(),
        ready: desktop.ready(),
        offer_zai: marv_provider::offered::ZAI_CODING_PLAN,
        offer_kimi: true,
        offer_chatgpt: marv_provider::offered::CHATGPT_SIGN_IN,
        has_zai_key: desktop.zai_key().is_some(),
        has_kimi_key: desktop.kimi_key().is_some(),
        chatgpt_account: desktop
            .chatgpt()
            .signed_in()
            .then(|| desktop.chatgpt().load().and_then(|a| a.email).unwrap_or_else(|| "your ChatGPT account".into())),
        active: settings.active_space().map(|s| s.id.clone()),
        account_asked: settings.account_asked,
        sandbox: marv_web::harness_sandbox::available(),
        spaces: settings.spaces,
        home: {
            let home = marv_session::paths::home_dir();
            marv_session::paths::real_folder(&home).unwrap_or(home).to_string_lossy().into_owned()
        },
        signed_in: plan::read_plan(&desktop.plan_ctx().dir).is_some(),
    }
}

#[tauri::command]
fn app_info(desktop: State<'_, Desktop>) -> AppInfo {
    info(&desktop)
}

/// A root folder: any folder inside the home folder. Not the home folder or above it, since Marv changes files
/// below its folder without asking (dotfiles included, there).
fn check_root(path: &str) -> Result<PathBuf, String> {
    let root = marv_session::paths::real_folder(path.as_ref()).map_err(|e| format!("Couldn't open {path}: {e}"))?;
    if !root.is_dir() {
        return Err(format!("{path} isn't a folder."));
    }
    if marv_session::paths::home_or_above(&root, &marv_session::paths::home_dir()) {
        return Err(format!(
            "{} is your home folder or above it: Marv changes files in its folder without asking, so pick a folder inside your home folder (like ~/Projects).",
            root.display()
        ));
    }
    Ok(root)
}

/// The Home chats' folder name, as the page sends it: the home folder itself, not a folder of the space.
const HOME_FOLDER: &str = "~";

/// A chat's folder: a root folder ([`check_root`]), or the home folder itself for a Home chat. There Marv runs as the
/// TUI does in `~`: the sandbox shows the home read-only with Marv's config and credentials hidden, every file change
/// asks (once, no "always"), and yolo is refused (marv-web's host holds it off; `no_yolo_reason`). Never above it.
fn check_chat_root(path: &str) -> Result<PathBuf, String> {
    let home = marv_session::paths::home_dir();
    match (marv_session::paths::real_folder(path.as_ref()), marv_session::paths::real_folder(&home)) {
        (Ok(dir), Ok(home)) if dir == home => Ok(dir),
        _ => check_root(path),
    }
}

/// Where chat `chat`'s session works: its own worktree when it has one (runtime-kernel §14.1), else its folder.
/// `Gone(branch)`: its worktree's folder is gone (removed by hand, the disk cleaned), so it works in its folder.
fn work_root(chat: &Chat) -> Result<(PathBuf, Option<String>), String> {
    match &chat.worktree {
        Some(wt) if std::path::Path::new(&wt.dir).is_dir() => {
            Ok((check_root(&wt.dir).map_err(|e| format!("its worktree ({}): {e}", wt.branch))?, None))
        }
        Some(wt) => Ok((check_chat_root(&chat.folder)?, Some(wt.branch.clone()))),
        None => Ok((check_chat_root(&chat.folder)?, None)),
    }
}

/// Runtime-kernel §14.1: chat `id` no longer works in its worktree (the person moved it back to its folder, the chat
/// was deleted, or the worktree is gone; never because its grant ended): the record forgets it, and the conversation
/// it had there (saved under the worktree, so it can't be resumed from the folder) with it; git forgets worktrees
/// whose folders are gone (`git worktree prune`). The worktree and its branch, if still there, are the person's to
/// merge or remove: nothing here deletes them (deleting a chat does, when nothing is lost: `delete_chat`).
fn end_worktree(desktop: &Desktop, id: &str) {
    let Some(chat) = desktop.chats().into_iter().find(|c| c.id == id) else { return };
    if chat.worktree.is_none() {
        return;
    }
    let _ = desktop.store.update_chat(id, |c| (c.worktree, c.session_id) = (None, None));
    git_quiet(std::path::Path::new(&chat.folder), &["worktree", "prune"]);
}

/// The person's choice in the chat's settings (runtime-kernel §14.1): chat `id` works in its folder again, in a new
/// conversation (its worktree conversation was saved there). Its session stops first if it runs. The worktree and its
/// branch are kept and the answer says so, and whether the branch has work not merged yet: never deleted here.
#[tauri::command]
fn move_chat_to_folder(desktop: State<'_, Desktop>, id: String) -> Result<String, String> {
    let chat = desktop.chats().into_iter().find(|c| c.id == id).ok_or("That chat is gone.")?;
    let wt = chat.worktree.clone().ok_or("This chat already works in its folder.")?;
    desktop.live.stop(&id);
    end_worktree(&desktop, &id);
    let repo = std::path::Path::new(&chat.folder);
    let unmerged = git_out(repo, &["branch", "--no-merged", "HEAD", "--list", &wt.branch]).is_some_and(|o| !o.is_empty());
    let changed = std::path::Path::new(&wt.dir).is_dir()
        && git_out(std::path::Path::new(&wt.dir), &["status", "--porcelain"]).is_some_and(|o| !o.is_empty());
    let mut answer = format!("It works in {} again, in a new conversation. Branch {} is kept", chat.folder, wt.branch);
    answer.push_str(match (unmerged, changed) {
        (_, true) => ", with changes not committed yet in its worktree",
        (true, false) => ", with commits not merged yet",
        (false, false) => "",
    });
    answer.push_str(&format!(
        " ({}): merge it, or remove it with git worktree remove and git branch -D.",
        if std::path::Path::new(&wt.dir).is_dir() { wt.dir.as_str() } else { "its folder is gone" }
    ));
    Ok(answer)
}

/// What worktree chat `id`'s branch has beside its folder's (None: it works in its folder).
#[tauri::command]
fn worktree_status(desktop: State<'_, Desktop>, id: String) -> Option<worktrees::WorktreeStatus> {
    let chat = desktop.chats().into_iter().find(|c| c.id == id)?;
    // A delegate shares its parent's worktree: its parent's Merge and Discard are the only ones.
    if chat.delegated_by.is_some() {
        return None;
    }
    let wt = chat.worktree.as_ref()?;
    Some(worktrees::status(Path::new(&chat.folder), wt))
}

/// A delegate works in its parent's worktree (`start_delegate`): merging or discarding it there would take the
/// worktree from under the parent, which still runs in it.
fn refuse_delegate_worktree(desktop: &Desktop, chat: &Chat) -> Result<(), String> {
    let Some(parent) = &chat.delegated_by else { return Ok(()) };
    let name = desktop.chats().into_iter().find(|c| &c.id == parent).map_or_else(|| "its parent".to_string(), |c| c.title);
    Err(format!("This chat works in {name}'s worktree: merge or discard it from there."))
}

/// Chat `parent`'s delegates stop: they run in its worktree, which is about to be merged or removed.
fn end_delegates(desktop: &Desktop, terminals: &terminals::Terminals, parent: &str) {
    for delegate in desktop.chats().iter().filter(|c| c.delegated_by.as_deref() == Some(parent)) {
        terminals.close_key(&format!("chat:{}", delegate.id));
        desktop.live.stop(&delegate.id);
    }
}

/// The person's **Merge** for worktree chat `id`: its agent stops, its work goes into the folder's branch, and its
/// worktree, branch and chat go (the task is done). Refused or conflicting: nothing changes but its agent stopped.
#[tauri::command]
async fn worktree_merge(
    desktop: State<'_, Desktop>,
    terminals: State<'_, terminals::Terminals>,
    id: String,
) -> Result<String, String> {
    let chat = desktop.chats().into_iter().find(|c| c.id == id).ok_or("That chat is gone.")?;
    refuse_delegate_worktree(&desktop, &chat)?;
    let wt = chat.worktree.clone().ok_or("This chat works in its folder: there's no branch to merge.")?;
    // Its agent stops first: nothing writes in the worktree while it's committed and merged.
    end_delegates(&desktop, &terminals, &id);
    terminals.close_key(&format!("chat:{id}"));
    desktop.live.stop(&id);
    let folder = PathBuf::from(&chat.folder);
    let title = chat.title.clone();
    let said = tauri::async_runtime::spawn_blocking(move || worktrees::merge(&folder, &wt, &title))
        .await
        .map_err(|e| e.to_string())??;
    remove_chat(&desktop, &terminals, &id).await?;
    Ok(said)
}

/// The person's **Discard** for worktree chat `id`: its agent stops and its worktree, branch (work and all) and chat go.
#[tauri::command]
async fn worktree_discard(
    desktop: State<'_, Desktop>,
    terminals: State<'_, terminals::Terminals>,
    id: String,
) -> Result<String, String> {
    let chat = desktop.chats().into_iter().find(|c| c.id == id).ok_or("That chat is gone.")?;
    refuse_delegate_worktree(&desktop, &chat)?;
    let wt = chat.worktree.clone().ok_or("This chat works in its folder: there's no branch to discard.")?;
    end_delegates(&desktop, &terminals, &id);
    terminals.close_key(&format!("chat:{id}"));
    desktop.live.stop(&id);
    let folder = PathBuf::from(&chat.folder);
    let branch = wt.branch.clone();
    tauri::async_runtime::spawn_blocking(move || worktrees::remove(&folder, &wt)).await.map_err(|e| e.to_string())??;
    remove_chat(&desktop, &terminals, &id).await?;
    Ok(format!("Discarded {branch}: its worktree and branch are removed."))
}

/// Chat `id`'s own worktree, if it has one (its settings offer to move it back to its folder).
#[tauri::command]
fn chat_worktree(desktop: State<'_, Desktop>, id: String) -> Option<store::ChatWorktree> {
    desktop.chats().into_iter().find(|c| c.id == id).and_then(|c| c.worktree)
}

/// Runs git in `dir` and gives its trimmed output when it succeeds.
fn git_out(dir: &std::path::Path, args: &[&str]) -> Option<String> {
    let out = marv_core::process::command("git").args(args).current_dir(dir).stdin(std::process::Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Runs git in `dir`, its output dropped (best effort).
fn git_quiet(dir: &std::path::Path, args: &[&str]) -> bool {
    marv_core::process::command("git")
        .args(args)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Takes back a worktree made for a grant that didn't happen (runtime-kernel §14.1): its folder, then its branch.
fn remove_worktree(repo: &std::path::Path, wt: &store::ChatWorktree) {
    let top = if wt.top.is_empty() { &wt.dir } else { &wt.top };
    git_quiet(repo, &["worktree", "remove", "--force", top]);
    git_quiet(repo, &["branch", "-D", &wt.branch]);
    git_quiet(repo, &["worktree", "prune"]);
}

fn save_root(desktop: &Desktop, root: PathBuf) -> Result<AppInfo, String> {
    desktop.store.modify_settings(|settings| settings.set_root(root))?;
    Ok(info(desktop))
}

/// A new space on `path` (named `name`), and switch to it.
#[tauri::command]
fn create_space(desktop: State<'_, Desktop>, name: String, path: String) -> Result<AppInfo, String> {
    let root = check_root(&path)?;
    let name: String = name.trim().chars().take(40).collect();
    let name =
        if name.is_empty() { root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() } else { name };
    let id = uuid::Uuid::new_v4().to_string();
    desktop.store.modify_settings(|settings| {
        settings.spaces.push(store::Space { id: id.clone(), name, root: root.clone(), apps_off: Vec::new() });
        settings.active = Some(id);
        settings.root = Some(root);
    })?;
    Ok(info(&desktop))
}

#[tauri::command]
fn switch_space(desktop: State<'_, Desktop>, id: String) -> Result<AppInfo, String> {
    let mut settings = desktop.store.settings();
    let root = settings.spaces.iter().find(|s| s.id == id).ok_or("That space is gone.")?.root.clone();
    settings.active = Some(id);
    settings.root = Some(root);
    desktop.store.save_settings(&settings)?;
    Ok(info(&desktop))
}

#[tauri::command]
fn rename_space(desktop: State<'_, Desktop>, id: String, name: String) -> Result<AppInfo, String> {
    let mut settings = desktop.store.settings();
    let space = settings.spaces.iter_mut().find(|s| s.id == id).ok_or("That space is gone.")?;
    let name: String = name.trim().chars().take(40).collect();
    if !name.is_empty() {
        space.name = name;
    }
    desktop.store.save_settings(&settings)?;
    Ok(info(&desktop))
}

/// Removes a space (its folder, files and chats stay); not the last one.
#[tauri::command]
fn remove_space(desktop: State<'_, Desktop>, id: String) -> Result<AppInfo, String> {
    let mut settings = desktop.store.settings();
    if settings.spaces.len() <= 1 {
        return Err("That's your only space.".into());
    }
    settings.spaces.retain(|s| s.id != id);
    if settings.active.as_ref() == Some(&id) {
        settings.active = settings.spaces.first().map(|s| s.id.clone());
    }
    desktop.store.save_settings(&settings)?;
    Ok(info(&desktop))
}

/// The apps (MCP servers) switched off in the active space.
#[tauri::command]
fn set_apps_off(desktop: State<'_, Desktop>, off: Vec<String>) -> Result<AppInfo, String> {
    let mut settings = desktop.store.settings();
    settings.active_space_mut().ok_or("Pick a folder first.")?.apps_off = off;
    desktop.store.save_settings(&settings)?;
    Ok(info(&desktop))
}

/// "openrouter" or "ollama", or (#159, when offered) "zai", "kimi" or "chatgpt": how Marv thinks, chosen at onboarding.
#[tauri::command]
fn set_provider(desktop: State<'_, Desktop>, provider: String) -> Result<AppInfo, String> {
    let known = match provider.as_str() {
        "openrouter" | "ollama" => true,
        "zai" => marv_provider::offered::ZAI_CODING_PLAN,
        "kimi" => true,
        "chatgpt" => marv_provider::offered::CHATGPT_SIGN_IN,
        _ => false,
    };
    if !known {
        return Err(format!("Unknown provider {provider}."));
    }
    desktop.store.modify_settings(|settings| settings.provider = Some(provider))?;
    Ok(info(&desktop))
}

#[derive(Serialize)]
struct LocalModel {
    /// `ollama/<name>`, as a chat's model.
    id: String,
    /// Ollama says it can call tools (Marv needs them).
    tools: bool,
}

/// The models pulled into the local Ollama; an error when it isn't running.
#[tauri::command]
async fn local_models() -> Result<Vec<LocalModel>, String> {
    let models = marv_provider::models::list_models(marv_provider::ProviderKind::Ollama, "http://localhost:11434", None)
        .await
        .map_err(|_| {
            if cfg!(target_os = "linux") {
                "Ollama isn't running (sudo systemctl enable --now ollama).".to_string()
            } else {
                "Ollama isn't running: start the Ollama app.".to_string()
            }
        })?;
    Ok(models.into_iter().map(|m| LocalModel { id: format!("ollama/{}", m.id), tools: m.tools.unwrap_or(false) }).collect())
}

/// `ollama pull <name>`, its progress lines to `on_progress`.
#[tauri::command]
async fn pull_model(name: String, on_progress: Channel<String>) -> Result<(), String> {
    use tokio::io::AsyncReadExt;
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || "-_.:/".contains(c)) {
        return Err(format!("\"{name}\" isn't a model name."));
    }
    let mut child = tokio::process::Command::from(marv_core::process::command("ollama"))
        .args(["pull", &name])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|_| "Ollama isn't installed.".to_string())?;
    // Progress comes on stderr, redrawn with \r.
    let mut err = child.stderr.take().ok_or("no output")?;
    let mut buf = [0u8; 4096];
    let mut line = String::new();
    while let Ok(n) = err.read(&mut buf).await {
        if n == 0 {
            break;
        }
        for c in String::from_utf8_lossy(&buf[..n]).chars() {
            if c == '\r' || c == '\n' {
                let text = marv_core::printable(line.trim());
                if !text.is_empty() {
                    let _ = on_progress.send(text);
                }
                line.clear();
            } else {
                line.push(c);
            }
        }
    }
    let status = child.wait().await.map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("ollama pull {name} failed.")) }
}

#[tauri::command]
fn set_root(desktop: State<'_, Desktop>, path: String) -> Result<AppInfo, String> {
    save_root(&desktop, check_root(&path)?)
}

/// Going into a folder (double-click in the tree): it becomes the top folder.
#[tauri::command]
fn enter_folder(desktop: State<'_, Desktop>, rel: String) -> Result<AppInfo, String> {
    let dir = tree::resolve(&desktop.root()?, &rel)?;
    save_root(&desktop, check_root(&dir.to_string_lossy())?)
}

/// The Marv folder's parent becomes the Marv folder (the tree's ↑), where that's allowed.
#[tauri::command]
fn root_up(desktop: State<'_, Desktop>) -> Result<AppInfo, String> {
    let root = desktop.root()?;
    let parent = root.parent().ok_or("There's nothing above this folder.")?;
    save_root(&desktop, check_root(&parent.to_string_lossy())?)
}

#[derive(Serialize)]
struct Launch {
    info: AppInfo,
    /// The folder to select, relative to the root.
    folder: String,
}

/// `marv-desktop <dir>`, once: a folder inside the Marv folder is selected; any other becomes the Marv folder.
///
/// The page asks for it first thing, so with `MARV_DESKTOP_SMOKE_TEST=1` (CI's launch check of a built bundle) this is
/// where the app is known to be up: the window opened, the bundled page ran and reached the backend. It prints
/// `marv-desktop: ready` (or why the launch folder was refused) and quits.
#[tauri::command]
fn take_launch(app: tauri::AppHandle, desktop: State<'_, Desktop>) -> Result<Option<Launch>, String> {
    let launch = launch_folder(&desktop);
    if std::env::var_os("MARV_DESKTOP_SMOKE_TEST").is_some_and(|v| v == "1") {
        match &launch {
            Ok(Some(l)) => println!("marv-desktop: ready, root {}", l.info.root.as_deref().unwrap_or("(none)")),
            Ok(None) => println!("marv-desktop: ready"),
            Err(e) => println!("marv-desktop: launch folder refused: {e}"),
        }
        app.exit(if launch.is_ok() { 0 } else { 1 });
    }
    launch
}

fn launch_folder(desktop: &Desktop) -> Result<Option<Launch>, String> {
    let Some(path) = desktop.launch.lock().unwrap_or_else(|e| e.into_inner()).take() else { return Ok(None) };
    let dir = marv_session::paths::real_folder(path.as_ref()).map_err(|e| format!("Couldn't open {path}: {e}"))?;
    if !dir.is_dir() {
        return Err(format!("{path} isn't a folder."));
    }
    if let Some(root) = desktop.store.settings().root
        && let Ok(rel) = dir.strip_prefix(&root)
    {
        return Ok(Some(Launch { info: info(desktop), folder: rel.to_string_lossy().into_owned() }));
    }
    let info = save_root(desktop, check_root(&path)?)?;
    Ok(Some(Launch { info, folder: String::new() }))
}

#[tauri::command]
fn set_api_key(desktop: State<'_, Desktop>, key: String) -> Result<AppInfo, String> {
    desktop.store.modify_settings(|settings| settings.api_key = Some(key.trim().to_string()).filter(|k| !k.is_empty()))?;
    Ok(info(&desktop))
}

/// #159: the Z.ai Coding Plan key, saved here (0600).
#[tauri::command]
fn set_zai_key(desktop: State<'_, Desktop>, key: String) -> Result<AppInfo, String> {
    desktop.store.modify_settings(|settings| settings.zai_api_key = Some(key.trim().to_string()).filter(|k| !k.is_empty()))?;
    Ok(info(&desktop))
}

/// #159: the Kimi Code plan key, saved here (0600).
#[tauri::command]
fn set_kimi_key(desktop: State<'_, Desktop>, key: String) -> Result<AppInfo, String> {
    desktop.store.modify_settings(|settings| settings.kimi_api_key = Some(key.trim().to_string()).filter(|k| !k.is_empty()))?;
    Ok(info(&desktop))
}

/// #159: Sign in with ChatGPT: the browser opens OpenAI's page, a 127.0.0.1 listener takes the one callback. Shared
/// with `marv` (~/.marv/chatgpt.json).
#[tauri::command]
async fn chatgpt_sign_in(desktop: State<'_, Desktop>, another: Option<bool>) -> Result<AppInfo, String> {
    if !marv_provider::offered::CHATGPT_SIGN_IN {
        return Err("Sign in with ChatGPT isn't available in this build.".into());
    }
    let cancel = tokio_util::sync::CancellationToken::new();
    {
        let mut pending = desktop.sign_in.lock().unwrap_or_else(|e| e.into_inner());
        if pending.as_ref().is_some_and(|c| !c.is_cancelled()) {
            return Err("A sign-in is already waiting for the browser.".into());
        }
        *pending = Some(cancel.clone());
    }
    // `another`: register another ChatGPT account; else the one in use (or a first registration).
    let target = if another == Some(true) {
        marv_provider::chatgpt::SignInTarget::New
    } else {
        marv_provider::chatgpt::SignInTarget::Current
    };
    let outcome = async {
        let sign_in = marv_provider::chatgpt::start_sign_in(desktop.chatgpt(), Default::default(), target).await?;
        let url = sign_in.url().parse().map_err(|e| format!("{e}"))?;
        marv_mcp::oauth::open_browser(&url, std::env::consts::OS);
        sign_in.finish(cancel.clone()).await
    }
    .await;
    desktop.sign_in.lock().unwrap_or_else(|e| e.into_inner()).take();
    outcome?;
    Ok(info(&desktop))
}

/// #159: stops the ChatGPT sign-in waiting for the browser.
#[tauri::command]
fn chatgpt_cancel_sign_in(desktop: State<'_, Desktop>) {
    if let Some(cancel) = desktop.sign_in.lock().unwrap_or_else(|e| e.into_inner()).take() {
        cancel.cancel();
    }
}

/// #159: signs out of ChatGPT (revoked at OpenAI when it answers).
#[tauri::command]
async fn chatgpt_sign_out(desktop: State<'_, Desktop>) -> Result<AppInfo, String> {
    marv_provider::chatgpt::sign_out(&desktop.chatgpt(), &Default::default()).await?;
    Ok(info(&desktop))
}

impl Desktop {
    /// The Marv plan's work (`plan`): Marv's folder, the server (`MARV_PLAN_URL`), and the OpenRouter keys that come
    /// before the config file's (pasted here, `OPENROUTER_API_KEY`).
    fn plan_ctx(&self) -> plan::Ctx {
        plan::Ctx {
            dir: marv_dir(),
            base: plan::base_url(std::env::var("MARV_PLAN_URL").ok()),
            keys: plan::Keys { pasted: self.store.settings().api_key, env: std::env::var("OPENROUTER_API_KEY").ok() },
            now: SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()),
            public_key: plan::public_key(),
        }
    }
}

/// The Marv plan: emails a sign-in code (the server never says whether an account exists).
#[tauri::command]
async fn plan_login_start(desktop: State<'_, Desktop>, email: String) -> Result<(), String> {
    plan::login_start(&desktop.plan_ctx(), &email).await
}

/// The Marv plan: the emailed code signs this computer in ("Marv OS on <hostname>"). Its OpenRouter key goes where an
/// OpenRouter key lives, unless the person has their own (then `ownKey`: the page offers the plan's instead).
#[tauri::command]
async fn plan_login_verify(
    desktop: State<'_, Desktop>,
    email: String,
    code: String,
    marketing: Option<bool>,
) -> Result<plan::PlanStatus, String> {
    let device = plan::device_name(&plan::hostname());
    let signed = plan::login_verify(&desktop.plan_ctx(), &email, &code, &device, marketing == Some(true)).await?;
    after_sign_in(&desktop, signed)
}

/// Signed in, by code or browser: the first run's account step is answered, and with no provider chosen yet the plan's
/// key makes OpenRouter ready.
fn after_sign_in(
    desktop: &Desktop,
    (status, installed): (plan::PlanStatus, Option<plan::KeySetup>),
) -> Result<plan::PlanStatus, String> {
    desktop.store.modify_settings(|settings| {
        settings.account_asked = true;
        if matches!(installed, Some(plan::KeySetup::Installed | plan::KeySetup::AlreadyIn)) && settings.provider.is_none() {
            settings.provider = Some("openrouter".into());
        }
    })?;
    Ok(status)
}

/// Marv account: sign in with Google or GitHub in the browser (it comes back to a port on 127.0.0.1).
#[tauri::command]
async fn plan_login_with(
    desktop: State<'_, Desktop>,
    provider: String,
    marketing: Option<bool>,
) -> Result<plan::PlanStatus, String> {
    let device = plan::device_name(&plan::hostname());
    let signed = plan::oauth_login(&desktop.plan_ctx(), &provider, &device, marketing == Some(true), |url| {
        if let Ok(url) = url.parse() {
            marv_mcp::oauth::open_browser(&url, std::env::consts::OS);
        }
    })
    .await?;
    after_sign_in(&desktop, signed)
}

/// Marv account: product emails on or off (the person's own switch in Settings › Account).
#[tauri::command]
async fn plan_set_marketing(desktop: State<'_, Desktop>, on: bool) -> Result<plan::PlanStatus, String> {
    plan::set_marketing(&desktop.plan_ctx(), on).await
}

/// Marv account: stops waiting for the browser.
#[tauri::command]
fn plan_login_cancel() {
    plan::cancel_sign_in();
}

/// The first run's "Sign in to Marv" step, skipped: it doesn't show again (Settings › Plan & account signs in later).
#[tauri::command]
fn account_step_done(desktop: State<'_, Desktop>) -> Result<AppInfo, String> {
    desktop.store.modify_settings(|settings| settings.account_asked = true)?;
    Ok(info(&desktop))
}

/// The Marv plan's status: the server's (`online`, the default), else what plan.json and the licence say.
#[tauri::command]
async fn plan_status(desktop: State<'_, Desktop>, online: Option<bool>) -> Result<plan::PlanStatus, String> {
    Ok(plan::status(&desktop.plan_ctx(), online != Some(false)).await)
}

#[derive(Serialize)]
struct PlanSignOut {
    info: AppInfo,
    /// A key stayed that this computer couldn't confirm as the plan's (`plan::KEY_LEFT`).
    notice: Option<String>,
}

/// The Marv plan: signs this computer out (the plan's key goes only when the server confirms it exactly).
#[tauri::command]
async fn plan_logout(desktop: State<'_, Desktop>) -> Result<PlanSignOut, String> {
    let out = plan::logout(&desktop.plan_ctx()).await?;
    Ok(PlanSignOut { info: info(&desktop), notice: out.notice })
}

/// The Marv plan: the subscription's billing page, in the person's browser.
#[tauri::command]
async fn plan_manage(desktop: State<'_, Desktop>) -> Result<(), String> {
    let url = plan::portal(&desktop.plan_ctx()).await?;
    let url = url.parse().map_err(|e| format!("{e}"))?;
    marv_mcp::oauth::open_browser(&url, std::env::consts::OS);
    Ok(())
}

/// The Marv plan: "Use the Marv plan instead": its key replaces the person's OpenRouter key here (theirs pasted here
/// is cleared, since it would come first).
#[tauri::command]
async fn plan_use_key(desktop: State<'_, Desktop>) -> Result<AppInfo, String> {
    plan::use_plan_key(&desktop.plan_ctx()).await?;
    let mut settings = desktop.store.settings();
    if settings.api_key.is_some() || settings.provider.is_none() {
        settings.api_key = None;
        settings.provider.get_or_insert_with(|| "openrouter".into());
        desktop.store.save_settings(&settings)?;
    }
    Ok(info(&desktop))
}

#[derive(Serialize)]
struct PlanModel {
    /// `zai/<model>`, `kimi/<model>` or `chatgpt/<slug>`, as a chat's model.
    id: String,
    /// "Z.ai Coding Plan", "Kimi Code plan" or "ChatGPT".
    plan: &'static str,
    context: Option<u64>,
    /// What to show (ChatGPT's `display_name`), when the list has one.
    name: Option<String>,
}

/// #159: the models of the plans set up here: the Z.ai Coding Plan's and the Kimi Code plan's (with a key) and the
/// ChatGPT account's (signed in).
#[tauri::command]
async fn plan_models(desktop: State<'_, Desktop>) -> Result<Vec<PlanModel>, String> {
    let mut models = Vec::new();
    if desktop.plan_allowed("zai") && desktop.zai_key().is_some() {
        models.extend(marv_provider::models::zai_models().into_iter().map(|m| PlanModel {
            id: format!("{}{}", marv_web::host::ZAI_PREFIX, m.id),
            plan: "Z.ai Coding Plan",
            context: m.context,
            name: m.name,
        }));
    }
    if desktop.plan_allowed("kimi") && desktop.kimi_key().is_some() {
        models.extend(marv_provider::models::table_for(marv_provider::ProviderKind::Kimi).unwrap_or_default().into_iter().map(
            |m| PlanModel {
                id: format!("{}{}", marv_web::host::KIMI_PREFIX, m.id),
                plan: "Kimi Code plan",
                context: m.context,
                name: m.name,
            },
        ));
    }
    let store = desktop.chatgpt();
    if desktop.plan_allowed("chatgpt") && store.signed_in() {
        let credentials = marv_provider::chatgpt::ChatGptCredentials { store, endpoints: Default::default() };
        let listed = marv_provider::chatgpt::list_models(&credentials).await.unwrap_or_default();
        models.extend(listed.into_iter().map(|m| PlanModel {
            id: format!("{}{}", marv_web::host::CHATGPT_PREFIX, m.id),
            plan: "ChatGPT",
            context: m.context,
            name: m.name,
        }));
    }
    Ok(models)
}

#[tauri::command]
fn list_dir(desktop: State<'_, Desktop>, rel: String) -> Result<Vec<tree::Entry>, String> {
    tree::list(&desktop.root()?, &rel)
}

#[tauri::command]
fn create_folder(desktop: State<'_, Desktop>, parent: String, name: String) -> Result<String, String> {
    tree::create_folder(&desktop.root()?, &parent, &name)
}

#[tauri::command]
fn rename_entry(desktop: State<'_, Desktop>, rel: String, name: String) -> Result<String, String> {
    let from = desktop.full(&rel)?;
    desktop.chats();
    let renamed = tree::rename(&desktop.root()?, &rel, &name)?;
    let to = desktop.full(&renamed)?;
    // Chats in the folder (or below it) follow it.
    desktop.store.modify_chats(|chats| {
        for chat in chats {
            if chat.folder == from {
                chat.folder = to.clone();
            } else if let Some(below) = chat.folder.strip_prefix(&format!("{from}/")) {
                chat.folder = format!("{to}/{below}");
            }
        }
    })?;
    Ok(renamed)
}

/// The person moves a file or folder to the trash: a folder's chats (and theirs under it) go with it, their agents
/// ended (left, they ran on with no folder to show them in).
#[tauri::command]
async fn trash_entry(desktop: State<'_, Desktop>, terminals: State<'_, terminals::Terminals>, rel: String) -> Result<(), String> {
    let full = desktop.full(&rel)?;
    tree::trash(&desktop.root()?, &rel)?;
    let inside = |folder: &str| Path::new(folder).starts_with(&full);
    for chat in desktop.chats().into_iter().filter(|c| inside(&c.folder)) {
        remove_chat(&desktop, &terminals, &chat.id).await?;
    }
    Ok(())
}

/// The chats, newest first: all of them, or one folder's.
#[tauri::command]
fn list_chats(desktop: State<'_, Desktop>, folder: Option<String>) -> Vec<Chat> {
    let folder = folder.and_then(|f| desktop.full(&f).ok());
    let mut chats: Vec<Chat> = desktop.chats().into_iter().filter(|c| folder.as_ref().is_none_or(|f| &c.folder == f)).collect();
    chats.sort_by_key(|c| std::cmp::Reverse(c.created));
    chats
}

#[tauri::command]
fn rename_chat(desktop: State<'_, Desktop>, id: String, title: String) -> Result<(), String> {
    let title: String = title.trim().chars().take(80).collect();
    desktop.store.update_chat(&id, |c| c.title = title).map(|_| ())
}

/// The person's pick in a chat's permission menu (`marv_web::terminal_agents::Permissions`; `own` or none: its agent's
/// own settings). Its agent takes it when its terminal next starts; what the chat will run with, held to what its
/// folder allows.
#[tauri::command]
fn set_chat_permissions(app: AppHandle, id: String, mode: Option<String>) -> Result<String, String> {
    use marv_web::terminal_agents::Permissions;
    let picked = Permissions::parse(mode.as_deref());
    let word = (picked != Permissions::Own).then(|| permissions_word(picked));
    let chat = app.state::<Desktop>().store.update_chat(&id, |c| c.permissions = word)?.ok_or("That chat is gone.")?;
    let held = AppHost(app.clone()).terminal_spec(&chat).map_or(picked, |spec| picked.held(&spec));
    Ok(permissions_word(held))
}

/// Settings' permission mode for new chats (`own`: each agent's own settings).
#[tauri::command]
fn default_permissions(desktop: State<'_, Desktop>) -> String {
    permissions_word(marv_web::terminal_agents::Permissions::parse(desktop.store.settings().default_permissions.as_deref()))
}

/// Settings' permission mode for new chats; `own` or none: each agent's own settings.
#[tauri::command]
fn set_default_permissions(desktop: State<'_, Desktop>, mode: Option<String>) -> Result<(), String> {
    use marv_web::terminal_agents::Permissions;
    let picked = Permissions::parse(mode.as_deref());
    desktop.store.modify_settings(|settings| {
        settings.default_permissions = (picked != Permissions::Own).then(|| permissions_word(picked));
    })
}

fn permissions_word(mode: marv_web::terminal_agents::Permissions) -> String {
    serde_json::to_value(mode).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| "own".into())
}

/// The person's click in a chat's menu: "Private: other chats can't read it" (runtime-kernel §9.2, R3). Applies to the
/// next read (each read looks at the record).
#[tauri::command]
fn set_chat_private(desktop: State<'_, Desktop>, id: String, private: bool) -> Result<(), String> {
    desktop.store.update_chat(&id, |c| c.private = private)?;
    // Runtime-kernel §14.4: its door stops (or starts) saying its write set at once.
    desktop.live.set_private(&id, private);
    Ok(())
}

/// Deleting a chat in its own worktree takes the worktree and its branch with it when nothing in them is lost (no commits
/// the folder's branch lacks, no change left uncommitted, [`worktrees::remove_if_clean`]); else they stay and the answer
/// says so (Merge or Discard, or git, are then the way). What it says, if anything.
#[tauri::command]
async fn delete_chat(
    desktop: State<'_, Desktop>,
    terminals: State<'_, terminals::Terminals>,
    id: String,
) -> Result<Option<String>, String> {
    let mut note = None;
    // A delegate works in its parent's worktree: only the parent's deletion takes it.
    if let Some(chat) = desktop.chats().into_iter().find(|c| c.id == id)
        && chat.delegated_by.is_none()
        && let Some(wt) = chat.worktree.clone()
    {
        // Its agent stops first: nothing writes in the worktree while it is looked at and removed.
        end_delegates(&desktop, &terminals, &id);
        terminals.close_key(&format!("chat:{id}"));
        desktop.live.stop(&id);
        let folder = PathBuf::from(&chat.folder);
        note = match tauri::async_runtime::spawn_blocking(move || worktrees::remove_if_clean(&folder, &wt)).await {
            Ok(worktrees::Cleanup::Kept(why)) => Some(why),
            Ok(worktrees::Cleanup::Removed) => None,
            Err(e) => Some(e.to_string()),
        };
    }
    remove_chat(&desktop, &terminals, &id).await?;
    Ok(note)
}

/// Chat `id` goes: its grant revoked (first: if it can't be, nothing else is done and the person is told), its agent
/// ended, its door and worktree gone, its record removed.
async fn remove_chat(desktop: &Desktop, terminals: &terminals::Terminals, id: &str) -> Result<(), String> {
    // A deleted chat's grant goes with it (a chat id is never reused, but nothing should outlive the chat).
    if let Some(grants) = &desktop.grants
        && let Some(chat) = marv_kernel::AgentId::new(id)
        && let Some(grant) = grants.file.snapshot().for_chat(&chat)
    {
        grants.file.store().revoke(grant.id).await.map_err(|e| {
            format!("The chat wasn't deleted: its permission to work while you're away couldn't be revoked ({e}).")
        })?;
        if let Some(log) = desktop.event_log() {
            marv_session::kernel_events::log_grant_revoked(log, grant.id);
        }
    }
    // Its agent ends with it: left running, it kept working with no chat to show it, off the mesh and off Activity.
    terminals.close_key(&format!("chat:{id}"));
    desktop.live.stop(id);
    let folder = desktop.chats().into_iter().find(|c| c.id == id).map(|c| c.folder);
    end_worktree(desktop, id);
    desktop.store.modify_chats(|chats| chats.retain(|c| c.id != id))?;
    // Its checkpoint refs and files go too: git runs off this thread, and a failure never fails the delete.
    if let Some(folder) = folder {
        let (id, data) = (id.to_string(), desktop.store.data_dir().to_path_buf());
        drop(tokio::task::spawn_blocking(move || checkpoints::forget(&id, std::path::Path::new(&folder), &data)));
    }
    Ok(())
}

/// A new chat in `folder` (relative to the top folder, or `~` for a Home chat in the home folder), titled from its first message, started as `role` when the person
/// picked one (`list_roles`): fixed for the chat's life. An unknown role is refused here, before any chat exists.
///
/// `worktree` (the harness plan, phase 2): it works in a git worktree of its own, a new branch from the folder's last
/// commit, so several chats in one folder (Claude Code's own edits aren't bounded by Marv) don't edit the same files.
#[tauri::command]
async fn create_chat(
    desktop: State<'_, Desktop>,
    folder: String,
    title: String,
    role: Option<String>,
    harness: Option<String>,
    worktree: Option<bool>,
) -> Result<Chat, String> {
    let full = if folder == HOME_FOLDER {
        check_chat_root(&marv_session::paths::home_dir().to_string_lossy())?.to_string_lossy().into_owned()
    } else {
        desktop.full(&folder)?
    };
    if !std::path::Path::new(&full).is_dir() {
        return Err(format!("{folder} isn't a folder."));
    }
    let role = role.filter(|r| !r.is_empty());
    if let Some(name) = &role {
        let (roles, _) =
            marv_session::roles::available_roles(std::path::Path::new(&full), &marv_session::paths::home_dir(), &marv_dir());
        if marv_session::roles::find_role(&roles, name).is_none() {
            return Err(marv_session::roles::unknown_role(&roles, name));
        }
    }
    // Another agent (the harness plan: Claude Code, OpenCode, Gemini CLI, Antigravity): only a name Marv OS knows,
    // only if it's installed. Its role's prompt goes to it at start.
    let harness = harness.filter(|h| !h.is_empty());
    if let Some(name) = &harness {
        check_harness(name)?;
        // An agent's Home chat runs only inside the outer sandbox (`claude_code::home_unsandboxed`): its secrets hidden.
        // On Windows, which has none yet, it starts without it (the page warned first).
        if folder == HOME_FOLDER && !marv_web::harness_sandbox::available() {
            let title = marv_web::terminal_agents::for_chat(Some(name)).title;
            match marv_web::claude_code::home_refusal(title) {
                Some(why) => return Err(why),
                None => applog::warn("chat", &format!("{title} starts in Home without the outer sandbox (none on Windows)")),
            }
        }
    }
    let tree = match worktree {
        Some(true) if folder == HOME_FOLDER => {
            return Err("A Home chat can't have a worktree: start it in a project's folder.".into());
        }
        Some(true) => {
            let dir = std::path::Path::new(&full);
            let base = marv_dir().join("worktrees").join(marv_session::paths::project_key(dir));
            let wt = marv_session::worktree::create_worktree(dir, &base, &title)
                .await
                .map_err(|e| format!("Couldn't make its worktree: {}", e.0))?;
            worktrees::copy_env_files(dir, &wt.work_dir);
            Some(store::ChatWorktree {
                dir: wt.work_dir.display().to_string(),
                branch: wt.branch.clone(),
                base: wt.base.clone(),
                top: wt.dir.display().to_string(),
            })
        }
        _ => None,
    };
    let made = new_chat(&desktop, full.clone(), &title, role, None, None, None).and_then(|mut chat| {
        if harness.is_some() || tree.is_some() {
            desktop.store.update_chat(&chat.id, |c| (c.harness, c.worktree) = (harness.clone(), tree.clone()))?;
            (chat.harness, chat.worktree) = (harness, tree.clone());
        }
        Ok(chat)
    });
    if let (Err(_), Some(wt)) = (&made, &tree) {
        remove_worktree(std::path::Path::new(&full), wt);
    }
    // Where it went, against the Marv folder the tree lists: a chat that doesn't show in its folder (Windows paths,
    // 2026-11-13) is told apart from one that wasn't made.
    match &made {
        Ok(chat) => applog::info(
            "chat",
            &format!(
                "chat {} made in {} (Marv folder {}), {}{}",
                chat.id,
                chat.folder,
                desktop.root().map(|r| r.display().to_string()).unwrap_or_default(),
                chat.harness.as_deref().unwrap_or("marv"),
                chat.role.as_deref().map(|r| format!(", role {r}")).unwrap_or_default()
            ),
        ),
        Err(e) => applog::warn("chat", &format!("a chat in {folder} wasn't made: {e}")),
    }
    made
}

/// One harness Marv OS can run chats with, for Settings → Harnesses (the harness plan, phase 2).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HarnessInfo {
    /// Its name in a chat record (`claude-code`, `opencode`, …).
    id: &'static str,
    name: &'static str,
    /// Where its program is; None: not installed.
    path: Option<String>,
    version: Option<String>,
    /// "api_key", "signed_in", "signed_out" or "unknown" (`marv_web::claude_code::login`).
    login: &'static str,
    /// Its chats now: loaded (a process at most each) and asleep.
    loaded: usize,
    asleep: usize,
    /// Phase 4: its chats run inside Marv's outer sandbox (the person's setting), and whether that works here.
    sandbox: bool,
    sandbox_available: bool,
}

/// The harnesses found here, with their version, login and chats.
#[tauri::command]
async fn harnesses(desktop: State<'_, Desktop>) -> Result<Vec<HarnessInfo>, String> {
    let chats = desktop.chats();
    let states = desktop.live.states();
    let sandbox = desktop.store.settings().harness_sandbox != Some(false);
    let sandbox_available = marv_web::harness_sandbox::available();
    let home = marv_session::paths::home_dir();
    let mut out = Vec::new();
    for harness in marv_web::host::Harness::foreign() {
        let id = harness.name().unwrap_or_default();
        let bin = harness.program();
        let version = match &bin {
            Some(bin) => marv_web::claude_code::version(bin).await,
            None => None,
        };
        let login = match harness {
            marv_web::host::Harness::ClaudeCode => marv_web::claude_code::login(|k| std::env::var(k).ok(), &home),
            _ => "unknown",
        };
        let ids: std::collections::HashSet<&str> =
            chats.iter().filter(|c| c.harness.as_deref() == Some(id)).map(|c| c.id.as_str()).collect();
        let mine: Vec<&background::LiveState> = states.iter().filter(|s| ids.contains(s.id.as_str())).collect();
        out.push(HarnessInfo {
            id,
            name: harness.title(),
            path: bin.map(|b| b.display().to_string()),
            version,
            login,
            loaded: mine.iter().filter(|s| !s.asleep).count(),
            asleep: mine.iter().filter(|s| s.asleep).count(),
            sandbox,
            sandbox_available,
        });
    }
    Ok(out)
}

/// The person turns the outer sandbox for harness chats on or off (Settings → Harnesses): from each chat's next start.
#[tauri::command]
async fn set_harness_sandbox(desktop: State<'_, Desktop>, on: bool) -> Result<Vec<HarnessInfo>, String> {
    desktop.store.modify_settings(|settings| settings.harness_sandbox = if on { None } else { Some(false) })?;
    harnesses(desktop).await
}

/// Whether `folder` (relative to the top folder) is in a git repository with a commit: a new chat there may work in
/// its own worktree.
#[tauri::command]
fn folder_in_git(desktop: State<'_, Desktop>, folder: String) -> bool {
    folder != HOME_FOLDER
        && desktop
            .full(&folder)
            .is_ok_and(|full| git_out(std::path::Path::new(&full), &["rev-parse", "--verify", "HEAD"]).is_some())
}

/// A harness name a chat may be made with: one Marv OS knows (never Marv's own), installed here.
fn check_harness(name: &str) -> Result<(), String> {
    let agent = marv_web::terminal_agents::find(name)
        .filter(|a| a.id != marv_web::terminal_agents::MARV)
        .ok_or_else(|| format!("\"{}\" isn't an agent Marv OS can run.", marv_core::printable(name)))?;
    match agent.installed() {
        Some(_) => Ok(()),
        None => Err(agent.not_installed()),
    }
}

/// One coding agent for the new chat's picker: its record name, what it's called, and whether it's installed here.
#[derive(serde::Serialize)]
struct TerminalAgentInfo {
    id: &'static str,
    name: &'static str,
    installed: bool,
    install: &'static str,
    /// Its winget package, on Windows only: the picker offers Install for it ([`install_package`]).
    winget: Option<&'static str>,
    /// `known`, `own` (the person added it) or `shell` (a plain terminal).
    kind: marv_web::terminal_agents::Kind,
}

/// Every coding agent Marv OS can run in a chat's terminal (`marv_web::terminal_agents`), installed or not: the known
/// ones, the person's shell and the agents they added.
#[tauri::command]
fn terminal_agents() -> Vec<TerminalAgentInfo> {
    marv_web::terminal_agents::all()
        .into_iter()
        .map(|a| TerminalAgentInfo {
            id: a.id,
            name: a.title,
            installed: a.installed().is_some(),
            install: a.install,
            winget: a.winget.filter(|_| cfg!(windows)),
            kind: a.kind,
        })
        .collect()
}

/// The agents the person added (Settings > Agents).
#[tauri::command]
fn own_agents(desktop: State<'_, Desktop>) -> Vec<marv_web::terminal_agents::OwnAgent> {
    desktop.store.own_agents()
}

/// Adds an agent of the person's own: `command` starts it (its program found here), `resume` continues its latest
/// conversation in the folder (empty: it starts afresh). It's in the new chat's picker at once.
#[tauri::command]
fn add_own_agent(
    desktop: State<'_, Desktop>,
    name: String,
    command: String,
    resume: String,
) -> Result<Vec<marv_web::terminal_agents::OwnAgent>, String> {
    let mut list = desktop.store.own_agents();
    if list.iter().any(|o| o.name.trim().eq_ignore_ascii_case(name.trim())) {
        return Err(format!("There's already an agent called {}.", name.trim()));
    }
    let taken: Vec<&str> = list.iter().map(|o| o.id.as_str()).collect();
    let added = marv_web::terminal_agents::OwnAgent::new(&name, &command, &resume, &taken)?;
    list.push(added);
    desktop.store.save_own_agents(&list)?;
    marv_web::terminal_agents::set_own(&list);
    Ok(list)
}

/// Removes one of the person's own agents. Its chats stay, and say so when they next start.
#[tauri::command]
fn remove_own_agent(desktop: State<'_, Desktop>, id: String) -> Result<Vec<marv_web::terminal_agents::OwnAgent>, String> {
    let mut list = desktop.store.own_agents();
    list.retain(|o| o.id != id);
    desktop.store.save_own_agents(&list)?;
    marv_web::terminal_agents::set_own(&list);
    Ok(list)
}

/// Git for Windows' winget package: Marv OS's checkpoints, turns, worktrees and the Git tab all run `git`.
const GIT_PACKAGE: &str = "Git.Git";

/// Whether this is Windows without `git` on the PATH, or a Mac without the developer tools (`/usr/bin/git` is there
/// but only a stub that asks for them): the page then says so, with Install Git, instead of the features that need it
/// quietly not showing (checkpoints took every folder for one outside a repository).
#[tauri::command]
fn git_missing() -> bool {
    if cfg!(target_os = "macos") {
        return !marv_core::process::command("xcode-select")
            .arg("-p")
            .stdin(std::process::Stdio::null())
            .output()
            .is_ok_and(|o| o.status.success());
    }
    cfg!(windows) && marv_session::paths::on_path("git").is_none()
}

/// Installs one of the packages Marv OS offers with winget: an agent's from [`marv_web::terminal_agents::AGENTS`],
/// or Git. Only those: the page names a package, never a command, so nothing else can be installed through here.
/// winget shows the publisher's own installer (and Windows asks before one that installs for every user); what it
/// said is returned, for the page.
#[tauri::command]
async fn install_package(id: String) -> Result<String, String> {
    // A Mac's git comes with Apple's developer tools: their own installer opens (it asks, and downloads).
    if cfg!(target_os = "macos") && id == GIT_PACKAGE {
        marv_core::process::command("xcode-select")
            .arg("--install")
            .spawn()
            .map_err(|e| format!("Couldn't start the installer ({e})."))?;
        return Ok("Apple's developer tools installer is open: finish it, then restart Marv OS.".into());
    }
    let offered = id == GIT_PACKAGE || marv_web::terminal_agents::AGENTS.iter().any(|a| a.winget == Some(id.as_str()));
    if !cfg!(windows) || !offered {
        return Err(format!("{id} isn't a package Marv OS installs."));
    }
    let out = tokio::task::spawn_blocking(move || {
        marv_core::process::command("winget")
            .args(["install", "--id", &id, "--exact", "--source", "winget", "--silent"])
            .args(["--accept-package-agreements", "--accept-source-agreements", "--disable-interactivity"])
            .output()
            .map(|out| (id, out))
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| {
        format!("Couldn't run winget ({e}). It comes with Windows 10 and 11 as App Installer, from the Microsoft Store.")
    })?;
    let (id, out) = out;
    let said = String::from_utf8_lossy(&out.stdout);
    // Its last few lines: winget draws progress bars with carriage returns before them.
    let tail = said.lines().map(|l| l.rsplit('\r').next().unwrap_or(l).trim()).filter(|l| !l.is_empty()).collect::<Vec<_>>();
    let tail = tail[tail.len().saturating_sub(3)..].join(" ");
    // Already there counts as done: "Found an existing package already installed".
    if out.status.success() || said.contains("already installed") {
        applog::info("install", &format!("{id}: {tail}"));
        Ok(tail)
    } else {
        applog::warn("install", &format!("{id} (exit {:?}): {tail}", out.status.code()));
        Err(format!("winget couldn't install {id}: {tail}"))
    }
}

/// Whether Claude Code is installed here, for the new chat's agent picker.
#[tauri::command]
fn claude_code_available() -> bool {
    marv_web::claude_code::find_claude().is_some()
}

/// Saves a new chat record in `folder` (a full path, checked by the caller), not started.
fn new_chat(
    desktop: &Desktop,
    folder: String,
    title: &str,
    role: Option<String>,
    team: Option<store::ChatTeam>,
    settings: Option<store::ChatSettings>,
    writes: Option<Vec<String>>,
) -> Result<Chat, String> {
    let chat = Chat {
        id: uuid::Uuid::new_v4().to_string(),
        folder,
        title: title.trim().chars().take(80).collect(),
        session_id: None,
        created: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        settings,
        private: false,
        worktree: None,
        role,
        team,
        writes,
        harness: None,
        agent_model: None,
        terminal_started: false,
        // Settings' default for new chats.
        permissions: desktop.store.settings().default_permissions,
        triage: false,
        past_agents: Vec::new(),
        delegated_by: None,
    };
    desktop.chats();
    desktop.store.modify_chats(|chats| chats.push(chat.clone()))?;
    Ok(chat)
}

/// The other chats of `chat`'s team (the same team name, in the same folder), each with its role, name and mesh name,
/// so its brief names their inboxes: a coordinator otherwise picks any Marv on the machine with the right role (a
/// `frontend` chat in another folder). The names are fixed from the chats' ids and folders, the same at every start.
fn team_peers(chats: &[Chat], chat: &Chat) -> Vec<marv_session::teams::TeamPeer> {
    let Some(team) = &chat.team else { return Vec::new() };
    chats
        .iter()
        .filter(|c| c.id != chat.id && c.folder == chat.folder && c.team.as_ref().is_some_and(|t| t.name == team.name))
        .filter_map(|c| {
            let agent = marv_kernel::AgentId::new(&c.id)?;
            let folder = check_chat_root(&c.folder).ok()?;
            Some(marv_session::teams::TeamPeer {
                role: c.role.clone()?,
                title: c.title.clone(),
                mesh_name: marv_kernel::mesh_name_for(&folder, &agent),
            })
        })
        .collect()
}

/// Opens a chat: the page is attached to its events (its cards, its state); its agent runs in the chat's terminal
/// (`chat_terminal_open`), the agent's own interface with its own model and provider pickers. `model`, `max_steps` and
/// `yolo` are kept as the chat's last settings, for a headless wake of a chat whose terminal isn't open.
#[tauri::command]
#[allow(clippy::too_many_arguments)] // A Tauri command's arguments are the page's named fields.
async fn open_chat(
    desktop: State<'_, Desktop>,
    id: String,
    model: String,
    max_steps: u32,
    yolo: bool,
    mcp_off: Vec<String>,
    generation: Option<u32>,
    after: Option<u64>,
    epoch: Option<String>,
    on_event: Channel<Value>,
) -> Result<Chat, String> {
    let _ = generation;
    let chat = desktop.chats().into_iter().find(|c| c.id == id).ok_or("That chat is gone.")?;
    // As the person left it: a message that wakes it with no page showing it loads it like this (runtime-kernel 1).
    // Its device switches (runtime-kernel §17.1, D1) stay as `set_chat_devices` left them.
    let devices_off = chat.settings.as_ref().map(|s| s.devices_off.clone()).unwrap_or_default();
    let last = store::ChatSettings { model, max_steps: max_steps.min(500), yolo, mcp_off, devices_off };
    let _ = desktop.store.update_chat(&chat.id, |c| c.settings = Some(last));
    // Wherever the top folder is now: the chat runs in its own folder (never above home; home only for a Home chat).
    let dir = check_chat_root(&chat.folder).map_err(|e| format!("This chat's folder: {e}"))?;
    let sink: background::Sink = Box::new(move |event| on_event.send(event).is_ok());
    let agent = marv_web::terminal_agents::for_chat(chat.harness.as_deref());
    desktop.live.attach(&chat.id, &dir, agent.id, sink, (after.unwrap_or(0), epoch.as_deref()));
    Ok(chat)
}

/// A device name the page may switch off (runtime-kernel §17.1): a door, or `mcp:<server>`.
fn is_device_name(name: &str) -> bool {
    matches!(name, "github" | "process" | "host" | "herdr")
        || name.strip_prefix("mcp:").is_some_and(|server| !server.is_empty() && server.len() <= 64)
}

/// Kernel #53, the dock's files: a folder in the chat's own root (its worktree when it works in one), relative to it.
#[tauri::command]
fn chat_files(desktop: State<'_, Desktop>, id: String, rel: String) -> Result<Vec<tree::Entry>, String> {
    let chat = desktop.chats().into_iter().find(|c| c.id == id).ok_or("That chat is gone.")?;
    let (root, _) = work_root(&chat)?;
    tree::list(&root, &rel)
}

/// Kernel #53: the files in the chat's root that a desktop chat wrote in the last 30 minutes (runtime-kernel §14.4),
/// with who. Paths only, never content.
#[tauri::command]
fn chat_writes(desktop: State<'_, Desktop>, id: String) -> Result<Vec<dock::WriteMark>, String> {
    let chats = desktop.chats();
    let chat = chats.iter().find(|c| c.id == id).ok_or("That chat is gone.")?;
    let (root, _) = work_root(chat)?;
    let title = |cid: &str| chats.iter().find(|c| c.id == cid).map(|c| c.title.clone()).unwrap_or_default();
    let sets: Vec<(String, String, Vec<marv_kernel::RecentWrite>)> =
        desktop.live.write_sets().into_iter().map(|(cid, writes)| (cid.clone(), title(&cid), writes)).collect();
    Ok(dock::marks(&root, &id, &sets))
}

/// Kernel #53, the dock's changes: the git diff of the chat's root, against its worktree's base (the branch's work) or
/// HEAD (what isn't committed), and the untracked files. Read-only.
#[tauri::command]
async fn chat_changes(desktop: State<'_, Desktop>, id: String) -> Result<dock::Changes, String> {
    let chat = desktop.chats().into_iter().find(|c| c.id == id).ok_or("That chat is gone.")?;
    let (root, gone) = work_root(&chat)?;
    let base = if gone.is_none() { chat.worktree.as_ref().map(|w| w.base.clone()) } else { None };
    tauri::async_runtime::spawn_blocking(move || dock::changes(&root, base.as_deref())).await.map_err(|e| e.to_string())
}

/// The folder a dock tab shows: the chat's own (its worktree when it has one), else a folder of the tree (a draft's).
fn dock_folder(desktop: &Desktop, chat: Option<&str>, folder: &str) -> Result<PathBuf, String> {
    match chat {
        Some(id) => {
            let chat = desktop.chats().into_iter().find(|c| c.id == id).ok_or("That chat is gone.")?;
            Ok(work_root(&chat)?.0)
        }
        None => Ok(PathBuf::from(desktop.full(folder)?)),
    }
}

/// The dock's Git tab: the folder's lazygit on `on_data` at this size (the running one, else a new one): its id.
#[tauri::command]
fn git_terminal_open(
    desktop: State<'_, Desktop>,
    terminals: State<'_, terminals::Terminals>,
    chat: Option<String>,
    folder: String,
    cols: u16,
    rows: u16,
    on_data: Channel<Value>,
) -> Result<u64, String> {
    let root = dock_folder(&desktop, chat.as_deref(), &folder)?;
    terminals.open(&root, cols, rows, on_data)
}

/// The provider Marv's config names now (`~/.marv/config.json`'s "provider"), if any.
fn config_provider() -> Option<String> {
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(marv_dir().join("config.json")).ok()?).ok()?;
    saved.get("provider")?.as_str().map(str::to_string)
}

/// What a provider is called on the page.
fn provider_title(id: Option<&str>) -> String {
    match id {
        Some("openrouter") => "OpenRouter".into(),
        Some("ollama") => "Ollama".into(),
        Some("zai") => "the Z.ai Coding Plan".into(),
        Some("kimi") => "the Kimi Code plan".into(),
        Some("chatgpt") => "ChatGPT".into(),
        Some(other) => other.to_string(),
        None => "no provider".into(),
    }
}

/// The card over Marv chat `id`'s terminal when Marv's provider changed after it started (it keeps the one it started
/// on until it starts again), else None.
fn provider_change_note(started: Option<&str>, now: Option<&str>) -> Option<String> {
    (started != now).then(|| {
        format!(
            "This chat's Marv started on {}, before {} was set up: it stays on {} until it starts again.",
            provider_title(started),
            provider_title(now),
            provider_title(started)
        )
    })
}

/// Whether running Marv chat `id` is on an older provider than Marv's config names now (`Desktop::started_on`).
#[tauri::command]
fn chat_provider_changed(app: AppHandle, desktop: State<'_, Desktop>, id: String) -> Option<String> {
    if !app.state::<terminals::Terminals>().running(&format!("chat:{id}")) {
        return None;
    }
    let started = desktop.started_on.lock().unwrap_or_else(|e| e.into_inner()).get(&id).cloned()?;
    provider_change_note(started.as_deref(), config_provider().as_deref())
}

/// Delivers terminal chat `id`'s waiting mail (the card over its terminal).
#[tauri::command]
fn chat_deliver_mail(desktop: State<'_, Desktop>, id: String) -> Result<(), String> {
    desktop.live.deliver_mail(&id)
}

/// Text copied in a terminal onto the system clipboard: what an agent's TUI copied (OSC 52: Claude Code's "copied"),
/// or the person's selection (ctrl+shift+c). The web view's clipboard only writes inside a click, and an agent copies
/// whenever its own selection ends: it said "copied" and the clipboard kept what was there.
#[tauri::command]
fn clipboard_write(app: tauri::AppHandle, text: String) -> Result<(), String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    app.clipboard().write_text(text).map_err(|e| e.to_string())
}

/// The system clipboard's text, for ctrl+shift+v in a terminal: the person's own paste, never an agent's request.
#[tauri::command]
fn clipboard_read(app: tauri::AppHandle) -> Result<String, String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    app.clipboard().read_text().map_err(|e| e.to_string())
}

/// Whether the clipboard holds a picture, for a terminal's paste that found no text (a screenshot): the page then
/// sends Claude Code its own key for pasting one. Only whether: the picture itself never passes through here.
/// Off the main thread, as the clipboard library asks (reading a picture there can deadlock on Linux).
#[tauri::command]
async fn clipboard_has_image(app: tauri::AppHandle) -> bool {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    tokio::task::spawn_blocking(move || app.clipboard().read_image().is_ok()).await.unwrap_or(false)
}

/// What the person typed into a terminal (a chat's agent, the Git tab's lazygit). A line sent to a chat's agent is the
/// person writing in that chat ([`chats::Chats::person_typed`]).
#[tauri::command]
fn git_terminal_write(
    desktop: State<'_, Desktop>,
    terminals: State<'_, terminals::Terminals>,
    id: u64,
    data: String,
) -> Result<(), String> {
    terminals.write(id, &data)?;
    if data.contains('\r')
        && let Some(chat) = terminals.key_of(id).as_deref().and_then(|k| k.strip_prefix("chat:"))
    {
        desktop.live.person_typed(chat);
    }
    Ok(())
}

/// The Git tab's new size.
#[tauri::command]
fn git_terminal_resize(terminals: State<'_, terminals::Terminals>, id: u64, cols: u16, rows: u16) {
    terminals.resize(id, cols, rows);
}

/// The Git tab stopped showing it (another tab, another chat): it keeps running.
#[tauri::command]
fn git_terminal_detach(terminals: State<'_, terminals::Terminals>, id: u64) {
    terminals.detach(id);
}

/// A fresh lazygit for the folder: this one ends.
#[tauri::command]
fn git_terminal_close(terminals: State<'_, terminals::Terminals>, id: u64) {
    terminals.close(id);
}

/// A chat's terminal: its agent's own interface (`marv_web::terminal_agents`: Claude Code, Codex, OpenCode, Marv…) on
/// the chat's conversation, as in the person's terminal (inside Marv's outer sandbox in a Home chat). The running one
/// is attached to again. See [`start_chat_terminal`].
#[tauri::command]
async fn chat_terminal_open(app: AppHandle, id: String, cols: u16, rows: u16, on_data: Channel<Value>) -> Result<u64, String> {
    start_chat_terminal(&app, &id, cols, rows, Some(on_data), false)
}

/// Starts chat `id`'s terminal (or shows the running one on `on_data`). `unattended`: Marv OS starts it for mail with
/// nobody watching, so inside the outer sandbox; one the person opens runs as their terminal would (the sandbox only in
/// a Home chat). A Marv chat's terminal takes the chat's door (its mesh name); Claude Code's, Codex's and OpenCode's get
/// its mesh tools. Nothing loads the chat headless while it runs (one process per conversation). Its turns reach
/// Activity: Claude Code's through its hooks, with its cost (`terminal_events`), any other agent's from its terminal's
/// output (working while it draws).
fn start_chat_terminal(
    app: &AppHandle,
    id: &str,
    cols: u16,
    rows: u16,
    on_data: Option<Channel<Value>>,
    unattended: bool,
) -> Result<u64, String> {
    let desktop = app.state::<Desktop>();
    let terminals = app.state::<terminals::Terminals>();
    let key = format!("chat:{id}");
    let chat = desktop.chats().into_iter().find(|c| c.id == id).ok_or("That chat is gone.")?;
    // One of the person's own agents they've since removed: never Marv in its place.
    if let Some(gone) = chat.harness.as_deref().filter(|h| h.starts_with("own-") && marv_web::terminal_agents::find(h).is_none())
    {
        return Err(format!(
            "This chat's agent ({}) was removed in Settings > Agents. Add it again there to continue this chat.",
            marv_core::printable(gone.trim_start_matches("own-"))
        ));
    }
    let agent = marv_web::terminal_agents::for_chat(chat.harness.as_deref());
    // One start at a time per chat: a second (the page opening while a queued message starts it) waits here, then
    // finds the first one's terminal running. Nothing below awaits, so a std lock is right.
    let start_lock = terminals.start_lock(&key);
    let _starting = start_lock.lock().unwrap_or_else(|e| e.into_inner());
    if terminals.running(&key) {
        let unused = terminals::Launch {
            argv: Vec::new(),
            path: None,
            withheld: Vec::new(),
            cwd: PathBuf::new(),
            keep: None,
            env: Vec::new(),
        };
        return terminals.open_with(key, unused, cols, rows, on_data, None, None);
    }
    let dir = check_chat_root(&chat.folder).map_err(|e| format!("This chat's folder: {e}"))?;
    // This start owns the chat's terminal: an older one's `ended`, whenever it comes, leaves the chat's state alone.
    let generation = terminals.claim(&key);
    desktop.live.to_terminal(id, &dir, agent.id == marv_web::terminal_agents::MARV)?;
    let started = (|| {
        let mut spec = AppHost(app.clone()).terminal_spec(&chat)?;
        let (keep, out) = desktop.live.terminal_parts(id).ok_or("This chat isn't open.")?;
        spec.resident = Some(keep.clone());
        // As in the person's terminal while they watch; the outer sandbox when nobody does, and always in a Home chat.
        spec.harness_sandbox = unattended && marv_web::harness_sandbox::available();
        // Its `delegate` tools (on its mesh server), unless it's a delegate itself: one level, no chains.
        if chat.delegated_by.is_none() {
            spec.delegates = Some(std::sync::Arc::new(delegates::DesktopDelegates { app: app.clone(), chat: id.to_string() }));
        }
        // Its permission mode, held to what its folder allows; Full access only ever inside the outer sandbox.
        let mode = marv_web::terminal_agents::Permissions::parse(chat.permissions.as_deref()).held(&spec);
        if mode.needs_sandbox() && agent.id != marv_web::terminal_agents::MARV {
            if !marv_web::harness_sandbox::available() {
                return Err(if cfg!(windows) {
                    "Full access needs Marv's sandbox, which Windows doesn't have yet. Pick another mode for this chat."
                } else {
                    "Full access needs Marv's sandbox, and bubblewrap isn't available here. Pick another mode for this \
                     chat, or install bubblewrap."
                }
                .into());
            }
            spec.harness_sandbox = true;
        }
        // Its conversation: an agent that keeps sessions by an id Marv gives continues the chat's own; another
        // continues its latest in this folder once it has run here.
        let (session, again) = if agent.id == marv_web::terminal_agents::MARV {
            // Marv: the chat's own session by id (recorded from its last run, below), else a new one.
            (chat.session_id.clone().unwrap_or_default(), false)
        } else if agent.takes_session_id() {
            match chat.session_id.clone() {
                Some(session) => (session, true),
                None => {
                    let session = uuid::Uuid::new_v4().to_string();
                    let _ = desktop.store.update_chat(id, |c| c.session_id = Some(session.clone()));
                    (session, false)
                }
            }
        } else {
            (String::new(), chat.terminal_started)
        };
        let _ = desktop.store.update_chat(id, |c| c.terminal_started = true);
        let events = marv_dir().join("terminals").join(format!("{id}.jsonl"));
        std::fs::create_dir_all(events.parent().unwrap_or(&events)).map_err(|e| e.to_string())?;
        std::fs::write(&events, "").map_err(|e| e.to_string())?;
        // A Marv reads its own config: the desktop's first-run choice goes in when it has no provider yet, else its
        // terminal opened Marv's setup and asked again for the key just pasted (and started on no provider).
        // The keys from the desktop itself: a terminal's spec carries none (`build_spec`: Marv's keys don't apply to a
        // terminal's agent), so the spec's empty key left the first Windows build of this asking again.
        if agent.id == marv_web::terminal_agents::MARV {
            let settings = desktop.store.settings();
            let (api_key, zai_api_key, kimi_api_key) = (desktop.api_key(), desktop.zai_key(), desktop.kimi_key());
            let choice = marv_config::DesktopChoice {
                provider: settings.provider.as_deref(),
                api_key: api_key.as_deref(),
                zai_api_key: zai_api_key.as_deref(),
                kimi_api_key: kimi_api_key.as_deref(),
                model: &spec.model,
            };
            if let Err(e) = marv_config::seed(&spec.config_dir, &choice) {
                applog::warn("chat", &format!("Marv's config wasn't set up from the desktop's choice: {e}"));
            }
        }
        let command = marv_web::terminal_agents::command(agent, &spec, &session, again, &events, mode)?;
        // A Marv records its session's id at start: kept as the chat's, so its next start continues it.
        if agent.id == marv_web::terminal_agents::MARV {
            // With no model of its own it runs on the config's provider, read once now (`chat_provider_changed`).
            let mut started_on = desktop.started_on.lock().unwrap_or_else(|e| e.into_inner());
            if spec.team.is_none() || spec.model.is_empty() {
                started_on.insert(id.to_string(), config_provider());
            } else {
                started_on.remove(id);
            }
            drop(started_on);
            let record = marv_web::terminal_agents::marv_session_record(&events);
            let _ = std::fs::remove_file(&record);
            let (app, chat_id) = (app.clone(), id.to_string());
            tauri::async_runtime::spawn(async move {
                for _ in 0..40 {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    if let Ok(saved) = std::fs::read_to_string(&record).map(|s| s.trim().to_string())
                        && !saved.is_empty()
                    {
                        let _ = app.state::<Desktop>().store.update_chat(&chat_id, |c| c.session_id = Some(saved));
                        return;
                    }
                }
            });
        }
        let launch = terminals::Launch {
            argv: command.argv,
            path: command.path,
            withheld: command.withheld,
            cwd: spec.root.clone(),
            keep: command.mesh.map(|m| Box::new(m) as Box<dyn std::any::Any + Send>),
            env: command.env,
        };
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let budget = marv_web::harness_budget::HarnessBudget::new(&spec, agent.id, keep.approver.clone(), out);
        let sink: std::sync::Arc<dyn terminal_events::Sink> = std::sync::Arc::new(terminal_events::ChatSink {
            chats: desktop.live.clone(),
            id: id.to_string(),
            budget,
            turns: checkpoints::Recorder::new(id, &spec.root, desktop.store.data_dir(), now_ms).map(std::sync::Arc::new),
            delegations: app.state::<std::sync::Arc<delegates::Delegations>>().inner().clone(),
        });
        // Every agent is seen from its terminal's screen (its rules: working, needs you, idle) and output (working
        // while it draws, idle once quiet); Claude Code also says its turns, questions and cost through its hooks, and
        // Codex its turns' ends, which win over the screen ([`agent_detect`]).
        let seen = std::sync::Arc::new(terminal_events::Output::default());
        if agent.id == marv_web::claude_code::NAME || agent.id == "codex" {
            tauri::async_runtime::spawn(terminal_events::watch(events, stop.clone(), sink.clone(), Some(seen.clone())));
        } else {
            let _ = std::fs::remove_file(&events);
        }
        tauri::async_runtime::spawn(agent_detect::watch(agent.id, seen.clone(), stop.clone(), sink));
        let output = Some(seen);
        let (live, back, ended_key, handle) = (desktop.live.clone(), id.to_string(), key.clone(), app.clone());
        let stopped = stop.clone();
        let ended = Box::new(move || {
            stop.store(true, std::sync::atomic::Ordering::SeqCst);
            // A restart or agent switch started a newer terminal for this chat: its state isn't this one's to clear.
            if !handle.state::<terminals::Terminals>().owns(&ended_key, generation) {
                return;
            }
            live.terminal_meter(&back, None);
            live.terminal_closed(&back);
        });
        // Never started: its watchers (events, screen, checkpoints) end with it, not run on for nothing.
        let opened = terminals.open_with(key.clone(), launch, cols, rows, on_data, Some(ended), output).inspect_err(|_| {
            stopped.store(true, std::sync::atomic::Ordering::SeqCst);
        })?;
        // Deleted while it started (a slow start, the person gave up on it): its agent ends now, never left running
        // with no chat to show it (a Marv deleted mid-start came back in Activity, still working; seen 2026-11-17).
        if !desktop.chats().iter().any(|c| c.id == id) {
            terminals.close_key(&key);
            desktop.live.stop(id);
            return Err("That chat was deleted.".into());
        }
        // Its row in Activity: the agent, its process (with what it runs) and its turns.
        if let Some(pid) = terminals.pid(&key) {
            desktop.live.terminal_meter(id, Some((agent.title, pid)));
        }
        Ok(opened)
    })();
    if started.is_err() {
        desktop.live.terminal_closed(id);
    }
    started
}

/// The person's message for a chat's agent from another view (a team's first task for its lead, "Tell the team"):
/// typed into its terminal, which starts if it isn't running (as the person's: no sandbox but a Home chat's), once its
/// agent is ready.
#[tauri::command]
async fn chat_terminal_type(app: AppHandle, id: String, text: String) -> Result<(), String> {
    let running = app.state::<terminals::Terminals>().running(&format!("chat:{id}"));
    if !running {
        start_chat_terminal(&app, &id, 120, 36, None, false)?;
    }
    let live = &app.state::<Desktop>().live;
    // The person's words, typed for them: their own turn, as if they'd typed it there.
    live.person_typed(&id);
    live.type_when_ready(&id, text, !running);
    Ok(())
}

/// The chat's terminal closes (its `claude` ends): the chat comes back.
#[tauri::command]
fn chat_terminal_close(desktop: State<'_, Desktop>, terminals: State<'_, terminals::Terminals>, id: String) {
    terminals.close_key(&format!("chat:{id}"));
    desktop.live.terminal_closed(&id);
}

/// Whether the chat is open in its terminal now (the page shows the terminal view then).
#[tauri::command]
fn chat_terminal_running(terminals: State<'_, terminals::Terminals>, id: String) -> bool {
    terminals.running(&format!("chat:{id}"))
}

/// Whether the Git tab's folder is a git repository (lazygit runs only in one). An empty `.git` (a sandbox
/// placeholder left behind) isn't one.
#[tauri::command]
fn git_repo_here(desktop: State<'_, Desktop>, chat: Option<String>, folder: String) -> Result<bool, String> {
    let root = dock_folder(&desktop, chat.as_deref(), &folder)?;
    Ok(git_out(&root, &["rev-parse", "--is-inside-work-tree"]).is_some_and(|out| out.trim() == "true"))
}

/// The person starts a git repository in the Git tab's folder (`git init`): lazygit runs there then.
#[tauri::command]
fn git_init_here(desktop: State<'_, Desktop>, chat: Option<String>, folder: String) -> Result<(), String> {
    let root = dock_folder(&desktop, chat.as_deref(), &folder)?;
    let out = marv_core::process::command("git")
        .arg("init")
        .current_dir(&root)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("git init: {}", marv_core::printable(String::from_utf8_lossy(&out.stderr).trim())))
    }
}

/// The Git tab's pop-out: lazygit in the folder in the person's own terminal window (`xdg-terminal-exec`). On a Mac
/// Terminal opens in the folder (it can't be told a program), and on Windows there's no pop-out yet.
#[tauri::command]
fn git_terminal_pop_out(desktop: State<'_, Desktop>, chat: Option<String>, folder: String) -> Result<(), String> {
    let root = dock_folder(&desktop, chat.as_deref(), &folder)?;
    if cfg!(windows) {
        return Err("Popping out isn't available on Windows yet.".into());
    }
    if cfg!(target_os = "macos") {
        return marv_core::process::command("open")
            .args(["-a", "Terminal"])
            .arg(&root)
            .stdin(std::process::Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Couldn't open Terminal ({e})."));
    }
    marv_core::process::command("xdg-terminal-exec")
        .arg(terminals::PROGRAM)
        .current_dir(&root)
        .stdin(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|_| "Popping out needs xdg-terminal-exec (on Omarchy it's there).".to_string())
}

/// mk-60m.11, the dock's Terminal tab: the background processes the chat started with the process tool, with the URLs
/// they're forwarded to. Empty while the chat isn't loaded (its processes ended with its session). Off the main thread:
/// the list is read under the manager's lock.
#[tauri::command]
async fn chat_processes(desktop: State<'_, Desktop>, id: String) -> Result<Vec<dock::ProcessView>, String> {
    let Some(session) = desktop.live.session(&id) else { return Ok(Vec::new()) };
    tauri::async_runtime::spawn_blocking(move || dock::processes(&session.processes())).await.map_err(|e| e.to_string())
}

/// mk-60m.11: a process's last lines of output, read as the process tool's `logs` action reads them (for the person's
/// eyes only: nothing here reaches a model). Off the main thread: up to a megabyte of log, converted under the lock.
#[tauri::command]
async fn chat_process_logs(desktop: State<'_, Desktop>, id: String, name: String) -> Result<String, String> {
    let session = desktop.live.session(&id).ok_or("The chat isn't running: its processes ended with it.")?;
    tauri::async_runtime::spawn_blocking(move || session.process_logs(&name, dock::LOG_LINES).map(|log| dock::log_tail(&log)))
        .await
        .map_err(|e| e.to_string())?
}

/// mk-60m.11: the person's Stop (confirmed on the page): the process and everything it started end, as the process
/// tool's `stop` does. `started_at` is the start the person saw ([`dock::ProcessView::started_at`]): a process of that
/// name started again since (while the confirm was open) is refused, never stopped in its place. False when it was
/// already gone.
#[tauri::command]
async fn stop_chat_process(desktop: State<'_, Desktop>, id: String, name: String, started_at: u64) -> Result<bool, String> {
    let session = desktop.live.session(&id).ok_or("The chat isn't running: its processes ended with it.")?;
    if !dock::same_start(&session.processes(), &name, started_at)? {
        return Ok(false);
    }
    Ok(session.stop_process(&name).await)
}

/// Kernel #53: `smallModel` and the mesh's wake, radio and cards from Marv's own file, for Settings and Teams.
#[tauri::command]
fn marv_config() -> marv_config::MarvConfig {
    marv_config::read(&marv_dir())
}

/// The person's click in Settings or Teams: saved to Marv's own file, merged with it as it is now.
#[tauri::command]
fn set_marv_config(patch: marv_config::ConfigPatch) -> Result<marv_config::MarvConfig, String> {
    marv_config::save(&marv_dir(), &patch)
}

/// The devices switched off for chat `id` (runtime-kernel §17.1, D1).
#[tauri::command]
fn chat_devices(desktop: State<'_, Desktop>, id: String) -> Result<ChatDevices, String> {
    let chat = desktop.chats().into_iter().find(|c| c.id == id).ok_or("That chat is gone.")?;
    let off = chat.settings.as_ref().map(|s| s.devices_off.clone()).unwrap_or_default();
    // The chat's own folder (where it runs), not the folder the sidebar shows: a chat opened from elsewhere lists its
    // own servers.
    let dir = check_chat_root(&chat.folder).map_err(|e| format!("This chat's folder: {e}"))?;
    let env: marv_mcp::Env = std::env::vars().collect();
    Ok(ChatDevices { names: device_list(&dir, &marv_dir(), &env, &off), off })
}

/// A chat's devices for its switches, and which are off.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct ChatDevices {
    names: Vec<String>,
    off: Vec<String>,
}

/// The doors a desktop chat can switch off (runtime-kernel §17.1).
const CHAT_DOORS: [&str; 4] = ["github", "herdr", "process", "host"];

/// What a chat in `dir` can switch: the doors, then `mcp:<name>` for every server configured for it (yours and the
/// folder's, whether running or not: a switched-off one isn't started) and every one it already switched off (so it
/// can be switched back on even if it's gone from the config), sorted.
fn device_list(dir: &Path, config_dir: &Path, env: &marv_mcp::Env, off: &[String]) -> Vec<String> {
    let include = marv_mcp::config::Include { personal: true, project: true };
    let mut servers: Vec<String> = marv_mcp::config::load_mcp_config(dir, config_dir, env, include)
        .servers
        .into_iter()
        .map(|s| format!("mcp:{}", s.name))
        .chain(off.iter().filter(|d| d.starts_with("mcp:")).cloned())
        .collect();
    servers.sort();
    servers.dedup();
    CHAT_DOORS.iter().map(|d| d.to_string()).chain(servers).collect()
}

/// Switches devices off (or back on) for chat `id`. Saved with the chat; its session reads them when it next starts
/// (a device set is fixed for a boot, the prompt cache), so a running chat keeps what it has until then.
#[tauri::command]
fn set_chat_devices(desktop: State<'_, Desktop>, id: String, off: Vec<String>) -> Result<Vec<String>, String> {
    let chat = desktop.chats().into_iter().find(|c| c.id == id).ok_or("That chat is gone.")?;
    let settings = chat.settings.clone().ok_or("Open this chat once first.")?;
    let mut off: Vec<String> = off.into_iter().filter(|d| is_device_name(d)).take(64).collect();
    off.sort();
    off.dedup();
    let saved = store::ChatSettings { devices_off: off.clone(), ..settings };
    desktop.store.update_chat(&chat.id, |c| c.settings = Some(saved))?;
    Ok(off)
}

/// The desktop as the chats' [`chats::Host`].
struct AppHost(AppHandle);

impl AppHost {
    /// The chat's spec for its agent's terminal ([`AppHost::build_spec`]).
    fn terminal_spec(&self, chat: &Chat) -> Result<ChatSpec, String> {
        self.build_spec(&chat.id, true)
    }

    /// Chat `id`'s spec. `terminal`: for its agent's terminal (`chat_terminal_open`), which runs on the agent's own
    /// login and settings: no Marv key, plan or model is needed, nor the chat opened before.
    fn build_spec(&self, id: &str, terminal: bool) -> Result<ChatSpec, String> {
        let desktop = self.0.state::<Desktop>();
        let chat = desktop.chats().into_iter().find(|c| c.id == id).ok_or("That chat is gone.")?;
        // Every agent but Claude Code (and Marv) runs only in its terminal: a wake starts its terminal instead
        // (`Host::start_terminal`), never a headless session, and never as Marv.
        let agent = marv_web::terminal_agents::for_chat(chat.harness.as_deref());
        if !terminal && chat.harness.as_deref().is_some_and(|h| h != marv_web::claude_code::NAME) {
            return Err(format!("{} runs in this chat's terminal: open the chat to use it.", agent.title));
        }
        let settings = match chat.settings.clone() {
            Some(settings) => settings,
            None if terminal => store::ChatSettings {
                model: String::new(),
                max_steps: 100,
                yolo: false,
                mcp_off: Vec::new(),
                devices_off: Vec::new(),
            },
            None => return Err("This chat hasn't been opened here yet: open it once.".into()),
        };
        // A terminal's agent is checked by its own launch (`terminal_agents`), and Marv's keys don't apply to it.
        let harness = if terminal {
            marv_web::host::Harness::ClaudeCode
        } else {
            marv_web::host::Harness::from_name(chat.harness.as_deref())
        };

        // As `open_chat` checks: a plan (#159) needs its own key or sign-in, never another provider's.
        let model = &settings.model;
        let (zai, chatgpt) = (model.starts_with(marv_web::host::ZAI_PREFIX), model.starts_with(marv_web::host::CHATGPT_PREFIX));
        let kimi = model.starts_with(marv_web::host::KIMI_PREFIX);
        let (zai, kimi, chatgpt) =
            if harness == marv_web::host::Harness::Marv { (zai, kimi, chatgpt) } else { (false, false, false) };
        if (zai && !desktop.plan_allowed("zai"))
            || (kimi && !desktop.plan_allowed("kimi"))
            || (chatgpt && !desktop.plan_allowed("chatgpt"))
        {
            return Err("That plan isn't available in this build.".into());
        }
        if zai && desktop.zai_key().is_none() {
            return Err("Add your Z.ai Coding Plan key first.".into());
        }
        if kimi && desktop.kimi_key().is_none() {
            return Err("Add your Kimi Code plan key first.".into());
        }
        if chatgpt && !desktop.chatgpt().signed_in() {
            return Err("Sign in with ChatGPT first.".into());
        }
        if !terminal && harness != marv_web::host::Harness::Marv && harness.program().is_none() {
            return Err(harness.not_installed());
        }
        let api_key = match desktop.api_key() {
            // Claude Code uses the person's own Claude login, never Marv's keys.
            _ if harness != marv_web::host::Harness::Marv => String::new(),
            Some(key) => key,
            None if zai || kimi || chatgpt || marv_web::host::is_local(model) => String::new(),
            None => return Err("Add an OpenRouter key first, or pick a local model (ollama/…).".into()),
        };
        // Wherever the top folder is now: the chat runs in its own folder (never above home; home only for a Home chat).
        let dir = check_chat_root(&chat.folder).map_err(|e| format!("This chat's folder: {e}"))?;
        // Runtime-kernel §14.1: a chat granted beside another works in its own worktree; it is named, listed and
        // covered by the rules from its folder still.
        let (root, gone) = work_root(&chat).map_err(|e| format!("This chat's folder: {e}"))?;
        let mut chat = chat;
        if let Some(branch) = gone {
            // Its worktree is gone: it works in its folder again, in a new conversation (the old one was saved there).
            end_worktree(&desktop, &chat.id);
            chat.session_id = None;
            let line = format!(
                "This chat's worktree (branch {branch}) is gone, so it works in its folder again, in a new conversation."
            );
            desktop.notices.lock().unwrap_or_else(|e| e.into_inner()).insert(chat.id.clone(), line);
        }
        // Named from its id: the same chat is the same name at every start, so wires find it again (runtime-kernel 2a).
        let mesh_name = marv_kernel::AgentId::new(&chat.id).map(|id| marv_kernel::mesh_name_for(&dir, &id));
        Ok(ChatSpec {
            root,
            // The CLI's own folder (~/.marv): the same memory, MCP servers and saved sessions as `marv`.
            config_dir: marv_dir(),
            api_key,
            zai_api_key: desktop.zai_key(),
            kimi_api_key: desktop.kimi_key(),
            chatgpt_auth: Some(desktop.chatgpt().path),
            model: settings.model,
            max_steps: settings.max_steps.min(500),
            yolo: settings.yolo,
            resume: chat.session_id.clone().map(marv_session::ResumeTarget::Id),
            // The person is at this machine: a dev server's port opens on their own 127.0.0.1.
            processes: true,
            // As the CLI: your skills, memory, MCP servers and instructions, and the folder's own.
            sources: vec![marv_session::sources::Source::User, marv_session::sources::Source::Project],
            // On the mesh like a terminal Marv: other Marvs (desktop chats, Herdr panes) see it and can be wired to it.
            // Its mesh is its resident record's (`chats`), which outlives the session.
            // Not on the mesh yet (phase 3 gives Claude Code chats the mesh's tools).
            mesh: harness == marv_web::host::Harness::Marv,
            mesh_name,
            chat_id: Some(chat.id.clone()),
            // Your mesh rules (the Mesh panel, `marv mesh allow`) apply to desktop chats.
            rules: true,
            mcp_off: settings.mcp_off,
            devices_off: settings.devices_off,
            manage_mcp: true,
            resident: None,
            provider: None,
            grant: None,
            meters: desktop.meters.clone(),
            host_commands: true,
            // #204: each turn's trajectory in ~/.marv/trajectories, as the TUI and `marv -p` write it, unless the
            // config turns them off. Woken and background turns are this same session.
            trajectories: marv_web::host::trajectories_on(&marv_dir()),
            // Runtime-kernel §11.4: its page-outs, in the kernel's log (sizes only).
            kernel_events: desktop.events.clone().map(|log| log as std::sync::Arc<dyn marv_kernel::EventLog>),
            timers: None,
            delegates: None,
            // Picked when the chat was made; every load starts as it (an unknown one fails the load, told on the page).
            role: chat.role.clone(),
            // Started in a team (`teams`): its brief follows the role, as the team was started.
            team: chat.team.as_ref().map(|t| (t.name.clone(), t.roster.clone())),
            team_writes: chat.team.as_ref().map(|t| t.writes.clone()).unwrap_or_default(),
            team_peers: team_peers(&desktop.chats(), &chat),
            // The chat shows a reply's choices as buttons (ChatView's ChoiceCard).
            choices: true,
            // The write paths the person gave it when its team started: its write set, in place of its role's.
            writes: chat.writes.clone(),
            harness,
            agent_model: chat.agent_model.clone(),
            // Phase 4: inside the outer sandbox unless the person turned it off (Settings → Harnesses).
            harness_sandbox: harness != marv_web::host::Harness::Marv && desktop.store.settings().harness_sandbox != Some(false),
        })
    }
}

impl chats::Host for AppHost {
    /// mk-60m.8: the person's `mesh.wake` from Marv's own config file (Settings → wake), as the TUI reads it.
    fn mesh_wake(&self) -> marv_session::config::MeshWake {
        marv_web::host::mesh_wake_in(&marv_dir())
    }
    fn harness(&self, id: &str) -> bool {
        let desktop = self.0.state::<Desktop>();
        desktop.chats().into_iter().any(|c| c.id == id && c.harness.is_some())
    }
    fn spec(&self, id: &str) -> Result<ChatSpec, String> {
        self.build_spec(id, false)
    }

    fn type_into(&self, id: &str, text: &str) -> bool {
        let terminals = self.0.state::<terminals::Terminals>();
        let key = format!("chat:{id}");
        // Pasted (bracketed, so its lines stay one message), then Enter a moment later, once the agent took the paste.
        let harness = self.0.state::<Desktop>().chats().into_iter().find(|c| c.id == id).and_then(|c| c.harness);
        let agent = marv_web::terminal_agents::for_chat(harness.as_deref()).id;
        let typed = terminals::submission(text, agent, cfg!(windows));
        if !terminals.write_key(&key, &typed.paste) {
            return false;
        }
        self.0.state::<std::sync::Arc<delegates::Delegations>>().typed(id);
        let app = self.0.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(typed.enter_after).await;
            app.state::<terminals::Terminals>().write_key(&key, "\r");
        });
        true
    }

    fn start_terminal(&self, id: &str) -> Result<(), String> {
        start_chat_terminal(&self.0, id, 120, 36, None, true).map(|_| ())
    }

    fn terminal_brief(&self, id: &str) -> Option<String> {
        let desktop = self.0.state::<Desktop>();
        let chat = desktop.chats().into_iter().find(|c| c.id == id)?;
        let agent = marv_web::terminal_agents::for_chat(chat.harness.as_deref());
        // Claude Code is told at start (`--append-system-prompt`); Marv has its own team brief.
        if agent.id == marv_web::claude_code::NAME || agent.id == marv_web::terminal_agents::MARV {
            return None;
        }
        let spec = self.terminal_spec(&chat).ok()?;
        marv_web::claude_code::member_brief(&spec, agent.title, "the marv-mesh tools").ok().flatten()
    }

    fn terminal_spec(&self, id: &str) -> Result<ChatSpec, String> {
        self.build_spec(id, true)
    }

    /// A question on the agent's screen now. Its screen, not its output's last 6,000 bytes: a question already answered
    /// stayed in those bytes (Claude Code's folder trust, then a redraw shorter than that), so a team's first task was
    /// held for an answer the person had given, and nothing tried again: the lead sat idle without its task. The raw
    /// tail only for a terminal without a screen.
    fn terminal_question(&self, id: &str) -> Option<String> {
        let terminals = self.0.state::<terminals::Terminals>();
        let key = format!("chat:{id}");
        let shown = terminals.screen(&key).or_else(|| terminals.tail(&key, 6000))?;
        marv_web::terminal_agents::waiting_question(&shown).map(String::from)
    }

    fn interrupt_terminal(&self, id: &str) {
        self.0.state::<terminals::Terminals>().write_key(&format!("chat:{id}"), "\x1b");
    }

    // Every chat on the desktop is its agent's terminal: mail and timers start it and go in there.
    fn terminal_only(&self, id: &str) -> bool {
        self.0.state::<Desktop>().chats().into_iter().any(|c| c.id == id)
    }

    fn grant(
        &self,
        id: &str,
        out: &tokio::sync::mpsc::UnboundedSender<Value>,
    ) -> Option<std::sync::Arc<dyn marv_session::grants::ChatGrant>> {
        let desktop = self.0.state::<Desktop>();
        let grants = desktop.grants.as_ref()?;
        let chat = marv_kernel::AgentId::new(id)?;
        // A pause (spent, expired) is told in the chat in Marv's fixed words, and as an event the page can show.
        let out = out.clone();
        let grant = marv_session::grants::FileChatGrant::new(grants.file.clone(), grants.spend.clone(), chat).on_pause(
            std::sync::Arc::new(move |pause| {
                let _ = out.send(serde_json::json!({"type": "notice", "text": marv_session::grants::pause_text(pause)}));
                let reason = match pause {
                    marv_session::grants::GrantPause::Spent(_) => "spend",
                    marv_session::grants::GrantPause::Expired(_) => "expired",
                };
                let _ = out.send(serde_json::json!({"type": "grant_paused", "reason": reason}));
            }),
        );
        let grant = match desktop.events.clone() {
            Some(log) => grant.events(log),
            None => grant,
        };
        Some(std::sync::Arc::new(grant))
    }

    fn timers(
        &self,
        id: &str,
        grant: &std::sync::Arc<dyn marv_session::grants::ChatGrant>,
    ) -> Option<std::sync::Arc<dyn marv_session::timers::SelfTimers>> {
        let desktop = self.0.state::<Desktop>();
        let chat = marv_kernel::AgentId::new(id)?;
        let timers = marv_session::timers::FileSelfTimers::new(timer_store().ok()?, chat, grant.clone());
        let timers = match desktop.events.clone() {
            Some(log) => timers.events(log),
            None => timers,
        };
        Some(std::sync::Arc::new(timers))
    }

    fn start_mesh(
        &self,
        id: &str,
        folder: &std::path::Path,
        approver: &std::sync::Arc<marv_web::approver::WebApprover>,
        out: &mpsc::UnboundedSender<Value>,
    ) -> Option<marv_session::mesh_process::Mesh> {
        // A Claude Code chat is on the mesh too (the harness plan, phase 3): its tools reach it through
        // `marv_web::harness_mesh`, and the mail it sends is never clean.
        // Named from its id: the same chat is the same name at every start, so wires find it again (runtime-kernel 2a).
        let name = marv_kernel::AgentId::new(id).map(|a| marv_kernel::mesh_name_for(folder, &a));
        // Your mesh rules (the Mesh panel, `marv mesh allow`) apply to desktop chats.
        // What it says on the way (why it's off the mesh) goes to the chat and to the log, for a report: a terminal
        // chat shows no notice, and a chat off the mesh just has no mesh name (seen on Windows, 2026-11-13).
        let (tap, mut said) = mpsc::unbounded_channel::<Value>();
        let started = marv_web::host::start_mesh(folder, name.as_deref(), Some(id.to_string()), true, &marv_dir(), &tap);
        let pass_on = |said: &mut mpsc::UnboundedReceiver<Value>| {
            let mut any = false;
            while let Ok(v) = said.try_recv() {
                if let Some(text) = v["text"].as_str() {
                    applog::warn("mesh", &format!("chat {id}: {text}"));
                    any = true;
                }
                let _ = out.send(v);
            }
            any
        };
        let told = pass_on(&mut said);
        let Some(mut mesh) = started else {
            if !told {
                let runtime = marv_sandbox::runtime_paths(&std::env::vars().collect());
                applog::warn("mesh", &format!("chat {id} isn't on the mesh: no runtime folder Marv may use (tried {runtime:?})"));
            }
            return None;
        };
        // Runtime-kernel coordination: its door says its role (picked when the chat was made) and whether a message
        // wakes it, sleeping or loaded, so `mesh_list` shows both to other Marvs.
        // Only a role that loads here now (a removed or broken role file publishes none: its chat won't load either).
        let role = self.0.state::<Desktop>().chats().into_iter().find(|c| c.id == id).and_then(|c| c.role).filter(|name| {
            let (roles, _) = marv_session::roles::available_roles(folder, &marv_session::paths::home_dir(), &marv_dir());
            marv_session::roles::find_role(&roles, name).is_some()
        });
        mesh.handle.set_role(role);
        mesh.handle.set_wake(Some(marv_session::config::ConfigStore::new(&marv_dir()).mesh_wake().peer()));
        // The Team view's reports: what arrives in its main inbox as a report, by the sender the switchboard stamped.
        mesh.handle.set_report_sink(self.0.state::<Desktop>().reports.sink(id));
        marv_web::host::open_mesh_door(&mut mesh, approver, &tap);
        pass_on(&mut said);
        applog::info("mesh", &format!("chat {id} is on the mesh as {}", mesh.name));
        Some(mesh)
    }

    fn session_saved(&self, id: &str, session: &str) {
        let session = session.to_string();
        let _ = self.0.state::<Desktop>().store.update_chat(id, |c| c.session_id = Some(session));
    }

    fn spawn(&self, task: chats::Task) {
        tauri::async_runtime::spawn(task);
    }

    fn notify(&self, id: &str, event: &Value, shown: bool) {
        let desktop = self.0.state::<Desktop>();
        let title = desktop.store.chats().into_iter().find(|c| c.id == id).map(|c| c.title).unwrap_or_default();
        desktop.notifier.event(&self.0, id, &title, event, shown);
        // A team member's turn ended and none of its team is working now: the team waits for the person.
        if event["type"] == "turn_end" {
            let chats = desktop.store.chats();
            if let Some(chat) = chats.iter().find(|c| c.id == id)
                && let Some(team) = &chat.team
            {
                let members: Vec<&Chat> = chats
                    .iter()
                    .filter(|c| c.folder == chat.folder && c.team.as_ref().is_some_and(|t| t.name == team.name))
                    .collect();
                let busy = desktop.live.busy_ids();
                if members.len() > 1 && members.iter().all(|m| !busy.contains(&m.id)) {
                    let lead = members.iter().find(|m| m.role.as_deref() == Some("coordinator")).unwrap_or(&members[0]);
                    desktop.notifier.team_idle(&self.0, &lead.id, &team.name);
                }
            }
        }
    }

    fn take_notice(&self, id: &str) -> Option<String> {
        self.0.state::<Desktop>().notices.lock().unwrap_or_else(|e| e.into_inner()).remove(id)
    }

    fn private(&self, id: &str) -> bool {
        // No record (deleted meanwhile): private, nothing is served.
        self.0.state::<Desktop>().chats().into_iter().find(|c| c.id == id).is_none_or(|c| c.private)
    }

    fn saved_snapshot(&self, id: &str) -> chats::SnapshotTask {
        // Read in the returned task, never under the chats' lock (as `saved_card`).
        let (app, id) = (self.0.clone(), id.to_string());
        Box::pin(async move {
            let saved = app
                .state::<Desktop>()
                .chats()
                .into_iter()
                .find(|c| c.id == id)
                .and_then(|c| Some((work_root(&c).ok()?.0, c.session_id?)));
            let (dir, session) = saved?;
            let store = marv_session::sessions::SessionStore::new(marv_dir().join("sessions"));
            store.load(&dir, &session).await.as_ref().map(marv_session::chat_read::snapshot_of_saved)
        })
    }

    fn saved_card(&self, id: &str) -> chats::CardTask {
        // Everything that reads a file runs in the returned task, never under the chats' lock (`make_resident`).
        let (app, id) = (self.0.clone(), id.to_string());
        Box::pin(async move {
            let saved = app
                .state::<Desktop>()
                .chats()
                .into_iter()
                .find(|c| c.id == id)
                .and_then(|c| Some((work_root(&c).ok()?.0, c.session_id?)));
            let (dir, session) = saved?;
            let store = marv_session::sessions::SessionStore::new(marv_dir().join("sessions"));
            store.load(&dir, &session).await.as_ref().map(marv_session::sessions::card_of)
        })
    }
}

/// D13: the chats a live mesh rule covers, which keep a door while the app runs (opened this run or not).
fn rule_covered(desktop: &Desktop) -> Vec<(String, PathBuf)> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    // A grant found expired is logged, once, on the same look.
    if let (Some(log), Some(grants)) = (desktop.event_log(), &desktop.grants) {
        marv_session::kernel_events::notice_grant_expiries(&desktop.grant_expiries, &grants.file.snapshot(), now, log);
    }
    let Ok(snapshot) = rule_store().and_then(|s| s.load().map_err(|e| e.to_string())) else { return Vec::new() };
    // Runtime-kernel 3: a rule found expired is logged, once.
    if let Some(log) = desktop.event_log() {
        marv_session::kernel_events::notice_expiries(&desktop.expiries, &snapshot, now, log);
    }
    // Runtime-kernel §19.2: an expired rule's team board goes with it, on the same look.
    if snapshot.rules.iter().any(|r| !r.live_at(now))
        && let Ok(boards) = board_store()
    {
        tauri::async_runtime::spawn(async move {
            let _ = boards.sweep(now).await;
        });
    }
    let live: Vec<&marv_kernel::Policy> = snapshot.rules.iter().filter(|r| r.live_at(now)).collect();
    if live.is_empty() {
        return Vec::new();
    }
    desktop
        .chats()
        .into_iter()
        .filter_map(|chat| {
            let id = marv_kernel::AgentId::new(&chat.id)?;
            let folder = check_chat_root(&chat.folder).ok()?;
            let me = marv_kernel::AgentRef {
                mesh_name: marv_kernel::mesh_name_for(&folder, &id),
                id: Some(id),
                folder: folder.clone(),
                kind: marv_kernel::AgentKind::Chat,
            };
            live.iter().any(|r| r.from.covers(&me) || r.to.covers(&me)).then_some((chat.id, folder))
        })
        .collect()
}

/// How often the rules file is looked at for chats it now covers (a rule made with `marv mesh allow`, one expired).
const RULES_LOOK: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Serialize)]
struct ServerEntry {
    name: String,
    /// "global" (~/.marv/mcp.json, every folder) or "folder" (the folder's .mcp.json, once trusted).
    scope: &'static str,
    /// The config as written (`${VAR}`s unexpanded, so no secrets).
    target: String,
}

/// A role a new chat may start as (`marv_session::roles`), as the new-chat picker lists it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RoleEntry {
    name: String,
    description: String,
    /// "built-in", "personal" or "project".
    source: &'static str,
    /// The tools it's limited to, or None for all (the coordinator: every one but the file tools, in `except`).
    tools: Option<Vec<String>>,
    except: Vec<String>,
    /// The paths it may write under; None: anywhere; empty: nothing.
    writes: Option<Vec<String>>,
    takes_tasks_from: Vec<String>,
}

/// The roles a chat in `folder` may start as (built-in, yours, and the folder's own once you trusted it), with the role
/// files that couldn't be loaded.
#[tauri::command]
fn list_roles(desktop: State<'_, Desktop>, folder: String) -> Result<(Vec<RoleEntry>, Vec<String>), String> {
    let dir = tree::resolve(&desktop.root()?, &folder)?;
    let (roles, problems) = marv_session::roles::available_roles(&dir, &marv_session::paths::home_dir(), &marv_dir());
    let entries = roles
        .into_iter()
        .map(|r| {
            let (tools, except) = match r.tools {
                marv_session::roles::RoleTools::All => (None, Vec::new()),
                marv_session::roles::RoleTools::Only(list) => (Some(list), Vec::new()),
                marv_session::roles::RoleTools::AllBut(list) => (None, list),
            };
            RoleEntry {
                name: r.name,
                description: marv_core::printable(&r.description),
                source: r.source.label(),
                tools,
                except,
                writes: r.writes,
                takes_tasks_from: r.takes_tasks_from,
            }
        })
        .collect();
    Ok((entries, problems.iter().map(|p| marv_core::printable(p)).collect()))
}

/// The MCP servers a chat in `folder` gets, from the config files (whether or not a chat runs them).
#[tauri::command]
fn mcp_servers(desktop: State<'_, Desktop>, folder: String) -> Result<Vec<ServerEntry>, String> {
    let dir = tree::resolve(&desktop.root()?, &folder)?;
    let env: marv_mcp::Env = std::env::vars().collect();
    let include = marv_mcp::config::Include { personal: true, project: true };
    let loaded = marv_mcp::config::load_mcp_config(&dir, &marv_dir(), &env, include);
    Ok(loaded
        .servers
        .into_iter()
        .map(|s| ServerEntry {
            scope: if s.source == marv_mcp::config::ServerSource::Project { "folder" } else { "global" },
            name: s.name,
            target: marv_core::printable(&s.display),
        })
        .collect())
}

/// `/mcp add`'s lookup: an npm package, a URL or a GitHub repo becomes the exact entry to write, shown to the person
/// first (with what was found about it). Several servers in one README: the choices.
#[tauri::command]
async fn mcp_lookup(
    desktop: State<'_, Desktop>,
    source: String,
    name: Option<String>,
    // Credentials the server reads: written as `${VAR}`, filled in by `mcp_install`.
    vars: Option<Vec<String>>,
    headers: Option<std::collections::BTreeMap<String, String>>,
) -> Result<Value, String> {
    use marv_mcp::install::{AddOptions, Resolved, resolve_add};
    let env: marv_session::config::Env = std::env::vars().collect();
    let options = AddOptions {
        source: source.trim().to_string(),
        name: name.filter(|n| !n.trim().is_empty()),
        env: vars.unwrap_or_default(),
        headers: headers.unwrap_or_default(),
        ..AddOptions::default()
    };
    let deps = marv_session::mcp_install::LiveDeps::new(env.clone(), tokio_util::sync::CancellationToken::new());
    let proposal = match resolve_add(&options, &deps).await.map_err(|e| e.to_string())? {
        Resolved::Choices(c) => return Ok(serde_json::json!({ "choices": c.choices })),
        Resolved::Proposal(p) => p,
    };
    if marv_mcp::personal_file::personal_server_names(&marv_dir()).contains(&proposal.name) {
        return Err(format!("You already have a server named \"{}\": give this one another name.", proposal.name));
    }
    let (approval, json) = marv_mcp::install::proposal::approval_for(&proposal, &|v| env.contains_key(v))?;
    let entry: Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    desktop.proposals.lock().unwrap_or_else(|e| e.into_inner()).insert(proposal.name.clone(), entry);
    Ok(serde_json::json!({
        "name": proposal.name,
        "title": approval.title,
        "command": approval.command,
        "text": approval.text,
        "warning": approval.warning,
    }))
}

/// Writes a looked-up server to ~/.marv/mcp.json (every folder's): exactly the entry `mcp_lookup` showed, with its
/// `${VAR}`s filled from `values` (the credentials typed at setup). The file is the user's alone (0600) and hidden
/// from Marv's sandbox.
#[tauri::command]
async fn mcp_install(
    desktop: State<'_, Desktop>,
    name: String,
    values: Option<std::collections::HashMap<String, String>>,
) -> Result<(), String> {
    let mut entry =
        desktop.proposals.lock().unwrap_or_else(|e| e.into_inner()).remove(&name).ok_or("Look the server up first.")?;
    fill(&mut entry, &values.unwrap_or_default());
    marv_mcp::personal_file::add_personal_server(&marv_dir(), &name, entry).await.map_err(|e| e.to_string())
}

/// `${VAR}` → its value, in every string of an entry (only the names given; anything else stays a reference).
fn fill(value: &mut Value, values: &std::collections::HashMap<String, String>) {
    match value {
        Value::String(text) => {
            for (name, v) in values {
                *text = text.replace(&format!("${{{name}}}"), v);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|i| fill(i, values)),
        Value::Object(map) => map.values_mut().for_each(|i| fill(i, values)),
        _ => {}
    }
}

/// Takes one of your servers out of ~/.marv/mcp.json (a folder's are in its .mcp.json).
#[tauri::command]
async fn mcp_remove(name: String) -> Result<(), String> {
    if !marv_mcp::personal_file::personal_server_names(&marv_dir()).contains(&name) {
        return Err(format!("\"{name}\" isn't in ~/.marv/mcp.json: a folder's server is removed from its .mcp.json."));
    }
    marv_mcp::personal_file::remove_personal_server(&marv_dir(), &name).await.map_err(|e| e.to_string())
}

/// Opens a link or folder with the system's own handler: macOS's `open`, Windows' `explorer` (xdg-open isn't there:
/// links and folders didn't open on Windows), elsewhere the freedesktop opener. A Windows link goes to its URL handler
/// as one argument, as the sign-in pages' do (`marv_mcp::oauth::open_browser_with`): `explorer` takes a comma in it
/// (an OAuth scope list) for its own separator and opened Documents.
fn system_open(target: &std::ffi::OsStr) -> Result<(), String> {
    if cfg!(windows) && target.to_str().is_some_and(|t| t.starts_with("https://") || t.starts_with("http://")) {
        return marv_core::process::command("rundll32")
            .arg("url.dll,FileProtocolHandler")
            .arg(target)
            .spawn()
            .map(|_| ())
            .map_err(|e| {
                applog::warn("open", &format!("rundll32 couldn't open {}: {e}", target.to_string_lossy()));
                e.to_string()
            });
    }
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    marv_core::process::command(opener).arg(target).spawn().map(|_| ()).map_err(|e| {
        applog::warn("open", &format!("{opener} couldn't open {}: {e}", target.to_string_lossy()));
        e.to_string()
    })
}

/// An http(s) link in the person's own browser (a setup page for an app's keys).
#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("Only web links open here.".into());
    }
    system_open(std::ffi::OsStr::new(&url))
}

/// Your apps (~/.marv/mcp.json), each with whether it is signed in: the Apps place.
#[tauri::command]
fn apps_status() -> Vec<apps::AppStatus> {
    apps::statuses(&marv_dir(), &marv_session::paths::home_dir())
}

/// Adds an app that signs in (a remote server with OAuth) to ~/.marv/mcp.json. `reuse` names one of your apps whose
/// OAuth client this one takes (its secret stays in the file). Signing in is `app_sign_in`, next.
#[tauri::command]
async fn app_connect(name: String, url: String, oauth: Option<apps::OAuthSetup>, reuse: Option<String>) -> Result<(), String> {
    needs_account()?;
    let dir = marv_dir();
    let reused = reuse.filter(|r| !r.trim().is_empty()).map(|r| apps::oauth_of(&dir, &r)).transpose()?;
    let entry = apps::entry(&url, &oauth.unwrap_or_default(), reused.as_ref())?;
    marv_mcp::personal_file::add_personal_server(&dir, &name, entry).await.map_err(|e| e.to_string())
}

/// Signs in to app `name`: the provider's page opens in the person's browser, and this answers once they come back
/// (with how many tools it offers), or with why not. No chat needed.
#[tauri::command]
async fn app_sign_in(name: String) -> Result<usize, String> {
    needs_account()?;
    let authorize: marv_mcp::oauth::Authorize = std::sync::Arc::new(|url| {
        let _ = open_url(url.to_string());
    });
    apps::sign_in(&marv_dir(), &marv_session::paths::home_dir(), &name, authorize).await
}

#[tauri::command]
async fn app_cancel_sign_in() {
    apps::cancel_sign_in().await
}

#[tauri::command]
async fn app_sign_out(name: String) -> Result<(), String> {
    apps::sign_out(&marv_dir(), &marv_session::paths::home_dir(), &name).await
}

/// Starts or stops dictation with Voxtype (Omarchy's; Super+Ctrl+X does the same): it types into the focused field.
#[tauri::command]
fn dictate() -> Result<(), String> {
    if cfg!(target_os = "macos") {
        return Err(
            "On macOS, use the system's dictation: press the Globe (fn) key twice, or turn it on in System Settings → Keyboard."
                .into(),
        );
    }
    marv_core::process::command("voxtype")
        .args(["record", "toggle"])
        .spawn()
        .map(|_| ())
        .map_err(|_| "Dictation needs Voxtype (on Omarchy: Super+Ctrl+X).".to_string())
}

#[tauri::command]
fn chat_message(desktop: State<'_, Desktop>, id: String, message: Value) -> Result<(), String> {
    let message: ClientMessage = serde_json::from_value(message).map_err(|e| e.to_string())?;
    if let ClientMessage::Configure { model, max_steps, yolo, .. } = &message {
        remember_settings(&desktop, &id, model.as_deref(), *max_steps, *yolo);
    }
    // An agent chat's model: kept for its next starts (a sleeping chat's next load too).
    if let ClientMessage::AgentModel { model } = &message
        && marv_web::protocol::is_agent_model(model)
    {
        let _ = desktop.store.update_chat(&id, |c| c.agent_model = (model != "default").then(|| model.clone()));
    }
    desktop.live.message(&id, message)
}

/// A change the page made to a chat's model, step limit or yolo, kept as its last settings (runtime-kernel 1).
fn remember_settings(desktop: &Desktop, id: &str, model: Option<&str>, max_steps: Option<u32>, yolo: Option<bool>) {
    let _ = desktop.store.update_chat(id, |c| {
        if let Some(settings) = c.settings.as_mut() {
            if let Some(model) = model.filter(|m| is_model_id(m)) {
                settings.model = model.to_string();
            }
            if let Some(steps) = max_steps {
                settings.max_steps = steps.min(500);
            }
            if let Some(yolo) = yolo {
                settings.yolo = yolo;
            }
        }
    });
}

/// The page stopped showing the chat: it keeps running (on the mesh, its turn going on) and keeps what happens.
#[tauri::command]
fn close_chat(desktop: State<'_, Desktop>, id: String) {
    desktop.live.close(&id);
}

/// Ends a running chat: it leaves the mesh (a rule doesn't bring it back this run). Opening it again continues the
/// conversation.
#[tauri::command]
fn stop_chat(desktop: State<'_, Desktop>, id: String) {
    desktop.live.stop(&id);
}

/// The running chats, for the sidebar: busy, and how many approvals wait.
#[tauri::command]
fn live_chats(desktop: State<'_, Desktop>) -> Vec<background::LiveState> {
    desktop.live.states()
}

/// Runtime-kernel §14.4: another chat (or Marv) editing the same files as a chat, as its card and the Mesh panel show
/// it ("also editing here: <chat> (2 files)"). Paths only, never content.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OverlapWith {
    /// The other desktop chat's id, when it is one.
    chat: Option<String>,
    /// Its title (a desktop chat's), else its mesh name.
    name: String,
    /// The paths both wrote in the last 30 minutes, relative to this chat's folder when inside it.
    paths: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WriteOverlapView {
    chat: String,
    title: String,
    with: Vec<OverlapWith>,
}

/// The chats editing the same files as another live chat in the last 30 minutes (runtime-kernel §14.4).
#[tauri::command]
fn write_overlaps(desktop: State<'_, Desktop>) -> Vec<WriteOverlapView> {
    let chats = desktop.chats();
    let chat_of = |id: &str| chats.iter().find(|c| c.id == id);
    // Only this desktop's own chats are named by their titles: another Marv's door can say any chat id, and its
    // entry is shown by its mesh name, never a title borrowed from a chat here.
    let own = desktop.live.resident_ids();
    desktop
        .live
        .write_overlaps()
        .into_iter()
        .map(|(id, overlaps)| {
            let folder = chat_of(&id).map(|c| std::path::PathBuf::from(&c.folder)).unwrap_or_default();
            let with = overlaps
                .into_iter()
                .map(|o| {
                    let chat = match &o.other {
                        marv_kernel::AgentKey::Chat(other) => Some(other.to_string()),
                        marv_kernel::AgentKey::Mesh(_) => None,
                    };
                    let name = chat
                        .as_deref()
                        .filter(|id| own.contains(*id))
                        .and_then(chat_of)
                        .map(|c| c.title.clone())
                        .filter(|t| !t.is_empty());
                    let paths = o
                        .paths
                        .iter()
                        .map(|p| p.strip_prefix(&folder).unwrap_or(p).display().to_string())
                        .map(|p| marv_core::printable(&p))
                        .collect();
                    OverlapWith { chat, name: name.unwrap_or(o.mesh_name), paths }
                })
                .collect();
            WriteOverlapView { title: chat_of(&id).map(|c| c.title.clone()).unwrap_or_default(), chat: id, with }
        })
        .collect()
}

/// Runtime-kernel §20.1 (O1): every agent on this machine for the Activity tab: the desktop's chats and the terminal
/// Marvs, each from its door's `marv/info` (`marv_session::live::collect`), with the pauses this app knows and the
/// teams of the person's rules. For the person's eyes only: the page renders it, nothing offers it to a model.
#[tauri::command]
async fn activity(
    desktop: State<'_, Desktop>,
    terminals: State<'_, terminals::Terminals>,
) -> Result<marv_kernel::activity::View, String> {
    let env: marv_mcp::Env = std::env::vars().collect();
    let Some(dir) = marv_mesh::discovery::mesh_dir(&env) else {
        return Ok(marv_kernel::activity::View::default());
    };
    let mut table = marv_session::live::collect(&dir, background::MAX_LIVE).await.map_err(|e| e.to_string())?;
    // Each chat's terminal agent, while it runs: its row's process when its door doesn't list it.
    let pids: Vec<(String, u32)> = table
        .rows
        .iter()
        .filter_map(|r| {
            let chat = r.chat.clone()?;
            let pid = terminals.pid(&format!("chat:{chat}"))?;
            Some((chat, pid))
        })
        .collect();
    // Off this task: macOS reads a process with `ps`.
    let rows = std::mem::take(&mut table.rows);
    table.rows = tauri::async_runtime::spawn_blocking(move || {
        let mut rows = rows;
        let mut sampler = TERMINAL_SAMPLER.lock().unwrap_or_else(|e| e.into_inner());
        add_terminal_processes(&mut rows, &pids, |pid| sampler.sample(pid, now_ms()).ok());
        sampler.retain(&pids.iter().map(|(_, p)| *p).collect::<Vec<_>>());
        rows
    })
    .await
    .map_err(|e| e.to_string())?;
    let paused: Vec<String> = desktop.live.states().into_iter().filter(|s| s.paused).map(|s| s.id).collect();
    let rules = rule_store().and_then(|s| s.load().map_err(|e| e.to_string())).map(|s| s.rules).unwrap_or_default();
    Ok(marv_kernel::activity::view(table, &paused, &rules, now_ms()))
}

/// The terminal agents' last samples, for their CPU % between two looks at Activity.
static TERMINAL_SAMPLER: std::sync::LazyLock<std::sync::Mutex<marv_kernel::procstats::Sampler>> =
    std::sync::LazyLock::new(Default::default);

/// A chat row whose door doesn't list its terminal agent's process gets it from here, measured with everything it
/// runs (`pids`: chat id, the agent's pid, which the terminal started). A Marv chat lends its door to the Marv in its
/// terminal, whose own `live` lists only the servers and processes it started, never itself, and the desktop keeps no
/// meter for it: its row showed "—" for CPU and memory (seen 2026-11-17). Its tree already holds those servers and
/// processes, so it replaces them rather than counting them twice.
fn add_terminal_processes(
    rows: &mut [marv_kernel::activity::AgentRow],
    pids: &[(String, u32)],
    mut sample: impl FnMut(u32) -> Option<marv_kernel::procstats::Sample>,
) {
    for row in rows {
        let Some(&(_, pid)) = pids.iter().find(|(chat, _)| row.chat.as_deref() == Some(chat.as_str())) else { continue };
        if row.live.processes.iter().any(|p| p.pid == pid) {
            continue;
        }
        let Some(got) = sample(pid) else { continue };
        row.live.processes = vec![marv_kernel::metrics::ChildProcess {
            kind: marv_kernel::metrics::ProcessKind::Process,
            name: "agent".into(),
            pid,
            rss_kb: got.rss_kb,
            cpu_pct: got.cpu_pct,
            alive: got.alive,
        }];
    }
}

/// (chat id, mesh name) for the desktop's chats: the timeline names a chat registered before today's log.
fn chat_names(desktop: &Desktop) -> Vec<(String, String)> {
    desktop
        .chats()
        .into_iter()
        .filter_map(|chat| {
            let agent = marv_kernel::AgentId::new(&chat.id)?;
            let folder = check_chat_root(&chat.folder).ok()?;
            Some((chat.id.clone(), marv_kernel::mesh_name_for(&folder, &agent)))
        })
        .collect()
}

/// Runtime-kernel §20.3 (O3): the kernel's log since `since_ms` as fixed sentences (no message text), newest first,
/// at most 500.
#[tauri::command]
fn timeline(desktop: State<'_, Desktop>, since_ms: u64) -> Vec<marv_kernel::console::Line> {
    let mut lines = marv_session::live::timeline(&marv_dir(), since_ms, now_ms(), &chat_names(&desktop));
    lines.reverse();
    lines.truncate(500);
    lines
}

/// Today's spend by every Marv on this machine (each load's), for "while you were away".
async fn spent_today() -> u64 {
    let env: marv_mcp::Env = std::env::vars().collect();
    let Some(dir) = marv_mesh::discovery::mesh_dir(&env) else { return 0 };
    marv_session::live::collect(&dir, background::MAX_LIVE).await.map_or(0, |a| a.bar.spend_usd_micros)
}

/// The window lost the person (hidden or unfocused): "while you were away" counts from now (§20.3).
#[tauri::command]
async fn away_leave(desktop: State<'_, Desktop>) -> Result<(), String> {
    let now = now_ms();
    desktop.away.leave(now, spent_today().await);
    Ok(())
}

/// The person is back: the card, if they were away 10 minutes or more and anything happened.
#[tauri::command]
async fn away_back(desktop: State<'_, Desktop>) -> Result<Option<marv_kernel::console::Away>, String> {
    let spent = spent_today().await;
    Ok(desktop.away.back(&marv_dir(), now_ms(), spent))
}

/// The notifications shown this run (§20.5), newest first: kept in memory only.
#[tauri::command]
fn notifications(desktop: State<'_, Desktop>) -> Vec<marv_kernel::console::Note> {
    desktop.notifier.history.list()
}

/// Runtime-kernel §20.4 (O4): what each chat may do, by chat, each line with what revokes it (the rules, grants and
/// timers views revoke with their own commands). Show and revoke only: nothing is made or widened here.
/// `utc_offset_minutes`: the page's clock, for a timer's time.
#[tauri::command]
fn permissions(
    desktop: State<'_, Desktop>,
    utc_offset_minutes: i64,
) -> Result<Vec<marv_kernel::permissions::ChatPermissions>, String> {
    let now = now_ms();
    let chats: Vec<(String, String, PathBuf)> = desktop
        .chats()
        .into_iter()
        .filter_map(|c| Some((c.id.clone(), c.title.clone(), check_chat_root(&c.folder).ok()?)))
        .collect();
    let (grants, spend) = match grants_of(&desktop) {
        Ok(g) => {
            let snapshot = g.file.snapshot();
            let spend = snapshot.grants.iter().map(|grant| (grant.id, g.spend.get(grant.id, now))).collect();
            (snapshot.grants.clone(), spend)
        }
        Err(_) => (Vec::new(), Default::default()),
    };
    // The rules as the Mesh panel reads them (this file's rule store): shown, never matched here.
    let rules = rule_store()?.load().map(|s| s.rules).unwrap_or_default();
    Ok(marv_session::live::permissions(&marv_dir(), &chats, &rules, &grants, &spend, now, utc_offset_minutes * 60_000))
}

/// Runtime-kernel §20.7 (O7): About this Marv: the version, the limits in force, where each store lives.
#[tauri::command]
fn about_marv() -> marv_kernel::about::About {
    marv_session::live::about(&marv_dir(), background::MAX_LIVE)
}

/// The person's mesh rules (`~/.marv/mesh/policies.json`, runtime-kernel 2b): the same file the TUI and
/// `marv mesh allow` use.
fn rule_store() -> Result<marv_kernel::PolicyStore, String> {
    marv_kernel::PolicyStore::new(&marv_dir(), &marv_session::paths::home_dir()).map_err(|e| e.to_string())
}

/// How long a rule lasts, as the Mesh panel offers it.
const HOUR_MS: u64 = 60 * 60 * 1000;
/// The backstop for "until Marv quits", should the app not get to revoke it (a crash).
const APP_RULE_BACKSTOP_MS: u64 = 24 * HOUR_MS;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RuleView {
    id: u64,
    label: String,
    /// The folders (or chat ids) it covers, each way.
    from: String,
    to: String,
    mode: String,
    mutual: bool,
    expires_at_ms: Option<u64>,
    /// Made "until Marv quits" in this run.
    until_quit: bool,
    by: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MeshRules {
    rules: Vec<RuleView>,
    /// Why the file isn't used, if it isn't (it then counts as no rules).
    problem: Option<String>,
    /// What the click did beyond the rule, for the person (runtime-kernel §14.1: chats moved to their own worktrees).
    #[serde(skip_serializing_if = "Option::is_none")]
    notice: Option<String>,
}

fn subject_text(subject: &marv_kernel::Subject) -> String {
    match subject {
        marv_kernel::Subject::Folder(scope) => scope.path().display().to_string(),
        marv_kernel::Subject::Agent(id) => format!("chat {id}"),
    }
}

fn mesh_rules_now(desktop: &Desktop) -> Result<MeshRules, String> {
    let store = rule_store()?;
    let session = desktop.session_rules.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let (rules, problem) = match store.load() {
        Ok(snapshot) => (snapshot.rules, None),
        Err(e) => (Vec::new(), Some(e.to_string())),
    };
    let rules = rules
        .into_iter()
        .map(|r| RuleView {
            id: r.id.0,
            label: r.label.clone(),
            from: subject_text(&r.from),
            to: subject_text(&r.to),
            mode: r.mode.to_string(),
            mutual: r.mutual,
            expires_at_ms: r.expires_at_ms,
            until_quit: session.contains(&r.id),
            by: format!("{:?}", r.by).to_lowercase(),
        })
        .collect();
    Ok(MeshRules { rules, problem, notice: None })
}

/// The rules, for the Mesh panel.
#[tauri::command]
fn mesh_rules(desktop: State<'_, Desktop>) -> Result<MeshRules, String> {
    mesh_rules_now(&desktop)
}

/// The person's click in the Mesh panel: chats under `rel` (relative to the top folder) may message each other
/// without asking (send, both ways). `lasts`: "revoked" (until revoked), "hour", or "quit" (until Marv quits). A rule
/// is only ever made here, from that click: never by a chat, a model or a message.
///
/// `keep_working` (the dialog's second checkbox): the chats it covers now also keep working while the person is away
/// ([`grant_chats_under`]). `read` (runtime-kernel §9.2, the "…and may read each other's chats" box, off by default): a
/// second rule, in the read mode, for the same folder and as long. Missing (an older page): neither.
#[tauri::command]
async fn allow_folder(
    desktop: State<'_, Desktop>,
    rel: String,
    lasts: String,
    keep_working: Option<bool>,
    read: Option<bool>,
    worktrees: Option<bool>,
) -> Result<MeshRules, String> {
    let folder = check_root(&desktop.full(&rel)?)?;
    let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let labels = (format!("Chats under {name} message each other"), format!("Chats under {name} may read each other's chats"));
    make_folder_rules(&desktop, &folder, &lasts, read.unwrap_or(false), labels).await?;
    let mut notice = None;
    if keep_working == Some(true) {
        // Runtime-kernel §14.1 (P1): on unless the person unticked it.
        notice = grant_chats_under(&desktop, &folder, &lasts, worktrees.unwrap_or(true), None)
            .await
            .map_err(|e| format!("The rule was made, but no chat was granted to keep working while you're away: {e}"))?;
    }
    desktop.live.sync_rules(&rule_covered(&desktop));
    Ok(MeshRules { notice, ..mesh_rules_now(&desktop)? })
}

/// The person's folder rule for `folder` (chats under it message each other, both ways), and with `read` a second in
/// the read mode: both or neither. Labelled `labels` (send, read). The caller syncs the live chats' rules.
async fn make_folder_rules(
    desktop: &Desktop,
    folder: &std::path::Path,
    lasts: &str,
    read: bool,
    labels: (String, String),
) -> Result<Vec<marv_kernel::PolicyId>, String> {
    make_rules_between(desktop, folder, folder, lasts, read, labels).await
}

/// [`make_folder_rules`] for two folders (a team member's worktree and a teammate's): chats under `from` and chats
/// under `to` message each other, both ways.
async fn make_rules_between(
    desktop: &Desktop,
    from: &std::path::Path,
    to: &std::path::Path,
    lasts: &str,
    read: bool,
    labels: (String, String),
) -> Result<Vec<marv_kernel::PolicyId>, String> {
    let home = marv_session::paths::home_dir();
    let scope = marv_kernel::FolderScope::new(from, &home).map_err(|e| e.to_string())?;
    let to_scope = marv_kernel::FolderScope::new(to, &home).map_err(|e| e.to_string())?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    let expires_at_ms = match lasts {
        "revoked" => None,
        "hour" => Some(now + HOUR_MS),
        "quit" => Some(now + APP_RULE_BACKSTOP_MS),
        other => return Err(format!("\"{other}\" isn't how long a rule lasts")),
    };
    let rule = marv_kernel::NewRule {
        from: marv_kernel::Subject::Folder(scope),
        to: marv_kernel::Subject::Folder(to_scope),
        mode: marv_kernel::Mode::Send,
        mutual: true,
        expires_at_ms,
        by: marv_kernel::RuleBy::Desktop,
        created_at_ms: now,
        label: labels.0,
    };
    let reads = read.then(|| marv_kernel::NewRule { mode: marv_kernel::Mode::Read, label: labels.1, ..rule.clone() });
    let store = rule_store()?;
    let mut made = Vec::new();
    for rule in std::iter::once(rule).chain(reads) {
        match store.add(rule).await {
            Ok(added) => made.push(added),
            Err(e) => {
                // Both or neither: the send rule made a moment ago goes again (never logged, never used).
                for (id, _) in made {
                    let _ = store.revoke(id).await;
                }
                return Err(e.to_string());
            }
        }
    }
    let mut ids = Vec::new();
    for (id, snapshot) in made {
        if let Some(log) = desktop.event_log() {
            marv_session::kernel_events::log_created(log, id, &snapshot);
        }
        if lasts == "quit" {
            desktop.session_rules.lock().unwrap_or_else(|e| e.into_inner()).push(id);
        }
        ids.push(id);
    }
    Ok(ids)
}

/// Revokes a rule: what it wired stops at the next message, in every running Marv.
#[tauri::command]
async fn revoke_rule(desktop: State<'_, Desktop>, id: u64) -> Result<MeshRules, String> {
    revoke_one(&desktop, id).await?;
    desktop.live.sync_rules(&rule_covered(&desktop));
    mesh_rules_now(&desktop)
}

/// Revokes rule `id` (logged, dropped from this run's list); the caller syncs the live chats.
async fn revoke_one(desktop: &Desktop, id: u64) -> Result<(), String> {
    rule_store()?.revoke(marv_kernel::PolicyId(id)).await.map_err(|e| e.to_string())?;
    if let Some(log) = desktop.event_log() {
        marv_session::kernel_events::log_revoked(log, marv_kernel::PolicyId(id));
    }
    desktop.session_rules.lock().unwrap_or_else(|e| e.into_inner()).retain(|r| r.0 != id);
    Ok(())
}

/// A team a folder may start (the New team dialog's cards): built-in, yours, the folder's own once trusted.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TeamEntry {
    name: String,
    title: String,
    description: String,
    /// "built-in", "personal" or "project".
    source: &'static str,
    members: Vec<TeamMemberEntry>,
    worktrees: bool,
    read_chats: bool,
    /// Whether it has a shared brief (its file's body).
    brief: bool,
    /// The brief itself (printable, at most 1000 characters): "Save as team file" shows it before copying a
    /// personal team's brief into the project.
    brief_text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TeamMemberEntry {
    role: String,
    model: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TeamList {
    teams: Vec<TeamEntry>,
    /// Team files that couldn't be loaded or used (a member's role missing).
    problems: Vec<String>,
    /// The teams started in this folder.
    here: Vec<teams::TeamHere>,
    /// The folder has a `.marv/team.md` (loaded or not): "Save as team file" asks before replacing it.
    has_file: bool,
    /// `mesh.wake` now ("on", "off", "rules"): the dialog says what Start team changes before the click.
    mesh_wake: &'static str,
    /// The roles that have a file in the folder's `.marv/roles` (Save leaves those alone).
    role_files: Vec<String>,
}

/// The teams the New team dialog offers in `folder`, and the teams already started there.
#[tauri::command]
fn list_teams(desktop: State<'_, Desktop>, folder: String) -> Result<TeamList, String> {
    let dir = tree::resolve(&desktop.root()?, &folder)?;
    let (found, problems, _) = marv_session::teams::available_teams(&dir, &marv_session::paths::home_dir(), &marv_dir());
    let entries = found
        .into_iter()
        .map(|t| TeamEntry {
            name: t.name,
            title: marv_core::printable(&t.title),
            description: marv_core::printable(&t.description),
            source: t.source.label(),
            members: t.members.into_iter().map(|m| TeamMemberEntry { role: m.role, model: m.model }).collect(),
            worktrees: t.worktrees,
            read_chats: t.read_chats,
            brief: !t.body.is_empty(),
            brief_text: marv_core::printable(&t.body.chars().take(1000).collect::<String>()),
        })
        .collect();
    Ok(TeamList {
        teams: entries,
        problems: problems.iter().map(|p| marv_core::printable(p)).collect(),
        here: teams_in(&desktop, &dir),
        has_file: teams::present(&dir, ".marv/team.md"),
        role_files: role_files(&dir),
        mesh_wake: match marv_config::read(&marv_dir()).mesh_wake {
            Some(true) => "on",
            Some(false) => "off",
            None => "rules",
        },
    })
}

/// The roles with a file in `dir`'s `.marv/roles` (by file name; never through a symlinked folder).
fn role_files(dir: &std::path::Path) -> Vec<String> {
    if teams::check_marv_dirs(dir).is_err() {
        return Vec::new();
    }
    let mut names: Vec<String> = std::fs::read_dir(marv_session::roles::roles_dir(dir))
        .map(|entries| {
            entries.flatten().filter_map(|e| e.file_name().to_string_lossy().strip_suffix(".md").map(str::to_string)).collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// The teams started in `dir` (its chats' records and its rules' labels).
fn teams_in(desktop: &Desktop, dir: &std::path::Path) -> Vec<teams::TeamHere> {
    let Ok(dir) = marv_session::paths::real_folder(dir) else { return Vec::new() };
    let now = now_ms();
    let worktrees: Vec<(PathBuf, PathBuf)> = desktop
        .chats()
        .into_iter()
        .filter_map(|c| {
            let wt = c.worktree.as_ref()?;
            Some((check_root(&wt.dir).ok()?, marv_session::paths::real_folder(c.folder.as_ref()).ok()?))
        })
        .collect();
    let rules: Vec<(u64, String, PathBuf)> = rule_store()
        .and_then(|s| s.load().map_err(|e| e.to_string()))
        .map(|s| s.rules)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.live_at(now))
        .filter_map(|r| match &r.from {
            marv_kernel::Subject::Folder(scope) => Some((r.id.0, r.label.clone(), scope.path().to_path_buf())),
            marv_kernel::Subject::Agent(_) => None,
        })
        // A rule made for a member's worktree belongs to the team of the chat that works there.
        .map(|(id, label, path)| (id, label, teams::rule_home(&path, &worktrees)))
        .collect();
    let chats: Vec<(String, PathBuf, Option<String>)> = desktop
        .chats()
        .into_iter()
        .filter_map(|c| Some((c.id.clone(), marv_session::paths::real_folder(c.folder.as_ref()).ok()?, c.team.map(|t| t.name))))
        .collect();
    teams::teams_here(&dir, &rules, &chats)
}

/// The desktop as a team start's [`teams::TeamHost`].
struct AppTeams<'a> {
    desktop: &'a Desktop,
    /// The folder as chats record it (the top folder joined, not canonical): the sidebar lists by it.
    folder: String,
}

impl teams::TeamHost for AppTeams<'_> {
    fn roles(&self, folder: &std::path::Path) -> Vec<marv_session::roles::Role> {
        marv_session::roles::available_roles(folder, &marv_session::paths::home_dir(), &marv_dir()).0
    }

    fn existing_teams(&self, folder: &std::path::Path) -> Vec<String> {
        teams_in(self.desktop, folder).into_iter().map(|t| t.name).collect()
    }

    fn other_teams(&self, folder: &std::path::Path) -> Vec<(String, Vec<u64>)> {
        teams_in(self.desktop, folder).into_iter().map(|t| (t.name, t.rules)).collect()
    }

    async fn make_rules(&self, folder: &std::path::Path, name: &str, read: bool) -> Result<Vec<u64>, String> {
        let labels = (teams::rule_label(name), teams::read_label(name));
        let ids = make_folder_rules(self.desktop, folder, "revoked", read, labels).await?;
        self.desktop.live.sync_rules(&rule_covered(self.desktop));
        Ok(ids.into_iter().map(|id| id.0).collect())
    }

    async fn cover_worktrees(
        &self,
        folder: &std::path::Path,
        name: &str,
        read: bool,
        chats: &[Chat],
    ) -> Result<Vec<u64>, String> {
        // The worktrees these chats were given (their records now, after the worktree step), checked like any folder.
        let ids: Vec<&str> = chats.iter().map(|c| c.id.as_str()).collect();
        let trees: Vec<PathBuf> = self
            .desktop
            .chats()
            .into_iter()
            .filter(|c| ids.contains(&c.id.as_str()))
            .filter_map(|c| c.worktree.and_then(|wt| check_root(&wt.dir).ok()))
            .collect();
        let labels = (teams::rule_label(name), teams::read_label(name));
        let mut made: Vec<u64> = Vec::new();
        for (from, to) in teams::worktree_pairs(folder, &trees) {
            match make_rules_between(self.desktop, &from, &to, "revoked", read, labels.clone()).await {
                Ok(new) => made.extend(new.into_iter().map(|id| id.0)),
                Err(e) => {
                    // All or none: what this made goes again.
                    let _ = self.revoke_rules(&made).await;
                    return Err(e);
                }
            }
        }
        self.desktop.live.sync_rules(&rule_covered(self.desktop));
        Ok(made)
    }

    async fn revoke_rules(&self, ids: &[u64]) -> Result<(), String> {
        let mut failed = Vec::new();
        for id in ids {
            if let Err(e) = revoke_one(self.desktop, *id).await {
                failed.push(e);
            }
        }
        self.desktop.live.sync_rules(&rule_covered(self.desktop));
        if failed.is_empty() { Ok(()) } else { Err(failed.join("; ")) }
    }

    fn ensure_wake(&self) -> Result<Option<String>, String> {
        let config = marv_config::read(&marv_dir());
        if let Some(problem) = config.problem {
            return Err(problem);
        }
        // mesh.wake is one setting for every chat (no per-chat wake): "never" would leave the team deaf.
        if config.mesh_wake != Some(false) {
            return Ok(None);
        }
        let patch = marv_config::ConfigPatch { mesh_wake: Some("rules".into()), ..marv_config::ConfigPatch::default() };
        marv_config::save(&marv_dir(), &patch)?;
        Ok(Some(
            "Wake on mail was off, so a teammate's message couldn't start a turn: it's now \"a rule's wires\" for every chat (Teams → Mesh changes it back)."
                .into(),
        ))
    }

    fn restore_wake(&self) -> Result<(), String> {
        let patch = marv_config::ConfigPatch { mesh_wake: Some("off".into()), ..marv_config::ConfigPatch::default() };
        marv_config::save(&marv_dir(), &patch).map(drop)
    }

    fn small_model(&self) -> Option<String> {
        marv_config::read(&marv_dir()).small_model
    }

    fn create_chat(&self, _folder: &std::path::Path, m: &teams::NewMember) -> Result<Chat, String> {
        let (model, max_steps, yolo) = (m.model.as_str(), m.max_steps, m.yolo);
        if !is_model_id(model) {
            return Err(format!("\"{}\" isn't a model id.", marv_core::printable(model)));
        }
        let mcp_off = self.desktop.store.settings().active_space().map(|s| s.apps_off.clone()).unwrap_or_default();
        // Its settings now, so a teammate's message can wake it before the person ever opens it (runtime-kernel 1).
        let settings = store::ChatSettings {
            model: model.to_string(),
            max_steps: max_steps.min(500),
            yolo,
            mcp_off,
            devices_off: Vec::new(),
        };
        // Phase 3: a member Claude Code runs (only that name, only if installed); its role and team brief go to it at start.
        if let Some(name) = &m.harness {
            check_harness(name)?;
        }
        let chat = new_chat(
            self.desktop,
            self.folder.clone(),
            &m.title,
            Some(m.role.clone()),
            Some(m.team.clone()),
            Some(settings),
            m.writes.clone(),
        )?;
        if m.harness.is_some() {
            self.desktop.store.update_chat(&chat.id, |c| c.harness = m.harness.clone())?;
        }
        Ok(Chat { harness: m.harness.clone(), ..chat })
    }

    async fn worktrees(&self, chats: &[Chat]) -> Result<Option<String>, String> {
        let listed: Vec<(Chat, PathBuf)> = chats.iter().filter_map(|c| Some((c.clone(), check_root(&c.folder).ok()?))).collect();
        let loaded = vec![false; listed.len()];
        let prepared = prepare_worktrees(&listed, &loaded, &[], true, &marv_dir().join("worktrees")).await;
        for (id, _, wt) in &prepared.made {
            self.desktop.store.update_chat(id, |c| c.worktree = Some(wt.clone()))?;
        }
        let mut notes = Vec::new();
        if !prepared.moved.is_empty() {
            notes.push(format!(
                "Each in its own worktree: {}. Merge each branch when you're happy with it.",
                prepared.moved.join(", ")
            ));
        }
        if !prepared.failed.is_empty() {
            notes.push(format!("Couldn't make a worktree, so working in the folder: {}.", prepared.failed.join(", ")));
        }
        Ok((!notes.is_empty()).then(|| notes.join(" ")))
    }

    async fn grant(&self, chats: &[Chat], worktrees: bool) -> Result<Option<String>, String> {
        let folder = check_root(&self.folder)?;
        let ids: Vec<String> = chats.iter().map(|c| c.id.clone()).collect();
        grant_chats_under(self.desktop, &folder, "revoked", worktrees, Some(&ids)).await
    }
}

/// Said when a feature that needs a (free) Marv account is used signed out; the page shows its sign-up card first.
const ACCOUNT_NEEDED: &str = "This needs a free Marv account: sign in under Settings › Account (Google, GitHub or your email).";

/// Teams and Apps need a Marv account, free or paid: signed in on this computer (plan.json holds a sign-in). Checked
/// here as well as on the page, so no other way in skips it.
fn needs_account() -> Result<(), String> {
    needs_account_in(&marv_dir())
}

fn needs_account_in(dir: &std::path::Path) -> Result<(), String> {
    plan::read_plan(dir).map(|_| ()).ok_or_else(|| ACCOUNT_NEEDED.to_string())
}

/// "Start team" (the New team dialog, the person's click): the folder rule(s), wake on a rule's wire, each chat with its
/// role and model (coordinator first), worktrees or grants as ticked; each step's result (`teams::start`).
#[tauri::command]
async fn start_team(
    app: AppHandle,
    desktop: State<'_, Desktop>,
    folder: String,
    spec: teams::TeamSpec,
) -> Result<teams::Started, String> {
    needs_account()?;
    let full = desktop.full(&folder)?;
    let dir = check_root(&full)?;
    let host = AppTeams { desktop: &desktop, folder: full };
    let mut started = teams::start(&host, &dir, &spec).await?;
    // Every member's agent starts in its terminal now, ready for its tasks: inside Marv's sandbox, since a team works
    // unattended (the person opening a member's chat sees its running terminal).
    for chat in &started.chats {
        let (ok, detail) = match start_chat_terminal(&app, &chat.id, 120, 36, None, true) {
            Ok(_) => (true, "started in its terminal".to_string()),
            Err(e) => (false, e),
        };
        started.steps.push(teams::Step { what: format!("Terminal {}", chat.title.trim()), ok, detail });
    }
    Ok(started)
}

/// "End team": its rules are revoked; its chats stay as normal chats (with their roles), out of the team.
#[tauri::command]
async fn end_team(desktop: State<'_, Desktop>, folder: String, name: String) -> Result<MeshRules, String> {
    let dir = tree::resolve(&desktop.root()?, &folder)?;
    let here = teams_in(&desktop, &dir);
    let team = here.iter().find(|t| t.name == name).ok_or("That team is gone.")?;
    let mut failed = Vec::new();
    for id in &team.rules {
        if let Err(e) = revoke_one(&desktop, *id).await {
            failed.push(e);
        }
    }
    // Their keep-working grants go with the team: a chat that's no longer in it doesn't work unattended.
    if let Some(grants) = &desktop.grants {
        let now = now_ms();
        let theirs: Vec<(u64, String)> = grants
            .file
            .snapshot()
            .grants
            .iter()
            .filter(|g| g.live_at(now) && team.chats.iter().any(|c| *c == g.chat.to_string()))
            .map(|g| (g.id.0, g.chat.to_string()))
            .collect();
        for (id, chat) in theirs {
            if let Err(e) = revoke_grant_of(&desktop, id, Some(&chat)).await {
                failed.push(e);
            }
        }
    }
    for id in &team.chats {
        desktop.store.update_chat(id, |c| c.team = None)?;
    }
    desktop.live.sync_rules(&rule_covered(&desktop));
    if !failed.is_empty() {
        return Err(format!("Some of the team's rules or grants couldn't be revoked: {}", failed.join("; ")));
    }
    mesh_rules_now(&desktop)
}

/// What "Save as team file" did.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SavedTeam {
    /// Project-relative paths written.
    written: Vec<String>,
    /// Role files already there, left as they are.
    kept: Vec<String>,
    /// `.marv/team.md` is there and `replace` wasn't given: nothing was written (the page asks, then sends `replace`).
    exists: bool,
    /// The written files joined the project's trust (it was trusted and nothing else changed).
    trusted: bool,
}

/// "Save as team file" (the person's click, after a confirm): `.marv/team.md`, and a `.marv/roles/<role>.md` for each
/// member whose role has no file there yet, with the write paths the person typed. A role file there is never touched;
/// `.marv/team.md` is replaced only with `replace`.
#[tauri::command]
async fn save_team_file(
    desktop: State<'_, Desktop>,
    folder: String,
    spec: teams::TeamSpec,
    replace: bool,
) -> Result<SavedTeam, String> {
    let dir = check_root(&desktop.full(&folder)?)?;
    let (home, config_dir) = (marv_session::paths::home_dir(), marv_dir());
    teams::check_marv_dirs(&dir)?;
    if teams::present(&dir, ".marv/team.md") && !replace {
        return Ok(SavedTeam { written: Vec::new(), kept: Vec::new(), exists: true, trusted: false });
    }
    let (found, _, roles) = marv_session::teams::available_teams(&dir, &home, &config_dir);
    // The same checks as a start: a valid name (it goes into YAML), 1 to 8 members, known roles, distinct chat names.
    teams::check(&spec, &roles)?;
    // The team it came from keeps its brief.
    let body = marv_session::teams::find_team(&found, &spec.name).map(|t| t.body.clone()).unwrap_or_default();
    let existing = role_files(&dir);
    let (files, kept) = teams::files_to_save(&spec, &roles, &body, &existing)?;
    let before = marv_session::project_trust::before_write(&dir, &home, &config_dir);
    let mut written = Vec::new();
    for (rel, text) in &files {
        // Never through a symlink, and never over a role file (only team.md is replaced, after the person's confirm).
        if let Err(e) = teams::safe_write(&dir, rel, text, replace && rel == ".marv/team.md") {
            let done = if written.is_empty() {
                "Nothing was written.".to_string()
            } else {
                format!("Written before it: {}.", written.join(", "))
            };
            return Err(format!("{e} {done}"));
        }
        written.push(rel.clone());
    }
    let trusted =
        marv_session::project_trust::trust_after_write(&dir, &home, &config_dir, &before, &files).await.unwrap_or(false);
    Ok(SavedTeam { written, kept, exists: false, trusted })
}

/// A team member's latest report-back, for the Team view's roster.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MemberReport {
    chat: String,
    /// "<kind>: <what>" (done, didn't act, failed, waiting for its user).
    text: String,
}

/// The latest report-back (`marv_session::task_report`) each of chats `ids` sent the team's lead `lead` this run, as
/// the lead's switchboard took it in (its stamped sender, never a claim in someone's text): for the person's eyes (the
/// Team view), never a model's.
#[tauri::command]
fn team_reports(desktop: State<'_, Desktop>, lead: String, ids: Vec<String>) -> Vec<MemberReport> {
    let members: Vec<(String, String)> = desktop
        .chats()
        .iter()
        .filter(|c| ids.contains(&c.id))
        .filter_map(|c| {
            let agent = marv_kernel::AgentId::new(&c.id)?;
            let folder = check_root(&c.folder).ok()?;
            Some((c.id.clone(), marv_kernel::mesh_name_for(&folder, &agent)))
        })
        .collect();
    desktop.reports.latest(&lead, &members).into_iter().map(|(chat, text)| MemberReport { chat, text }).collect()
}

/// The person's team boards (runtime-kernel §19): `~/.marv/mesh/boards/<rule>.json`, one per rule.
fn board_store() -> Result<marv_kernel::BoardStore, String> {
    marv_kernel::BoardStore::new(&marv_dir(), &marv_session::paths::home_dir()).map_err(|e| e.to_string())
}

/// One board entry, as the Mesh panel's Board tab shows it.
#[derive(Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct BoardEntryView {
    /// Where it is kept: (rule, entry id). More than one when the person's post went to several boards of a team.
    refs: Vec<(u64, u64)>,
    /// The chat's mesh name, or empty for you.
    by: String,
    kind: String,
    text: String,
    paths: Vec<String>,
    at_ms: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BoardView {
    rules: Vec<u64>,
    entries: Vec<BoardEntryView>,
    /// Why a board can't be read, if one can't.
    problem: Option<String>,
}

/// The entries of several boards as one, oldest first. Your own post to a team's boards is one entry kept in each
/// (same time, kind and text): shown once, with every place it is kept.
fn merge_boards(boards: Vec<(u64, Vec<marv_kernel::BoardEntry>)>) -> Vec<BoardEntryView> {
    let mut out: Vec<BoardEntryView> = Vec::new();
    for (rule, entries) in boards {
        for e in entries {
            let person = matches!(e.by, marv_kernel::BoardBy::Person);
            let kind: String = e.kind.word().into();
            if person
                && let Some(same) =
                    out.iter_mut().find(|o| o.by.is_empty() && o.at_ms == e.at_ms && o.kind == kind && o.text == e.text)
            {
                same.refs.push((rule, e.id));
                continue;
            }
            out.push(BoardEntryView {
                refs: vec![(rule, e.id)],
                by: match e.by {
                    marv_kernel::BoardBy::Chat(name) => name,
                    marv_kernel::BoardBy::Person => String::new(),
                },
                kind,
                text: e.text,
                paths: e.paths,
                at_ms: e.at_ms,
            });
        }
    }
    out.sort_by_key(|e| e.at_ms);
    out
}

fn board_now(rules: &[u64]) -> Result<BoardView, String> {
    let store = board_store()?;
    let now = now_ms();
    let mut boards = Vec::new();
    let mut problem = None;
    for rule in rules {
        match store.read(marv_kernel::PolicyId(*rule), now) {
            Ok(entries) => boards.push((*rule, entries)),
            Err(e) => problem = Some(e.to_string()),
        }
    }
    Ok(BoardView { rules: rules.to_vec(), entries: merge_boards(boards), problem })
}

/// The team boards of `rules` as one (a team in worktrees has one board per pair of its folders: the Teams screen
/// reads them all), for the Mesh panel's Board tab.
#[tauri::command]
fn board(rules: Vec<u64>) -> Result<BoardView, String> {
    board_now(&rules)
}

/// The person's post (as "you") to each of `rules`' boards, so every chat under any of them reads it: logged with its
/// size, never its text. Refused only when no board took it.
#[tauri::command]
async fn board_post(desktop: State<'_, Desktop>, rules: Vec<u64>, kind: String, text: String) -> Result<BoardView, String> {
    let kind = kind.parse::<marv_kernel::BoardKind>()?;
    let store = board_store()?;
    // One time for all of them: the merged view shows the post once.
    let now = now_ms();
    let mut failed = None;
    let mut posted = 0;
    for rule in &rules {
        let entry = marv_kernel::NewEntry { by: marv_kernel::BoardBy::Person, kind, text: text.clone(), paths: Vec::new() };
        match store.post(marv_kernel::PolicyId(*rule), entry, now).await {
            Ok(marv_kernel::Posted { entry: kept, .. }) => {
                posted += 1;
                if let Some(log) = desktop.event_log() {
                    let event = marv_kernel::KernelEvent::BoardPost {
                        rule: marv_kernel::PolicyId(*rule),
                        by: None,
                        kind: kept.kind,
                        size: kept.text.len(),
                        paths_count: kept.paths.len(),
                    };
                    log.append(now, event);
                }
            }
            Err(e) => failed = Some(e.to_string()),
        }
    }
    match failed {
        Some(e) if posted == 0 => Err(e),
        _ => board_now(&rules),
    }
}

/// The person's Clear: one entry (`entry`: where it is kept, [`BoardEntryView::refs`]), or every board of `rules`. Logged
/// with the count.
#[tauri::command]
async fn board_clear(desktop: State<'_, Desktop>, rules: Vec<u64>, entry: Option<Vec<(u64, u64)>>) -> Result<BoardView, String> {
    let store = board_store()?;
    let targets: Vec<(u64, marv_kernel::Clear)> = match entry {
        // Only boards of these rules.
        Some(refs) => {
            refs.into_iter().filter(|(r, _)| rules.contains(r)).map(|(r, id)| (r, marv_kernel::Clear::Entry(id, None))).collect()
        }
        None => rules.iter().map(|r| (*r, marv_kernel::Clear::All)).collect(),
    };
    for (rule, what) in targets {
        let count = store.clear(marv_kernel::PolicyId(rule), what, now_ms()).await.map_err(|e| e.to_string())?;
        if count > 0
            && let Some(log) = desktop.event_log()
        {
            log.append(now_ms(), marv_kernel::KernelEvent::BoardCleared { rule: marv_kernel::PolicyId(rule), by: None, count });
        }
    }
    board_now(&rules)
}

/// A chat's grant, as the Background list and the chat's header show it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GrantView {
    id: u64,
    chat: String,
    /// The chat's title (empty if the chat is gone).
    title: String,
    folder: String,
    expires_at_ms: Option<u64>,
    /// Made "until Marv quits" in this run.
    until_quit: bool,
    spent_usd: f64,
    cap_usd: f64,
    spent_tokens: u64,
    cap_tokens: u64,
    /// When it last ran a call (this run), if it has.
    last_run_ms: Option<u64>,
    /// Expired, or at its cap for today: the chat asks for everything again.
    paused: bool,
    /// The person (or its deadline) paused the chat itself (runtime-kernel §15.3): no wakes or timers until resumed.
    held: bool,
    /// The chat's timers (runtime-kernel §15.4): the person's and its own `wake_me` ones.
    timers: Vec<TimerView>,
}

/// A timer, as the Background list shows it (the person's own words for theirs; a chat's note is never shown as
/// theirs).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TimerView {
    id: u64,
    /// "person" or "chat".
    by: &'static str,
    at_ms: u64,
    /// Minutes between fires; none: once.
    every_minutes: Option<u64>,
    /// A daily timer's local time and zone ("09:00 Europe/Berlin").
    daily_at: Option<String>,
    /// The person's words for theirs; empty for a chat's own.
    note: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GrantList {
    grants: Vec<GrantView>,
    /// Why the grants file isn't used, if it isn't (it then counts as no grants).
    problem: Option<String>,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn grants_of(desktop: &Desktop) -> Result<&Grants, String> {
    desktop.grants.as_ref().ok_or_else(|| "Grants need your config folder to be an absolute path.".to_string())
}

fn grant_list(desktop: &Desktop) -> Result<GrantList, String> {
    let grants = grants_of(desktop)?;
    let snapshot = grants.file.snapshot();
    let session = desktop.session_grants.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let chats = desktop.chats();
    let now = now_ms();
    let held: std::collections::HashSet<String> = desktop.live.states().into_iter().filter(|s| s.paused).map(|s| s.id).collect();
    let timers = timer_store().and_then(|s| s.load().map_err(|e| e.to_string())).unwrap_or_default();
    let views = snapshot
        .grants
        .iter()
        .map(|g| {
            let spent = grants.spend.get(g.id, now);
            GrantView {
                id: g.id.0,
                chat: g.chat.to_string(),
                title: chats.iter().find(|c| c.id == g.chat.as_str()).map(|c| c.title.clone()).unwrap_or_default(),
                folder: g.folder.path().display().to_string(),
                expires_at_ms: g.expires_at_ms,
                until_quit: session.contains(&g.id),
                spent_usd: spent.usd_micros as f64 / 1e6,
                cap_usd: g.cap.usd_micros as f64 / 1e6,
                spent_tokens: spent.tokens,
                cap_tokens: g.cap.tokens,
                last_run_ms: grants.spend.last_used(g.id),
                paused: !g.live_at(now) || spent.over(&g.cap, now),
                held: held.contains(g.chat.as_str()),
                timers: timers
                    .for_chat(&g.chat)
                    .map(|t| TimerView {
                        id: t.id.0,
                        by: match t.by {
                            marv_kernel::TimerBy::Person => "person",
                            marv_kernel::TimerBy::Chat => "chat",
                        },
                        at_ms: t.at_ms,
                        every_minutes: match &t.repeat {
                            marv_kernel::Repeat::Once => None,
                            marv_kernel::Repeat::Every { every_ms } => Some(every_ms / 60_000),
                            marv_kernel::Repeat::Daily { .. } => Some(24 * 60),
                        },
                        daily_at: match &t.repeat {
                            marv_kernel::Repeat::Daily { minute_of_day, tz } => {
                                Some(format!("{:02}:{:02} {tz}", minute_of_day / 60, minute_of_day % 60))
                            }
                            _ => None,
                        },
                        note: if t.by == marv_kernel::TimerBy::Person { t.note.clone() } else { String::new() },
                    })
                    .collect(),
            }
        })
        .collect();
    Ok(GrantList { grants: views, problem: grants.file.problem() })
}

/// The grants, for the Background list.
#[tauri::command]
fn chat_grants(desktop: State<'_, Desktop>) -> Result<GrantList, String> {
    grant_list(&desktop)
}

/// The person's click in the rule dialog ("…and keep working while I'm away"): one grant for each chat the folder's
/// rule covers now, lasting as the rule does. Later chats in the folder ask for their own. A grant is only ever made
/// here, from that click: never by a chat, a model, a message, a project or a flag.
async fn grant_chats_under(
    desktop: &Desktop,
    folder: &std::path::Path,
    lasts: &str,
    worktrees: bool,
    only: Option<&[String]>,
) -> Result<Option<String>, String> {
    let grants = grants_of(desktop)?;
    let home = marv_session::paths::home_dir();
    let now = now_ms();
    let expires_at_ms = match lasts {
        "revoked" => None,
        "hour" => Some(now + HOUR_MS),
        "quit" => Some(now + APP_RULE_BACKSTOP_MS),
        other => return Err(format!("\"{other}\" isn't how long a grant lasts")),
    };
    let scope = marv_kernel::FolderScope::new(folder, &home).map_err(|e| e.to_string())?;
    let store = grants.file.store().clone();
    // All or none: a grant that fails takes back the ones this click made (best effort; what can't be taken back is
    // said).
    let mut made: Vec<marv_kernel::GrantId> = Vec::new();
    let result = grant_each(desktop, &store, &scope, &home, expires_at_ms, now, lasts, worktrees, only, &mut made).await;
    if let Err(e) = &result {
        let e = e.clone();
        let mut left = Vec::new();
        for id in made {
            match store.revoke(id).await {
                Ok(_) => {
                    if let Some(log) = desktop.event_log() {
                        marv_session::kernel_events::log_grant_revoked(log, id);
                    }
                    desktop.session_grants.lock().unwrap_or_else(|e| e.into_inner()).retain(|g| *g != id);
                }
                Err(_) => left.push(id.to_string()),
            }
        }
        return Err(if left.is_empty() { e } else { format!("{e} (still granted, revoke by hand: {})", left.join(", ")) });
    }
    result
}

/// Where a chat being granted works (runtime-kernel §14.1, decision P1).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Placement {
    /// In its folder, as today: alone there, the person turned worktrees off, or it already has one.
    Folder,
    /// In a new worktree of its own: it shares its folder with another granted chat and has no conversation yet.
    Worktree,
    /// It shares its folder with another granted chat, but its conversation is saved in the folder, or its session is
    /// loaded there now: it stays.
    Stays,
}

/// P1 for each chat being granted (its folder, whether it has a conversation or a loaded session there, whether it
/// has a worktree already), beside the folders where chats already granted work: two or more granted chats sharing a
/// folder start in their own worktrees, unless the person turned it off. A chat whose conversation is saved in the
/// folder can't move (a session belongs to its folder), nor one whose session runs there now: it stays, and the
/// person is told.
fn placements(chats: &[(PathBuf, bool, bool)], granted: &[PathBuf], worktrees: bool) -> Vec<Placement> {
    chats
        .iter()
        .enumerate()
        .map(|(i, (dir, there, has_worktree))| {
            let others = chats
                .iter()
                .enumerate()
                .filter(|(j, (_, _, wt))| *j != i && !wt)
                .map(|(_, (d, _, _))| d.as_path())
                .chain(granted.iter().map(PathBuf::as_path));
            if !worktrees || *has_worktree || !marv_kernel::worktree_by_default(dir, others) {
                Placement::Folder
            } else if *there {
                Placement::Stays
            } else {
                Placement::Worktree
            }
        })
        .collect()
}

/// What P1 prepared for a grant click, before any grant is made.
struct Prepared {
    /// Each chat's work folder, in order: its grant's folder (a new worktree's, its old one's, or its folder).
    works: Vec<PathBuf>,
    /// The worktrees made (chat id, its repository folder, the record): not yet in any chat's record.
    made: Vec<(String, PathBuf, store::ChatWorktree)>,
    moved: Vec<String>,
    stayed: Vec<String>,
    failed: Vec<String>,
}

/// Runtime-kernel §14.1: decides each chat's place ([`placements`]) and makes the worktrees, under `trees`
/// (`~/.marv/worktrees`) `/<project key>/<branch>`. `chats`: each with its checked folder; `loaded`: whether its
/// session is loaded now. Nothing is saved: the caller records the worktrees once every grant is made.
async fn prepare_worktrees(
    chats: &[(Chat, PathBuf)],
    loaded: &[bool],
    granted: &[PathBuf],
    worktrees: bool,
    trees: &std::path::Path,
) -> Prepared {
    let facts: Vec<(PathBuf, bool, bool)> = chats
        .iter()
        .zip(loaded)
        .map(|((c, dir), loaded)| (dir.clone(), c.session_id.is_some() || *loaded, c.worktree.is_some()))
        .collect();
    let placed = placements(&facts, granted, worktrees);
    let mut out = Prepared { works: Vec::new(), made: Vec::new(), moved: Vec::new(), stayed: Vec::new(), failed: Vec::new() };
    for ((chat, dir), placement) in chats.iter().zip(placed) {
        let mut work = work_root(chat).map(|(root, _)| root).unwrap_or_else(|_| dir.clone());
        match placement {
            Placement::Folder => {}
            Placement::Stays => out.stayed.push(chat.title.clone()),
            Placement::Worktree => {
                let base = trees.join(marv_session::paths::project_key(dir));
                match marv_session::worktree::create_worktree(dir, &base, &chat.title).await {
                    Ok(wt) => {
                        worktrees::copy_env_files(dir, &wt.work_dir);
                        let record = store::ChatWorktree {
                            dir: wt.work_dir.display().to_string(),
                            branch: wt.branch.clone(),
                            base: wt.base.clone(),
                            top: wt.dir.display().to_string(),
                        };
                        work = wt.work_dir.clone();
                        out.moved.push(format!("{} ({})", chat.title, wt.branch));
                        out.made.push((chat.id.clone(), dir.clone(), record));
                    }
                    // Not a repository (or git failed): it works in the folder, and the person is told.
                    Err(e) => out.failed.push(format!("{} ({})", chat.title, e.0)),
                }
            }
        }
        out.works.push(work);
    }
    out
}

#[allow(clippy::too_many_arguments)]
async fn grant_each(
    desktop: &Desktop,
    store: &marv_kernel::GrantStore,
    scope: &marv_kernel::FolderScope,
    home: &std::path::Path,
    expires_at_ms: Option<u64>,
    now: u64,
    lasts: &str,
    worktrees: bool,
    only: Option<&[String]>,
    made: &mut Vec<marv_kernel::GrantId>,
) -> Result<Option<String>, String> {
    let mut chats = Vec::new();
    for chat in desktop.chats() {
        // A team's start grants its own chats only (`teams`), not every chat under the folder.
        if only.is_some_and(|ids| !ids.contains(&chat.id)) {
            continue;
        }
        let Some(id) = marv_kernel::AgentId::new(&chat.id) else { continue };
        let Ok(dir) = check_root(&chat.folder) else { continue };
        if scope.contains(&dir) {
            chats.push((chat, id, dir));
        }
    }
    // Where the chats granted before (and not now) work: a grant's folder is its chat's work folder.
    let now_ids: std::collections::BTreeSet<&str> = chats.iter().map(|(c, _, _)| c.id.as_str()).collect();
    let granted: Vec<PathBuf> = store
        .load()
        .map(|s| s.grants)
        .unwrap_or_default()
        .into_iter()
        .filter(|g| g.live_at(now) && !now_ids.contains(g.chat.as_str()))
        .map(|g| g.folder.path().to_path_buf())
        .collect();
    let loaded: Vec<bool> = chats.iter().map(|(c, _, _)| desktop.live.is_loaded(&c.id)).collect();
    let listed: Vec<(Chat, PathBuf)> = chats.iter().map(|(c, _, dir)| (c.clone(), dir.clone())).collect();
    let prepared = prepare_worktrees(&listed, &loaded, &granted, worktrees, &marv_dir().join("worktrees")).await;
    let Prepared { works, made: made_trees, moved, stayed, failed } = prepared;
    // All or nothing (runtime-kernel §14.1): a worktree is the chat's only once every grant is made and every record
    // saved; otherwise every one made here is taken back (the caller revokes the grants made).
    let undo = |made_trees: &[(String, PathBuf, store::ChatWorktree)]| {
        for (id, repo, wt) in made_trees {
            let _ = desktop.store.update_chat(id, |c| {
                if c.worktree.as_ref() == Some(wt) {
                    c.worktree = None;
                }
            });
            remove_worktree(repo, wt);
        }
    };
    for ((chat, id, _), work) in chats.iter().zip(&works) {
        let made_grant = async {
            let grant = marv_kernel::NewGrant {
                chat: id.clone(),
                folder: marv_kernel::FolderScope::new(work, home).map_err(|e| e.to_string())?,
                expires_at_ms,
                cap: marv_kernel::SpendCap::default(),
                by: marv_kernel::GrantBy::Desktop,
                created_at_ms: now,
                label: format!("{} works while you're away", chat.title),
            };
            store.add(grant).await.map_err(|e| format!("{}: {e}", chat.title))
        };
        let (gid, change) = match made_grant.await {
            Ok(made_grant) => made_grant,
            Err(e) => {
                undo(&made_trees);
                return Err(e);
            }
        };
        made.push(gid);
        if let Some(log) = desktop.event_log() {
            marv_session::kernel_events::log_grant_made(log, gid, &change.snapshot, &change.replaced);
        }
        if lasts == "quit" {
            desktop.session_grants.lock().unwrap_or_else(|e| e.into_inner()).push(gid);
        }
    }
    for (id, _, wt) in &made_trees {
        if let Err(e) = desktop.store.update_chat(id, |c| c.worktree = Some(wt.clone())) {
            undo(&made_trees);
            return Err(format!("Couldn't save a chat's worktree: {e}"));
        }
    }
    let mut notes = Vec::new();
    if !moved.is_empty() {
        notes.push(format!(
            "Each in its own worktree (they share a folder): {}. Merge each branch when you're happy with it.",
            moved.join(", ")
        ));
    }
    if !stayed.is_empty() {
        notes.push(format!(
            "Staying in the folder, as their conversations are saved (or open) there: {}. Marv refuses an edit over a change a chat hasn't read; a new chat granted here gets its own worktree.",
            stayed.join(", ")
        ));
    }
    if !failed.is_empty() {
        notes.push(format!("Couldn't make a worktree, so working in the folder: {}.", failed.join(", ")));
    }
    Ok((!notes.is_empty()).then(|| notes.join(" ")))
}

/// One meter's line in the Usage view (runtime-kernel §16.9).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageRow {
    /// `chat`, `folder`, `core` or `grant`.
    kind: &'static str,
    /// The chat's or grant's id, the folder's path, the core's `provider/model`.
    key: String,
    /// What the person reads: a chat's title, a folder's name, the core.
    name: String,
    today_usd: f64,
    week_usd: f64,
    today_tokens: u64,
    week_tokens: u64,
    today_requests: u64,
}

/// A budget in the Usage view, with today's spend against it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BudgetView {
    /// `global`, `folder` or `chat`.
    scope: &'static str,
    /// The folder's path or the chat's id (empty for global).
    target: String,
    name: String,
    cap_usd: f64,
    spent_usd: f64,
    percent: u64,
    /// At 80% or more (runtime-kernel §16.6).
    warning: bool,
    reached: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageView {
    today_usd: f64,
    week_usd: f64,
    today_requests: u64,
    rows: Vec<UsageRow>,
    budgets: Vec<BudgetView>,
    /// Why the budgets file isn't used (it then counts as reached).
    problem: Option<String>,
}

fn usd(micros: u64) -> f64 {
    micros as f64 / 1_000_000.0
}

fn usage_view_of(desktop: &Desktop) -> Result<UsageView, String> {
    use marv_kernel::MeterScope;
    let book = desktop.meters.as_ref().ok_or("Usage needs your config folder to be an absolute path.")?;
    let now = book.now();
    let today = marv_kernel::day_of(now);
    let week = marv_session::allocator::meters_from_log(&marv_dir(), now, 7);
    let live = book.meters();
    let chats = desktop.chats();
    let title = |id: &str| chats.iter().find(|c| c.id == id).map(|c| c.title.clone());
    let mut rows = Vec::new();
    type Pick = fn(&MeterScope) -> bool;
    let kinds: [(&'static str, Pick); 4] = [
        ("folder", |s| matches!(s, MeterScope::Folder(_))),
        ("chat", |s| matches!(s, MeterScope::Chat(_))),
        ("core", |s| matches!(s, MeterScope::Core(_))),
        ("grant", |s| matches!(s, MeterScope::Grant(_))),
    ];
    for (kind, pick) in kinds {
        let mut scopes: Vec<MeterScope> = Vec::new();
        for day in week.days() {
            for (scope, _) in week.on_day(day, pick) {
                if !scopes.contains(&scope) {
                    scopes.push(scope);
                }
            }
        }
        for (scope, _) in live.on_day(today, pick) {
            if !scopes.contains(&scope) {
                scopes.push(scope);
            }
        }
        for scope in scopes {
            let t = live.get(today, &scope);
            // Earlier days from the log, today from the live meters (which hold the log's today and this run's).
            let (week_usd, week_tokens) =
                week.days().iter().filter(|d| **d != today).fold((t.usd_micros, t.unpriced_tokens), |(u, k), d| {
                    let m = week.get(*d, &scope);
                    (u.saturating_add(m.usd_micros), k.saturating_add(m.unpriced_tokens))
                });
            let (key, name) = match &scope {
                MeterScope::Chat(id) => (id.clone(), title(id).unwrap_or_else(|| "a chat".into())),
                MeterScope::Folder(f) => (
                    f.display().to_string(),
                    f.file_name().map_or_else(|| f.display().to_string(), |n| n.to_string_lossy().into_owned()),
                ),
                MeterScope::Core(c) => (c.clone(), c.clone()),
                MeterScope::Grant(g) => (g.0.to_string(), g.to_string()),
                MeterScope::All => continue,
            };
            rows.push(UsageRow {
                kind,
                key,
                name: marv_core::printable(&name),
                today_usd: usd(t.usd_micros),
                week_usd: usd(week_usd),
                today_tokens: t.unpriced_tokens,
                week_tokens,
                today_requests: t.requests,
            });
        }
    }
    let all = live.get(today, &MeterScope::All);
    let week_all: u64 = week
        .days()
        .iter()
        .filter(|d| **d != today)
        .fold(all.usd_micros, |sum, d| sum.saturating_add(week.get(*d, &MeterScope::All).usd_micros));
    let store = marv_kernel::BudgetStore::new(&marv_dir(), &marv_session::paths::home_dir()).map_err(|e| e.to_string())?;
    let (budgets, problem) = match store.load() {
        Ok(snapshot) => (snapshot.budgets, None),
        Err(e) => (Vec::new(), Some(e.to_string())),
    };
    let budgets = budgets
        .into_iter()
        .map(|b| {
            let spent = book.spent(&b.scope);
            let percent = marv_kernel::budget::percent(&spent, &b.cap);
            let (scope, target, name) = match &b.scope {
                marv_kernel::BudgetScope::Global => ("global", String::new(), "everything".to_string()),
                marv_kernel::BudgetScope::Folder(f) => (
                    "folder",
                    f.path().display().to_string(),
                    f.path().file_name().map_or_else(|| f.path().display().to_string(), |n| n.to_string_lossy().into_owned()),
                ),
                marv_kernel::BudgetScope::Chat(id) => ("chat", id.clone(), title(id).unwrap_or_else(|| "a chat".into())),
            };
            BudgetView {
                scope,
                target,
                name: marv_core::printable(&name),
                cap_usd: usd(b.cap.usd_micros),
                spent_usd: usd(spent.usd_micros),
                percent,
                warning: percent >= marv_kernel::WARN_PERCENT,
                reached: spent.usd_micros >= b.cap.usd_micros || spent.tokens >= b.cap.tokens,
            }
        })
        .collect();
    Ok(UsageView {
        today_usd: usd(all.usd_micros),
        week_usd: usd(week_all),
        today_requests: all.requests,
        rows,
        budgets,
        problem,
    })
}

/// The Mesh panel's Usage view (runtime-kernel §16.9): today and this week, per folder, chat, core and grant, with the
/// budgets inline.
#[tauri::command]
fn usage_view(desktop: State<'_, Desktop>) -> Result<UsageView, String> {
    usage_view_of(&desktop)
}

/// What the coding agents spent over the last `days` days (1 to 90), from their own history on this computer
/// ([`agent_usage`]): Claude Code, Codex and OpenCode, in Marv OS or not. Read off the main thread (a month of session
/// files is tens of megabytes the first time).
#[tauri::command]
async fn agent_usage(days: u64) -> Result<agent_usage::AgentUsage, String> {
    let days = days.clamp(1, 90);
    tokio::task::spawn_blocking(move || agent_usage::read(&marv_session::paths::home_dir(), days, now_ms()))
        .await
        .map_err(|e| e.to_string())
}

/// The person's click in the Usage view: a budget in dollars a day for everything (`global`), a folder or a chat;
/// `usd` none removes it. Only ever set here, by the person (or `marv kernel budget` at a terminal).
#[tauri::command]
async fn set_budget(
    desktop: State<'_, Desktop>,
    scope: String,
    target: Option<String>,
    usd: Option<f64>,
) -> Result<UsageView, String> {
    let cap = match usd {
        Some(usd) => Some(marv_kernel::budget::dollar_cap(usd).ok_or("A budget is more than $0 and at most $100000 a day.")?),
        None => None,
    };
    let home = marv_session::paths::home_dir();
    let scope = match (scope.as_str(), target) {
        ("global", _) => marv_kernel::BudgetScope::Global,
        ("folder", Some(folder)) => {
            let dir = check_root(&folder).map_err(|e| e.to_string())?;
            marv_kernel::BudgetScope::Folder(marv_kernel::FolderScope::new(&dir, &home).map_err(|e| e.to_string())?)
        }
        ("chat", Some(id)) => {
            let id = marv_kernel::AgentId::new(&id).ok_or("That isn't a chat.")?;
            marv_kernel::BudgetScope::Chat(id.as_str().to_string())
        }
        _ => return Err("A budget is for everything, a folder or a chat.".into()),
    };
    let store = marv_kernel::BudgetStore::new(&marv_dir(), &home).map_err(|e| e.to_string())?;
    store.set(scope.clone(), cap, now_ms()).await.map_err(|e| e.to_string())?;
    if let Some(log) = desktop.event_log() {
        log.append(now_ms(), marv_kernel::KernelEvent::BudgetSet { scope, cap });
    }
    usage_view_of(&desktop)
}

/// Revokes a grant: the chat's next unattended call asks (a call already running finishes).
#[tauri::command]
async fn revoke_grant(desktop: State<'_, Desktop>, id: u64) -> Result<GrantList, String> {
    // Runtime-kernel §14.1: a chat in its own worktree keeps it, and its conversation there: only its unattended runs
    // stop. The person moves it back to its folder from the chat's settings (`move_chat_to_folder`).
    let grants = grants_of(&desktop)?;
    let chat = grants.file.snapshot().grants.iter().find(|g| g.id.0 == id).map(|g| g.chat.to_string());
    revoke_grant_of(&desktop, id, chat.as_deref()).await?;
    grant_list(&desktop)
}

/// Revokes grant `id` (chat `chat`'s): an unattended turn under it stops now (§15.2), logged, dropped from this run's.
async fn revoke_grant_of(desktop: &Desktop, id: u64, chat: Option<&str>) -> Result<(), String> {
    let grants = grants_of(desktop)?;
    grants.file.store().clone().revoke(marv_kernel::GrantId(id)).await.map_err(|e| e.to_string())?;
    if let Some(chat) = chat {
        desktop.live.grant_revoked(chat);
    }
    if let Some(log) = desktop.event_log() {
        marv_session::kernel_events::log_grant_revoked(log, marv_kernel::GrantId(id));
    }
    desktop.session_grants.lock().unwrap_or_else(|e| e.into_inner()).retain(|g| g.0 != id);
    Ok(())
}

/// The person raises (or lowers) a grant's spend cap a day, in dollars: a grant paused at its cap works again.
#[tauri::command]
async fn set_grant_cap(desktop: State<'_, Desktop>, id: u64, usd: f64) -> Result<GrantList, String> {
    if !(usd.is_finite() && (0.0..=1000.0).contains(&usd)) {
        return Err("A spend cap is between $0 and $1000 a day.".into());
    }
    let grants = grants_of(&desktop)?;
    let snapshot = grants.file.snapshot();
    let grant = snapshot.grants.iter().find(|g| g.id.0 == id).ok_or("That grant is gone.")?;
    // The token cap (for requests with no price) moves with the dollars, so raising the cap resumes a grant paused
    // at either.
    let usd_micros = (usd * 1e6).round() as u64;
    let tokens = match grant.cap.usd_micros {
        0 => (marv_kernel::DEFAULT_DAILY_TOKENS as f64 * usd / 2.0).round() as u64,
        old => (grant.cap.tokens as f64 * usd_micros as f64 / old as f64).round() as u64,
    };
    let cap = marv_kernel::SpendCap { usd_micros, tokens };
    let changed = grants.file.store().set_cap(grant.id, cap).await.map_err(|e| e.to_string())?;
    if let Some(log) = desktop.event_log() {
        marv_session::kernel_events::log_grant_made(log, grant.id, &changed, &[]);
    }
    grant_list(&desktop)
}

/// The person's timers file (`~/.marv/kernel/timers.json`, runtime-kernel §15.4).
fn timer_store() -> Result<marv_kernel::TimerStore, String> {
    marv_kernel::TimerStore::new(&marv_dir()).map_err(|e| e.to_string())
}

/// The person's signal to a chat from the Mesh panel (runtime-kernel §15.3): "stop", "pause", "resume" or "close".
/// Only the page calls this (the person's click); nothing on the mesh can.
#[tauri::command]
fn chat_signal(desktop: State<'_, Desktop>, id: String, kind: String) -> Result<GrantList, String> {
    let kind: marv_kernel::SignalKind =
        serde_json::from_value(Value::String(kind)).map_err(|_| "That isn't a signal Marv knows.".to_string())?;
    desktop.live.signal(&id, kind)?;
    grant_list(&desktop)
}

/// The chats rule `id` covers now (§15.3's team).
fn team_of(desktop: &Desktop, id: u64) -> Result<Vec<String>, String> {
    let snapshot = rule_store()?.load().map_err(|e| e.to_string())?;
    let rule = snapshot.rules.iter().find(|r| r.id.0 == id).ok_or("That rule is gone.")?;
    Ok(desktop
        .chats()
        .into_iter()
        .filter_map(|chat| {
            let agent = marv_kernel::AgentId::new(&chat.id)?;
            let folder = check_chat_root(&chat.folder).ok()?;
            let me = marv_kernel::AgentRef {
                mesh_name: marv_kernel::mesh_name_for(&folder, &agent),
                id: Some(agent),
                folder,
                kind: marv_kernel::AgentKind::Chat,
            };
            (rule.from.covers(&me) || rule.to.covers(&me)).then_some(chat.id)
        })
        .collect())
}

/// "Stop the team" (§15.3): every chat rule `id` covers is paused and its turn stopped, and its agent's terminal ends
/// with its whole session, as closing it does (paused, the members' agents and what they started ran on until their
/// chats were deleted; seen 2026-11-17). A paused chat isn't woken, so mail doesn't start it again until resumed. How
/// many were running.
#[tauri::command]
fn stop_team(desktop: State<'_, Desktop>, terminals: State<'_, terminals::Terminals>, id: u64) -> Result<usize, String> {
    let team = team_of(&desktop, id)?;
    let paused = desktop.live.stop_team(&team);
    let ended = terminals.close_chats(&team);
    for chat in &ended {
        desktop.live.terminal_closed(chat);
    }
    Ok(paused.max(ended.len()))
}

/// "Resume the team": every paused chat rule `id` covers runs again. How many were paused.
#[tauri::command]
fn resume_team(desktop: State<'_, Desktop>, id: u64) -> Result<usize, String> {
    needs_account()?;
    Ok(desktop.live.resume_team(&team_of(&desktop, id)?))
}

/// The person makes a timer for a granted chat (§15.4): once at `at_ms`, or every `every_minutes` from then ("every
/// morning at 9": the page gives the next 9:00 and 1440). It runs while Marv desktop runs, under the chat's grant.
#[tauri::command]
async fn add_timer(
    desktop: State<'_, Desktop>,
    chat: String,
    at_ms: u64,
    every_minutes: Option<u64>,
    daily_minute: Option<u16>,
    tz: Option<String>,
    note: String,
) -> Result<GrantList, String> {
    let agent = marv_kernel::AgentId::new(&chat).ok_or("That chat is gone.")?;
    let grants = grants_of(&desktop)?;
    let now = now_ms();
    let snapshot = grants.file.snapshot();
    if marv_kernel::grant_for(&snapshot, &agent, now, |g| grants.spend.get(g, now)).is_err() {
        return Err("A timer needs the chat's permission to work while you're away (a live grant) first.".into());
    }
    let note = note.trim().to_string();
    if note.is_empty() {
        return Err("Say what the chat should do when the timer fires.".into());
    }
    // "Every morning at 9": the page's local time and IANA zone, kept across DST (review P3-1).
    let repeat = match (daily_minute, tz, every_minutes) {
        (Some(minute_of_day), Some(tz), _) => marv_kernel::Repeat::Daily { minute_of_day, tz },
        (_, _, None) => marv_kernel::Repeat::Once,
        (_, _, Some(m)) => marv_kernel::Repeat::Every { every_ms: m.saturating_mul(60_000) },
    };
    let timer = marv_kernel::PersonTimer { chat: agent, repeat, at_ms: at_ms.max(now), note };
    let (made, _) = timer_store()?.add_person(timer, now).await.map_err(|e| e.to_string())?;
    if let Some(log) = desktop.event_log() {
        log.append(
            now,
            marv_kernel::KernelEvent::TimerMade {
                id: made.id,
                chat: made.chat.clone(),
                by: made.by,
                recurring: made.repeat.recurs(),
                fires_at_ms: made.at_ms,
            },
        );
    }
    grant_list(&desktop)
}

/// The person removes a timer.
#[tauri::command]
async fn remove_timer(desktop: State<'_, Desktop>, id: u64) -> Result<GrantList, String> {
    let store = timer_store()?;
    let chat = store.load().ok().and_then(|s| s.timers.into_iter().find(|t| t.id.0 == id)).map(|t| t.chat);
    let (gone, _) = store.remove(marv_kernel::TimerId(id)).await.map_err(|e| e.to_string())?;
    if let (true, Some(chat), Some(log)) = (gone, chat, desktop.event_log()) {
        log.append(
            now_ms(),
            marv_kernel::KernelEvent::TimerVoid {
                id: marv_kernel::TimerId(id),
                chat,
                reason: marv_kernel::TimerVoidReason::Removed,
            },
        );
    }
    grant_list(&desktop)
}

/// How often the kernel looks at timers and at unattended turns whose grant ended (runtime-kernel §15).
const KERNEL_TICK: std::time::Duration = std::time::Duration::from_secs(10);

/// Whether native dialogs can open: on Linux, GTK's backend is Wayland or X11 (or GTK picks, `GDK_BACKEND` unset or
/// `*`). Broadway and other headless backends have none that work.
fn native_dialogs(gdk_backend: Option<&str>) -> bool {
    if !cfg!(target_os = "linux") {
        return true;
    }
    gdk_backend.is_none_or(|v| v.split(',').any(|b| matches!(b.trim(), "wayland" | "x11" | "*" | "")))
}

/// The window shows the app and nothing else: a link that would navigate it away (a reply's link to a dev server, a
/// page the model named) opens in the person's browser instead, since the window has no back button.
fn stay_in_the_app<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("stay-in-the-app")
        .on_navigation(|_webview, url| {
            if in_the_app(url) {
                return true;
            }
            if matches!(url.scheme(), "http" | "https") {
                let _ = open_url(url.to_string());
            }
            false
        })
        .build()
}

/// The app's own pages: the bundled frontend (tauri:// or the tauri.localhost host), IPC, about:/data: frames, and in a
/// debug build the dev server.
fn in_the_app(url: &tauri::Url) -> bool {
    match url.scheme() {
        "tauri" | "ipc" | "about" | "data" | "blob" | "asset" => true,
        "http" | "https" => {
            let host = url.host_str().unwrap_or_default();
            host == "tauri.localhost"
                || host == "ipc.localhost"
                || host == "asset.localhost"
                || (cfg!(debug_assertions) && (host == "localhost" || host == "127.0.0.1") && url.port() == Some(1420))
        }
        _ => false,
    }
}

/// The person's keys, to keep out of the app's log: every string under a key-ish name in Marv's config file
/// (`apiKey`, `zaiApiKey`, a provider's `token`), and the OpenRouter key from the environment.
fn own_secrets() -> marv_core::Secrets {
    fn collect(value: &Value, name: &str, out: &mut Vec<String>) {
        match value {
            Value::String(s) => {
                let n = name.to_ascii_lowercase();
                if n.contains("key") || n.contains("token") || n.contains("secret") {
                    out.push(s.clone());
                }
            }
            Value::Object(map) => map.iter().for_each(|(k, v)| collect(v, k, out)),
            Value::Array(items) => items.iter().for_each(|v| collect(v, name, out)),
            _ => {}
        }
    }
    let mut values: Vec<String> = std::env::var("OPENROUTER_API_KEY").into_iter().collect();
    for file in ["config.json", "mcp.json"] {
        if let Some(v) = std::fs::read_to_string(marv_dir().join(file)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok())
        {
            collect(&v, "", &mut values);
        }
    }
    marv_core::Secrets::new(values)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();
    // Native dialogs (and their parent window) only on a display that has them: a headless GTK backend (Broadway)
    // crashed in one. The page's dialog calls then fail and are logged (`app/src/lib/dialog.ts`).
    let builder = if native_dialogs(std::env::var("GDK_BACKEND").ok().as_deref()) {
        builder.plugin(tauri_plugin_dialog::init())
    } else {
        applog::warn("start", "no native dialogs on this GDK backend (use Wayland or X11, or Xvfb when headless)");
        builder
    };
    builder
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(stay_in_the_app())
        .setup(|app| {
            if let Ok(logs) = app.path().app_log_dir() {
                applog::init(&logs, own_secrets(), &app.package_info().version.to_string());
            }
            let config = app.path().app_config_dir()?;
            let data = app.path().app_data_dir()?;
            // Kernel #41: an earlier window that stopped responding is closed first (its mesh names freed), its report
            // written; then this window's own record.
            let store = Store::new(config, data.clone());
            marv_web::terminal_agents::set_own(&store.own_agents());
            // A chat whose folder is missing stays: it may be an unmounted drive, another branch's folder, a rename or
            // a denied permission. Starting it says so (`check_chat_root`); the person deletes it.
            let plan = WatchPlan::from_env(|k| std::env::var(k).ok(), cfg!(debug_assertions));
            // Never this process's launcher (`main.rs` runs Marv again with WebKit's renderer setting): the same program.
            #[cfg(unix)]
            let skip = [std::os::unix::process::parent_id()];
            // Windows: no launcher process in between (the renderer setting is Linux's).
            #[cfg(not(unix))]
            let skip: [u32; 0] = [];
            let (notice, own) = start_instances(&data, &plan, store.chats().len(), &instance::Machine, &skip);
            // The first argument that isn't a flag: `marv-desktop .`, `marv-desktop ~/Projects/site`.
            let launch = std::env::args().skip(1).find(|a| !a.starts_with('-'));
            let live = chats::Chats::new(std::sync::Arc::new(AppHost(app.handle().clone())));
            // The kernel's event log (runtime-kernel 3): in the user's config folder, never a project's. A problem is
            // said once (the app's log); it never stops a chat.
            let events = marv_session::kernel_events::EventWriter::new(&marv_dir()).ok();
            if let Some(log) = &events {
                log.on_error(std::sync::Arc::new(|text: String| applog::warn("events", &text)));
                live.set_events(log.clone());
            }
            // Runtime-kernel §15.4: the person's timers, fired while the app runs (a missed one fires once, late).
            if let Ok(timers) = timer_store() {
                live.set_timers(timers);
            }
            // The dock's Git tab's lazygits: they end with the app.
            app.manage(terminals::Terminals::default());
            app.manage(std::sync::Arc::new(delegates::Delegations::default()));
            app.manage(Desktop {
                store,
                live,
                launch: Mutex::new(launch),
                proposals: Mutex::new(HashMap::new()),
                session_rules: Mutex::new(Vec::new()),
                events: events.clone(),
                expiries: Mutex::new(marv_kernel::Expiries::default()),
                sign_in: Mutex::new(None),
                notifier: notify::Notifier::default(),
                away: Default::default(),
                grants: marv_kernel::GrantStore::new(&marv_dir(), &marv_session::paths::home_dir()).ok().map(|store| Grants {
                    spend: std::sync::Arc::new(marv_session::grants::SpendBook::load(store.clone())),
                    file: std::sync::Arc::new(marv_kernel::FileGrants::new(store)),
                }),
                session_grants: Mutex::new(Vec::new()),
                // The watchdog's stop notices from the last run, told in each chat as it loads.
                notices: Mutex::new(watchdog::take_notices(&data)),
                started_on: Mutex::default(),
                reports: teams::TeamReports::default(),
                grant_expiries: Mutex::default(),
                meters: Some(std::sync::Arc::new(marv_session::allocator::MeterBook::for_person(
                    &marv_dir(),
                    &marv_session::paths::home_dir(),
                ))),
                watch: Watch {
                    data: data.clone(),
                    started: std::time::Instant::now(),
                    heartbeat: Mutex::new(plan.timings.map(watchdog::Heartbeat::new)),
                    instance: own,
                    notice: Mutex::new(notice),
                    exit: Default::default(),
                    closing: Default::default(),
                },
            });
            if let Some(t) = plan.timings {
                tauri::async_runtime::spawn(watch_page(app.handle().clone(), t.every));
                #[cfg(target_os = "linux")]
                watch_web_process(app);
            }
            // P2-6: SIGTERM (a later launch closing this window, a logout) and SIGINT close the normal way.
            tauri::async_runtime::spawn(close_on_sigterm(app.handle().clone()));
            // Runtime-kernel §15: the kernel's tick (timers due, grants ended under an unattended turn).
            let ticker = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    let desktop = ticker.state::<Desktop>();
                    desktop.live.tick(now_ms()).await;
                    tokio::time::sleep(KERNEL_TICK).await;
                }
            });
            // D13: chats a rule covers are on the mesh from the start, and as rules come and go.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    let desktop = handle.state::<Desktop>();
                    // Runtime-kernel chat card: `mesh.cards` read again (a change applies within a look), off any lock.
                    desktop.live.set_cards(marv_session::config::ConfigStore::new(&marv_dir()).cards());
                    desktop.live.sync_rules(&rule_covered(&desktop));
                    tokio::time::sleep(RULES_LOOK).await;
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            set_root,
            root_up,
            enter_folder,
            create_space,
            switch_space,
            rename_space,
            remove_space,
            set_apps_off,
            chat_devices,
            set_chat_devices,
            set_provider,
            local_models,
            pull_model,
            mcp_servers,
            list_roles,
            list_teams,
            start_team,
            end_team,
            save_team_file,
            team_reports,
            mcp_lookup,
            mcp_install,
            mcp_remove,
            dictate,
            open_url,
            apps_status,
            app_connect,
            app_sign_in,
            app_cancel_sign_in,
            app_sign_out,
            take_launch,
            set_api_key,
            set_zai_key,
            set_kimi_key,
            chatgpt_sign_in,
            chatgpt_cancel_sign_in,
            chatgpt_sign_out,
            plan_models,
            plan_login_start,
            plan_login_verify,
            plan_login_with,
            plan_login_cancel,
            plan_set_marketing,
            account_step_done,
            plan_status,
            plan_logout,
            plan_manage,
            plan_use_key,
            list_dir,
            create_folder,
            rename_entry,
            trash_entry,
            list_chats,
            rename_chat,
            set_chat_private,
            set_chat_permissions,
            set_default_permissions,
            default_permissions,
            delete_chat,
            create_chat,
            claude_code_available,
            folder_in_git,
            harnesses,
            set_harness_sandbox,
            open_chat,
            chat_message,
            close_chat,
            stop_chat,
            live_chats,
            write_overlaps,
            chat_files,
            chat_writes,
            chat_changes,
            git_terminal_open,
            git_terminal_write,
            clipboard_write,
            log_page,
            logs_open,
            logs_recent,
            start_triage,
            switch_agent,
            chat_turns,
            chat_turn_diff,
            chat_turn_restore,
            chat_deliver_mail,
            chat_provider_changed,
            clipboard_read,
            clipboard_has_image,
            git_terminal_resize,
            git_terminal_detach,
            git_terminal_close,
            git_terminal_pop_out,
            git_repo_here,
            git_init_here,
            chat_terminal_open,
            chat_terminal_type,
            terminal_agents,
            own_agents,
            add_own_agent,
            remove_own_agent,
            git_missing,
            install_package,
            chat_terminal_close,
            chat_terminal_running,
            chat_processes,
            chat_process_logs,
            stop_chat_process,
            marv_config,
            set_marv_config,
            move_chat_to_folder,
            chat_worktree,
            worktree_status,
            worktree_merge,
            worktree_discard,
            activity,
            timeline,
            away_leave,
            away_back,
            notifications,
            permissions,
            about_marv,
            mesh_rules,
            allow_folder,
            revoke_rule,
            board,
            board_post,
            board_clear,
            chat_grants,
            revoke_grant,
            set_grant_cap,
            usage_view,
            agent_usage,
            set_budget,
            chat_signal,
            stop_team,
            resume_team,
            add_timer,
            remove_timer,
            watchdog_notice,
            hang_reports,
            open_hang_reports,
        ])
        .build(tauri::generate_context!())
        .expect("error while running Marv desktop")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                on_exit(app);
            }
        });
}

/// As the app exits (once: the normal exit, or the fallback when the main thread is stuck): rules and grants made
/// "until Marv quits" end with it, the event log is flushed, and this window's instance record goes.
fn on_exit(app: &AppHandle) {
    use std::sync::atomic::Ordering::SeqCst;
    let desktop = app.state::<Desktop>();
    if desktop.watch.exit.compare_exchange(0, 1, SeqCst, SeqCst).is_err() {
        return;
    }
    if let Some(own) = &desktop.watch.instance {
        own.remove();
    }
    // The agents' processes (and what they started) end with the app: both exit paths come through here.
    if let Some(terminals) = app.try_state::<terminals::Terminals>() {
        terminals.close_all();
    }
    let ids = std::mem::take(&mut *desktop.session_rules.lock().unwrap_or_else(|e| e.into_inner()));
    if let Ok(store) = rule_store() {
        tauri::async_runtime::block_on(async {
            for id in ids {
                if store.revoke(id).await.is_ok()
                    && let Some(log) = desktop.event_log()
                {
                    marv_session::kernel_events::log_revoked(log, id);
                }
            }
            // ... and so do grants made "until Marv quits".
            let grants = std::mem::take(&mut *desktop.session_grants.lock().unwrap_or_else(|e| e.into_inner()));
            if let Some(g) = &desktop.grants {
                for id in grants {
                    if g.file.store().revoke(id).await.is_ok()
                        && let Some(log) = desktop.event_log()
                    {
                        marv_session::kernel_events::log_grant_revoked(log, id);
                    }
                }
            }
            if let Some(log) = &desktop.events {
                log.flush().await;
            }
        });
    }
    desktop.watch.exit.store(2, SeqCst);
}

/// Ends the process after `after` if the normal exit hasn't: runs the exit's cleanup if it never started, waits (up
/// to 10 s) for one that's running, then exits. For when the main thread is stuck and `app.exit` never comes.
fn exit_fallback(app: AppHandle, after: std::time::Duration) {
    std::thread::spawn(move || {
        std::thread::sleep(after);
        on_exit(&app);
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.state::<Desktop>().watch.exit.load(std::sync::atomic::Ordering::SeqCst) != 2
            && std::time::Instant::now() < until
        {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        std::process::exit(0);
    });
}

/// This build's version (the workspace's), in instance records and hang reports.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Kernel #41, at start: earlier windows whose process is gone lose their records; one that runs but stopped
/// responding is closed (`instance::take_over`), with a hang report each. The notice for the person, if one was.
fn take_over_hung_windows(data: &Path, chats: usize, system: &dyn instance::System, own: u32, skip: &[u32]) -> Option<String> {
    let closed = instance::take_over(&data.join("instances"), own, skip, system);
    for c in &closed {
        applog::warn("start", &format!("an earlier Marv window (pid {}) stopped responding; closed it", c.record.pid));
        let report = hang_report::HangReport {
            version: c.record.version.clone(),
            time_ms: now_ms(),
            fired: watchdog::Fired::StaleInstance,
            acted: hang_report::Acted::Closed,
            uptime_s: now_ms().saturating_sub(c.record.started_ms) / 1000,
            chats,
            reloads: 0,
            os: std::env::consts::OS.into(),
        };
        let _ = hang_report::write(data, &report);
    }
    (!closed.is_empty()).then(|| watchdog::TAKEN_OVER_TEXT.to_string())
}

/// A hang report for this window.
fn report(desktop: &Desktop, fired: watchdog::Fired, acted: hang_report::Acted, reloads: u32) {
    let report = hang_report::HangReport {
        version: VERSION.into(),
        time_ms: now_ms(),
        fired,
        acted,
        uptime_s: desktop.watch.started.elapsed().as_secs(),
        chats: desktop.store.chats().len(),
        reloads,
        os: std::env::consts::OS.into(),
    };
    applog::error("watchdog", &format!("{fired:?}, {acted:?}"));
    let _ = hang_report::write(&desktop.watch.data, &report);
}

/// Kernel #41: the watchdog's loop (`watchdog` says what it decides and why). Every `every` it pings the page (a
/// script round-trip answering `document.visibilityState`) and the main thread (`run_on_main_thread`), and acts on
/// what the heartbeat says: reload the page, write a report, or exit if idle (the main thread stuck for minutes).
/// While the main thread answers, this window's instance record is touched, so a later launch leaves it alone.
async fn watch_page(app: AppHandle, every: std::time::Duration) {
    use hang_report::Acted;
    use watchdog::Action;
    loop {
        tokio::time::sleep(every).await;
        let desktop = app.state::<Desktop>();
        let watch = &desktop.watch;
        if watch.closing.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let now = watch.now();
        let (mut action, mut alive, mut reloads) = (Action::Nothing, false, 0);
        watch.heartbeat(|hb| (action, alive, reloads) = (hb.tick(now), hb.alive(now), hb.reloads()));
        if alive && let Some(own) = &watch.instance {
            own.touch();
        }
        match action {
            Action::Nothing => {}
            Action::Reload(why) => {
                report(&desktop, why, Acted::Reloaded, reloads);
                if reloads > 1 {
                    // The window can't say it: a desktop notification does.
                    tauri::async_runtime::spawn_blocking(|| {
                        let _ = notify_rust::Notification::new()
                            .appname("Marv")
                            .summary("Marv")
                            .body(watchdog::RELOADING_TEXT)
                            .show();
                    });
                }
                if let Some(window) = app.get_webview_window("main") {
                    reload(&window, why);
                }
            }
            Action::Report(why) => report(&desktop, why, Acted::Reported, reloads),
            Action::ExitIfIdle(why) => {
                // Only when no turn runs; the look is bounded (a stuck main thread can be the chats' own lock), and a
                // look that doesn't come back counts as "something runs": keep running.
                let live = desktop.live.clone();
                let busy = tauri::async_runtime::spawn_blocking(move || live.busy_ids());
                let idle = matches!(
                    tokio::time::timeout(std::time::Duration::from_secs(5), busy).await,
                    Ok(Ok(ids)) if ids.is_empty()
                );
                if idle {
                    report(&desktop, why, Acted::Exited, reloads);
                    exit_fallback(app.clone(), std::time::Duration::from_secs(2));
                    app.exit(0);
                    return;
                }
            }
        }
        watch.heartbeat(|hb| {
            hb.page_pinged(now);
            hb.main_probed(now);
        });
        if let Some(window) = app.get_webview_window("main") {
            let handle = app.clone();
            let _ = window.eval_with_callback("document.visibilityState", move |state: String| {
                let visible = state.trim_matches('"') != "hidden";
                handle.state::<Desktop>().watch.heartbeat(|hb| hb.page_answered(visible));
            });
        }
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || handle.state::<Desktop>().watch.heartbeat(|hb| hb.main_answered()));
    }
}

/// Reloads the page. A hung web process (silent, or WebKit says unresponsive) is ended first on Linux
/// (`terminate_web_process`): reloading into a process that doesn't answer does nothing, and WebKit starts a new one.
fn reload(window: &tauri::WebviewWindow, why: watchdog::Fired) {
    #[cfg(target_os = "linux")]
    {
        let hung = matches!(why, watchdog::Fired::HeartbeatTimeout | watchdog::Fired::WebProcessUnresponsive);
        let done = window.with_webview(move |view| {
            use webkit2gtk::WebViewExt;
            let view = view.inner();
            if hung {
                view.terminate_web_process();
            }
            view.reload();
        });
        if done.is_ok() {
            return;
        }
    }
    let _ = why;
    let _ = window.reload();
}

/// Kernel #41 (P2-6): SIGTERM closes the window the normal way instead of cutting it off: a later launch closing a
/// window that stopped responding (`instance`, SIGKILL only if this doesn't finish), or the session ending.
/// Windows: the user logging off or the machine shutting down.
#[cfg(windows)]
async fn close_on_sigterm(app: AppHandle) {
    let (Ok(mut logoff), Ok(mut shutdown)) = (tokio::signal::windows::ctrl_logoff(), tokio::signal::windows::ctrl_shutdown())
    else {
        return;
    };
    tokio::select! {
        _ = logoff.recv() => {}
        _ = shutdown.recv() => {}
    }
    close(&app).await;
}

/// Unix: SIGINT too (Ctrl+C where it was started, or the launcher passing it on): its default ended the process
/// without the exit's cleanup, and the agents, each in a session of its own, ran on.
#[cfg(unix)]
async fn close_on_sigterm(app: AppHandle) {
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut term), Ok(mut interrupt)) = (signal(SignalKind::terminate()), signal(SignalKind::interrupt())) else {
        return;
    };
    let got = tokio::select! {
        Some(()) = term.recv() => "SIGTERM",
        Some(()) = interrupt.recv() => "SIGINT",
        else => return,
    };
    applog::info("exit", &format!("asked to close ({got})"));
    close(&app).await;
}

/// Closes the app: chats with a turn running are stopped through the person's stop path (each told why when it
/// next loads, `<data>/notices.json`), their turns get up to 3 s to end (P3-4), then the normal exit. If the main
/// thread is stuck (the exit never comes), the fallback runs the cleanup and ends the process.
async fn close(app: &AppHandle) {
    let desktop = app.state::<Desktop>();
    desktop.watch.closing.store(true, std::sync::atomic::Ordering::SeqCst);
    exit_fallback(app.clone(), std::time::Duration::from_secs(10));
    let (live, data) = (desktop.live.clone(), desktop.watch.data.clone());
    // Off this task, and bounded: the chats' lock could be what's stuck.
    let stopping = tauri::async_runtime::spawn_blocking(move || {
        let (ids, sessions) = live.stop_running();
        let notices = ids.iter().map(|id| (id.clone(), watchdog::STOPPED_TEXT.to_string())).collect();
        let _ = watchdog::save_notices(&data, &notices);
        sessions
    });
    let sessions =
        tokio::time::timeout(std::time::Duration::from_secs(5), stopping).await.ok().and_then(|r| r.ok()).unwrap_or_default();
    let until = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    while sessions.iter().any(|s| s.busy()) && tokio::time::Instant::now() < until {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    drop(sessions);
    app.exit(0);
}

/// Kernel #41 (Linux): the web view says when its web process ends (crashed, killed, out of memory) and when it stops
/// (or starts again) responding (WebKitGTK's `is-web-process-responsive`): the watchdog acts on these, and uses the
/// latter to corroborate a silent page before reloading it.
#[cfg(target_os = "linux")]
fn watch_web_process(app: &tauri::App) {
    let Some(window) = app.get_webview_window("main") else { return };
    let (gone, answers) = (app.handle().clone(), app.handle().clone());
    let hooked = window.with_webview(move |view| {
        use webkit2gtk::WebViewExt;
        let view = view.inner();
        view.connect_web_process_terminated(move |_, reason| {
            applog::error("webview", &format!("the window's web process ended ({reason:?})"));
            // Ours (a hung process ended before a reload): not another death.
            if reason == webkit2gtk::WebProcessTerminationReason::TerminatedByApi {
                return;
            }
            gone.state::<Desktop>().watch.heartbeat(|hb| hb.web_process_gone());
        });
        view.connect_is_web_process_responsive_notify(move |view| {
            let responsive = view.is_web_process_responsive();
            let desktop = answers.state::<Desktop>();
            let now = desktop.watch.now();
            desktop.watch.heartbeat(|hb| hb.web_responsive(responsive, now));
        });
    });
    if let Err(e) = hooked {
        applog::warn("watchdog", &format!("can't watch the web process ({e}): a silent page is only reported"));
    }
}

/// Kernel #41: what this launch did at start, for the page once (it closed an earlier window that stopped responding).
#[tauri::command]
fn watchdog_notice(desktop: State<'_, Desktop>) -> Option<String> {
    desktop.watch.notice.lock().unwrap_or_else(|e| e.into_inner()).take()
}

#[derive(Serialize)]
struct HangReports {
    count: usize,
    /// The folder they're in (the app's own data folder).
    dir: String,
}

/// Kernel #41: the hang reports, for Settings › About.
#[tauri::command]
fn hang_reports(desktop: State<'_, Desktop>) -> HangReports {
    let data = &desktop.watch.data;
    HangReports { count: hang_report::count(data), dir: hang_report::dir(data).display().to_string() }
}

/// Kernel #41: the reports' folder in the file manager. Nothing is sent anywhere.
#[tauri::command]
fn open_hang_reports(desktop: State<'_, Desktop>) -> Result<(), String> {
    let dir = hang_report::dir(&desktop.watch.data);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    system_open(dir.as_os_str())
}

/// A line from the page for the app's log (`applog`): an error on the page, a call that failed. `area` names where.
#[tauri::command]
fn log_page(level: String, area: String, text: String) {
    applog::write(applog::Level::from_word(&level), &format!("page/{area}"), &text);
}

/// The log's folder in the file manager (Settings › About). Nothing is sent anywhere.
#[tauri::command]
fn logs_open() -> Result<(), String> {
    let dir = applog::dir().ok_or("There's no log on this computer.")?;
    system_open(dir.as_os_str())
}

/// A delegate for chat `parent` (its agent's `delegate`, `delegates`): a new chat in its folder (and worktree), run
/// by `to`, with its permission mode, its terminal started unattended (so inside the outer sandbox); the new chat's id.
fn start_delegate(app: &AppHandle, parent: &str, to: &marv_web::terminal_agents::Agent) -> Result<String, String> {
    let desktop = app.state::<Desktop>();
    let from = desktop.chats().into_iter().find(|c| c.id == parent).ok_or("This chat is gone.")?;
    if from.delegated_by.is_some() {
        return Err("A delegate can't hand its task on: do it here, or say what's missing.".into());
    }
    let title: String = format!("{} for {}", to.title, from.title).chars().take(80).collect();
    let chat = new_chat(&desktop, from.folder.clone(), &title, None, None, None, None)?;
    let harness = (to.id != marv_web::terminal_agents::MARV).then(|| to.id.to_string());
    desktop.store.update_chat(&chat.id, |c| {
        (c.harness, c.worktree, c.permissions, c.delegated_by) =
            (harness, from.worktree.clone(), from.permissions.clone(), Some(parent.to_string()));
    })?;
    app.state::<std::sync::Arc<delegates::Delegations>>().started(&chat.id, parent);
    applog::info("delegate", &format!("a chat delegated a task to {}", to.title));
    start_chat_terminal(app, &chat.id, 120, 36, None, true)?;
    Ok(chat.id)
}

/// The person switches chat `id` to another agent (`agent`, an installed one, or `marv`), in place: its terminal ends,
/// the agent taking over is given the conversation so far from the outgoing agent's own history (`handoff`), and an
/// agent it ran before continues its own conversation. A team member keeps its agent (its role and the team's mail are
/// set up for it). What to type into the new agent's terminal once it's ready.
#[tauri::command]
async fn switch_agent(
    desktop: State<'_, Desktop>,
    terminals: State<'_, terminals::Terminals>,
    id: String,
    agent: String,
) -> Result<String, String> {
    use marv_web::terminal_agents::{MARV, find, for_chat};
    let chat = desktop.chats().into_iter().find(|c| c.id == id).ok_or("That chat is gone.")?;
    if chat.team.is_some() {
        return Err("A team member keeps its agent: its role and the team's mail are set up for it.".into());
    }
    let from = for_chat(chat.harness.as_deref());
    let to = find(&agent).ok_or_else(|| format!("\"{}\" isn't an agent Marv OS can run.", marv_core::printable(&agent)))?;
    if to.id == from.id {
        return Err(format!("This chat already runs {}.", to.title));
    }
    if to.id != MARV {
        check_harness(to.id)?;
    }
    let (dir, _) = work_root(&chat)?;
    let (session, home, marv) = (chat.session_id.clone(), marv_session::paths::home_dir(), marv_dir());
    let conversation = {
        let (from_id, dir) = (from.id, dir.clone());
        tokio::task::spawn_blocking(move || handoff::conversation(from_id, session.as_deref(), &dir, &home, &marv))
            .await
            .map_err(|e| e.to_string())?
    };
    let path = handoff::write(&dir, desktop.store.data_dir(), &id, from.title, conversation.as_deref())?;
    terminals.close_key(&format!("chat:{id}"));
    desktop.live.terminal_closed(&id);
    desktop.store.update_chat(&id, |c| {
        let mine =
            store::PastAgent { agent: from.id.into(), session_id: c.session_id.take(), terminal_started: c.terminal_started };
        c.past_agents.retain(|p| p.agent != from.id);
        c.past_agents.push(mine);
        let back = c.past_agents.iter().position(|p| p.agent == to.id).map(|i| c.past_agents.remove(i));
        (c.session_id, c.terminal_started) = back.map_or((None, false), |b| (b.session_id, b.terminal_started));
        c.harness = (to.id != MARV).then(|| to.id.to_string());
        c.agent_model = None;
    })?;
    applog::info("handoff", &format!("a chat switched from {} to {}", from.title, to.title));
    Ok(handoff::first_message(from.title, &path))
}

/// Chat `id`'s checkpoints' book, where it works (its worktree or folder); an error outside a git repository.
fn chat_book(desktop: &Desktop, id: &str) -> Result<checkpoints::Book, String> {
    let chat = desktop.chats().into_iter().find(|c| c.id == id).ok_or("That chat is gone.")?;
    let (dir, _) = work_root(&chat)?;
    checkpoints::book_for(id, &dir, desktop.store.data_dir()).ok_or_else(|| {
        "Turns are kept in a git repository: this chat's folder isn't in one (Git in the dock can start one).".into()
    })
}

/// Chat `id`'s kept turns, newest last ([`checkpoints`]): each that changed files, with them.
#[tauri::command]
fn chat_turns(desktop: State<'_, Desktop>, id: String) -> Result<Vec<checkpoints::Turn>, String> {
    Ok(chat_book(&desktop, &id)?.turns())
}

/// Turn `n`'s own diff (what it changed, not the branch's).
#[tauri::command]
async fn chat_turn_diff(desktop: State<'_, Desktop>, id: String, n: u32) -> Result<String, String> {
    let book = chat_book(&desktop, &id)?;
    tokio::task::spawn_blocking(move || book.diff(n)).await.map_err(|e| e.to_string())?
}

/// The person's click: the files back as they were before turn `n` (itself kept as a turn); what was done.
#[tauri::command]
async fn chat_turn_restore(desktop: State<'_, Desktop>, id: String, n: u32) -> Result<String, String> {
    let book = chat_book(&desktop, &id)?;
    let said = tokio::task::spawn_blocking(move || checkpoints::locked(&book, |b| b.restore(n, now_ms())))
        .await
        .map_err(|e| e.to_string())??;
    applog::info("checkpoints", &format!("a chat's files were restored to before turn {n}"));
    Ok(said)
}

/// "Report a problem" (`triage`): a folder of its own with the playbook, the log, the latest hang reports and what's
/// known about this computer, and a new chat there run by `harness` (an installed agent; none: Marv), its first
/// message to type once its agent is ready. Its permission mode is Supervised: it only reads and writes its report.
#[tauri::command]
async fn start_triage(app: AppHandle, harness: Option<String>) -> Result<(Chat, String), String> {
    let desktop = app.state::<Desktop>();
    let harness = harness.filter(|h| !h.is_empty() && h != marv_web::terminal_agents::MARV);
    if let Some(name) = &harness {
        check_harness(name)?;
    }
    let terminals = app.state::<terminals::Terminals>();
    let mut agents = Vec::new();
    for agent in marv_web::terminal_agents::AGENTS {
        let found = agent.installed();
        let version = match &found {
            Some(bin) => marv_web::claude_code::version(bin).await,
            None => None,
        };
        agents.push((agent.title.to_string(), found.map(|p| p.display().to_string()), version));
    }
    let home = marv_session::paths::home_dir().display().to_string();
    let chats = desktop
        .chats()
        .iter()
        .map(|c| {
            let agent = marv_web::terminal_agents::for_chat(c.harness.as_deref()).title;
            let mode = permissions_word(marv_web::terminal_agents::Permissions::parse(c.permissions.as_deref()));
            let place = if c.folder == home {
                "Home"
            } else if c.worktree.is_some() {
                "a worktree"
            } else {
                "a project folder"
            };
            let running = if terminals.running(&format!("chat:{}", c.id)) { "running" } else { "not running" };
            format!("\"{}\" · {agent} · {mode} · {place} · {running}", marv_core::printable(&c.title))
        })
        .collect();
    let about = triage::About {
        version: app.package_info().version.to_string(),
        os: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        agents,
        sandbox: marv_web::harness_sandbox::available(),
        chats,
    };
    let dir = triage::prepare(desktop.store.data_dir(), applog::dir().as_deref(), &about, now_ms())?;
    let folder = check_root(&dir.display().to_string())?.display().to_string();
    let chat = new_chat(&desktop, folder, triage::TITLE, None, None, None, None)?;
    let word = permissions_word(marv_web::terminal_agents::Permissions::Supervised);
    let chat = desktop
        .store
        .update_chat(&chat.id, |c| (c.harness, c.triage, c.permissions) = (harness, true, Some(word)))?
        .ok_or("The new chat is gone.")?;
    applog::info("triage", &format!("a problem report started in {}", dir.display()));
    Ok((chat, triage::FIRST_MESSAGE.to_string()))
}

/// The log's recent lines, to paste into a problem report (Settings › About › Copy the log).
#[tauri::command]
fn logs_recent() -> String {
    applog::recent(400)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Marv chat's row (its door lent to the terminal's Marv, which lists only what it started) gets its terminal
    /// agent's process, measured; a row that lists that process already, and a row that's no chat, are left alone.
    #[cfg(target_os = "linux")] // memory read from `/proc`
    #[test]
    fn activity_measures_a_terminal_agent_its_door_doesnt_list() {
        use marv_kernel::activity::{AgentRow, RowKind};
        use marv_kernel::metrics::{ChildProcess, Live, ProcessKind};
        let mut agent = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let pid = agent.id();
        marv_kernel::procstats::own(pid);
        let listed = |pid| ChildProcess {
            kind: ProcessKind::Mcp,
            name: "srv".into(),
            pid,
            rss_kb: Some(1),
            cpu_pct: Some(0),
            alive: Some(true),
        };
        let row = |chat: Option<&str>, processes| AgentRow {
            name: "n".into(),
            folder: "/w".into(),
            kind: RowKind::Chat,
            chat: chat.map(String::from),
            pid: 1,
            state: None,
            title: None,
            last_active_ms: None,
            live: Live { processes, ..Default::default() },
        };
        let mut rows =
            vec![row(Some("marv"), vec![listed(4_000_000)]), row(Some("claude"), vec![listed(pid)]), row(None, Vec::new())];
        let before = rows.clone();
        let pids = vec![("marv".to_string(), pid), ("claude".to_string(), pid)];
        let mut sampler = marv_kernel::procstats::Sampler::default();
        add_terminal_processes(&mut rows, &pids, |p| sampler.sample(p, now_ms()).ok());
        let _ = agent.kill();
        let _ = agent.wait();
        let shown = &rows[0].live.processes;
        assert_eq!(shown.len(), 1, "the agent's tree, not it and its server counted twice: {shown:?}");
        assert_eq!(shown[0].pid, pid);
        assert!(shown[0].rss_kb.is_some_and(|kb| kb > 0), "its memory: {shown:?}");
        assert_eq!(rows[1..], before[1..]);
    }

    /// A team in worktrees has a board per rule: the Teams screen shows them as one, oldest first, and your post, kept
    /// once in each, once.
    #[test]
    fn a_teams_boards_merge_into_one_and_your_post_shows_once() {
        use marv_kernel::{BoardBy, BoardEntry, BoardKind};
        let entry = |id, by: BoardBy, text: &str, at_ms| BoardEntry {
            id,
            by,
            kind: BoardKind::Note,
            text: text.into(),
            paths: vec![],
            at_ms,
        };
        let chat = |n: &str| BoardBy::Chat(n.into());
        let merged = merge_boards(vec![
            (7, vec![entry(1, chat("lead"), "plan", 30), entry(2, BoardBy::Person, "hello", 50)]),
            (13, vec![entry(1, chat("worker"), "claiming a", 10), entry(2, BoardBy::Person, "hello", 50)]),
            (14, vec![entry(1, chat("worker"), "same words", 30), entry(2, BoardBy::Person, "hello", 60)]),
        ]);
        let said: Vec<(&str, &str)> = merged.iter().map(|e| (e.by.as_str(), e.text.as_str())).collect();
        assert_eq!(said, [("worker", "claiming a"), ("lead", "plan"), ("worker", "same words"), ("", "hello"), ("", "hello")]);
        assert_eq!(merged[3].refs, [(7, 2), (13, 2)], "one post, kept on two boards");
        assert_eq!(merged[4].refs, [(14, 2)], "a later post of the same words is another");
        assert!(merge_boards(vec![(1, vec![])]).is_empty());
    }

    /// Marv reads its provider once, at start: a Marv chat started on Ollama before OpenRouter was set up gets a card
    /// saying so, and one still on the config's provider gets none.
    #[test]
    fn a_marv_started_before_another_provider_was_set_up_is_told_so() {
        let note = provider_change_note(Some("ollama"), Some("openrouter")).unwrap();
        assert_eq!(
            note,
            "This chat's Marv started on Ollama, before OpenRouter was set up: it stays on Ollama until it starts again."
        );
        assert!(provider_change_note(Some("openrouter"), Some("openrouter")).is_none());
        assert!(provider_change_note(None, Some("ollama")).unwrap().starts_with("This chat's Marv started on no provider"));
    }

    /// Teams and Apps need a sign-in on this computer, free or paid: none (or a broken plan.json) is refused with the
    /// sentence the page shows.
    #[test]
    fn teams_and_apps_need_an_account() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(needs_account_in(dir.path()), Err(ACCOUNT_NEEDED.to_string()));
        std::fs::write(dir.path().join("plan.json"), "not json").unwrap();
        assert!(needs_account_in(dir.path()).is_err());
        let free = plan::PlanFile {
            email: "a@b.co".into(),
            token: "t".repeat(64),
            plan: "free".into(),
            licence: String::new(),
            key_label: None,
            allowed_models: Vec::new(),
            extra: Default::default(),
        };
        plan::write_plan(dir.path(), &free).unwrap();
        assert_eq!(needs_account_in(dir.path()), Ok(()));
    }

    /// A machine where this test process runs as a Marv, with a clock moved by `sleep`.
    struct ThisMachine {
        now: std::cell::Cell<u64>,
        terminated: std::cell::RefCell<Vec<u32>>,
    }

    impl instance::System for ThisMachine {
        fn now_ms(&self) -> u64 {
            self.now.get()
        }
        fn sleep(&self, d: std::time::Duration) {
            self.now.set(self.now.get() + d.as_millis() as u64);
        }
        fn alive(&self, pid: u32) -> bool {
            pid == std::process::id() && !self.terminated.borrow().contains(&pid)
        }
        fn is_marv(&self, pid: u32, _exe: &str, _start: Option<u64>) -> bool {
            self.alive(pid)
        }
        fn terminate(&self, pid: u32) -> bool {
            self.terminated.borrow_mut().push(pid);
            true
        }
    }

    /// Kernel #41 (P1-1, P3-2), the start's glue: the watchdog is off in a debug build unless asked for and off when
    /// the person says so; a window started without it records that, and a later launch with the watchdog on never
    /// closes it, however long its record goes untouched. A window with the watchdog that stops touching its record
    /// is closed.

    #[test]
    fn a_team_chat_knows_its_own_teammates_and_not_another_folders_chat_with_the_same_role() {
        let (game, test) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let member = |id: &str, dir: &tempfile::TempDir, role: &str, team: Option<&str>| Chat {
            id: id.into(),
            folder: dir.path().display().to_string(),
            title: format!("{role} chat"),
            session_id: None,
            created: 0,
            settings: None,
            private: false,
            worktree: None,
            role: Some(role.into()),
            team: team.map(|name| store::ChatTeam { name: name.into(), roster: vec![], writes: vec![] }),
            writes: None,
            harness: None,
            agent_model: None,
            terminal_started: false,
            permissions: None,
            triage: false,
            past_agents: Vec::new(),
            delegated_by: None,
        };
        let chats = vec![
            member("11111111-1111-4111-8111-111111111111", &game, "coordinator", Some("web")),
            member("22222222-2222-4222-8222-222222222222", &game, "frontend", Some("web")),
            member("33333333-3333-4333-8333-333333333333", &test, "frontend", Some("web")),
            member("44444444-4444-4444-8444-444444444444", &game, "frontend", None),
        ];
        let peers = team_peers(&chats, &chats[0]);
        assert_eq!(peers.len(), 1, "only the same team in the same folder: {peers:?}");
        let agent = marv_kernel::AgentId::new(&chats[1].id).unwrap();
        assert_eq!(peers[0].mesh_name, marv_kernel::mesh_name_for(&game.path().canonicalize().unwrap(), &agent));
        assert!(team_peers(&chats, &chats[3]).is_empty(), "not in a team");
    }

    #[test]
    fn a_home_chat_may_run_in_the_home_folder_and_nothing_else_may() {
        let home = marv_session::paths::home_dir();
        let real = std::fs::canonicalize(&home).unwrap();
        assert_eq!(check_chat_root(&home.to_string_lossy()).unwrap(), real, "a Home chat runs in the home folder");
        assert!(check_root(&home.to_string_lossy()).is_err(), "a space, a team or a grant never does");
        assert!(check_chat_root("/").is_err(), "nothing runs above the home folder");
        if let Some(parent) = real.parent() {
            assert!(check_chat_root(&parent.to_string_lossy()).is_err());
        }
    }
    #[test]
    fn a_window_without_the_watchdog_is_never_taken_over() {
        let env = |v: Option<&'static str>| move |k: &str| (k == "MARV_WATCHDOG").then_some(v).flatten().map(String::from);
        assert!(WatchPlan::from_env(env(None), true).timings.is_none(), "a debug build: off");
        assert!(WatchPlan::from_env(env(Some("on")), true).timings.is_some());
        assert!(WatchPlan::from_env(env(None), false).timings.is_some(), "a release build: on");
        assert!(WatchPlan::from_env(env(Some("off")), false).timings.is_none());

        for watchdog in [false, true] {
            let data = std::env::temp_dir().join(format!("marv-glue-{}", uuid::Uuid::new_v4().simple()));
            let plan = WatchPlan::from_env(env(Some(if watchdog { "on" } else { "off" })), false);
            let machine = ThisMachine { now: std::cell::Cell::new(0), terminated: Default::default() };
            let (notice, own) = start_instances(&data, &plan, 0, &machine, &[]);
            assert!(notice.is_none() && own.is_some());
            // An hour later, a new launch (another pid) with the watchdog on; the record was never touched.
            machine.now.set(now_ms() + 3_600_000);
            let notice = take_over_hung_windows(&data, 3, &machine, 1, &[]);
            let record = data.join("instances").join(format!("{}.json", std::process::id()));
            if watchdog {
                assert_eq!(*machine.terminated.borrow(), [std::process::id()], "untouched with a watchdog: closed");
                assert_eq!(notice.as_deref(), Some(watchdog::TAKEN_OVER_TEXT));
                assert!(!record.exists());
                assert_eq!(hang_report::count(&data), 1);
            } else {
                assert!(machine.terminated.borrow().is_empty(), "no watchdog: never closed");
                assert!(notice.is_none() && record.exists());
                assert_eq!(hang_report::count(&data), 0);
            }
            let _ = std::fs::remove_dir_all(&data);
        }
    }

    /// Runtime-kernel §14.1 (P1): chats granted together in one folder start in their own worktrees, one beside a
    /// chat granted before does too; a chat alone (or in a sibling folder) stays; a saved conversation stays and is
    /// told; the person's "no" keeps every one in its folder; a chat already in a worktree doesn't count as sharing.
    #[test]
    fn granted_chats_sharing_a_folder_start_in_worktrees() {
        use Placement::*;
        let p = |s: &str| PathBuf::from(s);
        let fresh = |s: &str| (p(s), false, false);
        assert_eq!(placements(&[fresh("/h/acme")], &[], true), [Folder], "alone");
        assert_eq!(placements(&[fresh("/h/acme"), fresh("/h/acme")], &[], true), [Worktree, Worktree]);
        assert_eq!(placements(&[fresh("/h/acme"), fresh("/h/acme")], &[], false), [Folder, Folder], "the person's no");
        assert_eq!(placements(&[fresh("/h/acme"), fresh("/h/web")], &[], true), [Folder, Folder]);
        assert_eq!(placements(&[fresh("/h/acme/api")], &[p("/h/acme")], true), [Worktree], "beside one granted before");
        assert_eq!(placements(&[(p("/h/acme"), true, false), fresh("/h/acme")], &[], true), [Stays, Worktree]);
        assert_eq!(placements(&[(p("/h/acme"), true, true), fresh("/h/acme")], &[], true), [Folder, Folder], "in its worktree");
    }

    /// Runtime-kernel §14.1 (P1), end to end against a real repository: chats granted together in its folder with no
    /// conversation get worktrees, and the session root a load uses (`work_root`, as `spec` takes it) and the grant's
    /// folder (`works`) are the created worktree; one with a conversation, and one loaded now, stay. Taken back, the
    /// worktree and its branch are gone. A worktree whose folder is gone falls back to the folder, said.
    ///
    /// Proven: `prepare_worktrees` keeping the folder as the work folder (`work = wt.work_dir` dropped) → failed;
    /// `placements` ignoring `loaded` → failed (the loaded chat moved).
    #[tokio::test]
    async fn granted_chats_get_their_worktree_as_root_and_grant_folder() {
        let base = std::env::temp_dir().join(format!("marv-desk-wt-{}-{}", std::process::id(), uuid::Uuid::new_v4()));
        // Removed when the test ends, passed or failed: CI fails on any folder a test leaves behind.
        struct RemoveOnDrop(PathBuf);
        impl Drop for RemoveOnDrop {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = RemoveOnDrop(base.clone());
        let (repo, trees) = (base.join("repo"), base.join("trees"));
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("a.rs"), "fn main() {}\n").unwrap();
        let repo = std::fs::canonicalize(&repo).unwrap();
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap();
            assert!(ok.status.success(), "{}", String::from_utf8_lossy(&ok.stderr));
        };
        git(&["init", "-q"]);
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "start"]);
        let chat = |id: &str, session: Option<&str>| Chat {
            id: id.into(),
            folder: repo.display().to_string(),
            title: format!("chat {id}"),
            session_id: session.map(String::from),
            created: 0,
            settings: None,
            private: false,
            worktree: None,
            role: None,
            team: None,
            writes: None,
            harness: None,
            agent_model: None,
            terminal_started: false,
            permissions: None,
            triage: false,
            past_agents: Vec::new(),
            delegated_by: None,
        };
        let chats = vec![
            (chat("a", None), repo.clone()),
            (chat("b", None), repo.clone()),
            (chat("c", Some("s1")), repo.clone()),
            (chat("d", None), repo.clone()),
        ];
        let p = prepare_worktrees(&chats, &[false, false, false, true], &[], true, &trees).await;
        assert_eq!(p.made.len(), 2, "a and b move; c (a conversation) and d (loaded) stay: {:?}", p.stayed);
        assert_eq!(p.stayed, ["chat c", "chat d"]);
        for (i, (id, repo_of, wt)) in p.made.iter().enumerate() {
            assert_eq!((id.as_str(), repo_of), (["a", "b"][i], &repo));
            let dir = PathBuf::from(&wt.dir);
            assert!(dir.is_dir() && !dir.starts_with(&repo) && wt.branch.starts_with("marv/"), "{wt:?}");
            assert_eq!(p.works[i], dir, "the grant's folder is the worktree");
            let recorded = Chat { worktree: Some(wt.clone()), ..chats[i].0.clone() };
            assert_eq!(work_root(&recorded).unwrap(), (dir.canonicalize().unwrap(), None), "the session's root");
        }
        assert_eq!(p.works[2..], [repo.clone(), repo.clone()]);

        // Taken back (a grant failed): the folder and the branch go.
        let (_, _, wt) = &p.made[0];
        remove_worktree(&repo, wt);
        assert!(!PathBuf::from(&wt.dir).exists());
        let branches =
            std::process::Command::new("git").args(["branch", "--list", &wt.branch]).current_dir(&repo).output().unwrap();
        assert!(branches.stdout.is_empty(), "the branch is gone");
        // A recorded worktree whose folder is gone: the folder, and the branch named for the notice.
        let gone = Chat { worktree: Some(wt.clone()), ..chats[0].0.clone() };
        assert_eq!(work_root(&gone).unwrap(), (repo.clone(), Some(wt.branch.clone())));
    }

    /// A chat's switches list its own folder's servers and yours, and one it switched off that's gone from the config.
    #[test]
    fn a_chats_device_list_is_its_own_folders_servers() {
        let base = std::env::temp_dir().join(format!("marv-devices-{}", uuid::Uuid::new_v4().simple()));
        let (chat, other, config) = (base.join("chat"), base.join("other"), base.join("config"));
        for dir in [&chat, &other, &config] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(chat.join(".mcp.json"), r#"{"mcpServers": {"tracker": {"command": "node", "args": ["s.js"]}}}"#).unwrap();
        std::fs::write(other.join(".mcp.json"), r#"{"mcpServers": {"elsewhere": {"command": "node"}}}"#).unwrap();
        std::fs::write(config.join("mcp.json"), r#"{"mcpServers": {"Linear": {"type": "http", "url": "https://x/mcp"}}}"#)
            .unwrap();
        let env = marv_mcp::Env::new();
        let names = device_list(&chat, &config, &env, &["mcp:gone".to_string(), "github".to_string()]);
        assert_eq!(names, ["github", "herdr", "process", "host", "mcp:Linear", "mcp:gone", "mcp:tracker"]);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn fill_puts_in_only_the_given_credentials() {
        let mut entry = serde_json::json!({
            "command": "uvx", "args": ["mcparmory-google-gmail==1.0.4"],
            "env": { "OAUTH2_CLIENT_ID": "${OAUTH2_CLIENT_ID}", "OTHER": "${OTHER}" },
            "headers": { "Authorization": "Bearer ${GITHUB_TOKEN}" }
        });
        let values = [("OAUTH2_CLIENT_ID", "id.apps"), ("GITHUB_TOKEN", "pat")].map(|(k, v)| (k.to_string(), v.to_string()));
        fill(&mut entry, &values.into_iter().collect());
        assert_eq!(entry["env"]["OAUTH2_CLIENT_ID"], "id.apps");
        assert_eq!(entry["env"]["OTHER"], "${OTHER}");
        assert_eq!(entry["headers"]["Authorization"], "Bearer pat");
    }
}

#[cfg(test)]
mod live {
    /// The gallery's sources resolve (network: `cargo test -p marv-desktop -- --ignored`).
    #[tokio::test]
    #[ignore]
    async fn gallery_sources_resolve() {
        use marv_mcp::install::{AddOptions, Resolved, resolve_add};
        let env: marv_session::config::Env = std::env::vars().collect();
        let deps = marv_session::mcp_install::LiveDeps::new(env, tokio_util::sync::CancellationToken::new());
        let cases = [
            ("pypi:mcparmory-google-gmail", vec!["OAUTH2_CLIENT_ID".to_string()], vec![]),
            ("slack-workspace-mcp-server", vec!["SLACK_BOT_TOKEN".to_string()], vec![]),
            ("pypi:mcp-server-kicad", vec![], vec![]),
            (
                "https://api.githubcopilot.com/mcp/",
                vec![],
                vec![("Authorization".to_string(), "Bearer ${GITHUB_TOKEN}".to_string())],
            ),
        ];
        for (source, env, headers) in cases {
            let options =
                AddOptions { source: source.into(), env, headers: headers.into_iter().collect(), ..AddOptions::default() };
            match resolve_add(&options, &deps).await {
                Ok(Resolved::Proposal(p)) => println!("OK {source} -> {} {}", p.name, serde_json::to_string(&p.entry).unwrap()),
                Ok(Resolved::Choices(c)) => println!("CHOICES {source}: {:?}", c.choices),
                Err(e) => println!("ERR {source}: {e}"),
            }
        }
    }
}

#[cfg(test)]
mod native_dialog_tests {
    #[test]
    fn native_dialogs_only_on_wayland_or_x11() {
        use super::native_dialogs;
        for yes in [None, Some("wayland"), Some("x11"), Some("wayland,x11"), Some("*")] {
            assert!(native_dialogs(yes), "{yes:?}");
        }
        if cfg!(target_os = "linux") {
            assert!(!native_dialogs(Some("broadway")));
        }
    }
}

#[cfg(test)]
mod stay_in_the_app_tests {
    use super::in_the_app;

    fn url(s: &str) -> tauri::Url {
        s.parse().unwrap()
    }

    #[test]
    fn the_app_s_own_pages_stay_and_everything_else_goes_to_the_browser() {
        for own in ["tauri://localhost/", "http://tauri.localhost/index.html", "ipc://localhost/x", "about:blank"] {
            assert!(in_the_app(&url(own)), "{own}");
        }
        for away in ["http://localhost:3002/", "https://example.com/", "http://127.0.0.1:8080/", "file:///etc/passwd"] {
            assert!(!in_the_app(&url(away)), "{away}");
        }
    }
}
