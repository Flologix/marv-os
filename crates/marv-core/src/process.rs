//! Starting a program from Marv: [`command`], in place of `Command::new`.
//!
//! On Windows a console program (git, an MCP server, a hook's shell) started by a GUI app gets a console window of its
//! own: Marv OS has none to share, so every `git` a team's start ran flashed up a window titled
//! `C:\Program Files\Git\cmd\git.exe`, one after another (seen 2026-11-13). [`command`] starts it without one
//! (`CREATE_NO_WINDOW`); its output still comes back through its pipes. Elsewhere it's `Command::new`.
//!
//! An agent's terminal isn't started here: it runs in a pseudo-terminal (ConPTY), which never opens a window.

use std::ffi::OsStr;

/// `CREATE_NO_WINDOW`: the process gets no console window (a console program's output goes to its pipes only).
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `std::process::Command::new(program)`, windowless on Windows. For tokio, `tokio::process::Command::from(..)` of it.
pub fn command(program: impl AsRef<OsStr>) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    no_window(&mut command);
    command
}

/// Makes `command` windowless on Windows (nothing elsewhere), for a command made some other way.
pub fn no_window(command: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Program `name` in folder `dir`, as the shell finds it: the file itself, or on Windows `name` with an extension it
/// runs (`PATHEXT`: Claude Code's `claude.exe`, an npm launcher's `claude.cmd`). On Windows a bare `claude` is npm's
/// shell script for Git Bash, which Windows can't start, so it doesn't count: agents went unfound there.
pub fn program_in(dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    program_in_with(dir, name, cfg!(windows), std::env::var("PATHEXT").ok().as_deref())
}

/// [`program_in`] with the OS and `PATHEXT` as arguments, so the Windows half is tested everywhere.
pub fn program_in_with(dir: &std::path::Path, name: &str, windows: bool, pathext: Option<&str>) -> Option<std::path::PathBuf> {
    if windows {
        let exts = pathext.unwrap_or(".COM;.EXE;.BAT;.CMD");
        return exts
            .split(';')
            .filter(|e| !e.is_empty())
            .map(|e| dir.join(format!("{name}{}", e.to_ascii_lowercase())))
            .find(|p| p.is_file());
    }
    Some(dir.join(name)).filter(|p| p.is_file())
}

/// Program `name` on the `PATH`.
pub fn on_path(name: &str) -> Option<std::path::PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|d| program_in(&d, name))
}

/// The program to start for `name` (an MCP server's `command`), looked up on `path` (a `PATH` value, else Marv's own).
/// On Windows a bare `npx` is `npx.cmd`, which `CreateProcess` doesn't find by that name: it is resolved through
/// `PATHEXT` ([`program_in`]), and left as given when nothing is found (the start then says why) or when it names a
/// path or has an extension already. Elsewhere it is `name` unchanged.
pub fn resolve_program(name: &str, path: Option<&OsStr>) -> std::ffi::OsString {
    let own = std::env::var_os("PATH");
    resolve_program_with(name, path.or(own.as_deref()), cfg!(windows), std::env::var("PATHEXT").ok().as_deref())
}

/// [`resolve_program`] with the OS and `PATHEXT` as arguments.
pub fn resolve_program_with(name: &str, path: Option<&OsStr>, windows: bool, pathext: Option<&str>) -> std::ffi::OsString {
    let bare = !name.contains(['/', '\\']) && std::path::Path::new(name).extension().is_none();
    if windows
        && bare
        && let Some(path) = path
        && let Some(found) = std::env::split_paths(path).find_map(|d| program_in_with(&d, name, true, pathext))
    {
        return found.into_os_string();
    }
    name.into()
}

/// The `bash` that runs the model's commands on this machine: `bash` elsewhere; on Windows Git for Windows' own
/// (`<Git>\bin\bash.exe`, found from `git.exe` as Claude Code finds it), never `C:\Windows\System32\bash.exe` or
/// WindowsApps' `bash.exe`, which are WSL's launchers. A bare `bash` found those first (Windows looks in System32
/// before the PATH), and every command died with WSL's `RPC_S_SYSTEM_HANDLE_TYPE_MISMATCH`. Found once.
pub fn bash() -> std::path::PathBuf {
    #[cfg(windows)]
    {
        static FOUND: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
        FOUND.get_or_init(git_bash).clone()
    }
    #[cfg(not(windows))]
    std::path::PathBuf::from("bash")
}

/// Said where Windows has no Git Bash: what to install and from where.
pub const GIT_FOR_WINDOWS_NOTE: &str = "Marv runs commands with Git Bash, which comes with Git for Windows and isn't installed here (or isn't on the PATH). Install it from https://git-scm.com/download/win, then start Marv again.";

/// The text for a failed start of [`bash`] (`e` from `spawn`). On Windows without Git for Windows the bare `bash`
/// fallback gives only "program not found" (or starts WSL's launcher), so the note on what to install is added; in any
/// other case `e` as it is.
pub fn bash_start_error(e: &std::io::Error) -> String {
    let missing = cfg!(windows) && !bash().is_absolute();
    bash_start_error_with(e, missing)
}

/// [`bash_start_error`] with "Git Bash isn't there" as an argument, so it is tested everywhere.
pub fn bash_start_error_with(e: &std::io::Error, git_bash_missing: bool) -> String {
    if git_bash_missing && e.kind() == std::io::ErrorKind::NotFound {
        format!("{e}. {GIT_FOR_WINDOWS_NOTE}")
    } else {
        e.to_string()
    }
}

