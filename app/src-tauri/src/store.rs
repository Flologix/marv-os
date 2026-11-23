//! What Marv desktop remembers: the user's root folder and key (`settings.json`, 0600, in the app's config folder)
//! and the chats (`chats.json`, in its data folder): which folder each one runs in and the Marv session it
//! continues.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// The active space's top folder (kept equal to it: what the tree and chats use).
    pub root: Option<PathBuf>,
    /// Spaces: each a top folder, its own chats (by folder) and the apps (MCP servers) switched off in it.
    #[serde(default)]
    pub spaces: Vec<Space>,
    #[serde(default)]
    pub active: Option<String>,
    /// "openrouter", "ollama", "zai", "kimi" or "chatgpt" (#159), as chosen at onboarding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The model a new chat starts on, as onboarding or Settings → Model last picked. The page keeps it (its
    /// `marv-settings`) and sends each pick here (`set_default_model`), so a chat started with no settings of its own
    /// (the first chat's terminal) can name it to Marv.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The OpenRouter key, when the user pasted one here (else `OPENROUTER_API_KEY` or `marv`'s own config).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// #159: the Z.ai Coding Plan key, when the user pasted one here (else `ZAI_API_KEY` or `marv`'s own config).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zai_api_key: Option<String>,
    /// #159: the Kimi Code plan key, when the user pasted one here (else `KIMI_API_KEY` or `marv`'s own config).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kimi_api_key: Option<String>,
    /// The harness plan, phase 4: harness chats (Claude Code) run inside Marv's outer sandbox. None: on (the default);
    /// `false` only when the person turned it off in Settings → Harnesses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness_sandbox: Option<bool>,
    /// The permission mode a new chat starts with (`Chat::permissions`), from Settings. None: each agent's own settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_permissions: Option<String>,
    /// The first run's "Sign in to Marv" step was answered (signed in or skipped): it doesn't show again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub account_asked: bool,
    /// The update notice (`update.rs`). None: on (the default); `false` only when the person turned it off in
    /// Settings › About.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_check: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Space {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
    /// MCP servers switched off in this space, by name (a new server is on everywhere).
    #[serde(default)]
    pub apps_off: Vec<String>,
}

impl Settings {
    /// Settings from before spaces: the root becomes the first space.
    fn migrate(&mut self) {
        if self.spaces.is_empty()
            && let Some(root) = &self.root
        {
            let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Projects".into());
            self.spaces.push(Space { id: "main".into(), name, root: root.clone(), apps_off: Vec::new() });
            self.active = Some("main".into());
        }
    }

    pub fn active_space(&self) -> Option<&Space> {
        self.spaces.iter().find(|s| Some(&s.id) == self.active.as_ref()).or(self.spaces.first())
    }

    pub fn active_space_mut(&mut self) -> Option<&mut Space> {
        let id = self.active_space()?.id.clone();
        self.spaces.iter_mut().find(|s| s.id == id)
    }

