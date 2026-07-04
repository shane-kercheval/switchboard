//! The built-in prompt provider: a fixed, read-only library of example prompts
//! baked into the binary (`include_str!`). It uses the identical file format and
//! rendering as the local provider (YAML frontmatter + a `MiniJinja` body) and
//! reuses its parser and renderer — it differs only in *source* (compiled-in vs.
//! filesystem) and in carrying the reserved `builtin` provider identity, so a
//! user's same-named local prompt never collides with a built-in.
//!
//! Built-ins are always current with the installed app: improving one or adding
//! a new one reaches every install on update, with no seeding, marker, or
//! manual distribution. The app never writes a built-in to the user's folder
//! unless the user explicitly copies one to customize it.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::PromptError;
use crate::local::{ParsedPrompt, parse_prompt_file, render_template};
use crate::model::{BUILTIN_PROVIDER, Prompt};
use crate::provider::PromptProvider;

/// Synthetic path for parse diagnostics only — built-ins have no real file. The
/// shared parser takes a `&Path` for its frontmatter error message; a generic
/// stand-in suffices because the parse test makes a broken built-in a CI
/// failure, not a field condition.
fn label() -> &'static Path {
    Path::new("<built-in>")
}

/// The bundled prompts as raw markdown. The frontmatter `name:` is the *single*
/// source of truth for a built-in's id — there is no redundant catalog name to
/// drift out of sync with it. Content lives beside this module so the pure crate
/// owns it end to end.
const BUILTIN_PROMPTS: &[&str] = &[
    include_str!("../resources/prompts/code-review.md"),
    include_str!("../resources/prompts/analyze-ai-reviews.md"),
    include_str!("../resources/prompts/security-review.md"),
];

/// Serves the binary-baked built-in prompts. Stateless — the content is a
/// compile-time constant — so it is cheap to construct per use.
pub(crate) struct BuiltinProvider;

impl BuiltinProvider {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl BuiltinProvider {
    /// The built-ins, resolved synchronously. The content is compiled in, so this
    /// is pure CPU work — no I/O, no await. Used directly by `PromptService::get`
    /// so a built-in resolves even under a cold cache (the freshness contract).
    pub(crate) fn list_sync() -> Vec<Prompt> {
        BUILTIN_PROMPTS
            .iter()
            .filter_map(|content| match parse_prompt_file(label(), content) {
                Ok(parsed) => Some(parsed.into_prompt(BUILTIN_PROVIDER)),
                // A baked-in prompt that won't parse is a build-time authoring
                // bug, not a runtime condition; skip it (with a warning) rather
                // than poison the whole list, mirroring the local provider. The
                // `all_built_in_prompts_parse` test makes this unreachable in a
                // shipped build.
                Err(e) => {
                    tracing::warn!(error = %e, "skipping unparseable built-in prompt");
                    None
                }
            })
            .collect()
    }
}

impl PromptProvider for BuiltinProvider {
    async fn list(&self) -> Vec<Prompt> {
        Self::list_sync()
    }

    async fn render(
        &self,
        name: &str,
        args: &BTreeMap<String, String>,
    ) -> Result<String, PromptError> {
        let parsed = find_parsed(name).ok_or_else(|| PromptError::PromptNotFound {
            provider: BUILTIN_PROVIDER.to_owned(),
            name: name.to_owned(),
        })?;
        render_template(name, &parsed.body, &parsed.arguments, args)
    }
}

/// Parse each built-in and return the first whose frontmatter `name:` matches
/// `name`. Parsing-to-locate is fine at this scale (a handful of cold-path
/// entries) and keeps the frontmatter the sole id authority. A built-in that
/// won't parse is skipped (it can't be addressed); the parse test guarantees
/// none ship broken.
fn find_parsed(name: &str) -> Option<ParsedPrompt> {
    BUILTIN_PROMPTS
        .iter()
        .filter_map(|content| parse_prompt_file(label(), content).ok())
        .find(|parsed| parsed.name == name)
}

/// The raw markdown of the built-in prompt named `name`, or `None` if there is
/// no such built-in. The app's "Copy to my prompts" command uses this to write
/// an owned copy into the user's prompts folder; it is a static-asset lookup, so
/// it needs no `PromptService` state. Matches on the frontmatter `name:` (the
/// same id authority `render` and `list` use).
#[must_use]
pub fn builtin_prompt_content(name: &str) -> Option<&'static str> {
    BUILTIN_PROMPTS
        .iter()
        .find(|content| {
            parse_prompt_file(label(), content)
                .ok()
                .is_some_and(|parsed| parsed.name == name)
        })
        .copied()
}

