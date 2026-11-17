//! `docs/SECURITY.md`, "Claude Code chats" (the harness plan, phase 1): a desktop chat run by the person's own
//! `claude`. Driven through `marv_web::claude_code::run_chat_with` with a stand-in `claude` (a shell script speaking
//! its stream-json), so nothing here needs Claude Code or a login.

use std::time::Duration;

use marv_guards::TempDir;
use marv_tools::types::Decision;
use marv_web::host::{ChatSpec, Harness};
use marv_web::protocol::ClientMessage;
use serde_json::{Value, json};
use tokio::sync::mpsc;

/// Marv's key for the chat: it must never reach Claude Code.
const MARV_KEY: &str = "sk-or-GUARD-must-not-reach-claude";

fn spec(root: &std::path::Path) -> ChatSpec {
    ChatSpec {
        root: root.to_path_buf(),
        config_dir: root.join(".marv"),
        api_key: MARV_KEY.into(),
        zai_api_key: None,
        kimi_api_key: None,
        chatgpt_auth: None,
        model: "z-ai/glm-5.3-flash".into(),
        max_steps: 10,
        yolo: true,
        resume: None,
        processes: false,
        sources: Vec::new(),
        mesh: false,
        mcp_off: Vec::new(),
        devices_off: Vec::new(),
        mesh_name: None,
        team_peers: Vec::new(),
        choices: false,
        chat_id: None,
        rules: false,
        manage_mcp: false,
        host_commands: false,
        kernel_events: None,
        timers: None,
        delegates: None,
        role: None,
        team: None,
        team_writes: Vec::new(),
        writes: None,
        harness: Harness::ClaudeCode,
        harness_sandbox: false,
        agent_model: None,
        resident: None,
        provider: None,
        grant: None,
        trajectories: false,
        meters: None,
    }
}

/// A stand-in `claude`: saves its arguments and environment, asks to write a file with a rule Claude Code would keep
/// in the project's settings, saves the answer, ends the turn.
#[cfg(unix)]
fn fake_claude(dir: &TempDir) -> std::path::PathBuf {
    let seen = dir.path().join("seen");
    let script = format!(
        r#"#!/usr/bin/env bash
{{ echo "ARGS $*"; env; }} > '{seen}/env'
# Marv OS's own control requests (`initialize`, for the models) come before the message: Claude Code answers them on
# the side.
while read -r line; do case "$line" in *'"control_request"'*) ;; *) break ;; esac; done
echo '{{"type":"system","subtype":"init","session_id":"s"}}'
echo '{{"type":"assistant","parent_tool_use_id":null,"message":{{"content":[{{"type":"tool_use","id":"t1","name":"Write","input":{{"file_path":"/p/a.txt","content":"x"}}}}]}}}}'
echo '{{"type":"control_request","request_id":"r1","request":{{"subtype":"can_use_tool","tool_name":"Write","input":{{"file_path":"/p/a.txt","content":"x"}},"permission_suggestions":[{{"type":"addRules","rules":[{{"toolName":"Write"}}],"behavior":"allow","destination":"projectSettings"}}],"tool_use_id":"t1"}}}}'
read -r answer
printf '%s\n' "$answer" > '{seen}/answer'
echo '{{"type":"result","subtype":"success","total_cost_usd":0.01,"usage":{{}}}}'
read -r _
"#,
        seen = seen.display()
    );
    std::fs::create_dir_all(&seen).unwrap();
    let path = dir.path().join("claude");
    std::fs::write(&path, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

async fn until(events: &mut mpsc::UnboundedReceiver<Value>, kind: &str) -> Value {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(20), events.recv()).await.expect(kind).expect(kind);
        if event["type"] == kind {
            return event;
        }
    }
}

