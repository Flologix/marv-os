//! Teams: a set of chats started together in one folder, each as a role (`crate::roles`), with how they work together
//! (a shared tree or worktrees each, whether they may read each other's chats) and a shared brief. New in marv-rs
//! (runtime-kernel coordination, one-step team setup).
//!
//! ```text
//! .marv/team.md             the project's team (loaded only when the project is trusted, as .marv/roles)
//! ~/.marv/teams/<name>.md   your personal ones
//! ```
//!
//! YAML frontmatter, then the team's shared brief (cut like a skill's body):
//! - `name` (lowercase letters, digits and dashes; else the file's name), `description` (required);
//! - `members`: a list of `{role, model}`; `role` must be a role a chat there may start as, `model` is optional:
//!   `inherit` (the chat's default), `small` (the person's `smallModel`) or a model id;
//! - `worktrees` (default false: one shared tree, members bounded by their roles' write sets);
//! - `read_chats` (default true: the team's chats may `chat_read` each other).
//!
//! Built in (no file needed): [`builtin_teams`]: Web app, Build + review, Pair. A file with a built-in's name replaces
//! it. The brief goes into each member's system prompt after its role, once, at the chat's start ([`Team::brief`]):
//! byte-stable for the session. Starting a team (the folder rule, each chat with its role) is the desktop's and
//! `marv team start`'s, never a model's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::agents::AgentSource;
use crate::frontmatter::parse_frontmatter;
use crate::roles::{Role, find_role};
use crate::skills::{cap_body, valid_name};

/// The most members a team may have (each is a chat, a session and a mesh plug).
pub const MAX_MEMBERS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamMember {
    pub role: String,
    /// `inherit`, `small` or a model id; `None`: the chat's default (as `inherit`).
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Team {
    pub name: String,
    /// What the person reads ("Web app"); a file's is its name.
    pub title: String,
    pub description: String,
    pub members: Vec<TeamMember>,
    /// Each member in its own git worktree (default false: one shared tree).
    pub worktrees: bool,
    /// The members may read each other's chats (a second rule, in the read mode). Default true.
    pub read_chats: bool,
    /// The shared brief, given to every member after its role.
    pub body: String,
    /// One line per member: its role's tools, what it may write and whom it takes tasks from ([`Team::describe_members`]),
    /// so a coordinator doesn't hand an edit to a role that can't make it. Empty until filled; not part of a team file.
    pub member_notes: Vec<String>,
    /// The members whose roles can edit files, with where (None: anywhere): who a read-only chat is told to ask
    /// ([`marv_tools::ReadOnlyNote`]). Filled with the notes.
    pub editors: Vec<(String, Option<Vec<String>>)>,
    /// The other members' chats as this one started (role, chat name, mesh name; [`TeamPeer`]): the brief names each
    /// one's inbox, so a task goes to this team's frontend and not another folder's Marv that has the same role.
    /// Empty: not known (a terminal Marv). Not part of a team file.
    pub peers: Vec<TeamPeer>,
    pub source: AgentSource,
}

/// A teammate the brief may name: a role name and a mesh name of plain characters (they go into the system prompt
/// and an inbox name).
fn valid_peer(p: &TeamPeer) -> bool {
    let plain = |s: &str| {
        !s.is_empty()
            && s.len() <= 64
            && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    };
    valid_name(&p.role) && plain(&p.mesh_name)
}

/// Another member of the chat's team: its role, the chat's name as the person gave it, and its mesh name (the
/// `<session>` in `peer.<session>.agent.parent.inbox`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamPeer {
    pub role: String,
    pub title: String,
    pub mesh_name: String,
}

impl Team {
    /// The members' roles, in order ("coordinator, frontend, backend, devserver").
    pub fn roster(&self) -> String {
        self.members.iter().map(|m| m.role.as_str()).collect::<Vec<_>>().join(", ")
    }

    /// The system prompt's team section, after the role's: fixed for the chat's life (the prompt cache). Marv's words
    /// around the team file's brief (the person's or a trusted project's, like a role's body).
    pub fn brief(&self) -> String {
        let mut out = format!(
            "# Your team: {}\n\nThe person started this chat as one of a team in this folder: {}. mesh_list shows each teammate's chat name and role; mesh_send reaches them.",
            self.name,
            self.roster()
        );
        let peers: Vec<&TeamPeer> = self.peers.iter().filter(|p| valid_peer(p)).collect();
        if !peers.is_empty() {
            out.push_str(
                "\n\nYour teammates' inboxes (send your team's messages only to these: another Marv on this machine with the same role works in another folder and isn't on your team, even if mesh_list shows it):",
            );
            for p in peers {
                let title = marv_core::printable(&p.title).chars().take(60).collect::<String>();
                out.push_str(&format!(
                    "\n- {} (\"{}\"): peer.{}.agent.parent.inbox",
                    p.role,
                    title.replace('"', "'"),
                    p.mesh_name
                ));
            }
        }
        out.push_str(TEAM_HABITS);
        if !self.member_notes.is_empty() {
            out.push_str("\n\nWho can do what (each chat's role, as started; hand a task only to a chat whose role can do it):");
            for note in &self.member_notes {
                out.push_str(&format!("\n- {note}"));
            }
        }
        if !self.body.is_empty() {
            out.push_str(&format!("\n\n{}", self.body));
        }
        out
    }

