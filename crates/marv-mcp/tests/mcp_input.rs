//! A server that asks the person before it answers (marv-mcp's `input`), against a test server shaped like Google's
//! Gmail MCP server as it answered on 2026-11-26: content withheld from a client that doesn't say it can ask, a
//! confirmation (`structuredContent.inputRequiredResult`) to one that does, and HTTP 403 with a JSON-RPC body while
//! its API is off in the person's Cloud project.

mod support;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use marv_mcp::input::Confirmation;
use marv_mcp::manager::McpState;
use marv_mcp::tool::{Confirmer, McpTool};
use serde_json::{Value, json};
use support::*;
use tokio_util::sync::CancellationToken;

const STATE: &str = "{\"elicitationId\":\"elicitations/1\",\"requestBinding\":\"b\"}";

fn ask() -> Value {
    json!({"structuredContent": {"inputRequiredResult": {
        "inputRequests": {"approval_choice": {"elicitRequest": {
            "message": "This agent wants to read **\"Lunch?\"** from an external user. Do you want to allow this?",
            "requestedSchema": {"approval_choice": {"enumSchema": {
                "enumList": ["ALLOW_ONCE", "ALLOW_RESOURCE_ALWAYS", "ALLOW_TOOL_ALWAYS", "DENY_ONCE"],
                "enumNames": ["Allow once", "Always allow for this email", "Always allow this tool", "Don't allow"],
                "title": "Allow this app to use search_threads for this email?"
            }}}
        }, "requiredFields": ["approval_choice"]}},
        "metadata": {"isConfirmation": true, "title": "Use this data?"},
        "requestState": STATE,
        "resultType": "RESULT_TYPE_INPUT_REQUIRED"
    }}})
}

/// Google's Gmail server, as far as these tests need it. `disabled`: its API is off (HTTP 403 on everything).
fn gmail(message: &Value, disabled: bool, calls: &Mutex<Vec<Value>>) -> (u16, Option<Value>) {
    let Some(id) = message.get("id").cloned() else { return (202, None) };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let reply = |result: Value| Some(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    let status = if disabled { 403 } else { 200 };
    match message["method"].as_str().unwrap_or_default() {
        "initialize" => (
            200,
            reply(
                json!({"protocolVersion": params["protocolVersion"], "capabilities": {"tools": {}}, "serverInfo": {"name": "StatelessServer", "version": "ESF"}}),
            ),
        ),
        "tools/list" => (
            status,
            reply(
                json!({"tools": [{"name": "search_threads", "inputSchema": {"type": "object", "properties": {"query": {"type": "string"}}}, "annotations": {"readOnlyHint": true}}]}),
            ),
        ),
        "tools/call" => {
            calls.lock().unwrap().push(params.clone());
            if disabled {
                let text = "Gmail MCP API has not been used in project 1 before or it is disabled. Enable it by visiting https://console.developers.google.com/apis/api/gmailmcp.googleapis.com/overview?project=1 then retry.";
                return (403, reply(json!({"content": [{"type": "text", "text": text}], "isError": true})));
            }
            let can_ask = params["_meta"]["io.modelcontextprotocol/clientCapabilities"]["elicitation"]["form"].is_object();
            if !can_ask {
                let withheld = json!({"resultCountEstimate": "1", "omittedContent": {"omittedReasonCounts": [{"reason": "DATA_PROTECTION_POLICY", "omittedCount": 1}]}});
                return (
                    200,
                    reply(json!({"content": [{"type": "text", "text": withheld.to_string()}], "structuredContent": withheld})),
                );
            }
            let answer = &params["inputResponses"]["approval_choice"];
            let chosen =
                answer["elicitResult"]["content"]["approval_choice"].as_str().or(answer["content"]["approval_choice"].as_str());
            match chosen {
                Some(c) if c.starts_with("ALLOW") && !c.ends_with("UNKNOWN") && params["requestState"] == STATE => {
                    (200, reply(json!({"content": [{"type": "text", "text": "Lunch? from sam@example.com"}]})))
                }
                _ => (200, reply(ask())),
            }
        }
        _ => (200, Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "Method not found"}}))),
    }
}

