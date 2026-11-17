//! Drives one chat's session: builds it in the chat's folder, then turns the page's messages into turns,
//! interrupts, approvals and settings, and the session's events into JSON for the page.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use marv_provider::ProviderOption;
use marv_session::sdk::Store;
use marv_session::skills::LoadedSkill;
use marv_session::sources::Source;
use marv_session::{Configure, MarvSession, ProviderChoice, ResumeTarget, SendOptions, SessionBuilder};
use serde_json::{Value, json};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::approver::WebApprover;
use crate::events;
use crate::protocol::{ClientMessage, is_model_id};

/// How a desktop chat asks the person to choose (`ChatSpec::choices`): fixed text, so the prompt stays byte-stable.
pub const CHOICES_PROMPT: &str = "# Asking the person to choose

When you need the person to decide between options (which approach, what to do next, a recommendation that needs their \
go-ahead), end your reply with a `choices` block and stop for their answer. One block per question:

```choices
? What should the first playable version include?
A) Scraping and payouts only (recommended) — something to try today
B) Scraping, payouts and an upgrade shop
C) The full loop, prestige included — longest wait
```

The first line, after `?`, is the question. Each option is a letter, `)`, a short label, then ` — ` and a one-line why \
(optional). Give 2 to 5 options. When you have a recommendation, mark exactly one option `(recommended)` and put it \
first. The person sees buttons, picks one with the mouse or keyboard (Enter takes the recommended one) or writes their \
own answer; their pick comes back as their next message, like `A) Scraping and payouts only`. Ask only what the \
person can judge from their goal (what to build, how it should feel, what to cut): settle technical and tuning questions \
(a constant, an exponent, a file layout) yourself and say what you chose. Don't ask what you can find out yourself, and \
never put a choices block in a message to another Marv.";

/// Where and how a chat runs.
pub struct ChatSpec {
    /// The folder Marv works in: it reads and changes what's below it, and nothing else without asking.
    pub root: PathBuf,
    /// Marv's own files: saved sessions, checkpoints.
    pub config_dir: PathBuf,
    /// The OpenRouter key.
    pub api_key: String,
    /// #159: the Z.ai Coding Plan key, for `zai/…` models (the person's own machine only).
    pub zai_api_key: Option<String>,
    /// #159: the Kimi Code plan key, for `kimi/…` models (the person's own machine only).
    pub kimi_api_key: Option<String>,
    /// #159: the ChatGPT sign-in's token file, for `chatgpt/…` models (the person's own machine only).
    pub chatgpt_auth: Option<PathBuf>,
    pub model: String,
    pub max_steps: u32,
    /// Sandboxed commands and edits below `root` run without asking.
    pub yolo: bool,
    /// The saved session to continue (`Id` from an earlier `ready`), or `None` for a new one.
    pub resume: Option<ResumeTarget>,
    /// Offer the process tool (dev servers, their ports on this machine's 127.0.0.1): only where the person using
    /// the page is on this machine.
    pub processes: bool,
    /// What Marv loads from disk, as the CLI does: `User` (~/.marv's skills, memory, MCP servers, instructions) and
    /// `Project` (the folder's). Only for the person's own machine; marv-web loads none.
    pub sources: Vec<Source>,
    /// Put the chat on this machine's mesh, as the TUI is: it gets a name other Marvs see (`marv mesh ls`, the
    /// `mesh_list` tool), wires need its person's approval, and a message on a wire wakes it. Only on the person's own
    /// machine.
    pub mesh: bool,
    /// MCP servers switched off for this chat, by name.
    pub mcp_off: Vec<String>,
    /// Devices switched off for this chat (runtime-kernel §17.1, D1), by name (`mcp:<server>`, `github`, `process`,
    /// `host`, `herdr`): applied when the session starts, so a change takes effect at the chat's next boot.
    pub devices_off: Vec<String>,
    /// The chat's name on the mesh when `mesh` is on: a desktop chat's comes from its id
    /// (`marv_kernel::mesh_name_for`), so it's the same at every start and wires find it again. None: a generated name.
    pub mesh_name: Option<String>,
    /// The desktop chat's id, told to other Marvs in `marv/info` (`PeerInfo::chat`). None: not a desktop chat.
    pub chat_id: Option<String>,
    /// Honor the person's mesh rules (`~/.marv/mesh/policies.json`, runtime-kernel 2b): a rule they made may wire
    /// this chat to another Marv without asking. Only for the person's own machine (the desktop); never marv-web.
    pub rules: bool,
    /// Allow `mcp` login/logout/trust (OAuth opens the browser on this machine): only on the person's own machine.
    pub manage_mcp: bool,
    /// Let bash run a command on this machine outside the sandbox, asked every time (never yolo, never "always"):
    /// only where the person approving is at this machine.
    pub host_commands: bool,
    /// #204: write each turn's trajectory (`config_dir/trajectories/<project>/<session>.jsonl`), as the TUI and
    /// `marv -p` do: the config's `trajectories` ([`trajectories_on`]).
    pub trajectories: bool,
    /// The chat is kept resident by its caller (runtime-kernel 1, Marv desktop's sleeping chats): its mesh (door open),
    /// "always" scopes, turn hop, approver and wake limit outlive this session, which may be unloaded and loaded
    /// again. With it, `mesh`, `mesh_name`, `chat_id` and `rules` are the caller's business: `run_chat` neither starts
    /// nor drops a mesh, and the caller watches it for wakes ([`check_wake`], [`run_wake`]). None (marv-web): as before.
    pub resident: Option<Resident>,
    /// The model to use instead of the one `model` names (tests, and an embedder's own provider). None: `model`.
    pub provider: Option<ProviderOption>,
    /// The chat's service grant (Marv desktop's, for a chat its person granted): a turn a rule's message wakes runs
    /// what yolo would under it ([`marv_session::grants`]). Only on the person's own machine; never marv-web's server.
    pub grant: Option<Arc<dyn marv_session::grants::ChatGrant>>,
    /// The kernel's event log (Marv desktop's, runtime-kernel 3): the chat's page-outs go there
    /// ([`marv_kernel::KernelEvent::Compacted`], sizes only), named by `chat_id` (else its mesh name). None: not logged.
    pub kernel_events: Option<Arc<dyn marv_kernel::EventLog>>,
    /// Runtime-kernel §16: the process's meters and the person's budgets (Marv desktop's). Each turn is metered and
    /// meets the budgets over the chat; a warning, the person's answer at a limit and a pause are told on the page.
    /// None (marv-web's server, tests): nothing is metered.
    pub meters: Option<Arc<marv_session::allocator::MeterBook>>,
    /// The chat's `wake_me` timers (Marv desktop's, runtime-kernel §15.4): offered in every desktop chat, set only
    /// under its grant. Never marv-web's server.
    pub timers: Option<Arc<dyn marv_session::timers::SelfTimers>>,
    /// The chat's delegates (Marv desktop's agent chats, `marv_session::delegate`): its `delegate` tools hand tasks to
    /// other agents in new chats. None: not offered (marv-web, a headless load).
    pub delegates: Option<Arc<dyn marv_session::delegate::Delegates>>,
    /// The chat's role by name (`marv_session::roles`): Marv desktop's, picked when the chat was made and the same at
    /// every load. An unknown one fails the session's start (told to the page). None: no role.
    pub role: Option<String>,
    /// The team the chat was started in, by name, and its members' roles (`marv_session::teams`, Marv desktop's New
    /// team): the team's brief follows the role in the system prompt. None: not in a team.
    pub team: Option<(String, Vec<String>)>,
    /// The write paths the person typed for each member of `team` when it started, in the roster's order (None: the
    /// role's own): so each chat's brief says what each teammate can really do. Empty: unknown (an older chat).
    pub team_writes: Vec<Option<Vec<String>>>,
    /// The team's other chats (role, name, mesh name): its brief names their inboxes. Empty: unknown.
    pub team_peers: Vec<marv_session::teams::TeamPeer>,
    /// The desktop shows a reply's `choices` blocks as buttons: the system prompt then says how to write one
    /// ([`CHOICES_PROMPT`]). False: a plain chat (the web UI, tests).
    pub choices: bool,
    /// The write paths the person gave the chat when its team started: its role's write set for this chat
    /// (`marv_session::teams::member_role`). None: the role's own.
    pub writes: Option<Vec<String>>,
    /// What runs the chat: Marv (this module), or Claude Code (`crate::claude_code`, the harness plan's phase 1).
    pub harness: Harness,
    /// A harness chat runs inside Marv's outer sandbox (`crate::harness_sandbox`, the harness plan's phase 4): the
    /// desktop's default; off in tests and marv-web.
    pub harness_sandbox: bool,
    /// An agent chat's model, as the person last picked it (`agent_model`): the agent starts with it. None: the
    /// agent's own default.
    pub agent_model: Option<String>,
}

/// The agent that runs a chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Harness {
    #[default]
    Marv,
    /// The person's own `claude` (`crate::claude_code`).
    ClaudeCode,
    /// An agent that speaks ACP (`crate::acp`, phase 4): OpenCode, Gemini CLI, Antigravity.
    Acp(crate::acp::Preset),
}

