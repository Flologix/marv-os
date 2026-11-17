//! The outer sandbox around a harness process (the harness plan, phase 4): Claude Code runs inside bubblewrap, built
//! by Marv's own [`marv_sandbox::bwrap::sandbox_args`] from a [`Plan`] for it.
//!
//! - **The project is writable, network is on** (it needs its API), and Claude Code's own config is bound back in
//!   writable: its folder (`$CLAUDE_CONFIG_DIR`, else `~/.claude`) and `~/.claude.json`, so its login, settings and
//!   saved conversations work as in a terminal.
//! - **Everything else of the home is a stand-in**, as for Marv's sandboxed commands: toolchains read-only, secrets
//!   (SSH, GPG, cloud logins, browsers, keyrings) hidden, other projects gone. Marv's config folder and runtime folder
//!   are hidden too, so no other chat's mesh plug is reachable; `/run` is empty.
//! - **A team member's role write set is enforced here** (consistency finding H6): the project is mounted read-only
//!   and only the set's prefixes writable on top ([`bound_to_role`]), as Marv's own sandboxed commands see it for a
//!   bounded role. A role that writes nothing sees the whole project read-only.
//! - Unlike a Marv command it may change `.git`, `CLAUDE.md`, `.env` and the rest of Marv's protected paths: a coding
//!   agent commits and edits its own instructions. Only `.marv/` (Marv's roles and config, which other chats obey)
//!   stays read-only.
//! - Its environment is cleared but for Claude Code's own variables ([`passed_env`]): never Marv's (`MARV_*`, the
//!   OpenRouter key).
//! - A chat on the mesh gets its MCP socket and the plain `marv` helper at their own paths under `/run/marv-harness`
//!   ([`GUEST_SOCKET`], [`GUEST_HELPER`]); nothing else of Marv's runtime folder.
//! - `claude` itself: a script (mise's shim and wrapper run `mise`, which writes in the hidden home) is resolved to
//!   the real binary first ([`real_claude`]).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use marv_sandbox::{Bind, CommandMounts, Plan};

/// Where a chat's mesh MCP socket is inside the sandbox.
pub const GUEST_SOCKET: &str = "/run/marv-harness/mcp.sock";
/// Where the `marv` helper that joins it is inside the sandbox.
pub const GUEST_HELPER: &str = "/run/marv-harness/marv";

/// Whether the outer sandbox can run here: Linux with a working bubblewrap, or Windows with AppContainer runs switched
/// on (`marv_appcontainer::enabled`, until tried on a real PC).
pub fn available() -> bool {
    (cfg!(target_os = "linux") && marv_sandbox::bwrap::sandbox_available()) || marv_appcontainer::enabled()
}

/// Whether the outer sandbox shows things at paths of its own (bubblewrap's mounts, `/run/marv-harness/…`). An
/// AppContainer can't mount: a bind is granted where it is, and the agent is told the host path.
pub const MOUNTS: bool = cfg!(not(windows));

/// Where `host`, bound at `guest` in the sandbox, is seen by the agent inside ([`MOUNTS`]).
pub fn guest(guest: &str, host: &Path) -> PathBuf {
    if MOUNTS { PathBuf::from(guest) } else { host.to_path_buf() }
}

/// The `claude` to run inside ([`real_program`]).
pub fn real_claude(bin: &Path, home: &Path) -> PathBuf {
    real_program(bin, home, "claude")
}

/// The program to run inside: `bin` itself unless it's a script that runs `mise` (mise's shim, or a wrapper around
/// `mise x <tool>`), else what mise has installed for `tool` (`mise which`; that may be a script itself, a Node
/// program, which is fine). `bin` when that can't be found.
pub fn real_program(bin: &Path, home: &Path, tool: &str) -> PathBuf {
    if !runs_mise(bin) {
        return bin.to_path_buf();
    }
    let which = marv_core::process::command("mise")
        .args(["which", tool])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
        .filter(|p| p.is_file() && !runs_mise(p));
    which.or_else(|| newest_mise_install(home, tool)).unwrap_or_else(|| bin.to_path_buf())
}

