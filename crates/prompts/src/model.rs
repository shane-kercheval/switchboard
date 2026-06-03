//! The prompt/argument data model and address parsing. The shape mirrors the
//! MCP `prompts/list` response (`name`, `title?`→omitted in v1, `description?`,
//! `arguments: [{ name, description?, required? }]`) plus Tiddly's `tags`
//! extension, so the local and MCP providers produce one type.

use serde::{Deserialize, Serialize};

use crate::error::PromptError;

/// Reserved prefix for the built-in local file store.
pub const LOCAL_PROVIDER: &str = "local";

/// A prompt as surfaced to the UI: provider-attributed metadata plus its
/// declared arguments. The template body is intentionally absent — it is never
/// sent to the frontend; only the rendered text is (via `render`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prompt {
    /// The provider prefix this prompt resolves under (`local`, or an MCP
    /// provider's registered name).
    pub provider: String,
    pub name: String,
    /// Optional in the MCP shape; required for local frontmatter (enforced at
    /// parse time). Modeled as `Option` so the two providers share one type.
    pub description: Option<String>,
    pub arguments: Vec<PromptArgument>,
    /// Tiddly's tag extension. Parsed and carried for a future library/browse
    /// view; unused by the v1 slash UI. Empty when the prompt declares none.
    pub tags: Vec<String>,
}

/// A declared argument. All arguments are strings — the MCP protocol has no type
/// system and no default-value field, and local frontmatter mirrors that surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptArgument {
    pub name: String,
    pub description: Option<String>,
    /// Defaults to `false` (matching the MCP protocol) when omitted.
    pub required: bool,
}

/// A fully-qualified prompt address: `provider:name`. The slash UI may accept a
/// bare name and resolve it against the single matching provider, but that is a
/// UI affordance — a persisted/parsed id is always prefixed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptId {
    pub provider: String,
    pub name: String,
}

impl PromptId {
    /// Parse a `provider:name` address. Strict: both parts must be non-empty.
    /// A bare name (no prefix) is rejected — prefixed lookup is the only
    /// persisted form (`docs/system-design.md` §6 "Resolution rules").
    pub fn parse(input: &str) -> Result<PromptId, PromptError> {
        match input.split_once(':') {
            Some((provider, name)) if !provider.is_empty() && !name.is_empty() => Ok(PromptId {
                provider: provider.to_owned(),
                name: name.to_owned(),
            }),
            _ => Err(PromptError::MalformedId {
                input: input.to_owned(),
            }),
        }
    }
}

impl std::fmt::Display for PromptId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.provider, self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_prefixed_id() {
        let id = PromptId::parse("local:code-review").unwrap();
        assert_eq!(id.provider, "local");
        assert_eq!(id.name, "code-review");
        assert_eq!(id.to_string(), "local:code-review");
    }

    #[test]
    fn parses_non_local_provider() {
        let id = PromptId::parse("tiddly:ai-review").unwrap();
        assert_eq!(id.provider, "tiddly");
        assert_eq!(id.name, "ai-review");
    }

    #[test]
    fn name_may_contain_colon() {
        // Split on the first colon only, so a name with a colon survives.
        let id = PromptId::parse("local:weird:name").unwrap();
        assert_eq!(id.provider, "local");
        assert_eq!(id.name, "weird:name");
    }

    #[test]
    fn rejects_bare_name() {
        assert!(matches!(
            PromptId::parse("code-review"),
            Err(PromptError::MalformedId { .. })
        ));
    }

    #[test]
    fn rejects_empty_parts() {
        for bad in ["local:", ":code-review", ":", ""] {
            assert!(
                matches!(PromptId::parse(bad), Err(PromptError::MalformedId { .. })),
                "{bad:?} should be malformed"
            );
        }
    }
}
