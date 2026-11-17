//! `marv mesh ls|wire|plug|allow|revoke|policies`, and the process's mesh (new in marv-rs; docs/MESH.md topology 2, docs/subsystems/mesh.md).
//!
//! - [`start`]: the switchboard of a `marv` process (the TUI's, or `marv -p`'s when asked for), named by
//!   `marv_mesh::discovery::mesh_name`, with Herdr's agent panes as receptacles when Marv runs in a Herdr pane. Its
//!   door ([`Mesh::open_door`]) puts it on the mesh: `<runtime>/mesh/<name>.sock`, in Marv's runtime folder
//!   (`marv_sandbox::runtime_dir`: `$XDG_RUNTIME_DIR/marv` on Linux, `/tmp/marv-<uid>` on macOS). No runtime folder,
//!   no mesh (never a fallback a sandboxed command could see).
//! - `ls`: the live switchboards in `<runtime>/mesh/` (owner and mode checked), one line each, and in a
//!   Herdr pane the agent panes it can reach. Everything printed goes through `printable()`.
//! - `wire 'alpha -> beta:inbox'`: asks `alpha` for a wire from its `parent` to `beta`'s parent inbox. `alpha`'s user
//!   approves, then `beta`'s (both ends, `Grant::remote`). Ends are `<name>[/<agent>]`; the target can also be one of
//!   `alpha`'s receptacles by name (`alpha -> herdr.pane.w1-p2.send:call`).
//! - `plug <name> [--as <plug>]`: the stdio↔socket shim. Another agent (Claude Code, opencode) runs it as an MCP
//!   server and gets a plug of its own on `<name>`'s switchboard, with no wires until the user wires it.
//! - `allow <folder> [--to <folder>] [--for 1h]`, `revoke <id>`, `policies` (runtime-kernel 2b): the person's rules,
//!   the same file as the desktop's Mesh panel. `allow` shows exactly what it allows and writes only after a y on a
//!   terminal (no terminal: refused, so a script or another agent's command can't make one); `revoke` asks nothing.
//!
//! **marv-rs difference from MESH.md rule 1:** running `marv mesh wire` asks, it doesn't approve. Any process of the
//! user's can run it, an unsandboxed agent in another pane included, so each end's Marv still asks its person (or
//! its `--approve mesh:<receptacle>` list headless).

use std::path::Path;
use std::sync::Arc;

use marv_core::printable;
use marv_mesh::discovery::{CONNECT, DoorClient, PLUG, PeerSwitchboard, check_socket, mesh_dir, peer_receptacle, session_socket};
use marv_mesh::herdr::{HerdrAdapter, PaneAction, herdr_target, pane_receptacle};
use marv_mesh::plug::{MODE_META, MODES_META};
use marv_mesh::{PlugId, ReceptacleName, WireMode};
use marv_session::mesh::PARENT;
use serde_json::{Map, Value, json};

pub use marv_session::mesh_process::{herdr_program, start};

fn env() -> marv_mcp::Env {
    std::env::vars().collect()
}

fn no_runtime_dir(command: &str) -> i32 {
    eprintln!(
        "marv mesh {command} needs {}: Marv puts the mesh only where sandboxed commands can't see it.",
        marv_sandbox::RUNTIME_DIR_RULE
    );
    if let Some(why) = marv_sandbox::override_refusal(&marv_sandbox::runtime_env()) {
        eprintln!("marv: {}", printable(&why));
    }
    1
}

/// `~/x` for a folder under the home folder.
pub(crate) fn short(path: &str) -> String {
    let home = marv_session::paths::home_dir();
    match Path::new(path).strip_prefix(&home) {
        Ok(rest) if !home.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => path.to_string(),
    }
}

