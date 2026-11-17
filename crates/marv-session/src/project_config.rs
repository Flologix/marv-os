//! A project's own `.marv/config.json`: the few settings a project may set, read through an allow-list that fails
//! closed (new in marv-rs; Marv has no project config layer).
//!
//! The order settings are read in is the person's `~/.marv/config.json`, then this file, then env and flags. A cloned
//! repository is somebody else's text, so this file may set only [`PROJECT_KEYS`]:
//!
//! - **Settings for the project's own code**: `formatter` (which formatter the edit tools run, still inside the
//!   sandbox) and `checks` (whether the project's checks run). The person's own `false` for either still wins: a
//!   project can't turn back on what the person turned off.
//! - **Settings that only narrow**: `"yolo": false`. `"yolo": true` is ignored.
//! - **Until the project is trusted, only narrowing.** A formatter map names commands, and turning `checks` on runs the
//!   project's check commands: both come from the project's files. An untrusted project's config may only turn
//!   things off (`"formatter": false`, `"checks": false`, `"yolo": false`); anything else is ignored and named in the
//!   note, until the person trusts the project. The caller says whether it is (`project_trusted`).
//!
//! - **Its `.marv/checks` files too.** Checks run the project's commands (declarative ones its patterns, script ones
//!   its code), so until the project is trusted [`ProjectConfig::load`] holds them back: checks are off for the
//!   session and the note names each ([`ProjectConfig::hold_checks`]).
//!
//! **Project trust** (`/trust`, `~/.marv/project-trust.json`, #220) decides `project_trusted` at each shell: the TUI
//! from its startup's `TrustStatus`, `marv -p` from `--trust-project` or the stored trust, Marv desktop from the
//! stored trust for the chat's folder (`crate::project_trust::config_trusted`). Only an explicit trust of exactly the
//! project's files counts (`TrustStatus::Trusted`): a project with no instruction files, skills or agent types has
//! given the person nothing to trust, so its config still only narrows. Known limits, left as they are: the config
//! file isn't among the hashed files, so a trusted project's later change to it applies without a new question; and in
//! Marv desktop an app added from the gallery reaches new chats but not an open chat after `/clear` (only the file is
//! written, not the running sessions' servers).
//!
//! Every other key, today's (`model`, `provider`, `approve`, `hooks`, `mesh`…) and any added to [`FileConfig`] later,
//! is ignored with one startup note ([`ProjectConfig::note`]). That's the point of an allow-list: a key added to the
//! person's config tomorrow is not settable by a project until it's added here on purpose.
//!
//! The file is read like skills and agents: only a regular file inside the project (after realpath), opened without
//! blocking, at most [`PROJECT_CONFIG_CAP`]. Where it *is* the person's own config (Marv started in the home folder with
//! the default config folder), it's not a project's at all: nothing from it is applied twice and there's no note.

use std::path::Path;

use marv_tools::formatter::FormatterSetting;
use serde_json::{Map, Value};

use crate::config::{Config, FileConfig};

/// The keys a project's `.marv/config.json` may set. Anything else is ignored.
pub const PROJECT_KEYS: &[&str] = &["formatter", "checks", "yolo"];

/// The most of a project's `.marv/config.json` that's read.
pub const PROJECT_CONFIG_CAP: u64 = 1024 * 1024;

/// What a project's config may change, once read through [`PROJECT_KEYS`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProjectSettings {
    pub formatter: Option<FormatterSetting>,
    pub checks: Option<bool>,
    /// `"yolo": false`: yolo is off in this project whatever the person's config says.
    pub yolo_off: bool,
    /// The project's `.marv/checks` files held back because the project isn't trusted (by check name): `checks` is then
    /// `Some(false)`, and `/checks on` says why.
    pub untrusted_checks: Vec<String>,
    /// Checks are held off this session because the project isn't trusted (whether or not it had any at the start):
    /// `/checks on` says why.
    pub checks_held: bool,
}

impl ProjectSettings {
    /// `config` (the person's, resolved) with the project's settings on top. A project's `formatter` or `checks`
    /// applies unless the person turned that off; `yolo_off` only ever turns yolo off.
    pub fn apply(&self, mut config: Config) -> Config {
        self.apply_to(&mut config.formatter, &mut config.checks, &mut config.yolo);
        config
    }