#[cfg(test)]
fn is_script(path: &Path) -> bool {
    std::fs::read(path).is_ok_and(|b| b.starts_with(b"#!"))
}

/// Whether `path` runs `mise` (it would write in the read-only installs folder): mise's shim (a link to `mise`
/// itself), or a script with a `mise` command in it. Not a script that only names a `.mise` folder, like the
/// launcher npm tools get (`node_modules/.bin/opencode`).
fn runs_mise(path: &Path) -> bool {
    if std::fs::canonicalize(path).is_ok_and(|p| p.file_name().is_some_and(|n| n == "mise")) {
        return true;
    }
    // A script starts with `#!`: those two bytes first, so a native agent (Claude Code's, the OpenAI CLI's: hundreds of
    // MB each) isn't read whole to find it isn't one.
    let mut start = [0u8; 2];
    let script =
        std::fs::File::open(path).and_then(|mut f| std::io::Read::read_exact(&mut f, &mut start)).is_ok() && &start == b"#!";
    if !script {
        return false;
    }
    let Ok(b) = std::fs::read(path) else { return false };
    b.starts_with(b"#!")
        && String::from_utf8_lossy(&b).lines().filter(|l| !l.trim_start().starts_with('#')).any(|l| {
            l.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | ';' | '&' | '|' | '"' | '\''))
                .any(|w| w == "mise" || w.ends_with("/mise"))
        })
}

/// `~/.local/share/mise/installs/<tool>/<newest version>/<tool>` (or its `bin/<tool>`).
fn newest_mise_install(home: &Path, tool: &str) -> Option<PathBuf> {
    let dir = home.join(".local/share/mise/installs").join(tool);
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .flat_map(|e| [e.path().join(tool), e.path().join("bin").join(tool)])
        .filter(|p| p.is_file())
        .collect();
    found.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    found.pop()
}

/// Claude Code's own variable prefixes.
pub const CLAUDE_ENV: &[&str] = &["ANTHROPIC_", "CLAUDE_"];

/// Claude Code's own variables, passed into the sandbox; everything else is cleared. Never Marv's.
pub fn passed_env(name: &str) -> bool {
    passed_env_for(name, CLAUDE_ENV)
}

/// An agent's own variables (`prefixes`), and the few every agent needs (proxies, CA files, the user's name), passed
/// into the sandbox. Never Marv's.
pub fn passed_env_for(name: &str, prefixes: &[&str]) -> bool {
    if crate::claude_code::withheld(name) {
        return false;
    }
    prefixes.iter().any(|p| name.starts_with(p))
        || matches!(
            name.to_ascii_uppercase().as_str(),
            "HTTP_PROXY" | "HTTPS_PROXY" | "NO_PROXY" | "ALL_PROXY" | "SSL_CERT_FILE" | "SSL_CERT_DIR" | "NODE_EXTRA_CA_CERTS"
        )
        || matches!(name, "USER" | "LOGNAME" | "TZ" | "COLORTERM")
}

/// Claude Code's config: its folder (`$CLAUDE_CONFIG_DIR`, else `~/.claude`) and, without that variable,
/// `~/.claude.json`: the ones that exist, bound back in writable.
pub fn config_paths(home: &Path, config_dir: Option<&str>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    match config_dir.filter(|d| !d.is_empty()) {
        Some(dir) => out.push(PathBuf::from(dir)),
        None => {
            out.push(home.join(".claude"));
            out.push(home.join(".claude.json"));
        }
    }
    out.into_iter().filter(|p| p.exists()).collect()
}

