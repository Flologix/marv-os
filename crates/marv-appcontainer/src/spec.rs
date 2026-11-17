//! What a run is allowed ([`Spec`]), and the two strings Windows wants for a process: its command line and its
//! environment block. Plain Rust, so it's tested on every platform.

use std::ffi::OsString;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One AppContainer run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spec {
    /// The container's name: one per project and mode (read-write, or read-only for Marv's checks), so its SID and
    /// lasting rules are the same at every run.
    pub profile: String,
    /// Where the lasting rules already written for this container are listed, so a run writes only what's new.
    pub record: Option<PathBuf>,
    /// What it may reach, in order: grants first, then the denials inside them.
    pub grants: Vec<Grant>,
    /// Internet and local network (the `internetClient` and `privateNetworkClientServer` capabilities).
    pub network: bool,
    /// Where the command starts.
    pub cwd: PathBuf,
    /// Its whole environment: nothing is inherited from Marv.
    pub env: Vec<(String, String)>,
}

/// A rule on one path for the container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub path: PathBuf,
    pub access: Access,
    /// Kept after the run: the project, the tools, and what is always hidden or protected. Windows copies a folder's
    /// rule onto every file below it, which takes seconds in a big project, so these are written once per container.
    /// The rest (an approved command's extra folders, git's write guard) is written for one run and removed after it.
    pub lasting: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Access {
    /// Read and run (programs, the toolchain, read-only project parts).
    Read,
    /// Read, run, write and delete (the project, a scratch folder).
    ReadWrite,
    /// No writes, deletes or permission changes; reading still works (`.git`'s hooks and config, protected files).
    DenyWrite,
    /// Nothing at all (credentials, Marv's own settings).
    DenyAll,
}

impl Access {
    pub fn is_deny(self) -> bool {
        matches!(self, Access::DenyWrite | Access::DenyAll)
    }
}

impl Spec {
    /// The grants with every denial after every allow: Windows checks denials first whatever the order, but writing
    /// them last means a folder's grant never lands on top of a path inside it that was already denied.
    pub fn ordered(&self) -> Vec<&Grant> {
        let (deny, allow): (Vec<_>, Vec<_>) = self.grants.iter().partition(|g| g.access.is_deny());
        allow.into_iter().chain(deny).collect()
    }
}

/// The lasting rules a record lists (an unreadable or missing record lists none: they're all written again).
pub fn read_record(text: Option<&str>) -> Vec<Grant> {
    text.and_then(|t| serde_json::from_str(t).ok()).unwrap_or_default()
}

/// What a run writes and what it leaves: the grants still to write (lasting ones a record doesn't list, and every
/// one-run grant), in [`Spec::ordered`] order.
pub fn to_write<'a>(spec: &'a Spec, recorded: &[Grant]) -> Vec<&'a Grant> {
    spec.ordered().into_iter().filter(|g| !(g.lasting && recorded.contains(g))).collect()
}

/// This run's container name: `profile`, kept to the 64 characters (letters, digits, `.` and `-`) Windows allows.
pub fn container_name(profile: &str) -> String {
    profile.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-').take(64).collect()
}

/// `argv` as one Windows command line, quoted the way `CommandLineToArgvW` (and the C runtime) splits it back: an
/// argument with a space, tab or quote (or an empty one) goes in quotes, a quote inside is `\"`, and backslashes are
/// doubled only where they come before a quote.
pub fn command_line(argv: &[OsString]) -> String {
    argv.iter().map(|a| quote(&a.to_string_lossy())).collect::<Vec<_>>().join(" ")
}

fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\x0b', '"']) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    // Before the closing quote, every backslash is doubled.
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('"');
    out
}

