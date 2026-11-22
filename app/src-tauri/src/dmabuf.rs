//! WebKitGTK's DMA-BUF renderer, on unless it can't be (Linux). Without it every frame is copied through the CPU, and
//! a terminal redrawing while text is selected lagged; with it, some Wayland + NVIDIA setups kill the window at start
//! ("Error 71 (Protocol error) dispatching to Wayland display"). Marv runs itself with it, and if that run dies at
//! start that way, once more without it, remembering so for the next launches ([`marker`]).

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How soon after its start a crash counts as the renderer's.
pub const AT_START: Duration = Duration::from_secs(20);

/// The file that says the renderer killed the window here: launches after it start without the renderer.
pub fn marker() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("ai.flologix.marv").join("dmabuf-off"))
}

/// Whether a run's error output says the Wayland display refused the renderer.
pub fn renderer_crash(said: &str) -> bool {
    said.contains("Error 71") || said.contains("Protocol error dispatching to Wayland display")
}

/// Runs `exe` (this program, with its arguments) with the renderer unless the marker says not to; a run with it that
/// dies at start of the renderer's crash runs again without it, and leaves the marker. Its exit code.
pub fn run_with_fallback(exe: &Path) -> i32 {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let off = marker().is_some_and(|m| m.exists());
    let run = |disable: bool, watch: bool| -> (i32, bool) {
        let mut command = Command::new(exe);
        command.args(&args).env("WEBKIT_DISABLE_DMABUF_RENDERER", if disable { "1" } else { "0" });
        run_child(command, watch)
    };
    if off {
        return run(true, false).0;
    }
    let (code, crashed) = run(false, true);
    if !crashed {
        return code;
    }
    if let Some(marker) = marker() {
        let _ = std::fs::create_dir_all(marker.parent().unwrap_or(&marker));
        let _ = std::fs::write(
            &marker,
            "WebKitGTK's DMA-BUF renderer killed the window at start here (Error 71); delete this file to try it again.\n",
        );
    }
    run(true, false).0
}

/// Runs `command` (the app, run again) until it ends, with this process's SIGTERM and SIGINT passed on to it.
/// `watch`: its error output passes through as it comes and is looked at for the renderer's crash. Its exit code, and
/// whether it died at start of that crash.
fn run_child(mut command: Command, watch: bool) -> (i32, bool) {
    let started = Instant::now();
    if watch {
        command.stderr(Stdio::piped());
    }
    let Ok(mut child) = command.spawn() else { return (1, false) };
    #[cfg(unix)]
    forward_signals(child.id());
    let mut crashed = false;
    if let Some(err) = child.stderr.take() {
        let mut out = std::io::stderr();
        for line in BufReader::new(err).lines().map_while(Result::ok) {
            let _ = writeln!(out, "{line}");
            crashed |= renderer_crash(&line);
        }
    }
    let code = child.wait().ok().and_then(|s| s.code()).unwrap_or(1);
    (code, crashed && code != 0 && started.elapsed() < AT_START)
}

/// This process's SIGTERM and SIGINT, passed on to the app it runs (`pid`), which quits on them as on Quit (its agents
/// end). Without this the signal ended only this launcher, and the app and its agents ran on, orphaned: anything that
/// stops Marv OS by the pid it started (a session manager, a script) left it behind (seen 2026-11-17).
#[cfg(unix)]
fn forward_signals(pid: u32) {
    use nix::sys::signal::{Signal, kill};
    use tokio::signal::unix::{SignalKind, signal};
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_io().build() else { return };
    // Taken here, before the launcher returns to waiting: a signal from now on is passed on, not this process's end.
    let signals = {
        let _entered = runtime.enter();
        signal(SignalKind::terminate()).and_then(|term| Ok((term, signal(SignalKind::interrupt())?)))
    };
    let Ok((mut term, mut interrupt)) = signals else { return };
    let pid = nix::unistd::Pid::from_raw(pid as i32);
    std::thread::spawn(move || {
        runtime.block_on(async move {
            loop {
                let got = tokio::select! {
                    Some(()) = term.recv() => Signal::SIGTERM,
                    Some(()) = interrupt.recv() => Signal::SIGINT,
                    else => return,
                };
                let _ = kill(pid, got);
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_wayland_protocol_crash_turns_the_renderer_off() {
        assert!(renderer_crash("Gdk-Message: Error 71 (Protocol error) dispatching to Wayland display."));
        assert!(!renderer_crash("Gtk-WARNING: Theme parsing error"));
    }

    /// SIGTERM to the launcher reaches the app it runs, and the launcher waits for it to end: before, the launcher
    /// died and the app ran on. Run as its own process (this test binary again, [`launcher_for_test`]): a SIGTERM to
    /// the test runner would end every test.
    #[cfg(unix)]
    #[test]
    fn sigterm_to_the_launcher_reaches_the_app() {
        let dir = tempfile::tempdir().unwrap();
        let said = dir.path().join("said");
        let mut launcher = Command::new(std::env::current_exe().unwrap())
            .args(["dmabuf::tests::launcher_for_test", "--exact", "--ignored", "--nocapture", "--test-threads=1"])
            .env("MARV_TEST_LAUNCHER_SAID", &said)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let started = Instant::now();
        while !std::fs::read_to_string(&said).is_ok_and(|s| s.contains("ready")) && started.elapsed().as_secs() < 10 {
            std::thread::sleep(Duration::from_millis(20));
        }
        let ready = std::fs::read_to_string(&said).unwrap_or_default().contains("ready");
        let mut ended = None;
        if ready {
            let pid = nix::unistd::Pid::from_raw(launcher.id() as i32);
            let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM);
            let start = Instant::now();
            while ended.is_none() && start.elapsed().as_secs() < 10 {
                ended = launcher.try_wait().ok().flatten();
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        if ended.is_none() {
            let _ = launcher.kill();
        }
        let _ = launcher.wait();
        let said = std::fs::read_to_string(&said).unwrap_or_default();
        // An app left running (the signal not passed on) is ended here, not left looping after the test.
        if let Some(app) = said.split_whitespace().skip_while(|w| *w != "ready").nth(1).and_then(|p| p.parse::<i32>().ok()) {
            let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(app), nix::sys::signal::Signal::SIGKILL);
        }
        assert!(ready, "the app started");
        assert!(said.contains("quit"), "the app got the SIGTERM: {said}");
        assert!(ended.is_some_and(|s| s.success()), "the launcher waited for the app and gave its exit code: {ended:?}");
    }

    /// The launcher side of the test above, run only by it: an "app" that says it's ready and quits on SIGTERM.
    #[cfg(unix)]
    #[test]
    #[ignore = "run by sigterm_to_the_launcher_reaches_the_app"]
    fn launcher_for_test() {
        let Some(said) = std::env::var_os("MARV_TEST_LAUNCHER_SAID") else { return };
        let mut app = Command::new("sh");
        app.args(["-c", "trap 'echo quit >> \"$0\"; exit 0' TERM; echo ready $$ >> \"$0\"; while :; do sleep 0.1; done"])
            .arg(&said);
        std::process::exit(run_child(app, false).0);
    }
}
