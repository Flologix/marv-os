//! Runtime-kernel phase 3 (`docs/architecture/runtime-kernel.md` §5.4, D10): the kernel's event log
//! (`~/.marv/events/<yyyy-mm-dd>.jsonl`) is private, the user's own, and never holds a message's words.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use marv_guards::TempDir;
use marv_mesh::approval::WireRequest;
use marv_mesh::switchboard::{NoTrajectory, Switchboard};
use marv_mesh::wire::{Approver, WireMode};
use marv_mesh::{PlugId, ReceptacleName};
use marv_session::kernel_events::EventWriter;
use tokio_util::sync::CancellationToken;

const MARKER: &str = "zq-SECRET-7f3a91-marker-never-logged";

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Every file under `dir`, read whole.
fn all_text(dir: &Path) -> String {
    let mut text = String::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        text.push_str(&std::fs::read_to_string(entry.path()).unwrap());
    }
    text
}

/// **The event log never holds a message's text** (runtime-kernel 3; docs/SECURITY.md, "The kernel's event log").
/// A message carrying a unique marker is sent over a wire (delivered), from a plug with no wire (refused, `not_wired`),
/// asked for a wire the person declines (a `wire_refused`, runtime-kernel live UX), waits with wake off (a
/// `wake_skipped`), and wakes its reader: the log has a `message` line for each, the `wire_refused`, the
/// `wake_skipped` and the `wake`, with sizes, and the marker appears nowhere in it.
///
/// Proven: put the body into the logged message's id (`Switchboard::log_event`) → this test failed (the marker was in
/// the log); dropped the decline's `log_wire_refused` in `MeshTool::approve_itself` → failed; logged no refusal
/// category → failed.
#[tokio::test]
async fn the_event_log_never_holds_a_messages_text() {
    let base = TempDir::new("ev-text");
    let config = base.path().join("home/.marv");
    let log = EventWriter::new(&config).unwrap();
    let mesh = Arc::new(Switchboard::new("alpha", base.path().join("plugs"), Arc::new(NoTrajectory)));
    let cancel = CancellationToken::new();
    tokio::spawn(Arc::clone(&mesh).serve(cancel.clone()));
    mesh.set_events(log.clone());
    let parent = PlugId::new("parent").unwrap();
    mesh.plug(parent.clone()).unwrap();
    let worker = mesh.plug_next("worker").unwrap().id;
    let stranger_plug = mesh.plug_next("worker").unwrap();
    let stranger = stranger_plug.id.clone();
    let inbox = ReceptacleName::new("agent.parent.inbox").unwrap();
    let request = WireRequest { from: worker.clone(), to: inbox.clone(), mode: WireMode::Send, asked_by: worker.clone() };
    mesh.grant(request, Approver::Tui).await.unwrap();

    let body = serde_json::json!({ "text": format!("please {MARKER} now") });
    mesh.deliver(&worker, inbox.as_str(), WireMode::Send, body.clone()).await.unwrap();
    assert!(mesh.deliver(&stranger, inbox.as_str(), WireMode::Send, body).await.is_err(), "unwired: refused");
    // Runtime-kernel live UX: the stranger's mesh_send asks for the wire, its person says no (a `wire_refused`), and
    // the send goes on to be refused (`not_wired`).
    let person = marv_guards::Recorder::new(marv_tools::Decision::No);
    let wires = marv_session::mesh::wire_approver(None, Some(&(person.clone() as Arc<dyn marv_tools::Approver>)));
    let send = marv_session::mesh::MeshTool::all(&stranger_plug, &mesh, wires)
        .into_iter()
        .map(|t| Arc::new(t) as marv_tools::ToolRef)
        .collect::<Vec<_>>();
    let mut ctx = marv_tools::ToolContext::new(base.path());
    ctx.approver = Some(person.clone());
    let call = marv_core::ToolCall {
        id: "c1".into(),
        name: "mesh_send".into(),
        arguments: serde_json::json!({"receptacle": "agent.parent.inbox", "text": format!("also {MARKER}")}).to_string(),
    };
    let out = marv_tools::run_tool(&call, &ctx, &send).await;
    assert!(out.result.declined || out.result.is_error, "{}", out.result.output);
    // Runtime-kernel live UX: mail that doesn't wake is logged as a skipped wake, with its reason word only.
    let check = marv_session::mesh::wake_check_as(&mesh, &mut Default::default(), 0, marv_session::config::MeshWake::Off);
    marv_session::mesh::log_waiting(&mesh, &check);
    let by = marv_session::mesh::WakeBy { sender: worker.clone(), receptacle: inbox };
    mesh.log_wake(&by.sender, marv_session::mesh::wake_hop(&mesh, &by));
    let read = mesh.drain_inbox(&parent);
    assert!(read[0].fenced().contains(MARKER), "the message itself carried it");
    log.flush().await;
    cancel.cancel();

    let text = all_text(&config.join("events"));
    let (events, skipped) = marv_kernel::parse_log(&text);
    assert_eq!(skipped, 0);
    let outcomes: Vec<marv_kernel::Delivery> = events
        .iter()
        .filter_map(|(_, e)| match e {
            marv_kernel::KernelEvent::Message { outcome, size, .. } if *size > MARKER.len() => Some(*outcome),
            _ => None,
        })
        .collect();
    assert_eq!(outcomes, [marv_kernel::Delivery::Delivered, marv_kernel::Delivery::Refused], "{text}");
    assert!(
        events
            .iter()
            .any(|(_, e)| matches!(e, marv_kernel::KernelEvent::Message { refusal: Some(marv_kernel::Refusal::NotWired), .. })),
        "{text}"
    );
    assert!(
        events.iter().any(|(_, e)| matches!(
            e,
            marv_kernel::KernelEvent::WireRefused { reason: marv_kernel::WireRefusal::Declined, from, .. } if *from == stranger.as_str()
        )),
        "{text}"
    );
    assert!(events.iter().any(|(_, e)| matches!(e, marv_kernel::KernelEvent::Wake { .. })), "{text}");
    assert!(
        events.iter().any(|(_, e)| matches!(
            e,
            marv_kernel::KernelEvent::WakeSkipped { reason: marv_kernel::WakeSkipReason::Off, count: 1, .. }
        )),
        "{text}"
    );
    assert!(!text.contains(MARKER), "the marker is in the event log:\n{text}");
    assert!(!text.contains("please"), "{text}");
}

