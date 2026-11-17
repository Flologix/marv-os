//! Observed write sets (runtime-kernel §14.4, decision P4). New in marv-rs.
//!
//! Two chats in one folder can edit the same files, and nothing tells the person. §8.1's answer (declared read and
//! write sets, tested at admit) asks for declarations nobody writes, so the kernel **observes** instead: the paths each
//! chat's edit tools (`edit_file`, `write_file`) reported writing, kept for [`WRITE_WINDOW_MS`] (30 minutes). When two
//! live chats' sets share a path, the person sees it ("also editing here: <chat> (2 files)") and the event log gets a
//! [`crate::KernelEvent::WriteOverlap`] with the count, never the paths' content (nor the paths: the log holds who
//! and how many).
//!
//! **Shown, never scheduled.** Nothing admits, delays, wires or refuses by a write set: a conflict is the person's to
//! see (and §14.2's stale-write refusal is the hard guarantee underneath). bash's writes aren't seen (the sandbox
//! doesn't report them); that's an accepted gap.
//!
//! A set crosses between Marvs (a door's `marv/info`), so a peer's is never trusted on receipt ([`received`]): only
//! absolute, lexical, printable paths of bounded length, at most [`MAX_WRITES`], none from the future.
//!
//! Pure: no I/O, no clock (time is passed in).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::registry::{AgentKey, Registry};

/// How long a write counts: 30 minutes.
pub const WRITE_WINDOW_MS: u64 = 30 * 60 * 1000;
/// At most this many paths are kept per chat (the newest).
pub const MAX_WRITES: usize = 200;
/// A longer path is dropped.
pub const MAX_PATH_BYTES: usize = 1024;

/// One path a chat's edit tools wrote, and when (ms since the epoch). Unknown fields are ignored: it crosses between
/// Marvs of different versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentWrite {
    pub path: PathBuf,
    pub at_ms: u64,
}

/// A chat's own write set: each absolute path it wrote and when it last did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WriteSet {
    by_path: BTreeMap<PathBuf, u64>,
}

impl WriteSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// The chat wrote `path` (absolute; anything else is ignored) at `at_ms`. Writes older than the window go, and
    /// past [`MAX_WRITES`] the oldest do.
    pub fn record(&mut self, path: &Path, at_ms: u64) {
        let Some(path) = clean(path) else { return };
        let at = self.by_path.entry(path).or_insert(at_ms);
        *at = (*at).max(at_ms);
        self.prune(at_ms);
    }

    /// Drops writes older than the window at `now_ms`, then the oldest past [`MAX_WRITES`].
    pub fn prune(&mut self, now_ms: u64) {
        self.by_path.retain(|_, at| live(*at, now_ms));
        while self.by_path.len() > MAX_WRITES {
            let oldest = self.by_path.iter().min_by_key(|(_, at)| **at).map(|(p, _)| p.clone());
            match oldest {
                Some(path) => self.by_path.remove(&path),
                None => break,
            };
        }
    }

    /// A new conversation (/clear): nothing written yet.
    pub fn clear(&mut self) {
        self.by_path.clear();
    }

    /// The writes in the window at `now_ms`, newest first (then by path).
    pub fn recent(&self, now_ms: u64) -> Vec<RecentWrite> {
        let mut out: Vec<RecentWrite> = self
            .by_path
            .iter()
            .filter(|(_, at)| live(**at, now_ms))
            .map(|(path, at)| RecentWrite { path: path.clone(), at_ms: *at })
            .collect();
        out.sort_by(|a, b| b.at_ms.cmp(&a.at_ms).then_with(|| a.path.cmp(&b.path)));
        out
    }
}

fn live(at_ms: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(at_ms) < WRITE_WINDOW_MS
}

