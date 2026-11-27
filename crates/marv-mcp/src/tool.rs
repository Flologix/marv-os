//! An MCP server's tool as one of Marv's tools. Ports `src/mcp/tool.ts`.
//!
//! - The model sees it as `mcp__<server>__<tool>` (sanitized, at most 64 chars:
//!   OpenAI-compatible APIs reject longer names; a hash keeps a shortened name
//!   unique), with the server's own description (prefixed with the server's
//!   name) and JSON schema ([`acceptable_schema`]: one tool a provider rejects
//!   fails every request of the session, since the tool list can't change).
//! - MCP servers run outside the sandbox, with the user's permissions and
//!   network, so a call asks first, scoped per tool, and **yolo never skips
//!   it** (no `autoSafe`: nothing contains what it does).
//! - Results: text blocks joined, images/audio *named but not sent* (Marv passes
//!   text only; images are a taint and exfiltration risk, docs/SECURITY.md),
//!   structured content as JSON, capped at 30k chars (head and tail kept). The session's adapter runs
//!   [`McpTool::run_whole`] and cuts it itself, saving a cut result whole (#61).
//! - A server error or a dead connection becomes an error result (`is_error`),
//!   since its text can be the server's own; a
//!   [`crate::oauth::LoginNeeded`] passes Marv's own sentence through as is (an
//!   `Err`), so the model tells the user to log in rather than retrying.
//!
//! **Difference from Marv (security fix, 2026-10-12 audit):** a `readOnlyHint`
//! tool is [`ToolKind::Read`] only while the session is untainted. Once web,
//! MCP, mesh or image text has entered the context, every MCP call is
//! [`ToolKind::Egress`] and asks even in yolo: a "read-only" tool still carries
//! its arguments to a server outside the sandbox, so an injected model could
//! leak data through them ([`McpTool::kind`]). One exception, as for GitHub
//! reads: a read-only tool of a *remote server you added yourself*
//! ([`McpTool::account_read`]: Notion, Linear, Gmail in `~/.marv/mcp.json`)
//! still runs unasked, since its arguments only reach that service, on your
//! own account. Every read of a Notion page or Linear issue asked, every time,
//! with no "always": about 9 prompts to file one test issue.
//!
//! This crate doesn't see `marv-tools`' `Tool` trait; `marv-session` wraps an
//! [`McpTool`] in it (name, spec, kind, scope, preview, run).

use std::sync::Arc;

use async_trait::async_trait;
use marv_core::{ToolKind, ToolResult, ToolSpec};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::McpError;
use crate::config::{McpServerConfig, ServerSource};
use crate::input::{Answers, Confirmation, input_required};
use crate::ordered::Ordered;
use crate::text::{printable, sha256_hex, truncate};

/// Tool names longer than this are rejected by OpenAI-compatible APIs.
pub const MAX_NAME: usize = 64;
pub const MAX_DESCRIPTION: usize = 2000;
/// A result is capped at this many chars: the first third and last two thirds are kept.
pub const MAX_OUTPUT: usize = 30_000;
/// The approval shows at most this much of the arguments.
pub const PREVIEW_CHARS: usize = 1500;

/// What a server says about one of its tools (`tools/list`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub input_schema: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<McpToolAnnotations>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolAnnotations {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// One content block of a `tools/call` result (loose: unknown types are named).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentBlock {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Base64 (image/audio).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<EmbeddedResource>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EmbeddedResource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// A `tools/call` result.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCallResult {
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    #[serde(default)]
    pub is_error: bool,
    /// An `InputRequiredResult` in the draft's shape ([`crate::input`]): `input_required`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_requests: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_state: Option<String>,
}

/// How a tool reaches its server: the manager's connection, by the server's own tool name.
#[async_trait]
pub trait McpCaller: Send + Sync {
    async fn call(&self, tool: &str, args: Map<String, Value>, cancel: CancellationToken) -> crate::Result<McpCallResult>;

    /// The retry of a call the server asked about, with the person's answers ([`crate::input`]). A caller that can't
    /// send answers sends the call again without them.
    async fn call_answering(
        &self,
        tool: &str,
        args: Map<String, Value>,
        _answers: &Answers,
        cancel: CancellationToken,
    ) -> crate::Result<McpCallResult> {
        self.call(tool, args, cancel).await
    }
}

