//! What `/mcp add` proposes, and the one approval that decides it. Ports
//! `src/mcp/install/proposal.ts`.
//!
//! **The approval is the security boundary.** The user approves the exact entry
//! that gets written, and nothing fetched after the approval changes what runs:
//! the preview is built from the entry itself (`display_of`, as `/mcp` shows
//! servers), and the App writes the JSON the preview showed, parsed back. An
//! entry that `printable()` would change, or with bidi overrides, can't be
//! shown as written, so it isn't offered. `once`: no "don't ask again".

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::config::{display_of, reads_of};
use crate::manager::{McpServerStatus, McpState};
use crate::text::{is_bidi, printable};

/// An `mcp.json` entry, as written: `${VAR}` references, never values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum McpEntry {
    Stdio {
        command: String,
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<BTreeMap<String, String>>,
    },
    Http {
        #[serde(rename = "type")]
        kind: HttpKind,
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        headers: Option<BTreeMap<String, String>>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HttpKind {
    Http,
}

/// What an approval says about a package runner's server (`npx`, `uvx`, …), in fixed words: the package is pinned, its
/// own dependencies aren't, so the code that runs can change after the person says yes.
pub const RUNNER_CHANGES: &str = "the package is pinned, but its own dependencies follow their version ranges: a later \
     start can run newer code than what you approve now";
/// The same for a remote server: Marv can pin an address, not what's deployed behind it.
pub const REMOTE_CHANGES: &str = "a remote server is whatever its owner deploys: this approves the address, not what runs \
     there later";

impl McpEntry {
    /// A local server started through a package runner (`npx`, `uvx`, `bunx`, `pnpx`, `uv`).
    pub fn runs_a_package(&self) -> bool {
        match self {
            McpEntry::Stdio { command, .. } => {
                let base = std::path::Path::new(command).file_name().and_then(|n| n.to_str()).unwrap_or(command);
                crate::manager::PACKAGE_RUNNERS.contains(&base)
            }
            McpEntry::Http { .. } => false,
        }
    }
    pub fn is_remote(&self) -> bool {
        matches!(self, McpEntry::Http { .. })
    }
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("entries serialize")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    /// The key in `mcp.json`.
    pub name: String,
    pub entry: McpEntry,
    /// Lines for the approval (`npm      pkg 1.2.3, published …`), already printable.
    pub facts: Vec<String>,
    /// Shown in the warning color: deprecated, brand new, a repository that doesn't match.
    pub warnings: Vec<String>,
}

/// A GitHub README that nominates several servers: the `/mcp add` commands to choose from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choices {
    pub choices: Vec<String>,
}

/// A fetched response: status and body text.
#[derive(Debug, Clone)]
pub struct Fetched {
    pub status: u16,
    pub body: String,
}

/// How resolvers reach the network: Marv's own `get` (address checks, time and
/// size limits). Tests point it at a local server.
#[async_trait]
pub trait ResolveDeps: Send + Sync {
    async fn get(&self, url: &str, accept: &str, max_bytes: usize, cancel: &CancellationToken) -> crate::Result<Fetched>;
    /// A GitHub repository's README (`None`: it has none). `GITHUB_TOKEN` goes to the API host only.
    async fn readme(&self, owner: &str, repo: &str, cancel: &CancellationToken) -> crate::Result<Option<String>>;
    /// Whether a remote server asks for an OAuth login (`oauth::probe_login`); `None`: not asked.
    async fn probe_login(&self, _url: &str, _cancel: &CancellationToken) -> Option<bool> {
        None
    }
    fn npm_registry(&self) -> &str {
        "https://registry.npmjs.org"
    }
    fn pypi(&self) -> &str {
        "https://pypi.org"
    }
    fn registry(&self) -> &str {
        "https://registry.modelcontextprotocol.io"
    }
    /// Now, in ms since the epoch (tests fix it).
    fn now_ms(&self) -> u64;
    fn cancel(&self) -> CancellationToken;
}

/// What the user passed besides the source.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolveOptions {
    pub name: Option<String>,
    pub env: Vec<String>,
    pub headers: BTreeMap<String, String>,
    pub args: Vec<String>,
}

/// `npm      text`: the label in a column, so the facts line up. Stranger-written text is made printable here.
pub fn fact(label: &str, text: &str) -> String {
    let clean = printable(text).split_whitespace().collect::<Vec<_>>().join(" ");
    format!("{label:<8} {clean}")
}

