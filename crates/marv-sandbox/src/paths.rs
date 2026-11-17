//! Path rules: confining every path a tool is given to its root, after `realpath`.
//!
//! Ports the path half of Marv's `src/tools/files.ts` (`resolveInProject`, `insideGitDir`, `touchesGit`,
//! `refuseGit`, `projectPath`) and fixes the skill-file read (`src/tools/skill.ts`), which checked only the
//! lexical path and so followed a symlink out of the skill's folder (a real vulnerability in Marv 0.3.0).
//!
//! - A leading `~/` is the home folder, as in a shell: taken literally, `~/.config/x` became `<project>/~/.config/x`,
//!   and a model told to create it would make a stray `~` folder. `~src` is not `~user`: it stays literal.
//! - A symlink inside the project can still point outside it. Every symlink on the way is followed
//!   ([`real_with_missing`]): a new file in a symlinked folder lands wherever that folder points, and a write through
//!   a symlink whose target doesn't exist yet creates that target (marv-rs fix, #8: Marv judged such a link by the
//!   nearest existing folder, so a dangling link could write outside the project). A dangling link into `.git` is
//!   refused outright.
//! - `.git` is judged through symlinks too: git runs its hooks and config outside any sandbox, so yolo never changes
//!   it without asking, and the check is repeated at apply time (`refuse_git`) since a parallel command could point a
//!   symlink into it meanwhile.
//! - Names are compared case-insensitively (ASCII) on every OS (marv-rs fix, audit 2026-11): Windows and default macOS
//!   file systems fold case, so `.GIT/hooks/pre-commit` *is* `.git/hooks/pre-commit`, and a case-sensitive match let
//!   a chat with edits pre-approved plant a hook with no card. Stricter on Linux too (a `.GIT` folder there asks
//!   needlessly, which is harmless). On Windows a component is also judged as NTFS reads it: trailing dots and spaces
//!   dropped (`.git.`), an 8.3 short name (`GIT~1`) as the name it stands for, an alternate data stream
//!   (`.git::$INDEX_ALLOCATION`) as its file. [`is_inside`] ignores case on Windows, so `c:\users\me\proj\a.rs` is
//!   inside `C:\Users\Me\proj`. The pure logic takes the OS as a flag so it is tested everywhere.
//! - Known limit (TOCTOU): a path checked here can be swapped for a symlink before it's opened. Planned: open with
//!   `openat2(RESOLVE_BENEATH)` where the kernel has it.

use std::path::{Component, Path, PathBuf};

use crate::SandboxError;

/// Lexical normalisation, like Node's `path.resolve` on an absolute path: `.` dropped, `..` pops (never above `/`).
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => out.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(name) => out.push(name),
        }
    }
    out
}

/// Whether `path` is `root` or inside it, lexically (both absolute and normalised). Ignoring case on Windows.
pub fn is_inside(root: &Path, path: &Path) -> bool {
    is_inside_with(root, path, cfg!(windows))
}

/// [`is_inside`] with the case rule as a flag (ASCII case-insensitive component by component when `case_insensitive`).
pub fn is_inside_with(root: &Path, path: &Path, case_insensitive: bool) -> bool {
    strip_prefix_with(path, root, case_insensitive).is_some()
}

/// `path` below `base` (`Path::strip_prefix`), ignoring case on Windows.
pub fn strip_prefix_ci(path: &Path, base: &Path) -> Option<PathBuf> {
    strip_prefix_with(path, base, cfg!(windows))
}

/// [`strip_prefix_ci`] with the case rule as a flag. The rest keeps its own spelling.
pub fn strip_prefix_with(path: &Path, base: &Path, case_insensitive: bool) -> Option<PathBuf> {
    if !case_insensitive {
        return path.strip_prefix(base).ok().map(Path::to_path_buf);
    }
    let same =
        |a: Component, b: Component| a.as_os_str().to_string_lossy().eq_ignore_ascii_case(&b.as_os_str().to_string_lossy());
    let mut rest = path.components();
    for want in base.components() {
        if !same(rest.next()?, want) {
            return None;
        }
    }
    Some(rest.as_path().to_path_buf())
}