    /// Fills [`Self::member_notes`] from `roles`, one line per member of the roster: the role's tools, whether it edits
    /// files and where, whether its commands can write caches, and whom it takes tasks from. Each line describes the
    /// member's *effective* role, as its own chat runs it: the write paths the person typed for it (`writes`, in the
    /// members' order, None or missing: the role's own; [`member_role`]) and the coordinator's tasks every member takes
    /// ([`in_team`]). A role that isn't found has no line.
    pub fn describe_members(&mut self, roles: &[Role], writes: &[Option<Vec<String>>]) {
        let roster: Vec<String> = self.members.iter().map(|m| m.role.clone()).collect();
        let effective: Vec<Role> = self
            .members
            .iter()
            .enumerate()
            .filter_map(|(i, m)| {
                let role = find_role(roles, &m.role)?.clone();
                let typed = writes.get(i).and_then(|w| w.as_deref());
                // A typed set that can't apply (it would widen a role that writes nothing) leaves the role's own.
                let role = member_role(role.clone(), typed).unwrap_or(role);
                Some(in_team(role, Some(roster.as_slice())))
            })
            .collect();
        self.member_notes = effective.iter().map(|role| role.team_line()).collect();
        self.editors =
            effective.iter().filter(|role| role.can_edit()).map(|role| (role.name.clone(), role.writes.clone())).collect();
    }

    /// Why a member can't start here, if one can't: its role isn't one a chat in this folder may start as.
    pub fn check(&self, roles: &[Role]) -> Result<(), String> {
        let missing: Vec<&str> = self.members.iter().map(|m| m.role.as_str()).filter(|r| find_role(roles, r).is_none()).collect();
        if missing.is_empty() {
            return Ok(());
        }
        let names: Vec<&str> = roles.iter().map(|r| r.name.as_str()).collect();
        Err(format!(
            "{} ({}): there's no role named {}. Roles here: {}.",
            marv_core::printable(&self.title),
            self.source.label(),
            missing.iter().map(|m| format!("\"{}\"", marv_core::printable(m))).collect::<Vec<_>>().join(", "),
            names.join(", ")
        ))
    }
}

/// The label of team `name`'s folder rule (the desktop and `marv team start` both write it, and find a team's rules
/// again by it).
pub fn rule_label(name: &str) -> String {
    format!("Team {name}")
}

/// The label of team `name`'s read rule (its members may read each other's chats).
/// How a team's chats keep in touch, in every member's brief: the inbox for work and answers, the board for one-line
/// status, and a teammate's chat read only when the inbox can't answer.
pub const TEAM_HABITS: &str = "\n\nHow your team keeps in touch: send work and questions to a teammate's inbox (mesh_send), at the exact address the list above gives. When you finish a task, end your turn with the result: Marv sends it back to whoever gave you the task, so never mesh_send a report, and never tell a teammate where to send theirs. The team board (board_post) is for one-line status everyone can see: at most 280 characters, so write it short the first time. To catch up after a pause, read your inbox, the board and your own notes first; read a teammate's chat (chat_read) only when they can't answer you. Never read a chat outside your team. A server a teammate keeps running with process (a dev server) answers your bash at its localhost address without network: true.";

pub fn read_rule_label(name: &str) -> String {
    format!("Team {name}: read each other's chats")
}

/// The team a rule labelled `label` belongs to, if it's a team's.
pub fn team_of_rule_label(label: &str) -> Option<&str> {
    let rest = label.strip_prefix("Team ")?;
    let name = rest.strip_suffix(": read each other's chats").unwrap_or(rest);
    valid_name(name).then_some(name)
}

fn member(role: &str, model: Option<&str>) -> TeamMember {
    TeamMember { role: role.into(), model: model.map(str::to_string) }
}

/// The built-in templates: Web app (coordinator, frontend, backend, devserver), Build + review (coordinator, worker,
/// reviewer) and Pair (worker, reviewer). All in one shared tree; members may read each other's chats.
pub fn builtin_teams() -> Vec<Team> {
    let team = |name: &str, title: &str, description: &str, members: Vec<TeamMember>| Team {
        name: name.into(),
        title: title.into(),
        description: description.into(),
        members,
        worktrees: false,
        read_chats: true,
        body: String::new(),
        member_notes: Vec::new(),
        editors: Vec::new(),
        peers: Vec::new(),
        source: AgentSource::BuiltIn,
    };
    vec![
        team(
            "web",
            "Web app",
            "A coordinator, a frontend and a backend chat, and one that runs the dev server.",
            vec![
                member("coordinator", None),
                member("frontend", None),
                member("backend", None),
                member("devserver", Some("small")),
            ],
        ),
        team(
            "build-review",
            "Build + review",
            "A coordinator hands tasks to a worker; a reviewer checks the work.",
            vec![member("coordinator", None), member("worker", None), member("reviewer", None)],
        ),
        team(
            "pair",
            "Pair",
            "A worker you talk to, and a reviewer that checks its work.",
            vec![member("worker", None), member("reviewer", None)],
        ),
    ]
}

