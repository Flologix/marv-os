//! Runtime-kernel phase 2b (`docs/architecture/runtime-kernel.md` §6.4): every wire decision goes through one check,
//! `Switchboard::authorize`; a rule the person made wires their own Marvs, and sends between them ask nothing (D2),
//! while nothing else is excused; a message's hop is Marv's to set.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use marv_guards::{Recorder, Scripted, TempDir, in_child_with_env, say, tool_call, use_tools};
use marv_mesh::approval::WireRequest;
use marv_mesh::discovery::{DoorGuard, DoorInfo, open_door};
use marv_mesh::hop::TurnHop;
use marv_mesh::switchboard::{MeshRecord, Switchboard, TrajectorySink};
use marv_mesh::wire::{Approver, Grant, Wire, WireMode};
use marv_mesh::{MeshError, PlugId, ReceptacleName};
use marv_session::mesh::{DoorWires, MESH_WIRE_CALL, MeshTool, wake_check, wire_approver};
use marv_session::mesh::{WakeLimit, parent_id};
use marv_tools::{Decision, ToolRef};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Log(Mutex<Vec<MeshRecord>>);
impl TrajectorySink for Log {
    fn mesh(&self, record: MeshRecord) {
        self.0.lock().unwrap().push(record);
    }
}

/// A served switchboard in a short runtime folder.
fn switchboard(tag: &str) -> (Arc<Switchboard>, TempDir, CancellationToken) {
    named("g", tag)
}

/// [`switchboard`] with its mesh name.
fn named(session: &str, tag: &str) -> (Arc<Switchboard>, TempDir, CancellationToken) {
    let runtime = TempDir::new(tag);
    let mesh = Arc::new(Switchboard::new(session, runtime.path().join("plugs"), Arc::new(Log::default())));
    let cancel = CancellationToken::new();
    tokio::spawn(Arc::clone(&mesh).serve(cancel.clone()));
    (mesh, runtime, cancel)
}

/// **A message's hop is set by Marv from the sending turn, never by the model** (runtime-kernel 2b, §6.2 item 6). A
/// turn woken by a message at hop 1 runs at hop 2, and its `mesh_send` carries 2, whatever the model put in the call:
/// a `hop` argument or a `_meta` with `marv/hop: 0` is ignored. The person's own turn is hop 0 again.
///
/// The model's `hop` and `_meta` never reach the switchboard (`MeshTool` builds the request from the receptacle and
/// text alone), and the switchboard ignores a claimed hop on Marv's own plugs (next test).
///
/// Proven: dropped the raise in `inbox_note_hop` → this test failed (the woken turn's send carried the person's 0,
/// not 2).
#[tokio::test]
async fn the_model_cant_set_a_messages_hop() {
    use futures::StreamExt;
    let project = TempDir::new("hop-project");
    let (mesh, _rt, cancel) = switchboard("hop-rt");
    let lying = serde_json::json!({
        "receptacle": "agent.worker-1.inbox",
        "text": "pass it on",
        "hop": 0,
        "_meta": {"marv/hop": 0},
    });
    let model = Scripted::new(vec![
        use_tools(vec![tool_call("m1", "mesh_send", lying.clone())]),
        say("Sent."),
        use_tools(vec![tool_call("m2", "mesh_send", lying)]),
        say("Sent."),
    ]);
    let person = Recorder::new(marv_tools::Decision::Yes);
    let mut init = marv_session::SessionInit::new(
        project.path(),
        marv_session::ProviderChoice::Option(marv_provider::ProviderOption::Custom(model.clone())),
    );
    (init.diagnostics, init.checks, init.checkpoints) = (false.into(), false, false);
    init.mesh = Some(mesh.clone());
    init.approve = Some(person.clone());
    let session = marv_session::MarvSession::new(init).unwrap();

    // The worker speaks for a turn at hop 1; both ways wired by the person.
    let worker = mesh.plug_next("worker").unwrap();
    let worker_hop = Arc::new(TurnHop::default());
    worker_hop.set(1);
    mesh.set_turn_hop(worker.id.clone(), worker_hop);
    let inbox = ReceptacleName::new("agent.parent.inbox").unwrap();
    for (from, to) in [(worker.id.clone(), inbox.clone()), (parent_id(), ReceptacleName::inbox(&worker.id))] {
        let request = WireRequest { from: from.clone(), to, mode: WireMode::Send, asked_by: from };
        mesh.grant(request, Approver::Tui).await.unwrap();
    }
    mesh.deliver(&worker.id, inbox.as_str(), WireMode::Send, serde_json::json!({"text": "at hop 1"})).await.unwrap();
    let wake = wake_check(&mesh, &mut Default::default(), 0).wake.expect("a wake");
    let mut turn = session.wake(&wake).unwrap().expect("a woken turn");
    while turn.next().await.is_some() {}
    let woken = mesh.drain_inbox(&worker.id);
    assert_eq!(woken.len(), 1, "{:?}", person.requests().iter().map(|r| r.tool.clone()).collect::<Vec<_>>());
    assert_eq!(woken[0].hop, Some(2), "one past the message the turn read, not the model's 0");

    // The person types: hop 0 again.
    let mut turn = session.send("go on", Default::default()).unwrap();
    while turn.next().await.is_some() {}
    let typed = mesh.drain_inbox(&worker.id);
    assert_eq!(typed.len(), 1);
    assert_eq!(typed[0].hop, Some(0));
    cancel.cancel();
    session.close().await;
}

/// **A shim plug's messages carry no hop, whatever it claims; a plug of Marv's own carries its turn's** (§5.2 commit
/// 10). Over the plugs' real sockets: Claude Code on a `marv mesh plug` shim claims hop 0, and a sandboxed command on a
/// subagent's plug claims hop 0 while its turn is at hop 2.
///
/// Proven: read the claimed `_meta` hop for every plug in `Board::hop_of` → this test failed (the shim's message
/// carried 0, the subagent's 0).
#[cfg(unix)]
#[tokio::test]
async fn a_claimed_hop_is_ignored_on_a_shim_or_a_marv_plug() {
    let (mesh, _rt, cancel) = switchboard("hop-claim");
    mesh.plug(parent_id()).unwrap();
    let inbox = ReceptacleName::new("agent.parent.inbox").unwrap();
    let shim = mesh.plug(PlugId::new("claude").unwrap()).unwrap();
    let worker = mesh.plug_next("worker").unwrap();
    let hop = Arc::new(TurnHop::default());
    hop.set(2);
    mesh.set_turn_hop(worker.id.clone(), hop);
    for from in [&shim.id, &worker.id] {
        let request = WireRequest { from: from.clone(), to: inbox.clone(), mode: WireMode::Send, asked_by: from.clone() };
        mesh.grant(request, Approver::Tui).await.unwrap();
    }
    for plug in [&shim, &worker] {
        let mut socket = tokio::net::UnixStream::connect(&plug.host_socket).await.unwrap();
        let call = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": "agent.parent.inbox",
            "arguments": {"text": "claims hop 0"},
            "_meta": {"marv/mode": "send", "marv/hop": 0},
        }});
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        socket.write_all(format!("{call}\n").as_bytes()).await.unwrap();
        let mut line = String::new();
        tokio::io::BufReader::new(&mut socket).read_line(&mut line).await.unwrap();
        assert!(line.contains("Delivered"), "{line}");
    }
    let hops: Vec<(String, Option<u32>)> = mesh.drain_inbox(&parent_id()).iter().map(|e| (e.sender.to_string(), e.hop)).collect();
    assert_eq!(hops, [("claude".to_string(), None), (worker.id.to_string(), Some(2))]);
    cancel.cancel();
}

fn name(s: &str) -> ReceptacleName {
    ReceptacleName::new(s).unwrap()
}

/// The three mesh tools of `plug`, wires asked of `person` (as the TUI's would be).
fn mesh_tools(plug: &marv_mesh::plug::Plug, mesh: &Arc<Switchboard>, person: &Arc<Recorder>) -> Vec<ToolRef> {
    let wires = wire_approver(None, Some(&(person.clone() as Arc<dyn marv_tools::Approver>)));
    MeshTool::all(plug, mesh, wires).into_iter().map(|t| Arc::new(t) as ToolRef).collect()
}

fn mesh_call(tool: &str, args: serde_json::Value) -> marv_core::ToolCall {
    marv_core::ToolCall { id: "c1".into(), name: tool.into(), arguments: args.to_string() }
}

