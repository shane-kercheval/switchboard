//! Invocation-time validation (`docs/workflow-spec.md` §Validation
//! "Invocation-time"): given the parsed workflow, the user's supplied input
//! values, the project's agent roster, and a prompt-resolution predicate, reject
//! missing required inputs, type mismatches, non-existent agents, unresolvable
//! prompt ids, and empty/duplicate `[agent]` lists — before any dispatch.
//!
//! The pure crate cannot call `PromptService`, so prompt resolution is injected
//! as a predicate; agent existence is checked against an injected roster. Both
//! comparisons normalize names (hyphen→underscore, lowercased) per the project's
//! uniqueness rule.
//!
//! **Not checked here (render-time concern):** that every template *variable*
//! reference resolves in scope. That depends on the runtime scope as it evolves
//! step to step (`user_input` after a pause, iteration vars, helper outputs), so
//! it surfaces at step render under strict-undefined — see the crate doc.

use std::collections::{BTreeMap, HashSet};

use switchboard_core::name::canonicalize_for_uniqueness;

use crate::error::{Result, WorkflowError};
use crate::model::{InputType, Workflow};
use crate::template::ScopeValue;

/// A value the user supplied for a declared input at invocation. A scalar input
/// (`agent` / `prompt_id` / `text`) takes [`InputValue::Text`]; a list input
/// (`[agent]` / `[text]`) takes [`InputValue::List`].
#[derive(Debug, Clone, PartialEq)]
pub enum InputValue {
    Text(String),
    List(Vec<String>),
}

/// Validate the user's supplied inputs against the workflow's declarations and
/// the project context. `agents` is the project roster (agent names);
/// `prompt_resolves` answers whether a `prompt_id` resolves through the
/// configured providers.
pub fn validate_invocation(
    workflow: &Workflow,
    supplied: &BTreeMap<String, InputValue>,
    agents: &[String],
    prompt_resolves: impl Fn(&str) -> bool,
) -> Result<()> {
    let declared: HashSet<&str> = workflow.inputs.iter().map(|i| i.name.as_str()).collect();
    for name in supplied.keys() {
        if !declared.contains(name.as_str()) {
            return Err(WorkflowError::invocation(format!(
                "supplied input {name:?} is not declared by this workflow"
            )));
        }
    }

    let roster: HashSet<String> = agents
        .iter()
        .map(|a| canonicalize_for_uniqueness(a))
        .collect();

    for input in &workflow.inputs {
        let Some(value) = supplied.get(&input.name) else {
            if !input.optional {
                return Err(WorkflowError::invocation(format!(
                    "required input {:?} was not supplied",
                    input.name
                )));
            }
            continue;
        };
        validate_value(
            &input.name,
            input.ty,
            input.optional,
            value,
            &roster,
            &prompt_resolves,
        )?;
    }
    Ok(())
}

/// Validate the supplied inputs and return the bound input scope — a
/// [`ScopeValue`] for **every** declared input, with defaults applied to omitted
/// optional inputs. This is the value M4 drops into [`crate::Scope::inputs`].
///
/// Default application is language semantics, so it lives here rather than being
/// reimplemented in the app: an omitted optional input (only `text` can be
/// optional in v1 — see the parser) binds to its declared default, so a later
/// strict-undefined render of `{{ that_input }}` resolves to the default instead
/// of failing. Validation runs first, so a returned map is always complete and
/// type-consistent.
pub fn bind_invocation(
    workflow: &Workflow,
    supplied: &BTreeMap<String, InputValue>,
    agents: &[String],
    prompt_resolves: impl Fn(&str) -> bool,
) -> Result<BTreeMap<String, ScopeValue>> {
    validate_invocation(workflow, supplied, agents, &prompt_resolves)?;

    let mut bound = BTreeMap::new();
    for input in &workflow.inputs {
        let value = match supplied.get(&input.name) {
            Some(InputValue::Text(s)) => ScopeValue::Text(s.clone()),
            Some(InputValue::List(items)) => ScopeValue::List(items.clone()),
            // Omitted: validation guaranteed this input is optional, and only
            // `text` can be optional, so its default is a string (empty for the
            // `text?` shorthand).
            None => ScopeValue::Text(input.default.clone().unwrap_or_default()),
        };
        bound.insert(input.name.clone(), value);
    }
    Ok(bound)
}

