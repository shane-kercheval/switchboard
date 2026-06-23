//! Per-step display metadata — the shape the progress and preview views render.
//!
//! Derived purely from a parsed [`Workflow`], so it lives here (not in the app):
//! the run-file snapshot ([`crate::RunRecord`]) persists it, and the app's wire
//! types reuse it. A step's recipients are its *declared* references — literal
//! agent names and input-slot references; the app resolves slots to concrete
//! agent names for a live run (the preview resolves them against the form). The
//! declared/resolved split is why a recipient is a two-variant enum rather than a
//! bare name.

use serde::{Deserialize, Serialize};

use crate::model::{InputDecl, LabeledStep, Step, Templated, Workflow};

/// One step, as the progress/preview views show it: its label, the agent(s) it
/// targets, and any "feeds from" forwarding hint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowStepInfo {
    pub label: String,
    /// The step's targets — who it sends to / waits for. Empty for steps with no
    /// agent target (e.g. a `for_each` wrapper).
    pub recipients: Vec<RecipientRef>,
    /// Forwarding sources (`send.forward_from`), shown as a "feeds from" hint.
    /// Empty when the step forwards nothing.
    pub feeds_from: Vec<RecipientRef>,
}

/// A declared recipient reference. `Literal` is a hardcoded agent name; `Slot` is
/// an `agent`/`[agent]` input the user binds at invocation — the preview resolves
/// it live against the form, and the app resolves it to concrete names for a live
/// run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecipientRef {
    Literal { name: String },
    Slot { input: String },
}

/// The declared per-step display info for a workflow, in step order. Slot
/// references stay unresolved (the file/preview owns resolution).
#[must_use]
pub fn step_display(workflow: &Workflow) -> Vec<WorkflowStepInfo> {
    workflow
        .steps
        .iter()
        .map(|labeled| step_info(labeled, &workflow.inputs))
        .collect()
}

fn step_info(labeled: &LabeledStep, inputs: &[InputDecl]) -> WorkflowStepInfo {
    let (recipients, feeds_from) = match &labeled.step {
        Step::Send(s) => (
            refs(&s.to, inputs),
            s.forward_from
                .as_ref()
                .map(|f| refs(f, inputs))
                .unwrap_or_default(),
        ),
        Step::WaitFor(w) => (vec![classify(&w.agent, inputs)], Vec::new()),
        Step::WaitForAll(w) => (refs(&w.agents, inputs), Vec::new()),
        Step::PauseForUser(p) => (
            p.recipient
                .as_ref()
                .map(|r| vec![classify(r, inputs)])
                .unwrap_or_default(),
            Vec::new(),
        ),
        // A `for_each` wrapper has no target of its own; its body steps carry
        // theirs. Bodies are not descended yet — revisit when `for_each` becomes
        // runnable (end of v1): an iterating run's progress view will need the body
        // steps flattened/recursed, not just the bare wrapper row.
        Step::ForEach(_) => (Vec::new(), Vec::new()),
    };
    WorkflowStepInfo {
        label: labeled.label.clone(),
        recipients,
        feeds_from,
    }
}

fn refs(value: &Templated, inputs: &[InputDecl]) -> Vec<RecipientRef> {
    match value {
        Templated::Scalar(s) => vec![classify(s, inputs)],
        Templated::List(items) => items.iter().map(|s| classify(s, inputs)).collect(),
    }
}

/// Classify a single target string. A bare `{{ name }}` referencing a declared
/// agent input is a `Slot`; anything else (a literal name, or a template that
/// isn't a single agent-input reference) is shown verbatim as a `Literal`.
fn classify(raw: &str, inputs: &[InputDecl]) -> RecipientRef {
    let trimmed = raw.trim();
    if let Some(var) = single_var(trimmed)
        && inputs.iter().any(|i| i.name == var && i.ty.is_agent())
    {
        return RecipientRef::Slot {
            input: var.to_owned(),
        };
    }
    RecipientRef::Literal {
        name: trimmed.to_owned(),
    }
}