/// A path as a write set may hold it: absolute, without `.` or `..`, at most [`MAX_PATH_BYTES`], printable (no
/// control, bidi (`marv_core::printable::is_bidi`), line or paragraph separator: it may be shown to a person and a
/// model). None otherwise.
fn clean(path: &Path) -> Option<PathBuf> {
    let text = path.to_str()?;
    let lexical = path.components().all(|c| matches!(c, Component::RootDir | Component::Prefix(_) | Component::Normal(_)));
    let printable =
        !text.chars().any(|c| c.is_control() || marv_core::printable::is_bidi(c) || matches!(c, '\u{2028}' | '\u{2029}'));
    (path.is_absolute() && lexical && printable && text.len() <= MAX_PATH_BYTES).then(|| path.to_path_buf())
}

/// A write set as it may be kept from another Marv: each path cleaned (an unclean one is dropped), only paths inside
/// one of `roots` (the folders the Marv says it works in: its folder, and a desktop worktree chat's worktree; a path
/// elsewhere is a spoof, or not its work), each at most once (its newest), none after `now_ms` (a time from the future
/// is now), none older than the window, the newest [`MAX_WRITES`].
pub fn received(writes: Vec<RecentWrite>, roots: &[&Path], now_ms: u64) -> Vec<RecentWrite> {
    let roots: Vec<PathBuf> = roots.iter().filter_map(|r| clean(r)).collect();
    let mut set = WriteSet::new();
    for write in writes {
        if let Some(path) = clean(&write.path).filter(|p| roots.iter().any(|r| p.starts_with(r) && p != r)) {
            let at = set.by_path.entry(path).or_insert(0);
            *at = (*at).max(write.at_ms.min(now_ms));
        }
    }
    set.prune(now_ms);
    set.recent(now_ms)
}

/// The paths both sets wrote within the window at `now_ms`, sorted.
pub fn shared_paths(a: &[RecentWrite], b: &[RecentWrite], now_ms: u64) -> Vec<PathBuf> {
    let mine: BTreeSet<&Path> = a.iter().filter(|w| live(w.at_ms, now_ms)).map(|w| w.path.as_path()).collect();
    let both: BTreeSet<&Path> =
        b.iter().filter(|w| live(w.at_ms, now_ms) && mine.contains(w.path.as_path())).map(|w| w.path.as_path()).collect();
    both.into_iter().map(Path::to_path_buf).collect()
}

/// Another agent whose recent writes share paths with one's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlap {
    pub other: AgentKey,
    pub mesh_name: String,
    /// The paths both wrote in the window, sorted.
    pub paths: Vec<PathBuf>,
}

