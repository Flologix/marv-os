//! Marv OS's chats are the coding agents' own terminals: Claude Code, Codex, OpenCode, Gemini CLI, Pi, Marv, Cursor
//! Agent, Crush… whichever the person has. A chat runs its agent's own interface in a terminal (the desktop's
//! `terminals`), with its slash commands, model and provider pickers and permission prompts, exactly as in a terminal;
//! Marv OS keeps the folders, the dock (lazygit), Activity and the teams around it.
//!
//! - **Its conversation:** each agent continues its own way ([`Resume`]): Claude Code and Gemini CLI by a session id
//!   Marv gives (so each chat has its own), the others by "continue the latest in this folder" (two chats with the same
//!   agent in one folder may swap after a restart; while the app runs each terminal stays alive).
//! - **Its access:** as in the person's terminal while they watch; inside Marv's outer sandbox in a Home chat (the home's
//!   secrets hidden). Marv itself never runs in the outer sandbox: it sandboxes its own commands.
//! - **Claude Code** also gets its mesh tools, team brief and Activity hooks ([`crate::claude_code::terminal_command`]).

use std::path::{Path, PathBuf};

use crate::claude_code::TerminalCommand;
use crate::host::ChatSpec;

/// How an agent continues a chat's conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resume {
    /// A new session under an id Marv gives (`new`, then the id), continued by it (`again`, then the id).
    ById { new: &'static str, again: &'static str },
    /// A new session as it starts; continued with these arguments (the latest in this folder).
    Latest(&'static [&'static str]),
    /// It starts afresh each time.
    Fresh,
}

/// One coding agent Marv OS can run in a chat's terminal.
#[derive(Debug)]
pub struct Agent {
    /// Its name in a chat record (`Chat::harness`); `marv` is Marv (a record without one too).
    pub id: &'static str,
    pub title: &'static str,
    pub program: &'static str,
    pub resume: Resume,
    /// Its own config and data folders under the home, shown writable inside the outer sandbox (a Home chat).
    pub config: &'static [&'static str],
    /// Its own variables' prefixes, passed into the outer sandbox.
    pub env: &'static [&'static str],
    /// Where to get it.
    pub install: &'static str,
    /// Its own package in winget (Windows' package manager), for Install in the new chat's picker there: none for an
    /// agent with no official one (Gemini CLI is npm only) and for Marv, which comes with Marv OS.
    pub winget: Option<&'static str>,
    /// Words before its own arguments: the rest of a person's own agent's command (`node ~/tools/agent.js`).
    pub lead: &'static [&'static str],
    pub kind: Kind,
}

/// Where an agent comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// One Marv OS knows ([`AGENTS`]).
    Known,
    /// One the person added in Settings ([`OwnAgent`]): just its own terminal, none of the known agents' extras.
    Own,
    /// The person's shell ([`SHELL`]): a plain terminal, for anything else.
    Shell,
}

/// Marv's own id.
pub const MARV: &str = "marv";

/// Every agent Marv OS knows, in the order the new-chat picker lists them.
pub const AGENTS: &[Agent] = &[
    Agent {
        id: MARV,
        title: "Marv",
        program: "marv",
        resume: Resume::Latest(&["--continue"]),
        config: &[],
        env: &["MARV_", "OPENROUTER_"],
        install: "https://github.com/Flologix/marv",
        winget: None,
        lead: &[],
        kind: Kind::Known,
    },
    Agent {
        id: crate::claude_code::NAME,
        title: "Claude Code",
        program: "claude",
        resume: Resume::ById { new: "--session-id", again: "--resume" },
        config: &[".claude", ".claude.json"],
        env: crate::harness_sandbox::CLAUDE_ENV,
        install: "https://claude.com/claude-code",
        winget: Some("Anthropic.ClaudeCode"),
        lead: &[],
        kind: Kind::Known,
    },
    Agent {
        id: "codex",
        title: "Codex",
        program: "codex",
        resume: Resume::Latest(&["resume", "--last"]),
        config: &[".codex"],
        env: &["CODEX_", "OPENAI_"],
        install: "https://github.com/openai/codex",
        winget: Some("OpenAI.Codex"),
        lead: &[],
        kind: Kind::Known,
    },
    Agent {
        id: "opencode",
        title: "OpenCode",
        program: "opencode",
        resume: Resume::Latest(&["--continue"]),
        config: &[".config/opencode", ".local/share/opencode", ".local/state/opencode", ".cache/opencode"],
        env: &["OPENCODE_", "ANTHROPIC_", "OPENAI_", "OPENROUTER_"],
        install: "https://opencode.ai",
        winget: Some("Anomaly.OpenCode"),
        lead: &[],
        kind: Kind::Known,
    },
    Agent {
        id: "gemini",
        title: "Gemini CLI",
        program: "gemini",
        resume: Resume::ById { new: "--session-id", again: "--resume" },
        config: &[".gemini"],
        env: &["GEMINI_", "GOOGLE_"],
        install: "https://github.com/google-gemini/gemini-cli",
        winget: None,
        lead: &[],
        kind: Kind::Known,
    },
    Agent {
        id: "pi",
        title: "Pi",
        program: "pi",
        resume: Resume::Latest(&["--continue"]),
        config: &[".pi"],
        env: &["PI_", "ANTHROPIC_", "OPENAI_", "OPENROUTER_"],
        install: "https://github.com/badlogic/pi-mono",
        winget: None,
        lead: &[],
        kind: Kind::Known,
    },
    Agent {
        id: "cursor-agent",
        title: "Cursor Agent",
        program: "cursor-agent",
        resume: Resume::Latest(&["resume"]),
        config: &[".cursor", ".config/cursor"],
        env: &["CURSOR_"],
        install: "https://cursor.com/cli",
        winget: None,
        lead: &[],
        kind: Kind::Known,
    },
    Agent {
        id: "crush",
        title: "Crush",
        program: "crush",
        resume: Resume::Latest(&["--continue"]),
        config: &[".config/crush", ".local/share/crush"],
        env: &["CRUSH_", "ANTHROPIC_", "OPENAI_", "OPENROUTER_"],
        install: "https://github.com/charmbracelet/crush",
        winget: None,
        lead: &[],
        kind: Kind::Known,
    },
    Agent {
        id: "antigravity",
        title: "Antigravity",
        program: "agy",
        resume: Resume::Fresh,
        config: &[".gemini", ".antigravity", ".config/antigravity"],
        env: &["GEMINI_", "GOOGLE_"],
        install: "https://antigravity.google",
        winget: None,
        lead: &[],
        kind: Kind::Known,
    },
    // From herdr's list of terminal agents. Those whose way to continue isn't known for sure start afresh each time
    // (a wrong flag would stop them starting at all).
    Agent {
        id: "hermes",
        title: "Hermes",
        program: "hermes",
        resume: Resume::Fresh,
        config: &[".hermes"],
        env: &["HERMES_", "NOUS_", "OPENROUTER_", "OPENAI_", "ANTHROPIC_"],
        install: "https://github.com/NousResearch/hermes-agent",
        winget: None,
        lead: &[],
        kind: Kind::Known,
    },
    Agent {
        id: "amp",
        title: "Amp",
        program: "amp",
        resume: Resume::Fresh,
        config: &[".config/amp", ".local/share/amp"],
        env: &["AMP_"],
        install: "https://ampcode.com",
        winget: None,
        lead: &[],
        kind: Kind::Known,
    },
    Agent {
        id: "droid",
        title: "Droid",
        program: "droid",
        resume: Resume::Fresh,
        config: &[".factory"],
        env: &["FACTORY_"],
        install: "https://factory.ai",
        winget: None,
        lead: &[],
        kind: Kind::Known,
    },
    Agent {
        id: "copilot",
        title: "Copilot CLI",
        program: "copilot",
        resume: Resume::Latest(&["--continue"]),
        config: &[".copilot"],
        env: &["COPILOT_", "GH_", "GITHUB_"],
        install: "https://github.com/github/copilot-cli",
        winget: Some("GitHub.Copilot"),
        lead: &[],
        kind: Kind::Known,
    },
    Agent {
        id: "kimi",
        title: "Kimi CLI",
        program: "kimi",
        resume: Resume::Latest(&["--continue"]),
        config: &[".kimi"],
        env: &["KIMI_", "MOONSHOT_"],
        install: "https://github.com/MoonshotAI/kimi-cli",
        winget: None,
        lead: &[],
        kind: Kind::Known,
    },
    Agent {
        id: "qwen",
        title: "Qwen Code",
        program: "qwen",
        resume: Resume::Fresh,
        config: &[".qwen"],
        env: &["QWEN_", "DASHSCOPE_", "OPENAI_"],
        install: "https://github.com/QwenLM/qwen-code",
        winget: None,
        lead: &[],
        kind: Kind::Known,
    },
    Agent {
        id: "cline",
        title: "Cline",
        program: "cline",
        resume: Resume::Fresh,
        config: &[".cline"],
        env: &["CLINE_", "ANTHROPIC_", "OPENAI_", "OPENROUTER_"],
        install: "https://cline.bot/cli",
        winget: None,
        lead: &[],
        kind: Kind::Known,
    },
    Agent {
        id: "kiro",
        title: "Kiro",
        program: "kiro-cli",
        resume: Resume::Fresh,
        config: &[".kiro"],
        env: &["KIRO_"],
        install: "https://kiro.dev/cli",
        winget: None,
        lead: &[],
        kind: Kind::Known,
    },
    Agent {
        id: "letta",
        title: "Letta Code",
        program: "letta",
        resume: Resume::Fresh,
        config: &[".letta"],
        env: &["LETTA_"],
        install: "https://github.com/letta-ai/letta-code",
        winget: None,
        lead: &[],
        kind: Kind::Known,
    },
    Agent {
        id: "qoder",
        title: "Qoder",
        program: "qodercli",
        resume: Resume::Fresh,
        config: &[".qoder"],
        env: &["QODER_"],
        install: "https://qoder.com/cli",
        winget: None,
        lead: &[],
        kind: Kind::Known,
    },
    Agent {
        id: "grok",
        title: "Grok CLI",
        program: "grok",
        resume: Resume::Fresh,
        config: &[".grok"],
        env: &["GROK_", "XAI_"],
        install: "https://github.com/superagent-ai/grok-cli",
        winget: None,
        lead: &[],
        kind: Kind::Known,
    },
];

