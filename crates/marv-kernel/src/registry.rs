//! Who exists (runtime-kernel phase 3, `docs/architecture/runtime-kernel.md` §5.4). New in marv-rs.
//!
//! One index of the agents a Marv knows about: its own (a desktop's chats, filled by the desktop, authoritative) and
//! the other Marvs on the machine (seen through their doors, refreshed by the mesh's scan). Each [`Entry`] is an
//! agent's key, mesh name, folder, kind, scheduler state (when known) and what it can be reached at
//! ([`Reach`]: its inboxes, the radio, a Herdr pane). Indexed by folder (a sorted map, so a subtree is one range:
//! [`Registry::under`]), by capability ([`Registry::with`]) and by space root ([`Registry::in_space`]).
//!
//! **Seeing grants nothing.** An entry is what an agent *is* and where it can be *asked*; it is never a permission.
//! Nothing that admits, wires or wakes reads the registry (`Switchboard::authorize` asks the person's wires and rules
//! alone): a [`Reach`] is an address, not a capability token.
//!
//! Pure: no I/O, no clock, no runtime.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::identity::{AgentId, AgentKind, FolderScope};
use crate::scheduler::AgentState;

/// An entry's key: a chat's id, or the mesh name of a Marv that has none (a terminal Marv).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentKey {
    /// A desktop chat, by its UUID.
    Chat(AgentId),
    /// A Marv without a chat id, by its mesh name (a door's name is unique among live doors).
    Mesh(String),
}

impl fmt::Display for AgentKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AgentKey::Chat(id) => write!(f, "chat {id}"),
            AgentKey::Mesh(name) => write!(f, "marv {name}"),
        }
    }
}

/// Where an agent can be reached: an address, never a permission (see the module doc).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Reach {
    /// An agent's inbox there (`agent.<plug>.inbox`): `parent`, `worker-N`, a shim's plug.
    Inbox(String),
    /// It's on the radio.
    Radio,
    /// It runs in this Herdr pane.
    HerdrPane(String),
}

/// A chat's card (runtime-kernel chat card, §5.6): what its conversation is about, in the person's own typed words,
/// so `mesh_list` can say more than a mesh name. Never model output, a plan step, a tool's output, a woken turn's text
/// or a message from the mesh: only what the person typed (`marv_mesh::card` builds it, and cuts and cleans a peer's
/// card again on receipt). No model call makes it. Unknown fields are ignored, not refused: it crosses between Marvs
/// of different versions, and a newer one's extra field must not drop the whole card on an older reader (it carries
/// only these fields onward).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Card {
    /// The conversation's first person-typed message, one printable line, cut with `…` (as a radio presence's title);
    /// empty before the person said anything. (With `"mesh": {"cards": false}` no card is published at all.)
    #[serde(default)]
    pub title: String,
    /// The latest person-typed message, by the same rules.
    #[serde(default)]
    pub now: String,
    /// When its last turn ended (ms since the epoch); 0 when unknown.
    #[serde(default)]
    pub last_active_ms: u64,
    /// Its device set (runtime-kernel §17.1): the MCP servers and doors it booted with, by name (`mcp:linear`,
    /// `github`, `web`…). Names from the person's own config, never their words; empty in older cards.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<String>,
    /// Runtime-kernel §11.2: the context its last request used (none before one, for a sleeping chat, or a peer that
    /// doesn't say) and its context budget (tokens; 0 when unknown), numbers only. A peer's are cut to
    /// [`MAX_CARD_TOKENS`] on receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_used: Option<u64>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub context_budget: u64,
}

/// The most tokens a card's context numbers say (10M: past any window there is): a peer's larger number is cut to it.
pub const MAX_CARD_TOKENS: u64 = 10_000_000;

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl Card {
    /// The card's context numbers as they may be shown, (used, budget): each cut to [`MAX_CARD_TOKENS`], used none
    /// before a request, and none at all without a budget (a used count alone says nothing about room).
    pub fn context(&self) -> Option<(Option<u64>, u64)> {
        let cut = |n: u64| n.min(MAX_CARD_TOKENS);
        (self.context_budget > 0).then(|| (self.context_used.map(cut), cut(self.context_budget)))
    }

    /// Whether it carries any of the person's words (a title or a latest message).
    pub fn has_words(&self) -> bool {
        !self.title.is_empty() || !self.now.is_empty()
    }
}

