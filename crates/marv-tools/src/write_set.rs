//! A chat role's write set (`writes:` in `.marv/roles/<name>.md`, `marv_session::roles`): the project-relative path
//! prefixes, folders or files, the edit tools may write under. New in marv-rs.
//!
//! - `None` on [`crate::ToolContext::write_set`]: no role, or a role without `writes`: unrestricted, as before.
//! - An empty set: the role may not write files at all.
//! - A prefix is a plain relative path (`pages/`, `styles.css`), never a glob, never absolute, never climbing out
//!   (`..`), never `.git` ([`parse_prefix`]). `pages` and `pages/` both mean the folder `pages` (and a file named so).
//! - A path is inside the set only when **both** its lexical place in the project and the place it really leads
//!   (through symlinks, `marv_sandbox::paths::real_with_missing`) are under one of the prefixes: a symlink inside
//!   `pages/` leading to `secret.txt` is outside ([`WriteSet::check`]). The edit tools check it in their plan (the
//!   preview, before anyone is asked, and again at apply time), so a refused path is never offered for approval.
//! - A role may also name `tool_cache` paths (same form): writable for commands only, created for them when missing
//!   (their parent must exist), so a dev server's or a type checker's own cache works in a read-only role.
//! - bash: a sandboxed command sees the project read-only with each existing prefix bound read-write on top
//!   (`crate::bash`); a prefix that doesn't exist yet can't be created from bash (write_file creates folders).

use std::path::{Path, PathBuf};

use crate::types::ToolError;

/// The prefixes a role may write under.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WriteSet {
    /// Normalized: no leading `./`, no trailing `/`, `/`-separated.
    prefixes: Vec<String>,
    /// Tool caches and build output the role's commands (bash, `process`) may write though it may not edit them: only
    /// those two see them ([`Self::existing`]), the edit tools still refuse them ([`Self::check`]).
    cache: Vec<String>,
}

/// The one-time note a role's read-only folder earns (Demo2: a coordinator whose `cat <<EOF > x` failed with "Read-only
/// file system" thought its sandbox had broken and tried /tmp). Marv's own words: the role and the editors' names come
/// from the person's role and team, never from output or messages.
#[derive(Debug)]
pub struct ReadOnlyNote {
    /// This chat's role.
    role: String,
    /// What this role may write, as the prompt says it ("nothing", or the prefixes).
    allowed: String,
    /// The team's roles that can edit files and where each may (None: anywhere in the project).
    editors: Vec<(String, Option<Vec<String>>)>,
    shown: std::sync::atomic::AtomicBool,
}

impl ReadOnlyNote {
    pub fn new(role: &str, set: &WriteSet, editors: Vec<(String, Option<Vec<String>>)>) -> Self {
        let allowed = if set.prefixes().is_empty() && set.cache().is_empty() {
            "nothing".to_string()
        } else {
            set.prefixes().iter().chain(set.cache()).cloned().collect::<Vec<_>>().join(", ")
        };
        Self { role: role.to_string(), allowed, editors, shown: false.into() }
    }

    /// The note for a failed command's or process's `output`, once per chat: when it shows a read-only file system error.
    pub fn note(&self, output: &str) -> Option<String> {
        if !(output.contains("Read-only file system") || output.contains("EROFS")) {
            return None;
        }
        if self.shown.swap(true, std::sync::atomic::Ordering::Relaxed) {
            return None;
        }
        let path_owners: Vec<&str> = self
            .editors
            .iter()
            .filter(|(_, writes)| {
                writes.as_ref().is_some_and(|list| {
                    output
                        .split(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | ':' | '`'))
                        .map(|t| t.trim_start_matches("./"))
                        .any(|t| list.iter().any(|p| t == p || t.strip_prefix(p.as_str()).is_some_and(|r| r.starts_with('/'))))
                })
            })
            .map(|(name, _)| name.as_str())
            .collect();
        let owners: Vec<&str> =
            if path_owners.is_empty() { self.editors.iter().map(|(n, _)| n.as_str()).collect() } else { path_owners };
        let ask = match owners.as_slice() {
            [] => "Ask the person to make the change.".to_string(),
            [one] => format!("The {one} role owns file changes: ask it (mesh_send) to make this one."),
            many => {
                format!("The roles that own file changes are {}: ask the one whose files these are (mesh_send).", many.join(", "))
            }
        };
        Some(format!(
            "Marv: the project folder is read-only for the {} role by design (it may write only {}), so this is not a broken sandbox and no workaround (a heredoc, /tmp, another path) will change a project file. {ask}",
            self.role, self.allowed
        ))
    }
}