/// The **unrendered** template body of the built-in named `name` (frontmatter
/// stripped, `MiniJinja` placeholders left intact), or `None` if there is no such
/// built-in. Backs the read-only UI preview of a prompt. Distinct from
/// [`builtin_prompt_content`], which returns the whole file (frontmatter + body)
/// for "Copy to my prompts"; a preview wants only the prompt text the user reads.
pub(crate) fn builtin_prompt_source(name: &str) -> Option<String> {
    find_parsed(name).map(|parsed| parsed.body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[tokio::test]
    async fn lists_built_in_prompts_under_builtin_provider() {
        let prompts = BuiltinProvider::new().list().await;
        let names: Vec<&str> = prompts.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"code-review"), "got {names:?}");
        assert!(names.contains(&"analyze-ai-reviews"), "got {names:?}");
        assert!(names.contains(&"security-review"), "got {names:?}");
        assert!(prompts.iter().all(|p| p.provider == BUILTIN_PROVIDER));
    }

    #[tokio::test]
    async fn security_review_renders_with_and_without_context() {
        let provider = BuiltinProvider::new();

        let with = provider
            .render(
                "security-review",
                &args(&[("context", "the new file-upload endpoint")]),
            )
            .await
            .unwrap();
        assert!(with.contains("the new file-upload endpoint"));
        // The base review target is ALWAYS present — context supplements it.
        assert!(with.contains("Uncommitted changes in the current directory"));
        assert!(with.contains("Additional context and focus"));

        let without = provider
            .render("security-review", &args(&[]))
            .await
            .unwrap();
        assert!(without.contains("Uncommitted changes in the current directory"));
        assert!(!without.contains("Additional context and focus"));
        assert!(!without.contains("the new file-upload endpoint"));
    }

    #[tokio::test]
    async fn code_review_renders_with_and_without_context() {
        let provider = BuiltinProvider::new();

        let with = provider
            .render(
                "code-review",
                &args(&[("context", "focus on the auth path")]),
            )
            .await
            .unwrap();
        assert!(with.contains("focus on the auth path"));
        // The base review target is ALWAYS present — context supplements, never
        // replaces it (a focus note must not become the entire review subject).
        assert!(with.contains("Uncommitted changes in the current directory"));
        assert!(with.contains("Additional context and focus"));

        let without = provider.render("code-review", &args(&[])).await.unwrap();
        assert!(without.contains("Uncommitted changes in the current directory"));
        assert!(!without.contains("Additional context and focus"));
        assert!(!without.contains("focus on the auth path"));
    }

    #[tokio::test]
    async fn analyze_ai_reviews_requires_review_and_substitutes_it() {
        let provider = BuiltinProvider::new();

        let err = provider
            .render("analyze-ai-reviews", &args(&[]))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            PromptError::MissingRequiredArgument { argument, .. } if argument == "review"
        ));

        let out = provider
            .render(
                "analyze-ai-reviews",
                &args(&[("review", "REVIEWER SAYS X")]),
            )
            .await
            .unwrap();
        assert!(out.contains("REVIEWER SAYS X"));
    }

    #[tokio::test]
    async fn render_unknown_built_in_is_not_found() {
        let err = BuiltinProvider::new()
            .render("nope", &args(&[]))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            PromptError::PromptNotFound { provider, name }
                if provider == BUILTIN_PROVIDER && name == "nope"
        ));
    }

    #[test]
    fn content_returns_raw_markdown_for_known_built_in() {
        let content = builtin_prompt_content("code-review").unwrap();
        assert!(content.starts_with("---"));
        assert!(content.contains("name: code-review"));
        assert!(builtin_prompt_content("nope").is_none());
    }

    #[test]
    fn all_built_in_prompts_parse() {
        // Built-ins are release-owned static assets: every shipped entry must
        // parse, or it would silently vanish from the picker and break a workflow
        // (or copy) that points at it. This converts that into a CI failure. With
        // the frontmatter `name:` as the sole id authority, there is no separate
        // catalog name that could drift — so listing, render, and copy can never
        // disagree about a built-in's id.
        for (i, content) in BUILTIN_PROMPTS.iter().enumerate() {
            parse_prompt_file(label(), content)
                .unwrap_or_else(|e| panic!("built-in prompt #{i} does not parse: {e}"));
        }
    }
}
