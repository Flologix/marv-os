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
        if !watch {
            return (command.status().ok().and_then(|s| s.code()).unwrap_or(1), false);
        }
        let started = Instant::now();
        let Ok(mut child) = command.stderr(Stdio::piped()).spawn() else { return (1, false) };
        // Its error output passes through as it comes, and is looked at for the renderer's crash.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_wayland_protocol_crash_turns_the_renderer_off() {
        assert!(renderer_crash("Gdk-Message: Error 71 (Protocol error) dispatching to Wayland display."));
        assert!(!renderer_crash("Gtk-WARNING: Theme parsing error"));
    }
}
