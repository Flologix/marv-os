//! The login shell's PATH and locale for the app (macOS). Started from Finder or the Dock, the app gets launchd's
//! `PATH=/usr/bin:/bin:/usr/sbin:/sbin` and no `LANG`, and every agent terminal inherits that: agents installed with
//! Homebrew or npm aren't found, a `#!/usr/bin/env node` agent dies, the agent's own shell has no brew, node or gh,
//! and non-ASCII input is mangled. So at start Marv asks the person's login shell for its environment and, if that
//! differs, runs itself again with it ([`reexec_with_login_env`]). Setting the variables in place needs `unsafe` in
//! Rust 2024, hence the re-run, the same way `dmabuf` does it on Linux. Elsewhere it does nothing.

/// Set in the re-run, so it isn't asked again.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const MARKER_VAR: &str = "MARV_LOGIN_ENV";
/// Printed by the shell before its environment, so anything an rc file prints first is skipped.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const OUTPUT_MARK: &str = "__MARV_LOGIN_ENV_9f3a1c__";
/// What the app takes from the login shell.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const TAKEN: [&str; 4] = ["PATH", "LANG", "LC_ALL", "LC_CTYPE"];
/// `LANG` when neither the app nor the login shell has one.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const DEFAULT_LANG: &str = "en_US.UTF-8";

/// The variables in `output` (a shell's `printf '%s\n' MARK; env -0`) that the app takes: what follows the mark's
/// line, split at NULs. Empty when the mark isn't there (the shell failed or was cut off).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_login_env(output: &[u8]) -> Vec<(String, String)> {
    let mark = format!("{OUTPUT_MARK}\n");
    let Some(at) = output.windows(mark.len()).position(|w| w == mark.as_bytes()) else { return Vec::new() };
    let mut taken = Vec::new();
    for entry in output[at + mark.len()..].split(|b| *b == 0) {
        let Ok(entry) = std::str::from_utf8(entry) else { continue };
        if let Some((name, value)) = entry.split_once('=')
            && TAKEN.contains(&name)
            && !value.is_empty()
        {
            taken.push((name.to_string(), value.to_string()));
        }
    }
    taken
}

/// The variables the re-run sets: those of `login` that differ from `current`, and `LANG` as `en_US.UTF-8` when it
/// is set in neither. Empty: nothing to change, no re-run.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn changes(current: impl Fn(&str) -> Option<String>, login: &[(String, String)]) -> Vec<(String, String)> {
    let mut set: Vec<(String, String)> =
        login.iter().filter(|(name, value)| current(name).as_deref() != Some(value.as_str())).cloned().collect();
    let has_lang = current("LANG").is_some_and(|v| !v.is_empty()) || login.iter().any(|(n, _)| n == "LANG");
    if !has_lang {
        set.push(("LANG".to_string(), DEFAULT_LANG.to_string()));
    }
    set
}

/// Runs this program again with the login shell's PATH and locale, unless that was done or there's nothing to change.
/// Call it at start, before anything is spawned or any thread started (it replaces the process, so it returns only
/// when it did not). Never fails start-up: any error or a slow shell leaves the environment as it is.
#[cfg(target_os = "macos")]
pub fn reexec_with_login_env() {
    use std::os::unix::process::CommandExt;
    if std::env::var_os(MARKER_VAR).is_some() {
        return;
    }
    let login = login_shell_env().unwrap_or_default();
    let set = changes(|name| std::env::var(name).ok(), &login);
    if set.is_empty() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let mut command = std::process::Command::new(exe);
    command.args(std::env::args_os().skip(1)).env(MARKER_VAR, "1");
    for (name, value) in &set {
        command.env(name, value);
    }
    // Only returns on an error: the app carries on with the environment it has.
    let _ = command.exec();
}

/// Elsewhere the environment is the person's own already.
#[cfg(not(target_os = "macos"))]
pub fn reexec_with_login_env() {}

/// The login shell's environment (`$SHELL`, else zsh, as an interactive login shell so ~/.zprofile and ~/.zshrc both
/// apply), within four seconds; None when it didn't answer.
#[cfg(target_os = "macos")]
fn login_shell_env() -> Option<Vec<(String, String)>> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".to_string());
    let mut child = Command::new(shell)
        .args(["-ilc", &format!("printf '%s\\n' {OUTPUT_MARK}; env -0")])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = stdout.read_to_end(&mut output);
        let _ = send.send(output);
    });
    let output = receive.recv_timeout(Duration::from_secs(4)).ok();
    if output.is_none() {
        // Killing the shell closes the pipe, which ends the reader.
        let _ = child.kill();
    }
    let _ = child.wait();
    Some(parse_login_env(&output?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(text: &str) -> Vec<u8> {
        text.replace('|', "\0").into_bytes()
    }

    #[test]
    fn takes_path_and_locale_after_the_mark() {
        let shell = out(&format!(
            "PATH=/ignored|motd noise\n{OUTPUT_MARK}\nHOME=/Users/a|PATH=/opt/homebrew/bin:/usr/bin|LANG=en_GB.UTF-8|LC_ALL=|EDITOR=vi|"
        ));
        assert_eq!(
            parse_login_env(&shell),
            vec![("PATH".to_string(), "/opt/homebrew/bin:/usr/bin".to_string()), ("LANG".to_string(), "en_GB.UTF-8".to_string())]
        );
    }

    #[test]
    fn no_mark_means_no_environment() {
        assert!(parse_login_env(&out("PATH=/x|")).is_empty());
        assert!(parse_login_env(b"").is_empty());
    }

    #[test]
    fn changes_are_only_what_differs() {
        let current = |name: &str| match name {
            "PATH" => Some("/usr/bin:/bin".to_string()),
            "LANG" => Some("en_US.UTF-8".to_string()),
            _ => None,
        };
        let same = vec![("LANG".to_string(), "en_US.UTF-8".to_string())];
        assert!(changes(current, &same).is_empty());
        let path = vec![("PATH".to_string(), "/opt/homebrew/bin:/usr/bin".to_string())];
        assert_eq!(changes(current, &path), path);
    }

    #[test]
    fn lang_defaults_to_utf8_only_when_nobody_has_one() {
        let bare = |name: &str| (name == "PATH").then(|| "/usr/bin".to_string());
        assert_eq!(changes(bare, &[]), vec![("LANG".to_string(), "en_US.UTF-8".to_string())]);
        let from_shell = vec![("LANG".to_string(), "de_DE.UTF-8".to_string())];
        assert_eq!(changes(bare, &from_shell), from_shell);
    }
}
