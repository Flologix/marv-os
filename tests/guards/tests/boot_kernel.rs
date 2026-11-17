//! Runtime-kernel boot and devices (`docs/architecture/runtime-kernel.md` §17–18, kernel #33): a chat's boot is logged
//! as hashes and names, never words (B1), and a device switched off for a chat isn't part of its boot (D1).

use std::sync::{Arc, Mutex};

use marv_guards::{Scripted, TempDir, say};
use marv_kernel::KernelEvent;
use marv_mesh::switchboard::{NoTrajectory, Switchboard};
use serde_json::json;

#[derive(Default)]
struct Events(Mutex<Vec<KernelEvent>>);
impl marv_kernel::EventLog for Events {
    fn append(&self, _at_ms: u64, event: KernelEvent) {
        self.0.lock().unwrap().push(event);
    }
}
impl Events {
    fn boots(&self) -> Vec<KernelEvent> {
        self.0.lock().unwrap().iter().filter(|e| matches!(e, KernelEvent::Booted { .. })).cloned().collect()
    }
}

async fn run(turn: marv_session::Turn) {
    let mut turn = turn;
    while futures::StreamExt::next(&mut turn).await.is_some() {}
}

/// **A boot is logged once, as hashes and names, never words (B1).** A chat's first request logs `Booted` with the
/// SHA-256 of the system prompt it sent and of its tool specs in order; a second turn of the same conversation logs
/// nothing; `/clear` boots again, with the same hashes (the same cacheable prefix). The logged line holds none of
/// the person's message, the prompt's text or a spec's words.
///
/// Proven: logged at every turn (dropped `boot_pending.take()`'s early return) → failed (two boots for one
/// conversation); passed the system prompt itself as `prompt_hash` → failed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_boot_is_logged_once_as_hashes_and_names_never_words() {
    let (project, plugs) = (TempDir::new("booted"), TempDir::new("booted-plugs"));
    let sb = Arc::new(Switchboard::new("alpha", plugs.path().to_path_buf(), Arc::new(NoTrajectory)));
    sb.plug(marv_session::mesh::parent_id()).unwrap();
    let events = Arc::new(Events::default());
    sb.set_events(events.clone());
    let model = Scripted::new(vec![say("one"), say("two"), say("three")]);
    let mut init = marv_session::SessionInit::new(
        project.path(),
        marv_session::ProviderChoice::Option(marv_provider::ProviderOption::Custom(model.clone())),
    );
    init.diagnostics = false.into();
    init.checks = false;
    init.mesh = Some(sb.clone());
    let session = marv_session::MarvSession::new(init).unwrap();
    run(session.send("SECRET PERSON WORDS about the launch", Default::default()).unwrap()).await;
    run(session.send("SECRET follow-up", Default::default()).unwrap()).await;
    let boots = events.boots();
    assert_eq!(boots.len(), 1, "{boots:?}");
    let requests = model.requests.lock().unwrap().clone();
    let (system, tools) = (requests[0].1.system.clone().unwrap_or_default(), requests[0].1.tools.clone());
    let KernelEvent::Booted { prompt_hash, tools_hash, resumed, devices, .. } = &boots[0] else { unreachable!() };
    assert_eq!(prompt_hash, &marv_session::boot::prompt_hash(&system));
    assert_eq!(tools_hash, &marv_session::boot::tools_hash(&tools));
    assert!(!resumed);
    assert!(devices.contains(&"web".to_string()), "{devices:?}");
    assert_eq!(&session.card().devices, devices, "the card carries the device set");
    let line = marv_kernel::LoggedEvent::new(1, boots[0].clone()).line();
    assert!(!line.contains("SECRET"), "{line}");
    assert!(!line.contains(&system[..system.len().min(40)]), "the prompt's text: {line}");
    assert!(!tools.iter().any(|t| line.contains(&t.description)), "a spec's words: {line}");

    session.clear().await.unwrap();
    run(session.send("SECRET again", Default::default()).unwrap()).await;
    let boots = events.boots();
    assert_eq!(boots.len(), 2, "{boots:?}");
    let KernelEvent::Booted { prompt_hash: again, tools_hash: tools_again, .. } = &boots[1] else { unreachable!() };
    assert_eq!((again, tools_again), (prompt_hash, tools_hash), "the same prefix after /clear");
    session.close().await;
}

/// **A device switched off for a chat isn't offered (D1).** `devices_off` (Marv desktop's per-chat switch) leaves
/// each door out of the chat (github, process and herdr not in its tools, host commands not in its prompt) and its
/// MCP server never started (its process doesn't run); the same session without the switch has them all.
///
/// Proven: made `sdk.rs` ignore `devices_off` for `github` → failed; for `mcp:` → failed; for
/// `process`, `host` and `herdr`, one at a time → failed.
#[tokio::test]
async fn a_device_switched_off_for_a_chat_isnt_offered() {
    // A stdio MCP server in sh with one tool, which notes that it ran in `$1`.
    const SERVER: &str = r#"printf 'ran\n' >> "$1"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([^,}]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"dev","version":"1"}}}\n' "$id" ;;
    *'"method":"tools/list"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"look","inputSchema":{"type":"object"}}]}}\n' "$id" ;;
    *'"id":'*) printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
  esac
done
"#;
    let offered = |off: Vec<String>| async move {
        let (project, config) = (TempDir::new("device-off"), TempDir::new("device-off-config"));
        let script = config.write("server.sh", SERVER);
        let ran = config.path().join("ran.log");
        let model = Scripted::new(vec![say("ok")]);
        marv_guards::sdk_turn(project.path(), config.path(), model.clone(), |o| {
            o.mcp_servers = vec![("dev".into(), json!({"command": "sh", "args": [script, ran.clone()]}))];
            (o.github, o.github_known) = (true, Some(true));
            (o.processes, o.host_commands) = (true, true);
            o.herdr = Some(marv_session::herdr_door::HerdrSetup {
                program: "/bin/true".into(),
                own_pane: None,
                marv: "/bin/true".into(),
                max_peers: 1,
                runs: true,
                meter_chat: None,
            });
            o.devices_off = off;
        })
        .await;
        let requests = model.requests.lock().unwrap().clone();
        let tools = requests[0].1.tools.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
        let host = requests[0].1.system.as_deref().unwrap_or_default().contains("bash with sandbox: false runs one command");
        (tools, host, ran.exists())
    };
    let (all, host, ran) = offered(vec![]).await;
    for name in ["github", "process", "herdr", "mcp__dev__look"] {
        assert!(all.contains(&name.to_string()), "{name} not offered: {all:?}");
    }
    assert!(host && ran, "host commands {host}, server ran {ran}");
    let off = ["github", "process", "herdr", "host", "mcp:dev"].map(String::from).to_vec();
    let (some, host, ran) = offered(off).await;
    for name in ["github", "process", "herdr"] {
        assert!(!some.contains(&name.to_string()), "{name} offered: {some:?}");
    }
    assert!(!some.iter().any(|t| t.starts_with("mcp__dev")), "{some:?}");
    assert!(!host, "host commands in the prompt");
    assert!(!ran, "the switched-off server's process ran");
    assert!(some.contains(&"read_file".to_string()), "{some:?}");
}