/// The person's answer to a server's question during a call ([`crate::input`]), through the session's approval
/// prompt.
#[async_trait]
pub trait Confirmer: Send + Sync {
    /// The value chosen from `confirmation`'s choices, or `None` to decline. `tool` is the asking tool.
    async fn confirm(&self, tool: &McpTool, confirmation: &Confirmation) -> Option<String>;
}

/// No one to ask (a caller without a session): every question is declined.
pub struct NoConfirmer;

#[async_trait]
impl Confirmer for NoConfirmer {
    async fn confirm(&self, _tool: &McpTool, _confirmation: &Confirmation) -> Option<String> {
        None
    }
}

/// How many times one call may ask before Marv gives up on it (each question is the person's to answer).
pub const MAX_QUESTIONS: usize = 12;

/// Only characters every provider accepts.
fn clean(text: &str) -> String {
    text.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect()
}

/// `mcp__<server>__<tool>`, only characters every provider accepts, at most 64
/// long (a hash of the original keeps a shortened or cleaned name unique).
pub fn mcp_tool_name(server: &str, tool: &str) -> String {
    let cleaned = clean(tool);
    let name = format!("mcp__{server}__{cleaned}");
    if name.len() <= MAX_NAME && cleaned == tool {
        return name;
    }
    let hash = &sha256_hex(tool.as_bytes())[..8];
    format!("{}_{hash}", &name[..name.len().min(MAX_NAME - 9)])
}

/// The model reads text: images and other binary blocks are named, not sent. Capped at [`MAX_OUTPUT`].
pub fn format_mcp_result(result: &McpCallResult) -> String {
    cut_mcp_output(&mcp_result_text(result))
}

/// The result as text, whole ([`format_mcp_result`] before the cap): the session saves a cut one (#61).
pub fn mcp_result_text(result: &McpCallResult) -> String {
    let parts: Vec<String> = result
        .content
        .iter()
        .map(|block| match block.kind.as_str() {
            "text" => block.text.clone().unwrap_or_default(),
            "image" | "audio" => {
                let kb = (block.data.as_ref().map_or(0, String::len) as f64 * 3.0 / 4.0 / 1024.0).round();
                format!(
                    "[{} ({}), {kb} KB, not shown: Marv passes text only]",
                    block.kind,
                    block.mime_type.as_deref().unwrap_or("unknown type")
                )
            }
            "resource" => {
                let r = block.resource.clone().unwrap_or_default();
                r.text.unwrap_or_else(|| format!("[resource {}]", r.uri.unwrap_or_default()))
            }
            "resource_link" => {
                let named = block.name.as_ref().map(|n| format!(" ({n})")).unwrap_or_default();
                format!("[link: {}{named}]", block.uri.as_deref().unwrap_or_default())
            }
            other => format!("[{other} content, not shown]"),
        })
        .collect();
    let mut text = parts.join("\n");
    if text.is_empty()
        && let Some(s) = &result.structured_content
    {
        text = serde_json::to_string_pretty(s).unwrap_or_default();
    }
    if text.is_empty() {
        text = "(no output)".into();
    }
    // Bidi/format and C0/C1 controls out before the person or the model reads it (docs/SECURITY.md).
    printable(&text)
}

/// The first third and the last two thirds of [`MAX_OUTPUT`].
pub fn cut_mcp_output(text: &str) -> String {
    let len = text.chars().count();
    if len <= MAX_OUTPUT {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX_OUTPUT / 3).collect();
    let tail: String = text.chars().skip(len - 2 * MAX_OUTPUT / 3).collect();
    format!("{head}\n\n… ({} characters omitted) …\n\n{tail}", len - MAX_OUTPUT)
}

/// Argument names that say what a call acts on, best first: shown before bulk content (`write_file`'s `content`).
const TARGET_KEYS: [&str; 10] = ["path", "file_path", "filepath", "file", "filename", "url", "uri", "name", "id", "query"];

