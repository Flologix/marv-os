//! The command line. Ports the argument handling of Marv's `src/cli.tsx` (and `bin/marv`).
//!
//! ```text
//! marv              start an interactive session
//! marv -c / -r      continue the latest session here / pick one to resume
//! marv -p <prompt>  one non-interactive turn (`-p -` reads stdin); `marv run <prompt>` is the same
//! marv mesh ls      the live Marvs on this machine (mesh v2)
//! marv mesh wire 'alpha -> beta:inbox'
//! marv mesh plug alpha --as claude   (an MCP server command: another agent plugs into alpha's switchboard)
//! marv update       update this install to the newest release (`--check`: only say whether there is one)
//! ```
//!
//! Port notes:
//! - `bin/marv` existed only to rename the process from `bun` to `marv`; a native binary is already `marv`.
//! - Marv parsed argv by hand (last `-p` wins, `--flag value` anywhere); clap gives the same flags. **Argument errors
//!   exit 1** (Marv's code for a bad option), not clap's default 2, which `marv -p` uses for "stopped".
//! - `--version`/`-v` and `--help`/`-h` exit 0, as in Marv.
//! - `__html-convert` is hidden: the web reader re-executes `marv __html-convert` to convert a page in a process of
//!   its own (a conversion that crashes is an answer, not a crash).
//! - `__serve <folder> <port>` is hidden: the process tool's static file server (`marv_tools::serve`).
//! - `__guard <parent-pid> -- <command…>` is hidden: Seatbelt's process supervisor (#165, `marv_tools::guard`).

use clap::{ArgAction, Parser, Subcommand, ValueEnum};
pub use marv_session::helper_modes::HelperMode;

pub const AFTER_HELP: &str = "Config:
  ~/.marv/config.json   created by the setup screen (/setup to change it)
  ~/.marv/sessions/     saved conversations, one folder per project
  ~/.marv/trajectories/ every turn, step by step, with your ratings (/trajectories)
  ~/.marv/agents/       your subagent types (a project's go in .marv/agents/)
  ~/.marv/mcp.json      your MCP servers (a project's go in .mcp.json; /mcp)
  ~/.marv/worktrees/    subagents' worktrees while they run
  OPENROUTER_API_KEY    overrides the saved OpenRouter key
  ZAI_API_KEY           overrides the saved Z.ai Coding Plan key
  KIMI_API_KEY          overrides the saved Kimi Code key
  DEEPSEEK_API_KEY      overrides the saved DeepSeek key
  QWEN_API_KEY          overrides the saved Qwen Coding Plan key
  OLLAMA_HOST           where Ollama runs (default localhost:11434)
  MARV_LOCAL_MODEL      a GGUF file to run with llama.cpp (as --local-model)
  MARV_MODEL            overrides the saved model
  MARV_MAX_STEPS        overrides the saved step limit (0 or off: never ask)
  MARV_CONFIG_DIR       use a different config directory (read from the environment only)
  MARV_NO_UPDATE_CHECK  1: don't check for a newer Marv at startup (or \"updateCheck\": false in the config)

Exit codes for -p: 0 finished, 1 error, 2 stopped (step limit, output limit, loop), 3 an approval it couldn't get";

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// The final reply as text (default).
    Text,
    /// A result object as JSON.
    Json,
    /// Each event as a JSON line.
    StreamJson,
}

#[derive(Debug, Parser)]
#[command(
    name = "marv",
    about = "Marv: a terminal coding agent",
    version,
    disable_version_flag = true,
    after_help = AFTER_HELP
)]
pub struct Cli {
    /// Print the version.
    #[arg(short = 'v', long = "version", action = ArgAction::Version)]
    pub version: Option<bool>,

    /// Run one non-interactive turn and print the reply (`-` reads the prompt from stdin).
    #[arg(short = 'p', long = "prompt", value_name = "PROMPT", allow_hyphen_values = true)]
    pub prompt: Option<String>,

    /// How -p prints its result.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub output: OutputFormat,