/// **Every wire check goes through `Switchboard::authorize`, and nothing else decides** (runtime-kernel §4.3, §6.4).
/// With a wire in the table for another plug (`parent -> agent.worker-2.inbox`), `worker-1` reaching that inbox is
/// refused, or asked of the person, at every public way in: `authorize`, `admit`, `deliver`, `reachable`, the plug's
/// socket (`tools/list`, `tools/call`), `wake_check`, and the mesh tools (`auto_safe`, the preview's note, and the
/// one prompt to wire it). And no code outside the switchboard reads the wire table to decide: the table's lookups
/// are crate-only, and a scan of every crate's non-test code finds them only inside the one check.
///
/// Proven: made `Board::authorize` match a wire on the receptacle and mode alone (ignoring `from`) → this test failed
/// at the first entry point (worker-1 rode the parent's wire); made `wake_check` decide with
/// `mesh.table().allows(..)` → failed (the scan found the bypass).
#[tokio::test]
async fn every_wire_check_goes_through_authorize() {
    let (mesh, _rt, cancel) = switchboard("authz");
    mesh.plug(parent_id()).unwrap();
    let worker = mesh.plug_next("worker").unwrap();
    let other = mesh.plug_next("worker").unwrap();
    let target = ReceptacleName::inbox(&other.id);
    mesh.wire(Wire::new(parent_id(), target.clone(), WireMode::Send, Grant::local(Approver::Tui))).unwrap();
    let not_wired = |e: &MeshError| matches!(e, MeshError::NotWired { .. });

    assert!(mesh.authorize(&parent_id(), &target, WireMode::Send).is_ok(), "the person's wire holds");
    let refused = mesh.authorize(&worker.id, &target, WireMode::Send).unwrap_err();
    assert!(not_wired(&refused), "authorize: {refused}");
    let refused = mesh.admit(&worker.id, &target, WireMode::Send, serde_json::json!("x"), 0).unwrap_err();
    assert!(not_wired(&refused), "admit: {refused}");
    let refused = mesh.deliver(&worker.id, target.as_str(), WireMode::Send, serde_json::json!("x")).await.unwrap_err();
    assert!(not_wired(&refused), "deliver: {refused}");
    assert!(mesh.reachable(&worker.id).is_empty(), "reachable");
    let mut client = marv_mesh::plug::PlugClient::connect(&worker.host_socket).await.unwrap();
    assert!(client.list().await.unwrap().is_empty(), "tools/list");
    let refused = client.send(&target, "x").await.unwrap_err();
    assert!(refused.to_string().starts_with("not wired"), "tools/call: {refused}");
    assert!(mesh.drain_inbox(&other.id).is_empty());

    // A message that came over a wire since removed doesn't wake.
    let back = ReceptacleName::inbox(&parent_id());
    mesh.wire(Wire::new(worker.id.clone(), back.clone(), WireMode::Send, Grant::local(Approver::Tui))).unwrap();
    mesh.deliver(&worker.id, back.as_str(), WireMode::Send, serde_json::json!("x")).await.unwrap();
    assert!(mesh.unwire(&worker.id, &back, WireMode::Send));
    assert_eq!(wake_check(&mesh, &mut WakeLimit::default(), 0).wake, None, "wake_check");

    // The mesh tools: not auto-safe in yolo, the preview says there's no wire, and running it asks the person once.
    let person = Recorder::new(Decision::No);
    let tools = mesh_tools(&worker, &mesh, &person);
    let input = serde_json::json!({"receptacle": target.as_str(), "text": "hi"});
    let mut ctx = marv_tools::ToolContext::new(std::env::temp_dir());
    ctx.yolo = true;
    let send = MeshTool::all(&worker, &mesh, None).into_iter().find(|t| t.name == "mesh_send").unwrap();
    let map = input.as_object().unwrap().clone();
    assert!(!marv_tools::Tool::auto_safe(&send, &map, &ctx), "auto_safe");
    let preview = marv_tools::Tool::preview(&send, &map, &ctx).await.unwrap();
    assert!(preview.note.unwrap_or_default().contains("has no send wire"), "the preview");
    let out = marv_tools::run_tool(&mesh_call("mesh_send", input), &ctx, &tools).await;
    let asked = person.requests();
    assert_eq!(asked.iter().map(|r| r.tool.as_str()).collect::<Vec<_>>(), [MESH_WIRE_CALL], "{}", out.result.output);
    assert!(mesh.authorize(&worker.id, &target, WireMode::Send).is_err());
    assert!(mesh.drain_inbox(&other.id).is_empty());
    cancel.cancel();

    // Nothing outside the one check reads the table to decide.
    assert_eq!(table_lookups_outside_the_check(), Vec::<String>::new());
}

/// The workspace's root (two folders up from this crate).
fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// Every Rust file of the product: each crate's `src`, and the desktop's.
fn product_sources() -> Vec<PathBuf> {
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
    let mut out = Vec::new();
    for crate_dir in std::fs::read_dir(root.join("crates")).unwrap().flatten() {
        walk(&crate_dir.path().join("src"), &mut out);
    }
    walk(&root.join("app/src-tauri/src"), &mut out);
    out.sort();
    assert!(out.len() > 50, "the scan found the sources: {}", out.len());
    out
}

/// A file's code without its test module (`#[cfg(test)]` followed by a `mod`) and without its comments.
fn non_test_code(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let end = (0..lines.len())
        .find(|&i| lines[i].trim() == "#[cfg(test)]" && lines.get(i + 1).is_some_and(|l| l.contains("mod ")))
        .unwrap_or(lines.len());
    lines[..end].iter().map(|l| l.split("//").next().unwrap_or_default()).collect::<Vec<_>>().join("\n")
}

/// Where non-test code reads the wire table's lookups, other than the one check (`Board::authorize`) and
/// `Switchboard::reachable`'s list (which keeps only what `authorize` allows).
fn table_lookups_outside_the_check() -> Vec<String> {
    let root = workspace();
    let mut found = Vec::new();
    for file in product_sources() {
        let shown = file.strip_prefix(&root).unwrap().display().to_string();
        if shown == "crates/marv-mesh/src/wire.rs" {
            continue; // the table itself
        }
        let code: String = non_test_code(&std::fs::read_to_string(&file).unwrap()).split_whitespace().collect();
        let mut hits: Vec<&str> = Vec::new();
        for needle in [".allows(", "table().check(", "table.check(", "table().find(", "table.find(", ".wires_from(", "WireTable"]
        {
            hits.extend(std::iter::repeat_n(needle, code.matches(needle).count()));
        }
        let allowed: &[&str] = match shown.as_str() {
            // The board's table (its type in a `use`, the field, `table()`'s snapshot), the one check, and the list.
            "crates/marv-mesh/src/switchboard.rs" => &["table.check(", ".wires_from(", "WireTable", "WireTable", "WireTable"],
            "crates/marv-mesh/src/lib.rs" => &["WireTable"],
            _ => &[],
        };
        let mut rest = hits.clone();
        for ok in allowed {
            if let Some(i) = rest.iter().position(|h| h == ok) {
                rest.remove(i);
            }
        }
        found.extend(rest.into_iter().map(|h| format!("{shown}: {h}")));
    }
    found
}

/// One Marv with a door in `dir`: its switchboard (the main agent plugged in), its person, and its door.
struct Marv {
    mesh: Arc<Switchboard>,
    person: Arc<Recorder>,
    _door: DoorGuard,
    _runtime: TempDir,
    cancel: CancellationToken,
}

