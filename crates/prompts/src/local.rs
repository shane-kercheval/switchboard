//! The local file-based prompt provider: scan configured directories for
//! markdown files with YAML frontmatter, parse them, and render bodies with
//! `MiniJinja`. All arguments are strings; missing optionals render empty, missing
//! required args and unknown args are errors.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use minijinja::{Environment, UndefinedBehavior};
use serde::Deserialize;

use crate::error::PromptError;
use crate::model::{LOCAL_PROVIDER, Prompt, PromptArgument};
use crate::provider::PromptProvider;

/// Reads local prompts from an ordered list of directories. Earlier directories
/// shadow later ones on name collision; within a directory, files are visited in
/// sorted filename order. A configured-but-missing directory contributes nothing.
pub struct LocalProvider {
    dirs: Vec<PathBuf>,
}

impl LocalProvider {
    #[must_use]
    pub fn new(dirs: Vec<PathBuf>) -> Self {
        Self { dirs }
    }

    /// Find the first prompt named `name` across the configured directories in
    /// resolution order. Malformed files are skipped (their name can't be read),
    /// so an unaddressable file never masks a valid one further down.
    fn find(&self, name: &str) -> Result<ParsedPrompt, PromptError> {
        for dir in &self.dirs {
            for path in md_files(dir) {
                let Ok(content) = std::fs::read_to_string(&path) else {
                    continue;
                };
                match parse_prompt_file(&path, &content) {
                    Ok(parsed) if parsed.name == name => return Ok(parsed),
                    Ok(_) => {}
                    // Mirror `list`'s diagnostic so a malformed file isn't an
                    // invisible cause of a later "not found". Debug, not warn:
                    // resolution scans siblings until a match, so a warn here
                    // could repeat per render once this is on a hot path.
                    Err(e) => {
                        tracing::debug!(path = %path.display(), error = %e, "skipping unparseable prompt while resolving by name");
                    }
                }
            }
        }
        Err(PromptError::PromptNotFound {
            provider: LOCAL_PROVIDER.to_owned(),
            name: name.to_owned(),
        })
    }
}

impl PromptProvider for LocalProvider {
    async fn list(&self) -> Vec<Prompt> {
        self.list_sync()
    }

    async fn render(
        &self,
        name: &str,
        args: &BTreeMap<String, String>,
    ) -> Result<String, PromptError> {
        self.render_sync(name, args)
    }
}

impl LocalProvider {
    /// Synchronous listing — the filesystem scan. The trait's `async list`
    /// delegates here; the work is blocking I/O, fast enough to run inline.
    fn list_sync(&self) -> Vec<Prompt> {
        let mut out = Vec::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for dir in &self.dirs {
            for path in md_files(dir) {
                let content = match std::fs::read_to_string(&path) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(path = %path.display(), error = %e, "skipping unreadable local prompt");
                        continue;
                    }
                };
                match parse_prompt_file(&path, &content) {
                    Ok(parsed) => {
                        // First occurrence of a name wins; later dirs/files are
                        // shadowed (declared-order resolution).
                        if seen.insert(parsed.name.clone()) {
                            out.push(parsed.into_prompt());
                        }
                    }
                    Err(e) => {
                        tracing::warn!(path = %path.display(), error = %e, "skipping malformed local prompt");
                    }
                }
            }
        }
        out
    }

    /// Synchronous render — find the named prompt and apply `MiniJinja`. The
    /// trait's `async render` delegates here.
    fn render_sync(
        &self,
        name: &str,
        args: &BTreeMap<String, String>,
    ) -> Result<String, PromptError> {
        let parsed = self.find(name)?;
        render_template(name, &parsed.body, &parsed.arguments, args)
    }
}

/// A parsed prompt file: frontmatter-derived metadata plus the template body.
#[derive(Debug)]
struct ParsedPrompt {
    name: String,
    description: String,
    arguments: Vec<PromptArgument>,
    tags: Vec<String>,
    body: String,
}

impl ParsedPrompt {
    fn into_prompt(self) -> Prompt {
        Prompt {
            provider: LOCAL_PROVIDER.to_owned(),
            name: self.name,
            description: Some(self.description),
            arguments: self.arguments,
            tags: self.tags,
        }
    }
}

