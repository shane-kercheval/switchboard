//! Fixture-driven tests of the `ops` layer over the scripted harness. Each
//! starts from a project the window has not opened: the project exists in
//! the store, and the state under test was built after it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use switchboard_core::{AgentId, JournalRecord, ProjectId};
use switchboard_harness::{CancelSource, TurnOutcome};
use switchboard_workflow::{InputValue, RunRecord, TerminalStatus};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::commands::{
    ForwardSourceRef, list_project_agents_readonly_impl, load_project_conversation_impl,
    send_message_impl,
};
use crate::error::AppError;
use crate::state::{TestSeam, lock};

use super::budget::fit;
use super::change::{
    SelectionChange, create_agent, create_project, dismiss_workflow_run, rename_agent,
    set_agent_selection,
};
use super::error_code::{self, error_code};
use super::ledger::SendStatus;
use super::read::conversation_budget;
use super::read::{
    AgentOutputStatus, DigestItem, OutputSource, ProjectStatus, list_agents, list_projects,
    read_agent_output, read_conversation, read_send_output,
};
use super::send::{
    AgentWaitStatus, Body, ForwardPrompt, ForwardRequest, OnBusyPolicy, SendRequest, WaitSource,
    cancel_send, forward, refuse_if_busy, send, wait_agent, wait_send, wait_send_budget,
};
use super::test_support::*;
use super::wait::WaitBudget;
use super::workflow::{
    RunWorkflowRequest, WorkflowRunStatus, describe_workflow, run_workflow, wait_workflow,
};
use super::{OUTPUT_PAGE_CHARS, RESULT_BUDGET_CHARS};

fn text_send(project_id: ProjectId, recipients: Vec<AgentId>, text: &str) -> SendRequest {
    SendRequest {
        project_id,
        recipients,
        body: Body::Text {
            text: text.to_owned(),
        },
        on_busy: OnBusyPolicy::Refuse,
        idempotency_key: None,
    }
}

fn short() -> WaitBudget {
    WaitBudget::new(Some(1), false)
}

/// `send` under the default budget, which is what the tool layer passes.
async fn send_now(
    handle: Arc<dyn super::handle::AppStateHandle>,
    home: std::path::PathBuf,
    request: SendRequest,
) -> Result<super::ledger::SendReceipt, AppError> {
    send(handle, home, request, WaitBudget::default_wait()).await
}

/// An env with one unopened project holding `names` Claude agents.
fn project_with_agents(names: &[&str]) -> (TestEnv, ProjectId, Vec<AgentId>) {
    let mut env = TestEnv::new();
    let project_id = env.project("proj");
    let agents: Vec<AgentId> = names.iter().map(|n| env.agent(project_id, n)).collect();
    (env.restart(), project_id, agents)
}

async fn wait_done(
    env: &TestEnv,
    project_id: ProjectId,
    send_id: Uuid,
) -> super::send::WaitSendReply {
    wait_send(
        &env.state,
        project_id,
        send_id,
        &WaitBudget::new(Some(30), false),
        &env.home(),
    )
    .await
    .unwrap()
}

// --- agents and projects ----------------------------------------------------

#[tokio::test]
async fn agent_lifecycle_on_an_unopened_project() {
    let mut env = TestEnv::new();
    let project_id = env.project("proj");
    let env = env.restart();
    {
        let mut prefs = lock(&env.state.preferences);
        let defaults = prefs
            .agent_defaults
            .get_mut(&switchboard_core::HarnessKind::ClaudeCode)
            .unwrap();
        defaults.default_model = Some("sonnet".to_owned());
        defaults.model_choices = vec!["sonnet".to_owned(), "opus".to_owned()];
    }

    assert!(
        list_agents(&env.state, project_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(lock(&env.state.projects).is_empty(), "a list opens nothing");

    let created = create_agent(
        &env.state,
        project_id,
        "coder",
        switchboard_core::HarnessKind::ClaudeCode,
        None,
        None,
    )
    .unwrap();
    assert_eq!(
        created.model.as_deref(),
        Some("sonnet"),
        "Settings default applied"
    );
    assert_eq!(created.model_choices, vec!["sonnet", "opus"]);

    let renamed = rename_agent(&env.state, project_id, created.id, "builder").unwrap();
    assert_eq!(renamed.name, "builder");

    let changed = set_agent_selection(
        &env.state,
        project_id,
        created.id,
        &SelectionChange::set("haiku"),
        &SelectionChange::unchanged(),
    )
    .unwrap();
    assert_eq!(changed.model.as_deref(), Some("haiku"));
    assert!(
        changed.model_choices.contains(&"haiku".to_owned()),
        "a new value joins the list"
    );

    let reset = set_agent_selection(
        &env.state,
        project_id,
        created.id,
        &SelectionChange::reset(),
        &SelectionChange::unchanged(),
    )
    .unwrap();
    assert_eq!(reset.model, None, "null resets to the harness default");

    let listed = list_agents(&env.state, project_id).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert!(!listed[0].busy);

    let output = read_agent_output(&env.state, project_id, created.id, &env.home())
        .await
        .unwrap();
    assert_eq!(output.status, AgentOutputStatus::None);

    assert_eq!(
        lock(&env.sync.roster_changed).len(),
        4,
        "every change notified the window"
    );
}

#[tokio::test]
async fn create_agent_rejects_an_invalid_selection_as_invalid_argument() {
    let mut env = TestEnv::new();
    let project_id = env.project("proj");
    let env = env.restart();
    lock(&env.state.preferences).agent_defaults.insert(
        switchboard_core::HarnessKind::Antigravity,
        crate::preferences::AgentDefaults::default(),
    );
    let err = create_agent(
        &env.state,
        project_id,
        "x",
        switchboard_core::HarnessKind::Antigravity,
        None,
        Some("high".to_owned()),
    )
    .expect_err("Antigravity's effort is meaningful only with a model");
    assert_eq!(error_code(&err), error_code::INVALID_ARGUMENT);
}

#[tokio::test]
async fn list_projects_reports_busy_and_idle() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let other = {
        let mut env2 = TestEnv::new();
        env2.project("other")
    };
    let _ = other;
    let listed = list_projects(&env.state).await.unwrap();
    assert_eq!(listed[0].status, ProjectStatus::Idle);

    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("working", &gate));
    send_now(
        env.handle(),
        env.home(),
        text_send(project_id, vec![agents[0]], "go"),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, agents[0]).await;
    let listed = list_projects(&env.state).await.unwrap();
    assert_eq!(listed[0].status, ProjectStatus::Busy);
    gate.notify_waiters();
}

// --- sends -------------------------------------------------------------------

#[tokio::test]
async fn fan_out_of_three_shares_one_send_id_and_groups_once() {
    let (env, project_id, agents) = project_with_agents(&["a", "b", "c"]);
    let receipt = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "hi"),
    )
    .await
    .unwrap();
    assert!(
        receipt
            .recipients
            .iter()
            .all(|r| r.status == SendStatus::Accepted)
    );
    let reply = wait_done(&env, project_id, receipt.send_id).await;
    assert!(reply.done);
    assert_eq!(reply.source, WaitSource::Live);
    for recipient in &reply.recipients {
        assert_eq!(recipient.status, SendStatus::Completed);
        assert_eq!(recipient.text.as_deref(), Some("ack: hi"));
    }

    let records = switchboard_core::journal::read_records(&env.journal_path(project_id)).unwrap();
    let sends: Vec<_> = records
        .iter()
        .filter(|r| matches!(r, JournalRecord::Send { send_id, .. } if *send_id == receipt.send_id))
        .collect();
    assert_eq!(
        sends.len(),
        3,
        "one journal send per recipient, one server-minted id"
    );

    let conversation = read_conversation(&env.state, project_id, None, &env.home())
        .await
        .unwrap();
    let user_rows: Vec<_> = conversation
        .items
        .iter()
        .filter_map(|i| match i {
            DigestItem::UserMessage { agent_ids, .. } => Some(agent_ids.len()),
            _ => None,
        })
        .collect();
    assert_eq!(
        user_rows,
        vec![3],
        "the fan-out renders once with three recipients"
    );
    assert!(
        env.emitter
            .snapshot()
            .iter()
            .any(|(_, p)| p["type"] == "user_message"),
        "an ops send announces its user message live"
    );
}

#[tokio::test]
async fn refuse_with_a_busy_recipient_up_front_dispatches_nothing() {
    let (env, project_id, agents) = project_with_agents(&["a", "b"]);
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("busy", &gate));
    crate::commands::open_project_impl(&env.state, project_id).unwrap();
    send_message_impl(
        &env.state,
        agents[0],
        "first",
        Vec::new(),
        Uuid::now_v7(),
        &env.home(),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, agents[0]).await;

    let mut request = text_send(project_id, agents.clone(), "hi");
    request.idempotency_key = Some("k".to_owned());
    let err = send_now(env.handle(), env.home(), request.clone())
        .await
        .expect_err("a busy recipient refuses the whole send");
    assert!(
        matches!(err.root(), AppError::RecipientsBusy { names } if names == &["a".to_owned()]),
        "{err:?}"
    );
    assert_eq!(lock(&env.adapter.prompts).len(), 1, "b was not dispatched");

    gate.notify_waiters();
    wait_agent(
        &env.state,
        project_id,
        agents[0],
        &WaitBudget::new(Some(30), false),
        &env.home(),
    )
    .await
    .unwrap();
    let receipt = send_now(env.handle(), env.home(), request)
        .await
        .expect("the key was released by the refusal");
    assert!(
        receipt
            .recipients
            .iter()
            .all(|r| r.status == SendStatus::Accepted)
    );
}

