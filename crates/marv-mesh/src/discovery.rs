//! Finding other Marvs on this machine, and wiring across them (mesh v2). New in marv-rs (docs/MESH.md, topology 2).
//!
//! Each switchboard also listens on `<runtime>/mesh/<session>.sock` (folder 0700, socket 0600: the user only), its
//! **door** ([`open_door`]), in Marv's runtime folder (`marv_sandbox::runtime_dir`: `$XDG_RUNTIME_DIR/marv` on Linux,
//! `/tmp/marv-<uid>` on macOS, or `MARV_RUNTIME_DIR`). `<session>` is the process's mesh name ([`mesh_name`]):
//! `MARV_MESH_NAME`, or the project folder's name and four hex digits. It names the process, not the conversation, so
//! it stays the same across `/clear` and resume. Without a runtime folder there's no discovery: Marv won't fall back to
//! a folder others can read. Sandboxes never see the folder (`/run` and `/tmp` are empty tmpfs in bwrap).
//!
//! The door speaks newline-delimited JSON-RPC, like the plugs:
//! - `marv/info`: the name, pid, folder, Herdr pane and agents (`marv mesh ls`, [`list`]);
//! - `marv/plug {name}`: a new plug with no wires, for the stdio↔socket shim (`marv mesh plug`) that lets another
//!   agent (Claude Code, opencode) in as an MCP client. Only new names: never an existing plug's socket (that would
//!   let the caller speak as that agent), never `parent`, `worker-*` or `peer-*`. The plug goes when the connection
//!   that asked for it closes;
//! - `marv/connect {from, to, mode}`: `marv mesh wire`. This Marv's user is asked, and for another Marv's receptacle
//!   that Marv's user too. Running `marv mesh wire` asks; it doesn't approve (see the port notes in mesh.md);
//! - `marv/wire {from, to, mode, by}`: another switchboard asks for the far end of a cross-process wire. The caller
//!   must be the process listening on `<from>.sock` (its pid, from the socket's peer credentials, never from the
//!   message), and `to` must be one of this Marv's inboxes. If this Marv's user approves, the caller gets a plug of its
//!   own here, `peer-<from>`, wired with both approvals (`Grant::remote`), and its socket's path;
//! - `marv/read {from, what, n}` (runtime-kernel §9.2, `chat_read`): another Marv reads this one's chat. The caller
//!   is checked by pid exactly as for `marv/wire`, and who it is comes from its own `marv/info` on that confirmed
//!   connection; then a `read` rule the person made must cover it reading this Marv ([`crate::chat_read::serve`]).
//!   Marv desktop's chats all have their doors in one process, so for a desktop caller the pid confirms the desktop
//!   app, not which of its chats asks: the chat named is that process's own word, trusted as the person's own app
//!   (the rule is still checked against that chat's identity);
//! - `marv/radio.join {from, presence}` (#124): another Marv on the radio asks for a link. Refused unless this Marv's
//!   radio is on; the caller is checked by pid exactly as for `marv/wire`. Accepted, the connection becomes the link
//!   ([`crate::radio`]).
//!
//! On the asking side another Marv's inbox is the receptacle `peer.<session>.agent.<agent>.inbox`, found on demand
//! ([`PeerResolver`]) and served by a [`PeerLink`]: its messages go out through that `peer-<session>` plug, so the
//! receiving switchboard checks, limits and logs them like any other plug's, and its model sees them fenced, from
//! `peer-<session>`. Both switchboards log every message.

use marv_fs::local_socket::{Listener as UnixListener, Stream as UnixStream};
use marv_fs::mode::{CreateMode, mode_of, uid_of};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use marv_fs::local_socket::{OwnedReadHalf, OwnedWriteHalf};
use marv_mcp::jsonrpc::{Message, Request, RequestId, Response, code, decode_line, encode_line, method};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

use crate::MeshError;
use crate::Result;
use crate::approval::WireRequest;
use crate::message::Envelope;
use crate::plug::{PlugClient, PlugId};
use crate::receptacle::{Receptacle, ReceptacleKind, ReceptacleName, valid_segment};
use crate::switchboard::{MAX_LINE_BYTES, PEER_PREFIX, Resolve, Service, Switchboard};
use crate::wire::{Approver, WireMode};

/// `marv/info`: who this switchboard is.
pub const INFO: &str = "marv/info";
/// `marv/plug`: a new, unwired plug (the shim).
pub const PLUG: &str = "marv/plug";
/// `marv/connect`: `marv mesh wire`.
pub const CONNECT: &str = "marv/connect";
/// `marv/wire`: the far end of a cross-process wire, asked by another switchboard.
pub const WIRE: &str = "marv/wire";

/// How long a door gets to answer `marv/info` (`marv mesh ls`, finding a peer). Approvals wait as long as a person
/// takes, except the far end of a new wire ([`WIRE_ANSWER_WAIT`]).
pub const DOOR_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a new wire to another Marv (`marv/wire`) waits for that Marv's person to answer (#201). A person's own
/// prompt waits as long as they take: they're the one who'll answer it, and Esc ends it. But this wait holds the
/// *other* Marv's turn, whose person can't answer it and once couldn't stop it either: someone who stepped away from
/// the far Marv left the caller's turn hung. Three minutes is far longer than answering a prompt you're looking at
/// takes (the radio calls a Marv silent at 45 s and drops it at 120 s, and lets `radio_tx` wait up to 300 s), yet
/// short enough that a turn waiting on an empty chair ends while its person still remembers asking. Both ends keep
/// it: the caller gives up with "didn't answer", and the far door withdraws its person's prompt.
pub const WIRE_ANSWER_WAIT: Duration = Duration::from_secs(180);
/// A remote plug is `peer-<session>`, one name segment: session names leave room for the prefix.
pub const MAX_SESSION_NAME: usize = 43;
const PEER_PLUG: &str = "peer-";

/// `<runtime>/mesh`, or `None` without a runtime folder Marv may use (`marv_sandbox::runtime_dir`: on Linux
/// `$XDG_RUNTIME_DIR/marv` when every sandbox hides it, on macOS `/tmp/marv-<uid>`, or `MARV_RUNTIME_DIR`).
pub fn mesh_dir(env: &marv_mcp::Env) -> Option<PathBuf> {
    mesh_dir_for(env, None)
}

/// [`mesh_dir`] for a session in `project`: a `MARV_RUNTIME_DIR` inside it is refused (`marv_sandbox::runtime_dir_for`,
/// #168).
pub fn mesh_dir_for(env: &marv_mcp::Env, project: Option<&Path>) -> Option<PathBuf> {
    Some(marv_sandbox::runtime_dir_for(env, project)?.join("mesh"))
}

/// A session's discovery socket. Session ids are one name segment, so the path can't escape the folder.
pub fn session_socket(dir: &Path, session: &str) -> Option<PathBuf> {
    valid_segment(session).then(|| dir.join(format!("{session}.sock")))
}

/// Names no Marv may take (runtime-kernel §19): they're how the person is shown on a team board, so a chat named so
/// could pass its words off as the person's.
pub const RESERVED_NAMES: &[&str] = &["you", "user", "the-user", "person"];

/// Whether `name` can be a Marv's name on the mesh: one name segment, short enough for the `peer-` prefix, and not
/// one of [`RESERVED_NAMES`]. `what` names where it came from in the error (`MARV_MESH_NAME`, a chat's name).
pub fn check_session_name(name: &str, what: &str) -> Result<()> {
    if RESERVED_NAMES.contains(&name) {
        return Err(MeshError::BadName(format!("{what} \"{name}\" is reserved: it's how the user is shown on a team board")));
    }
    if valid_segment(name) && name.len() <= MAX_SESSION_NAME {
        return Ok(());
    }
    Err(MeshError::BadName(format!(
        "{what} \"{name}\" isn't a mesh name: lowercase letters, digits, - and _, at most {MAX_SESSION_NAME}"
    )))
}

/// This process's name on the mesh: `MARV_MESH_NAME` when set (checked), else the project folder's name (lowercased,
/// other characters as `-`, at most 20: `marv_kernel::name_base`) and four hex digits, e.g. `api-3f9a`. A desktop chat
/// is named from its id instead (`marv_kernel::mesh_name_for`), the same at every start.
pub fn mesh_name(env: &marv_mcp::Env, root: &Path) -> Result<String> {
    if let Some(name) = env.get("MARV_MESH_NAME").filter(|n| !n.is_empty()) {
        check_session_name(name, "MARV_MESH_NAME")?;
        return Ok(name.clone());
    }
    let base = marv_kernel::name_base(root);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    let suffix = (nanos ^ std::process::id().rotate_left(16)) & 0xffff;
    Ok(format!("{base}-{suffix:04x}"))
}

/// `peer.<session>.<remote>`: another Marv's receptacle, as this switchboard offers it.
pub fn peer_receptacle(session: &str, remote: &ReceptacleName) -> Result<ReceptacleName> {
    ReceptacleName::new(format!("{PEER_PREFIX}{session}.{remote}"))
}

/// The other Marv and its receptacle in a `peer.<session>.<remote>` name.
pub fn parse_peer(name: &ReceptacleName) -> Option<(String, ReceptacleName)> {
    let (session, remote) = name.as_str().strip_prefix(PEER_PREFIX)?.split_once('.')?;
    Some((session.to_string(), ReceptacleName::new(remote).ok()?))
}

/// The other Marv a `peer-<session>` plug is the link to (who sent a message that arrived on it); None for any other
/// plug (a subagent, a shim).
pub fn peer_session(plug: &PlugId) -> Option<&str> {
    plug.as_str().strip_prefix(PEER_PLUG).filter(|s| valid_segment(s))
}

/// The plug another Marv's messages arrive on here: `peer-<session>`.
pub fn peer_plug(session: &str) -> Result<PlugId> {
    PlugId::new(format!("{PEER_PLUG}{session}"))
}

/// Names a client may not take with `marv/plug`: Marv's own agents (`parent`, marv-session's `PARENT`, and
/// `worker-N`) and the peers'.
fn reserved(name: &str) -> bool {
    name == "parent" || name.starts_with("worker-") || name.starts_with(PEER_PLUG)
}

fn io(context: &str, err: std::io::Error) -> MeshError {
    MeshError::Io(format!("{context}: {err}"))
}

/// This process's effective uid, the owner of the files and sockets it makes. Through nix rather than `/proc/self`,
/// which macOS doesn't have (#150).
pub fn own_uid() -> Result<u32> {
    Ok(marv_fs::mode::own_uid())
}

/// Makes `dir` this user's private folder (0700), refusing a symlink or someone else's folder.
pub fn private_folder(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|e| io(&format!("can't make {}", dir.display()), e))?;
    let meta = std::fs::symlink_metadata(dir).map_err(|e| io(&dir.display().to_string(), e))?;
    if !meta.is_dir() || uid_of(&meta) != own_uid()? {
        return Err(MeshError::Io(format!("{} isn't a folder of this user's", dir.display())));
    }
    marv_fs::mode::set_mode(dir, 0o700).map_err(|e| io(&dir.display().to_string(), e))
}

/// A socket this user owns, mode 0600: anything else in the folder is never connected to.
pub fn check_socket(path: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| io(&path.display().to_string(), e))?;
    if !marv_fs::local_socket::is_socket(&meta, path)
        || uid_of(&meta) != own_uid()?
        || mode_of(&meta.permissions()) & 0o777 != 0o600
    {
        return Err(MeshError::Io(format!("{} isn't a private socket of this user's", path.display())));
    }
    Ok(())
}