/// One turn answered `decision`: what the stand-in got back, and what it saw of its environment and arguments.
#[cfg(unix)]
async fn turn(tag: &str, decision: Decision) -> (Value, String) {
    let dir = TempDir::new(tag);
    let bin = fake_claude(&dir);
    let (to_chat, incoming) = mpsc::unbounded_channel();
    let (out, mut events) = mpsc::unbounded_channel();
    let chat = tokio::spawn(marv_web::claude_code::run_chat_with(bin, spec(dir.path()), incoming, out));
    assert_eq!(until(&mut events, "ready").await["harness"], "claude-code");
    to_chat.send(ClientMessage::Send { text: "write a.txt".into() }).unwrap();
    let card = until(&mut events, "approval").await;
    assert_eq!(card["agent"], "Claude Code", "the card says who asks");
    to_chat.send(ClientMessage::Approval { id: card["id"].as_u64().unwrap(), decision }).unwrap();
    until(&mut events, "turn_end").await;
    let seen = dir.path().join("seen");
    let answer = std::fs::read_to_string(seen.join("answer")).unwrap();
    let env = std::fs::read_to_string(seen.join("env")).unwrap();
    drop(to_chat);
    let _ = tokio::time::timeout(Duration::from_secs(5), chat).await;
    (serde_json::from_str(answer.trim()).unwrap(), env)
}

/// **"Always" on a Claude Code card is for this chat only**: Claude Code suggests rules it would save in its
/// settings files (here the project's, shared with every `claude` in the folder), while the card says "for this
/// chat". Every rule goes back with the session as its destination.
///
/// Proven: `permission_response` sending the suggestions as they came → failed (`projectSettings`).
#[cfg(unix)]
#[tokio::test]
async fn always_on_a_claude_code_card_stays_in_this_chat() {
    let (answer, _) = turn("cc-always", Decision::Always).await;
    let response = &answer["response"]["response"];
    assert_eq!(response["behavior"], "allow");
    let rules = response["updatedPermissions"].as_array().expect("the rule goes back");
    assert!(!rules.is_empty() && rules.iter().all(|r| r["destination"] == "session"), "{rules:?}");
}

/// **A decline denies, and Marv's key never reaches Claude Code**: Claude Code runs on the person's own login, so
/// the chat's OpenRouter key is in neither its arguments nor its environment.
///
/// Proven: a decline mapped to allow → failed; a `MARV_` variable added to the process's environment → failed.
#[cfg(unix)]
#[tokio::test]
async fn a_decline_denies_and_marvs_key_never_reaches_claude_code() {
    let (answer, env) = turn("cc-decline", Decision::No).await;
    assert_eq!(answer["response"]["response"], json!({"behavior": "deny", "message": "The person declined."}));
    assert!(!env.contains(MARV_KEY), "Marv's key reached Claude Code");
    assert!(!env.lines().any(|l| l.starts_with("MARV_") || l.starts_with("OPENROUTER_API_KEY=")), "{env}");
    assert!(env.contains("--permission-prompt-tool stdio"), "prompts must come to Marv: {env}");
}

/// The person, for the mesh guards: answers every card with `answer` and keeps what was asked.
struct Person(Decision, std::sync::Mutex<Vec<marv_tools::ApprovalRequest>>);

#[async_trait::async_trait]
impl marv_tools::Approver for Person {
    async fn approve(&self, request: marv_tools::ApprovalRequest) -> Decision {
        self.1.lock().unwrap().push(request);
        self.0.clone()
    }
}

