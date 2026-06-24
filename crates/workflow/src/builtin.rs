//! The app-owned, read-only built-in workflow library, baked into the binary.
//! Mirrors the built-in prompt library: the pure crate owns the content; the app
//! merges these into the workflow list (tagged read-only, governed by the
//! "show built-ins" toggle) and offers "Copy to my workflows". The YAML's `name:`
//! is the **single id authority** — there is no separate catalog name to drift
//! out of sync (the same lesson the built-in prompts learned).

use crate::error::{Result, WorkflowError};
use crate::model::Workflow;
use crate::parse::parse_workflow;

/// The bundled workflows as raw YAML. Each is parsed with its own `name:` as the
/// file stem, so the YAML is the sole source of its id.
const BUILTIN_WORKFLOWS: &[&str] = &[
    include_str!("../resources/workflows/review-and-recommend.yaml"),
    include_str!("../resources/workflows/review-and-reconcile.yaml"),
];

/// Extract the top-level `name:` from a workflow YAML without a full parse, so a
/// built-in can be parsed with its own name as the (file-stem-equal) stem.
fn probe_name(content: &str) -> Option<String> {
    let value: serde_norway::Value = serde_norway::from_str(content).ok()?;
    value.get("name")?.as_str().map(str::to_owned)
}

/// Parse every built-in workflow, surfacing parse errors (a malformed built-in is
/// a build-time bug — the `all_builtin_workflows_parse` test makes that a CI
/// failure rather than a silent omission).
#[must_use]
pub fn parse_builtin_workflows() -> Vec<Result<Workflow>> {
    BUILTIN_WORKFLOWS
        .iter()
        .map(|content| {
            let name = probe_name(content).ok_or_else(|| {
                WorkflowError::Yaml("built-in workflow is missing a `name`".to_owned())
            })?;
            parse_workflow(&name, content)
        })
        .collect()
}

/// The successfully-parsed built-in workflows, for the workflow list.
#[must_use]
pub fn builtin_workflows() -> Vec<Workflow> {
    parse_builtin_workflows()
        .into_iter()
        .filter_map(Result::ok)
        .collect()
}

/// The parsed built-in named `name`, or `None` if there is no such built-in.
#[must_use]
pub fn builtin_workflow(name: &str) -> Option<Workflow> {
    builtin_workflows().into_iter().find(|w| w.name == name)
}

/// The raw YAML of the built-in named `name`, for "Copy to my workflows".
#[must_use]
pub fn builtin_workflow_content(name: &str) -> Option<&'static str> {
    BUILTIN_WORKFLOWS
        .iter()
        .copied()
        .find(|content| probe_name(content).as_deref() == Some(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_builtin_workflows_parse() {
        // Release-owned static assets: every shipped built-in must parse, or it
        // would silently vanish from the workflow list and break invoke/copy.
        for (i, result) in parse_builtin_workflows().into_iter().enumerate() {
            result.unwrap_or_else(|e| panic!("built-in workflow #{i} does not parse: {e}"));
        }
    }

    #[test]
    fn the_two_shipped_built_ins_are_present_and_runnable() {
        let names: Vec<String> = builtin_workflows().into_iter().map(|w| w.name).collect();
        assert!(
            names.contains(&"review-and-recommend".to_owned()),
            "{names:?}"
        );
        assert!(
            names.contains(&"review-and-reconcile".to_owned()),
            "{names:?}"
        );
        // Both use only runnable steps (no pause_for_user / for_each).
        for w in builtin_workflows() {
            assert_eq!(w.gated_step_kind(), None, "{} should be runnable", w.name);
        }
    }

    #[test]
    fn content_matches_by_name() {
        let yaml = builtin_workflow_content("review-and-recommend").unwrap();
        assert!(yaml.contains("name: review-and-recommend"));
        assert!(builtin_workflow_content("nope").is_none());
    }
}