impl Harness {
    /// From a chat record's name: `"claude-code"`, an ACP preset's (`"opencode"`, …); anything else (and none) is Marv.
    pub fn from_name(name: Option<&str>) -> Self {
        match name {
            Some(crate::claude_code::NAME) => Self::ClaudeCode,
            Some(other) => crate::acp::Preset::from_name(other).map_or(Self::Marv, Self::Acp),
            None => Self::Marv,
        }
    }

    /// Every harness that isn't Marv, in the order the page lists them.
    pub fn foreign() -> Vec<Harness> {
        std::iter::once(Self::ClaudeCode).chain(crate::acp::Preset::ALL.map(Self::Acp)).collect()
    }

    /// Its name in a chat record (None: Marv).
    pub fn name(self) -> Option<&'static str> {
        match self {
            Self::Marv => None,
            Self::ClaudeCode => Some(crate::claude_code::NAME),
            Self::Acp(p) => Some(p.name()),
        }
    }

    /// What the page calls it.
    pub fn title(self) -> &'static str {
        match self {
            Self::Marv => "Marv",
            Self::ClaudeCode => "Claude Code",
            Self::Acp(p) => p.title(),
        }
    }

    /// Its program, when it's installed here (Marv: none needed).
    pub fn program(self) -> Option<std::path::PathBuf> {
        match self {
            Self::Marv => None,
            Self::ClaudeCode => crate::claude_code::find_claude(),
            Self::Acp(p) => crate::acp::find(p),
        }
    }

    /// Said when its program isn't installed.
    pub fn not_installed(self) -> String {
        match self {
            Self::Marv => String::new(),
            Self::ClaudeCode => crate::claude_code::NOT_INSTALLED.into(),
            Self::Acp(p) => p.not_installed(),
        }
    }
}

/// What a resident chat keeps between loads of its session ([`ChatSpec::resident`]). Clones share it all.
#[derive(Clone)]
pub struct Resident {
    /// Its switchboard: wires, inbox and limiters live there, and its door stays open while it sleeps. None: not on
    /// the mesh (no `XDG_RUNTIME_DIR`, or it couldn't start).
    pub mesh: Option<marv_session::mesh::MeshHandle>,
    pub mesh_name: Option<String>,
    /// The person's "always" answers, which last as long as the chat (as they did while its session stayed loaded).
    pub always: marv_session::AlwaysScopes,
    pub turn_hop: Arc<marv_session::radio::TurnHop>,
    /// What its conversation has read and written (runtime-kernel §14.2): kept while it sleeps, so its next edit after
    /// a wake isn't refused for a file it did read.
    pub file_versions: Arc<marv_tools::FileVersions>,
    /// Asks the person (the page): the session's approvals and, while it sleeps, its door's wire requests.
    pub approver: Arc<WebApprover>,
    /// Wakes per wire and the rules' overall limit: a limit hit while it slept is still hit once it's loaded.
    pub wake_limit: Arc<std::sync::Mutex<marv_session::mesh::WakeLimit>>,
    /// The session while `run_chat` runs it (for the caller's wakes); None while it loads and once it's unloaded.
    pub session: Arc<std::sync::Mutex<Option<MarvSession>>>,
    /// A harness chat's next turn is unattended (a timer's or mail's message): its budgets pause rather than ask
    /// (`crate::harness_budget`). Set by the host with the message; read and cleared when the turn starts.
    pub woken: Arc<std::sync::atomic::AtomicBool>,
    /// A harness chat's Marv OS tool calls' state across them and its sleeps: its taint, its turn's kind (H20).
    pub harness_calls: crate::harness_mesh::HarnessCalls,
}

impl Resident {
    /// The loaded session, if there is one.
    pub fn session(&self) -> Option<MarvSession> {
        self.session.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn publish(&self, session: Option<MarvSession>) {
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = session;
    }
}

/// #204: the config's `trajectories` in `config_dir` (on by default), resolved as `marv -p` resolves it. A config file
/// that can't be read leaves the default: the chat still opens (`marv -p` would stop and say why).
pub fn trajectories_on(config_dir: &std::path::Path) -> bool {
    use marv_session::config::{ConfigStore, Env, resolve_config};
    let env: Env = std::env::vars().collect();
    let file = ConfigStore::new(config_dir).load().ok().flatten();
    resolve_config(file.as_ref(), &env).trajectories
}

/// `/trust`, offered where the chat loads the project's files (the person's own machine).
const TRUST_COMMAND: (&str, &str) =
    ("trust", "Use this folder's instructions, skills and agent types as yours (from the next chat)");

/// The commands the page offers in its `/` menu (besides the skills), and what they do here.
const COMMANDS: &[(&str, &str)] = &[
    ("help", "Show the commands"),
    ("skills", "List the skills; run one with /<skill> <request>"),
    ("compact", "Summarize the conversation to free up context (/compact <what to keep>; /compact budget <tokens>|default)"),
    ("cost", "Tokens used and cost so far"),
    ("memory", "Show what Marv remembers"),
    ("model", "Switch the model (/model <id>)"),
    ("permissions", "What Marv may do without asking: this chat's approvals, and what's saved"),
];

/// A model id that starts with this runs on the local Ollama (`ollama/gemma4:12b`); one with [`ZAI_PREFIX`],
/// [`KIMI_PREFIX`] or [`CHATGPT_PREFIX`] on that plan (#159); any other is OpenRouter's.
pub const OLLAMA_PREFIX: &str = "ollama/";
/// #159: `zai/glm-5.3`: the Z.ai GLM Coding Plan, with its own key. (OpenRouter's Z.ai models are `z-ai/…`.)
pub const ZAI_PREFIX: &str = "zai/";
/// #159: `kimi/kimi-for-coding`: the Kimi Code plan, with its own key. (OpenRouter's Kimi models are `moonshotai/…`.)
pub const KIMI_PREFIX: &str = "kimi/";
/// #159: `chatgpt/<slug>`: the person's ChatGPT plan, through their sign-in.
pub const CHATGPT_PREFIX: &str = "chatgpt/";

pub fn is_local(model: &str) -> bool {
    model.starts_with(OLLAMA_PREFIX)
}

/// Puts a chat on this machine's mesh (`None`: no `XDG_RUNTIME_DIR`, or it couldn't; the page is told why): its
/// switchboard named `name` (else generated) in `root`, telling other Marvs it's `chat`, with the person's rules
/// honored when `rules`. The door isn't open yet ([`open_mesh_door`]).
pub fn start_mesh(
    root: &std::path::Path,
    name: Option<&str>,
    chat: Option<String>,
    rules: bool,
    config_dir: &std::path::Path,
    out: &UnboundedSender<Value>,
) -> Option<marv_session::mesh_process::Mesh> {
    let env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    match marv_session::mesh_process::start(&env, root, &[root.to_path_buf()], name) {
        // A MARV_RUNTIME_DIR that was refused says why (a symlink on the way, inside the project), as the TUI does,
        // rather than the chat just being off the mesh (#168).
        Ok(None) => {
            if let Some(text) = mesh_refused_notice(&env, root) {
                let _ = out.send(json!({"type": "notice", "text": text}));
            }
            None
        }
        Ok(started) => {
            let mesh = started.map(|mut m| {
                m.chat = chat;
                m
            });
            if let Some(m) = mesh.as_ref().filter(|_| rules)
                && let Err(e) = m.honor_rules(config_dir, &marv_session::paths::home_dir())
            {
                let text = format!("Mesh rules aren't used: {}", marv_core::printable(&e));
                let _ = out.send(json!({"type": "notice", "text": text}));
            }
            mesh
        }
        Err(e) => {
            let _ = out.send(json!({"type": "notice", "text": format!("Not on the mesh: {}", marv_core::printable(&e))}));
            None
        }
    }
}

/// Opens the mesh's door: other Marvs find the chat and ask for wires, which its person answers through `approver`.
pub fn open_mesh_door(mesh: &mut marv_session::mesh_process::Mesh, approver: &Arc<WebApprover>, out: &UnboundedSender<Value>) {
    let as_approver: Arc<dyn marv_tools::types::Approver> = approver.clone();
    if let Some(wires) = marv_session::mesh::wire_approver(None, Some(&as_approver))
        && let Err(e) = mesh.open_door(wires)
    {
        let _ = out.send(json!({"type": "notice", "text": format!("Not on the mesh: {}", marv_core::printable(&e))}));
    }
}

/// The provider a chat's model id names. Each gets only its own credential: a plan without one gets an empty key
/// (its first request says what's missing), never another provider's.
pub fn provider_option(spec: &ChatSpec, model: &str) -> ProviderOption {
    if let Some(local) = model.strip_prefix(OLLAMA_PREFIX) {
        // Ollama on its default port, with the CLI's default context window (Ollama's own is far smaller).
        return ProviderOption::Ollama {
            model: local.to_string(),
            host: None,
            context_length: Some(marv_session::config::DEFAULT_CONTEXT_LENGTH),
        };
    }
    if let Some(plan) = model.strip_prefix(ZAI_PREFIX) {
        let api_key = spec.zai_api_key.clone().unwrap_or_default();
        return ProviderOption::Zai { api_key, model: Some(plan.to_string()), base_url: None };
    }
    if let Some(plan) = model.strip_prefix(KIMI_PREFIX) {
        let api_key = spec.kimi_api_key.clone().unwrap_or_default();
        return ProviderOption::Kimi { api_key, model: Some(plan.to_string()), base_url: None };
    }
    if let Some(slug) = model.strip_prefix(CHATGPT_PREFIX) {
        return match spec.chatgpt_auth.clone() {
            Some(auth_file) => ProviderOption::ChatGpt { model: slug.to_string(), auth_file },
            // Never an empty path (it would read a file in the working folder).
            None => ProviderOption::Custom(std::sync::Arc::new(marv_provider::FailingProvider {
                name: format!("chatgpt · {slug}"),
                message: "Sign in with ChatGPT isn't available here.".into(),
            })),
        };
    }
    ProviderOption::OpenRouter { api_key: spec.api_key.clone(), model: Some(model.to_string()), base_url: None }
}

/// Other providers' cores for a chat (`marv_session::cores`): the person's own agent types and `smallModel` may run
/// on any provider set up here (its key, its sign-in), never one that isn't.
pub fn core_maker(spec: &ChatSpec) -> marv_session::cores::CoreMaker {
    let (api_key, zai_api_key, chatgpt_auth) = (spec.api_key.clone(), spec.zai_api_key.clone(), spec.chatgpt_auth.clone());
    let kimi_api_key = spec.kimi_api_key.clone();
    std::sync::Arc::new(move |kind, model| {
        let model = model.to_string();
        let option = match kind {
            marv_provider::ProviderKind::OpenRouter => {
                let api_key = Some(api_key.clone()).filter(|k| !k.is_empty())?;
                ProviderOption::OpenRouter { api_key, model: Some(model), base_url: None }
            }
            marv_provider::ProviderKind::Zai => {
                let api_key = zai_api_key.clone().filter(|k| !k.is_empty())?;
                ProviderOption::Zai { api_key, model: Some(model), base_url: None }
            }
            marv_provider::ProviderKind::Kimi => {
                let api_key = kimi_api_key.clone().filter(|k| !k.is_empty())?;
                ProviderOption::Kimi { api_key, model: Some(model), base_url: None }
            }
            // Not set up in the desktop: no key for them here. Qwen's Coding Plan is interactive-only by its terms.
            // Nor a custom provider or llama.cpp, which the desktop doesn't set up (`marv_session::config::core_config`).
            marv_provider::ProviderKind::DeepSeek
            | marv_provider::ProviderKind::Qwen
            | marv_provider::ProviderKind::Custom
            | marv_provider::ProviderKind::LlamaCpp => return None,
            marv_provider::ProviderKind::ChatGpt => {
                ProviderOption::ChatGpt { model, auth_file: chatgpt_auth.clone().filter(|f| f.is_file())? }
            }
            marv_provider::ProviderKind::Ollama => {
                ProviderOption::Ollama { model, host: None, context_length: Some(marv_session::config::DEFAULT_CONTEXT_LENGTH) }
            }
        };
        Some(marv_session::config::provider_factory(option, false, marv_session::config::DEFAULT_REASONING_EFFORT))
    })
}

/// The person's `smallModel`, from their own config file in `config_dir` (an unreadable file: none).
fn small_model(config_dir: &std::path::Path) -> Option<String> {
    marv_session::config::ConfigStore::new(config_dir).load().ok().flatten().and_then(|f| f.small_model)
}

/// `image_pasted` for a pasted image: where Marv saved it, or why it didn't. The bytes decide what it is, not the
/// page's `media_type` (only checked to be an image type).
pub(crate) fn pasted_image(config_dir: &std::path::Path, media_type: &str, data: &str) -> serde_json::Value {
    use base64::Engine;
    let saved = if !media_type.starts_with("image/") {
        Err("Only images can be pasted.".to_string())
    } else if data.len() > marv_core::image::MAX_IMAGE_BYTES / 3 * 4 + 4 {
        Err("That image is over 5 MB: models take at most 5 MB.".to_string())
    } else {
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|_| "The image couldn't be read.".to_string())
            .and_then(|bytes| marv_session::images::save_pasted(config_dir, &bytes))
    };
    match saved {
        Ok(path) => json!({"type": "image_pasted", "path": path.to_string_lossy()}),
        Err(e) => json!({"type": "image_pasted", "error": e}),
    }
}