/// The agent a chat record names (none, or one Marv OS doesn't know: Marv).
pub fn for_chat(harness: Option<&str>) -> &'static Agent {
    harness.and_then(find).unwrap_or(&AGENTS[0])
}

pub fn find(id: &str) -> Option<&'static Agent> {
    all().into_iter().find(|a| a.id == id)
}

/// Every agent a chat can run, in the new-chat picker's order: the known ones, the shell, then the person's own.
pub fn all() -> Vec<&'static Agent> {
    let own = OWN.read().unwrap_or_else(|e| e.into_inner());
    AGENTS.iter().chain(std::iter::once(shell())).chain(own.iter().copied()).collect()
}

/// The shell's id in a chat record.
pub const SHELL: &str = "terminal";

/// The person's shell as an agent: `$SHELL` (else `sh`) on Unix; PowerShell 7 (`pwsh`), else Windows PowerShell.
fn shell() -> &'static Agent {
    static SHELL_AGENT: std::sync::OnceLock<Agent> = std::sync::OnceLock::new();
    SHELL_AGENT.get_or_init(|| {
        let program: &'static str = if cfg!(windows) {
            if locate("pwsh").is_some() { "pwsh" } else { "powershell" }
        } else {
            let shell = std::env::var("SHELL").ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "sh".into());
            Box::leak(shell.into_boxed_str())
        };
        Agent {
            id: SHELL,
            title: "Terminal",
            program,
            resume: Resume::Fresh,
            config: &[],
            env: &[],
            install: "",
            winget: None,
            lead: &[],
            kind: Kind::Shell,
        }
    })
}

/// The person's own agents (Settings > Agents), as set last ([`set_own`]).
static OWN: std::sync::RwLock<Vec<&'static Agent>> = std::sync::RwLock::new(Vec::new());

/// An agent the person added: what it's called and the command that starts it, as typed. Kept in Marv OS's config
/// (`agents.json`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OwnAgent {
    /// Its id in a chat record: `own-` and its name's letters and digits.
    pub id: String,
    pub name: String,
    /// The program, then its arguments (`hermes`, `node ~/tools/agent.js --fast`); a word in double quotes may hold
    /// spaces.
    pub command: String,
    /// The arguments that continue its latest conversation in the folder (`--continue`); empty: it starts afresh.
    #[serde(default)]
    pub resume: String,
}

impl OwnAgent {
    /// A new one from what the person typed, checked: a name, and a command whose program is found here.
    pub fn new(name: &str, command: &str, resume: &str, taken: &[&str]) -> Result<Self, String> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 40 || name.chars().any(char::is_control) {
            return Err("Give it a name, up to 40 characters.".into());
        }
        // An odd number of `"` leaves one open, and the rest of the line would silently become part of one word.
        if [command, resume].iter().any(|text| text.matches('"').count() % 2 == 1) {
            return Err("The command has a \" that isn't closed: put a quote at both ends of a part with spaces.".into());
        }
        let words = split_words(command);
        let Some(program) = words.first() else { return Err("Type the command that starts it, e.g. hermes.".into()) };
        if find_own_program(program).is_none() {
            return Err(format!("There's no `{}` here: type its full path, or install it first.", marv_core::printable(program)));
        }
        let slug: String = name.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        let slug = slug.split('-').filter(|w| !w.is_empty()).collect::<Vec<_>>().join("-");
        let base = format!("own-{}", if slug.is_empty() { "agent" } else { &slug });
        let id = (1..)
            .map(|n| if n == 1 { base.clone() } else { format!("{base}-{n}") })
            .find(|id| !taken.contains(&id.as_str()))
            .unwrap_or(base);
        Ok(Self { id, name: name.to_string(), command: command.trim().to_string(), resume: resume.trim().to_string() })
    }

    fn agent(&self) -> Option<Agent> {
        let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
        let mut words = split_words(&self.command).into_iter();
        let program = leak(words.next()?);
        // `~/` starts every word, not just the program: `node ~/tools/agent.js --fast` runs that script.
        let home = marv_session::paths::home_dir();
        let lead: Vec<&'static str> = words.map(|w| leak(expand_tilde(&w, &home))).collect();
        let resume: Vec<&'static str> = split_words(&self.resume).into_iter().map(|w| leak(expand_tilde(&w, &home))).collect();
        Some(Agent {
            id: leak(self.id.clone()),
            title: leak(self.name.clone()),
            program,
            resume: if resume.is_empty() { Resume::Fresh } else { Resume::Latest(Box::leak(resume.into_boxed_slice())) },
            config: &[],
            env: &[],
            install: "",
            winget: None,
            lead: Box::leak(lead.into_boxed_slice()),
            kind: Kind::Own,
        })
    }
}

/// Sets the person's own agents (read at start, and after each change in Settings). Each set leaks a few strings: it
/// happens when the person adds or removes one.
pub fn set_own(list: &[OwnAgent]) {
    let agents: Vec<&'static Agent> =
        list.iter().filter(|o| o.id.starts_with("own-")).filter_map(OwnAgent::agent).map(|a| &*Box::leak(Box::new(a))).collect();
    *OWN.write().unwrap_or_else(|e| e.into_inner()) = agents;
}

/// A command's words: split on whitespace, a double-quoted part kept whole (`"C:\My Tools\agent.exe" --x`). No
/// escapes, so a Windows path's backslashes stay as typed.
pub fn split_words(command: &str) -> Vec<String> {
    let (mut words, mut word, mut quoted, mut any) = (Vec::new(), String::new(), false, false);
    for c in command.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    words.push(std::mem::take(&mut word));
                }
                any = false;
            }
            c => {
                word.push(c);
                any = true;
            }
        }
    }
    if any {
        words.push(word);
    }
    words
}

/// A word that starts with `~/` (or `~\`) with the home folder in its place; any other word as typed (`~src` is not
/// `~user`, and `a/~/b` is a path like any other).
fn expand_tilde(word: &str, home: &std::path::Path) -> String {
    match word.strip_prefix("~/").or_else(|| word.strip_prefix("~\\")) {
        Some(rest) => home.join(rest).display().to_string(),
        None => word.to_string(),
    }
}

/// A person's own agent's program: a path (`~/` is the home), else found as [`locate`] finds one.
fn find_own_program(program: &str) -> Option<PathBuf> {
    let path = PathBuf::from(expand_tilde(program, &marv_session::paths::home_dir()));
    if path.components().count() > 1 || path.is_absolute() {
        return path.is_file().then_some(path);
    }
    locate(program)
}