/// For the transcript and the approval: the meaningful target (path, url, name...) if there is one, else the first
/// non-empty string argument, else the arguments as JSON; sanitized and at most 60 chars.
pub fn label_of(args: &Map<String, Value>) -> String {
    let target = TARGET_KEYS.iter().find_map(|k| args.get(*k).and_then(Value::as_str).filter(|s| !s.is_empty()));
    let first = target.or_else(|| args.values().find_map(|v| v.as_str().filter(|s| !s.is_empty())));
    let text = match first {
        Some(s) => s.to_string(),
        None if args.is_empty() => String::new(),
        None => Value::Object(args.clone()).to_string(),
    };
    truncate(&printable(&text), 60)
}

/// The server's schema, made acceptable to every provider: `$schema` dropped; a
/// root that isn't a plain object (`anyOf`/`oneOf`/`allOf`, or another type)
/// becomes an open object; there's always a `properties`. The server validates
/// the arguments anyway.
pub fn acceptable_schema(schema: &Map<String, Value>) -> Value {
    let mut rest = schema.clone();
    rest.remove("$schema");
    let ty = rest.get("type");
    let plain = (ty.is_none() || ty == Some(&Value::from("object")))
        && !["anyOf", "oneOf", "allOf"].iter().any(|k| rest.contains_key(*k));
    if !plain {
        return serde_json::json!({"type": "object", "properties": {}});
    }
    rest.insert("type".into(), "object".into());
    rest.entry("properties").or_insert_with(|| Value::Object(Map::new()));
    Value::Object(rest)
}

/// [`acceptable_schema`] with the server's key order kept (Marv's `{ ...rest, type: "object", properties }`):
/// `type` and `properties` stay where the server put them, or come last.
pub fn acceptable_schema_ordered(schema: &Ordered) -> Ordered {
    let open = || {
        Ordered::Object(vec![("type".into(), Ordered::String("object".into())), ("properties".into(), Ordered::Object(vec![]))])
    };
    let Some(fields) = schema.as_object() else { return open() };
    let mut rest: Vec<(String, Ordered)> = fields.iter().filter(|(k, _)| k != "$schema").cloned().collect();
    let ty = rest.iter().find(|(k, _)| k == "type").map(|(_, v)| v);
    let plain = ty.is_none_or(|t| t.as_str() == Some("object"))
        && !rest.iter().any(|(k, _)| ["anyOf", "oneOf", "allOf"].contains(&k.as_str()));
    if !plain {
        return open();
    }
    for (key, value) in [("type", Ordered::String("object".into())), ("properties", Ordered::Object(vec![]))] {
        match rest.iter_mut().find(|(k, _)| k == key) {
            Some(slot) if key == "type" => slot.1 = value,
            Some(_) => {}
            None => rest.push((key.into(), value)),
        }
    }
    Ordered::Object(rest)
}

/// What the approval shows for a call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPreview {
    /// `<server>: <title or tool>`.
    pub title: String,
    /// The arguments as pretty JSON, cut at [`PREVIEW_CHARS`]; none for `{}`.
    pub text: Option<String>,
    /// Where it runs: `MCP server "x" (this project's .mcp.json) · runs outside the sandbox`.
    pub note: String,
}

/// One MCP tool, ready to be wrapped as a Marv `Tool`.
#[derive(Clone)]
pub struct McpTool {
    /// Model-facing: `mcp__<server>__<tool>`.
    pub name: String,
    pub description: String,
    pub spec: ToolSpec,
    pub server: String,
    pub source: ServerSource,
    pub info: McpToolInfo,
    /// `spec.parameters` as JSON text, in the server's key order when known ([`McpTool::with_schema_order`]):
    /// `tool_search` shows it as Marv did (`ToolSpec::parameters` is a `serde_json::Value`, which sorts keys).
    pub parameters_text: String,
    /// Reached over HTTP (a service), not a process on this machine.
    pub remote: bool,
    caller: Arc<dyn McpCaller>,
}

impl std::fmt::Debug for McpTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpTool").field("name", &self.name).field("server", &self.server).finish_non_exhaustive()
    }
}

impl McpTool {
    /// Keeps the server's key order for `parameters_text`, given the schema as the server sent it. Ignored unless it
    /// is the same schema as the spec's.
    pub fn with_schema_order(mut self, schema: &Ordered) -> Self {
        let ordered = acceptable_schema_ordered(schema);
        if let Ok(text) = serde_json::to_string(&ordered)
            && serde_json::from_str::<Value>(&text).ok().as_ref() == Some(&self.spec.parameters)
        {
            self.parameters_text = text;
        }
        self
    }

