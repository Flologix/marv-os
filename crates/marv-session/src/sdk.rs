//! The library entry point: a session without a terminal. Ports Marv's `src/sdk.ts` (`createSession`) as a builder:
//!
//! ```ignore
//! let session = SessionBuilder::new("/path/to/project", ProviderOption::OpenRouter { api_key, model: None, base_url: None })
//!     .sources(&[Source::User, Source::Project])
//!     .approve(my_approver)
//!     .build()
//!     .await?;
//! let mut turn = session.send("fix the failing test", SendOptions::default())?;
//! while let Some(event) = turn.next().await { /* … */ }
//! ```
//!
//! It reads nothing from disk unless asked (`sources`). Without an approver only what yolo vouches for runs (reads,
//! edits outside `.git`, sandboxed commands without network); anything else goes back to the model as refused.
//! Checked before anything starts:
//! - `cwd` is a folder;
//! - with yolo on (the default) it isn't the home folder, above it, or `/`: yolo-safe edits run before any approver
//!   is asked, so they'd change dotfiles unasked. With `yolo(false)` it's allowed, and `no_yolo` keeps it off;
//! - `resume` needs `persist`.
//!
//! `mcp_servers` given in code are trusted like your own (no trust prompt, started in your home folder,
//! `${MARV_PROJECT_DIR}` points at the project) and win a name clash with the config files. Never pass config read
//! from a repository you don't control: that's what `Source::Project` (still trust-gated) is for. The session is
//! built before the MCP servers start (its constructor can fail), and everything is closed if anything after fails.
//! The servers start in the background (`McpManager::start`); the first turn waits for them.
//! Env vars (`OPENROUTER_API_KEY`, `MARV_MODEL`) are ignored: a library uses what it's given.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use marv_mcp::deferred::McpToolsSetting;
use marv_mcp::manager::{CONNECT_TIMEOUT, LOGIN_TIMEOUT, ManagerOptions, McpManager, McpState};
use marv_mcp::oauth_store::McpTokenStore;
use marv_mcp::trust::McpTrust;
use marv_provider::ProviderOption;
use marv_tools::{Approver, ToolRef};
use serde_json::Value;

use crate::config::{Env, default_config_dir};
use crate::paths::{home_dir, home_or_above, project_key};
use crate::session::{MarvSession, ProviderChoice, ResumeTarget, SessionError, SessionInit, SystemPrompt, TranscriptFn, Warn};
use crate::sessions::SessionStore;
use crate::sources::{Source, load_sources};
use crate::trajectory::TrajectoryStore;
use marv_tools::formatter::FormatterSetting;

/// `true` → the default store; a store → that one; `false`/absent → none.
#[derive(Clone, Default)]
pub enum Store<T> {
    #[default]
    Off,
    Default,
    Given(Arc<T>),
}