/// What a door says about its switchboard (`marv/info`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerInfo {
    pub name: String,
    pub pid: u32,
    /// The project folder.
    pub cwd: String,
    /// The Herdr pane it runs in (`HERDR_PANE_ID`), if any.
    pub pane: Option<String>,
    /// Its agents' plugs (`parent`, `worker-1`, `peer-<session>`, a shim's).
    pub agents: Vec<String>,
    pub version: String,
    /// Whether its radio is on (#124). Missing from older Marvs: off.
    #[serde(default)]
    pub radio: bool,
    /// The desktop chat it is (its id, runtime-kernel 2a): the registry's key in later phases. None for a terminal
    /// Marv, and missing from older Marvs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
    /// Its chat card (runtime-kernel chat card, [`crate::card`]): the person's own typed words, cut and cleaned again
    /// on receipt ([`PeerInfo::received`]). Missing from older Marvs, and one that doesn't parse is none.
    #[serde(default, deserialize_with = "crate::card::lenient", skip_serializing_if = "Option::is_none")]
    pub card: Option<marv_kernel::Card>,
    /// Its scheduler state (a desktop chat's: sleeping, idle, busy, ...). Missing from a TUI and older Marvs; a word
    /// this Marv doesn't know is none.
    #[serde(default, deserialize_with = "crate::card::lenient", skip_serializing_if = "Option::is_none")]
    pub state: Option<marv_kernel::AgentState>,
    /// The paths its edit tools wrote in the last 30 minutes (runtime-kernel §14.4, [`marv_kernel::writes`]):
    /// absolute, newest first, inside `cwd` or `work_dir`. Published only with its `mesh.cards` on and never for a
    /// private chat. Cleaned again on receipt (a path outside its folders is dropped); missing from older Marvs, and an
    /// entry that doesn't parse is dropped.
    #[serde(default, deserialize_with = "lenient_writes", skip_serializing_if = "Vec::is_empty")]
    pub writes: Vec<marv_kernel::RecentWrite>,
    /// Where its session works when that isn't `cwd` (a desktop chat granted into its own worktree, runtime-kernel
    /// §14.1): its write paths may be there too. Missing otherwise, and from older Marvs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_dir: Option<String>,
    /// Runtime-kernel §20.2 (O2): its core, context, metrics, spend and real processes, for the person's Activity
    /// table and `marv kernel ps`. Fixed shapes, cut and cleaned on receipt; one that doesn't parse is none. Never
    /// shown to a model (`mesh_list` doesn't read it).
    #[serde(default, deserialize_with = "crate::card::lenient", skip_serializing_if = "Option::is_none")]
    pub live: Option<marv_kernel::metrics::Live>,
    /// The role its chat was started as (runtime-kernel coordination, `marv_session::roles`), fixed for its life.
    /// Missing from a chat without one and from older Marvs; one that isn't a plain name is dropped on receipt. A
    /// peer's own claim: shown in `mesh_list`, and matched against a role's `takes_tasks_from` (which only decides
    /// what a task turn may run without asking, inside that chat's own bounds; a wire is still needed to send at all).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Whether a message wakes it (its `mesh.wake`: on, rules, off). Missing from older Marvs; a word this Marv
    /// doesn't know is none.
    #[serde(default, deserialize_with = "crate::card::lenient", skip_serializing_if = "Option::is_none")]
    pub wake: Option<PeerWake>,
    /// Until when (epoch ms) a message over its team's wire won't wake it, its wake budget being used up
    /// ([`Switchboard::set_team_pause`]); missing when it would. Lets a sender's `mesh_send` say who needs to act.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_paused_until: Option<u64>,
}

/// A Marv's `mesh.wake` as its door says it ([`PeerInfo::wake`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PeerWake {
    /// Any wire's message wakes it.
    On,
    /// Only a wire a rule the person made issued wakes it (the default).
    Rules,
    /// Nothing wakes it: messages wait for its person.
    Off,
}

impl PeerWake {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Rules => "rules",
            Self::Off => "off",
        }
    }
}

/// Whether a published role name may be kept and shown: a plain lowercase name, as role files' names are.
pub fn valid_role_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A peer's write set as far as it parses: entries that don't are dropped, anything but a list is none.
fn lenient_writes<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Vec<marv_kernel::RecentWrite>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    let serde_json::Value::Array(items) = value else { return Ok(Vec::new()) };
    Ok(items.into_iter().take(4 * marv_kernel::MAX_WRITES).filter_map(|item| serde_json::from_value(item).ok()).collect())
}

impl PeerInfo {
    /// Another Marv's `marv/info` as it may be kept and shown: its card cut and cleaned again, its write set too
    /// (never trusted as sent), and its `live` too.
    pub fn received(mut self) -> Self {
        self.card = self.card.map(crate::card::received);
        let writes = std::mem::take(&mut self.writes);
        self.writes = marv_kernel::writes::received(writes, &self.roots(), crate::switchboard::now_ms());
        self.live = self.live.map(marv_kernel::metrics::Live::received);
        self.role = self.role.filter(|r| valid_role_name(r));
        self
    }

    /// The folders it says it works in (runtime-kernel §14.4): its folder, and its worktree if it has one.
    pub fn roots(&self) -> Vec<&Path> {
        std::iter::once(self.cwd.as_str()).chain(self.work_dir.as_deref()).map(Path::new).collect()
    }
}

/// A switchboard found with its live numbers, for the person's Activity table (runtime-kernel §20.2), and the pid
/// of the process listening on its door (its socket's peer credentials: the kernel's word, never the door's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LivePeer {
    pub peer: PeerSwitchboard,
    pub credential_pid: Option<u32>,
}

/// A switchboard found by `marv mesh ls`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerSwitchboard {
    pub session: String,
    pub socket: PathBuf,
    pub info: PeerInfo,
}

/// Every live switchboard in the folder, owned by this user: sockets that aren't 0600 and this user's, or that nobody
/// answers on (a Marv that crashed), are skipped and left alone. A missing folder is an empty list.
pub async fn list(dir: &Path) -> Result<Vec<PeerSwitchboard>> {
    Ok(probe(dir, false).await?.into_iter().map(|p| p.peer).collect())
}

/// [`list`] with each door's live numbers (`marv/info {"live": true}`) and credential pid: Activity and `marv kernel
/// ps` only. Never what `mesh_list` reads.
pub async fn list_live(dir: &Path) -> Result<Vec<LivePeer>> {
    probe(dir, true).await
}

async fn probe(dir: &Path, live: bool) -> Result<Vec<LivePeer>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io(&dir.display().to_string(), e)),
    };
    let meta = std::fs::symlink_metadata(dir).map_err(|e| io(&dir.display().to_string(), e))?;
    if !meta.is_dir() || uid_of(&meta) != own_uid()? || mode_of(&meta.permissions()) & 0o077 != 0 {
        return Err(MeshError::Io(format!("{} isn't a private folder of this user's: not listing it", dir.display())));
    }
    // Every door at once: an unresponsive Marv costs DOOR_TIMEOUT once, not once per socket after it.
    let probes = entries.flatten().filter_map(|entry| {
        let file = entry.file_name().to_string_lossy().into_owned();
        let session = file.strip_suffix(".sock").filter(|s| valid_segment(s))?.to_string();
        let socket = entry.path();
        Some(async move {
            let info = async {
                let mut door = DoorClient::connect(&socket).await?;
                let pid = door.pid.and_then(|p| u32::try_from(p).ok());
                let info = if live { door.info_live().await? } else { door.info().await? };
                Ok::<_, MeshError>((info, pid))
            };
            match tokio::time::timeout(DOOR_TIMEOUT, info).await {
                Ok(Ok((info, credential_pid))) => {
                    Some(LivePeer { peer: PeerSwitchboard { session, socket, info }, credential_pid })
                }
                _ => None,
            }
        })
    });
    let mut found: Vec<LivePeer> = futures::future::join_all(probes).await.into_iter().flatten().collect();
    found.sort_by(|a, b| a.peer.session.cmp(&b.peer.session));
    Ok(found)
}

/// A client on another switchboard's door. One request at a time.
pub struct DoorClient {
    read: BufReader<OwnedReadHalf>,
    write: OwnedWriteHalf,
    next_id: i64,
    /// The pid of the process listening on the door (its socket's peer credentials).
    pub pid: Option<i32>,
}

impl DoorClient {
    /// Connects to a door after checking it's a private socket of this user's.
    pub async fn connect(socket: &Path) -> Result<Self> {
        check_socket(socket)?;
        let stream =
            marv_fs::local_socket::connect(socket).await.map_err(|e| io(&format!("can't reach {}", socket.display()), e))?;
        let pid = marv_fs::local_socket::peer_pid(&stream);
        let (read, write) = stream.into_split();
        Ok(Self { read: BufReader::new(read), write, next_id: 0, pid })
    }

    /// One request and its result; a JSON-RPC error comes back as `MeshError::Io` with the door's message.
    pub async fn request(&mut self, name: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = RequestId::Number(self.next_id);
        let line = encode_line(&Message::Request(Request::new(id.clone(), name, Some(params))));
        self.write.write_all(line.as_bytes()).await.map_err(|e| io("the other Marv", e))?;
        loop {
            let mut line = String::new();
            let n = (&mut self.read)
                .take(MAX_LINE_BYTES as u64 + 1)
                .read_line(&mut line)
                .await
                .map_err(|e| io("the other Marv", e))?;
            if n == 0 || line.len() > MAX_LINE_BYTES {
                return Err(MeshError::Io("the other Marv closed the connection".into()));
            }
            if let Ok(Message::Response(r)) = decode_line(&line)
                && r.id == id
            {
                return match (r.result, r.error) {
                    (_, Some(err)) => Err(MeshError::Io(err.message)),
                    (result, None) => Ok(result.unwrap_or(Value::Null)),
                };
            }
        }
    }

    pub async fn info(&mut self) -> Result<PeerInfo> {
        self.info_with(json!({})).await
    }

    /// `marv/info` with the door's live numbers (runtime-kernel §20.2): Activity only.
    pub async fn info_live(&mut self) -> Result<PeerInfo> {
        self.info_with(json!({"live": true})).await
    }

    async fn info_with(&mut self, params: Value) -> Result<PeerInfo> {
        let value = self.request(INFO, params).await?;
        serde_json::from_value(value)
            .map(PeerInfo::received)
            .map_err(|_| MeshError::Io("the other Marv's answer isn't marv/info".into()))
    }
}

/// Who approves wires on this end for the door: the session's person (the TUI), or `--approve mesh:<r>` headless.
/// Never yolo, never an "always" scope.
#[async_trait]
pub trait PeerApprover: Send + Sync {
    async fn approve_wire(&self, request: WireRequest) -> Option<Approver>;
}

/// What the door says about this process.
#[derive(Debug, Clone)]
pub struct DoorInfo {
    pub cwd: PathBuf,
    pub pane: Option<String>,
    /// The desktop chat behind this door, if it is one (`PeerInfo::chat`).
    pub chat: Option<String>,
}

/// The open door. Dropping it stops serving and removes the socket.
pub struct DoorGuard {
    pub socket: PathBuf,
    cancel: CancellationToken,
}

impl Drop for DoorGuard {
    fn drop(&mut self) {
        self.cancel.cancel();
        let _ = std::fs::remove_file(&self.socket);
    }
}

struct Door {
    sb: Arc<Switchboard>,
    dir: PathBuf,
    info: DoorInfo,
    approver: Arc<dyn PeerApprover>,
}

/// Whether nothing listens on `socket` any more (the connection is refused). The connect is non-blocking: a live Marv
/// whose backlog is full answers "would block" at once instead of holding this one up, and counts as alive.
#[cfg(windows)]
fn nobody_listens(socket: &Path) -> bool {
    marv_fs::local_socket::nobody_listens(socket)
}

#[cfg(unix)]
fn nobody_listens(socket: &Path) -> bool {
    use socket2::{Domain, SockAddr, Socket, Type};
    let (Ok(probe), Ok(addr)) = (Socket::new(Domain::UNIX, Type::STREAM, None), SockAddr::unix(socket)) else { return false };
    if probe.set_nonblocking(true).is_err() {
        return false;
    }
    matches!(probe.connect(&addr), Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused)
}

/// Whether a live process listens on `socket`: the connect, non-blocking, succeeds or would block (a listener whose
/// backlog is full: alive, just not accepting). Never waits. A missing socket, a refused connection or any other error
/// is nobody.
///
/// Kernel #41: a blocking `UnixStream::connect` to a door whose owner stopped accepting (a hung Marv desktop) waits
/// until the backlog drains, which is never; a new desktop's chat checking its name that way hung with it.
#[cfg(windows)]
pub fn someone_listens(socket: &Path) -> bool {
    std::fs::symlink_metadata(socket).is_ok() && !marv_fs::local_socket::nobody_listens(socket)
}