/// The person's `visionModel`, read like [`small_model`].
fn vision_model(config_dir: &std::path::Path) -> Option<String> {
    marv_session::config::ConfigStore::new(config_dir).load().ok().flatten().and_then(|f| f.vision_model)
}

/// Why a chat in `root` has no runtime folder, when it's because `MARV_RUNTIME_DIR` was refused
/// (`marv_sandbox::override_refusal_for`), printable; `None` otherwise (no override: the mesh is quietly off).
fn mesh_refused_notice(env: &std::collections::BTreeMap<String, String>, root: &std::path::Path) -> Option<String> {
    marv_sandbox::override_refusal_for(env, Some(root)).map(|why| format!("Not on the mesh: {}", marv_core::printable(&why)))
}

/// Runs the chat until `incoming` closes (the page went away). Sends `ready` (with the session's id, for a later
/// resume) or `fatal` first.
pub async fn run_chat(spec: ChatSpec, mut incoming: UnboundedReceiver<ClientMessage>, out: UnboundedSender<Value>) {
    match spec.harness {
        Harness::ClaudeCode => return crate::claude_code::run_chat(spec, incoming, out).await,
        Harness::Acp(preset) => return crate::acp::run_chat(preset, spec, incoming, out).await,
        Harness::Marv => {}
    }
    let resident = spec.resident.clone();
    let approver = match &resident {
        Some(r) => r.approver.clone(),
        None => Arc::new(WebApprover::new(out.clone())),
    };
    // The mesh first: the session's tools include it from the start (the prompt cache). A resident chat's is its
    // caller's, door and all.
    let mut mesh = None;
    if spec.mesh && resident.is_none() {
        mesh = start_mesh(&spec.root, spec.mesh_name.as_deref(), spec.chat_id.clone(), spec.rules, &spec.config_dir, &out);
        // Runtime-kernel chat card: `mesh.cards` from the person's config (a resident chat's is its caller's, Marv
        // desktop's `Chats::set_cards`).
        if let Some(m) = &mesh {
            m.handle.set_cards(marv_session::config::ConfigStore::new(&spec.config_dir).cards());
        }
    }
    let handle = match &resident {
        Some(r) => r.mesh.clone(),
        None => mesh.as_ref().map(|m| m.handle.clone()),
    };
    // A folder whose own config turns yolo off keeps it off whatever the page sets (as the TUI's `/yolo on`), and so
    // does the home folder (a desktop Home chat): yolo is refused there.
    let yolo_held_off = yolo_held_off(&spec);
    let session = match open(&spec, approver.clone(), out.clone(), handle).await {
        Ok(session) => session,
        Err(message) => {
            let _ = out.send(json!({"type": "fatal", "message": message}));
            return;
        }
    };
    // #202: the chat's last turn was cut off by Marv quitting (a force-quit during an approval): the window's own
    // transcript still shows the turn as it was, so it gets the session's notice.
    if let Some(text) = session.take_cut_off_notice() {
        let _ = out.send(json!({"type": "notice", "text": text}));
    }
    // Its door: other Marvs find it and ask for wires, which its person answers here.
    if let Some(mesh) = mesh.as_mut() {
        open_mesh_door(mesh, &approver, &out);
    }
    let watching = CancellationToken::new();
    // Shared with the watcher: the person sending a message resumes wakes the rules' overall limit paused.
    let wake_limit = match &resident {
        Some(r) => r.wake_limit.clone(),
        None => Arc::new(std::sync::Mutex::new(marv_session::mesh::WakeLimit::default())),
    };
    match &resident {
        // The caller watches its mesh and wakes this session through it.
        Some(r) => r.publish(Some(session.clone())),
        None => {
            if let Some(handle) = session.mesh() {
                let watch = watch_mesh(
                    handle,
                    session.clone(),
                    approver.clone(),
                    out.clone(),
                    wake_limit.clone(),
                    spec.config_dir.clone(),
                    watching.clone(),
                );
                tokio::spawn(watch);
            }
        }
    }
    if let Some(log) = &spec.kernel_events {
        let chat = match spec.chat_id.as_deref().and_then(marv_kernel::AgentId::new) {
            Some(id) => Some(marv_kernel::AgentKey::Chat(id)),
            None => session.mesh().map(|m| m.own_key()),
        };
        if let Some(chat) = chat {
            session.log_kernel_events(log.clone(), chat);
        }
    }
    // The skills and the project's files exactly as the session loaded them (one read, with the trust applied): an
    // untrusted project's skills aren't offered, and /trust trusts what was loaded.
    let project_trust = session.project_files();
    let skills: Vec<LoadedSkill> = session.skills();
    // A built-in command's name is Marv's: a skill (a project's, most of all) named like one would take the person's
    // `/compact` or `/model` (or `/trust`) over. It isn't offered, and the person is told once.
    let (skills, shadowed): (Vec<LoadedSkill>, Vec<LoadedSkill>) = skills.into_iter().partition(|s| {
        !COMMANDS.iter().chain(std::iter::once(&TRUST_COMMAND)).any(|(name, _)| s.skill.name.eq_ignore_ascii_case(name))
    });
    let menu: Vec<Value> = COMMANDS
        .iter()
        .copied()
        .chain(project_trust.as_ref().map(|_| TRUST_COMMAND))
        .map(|(name, description)| json!({"name": name, "description": description}))
        .chain(skills.iter().map(|s| json!({"name": s.skill.name, "description": s.skill.description, "skill": true})))
        .collect();
    let mesh_name = match &resident {
        Some(r) => r.mesh_name.clone(),
        None => mesh.as_ref().map(|m| m.name.clone()),
    };
    let _ = out.send(json!({
        "type": "ready", "sessionId": session.id(), "model": spec.model, "commands": menu, "mesh": mesh_name,
        "contextBudget": session.context_budget(), "contextBudgetSet": session.context_budget_set(),
    }));
    for skill in &shadowed {
        let name = marv_core::printable(&skill.skill.name);
        let _ = out.send(json!({"type": "notice", "text": format!("The skill /{name} isn't offered: Marv's own /{name} command has that name.")}));
    }

    let mut turn: Option<(CancellationToken, JoinHandle<()>)> = None;
    while let Some(message) = incoming.recv().await {
        match message {
            ClientMessage::Send { text } => {
                let text = text.trim();
                if text.is_empty() {
                    continue;
                }
                wake_limit.lock().unwrap_or_else(|e| e.into_inner()).person_typed();
                let cancel = CancellationToken::new();
                let before = session.usage().totals;
                match session.send(text, SendOptions { for_model: None, cancel: Some(cancel.clone()) }) {
                    Ok(events) => {
                        turn = Some((cancel, tokio::spawn(forward(events, before, session.clone(), out.clone()))));
                    }
                    Err(e) => {
                        let _ = out.send(send_refused(text, &e.to_string()));
                    }
                }
            }
            ClientMessage::PasteImage { media_type, data } => {
                let _ = out.send(pasted_image(&spec.config_dir, &media_type, &data));
            }
            ClientMessage::SendNow { text } => {
                let text = text.trim();
                if text.is_empty() {
                    continue;
                }
                // No turn to take it (it just ended): back to the composer, never started as a turn of its own.
                if session.send_now(text).is_err() {
                    let _ = out.send(json!({"type": "not_sent", "texts": [text]}));
                }
            }
            ClientMessage::Command { line } => {
                let (name, args) =
                    line.trim().trim_start_matches('/').split_once(' ').unwrap_or((line.trim().trim_start_matches('/'), ""));
                let (name, args) = (name.to_lowercase(), args.trim());
                if let Some(skill) = skills.iter().find(|s| s.skill.name == name) {
                    // As typed in the transcript; the skill's instructions for the model.
                    let for_model = marv_session::skills::skill_message(&skill.skill, args);
                    let cancel = CancellationToken::new();
                    let before = session.usage().totals;
                    match session.send(line.trim(), SendOptions { for_model: Some(for_model), cancel: Some(cancel.clone()) }) {
                        Ok(events) => {
                            turn = Some((cancel, tokio::spawn(forward(events, before, session.clone(), out.clone()))));
                        }
                        Err(e) => {
                            let _ = out.send(json!({"type": "error", "message": e.to_string()}));
                        }
                    }
                    continue;
                }
                let print = |text: String| {
                    let _ = out.send(json!({"type": "command_output", "text": text}));
                };
                match name.as_str() {
                    "help" | "" => print(help(&skills)),
                    "skills" => print(skills_text(&skills)),
                    "cost" => {
                        let usage = session.usage();
                        let text = marv_session::usage::usage_report(&usage.totals, usage.last.as_ref(), usage.context_length);
                        // Runtime-kernel §16.9: and the folder's day, every chat in it.
                        print(match session.folder_day() {
                            Some(day) => format!("{text}\n{day}"),
                            None => text,
                        });
                    }
                    // The same view as the TUI's (#82), shown here only: never sent to the model. The desktop has no
                    // config file of its own to show.
                    "permissions" if !args.is_empty() => print(marv_session::permissions::PERMISSIONS_USAGE.into()),
                    "permissions" => {
                        let servers = session.mcp().map(|m| m.status());
                        print(marv_session::permissions::permissions_text(&session.permissions(), None, servers.as_deref()))
                    }
                    "trust" if project_trust.is_some() => {
                        let Some((files, status)) = &project_trust else { continue };
                        let quoted = session.nested_quoted();
                        let said = marv_session::project_trust::trust_project(
                            &spec.config_dir,
                            &spec.root,
                            files,
                            status,
                            &quoted,
                            "the next chat",
                        )
                        .await;
                        print(said.unwrap_or_else(|e| e))
                    }
                    "memory" => print(match session.memory() {
                        Some(m) if !m.personal.is_empty() || !m.project.is_empty() => {
                            let list = |items: &[String]| items.iter().map(|i| format!("- {i}")).collect::<Vec<_>>().join("\n");
                            format!("**Personal**\n{}\n\n**This project**\n{}", list(&m.personal), list(&m.project))
                        }
                        _ => "Marv doesn't remember anything yet.".into(),
                    }),
                    "model" if is_model_id(args) => {
                        let changes = Configure {
                            provider: Some(ProviderChoice::Option(provider_option(&spec, args))),
                            ..Configure::default()
                        };
                        match session.configure(changes) {
                            Ok(()) => {
                                let _ = out.send(json!({"type": "configured", "model": args}));
                                print(format!("Model: `{args}` from the next message."));
                            }
                            Err(e) => print(e.to_string()),
                        }
                    }
                    "model" => {
                        print("Use /model <OpenRouter id>, e.g. /model z-ai/glm-5.3-flash, or the model menu up top.".into())
                    }
                    // Runtime-kernel §11.2: the person sets this chat's context budget (`/compact budget 60000`, or
                    // `default`; `marv_session::commands::compact_budget`). A command line comes only from the page,
                    // never from the model or the mesh.
                    "compact" if marv_session::commands::compact_budget(args).is_some() => {
                        use marv_session::commands::BudgetArg;
                        let change = match marv_session::commands::compact_budget(args) {
                            Some(BudgetArg::Set(n)) => Some(Some(n)),
                            Some(BudgetArg::Default) => Some(None),
                            _ => None,
                        };
                        if let Some(change) = change {
                            if let Err(e) = session.configure(Configure { context_budget: Some(change), ..Configure::default() })
                            {
                                print(e.to_string());
                                continue;
                            }
                            let _ = out.send(budget_json(&session));
                        }
                        print(marv_session::commands::context_budget_text(
                            session.context_budget(),
                            session.context_budget_set(),
                            session.context_used(),
                        ));
                    }
                    "compact" => {
                        let (session, out) = (session.clone(), out.clone());
                        let focus = (!args.is_empty()).then(|| args.to_string());
                        tokio::spawn(async move {
                            let text = match session.compact(focus.as_deref()).await {
                                Ok(marv_session::CompactResult::Compacted { .. }) => {
                                    let _ = out.send(context_json(&session));
                                    "Conversation compacted.".to_string()
                                }
                                Ok(marv_session::CompactResult::NotCompacted { reason, .. }) => {
                                    format!("Not compacted ({}).", reason.as_str())
                                }
                                Err(e) => e.to_string(),
                            };
                            let _ = out.send(json!({"type": "command_output", "text": text}));
                        });
                    }
                    other => print(format!("/{other} isn't in Marv desktop yet. Type /help for what is.")),
                }
            }
            ClientMessage::Interrupt => {
                if let Some((cancel, _)) = &turn {
                    cancel.cancel();
                }
                // A turn a mesh message woke, too.
                session.interrupt();
                approver.cancel_all();
            }
            ClientMessage::Approval { id, decision } => approver.answer(id, decision),
            ClientMessage::Configure { model, max_steps, yolo, context_budget } => {
                let model = model.filter(|m| is_model_id(m));
                let changes = Configure {
                    provider: model.as_deref().map(|m| ProviderChoice::Option(provider_option(&spec, m))),
                    max_steps: max_steps.map(|s| s.min(500)),
                    yolo: yolo.map(|on| on && !yolo_held_off),
                    // Runtime-kernel §11.2: the chat settings' budget, the person's (0: back to the default).
                    context_budget: context_budget.map(|b| (b > 0).then_some(b)),
                    ..Configure::default()
                };
                let yolo = yolo.map(|on| on && !yolo_held_off);
                let reply = match session.configure(changes) {
                    Ok(()) => json!({"type": "configured", "model": model, "maxSteps": max_steps, "yolo": yolo}),
                    Err(e) => json!({"type": "error", "message": e.to_string()}),
                };
                let _ = out.send(reply);
                if context_budget.is_some() {
                    let _ = out.send(budget_json(&session));
                    // Runtime-kernel §11.2: what it is now, and whether the next message compacts.
                    let text = marv_session::commands::context_budget_text(
                        session.context_budget(),
                        session.context_budget_set(),
                        session.context_used(),
                    );
                    let _ = out.send(json!({"type": "notice", "text": text}));
                }
            }
            ClientMessage::Mcp { action, name } => {
                let Some(mcp) = session.mcp() else {
                    let _ = out.send(json!({"type": "mcp", "servers": []}));
                    continue;
                };
                if action != "status" && !spec.manage_mcp {
                    continue;
                }
                let (out, name) = (out.clone(), name.unwrap_or_default());
                // When a server started now reaches the model: the next conversation once this one fixed its tools.
                let when = if session.mcp_fixed() {
                    marv_core::APPLIES_AT_NEXT_START
                } else {
                    marv_mcp::install::proposal::USABLE_NEXT_MESSAGE
                };
                tokio::spawn(async move {
                    let say = |text: String| {
                        let _ = out.send(json!({"type": "command_output", "text": text}));
                    };
                    match action.as_str() {
                        "login" => {
                            let tell = out.clone();
                            let server = name.clone();
                            let authorize: marv_mcp::oauth::Authorize = Arc::new(move |url| {
                                let _ = tell.send(json!({"type": "command_output", "text": format!(
                                    "Log in to **{server}**: your browser is opening. If it doesn't, open [this link]({url})."
                                )}));
                                // The person's own browser, on their machine.
                                marv_mcp::oauth::open_browser(&url, std::env::consts::OS);
                            });
                            match mcp.login(&name, authorize) {
                                Ok(login) => match login.done.await {
                                    Ok(Ok(status)) => say(format!("{name}: logged in · {} tools. {}", status.tools.len(), when)),
                                    Ok(Err(e)) => say(format!("{name}: not logged in: {e}")),
                                    Err(_) => say(format!("{name}: not logged in: the login stopped")),
                                },
                                Err(e) => say(e.to_string()),
                            }
                        }
                        "logout" => match mcp.logout(&name).await {
                            Ok(_) => say(format!("{name}: logged out.")),
                            Err(e) => say(e.to_string()),
                        },
                        "trust" => match mcp.trust_all().await {
                            Ok(started) => say(format!(
                                "{}\n\n{}",
                                started
                                    .iter()
                                    .map(|s| format!("{}: {}", s.name, state_name(s.state)))
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                                when
                            )),
                            Err(e) => say(format!("Couldn't trust the servers: {e}")),
                        },
                        _ => {}
                    }
                    let _ = out.send(mcp_status(&mcp));
                });
            }
            ClientMessage::Hello { .. } => {}
            // Marv's own model is `configure`'s.
            ClientMessage::AgentModels | ClientMessage::AgentModel { .. } => {}
        }
    }
    // The page went away: stop the turn, wait for its cleanup, then close the session.
    if let Some((cancel, work)) = turn {
        cancel.cancel();
        approver.cancel_all();
        let _ = tokio::time::timeout(Duration::from_secs(5), work).await;
    }
    watching.cancel();
    if let Some(r) = &resident {
        r.publish(None);
    }
    let _ = tokio::time::timeout(Duration::from_secs(3), session.close()).await;
    drop(mesh);
}

