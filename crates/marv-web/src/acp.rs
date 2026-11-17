//! Chats run by agents that speak the Agent Client Protocol (the harness plan, phase 4): OpenCode (`opencode acp`),
//! Gemini CLI (`gemini --acp`) and Antigravity (`agy acp`), each the person's own install and login. ACP is JSON-RPC
//! 2.0 over the agent's stdio (one message per line); Marv is the client.
//!
//! - `initialize`, then `session/new` (cwd = the chat's folder; the Marv mesh as a stdio MCP server when the chat is on
//!   it) or, for a saved chat whose agent can, `session/load`, whose replayed history the page already has. The
//!   session id is saved at once (a `session` event, as for Claude Code).
//! - `session/prompt` per message; its reply's `stopReason` ends the turn. `session/update` notifications stream the
//!   reply (`agent_message_chunk`), thinking, tool calls and their updates, usage. Updates for other sessions (an
//!   agent's own subagents) don't show.
//! - `session/request_permission` becomes an approval card ("OpenCode asks"), answered with the agent's own option:
//!   allow once, allow always (its "always", for this session), reject. The person's Stop is `session/cancel`; a
//!   pending card is answered cancelled. `fs/*` and `terminal/*` are declined (the client offers neither).
//! - OpenCode lets its tools run unasked by default: Marv starts it with edits, commands and fetches set to ask
//!   (`OPENCODE_CONFIG_CONTENT`), so they come to the person as cards, as Claude Code's do.
//! - ACP has no system prompt: a chat's start lines (the mesh line, a team member's role and brief) go before its first
//!   message, once per session.
//! - Like Claude Code: its own tools run under its own rules (inside Marv's outer sandbox when on), nothing it reads
//!   taints anything in Marv, its mail is never clean, Marv's keys never reach it.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use marv_core::printable;
use marv_tools::types::{ApprovalRequest, Approver, Decision, Preview, Scope};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::approver::WebApprover;
use crate::host::ChatSpec;
use crate::protocol::ClientMessage;

/// The ACP version this client speaks.
pub const PROTOCOL_VERSION: u64 = 1;

/// Lines of the agent's stderr kept, for a failure's message.
const STDERR_LINES: usize = 20;

/// An agent Marv OS knows how to start over ACP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Preset {
    OpenCode,
    Gemini,
    Antigravity,
}

impl Preset {
    pub const ALL: [Preset; 3] = [Preset::OpenCode, Preset::Gemini, Preset::Antigravity];

    /// Its name in a chat record and on the page.
    pub fn name(self) -> &'static str {
        match self {
            Self::OpenCode => "opencode",
            Self::Gemini => "gemini",
            Self::Antigravity => "antigravity",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }

    /// What the page calls it.
    pub fn title(self) -> &'static str {
        match self {
            Self::OpenCode => "OpenCode",
            Self::Gemini => "Gemini CLI",
            Self::Antigravity => "Antigravity",
        }
    }

    /// The program, and the arguments that start its ACP server.
    pub fn program(self) -> &'static str {
        match self {
            Self::OpenCode => "opencode",
            Self::Gemini => "gemini",
            Self::Antigravity => "agy",
        }
    }

    pub fn args(self) -> &'static [&'static str] {
        match self {
            Self::OpenCode => &["acp"],
            Self::Gemini => &["--acp"],
            Self::Antigravity => &["acp"],
        }
    }

    /// Said when its program isn't found.
    pub fn not_installed(self) -> String {
        let how = match self {
            Self::OpenCode => {
                "Install it from https://opencode.ai, then sign in once in a terminal (`opencode auth login`) if you use a paid provider."
            }
            Self::Gemini => "Install it (`npm i -g @google/gemini-cli`), then sign in once in a terminal (`gemini`).",
            Self::Antigravity => {
                "Install the Antigravity CLI from https://antigravity.google, then sign in once in a terminal (`agy`)."
            }
        };
        format!("{} isn't installed here (no `{}` on your PATH). {how}", self.title(), self.program())
    }

    /// Its own folders under the home (config, login, saved sessions), bound back in writable in the outer sandbox:
    /// the ones that exist.
    pub fn config_paths(self, home: &Path) -> Vec<PathBuf> {
        let rel: &[&str] = match self {
            Self::OpenCode => &[".config/opencode", ".local/share/opencode", ".local/state/opencode", ".cache/opencode"],
            Self::Gemini => &[".gemini"],
            Self::Antigravity => &[".gemini", ".antigravity", ".config/antigravity"],
        };
        rel.iter().map(|r| home.join(r)).filter(|p| p.exists()).collect()
    }

    /// Makes its own config and data folders when they don't exist yet (0700), so they're bound into the outer sandbox
    /// from its first run: else what it saves (its login, its sessions) would go to the sandbox's throwaway home.
    pub fn ensure_config(self, home: &Path) {
        use marv_fs::mode::CreateMode;
        let rel: &[&str] = match self {
            Self::OpenCode => &[".config/opencode", ".local/share/opencode"],
            Self::Gemini | Self::Antigravity => &[".gemini"],
        };
        for r in rel {
            let _ = std::fs::DirBuilder::new().recursive(true).create_mode(0o700).create(home.join(r));
        }
    }

    /// The prefixes of its own environment variables, passed into the outer sandbox.
    pub fn env_prefixes(self) -> &'static [&'static str] {
        match self {
            Self::OpenCode => &["OPENCODE_"],
            Self::Gemini | Self::Antigravity => &["GEMINI_", "GOOGLE_"],
        }
    }

    /// What Marv sets for it: OpenCode asks before edits, commands and fetches, or with `yolo` (H4: as Marv's yolo)
    /// runs edits and commands unasked and still asks before a fetch.
    pub fn own_env(self, yolo: bool) -> Vec<(String, String)> {
        match self {
            Self::OpenCode => vec![("OPENCODE_CONFIG_CONTENT".into(), if yolo { OPENCODE_YOLO } else { OPENCODE_ASKS }.into())],
            _ => Vec::new(),
        }
    }

    /// Whether Marv can set its yolo (`own_env`). Gemini CLI's and Antigravity's ACP modes aren't checked here yet: they
    /// ask by their own settings, and a chat set to yolo says so.
    pub fn takes_yolo(self) -> bool {
        matches!(self, Self::OpenCode)
    }
}

