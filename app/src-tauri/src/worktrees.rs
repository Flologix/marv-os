//! A worktree chat's branch, finished by the person (runtime-kernel §14.1): what it has (its commits and changes
//! beside the folder's branch), **Merge** (its uncommitted work committed, the branch merged into the folder's, then
//! the worktree and branch removed) and **Discard** (the worktree and branch removed). A merge that conflicts is undone
//! at once (`git merge --abort`): the folder is as it was and the branch and worktree are kept.
//!
//! Also, as a worktree is made: the folder's untracked `.env` files copied into it (a fresh checkout has none, so the
//! agent's dev server and tests ran without the person's settings).

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::store::ChatWorktree;

/// What a worktree chat's branch has, beside the folder's branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeStatus {
    pub branch: String,
    /// The folder's branch, which Merge merges into (None: the folder isn't on a branch).
    pub into: Option<String>,
    /// Commits on the chat's branch that the folder's branch doesn't have.
    pub commits: usize,
    /// Files changed in the worktree and not committed yet (Merge commits them first).
    pub uncommitted: usize,
    /// Files the merge would change in the folder (committed and not).
    pub files: usize,
    /// Files with changes not committed in the folder itself: Merge waits for them.
    pub folder_changes: usize,
    /// The worktree's folder is gone (removed by hand): only Discard is left.
    pub gone: bool,
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    // `core.longpaths`: Git for Windows refuses a path past 260 characters by default ("Filename too long").
    let out = marv_core::process::command("git")
        .args(["-c", "core.longpaths=true"])
        .args(args)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(marv_core::printable(String::from_utf8_lossy(&out.stderr).trim()).to_string())
    }
}

fn lines(text: &str) -> usize {
    text.lines().filter(|l| !l.trim().is_empty()).count()
}

/// The worktree's own top folder (`dir` in records from before `top`).
fn top(wt: &ChatWorktree) -> &str {
    if wt.top.is_empty() { &wt.dir } else { &wt.top }
}

/// What chat worktree `wt` of the repository at `repo` has, for showing: a git call that fails counts as nothing.
pub fn status(repo: &Path, wt: &ChatWorktree) -> WorktreeStatus {
    status_of(repo, wt, false).unwrap_or_else(|_| WorktreeStatus {
        branch: wt.branch.clone(),
        into: None,
        commits: 0,
        uncommitted: 0,
        files: 0,
        folder_changes: 0,
        gone: false,
    })
}

/// [`status`], as [`merge`] needs it. `strict`: a failing `git status` or `rev-list` is an error, not "clean" and "no
/// commits": a merge on those numbers reported success and removed a worktree whose work it never saw.
fn status_of(repo: &Path, wt: &ChatWorktree, strict: bool) -> Result<WorktreeStatus, String> {
    let into = git(repo, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok().filter(|b| !b.is_empty());
    let gone = !Path::new(top(wt)).is_dir();
    let commits = match into.as_deref() {
        None => 0,
        Some(into) => match git(repo, &["rev-list", "--count", &format!("{into}..{}", wt.branch)]) {
            Ok(n) => n.parse().unwrap_or(0),
            Err(e) if strict => return Err(format!("Couldn't count the commits on {}: {e}", wt.branch)),
            Err(_) => 0,
        },
    };
    let mut uncommitted = 0;
    let mut worktree_status = String::new();
    if !gone {
        match git(Path::new(top(wt)), &["status", "--porcelain"]) {
            Ok(s) => {
                uncommitted = lines(&s);
                worktree_status = s;
            }
            Err(e) if strict => return Err(format!("Couldn't read the worktree's changes: {e}")),
            Err(_) => {}
        }
    }
    let mut changed: std::collections::BTreeSet<String> = into
        .as_deref()
        .and_then(|into| git(repo, &["diff", "--name-only", &format!("{into}...{}", wt.branch)]).ok())
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default();
    changed.extend(worktree_status.lines().filter_map(|l| l.get(3..)).map(str::to_string));
    let folder_changes = match git(repo, &["status", "--porcelain", "--untracked-files=no"]) {
        Ok(s) => lines(&s),
        Err(e) if strict => return Err(format!("Couldn't read the folder's changes: {e}")),
        Err(_) => 0,
    };
    Ok(WorktreeStatus { branch: wt.branch.clone(), into, commits, uncommitted, files: changed.len(), folder_changes, gone })
}

/// Commits what's not committed in the worktree, as the chat's work (an author of Marv's when git has none set).
fn commit_leftovers(wt: &ChatWorktree, title: &str) -> Result<(), String> {
    let dir = Path::new(top(wt));
    let pending = git(dir, &["status", "--porcelain"]).map_err(|e| format!("Couldn't read the worktree's changes: {e}"))?;
    if lines(&pending) == 0 {
        return Ok(());
    }
    git(dir, &["add", "-A"])?;
    let message = format!("Work from the chat \"{}\"", marv_core::printable(title.trim()));
    // No signing: with `commit.gpgsign` on, a commit with no terminal to ask for the passphrase always failed.
    let mut args = vec!["-c", "commit.gpgSign=false"];
    if git(dir, &["config", "user.email"]).is_err() {
        args.extend(["-c", "user.name=Marv", "-c", "user.email=marv@localhost"]);
    }
    args.extend(["commit", "-q", "-m", &message]);
    git(dir, &args).map(|_| ()).map_err(|e| format!("Couldn't commit its changes: {e}"))
}

/// Takes the worktree and its branch away (the worktree's folder, then the branch; git forgets it). Err, saying what is
/// left, when either couldn't go: Merge and Discard must not say they're done while the work is still there.
pub fn remove(repo: &Path, wt: &ChatWorktree) -> Result<(), String> {
    let mut removed = Err(String::new());
    // On Windows a folder something still has open (an editor, a build, the agent's last process) fails the first try.
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        removed = git(repo, &["worktree", "remove", "--force", top(wt)]);
        if removed.is_ok() {
            break;
        }
    }
    let _ = git(repo, &["worktree", "prune"]);
    // Not registered with git any more (removed by hand) is fine as long as the folder is gone.
    if let Err(e) = removed
        && Path::new(top(wt)).exists()
    {
        return Err(format!(
            "The worktree's folder {} couldn't be removed ({e}): close what is using it and try again.",
            top(wt)
        ));
    }
    let branch_ref = format!("refs/heads/{}", wt.branch);
    if let Err(e) = git(repo, &["branch", "-D", &wt.branch])
        && git(repo, &["rev-parse", "--verify", "--quiet", &branch_ref]).is_ok()
    {
        return Err(format!("The branch {} couldn't be removed ({e}).", wt.branch));
    }
    Ok(())
}

