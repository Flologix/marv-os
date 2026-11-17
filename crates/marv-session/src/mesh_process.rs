//! A process's place on the mesh (moved from marv-cli's `mesh_cmd` so any SDK host can use it): its switchboard,
//! named by `marv_mesh::discovery::mesh_name`, with Herdr's agent panes as receptacles when Marv runs in a Herdr
//! pane, and its door ([`Mesh::open_door`]) at `<runtime>/mesh/<name>.sock`. No runtime folder, no
//! mesh (never a `/tmp` fallback).

use std::path::Path;
use std::sync::Arc;

use marv_mesh::discovery::{DoorGuard, DoorInfo, check_session_name, mesh_dir_for, mesh_name, open_door, session_socket};
use marv_mesh::herdr::{HerdrAdapter, herdr_target};
use marv_mesh::radio::{Presence, Radio, RadioTimings};
use tokio_util::sync::CancellationToken;

use crate::mesh::{DoorWires, MeshHandle, WireApprover};

/// The process's mesh while it runs: its switchboard, served until this drops, and its door once opened.
pub struct Mesh {
    pub handle: MeshHandle,
    pub name: String,
    /// The desktop chat this is (its id), told to other Marvs in `marv/info`; set before `open_door`.
    pub chat: Option<String>,
    root: std::path::PathBuf,
    pane: Option<String>,
    dir: std::path::PathBuf,
    door: Option<DoorGuard>,
    /// The radio (#124), the TUI's only ([`Mesh::start_radio`]).
    radio: Option<Arc<Radio>>,
    cancel: CancellationToken,
}

impl Drop for Mesh {
    fn drop(&mut self) {
        if let Some(radio) = self.radio.take() {
            radio.stop();
        }
        self.door.take();
        self.cancel.cancel();
    }
}

/// How long a given name is waited for while a live Marv still has it (`GIVEN_NAME_WAITS` × `GIVEN_NAME_WAIT`).
const GIVEN_NAME_WAITS: u32 = 20;
const GIVEN_NAME_WAIT: std::time::Duration = std::time::Duration::from_millis(100);