#[cfg(unix)]
pub fn someone_listens(socket: &Path) -> bool {
    use socket2::{Domain, SockAddr, Socket, Type};
    let (Ok(probe), Ok(addr)) = (Socket::new(Domain::UNIX, Type::STREAM, None), SockAddr::unix(socket)) else { return false };
    if probe.set_nonblocking(true).is_err() {
        return false;
    }
    match probe.connect(&addr) {
        Ok(()) => true,
        Err(e) => e.kind() == std::io::ErrorKind::WouldBlock || e.raw_os_error() == Some(nix::libc::EINPROGRESS),
    }
}

/// Removes the sockets of Marvs that are gone: a door's socket outlives a Marv that was killed or crashed (the guard's
/// drop never ran), and the folder filled up with them. Only a refused connection counts as gone ([`nobody_listens`]);
/// a busy Marv keeps its socket. The folder is this user's and private ([`private_folder`]), so these are all
/// this user's Marvs. Called with the folder's lock held ([`open_door`]), so no other Marv is replacing a socket.
fn sweep_dead(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_socket = entry.metadata().is_ok_and(|m| marv_fs::local_socket::is_socket(&m, &path));
        if !is_socket || path.extension().is_none_or(|e| e != "sock") {
            continue;
        }
        if nobody_listens(&path) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// [`sweep_dead`] with the folder's lock held, as [`open_door`] does it: the radio's re-list, and a link that was
/// lost (#124). Blocks while another Marv holds the lock. The lock file is never made here: a door opened in the folder
/// made it, and a folder on its way out isn't filled again.
pub fn sweep_locked(dir: &Path) {
    let Ok(lock) = std::fs::OpenOptions::new().write(true).open(dir.join(".lock")) else { return };
    if lock.lock().is_ok() {
        sweep_dead(dir);
    }
}

/// The caller of a door is who listens on `<from>.sock`: checked by pid (the socket's peer credentials), never taken
/// from the message. `marv/wire` and `marv/radio.join` both ask this.
async fn confirm_caller(dir: &Path, from: &str, caller: Option<i32>) -> Result<()> {
    confirmed_door(dir, from, caller).await.map(drop)
}

/// [`confirm_caller`], keeping the connection to the confirmed Marv's door.
async fn confirmed_door(dir: &Path, from: &str, caller: Option<i32>) -> Result<Option<DoorClient>> {
    let socket = session_socket(dir, from).ok_or_else(|| MeshError::BadName(from.to_string()))?;
    let door = DoorClient::connect(&socket).await.ok();
    let listening = door.as_ref().and_then(|d| d.pid);
    if caller.is_none() || listening != caller {
        return Err(MeshError::Io(format!("can't confirm the caller is the Marv \"{from}\"")));
    }
    Ok(door)
}

/// Who a pid-confirmed peer is, from its own `marv/info` on the confirmed connection (runtime-kernel 2b): its chat id
/// (a desktop chat) or none (a terminal Marv), its name and its folder. `None` when it doesn't answer in
/// [`DOOR_TIMEOUT`] or names itself otherwise than `from`. Data only: what a rule would see, never a reason to admit.
async fn peer_identity(door: Option<DoorClient>, from: &str) -> Option<marv_kernel::AgentRef> {
    let mut door = door?;
    let info = tokio::time::timeout(DOOR_TIMEOUT, door.info()).await.ok()?.ok()?;
    (info.name == from).then(|| agent_ref(&info))
}

/// A Marv's [`marv_kernel::AgentRef`] from its `marv/info`.
pub fn agent_ref(info: &PeerInfo) -> marv_kernel::AgentRef {
    let id = info.chat.as_deref().and_then(marv_kernel::AgentId::new);
    let kind = if id.is_some() { marv_kernel::AgentKind::Chat } else { marv_kernel::AgentKind::Terminal };
    marv_kernel::AgentRef { id, mesh_name: info.name.clone(), folder: PathBuf::from(&info.cwd), kind }
}

/// Opens `sb`'s door at `<dir>/<sb.session>.sock` (0600, in the 0700 folder), serves it until the guard drops, and
/// lets `sb` find the other Marvs' inboxes in `dir` ([`PeerResolver`]). Refuses when a live Marv already has the
/// name; a dead one's socket is replaced. Needs a Tokio runtime.
pub fn open_door(sb: Arc<Switchboard>, dir: &Path, info: DoorInfo, approver: Arc<dyn PeerApprover>) -> Result<DoorGuard> {
    let name = sb.session.clone();
    let socket = session_socket(dir, &name)
        .filter(|_| name.len() <= MAX_SESSION_NAME)
        .ok_or_else(|| MeshError::BadName(format!("\"{name}\" isn't a mesh name")))?;
    private_folder(dir)?;
    // Sweeping and replacing sockets happen under one lock: otherwise a sweep that found a dead socket could remove it
    // after another Marv replaced it with its live one. Released when this function returns, the door bound.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .create_mode(0o600)
        .open(dir.join(".lock"))
        .map_err(|e| io("the mesh folder's lock", e))?;
    lock.lock().map_err(|e| io("the mesh folder's lock", e))?;
    sweep_dead(dir);
    if std::fs::symlink_metadata(&socket).is_ok() {
        if !nobody_listens(&socket) {
            return Err(MeshError::Io(format!("a Marv named \"{name}\" is already on the mesh (set MARV_MESH_NAME)")));
        }
        let _ = std::fs::remove_file(&socket);
    }
    marv_sandbox::runtime::check_socket_path(&socket).map_err(MeshError::Io)?;
    let listener =
        marv_fs::local_socket::bind_private(&socket).map_err(|e| io(&format!("can't listen on {}", socket.display()), e))?;
    sb.set_peer_dir(dir.to_path_buf());
    sb.add_resolver(Arc::new(PeerResolver { dir: dir.to_path_buf(), own: name, wait: sb.wire_wait() }));
    let door = Arc::new(Door { sb, dir: dir.to_path_buf(), info, approver });
    let cancel = CancellationToken::new();
    tokio::spawn(door.accept(listener, cancel.clone()));
    Ok(DoorGuard { socket, cancel })
}

impl Door {
    async fn accept(self: Arc<Self>, listener: UnixListener, cancel: CancellationToken) {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                conn = listener.accept() => match conn {
                    Ok((stream, _)) => {
                        tokio::spawn(Arc::clone(&self).connection(stream, cancel.clone()));
                    }
                    Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                },
            }
        }
    }

    /// One client: requests in order. Plugs it asked for (`marv/plug`) go when it leaves.
    async fn connection(self: Arc<Self>, stream: UnixStream, cancel: CancellationToken) {
        let caller = marv_fs::local_socket::peer_pid(&stream);
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        let mut made: Vec<PlugId> = Vec::new();
        loop {
            let mut line = Vec::new();
            let mut limited = (&mut read).take(MAX_LINE_BYTES as u64 + 1);
            let n = tokio::select! {
                _ = cancel.cancelled() => break,
                n = limited.read_until(b'\n', &mut line) => n,
            };
            match n {
                Ok(0) | Err(_) => break,
                Ok(_) if line.len() > MAX_LINE_BYTES => break,
                Ok(_) => {}
            }
            let Ok(text) = std::str::from_utf8(&line) else { break };
            let Ok(Message::Request(request)) = decode_line(text) else { continue };
            if request.method == crate::radio::JOIN {
                let id = request.id.clone();
                match self.radio_join(request.params.unwrap_or(Value::Null), caller).await {
                    Ok((radio, accepted)) => {
                        let answer = Response::ok(id, json!({"presence": accepted.presence}));
                        if write.write_all(encode_line(&Message::Response(answer)).as_bytes()).await.is_ok() {
                            for plug in made.drain(..) {
                                self.sb.unplug(&plug);
                            }
                            // From here the connection is the radio link.
                            return radio.adopt(accepted, read, write);
                        }
                        break;
                    }
                    Err(err) => {
                        let answer = Response::err(id, code::INVALID_PARAMS, err);
                        if write.write_all(encode_line(&Message::Response(answer)).as_bytes()).await.is_err() {
                            break;
                        }
                        continue;
                    }
                }
            }
            // #201: a request can wait on this Marv's person (`marv/wire`, `marv/connect`). A caller that hangs up
            // (it gave up, or its turn was stopped) or the door closing ends that wait, which withdraws the prompt:
            // the approver's future is dropped. `fill_buf` only peeks, so a next request already sent stays read.
            let response = tokio::select! {
                response = self.handle(request, caller, &mut made) => response,
                () = hung_up(&mut read) => break,
                _ = cancel.cancelled() => break,
            };
            if write.write_all(encode_line(&Message::Response(response)).as_bytes()).await.is_err() {
                break;
            }
        }
        for plug in made {
            self.sb.unplug(&plug);
        }
    }

    async fn handle(&self, request: Request, caller: Option<i32>, made: &mut Vec<PlugId>) -> Response {
        let id = request.id.clone();
        let params = request.params.unwrap_or(Value::Null);
        let text = |key: &str| params.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
        let result = match request.method.as_str() {
            method::INITIALIZE => Ok(json!({
                "protocolVersion": marv_mcp::jsonrpc::PROTOCOL_VERSION,
                "capabilities": {},
                "serverInfo": {"name": "marv-door", "version": env!("CARGO_PKG_VERSION")},
            })),
            method::PING => Ok(json!({})),
            // Runtime-kernel §20.2 (review P2-3): the live numbers only when asked (`{"live": true}`: Activity's
            // collect); a scan for `mesh_list` or a peer's identity check never makes the session compute them.
            INFO => {
                let live = params.get("live").and_then(Value::as_bool).unwrap_or(false);
                Ok(serde_json::to_value(self.info(live)).unwrap_or(Value::Null))
            }
            PLUG => self.plug(&text("name"), made),
            CONNECT => self.connect(&text("from"), &text("to"), &text("mode")).await,
            WIRE => self.wire(&text("from"), &text("to"), &text("mode"), &text("by"), caller).await,
            crate::chat_read::READ_METHOD => self.read(&text("from"), &params, caller).await,
            _ => return Response::err(id, code::METHOD_NOT_FOUND, "the door doesn't know that method"),
        };
        match result {
            Ok(value) => Response::ok(id, value),
            Err(err) => Response::err(id, code::INVALID_PARAMS, err.to_string()),
        }
    }

    fn info(&self, live: bool) -> PeerInfo {
        PeerInfo {
            name: self.sb.session.clone(),
            pid: std::process::id(),
            cwd: self.info.cwd.display().to_string(),
            pane: self.info.pane.clone(),
            agents: self.sb.agents().into_iter().map(String::from).collect(),
            version: env!("CARGO_PKG_VERSION").into(),
            radio: self.sb.radio().is_some_and(|r| r.is_on()),
            chat: self.info.chat.clone(),
            card: self.sb.card(),
            state: self.sb.state(),
            writes: self.sb.published_writes(),
            work_dir: self.sb.published_work_dir(&self.info.cwd),
            live: if live { self.sb.live() } else { None },
            role: self.sb.role(),
            wake: self.sb.wake(),
            team_paused_until: self.sb.team_pause(),
        }
    }

    /// A new plug with no wires, under a name nobody has.
    fn plug(&self, name: &str, made: &mut Vec<PlugId>) -> Result<Value> {
        let id = PlugId::new(name)?;
        if reserved(name) || self.sb.has_plug(&id) {
            return Err(MeshError::BadName(format!("\"{name}\" is taken: pick another name for the plug")));
        }
        let plug = self.sb.plug(id.clone())?;
        made.push(id);
        Ok(json!({"plug": plug.id, "socket": plug.host_socket}))
    }

    /// `marv mesh wire`: this end's person approves, then (for another Marv's receptacle) that Marv's.
    async fn connect(&self, from: &str, to: &str, mode: &str) -> Result<Value> {
        let from = PlugId::new(from)?;
        if !self.sb.has_plug(&from) {
            return Err(MeshError::NoReceptacle(format!("agent {from} on {}", self.sb.session)));
        }
        let to = ReceptacleName::new(to)?;
        let mode: WireMode = mode.parse()?;
        let receptacle = self.sb.lookup(&to).await.ok_or_else(|| MeshError::NoReceptacle(to.to_string()))?;
        if !receptacle.supports(mode) {
            return Err(MeshError::NotServed { receptacle: to.to_string(), mode });
        }
        let request = WireRequest { from: from.clone(), to: to.clone(), mode, asked_by: from };
        let Some(by) = self.approver.approve_wire(request.clone()).await else {
            return Ok(json!({"wired": false, "reason": format!("{}'s user said no", self.sb.session)}));
        };
        match self.sb.grant(request, by).await {
            Ok(()) => {
                Ok(json!({"wired": true, "by": by, "cross_process": matches!(receptacle.kind, ReceptacleKind::Peer { .. })}))
            }
            Err(err) => Ok(json!({"wired": false, "reason": err.to_string()})),
        }
    }

    /// Another switchboard asks for the far end of a wire to one of this Marv's inboxes.
    async fn wire(&self, from: &str, to: &str, mode: &str, by: &str, caller: Option<i32>) -> Result<Value> {
        let remote_by: Approver = serde_json::from_value(Value::from(by))
            .map_err(|_| MeshError::BadName(format!("\"{by}\" isn't who approved a wire")))?;
        if from == self.sb.session || !valid_segment(from) || from.len() > MAX_SESSION_NAME {
            return Err(MeshError::BadName(format!("\"{from}\" isn't another Marv's name")));
        }
        // The caller is who listens on <from>.sock: checked by pid, never taken from the message.
        let mut confirmed = confirmed_door(&self.dir, from, caller).await?;
        let to = ReceptacleName::new(to)?;
        let mode: WireMode = mode.parse()?;
        let receptacle = self.sb.receptacle(&to).ok_or_else(|| MeshError::NoReceptacle(to.to_string()))?;
        if receptacle.kind != ReceptacleKind::Inbox || !receptacle.supports(mode) {
            return Err(MeshError::NotServed { receptacle: to.to_string(), mode });
        }
        let plug_id = peer_plug(from)?;
        let request = WireRequest { from: plug_id.clone(), to, mode, asked_by: plug_id.clone() };
        // A rule the person here made may answer for them (runtime-kernel 2b): only when this Marv honors rules and
        // has some, for its main agent's inbox, and for a caller whose own pid-confirmed `marv/info` says who it is.
        // Anything else asks the person, as before.
        let mut caller_is = None;
        let to_main = request.to.inbox_owner() == Some(crate::PARENT_AGENT);
        let by_rule = if to_main && self.sb.rules_now().is_some_and(|r| !r.rules.is_empty()) {
            caller_is = peer_identity(confirmed.take(), from).await;
            let me = self.sb.identity(&PlugId::new(crate::PARENT_AGENT)?);
            match (&caller_is, me) {
                (Some(caller), Some(me)) => self.sb.rule_grant(&request, caller, &me),
                _ => None,
            }
        } else {
            None
        };
        let by_here = match by_rule {
            Some(by) => by,
            // Bounded like the caller's wait (#201): a prompt nobody answers is withdrawn, not left for later.
            None => match tokio::time::timeout(self.sb.wire_wait(), self.approver.approve_wire(request.clone())).await {
                Err(_) => {
                    // In the event log too: a "yes" that never reached this door (or came too late) shows here.
                    self.sb.log_wire_refused(&request, marv_kernel::WireRefusal::Unanswered);
                    return Ok(json!({"granted": false, "unanswered": true}));
                }
                Ok(Some(by)) => by,
                Ok(None) => {
                    // Runtime-kernel live UX: the person here said no, in the event log (ids and the reason only).
                    self.sb.log_wire_refused(&request, marv_kernel::WireRefusal::Declined);
                    return Ok(json!({"granted": false}));
                }
            },
        };
        let plug = self.sb.plug_peer(plug_id.clone())?;
        let mut wire = request.grant(by_here);
        wire.cross_process = true;
        wire.grant.remote = Some(remote_by);
        self.sb.wire(wire)?;
        // Who the wire's far end is, kept for the peer plug's life: known already when a rule was looked for,
        // else asked now that the person here said yes.
        let who = match caller_is {
            Some(who) => Some(who),
            None => peer_identity(confirmed, from).await,
        };
        if let Some(who) = who {
            self.sb.set_identity(plug_id, who);
        }
        // A rule here is only "peer_rule" there: a RuleGrant never leaves this process.
        Ok(json!({"granted": true, "by": by_here.on_the_wire(), "socket": plug.host_socket}))
    }
}

