//! #69: `marv kernel about | head -1` printed its first line, then panicked ("failed printing to stdout: Broken
//! pipe"). Each `marv kernel` command that prints is run with a stdout whose reader has already closed: it must exit 0
//! with nothing on stderr. The config folder is an empty one of the test's own, so each prints its "nothing yet".

use std::process::Command;

#[test]
fn kernel_commands_end_quietly_when_their_reader_closed() {
    let config = tempfile::tempdir().unwrap();
    let log = config.path().join("empty.jsonl");
    std::fs::write(&log, "").unwrap();
    let log = log.to_str().unwrap();
    for args in [
        &["kernel", "about"][..],
        &["kernel", "grants"],
        &["kernel", "usage"],
        &["kernel", "log", log],
        &["kernel", "replay", log],
    ] {
        let (reader, writer) = std::io::pipe().unwrap();
        // Every write now fails with EPIPE, however soon marv gets to it.
        drop(reader);
        let out = Command::new(env!("CARGO_BIN_EXE_marv"))
            .args(args)
            .env("MARV_CONFIG_DIR", config.path())
            .env("HOME", config.path())
            .stdout(writer)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "marv {}: {:?}, stderr: {stderr}", args.join(" "), out.status);
        assert!(stderr.is_empty(), "marv {}: {stderr}", args.join(" "));
    }
}
