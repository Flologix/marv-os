//! `docs/SECURITY.md`, "Doors out of the sandbox": the process door (Marv #51). What a background process must never
//! manage, with the real bubblewrap (Marv's `tests/guards/process.test.ts`). The relay is the `marv-guards-fixture`
//! binary, standing in for `marv __relay`. Skipped only where bwrap can't run; CI requires it.

use std::sync::Arc;
use std::time::Duration;

use marv_core::Door;
use marv_guards::{Recorder, TempDir, call, require_bwrap};
use marv_session::processes::{ProcessManager, ProcessManagerOptions};
use marv_tools::process::NO_PROCESS_DOOR;
use marv_tools::{Decision, ProcessHost, ToolContext, builtin_tools, run_tool};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const FIXTURE: &str = env!("CARGO_BIN_EXE_marv-guards-fixture");

/// Decoys outside `/tmp` (the sandbox's private `/tmp` would hide them anyway, and the tests would pass without the
/// guards): a home with a secret, the project inside it with a `.git`, and a folder outside both.
struct Decoys {
    _base: TempDir,
    home: std::path::PathBuf,
    root: std::path::PathBuf,
    outside: std::path::PathBuf,
    runtime: TempDir,
}

impl Decoys {
    fn new(tag: &str) -> Self {
        let base = TempDir::outside_tmp(tag);
        base.write("home/secret.txt", "DECOY-HOME-SECRET");
        base.write("home/proj/readme.txt", "PROJECT-FILE");
        base.write("home/proj/.git/hooks/.keep", "");
        base.write("outside/.keep", "");
        let (home, root, outside) = (base.path().join("home"), base.path().join("home/proj"), base.path().join("outside"));
        Self { _base: base, home, root, outside, runtime: TempDir::new(&format!("{tag}-rt")) }
    }

    fn manager(&self, start_wait_ms: u64) -> ProcessManager {
        marv_tools::relay::set_relay_exe(FIXTURE);
        let mut options = ProcessManagerOptions::new(&self.root);
        options.home = Some(self.home.clone());
        options.runtime_dir = Some(self.runtime.path().to_path_buf());
        options.poll = Duration::from_millis(100);
        options.start_wait = Duration::from_millis(start_wait_ms);
        ProcessManager::new(options)
    }
}

