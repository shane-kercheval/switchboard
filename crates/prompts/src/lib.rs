//! Switchboard prompts — provider framework and the local file-based prompt
//! provider. Pure Rust, no Tauri, no async, no network (the MCP provider lands
//! in a later milestone).
//!
//! A **prompt** is a reusable, optionally parameterized text template resolved
//! from a **provider**. Providers are addressed by prefix (`local:<name>`,
//! `<provider>:<name>`); the `local` prefix is reserved for the built-in file
//! store. The prompt/argument data model mirrors the MCP `prompts/list` shape so
//! the local and (future) MCP providers share one type. See
//! `docs/system-design.md` §6 and the milestone plan for the design.
//!
//! Config-directory resolution lives in `crates/app` (it owns the
//! `directories`/`SWITCHBOARD_CONFIG_DIR` logic); this crate takes already
//! resolved paths so dev-instance isolation and test hermeticity stay intact.

mod config;
mod local;
mod model;
mod provider;
mod service;

pub use config::{PromptConfig, resolve_local_dirs};
pub use model::{LOCAL_PROVIDER, Prompt, PromptArgument, PromptId};
pub use provider::PromptProvider;
pub use service::{PromptService, RenderedPrompt};

pub use error::PromptError;

mod error {
    use std::path::PathBuf;

    /// Errors raised while listing or rendering prompts. Carries enough context
    /// to be actionable; never embeds secrets (the local provider has none, and
    /// the future MCP provider must redact bearer tokens at its own boundary).
    #[derive(Debug, thiserror::Error)]
    #[non_exhaustive]
    pub enum PromptError {
        /// The address string was not a well-formed `provider:name` pair.
        #[error(
            "malformed prompt id {input:?}: expected `provider:name` with both parts non-empty"
        )]
        MalformedId { input: String },

        /// No provider is registered under the requested prefix.
        #[error("unknown prompt provider {provider:?}")]
        ProviderNotFound { provider: String },

        /// The provider has no prompt with the requested name.
        #[error("prompt {name:?} not found in provider {provider:?}")]
        PromptNotFound { provider: String, name: String },

        /// A required argument was not supplied.
        #[error("missing required argument {argument:?} for prompt {name:?}")]
        MissingRequiredArgument { name: String, argument: String },

        /// An argument was supplied that the prompt does not declare. Local
        /// rejects unknown args to match the MCP server's strict behavior, so a
        /// prompt behaves identically whichever store it lives in.
        #[error("unknown argument {argument:?} for prompt {name:?}; valid arguments: {valid}")]
        UnknownArgument {
            name: String,
            argument: String,
            valid: String,
        },

        /// The prompt file's frontmatter could not be parsed.
        #[error("invalid prompt frontmatter in {path}: {message}")]
        Frontmatter { path: PathBuf, message: String },

        /// The template body failed to render.
        #[error("failed to render prompt {name:?}: {message}")]
        Render { name: String, message: String },

        /// A filesystem error while reading a prompt file.
        #[error("I/O error reading prompt at {path}: {source}")]
        Io {
            path: PathBuf,
            #[source]
            source: std::io::Error,
        },
    }
}