/// The project's team file.
pub fn team_file(root: &Path) -> PathBuf {
    root.join(".marv").join("team.md")
}

/// The person's teams folder.
pub fn teams_dir(home: &Path) -> PathBuf {
    home.join(".marv").join("teams")
}

/// A model a member may name: `inherit`, `small`, or a model id (no spaces or control characters).
fn valid_model(model: &str) -> bool {
    !model.is_empty() && model.len() <= 200 && !model.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn flag(value: Option<&Value>, field: &str, default: bool, shown: &str) -> Result<bool, String> {
    match value {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(format!("{shown}: {field} should be true or false.")),
    }
}

/// Parses a team file's text.
pub fn parse_team(text: &str, file: &str, source: AgentSource, shown: &str) -> Result<Team, String> {
    let parsed = parse_frontmatter(text, shown)?;
    let field = |k: &str| parsed.fields.get(k).and_then(Value::as_str).map(|s| s.trim().to_string());
    let name = field("name").unwrap_or_else(|| file.trim_end_matches(".md").to_string());
    if !valid_name(&name) {
        return Err(format!(
            "{shown}: the name \"{}\" should be lowercase letters, digits and dashes.",
            marv_core::printable(&name)
        ));
    }
    let description = field("description").unwrap_or_default();
    if description.is_empty() {
        return Err(format!("{shown} needs a description: it's how the person tells the team from the others."));
    }
    let Some(Value::Array(items)) = parsed.fields.get("members") else {
        return Err(format!("{shown} needs members: a list like `- role: coordinator`."));
    };
    if items.is_empty() || items.len() > MAX_MEMBERS {
        return Err(format!("{shown}: a team has 1 to {MAX_MEMBERS} members."));
    }
    let mut members = Vec::new();
    for item in items {
        let (role, model) = match item {
            Value::String(role) => (role.trim().to_string(), None),
            Value::Object(map) => {
                let role = map.get("role").and_then(Value::as_str).map(|s| s.trim().to_string()).unwrap_or_default();
                let model = match map.get("model") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(m)) => Some(m.trim().to_string()),
                    Some(_) => return Err(format!("{shown}: a member's model should be inherit, small or a model id.")),
                };
                (role, model)
            }
            _ => return Err(format!("{shown}: each member should be `- role: <name>`.")),
        };
        if !valid_name(&role) {
            return Err(format!("{shown}: \"{}\" isn't a role name.", marv_core::printable(&role)));
        }
        if let Some(bad) = model.as_ref().filter(|m| !valid_model(m)) {
            return Err(format!("{shown}: \"{}\" isn't a model id.", marv_core::printable(bad)));
        }
        members.push(TeamMember { role, model });
    }
    let worktrees = flag(parsed.fields.get("worktrees"), "worktrees", false, shown)?;
    let read_chats = flag(parsed.fields.get("read_chats"), "read_chats", true, shown)?;
    Ok(Team {
        title: name.clone(),
        name,
        description,
        members,
        worktrees,
        read_chats,
        body: cap_body(parsed.body, file),
        member_notes: Vec::new(),
        editors: Vec::new(),
        peers: Vec::new(),
        source,
    })
}

fn load_file(path: &Path, file: &str, source: AgentSource, shown: &str) -> Result<Team, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{shown}: couldn't be read ({e})."))?;
    parse_team(&text, file, source, shown)
}

/// The project's team (`.marv/team.md`), or why it couldn't be loaded. Never through a symlink leading out of `.marv`.
pub fn load_project_team(root: &Path) -> (Option<Team>, Vec<String>) {
    let dir = root.join(".marv");
    let shown = ".marv/team.md";
    if !dir.join("team.md").exists() {
        return (None, Vec::new());
    }
    let result = marv_sandbox::resolve_within(&dir, "team.md")
        .map_err(|_| format!("{shown}: leads outside .marv, so it wasn't loaded."))
        .and_then(|path| load_file(&path, "team.md", AgentSource::Project, shown));
    match result {
        Ok(team) => (Some(team), Vec::new()),
        Err(problem) => (None, vec![problem]),
    }
}

/// The person's teams (`~/.marv/teams/*.md`), and the files that couldn't be loaded.
pub fn load_personal_teams(home: Option<&Path>) -> (Vec<Team>, Vec<String>) {
    let Some(home) = home else { return (Vec::new(), Vec::new()) };
    let dir = teams_dir(home);
    let Ok(entries) = std::fs::read_dir(&dir) else { return (Vec::new(), Vec::new()) };
    let mut files: Vec<String> =
        entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|f| f.ends_with(".md")).collect();
    files.sort();
    let (mut teams, mut problems) = (Vec::new(), Vec::new());
    for file in files {
        let shown = format!("~/.marv/teams/{file}");
        let result = marv_sandbox::resolve_within(&dir, &file)
            .map_err(|_| format!("{shown}: leads outside ~/.marv/teams, so it wasn't loaded."))
            .and_then(|path| load_file(&path, &file, AgentSource::Personal, &shown));
        match result {
            Ok(team) => teams.push(team),
            Err(problem) => problems.push(problem),
        }
    }
    (teams, problems)
}