async fn until(check: impl Fn() -> bool, within: Duration) -> bool {
    let end = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < end {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    check()
}

/// Starts `command`, waits for it to end, and returns its logs.
async fn output(manager: &ProcessManager, command: &str, network: bool) -> String {
    let started = manager.start(&format!("{command}; echo END"), None, network).await.unwrap();
    let name = started.info.name;
    until(|| manager.logs(&name, 400).unwrap_or_default().contains("END"), Duration::from_secs(8)).await;
    manager.logs(&name, 400).unwrap()
}

fn pgrep(marker: &str) -> String {
    let out = std::process::Command::new("pgrep").args(["-f", marker]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port()
}

/// The body of `GET /` at host:port; an error when nothing answers.
async fn get(host: &str, port: u16) -> std::io::Result<String> {
    let wait = Duration::from_secs(2);
    let mut conn = tokio::time::timeout(wait, tokio::net::TcpStream::connect((host, port))).await??;
    conn.write_all(b"GET / HTTP/1.0\r\n\r\n").await?;
    let mut reply = Vec::new();
    tokio::time::timeout(wait, conn.read_to_end(&mut reply)).await??;
    let reply = String::from_utf8_lossy(&reply).into_owned();
    reply.split_once("\r\n\r\n").map(|(_, body)| body.to_string()).ok_or_else(|| std::io::Error::other(reply))
}

/// A non-loopback IPv4 address of this machine, if it has one.
fn other_address() -> Option<String> {
    let out = std::process::Command::new("ip").args(["-4", "-o", "addr", "show", "scope", "global"]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let word = text.split_whitespace().skip_while(|w| *w != "inet").nth(1)?;
    Some(word.split('/').next()?.to_string())
}

/// **A background process can't write outside the project** (but can inside it): it runs in the same sandbox a bash
/// command does.
///
/// Proven: started the process's command with `bwrap` replaced by plain `bash` (`launch` building `["bash", "-c",
/// script]` instead of `Sandbox::prepare`) → failed (`planted` written outside).
#[tokio::test]
async fn a_process_cant_write_outside_the_project() {
    require_bwrap!();
    let decoys = Decoys::new("proc-write");
    let manager = decoys.manager(1500);
    output(&manager, &format!("echo x > {}/planted; echo x > {}/made", decoys.outside.display(), decoys.root.display()), false)
        .await;
    assert!(!decoys.outside.join("planted").exists());
    assert!(decoys.root.join("made").exists()); // control
    manager.close_all().await;
}

/// **A chat role's write set bounds a background process too** (runtime-kernel coordination): with the set given to
/// the manager (`bound_writes`, as the session does from its role), the project is read-only but for the set's
/// prefixes, so a process a task turn started unasked can't write past the role.
///
/// Proven: dropped the `write_set` match in `launch` (back to `project_read_only: false`, no writable prefixes) →
/// failed (`script.js` overwritten).
#[tokio::test]
async fn a_process_is_bounded_by_the_roles_write_set() {
    require_bwrap!();
    let decoys = Decoys::new("proc-write-set");
    std::fs::create_dir_all(decoys.root.join("pages")).unwrap();
    std::fs::write(decoys.root.join("script.js"), "kept\n").unwrap();
    let manager = decoys.manager(1500);
    manager.bound_writes(Some(marv_tools::WriteSet::new(vec!["pages".into()])));
    output(&manager, "echo no > script.js; echo yes > pages/made.html", false).await;
    assert_eq!(std::fs::read_to_string(decoys.root.join("script.js")).unwrap(), "kept\n");
    assert_eq!(std::fs::read_to_string(decoys.root.join("pages/made.html")).unwrap(), "yes\n"); // control
    manager.close_all().await;
}

/// **A background process can't change the project's `.git`**: a hook planted there would run outside the sandbox
/// at the user's next commit.
///
/// Proven: dropped `git_guard`'s read-only mounts in `launch` → failed (`pre-commit` written).
#[tokio::test]
async fn a_process_cant_change_the_projects_git() {
    require_bwrap!();
    let decoys = Decoys::new("proc-git");
    let manager = decoys.manager(1500);
    output(&manager, "echo hook > .git/hooks/pre-commit", false).await;
    assert!(!decoys.root.join(".git/hooks/pre-commit").exists());
    manager.close_all().await;
}

/// **A background process can't read the real home folder** (but can read the project): it's the stand-in home.
///
/// Proven: removed `--tmpfs <home>` from `sandbox_args` → failed (the decoy secret printed).
#[tokio::test]
async fn a_process_cant_read_the_real_home() {
    require_bwrap!();
    let decoys = Decoys::new("proc-home");
    let manager = decoys.manager(1500);
    let logs =
        output(&manager, &format!("cat {}/secret.txt; cat {}/readme.txt", decoys.home.display(), decoys.root.display()), false)
            .await;
    assert!(!logs.contains("DECOY-HOME-SECRET"), "{logs}");
    assert!(logs.contains("PROJECT-FILE"), "{logs}"); // control
    manager.close_all().await;
}

/// **Without `network: true` a background process can't reach the host's 127.0.0.1** (the user's databases, other
/// dev servers), nor anything else; with it, it can. Its ports are forwarded one way only.
///
/// Proven: launched every process with `network: true` → failed (`HOST-SERVICE` reached).
#[tokio::test]
async fn a_process_cant_reach_the_hosts_localhost_without_network() {
    require_bwrap!();
    let decoys = Decoys::new("proc-net");
    let manager = decoys.manager(1500);
    let host = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = host.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut conn, _)) = host.accept().await {
            let _ = conn.write_all(b"HTTP/1.0 200 OK\r\n\r\nHOST-SERVICE").await;
        }
    });
    let curl = format!("curl -s --max-time 3 http://127.0.0.1:{port}/");
    assert!(!output(&manager, &curl, false).await.contains("HOST-SERVICE"));
    assert!(output(&manager, &curl, true).await.contains("HOST-SERVICE")); // control
    manager.close_all().await;
}