    /// The server's own claim, `readOnlyHint`.
    pub fn read_only(&self) -> bool {
        self.info.annotations.as_ref().and_then(|a| a.read_only_hint) == Some(true)
    }

    /// A read that stays unasked once the session is tainted: a `readOnlyHint` tool of a remote server from your own
    /// `~/.marv/mcp.json`. Its arguments go only to that service (your Notion, your Linear), which shows nobody else
    /// what was searched, so it isn't a way out, as a GitHub read isn't (docs/SECURITY.md). Its result still taints.
    /// Never a project's server (`.mcp.json` is the project's choice, not yours) and never a local one (a process on
    /// this machine can reach anything). The claim is the server's own: you chose to add it.
    pub fn account_read(&self) -> bool {
        self.read_only() && self.remote && self.source == ServerSource::Personal
    }

    /// How much this call can do, given whether the session is tainted.
    ///
    /// Untainted: `readOnlyHint` tools are [`ToolKind::Read`] (you chose to run
    /// that server, so its read-only tools run freely), the rest
    /// [`ToolKind::Execute`] (asks). Tainted: [`ToolKind::Egress`], so it asks
    /// even in yolo (the arguments leave the sandbox), except an
    /// [`McpTool::account_read`], which stays [`ToolKind::Read`].
    pub fn kind(&self, tainted: bool) -> ToolKind {
        match (tainted, self.read_only()) {
            (true, true) if self.account_read() => ToolKind::Read,
            (true, _) => ToolKind::Egress,
            (false, true) => ToolKind::Read,
            (false, false) => ToolKind::Execute,
        }
    }

    /// The server is outside the sandbox, so a tainted session asks first; an [`McpTool::account_read`] only reaches
    /// your own account at that service, which isn't a way out.
    pub fn uses_network(&self) -> bool {
        !self.account_read()
    }

    /// Never: yolo doesn't skip an MCP call.
    pub fn auto_safe(&self) -> bool {
        false
    }

    /// The approval scope: per tool (`mcp:<server>:<tool>`), and its description.
    pub fn scope(&self) -> (String, String) {
        (format!("mcp:{}:{}", self.server, self.info.name), printable(&format!("{}'s {}", self.server, self.info.name)))
    }

    pub fn label(&self, args: &Map<String, Value>) -> String {
        label_of(args)
    }

    pub fn preview(&self, args: &Map<String, Value>) -> McpPreview {
        let json = printable(&serde_json::to_string_pretty(&Value::Object(args.clone())).unwrap_or_default());
        let text = if args.is_empty() {
            None
        } else if json.chars().count() > PREVIEW_CHARS {
            Some(format!("{}…", json.chars().take(PREVIEW_CHARS).collect::<String>()))
        } else {
            Some(json)
        };
        let title = self.info.annotations.as_ref().and_then(|a| a.title.clone()).unwrap_or_else(|| self.info.name.clone());
        let title = printable(&title);
        let whose = if self.source == ServerSource::Project { "this project's .mcp.json" } else { "yours" };
        McpPreview {
            title: format!("{}: {title}", printable(&self.server)),
            text,
            note: format!("MCP server \"{}\" ({whose}) · runs outside the sandbox", self.server),
        }
    }

    /// Runs the call. `Err` is Marv's own sentence (a login is needed); a failure that can carry the server's text is
    /// an `is_error` result.
    pub async fn run(&self, args: Map<String, Value>, cancel: CancellationToken) -> Result<ToolResult, String> {
        let mut result = self.run_whole(args, cancel).await?;
        result.output = cut_mcp_output(&result.output);
        Ok(result)
    }

    /// [`McpTool::run`] without the cap: the session cuts it, saving the whole output (#61).
    pub async fn run_whole(&self, args: Map<String, Value>, cancel: CancellationToken) -> Result<ToolResult, String> {
        self.run_asking(args, cancel, &NoConfirmer).await
    }