/// OpenCode's permissions in a Marv OS chat: its tools that change files, run commands or fetch ask first.
pub const OPENCODE_ASKS: &str = r#"{"permission":{"edit":"ask","bash":"ask","webfetch":"ask"}}"#;
/// OpenCode's permissions in a chat set to yolo, inside the outer sandbox: what Marv's yolo runs unasked, it runs.
pub const OPENCODE_YOLO: &str = r#"{"permission":{"edit":"allow","bash":"allow","webfetch":"ask"}}"#;

/// The agent's program: the first on the PATH, else where its installers put it.
pub fn find(preset: Preset) -> Option<PathBuf> {
    let name = preset.program();
    let own = format!(".{name}/bin");
    marv_session::paths::find_program(name, &[".local/bin", &own, ".local/share/mise/shims"])
}

/// The approval card for a `session/request_permission`; `once` when the agent offers no "always".
pub fn approval_request(preset: Preset, params: &Value, sandboxed: bool) -> ApprovalRequest {
    let call = &params["toolCall"];
    let input = &call["rawInput"];
    let kind = call["kind"].as_str().unwrap_or("other");
    let tool = call["name"].as_str().or(call["title"].as_str()).unwrap_or(kind).to_string();
    let mut preview = Preview::default();
    let diff = call["content"].as_array().into_iter().flatten().find(|c| c["type"] == "diff");
    match (kind, diff) {
        ("execute", _) => {
            preview.title = "Run a command".into();
            preview.command = input["command"].as_str().or(call["title"].as_str()).map(printable);
        }
        (_, Some(diff)) => {
            preview.title = format!("Change {}", printable(diff["path"].as_str().unwrap_or("a file")));
            let (old, new) = (diff["oldText"].as_str().unwrap_or_default(), diff["newText"].as_str().unwrap_or_default());
            preview.diff = Some(marv_tools::diff::diff_text(old, new, 3).lines);
        }
        _ => {
            preview.title = format!("Use {}", printable(call["title"].as_str().unwrap_or(&tool)));
            let text = serde_json::to_string_pretty(input).unwrap_or_default();
            preview.text = Some(printable(&cut(&text, 2_000)));
        }
    }
    preview.note = Some(format!(
        "{}'s own tool{}",
        preset.title(),
        if sandboxed {
            ", inside Marv's outer sandbox: this folder and its own settings only"
        } else {
            ": Marv's sandbox doesn't apply"
        }
    ));
    let options = params["options"].as_array().cloned().unwrap_or_default();
    let always = options.iter().any(|o| o["kind"] == "allow_always");
    let what = match kind {
        "edit" => "its file edits",
        "execute" => "its commands",
        "fetch" => "its web fetches",
        _ => "calls like this",
    };
    ApprovalRequest {
        label: label(&tool, call),
        scope: Scope { key: format!("acp:{}:{kind}", preset.name()), description: what.into() },
        tool,
        preview,
        agent: Some(preset.title().into()),
        network: false,
        once: !always,
        must_ask: false,
        protected: None,
        door: None,
        reason: true,
        persists: false,
    }
}

/// The reply to a permission request: the agent's own option for the decision (`cancelled` when it has none).
pub fn permission_result(params: &Value, decision: &Decision) -> Value {
    let options = params["options"].as_array().cloned().unwrap_or_default();
    let pick =
        |kinds: &[&str]| kinds.iter().find_map(|k| options.iter().find(|o| o["kind"] == *k)).and_then(|o| o["optionId"].as_str());
    let chosen = match decision {
        Decision::Yes => pick(&["allow_once", "allow_always"]),
        Decision::Always => pick(&["allow_always", "allow_once"]),
        Decision::No | Decision::Declined(_) => pick(&["reject_once", "reject_always"]),
        Decision::Unavailable => None,
    };
    match chosen {
        Some(id) => json!({"outcome": {"outcome": "selected", "optionId": id}}),
        None => json!({"outcome": {"outcome": "cancelled"}}),
    }
}

/// A tool call's short label: its command or path (`crate::claude_code::label`), else its title.
fn label(tool: &str, call: &Value) -> String {
    let from_input = crate::claude_code::label(tool, &call["rawInput"]);
    if from_input != tool {
        return from_input;
    }
    let title = call["title"].as_str().unwrap_or(tool);
    crate::claude_code::label(title, &json!({}))
}

fn cut(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => format!("{}\n… ({} more bytes)", &text[..end], text.len() - end),
        None => text.to_string(),
    }
}