#[tokio::test]
async fn refuse_stops_at_the_first_refusal_after_the_check() {
    let (env, project_id, agents) = project_with_agents(&["a", "b", "c"]);
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("a-running", &gate));
    env.adapter.push(Script::parked("b-busy", &gate));
    let barrier = env.seam(TestSeam::OpsDispatch);
    let task = tokio::spawn(send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "hi"),
    ));
    // Before a: let it through.
    barrier.entered.notified().await;
    barrier.release.notify_one();
    // Before b: make b busy now, after the up-front check passed.
    barrier.entered.notified().await;
    send_message_impl(
        &env.state,
        agents[1],
        "compose",
        Vec::new(),
        Uuid::now_v7(),
        &env.home(),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, agents[1]).await;
    env.clear_seam(TestSeam::OpsDispatch);
    barrier.release.notify_one();

    let receipt = task.await.unwrap().unwrap();
    assert_eq!(receipt.recipients[0].status, SendStatus::Accepted);
    assert_eq!(receipt.recipients[1].status, SendStatus::Rejected);
    assert_eq!(receipt.recipients[1].code, Some(error_code::AGENT_BUSY));
    assert_eq!(receipt.recipients[2].status, SendStatus::NotDispatched);
    assert!(
        env.state
            .dispatcher
            .running_turn_kind(agents[0])
            .await
            .is_some(),
        "the accepted turn keeps running"
    );
    gate.notify_waiters();
}

#[tokio::test]
async fn an_immediate_follow_up_after_wait_send_is_accepted() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let first = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "one"),
    )
    .await
    .unwrap();
    assert!(wait_done(&env, project_id, first.send_id).await.done);
    let second = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "two"),
    )
    .await
    .unwrap();
    assert_eq!(second.recipients[0].status, SendStatus::Accepted);
    assert!(wait_done(&env, project_id, second.send_id).await.done);
}

#[tokio::test]
async fn queue_behind_a_running_turn_completes_and_not_done_replies_carry_no_text() {
    let (env, project_id, agents) = project_with_agents(&["a", "b"]);
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("slow", &gate));
    let mut request = text_send(project_id, agents.clone(), "hi");
    request.on_busy = OnBusyPolicy::Queue;
    let first = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, vec![agents[0]], "first"),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, agents[0]).await;
    let queued = send_now(env.handle(), env.home(), request).await.unwrap();
    assert!(
        queued
            .recipients
            .iter()
            .all(|r| r.status == SendStatus::Accepted)
    );

    let partial = wait_send(
        &env.state,
        project_id,
        queued.send_id,
        &short(),
        &env.home(),
    )
    .await
    .unwrap();
    assert!(!partial.done);
    let b = partial
        .recipients
        .iter()
        .find(|r| r.agent_id == agents[1])
        .unwrap();
    assert_eq!(b.status, SendStatus::Completed);
    assert!(b.text.is_none(), "a not-done reply carries statuses only");

    gate.notify_waiters();
    assert!(wait_done(&env, project_id, first.send_id).await.done);
    let reply = wait_done(&env, project_id, queued.send_id).await;
    assert!(reply.done);
    let a = reply
        .recipients
        .iter()
        .find(|r| r.agent_id == agents[0])
        .unwrap();
    assert_eq!(a.text.as_deref(), Some("ack: hi"));
}

#[tokio::test]
async fn a_saved_prompt_body_renders_with_its_appended_text() {
    let mut env = TestEnv::new().with_prompts_and_workflows();
    std::fs::write(env.prompts_dir().join("greet.md"), GREET_PROMPT).unwrap();
    env.state.prompts.sync().await;
    let project_id = env.project("proj");
    let agent = env.agent(project_id, "a");
    let request = SendRequest {
        project_id,
        recipients: vec![agent],
        body: Body::Prompt {
            provider: "local".to_owned(),
            name: "greet".to_owned(),
            args: BTreeMap::from([("who".to_owned(), "bob".to_owned())]),
            appended_text: Some("  thanks  ".to_owned()),
        },
        on_busy: OnBusyPolicy::Refuse,
        idempotency_key: None,
    };
    let receipt = send_now(env.handle(), env.home(), request).await.unwrap();
    wait_done(&env, project_id, receipt.send_id).await;
    assert_eq!(lock(&env.adapter.prompts)[0], "Hi bob\n\nthanks");
}

// --- idempotency -------------------------------------------------------------

#[tokio::test]
async fn a_sequential_repeat_returns_the_stored_receipt() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let mut request = text_send(project_id, agents, "hi");
    request.idempotency_key = Some("k1".to_owned());
    let first = send_now(env.handle(), env.home(), request.clone())
        .await
        .unwrap();
    wait_done(&env, project_id, first.send_id).await;
    let again = send_now(env.handle(), env.home(), request).await.unwrap();
    assert_eq!(again.send_id, first.send_id);
    assert_eq!(lock(&env.adapter.prompts).len(), 1, "never sent twice");
}

#[tokio::test]
async fn two_concurrent_repeats_dispatch_once() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let mut request = text_send(project_id, agents, "hi");
    request.idempotency_key = Some("k2".to_owned());
    let (one, two) = tokio::join!(
        send_now(env.handle(), env.home(), request.clone()),
        send_now(env.handle(), env.home(), request.clone()),
    );
    let (one, two) = (one.unwrap(), two.unwrap());
    assert_eq!(one.send_id, two.send_id);
    wait_done(&env, project_id, one.send_id).await;
    assert_eq!(lock(&env.adapter.prompts).len(), 1);
}

#[tokio::test]
async fn the_same_key_with_a_different_request_is_invalid_argument() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let mut request = text_send(project_id, agents, "hi");
    request.idempotency_key = Some("k3".to_owned());
    send_now(env.handle(), env.home(), request.clone())
        .await
        .unwrap();
    request.body = Body::Text {
        text: "different".to_owned(),
    };
    let err = send_now(env.handle(), env.home(), request)
        .await
        .unwrap_err();
    assert_eq!(error_code(&err), error_code::INVALID_ARGUMENT);
}

#[tokio::test]
async fn the_same_key_in_another_project_does_not_collide() {
    let mut env = TestEnv::new();
    let p1 = env.project("one");
    let p2 = env.project("two");
    let a1 = env.agent(p1, "a");
    let a2 = env.agent(p2, "a");
    let env = env.restart();
    let mut r1 = text_send(p1, vec![a1], "hi");
    r1.idempotency_key = Some("shared".to_owned());
    let mut r2 = text_send(p2, vec![a2], "hi");
    r2.idempotency_key = Some("shared".to_owned());
    let one = send_now(env.handle(), env.home(), r1).await.unwrap();
    let two = send_now(env.handle(), env.home(), r2).await.unwrap();
    assert_ne!(one.send_id, two.send_id);
}

#[tokio::test]
async fn a_request_level_error_releases_the_key() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let mut bad = text_send(project_id, vec![Uuid::now_v7()], "hi");
    bad.idempotency_key = Some("k4".to_owned());
    let err = send_now(env.handle(), env.home(), bad).await.unwrap_err();
    assert_eq!(error_code(&err), error_code::UNKNOWN_RECIPIENT);
    let mut good = text_send(project_id, agents, "hi");
    good.idempotency_key = Some("k4".to_owned());
    send_now(env.handle(), env.home(), good)
        .await
        .expect("the key was released, so a different request may use it");
}

#[tokio::test]
async fn dropping_the_caller_mid_fan_out_still_finishes_every_recipient() {
    let (env, project_id, agents) = project_with_agents(&["a", "b"]);
    let barrier = env.seam(TestSeam::OpsDispatch);
    let mut request = text_send(project_id, agents.clone(), "hi");
    request.idempotency_key = Some("k5".to_owned());
    let caller = tokio::spawn(send_now(env.handle(), env.home(), request.clone()));
    barrier.entered.notified().await;
    barrier.release.notify_one();
    // Parked before b: a was submitted, b was not.
    barrier.entered.notified().await;
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    env.clear_seam(TestSeam::OpsDispatch);
    barrier.release.notify_one();

    let receipt = send_now(env.handle(), env.home(), request)
        .await
        .expect("a retry with the key joins the finished send");
    assert!(
        receipt
            .recipients
            .iter()
            .all(|r| r.status == SendStatus::Accepted)
    );
    let reply = wait_done(&env, project_id, receipt.send_id).await;
    assert!(
        reply
            .recipients
            .iter()
            .all(|r| r.status == SendStatus::Completed)
    );
    assert_eq!(lock(&env.adapter.prompts).len(), 2);
}

// --- forwards ---------------------------------------------------------------

#[tokio::test]
async fn forward_from_a_source_in_a_second_unopened_project() {
    let mut env = TestEnv::new();
    let p1 = env.project("one");
    let p2 = env.project("two");
    let recipient = env.agent(p1, "r");
    let source = env.agent(p2, "src");
    env.seed_claude_turn(p2, source, "SOURCE OUTPUT", "m1");
    let env = env.restart();
    let request = ForwardRequest {
        project_id: p1,
        sources: vec![ForwardSourceRef {
            agent_id: source,
            project_id: p2,
        }],
        recipients: vec![recipient],
        text: Some("Please review:".to_owned()),
        prompt: None,
        on_busy: OnBusyPolicy::Refuse,
        idempotency_key: None,
    };
    let receipt = forward(
        env.handle(),
        env.home(),
        request,
        WaitBudget::default_wait(),
    )
    .await
    .unwrap();
    wait_done(&env, p1, receipt.send_id).await;
    let prompt = lock(&env.adapter.prompts)[0].clone();
    assert_eq!(
        prompt,
        "Please review:\n\n=== START forwarded from two · src ===\nSOURCE OUTPUT\n=== END forwarded from two · src ==="
    );
}

#[tokio::test(start_paused = true)]
async fn forward_with_a_mid_turn_source_past_the_budget_is_source_busy() {
    let (env, project_id, agents) = project_with_agents(&["src", "r"]);
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("thinking", &gate));
    crate::commands::open_project_impl(&env.state, project_id).unwrap();
    send_message_impl(
        &env.state,
        agents[0],
        "go",
        Vec::new(),
        Uuid::now_v7(),
        &env.home(),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, agents[0]).await;
    let request = ForwardRequest {
        project_id,
        sources: vec![ForwardSourceRef {
            agent_id: agents[0],
            project_id,
        }],
        recipients: vec![agents[1]],
        text: None,
        prompt: None,
        on_busy: OnBusyPolicy::Refuse,
        idempotency_key: Some("fwd".to_owned()),
    };
    let err = forward(
        env.handle(),
        env.home(),
        request.clone(),
        WaitBudget::new(Some(2), false),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err.root(), AppError::SourceBusy { name } if name == "src"),
        "{err:?}"
    );
    assert_eq!(lock(&env.adapter.prompts).len(), 1, "nothing dispatched");

    gate.notify_waiters();
    wait_agent(
        &env.state,
        project_id,
        agents[0],
        &WaitBudget::new(Some(30), false),
        &env.home(),
    )
    .await
    .unwrap();
    forward(
        env.handle(),
        env.home(),
        request,
        WaitBudget::new(Some(2), false),
    )
    .await
    .expect("the key was released, so the natural retry succeeds");
}