    /// [`McpTool::run_whole`], where the server may ask the person before it answers ([`crate::input`]): each
    /// question goes to `confirmer`, and the call is sent again with the answers until a result comes (at most
    /// [`MAX_QUESTIONS`] rounds). Google asks per email, so a search can ask several times. A question Marv can't
    /// show, or the same question again after a "no", ends in an error result saying so.
    pub async fn run_asking(
        &self,
        args: Map<String, Value>,
        cancel: CancellationToken,
        confirmer: &dyn Confirmer,
    ) -> Result<ToolResult, String> {
        let mut answers: Option<Answers> = None;
        let mut declined: Vec<String> = Vec::new();
        let mut allowed: Vec<String> = Vec::new();
        let mut rounds = 0;
        let result = loop {
            let called = match &answers {
                None => self.caller.call(&self.info.name, args.clone(), cancel.clone()).await,
                Some(a) => self.caller.call_answering(&self.info.name, args.clone(), a, cancel.clone()).await,
            };
            let result = match called {
                Ok(r) => r,
                // Marv's own sentence, as it is: the model should tell the user to log in, not retry.
                Err(McpError::LoginNeeded(e)) => return Err(e.to_string()),
                // Anything else can carry the server's own words (a JSON-RPC error's message): an error *result*, so the
                // caller handles it as the server's text (fenced as data, tainting), not as Marv's.
                Err(e) => {
                    let output = printable(&format!("{}'s {} failed: {e}", self.server, self.info.name));
                    return Ok(ToolResult { output, summary: Some("error".into()), is_error: true, images: Vec::new() });
                }
            };
            let asked = serde_json::to_value(&result).ok().and_then(|raw| input_required(&raw));
            let Some(asked) = asked else { break result };
            let refused = |why: String| {
                let output = printable(&format!("{}'s {} didn't run: {why}", self.server, self.info.name));
                Ok(ToolResult { output, summary: Some("not allowed".into()), is_error: true, images: Vec::new() })
            };
            let asked = match asked {
                Ok(a) => a,
                Err(why) => return refused(why),
            };
            // Asked again about what the person said no to: done, not asked twice.
            if asked.confirmations.iter().any(|c| declined.contains(&c.message)) {
                return refused("you didn't allow it.".into());
            }
            // Asked again about what the person allowed: the server didn't take the answer, and asking on would only
            // ask the person the same thing over and over.
            if asked.confirmations.iter().any(|c| allowed.contains(&c.message)) {
                return refused("the server asked again after you allowed it (it didn't take Marv's answer).".into());
            }
            rounds += 1;
            if rounds > MAX_QUESTIONS {
                return refused(format!("the server asked {MAX_QUESTIONS} times; ask for fewer items at once."));
            }
            let mut chosen = Vec::new();
            for c in &asked.confirmations {
                let value = confirmer.confirm(self, c).await;
                if value.is_none() || value == c.meanings().no {
                    declined.push(c.message.clone());
                } else {
                    allowed.push(c.message.clone());
                }
                chosen.push((c.key.clone(), value));
            }
            answers = Some(asked.answer(&chosen));
        };
        let output = mcp_result_text(&result);
        if result.is_error {
            return Ok(ToolResult { output, summary: Some("error".into()), is_error: true, images: Vec::new() });
        }
        let lines = output.split('\n').count();
        Ok(ToolResult {
            output,
            summary: Some(format!("{lines} line{}", if lines == 1 { "" } else { "s" })),
            is_error: false,
            images: Vec::new(),
        })
    }
}

