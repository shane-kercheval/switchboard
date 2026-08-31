//! Switchboard harness adapters.
//!
//! Defines the `HarnessAdapter` trait and provides four implementations:
//! - `ClaudeCodeAdapter` — spawns `claude -p` and maps stream-json output to events.
//! - `CodexAdapter` — spawns `codex exec --json` and maps the Codex stream-event
//!   vocabulary to events. Separate module from Claude because Codex's vocabulary
//!   differs structurally (no envelope wrapper; `item.started` / `item.completed`;
//!   `thread.started` for session capture).
//! - `MockHarnessAdapter` — emits canned events in-process; no subprocess needed.
//!   Select via `SWITCHBOARD_HARNESS=mock` at app startup.

pub mod adapter;
pub mod antigravity;
pub mod claude_code;
pub mod codex;
pub mod events;
pub mod facets;
pub mod forward;
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
pub use claude_code::{
    ClaudeCodeAdapter, claude_session_file_path, claude_transport_prompt, load_claude_transcript,
};
pub use codex::CodexAdapter;
pub use codex::session_file::{
    AttachLookupError, find_codex_session_file_for_attach, load_codex_transcript,
};
pub use events::{
    AdapterEvent, CancelSource, ContentKind, ContextWindowSource, FailureKind, McpServerStatus,
    MessageId, NormalizedEvent, RateLimitSource, ToolKind, TurnId, TurnOutcome, TurnSpend,
    TurnUsage,
};
pub use facets::{
    EditChange, EditPair, EditedFile, McpMutation, McpMutationField, TodoItem, ToolFacet,
};
pub use forward::{
    ForwardedBlock, compose_forwarded_message, empty_sources_reason, is_forwardable_text,
    latest_completed_agent_text,
};
pub use mock::{MockHarnessAdapter, MockScenario};
pub use resume::interactive_resume_command;
pub use transcript::{
    LoadTranscriptError, LoadedTranscript, ParseWarning, SessionMetaInfo, SystemMarker, Turn,
    TurnItem, TurnStatus, UserPromptSource,
};