/// **Phase 3: a Claude Code chat's mesh tools are Marv's, and its mail is never clean.** Its MCP server
/// (`marv_web::harness_mesh`) offers the four mesh tools on the chat's main plug. A send with no wire asks the person
/// (a decline sends nothing), and mail it does send reaches the other agent marked not clean, since Marv can't see
/// what Claude Code read: a Marv reading it is tainted. A reply is given to it fenced, in the next call's result.
///
/// Proven: `harness_tools` with no wire approver → failed (nobody asked; refused unasked); a clean turn hop
/// registered for the harness's plug, as a Marv session registers its own → failed (the mail was clean).
#[tokio::test]
async fn a_claude_code_chats_mesh_send_asks_for_its_wire_and_its_mail_is_never_clean() {
    use marv_mesh::PlugId;
    use marv_session::mesh::{harness_tools, parent_id};
    let runtime = TempDir::short("cc-mesh");
    let mesh = std::sync::Arc::new(marv_mesh::switchboard::Switchboard::new(
        "cc",
        runtime.path().join("plugs"),
        std::sync::Arc::new(marv_mesh::switchboard::NoTrajectory),
    ));
    let stop = tokio_util::sync::CancellationToken::new();
    tokio::spawn(std::sync::Arc::clone(&mesh).serve(stop.clone()));
    let lead = PlugId::new("lead").unwrap();
    mesh.plug(lead.clone()).unwrap();
    let send = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "mesh_send", "arguments": {"receptacle": "agent.lead.inbox", "text": "the tests pass"}}});

    for (decision, delivered) in [(Decision::No, false), (Decision::Yes, true)] {
        let person = std::sync::Arc::new(Person(decision.clone(), Default::default()));
        let approver: std::sync::Arc<dyn marv_tools::Approver> = person.clone();
        let tools = harness_tools(&mesh, &approver, None).unwrap();
        let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
        let listed = marv_web::harness_mesh::answer(&list, &mesh, &tools, runtime.path(), &approver, &Default::default(), &stop)
            .await
            .unwrap();
        let names: Vec<&str> =
            listed["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        // H20: Marv OS's own tools a Marv member has (`wake_me` only where the host has timers).
        assert_eq!(
            names,
            ["mesh_list", "mesh_call", "mesh_send", "mesh_wait", "board_read", "board_post", "board_clear", "chat_read"]
        );
        let sent = marv_web::harness_mesh::answer(&send, &mesh, &tools, runtime.path(), &approver, &Default::default(), &stop)
            .await
            .unwrap();
        assert_eq!(person.1.lock().unwrap().len(), 1, "{decision:?}: the person is asked for the wire: {sent}");
        let mail = mesh.drain_inbox(&lead);
        assert_eq!(mail.len(), usize::from(delivered), "{decision:?}: {sent}");
        assert!(mail.iter().all(|m| !m.clean), "a harness's mail is never clean");
        assert!(mail.iter().all(|m| m.sender == parent_id()));
    }
    // The lead's reply: a Marv chat would read it at its next step; the harness gets it fenced in mesh_wait's result.
    mesh.wire(marv_mesh::wire::Wire::new(
        lead.clone(),
        marv_mesh::ReceptacleName::new("agent.parent.inbox").unwrap(),
        marv_mesh::wire::WireMode::Send,
        marv_mesh::wire::Grant::local(marv_mesh::wire::Approver::Tui),
    ))
    .unwrap();
    mesh.deliver(&lead, "agent.parent.inbox", marv_mesh::wire::WireMode::Send, json!({"text": "good, now the docs"}))
        .await
        .unwrap();
    let approver: std::sync::Arc<dyn marv_tools::Approver> = std::sync::Arc::new(Person(Decision::No, Default::default()));
    let tools = harness_tools(&mesh, &approver, None).unwrap();
    let wait = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "mesh_wait", "arguments": {"timeout_seconds": 1}}});
    let waited = marv_web::harness_mesh::answer(&wait, &mesh, &tools, runtime.path(), &approver, &Default::default(), &stop)
        .await
        .unwrap();
    let text = waited["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("good, now the docs") && text.contains(marv_mesh::message::FENCE_TAG), "{text}");
    assert!(mesh.unread(&parent_id()).is_empty(), "read once");
    stop.cancel();
}

