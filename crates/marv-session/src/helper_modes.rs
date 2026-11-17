//! Marv's hidden helper modes: the subcommands Marv runs *itself* as (a process of its own, often inside a sandbox),
//! shared by every binary that runs sessions. New in marv-rs.
//!
//! - `__html-convert`: the web reader's converter process (`marv_tools::web::html_convert_cli`);
//! - `__run-checks <spec>`: the project checks' runner, inside the sandbox ([`crate::checks::runner`]);
//! - `__relay <spec>`: the process door's relay, inside a sandbox (`marv_tools::relay`, Marv #51);
//! - `__serve <folder> <port>`: the process tool's static file server (`marv_tools::serve`);
//! - `__guard <parent-pid> -- <command…>`: Seatbelt's process supervisor (`marv_tools::guard`, #165);
//! - `__mesh-mcp <socket>`: a Claude Code chat's mesh tools (the harness plan, phase 3): stdin and stdout joined to the
//!   desktop's MCP socket for that chat ([`mesh_mcp`]), which Claude Code starts as an MCP server.
//! - `__append-line <file> [line] [--echo <text>]`: a terminal agent's event, as one line, appended to its chat's
//!   events file ([`append_line`]): `line`, or stdin (Claude Code's hooks) with its newlines dropped; then prints
//!   `text` (Claude Code's status line shows it). What `sh` and `tr` do on Unix, for Windows, which has neither.
//!
//! Each is reached through `std::env::current_exe()` (the relay's and the checks' bind mounts, the converter, the
//! guard), or through the helper binary a program names at start (`marv_sandbox::helper_exe`: Marv desktop's bundles
//! ship the plain `marv` for the first three, mk-ssf), so **every** binary that runs a session must answer them, before it does anything else: `marv` parses them
//! with clap ([`HelperMode`] is what its `Mode` carries), and Marv desktop with [`from_args`] at the top of its `main`
//! (mk-60m.10: without it, `process start` in the desktop ran the Tauri app inside the sandbox, which died with
//! "Failed to initialize gtk backend", code 101).
//!
//! The first argument decides: a person who starts Marv desktop with a relative folder that happens to be named like a
//! helper (`marv-desktop __relay`) gets the helper, not the app. Name such a folder `./__relay` (or by its full path).

use std::ffi::{OsStr, OsString};
use std::process::ExitCode;

pub use marv_sandbox::helper_exe::{bundled_helper, helper_exe, set_helper_exe};
use marv_sandbox::seatbelt_backend::GUARD_SUBCOMMAND;
use marv_tools::relay::RELAY_SUBCOMMAND;
use marv_tools::serve::SERVE_SUBCOMMAND;

use crate::checks::runner::RUNNER_SUBCOMMAND;

/// `__html-convert`.
pub const HTML_CONVERT_SUBCOMMAND: &str = "__html-convert";
/// `__mesh-mcp`.
pub const MESH_MCP_SUBCOMMAND: &str = "__mesh-mcp";
/// `__append-line`.
pub const APPEND_LINE_SUBCOMMAND: &str = "__append-line";

/// One hidden helper mode, with its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelperMode {
    HtmlConvert,
    RunChecks { spec: String },
    Relay { spec: String },
    Serve { folder: String, port: u16 },
    Guard { parent: u32, command: Vec<OsString> },
    AppContainer { spec: String, command: Vec<OsString> },
    MeshMcp { socket: String },
    AppendLine { file: String, line: Option<String>, echo: Option<String> },
}

impl HelperMode {
    /// Runs the mode to its end; the caller exits with the code. Call it before any runtime or window starts.
    pub fn run(self) -> ExitCode {
        match self {
            Self::HtmlConvert => match marv_tools::web::html_convert_cli::main() {
                Ok(()) => ExitCode::SUCCESS,
                Err(_) => ExitCode::from(1),
            },
            Self::RunChecks { spec } => crate::checks::runner::main(&spec),
            Self::Relay { spec } => marv_tools::relay::main(&spec),
            Self::Serve { folder, port } => marv_tools::serve::main(&folder, port),
            Self::Guard { parent, command } => marv_tools::guard::main(parent, &command),
            Self::AppContainer { spec, command } => app_container(std::path::Path::new(&spec), &command),
            Self::MeshMcp { socket } => mesh_mcp(std::path::Path::new(&socket)),
            Self::AppendLine { file, line, echo } => append_line(std::path::Path::new(&file), line, echo.as_deref()),
        }
    }
}

