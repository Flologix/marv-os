//! How one agent may reach another (runtime-kernel phase 2b). New in marv-rs.
//!
//! The mesh's wire mode (`marv_mesh::WireMode` re-exports this type, same serde names), and a rule's mode: rules cover
//! `send`, `call` or `read`, never `watch` ([`crate::policy::Policy::check`]). `read` (runtime-kernel §9.2,
//! `chat_read`) is a rule's mode only: no receptacle takes it, so no wire is ever made in it.

use std::fmt;

use serde::{Deserialize, Serialize};

/// How a wire, or a rule, may be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Request and response (`tools/call`).
    Call,
    /// Fire-and-forget into an inbox.
    Send,
    /// Subscribe to events (notifications), such as "agent B changed a file you read".
    Watch,
    /// Read another chat's conversation (`chat_read`, runtime-kernel §9.2): a rule's mode, never a wire's. Never
    /// implied by `send`: a rule covers exactly one mode.
    Read,
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Mode::Call => "call",
            Mode::Send => "send",
            Mode::Watch => "watch",
            Mode::Read => "read",
        })
    }
}

/// A word that isn't a mode. Its text is the sentence the mesh has always shown for it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("\"{0}\" isn't a wire mode: call, send, watch or read")]
pub struct ModeError(pub String);

impl std::str::FromStr for Mode {
    type Err = ModeError;
    fn from_str(s: &str) -> Result<Self, ModeError> {
        match s {
            "call" => Ok(Mode::Call),
            "send" => Ok(Mode::Send),
            "watch" => Ok(Mode::Watch),
            "read" => Ok(Mode::Read),
            other => Err(ModeError(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_parse_show_and_serialize_lowercase() {
        for (word, mode) in [("call", Mode::Call), ("send", Mode::Send), ("watch", Mode::Watch), ("read", Mode::Read)] {
            assert_eq!(word.parse::<Mode>().unwrap(), mode);
            assert_eq!(mode.to_string(), word);
            assert_eq!(serde_json::to_value(mode).unwrap(), serde_json::Value::from(word));
        }
        assert_eq!("all".parse::<Mode>().unwrap_err().to_string(), "\"all\" isn't a wire mode: call, send, watch or read");
    }
}
