//! Windows: commands in an AppContainer (`marv-appcontainer`), switched on with `MARV_APPCONTAINER=1` until it has
//! been tried on a real PC; off, [`crate::windows::NoSandbox`] refuses sandboxed commands as before.
//!
//! [`spec`] turns a [`Plan`] into the container's rules, the same policy bubblewrap mounts and Seatbelt writes:
//!
//! | Plan | Rule for the container |
//! |---|---|
//! | the project | read-write (read-only for Marv's checks and a home-folder project not shown writable) |
//! | read-only extras, toolchains, PATH folders under the home | read and run |
//! | writable extras, binds, the scratch folder, the command's temporary folder | read-write |
//! | protected paths | no writing (lasting) |
//! | `read_only_on_top` (git's write guard) | no writing, this run only (an approved commit writes there) |
//! | Marv's runtime and config folders, the home's private entries, credentials | nothing (lasting) |
//! | in a home-folder project: every hidden entry at the top of the home, and `AppData` | nothing (lasting) |
//!
//! System folders (`C:\Windows`, `Program Files`) are readable by every AppContainer already; nothing else of the
//! person's is, so the home stays hidden without a stand-in. The network is the plan's.
//!
//! Known gaps, to close on a PC: `placeholders` (a path that must not be created) have nothing to put a rule on, so a
//! project without `.git` can still get one; the mesh's named pipes don't let a container connect yet (their access
//! list names the person only), so a sandboxed command has no plug; binds keep their host path (no mounts), so a
//! caller that put a guest path in what runs inside must use [`SandboxBackend::guest_path`].

#[cfg(windows)]
use std::ffi::OsString;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use marv_appcontainer::{Access, Grant, Spec};

use crate::SandboxError;
#[cfg(windows)]
use crate::backend::{SandboxBackend, SandboxCommand};
use crate::fs::FsProbe;
#[cfg(windows)]
use crate::plan::Bind;
use crate::plan::Plan;

/// What each command's private folder is called, in the user's temporary folder: `marv-cmd-<id>`.
#[cfg(windows)]
const TMP_PREFIX: &str = "marv-cmd-";

/// The container `plan` runs in: one per project and mode, named from the project's real path.
pub fn profile(plan: &Plan, fs: &dyn FsProbe) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    plan.real_root(fs).hash(&mut hasher);
    let mode = if plan.project_read_only(fs) { "ro" } else { "rw" };
    format!("marv.p.{:016x}.{mode}", hasher.finish())
}

