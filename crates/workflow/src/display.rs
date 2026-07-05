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

use crate::model::{InputDecl, LabeledStep, SendStep, Step, Templated, Workflow};

/// What a step *is*, surfaced so the progress view can group a `send` with the
/// `wait` that synchronizes it (and tell both apart from a `pause`). Both wait
/// variants collapse to `Wait` — the display never distinguishes single from all.
///
/// `#[non_exhaustive]` because this enum crosses IPC to the frontend (whose
/// discriminated union degrades gracefully on an unknown kind): a future variant
/// must not be a breaking change for that consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WorkflowStepKind {
    Send,
    Wait,
    Pause,
    ForEach,
}

/// One step, as the progress/preview views show it: its kind, label, the prompt it
/// runs, the agent(s) it targets, and any "feeds from" forwarding hint.
///
/// **Persisted** inside [`crate::RunRecord::Started`] — a crash-recovery snapshot
/// that must still load after a relaunch, including one that spans an app update.
/// Evolution rule: every field added here from now on must be `Option<T>` +
/// `#[serde(default)]`, so a run file written by an older build never fails to
/// deserialize and silently drops the held run from the run list. `kind` predates
/// this rule and stays required (it is always written since it existed); the
/// `Option` fields below carry the default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowStepInfo {
    pub kind: WorkflowStepKind,
    pub label: String,
    /// Optional one-line explanation, shown as a sub-line under the label.
    #[serde(default)]
    pub description: Option<String>,
    /// The prompt this step runs, surfaced as a chip: a named prompt
    /// (`builtin:code-review`) or inline text. `None` for steps that run no prompt
    /// (a wait/pause, or a pure-forward send whose message is the forwarded output).
    #[serde(default)]
    pub prompt: Option<StepPrompt>,
    /// The step's targets — who it sends to / waits for. Empty for steps with no
    /// agent target (e.g. a `for_each` wrapper).
    pub recipients: Vec<RecipientRef>,
    /// Forwarding sources (`send.forward_from`), shown as a "feeds from" hint.
    /// Empty when the step forwards nothing.
    pub feeds_from: Vec<RecipientRef>,
}

/// The prompt a `send` step runs, for the progress/preview "which prompt" chip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StepPrompt {
    /// A named prompt referenced by id (e.g. `builtin:code-review`). The id keeps
    /// its `provider:name` form; the view shows the name and the full id on hover.
    Named { id: String },
    /// The send carries inline text rather than a named prompt. `text` is the raw
    /// template as authored (`MiniJinja` placeholders intact), carried so the view
    /// can preview it directly — there is no provider to fetch it from.
    ///
    /// `#[serde(default)]` is load-bearing: `WorkflowStepInfo` is persisted in
    /// `RunRecord::Started` (see the struct doc above), and `Inline` was a unit
    /// variant before `text` existed. A run file written by an older build
    /// serializes it as `{"kind":"inline"}` with no `text`; without the default,
    /// that record fails to deserialize and the whole run silently drops from the
    /// list on the first post-upgrade launch. A legacy inline chip previews empty.
    Inline {
        #[serde(default)]
        text: String,
    },
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
    let (kind, recipients, feeds_from, prompt) = match &labeled.step {
        Step::Send(s) => (
            WorkflowStepKind::Send,
            refs(&s.to, inputs),
            s.forward_from
                .as_ref()
                .map(|f| refs(f, inputs))
                .unwrap_or_default(),
            send_prompt(s),
        ),
        Step::WaitFor(w) => (
            WorkflowStepKind::Wait,
            vec![classify(&w.agent, inputs)],
            Vec::new(),
            None,
        ),
        Step::WaitForAll(w) => (
            WorkflowStepKind::Wait,
            refs(&w.agents, inputs),
            Vec::new(),
            None,
        ),
        Step::PauseForUser(p) => (
            WorkflowStepKind::Pause,
            p.recipient
                .as_ref()
                .map(|r| vec![classify(r, inputs)])
                .unwrap_or_default(),
            Vec::new(),
            None,
        ),
        // A `for_each` wrapper has no target of its own; its body steps carry
        // theirs. Bodies are not descended yet — revisit when `for_each` becomes
        // runnable (end of v1): an iterating run's progress view will need the body
        // steps flattened/recursed, not just the bare wrapper row.
        Step::ForEach(_) => (WorkflowStepKind::ForEach, Vec::new(), Vec::new(), None),
    };
    WorkflowStepInfo {
        kind,
        label: labeled.label.clone(),
        description: labeled.description.clone(),
        prompt,
        recipients,
        feeds_from,
    }
}