/// A message on a wired inbox wakes the chat, as `mesh.wake` does in the TUI: only while nothing runs and no approval
/// waits (otherwise it waits in the inbox for the next turn, and a later check picks it up), and within
/// `WAKES_PER_MINUTE` per wire. The woken turn reads it fenced as data from another agent, tainting the session.
/// The person's `mesh.wake` (in `config_dir`, read at each look so a change applies at once) decides which wires wake.
#[allow(clippy::too_many_arguments)]
async fn watch_mesh(
    mesh: marv_session::mesh::MeshHandle,
    session: MarvSession,
    approver: Arc<WebApprover>,
    out: UnboundedSender<Value>,
    limit: Arc<std::sync::Mutex<marv_session::mesh::WakeLimit>>,
    config_dir: PathBuf,
    stop: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = mesh.mail() => {}
            // Mail that came during a turn is still unread after it: look again now and then.
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            _ = stop.cancelled() => return,
        }
        if session.busy() || approver.waiting() {
            continue;
        }
        let wake = mesh_wake_in(&config_dir);
        let Some(by) = check_wake(&mesh, &limit, &out, wake) else { continue };
        run_wake_then(&session, &by, &out, wake, || {}).await;
    }
}

/// The person's `mesh.wake` from their config file in `config_dir` (mk-60m.8), as the TUI resolves it: unset (or no
/// file), only rules' wires wake (`Rules`); `true` every wire; `false` none. A file that can't be read: `Off`, so a
/// broken file never wakes more than the person may have set (the message waits; the next look reads it again).
pub fn mesh_wake_in(config_dir: &std::path::Path) -> marv_session::config::MeshWake {
    match marv_session::config::ConfigStore::new(config_dir).load() {
        Ok(file) => marv_session::config::MeshWake::from_file(file.and_then(|f| f.mesh_wake)),
        Err(_) => marv_session::config::MeshWake::Off,
    }
}