/// What `createSession` takes (Marv's `SessionOptions`).
#[derive(Clone)]
pub struct SessionOptions {
    pub cwd: PathBuf,
    pub provider: ProviderOption,
    pub sources: Vec<Source>,
    pub system_prompt: Option<SystemPrompt>,
    /// When the run must be done (`marv -p --time-limit`); see `SessionInit::deadline`.
    pub deadline: Option<std::time::Instant>,
    /// See `SessionInit::files_anywhere`.
    pub files_anywhere: bool,
    /// Offered to the main agent next to the built-in ones; names must be their own.
    pub tools: Vec<ToolRef>,
    /// `.mcp.json`'s format, `{ name: entry }`, in order.
    pub mcp_servers: Vec<(String, Value)>,
    /// Servers from the config files (or `mcp_servers`) this session leaves out, by name: switched off for one
    /// conversation without touching the files.
    pub mcp_skip: Vec<String>,
    /// Devices switched off for this chat (runtime-kernel §17.1, D1: Marv desktop's per-chat switch), by their
    /// `boot::devices` names: `mcp:<server>` (not started), `github`, `herdr`, `process`, `host` (not offered). Read
    /// once, at the session's start: a device set is fixed for a boot. Default none.
    pub devices_off: Vec<String>,
    pub approve: Option<Arc<dyn Approver>>,
    pub max_steps: Option<u32>,
    /// The person's `smallModel` ([`SessionInit::small_model`]). Default none: the chat's model.
    pub small_model: Option<String>,
    /// marv-rs: `visionModel` ([`crate::session::SessionInit::vision_model`]).
    pub vision_model: Option<String>,
    /// Another provider's cores ([`SessionInit::cores`]). Default none: every core is on the session's provider.
    pub cores: Option<crate::cores::CoreMaker>,
    /// Offer the plan tool: a checklist the model keeps for a long task (`session.plan()` reads it, each change also
    /// arrives as a tool end's `result.plan`). Default true. Fixed for the session, since it changes the tool list.
    pub plan: bool,
    /// Which languages' checkers run after the model's edits (#187); `false.into()` turns them all off.
    pub diagnostics: crate::config::Diagnostics,
    pub checks: bool,
    /// `compactTokens` (#140): compact at this many tokens of context, or at 85% of the window if sooner; `None`: off.
    pub compact_tokens: Option<u64>,
    /// `compaction` (#63): v1 (default) or v2; temporary, for the A/B.
    pub compaction: marv_agent::compact::Version,
    /// `compactTailTokens` (#63): v2's verbatim tail; `None`: the default.
    pub compact_tail_tokens: Option<u64>,
    /// The formatter the edit tools run after a change (#67). Default on (detect from the project's config).
    pub formatter: FormatterSetting,
    /// Before a turn's first change, snapshot the project (git commits under a hidden ref,
    /// `refs/marv/checkpoints/<session>`, or a git dir of Marv's own in `config_dir` outside a repository), and again
    /// when it ends, so `session.undo_plan()` and `session.undo(plan)` can put that turn's files back. Default true.
    pub checkpoints: bool,
    pub mcp_tools: McpToolsSetting,
    pub sandbox: bool,
    /// Let bash run a command on the host, outside the sandbox (`sandbox: false`), when your approver says yes. Asked
    /// every time (the request has `door: Host` and `once`; an "always" counts as one yes). Default false: such calls
    /// are refused before your approver is asked (Marv #52).
    pub host_commands: bool,
    /// Offer the github tool (gh with the user's login, outside the sandbox) when gh is on PATH and logged in,
    /// checked once here. Reads run without asking; writes and sensitive API reads go to your approver (`door:
    /// Github`, scope `github:<command>`); what can never run (auth, config, extensions, --web, another host…) is
    /// refused. Default false (Marv #47).
    pub github: bool,
    /// Whether gh is there and logged in, when the caller already asked (`marv -p` asks before it joins the mesh);
    /// None: asked here, when `github` is set.
    pub github_known: Option<bool>,
    /// Offer `web_fetch` (`SessionInit::web`). Default true; `marv -p` turns it off when nothing approves the web
    /// scope, since every fetch would be refused there.
    pub web: bool,
    pub yolo: bool,
    pub thinking: bool,
    /// `reasoningEffort` (#143): how hard an OpenRouter reasoning model reasons; `None`: the model's default.
    pub reasoning_effort: Option<marv_provider::ReasoningEffort>,
    pub persist: Store<SessionStore>,
    pub trajectories: Store<TrajectoryStore>,
    pub transcript: Option<TranscriptFn>,
    /// Continue a saved session (needs `persist`). `Latest` with nothing saved starts a new one; an unknown id fails.
    pub resume: Option<ResumeTarget>,
    /// Where Marv keeps its files (default `~/.marv`, or `MARV_CONFIG_DIR` from the real environment).
    pub config_dir: Option<PathBuf>,
    pub on_warning: Option<Warn>,
    /// The process's switchboard (marv-rs mesh; `crate::mesh::start`), and who grants its wires.
    pub mesh: Option<crate::mesh::MeshHandle>,
    pub wire_approver: Option<Arc<dyn crate::mesh::WireApprover>>,
    /// Offer the process tool (Marv #51): a dev server or watcher that keeps running in its own sandbox, its ports
    /// forwarded to 127.0.0.1, each start through your approver (`door: process`; scope `process:<command>`). Default
    /// true, where bubblewrap works. `close()` stops them all.
    pub processes: bool,
    /// Pre-approved scopes (#98): `--approve`-style names ("web", "network"…) that seed the session's "always"
    /// scopes, and are listed in the prompt. A subagent inherits the egress ones never. Default: none.
    pub pre_approved: Vec<String>,
    /// Extra writable sandbox folders (#98), e.g. bd's database folder: canonical paths, already checked
    /// (`marv_sandbox::resolve_writable`), bound read-write into every command's sandbox. Default: none.
    pub writable: Vec<PathBuf>,
    /// Session-start hooks (#108): commands run as each conversation begins, outside the sandbox, their output sent
    /// once at the start of its first message. Pass only commands the user wrote into their own config
    /// (`crate::hooks::session_start_hooks`), never a repository's. Default none.
    pub hooks: Vec<crate::hooks::HookCommand>,
    /// A one-shot run, like `marv -p`: the process tool tells the model its processes end when the run does (#86).
    /// Default false.
    pub one_shot: bool,
    /// Offer the herdr tool (#74) for a session running in a Herdr pane: reads run at once (fenced, tainting); focus,
    /// rename and prompts go to your approver (`door: Herdr`); commands and spawns only with `host_commands`, asked
    /// every time (`once`). Default none.
    pub herdr: Option<crate::herdr_door::HerdrSetup>,
    /// Start tainted (#74, `MARV_TAINTED=1`): this session's first message came from a conversation that held
    /// untrusted text. Default false.
    pub tainted_at_start: bool,
    /// The "always" scopes and the mesh turn hop to share across loads of one chat (runtime-kernel 1:
    /// [`SessionInit::always`], [`SessionInit::mesh_hop`]). Default none: the session's own.
    pub always: Option<crate::runner::AlwaysScopes>,
    pub mesh_hop: Option<Arc<marv_mesh::hop::TurnHop>>,
    /// The file versions to keep across loads of one chat ([`SessionInit::file_versions`]). Default none.
    pub file_versions: Option<Arc<marv_tools::FileVersions>>,
    /// The chat's service grant ([`SessionInit::grant`]): the desktop's, for a chat the person granted. Default none.
    pub grant: Option<Arc<dyn crate::grants::ChatGrant>>,
    /// Runtime-kernel §16: the meters and budgets ([`SessionInit::meters`], `meter_chat`, `on_budget`). Never kept for a
    /// one-shot run. Default none.
    pub meters: Option<Arc<crate::allocator::MeterBook>>,
    pub meter_chat: Option<String>,
    pub on_budget: Option<crate::allocator::OnBudget>,
    /// [`SessionInit::budget_asks`]. Default true.
    pub budget_asks: bool,
    /// Offer the main agent `chat_read` ([`SessionInit::chat_read`]): only a host that honors the person's rules (Marv
    /// desktop's chats). Default false.
    pub chat_read: bool,
    /// With the `Project` source: whether the project's instruction files, skills and agent types are used as the
    /// person's own (`crate::project_trust`). `None` (the default): only when the person trusted exactly these files
    /// here (stored beside MCP trust); `Some(true)`: trusted for this session (`marv -p --trust-project`), nothing
    /// stored; `Some(false)`: never. Not trusted, the instruction files are quoted to the model as the project's data,
    /// and its skills and agent types aren't loaded.
    pub trust_project: Option<bool>,
    /// Offer the main agent `wake_me` ([`SessionInit::timers`]): Marv desktop's chats only. Default none.
    pub timers: Option<Arc<dyn crate::timers::SelfTimers>>,
    /// Say this session's own state on its door ([`SessionInit::door_state`]): a terminal-like host (the TUI) with no
    /// scheduler of its own. Default false.
    pub door_state: bool,
    /// The chat's role by name ([`SessionInit::role`], `crate::roles`): one of the built-in roles, the person's
    /// (`User` source) or the project's (`Project` source, once trusted). An unknown name fails the build. Default none.
    pub role: Option<String>,
    /// The team the chat was started in, by name ([`SessionInit::team`], `crate::teams`): its brief follows the role's
    /// section. A team no longer there (its file removed, the project no longer trusted) is left out: the chat still
    /// starts, with its role. Default none.
    pub team: Option<String>,
    /// The team's members' roles as it was started (the desktop's dialog may add or drop members): replaces the
    /// file's in the brief. A team named here but not found (a template renamed in the dialog) still gets a brief with
    /// its name and these roles, without a file's words. Default empty: the file's members.
    pub team_roster: Vec<String>,
    /// The write paths the person typed for each member of `team_roster` (same order; None: the role's own), so the
    /// brief's "Who can do what" lines are each member's effective role. Empty: not known, the roles' own.
    pub team_writes: Vec<Option<Vec<String>>>,
    /// The team's other chats (role, name, mesh name), so its brief names their inboxes (`crate::teams::TeamPeer`).
    /// Empty: not known.
    pub team_peers: Vec<crate::teams::TeamPeer>,
    /// The write paths the person typed for this chat when its team started (`crate::teams::member_role`): they
    /// replace its role's write set. Ignored without a role; one that would widen a role that writes nothing fails
    /// the build. Default none.
    pub role_writes: Option<Vec<String>>,
}

