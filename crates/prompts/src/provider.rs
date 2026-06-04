//! The provider abstraction. Two implementations: `LocalProvider` (filesystem,
//! synchronous work) and `McpProvider` (network). Both are `async` because the
//! MCP provider does real I/O; `LocalProvider` satisfies the async signatures
//! with synchronous bodies.
//!
//! Crate-internal: the app drives everything through `PromptService`, never the
//! provider trait directly, so this stays `pub(crate)` (which also keeps the
//! `async_fn_in_trait` lint — a public-API concern — from applying).

use std::collections::BTreeMap;

use crate::error::PromptError;
use crate::model::Prompt;

/// A source of prompts addressed under a single prefix. Implementations resolve
/// only within their own prefix — there is no cross-provider fallback.
pub(crate) trait PromptProvider: Send + Sync {
    /// All prompts this provider exposes. **Infallible**: a provider that can't
    /// be reached degrades to an empty list with a warning (the cache-build path
    /// must not fail because one provider is down). The cache, not this call, is
    /// what the `list_prompts` hot path reads.
    async fn list(&self) -> Vec<Prompt>;

    /// Render `name` with `args`, returning the finished text. `args` is the
    /// identical map used for preview and send so the two never diverge. The
    /// error must never embed a bearer token.
    async fn render(
        &self,
        name: &str,
        args: &BTreeMap<String, String>,
    ) -> Result<String, PromptError>;
}