/// The tool Marv runs, and the spec the model is sent (the server's own schema, made acceptable).
pub fn make_mcp_tool(server: &McpServerConfig, info: McpToolInfo, caller: Arc<dyn McpCaller>) -> McpTool {
    let name = mcp_tool_name(&server.name, &info.name);
    let about = info.description.as_deref().unwrap_or("").trim();
    let about = if about.chars().count() > MAX_DESCRIPTION {
        format!("{}…", about.chars().take(MAX_DESCRIPTION).collect::<String>())
    } else {
        about.to_string()
    };
    let description = printable(&format!("(MCP server \"{}\") {about}", server.name)).trim().to_string();
    let spec =
        ToolSpec { name: name.clone(), description: description.clone(), parameters: acceptable_schema(&info.input_schema) };
    let parameters_text = spec.parameters.to_string();
    let remote = matches!(server.transport, crate::config::McpTransport::Http { .. });
    McpTool { name, description, spec, server: server.name.clone(), source: server.source, info, parameters_text, remote, caller }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_names_are_sanitized_short_and_unique() {
        assert_eq!(mcp_tool_name("gh", "list_issues"), "mcp__gh__list_issues");
        let dotted = mcp_tool_name("gh", "issues.list");
        assert!(dotted.starts_with("mcp__gh__issues_list_") && dotted.len() == "mcp__gh__issues_list_".len() + 8);
        assert_ne!(dotted, mcp_tool_name("gh", "issues/list"));
        let long = mcp_tool_name("server", &"x".repeat(100));
        assert_eq!(long.len(), MAX_NAME);
    }

    #[test]
    fn images_are_named_not_sent_and_output_is_capped() {
        let r = McpCallResult {
            content: vec![
                ContentBlock { kind: "text".into(), text: Some("hi".into()), ..Default::default() },
                ContentBlock {
                    kind: "image".into(),
                    mime_type: Some("image/png".into()),
                    data: Some("A".repeat(4096)),
                    ..Default::default()
                },
                ContentBlock {
                    kind: "resource_link".into(),
                    uri: Some("file:///a".into()),
                    name: Some("a".into()),
                    ..Default::default()
                },
                ContentBlock { kind: "weird".into(), ..Default::default() },
            ],
            ..Default::default()
        };
        assert_eq!(
            format_mcp_result(&r),
            "hi\n[image (image/png), 3 KB, not shown: Marv passes text only]\n[link: file:///a (a)]\n[weird content, not shown]"
        );
        let big = McpCallResult {
            content: vec![ContentBlock { kind: "text".into(), text: Some("y".repeat(40_000)), ..Default::default() }],
            ..Default::default()
        };
        let out = format_mcp_result(&big);
        assert!(out.contains("(10000 characters omitted)"));
        assert_eq!(out.chars().filter(|&c| c == 'y').count(), MAX_OUTPUT);
        let structured = McpCallResult { structured_content: Some(json!({"a": 1})), ..Default::default() };
        assert_eq!(format_mcp_result(&structured), "{\n  \"a\": 1\n}");
        assert_eq!(format_mcp_result(&McpCallResult::default()), "(no output)");
    }

    #[test]
    fn schemas_become_plain_objects() {
        let s = |v: Value| acceptable_schema(v.as_object().unwrap());
        assert_eq!(s(json!({"$schema": "x", "type": "object"})), json!({"type": "object", "properties": {}}));
        assert_eq!(s(json!({"anyOf": [{}]})), json!({"type": "object", "properties": {}}));
        assert_eq!(
            s(json!({"properties": {"a": {"type": "string"}}, "required": ["a"]})),
            json!({"type": "object", "properties": {"a": {"type": "string"}}, "required": ["a"]})
        );
    }

    struct Never;
    #[async_trait]
    impl McpCaller for Never {
        async fn call(&self, _tool: &str, _args: Map<String, Value>, _cancel: CancellationToken) -> crate::Result<McpCallResult> {
            Err(crate::oauth::LoginNeeded { server: "gh".into() }.into())
        }
    }

    fn server() -> McpServerConfig {
        crate::config::parse_server("gh", &json!({"command": "x"}), ServerSource::Project, &|_| None, "f").unwrap()
    }

    #[test]
    fn read_only_hint_is_egress_once_tainted() {
        let info = McpToolInfo {
            name: "get".into(),
            description: Some("Gets.".into()),
            input_schema: Map::new(),
            annotations: Some(McpToolAnnotations { read_only_hint: Some(true), title: None }),
        };
        let tool = make_mcp_tool(&server(), info, Arc::new(Never));
        assert_eq!(tool.kind(false), ToolKind::Read);
        assert_eq!(tool.kind(true), ToolKind::Egress);
        assert!(!tool.auto_safe());
        assert_eq!(tool.description, "(MCP server \"gh\") Gets.");
        let plain = make_mcp_tool(
            &server(),
            McpToolInfo { name: "put".into(), description: None, input_schema: Map::new(), annotations: None },
            Arc::new(Never),
        );
        assert_eq!(plain.kind(false), ToolKind::Execute);
        assert_eq!(plain.description, "(MCP server \"gh\")");
    }

    #[test]
    fn only_your_own_remote_servers_reads_stay_unasked_once_tainted() {
        let read = |name: &str, raw: serde_json::Value, source: ServerSource| {
            let server = crate::config::parse_server(name, &raw, source, &|_| None, "f").unwrap();
            let info = McpToolInfo {
                name: "search".into(),
                description: None,
                input_schema: Map::new(),
                annotations: Some(McpToolAnnotations { read_only_hint: Some(true), title: None }),
            };
            make_mcp_tool(&server, info, Arc::new(Never))
        };
        let remote = json!({"type": "http", "url": "https://mcp.linear.app/mcp"});
        let yours = read("linear", remote.clone(), ServerSource::Personal);
        assert!(yours.account_read());
        assert_eq!((yours.kind(true), yours.uses_network()), (ToolKind::Read, false));
        // A project's remote server, and a local one of yours: the arguments could go anywhere.
        for other in
            [read("linear", remote, ServerSource::Project), read("local", json!({"command": "x"}), ServerSource::Personal)]
        {
            assert!(!other.account_read());
            assert_eq!((other.kind(true), other.uses_network()), (ToolKind::Egress, true));
        }
        // A write of your own remote server still asks once tainted.
        let server = crate::config::parse_server(
            "linear",
            &json!({"type": "http", "url": "https://mcp.linear.app/mcp"}),
            ServerSource::Personal,
            &|_| None,
            "f",
        )
        .unwrap();
        let write = make_mcp_tool(
            &server,
            McpToolInfo { name: "save_issue".into(), description: None, input_schema: Map::new(), annotations: None },
            Arc::new(Never),
        );
        assert_eq!((write.kind(true), write.uses_network()), (ToolKind::Egress, true));
    }

    #[tokio::test]
    async fn login_needed_reaches_the_model_as_marvs_sentence() {
        let tool = make_mcp_tool(
            &server(),
            McpToolInfo { name: "put".into(), description: None, input_schema: Map::new(), annotations: None },
            Arc::new(Never),
        );
        let err = tool.run(Map::new(), CancellationToken::new()).await.unwrap_err();
        assert_eq!(err, "gh needs a new login: the user can run /mcp login gh");
    }

    #[test]
    fn labels_and_previews() {
        let args = json!({"n": 3, "q": "x".repeat(80)});
        assert_eq!(label_of(args.as_object().unwrap()).chars().count(), 60);
        assert_eq!(label_of(json!({"n": 3}).as_object().unwrap()), "{\"n\":3}");
        let tool = make_mcp_tool(
            &server(),
            McpToolInfo { name: "put".into(), description: None, input_schema: Map::new(), annotations: None },
            Arc::new(Never),
        );
        let p = tool.preview(&Map::new());
        assert_eq!(p.text, None);
        assert_eq!(p.note, "MCP server \"gh\" (this project's .mcp.json) · runs outside the sandbox");
    }

    #[test]
    fn label_leads_with_the_target_not_the_content() {
        let args = json!({"content": "x".repeat(500), "path": "/tmp/a.txt"});
        assert_eq!(label_of(args.as_object().unwrap()), "/tmp/a.txt");
        let url = json!({"body": "hello", "url": "https://e.com/x"});
        assert_eq!(label_of(url.as_object().unwrap()), "https://e.com/x");
        let none = json!({"content": "y".repeat(100)});
        assert_eq!(label_of(none.as_object().unwrap()).chars().count(), 60);
    }

    #[test]
    fn controls_are_stripped_from_labels_output_and_descriptions() {
        let args = json!({"path": "a\u{202e}b\u{1b}[31m\u{200f}c"});
        assert_eq!(label_of(args.as_object().unwrap()), "abc");
        let result = McpCallResult {
            content: vec![ContentBlock {
                kind: "text".into(),
                text: Some("ok\u{2066}\u{61c}\u{85}\n\tdone\u{7}".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(format_mcp_result(&result), "ok\n\tdone");
        let tool = make_mcp_tool(
            &server(),
            McpToolInfo {
                name: "put".into(),
                description: Some("Gets\u{202d}.".into()),
                input_schema: Map::new(),
                annotations: None,
            },
            Arc::new(Never),
        );
        assert_eq!(tool.description, "(MCP server \"gh\") Gets.");
    }
}
