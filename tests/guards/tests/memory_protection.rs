//! Runtime-kernel §14 (memory protection between processes, `docs/architecture/runtime-kernel.md` §14.6). Two chats in
//! one folder write the same files:
//! - **P2/P3** (public, `stale_writes.rs` guards them for every Marv): an edit or write over a change the chat hasn't
//!   read is refused, the check and write are one step under a short cross-process lock, and no lock outlives a call.
//!   Here they're guarded as the kernel wires them: with the chat's write set observing the edit tools
//!   (`marv_tools::WriteObserver`, `marv_session::mesh::MeshWrites`), a refused call is never a write, and the
//!   observer is told only after the lock is let go.
//! - **P4** (private): observed write sets. Two chats writing one path is shown as **paths, never content**: the
//!   desktop reads the shared paths, a covered peer's `mesh_list` lists them (fenced), and the event log gets a
//!   `WriteOverlap` with who and how many, never a path or a byte of a file.
//! - **P1** (private): granted chats sharing a folder default to their own git worktrees.

use std::fs::File;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use marv_guards::{Scripted, TempDir, call, headless_steps, say, tool_call, use_tools};
use marv_mesh::approval::WireRequest;
use marv_mesh::discovery::{DoorGuard, DoorInfo, PeerApprover, open_door};
use marv_mesh::registry::MeshRegistry;
use marv_mesh::switchboard::{NoTrajectory, Switchboard};
use marv_mesh::wire::Approver;
use marv_oracle::script::ScriptCall;
use marv_tools::file_lock::lock_path;
use marv_tools::{ToolContext, WriteObserver, builtin_tools, run_tool};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

fn script(id: &str, name: &str, arguments: Value) -> ScriptCall {
    ScriptCall { id: Some(id.into()), name: name.into(), arguments }
}

/// The paths a chat's edit tools reported writing, in order.
#[derive(Default)]
struct Seen(Mutex<Vec<PathBuf>>);
impl WriteObserver for Seen {
    fn wrote(&self, path: &Path) {
        self.0.lock().unwrap().push(path.to_path_buf());
    }
}
impl Seen {
    fn paths(&self) -> Vec<PathBuf> {
        self.0.lock().unwrap().clone()
    }
}

/// A chat in `root` that says yes to every change (only the stale check can stop it), its writes observed.
fn chat(root: &Path, seen: &Arc<Seen>) -> ToolContext {
    ToolContext { yolo: true, writes: Some(seen.clone() as Arc<dyn WriteObserver>), ..ToolContext::new(root) }
}

async fn run(ctx: &ToolContext, name: &str, args: Value) -> marv_tools::ToolOutput {
    run_tool(&call(name, args), ctx, builtin_tools()).await.result
}

/// **An edit after another chat's write is refused until the file is read again** (two sessions, two processes). This
/// chat reads `a.txt`; another Marv process (`marv -p`) reads and writes it; this chat's edit, built on the old text,
/// is refused, writes nothing and isn't a write in its write set. After reading the file again the edit goes through
/// and is.
///
/// Proven: removed the `file_versions.check` from `edit_file::plan` → failed (the edit dropped the other process's
/// line).
#[tokio::test]
async fn an_edit_after_another_chats_write_is_refused_until_reread() {
    let (root, config) = (TempDir::new("mp-stale"), TempDir::new("mp-stale-config"));
    root.write("a.txt", "one\ntwo\n");
    let seen = Arc::new(Seen::default());
    let ctx = chat(root.path(), &seen);
    assert!(!run(&ctx, "read_file", json!({"path": "a.txt"})).await.is_error);

    let other = vec![
        vec![script("r1", "read_file", json!({"path": "a.txt"}))],
        vec![script("w1", "write_file", json!({"path": "a.txt", "content": "one\ntwo\ntheirs\n"}))],
    ];
    let out = headless_steps(root.path(), config.path(), other, &["--approve", "files"], &[("MARV_GITHUB", "off")]);
    assert_eq!(out.code, Some(0), "{}\n{}", out.result, out.stderr);

    let edit = json!({"path": "a.txt", "old_string": "two", "new_string": "TWO"});
    let refused = run(&ctx, "edit_file", edit.clone()).await;
    assert!(refused.is_error && refused.output.contains("changed since you last read it"), "{}", refused.output);
    assert_eq!(root.read("a.txt").as_deref(), Some("one\ntwo\ntheirs\n"), "nothing written over theirs");
    assert!(seen.paths().is_empty(), "a refused edit is no write");

    assert!(!run(&ctx, "read_file", json!({"path": "a.txt"})).await.is_error);
    assert!(!run(&ctx, "edit_file", edit).await.is_error);
    assert_eq!(root.read("a.txt").as_deref(), Some("one\nTWO\ntheirs\n"));
    assert_eq!(seen.paths(), [root.path().join("a.txt")]);
}

