//! The three worked examples from `docs/workflow-spec.md` §"Worked examples"
//! parse and validate. They are the canonical fixtures: a spec change that breaks
//! one should break this test. Each is also run through invocation-time
//! validation against a mock roster + prompt resolver.

use std::collections::BTreeMap;

use switchboard_workflow::{
    InputValue, OutputScope, Scope, Step, Workflow, bind_invocation, parse_workflow, render,
    validate_invocation,
};

fn load(stem: &str) -> Workflow {
    let path = format!("{}/tests/fixtures/{stem}.yaml", env!("CARGO_MANIFEST_DIR"));
    let content = std::fs::read_to_string(&path).expect("fixture readable");
    parse_workflow(stem, &content).unwrap_or_else(|e| panic!("{stem} must parse: {e}"))
}

#[test]
fn sequential_handoff_parses() {
    let wf = load("plan-then-implement");
    assert_eq!(wf.name, "plan-then-implement");
    assert_eq!(wf.inputs.len(), 3);
    assert_eq!(wf.steps.len(), 4);
    assert!(matches!(wf.steps[0], Step::Send(_)));
    assert!(matches!(wf.steps[1], Step::WaitFor(_)));
}

#[test]
fn fan_in_review_parses_and_invocation_validates() {
    let wf = load("review-and-aggregate");
    assert_eq!(wf.steps.len(), 3);
    assert!(matches!(wf.steps[1], Step::WaitForAll(_)));

    let agents = vec![
        "primary".to_owned(),
        "reviewer-1".to_owned(),
        "reviewer-2".to_owned(),
    ];
    let mut supplied = BTreeMap::new();
    supplied.insert(
        "primary_agent".to_owned(),
        InputValue::Text("primary".to_owned()),
    );
    supplied.insert(
        "reviewer_agents".to_owned(),
        InputValue::List(vec!["reviewer-1".to_owned(), "reviewer-2".to_owned()]),
    );
    supplied.insert(
        "review_prompt".to_owned(),
        InputValue::Text("local:review".to_owned()),
    );
    supplied.insert(
        "aggregation_prompt".to_owned(),
        InputValue::Text("local:aggregate".to_owned()),
    );
    // user_context is text? — omitted; optional, so invocation still validates.

    validate_invocation(&wf, &supplied, &agents, |id| id.starts_with("local:"))
        .expect("invocation should validate");
}

#[test]
fn milestone_iteration_parses_with_for_each_and_pause() {
    // Example 3 uses for_each + pause_for_user. Both are syntactically valid and
    // must parse here (they are gated only at *invocation* by a later milestone's
    // capability check, never at parse time — else this example couldn't exist).
    let wf = load("implement-milestones");
    assert_eq!(wf.steps.len(), 1);
    let Step::ForEach(fe) = &wf.steps[0] else {
        panic!("expected a for_each step");
    };
    assert_eq!(fe.item, "milestone");
    assert_eq!(fe.steps.len(), 8);
    assert!(matches!(fe.steps[2], Step::PauseForUser(_)));
}

/// End-to-end over a real fixture: parse → bind (with an omitted optional) →
/// render. Guards two things at once — that an omitted `text?` input renders as
/// its default rather than failing strict-undefined, and that the step-3
/// `aggregated_responses` `template_var` composes the reviewers' outputs in
/// declared order.
fn send_step(wf: &Workflow, index: usize) -> &switchboard_workflow::SendStep {
    match &wf.steps[index] {
        Step::Send(s) => s,
        other => panic!("step {index} should be a send, got {other:?}"),
    }
}

#[test]
fn review_and_aggregate_renders_end_to_end_with_omitted_optional() {
    let wf = load("review-and-aggregate");
    let agents = vec![
        "primary".to_owned(),
        "reviewer-1".to_owned(),
        "reviewer-2".to_owned(),
    ];
    let mut supplied = BTreeMap::new();
    supplied.insert(
        "primary_agent".to_owned(),
        InputValue::Text("primary".to_owned()),
    );
    supplied.insert(
        "reviewer_agents".to_owned(),
        InputValue::List(vec!["reviewer-1".to_owned(), "reviewer-2".to_owned()]),
    );
    supplied.insert(
        "review_prompt".to_owned(),
        InputValue::Text("local:review".to_owned()),
    );
    supplied.insert(
        "aggregation_prompt".to_owned(),
        InputValue::Text("local:aggregate".to_owned()),
    );
    // user_context (text?) deliberately omitted.

    let inputs = bind_invocation(&wf, &supplied, &agents, |id| id.starts_with("local:")).unwrap();

    // Step 1's `context` template_var references the omitted optional → "".
    let context_tmpl = &send_step(&wf, 0)
        .template_vars
        .iter()
        .find(|(k, _)| k == "context")
        .expect("step 1 has a `context` template_var")
        .1;
    let scope = Scope {
        inputs: inputs.clone(),
        ..Default::default()
    };
    assert_eq!(
        render(context_tmpl, scope, &OutputScope::new()).unwrap(),
        ""
    );

    // Step 3's `feedback` aggregates the reviewers' outputs in declared order.
    let feedback_tmpl = &send_step(&wf, 2)
        .template_vars
        .iter()
        .find(|(k, _)| k == "feedback")
        .expect("step 3 has a `feedback` template_var")
        .1;
    let mut outputs = OutputScope::new();
    outputs.insert("reviewer_1".to_owned(), "looks good".to_owned());
    outputs.insert("reviewer_2".to_owned(), "ship it".to_owned());
    let scope = Scope {
        inputs,
        ..Default::default()
    };
    let rendered = render(feedback_tmpl, scope, &outputs).unwrap();
    assert!(rendered.contains("=== START response from reviewer-1 ==="));
    assert!(rendered.contains("looks good"));
    assert!(rendered.contains("ship it"));
    assert!(
        rendered.find("reviewer-1").unwrap() < rendered.find("reviewer-2").unwrap(),
        "reviewers must aggregate in declared order"
    );
}