/// The prompt a `send` runs: a named prompt id, inline text, or `None` for a
/// pure-forward send (no prompt and no text — its message is the forwarded output).
fn send_prompt(s: &SendStep) -> Option<StepPrompt> {
    if let Some(id) = &s.prompt {
        Some(StepPrompt::Named { id: id.clone() })
    } else {
        s.text
            .as_ref()
            .map(|text| StepPrompt::Inline { text: text.clone() })
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
            kind: WorkflowStepKind::Send,
            label: "Send the review".to_owned(),
            description: Some("Each reviewer reviews the diff.".to_owned()),
            prompt: Some(StepPrompt::Named {
                id: "builtin:code-review".to_owned(),
            }),
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
        assert_eq!(v["kind"], "send");
        assert_eq!(v["description"], "Each reviewer reviews the diff.");
        assert_eq!(v["prompt"]["kind"], "named");
        assert_eq!(v["prompt"]["id"], "builtin:code-review");
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
    fn step_kind_is_populated_per_step_type() {
        let steps = display(
            "name: wf\ndescription: d\ninputs:\n  a: agent\n  b: agent\nsteps:\n  - {label: S, send: {to: \"{{ a }}\", text: hi}}\n  - {label: W, wait_for: {agent: \"{{ a }}\"}}\n  - {label: WA, wait_for_all: {agents: [\"{{ a }}\", \"{{ b }}\"]}}\n  - {label: P, pause_for_user: {context: c}}\n",
        );
        let kinds: Vec<WorkflowStepKind> = steps.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            vec![
                WorkflowStepKind::Send,
                WorkflowStepKind::Wait,
                WorkflowStepKind::Wait, // both wait variants collapse to Wait
                WorkflowStepKind::Pause,
            ]
        );
    }

    #[test]
    fn persisted_step_tolerates_omitted_optional_fields() {
        // `WorkflowStepInfo` is persisted in run files (`RunRecord::Started`). A
        // snapshot written by a build before `description`/`prompt` existed omits
        // those keys; it must still load (degrading to `None`) so a held/interrupted
        // run never silently drops from the run list across an app update. `kind` is
        // required (always written since it existed) — see the struct's evolution rule.
        let snapshot = serde_json::json!({
            "kind": "send",
            "label": "Code review",
            "recipients": [{ "kind": "slot", "input": "reviewers" }],
            "feeds_from": [],
        });
        let back: WorkflowStepInfo = serde_json::from_value(snapshot).unwrap();
        assert_eq!(back.label, "Code review");
        assert_eq!(back.description, None);
        assert_eq!(back.prompt, None);
    }

    #[test]
    fn step_description_is_optional_and_carried_through() {
        let steps = display(
            "name: wf\ndescription: d\ninputs:\n  a: agent\nsteps:\n  - {label: S, description: Reviews the diff, send: {to: \"{{ a }}\", text: hi}}\n  - {label: W, wait_for: {agent: \"{{ a }}\"}}\n",
        );
        assert_eq!(steps[0].description.as_deref(), Some("Reviews the diff"));
        // A step without a `description:` key carries `None`.
        assert_eq!(steps[1].description, None);
    }

    #[test]
    fn step_prompt_reflects_named_inline_and_forward_only_sends() {
        let steps = display(
            "name: wf\ndescription: d\ninputs:\n  a: agent\n  b: agent\nsteps:\n  - {label: Named, send: {to: \"{{ a }}\", prompt: \"builtin:code-review\"}}\n  - {label: Inline, send: {to: \"{{ a }}\", text: hi}}\n  - {label: Forward, send: {to: \"{{ b }}\", forward_from: \"{{ a }}\"}}\n  - {label: Wait, wait_for: {agent: \"{{ a }}\"}}\n",
        );
        assert_eq!(
            steps[0].prompt,
            Some(StepPrompt::Named {
                id: "builtin:code-review".to_owned()
            })
        );
        assert_eq!(
            steps[1].prompt,
            Some(StepPrompt::Inline {
                text: "hi".to_owned()
            })
        );
        // A pure-forward send runs no prompt and no inline text.
        assert_eq!(steps[2].prompt, None);
        // A wait runs no prompt.
        assert_eq!(steps[3].prompt, None);
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