/// **A forwarded port listens on the host's 127.0.0.1 only**, never another interface, even for a server that binds
/// 0.0.0.0 in its sandbox: the user's machine, not their network, reaches it.
///
/// Proven: bound the forwarder to `Ipv4Addr::UNSPECIFIED` in `ProcessManager::forward` → failed (the page answered
/// on the machine's own address).
#[tokio::test]
async fn a_forwarded_port_is_on_the_hosts_127_0_0_1_only() {
    require_bwrap!();
    let decoys = Decoys::new("proc-lo");
    let manager = decoys.manager(1500);
    let port = free_port();
    let started = manager.start(&format!("/run/marv-bin serve 0.0.0.0 PROC {port}"), None, false).await.unwrap();
    assert_eq!(started.info.ports, [port]);
    assert_eq!(get("127.0.0.1", port).await.unwrap(), "PROC");
    match other_address() {
        Some(other) => assert!(get(&other, port).await.is_err(), "reachable on {other}:{port}"),
        None => eprintln!("no other interface here: only the loopback half ran"),
    }
    manager.close_all().await;
}

/// **A background process dies on stop**, with everything it started (its own PID namespace).
///
/// Proven: skipped the kill in `ProcessManager::stop` (the entry forgotten, its sandbox left running) → failed (the
/// marker still ran).
#[tokio::test]
async fn a_process_dies_on_stop() {
    require_bwrap!();
    let decoys = Decoys::new("proc-stop");
    let manager = decoys.manager(300);
    let marker = format!("marv-guard-stop-{}", std::process::id());
    manager.start(&format!("sleep 300 & sleep 301; echo {marker}"), Some("s"), false).await.unwrap();
    assert!(until(|| !pgrep(&marker).is_empty(), Duration::from_secs(5)).await);
    manager.stop("s").await;
    assert_eq!(pgrep(&marker), "");
    manager.close_all().await;
}

/// **A background process dies when Marv is SIGKILLed**: no chance to clean up, so bwrap's `--die-with-parent` (and
/// its PID namespace) must take it down.
///
/// Proven: dropped `--die-with-parent` from `sandbox_args` → failed (the marker survived its parent).
#[tokio::test]
async fn a_process_dies_when_marv_is_sigkilled() {
    require_bwrap!();
    let decoys = Decoys::new("proc-kill");
    let marker = format!("marv-guard-kill-{}", std::process::id());
    let mut parent = tokio::process::Command::new(FIXTURE)
        .args([
            "parent",
            &decoys.root.to_string_lossy(),
            &decoys.runtime.path().to_string_lossy(),
            &format!("sleep 300; echo {marker}"),
        ])
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut line = vec![0u8; 16];
    let n = parent.stdout.as_mut().unwrap().read(&mut line).await.unwrap();
    assert!(String::from_utf8_lossy(&line[..n]).contains("started"));
    assert!(!pgrep(&marker).is_empty());
    parent.start_kill().unwrap();
    parent.wait().await.unwrap();
    assert!(until(|| pgrep(&marker).is_empty(), Duration::from_secs(5)).await);
}

fn door_ctx(decoys: &Decoys, manager: &ProcessManager, recorder: &Arc<Recorder>) -> ToolContext {
    let mut ctx = ToolContext::new(&decoys.root);
    ctx.yolo = true;
    ctx.doors.process = true;
    ctx.processes = Some(Arc::new(manager.clone()) as Arc<dyn ProcessHost>);
    ctx.approver = Some(recorder.clone());
    ctx
}