/// `--env NAME` → `"NAME": "${NAME}"`.
pub fn env_entries(names: &[String]) -> BTreeMap<String, String> {
    names.iter().map(|n| (n.clone(), format!("${{{n}}}"))).collect()
}

/// `git+https://github.com/o/r.git` → `github.com/o/r`: how a repository is shown.
pub fn repo_of(url: Option<&str>) -> Option<String> {
    let mut s = url?.trim();
    s = s.strip_prefix("git+").unwrap_or(s);
    let mut owned;
    if let Some(rest) = s.strip_prefix("git@") {
        owned = rest.replacen(':', "/", 1);
    } else {
        owned = s.to_string();
    }
    if let Some(idx) = owned.find("://")
        && owned[..idx].bytes().all(|c| c.is_ascii_alphabetic() || c == b'+')
        && idx > 0
    {
        owned = owned[idx + 3..].to_string();
    }
    let mut shown = owned.strip_prefix("www.").unwrap_or(&owned);
    shown = shown.strip_suffix(".git").unwrap_or(shown);
    let shown = shown.trim_end_matches('/');
    (!shown.is_empty()).then(|| shown.chars().take(200).collect())
}

/// A remote server: nothing to fetch and nothing to pin.
pub fn http_proposal(url: &str, name: String, headers: &BTreeMap<String, String>) -> Proposal {
    let host = url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
    Proposal {
        name,
        entry: McpEntry::Http { kind: HttpKind::Http, url: url.into(), headers: (!headers.is_empty()).then(|| headers.clone()) },
        facts: vec![fact("remote", &format!("{host}: nothing to pin"))],
        warnings: vec![],
    }
}

/// The approval `/mcp add` shows (`marv-session` turns it into an `ApprovalRequest` with `once: true`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddApproval {
    pub tool: &'static str,
    pub label: String,
    pub title: String,
    /// The command line, for a local server.
    pub command: Option<String>,
    pub text: String,
    pub note: &'static str,
    pub warning: Option<String>,
    /// Unique, so nothing is ever remembered.
    pub scope_key: String,
    pub scope_description: String,
}

static NEXT_SCOPE: AtomicU64 = AtomicU64::new(0);

/// The approval for a proposal, and the JSON it shows: the App writes exactly
/// that JSON (parsed back), so what was shown and what's written can't drift.
/// `is_set` says whether a variable is set in the user's environment.
pub fn approval_for(proposal: &Proposal, is_set: &dyn Fn(&str) -> bool) -> Result<(AddApproval, String), String> {
    let Proposal { name, entry, .. } = proposal;
    let json = serde_json::to_string(entry).expect("entries serialize");
    if printable(&json) != json || json.chars().any(is_bidi) {
        return Err(format!(
            "The entry for \"{name}\" contains control characters, so it can't be shown exactly as it would be written: not added."
        ));
    }
    let value = entry.to_value();
    let remote = entry.is_remote();
    let mut lines = proposal.facts.clone();
    if remote {
        lines.insert(0, fact("url", &display_of(&value)));
    }
    for variable in reads_of(&value) {
        let state = if is_set(&variable) {
            format!("{variable} (set)")
        } else {
            format!("{variable} (not set: it won't start until you set it and restart Marv)")
        };
        lines.push(fact("reads", &state));
    }
    // What a yes can't cover, said the same way every time.
    if remote {
        lines.push(fact("changes", REMOTE_CHANGES));
    } else if entry.runs_a_package() {
        lines.push(fact("changes", RUNNER_CHANGES));
    }
    lines.push(fact("writes", &format!("{}: {json}", serde_json::Value::from(name.as_str()))));
    let n = NEXT_SCOPE.fetch_add(1, Ordering::Relaxed) + 1;
    let approval = AddApproval {
        tool: "mcp_add",
        label: format!("/mcp add {name}"),
        title: format!("Add MCP server \"{name}\" to ~/.marv/mcp.json (yours, every project)"),
        command: (!remote).then(|| display_of(&value)),
        text: lines.join("\n"),
        note: if remote {
            "a remote server: it gets what Marv's calls send it, and the headers above"
        } else {
            "runs outside the sandbox, with your permissions and network, started in your home folder"
        },
        warning: (!proposal.warnings.is_empty())
            .then(|| proposal.warnings.iter().map(|w| printable(w)).collect::<Vec<_>>().join("\n")),
        scope_key: format!("mcp-add:{n}"),
        scope_description: format!("adding {name}"),
    };
    Ok((approval, json))
}