fn validate_value(
    name: &str,
    ty: InputType,
    optional: bool,
    value: &InputValue,
    roster: &HashSet<String>,
    prompt_resolves: &impl Fn(&str) -> bool,
) -> Result<()> {
    // Shape: list types need a list value; scalar types need a scalar.
    match (ty.is_list(), value) {
        (true, InputValue::List(_)) | (false, InputValue::Text(_)) => {}
        (true, InputValue::Text(_)) => {
            return Err(WorkflowError::invocation(format!(
                "input {name:?} expects a list of values"
            )));
        }
        (false, InputValue::List(_)) => {
            return Err(WorkflowError::invocation(format!(
                "input {name:?} expects a single value, not a list"
            )));
        }
    }

    match (ty, value) {
        (InputType::Agent, InputValue::Text(v)) => {
            require_non_empty(name, optional, v)?;
            require_agent_exists(name, v, roster)?;
        }
        (InputType::AgentList, InputValue::List(items)) => {
            // `[agent]` lists are never validly empty (the empty-fan-in rule),
            // regardless of optionality.
            validate_agent_list(items, roster).map_err(|e| match e {
                WorkflowError::Invocation { message } => {
                    WorkflowError::invocation(format!("input {name:?}: {message}"))
                }
                other => other,
            })?;
        }
        (InputType::PromptId, InputValue::Text(v)) => {
            require_non_empty(name, optional, v)?;
            if !prompt_resolves(v) {
                return Err(WorkflowError::invocation(format!(
                    "input {name:?}: prompt id {v:?} does not resolve through configured providers"
                )));
            }
        }
        (InputType::Text, InputValue::Text(v)) => require_non_empty(name, optional, v)?,
        // `[text]` lists may be empty (used by `for_each`); no per-item checks.
        (InputType::TextList, InputValue::List(_)) => {}
        _ => unreachable!("shape validated above"),
    }
    Ok(())
}

fn require_non_empty(name: &str, optional: bool, value: &str) -> Result<()> {
    if !optional && value.trim().is_empty() {
        return Err(WorkflowError::invocation(format!(
            "required input {name:?} must not be blank"
        )));
    }
    Ok(())
}

fn require_agent_exists(name: &str, agent: &str, roster: &HashSet<String>) -> Result<()> {
    if roster.contains(&canonicalize_for_uniqueness(agent.trim())) {
        Ok(())
    } else {
        Err(WorkflowError::invocation(format!(
            "input {name:?}: agent {agent:?} does not exist in this project"
        )))
    }
}

