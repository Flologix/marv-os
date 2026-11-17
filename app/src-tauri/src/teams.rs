//! One-step team setup (the New team dialog, `marv_session::teams`): what the page sends to start a team, the order a
//! start goes in, and how a team already started is found again (its rules' labels, its chats' records).
//!
//! A start, in order (each step's result goes back to the page): the folder rule(s) (`allow_folder`'s, labelled
//! "Team <name>"), the mesh's wake setting (global in `~/.marv/config.json`: "off" becomes "rules", and the person is
//! told), each chat with its role and model (the coordinator first), worktrees when asked, and grants when the person
//! ticked "keep working while I'm away". No chat made: the rules go again, so nothing is left half made.
//! [`TeamHost`] is the desktop (tests use a fake).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::store::{Chat, ChatTeam};

pub use marv_session::teams::{read_rule_label as read_label, rule_label, team_of_rule_label as team_of_label};

/// One member as the dialog sends it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberSpec {
    pub role: String,
    /// The chat's name (default `<folder>-<role>`).
    pub title: String,
    /// `inherit` / none (the default model), `small` (the person's small model) or a model id.
    #[serde(default)]
    pub model: Option<String>,
    /// The write paths the person typed (used by "Save as team file"; a start uses the role's own).
    #[serde(default)]
    pub writes: Option<Vec<String>>,
    /// What runs it (the harness plan, phase 3): none for Marv, `claude-code` for the person's own Claude Code.
    #[serde(default)]
    pub harness: Option<String>,
}

/// A team as the dialog sends it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamSpec {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub members: Vec<MemberSpec>,
    #[serde(default)]
    pub worktrees: bool,
    #[serde(default)]
    pub read_chats: bool,
    #[serde(default)]
    pub keep_working: bool,
    /// The page's chat settings: each member starts with them (its own model).
    pub default_model: String,
    pub max_steps: u32,
    pub yolo: bool,
}

/// One step of a start, for the page's progress list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub what: String,
    pub ok: bool,
    pub detail: String,
}

/// What a start did.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Started {
    pub steps: Vec<Step>,
    /// The chats made, coordinator first.
    pub chats: Vec<Chat>,
    pub rules: Vec<u64>,
    /// Whether the team is up (a rule and at least one chat).
    pub ok: bool,
}

/// What a start needs from the desktop.
pub trait TeamHost {
    /// The roles a chat in `folder` may start as.
    fn roles(&self, folder: &Path) -> Vec<marv_session::roles::Role>;
    /// The teams already started in `folder` (a second with the same name is refused).
    fn existing_teams(&self, folder: &Path) -> Vec<String>;
    /// The other teams started in `folder` and their live rules' ids: a new start ends them (their rules are revoked),
    /// since the folder rule covers every chat in it and two teams' wires would mix.
    fn other_teams(&self, _folder: &Path) -> Vec<(String, Vec<u64>)> {
        Vec::new()
    }
    /// The folder rule (and the read rule): their ids, both or neither.
    async fn make_rules(&self, folder: &Path, name: &str, read: bool) -> Result<Vec<u64>, String>;
    async fn revoke_rules(&self, ids: &[u64]) -> Result<(), String>;
    /// Makes sure a rule's wire wakes a chat: `Some(note)` when the setting was changed.
    fn ensure_wake(&self) -> Result<Option<String>, String>;
    /// Puts the wake setting back to "never" (a start that made nothing undoes its change).
    fn restore_wake(&self) -> Result<(), String>;
    /// The model `small` means (the person's small model), if they set one.
    fn small_model(&self) -> Option<String>;
    /// Makes one chat (saved, not started).
    fn create_chat(&self, folder: &Path, member: &NewMember) -> Result<Chat, String>;
    /// Each chat in its own worktree: a note for the person.
    async fn worktrees(&self, chats: &[Chat]) -> Result<Option<String>, String>;
    /// A rule for each pair of the chats' worktrees, and each worktree with the folder ([`worktree_pairs`]), labelled for
    /// the team like its folder rule: a member in its worktree is a different folder from the one the team's rule
    /// covers, so without these its wires asked, and mail to it didn't wake it. The rules' ids; none when no chat has a
    /// worktree. All or none.
    async fn cover_worktrees(&self, _folder: &Path, _name: &str, _read: bool, _chats: &[Chat]) -> Result<Vec<u64>, String> {
        Ok(Vec::new())
    }
    /// "Keep working while I'm away": a grant for each chat.
    async fn grant(&self, chats: &[Chat], worktrees: bool) -> Result<Option<String>, String>;
}

/// One chat a start makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMember {
    pub title: String,
    pub role: String,
    pub team: ChatTeam,
    pub model: String,
    /// The write paths the person typed, checked (`marv_session::teams::member_role`): the chat's write set from its
    /// start, in place of its role's. None: the role's own.
    pub writes: Option<Vec<String>>,
    pub max_steps: u32,
    pub yolo: bool,
    /// What runs it: None for Marv, `claude-code` (phase 3).
    pub harness: Option<String>,
}

/// A member's typed write paths, checked against its role: None (the role's own) when it typed none.
pub fn member_writes(m: &MemberSpec, roles: &[marv_session::roles::Role]) -> Result<Option<Vec<String>>, String> {
    let role = marv_session::roles::find_role(roles, &m.role)
        .ok_or_else(|| format!("There's no role named \"{}\" here.", marv_core::printable(&m.role)))?;
    let typed: Vec<String> = m.writes.iter().flatten().map(|w| w.trim().to_string()).filter(|w| !w.is_empty()).collect();
    if typed.is_empty() {
        return Ok(None);
    }
    let bounded = marv_session::teams::member_role(role.clone(), Some(&typed)).map_err(|e| format!("{}: {e}", m.title.trim()))?;
    Ok(bounded.writes)
}