/// One agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: AgentKey,
    pub mesh_name: String,
    /// Where it works, as it said (a peer's `marv/info`) or as the desktop knows it (a chat's folder).
    pub folder: PathBuf,
    pub kind: AgentKind,
    /// Its session's state: a desktop's own chats' as this Marv schedules them, another Marv's chat's as its door says
    /// it (`marv/info`'s `state`); `None` for a terminal Marv, or an older one's door.
    #[serde(default)]
    pub state: Option<AgentState>,
    /// Where it can be reached, in the order it said (its inboxes as its door lists its agents), each once.
    #[serde(default)]
    pub capabilities: Vec<Reach>,
    /// Its chat card, when it has one (a desktop chat's, a TUI's from its `marv/info`); missing from older peers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<Card>,
    /// The paths its edit tools wrote in the last 30 minutes (runtime-kernel §14.4, [`crate::writes`]): a desktop
    /// chat's own, a peer's from its `marv/info` (cleaned on receipt). Shown, never consulted by anything that admits.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writes: Vec<crate::writes::RecentWrite>,
}

impl Entry {
    /// An entry with no state and nothing to reach it at yet.
    pub fn new(id: AgentKey, mesh_name: impl Into<String>, folder: impl Into<PathBuf>, kind: AgentKind) -> Self {
        Self {
            id,
            mesh_name: mesh_name.into(),
            folder: folder.into(),
            kind,
            state: None,
            capabilities: Vec::new(),
            card: None,
            writes: Vec::new(),
        }
    }

    pub fn can(&self, reach: &Reach) -> bool {
        self.capabilities.contains(reach)
    }
}

