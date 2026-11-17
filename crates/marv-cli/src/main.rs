//! The `marv` binary. Ports Marv's `src/cli.tsx` (dispatch), `src/headless.ts` and `bin/marv`.
//!
//! - no arguments, `-c`, `-r`: the TUI ([`tui`]);
//! - `-p <prompt>` / `run <prompt>`: one headless turn ([`headless`]), exit codes 0/1/2/3;
//! - `mesh ls|wire|plug`: the mesh ([`mesh_cmd`]);
//! - `update [--check]`: update this install to the newest release ([`update`]);
//! - `__html-convert` (hidden): the web reader's converter process (`marv_tools::web::html_convert_cli::main`);
//! - `__run-checks <spec>` (hidden): the project checks' runner, inside the sandbox (`marv_session::checks::runner`);
//! - `__relay <spec>` (hidden): the process door's relay, inside a sandbox (`marv_tools::relay`, Marv #51);
//! - `__serve <folder> <port>` (hidden): the process tool's static file server, inside a sandbox (`marv_tools::serve`);
//! - `__guard <parent-pid> -- <command…>` (hidden): runs a sandboxed command in a session of its own and kills its
//!   process group when Marv dies, for a backend without a PID namespace (`marv_tools::guard`, #165).
//!
//! The hidden ones run before the Tokio runtime starts: they're processes of their own. They live in
//! `marv_session::helper_modes`, which Marv desktop's `main` runs too (mk-60m.10).
//!
//! Background processes (Marv #51) never outlive Marv: the TUI and `-p` stop them on the way out (SIGTERM, then
//! SIGKILL), and whatever is left is SIGKILLed when `main` ends, a panic included
//! (`marv_session::processes::kill_running`); if Marv is killed harder, bwrap's `--die-with-parent` takes them.
//!
//! SIGHUP (the terminal closed) and SIGTERM end the TUI and `-p` as `/exit` would, within a deadline ([`signals`],
//! #196); the exit code is then 128 + the signal's number.

mod cli;
mod headless;
mod kernel_cmd;
mod mesh_cmd;
mod signals;
mod team_cmd;
mod tui;
mod update;

use std::io::{Read, Write};
use std::process::ExitCode;

use clap::Parser;
use clap::error::ErrorKind;

use crate::cli::{Cli, Mode};

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            let _ = err.print();
            // Help and version are answers (exit 0); a bad argument is an error (exit 1, as Marv's), never clap's 2,
            // which `marv -p` uses for "stopped".
            return match err.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
                    ExitCode::SUCCESS
                }
                _ => ExitCode::from(1),
            };
        }
    };
    let mut mode = cli.mode();
    // Read before SIGTERM is caught (#196), which a blocking read here would hold off.
    if let Mode::Headless { prompt } = &mut mode
        && prompt == "-"
    {
        let mut text = String::new();
        if std::io::stdin().read_to_string(&mut text).is_err() {
            eprintln!("Couldn't read the prompt from stdin.");
            return ExitCode::from(1);
        }
        *prompt = text;
    }
    // The same helpers, and the same code, as Marv desktop's (`marv_session::helper_modes`).
    if let Mode::Helper(helper) = mode {
        return helper.run();
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("marv: couldn't start: {e}");
            return ExitCode::from(1);
        }
    };
    // Dropped last thing before the runtime, or while a panic unwinds out of it.
    let processes = StopProcesses;
    let (code, signalled) = runtime.block_on(async move {
        // The TUI and -p leave on SIGHUP or SIGTERM as on /exit (#196); the mesh commands keep the default (they end).
        let quit = matches!(mode, Mode::Headless { .. } | Mode::Interactive { .. }).then(signals::Quit::listen);
        let work = run_mode(cli, mode, quit.clone().unwrap_or_default());
        let Some(quit) = quit else { return (work.await, false) };
        tokio::select! {
            code = work => (quit.exit_code().unwrap_or(code), quit.signal().is_some()),
            // The way out took too long: it's dropped, and the runtime's shutdown takes what's left.
            () = quit.expired() => (quit.exit_code().unwrap_or(1), true),
        }
    });
    // An HTML converter still running (a turn ended mid-fetch) would outlive Marv: stop it, as Marv's exit handler did.
    marv_tools::web::convert::kill_running();
    if signalled {
        drop(processes);
        // Dropping the runtime waits for every blocking task: one stuck writing to a terminal that's gone would hold
        // Marv forever. Its tasks are dropped either way (a checkpoint's git with them, its process group killed).
        runtime.shutdown_timeout(signals::RUNTIME_SHUTDOWN);
    }
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

