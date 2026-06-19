//! The workflow templating engine: a **`MiniJinja` subset** per
//! `docs/workflow-spec.md` §Templating, plus the four built-in helper functions.
//!
//! Two distinct enforcement mechanisms, deliberately not conflated:
//!
//! 1. **Undefined variables → render error.** The environment uses
//!    *strict*-undefined (unlike `crates/prompts`, which is lenient so a missing
//!    optional prompt arg renders empty). A workflow variable that resolves in no
//!    scope is an error, never a silent empty string.
//! 2. **Unsupported tags/filters → parse-time rejection.** The forbidden features
//!    (`{% set %}`, `{% raw %}`, macros, inheritance, includes, the `do` tag, and
//!    any filter outside the spec's six) are *valid `MiniJinja` syntax* under the
//!    crate's compiled feature set, so strict-undefined does nothing for them.
//!    They are caught by an explicit [`enforce_subset`] scan. `MiniJinja`'s own
//!    feature gating can't do this: `crates/prompts` enables `macros` /
//!    `multi_template`, and Cargo feature unification is additive across the
//!    workspace, so those tags compile in regardless of this crate's manifest.
//!
//! The scan is a small block-aware lexer rather than `MiniJinja`'s AST machinery:
//! the AST API is gated behind the explicitly-unstable `unstable_machinery`
//! feature, and coupling a load-bearing validation to it across caret-ranged
//! version bumps is the larger risk. The scan is pure, stable, and bounded to a
//! fixed allowlist of tags and filters; it tracks `{# #}` comments, `{{ }}` /
//! `{% %}` regions, and string literals so a forbidden keyword appearing inside a
//! comment or a string literal is **not** a false positive. `MiniJinja`'s `compile`
//! runs on top to catch genuine syntax errors. The scan covers tags and filters
//! (what authors reach for); operator/test-level subset deviations are left to
//! surface as render differences rather than growing the scanner into a general
//! Jinja linter.

use std::collections::BTreeMap;
use std::sync::Arc;

use minijinja::value::{Value, ValueKind};
use minijinja::{Environment, Error as MjError, ErrorKind, UndefinedBehavior};
use switchboard_core::name::canonicalize_for_uniqueness;

use crate::checkpoint::OutputScope;
use crate::error::{Result, WorkflowError};

/// Control-structure tags the workflow subset permits. Any other tag keyword
/// (`set`, `macro`, `block`, `include`, `raw`, …) is rejected — an allowlist, so
/// a future `MiniJinja` tag is excluded by default, matching the "subset only"
/// posture.
const ALLOWED_TAGS: &[&str] = &["for", "endfor", "if", "elif", "else", "endif"];

/// Filters the workflow subset permits (per the spec). A filter outside this set
/// — including real `MiniJinja` built-ins like `reverse` / `first` that would
/// render fine locally — breaks the cross-engine portability the subset exists
/// to guarantee, so it is rejected at parse time rather than left to a
/// render-time "unknown filter" that only fires if the branch is hit.
const ALLOWED_FILTERS: &[&str] = &["length", "lower", "upper", "default", "join", "trim"];

/// A variable value in a render scope: a scalar string, or a list (for
/// `[agent]` / `[text]` inputs, so `{% for %}` and the agent helpers see a
/// sequence).
#[derive(Debug, Clone, PartialEq)]
pub enum ScopeValue {
    Text(String),
    List(Vec<String>),
}

impl From<&ScopeValue> for Value {
    fn from(value: &ScopeValue) -> Self {
        match value {
            ScopeValue::Text(s) => Value::from(s.clone()),
            ScopeValue::List(items) => Value::from(items.clone()),
        }
    }
}

/// The variables visible to one template render, layered by the spec's
/// precedence (innermost wins): `template_vars` → iteration var → `user_input` →
/// workflow inputs. A name absent from every layer is a render error under
/// strict-undefined.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    pub inputs: BTreeMap<String, ScopeValue>,
    pub user_input: Option<String>,
    pub iteration: Option<(String, String)>,
    pub template_vars: BTreeMap<String, ScopeValue>,
}