    /// Auto-approve a scope for this session (git_write, network, web, mcp, files, memory, agent_worktree, process,
    /// github, mesh:<receptacle>) — like answering "always" at the start. Works in -p and the TUI; the config's
    /// "approve" list is the same thing.
    #[arg(long = "approve", value_name = "SCOPE")]
    pub approve: Vec<String>,

    /// With --approve, in -p: also approve egress (web, network, MCP, mesh, GitHub) after the session read
    /// untrusted content. Without it, those calls are refused.
    #[arg(long = "approve-tainted")]
    pub approve_tainted: bool,

    /// With --approve files, in -p: also approve writes to protected paths (.git, .env*, AGENTS.md, .marv/…).
    /// Without it, those writes are refused.
    #[arg(long = "approve-protected")]
    pub approve_protected: bool,

    /// In -p only, for a disposable container or CI machine: run commands without the sandbox and approve each one
    /// (with access to everything this user can reach). Protected paths and egress after untrusted content still
    /// need their own flags: an unsandboxed command's output taints the session, so a container run needs
    /// --approve-tainted too. Never from the config, and refused in the TUI.
    #[arg(long = "dangerously-run-unsandboxed")]
    pub dangerously_run_unsandboxed: bool,

    /// In -p: use this project's instruction files (AGENTS.md, CLAUDE.md), skills and agent types as yours for this
    /// run, as if you'd trusted them (/trust in the TUI). Without it, they count only where you trusted exactly these
    /// files; otherwise the model reads the instruction files as the project's data, and its skills and agent types
    /// aren't offered.
    #[arg(long = "trust-project")]
    pub trust_project: bool,

    /// In -p: the run's time limit in seconds. The model is told its budget, each command is capped at the time left,
    /// and command results say how much is left. Nothing stops the run at the limit: the caller does.
    #[arg(long = "time-limit", value_name = "SECONDS")]
    pub time_limit: Option<u64>,

    /// Steps before the turn stops (0 never stops; default: your config's maxSteps).
    #[arg(long = "max-steps", value_name = "N")]
    pub max_steps: Option<u32>,

    /// Use a different model.
    #[arg(short = 'm', long)]
    pub model: Option<String>,

    /// Run a GGUF model on this machine with llama.cpp, for this run: Marv starts llama-server (from your PATH, or
    /// "llamaCpp": {"server"} in the config) and stops it when it exits. Nothing is saved.
    #[arg(long = "local-model", value_name = "GGUF")]
    pub local_model: Option<String>,

    /// Continue the latest session in this folder.
    #[arg(short = 'c', long = "continue")]
    pub continue_: bool,

    /// Pick an earlier session to resume.
    #[arg(short = 'r', long)]
    pub resume: bool,

    /// Start this chat as a role (.marv/roles/<name>.md, ~/.marv/roles/<name>.md, or the built-in coordinator and
    /// worker): its tools, the files it may write and whose mesh messages it takes as tasks. Fixed for the chat's life.
    #[arg(long = "role", value_name = "NAME")]
    pub role: Option<String>,