/// Whether the main agent's unread messages may wake it now, as `wake` (the person's `mesh.wake`) allows
/// ([`marv_session::mesh::wake_check_as`]: `Off` never, `Rules` only for a rule's wire, `On` any wire), telling the
/// page about wires over their limit, paused rule wakes and mail that waits (runtime-kernel live UX). The caller has
/// checked the rest: idle, no approval waiting.
pub fn check_wake(
    mesh: &marv_session::mesh::MeshHandle,
    limit: &std::sync::Mutex<marv_session::mesh::WakeLimit>,
    out: &UnboundedSender<Value>,
    wake: marv_session::config::MeshWake,
) -> Option<marv_session::mesh::WakeBy> {
    check_wake_waiting(mesh, limit, out, wake).0
}

/// [`check_wake`], and the unread messages that don't wake the chat that the person wasn't told of yet (each once).
pub fn check_wake_waiting(
    mesh: &marv_session::mesh::MeshHandle,
    limit: &std::sync::Mutex<marv_session::mesh::WakeLimit>,
    out: &UnboundedSender<Value>,
    wake: marv_session::config::MeshWake,
) -> (Option<marv_session::mesh::WakeBy>, Vec<marv_session::mesh::Waiting>) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    let check = marv_session::mesh::wake_check_as(mesh, &mut limit.lock().unwrap_or_else(|e| e.into_inner()), now, wake);
    if check.rule_paused {
        let _ = out.send(json!({"type": "notice", "text": marv_session::mesh::RULE_WAKES_PAUSED}));
    }
    for wire in &check.limited {
        let _ = out.send(json!({"type": "notice", "text": format!(
            "{} woke Marv {} times a minute already: its messages on {} wait for your next message.",
            marv_core::printable(wire.sender.as_str()),
            marv_session::mesh::WAKES_PER_MINUTE,
            marv_core::printable(wire.receptacle.as_str()),
        )}));
    }
    // Runtime-kernel live UX: mail that doesn't wake the chat (a rule's chain at its hop limit, say) is told once,
    // sender and count only.
    marv_session::mesh::log_waiting(mesh, &check);
    for line in marv_session::mesh::waiting_lines(&check, wake) {
        let _ = out.send(json!({"type": "notice", "text": line}));
    }
    (check.wake, check.waiting)
}

/// Told when a wake decided earlier (a sleeping chat's, waiting for a slot) finds mesh wake turned off since.
pub const WAKE_OFF_NOTE: &str =
    "Mesh wake was turned off: the message that would have woken this chat waits for your next message.";

/// Told when a granted chat wakes while its grant is paused or expired (Marv's words, never a model's).
pub const GRANT_NOT_LIVE: &str =
    "This chat's permission to work while you're away is paused or has expired: this turn asks you for everything.";

/// Wakes the session for `by` ([`MarvSession::wake`]: automatic approval off) and sends the turn's events. False when
/// no turn started (the inbox was read meanwhile, or the session is busy). Every wire's messages (`MeshWake::On`).
pub async fn run_wake(session: &MarvSession, by: &marv_session::mesh::WakeBy, out: &UnboundedSender<Value>) -> bool {
    run_wake_then(session, by, out, marv_session::config::MeshWake::On, || {}).await
}

/// [`run_wake`] under the person's `mesh.wake` (`wake`, mk-60m.8: `Off` starts nothing; `Rules` reads only rules'
/// wires' messages, as the TUI's woken turn does), calling `woke` once the woken turn has started (the kernel's
/// `Wake` event, runtime-kernel 3).
pub async fn run_wake_then(
    session: &MarvSession,
    by: &marv_session::mesh::WakeBy,
    out: &UnboundedSender<Value>,
    wake: marv_session::config::MeshWake,
    woke: impl FnOnce(),
) -> bool {
    // Turned off since the wake was decided (a sleeping chat's waits for a slot): nothing starts, and the page is told
    // (`MarvSession::wake_as` refuses `Off` too).
    if !wake.any() {
        let _ = out.send(json!({"type": "notice", "text": WAKE_OFF_NOTE}));
        return false;
    }
    let before = session.usage().totals;
    // Services: a chat whose grant holds, woken by a message on a rule's wire, wakes reading only rules' messages, so
    // the turn carries the grant; the messages on wires the person approved by hand wait for the person's turn (or
    // wake a turn of their own, without the grant). A chat whose grant is paused or expired wakes as an ungranted
    // chat does (it asks for everything), and the page is told so. Any other wake is as before.
    let over_a_rule = session.mesh().is_some_and(|m| marv_session::mesh::over_a_rule(&m, &by.sender, &by.receptacle));
    let granted = over_a_rule && session.grant_live();
    let paused = over_a_rule && !granted && session.has_grant();
    let wake = if granted { marv_session::config::MeshWake::Rules } else { wake };
    match session.wake_as(by, wake) {
        Ok(Some(turn)) => {
            woke();
            let _ = out.send(json!({"type": "notice", "text": by.line()}));
            if paused {
                let _ = out.send(json!({"type": "notice", "text": GRANT_NOT_LIVE}));
            }
            forward(turn, before, session.clone(), out.clone()).await;
            true
        }
        _ => false,
    }
}