/// Where `program` is: on the `PATH`, else where installers put it in the home (`~/.local/bin`, mise's shims).
pub fn locate(program: &str) -> Option<PathBuf> {
    marv_session::paths::find_program(program, &[".local/bin", ".local/share/mise/shims", ".bun/bin", ".npm-global/bin"])
}

/// Windows: `program` in the folder winget installed package `id` into (`%LOCALAPPDATA%\Microsoft\WinGet\Packages\
/// <id>_<source>`): `<program>.exe`, or `<program>-x86_64-pc-windows-msvc.exe` as the OpenAI CLI ships it. Winget
/// links such a program as `<program>.exe` in its Links folder only where it may make symlinks (Developer Mode); else
/// it puts the package's folder on the PATH, where the CLI's long name isn't its command, and a Marv OS started before
/// the install doesn't see that PATH anyway: Install said "installed" and the picker still offered Install (seen
/// 2026-11-14). None elsewhere, or when it isn't there.
fn winget_program(id: &str, program: &str) -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let packages = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Microsoft").join("WinGet").join("Packages");
    let prefix = format!("{id}_");
    std::fs::read_dir(packages).ok()?.flatten().filter(|e| e.file_name().to_string_lossy().starts_with(&prefix)).find_map(|e| {
        [format!("{program}.exe"), format!("{program}-x86_64-pc-windows-msvc.exe")]
            .into_iter()
            .map(|name| e.path().join(name))
            .find(|p| p.is_file())
    })
}

impl Agent {
    /// Its program here, if it's installed: the real one, never a mise shim or launcher script (`~/.local/bin/claude`
    /// running `mise use -g claude` and `mise x`). Those ran mise at every start, which looks for a newer release and
    /// may install it first: seconds of a blank terminal, or a failed start ("mise ERROR Failed to install
    /// npm:@opencode/cli@latest" in a run). Found once per program for the app's life.
    pub fn installed(&self) -> Option<PathBuf> {
        static FOUND: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<&'static str, PathBuf>>> =
            std::sync::OnceLock::new();
        let found = FOUND.get_or_init(Default::default);
        if let Some(path) = found.lock().unwrap_or_else(|e| e.into_inner()).get(self.program).filter(|p| p.is_file()) {
            return Some(path.clone());
        }
        if self.kind == Kind::Own {
            return find_own_program(self.program);
        }
        let path = locate(self.program).or_else(|| self.winget.and_then(|id| winget_program(id, self.program)))?;
        let real = crate::harness_sandbox::real_program(&path, &marv_session::paths::home_dir(), self.program);
        found.lock().unwrap_or_else(|e| e.into_inner()).insert(self.program, real.clone());
        Some(real)
    }

    /// Said when it isn't installed.
    pub fn not_installed(&self) -> String {
        if self.install.is_empty() {
            return format!("{} can't start: there's no `{}` here any more.", self.title, self.program);
        }
        format!(
            "{} isn't installed here (no `{}` on your PATH). Install it from {}, then start the chat again.",
            self.title, self.program, self.install
        )
    }

    /// The arguments that start (`again`: continue) the chat's conversation; `session` is the chat's own id for an
    /// agent that takes one.
    pub fn session_args(&self, session: &str, again: bool) -> Vec<String> {
        match self.resume {
            Resume::ById { new, again: flag } => vec![if again { flag } else { new }.to_string(), session.to_string()],
            Resume::Latest(args) if again => args.iter().map(|a| a.to_string()).collect(),
            Resume::Latest(_) | Resume::Fresh => Vec::new(),
        }
    }

    /// Whether it keeps a session under an id Marv gives (the chat records it).
    pub fn takes_session_id(&self) -> bool {
        matches!(self.resume, Resume::ById { .. })
    }
}

/// How much a chat's agent may do without asking: the same four modes for every agent, as Marv OS's harness layer is
/// the same around each (`HARNESS-CONSISTENCY.md`), set on each agent its own way ([`permission_settings`]).
///
/// - **Own** (the default, and records from before): the agent's own settings, as in the person's terminal.
/// - **Supervised:** it asks before each edit and command.
/// - **Auto-accept edits:** edits below the folder go ahead; commands ask.
/// - **Auto:** the agent's own automatic review decides what's routine (Claude Code's auto mode); an agent without one
///   runs as auto-accept edits.
/// - **Full access:** nothing asks, so it only ever runs inside Marv's outer sandbox (the folder and the agent's own
///   settings, the home's secrets hidden), as Marv's yolo does (H4); where the sandbox can't run, the chat won't start.
///
/// A home folder holds every mode above Supervised off, as it holds Marv's yolo off ([`Permissions::held`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Permissions {
    #[default]
    Own,
    Supervised,
    AutoEdits,
    Auto,
    Full,
}

impl Permissions {
    /// A chat record's word for it (`auto-edits`); anything else is its agent's own settings.
    pub fn parse(word: Option<&str>) -> Self {
        word.and_then(|w| serde_json::from_value(serde_json::Value::from(w)).ok()).unwrap_or_default()
    }

    /// The mode a chat in `spec`'s folder runs with: above Supervised only where Marv allows yolo (never in the home
    /// folder or above it, nor in a project that turned it off).
    pub fn held(self, spec: &ChatSpec) -> Self {
        if self > Permissions::Supervised && crate::host::yolo_held_off(spec) { Permissions::Supervised } else { self }
    }

    /// It runs only inside Marv's outer sandbox.
    pub fn needs_sandbox(self) -> bool {
        self == Permissions::Full
    }

    /// Its name inside a sentence, as the page names it (`PERMISSION_MODES` in app/src/lib/harness.ts).
    pub fn title(self) -> &'static str {
        match self {
            Permissions::Own => "its own settings",
            Permissions::Supervised => "Supervised",
            Permissions::AutoEdits => "Auto-accept edits",
            Permissions::Auto => "Auto",
            Permissions::Full => "Full access",
        }
    }
}

/// A permission mode set on one agent: its arguments, its environment (Marv's `MARV_YOLO`), and OpenCode's
/// `permission` config.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PermissionSettings {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub opencode: Option<serde_json::Value>,
}

/// `mode` as `agent` takes it; `None`: its own settings (the mode is Own, or Marv OS doesn't know how to set this
/// agent's yet: Pi, Cursor Agent, Crush, Antigravity).
pub fn permission_settings(agent: &Agent, mode: Permissions) -> Option<PermissionSettings> {
    use Permissions::*;
    let args = |a: &[&str]| {
        Some(PermissionSettings { args: a.iter().map(|s| s.to_string()).collect(), ..PermissionSettings::default() })
    };
    let opencode = |edit: &str, bash: &str| {
        let permission = serde_json::json!({"edit": edit, "bash": bash, "webfetch": "ask"});
        Some(PermissionSettings { opencode: Some(permission), ..PermissionSettings::default() })
    };
    // Marv's yolo is on unless its config turns it off, so a chat that asks holds it off for that Marv
    // (`MARV_YOLO=off`, which also holds `approveSandboxed` off). Without it a Supervised Marv chat started with no
    // arguments, ran in yolo and wrote a file without asking (the Windows VM, 2026-11-22).
    let asks = |a: &[&str]| {
        let mut settings = args(a)?;
        settings.env.push(("MARV_YOLO".into(), "off".into()));
        Some(settings)
    };
    match (agent.id, mode) {
        (_, Own) => None,
        ("claude-code", Supervised) => args(&["--permission-mode", "manual"]),
        ("claude-code", AutoEdits) => args(&["--permission-mode", "acceptEdits"]),
        ("claude-code", Auto) => args(&["--permission-mode", "auto"]),
        ("claude-code", Full) => args(&["--permission-mode", "bypassPermissions"]),
        // Codex asks for writes in its read-only sandbox, and for what leaves the folder in its workspace one.
        ("codex", Supervised) => args(&["--sandbox", "read-only", "--ask-for-approval", "on-request"]),
        ("codex", AutoEdits | Auto) => args(&["--sandbox", "workspace-write", "--ask-for-approval", "on-request"]),
        // Its own sandbox off inside Marv's (it can't nest bubblewrap).
        ("codex", Full) => args(&["--dangerously-bypass-approvals-and-sandbox"]),
        ("gemini", Supervised) => args(&["--approval-mode", "default"]),
        ("gemini", AutoEdits | Auto) => args(&["--approval-mode", "auto_edit"]),
        ("gemini", Full) => args(&["--approval-mode", "yolo"]),
        ("opencode", Supervised) => opencode("ask", "ask"),
        ("opencode", AutoEdits | Auto) => opencode("allow", "ask"),
        ("opencode", Full) => opencode("allow", "allow"),
        // Marv: Supervised asks for edits and commands (yolo off); auto-accept edits approves its edits, commands ask;
        // Auto adds its yolo (sandboxed commands run unasked, as its config has it); Full approves its edits and
        // commits for the session too.
        (MARV, Supervised) => asks(&[]),
        (MARV, AutoEdits) => asks(&["--approve", "files"]),
        (MARV, Auto) => args(&["--approve", "files"]),
        (MARV, Full) => args(&["--approve", "files", "--approve", "git_write"]),
        _ => None,
    }
}