/// Characters that would make a prefix a glob: refused, so nobody believes `src/*.ts` limits anything.
const GLOB: [char; 5] = ['*', '?', '[', ']', '{'];

/// One `writes:` entry, normalized, or why it isn't a prefix.
pub fn parse_prefix(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("an empty entry (leave writes out to allow the whole project)".into());
    }
    if trimmed.contains(GLOB) {
        return Err(format!("\"{trimmed}\" is a glob: list folders (pages/) or files (styles.css) instead"));
    }
    if trimmed.starts_with('/') || trimmed.starts_with('~') || trimmed.contains('\\') {
        return Err(format!("\"{trimmed}\" should be a path relative to the project"));
    }
    let parts: Vec<&str> = trimmed.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    if parts.is_empty() {
        return Err(format!("\"{trimmed}\" is the whole project: leave writes out for that"));
    }
    if parts.contains(&"..") {
        return Err(format!("\"{trimmed}\" climbs out of the project"));
    }
    if parts[0] == ".git" {
        return Err(format!("\"{trimmed}\" is inside .git"));
    }
    // Review P2-3: never a protected path's prefix (`.marv`, `.env*`, `AGENTS.md`, `.vscode`…): the sandbox keeps them
    // read-only inside a writable folder anyway, but a role must not even name one as its own.
    let joined = parts.join("/");
    if marv_sandbox::protected::rule_for_relative(std::path::Path::new(&joined)).is_some() {
        return Err(format!("\"{trimmed}\" is (or is inside) a protected path ({joined}): roles can't own those"));
    }
    Ok(parts.join("/"))
}

impl WriteSet {
    /// From normalized prefixes ([`parse_prefix`]'s).
    pub fn new(prefixes: Vec<String>) -> Self {
        Self { prefixes, cache: Vec::new() }
    }

    /// This set with `cache` (normalized prefixes, [`parse_prefix`]'s) writable for commands.
    pub fn with_cache(mut self, cache: Vec<String>) -> Self {
        self.cache = cache;
        self
    }

    pub fn cache(&self) -> &[String] {
        &self.cache
    }

    pub fn prefixes(&self) -> &[String] {
        &self.prefixes
    }

    pub fn is_empty(&self) -> bool {
        self.prefixes.is_empty()
    }

    /// Whether the project-relative `rel` (`/`-separated, no `./`) is one of the prefixes or under one.
    pub fn allows_relative(&self, rel: &str) -> bool {
        self.prefixes.iter().any(|p| rel == p || rel.strip_prefix(p.as_str()).is_some_and(|rest| rest.starts_with('/')))
    }

    /// Makes the tool-cache folder `cache` (project-relative) when it is missing and its parent already exists (so
    /// `node_modules/.vite` is made only where `node_modules` is, never a folder tree). The parent is canonicalized and
    /// must lie inside `real_root`; the folder is made as that real parent joined with the single last name
    /// (`create_dir`, which never follows a final symlink), then checked again with `symlink_metadata`: it must be a real
    /// folder, not a link, that really lies inside the project. A path that doesn't pass is left as it is.
    fn make_cache(real_root: &Path, cache: &str) {
        let (parent_rel, name) = cache.rsplit_once('/').unwrap_or(("", cache));
        if name.is_empty() || name.contains('/') {
            return;
        }
        let parent = if parent_rel.is_empty() { real_root.to_path_buf() } else { real_root.join(parent_rel) };
        let Ok(real_parent) = std::fs::canonicalize(&parent) else { return };
        if !real_parent.starts_with(real_root) {
            return;
        }
        let target = real_parent.join(name);
        if std::fs::symlink_metadata(&target).is_ok() || std::fs::create_dir(&target).is_err() {
            return;
        }
        // Made just now: it must be a real folder (not a link a racing process put there) that really lies inside.
        let is_real_dir = std::fs::symlink_metadata(&target).is_ok_and(|m| m.is_dir());
        let inside = std::fs::canonicalize(&target).is_ok_and(|p| p.starts_with(real_root));
        if !is_real_dir || !inside {
            let _ = std::fs::remove_dir(&target);
        }
    }