    /// The active space's top folder, now `root`, in both places.
    pub fn set_root(&mut self, root: PathBuf) {
        match self.active_space_mut() {
            Some(space) => space.root = root.clone(),
            None => {
                let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Projects".into());
                self.spaces.push(Space { id: "main".into(), name, root: root.clone(), apps_off: Vec::new() });
                self.active = Some("main".into());
            }
        }
        self.root = Some(root);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Chat {
    pub id: String,
    /// The folder Marv works in, as a full path: the tree's top folder can move (↑, going into a folder) without
    /// losing a chat's place. (Chats saved before were relative to the root: [`crate::Desktop::chats`] converts them.)
    pub folder: String,
    pub title: String,
    /// Marv's saved session, once the chat has one (`ready`'s `sessionId`).
    #[serde(default)]
    pub session_id: Option<String>,
    /// Seconds since the epoch.
    pub created: u64,
    /// How the person last ran it (runtime-kernel 1): a message that wakes it while no page shows it loads it like
    /// this. None until it's first opened (and in files from before).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<ChatSettings>,
    /// "Private: other chats can't read it" (runtime-kernel §9.2, R3): no `chat_read` is served for it, whatever the
    /// rules. Only the person sets it, from the chat's menu. Off in files from before.
    #[serde(default)]
    pub private: bool,
    /// Runtime-kernel §14.1 (P1): the git worktree it works in, when it was granted to keep working while the person
    /// is away beside another granted chat in its folder. Its session runs there; it stays listed (and on the mesh, for
    /// the rules) under `folder`. The person merges the branch. None in files from before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<ChatWorktree>,
    /// The chat's role (`marv_session::roles`), picked when the person made it and fixed for its life: every load of
    /// its session starts as it. None: no role (and files from before).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// The team it was started in by the New team dialog (`crate::teams`): the sidebar groups it under the team, and
    /// its session gets the team's brief after its role. "End team" clears it. None: not in a team.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<ChatTeam>,
    /// The write paths the person gave it when its team started (`marv_session::teams::member_role`): its write set
    /// for its whole life, in place of its role's. None: its role's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writes: Option<Vec<String>>,
    /// What runs it (`marv_web::host::Harness`): `"claude-code"` for the person's own Claude Code, picked when the chat
    /// was made and fixed for its life. None: Marv (and files from before).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// An agent chat's model, as the person last picked it in the chat (its agent's own id, `sonnet`,
    /// `opencode/big-pickle`…): the agent starts with it. None: the agent's own default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_model: Option<String>,
    /// Its agent has run in its terminal: the next start continues its conversation (an agent that keeps its own
    /// session ids continues by `session_id` instead). False in files from before.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub terminal_started: bool,
    /// How much its agent may do without asking (`marv_web::terminal_agents::Permissions`: `supervised`,
    /// `auto-edits`, `auto`, `full`), as the person last set it; applied when its terminal starts. None: the agent's
    /// own settings (and files from before).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<String>,
    /// Made by "Report a problem" (`crate::triage`): it works in a report's own folder, outside every space, and the
    /// sidebar lists it with the Home chats. False in files from before.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub triage: bool,
    /// The agents it ran before a switch (`crate::handoff`), each with its own conversation: switching back to one
    /// continues its session. Empty in files from before.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub past_agents: Vec<PastAgent>,
    /// The chat whose agent started it with `delegate` (`crate::delegates`): it works unattended on the task it was
    /// given, and can't delegate in turn. None: the person made it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_by: Option<String>,
}

/// The team a chat was started in (`marv_session::teams`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatTeam {
    pub name: String,
    /// The members' roles as the team started, coordinator first.
    #[serde(default)]
    pub roster: Vec<String>,
    /// The write paths typed for each member, in the roster's order (None: its role's own). Empty for a team started
    /// before this was kept.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writes: Vec<Option<Vec<String>>>,
}

/// An agent a chat ran before it was switched to another: its conversation, to continue when it's switched back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PastAgent {
    /// Its id (`marv_web::terminal_agents::Agent::id`).
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub terminal_started: bool,
}

/// A chat's own worktree (runtime-kernel §14.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatWorktree {
    /// Where its session works (the worktree, at the chat folder's place in the repository), a full path.
    pub dir: String,
    /// Its branch (`marv/<title>-<id>`).
    pub branch: String,
    /// The short hash it started from.
    pub base: String,
    /// The worktree's own top folder (`dir` is it, or a folder inside it); empty in records from before (then `dir`).
    #[serde(default)]
    pub top: String,
}

/// A chat's settings as the page last set them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSettings {
    pub model: String,
    pub max_steps: u32,
    pub yolo: bool,
    /// MCP servers switched off for it, by name.
    #[serde(default)]
    pub mcp_off: Vec<String>,
    /// Devices switched off for it (runtime-kernel §17.1, D1): `mcp:<server>`, `github`, `process`, `host`, `herdr`.
    /// Read when its session starts: a change applies at its next boot.
    #[serde(default)]
    pub devices_off: Vec<String>,
}