    /// [`Self::apply`] on the three settings themselves, wherever a shell keeps them (Marv desktop's session options).
    pub fn apply_to(&self, formatter: &mut FormatterSetting, checks: &mut bool, yolo: &mut bool) {
        if let Some(project) = &self.formatter
            && *formatter != FormatterSetting::Off
        {
            *formatter = project.clone();
        }
        if let Some(project) = self.checks
            && *checks
        {
            *checks = project;
        }
        if self.yolo_off {
            *yolo = false;
        }
    }
}

/// A project's config as read: what applies, and the startup note (what was ignored, and why).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProjectConfig {
    pub settings: ProjectSettings,
    pub note: Option<String>,
}

impl ProjectConfig {
    /// The project in `root`'s `.marv/config.json`, unless it's `user_config` (the person's own file). No file, or one
    /// that isn't a regular file inside the project or is over the cap: nothing applies and there's no note.
    /// `project_trusted`: the person trusts this project (see the module's merge note); untrusted, only narrowing
    /// values apply.
    /// Untrusted, the project's `.marv/checks` (commands from its files too) are held back the same way: checks are off
    /// for the whole session, so a check file that arrives after the start (checks load again each pass) never runs.
    pub fn load(root: &Path, user_config: &Path, project_trusted: bool) -> Self {
        let mut read =
            read_project_file(root, user_config).map(|raw| Self::from_value(&raw, project_trusted)).unwrap_or_default();
        if !project_trusted {
            read.hold_checks(crate::checks::load_checks(root).0.into_iter().map(|c| c.name).collect());
        }
        read
    }

    /// The project isn't trusted: its checks don't run this session, whatever is in `.marv/checks` now (`names`, named
    /// in the note) or later.
    pub fn hold_checks(&mut self, names: Vec<String>) {
        self.settings.checks = Some(false);
        self.settings.checks_held = true;
        if !names.is_empty() {
            let shown: Vec<String> = names.iter().map(|n| format!("`{}`", shown_name(n))).collect();
            let held = format!(
                "This project's .marv/checks has {}: not run until you trust this project (they run commands from its files).",
                listed(shown)
            );
            self.note = Some(cap_note(&match self.note.take() {
                Some(note) => format!("{note} {held}"),
                None => held,
            }));
        }
        self.settings.untrusted_checks = names;
    }

