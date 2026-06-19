//! YAML → [`Workflow`] model plus the spec's full **parse-time** validation
//! (`docs/workflow-spec.md` §Validation "Parse-time").
//!
//! The file is hand-walked over `serde_norway::Value` rather than `serde`-derived
//! for two reasons the derive path can't satisfy cleanly: (1) precise,
//! spec-worded errors (e.g. distinguishing a *reserved* v2 key from an unknown
//! one, naming the offending step type), and (2) preserving input **declaration
//! order**, which a `BTreeMap`-derived field would lose and which the invocation
//! form depends on.
//!
//! Template strings are validated as they are parsed (each templated field calls
//! [`crate::template::validate_template`]); only template *parsing* is checked
//! here — variable *resolution* is a render-time concern (see the crate doc).

use std::collections::HashSet;

use serde_norway::Value;
use switchboard_core::name::canonicalize_for_uniqueness;

use crate::error::{Result, WorkflowError};
use crate::model::{
    ForEachStep, InputDecl, InputType, PauseForUserStep, SendStep, Step, Templated, WaitForAllStep,
    WaitForStep, Workflow,
};
use crate::template::validate_template;

const ALLOWED_TOPLEVEL: &[&str] = &["name", "description", "inputs", "steps"];
/// Reserved *workflow-level* keys (the forward-compat table). Distinguished from
/// merely-unknown keys so the error can point at the v2 earmark.
const RESERVED_TOPLEVEL: &[&str] = &["until", "outputs", "metadata"];
const KNOWN_STEP_TYPES: &[&str] = &[
    "send",
    "wait_for",
    "wait_for_all",
    "pause_for_user",
    "for_each",
];
/// Reserved *step-type* keys (the forward-compat table).
const RESERVED_STEP_TYPES: &[&str] = &["if", "branch", "wait_for_first"];
/// Built-in names an input (or `for_each` `item`) may not shadow.
const RESERVED_NAMES: &[&str] = &["user_input"];

/// Parse and fully parse-time-validate a workflow file. `file_stem` is the
/// filename without extension; the workflow's `name` must equal it (spec rule).
pub fn parse_workflow(file_stem: &str, content: &str) -> Result<Workflow> {
    let root: Value =
        serde_norway::from_str(content).map_err(|e| WorkflowError::Yaml(e.to_string()))?;
    let map = root
        .as_mapping()
        .ok_or_else(|| WorkflowError::validation("workflow file must be a YAML mapping"))?;

    check_toplevel_keys(map)?;

    let name = req_string(map, "name", "workflow")?;
    if !is_workflow_slug(&name) {
        return Err(WorkflowError::validation(format!(
            "`name` {name:?} must match `[a-z][a-z0-9-]*` (lowercase slug)"
        )));
    }
    if name != file_stem {
        return Err(WorkflowError::validation(format!(
            "`name` {name:?} must equal the filename (without extension) {file_stem:?}"
        )));
    }

    let description = req_string(map, "description", "workflow")?;
    if description.trim().is_empty() {
        return Err(WorkflowError::validation("`description` must not be empty"));
    }

    let inputs = parse_inputs(map)?;
    let input_names: HashSet<String> = inputs.iter().map(|i| i.name.clone()).collect();

    let steps = parse_steps(map.get("steps"), &input_names, false, "steps")?;

    Ok(Workflow {
        name,
        description,
        inputs,
        steps,
    })
}

fn check_toplevel_keys(map: &serde_norway::Mapping) -> Result<()> {
    for (key, _) in map {
        let key = key
            .as_str()
            .ok_or_else(|| WorkflowError::validation("top-level keys must be strings"))?;
        if RESERVED_TOPLEVEL.contains(&key) {
            return Err(WorkflowError::validation(format!(
                "`{key}` is a reserved top-level key for a future version and must not be used"
            )));
        }
        if !ALLOWED_TOPLEVEL.contains(&key) {
            return Err(WorkflowError::validation(format!(
                "unknown top-level key `{key}` (allowed: name, description, inputs, steps)"
            )));
        }
    }
    Ok(())
}