/// **Phase 4: Claude Code runs inside Marv's outer sandbox.** A stand-in `claude` run through
/// `marv_web::harness_sandbox` (a fake home) can read and write its project and Claude Code's own config, and nothing
/// else of the home: not another project, not `~/.ssh`, not Marv's config; and none of Marv's variables reach it.
///
/// Proven: `plan` without the stand-in home (`sandbox.home` left as a folder outside the fake home) → failed (the
/// other project and `~/.ssh` showed); `passed_env` letting `MARV_*` through → failed (`LEAK`); no writable bind for
/// the config → failed (Claude's settings unreadable).
#[cfg(unix)]
#[test]
fn claude_code_in_the_outer_sandbox_sees_its_project_and_config_and_nothing_else() {
    if !marv_web::harness_sandbox::available() {
        eprintln!("skipped: bubblewrap isn't available here");
        return;
    }
    let dir = TempDir::new("cc-sandbox");
    let home = dir.path().join("home");
    let (project, other) = (home.join("Projects/app"), home.join("Projects/other"));
    for d in [&project, &other, &home.join(".ssh"), &home.join(".marv"), &home.join(".claude"), &home.join("bin")] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(other.join("secret.txt"), "other project").unwrap();
    std::fs::write(home.join(".ssh/id_ed25519"), "key").unwrap();
    std::fs::write(home.join(".marv/config.json"), MARV_KEY).unwrap();
    std::fs::write(home.join(".claude/settings.json"), "CLAUDE-SETTINGS").unwrap();
    std::fs::write(home.join(".claude.json"), "{}").unwrap();
    let h = home.display();
    let claude = home.join("bin/claude");
    std::fs::write(
        &claude,
        format!(
            "#!/bin/sh\n\
             test -e '{h}/Projects/other/secret.txt' && echo OTHER\n\
             test -e '{h}/.ssh/id_ed25519' && echo SSH\n\
             grep -q GUARD '{h}/.marv/config.json' 2>/dev/null && echo MARV\n\
             cat '{h}/.claude/settings.json'; echo\n\
             echo w > '{h}/.claude/new' && echo CLAUDE-WRITE\n\
             echo p > '{h}/Projects/app/made' && echo PROJECT-WRITE\n\
             env | grep -E '^(MARV_|OPENROUTER_API_KEY)' && echo LEAK\n\
             env | grep -q '^ANTHROPIC_API_KEY=k$' && echo ANTHROPIC\n"
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    let config = marv_web::harness_sandbox::config_paths(&home, None);
    let plan = marv_web::harness_sandbox::plan(&home, &project, &claude, &config, None);
    let env: Vec<(String, String)> = [("ANTHROPIC_API_KEY", "k"), ("MARV_CONFIG_DIR", "/x"), ("OPENROUTER_API_KEY", MARV_KEY)]
        .into_iter()
        .filter(|(k, _)| marv_web::harness_sandbox::passed_env(k))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let argv = marv_web::harness_sandbox::argv(&plan, &env, &[claude.clone().into_os_string()]).unwrap();
    let out = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .env_clear()
        .env("PATH", &plan.path)
        .current_dir(&project)
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{said}\n{}", String::from_utf8_lossy(&out.stderr));
    for hidden in ["OTHER", "SSH", "MARV", "LEAK"] {
        assert!(!said.lines().any(|l| l == hidden), "{hidden} reached Claude Code: {said}");
    }
    for shown in ["CLAUDE-SETTINGS", "CLAUDE-WRITE", "PROJECT-WRITE", "ANTHROPIC"] {
        assert!(said.contains(shown), "{shown} missing: {said}");
    }
    assert!(home.join(".claude/new").is_file() && project.join("made").is_file(), "its writes are real");
}

/// **An agent's Home chat asks before each change** (docs/SECURITY.md, Claude Code chats). With the home folder as
/// its project, a stand-in agent can write in the home's visible folders and reach its own login, while the home's
/// private entries (`~/.ssh`) and Marv's config stay hidden; and every card it raises there is yes-or-no, asked of the
/// person, never answered from an earlier "always".
///
/// Proven: `plan` without `over_masks` (the config bound before the masks) → failed (`CLAUDE-SETTINGS` missing, as the
/// real `claude` said "Not logged in"); `home_masks` skipped for a writable home → failed (`SSH`); `home_card` leaving
/// the card as it came → failed (`once`).
#[cfg(unix)]
#[test]
fn an_agents_home_chat_writes_the_home_but_not_its_secrets_and_asks_every_time() {
    let card = || {
        marv_web::claude_code::approval_request(
            &json!({"tool_name": "Write", "input": {"file_path": "/x"}, "permission_suggestions": [{"type": "addRules"}]}),
            true,
        )
    };
    let mut home_spec = spec(std::path::Path::new("/"));
    home_spec.root = marv_session::paths::home_dir();
    let asked = marv_web::claude_code::home_card(card(), &home_spec);
    assert!(asked.once && asked.must_ask, "a Home chat's card is yes-or-no and asked every time");
    let elsewhere = marv_web::claude_code::home_card(card(), &spec(&marv_session::paths::home_dir().join("Projects/app")));
    assert!(!elsewhere.once && !elsewhere.must_ask, "a project's card keeps its \"always\"");
    if !marv_web::harness_sandbox::available() {
        eprintln!("skipped: bubblewrap isn't available here");
        return;
    }
    let dir = TempDir::new("cc-home");
    let home = dir.path().join("home");
    for d in [&home.join("Documents"), &home.join(".ssh"), &home.join(".marv"), &home.join(".claude"), &home.join("bin")] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(home.join(".ssh/id_ed25519"), "key").unwrap();
    std::fs::write(home.join(".marv/config.json"), MARV_KEY).unwrap();
    std::fs::write(home.join(".claude/settings.json"), "CLAUDE-SETTINGS").unwrap();
    std::fs::write(home.join(".claude.json"), "{}").unwrap();
    let h = home.display();
    let claude = home.join("bin/claude");
    std::fs::write(
        &claude,
        format!(
            "#!/bin/sh\n\
             test -e '{h}/.ssh/id_ed25519' && echo SSH\n\
             grep -q GUARD '{h}/.marv/config.json' 2>/dev/null && echo MARV\n\
             cat '{h}/.claude/settings.json'; echo\n\
             echo w > '{h}/.claude/new' && echo CLAUDE-WRITE\n\
             echo n > '{h}/Documents/note.txt' && echo HOME-WRITE\n"
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    let config = marv_web::harness_sandbox::config_paths(&home, None);
    let plan = marv_web::harness_sandbox::plan(&home, &home, &claude, &config, None);
    let argv = marv_web::harness_sandbox::argv(&plan, &[], &[claude.clone().into_os_string()]).unwrap();
    let out = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .env_clear()
        .env("PATH", &plan.path)
        .current_dir(&home)
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{said}\n{}", String::from_utf8_lossy(&out.stderr));
    for hidden in ["SSH", "MARV"] {
        assert!(!said.lines().any(|l| l == hidden), "{hidden} reached the agent: {said}");
    }
    for shown in ["CLAUDE-SETTINGS", "CLAUDE-WRITE", "HOME-WRITE"] {
        assert!(said.contains(shown), "{shown} missing: {said}");
    }
    assert!(home.join("Documents/note.txt").is_file() && home.join(".claude/new").is_file(), "its writes are real");
    assert_eq!(std::fs::read_to_string(home.join(".ssh/id_ed25519")).unwrap(), "key");
}

/// **H6: a harness team member's role write set is enforced by the outer sandbox** (docs/SECURITY.md, Claude Code
/// chats, phase 3 part 2). Inside it, the agent writes under its role's prefix (made first when missing) and nowhere
/// else in the project; a prefix whose path leads out through a symlink is never made; a role that writes nothing
/// changes nothing.
///
/// Proven: `bound_to_role` not setting `project_read_only` → failed (README.md changed); creating prefixes without the
/// inside-the-project check → failed (the folder appeared outside).
#[cfg(unix)]
#[test]
fn a_harness_members_write_set_is_all_its_agent_can_write() {
    if !marv_web::harness_sandbox::available() {
        eprintln!("skipped: bubblewrap isn't available here");
        return;
    }
    let dir = TempDir::new("cc-writes");
    let home = dir.path().join("home");
    let project = home.join("Projects/app");
    let outside = home.join("Projects/other");
    for d in [&project, &outside] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(project.join("README.md"), "kept\n").unwrap();
    std::os::unix::fs::symlink(&outside, project.join("link")).unwrap();
    // The shell by its real path: the plan binds the program at its path, and bwrap can't mount on a symlink.
    let sh = std::fs::canonicalize("/bin/sh").unwrap();
    let run = |set: marv_tools::WriteSet, script: &str| {
        let mut plan = marv_web::harness_sandbox::plan(&home, &project, &sh, &[], None);
        marv_web::harness_sandbox::bound_to_role(&mut plan, &set, &project, &home);
        let inner: Vec<std::ffi::OsString> = vec![sh.clone().into(), "-c".into(), script.into()];
        let argv = marv_web::harness_sandbox::argv(&plan, &[], &inner).unwrap();
        std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .env_clear()
            .env("PATH", &plan.path)
            .current_dir(&project)
            .output()
            .unwrap()
    };

    let out = run(
        marv_tools::WriteSet::new(vec!["site".into(), "link/new".into()]),
        "echo hi > site/index.html; echo no > README.md; touch other.txt; true",
    );
    assert!(!String::from_utf8_lossy(&out.stderr).starts_with("bwrap:"), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(std::fs::read_to_string(project.join("site/index.html")).unwrap(), "hi\n", "its prefix, made for it");
    assert_eq!(std::fs::read_to_string(project.join("README.md")).unwrap(), "kept\n", "the rest is read-only");
    assert!(!project.join("other.txt").exists());
    assert!(!outside.join("new").exists(), "nothing made through a symlink leading out");

    run(marv_tools::WriteSet::new(Vec::new()), "touch site/second.html; true");
    assert!(!project.join("site/second.html").exists(), "a role that writes nothing changes nothing");
}

/// A stand-in ACP agent (OpenCode's shapes, recorded 2026-11-08): saves its environment and every request, asks one
/// permission per prompt, says "done", ends the turn.
#[cfg(unix)]
fn fake_acp(dir: &TempDir) -> std::path::PathBuf {
    let seen = dir.path().join("seen");
    std::fs::create_dir_all(&seen).unwrap();
    let script = format!(
        r#"#!/usr/bin/env python3
import json, os, sys
seen = {seen:?}
open(os.path.join(seen, "env"), "w").write("\n".join(f"{{k}}={{v}}" for k, v in os.environ.items()))
log = open(os.path.join(seen, "requests"), "a")
def send(o):
    sys.stdout.write(json.dumps(o) + "\n"); sys.stdout.flush()
for line in sys.stdin:
    m = json.loads(line)
    log.write(json.dumps(m) + "\n"); log.flush()
    method = m.get("method")
    if method == "initialize":
        send({{"jsonrpc": "2.0", "id": m["id"], "result": {{"protocolVersion": 1, "agentCapabilities": {{"loadSession": True}}}}}})
    elif method == "session/new":
        send({{"jsonrpc": "2.0", "id": m["id"], "result": {{"sessionId": "ses_1"}}}})
    elif method == "session/load":
        send({{"jsonrpc": "2.0", "id": m["id"], "result": None}})
    elif method == "session/prompt":
        sid = m["params"]["sessionId"]
        send({{"jsonrpc": "2.0", "id": 900, "method": "session/request_permission", "params": {{"sessionId": sid, "toolCall": {{"toolCallId": "c1", "name": "shell", "title": "rm -rf build", "kind": "execute", "rawInput": {{"command": "rm -rf build"}}}}, "options": [{{"optionId": "once", "kind": "allow_once"}}, {{"optionId": "always", "kind": "allow_always"}}, {{"optionId": "reject", "kind": "reject_once"}}]}}}})
        answer = json.loads(sys.stdin.readline())
        open(os.path.join(seen, "answer"), "w").write(json.dumps(answer))
        send({{"jsonrpc": "2.0", "method": "session/update", "params": {{"sessionId": sid, "update": {{"sessionUpdate": "agent_message_chunk", "content": {{"type": "text", "text": "done"}}}}}}}})
        send({{"jsonrpc": "2.0", "id": m["id"], "result": {{"stopReason": "end_turn", "usage": {{"inputTokens": 5, "outputTokens": 1}}}}}})
"#
    );
    let path = dir.path().join("opencode");
    std::fs::write(&path, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// **Phase 4: an ACP agent's chat (OpenCode) asks through Marv, and Marv's key never reaches it.** It's started with
/// its tools set to ask (`OPENCODE_CONFIG_CONTENT`); its permission request is a card that says who asks, answered
/// with the agent's own option (a decline is its "reject"); Marv's key and variables aren't in its environment; and
/// a saved chat continues its session (`session/load` with the saved id).
///
/// Proven: `permission_result` picking allow for a decline → failed; `Preset::own_env` empty → failed (no asks set);
/// `open` ignoring the saved id → failed (`session/new` again).
#[cfg(unix)]
#[tokio::test]
async fn an_acp_agent_asks_through_marv_and_continues_its_session() {
    use marv_web::acp::{Preset, run_chat_with};
    for (decision, option, resume) in [(Decision::No, "reject", None), (Decision::Always, "always", Some("ses_1"))] {
        let dir = TempDir::new("acp-chat");
        let bin = fake_acp(&dir);
        let mut chat = spec(dir.path());
        chat.harness = Harness::Acp(Preset::OpenCode);
        chat.resume = resume.map(|id| marv_session::ResumeTarget::Id(id.into()));
        let (to_chat, incoming) = mpsc::unbounded_channel();
        let (out, mut events) = mpsc::unbounded_channel();
        let task = tokio::spawn(run_chat_with(Preset::OpenCode, bin, chat, incoming, out));
        assert_eq!(until(&mut events, "ready").await["harness"], "opencode");
        to_chat.send(ClientMessage::Send { text: "clean the build".into() }).unwrap();
        let card = until(&mut events, "approval").await;
        assert_eq!(card["agent"], "OpenCode", "the card says who asks");
        to_chat.send(ClientMessage::Approval { id: card["id"].as_u64().unwrap(), decision: decision.clone() }).unwrap();
        assert_eq!(until(&mut events, "turn_end").await["reason"], "end");
        let seen = dir.path().join("seen");
        let answer: Value = serde_json::from_str(&std::fs::read_to_string(seen.join("answer")).unwrap()).unwrap();
        assert_eq!(answer["result"]["outcome"]["optionId"], option, "{decision:?}");
        let env = std::fs::read_to_string(seen.join("env")).unwrap();
        assert!(!env.contains(MARV_KEY) && !env.lines().any(|l| l.starts_with("MARV_")), "{env}");
        assert!(
            env.lines().any(|l| l.starts_with("OPENCODE_CONFIG_CONTENT=") && l.contains("\"bash\":\"ask\"")),
            "its tools ask"
        );
        let requests = std::fs::read_to_string(seen.join("requests")).unwrap();
        match resume {
            Some(id) => {
                assert!(requests.contains("\"session/load\"") && requests.contains(id) && !requests.contains("\"session/new\""))
            }
            None => assert!(requests.contains("\"session/new\"")),
        }
        drop(to_chat);
        let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
    }
}

/// Live, by hand (`--ignored`): the person's real Claude Code and OpenCode, in the outer sandbox, list their models
/// for the chat's picker and answer a message on the one picked.
#[tokio::test]
#[ignore]
async fn live_agents_list_their_models_and_switch() {
    use marv_web::acp::Preset;
    for harness in [Harness::ClaudeCode, Harness::Acp(Preset::OpenCode)] {
        let project = TempDir::new("live-models");
        let mut chat = spec(project.path());
        chat.harness = harness;
        chat.harness_sandbox = marv_web::harness_sandbox::available();
        let (to_chat, incoming) = mpsc::unbounded_channel();
        let (out, mut events) = mpsc::unbounded_channel();
        let task = match harness {
            Harness::Acp(preset) => tokio::spawn(marv_web::acp::run_chat_with(
                preset,
                marv_web::acp::find(preset).expect("opencode"),
                chat,
                incoming,
                out,
            )),
            _ => tokio::spawn(marv_web::claude_code::run_chat_with(
                marv_web::claude_code::find_claude().expect("claude"),
                chat,
                incoming,
                out,
            )),
        };
        until(&mut events, "ready").await;
        to_chat.send(ClientMessage::AgentModels).unwrap();
        let listed = until(&mut events, "agent_models").await;
        let models = listed["models"].as_array().cloned().unwrap_or_default();
        eprintln!(
            "{harness:?}: {} models, current {}, first {:?}",
            models.len(),
            listed["current"],
            models.iter().take(4).map(|m| m["id"].clone()).collect::<Vec<_>>()
        );
        assert!(listed["error"].is_null() && models.len() > 1, "{listed}");
        let pick = models
            .iter()
            .map(|m| m["id"].as_str().unwrap().to_string())
            .find(|id| {
                Some(id.as_str()) != listed["current"].as_str()
                    && id != "default"
                    && (id.contains("haiku") || id.contains("free") || id.contains("pickle"))
            })
            .unwrap_or_else(|| models[1]["id"].as_str().unwrap().into());
        to_chat.send(ClientMessage::AgentModel { model: pick.clone() }).unwrap();
        assert_eq!(until(&mut events, "agent_model").await["model"], pick.as_str());
        to_chat.send(ClientMessage::Send { text: "Reply with just the word OK.".into() }).unwrap();
        let mut said = String::new();
        loop {
            let e = tokio::time::timeout(Duration::from_secs(120), events.recv()).await.expect("turn").expect("turn");
            if e["type"] == "notice" || e["type"] == "error" {
                said.push_str(&format!("{e}\n"));
            }
            if e["type"] == "approval" {
                to_chat.send(ClientMessage::Approval { id: e["id"].as_u64().unwrap(), decision: Decision::No }).unwrap();
            }
            if e["type"] == "turn_end" {
                eprintln!("{harness:?} on {pick}: {e} {said}");
                assert_eq!(e["reason"], "end", "{said}");
                break;
            }
        }
        assert!(!said.contains("couldn't switch") && !said.contains("mise"), "{said}");
        drop(to_chat);
        let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
    }
}

/// Live, by hand (`--ignored`): a Claude Code chat's terminal command, written for a terminal to run
/// (`MARV_TERM_OUT`): the real `claude` interface in a project, inside the outer sandbox.
#[test]
#[ignore]
fn live_terminal_command_for_a_chat() {
    let project = std::path::PathBuf::from(std::env::var("MARV_TERM_PROJECT").expect("MARV_TERM_PROJECT"));
    let mut chat = spec(&project);
    chat.harness_sandbox = marv_web::harness_sandbox::available();
    let session = format!("8f1c2a3e-0000-4000-8000-{:012x}", std::process::id());
    let events = std::path::PathBuf::from(std::env::var("MARV_TERM_EVENTS").expect("MARV_TERM_EVENTS"));
    std::fs::write(&events, "").unwrap();
    let command = marv_web::claude_code::terminal_command(&chat, &session, false, &events).unwrap();
    let argv: Vec<String> = command.argv.iter().map(|a| a.to_string_lossy().into_owned()).collect();
    let out = json!({"argv": argv, "path": command.path, "session": session});
    std::fs::write(std::env::var("MARV_TERM_OUT").expect("MARV_TERM_OUT"), out.to_string()).unwrap();
}

/// Live, by hand (`--ignored`): every installed agent's terminal command for a new chat in `MARV_TERM_PROJECT`, as
/// JSON lines in `MARV_TERM_OUT` (for a terminal to run).
#[test]
#[ignore]
fn live_terminal_commands_for_every_agent() {
    let project = std::path::PathBuf::from(std::env::var("MARV_TERM_PROJECT").expect("MARV_TERM_PROJECT"));
    let events = project.join("events.jsonl");
    let mut out = String::new();
    for agent in marv_web::terminal_agents::AGENTS.iter().filter(|a| a.installed().is_some()) {
        let mut chat = spec(&project);
        chat.harness_sandbox = false;
        let session = format!("8f1c2a3e-0000-4000-8000-{:012x}", std::process::id());
        match marv_web::terminal_agents::command(
            agent,
            &chat,
            &session,
            false,
            &events,
            marv_web::terminal_agents::Permissions::Own,
        ) {
            Ok(c) => {
                let argv: Vec<String> = c.argv.iter().map(|a| a.to_string_lossy().into_owned()).collect();
                out.push_str(&json!({"agent": agent.id, "argv": argv, "path": c.path, "withheld": c.withheld}).to_string());
            }
            Err(e) => out.push_str(&json!({"agent": agent.id, "error": e}).to_string()),
        }
        out.push('\n');
    }
    std::fs::write(std::env::var("MARV_TERM_OUT").expect("MARV_TERM_OUT"), out).unwrap();
}

/// Live, by hand (`--ignored`): how long Marv OS takes to build each installed agent's terminal command (its own work
/// before the agent starts: finding the program, mise's lookups, the sandbox plan), direct and sandboxed, written with
/// the commands to `MARV_TERM_OUT` for a terminal to time the agents' own start.
#[test]
#[ignore]
fn live_terminal_command_build_times() {
    let project = std::path::PathBuf::from(std::env::var("MARV_TERM_PROJECT").expect("MARV_TERM_PROJECT"));
    let events = project.join("events.jsonl");
    let mut out = String::new();
    for sandbox in [false, true] {
        for agent in marv_web::terminal_agents::AGENTS.iter().filter(|a| a.installed().is_some()) {
            let mut chat = spec(&project);
            chat.harness_sandbox = sandbox && marv_web::harness_sandbox::available();
            let session = format!("8f1c2a3e-0000-4000-8000-{:012x}", std::process::id());
            let started = std::time::Instant::now();
            let built = marv_web::terminal_agents::command(
                agent,
                &chat,
                &session,
                false,
                &events,
                marv_web::terminal_agents::Permissions::Own,
            );
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            let line = match built {
                Ok(c) => {
                    json!({"agent": agent.id, "sandbox": sandbox, "build_ms": ms, "argv": c.argv.iter().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>(), "path": c.path, "env": c.env})
                }
                Err(e) => json!({"agent": agent.id, "sandbox": sandbox, "build_ms": ms, "error": e}),
            };
            out.push_str(&line.to_string());
            out.push('\n');
        }
    }
    std::fs::write(std::env::var("MARV_TERM_OUT").expect("MARV_TERM_OUT"), out).unwrap();
}