/// What deleting a chat did with its worktree.
#[derive(Debug, PartialEq, Eq)]
pub enum Cleanup {
    /// Nothing in it that the folder's branch lacks: the worktree and branch are gone.
    Removed,
    /// Kept, for this reason (work only the worktree has, or git couldn't say): the person's to merge or discard.
    Kept(String),
}

/// A deleted chat's worktree and branch, taken away like **Discard** does, but only when nothing is lost: no commits the
/// folder's branch lacks, no file changed or new in the worktree. Anything else, or any git call that fails, keeps it
/// (the chat's record goes either way, so the note says where the work is).
pub fn remove_if_clean(repo: &Path, wt: &ChatWorktree) -> Cleanup {
    let status = match status_of(repo, wt, true) {
        Ok(s) => s,
        Err(e) => return Cleanup::Kept(format!("Branch {} and its worktree are kept: {e}", wt.branch)),
    };
    let kept = |why: String| {
        Cleanup::Kept(format!(
            "Branch {} is kept ({why}), in {}: merge it, or remove it with git worktree remove and git branch -D.",
            wt.branch,
            top(wt)
        ))
    };
    if status.into.is_none() {
        return kept("the folder isn't on a branch, so what it has beside it can't be told".into());
    }
    match (status.commits, status.uncommitted) {
        (0, 0) => match remove(repo, wt) {
            Ok(()) => Cleanup::Removed,
            Err(e) => Cleanup::Kept(format!("Branch {} is kept: {e}", wt.branch)),
        },
        (c, 0) => kept(format!("{c} commit{} not merged yet", if c == 1 { "" } else { "s" })),
        (0, u) => kept(format!("{u} file{} changed and not committed", if u == 1 { "" } else { "s" })),
        (c, u) => kept(format!(
            "{c} commit{} not merged yet and {u} file{} not committed",
            if c == 1 { "" } else { "s" },
            if u == 1 { "" } else { "s" }
        )),
    }
}

