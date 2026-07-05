//! Switchboard prompts — the provider framework, the local file-based prompt
//! provider, and the MCP-server provider. Pure Rust, no Tauri.
//!
//! A **prompt** is a reusable, optionally parameterized text template resolved
//! from a **provider**. Providers are addressed by prefix (`local:<name>`,
//! `<provider>:<name>`); the `local` prefix is reserved for the built-in file
//! store. The prompt/argument data model mirrors the MCP `prompts/list` shape so
//! the local and MCP providers share one type. See `docs/system-design.md` §6
//! and the milestone plan for the design.
//!
//! Config-directory resolution and the secret-store backend live in `crates/app`
//! (it owns the `directories`/`SWITCHBOARD_CONFIG_DIR` logic and the keychain);
//! this crate takes already-resolved paths and an injected [`SecretStore`] so
//! dev-instance isolation and test hermeticity stay intact.

mod builtin;
mod config;
mod local;
mod mcp;
mod model;
mod provider;
mod secret;
mod service;

pub use builtin::builtin_prompt_content;
pub use config::{McpProviderConfig, McpTransport, PromptConfig, resolve_local_dirs};
pub use model::{BUILTIN_PROVIDER, LOCAL_PROVIDER, Prompt, PromptArgument, PromptId};
pub use secret::{InMemorySecretStore, SecretStore, SecretStoreError};
pub use service::{McpProviderInfo, PromptService, PromptSource, ProviderStatus, RenderedPrompt};

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

        /// Could not connect to (or initialize a session with) an MCP provider.
        /// `message` is the transport/SDK error — never a bearer token.
        #[error("could not reach MCP provider {provider:?}: {message}")]
        McpConnect { provider: String, message: String },

        /// An MCP `prompts/get` failed for a reason other than bad arguments.
        #[error("MCP provider {provider:?} failed to render prompt {name:?}: {message}")]
        McpRequest {
            provider: String,
            name: String,
            message: String,
        },

        /// The MCP server rejected the supplied arguments (`-32602` invalid
        /// params — bad name or missing/invalid required argument). The server's
        /// message is surfaced (it typically names the offending argument).
        #[error("MCP provider {provider:?} rejected arguments for prompt {name:?}: {message}")]
        McpInvalidArguments {
            provider: String,
            name: String,
            message: String,
        },

        /// `prompts/get` succeeded but returned no text content (only image /
        /// resource parts, which v1 drops) — there is nothing to send.
        #[error("MCP prompt {name:?} from provider {provider:?} produced no text content")]
        McpEmptyContent { provider: String, name: String },

        /// A provider name supplied to add/update is not a usable addressing
        /// prefix (empty, a reserved prefix `local`/`builtin`, or contains `:`).
        #[error(
            "invalid provider name {name:?}: must be non-empty, not a reserved prefix (`local`, `builtin`), and contain no `:`"
        )]
        InvalidProviderName { name: String },

        /// A provider with this name is already configured.
        #[error("an MCP provider named {name:?} already exists")]
        DuplicateProvider { name: String },

        /// Writing the user-global `config.yaml` failed (I/O, or it isn't a YAML
        /// mapping we can safely round-trip).
        #[error("could not update {path}: {message}")]
        ConfigWrite { path: PathBuf, message: String },

        /// The service has no resolved config path (the disabled service); there
        /// is nowhere to persist provider config.
        #[error("prompt providers are not configured (no config path)")]
        NotConfigured,

        /// The secret store could not store or delete a credential.
        #[error(transparent)]
        Secret(#[from] crate::secret::SecretStoreError),
    }
}