/// A path component as the file system reads it, lower-cased (ASCII), for comparing with a protected name: on Windows
/// also without trailing dots and spaces (`.git.` and `.git ` are `.git`). `windows` says which rules apply.
pub fn fold_name(name: &str, windows: bool) -> String {
    let lower = name.to_ascii_lowercase();
    if windows { lower.trim_end_matches(['.', ' ']).to_string() } else { lower }
}

/// Names whose 8.3 short form (`GIT~1`, `VSCODE~1`, `BUNFIG~1.TOM`) Windows also answers to: the protected names that
/// [`rule_for_relative`](crate::protected::rule_for_relative) knows by name. Lower case.
const SHORT_NAMED: &[&str] =
    &[".git", ".marv", ".vscode", ".idea", ".envrc", ".env", "bunfig.toml", ".mcp.json", "agents.md", "claude.md"];

/// The protected name a folded component stands for as an 8.3 short name (`git~1` → `.git`; `env~1.loc` → `.env`),
/// or `None`. The short name keeps the first six letters of the name without its leading dot, then `~` and a number.
/// Over-matching (a plain file called `environ~1.txt` reads as `.env*`) only asks more, which is the safe side.
pub fn long_name_of_short(folded: &str) -> Option<&'static str> {
    let base = folded.split('.').next()?;
    let (prefix, digits) = base.rsplit_once('~')?;
    if prefix.is_empty() || digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let stem_of = |name: &str| name.trim_start_matches('.').split('.').next().unwrap_or("").chars().take(6).collect::<String>();
    SHORT_NAMED.iter().find(|name| stem_of(name) == prefix).copied().or_else(|| prefix.starts_with("env").then_some(".env"))
}

/// Whether one path component names `.git`: any case, and on Windows also `.git.`, `GIT~1` and `.git::$INDEX_ALLOCATION`
/// (a stream of the folder is the folder).
pub fn is_git_name(component: &str, windows: bool) -> bool {
    let base = if windows { component.split(':').next().unwrap_or("") } else { component };
    let folded = fold_name(base, windows);
    folded == ".git" || (windows && long_name_of_short(&folded) == Some(".git"))
}

/// Where a path really leads, for one that may not exist yet: every symlink on the way followed, *including one whose
/// target doesn't exist yet* (a write through it creates that target), and the missing rest as written.
///
/// marv-rs fix (found by the guard suite, #8): this used to canonicalize the nearest ancestor that exists, and
/// `exists()` follows symlinks, so a dangling `out.sh -> /elsewhere/planted.sh` read as a new file in the project and
/// `write_file` created `/elsewhere/planted.sh`. Likewise `innocent.sh -> .envrc` wasn't seen as protected.
pub fn real_with_missing(path: &Path) -> std::io::Result<PathBuf> {
    let absolute = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir()?.join(path) };
    // Components still to walk, last first; `out` has no symlinks in it, so `..` can pop it.
    let mut pending: Vec<std::ffi::OsString> = Vec::new();
    push_components(&mut pending, &absolute);
    let mut out = anchor(&absolute);
    let mut hops = 0;
    while let Some(name) = pending.pop() {
        if name == ".." {
            out.pop();
            continue;
        }
        let next = out.join(&name);
        match std::fs::symlink_metadata(&next) {
            Ok(meta) if meta.file_type().is_symlink() => {
                hops += 1;
                if hops > 40 {
                    return Err(std::io::Error::other(format!("{}: too many levels of symbolic links", path.display())));
                }
                let target = std::fs::read_link(&next)?;
                if target.is_absolute() {
                    out = anchor(&target);
                }
                push_components(&mut pending, &target);
            }
            Ok(_) => out = next,
            // Missing: the rest is taken as written (nothing below it can be a symlink yet).
            Err(e) if e.kind() == std::io::ErrorKind::NotFound || e.kind() == std::io::ErrorKind::NotADirectory => out = next,
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

/// Where an absolute path's walk starts: its root as `std::fs::canonicalize` writes it, so [`real_with_missing`]'s
/// answer compares with a canonicalized root (every caller's other side). `/` on Linux and macOS. On Windows the drive
/// (`C:\` → `\\?\C:\`): the walk started at `/` and dropped it, so `C:\Users\me\app\x` came out as `\Users\me\app\x`,
/// never inside the canonical `\\?\C:\Users\me\app`, and every one of Marv's file tools said "outside the project".
fn anchor(path: &Path) -> PathBuf {
    let root: PathBuf = path.components().take_while(|c| matches!(c, Component::Prefix(_) | Component::RootDir)).collect();
    if root.as_os_str().is_empty() {
        return PathBuf::from("/");
    }
    std::fs::canonicalize(&root).unwrap_or(root)
}

/// Pushes `path`'s normal and `..` components onto `pending` so they pop first, in order.
fn push_components(pending: &mut Vec<std::ffi::OsString>, path: &Path) {
    let names: Vec<std::ffi::OsString> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some(name.to_os_string()),
            Component::ParentDir => Some("..".into()),
            _ => None,
        })
        .collect();
    pending.extend(names.into_iter().rev());
}