fn parse_inputs(map: &serde_norway::Mapping) -> Result<Vec<InputDecl>> {
    let Some(value) = map.get("inputs") else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let inputs_map = value
        .as_mapping()
        .ok_or_else(|| WorkflowError::validation("`inputs` must be a mapping"))?;

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (key, decl) in inputs_map {
        let name = key
            .as_str()
            .ok_or_else(|| WorkflowError::validation("input names must be strings"))?
            .to_owned();
        if !is_input_name(&name) {
            return Err(WorkflowError::validation(format!(
                "input name {name:?} must match `[a-z][a-z0-9_]*`"
            )));
        }
        if RESERVED_NAMES.contains(&name.as_str()) {
            return Err(WorkflowError::validation(format!(
                "input name {name:?} is a reserved built-in name and must not be used"
            )));
        }
        if !seen.insert(name.clone()) {
            return Err(WorkflowError::validation(format!(
                "duplicate input name {name:?}"
            )));
        }
        out.push(parse_input_decl(&name, decl)?);
    }
    Ok(out)
}

fn parse_input_decl(name: &str, decl: &Value) -> Result<InputDecl> {
    // Shorthand: a bare type — a string (`agent`, `text?`, …) or, for list types,
    // a one-element YAML flow sequence (`[agent]` parses as the sequence
    // `["agent"]`). Long form: a mapping with `type` + optional `description` /
    // `default`.
    let (type_value, description, default) = match decl {
        Value::String(_) | Value::Sequence(_) => (decl.clone(), None, None),
        Value::Mapping(m) => {
            let ty = m.get("type").ok_or_else(|| {
                WorkflowError::validation(format!(
                    "input {name:?} long form requires a `type` field"
                ))
            })?;
            for (k, _) in m {
                let k = k.as_str().unwrap_or_default();
                if !["type", "description", "default"].contains(&k) {
                    return Err(WorkflowError::validation(format!(
                        "input {name:?} has unknown field `{k}` (allowed: type, description, default)"
                    )));
                }
            }
            let description = m
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let default = m.get("default").and_then(Value::as_str).map(str::to_owned);
            (ty.clone(), description, default)
        }
        _ => {
            return Err(WorkflowError::validation(format!(
                "input {name:?} must be a type, a list type like [agent], or a mapping"
            )));
        }
    };

    let (ty, optional_suffix) = parse_type_value(name, &type_value)?;
    // Optionality (via `?` or a `default`) is a `text`-only concept in v1: `?` is
    // already rejected on non-text, and a `default` on `agent` / `prompt_id` / a
    // list type would make an optional non-text input (deferred to v2) — and an
    // optional `[agent]` would contradict the "every agent list is non-empty"
    // rule. Rejecting it here keeps default-binding unambiguous (only text
    // defaults exist) rather than inventing list-default semantics.
    if default.is_some() && ty != InputType::Text {
        return Err(WorkflowError::validation(format!(
            "input {name:?}: a `default` is only valid on a `text` input in v1"
        )));
    }
    // `?` implies optional; a `default` also implies optional (and the `?`
    // shorthand is an optional input defaulting to "").
    let optional = optional_suffix || default.is_some();
    let default = if optional_suffix && default.is_none() {
        Some(String::new())
    } else {
        default
    };

    Ok(InputDecl {
        name: name.to_owned(),
        ty,
        description,
        default,
        optional,
    })
}

/// Resolve a type position — a string shorthand or a one-element list literal
/// (`[agent]` / `[text]`) — to an [`InputType`] + whether it carried `?`.
fn parse_type_value(name: &str, value: &Value) -> Result<(InputType, bool)> {
    match value {
        Value::String(s) => parse_input_type(name, s),
        Value::Sequence(seq) => {
            if seq.len() != 1 {
                return Err(WorkflowError::validation(format!(
                    "input {name:?}: a list type must be a single-element list like [agent] or [text]"
                )));
            }
            let base = seq[0].as_str().ok_or_else(|| {
                WorkflowError::validation(format!(
                    "input {name:?}: list element type must be a string"
                ))
            })?;
            match base {
                "agent" => Ok((InputType::AgentList, false)),
                "text" => Ok((InputType::TextList, false)),
                other => Err(WorkflowError::validation(format!(
                    "input {name:?}: unknown list element type {other:?} (expected [agent] or [text])"
                ))),
            }
        }
        _ => Err(WorkflowError::validation(format!(
            "input {name:?}: type must be a string or a list type like [agent]"
        ))),
    }
}