/// `env` as a Windows environment block: `NAME=value\0` for each, sorted by name without regard to case (as Windows
/// keeps them), then one more `\0`.
pub fn env_block(env: &[(String, String)]) -> Vec<u16> {
    let mut sorted: Vec<&(String, String)> = env.iter().collect();
    sorted.sort_by_key(|(k, _)| k.to_uppercase());
    let mut block: Vec<u16> = Vec::new();
    for (k, v) in sorted {
        block.extend(format!("{k}={v}").encode_utf16());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(args: &[&str]) -> String {
        command_line(&args.iter().map(OsString::from).collect::<Vec<_>>())
    }

    /// The quoting `CommandLineToArgvW` undoes: what a Windows program gets back is the argument Marv gave.
    #[test]
    fn arguments_are_quoted_as_windows_splits_them() {
        assert_eq!(line(&["claude", "--resume", "abc"]), "claude --resume abc");
        assert_eq!(
            line(&[r"C:\Program Files\Git\bin\bash.exe", "-c", "echo hi"]),
            r#""C:\Program Files\Git\bin\bash.exe" -c "echo hi""#
        );
        assert_eq!(line(&[""]), r#""""#);
        assert_eq!(line(&[r#"say "hi""#]), r#""say \"hi\"""#);
        // Backslashes double only before a quote, and before the closing quote.
        assert_eq!(line(&[r"C:\dir\"]), r"C:\dir\");
        assert_eq!(line(&[r"C:\my dir\"]), r#""C:\my dir\\""#);
        assert_eq!(line(&[r#"a\"b c"#]), r#""a\\\"b c""#);
    }

    #[test]
    fn the_environment_block_is_sorted_and_ends_twice() {
        let block = env_block(&[("path".into(), "x".into()), ("HOME".into(), "h".into())]);
        let text = String::from_utf16(&block).unwrap();
        assert_eq!(text, "HOME=h\0path=x\0\0");
        assert_eq!(String::from_utf16(&env_block(&[])).unwrap(), "\0\0");
    }

    #[test]
    fn denials_are_written_after_every_grant() {
        let g = |p: &str, access| Grant { path: p.into(), access, lasting: true };
        let spec = Spec {
            profile: "p".into(),
            record: None,
            grants: vec![g("a/.git/hooks", Access::DenyWrite), g("a", Access::ReadWrite), g("tools", Access::Read)],
            network: false,
            cwd: "a".into(),
            env: Vec::new(),
        };
        let order: Vec<_> = spec.ordered().iter().map(|g| g.path.display().to_string()).collect();
        assert_eq!(order, ["a", "tools", "a/.git/hooks"]);
    }

    /// A lasting rule a record lists isn't written again; a one-run rule always is; a damaged record lists nothing.
    #[test]
    fn only_new_lasting_rules_and_every_one_run_rule_are_written() {
        let g = |p: &str, access, lasting| Grant { path: p.into(), access, lasting };
        let spec = Spec {
            profile: "p".into(),
            record: None,
            grants: vec![
                g("proj", Access::ReadWrite, true),
                g("extra", Access::ReadWrite, false),
                g("proj/.env", Access::DenyAll, true),
            ],
            network: false,
            cwd: "proj".into(),
            env: Vec::new(),
        };
        let recorded = read_record(Some(&serde_json::to_string(&[g("proj", Access::ReadWrite, true)]).unwrap()));
        let paths: Vec<_> = to_write(&spec, &recorded).iter().map(|g| g.path.display().to_string()).collect();
        assert_eq!(paths, ["extra", "proj/.env"]);
        // Read-only now: a different rule, written.
        let recorded = read_record(Some(&serde_json::to_string(&[g("proj", Access::Read, true)]).unwrap()));
        assert_eq!(to_write(&spec, &recorded).len(), 3);
        assert!(read_record(Some("not json")).is_empty() && read_record(None).is_empty());
    }

    #[test]
    fn container_names_keep_to_what_windows_allows() {
        assert_eq!(container_name("marv.p.0123abcd.rw"), "marv.p.0123abcd.rw");
        assert_eq!(container_name("a b/c"), "abc");
        assert_eq!(container_name(&"x".repeat(80)).len(), 64);
    }
}
