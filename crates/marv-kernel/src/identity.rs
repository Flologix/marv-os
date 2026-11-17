//! Who an agent is (runtime-kernel phase 2a). New in marv-rs.
//!
//! A desktop chat is an agent with a UUID. Its mesh name used to be its folder plus four random hex digits, picked
//! anew at every start, so a chat that was stopped and opened again came back under another name and the wires to
//! it were lost. [`mesh_name_for`] derives the name from the id instead: the same chat is always the same name.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Longest mesh session name (`marv_mesh::discovery::MAX_SESSION_NAME`, which leaves room for the `peer-` prefix).
/// A name from [`mesh_name_for`] is at most `20 + 1 + 8`, well under it and short enough for a socket path on macOS.
pub const NAME_BASE_MAX: usize = 20;
/// Hex digits of the id in a mesh name: 32 bits, unique enough among one person's chats.
const ID_DIGITS: usize = 8;

/// An agent's id: a desktop chat's UUID. The registry's key (later phases); never shown to a model as text it could
/// confuse with an instruction, as it's hex and dashes only. Read from a file (a rule, phase 2b) it is checked the same
/// way: anything [`AgentId::new`] refuses doesn't parse.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AgentId(String);

impl TryFrom<String> for AgentId {
    type Error = String;
    fn try_from(id: String) -> Result<Self, String> {
        Self::new(&id).ok_or_else(|| format!("\"{id}\" isn't an agent id"))
    }
}

impl From<AgentId> for String {
    fn from(id: AgentId) -> String {
        id.0
    }
}