/// The container's rules for `plan`, with `tmp` as the command's temporary folder (see the module doc's table).
pub fn grants(plan: &Plan, tmp: &Path, fs: &dyn FsProbe) -> Result<Vec<Grant>, SandboxError> {
    let rule = |path: PathBuf, access, lasting| Grant { path, access, lasting };
    let root = plan.real_root(fs);
    let mut out = vec![rule(root.clone(), if plan.project_read_only(fs) { Access::Read } else { Access::ReadWrite }, true)];
    for extra in plan.read_only_extras(fs)? {
        out.push(rule(extra.real, Access::Read, true));
    }
    // Tools: the toolchains under the home, and the PATH folders there (where Claude Code, Node and Git Bash's own
    // installs live on Windows), read and run only.
    let home = plan.real_home(fs);
    let mut tools: Vec<PathBuf> = plan.toolchains(fs);
    tools.extend(std::env::split_paths(&plan.path).filter(|dir| dir.starts_with(&home) && fs.exists(dir)));
    for dir in tools {
        if !dir.starts_with(&root) && !out.iter().any(|g| g.path == dir) {
            out.push(rule(dir, Access::Read, true));
        }
    }
    if let Some(scratch) = &plan.scratch {
        out.push(rule(scratch.clone(), Access::ReadWrite, true));
    }
    out.push(rule(tmp.to_path_buf(), Access::ReadWrite, false));
    for extra in plan.writable_extras(fs)? {
        out.push(rule(extra.real, Access::ReadWrite, false));
    }
    // Binds, and an agent's own settings shown again over the home's masks (`over_masks`: `~/.claude` in a Home chat).
    for bind in plan.binds.iter().chain(&plan.over_masks) {
        out.push(rule(bind.source.clone(), if bind.writable { Access::ReadWrite } else { Access::Read }, false));
    }
    for path in &plan.protected {
        out.push(rule(path.clone(), Access::DenyWrite, true));
    }
    for path in &plan.read_only_on_top {
        out.push(rule(path.clone(), Access::DenyWrite, false));
    }
    // A home-folder project: as bubblewrap hides every hidden entry at the top of the home, and on Windows `AppData`
    // too (browser profiles, logins, other programs' settings).
    let mut home_private: Vec<PathBuf> = Vec::new();
    if plan.home_project(fs)
        && let Ok(entries) = fs.entries(&home)
    {
        for (name, _) in entries {
            let name = name.to_string_lossy();
            if name.starts_with('.') || name.eq_ignore_ascii_case("AppData") {
                home_private.push(home.join(name.as_ref()));
            }
        }
    }
    let shown: Vec<PathBuf> = out.iter().filter(|g| !g.access.is_deny()).map(|g| g.path.clone()).collect();
    let hidden = plan
        .hidden_dirs(fs)
        .into_iter()
        .chain(plan.config_dir_hidden(fs))
        .chain(plan.home_masks(fs))
        .chain(home_private)
        .chain(plan.credentials(&shown, fs));
    // A denial reaches everything below it and beats any grant, so a hidden folder that holds something shown again
    // (an over-mask, a bind) isn't denied whole: the bubblewrap mount order shows it again, and here that's the only way.
    let again: Vec<&Path> = plan.binds.iter().chain(&plan.over_masks).map(|b| b.source.as_path()).collect();
    for path in hidden {
        if again.iter().any(|a| a.starts_with(&path)) {
            continue;
        }
        if !out.iter().any(|g| g.path == path && g.access == Access::DenyAll) {
            out.push(rule(path, Access::DenyAll, true));
        }
    }
    Ok(out)
}

/// The run `plan` describes: its container, rules, network, folder and environment.
pub fn spec(
    plan: &Plan,
    tmp: &Path,
    record_dir: Option<&Path>,
    env: Vec<(String, String)>,
    fs: &dyn FsProbe,
) -> Result<Spec, SandboxError> {
    let profile = profile(plan, fs);
    Ok(Spec {
        record: record_dir.map(|dir| dir.join(format!("{profile}.json"))),
        grants: grants(plan, tmp, fs)?,
        profile,
        network: plan.network,
        cwd: plan.root.clone(),
        env,
    })
}

/// The Windows variables a program needs to start and find its folders, from Marv's own environment, with the
/// temporary folder and the home set to the command's.
pub fn windows_env(tmp: &str, home: &Path) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> =
        ["SystemRoot", "windir", "SystemDrive", "PATHEXT", "ComSpec", "NUMBER_OF_PROCESSORS", "PROCESSOR_ARCHITECTURE"]
            .into_iter()
            .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
            .collect();
    env.extend([("TEMP".into(), tmp.into()), ("TMP".into(), tmp.into()), ("USERPROFILE".into(), home.display().to_string())]);
    env
}

/// Where the containers' records go: `%LOCALAPPDATA%\Marv\appcontainer`.
#[cfg(windows)]
fn record_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Marv").join("appcontainer"))
}

/// A private folder for one command, in the user's temporary folder (`marv-cmd-<pid>-<n>-<nanos>`).
#[cfg(windows)]
fn make_tmp() -> std::io::Result<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    let dir =
        std::env::temp_dir().join(format!("{TMP_PREFIX}{}-{}-{nanos}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    std::fs::create_dir(&dir)?;
    Ok(dir)
}

/// Only a `marv-cmd-*` folder right in the user's temporary folder is ever removed after a command.
#[cfg(windows)]
fn is_private_tmp(path: &Path) -> bool {
    path.parent() == Some(std::env::temp_dir().as_path())
        && path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(TMP_PREFIX))
}

#[cfg(windows)]
#[derive(Debug)]
pub struct AppContainer;