/// Built-in, then `personal`, then `project` (later wins a name): the built-ins in their order, then the files' by name.
pub fn merge_teams(personal: Vec<Team>, project: Option<Team>) -> Vec<Team> {
    let mut files: BTreeMap<String, Team> = BTreeMap::new();
    files.extend(personal.into_iter().chain(project).map(|t| (t.name.clone(), t)));
    let mut out: Vec<Team> = builtin_teams().into_iter().filter(|t| !files.contains_key(&t.name)).collect();
    out.extend(files.into_values());
    out
}

pub fn find_team<'a>(teams: &'a [Team], name: &str) -> Option<&'a Team> {
    teams.iter().find(|t| t.name == name)
}

/// The team a chat started in team `name` with `roster` (its members' roles as started; empty: the file's) gets for
/// its brief: the file's (or built-in's) with that roster, or, when no team has that name (a template renamed when it
/// was started), one with only the name and the roster. `None` when there's nothing to say (no file, no roster) or the
/// name isn't a team name.
pub fn team_for_chat(teams: &[Team], name: &str, roster: &[String]) -> Option<Team> {
    if !valid_name(name) || roster.iter().any(|r| !valid_name(r)) {
        return None;
    }
    let members = || roster.iter().map(|r| member(r, None)).collect::<Vec<_>>();
    match find_team(teams, name) {
        Some(team) => {
            let mut team = team.clone();
            if !roster.is_empty() {
                team.members = members();
            }
            Some(team)
        }
        None if !roster.is_empty() => Some(Team {
            name: name.to_string(),
            title: name.to_string(),
            description: String::new(),
            members: members(),
            worktrees: false,
            read_chats: true,
            body: String::new(),
            member_notes: Vec::new(),
            editors: Vec::new(),
            peers: Vec::new(),
            source: AgentSource::BuiltIn,
        }),
        None => None,
    }
}

/// Role `role` for one team member, with the write paths the person typed for that chat when it started (`writes`,
/// Marv desktop's New team): they replace the role's for that chat. Each is a project-relative prefix
/// (`marv_tools::write_set::parse_prefix`). A role that changes no files (`writes: []`) can't be given paths: that
/// would widen it. `None` or empty: the role's own.
pub fn member_role(mut role: Role, writes: Option<&[String]>) -> Result<Role, String> {
    let Some(list) = writes.filter(|w| !w.is_empty()) else { return Ok(role) };
    if role.writes.as_ref().is_some_and(Vec::is_empty) {
        return Err(format!("The {} role changes no files, so it can't be given write paths.", role.name));
    }
    let mut out: Vec<String> = Vec::new();
    for raw in list {
        let prefix = marv_tools::write_set::parse_prefix(raw.trim()).map_err(|why| format!("writes: {why}."))?;
        if !out.contains(&prefix) {
            out.push(prefix);
        }
    }
    role.writes = Some(out);
    Ok(role)
}

/// `role` as a member of a team whose members' roles are `roster` (None: not in a team): when the roster has a
/// `coordinator` and this isn't it, the role takes the coordinator's tasks and reports to it, whatever its own
/// `takes_tasks_from` said (a role file written before the team, or for another, mustn't leave a member refusing the
/// coordinator it was started under: the Demo2 devserver). The roster is the person's: Start team's own.
pub fn in_team(mut role: Role, roster: Option<&[String]>) -> Role {
    const COORDINATOR: &str = "coordinator";
    let Some(roster) = roster else { return role };
    if role.name != COORDINATOR && roster.iter().any(|r| r == COORDINATOR) {
        if !role.takes_tasks_from.iter().any(|t| t == COORDINATOR) {
            role.takes_tasks_from.push(COORDINATOR.into());
        }
        role.reports_to.get_or_insert_with(|| COORDINATOR.into());
    }
    role
}

/// The teams `roles` can start: those whose every member's role exists (the others become problems).
pub fn usable_teams(teams: Vec<Team>, roles: &[Role]) -> (Vec<Team>, Vec<String>) {
    let (mut ok, mut problems) = (Vec::new(), Vec::new());
    for team in teams {
        match team.check(roles) {
            Ok(()) => ok.push(team),
            Err(problem) => problems.push(problem),
        }
    }
    (ok, problems)
}

/// The teams a person may start in `root`, as a session there would load them (the project's only once trusted, and
/// only those whose members' roles exist there), and the team files that couldn't be loaded or used: the desktop's
/// What a harness member (Claude Code; the harness plan, phase 3) is told at start (`--append-system-prompt`): its
/// role's prompt and its team's brief, the same sections a Marv member's system prompt has (`crate::sdk`), bounded the
/// same way (`member_role`, `in_team`), then [`harness_team_note`] for its agent (`agent`: "Claude Code",
/// "OpenCode", …; `tools`: how that agent sees Marv's tools). Err: the role isn't found here.
#[allow(clippy::too_many_arguments)] // The chat's team facts, as `ChatSpec` carries them.
pub fn harness_brief(
    root: &Path,
    home: &Path,
    config_dir: &Path,
    role: &str,
    role_writes: Option<&[String]>,
    team: Option<(&str, &[String])>,
    team_writes: &[Option<Vec<String>>],
    peers: &[TeamPeer],
    agent: &str,
    tools: &str,
) -> Result<String, String> {
    let (teams, _, roles) = available_teams(root, home, config_dir);
    let found = bounded_role(&roles, role, role_writes, team.map(|(_, roster)| roster))?;
    let mut out = found.prompt();
    if let Some(mut team) = team.and_then(|(name, roster)| team_for_chat(&teams, name, roster)) {
        team.describe_members(&roles, team_writes);
        team.peers = peers.to_vec();
        out.push_str("\n\n");
        out.push_str(&team.brief());
    }
    out.push_str(&harness_team_note(agent, tools));
    Ok(out)
}

