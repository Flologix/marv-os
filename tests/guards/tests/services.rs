//! `docs/SECURITY.md`, "Services: chats that work while you're away": a chat the person granted runs, in a turn a
//! rule's message started, what yolo would run without asking and nothing past it, under an expiry and a daily spend
//! cap; nobody but the person makes a grant; every call it runs is logged without its input; a grant never lifts a
//! wake limit; and what the person is told about it never reaches a model.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use marv_core::ProviderEvent;
use marv_guards::{Recorder, Scripted, TempDir, say, tool_call, use_tools};
use marv_kernel::{AgentId, FileGrants, GrantBy, GrantId, GrantStore, KernelEvent, NewGrant, SpendCap};
use marv_mesh::approval::WireRequest;
use marv_mesh::discovery::{DoorGuard, DoorInfo, open_door};
use marv_mesh::hop::TurnHop;
use marv_mesh::switchboard::{NoTrajectory, Switchboard};
use marv_mesh::wire::{Approver as WireBy, WireMode};
use marv_mesh::{PlugId, ReceptacleName};
use marv_session::grants::{ChatGrant, FileChatGrant, GrantPause, SpendBook, pause_text};
use marv_session::mesh::{DoorWires, MeshTool, parent_id, wire_approver};
use marv_session::{MarvSession, SessionInit};
use marv_tools::{Approval, Decision, ServiceGrant, ToolContext, ToolRef, builtin_tools, run_tool};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// The desktop chat the grants here are for.
const CHAT: &str = "5e7c1ce0-0000-4000-8000-000000000001";

fn chat() -> AgentId {
    AgentId::new(CHAT).unwrap()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}

// ---- the person's grants ----------------------------------------------------------------------------------------

/// The person's grants file in `home`'s `.marv`.
fn grant_store(home: &Path) -> GrantStore {
    GrantStore::new(&home.join(".marv"), home).unwrap()
}

/// A grant for [`CHAT`] (folder `home/Projects/acme`), as the desktop makes one.
async fn grant(store: &GrantStore, home: &Path, expires_at_ms: Option<u64>, cap: SpendCap) -> GrantId {
    let acme = home.join("Projects/acme");
    std::fs::create_dir_all(&acme).unwrap();
    let new = NewGrant {
        chat: chat(),
        folder: marv_kernel::FolderScope::new(&acme, home).unwrap(),
        expires_at_ms,
        cap,
        by: GrantBy::Desktop,
        created_at_ms: now_ms(),
        label: "acme works while you're away".into(),
    };
    store.add(new).await.unwrap().0
}

/// The desktop's grant for [`CHAT`] over `store`.
fn file_grant(store: &GrantStore) -> FileChatGrant {
    FileChatGrant::new(Arc::new(FileGrants::new(store.clone())), Arc::new(SpendBook::load(store.clone())), chat())
}

/// A [`ChatGrant`] as the tool registry sees it (the session's own adapter is crate-private).
struct Via(Arc<dyn ChatGrant>);
impl ServiceGrant for Via {
    fn live(&self) -> Option<u64> {
        self.0.live().map(|id| id.0)
    }
    fn used(&self, grant: u64, tool: &str, audit_key: &str) {
        self.0.used(GrantId(grant), tool, audit_key, 1);
    }
}

/// A grant that always holds, recording what it ran.
#[derive(Default)]
struct Live(Mutex<Vec<String>>);
impl ServiceGrant for Live {
    fn live(&self) -> Option<u64> {
        Some(1)
    }
    fn used(&self, _grant: u64, tool: &str, _audit_key: &str) {
        self.0.lock().unwrap().push(tool.into());
    }
}

/// The context of a call in a turn the person didn't start, under `grant`, asking `person`.
fn unattended(root: &Path, grant: Arc<dyn ServiceGrant>, person: &Arc<Recorder>) -> ToolContext {
    let mut ctx = ToolContext::new(root);
    ctx.mode = marv_tools::CallMode::Woken;
    ctx.grant = Some(grant);
    ctx.approver = Some(person.clone());
    ctx
}

#[derive(Default)]
struct Events(Mutex<Vec<KernelEvent>>);
impl marv_kernel::EventLog for Events {
    fn append(&self, _at_ms: u64, event: KernelEvent) {
        self.0.lock().unwrap().push(event);
    }
}

// ---- two Marvs under a rule, as the desktop runs them -----------------------------------------------------------

struct Marv {
    mesh: Arc<Switchboard>,
    _door: DoorGuard,
    _runtime: TempDir,
    cancel: CancellationToken,
}