impl Scope {
    /// Build the merged `MiniJinja` context, applying precedence by inserting from
    /// lowest to highest so an inner scope overwrites an outer one.
    fn into_context(self) -> Value {
        let mut ctx: BTreeMap<String, Value> = BTreeMap::new();
        for (k, v) in &self.inputs {
            ctx.insert(k.clone(), Value::from(v));
        }
        if let Some(input) = self.user_input {
            ctx.insert("user_input".to_owned(), Value::from(input));
        }
        if let Some((var, value)) = self.iteration {
            ctx.insert(var, Value::from(value));
        }
        for (k, v) in &self.template_vars {
            ctx.insert(k.clone(), Value::from(v));
        }
        ctx.into_iter().collect()
    }
}

/// Validate a single template string at **parse time**: enforce the subset, then
/// compile it to catch syntax errors. Variable *resolution* is not checked here
/// (that is a render-time concern — see the crate doc's known-limitation note).
pub fn validate_template(field: &str, src: &str) -> Result<()> {
    enforce_subset(src).map_err(|m| WorkflowError::template(field, m))?;
    let env = Environment::new();
    env.template_from_str(src)
        .map_err(|e| WorkflowError::template(field, e.to_string()))?;
    Ok(())
}

/// Render a template against a scope and the per-run output scope (which the
/// helper functions read). Undefined variables and helper failures surface as
/// [`WorkflowError::Render`].
pub fn render(src: &str, scope: Scope, outputs: &OutputScope) -> Result<String> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    // Expose `.items()` on maps (the form `responses_from(...)` callers use, per
    // the spec's worked example 3) by delegating to MiniJinja's built-in `items`
    // filter. The filter itself stays out of the author-facing subset (the scan
    // blocks `| items`); this only wires the *method* call the spec documents.
    env.set_unknown_method_callback(|state, value, method, args| {
        if value.kind() == ValueKind::Map && method == "items" {
            if !args.is_empty() {
                return Err(MjError::new(
                    ErrorKind::InvalidOperation,
                    "items() takes no arguments",
                ));
            }
            state.apply_filter("items", std::slice::from_ref(value))
        } else {
            Err(MjError::from(ErrorKind::UnknownMethod))
        }
    });
    register_helpers(&mut env, &Arc::new(outputs.clone()));
    let ctx = scope.into_context();
    env.render_str(src, ctx)
        .map_err(|e| WorkflowError::render(render_error_message(&e)))
}

/// `MiniJinja` nests the useful detail (the helper's message, the undefined name)
/// in the error's source chain; flatten it into one line so the surfaced message
/// is actionable rather than a bare "invalid operation".
fn render_error_message(err: &MjError) -> String {
    let mut msg = err.to_string();
    let mut source = std::error::Error::source(err);
    while let Some(e) = source {
        msg.push_str(": ");
        msg.push_str(&e.to_string());
        source = e.source();
    }
    msg
}

fn register_helpers(env: &mut Environment<'_>, outputs: &Arc<OutputScope>) {
    let scope = outputs.clone();
    env.add_function("responses_from", move |agents: Value| {
        let names = coerce_agents(&agents)?;
        let mut entries: Vec<(String, Value)> = Vec::with_capacity(names.len());
        for name in &names {
            let text = lookup(&scope, name)?;
            entries.push((canonicalize_for_uniqueness(name), Value::from(text)));
        }
        Ok::<Value, MjError>(entries.into_iter().collect())
    });

    let scope = outputs.clone();
    env.add_function("aggregated_responses", move |agents: Value| {
        let names = coerce_agents(&agents)?;
        let mut blocks: Vec<String> = Vec::with_capacity(names.len());
        for name in &names {
            let text = lookup(&scope, name)?;
            blocks.push(format!(
                "=== START response from {name} ===\n{text}\n=== END response from {name} ==="
            ));
        }
        Ok::<Value, MjError>(Value::from(blocks.join("\n\n")))
    });

    let scope = outputs.clone();
    env.add_function("last_output", move |agent: Value| {
        let name = agent.as_str().ok_or_else(|| {
            MjError::new(
                ErrorKind::InvalidOperation,
                "last_output expects a single agent name",
            )
        })?;
        Ok::<Value, MjError>(Value::from(lookup(&scope, name)?))
    });

    env.add_function("agent_names", move |agents: Value| {
        let names = coerce_agents(&agents)?;
        Ok::<Value, MjError>(Value::from(names))
    });
}