/// The agents and their indexes. Every change goes through [`Registry::upsert`], [`Registry::remove`] or
/// [`Registry::set_state`], which keep the indexes in step with the entries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Registry {
    entries: BTreeMap<AgentKey, Entry>,
    /// Mesh name → key: one agent per name.
    by_name: BTreeMap<String, AgentKey>,
    /// Folder → keys. `PathBuf` orders by component, so a folder's subtree is the range from it to the first path
    /// that doesn't start with it (`/a/b`, `/a/b/c`, … then `/a/bc`).
    by_folder: BTreeMap<PathBuf, BTreeSet<AgentKey>>,
    by_capability: BTreeMap<Reach, BTreeSet<AgentKey>>,
    /// Space roots → the keys under them (an entry is in every space whose root contains its folder).
    by_space: BTreeMap<PathBuf, BTreeSet<AgentKey>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, id: &AgentKey) -> Option<&Entry> {
        self.entries.get(id)
    }

    /// The agent with this mesh name.
    pub fn named(&self, mesh_name: &str) -> Option<&Entry> {
        self.by_name.get(mesh_name).and_then(|id| self.entries.get(id))
    }

    /// Every entry, by key.
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }

    /// Every entry, by mesh name (how `marv mesh ls` and `mesh_list` order Marvs).
    pub fn by_mesh_name(&self) -> impl Iterator<Item = &Entry> {
        self.by_name.values().filter_map(|id| self.entries.get(id))
    }

    /// Adds or replaces the entry under its key; returns the one it replaced. A mesh name names one agent: another
    /// entry holding `entry`'s name goes (a door's name is unique among live doors, so it was that name's last
    /// holder). Repeated capabilities are kept once.
    pub fn upsert(&mut self, mut entry: Entry) -> Option<Entry> {
        let mut seen = BTreeSet::new();
        entry.capabilities.retain(|c| seen.insert(c.clone()));
        if let Some(holder) = self.by_name.get(&entry.mesh_name).filter(|h| **h != entry.id).cloned() {
            self.remove(&holder);
        }
        let old = self.remove(&entry.id);
        let id = entry.id.clone();
        self.by_name.insert(entry.mesh_name.clone(), id.clone());
        self.by_folder.entry(entry.folder.clone()).or_default().insert(id.clone());
        for reach in &entry.capabilities {
            self.by_capability.entry(reach.clone()).or_default().insert(id.clone());
        }
        for (root, keys) in self.by_space.iter_mut() {
            if entry.folder.starts_with(root) {
                keys.insert(id.clone());
            }
        }
        self.entries.insert(id, entry);
        old
    }

    /// Removes the entry, and it from every index.
    pub fn remove(&mut self, id: &AgentKey) -> Option<Entry> {
        let entry = self.entries.remove(id)?;
        if self.by_name.get(&entry.mesh_name) == Some(id) {
            self.by_name.remove(&entry.mesh_name);
        }
        unindex(&mut self.by_folder, &entry.folder, id);
        for reach in &entry.capabilities {
            unindex(&mut self.by_capability, reach, id);
        }
        for keys in self.by_space.values_mut() {
            keys.remove(id);
        }
        Some(entry)
    }

    /// Sets an entry's state; `false` when there's no such entry. (State isn't indexed.)
    pub fn set_state(&mut self, id: &AgentKey, state: Option<AgentState>) -> bool {
        match self.entries.get_mut(id) {
            Some(entry) => {
                entry.state = state;
                true
            }
            None => false,
        }
    }

    /// The agents working in `scope`'s subtree, by folder: component-wise, so `/a/b` covers `/a/b` and `/a/b/c`, never
    /// `/a/bc`.
    pub fn under(&self, scope: &FolderScope) -> Vec<&Entry> {
        self.subtree(&self.by_folder, scope.path())
    }

    /// The agents that can be reached at `reach`, by key.
    pub fn with(&self, reach: &Reach) -> Vec<&Entry> {
        self.by_capability.get(reach).into_iter().flatten().filter_map(|id| self.entries.get(id)).collect()
    }

    /// Indexes a space by its root from now on (the desktop's tabs). A chat has no space id: it is in every space
    /// whose root contains its folder (`docs/architecture/runtime-kernel.md` §2.1).
    pub fn add_space(&mut self, root: &FolderScope) {
        let root = root.path().to_path_buf();
        let keys = self.subtree(&self.by_folder, &root).into_iter().map(|e| e.id.clone()).collect();
        self.by_space.insert(root, keys);
    }

    pub fn remove_space(&mut self, root: &Path) {
        self.by_space.remove(root);
    }

    /// The space roots indexed.
    pub fn spaces(&self) -> impl Iterator<Item = &Path> {
        self.by_space.keys().map(PathBuf::as_path)
    }

    /// The agents in the space rooted at `root` (none when it isn't indexed), by key.
    pub fn in_space(&self, root: &Path) -> Vec<&Entry> {
        self.by_space.get(root).into_iter().flatten().filter_map(|id| self.entries.get(id)).collect()
    }

    fn subtree(&self, index: &BTreeMap<PathBuf, BTreeSet<AgentKey>>, root: &Path) -> Vec<&Entry> {
        index
            .range::<Path, _>((std::ops::Bound::Included(root), std::ops::Bound::Unbounded))
            .take_while(|(folder, _)| folder.starts_with(root))
            .flat_map(|(_, keys)| keys)
            .filter_map(|id| self.entries.get(id))
            .collect()
    }

    /// Checks that every index says exactly what the entries do (tests; a property test runs it after each step).
    pub fn consistent(&self) -> Result<(), String> {
        let mut rebuilt = Registry::default();
        for root in self.by_space.keys() {
            rebuilt.by_space.insert(root.clone(), BTreeSet::new());
        }
        for entry in self.entries.values() {
            rebuilt.upsert(entry.clone());
        }
        let names: BTreeSet<&String> = self.entries.values().map(|e| &e.mesh_name).collect();
        if names.len() != self.entries.len() {
            return Err("two entries share a mesh name".into());
        }
        for (what, ok) in [
            ("by_name", rebuilt.by_name == self.by_name),
            ("by_folder", rebuilt.by_folder == self.by_folder),
            ("by_capability", rebuilt.by_capability == self.by_capability),
            ("by_space", rebuilt.by_space == self.by_space),
        ] {
            if !ok {
                return Err(format!("{what} doesn't match the entries"));
            }
        }
        Ok(())
    }
}