/// **An existing file the chat never read can't be overwritten**, nor edited, and the refusal is no write; a new file
/// needs no read and is one.
///
/// Proven: let `FileVersions::check` pass a file it has no hash for → failed (`keep.txt` was overwritten).
#[tokio::test]
async fn an_existing_file_never_read_cant_be_overwritten() {
    let root = TempDir::new("mp-unread");
    root.write("keep.txt", "the user's text\n");
    root.write("keep2.txt", "the user's text\n");
    let seen = Arc::new(Seen::default());
    let ctx = chat(root.path(), &seen);
    // The first call on each is refused (and shows the file as it is now: `check_showing`, which records that version
    // as seen, so a call after it may succeed; that is the model having read it, one file per call here).
    assert!(run(&ctx, "write_file", json!({"path": "keep.txt", "content": "clobbered\n"})).await.is_error);
    assert!(run(&ctx, "edit_file", json!({"path": "keep2.txt", "old_string": "user", "new_string": "model"})).await.is_error);
    assert_eq!(root.read("keep.txt").as_deref(), Some("the user's text\n"));
    assert_eq!(root.read("keep2.txt").as_deref(), Some("the user's text\n"));
    assert!(seen.paths().is_empty());
    assert!(!run(&ctx, "write_file", json!({"path": "new.txt", "content": "first\n"})).await.is_error);
    assert_eq!(seen.paths(), [root.path().join("new.txt")]);
}

/// **The check and the write are atomic across processes.** While another process holds `new.txt`'s lock (this test,
/// on the lock file: an `flock` belongs to an open file, so a second one conflicts as another process's does), the
/// chat's `write_file` of the new file waits; the other creates the file meanwhile and lets go. The chat then checks
/// the file as it is now, finds an existing file it never read, and writes nothing.
///
/// Proven: dropped the cross-process lock from `with_file_lock` (`file_locks` ignored) → failed (the chat created
/// `new.txt` while the lock was held).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_check_and_write_are_atomic_across_processes() {
    let (root, locks) = (TempDir::new("mp-atomic"), TempDir::new("mp-atomic-locks"));
    let seen = Arc::new(Seen::default());
    let ctx = ToolContext { file_locks: Some(locks.path().to_path_buf()), ..chat(root.path(), &seen) };
    let theirs = File::create(lock_path(locks.path(), &root.path().join("new.txt"))).unwrap();
    theirs.lock().unwrap();

    let write = tokio::spawn(async move { run(&ctx, "write_file", json!({"path": "new.txt", "content": "the chat's\n"})).await });
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(root.read("new.txt"), None, "the chat wrote while another process held the lock");
    std::fs::write(root.path().join("new.txt"), "theirs\n").unwrap();
    theirs.unlock().unwrap();

    let out = write.await.unwrap();
    assert!(out.is_error, "{}", out.output);
    assert_eq!(root.read("new.txt").as_deref(), Some("theirs\n"), "the other process's file stays");
    assert!(seen.paths().is_empty());
}

/// Takes the reported file's lock at the moment the edit tools report it written: the kernel's observer runs after
/// the call let its lock go, never under it.
struct TriesTheLock {
    locks: PathBuf,
    free: Mutex<Vec<bool>>,
}
impl WriteObserver for TriesTheLock {
    fn wrote(&self, path: &Path) {
        let lock = File::open(lock_path(&self.locks, path)).expect("the call made its lock file");
        // Within a moment, as `free` below: a command another test starts meanwhile shares the open file between its
        // fork and exec. A lock the call still holds stays held.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut free = lock.try_lock().is_ok();
        while !free && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            free = lock.try_lock().is_ok();
        }
        self.free.lock().unwrap().push(free);
    }
}