/// Whether the last component of `path` is a symlink whose target doesn't exist (yet).
pub fn is_dangling_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) && !path.exists()
}

/// `~` and `~/x` expanded to the home folder; anything else as written.
pub fn expand_home(path: &str, home: &Path) -> PathBuf {
    if path == "~" {
        home.to_path_buf()
    } else if let Some(rest) = path.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(path)
    }
}

/// Resolves a path the model gave (relative to the project, absolute, or starting with `~/`) and refuses anything
/// outside the project, including via symlinks. Returns the absolute (lexical) path.
pub fn resolve_in_project(root: &Path, path: &str, home: &Path) -> Result<PathBuf, SandboxError> {
    let expanded = expand_home(path, home);
    let absolute = if expanded.is_absolute() { normalize(&expanded) } else { normalize(&root.join(expanded)) };
    let outside = || SandboxError::OutsideProject { path: path.to_string(), root: root.display().to_string() };
    if !is_inside(root, &absolute) {
        return Err(outside());
    }
    let real_root = std::fs::canonicalize(root)?;
    let real = real_with_missing(&absolute)?;
    if !is_inside(&real_root, &real) {
        return Err(outside());
    }
    // A dangling link into .git: writing through it would create a hook or config git runs outside the sandbox, and
    // nothing about the link's own name says .git (a link that resolves goes through `touches_git` instead).
    if is_dangling_symlink(&absolute)
        && strip_prefix_ci(&real, &real_root)
            .as_deref()
            .and_then(|rel| rel.components().next())
            .is_some_and(|c| is_git_name(&c.as_os_str().to_string_lossy(), cfg!(windows)))
    {
        return Err(SandboxError::IntoGit(path.to_string()));
    }
    Ok(absolute)
}

/// A file inside `dir` (a skill's folder), resolved through symlinks: refused when it is `dir` itself, climbs out
/// lexically, or *leads* out through a symlink. Returns the real path, which is what should be opened.
///
/// marv-rs fix (docs/SECURITY.md, "every path is checked after realpath"): Marv's `skill` tool compared only
/// `resolve(dir, file)` with the folder, so `ln -s ~/.ssh/id_ed25519 .marv/skills/x/key` let a model read the key,
/// outside the sandbox.
pub fn resolve_within(dir: &Path, file: &str) -> Result<PathBuf, SandboxError> {
    let outside = || SandboxError::OutsideDir { path: file.to_string(), within: dir.display().to_string() };
    let dir = normalize(dir);
    let lexical = normalize(&dir.join(file));
    if !is_inside(&dir, &lexical) || lexical == dir {
        return Err(outside());
    }
    let real_dir = std::fs::canonicalize(&dir)?;
    let real = match std::fs::canonicalize(&lexical) {
        Ok(real) => real,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Err(SandboxError::Missing(file.to_string())),
        Err(err) => return Err(err.into()),
    };
    if !is_inside(&real_dir, &real) || real == real_dir {
        return Err(outside());
    }
    Ok(real)
}