/// One lock for every load-modify-save of the app's files: chats are changed from the page's commands, the terminals'
/// threads and the mesh at once, and each change reads the whole file and writes it back whole.
static FILES: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    FILES.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn read<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> T {
    let Ok(text) = std::fs::read_to_string(path) else { return T::default() };
    match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(e) => {
            // A file that is there but doesn't parse (torn by a crash, edited by hand) is kept aside: reading it as
            // "nothing" and then saving would make the loss permanent.
            let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            let mut aside = path.as_os_str().to_owned();
            aside.push(format!(".corrupt-{secs}"));
            // Moved, not copied: the file is read every few seconds, and each read would keep another copy.
            let kept = std::fs::rename(path, &aside).is_ok();
            let note = if kept { format!("kept as {}", Path::new(&aside).display()) } else { "couldn't be kept".into() };
            crate::applog::warn("store", &format!("{} doesn't parse ({e}); starting empty, the file {note}", path.display()));
            T::default()
        }
    }
}

fn write(path: &Path, value: &impl Serialize, private: bool) -> Result<(), String> {
    use std::io::Write;
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    // Written whole to its own temp file, synced, and renamed over, so a crash never leaves half a file.
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}-{n}.tmp", std::process::id()));
    let temp = path.with_file_name(name);
    let result = (|| {
        let mut file = std::fs::File::create(&temp).map_err(|e| e.to_string())?;
        file.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        if private {
            marv_fs::mode::set_mode(&temp, 0o600).map_err(|e| e.to_string())?;
        }
        std::fs::rename(&temp, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

pub struct Store {
    config: PathBuf,
    data: PathBuf,
}

impl Store {
    pub fn new(config: PathBuf, data: PathBuf) -> Self {
        Self { config, data }
    }

    pub fn settings(&self) -> Settings {
        let mut settings: Settings = read(&self.config.join("settings.json"));
        settings.migrate();
        // Windows: roots saved verbatim (`\\?\C:\...`, from `canonicalize`) read back in the form chats' folders now use.
        let standard = |p: &Path| PathBuf::from(marv_session::paths::standard_folder(&p.to_string_lossy()));
        settings.root = settings.root.as_deref().map(standard);
        settings.spaces.iter_mut().for_each(|s| s.root = standard(&s.root));
        // The active space wins: `root` is only its mirror.
        if let Some(root) = settings.active_space().map(|s| s.root.clone()) {
            settings.root = Some(root);
        }
        settings
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<(), String> {
        let _guard = lock();
        write(&self.config.join("settings.json"), settings, true)
    }

    /// Reads the settings, applies `change` and saves them, under the lock: two changes at once both land.
    pub fn modify_settings<R>(&self, change: impl FnOnce(&mut Settings) -> R) -> Result<R, String> {
        let _guard = lock();
        let mut settings = self.settings();
        let out = change(&mut settings);
        write(&self.config.join("settings.json"), &settings, true)?;
        Ok(out)
    }

    /// The agents the person added in Settings > Agents (`marv_web::terminal_agents::OwnAgent`).
    pub fn own_agents(&self) -> Vec<marv_web::terminal_agents::OwnAgent> {
        read(&self.config.join("agents.json"))
    }

    pub fn save_own_agents(&self, agents: &[marv_web::terminal_agents::OwnAgent]) -> Result<(), String> {
        let _guard = lock();
        write(&self.config.join("agents.json"), &agents, true)
    }

    /// The chats, each folder in its standard form (Windows: one saved verbatim, `\\?\C:\...`, matches its folder again).
    pub fn chats(&self) -> Vec<Chat> {
        let mut chats: Vec<Chat> = read(&self.data.join("chats.json"));
        if cfg!(windows) {
            chats.iter_mut().for_each(|c| c.folder = marv_session::paths::standard_folder(&c.folder));
        }
        chats
    }

    #[cfg(test)]
    pub fn save_chats(&self, chats: &[Chat]) -> Result<(), String> {
        let _guard = lock();
        write(&self.data.join("chats.json"), &chats, false)
    }

    /// Reads the chats, applies `change` and saves them, all under the lock (new, remove and edit from several
    /// threads at once all land). `change` returns what the caller wants back.
    pub fn modify_chats<R>(&self, change: impl FnOnce(&mut Vec<Chat>) -> R) -> Result<R, String> {
        let _guard = lock();
        let mut chats = self.chats();
        let out = change(&mut chats);
        write(&self.data.join("chats.json"), &chats, false)?;
        Ok(out)
    }

    /// The app's own data folder (chats, hang reports, problem reports).
    pub fn data_dir(&self) -> &Path {
        &self.data
    }

    pub fn update_chat(&self, id: &str, change: impl FnOnce(&mut Chat)) -> Result<Option<Chat>, String> {
        let _guard = lock();
        let mut chats = self.chats();
        let Some(chat) = chats.iter_mut().find(|c| c.id == id) else { return Ok(None) };
        change(chat);
        let chat = chat.clone();
        write(&self.data.join("chats.json"), &chats, false)?;
        Ok(Some(chat))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_root_from_before_spaces_becomes_the_first_space() {
        let mut settings = Settings { root: Some("/home/u/Projects".into()), ..Settings::default() };
        settings.migrate();
        assert_eq!(settings.spaces.len(), 1);
        assert_eq!((settings.spaces[0].name.as_str(), settings.active.as_deref()), ("Projects", Some("main")));
        // Moving the top folder moves the active space's.
        settings.set_root("/home/u/Work".into());
        assert_eq!(settings.active_space().unwrap().root, PathBuf::from("/home/u/Work"));
        assert_eq!(settings.root, Some(PathBuf::from("/home/u/Work")));
    }

    #[test]
    fn a_chats_last_settings_are_kept_and_a_chat_from_before_has_none() {
        let old: Vec<Chat> =
            serde_json::from_str(r#"[{"id": "c", "folder": "/p", "title": "t", "sessionId": "s", "created": 1}]"#).unwrap();
        assert_eq!(old[0].settings, None);
        assert!(!serde_json::to_string(&old).unwrap().contains("settings"), "written back as it was");
        let dir = std::env::temp_dir().join(format!("marv-desktop-store-{}", std::process::id()));
        let store = Store::new(dir.join("config"), dir.join("data"));
        store.save_chats(&old).unwrap();
        let settings = ChatSettings {
            model: "z-ai/glm-5".into(),
            max_steps: 40,
            yolo: false,
            mcp_off: vec!["gmail".into()],
            devices_off: vec!["github".into()],
        };
        store.update_chat("c", |c| c.settings = Some(settings.clone())).unwrap();
        assert_eq!(store.chats()[0].settings, Some(settings));
        let _ = std::fs::remove_dir_all(dir);
    }

    fn chat(id: &str) -> Chat {
        serde_json::from_str(&format!(r#"{{"id": "{id}", "folder": "/p", "title": "t", "created": 1}}"#)).unwrap()
    }

    #[test]
    fn a_file_that_does_not_parse_is_kept_aside_not_lost() {
        let dir = std::env::temp_dir().join(format!("marv-desktop-store-corrupt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("data")).unwrap();
        std::fs::write(dir.join("data/chats.json"), "[{\"id\": \"c\", \"fol").unwrap();
        let store = Store::new(dir.join("config"), dir.join("data"));
        assert!(store.chats().is_empty());
        let kept: Vec<_> = std::fs::read_dir(dir.join("data"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("chats.json.corrupt-"))
            .collect();
        assert_eq!(kept.len(), 1);
        assert_eq!(std::fs::read_to_string(kept[0].path()).unwrap(), "[{\"id\": \"c\", \"fol");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn chats_added_from_many_threads_all_land() {
        let dir = std::env::temp_dir().join(format!("marv-desktop-store-threads-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = std::sync::Arc::new(Store::new(dir.join("config"), dir.join("data")));
        let threads: Vec<_> = (0..16)
            .map(|i| {
                let store = store.clone();
                std::thread::spawn(move || store.modify_chats(|chats| chats.push(chat(&format!("c{i}")))).unwrap())
            })
            .collect();
        threads.into_iter().for_each(|t| t.join().unwrap());
        assert_eq!(store.chats().len(), 16);
        let _ = std::fs::remove_dir_all(dir);
    }
}
