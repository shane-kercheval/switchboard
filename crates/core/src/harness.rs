use std::fmt;

use serde::{Deserialize, Serialize};

/// Identifies which AI coding harness an agent is bound to.
/// `#[non_exhaustive]` so further variants remain non-breaking.
///
/// **Session-id asymmetry** (load-bearing): Claude Code agents pre-generate
/// `AgentRecord.session_locator` at registration time (passed via
/// `--session-id <uuid>` on first dispatch, `--resume <uuid>` thereafter);
/// Codex and Antigravity agents leave it `None` and capture the locator
/// post-spawn — Codex from the `thread.started` stream event on first dispatch,
/// Antigravity from the server-assigned conversation UUID captured by watching
/// for a new `~/.gemini/antigravity-cli/brain/<uuid>/` directory. The captured
/// locator is emitted as a `SessionLocatorCaptured` event and persisted by the
/// dispatcher onto the registry record (no sidecar).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HarnessKind {
    ClaudeCode,
    Codex,
    Antigravity,
}

impl HarnessKind {
    /// Whether Switchboard can set this harness's model per agent (via a
    /// per-invocation CLI flag). True for every harness: Claude (`--model`),
    /// Codex (`-m`), Antigravity (`--model`).
    ///
    /// Antigravity was false until `agy` 1.1.x made `--model` work headlessly
    /// without mutating the harness's own global config — the two facts that
    /// had ruled it out (`harness-behavior.md` §3.3). The arm is still written
    /// out per variant rather than collapsed to `true` so that adding a harness
    /// remains a deliberate decision here.
    ///
    /// The single authority for the model-selection gate — command validation,
    /// picker visibility, and the per-agent change action all derive from this
    /// rather than re-deriving `if harness == …`. Exhaustive (no `_` arm) so a
    /// new harness forces a deliberate decision here.
    #[must_use]
    pub fn supports_model_selection(self) -> bool {
        match self {
            Self::ClaudeCode | Self::Codex | Self::Antigravity => true,
        }
    }

    /// Whether Switchboard can set this harness's reasoning-effort level per
    /// agent. True for every supported harness: Claude (`--effort`), Codex (`-c
    /// model_reasoning_effort=`), Antigravity (`--effort`) — per
    /// `harness-behavior.md` §3.4.
    ///
    /// **Antigravity's levels are per-model and mandatory where they exist.**
    /// Unlike Claude, which accepts any level for any model and silently
    /// degrades, `agy` validates client-side before dispatch: a model with an
    /// effort axis *requires* one, a model without an axis *rejects* one, and
    /// the valid set differs by model (Gemini 3.1 Pro has low/high; the Flash
    /// models add medium). This flag only says the axis is drivable; which
    /// levels a given model accepts is the picker's business — see
    /// `effortOptionsFor` in `src/lib/agentSelection.ts`.
    ///
    /// A *separate* axis from model selection, kept as its own gate even though
    /// every current harness supports both: the axes are independent (a harness
    /// with model control but no effort control has existed here before), and
    /// collapsing them would erase that distinction the next time one does.
    /// Same authority role and exhaustiveness rationale as
    /// [`Self::supports_model_selection`].
    #[must_use]
    pub fn supports_effort_selection(self) -> bool {
        match self {
            Self::ClaudeCode | Self::Codex | Self::Antigravity => true,
        }
    }

    /// Whether this harness's reasoning effort is only meaningful **alongside a
    /// model** — i.e. whether an effort with no model selected is a coherent
    /// model/effort selection.
    ///
    /// False for Claude and Codex: their effort flag is independent of the
    /// model flag, so "harness's own default model, at high effort" is a valid
    /// configuration and both adapters emit the effort on its own.
    ///
    /// True for Antigravity, where the valid levels are a *property of the
    /// model*: `agy` decides them per model, offers none with no model
    /// selected, and rejects a level the chosen model does not have. An effort
    /// alone therefore cannot be validated or dispatched, so storing one would
    /// leave a record asserting a selection no turn can apply.
    ///
    /// Same authority + exhaustiveness role as the siblings above: the
    /// persistence boundary reads this rather than naming a harness, so adding
    /// one forces the decision here instead of silently inheriting a rule
    /// written for a different harness's constraints.
    #[must_use]
    pub fn effort_requires_model(self) -> bool {
        match self {
            Self::Antigravity => true,
            Self::ClaudeCode | Self::Codex => false,
        }
    }

    /// Whether Switchboard may re-read this harness's session file to pick up
    /// turns the user added by continuing the session in the harness's own TUI
    /// (staleness refresh on project re-activation).
    ///
    /// This is the **live-matched** capability: only true when the live stream's
    /// per-turn id equals the one the session file stores, so a turn that
    /// streamed live *and* is on disk dedups as one. Without it, re-reading a
    /// file that already contains a turn we streamed live would duplicate that
    /// turn (the disk copy's hydration key wouldn't match the live turn's). Only
    /// Claude is confirmed: the **first** assistant `message.id` round-trips
    /// live↔disk and is the dedup `hydration_key` — it is parse-invariant across
    /// a mid-flight vs completed read, unlike the final id (which the cost-join
    /// `stable_message_id` uses). Codex has a re-parse-stable disk key but its
    /// live-stream parity is unprobed, so it stays once-per-session;
    /// Antigravity has no per-turn id at all. Same authority + exhaustiveness
    /// role as the two siblings above.
    #[must_use]
    pub fn supports_refresh(self) -> bool {
        match self {
            Self::ClaudeCode => true,
            Self::Codex | Self::Antigravity => false,
        }
    }