impl Door {
    /// `marv/read` (runtime-kernel §9.2): only from the process listening on `<from>.sock` (by pid, as `marv/wire`),
    /// who is who its own `marv/info` on that connection says; the rest is [`crate::chat_read::serve`]'s.
    async fn read(&self, from: &str, params: &Value, caller: Option<i32>) -> Result<Value> {
        if from == self.sb.session || !valid_segment(from) || from.len() > MAX_SESSION_NAME {
            return Err(MeshError::BadName(format!("\"{from}\" isn't another Marv's name")));
        }
        let confirmed = confirmed_door(&self.dir, from, caller).await?;
        let who = peer_identity(confirmed, from).await;
        Ok(crate::chat_read::serve(&self.sb, from, who, params).await)
    }

    /// `marv/radio.join` (#124): only while this Marv's radio is on, only from the process listening on `<from>.sock`
    /// (by pid, as `marv/wire`), then the radio's own checks ([`crate::radio::Radio::accept_join`]).
    async fn radio_join(
        &self,
        params: Value,
        caller: Option<i32>,
    ) -> std::result::Result<(Arc<crate::radio::Radio>, crate::radio::Accepted), String> {
        // No radio at all (`marv -p`) is off; one that's off says so itself (`accept_join`).
        let radio = self.sb.radio().ok_or_else(|| format!("{}'s radio is off", self.sb.session))?;
        let from = params.get("from").and_then(Value::as_str).unwrap_or_default();
        if from == self.sb.session || !valid_segment(from) || from.len() > MAX_SESSION_NAME {
            return Err(format!("\"{from}\" isn't another Marv's name"));
        }
        confirm_caller(&self.dir, from, caller).await.map_err(|e| e.to_string())?;
        let accepted = radio.accept_join(from, params.get("presence").unwrap_or(&Value::Null))?;
        Ok((radio, accepted))
    }
}

/// Resolves once `read` is at its end: the caller hung up (or its connection broke). Never while it's still there,
/// even with a request already sent: that's left in the buffer for the next read. So a caller that sends a second
/// request before the first is answered isn't seen hanging up until then: Marv's own clients (`DoorClient`, `marv mesh
/// wire`) never do, and `marv/wire`'s wait is bounded anyway (`WIRE_ANSWER_WAIT`, as is the door closing).
async fn hung_up(read: &mut BufReader<OwnedReadHalf>) {
    match read.fill_buf().await {
        Ok([]) | Err(_) => {}
        Ok(_) => std::future::pending().await,
    }
}

/// A wait in words, for a message: "3 minutes", "1 minute", "45 seconds", "200 ms".
fn words(wait: Duration) -> String {
    let secs = wait.as_secs();
    match secs {
        0 => format!("{} ms", wait.as_millis()),
        60 => "1 minute".into(),
        s if s % 60 == 0 => format!("{} minutes", s / 60),
        1 => "1 second".into(),
        s => format!("{s} seconds"),
    }
}

/// Finds `peer.<session>.agent.<agent>.inbox`: a live Marv named `<session>` in the folder that has that agent.
pub struct PeerResolver {
    pub dir: PathBuf,
    /// This switchboard's own name: never a peer of itself.
    pub own: String,
    /// How long a new wire waits for the far Marv's person ([`WIRE_ANSWER_WAIT`]).
    pub wait: Duration,
}

#[async_trait]
impl Resolve for PeerResolver {
    async fn resolve(&self, name: &ReceptacleName) -> Option<(Receptacle, Arc<dyn Service>)> {
        let (session, remote) = parse_peer(name)?;
        let agent = remote.inbox_owner()?.to_string();
        if session == self.own {
            return None;
        }
        let socket = session_socket(&self.dir, &session)?;
        let info =
            tokio::time::timeout(DOOR_TIMEOUT, async { DoorClient::connect(&socket).await?.info().await }).await.ok()?.ok()?;
        if !info.agents.contains(&agent) {
            return None;
        }
        let template = Receptacle::inbox(PlugId::new(agent.clone()).ok()?);
        let receptacle = Receptacle {
            name: name.clone(),
            owner: None,
            kind: ReceptacleKind::Peer { session: session.clone() },
            description: format!("{agent}'s inbox on the Marv \"{session}\" ({}): messages it reads at its next turn", info.cwd),
            modes: vec![WireMode::Send],
            input_schema: template.input_schema,
        };
        let link =
            PeerLink { dir: self.dir.clone(), own: self.own.clone(), session, remote, wait: self.wait, socket: Mutex::default() };
        Some((receptacle, Arc::new(link)))
    }
}

/// Serves `peer.<session>.<remote>`: the far end's approval through its door, then messages through the plug it
/// gave this Marv (`peer-<own>`), so the far switchboard knows the sender by the socket.
pub struct PeerLink {
    pub dir: PathBuf,
    pub own: String,
    pub session: String,
    pub remote: ReceptacleName,
    /// How long the far Marv's person gets to answer a new wire ([`WIRE_ANSWER_WAIT`]).
    pub wait: Duration,
    /// The far end's plug socket for this Marv, once its user approved.
    socket: Mutex<Option<PathBuf>>,
}

#[async_trait]
impl Service for PeerLink {
    async fn deliver(&self, envelope: &Envelope) -> Result<Option<Value>> {
        let socket = self.socket.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let socket = socket.ok_or_else(|| MeshError::Io(format!("the Marv \"{}\" hasn't approved this wire", self.session)))?;
        let mut client = PlugClient::connect(&socket).await?;
        // The message's hop goes with it (runtime-kernel 2b): the far switchboard reads it from this peer plug only.
        match envelope.mode {
            WireMode::Send => {
                let text = json!({"text": envelope.send_text()?});
                client
                    .forward(
                        &self.remote,
                        WireMode::Send,
                        text,
                        envelope.hop,
                        envelope.rounds_out,
                        envelope.report.then_some(envelope.task),
                        envelope.clean,
                    )
                    .await
                    .map(|_| None)
            }
            WireMode::Call => client
                .forward(
                    &self.remote,
                    WireMode::Call,
                    envelope.body.clone(),
                    envelope.hop,
                    envelope.rounds_out,
                    envelope.report.then_some(envelope.task),
                    envelope.clean,
                )
                .await
                .map(|answer| Some(Value::String(answer))),
            WireMode::Watch | WireMode::Read => {
                Err(MeshError::NotServed { receptacle: envelope.receptacle.to_string(), mode: envelope.mode })
            }
        }
    }

