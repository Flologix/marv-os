//! mk-60m.10: the built `marv-desktop` answers Marv's hidden helper modes (`marv_session::helper_modes`) itself, before
//! the WebKit re-exec and before Tauri starts. The process tool, the project checks and the web reader run
//! `current_exe()` as their helper; in the desktop that's this binary, and it used to start the app inside the
//! sandbox ("Failed to initialize gtk backend", code 101). Run with no display, so a GUI start fails loudly.

use std::io::Write;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn desktop(args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_marv-desktop"))
        .args(args)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("WEBKIT_DISABLE_DMABUF_RENDERER")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("marv-desktop starts");
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            panic!("marv-desktop {args:?} didn't end: did the app start?");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("gtk") && !stderr.contains("panicked"), "marv-desktop {args:?} started the app: {stderr}");
    out
}

#[test]
fn answers_relay_with_the_commands_exit_code_and_output() {
    let out = desktop(&["__relay", r#"{"argv":["sh","-c","echo relayed; exit 7"]}"#], "");
    assert_eq!(out.status.code(), Some(7), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "relayed\n");
}

#[test]
fn answers_html_convert() {
    let out = desktop(&["__html-convert"], r#"{"html":"<h1>Hello</h1><p>desktop</p>","url":"https://example.com/"}"#);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(r#""ok":true"#) && stdout.contains("Hello"), "{stdout}");
}

#[test]
fn answers_run_checks() {
    // No checks in an empty folder: the runner's empty report.
    let dir = std::env::temp_dir().join(format!("marv-desktop-checks-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_marv-desktop"))
        .args(["__run-checks", r#"{"checks":[],"timeoutMs":1000}"#])
        .current_dir(&dir)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{out:?}");
}

#[test]
fn a_malformed_helper_exits_127_without_starting_the_app() {
    assert_eq!(desktop(&["__relay"], "").status.code(), Some(127));
    assert_eq!(desktop(&["__relay", "not json"], "").status.code(), Some(127));
    assert_eq!(desktop(&["__guard", "1", "bash"], "").status.code(), Some(127));
}

/// The process tool's own path, end to end: a `ProcessManager` whose relay is this binary (as `current_exe()` is in
/// the desktop) starts a command in its bubblewrap sandbox, and the command runs (it used to exit 101 at once).
#[tokio::test]
async fn the_process_tool_runs_commands_through_the_desktop_binary() {
    let bwrap = Command::new("bwrap")
        .args(["--ro-bind", "/", "/", "--unshare-net", "--unshare-pid", "--", "true"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !bwrap {
        eprintln!("skipped: bubblewrap isn't available here");
        return;
    }
    marv_tools::relay::set_relay_exe(env!("CARGO_BIN_EXE_marv-desktop"));
    let base = std::env::temp_dir().join(format!("marv-desktop-proc-{}", std::process::id()));
    let (root, home, runtime) = (base.join("root"), base.join("home"), base.join("rt"));
    for dir in [&root, &home, &runtime] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let mut options = marv_session::processes::ProcessManagerOptions::new(&root);
    options.home = Some(home);
    options.runtime_dir = Some(runtime);
    options.poll = Duration::from_millis(100);
    options.start_wait = Duration::from_secs(5);
    let manager = marv_session::processes::ProcessManager::new(options);
    let started = manager.start("type -a sh && echo desktop-relay-ok", Some("probe"), false).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let info = loop {
        let info = manager.list().into_iter().find(|p| p.name == "probe").unwrap();
        if info.exit_code.is_some() || Instant::now() > deadline {
            break info;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let logs = manager.logs("probe", 50).unwrap();
    manager.close_all().await;
    let _ = std::fs::remove_dir_all(&base);
    assert_eq!(info.exit_code, Some(0), "{started:?}\n{logs}");
    assert!(logs.contains("desktop-relay-ok") && !logs.contains("gtk"), "{logs}");
}
