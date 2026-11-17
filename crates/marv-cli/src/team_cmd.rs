//! `marv team ls` and `marv team start [name]` (one-step team setup, `marv_session::teams`): the teams a folder can
//! start, and starting one from a terminal. New in marv-rs.
//!
//! `start` asks once, y/N on a terminal (as `marv mesh allow`, decision D7: no terminal, nothing happens), for the whole
//! team: the folder rule (and the read rule when the team reads), labelled as the desktop labels a team's, then one
//! Herdr pane per member running `marv --role <role> --team <name>` (inside Herdr, `HERDR_ENV`: the Herdr door's own
//! pane calls, a new tab for the team, the members split in it). Outside Herdr it prints the commands to run, one per
//! terminal. Worktrees aren't made here (a member started elsewhere wouldn't be under the rule's folder): the members
//! share the folder, bounded by their roles' write sets.

use std::path::{Path, PathBuf};

use marv_core::printable;
use marv_kernel::Mode as WireMode;
use marv_session::teams::{Team, available_teams, find_team, read_rule_label, rule_label, team_file, unknown_team};

use crate::mesh_cmd::{confirm_on_tty, event_log, rule_store, short};

fn here() -> Result<(PathBuf, PathBuf, PathBuf), String> {
    let root = std::env::current_dir().and_then(std::fs::canonicalize).map_err(|e| format!("this folder: {e}"))?;
    let home = marv_session::paths::home_dir();
    let env: marv_session::config::Env = std::env::vars().collect();
    let config = marv_session::config::default_config_dir(&env, &home);
    Ok((root, home, config))
}

/// `marv team ls`'s lines.
fn ls_lines(teams: &[Team], problems: &[String]) -> Vec<String> {
    let mut out: Vec<String> = teams
        .iter()
        .map(|t| printable(&format!("{:<14} {} ({}): {} — {}", t.name, t.title, t.source.label(), t.roster(), t.description)))
        .collect();
    for problem in problems {
        out.push(printable(&format!("not loaded: {problem}")));
    }
    out
}

/// `marv team ls`: the teams this folder can start.
pub fn ls() -> i32 {
    let (root, home, config) = match here() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("marv team ls: {}", printable(&e));
            return 1;
        }
    };
    let (teams, problems, _) = available_teams(&root, &home, &config);
    for line in ls_lines(&teams, &problems) {
        println!("{line}");
    }
    println!("\nStart one with `marv team start <name>`.");
    0
}

/// The team `start` starts: `name`, else the folder's own (.marv/team.md, once trusted), else `build-review`.
fn pick<'a>(teams: &'a [Team], name: Option<&str>, root: &Path) -> Result<&'a Team, String> {
    let name = match name {
        Some(name) => name.to_string(),
        None => teams
            .iter()
            .find(|t| t.source == marv_session::agents::AgentSource::Project && team_file(root).exists())
            .map_or_else(|| "build-review".to_string(), |t| t.name.clone()),
    };
    find_team(teams, &name).ok_or_else(|| unknown_team(teams, &name))
}