const GREET_PROMPT: &str = "---\nname: greet\ndescription: Greeting.\narguments:\n  - name: who\n    required: true\n---\nHi {{ who }}\n";
const REVIEW_PROMPT: &str = "---\nname: review\ndescription: Review.\narguments:\n  - name: diff\n    required: true\n---\nReview this:\n{{ diff }}\n";

#[tokio::test]
async fn forward_into_a_named_prompt_argument() {
    let mut env = TestEnv::new().with_prompts_and_workflows();
    std::fs::write(env.prompts_dir().join("review.md"), REVIEW_PROMPT).unwrap();
    env.state.prompts.sync().await;
    let project_id = env.project("proj");
    let source = env.agent(project_id, "coder");
    let recipient = env.agent(project_id, "reviewer");
    env.seed_claude_turn(project_id, source, "THE DIFF", "m1");
    let request = ForwardRequest {
        project_id,
        sources: Vec::new(),
        recipients: vec![recipient],
        text: None,
        prompt: Some(ForwardPrompt {
            provider: "local".to_owned(),
            name: "review".to_owned(),
            args: BTreeMap::new(),
            forward_args: BTreeMap::from([(
                "diff".to_owned(),
                vec![ForwardSourceRef {
                    agent_id: source,
                    project_id,
                }],
            )]),
        }),
        on_busy: OnBusyPolicy::Refuse,
        idempotency_key: None,
    };
    let receipt = forward(
        env.handle(),
        env.home(),
        request,
        WaitBudget::default_wait(),
    )
    .await
    .unwrap();
    wait_done(&env, project_id, receipt.send_id).await;
    let prompt = lock(&env.adapter.prompts)[0].clone();
    assert!(
        prompt.starts_with("Review this:\n=== START forwarded from coder ===\nTHE DIFF"),
        "{prompt}"
    );

    let mut unknown = ForwardRequest {
        project_id,
        sources: Vec::new(),
        recipients: vec![recipient],
        text: None,
        prompt: None,
        on_busy: OnBusyPolicy::Refuse,
        idempotency_key: None,
    };
    let err = forward(
        env.handle(),
        env.home(),
        unknown.clone(),
        WaitBudget::default_wait(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, AppError::NoForwardSources));
    unknown.prompt = Some(ForwardPrompt {
        provider: "local".to_owned(),
        name: "review".to_owned(),
        args: BTreeMap::new(),
        forward_args: BTreeMap::from([(
            "nope".to_owned(),
            vec![ForwardSourceRef {
                agent_id: source,
                project_id,
            }],
        )]),
    });
    let err = forward(
        env.handle(),
        env.home(),
        unknown,
        WaitBudget::default_wait(),
    )
    .await
    .unwrap_err();
    assert_eq!(error_code(&err), error_code::INVALID_ARGUMENT);
}

// --- waits ----------------------------------------------------------------------

#[tokio::test]
async fn wait_agent_across_a_running_turn_with_one_queued_returns_running_until_both_finish() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let gate1 = Arc::new(Notify::new());
    let gate2 = Arc::new(Notify::new());
    env.adapter.push(Script::parked("first", &gate1));
    env.adapter.push(Script::parked("second", &gate2));
    crate::commands::open_project_impl(&env.state, project_id).unwrap();
    for text in ["one", "two"] {
        send_message_impl(
            &env.state,
            agents[0],
            text,
            Vec::new(),
            Uuid::now_v7(),
            &env.home(),
        )
        .await
        .unwrap();
    }
    await_turn_running(&env.state, agents[0]).await;
    let home = env.home();
    let budget = short();
    let status = wait_agent(&env.state, project_id, agents[0], &budget, &home)
        .await
        .unwrap()
        .status;
    assert_eq!(status, AgentWaitStatus::Running);
    gate1.notify_waiters();
    let status = wait_agent(&env.state, project_id, agents[0], &budget, &home)
        .await
        .unwrap()
        .status;
    assert_eq!(
        status,
        AgentWaitStatus::Running,
        "the queued turn runs next"
    );
    gate2.notify_waiters();
    let reply = wait_agent(
        &env.state,
        project_id,
        agents[0],
        &WaitBudget::new(Some(30), false),
        &env.home(),
    )
    .await
    .unwrap();
    assert_eq!(reply.status, AgentWaitStatus::Idle);
    let output = reply.output.unwrap();
    assert_eq!(output.status, AgentOutputStatus::Completed);
    assert_eq!(output.source, OutputSource::DurableLink);
}

#[tokio::test(start_paused = true)]
async fn wait_agent_on_a_closing_agent_returns_running_at_the_timeout_without_spinning() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let gate = Arc::new(Notify::new());
    let mut script = Script::parked("stuck", &gate);
    script.ignore_cancel = true;
    env.adapter.push(script);
    crate::commands::open_project_impl(&env.state, project_id).unwrap();
    send_message_impl(
        &env.state,
        agents[0],
        "go",
        Vec::new(),
        Uuid::now_v7(),
        &env.home(),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, agents[0]).await;
    let dispatcher = Arc::clone(&env.state.dispatcher);
    let agent = agents[0];
    let shutdown = tokio::spawn(async move {
        dispatcher
            .shutdown_agent(agent, CancelSource::Shutdown)
            .await;
    });
    tokio::task::yield_now().await;
    let started = tokio::time::Instant::now();
    let reply = wait_agent(&env.state, project_id, agents[0], &short(), &env.home())
        .await
        .unwrap();
    assert_eq!(reply.status, AgentWaitStatus::Running);
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(1),
        "ended at the deadline"
    );
    gate.notify_waiters();
    shutdown.await.unwrap();
    let reply = wait_agent(&env.state, project_id, agents[0], &short(), &env.home())
        .await
        .unwrap();
    assert_eq!(reply.status, AgentWaitStatus::Idle);
}

#[tokio::test(start_paused = true)]
async fn wait_agent_rechecks_at_the_deadline() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let gate = Arc::new(Notify::new());
    let mut script = Script::parked("stuck", &gate);
    script.ignore_cancel = true;
    env.adapter.push(script);
    crate::commands::open_project_impl(&env.state, project_id).unwrap();
    send_message_impl(
        &env.state,
        agents[0],
        "go",
        Vec::new(),
        Uuid::now_v7(),
        &env.home(),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, agents[0]).await;
    let dispatcher = Arc::clone(&env.state.dispatcher);
    let agent = agents[0];
    tokio::spawn(async move {
        dispatcher
            .shutdown_agent(agent, CancelSource::Shutdown)
            .await;
    });
    let releaser = Arc::clone(&gate);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        releaser.notify_waiters();
    });
    let reply = wait_agent(&env.state, project_id, agents[0], &short(), &env.home())
        .await
        .unwrap();
    assert_eq!(
        reply.status,
        AgentWaitStatus::Idle,
        "the shutdown finished before the deadline, so the final observation sees idle"
    );
}

#[tokio::test]
async fn cancel_send_stops_running_recipients() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("long", &gate));
    let receipt = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "go"),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, agents[0]).await;
    let cancelled = cancel_send(&env.state, project_id, receipt.send_id).unwrap();
    assert_eq!(cancelled.recipients, agents);
    let reply = wait_done(&env, project_id, receipt.send_id).await;
    assert_eq!(reply.recipients[0].status, SendStatus::Cancelled);
    let err = cancel_send(&env.state, project_id, Uuid::now_v7()).unwrap_err();
    assert_eq!(error_code(&err), error_code::SEND_UNKNOWN);
}

// --- outputs and the budget ------------------------------------------------------

#[tokio::test]
async fn read_send_output_pages_one_reply_stably_while_the_agent_starts_a_new_turn() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let long = "x".repeat(OUTPUT_PAGE_CHARS * 2 + 5);
    env.adapter.push(Script::reply(&long));
    let receipt = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "go"),
    )
    .await
    .unwrap();
    wait_done(&env, project_id, receipt.send_id).await;
    let first = read_send_output(
        &env.state,
        project_id,
        receipt.send_id,
        agents[0],
        0,
        &env.home(),
    )
    .await
    .unwrap();
    assert_eq!(first.text.len(), OUTPUT_PAGE_CHARS);
    assert_eq!(first.total_chars, long.len());
    assert_eq!(first.next_offset, Some(OUTPUT_PAGE_CHARS));
    assert_eq!(first.source, OutputSource::Live);

    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("newer", &gate));
    send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "again"),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, agents[0]).await;

    let last = read_send_output(
        &env.state,
        project_id,
        receipt.send_id,
        agents[0],
        OUTPUT_PAGE_CHARS * 2,
        &env.home(),
    )
    .await
    .unwrap();
    assert_eq!(last.text, "xxxxx");
    assert_eq!(last.next_offset, None);
    gate.notify_waiters();
}

#[tokio::test]
async fn a_three_recipient_fan_out_with_long_replies_stays_under_the_budget() {
    let (env, project_id, agents) = project_with_agents(&["a", "b", "c"]);
    for _ in 0..3 {
        env.adapter.push(Script::reply(&"y".repeat(15_000)));
    }
    let receipt = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "go"),
    )
    .await
    .unwrap();
    let reply = wait_done(&env, project_id, receipt.send_id).await;
    let fitted = fit(
        serde_json::to_value(&reply).unwrap(),
        &wait_send_budget(project_id, receipt.send_id),
    )
    .unwrap();
    assert!(fitted.serialized_chars() <= RESULT_BUDGET_CHARS);
    assert_eq!(fitted.truncated.len(), 3);
    for cut in &fitted.truncated {
        assert_eq!(cut.total_chars, 15_000);
        let read_with = cut.read_with.as_ref().unwrap();
        assert!(read_with["agent_id"].is_string());
        assert_eq!(read_with["send_id"], receipt.send_id.to_string());
        assert_eq!(
            read_with["offset"], cut.kept_chars,
            "a page reads on where the cut ended"
        );
    }
    let kept: Vec<usize> = fitted.result["recipients"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["text"].as_str().unwrap().len())
        .collect();
    assert!(
        kept.iter().all(|k| *k == kept[0]),
        "an even split: {kept:?}"
    );
    let recipients = fitted.result["recipients"].as_array().unwrap();
    for (recipient, agent) in recipients.iter().zip(&agents) {
        assert_eq!(
            recipient["agent_id"],
            agent.to_string(),
            "ids survive the cut"
        );
        assert_eq!(recipient["status"], "completed");
    }
}