impl Drop for Marv {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

fn marv(name: &str, dir: &Path, folder: &Path) -> Marv {
    let runtime = TempDir::new(&format!("svc-{name}-rt"));
    let mesh = Arc::new(Switchboard::new(name, runtime.path().join("plugs"), Arc::new(NoTrajectory)));
    let cancel = CancellationToken::new();
    tokio::spawn(Arc::clone(&mesh).serve(cancel.clone()));
    mesh.plug(parent_id()).unwrap();
    let nobody = Recorder::new(Decision::No);
    let wires = wire_approver(None, Some(&(nobody as Arc<dyn marv_tools::Approver>))).unwrap();
    let info = DoorInfo { cwd: folder.to_path_buf(), pane: None, chat: None };
    let door = open_door(mesh.clone(), dir, info, Arc::new(DoorWires(wires))).unwrap();
    mesh.set_identity(
        parent_id(),
        marv_kernel::AgentRef {
            id: None,
            mesh_name: name.into(),
            folder: folder.to_path_buf(),
            kind: marv_kernel::AgentKind::Terminal,
        },
    );
    Marv { mesh, _door: door, _runtime: runtime, cancel }
}

/// Alpha and beta under `~/Projects/acme`, a rule the person made letting them message each other, and alpha wired to
/// beta's main agent by it. Beta's folder (`acme/web`) is where its chat works.
struct Pair {
    home: TempDir,
    _doors: TempDir,
    alpha: Marv,
    beta: Marv,
    alpha_hop: Arc<TurnHop>,
}

impl Pair {
    async fn new(tag: &str) -> Self {
        let home = TempDir::new(&format!("{tag}-home"));
        let acme = home.path().join("Projects/acme");
        for folder in ["api", "web"] {
            std::fs::create_dir_all(acme.join(folder)).unwrap();
        }
        let doors = TempDir::new(tag);
        let dir = doors.path().join("mesh");
        let alpha = marv("alpha", &dir, &acme.join("api"));
        let beta = marv("beta", &dir, &acme.join("web"));
        let rules = marv_kernel::PolicyStore::new(&home.path().join(".marv"), home.path()).unwrap();
        let scope = || marv_kernel::Subject::Folder(marv_kernel::FolderScope::new(&acme, home.path()).unwrap());
        let rule = marv_kernel::NewRule {
            from: scope(),
            to: scope(),
            mode: marv_kernel::Mode::Send,
            mutual: true,
            expires_at_ms: None,
            by: marv_kernel::RuleBy::Desktop,
            created_at_ms: 1,
            label: "Acme".into(),
        };
        rules.add(rule).await.unwrap();
        for m in [&alpha, &beta] {
            m.mesh.set_rules(Arc::new(marv_kernel::FileRules::new(rules.clone())));
        }
        let to_beta = ReceptacleName::new("peer.beta.agent.parent.inbox").unwrap();
        alpha.mesh.lookup(&to_beta).await.unwrap();
        let request = WireRequest { from: parent_id(), to: to_beta, mode: WireMode::Send, asked_by: parent_id() };
        let by = alpha.mesh.rule_for(&request).await.expect("the rule covers alpha and beta");
        alpha.mesh.grant(request, by).await.unwrap();
        let alpha_hop = Arc::new(TurnHop::default());
        alpha.mesh.set_turn_hop(parent_id(), alpha_hop.clone());
        Self { home, _doors: doors, alpha, beta, alpha_hop }
    }

    fn project(&self) -> PathBuf {
        self.home.path().join("Projects/acme/web")
    }

    /// Alpha's message to beta, on the rule's wire.
    async fn alpha_says(&self, text: &str) {
        self.alpha
            .mesh
            .deliver(&parent_id(), "peer.beta.agent.parent.inbox", WireMode::Send, json!({ "text": text }))
            .await
            .unwrap();
    }

    /// A worker on beta, wired to beta's main agent by beta's person by hand, says `text`.
    async fn hand_wired_worker_says(&self, text: &str) -> PlugId {
        let worker = self.beta.mesh.plug_next("worker").unwrap();
        let inbox = ReceptacleName::new("agent.parent.inbox").unwrap();
        let request =
            WireRequest { from: worker.id.clone(), to: inbox.clone(), mode: WireMode::Send, asked_by: worker.id.clone() };
        self.beta.mesh.grant(request, WireBy::Tui).await.unwrap();
        self.beta.mesh.deliver(&worker.id, inbox.as_str(), WireMode::Send, json!({ "text": text })).await.unwrap();
        worker.id
    }