/// **Starting a background process asks, even in yolo, and a no starts nothing.** It outlives the turn and the user
/// can reach it: a door out of the sandbox.
///
/// Proven: gave `ProcessTool` an `auto_safe` that says yes → failed (nobody asked, the process ran).
#[tokio::test]
async fn a_start_asks_even_in_yolo_and_a_no_starts_nothing() {
    require_bwrap!();
    let decoys = Decoys::new("proc-ask");
    let manager = decoys.manager(300);
    let recorder = Recorder::new(Decision::No);
    let start = call("process", json!({"action": "start", "command": "sleep 100"}));
    let outcome = run_tool(&start, &door_ctx(&decoys, &manager, &recorder), builtin_tools()).await;
    let asked = recorder.requests();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].door, Some(Door::Process));
    assert_eq!(asked[0].scope.key, "process:sleep 100");
    assert!(outcome.result.declined);
    assert!(manager.list().is_empty());
    manager.close_all().await;
}

/// **With `approveSandboxed`, `serve` runs unasked in a person's turn; a `start`, and a `serve` in a woken turn,
/// still ask.** `serve` is Marv's own server with the project read-only and no network: no more than a sandboxed
/// command. A start runs the project's code; a wake is another agent's message, which opens no door (#74).
///
/// Proven: made `ProcessTool::auto_sandboxed` true for every action → failed (the start wasn't asked); dropped it →
/// failed (the person's serve was asked).
#[tokio::test]
async fn approve_sandboxed_serves_a_folder_unasked_but_a_start_still_asks() {
    require_bwrap!();
    let decoys = Decoys::new("proc-serve-unasked");
    std::fs::create_dir_all(decoys.root.join("site")).unwrap();
    std::fs::write(decoys.root.join("site/index.html"), "<p>hi</p>").unwrap();
    let manager = decoys.manager(300);
    let ctx = |recorder: &Arc<Recorder>, mode: marv_tools::CallMode| ToolContext {
        yolo: false,
        approve_sandboxed: true,
        mode,
        ..door_ctx(&decoys, &manager, recorder)
    };
    let serve = call("process", json!({"action": "serve", "path": "site"}));
    let recorder = Recorder::new(Decision::No);
    let outcome = run_tool(&serve, &ctx(&recorder, marv_tools::CallMode::Person), builtin_tools()).await;
    assert!(recorder.requests().is_empty(), "the person's serve wasn't asked: {}", outcome.result.output);
    assert!(!outcome.result.declined);
    manager.close_all().await;

    let recorder = Recorder::new(Decision::No);
    let outcome = run_tool(&serve, &ctx(&recorder, marv_tools::CallMode::Woken), builtin_tools()).await;
    assert_eq!(recorder.requests().len(), 1, "a woken turn's serve asks");
    assert!(outcome.result.declined);

    let recorder = Recorder::new(Decision::No);
    let start = call("process", json!({"action": "start", "command": "sleep 100"}));
    let outcome = run_tool(&start, &ctx(&recorder, marv_tools::CallMode::Person), builtin_tools()).await;
    assert_eq!(recorder.requests().len(), 1, "a start asks");
    assert!(outcome.result.declined && manager.list().is_empty());
    manager.close_all().await;
}