/// The repository's shared `.git` when `root` is a git worktree (its `.git` is a file): writable too, so a commit in
/// the worktree works.
fn worktree_git(root: &Path) -> Option<PathBuf> {
    if !root.join(".git").is_file() {
        return None;
    }
    let out = marv_core::process::command("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status.success().then(|| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim())).filter(|p| p.is_dir())
}

/// The plan for Claude Code in `root` with `home` the home folder it hides: `claude` the binary to show, `config` its
/// config paths ([`config_paths`]), `mesh` the chat's MCP socket and the helper to show with it.
pub fn plan(home: &Path, root: &Path, claude: &Path, config: &[PathBuf], mesh: Option<(&Path, &Path)>) -> Plan {
    let mut sandbox = marv_sandbox::Sandbox::from_env(true);
    sandbox.home = home.to_path_buf();
    let writable: Vec<PathBuf> = worktree_git(root).into_iter().collect();
    let mut plan = sandbox.plan(root, &CommandMounts { network: true, writable, ..Default::default() });
    plan.protected = [root.join(".marv")].into_iter().filter(|p| p.exists()).collect();
    // No placeholders: they stop a Marv command making a `.git` (or another protected path) where there's none, but a
    // coding agent may (`git init`), and nothing cleans their empty mount points after an agent's bwrap: a project
    // with no repository was left an empty `.git`, which agents took for a repository and lazygit refused.
    plan.placeholders.clear();
    plan.path = sandbox_path(&plan.path, home, &mise_bin_paths(root));
    plan.mesh_socket = None;
    plan.scratch = None;
    plan.binds.push(Bind { source: claude.to_path_buf(), dest: claude.to_path_buf(), writable: false });
    // A Home chat: the home writable to it, its private entries still masked and Marv's own folders still hidden, so
    // its edits work (each one asked: its cards there are yes-or-no, `claude_code::home_card`); its own config is shown
    // again over the masks (`~/.claude` is one).
    let home_chat = marv_sandbox::home_project(root, home);
    plan.home_writable = home_chat;
    for path in config {
        let bind = Bind { source: path.clone(), dest: path.clone(), writable: true };
        if home_chat { plan.over_masks.push(bind) } else { plan.binds.push(bind) }
    }
    if let Some((socket, helper)) = mesh {
        plan.binds.push(Bind { source: socket.to_path_buf(), dest: GUEST_SOCKET.into(), writable: false });
        plan.binds.push(Bind { source: helper.to_path_buf(), dest: GUEST_HELPER.into(), writable: false });
    }
    plan
}

/// H6: `plan` held to a role's write set: the project read-only, each prefix of `set` writable over it. A prefix that
/// doesn't exist yet is made as a folder first (a mount needs it to exist, and the agent's own tools can't make it in
/// a read-only project), but only where what already exists of it is really inside the project: never through a
/// symlink leading out. Nothing is made in a home-folder project.
pub fn bound_to_role(plan: &mut Plan, set: &marv_tools::WriteSet, root: &Path, home: &Path) {
    let create = !marv_sandbox::home_project(root, home);
    if create && let Ok(real_root) = std::fs::canonicalize(root) {
        for prefix in set.prefixes() {
            let path = root.join(prefix);
            let inside = path.ancestors().find(|a| a.exists()).and_then(|a| std::fs::canonicalize(a).ok());
            if !path.exists() && inside.is_some_and(|real| real.starts_with(&real_root)) {
                let _ = std::fs::create_dir_all(&path);
            }
        }
    }
    plan.project_read_only = true;
    plan.writable.extend(set.existing(root, create));
}

/// The `PATH` inside: mise's tool folders (`mise bin-paths`, found outside) first, then the host's without mise's
/// shims. A shim runs `mise`, which inside repairs its symlinks in the read-only installs folder and fails
/// ("Read-only file system"), so the agent, and every `node` or `gh` it runs, would stop.
pub fn sandbox_path(host: &str, home: &Path, tools: &[PathBuf]) -> String {
    let shims = home.join(".local/share/mise/shims");
    let mut out: Vec<String> = tools.iter().map(|p| p.display().to_string()).collect();
    for dir in host.split(':').filter(|d| !d.is_empty() && Path::new(d) != shims) {
        if !out.iter().any(|o| o == dir) {
            out.push(dir.to_string());
        }
    }
    out.join(":")
}

/// mise's tool folders for `root` (`mise bin-paths`, run outside the sandbox, where it may write): none without mise.
fn mise_bin_paths(root: &Path) -> Vec<PathBuf> {
    let Ok(out) = marv_core::process::command("mise")
        .arg("bin-paths")
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout).lines().map(PathBuf::from).filter(|p| p.is_dir()).collect()
}