impl Drop for Marv {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

fn marv(name: &str, dir: &Path, folder: &Path, answer: Decision) -> Marv {
    let (mesh, runtime, cancel) = named(name, &format!("{name}-rt"));
    mesh.plug(parent_id()).unwrap();
    let person = Recorder::new(answer);
    let wires = wire_approver(None, Some(&(person.clone() as Arc<dyn marv_tools::Approver>))).unwrap();
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
    Marv { mesh, person, _door: door, _runtime: runtime, cancel }
}

/// **Seeing an agent is never permission** (runtime-kernel §6.2 item 1). After `mesh_list` (and `marv mesh ls`) has
/// shown alpha the Marv beta and its inbox, alpha still has no wire to it: the one check refuses, nothing is wired on
/// either end, and a `mesh_send` there in yolo asks alpha's person, whose no leaves it unwired and unsent.
///
/// Proven: made `MeshTool::wireable` grant a wire to each listed Marv's inbox (`mesh.grant(.., Approver::Tui)`, beta's
/// person saying yes) → this test failed (the listed inbox passed `authorize`).
#[tokio::test]
async fn seeing_an_agent_in_mesh_list_grants_nothing() {
    let doors = TempDir::new("seen");
    let dir = doors.path().join("mesh");
    let alpha = marv("alpha", &dir, &doors.path().join("a"), Decision::No);
    let beta = marv("beta", &dir, &doors.path().join("b"), Decision::Yes);
    let listed = marv_mesh::discovery::list(&dir).await.unwrap();
    assert!(listed.iter().any(|p| p.session == "beta"), "marv mesh ls sees beta");
    let plug = alpha.mesh.plug(parent_id()).unwrap();
    let tools = mesh_tools(&plug, &alpha.mesh, &alpha.person);
    let mut ctx = marv_tools::ToolContext::new(std::env::temp_dir());
    ctx.yolo = true;
    let out = marv_tools::run_tool(&mesh_call("mesh_list", serde_json::json!({})), &ctx, &tools).await;
    assert!(out.result.output.contains("peer.beta.agent.parent.inbox"), "{}", out.result.output);

    let to_beta = name("peer.beta.agent.parent.inbox");
    assert!(alpha.mesh.authorize(&parent_id(), &to_beta, WireMode::Send).is_err(), "listed, not wired");
    assert!(alpha.mesh.table().all().is_empty() && beta.mesh.table().all().is_empty());
    assert!(alpha.person.requests().is_empty() && beta.person.requests().is_empty(), "listing asks nobody");

    let send = mesh_call("mesh_send", serde_json::json!({"receptacle": to_beta.as_str(), "text": "hi"}));
    let out = marv_tools::run_tool(&send, &ctx, &tools).await;
    assert_eq!(alpha.person.requests().iter().map(|r| r.tool.clone()).collect::<Vec<_>>(), [MESH_WIRE_CALL]);
    assert!(out.result.declined, "{}", out.result.output);
    assert!(beta.person.requests().is_empty(), "alpha's no stopped it before beta was asked");
    assert!(alpha.mesh.authorize(&parent_id(), &to_beta, WireMode::Send).is_err());
    assert!(beta.mesh.drain_inbox(&parent_id()).is_empty());
}

/// **Being in the registry is never permission either** (runtime-kernel 3, §6.2 item 1). Alpha and beta share a
/// registry as a desktop's chats do: both entered as its own (authoritative), each with its inbox and a scheduler
/// state, and listed by `mesh_list` from it. Still: the one check refuses alpha's send to beta, nothing is wired on
/// either end, `wake_check` finds nothing to wake beta with, and a `mesh_send` in yolo asks alpha's person, whose no
/// leaves it unwired and unsent. And no code that admits, wires or wakes reads the registry: a scan of the product's
/// non-test code finds it read only where `mesh_list` lists and where it's filled.
///
/// Proven: made `Board::authorize` admit a message to an inbox whose Marv is in the switchboard's registry → this test
/// failed (the registered inbox passed `authorize`), and the source scan listed the new read in `switchboard.rs`.
#[tokio::test]
async fn being_in_the_registry_grants_nothing() {
    let doors = TempDir::new("reg-seen");
    let dir = doors.path().join("mesh");
    let alpha = marv("alpha", &dir, &doors.path().join("a"), Decision::No);
    let beta = marv("beta", &dir, &doors.path().join("b"), Decision::Yes);
    let registry = marv_mesh::registry::MeshRegistry::new();
    for m in [&alpha, &beta] {
        m.mesh.set_registry(registry.clone());
        registry.register_own(&m.mesh, &m.mesh.identity(&parent_id()).unwrap(), &dir, None);
        registry.set_state(&m.mesh.session, marv_kernel::AgentState::Idle);
    }
    let seen = registry.snapshot();
    assert!(seen.named("beta").unwrap().can(&marv_kernel::Reach::Inbox("parent".into())), "beta is registered");

    let plug = alpha.mesh.plug(parent_id()).unwrap();
    let tools = mesh_tools(&plug, &alpha.mesh, &alpha.person);
    let mut ctx = marv_tools::ToolContext::new(std::env::temp_dir());
    ctx.yolo = true;
    let out = marv_tools::run_tool(&mesh_call("mesh_list", serde_json::json!({})), &ctx, &tools).await;
    assert!(out.result.output.contains("peer.beta.agent.parent.inbox"), "{}", out.result.output);

    let to_beta = name("peer.beta.agent.parent.inbox");
    assert!(alpha.mesh.authorize(&parent_id(), &to_beta, WireMode::Send).is_err(), "registered, not wired");
    assert!(alpha.mesh.table().all().is_empty() && beta.mesh.table().all().is_empty());
    assert_eq!(wake_check(&beta.mesh, &mut WakeLimit::default(), 1).wake, None);
    assert!(alpha.person.requests().is_empty() && beta.person.requests().is_empty(), "registering asks nobody");

    let send = mesh_call("mesh_send", serde_json::json!({"receptacle": to_beta.as_str(), "text": "hi"}));
    let out = marv_tools::run_tool(&send, &ctx, &tools).await;
    assert_eq!(alpha.person.requests().iter().map(|r| r.tool.clone()).collect::<Vec<_>>(), [MESH_WIRE_CALL]);
    assert!(out.result.declined, "{}", out.result.output);
    assert!(beta.person.requests().is_empty());
    assert!(alpha.mesh.authorize(&parent_id(), &to_beta, WireMode::Send).is_err());
    assert!(beta.mesh.drain_inbox(&parent_id()).is_empty());

    assert_eq!(registry_reads(), Vec::<String>::new());
}

/// Where non-test code reads or fills a registry, beyond the places that list (`mesh_list`'s `wireable`), fill it
/// (the door scan, the radio's re-list, a desktop's chats, a Marv entering itself) or hold it (the switchboard's
/// field, its setter and getter): every other hit would be a decision reading what an agent sees.
fn registry_reads() -> Vec<String> {
    let root = workspace();
    let mut found = Vec::new();
    for file in product_sources() {
        let shown = file.strip_prefix(&root).unwrap().display().to_string();
        if shown == "crates/marv-mesh/src/registry.rs" || shown == "crates/marv-kernel/src/registry.rs" {
            continue; // the registries themselves
        }
        if shown == "crates/marv-mcp/src/install/registry.rs" {
            continue; // the MCP server catalogue: another registry, of servers to install
        }
        let code: String = non_test_code(&std::fs::read_to_string(&file).unwrap()).split_whitespace().collect();
        let mut hits: Vec<String> = Vec::new();
        for needle in ["MeshRegistry", ".registry()", "registry.", "kernel::Registry", "Registry::new"] {
            hits.extend(std::iter::repeat_n(needle.to_string(), code.matches(needle).count()));
        }
        let allowed: usize = match shown.as_str() {
            "crates/marv-mesh/src/switchboard.rs" => REGISTRY_IN_SWITCHBOARD,
            "crates/marv-mesh/src/radio.rs" => REGISTRY_IN_RADIO,
            "crates/marv-session/src/mesh.rs" => REGISTRY_IN_SESSION_MESH,
            "crates/marv-session/src/mesh_process.rs" => REGISTRY_IN_MESH_PROCESS,
            "app/src-tauri/src/chats.rs" => REGISTRY_IN_CHATS,
            "crates/marv-cli/src/tui.rs" => REGISTRY_IN_TUI,
            "crates/marv-kernel/src/eventlog.rs" => REGISTRY_IN_REPLAY,
            "crates/marv-cli/src/kernel_cmd.rs" => REGISTRY_IN_REPLAY_CMD,
            _ => 0,
        };
        if hits.len() != allowed {
            found.push(format!("{shown}: {} registry reads, expected {allowed}: {hits:?}", hits.len()));
        }
    }
    found
}

/// The registry mentions each file may have: the switchboard's field, setter and getter; the radio's fill; the
/// session's one list; a Marv entering itself (and logging its entry); the desktop filling its chats (and their
/// states); the TUI entering itself; the event log's replay rebuilding one, and `marv kernel replay` printing it.
/// Runtime-kernel §14.4 adds one each, both shown or logged only: the switchboard noting its edit tools' write
/// (`MeshRegistry::noted_write`) and the desktop reading its chats' write overlaps (`overlaps_of`). Runtime-kernel
/// coordination adds one to the session's mesh: a task wake looking up its sender's published role
/// (`mesh::sender_role`), which decides only whether a turn already woken runs as a task inside its chat's own role,
/// never what's admitted, wired or woken.
const REGISTRY_IN_SWITCHBOARD: usize = 8;
const REGISTRY_IN_RADIO: usize = 1;
const REGISTRY_IN_SESSION_MESH: usize = 2;
const REGISTRY_IN_MESH_PROCESS: usize = 4;
/// The desktop's chats: their entering and states, and (the harness plan's terminals) a chat whose door is lent to its
/// terminal's Marv leaving the registry meanwhile (`Chats::to_terminal`, `unregister_own`): it removes an entry, it
/// grants nothing (the terminal's Marv enters itself under the same name).
const REGISTRY_IN_CHATS: usize = 10;
const REGISTRY_IN_TUI: usize = 1;
const REGISTRY_IN_REPLAY: usize = 3;
const REGISTRY_IN_REPLAY_CMD: usize = 2;

/// **A project folder is never a source of rules** (runtime-kernel §6.2 item 2, D4/D7). A project holding
/// `.marv/mesh/policies.json` (a valid rule) and a `.marv/config.json` naming policies: the rules store is the user's
/// config folder's (`~/.marv`, or an absolute `MARV_CONFIG_DIR`), so the project's file is never read; a relative
/// config folder (one that would resolve inside the project) is refused for rules outright.
///
/// Proven: made `PolicyStore::new` accept a relative config folder → this test failed.
#[test]
fn a_projects_policies_json_is_never_read() {
    let base = TempDir::new("proj-rules");
    let (home, project) = (base.path().join("home"), base.path().join("home/Projects/acme"));
    std::fs::create_dir_all(&project).unwrap();
    let rule = r#"{"version": 1, "generation": 1, "next_id": 2, "rules": [{"id": 1, "from": {"folder": "/"}, "to": {"folder": "/"}, "mode": "send", "by": "cli", "created_at_ms": 1}]}"#;
    let planted = project.join(".marv/mesh/policies.json");
    std::fs::create_dir_all(planted.parent().unwrap()).unwrap();
    std::fs::write(&planted, rule).unwrap();
    std::fs::write(project.join(".marv/config.json"), r#"{"mesh": {"policies": [{"from": "/", "to": "/"}]}}"#).unwrap();

    let env = marv_mcp::Env::new();
    let store = marv_kernel::PolicyStore::new(&marv_session::config::default_config_dir(&env, &home), &home).unwrap();
    assert_eq!(store.path(), home.join(".marv/mesh/policies.json"));
    assert_eq!(store.load().unwrap(), marv_kernel::PolicySnapshot::default(), "the user has no rules");
    let relative: marv_mcp::Env = [("MARV_CONFIG_DIR".to_string(), ".marv".to_string())].into_iter().collect();
    let dir = marv_session::config::default_config_dir(&relative, &home);
    assert!(marv_kernel::PolicyStore::new(&dir, &home).is_err(), "a relative config folder never holds rules");
}

const INERT: &str = "where_rules_arent_honored_a_matching_rule_changes_nothing";

/// **Where rules aren't honored, a matching rule changes nothing** (runtime-kernel 2b, D8). A switchboard the session
/// didn't give the person's rules (`marv -p`, an SDK client, these test Marvs) behaves as with no rules even when the
/// person's `~/.marv/mesh/policies.json` holds a rule that matches ("chats under ~/Projects/acme may send to each
/// other", checked here with `match_rule` on the two Marvs' identities): the one check refuses the unwired send; a
/// `mesh_send` asks alpha's person, then beta's (both prompts, as today); either one's no leaves it unwired and unsent;
/// only both yeses wire it, as a person's wire.
///
/// And the rules are read only where they should be: the switchboard's own rule check (`rule_grant`, `authorize`), the
/// session's `honor_rules` (which only the TUI and the desktop's chat host call; `marv -p` never does), and the
/// desktop's Mesh panel and `marv mesh allow|revoke|policies`, where the person makes and revokes them.
///
/// Run in a child with `HOME` and `MARV_CONFIG_DIR` pointing at a fake home, so the rule is where Marv would look.
///
/// Proven: made `Switchboard::authorize` let a `peer.*` receptacle through when the user's `policies.json` held a rule
/// → this test failed (in the child: the unwired send passed the one check); called `honor_rules` from headless.rs →
/// it failed.
#[tokio::test]
async fn where_rules_arent_honored_a_matching_rule_changes_nothing() {
    if std::env::var("MARV_GUARD_CHILD").as_deref() != Ok(INERT) {
        let home = TempDir::new("inert-home");
        let config = home.path().join(".marv");
        let (h, c) = (home.path().display().to_string(), config.display().to_string());
        assert!(!in_child_with_env(INERT, &[("HOME", h.as_str()), ("MARV_CONFIG_DIR", c.as_str())]));
        return;
    }
    let home = PathBuf::from(std::env::var("HOME").unwrap());
    let env: marv_mcp::Env = [("MARV_CONFIG_DIR".to_string(), std::env::var("MARV_CONFIG_DIR").unwrap())].into_iter().collect();
    let store = marv_kernel::PolicyStore::new(&marv_session::config::default_config_dir(&env, &home), &home).unwrap();
    let acme = home.join("Projects/acme");
    let rule = marv_kernel::NewRule {
        from: marv_kernel::Subject::Folder(marv_kernel::FolderScope::new(&acme, &home).unwrap()),
        to: marv_kernel::Subject::Folder(marv_kernel::FolderScope::new(&acme, &home).unwrap()),
        mode: marv_kernel::Mode::Send,
        mutual: true,
        expires_at_ms: None,
        by: marv_kernel::RuleBy::Desktop,
        created_at_ms: 1,
        label: "Acme".into(),
    };
    store.add(rule).await.unwrap();
    let rules = store.load().unwrap();
    assert_eq!(store.path(), home.join(".marv/mesh/policies.json"));

    let doors = TempDir::new("inert");
    let dir = doors.path().join("mesh");
    for (alpha_says, beta_says) in [(Decision::No, Decision::Yes), (Decision::Yes, Decision::No), (Decision::Yes, Decision::Yes)]
    {
        let alpha = marv("alpha", &dir, &acme.join("api"), alpha_says.clone());
        let beta = marv("beta", &dir, &acme.join("web"), beta_says.clone());
        let (a, b) = (alpha.mesh.identity(&parent_id()).unwrap(), beta.mesh.identity(&parent_id()).unwrap());
        let access = marv_kernel::Access { from: &a, to: &b, mode: marv_kernel::Mode::Send, now_ms: 2 };
        assert!(marv_kernel::match_rule(&rules, &access).is_ok(), "the stored rule matches the two");

        let to_beta = name("peer.beta.agent.parent.inbox");
        assert!(alpha.mesh.authorize(&parent_id(), &to_beta, WireMode::Send).is_err(), "rules aren't honored yet");
        let plug = alpha.mesh.plug(parent_id()).unwrap();
        let tools = mesh_tools(&plug, &alpha.mesh, &alpha.person);
        let mut ctx = marv_tools::ToolContext::new(std::env::temp_dir());
        ctx.yolo = true;
        let send = mesh_call("mesh_send", serde_json::json!({"receptacle": to_beta.as_str(), "text": "hi"}));
        let out = marv_tools::run_tool(&send, &ctx, &tools).await;
        assert_eq!(alpha.person.requests().len(), 1, "alpha's person asked: {}", out.result.output);
        let beta_asked = usize::from(alpha_says == Decision::Yes);
        assert_eq!(beta.person.requests().len(), beta_asked, "beta's person asked when alpha's said yes");
        let both = alpha_says == Decision::Yes && beta_says == Decision::Yes;
        assert_eq!(alpha.mesh.authorize(&parent_id(), &to_beta, WireMode::Send).is_ok(), both);
        let delivered = beta.mesh.drain_inbox(&parent_id());
        assert_eq!(delivered.len(), usize::from(both), "{}", out.result.output);
        if both {
            let wire = alpha.mesh.authorize(&parent_id(), &to_beta, WireMode::Send).unwrap();
            assert_eq!(wire.grant, Grant { local: Approver::Tui, remote: Some(Approver::Tui) }, "a person's wire");
        }
    }

    // The rules are read only by the switchboard's rule check, the session's honor_rules and the person's own tools
    // (the Mesh panel and New team, marv mesh allow, marv team start).
    let root = workspace();
    let in_code = |names: &[&str]| -> Vec<String> {
        product_sources()
            .into_iter()
            .filter(|f| !f.starts_with(root.join("crates/marv-kernel")))
            .filter(|f| {
                let code = non_test_code(&std::fs::read_to_string(f).unwrap());
                names.iter().any(|n| code.contains(n))
            })
            .map(|f| f.strip_prefix(&root).unwrap().display().to_string())
            .collect()
    };
    let readers = in_code(&["PolicyStore", "match_rule", "Capability::issue", "policies.json", "FileRules"]);
    assert_eq!(
        readers,
        [
            "app/src-tauri/src/lib.rs",
            "crates/marv-cli/src/mesh_cmd.rs",
            "crates/marv-cli/src/team_cmd.rs",
            "crates/marv-mesh/src/switchboard.rs",
            "crates/marv-session/src/mesh_process.rs"
        ]
    );
    // And honored only where the person is at a Marv of their own: the TUI and the desktop's chat host.
    let mut honoring = in_code(&["honor_rules(", "set_rules("]);
    honoring.sort();
    assert_eq!(
        honoring,
        [
            "crates/marv-cli/src/tui.rs",
            "crates/marv-mesh/src/switchboard.rs",
            "crates/marv-session/src/mesh_process.rs",
            "crates/marv-web/src/host.rs"
        ]
    );
}

/// **Live comms: under a rule, a send to another of the person's own Marvs asks nothing, and nothing else is
/// excused** (runtime-kernel 2b, D2; docs/SECURITY.md, "Live comms between the person's own Marvs"). Alpha's session
/// is tainted and the turn was started by a message (automatic approval off), and every person here would say no:
/// - under a rule covering `~/Projects/acme`, alpha's `mesh_send`s to beta wire, run and arrive, nobody asked;
/// - to gamma, wired by the people themselves and no rule, each send still asks (must_ask) and is declined;
/// - a worker (subagent) of alpha's is never covered: it asks;
/// - revoked, the next send to beta asks, and nothing more arrives.
///
/// Proven: made `MeshTool::own_marvs_send` accept any wire (dropped `is_policy()`) → this test failed (the send to
/// gamma on the people's wire ran unasked in the tainted session); dropped the `own_marvs` term from `must_ask` in
/// `run_tool` → failed (the sends to beta asked).
#[tokio::test]
async fn under_a_rule_sends_to_the_persons_own_marvs_ask_nothing_and_nothing_else_is_excused() {
    let home = TempDir::new("live-home");
    let acme = home.path().join("Projects/acme");
    let store = marv_kernel::PolicyStore::new(&home.path().join(".marv"), home.path()).unwrap();
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
    let (rule, _) = store.add(rule).await.unwrap();

    let doors = TempDir::new("live");
    let dir = doors.path().join("mesh");
    let alpha = marv("alpha", &dir, &acme.join("api"), Decision::No);
    let beta = marv("beta", &dir, &acme.join("web"), Decision::No);
    let gamma = marv("gamma", &dir, &home.path().join("Other/site"), Decision::Yes);
    for m in [&alpha, &beta, &gamma] {
        m.mesh.set_rules(Arc::new(marv_kernel::FileRules::new(store.clone())));
    }
    let plug = alpha.mesh.plug(parent_id()).unwrap();
    let tools = mesh_tools(&plug, &alpha.mesh, &alpha.person);
    let mut ctx = marv_tools::ToolContext::new(std::env::temp_dir());
    ctx.mode = marv_tools::CallMode::Woken;
    ctx.approver = Some(alpha.person.clone());
    ctx.taint.mark(marv_core::Provenance::Mesh);
    let send = |to: &str, text: &str| mesh_call("mesh_send", serde_json::json!({"receptacle": to, "text": text}));

    for text in ["hello beta", "and again"] {
        let out = marv_tools::run_tool(&send("peer.beta.agent.parent.inbox", text), &ctx, &tools).await;
        assert!(!out.result.declined && !out.result.is_error, "{}", out.result.output);
    }
    assert!(alpha.person.requests().is_empty() && beta.person.requests().is_empty(), "nobody was asked");
    let got: Vec<String> = beta.mesh.drain_inbox(&parent_id()).iter().map(|m| m.text()).collect();
    assert_eq!(got, ["hello beta", "and again"]);

    // Gamma: the people wired it themselves (no rule covers ~/Other). In the tainted session each send still asks.
    let to_gamma = ReceptacleName::new("peer.gamma.agent.parent.inbox").unwrap();
    alpha.mesh.lookup(&to_gamma).await.unwrap();
    let request = WireRequest { from: parent_id(), to: to_gamma.clone(), mode: WireMode::Send, asked_by: parent_id() };
    alpha.mesh.grant(request, Approver::Tui).await.unwrap();
    let out = marv_tools::run_tool(&send(to_gamma.as_str(), "hi gamma"), &ctx, &tools).await;
    assert!(out.result.declined, "{}", out.result.output);
    let asked = alpha.person.requests();
    assert_eq!(asked.len(), 1);
    assert!(asked[0].must_ask, "tainted egress, as before");
    assert!(gamma.mesh.drain_inbox(&parent_id()).is_empty());

    // A worker of alpha's is never covered by a rule.
    let worker = alpha.mesh.plug_next("worker").unwrap();
    let worker_tools = mesh_tools(&worker, &alpha.mesh, &alpha.person);
    let out = marv_tools::run_tool(&send("peer.beta.agent.parent.inbox", "from a worker"), &ctx, &worker_tools).await;
    assert!(out.result.declined || out.result.is_error, "{}", out.result.output);
    assert!(alpha.person.requests().len() > 1, "the worker's send was asked about");

    // Revoked: the next send to beta asks (and is declined); nothing more arrives.
    let before = alpha.person.requests().len();
    store.revoke(rule).await.unwrap();
    let out = marv_tools::run_tool(&send("peer.beta.agent.parent.inbox", "after revoke"), &ctx, &tools).await;
    assert!(out.result.declined || out.result.is_error, "{}", out.result.output);
    assert!(alpha.person.requests().len() > before, "asked again once the rule was gone");
    assert!(beta.mesh.drain_inbox(&parent_id()).is_empty());
}

/// A rule over `<home>/Projects/acme` (send, both ways) in a temp config folder, honored by the given Marvs.
async fn acme_rule(home: &Path, expires_at_ms: Option<u64>, marvs: &[&Marv]) -> marv_kernel::PolicyStore {
    labelled_acme_rule(home, expires_at_ms, "Acme", marvs).await
}

async fn labelled_acme_rule(home: &Path, expires_at_ms: Option<u64>, label: &str, marvs: &[&Marv]) -> marv_kernel::PolicyStore {
    let acme = home.join("Projects/acme");
    let store = marv_kernel::PolicyStore::new(&home.join(".marv"), home).unwrap();
    let scope = || marv_kernel::Subject::Folder(marv_kernel::FolderScope::new(&acme, home).unwrap());
    let rule = marv_kernel::NewRule {
        from: scope(),
        to: scope(),
        mode: marv_kernel::Mode::Send,
        mutual: true,
        expires_at_ms,
        by: marv_kernel::RuleBy::Desktop,
        created_at_ms: 1,
        label: label.into(),
    };
    store.add(rule).await.unwrap();
    for m in marvs {
        m.mesh.set_rules(Arc::new(marv_kernel::FileRules::new(store.clone())));
    }
    store
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}

/// **Only the person's own tools make a rule** (§6.4: a model, a mesh message, yolo or a project's config can't make
/// or widen one). A rule is a `marv_kernel::NewRule` written to the user's own file; outside marv-kernel, product code
/// builds one in exactly three places: the desktop's Mesh panel and New team (`allow_folder`/`start_team`, Tauri
/// commands the page calls on the person's click), `marv mesh allow` (written only after a y at a terminal,
/// `mesh_cmd::allow_with`, tested there) and `marv team start` (the same y, `team_cmd::start_with`, tested there). No tool, no mesh or radio handler, no config loader builds one, and the rules file lives in the config
/// folder every sandbox hides.
///
/// Proven: built a `NewRule` in `MeshTool::run` (a mesh_send making a rule for its receptacle) → this test failed.
#[test]
fn only_the_persons_own_tools_make_a_rule() {
    let root = workspace();
    let mut makers: Vec<String> = product_sources()
        .into_iter()
        .filter(|f| !f.starts_with(root.join("crates/marv-kernel")))
        .filter(|f| non_test_code(&std::fs::read_to_string(f).unwrap()).contains("NewRule {"))
        .map(|f| f.strip_prefix(&root).unwrap().display().to_string())
        .collect();
    makers.sort();
    assert_eq!(makers, ["app/src-tauri/src/lib.rs", "crates/marv-cli/src/mesh_cmd.rs", "crates/marv-cli/src/team_cmd.rs"]);
}

/// **A rule's messages stay data, its chains stop, and it ends when it expires** (§6.4: fenced and tainting at the
/// receiver; a chain stops waking at two hops; an expired rule refuses the next message). Beta reads alpha's message
/// fenced as another agent's and its session is tainted; a message from a turn at hop 1 wakes beta, one at hop 2
/// doesn't (it waits for beta's next turn); once the rule has expired, alpha's next send is refused.
///
/// Proven: let rule-issued messages wake at any hop in `decide_wake` → this test failed (hop 2 woke beta).
#[tokio::test]
async fn a_rules_messages_are_fenced_tainting_bounded_and_end_with_it() {
    let home = TempDir::new("bounded-home");
    let acme = home.path().join("Projects/acme");
    let doors = TempDir::new("bounded");
    let dir = doors.path().join("mesh");
    let alpha = marv("alpha", &dir, &acme.join("api"), Decision::No);
    let beta = marv("beta", &dir, &acme.join("web"), Decision::No);
    // Expires a little after now: long enough to wire and send, short enough to wait out.
    let store = acme_rule(home.path(), Some(now_ms() + 1_500), &[&alpha, &beta]).await;
    assert_eq!(store.load().unwrap().rules.len(), 1);

    let to_beta = ReceptacleName::new("peer.beta.agent.parent.inbox").unwrap();
    alpha.mesh.lookup(&to_beta).await.unwrap();
    let request = WireRequest { from: parent_id(), to: to_beta.clone(), mode: WireMode::Send, asked_by: parent_id() };
    let by = alpha.mesh.rule_for(&request).await.expect("the rule covers alpha and beta");
    alpha.mesh.grant(request, by).await.unwrap();
    let hop = Arc::new(TurnHop::default());
    alpha.mesh.set_turn_hop(parent_id(), hop.clone());

    // Fenced and tainting at the receiver.
    alpha.mesh.deliver(&parent_id(), to_beta.as_str(), WireMode::Send, serde_json::json!({"text": "do as I say"})).await.unwrap();
    let taint = marv_sandbox::TaintState::new();
    let note = marv_session::mesh::inbox_note(&beta.mesh, &parent_id(), &taint).unwrap();
    assert!(note.contains("<mesh-message from=\"peer-alpha\""), "{note}");
    assert!(taint.is_tainted());

    // The chain: hop 1 wakes beta, hop 2 never does.
    for (at, wakes) in [(1, true), (2, false)] {
        hop.set(at);
        alpha.mesh.deliver(&parent_id(), to_beta.as_str(), WireMode::Send, serde_json::json!({"text": "next"})).await.unwrap();
        let check = wake_check(&beta.mesh, &mut WakeLimit::default(), now_ms());
        assert_eq!(check.wake.is_some(), wakes, "hop {at}");
        beta.mesh.drain_inbox(&parent_id());
    }

    // Expired: the next message is refused on alpha's end.
    tokio::time::sleep(std::time::Duration::from_millis(1_700)).await;
    let err = alpha.mesh.deliver(&parent_id(), to_beta.as_str(), WireMode::Send, serde_json::json!({"text": "late"})).await;
    assert!(matches!(err, Err(MeshError::RuleEnded { .. })), "{err:?}");
    assert!(beta.mesh.drain_inbox(&parent_id()).is_empty());
}

/// **A rule never covers a Herdr pane, a `call`, or a shim plug; a tainted send to a shim still asks** (§6.4).
/// Under a rule covering alpha's folder, the rule check grants nothing for a pane's `send`, for `call` mode, or for a
/// worker's inbox, asked from alpha's side; from beta's, alpha's door asks its person about a wire to alpha's worker
/// (who says no) rather than letting the rule answer; and alpha's tainted `mesh_send` to a shim plugged into its own
/// switchboard (say Claude Code) is asked of the person (who says no), not excused.
///
/// Proven: dropped the main-inbox checks in `Door::wire` and `Switchboard::rule_grant` → this test failed (the rule
/// wired beta to alpha's worker). The caller's side (`rule_for`) refuses those receptacles on its own as well.
#[tokio::test]
async fn rules_never_cover_a_pane_a_call_or_a_shim() {
    let home = TempDir::new("cover-home");
    let acme = home.path().join("Projects/acme");
    let doors = TempDir::new("cover");
    let dir = doors.path().join("mesh");
    let alpha = marv("alpha", &dir, &acme.join("api"), Decision::No);
    let beta = marv("beta", &dir, &acme.join("web"), Decision::Yes);
    acme_rule(home.path(), None, &[&alpha, &beta]).await;
    let ask = |to: &str, mode: WireMode| WireRequest {
        from: parent_id(),
        to: ReceptacleName::new(to).unwrap(),
        mode,
        asked_by: parent_id(),
    };
    assert!(alpha.mesh.rule_for(&ask("peer.beta.agent.parent.inbox", WireMode::Send)).await.is_some(), "the control");
    for (to, mode) in [
        ("herdr.pane.w1-p2.send", WireMode::Send),
        ("peer.beta.agent.parent.inbox", WireMode::Call),
        ("peer.beta.agent.worker-1.inbox", WireMode::Send),
        ("agent.claude.inbox", WireMode::Send),
    ] {
        assert_eq!(alpha.mesh.rule_for(&ask(to, mode)).await, None, "{to} ({mode})");
    }

    // From beta: alpha's door asks its person about a wire to alpha's worker, whatever the rule says.
    let worker = alpha.mesh.plug_next("worker").unwrap();
    let to_worker = ReceptacleName::new(format!("peer.alpha.agent.{}.inbox", worker.id)).unwrap();
    beta.mesh.lookup(&to_worker).await.expect("alpha's worker is on its door");
    let request = WireRequest { from: parent_id(), to: to_worker.clone(), mode: WireMode::Send, asked_by: parent_id() };
    assert!(beta.mesh.grant(request, Approver::Tui).await.is_err(), "alpha's person said no");
    assert_eq!(alpha.person.requests().len(), 1, "asked, not answered by the rule");
    assert!(beta.mesh.authorize(&parent_id(), &to_worker, WireMode::Send).is_err());

    // A shim on alpha's own switchboard, wired by the person: alpha's tainted send to it still asks.
    let shim = PlugId::new("claude").unwrap();
    alpha.mesh.plug(shim.clone()).unwrap();
    let to_shim = ReceptacleName::inbox(&shim);
    let request = WireRequest { from: parent_id(), to: to_shim.clone(), mode: WireMode::Send, asked_by: parent_id() };
    alpha.mesh.grant(request, Approver::Tui).await.unwrap();
    let plug = alpha.mesh.plug(parent_id()).unwrap();
    let tools = mesh_tools(&plug, &alpha.mesh, &alpha.person);
    let mut ctx = marv_tools::ToolContext::new(std::env::temp_dir());
    ctx.yolo = true;
    ctx.approver = Some(alpha.person.clone());
    ctx.taint.mark(marv_core::Provenance::Web);
    let send = mesh_call("mesh_send", serde_json::json!({"receptacle": to_shim.as_str(), "text": "secrets"}));
    let out = marv_tools::run_tool(&send, &ctx, &tools).await;
    assert!(out.result.declined, "{}", out.result.output);
    assert!(alpha.person.requests().iter().any(|r| r.must_ask));
    assert!(alpha.mesh.drain_inbox(&shim).is_empty());
}

/// **By default an idle TUI wakes only for a rule's wire; `"wake": false` stops every wake** (runtime-kernel live UX).
/// The TUI's default (no `mesh.wake` in the person's file) is `MeshWake::Rules`: a message on a wire a rule the person
/// made issued wakes it, within the rule wakes' hop limit, while one on a wire the person approved by hand waits for
/// their next turn, as before; with `false` neither wakes, and with `true` both do.
///
/// Proven: dropped the `rule_issued` filter in `wake_check_as` → this test failed (the person's wire woke under
/// `Rules`); made `Off` pass every candidate → failed too.
#[tokio::test]
async fn the_default_wake_is_a_rules_wire_only_and_off_is_off() {
    use marv_session::config::MeshWake;
    use marv_session::mesh::wake_check_as;
    let home = TempDir::new("defwake-home");
    let acme = home.path().join("Projects/acme");
    let doors = TempDir::new("defwake");
    let dir = doors.path().join("mesh");
    let alpha = marv("alpha", &dir, &acme.join("api"), Decision::No);
    let beta = marv("beta", &dir, &acme.join("web"), Decision::No);
    let _store = acme_rule(home.path(), None, &[&alpha, &beta]).await;
    assert_eq!(marv_session::config::resolve_config(None, &Default::default()).mesh_wake, MeshWake::Rules);

    // A worker of beta's on a wire beta's person approved by hand.
    let inbox = name("agent.parent.inbox");
    let worker = beta.mesh.plug_next("worker").unwrap().id;
    let request = WireRequest { from: worker.clone(), to: inbox.clone(), mode: WireMode::Send, asked_by: worker.clone() };
    beta.mesh.grant(request, Approver::Tui).await.unwrap();
    beta.mesh.deliver(&worker, inbox.as_str(), WireMode::Send, serde_json::json!({"text": "by hand"})).await.unwrap();
    let check = |wake| wake_check_as(&beta.mesh, &mut WakeLimit::default(), now_ms(), wake).wake;
    assert_eq!(check(MeshWake::Rules), None, "the person's wire doesn't wake by default");
    assert_eq!(check(MeshWake::On).map(|b| b.sender), Some(worker.clone()), "it does with wake on");

    // Alpha, over the rule's wire, at hop 0.
    let to_beta = name("peer.beta.agent.parent.inbox");
    alpha.mesh.lookup(&to_beta).await.unwrap();
    let request = WireRequest { from: parent_id(), to: to_beta.clone(), mode: WireMode::Send, asked_by: parent_id() };
    let by = alpha.mesh.rule_for(&request).await.expect("the rule covers alpha and beta");
    alpha.mesh.grant(request, by).await.unwrap();
    alpha.mesh.set_turn_hop(parent_id(), Arc::new(TurnHop::default()));
    alpha.mesh.deliver(&parent_id(), to_beta.as_str(), WireMode::Send, serde_json::json!({"text": "by rule"})).await.unwrap();
    assert_eq!(check(MeshWake::Rules).map(|b| b.sender.to_string()), Some("peer-alpha".into()), "the rule's wire wakes");
    assert_eq!(check(MeshWake::Off), None, "off is off");
}

/// The kernel events a Marv logs, kept in memory.
#[derive(Default)]
struct Events(Mutex<Vec<marv_kernel::KernelEvent>>);
impl marv_kernel::EventLog for Events {
    fn append(&self, _at_ms: u64, event: marv_kernel::KernelEvent) {
        self.0.lock().unwrap().push(event);
    }
}

/// **The hop stop is never silent** (runtime-kernel live UX; the cap itself unchanged). Under a rule, alpha's
/// `mesh_send` to beta from a turn at hop 1 says nothing more and wakes beta; from a turn at hop 2 it's delivered, its
/// result tells alpha's model that it won't wake beta and that its user must continue, beta's person is told a message
/// from peer-alpha at hop 2 waits for their next turn, and beta's event log has a `wake_skipped` with the reason `hop`
/// and no word of the message. A wire the person granted by hand (to gamma) has no hop stop and no such note.
///
/// Proven: dropped the note in `MeshTool::run` → this test failed (alpha's hop-2 result said nothing); dropped
/// `log_waiting`'s append → failed (no `wake_skipped`).
#[tokio::test]
async fn at_the_hop_stop_the_sender_the_person_and_the_log_are_told() {
    use marv_kernel::{KernelEvent, WakeSkipReason};
    use marv_session::config::MeshWake;
    use marv_session::mesh::{WakeSkip, log_waiting, waiting_lines, wake_check_as};

    let home = TempDir::new("stop-home");
    let acme = home.path().join("Projects/acme");
    let doors = TempDir::new("stop");
    let dir = doors.path().join("mesh");
    let alpha = marv("alpha", &dir, &acme.join("api"), Decision::Yes);
    let beta = marv("beta", &dir, &acme.join("web"), Decision::No);
    let gamma = marv("gamma", &dir, &home.path().join("Other/site"), Decision::Yes);
    acme_rule(home.path(), None, &[&alpha, &beta, &gamma]).await;
    let events = Arc::new(Events::default());
    beta.mesh.set_events(events.clone());
    let plug = alpha.mesh.plug(parent_id()).unwrap();
    let hop = Arc::new(TurnHop::default());
    alpha.mesh.set_turn_hop(parent_id(), hop.clone());
    let tools = mesh_tools(&plug, &alpha.mesh, &alpha.person);
    let mut ctx = marv_tools::ToolContext::new(std::env::temp_dir());
    ctx.approver = Some(alpha.person.clone());
    let send = |to: &str, text: &str| mesh_call("mesh_send", serde_json::json!({"receptacle": to, "text": text}));
    let secret = "zq-hop-stop-words";

    hop.set(1);
    let out = marv_tools::run_tool(&send("peer.beta.agent.parent.inbox", "hop one"), &ctx, &tools).await;
    assert!(!out.result.is_error && !out.result.output.contains("won't wake"), "{}", out.result.output);
    assert!(wake_check_as(&beta.mesh, &mut WakeLimit::default(), now_ms(), MeshWake::Rules).wake.is_some());
    beta.mesh.drain_inbox(&parent_id());

    hop.set(2);
    let out = marv_tools::run_tool(&send("peer.beta.agent.parent.inbox", secret), &ctx, &tools).await;
    let said = &out.result.output;
    assert!(!out.result.is_error && said.starts_with("Sent to peer.beta.agent.parent.inbox."), "{said}");
    assert!(said.contains("It won't wake the Marv at peer.beta.agent.parent.inbox: this message is at hop 2"), "{said}");
    assert!(said.contains("tell your user"), "{said}");
    let check = wake_check_as(&beta.mesh, &mut WakeLimit::default(), now_ms(), MeshWake::Rules);
    assert!(check.wake.is_none(), "the cap stands");
    assert_eq!((check.waiting.len(), check.waiting[0].why), (1, WakeSkip::Hop));
    let lines = waiting_lines(&check, MeshWake::Rules);
    assert!(lines[0].starts_with("✉ A message from peer-alpha at hop 2 waits for your next turn"), "{lines:?}");
    log_waiting(&beta.mesh, &check);
    let logged = events.0.lock().unwrap().clone();
    let skipped: Vec<_> = logged.iter().filter(|e| matches!(e, KernelEvent::WakeSkipped { .. })).collect();
    assert_eq!(skipped.len(), 1, "{logged:?}");
    let KernelEvent::WakeSkipped { by, hop: at, reason, count, .. } = skipped[0] else { unreachable!() };
    assert_eq!((by.as_str(), *at, *reason, *count), ("peer-alpha", Some(2), WakeSkipReason::Hop, 1));
    assert!(!serde_json::to_string(&logged).unwrap().contains(secret));
    assert_eq!(beta.mesh.drain_inbox(&parent_id()).len(), 1, "delivered all the same");

    // Gamma, wired by hand (no rule covers ~/Other): no hop stop, no note.
    let to_gamma = ReceptacleName::new("peer.gamma.agent.parent.inbox").unwrap();
    alpha.mesh.lookup(&to_gamma).await.unwrap();
    let request = WireRequest { from: parent_id(), to: to_gamma.clone(), mode: WireMode::Send, asked_by: parent_id() };
    alpha.mesh.grant(request, Approver::Tui).await.unwrap();
    let out = marv_tools::run_tool(&send(to_gamma.as_str(), "hi gamma"), &ctx, &tools).await;
    assert!(!out.result.is_error && !out.result.output.contains("won't wake"), "{}", out.result.output);
    assert_eq!(gamma.mesh.drain_inbox(&parent_id()).len(), 1);
}

/// **A team's wires wake past the hop stop, within a per-chat budget** (runtime-kernel coordination, Demo2: the
/// coordinator's follow-ups were at hop 2 and woke nobody, so the person had to write in every worker chat). Under a
/// rule labelled `Team web` (what Start team writes), alpha's hop-2 send to beta wakes beta and its result carries no
/// hop-stop note; the same send under a rule labelled `Acme` still stops at hop 2. Once beta's team budget is used
/// up, the next team send is delivered but wakes nobody: its result tells alpha's model which chat needs its user,
/// beta's person is told "✉ ... waits for your next turn" with the budget as the reason, and beta's event log has a
/// `wake_skipped` with the reason `budget`. The person typing in beta starts the budget over.
///
/// Proven: dropped the `candidate.team` arm in `decide_wake_as` → the hop-2 team send waited (no wake); made
/// `hop_stopped` ignore `team_send` → the team send's result carried the hop note; dropped
/// `mesh.set_team_pause` in `wake_check_as` → the budget note was missing from the sender's result.
#[tokio::test]
async fn a_teams_wires_wake_past_the_hop_stop_within_a_budget_and_a_dropped_wake_says_who_acts() {
    use marv_kernel::{KernelEvent, WakeSkipReason};
    use marv_session::config::MeshWake;
    use marv_session::mesh::{WakeSkip, log_waiting, waiting_lines, wake_check_as};

    for (label, team) in [("Team web", true), ("Acme", false)] {
        let home = TempDir::new("team-home");
        let acme = home.path().join("Projects/acme");
        let doors = TempDir::new("team");
        let dir = doors.path().join("mesh");
        let alpha = marv("alpha", &dir, &acme.join("api"), Decision::Yes);
        let beta = marv("beta", &dir, &acme.join("web"), Decision::No);
        labelled_acme_rule(home.path(), None, label, &[&alpha, &beta]).await;
        let events = Arc::new(Events::default());
        beta.mesh.set_events(events.clone());
        let plug = alpha.mesh.plug(parent_id()).unwrap();
        let hop = Arc::new(TurnHop::default());
        alpha.mesh.set_turn_hop(parent_id(), hop.clone());
        let tools = mesh_tools(&plug, &alpha.mesh, &alpha.person);
        let mut ctx = marv_tools::ToolContext::new(std::env::temp_dir());
        ctx.approver = Some(alpha.person.clone());
        let send =
            |text: &str| mesh_call("mesh_send", serde_json::json!({"receptacle": "peer.beta.agent.parent.inbox", "text": text}));
        let mut limit = WakeLimit::default().with_team_budget(1);
        hop.set(2);
        let out = marv_tools::run_tool(&send("follow-up"), &ctx, &tools).await;
        let said = out.result.output.clone();
        let check = wake_check_as(&beta.mesh, &mut limit, now_ms(), MeshWake::Rules);
        if !team {
            assert!(said.contains("It won't wake the Marv at") && said.contains("hop 2"), "{said}");
            assert!(check.wake.is_none());
            continue;
        }
        assert!(!said.contains("won't wake"), "{said}");
        assert!(check.wake.is_some(), "a team's hop-2 mail wakes");
        beta.mesh.drain_inbox(&parent_id());

        // The budget (one) is spent: delivered, no wake, and everyone who must act is told.
        let out = marv_tools::run_tool(&send("one more"), &ctx, &tools).await;
        let check = wake_check_as(&beta.mesh, &mut limit, now_ms(), MeshWake::Rules);
        assert!(check.wake.is_none());
        let said = out.result.output;
        assert!(said.contains("It won't wake the Marv at peer.beta.agent.parent.inbox"), "{said}");
        assert!(said.contains("team's wake budget") && said.contains("tell your user which chat"), "{said}");
        assert_eq!((check.waiting.len(), check.waiting[0].why), (1, WakeSkip::TeamBudget));
        let lines = waiting_lines(&check, MeshWake::Rules);
        assert!(lines[0].starts_with("✉ A message from peer-alpha waits for your next turn"), "{lines:?}");
        assert!(lines[0].contains("until you write in this chat"), "{lines:?}");
        log_waiting(&beta.mesh, &check);
        let logged = events.0.lock().unwrap().clone();
        assert!(
            logged.iter().any(|e| matches!(e, KernelEvent::WakeSkipped { reason: WakeSkipReason::Budget, .. })),
            "{logged:?}"
        );
        assert_eq!(beta.mesh.drain_inbox(&parent_id()).len(), 1, "delivered all the same");
        limit.person_typed();
        wake_check_as(&beta.mesh, &mut limit, now_ms(), MeshWake::Rules); // beta's next look republishes its door
        let out = marv_tools::run_tool(&send("after the person wrote"), &ctx, &tools).await;
        assert!(!out.result.output.contains("won't wake"), "{}", out.result.output);
        assert!(wake_check_as(&beta.mesh, &mut limit, now_ms(), MeshWake::Rules).wake.is_some());
    }
}

/// **`mesh_list` says which Marvs a rule covers, and saying so grants nothing** (runtime-kernel live UX). An agent
/// read "the first send asks the user to wire it" under a rule covering the folder and wrongly took it to need
/// approval. Under a rule over `~/Projects/acme`, alpha's `mesh_list` lists beta's inbox as covered by the rule (sends
/// there don't ask) and gamma's (outside the folder) as before; the listing wires nothing.
///
/// Proven: made `Switchboard::rule_covers` match the rule against alpha's own identity instead of the peer's (so any
/// peer is covered) → this test failed (gamma listed as covered).
#[tokio::test]
async fn mesh_list_says_which_marvs_a_rule_covers_and_that_grants_nothing() {
    let home = TempDir::new("covered-home");
    let acme = home.path().join("Projects/acme");
    let doors = TempDir::new("covered");
    let dir = doors.path().join("mesh");
    let alpha = marv("alpha", &dir, &acme.join("api"), Decision::No);
    let beta = marv("beta", &dir, &acme.join("web"), Decision::No);
    let gamma = marv("gamma", &dir, &home.path().join("Other/site"), Decision::No);
    let plug = alpha.mesh.plug(parent_id()).unwrap();
    let tools = mesh_tools(&plug, &alpha.mesh, &alpha.person);
    let ctx = marv_tools::ToolContext::new(std::env::temp_dir());
    let list =
        || async { marv_tools::run_tool(&mesh_call("mesh_list", serde_json::json!({})), &ctx, &tools).await.result.output };

    let before = list().await;
    assert!(!before.contains("covered by") && !before.contains("unless a rule"), "no rules yet:\n{before}");
    let store = acme_rule(home.path(), None, &[&alpha, &beta, &gamma]).await;
    let rule = store.load().unwrap().rules[0].id;
    let text = list().await;
    assert!(
        text.contains(&format!("\n- beta: peer.beta.agent.parent.inbox (covered by {rule}: messages send without asking)")),
        "{text}"
    );
    assert!(text.contains("\n- gamma: peer.gamma.agent.parent.inbox"), "{text}");
    assert!(!text.contains("gamma.agent.parent.inbox (covered"), "{text}");
    assert!(text.contains("unless a rule the user made covers it"), "{text}");
    let to_beta = ReceptacleName::new("peer.beta.agent.parent.inbox").unwrap();
    assert!(alpha.mesh.authorize(&parent_id(), &to_beta, WireMode::Send).is_err(), "listing wired nothing");
    assert!(alpha.person.requests().is_empty() && beta.person.requests().is_empty());
}

/// **A person's turn can't keep two Marvs going forever** (runtime-kernel live UX; review P2). A turn the person
/// started stays at hop 0 however many replies it reads mid-turn (`mesh_wait`), so the hop cap never stops it; it
/// counts rounds instead. Under a rule, alpha's person's turn reads beta's reply mid-turn `MAX_LEAD_ROUNDS` times
/// (each a round); its next `mesh_send` to beta arrives, says beta won't be woken until alpha's user continues, and
/// doesn't wake beta (the waiting line and a `wake_skipped` with the reason `rounds`). Before the cap a send wakes
/// beta, and the person's next message starts the count again.
///
/// Proven: dropped the `rounds_out` check in `decide_wake_as` → this test failed (beta woke past the cap); dropped
/// the `count_round` in `inbox_note_turn` → failed (no round counted, no note).
#[tokio::test]
async fn a_persons_turn_runs_out_of_rounds_and_its_sends_stop_waking() {
    use marv_kernel::{KernelEvent, WakeSkipReason};
    use marv_session::config::MeshWake;
    use marv_session::mesh::{
        MAX_LEAD_ROUNDS, RadioRead, TurnInbox, WakeSkip, inbox_note_turn, log_waiting, waiting_lines, wake_check_as,
    };

    let home = TempDir::new("rounds-home");
    let acme = home.path().join("Projects/acme");
    let doors = TempDir::new("rounds");
    let dir = doors.path().join("mesh");
    let alpha = marv("alpha", &dir, &acme.join("api"), Decision::No);
    let beta = marv("beta", &dir, &acme.join("web"), Decision::No);
    acme_rule(home.path(), None, &[&alpha, &beta]).await;
    let events = Arc::new(Events::default());
    beta.mesh.set_events(events.clone());
    let alpha_plug = alpha.mesh.plug(parent_id()).unwrap();
    let beta_plug = beta.mesh.plug(parent_id()).unwrap();
    let (alpha_turn, beta_turn) = (Arc::new(TurnHop::default()), Arc::new(TurnHop::default()));
    alpha.mesh.set_turn_hop(parent_id(), alpha_turn.clone());
    beta.mesh.set_turn_hop(parent_id(), beta_turn.clone());
    let alpha_tools = mesh_tools(&alpha_plug, &alpha.mesh, &alpha.person);
    let beta_tools = mesh_tools(&beta_plug, &beta.mesh, &beta.person);
    let ctx = marv_tools::ToolContext::new(std::env::temp_dir());
    let send = |to: &str, text: &str| mesh_call("mesh_send", serde_json::json!({"receptacle": to, "text": text}));
    let taint = marv_sandbox::TaintState::new();

    // The person's turn on alpha: send, read beta's reply mid-turn, again.
    alpha_turn.new_turn(0);
    for round in 0..MAX_LEAD_ROUNDS {
        let out = marv_tools::run_tool(&send("peer.beta.agent.parent.inbox", "next?"), &ctx, &alpha_tools).await;
        assert!(!out.result.is_error && !out.result.output.contains("won't wake"), "round {round}: {}", out.result.output);
        let check = wake_check_as(&beta.mesh, &mut WakeLimit::default(), now_ms(), MeshWake::Rules);
        assert!(check.wake.is_some(), "round {round}: beta wakes");
        beta.mesh.drain_inbox(&parent_id());
        // Beta's woken turn replies (hop 1); alpha's turn reads it mid-turn: a round.
        beta_turn.new_turn(1);
        let out = marv_tools::run_tool(&send("peer.alpha.agent.parent.inbox", "reply"), &ctx, &beta_tools).await;
        assert!(!out.result.is_error, "{}", out.result.output);
        let turn = TurnInbox { rounds: Some(alpha_turn.clone()), ..TurnInbox::default() };
        let read = inbox_note_turn(&alpha.mesh, &parent_id(), &taint, &RadioRead::Skip, &turn);
        assert!(read.is_some_and(|r| r.contains("reply")));
        // Reading beta's hop-1 reply raises the person's turn to 1 (no task-eligible sends), never past it: its sends
        // still wake beta until the rounds run out.
        assert_eq!(alpha_turn.get(), 1, "the person's turn is at hop 1 after reading the reply, never higher");
    }
    assert!(alpha_turn.rounds_out());

    let out = marv_tools::run_tool(&send("peer.beta.agent.parent.inbox", "zq-rounds-words"), &ctx, &alpha_tools).await;
    let said = &out.result.output;
    assert!(!out.result.is_error && said.contains(&format!("{MAX_LEAD_ROUNDS} rounds this turn")), "{said}");
    assert!(said.contains("won't be woken until your user continues"), "{said}");
    let check = wake_check_as(&beta.mesh, &mut WakeLimit::default(), now_ms(), MeshWake::Rules);
    assert!(check.wake.is_none(), "past the cap: no wake");
    assert_eq!(check.waiting[0].why, WakeSkip::Rounds);
    let lines = waiting_lines(&check, MeshWake::Rules);
    assert!(lines[0].starts_with("✉ A message from peer-alpha waits for your next turn: peer-alpha went 5 rounds"), "{lines:?}");
    log_waiting(&beta.mesh, &check);
    let logged = events.0.lock().unwrap().clone();
    assert!(logged.iter().any(|e| matches!(e, KernelEvent::WakeSkipped { reason: WakeSkipReason::Rounds, .. })), "{logged:?}");
    assert!(!serde_json::to_string(&logged).unwrap().contains("zq-rounds"));
    assert_eq!(beta.mesh.drain_inbox(&parent_id()).len(), 1, "delivered all the same");

    // The person's next message: a new turn, and sends wake again.
    alpha_turn.new_turn(0);
    let out = marv_tools::run_tool(&send("peer.beta.agent.parent.inbox", "again"), &ctx, &alpha_tools).await;
    assert!(!out.result.output.contains("won't wake"), "{}", out.result.output);
    assert!(wake_check_as(&beta.mesh, &mut WakeLimit::default(), now_ms(), MeshWake::Rules).wake.is_some());
}

/// **A turn a rule's message woke reads only rules' wires** (runtime-kernel live UX, review P3). Under the default
/// `mesh.wake` (only rules' wires wake), a message from alpha over a rule's wire wakes beta; a worker's message on a
/// wire beta's person approved by hand, told to wait for the person's next turn, stays unread through the woken
/// (unattended) turn, before its first request and between steps, and the person's own turn then reads it.
///
/// Proven: woke with `rules_only: false` in `MarvSession::wake_as` → this test failed (the woken turn read the
/// worker's message).
#[tokio::test]
async fn a_rule_woken_turn_leaves_hand_wired_mail_for_the_persons_turn() {
    use futures::StreamExt;
    use marv_core::ChatTurn;
    use marv_session::config::MeshWake;
    use marv_session::mesh::wake_check_as;

    let home = TempDir::new("only-home");
    let acme = home.path().join("Projects/acme");
    let doors = TempDir::new("only");
    let dir = doors.path().join("mesh");
    let alpha = marv("alpha", &dir, &acme.join("api"), Decision::No);
    let beta = marv("beta", &dir, &acme.join("web"), Decision::No);
    acme_rule(home.path(), None, &[&alpha, &beta]).await;
    let project = TempDir::new("only-project");
    let model = Scripted::new(vec![say("Noted."), say("Read it.")]);
    let person = Recorder::new(Decision::No);
    let mut init = marv_session::SessionInit::new(
        project.path(),
        marv_session::ProviderChoice::Option(marv_provider::ProviderOption::Custom(model.clone())),
    );
    (init.diagnostics, init.checks, init.checkpoints) = (false.into(), false, false);
    init.mesh = Some(beta.mesh.clone());
    init.approve = Some(person.clone());
    let session = marv_session::MarvSession::new(init).unwrap();

    // A worker on beta, wired by beta's person by hand, writes first; then alpha, over the rule's wire.
    let worker = beta.mesh.plug_next("worker").unwrap();
    let inbox = ReceptacleName::new("agent.parent.inbox").unwrap();
    let request = WireRequest { from: worker.id.clone(), to: inbox.clone(), mode: WireMode::Send, asked_by: worker.id.clone() };
    beta.mesh.grant(request, Approver::Tui).await.unwrap();
    beta.mesh.deliver(&worker.id, inbox.as_str(), WireMode::Send, serde_json::json!({"text": "zq-hand-wired"})).await.unwrap();
    let to_beta = ReceptacleName::new("peer.beta.agent.parent.inbox").unwrap();
    alpha.mesh.lookup(&to_beta).await.unwrap();
    let request = WireRequest { from: parent_id(), to: to_beta.clone(), mode: WireMode::Send, asked_by: parent_id() };
    let by = alpha.mesh.rule_for(&request).await.expect("the rule covers alpha and beta");
    alpha.mesh.grant(request, by).await.unwrap();
    alpha.mesh.set_turn_hop(parent_id(), Arc::new(TurnHop::default()));
    alpha.mesh.deliver(&parent_id(), to_beta.as_str(), WireMode::Send, serde_json::json!({"text": "zq-by-rule"})).await.unwrap();

    let check = wake_check_as(&beta.mesh, &mut WakeLimit::default(), now_ms(), MeshWake::Rules);
    let wake = check.wake.expect("the rule's message wakes beta");
    let mut turn = session.wake_as(&wake, MeshWake::Rules).unwrap().expect("a woken turn");
    while turn.next().await.is_some() {}
    let first: String = model.requests.lock().unwrap()[0]
        .0
        .iter()
        .filter_map(|t| if let ChatTurn::User { text } = t { Some(text.clone()) } else { None })
        .collect();
    assert!(first.contains("zq-by-rule") && !first.contains("zq-hand-wired"), "{first}");
    let left = beta.mesh.unread(&parent_id());
    assert_eq!(left, [(worker.id.clone(), inbox.clone())], "the hand-wired message waits");

    // The person's turn reads it.
    let mut turn = session.send("what else came?", Default::default()).unwrap();
    while turn.next().await.is_some() {}
    let second = format!("{:?}", model.requests.lock().unwrap()[1].0);
    assert!(second.contains("zq-hand-wired"), "{second}");
    assert!(beta.mesh.unread(&parent_id()).is_empty());
    session.close().await;
}

/// Review P3: `mesh_list` never says "covered" where the send would ask: `Switchboard::rule_covers` checks, as
/// `rule_for` does, that the door answered as the Marv it's named for.
/// Proven: dropped the `info.name == session` check from `rule_covers` → this test failed.
#[tokio::test]
async fn rule_coverage_needs_the_door_to_answer_as_its_marv() {
    let home = TempDir::new("named-home");
    let acme = home.path().join("Projects/acme");
    let doors = TempDir::new("named");
    let dir = doors.path().join("mesh");
    let alpha = marv("alpha", &dir, &acme.join("api"), Decision::No);
    let beta = marv("beta", &dir, &acme.join("web"), Decision::No);
    acme_rule(home.path(), None, &[&alpha, &beta]).await;
    let mut peer = marv_mesh::discovery::list(&dir).await.unwrap().into_iter().find(|p| p.session == "beta").unwrap();
    assert!(alpha.mesh.rule_covers(&parent_id(), &peer).is_some(), "the control");
    peer.info.name = "mallory".into();
    assert!(alpha.mesh.rule_covers(&parent_id(), &peer).is_none());
    drop(beta);
}