// --- restart fallback ----------------------------------------------------------

#[tokio::test]
async fn the_restart_fallback_answers_from_the_journal() {
    let mut env = TestEnv::new();
    let _other = env.project("other");
    let project_id = env.project("proj");
    let a = env.agent(project_id, "a");
    let b = env.agent(project_id, "b");
    let c = env.agent(project_id, "c");
    let (s_a, s_b, s_c) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    // The sends predate the seeded turns, which are stamped now, so each turn
    // falls inside its send's dispatch window as a real turn would.
    let now = Utc::now() - chrono::Duration::minutes(1);
    let journal = env.journal_path(project_id);
    let write = |record: JournalRecord| {
        switchboard_core::journal::append_record(&journal, &record).unwrap();
    };
    let send_record = |send_id, turn_id, agent_id| JournalRecord::Send {
        send_id,
        turn_id,
        agent_id,
        prompt: "hi".to_owned(),
        attachments: Vec::new(),
        at: now,
    };
    let turn_a = Uuid::now_v7();
    write(send_record(s_a, turn_a, a));
    write(JournalRecord::TurnLink {
        send_id: s_a,
        turn_id: turn_a,
        agent_id: a,
        hydration_key: "m1".to_owned(),
        at: now,
    });
    let turn_b = Uuid::now_v7();
    write(send_record(s_b, turn_b, b));
    write(JournalRecord::TurnLink {
        send_id: s_b,
        turn_id: turn_b,
        agent_id: b,
        hydration_key: "m2".to_owned(),
        at: now,
    });
    write(JournalRecord::Outcome {
        send_id: s_b,
        turn_id: turn_b,
        agent_id: b,
        outcome: serde_json::to_value(TurnOutcome::Failed {
            kind: switchboard_harness::FailureKind::HarnessError,
            message: "boom".to_owned(),
        })
        .unwrap(),
        started_at: now,
        ended_at: now,
    });
    write(send_record(s_c, Uuid::now_v7(), c));
    env.seed_claude_turn(project_id, a, "DONE", "m1");
    env.seed_claude_turn(project_id, b, "partial", "m2");
    let env = env.restart();

    let reply = wait_done(&env, project_id, s_a).await;
    assert_eq!(reply.source, WaitSource::Journal);
    assert_eq!(reply.recipients[0].status, SendStatus::Completed);
    assert_eq!(reply.recipients[0].text.as_deref(), Some("DONE"));

    let reply = wait_done(&env, project_id, s_b).await;
    assert_eq!(reply.recipients[0].status, SendStatus::Failed);
    assert_eq!(reply.recipients[0].error.as_deref(), Some("boom"));

    let reply = wait_done(&env, project_id, s_c).await;
    assert_eq!(reply.recipients[0].status, SendStatus::Started);

    let reply = wait_done(&env, project_id, Uuid::now_v7()).await;
    assert_eq!(reply.source, WaitSource::Unknown);
    assert!(reply.recipients.is_empty());

    let page = read_send_output(&env.state, project_id, s_a, a, 0, &env.home())
        .await
        .unwrap();
    assert_eq!(page.text, "DONE");
    assert_eq!(page.source, OutputSource::DurableLink);
}

// --- workflows ---------------------------------------------------------------------

const PAIR_WORKFLOW: &str = "name: pair
description: test
inputs:
  coder:
    type: agent
  reviewers:
    type: [agent]
steps:
  - label: Greet
    send:
      to: \"{{ coder }}\"
      prompt: \"local:greet\"
  - label: Greeted
    wait_for:
      agent: \"{{ coder }}\"
  - label: Review
    send:
      to: \"{{ reviewers }}\"
      prompt: \"local:greet\"
  - label: Reviewed
    wait_for_all:
      agents: \"{{ reviewers }}\"
";

async fn workflow_env() -> (TestEnv, ProjectId, Vec<AgentId>) {
    let mut env = TestEnv::new().with_prompts_and_workflows();
    std::fs::write(env.prompts_dir().join("greet.md"), GREET_PROMPT).unwrap();
    std::fs::write(env.workflows_dir().join("pair.yaml"), PAIR_WORKFLOW).unwrap();
    env.state.prompts.sync().await;
    let project_id = env.project("proj");
    let agents = ["coder", "r1", "r2"]
        .iter()
        .map(|n| env.agent(project_id, n))
        .collect();
    (env, project_id, agents)
}

fn pair_request(project_id: ProjectId, agents: &[AgentId]) -> RunWorkflowRequest {
    RunWorkflowRequest {
        project_id,
        name: "pair".to_owned(),
        is_builtin: false,
        inputs: BTreeMap::from([
            ("coder".to_owned(), InputValue::Text(agents[0].to_string())),
            (
                "reviewers".to_owned(),
                InputValue::List(vec![agents[1].to_string(), agents[2].to_string()]),
            ),
            ("who".to_owned(), InputValue::Text("bob".to_owned())),
        ]),
        forward_sources: BTreeMap::new(),
    }
}

#[tokio::test]
async fn describe_run_and_wait_a_workflow_with_agent_ids() {
    let (env, project_id, agents) = workflow_env().await;
    let form = describe_workflow(&env.state, project_id, "pair", false)
        .await
        .unwrap();
    let who = form.derived_args.iter().find(|a| a.name == "who").unwrap();
    assert!(
        who.required,
        "a required argument derived from the saved prompt"
    );

    let run_id = run_workflow(&env.state, &env.home(), pair_request(project_id, &agents))
        .await
        .unwrap();
    let reply = wait_workflow(
        &env.state,
        project_id,
        run_id,
        &WaitBudget::new(Some(30), false),
    )
    .await
    .unwrap();
    assert_eq!(reply.status, WorkflowRunStatus::Completed);
    assert!(reply.done);
    let run_file = lock(&env.state.projects)
        .get(&project_id)
        .unwrap()
        .run_path(run_id);
    assert!(
        !run_file.exists(),
        "a completed run's file is pruned, yet the ledger answered"
    );
    assert_eq!(lock(&env.adapter.prompts).len(), 3);

    let foreign = Uuid::now_v7();
    let mut bad = pair_request(project_id, &agents);
    bad.inputs
        .insert("coder".to_owned(), InputValue::Text(foreign.to_string()));
    let err = run_workflow(&env.state, &env.home(), bad)
        .await
        .unwrap_err();
    assert_eq!(error_code(&err), error_code::UNKNOWN_RECIPIENT);
}

#[tokio::test]
async fn a_failed_run_blocks_until_dismissed() {
    let (env, project_id, agents) = workflow_env().await;
    env.adapter.push(Script::failing("nope"));
    let run_id = run_workflow(&env.state, &env.home(), pair_request(project_id, &agents))
        .await
        .unwrap();
    let reply = wait_workflow(
        &env.state,
        project_id,
        run_id,
        &WaitBudget::new(Some(30), false),
    )
    .await
    .unwrap();
    assert_eq!(reply.status, WorkflowRunStatus::Failed);

    let err = run_workflow(&env.state, &env.home(), pair_request(project_id, &agents))
        .await
        .unwrap_err();
    assert_eq!(
        error_code(&err),
        error_code::WORKFLOW_RUN_REQUIRES_DISMISSAL
    );

    let dismissed = dismiss_workflow_run(&env.state, project_id, run_id).unwrap();
    assert_eq!(dismissed.status, "failed");
    // The send is accepted; the harness failure surfaces at the wait step.
    assert_eq!(dismissed.step, 1);
    assert!(dismissed.reason.is_some());

    let rerun = run_workflow(&env.state, &env.home(), pair_request(project_id, &agents))
        .await
        .unwrap();
    let reply = wait_workflow(
        &env.state,
        project_id,
        rerun,
        &WaitBudget::new(Some(30), false),
    )
    .await
    .unwrap();
    assert_eq!(reply.status, WorkflowRunStatus::Completed);
}

#[tokio::test]
async fn dismissing_a_live_or_completed_run_is_refused() {
    let (env, project_id, agents) = workflow_env().await;
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("working", &gate));
    let run_id = run_workflow(&env.state, &env.home(), pair_request(project_id, &agents))
        .await
        .unwrap();
    await_turn_running(&env.state, agents[0]).await;
    let err = dismiss_workflow_run(&env.state, project_id, run_id).unwrap_err();
    assert!(matches!(err, AppError::WorkflowRunActive { .. }));
    gate.notify_waiters();
    wait_workflow(
        &env.state,
        project_id,
        run_id,
        &WaitBudget::new(Some(30), false),
    )
    .await
    .unwrap();

    let completed = Uuid::now_v7();
    let project = lock(&env.state.projects).get(&project_id).unwrap().clone();
    let now = Utc::now();
    for record in [
        RunRecord::Started {
            workflow: "pair".to_owned(),
            total_steps: 1,
            steps: Vec::new(),
            at: now,
        },
        RunRecord::Terminal {
            status: TerminalStatus::Complete,
            failed_step: None,
            reason: None,
            at: now,
        },
    ] {
        switchboard_core::append_jsonl(&project.run_path(completed), &record).unwrap();
    }
    let err = dismiss_workflow_run(&env.state, project_id, completed).unwrap_err();
    assert_eq!(error_code(&err), error_code::INVALID_ARGUMENT);
    assert!(
        project.run_path(completed).exists(),
        "a completed file is never deleted here"
    );
    let err = dismiss_workflow_run(&env.state, project_id, Uuid::now_v7()).unwrap_err();
    assert_eq!(error_code(&err), error_code::RUN_UNKNOWN);
}

// --- refusals and codes --------------------------------------------------------

#[tokio::test]
async fn a_missing_working_directory_refuses_the_send() {
    let (mut env, project_id, agents) = project_with_agents(&["a"]);
    let dir = env.dirs.remove(0);
    let path = dir.path().to_path_buf();
    drop(dir);
    assert!(!path.exists());
    let err = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents, "hi"),
    )
    .await
    .unwrap_err();
    assert_eq!(error_code(&err), error_code::DIRECTORY_MISSING);
}

