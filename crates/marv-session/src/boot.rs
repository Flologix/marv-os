//! A chat's boot, as the kernel logs it (runtime-kernel §18.1, decision B1). New in marv-rs.
//!
//! At a conversation's first request (a new chat, `/clear`, a resume, a new core), once its system prompt and tool
//! specs are fixed, the session logs `KernelEvent::Booted` through its mesh: the shell, the core, a SHA-256 of the
//! prompt's bytes ([`prompt_hash`]) and of the tool specs in order ([`tools_hash`]), the device set ([`devices`]) and
//! whether it resumed. Two boots with the same hashes send the same cacheable prefix, which makes the prompt cache's
//! byte-stability checkable from the log. Hashes and names only: never the prompt, a spec's words or a message.

use marv_core::ToolSpec;
use marv_tools::ToolRef;
use sha2::{Digest, Sha256};

use marv_mcp::tool::McpTool;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// SHA-256 of the system prompt's bytes, as hex.
pub fn prompt_hash(system: &str) -> String {
    hex(&Sha256::digest(system.as_bytes()))
}

/// SHA-256 of the tool specs in their order: each spec's name, description and parameters (as sent), separated so
/// that no two lists hash alike by shifting text between fields.
pub fn tools_hash(specs: &[ToolSpec]) -> String {
    let mut hash = Sha256::new();
    for spec in specs {
        for field in [spec.name.as_str(), spec.description.as_str(), &spec.parameters.to_string()] {
            hash.update((field.len() as u64).to_le_bytes());
            hash.update(field.as_bytes());
        }
    }
    hex(&hash.finalize())
}

/// The device set (runtime-kernel §17.1): what reaches outside the process through a driver. Each MCP server with
/// tools (`mcp:<name>`), and each door the conversation offers: `github`, `herdr`, `process`, `web` (web_fetch), and
/// `host` where bash may run a command on the host. `mcp_tools`: the conversation's own (its snapshot), not the
/// manager's now. Sorted, by name.
pub fn devices(tools: &[ToolRef], mcp_tools: &[McpTool], host_commands: bool) -> Vec<String> {
    let mut found: Vec<String> = tools
        .iter()
        .filter_map(|t| match t.name() {
            "github" => Some("github"),
            "herdr" => Some("herdr"),
            "process" => Some("process"),
            "web_fetch" => Some("web"),
            _ => None,
        })
        .map(str::to_string)
        .collect();
    if host_commands {
        found.push("host".into());
    }
    found.extend(mcp_tools.iter().map(|t| format!("mcp:{}", t.server)));
    found.sort();
    found.dedup();
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(name: &str, description: &str) -> ToolSpec {
        ToolSpec { name: name.into(), description: description.into(), parameters: json!({"type": "object"}) }
    }

    #[test]
    fn hashes_are_stable_and_see_order_and_field_bounds() {
        assert_eq!(prompt_hash(""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        let (a, b) = (spec("a", "one"), spec("b", "two"));
        assert_eq!(tools_hash(&[a.clone(), b.clone()]), tools_hash(&[a.clone(), b.clone()]));
        assert_ne!(tools_hash(&[a.clone(), b.clone()]), tools_hash(&[b, a]), "order is part of the prefix");
        assert_ne!(tools_hash(&[spec("ab", "c")]), tools_hash(&[spec("a", "bc")]));
    }

    #[test]
    fn devices_are_the_doors_offered_and_host() {
        let tools: Vec<ToolRef> = marv_tools::builtin_tools().iter().filter(|t| t.name() != "github").cloned().collect();
        let found = devices(&tools, &[], true);
        assert!(found.contains(&"web".to_string()) && found.contains(&"host".to_string()), "{found:?}");
        assert!(!found.contains(&"github".to_string()));
    }
}