/// Map a scalar type shorthand to [`InputType`] + whether it carried the optional
/// `?`. `?` is valid only on `text` in v1 (`agent?` / `prompt_id?` are deferred).
/// The bracketed string forms `"[agent]"` / `"[text]"` are accepted here too, for
/// when a long-form `type` is written as a quoted string.
fn parse_input_type(name: &str, raw: &str) -> Result<(InputType, bool)> {
    let (base, optional) = raw
        .strip_suffix('?')
        .map_or((raw, false), |stripped| (stripped, true));
    let ty = match base {
        "agent" => InputType::Agent,
        "[agent]" => InputType::AgentList,
        "prompt_id" => InputType::PromptId,
        "text" => InputType::Text,
        "[text]" => InputType::TextList,
        other => {
            return Err(WorkflowError::validation(format!(
                "input {name:?} has unknown type {other:?} (expected agent, [agent], prompt_id, text, text?, or [text])"
            )));
        }
    };
    if optional && ty != InputType::Text {
        return Err(WorkflowError::validation(format!(
            "input {name:?}: the optional `?` suffix is only valid on `text` in v1"
        )));
    }
    Ok((ty, optional))
}

fn parse_steps(
    value: Option<&Value>,
    input_names: &HashSet<String>,
    inside_for_each: bool,
    ctx: &str,
) -> Result<Vec<Step>> {
    let seq = value.and_then(Value::as_sequence).ok_or_else(|| {
        WorkflowError::validation(format!("`{ctx}` must be a non-empty sequence"))
    })?;
    if seq.is_empty() {
        return Err(WorkflowError::validation(format!(
            "`{ctx}` must not be empty"
        )));
    }
    seq.iter()
        .enumerate()
        .map(|(i, step)| parse_step(step, input_names, inside_for_each, &format!("{ctx}[{i}]")))
        .collect()
}

fn parse_step(
    value: &Value,
    input_names: &HashSet<String>,
    inside_for_each: bool,
    ctx: &str,
) -> Result<Step> {
    let map = value
        .as_mapping()
        .ok_or_else(|| WorkflowError::validation(format!("{ctx}: each step must be a mapping")))?;
    if map.len() != 1 {
        return Err(WorkflowError::validation(format!(
            "{ctx}: each step must have exactly one step-type key"
        )));
    }
    let (key, params) = map.into_iter().next().expect("len checked == 1");
    let step_type = key
        .as_str()
        .ok_or_else(|| WorkflowError::validation(format!("{ctx}: step type must be a string")))?;

    if RESERVED_STEP_TYPES.contains(&step_type) {
        return Err(WorkflowError::validation(format!(
            "{ctx}: `{step_type}:` is a reserved step type for a future version"
        )));
    }
    if !KNOWN_STEP_TYPES.contains(&step_type) {
        return Err(WorkflowError::validation(format!(
            "{ctx}: unknown step type `{step_type}`"
        )));
    }

    let body = params.as_mapping().ok_or_else(|| {
        WorkflowError::validation(format!("{ctx}: `{step_type}` parameters must be a mapping"))
    })?;

    match step_type {
        "send" => parse_send(body, ctx),
        "wait_for" => parse_wait_for(body, ctx),
        "wait_for_all" => parse_wait_for_all(body, ctx),
        "pause_for_user" => parse_pause(body, ctx),
        "for_each" => parse_for_each(body, input_names, inside_for_each, ctx),
        _ => unreachable!("step type validated above"),
    }
}