#[tokio::test]
async fn a_recipient_the_window_cannot_hear_is_rejected_while_the_others_are_sent() {
    let (env, project_id, agents) = project_with_agents(&["a", "b"]);
    *lock(&env.sync.unsubscribed) = vec![agents[1]];
    let receipt = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "hi"),
    )
    .await
    .unwrap();
    assert_eq!(receipt.recipients[0].status, SendStatus::Accepted);
    assert_eq!(receipt.recipients[1].status, SendStatus::Rejected);
    assert_eq!(
        receipt.recipients[1].code,
        Some(error_code::DESKTOP_UNAVAILABLE)
    );
    assert_eq!(*lock(&env.sync.ready_calls), vec![project_id]);
}

#[tokio::test]
async fn a_window_that_does_not_answer_dispatches_nothing() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    *lock(&env.sync.times_out) = true;
    let mut request = text_send(project_id, agents, "hi");
    request.idempotency_key = Some("k".to_owned());
    let err = send_now(env.handle(), env.home(), request.clone())
        .await
        .unwrap_err();
    assert_eq!(error_code(&err), error_code::PROJECT_LOAD_TIMEOUT);
    assert!(lock(&env.adapter.prompts).is_empty());
    *lock(&env.sync.times_out) = false;
    send_now(env.handle(), env.home(), request)
        .await
        .expect("the key was released");
}

#[tokio::test]
async fn an_oversized_body_is_refused_before_dispatch() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let body = "z".repeat(switchboard_harness::MAX_PROMPT_BYTES + 1);
    let err = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents, &body),
    )
    .await
    .unwrap_err();
    assert_eq!(error_code(&err), error_code::PROMPT_TOO_LARGE);
    assert!(lock(&env.adapter.prompts).is_empty());
}

#[test]
fn the_error_code_mapping_covers_the_cases_a_caller_acts_on() {
    assert_eq!(
        error_code(&AppError::SessionInUse),
        error_code::AGENT_BUSY_ELSEWHERE
    );
    assert_eq!(
        error_code(&AppError::Prompt(
            switchboard_prompts::PromptError::McpNeedsAuth {
                provider: "tiddly".to_owned()
            }
        )),
        error_code::PROMPT_NEEDS_SIGN_IN
    );
    assert_eq!(error_code(&AppError::NoActiveProject), error_code::INTERNAL);
    assert_eq!(
        error_code(&AppError::TimedOut {
            what: "rendering the saved prompt"
        }),
        error_code::INTERNAL,
        "a slow provider is not the caller's bad argument"
    );
    assert_eq!(
        error_code(&AppError::Shared(Arc::new(AppError::SourceBusy {
            name: "x".to_owned()
        }))),
        error_code::SOURCE_BUSY
    );
    assert_eq!(
        error_code(&AppError::ForwardSourceMoved {
            agent_id: Uuid::now_v7(),
            declared: Uuid::now_v7(),
            actual: Uuid::now_v7()
        }),
        error_code::AGENT_NOT_FOUND
    );
}

// --- the end-to-end proof, on the files --------------------------------------------

#[tokio::test]
async fn create_send_forward_and_read_leave_the_expected_files() {
    let env = TestEnv::new();
    let dir = tempfile::TempDir::new().unwrap();
    let project = create_project(&env.state, "proof", dir.path().to_str().unwrap()).unwrap();
    assert_eq!(*lock(&env.sync.projects_changed), 1);
    let coder = create_agent(
        &env.state,
        project.id,
        "coder",
        switchboard_core::HarnessKind::ClaudeCode,
        None,
        None,
    )
    .unwrap();
    let reviewer = create_agent(
        &env.state,
        project.id,
        "reviewer",
        switchboard_core::HarnessKind::ClaudeCode,
        None,
        None,
    )
    .unwrap();

    let first = send_now(
        env.handle(),
        env.home(),
        text_send(project.id, vec![coder.id, reviewer.id], "write a haiku"),
    )
    .await
    .unwrap();
    wait_done(&env, project.id, first.send_id).await;

    let second = forward(
        env.handle(),
        env.home(),
        ForwardRequest {
            project_id: project.id,
            sources: vec![ForwardSourceRef {
                agent_id: coder.id,
                project_id: project.id,
            }],
            recipients: vec![reviewer.id],
            text: Some("critique this".to_owned()),
            prompt: None,
            on_busy: OnBusyPolicy::Refuse,
            idempotency_key: None,
        },
        WaitBudget::default_wait(),
    )
    .await
    .unwrap();
    let reply = wait_done(&env, project.id, second.send_id).await;
    assert_eq!(reply.recipients[0].status, SendStatus::Completed);
    let page = read_send_output(
        &env.state,
        project.id,
        second.send_id,
        reviewer.id,
        0,
        &env.home(),
    )
    .await
    .unwrap();
    assert!(page.text.contains("ack: critique this"));

    // The files, not the return values.
    let records = switchboard_core::journal::read_records(&env.journal_path(project.id)).unwrap();
    let mut send_ids: Vec<Uuid> = records
        .iter()
        .filter_map(|r| match r {
            JournalRecord::Send { send_id, .. } => Some(*send_id),
            _ => None,
        })
        .collect();
    send_ids.dedup();
    assert_eq!(
        send_ids,
        vec![first.send_id, second.send_id],
        "two sends, server-minted ids"
    );
    let registry = env.state.store.list_project_agents(project.id).unwrap();
    assert_eq!(registry.len(), 2);
    let conversation = read_conversation(&env.state, project.id, None, &env.home())
        .await
        .unwrap();
    let groups: Vec<(Uuid, usize)> = conversation
        .items
        .iter()
        .filter_map(|i| match i {
            DigestItem::UserMessage {
                send_id: Some(s),
                agent_ids,
                ..
            } => Some((*s, agent_ids.len())),
            _ => None,
        })
        .collect();
    assert_eq!(groups, vec![(first.send_id, 2), (second.send_id, 1)]);
    let prompts = lock(&env.adapter.prompts);
    assert!(prompts[2].contains("=== START forwarded from coder ==="));
}

/// A wait budget's cancel token ends a wait at once; exercised here against
/// a real send so the signal path is the production one.
#[tokio::test(start_paused = true)]
async fn the_cancellation_signal_ends_a_wait_at_once() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("long", &gate));
    let receipt = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents, "go"),
    )
    .await
    .unwrap();
    let cancel = CancellationToken::new();
    let budget = WaitBudget::new(Some(3_600), true).with_cancel(cancel.clone());
    let canceller = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        canceller.cancel();
    });
    let started = tokio::time::Instant::now();
    let reply = wait_send(
        &env.state,
        project_id,
        receipt.send_id,
        &budget,
        &env.home(),
    )
    .await
    .unwrap();
    assert!(!reply.done);
    assert_eq!(started.elapsed(), Duration::from_secs(5));
    gate.notify_waiters();
}

// --- review round two: the cases the first implementation got wrong ----------

#[tokio::test]
async fn repeated_recipients_are_refused_under_both_policies() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    for policy in [OnBusyPolicy::Refuse, OnBusyPolicy::Queue] {
        let mut request = text_send(project_id, vec![agents[0], agents[0]], "hi");
        request.on_busy = policy;
        request.idempotency_key = Some(format!("dup-{policy:?}"));
        let err = send_now(env.handle(), env.home(), request)
            .await
            .unwrap_err();
        assert_eq!(error_code(&err), error_code::INVALID_ARGUMENT);
    }
    assert!(lock(&env.adapter.prompts).is_empty(), "nothing dispatched");
    assert!(
        env.state
            .ledger
            .send_id_for_key(project_id, "dup-Refuse")
            .is_none(),
        "refused before any key was claimed"
    );
}

#[tokio::test(start_paused = true)]
async fn a_kept_alive_forward_fires_progress_past_five_minutes_and_completes() {
    let (env, project_id, agents) = project_with_agents(&["src", "r"]);
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("slow source", &gate));
    crate::commands::open_project_impl(&env.state, project_id).unwrap();
    send_message_impl(
        &env.state,
        agents[0],
        "go",
        Vec::new(),
        Uuid::now_v7(),
        &env.home(),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, agents[0]).await;
    let ticks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&ticks);
    let budget = WaitBudget::new(Some(3_600), true).with_progress(Arc::new(move || {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }));
    let releaser = Arc::clone(&gate);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(400)).await;
        releaser.notify_waiters();
    });
    let request = ForwardRequest {
        project_id,
        sources: vec![ForwardSourceRef {
            agent_id: agents[0],
            project_id,
        }],
        recipients: vec![agents[1]],
        text: None,
        prompt: None,
        on_busy: OnBusyPolicy::Refuse,
        idempotency_key: None,
    };
    let started = tokio::time::Instant::now();
    let receipt = forward(env.handle(), env.home(), request, budget)
        .await
        .expect("the compose outlives Claude Code's 300-second silence limit");
    assert_eq!(receipt.recipients[0].status, SendStatus::Accepted);
    assert!(started.elapsed() >= Duration::from_secs(400));
    let fired = ticks.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        fired >= 6,
        "progress fired {fired} times across the compose"
    );
}