impl AgentId {
    /// An id: ASCII letters, digits and `-`, 8 to 64 characters (a UUID is 36).
    pub fn new(id: &str) -> Option<Self> {
        let ok = (8..=64).contains(&id.len()) && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        ok.then(|| Self(id.to_ascii_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScopeError {
    #[error("{0} isn't an absolute path without . or .. in it")]
    NotCanonical(String),
    #[error("{0} is the home folder or above it")]
    HomeOrAbove(String),
}

/// A folder subtree an agent works in, or a rule names (phase 2b). Absolute, with no `..` (the caller canonicalizes,
/// symlinks included: this crate doesn't touch the file system; a `.` inside is dropped), and never the home folder or
/// above it, the desktop's rule for a chat's folder.
///
/// Read from a file (a rule's subject, phase 2b) only the first part can be checked, as the home isn't known there:
/// the store checks the rest ([`FolderScope::check_home`]) before a rule is used.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "PathBuf", into = "PathBuf")]
pub struct FolderScope(PathBuf);

impl TryFrom<PathBuf> for FolderScope {
    type Error = ScopeError;
    fn try_from(path: PathBuf) -> Result<Self, ScopeError> {
        Self::canonical(&path)
    }
}

impl From<FolderScope> for PathBuf {
    fn from(scope: FolderScope) -> PathBuf {
        scope.0
    }
}

/// Absolute, with nothing but plain names after the root: no `.`, no `..`.
pub(crate) fn is_canonical(path: &Path) -> bool {
    path.is_absolute() && path.components().all(|c| matches!(c, Component::RootDir | Component::Normal(_) | Component::Prefix(_)))
}

impl FolderScope {
    pub fn new(path: &Path, home: &Path) -> Result<Self, ScopeError> {
        let scope = Self::canonical(path)?;
        scope.check_home(home)?;
        Ok(scope)
    }

    /// The first half of [`Self::new`]: absolute and canonical, home not checked.
    fn canonical(path: &Path) -> Result<Self, ScopeError> {
        if !is_canonical(path) {
            return Err(ScopeError::NotCanonical(path.display().to_string()));
        }
        // Rebuilt from its components: a `.` inside (`/a/./b`) is dropped, as `Path::components` does.
        Ok(Self(path.components().collect()))
    }

    /// The second half of [`Self::new`]: never the home folder or above it.
    pub fn check_home(&self, home: &Path) -> Result<(), ScopeError> {
        if contains(&self.0, home) {
            return Err(ScopeError::HomeOrAbove(self.0.display().to_string()));
        }
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Component-wise: `/a/b` contains `/a/b` and `/a/b/c`, never `/a/bc`.
    pub fn contains(&self, folder: &Path) -> bool {
        contains(&self.0, folder)
    }

    pub fn contains_scope(&self, other: &FolderScope) -> bool {
        self.contains(&other.0)
    }

    /// Whether two agents' subtrees can hold the same files: only when one contains the other. Siblings never do.
    pub fn overlaps(&self, other: &FolderScope) -> bool {
        self.contains_scope(other) || other.contains_scope(self)
    }
}

fn contains(outer: &Path, inner: &Path) -> bool {
    // `Path::starts_with` compares whole components, so `/a/bc` doesn't start with `/a/b`.
    inner.starts_with(outer)
}

/// What kind of agent a mesh end is (runtime-kernel 2b). Rules cover Marvs only (decision D8): a desktop chat or a
/// terminal Marv. A subagent and a shim plug (`marv mesh plug`: Claude Code, opencode) are never covered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    /// A desktop chat: it has an [`AgentId`] (its UUID).
    Chat,
    /// A Marv in a terminal (the TUI, `marv -p`).
    Terminal,
    /// A Marv's subagent (`worker-N`).
    Subagent,
    /// Another program on a shim plug.
    Shim,
}

/// Who an agent is, as a rule sees it: its id (a chat's; a terminal Marv has none), its mesh name, its folder and its
/// kind. Built from what Marv knows itself (its own session) or from a peer's pid-confirmed `marv/info`, never from
/// a message.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AgentRef {
    pub id: Option<AgentId>,
    pub mesh_name: String,
    /// Where it works, as it reported it. Matched against a rule's subtree only when it is absolute and canonical.
    pub folder: PathBuf,
    pub kind: AgentKind,
}

impl AgentRef {
    /// Whether two references are the same agent: the same mesh name, or the same chat.
    pub fn same_agent(&self, other: &AgentRef) -> bool {
        self.mesh_name == other.mesh_name || (self.id.is_some() && self.id == other.id)
    }
}

/// A folder's name as a mesh name's base: lowercased, anything but `a-z`, `0-9` and `_` as `-`, trimmed, at most
/// [`NAME_BASE_MAX`] characters, `marv` when nothing is left (`/`, a name of symbols).
pub fn name_base(folder: &Path) -> String {
    let name = folder.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let mapped: String =
        name.chars().map(|c| if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' { c } else { '-' }).collect();
    let base: String = mapped.trim_matches(|c: char| !c.is_ascii_alphanumeric()).chars().take(NAME_BASE_MAX).collect();
    let base = base.trim_end_matches(['-', '_']);
    if base.is_empty() { "marv".to_string() } else { base.to_string() }
}

/// An agent's mesh name: `<folder base>-<first 8 hex digits of its id>`, e.g. `circuit-554963ee`. The same for the
/// same agent at every start; different for two chats in one folder. A valid mesh segment, at most 29 characters.
pub fn mesh_name_for(folder: &Path, id: &AgentId) -> String {
    let digits: String = id.0.chars().filter(char::is_ascii_hexdigit).take(ID_DIGITS).collect();
    format!("{}-{digits}", name_base(folder))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/home/me";

    fn scope(path: &str) -> FolderScope {
        FolderScope::new(Path::new(path), Path::new(HOME)).unwrap()
    }

    fn id(text: &str) -> AgentId {
        AgentId::new(text).unwrap()
    }

    #[test]
    fn containment_is_component_wise_and_siblings_never_overlap() {
        let b = scope("/home/me/a/b");
        assert!(b.contains(Path::new("/home/me/a/b")));
        assert!(b.contains(Path::new("/home/me/a/b/c/d")));
        assert!(!b.contains(Path::new("/home/me/a/bc")));
        assert!(!b.contains(Path::new("/home/me/a")));
        assert!(b.overlaps(&scope("/home/me/a/b/c")) && scope("/home/me/a/b/c").overlaps(&b));
        assert!(!b.overlaps(&scope("/home/me/a/c")), "siblings run in parallel");
    }

    #[test]
    fn a_scope_is_absolute_and_canonical_and_never_home_or_above() {
        let home = Path::new(HOME);
        for bad in ["relative/path", "/home/me/a/../b", "./a"] {
            assert!(matches!(FolderScope::new(Path::new(bad), home), Err(ScopeError::NotCanonical(_))), "{bad}");
        }
        for above in ["/home/me", "/home", "/"] {
            assert!(matches!(FolderScope::new(Path::new(above), home), Err(ScopeError::HomeOrAbove(_))), "{above}");
        }
        assert!(FolderScope::new(Path::new("/srv/work"), home).is_ok(), "outside home is the caller's to refuse");
        assert_eq!(scope("/home/me/./a/b/").path(), Path::new("/home/me/a/b"));
    }

    #[test]
    fn ids_are_checked_and_lowercased() {
        assert_eq!(id("554963EE-fd8e-4bec-a7dc-49051f090acc").as_str(), "554963ee-fd8e-4bec-a7dc-49051f090acc");
        assert!(AgentId::new("short").is_none());
        assert!(AgentId::new("../../etc/passwd-aaaaaaaa").is_none());
        assert!(AgentId::new("has space in it here").is_none());
    }

    #[test]
    fn a_chats_mesh_name_is_stable_unique_per_chat_and_a_valid_short_segment() {
        let folder = Path::new("/home/me/Projects/My Circuit Board!");
        let a = id("554963ee-fd8e-4bec-a7dc-49051f090acc");
        let b = id("0a1b2c3d-0000-4000-8000-000000000000");
        assert_eq!(mesh_name_for(folder, &a), "my-circuit-board-554963ee");
        assert_eq!(mesh_name_for(folder, &a), mesh_name_for(folder, &a), "the same at every start");
        assert_ne!(mesh_name_for(folder, &a), mesh_name_for(folder, &b), "two chats in one folder");
        let long = mesh_name_for(Path::new("/x/a-very-long-folder-name-indeed-yes-really"), &a);
        assert!(long.len() <= NAME_BASE_MAX + 1 + ID_DIGITS, "{long}");
        for name in [mesh_name_for(folder, &a), long, mesh_name_for(Path::new("/"), &b)] {
            assert!(name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'), "{name}");
            assert!(name.starts_with(|c: char| c.is_ascii_alphanumeric()), "{name}");
        }
        assert_eq!(mesh_name_for(Path::new("/"), &b), "marv-0a1b2c3d");
    }
}