    /// Beta's chat as the desktop runs it (yolo on: whatever asks below asks despite it), with `grant`.
    fn session(
        &self,
        model: &Arc<Scripted>,
        person: &Arc<Recorder>,
        grant: Option<Arc<dyn ChatGrant>>,
        edit: impl FnOnce(&mut SessionInit),
    ) -> MarvSession {
        let mut init = SessionInit::new(
            self.project(),
            marv_session::ProviderChoice::Option(marv_provider::ProviderOption::Custom(model.clone())),
        );
        (init.diagnostics, init.checks, init.checkpoints, init.yolo) = (false.into(), false, false, true);
        init.mesh = Some(self.beta.mesh.clone());
        init.approve = Some(person.clone());
        init.grant = grant;
        edit(&mut init);
        MarvSession::new(init).unwrap()
    }
}

/// What the desktop does when beta's inbox has mail: its wake check, then the woken turn (`marv_web::host`), the
/// page's events collected. None: no turn started.
async fn desktop_wake(
    session: &MarvSession,
    mesh: &Arc<Switchboard>,
    limit: &Mutex<marv_session::mesh::WakeLimit>,
) -> Option<Vec<Value>> {
    let (out, mut rx) = mpsc::unbounded_channel();
    let by = marv_web::host::check_wake(mesh, limit, &out, marv_session::config::MeshWake::On)?;
    if !marv_web::host::run_wake_then(session, &by, &out, marv_session::config::MeshWake::On, || {}).await {
        return None;
    }
    let mut page = Vec::new();
    while let Ok(event) = rx.try_recv() {
        page.push(event);
    }
    Some(page)
}

fn write(id: &str, path: &str, content: &str) -> marv_core::ToolCall {
    tool_call(id, "write_file", json!({ "path": path, "content": content }))
}

fn history_text(model: &Scripted) -> String {
    model.requests.lock().unwrap().iter().map(|(history, _)| format!("{history:?}")).collect::<Vec<_>>().join("\n")
}

// ---- the guards ---------------------------------------------------------------------------------------------------

/// **No grant: a woken turn asks for everything, as before.** A rule's message wakes beta's chat (yolo on) through the
/// desktop's path; its edit asks the person, once, and doesn't run. The same with the desktop's grant handle when the
/// person never granted the chat.
///
/// Proven: made `run_tool`'s yolo test run unattended calls without a grant (dropped the `grant.is_some()` term) →
/// this test failed (the edit ran unasked).
#[tokio::test]
async fn no_grant_means_a_woken_turn_asks_for_everything() {
    for granted_never in [false, true] {
        let pair = Pair::new(if granted_never { "svc-nogrant-handle" } else { "svc-nogrant" }).await;
        let model = Scripted::new(vec![use_tools(vec![write("w1", "a.txt", "x")]), say("Done.")]);
        let person = Recorder::new(Decision::No);
        let handle: Option<Arc<dyn ChatGrant>> =
            granted_never.then(|| Arc::new(file_grant(&grant_store(pair.home.path()))) as Arc<dyn ChatGrant>);
        let session = pair.session(&model, &person, handle, |_| {});
        pair.alpha_says("write a.txt").await;
        assert!(desktop_wake(&session, &pair.beta.mesh, &Mutex::default()).await.is_some(), "a rule's message wakes it");
        assert!(!pair.project().join("a.txt").exists(), "granted_never={granted_never}");
        let asked = person.requests();
        assert_eq!(asked.len(), 1, "{asked:?}");
        assert!(asked[0].once, "never answered from a remembered always");
        session.close().await;
    }
}

/// **A grant runs only what yolo would.** Under a live grant, in a turn the person didn't start: a file edit outside
/// `.git` runs (confined, `Approval::Grant`), and so does a sandboxed command without network where bubblewrap
/// works. Bash with network, a git write, a write to a protected path (`.git/config`), a file write where the project
/// is the home folder, an MCP call and a host command each still ask (the person says no) and don't run.
///
/// Proven: made the grant answer the yolo test without `tool.auto_safe` (any call) → this test failed (bash with
/// network ran); dropped `!home_write` from the test → failed (the home write ran).
#[tokio::test]
async fn a_grant_runs_only_what_yolo_would() {
    let project = TempDir::new("svc-only-yolo");
    std::fs::create_dir_all(project.path().join(".git")).unwrap();
    let person = Recorder::new(Decision::No);
    let live = Arc::new(Live::default());
    let mut ctx = unattended(project.path(), live.clone(), &person);
    ctx.doors.host = true;
    let mcp: ToolRef = {
        use marv_mcp::config::{ServerSource, parse_server};
        use marv_mcp::tool::{McpCallResult, McpCaller, McpToolInfo, make_mcp_tool};
        struct Server;
        #[async_trait::async_trait]
        impl McpCaller for Server {
            async fn call(
                &self,
                _tool: &str,
                _args: serde_json::Map<String, Value>,
                _cancel: CancellationToken,
            ) -> marv_mcp::Result<McpCallResult> {
                panic!("the MCP call ran")
            }
        }
        let server =
            parse_server("tracker", &json!({"command": "tracker"}), ServerSource::Project, &|_| None, ".mcp.json").unwrap();
        let info = McpToolInfo {
            name: "update_issue".into(),
            description: Some("Changes an issue.".into()),
            input_schema: serde_json::Map::new(),
            annotations: None,
        };
        Arc::new(marv_session::mcp_tools::McpToolAdapter {
            tool: make_mcp_tool(&server, info, Arc::new(Server)),
            taint: ctx.taint.clone(),
        })
    };
    let mcp_name = mcp.name().to_string();
    let mut tools: Vec<ToolRef> = builtin_tools().to_vec();
    tools.push(mcp);

    let edit = run_tool(&marv_guards::call("write_file", json!({"path": "a.txt", "content": "x"})), &ctx, &tools).await;
    assert_eq!(edit.result.approval, Some(Approval::Grant), "{}", edit.result.output);
    assert!(project.path().join("a.txt").exists());
    let mut ran = vec!["write_file".to_string()];
    if marv_sandbox::sandbox_available() {
        let bash = run_tool(&marv_guards::call("bash", json!({"command": "echo hi > b.txt"})), &ctx, &tools).await;
        assert_eq!(bash.result.approval, Some(Approval::Grant), "{}", bash.result.output);
        assert!(project.path().join("b.txt").exists());
        ran.push("bash".into());
    } else {
        eprintln!("skipped the sandboxed command: bubblewrap isn't available here");
    }

    let home_ctx = ToolContext { home: project.path().to_path_buf(), ..ctx.clone() };
    let asks: Vec<(&ToolContext, marv_core::ToolCall)> = vec![
        (&ctx, marv_guards::call("bash", json!({"command": "echo net > n.txt", "network": true}))),
        (&ctx, marv_guards::call("bash", json!({"command": "echo git > g.txt", "git_write": true}))),
        (&ctx, marv_guards::call("write_file", json!({"path": ".git/config", "content": "[core]\n"}))),
        (&home_ctx, marv_guards::call("write_file", json!({"path": "home.txt", "content": "x"}))),
        (&ctx, marv_guards::call(&mcp_name, json!({}))),
        (&ctx, marv_guards::call("bash", json!({"command": "echo host > h.txt", "sandbox": false}))),
    ];
    let n = asks.len();
    for (context, call) in asks {
        let out = run_tool(&call, context, &tools).await;
        assert!(out.result.declined, "{} {}: {}", call.name, call.arguments, out.result.output);
    }
    assert_eq!(person.requests().len(), n, "each asked the person");
    for file in ["n.txt", "g.txt", ".git/config", "home.txt", "h.txt"] {
        assert!(!project.path().join(file).exists(), "{file} was written");
    }
    assert_eq!(*live.0.lock().unwrap(), ran, "the grant ran only yolo's set");
}

/// **Tainted egress asks under a grant.** A send on a wire the person made is in yolo's set (it runs under a grant in
/// an untainted context, the control); in a tainted one, as every woken turn is, it asks with `must_ask` and nothing
/// is sent.
///
/// Proven: dropped `!must_ask` from `run_tool`'s yolo test → this test failed (the tainted send ran).
#[tokio::test]
async fn tainted_egress_asks_under_a_grant() {
    let runtime = TempDir::new("svc-egress");
    let mesh = Arc::new(Switchboard::new("g", runtime.path().join("plugs"), Arc::new(NoTrajectory)));
    let cancel = CancellationToken::new();
    tokio::spawn(Arc::clone(&mesh).serve(cancel.clone()));
    let plug = mesh.plug(parent_id()).unwrap();
    let worker = mesh.plug_next("worker").unwrap();
    let inbox = ReceptacleName::inbox(&worker.id);
    let request = WireRequest { from: parent_id(), to: inbox.clone(), mode: WireMode::Send, asked_by: parent_id() };
    mesh.grant(request, WireBy::Tui).await.unwrap();
    let person = Recorder::new(Decision::No);
    let wires = wire_approver(None, Some(&(person.clone() as Arc<dyn marv_tools::Approver>)));
    let tools: Vec<ToolRef> = MeshTool::all(&plug, &mesh, wires).into_iter().map(|t| Arc::new(t) as ToolRef).collect();
    let send = marv_guards::call("mesh_send", json!({"receptacle": inbox.as_str(), "text": "the secret is 42"}));
    let project = TempDir::new("svc-egress-project");

    let clean = unattended(project.path(), Arc::new(Live::default()), &person);
    let sent = run_tool(&send, &clean, &tools).await;
    assert_eq!(sent.result.approval, Some(Approval::Grant), "the control: {}", sent.result.output);
    assert_eq!(mesh.drain_inbox(&worker.id).len(), 1);

    let tainted = unattended(project.path(), Arc::new(Live::default()), &person);
    tainted.taint.mark(marv_core::Provenance::Mesh);
    let asked = run_tool(&send, &tainted, &tools).await;
    assert!(asked.result.declined, "{}", asked.result.output);
    let requests = person.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].must_ask);
    assert!(mesh.drain_inbox(&worker.id).is_empty(), "nothing was sent");
    cancel.cancel();
}