#[tokio::test(start_paused = true)]
async fn a_retry_that_joins_an_in_flight_send_and_runs_out_of_budget_reports_in_progress() {
    let (env, project_id, agents) = project_with_agents(&["src", "r"]);
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("slow source", &gate));
    crate::commands::open_project_impl(&env.state, project_id).unwrap();
    send_message_impl(
        &env.state,
        agents[0],
        "go",
        Vec::new(),
        Uuid::now_v7(),
        &env.home(),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, agents[0]).await;
    let request = ForwardRequest {
        project_id,
        sources: vec![ForwardSourceRef {
            agent_id: agents[0],
            project_id,
        }],
        recipients: vec![agents[1]],
        text: None,
        prompt: None,
        on_busy: OnBusyPolicy::Refuse,
        idempotency_key: Some("joined".to_owned()),
    };
    let original = tokio::spawn(forward(
        env.handle(),
        env.home(),
        request.clone(),
        WaitBudget::new(Some(600), true),
    ));
    tokio::task::yield_now().await;
    let err = forward(
        env.handle(),
        env.home(),
        request.clone(),
        WaitBudget::new(Some(5), false),
    )
    .await
    .unwrap_err();
    assert_eq!(error_code(&err), error_code::REQUEST_IN_PROGRESS);
    let AppError::RequestInProgress { send_id } = err else {
        panic!("{err:?}");
    };
    assert_eq!(
        env.state.ledger.send_id_for_key(project_id, "joined"),
        Some(send_id),
        "the key stays claimed by the original"
    );
    gate.notify_waiters();
    let receipt = original.await.unwrap().unwrap();
    assert_eq!(receipt.send_id, send_id);
    let again = forward(
        env.handle(),
        env.home(),
        request,
        WaitBudget::new(Some(5), false),
    )
    .await
    .unwrap();
    assert_eq!(again.send_id, send_id, "a later retry gets the receipt");
    wait_done(&env, project_id, send_id).await;
    assert_eq!(
        lock(&env.adapter.prompts).len(),
        2,
        "the forward was dispatched once"
    );
}

#[tokio::test]
async fn a_first_recipient_refused_after_the_check_is_an_error_for_caller_and_joiner_alike() {
    let (env, project_id, agents) = project_with_agents(&["a", "b"]);
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("a-busy", &gate));
    let barrier = env.seam(TestSeam::OpsDispatch);
    let mut request = text_send(project_id, agents.clone(), "hi");
    request.idempotency_key = Some("race".to_owned());
    let caller = tokio::spawn(send_now(env.handle(), env.home(), request.clone()));
    // Before a: make a busy now, after the up-front check passed.
    barrier.entered.notified().await;
    send_message_impl(
        &env.state,
        agents[0],
        "compose",
        Vec::new(),
        Uuid::now_v7(),
        &env.home(),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, agents[0]).await;
    let joiner = tokio::spawn(send_now(env.handle(), env.home(), request.clone()));
    tokio::task::yield_now().await;
    env.clear_seam(TestSeam::OpsDispatch);
    barrier.release.notify_one();

    let err = caller.await.unwrap().unwrap_err();
    assert_eq!(error_code(&err), error_code::AGENT_BUSY);
    let retry_err = joiner.await.unwrap().unwrap_err();
    assert_eq!(
        error_code(&retry_err),
        error_code::AGENT_BUSY,
        "one answer for both callers"
    );
    assert!(
        env.state
            .ledger
            .send_id_for_key(project_id, "race")
            .is_none(),
        "key released"
    );
    assert_eq!(
        lock(&env.adapter.prompts).len(),
        1,
        "b was never dispatched"
    );
    gate.notify_waiters();
}

#[tokio::test]
async fn the_task_guard_settles_or_releases_without_the_caller() {
    use super::ledger::Claim;
    let (env, project_id, agents) = project_with_agents(&["a"]);
    // Nothing accepted: the key is released.
    let Claim::Fresh { send_id } = env.state.ledger.claim(project_id, Some("g1"), "h") else {
        panic!("fresh");
    };
    env.state.ledger.register_send(
        send_id,
        project_id,
        &agents,
        Some("g1"),
        tokio_util::sync::CancellationToken::new(),
    );
    super::send::settle_abnormally(&env.state, send_id);
    assert!(env.state.ledger.send(send_id).is_none());
    assert!(matches!(
        env.state.ledger.claim(project_id, Some("g1"), "h"),
        Claim::Fresh { .. }
    ));
    // Something accepted: the record and key stay, so a retry joins.
    let Claim::Fresh { send_id } = env.state.ledger.claim(project_id, Some("g2"), "h") else {
        panic!("fresh");
    };
    env.state.ledger.register_send(
        send_id,
        project_id,
        &agents,
        Some("g2"),
        tokio_util::sync::CancellationToken::new(),
    );
    env.state.ledger.mark_accepted(send_id, agents[0]).unwrap();
    super::send::settle_abnormally(&env.state, send_id);
    assert!(env.state.ledger.send(send_id).is_some_and(|s| s.settled));
    assert!(matches!(
        env.state.ledger.claim(project_id, Some("g2"), "h"),
        Claim::Joined { .. }
    ));
}

#[tokio::test]
async fn a_session_held_elsewhere_reports_agent_busy_elsewhere() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    crate::commands::open_project_impl(&env.state, project_id).unwrap();
    let agent = lock(&env.state.agents_by_id)
        .get(&agents[0])
        .cloned()
        .unwrap();
    let directory = lock(&env.state.projects)
        .get(&project_id)
        .unwrap()
        .directory
        .clone();
    let key = crate::session_lock::session_lock_key(
        agent.harness,
        agent.session_locator.as_ref().unwrap(),
        &directory,
    )
    .unwrap();
    // Another process (a terminal resume) holds the session's lock.
    let _held = crate::session_lock::acquire_session_locks(
        &env.state.lock_root,
        &std::collections::BTreeSet::from([key]),
    )
    .unwrap();
    let receipt = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "go"),
    )
    .await
    .unwrap();
    let reply = wait_done(&env, project_id, receipt.send_id).await;
    assert_eq!(reply.recipients[0].status, SendStatus::Failed);
    assert_eq!(
        reply.recipients[0].code,
        Some(error_code::AGENT_BUSY_ELSEWHERE)
    );
}

#[tokio::test]
async fn concurrent_selection_changes_both_survive() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let agent_id = agents[0];
    let barrier = env.seam(TestSeam::SelectionUpdate);
    let state = Arc::clone(&env.state);
    let first = tokio::task::spawn_blocking(move || {
        set_agent_selection(
            &state,
            project_id,
            agent_id,
            &SelectionChange::set("opus"),
            &SelectionChange::unchanged(),
        )
    });
    barrier.entered.notified().await;
    // The first holds the registry lock between its read and its write; the
    // second must wait for it rather than merge onto a stale read.
    let state = Arc::clone(&env.state);
    let second = tokio::task::spawn_blocking(move || {
        set_agent_selection(
            &state,
            project_id,
            agent_id,
            &SelectionChange::unchanged(),
            &SelectionChange::set("high"),
        )
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !second.is_finished(),
        "the second write waits for the first"
    );
    env.clear_seam(TestSeam::SelectionUpdate);
    barrier.release.notify_one();
    first.await.unwrap().unwrap();
    let record = second.await.unwrap().unwrap();
    assert_eq!(record.model.as_deref(), Some("opus"));
    assert_eq!(record.effort.as_deref(), Some("high"));
}

#[tokio::test]
async fn a_workflow_whose_steps_name_an_unheard_agent_is_refused() {
    let (env, project_id, agents) = workflow_env().await;
    *lock(&env.sync.unsubscribed) = vec![agents[2]];
    let err = run_workflow(&env.state, &env.home(), pair_request(project_id, &agents))
        .await
        .unwrap_err();
    assert_eq!(error_code(&err), error_code::DESKTOP_UNAVAILABLE);
    assert!(
        lock(&env.state.workflow_runs).is_empty(),
        "no run was registered"
    );
}

#[tokio::test]
async fn a_workflow_runs_the_agent_an_id_names_even_after_a_rename_and_name_reuse() {
    let (env, project_id, agents) = workflow_env().await;
    rename_agent(&env.state, project_id, agents[0], "builder").unwrap();
    let usurper = create_agent(
        &env.state,
        project_id,
        "coder",
        switchboard_core::HarnessKind::ClaudeCode,
        None,
        None,
    )
    .unwrap();
    let run_id = run_workflow(&env.state, &env.home(), pair_request(project_id, &agents))
        .await
        .unwrap();
    wait_workflow(
        &env.state,
        project_id,
        run_id,
        &WaitBudget::new(Some(30), false),
    )
    .await
    .unwrap();
    let builder = read_agent_output(&env.state, project_id, agents[0], &env.home())
        .await
        .unwrap();
    assert_eq!(
        builder.status,
        AgentOutputStatus::Completed,
        "the renamed agent ran"
    );
    let coder = read_agent_output(&env.state, project_id, usurper.id, &env.home())
        .await
        .unwrap();
    assert_eq!(
        coder.status,
        AgentOutputStatus::None,
        "the name's new owner did not"
    );
}

#[tokio::test]
async fn a_page_is_sized_by_its_serialized_length() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let quoted = "\"".repeat(30_000);
    env.adapter.push(Script::reply(&quoted));
    let receipt = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "go"),
    )
    .await
    .unwrap();
    wait_done(&env, project_id, receipt.send_id).await;
    let page = read_send_output(
        &env.state,
        project_id,
        receipt.send_id,
        agents[0],
        0,
        &env.home(),
    )
    .await
    .unwrap();
    assert_eq!(
        page.text.len(),
        OUTPUT_PAGE_CHARS / 2,
        "a quote costs two serialized characters"
    );
    assert!(serde_json::to_string(&page).unwrap().len() <= RESULT_BUDGET_CHARS);
    assert_eq!(page.next_offset, Some(OUTPUT_PAGE_CHARS / 2));
}

#[tokio::test]
async fn a_long_conversation_fits_the_budget_with_its_newest_items_intact() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    for i in 0..12 {
        env.adapter
            .push(Script::reply(&format!("{i}-{}", "r".repeat(4_000))));
        let receipt = send_now(
            env.handle(),
            env.home(),
            text_send(project_id, agents.clone(), "go"),
        )
        .await
        .unwrap();
        wait_done(&env, project_id, receipt.send_id).await;
    }
    let conversation = read_conversation(&env.state, project_id, None, &env.home())
        .await
        .unwrap();
    let fitted = fit(
        serde_json::to_value(&conversation).unwrap(),
        &conversation_budget(project_id),
    )
    .unwrap();
    assert!(fitted.serialized_chars() <= RESULT_BUDGET_CHARS);
    assert!(fitted.items_omitted > 0);
    let items = fitted.result["items"].as_array().unwrap();
    let newest = items.last().unwrap();
    assert_eq!(newest["kind"], "agent_turn");
    let text = newest["text"].as_str().unwrap();
    assert!(
        text.starts_with("11-"),
        "the newest reply is kept whole: {text:.10}"
    );
    assert!(text.len() > 4_000);
}

// --- review round three: written before the fixes, each must fail first --------