/// One line per live switchboard: name, whether its radio is on (#124), pane, agents, folder, and the receptacle
/// another Marv reaches its parent by.
fn ls_lines(peers: &[PeerSwitchboard]) -> Vec<String> {
    let width = peers.iter().map(|p| p.session.len()).max().unwrap_or(0);
    peers
        .iter()
        .map(|p| {
            let inbox = ReceptacleName::inbox(&PlugId::new(PARENT).expect("a plug id"));
            let reach = peer_receptacle(&p.session, &inbox).map(|r| r.to_string()).unwrap_or_default();
            printable(&format!(
                "{:width$}  radio {:<3}  pane {}  agents {}  {}  ({})",
                p.session,
                if p.info.radio { "on" } else { "off" },
                p.info.pane.as_deref().unwrap_or("-"),
                p.info.agents.join(","),
                short(&p.info.cwd),
                reach,
            ))
        })
        .collect()
}

/// `marv mesh ls`. Returns the exit code.
pub async fn ls() -> i32 {
    let env = env();
    let Some(dir) = mesh_dir(&env) else { return no_runtime_dir("ls") };
    let peers = match marv_mesh::discovery::list(&dir).await {
        Ok(peers) => peers,
        Err(e) => {
            eprintln!("{}", printable(&e.to_string()));
            return 1;
        }
    };
    if peers.is_empty() {
        println!("No Marv is on the mesh here.");
    }
    for line in ls_lines(&peers) {
        println!("{line}");
    }
    // In Herdr: the agent panes a Marv here can be wired to (reading Herdr's list changes nothing).
    // Never a herdr from the folder `marv mesh ls` runs in (a project's node_modules/.bin, `.` on PATH).
    let here: Vec<std::path::PathBuf> = std::env::current_dir().ok().into_iter().collect();
    if let Some(target) = herdr_target(&env)
        && let Some(program) = herdr_program(&env, &here)
        && let Ok(panes) = HerdrAdapter::new(program, None).agents().await
        && !panes.is_empty()
    {
        println!("\nHerdr agent panes:");
        for pane in panes {
            let marv = peers.iter().find(|p| p.info.pane.as_deref() == Some(pane.pane_id.as_str())).map(|p| p.session.as_str());
            let here = if pane.pane_id == target.pane_id { " (this pane)" } else { "" };
            let names = [PaneAction::Send, PaneAction::Status]
                .into_iter()
                .filter_map(|a| pane_receptacle(&pane.pane_id, a))
                .map(|r| r.to_string())
                .collect::<Vec<_>>()
                .join(" ");
            println!(
                "{}",
                printable(&format!(
                    "  {}{here}  {} {}{}  {names}",
                    pane.pane_id,
                    pane.agent.as_deref().unwrap_or("?"),
                    pane.status.as_deref().unwrap_or("?"),
                    marv.map(|m| format!("  marv {m}")).unwrap_or_default(),
                ))
            );
        }
    }
    0
}

/// A `marv mesh wire` spec: which Marv asks, as which of its agents, for which of its receptacles, in which mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliWire {
    pub session: String,
    pub from: PlugId,
    pub to: ReceptacleName,
    pub mode: WireMode,
}

const WIRE_USAGE: &str = "write a wire as <marv>[/<agent>] -> <marv>[/<agent>]:inbox (e.g. alpha -> beta:inbox), or \
     <marv>[/<agent>] -> <receptacle>[:<mode>] (e.g. alpha -> herdr.pane.w1-p2.send:call); marv mesh ls shows the names";

/// `<marv>[/<agent>]`, the agent `parent` when left out.
fn end(text: &str) -> Result<(String, PlugId), String> {
    let (session, agent) = text.trim().split_once('/').unwrap_or((text.trim(), PARENT));
    let session_ok = !session.is_empty() && PlugId::new(session).is_ok();
    if !session_ok {
        return Err(format!("\"{session}\" isn't a Marv's name: {WIRE_USAGE}"));
    }
    Ok((session.to_string(), PlugId::new(agent).map_err(|e| e.to_string())?))
}