/// OpenCode's inline config (`OPENCODE_CONFIG_CONTENT`) with `permission` set, beside its mesh server and
/// instructions.
fn opencode_permission(env: &mut Vec<(String, String)>, permission: &serde_json::Value) {
    let at = env.iter().position(|(k, _)| k == "OPENCODE_CONFIG_CONTENT");
    let mut config: serde_json::Value =
        at.and_then(|i| serde_json::from_str(&env[i].1).ok()).unwrap_or_else(|| serde_json::json!({}));
    config["permission"] = permission.clone();
    match at {
        Some(i) => env[i].1 = config.to_string(),
        None => env.push(("OPENCODE_CONFIG_CONTENT".into(), config.to_string())),
    }
}

/// A question an agent's screen shows that only the person can answer, from its latest output's text (Claude Code's
/// folder trust and first-run screens, a login): what to tell them. Marv OS types nothing into a terminal then (a
/// teammate's mail or a timer would answer it: Enter on Claude Code's trust question is "No, exit").
pub fn waiting_question(screen: &str) -> Option<&'static str> {
    let squashed: String = screen.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase();
    let last = squashed.len().saturating_sub(1500);
    let tail = squashed.get(last..).unwrap_or(&squashed);
    if tail.contains("yes,itrustthisfolder") || tail.contains("doyoutrustthefilesinthisfolder") {
        Some("It's asking you to trust this folder: open the chat and answer it in its terminal.")
    } else if tail.contains("pressentertocontinue") || tail.contains("choosethetextstyle") || tail.contains("selectloginmethod") {
        Some("It's showing its first-run screens: open the chat and go through them in its terminal.")
    } else if tail.contains("pressanykeytologin") || tail.contains("pleaserun/login") || tail.contains("notloggedin") {
        Some("It needs you to sign in: open the chat and sign in in its terminal.")
    } else {
        None
    }
}

/// How an agent takes the chat's mesh tools (the chat's door, served as an MCP server over the `marv` helper):
/// none, or its own way of being given an MCP server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshVia {
    /// Codex's `-c mcp_servers.<name>.…` settings.
    CodexConfig,
    /// OpenCode's `OPENCODE_CONFIG_CONTENT` (added to the person's own config).
    OpenCodeConfig,
    /// A copy of the person's Crush config with the server added, as Crush's global config (`CRUSH_GLOBAL_CONFIG`);
    /// their own file is left as it is.
    CrushGlobal,
}

impl Agent {
    /// How it gets the chat's mesh tools, if it can be given them without writing into the person's files.
    pub fn mesh_via(&self) -> Option<MeshVia> {
        match self.id {
            "codex" => Some(MeshVia::CodexConfig),
            "opencode" => Some(MeshVia::OpenCodeConfig),
            "crush" => Some(MeshVia::CrushGlobal),
            // Cursor Agent reads MCP servers only from `.cursor/mcp.json` files (the project's, the home's); Gemini CLI
            // from its settings files, its system ones only in folders root owns (seen: "Skipping system settings
            // file … not owned by root"); Pi has none by design. Not given the mesh rather than written into the
            // person's files.
            _ => None,
        }
    }
}

/// Where an agent's mesh settings files are inside the outer sandbox (`files`, bound there).
pub const GUEST_AGENT_FILES: &str = "/run/marv-harness/agent";

/// The arguments and variables that give an agent the chat's mesh tools: `exe` the `marv` helper and `socket` the
/// chat's MCP socket, as the agent sees them (inside the sandbox, its own paths). An agent given them by a file has it
/// written in `files` (the host's folder), and named at `shown` (where the agent sees that folder).
pub fn mesh_settings(
    via: MeshVia,
    exe: &Path,
    socket: &Path,
    files: &Path,
    shown: &Path,
) -> (Vec<String>, Vec<(String, String)>) {
    let command = vec![
        exe.display().to_string(),
        marv_session::helper_modes::MESH_MCP_SUBCOMMAND.to_string(),
        socket.display().to_string(),
    ];
    let name = crate::harness_mesh::SERVER;
    match via {
        // TOML values: a JSON string or array of strings is one.
        MeshVia::CodexConfig => (
            vec![
                "-c".into(),
                format!("mcp_servers.{name}.command={}", serde_json::Value::from(command[0].clone())),
                "-c".into(),
                format!("mcp_servers.{name}.args={}", serde_json::Value::from(command[1..].to_vec())),
            ],
            Vec::new(),
        ),
        MeshVia::OpenCodeConfig => {
            let config = serde_json::json!({"mcp": {name: {"type": "local", "command": command, "enabled": true}}});
            (Vec::new(), vec![("OPENCODE_CONFIG_CONTENT".into(), config.to_string())])
        }
        MeshVia::CrushGlobal => {
            // The person's own global config (`~/.config/crush/crush.json`), with the server added to its `mcp`.
            let own = marv_session::paths::home_dir().join(".config/crush/crush.json");
            let mut config: serde_json::Value = std::fs::read_to_string(&own)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_else(|| serde_json::json!({}));
            if !config.is_object() {
                config = serde_json::json!({});
            }
            if !config["mcp"].is_object() {
                config["mcp"] = serde_json::json!({});
            }
            config["mcp"][name] = serde_json::json!({"type": "stdio", "command": command[0], "args": command[1..].to_vec()});
            let dir = files.join("crush");
            if std::fs::create_dir_all(&dir).is_err() || write_private(&dir.join("crush.json"), &config).is_err() {
                return (Vec::new(), Vec::new());
            }
            (Vec::new(), vec![("CRUSH_GLOBAL_CONFIG".into(), shown.join("crush").display().to_string())])
        }
    }
}

