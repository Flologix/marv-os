//! Starting a program from Marv: [`command`], in place of `Command::new`.
//!
//! On Windows a console program (git, an MCP server, a hook's shell) started by a GUI app gets a console window of its
//! own: Marv OS has none to share, so every `git` a team's start ran flashed up a window titled
//! `C:\Program Files\Git\cmd\git.exe`, one after another (seen 2026-11-13). [`command`] starts it without one
//! (`CREATE_NO_WINDOW`); its output still comes back through its pipes. Elsewhere it's `Command::new`.
//!
//! An agent's terminal isn't started here: it runs in a pseudo-terminal (ConPTY), which never opens a window.
//!
//! **The host's environment, not the AppImage's.** Marv OS's AppImage starts it through linuxdeploy's AppRun, which
//! points Python, Perl, Qt, GTK, GLib and the dynamic linker into the image (`PYTHONHOME=$APPDIR/usr/`,
//! `LD_LIBRARY_PATH=$APPDIR/usr/lib/:…`, …). The app needs them; what it starts doesn't, and broke on them: a
//! person's `python3 hello.py` in an OpenCode chat died with "No module named 'encodings'" (2026-11-22). Every child
//! gets [`vars`] instead ([`command`] applies [`host_env_fixes`]; a site that builds an environment of its own takes
//! [`vars`] or [`host_var`]).

use std::ffi::OsStr;

/// `CREATE_NO_WINDOW`: the process gets no console window (a console program's output goes to its pipes only).
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `std::process::Command::new(program)`, windowless on Windows. For tokio, `tokio::process::Command::from(..)` of it.
pub fn command(program: impl AsRef<OsStr>) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    no_window(&mut command);
    for (name, value) in host_env_fixes() {
        match value {
            Some(value) => command.env(name, value),
            None => command.env_remove(name),
        };
    }
    command
}

/// What an AppImage's AppRun sets outright (its values aren't under `$APPDIR`): gone from a child's environment.
/// `GTK_THEME` and `PYTHONDONTWRITEBYTECODE` replaced any value of the person's, so there's none to give back.
const APPIMAGE_SET: [&str; 6] = ["APPDIR", "APPIMAGE", "ARGV0", "OWD", "GTK_THEME", "PYTHONDONTWRITEBYTECODE"];

/// What AppRun (linuxdeploy's `AppRun.wrapped` and its GTK hook) points into the image: as path lists, only their
/// entries under `$APPDIR` go, so the person's own (`LD_LIBRARY_PATH=$APPDIR/usr/lib/:…:/home/u/lib`) stay. The
/// single-path ones (`PYTHONHOME`, `GTK_PATH`…) are lists of one.
const APPIMAGE_PATHS: [&str; 17] = [
    "PATH",
    "LD_LIBRARY_PATH",
    "XDG_DATA_DIRS",
    "PYTHONHOME",
    "PYTHONPATH",
    "PERLLIB",
    "QT_PLUGIN_PATH",
    "GSETTINGS_SCHEMA_DIR",
    "GST_PLUGIN_SYSTEM_PATH",
    "GST_PLUGIN_SYSTEM_PATH_1_0",
    "GI_TYPELIB_PATH",
    "GIO_MODULE_DIR",
    "GTK_PATH",
    "GTK_DATA_PREFIX",
    "GTK_EXE_PREFIX",
    "GTK_IM_MODULE_FILE",
    "GDK_PIXBUF_MODULE_FILE",
];