/// Parses `alpha -> beta:inbox`, `alpha/worker-1 -> alpha:inbox`, `alpha -> herdr.pane.w1-p2.send:call`.
pub fn parse_cli_wire(spec: &str) -> Result<CliWire, String> {
    let (from, to) = spec.split_once("->").ok_or_else(|| format!("\"{spec}\": {WIRE_USAGE}"))?;
    let (session, from) = end(from)?;
    let to = to.trim();
    if let Some(target) = to.strip_suffix(":inbox").filter(|t| !t.contains('.')) {
        let (to_session, agent) = end(target)?;
        let inbox = ReceptacleName::inbox(&agent);
        let to = if to_session == session { inbox } else { peer_receptacle(&to_session, &inbox).map_err(|e| e.to_string())? };
        return Ok(CliWire { session, from, to, mode: WireMode::Send });
    }
    let (name, mode) = match to.rsplit_once(':') {
        Some((name, mode)) => (name, mode.parse::<WireMode>().map_err(|e| e.to_string())?),
        None => (to, WireMode::Call),
    };
    if !name.contains('.') {
        return Err(format!("\"{spec}\": {WIRE_USAGE}"));
    }
    Ok(CliWire { session, from, to: ReceptacleName::new(name).map_err(|e| e.to_string())?, mode })
}

/// Connects to a live Marv's door by name.
async fn door(dir: &Path, session: &str) -> Result<DoorClient, String> {
    let socket = session_socket(dir, session).ok_or_else(|| format!("\"{session}\" isn't a Marv's name"))?;
    DoorClient::connect(&socket).await.map_err(|_| format!("No Marv named \"{session}\" is on the mesh (marv mesh ls)."))
}

/// `marv mesh wire <spec>`. Returns the exit code.
pub async fn wire(spec: &str) -> i32 {
    let wire = match parse_cli_wire(spec) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("{}", printable(&e));
            return 1;
        }
    };
    let env = env();
    let Some(dir) = mesh_dir(&env) else { return no_runtime_dir("wire") };
    let mut client = match door(&dir, &wire.session).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{}", printable(&e));
            return 1;
        }
    };
    eprintln!(
        "{}",
        printable(&format!("Asking for {}/{} -> {} ({}): approve it in that Marv.", wire.session, wire.from, wire.to, wire.mode))
    );
    let answer = client
        .request(CONNECT, json!({"from": wire.from, "to": wire.to, "mode": wire.mode}))
        .await
        .unwrap_or_else(|e| json!({"wired": false, "reason": e.to_string()}));
    if answer.get("wired").and_then(Value::as_bool) == Some(true) {
        println!("{}", printable(&format!("Wired {}/{} -> {} ({}).", wire.session, wire.from, wire.to, wire.mode)));
        0
    } else {
        let reason = answer.get("reason").and_then(Value::as_str).unwrap_or("refused");
        eprintln!("{}", printable(&format!("Not wired: {reason}")));
        1
    }
}

/// The shim's translation between an MCP client and the switchboard. MCP clients (and the models behind them) take
/// tool names of `[A-Za-z0-9_-]` only, so `agent.parent.inbox` is shown as `agent_parent_inbox` (a number added if two
/// collide) and mapped back on `tools/call`. A client doesn't know about wire modes: a call to a receptacle wired for
/// `send` only goes as a send.
#[derive(Debug, Default)]
pub struct ShimNames {
    /// Shown name → (receptacle, its wired modes).
    shown: Vec<(String, String, Vec<WireMode>)>,
    /// `tools/list` requests waiting for their answer.
    listing: Vec<Value>,
}

impl ShimNames {
    /// A line from the client, as the switchboard gets it.
    pub fn outgoing(&mut self, line: &str) -> String {
        let Ok(mut message) = serde_json::from_str::<Map<String, Value>>(line.trim_end()) else { return line.to_string() };
        match message.get("method").and_then(Value::as_str) {
            Some("tools/list") => {
                if let Some(id) = message.get("id") {
                    self.listing.push(id.clone());
                }
                line.to_string()
            }
            Some("tools/call") => {
                let Some(params) = message.get_mut("params").and_then(Value::as_object_mut) else { return line.to_string() };
                let shown = params.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
                if let Some((_, real, modes)) = self.shown.iter().find(|(s, _, _)| *s == shown) {
                    params.insert("name".into(), Value::from(real.as_str()));
                    let asked = params.get("_meta").and_then(|m| m.get(MODE_META)).is_some();
                    if !asked && !modes.contains(&WireMode::Call) && modes.contains(&WireMode::Send) {
                        let meta = params.entry("_meta").or_insert_with(|| json!({}));
                        if let Some(meta) = meta.as_object_mut() {
                            meta.insert(MODE_META.into(), json!(WireMode::Send));
                        }
                    }
                }
                format!("{}\n", Value::Object(message))
            }
            _ => line.to_string(),
        }
    }

