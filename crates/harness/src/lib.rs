//! Switchboard harness adapters.
//!
//! Defines the `HarnessAdapter` trait and provides three implementations:
//! - `ClaudeCodeAdapter` — spawns `claude -p` and maps stream-json output to events.
//! - `CodexAdapter` — spawns `codex exec --json` and maps the Codex stream-event
//!   vocabulary to events. Separate module from Claude because Codex's vocabulary
//!   differs structurally (no envelope wrapper; `item.started` / `item.completed`;
//!   `thread.started` for session capture).
//! - `MockHarnessAdapter` — emits canned events in-process; no subprocess needed.
//!   Select via `SWITCHBOARD_HARNESS=mock` at app startup.

pub mod adapter;
pub mod claude_code;
pub mod codex;
pub mod events;
pub mod mock;
mod parser;
pub mod subprocess;

pub use adapter::{DispatchError, EventStream, HarnessAdapter};
pub use claude_code::ClaudeCodeAdapter;
pub use codex::CodexAdapter;
pub use events::{
    AdapterEvent, ContentKind, FailureKind, McpServerStatus, NormalizedEvent, ToolKind, TurnId,
    TurnOutcome, TurnUsage,
};
pub use mock::{MockHarnessAdapter, MockScenario};