async fn gmail_server(disabled: bool) -> (HttpServer, Arc<Mutex<Vec<Value>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = calls.clone();
    let server = serve(Arc::new(move |req: Req| {
        let seen = seen.clone();
        Box::pin(async move {
            let Ok(message) = serde_json::from_str::<Value>(&req.body) else { return Resp::text(400, "bad json") };
            match gmail(&message, disabled, &seen) {
                (status, Some(body)) => {
                    Resp { status, headers: vec![("content-type".into(), "application/json".into())], body: body.to_string() }
                }
                (status, None) => Resp::text(status, ""),
            }
        })
    }))
    .await;
    (server, calls)
}

/// Answers every question with one of its values, and keeps what it was asked.
struct Picks(Option<&'static str>, Mutex<Vec<Confirmation>>);

#[async_trait]
impl Confirmer for Picks {
    async fn confirm(&self, _tool: &McpTool, confirmation: &Confirmation) -> Option<String> {
        self.1.lock().unwrap().push(confirmation.clone());
        self.0.map(str::to_string)
    }
}

async fn connected(disabled: bool) -> (Arc<marv_mcp::manager::McpManager>, HttpServer, Arc<Mutex<Vec<Value>>>) {
    let (server, calls) = gmail_server(disabled).await;
    let m = manager(vec![remote("gmail", &format!("{}/mcp", server.base()), json!({}))], Opts::new(temp_dir("root")));
    m.start().await;
    assert_eq!(m.status()[0].state, McpState::Connected, "{:?}", m.status()[0].error);
    (m, server, calls)
}

async fn search(m: &marv_mcp::manager::McpManager, confirmer: &dyn Confirmer) -> (String, bool) {
    let tools = m.tools();
    let tool = tools.iter().find(|t| t.name == "mcp__gmail__search_threads").unwrap();
    let args = json!({"query": "in:inbox"}).as_object().cloned().unwrap();
    let r = tool.run_asking(args, CancellationToken::new(), confirmer).await.unwrap();
    (r.output, r.is_error)
}

#[tokio::test]
async fn an_allowed_read_is_sent_again_with_the_answer_and_the_servers_state() {
    let (m, _server, calls) = connected(false).await;
    let picks = Picks(Some("ALLOW_ONCE"), Mutex::default());
    assert_eq!(search(&m, &picks).await, ("Lunch? from sam@example.com".into(), false));
    let asked = picks.1.lock().unwrap();
    assert_eq!(asked.len(), 1);
    assert!(asked[0].message.contains("from an external user"), "{}", asked[0].message);
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2, "the call, then its retry");
    assert!(calls[0].get("inputResponses").is_none());
    assert_eq!(calls[1]["requestState"], STATE);
    assert_eq!(calls[1]["arguments"], json!({"query": "in:inbox"}));
}

#[tokio::test]
async fn a_read_not_allowed_says_so_and_isnt_asked_twice() {
    let (m, _server, calls) = connected(false).await;
    let picks = Picks(Some("DENY_ONCE"), Mutex::default());
    let (output, is_error) = search(&m, &picks).await;
    assert!(is_error && output.contains("you didn't allow it"), "{output}");
    assert_eq!(picks.1.lock().unwrap().len(), 1);
    assert_eq!(calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn with_no_one_to_ask_the_question_is_declined() {
    let (m, _server, _calls) = connected(false).await;
    let (output, is_error) = call(&m, "mcp__gmail__search_threads", json!({"query": "in:inbox"})).await;
    assert!(is_error && output.contains("you didn't allow it"), "{output}");
}

#[tokio::test]
async fn a_disabled_api_still_lists_its_tools_and_a_call_gives_googles_reason() {
    let (m, _server, _calls) = connected(true).await;
    let (output, is_error) = call(&m, "mcp__gmail__search_threads", json!({"query": "in:inbox"})).await;
    assert!(is_error && output.contains("Gmail MCP API has not been used in project 1"), "{output}");
}

#[tokio::test]
async fn a_server_that_asks_again_after_an_allow_is_stopped_not_asked_over_and_over() {
    let (m, _server, _calls) = connected(false).await;
    // An answer the server doesn't take: it asks the same thing again.
    let picks = Picks(Some("ALLOW_RESOURCE_ALWAYS_UNKNOWN"), Mutex::default());
    let (output, is_error) = search(&m, &picks).await;
    assert!(is_error && output.contains("asked again after you allowed it"), "{output}");
    assert_eq!(picks.1.lock().unwrap().len(), 1);
}