/// The helper mode `args` ask for (the arguments after the program's name), as Marv itself writes them:
/// `None` when the first one isn't a helper's name (start normally), `Some(Err)` when it is but the rest don't fit
/// (the caller exits 127: it must not start normally either, inside a sandbox).
pub fn from_args(args: &[OsString]) -> Option<Result<HelperMode, String>> {
    let name = args.first()?.to_str()?;
    let spec = || match args {
        [_, spec] => spec.to_str().map(str::to_string).ok_or_else(|| format!("{name}: the spec isn't UTF-8")),
        _ => Err(format!("usage: {name} <spec>")),
    };
    Some(match name {
        HTML_CONVERT_SUBCOMMAND if args.len() == 1 => Ok(HelperMode::HtmlConvert),
        HTML_CONVERT_SUBCOMMAND => Err(format!("usage: {name}")),
        RUNNER_SUBCOMMAND => spec().map(|spec| HelperMode::RunChecks { spec }),
        RELAY_SUBCOMMAND => spec().map(|spec| HelperMode::Relay { spec }),
        MESH_MCP_SUBCOMMAND => spec().map(|socket| HelperMode::MeshMcp { socket }),
        APPEND_LINE_SUBCOMMAND => {
            append_line_args(&args[1..]).ok_or_else(|| format!("usage: {name} <file> [line] [--echo <text>]"))
        }
        SERVE_SUBCOMMAND => match args {
            [_, folder, port] => match (folder.to_str(), port.to_str().and_then(|p| p.parse().ok())) {
                (Some(folder), Some(port)) => Ok(HelperMode::Serve { folder: folder.to_string(), port }),
                _ => Err(format!("{name}: the folder isn't UTF-8 or the port isn't a number")),
            },
            _ => Err(format!("usage: {name} <folder> <port>")),
        },
        marv_appcontainer::SUBCOMMAND => match args {
            [_, spec, dashes, command @ ..] if dashes.as_os_str() == OsStr::new("--") && !command.is_empty() => {
                match spec.to_str() {
                    Some(spec) => Ok(HelperMode::AppContainer { spec: spec.to_string(), command: command.to_vec() }),
                    None => Err(format!("{name}: the spec's path isn't UTF-8")),
                }
            }
            _ => Err(format!("usage: {name} <spec-file> -- <command…>")),
        },
        GUARD_SUBCOMMAND => match args {
            [_, parent, dashes, command @ ..] if dashes.as_os_str() == OsStr::new("--") && !command.is_empty() => {
                match parent.to_str().and_then(|p| p.parse().ok()) {
                    Some(parent) => Ok(HelperMode::Guard { parent, command: command.to_vec() }),
                    None => Err(format!("{name}: the parent isn't a process id")),
                }
            }
            _ => Err(format!("usage: {name} <parent-pid> -- <command…>")),
        },
        _ => return None,
    })
}

/// `__appcontainer <spec-file> -- <command…>` (Windows): the command in the AppContainer the spec describes
/// (`marv_sandbox::appcontainer`), exiting with its code. 127 when it can't start, as a missing command would.
fn app_container(spec: &std::path::Path, command: &[OsString]) -> ExitCode {
    let parsed = std::fs::read_to_string(spec)
        .map_err(|e| format!("can't read {}: {e}", spec.display()))
        .and_then(|text| serde_json::from_str::<marv_appcontainer::Spec>(&text).map_err(|e| format!("a damaged spec: {e}")));
    match parsed.and_then(|spec| marv_appcontainer::run(&spec, command)) {
        // Windows exit codes are 32 bits: passed on whole.
        Ok(code) => std::process::exit(code as i32),
        Err(why) => {
            eprintln!("marv {}: {why}", marv_appcontainer::SUBCOMMAND);
            ExitCode::from(127)
        }
    }
}

