//! Display paths, the home-folder guard and per-project keys. Ports Marv's `src/paths.ts`.
//!
//! - [`project_key`]: per-project files (sessions, project memory, trajectories, MCP trust, worktrees) live under
//!   `<readable path>-<10-char hash of the full path>`. The readable part alone collided (`/a-b/c` vs `/a/b-c`, any
//!   two non-ASCII names of equal length), which made projects share memory, sessions and trusted MCP servers.
//! - [`legacy_project_key`]: the old key, still searched (sessions check their recorded root; memory is moved).
//! - [`home_or_above`]: a project there would put every dotfile within reach of yolo's unasked edits (the SDK refuses
//!   yolo there). Compared as real paths, so a symlink to either is caught too.
//! - [`real_folder`] / [`normalize_folder`] / [`standard_folder`]: one folder, one string. On Windows `canonicalize`
//!   answers with a verbatim `\\?\C:\...` path, which cmd.exe can't start in and which never equals the `C:\...` the
//!   person picked; a drive typed `c:` or a folder typed in another case is the same folder too. Chats are matched to
//!   folders, and lazygits keyed, by these strings, so the same folder must always read the same (herdr's
//!   `normalize_cwd_for_launch_platform` does the same). Elsewhere they change nothing.
//!
//! Port note: Marv's legacy key replaced each UTF-16 unit; here each `char` becomes `len_utf16()` dashes, so keys
//! are identical, astral characters included.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The readable part keeps at most this many characters (the end, which names the project).
pub const MAX_SLUG: usize = 64;

/// The user's home folder (`os.homedir()`).
pub fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

pub use marv_core::process::{on_path, program_in};

/// An agent's program: on the `PATH`, else in one of `home_dirs` (folders below the home where its installers put
/// it: `.local/bin`, mise's shims), else on Windows where npm and Scoop put global programs (`%APPDATA%\npm`,
/// `~\scoop\shims`), which a desktop app started from the Start menu may not have on its `PATH`.
pub fn find_program(name: &str, home_dirs: &[&str]) -> Option<PathBuf> {
    if let Some(found) = on_path(name) {
        return Some(found);
    }
    let home = home_dir();
    let mut dirs: Vec<PathBuf> = home_dirs.iter().map(|d| home.join(d)).collect();
    dirs.extend(toolchain_dirs(&home));
    if cfg!(windows) {
        dirs.extend(std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("npm")));
        dirs.push(home.join("scoop").join("shims"));
        // Where winget links the programs it installs (Install in Marv OS's agent picker uses it). It joins the PATH
        // only of programs started after the install, so a running Marv OS looks here itself.
        dirs.extend(std::env::var_os("LOCALAPPDATA").map(|a| PathBuf::from(a).join("Microsoft").join("WinGet").join("Links")));
        // fnm's default Node (npm's global programs) and mise's shims, which a Start-menu launch may not have on its PATH.
        dirs.extend(std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("fnm").join("aliases").join("default")));
        dirs.extend(std::env::var_os("LOCALAPPDATA").map(|a| PathBuf::from(a).join("mise").join("shims")));
    }
    dirs.iter().find_map(|d| program_in(d, name))
}

/// Where Node, Rust and Homebrew put programs, after the agent's own folders: an app started from the macOS Finder
/// or Dock gets `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, so an agent installed by Homebrew or npm wasn't found. Nothing on
/// Windows (its folders are in [`find_program`]). Missing folders are harmless: nothing is found in them.
fn toolchain_dirs(home: &Path) -> Vec<PathBuf> {
    toolchain_dirs_for(home, cfg!(target_os = "macos"), cfg!(unix))
}

/// [`toolchain_dirs`] for a platform given as flags, so each is tested everywhere.
fn toolchain_dirs_for(home: &Path, macos: bool, unix: bool) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if macos {
        dirs.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    }
    if unix {
        dirs.extend([".volta/bin", ".cargo/bin"].map(|d| home.join(d)));
        let fnm =
            if macos { "Library/Application Support/fnm/aliases/default/bin" } else { ".local/share/fnm/aliases/default/bin" };
        dirs.push(home.join(fnm));
        dirs.extend(newest_nvm_bin(&home.join(".nvm/versions/node")));
    }
    dirs
}

/// `<versions>/<newest>/bin` of nvm's installed Node versions (`v20.11.1`, `v18.19.0`…): the highest version number,
/// compared part by part (`v20.9.0` is older than `v20.11.1`). `None` without any.
fn newest_nvm_bin(versions: &Path) -> Option<PathBuf> {
    let parse = |name: &str| -> Option<Vec<u64>> { name.strip_prefix('v')?.split('.').map(|p| p.parse().ok()).collect() };
    std::fs::read_dir(versions)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| Some((parse(&e.file_name().to_string_lossy())?, e.path())))
        .filter(|(_, path)| path.is_dir())
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, path)| path.join("bin"))
}

