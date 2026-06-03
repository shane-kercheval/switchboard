//! The provider abstraction. One trait, two implementations across milestones:
//! `LocalProvider` (this milestone) and the MCP provider (later). Each provider
//! owns one prefix and resolves prompts strictly within it.

use std::collections::BTreeMap;

use crate::error::PromptError;
use crate::model::Prompt;

/// A source of prompts addressed under a single prefix. Implementations resolve
/// only within their own prefix — there is no cross-provider fallback.
pub trait PromptProvider {
    /// The prefix this provider answers to (e.g. `local`, `tiddly`).
    fn prefix(&self) -> &str;

    /// All prompts this provider exposes, in resolution order. A provider that
    /// cannot be reached degrades to an empty list with a warning rather than
    /// failing the whole listing (graceful degradation — see the cross-cutting
    /// requirements); a per-prompt parse failure is skipped, not fatal.
    fn list(&self) -> Vec<Prompt>;

    /// Render `name` with `args`, returning the finished text. Required-argument
    /// enforcement, unknown-argument rejection, and (for local) `MiniJinja`
    /// rendering happen here; `args` is the identical map used for preview and
    /// send so the two never diverge.
    fn render(&self, name: &str, args: &BTreeMap<String, String>) -> Result<String, PromptError>;
}