/// Why `spec` can't start in a folder whose roles are `roles`, if it can't (checked before anything is made).
pub fn check(spec: &TeamSpec, roles: &[marv_session::roles::Role]) -> Result<(), String> {
    let names: Vec<String> = roles.iter().map(|r| r.name.clone()).collect();
    let roles_named = &names;
    if !marv_session::skills::valid_name(&spec.name) {
        return Err(format!("\"{}\" isn't a team name: lowercase letters, digits and dashes.", marv_core::printable(&spec.name)));
    }
    if spec.members.is_empty() || spec.members.len() > marv_session::teams::MAX_MEMBERS {
        return Err(format!("A team has 1 to {} members.", marv_session::teams::MAX_MEMBERS));
    }
    let mut titles = std::collections::BTreeSet::new();
    for m in &spec.members {
        if !roles_named.iter().any(|r| r == &m.role) {
            return Err(format!(
                "There's no role named \"{}\" here. Roles here: {}.",
                marv_core::printable(&m.role),
                roles_named.join(", ")
            ));
        }
        member_writes(m, roles)?;
        let title = m.title.trim();
        if title.is_empty() {
            return Err(format!("The {} chat needs a name.", m.role));
        }
        if !titles.insert(title.to_string()) {
            return Err(format!("Two chats are named \"{}\": give each its own name.", marv_core::printable(title)));
        }
    }
    Ok(())
}

/// The members in the order they start: the coordinator(s) first, the rest as listed.
pub fn start_order(members: &[MemberSpec]) -> Vec<&MemberSpec> {
    let (first, rest): (Vec<&MemberSpec>, Vec<&MemberSpec>) = members.iter().partition(|m| m.role == "coordinator");
    first.into_iter().chain(rest).collect()
}

/// The model a member starts with.
pub fn member_model(model: Option<&str>, default: &str, small: Option<&str>) -> String {
    match model.map(str::trim) {
        None | Some("") | Some("inherit") => default.to_string(),
        Some("small") => small.unwrap_or(default).to_string(),
        Some(id) => id.to_string(),
    }
}

/// Starts `spec` in `folder` ([`TeamHost`]), in order; see the module doc.
pub async fn start(host: &impl TeamHost, folder: &Path, spec: &TeamSpec) -> Result<Started, String> {
    let roles = host.roles(folder);
    check(spec, &roles)?;
    if host.existing_teams(folder).contains(&spec.name) {
        return Err(format!(
            "There's a team named {} in this folder already: end it first, or give this one another name.",
            spec.name
        ));
    }
    let mut out = Started::default();
    let step = |what: &str, ok: bool, detail: String| Step { what: what.to_string(), ok, detail };
    // The teams here before this start (a snapshot taken before the new rules exist; only those with live rules).
    let before: Vec<(String, Vec<u64>)> = host.other_teams(folder).into_iter().filter(|(_, ids)| !ids.is_empty()).collect();
    // 1. The rule(s): nothing is made without them.
    let rules = host.make_rules(folder, &spec.name, spec.read_chats).await?;
    let detail = if spec.read_chats {
        "The team's chats may message each other and read each other's chats.".to_string()
    } else {
        "The team's chats may message each other.".to_string()
    };
    out.steps.push(step("Folder rule", true, detail));
    out.rules = rules.clone();
    // 1b. A team started here before is ended: its rules are revoked, after the new ones exist (a failed start keeps it).
    // Never this team itself, nor a rule just made.
    for (name, old) in before {
        let old: Vec<u64> = old.into_iter().filter(|id| !rules.contains(id)).collect();
        if name == spec.name || old.is_empty() {
            continue;
        }
        let detail = match host.revoke_rules(&old).await {
            Ok(()) => step(
                "Previous team ended",
                true,
                format!(
                    "The team {name} started in this folder before is ended: its rules are revoked, so its chats no longer reach each other (they stay, as ordinary chats)."
                ),
            ),
            Err(e) => step(
                "Previous team ended",
                false,
                format!(
                    "The team {name}'s rules couldn't be revoked ({e}): revoke them under Advanced: rules, or its chats and this team's will share the folder rule."
                ),
            ),
        };
        out.steps.push(detail);
    }
    // 2. Wake on a rule's wire (a failure here leaves the team usable: said, not undone).
    let mut wake_changed = false;
    match host.ensure_wake() {
        Ok(note) => out.steps.push(step(
            "Wake on mail",
            true,
            note.inspect(|_| wake_changed = true).unwrap_or_else(|| "A teammate's message wakes an idle member.".to_string()),
        )),
        Err(e) => out.steps.push(step("Wake on mail", false, format!("Couldn't check the wake setting: {e}"))),
    }
    // 3. The chats, coordinator first.
    let small = host.small_model();
    let order = start_order(&spec.members);
    // Each member's typed write paths (checked), in the roster's order: every chat's brief describes its teammates by them.
    let mut typed = Vec::new();
    for m in &order {
        typed.push(member_writes(m, &roles)?);
    }
    let team = ChatTeam { name: spec.name.clone(), roster: order.iter().map(|m| m.role.clone()).collect(), writes: typed };
    for m in order {
        let model = member_model(m.model.as_deref(), &spec.default_model, small.as_deref());
        let writes = member_writes(m, &roles)?;
        let harness = m.harness.clone().filter(|h| !h.is_empty());
        let on = match &harness {
            // Every agent a chat can run (Codex too), not only the older harness list's.
            Some(name) => marv_web::terminal_agents::find(name).map_or(name.as_str(), |a| a.title).to_string(),
            None => model.clone(),
        };
        let shown = match &writes {
            Some(list) => format!("as {}, on {on}, writes only {}", m.role, list.join(", ")),
            None => format!("as {}, on {on}", m.role),
        };
        let member = NewMember {
            title: m.title.trim().to_string(),
            role: m.role.clone(),
            team: team.clone(),
            model: model.clone(),
            writes,
            max_steps: spec.max_steps,
            yolo: spec.yolo,
            harness,
        };
        match host.create_chat(folder, &member) {
            Ok(chat) => {
                out.steps.push(step(&format!("Chat {}", m.title.trim()), true, shown));
                out.chats.push(chat);
            }
            Err(e) => out.steps.push(step(&format!("Chat {}", m.title.trim()), false, e)),
        }
    }
    if out.chats.is_empty() {
        let undone = host.revoke_rules(&rules).await;
        out.steps.push(match undone {
            Ok(()) => step("Undo", true, "No chat could be made, so the rule was taken back.".to_string()),
            Err(e) => step(
                "Undo",
                false,
                format!("No chat could be made, and the rule couldn't be taken back ({e}): revoke it under Advanced: rules."),
            ),
        });
        out.rules.clear();
        if wake_changed {
            out.steps.push(match host.restore_wake() {
                Ok(()) => step("Undo", true, "Wake on mail is back to \"never\", as it was.".to_string()),
                Err(e) => {
                    step("Undo", false, format!("Wake on mail couldn't be set back to \"never\" ({e}): Teams → Mesh changes it."))
                }
            });
        }
        return Ok(out);
    }
    // 4. Worktrees (when asked, and no grant will place them).
    if spec.worktrees && !spec.keep_working {
        match host.worktrees(&out.chats).await {
            Ok(note) => {
                out.steps.push(step("Worktrees", true, note.unwrap_or_else(|| "Each chat works in its own worktree.".into())))
            }
            Err(e) => out.steps.push(step("Worktrees", false, format!("{e} They work in the shared folder."))),
        }
    }
    // 5. Keep working while you're away (a grant each; it places the worktrees too).
    if spec.keep_working {
        match host.grant(&out.chats, spec.worktrees).await {
            Ok(note) => out.steps.push(step(
                "Keep working while you're away",
                true,
                note.unwrap_or_else(|| "Each chat may run what yolo would on a teammate's message, within $2 a day.".into()),
            )),
            Err(e) => out.steps.push(step("Keep working while you're away", false, e)),
        }
    }
    // 6. Members in worktrees are other folders than the one the rule covers: the rule is made for them too.
    if spec.worktrees {
        match host.cover_worktrees(folder, &spec.name, spec.read_chats, &out.chats).await {
            Ok(ids) if ids.is_empty() => {}
            Ok(ids) => {
                out.rules.extend(ids);
                out.steps.push(step(
                    "Worktree rules",
                    true,
                    "The team's chats in their worktrees may message each other.".into(),
                ));
            }
            Err(e) => out.steps.push(step(
                "Worktree rules",
                false,
                format!("{e} Members in worktrees will ask before each wire: allow the team's worktrees under Advanced: rules."),
            )),
        }
    }
    out.ok = true;
    Ok(out)
}

