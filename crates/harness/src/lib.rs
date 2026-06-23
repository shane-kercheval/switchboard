//! Switchboard harness adapters.
//!
//! Defines the `HarnessAdapter` trait and provides four implementations:
//! - `ClaudeCodeAdapter` — spawns `claude -p` and maps stream-json output to events.
//! - `CodexAdapter` — spawns `codex exec --json` and maps the Codex stream-event
//!   vocabulary to events. Separate module from Claude because Codex's vocabulary
//!   differs structurally (no envelope wrapper; `item.started` / `item.completed`;
//!   `thread.started` for session capture).
//! - `GeminiAdapter` — spawns `gemini` (prompt via attached `--prompt=<value>`,
//!   not split `-p <value>`) and maps Gemini's flat stream-json vocabulary to
//!   events. Pattern parallels Claude (caller-controlled session
//!   ID); Gemini-specific behaviors (UUID v4 for session IDs, `--skip-trust`,
//!   `update_topic` filtering, empty live tool output) live in the module
//!   docstring.
//! - `MockHarnessAdapter` — emits canned events in-process; no subprocess needed.
//!   Select via `SWITCHBOARD_HARNESS=mock` at app startup.

pub mod adapter;
pub mod antigravity;
pub mod claude_code;
pub mod codex;
pub mod events;
pub mod forward;
pub mod gemini;
pub mod meta_sidecar;
pub mod mock;
mod parser;
pub mod resume;
pub mod subprocess;
pub mod transcript;
pub mod turnmeta_sidecar;

pub use adapter::{DispatchError, DispatchOptions, EventStream, HarnessAdapter};
pub use antigravity::AntigravityAdapter;
pub use antigravity::session_file::load_antigravity_transcript;
pub use claude_code::{ClaudeCodeAdapter, claude_session_file_path, load_claude_transcript};
pub use codex::CodexAdapter;
pub use codex::session_file::{
    AttachLookupError, find_codex_session_file_for_attach, load_codex_transcript,
};
pub use events::{
    AdapterEvent, CancelSource, ContentKind, ContextWindowSource, FailureKind, McpServerStatus,
    MessageId, NormalizedEvent, RateLimitSource, ToolKind, TurnId, TurnOutcome, TurnSpend,
    TurnUsage,
};
pub use forward::{ForwardedBlock, compose_forwarded_message, latest_completed_agent_text};
pub use gemini::GeminiAdapter;
pub use gemini::session_file::{
    CandidateMatch as GeminiCandidateMatch, classify_candidate as classify_gemini_candidate,
    gemini_session_file_candidates, id_prefix as gemini_session_id_prefix, load_gemini_transcript,
};
pub use mock::{MockHarnessAdapter, MockScenario};
pub use resume::interactive_resume_command;
pub use transcript::{
    LoadTranscriptError, LoadedTranscript, ParseWarning, SessionMetaInfo, SystemMarker, Turn,
    TurnItem, TurnStatus, UserPromptSource,
};