/// The changes that give a child the host's environment back when this process runs from an AppImage (`APPDIR` set,
/// read with `get`): each variable to set (`Some`) or remove (`None`). Empty anywhere else.
pub fn appimage_env_fixes(get: impl Fn(&str) -> Option<String>) -> Vec<(&'static str, Option<String>)> {
    let Some(appdir) = get("APPDIR").map(|d| d.trim_end_matches('/').to_string()).filter(|d| d.starts_with('/') && d.len() > 1)
    else {
        return Vec::new();
    };
    let inside = |entry: &str| entry == appdir || entry.starts_with(&format!("{appdir}/"));
    let mut fixes: Vec<(&'static str, Option<String>)> =
        APPIMAGE_SET.iter().filter(|name| get(name).is_some()).map(|name| (*name, None)).collect();
    for name in APPIMAGE_PATHS {
        let Some(value) = get(name) else { continue };
        let kept: Vec<&str> = value.split(':').filter(|e| !e.is_empty() && !inside(e)).collect();
        if kept.is_empty() {
            fixes.push((name, None));
        } else if kept.join(":") != value {
            fixes.push((name, Some(kept.join(":"))));
        }
    }
    fixes
}

/// [`appimage_env_fixes`] of this process's environment, worked out once.
pub fn host_env_fixes() -> &'static [(&'static str, Option<String>)] {
    static FIXES: std::sync::LazyLock<Vec<(&'static str, Option<String>)>> =
        std::sync::LazyLock::new(|| appimage_env_fixes(|name| std::env::var(name).ok()));
    &FIXES
}

/// This process's environment as a child should get it ([`host_env_fixes`] applied): for a site that clears a
/// command's environment and builds its own. Variables that aren't UTF-8 are left out.
pub fn vars() -> Vec<(String, String)> {
    let fixes = host_env_fixes();
    std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .filter_map(|(k, v)| match fixes.iter().find(|(name, _)| *name == k) {
            Some((_, fixed)) => fixed.clone().map(|f| (k, f)),
            None => Some((k, v)),
        })
        .collect()
}

/// Variable `name` as a child should get it ([`vars`]).
pub fn host_var(name: &str) -> Option<String> {
    match host_env_fixes().iter().find(|(n, _)| *n == name) {
        Some((_, fixed)) => fixed.clone(),
        None => std::env::var(name).ok(),
    }
}

/// Said when `git` itself can't be started. Windows has no git of its own, and without Git for Windows a checkpoint
/// said only "program not found" (Rust's text for a program that isn't on the PATH), which didn't name what to
/// install. Marv OS's checkpoints and Git tab and the CLI's checkpoints and worktrees all say this.
pub const GIT_MISSING: &str = if cfg!(windows) {
    "Git isn't installed: install Git for Windows (git-scm.com) for checkpoints, the Git tab and worktrees."
} else {
    "Git isn't installed: install it (git-scm.com) for checkpoints, the Git tab and worktrees."
};

/// The text for a failed start of `git` in `dir` (`e` from `spawn` or `output`): [`GIT_MISSING`] when there's no git,
/// else `e` as it is. A folder that's gone fails the start with the same NotFound, so it counts only when `dir` is
/// there.
pub fn git_start_error(e: &std::io::Error, dir: &std::path::Path) -> String {
    if e.kind() == std::io::ErrorKind::NotFound && dir.is_dir() { GIT_MISSING.to_string() } else { format!("git: {e}") }
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
    std::env::split_paths(&host_var("PATH")?).find_map(|d| program_in(&d, name))
}

/// The program to start for `name` (an MCP server's `command`), looked up on `path` (a `PATH` value, else Marv's own).
/// On Windows a bare `npx` is `npx.cmd`, which `CreateProcess` doesn't find by that name: it is resolved through
/// `PATHEXT` ([`program_in`]), and left as given when nothing is found (the start then says why) or when it names a
/// path or has an extension already. Elsewhere it is `name` unchanged.
pub fn resolve_program(name: &str, path: Option<&OsStr>) -> std::ffi::OsString {
    let own = host_var("PATH").map(std::ffi::OsString::from);
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

    /// An AppImage's environment (rc.3's AppRun, its values as it writes them) given back as the host's: what it added
    /// goes, the person's own entries stay; nothing changes outside an AppImage.
    #[test]
    fn a_child_gets_the_hosts_environment_not_the_appimages() {
        let env: std::collections::HashMap<&str, &str> = [
            ("APPDIR", "/tmp/.mount_x"),
            ("APPIMAGE", "/home/u/Marv.AppImage"),
            ("LD_LIBRARY_PATH", "/tmp/.mount_x/usr/lib/:/tmp/.mount_x/lib64/:/home/u/lib"),
            ("PYTHONHOME", "/tmp/.mount_x/usr/"),
            ("PYTHONPATH", "/tmp/.mount_x/usr/share/pyshared/:"),
            ("PYTHONDONTWRITEBYTECODE", "1"),
            ("XDG_DATA_DIRS", "/tmp/.mount_x/usr/share/:/tmp/.mount_x/usr/share:/usr/share:/usr/local/share:/usr/share"),
            ("PATH", "/tmp/.mount_x/usr/bin/:/tmp/.mount_x/bin/:/home/u/.local/bin:/usr/bin"),
            ("GSETTINGS_SCHEMA_DIR", "/tmp/.mount_x/usr/share/glib-2.0/schemas/:/tmp/.mount_x//usr/share/glib-2.0/schemas"),
            ("GTK_PATH", "/tmp/.mount_x//usr/lib/gtk-3.0"),
            ("GTK_THEME", "Adwaita:dark"),
            ("HOME", "/home/u"),
            ("LANG", "en_US.UTF-8"),
        ]
        .into();
        let fixes = appimage_env_fixes(|k| env.get(k).map(|v| v.to_string()));
        let fixed = |name: &str| fixes.iter().find(|(n, _)| *n == name).map(|(_, v)| v.clone());
        assert_eq!(fixed("LD_LIBRARY_PATH"), Some(Some("/home/u/lib".into())));
        assert_eq!(fixed("PATH"), Some(Some("/home/u/.local/bin:/usr/bin".into())));
        assert_eq!(fixed("XDG_DATA_DIRS"), Some(Some("/usr/share:/usr/local/share:/usr/share".into())));
        for gone in ["PYTHONHOME", "PYTHONPATH", "PYTHONDONTWRITEBYTECODE", "GSETTINGS_SCHEMA_DIR", "GTK_PATH", "GTK_THEME"] {
            assert_eq!(fixed(gone), Some(None), "{gone}");
        }
        assert_eq!(fixed("APPDIR"), Some(None));
        assert_eq!(fixed("APPIMAGE"), Some(None));
        assert_eq!(fixed("HOME"), None);
        assert_eq!(fixed("LANG"), None);
        assert_eq!(fixed("QT_PLUGIN_PATH"), None, "not set: nothing to do");

        // Not an AppImage: nothing changes, even with the same variables.
        assert!(appimage_env_fixes(|k| (k != "APPDIR").then(|| env.get(k).map(|v| v.to_string())).flatten()).is_empty());
        // An APPDIR of "/" would match everything: taken for none.
        assert!(appimage_env_fixes(|k| (k == "APPDIR").then(|| "/".to_string())).is_empty());
        // A folder whose name starts like APPDIR's isn't inside it.
        let near = appimage_env_fixes(|k| match k {
            "APPDIR" => Some("/tmp/.mount_x".into()),
            "LD_LIBRARY_PATH" => Some("/tmp/.mount_xy/lib".into()),
            _ => None,
        });
        assert_eq!(near, vec![("APPDIR", None)]);
    }

    /// No git names Git; a folder that's gone (the same NotFound from the start) doesn't.
    #[test]
    fn a_missing_git_is_told_apart_from_a_missing_folder() {
        let here = std::env::temp_dir();
        let missing = command("marv-test-no-such-git").current_dir(&here).output().unwrap_err();
        assert_eq!(git_start_error(&missing, &here), GIT_MISSING);
        let gone = here.join("marv-test-no-such-folder");
        assert!(git_start_error(&missing, &gone).starts_with("git: "));
    }

    /// End to end: this test run again as if from an AppImage (AppRun's variables in its environment), where a
    /// [`command`] it starts sees no PYTHONHOME and only the person's own library folder.
    #[cfg(unix)]
    #[test]
    fn a_command_started_from_an_appimage_sees_the_hosts_environment() {
        if std::env::var_os("MARV_TEST_APPIMAGE_CHILD").is_some() {
            let out = command("sh").args(["-c", "printf '%s|%s' \"${PYTHONHOME-unset}\" \"$LD_LIBRARY_PATH\""]).output().unwrap();
            print!("<<{}>>", String::from_utf8_lossy(&out.stdout));
            return;
        }
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "process::tests::a_command_started_from_an_appimage_sees_the_hosts_environment", "--nocapture"])
            .env("MARV_TEST_APPIMAGE_CHILD", "1")
            .env("APPDIR", "/tmp/.mount_x")
            .env("PYTHONHOME", "/tmp/.mount_x/usr/")
            .env("LD_LIBRARY_PATH", "/tmp/.mount_x/usr/lib/:/home/u/lib")
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(said.contains("<<unset|/home/u/lib>>"), "{said}");
    }

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