/// **Merge**: worktree `wt`'s work (its uncommitted changes committed first, as chat `title`'s) merged into the folder's
/// branch at `repo`, then the worktree and branch removed. Refused, with nothing changed, while the folder has changes
/// not committed or isn't on a branch; a merge that conflicts is undone, the branch and worktree kept. What it did.
pub fn merge(repo: &Path, wt: &ChatWorktree, title: &str) -> Result<String, String> {
    let before = status_of(repo, wt, true)?;
    let into = before.into.clone().ok_or("The folder isn't on a branch (a detached HEAD): check one out to merge into.")?;
    if before.folder_changes > 0 {
        return Err(format!(
            "The folder has changes not committed in {} file{}: commit or stash them, then merge.",
            before.folder_changes,
            if before.folder_changes == 1 { "" } else { "s" }
        ));
    }
    if !before.gone {
        // The agent may have switched the worktree to another branch (or a detached HEAD): merging `wt.branch` would
        // leave that work behind and then delete the worktree it is in.
        let on = git(Path::new(top(wt)), &["symbolic-ref", "--quiet", "--short", "HEAD"]).unwrap_or_default();
        if on != wt.branch {
            let now = if on.is_empty() { "a detached HEAD".to_string() } else { format!("the branch {on}") };
            return Err(format!(
                "The worktree is on {now}, not {}: nothing was merged or removed. Switch it back to {} (or merge {now} yourself), then merge.",
                wt.branch, wt.branch
            ));
        }
        commit_leftovers(wt, title)?;
    }
    let commits: usize = git(repo, &["rev-list", "--count", &format!("{into}..{}", wt.branch)])
        .map_err(|e| format!("Couldn't count the commits on {}: {e}", wt.branch))?
        .parse()
        .unwrap_or(0);
    if commits > 0 {
        let message = format!("Merge {} (the chat \"{}\")", wt.branch, marv_core::printable(title.trim()));
        // Unsigned, as the leftovers' commit: there is no terminal for a passphrase.
        let mut args = vec!["-c", "commit.gpgSign=false"];
        if git(repo, &["config", "user.email"]).is_err() {
            args.extend(["-c", "user.name=Marv", "-c", "user.email=marv@localhost"]);
        }
        args.extend(["merge", "--no-edit", "--no-gpg-sign", "-m", &message, &wt.branch]);
        if let Err(why) = git(repo, &args) {
            let conflicts = git(repo, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
            let _ = git(repo, &["merge", "--abort"]);
            let files: Vec<&str> = conflicts.lines().take(5).collect();
            return Err(if files.is_empty() {
                format!("Couldn't merge {} into {into} ({why}): nothing changed, the branch is kept.", wt.branch)
            } else {
                format!(
                    "Merging {} into {into} conflicts in {}. Nothing changed: the branch and its worktree are kept (ask the chat to merge {into} into its branch and resolve them).",
                    wt.branch,
                    files.join(", ")
                )
            });
        }
    }
    remove(repo, wt).map_err(|e| format!("The work is merged into {into}, but {e}"))?;
    Ok(match commits {
        0 => format!("{} had nothing new for {into}: its worktree and branch are removed.", wt.branch),
        1 => format!("Merged 1 commit from {} into {into}.", wt.branch),
        n => format!("Merged {n} commits from {} into {into}.", wt.branch),
    })
}

/// The folder's `.env` files git doesn't track (`.env`, `.env.local`…), copied into a new worktree's folder `into`
/// unless it has one already. Tracked ones are checked out there already.
pub fn copy_env_files(folder: &Path, into: &Path) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    let mut copied = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name == ".env" || name.starts_with(".env.")) || !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let tracked = git(folder, &["ls-files", "--error-unmatch", &name]).is_ok();
        let to = into.join(&*name);
        if !tracked && !to.exists() && std::fs::copy(entry.path(), to).is_ok() {
            copied.push(name.to_string());
        }
    }
    exclude_from_git(into, &copied);
}