    /// Asks the far Marv's door; its user decides. Every plug of this Marv reaches it as `peer-<own>`: which of them
    /// may is this end's wire table's business. Waits [`Self::wait`] at most (#201): then the connection is dropped,
    /// which withdraws the far prompt, and the caller reads that nobody answered. Dropped sooner (the turn stopped),
    /// the same happens.
    async fn approve_remote(&self, _from: &PlugId, mode: WireMode, local: Approver) -> Option<Result<Approver>> {
        let unanswered = || {
            MeshError::Io(format!(
                "the Marv \"{}\"'s person didn't answer within {}: no wire was made",
                self.session,
                words(self.wait)
            ))
        };
        let ask = async {
            let socket = session_socket(&self.dir, &self.session).ok_or_else(|| MeshError::BadName(self.session.clone()))?;
            let mut door = DoorClient::connect(&socket).await?;
            let request =
                door.request(WIRE, json!({"from": self.own, "to": self.remote, "mode": mode, "by": local.on_the_wire()}));
            let answer = tokio::time::timeout(self.wait, request).await.map_err(|_| unanswered())??;
            if answer.get("unanswered").and_then(Value::as_bool) == Some(true) {
                return Err(unanswered());
            }
            if answer.get("granted").and_then(Value::as_bool) != Some(true) {
                return Err(MeshError::Io(format!("the Marv \"{}\" didn't wire it: its user said no", self.session)));
            }
            let by: Approver = serde_json::from_value(answer.get("by").cloned().unwrap_or_default())
                .map_err(|_| MeshError::Io("the other Marv's answer has no approver".into()))?;
            let socket = PathBuf::from(answer.get("socket").and_then(Value::as_str).unwrap_or_default());
            check_socket(&socket)?;
            *self.socket.lock().unwrap_or_else(|e| e.into_inner()) = Some(socket);
            Ok(by)
        };
        Some(ask.await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::switchboard::tests::{Log, Runtime};
    use crate::wire::Grant;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn id(s: &str) -> PlugId {
        PlugId::new(s).unwrap()
    }
    fn name(s: &str) -> ReceptacleName {
        ReceptacleName::new(s).unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn sockets_live_under_the_runtime_dir_only() {
        let env = |v: &str| [("XDG_RUNTIME_DIR".to_string(), v.to_string())].into_iter().collect::<marv_mcp::Env>();
        #[cfg(not(target_os = "macos"))]
        {
            // The runtime folder is made and checked, so it has to be a real private folder of this user's.
            let xdg = PathBuf::from(format!("/tmp/mx-disc-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&xdg);
            std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700).create(&xdg).unwrap();
            let got = mesh_dir(&env(xdg.to_str().unwrap()));
            let _ = std::fs::remove_dir_all(&xdg);
            assert_eq!(got, Some(xdg.join("marv/mesh")));
            assert_eq!(mesh_dir(&env("relative")), None);
            assert_eq!(mesh_dir(&Default::default()), None);
        }
        // macOS has no XDG_RUNTIME_DIR: the runtime folder is /tmp/marv-<uid> whatever it says.
        #[cfg(target_os = "macos")]
        assert_eq!(mesh_dir(&env("/run/user/1000")), Some(PathBuf::from(format!("/tmp/marv-{}/mesh", own_uid().unwrap()))));
        let dir = PathBuf::from("/run/user/1000/marv/mesh");
        assert_eq!(session_socket(&dir, "abc-1"), Some(dir.join("abc-1.sock")));
        assert_eq!(session_socket(&dir, "../x"), None);
    }

    #[test]
    fn mesh_names() {
        let env = |v: &str| [("MARV_MESH_NAME".to_string(), v.to_string())].into_iter().collect::<marv_mcp::Env>();
        assert_eq!(mesh_name(&env("api"), Path::new("/x")).unwrap(), "api");
        assert!(mesh_name(&env("Not/A Name"), Path::new("/x")).is_err());
        for reserved in RESERVED_NAMES {
            assert!(mesh_name(&env(reserved), Path::new("/x")).is_err(), "{reserved}");
        }
        assert!(mesh_name(&env(&"a".repeat(44)), Path::new("/x")).is_err(), "leaves room for peer-");
        let named = mesh_name(&Default::default(), Path::new("/home/me/My Project!")).unwrap();
        assert!(named.starts_with("my-project-") && named.len() == "my-project-".len() + 4, "{named}");
        assert!(valid_segment(&named));
        assert!(mesh_name(&Default::default(), Path::new("/")).unwrap().starts_with("marv-"));
        let long = mesh_name(&Default::default(), Path::new("/a-very-long-folder-name-indeed-yes")).unwrap();
        assert!(long.len() <= 25, "{long}");
    }

    #[test]
    fn info_names_the_chat_and_an_older_marvs_info_without_it_parses() {
        let old = r#"{"name":"api-3f9a","pid":7,"cwd":"/w","pane":null,"agents":["parent"],"version":"0.7.0"}"#;
        let parsed: PeerInfo = serde_json::from_str(old).unwrap();
        assert_eq!((parsed.chat.as_deref(), parsed.radio), (None, false));
        let chat = PeerInfo { chat: Some("554963ee-fd8e-4bec-a7dc-49051f090acc".into()), ..parsed.clone() };
        let wire = serde_json::to_value(&chat).unwrap();
        assert_eq!(wire["chat"], "554963ee-fd8e-4bec-a7dc-49051f090acc");
        assert!(serde_json::to_value(&parsed).unwrap().get("chat").is_none(), "a terminal Marv's info is unchanged");
    }

    /// Runtime-kernel chat card: `marv/info` carries the card and state both ways; an older Marv's info (none) parses,
    /// a newer Marv's extra card field is dropped with the card kept, and a card or state that doesn't parse is none
    /// without hiding the door. A received card is cut and cleaned
    /// again, whatever the peer sent.
    #[test]
    fn info_round_trips_with_and_without_a_card() {
        let old = r#"{"name":"api-3f9a","pid":7,"cwd":"/w","pane":null,"agents":["parent"],"version":"0.7.0"}"#;
        let parsed: PeerInfo = serde_json::from_str(old).unwrap();
        assert_eq!((parsed.card.as_ref(), parsed.state), (None, None));
        let text = serde_json::to_string(&parsed).unwrap();
        assert!(!text.contains("card") && !text.contains("state"), "a Marv without them says what it said: {text}");

        let card = marv_kernel::Card {
            title: "Set up the Rust API skeleton".into(),
            now: "add auth".into(),
            last_active_ms: 9,
            ..Default::default()
        };
        let with = PeerInfo { card: Some(card.clone()), state: Some(marv_kernel::AgentState::Sleeping), ..parsed.clone() };
        let wire = serde_json::to_value(&with).unwrap();
        assert_eq!(wire["state"], "sleeping");
        assert_eq!(serde_json::from_value::<PeerInfo>(wire.clone()).unwrap(), with);

        // A newer Marv's card with a field this one doesn't know: the card is kept, the field dropped.
        let mut newer = wire.clone();
        newer["card"] = json!({"title": "t", "mood": "a future field", "last_active_ms": 2});
        let newer = serde_json::from_value::<PeerInfo>(newer).unwrap().received();
        assert_eq!(
            newer.card,
            Some(marv_kernel::Card { title: "t".into(), now: String::new(), last_active_ms: 2, ..Default::default() })
        );
        // A card or state that doesn't parse is none, and the door still lists.
        let mut odd = wire.clone();
        odd["card"] = json!({"title": 7});
        odd["state"] = json!("hibernating");
        let odd: PeerInfo = serde_json::from_value(odd).unwrap();
        assert_eq!((odd.card, odd.state, odd.name.as_str()), (None, None, "api-3f9a"));

        let mut hostile = wire;
        hostile["card"] = json!({"title": format!("\x1b]52;c;aGk=\x07{}", "y".repeat(400)), "now": "a\nb", "last_active_ms": 1});
        let got = serde_json::from_value::<PeerInfo>(hostile).unwrap().received().card.unwrap();
        assert_eq!(got.title.chars().count(), crate::radio::MAX_TITLE_CHARS);
        assert!(!got.title.contains('\x1b') && !got.title.contains('\x07'), "{}", got.title);
        assert_eq!(got.now, "a b");
    }

    /// The door says its switchboard's card (without words while titles are off) and state; and a client reading it
    /// gets the card cleaned.
    #[tokio::test]
    async fn the_door_says_the_card_and_state() {
        let mesh = Runtime::new();
        let a = marv("alpha", &mesh.0, Person::new(None));
        let read = || async { list(&mesh.0).await.unwrap().remove(0).info };
        let first = read().await;
        assert_eq!((first.card, first.state), (None, None));

        a.sb.set_card(marv_kernel::Card {
            title: "the client's migration".into(),
            now: "step 2".into(),
            last_active_ms: 4,
            ..Default::default()
        });
        a.sb.set_state(Some(marv_kernel::AgentState::Idle));
        let off = read().await;
        assert_eq!(off.card, None, "mesh.cards off until the person's config turns it on: nothing published");
        assert_eq!(off.state, Some(marv_kernel::AgentState::Idle));
        a.sb.set_cards(true);
        assert_eq!(read().await.card.unwrap().title, "the client's migration");
    }

    /// Runtime-kernel §20.2 (review P2-3): the door computes its live numbers only when asked (`list_live`), never
    /// for a plain scan; and the credential pid is the kernel's, this process's here.
    #[tokio::test]
    async fn the_door_says_live_numbers_only_when_asked() {
        let mesh = Runtime::new();
        let a = marv("alpha", &mesh.0, Person::new(None));
        let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = asked.clone();
        a.sb.set_live(Arc::new(move || {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Some(marv_kernel::metrics::Live { core: Some("glm".into()), ..Default::default() })
        }));
        assert_eq!(list(&mesh.0).await.unwrap()[0].info.live, None);
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 0, "a scan never computes them");
        let live = list_live(&mesh.0).await.unwrap();
        assert_eq!(live[0].peer.info.live.as_ref().and_then(|l| l.core.as_deref()), Some("glm"));
        assert_eq!(live[0].credential_pid, Some(std::process::id()));
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn a_chats_stable_name_is_a_session_name() {
        let id = marv_kernel::AgentId::new("554963ee-fd8e-4bec-a7dc-49051f090acc").unwrap();
        let long = Path::new("/home/me/a-very-long-folder-name-indeed-yes-really");
        for folder in [Path::new("/home/me/My Project!"), long, Path::new("/")] {
            let stable = marv_kernel::mesh_name_for(folder, &id);
            assert!(check_session_name(&stable, "a chat's name").is_ok(), "{stable}");
            assert!(session_socket(Path::new("/run/user/1000/marv/mesh"), &stable).is_some());
        }
        assert!(check_session_name("Bad Name", "a chat's name").unwrap_err().to_string().contains("a chat's name"));
    }

    #[test]
    fn peer_names() {
        let inbox = name("agent.parent.inbox");
        let peer = peer_receptacle("api-3f9a", &inbox).unwrap();
        assert_eq!(peer.as_str(), "peer.api-3f9a.agent.parent.inbox");
        assert_eq!(parse_peer(&peer), Some(("api-3f9a".to_string(), inbox)));
        assert_eq!(parse_peer(&name("agent.parent.inbox")), None);
        assert_eq!(peer_plug("api-3f9a").unwrap().as_str(), "peer-api-3f9a");
        assert!(reserved("parent") && reserved("worker-3") && reserved("peer-x") && !reserved("claude"));
    }

    /// Says `answer` to every wire, keeping what it was asked.
    struct Person(Option<Approver>, Mutex<Vec<WireRequest>>);
    impl Person {
        fn new(answer: Option<Approver>) -> Arc<Self> {
            Arc::new(Self(answer, Mutex::default()))
        }
        fn asked(&self) -> Vec<String> {
            self.1.lock().unwrap().iter().map(WireRequest::title).collect()
        }
    }
    #[async_trait]
    impl PeerApprover for Person {
        async fn approve_wire(&self, request: WireRequest) -> Option<Approver> {
            self.1.lock().unwrap().push(request);
            self.0
        }
    }

    /// One Marv: a switchboard serving its plugs, with a parent, and its door in the shared mesh folder.
    struct Marv {
        sb: Arc<Switchboard>,
        log: Arc<Log>,
        _door: DoorGuard,
        _runtime: Runtime,
        cancel: CancellationToken,
    }
    impl Drop for Marv {
        fn drop(&mut self) {
            self.cancel.cancel();
        }
    }

    fn marv(session: &str, mesh: &Path, person: Arc<Person>) -> Marv {
        let log = Arc::new(Log::default());
        let runtime = Runtime::new();
        let sb = Arc::new(Switchboard::new(session, runtime.0.clone(), log.clone()));
        sb.plug(id("parent")).unwrap();
        let cancel = CancellationToken::new();
        tokio::spawn(Arc::clone(&sb).serve(cancel.clone()));
        // beta is a desktop chat: its info names the chat.
        let chat = (session == "beta").then(|| "0a1b2c3d-0000-4000-8000-000000000000".to_string());
        let info = DoorInfo { cwd: PathBuf::from(format!("/work/{session}")), pane: Some(format!("w1:{session}")), chat };
        let door = open_door(sb.clone(), mesh, info, person).unwrap();
        Marv { sb, log, _door: door, _runtime: runtime, cancel }
    }

    #[cfg(unix)]
    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    /// Whether nothing listens on `socket` within a second. A listener closed here can live on for a moment: a child
    /// that another test is spawning holds a copy of it until its exec.
    #[cfg(unix)]
    fn gone_soon(socket: &Path) -> bool {
        (0..100).any(|_| {
            let gone = nobody_listens(socket);
            if !gone {
                std::thread::sleep(Duration::from_millis(10));
            }
            gone
        })
    }

    /// Kernel #41: `someone_listens` says yes for a live listener (accepting or not), no for a stale socket file whose
    /// owner is gone, a missing path or a plain file, and never blocks.
    #[cfg(unix)]
    #[test]
    fn someone_listens_only_for_a_live_listener() {
        let mesh = Runtime::new();
        std::fs::create_dir_all(&mesh.0).unwrap();
        let live = std::os::unix::net::UnixListener::bind(mesh.0.join("live.sock")).unwrap();
        assert!(someone_listens(&mesh.0.join("live.sock")));
        drop(std::os::unix::net::UnixListener::bind(mesh.0.join("stale.sock")).unwrap());
        assert!(mesh.0.join("stale.sock").exists(), "the socket file outlives its listener");
        assert!((0..100).any(|_| {
            let gone = !someone_listens(&mesh.0.join("stale.sock"));
            if !gone {
                std::thread::sleep(Duration::from_millis(10));
            }
            gone
        }));
        assert!(!someone_listens(&mesh.0.join("missing.sock")));
        std::fs::write(mesh.0.join("plain.sock"), "").unwrap();
        assert!(!someone_listens(&mesh.0.join("plain.sock")));
        drop(live);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn opening_a_door_sweeps_dead_marvs_sockets_only() {
        let mesh = Runtime::new();
        let _alpha = marv("alpha", &mesh.0, Person::new(None));
        drop(std::os::unix::net::UnixListener::bind(mesh.0.join("dead.sock")).unwrap());
        assert!(gone_soon(&mesh.0.join("dead.sock")));
        std::fs::write(mesh.0.join("notes.sock"), "not a socket").unwrap();
        let beta = marv("beta", &mesh.0, Person::new(None));
        assert!(!mesh.0.join("dead.sock").exists(), "nothing listens there: swept");
        assert!(mesh.0.join("alpha.sock").exists() && mesh.0.join("notes.sock").exists());
        assert_eq!(beta.sb.peer_dir(), Some(mesh.0.clone()));
        // A Marv that takes no connections (a listener nobody accepts on) is busy, not gone.
        let busy = std::os::unix::net::UnixListener::bind(mesh.0.join("busy.sock")).unwrap();
        assert!(!nobody_listens(&mesh.0.join("busy.sock")));
        drop(busy);
        assert!(gone_soon(&mesh.0.join("busy.sock")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ls_lists_live_private_doors_only() {
        let mesh = Runtime::new();
        let (a, b) = (marv("alpha", &mesh.0, Person::new(None)), marv("beta", &mesh.0, Person::new(None)));
        assert_eq!(mode(&mesh.0), 0o700);
        assert_eq!(mode(&mesh.0.join("alpha.sock")), 0o600);
        // A Marv that crashed left its socket: nobody answers there.
        drop(std::os::unix::net::UnixListener::bind(mesh.0.join("dead.sock")).unwrap());
        std::fs::set_permissions(mesh.0.join("dead.sock"), std::fs::Permissions::from_mode(0o600)).unwrap();
        // A socket anyone may use isn't connected to, live or not.
        let open = std::os::unix::net::UnixListener::bind(mesh.0.join("open.sock")).unwrap();
        std::fs::set_permissions(mesh.0.join("open.sock"), std::fs::Permissions::from_mode(0o666)).unwrap();
        std::fs::write(mesh.0.join("notes.txt"), "x").unwrap();
        let found = list(&mesh.0).await.unwrap();
        assert_eq!(found.iter().map(|p| p.session.as_str()).collect::<Vec<_>>(), ["alpha", "beta"]);
        assert_eq!(found[0].info.agents, ["parent"]);
        assert_eq!(found[0].info.cwd, "/work/alpha");
        assert_eq!(found[1].info.pane.as_deref(), Some("w1:beta"));
        assert_eq!(found[0].info.pid, std::process::id());
        assert_eq!(found[0].info.chat, None);
        assert_eq!(found[1].info.chat.as_deref(), Some("0a1b2c3d-0000-4000-8000-000000000000"));
        // The name is taken while alpha lives.
        let again = Arc::new(Switchboard::new("alpha", Runtime::new().0.clone(), Arc::new(Log::default())));
        let info = DoorInfo { cwd: "/".into(), pane: None, chat: None };
        assert!(open_door(again, &mesh.0, info.clone(), Person::new(None)).is_err());
        // Gone with its guard; a dead Marv's name can be taken again.
        drop(a);
        assert!(!mesh.0.join("alpha.sock").exists());
        assert_eq!(list(&mesh.0).await.unwrap().len(), 1);
        let dead = Arc::new(Switchboard::new("dead", Runtime::new().0.clone(), Arc::new(Log::default())));
        let _dead = open_door(dead, &mesh.0, info, Person::new(None)).unwrap();
        assert_eq!(list(&mesh.0).await.unwrap().len(), 2);
        drop((b, open));
        // No folder, nothing listed; a folder others can read isn't listed at all.
        assert!(list(&mesh.0.join("missing")).await.unwrap().is_empty());
        std::fs::set_permissions(&mesh.0, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(list(&mesh.0).await.is_err());
    }

    #[tokio::test]
    async fn a_cross_process_wire_needs_both_users_then_carries_messages() {
        let mesh = Runtime::new();
        let (pa, pb) = (Person::new(Some(Approver::Cli)), Person::new(Some(Approver::Tui)));
        let (a, b) = (marv("alpha", &mesh.0, pa.clone()), marv("beta", &mesh.0, pb.clone()));
        let to_b = name("peer.beta.agent.parent.inbox");
        // Found on demand: a peer's inbox, by its name.
        let r = a.sb.lookup(&to_b).await.unwrap();
        assert_eq!(r.kind, ReceptacleKind::Peer { session: "beta".into() });
        assert_eq!(r.modes, [WireMode::Send]);
        assert!(a.sb.lookup(&name("peer.beta.agent.nobody.inbox")).await.is_none());
        assert!(a.sb.lookup(&name("peer.alpha.agent.parent.inbox")).await.is_none(), "never its own peer");
        assert!(a.sb.lookup(&name("peer.gamma.agent.parent.inbox")).await.is_none());

        let ask = WireRequest { from: id("parent"), to: to_b.clone(), mode: WireMode::Send, asked_by: id("parent") };
        a.sb.grant(ask, Approver::Tui).await.unwrap();
        // Both ends hold a wire with both approvals; beta's user was asked about alpha's plug there.
        assert_eq!(pb.asked(), ["Wire peer-alpha -> agent.parent.inbox (send)"]);
        let wire = a.sb.table().find(&id("parent"), &to_b, WireMode::Send).cloned().unwrap();
        assert!(wire.cross_process);
        assert_eq!(wire.grant, Grant { local: Approver::Tui, remote: Some(Approver::Tui) });
        let far = b.sb.table().find(&id("peer-alpha"), &name("agent.parent.inbox"), WireMode::Send).cloned().unwrap();
        assert_eq!(far.grant, Grant { local: Approver::Tui, remote: Some(Approver::Tui) });

        a.sb.deliver(&id("parent"), to_b.as_str(), WireMode::Send, json!({"text": "hello beta"})).await.unwrap();
        let got = b.sb.drain_inbox(&id("parent"));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].hop, None, "alpha's session registered no turn hop");
        assert_eq!((got[0].sender.as_str(), got[0].text().as_str()), ("peer-alpha", "hello beta"));
        assert!(got[0].fenced().contains("<mesh-message from=\"peer-alpha\" receptacle=\"agent.parent.inbox\">"));
        // Logged on both ends; only alpha's parent has the wire, not its other agents.
        assert_eq!(a.log.0.lock().unwrap().len(), 1);
        assert_eq!(b.log.0.lock().unwrap().len(), 1);
        a.sb.plug(id("worker-1")).unwrap();
        assert!(a.sb.deliver(&id("worker-1"), to_b.as_str(), WireMode::Send, json!({"text": "x"})).await.is_err());
        assert!(b.sb.drain_inbox(&id("parent")).is_empty());
        assert!(pa.asked().is_empty(), "alpha's door asked nothing: its own session asked for the wire");
    }

    /// The person's rules in a short temp config folder: one mutual `send` rule over `/work`, where the test Marvs
    /// work (`marv`'s `/work/<session>`). Home is elsewhere, so `/work` may be a rule's folder.
    async fn work_rule(config: &Path) -> (marv_kernel::PolicyStore, marv_kernel::PolicyId) {
        let home = Path::new("/home/nobody");
        let store = marv_kernel::PolicyStore::new(config, home).unwrap();
        let work = marv_kernel::Subject::Folder(marv_kernel::FolderScope::new(Path::new("/work"), home).unwrap());
        let rule = marv_kernel::NewRule {
            from: work.clone(),
            to: work,
            mode: WireMode::Send,
            mutual: true,
            expires_at_ms: None,
            by: marv_kernel::RuleBy::Desktop,
            created_at_ms: 1,
            label: "work".into(),
        };
        let (id, _) = store.add(rule).await.unwrap();
        (store, id)
    }

    /// Who the main agent is: the session records it when the door opens (`mesh_process`); `marv` doesn't.
    fn main_agent(m: &Marv, session: &str) {
        let me = marv_kernel::AgentRef {
            id: None,
            mesh_name: session.into(),
            folder: PathBuf::from(format!("/work/{session}")),
            kind: marv_kernel::AgentKind::Terminal,
        };
        m.sb.set_identity(id("parent"), me);
    }

    /// Runtime-kernel 2b: a rule the person made wires two Marvs' main agents with no prompt on either end (it
    /// crosses as `peer_rule`, never as this end's rule), and revoking it ends the wire at the next message on both
    /// ends. A Marv that doesn't honor rules still asks its person; a worker plug is never covered.
    #[tokio::test]
    async fn a_rule_wires_two_marvs_without_asking_and_revoking_it_ends_the_wire() {
        let (mesh, config) = (Runtime::new(), Runtime::new());
        let (pa, pb, pc) = (Person::new(None), Person::new(None), Person::new(None));
        let (a, b, c) =
            (marv("alpha", &mesh.0, pa.clone()), marv("beta", &mesh.0, pb.clone()), marv("gamma", &mesh.0, pc.clone()));
        for (m, session) in [(&a, "alpha"), (&b, "beta"), (&c, "gamma")] {
            main_agent(m, session);
        }
        let (store, rule) = work_rule(&config.0).await;
        // alpha and beta honor the person's rules; gamma (say `marv -p`) doesn't.
        a.sb.set_rules(Arc::new(marv_kernel::FileRules::new(store.clone())));
        b.sb.set_rules(Arc::new(marv_kernel::FileRules::new(store.clone())));

        let to_b = name("peer.beta.agent.parent.inbox");
        a.sb.lookup(&to_b).await.unwrap();
        let ask = WireRequest { from: id("parent"), to: to_b.clone(), mode: WireMode::Send, asked_by: id("parent") };
        let by = a.sb.rule_for(&ask).await.unwrap();
        assert!(by.is_policy());
        a.sb.grant(ask, by).await.unwrap();
        assert!(pa.asked().is_empty() && pb.asked().is_empty(), "nobody was asked: the rule answered on both ends");
        let near = a.sb.table().find(&id("parent"), &to_b, WireMode::Send).cloned().unwrap();
        assert!(near.grant.local.is_policy());
        assert_eq!(near.grant.remote, Some(Approver::PeerRule), "beta's rule crossed as a word, not a RuleGrant");
        let far = b.sb.table().find(&id("peer-alpha"), &name("agent.parent.inbox"), WireMode::Send).cloned().unwrap();
        assert!(far.grant.local.is_policy());
        assert_eq!(far.grant.remote, Some(Approver::PeerRule));
        a.sb.deliver(&id("parent"), to_b.as_str(), WireMode::Send, json!({"text": "hello beta"})).await.unwrap();
        assert_eq!(b.sb.drain_inbox(&id("parent"))[0].text(), "hello beta");

        // A worker isn't covered, nor a Marv that doesn't honor rules: its person is asked (and says no).
        a.sb.plug(id("worker-1")).unwrap();
        let worker = WireRequest { from: id("worker-1"), to: to_b.clone(), mode: WireMode::Send, asked_by: id("worker-1") };
        assert_eq!(a.sb.rule_for(&worker).await, None);
        let to_c = name("peer.gamma.agent.parent.inbox");
        a.sb.lookup(&to_c).await.unwrap();
        let ask_c = WireRequest { from: id("parent"), to: to_c.clone(), mode: WireMode::Send, asked_by: id("parent") };
        let by = a.sb.rule_for(&ask_c).await.unwrap();
        assert!(a.sb.grant(ask_c, by).await.is_err(), "gamma's person said no");
        assert_eq!(pc.asked(), ["Wire peer-alpha -> agent.parent.inbox (send)"]);

        // Revoked: what it wired admits nothing from the next message, on either end.
        store.revoke(rule).await.unwrap();
        let err = a.sb.deliver(&id("parent"), to_b.as_str(), WireMode::Send, json!({"text": "again"})).await.unwrap_err();
        assert!(matches!(err, MeshError::RuleEnded { .. }), "{err}");
        let err = b.sb.authorize(&id("peer-alpha"), &name("agent.parent.inbox"), WireMode::Send).unwrap_err();
        assert!(matches!(err, MeshError::RuleEnded { .. }), "{err}");
        assert!(b.sb.drain_inbox(&id("parent")).is_empty());
        let ask = WireRequest { from: id("parent"), to: to_b, mode: WireMode::Send, asked_by: id("parent") };
        assert_eq!(a.sb.rule_for(&ask).await, None, "no rule, no grant: the person is asked again");
    }

    /// Kernel events kept in memory.
    #[derive(Default)]
    struct Events(Mutex<Vec<marv_kernel::KernelEvent>>);
    impl marv_kernel::EventLog for Events {
        fn append(&self, _at_ms: u64, event: marv_kernel::KernelEvent) {
            self.0.lock().unwrap().push(event);
        }
    }

    /// A chat to read: its turns, private or not, and the notices it was given.
    #[derive(Default)]
    struct Chat {
        private: std::sync::atomic::AtomicBool,
        notices: Mutex<Vec<String>>,
    }
    #[async_trait]
    impl crate::chat_read::ChatSource for Chat {
        async fn snapshot(&self) -> std::result::Result<crate::chat_read::ChatSnapshot, crate::chat_read::Unreadable> {
            use crate::chat_read::Turn;
            if self.private.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(crate::chat_read::Unreadable::Private);
            }
            Ok(crate::chat_read::ChatSnapshot {
                card: None,
                summary: None,
                turns: vec![Turn::Person("fix the login".into()), Turn::Tool("read_file".into()), Turn::Assistant("done".into())],
                mid_turn: None,
            })
        }
        fn read_by(&self, reader: &str, ask: crate::chat_read::ReadAsk) {
            self.notices.lock().unwrap().push(crate::chat_read::notice_line(reader, ask));
        }
    }

    /// Runtime-kernel §9.2: a read needs a `read` rule the person made, checked on both ends (a `send` rule alone
    /// doesn't do); a private chat refuses under one too; a served read tells the read chat and is logged without its
    /// text; the reply comes back fenced.
    #[tokio::test]
    async fn chat_read_needs_a_read_rule_and_refuses_a_private_chat() {
        use crate::chat_read::{ReadAsk, read_chat};
        use marv_kernel::{KernelEvent, ReadOutcome, ReadRefusal};
        let (mesh, config) = (Runtime::new(), Runtime::new());
        let (a, b) = (marv("alpha", &mesh.0, Person::new(None)), marv("beta", &mesh.0, Person::new(None)));
        main_agent(&a, "alpha");
        main_agent(&b, "beta");
        let (store, _send) = work_rule(&config.0).await;
        for m in [&a, &b] {
            m.sb.set_rules(Arc::new(marv_kernel::FileRules::new(store.clone())));
        }
        let (ea, eb) = (Arc::new(Events::default()), Arc::new(Events::default()));
        a.sb.set_events(ea.clone());
        b.sb.set_events(eb.clone());
        let chat = Arc::new(Chat::default());
        b.sb.set_chat_source(chat.clone());

        // A send rule alone: refused on alpha's side, before beta is asked.
        let err = read_chat(&a.sb, "beta", ReadAsk::summary()).await.unwrap_err();
        assert_eq!(err.reason, ReadRefusal::NoRule, "{}", err.text);
        assert!(eb.0.lock().unwrap().is_empty() && chat.notices.lock().unwrap().is_empty());
        // Asked directly at beta's door, beta refuses it too: its host checks the rule itself.
        let mut door = DoorClient::connect(&mesh.0.join("beta.sock")).await.unwrap();
        let answer = door.request(crate::chat_read::READ_METHOD, ReadAsk::summary().params("alpha")).await.unwrap();
        assert_eq!(answer, json!({"served": false, "reason": "no_rule"}));
        // A caller that isn't who listens on <from>.sock is refused outright.
        assert!(door.request(crate::chat_read::READ_METHOD, ReadAsk::summary().params("gamma")).await.is_err());

        // A read rule over beta's folder, one way: alpha may read beta, not the other way.
        let home = Path::new("/home/nobody");
        let read = marv_kernel::NewRule {
            from: marv_kernel::Subject::Folder(marv_kernel::FolderScope::new(Path::new("/work/alpha"), home).unwrap()),
            to: marv_kernel::Subject::Folder(marv_kernel::FolderScope::new(Path::new("/work/beta"), home).unwrap()),
            mode: WireMode::Read,
            mutual: false,
            expires_at_ms: None,
            by: marv_kernel::RuleBy::Cli,
            created_at_ms: 2,
            label: "alpha reads beta".into(),
        };
        store.add(read).await.unwrap();
        let text = read_chat(&a.sb, "beta", ReadAsk::tail(5)).await.unwrap();
        assert!(text.contains("<untrusted-data source=\"chat-read\" from=\"beta\">"), "{text}");
        assert!(text.contains(r#"{"person":"fix the login"}"#) && text.contains(r#"{"tools":["read_file"]}"#), "{text}");
        assert_eq!(*chat.notices.lock().unwrap(), ["Read by alpha · last 5 turns"]);
        assert_eq!(read_chat(&b.sb, "alpha", ReadAsk::summary()).await.unwrap_err().reason, ReadRefusal::NoRule);
        // No wire was made by it, and nothing went to an inbox.
        assert!(a.sb.table().all().is_empty() && b.sb.table().all().is_empty());
        assert!(b.sb.drain_inbox(&id("parent")).is_empty());

        // Private: refused under the rule too.
        chat.private.store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(read_chat(&a.sb, "beta", ReadAsk::summary()).await.unwrap_err().reason, ReadRefusal::Private);
        assert_eq!(chat.notices.lock().unwrap().len(), 1, "a refused read isn't told to the chat");

        // The log: beta's served and refused reads, alpha's own refusals; never a word of the chat.
        let reads = |e: &Events| -> Vec<(ReadOutcome, Option<ReadRefusal>, usize)> {
            e.0.lock()
                .unwrap()
                .iter()
                .filter_map(|e| match e {
                    KernelEvent::Read { outcome, reason, size, .. } => Some((*outcome, *reason, *size)),
                    _ => None,
                })
                .collect()
        };
        let beta = reads(&eb);
        assert_eq!(
            beta.iter().map(|(o, r, _)| (*o, *r)).collect::<Vec<_>>(),
            [
                (ReadOutcome::Refused, Some(ReadRefusal::NoRule)),
                (ReadOutcome::Served, None),
                // beta's own read of alpha, refused on its side.
                (ReadOutcome::Refused, Some(ReadRefusal::NoRule)),
                (ReadOutcome::Refused, Some(ReadRefusal::Private)),
            ]
        );
        assert!(beta[1].2 > 0);
        assert_eq!(reads(&ea), [(ReadOutcome::Refused, Some(ReadRefusal::NoRule), 0)]);
        let logged: String = eb.0.lock().unwrap().iter().map(|e| marv_kernel::LoggedEvent::new(1, e.clone()).line()).collect();
        assert!(!logged.contains("fix the login") && !logged.contains("done"), "{logged}");
    }

    /// Runtime-kernel §9.2: a reader reads at most READ_BURST at once, then waits.
    #[tokio::test]
    async fn chat_reads_are_rate_limited_per_reader() {
        use crate::chat_read::{READ_BURST, ReadAsk, read_chat};
        let (mesh, config) = (Runtime::new(), Runtime::new());
        let (a, b) = (marv("alpha", &mesh.0, Person::new(None)), marv("beta", &mesh.0, Person::new(None)));
        main_agent(&a, "alpha");
        main_agent(&b, "beta");
        let home = Path::new("/home/nobody");
        let store = marv_kernel::PolicyStore::new(&config.0, home).unwrap();
        let work = marv_kernel::Subject::Folder(marv_kernel::FolderScope::new(Path::new("/work"), home).unwrap());
        let rule = marv_kernel::NewRule {
            from: work.clone(),
            to: work,
            mode: WireMode::Read,
            mutual: true,
            expires_at_ms: None,
            by: marv_kernel::RuleBy::Desktop,
            created_at_ms: 1,
            label: String::new(),
        };
        store.add(rule).await.unwrap();
        for m in [&a, &b] {
            m.sb.set_rules(Arc::new(marv_kernel::FileRules::new(store.clone())));
        }
        b.sb.set_chat_source(Arc::new(Chat::default()));
        for _ in 0..READ_BURST {
            read_chat(&a.sb, "beta", ReadAsk::summary()).await.unwrap();
        }
        let err = read_chat(&a.sb, "beta", ReadAsk::summary()).await.unwrap_err();
        assert_eq!(err.reason, marv_kernel::ReadRefusal::Limit);
        // Without a source (a Marv whose host serves no reads) it's unavailable.
        assert_eq!(
            read_chat(&b.sb, "alpha", ReadAsk::summary()).await.unwrap_err().reason,
            marv_kernel::ReadRefusal::Unavailable
        );
    }

    /// Runtime-kernel 2b: a message's hop crosses to the other Marv with it, through the plug beta gave alpha.
    #[tokio::test]
    async fn the_hop_crosses_to_the_other_marv() {
        let mesh = Runtime::new();
        let (a, b) = (marv("alpha", &mesh.0, Person::new(None)), marv("beta", &mesh.0, Person::new(Some(Approver::Tui))));
        let hop = Arc::new(crate::hop::TurnHop::default());
        hop.set(1);
        a.sb.set_turn_hop(id("parent"), hop);
        let to_b = name("peer.beta.agent.parent.inbox");
        a.sb.lookup(&to_b).await.unwrap();
        let ask = WireRequest { from: id("parent"), to: to_b.clone(), mode: WireMode::Send, asked_by: id("parent") };
        a.sb.grant(ask, Approver::Tui).await.unwrap();
        a.sb.deliver(&id("parent"), to_b.as_str(), WireMode::Send, json!({"text": "at hop 1"})).await.unwrap();
        let got = b.sb.drain_inbox(&id("parent"));
        assert_eq!((got[0].sender.as_str(), got[0].hop), ("peer-alpha", Some(1)));
    }

    /// Runtime-kernel 2b: once beta's person wired alpha's plug, beta knows who it is from alpha's own pid-confirmed
    /// `marv/info`, for the plug's life. Knowing grants nothing: an identity with no wire is still refused.
    #[tokio::test]
    async fn a_wired_peers_identity_comes_from_its_confirmed_info_and_grants_nothing() {
        let mesh = Runtime::new();
        let (a, b) = (marv("alpha", &mesh.0, Person::new(None)), marv("beta", &mesh.0, Person::new(Some(Approver::Tui))));
        let c = marv("gamma", &mesh.0, Person::new(None));
        // beta asks for a wire to alpha: alpha's person says no, so alpha learns nothing about beta.
        let to_a = name("peer.alpha.agent.parent.inbox");
        b.sb.lookup(&to_a).await.unwrap();
        let ask = WireRequest { from: id("parent"), to: to_a, mode: WireMode::Send, asked_by: id("parent") };
        assert!(b.sb.grant(ask, Approver::Tui).await.is_err());
        assert_eq!(a.sb.identity(&id("peer-beta")), None);
        // gamma asks beta, whose person says yes: beta now knows gamma (a terminal Marv: no chat id).
        let to_b = name("peer.beta.agent.parent.inbox");
        c.sb.lookup(&to_b).await.unwrap();
        let ask = WireRequest { from: id("parent"), to: to_b.clone(), mode: WireMode::Send, asked_by: id("parent") };
        c.sb.grant(ask, Approver::Cli).await.unwrap();
        let who = b.sb.identity(&id("peer-gamma")).unwrap();
        assert_eq!(who.mesh_name, "gamma");
        assert_eq!(who.folder, PathBuf::from("/work/gamma"));
        assert_eq!((who.id.clone(), who.kind), (None, marv_kernel::AgentKind::Terminal));
        // A desktop chat's info carries its id.
        let beta_info = DoorClient::connect(&mesh.0.join("beta.sock")).await.unwrap().info().await.unwrap();
        let beta = agent_ref(&beta_info);
        assert_eq!((beta.kind, beta.id.map(|i| i.to_string())), (marv_kernel::AgentKind::Chat, beta_info.chat));
        // An identity alone admits nothing: the one check asks the person's wires only.
        b.sb.plug(id("worker-1")).unwrap();
        b.sb.set_identity(id("worker-1"), who);
        assert!(b.sb.authorize(&id("worker-1"), &name("agent.parent.inbox"), WireMode::Send).is_err());
        assert!(b.sb.authorize(&id("peer-gamma"), &name("agent.parent.inbox"), WireMode::Send).is_ok());
        // Gone with its plug.
        b.sb.unplug(&id("peer-gamma"));
        assert_eq!(b.sb.identity(&id("peer-gamma")), None);
    }

    #[tokio::test]
    async fn the_far_user_saying_no_leaves_it_unwired() {
        let mesh = Runtime::new();
        let (a, b) = (marv("alpha", &mesh.0, Person::new(None)), marv("beta", &mesh.0, Person::new(None)));
        let to_b = name("peer.beta.agent.parent.inbox");
        a.sb.lookup(&to_b).await.unwrap();
        let ask = WireRequest { from: id("parent"), to: to_b.clone(), mode: WireMode::Send, asked_by: id("parent") };
        let err = a.sb.grant(ask, Approver::Tui).await.unwrap_err();
        assert_eq!(err.to_string(), "the Marv \"beta\" didn't wire it: its user said no");
        assert!(a.sb.table().all().is_empty());
        assert!(b.sb.table().all().is_empty());
        assert!(!b.sb.has_plug(&id("peer-alpha")));
        // A one-ended wire built by hand doesn't count either: a peer receptacle is always cross-process.
        a.sb.wire(crate::Wire::new(id("parent"), to_b.clone(), WireMode::Send, Grant::local(Approver::Tui))).unwrap();
        let err = a.sb.deliver(&id("parent"), to_b.as_str(), WireMode::Send, json!({"text": "x"})).await.unwrap_err();
        assert!(matches!(err, MeshError::NotWired { .. }), "{err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_door_plugs_new_names_only_and_unplugs_them_on_leaving() {
        let mesh = Runtime::new();
        let a = marv("alpha", &mesh.0, Person::new(None));
        let mut door = DoorClient::connect(&mesh.0.join("alpha.sock")).await.unwrap();
        for taken in ["parent", "worker-1", "peer-beta", "Bad Name"] {
            assert!(door.request(PLUG, json!({"name": taken})).await.is_err(), "{taken}");
        }
        let answer = door.request(PLUG, json!({"name": "claude"})).await.unwrap();
        let socket = PathBuf::from(answer["socket"].as_str().unwrap());
        assert_eq!(mode(&socket), 0o600);
        assert!(door.request(PLUG, json!({"name": "claude"})).await.is_err(), "not twice");
        // Its socket is a plug like any other: nothing wired, nothing listed.
        let mut plug = PlugClient::connect(&socket).await.unwrap();
        assert!(plug.list().await.unwrap().is_empty());
        assert!(a.sb.has_plug(&id("claude")));
        drop(door);
        for _ in 0..100 {
            if !a.sb.has_plug(&id("claude")) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!a.sb.has_plug(&id("claude")));
    }

    #[tokio::test]
    async fn marv_mesh_wire_asks_each_end() {
        let mesh = Runtime::new();
        let (pa, pb) = (Person::new(Some(Approver::Tui)), Person::new(Some(Approver::Headless)));
        let (a, b) = (marv("alpha", &mesh.0, pa.clone()), marv("beta", &mesh.0, pb.clone()));
        a.sb.plug(id("worker-1")).unwrap();
        let mut door = DoorClient::connect(&mesh.0.join("alpha.sock")).await.unwrap();
        // Within one Marv: its user only.
        let local = door.request(CONNECT, json!({"from": "worker-1", "to": "agent.parent.inbox", "mode": "send"})).await.unwrap();
        assert_eq!(local["wired"], true);
        assert_eq!(local["cross_process"], false);
        // To another Marv: both users.
        let far =
            door.request(CONNECT, json!({"from": "parent", "to": "peer.beta.agent.parent.inbox", "mode": "send"})).await.unwrap();
        assert_eq!(far["wired"], true, "{far}");
        assert_eq!(far["cross_process"], true);
        assert_eq!(
            pa.asked(),
            ["Wire worker-1 -> agent.parent.inbox (send)", "Wire parent -> peer.beta.agent.parent.inbox (send)"]
        );
        assert_eq!(pb.asked(), ["Wire peer-alpha -> agent.parent.inbox (send)"]);
        assert!(b.sb.table().allows(&id("peer-alpha"), &name("agent.parent.inbox"), WireMode::Send));
        // Unknown agents and receptacles are errors, never asked about.
        assert!(door.request(CONNECT, json!({"from": "ghost", "to": "agent.parent.inbox", "mode": "send"})).await.is_err());
        assert!(door.request(CONNECT, json!({"from": "parent", "to": "peer.beta.agent.x.inbox", "mode": "send"})).await.is_err());
        assert!(door.request(CONNECT, json!({"from": "parent", "to": "agent.worker-1.inbox", "mode": "call"})).await.is_err());
        assert_eq!(pa.asked().len(), 2);
    }

    /// A person who's away (#201): asked, never answers; counts the asks and the ones withdrawn (the wait dropped).
    #[derive(Default)]
    struct Away {
        asked: tokio::sync::Notify,
        withdrawn: Arc<tokio::sync::Notify>,
        count: Arc<std::sync::atomic::AtomicUsize>,
    }
    struct Withdrawn(Arc<tokio::sync::Notify>, Arc<std::sync::atomic::AtomicUsize>);
    impl Drop for Withdrawn {
        fn drop(&mut self) {
            self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.0.notify_one();
        }
    }
    #[async_trait]
    impl PeerApprover for Away {
        async fn approve_wire(&self, _request: WireRequest) -> Option<Approver> {
            let _gone = Withdrawn(self.withdrawn.clone(), self.count.clone());
            self.asked.notify_one();
            std::future::pending().await
        }
    }

    fn away_marv(session: &str, mesh: &Path, away: Arc<Away>, wait: Duration) -> (Arc<Switchboard>, DoorGuard, Runtime) {
        let runtime = Runtime::new();
        let sb = Arc::new(Switchboard::new(session, runtime.0.clone(), Arc::new(Log::default())));
        sb.plug(id("parent")).unwrap();
        sb.set_wire_wait(wait);
        let info = DoorInfo { cwd: PathBuf::from("/work"), pane: None, chat: None };
        let door = open_door(sb.clone(), mesh, info, away).unwrap();
        (sb, door, runtime)
    }

    /// #201: a caller that hangs up while the far person's prompt waits (its turn was stopped, or it gave up) takes
    /// the prompt with it: the door stops waiting, and nothing is plugged or wired there.
    #[tokio::test]
    async fn a_caller_hanging_up_withdraws_the_far_prompt() {
        let mesh = Runtime::new();
        let _alpha = marv("alpha", &mesh.0, Person::new(Some(Approver::Tui)));
        let away = Arc::new(Away::default());
        let (beta, _door, _rt) = away_marv("beta", &mesh.0, away.clone(), Duration::from_secs(3600));
        let mut door = DoorClient::connect(&mesh.0.join("beta.sock")).await.unwrap();
        let (asked, withdrawn) = (away.asked.notified(), away.withdrawn.notified());
        let ask = json!({"from": "alpha", "to": "agent.parent.inbox", "mode": "send", "by": "tui"});
        tokio::select! {
            _ = door.request(WIRE, ask) => panic!("nobody answered"),
            () = asked => {}
        }
        drop(door);
        tokio::time::timeout(Duration::from_secs(30), withdrawn).await.expect("withdrawn when the caller hung up");
        assert_eq!(away.count.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(beta.table().all().is_empty());
        assert!(!beta.has_plug(&id("peer-alpha")));
    }

    /// #201: the far door doesn't wait for its person forever either. After the wait, the prompt is withdrawn and the
    /// caller reads that nobody answered (not that the person said no).
    #[tokio::test]
    async fn the_far_door_withdraws_a_prompt_nobody_answers() {
        let mesh = Runtime::new();
        let alpha = marv("alpha", &mesh.0, Person::new(Some(Approver::Tui)));
        let away = Arc::new(Away::default());
        let (beta, _door, _rt) = away_marv("beta", &mesh.0, away.clone(), Duration::from_millis(200));
        // The far door answers first: its "unanswered" reads as nobody answering.
        let mut door = DoorClient::connect(&mesh.0.join("beta.sock")).await.unwrap();
        let ask = json!({"from": "alpha", "to": "agent.parent.inbox", "mode": "send", "by": "tui"});
        let answer = tokio::time::timeout(Duration::from_secs(30), door.request(WIRE, ask)).await.unwrap().unwrap();
        assert_eq!(answer, json!({"granted": false, "unanswered": true}));
        assert_eq!(away.count.load(std::sync::atomic::Ordering::SeqCst), 1, "withdrawn");
        // Through alpha's switchboard (whose own wait is minutes): the far door's answer ends it, in the same words.
        let to_b = name("peer.beta.agent.parent.inbox");
        alpha.sb.lookup(&to_b).await.unwrap();
        let ask = WireRequest { from: id("parent"), to: to_b, mode: WireMode::Send, asked_by: id("parent") };
        let err = tokio::time::timeout(Duration::from_secs(30), alpha.sb.grant(ask, Approver::Tui)).await.unwrap().unwrap_err();
        assert!(err.to_string().starts_with("the Marv \"beta\"'s person didn't answer within "), "{err}");
        assert!(alpha.sb.table().all().is_empty() && beta.table().all().is_empty());
        assert!(!beta.has_plug(&id("peer-alpha")));
    }

    #[test]
    fn waits_in_words() {
        assert_eq!(words(WIRE_ANSWER_WAIT), "3 minutes");
        assert_eq!(words(Duration::from_secs(60)), "1 minute");
        assert_eq!(words(Duration::from_secs(45)), "45 seconds");
        assert_eq!(words(Duration::from_millis(200)), "200 ms");
    }

    #[tokio::test]
    async fn the_far_end_checks_the_caller_by_pid() {
        let mesh = Runtime::new();
        let pb = Person::new(Some(Approver::Tui));
        let _b = marv("beta", &mesh.0, pb.clone());
        let mut door = DoorClient::connect(&mesh.0.join("beta.sock")).await.unwrap();
        let ask = |from: &str| json!({"from": from, "to": "agent.parent.inbox", "mode": "send", "by": "tui"});
        // Nobody listens as "ghost", and beta isn't its own peer.
        assert!(door.request(WIRE, ask("ghost")).await.is_err());
        assert!(door.request(WIRE, ask("beta")).await.is_err());
        // Someone else's process listens as "mallory": this caller isn't it.
        let Ok(mut other) = std::process::Command::new("python3")
            .arg("-c")
            .arg("import socket,sys,os,time\ns=socket.socket(socket.AF_UNIX)\ns.bind(sys.argv[1])\nos.chmod(sys.argv[1],0o600)\ns.listen(4)\nprint('up',flush=True)\nwhile True:\n  c,_=s.accept()")
            .arg(mesh.0.join("mallory.sock"))
            .stdout(std::process::Stdio::piped())
            .spawn()
        else {
            eprintln!("skipped the pid half: no python3");
            return;
        };
        let mut up = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(other.stdout.take().unwrap()), &mut up).unwrap();
        let err = door.request(WIRE, ask("mallory")).await.unwrap_err();
        assert_eq!(err.to_string(), "can't confirm the caller is the Marv \"mallory\"");
        assert!(pb.asked().is_empty(), "never asked about a caller it couldn't confirm");
        let _ = other.kill();
        let _ = other.wait();
    }
}