/// How an SDK session's notice says a project's instructions get trusted.
pub const TRUST_HINT: &str = "Trusting them (/trust in the TUI, or --trust-project for marv -p)";

/// Builds a [`MarvSession`] (Marv's `createSession`).
pub struct SessionBuilder {
    pub options: SessionOptions,
}

impl SessionBuilder {
    pub fn new(cwd: impl Into<PathBuf>, provider: ProviderOption) -> Self {
        Self {
            options: SessionOptions {
                cwd: cwd.into(),
                provider,
                sources: Vec::new(),
                system_prompt: None,
                deadline: None,
                files_anywhere: false,
                tools: Vec::new(),
                mcp_servers: Vec::new(),
                mcp_skip: Vec::new(),
                devices_off: Vec::new(),
                approve: None,
                max_steps: None,
                small_model: None,
                vision_model: None,
                cores: None,
                plan: true,
                diagnostics: crate::config::Diagnostics::ON,
                checks: true,
                compact_tokens: Some(crate::config::DEFAULT_COMPACT_TOKENS),
                compaction: marv_agent::compact::Version::V1,
                compact_tail_tokens: None,
                formatter: FormatterSetting::Auto,
                checkpoints: true,
                mcp_tools: McpToolsSetting::Auto,
                sandbox: true,
                host_commands: false,
                github: false,
                github_known: None,
                web: true,
                yolo: true,
                thinking: false,
                reasoning_effort: None,
                persist: Store::Off,
                trajectories: Store::Off,
                transcript: None,
                resume: None,
                config_dir: None,
                on_warning: None,
                mesh: None,
                wire_approver: None,
                processes: true,
                pre_approved: Vec::new(),
                writable: Vec::new(),
                hooks: Vec::new(),
                one_shot: false,
                herdr: None,
                tainted_at_start: false,
                always: None,
                file_versions: None,
                mesh_hop: None,
                grant: None,
                meters: None,
                meter_chat: None,
                on_budget: None,
                budget_asks: true,
                chat_read: false,
                trust_project: None,
                timers: None,
                door_state: false,
                role: None,
                team: None,
                team_roster: Vec::new(),
                team_writes: Vec::new(),
                team_peers: Vec::new(),
                role_writes: None,
            },
        }
    }