/// `bwrap <the plan's mounts> --setenv … -- <inner>`: spawn it with only `PATH` (bwrap clears the rest and sets the
/// plan's, plus `env`).
pub fn argv(plan: &Plan, env: &[(String, String)], inner: &[OsString]) -> Result<Vec<OsString>, String> {
    if !MOUNTS {
        return app_container_argv(plan, env, inner);
    }
    let mut argv: Vec<OsString> = vec!["bwrap".into()];
    argv.extend(marv_sandbox::bwrap::sandbox_args(plan, &marv_sandbox::bwrap::RealFs).map_err(|e| e.to_string())?);
    for (key, value) in env {
        argv.extend(["--setenv".into(), key.into(), value.into()]);
    }
    argv.push("--".into());
    argv.extend(inner.iter().cloned());
    Ok(argv)
}

/// Windows: `marv __appcontainer <spec> -- inner…`, the agent in an AppContainer with the plan's rules
/// (`marv_sandbox::appcontainer`). The spec and the agent's temporary folder go in a folder of their own in the user's
/// temporary folder (`marv-agent-*`), left there: an agent's terminal has no end hook to remove it.
fn app_container_argv(plan: &Plan, env: &[(String, String)], inner: &[OsString]) -> Result<Vec<OsString>, String> {
    let exe = marv_session::helper_modes::helper_exe().ok_or("Couldn't find Marv's own binary for the sandbox.")?;
    let dir = std::env::temp_dir().join(format!("marv-agent-{}", uuid::Uuid::new_v4().simple()));
    let tmp = dir.join("tmp");
    std::fs::create_dir_all(&tmp).map_err(|e| format!("Couldn't make the agent's temporary folder: {e}"))?;
    let mut all = env.to_vec();
    all.extend(marv_sandbox::appcontainer::windows_env(&tmp.display().to_string(), &plan.home));
    let records = std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Marv").join("appcontainer"));
    let spec = marv_sandbox::appcontainer::spec(plan, &tmp, records.as_deref(), all, &marv_sandbox::bwrap::RealFs)
        .map_err(|e| e.to_string())?;
    let file = dir.join("spec.json");
    std::fs::write(&file, serde_json::to_string(&spec).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    Ok([exe.into_os_string(), marv_appcontainer::SUBCOMMAND.into(), file.into_os_string(), "--".into()]
        .into_iter()
        .chain(inner.iter().cloned())
        .collect())
}

/// The variables passed in from this process's environment ([`passed_env`]), and `SHELL` for Claude Code's Bash tool.
pub fn env_from_host() -> Vec<(String, String)> {
    env_from_host_for(CLAUDE_ENV)
}

/// [`env_from_host`] for an agent with these variable prefixes.
pub fn env_from_host_for(prefixes: &[&str]) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars().filter(|(k, _)| passed_env_for(k, prefixes)).collect();
    if Path::new("/bin/bash").exists() {
        env.push(("SHELL".into(), "/bin/bash".into()));
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_script_with_a_mise_command_runs_mise() {
        let dir = std::env::temp_dir().join(format!("marv-mise-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let case = |name: &str, body: &str| {
            let p = dir.join(name);
            std::fs::write(&p, body).unwrap();
            runs_mise(&p)
        };
        assert!(case(
            "wrapper",
            "#!/bin/bash\nmise use -g --quiet \"opencode\" || exit 1\nexec mise x opencode -- opencode \"$@\"\n"
        ));
        assert!(case("abs", "#!/bin/sh\nexec /usr/bin/mise x node -- node\n"));
        assert!(!case(
            "npm",
            "#!/bin/sh\n# target=../.mise/@opencode+cli@2.2.3/bin/opencode.exe\nexec \"$(dirname \"$0\")/../.mise/x/opencode.exe\" \"$@\"\n"
        ));
        assert!(!case("plain", "#!/bin/sh\necho hi\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An agent's sandbox leaves a project with no repository as it is: no `.git` placeholder (it may make one itself).
    #[test]
    fn an_agents_plan_has_no_placeholders() {
        let dir = std::env::temp_dir().join(format!("marv-noplace-{}", uuid::Uuid::new_v4()));
        let (home, project) = (dir.join("home"), dir.join("home/Projects/app"));
        std::fs::create_dir_all(&project).unwrap();
        let plan = plan(&home, &project, Path::new("/usr/bin/true"), &[], None);
        assert!(plan.placeholders.is_empty(), "{:?}", plan.placeholders);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_path_inside_has_mises_tools_first_and_no_shims() {
        let home = Path::new("/h");
        let tools = [PathBuf::from("/h/.local/share/mise/installs/node/26/bin")];
        let host = "/h/.local/bin:/h/.local/share/mise/shims:/usr/bin:/h/.local/share/mise/installs/node/26/bin";
        assert_eq!(sandbox_path(host, home, &tools), "/h/.local/share/mise/installs/node/26/bin:/h/.local/bin:/usr/bin");
        assert_eq!(sandbox_path("/usr/bin:/bin", home, &[]), "/usr/bin:/bin");
    }

    #[test]
    fn only_claude_codes_own_variables_go_in() {
        for yes in ["ANTHROPIC_API_KEY", "CLAUDE_CONFIG_DIR", "HTTPS_PROXY", "https_proxy", "NODE_EXTRA_CA_CERTS", "USER"] {
            assert!(passed_env(yes), "{yes}");
        }
        for no in [
            "MARV_CONFIG_DIR",
            "OPENROUTER_API_KEY",
            "AWS_SECRET_ACCESS_KEY",
            "GITHUB_TOKEN",
            "SSH_AUTH_SOCK",
            "HOME",
            "DBUS_SESSION_BUS_ADDRESS",
            // A parent Claude Code session's markers and messaging token (Marv OS started from inside one).
            "CLAUDECODE",
            "CLAUDE_CODE_CHILD_SESSION",
            "CLAUDE_CODE_MESSAGING_TOKEN",
        ] {
            assert!(!passed_env(no), "{no}");
        }
    }

    #[test]
    fn a_mise_wrapper_resolves_to_the_installed_binary_and_a_program_stays() {
        let home = std::env::temp_dir().join(format!("marv-hs-{}", uuid::Uuid::new_v4()));
        let install = home.join(".local/share/mise/installs/claude/9.9.9");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::write(install.join("claude"), b"\x7fELF").unwrap();
        let wrapper = home.join("claude");
        std::fs::write(&wrapper, "#!/bin/bash\nexec mise x claude -- claude \"$@\"\n").unwrap();
        // `mise which` may not know this fake home; the newest install there is found either way.
        let found = real_claude(&wrapper, &home);
        assert!(found.ends_with("claude") && !is_script(&found), "{}", found.display());
        assert_eq!(real_claude(&install.join("claude"), &home), install.join("claude"));
        let _ = std::fs::remove_dir_all(&home);
    }
}

/// Runs the person's real `claude` once inside the sandbox with their real home (`cargo test -- --ignored`): it
/// answers, and its config file is still writable. Uses their login; never run in CI.
#[cfg(test)]
mod live {
    #[test]
    #[ignore]
    fn the_real_claude_answers_inside_the_sandbox() {
        let home = marv_session::paths::home_dir();
        let bin = crate::claude_code::find_claude().expect("claude installed");
        let claude = super::real_claude(&bin, &home);
        let project = std::env::temp_dir().join(format!("marv-live-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&project).unwrap();
        let config = super::config_paths(&home, std::env::var("CLAUDE_CONFIG_DIR").ok().as_deref());
        let before = std::fs::metadata(home.join(".claude.json")).and_then(|m| m.modified()).ok();
        let plan = super::plan(&home, &project, &claude, &config, None);
        let inner: Vec<std::ffi::OsString> =
            [claude.as_os_str(), "-p".as_ref(), "Reply with just the word OK.".as_ref()].map(Into::into).to_vec();
        let argv = super::argv(&plan, &super::env_from_host(), &inner).unwrap();
        let out = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .env_clear()
            .env("PATH", &plan.path)
            .current_dir(&project)
            .output()
            .unwrap();
        let after = std::fs::metadata(home.join(".claude.json")).and_then(|m| m.modified()).ok();
        eprintln!(
            "claude: {}\nstdout: {}\nstderr: {}\n.claude.json modified: {before:?} -> {after:?}",
            claude.display(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success() && String::from_utf8_lossy(&out.stdout).contains("OK"));
        let _ = std::fs::remove_dir_all(&project);
    }

    /// Every tool a coding agent runs (`node`, `gh`, OpenCode itself) starts inside without `mise` (which fails there
    /// on its read-only installs folder), even with mise's shims first on the host's `PATH`.
    #[test]
    #[ignore]
    fn mises_tools_run_inside_without_mise() {
        let home = marv_session::paths::home_dir();
        let project = std::env::temp_dir().join(format!("marv-live-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&project).unwrap();
        let opencode = super::real_program(&home.join(".local/bin/opencode"), &home, "opencode");
        let mut plan = super::plan(&home, &project, &opencode, &[], None);
        let shims = home.join(".local/share/mise/shims").display().to_string();
        plan.path = super::sandbox_path(&format!("{shims}:/usr/bin"), &home, &super::mise_bin_paths(&project));
        assert!(!plan.path.contains("shims"), "{}", plan.path);
        for inner in [
            vec!["node".into(), "--version".into()],
            vec!["gh".into(), "--version".into()],
            vec![opencode.clone().into_os_string(), "--version".into()],
        ] {
            let argv = super::argv(&plan, &super::env_from_host_for(&["OPENCODE"]), &inner).unwrap();
            let out = std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .env_clear()
                .env("PATH", &plan.path)
                .current_dir(&project)
                .output()
                .unwrap();
            let err = String::from_utf8_lossy(&out.stderr);
            eprintln!("{inner:?}: {} {err}", String::from_utf8_lossy(&out.stdout).trim());
            assert!(out.status.success() && !err.contains("mise"), "{inner:?}");
        }
        let _ = std::fs::remove_dir_all(&project);
    }

    /// Live, by hand: Claude Code with the home folder as its project (a Home chat): signed in, the home writable, its
    /// secrets hidden.
    #[test]
    #[ignore]
    fn the_real_claude_in_the_home_folder() {
        let home = marv_session::paths::home_dir();
        let bin = crate::claude_code::find_claude().expect("claude installed");
        let claude = super::real_claude(&bin, &home);
        let config = super::config_paths(&home, None);
        let plan = super::plan(&home, &home, &claude, &config, None);
        let probe = "ls ~/.ssh >/dev/null 2>&1 && echo SSH-SEEN || echo SSH-HIDDEN; touch ~/marv-home-probe 2>/dev/null && echo HOME-WRITABLE || echo HOME-READONLY; ls ~/Downloads >/dev/null 2>&1 && echo DOWNLOADS-SEEN";
        let inner: Vec<std::ffi::OsString> = ["/bin/sh", "-c", probe].map(Into::into).to_vec();
        let argv = super::argv(&plan, &super::env_from_host(), &inner).unwrap();
        let out = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .env_clear()
            .env("PATH", &plan.path)
            .current_dir(&home)
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&out.stdout).to_string();
        let _ = std::fs::remove_file(home.join("marv-home-probe"));
        assert!(said.contains("SSH-HIDDEN") && said.contains("HOME-WRITABLE") && said.contains("DOWNLOADS-SEEN"), "{said}");
        let inner: Vec<std::ffi::OsString> =
            [claude.as_os_str(), "-p".as_ref(), "Reply with just the word OK.".as_ref()].map(Into::into).to_vec();
        let argv = super::argv(&plan, &super::env_from_host(), &inner).unwrap();
        let out = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .env_clear()
            .env("PATH", &plan.path)
            .current_dir(&home)
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(said.contains("OK"), "signed in, it answers: {said} {}", String::from_utf8_lossy(&out.stderr));
    }
}