/// `/home/me/projects/x` → `~/projects/x`, for display only.
pub fn shorten_home(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        // One kind of slash: `~\Desktop\Projects` on Windows (it said `~/Desktop\Projects`).
        Ok(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// Whether `folder` is the home folder or a folder above it (`/home`, `/`), compared as real paths.
pub fn home_or_above(folder: &Path, home: &Path) -> bool {
    // A path that doesn't exist (a home folder that was never made) is compared as given.
    let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let target = real(folder);
    let h = real(home);
    target == h || target == Path::new("/") || h.starts_with(&target)
}

/// `path` as Windows programs and people write it: a verbatim prefix dropped (`\\?\C:\x` → `C:\x`,
/// `\\?\UNC\server\share` → `\\server\share`) and the drive letter upper-cased. Pure string work, so it runs (and is
/// tested) everywhere; [`standard_folder`] applies it on Windows only.
pub fn standard_windows_path(path: &str) -> String {
    let path = match path.strip_prefix(r"\\?\UNC\") {
        Some(unc) => format!(r"\\{unc}"),
        None => path.strip_prefix(r"\\?\").unwrap_or(path).to_string(),
    };
    let mut chars = path.chars();
    match (chars.next(), chars.next()) {
        (Some(drive), Some(':')) if drive.is_ascii_alphabetic() => format!("{}{}", drive.to_ascii_uppercase(), &path[1..]),
        _ => path,
    }
}

/// A folder's path as stored (a chat's folder, a space's root) in its one standard form: on Windows
/// [`standard_windows_path`], elsewhere unchanged. No disk access, so strings saved before can be read back through it
/// on every load.
pub fn standard_folder(path: &str) -> String {
    if cfg!(windows) { standard_windows_path(path) } else { path.to_string() }
}

/// `path`'s real path (links resolved, as `std::fs::canonicalize`) in the standard form: on Windows without the
/// verbatim prefix `canonicalize` adds, with each folder's casing as it is on disk (canonicalize reports that) and an
/// upper-case drive letter. Elsewhere exactly `canonicalize`.
pub fn real_folder(path: &Path) -> std::io::Result<PathBuf> {
    let real = std::fs::canonicalize(path)?;
    Ok(if cfg!(windows) { PathBuf::from(standard_windows_path(&real.to_string_lossy())) } else { real })
}

/// A folder about to be stored or used as a key (a chat's folder, a lazygit's): on Windows its [`real_folder`] when it
/// exists, else [`standard_folder`]; elsewhere unchanged (Linux and macOS keep the path as given, as before).
pub fn normalize_folder(path: &Path) -> PathBuf {
    if !cfg!(windows) {
        return path.to_path_buf();
    }
    real_folder(path).unwrap_or_else(|_| PathBuf::from(standard_folder(&path.to_string_lossy())))
}

/// The key before it had a hash. Not unique: never use it to decide anything.
pub fn legacy_project_key(root: &Path) -> String {
    root.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_string() } else { "-".repeat(c.len_utf16()) })
        .collect()
}

/// `/home/me/proj` → `-home-me-proj-1a2b3c4d5e`.
pub fn project_key(root: &Path) -> String {
    let digest = Sha256::digest(root.as_os_str().as_encoded_bytes());
    let hash: String = digest.iter().map(|b| format!("{b:02x}")).collect::<String>()[..10].to_string();
    let legacy = legacy_project_key(root);
    let slug = &legacy[legacy.len().saturating_sub(MAX_SLUG)..]; // ASCII only by now
    format!("{slug}-{hash}")
}

#[cfg(test)]
mod tests {
    /// nvm's newest Node wins, by number (not by name: `v9` sorts after `v20` as text), and a stray entry is skipped.
    #[test]
    fn the_newest_nvm_node_is_picked_by_version_number() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(newest_nvm_bin(dir.path()), None);
        assert_eq!(newest_nvm_bin(&dir.path().join("missing")), None);
        for v in ["v9.11.2", "v20.9.0", "v20.11.1", "v18.19.0"] {
            std::fs::create_dir_all(dir.path().join(v).join("bin")).unwrap();
        }
        std::fs::create_dir_all(dir.path().join("system")).unwrap();
        std::fs::write(dir.path().join("v99.0.0"), "a file, not an install").unwrap();
        assert_eq!(newest_nvm_bin(dir.path()), Some(dir.path().join("v20.11.1").join("bin")));
    }

    #[test]
    fn toolchain_folders_follow_the_platform() {
        let home = Path::new("/h");
        let mac = toolchain_dirs_for(home, true, true);
        assert_eq!(&mac[..2], [PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin")]);
        assert!(mac.contains(&home.join("Library/Application Support/fnm/aliases/default/bin")));
        assert!(mac.contains(&home.join(".volta/bin")) && mac.contains(&home.join(".cargo/bin")));
        let linux = toolchain_dirs_for(home, false, true);
        assert!(linux.contains(&home.join(".local/share/fnm/aliases/default/bin")));
        assert!(!linux.contains(&PathBuf::from("/opt/homebrew/bin")));
        assert!(toolchain_dirs_for(home, false, false).is_empty());
    }

    /// An agent's program is found as the shell finds it: on Windows with the extension it runs (`claude.exe`, npm's
    /// `claude.cmd`), never npm's bare shell script; elsewhere the file itself.
    #[test]
    fn a_program_is_found_with_its_windows_extension() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("claude"), "#!/bin/sh").unwrap();
        if cfg!(windows) {
            assert_eq!(program_in(dir.path(), "claude"), None, "npm's sh script doesn't run on Windows");
            std::fs::write(dir.path().join("claude.cmd"), "@echo off").unwrap();
            assert_eq!(program_in(dir.path(), "claude"), Some(dir.path().join("claude.cmd")));
        } else {
            assert_eq!(program_in(dir.path(), "claude"), Some(dir.path().join("claude")));
            assert_eq!(program_in(dir.path(), "opencode"), None);
        }
    }

    use super::*;

    #[test]
    fn different_folders_never_share_a_project_key() {
        let pairs = [
            ("/home/me/a-b/c", "/home/me/a/b-c"),
            ("/home/me/my_app", "/home/me/my-app"),
            ("/home/me/проект", "/home/me/работа"),
            ("/home/me/项目一", "/home/me/客户端"),
        ];
        for (a, b) in pairs {
            assert_eq!(legacy_project_key(Path::new(a)), legacy_project_key(Path::new(b)));
            assert_ne!(project_key(Path::new(a)), project_key(Path::new(b)));
        }
    }

    #[test]
    fn a_key_is_stable_readable_and_safe_as_a_file_name() {
        let key = project_key(Path::new("/home/me/Projects/marv"));
        assert_eq!(key, project_key(Path::new("/home/me/Projects/marv")));
        let re = regex::Regex::new(r"^-home-me-Projects-marv-[0-9a-f]{10}$").unwrap();
        assert!(re.is_match(&key), "{key}");
        let long = project_key(Path::new(&format!("/home/me/{}project", "deep/".repeat(60))));
        assert!(long.len() <= 80);
        assert!(regex::Regex::new(r"project-[0-9a-f]{10}$").unwrap().is_match(&long));
    }

    /// The same Windows folder reads the same however it came: verbatim from `canonicalize`, or with a lower-case drive.
    #[test]
    fn a_windows_path_has_one_standard_form() {
        assert_eq!(standard_windows_path(r"\\?\C:\Users\me\Projects"), r"C:\Users\me\Projects");
        assert_eq!(standard_windows_path(r"c:\Users\me"), r"C:\Users\me");
        assert_eq!(standard_windows_path(r"\\?\d:\x"), r"D:\x");
        assert_eq!(standard_windows_path(r"\\?\UNC\server\share\p"), r"\\server\share\p");
        assert_eq!(standard_windows_path(r"\\server\share"), r"\\server\share");
        assert_eq!(standard_windows_path("C:"), "C:");
        assert_eq!(standard_windows_path("/home/me/c:x"), "/home/me/c:x");
        assert_eq!(standard_windows_path("é:"), "é:");
    }

    /// Linux and macOS folders are kept exactly as given.
    #[cfg(not(windows))]
    #[test]
    fn other_platforms_keep_folders_as_given() {
        assert_eq!(standard_folder(r"c:\x"), r"c:\x");
        assert_eq!(normalize_folder(Path::new("/nonexistent/Proj")), Path::new("/nonexistent/Proj"));
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(real_folder(dir.path()).unwrap(), std::fs::canonicalize(dir.path()).unwrap());
    }

    #[test]
    fn home_is_shortened_for_display() {
        let home = Path::new("/home/me");
        assert_eq!(shorten_home(Path::new("/home/me/x/y"), home), "~/x/y");
        assert_eq!(shorten_home(home, home), "~");
        assert_eq!(shorten_home(Path::new("/home/meme"), home), "/home/meme");
    }

    #[test]
    fn home_and_above_are_refused() {
        let home = Path::new("/nonexistent-home/me");
        assert!(home_or_above(home, home));
        assert!(home_or_above(Path::new("/nonexistent-home"), home));
        assert!(home_or_above(Path::new("/"), home));
        assert!(!home_or_above(Path::new("/nonexistent-home/me/project"), home));
    }
}