/// The folder pairs a team in worktrees needs rules for: each worktree with every other (`worktrees` are the members'
/// own, never a folder that merely sits beside them), and each with the team's `folder`, for a member that stayed there
/// (the folder with itself is the folder rule's). The first of a pair is always a worktree: [`rule_home`] finds the
/// team by it.
pub fn worktree_pairs(folder: &Path, worktrees: &[PathBuf]) -> Vec<(PathBuf, PathBuf)> {
    let mut pairs = Vec::new();
    for (i, a) in worktrees.iter().enumerate() {
        if a == folder || worktrees[..i].contains(a) {
            continue;
        }
        pairs.push((a.clone(), folder.to_path_buf()));
        pairs.extend(worktrees[i + 1..].iter().filter(|b| *b != folder && *b != a).map(|b| (a.clone(), b.clone())));
    }
    pairs
}

/// The folder a rule made for a team is listed under (the Team view finds a team's rules by it): the rule's own folder,
/// or, when that is one of the chats' worktrees, the folder of the chat it was made for. `worktrees`: (the worktree,
/// its chat's folder).
pub fn rule_home(rule_folder: &Path, worktrees: &[(PathBuf, PathBuf)]) -> PathBuf {
    worktrees.iter().find(|(wt, _)| wt == rule_folder).map_or_else(|| rule_folder.to_path_buf(), |(_, home)| home.clone())
}

/// A team started in a folder, as the Team view finds it again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamHere {
    pub name: String,
    /// Its live rules' ids (the send rule first).
    pub rules: Vec<u64>,
    /// Its chats' ids, in the order they started.
    pub chats: Vec<String>,
}

/// The teams in `folder`: chats whose records say a team, with the rules labelled for it whose scope is `folder`.
/// `rules`: (id, label, the folder it covers); `chats`: (id, its folder, its team).
pub fn teams_here(
    folder: &Path,
    rules: &[(u64, String, std::path::PathBuf)],
    chats: &[(String, std::path::PathBuf, Option<String>)],
) -> Vec<TeamHere> {
    fn entry<'a>(out: &'a mut Vec<TeamHere>, name: &str) -> &'a mut TeamHere {
        match out.iter().position(|t| t.name == name) {
            Some(i) => &mut out[i],
            None => {
                out.push(TeamHere { name: name.to_string(), rules: Vec::new(), chats: Vec::new() });
                out.last_mut().expect("just pushed")
            }
        }
    }
    let mut out: Vec<TeamHere> = Vec::new();
    for (id, dir, team) in chats {
        if let Some(name) = team.as_deref().filter(|_| dir == folder) {
            entry(&mut out, name).chats.push(id.clone());
        }
    }
    for (id, label, dir) in rules {
        if let Some(name) = team_of_label(label).filter(|_| dir == folder) {
            entry(&mut out, name).rules.push(*id);
        }
    }
    out
}

/// The Team view's reports: for each chat (a lead), the latest report-back each sender put in its main inbox this
/// run, keyed by the sender the switchboard stamped (`marv_mesh::switchboard::ReportSink`): a line of text claiming
/// to be someone's report, inside another message or a tool's output, is never one.
#[derive(Clone, Default)]
pub struct TeamReports(
    std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, std::collections::HashMap<String, String>>>>,
);

impl TeamReports {
    /// The sink for chat `lead`'s switchboard.
    pub fn sink(&self, lead: &str) -> marv_mesh::switchboard::ReportSink {
        let (map, lead) = (self.0.clone(), lead.to_string());
        std::sync::Arc::new(move |sender: &marv_mesh::PlugId, text: &str| {
            let mut map = map.lock().unwrap_or_else(|e| e.into_inner());
            map.entry(lead.clone()).or_default().insert(sender.to_string(), text.chars().take(2000).collect());
        })
    }

    /// For each of `members` (chat id, mesh name): its latest report to `lead`, as "<kind>: <what>" or just "<kind>"
    /// for a report of Marv's fixed words. The switchboard tells the sink only stamped reports (`Envelope::report`),
    /// keyed by the stamped sender (`peer-<name>`); the text is only parsed here for display.
    pub fn latest(&self, lead: &str, members: &[(String, String)]) -> Vec<(String, String)> {
        let map = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let Some(got) = map.get(lead) else { return Vec::new() };
        members
            .iter()
            .filter_map(|(chat, name)| {
                let text = got.get(&format!("peer-{name}"))?;
                report_line(text, name).map(|line| (chat.clone(), line))
            })
            .collect()
    }
}