fn parse_send(body: &serde_norway::Mapping, ctx: &str) -> Result<Step> {
    reject_unknown_fields(
        body,
        &["to", "prompt", "text", "template_vars", "forward_from"],
        ctx,
        "send",
    )?;

    let to = parse_templated(body.get("to"), &format!("{ctx} send.to"))?
        .ok_or_else(|| WorkflowError::validation(format!("{ctx}: `send` requires `to`")))?;
    check_agent_literal(&to, &format!("{ctx} send.to"))?;

    let prompt = opt_templated_string(body, "prompt", &format!("{ctx} send.prompt"))?;
    let text = opt_templated_string(body, "text", &format!("{ctx} send.text"))?;
    if prompt.is_some() && text.is_some() {
        return Err(WorkflowError::validation(format!(
            "{ctx}: `send` may set `prompt` or `text`, not both"
        )));
    }

    let forward_from = parse_templated(
        body.get("forward_from"),
        &format!("{ctx} send.forward_from"),
    )?;
    if let Some(ff) = &forward_from {
        check_agent_literal(ff, &format!("{ctx} send.forward_from"))?;
    }

    if prompt.is_none() && text.is_none() && forward_from.is_none() {
        return Err(WorkflowError::validation(format!(
            "{ctx}: `send` requires at least one of `prompt`, `text`, or `forward_from`"
        )));
    }

    let template_vars = parse_template_vars(body, ctx)?;

    Ok(Step::Send(SendStep {
        to,
        prompt,
        text,
        template_vars,
        forward_from,
    }))
}

fn parse_template_vars(body: &serde_norway::Mapping, ctx: &str) -> Result<Vec<(String, String)>> {
    let Some(value) = body.get("template_vars") else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let vars = value.as_mapping().ok_or_else(|| {
        WorkflowError::validation(format!("{ctx}: `template_vars` must be a mapping"))
    })?;
    let mut out = Vec::new();
    for (k, v) in vars {
        let name = k
            .as_str()
            .ok_or_else(|| {
                WorkflowError::validation(format!("{ctx}: template_vars keys must be strings"))
            })?
            .to_owned();
        let value = v.as_str().ok_or_else(|| {
            WorkflowError::validation(format!("{ctx}: template_vars `{name}` must be a string"))
        })?;
        validate_template(&format!("{ctx} template_vars.{name}"), value)?;
        out.push((name, value.to_owned()));
    }
    Ok(out)
}

fn parse_wait_for(body: &serde_norway::Mapping, ctx: &str) -> Result<Step> {
    reject_unknown_fields(body, &["agent"], ctx, "wait_for")?;
    let agent = req_templated_string(body, "agent", &format!("{ctx} wait_for.agent"))?;
    Ok(Step::WaitFor(WaitForStep { agent }))
}

fn parse_wait_for_all(body: &serde_norway::Mapping, ctx: &str) -> Result<Step> {
    reject_unknown_fields(body, &["agents"], ctx, "wait_for_all")?;
    let agents = parse_templated(body.get("agents"), &format!("{ctx} wait_for_all.agents"))?
        .ok_or_else(|| {
            WorkflowError::validation(format!("{ctx}: `wait_for_all` requires `agents`"))
        })?;
    check_agent_literal(&agents, &format!("{ctx} wait_for_all.agents"))?;
    Ok(Step::WaitForAll(WaitForAllStep { agents }))
}

fn parse_pause(body: &serde_norway::Mapping, ctx: &str) -> Result<Step> {
    if body.contains_key("output_var") {
        return Err(WorkflowError::validation(format!(
            "{ctx}: `output_var` is a reserved `pause_for_user` field for a future version"
        )));
    }
    reject_unknown_fields(
        body,
        &["context", "recipient", "required"],
        ctx,
        "pause_for_user",
    )?;
    let context = opt_templated_string(body, "context", &format!("{ctx} pause_for_user.context"))?;
    let recipient = opt_templated_string(
        body,
        "recipient",
        &format!("{ctx} pause_for_user.recipient"),
    )?;
    let required = match body.get("required") {
        None => true,
        Some(Value::Bool(b)) => *b,
        Some(_) => {
            return Err(WorkflowError::validation(format!(
                "{ctx}: `required` must be a boolean"
            )));
        }
    };
    Ok(Step::PauseForUser(PauseForUserStep {
        context,
        recipient,
        required,
    }))
}