#[tokio::test]
async fn a_send_whose_only_recipient_is_unheard_reports_desktop_unavailable() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    *lock(&env.sync.unsubscribed) = vec![agents[0]];
    let err = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents, "hi"),
    )
    .await
    .unwrap_err();
    assert_eq!(error_code(&err), error_code::DESKTOP_UNAVAILABLE, "{err}");
}

#[tokio::test]
async fn a_busy_refusal_after_the_check_names_the_agent() {
    let (env, project_id, agents) = project_with_agents(&["alpha"]);
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("busy", &gate));
    let barrier = env.seam(TestSeam::OpsDispatch);
    let caller = tokio::spawn(send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "hi"),
    ));
    barrier.entered.notified().await;
    send_message_impl(
        &env.state,
        agents[0],
        "compose",
        Vec::new(),
        Uuid::now_v7(),
        &env.home(),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, agents[0]).await;
    env.clear_seam(TestSeam::OpsDispatch);
    barrier.release.notify_one();
    let err = caller.await.unwrap().unwrap_err();
    assert_eq!(error_code(&err), error_code::AGENT_BUSY);
    assert!(err.to_string().contains("alpha"), "names, not ids: {err}");
    gate.notify_waiters();
}

#[tokio::test]
async fn cancelling_a_send_between_its_check_and_its_admission_stops_it() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    // The admitted turn stays running until cancelled, so the cancel that
    // reaches it is the one under test, not a race with its completion.
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("would run", &gate));
    let barrier = env.seam(TestSeam::OpsDispatch);
    let mut request = text_send(project_id, agents.clone(), "hi");
    request.idempotency_key = Some("cancel-gap".to_owned());
    let caller = tokio::spawn(send_now(env.handle(), env.home(), request.clone()));
    // Parked after the cancel check and before the dispatcher admits it.
    barrier.entered.notified().await;
    let send_id = env
        .state
        .ledger
        .send_id_for_key(project_id, "cancel-gap")
        .expect("the send is registered");
    cancel_send(&env.state, project_id, send_id).unwrap();
    env.clear_seam(TestSeam::OpsDispatch);
    barrier.release.notify_one();
    let receipt = caller
        .await
        .unwrap()
        .expect("a cancelled send settles with a receipt");
    let reply = wait_done(&env, project_id, receipt.send_id).await;
    assert_eq!(reply.recipients[0].status, SendStatus::Cancelled);
    let again = send_now(env.handle(), env.home(), request).await.unwrap();
    assert_eq!(
        again.send_id, send_id,
        "a retry returns the cancelled receipt"
    );
    assert!(
        lock(&env.adapter.prompts).len() <= 1,
        "at most the admitted turn started, and it was cancelled"
    );
    gate.notify_waiters();
}

#[tokio::test]
async fn cancelling_a_forward_while_it_waits_on_a_source_dispatches_nothing() {
    let (env, project_id, agents) = project_with_agents(&["src", "r"]);
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("slow source", &gate));
    crate::commands::open_project_impl(&env.state, project_id).unwrap();
    send_message_impl(
        &env.state,
        agents[0],
        "go",
        Vec::new(),
        Uuid::now_v7(),
        &env.home(),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, agents[0]).await;
    let request = ForwardRequest {
        project_id,
        sources: vec![ForwardSourceRef {
            agent_id: agents[0],
            project_id,
        }],
        recipients: vec![agents[1]],
        text: None,
        prompt: None,
        on_busy: OnBusyPolicy::Refuse,
        idempotency_key: Some("held".to_owned()),
    };
    let caller = tokio::spawn(forward(
        env.handle(),
        env.home(),
        request,
        WaitBudget::new(Some(600), true),
    ));
    let send_id = loop {
        if let Some(id) = env.state.ledger.send_id_for_key(project_id, "held") {
            break id;
        }
        tokio::task::yield_now().await;
    };
    cancel_send(&env.state, project_id, send_id).unwrap();
    let receipt = tokio::time::timeout(Duration::from_secs(5), caller)
        .await
        .expect("the cancel stops the compose at once")
        .unwrap()
        .unwrap();
    assert_eq!(receipt.recipients[0].status, SendStatus::Cancelled);
    gate.notify_waiters();
    wait_agent(
        &env.state,
        project_id,
        agents[0],
        &WaitBudget::new(Some(30), false),
        &env.home(),
    )
    .await
    .unwrap();
    assert_eq!(
        lock(&env.adapter.prompts).len(),
        1,
        "only the source's own turn ran"
    );
}

#[tokio::test]
async fn paging_after_a_restart_agrees_with_wait_send_on_a_failed_send() {
    let mut env = TestEnv::new();
    let project_id = env.project("proj");
    let a = env.agent(project_id, "a");
    let send_id = Uuid::now_v7();
    let turn_id = Uuid::now_v7();
    let now = Utc::now();
    let journal = env.journal_path(project_id);
    for record in [
        JournalRecord::Send {
            send_id,
            turn_id,
            agent_id: a,
            prompt: "hi".to_owned(),
            attachments: Vec::new(),
            at: now,
        },
        JournalRecord::Outcome {
            send_id,
            turn_id,
            agent_id: a,
            outcome: serde_json::to_value(TurnOutcome::Failed {
                kind: switchboard_harness::FailureKind::HarnessError,
                message: "boom".to_owned(),
            })
            .unwrap(),
            started_at: now,
            ended_at: now,
        },
    ] {
        switchboard_core::journal::append_record(&journal, &record).unwrap();
    }
    let env = env.restart();
    let waited = wait_done(&env, project_id, send_id).await;
    assert_eq!(waited.recipients[0].status, SendStatus::Failed);
    let page = read_send_output(&env.state, project_id, send_id, a, 0, &env.home())
        .await
        .expect("a failed send with no reply still pages");
    assert_eq!(page.status, SendStatus::Failed);
    assert!(page.text.is_empty());
}

const LOWER_WORKFLOW: &str = "name: lowered
description: test
inputs:
  worker:
    type: agent
steps:
  - label: Greet
    send:
      to: \"{{ worker | lower }}\"
      prompt: \"local:greet\"
  - label: Greeted
    wait_for:
      agent: \"{{ worker | lower }}\"
";

#[tokio::test]
async fn a_step_naming_an_unheard_agent_through_a_filter_is_not_sent() {
    let (env, project_id, agents) = workflow_env().await;
    std::fs::write(env.workflows_dir().join("lowered.yaml"), LOWER_WORKFLOW).unwrap();
    *lock(&env.sync.unsubscribed) = vec![agents[0]];
    let request = RunWorkflowRequest {
        project_id,
        name: "lowered".to_owned(),
        is_builtin: false,
        inputs: BTreeMap::from([
            ("worker".to_owned(), InputValue::Text(agents[0].to_string())),
            ("who".to_owned(), InputValue::Text("bob".to_owned())),
        ]),
        forward_sources: BTreeMap::new(),
    };
    let outcome = run_workflow(&env.state, &env.home(), request).await;
    if let Ok(run_id) = outcome {
        let reply = wait_workflow(
            &env.state,
            project_id,
            run_id,
            &WaitBudget::new(Some(30), false),
        )
        .await
        .unwrap();
        assert_eq!(reply.status, WorkflowRunStatus::Failed);
        assert!(
            reply
                .reason
                .as_deref()
                .unwrap_or_default()
                .contains("cannot listen"),
            "{:?}",
            reply.reason
        );
    }
    assert!(
        lock(&env.adapter.prompts).is_empty(),
        "nothing was sent to the unheard agent"
    );
}

/// One project whose agent `a` covers every case the classifier distinguishes
/// — an imported turn, a durably linked fan-out (with agent `b`), a poisoned
/// link, an unlinked send, and a failed send with no reply. Returns the
/// project, `a`, the failed send, and the unlinked send.
fn correlation_fixture() -> (TestEnv, ProjectId, AgentId, Uuid, Uuid) {
    let mut env = TestEnv::new();
    let project_id = env.project("proj");
    let a = env.agent(project_id, "a");
    let b = env.agent(project_id, "b");
    let t = |minutes: i64| Utc::now() - chrono::Duration::minutes(60 - minutes);
    let (s1, s2, s3, s4) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    let decoy = Uuid::now_v7();
    let send = |send_id, agent_id, prompt: &str, at| JournalRecord::Send {
        send_id,
        turn_id: Uuid::now_v7(),
        agent_id,
        prompt: prompt.to_owned(),
        attachments: Vec::new(),
        at,
    };
    let link = |send_id, agent_id, key: &str, at| JournalRecord::TurnLink {
        send_id,
        turn_id: Uuid::now_v7(),
        agent_id,
        hydration_key: key.to_owned(),
        at,
    };
    let records = vec![
        // A fan-out to both agents, each turn durably linked.
        send(s1, a, "one", t(10)),
        send(s1, b, "one", t(10)),
        link(s1, a, "a1", t(11)),
        link(s1, b, "b1", t(11)),
        // Two links naming one key for different sends: poisoned, so the turn
        // falls back to positional matching.
        send(s2, a, "two", t(20)),
        link(s2, a, "a2", t(21)),
        link(decoy, a, "a2", t(21)),
        // A send that failed before any reply.
        send(s3, a, "three", t(30)),
        JournalRecord::Outcome {
            send_id: s3,
            turn_id: Uuid::now_v7(),
            agent_id: a,
            outcome: serde_json::to_value(TurnOutcome::Failed {
                kind: switchboard_harness::FailureKind::HarnessError,
                message: "boom".to_owned(),
            })
            .unwrap(),
            started_at: t(30),
            ended_at: t(30),
        },
        // A send with no link at all: positional.
        send(s4, a, "four", t(40)),
    ];
    let journal = env.journal_path(project_id);
    for record in &records {
        switchboard_core::journal::append_record(&journal, record).unwrap();
    }
    env.seed_claude_turns(
        project_id,
        a,
        &[
            // Before the agent's first journaled send: imported history.
            ("old", "imported answer", "a0", t(1)),
            ("one", "answer one", "a1", t(10)),
            ("two", "answer two", "a2", t(20)),
            ("four", "answer four", "a4", t(40)),
        ],
    );
    env.seed_claude_turns(project_id, b, &[("one", "b answer one", "b1", t(10))]);
    (env, project_id, a, s3, s4)
}