/// The person's apps (`claude_code::person_servers`: Settings › Apps, as Claude Code's `mcpServers` entries) given to an
/// agent its own way, beside its mesh server: before, only Claude Code got them. No secret goes on its command line,
/// which other programs can read: Codex takes a stdio server's variables and an app's headers through its own
/// environment (`env_vars`, `env_http_headers`); OpenCode's inline config is an environment variable; Crush's is a
/// private file. An OAuth app is passed by its address only, and the agent signs in to it on its own. `args` and `env`
/// are added to; Crush's file is the one [`mesh_settings`] wrote in `files`, if it did.
pub fn app_settings(
    via: MeshVia,
    apps: &serde_json::Map<String, serde_json::Value>,
    files: &Path,
    shown: &Path,
    args: &mut Vec<String>,
    env: &mut Vec<(String, String)>,
) {
    use serde_json::{Value, json};
    if apps.is_empty() {
        return;
    }
    let text = |v: &Value| v.as_str().unwrap_or_default().to_string();
    let pairs = |v: &Value| -> Vec<(String, String)> {
        v.as_object().map(|m| m.iter().map(|(k, v)| (k.clone(), text(v))).collect()).unwrap_or_default()
    };
    match via {
        MeshVia::CodexConfig => {
            for (n, (name, app)) in apps.iter().enumerate() {
                // A bare key: Codex's `-c` keeps quotes as part of the name, so other characters become `-`.
                let bare: String =
                    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' }).collect();
                let key = format!("mcp_servers.{bare}");
                let mut set = |field: &str, value: Value| args.extend(["-c".into(), format!("{key}.{field}={value}")]);
                if app["type"] == "http" {
                    set("url", app["url"].clone());
                    let headers = pairs(&app["headers"]);
                    if !headers.is_empty() {
                        let mut table = Vec::new();
                        for (h, (header, value)) in headers.into_iter().enumerate() {
                            let var = format!("MARV_APP_{n}_HEADER_{h}");
                            table.push(format!("{}={}", Value::from(header), Value::from(var.as_str())));
                            env.push((var, value));
                        }
                        // A TOML inline table: header = the variable holding its value.
                        args.extend(["-c".into(), format!("{key}.env_http_headers={{{}}}", table.join(","))]);
                    }
                } else {
                    set("command", app["command"].clone());
                    set("args", if app["args"].is_array() { app["args"].clone() } else { json!([]) });
                    let vars = pairs(&app["env"]);
                    if !vars.is_empty() {
                        set("env_vars", Value::from(vars.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>()));
                        env.extend(vars);
                    }
                }
            }
        }
        MeshVia::OpenCodeConfig => {
            let at = env.iter().position(|(k, _)| k == "OPENCODE_CONFIG_CONTENT");
            let mut config: Value =
                at.and_then(|i| serde_json::from_str(&env[i].1).ok()).filter(Value::is_object).unwrap_or_else(|| json!({}));
            if !config["mcp"].is_object() {
                config["mcp"] = json!({});
            }
            for (name, app) in apps {
                config["mcp"][name] = if app["type"] == "http" {
                    let mut entry = json!({"type": "remote", "url": app["url"], "enabled": true});
                    if app["headers"].is_object() {
                        entry["headers"] = app["headers"].clone();
                    }
                    entry
                } else {
                    let mut command = vec![app["command"].clone()];
                    command.extend(app["args"].as_array().cloned().unwrap_or_default());
                    let mut entry = json!({"type": "local", "command": command, "enabled": true});
                    if app["env"].as_object().is_some_and(|e| !e.is_empty()) {
                        entry["environment"] = app["env"].clone();
                    }
                    entry
                };
            }
            match at {
                Some(i) => env[i].1 = config.to_string(),
                None => env.push(("OPENCODE_CONFIG_CONTENT".into(), config.to_string())),
            }
        }
        MeshVia::CrushGlobal => {
            let dir = files.join("crush");
            let written = dir.join("crush.json");
            let own = marv_session::paths::home_dir().join(".config/crush/crush.json");
            let read = |p: &Path| std::fs::read_to_string(p).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok());
            let mut config = read(&written).or_else(|| read(&own)).filter(Value::is_object).unwrap_or_else(|| json!({}));
            if !config["mcp"].is_object() {
                config["mcp"] = json!({});
            }
            for (name, app) in apps {
                config["mcp"][name] = if app["type"] == "http" {
                    json!({"type": "http", "url": app["url"], "headers": app["headers"].as_object().cloned().unwrap_or_default()})
                } else {
                    json!({"type": "stdio", "command": app["command"], "args": app["args"].as_array().cloned().unwrap_or_default(),
                        "env": app["env"].as_object().cloned().unwrap_or_default()})
                };
            }
            if std::fs::create_dir_all(&dir).is_err() || write_private(&written, &config).is_err() {
                return;
            }
            let shown = shown.join("crush").display().to_string();
            match env.iter().position(|(k, _)| k == "CRUSH_GLOBAL_CONFIG") {
                Some(i) => env[i].1 = shown,
                None => env.push(("CRUSH_GLOBAL_CONFIG".into(), shown)),
            }
        }
    }
}

/// `value` as JSON at `path`, readable by this user only (a Crush config can hold the person's provider keys).
fn write_private(path: &Path, value: &serde_json::Value) -> std::io::Result<()> {
    use marv_fs::mode::CreateMode;
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).create_mode(0o600).open(path)?;
    file.write_all(value.to_string().as_bytes())
}

/// Where a chat's terminal Marv writes its session's id (beside the chat's events file), for the chat to keep.
pub fn marv_session_record(events: &Path) -> PathBuf {
    events.with_extension("marv-session")
}

/// Marv's program for a chat: the `marv` this Marv OS ships (beside the desktop, the same version, which takes the
/// chat's mesh name), else the person's own.
fn marv_program() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    let bundled = exe.as_deref().and_then(marv_session::helper_modes::bundled_helper);
    // A build of this repository (cargo): the `marv` built beside it, the same version (an older one on the PATH may
    // not take `--role`: "unexpected argument '--role' found").
    let beside =
        exe.and_then(|e| e.parent().map(|d| d.join(format!("marv{}", std::env::consts::EXE_SUFFIX)))).filter(|m| m.is_file());
    bundled.or(beside).or_else(|| locate("marv"))
}

/// What every agent in a Marv OS chat is told at start (Claude Code's appended system prompt, Codex's developer
/// instructions, OpenCode's instructions file): where it runs and what the person sees around it, so it can answer
/// their questions about the app (asked about "the Activity tab", a Claude chat guessed). Fixed text: the same at
/// every start (the prompt cache), and nothing it says grants anything.
pub const MARV_OS_BRIEF: &str = "You are running inside Marv OS, a desktop for coding agents. The person sees you as one chat: this terminal is the chat, and you are its agent. Around it:
- Folders (left): each folder is a project; its chats are listed under it, each run by an agent (Marv, Claude Code, Codex, OpenCode and others), each in its own terminal like this one.
- The right pane: the folder's files, and Git (lazygit for the folder; it offers to start a repository where there isn't one).
- Home: a greeting, what needs the person, and Home chats that work in their home folder (always sandboxed).
- Teams: chats started together in a folder (a lead and workers) that message each other on Marv's mesh, without the person approving each message. A lead hands out tasks; each worker's turn on a task ends with its report back.
- Activity: every chat's agent with its state, its process's CPU and memory, and what it cost today; the person can open, pause or stop a chat there.
- Notifications: the person is told when a chat they aren't looking at needs them or finishes.
- Settings: plan and account, model connections, spending limits, appearance.
If the person asks about Marv OS, answer from this; say so when you don't know.";

/// OpenCode's inline config (`OPENCODE_CONFIG_CONTENT`, its mesh server's when it has one) with `file` among its
/// instructions.
fn opencode_instructions(env: &mut Vec<(String, String)>, file: &Path) {
    let at = env.iter().position(|(k, _)| k == "OPENCODE_CONFIG_CONTENT");
    let mut config: serde_json::Value =
        at.and_then(|i| serde_json::from_str(&env[i].1).ok()).unwrap_or_else(|| serde_json::json!({}));
    config["instructions"] = serde_json::json!([file.display().to_string()]);
    match at {
        Some(i) => env[i].1 = config.to_string(),
        None => env.push(("OPENCODE_CONFIG_CONTENT".into(), config.to_string())),
    }
}

/// Codex's `notify` setting: its JSON (`agent-turn-complete`, with `last-assistant-message`) appended to `events` as
/// one line. Codex runs the program itself with the JSON as its last argument: `sh` on Unix; on Windows, which has no
/// `sh`, Marv's own binary (`__append-line`).
fn codex_notify(events: &str) -> String {
    let argv = if cfg!(windows) {
        let exe = marv_session::helper_modes::helper_exe().map(|p| p.display().to_string()).unwrap_or_else(|| "marv".into());
        vec![exe, marv_session::helper_modes::APPEND_LINE_SUBCOMMAND.to_string(), events.to_string()]
    } else {
        let script = format!("printf '%s\\n' \"$1\" >> {}", shell_quote(events));
        vec!["sh".to_string(), "-c".into(), script, "marv".into()]
    };
    format!("notify={}", serde_json::Value::from(argv))
}