/// Quotes an argument where a shell would split it.
fn shell_quote(arg: &str) -> String {
    if !arg.is_empty() && arg.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ':' | '+' | '@')) {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

/// Each member's command line, in the team's order (`marv` is this program).
fn commands(team: &Team, marv: &Path) -> Vec<(String, String)> {
    team.members
        .iter()
        .map(|m| (m.role.clone(), format!("{} --role {} --team {}", shell_quote(&marv.to_string_lossy()), m.role, team.name)))
        .collect()
}

/// The sentence the person says yes or no to.
fn question(team: &Team, folder: &str, herdr: bool) -> String {
    let may = if team.read_chats { "may message each other and read each other's chats" } else { "may message each other" };
    let then = if herdr {
        format!("then open {} Herdr panes here running marv as {}", team.members.len(), team.roster())
    } else {
        format!("then print the {} commands to run (one terminal each)", team.members.len())
    };
    format!(
        "Start team {} in {folder}: your Marvs under {folder} {may} without asking, until you revoke it (`marv mesh policies`); {then}. What they send each other arrives marked as from another agent, and anything leaving your machine still asks.",
        team.name
    )
}

/// The team's rules for `folder`: the send rule, and the read rule when the team reads.
fn team_rules(team: &Team, folder: &Path, home: &Path, now_ms: u64) -> Result<Vec<marv_kernel::NewRule>, String> {
    let scope = marv_kernel::FolderScope::new(folder, home).map_err(|e| e.to_string())?;
    let rule = marv_kernel::NewRule {
        from: marv_kernel::Subject::Folder(scope.clone()),
        to: marv_kernel::Subject::Folder(scope),
        mode: WireMode::Send,
        mutual: true,
        expires_at_ms: None,
        by: marv_kernel::RuleBy::Cli,
        created_at_ms: now_ms,
        label: rule_label(&team.name),
    };
    let read = team.read_chats.then(|| marv_kernel::NewRule {
        mode: WireMode::Read,
        label: read_rule_label(&team.name),
        ..rule.clone()
    });
    Ok(std::iter::once(rule).chain(read).collect())
}

/// Makes `rules`, both or neither; their ids.
async fn add_rules(
    store: &marv_kernel::PolicyStore,
    rules: Vec<marv_kernel::NewRule>,
    log: Option<&dyn marv_kernel::EventLog>,
) -> Result<Vec<marv_kernel::PolicyId>, String> {
    let mut made = Vec::new();
    for rule in rules {
        match store.add(rule).await {
            Ok(added) => made.push(added),
            Err(e) => {
                for (id, _) in made {
                    let _ = store.revoke(id).await;
                }
                return Err(e.to_string());
            }
        }
    }
    let mut ids = Vec::new();
    for (id, snapshot) in made {
        if let Some(log) = log {
            marv_session::kernel_events::log_created(log, id, &snapshot);
        }
        ids.push(id);
    }
    Ok(ids)
}

/// `marv team start [name]`.
pub async fn start(name: Option<&str>) -> i32 {
    let (root, home, config) = match here() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("marv team start: {}", printable(&e));
            return 1;
        }
    };
    if marv_session::paths::home_or_above(&root, &home) {
        eprintln!("marv team start: start a team in a project folder, not your home folder or above it.");
        return 1;
    }
    let env: marv_mcp::Env = std::env::vars().collect();
    let herdr = marv_mesh::herdr::herdr_target(&env)
        .and_then(|target| Some((target, marv_session::mesh_process::herdr_program(&env, std::slice::from_ref(&root))?)));
    let store = match rule_store() {
        Ok(store) => store,
        Err(e) => {
            eprintln!("marv team start: {}", printable(&e));
            return 1;
        }
    };
    let log = event_log();
    let marv = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("marv"));
    let code = start_with(
        &store,
        &root,
        &home,
        &config,
        name,
        herdr.is_some(),
        confirm_on_tty,
        &marv,
        log.as_deref().map(|l| l as &dyn marv_kernel::EventLog),
    )
    .await;
    let code = match code {
        Ok(Started { team, commands }) => match herdr {
            Some((_, program)) => open_panes(&program, &root, &team, &commands).await,
            None => {
                println!("Not in Herdr: run each in a terminal of its own, in {}:", printable(&short(&root.to_string_lossy())));
                for (_, line) in &commands {
                    println!("  cd {} && {}", shell_quote(&root.to_string_lossy()), printable(line));
                }
                0
            }
        },
        Err(code) => code,
    };
    if let Some(log) = log {
        log.flush().await;
    }
    code
}

/// What `start_with` made: the team, and each member's command.
struct Started {
    team: Team,
    commands: Vec<(String, String)>,
}