    pub fn sources(mut self, sources: &[Source]) -> Self {
        self.options.sources = sources.to_vec();
        self
    }
    /// The client's approver. A request with `must_ask` (tainted egress, a protected-path write) is one a person
    /// must answer every time: an approver that answers from a policy instead (as `marv -p --approve` does) should
    /// refuse it unless its user opted in (`--approve-tainted`, `--approve-protected`).
    pub fn approve(mut self, approver: Arc<dyn Approver>) -> Self {
        self.options.approve = Some(approver);
        self
    }
    pub fn yolo(mut self, on: bool) -> Self {
        self.options.yolo = on;
        self
    }
    pub fn persist(mut self, store: Store<SessionStore>) -> Self {
        self.options.persist = store;
        self
    }
    pub fn trajectories(mut self, store: Store<TrajectoryStore>) -> Self {
        self.options.trajectories = store;
        self
    }
    pub fn resume(mut self, target: ResumeTarget) -> Self {
        self.options.resume = Some(target);
        self
    }
    pub fn processes(mut self, on: bool) -> Self {
        self.options.processes = on;
        self
    }
    /// Pre-approved scopes (#98): `--approve` names ("web", "network"…) seeded as "always" for the session.
    pub fn pre_approve(mut self, names: &[String]) -> Self {
        self.options.pre_approved = names.to_vec();
        self
    }
    /// Extra writable sandbox folders (#98), already checked (`marv_sandbox::resolve_writable`).
    pub fn writable(mut self, folders: Vec<PathBuf>) -> Self {
        self.options.writable = folders;
        self
    }
    pub fn one_shot(mut self, on: bool) -> Self {
        self.options.one_shot = on;
        self
    }
    pub fn max_steps(mut self, steps: u32) -> Self {
        self.options.max_steps = Some(steps);
        self
    }
    /// `false` leaves the plan tool out (#15; the eval's "off" arm).
    pub fn plan(mut self, on: bool) -> Self {
        self.options.plan = on;
        self
    }

    /// Checks the options, loads the sources, makes the session, starts its MCP servers, resumes.
    pub async fn build(self) -> Result<MarvSession, SessionError> {
        create_session(self.options).await
    }
}

/// Why yolo isn't allowed in `root`, or `None` (the SDK's `noYolo`).
pub fn no_yolo_reason(root: &Path, home: &Path) -> Option<String> {
    if !home_or_above(root, home) {
        return None;
    }
    let where_ = if root == Path::new("/") { "the root of the file system" } else { "your home folder (or above it)" };
    Some(format!(
        "cwd {} is {where_}: with yolo on, Marv would change files there without asking, dotfiles included. Use a project folder, or pass yolo: false (then every change goes to your approver, or is refused without one). There, sandboxed commands see the home read-only, with Marv's config, credentials and agent sockets hidden.",
        root.display()
    ))
}

fn store_for<T>(option: &Store<T>, make: impl FnOnce() -> T) -> Option<Arc<T>> {
    match option {
        Store::Off => None,
        Store::Default => Some(Arc::new(make())),
        Store::Given(t) => Some(t.clone()),
    }
}