/// **A grant ignores remembered "always" answers and approved URL prefixes.** Beta's person pre-approved git writes
/// (`"approve": ["git_write"]`); in a granted turn a rule's message started, a git write asks anyway, once. And a web
/// request under a URL prefix approved after the taint asks under a grant, with `must_ask`.
///
/// Proven: gave the woken turn the session's own "always" scopes (`always` in `MarvSession::start`) and dropped
/// `|| ctx.no_auto_approval` from `once` in `run_tool` (two guards hold this; either alone keeps it) → this test failed
/// (the git write wasn't asked); dropped `!ctx.no_auto_approval` from `prefix_approved` → failed (the request ran
/// unasked).
#[tokio::test]
async fn a_grant_ignores_remembered_always_and_url_prefixes() {
    let pair = Pair::new("svc-always").await;
    let store = grant_store(pair.home.path());
    grant(&store, pair.home.path(), None, SpendCap::default()).await;
    let model = Scripted::new(vec![
        use_tools(vec![tool_call("b1", "bash", json!({"command": "echo committed", "git_write": true}))]),
        say("Done."),
    ]);
    let person = Recorder::new(Decision::No);
    let session = pair.session(&model, &person, Some(Arc::new(file_grant(&store))), |init| {
        init.pre_approved = vec!["git_write".into()];
    });
    pair.alpha_says("commit it").await;
    assert!(desktop_wake(&session, &pair.beta.mesh, &Mutex::default()).await.is_some());
    let asked = person.requests();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(asked[0].once);
    session.close().await;

    let project = TempDir::new("svc-prefix");
    let person = Recorder::new(Decision::No);
    let ctx = unattended(project.path(), Arc::new(Live::default()), &person);
    ctx.taint.mark(marv_core::Provenance::Web);
    ctx.taint.approve_prefix("http://127.0.0.1:9/docs/");
    let fetch = marv_guards::call("web_fetch", json!({"url": "http://127.0.0.1:9/docs/page"}));
    // A local address the person may fetch (as the provenance guards' local server), nothing listening on it.
    let web: Vec<ToolRef> = vec![Arc::new(marv_tools::web_fetch::WebFetch::new(marv_tools::web_fetch::WebOptions {
        allow_private: true,
        ..Default::default()
    }))];
    let out = run_tool(&fetch, &ctx, &web).await;
    assert!(out.result.declined, "{}", out.result.output);
    assert!(person.requests()[0].must_ask);
}

/// A file's code without its test module and comments.
fn non_test_code(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let end = (0..lines.len())
        .find(|&i| lines[i].trim() == "#[cfg(test)]" && lines.get(i + 1).is_some_and(|l| l.contains("mod ")))
        .unwrap_or(lines.len());
    lines[..end].iter().map(|l| l.split("//").next().unwrap_or_default()).collect::<Vec<_>>().join("\n")
}

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// The product's non-kernel Rust files whose code (no tests, no comments) contains any of `needles`.
fn sources_with(needles: &[&str]) -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let root = workspace();
    let mut files = Vec::new();
    for crate_dir in std::fs::read_dir(root.join("crates")).unwrap().flatten() {
        if crate_dir.file_name() != "marv-kernel" {
            walk(&crate_dir.path().join("src"), &mut files);
        }
    }
    walk(&root.join("app/src-tauri/src"), &mut files);
    assert!(files.len() > 50, "the scan found the sources");
    let mut found: Vec<String> = files
        .into_iter()
        .filter(|f| {
            let code = non_test_code(&std::fs::read_to_string(f).unwrap());
            needles.iter().any(|n| code.contains(n))
        })
        .map(|f| f.strip_prefix(&root).unwrap().display().to_string())
        .collect();
    found.sort();
    found
}

/// **A model, a mesh message or a project can't make a grant.** Only the desktop's own commands (the rule dialog's
/// click, raising a cap) make or widen one: a scan of every crate's non-test code finds grants made (`NewGrant`),
/// widened (`set_cap`) or attributed (`GrantBy::`) only in the desktop; the only maker is the desktop; and a session
/// gets a grant only from its caller (`SessionInit::grant`, set by the SDK's options, which only marv-web's chat host
/// sets, from the desktop's handle). No tool is about grants. A grants file is read only from the user's own config
/// folder (an absolute one: never a project's `.marv`), and a file naming any other maker, or with a field it
/// doesn't know, is refused whole.
///
/// Proven: planted `.set_cap(` in `marv_session::mesh`'s code → this test failed (the scan found it).
#[test]
fn a_model_a_mesh_message_or_a_project_cant_make_a_grant() {
    assert_eq!(sources_with(&["NewGrant", ".set_cap(", "GrantBy::"]), ["app/src-tauri/src/lib.rs"]);
    assert_eq!(
        sources_with(&["GrantStore"]),
        ["app/src-tauri/src/lib.rs", "crates/marv-cli/src/kernel_cmd.rs", "crates/marv-session/src/grants.rs"],
        "who opens the grants file"
    );
    assert_eq!(
        sources_with(&["init.grant =", "options.grant =", "spec.grant ="]),
        ["app/src-tauri/src/chats.rs", "crates/marv-session/src/sdk.rs", "crates/marv-web/src/host.rs"],
        "who hands a session a grant"
    );
    for by in ["model", "mesh", "project", "tui", "cli", "env", "flag", "radio"] {
        assert!(serde_json::from_value::<GrantBy>(json!(by)).is_err(), "{by}");
    }
    let mut names: Vec<String> = marv_tools::tool_specs().iter().map(|s| s.name.clone()).collect();
    names.extend(marv_mesh::tools::mesh_specs().iter().map(|s| s.name.clone()));
    assert!(names.iter().all(|n| !n.contains("grant")), "{names:?}");

    let project = TempDir::new("svc-project-grants");
    assert!(GrantStore::new(Path::new(".marv"), project.path()).is_err(), "a relative config folder");
    let home = TempDir::new("svc-made-by");
    let store = grant_store(home.path());
    let file = |by: &str, extra: &str| {
        format!(
            r#"{{"version": 1, "generation": 1, "next_id": 2, "grants": [{{"id": 1, "chat": "{CHAT}", "folder": "{}", "by": "{by}", "created_at_ms": 1{extra}}}]}}"#,
            home.path().join("Projects/acme").display()
        )
    };
    let write = |text: &str| {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        std::fs::write(store.path(), text).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o600)).unwrap();
    };
    write(&file("desktop", ""));
    assert_eq!(store.load().unwrap().grants.len(), 1, "the control");
    for text in [file("model", ""), file("mesh", ""), file("desktop", r#", "unlimited": true"#)] {
        write(&text);
        assert!(store.load().is_err(), "{text}");
        assert!(FileGrants::new(store.clone()).snapshot().grants.is_empty(), "refused whole: no grants");
    }
}