    /// The team this chat is one of (`marv team start` opens its panes with it): its shared brief follows the role in
    /// the system prompt. A team in .marv/team.md (once trusted), ~/.marv/teams/<name>.md, or a built-in template.
    #[arg(long = "team", value_name = "NAME", requires = "role")]
    pub team: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Alias for -p: run one non-interactive turn (no prompt: read stdin).
    Run {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        prompt: Vec<String>,
    },
    /// The mesh: wires between sandboxed agents.
    #[command(subcommand)]
    Mesh(MeshCommand),
    /// The runtime kernel's event log.
    #[command(subcommand)]
    Kernel(KernelCommand),
    /// Teams: chats started together in a folder, each as a role (`.marv/team.md`, `~/.marv/teams`, built-ins).
    #[command(subcommand)]
    Team(TeamCommand),
    /// Update Marv to the newest release (npm or install.sh, however this one was installed).
    Update {
        /// Only say whether there's a newer release (exit 1 if there is), without updating.
        #[arg(long)]
        check: bool,
    },
    /// Converts one web page: `{html, url}` JSON on stdin, the result on stdout (the web reader's own process).
    #[command(name = "__html-convert", hide = true)]
    HtmlConvert,
    /// Runs the project's checks in the current folder (inside the sandbox): `marv_session::checks::runner`.
    #[command(name = "__run-checks", hide = true)]
    RunChecks { spec: String },
    /// The process door's relay, inside a sandbox (Marv #51): `marv_tools::relay`.
    #[command(name = "__relay", hide = true)]
    Relay { spec: String },
    /// A Claude Code chat's mesh tools (the harness plan, phase 3): stdio joined to the desktop's MCP socket for it.
    /// Run from inside the chat's outer sandbox (phase 4), where only this plain binary is shown.
    #[command(name = "__mesh-mcp", hide = true)]
    MeshMcp { socket: String },
    /// A terminal agent's event appended to its chat's events file (Windows has no `sh`): `helper_modes::append_line`.
    #[command(name = "__append-line", hide = true)]
    AppendLine {
        file: String,
        line: Option<String>,
        #[arg(long)]
        echo: Option<String>,
    },
    /// The process tool's static file server for a project folder, inside a sandbox: `marv_tools::serve`.
    #[command(name = marv_tools::serve::SERVE_SUBCOMMAND, hide = true)]
    Serve { folder: String, port: u16 },
    /// Runs a command in a session of its own and kills its process group when `parent` dies (#165): `guard`.
    #[command(name = marv_sandbox::seatbelt_backend::GUARD_SUBCOMMAND, hide = true)]
    Guard {
        parent: u32,
        #[arg(last = true, required = true)]
        command: Vec<std::ffi::OsString>,
    },
    /// Windows: runs a command in the AppContainer a spec file describes (`marv_sandbox::appcontainer`).
    #[command(name = "__appcontainer", hide = true)]
    AppContainer {
        spec: String,
        #[arg(last = true, required = true)]
        command: Vec<std::ffi::OsString>,
    },
}