/// `__mesh-mcp <socket>`: stdin to the socket and the socket to stdout until either side closes. Plain bytes both ways:
/// the desktop's end speaks MCP, and only a socket of the person's own (0600, in Marv's runtime folder) is joined.
fn mesh_mcp(socket: &std::path::Path) -> ExitCode {
    let ours = std::fs::symlink_metadata(socket)
        .is_ok_and(|m| marv_fs::local_socket::is_socket(&m, socket) && marv_fs::mode::uid_of(&m) == marv_fs::mode::own_uid());
    if !ours {
        eprintln!("marv __mesh-mcp: {} isn't a mesh socket of yours", socket.display());
        return ExitCode::from(1);
    }
    let Ok(stream) = marv_fs::local_socket::connect_blocking(socket) else {
        eprintln!("marv __mesh-mcp: can't reach {} (is Marv OS running?)", socket.display());
        return ExitCode::from(1);
    };
    let Ok(mut to) = stream.try_clone() else { return ExitCode::from(1) };
    let up = std::thread::spawn(move || {
        let _ = std::io::copy(&mut std::io::stdin().lock(), &mut to);
        let _ = to.shutdown(std::net::Shutdown::Write);
    });
    let mut from = stream;
    let _ = std::io::copy(&mut from, &mut std::io::stdout().lock());
    drop(up);
    ExitCode::SUCCESS
}

/// `__append-line`'s arguments: the file, then a line or `--echo <text>`, in either order.
fn append_line_args(args: &[OsString]) -> Option<HelperMode> {
    let mut words = args.iter().map(|a| a.to_str());
    let file = words.next()??.to_string();
    let (mut line, mut echo) = (None, None);
    while let Some(word) = words.next() {
        match word? {
            "--echo" => echo = Some(words.next()??.to_string()),
            text if line.is_none() => line = Some(text.to_string()),
            _ => return None,
        }
    }
    Some(HelperMode::AppendLine { file, line, echo })
}

/// `__append-line`: `line` (else stdin), newlines dropped, appended to `file` as one line; then `echo` printed.
fn append_line(file: &std::path::Path, line: Option<String>, echo: Option<&str>) -> ExitCode {
    use std::io::{Read, Write};
    let text = match line {
        Some(line) => line,
        None => {
            let mut text = String::new();
            if std::io::stdin().read_to_string(&mut text).is_err() {
                return ExitCode::from(1);
            }
            text
        }
    };
    let one_line: String = text.chars().filter(|c| *c != '\n' && *c != '\r').collect();
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .and_then(|mut f| f.write_all(format!("{one_line}\n").as_bytes()));
    if let Some(echo) = echo {
        println!("{echo}");
    }
    if written.is_ok() { ExitCode::SUCCESS } else { ExitCode::from(1) }
}

