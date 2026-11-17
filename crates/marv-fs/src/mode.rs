//! Unix permission bits on what Marv creates, written once for every platform.
//!
//! On Unix these set the mode (0600 files, 0700 folders). On Windows there are no mode bits: a file under the user's
//! profile (`%USERPROFILE%\.marv`) already inherits an ACL that only that user, SYSTEM and Administrators can read, so
//! the calls do nothing and the code that uses them stays one path.

use std::fs::{DirBuilder, OpenOptions, Permissions};
use std::io;
use std::path::Path;

/// `.mode(0o600)` on an [`OpenOptions`] or [`DirBuilder`]: the mode on Unix, nothing on Windows.
pub trait CreateMode {
    fn create_mode(&mut self, mode: u32) -> &mut Self;
}

impl CreateMode for OpenOptions {
    #[cfg(unix)]
    fn create_mode(&mut self, mode: u32) -> &mut Self {
        std::os::unix::fs::OpenOptionsExt::mode(self, mode)
    }
    #[cfg(not(unix))]
    fn create_mode(&mut self, _mode: u32) -> &mut Self {
        self
    }
}

impl CreateMode for DirBuilder {
    #[cfg(unix)]
    fn create_mode(&mut self, mode: u32) -> &mut Self {
        std::os::unix::fs::DirBuilderExt::mode(self, mode)
    }
    #[cfg(not(unix))]
    fn create_mode(&mut self, _mode: u32) -> &mut Self {
        self
    }
}

/// `chmod`: the mode on Unix, nothing on Windows.
pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, Permissions::from_mode(mode))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

/// A file's permission bits (`mode & 0o777`); on Windows 0o600, or 0o400 when read-only.
pub fn mode_of(permissions: &Permissions) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.mode() & 0o777
    }
    #[cfg(not(unix))]
    {
        if permissions.readonly() { 0o400 } else { 0o600 }
    }
}

/// What says a file changed, for caches that re-read a file only then: on Unix its inode, mode, size and both change
/// times (to the nanosecond); on Windows its size, modification time and read-only bit (no inode or ctime there).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    id: u64,
    mode: u32,
    len: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

pub fn stamp(meta: &std::fs::Metadata) -> FileStamp {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        FileStamp {
            id: meta.ino(),
            mode: meta.mode(),
            len: meta.len(),
            modified: (meta.mtime(), meta.mtime_nsec()),
            changed: (meta.ctime(), meta.ctime_nsec()),
        }
    }
    #[cfg(not(unix))]
    {
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or((0, 0), |d| (d.as_secs() as i64, i64::from(d.subsec_nanos())));
        FileStamp { id: 0, mode: mode_of(&meta.permissions()), len: meta.len(), modified, changed: modified }
    }
}

/// Two secrets compared in constant time (for equal lengths; a length is no secret).
pub fn same_secret(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

/// Opening never follows a symlink at the last part of the path: `O_NOFOLLOW` on Unix; on Windows the reparse point
/// itself is opened (`FILE_FLAG_OPEN_REPARSE_POINT`), not what it leads to.
pub trait NoFollow {
    fn no_follow(&mut self) -> &mut Self;
}

impl NoFollow for OpenOptions {
    #[cfg(unix)]
    fn no_follow(&mut self) -> &mut Self {
        std::os::unix::fs::OpenOptionsExt::custom_flags(self, nix::libc::O_NOFOLLOW)
    }
    #[cfg(windows)]
    fn no_follow(&mut self) -> &mut Self {
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        std::os::windows::fs::OpenOptionsExt::custom_flags(self, FILE_FLAG_OPEN_REPARSE_POINT)
    }
}

/// Whether two metadata are of one file: device and inode on Unix; on Windows, where std doesn't expose the file's
/// index, the same size and modification time (enough for "was it replaced meanwhile").
pub fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        a.dev() == b.dev() && a.ino() == b.ino()
    }
    #[cfg(not(unix))]
    {
        a.len() == b.len() && a.modified().ok() == b.modified().ok()
    }
}

/// This process's effective uid; 0 on Windows, where [`uid_of`] is 0 too (a file under the user's profile is theirs).
pub fn own_uid() -> u32 {
    #[cfg(unix)]
    return nix::unistd::geteuid().as_raw();
    #[cfg(not(unix))]
    0
}

/// The file's owner (`st_uid`); 0 on Windows (see [`own_uid`]).
pub fn uid_of(meta: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    return std::os::unix::fs::MetadataExt::uid(meta);
    #[cfg(not(unix))]
    {
        let _ = meta;
        0
    }
}