/// Whether an absolute project path is (or is inside) the project's `.git`, also through a symlink. A path that
/// doesn't exist yet is judged by the nearest folder that does.
pub fn inside_git_dir(root: &Path, absolute: &Path) -> std::io::Result<bool> {
    let real = real_with_missing(absolute)?;
    let real_root = std::fs::canonicalize(root)?;
    Ok(strip_prefix_ci(&real, &real_root)
        .as_deref()
        .and_then(|rel| rel.components().next())
        .is_some_and(|first| is_git_name(&first.as_os_str().to_string_lossy(), cfg!(windows))))
}

/// For `auto_safe`: whether a path the model gave leads into `.git` (a path outside the project counts too: it
/// asks, and fails).
pub fn touches_git(root: &Path, path: &str, home: &Path) -> bool {
    match resolve_in_project(root, path, home) {
        Ok(absolute) => inside_git_dir(root, &absolute).unwrap_or(true),
        Err(_) => true,
    }
}

/// Checked again when a change that runs without asking is applied: a command running meanwhile (a parallel
/// subagent) could have pointed a symlink into `.git`.
pub fn refuse_git(root: &Path, absolute: &Path) -> Result<(), SandboxError> {
    if inside_git_dir(root, absolute).unwrap_or(true) {
        return Err(SandboxError::IntoGit(project_path(root, absolute)));
    }
    Ok(())
}

/// `src/app.ts` for display and for the model; `.` for the root itself.
pub fn project_path(root: &Path, absolute: &Path) -> String {
    match absolute.strip_prefix(root) {
        Ok(rel) if rel.as_os_str().is_empty() => ".".into(),
        Ok(rel) => rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"),
        Err(_) => absolute.display().to_string(),
    }
}

#[cfg(test)]
pub(crate) mod test_dir {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A fresh temp folder, removed on drop (no tempfile dependency for a skeleton).
    pub struct TempDir(pub PathBuf);
    impl TempDir {
        pub fn new(tag: &str) -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("marv-sandbox-{tag}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(std::fs::canonicalize(dir).unwrap())
        }
        pub fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_dir::TempDir;
    use super::*;
    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    #[cfg(unix)]
    #[test]
    fn real_with_missing_follows_every_symlink_even_dangling_ones() {
        let dir = TempDir::new("real");
        let d = dir.path();
        fs::create_dir_all(d.join("a/b")).unwrap();
        symlink(d.join("a"), d.join("la")).unwrap();
        symlink("../nowhere/x", d.join("a/b/dangling")).unwrap();
        symlink("loop2", d.join("loop1")).unwrap();
        symlink("loop1", d.join("loop2")).unwrap();
        assert_eq!(real_with_missing(&d.join("la/b")).unwrap(), d.join("a/b"));
        assert_eq!(real_with_missing(&d.join("la/b/new/file")).unwrap(), d.join("a/b/new/file"));
        assert_eq!(real_with_missing(&d.join("la/b/dangling")).unwrap(), d.join("a/nowhere/x"));
        assert_eq!(real_with_missing(&d.join("a/../la/./b")).unwrap(), d.join("a/b"));
        assert!(real_with_missing(&d.join("loop1")).is_err());
    }

    #[test]
    fn normalize_resolves_dots_lexically() {
        assert_eq!(normalize(Path::new("/a/b/../c/./d")), PathBuf::from("/a/c/d"));
        assert_eq!(normalize(Path::new("/../x")), PathBuf::from("/x"));
    }

    #[test]
    fn resolves_relative_and_absolute_paths_inside_the_project() {
        let root = TempDir::new("resolve");
        let r = root.path();
        assert_eq!(resolve_in_project(r, "src/app.ts", Path::new("/nowhere")).unwrap(), r.join("src/app.ts"));
        assert_eq!(resolve_in_project(r, &r.join("a.ts").display().to_string(), Path::new("/nowhere")).unwrap(), r.join("a.ts"));
        assert_eq!(resolve_in_project(r, ".", Path::new("/nowhere")).unwrap(), r);
    }