/// For a binary's `main`: runs the helper mode this process was started as, if any, and returns its exit code.
/// None: not a helper; start normally.
pub fn run_from_env() -> Option<ExitCode> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match from_args(&args)? {
        Ok(mode) => Some(mode.run()),
        Err(e) => {
            eprintln!("marv {e}");
            Some(ExitCode::from(127))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<OsString> {
        a.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_what_marv_writes() {
        assert_eq!(from_args(&args(&["__html-convert"])), Some(Ok(HelperMode::HtmlConvert)));
        assert_eq!(from_args(&args(&["__run-checks", "{}"])), Some(Ok(HelperMode::RunChecks { spec: "{}".into() })));
        assert_eq!(from_args(&args(&["__relay", "{\"argv\":[]}"])), Some(Ok(HelperMode::Relay { spec: "{\"argv\":[]}".into() })));
        assert_eq!(
            from_args(&args(&["__mesh-mcp", "/run/x.sock"])),
            Some(Ok(HelperMode::MeshMcp { socket: "/run/x.sock".into() }))
        );
        assert_eq!(
            from_args(&args(&["__append-line", "C:\\e.jsonl", "{}"])),
            Some(Ok(HelperMode::AppendLine { file: "C:\\e.jsonl".into(), line: Some("{}".into()), echo: None }))
        );
        assert_eq!(
            from_args(&args(&["__append-line", "e.jsonl", "--echo", "Marv OS"])),
            Some(Ok(HelperMode::AppendLine { file: "e.jsonl".into(), line: None, echo: Some("Marv OS".into()) }))
        );
        assert!(matches!(from_args(&args(&["__append-line"])), Some(Err(_))));
        assert_eq!(
            from_args(&args(&["__serve", "dist", "4173"])),
            Some(Ok(HelperMode::Serve { folder: "dist".into(), port: 4173 }))
        );
        assert_eq!(
            from_args(&args(&["__guard", "42", "--", "bash", "-c", "--x"])),
            Some(Ok(HelperMode::Guard { parent: 42, command: args(&["bash", "-c", "--x"]) }))
        );
    }

    /// `__append-line` writes one line per event, its newlines dropped, after what's there: Codex's JSON as an
    /// argument, a hook's pretty-printed JSON from stdin (here the argument path, which shares the code).
    #[test]
    fn append_line_adds_one_line_per_event() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("it's events.jsonl");
        assert_eq!(append_line(&file, Some("{\"a\":1}".into()), None), ExitCode::SUCCESS);
        assert_eq!(append_line(&file, Some("{\n  \"b\": 2\r\n}\n".into()), None), ExitCode::SUCCESS);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{\"a\":1}\n{  \"b\": 2}\n");
        assert_eq!(append_line(&dir.path().join("missing/e.jsonl"), Some("x".into()), None), ExitCode::from(1));
    }

    /// The names are the ones the callers run: the relay's sandbox argv, the checks' command, the converter, the guard.
    #[test]
    fn names_match_the_callers() {
        let spec = marv_tools::relay::RelaySpec { argv: vec!["true".into()], ..Default::default() };
        let argv = marv_tools::relay::sandbox_argv(&spec);
        assert_eq!(from_args(&argv[1..]), Some(Ok(HelperMode::Relay { spec: argv[2].to_str().unwrap().into() })));
        let command = crate::checks::kind::runner_command(std::path::Path::new("/m"), &[], 1);
        assert!(command.starts_with(&format!("'/m' {RUNNER_SUBCOMMAND} ")), "{command}");
        let converter = marv_tools::web::convert::Converter::current().unwrap();
        assert_eq!(
            from_args(&args(&converter.args.iter().map(String::as_str).collect::<Vec<_>>())),
            Some(Ok(HelperMode::HtmlConvert))
        );
        assert_eq!(GUARD_SUBCOMMAND, "__guard");
        assert_eq!(marv_appcontainer::SUBCOMMAND, "__appcontainer");
        assert_eq!(
            from_args(&args(&["__appcontainer", "spec.json", "--", "claude", "--resume", "x"])),
            Some(Ok(HelperMode::AppContainer {
                spec: "spec.json".into(),
                command: vec!["claude".into(), "--resume".into(), "x".into()]
            }))
        );
        assert!(matches!(from_args(&args(&["__appcontainer", "spec.json"])), Some(Err(_))));
        assert!(matches!(from_args(&args(&["__appcontainer", "spec.json", "--"])), Some(Err(_))));
    }

    #[test]
    fn anything_else_starts_normally() {
        assert_eq!(from_args(&[]), None);
        assert_eq!(from_args(&args(&["-p", "hi"])), None);
        assert_eq!(from_args(&args(&["__other", "x"])), None);
        assert_eq!(from_args(&args(&["relay", "{}"])), None);
    }

    #[test]
    fn a_malformed_helper_is_refused_not_started() {
        for bad in [
            &["__html-convert", "x"][..],
            &["__relay"],
            &["__relay", "{}", "more"],
            &["__run-checks"],
            &["__serve", "dist"],
            &["__serve", "dist", "x"],
            &["__guard", "42"],
            &["__guard", "42", "--"],
            &["__guard", "42", "bash"],
            &["__guard", "x", "--", "bash"],
        ] {
            assert!(matches!(from_args(&args(bad)), Some(Err(_))), "{bad:?}");
        }
    }
}