/// **A revoked, expired or spent grant asks at the next call.** The desktop's grant handle, consulted at every call:
/// live, the edit runs; revoked, the next one asks; a grant that has expired asks; one at today's cap asks; a new
/// grant runs again (the control).
///
/// Proven: made `FileChatGrant::live` hold whenever the chat has a grant (ignoring `grant_for`'s expiry and spend) →
/// this test failed (the expired grant ran the edit).
#[tokio::test]
async fn a_revoked_expired_or_spent_grant_asks_at_the_next_call() {
    let home = TempDir::new("svc-next-call");
    let project = TempDir::new("svc-next-call-project");
    let store = grant_store(home.path());
    let first = grant(&store, home.path(), None, SpendCap::default()).await;
    let handle: Arc<dyn ChatGrant> = Arc::new(file_grant(&store));
    let person = Recorder::new(Decision::No);
    let ctx = unattended(project.path(), Arc::new(Via(handle.clone())), &person);
    let tools = builtin_tools();
    let edit = |n: u32| marv_guards::call("write_file", json!({"path": format!("{n}.txt"), "content": "x"}));

    assert_eq!(run_tool(&edit(1), &ctx, tools).await.result.approval, Some(Approval::Grant));
    store.revoke(first).await.unwrap();
    assert!(run_tool(&edit(2), &ctx, tools).await.result.declined, "revoked");
    grant(&store, home.path(), Some(1), SpendCap::default()).await;
    assert!(run_tool(&edit(3), &ctx, tools).await.result.declined, "expired");
    grant(&store, home.path(), None, SpendCap { usd_micros: 1_000_000, tokens: 1_000 }).await;
    handle.spend(Some(1.0), 0);
    assert!(run_tool(&edit(4), &ctx, tools).await.result.declined, "spent");
    grant(&store, home.path(), None, SpendCap::default()).await;
    assert_eq!(run_tool(&edit(5), &ctx, tools).await.result.approval, Some(Approval::Grant), "a new grant: the control");
    assert_eq!(person.requests().len(), 3);
    for (n, written) in [(1, true), (2, false), (3, false), (4, false), (5, true)] {
        assert_eq!(project.path().join(format!("{n}.txt")).exists(), written, "{n}");
    }
}

/// **A grant never lifts a wake limit or the hop stop.** With beta's chat granted: a rule's message at the hop limit
/// doesn't wake it (it waits), a rule's wire wakes it at most three times a minute (the fourth message waits), and
/// after ten rule wakes in ten minutes (other wires') the rule wakes pause: the next message waits.
///
/// Proven: made `decide_wake_as` skip the hop stop (dropped its `RULE_WAKE_MAX_HOP` check) → this test failed (the
/// message at hop 2 woke beta).
#[tokio::test]
async fn a_grant_never_lifts_a_wake_limit_or_the_hop_stop() {
    let pair = Pair::new("svc-wake-limits").await;
    let store = grant_store(pair.home.path());
    grant(&store, pair.home.path(), None, SpendCap::default()).await;
    let model = Scripted::new((0..8).map(|_| say("Read it.")).collect());
    let person = Recorder::new(Decision::No);
    let session = pair.session(&model, &person, Some(Arc::new(file_grant(&store))), |_| {});
    let limit = Mutex::new(marv_session::mesh::WakeLimit::default());

    pair.alpha_hop.set(marv_session::mesh::RULE_WAKE_MAX_HOP);
    pair.alpha_says("at the hop stop").await;
    assert!(desktop_wake(&session, &pair.beta.mesh, &limit).await.is_none(), "the hop stop holds under a grant");
    assert_eq!(marv_session::mesh::unread_on_wires(&pair.beta.mesh), 1, "it waits");
    pair.beta.mesh.drain_inbox(&parent_id());

    pair.alpha_hop.set(0);
    for n in 0..marv_session::mesh::WAKES_PER_MINUTE {
        pair.alpha_says(&format!("message {n}")).await;
        assert!(desktop_wake(&session, &pair.beta.mesh, &limit).await.is_some(), "wake {n}");
    }
    pair.alpha_says("one more").await;
    assert!(desktop_wake(&session, &pair.beta.mesh, &limit).await.is_none(), "the per-wire limit holds under a grant");
    pair.beta.mesh.drain_inbox(&parent_id());

    // Ten rule wakes in ten minutes, from other wires, on the same limit the desktop keeps for the chat.
    let mut fresh = marv_session::mesh::WakeLimit::default();
    let now = now_ms();
    for n in 0..marv_session::mesh::RULE_WAKES_OVERALL {
        let candidate = marv_session::mesh::WakeCandidate {
            id: format!("m{n}"),
            by: marv_session::mesh::WakeBy {
                sender: PlugId::new(format!("peer-other{n}")).unwrap(),
                receptacle: ReceptacleName::new("agent.parent.inbox").unwrap(),
            },
            hop: Some(0),
            rule_issued: true,
            rounds_out: false,
            report: false,
            team: false,
        };
        assert!(marv_session::mesh::decide_wake(vec![candidate], &mut fresh, now).wake.is_some(), "wake {n}");
    }
    let limit = Mutex::new(fresh);
    pair.alpha_says("the eleventh").await;
    assert!(desktop_wake(&session, &pair.beta.mesh, &limit).await.is_none(), "paused after ten, under a grant");
    assert!(limit.lock().unwrap().rule_wakes_paused());
    session.close().await;
}