/// Takes `id` off `key`'s set, dropping the set once empty (so an index never holds empty keys).
fn unindex<K: Ord + Clone>(index: &mut BTreeMap<K, BTreeSet<AgentKey>>, key: &K, id: &AgentKey) {
    if let Some(keys) = index.get_mut(key) {
        keys.remove(id);
        if keys.is_empty() {
            index.remove(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/home/me";

    fn scope(path: &str) -> FolderScope {
        FolderScope::new(Path::new(path), Path::new(HOME)).unwrap()
    }

    fn chat(n: u32, folder: &str) -> Entry {
        let id = AgentId::new(&format!("{n:08x}-0000-4000-8000-000000000000")).unwrap();
        let name = format!("c-{n:08x}");
        Entry::new(AgentKey::Chat(id), name, folder, AgentKind::Chat)
    }

    fn terminal(name: &str, folder: &str) -> Entry {
        Entry::new(AgentKey::Mesh(name.into()), name, folder, AgentKind::Terminal)
    }

    fn names(entries: Vec<&Entry>) -> Vec<String> {
        let mut names: Vec<String> = entries.into_iter().map(|e| e.mesh_name.clone()).collect();
        names.sort();
        names
    }

    #[test]
    fn subtree_queries_are_component_wise() {
        let mut r = Registry::new();
        r.upsert(terminal("b", "/home/me/a/b"));
        r.upsert(terminal("bc", "/home/me/a/bc"));
        r.upsert(terminal("deep", "/home/me/a/b/c/d"));
        r.upsert(terminal("a", "/home/me/a"));
        r.upsert(terminal("b-dash", "/home/me/a/b-x"));
        assert_eq!(names(r.under(&scope("/home/me/a/b"))), ["b", "deep"]);
        assert_eq!(names(r.under(&scope("/home/me/a/bc"))), ["bc"]);
        assert_eq!(names(r.under(&scope("/home/me/a"))), ["a", "b", "b-dash", "bc", "deep"]);
        assert!(r.under(&scope("/home/me/a/b/c/d/e")).is_empty());
        r.consistent().unwrap();
    }

    #[test]
    fn capabilities_and_spaces_are_indexed_and_follow_updates_and_removals() {
        let mut r = Registry::new();
        r.add_space(&scope("/home/me/acme"));
        let mut a = chat(1, "/home/me/acme/api");
        a.capabilities = vec![Reach::Inbox("parent".into()), Reach::Radio, Reach::Inbox("parent".into())];
        r.upsert(a.clone());
        assert_eq!(r.get(&a.id).unwrap().capabilities.len(), 2, "kept once each");
        r.upsert(terminal("web", "/home/me/web"));
        assert_eq!(names(r.with(&Reach::Radio)), ["c-00000001"]);
        assert_eq!(names(r.in_space(Path::new("/home/me/acme"))), ["c-00000001"]);
        // A space added after its agents indexes them.
        r.add_space(&scope("/home/me/web"));
        assert_eq!(names(r.in_space(Path::new("/home/me/web"))), ["web"]);

        // The chat moves out of acme and off the radio.
        let mut moved = a.clone();
        moved.folder = "/home/me/web/front".into();
        moved.capabilities = vec![Reach::Inbox("parent".into())];
        assert_eq!(
            r.upsert(moved).unwrap(),
            Entry { capabilities: vec![Reach::Inbox("parent".into()), Reach::Radio], ..a.clone() }
        );
        assert!(r.with(&Reach::Radio).is_empty());
        assert!(r.in_space(Path::new("/home/me/acme")).is_empty());
        assert_eq!(names(r.in_space(Path::new("/home/me/web"))), ["c-00000001", "web"]);
        assert!(r.set_state(&a.id, Some(AgentState::Idle)));
        assert_eq!(r.named("c-00000001").unwrap().state, Some(AgentState::Idle));
        r.consistent().unwrap();

        r.remove(&a.id).unwrap();
        assert!(r.named("c-00000001").is_none() && r.with(&Reach::Inbox("parent".into())).is_empty());
        assert!(!r.set_state(&a.id, None));
        r.remove_space(Path::new("/home/me/acme"));
        assert_eq!(r.spaces().collect::<Vec<_>>(), [Path::new("/home/me/web")]);
        r.consistent().unwrap();
    }

    #[test]
    fn a_mesh_name_names_one_agent() {
        let mut r = Registry::new();
        r.upsert(terminal("api", "/srv/old"));
        let mut chat = chat(7, "/srv/new");
        chat.mesh_name = "api".into();
        r.upsert(chat.clone());
        assert_eq!(r.len(), 1);
        assert_eq!(r.named("api").unwrap().id, chat.id, "the name's newest holder");
        assert!(r.get(&AgentKey::Mesh("api".into())).is_none());
        r.consistent().unwrap();
    }

    #[test]
    fn keys_serialize_tagged() {
        let id = AgentKey::Chat(AgentId::new("554963ee-fd8e-4bec-a7dc-49051f090acc").unwrap());
        assert_eq!(serde_json::to_string(&id).unwrap(), r#"{"chat":"554963ee-fd8e-4bec-a7dc-49051f090acc"}"#);
        assert_eq!(serde_json::to_string(&AgentKey::Mesh("api-3f9a".into())).unwrap(), r#"{"mesh":"api-3f9a"}"#);
        assert!(serde_json::from_str::<AgentKey>(r#"{"chat":"../x"}"#).is_err());
    }

    /// Chat card: an entry without a card (an older peer's, or the event log's) still parses, a card tolerates a newer
    /// Marv's extra field (kept to its own), and an entry without one serializes as before.
    #[test]
    fn entries_parse_with_and_without_a_card() {
        let plain = terminal("api-3f9a", "/w");
        let text = serde_json::to_string(&plain).unwrap();
        assert!(!text.contains("card"), "{text}");
        assert_eq!(serde_json::from_str::<Entry>(&text).unwrap(), plain);
        let card = Card {
            title: "Set up the Rust API skeleton".into(),
            now: "add auth middleware".into(),
            last_active_ms: 7,
            ..Default::default()
        };
        let with = Entry { card: Some(card.clone()), ..plain.clone() };
        let text = serde_json::to_string(&with).unwrap();
        assert_eq!(serde_json::from_str::<Entry>(&text).unwrap().card, Some(card));
        let newer = serde_json::from_str::<Card>(r#"{"title":"t","mood":"new field"}"#).unwrap();
        assert_eq!(newer, Card { title: "t".into(), ..Card::default() }, "a newer field is dropped, the card kept");
        assert!(!serde_json::to_string(&newer).unwrap().contains("mood"));
        assert_eq!(serde_json::from_str::<Card>("{}").unwrap(), Card::default());
        assert!(!Card::default().has_words() && Card { now: "x".into(), ..Card::default() }.has_words());
    }

    /// A small deterministic generator (xorshift), so the property test needs no crate.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self, n: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % n
        }
    }

    /// Property: after any sequence of upserts, removals, state changes and spaces, every index matches the entries,
    /// and each query matches a brute-force filter over them.
    #[test]
    fn random_changes_keep_every_index_consistent() {
        const FOLDERS: [&str; 7] =
            ["/home/me/a", "/home/me/a/b", "/home/me/a/bc", "/home/me/a/b/c", "/home/me/x", "/home/me/x/y", "/srv/z"];
        let reaches =
            [Reach::Inbox("parent".into()), Reach::Inbox("worker-1".into()), Reach::Radio, Reach::HerdrPane("w1".into())];
        for seed in 1..=40u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
            let mut r = Registry::new();
            for _ in 0..200 {
                let n = rng.next(12) as u32;
                let key_entry = if n.is_multiple_of(2) { chat(n, FOLDERS[0]) } else { terminal(&format!("t{n}"), FOLDERS[0]) };
                match rng.next(6) {
                    0..=2 => {
                        let mut e = key_entry;
                        e.folder = FOLDERS[rng.next(FOLDERS.len() as u64) as usize].into();
                        if rng.next(4) == 0 {
                            e.mesh_name = format!("shared{}", rng.next(3));
                        }
                        e.capabilities = reaches.iter().filter(|_| rng.next(2) == 0).cloned().collect();
                        r.upsert(e);
                    }
                    3 => {
                        r.remove(&key_entry.id);
                    }
                    4 => {
                        r.set_state(&key_entry.id, Some(AgentState::Busy));
                    }
                    _ => {
                        let root = FOLDERS[rng.next(FOLDERS.len() as u64) as usize];
                        if rng.next(2) == 0 {
                            r.add_space(&scope(root));
                        } else {
                            r.remove_space(Path::new(root));
                        }
                    }
                }
                r.consistent().unwrap_or_else(|e| panic!("seed {seed}: {e}"));
                for folder in FOLDERS {
                    let want: Vec<String> = names(r.entries().filter(|e| e.folder.starts_with(folder)).collect());
                    assert_eq!(names(r.under(&scope(folder))), want, "seed {seed}, under {folder}");
                }
                for reach in &reaches {
                    assert_eq!(names(r.with(reach)), names(r.entries().filter(|e| e.can(reach)).collect()));
                }
                for root in r.spaces().map(Path::to_path_buf).collect::<Vec<_>>() {
                    let want = names(r.entries().filter(|e| e.folder.starts_with(&root)).collect());
                    assert_eq!(names(r.in_space(&root)), want, "seed {seed}, space {}", root.display());
                }
            }
        }
    }
}