/// Marv's `createSession`. See the module docs for what's checked.
pub async fn create_session(options: SessionOptions) -> Result<MarvSession, SessionError> {
    // The real path without Windows' `\\?\` (`real_folder`; the same as canonicalize elsewhere).
    let root = crate::paths::real_folder(&options.cwd).unwrap_or_else(|_| options.cwd.clone());
    if !root.is_dir() {
        return Err(SessionError::Invalid(format!("cwd {} isn't a folder", root.display())));
    }
    let home = home_dir();
    let no_yolo = no_yolo_reason(&root, &home);
    if let Some(why) = &no_yolo
        && options.yolo
    {
        return Err(SessionError::Invalid(why.clone()));
    }
    if options.resume.is_some() && matches!(options.persist, Store::Off) {
        return Err(SessionError::Invalid("resume needs persist (there's nowhere to resume from)".into()));
    }
    // The real environment only: never a project file (docs/SECURITY.md).
    let env: Env = std::env::vars().collect();
    let config_dir = options.config_dir.clone().unwrap_or_else(|| default_config_dir(&env, &home));
    // A project's instructions are the person's only once trusted (docs/SECURITY.md): a cloned repository's aren't.
    let loaded = load_sources(&root, &options.sources, &config_dir, &home, &env, options.trust_project).await;
    let trust_notice = match &loaded.project_status {
        Some(status) if !loaded.trusted => crate::project_trust::untrusted_notice(status, &loaded.project_files, TRUST_HINT),
        _ => None,
    };
    let nested_trust = loaded.nested.clone();
    let project_files = loaded.project_status.clone().map(|status| (loaded.project_files.clone(), status));
    let mut problems = loaded.problems;
    match (&options.on_warning, trust_notice) {
        (Some(warn), Some(notice)) => warn(notice),
        (None, notice) => problems.extend(notice),
        (Some(_), None) => {}
    }
    let given = if options.mcp_servers.is_empty() {
        marv_mcp::config::LoadedConfig::default()
    } else {
        let mut env = env.clone();
        env.insert(marv_mcp::config::PROJECT_DIR_VAR.into(), root.to_string_lossy().into_owned());
        marv_mcp::config::parse_mcp_servers(&options.mcp_servers, &env)
    };
    problems.extend(given.problems);
    // Servers given in code first; one from the config files with the same name is skipped.
    let mut servers = given.servers;
    for server in loaded.mcp_servers {
        if servers.iter().any(|s| s.name == server.name) {
            problems.push(format!(
                "MCP server \"{}\" from the config files was ignored: mcpServers has one with that name.",
                server.name
            ));
        } else {
            servers.push(server);
        }
    }
    let off = |device: &str| options.devices_off.iter().any(|d| d == device);
    servers.retain(|s| !options.mcp_skip.contains(&s.name) && !off(&format!("mcp:{}", s.name)));
    // Logins made with /mcp login in the TUI (same config dir) are used and refreshed; a library never starts one.
    let tokens_file = config_dir.join("mcp-tokens.json");
    let mcp: Option<crate::mcp_tools::McpHandle> = (!servers.is_empty()).then(|| {
        McpManager::new(
            servers,
            ManagerOptions {
                root: root.clone(),
                project_key: project_key(&root),
                home: home.clone(),
                version: env!("CARGO_PKG_VERSION").into(),
                trust: Some(McpTrust::new(config_dir.join("mcp-trust.json"))),
                logins: Some(McpTokenStore::new(&tokens_file)),
                env: env.clone(),
                connect_timeout: CONNECT_TIMEOUT,
                login_timeout: LOGIN_TIMEOUT,
            },
        )
    });

    // Before the session: its tool list is fixed from the start (the prompt cache).
    let github = options.github
        && !off("github")
        && match options.github_known {
            Some(known) => known,
            None => {
                let pairs: Vec<(String, String)> = env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                let outside: Vec<PathBuf> = std::iter::once(root.clone()).chain(options.writable.iter().cloned()).collect();
                marv_tools::github::github_available(&pairs, &outside, marv_tools::github::AUTH_CHECK).await
            }
        };
    let given_key = crate::config::provider_option_secrets(&options.provider);
    let mut init = SessionInit::new(root.clone(), ProviderChoice::Option(options.provider));
    init.thinking = options.thinking;
    init.reasoning_effort = options.reasoning_effort;
    init.instructions = loaded.instructions.map(|i| i.text);
    // Nested AGENTS.md files are the project's too (#62).
    init.nested_instructions = options.sources.contains(&Source::Project);
    init.nested_trust = nested_trust;
    init.project_files = project_files;
    init.skills = loaded.skills;
    init.agents = loaded.agents;
    init.role = match &options.role {
        Some(name) => Some(
            crate::roles::find_role(&loaded.roles, name)
                .cloned()
                .ok_or_else(|| SessionError::Invalid(crate::roles::unknown_role(&loaded.roles, name)))?,
        ),
        None => None,
    };
    if let Some(role) = init.role.take() {
        let role = crate::teams::member_role(role, options.role_writes.as_deref()).map_err(SessionError::Invalid)?;
        init.role = Some(crate::teams::in_team(role, options.team.as_deref().map(|_| options.team_roster.as_slice())));
    }
    init.team = options.team.as_deref().and_then(|name| crate::teams::team_for_chat(&loaded.teams, name, &options.team_roster));
    if let Some(team) = &mut init.team {
        team.describe_members(&loaded.roles, &options.team_writes);
        team.peers = options.team_peers.clone();
    }
    init.memory = loaded.memory;
    init.mcp = mcp;
    init.owns_mcp = true;
    init.tools = options.tools;
    init.system_prompt = options.system_prompt;
    init.deadline = options.deadline;
    init.files_anywhere = options.files_anywhere;
    // Marv's own keys, redacted from every tool result (docs/SECURITY.md): the user's config file's, the environment's
    // and the one this session was given. A config that can't be read adds none (loading it reports that elsewhere).
    let file = crate::config::read_file_config(&config_dir);
    init.secrets = crate::config::configured_secrets(file.as_ref(), &env, None);
    init.secrets.extend(given_key);
    init.approve = options.approve;
    init.mesh = options.mesh;
    init.wire_approver = options.wire_approver;
    init.max_steps = options.max_steps;
    (init.small_model, init.cores) = (options.small_model, options.cores);
    init.vision_model = options.vision_model;
    init.plan = options.plan;
    init.diagnostics = options.diagnostics;
    init.checks = options.checks;
    init.compact_tokens = options.compact_tokens;
    (init.compaction, init.compact_tail_tokens) = (options.compaction, options.compact_tail_tokens);
    init.formatter = options.formatter;
    init.checkpoints = options.checkpoints;
    init.checkpoints_dir = Some(config_dir.join("checkpoints").join(project_key(&root)));
    init.mcp_tools = options.mcp_tools;
    init.sandbox = options.sandbox;
    init.host_commands = options.host_commands && !off("host");
    init.github = github;
    init.web = options.web;
    init.yolo = options.yolo;
    init.no_yolo = no_yolo;
    init.sessions = store_for(&options.persist, || SessionStore::new(config_dir.join("sessions")));
    init.trajectories = store_for(&options.trajectories, || TrajectoryStore::new(config_dir.join("trajectories")));
    init.worktrees_dir = Some(config_dir.join("worktrees").join(project_key(&root)));
    init.tool_output_dir = Some(config_dir.join(marv_tools::output::TOOL_OUTPUT_DIR));
    init.file_locks_dir = Some(config_dir.join(marv_tools::file_lock::LOCK_FOLDER));
    init.transcript = options.transcript;
    init.on_warning = options.on_warning.clone();
    init.problems = problems;
    init.processes = options.processes && !off("process");
    init.pre_approved = options.pre_approved;
    init.writable = options.writable;
    init.hooks = options.hooks;
    init.one_shot = options.one_shot;
    init.herdr = options.herdr.filter(|_| !off("herdr"));
    init.tainted_at_start = options.tainted_at_start;
    init.always = options.always;
    init.mesh_hop = options.mesh_hop;
    init.file_versions = options.file_versions;
    // Never for a one-shot run (`marv -p`): there's no person to come back to.
    init.grant = options.grant.filter(|_| !options.one_shot);
    init.chat_read = options.chat_read;
    // Metered in `marv -p` too: its turns are charged and meet the person's budgets (asking no one).
    init.meters = options.meters;
    (init.meter_chat, init.on_budget, init.budget_asks) = (options.meter_chat, options.on_budget, options.budget_asks);
    init.door_state = options.door_state;
    init.timers = options.timers.filter(|_| !options.one_shot);
    let session = MarvSession::new(init)?;
    if let Some(mcp) = &session.mcp() {
        // A server that needs a login is told once (again only after it connected meanwhile).
        let warned = std::sync::Mutex::new(std::collections::HashSet::new());
        let (weak, warn) = (Arc::downgrade(mcp), options.on_warning.clone());
        mcp.set_on_change(move || {
            let Some(mcp) = weak.upgrade() else { return };
            let mut warned = warned.lock().unwrap_or_else(|e| e.into_inner());
            for s in mcp.status() {
                if s.state != McpState::NeedsLogin {
                    if s.state == McpState::Connected {
                        warned.remove(&s.name);
                    }
                    continue;
                }
                if !warned.insert(s.name.clone()) {
                    continue;
                }
                if let Some(warn) = &warn {
                    let text = format!(
                        "MCP server \"{}\" needs a login: run marv in a terminal and /mcp login {}; logins are stored in {}",
                        s.name,
                        s.name,
                        tokens_file.display()
                    );
                    // The caller's handler must not break the manager's state change.
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| warn(text)));
                }
            }
        });
        let mcp = mcp.clone();
        tokio::spawn(async move { mcp.start().await });
    }
    if let Some(target) = options.resume {
        let latest = target == ResumeTarget::Latest;
        match session.resume(target.clone()).await {
            Ok(Some(_)) => {}
            Ok(None) if latest => {}
            Ok(None) => {
                session.close().await;
                let ResumeTarget::Id(id) = target else { unreachable!() };
                return Err(SessionError::Invalid(format!("There's no saved session \"{id}\" for {}.", root.display())));
            }
            Err(err) => {
                session.close().await;
                return Err(err);
            }
        }
    }
    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yolo_is_refused_at_home_and_above() {
        let home = Path::new("/nonexistent/me");
        assert!(no_yolo_reason(home, home).unwrap().contains("your home folder (or above it)"));
        assert!(no_yolo_reason(Path::new("/"), home).unwrap().contains("the root of the file system"));
        assert!(no_yolo_reason(Path::new("/nonexistent/me/project"), home).is_none());
    }

    #[tokio::test]
    async fn resume_needs_persist_and_cwd_must_be_a_folder() {
        let provider = || ProviderOption::Ollama { model: "m".into(), host: None, context_length: None };
        let tmp = std::env::temp_dir();
        let err = SessionBuilder::new(&tmp, provider()).resume(ResumeTarget::Latest).build().await.err().unwrap();
        assert_eq!(err.to_string(), "resume needs persist (there's nowhere to resume from)");
        let err = SessionBuilder::new("/nonexistent/marv/x", provider()).build().await.err().unwrap();
        assert!(err.to_string().ends_with("isn't a folder"));
    }

    #[tokio::test]
    async fn plan_false_leaves_the_plan_tool_out_by_default_its_offered() {
        let provider = || ProviderOption::Ollama { model: "m".into(), host: None, context_length: None };
        let dir = std::env::temp_dir().join(format!("marv-sdk-plan-{}-{}", std::process::id(), marv_fs::random_hex()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut on = SessionBuilder::new(&dir, provider());
        on.options.config_dir = Some(dir.join("config"));
        let off = SessionBuilder { options: SessionOptions { plan: false, ..on.options.clone() } };
        assert!(on.options.plan);
        on = on.plan(true);
        let (on, off) = (on.build().await.unwrap(), off.build().await.unwrap());
        assert!(on.system_prompt().contains("keep a plan with the plan tool"));
        assert!(!off.system_prompt().contains("plan tool"));
        assert_eq!(on.plan(), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Writes made.txt, then says it did.
    struct MakesAFile(std::sync::Mutex<u32>);
    impl marv_provider::Provider for MakesAFile {
        fn name(&self) -> &str {
            "scripted"
        }
        fn context_length(&self) -> Option<u64> {
            None
        }
        fn stream(&self, _history: &[marv_core::ChatTurn], _options: marv_provider::StreamOptions) -> marv_provider::EventStream {
            use marv_core::ProviderEvent;
            let first = {
                let mut n = self.0.lock().unwrap();
                *n += 1;
                *n == 1
            };
            let events = if first {
                let arguments = serde_json::json!({"path": "made.txt", "content": "made\n"}).to_string();
                vec![
                    ProviderEvent::ToolCall(marv_core::ToolCall { id: "w".into(), name: "write_file".into(), arguments }),
                    ProviderEvent::Done { reason: Some("tool_calls".into()) },
                ]
            } else {
                vec![ProviderEvent::TextDelta("Made it.".into()), ProviderEvent::Done { reason: Some("stop".into()) }]
            };
            Box::pin(futures::stream::iter(events))
        }
    }

    /// Counts what it's asked and answers "always".
    #[derive(Default)]
    struct AlwaysYes(std::sync::Mutex<Vec<String>>);
    #[async_trait::async_trait]
    impl Approver for AlwaysYes {
        async fn approve(&self, request: marv_tools::ApprovalRequest) -> marv_tools::Decision {
            self.0.lock().unwrap().push(request.scope.key);
            marv_tools::Decision::Always
        }
    }

    /// Runtime-kernel 1: a chat that's unloaded and loaded again (Marv desktop's sleeping chats) passes the same
    /// "always" scopes and mesh turn hop to each load, so a person's "always" in the first load still answers in the
    /// second, and the hop the switchboard stamps is the one the caller keeps.
    #[tokio::test]
    async fn given_always_scopes_and_turn_hop_last_across_loads() {
        use futures::StreamExt;
        let base = std::env::temp_dir().join(format!("marv-sdk-al-{}-{}", std::process::id(), marv_fs::random_hex()));
        let (root, config_dir) = (base.join("project"), base.join("config"));
        std::fs::create_dir_all(&root).unwrap();
        let (always, hop) = (crate::runner::AlwaysScopes::default(), Arc::new(marv_mesh::hop::TurnHop::default()));
        let cancel = tokio_util::sync::CancellationToken::new();
        // 0.8.0: the runtime folder (`$XDG_RUNTIME_DIR/marv`) is made only inside an existing XDG_RUNTIME_DIR.
        std::fs::create_dir_all(base.join("run")).unwrap();
        let env: Env = [("XDG_RUNTIME_DIR".to_string(), base.join("run").display().to_string())].into_iter().collect();
        let mesh = crate::mesh::start("al-test", &env, Some(&root), cancel.clone()).unwrap();
        let person = Arc::new(AlwaysYes::default());
        let load = |shared: bool| {
            let mut builder = SessionBuilder::new(&root, ProviderOption::Custom(Arc::new(MakesAFile(std::sync::Mutex::new(0)))));
            builder.options.config_dir = Some(config_dir.clone());
            (builder.options.sandbox, builder.options.checkpoints, builder.options.yolo) = (false, false, false);
            builder.options.approve = Some(person.clone());
            builder.options.mesh = Some(mesh.clone());
            if shared {
                builder.options.always = Some(always.clone());
                builder.options.mesh_hop = Some(hop.clone());
            }
            builder.build()
        };
        for _ in 0..2 {
            let session = load(true).await.unwrap();
            assert!(Arc::ptr_eq(&mesh.turn_hop(&crate::mesh::parent_id()).unwrap(), &hop));
            // A new file each time: a later load never read the last one's, so writing over it would be refused
            // before anyone is asked (the stale-write refusal), and the "always" would never be tested.
            let _ = std::fs::remove_file(root.join("made.txt"));
            session.send("make a file", crate::session::SendOptions::default()).unwrap().collect::<Vec<_>>().await;
            session.close().await;
        }
        assert_eq!(person.0.lock().unwrap().len(), 1, "the second load's write was answered by the first's \"always\"");
        // Without them, each load has its own: asked again.
        let session = load(false).await.unwrap();
        assert!(!Arc::ptr_eq(&mesh.turn_hop(&crate::mesh::parent_id()).unwrap(), &hop));
        let _ = std::fs::remove_file(root.join("made.txt"));
        session.send("make a file", crate::session::SendOptions::default()).unwrap().collect::<Vec<_>>().await;
        session.close().await;
        assert_eq!(person.0.lock().unwrap().len(), 2);
        cancel.cancel();
        let _ = std::fs::remove_dir_all(&base);
    }

    // Ports tests/sdk.test.ts "checkpoints: a turn's files can be put back with undoPlan() and undo(), outside a
    // repository too (#16)".
    #[tokio::test]
    async fn checkpoints_a_turns_files_can_be_put_back_outside_a_repository_too() {
        use futures::StreamExt;
        let base = std::env::temp_dir().join(format!("marv-sdk-cp-{}-{}", std::process::id(), marv_fs::random_hex()));
        let (root, config_dir) = (base.join("project"), base.join("config"));
        std::fs::create_dir_all(&root).unwrap();
        let session_with = |checkpoints: bool| {
            let mut builder = SessionBuilder::new(&root, ProviderOption::Custom(Arc::new(MakesAFile(std::sync::Mutex::new(0)))));
            builder.options.config_dir = Some(config_dir.clone());
            builder.options.sandbox = false;
            builder.options.checkpoints = checkpoints;
            builder.build()
        };
        let session = session_with(true).await.unwrap();
        session.send("make a file", crate::session::SendOptions::default()).unwrap().collect::<Vec<_>>().await;
        assert!(root.join("made.txt").exists());
        let plan = session.undo_plan().await.unwrap().unwrap_or_else(|r| panic!("{}", r.reason));
        let files: Vec<(&str, crate::checkpoints::undo::UndoAction)> =
            plan.files.iter().map(|f| (f.path.as_str(), f.action)).collect();
        assert_eq!(files, [("made.txt", crate::checkpoints::undo::UndoAction::Delete)]);
        assert_eq!(session.undo(&plan).await.unwrap().deleted, ["made.txt"]);
        assert!(!root.join("made.txt").exists());
        assert!(config_dir.join("checkpoints").exists());
        session.close().await;

        let off = session_with(false).await.unwrap();
        off.send("make a file", crate::session::SendOptions::default()).unwrap().collect::<Vec<_>>().await;
        assert!(off.undo_plan().await.unwrap().unwrap_err().reason.contains("off"));
        off.close().await;
        let _ = std::fs::remove_dir_all(&base);
    }
}
