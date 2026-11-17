//! `docs/SECURITY.md`, "MCP" (devices): what an MCP server may ask of Marv (nothing), what it may change once Marv has
//! started (nothing: its tool list is read once), and who installs one (the person, never a tool).

use std::sync::Arc;
use std::time::Duration;

use marv_guards::TempDir;
use marv_mcp::config::{ServerSource, parse_server};
use marv_mcp::manager::{ManagerOptions, McpManager};
use serde_json::{Value, json};

/// A stdio MCP server in POSIX sh: logs each line it's sent to `$1`, answers `initialize` (claiming `listChanged`) and
/// `tools/list` (the first time one tool, `first`; after that two), and right after its first `tools/list` answer
/// says its tool list changed.
const SERVER: &str = r#"log="$1"; lists=0
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$log"
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([^,}]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{"listChanged":true}},"serverInfo":{"name":"dev","version":"1"}}}\n' "$id" ;;
    *'"method":"tools/list"'*)
      lists=$((lists + 1))
      if [ "$lists" = 1 ]; then tools='[{"name":"first","inputSchema":{"type":"object"}}]'
      else tools='[{"name":"first","inputSchema":{"type":"object"}},{"name":"second","inputSchema":{"type":"object"}}]'; fi
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":%s}}\n' "$id" "$tools"
      printf '{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}\n' ;;
    *'"id":'*) printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
  esac
done
"#;

fn manager(dir: &TempDir) -> Arc<McpManager> {
    let script = dir.write("server.sh", SERVER);
    let log = dir.path().join("sent.log");
    let raw = json!({"command": "sh", "args": [script, log]});
    let server = parse_server("dev", &raw, ServerSource::Personal, &|_| None, "mcp.json").unwrap();
    McpManager::new(
        vec![server],
        ManagerOptions {
            root: dir.path().to_path_buf(),
            project_key: "p".into(),
            home: dir.path().to_path_buf(),
            version: "0".into(),
            trust: None,
            logins: None,
            env: [("PATH".to_string(), std::env::var("PATH").unwrap_or_default())].into(),
            connect_timeout: Duration::from_secs(10),
            login_timeout: Duration::from_secs(10),
        },
    )
}