/// A report's text from Marv `name` as one printable line: "<kind>: <what>", or "<kind>" when it carries only Marv's
/// fixed words. None when the text doesn't start as `name`'s report.
pub fn report_line(text: &str, name: &str) -> Option<String> {
    let rest = text.strip_prefix(&format!("{}{name} (", marv_mesh::message::REPORT_PREFIX))?;
    let line = rest.lines().next().unwrap_or_default();
    let shown = match line.split_once("): ") {
        Some((kind, what)) if !what.trim().is_empty() => format!("{kind}: {}", what.trim()),
        Some((kind, _)) => kind.to_string(),
        None => line.trim_end().strip_suffix(')')?.to_string(),
    };
    Some(marv_core::printable(&shown.chars().take(300).collect::<String>()))
}

/// A file "Save as team file" writes: its project-relative path and text.
pub type SavedFile = (String, String);

/// The files "Save as team file" writes, each project-relative with its text, and the role files it leaves alone (one
/// is there already). `existing_roles`: the role files the project has (`.marv/roles/<name>.md`).
pub fn files_to_save(
    spec: &TeamSpec,
    roles: &[marv_session::roles::Role],
    body: &str,
    existing_roles: &[String],
) -> Result<(Vec<SavedFile>, Vec<String>), String> {
    use marv_session::teams::{Team, TeamMember, render_role_file, render_team_file};
    let team = Team {
        name: spec.name.clone(),
        title: spec.name.clone(),
        description: spec
            .description
            .clone()
            .filter(|d| !d.trim().is_empty())
            .unwrap_or_else(|| format!("The {} team.", spec.name)),
        members: spec
            .members
            .iter()
            .map(|m| TeamMember { role: m.role.clone(), model: m.model.clone().filter(|x| !x.trim().is_empty()) })
            .collect(),
        worktrees: spec.worktrees,
        read_chats: spec.read_chats,
        body: body.to_string(),
        member_notes: Vec::new(),
        editors: Vec::new(),
        peers: Vec::new(),
        source: marv_session::agents::AgentSource::Project,
    };
    let mut files = vec![(".marv/team.md".to_string(), render_team_file(&team))];
    let mut kept = Vec::new();
    for m in &spec.members {
        let path = format!(".marv/roles/{}.md", m.role);
        if files.iter().any(|(p, _)| *p == path) {
            continue;
        }
        if existing_roles.contains(&m.role) {
            kept.push(path);
            continue;
        }
        let role = marv_session::roles::find_role(roles, &m.role)
            .ok_or_else(|| format!("There's no role named \"{}\" here.", m.role))?;
        let writes = match &m.writes {
            Some(list) => {
                let mut out = Vec::new();
                for raw in list.iter().map(|w| w.trim()).filter(|w| !w.is_empty()) {
                    let prefix = marv_tools::write_set::parse_prefix(raw).map_err(|why| format!("{}: {why}.", m.title))?;
                    if !out.contains(&prefix) {
                        out.push(prefix);
                    }
                }
                Some(out)
            }
            None => None,
        };
        // A role the person left with no paths keeps its own bounds (none for a built-in that edits).
        let writes = writes.filter(|w| !w.is_empty() || role.writes.as_ref().is_some_and(Vec::is_empty));
        files.push((path, render_role_file(role, writes.as_deref())));
    }
    Ok((files, kept))
}

/// Why `.marv` or `.marv/roles` under `root` can't be written into, if it can't: one is a symlink (it could lead out
/// of the project) or not a folder. Missing is fine (they're made).
pub fn check_marv_dirs(root: &Path) -> Result<(), String> {
    for rel in [".marv", ".marv/roles"] {
        match std::fs::symlink_metadata(root.join(rel)) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(format!("{rel} is a symlink, so Marv doesn't write through it."));
            }
            Ok(meta) if !meta.is_dir() => return Err(format!("{rel} isn't a folder.")),
            _ => {}
        }
    }
    Ok(())
}

/// Whether `rel` under `root` is there in any form (a dangling symlink too).
pub fn present(root: &Path, rel: &str) -> bool {
    std::fs::symlink_metadata(root.join(rel)).is_ok()
}

