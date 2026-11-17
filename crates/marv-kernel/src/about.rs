//! About this Marv (runtime-kernel §20.7, decision O7). New in marv-rs.
//!
//! What a support ticket can quote without a word of any conversation (§0.7.4: the log, not a replay): the kernel's
//! version, the limits in force (K, the wake limits, the hop cap, the budgets, the deadline) and where each store
//! lives. The tool ABI hash (§13.5) and a chat's boot hashes (§18.1) belong here too once those are built; until then
//! [`About::lines`] says so.

use serde::{Deserialize, Serialize};

/// One limit, as fixed words and its value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limit {
    pub what: String,
    pub value: String,
}

/// One store: what it holds and its path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Store {
    pub what: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct About {
    pub version: String,
    pub limits: Vec<Limit>,
    pub stores: Vec<Store>,
    /// The tool ABI hash (§13.5), once built.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_abi: Option<String>,
}

/// The limits the kernel holds every chat to. `wakes_per_minute` and `compact_tokens` are the session's (the host
/// passes them: they live above this crate).
pub fn limits(cap: usize, wakes_per_minute: usize, compact_tokens: u64) -> Vec<Limit> {
    let l = |what: &str, value: String| Limit { what: what.into(), value };
    vec![
        l("chats loaded at once (K)", cap.to_string()),
        l("wakes per wire per minute", wakes_per_minute.to_string()),
        l("rule hop cap", crate::RULE_HOP_LIMIT.to_string()),
        l("a grant's default spend cap a day", format!("${:.2}", crate::DEFAULT_DAILY_USD_MICROS as f64 / 1e6)),
        l("an unattended turn's deadline", format!("{} min", crate::TURN_DEADLINE_MS / 60_000)),
        l("timers a chat may set itself a day", crate::SELF_TIMERS_PER_DAY.to_string()),
        l("context budget (compaction)", format!("{compact_tokens} tokens")),
    ]
}

impl About {
    /// What `marv kernel about` prints.
    pub fn lines(&self) -> Vec<String> {
        let mut out = vec![format!("Marv kernel {}", self.version)];
        out.push(format!(
            "Tool ABI: {}",
            self.tool_abi.clone().unwrap_or_else(|| "not versioned yet (runtime-kernel §13.5)".into())
        ));
        out.push("Boot hashes: not recorded yet (runtime-kernel §18.1)".into());
        out.push("Limits:".into());
        out.extend(self.limits.iter().map(|l| format!("- {}: {}", l.what, l.value)));
        out.push("Stores:".into());
        out.extend(self.stores.iter().map(|s| format!("- {}: {}", s.what, s.path)));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn about_lists_the_version_limits_and_stores() {
        let about = About {
            version: "0.8.0".into(),
            limits: limits(8, 3, 100_000),
            stores: vec![Store { what: "rules".into(), path: "/home/u/.marv/mesh/policies.json".into() }],
            tool_abi: None,
        };
        let lines = about.lines();
        assert_eq!(lines[0], "Marv kernel 0.8.0");
        assert!(lines.contains(&"- chats loaded at once (K): 8".to_string()));
        assert!(lines.contains(&"- rule hop cap: 2".to_string()));
        assert!(lines.contains(&"- an unattended turn's deadline: 30 min".to_string()));
        assert_eq!(lines.last().unwrap(), "- rules: /home/u/.marv/mesh/policies.json");
    }
}