/// A tool call's result text: its content's text parts, a diff's path.
fn content_text(content: &Value) -> String {
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| match c["type"].as_str() {
            Some("content") => c["content"]["text"].as_str().map(str::to_string),
            Some("diff") => Some(format!("changed {}", c["path"].as_str().unwrap_or("a file"))),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The agent's updates as page events, for one session.
#[derive(Default)]
pub struct Translate {
    /// Each running call's tool, label and whether its start was shown, by id.
    calls: HashMap<String, (String, String)>,
    /// How each call that asked was answered.
    pub answered: HashMap<String, &'static str>,
    /// The reply so far, sent whole (`assistant`) before a tool call and at the turn's end.
    text: String,
    /// Context used and its size, and the session's cost so far, as the last `usage_update` said.
    context: Option<(u64, u64)>,
    cost: f64,
    cost_shown: f64,
    /// The person stopped this turn.
    pub stopped: bool,
}

impl Translate {
    fn flush(&mut self) -> Vec<Value> {
        let text = std::mem::take(&mut self.text);
        if text.trim().is_empty() { Vec::new() } else { vec![json!({"type": "assistant", "text": text})] }
    }

    /// The page events for one `session/update`'s `update`.
    pub fn update(&mut self, u: &Value) -> Vec<Value> {
        match u["sessionUpdate"].as_str() {
            Some("agent_message_chunk") => match u["content"]["text"].as_str() {
                Some(text) => {
                    self.text.push_str(text);
                    vec![json!({"type": "text_delta", "text": text})]
                }
                None => Vec::new(),
            },
            Some("agent_thought_chunk") => {
                u["content"]["text"].as_str().map(|t| vec![json!({"type": "thinking_delta", "text": t})]).unwrap_or_default()
            }
            Some("tool_call") | Some("tool_call_update") => {
                let id = u["toolCallId"].as_str().unwrap_or_default().to_string();
                let mut events = Vec::new();
                if !self.calls.contains_key(&id) {
                    events.extend(self.flush());
                    let tool = u["name"].as_str().or(u["kind"].as_str()).unwrap_or("tool").to_string();
                    let shown = label(&tool, u);
                    events.push(json!({"type": "tool_start", "id": id, "tool": tool, "label": shown}));
                    self.calls.insert(id.clone(), (tool, shown));
                } else if u["rawInput"].as_object().is_some_and(|m| !m.is_empty())
                    && let Some((tool, shown)) = self.calls.get_mut(&id)
                {
                    *shown = label(tool, u);
                }
                if matches!(u["status"].as_str(), Some("completed" | "failed"))
                    && let Some((tool, shown)) = self.calls.remove(&id)
                {
                    let error = u["status"] == "failed";
                    let text = content_text(&u["content"]);
                    let lines = text.trim_end().lines().count();
                    let summary =
                        if error { "error".to_string() } else { format!("{lines} line{}", if lines == 1 { "" } else { "s" }) };
                    events.push(json!({
                        "type": "tool_end", "id": id, "tool": tool, "label": shown,
                        "result": crate::events::clip(&text), "summary": summary, "error": error,
                        "approval": self.answered.remove(&id).unwrap_or("none"),
                    }));
                }
                events
            }
            Some("usage_update") => {
                if let (Some(used), Some(size)) = (u["used"].as_u64(), u["size"].as_u64()) {
                    self.context = Some((used, size));
                }
                if let Some(amount) = u["cost"]["amount"].as_f64() {
                    self.cost = amount;
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// The events for the prompt's reply (or its error): the reply's rest, usage, the turn's end.
    pub fn end(&mut self, reply: &Value) -> Vec<Value> {
        let mut events = self.flush();
        // Calls the agent never closed (a stop mid-call) end with the turn.
        for (id, (tool, shown)) in std::mem::take(&mut self.calls) {
            events.push(json!({"type": "tool_end", "id": id, "tool": tool, "label": shown, "result": "", "summary": "stopped", "error": true, "approval": "none"}));
        }
        let usage = &reply["result"]["usage"];
        let n = |k: &str| usage[k].as_u64().unwrap_or(0);
        let prompt = n("inputTokens") + n("cachedReadTokens") + n("cachedWriteTokens");
        let cost = (self.cost - self.cost_shown).max(0.0);
        self.cost_shown = self.cost;
        let (context, window) = self.context.map_or((prompt, None), |(used, size)| (used, Some(size)));
        events.push(json!({
            "type": "usage", "promptTokens": prompt, "cachedTokens": n("cachedReadTokens"),
            "completionTokens": n("outputTokens"), "cost": cost, "context": context, "contextLength": window,
        }));
        let reason = match (reply.get("error"), reply["result"]["stopReason"].as_str()) {
            (Some(_), _) if self.stopped => "interrupted",
            (Some(error), _) => {
                events.push(
                    json!({"type": "error", "message": printable(error["message"].as_str().unwrap_or("the agent failed"))}),
                );
                "error"
            }
            (None, Some("end_turn")) => "end",
            (None, Some("max_tokens" | "max_turn_requests")) => "max_steps",
            (None, Some("cancelled")) => "interrupted",
            (None, Some("refusal")) => {
                events.push(json!({"type": "error", "message": "The agent refused this request."}));
                "error"
            }
            _ if self.stopped => "interrupted",
            _ => "end",
        };
        self.stopped = false;
        events.push(json!({"type": "turn_end", "reason": reason}));
        events
    }
}

/// One running agent: lines to its stdin, its parsed output lines (None once it has exited).
struct Process {
    stdin: UnboundedSender<String>,
    lines: UnboundedReceiver<Option<Value>>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    next_id: u64,
    _child: tokio::process::Child,
}

impl Process {
    fn request(&mut self, method: &str, params: Value) -> u64 {
        self.next_id += 1;
        let _ = self.stdin.send(json!({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params}).to_string());
        self.next_id
    }

    fn notify(&self, method: &str, params: Value) {
        let _ = self.stdin.send(json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string());
    }

    fn said(&self) -> String {
        printable(&self.stderr.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect::<Vec<_>>().join("\n"))
    }

    /// Waits for the reply to request `id`, answering what the agent asks meanwhile (nothing of the client's is
    /// offered during setup) and dropping its notifications (a load's replayed history). None: it exited.
    async fn reply(&mut self, id: u64) -> Option<Value> {
        loop {
            let line = self.lines.recv().await.flatten()?;
            if line["id"].as_u64() == Some(id) && line.get("method").is_none() {
                return Some(line);
            }
            if line.get("method").is_some() && line.get("id").is_some() {
                let reply = json!({"jsonrpc": "2.0", "id": line["id"], "error": {"code": -32601, "message": "Not now."}});
                let _ = self.stdin.send(reply.to_string());
            }
        }
    }
}

fn spawn(launch: &crate::claude_code::Launch, preset: Preset, root: &Path) -> std::io::Result<Process> {
    let args: Vec<String> = preset.args().iter().map(|a| a.to_string()).collect();
    let mut command = launch.command(&args)?;
    if matches!(launch, crate::claude_code::Launch::Direct(_)) {
        // Direct: outside the outer sandbox, so never yolo (`claude_code::harness_yolo`).
        command.envs(preset.own_env(false));
    }
    let mut child = command
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let (stdin_tx, mut stdin_rx) = mpsc::unbounded_channel::<String>();
    let mut stdin = child.stdin.take().ok_or_else(|| std::io::Error::other("no stdin"))?;
    tokio::spawn(async move {
        while let Some(line) = stdin_rx.recv().await {
            if stdin.write_all(format!("{line}\n").as_bytes()).await.is_err() || stdin.flush().await.is_err() {
                break;
            }
        }
    });
    let (lines_tx, lines) = mpsc::unbounded_channel();
    let stdout = child.stdout.take().ok_or_else(|| std::io::Error::other("no stdout"))?;
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                let _ = lines_tx.send(Some(value));
            }
        }
        let _ = lines_tx.send(None);
    });
    let stderr = Arc::new(Mutex::new(VecDeque::new()));
    if let Some(err) = child.stderr.take() {
        let kept = stderr.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(err).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let mut kept = kept.lock().unwrap_or_else(|e| e.into_inner());
                if kept.len() == STDERR_LINES {
                    kept.pop_front();
                }
                kept.push_back(line);
            }
        });
    }
    Ok(Process { stdin: stdin_tx, lines, stderr, next_id: 0, _child: child })
}

