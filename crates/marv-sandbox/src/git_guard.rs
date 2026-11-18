//! How a command that runs without asking is kept away from the project's `.git`: read-only if it's there, an empty
//! read-only placeholder if it isn't (so it can't create a repository whose config `git status` would run).
//!
//! Ports `gitGuard`, `worktreeGit` and `worktreeGitDirs` from Marv's `src/tools/bash.ts`.
//!
//! marv-rs: [`git_write_guard`], the mounts for a `git_write` command that runs without asking (bubblewrap): `.git`
//! writable, with what git runs programs from read-only on top.
//!
//! - bwrap can't mount on a symlink, so for a `.git` that is one, what it points to is mounted, when that's inside
//!   the project; outside, the sandbox already shows it read-only (or hides it, in the home folder).
//! - In a git worktree `.git` is a file (`gitdir: <repo>/.git/worktrees/<name>`) and the repository's data lives
//!   outside the project, usually in the hidden home. The worktree's own folder and the common `.git` are shown
//!   read-only, and the `.git` file too, so a command can't repoint it. The project's files say where those are, so
//!   they're only trusted when they check out the way git checks them: the admin folder sits in
//!   `<common>/worktrees/`, and its `gitdir` file points back at this `.git`. Otherwise a planted
//!   `gitdir: /home/me/.ssh` would show that folder to every command that runs without asking.
//! - marv-rs, Linux: a worktree whose records pass that check also gets a [`git_write_guard`]: the repository's `.git`
//!   writable, its program parts and every other worktree's record read-only on top, this worktree's own folder
//!   writable again inside, and its pointers (`commondir`, `gitdir`, `config.worktree`) and the project's `.git` file
//!   read-only (see [`GitWrite`] for the mounts and their order). The lock is on the common `.git`, which is also the
//!   main checkout's `.git`: they never hold a guard at once.

use std::path::{Path, PathBuf};

use crate::paths::{is_inside, normalize};

/// Extra mounts for a confined command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitGuard {
    pub read_only: Vec<PathBuf>,
    pub placeholders: Vec<PathBuf>,
}

/// The guard for a project root.
pub fn git_guard(root: &Path) -> GitGuard {
    let git_dir = root.join(".git");
    let Ok(meta) = std::fs::symlink_metadata(&git_dir) else {
        return GitGuard { placeholders: vec![git_dir], ..Default::default() };
    };
    if meta.file_type().is_symlink() {
        let real_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let target = std::fs::read_link(&git_dir).map(|t| normalize(&root.join(t))).unwrap_or_else(|_| git_dir.clone());
        return match std::fs::canonicalize(&git_dir) {
            Ok(real) if is_inside(&real_root, &real) => GitGuard { read_only: vec![real], ..Default::default() },
            Ok(_) => GitGuard::default(),
            // It leads nowhere yet: inside the project, keep that spot empty too.
            Err(_) if is_inside(&real_root, &target) || is_inside(root, &target) => {
                GitGuard { placeholders: vec![target], ..Default::default() }
            }
            Err(_) => GitGuard::default(),
        };
    }
    if meta.is_file() {
        let mut read_only = vec![git_dir.clone()];
        read_only.extend(worktree_git_dirs(&git_dir));
        return GitGuard { read_only, ..Default::default() };
    }
    GitGuard { read_only: vec![git_dir], ..Default::default() }
}

/// What stays read-only in a `.git` that a command changes without asking (`git_write` with yolo or
/// `approveSandboxed`; bubblewrap only): everything git runs a program from, or follows to find one. `config` (hooksPath,
/// fsmonitor, aliases, filter and diff drivers, sshCommand), `hooks/`, `info/` (attributes pick drivers), `modules/` and
/// `worktrees/` (other repositories' config and hooks), `config.worktree` (read with `extensions.worktreeConfig`) and
/// `commondir` (where git looks for all of those). The rest (objects, refs, the index, HEAD, logs, a merge's or rebase's
/// state) is what commits, branches and merges write.
pub const GIT_PROGRAM_DIRS: [&str; 4] = ["hooks", "info", "modules", "worktrees"];
/// The files among them, with what stands in for one that isn't there (see [`git_write_guard`]). `config` must exist.
pub const GIT_PROGRAM_FILES: [(&str, Option<&str>); 3] =
    [("config", None), ("config.worktree", Some("")), ("commondir", Some(".\n"))];

