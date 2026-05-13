use serde::{Deserialize, Serialize};

/// Identifies which AI coding harness an agent is bound to. M1 has only Claude Code;
/// M2 adds Codex. `#[non_exhaustive]` so adding variants in M2 is non-breaking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HarnessKind {
    ClaudeCode,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_as_snake_case() {
        let json = serde_json::to_string(&HarnessKind::ClaudeCode).unwrap();
        assert_eq!(json, "\"claude_code\"");
    }

    #[test]
    fn deserializes_from_snake_case() {
        let parsed: HarnessKind = serde_json::from_str("\"claude_code\"").unwrap();
        assert_eq!(parsed, HarnessKind::ClaudeCode);
    }
}
