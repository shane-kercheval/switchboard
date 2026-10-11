//! The application layer for callers that are **not the window**: the
//! operations the compose bar performs in TypeScript today (fan-out, a
//! composed forward, outcome tracking, waiting), plus project, agent, prompt,
//! and workflow operations, callable without the window for any project
//! whether or not the window has opened it. The MCP tools are thin wrappers
//! over these functions; the remote-control backend is meant to become one too.
//!
//! **Explicit ids.** Nothing here reads `active_project_id`. Every operation
//! takes the `project_id` it acts on; agent operations take `(project_id,
//! agent_id)` and check the agent belongs to that project. Names are for
//! display only.
//!
//! **Loading tiers.** Each operation opens the project only as far as it needs,
//! because opening locks the project for the app's lifetime and the window
//! round trip exists only because the window's first history load assumes no
//! turn is running:
//! - *Read* (lists, conversation, outputs, waits, cancels, workflow description
//!   and runs): open nothing, lock nothing; a missing working directory does
//!   not fail a read.
//! - *Change* (create, rename, model and effort, dismissing a run): open the
//!   project in Rust, then notify the window without waiting.
//! - *Turn* (send, forward, run workflow): open the project, then the blocking
//!   window sync ([`desktop_sync`]), then the operation. Nothing is dispatched
//!   if either step fails.
//!
//! **One error type.** Every operation returns [`crate::error::AppError`];
//! [`error_code`] maps it to the closed set of codes a program acts on.
//!
//! **Sizes and waits.** Every blocking operation takes a [`wait::WaitBudget`];
//! every result that can be large goes through [`budget::fit`] on its way out
//! of the tool layer, with the rules for each result type kept beside that
//! type here. The numbers come from the client measurements recorded in
//! `docs/research/mcp-client-behavior.md`.

pub mod budget;
pub mod change;
pub mod desktop_sync;
pub mod error_code;
pub mod handle;
pub mod ledger;
pub mod read;
pub mod send;
pub mod wait;
pub mod workflow;

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

/// The most characters a serialized tool result may carry. Codex keeps only
/// about 10,000 tokens of a result and silently cuts the middle; at the
/// densest ratio measured (2.5 characters per token for JSON with escapes)
/// 25,000 characters stays under that. Claude Code's own cut, at ~50,000
/// characters, is looser.
pub const RESULT_BUDGET_CHARS: usize = 25_000;

/// Characters per page of [`read::read_send_output`]: the result budget less
/// room for the envelope around the text.
pub const OUTPUT_PAGE_CHARS: usize = 20_000;