    /// Reads `raw` through the allow-list; untrusted, only its narrowing values.
    pub fn from_value(raw: &Value, project_trusted: bool) -> Self {
        let Value::Object(map) = raw else {
            return Self { note: Some(format!("{SHOWN} isn't a JSON object: ignored.")), ..Self::default() };
        };
        let mut settings = ProjectSettings::default();
        let (mut ignored, mut invalid, mut notes, mut untrusted) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for (key, value) in map {
            // The allow-list first: a key that isn't on it never reaches anything that applies one.
            if !PROJECT_KEYS.contains(&key.as_str()) {
                // Told about with why, below.
                if !["hooks", "approve", "writable", "mesh"].contains(&key.as_str()) {
                    ignored.push(format!("\"{}\"", shown_name(key)));
                }
                continue;
            }
            match key.as_str() {
                "formatter" | "checks" => {
                    match FileConfig::from_value(&Value::Object(Map::from_iter([(key.clone(), value.clone())]))) {
                        // Untrusted, only off: a formatter map or `true` runs the project's commands.
                        Ok(file)
                            if !project_trusted
                                && (!matches!(file.formatter, None | Some(FormatterSetting::Off))
                                    || !matches!(file.checks, None | Some(false))) =>
                        {
                            untrusted.push(format!("\"{key}\": {}", shown_value(value)))
                        }
                        Ok(file) => {
                            settings.formatter = file.formatter.or(settings.formatter.take());
                            settings.checks = file.checks.or(settings.checks);
                        }
                        Err(problem) => invalid.push(problem),
                    }
                }
                "yolo" if *value == Value::Bool(false) => settings.yolo_off = true,
                // `"yolo": true` (or anything but false): it would widen.
                _ => ignored.push(format!("\"{key}\": {}", shown_value(value))),
            }
        }
        if map.contains_key("hooks") {
            notes.push(format!(
                "{SHOWN} has \"hooks\": not run. Hooks run only from your own ~/.marv/config.json, never from a project's (a \
                 cloned repository could otherwise run commands when Marv starts)."
            ));
        }
        let widening: Vec<String> =
            ["approve", "writable"].into_iter().filter(|k| map.contains_key(*k)).map(|k| format!("\"{k}\"")).collect();
        if !widening.is_empty() {
            notes.push(format!(
                "{SHOWN} has {}: ignored. Pre-approved scopes and writable sandbox folders come only from your own \
                 ~/.marv/config.json, never from a project's (a cloned repository could otherwise approve its own calls or \
                 widen the sandbox).",
                widening.join(" and ")
            ));
        }
        // The radio, and wake with it: a project turning the radio on would put this person's Marv on a channel with
        // their other Marvs without them choosing it.
        if map.contains_key("mesh") {
            notes.push(format!("{SHOWN} sets \"mesh\". Only your own config can turn the radio or wake on: ignored."));
        }
        if !ignored.is_empty() {
            notes.push(format!(
                "{SHOWN} sets {}: ignored. A project's config may set only \"formatter\", \"checks\" and \"yolo\": false; \
                 everything else comes from your own ~/.marv/config.json.",
                listed(ignored)
            ));
        }
        if !untrusted.is_empty() {
            notes.push(format!(
                "{SHOWN} sets {}: ignored until you trust this project (they run commands from its files). Settings that \
                 only turn things off apply.",
                listed(untrusted)
            ));
        }
        if !invalid.is_empty() {
            let invalid: Vec<String> = invalid.iter().map(|p| shown_value(&Value::from(p.as_str()))).collect();
            notes.push(format!("{SHOWN}: {}: ignored.", listed(invalid)));
        }
        if settings.yolo_off {
            notes.push(format!("{SHOWN} turns yolo off: changes and commands are asked about in this project."));
        }
        Self { settings, note: (!notes.is_empty()).then(|| cap_note(&notes.join(" "))) }
    }
}

const SHOWN: &str = "This project's .marv/config.json";

/// The longest key or check name a note shows, and the most it names; the longest a note gets. The names are the
/// project's, so they're cut, cleaned and counted before they reach a terminal.
const MAX_NAME_CHARS: usize = 40;
const MAX_NAMED: usize = 20;
const MAX_NOTE_CHARS: usize = 2000;

/// `text` as one printable line of at most `max` characters, cut with `…`.
fn one_line(text: &str, max: usize) -> String {
    let clean: String = marv_core::printable(text).chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    if clean.chars().count() <= max { clean } else { clean.chars().take(max - 1).collect::<String>() + "…" }
}

/// A project's key or check name as a note shows it.
fn shown_name(name: &str) -> String {
    one_line(name, MAX_NAME_CHARS)
}

/// A project's value as a note shows it: its JSON, cut short.
fn shown_value(value: &Value) -> String {
    let text = match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    one_line(&text, 60)
}

/// At most [`MAX_NAMED`] items, then how many more.
fn listed(items: Vec<String>) -> String {
    let more = items.len().saturating_sub(MAX_NAMED);
    let mut shown: Vec<String> = items.into_iter().take(MAX_NAMED).collect();
    if more > 0 {
        shown.push(format!("and {more} more"));
    }
    shown.join(", ")
}

fn cap_note(note: &str) -> String {
    one_line(note, MAX_NOTE_CHARS)
}