/// **No lock outlives one call**, nor reaches the kernel: when the chat's write set is told of a write, the file's lock
/// is already free, and it stays free after a written edit, a refused edit and a created file.
///
/// Proven: moved the `ctx.writes` report inside `with_file_lock`'s closure in `edit_file` → failed (the lock was held
/// when the write set was told).
#[tokio::test]
async fn no_lock_outlives_one_call() {
    let (root, locks) = (TempDir::new("mp-nolock"), TempDir::new("mp-nolock-locks"));
    root.write("a.txt", "one\n");
    let observer = Arc::new(TriesTheLock { locks: locks.path().to_path_buf(), free: Mutex::default() });
    let ctx = ToolContext {
        yolo: true,
        file_locks: Some(locks.path().to_path_buf()),
        writes: Some(observer.clone() as Arc<dyn WriteObserver>),
        ..ToolContext::new(root.path())
    };
    let free = |file: &str| {
        let lock = File::open(lock_path(locks.path(), &root.path().join(file))).unwrap();
        // Within a moment: a command another test starts meanwhile shares the open file between its fork and exec.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while lock.try_lock().is_err() {
            if std::time::Instant::now() > deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
    };
    assert!(!run(&ctx, "read_file", json!({"path": "a.txt"})).await.is_error);
    assert!(!run(&ctx, "edit_file", json!({"path": "a.txt", "old_string": "one", "new_string": "ONE"})).await.is_error);
    assert!(free("a.txt"), "held after a written edit");
    std::fs::write(root.path().join("a.txt"), "changed\n").unwrap();
    assert!(run(&ctx, "edit_file", json!({"path": "a.txt", "old_string": "changed", "new_string": "x"})).await.is_error);
    assert!(free("a.txt"), "held after a refused edit");
    assert!(!run(&ctx, "write_file", json!({"path": "c.txt", "content": "new\n"})).await.is_error);
    assert!(free("c.txt"), "held after a created file");
    assert_eq!(*observer.free.lock().unwrap(), [true, true], "the write set is told after the lock is let go");
}

struct NoOne;
#[async_trait]
impl PeerApprover for NoOne {
    async fn approve_wire(&self, _request: WireRequest) -> Option<Approver> {
        None
    }
}

/// The kernel's events, kept in memory.
#[derive(Default)]
struct Events(Mutex<Vec<marv_kernel::KernelEvent>>);
impl marv_kernel::EventLog for Events {
    fn append(&self, _at_ms: u64, event: marv_kernel::KernelEvent) {
        self.0.lock().unwrap().push(event);
    }
}