    /// A line from the switchboard, as the client gets it.
    pub fn incoming(&mut self, line: &str) -> String {
        let Ok(mut message) = serde_json::from_str::<Map<String, Value>>(line.trim_end()) else { return line.to_string() };
        let Some(at) = message.get("id").and_then(|id| self.listing.iter().position(|l| l == id)) else {
            return line.to_string();
        };
        self.listing.remove(at);
        let Some(tools) = message.get_mut("result").and_then(|r| r.get_mut("tools")).and_then(Value::as_array_mut) else {
            return line.to_string();
        };
        self.shown.clear();
        for tool in tools.iter_mut() {
            let Some(real) = tool.get("name").and_then(Value::as_str).map(str::to_string) else { continue };
            let modes: Vec<WireMode> = tool
                .pointer(&format!("/_meta/{}", MODES_META.replace('/', "~1")))
                .cloned()
                .and_then(|m| serde_json::from_value(m).ok())
                .unwrap_or_default();
            let base: String = real.chars().map(|c| if c == '.' { '_' } else { c }).collect();
            let mut shown = base.clone();
            let mut n = 2;
            while self.shown.iter().any(|(s, _, _)| *s == shown) {
                shown = format!("{base}_{n}");
                n += 1;
            }
            tool["name"] = Value::from(shown.as_str());
            self.shown.push((shown, real, modes));
        }
        format!("{}\n", Value::Object(message))
    }
}

