//! A Claude Code chat's mesh tools (the harness plan, phase 3): an MCP server on a Unix socket of its own, serving
//! Marv's `mesh_list`, `mesh_call`, `mesh_send` and `mesh_wait` for the chat's main agent, and (consistency finding
//! H20) the team board, `chat_read` and `wake_me`, as a Marv member has them ([`marv_session::mesh::harness_tools`]).
//! What one call leaves for the next is the chat's [`HarnessCalls`]: its taint (after a `chat_read`, a board post or a
//! rule's send asks, as in a Marv chat: no read-and-relay) and whether the running turn is the person's (`chat_read`
//! runs only in theirs). Claude Code starts `<marv> __mesh-mcp <socket>`
//! ([`marv_session::helper_modes`]), which joins its stdio to this socket.
//!
//! - The calls run here, in Marv, through `run_tool` with the chat's approver: a send on a wire nobody made asks the
//!   person (a card in the chat), a rule the person made answers first, the switchboard checks, limits and logs every
//!   message, as for a Marv chat's model. Claude Code's own permission prompt for these tools is skipped
//!   (`--allowedTools`): Marv's rules decide.
//! - The socket is 0600, in the chat's mesh folder (Marv's runtime folder, 0700); it goes when the server does.
//! - What the harness sends is never clean mail: Marv can't see what it read.
//! - A Marv chat reads its inbox between steps, so `mesh_wait` only says who wrote. A harness has no such step: unread
//!   mail is read after each of these calls and added to its result, fenced as other agents' words (the same note).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use marv_session::mesh::MeshHandle;
use marv_tools::{Approver, ToolContext, ToolRef};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

/// What a harness chat's calls carry from one to the next, kept with the chat (it outlives a sleep): its taint, as a
/// Marv session's, and whether its running turn is unattended (a timer's or mail's: set when the turn starts,
/// `crate::harness_budget`).
#[derive(Clone, Default)]
pub struct HarnessCalls {
    pub taint: Arc<marv_sandbox::TaintState>,
    pub woken: Arc<std::sync::atomic::AtomicBool>,
}

impl HarnessCalls {
    /// A call's context in `root`: the chat's taint, and its turn's kind.
    fn context(&self, root: &Path) -> ToolContext {
        let mut ctx = ToolContext::new(root);
        ctx.taint = self.taint.clone();
        let woken = self.woken.load(std::sync::atomic::Ordering::SeqCst);
        ctx.mode = if woken { marv_tools::floor::CallMode::Woken } else { marv_tools::floor::CallMode::Person };
        ctx
    }
}

/// The MCP server's name in Claude Code's config: its tools are `mcp__marv-mesh__<tool>`.
pub const SERVER: &str = "marv-mesh";

/// A running server: dropping it stops it and removes its socket.
pub struct MeshMcp {
    pub socket: PathBuf,
    stop: CancellationToken,
}

impl Drop for MeshMcp {
    fn drop(&mut self) {
        self.stop.cancel();
        let _ = std::fs::remove_file(&self.socket);
    }
}

impl MeshMcp {
    /// Claude Code's `--mcp-config`: this server, started as `exe __mesh-mcp <socket>`.
    pub fn config(&self, exe: &Path) -> String {
        Self::config_at(exe, &self.socket)
    }

    /// [`Self::config`] with the helper and the socket where Claude Code sees them (inside its outer sandbox, phase 4).
    pub fn config_at(exe: &Path, socket: &Path) -> String {
        json!({"mcpServers": {SERVER: {
            "type": "stdio",
            "command": exe.display().to_string(),
            "args": [marv_session::helper_modes::MESH_MCP_SUBCOMMAND, socket.display().to_string()],
        }}})
        .to_string()
    }

    /// Claude Code's `--allowedTools` for these tools (Marv asks for what needs asking).
    pub fn allowed() -> String {
        marv_session::mesh::harness_specs().iter().map(|s| format!("mcp__{SERVER}__{}", s.name)).collect::<Vec<_>>().join(",")
    }
}

/// Serves `tools` on `socket` (made fresh, 0600) for a chat in `root`, asking through `approver`.
pub fn serve(
    socket: PathBuf,
    root: PathBuf,
    mesh: MeshHandle,
    tools: Vec<ToolRef>,
    approver: Arc<dyn Approver>,
    calls: HarnessCalls,
) -> std::io::Result<MeshMcp> {
    let _ = std::fs::remove_file(&socket);
    let listener = marv_fs::local_socket::bind_private(&socket)?;
    let stop = CancellationToken::new();
    let (tools, until) = (Arc::new(tools), stop.clone());
    tokio::spawn(async move {
        loop {
            let stream = tokio::select! {
                accepted = listener.accept() => match accepted {
                    Ok((stream, _)) => stream,
                    Err(_) => return,
                },
                _ = until.cancelled() => return,
            };
            let (tools, approver, root, until, mesh, calls) =
                (tools.clone(), approver.clone(), root.clone(), until.clone(), mesh.clone(), calls.clone());
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                loop {
                    let line = tokio::select! {
                        line = lines.next_line() => match line {
                            Ok(Some(line)) => line,
                            _ => return,
                        },
                        _ = until.cancelled() => return,
                    };
                    let Ok(request) = serde_json::from_str::<Value>(&line) else { continue };
                    if let Some(reply) = answer(&request, &mesh, &tools, &root, &approver, &calls, &until).await
                        && write.write_all(format!("{reply}\n").as_bytes()).await.is_err()
                    {
                        return;
                    }
                }
            });
        }
    });
    Ok(MeshMcp { socket, stop })
}

/// The reply to one JSON-RPC message (None for a notification).
pub async fn answer(
    request: &Value,
    mesh: &MeshHandle,
    tools: &[ToolRef],
    root: &Path,
    approver: &Arc<dyn Approver>,
    calls: &HarnessCalls,
    stop: &CancellationToken,
) -> Option<Value> {
    let id = request.get("id")?.clone();
    let result = match request["method"].as_str() {
        Some("initialize") => json!({
            "protocolVersion": request["params"]["protocolVersion"].as_str().unwrap_or(marv_mcp::jsonrpc::PROTOCOL_VERSION),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": SERVER, "version": env!("CARGO_PKG_VERSION")},
        }),
        Some("ping") => json!({}),
        Some("tools/list") => {
            // What this chat really has (`wake_me` only where the host has timers).
            let list: Vec<Value> = tools
                .iter()
                .map(|t| t.spec())
                .map(|s| json!({"name": s.name, "description": s.description, "inputSchema": s.parameters}))
                .collect();
            json!({"tools": list})
        }
        Some("tools/call") => {
            let name = request["params"]["name"].as_str().unwrap_or_default().to_string();
            let arguments = request["params"].get("arguments").cloned().unwrap_or_else(|| json!({}));
            let call = marv_core::ToolCall { id: uuid::Uuid::new_v4().to_string(), name, arguments: arguments.to_string() };
            let mut ctx = calls.context(root);
            ctx.approver = Some(approver.clone());
            ctx.cancel = stop.child_token();
            let outcome = marv_tools::registry::run_tool(&call, &ctx, tools).await;
            let mut text = outcome.result.output;
            if let Some(mail) = marv_session::mesh::harness_inbox(mesh) {
                text = format!("{text}\n\n{mail}");
            }
            json!({"content": [{"type": "text", "text": text}], "isError": outcome.result.is_error})
        }
        _ => {
            return Some(
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "Marv's mesh server doesn't do that."}}),
            );
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}