/// A radio pair (`marv_mesh::radio`) for the radio half below.
mod radio {
    use super::*;
    use marv_mesh::discovery::PeerApprover;
    use marv_mesh::radio::{By, Presence, Radio, RadioTimings, To};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    struct NoOne;
    #[async_trait::async_trait]
    impl PeerApprover for NoOne {
        async fn approve_wire(&self, _request: WireRequest) -> Option<WireBy> {
            None
        }
    }

    pub struct RadioMarv {
        pub sb: Arc<Switchboard>,
        pub radio: Arc<Radio>,
        _door: DoorGuard,
        _plugs: TempDir,
        cancel: CancellationToken,
    }
    impl Drop for RadioMarv {
        fn drop(&mut self) {
            self.radio.stop();
            self.cancel.cancel();
        }
    }

    fn timings() -> RadioTimings {
        RadioTimings {
            heartbeat: Duration::from_secs(60),
            silent_after: Duration::from_secs(60),
            drop_after: Duration::from_secs(120),
            relist: Duration::from_secs(60),
            left_shown: Duration::from_secs(5),
            presence_gap: Duration::from_millis(100),
            tick: Duration::from_millis(20),
            write_timeout: Duration::from_secs(5),
        }
    }

    fn radio_marv(name: &str, mesh: &Path, root: &str) -> RadioMarv {
        let plugs = TempDir::short("svc-radio-plugs");
        let sb = Arc::new(Switchboard::new(name, plugs.path().to_path_buf(), Arc::new(NoTrajectory)));
        let cancel = CancellationToken::new();
        tokio::spawn(Arc::clone(&sb).serve(cancel.clone()));
        sb.plug(parent_id()).unwrap();
        let door = open_door(sb.clone(), mesh, DoorInfo { cwd: root.into(), pane: None, chat: None }, Arc::new(NoOne)).unwrap();
        let radio = Radio::new(&sb, mesh, Presence::new(name, Path::new(root), None, false), timings());
        radio.start();
        RadioMarv { sb, radio, _door: door, _plugs: plugs, cancel }
    }