/// `marv mesh plug <marv> [--as <plug>]`: stdin and stdout become an MCP connection on a new plug of that Marv's
/// switchboard. The plug lives while this runs. Returns the exit code.
pub async fn plug(session: &str, name: Option<&str>) -> i32 {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let env = env();
    let Some(dir) = mesh_dir(&env) else { return no_runtime_dir("plug") };
    let name = name.map_or_else(|| format!("ext-{}", std::process::id()), str::to_string);
    let fail = |e: String| {
        eprintln!("marv mesh plug: {}", printable(&e));
        1
    };
    // Held while the shim runs: the switchboard unplugs it when this connection closes.
    let mut client = match door(&dir, session).await {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    let answer = match client.request(PLUG, json!({"name": name})).await {
        Ok(a) => a,
        Err(e) => return fail(e.to_string()),
    };
    let socket = std::path::PathBuf::from(answer.get("socket").and_then(Value::as_str).unwrap_or_default());
    if let Err(e) = check_socket(&socket) {
        return fail(e.to_string());
    }
    let stream = match marv_fs::local_socket::connect(&socket).await {
        Ok(s) => s,
        Err(e) => return fail(format!("can't reach the plug: {e}")),
    };
    eprintln!("{}", printable(&format!("marv mesh plug: plugged into {session} as {name}; the user wires it (marv mesh wire).")));
    let (read, mut write) = stream.into_split();
    let names = Arc::new(std::sync::Mutex::new(ShimNames::default()));
    let up = {
        let names = names.clone();
        async move {
            let mut stdin = BufReader::new(tokio::io::stdin());
            loop {
                let mut line = String::new();
                match stdin.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let line = names.lock().unwrap_or_else(|e| e.into_inner()).outgoing(&line);
                if write.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
            }
        }
    };
    let down = async move {
        let mut from = BufReader::new(read);
        let mut stdout = tokio::io::stdout();
        loop {
            let mut line = String::new();
            match from.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let line = names.lock().unwrap_or_else(|e| e.into_inner()).incoming(&line);
            if stdout.write_all(line.as_bytes()).await.is_err() || stdout.flush().await.is_err() {
                break;
            }
        }
    };
    // Either side closing ends the shim: the client left, or the Marv did.
    tokio::select! {
        _ = up => {}
        _ = down => {}
    }
    drop(client);
    0
}

/// The person's rules file (runtime-kernel 2b): the same one the desktop's Mesh panel and the TUI use.
pub(crate) fn rule_store() -> Result<marv_kernel::PolicyStore, String> {
    let (env, home) = (env(), marv_session::paths::home_dir());
    let config = marv_session::config::default_config_dir(&env, &home);
    marv_kernel::PolicyStore::new(&config, &home).map_err(|e| e.to_string())
}

/// The kernel's event log in the user's config folder (runtime-kernel 3): a rule made or revoked here is logged. A
/// problem is said once, on stderr; it never stops the command.
pub(crate) fn event_log() -> Option<std::sync::Arc<marv_session::kernel_events::EventWriter>> {
    let config = marv_session::config::default_config_dir(&env(), &marv_session::paths::home_dir());
    let log = marv_session::kernel_events::EventWriter::new(&config).ok()?;
    log.on_error(std::sync::Arc::new(|text: String| eprintln!("marv: {}", printable(&text))));
    Some(log)
}

/// `~/x` and relative folders as the real (canonical) folder, which must exist.
pub(crate) fn real_folder(folder: &str, home: &Path) -> Result<std::path::PathBuf, String> {
    // `~\x` too on Windows, as `marv mesh ls` shows it there.
    let expanded = match folder.strip_prefix("~/").or_else(|| folder.strip_prefix("~\\").filter(|_| cfg!(windows))) {
        Some(rest) => home.join(rest),
        None if folder == "~" => home.to_path_buf(),
        None => std::path::PathBuf::from(folder),
    };
    std::fs::canonicalize(&expanded).map_err(|e| format!("{folder}: {e}"))
}

/// `1h`, `30m`, `2h` as milliseconds: how long a rule lasts.
fn duration_ms(text: &str) -> Result<u64, String> {
    let bad = || format!("\"{text}\" isn't a duration like 1h or 30m");
    let (number, unit) = text.split_at(text.find(|c: char| !c.is_ascii_digit()).ok_or_else(bad)?);
    let n: u64 = number.parse().map_err(|_| bad())?;
    let ms = match unit {
        "m" | "min" => n * 60_000,
        "h" => n * 3_600_000,
        "d" => n * 86_400_000,
        _ => return Err(bad()),
    };
    if ms == 0 { Err(bad()) } else { Ok(ms) }
}

/// The rule `marv mesh allow` would make, and the sentence the person says yes or no to.
fn allow_rule(
    (folder, to, lasts, read): AllowArgs<'_>,
    home: &Path,
    now_ms: u64,
) -> Result<(marv_kernel::NewRule, String), String> {
    let subject = |path: &Path| -> Result<marv_kernel::Subject, String> {
        let scope = marv_kernel::FolderScope::new(path, home).map_err(|e| e.to_string())?;
        Ok(marv_kernel::Subject::Folder(scope))
    };
    let from_path = real_folder(folder, home)?;
    let to_path = match to {
        Some(to) => real_folder(to, home)?,
        None => from_path.clone(),
    };
    let expires_at_ms = lasts.map(duration_ms).transpose()?.map(|ms| now_ms + ms);
    let (shown_from, shown_to) = (short(&from_path.to_string_lossy()), short(&to_path.to_string_lossy()));
    let may = if read { "may read each other's chats" } else { "may message each other" };
    let between = if from_path == to_path {
        format!("Your Marvs under {shown_from} {may}")
    } else {
        format!("Your Marvs under {shown_from} and under {shown_to} {may}")
    };
    let until = lasts.map_or("until you revoke it".to_string(), |l| format!("for {l}"));
    let text = if read {
        // Runtime-kernel §9.2: a read rule is its own; it doesn't let them message each other.
        format!(
            "{between} without asking, {until} (desktop chats and terminal Marvs; never subagents, panes or `marv -p`): what you typed and what their assistants replied, never what a tool read or ran, and never a chat you marked private. What one reads arrives marked as another chat's words, and anything leaving your machine still asks. It doesn't let them message each other."
        )
    } else {
        format!(
            "{between} without asking, {until} (desktop chats and terminal Marvs; never subagents, panes or `marv -p`). What they send arrives marked as from another agent, and anything leaving your machine still asks."
        )
    };
    let rule = marv_kernel::NewRule {
        from: subject(&from_path)?,
        to: subject(&to_path)?,
        mode: if read { WireMode::Read } else { WireMode::Send },
        mutual: true,
        expires_at_ms,
        by: marv_kernel::RuleBy::Cli,
        created_at_ms: now_ms,
        label: between,
    };
    Ok((rule, text))
}

/// `marv mesh allow`'s folder, `--to`, `--for` and `--read`.
type AllowArgs<'a> = (&'a str, Option<&'a str>, Option<&'a str>, bool);