/// **The event log is private and in the user's own config folder** (runtime-kernel 3, D10). Written from a session
/// whose project is elsewhere, its file is `~/.marv/events/<yyyy-mm-dd>.jsonl`, 0600, in a 0700 folder (a folder or
/// file made wider before is made private again), and nothing is written under the project.
///
/// Proven: dropped the folder's `set_permissions(0o700)` in `kernel_events::open_log` → this test failed (the 0755
/// folder stayed 0755).
#[cfg(unix)]
#[tokio::test]
async fn the_event_log_is_0600_in_a_0700_folder_in_the_users_config_never_a_project() {
    let base = TempDir::new("ev-perm");
    let (home, project) = (base.path().join("home"), base.path().join("home/Projects/acme"));
    std::fs::create_dir_all(project.join(".marv")).unwrap();
    let config = marv_session::config::default_config_dir(&marv_mcp::Env::new(), &home);
    assert_eq!(config, home.join(".marv"));
    std::fs::create_dir_all(config.join("events")).unwrap();
    std::fs::set_permissions(config.join("events"), std::fs::Permissions::from_mode(0o755)).unwrap();

    let log = EventWriter::new(&config).unwrap();
    let now = marv_session::time::now_ms();
    marv_kernel::EventLog::append(
        log.as_ref(),
        now,
        marv_kernel::KernelEvent::Unregistered { agent: marv_kernel::AgentKey::Mesh("acme-3f9a".into()) },
    );
    log.flush().await;
    let file = log.path_at(now);
    assert_eq!(file.parent().unwrap(), home.join(".marv/events"));
    assert!(file.is_file());
    assert_eq!(mode(&file), 0o600);
    assert_eq!(mode(&home.join(".marv/events")), 0o700);
    assert!(!file.starts_with(&project));
    assert!(!project.join(".marv/events").exists(), "nothing under the project");
}