/// The chat's mesh as an ACP stdio MCP server (`session/new`'s `mcpServers`): none when it's not on the mesh.
fn mcp_servers(mesh: &Option<(PathBuf, PathBuf)>) -> Value {
    match mesh {
        Some((exe, socket)) => json!([{
            "name": crate::harness_mesh::SERVER,
            "command": exe.display().to_string(),
            "args": [marv_session::helper_modes::MESH_MCP_SUBCOMMAND, socket.display().to_string()],
            "env": [],
        }]),
        None => json!([]),
    }
}

/// Runs chat `spec` with `preset` until the page goes away, as [`crate::host::run_chat`].
pub async fn run_chat(preset: Preset, spec: ChatSpec, incoming: UnboundedReceiver<ClientMessage>, out: UnboundedSender<Value>) {
    let Some(bin) = find(preset) else {
        let _ = out.send(json!({"type": "fatal", "message": preset.not_installed()}));
        return;
    };
    run_chat_with(preset, bin, spec, incoming, out).await
}

/// An agent's models, from its `session/new` or `session/load` answer, as the page's `agent_models` lists them, and
/// the current one, and the config option that sets it (`Some`: `session/set_config_option` with that id; `None`:
/// `session/set_model`). Agents say them as a config option of category `model` (`configOptions`, OpenCode) or, in
/// ACP's older form, as `models` (`availableModels`, `currentModelId`). None listed: it offers no choice.
pub fn session_models(result: &Value) -> (Vec<Value>, Option<String>, Option<String>) {
    let option =
        result["configOptions"].as_array().into_iter().flatten().find(|o| o["category"] == "model" && o["type"] == "select");
    if let Some(option) = option {
        let list = option["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| {
                let id = m["value"].as_str().filter(|v| crate::protocol::is_agent_model(v))?;
                let name = m["name"].as_str().unwrap_or(id);
                Some(json!({"id": id, "name": printable(name), "description": printable(m["description"].as_str().unwrap_or_default())}))
            })
            .collect();
        return (list, option["currentValue"].as_str().map(String::from), option["id"].as_str().map(String::from));
    }
    let list = result["models"]["availableModels"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m["modelId"].as_str().filter(|v| crate::protocol::is_agent_model(v))?;
            let name = m["name"].as_str().unwrap_or(id);
            Some(json!({"id": id, "name": printable(name), "description": printable(m["description"].as_str().unwrap_or_default())}))
        })
        .collect();
    (list, result["models"]["currentModelId"].as_str().map(String::from), None)
}

/// The request that switches session `session` to `model`: through its model config option when it has one.
pub fn set_model_request(session: &str, config: Option<&str>, model: &str) -> (&'static str, Value) {
    match config {
        Some(id) => ("session/set_config_option", json!({"sessionId": session, "configId": id, "value": model})),
        None => ("session/set_model", json!({"sessionId": session, "modelId": model})),
    }
}

/// A started agent's session: its id, whether it's new, its models and the current one.
struct Opened {
    session: String,
    fresh: bool,
    models: Vec<Value>,
    current: Option<String>,
    /// The config option that sets the model ([`session_models`]).
    config: Option<String>,
}