/// Runtime-kernel §15.2: the person's message, run by the caller's watcher (Marv desktop's resident chat) rather than
/// the page's loop, so it always goes ahead of a queued wake: the watcher looks at the person's queue first. As the
/// page's `send`: the rule wakes' pause lifts, and the turn's events go out. False when it couldn't start (busy).
pub async fn run_person(
    session: &MarvSession,
    text: &str,
    out: &UnboundedSender<Value>,
    limit: &std::sync::Mutex<marv_session::mesh::WakeLimit>,
) -> bool {
    limit.lock().unwrap_or_else(|e| e.into_inner()).person_typed();
    let before = session.usage().totals;
    match session.send(text, SendOptions { for_model: None, cancel: None }) {
        Ok(turn) => {
            forward(turn, before, session.clone(), out.clone()).await;
            true
        }
        Err(e) => {
            let _ = out.send(json!({"type": "error", "message": e.to_string()}));
            false
        }
    }
}

/// How long [`with_deadline`] waits for a stopped turn to end before giving up on it (review P3-6).
pub const DEADLINE_GRACE: Duration = Duration::from_secs(30);

/// What became of a turn run under [`with_deadline`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deadline {
    /// It ended by itself.
    Ended,
    /// The deadline stopped it, and it ended.
    Stopped,
    /// The deadline stopped it and it didn't end within the grace: the caller drops the session (unloads it).
    Stuck,
}

/// Runtime-kernel §15.4: runs `turn` (an unattended turn: a wake, a timer) under `deadline` of wall time. Past it the
/// kernel stops the turn (a preempt: `interrupt()`, and every approval waiting is cancelled) and waits at most `grace`
/// for it to end. Unless `attended` is set by then (review P3-7: the person sent a message into the turn, so it's
/// theirs now and runs on). The caller pauses the chat and tells the person.
pub async fn with_deadline(
    session: &MarvSession,
    approver: &WebApprover,
    deadline: Duration,
    grace: Duration,
    attended: &std::sync::atomic::AtomicBool,
    turn: impl std::future::Future<Output = ()>,
) -> Deadline {
    tokio::pin!(turn);
    tokio::select! {
        _ = &mut turn => return Deadline::Ended,
        _ = tokio::time::sleep(deadline) => {}
    }
    if attended.load(std::sync::atomic::Ordering::SeqCst) {
        turn.await;
        return Deadline::Ended;
    }
    session.interrupt();
    approver.cancel_all();
    match tokio::time::timeout(grace, turn).await {
        Ok(()) => Deadline::Stopped,
        Err(_) => Deadline::Stuck,
    }
}

/// Runtime-kernel §15.4: a fired timer's turn ([`MarvSession::wake_timer`]), its events sent. False when no turn
/// started (busy).
pub async fn run_timer(session: &MarvSession, timer: &marv_session::timers::TimerWake, out: &UnboundedSender<Value>) -> bool {
    let before = session.usage().totals;
    match session.wake_timer(timer) {
        Ok(turn) => {
            let _ = out.send(json!({"type": "notice", "text": timer.line()}));
            forward(turn, before, session.clone(), out.clone()).await;
            true
        }
        Err(_) => false,
    }
}

/// A `send` the session refused (#184: a mesh wake took it first, say): the text comes back with the reason, so the
/// page puts it back in its queue by this reply, whatever order it gets the wake's events in.
fn send_refused(text: &str, message: &str) -> Value {
    json!({"type": "send_refused", "text": text, "message": message})
}

fn state_name(state: marv_mcp::manager::McpState) -> &'static str {
    use marv_mcp::manager::McpState as S;
    match state {
        S::Untrusted => "untrusted",
        S::Connecting => "connecting",
        S::Connected => "connected",
        S::Failed => "failed",
        S::Removed => "removed",
        S::NeedsLogin => "needs_login",
    }
}

/// The chat's servers as the page shows them (never a token).
fn mcp_status(mcp: &marv_session::mcp_tools::McpHandle) -> Value {
    let servers: Vec<Value> = mcp
        .status()
        .iter()
        .map(|s| {
            json!({
                "name": s.name,
                "scope": if s.source == marv_mcp::config::ServerSource::Project { "folder" } else { "global" },
                "state": state_name(s.state),
                "tools": s.tools.len(),
                "error": s.error.as_deref().map(marv_core::printable),
                "canLogin": s.login.is_some(),
                "loggedIn": s.login.as_ref().is_some_and(|l| l.logged_in),
            })
        })
        .collect();
    json!({"type": "mcp", "servers": servers})
}

fn help(skills: &[LoadedSkill]) -> String {
    let mut text: String = COMMANDS.iter().map(|(name, d)| format!("- `/{name}`: {d}\n")).collect();
    if !skills.is_empty() {
        text.push_str(&format!("\n{} skills: `/{}` and more (see `/skills`).", skills.len(), skills[0].skill.name));
    }
    text
}

fn skills_text(skills: &[LoadedSkill]) -> String {
    if skills.is_empty() {
        return "No skills. Put a SKILL.md in ~/.marv/skills/<name>/ (yours) or .marv/skills/<name>/ (this folder's).".into();
    }
    skills.iter().map(|s| format!("- `/{}`: {}\n", s.skill.name, s.skill.description)).collect()
}

/// The chat's folder's own `.marv/config.json`, through the same allow-list as the TUI and `marv -p`
/// (`marv_session::project_config`): only where the chat loads the folder's files (`Source::Project`, Marv desktop),
/// never marv-web's server.
/// Whether chat `spec` runs without yolo whatever its setting: its folder's own config turns yolo off, or it's the
/// home folder (a desktop Home chat), where yolo is refused. A Marv chat's and a harness chat's alike (H4).
pub fn yolo_held_off(spec: &ChatSpec) -> bool {
    project_config(spec).settings.yolo_off || yolo_refused(&spec.root)
}

/// Yolo isn't allowed in `root`: the home folder or above it (`marv_session::sdk::no_yolo_reason`).
fn yolo_refused(root: &std::path::Path) -> bool {
    marv_session::sdk::no_yolo_reason(root, &marv_session::paths::home_dir()).is_some()
}

pub fn project_config(spec: &ChatSpec) -> marv_session::project_config::ProjectConfig {
    if !spec.sources.contains(&marv_session::sources::Source::Project) {
        return Default::default();
    }
    let user_config = marv_session::config::ConfigStore::new(&spec.config_dir).path;
    // Project trust (#220), as `/trust` stored it for this folder: untrusted, its config only narrows.
    let home = marv_session::paths::home_dir();
    let trusted = marv_session::project_trust::config_trusted(&spec.root, &home, &spec.config_dir, None);
    marv_session::project_config::ProjectConfig::load(&spec.root, &user_config, trusted)
}