/// When a server's tools reach a conversation that hasn't fixed its tools yet (no message sent): its first.
pub const USABLE_NEXT_MESSAGE: &str = "Marv can use its tools from your next message.";

/// What `/mcp add` says once the server was written and started: (text, is_error). `when`: when the tools reach the
/// model (`marv_core::APPLIES_AT_NEXT_START` once the conversation has fixed its tools, [`USABLE_NEXT_MESSAGE`] before).
pub fn added_notice(status: &McpServerStatus, when: &str) -> (String, bool) {
    let where_ = "~/.marv/mcp.json";
    match status.state {
        McpState::Connected => {
            let n = status.tools.len();
            let s = if n == 1 { "" } else { "s" };
            (format!("Added {} to {where_}: connected · {n} tool{s}. {}", status.name, marv_core::APPLIES_AT_NEXT_START), false)
        }
        McpState::NeedsLogin => {
            (format!("Added {} to {where_}. It needs a login: /mcp login {}. {when}", status.name, status.name), false)
        }
        _ => {
            let why = status.error.clone().unwrap_or_else(|| format!("{:?}", status.state).to_lowercase());
            (
                format!(
                    "Added {} to {where_}, but it didn't start: {why} (see /mcp; fix the entry or /mcp remove {}).",
                    status.name, status.name
                ),
                true,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repos_are_shown_short() {
        assert_eq!(repo_of(Some("git+https://github.com/o/r.git")).as_deref(), Some("github.com/o/r"));
        assert_eq!(repo_of(Some("git@github.com:o/r.git")).as_deref(), Some("github.com/o/r"));
        assert_eq!(repo_of(Some("https://www.gitlab.com/o/r/")).as_deref(), Some("gitlab.com/o/r"));
        assert_eq!(repo_of(Some("  ")), None);
    }

    #[test]
    fn the_approval_shows_exactly_what_is_written() {
        let p = Proposal {
            name: "w".into(),
            entry: McpEntry::Stdio {
                command: "npx".into(),
                args: vec!["-y".into(), "w@1.0.0".into()],
                env: Some(env_entries(&["KEY".into()])),
            },
            facts: vec![fact("npm", "w 1.0.0\n published")],
            warnings: vec![],
        };
        let (a, json) = approval_for(&p, &|_| false).unwrap();
        assert_eq!(json, r#"{"command":"npx","args":["-y","w@1.0.0"],"env":{"KEY":"${KEY}"}}"#);
        assert!(a.text.contains("npm      w 1.0.0 published"));
        assert!(a.text.contains("reads    KEY (not set"));
        assert!(a.text.ends_with(&format!("writes   \"w\": {json}")));
        assert_eq!(a.command.as_deref(), Some("npx -y w@1.0.0 · env KEY=${KEY}"));
        let evil = Proposal { entry: McpEntry::Stdio { command: "x\u{202e}".into(), args: vec![], env: None }, ..p };
        assert!(approval_for(&evil, &|_| true).is_err());
    }

    #[test]
    fn remote_entries_carry_type_http() {
        let p = http_proposal("https://mcp.example.com/", "example".into(), &BTreeMap::new());
        assert_eq!(serde_json::to_string(&p.entry).unwrap(), r#"{"type":"http","url":"https://mcp.example.com/"}"#);
        assert!(p.facts[0].starts_with("remote   mcp.example.com: nothing to pin"));
        let (a, _) = approval_for(&p, &|_| false).unwrap();
        assert!(a.text.contains(&fact("changes", REMOTE_CHANGES)), "{}", a.text);
        assert!(!a.text.contains(RUNNER_CHANGES));
    }

    /// What a yes can't cover is said in fixed words: a package runner's dependencies follow their ranges, a remote
    /// server is what its owner deploys. A plain local command gets neither.
    #[test]
    fn the_approval_says_what_a_yes_cant_pin() {
        let local = |command: &str| Proposal {
            name: "w".into(),
            entry: McpEntry::Stdio { command: command.into(), args: vec!["w@1.0.0".into()], env: None },
            facts: vec![],
            warnings: vec![],
        };
        for runner in ["npx", "uvx", "/usr/local/bin/bunx"] {
            let (a, _) = approval_for(&local(runner), &|_| false).unwrap();
            assert!(a.text.contains(&fact("changes", RUNNER_CHANGES)), "{runner}: {}", a.text);
        }
        let (a, _) = approval_for(&local("node"), &|_| false).unwrap();
        assert!(!a.text.contains("changes"), "{}", a.text);
    }
}