/// **A subagent's context can't start one, even when the parent's could: refused before any ask.** Its process would
/// outlive its report; the door is the context's to open (the main agent's), not the approver's.
///
/// Proven: copied `doors` and `processes` onto the subagent's context in `subagent_context` → failed (asked, and the
/// process started). Since run_tool refuses a main-agent tool in a subagent's context (`marv_tools::floor`), the
/// tool's own refusal is checked by calling it directly; dropping run_tool's check → the first assert fails.
#[tokio::test]
async fn a_subagents_context_cant_start_one() {
    require_bwrap!();
    let decoys = Decoys::new("proc-sub");
    let manager = decoys.manager(300);
    let recorder = Recorder::new(Decision::Yes);
    let parent = door_ctx(&decoys, &manager, &recorder);
    let start = call("process", json!({"action": "start", "command": "sleep 100"}));
    let control = run_tool(&start, &parent, builtin_tools()).await; // the parent can
    assert!(!control.result.is_error, "{}", control.result.output);
    manager.close_all().await;
    let manager = decoys.manager(300);
    let parent = door_ctx(&decoys, &manager, &recorder);
    let sub = marv_session::subagent::subagent_context(&parent, "general-purpose · x", None, parent.approver.clone());
    // Handed the full list, run_tool refuses it first: process is the main agent's alone (`marv_tools::floor`).
    let outcome = run_tool(&start, &sub, builtin_tools()).await;
    assert!(outcome.result.output.starts_with("Unknown tool \"process\""), "{}", outcome.result.output);
    // And the tool refuses it by itself too: a subagent's context has no process door.
    let input: marv_tools::process::ProcessInput =
        serde_json::from_value(json!({"action": "start", "command": "sleep 100"})).unwrap();
    let own = marv_tools::Tool::run(&marv_tools::process::ProcessTool, &input, &sub).await;
    assert_eq!(own.unwrap_err().0, NO_PROCESS_DOOR);
    assert_eq!(recorder.requests().len(), 1); // only the parent's
    assert!(manager.list().is_empty());
    manager.close_all().await;
}

/// What a sandboxed bash command without network gets at 127.0.0.1:`port`, as the bash tool runs it with `forwards`.
async fn fetched_in_sandbox(root: &std::path::Path, forwards: Vec<marv_tools::ProcessForward>, port: u16) -> String {
    let command =
        format!("exec 3<>/dev/tcp/127.0.0.1/{port} && printf 'GET / HTTP/1.0\\r\\n\\r\\n' >&3 && cat <&3 || echo UNREACHABLE");
    let options = marv_tools::bash::RunOptions {
        command,
        root: root.to_path_buf(),
        sandbox: true,
        timeout: Duration::from_secs(10),
        forwards,
        ..Default::default()
    };
    let sandbox = marv_sandbox::Sandbox::from_env(true);
    marv_tools::bash::run_command(options, &sandbox, &tokio_util::sync::CancellationToken::new()).await.output
}

/// **Another chat's process in the same folder is reached without network, and only there**: a team's lead checking
/// the devserver's server asked for `network: true` (Marv OS simulation, 2026-11-08), which also tainted its chat. A
/// chat in another folder still can't reach it.
///
/// Proven: `ProcessManager::forwards` returning only its own manager's ports → failed (UNREACHABLE for the neighbour).
#[tokio::test]
async fn a_sandboxed_command_reaches_a_process_another_chat_in_the_folder_started_and_no_other() {
    require_bwrap!();
    let decoys = Decoys::new("proc-neighbour");
    let (devserver, lead) = (decoys.manager(1500), decoys.manager(1500));
    let elsewhere = Decoys::new("proc-elsewhere");
    let stranger = elsewhere.manager(1500);
    let port = free_port();
    let started = devserver.start(&format!("/run/marv-bin serve 127.0.0.1 TEAMMATE {port}"), None, false).await.unwrap();
    assert_eq!(started.info.ports, [port]);

    let forwards = lead.forwards();
    assert_eq!(forwards.len(), 1, "{forwards:?}");
    assert_ne!(forwards[0].id, "p1", "a neighbour's mount point can't clash with the lead's own p1");
    assert!(fetched_in_sandbox(&decoys.root, forwards, port).await.contains("TEAMMATE"));

    assert!(stranger.forwards().is_empty(), "another folder's chat sees no forward");
    let out = fetched_in_sandbox(&elsewhere.root, stranger.forwards(), port).await;
    assert!(out.contains("UNREACHABLE") && !out.contains("TEAMMATE"), "{out}");

    devserver.close_all().await;
    assert!(lead.forwards().is_empty(), "a stopped process isn't forwarded");
}