async fn open(
    spec: &ChatSpec,
    approver: Arc<WebApprover>,
    out: UnboundedSender<Value>,
    mesh: Option<marv_session::mesh::MeshHandle>,
) -> Result<MarvSession, String> {
    let project = project_config(spec);
    if let Some(note) = &project.note {
        let _ = out.send(json!({"type": "notice", "text": marv_core::printable(note)}));
    }
    let build = |resume: Option<ResumeTarget>| {
        let mut builder = session_builder(spec, approver.clone());
        if let Some(resume) = resume {
            builder = builder.resume(resume);
        }
        // Only where Marv loads the person's own files (the desktop): marv-web's server loads none.
        if spec.sources.contains(&marv_session::sources::Source::User) {
            builder.options.small_model = small_model(&spec.config_dir);
            builder.options.vision_model = vision_model(&spec.config_dir);
            builder.options.cores = Some(core_maker(spec));
        }
        builder.options.mesh = mesh.clone();
        // Runtime-kernel §11.2: the default context budget is the person's own `compactTokens` (their ~/.marv config;
        // never a project's), as in the CLI. marv-web's server reads no user config: the built-in default.
        if spec.sources.contains(&Source::User) {
            let file = marv_session::config::ConfigStore::new(&spec.config_dir).load().ok().flatten();
            builder.options.compact_tokens =
                marv_session::config::resolve_config(file.as_ref(), &marv_session::config::Env::new()).compact_tokens;
        }
        builder.options.grant = spec.grant.clone();
        builder.options.meters = spec.meters.clone();
        builder.options.meter_chat = spec.chat_id.clone();
        // Only the person at this machine (the desktop) is asked at a limit; marv-web's server asks no one (a page's
        // visitor can't spend past its operator's budget).
        builder.options.budget_asks = spec.sources.contains(&Source::User);
        if spec.meters.is_some() {
            let out = out.clone();
            builder.options.on_budget = Some(crate::harness_budget::budget_notifier(out));
        }
        builder.options.timers = spec.timers.clone();
        builder.options.devices_off = spec.devices_off.clone();
        // The folder's own config: what it may set (formatter, checks, yolo off), on top of the chat's settings.
        let o = &mut builder.options;
        project.settings.apply_to(&mut o.formatter, &mut o.checks, &mut o.yolo);
        // The home folder or above: the session refuses yolo there, so a chat set to yolo starts without it.
        o.yolo &= !yolo_refused(&spec.root);
        if let Some(resident) = &spec.resident {
            builder.options.always = Some(resident.always.clone());
            builder.options.mesh_hop = Some(resident.turn_hop.clone());
            builder.options.file_versions = Some(resident.file_versions.clone());
            // Runtime-kernel §9.2: a resident chat (Marv desktop's, which honors the person's rules and serves its
            // own chat to reads: `Chats`) may chat_read another under a read rule.
            builder.options.chat_read = true;
        }
        let out = out.clone();
        builder.options.on_warning = Some(Arc::new(move |text: String| {
            let _ = out.send(json!({"type": "notice", "text": marv_core::printable(&text)}));
        }));
        builder.build()
    };
    match build(spec.resume.clone()).await {
        Ok(session) => Ok(session),
        // A session that was never saved (no message sent) or was deleted: start a new one in the same folder.
        Err(_) if matches!(spec.resume, Some(ResumeTarget::Id(_))) => build(None).await.map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// The chat's session as `spec` says: saved, and its trajectory written where the config has them on (#204).
fn session_builder(spec: &ChatSpec, approver: Arc<WebApprover>) -> SessionBuilder {
    let provider = spec.provider.clone().unwrap_or_else(|| provider_option(spec, &spec.model));
    let mut builder = SessionBuilder::new(&spec.root, provider)
        .approve(approver)
        .max_steps(spec.max_steps)
        .yolo(spec.yolo)
        .persist(Store::Default)
        .trajectories(if spec.trajectories { Store::Default } else { Store::Off })
        .processes(spec.processes)
        .sources(&spec.sources);
    // No `sources`: none of the user's skills, memory, MCP servers or instructions from ~/.marv.
    builder.options.config_dir = Some(spec.config_dir.clone());
    builder.options.mcp_skip = spec.mcp_off.clone();
    builder.options.host_commands = spec.host_commands;
    builder.options.role = spec.role.clone();
    if let Some((name, roster)) = &spec.team {
        builder.options.team = Some(name.clone());
        builder.options.team_roster = roster.clone();
        builder.options.team_writes = spec.team_writes.clone();
        builder.options.team_peers = spec.team_peers.clone();
    }
    builder.options.role_writes = spec.writes.clone();
    if spec.choices {
        builder.options.system_prompt = Some(marv_session::SystemPrompt::Append(CHOICES_PROMPT.into()));
    }
    builder
}

/// The chat's context budget as the page shows it (runtime-kernel §11.2): `contextBudget` (the budget in use, after
/// the window's cap; null when compaction is off) and `contextBudgetSet` (the person's, or null for the default).
fn budget_json(session: &MarvSession) -> Value {
    json!({
        "type": "context_budget",
        "contextBudget": session.context_budget(),
        "contextBudgetSet": session.context_budget_set(),
    })
}

/// The context the chat uses now (runtime-kernel §11.2): after a compaction, the compacted history's estimate.
fn context_json(session: &MarvSession) -> Value {
    json!({"type": "context", "context": session.context_used(), "contextBudget": session.context_budget()})
}

/// Sends a turn's events, then its usage.
async fn forward(
    mut events: marv_session::Turn,
    before: marv_session::usage::Totals,
    session: MarvSession,
    out: UnboundedSender<Value>,
) {
    while let Some(event) = events.next().await {
        if let Some(line) = events::to_json(&event) {
            let _ = out.send(line);
        }
        // Runtime-kernel §11.2: after a page-out the meter shows the compacted context, not the last request's; and
        // after each of the main agent's requests, so the meter fills during a long turn (a team member woken by its
        // lead can run 100 steps before its turn ends and `usage` says it).
        if matches!(
            event,
            marv_session::SessionEvent::Compaction(marv_session::CompactResult::Compacted { .. })
                | marv_session::SessionEvent::Loop(marv_agent::LoopEvent::Usage(..))
        ) {
            let _ = out.send(context_json(&session));
        }
    }
    let after = session.usage();
    let _ = out.send(json!({
        "type": "usage",
        "promptTokens": after.totals.prompt_tokens - before.prompt_tokens,
        "cachedTokens": after.totals.cached_tokens - before.cached_tokens,
        "completionTokens": after.totals.completion_tokens - before.completion_tokens,
        "cost": after.totals.cost.unwrap_or(0.0) - before.cost.unwrap_or(0.0),
        "context": after.last.map(|u| u.prompt_tokens),
        "contextLength": after.context_length,
        "contextBudget": session.context_budget(),
    }));
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Says "hi" to every request.
    struct Hi;
    impl marv_provider::Provider for Hi {
        fn name(&self) -> &str {
            "scripted"
        }
        fn context_length(&self) -> Option<u64> {
            None
        }
        fn stream(&self, _: &[marv_core::ChatTurn], _: marv_provider::StreamOptions) -> marv_provider::EventStream {
            let events = vec![
                marv_core::ProviderEvent::TextDelta("hi".into()),
                marv_core::ProviderEvent::Done { reason: Some("stop".into()) },
            ];
            Box::pin(futures::stream::iter(events))
        }
    }

    /// Runtime-kernel 1: a resident chat's session runs on the mesh its caller gives it, published for the caller's
    /// wakes while it runs; when it ends, the mesh (door, plug, inbox) is still the caller's, untouched.
    #[tokio::test]
    async fn a_resident_chat_runs_on_its_callers_mesh_and_leaves_it_up() {
        let base = std::path::PathBuf::from(format!(
            "/tmp/mw-{}-{}",
            std::process::id(),
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        let (root, config_dir) = (base.join("p"), base.join("c"));
        std::fs::create_dir_all(&root).unwrap();
        // 0.8.0: the runtime folder (`$XDG_RUNTIME_DIR/marv`) is made only inside an existing XDG_RUNTIME_DIR.
        std::fs::create_dir_all(base.join("r")).unwrap();
        let env: marv_mcp::Env = [("XDG_RUNTIME_DIR".to_string(), base.join("r").display().to_string())].into_iter().collect();
        let (out, mut events) = tokio::sync::mpsc::unbounded_channel();
        let mut mesh = marv_session::mesh_process::start(&env, &root, &[], Some("p-554963ee")).unwrap().unwrap();
        let approver = Arc::new(WebApprover::new(out.clone()));
        open_mesh_door(&mut mesh, &approver, &out);
        let socket = base.join("r/marv/mesh/p-554963ee.sock");
        assert!(socket.exists());
        let resident = Resident {
            mesh: Some(mesh.handle.clone()),
            mesh_name: Some(mesh.name.clone()),
            always: Default::default(),
            turn_hop: Default::default(),
            file_versions: Default::default(),
            approver,
            wake_limit: Default::default(),
            session: Default::default(),
            woken: Default::default(),
            harness_calls: Default::default(),
        };
        let spec = ChatSpec {
            root: root.clone(),
            config_dir,
            api_key: String::new(),
            zai_api_key: None,
            kimi_api_key: None,
            chatgpt_auth: None,
            model: "test/model".into(),
            max_steps: 5,
            yolo: false,
            resume: None,
            processes: false,
            sources: Vec::new(),
            mesh: true,
            mcp_off: Vec::new(),
            devices_off: Vec::new(),
            mesh_name: None,
            chat_id: None,
            rules: false,
            manage_mcp: false,
            host_commands: false,
            kernel_events: None,
            timers: None,
            delegates: None,
            role: None,
            team: None,
            team_writes: Vec::new(),
            team_peers: Vec::new(),
            choices: false,
            writes: None,
            harness: Default::default(),
            harness_sandbox: false,
            agent_model: None,
            resident: Some(resident.clone()),
            provider: Some(ProviderOption::Custom(Arc::new(Hi))),
            grant: None,
            trajectories: false,
            meters: None,
        };
        let (to_chat, incoming) = tokio::sync::mpsc::unbounded_channel();
        let running = tokio::spawn(run_chat(spec, incoming, out));
        let ready = loop {
            let event = events.recv().await.unwrap();
            if event["type"] == "ready" {
                break event;
            }
        };
        assert_eq!(ready["mesh"], "p-554963ee");
        let session = resident.session().expect("published while it runs");
        assert!(Arc::ptr_eq(&session.mesh().unwrap(), &mesh.handle));
        drop(to_chat);
        running.await.unwrap();
        assert!(resident.session().is_none(), "unpublished once it ends");
        assert!(socket.exists(), "the door is the caller's: still open");
        drop(mesh);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Marv desktop reads a chat folder's `.marv/config.json` through the same allow-list as the TUI and `marv -p`;
    /// marv-web's server (no `Source::Project`) reads none.
    #[test]
    fn a_desktop_chat_reads_its_folders_config_like_the_tui() {
        let root = std::env::temp_dir().join(format!("mw-project-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(root.join(".marv")).unwrap();
        std::fs::write(root.join(".marv/config.json"), r#"{"yolo": false, "model": "x", "formatter": {"rs": "x"}}"#).unwrap();
        let desktop = ChatSpec { root: root.clone(), sources: vec![marv_session::sources::Source::Project], ..spec() };
        let read = project_config(&desktop);
        assert!(read.settings.yolo_off && read.settings.formatter.is_none(), "untrusted: only narrowing");
        let note = read.note.unwrap_or_default();
        assert!(note.contains("sets \"model\": ignored") && note.contains("until you trust"), "{note}");
        let (mut formatter, mut checks, mut yolo) = (Default::default(), true, true);
        read.settings.apply_to(&mut formatter, &mut checks, &mut yolo);
        assert!(!yolo && !checks, "yolo held off; untrusted, checks held off");
        assert_eq!(project_config(&ChatSpec { root: root.clone(), sources: Vec::new(), ..spec() }), Default::default());
        let _ = std::fs::remove_dir_all(&root);
    }

    pub(crate) fn spec() -> ChatSpec {
        ChatSpec {
            root: PathBuf::from("/tmp"),
            config_dir: PathBuf::from("/tmp/.marv"),
            api_key: "sk-or".into(),
            zai_api_key: Some("zai-key".into()),
            kimi_api_key: Some("kimi-key".into()),
            chatgpt_auth: Some(PathBuf::from("/tmp/.marv/chatgpt.json")),
            model: String::new(),
            max_steps: 0,
            yolo: false,
            resume: None,
            processes: false,
            sources: Vec::new(),
            mesh: false,
            mcp_off: Vec::new(),
            devices_off: Vec::new(),
            mesh_name: None,
            chat_id: None,
            rules: false,
            manage_mcp: false,
            host_commands: false,
            kernel_events: None,
            timers: None,
            delegates: None,
            role: None,
            team: None,
            team_writes: Vec::new(),
            team_peers: Vec::new(),
            choices: false,
            writes: None,
            harness: Default::default(),
            harness_sandbox: false,
            agent_model: None,
            resident: None,
            provider: None,
            grant: None,
            trajectories: true,
            meters: None,
        }
    }

    /// #204: desktop chats write trajectories as the TUI and `marv -p` do, unless the config turns them off.
    #[test]
    fn the_session_writes_trajectories_as_the_config_says() {
        let approver = || Arc::new(WebApprover::new(tokio::sync::mpsc::unbounded_channel().0));
        let on = session_builder(&spec(), approver());
        assert!(matches!(on.options.trajectories, Store::Default));
        assert!(matches!(on.options.persist, Store::Default));
        let off = session_builder(&ChatSpec { trajectories: false, ..spec() }, approver());
        assert!(matches!(off.options.trajectories, Store::Off));

        let dir = PathBuf::from(format!("/tmp/mx-web-traj-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(trajectories_on(&dir), "on by default, with no config file");
        let store = marv_session::config::ConfigStore::new(&dir);
        let config = marv_session::config::FileConfig { trajectories: Some(false), ..Default::default() };
        store.save(&config).unwrap();
        assert!(!trajectories_on(&dir));
        store.save(&marv_session::config::FileConfig { trajectories: Some(true), ..config }).unwrap();
        assert!(trajectories_on(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// mk-60m.8: the desktop's wake reads the person's `mesh.wake` as the TUI does: unset, rules' wires only; `true`
    /// every wire; `false` none.
    #[test]
    fn mesh_wake_is_read_from_the_persons_config() {
        use marv_session::config::{ConfigStore, FileConfig, MeshWake};
        let dir = PathBuf::from(format!("/tmp/mx-web-wake-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(mesh_wake_in(&dir), MeshWake::Rules, "no config file: the default");
        let store = ConfigStore::new(&dir);
        for (wake, expected) in [(Some(true), MeshWake::On), (Some(false), MeshWake::Off), (None, MeshWake::Rules)] {
            store.save(&FileConfig { mesh_wake: wake, ..Default::default() }).unwrap();
            assert_eq!(mesh_wake_in(&dir), expected, "{wake:?}");
        }
        std::fs::write(dir.join("config.json"), "{ not json").unwrap();
        assert_eq!(mesh_wake_in(&dir), MeshWake::Off, "an unreadable file: fail closed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Runtime-kernel §11.2: a desktop chat's default context budget is the person's own `compactTokens`; marv-web's
    /// server (no user sources) keeps the built-in default.
    #[tokio::test]
    async fn the_default_context_budget_is_the_persons_compact_tokens() {
        let base = std::env::temp_dir().join(format!("mw-budget-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
        let (root, config_dir) = (base.join("p"), base.join("c"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join("config.json"), r#"{"compactTokens": 150000}"#).unwrap();
        let (out, _events) = tokio::sync::mpsc::unbounded_channel();
        let mut spec =
            ChatSpec { root: root.clone(), config_dir, provider: Some(ProviderOption::Custom(Arc::new(Hi))), ..spec() };
        spec.sources = vec![Source::User];
        let desktop = open(&spec, Arc::new(WebApprover::new(out.clone())), out.clone(), None).await.unwrap();
        assert_eq!(desktop.context_budget(), Some(150_000));
        desktop.close().await;
        spec.sources = Vec::new();
        let server = open(&spec, Arc::new(WebApprover::new(out.clone())), out, None).await.unwrap();
        assert_eq!(server.context_budget(), Some(marv_session::config::DEFAULT_COMPACT_TOKENS));
        server.close().await;
        let _ = std::fs::remove_dir_all(&base);
    }

    /// #159: the model id's prefix picks the provider, and each gets only its own credential.
    #[test]
    fn the_prefix_picks_the_provider_and_its_own_credential() {
        let s = spec();
        assert!(matches!(provider_option(&s, "zai/glm-5.3"),
            ProviderOption::Zai { ref api_key, ref model, .. } if api_key == "zai-key" && model.as_deref() == Some("glm-5.3")));
        assert!(matches!(provider_option(&s, "kimi/kimi-for-coding"),
            ProviderOption::Kimi { ref api_key, ref model, .. } if api_key == "kimi-key" && model.as_deref() == Some("kimi-for-coding")));
        assert!(matches!(provider_option(&s, "moonshotai/kimi-k3"), ProviderOption::OpenRouter { .. }));
        assert!(matches!(provider_option(&s, "chatgpt/gpt-6.1-sol"),
            ProviderOption::ChatGpt { ref model, ref auth_file } if model == "gpt-6.1-sol" && auth_file.ends_with("chatgpt.json")));
        assert!(
            matches!(provider_option(&s, "z-ai/glm-5.3"),
            ProviderOption::OpenRouter { ref api_key, .. } if api_key == "sk-or"),
            "OpenRouter's z-ai/ models stay OpenRouter's"
        );
        assert!(matches!(provider_option(&s, "ollama/qwen3:14b"), ProviderOption::Ollama { .. }));
        let none = ChatSpec { zai_api_key: None, ..spec() };
        assert!(
            matches!(provider_option(&none, "zai/glm-5.3"), ProviderOption::Zai { ref api_key, .. } if api_key.is_empty()),
            "never the OpenRouter key"
        );
    }

    /// #184: a refused send is a typed reply that carries the text back (not a bare error), like `not_sent`.
    #[test]
    fn a_refused_send_gives_its_text_back() {
        let reply = send_refused("run the tests", "Marv is working");
        assert_eq!(reply["type"], "send_refused");
        assert_eq!(reply["text"], "run the tests");
        assert_eq!(reply["message"], "Marv is working");
    }

    #[test]
    fn a_refused_runtime_dir_override_is_a_notice_that_says_why() {
        // #168: the desktop said nothing when MARV_RUNTIME_DIR was refused; the CLI, -p and the TUI say why.
        let base = PathBuf::from(format!("/tmp/mx-web-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("proj")).unwrap();
        let env = |dir: &std::path::Path| {
            [("MARV_RUNTIME_DIR".to_string(), dir.display().to_string())]
                .into_iter()
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        let text = mesh_refused_notice(&env(&base.join("proj/marv")), &base.join("proj")).unwrap();
        assert!(text.starts_with("Not on the mesh: MARV_RUNTIME_DIR") && text.contains("inside the project"), "{text}");
        let text = mesh_refused_notice(&env(std::path::Path::new("rel/marv")), &base.join("proj")).unwrap();
        assert!(text.contains("isn't an absolute path"), "{text}");
        // Accepted, or not set: nothing to say.
        assert_eq!(mesh_refused_notice(&env(&base.join("marv")), &base.join("proj")), None);
        assert_eq!(mesh_refused_notice(&Default::default(), &base.join("proj")), None);
        let _ = std::fs::remove_dir_all(&base);
    }
}