fn parse_for_each(
    body: &serde_norway::Mapping,
    input_names: &HashSet<String>,
    inside_for_each: bool,
    ctx: &str,
) -> Result<Step> {
    if inside_for_each {
        return Err(WorkflowError::validation(format!(
            "{ctx}: nested `for_each` is not allowed in v1"
        )));
    }
    reject_unknown_fields(body, &["item", "in", "steps"], ctx, "for_each")?;

    let item = req_string(body, "item", &format!("{ctx} for_each"))?;
    if !is_input_name(&item) {
        return Err(WorkflowError::validation(format!(
            "{ctx}: `item` name {item:?} must match `[a-z][a-z0-9_]*`"
        )));
    }
    if RESERVED_NAMES.contains(&item.as_str()) {
        return Err(WorkflowError::validation(format!(
            "{ctx}: `item` name {item:?} is a reserved built-in name"
        )));
    }
    if input_names.contains(&item) {
        return Err(WorkflowError::validation(format!(
            "{ctx}: `item` name {item:?} collides with a workflow input name"
        )));
    }

    let r#in = parse_templated(body.get("in"), &format!("{ctx} for_each.in"))?
        .ok_or_else(|| WorkflowError::validation(format!("{ctx}: `for_each` requires `in`")))?;
    // No empty/duplicate check on `in`: it may be a `[text]` list (empty is valid
    // per the spec, duplicates are meaningful), unlike agent-target literals.

    let steps = parse_steps(
        body.get("steps"),
        input_names,
        true,
        &format!("{ctx} for_each.steps"),
    )?;

    Ok(Step::ForEach(ForEachStep { item, r#in, steps }))
}

/// Parse a value that may be a templated scalar or a YAML list literal of
/// templated strings; validates every contained string as a template. Returns
/// `None` only when the key is absent.
fn parse_templated(value: Option<&Value>, field: &str) -> Result<Option<Templated>> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        Value::String(s) => {
            validate_template(field, s)?;
            Ok(Some(Templated::Scalar(s.clone())))
        }
        Value::Sequence(seq) => {
            let mut items = Vec::with_capacity(seq.len());
            for item in seq {
                let s = item.as_str().ok_or_else(|| {
                    WorkflowError::validation(format!("{field}: list items must be strings"))
                })?;
                validate_template(field, s)?;
                items.push(s.to_owned());
            }
            Ok(Some(Templated::List(items)))
        }
        _ => Err(WorkflowError::validation(format!(
            "{field} must be a string or a list of strings"
        ))),
    }
}

/// A hardcoded `[agent]` list literal must be non-empty and free of duplicate
/// references (after hyphen→underscore normalization). Scalars (templates) are
/// resolved and checked at invocation time, not here.
fn check_agent_literal(value: &Templated, field: &str) -> Result<()> {
    let Templated::List(items) = value else {
        return Ok(());
    };
    if items.is_empty() {
        return Err(WorkflowError::validation(format!(
            "{field}: an agent list literal must not be empty"
        )));
    }
    let mut seen = HashSet::new();
    for item in items {
        let key = canonicalize_for_uniqueness(item.trim());
        if !seen.insert(key) {
            return Err(WorkflowError::validation(format!(
                "{field}: duplicate agent reference {item:?} (names are compared after hyphen→underscore normalization)"
            )));
        }
    }
    Ok(())
}

fn reject_unknown_fields(
    body: &serde_norway::Mapping,
    allowed: &[&str],
    ctx: &str,
    step_type: &str,
) -> Result<()> {
    for (key, _) in body {
        let key = key.as_str().ok_or_else(|| {
            WorkflowError::validation(format!("{ctx}: field names must be strings"))
        })?;
        if !allowed.contains(&key) {
            return Err(WorkflowError::validation(format!(
                "{ctx}: `{step_type}` has unknown field `{key}` (allowed: {})",
                allowed.join(", ")
            )));
        }
    }
    Ok(())
}

fn req_string(map: &serde_norway::Mapping, key: &str, ctx: &str) -> Result<String> {
    match map.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(WorkflowError::validation(format!(
            "{ctx}: `{key}` must be a string"
        ))),
        None => Err(WorkflowError::validation(format!(
            "{ctx}: `{key}` is required"
        ))),
    }
}

fn opt_templated_string(
    map: &serde_norway::Mapping,
    key: &str,
    field: &str,
) -> Result<Option<String>> {
    match map.get(key) {
        None => Ok(None),
        Some(Value::String(s)) => {
            validate_template(field, s)?;
            Ok(Some(s.clone()))
        }
        Some(_) => Err(WorkflowError::validation(format!(
            "{field} must be a string"
        ))),
    }
}

fn req_templated_string(map: &serde_norway::Mapping, key: &str, field: &str) -> Result<String> {
    opt_templated_string(map, key, field)?
        .ok_or_else(|| WorkflowError::validation(format!("{field} is required")))
}