/// Role `name` as a team member runs it: bounded by its typed write paths and the coordinator's tasks (`crate::sdk`).
fn bounded_role(roles: &[Role], name: &str, writes: Option<&[String]>, roster: Option<&[String]>) -> Result<Role, String> {
    let found = crate::roles::find_role(roles, name).cloned().ok_or_else(|| crate::roles::unknown_role(roles, name))?;
    Ok(in_team(member_role(found, writes)?, roster))
}

/// A harness member's role as its turns are judged (who it takes tasks from: `crate::task_report::inbox_turn_for`).
pub fn harness_role(
    root: &Path,
    home: &Path,
    config_dir: &Path,
    name: &str,
    writes: Option<&[String]>,
    roster: Option<&[String]>,
) -> Result<Role, String> {
    let (_, _, roles) = available_teams(root, home, config_dir);
    bounded_role(&roles, name, writes, roster)
}

/// Under a harness member's brief (consistency findings H14, H20): which agent it is (never another's name), which
/// of the Marv tools the brief names it has (all of Marv OS's own, through Marv: `mesh::harness_tools`), and what
/// holds it to its role: Marv's sandbox for its write paths when it runs in one (H6), its own word for the rest.
pub fn harness_team_note(agent: &str, tools: &str) -> String {
    format!(
        "\n\nYou are {agent}, run by Marv OS as this team member. Of the Marv tools named above you have mesh_list, mesh_send, mesh_wait, board_read, board_post, board_clear, chat_read and wake_me ({tools}); use your own tools for the rest. Marv's sandbox holds you to your role's write paths when it runs you in one; keep to the rest of your role's bounds yourself. When a teammate gives you a task, end your turn with the result: Marv sends it back to them."
    )
}

/// New team dialog and `marv team ls`.
pub fn available_teams(root: &Path, home: &Path, config_dir: &Path) -> (Vec<Team>, Vec<String>, Vec<Role>) {
    let read = crate::project_trust::load_project(root, home, config_dir, true, true, None);
    let mut problems = read.team_problems;
    let (teams, unusable) = usable_teams(read.teams, &read.roles);
    problems.extend(unusable);
    (teams, problems, read.roles)
}

/// The error for a team name nobody defined.
pub fn unknown_team(teams: &[Team], name: &str) -> String {
    let names: Vec<&str> = teams.iter().map(|t| t.name.as_str()).collect();
    format!(
        "There's no team named \"{}\". Teams here: {}. A team is .marv/team.md (the project's, once trusted) or ~/.marv/teams/<name>.md.",
        marv_core::printable(name),
        names.join(", ")
    )
}

/// A YAML string as written in a file we write: plain when it's a simple word, else JSON-quoted (valid YAML).
fn yaml_str(text: &str) -> String {
    if !text.is_empty() && text.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./".contains(&b)) {
        text.to_string()
    } else {
        serde_json::to_string(text).unwrap_or_default()
    }
}

/// The text of `.marv/team.md` for `team` ("Save as team file"): what [`parse_team`] reads back.
pub fn render_team_file(team: &Team) -> String {
    let mut out = format!("---\nname: {}\ndescription: {}\nmembers:\n", team.name, yaml_str(&team.description));
    for m in &team.members {
        out.push_str(&format!("  - role: {}\n", m.role));
        if let Some(model) = &m.model {
            out.push_str(&format!("    model: {}\n", yaml_str(model)));
        }
    }
    out.push_str(&format!("worktrees: {}\nread_chats: {}\n---\n", team.worktrees, team.read_chats));
    if !team.body.is_empty() {
        out.push_str(&format!("\n{}\n", team.body));
    }
    out
}