#[derive(Debug, Subcommand)]
pub enum TeamCommand {
    /// The teams this folder can start, with their members' roles.
    Ls,
    /// Starts a team here: the folder rule (asked y/N), then one Herdr pane per member running `marv --role <role>`
    /// (outside Herdr: the commands to run in your terminals).
    Start {
        /// A team or template name (default: the folder's .marv/team.md, else `build-review`).
        name: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum MeshCommand {
    /// List the live Marv switchboards on this machine.
    Ls,
    /// Ask for a wire, e.g. `marv mesh wire 'alpha -> beta:inbox'`; each Marv's user approves it.
    Wire {
        /// `<marv>[/<agent>] -> <marv>[/<agent>]:inbox`, or `<marv>[/<agent>] -> <receptacle>[:<mode>]`
        #[arg(num_args = 1.., trailing_var_arg = true)]
        spec: Vec<String>,
    },
    /// Plug another agent into a Marv's switchboard over stdin/stdout (use it as an MCP server command).
    Plug {
        /// The Marv's name (`marv mesh ls`).
        session: String,
        /// The plug's name (default `ext-<pid>`).
        #[arg(long = "as", value_name = "NAME")]
        name: Option<String>,
    },
    /// Let your Marvs under a folder message each other without asking (asks y/N first; needs a terminal).
    Allow {
        /// The folder whose Marvs (desktop chats and terminal Marvs, and their subfolders') the rule covers.
        folder: String,
        /// Another folder they may message, and be messaged from (default: each other, within `folder`).
        #[arg(long, value_name = "FOLDER")]
        to: Option<String>,
        /// How long it lasts, e.g. `1h` or `30m` (default: until revoked).
        #[arg(long = "for", value_name = "DURATION")]
        lasts: Option<String>,
        /// Instead: let them read each other's chats (`chat_read`: the person's and assistants' words, never tool
        /// inputs or outputs; a chat marked private refuses). Its own rule: it doesn't let them message each other.
        #[arg(long)]
        read: bool,
    },
    /// Revoke a rule (`marv mesh policies` lists them): what it allowed stops at the next message.
    Revoke { id: u64 },
    /// List your mesh rules.
    Policies,
}

#[derive(Debug, Subcommand)]
pub enum KernelCommand {
    /// Rebuild the registry, the rules and the states from event logs (`~/.marv/events/<yyyy-mm-dd>.jsonl`; default:
    /// today's) and print them.
    Replay {
        /// Log files, replayed in this order.
        files: Vec<std::path::PathBuf>,
    },
    /// List the chats you let keep working while you're away (`~/.marv/mesh/grants.json`): chat, folder, expiry, and
    /// today's spend. Grants are made and revoked only in Marv desktop.
    Grants,
    /// What your chats, folders, grants and cores used today (`--week`: the last 7 days), from the event log.
    Usage {
        #[arg(long)]
        week: bool,
    },
    /// Your budgets (`~/.marv/mesh/budgets.json`): with no amount, list them; with one (dollars a day, or `off`), set
    /// the global one, or a folder's (`--folder`) or a desktop chat's (`--chat`), after a y/N at a terminal.
    Budget {
        /// Dollars a day (`10`, `2.50`), or `off` to remove it.
        amount: Option<String>,
        #[arg(long, conflicts_with = "chat")]
        folder: Option<String>,
        #[arg(long)]
        chat: Option<String>,
    },
    /// Every Marv on this machine, as Marv desktop's Activity tab shows it: state, context, the last minute's split
    /// (model / tools / waiting for you), $/h, cache hit %, model; subagents and real processes under each.
    Ps {
        /// Refresh every 2 seconds, like `top` (Ctrl-C stops).
        #[arg(long)]
        watch: bool,
    },
    /// The kernel's log as sentences, the desktop's timeline (no message text): who woke whom, what a grant ran,
    /// what was refused. Default: today's log.
    Log {
        /// Log files, in this order.
        files: Vec<std::path::PathBuf>,
    },
    /// About this Marv: the kernel's version, the limits in force and where each store lives (no conversation text,
    /// so a support ticket can quote it).
    About,
}

/// What the process should do, resolved from the arguments.
#[derive(Debug, PartialEq, Eq)]
pub enum Mode {
    /// The TUI. `resume`: `Some(true)` for -c (latest), `Some(false)` for -r (pick). `approve`: the `--approve`
    /// scopes (#98), which seed "always" like the config's list.
    Interactive {
        resume: Option<bool>,
        approve: Vec<String>,
        /// `--role`: the chat's role (`marv_session::roles`).
        role: Option<String>,
    },
    /// `-p`/`run`. `prompt` is `-` for stdin.
    Headless {
        prompt: String,
    },
    MeshLs,
    /// `marv update [--check]`.
    Update {
        check: bool,
    },
    MeshWire {
        spec: String,
    },
    MeshPlug {
        session: String,
        name: Option<String>,
    },
    MeshAllow {
        folder: String,
        to: Option<String>,
        lasts: Option<String>,
        read: bool,
    },
    MeshRevoke {
        id: u64,
    },
    MeshPolicies,
    TeamLs,
    TeamStart {
        name: Option<String>,
    },
    KernelReplay {
        files: Vec<std::path::PathBuf>,
    },
    KernelGrants,
    KernelUsage {
        week: bool,
    },
    KernelBudget {
        amount: Option<String>,
        folder: Option<String>,
        chat: Option<String>,
    },
    KernelPs {
        watch: bool,
    },
    KernelLog {
        files: Vec<std::path::PathBuf>,
    },
    KernelAbout,
    /// A hidden helper (`__html-convert`, `__run-checks`, `__relay`, `__guard`): `marv_session::helper_modes`, which
    /// Marv desktop answers too.
    Helper(HelperMode),
}

impl Cli {
    /// `marv run <prompt>` is `marv -p <prompt>` (no prompt: stdin).
    pub fn mode(&self) -> Mode {
        match &self.command {
            Some(Command::Run { prompt }) => {
                Mode::Headless { prompt: if prompt.is_empty() { "-".into() } else { prompt.join(" ") } }
            }
            Some(Command::Mesh(MeshCommand::Ls)) => Mode::MeshLs,
            Some(Command::Update { check }) => Mode::Update { check: *check },
            Some(Command::Mesh(MeshCommand::Wire { spec })) => Mode::MeshWire { spec: spec.join(" ") },
            Some(Command::Mesh(MeshCommand::Plug { session, name })) => {
                Mode::MeshPlug { session: session.clone(), name: name.clone() }
            }
            Some(Command::Mesh(MeshCommand::Allow { folder, to, lasts, read })) => {
                Mode::MeshAllow { folder: folder.clone(), to: to.clone(), lasts: lasts.clone(), read: *read }
            }
            Some(Command::Mesh(MeshCommand::Revoke { id })) => Mode::MeshRevoke { id: *id },
            Some(Command::Mesh(MeshCommand::Policies)) => Mode::MeshPolicies,
            Some(Command::Team(TeamCommand::Ls)) => Mode::TeamLs,
            Some(Command::Team(TeamCommand::Start { name })) => Mode::TeamStart { name: name.clone() },
            Some(Command::Kernel(KernelCommand::Replay { files })) => Mode::KernelReplay { files: files.clone() },
            Some(Command::Kernel(KernelCommand::Grants)) => Mode::KernelGrants,
            Some(Command::Kernel(KernelCommand::Usage { week })) => Mode::KernelUsage { week: *week },
            Some(Command::Kernel(KernelCommand::Budget { amount, folder, chat })) => {
                Mode::KernelBudget { amount: amount.clone(), folder: folder.clone(), chat: chat.clone() }
            }
            Some(Command::Kernel(KernelCommand::Ps { watch })) => Mode::KernelPs { watch: *watch },
            Some(Command::Kernel(KernelCommand::Log { files })) => Mode::KernelLog { files: files.clone() },
            Some(Command::Kernel(KernelCommand::About)) => Mode::KernelAbout,
            Some(Command::HtmlConvert) => Mode::Helper(HelperMode::HtmlConvert),
            Some(Command::RunChecks { spec }) => Mode::Helper(HelperMode::RunChecks { spec: spec.clone() }),
            Some(Command::Relay { spec }) => Mode::Helper(HelperMode::Relay { spec: spec.clone() }),
            Some(Command::MeshMcp { socket }) => Mode::Helper(HelperMode::MeshMcp { socket: socket.clone() }),
            Some(Command::AppendLine { file, line, echo }) => {
                Mode::Helper(HelperMode::AppendLine { file: file.clone(), line: line.clone(), echo: echo.clone() })
            }
            Some(Command::Serve { folder, port }) => Mode::Helper(HelperMode::Serve { folder: folder.clone(), port: *port }),
            Some(Command::Guard { parent, command }) => {
                Mode::Helper(HelperMode::Guard { parent: *parent, command: command.clone() })
            }
            Some(Command::AppContainer { spec, command }) => {
                Mode::Helper(HelperMode::AppContainer { spec: spec.clone(), command: command.clone() })
            }
            None => match &self.prompt {
                Some(prompt) => Mode::Headless { prompt: prompt.clone() },
                None => Mode::Interactive {
                    resume: if self.continue_ {
                        Some(true)
                    } else if self.resume {
                        Some(false)
                    } else {
                        None
                    },
                    approve: self.approve.clone(),
                    role: self.role.clone(),
                },
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("marv").chain(args.iter().copied())).unwrap()
    }

    #[test]
    fn modes() {
        assert_eq!(parse(&[]).mode(), Mode::Interactive { resume: None, approve: vec![], role: None });
        assert_eq!(parse(&["-c"]).mode(), Mode::Interactive { resume: Some(true), approve: vec![], role: None });
        assert_eq!(parse(&["-r"]).mode(), Mode::Interactive { resume: Some(false), approve: vec![], role: None });
        assert_eq!(
            parse(&["--role", "frontend"]).mode(),
            Mode::Interactive { resume: None, approve: vec![], role: Some("frontend".into()) }
        );
        // #98: --approve is accepted in the TUI too, not only -p.
        assert_eq!(
            parse(&["--approve", "web", "-c"]).mode(),
            Mode::Interactive { resume: Some(true), approve: vec!["web".into()], role: None }
        );
        assert_eq!(parse(&["-p", "fix it"]).mode(), Mode::Headless { prompt: "fix it".into() });
        assert_eq!(parse(&["-p", "-"]).mode(), Mode::Headless { prompt: "-".into() });
        assert_eq!(parse(&["run", "fix", "it"]).mode(), Mode::Headless { prompt: "fix it".into() });
        assert_eq!(parse(&["run"]).mode(), Mode::Headless { prompt: "-".into() });
        assert_eq!(
            parse(&["mesh", "wire", "alpha", "->", "beta:inbox"]).mode(),
            Mode::MeshWire { spec: "alpha -> beta:inbox".into() }
        );
        assert_eq!(
            parse(&["mesh", "plug", "alpha", "--as", "claude"]).mode(),
            Mode::MeshPlug { session: "alpha".into(), name: Some("claude".into()) }
        );
        assert_eq!(parse(&["team", "ls"]).mode(), Mode::TeamLs);
        assert_eq!(parse(&["team", "start", "web"]).mode(), Mode::TeamStart { name: Some("web".into()) });
        assert_eq!(parse(&["team", "start"]).mode(), Mode::TeamStart { name: None });
        assert_eq!(parse(&["--role", "frontend", "--team", "web"]).team.as_deref(), Some("web"));
        assert!(Cli::try_parse_from(["marv", "--team", "web"]).is_err(), "--team needs --role");
        assert_eq!(
            parse(&["mesh", "allow", "~/Projects/acme", "--for", "1h"]).mode(),
            Mode::MeshAllow { folder: "~/Projects/acme".into(), to: None, lasts: Some("1h".into()), read: false }
        );
        assert_eq!(
            parse(&["mesh", "allow", "~/Projects/acme", "--read"]).mode(),
            Mode::MeshAllow { folder: "~/Projects/acme".into(), to: None, lasts: None, read: true }
        );
        assert_eq!(parse(&["mesh", "revoke", "3"]).mode(), Mode::MeshRevoke { id: 3 });
        assert_eq!(parse(&["mesh", "policies"]).mode(), Mode::MeshPolicies);
        assert_eq!(parse(&["update"]).mode(), Mode::Update { check: false });
        assert_eq!(parse(&["update", "--check"]).mode(), Mode::Update { check: true });
        assert_eq!(parse(&["__html-convert"]).mode(), Mode::Helper(HelperMode::HtmlConvert));
        assert_eq!(parse(&["__run-checks", "{}"]).mode(), Mode::Helper(HelperMode::RunChecks { spec: "{}".into() }));
        assert_eq!(parse(&["__relay", "{}"]).mode(), Mode::Helper(HelperMode::Relay { spec: "{}".into() }));
        assert_eq!(
            parse(&["__mesh-mcp", "/run/x.sock"]).mode(),
            Mode::Helper(HelperMode::MeshMcp { socket: "/run/x.sock".into() })
        );
        assert_eq!(
            parse(&["__serve", "dist", "4173"]).mode(),
            Mode::Helper(HelperMode::Serve { folder: "dist".into(), port: 4173 })
        );
        assert_eq!(
            parse(&["__appcontainer", "s.json", "--", "bash", "-c", "--x"]).mode(),
            Mode::Helper(HelperMode::AppContainer {
                spec: "s.json".into(),
                command: vec!["bash".into(), "-c".into(), "--x".into()]
            })
        );
        assert_eq!(
            parse(&["__guard", "42", "--", "bash", "-c", "--x"]).mode(),
            Mode::Helper(HelperMode::Guard { parent: 42, command: vec!["bash".into(), "-c".into(), "--x".into()] })
        );
    }

    /// #124: the radio is the user's own config (or `/radio on`) only: no flag turns it on, so nothing that starts Marv
    /// for them (a script, a project's tooling) can.
    #[test]
    fn kernel_replay_takes_its_files_in_order() {
        assert_eq!(parse(&["kernel", "replay"]).mode(), Mode::KernelReplay { files: vec![] });
        assert_eq!(
            parse(&["kernel", "replay", "b.jsonl", "a.jsonl"]).mode(),
            Mode::KernelReplay { files: vec!["b.jsonl".into(), "a.jsonl".into()] }
        );
        assert_eq!(parse(&["kernel", "grants"]).mode(), Mode::KernelGrants);
        assert_eq!(parse(&["kernel", "usage", "--week"]).mode(), Mode::KernelUsage { week: true });
        assert_eq!(
            parse(&["kernel", "budget", "10", "--folder", "acme"]).mode(),
            Mode::KernelBudget { amount: Some("10".into()), folder: Some("acme".into()), chat: None }
        );
        assert_eq!(parse(&["kernel", "ps"]).mode(), Mode::KernelPs { watch: false });
        assert_eq!(parse(&["kernel", "ps", "--watch"]).mode(), Mode::KernelPs { watch: true });
        assert_eq!(parse(&["kernel", "log"]).mode(), Mode::KernelLog { files: vec![] });
        assert_eq!(parse(&["kernel", "about"]).mode(), Mode::KernelAbout);
        // Listing only: nothing on the command line makes or widens a grant.
        for args in [&["kernel", "grants", "add"][..], &["kernel", "grant"], &["kernel", "grants", "--cap", "100"]] {
            assert!(Cli::try_parse_from(std::iter::once("marv").chain(args.iter().copied())).is_err(), "{args:?}");
        }
    }

    #[test]
    fn no_flag_turns_the_radio_on() {
        for args in [&["--radio"][..], &["--radio", "on"], &["--mesh-radio"], &["-p", "x", "--radio"]] {
            assert!(Cli::try_parse_from(std::iter::once("marv").chain(args.iter().copied())).is_err(), "{args:?}");
        }
    }

    #[test]
    fn headless_flags() {
        let cli = parse(&[
            "-p",
            "x",
            "--output",
            "stream-json",
            "--approve",
            "git_write",
            "--approve",
            "mesh:agent.parent.inbox",
            "--max-steps",
            "0",
            "-m",
            "m",
            "-c",
        ]);
        assert_eq!(cli.output, OutputFormat::StreamJson);
        assert_eq!(cli.approve, vec!["git_write", "mesh:agent.parent.inbox"]);
        assert!(!cli.approve_tainted && !cli.approve_protected);
        let opted = parse(&["-p", "x", "--approve", "web", "--approve-tainted", "--approve-protected"]);
        assert!(opted.approve_tainted && opted.approve_protected);
        assert!(!opted.trust_project && parse(&["-p", "x", "--trust-project"]).trust_project);
        assert_eq!((cli.max_steps, cli.model.as_deref(), cli.continue_), (Some(0), Some("m"), true));
    }

    #[test]
    fn bad_values_are_errors() {
        let bad = |args: &[&str]| Cli::try_parse_from(std::iter::once("marv").chain(args.iter().copied())).is_err();
        assert!(bad(&["-p", "x", "--output", "yaml"]));
        assert!(bad(&["-p", "x", "--max-steps", "-1"]));
        assert!(bad(&["-p", "x", "--max-steps", "2.5"]));
        assert!(bad(&["-p"]));
        // The guard's command comes after `--` only, and there must be one.
        assert!(bad(&["__guard", "42"]));
        assert!(bad(&["__guard", "42", "--"]));
        assert!(bad(&["__guard", "42", "bash"]));
        assert!(bad(&["__guard", "x", "--", "bash"]));
    }

    #[test]
    fn the_command_line_is_well_formed() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    // The flag's help names the scope names by hand; it must not drift from the real list (#98).
    #[test]
    fn the_help_names_every_approve_scope() {
        let help: String = {
            use clap::CommandFactory as _;
            Cli::command()
                .get_arguments()
                .find(|a| a.get_long() == Some("approve"))
                .map(|a| a.get_long_help().or_else(|| a.get_help()).map(|h| h.to_string()).unwrap_or_default())
                .unwrap_or_default()
        };
        for (name, _) in marv_session::approvals::APPROVE_PREFIXES {
            assert!(help.contains(name), "the --approve help doesn't name \"{name}\": {help}");
        }
        assert!(help.contains("TUI"), "the help should say --approve works in the TUI too: {help}");
    }
}