/// A served switchboard with its parent plugged in and its door open in `mesh`.
struct Marv {
    sb: Arc<Switchboard>,
    _door: DoorGuard,
    _plugs: TempDir,
    cancel: CancellationToken,
}
impl Drop for Marv {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

fn marv(name: &str, mesh: &Path, root: &Path) -> Marv {
    let plugs = TempDir::short("mp-plugs");
    let sb = Arc::new(Switchboard::new(name, plugs.path().to_path_buf(), Arc::new(NoTrajectory)));
    let cancel = CancellationToken::new();
    tokio::spawn(Arc::clone(&sb).serve(cancel.clone()));
    sb.plug(marv_session::mesh::parent_id()).unwrap();
    // The person's `mesh.cards` on (their config's default): a door publishes its write set only then.
    sb.set_cards(true);
    let door = open_door(sb.clone(), mesh, DoorInfo { cwd: root.into(), pane: None, chat: None }, Arc::new(NoOne)).unwrap();
    Marv { sb, _door: door, _plugs: plugs, cancel }
}

fn me(name: &str, root: &Path) -> marv_kernel::AgentRef {
    marv_kernel::AgentRef { id: None, mesh_name: name.into(), folder: root.into(), kind: marv_kernel::AgentKind::Terminal }
}

fn session(project: &Path, model: Arc<Scripted>, mesh: &Arc<Switchboard>) -> marv_session::MarvSession {
    let mut init = marv_session::SessionInit::new(
        project,
        marv_session::ProviderChoice::Option(marv_provider::ProviderOption::Custom(model)),
    );
    init.diagnostics = false.into();
    init.checks = false;
    init.mesh = Some(mesh.clone());
    marv_session::MarvSession::new(init).unwrap()
}

async fn turn(session: &marv_session::MarvSession, text: &str) {
    let mut turn = session.send(text, Default::default()).unwrap();
    while futures::StreamExt::next(&mut turn).await.is_some() {}
}

const SECRET: &str = "SECRET-FILE-CONTENT-7f3a";

/// **Overlaps are shown as paths, never content.** Two desktop chats (one registry, one event log) write
/// `src/api.rs`, whose text is a secret; one of them also writes a file of its own. The desktop reads the shared path;
/// the event log gets one `WriteOverlap` with who and how many, and no path or byte of the file; a third Marv's
/// `mesh_list` lists the paths of the chat a rule lets send to it (fenced, and tainting: another model chose the
/// names), never the other chat's, and never content. A private chat's door, or one with `mesh.cards` off, publishes
/// no paths at all.
///
/// Proven: dropped the rule filter on the "editing:" line in `tools::peer_lines` → failed (the uncovered chat's file
/// was listed); `WRITES_TAINT = false` → failed (the list didn't taint); `Switchboard::published_writes` without its
/// cards/private check → failed (a private chat's door said its paths). The log's side: `KernelEvent::WriteOverlap`
/// has no field a path could go into (`deny_unknown_fields`, its eventlog test).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overlaps_are_shown_as_paths_never_content() {
    let mesh = TempDir::short("mp-mesh");
    #[cfg(unix)]
    std::fs::set_permissions(mesh.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let project = TempDir::new("mp-project");
    let (a, b) = (marv("alpha", mesh.path(), project.path()), marv("beta", mesh.path(), project.path()));
    let desktop = MeshRegistry::new();
    let events = Arc::new(Events::default());
    desktop.set_events(events.clone());
    for (m, name) in [(&a, "alpha"), (&b, "beta")] {
        m.sb.set_registry(desktop.clone());
        desktop.register_own(&m.sb, &me(name, project.path()), mesh.path(), None);
    }
    let content = format!("pub const KEY: &str = \"{SECRET}\";\n");
    let alpha = Scripted::new(vec![
        use_tools(vec![tool_call("w1", "write_file", json!({"path": "src/api.rs", "content": content}))]),
        say("done"),
    ]);
    let beta = Scripted::new(vec![
        use_tools(vec![tool_call("r1", "read_file", json!({"path": "src/api.rs"}))]),
        use_tools(vec![
            tool_call("e1", "edit_file", json!({"path": "src/api.rs", "old_string": "KEY", "new_string": "TOKEN"})),
            tool_call("w2", "write_file", json!({"path": "src/beta_only.rs", "content": format!("// {SECRET}\n")})),
        ]),
        say("done"),
    ]);
    turn(&session(project.path(), alpha, &a.sb), "write the api").await;
    turn(&session(project.path(), beta, &b.sb), "rename the key").await;
    assert!(project.read("src/api.rs").unwrap().contains("TOKEN"), "both wrote");

    // The desktop: the shared path, nothing else.
    let shown = desktop.overlaps_of("alpha");
    assert_eq!(shown.len(), 1);
    assert_eq!((shown[0].mesh_name.as_str(), shown[0].paths.clone()), ("beta", vec![project.path().join("src/api.rs")]));

    // The log: who and how many.
    let overlaps: Vec<marv_kernel::KernelEvent> =
        events.0.lock().unwrap().iter().filter(|e| matches!(e, marv_kernel::KernelEvent::WriteOverlap { .. })).cloned().collect();
    let key = |n: &str| marv_kernel::AgentKey::Mesh(n.into());
    assert_eq!(overlaps, [marv_kernel::KernelEvent::WriteOverlap { a: key("beta"), b: key("alpha"), paths_count: 1 }]);
    for event in events.0.lock().unwrap().iter() {
        let line = marv_kernel::LoggedEvent::new(1, event.clone()).line();
        assert!(!line.contains("api.rs") && !line.contains(SECRET) && !line.contains("beta_only"), "{line}");
    }

    // A third Marv, which a rule lets alpha (only) send to: alpha's paths, never beta's, never content; and the list
    // taints the reader (the names are another model's).
    let peers = marv_mesh::discovery::list(mesh.path()).await.unwrap();
    let mut w = marv_mesh::tools::Wireable::new("gamma", marv_session::mesh::parent_id());
    (w.on_mesh, w.peers) = (true, peers);
    w.covered = vec![("alpha".into(), marv_kernel::PolicyId(1))];
    w.covered_from = vec!["alpha".into()];
    let listed = marv_mesh::tools::list_answer(&[], &w);
    assert!(
        listed.text.contains("editing: <untrusted-data source=\"mesh-writes\" from=\"alpha\">[\"src/api.rs\"]"),
        "{}",
        listed.text
    );
    assert!(!listed.text.contains("beta_only") && !listed.text.contains(SECRET), "{}", listed.text);
    assert!(listed.from_mesh, "an editing: line taints the reader");
    // Through the doors too, only paths: the whole of beta's `marv/info` has no content.
    let beta_info = marv_mesh::discovery::list(mesh.path()).await.unwrap().into_iter().find(|p| p.session == "beta").unwrap();
    assert_eq!(beta_info.info.writes.len(), 2);
    assert!(!format!("{:?}", beta_info.info).contains(SECRET));

    // Private, or `mesh.cards` off: the door publishes no write set (the desktop's own overlap view still has it).
    b.sb.set_private(true);
    let beta_info = marv_mesh::discovery::list(mesh.path()).await.unwrap().into_iter().find(|p| p.session == "beta").unwrap();
    assert!(beta_info.info.writes.is_empty(), "a private chat's door says no paths");
    b.sb.set_private(false);
    b.sb.set_cards(false);
    let beta_info = marv_mesh::discovery::list(mesh.path()).await.unwrap().into_iter().find(|p| p.session == "beta").unwrap();
    assert!(beta_info.info.writes.is_empty(), "cards off: no paths");
    assert_eq!(desktop.overlaps_of("alpha").len(), 1, "the desktop's own view");
}

/// **A peer's write set is cleaned on receipt** (P4): another Marv's door can say any path; only paths inside the
/// folders it says it works in are kept (a spoof of a path in this chat's folder isn't an overlap), and only those
/// count. A terminal Marv working in `other/` says it wrote `other/x.rs` and, falsely, this chat's `src/api.rs`.
///
/// Proven: kept a peer's write set as sent (`marv_kernel::writes::received` returning its input, roots unchecked) →
/// failed (the spoofed path was kept and made an overlap).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_peers_write_set_is_cleaned_on_receipt() {
    let mesh = TempDir::short("mp-spoof-mesh");
    #[cfg(unix)]
    std::fs::set_permissions(mesh.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let project = TempDir::new("mp-spoof-project");
    let (mine, theirs) = (project.path().join("src"), project.path().join("other"));
    std::fs::create_dir_all(&mine).unwrap();
    std::fs::create_dir_all(&theirs).unwrap();
    let a = marv("alpha", mesh.path(), &mine);
    let t = marv("tango", mesh.path(), &theirs);
    let desktop = MeshRegistry::new();
    a.sb.set_registry(desktop.clone());
    desktop.register_own(&a.sb, &me("alpha", &mine), mesh.path(), None);
    a.sb.note_write(&mine.join("api.rs"));
    // tango's own switchboard records whatever it's told; its door says it.
    t.sb.note_write(&theirs.join("x.rs"));
    t.sb.note_write(&mine.join("api.rs"));
    let said = marv_mesh::discovery::list(mesh.path()).await.unwrap().into_iter().find(|p| p.session == "tango").unwrap();
    assert_eq!(said.info.writes.iter().map(|w| w.path.clone()).collect::<Vec<_>>(), [theirs.join("x.rs")], "cleaned as read");
    desktop.refresh(mesh.path(), true).await;
    let entry = desktop.snapshot().named("tango").unwrap().clone();
    assert_eq!(entry.writes.iter().map(|w| w.path.clone()).collect::<Vec<_>>(), [theirs.join("x.rs")]);
    assert!(desktop.overlaps_of("alpha").is_empty(), "a spoofed path is no overlap");
}

fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// **Granted chats sharing a folder default to worktrees** (P1). A chat granted alone in its folder stays there; one
/// granted beside another (the same folder, or one inside the other) starts in its own worktree by default: a branch
/// of its own, outside the folder, where a grant can be made for it and its writes are never the folder's (no write
/// overlap with the chat that stays).
///
/// Proven: made `marv_kernel::worktree_by_default` return false → failed (the second chat shared the folder).
#[tokio::test]
async fn granted_chats_sharing_a_folder_default_to_worktrees() {
    let (repo, trees) = (TempDir::new("mp-repo"), TempDir::new("mp-trees"));
    repo.write("src/api.rs", "fn main() {}\n");
    git(repo.path(), &["init", "-q"]);
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "start"]);
    let folder = repo.path();
    assert!(!marv_kernel::worktree_by_default(folder, []), "alone: stays");
    assert!(!marv_kernel::worktree_by_default(folder, [Path::new("/elsewhere/web")]));
    assert!(marv_kernel::worktree_by_default(folder, [folder]), "two granted chats in one folder");
    assert!(marv_kernel::worktree_by_default(&folder.join("src"), [folder]), "one inside the other's folder");

    // The second chat's worktree: its own branch, outside the folder, a folder a grant can be made for.
    let wt = marv_session::worktree::create_worktree(folder, trees.path(), "second chat").await.unwrap();
    assert!(wt.branch.starts_with("marv/") && !wt.work_dir.starts_with(folder), "{}", wt.work_dir.display());
    assert!(marv_kernel::FolderScope::new(&wt.work_dir, &std::env::temp_dir().join("no-such-home")).is_ok());
    // Its writes are the worktree's: no overlap with the chat in the folder.
    let now = 1_000;
    let mut first = marv_kernel::WriteSet::new();
    first.record(&folder.join("src/api.rs"), now);
    let mut second = marv_kernel::WriteSet::new();
    second.record(&wt.work_dir.join("src/api.rs"), now);
    assert!(marv_kernel::shared_paths(&first.recent(now), &second.recent(now), now).is_empty());
}