/// The mounts for a command that may change the repository without asking, while it runs: `.git` writable and itself
/// a mount point (so it can't be renamed away and replaced by one with a planted config), and what
/// [`GIT_PROGRAM_DIRS`] and [`GIT_PROGRAM_FILES`] name read-only on top. A missing folder gets an empty read-only
/// placeholder (removed afterwards, like any placeholder). A missing file is created first with content that changes
/// nothing (an empty `config.worktree`; `commondir` = `.`, the `.git` itself: git dies on an empty one), mounted
/// read-only, and removed when this is dropped: created afterwards instead, git outside the sandbox (an editor's
/// `git status` every few seconds) would follow it while the command runs. Holds an exclusive lock on `.git` until
/// dropped, so two such commands (subagents, another Marv) never share the files: the first one's cleanup would take
/// the second one's mount away.
///
/// marv-rs, Linux: a git worktree (`.git` is a file whose records pass [`worktree_git_dirs`]) gets the same, built
/// from the repository's common `.git` (`<common>`) and the worktree's admin folder (`<common>/worktrees/<name>`),
/// mounted in this order, each on top of the ones before:
/// 1. `<common>` writable ([`GitWrite::writable`]; also what is locked, so two worktrees of one repository and its main
///    checkout never hold a guard at once);
/// 2. read-only on top ([`GitWrite::read_only_on_top`]): the project's own `.git` file, and in `<common>` the same
///    folders and files as above (`hooks`, `info`, `modules`, the whole `worktrees` folder, `config`, `config.worktree`,
///    `commondir`);
/// 3. [`GitWrite::on_top_then`]: the admin folder writable again, on top of the read-only `worktrees` folder, then
///    read-only inside it: its `config.worktree` (a stand-in if missing), `commondir` and `gitdir` (the pointers
///    git follows to find the config and the repository).
///
/// Net effect: objects, refs, packed-refs, logs and this worktree's HEAD, index, logs and merge state are writable;
/// the config, hooks, `info`, `modules`, every other worktree's record and this worktree's pointers are not.
#[derive(Debug)]
pub struct GitWrite {
    pub writable: PathBuf,
    pub read_only_on_top: Vec<PathBuf>,
    /// A worktree's: mounted after `read_only_on_top`, in order (`true`: writable). Empty for a plain repository.
    pub on_top_then: Vec<(PathBuf, bool)>,
    pub placeholders: Vec<PathBuf>,
    created: Vec<(PathBuf, &'static str)>,
    _lock: std::fs::File,
}

/// Why a [`GitWrite`] couldn't be made.
#[derive(Debug, PartialEq, Eq)]
pub enum GitWriteBlocked {
    /// Another such command holds `.git`: wait.
    Busy,
    /// Not a plain repository folder or a worktree whose records check out (no `.git`, a worktree's file that points
    /// elsewhere, a symlink, a symlinked `config`…): the command is asked, as before.
    Unsupported,
}

/// Whether [`git_write_guard`] can work for this project (it may still be [`GitWriteBlocked::Busy`]).
pub fn git_write_supported(root: &Path) -> bool {
    plan_git_write(root).is_some()
}

/// Whether this project's `.git` is a worktree's file: the guard [`git_write_guard`] makes for it is the worktree's.
pub fn git_write_is_worktree(root: &Path) -> bool {
    std::fs::symlink_metadata(root.join(".git")).is_ok_and(|m| m.is_file())
}

/// What [`git_write_guard`] mounts and creates.
struct GitWritePlan {
    /// Writable, and what is locked.
    git: PathBuf,
    /// Read-only on top of `git`.
    read_only: Vec<PathBuf>,
    /// After those (see [`GitWrite::on_top_then`]).
    then: Vec<(PathBuf, bool)>,
    placeholders: Vec<PathBuf>,
    /// Stand-ins to create, and whether each is mounted in `then` (else in `read_only`).
    create: Vec<(PathBuf, &'static str, bool)>,
}

/// What to mount and what to create, without changing anything.
fn plan_git_write(root: &Path) -> Option<GitWritePlan> {
    let git = root.join(".git");
    let meta = std::fs::symlink_metadata(&git).ok()?;
    if meta.is_dir() {
        return plan_git_dir(git, Vec::new());
    }
    #[cfg(target_os = "linux")]
    if meta.is_file() {
        return plan_worktree(&git);
    }
    None
}

/// The parts of a `.git` folder (a plain repository's, or a worktree's common one) that stay read-only.
fn plan_git_dir(git: PathBuf, read_only_first: Vec<PathBuf>) -> Option<GitWritePlan> {
    let (mut read_only, mut placeholders, mut create) = (read_only_first, Vec::new(), Vec::new());
    for name in GIT_PROGRAM_DIRS {
        let path = git.join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => read_only.push(path),
            Ok(_) => return None,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => placeholders.push(path),
            Err(_) => return None,
        }
    }
    for (name, stand_in) in GIT_PROGRAM_FILES {
        let path = git.join(name);
        match (std::fs::symlink_metadata(&path), stand_in) {
            (Ok(meta), _) if meta.is_file() => read_only.push(path),
            (Err(e), Some(content)) if e.kind() == std::io::ErrorKind::NotFound => create.push((path, content, false)),
            _ => return None,
        }
    }
    Some(GitWritePlan { git, read_only, then: Vec::new(), placeholders, create })
}

/// A worktree's: the common `.git` as a plain repository's, with the project's own `.git` file read-only too, then the
/// worktree's admin folder writable again on top of the read-only `worktrees` folder, its pointers read-only on top.
/// Only for records [`worktree_git_dirs`] trusts: the admin folder is `<common>/worktrees/<name>` and points back at
/// this `.git` file.
#[cfg(target_os = "linux")]
fn plan_worktree(git_file: &Path) -> Option<GitWritePlan> {
    let dirs = worktree_git_dirs(git_file);
    let [admin, common] = <[PathBuf; 2]>::try_from(dirs).ok()?;
    if !std::fs::symlink_metadata(&common).ok()?.is_dir() || !std::fs::symlink_metadata(&admin).ok()?.is_dir() {
        return None;
    }
    // `worktrees` must be there to be mounted on (it holds the admin folder), and `.git`'s own file is read-only.
    let mut plan = plan_git_dir(common, vec![git_file.to_path_buf()])?;
    plan.then.push((admin.clone(), true));
    // The pointers inside it. `gitdir` and `commondir` were read to trust it: plain files, not symlinks (bwrap can't
    // mount on one). `config.worktree` is read with `extensions.worktreeConfig`: a stand-in when missing.
    for name in ["config.worktree", "commondir", "gitdir"] {
        let path = admin.join(name);
        match (std::fs::symlink_metadata(&path), name) {
            (Ok(meta), _) if meta.is_file() => plan.then.push((path, false)),
            (Err(e), "config.worktree") if e.kind() == std::io::ErrorKind::NotFound => plan.create.push((path, "", true)),
            _ => return None,
        }
    }
    Some(plan)
}

/// The [`GitWrite`] for this project: locks `.git` (a worktree's: the repository's common one) and creates the
/// stand-ins.
pub fn git_write_guard(root: &Path) -> Result<GitWrite, GitWriteBlocked> {
    let GitWritePlan { git, mut read_only, mut then, placeholders, create } =
        plan_git_write(root).ok_or(GitWriteBlocked::Unsupported)?;
    let lock = std::fs::File::open(&git).map_err(|_| GitWriteBlocked::Unsupported)?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Err(GitWriteBlocked::Busy),
        Err(_) => return Err(GitWriteBlocked::Unsupported),
    }
    let mut guard = GitWrite {
        writable: git,
        read_only_on_top: Vec::new(),
        on_top_then: Vec::new(),
        placeholders,
        created: Vec::new(),
        _lock: lock,
    };
    for (path, content, in_then) in create {
        // Locked, and nothing runs yet: it's still missing (create_new fails rather than write over anything).
        let made = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).and_then(|mut f| {
            use std::io::Write;
            f.write_all(content.as_bytes())
        });
        if made.is_err() {
            return Err(GitWriteBlocked::Unsupported); // Dropping the guard removes what was made so far.
        }
        guard.created.push((path.clone(), content));
        if in_then {
            then.push((path, false));
        } else {
            read_only.push(path);
        }
    }
    guard.read_only_on_top = read_only;
    guard.on_top_then = then;
    Ok(guard)
}