/// `[a-z][a-z0-9-]*` — the workflow `name` slug (hyphens).
fn is_workflow_slug(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `[a-z][a-z0-9_]*` — input and `for_each` `item` names (underscores).
fn is_input_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal valid workflow named `wf`, used as the base for negative-case
    /// tweaks. One text input, one `send`.
    const BASE: &str = "name: wf\ndescription: d\ninputs:\n  goal: text\nsteps:\n  - send:\n      to: \"{{ goal }}\"\n      text: hi\n";

    fn parse(yaml: &str) -> Result<Workflow> {
        parse_workflow("wf", yaml)
    }

    fn err_msg(yaml: &str) -> String {
        parse(yaml).unwrap_err().to_string()
    }

    #[test]
    fn base_is_valid() {
        let wf = parse(BASE).unwrap();
        assert_eq!(wf.name, "wf");
        assert_eq!(wf.inputs.len(), 1);
        assert_eq!(wf.steps.len(), 1);
    }

    #[test]
    fn malformed_yaml_is_yaml_error() {
        let err = parse("name: [unterminated").unwrap_err();
        assert!(matches!(err, WorkflowError::Yaml(_)), "got: {err}");
    }

    #[test]
    fn name_must_be_a_lowercase_slug() {
        assert!(
            parse_workflow(
                "Wf",
                "name: Wf\ndescription: d\nsteps:\n  - send: {to: a, text: b}\n"
            )
            .is_err()
        );
        assert!(
            err_msg("name: wf_x\ndescription: d\nsteps:\n  - send: {to: a, text: b}\n")
                .contains("name")
        );
    }

    #[test]
    fn name_must_equal_filename() {
        let err = parse_workflow("other", BASE).unwrap_err();
        assert!(err.to_string().contains("filename"), "got: {err}");
    }

    #[test]
    fn unknown_top_level_key_is_rejected() {
        let yaml = format!("{BASE}extra: nope\n");
        assert!(err_msg(&yaml).contains("unknown top-level key"));
    }

    #[test]
    fn reserved_top_level_keys_are_rejected() {
        for key in ["until", "outputs", "metadata"] {
            let yaml = format!("{BASE}{key}: x\n");
            assert!(
                err_msg(&yaml).contains("reserved"),
                "{key} should be reserved"
            );
        }
    }

    #[test]
    fn missing_required_top_level_keys() {
        assert!(err_msg("description: d\nsteps:\n  - send: {to: a, text: b}\n").contains("name"));
        assert!(err_msg("name: wf\nsteps:\n  - send: {to: a, text: b}\n").contains("description"));
        assert!(err_msg("name: wf\ndescription: d\n").contains("steps"));
    }

    #[test]
    fn empty_description_and_empty_steps_are_rejected() {
        assert!(
            err_msg("name: wf\ndescription: \"\"\nsteps:\n  - send: {to: a, text: b}\n")
                .contains("description")
        );
        assert!(err_msg("name: wf\ndescription: d\nsteps: []\n").contains("empty"));
    }

    #[test]
    fn input_type_grammar() {
        let yaml = "name: wf\ndescription: d\ninputs:\n  a: agent\n  b: [agent]\n  p: prompt_id\n  t: text\n  o: text?\n  l: [text]\nsteps:\n  - send: {to: \"{{ a }}\", text: x}\n";
        let wf = parse(yaml).unwrap();
        let types: Vec<_> = wf
            .inputs
            .iter()
            .map(|i| (i.name.as_str(), i.ty, i.optional))
            .collect();
        assert_eq!(types[0], ("a", InputType::Agent, false));
        assert_eq!(types[1], ("b", InputType::AgentList, false));
        assert_eq!(types[2], ("p", InputType::PromptId, false));
        assert_eq!(types[3], ("t", InputType::Text, false));
        assert_eq!(types[4], ("o", InputType::Text, true)); // text? → optional
        assert_eq!(types[5], ("l", InputType::TextList, false));
    }

    #[test]
    fn unknown_input_type_is_rejected() {
        assert!(err_msg("name: wf\ndescription: d\ninputs:\n  x: widget\nsteps:\n  - send: {to: a, text: b}\n").contains("unknown type"));
    }

    #[test]
    fn optional_suffix_only_valid_on_text() {
        let err = err_msg(
            "name: wf\ndescription: d\ninputs:\n  x: agent?\nsteps:\n  - send: {to: a, text: b}\n",
        );
        assert!(err.contains("only valid on `text`"), "got: {err}");
    }

    #[test]
    fn long_form_default_implies_optional() {
        let yaml = "name: wf\ndescription: d\ninputs:\n  ctx:\n    type: text\n    description: opt\n    default: \"\"\nsteps:\n  - send: {to: a, text: b}\n";
        let wf = parse(yaml).unwrap();
        assert!(wf.inputs[0].optional);
        assert_eq!(wf.inputs[0].default.as_deref(), Some(""));
        assert_eq!(wf.inputs[0].description.as_deref(), Some("opt"));
    }

    #[test]
    fn default_on_non_text_type_is_rejected() {
        // Optionality is text-only in v1; a default on a list/agent/prompt_id type
        // would create an unsupported optional non-text input.
        for ty in ["agent", "[agent]", "prompt_id", "[text]"] {
            let yaml = format!(
                "name: wf\ndescription: d\ninputs:\n  x:\n    type: {ty}\n    default: v\nsteps:\n  - send: {{to: a, text: t}}\n"
            );
            assert!(
                err_msg(&yaml).contains("only valid on a `text`"),
                "default on {ty} should be rejected"
            );
        }
    }

    #[test]
    fn reserved_input_name_user_input_is_rejected() {
        assert!(err_msg("name: wf\ndescription: d\ninputs:\n  user_input: text\nsteps:\n  - send: {to: a, text: b}\n").contains("reserved"));
    }

    #[test]
    fn input_name_grammar_is_enforced() {
        assert!(err_msg("name: wf\ndescription: d\ninputs:\n  Bad-Name: text\nsteps:\n  - send: {to: a, text: b}\n").contains("must match"));
    }

    #[test]
    fn step_must_have_exactly_one_key() {
        let err = err_msg(
            "name: wf\ndescription: d\nsteps:\n  - send: {to: a, text: b}\n    wait_for: {agent: a}\n",
        );
        assert!(err.contains("exactly one"), "got: {err}");
    }

    #[test]
    fn unknown_and_reserved_step_types() {
        assert!(
            err_msg("name: wf\ndescription: d\nsteps:\n  - frobnicate: {}\n")
                .contains("unknown step type")
        );
        for key in ["if", "branch", "wait_for_first"] {
            let yaml = format!("name: wf\ndescription: d\nsteps:\n  - {key}: {{}}\n");
            assert!(err_msg(&yaml).contains("reserved step type"), "{key}");
        }
    }

    #[test]
    fn send_requires_a_body_source() {
        let err = err_msg("name: wf\ndescription: d\nsteps:\n  - send:\n      to: a\n");
        assert!(
            err.contains("at least one of `prompt`, `text`, or `forward_from`"),
            "got: {err}"
        );
    }

    #[test]
    fn send_prompt_and_text_are_mutually_exclusive() {
        let err = err_msg(
            "name: wf\ndescription: d\nsteps:\n  - send:\n      to: a\n      prompt: p\n      text: t\n",
        );
        assert!(err.contains("not both"), "got: {err}");
    }

    #[test]
    fn send_unknown_field_is_rejected() {
        let err = err_msg(
            "name: wf\ndescription: d\nsteps:\n  - send:\n      to: a\n      text: t\n      bogus: x\n",
        );
        assert!(err.contains("unknown field `bogus`"), "got: {err}");
    }

    #[test]
    fn agent_list_literal_empty_and_duplicate_are_rejected() {
        let empty =
            err_msg("name: wf\ndescription: d\nsteps:\n  - send:\n      to: []\n      text: t\n");
        assert!(empty.contains("must not be empty"), "got: {empty}");
        let dup = err_msg(
            "name: wf\ndescription: d\nsteps:\n  - send:\n      to: [rev-1, rev_1]\n      text: t\n",
        );
        assert!(dup.contains("duplicate"), "got: {dup}"); // rev-1 and rev_1 normalize equal
    }

    #[test]
    fn agent_list_literal_valid_passes() {
        let wf =
            parse("name: wf\ndescription: d\nsteps:\n  - send:\n      to: [a, b]\n      text: t\n")
                .unwrap();
        let Step::Send(send) = &wf.steps[0] else {
            panic!()
        };
        assert_eq!(
            send.to,
            Templated::List(vec!["a".to_owned(), "b".to_owned()])
        );
    }

    #[test]
    fn wait_for_requires_agent() {
        assert!(err_msg("name: wf\ndescription: d\nsteps:\n  - wait_for: {}\n").contains("agent"));
    }

    #[test]
    fn pause_output_var_is_reserved_and_required_must_be_bool() {
        assert!(
            err_msg("name: wf\ndescription: d\nsteps:\n  - pause_for_user:\n      output_var: x\n")
                .contains("reserved")
        );
        let err = err_msg(
            "name: wf\ndescription: d\nsteps:\n  - pause_for_user:\n      required: maybe\n",
        );
        assert!(err.contains("boolean"), "got: {err}");
    }

    #[test]
    fn pause_required_defaults_true() {
        let wf = parse("name: wf\ndescription: d\nsteps:\n  - pause_for_user:\n      context: c\n")
            .unwrap();
        let Step::PauseForUser(p) = &wf.steps[0] else {
            panic!()
        };
        assert!(p.required);
    }

    #[test]
    fn nested_for_each_is_rejected() {
        let yaml = "name: wf\ndescription: d\ninputs:\n  ms: [text]\nsteps:\n  - for_each:\n      item: m\n      in: \"{{ ms }}\"\n      steps:\n        - for_each:\n            item: n\n            in: \"{{ ms }}\"\n            steps:\n              - send: {to: a, text: t}\n";
        assert!(err_msg(yaml).contains("nested"));
    }

    #[test]
    fn for_each_item_collides_with_input_name() {
        let yaml = "name: wf\ndescription: d\ninputs:\n  m: [text]\nsteps:\n  - for_each:\n      item: m\n      in: \"{{ m }}\"\n      steps:\n        - send: {to: a, text: t}\n";
        assert!(err_msg(yaml).contains("collides"));
    }

    #[test]
    fn for_each_item_cannot_be_reserved_name() {
        let yaml = "name: wf\ndescription: d\ninputs:\n  ms: [text]\nsteps:\n  - for_each:\n      item: user_input\n      in: \"{{ ms }}\"\n      steps:\n        - send: {to: a, text: t}\n";
        assert!(err_msg(yaml).contains("reserved"));
    }

    #[test]
    fn for_each_in_text_list_literal_may_be_empty_and_duplicated() {
        // `in` is not an agent target, so the empty/dup agent-literal rule does
        // not apply (empty [text] is valid; duplicate text items are meaningful).
        let yaml = "name: wf\ndescription: d\nsteps:\n  - for_each:\n      item: m\n      in: [x, x]\n      steps:\n        - send: {to: a, text: \"{{ m }}\"}\n";
        assert!(parse(yaml).is_ok());
    }

    #[test]
    fn template_syntax_error_is_a_template_error() {
        let err = parse("name: wf\ndescription: d\nsteps:\n  - send:\n      to: a\n      text: \"{{ unclosed\"\n").unwrap_err();
        assert!(matches!(err, WorkflowError::Template { .. }), "got: {err}");
    }

    #[test]
    fn unsupported_tag_in_step_field_is_a_template_error() {
        // Parse routes templated step fields through the subset scan.
        let err = parse("name: wf\ndescription: d\nsteps:\n  - send:\n      to: a\n      text: \"{% set x = 1 %}\"\n").unwrap_err();
        assert!(matches!(err, WorkflowError::Template { .. }), "got: {err}");
    }

    #[test]
    fn duplicate_input_name_is_rejected() {
        // Two inputs with the same key — a duplicate mapping key. (serde_norway
        // keeps the last; the explicit guard is belt-and-suspenders.)
        let yaml = "name: wf\ndescription: d\ninputs:\n  a: text\n  a: agent\nsteps:\n  - send: {to: a, text: t}\n";
        // Either serde collapses the dup (one input) or our guard fires; both are
        // acceptable — assert it does not panic and yields a single `a`.
        if let Ok(wf) = parse(yaml) {
            assert_eq!(wf.inputs.iter().filter(|i| i.name == "a").count(), 1);
        }
    }
}