#[cfg(windows)]
impl SandboxBackend for AppContainer {
    fn name(&self) -> &'static str {
        "AppContainer"
    }

    fn available(&self) -> bool {
        marv_appcontainer::enabled() && crate::helper_exe::helper_exe().is_some()
    }

    /// `marv __appcontainer <spec-file> -- inner…`: the helper writes the rules, starts `inner` in the container and
    /// waits. The spec file sits beside the command's temporary folder, not in it, so the command can't change it.
    fn command(&self, plan: &Plan, inner: &[OsString], fs: &dyn FsProbe) -> Result<SandboxCommand, SandboxError> {
        let exe = crate::helper_exe::helper_exe()
            .ok_or_else(|| SandboxError::Mount("Couldn't find Marv's own binary for the sandbox.".into()))?;
        let dir = make_tmp().map_err(|e| SandboxError::Mount(format!("Couldn't make the command's temporary folder: {e}")))?;
        let built = (|| {
            let tmp = dir.join("tmp");
            std::fs::create_dir(&tmp)?;
            let tmp_text = tmp.display().to_string();
            let mut env = plan.env(&tmp_text, self);
            env.extend(windows_env(&tmp_text, &plan.home));
            let spec = spec(plan, &tmp, record_dir().as_deref(), env.clone(), fs)?;
            let file = dir.join("spec.json");
            std::fs::write(&file, serde_json::to_string(&spec).map_err(std::io::Error::other)?)?;
            let argv: Vec<OsString> =
                [exe.into_os_string(), marv_appcontainer::SUBCOMMAND.into(), file.into_os_string(), "--".into()]
                    .into_iter()
                    .chain(inner.iter().cloned())
                    .collect();
            Ok::<_, SandboxError>(SandboxCommand {
                argv,
                env: env.into_iter().map(|(k, v)| (k.into(), v.into())).collect(),
                leftovers: vec![dir.clone()],
            })
        })();
        if built.is_err() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        built
    }

    fn guest_path(&self, bind: &Bind) -> PathBuf {
        bind.source.clone()
    }

    fn hides(&self, _dir: &Path, _home: &Path) -> bool {
        false
    }

    fn kill(&self, pid: u32) {
        crate::windows::end_tree(pid);
    }

    fn terminate(&self, pid: u32) {
        crate::windows::end_tree(pid);
    }

    fn after(&self, command: &SandboxCommand) {
        for dir in command.leftovers.iter().filter(|d| is_private_tmp(d)) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    fn processes(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::EntryKind;
    use crate::plan::Bind;
    use crate::test_fs::Fake;

    fn plan() -> Plan {
        Plan {
            root: "/home/me/proj".into(),
            home: "/home/me".into(),
            path: "/home/me/.local/bin:/usr/bin".into(),
            protected: vec!["/home/me/proj/.git/hooks".into()],
            read_only_on_top: vec!["/home/me/proj/.git/config".into()],
            hidden: vec!["/run/user/1000/marv".into()],
            config_dir: Some("/home/me/.marv".into()),
            scratch: Some("/tmp/scratch".into()),
            ..Plan::default()
        }
    }

    fn fs() -> Fake {
        Fake::new(|p| ["/home/me/.local/bin", "/run/user/1000/marv", "/home/me/.marv", "/home/me/proj", "/home/me"].contains(&p))
    }

    fn find<'a>(grants: &'a [Grant], path: &str) -> Option<&'a Grant> {
        grants.iter().find(|g| g.path == Path::new(path))
    }

    /// The project is the container's to change; tools on the PATH under the home are readable; protected paths and
    /// git's write guard can't be written; Marv's own folders aren't there at all. The guard is for this run only.
    #[test]
    fn a_plan_becomes_the_containers_rules() {
        let fs = fs();
        let grants = grants(&plan(), Path::new("/tmp/marv-cmd-1/tmp"), &fs).unwrap();
        let rule = |p| find(&grants, p).map(|g| (g.access, g.lasting));
        assert_eq!(rule("/home/me/proj"), Some((Access::ReadWrite, true)));
        assert_eq!(rule("/home/me/.local/bin"), Some((Access::Read, true)));
        assert_eq!(rule("/usr/bin"), None, "system folders need no rule");
        assert_eq!(rule("/tmp/scratch"), Some((Access::ReadWrite, true)));
        assert_eq!(rule("/tmp/marv-cmd-1/tmp"), Some((Access::ReadWrite, false)));
        assert_eq!(rule("/home/me/proj/.git/hooks"), Some((Access::DenyWrite, true)));
        assert_eq!(rule("/home/me/proj/.git/config"), Some((Access::DenyWrite, false)));
        assert_eq!(rule("/run/user/1000/marv"), Some((Access::DenyAll, true)));
        assert_eq!(rule("/home/me/.marv"), Some((Access::DenyAll, true)));
        assert!(find(&grants, "/home/me").is_none(), "the rest of the home isn't granted");
    }

    /// Marv's checks run the project read-only, in a container of their own (so its lasting rule isn't the
    /// read-write one); a home-folder project is read-only too, and its private entries are denied.
    #[test]
    fn read_only_runs_get_their_own_container_and_the_homes_secrets_stay_hidden() {
        let fs = fs();
        let checks = Plan { project_read_only: true, ..plan() };
        assert_ne!(profile(&checks, &fs), profile(&plan(), &fs));
        assert!(profile(&plan(), &fs).ends_with(".rw") && profile(&checks, &fs).ends_with(".ro"));
        assert_eq!(find(&grants(&checks, Path::new("/t"), &fs).unwrap(), "/home/me/proj").unwrap().access, Access::Read);

        let home = Plan { root: "/home/me".into(), ..plan() };
        let grants = grants(&home, Path::new("/t"), &fs).unwrap();
        assert_eq!(find(&grants, "/home/me").unwrap().access, Access::Read);
        assert!(grants.iter().any(|g| g.access == Access::DenyAll && g.path.ends_with(".ssh")), "{grants:?}");
    }

    /// A Home chat's agent: the home is writable, the private entries are denied, but an agent's own settings shown
    /// again over the masks are granted, and no denial covers them.
    #[test]
    fn an_agents_own_settings_shown_over_the_masks_are_never_denied() {
        let mut fs = Fake::new(|p| ["/home/me", "/home/me/.config/gh", "/home/me/.config"].contains(&p));
        fs.dirs.insert(
            "/home/me".into(),
            [
                (".config", EntryKind::Dir),
                (".npmrc", EntryKind::File),
                ("AppData", EntryKind::Dir),
                ("Documents", EntryKind::Dir),
            ]
            .into_iter()
            .map(|(n, k)| (n.to_string(), k))
            .collect(),
        );
        let claude = Bind { source: "/home/me/.config/gh".into(), dest: "/home/me/.config/gh".into(), writable: true };
        let plan = Plan { root: "/home/me".into(), home_writable: true, over_masks: vec![claude], ..plan() };
        let grants = grants(&plan, Path::new("/t"), &fs).unwrap();
        assert_eq!(find(&grants, "/home/me").unwrap().access, Access::ReadWrite);
        assert_eq!(find(&grants, "/home/me/.config/gh").unwrap().access, Access::ReadWrite);
        let denied: Vec<_> = grants.iter().filter(|g| g.access == Access::DenyAll).map(|g| g.path.clone()).collect();
        assert!(!denied.iter().any(|d| Path::new("/home/me/.config/gh").starts_with(d)), "{denied:?}");
        assert!(denied.iter().any(|d| d.ends_with(".ssh")), "{denied:?}");
        assert!(
            denied.contains(&PathBuf::from("/home/me/.npmrc")) && denied.contains(&PathBuf::from("/home/me/AppData")),
            "{denied:?}"
        );
        assert!(!denied.contains(&PathBuf::from("/home/me/Documents")), "an ordinary folder of the home stays");
        assert!(!denied.contains(&PathBuf::from("/home/me/.config")), "it holds an agent's settings shown again");
    }

    #[test]
    fn the_spec_names_its_record_after_the_container() {
        let fs = fs();
        let spec = spec(&plan(), Path::new("/t"), Some(Path::new("/data")), vec![("PATH".into(), "x".into())], &fs).unwrap();
        assert_eq!(spec.record, Some(PathBuf::from(format!("/data/{}.json", spec.profile))));
        assert_eq!(spec.cwd, PathBuf::from("/home/me/proj"));
        assert!(!spec.network);
    }
}
