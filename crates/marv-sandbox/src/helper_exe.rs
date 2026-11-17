//! The binary that answers Marv's hidden helper subcommands (`__relay`, `__run-checks`, `__html-convert`): Marv's
//! own (`std::env::current_exe()`), unless the program set another one at start.
//!
//! Why another one: the desktop (`marv-desktop`) links WebKitGTK and GTK. The AppImage ships those libraries next to
//! it (`$ORIGIN/../lib`), and a helper re-exec'd inside the sandbox (`/run/marv-bin`, environment cleared) can't see
//! them, so on a machine without the system's WebKitGTK it died with "error while loading shared libraries" (exit
//! 127). The bundles ship the plain `marv` CLI (no GTK; the same source, the same helpers) at [`BUNDLED_HELPER`]
//! beside the desktop, and the desktop names it here at start. The sandbox is unchanged: the same read-only mount of
//! one binary, only a different binary in it.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Where the bundles put the plain `marv` CLI, relative to the desktop binary's folder (`usr/bin` in both the .deb
/// and the AppImage): `usr/libexec/marv-os/marv`.
#[cfg(not(windows))]
pub const BUNDLED_HELPER: &str = "../libexec/marv-os/marv";
/// Windows: the installer puts `marv.exe` beside `marv-desktop.exe`.
#[cfg(windows)]
pub const BUNDLED_HELPER: &str = "marv.exe";

static HELPER: OnceLock<PathBuf> = OnceLock::new();

/// Names the binary that answers the helper subcommands, instead of this one. The first call wins; call it at start,
/// before any session runs a command.
pub fn set_helper_exe(path: impl Into<PathBuf>) {
    let _ = HELPER.set(path.into());
}

/// The binary that answers the helper subcommands: [`set_helper_exe`]'s, else this one.
pub fn helper_exe() -> Option<PathBuf> {
    HELPER.get().cloned().or_else(|| std::env::current_exe().ok())
}

/// The bundled helper beside the program at `exe` ([`BUNDLED_HELPER`]), by its real path, when it's a file there.
/// None in a build that doesn't ship one (cargo, `tauri dev`): the program then answers its helpers itself.
pub fn bundled_helper(exe: &Path) -> Option<PathBuf> {
    let exe = std::fs::canonicalize(exe).ok()?;
    let helper = std::fs::canonicalize(exe.parent()?.join(BUNDLED_HELPER)).ok()?;
    helper.is_file().then(|| without_verbatim(helper))
}

/// `\\?\C:\…` as `C:\…`. Windows' canonicalize gives the verbatim form, and this path goes into commands other programs
/// run: Claude Code's hooks run through Git Bash, which can't start `\\?\C:\Users\…\marv.exe` ("No such file or
/// directory": every hook failed, so no chat heard its turns end). A UNC path (`\\?\UNC\…`) is left as it is.
/// (marv-session's `standard_windows_path` does the same for folders; this crate sits below it.)
fn without_verbatim(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|p| p.strip_prefix(r"\\?\")) {
        Some(plain) if plain.as_bytes().get(1) == Some(&b':') => PathBuf::from(plain),
        _ => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_helper_is_found_beside_the_program_only() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("usr/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("marv-desktop");
        std::fs::write(&exe, b"").unwrap();
        assert_eq!(bundled_helper(&exe), None);

        let lib = root.path().join("usr/libexec/marv-os");
        std::fs::create_dir_all(&lib).unwrap();
        // A folder by that name isn't a helper.
        std::fs::create_dir(lib.join("marv")).unwrap();
        assert_eq!(bundled_helper(&exe), None);
        std::fs::remove_dir(lib.join("marv")).unwrap();

        std::fs::write(lib.join("marv"), b"").unwrap();
        assert_eq!(bundled_helper(&exe), Some(without_verbatim(std::fs::canonicalize(lib.join("marv")).unwrap())));
        assert_eq!(bundled_helper(&root.path().join("nope")), None);
    }

    /// Never the verbatim form: Git Bash (Claude Code's hooks on Windows) can't start it.
    #[test]
    fn the_helper_path_is_one_other_programs_can_start() {
        assert_eq!(
            without_verbatim(PathBuf::from(r"\\?\C:\Users\me\Marv\marv.exe")),
            PathBuf::from(r"C:\Users\me\Marv\marv.exe")
        );
        assert_eq!(
            without_verbatim(PathBuf::from(r"\\?\UNC\server\share\marv.exe")),
            PathBuf::from(r"\\?\UNC\server\share\marv.exe")
        );
        assert_eq!(without_verbatim(PathBuf::from("/usr/libexec/marv-os/marv")), PathBuf::from("/usr/libexec/marv-os/marv"));
    }

    #[test]
    fn helper_exe_defaults_to_this_binary() {
        // Nothing in this test binary sets one.
        assert_eq!(helper_exe(), std::env::current_exe().ok());
    }
}
