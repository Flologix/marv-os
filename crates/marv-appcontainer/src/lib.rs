//! Marv's sandbox on Windows: a command run in an AppContainer.
//!
//! An AppContainer process starts with almost no rights of its own: it can't read the user's folders, other programs'
//! settings or the network, only what is granted to its container's SID. Each sandboxed run gets a [`Spec`]: the
//! container's name (one per project), the folders it may read or write, the paths inside them it may not touch, and
//! whether it may use the network. [`run`] (the `__appcontainer` helper) makes or opens the container, writes the
//! rules it doesn't have yet, starts the command inside, waits, and removes the rules meant for that run only.
//!
//! **Why rules on the files themselves.** Windows has no mount namespace: what an AppContainer may open is decided
//! by each file's access list. A grant on a folder reaches everything below it, so a path inside that must stay
//! hidden (a protected file, `.git`'s hooks, a credentials folder) gets an explicit deny for the container's SID,
//! which Windows checks before any allow. Rules name only this container's SID, so they never touch the person's own
//! access. Windows copies a folder's rule onto every file below it (seconds in a big project), so the rules that are
//! the same at every run ([`Grant::lasting`]: the project, the tools, what's always hidden) are written once per
//! container and listed in its record; the rest is written and removed around each run.
//!
//! **Why a launcher step.** portable-pty starts an agent's terminal with an attribute list holding only its
//! pseudoconsole, so the AppContainer attribute can't be added there. The terminal starts `marv __appcontainer …`
//! instead, which starts the command with the AppContainer attribute and lets it inherit the same console.
//!
//! **Unsafe.** This crate is the one place Marv calls the Win32 security APIs, which have no safe wrappers here. It
//! allows the `unsafe` lint for itself (an exception the person approved, 2026-11-16, kept the only crate-level one by
//! `marv-cli`'s guard test), and every unsafe block has a `SAFETY:` comment saying why its call is sound.

#![allow(unsafe_code)]

mod spec;
#[cfg(windows)]
mod win;

pub use spec::{Access, Grant, Spec, command_line, container_name, env_block, read_record, to_write};

/// The hidden helper's name: `marv __appcontainer <spec-file> -- <command…>`.
pub const SUBCOMMAND: &str = "__appcontainer";

/// Runs `command` in the AppContainer `spec` describes, and returns its exit code. The rules written for it are
/// removed again when it ends.
#[cfg(windows)]
pub fn run(spec: &Spec, command: &[std::ffi::OsString]) -> Result<u32, String> {
    win::run(spec, command)
}

/// AppContainers are Windows-only.
#[cfg(not(windows))]
pub fn run(_spec: &Spec, _command: &[std::ffi::OsString]) -> Result<u32, String> {
    Err("AppContainers exist only on Windows.".into())
}

/// Whether AppContainer runs are switched on here: Windows, with `MARV_APPCONTAINER=1`, until the backend has been
/// tried on a real PC. Off, Windows keeps refusing sandboxed commands as before.
pub fn enabled() -> bool {
    cfg!(windows) && std::env::var("MARV_APPCONTAINER").is_ok_and(|v| v == "1")
}
