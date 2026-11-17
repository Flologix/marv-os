//! marv-fs: the file helpers marv-mcp and marv-session both need, in a crate below both.
//!
//! - [`private_file`]: `write_private`, 0600 in a 0700 folder, atomic (Marv's `src/private-file.ts`);
//! - [`file_lock`]: `with_file_lock`, one read-change-write of a shared file at a time across Marv processes
//!   (Marv's `src/file-lock.ts`);
//! - [`mode`]: Unix permission bits that are a no-op on Windows;
//! - [`local_socket`]: a socket at a path (Unix sockets; TCP on 127.0.0.1 with a token on Windows).

pub mod file_lock;
pub mod local_socket;
pub mod mode;
pub mod private_file;

pub use file_lock::{LockError, LockOptions, STALE_LOCK, with_file_lock};
pub use private_file::{random_hex, write_private, write_private_under};