impl Drop for GitWrite {
    /// Removes the stand-ins it made, still as made (they were read-only to the command), then the lock goes.
    fn drop(&mut self) {
        for (path, content) in &self.created {
            if std::fs::read_to_string(path).is_ok_and(|now| now == *content) {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

/// For a project whose `.git` is a worktree's file: its trusted git folders; else none.
pub fn worktree_git(root: &Path) -> Vec<PathBuf> {
    let git_file = root.join(".git");
    match std::fs::symlink_metadata(&git_file) {
        Ok(meta) if meta.is_file() => worktree_git_dirs(&git_file),
        _ => vec![],
    }
}

/// A worktree's own git folder and the repository's common `.git`, when its `.git` file checks out; else none
/// (missing or unreadable records: show nothing more, and git fails, as before).
pub fn worktree_git_dirs(git_file: &Path) -> Vec<PathBuf> {
    let check = || -> Option<Vec<PathBuf>> {
        let text = std::fs::read_to_string(git_file).ok()?;
        let target = text.lines().find_map(|l| l.strip_prefix("gitdir:")).map(str::trim)?;
        let parent = git_file.parent()?;
        let admin = std::fs::canonicalize(parent.join(target)).ok()?;
        // git's own check: the worktree's record points back at this .git file.
        let back = std::fs::read_to_string(admin.join("gitdir")).ok()?;
        if std::fs::canonicalize(admin.join(back.trim())).ok()? != std::fs::canonicalize(git_file).ok()? {
            return None;
        }
        let common_rel = std::fs::read_to_string(admin.join("commondir")).ok()?;
        let common = std::fs::canonicalize(admin.join(common_rel.trim())).ok()?;
        // And it's one of that repository's worktrees.
        if admin.parent()? != common.join("worktrees") {
            return None;
        }
        Some(vec![admin, common])
    };
    check().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_dir::TempDir;
    use std::fs;

    #[test]
    fn no_git_gets_a_placeholder_and_a_git_folder_is_read_only() {
        let root = TempDir::new("guard");
        assert_eq!(git_guard(root.path()).placeholders, vec![root.path().join(".git")]);
        fs::create_dir(root.path().join(".git")).unwrap();
        assert_eq!(git_guard(root.path()).read_only, vec![root.path().join(".git")]);
    }

    #[test]
    fn a_git_write_guard_locks_makes_its_stand_ins_and_removes_them() {
        let root = TempDir::new("guard-write");
        let git = root.path().join(".git");
        fs::create_dir_all(git.join("hooks")).unwrap();
        fs::write(git.join("config"), "[core]\n").unwrap();
        let guard = git_write_guard(root.path()).unwrap();
        assert_eq!(guard.writable, git);
        assert_eq!(guard.placeholders, ["info", "modules", "worktrees"].map(|d| git.join(d)));
        assert_eq!(guard.read_only_on_top, ["hooks", "config", "config.worktree", "commondir"].map(|f| git.join(f)));
        assert_eq!(fs::read_to_string(git.join("commondir")).unwrap(), ".\n");
        assert_eq!(fs::read_to_string(git.join("config.worktree")).unwrap(), "");
        // A second one waits for the first.
        assert_eq!(git_write_guard(root.path()).unwrap_err(), GitWriteBlocked::Busy);
        drop(guard);
        assert!(!git.join("commondir").exists() && !git.join("config.worktree").exists());
        assert_eq!(fs::read_to_string(git.join("config")).unwrap(), "[core]\n");
        drop(git_write_guard(root.path()).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn a_git_write_guard_needs_a_plain_repository() {
        let root = TempDir::new("guard-write-plain");
        assert!(!git_write_supported(root.path()));
        fs::write(root.path().join(".git"), "gitdir: /elsewhere\n").unwrap();
        assert_eq!(git_write_guard(root.path()).unwrap_err(), GitWriteBlocked::Unsupported);
        fs::remove_file(root.path().join(".git")).unwrap();
        fs::create_dir(root.path().join(".git")).unwrap();
        assert!(!git_write_supported(root.path()), "no config");
        fs::write(root.path().join("elsewhere"), "").unwrap();
        std::os::unix::fs::symlink(root.path().join("elsewhere"), root.path().join(".git/config")).unwrap();
        assert!(!git_write_supported(root.path()), "a symlinked config can't be mounted on");
        fs::remove_file(root.path().join(".git/config")).unwrap();
        fs::write(root.path().join(".git/config"), "").unwrap();
        assert!(git_write_supported(root.path()));
    }

    #[test]
    fn a_worktree_record_is_trusted_only_when_it_points_back() {
        let repo = TempDir::new("guard-repo");
        let tree = TempDir::new("guard-tree");
        let admin = repo.path().join(".git/worktrees/w1");
        fs::create_dir_all(&admin).unwrap();
        fs::write(tree.path().join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
        fs::write(admin.join("commondir"), "../..\n").unwrap();
        fs::write(admin.join("gitdir"), format!("{}\n", tree.path().join(".git").display())).unwrap();
        assert_eq!(worktree_git(tree.path()), vec![admin.clone(), repo.path().join(".git")]);
        // A planted gitdir that doesn't point back: nothing.
        fs::write(admin.join("gitdir"), "/somewhere/else/.git\n").unwrap();
        assert!(worktree_git(tree.path()).is_empty());
        assert_eq!(git_guard(tree.path()).read_only, vec![tree.path().join(".git")]);
    }

    /// A repository (`repo`, with `.git/config` and `hooks`) and a worktree of it (`tree`) as git records them.
    #[cfg(target_os = "linux")]
    fn worktree_pair() -> (TempDir, TempDir, PathBuf, PathBuf) {
        let repo = TempDir::new("guard-wt-repo");
        let tree = TempDir::new("guard-wt-tree");
        let common = fs::canonicalize(repo.path()).unwrap().join(".git");
        let admin = common.join("worktrees/w1");
        fs::create_dir_all(&admin).unwrap();
        fs::create_dir_all(common.join("hooks")).unwrap();
        fs::write(common.join("config"), "[core]\n").unwrap();
        fs::write(tree.path().join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
        fs::write(admin.join("commondir"), "../..\n").unwrap();
        fs::write(admin.join("gitdir"), format!("{}\n", tree.path().join(".git").display())).unwrap();
        (repo, tree, common, admin)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_worktree_guard_mounts_the_common_git_writable_with_the_admin_folder_writable_again_on_top() {
        let (_repo, tree, common, admin) = worktree_pair();
        assert!(git_write_supported(tree.path()) && git_write_is_worktree(tree.path()));
        let guard = git_write_guard(tree.path()).unwrap();
        assert_eq!(guard.writable, common);
        // Read-only on top of the common `.git`: the project's `.git` file, then what git runs programs from.
        assert_eq!(
            guard.read_only_on_top,
            [
                tree.path().join(".git"),
                common.join("hooks"),
                common.join("worktrees"),
                common.join("config"),
                common.join("config.worktree"),
                common.join("commondir")
            ]
        );
        assert_eq!(guard.placeholders, ["info", "modules"].map(|d| common.join(d)));
        // Then, in order: the admin folder writable again over the read-only `worktrees`, its pointers read-only over it.
        assert_eq!(
            guard.on_top_then,
            vec![
                (admin.clone(), true),
                (admin.join("commondir"), false),
                (admin.join("gitdir"), false),
                (admin.join("config.worktree"), false),
            ]
        );
        let at = |path: &Path| guard.on_top_then.iter().position(|(p, _)| p == path).unwrap();
        assert!(at(&admin) < at(&admin.join("gitdir")) && at(&admin) < at(&admin.join("config.worktree")));
        assert!(guard.read_only_on_top.contains(&common.join("worktrees")), "the whole folder, before the admin folder");
        // Nothing else in the admin folder is read-only: HEAD, index, logs and merge state are what a commit writes.
        assert!(guard.on_top_then.iter().all(|(p, _)| p == &admin || p.parent() == Some(admin.as_path())));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_worktree_guards_stand_ins_are_removed_on_drop_and_a_changed_one_is_kept() {
        let (_repo, tree, common, admin) = worktree_pair();
        let guard = git_write_guard(tree.path()).unwrap();
        assert_eq!(fs::read_to_string(common.join("commondir")).unwrap(), ".\n");
        assert_eq!(fs::read_to_string(common.join("config.worktree")).unwrap(), "");
        assert_eq!(fs::read_to_string(admin.join("config.worktree")).unwrap(), "");
        drop(guard);
        for gone in [common.join("commondir"), common.join("config.worktree"), admin.join("config.worktree")] {
            assert!(!gone.exists(), "{}", gone.display());
        }
        // The real ones stay as they were.
        assert_eq!(fs::read_to_string(admin.join("commondir")).unwrap(), "../..\n");
        assert_eq!(fs::read_to_string(common.join("config")).unwrap(), "[core]\n");
        // An admin `config.worktree` that exists is mounted as it is, never replaced or removed.
        fs::write(admin.join("config.worktree"), "[x]\n").unwrap();
        let guard = git_write_guard(tree.path()).unwrap();
        assert!(guard.on_top_then.contains(&(admin.join("config.worktree"), false)));
        drop(guard);
        assert_eq!(fs::read_to_string(admin.join("config.worktree")).unwrap(), "[x]\n");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_worktree_guard_and_the_main_checkouts_exclude_each_other() {
        let (repo, tree, common, _admin) = worktree_pair();
        let other = TempDir::new("guard-wt-tree2");
        let admin2 = common.join("worktrees/w2");
        fs::create_dir_all(&admin2).unwrap();
        fs::write(other.path().join(".git"), format!("gitdir: {}\n", admin2.display())).unwrap();
        fs::write(admin2.join("commondir"), "../..\n").unwrap();
        fs::write(admin2.join("gitdir"), format!("{}\n", other.path().join(".git").display())).unwrap();
        let first = git_write_guard(tree.path()).unwrap();
        assert_eq!(git_write_guard(other.path()).unwrap_err(), GitWriteBlocked::Busy, "another worktree of it");
        assert_eq!(git_write_guard(repo.path()).unwrap_err(), GitWriteBlocked::Busy, "the main checkout");
        drop(first);
        let main = git_write_guard(repo.path()).unwrap();
        assert_eq!(main.writable, common);
        assert_eq!(git_write_guard(tree.path()).unwrap_err(), GitWriteBlocked::Busy, "and the other way round");
        drop(main);
        drop(git_write_guard(other.path()).unwrap());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_worktree_record_that_doesnt_check_out_stays_unsupported() {
        let (repo, tree, common, admin) = worktree_pair();
        let git_file = tree.path().join(".git");
        let blocked = |why: &str| {
            assert!(!git_write_supported(tree.path()), "{why}");
            assert_eq!(git_write_guard(tree.path()).unwrap_err(), GitWriteBlocked::Unsupported, "{why}");
            assert!(!common.join("commondir").exists() && !admin.join("config.worktree").exists(), "{why}: nothing made");
        };
        // The `.git` file points somewhere else.
        fs::write(&git_file, "gitdir: /elsewhere\n").unwrap();
        blocked("a gitdir that isn't there");
        let ssh = TempDir::new("guard-wt-ssh");
        fs::write(&git_file, format!("gitdir: {}\n", ssh.path().display())).unwrap();
        blocked("a gitdir that's some other folder");
        // An admin folder outside `<common>/worktrees`, even one that points back.
        let rogue = TempDir::new("guard-wt-rogue");
        fs::write(rogue.path().join("commondir"), format!("{}\n", common.display())).unwrap();
        fs::write(rogue.path().join("gitdir"), format!("{}\n", git_file.display())).unwrap();
        fs::write(&git_file, format!("gitdir: {}\n", rogue.path().display())).unwrap();
        blocked("an admin folder that isn't under common/worktrees");
        // A record that doesn't point back.
        fs::write(&git_file, format!("gitdir: {}\n", admin.display())).unwrap();
        fs::write(admin.join("gitdir"), "/somewhere/else/.git\n").unwrap();
        blocked("a record that points elsewhere");
        fs::write(admin.join("gitdir"), format!("{}\n", git_file.display())).unwrap();
        assert!(git_write_supported(tree.path()), "restored");
        // A symlinked pointer or config can't be mounted on.
        fs::rename(admin.join("gitdir"), admin.join("gitdir.real")).unwrap();
        std::os::unix::fs::symlink(admin.join("gitdir.real"), admin.join("gitdir")).unwrap();
        blocked("a symlinked gitdir");
        fs::remove_file(admin.join("gitdir")).unwrap();
        fs::rename(admin.join("gitdir.real"), admin.join("gitdir")).unwrap();
        fs::rename(common.join("config"), common.join("config.real")).unwrap();
        std::os::unix::fs::symlink(common.join("config.real"), common.join("config")).unwrap();
        blocked("a symlinked common config");
        let _ = repo;
    }
}