/// Adds `names` (files in `folder`, inside a worktree) to the repository's `info/exclude`, so the leftovers commit's
/// `git add -A` never takes a copied `.env` (secrets) into history, after which the merge is refused. A linked
/// worktree shares its repository's `info/exclude`, so the pattern is anchored at the folder's place in the repository.
fn exclude_from_git(folder: &Path, names: &[String]) {
    if names.is_empty() {
        return;
    }
    let (Ok(file), Ok(prefix)) =
        (git(folder, &["rev-parse", "--git-path", "info/exclude"]), git(folder, &["rev-parse", "--show-prefix"]))
    else {
        return;
    };
    let file = if Path::new(&file).is_absolute() { PathBuf::from(file) } else { folder.join(file) };
    let have = std::fs::read_to_string(&file).unwrap_or_default();
    let mut add = String::new();
    for name in names {
        let escaped: String = name.chars().flat_map(|c| if "*?[\\".contains(c) { vec!['\\', c] } else { vec![c] }).collect();
        let pattern = format!("/{prefix}{escaped}");
        if !have.lines().any(|l| l == pattern) {
            add.push_str(&pattern);
            add.push('\n');
        }
    }
    if add.is_empty() {
        return;
    }
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let sep = if have.is_empty() || have.ends_with('\n') { "" } else { "\n" };
    let _ = std::fs::write(&file, format!("{have}{sep}{add}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args(["-c", "user.name=T", "-c", "user.email=t@t", "-c", "init.defaultBranch=main"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(ok.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&ok.stderr));
    }

    /// A repository with one commit, and a worktree on its own branch, as a chat's.
    fn repo_and_worktree(name: &str) -> (marv_guards::TempDir, std::path::PathBuf, ChatWorktree) {
        let tmp = marv_guards::TempDir::new(name);
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        run(&repo, &["init", "-q"]);
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        run(&repo, &["add", "."]);
        run(&repo, &["commit", "-qm", "start"]);
        let dir = tmp.path().join("wt");
        run(&repo, &["worktree", "add", "-q", "-b", "marv/task-1a2b", dir.to_str().unwrap(), "HEAD"]);
        let wt = ChatWorktree {
            dir: dir.display().to_string(),
            branch: "marv/task-1a2b".into(),
            base: "abc".into(),
            top: String::new(),
        };
        (tmp, repo, wt)
    }

    /// A deleted chat's worktree goes only when it holds nothing the folder's branch lacks.
    #[test]
    fn a_deleted_chats_worktree_goes_only_when_clean() {
        let (_tmp, repo, wt) = repo_and_worktree("wt-delete");
        let dir = Path::new(&wt.dir);
        std::fs::write(dir.join("b.txt"), "new\n").unwrap();
        assert!(matches!(remove_if_clean(&repo, &wt), Cleanup::Kept(why) if why.contains("1 file")), "an uncommitted file");
        assert!(dir.exists());
        run(dir, &["add", "."]);
        run(dir, &["commit", "-qm", "b"]);
        assert!(matches!(remove_if_clean(&repo, &wt), Cleanup::Kept(why) if why.contains("1 commit not merged")), "a commit");
        assert!(dir.exists() && git(&repo, &["rev-parse", "--verify", "marv/task-1a2b"]).is_ok());
        run(&repo, &["merge", "-q", "marv/task-1a2b"]);
        assert_eq!(remove_if_clean(&repo, &wt), Cleanup::Removed, "merged and clean");
        assert!(!dir.exists(), "its worktree is removed");
        assert!(git(&repo, &["rev-parse", "--verify", "marv/task-1a2b"]).is_err(), "its branch is removed");
    }

    /// A worktree nothing was done in goes with its chat.
    #[test]
    fn an_untouched_worktree_goes_with_its_chat() {
        let (_tmp, repo, wt) = repo_and_worktree("wt-delete-clean");
        assert_eq!(remove_if_clean(&repo, &wt), Cleanup::Removed);
        assert!(!Path::new(&wt.dir).exists());
    }

    /// Merge commits what the chat left uncommitted, merges its branch, and takes the worktree and branch away.
    #[test]
    fn merge_brings_the_chats_work_in_and_cleans_up() {
        let (_tmp, repo, wt) = repo_and_worktree("wt-merge");
        let dir = Path::new(&wt.dir);
        std::fs::write(dir.join("b.txt"), "new\n").unwrap();
        run(dir, &["add", "."]);
        run(dir, &["commit", "-qm", "b"]);
        std::fs::write(dir.join("c.txt"), "left uncommitted\n").unwrap();
        let s = status(&repo, &wt);
        assert_eq!((s.commits, s.uncommitted, s.files, s.into.as_deref()), (1, 1, 2, Some("main")));
        let said = merge(&repo, &wt, "add b and c").unwrap();
        assert!(said.contains("Merged 2 commits"), "{said}");
        assert_eq!(std::fs::read_to_string(repo.join("c.txt")).unwrap(), "left uncommitted\n");
        assert!(!dir.exists(), "its worktree is removed");
        assert!(git(&repo, &["rev-parse", "--verify", "marv/task-1a2b"]).is_err(), "its branch is removed");
    }

    /// A merge that conflicts changes nothing: the folder's branch is as it was, the branch and worktree are kept.
    #[test]
    fn a_conflicting_merge_is_undone() {
        let (_tmp, repo, wt) = repo_and_worktree("wt-conflict");
        let dir = Path::new(&wt.dir);
        std::fs::write(dir.join("a.txt"), "the chat's\n").unwrap();
        run(dir, &["commit", "-qam", "chat"]);
        std::fs::write(repo.join("a.txt"), "the person's\n").unwrap();
        run(&repo, &["commit", "-qam", "person"]);
        let head = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        let why = merge(&repo, &wt, "edit a").unwrap_err();
        assert!(why.contains("conflicts in a.txt"), "{why}");
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]).unwrap(), head);
        assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "the person's\n");
        assert!(git(&repo, &["status", "--porcelain"]).unwrap().is_empty(), "no merge left half done");
        assert!(dir.exists() && git(&repo, &["rev-parse", "--verify", "marv/task-1a2b"]).is_ok());
    }

    /// Merge waits for the person's own uncommitted changes in the folder, and changes nothing meanwhile.
    #[test]
    fn merge_waits_for_the_folders_own_changes() {
        let (_tmp, repo, wt) = repo_and_worktree("wt-dirty");
        std::fs::write(Path::new(&wt.dir).join("b.txt"), "b\n").unwrap();
        std::fs::write(repo.join("a.txt"), "editing\n").unwrap();
        let why = merge(&repo, &wt, "b").unwrap_err();
        assert!(why.contains("1 file"), "{why}");
        assert!(Path::new(&wt.dir).join("b.txt").exists() && git(&repo, &["rev-parse", "--verify", "marv/task-1a2b"]).is_ok());
    }

    /// Discard takes the worktree and branch away, work and all.
    #[test]
    fn discard_removes_the_worktree_and_branch() {
        let (_tmp, repo, wt) = repo_and_worktree("wt-discard");
        std::fs::write(Path::new(&wt.dir).join("b.txt"), "b\n").unwrap();
        remove(&repo, &wt).unwrap();
        assert!(!Path::new(&wt.dir).exists());
        assert!(git(&repo, &["rev-parse", "--verify", "marv/task-1a2b"]).is_err());
        assert!(!repo.join("b.txt").exists());
    }

    /// A new worktree gets the folder's untracked `.env` files; a tracked one is checked out there already.
    #[test]
    fn untracked_env_files_are_copied_into_a_new_worktree() {
        let (_tmp, repo, wt) = repo_and_worktree("wt-env");
        std::fs::write(repo.join(".env"), "KEY=1\n").unwrap();
        std::fs::write(repo.join(".env.local"), "LOCAL=1\n").unwrap();
        std::fs::write(repo.join(".envrc"), "no\n").unwrap();
        copy_env_files(&repo, Path::new(&wt.dir));
        assert_eq!(std::fs::read_to_string(Path::new(&wt.dir).join(".env")).unwrap(), "KEY=1\n");
        assert!(Path::new(&wt.dir).join(".env.local").exists());
        assert!(!Path::new(&wt.dir).join(".envrc").exists());
    }

    /// The copied `.env` never goes into the leftovers commit (or the merge): it stays untracked.
    #[test]
    fn a_copied_env_file_is_never_committed() {
        let (_tmp, repo, wt) = repo_and_worktree("wt-env-secret");
        std::fs::write(repo.join(".env"), "KEY=secret\n").unwrap();
        copy_env_files(&repo, Path::new(&wt.dir));
        std::fs::write(Path::new(&wt.dir).join("b.txt"), "b\n").unwrap();
        merge(&repo, &wt, "b").unwrap();
        assert!(repo.join("b.txt").exists());
        assert!(git(&repo, &["ls-files", ".env"]).unwrap().is_empty(), ".env was committed");
        assert_eq!(std::fs::read_to_string(repo.join(".env")).unwrap(), "KEY=secret\n");
    }

    /// A worktree the agent moved to another branch is not merged or removed: that work would be lost.
    #[test]
    fn a_worktree_on_another_branch_is_refused() {
        let (_tmp, repo, wt) = repo_and_worktree("wt-other-branch");
        let dir = Path::new(&wt.dir);
        run(dir, &["switch", "-qc", "side"]);
        std::fs::write(dir.join("b.txt"), "b\n").unwrap();
        run(dir, &["add", "."]);
        run(dir, &["commit", "-qm", "b"]);
        let why = merge(&repo, &wt, "b").unwrap_err();
        assert!(why.contains("side") && why.contains("nothing was merged"), "{why}");
        assert!(dir.exists() && git(&repo, &["rev-parse", "--verify", "side"]).is_ok());
        assert!(!repo.join("b.txt").exists());
    }

    /// A git call that fails is an error, not "no commits": a missing branch can't be merged as nothing.
    #[test]
    fn a_failing_git_call_is_an_error_not_nothing() {
        let (_tmp, repo, mut wt) = repo_and_worktree("wt-missing-branch");
        wt.branch = "marv/never-made".into();
        assert!(merge(&repo, &wt, "x").is_err());
        assert!(Path::new(&wt.dir).exists());
    }
}