/// **A project's `.marv` can't redirect the event log** (runtime-kernel 3). The log's folder is the user's config
/// folder, absolute, from the user's environment and home, never the project: a project's `.marv/events` (here a
/// symlink to elsewhere) and a `.marv/config.json` naming another place are never used, a relative `MARV_CONFIG_DIR`
/// (which would resolve inside the project) is refused outright, and the user's own `events` folder being a symlink
/// isn't written through either.
///
/// Proven: let `EventWriter::with_queue` accept a relative config folder → this test failed.
#[cfg(unix)]
#[tokio::test]
async fn a_projects_marv_folder_cant_redirect_the_event_log() {
    let base = TempDir::new("ev-redirect");
    let (home, project) = (base.path().join("home"), base.path().join("home/Projects/acme"));
    let elsewhere = base.path().join("elsewhere");
    std::fs::create_dir_all(project.join(".marv")).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, project.join(".marv/events")).unwrap();
    std::fs::write(project.join(".marv/config.json"), format!(r#"{{"events": "{}"}}"#, elsewhere.display())).unwrap();

    let relative: marv_mcp::Env = [("MARV_CONFIG_DIR".to_string(), ".marv".to_string())].into_iter().collect();
    assert!(EventWriter::new(&marv_session::config::default_config_dir(&relative, &home)).is_err(), "relative: refused");

    // The user's own folder, its events folder a symlink to elsewhere: nothing is written through it.
    let config = marv_session::config::default_config_dir(&marv_mcp::Env::new(), &home);
    std::fs::create_dir_all(&config).unwrap();
    std::os::unix::fs::symlink(&elsewhere, config.join("events")).unwrap();
    let log = EventWriter::new(&config).unwrap();
    marv_kernel::EventLog::append(
        log.as_ref(),
        marv_session::time::now_ms(),
        marv_kernel::KernelEvent::Unregistered { agent: marv_kernel::AgentKey::Mesh("acme-3f9a".into()) },
    );
    log.flush().await;
    assert!(std::fs::read_dir(&elsewhere).unwrap().next().is_none(), "nothing written where the project pointed");
    assert!(log.path_at(0).starts_with(home.join(".marv/events")));
}

/// Review P3: a waiting message is logged as a skipped wake once for the switchboard's life. A session reloaded on it
/// (new wake limits, as a desktop chat waking from sleep has) tells the person again but logs nothing new, so
/// `marv kernel replay` doesn't count it twice; a message that comes after is logged.
/// Proven: dropped the `first_skip_logs` filter in `log_waiting` → this test failed (two `wake_skipped` for one message).
#[tokio::test]
async fn a_waiting_message_is_logged_once_across_a_reload() {
    use marv_session::config::MeshWake;
    use marv_session::mesh::{log_waiting, wake_check_as};
    let base = TempDir::new("ev-once");
    let config = base.path().join("home/.marv");
    let log = EventWriter::new(&config).unwrap();
    let mesh = Arc::new(Switchboard::new("alpha", base.path().join("plugs"), Arc::new(NoTrajectory)));
    let cancel = CancellationToken::new();
    tokio::spawn(Arc::clone(&mesh).serve(cancel.clone()));
    mesh.set_events(log.clone());
    mesh.plug(PlugId::new("parent").unwrap()).unwrap();
    let worker = mesh.plug_next("worker").unwrap().id;
    let inbox = ReceptacleName::new("agent.parent.inbox").unwrap();
    let request = WireRequest { from: worker.clone(), to: inbox.clone(), mode: WireMode::Send, asked_by: worker.clone() };
    mesh.grant(request, Approver::Tui).await.unwrap();
    mesh.deliver(&worker, inbox.as_str(), WireMode::Send, serde_json::json!({"text": "one"})).await.unwrap();
    for _reload in 0..2 {
        let check = wake_check_as(&mesh, &mut Default::default(), 0, MeshWake::Off);
        assert_eq!(check.waiting.len(), 1, "the person is told again");
        log_waiting(&mesh, &check);
    }
    mesh.deliver(&worker, inbox.as_str(), WireMode::Send, serde_json::json!({"text": "two"})).await.unwrap();
    let check = wake_check_as(&mesh, &mut Default::default(), 0, MeshWake::Off);
    log_waiting(&mesh, &check);
    log.flush().await;
    cancel.cancel();
    let (events, _) = marv_kernel::parse_log(&all_text(&config.join("events")));
    let counts: Vec<usize> = events
        .iter()
        .filter_map(|(_, e)| match e {
            marv_kernel::KernelEvent::WakeSkipped { count, .. } => Some(*count),
            _ => None,
        })
        .collect();
    assert_eq!(counts, [1, 1], "the first message once, then the second");
}
