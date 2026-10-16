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
//! - *Turn* (send, forward, run workflow, fork): open the project, then the
//!   blocking window sync ([`desktop_sync`]), then the operation. Nothing is
//!   dispatched if either step fails.
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
pub mod compact;
pub mod context;
pub mod desktop_sync;
pub mod error_code;
pub mod fork;
pub mod handle;
pub mod ledger;
pub mod read;
pub mod send;
pub mod usage;
pub mod wait;
pub mod workflow;

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;

/// The most characters a serialized tool result may carry, for every client.
/// MCP sets no limit; each client applies its own on top of this one. Claude
/// Code shows the model up to about 50,000 characters directly and saves a
/// larger result to a file the model can open; Codex keeps about 10,000
/// tokens and cuts the middle of anything longer. A model that needs more
/// pages with `read_send_output` or asks the agent.
pub const RESULT_BUDGET_CHARS: usize = 100_000;

/// Characters per page of [`read::read_send_output`]: the result budget less
/// room for the envelope around the text.
pub const OUTPUT_PAGE_CHARS: usize = 95_000;

/// The smallest result budget a caller may ask for with `max_chars`: room
/// for the envelope and one useful page of text.
pub const MIN_RESULT_CHARS: usize = 10_000;

/// The result budget for one call: the caller's `max_chars` clamped to
/// [`MIN_RESULT_CHARS`]..=[`RESULT_BUDGET_CHARS`], or the full budget. A
/// caller lowers it to fit its client: Claude Desktop's Code tab shows a
/// result inline only under 50,000 characters.
#[must_use]
pub fn result_budget_chars(max_chars: Option<usize>) -> usize {
    max_chars.map_or(RESULT_BUDGET_CHARS, |n| {
        n.clamp(MIN_RESULT_CHARS, RESULT_BUDGET_CHARS)
    })
}

/// A cut's follow-up call with the caller's cap kept: `max_chars` is added
/// when the budget is below the full one, so following the hint stays under
/// the size the caller asked for.
#[must_use]
pub fn with_cap(mut call: serde_json::Value, budget_chars: usize) -> serde_json::Value {
    if budget_chars < RESULT_BUDGET_CHARS {
        call["max_chars"] = serde_json::json!(budget_chars);
    }
    call
}

/// Characters of text a `read_send_output` page may hold within
/// `budget_chars`: the budget less the envelope room the full budget leaves
/// for [`OUTPUT_PAGE_CHARS`].
#[must_use]
pub fn output_page_chars(budget_chars: usize) -> usize {
    budget_chars
        .saturating_sub(RESULT_BUDGET_CHARS - OUTPUT_PAGE_CHARS)
        .max(1)
}
