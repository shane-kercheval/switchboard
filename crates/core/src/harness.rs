use std::fmt;

use serde::{Deserialize, Serialize};

/// Identifies which AI coding harness an agent is bound to.
/// `#[non_exhaustive]` so further variants remain non-breaking.
///
/// **Session-id asymmetry** (load-bearing): Claude Code and Gemini agents
/// pre-generate `AgentRecord.session_locator` at registration time (passed via
/// `--session-id <uuid>` on first dispatch, `--resume <uuid>` thereafter);
/// Codex and Antigravity agents leave it `None` and capture the locator
/// post-spawn — Codex from the `thread.started` stream event on first dispatch,
/// Antigravity from the server-assigned conversation UUID captured by watching
/// for a new `~/.gemini/antigravity-cli/brain/<uuid>/` directory. The captured
/// locator is emitted as a `SessionLocatorCaptured` event and persisted by the
/// dispatcher onto the registry record (no sidecar).
///
/// **UUID-version asymmetry** (load-bearing): Claude Code uses UUID v7
/// (time-ordered) like the rest of Switchboard; Gemini uses UUID v4
/// (random) because Gemini's session-file naming uses the first 8 hex chars
/// of the session ID as a filename component, and UUID v7s minted in the
/// same millisecond share their first 8 chars, causing on-disk session-file
/// interleave under concurrent dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HarnessKind {
    ClaudeCode,
    Codex,
    Gemini,
    Antigravity,
}

impl HarnessKind {
    /// Whether Switchboard can set this harness's model per agent (via a
    /// per-invocation CLI flag). True for Claude (`--model`), Codex (`-m`), and
    /// Gemini (`-m`); false for Antigravity, whose model is a global,
    /// harness-owned config we never touch (per `harness-behavior.md` §3.3).
    ///
    /// The single authority for the model-selection gate — command validation,
    /// picker visibility, and the per-agent change action all derive from this
    /// rather than re-deriving `if harness == …`. Exhaustive (no `_` arm) so a
    /// new harness forces a deliberate decision here.
    #[must_use]
    pub fn supports_model_selection(self) -> bool {
        match self {
            Self::ClaudeCode | Self::Codex | Self::Gemini => true,
            Self::Antigravity => false,
        }
    }

    /// Whether Switchboard can set this harness's reasoning-effort level per
    /// agent. True for Claude (`--effort`) and Codex (`-c
    /// model_reasoning_effort=`); false for Gemini (thinking is `settings.json`
    /// config-only, no CLI flag) and Antigravity (effort is folded into the
    /// model display name, which we can't set anyway) — per
    /// `harness-behavior.md` §3.4.
    ///
    /// A *separate* axis from model selection with a *different* capability set
    /// (Gemini has model but not effort), so it is its own gate. Same authority
    /// role and exhaustiveness rationale as [`Self::supports_model_selection`].
    #[must_use]
    pub fn supports_effort_selection(self) -> bool {
        match self {
            Self::ClaudeCode | Self::Codex => true,
            Self::Gemini | Self::Antigravity => false,
        }
    }
}

/// User-facing names. Used in `thiserror` `#[error]` format strings that
/// surface to the frontend via Tauri (where `AppError::to_string()` is the
/// IPC error payload). The `Debug` impl prints `ClaudeCode` without a
/// space; this `Display` impl prints `Claude Code` which is what users see
/// on Anthropic's product surface.
///
/// Tracing logs continue to use `{:?}` (Debug) since logs are dev-facing
/// and the Debug-precise variant name is more useful for grep.
impl fmt::Display for HarnessKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Exhaustive match within the defining crate (the `#[non_exhaustive]`
        // attribute applies across crate boundaries only). Adding a future
        // variant forces this impl to be updated — exactly the right
        // pressure for "new harness → new user-facing name."
        match self {
            Self::ClaudeCode => f.write_str("Claude Code"),
            Self::Codex => f.write_str("Codex"),
            Self::Gemini => f.write_str("Gemini"),
            Self::Antigravity => f.write_str("Antigravity"),
        }
    }
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

    #[test]
    fn gemini_serializes_as_snake_case() {
        let json = serde_json::to_string(&HarnessKind::Gemini).unwrap();
        assert_eq!(json, "\"gemini\"");
    }

    #[test]
    fn gemini_deserializes_from_snake_case() {
        let parsed: HarnessKind = serde_json::from_str("\"gemini\"").unwrap();
        assert_eq!(parsed, HarnessKind::Gemini);
    }

    #[test]
    fn antigravity_serializes_as_snake_case() {
        let json = serde_json::to_string(&HarnessKind::Antigravity).unwrap();
        assert_eq!(json, "\"antigravity\"");
    }

    #[test]
    fn antigravity_deserializes_from_snake_case() {
        let parsed: HarnessKind = serde_json::from_str("\"antigravity\"").unwrap();
        assert_eq!(parsed, HarnessKind::Antigravity);
    }

    #[test]
    fn supports_model_selection_per_variant() {
        assert!(HarnessKind::ClaudeCode.supports_model_selection());
        assert!(HarnessKind::Codex.supports_model_selection());
        assert!(HarnessKind::Gemini.supports_model_selection());
        assert!(!HarnessKind::Antigravity.supports_model_selection());
    }

    #[test]
    fn supports_effort_selection_per_variant() {
        assert!(HarnessKind::ClaudeCode.supports_effort_selection());
        assert!(HarnessKind::Codex.supports_effort_selection());
        assert!(!HarnessKind::Gemini.supports_effort_selection());
        assert!(!HarnessKind::Antigravity.supports_effort_selection());
    }

    #[test]
    fn display_uses_user_facing_names_with_space_for_claude() {
        assert_eq!(format!("{}", HarnessKind::ClaudeCode), "Claude Code");
        assert_eq!(format!("{}", HarnessKind::Codex), "Codex");
        assert_eq!(format!("{}", HarnessKind::Gemini), "Gemini");
        assert_eq!(format!("{}", HarnessKind::Antigravity), "Antigravity");
    }
}