    /// The prefixes as the person wrote them for a message: `pages/, styles.css`, or "nothing".
    pub fn shown(&self, root: &Path) -> String {
        if self.prefixes.is_empty() {
            return "nothing (this role may not change files)".into();
        }
        self.prefixes
            .iter()
            .map(|p| if root.join(p).is_dir() { format!("{p}/") } else { p.clone() })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Whether `absolute` (a path `resolve_in_project` accepted) may be written: lexically and really inside a prefix.
    pub fn check(&self, root: &Path, absolute: &Path, shown: &str) -> Result<(), ToolError> {
        let refuse = || {
            ToolError(format!(
                "\"{shown}\" is outside this chat's role write set, so it wasn't changed. This role may write only: {}. Ask the person if this file needs changing.",
                self.shown(root)
            ))
        };
        let lexical = marv_sandbox::project_path(root, absolute);
        if !self.allows_relative(&lexical) {
            return Err(refuse());
        }
        let real_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let real = marv_sandbox::paths::real_with_missing(absolute).map_err(|_| refuse())?;
        match real.strip_prefix(&real_root) {
            Ok(rel)
                if self.allows_relative(
                    &rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"),
                ) =>
            {
                Ok(())
            }
            _ => Err(refuse()),
        }
    }

    /// The prefixes that exist now, as real paths inside the project's real folder: what a sandboxed command may
    /// change (bash binds each read-write over the read-only project). One leading out of the project is left out.
    /// With `create`, the tool-cache paths too, made first when missing (Marv makes them, outside the sandbox, since a
    /// mount needs the path to exist): see [`Self::make_cache`]. `create` is false in a home-folder project, where
    /// nothing is made.
    pub fn existing(&self, root: &Path, create: bool) -> Vec<PathBuf> {
        let Ok(real_root) = std::fs::canonicalize(root) else { return Vec::new() };
        if create {
            for cache in &self.cache {
                Self::make_cache(&real_root, cache);
            }
        }
        let cache_allows =
            |rel: &str| self.cache.iter().any(|p| rel == p || rel.strip_prefix(p.as_str()).is_some_and(|r| r.starts_with('/')));
        self.prefixes
            .iter()
            .chain(&self.cache)
            .filter_map(|p| std::fs::canonicalize(root.join(p)).ok())
            .filter(|real| real != &real_root && real.starts_with(&real_root))
            // Review P3-1: a prefix that is a symlink binds where it leads, so that place must be in the set itself (a
            // `pages -> ..` link would otherwise make the whole project writable).
            .filter(|real| {
                real.strip_prefix(&real_root).is_ok_and(|rel| {
                    let rel = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
                    (self.allows_relative(&rel) || cache_allows(&rel))
                        && marv_sandbox::protected::rule_for_relative(Path::new(&rel)).is_none()
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_only_failure_gets_one_note_naming_who_owns_the_path() {
        let editors = vec![
            ("frontend".to_string(), Some(vec!["site".to_string()])),
            ("backend".to_string(), Some(vec!["api".to_string()])),
        ];
        let note = ReadOnlyNote::new("devserver", &WriteSet::new(Vec::new()), editors.clone());
        assert_eq!(note.note("all fine"), None, "other failures get nothing");
        let said = note.note("EROFS: read-only file system, open 'api/server.ts'").unwrap();
        assert!(said.contains("read-only for the devserver role by design"), "{said}");
        assert!(said.contains("The backend role owns file changes") && !said.contains("frontend"), "{said}");
        assert_eq!(note.note("touch: x: Read-only file system"), None, "once");
        let cached = ReadOnlyNote::new("reviewer", &WriteSet::new(Vec::new()).with_cache(vec!["dist".into()]), editors);
        let said = cached.note("touch: x: Read-only file system").unwrap();
        assert!(said.contains("may write only dist") && said.contains("frontend, backend"), "{said}");
        let alone = ReadOnlyNote::new("coordinator", &WriteSet::new(Vec::new()), Vec::new());
        assert!(alone.note("Read-only file system").unwrap().contains("Ask the person"));
    }

    #[test]
    fn prefixes_are_plain_relative_paths() {
        assert_eq!(parse_prefix("./pages/").unwrap(), "pages");
        assert_eq!(parse_prefix("styles.css").unwrap(), "styles.css");
        assert_eq!(parse_prefix("a//b/").unwrap(), "a/b");
        for bad in [
            "",
            " ",
            ".",
            "./",
            "src/*.ts",
            "/etc",
            "~/x",
            "../up",
            "a/../../b",
            ".git",
            ".git/hooks",
            "a\\b",
            ".marv",
            ".marv/roles",
            "pages/.envrc",
            "AGENTS.md",
            ".vscode",
        ] {
            assert!(parse_prefix(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_path_is_inside_only_under_a_whole_segment() {
        let set = WriteSet::new(vec!["pages".into(), "styles.css".into()]);
        assert!(set.allows_relative("pages") && set.allows_relative("pages/a/b.html") && set.allows_relative("styles.css"));
        assert!(!set.allows_relative("pages2/x") && !set.allows_relative("styles.css.bak") && !set.allows_relative("script.js"));
        assert!(!WriteSet::default().allows_relative("anything"));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_leading_out_of_the_set_is_outside() {
        let dir = std::env::temp_dir().join(format!("marv-write-set-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(&dir).unwrap();
        let root = std::fs::canonicalize(&dir).unwrap();
        std::fs::create_dir_all(root.join("pages")).unwrap();
        std::fs::write(root.join("secret.txt"), "s").unwrap();
        std::os::unix::fs::symlink(root.join("secret.txt"), root.join("pages/link.txt")).unwrap();
        let set = WriteSet::new(vec!["pages".into()]);
        assert!(set.check(&root, &root.join("pages/new.html"), "pages/new.html").is_ok());
        let err = set.check(&root, &root.join("pages/link.txt"), "pages/link.txt").unwrap_err();
        assert!(err.0.contains("outside this chat's role write set") && err.0.contains("pages/"), "{}", err.0);
        assert!(set.check(&root, &root.join("secret.txt"), "secret.txt").is_err());
        assert_eq!(set.existing(&root, true), vec![root.join("pages")]);
        // A prefix that's a symlink to somewhere outside the set is never bound writable.
        std::fs::create_dir_all(root.join("other")).unwrap();
        std::os::unix::fs::symlink(root.join("other"), root.join("up")).unwrap();
        assert!(WriteSet::new(vec!["up".into()]).existing(&root, true).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_tool_cache_is_made_only_under_a_folder_that_is_there_and_never_through_a_link_or_a_home_folder() {
        let dir = std::env::temp_dir().join(format!("marv-write-set-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(&dir).unwrap();
        let root = std::fs::canonicalize(&dir).unwrap();
        let set = WriteSet::new(Vec::new()).with_cache(vec![
            "node_modules/.vite".into(),
            "node_modules/.cache".into(),
            ".cache".into(),
        ]);
        // No node_modules: neither of its caches is made (and no folder tree); the root one is.
        assert_eq!(set.existing(&root, true), vec![root.join(".cache")]);
        assert!(!root.join("node_modules").exists());
        // A home-folder project: nothing is made.
        std::fs::remove_dir(root.join(".cache")).unwrap();
        assert!(set.existing(&root, false).is_empty() && !root.join(".cache").exists());
        // With node_modules there, both are made, as real folders.
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        let made = set.existing(&root, true);
        assert!(made.contains(&root.join("node_modules/.vite")) && made.contains(&root.join("node_modules/.cache")));
        assert!(std::fs::symlink_metadata(root.join("node_modules/.vite")).unwrap().is_dir());
        // A node_modules that leads out of the project gets nothing made through it.
        let outside = std::env::temp_dir().join(format!("marv-write-set-out-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(&outside).unwrap();
        let root2 = root.join("p2");
        std::fs::create_dir_all(&root2).unwrap();
        std::os::unix::fs::symlink(&outside, root2.join("node_modules")).unwrap();
        assert!(set.existing(&root2, true).iter().all(|p| !p.starts_with(&outside)));
        assert!(!outside.join(".vite").exists() && !outside.join(".cache").exists());
        // A cache name that is already a link is left alone and never bound.
        std::fs::create_dir_all(root2.join("elsewhere")).unwrap();
        std::fs::remove_file(root2.join("node_modules")).unwrap();
        std::fs::create_dir_all(root2.join("node_modules")).unwrap();
        std::os::unix::fs::symlink(&outside, root2.join("node_modules/.vite")).unwrap();
        assert!(set.existing(&root2, true).iter().all(|p| !p.starts_with(&outside)));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