fn sent(dir: &TempDir) -> Vec<Value> {
    dir.read("sent.log").unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// **Marv's MCP client advertises no capabilities**: no `sampling` (a server may not spend the person's model or
/// money), no `roots` beyond the project path Marv passes, no `elicitation` (a server may not ask the model, or the
/// person, anything through Marv). Its `initialize` says `"capabilities": {}`.
///
/// Proven: made `Client::initialize` send `json!({"sampling": {}})` → failed.
#[tokio::test]
async fn the_mcp_client_advertises_no_capabilities() {
    let dir = TempDir::new("devices-caps");
    let mcp = manager(&dir);
    mcp.start().await;
    mcp.ready().await;
    let init = sent(&dir).into_iter().find(|m| m["method"] == "initialize").expect("an initialize was sent");
    assert_eq!(init["params"]["capabilities"], json!({}), "{init}");
    mcp.close().await;
}

/// **A server's `tools/list_changed` never changes a running tool list**: the tools are read once at start (the tool
/// specs are part of the cached prefix, and a server that adds a tool mid-conversation would be adding words to it).
/// The change takes effect when Marv next starts.
///
/// Proven: made the client's reader call `list_tools` again on `notifications/tools/list_changed` and the manager take
/// the new list → failed.
#[tokio::test]
async fn list_changed_never_changes_a_running_tool_list() {
    let dir = TempDir::new("devices-changed");
    let mcp = manager(&dir);
    mcp.start().await;
    mcp.ready().await;
    let names = || mcp.tools().iter().map(|t| t.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(), ["mcp__dev__first"], "{:?}", mcp.status());
    let specs = mcp.specs();
    // The server said its list changed right after answering: give a reader that would act on it the time to.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(names(), ["mcp__dev__first"]);
    assert_eq!(mcp.specs(), specs);
    let lists = sent(&dir).iter().filter(|m| m["method"] == "tools/list").count();
    assert_eq!(lists, 1, "the tool list is read once");
    mcp.close().await;
}

/// **No tool installs a driver**: writing an MCP server into the person's `mcp.json` (`add_personal_server`), taking
/// one out, or trusting a project's servers (`McpTrust::trust`) is the person's act, in `/mcp add`, `/mcp remove`,
/// the startup trust question and the desktop's gallery, never a model's tool. Outside the shells (the TUI and the
/// desktop) and the functions themselves, no code calls them but the manager's `trust_all`, which only the shells call
/// (`/mcp trust`, the person's answer).
///
/// Proven: added an `add_personal_server` call to `marv-session/src/subagent.rs` → failed; added an `mcp.add(` call
/// there → failed.
#[test]
fn no_tool_installs_a_driver() {
    // `McpManager::add` too: it starts a server in the running Marv (the TUI's `/mcp add`, after the person's yes).
    let writers = ["add_personal_server", "remove_personal_server", ".trust(", "trust_all(", "mcp.add(", "McpManager::add"];
    let callers = marv_guards::callers_outside_the_shells(&writers, &["marv-mcp/src/personal_file.rs", "marv-mcp/src/trust.rs"]);
    // `trust_all` itself, defined in the manager, and its one call of `McpTrust::trust`.
    // Release merge: #220's `ProjectTrust::trust` (a project's instruction files, not its servers), called once by
    // `project_trust::trust_project`, which only the shells call (the TUI's and the desktop's `/trust`, the person's
    // command). It installs no server; listed so a new caller still shows up here.
    assert_eq!(
        callers,
        ["marv-mcp/src/manager.rs: .trust(", "marv-mcp/src/manager.rs: trust_all(", "marv-session/src/project_trust.rs: .trust("],
        "{callers:?}"
    );
}

/// **`/mcp add` never changes a running conversation's tool list**: a server added mid-conversation (what `/mcp add`
/// does to the session's manager) is offered from the next conversation (`/clear`), while the running one's tool specs
/// stay byte for byte what its first request sent (the prompt cache: the tool list is fixed for a conversation). With the
/// tools deferred, `tool_search` can't find the new server's tool and `mcp_call` can't call it either, until then.
///
/// Proven: made `MarvSession::offered` read the manager's live specs and tools instead of the conversation's
/// (`mcp_frozen`) → failed (the second request listed `mcp__more__first`); gave `tool_search` and `mcp_call`
/// `frozen: None` (the live manager) → failed (the search found it).
#[tokio::test]
async fn mcp_add_never_changes_a_running_conversations_tool_list() {
    use marv_guards::{say, tool_call, use_tools};
    use marv_mcp::deferred::McpToolsSetting;
    for mode in [McpToolsSetting::Eager, McpToolsSetting::Deferred] {
        let (project, config, servers) = (TempDir::new("add-project"), TempDir::new("add-config"), TempDir::new("add-servers"));
        let script = servers.write("server.sh", SERVER);
        let entry = |log: &str| json!({"command": "sh", "args": [script, servers.path().join(log)]});
        let deferred = mode == McpToolsSetting::Deferred;
        let reach = |id: &str| {
            use_tools(vec![
                tool_call(&format!("s{id}"), "tool_search", json!({"names": ["mcp__more__first"]})),
                tool_call(&format!("c{id}"), "mcp_call", json!({"tool": "mcp__more__first", "arguments": {}})),
            ])
        };
        let script_replies = if deferred {
            vec![say("one"), reach("2"), say("two"), reach("3"), say("three")]
        } else {
            vec![say("one"), say("two"), say("three")]
        };
        let model = marv_guards::Scripted::new(script_replies);
        let mut builder = marv_session::SessionBuilder::new(project.path(), marv_provider::ProviderOption::Custom(model.clone()));
        builder.options.config_dir = Some(config.path().to_path_buf());
        builder.options.diagnostics = false.into();
        builder.options.checks = false;
        builder.options.mcp_tools = mode;
        builder.options.mcp_servers = vec![("dev".into(), entry("dev.log"))];
        let session = builder.build().await.expect("a session");
        let run = |text: &'static str| {
            let session = &session;
            async move {
                let mut turn = session.send(text, Default::default()).expect("a turn");
                let mut results = Vec::new();
                while let Some(event) = futures::StreamExt::next(&mut turn).await {
                    if let marv_session::session::SessionEvent::Loop(marv_agent::LoopEvent::ToolEnd { run, .. }) = event {
                        results.push(run.result.output.clone());
                    }
                }
                results
            }
        };
        run("first").await;
        let more = parse_server("more", &entry("more.log"), ServerSource::Personal, &|_| None, "mcp.json").unwrap();
        session.mcp().expect("the session's servers").add(more).await.expect("added");
        let during = run("second").await;
        session.clear().await.unwrap();
        let after = run("third").await;
        session.close().await;

        let requests = model.requests.lock().unwrap().clone();
        let specs = |i: usize| serde_json::to_string(&requests[i].1.tools).unwrap();
        let (second, third) = if deferred { (1, 3) } else { (1, 2) };
        assert_eq!(specs(second), specs(0), "{mode:?}: the running conversation's tool specs changed");
        if deferred {
            assert!(during[0].contains("no MCP tool is named mcp__more__first"), "tool_search found it: {during:?}");
            assert!(during[1].contains("No MCP tool is named \"mcp__more__first\""), "mcp_call reached it: {during:?}");
            assert!(after[0].contains("\"name\":\"mcp__more__first\""), "a new conversation finds it: {after:?}");
        } else {
            assert!(specs(0).contains("mcp__dev__first"), "{}", specs(0));
            assert!(specs(third).contains("mcp__more__first"), "a new conversation gets it: {}", specs(third));
        }
    }
}