/// Git for Windows' bash: beside the `git.exe` on the PATH (`<Git>\cmd\git.exe` or `<Git>\bin\git.exe` →
/// `<Git>\bin\bash.exe`), else in its usual install folders, else a `bash.exe` on the PATH that isn't WSL's. `bash`
/// when there's none (the start then fails; [`bash_start_error`] says what to install).
#[cfg(windows)]
fn git_bash() -> std::path::PathBuf {
    use std::path::{Path, PathBuf};
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = std::env::split_paths(&path).filter(|d| d.is_absolute()).collect();
    let from_git = dirs.iter().map(|d| d.join("git.exe")).filter(|g| g.is_file()).find_map(|git| {
        let root = git.parent()?.parent()?;
        Some(root.join("bin").join("bash.exe")).filter(|b| b.is_file())
    });
    let installed = || {
        [
            ("ProgramFiles", r"Git\bin\bash.exe"),
            ("ProgramW6432", r"Git\bin\bash.exe"),
            ("LOCALAPPDATA", r"Programs\Git\bin\bash.exe"),
        ]
        .iter()
        .filter_map(|(var, rest)| Some(Path::new(&std::env::var_os(var)?).join(rest)))
        .find(|b| b.is_file())
    };
    let wsl = |p: &Path| {
        let p = p.to_string_lossy().to_lowercase();
        p.contains(r"\windows\system32\") || p.contains(r"\windowsapps\")
    };
    let on_path = || dirs.iter().map(|d| d.join("bash.exe")).find(|b| b.is_file() && !wsl(b));
    from_git.or_else(installed).or_else(on_path).unwrap_or_else(|| PathBuf::from("bash"))
}

/// Windows' own variables every program there needs to start (`SystemRoot` above all: without it git, node and bash
/// itself fail in odd ways), for a command whose environment is otherwise cleared to a few names. None of them holds a
/// secret. Empty elsewhere.
pub const WINDOWS_SYSTEM_ENV: &[&str] = if cfg!(windows) {
    &[
        "SystemRoot",
        "SystemDrive",
        "windir",
        "ComSpec",
        "PATHEXT",
        "TEMP",
        "TMP",
        "OS",
        "NUMBER_OF_PROCESSORS",
        "PROCESSOR_ARCHITECTURE",
        "ProgramData",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
        "CommonProgramFiles",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "USERNAME",
    ]
} else {
    &[]
};

#[cfg(test)]
mod tests {
    use super::*;

    /// Never WSL's launcher: Git for Windows' bash, when Git is installed.
    #[cfg(windows)]
    #[test]
    fn bash_on_windows_is_git_bash_not_wsl() {
        let bash = bash().to_string_lossy().to_lowercase();
        assert!(!bash.contains(r"\windows\system32\") && !bash.contains(r"\windowsapps\"), "{bash}");
        if std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).any(|d| d.join("git.exe").is_file()) {
            assert!(bash.ends_with(r"\bin\bash.exe"), "{bash}");
            let out = command(bash.as_str()).args(["-c", "echo hi"]).output().unwrap();
            assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hi");
        }
    }

    #[test]
    fn a_command_runs_as_command_new_does() {
        let program = if cfg!(windows) { "cmd" } else { "sh" };
        let flag = if cfg!(windows) { "/C" } else { "-c" };
        let out = command(program).args([flag, "echo hi"]).output().unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hi");
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("marv-core-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_bare_program_resolves_through_pathext_on_windows_only() {
        let dir = scratch("resolve");
        std::fs::write(dir.join("npx"), "#!/bin/sh\n").unwrap();
        std::fs::write(dir.join("npx.cmd"), "@echo off\r\n").unwrap();
        std::fs::write(dir.join("node.exe"), "").unwrap();
        let path = std::env::join_paths([std::path::Path::new("/nowhere"), dir.as_path()]).unwrap();
        let path = Some(path.as_os_str());
        // npm's bare `npx` is a shell script Windows can't start: `.cmd` wins, in PATHEXT's order.
        assert_eq!(resolve_program_with("npx", path, true, None), dir.join("npx.cmd").into_os_string());
        assert_eq!(resolve_program_with("node", path, true, Some(".EXE;.CMD")), dir.join("node.exe").into_os_string());
        assert_eq!(resolve_program_with("npx", path, false, None), std::ffi::OsString::from("npx"));
        // Left alone: not found, a path, or one with an extension.
        assert_eq!(resolve_program_with("uvx", path, true, None), std::ffi::OsString::from("uvx"));
        assert_eq!(resolve_program_with("./npx", path, true, None), std::ffi::OsString::from("./npx"));
        assert_eq!(resolve_program_with("npx.cmd", path, true, None), std::ffi::OsString::from("npx.cmd"));
        assert_eq!(resolve_program_with("npx", None, true, None), std::ffi::OsString::from("npx"));
        assert_eq!(program_in_with(&dir, "npx", false, None), Some(dir.join("npx")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_git_bash_says_what_to_install() {
        let gone = std::io::Error::from(std::io::ErrorKind::NotFound);
        let text = bash_start_error_with(&gone, true);
        assert!(text.contains("Git for Windows") && text.contains("https://git-scm.com/download/win"), "{text}");
        assert_eq!(bash_start_error_with(&gone, false), gone.to_string());
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert_eq!(bash_start_error_with(&denied, true), denied.to_string());
    }
}