/// Look up an agent's resolved output text in the per-run output scope, keyed by
/// the canonical (hyphen→underscore, lowercased) name so it matches how the
/// runtime stores it and the project's agent-name uniqueness rule. A miss is the
/// spec's "no completed output yet from this workflow run" error.
fn lookup(outputs: &OutputScope, name: &str) -> std::result::Result<String, MjError> {
    let key = canonicalize_for_uniqueness(name.trim());
    outputs.get(&key).cloned().ok_or_else(|| {
        MjError::new(
            ErrorKind::InvalidOperation,
            format!(
                "no completed output for agent `{}` in this workflow run",
                name.trim()
            ),
        )
    })
}

/// Coerce a helper's `agents` argument — a single agent name or a list of them —
/// into a `Vec<String>`, trimming each. Per the spec, a single reference is
/// treated as a one-element list.
fn coerce_agents(value: &Value) -> std::result::Result<Vec<String>, MjError> {
    if let Some(s) = value.as_str() {
        return Ok(vec![s.trim().to_owned()]);
    }
    match value.kind() {
        ValueKind::Seq | ValueKind::Iterable => {
            let mut out = Vec::new();
            for item in value.try_iter()? {
                let s = item.as_str().ok_or_else(|| {
                    MjError::new(
                        ErrorKind::InvalidOperation,
                        "agent references must be strings",
                    )
                })?;
                out.push(s.trim().to_owned());
            }
            Ok(out)
        }
        _ => Err(MjError::new(
            ErrorKind::InvalidOperation,
            "expected an agent name or a list of agent names",
        )),
    }
}

/// The block-aware subset scan. Returns `Err(message)` naming the first
/// offending tag or filter; the caller wraps it with the field context.
fn enforce_subset(src: &str) -> std::result::Result<(), String> {
    let chars: Vec<char> = src.chars().collect();
    let n = chars.len();
    let mut i = 0;
    while i < n {
        if chars[i] == '{' && i + 1 < n {
            match chars[i + 1] {
                '#' => i = skip_comment(&chars, i + 2),
                '{' => i = scan_region(&chars, i + 2, Region::Expr)?,
                '%' => i = scan_block(&chars, i + 2)?,
                _ => i += 1,
            }
        } else {
            i += 1;
        }
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq)]
enum Region {
    Expr,
    Block,
}

/// Scan a `{% ... %}` block: validate the leading tag keyword, then scan the rest
/// to `%}` (checking filters, skipping strings).
fn scan_block(chars: &[char], start: usize) -> std::result::Result<usize, String> {
    let mut i = start;
    // Optional whitespace-control dash, then whitespace, then the tag keyword.
    if i < chars.len() && chars[i] == '-' {
        i += 1;
    }
    i = skip_ws(chars, i);
    let (keyword, after) = read_ident(chars, i);
    if !keyword.is_empty() && !ALLOWED_TAGS.contains(&keyword.as_str()) {
        return Err(format!(
            "unsupported tag `{{% {keyword} %}}`; the workflow template subset allows only `for` and `if` blocks"
        ));
    }
    scan_region(chars, after, Region::Block)
}