impl Registry {
    /// The other agents whose writes in the window share a path with `id`'s, by mesh name. Seeing grants nothing: it
    /// is shown to the person and logged, never consulted by anything that admits, wires or wakes.
    pub fn overlaps(&self, id: &AgentKey, now_ms: u64) -> Vec<Overlap> {
        let Some(me) = self.get(id) else { return Vec::new() };
        if me.writes.is_empty() {
            return Vec::new();
        }
        self.by_mesh_name()
            .filter(|other| other.id != *id)
            .filter_map(|other| {
                let paths = shared_paths(&me.writes, &other.writes, now_ms);
                (!paths.is_empty()).then(|| Overlap { other: other.id.clone(), mesh_name: other.mesh_name.clone(), paths })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::AgentKind;
    use crate::registry::Entry;

    const MIN: u64 = 60 * 1000;

    fn w(path: &str, at_ms: u64) -> RecentWrite {
        RecentWrite { path: path.into(), at_ms }
    }

    #[test]
    fn a_set_keeps_absolute_paths_for_thirty_minutes() {
        let mut set = WriteSet::new();
        set.record(Path::new("/w/src/a.rs"), 1_000);
        set.record(Path::new("src/relative.rs"), 1_000);
        set.record(Path::new("/w/src/../b.rs"), 1_000);
        set.record(Path::new("/w/src/a.rs"), 2_000);
        set.record(Path::new("/w/src/c.rs"), 3_000);
        assert_eq!(set.recent(3_000), [w("/w/src/c.rs", 3_000), w("/w/src/a.rs", 2_000)]);
        assert_eq!(set.recent(2_000 + WRITE_WINDOW_MS), [w("/w/src/c.rs", 3_000)], "a.rs is 30 minutes old");
        assert!(set.recent(3_000 + WRITE_WINDOW_MS).is_empty());
        set.clear();
        assert!(set.recent(3_000).is_empty());
    }

    #[test]
    fn a_set_holds_the_newest_paths_only() {
        let mut set = WriteSet::new();
        for n in 0..(MAX_WRITES as u64 + 5) {
            set.record(Path::new(&format!("/w/{n}.rs")), 10 + n);
        }
        let recent = set.recent(10 + MAX_WRITES as u64 + 5);
        assert_eq!(recent.len(), MAX_WRITES);
        assert!(!recent.iter().any(|w| w.path == Path::new("/w/0.rs")), "the oldest went");
    }

    #[test]
    fn a_peers_set_is_cleaned_on_receipt() {
        let now = 100 * MIN;
        let got = received(
            vec![
                w("/elsewhere/spoof.rs", now),
                w("/w", now),
                w("/w/a\u{2028}b.rs", now),
                w("/w/ok.rs", now - MIN),
                w("/w/ok.rs", now - 2 * MIN),
                w("relative.rs", now),
                w("/w/../etc/passwd", now),
                w("/w/\x1b[2Jx.rs", now),
                w("/w/\u{202e}sr.txt", now),
                w(&format!("/w/{}", "x".repeat(MAX_PATH_BYTES)), now),
                w("/w/future.rs", now + 50 * MIN),
                w("/w/old.rs", now - 31 * MIN),
            ],
            &[Path::new("/w")],
            now,
        );
        assert_eq!(got, [w("/w/future.rs", now), w("/w/ok.rs", now - MIN)], "outside its folder, a separator: dropped");
        let many: Vec<RecentWrite> = (0..500).map(|n| w(&format!("/w/{n}"), now)).collect();
        assert_eq!(received(many.clone(), &[Path::new("/w")], now).len(), MAX_WRITES);
        assert!(received(many, &[], now).is_empty(), "no folder: nothing");
        let both = [Path::new("/w"), Path::new("/trees/x")];
        assert_eq!(received(vec![w("/trees/x/a.rs", now), w("/w/b.rs", now)], &both, now).len(), 2, "a worktree counts");
    }

    #[test]
    fn overlaps_are_the_shared_paths_in_the_window() {
        let now = 100 * MIN;
        let mut r = Registry::new();
        let key = |name: &str| AgentKey::Mesh(name.into());
        let entry = |name: &str, writes: Vec<RecentWrite>| {
            let mut e = Entry::new(key(name), name, "/w", AgentKind::Terminal);
            e.writes = writes;
            e
        };
        r.upsert(entry("a", vec![w("/w/x.rs", now), w("/w/y.rs", now), w("/w/old.rs", now - 40 * MIN)]));
        r.upsert(entry("b", vec![w("/w/y.rs", now - 29 * MIN), w("/w/x.rs", now - MIN), w("/w/z.rs", now)]));
        r.upsert(entry("c", vec![w("/w/old.rs", now)]));
        r.upsert(entry("d", Vec::new()));
        let got = r.overlaps(&key("a"), now);
        assert_eq!(got.len(), 1, "c's old.rs is outside a's window: {got:?}");
        assert_eq!((got[0].mesh_name.as_str(), got[0].paths.clone()), ("b", vec![PathBuf::from("/w/x.rs"), "/w/y.rs".into()]));
        assert_eq!(r.overlaps(&key("b"), now)[0].paths.len(), 2, "both ways");
        assert_eq!(r.overlaps(&key("b"), now + 2 * MIN)[0].paths, [PathBuf::from("/w/x.rs")], "y.rs aged out of b's");
        assert!(r.overlaps(&key("d"), now).is_empty() && r.overlaps(&key("nobody"), now).is_empty());
    }
}