/// Writes `text` to `rel` (`.marv/team.md`, `.marv/roles/<name>.md`) under `root` (canonical), never through a symlink:
/// `.marv` and `.marv/roles` are refused when they're symlinks and made when missing, the file itself is refused when
/// it's a symlink (dangling too) and when it's there and `replace` isn't given, the parent must canonicalize inside
/// `root`, and the text goes to a new temp file there, renamed over the target (a rename replaces a link, never
/// follows it).
pub fn safe_write(root: &Path, rel: &str, text: &str, replace: bool) -> Result<(), String> {
    check_marv_dirs(root)?;
    let target = root.join(rel);
    let parent = target.parent().ok_or_else(|| format!("{rel} has no folder."))?;
    std::fs::create_dir_all(parent).map_err(|e| format!("Couldn't make the folder for {rel}: {e}"))?;
    check_marv_dirs(root)?;
    let real_parent = std::fs::canonicalize(parent).map_err(|e| format!("{rel}: {e}"))?;
    if !real_parent.starts_with(root) {
        return Err(format!("{rel} leads outside the project, so it wasn't written."));
    }
    match std::fs::symlink_metadata(&target) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(format!("{rel} is a symlink, so Marv doesn't write through it."));
        }
        Ok(meta) if !meta.is_file() => return Err(format!("{rel} isn't a file.")),
        Ok(_) if !replace => return Err(format!("{rel} is there already.")),
        _ => {}
    }
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let temp = real_parent.join(format!(".{name}.{}-{nanos}.tmp", std::process::id()));
    let written = (|| {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temp, real_parent.join(&name))
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("Couldn't write {rel}: {e}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<String>>,
        fail_chats: Vec<String>,
        fail_rules: bool,
        wake: Option<String>,
        existing: Vec<String>,
        previous: Vec<(String, Vec<u64>)>,
        /// `other_teams` lists the team `make_rules` just made too (as the desktop does).
        lists_new: bool,
        made: Mutex<Option<(String, Vec<u64>)>>,
    }

    impl Fake {
        fn log(&self, line: String) {
            self.calls.lock().unwrap().push(line);
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl TeamHost for Fake {
        fn roles(&self, _: &Path) -> Vec<marv_session::roles::Role> {
            marv_session::roles::builtin_roles()
        }
        fn existing_teams(&self, _: &Path) -> Vec<String> {
            self.existing.clone()
        }
        fn other_teams(&self, _: &Path) -> Vec<(String, Vec<u64>)> {
            let mut all = self.previous.clone();
            if self.lists_new {
                all.extend(self.made.lock().unwrap().clone());
            }
            all
        }
        async fn make_rules(&self, _: &Path, name: &str, read: bool) -> Result<Vec<u64>, String> {
            self.log(format!("rules {name} read={read}"));
            if self.fail_rules {
                return Err("rules file broken".into());
            }
            let ids = if read { vec![1, 2] } else { vec![1] };
            *self.made.lock().unwrap() = Some((name.to_string(), ids.clone()));
            Ok(ids)
        }
        async fn revoke_rules(&self, ids: &[u64]) -> Result<(), String> {
            self.log(format!("revoke {ids:?}"));
            Ok(())
        }
        fn ensure_wake(&self) -> Result<Option<String>, String> {
            self.log("wake".into());
            Ok(self.wake.clone())
        }
        fn restore_wake(&self) -> Result<(), String> {
            self.log("wake back".into());
            Ok(())
        }
        fn small_model(&self) -> Option<String> {
            Some("cheap/one".into())
        }
        fn create_chat(&self, _: &Path, m: &NewMember) -> Result<Chat, String> {
            let (title, role, team, model) = (m.title.as_str(), m.role.as_str(), &m.team, m.model.as_str());
            let writes = m.writes.as_ref().map(|w| format!(" writes={}", w.join(","))).unwrap_or_default();
            self.log(format!("chat {title} {role} {model} [{}]{writes}", team.roster.join(",")));
            if self.fail_chats.iter().any(|t| t == title) {
                return Err("disk full".into());
            }
            Ok(Chat {
                id: format!("id-{title}"),
                folder: "/p".into(),
                title: title.into(),
                session_id: None,
                created: 0,
                settings: None,
                private: false,
                worktree: None,
                role: Some(role.into()),
                team: Some(team.clone()),
                writes: m.writes.clone(),
                harness: None,
                agent_model: None,
                terminal_started: false,
                permissions: None,
                triage: false,
                past_agents: Vec::new(),
                delegated_by: None,
            })
        }
        async fn worktrees(&self, chats: &[Chat]) -> Result<Option<String>, String> {
            self.log(format!("worktrees {}", chats.len()));
            Ok(None)
        }
        async fn grant(&self, chats: &[Chat], worktrees: bool) -> Result<Option<String>, String> {
            self.log(format!("grant {} worktrees={worktrees}", chats.len()));
            Ok(None)
        }
    }

    fn member(role: &str, model: Option<&str>) -> MemberSpec {
        MemberSpec {
            role: role.into(),
            title: format!("demo-{role}"),
            model: model.map(String::from),
            writes: None,
            harness: None,
        }
    }

    fn web() -> TeamSpec {
        TeamSpec {
            name: "web".into(),
            description: None,
            // The coordinator listed last still starts first.
            members: vec![
                member("frontend", Some("inherit")),
                member("backend", None),
                member("devserver", Some("small")),
                member("coordinator", Some("big/model")),
            ],
            worktrees: false,
            read_chats: true,
            keep_working: false,
            default_model: "z-ai/glm".into(),
            max_steps: 50,
            yolo: true,
        }
    }

    #[tokio::test]
    async fn a_start_makes_the_rule_then_wake_then_each_chat_coordinator_first() {
        let host = Fake { wake: Some("Wake was off; now a rule's wires.".into()), ..Fake::default() };
        let started = start(&host, Path::new("/p"), &web()).await.unwrap();
        assert!(started.ok);
        assert_eq!(
            host.calls(),
            [
                "rules web read=true",
                "wake",
                "chat demo-coordinator coordinator big/model [coordinator,frontend,backend,devserver]",
                "chat demo-frontend frontend z-ai/glm [coordinator,frontend,backend,devserver]",
                "chat demo-backend backend z-ai/glm [coordinator,frontend,backend,devserver]",
                "chat demo-devserver devserver cheap/one [coordinator,frontend,backend,devserver]",
            ]
        );
        assert_eq!(started.rules, [1, 2]);
        assert_eq!(started.chats.len(), 4);
        assert_eq!(started.steps[1].detail, "Wake was off; now a rule's wires.");
        assert!(started.steps.iter().all(|s| s.ok), "{:?}", started.steps);
    }

    #[tokio::test]
    async fn no_chat_made_takes_the_rule_back() {
        let spec = TeamSpec { members: vec![member("worker", None), member("reviewer", None)], read_chats: false, ..web() };
        let host = Fake { fail_chats: vec!["demo-worker".into(), "demo-reviewer".into()], ..Fake::default() };
        let started = start(&host, Path::new("/p"), &spec).await.unwrap();
        assert!(!started.ok && started.chats.is_empty() && started.rules.is_empty());
        assert_eq!(host.calls().last().unwrap(), "revoke [1]", "wake wasn't changed, so it isn't put back");
        let last = started.steps.last().unwrap();
        assert!(last.what == "Undo" && last.ok, "{last:?}");
        // The start turned wake on: the undo turns it off again.
        let host = Fake {
            fail_chats: vec!["demo-worker".into(), "demo-reviewer".into()],
            wake: Some("was off".into()),
            ..Fake::default()
        };
        let started = start(&host, Path::new("/p"), &spec).await.unwrap();
        assert_eq!(host.calls()[host.calls().len() - 2..], ["revoke [1]", "wake back"]);
        assert!(started.steps.last().unwrap().detail.contains("back to \"never\""));
    }

    #[tokio::test]
    async fn one_chat_failing_keeps_the_rest_and_says_so() {
        let host = Fake { fail_chats: vec!["demo-backend".into()], ..Fake::default() };
        let started = start(&host, Path::new("/p"), &TeamSpec { keep_working: true, worktrees: true, ..web() }).await.unwrap();
        assert!(started.ok);
        assert_eq!(started.chats.len(), 3);
        let failed: Vec<&Step> = started.steps.iter().filter(|s| !s.ok).collect();
        assert_eq!(failed.len(), 1);
        assert!(failed[0].what == "Chat demo-backend" && failed[0].detail == "disk full");
        // Keep working places the worktrees with its grants; no separate worktree step.
        assert_eq!(host.calls().last().unwrap(), "grant 3 worktrees=true");
        assert!(!host.calls().iter().any(|c| c.starts_with("worktrees")));
    }

    #[tokio::test]
    async fn typed_write_paths_go_with_the_chat_and_a_second_team_of_a_name_is_refused() {
        let host = Fake::default();
        let mut spec = web();
        spec.members[0].writes = Some(vec![" index.html ".into(), "./css/".into(), "".into()]);
        let started = start(&host, Path::new("/p"), &spec).await.unwrap();
        assert!(
            host.calls().contains(
                &"chat demo-frontend frontend z-ai/glm [coordinator,frontend,backend,devserver] writes=index.html,css"
                    .to_string()
            ),
            "{:?}",
            host.calls()
        );
        assert_eq!(
            started.chats.iter().find(|c| c.title == "demo-frontend").unwrap().writes,
            Some(vec!["index.html".into(), "css".into()])
        );
        assert!(started.steps.iter().any(|s| s.detail.ends_with("writes only index.html, css")));
        let host = Fake { existing: vec!["web".into()], ..Fake::default() };
        assert!(start(&host, Path::new("/p"), &web()).await.unwrap_err().contains("already"));
        assert!(host.calls().is_empty(), "nothing made");
    }

    #[tokio::test]
    async fn a_start_ends_the_rules_of_a_team_started_here_before_and_says_so() {
        let host = Fake { previous: vec![("old".into(), vec![10, 11])], ..Fake::default() };
        let started = start(&host, Path::new("/p"), &web()).await.unwrap();
        let calls = host.calls();
        let (made, revoked) =
            (calls.iter().position(|c| c.starts_with("rules")), calls.iter().position(|c| c == "revoke [10, 11]"));
        assert!(made.is_some() && revoked > made, "new rules first, then the old team's end: {calls:?}");
        let step = started.steps.iter().find(|s| s.what == "Previous team ended").unwrap();
        assert!(step.ok && step.detail.contains("old"), "{step:?}");
        assert!(started.ok && !started.rules.contains(&10));
        // A start that makes no rule leaves the old team alone.
        let host = Fake { previous: vec![("old".into(), vec![10])], fail_rules: true, ..Fake::default() };
        assert!(start(&host, Path::new("/p"), &web()).await.is_err());
        assert!(!host.calls().iter().any(|c| c.starts_with("revoke")));
    }

    #[tokio::test]
    async fn a_start_never_revokes_its_own_new_rules_nor_ends_a_team_with_no_live_rules() {
        // The desktop's `other_teams` lists every team here, the one just made included, and stale chat-only teams.
        let host =
            Fake { previous: vec![("old".into(), vec![10]), ("stale".into(), vec![])], lists_new: true, ..Fake::default() };
        let started = start(&host, Path::new("/p"), &web()).await.unwrap();
        let revokes: Vec<String> = host.calls().into_iter().filter(|c| c.starts_with("revoke")).collect();
        assert_eq!(revokes, ["revoke [10]"], "only the old team's rule: {:?}", host.calls());
        let ended: Vec<&Step> = started.steps.iter().filter(|s| s.what == "Previous team ended").collect();
        assert_eq!(ended.len(), 1, "{ended:?}");
        assert_eq!(started.rules, [1, 2]);
    }

    #[tokio::test]
    async fn worktrees_without_keep_working_are_made_after_the_chats() {
        let host = Fake::default();
        start(&host, Path::new("/p"), &TeamSpec { worktrees: true, ..web() }).await.unwrap();
        assert_eq!(host.calls().last().unwrap(), "worktrees 4");
    }

    #[tokio::test]
    async fn a_bad_spec_makes_nothing() {
        for (spec, says) in [
            (TeamSpec { name: "Web App".into(), ..web() }, "isn't a team name"),
            (TeamSpec { members: vec![], ..web() }, "1 to 8 members"),
            (TeamSpec { members: vec![member("designer", None)], ..web() }, "no role named \"designer\""),
            (TeamSpec { members: vec![member("worker", None), member("worker", None)], ..web() }, "Two chats are named"),
            (TeamSpec { members: vec![MemberSpec { title: " ".into(), ..member("worker", None) }], ..web() }, "needs a name"),
            (
                TeamSpec { members: vec![MemberSpec { writes: Some(vec!["src".into()]), ..member("devserver", None) }], ..web() },
                "changes no files",
            ),
            (
                TeamSpec { members: vec![MemberSpec { writes: Some(vec!["../x".into()]), ..member("frontend", None) }], ..web() },
                "climbs out",
            ),
        ] {
            let host = Fake::default();
            let err = start(&host, Path::new("/p"), &spec).await.unwrap_err();
            assert!(err.contains(says), "{err}");
            assert!(host.calls().is_empty(), "nothing made: {:?}", host.calls());
        }
        let host = Fake { fail_rules: true, ..Fake::default() };
        assert_eq!(start(&host, Path::new("/p"), &web()).await.unwrap_err(), "rules file broken");
        assert_eq!(host.calls(), ["rules web read=true"], "no chat without its rule");
    }

    #[test]
    fn teams_are_found_again_by_their_rules_labels_and_chat_records() {
        assert_eq!(team_of_label(&rule_label("web")), Some("web"));
        assert_eq!(team_of_label(&read_label("web")), Some("web"));
        assert_eq!(team_of_label("Chats under p message each other"), None);
        assert_eq!(team_of_label("Team Not A Name"), None);
        let p = std::path::PathBuf::from("/p");
        let q = std::path::PathBuf::from("/q");
        let rules = vec![
            (7, rule_label("web"), p.clone()),
            (8, read_label("web"), p.clone()),
            (9, rule_label("web"), q.clone()),
            (10, "x".into(), p.clone()),
        ];
        let chats = vec![
            ("a".into(), p.clone(), Some("web".into())),
            ("b".into(), p.clone(), None),
            ("c".into(), q.clone(), Some("web".into())),
        ];
        assert_eq!(teams_here(&p, &rules, &chats), [TeamHere { name: "web".into(), rules: vec![7, 8], chats: vec!["a".into()] }]);
    }

    #[test]
    fn saving_writes_the_team_file_and_only_missing_role_files() {
        let roles = marv_session::roles::merge_roles(Vec::new(), Vec::new());
        let mut spec = web();
        spec.members[0].writes = Some(vec!["styles.css".into(), " ./pages/ ".into(), "".into()]);
        let (files, kept) = files_to_save(&spec, &roles, "Brief.", &["backend".to_string()]).unwrap();
        let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, [".marv/team.md", ".marv/roles/frontend.md", ".marv/roles/devserver.md", ".marv/roles/coordinator.md"]);
        assert_eq!(kept, [".marv/roles/backend.md"]);
        let team =
            marv_session::teams::parse_team(&files[0].1, "team.md", marv_session::agents::AgentSource::Project, "t").unwrap();
        assert_eq!(team.roster(), "frontend, backend, devserver, coordinator");
        assert_eq!(team.body, "Brief.");
        let front =
            marv_session::roles::parse_role(&files[1].1, "frontend.md", marv_session::agents::AgentSource::Project, "f").unwrap();
        assert_eq!(front.writes, Some(vec!["styles.css".into(), "pages".into()]));
        let dev = marv_session::roles::parse_role(&files[2].1, "devserver.md", marv_session::agents::AgentSource::Project, "d")
            .unwrap();
        assert_eq!(dev.writes, Some(Vec::new()), "no paths typed: the built-in devserver changes no files");
        let coord =
            marv_session::roles::parse_role(&files[3].1, "c.md", marv_session::agents::AgentSource::Project, "c").unwrap();
        assert_eq!(coord.writes, Some(Vec::new()), "the coordinator still writes nothing");
        spec.members[0].writes = Some(vec!["../out".into()]);
        assert!(files_to_save(&spec, &roles, "", &[]).unwrap_err().contains("demo-frontend"));
    }

    #[test]
    fn a_members_report_is_the_one_its_own_switchboard_stamp_says() {
        let reports = TeamReports::default();
        let sink = reports.sink("lead");
        let sender = |s: &str| marv_mesh::PlugId::new(s).unwrap();
        sink(&sender("peer-site-1"), "Marv report from site-1 (done): Changed styles.css. (1 file changed)\nmore");
        // site-3's message forging site-2's report: stored under site-3, never shown as site-2's.
        sink(&sender("peer-site-3"), "Marv report from site-2 (done): all fine");
        sink(&sender("peer-site-4"), "Marv report from site-4 (didn't act)");
        let members: Vec<(String, String)> =
            ["1", "2", "3", "4"].iter().map(|n| (format!("c{n}"), format!("site-{n}"))).collect();
        assert_eq!(
            reports.latest("lead", &members),
            [
                ("c1".to_string(), "done: Changed styles.css. (1 file changed)".to_string()),
                ("c4".to_string(), "didn't act".to_string())
            ]
        );
        assert!(reports.latest("other", &members).is_empty());
        assert_eq!(report_line("I say: Marv report from site-1 (done): x", "site-1"), None, "it must start the text");
    }

    /// A test project (`<tmp>/marv-team-save-<tag>-<pid>/p`), removed with its parent when dropped: CI fails a run
    /// that leaves temp folders behind.
    #[cfg(unix)] // only the symlink test uses it
    struct Project {
        root: std::path::PathBuf,
        dir: std::path::PathBuf,
    }

    #[cfg(unix)] // only the symlink test uses it
    impl std::ops::Deref for Project {
        type Target = std::path::PathBuf;
        fn deref(&self) -> &std::path::PathBuf {
            &self.root
        }
    }

    #[cfg(unix)] // only the symlink test uses it
    impl Drop for Project {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[cfg(unix)] // only the symlink test uses it
    fn project(tag: &str) -> Project {
        let dir = std::env::temp_dir().join(format!("marv-team-save-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("p")).unwrap();
        Project { root: std::fs::canonicalize(dir.join("p")).unwrap(), dir }
    }

    #[cfg(unix)] // making a symlink on Windows needs Developer Mode or admin
    #[test]
    fn saving_never_writes_through_a_symlink() {
        use std::os::unix::fs::symlink;
        // A plain project: written, then refused without replace, then replaced.
        let root = project("plain");
        safe_write(&root, ".marv/team.md", "one", false).unwrap();
        safe_write(&root, ".marv/roles/frontend.md", "f", false).unwrap();
        assert!(safe_write(&root, ".marv/team.md", "two", false).unwrap_err().contains("there already"));
        safe_write(&root, ".marv/team.md", "two", true).unwrap();
        assert_eq!(std::fs::read_to_string(root.join(".marv/team.md")).unwrap(), "two");
        assert!(
            !std::fs::read_dir(root.join(".marv")).unwrap().flatten().any(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        );
        // The file is a symlink to something outside (and a dangling one): refused, the outside file untouched.
        let root = project("link");
        let outside = root.parent().unwrap().join("bashrc");
        std::fs::write(&outside, "keep").unwrap();
        std::fs::create_dir_all(root.join(".marv/roles")).unwrap();
        symlink(&outside, root.join(".marv/team.md")).unwrap();
        assert!(safe_write(&root, ".marv/team.md", "x", true).unwrap_err().contains("symlink"));
        symlink(root.parent().unwrap().join("nowhere"), root.join(".marv/roles/worker.md")).unwrap();
        assert!(safe_write(&root, ".marv/roles/worker.md", "x", false).unwrap_err().contains("symlink"));
        assert!(present(&root, ".marv/roles/worker.md"), "a dangling link counts as there");
        assert!(!root.parent().unwrap().join("nowhere").exists());
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep");
        // .marv (or .marv/roles) is a symlink out of the project: refused.
        let root = project("dir");
        let elsewhere = root.parent().unwrap().join("elsewhere");
        std::fs::create_dir_all(elsewhere.join("roles")).unwrap();
        symlink(&elsewhere, root.join(".marv")).unwrap();
        assert!(safe_write(&root, ".marv/team.md", "x", false).unwrap_err().contains(".marv is a symlink"));
        assert!(check_marv_dirs(&root).is_err());
        let root = project("roles");
        std::fs::create_dir_all(root.join(".marv")).unwrap();
        symlink(&elsewhere, root.join(".marv/roles")).unwrap();
        assert!(safe_write(&root, ".marv/roles/frontend.md", "x", false).unwrap_err().contains(".marv/roles is a symlink"));
        assert!(!elsewhere.join("frontend.md").exists() && !elsewhere.join("team.md").exists());
    }

    #[test]
    fn a_members_model() {
        assert_eq!(member_model(None, "d", Some("s")), "d");
        assert_eq!(member_model(Some("inherit"), "d", Some("s")), "d");
        assert_eq!(member_model(Some("small"), "d", Some("s")), "s");
        assert_eq!(member_model(Some("small"), "d", None), "d");
        assert_eq!(member_model(Some(" a/b "), "d", None), "a/b");
    }

    /// A rule for each pair of the team's worktrees and each with the folder; the folder alone is the folder rule's.
    #[test]
    fn worktree_pairs_cover_each_member_with_each_other_and_the_folder() {
        let p = |s: &str| PathBuf::from(s);
        let (a, b, c) = (p("/w/a"), p("/w/b"), p("/w/c"));
        let pairs = worktree_pairs(&p("/proj"), &[a.clone(), b.clone(), c.clone()]);
        assert_eq!(pairs.len(), 6, "3 with the folder, 3 among themselves");
        for (x, y) in [(&a, &b), (&a, &c), (&b, &c)] {
            assert!(pairs.iter().any(|(f, t)| (f, t) == (x, y) || (f, t) == (y, x)), "{x:?} with {y:?}");
        }
        assert!(pairs.iter().all(|(f, _)| [&a, &b, &c].contains(&f)), "a worktree is always the first of a pair");
        assert!(worktree_pairs(&p("/proj"), &[]).is_empty(), "no worktree, no pair");
        // A member that stayed in the folder is the folder rule's own.
        assert_eq!(worktree_pairs(&p("/proj"), &[p("/proj"), a.clone()]), [(a, p("/proj"))]);
    }

    /// What the kernel decides for those rules (the pure `match_rule`, as a wire is checked): a member in its worktree
    /// reaches a teammate in theirs and the lead in the folder; a chat in another project's worktree, or in a worktree
    /// the team never made, is covered by none of them, and nor is one that merely shares a name prefix.
    #[test]
    fn a_members_worktree_is_covered_by_the_teams_rules_and_no_other_is() {
        use marv_kernel::{Access, AgentId, AgentKind, AgentRef, Mode, PolicyId, PolicySnapshot, RuleBy, Subject, match_rule};
        let home = Path::new("/home/me");
        let at = |s: &str| marv_kernel::FolderScope::new(Path::new(s), home).unwrap();
        let (proj, wa, wb) = ("/home/me/proj", "/home/me/.marv/worktrees/proj/a", "/home/me/.marv/worktrees/proj/b");
        let folder_rule = |id: u64, from: &str, to: &str| marv_kernel::Policy {
            id: PolicyId(id),
            from: Subject::Folder(at(from)),
            to: Subject::Folder(at(to)),
            mode: Mode::Send,
            mutual: true,
            expires_at_ms: None,
            by: RuleBy::Desktop,
            created_at_ms: id,
            label: rule_label("web"),
        };
        let mut rules = vec![folder_rule(1, proj, proj)];
        for (x, y) in worktree_pairs(Path::new(proj), &[PathBuf::from(wa), PathBuf::from(wb)]) {
            let id = rules.len() as u64 + 1;
            rules.push(folder_rule(id, &x.display().to_string(), &y.display().to_string()));
        }
        let snapshot = PolicySnapshot { generation: 1, rules };
        let terminal = |name: &str, folder: &str| AgentRef {
            id: None::<AgentId>,
            mesh_name: name.into(),
            folder: PathBuf::from(folder),
            kind: AgentKind::Terminal,
        };
        let allowed = |from: &AgentRef, to: &AgentRef| {
            [Mode::Send].iter().all(|mode| match_rule(&snapshot, &Access { from, to, mode: *mode, now_ms: 0 }).is_ok())
        };
        let (a, b, lead) = (terminal("a", wa), terminal("b", wb), terminal("lead", proj));
        assert!(allowed(&a, &b) && allowed(&b, &a), "worker to worker, both ways");
        assert!(allowed(&a, &lead) && allowed(&lead, &a), "a report to the lead in the folder, and its task back");
        let other_project = terminal("x", "/home/me/.marv/worktrees/other/a");
        assert!(!allowed(&other_project, &a) && !allowed(&a, &other_project), "another project's worktree");
        let not_this_teams = terminal("y", "/home/me/.marv/worktrees/proj/c");
        assert!(!allowed(&not_this_teams, &a) && !allowed(&b, &not_this_teams), "a worktree the team didn't make");
        let beside = terminal("z", "/home/me/.marv/worktrees/proj/a-other");
        assert!(!allowed(&beside, &b), "a name that only starts like one of theirs");
    }

    /// The Team view finds a team's worktree rules by the folder of the chat they were made for.
    #[test]
    fn worktree_rules_are_listed_under_their_chats_folder() {
        let p = |s: &str| PathBuf::from(s);
        let trees = [(p("/w/a"), p("/proj")), (p("/w/b"), p("/proj"))];
        assert_eq!(rule_home(Path::new("/w/a"), &trees), p("/proj"));
        assert_eq!(rule_home(Path::new("/proj"), &trees), p("/proj"));
        assert_eq!(rule_home(Path::new("/elsewhere"), &trees), p("/elsewhere"), "another folder's rule stays its own");
        let rules = [(1, rule_label("web"), p("/proj")), (5, rule_label("web"), rule_home(Path::new("/w/a"), &trees))];
        let found = teams_here(Path::new("/proj"), &rules, &[("c".into(), p("/proj"), Some("web".into()))]);
        assert_eq!(found[0].rules, [1, 5]);
    }
}