/// Asks y/N on the terminal: `None` when there's no terminal to ask on (a script, an agent's command).
pub(crate) fn confirm_on_tty(question: &str) -> Option<bool> {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return None;
    }
    eprint!("{}\nAllow? [y/N] ", printable(question));
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer).ok()?;
    Some(matches!(answer.trim(), "y" | "Y" | "yes" | "Yes"))
}

/// `marv mesh allow`: makes the rule only after the person's y on a terminal (decision D7). No terminal: refused, so a
/// script or another agent's command can't make one.
pub async fn allow(folder: &str, to: Option<&str>, lasts: Option<&str>, read: bool) -> i32 {
    let store = match rule_store() {
        Ok(store) => store,
        Err(e) => {
            eprintln!("marv mesh allow: {}", printable(&e));
            return 1;
        }
    };
    let log = event_log();
    let code = allow_logged(
        &store,
        &marv_session::paths::home_dir(),
        (folder, to, lasts, read),
        confirm_on_tty,
        log.as_deref().map(|l| l as &dyn marv_kernel::EventLog),
    )
    .await;
    if let Some(log) = log {
        log.flush().await;
    }
    code
}

/// [`allow`] with its store, home and way of asking given (tests).
#[cfg(test)]
async fn allow_with(
    store: &marv_kernel::PolicyStore,
    home: &Path,
    args: AllowArgs<'_>,
    confirm: impl FnOnce(&str) -> Option<bool>,
) -> i32 {
    allow_logged(store, home, args, confirm, None).await
}

/// [`allow_with`], logging the rule made to `log` (runtime-kernel 3).
async fn allow_logged(
    store: &marv_kernel::PolicyStore,
    home: &Path,
    args: AllowArgs<'_>,
    confirm: impl FnOnce(&str) -> Option<bool>,
    log: Option<&dyn marv_kernel::EventLog>,
) -> i32 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    let (rule, text) = match allow_rule(args, home, now) {
        Ok(made) => made,
        Err(e) => {
            eprintln!("marv mesh allow: {}", printable(&e));
            return 1;
        }
    };
    match confirm(&text) {
        None => {
            eprintln!("marv mesh allow asks you at a terminal, and there isn't one here: nothing was allowed.");
            return 1;
        }
        Some(false) => {
            eprintln!("Nothing was allowed.");
            return 1;
        }
        Some(true) => {}
    }
    match store.add(rule).await {
        Ok((id, snapshot)) => {
            if let Some(log) = log {
                marv_session::kernel_events::log_created(log, id, &snapshot);
            }
            println!("Allowed ({id}). Running Marvs use it from their next message; `marv mesh revoke {}` ends it.", id.0);
            0
        }
        Err(e) => {
            eprintln!("marv mesh allow: {}", printable(&e.to_string()));
            1
        }
    }
}

/// `marv mesh revoke <id>`: ends a rule (no prompt: it only takes permission away).
pub async fn revoke(id: u64) -> i32 {
    let log = event_log();
    let result = match rule_store() {
        Ok(store) => store.revoke(marv_kernel::PolicyId(id)).await.map_err(|e| e.to_string()),
        Err(e) => Err(e),
    };
    if let Some(log) = &log {
        if result.is_ok() {
            marv_session::kernel_events::log_revoked(log.as_ref(), marv_kernel::PolicyId(id));
        }
        log.flush().await;
    }
    match result {
        Ok(_) => {
            println!("Revoked rule {id}: what it allowed stops at the next message.");
            0
        }
        Err(e) => {
            eprintln!("marv mesh revoke: {}", printable(&e));
            1
        }
    }
}