/// Scan an expression / block body to its closing delimiter, skipping string
/// literals and validating each filter operator against the allowlist.
fn scan_region(chars: &[char], start: usize, region: Region) -> std::result::Result<usize, String> {
    let n = chars.len();
    let mut i = start;
    while i < n {
        let c = chars[i];
        match c {
            '\'' | '"' => i = skip_string(chars, i),
            '%' if region == Region::Block && i + 1 < n && chars[i + 1] == '}' => return Ok(i + 2),
            '}' if region == Region::Expr && i + 1 < n && chars[i + 1] == '}' => return Ok(i + 2),
            '|' => {
                let j = skip_ws(chars, i + 1);
                let (filter, after) = read_ident(chars, j);
                if !filter.is_empty() && !ALLOWED_FILTERS.contains(&filter.as_str()) {
                    return Err(format!(
                        "unsupported filter `{filter}`; the workflow template subset allows only: {}",
                        ALLOWED_FILTERS.join(", ")
                    ));
                }
                i = after.max(i + 1);
            }
            _ => i += 1,
        }
    }
    // Unterminated region: leave it to `MiniJinja`'s compile to report.
    Ok(n)
}

/// Skip a `{# ... #}` comment; returns the index just past `#}` (or end).
fn skip_comment(chars: &[char], start: usize) -> usize {
    let n = chars.len();
    let mut i = start;
    while i < n {
        if chars[i] == '#' && i + 1 < n && chars[i + 1] == '}' {
            return i + 2;
        }
        i += 1;
    }
    n
}

/// Skip a string literal starting at the opening quote; returns the index just
/// past the closing quote (or end). Honors backslash escapes.
fn skip_string(chars: &[char], start: usize) -> usize {
    let n = chars.len();
    let quote = chars[start];
    let mut i = start + 1;
    while i < n {
        match chars[i] {
            '\\' => i += 2,
            c if c == quote => return i + 1,
            _ => i += 1,
        }
    }
    n
}

fn skip_ws(chars: &[char], start: usize) -> usize {
    let mut i = start;
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    i
}