/// Extract `name` from a string that is exactly `{{ name }}` — a single bare
/// identifier (the input-name grammar), no filters or calls — else `None`.
fn single_var(s: &str) -> Option<&str> {
    let inner = s.strip_prefix("{{")?.strip_suffix("}}")?.trim();
    let mut chars = inner.chars();
    let valid = matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && inner
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    valid.then_some(inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_workflow;

    fn display(yaml: &str) -> Vec<WorkflowStepInfo> {
        step_display(&parse_workflow("wf", yaml).expect("fixture parses"))
    }

    #[test]
    fn send_to_a_slot_and_a_literal_are_classified() {
        let steps = display(
            "name: wf\ndescription: d\ninputs:\n  reviewers: [agent]\nsteps:\n  - {label: Review, send: {to: \"{{ reviewers }}\", text: hi}}\n  - {label: Ping bob, send: {to: bob, text: hi}}\n",
        );
        assert_eq!(steps[0].label, "Review");
        assert_eq!(
            steps[0].recipients,
            vec![RecipientRef::Slot {
                input: "reviewers".to_owned()
            }]
        );
        assert_eq!(
            steps[1].recipients,
            vec![RecipientRef::Literal {
                name: "bob".to_owned()
            }]
        );
    }

    #[test]
    fn list_literal_targets_each_classify() {
        let steps = display(
            "name: wf\ndescription: d\nsteps:\n  - {label: Fan out, send: {to: [a, b], text: hi}}\n",
        );
        assert_eq!(
            steps[0].recipients,
            vec![
                RecipientRef::Literal {
                    name: "a".to_owned()
                },
                RecipientRef::Literal {
                    name: "b".to_owned()
                }
            ]
        );
    }

    #[test]
    fn non_agent_input_reference_is_a_literal_not_a_slot() {
        // `{{ note }}` references a text input, not an agent, so it is not a Slot.
        let steps = display(
            "name: wf\ndescription: d\ninputs:\n  note: text\nsteps:\n  - {label: S, send: {to: \"{{ note }}\", text: hi}}\n",
        );
        assert_eq!(
            steps[0].recipients,
            vec![RecipientRef::Literal {
                name: "{{ note }}".to_owned()
            }]
        );
    }

    #[test]
    fn forward_from_becomes_feeds_from() {
        let steps = display(
            "name: wf\ndescription: d\ninputs:\n  planner: agent\n  worker: agent\nsteps:\n  - {label: Hand off, send: {to: \"{{ worker }}\", forward_from: \"{{ planner }}\", text: go}}\n",
        );
        assert_eq!(
            steps[0].feeds_from,
            vec![RecipientRef::Slot {
                input: "planner".to_owned()
            }]
        );
    }

    #[test]
    fn wire_shape_is_snake_case_with_kind_tagged_recipients() {
        let info = WorkflowStepInfo {
            label: "Send the review".to_owned(),
            recipients: vec![
                RecipientRef::Slot {
                    input: "reviewers".to_owned(),
                },
                RecipientRef::Literal {
                    name: "ops".to_owned(),
                },
            ],
            feeds_from: Vec::new(),
        };
        let v = serde_json::to_value(&info).unwrap();
        assert_eq!(v["label"], "Send the review");
        assert_eq!(v["recipients"][0]["kind"], "slot");
        assert_eq!(v["recipients"][0]["input"], "reviewers");
        assert_eq!(v["recipients"][1]["kind"], "literal");
        assert_eq!(v["recipients"][1]["name"], "ops");
        assert!(v["feeds_from"].as_array().unwrap().is_empty());
        // Round-trips.
        let back: WorkflowStepInfo = serde_json::from_value(v).unwrap();
        assert_eq!(back, info);
    }

    #[test]
    fn wait_and_pause_recipients_are_captured() {
        let steps = display(
            "name: wf\ndescription: d\ninputs:\n  planner: agent\nsteps:\n  - {label: Wait, wait_for: {agent: \"{{ planner }}\"}}\n  - {label: Ask, pause_for_user: {context: c, recipient: \"{{ planner }}\"}}\n",
        );
        assert_eq!(
            steps[0].recipients,
            vec![RecipientRef::Slot {
                input: "planner".to_owned()
            }]
        );
        assert_eq!(
            steps[1].recipients,
            vec![RecipientRef::Slot {
                input: "planner".to_owned()
            }]
        );
    }
}