    /// Whether this harness supports **the fork lifecycle Switchboard
    /// implements**: branching a session at its current tip into a new one that
    /// carries the full prior context, *deferred* so that nothing happens until
    /// the branch's first dispatch, which materializes it under a session id the
    /// caller assigned in advance.
    ///
    /// The deferral and the caller-assigned id are part of the definition, not
    /// incidental — together they are what let a fork pre-generate its locator
    /// at registration like any fresh agent and carry
    /// [`crate::AgentRecord::forked_from_session`] as its materialization token.
    /// True only for Claude (`--resume <parent> --session-id <new>
    /// --fork-session`), the only harness offering that shape.
    ///
    /// **A harness whose fork is *eager* does not belong here.** Codex's
    /// experimental app-server `thread/fork` creates the new session
    /// immediately and hands back its id, so it has no deferred state for the
    /// provenance field to hold and would need its own registration path —
    /// flipping this arm to `true` is *necessary but not sufficient* for it,
    /// and doing only that would produce records
    /// [`crate::project::Project::fork_agent`] cannot honor. Antigravity forks
    /// only server-side, on its own initiative. Per `harness-behavior.md` §3.5.
    ///
    /// **Naming:** "session fork" here is Claude's `--fork-session`, which is
    /// the headless equivalent of its TUI `/branch`. Claude's TUI `/fork` is an
    /// unrelated operation (spawning a background agent that inherits the
    /// conversation) — see the naming note in `harness-behavior.md` §3.5.
    ///
    /// Same authority + exhaustiveness role as the three siblings above.
    #[must_use]
    pub fn supports_session_fork(self) -> bool {
        match self {
            Self::ClaudeCode => true,
            Self::Codex | Self::Antigravity => false,
        }
    }
}

/// The two independent per-agent selection axes. A closed, complete set — model
/// and effort are the whole feature; a third axis would be a new feature, not an
/// additive variant — so this is deliberately **not** `#[non_exhaustive]`: every
/// match site should break if the set ever changes. Used to tag which axis a
/// [`crate::error::CoreError::SelectionUnsupported`] refers to, modeled as a
/// type (not a string) so a mistyped axis is a compile error, consistent with
/// the rest of the crate's closed-set-as-enum style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionAxis {
    Model,
    Effort,
}

/// Lowercase wording for error messages (`"model"` / `"effort"`).
impl fmt::Display for SelectionAxis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Model => f.write_str("model"),
            Self::Effort => f.write_str("effort"),
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
        assert!(HarnessKind::Antigravity.supports_model_selection());
    }

    #[test]
    fn supports_effort_selection_per_variant() {
        assert!(HarnessKind::ClaudeCode.supports_effort_selection());
        assert!(HarnessKind::Codex.supports_effort_selection());
        assert!(HarnessKind::Antigravity.supports_effort_selection());
    }

    #[test]
    fn effort_requires_model_is_antigravity_only() {
        // Claude and Codex emit `--effort` independently of `--model`, so
        // "default model at high effort" is a valid selection for them.
        assert!(HarnessKind::Antigravity.effort_requires_model());
        assert!(!HarnessKind::ClaudeCode.effort_requires_model());
        assert!(!HarnessKind::Codex.effort_requires_model());
    }

    #[test]
    fn supports_refresh_is_claude_only() {
        assert!(HarnessKind::ClaudeCode.supports_refresh());
        assert!(!HarnessKind::Codex.supports_refresh());
        assert!(!HarnessKind::Antigravity.supports_refresh());
    }

    #[test]
    fn supports_session_fork_is_claude_only() {
        // Codex's app-server `thread/fork` exists but is deliberately unwired in
        // v1 — this gate is what keeps the Fork action off those agents.
        assert!(HarnessKind::ClaudeCode.supports_session_fork());
        assert!(!HarnessKind::Codex.supports_session_fork());
        assert!(!HarnessKind::Antigravity.supports_session_fork());
    }

    #[test]
    fn selection_axis_display_is_lowercase_wording() {
        assert_eq!(SelectionAxis::Model.to_string(), "model");
        assert_eq!(SelectionAxis::Effort.to_string(), "effort");
    }

    #[test]
    fn display_uses_user_facing_names_with_space_for_claude() {
        assert_eq!(format!("{}", HarnessKind::ClaudeCode), "Claude Code");
        assert_eq!(format!("{}", HarnessKind::Codex), "Codex");
        assert_eq!(format!("{}", HarnessKind::Antigravity), "Antigravity");
    }
}