/// Starts the agent and its session ([`open`]), on the chat's model when it has one and the agent lists it.
async fn boot(
    launch: &crate::claude_code::Launch,
    preset: Preset,
    spec: &ChatSpec,
    saved: Option<&str>,
    mesh: &Option<(PathBuf, PathBuf)>,
    wanted: Option<&str>,
    out: &UnboundedSender<Value>,
) -> Result<(Process, Opened), String> {
    let mut p = spawn(launch, preset, &spec.root).map_err(|e| format!("Couldn't start {}: {e}", preset.title()))?;
    let mut opened =
        open(&mut p, spec, saved, mesh, out).await.map_err(|why| format!("{} couldn't start: {why}", preset.title()))?;
    if let Some(wanted) = wanted.filter(|w| Some(*w) != opened.current.as_deref() && opened.models.iter().any(|m| m["id"] == *w))
    {
        let (method, params) = set_model_request(&opened.session, opened.config.as_deref(), wanted);
        let id = p.request(method, params);
        match p.reply(id).await {
            Some(reply) if reply.get("error").is_none() => opened.current = Some(wanted.to_string()),
            _ => {
                let _ = out.send(json!({"type": "notice", "text": format!("{} couldn't switch to {wanted}, so it uses its own model.", preset.title())}));
            }
        }
    }
    Ok((p, opened))
}