/// Validate a resolved `[agent]` list: non-empty, free of duplicates (after
/// normalization), and every member exists in the roster. Exposed so the M4
/// interpreter applies the identical rule to lists it resolves from templates at
/// dispatch (the spec applies this to every agent list used as a target, sync
/// argument, forward source, or helper argument).
///
/// `roster` keys are pre-normalized canonical names. The `implicit_hasher` allow
/// keeps the signature a plain `HashSet<String>` — callers build it with the
/// default hasher and a generic hasher param would be noise here.
#[allow(clippy::implicit_hasher)]
pub fn validate_agent_list(items: &[String], roster: &HashSet<String>) -> Result<()> {
    if items.is_empty() {
        return Err(WorkflowError::invocation(
            "agent list must not be empty".to_owned(),
        ));
    }
    let mut seen = HashSet::new();
    for item in items {
        let key = canonicalize_for_uniqueness(item.trim());
        if !seen.insert(key.clone()) {
            return Err(WorkflowError::invocation(format!(
                "duplicate agent reference {item:?}"
            )));
        }
        if !roster.contains(&key) {
            return Err(WorkflowError::invocation(format!(
                "agent {item:?} does not exist in this project"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_workflow;

    fn workflow(inputs_yaml: &str) -> Workflow {
        let yaml = format!(
            "name: wf\ndescription: d\ninputs:\n{inputs_yaml}steps:\n  - send: {{to: a, text: t}}\n"
        );
        parse_workflow("wf", &yaml).expect("fixture workflow parses")
    }

    fn agents() -> Vec<String> {
        vec![
            "primary".to_owned(),
            "reviewer-1".to_owned(),
            "reviewer-2".to_owned(),
        ]
    }

    fn supply(pairs: Vec<(&str, InputValue)>) -> BTreeMap<String, InputValue> {
        pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
    }

    fn text(s: &str) -> InputValue {
        InputValue::Text(s.to_owned())
    }

    fn list(items: &[&str]) -> InputValue {
        InputValue::List(items.iter().map(|s| (*s).to_owned()).collect())
    }

    fn always(_: &str) -> bool {
        true
    }

    #[test]
    fn missing_required_input_is_rejected() {
        let wf = workflow("  goal: text\n");
        let err = validate_invocation(&wf, &BTreeMap::new(), &agents(), always).unwrap_err();
        assert!(err.to_string().contains("required input"), "got: {err}");
    }

    #[test]
    fn optional_text_input_may_be_omitted() {
        let wf = workflow("  ctx: text?\n");
        validate_invocation(&wf, &BTreeMap::new(), &agents(), always).unwrap();
    }

    #[test]
    fn required_text_must_not_be_blank() {
        let wf = workflow("  goal: text\n");
        let err = validate_invocation(&wf, &supply(vec![("goal", text("   "))]), &agents(), always)
            .unwrap_err();
        assert!(err.to_string().contains("blank"), "got: {err}");
    }

    #[test]
    fn agent_input_existence_is_checked_with_normalization() {
        let wf = workflow("  a: agent\n");
        // "Reviewer-1" normalizes to the same key as roster "reviewer-1".
        validate_invocation(
            &wf,
            &supply(vec![("a", text("Reviewer-1"))]),
            &agents(),
            always,
        )
        .unwrap();
        let err = validate_invocation(&wf, &supply(vec![("a", text("ghost"))]), &agents(), always)
            .unwrap_err();
        assert!(err.to_string().contains("does not exist"), "got: {err}");
    }

    #[test]
    fn agent_list_empty_duplicate_and_missing_are_rejected() {
        let wf = workflow("  rs: [agent]\n");
        assert!(
            validate_invocation(&wf, &supply(vec![("rs", list(&[]))]), &agents(), always).is_err()
        );
        // reviewer-1 and reviewer_1 normalize equal → duplicate.
        assert!(
            validate_invocation(
                &wf,
                &supply(vec![("rs", list(&["reviewer-1", "reviewer_1"]))]),
                &agents(),
                always
            )
            .unwrap_err()
            .to_string()
            .contains("duplicate")
        );
        assert!(
            validate_invocation(
                &wf,
                &supply(vec![("rs", list(&["ghost"]))]),
                &agents(),
                always
            )
            .unwrap_err()
            .to_string()
            .contains("does not exist")
        );
        validate_invocation(
            &wf,
            &supply(vec![("rs", list(&["reviewer-1", "reviewer-2"]))]),
            &agents(),
            always,
        )
        .unwrap();
    }

    #[test]
    fn prompt_id_must_resolve() {
        let wf = workflow("  p: prompt_id\n");
        let err = validate_invocation(
            &wf,
            &supply(vec![("p", text("local:nope"))]),
            &agents(),
            |_| false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("does not resolve"), "got: {err}");
        validate_invocation(
            &wf,
            &supply(vec![("p", text("local:ok"))]),
            &agents(),
            |id| id == "local:ok",
        )
        .unwrap();
    }

    #[test]
    fn shape_mismatch_is_rejected() {
        let scalar = workflow("  a: agent\n");
        assert!(
            validate_invocation(
                &scalar,
                &supply(vec![("a", list(&["x"]))]),
                &agents(),
                always
            )
            .unwrap_err()
            .to_string()
            .contains("not a list")
        );
        let listed = workflow("  rs: [agent]\n");
        assert!(
            validate_invocation(&listed, &supply(vec![("rs", text("x"))]), &agents(), always)
                .unwrap_err()
                .to_string()
                .contains("expects a list")
        );
    }

    #[test]
    fn unknown_supplied_input_is_rejected() {
        let wf = workflow("  goal: text\n");
        let err = validate_invocation(
            &wf,
            &supply(vec![("goal", text("g")), ("bogus", text("x"))]),
            &agents(),
            always,
        )
        .unwrap_err();
        assert!(err.to_string().contains("not declared"), "got: {err}");
    }

    #[test]
    fn empty_text_list_is_allowed() {
        // [text] (unlike [agent]) may be empty — it feeds `for_each`.
        let wf = workflow("  ms: [text]\n");
        validate_invocation(&wf, &supply(vec![("ms", list(&[]))]), &agents(), always).unwrap();
    }

    #[test]
    fn bind_applies_text_default_for_omitted_optional() {
        // The footgun this guards: omit `ctx` (text?), and without binding its
        // default a later `{{ ctx }}` render would fail strict-undefined.
        let wf = workflow("  goal: text\n  ctx: text?\n");
        let bound =
            bind_invocation(&wf, &supply(vec![("goal", text("g"))]), &agents(), always).unwrap();
        assert_eq!(bound.get("goal"), Some(&ScopeValue::Text("g".to_owned())));
        assert_eq!(bound.get("ctx"), Some(&ScopeValue::Text(String::new())));
    }

    #[test]
    fn bind_uses_explicit_long_form_default() {
        let yaml = "name: wf\ndescription: d\ninputs:\n  ctx:\n    type: text\n    default: fallback\nsteps:\n  - send: {to: a, text: t}\n";
        let wf = parse_workflow("wf", yaml).unwrap();
        let bound = bind_invocation(&wf, &BTreeMap::new(), &agents(), always).unwrap();
        assert_eq!(
            bound.get("ctx"),
            Some(&ScopeValue::Text("fallback".to_owned()))
        );
    }

    #[test]
    fn bind_passes_supplied_values_through_and_rejects_invalid() {
        let wf = workflow("  a: agent\n  rs: [agent]\n");
        let bound = bind_invocation(
            &wf,
            &supply(vec![("a", text("primary")), ("rs", list(&["reviewer-1"]))]),
            &agents(),
            always,
        )
        .unwrap();
        assert_eq!(
            bound.get("a"),
            Some(&ScopeValue::Text("primary".to_owned()))
        );
        assert_eq!(
            bound.get("rs"),
            Some(&ScopeValue::List(vec!["reviewer-1".to_owned()]))
        );
        // bind validates first: a bad agent still errors rather than binding.
        assert!(
            bind_invocation(
                &wf,
                &supply(vec![("a", text("ghost")), ("rs", list(&["reviewer-1"]))]),
                &agents(),
                always
            )
            .is_err()
        );
    }

    #[test]
    fn validate_agent_list_standalone() {
        let roster: HashSet<String> = agents()
            .iter()
            .map(|a| canonicalize_for_uniqueness(a))
            .collect();
        assert!(validate_agent_list(&[], &roster).is_err());
        assert!(
            validate_agent_list(&["reviewer-1".to_owned(), "reviewer-1".to_owned()], &roster)
                .is_err()
        );
        assert!(validate_agent_list(&["ghost".to_owned()], &roster).is_err());
        validate_agent_list(&["primary".to_owned(), "reviewer-2".to_owned()], &roster).unwrap();
    }
}