/// The text of `.marv/roles/<name>.md` for a built-in `role` with the person's `writes` ("Save as team file"): its
/// bounds and instructions, now the project's to change. What `crate::roles::parse_role` reads back.
pub fn render_role_file(role: &Role, writes: Option<&[String]>) -> String {
    let mut out = format!("---\nname: {}\ndescription: {}\n", role.name, yaml_str(&role.description));
    match &role.tools {
        crate::roles::RoleTools::All => {}
        crate::roles::RoleTools::Only(list) => out.push_str(&format!("tools: [{}]\n", list.join(", "))),
        // A file lists what it may use: every known built-in tool but the excepted ones.
        crate::roles::RoleTools::AllBut(except) => {
            let mut names: Vec<String> = marv_tools::builtin_tools().iter().map(|t| t.name().to_string()).collect();
            names.extend(marv_mesh::tools::mesh_specs().iter().map(|s| s.name.to_string()));
            names.extend(marv_mesh::board::board_specs().iter().map(|s| s.name.to_string()));
            names.extend(["herdr", "chat_read", "wake_me", "tool_search", "mcp_call"].map(str::to_string));
            names.retain(|n| !except.contains(n));
            names.dedup();
            out.push_str(&format!("tools: [{}]\n", names.join(", ")));
        }
    }
    let writes = writes.map(<[String]>::to_vec).or_else(|| role.writes.clone());
    if let Some(list) = writes {
        out.push_str(&format!("writes: [{}]\n", list.iter().map(|w| yaml_str(w)).collect::<Vec<_>>().join(", ")));
        if !role.tool_cache.is_empty() {
            out.push_str(&format!(
                "tool_cache: [{}]\n",
                role.tool_cache.iter().map(|w| yaml_str(w)).collect::<Vec<_>>().join(", ")
            ));
        }
    }
    if !role.takes_tasks_from.is_empty() {
        out.push_str(&format!("takes_tasks_from: [{}]\n", role.takes_tasks_from.join(", ")));
    }
    if let Some(to) = &role.reports_to {
        out.push_str(&format!("reports_to: {to}\n"));
    }
    out.push_str("---\n");
    if !role.body.is_empty() {
        out.push_str(&format!("\n{}\n", role.body));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roles::merge_roles;

    #[test]
    fn every_member_takes_the_coordinators_tasks_and_the_brief_lists_what_each_can_do() {
        let mut role = crate::roles::devserver();
        role.takes_tasks_from = vec!["someone".into()];
        role.reports_to = None;
        let roster = vec!["coordinator".to_string(), "devserver".to_string()];
        let role = in_team(role, Some(&roster));
        assert!(role.takes_tasks_from("chat-1", Some("coordinator")), "{:?}", role.takes_tasks_from);
        assert_eq!(role.reports_to.as_deref(), Some("coordinator"));
        assert!(!in_team(crate::roles::coordinator(), Some(&roster)).takes_tasks_from("x", Some("coordinator")));
        let alone = in_team(crate::roles::devserver(), None);
        assert_eq!(alone.takes_tasks_from, vec!["coordinator".to_string()]);
        let mut team = parse(WEB).unwrap();
        team.describe_members(&crate::roles::builtin_roles(), &[]);
        let brief = team.brief();
        assert!(
            brief.contains("- devserver: can't edit files") && brief.contains("- frontend: edits anywhere in the project"),
            "{brief}"
        );
    }

    fn parse(text: &str) -> Result<Team, String> {
        parse_team(text, "team.md", AgentSource::Project, ".marv/team.md")
    }

    const WEB: &str = "---\nname: web\ndescription: Coordinator + frontend + backend + dev server\nmembers:\n  - role: coordinator\n  - role: frontend\n    model: inherit\n  - role: backend\n  - role: devserver\n    model: small\nworktrees: false\nread_chats: true\n---\nThe site is in site/; keep it static.";

    #[test]
    fn the_specs_team_file_parses() {
        let team = parse(WEB).unwrap();
        assert_eq!(team.name, "web");
        assert_eq!(team.roster(), "coordinator, frontend, backend, devserver");
        assert_eq!(team.members[1].model.as_deref(), Some("inherit"));
        assert_eq!(team.members[3].model.as_deref(), Some("small"));
        assert_eq!(team.members[0].model, None);
        assert!(!team.worktrees && team.read_chats);
        let brief = team.brief();
        assert!(brief.starts_with("# Your team: web\n"), "{brief}");
        assert!(brief.contains("coordinator, frontend, backend, devserver") && brief.ends_with("keep it static."), "{brief}");
        assert_eq!(team.brief(), brief, "byte-stable");
    }

    #[test]
    fn defaults_and_bare_role_names() {
        let team = parse("---\ndescription: d\nmembers: [worker, reviewer]\n---\n").unwrap();
        assert_eq!(team.name, "team");
        assert!(!team.worktrees && team.read_chats, "shared tree and read by default");
        assert_eq!(team.roster(), "worker, reviewer");
        assert_eq!(team.body, "");
        assert!(!team.brief().contains("\n\n\n"));
    }

    #[test]
    fn bad_team_files_are_problems_not_guesses() {
        for (text, says) in [
            ("---\nname: t\nmembers: [worker]\n---\n", "needs a description"),
            ("---\ndescription: d\n---\n", "needs members"),
            ("---\ndescription: d\nmembers: []\n---\n", "1 to 8 members"),
            ("---\ndescription: d\nmembers: [a, b, c, d, e, f, g, h, i]\n---\n", "1 to 8 members"),
            ("---\ndescription: d\nmembers:\n  - role: Not A Role\n---\n", "isn't a role name"),
            ("---\ndescription: d\nmembers:\n  - role: worker\n    model: two words\n---\n", "isn't a model id"),
            ("---\ndescription: d\nmembers:\n  - role: worker\n    model: 3\n---\n", "inherit, small or a model id"),
            ("---\ndescription: d\nmembers: [worker]\nworktrees: maybe\n---\n", "true or false"),
            ("---\nname: Bad\ndescription: d\nmembers: [worker]\n---\n", "lowercase"),
            ("no frontmatter", "frontmatter"),
        ] {
            let err = parse(text).unwrap_err();
            assert!(err.contains(says), "{text}: {err}");
        }
    }

    #[test]
    fn the_built_in_templates_start_with_the_built_in_roles() {
        let roles = merge_roles(Vec::new(), Vec::new());
        let teams = builtin_teams();
        let titles: Vec<&str> = teams.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["Web app", "Build + review", "Pair"]);
        assert_eq!(teams[0].roster(), "coordinator, frontend, backend, devserver");
        assert_eq!(teams[1].roster(), "coordinator, worker, reviewer");
        assert_eq!(teams[2].roster(), "worker, reviewer");
        for team in &teams {
            assert_eq!(team.check(&roles), Ok(()), "{}", team.name);
            assert!(!team.worktrees && team.read_chats);
        }
    }

    /// H14, H20: a harness member is told which agent it is (an OpenCode member is never told it's Claude Code), how
    /// it sees Marv's tools, and that it has the board and `chat_read` its team's habits name.
    #[test]
    fn a_harness_members_note_names_its_own_agent_and_its_tools() {
        let note = harness_team_note("OpenCode", "the marv-mesh tools");
        assert!(note.contains("You are OpenCode,") && !note.contains("Claude Code"), "{note}");
        assert!(note.contains("(the marv-mesh tools)") && !note.contains("mcp__"), "{note}");
        for tool in ["board_read", "board_post", "chat_read", "wake_me"] {
            assert!(note.contains(tool), "{tool}");
        }
        assert!(harness_team_note("Claude Code", "the mcp__marv-mesh tools").contains("You are Claude Code,"));
    }

    #[test]
    fn the_brief_names_each_teammates_inbox_and_says_others_arent_the_team() {
        let mut team = builtin_teams().remove(0);
        team.peers = vec![
            TeamPeer { role: "frontend".into(), title: "game-frontend".into(), mesh_name: "game-1a2b3c4d".into() },
            // Not plain: never in the prompt or an inbox name.
            TeamPeer { role: "backend".into(), title: "x".into(), mesh_name: "Bad Name".into() },
        ];
        let brief = team.brief();
        assert!(brief.contains("- frontend (\"game-frontend\"): peer.game-1a2b3c4d.agent.parent.inbox"), "{brief}");
        assert!(brief.contains("isn't on your team"));
        assert!(!brief.contains("Bad Name"));
        assert!(brief.contains(TEAM_HABITS) && TEAM_HABITS.contains("at most 280 characters"));
        team.peers.clear();
        assert!(!team.brief().contains("inboxes"), "nothing said without the names");
    }

    #[test]
    fn a_chats_team_takes_the_roster_it_was_started_with() {
        let mut teams = builtin_teams();
        teams[0].body = "Brief.".into();
        let roster = vec!["coordinator".to_string(), "frontend".to_string()];
        let web = team_for_chat(&teams, "web", &roster).unwrap();
        assert_eq!((web.roster().as_str(), web.body.as_str()), ("coordinator, frontend", "Brief."));
        assert_eq!(team_for_chat(&teams, "web", &[]).unwrap().roster(), "coordinator, frontend, backend, devserver");
        let renamed = team_for_chat(&teams, "acme", &roster).unwrap();
        assert_eq!((renamed.name.as_str(), renamed.body.as_str()), ("acme", ""));
        assert!(renamed.brief().contains("coordinator, frontend"));
        assert_eq!(team_for_chat(&teams, "acme", &[]), None);
        assert_eq!(team_for_chat(&teams, "Not a name", &roster), None);
        assert_eq!(team_for_chat(&teams, "web", &["x y".to_string()]), None);
    }

    #[test]
    fn a_members_typed_write_paths_replace_its_roles_and_never_widen_a_role_that_writes_nothing() {
        let front = member_role(crate::roles::frontend(), Some(&["index.html".to_string(), "./css/".to_string()])).unwrap();
        assert_eq!(front.writes, Some(vec!["index.html".into(), "css".into()]));
        assert!(front.prompt().contains("only under index.html, css"));
        assert_eq!(member_role(crate::roles::frontend(), Some(&[])).unwrap().writes, None, "none typed: the role's own");
        assert_eq!(member_role(crate::roles::frontend(), None).unwrap().writes, None);
        assert!(member_role(crate::roles::devserver(), Some(&["src".to_string()])).unwrap_err().contains("changes no files"));
        assert!(member_role(crate::roles::frontend(), Some(&["../x".to_string()])).unwrap_err().contains("climbs out"));
        let dev = crate::roles::devserver();
        assert!(!dev.tools.permits("edit_file") && dev.tools.permits("process") && dev.tools.permits("bash"));
        assert_eq!(dev.writes, Some(Vec::new()));
    }

    #[test]
    fn the_briefs_lines_describe_each_members_effective_role() {
        let mut team = parse(WEB).unwrap();
        let roster: Vec<String> = team.members.iter().map(|m| m.role.clone()).collect();
        // Typed write paths for the frontend (and a role file that took tasks from someone else).
        let mut roles = crate::roles::builtin_roles();
        roles.iter_mut().find(|r| r.name == "backend").unwrap().takes_tasks_from = vec!["someone".into()];
        let writes: Vec<Option<Vec<String>>> =
            roster.iter().map(|r| (r == "frontend").then(|| vec!["site".to_string(), "css".to_string()])).collect();
        team.describe_members(&roles, &writes);
        let notes = team.member_notes.join("\n");
        assert!(
            notes.contains("frontend: edits only under site, css") && notes.contains("writing only under site, css"),
            "{notes}"
        );
        assert!(notes.contains("backend: edits anywhere in the project"), "{notes}");
        assert!(
            notes.contains("backend:")
                && notes.lines().find(|l| l.starts_with("backend")).unwrap().contains("takes tasks from someone, coordinator"),
            "{notes}"
        );
        assert!(notes.contains("devserver: can't edit files") && notes.contains("coordinator: can't edit files"), "{notes}");
        // Who a read-only chat is told to ask: the typed set, not the role's.
        assert!(team.editors.contains(&("frontend".to_string(), Some(vec!["site".to_string(), "css".to_string()]))));
        assert!(team.editors.contains(&("backend".to_string(), None)));
        // A set that can't apply (a role that writes nothing) leaves the role's own.
        let bad: Vec<Option<Vec<String>>> = roster.iter().map(|_| Some(vec!["src".to_string()])).collect();
        team.describe_members(&roles, &bad);
        assert!(team.member_notes.join("\n").contains("devserver: can't edit files"));
        assert!(team.member_notes.join("\n").contains("frontend: edits only under src"));
    }

    #[test]
    fn a_member_whose_role_is_missing_is_a_problem() {
        let team = parse("---\nname: x\ndescription: d\nmembers: [coordinator, designer]\n---\n").unwrap();
        let (ok, problems) = usable_teams(vec![team], &merge_roles(Vec::new(), Vec::new()));
        assert!(ok.is_empty());
        assert_eq!(problems.len(), 1);
        assert!(
            problems[0].contains("no role named \"designer\"") && problems[0].contains("Roles here: backend"),
            "{problems:?}"
        );
    }

    #[test]
    fn files_replace_built_ins_and_the_project_wins() {
        let mine =
            parse_team("---\nname: web\ndescription: Mine.\nmembers: [worker]\n---\n", "web.md", AgentSource::Personal, "~")
                .unwrap();
        let other =
            parse_team("---\nname: solo\ndescription: S.\nmembers: [worker]\n---\n", "solo.md", AgentSource::Personal, "~")
                .unwrap();
        let project = parse("---\nname: solo\ndescription: Ours.\nmembers: [worker]\n---\n").unwrap();
        let teams = merge_teams(vec![mine, other], Some(project));
        let names: Vec<&str> = teams.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["build-review", "pair", "solo", "web"]);
        assert_eq!(find_team(&teams, "web").unwrap().description, "Mine.");
        assert_eq!(find_team(&teams, "solo").unwrap().source, AgentSource::Project);
        assert!(unknown_team(&teams, "nope").contains("build-review, pair, solo, web"));
    }

    #[test]
    fn a_saved_team_file_and_role_file_read_back() {
        let mut team = builtin_teams().remove(0);
        team.description = "Web: \"quoted\" and: colons".into();
        team.body = "Keep it static.".into();
        let text = render_team_file(&team);
        let back = parse(&text).unwrap();
        assert_eq!((back.name.as_str(), back.description.as_str()), ("web", team.description.as_str()));
        assert_eq!(back.members, team.members);
        assert_eq!(back.body, "Keep it static.");
        for role in crate::roles::builtin_roles() {
            let text = render_role_file(&role, Some(&["site/css".to_string(), "index.html".to_string()]));
            let parsed =
                crate::roles::parse_role(&text, "r.md", AgentSource::Project, "r.md").unwrap_or_else(|e| panic!("{e}\n{text}"));
            assert_eq!(parsed.name, role.name);
            assert_eq!(parsed.writes, Some(vec!["site/css".into(), "index.html".into()]));
            assert_eq!(parsed.takes_tasks_from, role.takes_tasks_from);
            for tool in crate::roles::FILE_TOOLS {
                assert_eq!(parsed.tools.permits(tool), role.tools.permits(tool), "{} {tool}", role.name);
            }
        }
    }

    #[test]
    fn personal_team_files_load_and_bad_ones_are_problems() {
        let home = std::env::temp_dir().join(format!("marv-teams-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(teams_dir(&home)).unwrap();
        std::fs::write(teams_dir(&home).join("duo.md"), "---\ndescription: Two.\nmembers: [worker, reviewer]\n---\n").unwrap();
        std::fs::write(teams_dir(&home).join("broken.md"), "nope").unwrap();
        let (teams, problems) = load_personal_teams(Some(&home));
        assert_eq!(teams.len(), 1);
        assert_eq!((teams[0].name.as_str(), teams[0].source), ("duo", AgentSource::Personal));
        assert_eq!(problems.len(), 1);
        assert!(problems[0].starts_with("~/.marv/teams/broken.md"), "{problems:?}");
        let _ = std::fs::remove_dir_all(&home);
    }
}