/// The merged conversation and the single-agent readers derive an agent's
/// sends from one place; this proves they assign the same send to every turn
/// across the cases the classifier distinguishes, so a `send_id` reported by
/// one pages the same reply through the other.
#[tokio::test]
async fn the_merged_conversation_and_reply_paging_agree_on_every_turn() {
    let (env, project_id, a, s3, s4) = correlation_fixture();
    let env = env.restart();

    let conversation = read_conversation(&env.state, project_id, None, &env.home())
        .await
        .unwrap();
    let mut checked = 0;
    for item in &conversation.items {
        let DigestItem::AgentTurn {
            agent_id,
            send_id,
            send_correlation,
            text: answer,
            ..
        } = item
        else {
            continue;
        };
        let Some(send_id) = send_id else {
            assert_eq!(
                answer, "imported answer",
                "only the imported turn has no send"
            );
            continue;
        };
        let page = read_send_output(&env.state, project_id, *send_id, *agent_id, 0, &env.home())
            .await
            .unwrap();
        assert_eq!(
            &page.text, answer,
            "send {send_id} pages the reply the conversation shows"
        );
        let expected = match send_correlation {
            Some(crate::commands::SendCorrelation::DurableLink) => OutputSource::DurableLink,
            _ => OutputSource::Positional,
        };
        assert_eq!(page.source, expected, "and with the same confidence");
        let expected_source = match answer.as_str() {
            "answer one" | "b answer one" => OutputSource::DurableLink,
            // The poisoned link and the unlinked send.
            "answer two" | "answer four" => OutputSource::Positional,
            other => panic!("unexpected turn {other:?}"),
        };
        assert_eq!(page.source, expected_source, "{answer}");
        checked += 1;
    }
    assert_eq!(checked, 4, "a1, a2, a4, and b1 each carry a send");

    let failed = read_send_output(&env.state, project_id, s3, a, 0, &env.home())
        .await
        .unwrap();
    assert_eq!(failed.status, SendStatus::Failed);
    assert!(failed.text.is_empty());
    let latest = read_agent_output(&env.state, project_id, a, &env.home())
        .await
        .unwrap();
    assert_eq!(latest.text.as_deref(), Some("answer four"));
    assert_eq!(latest.send_id, Some(s4));
}

// --- review round four ------------------------------------------------------------

#[tokio::test]
async fn an_unheard_recipient_first_or_in_the_middle_does_not_stop_the_others() {
    for unheard in [0, 1] {
        let (env, project_id, agents) = project_with_agents(&["a", "b", "c"]);
        *lock(&env.sync.unsubscribed) = vec![agents[unheard]];
        let receipt = send_now(
            env.handle(),
            env.home(),
            text_send(project_id, agents.clone(), "hi"),
        )
        .await
        .unwrap();
        for (index, recipient) in receipt.recipients.iter().enumerate() {
            if index == unheard {
                assert_eq!(recipient.status, SendStatus::Rejected);
                assert_eq!(recipient.code, Some(error_code::DESKTOP_UNAVAILABLE));
            } else {
                assert_eq!(
                    recipient.status,
                    SendStatus::Accepted,
                    "recipient {index}, with the unheard agent at {unheard}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_send_whose_recipients_are_all_unheard_reports_desktop_unavailable() {
    let (env, project_id, agents) = project_with_agents(&["a", "b"]);
    *lock(&env.sync.unsubscribed) = agents.clone();
    let err = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents, "hi"),
    )
    .await
    .unwrap_err();
    assert_eq!(error_code(&err), error_code::DESKTOP_UNAVAILABLE, "{err}");
}

#[tokio::test(start_paused = true)]
async fn a_busy_check_whose_deadline_passed_before_asking_names_no_one_busy() {
    let (env, project_id, agents) = project_with_agents(&["a", "b"]);
    let roster = list_project_agents_readonly_impl(&env.state, project_id).unwrap();
    let budget = short();
    tokio::time::advance(Duration::from_secs(2)).await;
    let err = refuse_if_busy(&env.state, &budget, &agents, &[], &roster)
        .await
        .unwrap_err();
    assert!(
        matches!(err, AppError::TimedOut { .. }),
        "both agents are idle, so neither may be reported busy: {err}"
    );
    assert_eq!(error_code(&err), error_code::INTERNAL);
}

#[tokio::test]
async fn after_a_restart_an_unreadable_transcript_hides_only_its_own_recipient() {
    let mut env = TestEnv::new();
    let project_id = env.project("proj");
    let a = env.agent(project_id, "a");
    let b = env.agent(project_id, "b");
    let send_id = Uuid::now_v7();
    let now = Utc::now() - chrono::Duration::minutes(1);
    let journal = env.journal_path(project_id);
    let turn_a = Uuid::now_v7();
    for record in [
        JournalRecord::Send {
            send_id,
            turn_id: turn_a,
            agent_id: a,
            prompt: "hi".to_owned(),
            attachments: Vec::new(),
            at: now,
        },
        JournalRecord::Send {
            send_id,
            turn_id: Uuid::now_v7(),
            agent_id: b,
            prompt: "hi".to_owned(),
            attachments: Vec::new(),
            at: now,
        },
        JournalRecord::TurnLink {
            send_id,
            turn_id: turn_a,
            agent_id: a,
            hydration_key: "m1".to_owned(),
            at: now,
        },
    ] {
        switchboard_core::journal::append_record(&journal, &record).unwrap();
    }
    env.seed_claude_turn(project_id, a, "DONE", "m1");
    env.seed_unreadable_session(project_id, b);
    let env = env.restart();

    let reply = wait_done(&env, project_id, send_id).await;
    assert_eq!(reply.source, WaitSource::Journal);
    assert_eq!(reply.recipients[0].agent_id, a);
    assert_eq!(reply.recipients[0].status, SendStatus::Completed);
    assert_eq!(reply.recipients[0].text.as_deref(), Some("DONE"));
    assert_eq!(reply.recipients[1].agent_id, b);
    assert_eq!(
        reply.recipients[1].status,
        SendStatus::Started,
        "outcome unknown, not failed"
    );
    let error = reply.recipients[1].error.as_deref().unwrap_or_default();
    assert!(
        error.contains("could not read this agent's transcript"),
        "{error}"
    );
}

#[tokio::test]
async fn a_turn_with_three_hundred_edits_comes_back_whole_in_the_conversation() {
    let mut env = TestEnv::new();
    let project_id = env.project("proj");
    let a = env.agent(project_id, "coder");
    let send_id = Uuid::now_v7();
    let turn_id = Uuid::now_v7();
    let at = Utc::now() - chrono::Duration::minutes(5);
    let journal = env.journal_path(project_id);
    for record in [
        JournalRecord::Send {
            send_id,
            turn_id,
            agent_id: a,
            prompt: "edit".to_owned(),
            attachments: Vec::new(),
            at,
        },
        JournalRecord::TurnLink {
            send_id,
            turn_id,
            agent_id: a,
            hydration_key: "m1".to_owned(),
            at,
        },
    ] {
        switchboard_core::journal::append_record(&journal, &record).unwrap();
    }
    let answer = "Refactored the parser; all tests pass.";
    env.seed_claude_editing_turn(
        project_id,
        a,
        300,
        answer,
        "m1",
        at + chrono::Duration::seconds(1),
    );
    let env = env.restart();

    let window_shape = load_project_conversation_impl(&env.state, project_id, &env.home())
        .await
        .unwrap();
    assert!(
        serde_json::to_string(&window_shape).unwrap().len() > 3 * RESULT_BUDGET_CHARS,
        "the window's shape, with every tool card, is far over the budget"
    );

    let conversation = read_conversation(&env.state, project_id, None, &env.home())
        .await
        .unwrap();
    let fitted = fit(
        serde_json::to_value(&conversation).unwrap(),
        &conversation_budget(project_id),
    )
    .unwrap();
    assert!(fitted.truncated.is_empty(), "nothing needed cutting");
    assert_eq!(fitted.items_omitted, 0);
    assert_eq!(
        conversation.items,
        vec![
            DigestItem::UserMessage {
                send_id: Some(send_id),
                agent_ids: vec![a],
                text: "edit".to_owned(),
                attachments: Vec::new(),
                at,
            },
            DigestItem::AgentTurn {
                agent_id: a,
                send_id: Some(send_id),
                send_correlation: Some(crate::commands::SendCorrelation::DurableLink),
                status: switchboard_harness::TurnStatus::Complete,
                text: answer.to_owned(),
                // A turn starts at its first assistant message, a moment
                // after its prompt.
                at: at + chrono::Duration::seconds(1) + chrono::Duration::milliseconds(1),
            },
        ]
    );
    assert_eq!(conversation.agents.len(), 1);
    assert_eq!(conversation.agents[0].name, "coder");
    assert_eq!(conversation.agents[0].load_error, None);
}

#[tokio::test]
async fn a_cut_answer_in_the_conversation_reads_on_from_where_it_was_cut() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let reply = "r".repeat(40_000);
    env.adapter.push(Script::reply(&reply));
    let receipt = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "go"),
    )
    .await
    .unwrap();
    wait_done(&env, project_id, receipt.send_id).await;
    let conversation = read_conversation(&env.state, project_id, None, &env.home())
        .await
        .unwrap();
    let fitted = fit(
        serde_json::to_value(&conversation).unwrap(),
        &conversation_budget(project_id),
    )
    .unwrap();
    assert!(fitted.serialized_chars() <= RESULT_BUDGET_CHARS);
    let cut = fitted
        .truncated
        .iter()
        .find(|t| t.path.ends_with("/text") && t.total_chars == reply.len())
        .expect("the answer was cut");
    let read_with = cut
        .read_with
        .as_ref()
        .expect("an answer with a send reads on");
    assert_eq!(read_with["tool"], "read_send_output");
    assert_eq!(read_with["offset"], cut.kept_chars);
    let page = read_send_output(
        &env.state,
        project_id,
        receipt.send_id,
        agents[0],
        cut.kept_chars,
        &env.home(),
    )
    .await
    .unwrap();
    assert_eq!(
        page.offset + page.text.len(),
        page.total_chars.min(cut.kept_chars + OUTPUT_PAGE_CHARS)
    );
    assert_eq!(
        page.total_chars,
        reply.len(),
        "the same reply the conversation cut"
    );
}