pub(crate) fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The chat's agent in its terminal: `session` its conversation (`again`: continue it), run as the person's own
/// terminal would (`spec.harness_sandbox` off), or inside the outer sandbox (on, and always in a Home chat).
///
/// - **Claude Code** goes through [`crate::claude_code::terminal_command`] (its mesh tools, brief and Activity hooks
///   into `events`).
/// - **Marv** runs as it is (its own sandbox) and *is* the chat on the mesh: it takes the chat's mesh name
///   (`MARV_MESH_NAME`, the chat's door lent to it while it runs), so it approves its own wires and wakes on its mail.
/// - **Codex and OpenCode** get the chat's mesh tools on its door ([`mesh_settings`]); the others have none yet.
///
/// `mode` is the chat's permission mode ([`Permissions`]), already held to what its folder allows; Full access only
/// inside the outer sandbox (refused here otherwise: the caller turns the sandbox on for it).
pub fn command(
    agent: &Agent,
    spec: &ChatSpec,
    session: &str,
    again: bool,
    events: &Path,
    mode: Permissions,
) -> Result<TerminalCommand, String> {
    if mode.needs_sandbox() && agent.id != MARV && !spec.harness_sandbox {
        return Err(format!("Full access runs {} only inside Marv's sandbox, which isn't on for this chat.", agent.title));
    }
    let permissions = permission_settings(agent, mode).unwrap_or_default();
    if agent.id == crate::claude_code::NAME {
        return crate::claude_code::terminal_command_with(spec, session, again, events, &permissions.args);
    }
    let bin = if agent.id == MARV { marv_program() } else { agent.installed() }.ok_or_else(|| agent.not_installed())?;
    let mut args: Vec<String> = agent.lead.iter().map(|a| a.to_string()).collect();
    args.extend(agent.session_args(session, again));
    args.extend(permissions.args.iter().cloned());
    let term = [("TERM", "xterm-256color"), ("COLORTERM", "truecolor")].map(|(k, v)| (k.to_string(), v.to_string())).to_vec();
    if agent.id == MARV {
        // A team member starts as its role (`marv --role`: its tools, the files it may write, whose tasks it takes,
        // held by Marv itself). Not `--team`: a Marv OS team isn't a team file the CLI could find.
        if let Some(role) = spec.role.as_deref().filter(|r| !r.is_empty()) {
            args.extend(["--role".to_string(), role.to_string()]);
        }
        // Its own session by id (`session`: the chat's, recorded from its last run), never "the latest in the folder",
        // which was a teammate's when two Marvs worked there; and where to record this run's id (`<events>.marv`).
        args.retain(|a| a != "--continue");
        let mut env: Vec<(String, String)> =
            spec.mesh_name.iter().map(|name| ("MARV_MESH_NAME".to_string(), name.clone())).collect();
        if !session.is_empty() {
            env.push(("MARV_RESUME_ID".into(), session.to_string()));
        }
        env.push(("MARV_SESSION_RECORD".into(), marv_session_record(events).display().to_string()));
        env.extend(permissions.env.iter().cloned());
        // Seen idle once its terminal is quiet: the welcome's waving martian kept it "working", so a team member's
        // first task and mail, typed in once it's idle, never went in.
        env.push(("MARV_STILL".into(), "1".into()));
        // A team member works unattended with the team's step limit and model (Marv's own default stopped it at 25
        // steps to ask "Keep going?", with nobody at its terminal).
        if spec.team.is_some() {
            // A team member may commit: its git commands that write run without asking (`--approve git_write`, as the
            // person answering "always" for them), else each commit waited on a card in a terminal nobody watched.
            args.extend(["--approve".to_string(), "git_write".to_string()]);
            env.push(("MARV_MAX_STEPS".into(), spec.max_steps.to_string()));
            if !spec.model.is_empty() {
                env.push(("MARV_MODEL".into(), spec.model.clone()));
            }
        }
        // Marv's own sandbox holds its commands; it reads its own config (`~/.marv`), which the outer sandbox hides.
        return Ok(TerminalCommand {
            argv: std::iter::once(bin.into_os_string()).chain(args.into_iter().map(Into::into)).collect(),
            path: None,
            withheld: std::env::vars()
                .map(|(k, _)| k)
                .filter(|k| {
                    crate::claude_code::PARENT_SESSION.contains(&k.as_str())
                        || matches!(
                            k.as_str(),
                            "MARV_MESH_NAME"
                                | "MARV_RESUME_ID"
                                | "MARV_SESSION_RECORD"
                                | "MARV_MAX_STEPS"
                                | "MARV_MODEL"
                                | "MARV_STILL"
                        )
                })
                .collect(),
            mesh: None,
            env,
        });
    }
    if let Some(why) = crate::claude_code::home_unsandboxed(spec, agent.title) {
        return Err(why);
    }
    // Its own socket, a new name each time: the chat's ended session, and a terminal before this one, remove theirs.
    let server = agent.mesh_via().and_then(|_| {
        let name = format!("harness-term-{}.sock", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        spec.resident.as_ref().and_then(|r| crate::claude_code::mesh_server_at(spec, &r.approver, &name))
    });
    let home = marv_session::paths::home_dir();
    let config: Vec<PathBuf> = agent.config.iter().map(|c| home.join(c)).filter(|p| p.exists()).collect();
    let (out, _notices) = tokio::sync::mpsc::unbounded_channel();
    let (launch, mesh_paths) =
        crate::claude_code::launch_for(&bin, agent.program, &config, agent.env, Vec::new(), spec, server.as_ref(), &out);
    let mut env = term;
    // Its settings files beside the chat's events file (Marv's own folder: private), seen inside the sandbox at
    // [`GUEST_AGENT_FILES`].
    let files = events.with_extension("agent");
    let sandboxed = matches!(launch, crate::claude_code::Launch::Sandboxed { .. });
    // Codex: no animations (its welcome logo redrew ten times a second, so a Codex at its prompt never looked idle and
    // the mail waiting for it was never typed), and each turn's end and last reply told to `events` (its `notify`
    // program, which takes no hook trust), as Claude Code's hooks tell its turns: the reply is its report to whoever
    // sent the task.
    if agent.id == "codex" {
        let shown = if sandboxed {
            crate::harness_sandbox::guest(crate::claude_code::GUEST_EVENTS, events)
        } else {
            events.to_path_buf()
        };
        let shown = shown.display().to_string();
        args.extend(["-c".into(), "tui.animations=false".into(), "-c".into(), codex_notify(&shown)]);
    }
    if let (Some(via), Some((exe, socket))) = (agent.mesh_via(), mesh_paths) {
        let _ = std::fs::create_dir_all(&files);
        let shown = if sandboxed { crate::harness_sandbox::guest(GUEST_AGENT_FILES, &files) } else { files.clone() };
        let (more_args, more_env) = mesh_settings(via, &exe, &socket, &files, &shown);
        args.extend(more_args);
        env.extend(more_env);
    }
    // The person's apps, as Claude Code's terminal gets them.
    if let Some(via) = agent.mesh_via() {
        let shown = if sandboxed { crate::harness_sandbox::guest(GUEST_AGENT_FILES, &files) } else { files.clone() };
        app_settings(via, &crate::claude_code::person_servers(spec), &files, &shown, &mut args, &mut env);
    }
    // What Marv OS is, for its questions about it (Claude Code's comes with its own command).
    match agent.id {
        "codex" => args.extend(["-c".into(), format!("developer_instructions={}", serde_json::Value::from(MARV_OS_BRIEF))]),
        "opencode" => {
            let _ = std::fs::create_dir_all(&files);
            if std::fs::write(files.join("marv-os.md"), MARV_OS_BRIEF).is_ok() {
                let shown = if sandboxed { crate::harness_sandbox::guest(GUEST_AGENT_FILES, &files) } else { files.clone() };
                opencode_instructions(&mut env, &shown.join("marv-os.md"));
            }
        }
        _ => {}
    }
    if let Some(permission) = &permissions.opencode {
        opencode_permission(&mut env, permission);
    }
    Ok(match launch.with_env(env.clone()) {
        crate::claude_code::Launch::Direct(bin) => TerminalCommand {
            argv: std::iter::once(bin.into_os_string()).chain(args.into_iter().map(Into::into)).collect(),
            path: None,
            withheld: std::env::vars().map(|(k, _)| k).filter(|k| crate::claude_code::withheld(k)).collect(),
            mesh: server,
            env,
        },
        crate::claude_code::Launch::Sandboxed { mut plan, claude, env: inside } => {
            if agent.id == "codex" {
                let dest = crate::claude_code::GUEST_EVENTS.into();
                plan.binds.push(marv_sandbox::Bind { source: events.to_path_buf(), dest, writable: true });
            }
            if files.is_dir() {
                plan.binds.push(marv_sandbox::Bind { source: files.clone(), dest: GUEST_AGENT_FILES.into(), writable: false });
            }
            let inner: Vec<std::ffi::OsString> =
                std::iter::once(claude.into_os_string()).chain(args.into_iter().map(Into::into)).collect();
            TerminalCommand {
                argv: crate::harness_sandbox::argv(&plan, &inside, &inner)?,
                path: Some(plan.path.clone()),
                withheld: Vec::new(),
                mesh: server,
                env: Vec::new(),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An agent winget installed is found in its package's folder, even with no link and the PATH unchanged (the OpenAI
    /// CLI's `codex-x86_64-pc-windows-msvc.exe`). Checked against this machine's installs: an agent whose package
    /// folder is there must be found.
    #[cfg(windows)]
    #[test]
    fn an_agent_winget_installed_is_found_in_its_package() {
        let packages = PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap()).join(r"Microsoft\WinGet\Packages");
        for agent in AGENTS {
            let Some(id) = agent.winget else { continue };
            let there = std::fs::read_dir(&packages)
                .map(|d| d.flatten().any(|e| e.file_name().to_string_lossy().starts_with(&format!("{id}_"))))
                .unwrap_or(false);
            if there {
                let found = winget_program(id, agent.program);
                assert!(found.as_deref().is_some_and(Path::is_file), "{id}: {found:?}");
                println!("{id}: {}", found.unwrap().display());
            }
        }
        assert_eq!(winget_program("Nobody.Nothing", "nothing"), None);
    }

    /// OpenCode is told what Marv OS is by an instructions file, in the same inline config as its mesh server (or a
    /// config of its own when it has none).
    #[test]
    fn opencode_gets_the_marv_os_brief_beside_its_mesh_server() {
        let mut env = vec![("OPENCODE_CONFIG_CONTENT".to_string(), r#"{"mcp":{"marv-mesh":{"type":"local"}}}"#.to_string())];
        opencode_instructions(&mut env, Path::new("/run/marv-harness/agent/marv-os.md"));
        let config: serde_json::Value = serde_json::from_str(&env[0].1).unwrap();
        assert_eq!(config["instructions"][0], "/run/marv-harness/agent/marv-os.md");
        assert_eq!(config["mcp"]["marv-mesh"]["type"], "local");
        let mut none = Vec::new();
        opencode_instructions(&mut none, Path::new("/f/marv-os.md"));
        assert_eq!(none[0].0, "OPENCODE_CONFIG_CONTENT");
    }

    /// Codex's turn ends reach the chat's events file: `notify` runs `sh` with the JSON as its last argument, which
    /// the line appends (a path with a quote in it stays one word).
    #[test]
    fn codex_tells_its_turns_end_to_the_events_file() {
        let dir = std::env::temp_dir().join(format!("codex-notify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let events = dir.join("it's.jsonl");
        let setting = codex_notify(&events.display().to_string());
        let argv: Vec<String> = serde_json::from_str(setting.strip_prefix("notify=").unwrap()).unwrap();
        let json = r#"{"type":"agent-turn-complete","last-assistant-message":"hi"}"#;
        let ok = std::process::Command::new(&argv[0]).args(&argv[1..]).arg(json).status().unwrap();
        assert!(ok.success());
        assert_eq!(std::fs::read_to_string(&events).unwrap(), format!("{json}\n"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn each_agent_starts_and_continues_its_own_way() {
        let claude = find("claude-code").unwrap();
        assert_eq!(claude.session_args("s1", false), ["--session-id", "s1"]);
        assert_eq!(claude.session_args("s1", true), ["--resume", "s1"]);
        let codex = find("codex").unwrap();
        assert!(codex.session_args("s1", false).is_empty(), "a new Codex session starts as it starts");
        assert_eq!(codex.session_args("s1", true), ["resume", "--last"]);
        assert_eq!(find(MARV).unwrap().session_args("x", true), ["--continue"]);
        assert!(find("antigravity").unwrap().session_args("x", true).is_empty());
        assert!(claude.takes_session_id() && !codex.takes_session_id());
    }

    #[test]
    fn a_question_only_the_person_can_answer_is_seen() {
        let trust =
            "Quick safety check: Is this a project you created?\n ❯ No, exit\n   Yes, I trust this folder\n Enter to confirm";
        assert!(waiting_question(trust).unwrap().contains("trust this folder"));
        assert!(waiting_question("Cursor Agent\nPress any key to log in...").unwrap().contains("sign in"));
        assert!(waiting_question("❯ Try \"how does <filepath> work?\"\n  ⏵⏵ auto mode on").is_none());
    }

    /// The person's apps reach Codex, OpenCode and Crush as they reach Claude Code, with no secret on Codex's command
    /// line (its variables and headers go through its environment), beside the mesh OpenCode and Crush already have.
    #[test]
    fn codex_opencode_and_crush_get_the_persons_apps_with_no_secret_on_the_command_line() {
        let apps: serde_json::Map<String, serde_json::Value> = serde_json::from_value(serde_json::json!({
            "linear": {"type": "http", "url": "https://mcp.linear.app/mcp", "headers": {"Authorization": "Bearer s3cret"}},
            "my notes": {"type": "stdio", "command": "npx", "args": ["-y", "notes"], "env": {"NOTES_KEY": "k3y"}},
        }))
        .unwrap();
        let files = std::env::temp_dir().join(format!("marv-agent-apps-{}", uuid::Uuid::new_v4()));
        let (mut args, mut env) = (Vec::new(), Vec::new());
        app_settings(MeshVia::CodexConfig, &apps, &files, &files, &mut args, &mut env);
        assert!(args.iter().all(|a| !a.contains("s3cret") && !a.contains("k3y")), "{args:?}");
        assert!(args.contains(&r#"mcp_servers.linear.url="https://mcp.linear.app/mcp""#.to_string()), "{args:?}");
        assert!(args.contains(&r#"mcp_servers.linear.env_http_headers={"Authorization"="MARV_APP_0_HEADER_0"}"#.to_string()));
        assert!(args.contains(&r#"mcp_servers.my-notes.env_vars=["NOTES_KEY"]"#.to_string()), "{args:?}");
        assert!(
            env.contains(&("MARV_APP_0_HEADER_0".into(), "Bearer s3cret".into()))
                && env.contains(&("NOTES_KEY".into(), "k3y".into()))
        );

        let mut env = vec![("OPENCODE_CONFIG_CONTENT".to_string(), r#"{"mcp":{"marv-mesh":{"type":"local"}}}"#.to_string())];
        app_settings(MeshVia::OpenCodeConfig, &apps, &files, &files, &mut Vec::new(), &mut env);
        let config: serde_json::Value = serde_json::from_str(&env[0].1).unwrap();
        assert_eq!(config["mcp"]["marv-mesh"]["type"], "local", "the mesh is kept");
        assert_eq!(config["mcp"]["linear"]["type"], "remote");
        assert_eq!(config["mcp"]["my notes"]["command"], serde_json::json!(["npx", "-y", "notes"]));
        assert_eq!(config["mcp"]["my notes"]["environment"]["NOTES_KEY"], "k3y");

        let mut env = Vec::new();
        app_settings(MeshVia::CrushGlobal, &apps, &files, Path::new("/shown"), &mut Vec::new(), &mut env);
        assert_eq!(env, [("CRUSH_GLOBAL_CONFIG".to_string(), "/shown/crush".to_string())]);
        let crush: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(files.join("crush/crush.json")).unwrap()).unwrap();
        assert_eq!(crush["mcp"]["linear"]["type"], "http");
        assert_eq!(crush["mcp"]["my notes"]["env"]["NOTES_KEY"], "k3y");
        let _ = std::fs::remove_dir_all(&files);
    }

    #[test]
    fn codex_and_opencode_get_the_mesh_their_own_way() {
        let (exe, socket) = (Path::new("/run/marv-harness/marv"), Path::new("/run/marv-harness/mcp.sock"));
        let files = std::env::temp_dir().join(format!("marv-agent-files-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&files).unwrap();
        let (args, env) = mesh_settings(MeshVia::CodexConfig, exe, socket, &files, &files);
        assert!(env.is_empty());
        assert_eq!(args[1], r#"mcp_servers.marv-mesh.command="/run/marv-harness/marv""#);
        assert_eq!(
            args[3],
            format!(
                r#"mcp_servers.marv-mesh.args=["{}","/run/marv-harness/mcp.sock"]"#,
                marv_session::helper_modes::MESH_MCP_SUBCOMMAND
            )
        );
        let (args, env) = mesh_settings(MeshVia::OpenCodeConfig, exe, socket, &files, &files);
        assert!(args.is_empty() && env[0].0 == "OPENCODE_CONFIG_CONTENT");
        let config: serde_json::Value = serde_json::from_str(&env[0].1).unwrap();
        assert_eq!(config["mcp"]["marv-mesh"]["command"][0], "/run/marv-harness/marv");
        let (_, env) = mesh_settings(MeshVia::CrushGlobal, exe, socket, &files, Path::new("/shown"));
        assert_eq!(env[0], ("CRUSH_GLOBAL_CONFIG".to_string(), "/shown/crush".to_string()));
        let crush: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(files.join("crush/crush.json")).unwrap()).unwrap();
        assert_eq!(crush["mcp"]["marv-mesh"]["type"], "stdio");
        #[cfg(unix)] // Windows has no mode bits
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(files.join("crush/crush.json")).unwrap().permissions().mode() & 0o777, 0o600, "private");
        }
        assert_eq!(find("codex").unwrap().mesh_via(), Some(MeshVia::CodexConfig));
        assert_eq!(find("pi").unwrap().mesh_via(), None);
        assert_eq!(find("cursor-agent").unwrap().mesh_via(), None);
        assert_eq!(find("gemini").unwrap().mesh_via(), None);
        let _ = std::fs::remove_dir_all(&files);
    }

    /// The same four modes on each agent, its own way; an agent Marv OS can't set keeps its own settings.
    #[test]
    fn each_agent_takes_the_permission_modes_its_own_way() {
        let on = |id: &str, mode| permission_settings(find(id).unwrap(), mode);
        assert_eq!(on("claude-code", Permissions::Auto).unwrap().args, ["--permission-mode", "auto"]);
        assert_eq!(on("claude-code", Permissions::Full).unwrap().args, ["--permission-mode", "bypassPermissions"]);
        assert_eq!(on("codex", Permissions::Supervised).unwrap().args[1], "read-only");
        assert_eq!(on("gemini", Permissions::Full).unwrap().args, ["--approval-mode", "yolo"]);
        assert_eq!(on("opencode", Permissions::AutoEdits).unwrap().opencode.unwrap()["bash"], "ask");
        assert!(on("claude-code", Permissions::Own).is_none() && on("pi", Permissions::Full).is_none());
        // Marv: a mode that asks holds its yolo off; one that doesn't leaves it to its config.
        let yolo_off = |mode| on("marv", mode).unwrap().env.contains(&("MARV_YOLO".to_string(), "off".to_string()));
        assert!(yolo_off(Permissions::Supervised) && yolo_off(Permissions::AutoEdits));
        assert!(!yolo_off(Permissions::Auto) && !yolo_off(Permissions::Full));
        assert!(on("marv", Permissions::Own).is_none());
        // OpenCode's permission joins its mesh server in one inline config.
        let mut env = vec![("OPENCODE_CONFIG_CONTENT".to_string(), r#"{"mcp":{"marv-mesh":{}}}"#.to_string())];
        opencode_permission(&mut env, &on("opencode", Permissions::Full).unwrap().opencode.unwrap());
        let config: serde_json::Value = serde_json::from_str(&env[0].1).unwrap();
        assert_eq!((config["permission"]["bash"].as_str(), config["mcp"]["marv-mesh"].is_object()), (Some("allow"), true));
        assert_eq!(Permissions::parse(Some("auto-edits")), Permissions::AutoEdits);
        assert_eq!(Permissions::parse(Some("nonsense")), Permissions::Own);
        assert_eq!(Permissions::parse(None), Permissions::Own);
    }

    /// **Full access only inside Marv's sandbox:** nothing asks, so an agent outside it is refused (not started with
    /// it), and a home folder holds every mode above Supervised off, as it holds Marv's yolo.
    #[test]
    fn full_access_runs_only_inside_the_sandbox_and_never_in_the_home_folder() {
        let project = std::env::temp_dir().join(format!("perm-full-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&project).unwrap();
        let spec = ChatSpec { root: project.clone(), harness_sandbox: false, ..crate::host::tests::spec() };
        let events = project.join("events.jsonl");
        for id in ["claude-code", "codex", "opencode"] {
            let Err(why) = command(find(id).unwrap(), &spec, "s", false, &events, Permissions::Full) else {
                panic!("{id} started with full access outside the sandbox");
            };
            assert!(why.contains("only inside Marv's sandbox"), "{why}");
        }
        assert_eq!(Permissions::Full.held(&spec), Permissions::Full);
        let home = ChatSpec { root: marv_session::paths::home_dir(), ..crate::host::tests::spec() };
        assert_eq!(Permissions::Full.held(&home), Permissions::Supervised);
        assert_eq!(Permissions::AutoEdits.held(&home), Permissions::Supervised);
        assert_eq!(Permissions::Own.held(&home), Permissions::Own, "the agent's own settings stay its own");
        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn a_record_without_an_agent_or_with_one_unknown_is_marv() {
        assert_eq!(for_chat(None).id, MARV);
        assert_eq!(for_chat(Some("nonsense")).id, MARV);
        assert_eq!(for_chat(Some("codex")).title, "Codex");
        let ids: std::collections::HashSet<&str> = AGENTS.iter().map(|a| a.id).collect();
        assert_eq!(ids.len(), AGENTS.len(), "ids are unique");
        assert!(
            !ids.contains(SHELL) && AGENTS.iter().all(|a| !a.id.starts_with("own-")),
            "the shell's and own agents' ids are free"
        );
    }

    /// A command's words: spaces separate, double quotes keep a word with spaces, a Windows path's backslashes stay.
    #[test]
    fn a_command_splits_into_words_with_quotes_kept_whole() {
        assert_eq!(split_words("  node ~/tools/agent.js   --fast "), ["node", "~/tools/agent.js", "--fast"]);
        assert_eq!(split_words(r#""C:\My Tools\agent.exe" -x """#), [r"C:\My Tools\agent.exe", "-x", ""]);
        assert!(split_words("   ").is_empty());
        // `~/` is expanded in every word when the command is built, not in the split itself.
        let home = std::path::Path::new("/home/me");
        assert_eq!(expand_tilde("~/tools/agent.js", home), home.join("tools/agent.js").display().to_string());
        assert_eq!(expand_tilde("~", home), "~");
        assert_eq!(expand_tilde("~src/a", home), "~src/a");
        assert_eq!(expand_tilde("--cfg=~/x", home), "--cfg=~/x");
    }

    /// A person's own agent runs its command's program with the rest of the command first, continues with the
    /// arguments they gave (or starts afresh), and is found by its id beside the known agents and the shell.
    #[test]
    fn an_own_agent_runs_its_command_and_is_found_by_its_id() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("my-agent");
        std::fs::write(&program, "").unwrap();
        let command = format!("\"{}\" --fast", program.display());
        let own = OwnAgent::new("My Agent!", &command, "--continue", &[]).unwrap();
        assert_eq!(own.id, "own-my-agent");
        assert_eq!(OwnAgent::new("My Agent", &command, "", &["own-my-agent"]).unwrap().id, "own-my-agent-2");
        assert!(OwnAgent::new("", &command, "", &[]).is_err());
        assert!(OwnAgent::new("X", "no-such-program-marv-test", "", &[]).is_err());
        // An unclosed quote is refused in the command and in the resume arguments, with a plain message.
        let open = format!("\"{} --fast", program.display());
        assert!(OwnAgent::new("X", &open, "", &[]).unwrap_err().contains("isn't closed"));
        assert!(OwnAgent::new("X", &command, "--session \"my id", &[]).unwrap_err().contains("isn't closed"));
        set_own(std::slice::from_ref(&own));
        let agent = find("own-my-agent").expect("found");
        assert_eq!((agent.kind, agent.title, agent.lead), (Kind::Own, "My Agent!", &["--fast"][..]));
        assert_eq!(agent.installed(), Some(program.clone()));
        assert_eq!(agent.session_args("", true), ["--continue"]);
        assert!(all().iter().any(|a| a.id == SHELL && a.kind == Kind::Shell));
        // `~/` in a later word is expanded when the agent is built (the documented `node ~/tools/agent.js --fast`).
        let tilde = OwnAgent::new("Tilde", &format!("\"{}\" ~/tools/agent.js --fast", program.display()), "", &[]).unwrap();
        set_own(std::slice::from_ref(&tilde));
        let home = marv_session::paths::home_dir();
        assert_eq!(find("own-tilde").unwrap().lead, [home.join("tools/agent.js").display().to_string().as_str(), "--fast"]);
        set_own(&[]);
        assert!(find("own-my-agent").is_none());
    }
}