/// The project's `.marv/config.json` as JSON: only a regular file inside the project, never the person's own, at most
/// [`PROJECT_CONFIG_CAP`]. Anything else is `None`.
fn read_project_file(root: &Path, user_config: &Path) -> Option<Value> {
    use std::io::Read;
    let real = marv_sandbox::resolve_within(root, ".marv/config.json").ok()?;
    if std::fs::canonicalize(user_config).is_ok_and(|own| own == real) {
        return None;
    }
    if !std::fs::symlink_metadata(&real).ok()?.file_type().is_file() {
        return None;
    }
    // Non-blocking, and checked again once open: the path could have changed in between.
    let mut open = std::fs::OpenOptions::new();
    open.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut open, nix::fcntl::OFlag::O_NONBLOCK.bits());
    let file = open.open(&real).ok()?;
    if !file.metadata().ok()?.file_type().is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(PROJECT_CONFIG_CAP + 1).read_to_string(&mut text).ok()?;
    if text.len() as u64 > PROJECT_CONFIG_CAP {
        return None;
    }
    match serde_json::from_str(&text) {
        Ok(raw) => Some(raw),
        // Not JSON: nothing applies, and the note says so.
        Err(_) => Some(Value::Null),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Env, resolve_config};
    use serde_json::json;
    use std::collections::BTreeMap;
    #[cfg(unix)]
    use std::time::{Duration, Instant};

    fn temp() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("marv-project-config-{}-{}", std::process::id(), marv_fs::random_hex()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(dir).unwrap()
    }

    fn note(raw: Value) -> String {
        ProjectConfig::from_value(&raw, true).note.unwrap_or_default()
    }

    /// A project whose `.marv/config.json` isn't a regular file inside it: nothing read past the cap, nothing that
    /// blocks, nothing applied.
    #[cfg(unix)]
    #[test]
    fn only_a_regular_file_inside_the_project_is_read() {
        use std::os::unix::fs::symlink;
        let base = temp();
        let (root, outside) = (base.join("project"), base.join("outside.json"));
        std::fs::create_dir_all(root.join(".marv")).unwrap();
        let config = root.join(".marv/config.json");
        let user = base.join("user-config.json");
        std::fs::write(&outside, r#"{"hooks": {"sessionStart": ["x"]}, "yolo": false}"#).unwrap();
        let started = Instant::now();
        for target in [Path::new("/dev/zero"), Path::new("/dev/stdin"), outside.as_path()] {
            symlink(target, &config).unwrap();
            assert_eq!(ProjectConfig::load(&root, &user, true), ProjectConfig::default(), "{}", target.display());
            std::fs::remove_file(&config).unwrap();
        }
        // A FIFO with no writer: opening it for reading would block.
        if std::process::Command::new("mkfifo").arg(&config).status().is_ok_and(|s| s.success()) {
            assert_eq!(ProjectConfig::load(&root, &user, true), ProjectConfig::default());
            std::fs::remove_file(&config).unwrap();
        }
        // Bigger than the cap: not read as a config.
        let big = format!(r#"{{"hooks": {{}}, "pad": "{}"}}"#, "x".repeat(PROJECT_CONFIG_CAP as usize));
        std::fs::write(&config, big).unwrap();
        assert_eq!(ProjectConfig::load(&root, &user, true), ProjectConfig::default());
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn hooks_approvals_and_the_mesh_are_refused_with_why_and_the_users_own_config_isnt_a_project_one() {
        let root = temp();
        let user = root.join("user-config.json");
        assert_eq!(ProjectConfig::load(&root, &user, true), ProjectConfig::default(), "no file");
        std::fs::create_dir_all(root.join(".marv")).unwrap();
        std::fs::write(root.join(".marv/config.json"), r#"{"hooks": {"sessionStart": ["touch pwned"]}}"#).unwrap();
        let refusal = ProjectConfig::load(&root, &user, true).note.unwrap();
        assert!(refusal.contains(".marv/config.json has \"hooks\": not run"), "{refusal}");
        let refusal = note(json!({"approve": ["network"], "writable": ["/"]}));
        assert!(refusal.contains("has \"approve\" and \"writable\": ignored"), "{refusal}");
        assert!(!refusal.contains("hooks"), "{refusal}");
        assert_eq!(
            note(json!({"mesh": {"radio": true, "wake": true}})),
            "This project's .marv/config.json sets \"mesh\". Only your own config can turn the radio or wake on: ignored."
        );
        // Marv started in the home folder: the project's .marv/config.json is the user's own.
        assert_eq!(ProjectConfig::load(&root, &root.join(".marv/config.json"), true), ProjectConfig::default());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn other_keys_are_ignored_in_one_note_and_bad_values_are_told() {
        let read = ProjectConfig::from_value(&json!({"model": "x", "provider": "ollama", "yolo": true, "futureKey": 1}), true);
        assert_eq!(read.settings, ProjectSettings::default());
        let said = read.note.unwrap();
        assert!(said.contains("sets \"futureKey\", \"model\", \"provider\", \"yolo\": true: ignored."), "{said}");
        assert_eq!(said.matches("ignored").count(), 1, "one note: {said}");
        let read = ProjectConfig::from_value(&json!({"checks": "yes", "formatter": 3}), true);
        assert_eq!(read.settings, ProjectSettings::default());
        assert!(read.note.unwrap().contains("checks: expected a boolean"));
        assert_eq!(ProjectConfig::from_value(&Value::Null, true).note.unwrap(), format!("{SHOWN} isn't a JSON object: ignored."));
        // Only what it may set, and quietly, except the narrowing it's told about.
        let read = ProjectConfig::from_value(&json!({"formatter": {"rs": "rustfmt"}, "checks": false}), true);
        assert_eq!(read.note, None);
        assert_eq!(
            read.settings,
            ProjectSettings {
                formatter: Some(FormatterSetting::Custom(BTreeMap::from([("rs".into(), "rustfmt".into())]))),
                checks: Some(false),
                ..ProjectSettings::default()
            }
        );
        assert!(note(json!({"yolo": false})).contains("turns yolo off"));
    }

    #[test]
    fn an_untrusted_project_only_turns_things_off() {
        let raw = json!({"formatter": {"rs": "rustfmt"}, "checks": true, "yolo": false});
        let read = ProjectConfig::from_value(&raw, false);
        assert_eq!(read.settings, ProjectSettings { yolo_off: true, ..ProjectSettings::default() });
        let note = read.note.unwrap();
        assert!(note.contains("\"checks\": true, \"formatter\": {\"rs\":\"rustfmt\"}: ignored until you trust"), "{note}");
        let read = ProjectConfig::from_value(&json!({"formatter": false, "checks": false}), false);
        assert_eq!(
            read.settings,
            ProjectSettings { formatter: Some(FormatterSetting::Off), checks: Some(false), ..ProjectSettings::default() }
        );
        assert_eq!(read.note, None);
        assert_eq!(ProjectConfig::from_value(&raw, true).settings.checks, Some(true), "trusted, it applies");
    }

    #[test]
    fn an_untrusted_projects_checks_are_held_back_and_named() {
        let root = temp();
        std::fs::create_dir_all(root.join(".marv/checks")).unwrap();
        std::fs::write(
            root.join(".marv/checks/imports.json"),
            r#"{"glob": "*.ts", "forbid": "x", "message": "no x", "why": "w"}"#,
        )
        .unwrap();
        let user = root.join("user-config.json");
        let held = ProjectConfig::load(&root, &user, false);
        assert_eq!((held.settings.checks, held.settings.untrusted_checks.clone()), (Some(false), vec!["imports".to_string()]));
        assert!(held.note.unwrap().contains(".marv/checks has `imports`: not run until you trust this project"));
        // Planted names are cut and cleaned, and only so many are named.
        let mut many = ProjectConfig::default();
        many.hold_checks((0..30).map(|n| format!("c{n}\x1b[2J{}", "x".repeat(80))).collect());
        let note = many.note.unwrap();
        assert!(!note.contains('\x1b') && note.contains("and 10 more") && note.chars().count() < 2100, "{note}");
        assert_eq!(ProjectConfig::load(&root, &user, true), ProjectConfig::default(), "trusted: as before");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_project_narrows_and_sets_its_own_but_the_persons_off_wins() {
        let user = resolve_config(None, &Env::new());
        assert!(user.yolo && user.checks && user.formatter == FormatterSetting::Auto);
        let project = ProjectSettings {
            formatter: Some(FormatterSetting::Off),
            checks: Some(false),
            yolo_off: true,
            ..ProjectSettings::default()
        };
        let applied = project.apply(user.clone());
        assert!(!applied.yolo && !applied.checks && applied.formatter == FormatterSetting::Off);
        let off = resolve_config(
            Some(&FileConfig { formatter: Some(FormatterSetting::Off), checks: Some(false), ..FileConfig::default() }),
            &Env::new(),
        );
        let project =
            ProjectSettings { formatter: Some(FormatterSetting::Auto), checks: Some(true), ..ProjectSettings::default() };
        let applied = project.apply(off);
        assert!(applied.formatter == FormatterSetting::Off && !applied.checks, "the person's off wins");
    }
}