/// Starts the agent and its session (loading the saved one when it can).
async fn open(
    p: &mut Process,
    spec: &ChatSpec,
    saved: Option<&str>,
    mesh: &Option<(PathBuf, PathBuf)>,
    out: &UnboundedSender<Value>,
) -> Result<Opened, String> {
    let init = json!({"protocolVersion": PROTOCOL_VERSION, "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false}, "clientInfo": {"name": "marv-os", "version": env!("CARGO_PKG_VERSION")}});
    let id = p.request("initialize", init);
    let reply = p.reply(id).await.ok_or_else(|| format!("it stopped while starting: {}", p.said()))?;
    if let Some(error) = reply.get("error") {
        return Err(printable(error["message"].as_str().unwrap_or("it refused to start")));
    }
    let loads = reply["result"]["agentCapabilities"]["loadSession"].as_bool().unwrap_or(false);
    let cwd = spec.root.display().to_string();
    if let Some(saved) = saved.filter(|_| loads) {
        let id = p.request("session/load", json!({"sessionId": saved, "cwd": cwd, "mcpServers": mcp_servers(mesh)}));
        match p.reply(id).await {
            Some(reply) if reply.get("error").is_none() => {
                let (models, current, config) = session_models(&reply["result"]);
                return Ok(Opened { session: saved.to_string(), fresh: false, models, current, config });
            }
            Some(_) => {
                let _ = out.send(json!({"type": "notice", "text": "The agent no longer has this chat's conversation, so this message starts a new one."}));
            }
            None => return Err(format!("it stopped while loading the conversation: {}", p.said())),
        }
    } else if saved.is_some() {
        let _ = out.send(json!({"type": "notice", "text": "This agent can't continue a saved conversation, so this message starts a new one."}));
    }
    let id = p.request("session/new", json!({"cwd": cwd, "mcpServers": mcp_servers(mesh)}));
    let reply = p.reply(id).await.ok_or_else(|| format!("it stopped while starting a session: {}", p.said()))?;
    if let Some(error) = reply.get("error") {
        return Err(printable(error["message"].as_str().unwrap_or("it couldn't start a session")));
    }
    let session = reply["result"]["sessionId"].as_str().ok_or("it gave no session id")?.to_string();
    let (models, current, config) = session_models(&reply["result"]);
    Ok(Opened { session, fresh: true, models, current, config })
}

/// [`run_chat`] with this program (the guards' stand-in).
pub async fn run_chat_with(
    preset: Preset,
    bin: PathBuf,
    spec: ChatSpec,
    mut incoming: UnboundedReceiver<ClientMessage>,
    out: UnboundedSender<Value>,
) {
    let approver = match &spec.resident {
        Some(r) => r.approver.clone(),
        None => Arc::new(WebApprover::new(out.clone())),
    };
    let mut saved = match &spec.resume {
        Some(marv_session::ResumeTarget::Id(id)) => Some(id.clone()),
        _ => None,
    };
    let _ = out.send(json!({
        "type": "ready", "sessionId": saved, "model": preset.title(), "commands": [],
        "mesh": spec.resident.as_ref().and_then(|r| r.mesh_name.clone()), "harness": preset.name(),
        "agentModel": spec.agent_model,
    }));
    let brief = match crate::claude_code::member_brief(&spec, preset.title(), "the marv-mesh tools") {
        Ok(brief) => brief,
        Err(e) => {
            let _ = out.send(json!({"type": "fatal", "message": format!("This chat's role: {}", printable(&e))}));
            return;
        }
    };
    if let Some(why) = crate::claude_code::home_unsandboxed(&spec, preset.title()) {
        let _ = out.send(json!({"type": "fatal", "message": why}));
        return;
    }
    let server = crate::claude_code::mesh_server(&spec, &approver);
    let home = marv_session::paths::home_dir();
    if spec.harness_sandbox {
        preset.ensure_config(&home);
    }
    let (launch, mesh) = crate::claude_code::launch_for(
        &bin,
        preset.program(),
        &preset.config_paths(&home),
        preset.env_prefixes(),
        Vec::new(),
        &spec,
        server.as_ref(),
        &out,
    );
    let mut yolo = crate::claude_code::harness_yolo(spec.yolo, &spec, &launch, preset.title(), &out);
    if yolo && !preset.takes_yolo() {
        let text = format!("{} asks by its own settings: Marv OS can't turn on its yolo yet.", preset.title());
        let _ = out.send(json!({"type": "notice", "text": text}));
        yolo = false;
    }
    let launch = launch.with_env(preset.own_env(yolo));
    let sandboxed = matches!(launch, crate::claude_code::Launch::Sandboxed { .. });
    let mut start_lines: Vec<String> = Vec::new();
    if mesh.is_some() {
        start_lines.push(format!(
            "You are a chat in Marv OS. {}",
            crate::claude_code::MESH_PROMPT.replace("mcp__marv-mesh tools", "marv-mesh tools")
        ));
    }
    start_lines.extend(brief);
    let mut process: Option<Process> = None;
    let mut session: Option<String> = None;
    let mut new_session = false;
    let mut prompt: Option<u64> = None;
    let mut translate = Translate::default();
    // The model the person picked (the agent's own when None), its models once a session said them, and a pending
    // `session/set_model`'s request id with the model it asks for.
    let mut wanted = spec.agent_model.clone().filter(|m| crate::protocol::is_agent_model(m));
    let mut models: Option<(Vec<Value>, Option<String>)> = None;
    let mut model_config: Option<String> = None;
    let mut switching: Option<(u64, String)> = None;
    let (answered_tx, mut answered_rx) = mpsc::unbounded_channel::<(String, &'static str, Value)>();
    // H2: each turn meets the person's budgets first (a task: its card is answered through this loop).
    let budget = crate::harness_budget::HarnessBudget::new(&spec, preset.name(), approver.clone(), out.clone());
    let (gated_tx, mut gated_rx) = mpsc::unbounded_channel::<(String, bool)>();
    let mut gating: Option<tokio_util::sync::CancellationToken> = None;
    loop {
        tokio::select! {
            Some((first, ok)) = gated_rx.recv() => {
                gating = None;
                match (process.as_mut(), session.clone(), ok) {
                    (Some(p), Some(id), true) => {
                        prompt = Some(p.request("session/prompt", json!({"sessionId": id, "prompt": [{"type": "text", "text": first}]})));
                    }
                    _ => {
                        let _ = out.send(json!({"type": "unsent", "text": first}));
                        let _ = out.send(json!({"type": "turn_end", "reason": "interrupted"}));
                    }
                }
            }
            message = incoming.recv() => {
                let Some(message) = message else { break };
                match message {
                    ClientMessage::Send { text } => {
                        let text = text.trim().to_string();
                        if text.is_empty() {
                            continue;
                        }
                        if prompt.is_some() || gating.is_some() {
                            let _ = out.send(json!({"type": "send_refused", "text": text, "message": format!("{} is still working on the last message.", preset.title())}));
                            continue;
                        }
                        if process.is_none() {
                            match boot(&launch, preset, &spec, saved.as_deref(), &mesh, wanted.as_deref(), &out).await {
                                Ok((started, opened)) => {
                                    if opened.fresh {
                                        let _ = out.send(json!({"type": "session", "sessionId": opened.session}));
                                    }
                                    let _ = out.send(json!({"type": "agent_models", "models": opened.models, "current": opened.current}));
                                    models = Some((opened.models, opened.current));
                                    model_config = opened.config;
                                    saved = Some(opened.session.clone());
                                    (session, new_session) = (Some(opened.session), opened.fresh);
                                    process = Some(started);
                                    translate = Translate::default();
                                }
                                Err(why) => {
                                    let _ = out.send(json!({"type": "send_refused", "text": text, "message": why}));
                                    continue;
                                }
                            }
                        }
                        if process.is_none() || session.is_none() {
                            continue;
                        }
                        let mut first = text;
                        if std::mem::take(&mut new_session) && !start_lines.is_empty() {
                            first = format!("[Marv OS: how this chat works]\n{}\n\n---\n\n{first}", start_lines.join("\n\n"));
                        }
                        let _ = out.send(json!({"type": "turn_start"}));
                        let (budget, gated, stop) = (budget.clone(), gated_tx.clone(), tokio_util::sync::CancellationToken::new());
                        gating = Some(stop.clone());
                        tokio::spawn(async move {
                            let ok = budget.begin(&stop).await;
                            let _ = gated.send((first, ok));
                        });
                    }
                    ClientMessage::SendNow { text } => {
                        let _ = out.send(json!({"type": "not_sent", "texts": [text.trim()]}));
                    }
                    ClientMessage::PasteImage { media_type, data } => {
                        let _ = out.send(crate::host::pasted_image(&spec.config_dir, &media_type, &data));
                    }
                    ClientMessage::Command { line } => {
                        let _ = out.send(json!({"type": "command_output", "text": format!(
                            "{} isn't available in a {} chat: Marv's commands are for Marv chats.",
                            printable(line.trim()), preset.title()
                        )}));
                    }
                    ClientMessage::Interrupt => {
                        if let Some(stop) = gating.take() {
                            stop.cancel();
                        } else if prompt.is_some() && let (Some(p), Some(id)) = (&process, &session) {
                            translate.stopped = true;
                            p.notify("session/cancel", json!({"sessionId": id}));
                        }
                        approver.cancel_all();
                    }
                    ClientMessage::Approval { id, decision } => approver.answer(id, decision),
                    ClientMessage::Configure { yolo: Some(on), .. } => {
                        // H4: an agent's permissions are set when it starts, and not every agent reloads its session,
                        // so a change waits for its next start (after it sleeps, or the app restarts).
                        let text = format!("{} gets the new yolo setting when it next starts (after it sleeps or Marv OS restarts).", preset.title());
                        let _ = out.send(json!({"type": "notice", "text": text}));
                        let _ = out.send(json!({"type": "configured", "yolo": on && yolo}));
                    }
                    ClientMessage::Configure { .. } => {
                        let _ = out.send(json!({"type": "configured"}));
                    }
                    ClientMessage::Mcp { .. } => {
                        let _ = out.send(json!({"type": "mcp", "servers": []}));
                    }
                    ClientMessage::Hello { .. } => {}
                    ClientMessage::AgentModels => {
                        if let Some((list, current)) = &models {
                            let _ = out.send(json!({"type": "agent_models", "models": list, "current": current}));
                            continue;
                        }
                        // Its session says them: started now, with no message yet.
                        match boot(&launch, preset, &spec, saved.as_deref(), &mesh, wanted.as_deref(), &out).await {
                            Ok((started, opened)) => {
                                if opened.fresh {
                                    let _ = out.send(json!({"type": "session", "sessionId": opened.session}));
                                }
                                let _ = out.send(json!({"type": "agent_models", "models": opened.models, "current": opened.current}));
                                models = Some((opened.models, opened.current));
                                model_config = opened.config;
                                saved = Some(opened.session.clone());
                                (session, new_session) = (Some(opened.session), opened.fresh);
                                process = Some(started);
                                translate = Translate::default();
                            }
                            Err(why) => {
                                let _ = out.send(json!({"type": "agent_models", "models": [], "error": why}));
                            }
                        }
                    }
                    ClientMessage::AgentModel { model } => {
                        if !crate::protocol::is_agent_model(&model) {
                            continue;
                        }
                        wanted = Some(model.clone());
                        if let (Some(p), Some(id)) = (process.as_mut(), &session) {
                            let (method, params) = set_model_request(id, model_config.as_deref(), &model);
                            let request = p.request(method, params);
                            switching = Some((request, model.clone()));
                        }
                        let _ = out.send(json!({"type": "agent_model", "model": model}));
                    }
                }
            }
            Some((call, how, reply)) = answered_rx.recv() => {
                if !call.is_empty() {
                    translate.answered.insert(call, how);
                }
                if let Some(p) = &process {
                    let _ = p.stdin.send(reply.to_string());
                }
            }
            line = async { process.as_mut()?.lines.recv().await }, if process.is_some() => {
                match line.flatten() {
                    Some(m) if m.get("method").is_some() && m.get("id").is_some() => {
                        if m["method"] != "session/request_permission" {
                            if let Some(p) = &process {
                                let reply = json!({"jsonrpc": "2.0", "id": m["id"], "error": {"code": -32601, "message": "Marv OS doesn't offer this."}});
                                let _ = p.stdin.send(reply.to_string());
                            }
                            continue;
                        }
                        let (approver, answered, params) = (approver.clone(), answered_tx.clone(), m["params"].clone());
                        let card = crate::claude_code::home_card(approval_request(preset, &params, sandboxed), &spec);
                        tokio::spawn(async move {
                            let decision = approver.approve(card).await;
                            let how = match decision {
                                Decision::Yes => "yes",
                                Decision::Always => "always",
                                _ => "no",
                            };
                            let call = params["toolCall"]["toolCallId"].as_str().unwrap_or_default().to_string();
                            let reply = json!({"jsonrpc": "2.0", "id": m["id"], "result": permission_result(&params, &decision)});
                            let _ = answered.send((call, how, reply));
                        });
                    }
                    Some(m) if m["method"] == "session/update" => {
                        if m["params"]["sessionId"].as_str() != session.as_deref() {
                            continue;
                        }
                        for event in translate.update(&m["params"]["update"]) {
                            let _ = out.send(event);
                        }
                    }
                    Some(m) if m.get("method").is_none() && switching.as_ref().is_some_and(|(id, _)| m["id"].as_u64() == Some(*id)) => {
                        let Some((_, model)) = switching.take() else { continue };
                        if let Some(error) = m.get("error") {
                            let why = printable(error["message"].as_str().unwrap_or("it refused"));
                            let _ = out.send(json!({"type": "notice", "text": format!("{} couldn't switch to {model}: {why}", preset.title())}));
                        } else if let Some((_, current)) = models.as_mut() {
                            *current = Some(model);
                        }
                    }
                    Some(m) if m.get("method").is_none() && prompt.is_some() && m["id"].as_u64() == prompt => {
                        prompt = None;
                        approver.cancel_all();
                        for event in translate.end(&m) {
                            budget.observe(&event);
                            let _ = out.send(event);
                        }
                    }
                    Some(_) => {}
                    None => {
                        // It exited: what it said, and the turn it was in ends. The next message starts it again.
                        let said = process.take().map(|p| p.said()).unwrap_or_default();
                        session = None;
                        approver.cancel_all();
                        if prompt.take().is_some() || !said.trim().is_empty() {
                            let tail = if said.trim().is_empty() { "no message".to_string() } else { cut(said.trim(), 1_500) };
                            let _ = out.send(json!({"type": "error", "message": format!("{} stopped: {tail}", preset.title())}));
                            budget.observe(&json!({"type": "turn_end"}));
                            let _ = out.send(json!({"type": "turn_end", "reason": "error"}));
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sessions_models_are_listed_with_the_current_one() {
        let result = json!({"sessionId": "s", "models": {"currentModelId": "opencode/big-pickle", "availableModels": [
            {"modelId": "opencode/big-pickle", "name": "Big Pickle"},
            {"modelId": "anthropic/claude-sonnet-5-5", "name": "Claude Sonnet 5.5", "description": "Anthropic"},
            {"modelId": "--yolo", "name": "not a model"},
        ]}});
        let (models, current, config) = session_models(&result);
        assert_eq!((current.as_deref(), config), (Some("opencode/big-pickle"), None), "ACP's older form");
        assert_eq!(
            models,
            [
                json!({"id": "opencode/big-pickle", "name": "Big Pickle", "description": ""}),
                json!({"id": "anthropic/claude-sonnet-5-5", "name": "Claude Sonnet 5.5", "description": "Anthropic"}),
            ]
        );
        assert_eq!(session_models(&json!({"sessionId": "s"})), (Vec::new(), None, None), "an agent with no choice");
        let result = json!({"sessionId": "s", "configOptions": [
            {"id": "mode", "category": "mode", "type": "select", "currentValue": "build", "options": [{"value": "build", "name": "Build"}]},
            {"id": "model", "category": "model", "type": "select", "currentValue": "opencode/big-pickle", "options": [
                {"value": "opencode/big-pickle", "name": "opencode/Big Pickle"}, {"value": "opencode/beam-free", "name": "opencode/Beam Free"}]},
        ]});
        let (models, current, config) = session_models(&result);
        assert_eq!(
            (models.len(), current.as_deref(), config.as_deref()),
            (2, Some("opencode/big-pickle"), Some("model")),
            "a model config option"
        );
        assert_eq!(
            set_model_request("s", Some("model"), "opencode/beam-free"),
            ("session/set_config_option", json!({"sessionId": "s", "configId": "model", "value": "opencode/beam-free"}))
        );
    }

    #[test]
    fn names_round_trip_and_each_preset_starts_its_acp_server() {
        for p in Preset::ALL {
            assert_eq!(Preset::from_name(p.name()), Some(p));
        }
        assert_eq!((Preset::OpenCode.program(), Preset::OpenCode.args()), ("opencode", &["acp"][..]));
        assert_eq!((Preset::Gemini.program(), Preset::Gemini.args()), ("gemini", &["--acp"][..]));
        assert_eq!((Preset::Antigravity.program(), Preset::Antigravity.args()), ("agy", &["acp"][..]));
        assert!(OPENCODE_ASKS.contains(r#""bash":"ask""#) && OPENCODE_ASKS.contains(r#""edit":"ask""#));
    }

    /// OpenCode 2.2.3's permission request and updates as recorded on 2026-11-08, trimmed.
    #[test]
    fn a_permission_request_is_a_card_and_the_agents_own_option_answers_it() {
        let params = json!({"sessionId": "s", "toolCall": {"toolCallId": "c1", "name": "write", "title": "/p/probe.txt", "kind": "edit", "rawInput": {"path": "/p/probe.txt", "content": "hi\n"}, "content": [{"type": "diff", "path": "/p/probe.txt", "oldText": null, "newText": "hi\n"}]}, "options": [{"optionId": "once", "kind": "allow_once", "name": "Allow once"}, {"optionId": "always", "kind": "allow_always", "name": "Always allow"}, {"optionId": "reject", "kind": "reject_once", "name": "Reject"}]});
        let card = approval_request(Preset::OpenCode, &params, true);
        assert_eq!((card.agent.as_deref(), card.label.as_str(), card.once), (Some("OpenCode"), "/p/probe.txt", false));
        assert!(card.preview.diff.is_some() && card.preview.note.as_deref().unwrap().contains("outer sandbox"));
        assert_eq!(permission_result(&params, &Decision::Yes)["outcome"]["optionId"], "once");
        assert_eq!(permission_result(&params, &Decision::Always)["outcome"]["optionId"], "always");
        assert_eq!(permission_result(&params, &Decision::Declined("no".into()))["outcome"]["optionId"], "reject");
        assert_eq!(permission_result(&params, &Decision::Unavailable)["outcome"]["outcome"], "cancelled");
        let shell = json!({"toolCall": {"toolCallId": "c2", "name": "shell", "title": "cat probe.txt", "kind": "execute", "rawInput": {"command": "cat probe.txt"}}, "options": [{"optionId": "once", "kind": "allow_once"}]});
        let card = approval_request(Preset::OpenCode, &shell, false);
        assert_eq!((card.preview.command.as_deref(), card.once), (Some("cat probe.txt"), true));
    }

    #[test]
    fn the_agents_updates_become_the_pages_events() {
        let mut t = Translate::default();
        let chunk = json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "I'll write it."}});
        assert_eq!(t.update(&chunk), [json!({"type": "text_delta", "text": "I'll write it."})]);
        let call = json!({"sessionUpdate": "tool_call", "toolCallId": "c1", "name": "write", "title": "write", "kind": "edit", "status": "pending", "rawInput": {}});
        let events = t.update(&call);
        assert_eq!(events[0], json!({"type": "assistant", "text": "I'll write it."}), "the reply so far, whole");
        assert_eq!((events[1]["type"].as_str(), events[1]["tool"].as_str()), (Some("tool_start"), Some("write")));
        let running = json!({"sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "in_progress", "rawInput": {"path": "/p/probe.txt", "content": "hi\n"}});
        assert!(t.update(&running).is_empty());
        t.answered.insert("c1".into(), "yes");
        let done = json!({"sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "completed", "content": [{"type": "content", "content": {"type": "text", "text": "Created file successfully: probe.txt"}}]});
        let end = &t.update(&done)[0];
        assert_eq!(
            (end["type"].as_str(), end["label"].as_str(), end["approval"].as_str()),
            (Some("tool_end"), Some("/p/probe.txt"), Some("yes"))
        );
        t.update(
            &json!({"sessionUpdate": "usage_update", "used": 8413, "size": 200000, "cost": {"amount": 0.02, "currency": "USD"}}),
        );
        let reply = json!({"id": 3, "result": {"stopReason": "end_turn", "usage": {"inputTokens": 6384, "outputTokens": 168, "cachedReadTokens": 18560}}});
        let events = t.end(&reply);
        let usage = events.iter().find(|e| e["type"] == "usage").unwrap();
        assert_eq!((usage["context"].as_u64(), usage["contextLength"].as_u64()), (Some(8413), Some(200000)));
        assert!((usage["cost"].as_f64().unwrap() - 0.02).abs() < 1e-9);
        assert_eq!(events.last().unwrap(), &json!({"type": "turn_end", "reason": "end"}));
        t.stopped = true;
        assert_eq!(t.end(&json!({"result": {"stopReason": "cancelled"}})).last().unwrap()["reason"], "interrupted");
        let failed = t.end(&json!({"error": {"message": "Gemini API key is missing or not configured."}}));
        assert!(failed.iter().any(|e| e["type"] == "error" && e["message"].as_str().unwrap().contains("API key")));
    }
}