    #[test]
    fn refuses_paths_outside_the_project() {
        let root = TempDir::new("outside");
        let err = resolve_in_project(root.path(), "../secret.txt", Path::new("/nowhere")).unwrap_err();
        assert!(err.to_string().contains("is outside the project"));
        assert!(resolve_in_project(root.path(), "/etc/hosts", Path::new("/nowhere")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn wont_follow_a_symlink_out_of_the_project() {
        let root = TempDir::new("link-in");
        let outside = TempDir::new("link-out");
        fs::write(outside.path().join("secret.txt"), "top secret\n").unwrap();
        symlink(outside.path().join("secret.txt"), root.path().join("link.txt")).unwrap();
        symlink(outside.path(), root.path().join("dir")).unwrap();
        assert!(resolve_in_project(root.path(), "link.txt", Path::new("/nowhere")).is_err());
        // A new file in a symlinked folder would land outside: judged by the nearest existing folder.
        assert!(resolve_in_project(root.path(), "dir/new.txt", Path::new("/nowhere")).is_err());
        // A dangling link: a write through it would create its target, outside (#8).
        symlink(outside.path().join("planted.sh"), root.path().join("out.sh")).unwrap();
        assert!(resolve_in_project(root.path(), "out.sh", Path::new("/nowhere")).is_err());
        // …or inside the project, which is fine, unless it's in .git.
        symlink(root.path().join("later.txt"), root.path().join("in.txt")).unwrap();
        assert_eq!(resolve_in_project(root.path(), "in.txt", Path::new("/nowhere")).unwrap(), root.path().join("in.txt"));
        fs::create_dir_all(root.path().join(".git/hooks")).unwrap();
        symlink(root.path().join(".git/hooks/pre-commit"), root.path().join("hook.sh")).unwrap();
        assert!(matches!(resolve_in_project(root.path(), "hook.sh", Path::new("/nowhere")), Err(SandboxError::IntoGit(_))));
        assert!(touches_git(root.path(), "hook.sh", Path::new("/nowhere")));
    }

    #[test]
    fn tilde_is_the_home_folder_and_tilde_name_is_literal() {
        let root = TempDir::new("tilde");
        let r = root.path();
        assert_eq!(resolve_in_project(r, "~/src/app.ts", r).unwrap(), r.join("src/app.ts"));
        assert_eq!(resolve_in_project(r, "~", r).unwrap(), r);
        assert_eq!(resolve_in_project(r, "~src/app.ts", r).unwrap(), r.join("~src/app.ts"));
    }

    #[cfg(unix)]
    #[test]
    fn touches_git_directly_and_through_a_symlink() {
        let root = TempDir::new("git");
        let r = root.path();
        fs::create_dir_all(r.join(".git/hooks")).unwrap();
        symlink(r.join(".git/hooks"), r.join("hooks")).unwrap();
        let home = Path::new("/nowhere");
        assert!(touches_git(r, ".git/config", home));
        assert!(touches_git(r, "hooks/pre-commit", home));
        assert!(touches_git(r, "../elsewhere", home)); // outside counts: it asks, and fails
        assert!(!touches_git(r, "src/a.ts", home));
        assert!(refuse_git(r, &r.join("hooks/pre-commit")).is_err());
        assert!(refuse_git(r, &r.join("src/a.ts")).is_ok());
    }

    #[test]
    fn project_path_is_slash_separated_and_dot_for_the_root() {
        assert_eq!(project_path(Path::new("/p"), Path::new("/p/src/a.ts")), "src/a.ts");
        assert_eq!(project_path(Path::new("/p"), Path::new("/p")), ".");
    }

    #[cfg(unix)]
    #[test]
    fn a_skill_file_is_confined_to_its_folder_after_realpath() {
        let skill = TempDir::new("skill");
        let outside = TempDir::new("skill-out");
        fs::write(skill.path().join("reference.md"), "ref").unwrap();
        fs::write(outside.path().join("id_ed25519"), "PRIVATE KEY").unwrap();
        symlink(outside.path().join("id_ed25519"), skill.path().join("key")).unwrap();
        fs::create_dir(skill.path().join("scripts")).unwrap();
        symlink(outside.path(), skill.path().join("scripts/out")).unwrap();

        assert_eq!(resolve_within(skill.path(), "reference.md").unwrap(), skill.path().join("reference.md"));
        // The vulnerability: lexically inside, really outside.
        assert!(matches!(resolve_within(skill.path(), "key"), Err(SandboxError::OutsideDir { .. })));
        assert!(matches!(resolve_within(skill.path(), "scripts/out/id_ed25519"), Err(SandboxError::OutsideDir { .. })));
        // Lexical escapes and the folder itself.
        assert!(matches!(resolve_within(skill.path(), "../../../config.json"), Err(SandboxError::OutsideDir { .. })));
        assert!(matches!(resolve_within(skill.path(), "/etc/passwd"), Err(SandboxError::OutsideDir { .. })));
        assert!(matches!(resolve_within(skill.path(), "."), Err(SandboxError::OutsideDir { .. })));
        assert!(matches!(resolve_within(skill.path(), "missing.md"), Err(SandboxError::Missing(_))));
    }

    #[test]
    fn inside_ignores_case_only_when_asked() {
        let (root, path) = (Path::new(r"C:\Users\Me\proj"), Path::new(r"c:\users\me\proj\a.rs"));
        // The pure logic, whatever OS runs the test: Windows paths only parse as such there, so use `/` ones.
        let (root, path) =
            if cfg!(windows) { (root, path) } else { (Path::new("/Users/Me/proj"), Path::new("/users/me/proj/a.rs")) };
        assert!(is_inside_with(root, path, true));
        assert!(!is_inside_with(root, path, false));
        assert!(is_inside_with(root, root, true));
        assert!(!is_inside_with(Path::new("/Users/Me/proj"), Path::new("/users/me/proj2/a.rs"), true), "whole components only");
        assert_eq!(strip_prefix_with(Path::new("/A/B/c.RS"), Path::new("/a/b"), true), Some(PathBuf::from("c.RS")));
        assert_eq!(strip_prefix_with(Path::new("/A/B/c.RS"), Path::new("/a/b"), false), None);
    }

    #[test]
    fn git_is_named_in_any_case_and_on_windows_in_ntfs_spellings_too() {
        for name in [".git", ".GIT", ".Git"] {
            assert!(is_git_name(name, false) && is_git_name(name, true), "{name}");
        }
        for name in [".git.", ".git ", ".git. .", "GIT~1", "git~12", ".git::$INDEX_ALLOCATION", ".GIT:x"] {
            assert!(is_git_name(name, true), "{name}");
            assert!(!is_git_name(name, false), "{name} is an ordinary name outside Windows");
        }
        for name in [".gitignore", ".github", "git", "GITX~1", "~1", ".gi"] {
            assert!(!is_git_name(name, true), "{name}");
        }
    }

    #[test]
    fn a_short_name_stands_for_its_protected_name() {
        assert_eq!(long_name_of_short("git~1"), Some(".git"));
        assert_eq!(long_name_of_short("marv~1"), Some(".marv"));
        assert_eq!(long_name_of_short("vscode~2"), Some(".vscode"));
        assert_eq!(long_name_of_short("envrc~1"), Some(".envrc"));
        assert_eq!(long_name_of_short("env~1.loc"), Some(".env"));
        assert_eq!(long_name_of_short("bunfig~1.tom"), Some("bunfig.toml"));
        assert_eq!(long_name_of_short("mcp~1.jso"), Some(".mcp.json"));
        assert_eq!(long_name_of_short("src~1"), None);
        assert_eq!(long_name_of_short("git~"), None);
        assert_eq!(long_name_of_short("git~x"), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_link_into_a_differently_cased_git_is_refused() {
        let root = TempDir::new("gitcase");
        symlink(".GIT/hooks/pre-commit", root.path().join("hook.sh")).unwrap();
        assert!(matches!(resolve_in_project(root.path(), "hook.sh", Path::new("/nowhere")), Err(SandboxError::IntoGit(_))));
        fs::create_dir_all(root.path().join(".Git")).unwrap();
        assert!(inside_git_dir(root.path(), &root.path().join(".Git/config")).unwrap());
        assert!(touches_git(root.path(), ".GIT/hooks/x", Path::new("/nowhere")));
    }
}