/// `marv mesh policies`: the rules, one line each.
pub fn policies() -> i32 {
    let snapshot = match rule_store().and_then(|s| s.load().map_err(|e| e.to_string())) {
        Ok(snapshot) => snapshot,
        Err(e) => {
            eprintln!("marv mesh policies: {}", printable(&e));
            return 1;
        }
    };
    if snapshot.rules.is_empty() {
        println!("No rules: your Marvs ask you before messaging each other.");
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    for rule in &snapshot.rules {
        let lasts = match rule.expires_at_ms {
            None => "until revoked".to_string(),
            Some(end) if end <= now => "expired".to_string(),
            Some(end) => format!("{} min left", (end - now).div_ceil(60_000)),
        };
        let label = if rule.label.is_empty() { format!("{} rule", rule.mode) } else { rule.label.clone() };
        let by = format!("{:?}", rule.by).to_lowercase();
        println!("{}", printable(&format!("{:>3}  {label}  ({lasts}, made in the {by})", rule.id.0)));
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(duration_ms("1h"), Ok(3_600_000));
        assert_eq!(duration_ms("30m"), Ok(1_800_000));
        assert_eq!(duration_ms("2d"), Ok(172_800_000));
        for bad in ["", "h", "0h", "1", "1w", "-1h", "1.5h"] {
            assert!(duration_ms(bad).is_err(), "{bad}");
        }
    }

    /// Runtime-kernel 2b, D7: `marv mesh allow` writes a rule only after the person's y on a terminal. No terminal (a
    /// script, another agent's command) or a no: nothing is written. Never the home folder or above it.
    #[tokio::test]
    async fn allow_writes_only_after_a_yes_at_a_terminal() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        let acme = home.join("Projects/acme");
        std::fs::create_dir_all(&acme).unwrap();
        let store = marv_kernel::PolicyStore::new(&home.join(".marv"), &home).unwrap();
        let args = ("~/Projects/acme", None, Some("1h"), false);
        let mut shown = String::new();
        assert_eq!(allow_with(&store, &home, args, |_| None).await, 1, "no terminal");
        assert_eq!(allow_with(&store, &home, args, |_| Some(false)).await, 1, "a no");
        assert!(store.load().unwrap().rules.is_empty(), "nothing written");
        let yes = |q: &str| {
            shown = q.to_string();
            Some(true)
        };
        assert_eq!(allow_with(&store, &home, args, yes).await, 0);
        assert!(shown.contains("may message each other without asking, for 1h"), "{shown}");
        let rules = store.load().unwrap().rules;
        assert_eq!(rules.len(), 1);
        let rule = &rules[0];
        assert_eq!((rule.mode, rule.mutual, rule.by), (WireMode::Send, true, marv_kernel::RuleBy::Cli));
        assert!(rule.expires_at_ms.is_some());
        assert_eq!(rule.from.scope().unwrap().path(), acme);
        for refused in ["~", "/", "~/no-such-folder"] {
            assert_eq!(allow_with(&store, &home, (refused, None, None, false), |_| Some(true)).await, 1, "{refused}");
        }
        assert_eq!(store.load().unwrap().rules.len(), 1);

        // Runtime-kernel §9.2: `--read` makes a read rule, its own (no send), behind the same y/N.
        let read = ("~/Projects/acme", None, None, true);
        assert_eq!(allow_with(&store, &home, read, |_| Some(false)).await, 1);
        assert_eq!(store.load().unwrap().rules.len(), 1, "a no writes nothing");
        let mut asked = String::new();
        let yes = |q: &str| {
            asked = q.to_string();
            Some(true)
        };
        assert_eq!(allow_with(&store, &home, read, yes).await, 0);
        assert!(asked.contains("may read each other's chats without asking, until you revoke it"), "{asked}");
        assert!(asked.contains("never what a tool read or ran") && asked.contains("doesn't let them message"), "{asked}");
        let rules = store.load().unwrap().rules;
        assert_eq!((rules[1].mode, rules[1].mutual), (WireMode::Read, true));
    }

    #[test]
    fn wire_specs() {
        let w = parse_cli_wire("alpha -> beta:inbox").unwrap();
        assert_eq!(
            (w.session.as_str(), w.from.as_str(), w.to.as_str(), w.mode),
            ("alpha", "parent", "peer.beta.agent.parent.inbox", WireMode::Send)
        );
        let w = parse_cli_wire("alpha/worker-1 -> alpha:inbox").unwrap();
        assert_eq!((w.from.as_str(), w.to.as_str()), ("worker-1", "agent.parent.inbox"));
        let w = parse_cli_wire("alpha -> beta/claude:inbox").unwrap();
        assert_eq!(w.to.as_str(), "peer.beta.agent.claude.inbox");
        let w = parse_cli_wire("alpha -> herdr.pane.w1-p2.send:call").unwrap();
        assert_eq!((w.to.as_str(), w.mode), ("herdr.pane.w1-p2.send", WireMode::Call));
        assert_eq!(parse_cli_wire("alpha -> herdr.pane.w1-p2.status").unwrap().mode, WireMode::Call);
        for bad in
            ["alpha beta", "alpha -> beta", "Alpha -> beta:inbox", "alpha -> beta:everything", "/x -> b:inbox", "a -> x.y:all"]
        {
            assert!(parse_cli_wire(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn ls_shows_one_line_each_and_how_to_reach_it() {
        let peer = |session: &str, pane: Option<&str>| PeerSwitchboard {
            session: session.into(),
            socket: format!("/run/x/{session}.sock").into(),
            info: marv_mesh::discovery::PeerInfo {
                name: session.into(),
                pid: 1,
                cwd: "/srv/api\x1b[2J".into(),
                pane: pane.map(String::from),
                agents: vec!["parent".into(), "worker-1".into()],
                version: "0".into(),
                radio: pane.is_some(),
                chat: None,
                card: None,
                state: None,
                writes: Vec::new(),
                work_dir: None,
                live: None,
                role: None,
                wake: None,
                team_paused_until: None,
            },
        };
        let lines = ls_lines(&[peer("alpha", Some("w1:p2")), peer("b", None)]);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("alpha  radio on   pane w1:p2  agents parent,worker-1  /srv/api"), "{}", lines[0]);
        assert!(lines[0].ends_with("(peer.alpha.agent.parent.inbox)"));
        assert!(!lines[0].contains('\x1b'), "printable");
        assert!(lines[1].starts_with("b      radio off  pane -"), "{}", lines[1]);
    }

    #[test]
    fn the_shim_shows_mcp_safe_names_and_sends_to_inboxes() {
        let mut names = ShimNames::default();
        let list = r#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#;
        assert_eq!(names.outgoing(list), list);
        let answer = json!({"jsonrpc": "2.0", "id": 7, "result": {"tools": [
            {"name": "agent.parent.inbox", "_meta": {"marv/modes": ["send"]}},
            {"name": "agent_parent.inbox", "_meta": {"marv/modes": ["call"]}},
        ]}});
        let shown: Value = serde_json::from_str(&names.incoming(&answer.to_string())).unwrap();
        let tools = shown["result"]["tools"].as_array().unwrap();
        assert_eq!(tools[0]["name"], "agent_parent_inbox");
        assert_eq!(tools[1]["name"], "agent_parent_inbox_2", "collisions get a number");
        let call = json!({"jsonrpc": "2.0", "id": 8, "method": "tools/call", "params": {"name": "agent_parent_inbox", "arguments": {"text": "hi"}}});
        let sent: Value = serde_json::from_str(&names.outgoing(&call.to_string())).unwrap();
        assert_eq!(sent["params"]["name"], "agent.parent.inbox");
        assert_eq!(sent["params"]["_meta"]["marv/mode"], "send");
        let call2 = json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {"name": "agent_parent_inbox_2"}});
        let sent: Value = serde_json::from_str(&names.outgoing(&call2.to_string())).unwrap();
        assert_eq!(sent["params"]["name"], "agent_parent.inbox");
        assert!(sent["params"].get("_meta").is_none(), "a call wire stays a call");
        // Anything else passes as is, unknown names included (the switchboard refuses them).
        let other = r#"{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"nope"}}"#;
        assert_eq!(serde_json::from_str::<Value>(&names.outgoing(other)).unwrap()["params"]["name"], "nope");
        assert_eq!(names.incoming("not json\n"), "not json\n");
    }
}