/// Read an identifier `[A-Za-z_][A-Za-z0-9_]*` from `start`; returns the
/// identifier (empty if none) and the index just past it.
fn read_ident(chars: &[char], start: usize) -> (String, usize) {
    let n = chars.len();
    if start >= n || !(chars[start].is_ascii_alphabetic() || chars[start] == '_') {
        return (String::new(), start);
    }
    let mut i = start + 1;
    while i < n && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
        i += 1;
    }
    (chars[start..i].iter().collect(), i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope_with_inputs(pairs: &[(&str, ScopeValue)]) -> Scope {
        let mut scope = Scope::default();
        for (k, v) in pairs {
            scope.inputs.insert((*k).to_owned(), v.clone());
        }
        scope
    }

    fn outputs(pairs: &[(&str, &str)]) -> OutputScope {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn allowed_tags_and_filters_pass() {
        for src in [
            "{{ name }}",
            "{% if x %}a{% elif y %}b{% else %}c{% endif %}",
            "{% for r in list %}{{ loop.index }}:{{ r }}{% endfor %}",
            "{{ name | upper }}",
            "{{ items | join(', ') | trim }}",
            "{{ maybe | default('none') }}",
            "{{ list | length }}",
            "{# a comment #}{{ x }}",
            "{{- x -}}",
        ] {
            assert!(validate_template("f", src).is_ok(), "should accept: {src}");
        }
    }

    #[test]
    fn unsupported_tags_are_rejected() {
        // Each forbidden tag is caught by the explicit subset pass (none of these
        // self-reject — they are valid MiniJinja under the compiled features).
        for (src, tag) in [
            ("{% set x = 1 %}", "set"),
            ("{% raw %}{{ x }}{% endraw %}", "raw"),
            ("{% macro m() %}{% endmacro %}", "macro"),
            ("{% extends 'base' %}", "extends"),
            ("{% block body %}{% endblock %}", "block"),
            ("{% include 'other' %}", "include"),
            ("{% filter upper %}x{% endfilter %}", "filter"),
            ("{% do thing() %}", "do"),
            ("{% with x = 1 %}{% endwith %}", "with"),
        ] {
            let err = validate_template("f", src).unwrap_err();
            let msg = err.to_string();
            assert!(
                matches!(err, WorkflowError::Template { .. }) && msg.contains(tag),
                "tag {tag} should be rejected, got: {msg}"
            );
        }
    }

    #[test]
    fn unsupported_filter_is_rejected() {
        // `reverse` is a real MiniJinja builtin (would render fine) but is outside
        // the portable subset, so the parse-time allowlist must reject it.
        let err = validate_template("f", "{{ items | reverse }}").unwrap_err();
        assert!(
            matches!(&err, WorkflowError::Template { message, .. } if message.contains("reverse")),
            "got: {err}"
        );
        // Chained: an allowed filter followed by a forbidden one is still caught.
        assert!(validate_template("f", "{{ x | trim | first }}").is_err());
    }

    #[test]
    fn forbidden_keyword_in_string_literal_is_not_a_false_positive() {
        // The literal text `{% set %}` inside a string is content, not a tag.
        assert!(validate_template("f", r#"{{ "use {% set %} carefully" }}"#).is_ok());
        // A filter-looking pipe inside a string is also not a filter.
        assert!(validate_template("f", r#"{{ "a | reverse b" }}"#).is_ok());
        // Inside a block's string argument.
        assert!(validate_template("f", r#"{% if x == "{% macro %}" %}y{% endif %}"#).is_ok());
    }

    #[test]
    fn forbidden_keyword_in_comment_is_not_a_false_positive() {
        assert!(
            validate_template("f", "{# you could use {% set %} or | reverse #}{{ x }}").is_ok()
        );
    }

    #[test]
    fn standard_expression_operators_are_accepted() {
        // Per the spec amendment: v1 parse-time enforcement covers tags + filters
        // only. Standard comparison/equality/boolean/arithmetic operators are
        // accepted (they render identically across Jinja engines) — they are NOT
        // rejected by the subset scan. Pinned so this isn't rediscovered as a bug.
        for src in [
            "{% if count > 3 %}big{% endif %}",
            "{% if a == b %}eq{% endif %}",
            "{% if x and not y %}z{% endif %}",
            "{{ 1 + 2 }}",
            "{{ total - 1 }}",
        ] {
            assert!(validate_template("f", src).is_ok(), "should accept: {src}");
        }
    }

    #[test]
    fn ident_prefix_does_not_trip_tag_match() {
        // `setup` starts with `set` but is a different identifier; as a variable
        // expression it is fine, and the tag matcher must read the whole token.
        assert!(validate_template("f", "{{ setup }}").is_ok());
    }

    #[test]
    fn render_resolves_inputs() {
        let scope = scope_with_inputs(&[("goal", ScopeValue::Text("ship it".to_owned()))]);
        let out = render("Plan to: {{ goal }}", scope, &OutputScope::new()).unwrap();
        assert_eq!(out, "Plan to: ship it");
    }

    #[test]
    fn scope_precedence_uses_innermost_binding() {
        // Same name `x` in all four layers; innermost (template_vars) must win,
        // then iteration, then user_input, then inputs.
        let base = || {
            let mut s = Scope::default();
            s.inputs
                .insert("x".to_owned(), ScopeValue::Text("input".to_owned()));
            s
        };

        let s = base();
        assert_eq!(render("{{ x }}", s, &OutputScope::new()).unwrap(), "input");

        // Iteration var beats the input binding of the same name.
        let mut s2 = base();
        s2.iteration = Some(("x".to_owned(), "iter".to_owned()));
        assert_eq!(render("{{ x }}", s2, &OutputScope::new()).unwrap(), "iter");

        let mut s3 = base();
        s3.iteration = Some(("x".to_owned(), "iter".to_owned()));
        s3.template_vars
            .insert("x".to_owned(), ScopeValue::Text("tvar".to_owned()));
        assert_eq!(render("{{ x }}", s3, &OutputScope::new()).unwrap(), "tvar");

        // sanity: user_input keeps its dedicated name
        let mut s4 = base();
        s4.user_input = Some("hi".to_owned());
        assert_eq!(
            render("{{ user_input }}", s4, &OutputScope::new()).unwrap(),
            "hi"
        );
    }

    #[test]
    fn undefined_variable_is_a_render_error() {
        let err = render("{{ nope }}", Scope::default(), &OutputScope::new()).unwrap_err();
        assert!(matches!(err, WorkflowError::Render { .. }), "got: {err}");
    }

    #[test]
    fn user_input_before_pause_is_a_render_error() {
        // No `user_input` bound in scope → strict-undefined errors.
        let err = render("{{ user_input }}", Scope::default(), &OutputScope::new()).unwrap_err();
        assert!(matches!(err, WorkflowError::Render { .. }));
    }

    #[test]
    fn aggregated_responses_canonical_shape_in_declared_order() {
        // Deliberately non-alphabetical order to catch a sorted-map regression.
        let out_scope = outputs(&[("zeta", "Z text"), ("alpha", "A text")]);
        let mut scope = Scope::default();
        scope.inputs.insert(
            "reviewers".to_owned(),
            ScopeValue::List(vec!["zeta".to_owned(), "alpha".to_owned()]),
        );
        let out = render("{{ aggregated_responses(reviewers) }}", scope, &out_scope).unwrap();
        let expected = "=== START response from zeta ===\nZ text\n=== END response from zeta ===\n\n\
                        === START response from alpha ===\nA text\n=== END response from alpha ===";
        assert_eq!(out, expected);
    }

    #[test]
    fn responses_from_preserves_order_and_normalizes_keys() {
        let out_scope = outputs(&[("reviewer_1", "first"), ("reviewer_2", "second")]);
        let mut scope = Scope::default();
        scope.inputs.insert(
            "reviewers".to_owned(),
            // hyphenated names, non-alphabetical (2 before 1)
            ScopeValue::List(vec!["reviewer-2".to_owned(), "reviewer-1".to_owned()]),
        );
        // The returned map iterates in the input list order and keys are
        // hyphen→underscore normalized, so `.items()` yields reviewer_2 first.
        let out = render(
            "{% for name, body in responses_from(reviewers).items() %}{{ name }}={{ body }};{% endfor %}",
            scope,
            &out_scope,
        )
        .unwrap();
        assert_eq!(out, "reviewer_2=second;reviewer_1=first;");
    }

    #[test]
    fn last_output_resolves_single_agent() {
        let out_scope = outputs(&[("planner", "the plan")]);
        let mut scope = Scope::default();
        scope
            .inputs
            .insert("planner".to_owned(), ScopeValue::Text("planner".to_owned()));
        let out = render("{{ last_output(planner) }}", scope, &out_scope).unwrap();
        assert_eq!(out, "the plan");
    }

    #[test]
    fn helper_errors_when_agent_has_no_output() {
        let mut scope = Scope::default();
        scope
            .inputs
            .insert("planner".to_owned(), ScopeValue::Text("planner".to_owned()));
        let err = render("{{ last_output(planner) }}", scope, &OutputScope::new()).unwrap_err();
        assert!(
            matches!(&err, WorkflowError::Render { message } if message.contains("no completed output") && message.contains("planner")),
            "got: {err}"
        );
    }

    #[test]
    fn agent_names_maps_references_to_names() {
        let mut scope = Scope::default();
        scope.inputs.insert(
            "reviewers".to_owned(),
            ScopeValue::List(vec!["a".to_owned(), "b".to_owned()]),
        );
        let out = render(
            "{{ agent_names(reviewers) | join(',') }}",
            scope,
            &OutputScope::new(),
        )
        .unwrap();
        assert_eq!(out, "a,b");
    }

    #[test]
    fn single_agent_reference_is_treated_as_one_element_list() {
        let out_scope = outputs(&[("solo", "only")]);
        let mut scope = Scope::default();
        scope
            .inputs
            .insert("solo".to_owned(), ScopeValue::Text("solo".to_owned()));
        let out = render("{{ aggregated_responses(solo) }}", scope, &out_scope).unwrap();
        assert_eq!(
            out,
            "=== START response from solo ===\nonly\n=== END response from solo ==="
        );
    }
}