    async fn until(mut ok: impl FnMut() -> bool) {
        for _ in 0..500 {
            if ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out");
    }

    /// Alpha and beta on the radio, and beta's addressed message waiting at alpha.
    pub async fn addressed_message(mesh: &TempDir) -> (RadioMarv, RadioMarv) {
        #[cfg(unix)]
        std::fs::set_permissions(mesh.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let (alpha, beta) = (radio_marv("alpha", mesh.path(), "/work/alpha"), radio_marv("beta", mesh.path(), "/work/beta"));
        alpha.radio.turn_on().await;
        beta.radio.turn_on().await;
        until(|| alpha.radio.links().len() == 1 && beta.radio.links().len() == 1).await;
        let sent = beta.radio.send_msg(&To::Names(vec![alpha.sb.session.clone()]), By::Model, 0, "write a.txt").unwrap();
        assert!(sent.refused.is_empty(), "{sent:?}");
        until(|| !alpha.sb.unread_radio().is_empty()).await;
        (alpha, beta)
    }
}

/// **A wake on a wire the person wired by hand, or the radio's, runs without the grant.** Beta's chat is granted;
/// a message from a worker wired by hand wakes it (as the desktop wakes on any wire), and its edit asks. A radio
/// message addressed to a granted chat wakes it, and its edit asks too.
///
/// Proven: made `MarvSession::start` give the grant to every woken turn (dropped the `rules_only: true` match) →
/// this test failed (the hand-wired wake's edit ran unasked).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hand_wired_or_radio_wake_runs_without_the_grant() {
    let pair = Pair::new("svc-hand-wired").await;
    let store = grant_store(pair.home.path());
    grant(&store, pair.home.path(), None, SpendCap::default()).await;
    let model = Scripted::new(vec![use_tools(vec![write("w1", "a.txt", "x")]), say("Done.")]);
    let person = Recorder::new(Decision::No);
    let session = pair.session(&model, &person, Some(Arc::new(file_grant(&store))), |_| {});
    pair.hand_wired_worker_says("write a.txt").await;
    assert!(desktop_wake(&session, &pair.beta.mesh, &Mutex::default()).await.is_some(), "a hand-wired message wakes it");
    assert!(!pair.project().join("a.txt").exists(), "the edit didn't run");
    assert_eq!(person.requests().len(), 1);
    session.close().await;

    let mesh = TempDir::short("svc-radio");
    let (alpha, _beta) = radio::addressed_message(&mesh).await;
    // In the grant's own folder, so only how it woke keeps the grant out.
    let project = pair.home.path().join("Projects/acme/radio");
    std::fs::create_dir_all(&project).unwrap();
    let model = Scripted::new(vec![use_tools(vec![write("w1", "a.txt", "x")]), say("Done.")]);
    let person = Recorder::new(Decision::No);
    let mut init =
        SessionInit::new(&project, marv_session::ProviderChoice::Option(marv_provider::ProviderOption::Custom(model.clone())));
    (init.diagnostics, init.checks, init.checkpoints, init.yolo) = (false.into(), false, false, true);
    init.mesh = Some(alpha.sb.clone());
    init.radio = Some(true);
    init.approve = Some(person.clone());
    init.grant = Some(Arc::new(file_grant(&store)));
    let session = MarvSession::new(init).unwrap();
    let check = marv_session::radio::radio_wake_check(&alpha.sb, &mut Default::default(), 0);
    let by = check.wake.expect("an addressed message wakes");
    let mut turn = session.wake_radio(&by).unwrap().expect("a woken turn");
    use futures::StreamExt;
    while turn.next().await.is_some() {}
    assert!(!project.join("a.txt").exists(), "the radio's turn ran no edit unasked");
    assert_eq!(person.requests().len(), 1);
    session.close().await;
}

/// **Every call a grant runs is logged, without its input.** A granted turn's edit is logged as one `GrantUsed`
/// (the grant, the chat, `write_file`, its audit key `write_file`, the turn), and nothing in the log holds the file's
/// name or its words. The page marks the call (`approval: "grant"`: "ran while you were away"). Where bubblewrap
/// works, a sandboxed command and a worktree subagent run under the grant are logged as `bash:` (the flags only, never
/// the command) and `agent`, never their command, task or description.
///
/// Proven: logged the call's label as its audit key (in `run_tool`'s `grant.used`) → this test failed (the log held
/// the file's name); logged bash's scope key → failed (the log held the command).
#[tokio::test]
async fn every_granted_call_is_logged_without_its_input() {
    let pair = Pair::new("svc-logged").await;
    let store = grant_store(pair.home.path());
    let id = grant(&store, pair.home.path(), None, SpendCap::default()).await;
    let events = Arc::new(Events::default());
    let model = Scripted::new(vec![use_tools(vec![write("w1", "zq-secret-name.txt", "zq-secret-words")]), say("Done.")]);
    let person = Recorder::new(Decision::No);
    let handle = file_grant(&store).events(events.clone());
    let session = pair.session(&model, &person, Some(Arc::new(handle)), |_| {});
    pair.alpha_says("write it").await;
    let page = desktop_wake(&session, &pair.beta.mesh, &Mutex::default()).await.expect("a woken turn");
    assert!(pair.project().join("zq-secret-name.txt").exists(), "the grant ran it");
    assert!(person.requests().is_empty());
    let logged = events.0.lock().unwrap().clone();
    assert_eq!(logged, [KernelEvent::grant_used(id, chat(), "write_file", "write_file", 1)]);
    let lines: String = logged.iter().map(|e| marv_kernel::LoggedEvent::new(1, e.clone()).line()).collect();
    assert!(!lines.contains("zq-secret"), "{lines}");
    let marked = page.iter().find(|e| e["type"] == "tool_end").expect("the call's end");
    assert_eq!(marked["approval"], "grant", "{marked}");
    session.close().await;

    if !marv_sandbox::sandbox_available() {
        eprintln!("skipped bash and agent: bubblewrap isn't available here");
        return;
    }
    let project = TempDir::new("svc-logged-tools");
    let person = Recorder::new(Decision::No);
    let handle: Arc<dyn ChatGrant> = Arc::new(file_grant(&store).events(events.clone()));
    let mut ctx = unattended(project.path(), Arc::new(Via(handle)), &person);
    ctx.agent_host = Some(Arc::new(Subagents));
    events.0.lock().unwrap().clear();
    let bash = marv_guards::call("bash", json!({"command": "echo zq-secret-command > out.txt"}));
    let ran = run_tool(&bash, &ctx, builtin_tools()).await;
    assert_eq!(ran.result.approval, Some(Approval::Grant), "{}", ran.result.output);
    let agent = marv_guards::call(
        "agent",
        json!({"description": "zq-secret-description", "prompt": "zq-secret-task", "isolation": "worktree"}),
    );
    let ran = run_tool(&agent, &ctx, builtin_tools()).await;
    assert_eq!(ran.result.approval, Some(Approval::Grant), "{}", ran.result.output);
    assert!(person.requests().is_empty());
    let logged = events.0.lock().unwrap().clone();
    assert_eq!(
        logged,
        [KernelEvent::grant_used(id, chat(), "bash", "bash:", 1), KernelEvent::grant_used(id, chat(), "agent", "agent", 1)]
    );
    let lines: String = logged.iter().map(|e| marv_kernel::LoggedEvent::new(1, e.clone()).line()).collect();
    assert!(!lines.contains("zq-secret"), "{lines}");
}

/// Worktree subagents that start and report at once (the session's host needs a repository and a model).
struct Subagents;
#[async_trait::async_trait]
impl marv_tools::AgentHost for Subagents {
    fn agent_types(&self) -> Vec<(String, String)> {
        vec![("general-purpose".into(), "Does the task.".into())]
    }
    fn worktrees(&self) -> bool {
        true
    }
    async fn preview(
        &self,
        _input: &marv_tools::agent::AgentInput,
        _ctx: &ToolContext,
    ) -> Result<marv_tools::Preview, marv_tools::ToolError> {
        Ok(marv_tools::Preview::default())
    }
    async fn run(
        &self,
        _input: &marv_tools::agent::AgentInput,
        _ctx: &ToolContext,
    ) -> Result<marv_tools::ToolOutput, marv_tools::ToolError> {
        Ok(marv_tools::ToolOutput::ok("Done.", "done"))
    }
}

/// **What the person is told about a grant never reaches a model.** A granted turn's first request costs past the
/// cap: the grant pauses, the person is told (Marv's fixed words, once), and the edit that follows asks. Neither that
/// turn's later requests nor the person's next turn carry the words.
///
/// Proven: planted the pause's words in the session's next-message note (`pending_note`, from `count`) → this test
/// failed (the person's next turn carried them).
#[tokio::test]
async fn a_notification_never_reaches_a_model() {
    let pair = Pair::new("svc-told").await;
    let store = grant_store(pair.home.path());
    let id = grant(&store, pair.home.path(), None, SpendCap { usd_micros: 1_000, tokens: 1_000_000 }).await;
    let costly = {
        let mut reply = use_tools(vec![write("w1", "a.txt", "x")]);
        let usage = marv_core::Usage { prompt_tokens: 10, completion_tokens: 5, cached_tokens: None, cost: Some(0.01) };
        reply.insert(0, ProviderEvent::Usage(usage, marv_core::ReplyEnd::default()));
        reply
    };
    let model = Scripted::new(vec![costly, say("Waiting for you."), say("Hello.")]);
    let person = Recorder::new(Decision::Unavailable);
    let told = Arc::new(Mutex::new(Vec::new()));
    let sink = told.clone();
    let handle = file_grant(&store).on_pause(Arc::new(move |p| sink.lock().unwrap().push(p)));
    let session = pair.session(&model, &person, Some(Arc::new(handle)), |_| {});
    pair.alpha_says("write a.txt").await;
    desktop_wake(&session, &pair.beta.mesh, &Mutex::default()).await.expect("a woken turn");
    assert_eq!(*told.lock().unwrap(), [GrantPause::Spent(id)], "the person was told, once");
    assert_eq!(person.requests().len(), 1, "paused: the edit asked");
    assert!(!pair.project().join("a.txt").exists());

    use futures::StreamExt;
    let mut turn = session.send("what happened?", Default::default()).unwrap();
    while turn.next().await.is_some() {}
    let seen = history_text(&model);
    assert_eq!(model.requests.lock().unwrap().len(), 3);
    for words in [pause_text(GrantPause::Spent(id)), pause_text(GrantPause::Expired(id))] {
        assert!(!seen.contains(words), "a model saw: {words}");
    }
    session.close().await;
}

/// **A paused grant's chat still wakes, as an ungranted chat, and the person is told** (review P2.2). Beta's chat is
/// granted, but today's spend already reached its cap: a rule's message wakes it all the same (the turn runs), the
/// turn carries no grant (its edit asks the person, once, as any unattended turn's does, and doesn't run), and the
/// page gets Marv's fixed words saying the grant is paused or expired (`GRANT_NOT_LIVE`), once. The control: a live
/// grant's wake runs the edit and says nothing of the kind.
///
/// Proven: dropped the `paused` notice from `run_wake_then` → this test failed (no notice); made a paused grant's
/// wake not start (`run_wake_then` returning false when `paused`) → failed (no turn); woke a paused grant's chat as a
/// live one (`granted` from `has_grant`, not `grant_live`) → failed (no notice; its edit still asked, as the registry
/// consults the grant at every call: `a_revoked_expired_or_spent_grant_asks_at_the_next_call`).
#[tokio::test]
async fn a_paused_grant_wakes_ungranted_and_the_person_is_told() {
    let pair = Pair::new("svc-paused").await;
    let store = grant_store(pair.home.path());
    grant(&store, pair.home.path(), None, SpendCap { usd_micros: 1_000_000, tokens: 1_000 }).await;
    let handle: Arc<dyn ChatGrant> = Arc::new(file_grant(&store));
    handle.spend(Some(1.0), 0);
    assert!(handle.granted() && handle.live().is_none(), "granted, and paused at today's cap");
    let model = Scripted::new(vec![use_tools(vec![write("w1", "a.txt", "x")]), say("Waiting for you.")]);
    let person = Recorder::new(Decision::No);
    let session = pair.session(&model, &person, Some(handle), |_| {});
    assert!(session.has_grant() && !session.grant_live());
    pair.alpha_says("write a.txt").await;
    let page = desktop_wake(&session, &pair.beta.mesh, &Mutex::default()).await.expect("a paused grant's chat still wakes");
    assert_eq!(person.requests().len(), 1, "ungranted: the edit asked");
    assert!(!pair.project().join("a.txt").exists(), "and didn't run");
    let told = page.iter().filter(|e| e["type"] == "notice" && e["text"] == marv_web::host::GRANT_NOT_LIVE).count();
    assert_eq!(told, 1, "the person is told, once: {page:?}");
    let end = page.iter().find(|e| e["type"] == "tool_end").expect("the call's end");
    assert_ne!(end["approval"], "grant", "{end}");
    session.close().await;

    // The control: under a live grant the same wake runs the edit, and nothing says the grant is paused.
    let store = grant_store(pair.home.path());
    grant(&store, pair.home.path(), None, SpendCap::default()).await;
    let model = Scripted::new(vec![use_tools(vec![write("w1", "b.txt", "x")]), say("Done.")]);
    let person = Recorder::new(Decision::No);
    let session = pair.session(&model, &person, Some(Arc::new(file_grant(&store))), |_| {});
    pair.alpha_says("write b.txt").await;
    let page = desktop_wake(&session, &pair.beta.mesh, &Mutex::default()).await.expect("a woken turn");
    assert!(pair.project().join("b.txt").exists(), "the live grant ran it");
    assert!(person.requests().is_empty());
    assert!(!page.iter().any(|e| e["text"] == marv_web::host::GRANT_NOT_LIVE), "{page:?}");
    session.close().await;
}

/// **Mesh wake off wakes nothing, not even a rule's wire** (mk-60m.8 review). Alpha's message reaches beta on the
/// rule's wire, which wakes beta's chat under the default (`Rules`). With the person's `mesh.wake` off:
/// `MarvSession::wake_as` starts no turn, and the desktop's `run_wake_then` (a wake decided before the setting
/// changed, as a sleeping chat's waiting for a slot) starts none either and tells the page why. No model request,
/// and the message waits. The control: the same wake under `Rules` runs.
///
/// Proven: dropped `wake_as`'s `!wake.any()` check → this test failed (Off read the rule's message, as Rules does);
/// dropped `run_wake_then`'s → failed (no notice).
#[tokio::test]
async fn mesh_wake_off_wakes_nothing_even_on_a_rules_wire() {
    use marv_session::config::MeshWake;
    let pair = Pair::new("svc-wake-off").await;
    let model = Scripted::new(vec![say("Read it.")]);
    let person = Recorder::new(Decision::No);
    let session = pair.session(&model, &person, None, |_| {});
    pair.alpha_says("hello beta").await;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    let by = marv_session::mesh::wake_check_as(&pair.beta.mesh, &mut Default::default(), now, MeshWake::Rules)
        .wake
        .expect("the rule's wire may wake beta under the default");

    assert!(session.wake_as(&by, MeshWake::Off).unwrap().is_none(), "off: no turn");
    let (out, mut rx) = mpsc::unbounded_channel();
    assert!(!marv_web::host::run_wake_then(&session, &by, &out, MeshWake::Off, || {}).await, "off: no turn");
    let page: Vec<Value> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(page.iter().any(|e| e["text"] == marv_web::host::WAKE_OFF_NOTE), "the page is told: {page:?}");
    assert!(model.requests.lock().unwrap().is_empty(), "no model request");
    assert_eq!(pair.beta.mesh.unread(&parent_id()).len(), 1, "the message waits");

    // The control: the same wake under the default runs, and reads it.
    let turn = session.wake_as(&by, MeshWake::Rules).unwrap().expect("a woken turn");
    let mut turn = turn;
    while futures::StreamExt::next(&mut turn).await.is_some() {}
    assert!(history_text(&model).contains("hello beta"));
    session.close().await;
}