/// Runs a mode that needs the runtime; `quit` is SIGHUP or SIGTERM (#196), for the TUI and `-p`.
async fn run_mode(cli: Cli, mode: Mode, quit: signals::Quit) -> i32 {
    match mode {
        Mode::Headless { prompt } => {
            if cli.role.is_some() {
                eprintln!("--role is for an interactive chat (marv --role <name>), not -p.");
                return 1;
            }
            if prompt.is_empty() {
                eprintln!("No prompt given. Usage: marv -p \"<prompt>\" or marv run <prompt>");
                return 1;
            }
            let options = headless::HeadlessOptions {
                prompt,
                output: cli.output,
                resume_latest: cli.continue_,
                model: cli.model,
                local_model: cli.local_model,
                max_steps: cli.max_steps,
                approve: cli.approve,
                must_ask: headless::MustAsk {
                    tainted: cli.approve_tainted,
                    protected: cli.approve_protected,
                    unsandboxed: cli.dangerously_run_unsandboxed,
                },
                trust_project: cli.trust_project,
                time_limit: cli.time_limit,
            };
            headless::run_headless(options, quit).await as i32
        }
        Mode::MeshLs => mesh_cmd::ls().await,
        Mode::Update { check } => update::run(check).await,
        Mode::MeshWire { spec } => mesh_cmd::wire(&spec).await,
        Mode::MeshPlug { session, name } => mesh_cmd::plug(&session, name.as_deref()).await,
        Mode::MeshAllow { folder, to, lasts, read } => mesh_cmd::allow(&folder, to.as_deref(), lasts.as_deref(), read).await,
        Mode::MeshRevoke { id } => mesh_cmd::revoke(id).await,
        Mode::MeshPolicies => mesh_cmd::policies(),
        Mode::TeamLs => team_cmd::ls(),
        Mode::TeamStart { name } => team_cmd::start(name.as_deref()).await,
        Mode::KernelReplay { mut files } => {
            if files.is_empty() {
                let env: marv_session::config::Env = std::env::vars().collect();
                let config = marv_session::config::default_config_dir(&env, &marv_session::paths::home_dir());
                files.push(kernel_cmd::todays_log(&config));
            }
            kernel_cmd::replay_files(&files)
        }
        Mode::KernelGrants => {
            let env: marv_session::config::Env = std::env::vars().collect();
            let config = marv_session::config::default_config_dir(&env, &marv_session::paths::home_dir());
            kernel_cmd::grants(&config, &marv_session::paths::home_dir())
        }
        Mode::KernelUsage { week } => {
            let env: marv_session::config::Env = std::env::vars().collect();
            let config = marv_session::config::default_config_dir(&env, &marv_session::paths::home_dir());
            kernel_cmd::usage(&config, week)
        }
        Mode::KernelBudget { amount, folder, chat } => {
            let env: marv_session::config::Env = std::env::vars().collect();
            let config = marv_session::config::default_config_dir(&env, &marv_session::paths::home_dir());
            kernel_cmd::budget(&config, &marv_session::paths::home_dir(), amount.as_deref(), folder.as_deref(), chat.as_deref())
                .await
        }
        Mode::KernelPs { watch } => kernel_cmd::ps(watch).await,
        Mode::KernelAbout => {
            let env: marv_session::config::Env = std::env::vars().collect();
            let config = marv_session::config::default_config_dir(&env, &marv_session::paths::home_dir());
            let about = marv_session::live::about(&config, marv_kernel::activity::DEFAULT_CAP);
            kernel_cmd::print_lines(about.lines().iter().map(|line| marv_core::printable(line)))
        }
        Mode::KernelLog { mut files } => {
            if files.is_empty() {
                let env: marv_session::config::Env = std::env::vars().collect();
                let config = marv_session::config::default_config_dir(&env, &marv_session::paths::home_dir());
                files.push(kernel_cmd::todays_log(&config));
            }
            kernel_cmd::log_files(&files)
        }
        // A person is at the TUI to approve each command; this flag would answer for them.
        _ if cli.dangerously_run_unsandboxed => {
            eprintln!("marv: --dangerously-run-unsandboxed works only with -p (marv -p \"<prompt>\").");
            1
        }
        Mode::Interactive { resume, approve, role } => {
            match tui::run(resume, approve, role, cli.team.clone(), cli.model, cli.local_model, quit).await {
                Ok(code) => code,
                Err(e) => {
                    // Not eprintln!, which panics when stderr is a terminal that's gone (#196: it closed).
                    let _ = writeln!(std::io::stderr(), "marv: {}", marv_core::printable(&e.to_string()));
                    1
                }
            }
        }
        Mode::Helper(_) => unreachable!("handled before the runtime starts"),
    }
}

/// SIGKILLs the background processes still running when it's dropped: at the end of `main`, or on a panic.
struct StopProcesses;

impl Drop for StopProcesses {
    fn drop(&mut self) {
        marv_session::processes::kill_running();
        // #278: the llama-servers this Marv started.
        marv_session::local_model::stop_all();
    }
}