/// This process's switchboard (`None` without a runtime folder, `marv_sandbox::runtime_dir`). An error names what's wrong (a bad
/// `MARV_MESH_NAME`, a name a live Marv has); the caller carries on without the mesh. Needs a Tokio runtime.
/// `outside`: the project and the sandbox's writable folders, where the `herdr` the pane receptacles run is never taken
/// from (`herdr_program`).
///
/// `name`: the name to take (a desktop chat's, from its id: `marv_kernel::mesh_name_for`), checked like
/// `MARV_MESH_NAME`. A live Marv with that name is an error, never a silent other name: the name is how wires find this
/// agent again.
pub fn start(
    env: &marv_mcp::Env,
    root: &Path,
    outside: &[std::path::PathBuf],
    name: Option<&str>,
) -> Result<Option<Mesh>, String> {
    // In the project, a MARV_RUNTIME_DIR is refused (#168): the caller says why (`marv_sandbox::override_refusal_for`).
    let Some(dir) = mesh_dir_for(env, Some(root)) else { return Ok(None) };
    let given = name.is_some();
    let chosen = given || env.contains_key("MARV_MESH_NAME");
    let mut name = match name {
        Some(name) => {
            check_session_name(name, "this chat's mesh name").map_err(|e| e.to_string())?;
            name.to_string()
        }
        None => mesh_name(env, root).map_err(|e| e.to_string())?,
    };
    // Never a blocking connect (kernel #41): a hung Marv's door with a full backlog would hold this start forever (and a
    // desktop's chats with it, the window never opening). A listener that doesn't accept is still a live Marv's name.
    let live = |name: &str| session_socket(&dir, name).is_some_and(|s| marv_mesh::discovery::someone_listens(&s));
    // A given name may still be held by this same chat's last session, closing (a restart after its servers changed):
    // its door goes within moments, so wait for it a little before calling the name taken.
    if given {
        for _ in 0..GIVEN_NAME_WAITS {
            if !live(&name) {
                break;
            }
            std::thread::sleep(GIVEN_NAME_WAIT);
        }
    }
    // A generated name that a live Marv already has: try another (the suffix changes with the clock).
    for _ in 0..5 {
        if !live(&name) {
            break;
        }
        if chosen {
            return Err(format!(
                "a Marv named \"{name}\" is already on the mesh (this chat open twice, or MARV_MESH_NAME taken)"
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
        name = mesh_name(env, root).map_err(|e| e.to_string())?;
    }
    let cancel = CancellationToken::new();
    let handle = crate::mesh::start(&name, env, Some(root), cancel.clone()).ok_or("no runtime folder")?;
    let pane = herdr_target(env).map(|t| t.pane_id);
    // No herdr found the safe way: no Herdr receptacles, rather than a PATH lookup that could run a planted one.
    if let (Some(own), Some(program)) = (&pane, herdr_program(env, outside)) {
        handle.set_herdr(Arc::new(HerdrAdapter::new(program, Some(own.clone()))));
    }
    Ok(Some(Mesh { handle, name, chat: None, root: root.to_path_buf(), pane, dir, door: None, radio: None, cancel }))
}

impl Mesh {
    /// Puts this Marv on the mesh: other processes find it (`marv mesh ls`) and ask it for wires, which `wires`
    /// answers (the session's person, or `--approve mesh:<r>`).
    pub fn open_door(&mut self, wires: Arc<dyn WireApprover>) -> Result<(), String> {
        let info = DoorInfo { cwd: self.root.clone(), pane: self.pane.clone(), chat: self.chat.clone() };
        let door = open_door(self.handle.clone(), &self.dir, info, Arc::new(DoorWires(wires))).map_err(|e| e.to_string())?;
        self.door = Some(door);
        self.handle.set_identity(crate::mesh::parent_id(), self.identity());
        Ok(())
    }

    /// Logs this Marv's kernel events to `log` from now on (runtime-kernel 3): its messages, rule wires and wakes,
    /// and its own entry in its registry. For a desktop chat or the TUI; never `marv -p`.
    pub fn log_events(&self, log: Arc<dyn marv_kernel::EventLog>) {
        self.handle.set_events(log.clone());
        self.handle.registry().set_events(log);
    }

    /// Lists the Marvs from `registry` (runtime-kernel 3) and, once the door is open, enters this one there as the
    /// process's own (authoritative: a desktop's chats share one registry). Data only: it admits nothing. False when
    /// the door isn't open (nothing entered).
    pub fn register(&self, registry: &Arc<marv_mesh::registry::MeshRegistry>) -> bool {
        self.handle.set_registry(registry.clone());
        if self.door.is_none() {
            return false;
        }
        registry.register_own(&self.handle, &self.identity(), &self.dir, self.pane.clone());
        true
    }

    /// Who the main agent is, as a rule would see it (runtime-kernel 2b): this chat's id, or none for a terminal Marv;
    /// its mesh name and folder. What the door tells others in `marv/info`. Data only: it admits nothing.
    pub fn identity(&self) -> marv_kernel::AgentRef {
        let id = self.chat.as_deref().and_then(marv_kernel::AgentId::new);
        let kind = if id.is_some() { marv_kernel::AgentKind::Chat } else { marv_kernel::AgentKind::Terminal };
        marv_kernel::AgentRef { id, mesh_name: self.name.clone(), folder: self.root.clone(), kind }
    }

    /// Honors the person's rules in `<config_dir>/mesh/policies.json` (runtime-kernel 2b): a rule they made may wire
    /// this Marv's main agent to another Marv's without asking, and what a rule wired ends when it's revoked. For a
    /// desktop chat or the TUI (D8); never `marv -p`. `config_dir` is the user's own (absolute), never a project's.
    pub fn honor_rules(&self, config_dir: &Path, home: &Path) -> Result<(), String> {
        let store = marv_kernel::PolicyStore::new(config_dir, home).map_err(|e| e.to_string())?;
        self.handle.set_rules(Arc::new(marv_kernel::FileRules::new(store)));
        // Runtime-kernel §19: the rules' team boards, beside them.
        let boards = marv_kernel::BoardStore::new(config_dir, home).map_err(|e| e.to_string())?;
        self.handle.set_boards(Arc::new(boards));
        Ok(())
    }

    /// The radio (#124) for this Marv's open door, off until the caller turns it on. The TUI's alone: `marv -p` never
    /// makes one, so its door refuses every join.
    pub fn start_radio(&mut self, wake: bool) -> Arc<Radio> {
        let me = Presence::new(&self.name, &self.root, self.pane.clone(), wake);
        let radio = Radio::new(&self.handle, &self.dir, me, RadioTimings::default());
        radio.start();
        self.radio = Some(radio.clone());
        radio
    }
}

/// The `herdr` the mesh runs (pane receptacles, `marv mesh ls`), the doors' way: from an absolute PATH entry only, and
/// never from inside `outside` (`marv_sandbox::host_env::host_program`). None: Marv offers no Herdr panes.
pub fn herdr_program(env: &marv_mcp::Env, outside: &[std::path::PathBuf]) -> Option<std::path::PathBuf> {
    let outside: Vec<&Path> = outside.iter().map(std::path::PathBuf::as_path).collect();
    marv_sandbox::host_env::host_program("herdr", env.get("PATH").map(String::as_str), &outside)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A short runtime folder (socket paths stay under macOS's 104 bytes), removed on drop.
    struct Runtime(std::path::PathBuf);
    impl Drop for Runtime {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn runtime() -> (Runtime, marv_mcp::Env) {
        let dir = std::path::PathBuf::from(format!("/tmp/mk-{}-{}", std::process::id(), marv_fs::random_hex()));
        std::fs::create_dir_all(&dir).unwrap();
        let env = [("XDG_RUNTIME_DIR", dir.to_str().unwrap()), ("HOME", "/nonexistent-home")]
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .into_iter()
            .collect();
        (Runtime(dir), env)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_given_name_is_taken_as_is_and_a_live_marv_with_it_is_an_error_not_another_name() {
        let (runtime, env) = runtime();
        let root = runtime.0.join("project");
        std::fs::create_dir_all(&root).unwrap();
        let mesh = start(&env, &root, &[], Some("circuit-554963ee")).unwrap().unwrap();
        assert_eq!(mesh.name, "circuit-554963ee");
        drop(mesh);

        // A live Marv already answers under that name (the same chat open twice).
        let dir = mesh_dir_for(&env, Some(&root)).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let _live = std::os::unix::net::UnixListener::bind(session_socket(&dir, "circuit-554963ee").unwrap()).unwrap();
        let taken = start(&env, &root, &[], Some("circuit-554963ee")).err().unwrap();
        assert!(taken.contains("already on the mesh"), "{taken}");

        assert!(start(&env, &root, &[], Some("Not A Name")).err().unwrap().contains("isn't a mesh name"));

        // The chat's last session still closing: its socket goes during the wait, and the name is taken, not refused.
        let socket = session_socket(&dir, "circuit-554963ee").unwrap();
        let closing = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(250));
            drop(_live);
            std::fs::remove_file(socket).unwrap();
        });
        let mesh = start(&env, &root, &[], Some("circuit-554963ee")).unwrap().unwrap();
        assert_eq!(mesh.name, "circuit-554963ee");
        closing.join().unwrap();
        let generated = start(&env, &root, &[], None).unwrap().unwrap();
        assert!(generated.name.starts_with("project-") && generated.name.len() == "project-".len() + 4, "{}", generated.name);
    }

    /// Kernel #41: a door whose owner stopped accepting (a hung desktop: its listen backlog full) is a live name, said
    /// within the given-name wait, never a start that blocks on the connect forever.
    ///
    /// Proven: the old blocking `UnixStream::connect` probe → this test timed out.
    #[tokio::test]
    async fn a_hung_door_with_a_full_backlog_never_blocks_a_start() {
        use socket2::{Domain, SockAddr, Socket, Type};
        let (runtime, env) = runtime();
        let root = runtime.0.join("project");
        std::fs::create_dir_all(&root).unwrap();
        let dir = mesh_dir_for(&env, Some(&root)).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let socket = session_socket(&dir, "circuit-554963ee").unwrap();
        // A listener that never accepts, its backlog filled until a connect would block.
        let hung = Socket::new(Domain::UNIX, Type::STREAM, None).unwrap();
        hung.bind(&SockAddr::unix(&socket).unwrap()).unwrap();
        hung.listen(1).unwrap();
        let mut queued = Vec::new();
        loop {
            let c = Socket::new(Domain::UNIX, Type::STREAM, None).unwrap();
            c.set_nonblocking(true).unwrap();
            let full = c.connect(&SockAddr::unix(&socket).unwrap()).is_err();
            queued.push(c);
            if full || queued.len() > 4096 {
                break;
            }
        }
        let (env2, root2) = (env.clone(), root.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(start(&env2, &root2, &[], Some("circuit-554963ee")).err());
        });
        let err = rx.recv_timeout(std::time::Duration::from_secs(10)).expect("start blocked on a hung door").unwrap();
        assert!(err.contains("already on the mesh"), "{err}");
        drop((hung, queued));
    }

    /// Runtime-kernel 2b: the main agent's identity comes from the session (its chat, name and folder), recorded on the
    /// switchboard when the door opens. Data only.
    #[tokio::test]
    async fn the_main_agents_identity_comes_from_the_session() {
        let (runtime, env) = runtime();
        let root = runtime.0.join("project");
        std::fs::create_dir_all(&root).unwrap();
        let mut chat = start(&env, &root, &[], Some("project-554963ee")).unwrap().unwrap();
        chat.chat = Some("554963ee-fd8e-4bec-a7dc-49051f090acc".into());
        chat.open_door(Arc::new(crate::mesh::HeadlessWires(Vec::new()))).unwrap();
        let who = chat.handle.identity(&crate::mesh::parent_id()).unwrap();
        assert_eq!(
            (who.mesh_name.as_str(), who.folder.as_path(), who.kind),
            ("project-554963ee", root.as_path(), marv_kernel::AgentKind::Chat)
        );
        assert_eq!(who.id.unwrap().as_str(), "554963ee-fd8e-4bec-a7dc-49051f090acc");
        let mut terminal = start(&env, &root, &[], None).unwrap().unwrap();
        terminal.open_door(Arc::new(crate::mesh::HeadlessWires(Vec::new()))).unwrap();
        let who = terminal.handle.identity(&crate::mesh::parent_id()).unwrap();
        assert_eq!((who.id, who.kind), (None, marv_kernel::AgentKind::Terminal));
    }
}