/// [`start`] up to the panes, with its store, folders and way of asking given (tests): the team's rules only after a
/// yes on a terminal.
#[allow(clippy::too_many_arguments)]
async fn start_with(
    store: &marv_kernel::PolicyStore,
    root: &Path,
    home: &Path,
    config: &Path,
    name: Option<&str>,
    herdr: bool,
    confirm: impl FnOnce(&str) -> Option<bool>,
    marv: &Path,
    log: Option<&dyn marv_kernel::EventLog>,
) -> Result<Started, i32> {
    let (teams, problems, _) = available_teams(root, home, config);
    let team = match pick(&teams, name, root) {
        Ok(team) => team.clone(),
        Err(e) => {
            eprintln!("marv team start: {}", printable(&e));
            for problem in &problems {
                eprintln!("  not loaded: {}", printable(problem));
            }
            return Err(1);
        }
    };
    let folder = short(&root.to_string_lossy());
    match confirm(&question(&team, &folder, herdr)) {
        None => {
            eprintln!("marv team start asks you at a terminal, and there isn't one here: nothing was started.");
            return Err(1);
        }
        Some(false) => {
            eprintln!("Nothing was started.");
            return Err(1);
        }
        Some(true) => {}
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    let rules = team_rules(&team, root, home, now).map_err(|e| {
        eprintln!("marv team start: {}", printable(&e));
        1
    })?;
    let ids = add_rules(store, rules, log).await.map_err(|e| {
        eprintln!("marv team start: {}", printable(&e));
        1
    })?;
    let ids: Vec<String> = ids.iter().map(|id| id.0.to_string()).collect();
    println!("Team {}: rule {} made (`marv mesh revoke <id>` ends it).", team.name, ids.join(" and "));
    if team.worktrees {
        println!(
            "The team file asks for worktrees: marv team start keeps everyone in this folder (their roles' write sets bound them)."
        );
    }
    let commands = commands(&team, marv);
    Ok(Started { team, commands })
}

/// One Herdr pane per member: a new tab for the team (the first member), the others split in it. A pane that can't be
/// opened is said, and the rest still open.
async fn open_panes(program: &Path, root: &Path, team: &Team, commands: &[(String, String)]) -> i32 {
    use marv_mesh::herdr::{HerdrAdapter, Place};
    let adapter = HerdrAdapter::new(program, None);
    let cwd = root.to_string_lossy().to_string();
    let label = format!("team {}", team.name);
    let mut first: Option<String> = None;
    let mut failed = 0;
    for (i, (role, line)) in commands.iter().enumerate() {
        let opened = match &first {
            None => adapter.open_pane(Place::Tab, None, &cwd, Some(&label)).await,
            Some(pane) => adapter.open_pane(if i % 2 == 1 { Place::Right } else { Place::Down }, Some(pane), &cwd, None).await,
        };
        let pane = match opened {
            Ok(pane) => pane,
            Err(e) => {
                eprintln!("Couldn't open a pane for the {role}: {}", printable(&e.to_string()));
                failed += 1;
                continue;
            }
        };
        let _ = adapter.call(&["pane", "rename", &pane, &format!("{}-{role}", team.name)]).await;
        if let Err(e) = adapter.pane_run(&pane, line).await {
            eprintln!("Opened pane {pane} for the {role}, but couldn't start Marv there: {}", printable(&e.to_string()));
            failed += 1;
            continue;
        }
        println!("{role}: Herdr pane {pane}");
        first.get_or_insert(pane);
    }
    if failed > 0 { 1 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_member_runs_marv_with_its_role_and_the_team() {
        let team = marv_session::teams::builtin_teams().remove(0);
        let lines = commands(&team, Path::new("/opt/my marv/marv"));
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], ("coordinator".into(), "'/opt/my marv/marv' --role coordinator --team web".into()));
        assert_eq!(lines[3].1, "'/opt/my marv/marv' --role devserver --team web");
        let q = question(&team, "~/p", true);
        assert!(q.contains("open 4 Herdr panes") && q.contains("read each other's chats"), "{q}");
        assert!(question(&team, "~/p", false).contains("print the 4 commands"));
        let ls = ls_lines(&[team], &["~/.marv/teams/x.md: broken".into()]);
        assert!(ls[0].starts_with("web ") && ls[0].contains("coordinator, frontend, backend, devserver"), "{ls:?}");
        assert!(ls[1].starts_with("not loaded:"));
    }

    /// Like `marv mesh allow` (D7): the team's rules are made only after a yes at a terminal, labelled as the desktop
    /// finds a team's, both (send and read) or neither.
    #[tokio::test]
    async fn the_rules_are_made_only_after_a_yes_and_carry_the_teams_labels() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        let root = home.join("Projects/demo");
        std::fs::create_dir_all(&root).unwrap();
        let config = home.join(".marv");
        let store = marv_kernel::PolicyStore::new(&config, &home).unwrap();
        let marv = Path::new("/m");
        let count = || store.load().unwrap().rules.len();
        assert!(
            start_with(&store, &root, &home, &config, Some("pair"), false, |_| None, marv, None).await.is_err(),
            "no terminal"
        );
        assert!(start_with(&store, &root, &home, &config, Some("pair"), false, |_| Some(false), marv, None).await.is_err());
        assert!(start_with(&store, &root, &home, &config, Some("nope"), false, |_| Some(true), marv, None).await.is_err());
        assert_eq!(count(), 0, "nothing made");
        let mut asked = String::new();
        let started = start_with(
            &store,
            &root,
            &home,
            &config,
            None,
            false,
            |q| {
                asked = q.to_string();
                Some(true)
            },
            marv,
            None,
        )
        .await
        .unwrap_or_else(|_| panic!("started"));
        assert_eq!(started.team.name, "build-review", "the default without a team file");
        assert!(asked.contains("Start team build-review in"), "{asked}");
        let rules = store.load().unwrap().rules;
        let labels: Vec<&str> = rules.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["Team build-review", "Team build-review: read each other's chats"]);
        assert_eq!(started.commands.len(), 3);
    }
}