/// The YAML frontmatter schema. Unknown keys are ignored (serde default), so a
/// Claude Code skill file — whose frontmatter carries extra keys like
/// `allowed-tools` — drops in and reads as a local prompt. `name` and
/// `description` are required (a skill has both); `arguments` and `tags` are the
/// prompt superset over a skill.
#[derive(Debug, Deserialize)]
struct Frontmatter {
    name: String,
    description: String,
    #[serde(default)]
    arguments: Vec<FrontmatterArgument>,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct FrontmatterArgument {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    required: bool,
}

/// Parse a prompt file into metadata + body. Errors if the file has no `---`
/// fenced frontmatter or the frontmatter is missing required fields.
fn parse_prompt_file(path: &Path, content: &str) -> Result<ParsedPrompt, PromptError> {
    let (yaml, body) = split_frontmatter(content).ok_or_else(|| PromptError::Frontmatter {
        path: path.to_owned(),
        message: "missing YAML frontmatter (a prompt file must begin with a `---` fenced block)"
            .to_owned(),
    })?;
    let front: Frontmatter =
        serde_norway::from_str(&yaml).map_err(|e| PromptError::Frontmatter {
            path: path.to_owned(),
            message: e.to_string(),
        })?;
    Ok(ParsedPrompt {
        name: front.name,
        description: front.description,
        arguments: front
            .arguments
            .into_iter()
            .map(|a| PromptArgument {
                name: a.name,
                description: a.description,
                required: a.required,
            })
            .collect(),
        tags: front.tags,
        body,
    })
}

/// Split a `---` fenced frontmatter block from the body. Returns `(yaml, body)`,
/// or `None` if the content doesn't open and close a frontmatter fence. Handles
/// both `\n` and `\r\n` line endings via `str::lines`.
fn split_frontmatter(content: &str) -> Option<(String, String)> {
    let mut lines = content.lines();
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    let mut yaml = String::new();
    let mut closed = false;
    let mut body = String::new();
    for line in lines {
        if closed {
            body.push_str(line);
            body.push('\n');
        } else if line.trim_end() == "---" {
            closed = true;
        } else {
            yaml.push_str(line);
            yaml.push('\n');
        }
    }
    closed.then_some((yaml, body))
}

/// Render `body` with `args`. Rejects unknown args (matching the MCP server's
/// strict behavior, so a prompt behaves identically across stores), enforces
/// required args, and renders missing optionals as empty via lenient undefined.
fn render_template(
    name: &str,
    body: &str,
    declared: &[PromptArgument],
    args: &BTreeMap<String, String>,
) -> Result<String, PromptError> {
    let declared_names: BTreeSet<&str> = declared.iter().map(|a| a.name.as_str()).collect();

    if let Some(unknown) = args.keys().find(|k| !declared_names.contains(k.as_str())) {
        let valid = declared
            .iter()
            .map(|a| a.name.clone())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(PromptError::UnknownArgument {
            name: name.to_owned(),
            argument: unknown.clone(),
            valid,
        });
    }

    for arg in declared {
        if arg.required && !args.contains_key(&arg.name) {
            return Err(PromptError::MissingRequiredArgument {
                name: name.to_owned(),
                argument: arg.name.clone(),
            });
        }
    }

    let mut env = Environment::new();
    // Lenient: an unfilled optional argument is undefined, which prints as empty
    // and is falsy in `{% if %}` — exactly the "missing optional renders empty"
    // semantics. Only supplied args go into the context.
    env.set_undefined_behavior(UndefinedBehavior::Lenient);
    let ctx: BTreeMap<&str, &str> = args.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    env.render_str(body, ctx).map_err(|e| PromptError::Render {
        name: name.to_owned(),
        message: e.to_string(),
    })
}

/// Sorted list of `*.md` files directly in `dir` (non-recursive). A missing
/// directory yields nothing; a read error is logged and yields nothing.
fn md_files(dir: &Path) -> Vec<PathBuf> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!(dir = %dir.display(), error = %e, "cannot read prompt directory");
            return Vec::new();
        }
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        })
        .collect();
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(dir: &Path, file: &str, content: &str) {
        std::fs::write(dir.join(file), content).unwrap();
    }

    fn args(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    const CODE_REVIEW: &str = "---\nname: code-review\ndescription: Review the diff.\narguments:\n  - name: focus\n    description: Optional focus area.\n    required: false\ntags:\n  - review\n---\nReview the changes.\n{% if focus %}Focus: {{ focus }}{% endif %}\n";

    #[test]
    fn lists_prompt_with_metadata() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "code-review.md", CODE_REVIEW);
        let provider = LocalProvider::new(vec![dir.path().to_path_buf()]);

        let prompts = provider.list_sync();
        assert_eq!(prompts.len(), 1);
        let p = &prompts[0];
        assert_eq!(p.provider, "local");
        assert_eq!(p.name, "code-review");
        assert_eq!(p.description.as_deref(), Some("Review the diff."));
        assert_eq!(p.tags, vec!["review".to_owned()]);
        assert_eq!(p.arguments.len(), 1);
        assert_eq!(p.arguments[0].name, "focus");
        assert!(!p.arguments[0].required);
        assert_eq!(
            p.arguments[0].description.as_deref(),
            Some("Optional focus area.")
        );
    }

    #[test]
    fn renders_with_optional_arg_supplied() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "code-review.md", CODE_REVIEW);
        let provider = LocalProvider::new(vec![dir.path().to_path_buf()]);

        let out = provider
            .render_sync("code-review", &args(&[("focus", "security")]))
            .unwrap();
        assert!(out.contains("Review the changes."));
        assert!(out.contains("Focus: security"));
    }

    #[test]
    fn missing_optional_renders_empty() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "code-review.md", CODE_REVIEW);
        let provider = LocalProvider::new(vec![dir.path().to_path_buf()]);

        let out = provider.render_sync("code-review", &args(&[])).unwrap();
        assert!(out.contains("Review the changes."));
        // The `{% if focus %}` block is omitted because focus is undefined/falsy.
        assert!(!out.contains("Focus:"));
    }

    #[test]
    fn missing_required_arg_is_error() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "p.md",
            "---\nname: p\ndescription: d\narguments:\n  - name: target\n    required: true\n---\nDo {{ target }}\n",
        );
        let provider = LocalProvider::new(vec![dir.path().to_path_buf()]);

        let err = provider.render_sync("p", &args(&[])).unwrap_err();
        assert!(matches!(
            err,
            PromptError::MissingRequiredArgument { argument, .. } if argument == "target"
        ));
    }

    #[test]
    fn required_arg_present_renders() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "p.md",
            "---\nname: p\ndescription: d\narguments:\n  - name: target\n    required: true\n---\nDo {{ target }}\n",
        );
        let provider = LocalProvider::new(vec![dir.path().to_path_buf()]);

        let out = provider
            .render_sync("p", &args(&[("target", "X")]))
            .unwrap();
        assert!(out.contains("Do X"));
    }

    #[test]
    fn unknown_arg_is_rejected_with_valid_names() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "code-review.md", CODE_REVIEW);
        let provider = LocalProvider::new(vec![dir.path().to_path_buf()]);

        let err = provider
            .render_sync("code-review", &args(&[("focus", "x"), ("bogus", "y")]))
            .unwrap_err();
        match err {
            PromptError::UnknownArgument {
                argument, valid, ..
            } => {
                assert_eq!(argument, "bogus");
                assert_eq!(valid, "focus");
            }
            other => panic!("expected UnknownArgument, got {other:?}"),
        }
    }

    #[test]
    fn render_unknown_prompt_is_not_found() {
        let dir = TempDir::new().unwrap();
        let provider = LocalProvider::new(vec![dir.path().to_path_buf()]);
        let err = provider.render_sync("nope", &args(&[])).unwrap_err();
        assert!(matches!(err, PromptError::PromptNotFound { .. }));
    }

    #[test]
    fn earlier_directory_shadows_later() {
        let first = TempDir::new().unwrap();
        let second = TempDir::new().unwrap();
        write(
            first.path(),
            "x.md",
            "---\nname: shared\ndescription: from-first\n---\nFIRST\n",
        );
        write(
            second.path(),
            "x.md",
            "---\nname: shared\ndescription: from-second\n---\nSECOND\n",
        );
        let provider = LocalProvider::new(vec![
            first.path().to_path_buf(),
            second.path().to_path_buf(),
        ]);

        let prompts = provider.list_sync();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].description.as_deref(), Some("from-first"));

        let out = provider.render_sync("shared", &args(&[])).unwrap();
        assert!(out.contains("FIRST"));
        assert!(!out.contains("SECOND"));
    }

    #[test]
    fn skill_file_with_extra_frontmatter_keys_parses() {
        // A Claude Code skill: name + description + body, plus skill-only keys we
        // don't model. It must read as a local prompt (skill ⊂ prompt).
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "SKILL.md",
            "---\nname: my-skill\ndescription: A skill.\nallowed-tools: [Read, Edit]\nlicense: MIT\n---\nSkill body instructions.\n",
        );
        let provider = LocalProvider::new(vec![dir.path().to_path_buf()]);

        let prompts = provider.list_sync();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].name, "my-skill");
        assert!(prompts[0].arguments.is_empty());
        let out = provider.render_sync("my-skill", &args(&[])).unwrap();
        assert!(out.contains("Skill body instructions."));
    }

    #[test]
    fn malformed_file_is_skipped_in_list_but_valid_sibling_remains() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "good.md",
            "---\nname: good\ndescription: d\n---\nBody\n",
        );
        write(dir.path(), "bad.md", "no frontmatter here\n");
        write(
            dir.path(),
            "missing-name.md",
            "---\ndescription: d\n---\nBody\n",
        );
        let provider = LocalProvider::new(vec![dir.path().to_path_buf()]);

        let prompts = provider.list_sync();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].name, "good");
    }

    #[test]
    fn parse_rejects_missing_required_frontmatter_field() {
        let err = parse_prompt_file(Path::new("/tmp/x.md"), "---\ndescription: d\n---\nBody\n")
            .unwrap_err();
        assert!(matches!(err, PromptError::Frontmatter { .. }));
    }

    #[test]
    fn parse_rejects_no_frontmatter() {
        let err = parse_prompt_file(Path::new("/tmp/x.md"), "just a body\n").unwrap_err();
        assert!(matches!(err, PromptError::Frontmatter { .. }));
    }

    #[test]
    fn parses_crlf_frontmatter() {
        // The parser claims `\r\n` support; pin it. The body is reconstructed
        // line-by-line, so CRLF normalizes to LF in the rendered output —
        // harmless for prompt text, but exercised here so the behavior is fixed.
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "crlf.md",
            "---\r\nname: crlf\r\ndescription: d\r\narguments:\r\n  - name: who\r\n    required: true\r\n---\r\nHi {{ who }}\r\nbye\r\n",
        );
        let provider = LocalProvider::new(vec![dir.path().to_path_buf()]);

        let prompts = provider.list_sync();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].name, "crlf");
        assert!(prompts[0].arguments[0].required);

        // CRLF is normalized to LF in the body; MiniJinja trims the single
        // trailing newline (Jinja2 default). The point of the assertion is that
        // no `\r` survives into the rendered output.
        let out = provider
            .render_sync("crlf", &args(&[("who", "Ada")]))
            .unwrap();
        assert_eq!(out, "Hi Ada\nbye");
        assert!(!out.contains('\r'));
    }

    #[test]
    fn non_md_files_are_ignored() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "notes.txt",
            "---\nname: notes\ndescription: d\n---\nB\n",
        );
        write(dir.path(), "p.md", "---\nname: p\ndescription: d\n---\nB\n");
        let provider = LocalProvider::new(vec![dir.path().to_path_buf()]);
        let prompts = provider.list_sync();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].name, "p");
    }

    #[test]
    fn missing_directory_contributes_nothing() {
        let provider = LocalProvider::new(vec![PathBuf::from("/no/such/dir")]);
        assert!(provider.list_sync().is_empty());
    }
}
