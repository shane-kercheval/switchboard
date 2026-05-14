//! Switchboard harness adapters.
//!
//! Defines the `HarnessAdapter` trait and provides two implementations:
//! - `ClaudeCodeAdapter` — spawns `claude -p` and maps stream-json output to events.
//! - `MockHarnessAdapter` — emits canned events in-process; no subprocess needed.
//!   Select via `SWITCHBOARD_HARNESS=mock` at app startup (see M1.3 plan §9).

pub mod adapter;
pub mod claude_code;
pub mod events;
pub mod mock;
mod parser;

pub use adapter::{DispatchError, EventStream, HarnessAdapter};
pub use claude_code::ClaudeCodeAdapter;
pub use events::{AdapterEvent, FailureKind, NormalizedEvent, TurnId, TurnOutcome};
pub use mock::{MockHarnessAdapter, MockScenario};
