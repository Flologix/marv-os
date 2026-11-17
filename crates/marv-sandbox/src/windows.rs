//! Windows: no sandbox backend yet, and processes ended by tree.
//!
//! Windows has neither bubblewrap's namespaces nor Seatbelt. Until an AppContainer backend exists, [`NoSandbox`]
//! reports itself unavailable and refuses every command it's asked to sandbox, so nothing that expects a sandbox runs
//! without one. (`/sandbox off` still runs commands directly, approved one by one, as on every platform.)
//!
//! There are no process groups or signals either: a command and everything it started end together with
//! `taskkill /T /F`, which walks the process tree from the given pid.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::SandboxError;
use crate::backend::{SandboxBackend, SandboxCommand};
use crate::fs::FsProbe;
use crate::plan::{Bind, Plan};

/// Said when a command needs the sandbox on Windows.
pub const NO_SANDBOX: &str = "Marv's sandbox isn't available on Windows yet. Turn it off with /sandbox off to run \
                              commands directly (each one is asked first).";

#[derive(Debug)]
pub struct NoSandbox;

impl SandboxBackend for NoSandbox {
    fn name(&self) -> &'static str {
        "none (Windows)"
    }

    fn available(&self) -> bool {
        false
    }

    fn command(&self, _plan: &Plan, _inner: &[OsString], _fs: &dyn FsProbe) -> Result<SandboxCommand, SandboxError> {
        Err(SandboxError::Mount(NO_SANDBOX.into()))
    }

    fn guest_path(&self, bind: &Bind) -> PathBuf {
        bind.source.clone()
    }

    fn hides(&self, _dir: &Path, _home: &Path) -> bool {
        false
    }

    fn kill(&self, pid: u32) {
        end_tree(pid);
    }

    fn terminate(&self, pid: u32) {
        end_tree(pid);
    }

    fn after(&self, _command: &SandboxCommand) {}

    fn processes(&self) -> bool {
        false
    }
}

/// Ends `pid` and every process under it (nothing for pid 0, or one that's gone).
pub fn end_tree(pid: u32) {
    taskkill(pid, true);
}

/// Ends `pid` alone.
pub fn end_process(pid: u32) {
    taskkill(pid, false);
}

fn taskkill(pid: u32, tree: bool) {
    if pid == 0 {
        return;
    }
    let mut command = marv_core::process::command("taskkill");
    command.args(["/PID", &pid.to_string(), "/F"]);
    if tree {
        command.arg("/T");
    }
    let _ = command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

/// The image name of the running process `pid` (`marv-desktop.exe`), from `tasklist`; None when there's none.
pub fn image_name(pid: u32) -> Option<String> {
    let out = marv_core::process::command("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    // `"marv-desktop.exe","1234","Console","1","120,000 K"`, or an "INFO: No tasks…" line.
    let text = String::from_utf8_lossy(&out.stdout);
    let mut fields = text.lines().next()?.split("\",\"");
    let name = fields.next()?.trim_start_matches('"').to_string();
    (fields.next()? == pid.to_string()).then_some(name)
}

/// Whether a process `pid` runs.
pub fn alive(pid: u32) -> bool {
    pid != 0 && image_name(pid).is_some()
}

/// Asks `pid` to close (no /F: a window gets WM_CLOSE); a console program may ignore it.
pub fn ask_to_close(pid: u32) {
    if pid != 0 {
        let _ = marv_core::process::command("taskkill")
            .args(["/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}
