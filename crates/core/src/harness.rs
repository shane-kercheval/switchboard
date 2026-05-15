use serde::{Deserialize, Serialize};

/// Identifies which AI coding harness an agent is bound to. M1 had only Claude Code;
/// M2.3 adds Codex. `#[non_exhaustive]` so further variants remain non-breaking.
///
/// **Session-id asymmetry** (load-bearing — see M1.2 invariant + M2.3 plan): Claude
/// Code agents pre-generate `AgentRecord.session_id` at registration time; Codex
/// agents leave it `None` and rely on a per-agent session-link sidecar populated
/// from the `thread.started` stream event on first dispatch. The sidecar is the
/// system-of-record for Codex's captured `thread_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HarnessKind {
    ClaudeCode,
    Codex,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_code_serializes_as_snake_case() {
        let json = serde_json::to_string(&HarnessKind::ClaudeCode).unwrap();
        assert_eq!(json, "\"claude_code\"");
    }

    #[test]
    fn claude_code_deserializes_from_snake_case() {
        let parsed: HarnessKind = serde_json::from_str("\"claude_code\"").unwrap();
        assert_eq!(parsed, HarnessKind::ClaudeCode);
    }

    #[test]
    fn codex_serializes_as_snake_case() {
        let json = serde_json::to_string(&HarnessKind::Codex).unwrap();
        assert_eq!(json, "\"codex\"");
    }

    #[test]
    fn codex_deserializes_from_snake_case() {
        let parsed: HarnessKind = serde_json::from_str("\"codex\"").unwrap();
        assert_eq!(parsed, HarnessKind::Codex);
    }
}
