//! Fixture-driven tests of the `ops` layer over the scripted harness. Each
//! starts from a project the window has not opened: the project exists in
//! the store, and the state under test was built after it.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use switchboard_core::{AgentId, JournalRecord, ProjectId, SessionLocator};
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
use crate::workflow::{DispatchEnd, DispatchFactoryProvider as _};
use crate::workflow_commands::ProjectDispatchFactoryProvider;

use super::budget::fit;
use super::change::{
    SelectionChange, SelectionChanges, create_agent, create_project, dismiss_workflow_run,
    rename_agent, set_agent_model_and_effort,
};
use super::desktop_sync::{Hearing, SyncAck, SyncFork};
use super::error_code::{self, error_code};
use super::ledger::{ForkClaim, SendStatus};
use super::read::conversation_budget;
use super::read::{
    DigestItem, DigestTurnStatus, LatestReplyStatus, OutputSource, ProjectStatus,
    ProjectStatusListing, ReadFrom, SERVER_TEMPLATE_NOTE, UNLISTED_NOTE, list_agents,
    list_projects, prompt_budget, read_conversation, read_latest_reply, read_prompt, read_reply,
};
use super::send::{
    AgentWaitStatus, Body, ForwardFrom, ForwardPrompt, ForwardRequest, OnBusyPolicy, SendRequest,
    WaitSource, cancel_send, forward, refuse_if_busy, send, wait_agent, wait_any_send, wait_send,
    wait_send_budget,
};
use super::test_support::*;
use super::wait::{FINAL_OBSERVATION_GRACE, WaitBudget};
use super::workflow::{
    RunSendReplies, RunWorkflowRequest, WorkflowRunStatus, describe_workflow, read_workflow_run,
    read_workflow_run_budget, run_workflow, wait_workflow,
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
        attachments: Vec::new(),
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

/// A forward source by its latest answer, as a forward without `send_id` names it.
fn latest_answer(project_id: ProjectId, agent_id: AgentId) -> ForwardFrom {
    ForwardFrom {
        project_id,
        agent_id,
        send_id: None,
    }
}

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
        list_agents(
            &env.state,
            project_id,
            &env.home(),
            &CancellationToken::new()
        )
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

    let changed = set_agent_model_and_effort(
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

    let reset = set_agent_model_and_effort(
        &env.state,
        project_id,
        created.id,
        &SelectionChange::reset(),
        &SelectionChange::unchanged(),
    )
    .unwrap();
    assert_eq!(reset.model, None, "null resets to the harness default");

    let listed = list_agents(
        &env.state,
        project_id,
        &env.home(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(listed.len(), 1);
    assert!(!listed[0].busy);

    let output = read_latest_reply(&env.state, project_id, created.id, &env.home())
        .await
        .unwrap();
    assert_eq!(output.status, LatestReplyStatus::None);

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
    let listed = list_projects(&env.state, false, &CancellationToken::new())
        .await
        .unwrap();
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
    let listed = list_projects(&env.state, false, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(listed[0].status, ProjectStatus::Busy);
    gate.notify_waiters();
}

/// An archived project is left out unless the caller asks for it, as the
/// window's project list leaves it out until the user opens its archived
/// view; the listing still says which ones are archived.
#[tokio::test]
async fn list_projects_leaves_out_archived_projects_unless_asked() {
    let mut env = TestEnv::new();
    let active = env.project("active");
    let archived = env.project("shelved");
    crate::commands::set_project_archived_impl(&env.state, archived, true).unwrap();
    let ids = |listed: &[ProjectStatusListing]| -> Vec<ProjectId> {
        listed.iter().map(|p| p.listing.id).collect()
    };

    let default = list_projects(&env.state, false, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        ids(&default),
        vec![active],
        "the archived project is left out"
    );

    let all = list_projects(&env.state, true, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(all.len(), 2, "both projects are listed");
    assert!(
        all.iter()
            .any(|p| p.listing.id == archived && p.listing.archived),
        "the archived one says so"
    );
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
        attachments: Vec::new(),
    };
    let receipt = send_now(env.handle(), env.home(), request).await.unwrap();
    wait_done(&env, project_id, receipt.send_id).await;
    assert_eq!(lock(&env.adapter.prompts)[0], "Hi bob\n\nthanks");
}

/// A local prompt's template comes back with its placeholders intact, its
/// arguments as `list_prompts` declares them, and no rendering until asked.
/// With args, the rendering is the text a send of the prompt delivers.
#[tokio::test]
async fn read_prompt_returns_a_local_template_and_the_rendering_a_send_delivers() {
    let mut env = TestEnv::new().with_prompts_and_workflows();
    std::fs::write(env.prompts_dir().join("greet.md"), GREET_PROMPT).unwrap();
    env.state.prompts.sync().await;
    let budget = WaitBudget::default_wait();

    let plain = read_prompt(&env.state, "local", "greet", None, &budget)
        .await
        .unwrap();
    assert_eq!(plain.template.unwrap().text, "Hi {{ who }}\n");
    assert_eq!(plain.template_note, None);
    assert_eq!(plain.rendered, None, "no rendering without args");
    let arguments = plain.arguments.unwrap();
    assert_eq!(arguments.len(), 1);
    assert!(arguments[0].required);
    assert_eq!(plain.listing_note, None);

    let args = BTreeMap::from([("who".to_owned(), "bob".to_owned())]);
    let rendered = read_prompt(&env.state, "local", "greet", Some(&args), &budget)
        .await
        .unwrap()
        .rendered
        .unwrap()
        .text;
    let project_id = env.project("proj");
    let agent = env.agent(project_id, "a");
    let request = SendRequest {
        project_id,
        recipients: vec![agent],
        body: Body::Prompt {
            provider: "local".to_owned(),
            name: "greet".to_owned(),
            args,
            appended_text: None,
        },
        on_busy: OnBusyPolicy::Refuse,
        idempotency_key: None,
        attachments: Vec::new(),
    };
    let receipt = send_now(env.handle(), env.home(), request).await.unwrap();
    wait_done(&env, project_id, receipt.send_id).await;
    assert_eq!(
        lock(&env.adapter.prompts)[0],
        rendered,
        "the rendering is what a send delivers"
    );
}

/// The argument checks are a send's: a missing required argument and an
/// unknown prompt are the caller's error, before anything is read.
#[tokio::test]
async fn read_prompt_refuses_a_missing_argument_and_an_unknown_prompt() {
    let env = TestEnv::new().with_prompts_and_workflows();
    std::fs::write(env.prompts_dir().join("greet.md"), GREET_PROMPT).unwrap();
    env.state.prompts.sync().await;
    let budget = WaitBudget::default_wait();

    let missing = read_prompt(
        &env.state,
        "local",
        "greet",
        Some(&BTreeMap::new()),
        &budget,
    )
    .await
    .expect_err("who is required");
    assert_eq!(error_code(&missing), error_code::INVALID_ARGUMENT);
    let unknown = read_prompt(&env.state, "local", "nope", None, &budget)
        .await
        .expect_err("no such prompt");
    assert_eq!(error_code(&unknown), error_code::INVALID_ARGUMENT);
}

/// A prompt server gives out no template: the result says so plainly, and
/// with args returns the server's rendering, which is what a send delivers.
/// Arguments the server refuses are the caller's error, as a local prompt's
/// missing argument is.
#[tokio::test]
async fn read_prompt_from_a_prompt_server_has_no_template_but_renders() {
    let server = spawn_prompt_server().await;
    let env = TestEnv::new().with_prompts_and_workflows();
    env.add_prompt_server("tiddly", &server.url);
    env.state.prompts.sync().await;
    let budget = WaitBudget::default_wait();

    let plain = read_prompt(&env.state, "tiddly", "review", None, &budget)
        .await
        .unwrap();
    assert_eq!(plain.template, None);
    assert_eq!(plain.template_note, Some(SERVER_TEMPLATE_NOTE));
    assert_eq!(plain.description.as_deref(), Some("Review code"));
    assert_eq!(plain.arguments.unwrap()[0].name, "context");

    let args = BTreeMap::from([("context".to_owned(), "the parser".to_owned())]);
    let reading = read_prompt(&env.state, "tiddly", "review", Some(&args), &budget)
        .await
        .unwrap();
    assert_eq!(reading.rendered.unwrap().text, "Review the parser");
    assert_eq!(
        reading.template_note,
        Some(SERVER_TEMPLATE_NOTE),
        "still no template beside the rendering"
    );

    let refused = read_prompt(
        &env.state,
        "tiddly",
        "review",
        Some(&BTreeMap::new()),
        &budget,
    )
    .await
    .expect_err("the server requires context");
    assert_eq!(error_code(&refused), error_code::INVALID_ARGUMENT);
}

/// With args, a prompt server's prompt renders as a send would, whether or
/// not Switchboard has listed it: here the server was never fetched. Its
/// description and arguments are then unknown, null rather than an empty
/// list that would read as "takes none", and the note says why. Without
/// args there is nothing to return, and the error says to pass them.
#[tokio::test]
async fn read_prompt_renders_a_server_prompt_switchboard_has_not_listed() {
    let server = spawn_prompt_server().await;
    let env = TestEnv::new().with_prompts_and_workflows();
    env.add_prompt_server("tiddly", &server.url);
    let budget = WaitBudget::default_wait();

    let args = BTreeMap::from([("context".to_owned(), "the parser".to_owned())]);
    let reading = read_prompt(&env.state, "tiddly", "review", Some(&args), &budget)
        .await
        .unwrap();
    assert_eq!(
        reading.rendered.as_ref().map(|r| r.text.as_str()),
        Some("Review the parser")
    );
    assert_eq!(reading.arguments, None);
    assert_eq!(reading.description, None);
    assert_eq!(reading.listing_note, Some(UNLISTED_NOTE));
    let shape = serde_json::to_value(&reading).unwrap();
    assert!(shape["arguments"].is_null(), "{shape}");

    let unlisted = read_prompt(&env.state, "tiddly", "review", None, &budget)
        .await
        .expect_err("nothing is known of the prompt without a listing or args");
    assert!(
        unlisted.to_string().contains("pass args"),
        "the error says how to read it anyway: {unlisted}"
    );
}

/// A server the user is signed out of is reported as needing sign-in, never
/// as a missing prompt: with args from the render, as a send reports it, and
/// without them from the server's state at its last fetch.
#[tokio::test]
async fn read_prompt_reports_a_signed_out_server_as_needing_sign_in() {
    let env = TestEnv::new().with_prompts_and_workflows();
    env.add_signed_out_prompt_server("tiddly");
    env.state.prompts.sync().await;
    let budget = WaitBudget::default_wait();

    let without = read_prompt(&env.state, "tiddly", "review", None, &budget)
        .await
        .expect_err("signed out");
    assert_eq!(error_code(&without), error_code::PROMPT_NEEDS_SIGN_IN);
    let args = BTreeMap::from([("context".to_owned(), "x".to_owned())]);
    let with = read_prompt(&env.state, "tiddly", "review", Some(&args), &budget)
        .await
        .expect_err("signed out");
    assert_eq!(error_code(&with), error_code::PROMPT_NEEDS_SIGN_IN);
}

/// A server that failed at its last fetch leaves the prompt unlisted, which
/// is not the same as missing: without args the error says so and how to
/// read the prompt anyway, rather than reporting it gone.
#[tokio::test]
async fn read_prompt_reports_a_server_that_failed_at_its_last_fetch() {
    let env = TestEnv::new().with_prompts_and_workflows();
    env.add_prompt_server("tiddly", "http://127.0.0.1:9/mcp");
    env.state.prompts.sync().await;

    let failed = read_prompt(
        &env.state,
        "tiddly",
        "review",
        None,
        &WaitBudget::default_wait(),
    )
    .await
    .expect_err("the server could not be fetched");
    assert_eq!(error_code(&failed), error_code::INTERNAL);
    let message = failed.to_string();
    assert!(
        message.contains("failed at Switchboard's last fetch") && message.contains("pass args"),
        "the error says what happened and how to read it anyway: {message}"
    );
}

/// A prompt the server's last successful fetch did not list is missing, the
/// caller's error, as an unknown local prompt is.
#[tokio::test]
async fn read_prompt_refuses_a_prompt_the_server_does_not_list() {
    let server = spawn_prompt_server().await;
    let env = TestEnv::new().with_prompts_and_workflows();
    env.add_prompt_server("tiddly", &server.url);
    env.state.prompts.sync().await;

    let missing = read_prompt(
        &env.state,
        "tiddly",
        "nope",
        None,
        &WaitBudget::default_wait(),
    )
    .await
    .expect_err("the server lists no such prompt");
    assert_eq!(error_code(&missing), error_code::INVALID_ARGUMENT);
}

/// A prompt over the result budget is cut to fit, the cut listed with its
/// full length and no call to read on: a saved prompt has no pager.
#[tokio::test]
async fn read_prompt_cuts_a_prompt_over_the_budget_and_reports_the_cut() {
    let env = TestEnv::new().with_prompts_and_workflows();
    let body = "x".repeat(30_000);
    std::fs::write(
        env.prompts_dir().join("long.md"),
        format!("---\nname: long\ndescription: Long.\n---\n{body}\n"),
    )
    .unwrap();
    env.state.prompts.sync().await;
    let reading = read_prompt(
        &env.state,
        "local",
        "long",
        Some(&BTreeMap::new()),
        &WaitBudget::default_wait(),
    )
    .await
    .unwrap();

    let fitted = fit(
        serde_json::to_value(&reading).unwrap(),
        &prompt_budget(10_000),
    )
    .unwrap();
    assert!(fitted.serialized_chars() <= 10_000);
    let mut paths: Vec<&str> = fitted.truncated.iter().map(|t| t.path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(paths, vec!["/rendered/text", "/template/text"], "both cut");
    assert!(fitted.truncated.iter().all(|t| t.read_with.is_none()));
    assert!(
        fitted.truncated.iter().all(|t| t.total_chars >= 30_000),
        "each cut reports the full length"
    );
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
        sources: vec![latest_answer(p2, source)],
        recipients: vec![recipient],
        text: Some("Please review:".to_owned()),
        prompt: None,
        on_busy: OnBusyPolicy::Refuse,
        idempotency_key: None,
        attachments: Vec::new(),
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
        sources: vec![latest_answer(project_id, agents[0])],
        recipients: vec![agents[1]],
        text: None,
        prompt: None,
        on_busy: OnBusyPolicy::Refuse,
        idempotency_key: Some("fwd".to_owned()),
        attachments: Vec::new(),
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
                vec![latest_answer(project_id, source)],
            )]),
        }),
        on_busy: OnBusyPolicy::Refuse,
        idempotency_key: None,
        attachments: Vec::new(),
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
        attachments: Vec::new(),
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
            vec![latest_answer(project_id, source)],
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
    // A stored release: the second turn may not have parked on its gate yet,
    // since the agent can answer the wait between the two turns.
    gate2.notify_one();
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
    assert_eq!(output.status, LatestReplyStatus::Completed);
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
    let cancelled = cancel_send(&env.state, project_id, receipt.send_id, None).unwrap();
    assert_eq!(cancelled.recipients, agents);
    let reply = wait_done(&env, project_id, receipt.send_id).await;
    assert_eq!(reply.recipients[0].status, SendStatus::Cancelled);
    let err = cancel_send(&env.state, project_id, Uuid::now_v7(), None).unwrap_err();
    assert_eq!(error_code(&err), error_code::SEND_UNKNOWN);
}

// --- outputs and the budget ------------------------------------------------------

#[tokio::test]
async fn read_reply_pages_one_reply_stably_while_the_agent_starts_a_new_turn() {
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
    let first = read_reply(
        &env.state,
        project_id,
        receipt.send_id,
        agents[0],
        0,
        ReadFrom::Auto,
        OUTPUT_PAGE_CHARS,
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

    let last = read_reply(
        &env.state,
        project_id,
        receipt.send_id,
        agents[0],
        OUTPUT_PAGE_CHARS * 2,
        ReadFrom::Auto,
        OUTPUT_PAGE_CHARS,
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
        env.adapter.push(Script::reply(&"y".repeat(45_000)));
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
        &wait_send_budget(project_id, receipt.send_id, RESULT_BUDGET_CHARS),
    )
    .unwrap();
    assert!(fitted.serialized_chars() <= RESULT_BUDGET_CHARS);
    assert_eq!(fitted.truncated.len(), 3);
    for cut in &fitted.truncated {
        assert_eq!(cut.total_chars, 45_000);
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

    let page = read_reply(
        &env.state,
        project_id,
        s_a,
        a,
        0,
        ReadFrom::Auto,
        OUTPUT_PAGE_CHARS,
        &env.home(),
    )
    .await
    .unwrap();
    assert_eq!(page.text, "DONE");
    assert_eq!(page.source, OutputSource::DurableLink);
}

// --- workflows ---------------------------------------------------------------------

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

    let run_id = run_workflow(
        &env.state,
        &env.home(),
        pair_request(project_id, &agents),
        &WaitBudget::default_wait(),
    )
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
    let err = run_workflow(&env.state, &env.home(), bad, &WaitBudget::default_wait())
        .await
        .unwrap_err();
    assert_eq!(error_code(&err), error_code::UNKNOWN_RECIPIENT);
}

#[tokio::test]
async fn a_failed_run_blocks_until_dismissed() {
    let (env, project_id, agents) = workflow_env().await;
    env.adapter.push(Script::failing("nope"));
    let run_id = run_workflow(
        &env.state,
        &env.home(),
        pair_request(project_id, &agents),
        &WaitBudget::default_wait(),
    )
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

    let err = run_workflow(
        &env.state,
        &env.home(),
        pair_request(project_id, &agents),
        &WaitBudget::default_wait(),
    )
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

    let rerun = run_workflow(
        &env.state,
        &env.home(),
        pair_request(project_id, &agents),
        &WaitBudget::default_wait(),
    )
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
    let run_id = run_workflow(
        &env.state,
        &env.home(),
        pair_request(project_id, &agents),
        &WaitBudget::default_wait(),
    )
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
    env.sync.hear_all_but(&agents, &[agents[1]]);
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
            sources: vec![latest_answer(project.id, coder.id)],
            recipients: vec![reviewer.id],
            text: Some("critique this".to_owned()),
            prompt: None,
            on_busy: OnBusyPolicy::Refuse,
            idempotency_key: None,
            attachments: Vec::new(),
        },
        WaitBudget::default_wait(),
    )
    .await
    .unwrap();
    let reply = wait_done(&env, project.id, second.send_id).await;
    assert_eq!(reply.recipients[0].status, SendStatus::Completed);
    let page = read_reply(
        &env.state,
        project.id,
        second.send_id,
        reviewer.id,
        0,
        ReadFrom::Auto,
        OUTPUT_PAGE_CHARS,
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
        sources: vec![latest_answer(project_id, agents[0])],
        recipients: vec![agents[1]],
        text: None,
        prompt: None,
        on_busy: OnBusyPolicy::Refuse,
        idempotency_key: None,
        attachments: Vec::new(),
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
        sources: vec![latest_answer(project_id, agents[0])],
        recipients: vec![agents[1]],
        text: None,
        prompt: None,
        on_busy: OnBusyPolicy::Refuse,
        idempotency_key: Some("joined".to_owned()),
        attachments: Vec::new(),
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
        set_agent_model_and_effort(
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
        set_agent_model_and_effort(
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
    env.sync.hear_all_but(&agents, &[agents[2]]);
    let err = run_workflow(
        &env.state,
        &env.home(),
        pair_request(project_id, &agents),
        &WaitBudget::default_wait(),
    )
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
    let run_id = run_workflow(
        &env.state,
        &env.home(),
        pair_request(project_id, &agents),
        &WaitBudget::default_wait(),
    )
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
    let builder = read_latest_reply(&env.state, project_id, agents[0], &env.home())
        .await
        .unwrap();
    assert_eq!(
        builder.status,
        LatestReplyStatus::Completed,
        "the renamed agent ran"
    );
    let coder = read_latest_reply(&env.state, project_id, usurper.id, &env.home())
        .await
        .unwrap();
    assert_eq!(
        coder.status,
        LatestReplyStatus::None,
        "the name's new owner did not"
    );
}

#[tokio::test]
async fn a_page_is_sized_by_its_serialized_length() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let quoted = "\"".repeat(60_000);
    env.adapter.push(Script::reply(&quoted));
    let receipt = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "go"),
    )
    .await
    .unwrap();
    wait_done(&env, project_id, receipt.send_id).await;
    let page = read_reply(
        &env.state,
        project_id,
        receipt.send_id,
        agents[0],
        0,
        ReadFrom::Auto,
        OUTPUT_PAGE_CHARS,
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

/// A run with more sends than one reply holds still reports its status: the
/// oldest sends are left out, the newest kept, and the count left out said.
#[test]
fn a_workflow_wait_with_more_sends_than_fit_keeps_the_newest() {
    let send = |step: usize| crate::ops::ledger::RunSend {
        step,
        label: Some(format!("Review round {step}")),
        send_id: Uuid::now_v7(),
        recipients: vec![crate::workflow::ResolvedRecipient {
            agent_id: Uuid::now_v7(),
            name: "reviewer-1".to_owned(),
            dispatched: Some(true),
        }],
    };
    let reply = crate::ops::workflow::WaitWorkflowReply {
        run_id: Uuid::now_v7(),
        capped_to_seconds: None,
        usage: None,
        status: WorkflowRunStatus::Completed,
        done: true,
        workflow: "long".to_owned(),
        step: 1_199,
        total: 1_200,
        label: Some("last".to_owned()),
        recipients: Vec::new(),
        sends: (0..600).map(|i| send(i * 2)).collect(),
        sends_unavailable: false,
        reason: None,
    };
    let fitted = fit(
        serde_json::to_value(&reply).unwrap(),
        &crate::ops::workflow::wait_workflow_budget(RESULT_BUDGET_CHARS),
    )
    .unwrap();
    assert!(fitted.serialized_chars() <= RESULT_BUDGET_CHARS);
    assert!(fitted.items_omitted > 0);
    let kept = fitted.result["sends"].as_array().unwrap();
    assert_eq!(
        kept.last().unwrap()["step"],
        1_198,
        "the newest send is kept"
    );
    assert_eq!(kept.len() + fitted.items_omitted, 600);
    assert_eq!(fitted.result["status"], "completed", "and the run's status");
}

#[tokio::test]
async fn a_wait_asked_for_longer_than_the_call_can_last_says_it_was_shortened() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let receipt = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "go"),
    )
    .await
    .unwrap();
    wait_done(&env, project_id, receipt.send_id).await;
    let shortened = wait_send(
        &env.state,
        project_id,
        receipt.send_id,
        &WaitBudget::new(Some(3_600), false),
        &env.home(),
    )
    .await
    .unwrap();
    assert_eq!(shortened.capped_to_seconds, Some(50));
    let as_asked = wait_send(
        &env.state,
        project_id,
        receipt.send_id,
        &WaitBudget::new(Some(10), false),
        &env.home(),
    )
    .await
    .unwrap();
    assert_eq!(as_asked.capped_to_seconds, None);
}

#[tokio::test]
async fn a_long_conversation_fits_the_budget_with_its_newest_items_intact() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    for i in 0..12 {
        env.adapter
            .push(Script::reply(&format!("{i}-{}", "r".repeat(10_000))));
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
        &conversation_budget(project_id, RESULT_BUDGET_CHARS),
    )
    .unwrap();
    assert!(fitted.serialized_chars() <= RESULT_BUDGET_CHARS);
    assert!(fitted.items_omitted > 0);
    let smaller = fit(
        serde_json::to_value(&conversation).unwrap(),
        &conversation_budget(project_id, 30_000),
    )
    .unwrap();
    assert!(smaller.serialized_chars() <= 30_000);
    assert!(
        smaller.items_omitted > fitted.items_omitted,
        "a smaller budget drops more of the oldest items"
    );
    let items = fitted.result["items"].as_array().unwrap();
    let newest = items.last().unwrap();
    assert_eq!(newest["kind"], "agent_turn");
    let text = newest["text"].as_str().unwrap();
    assert!(
        text.starts_with("11-"),
        "the newest reply is kept whole: {text:.10}"
    );
    assert!(text.len() > 10_000);
}

// --- review round three: written before the fixes, each must fail first --------

#[tokio::test]
async fn a_send_whose_only_recipient_is_unheard_reports_desktop_unavailable() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    env.sync.hear_all_but(&agents, &[agents[0]]);
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
    cancel_send(&env.state, project_id, send_id, None).unwrap();
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
        sources: vec![latest_answer(project_id, agents[0])],
        recipients: vec![agents[1]],
        text: None,
        prompt: None,
        on_busy: OnBusyPolicy::Refuse,
        idempotency_key: Some("held".to_owned()),
        attachments: Vec::new(),
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
    cancel_send(&env.state, project_id, send_id, None).unwrap();
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
    let page = read_reply(
        &env.state,
        project_id,
        send_id,
        a,
        0,
        ReadFrom::Auto,
        OUTPUT_PAGE_CHARS,
        &env.home(),
    )
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
    env.sync.hear_all_but(&agents, &[agents[0]]);
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
    let outcome = run_workflow(
        &env.state,
        &env.home(),
        request,
        &WaitBudget::default_wait(),
    )
    .await;
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
        let page = read_reply(
            &env.state,
            project_id,
            *send_id,
            *agent_id,
            0,
            ReadFrom::Auto,
            OUTPUT_PAGE_CHARS,
            &env.home(),
        )
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

    let failed = read_reply(
        &env.state,
        project_id,
        s3,
        a,
        0,
        ReadFrom::Auto,
        OUTPUT_PAGE_CHARS,
        &env.home(),
    )
    .await
    .unwrap();
    assert_eq!(failed.status, SendStatus::Failed);
    assert!(failed.text.is_empty());
    let latest = read_latest_reply(&env.state, project_id, a, &env.home())
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
        env.sync.hear_all_but(&agents, &[agents[unheard]]);
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
    env.sync.hear_all_but(&agents, &agents);
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
    // Past the deadline and the finishing allowance the check runs under.
    tokio::time::advance(Duration::from_secs(1) + FINAL_OBSERVATION_GRACE + Duration::from_secs(1))
        .await;
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
        &conversation_budget(project_id, RESULT_BUDGET_CHARS),
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
                status: DigestTurnStatus::Complete,
                reason: None,
                failure_kind: None,
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

// --- review round five ------------------------------------------------------------

/// Text that differs at every position, so a wrong offset cannot read as
/// right: `chars` characters of numbered lines.
fn numbered(chars: usize) -> String {
    let mut text = String::new();
    let mut line = 0;
    while text.len() < chars {
        line += 1;
        let _ = writeln!(text, "line {line:06}");
    }
    text.truncate(chars);
    text
}

/// The conversation fitted to its budget, and the cut answer of `agent_id`'s
/// turn for `send_id`: its kept text and the call that reads on.
async fn cut_answer(
    env: &TestEnv,
    project_id: ProjectId,
    send_id: Uuid,
    agent_id: AgentId,
) -> (String, serde_json::Value) {
    let conversation = read_conversation(&env.state, project_id, None, &env.home())
        .await
        .unwrap();
    let fitted = fit(
        serde_json::to_value(&conversation).unwrap(),
        &conversation_budget(project_id, RESULT_BUDGET_CHARS),
    )
    .unwrap();
    assert!(fitted.serialized_chars() <= RESULT_BUDGET_CHARS);
    let items = fitted.result["items"].as_array().unwrap();
    let index = items
        .iter()
        .position(|item| {
            item["kind"] == "agent_turn"
                && item["send_id"] == send_id.to_string()
                && item["agent_id"] == agent_id.to_string()
        })
        .expect("the turn is in the conversation");
    let path = format!("/items/{index}/text");
    let cut = fitted
        .truncated
        .iter()
        .find(|t| t.path == path)
        .expect("the answer was cut");
    let kept = items[index]["text"].as_str().unwrap().to_owned();
    assert_eq!(kept.chars().count(), cut.kept_chars);
    let read_with = cut
        .read_with
        .clone()
        .expect("an answer with a send reads on");
    assert_eq!(read_with["tool"], "read_reply");
    (kept, read_with)
}

/// Follow a read-on call to the end of the reply and return everything it
/// paged.
async fn read_on(env: &TestEnv, project_id: ProjectId, read_with: &serde_json::Value) -> String {
    let send_id: Uuid = serde_json::from_value(read_with["send_id"].clone()).unwrap();
    let agent_id: AgentId = serde_json::from_value(read_with["agent_id"].clone()).unwrap();
    let read_from: ReadFrom = serde_json::from_value(read_with["read_from"].clone()).unwrap();
    let mut offset = usize::try_from(read_with["offset"].as_u64().unwrap()).unwrap();
    let mut text = String::new();
    loop {
        let page = read_reply(
            &env.state,
            project_id,
            send_id,
            agent_id,
            offset,
            read_from,
            OUTPUT_PAGE_CHARS,
            &env.home(),
        )
        .await
        .unwrap();
        text.push_str(&page.text);
        match page.next_offset {
            Some(next) => offset = next,
            None => return text,
        }
    }
}

#[tokio::test]
async fn a_cut_answer_rebuilds_exactly_from_its_pages_before_and_after_a_restart() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let reply = numbered(150_000);
    env.adapter.push(Script::reply(&reply));
    let receipt = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "go"),
    )
    .await
    .unwrap();
    wait_done(&env, project_id, receipt.send_id).await;

    let (kept, read_with) = cut_answer(&env, project_id, receipt.send_id, agents[0]).await;
    assert_eq!(read_with["read_from"], "transcript");
    let rest = read_on(&env, project_id, &read_with).await;
    assert_eq!(kept + &rest, reply, "in the same session");

    let env = env.restart();
    let (kept, read_with) = cut_answer(&env, project_id, receipt.send_id, agents[0]).await;
    let rest = read_on(&env, project_id, &read_with).await;
    assert_eq!(kept + &rest, reply, "after a restart");
}

#[tokio::test]
async fn a_cancelled_partial_reply_reads_on_in_the_same_session() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let partial = numbered(150_000);
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked(&partial, &gate));
    let receipt = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "go"),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, agents[0]).await;
    cancel_send(&env.state, project_id, receipt.send_id, None).unwrap();
    let reply = wait_done(&env, project_id, receipt.send_id).await;
    assert_eq!(reply.recipients[0].status, SendStatus::Cancelled);

    let page = read_reply(
        &env.state,
        project_id,
        receipt.send_id,
        agents[0],
        0,
        ReadFrom::Auto,
        OUTPUT_PAGE_CHARS,
        &env.home(),
    )
    .await
    .unwrap();
    assert_eq!(page.status, SendStatus::Cancelled);
    assert_eq!(
        page.total_chars,
        partial.len(),
        "the partial reply on disk, not an empty page"
    );

    let conversation = read_conversation(&env.state, project_id, None, &env.home())
        .await
        .unwrap();
    let status = conversation.items.iter().find_map(|item| match item {
        DigestItem::AgentTurn { status, .. } => Some(*status),
        _ => None,
    });
    assert_eq!(status, Some(DigestTurnStatus::Cancelled));
    let (kept, read_with) = cut_answer(&env, project_id, receipt.send_id, agents[0]).await;
    let rest = read_on(&env, project_id, &read_with).await;
    assert_eq!(kept + &rest, partial);
}

/// A session file holding one reply that a context compaction split: the
/// prompt `go`, the piece before (message `m1`), the compaction summary, and
/// the piece after (message `m2`).
fn split_by_compaction(first: &str, second: &str) -> Vec<serde_json::Value> {
    let at = Utc::now();
    let stamp = |ms: i64| (at + chrono::Duration::milliseconds(ms)).to_rfc3339();
    let assistant = |id: &str, text: &str, ms: i64| {
        serde_json::json!({
            "type": "assistant",
            "message": {
                "id": id,
                "model": "claude-sonnet-4-6",
                "role": "assistant",
                "content": [{ "type": "text", "text": text }],
                "stop_reason": "end_turn",
                "usage": { "input_tokens": 10, "output_tokens": 5 },
            },
            "timestamp": stamp(ms),
        })
    };
    vec![
        serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": "go" },
            "timestamp": stamp(0),
        }),
        assistant("m1", first, 1),
        serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": "This session is being continued…" },
            "isCompactSummary": true,
            "timestamp": stamp(2),
        }),
        assistant("m2", second, 3),
    ]
}

#[tokio::test]
async fn a_reply_split_by_compaction_pages_the_piece_the_conversation_shows() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let first = numbered(30_000);
    let second = "after the compaction".to_owned();
    // Live, the turn streams one reply; on disk the session file splits it at
    // the compaction, and only the first piece carries the send.
    let mut script = Script::reply(&format!("{first}\n\n{second}"));
    script.key = Some("m1".to_owned());
    env.adapter.push(script);
    let receipt = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "go"),
    )
    .await
    .unwrap();
    wait_done(&env, project_id, receipt.send_id).await;
    env.write_claude_session(project_id, agents[0], &split_by_compaction(&first, &second));

    let conversation = read_conversation(&env.state, project_id, None, &env.home())
        .await
        .unwrap();
    let shape: Vec<(&str, Option<&str>)> = conversation
        .items
        .iter()
        .map(|item| match item {
            DigestItem::UserMessage { .. } => ("user", None),
            DigestItem::AgentTurn {
                send_id: Some(_),
                text,
                ..
            } => ("reply", Some(text.as_str())),
            DigestItem::AgentTurn {
                send_id: None,
                text,
                ..
            } => ("continuation", Some(text.as_str())),
            DigestItem::Compacted { .. } => ("compacted", None),
            DigestItem::Outcome { .. } => ("outcome", None),
        })
        .collect();
    assert_eq!(
        shape,
        vec![
            ("user", None),
            ("reply", Some(first.as_str())),
            ("compacted", None),
            ("continuation", Some(second.as_str())),
        ],
        "the marker sits between the two pieces"
    );

    // Reading on from inside the first piece, as a cut in the conversation
    // would, stays within that piece.
    let offset = 1_000;
    let rest = read_on(
        &env,
        project_id,
        &serde_json::json!({
            "send_id": receipt.send_id,
            "agent_id": agents[0],
            "offset": offset,
            "read_from": ReadFrom::Transcript,
        }),
    )
    .await;
    assert_eq!(rest, first[offset..]);
    let captured = read_reply(
        &env.state,
        project_id,
        receipt.send_id,
        agents[0],
        0,
        ReadFrom::Auto,
        OUTPUT_PAGE_CHARS,
        &env.home(),
    )
    .await
    .unwrap();
    assert_eq!(
        captured.total_chars,
        first.len() + 2 + second.len(),
        "the captured copy is the whole reply"
    );
}

#[tokio::test]
async fn a_cancelled_or_failed_turn_carries_its_outcome_however_the_conversation_is_cut() {
    let mut env = TestEnv::new();
    let project_id = env.project("proj");
    let a = env.agent(project_id, "a");
    let (s1, s2, s3) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let t = |minutes: i64| Utc::now() - chrono::Duration::minutes(60 - minutes);
    let send = |send_id, at| JournalRecord::Send {
        send_id,
        turn_id: Uuid::now_v7(),
        agent_id: a,
        prompt: "go".to_owned(),
        attachments: Vec::new(),
        at,
    };
    let outcome = |send_id, outcome: TurnOutcome, at| JournalRecord::Outcome {
        send_id,
        turn_id: Uuid::now_v7(),
        agent_id: a,
        outcome: serde_json::to_value(outcome).unwrap(),
        started_at: at,
        ended_at: at,
    };
    let journal = env.journal_path(project_id);
    for record in [
        // Cancelled after writing part of a reply, then a later send.
        send(s1, t(10)),
        outcome(
            s1,
            TurnOutcome::Cancelled {
                source: CancelSource::User,
            },
            t(10),
        ),
        send(s2, t(20)),
        JournalRecord::TurnLink {
            send_id: s2,
            turn_id: Uuid::now_v7(),
            agent_id: a,
            hydration_key: "m2".to_owned(),
            at: t(20),
        },
        // Failed before any reply.
        send(s3, t(30)),
        outcome(
            s3,
            TurnOutcome::Failed {
                kind: switchboard_harness::FailureKind::UsageLimit,
                message: "limit reached".to_owned(),
            },
            t(30),
        ),
    ] {
        switchboard_core::journal::append_record(&journal, &record).unwrap();
    }
    // The session file closes the cancelled reply as complete once the next
    // prompt starts.
    env.seed_claude_turns(
        project_id,
        a,
        &[
            ("go", "half a reply", "m1", t(10)),
            ("go", "done", "m2", t(20)),
        ],
    );
    let env = env.restart();

    // Six items: each send's message, the two replies, and the failure. The
    // last five start at the cancelled reply, and would have left out the
    // separate cancellation marker, which sorts before it.
    let conversation = read_conversation(&env.state, project_id, Some(5), &env.home())
        .await
        .unwrap();
    let cancelled = conversation
        .items
        .iter()
        .find_map(|item| match item {
            DigestItem::AgentTurn {
                send_id: Some(s),
                status,
                ..
            } if *s == s1 => Some(*status),
            _ => None,
        })
        .expect("the cancelled reply is within the last five items");
    assert_eq!(cancelled, DigestTurnStatus::Cancelled);
    assert!(
        conversation.items.iter().all(|item| !matches!(
            item,
            DigestItem::Outcome { send_id, .. } if *send_id == s1
        )),
        "its outcome is folded into the turn, not a separate item"
    );
    let failed = conversation.items.last().unwrap();
    match failed {
        DigestItem::Outcome {
            send_id,
            status,
            failure_kind,
            reason,
            ..
        } => {
            assert_eq!(*send_id, s3);
            assert_eq!(*status, crate::commands::OutcomeStatus::Failed);
            assert_eq!(failure_kind.as_deref(), Some("usage_limit"));
            assert_eq!(reason.as_deref(), Some("limit reached"));
        }
        other => panic!("a send with no reply keeps its own marker: {other:?}"),
    }
}

// --- The window round trip, through the production `WindowSync` ---

fn ready(listening_agents: Vec<AgentId>) -> SyncAck {
    SyncAck::Ready { listening_agents }
}

#[tokio::test]
async fn a_window_that_answers_after_the_deadline_times_out_and_nothing_is_dispatched() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let (env, sync, _window, mut requests) = env.with_window(Duration::from_millis(100));
    let pending = tokio::spawn(send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents, "hi"),
    ));
    let request = requests.recv().await.unwrap();
    assert_eq!(request.project_id, project_id);
    let err = pending.await.unwrap().unwrap_err();
    assert_eq!(error_code(&err), error_code::PROJECT_LOAD_TIMEOUT);
    assert_eq!(sync.pending_requests(), 0);
    assert!(
        !sync.acknowledge(request.request_id, ready(Vec::new())),
        "the late answer is ignored"
    );
    assert!(lock(&env.adapter.prompts).is_empty());
}

#[tokio::test]
async fn a_send_cancelled_during_the_round_trip_leaves_nothing_pending() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let (env, sync, _window, mut requests) = env.with_window(Duration::from_mins(1));
    let mut request = text_send(project_id, agents, "hi");
    request.idempotency_key = Some("k".to_owned());
    let pending = tokio::spawn(send_now(env.handle(), env.home(), request));
    let sync_request = requests.recv().await.unwrap();
    assert_eq!(sync.pending_requests(), 1);
    let send_id = env.state.ledger.send_id_for_key(project_id, "k").unwrap();
    cancel_send(&env.state, project_id, send_id, None).unwrap();
    let receipt = pending.await.unwrap().unwrap();
    assert_eq!(receipt.recipients[0].status, SendStatus::Cancelled);
    assert_eq!(sync.pending_requests(), 0, "the request went with the send");
    assert!(
        !sync.acknowledge(sync_request.request_id, ready(Vec::new())),
        "the window's late answer is ignored"
    );
    assert!(lock(&env.adapter.prompts).is_empty());
}

#[tokio::test]
async fn a_window_that_cannot_load_the_project_refuses_the_send() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let (env, sync, _window, mut requests) = env.with_window(Duration::from_mins(1));
    let pending = tokio::spawn(send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents, "hi"),
    ));
    let request = requests.recv().await.unwrap();
    assert!(sync.acknowledge(
        request.request_id,
        SyncAck::Failed {
            message: "roster read failed".to_owned(),
        },
    ));
    let err = pending.await.unwrap().unwrap_err();
    assert_eq!(error_code(&err), error_code::DESKTOP_UNAVAILABLE);
    assert!(err.to_string().contains("roster read failed"));
    assert!(lock(&env.adapter.prompts).is_empty());
}

#[tokio::test]
async fn a_recipient_the_window_does_not_list_as_heard_is_rejected_and_the_others_are_sent() {
    let (env, project_id, agents) = project_with_agents(&["a", "b"]);
    let (env, sync, _window, mut requests) = env.with_window(Duration::from_mins(1));
    let pending = tokio::spawn(send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "hi"),
    ));
    let request = requests.recv().await.unwrap();
    // The window lists only "b": "a" is refused though nothing reported it
    // failed, which is what an agent the window never heard of looks like.
    assert!(sync.acknowledge(request.request_id, ready(vec![agents[1]])));
    let receipt = pending.await.unwrap().unwrap();
    assert_eq!(receipt.recipients[0].status, SendStatus::Rejected);
    assert_eq!(
        receipt.recipients[0].code,
        Some(error_code::DESKTOP_UNAVAILABLE)
    );
    assert_eq!(receipt.recipients[1].status, SendStatus::Accepted);
    wait_done(&env, project_id, receipt.send_id).await;
    assert_eq!(*lock(&env.adapter.prompts), vec!["hi".to_owned()]);
}

#[tokio::test]
async fn changes_that_start_no_turn_notify_the_window_without_waiting() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let (env, sync, window, _requests) = env.with_window(Duration::from_mins(1));
    create_agent(
        &env.state,
        project_id,
        "b",
        switchboard_core::HarnessKind::ClaudeCode,
        None,
        None,
    )
    .unwrap();
    rename_agent(&env.state, project_id, agents[0], "renamed").unwrap();
    let dir = tempfile::TempDir::new().unwrap();
    create_project(&env.state, "other", dir.path().to_str().unwrap()).unwrap();
    let names = window.names();
    assert_eq!(
        names,
        vec![
            "desktop_roster_changed",
            "desktop_roster_changed",
            "projects_changed"
        ]
    );
    assert_eq!(
        lock(&window.events)[0].1["project_id"],
        project_id.to_string()
    );
    assert_eq!(sync.pending_requests(), 0);
}

/// Every event on the agent's channel, by type, with its message id when it
/// carries one.
fn channel_events(env: &TestEnv, agent_id: AgentId) -> Vec<(String, Option<String>)> {
    let channel = format!("agent:{agent_id}");
    env.emitter
        .snapshot()
        .into_iter()
        .filter(|(name, _)| *name == channel)
        .map(|(_, v)| {
            (
                v["type"].as_str().unwrap_or_default().to_owned(),
                v["message_id"].as_str().map(str::to_owned),
            )
        })
        .collect()
}

fn position_of(events: &[(String, Option<String>)], kind: &str, message_id: &str) -> usize {
    events
        .iter()
        .position(|(k, m)| k == kind && m.as_deref() == Some(message_id))
        .unwrap_or_else(|| panic!("no {kind} for {message_id}: {events:?}"))
}

#[tokio::test]
async fn a_send_is_announced_as_accepted_before_its_turn_starts_on_an_idle_agent() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let receipt = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "hi"),
    )
    .await
    .unwrap();
    wait_done(&env, project_id, receipt.send_id).await;
    let message_id = receipt.recipients[0].message_id.unwrap().to_string();
    let events = channel_events(&env, agents[0]);
    assert!(
        position_of(&events, "send_accepted", &message_id)
            < position_of(&events, "turn_start", &message_id)
    );
}

#[tokio::test]
async fn a_send_queued_behind_a_running_turn_is_announced_while_it_waits() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("first", &gate));
    let first = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "first"),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, agents[0]).await;
    let mut queued = text_send(project_id, agents.clone(), "second");
    queued.on_busy = OnBusyPolicy::Queue;
    let second = send_now(env.handle(), env.home(), queued).await.unwrap();
    let message_id = second.recipients[0].message_id.unwrap().to_string();
    let events = channel_events(&env, agents[0]);
    position_of(&events, "send_accepted", &message_id);
    assert_eq!(
        events.iter().filter(|(k, _)| k == "turn_start").count(),
        1,
        "announced while the first turn still runs"
    );
    gate.notify_one();
    wait_done(&env, project_id, first.send_id).await;
    wait_done(&env, project_id, second.send_id).await;
    let events = channel_events(&env, agents[0]);
    assert!(
        position_of(&events, "send_accepted", &message_id)
            < position_of(&events, "turn_start", &message_id)
    );
}

#[tokio::test]
async fn a_window_model_change_and_an_ops_effort_change_made_together_both_survive() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let agent_id = agents[0];
    // Load the project, as the window would have before its editor saves.
    crate::commands::open_project_impl(&env.state, project_id).unwrap();
    let barrier = env.seam(TestSeam::SelectionUpdate);
    let state = Arc::clone(&env.state);
    let window = tokio::task::spawn_blocking(move || {
        crate::commands::change_agent_selection_impl(
            &state,
            agent_id,
            &SelectionChanges {
                model: SelectionChange::set("opus"),
                ..SelectionChanges::default()
            },
        )
    });
    barrier.entered.notified().await;
    let state = Arc::clone(&env.state);
    let orchestrator = tokio::task::spawn_blocking(move || {
        set_agent_model_and_effort(
            &state,
            project_id,
            agent_id,
            &SelectionChange::unchanged(),
            &SelectionChange::set("high"),
        )
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !orchestrator.is_finished(),
        "the orchestrator's write waits for the window's"
    );
    env.clear_seam(TestSeam::SelectionUpdate);
    barrier.release.notify_one();
    window.await.unwrap().unwrap();
    let record = orchestrator.await.unwrap().unwrap();
    assert_eq!(record.model.as_deref(), Some("opus"));
    assert_eq!(record.effort.as_deref(), Some("high"));
}

/// Compiles only while the waits' futures are `Send`, which a request handler
/// or a spawned task needs; running them is incidental.
#[tokio::test]
async fn the_waits_can_run_as_tasks() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let state = Arc::clone(&env.state);
    let home = env.home();
    tokio::spawn(async move {
        let budget = short();
        let _ = wait_send(&state, project_id, Uuid::now_v7(), &budget, &home).await;
        let _ = wait_agent(&state, project_id, agents[0], &budget, &home).await;
        let _ = wait_workflow(&state, project_id, Uuid::now_v7(), &budget).await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_window_that_answers_just_past_the_deadline_still_gets_the_send_out() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let (env, sync, _window, mut requests) = env.with_window(Duration::from_mins(1));
    let heard = agents.clone();
    let window = tokio::spawn(async move {
        let request = requests.recv().await.unwrap();
        // Past the one-second deadline, inside the finishing allowance.
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        assert!(sync.acknowledge(request.request_id, ready(heard)));
    });
    let receipt = send(
        env.handle(),
        env.home(),
        text_send(project_id, agents, "hi"),
        WaitBudget::new(Some(1), false),
    )
    .await
    .unwrap();
    window.await.unwrap();
    assert_eq!(receipt.recipients[0].status, SendStatus::Accepted);
}

#[tokio::test]
async fn a_forward_asks_the_window_only_once_its_source_has_finished() {
    let (env, project_id, agents) = project_with_agents(&["source", "target"]);
    let (env, sync, _window, mut requests) = env.with_window(Duration::from_mins(1));
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("draft", &gate));
    let first = tokio::spawn(send_now(
        env.handle(),
        env.home(),
        text_send(project_id, vec![agents[0]], "write"),
    ));
    let request = requests.recv().await.unwrap();
    assert!(sync.acknowledge(request.request_id, ready(agents.clone())));
    first.await.unwrap().unwrap();
    await_turn_running(&env.state, agents[0]).await;

    let pending = tokio::spawn(forward(
        env.handle(),
        env.home(),
        ForwardRequest {
            project_id,
            sources: vec![latest_answer(project_id, agents[0])],
            recipients: vec![agents[1]],
            text: None,
            prompt: None,
            on_busy: OnBusyPolicy::Refuse,
            idempotency_key: None,
            attachments: Vec::new(),
        },
        WaitBudget::new(Some(30), false),
    ));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        requests.try_recv().is_err(),
        "the window is not asked while the source is still writing"
    );
    gate.notify_one();
    let request = requests.recv().await.unwrap();
    assert!(sync.acknowledge(request.request_id, ready(agents.clone())));
    let receipt = pending.await.unwrap().unwrap();
    assert_eq!(receipt.recipients[0].status, SendStatus::Accepted);
}

#[tokio::test]
async fn a_request_the_window_missed_can_be_fetched_and_answered() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let (env, sync, _window, _requests) = env.with_window(Duration::from_mins(1));
    let pending = tokio::spawn(send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "hi"),
    ));
    // The window was not listening when the request went out, so it fetches
    // what is waiting instead.
    let mut waiting = sync.pending();
    while waiting.is_empty() {
        tokio::time::sleep(Duration::from_millis(5)).await;
        waiting = sync.pending();
    }
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].project_id, project_id);
    assert!(sync.acknowledge(waiting[0].request_id, ready(agents)));
    let receipt = pending.await.unwrap().unwrap();
    assert_eq!(receipt.recipients[0].status, SendStatus::Accepted);
    assert!(sync.pending().is_empty());
}

const LITERAL_WORKFLOW: &str = "name: literal
description: test
steps:
  - label: Greet
    send:
      to: worker
      text: \"hello\"
";

#[tokio::test]
async fn a_workflow_step_naming_an_agent_created_after_the_window_answered_is_refused() {
    let (env, project_id, agents) = workflow_env().await;
    std::fs::write(env.workflows_dir().join("literal.yaml"), LITERAL_WORKFLOW).unwrap();
    let (env, sync, _window, mut requests) = env.with_window(Duration::from_mins(1));
    let state = Arc::clone(&env.state);
    let home = env.home();
    let pending = tokio::spawn(async move {
        run_workflow(
            &state,
            &home,
            RunWorkflowRequest {
                project_id,
                name: "literal".to_owned(),
                is_builtin: false,
                inputs: BTreeMap::new(),
                forward_sources: BTreeMap::new(),
            },
            &WaitBudget::default_wait(),
        )
        .await
    });
    // The window read its roster and is answering; another client creates the
    // agent the step names before the run binds its agents.
    let request = requests.recv().await.unwrap();
    env.agent(project_id, "worker");
    assert!(sync.acknowledge(request.request_id, ready(agents)));
    let err = pending.await.unwrap().unwrap_err();
    assert_eq!(error_code(&err), error_code::DESKTOP_UNAVAILABLE);
    assert!(
        lock(&env.state.workflow_runs).is_empty(),
        "no run was registered"
    );
    assert!(lock(&env.adapter.prompts).is_empty(), "nothing was sent");
}

// --- context fullness ------------------------------------------------------------

#[allow(clippy::cast_precision_loss)] // Test figures are small.
fn context_figures(used: u64, window: u32) -> super::context::ContextFigures {
    super::context::ContextFigures {
        used_tokens: used,
        window_tokens: window,
        fraction: used as f64 / f64::from(window),
    }
}

/// A seeded Claude turn's figures: the parser's after-turn count for the
/// seeded usage (`input_tokens: 10, output_tokens: 5`), with the window the
/// sidecar saved for that message.
const SEEDED_AFTER_TURN: u64 = 15;

fn save_window_for(
    env: &TestEnv,
    project_id: ProjectId,
    agent_id: AgentId,
    key: &str,
    window: u32,
) {
    let root = super::read::project_readonly(&env.state, project_id)
        .unwrap()
        .root;
    switchboard_harness::meta_sidecar::write_context_window(
        &switchboard_harness::meta_sidecar::meta_sidecar_path(&root, agent_id),
        window,
        "claude-sonnet-4-6".to_owned(),
        key.to_owned(),
        Utc::now(),
    )
    .unwrap();
}

#[tokio::test]
async fn list_agents_reports_the_context_a_live_turn_ended_with() {
    let mut env = TestEnv::with_scripts(vec![Script::reply("ack").with_context(1_200, 200_000)]);
    let project_id = env.project("p");
    let agent_id = env.agent(project_id, "a");

    // Never dispatched and no file: settled as nothing, and not pending.
    let listed = list_agents(
        &env.state,
        project_id,
        &env.home(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(listed[0].context, None);
    assert!(!listed[0].context_pending);

    let receipt = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, vec![agent_id], "hi"),
    )
    .await
    .unwrap();
    wait_done(&env, project_id, receipt.send_id).await;
    let listed = list_agents(
        &env.state,
        project_id,
        &env.home(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(listed[0].context, Some(context_figures(1_200, 200_000)));
    assert!(!listed[0].context_pending);
}

#[tokio::test]
async fn list_agents_reads_an_agents_file_once_and_not_again() {
    let mut env = TestEnv::new();
    let project_id = env.project("p");
    let agent_id = env.agent(project_id, "a");
    let at = Utc::now() - chrono::Duration::hours(1);
    env.seed_claude_turns(project_id, agent_id, &[("one", "answer", "a1", at)]);
    save_window_for(&env, project_id, agent_id, "a1", 200_000);
    let env = env.restart();

    let listed = list_agents(
        &env.state,
        project_id,
        &env.home(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        listed[0].context,
        Some(context_figures(SEEDED_AFTER_TURN, 200_000))
    );
    assert!(!listed[0].context_pending);

    // A newer turn appears in the file; a second read would take it.
    env.overwrite_claude_turns(
        project_id,
        agent_id,
        &[
            ("one", "answer", "a1", at),
            ("two", "later", "a2", at + chrono::Duration::minutes(10)),
        ],
    );
    save_window_for(&env, project_id, agent_id, "a2", 100_000);
    let listed = list_agents(
        &env.state,
        project_id,
        &env.home(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        listed[0].context,
        Some(context_figures(SEEDED_AFTER_TURN, 200_000)),
        "the file was not read again"
    );
}

#[tokio::test]
async fn list_agents_does_not_read_a_file_the_project_load_parsed() {
    let mut env = TestEnv::new();
    let project_id = env.project("p");
    let agent_id = env.agent(project_id, "a");
    let at = Utc::now() - chrono::Duration::hours(1);
    env.seed_claude_turns(project_id, agent_id, &[("one", "answer", "a1", at)]);
    save_window_for(&env, project_id, agent_id, "a1", 200_000);
    let env = env.restart();

    load_project_conversation_impl(&env.state, project_id, &env.home())
        .await
        .unwrap();
    env.overwrite_claude_turns(
        project_id,
        agent_id,
        &[
            ("one", "answer", "a1", at),
            ("two", "later", "a2", at + chrono::Duration::minutes(10)),
        ],
    );
    save_window_for(&env, project_id, agent_id, "a2", 100_000);
    let listed = list_agents(
        &env.state,
        project_id,
        &env.home(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        listed[0].context,
        Some(context_figures(SEEDED_AFTER_TURN, 200_000)),
        "the project load's figures stand; no read ran"
    );
    assert!(!listed[0].context_pending);
}

#[tokio::test]
async fn list_agents_settles_an_unreadable_file_as_no_context() {
    let mut env = TestEnv::new();
    let project_id = env.project("p");
    let agent_id = env.agent(project_id, "a");
    env.seed_unreadable_session(project_id, agent_id);
    let env = env.restart();
    let listed = list_agents(
        &env.state,
        project_id,
        &env.home(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(listed[0].context, None);
    assert!(!listed[0].context_pending);
}

#[tokio::test]
async fn read_latest_reply_records_the_figures_it_parsed() {
    let mut env = TestEnv::new();
    let project_id = env.project("p");
    let agent_id = env.agent(project_id, "a");
    let at = Utc::now() - chrono::Duration::hours(1);
    env.seed_claude_turns(project_id, agent_id, &[("one", "answer", "a1", at)]);
    save_window_for(&env, project_id, agent_id, "a1", 200_000);
    let env = env.restart();
    read_latest_reply(&env.state, project_id, agent_id, &env.home())
        .await
        .unwrap();
    assert_eq!(
        env.state.context.lookup(agent_id),
        super::context::ContextState::Known(Some(context_figures(SEEDED_AFTER_TURN, 200_000)))
    );
}

#[tokio::test(start_paused = true)]
async fn list_agents_reports_pending_context_when_a_read_outlasts_its_wait() {
    let mut env = TestEnv::new();
    let project_id = env.project("p");
    let agent_id = env.agent(project_id, "a");
    // A read held open, which the listing joins rather than starting its own.
    let gate = Arc::new(Notify::new());
    let mut done = env.state.context.ensure_reading(agent_id, {
        let gate = Arc::clone(&gate);
        async move {
            gate.notified().await;
            Ok(switchboard_harness::LoadedTranscript::default())
        }
    });
    let started = tokio::time::Instant::now();
    let listed = list_agents(
        &env.state,
        project_id,
        &env.home(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(
        started.elapsed() >= super::context::CONTEXT_READ_WAIT,
        "the listing waited out its bound"
    );
    assert_eq!(listed[0].context, None);
    assert!(listed[0].context_pending, "the read is still running");
    gate.notify_one();
    done.wait_for(|done| *done).await.unwrap();
    assert_eq!(
        env.state.context.lookup(agent_id),
        super::context::ContextState::Known(None),
        "the read finished on its own"
    );
}

#[tokio::test(start_paused = true)]
async fn a_listing_cancelled_during_its_context_wait_is_interrupted() {
    let mut env = TestEnv::new();
    let project_id = env.project("p");
    let agent_id = env.agent(project_id, "a");
    let gate = Arc::new(Notify::new());
    let mut done = env.state.context.ensure_reading(agent_id, {
        let gate = Arc::clone(&gate);
        async move {
            gate.notified().await;
            Ok(switchboard_harness::LoadedTranscript::default())
        }
    });
    let cancel = CancellationToken::new();
    let call = tokio::spawn({
        let (state, home, cancel) = (Arc::clone(&env.state), env.home(), cancel.clone());
        async move { list_agents(&state, project_id, &home, &cancel).await }
    });
    // One virtual second in: the call is inside its five-second wait.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let cancelled_at = tokio::time::Instant::now();
    cancel.cancel();
    let result = call.await.unwrap();
    assert!(matches!(result, Err(AppError::Interrupted)), "{result:?}");
    assert!(
        cancelled_at.elapsed() < Duration::from_secs(1),
        "the stop ended the wait at once"
    );
    gate.notify_one();
    done.wait_for(|done| *done).await.unwrap();
}

/// Both listings ask each agent's actor whether it is busy, and an actor
/// preparing a turn answers nothing until it is ready, so the stop must end
/// that wait too.
#[tokio::test]
async fn a_listing_cancelled_during_a_busy_check_is_interrupted() {
    let held = Arc::new(Notify::new());
    let mut env = TestEnv::with_scripts(vec![Script::reply("ack").holding_dispatch(&held)]);
    let project_id = env.project("p");
    let agent_id = env.agent(project_id, "a");
    // Known context first, so the listings go straight to their busy checks.
    list_agents(
        &env.state,
        project_id,
        &env.home(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    // A send whose dispatch the adapter holds: the actor is preparing the turn.
    let send = tokio::spawn(send_now(
        env.handle(),
        env.home(),
        text_send(project_id, vec![agent_id], "hi"),
    ));
    for _ in 0..500 {
        if !lock(&env.adapter.prompts).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        !lock(&env.adapter.prompts).is_empty(),
        "the actor reached the adapter"
    );

    let cancel = CancellationToken::new();
    let agents = tokio::spawn({
        let (state, home, cancel) = (Arc::clone(&env.state), env.home(), cancel.clone());
        async move { list_agents(&state, project_id, &home, &cancel).await }
    });
    let projects = tokio::spawn({
        let (state, cancel) = (Arc::clone(&env.state), cancel.clone());
        async move { list_projects(&state, false, &cancel).await }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !agents.is_finished() && !projects.is_finished(),
        "both are blocked on the actor"
    );
    cancel.cancel();
    let agents = agents.await.unwrap();
    let projects = projects.await.unwrap();
    assert!(matches!(agents, Err(AppError::Interrupted)), "{agents:?}");
    assert!(
        matches!(projects, Err(AppError::Interrupted)),
        "{projects:?}"
    );

    held.notify_one();
    let receipt = send.await.unwrap().unwrap();
    wait_done(&env, project_id, receipt.send_id).await;
}

// --- compaction ----------------------------------------------------------------------

use super::compact::{CompactRequest, compact};

/// A project whose one Claude agent has a session (one seeded reply), so the
/// compaction gates pass. The scripted adapter appends every later reply to
/// that session file.
fn compactable_agent() -> (TestEnv, ProjectId, AgentId) {
    let mut env = TestEnv::new();
    let project_id = env.project("p");
    let agent_id = env.agent(project_id, "a");
    env.seed_claude_turn(project_id, agent_id, "seeded", "s1");
    (env.restart(), project_id, agent_id)
}

fn compact_request(project_id: ProjectId, agent_id: AgentId, key: Option<&str>) -> CompactRequest {
    CompactRequest {
        project_id,
        agent_id,
        idempotency_key: key.map(str::to_owned),
    }
}

async fn compact_now(
    env: &TestEnv,
    request: CompactRequest,
) -> Result<super::ledger::SendReceipt, AppError> {
    compact(
        env.handle(),
        env.home(),
        request,
        WaitBudget::default_wait(),
    )
    .await
}

fn announced_compactions(env: &TestEnv) -> Vec<serde_json::Value> {
    env.emitter
        .snapshot()
        .into_iter()
        .filter(|(_, event)| event["type"] == "send_accepted" && event["kind"] == "compaction")
        .map(|(_, event)| event)
        .collect()
}

#[tokio::test]
async fn a_compaction_of_an_idle_agent_completes_and_reports_the_compacted_size() {
    let (env, project_id, agent_id) = compactable_agent();
    let receipt = compact_now(&env, compact_request(project_id, agent_id, None))
        .await
        .unwrap();
    assert_eq!(receipt.recipients.len(), 1);
    assert_eq!(receipt.recipients[0].status, SendStatus::Accepted);

    let reply = wait_done(&env, project_id, receipt.send_id).await;
    assert!(reply.done);
    assert_eq!(reply.recipients[0].status, SendStatus::Completed);
    assert!(
        reply.recipients[0].text.as_deref().unwrap_or("").is_empty(),
        "a compaction emits no reply text"
    );
    assert_eq!(*lock(&env.adapter.compactions), vec![agent_id]);
    // Announced at admission with its kind and no text, so the window shows
    // the compaction row.
    let announced = announced_compactions(&env);
    assert_eq!(announced.len(), 1);
    assert_eq!(announced[0]["text"], "");
    assert_eq!(announced[0]["send_id"], serde_json::json!(receipt.send_id));
    // The compacted size reaches the context figures.
    let listed = list_agents(
        &env.state,
        project_id,
        &env.home(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(listed[0].context, Some(context_figures(18_000, 200_000)));
}

#[tokio::test]
async fn a_compaction_queues_behind_a_running_turn_and_runs_after_it() {
    let (env, project_id, agent_id) = compactable_agent();
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("busy", &gate));
    let busy = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, vec![agent_id], "work"),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, agent_id).await;

    let receipt = compact_now(&env, compact_request(project_id, agent_id, None))
        .await
        .unwrap();
    assert_eq!(receipt.recipients[0].status, SendStatus::Accepted);
    let waiting = wait_send(
        &env.state,
        project_id,
        receipt.send_id,
        &short(),
        &env.home(),
    )
    .await
    .unwrap();
    assert!(!waiting.done, "queued behind the running turn");
    assert!(lock(&env.adapter.compactions).is_empty());

    gate.notify_one();
    wait_done(&env, project_id, busy.send_id).await;
    let reply = wait_done(&env, project_id, receipt.send_id).await;
    assert_eq!(reply.recipients[0].status, SendStatus::Completed);
    assert_eq!(*lock(&env.adapter.compactions), vec![agent_id]);
}

#[tokio::test]
async fn cancel_send_cancels_a_queued_compaction() {
    let (env, project_id, agent_id) = compactable_agent();
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("busy", &gate));
    let busy = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, vec![agent_id], "work"),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, agent_id).await;
    let receipt = compact_now(&env, compact_request(project_id, agent_id, None))
        .await
        .unwrap();

    cancel_send(&env.state, project_id, receipt.send_id, None).unwrap();
    gate.notify_one();
    wait_done(&env, project_id, busy.send_id).await;
    let reply = wait_done(&env, project_id, receipt.send_id).await;
    assert_eq!(reply.recipients[0].status, SendStatus::Cancelled);
    assert!(lock(&env.adapter.compactions).is_empty(), "nothing ran");
}

#[tokio::test]
async fn an_unsupported_harness_and_an_agent_without_a_session_are_refused_before_the_window() {
    let mut env = TestEnv::new();
    let project_id = env.project("p");
    let no_session = env.agent(project_id, "fresh");
    let antigravity = crate::commands::create_agent_in_project_impl(
        &env.state,
        project_id,
        "ag",
        switchboard_core::HarnessKind::Antigravity,
        switchboard_core::AgentSelection::default(),
    )
    .unwrap()
    .id;
    let env = env.restart();

    let err = compact_now(&env, compact_request(project_id, antigravity, Some("k1")))
        .await
        .unwrap_err();
    assert_eq!(error_code(&err), error_code::COMPACTION_UNSUPPORTED);
    let err = compact_now(&env, compact_request(project_id, no_session, Some("k2")))
        .await
        .unwrap_err();
    assert_eq!(error_code(&err), error_code::NO_SESSION);
    assert!(
        lock(&env.sync.ready_calls).is_empty(),
        "refused before any window round trip"
    );
    assert!(announced_compactions(&env).is_empty());
}

#[tokio::test]
async fn a_repeat_with_the_same_key_returns_the_same_receipt_and_queues_nothing() {
    let (env, project_id, agent_id) = compactable_agent();
    let first = compact_now(&env, compact_request(project_id, agent_id, Some("k")))
        .await
        .unwrap();
    wait_done(&env, project_id, first.send_id).await;
    let again = compact_now(&env, compact_request(project_id, agent_id, Some("k")))
        .await
        .unwrap();
    assert_eq!(again.send_id, first.send_id);
    assert_eq!(lock(&env.adapter.compactions).len(), 1);
}

#[tokio::test]
async fn a_request_while_a_compaction_is_queued_returns_its_receipt_whatever_the_key() {
    let (env, project_id, agent_id) = compactable_agent();
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("busy", &gate));
    let busy = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, vec![agent_id], "work"),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, agent_id).await;

    let first = compact_now(&env, compact_request(project_id, agent_id, Some("k1")))
        .await
        .unwrap();
    let other_key = compact_now(&env, compact_request(project_id, agent_id, Some("k2")))
        .await
        .unwrap();
    let no_key = compact_now(&env, compact_request(project_id, agent_id, None))
        .await
        .unwrap();
    assert_eq!(other_key.send_id, first.send_id);
    assert_eq!(no_key.send_id, first.send_id);
    // The new key is associated with the reused receipt: a repeat under it
    // returns the same receipt too.
    let repeat = compact_now(&env, compact_request(project_id, agent_id, Some("k2")))
        .await
        .unwrap();
    assert_eq!(repeat.send_id, first.send_id);

    gate.notify_one();
    wait_done(&env, project_id, busy.send_id).await;
    wait_done(&env, project_id, first.send_id).await;
    assert_eq!(
        lock(&env.adapter.compactions).len(),
        1,
        "one compaction queued"
    );
}

/// The window stand-in holds every readiness request until released.
fn hold_window(env: &TestEnv) -> Arc<crate::state::MaintenanceBarrier> {
    let hold = Arc::new(crate::state::MaintenanceBarrier::default());
    *lock(&env.sync.hold) = Some(Arc::clone(&hold));
    hold
}

fn spawn_compact(
    env: &TestEnv,
    request: CompactRequest,
) -> tokio::task::JoinHandle<Result<super::ledger::SendReceipt, AppError>> {
    let (handle, home) = (env.handle(), env.home());
    tokio::spawn(async move { compact(handle, home, request, WaitBudget::default_wait()).await })
}

/// The send `key` was claimed for, which exists as soon as the request made
/// its claim.
fn claimed(env: &TestEnv, project_id: ProjectId, key: &str) -> Uuid {
    env.state
        .ledger
        .send_id_for_key(project_id, key)
        .unwrap_or_else(|| panic!("{key} is claimed"))
}

/// Wait until `key`'s claim points at `send_id`: the request joined that send.
/// With the window held, the send cannot settle, so this is a join and not a
/// reuse; a request that queued its own compaction instead fails here rather
/// than hanging at the window.
async fn joined(env: &TestEnv, project_id: ProjectId, key: &str, send_id: Uuid) {
    for _ in 0..500 {
        if env.state.ledger.send_id_for_key(project_id, key) == Some(send_id) {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("{key} did not join {send_id}");
}

#[tokio::test]
async fn concurrent_requests_with_different_keys_queue_one_compaction() {
    let (env, project_id, agent_id) = compactable_agent();
    // The first request is parked at the window wait when the second arrives,
    // which joins it.
    let hold = hold_window(&env);
    let first = spawn_compact(&env, compact_request(project_id, agent_id, Some("k1")));
    hold.entered.notified().await;
    let first_id = claimed(&env, project_id, "k1");
    let second = spawn_compact(&env, compact_request(project_id, agent_id, Some("k2")));
    joined(&env, project_id, "k2", first_id).await;
    hold.release.notify_one();
    let (one, two) = (
        first.await.unwrap().unwrap(),
        second.await.unwrap().unwrap(),
    );
    assert_eq!(one.send_id, first_id);
    assert_eq!(two.send_id, first_id);
    wait_done(&env, project_id, one.send_id).await;
    assert_eq!(lock(&env.adapter.compactions).len(), 1);
}

#[tokio::test]
async fn a_compaction_then_a_message_then_a_compaction_queues_two() {
    let (env, project_id, agent_id) = compactable_agent();
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("busy", &gate));
    let busy = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, vec![agent_id], "work"),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, agent_id).await;

    let first = compact_now(&env, compact_request(project_id, agent_id, None))
        .await
        .unwrap();
    let mut message = text_send(project_id, vec![agent_id], "after");
    message.on_busy = OnBusyPolicy::Queue;
    let queued = send_now(env.handle(), env.home(), message).await.unwrap();
    let second = compact_now(&env, compact_request(project_id, agent_id, None))
        .await
        .unwrap();
    assert_ne!(second.send_id, first.send_id, "a message sits between them");

    gate.notify_one();
    wait_done(&env, project_id, busy.send_id).await;
    wait_done(&env, project_id, first.send_id).await;
    wait_done(&env, project_id, queued.send_id).await;
    wait_done(&env, project_id, second.send_id).await;
    assert_eq!(lock(&env.adapter.compactions).len(), 2);
}

#[tokio::test]
async fn a_message_registered_before_a_compaction_but_admitted_after_it_keeps_the_next_compaction_separate()
 {
    // The message is a forward whose source is still replying: registered at
    // its claim, admitted only once the source finishes. The first compaction
    // is requested meanwhile, admitted at once, and parks running.
    let mut env = TestEnv::new();
    let project_id = env.project("p");
    let target = env.agent(project_id, "target");
    let source = env.agent(project_id, "source");
    env.seed_claude_turn(project_id, target, "seeded", "s1");
    let env = env.restart();
    let source_gate = Arc::new(Notify::new());
    env.adapter
        .push(Script::parked("source reply", &source_gate));
    let source_send = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, vec![source], "think"),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, source).await;

    let held = tokio::spawn({
        let (handle, home) = (env.handle(), env.home());
        async move {
            forward(
                handle,
                home,
                ForwardRequest {
                    project_id,
                    sources: vec![latest_answer(project_id, source)],
                    recipients: vec![target],
                    text: Some("see this".to_owned()),
                    prompt: None,
                    on_busy: OnBusyPolicy::Queue,
                    idempotency_key: Some("held-forward".to_owned()),
                    attachments: Vec::new(),
                },
                WaitBudget::default_wait(),
            )
            .await
        }
    });
    // Registered (its key is claimed) but not admitted: it waits on the source.
    for _ in 0..500 {
        if env
            .state
            .ledger
            .send_id_for_key(project_id, "held-forward")
            .is_some()
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        env.state
            .ledger
            .send_id_for_key(project_id, "held-forward")
            .is_some()
    );

    let compacting = Arc::new(Notify::new());
    env.adapter.push(Script::parked("compacting", &compacting));
    let first = compact_now(&env, compact_request(project_id, target, None))
        .await
        .unwrap();
    await_turn_running(&env.state, target).await;

    // The source finishes: the forward composes and is admitted, after the
    // compaction.
    source_gate.notify_one();
    wait_done(&env, project_id, source_send.send_id).await;
    let held = held.await.unwrap().unwrap();
    assert_eq!(held.recipients[0].status, SendStatus::Accepted);

    let second = compact_now(&env, compact_request(project_id, target, None))
        .await
        .unwrap();
    assert_ne!(
        second.send_id, first.send_id,
        "the message was admitted after the first compaction, so a new one queues"
    );

    compacting.notify_one();
    wait_done(&env, project_id, first.send_id).await;
    wait_done(&env, project_id, held.send_id).await;
    wait_done(&env, project_id, second.send_id).await;
    assert_eq!(lock(&env.adapter.compactions).len(), 2);
}

#[tokio::test]
async fn a_request_during_the_window_wait_gets_the_first_receipt_or_its_error() {
    let (env, project_id, agent_id) = compactable_agent();
    let hold = hold_window(&env);
    let first = spawn_compact(&env, compact_request(project_id, agent_id, Some("k1")));
    hold.entered.notified().await;
    let first_id = claimed(&env, project_id, "k1");
    let second = spawn_compact(&env, compact_request(project_id, agent_id, Some("k2")));
    joined(&env, project_id, "k2", first_id).await;
    hold.release.notify_one();
    let (first, second) = (
        first.await.unwrap().unwrap(),
        second.await.unwrap().unwrap(),
    );
    assert_eq!(first.send_id, first_id);
    assert_eq!(second.send_id, first_id);
    wait_done(&env, project_id, first_id).await;

    // The window times out: both callers get the error, never an id the
    // ledger does not know.
    *lock(&env.sync.times_out) = true;
    let first = spawn_compact(&env, compact_request(project_id, agent_id, Some("k3")));
    hold.entered.notified().await;
    let third_id = claimed(&env, project_id, "k3");
    let second = spawn_compact(&env, compact_request(project_id, agent_id, Some("k4")));
    joined(&env, project_id, "k4", third_id).await;
    hold.release.notify_one();
    let (first, second) = (
        first.await.unwrap().unwrap_err(),
        second.await.unwrap().unwrap_err(),
    );
    assert_eq!(error_code(&first), error_code::PROJECT_LOAD_TIMEOUT);
    assert_eq!(error_code(&second), error_code::PROJECT_LOAD_TIMEOUT);
}

#[tokio::test]
async fn after_a_restart_a_compaction_reports_unknown() {
    let (env, project_id, agent_id) = compactable_agent();
    let receipt = compact_now(&env, compact_request(project_id, agent_id, None))
        .await
        .unwrap();
    wait_done(&env, project_id, receipt.send_id).await;
    let env = env.restart();
    let reply = wait_done(&env, project_id, receipt.send_id).await;
    assert!(reply.done);
    assert_eq!(reply.source, WaitSource::Unknown);
    assert!(reply.recipients.is_empty());
}

#[tokio::test]
async fn the_latest_answer_survives_an_orchestrators_compaction() {
    let (env, project_id, agent_id) = compactable_agent();
    let receipt = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, vec![agent_id], "hi"),
    )
    .await
    .unwrap();
    wait_done(&env, project_id, receipt.send_id).await;
    let compacted = compact_now(&env, compact_request(project_id, agent_id, None))
        .await
        .unwrap();
    wait_done(&env, project_id, compacted.send_id).await;

    let output = read_latest_reply(&env.state, project_id, agent_id, &env.home())
        .await
        .unwrap();
    assert_eq!(output.status, LatestReplyStatus::Completed);
    assert_eq!(output.text.as_deref(), Some("ack: hi"));
    assert_eq!(output.send_id, Some(receipt.send_id));
    let idle = wait_agent(
        &env.state,
        project_id,
        agent_id,
        &WaitBudget::new(Some(5), false),
        &env.home(),
    )
    .await
    .unwrap();
    assert_eq!(idle.status, AgentWaitStatus::Idle);
    assert_eq!(
        idle.output.as_ref().and_then(|o| o.text.as_deref()),
        Some("ack: hi")
    );
}

#[tokio::test]
async fn an_agent_of_another_project_is_not_a_recipient() {
    let mut env = TestEnv::new();
    let p1 = env.project("one");
    let p2 = env.project("two");
    let elsewhere = crate::commands::create_agent_in_project_impl(
        &env.state,
        p2,
        "ag",
        switchboard_core::HarnessKind::Antigravity,
        switchboard_core::AgentSelection::default(),
    )
    .unwrap()
    .id;
    let env = env.restart();
    let err = compact_now(&env, compact_request(p1, elsewhere, None))
        .await
        .unwrap_err();
    assert_eq!(error_code(&err), error_code::UNKNOWN_RECIPIENT);
}

// --- forks --------------------------------------------------------------------

use super::fork::{ForkReceipt, ForkRequest, fork};

/// A project whose one Claude agent has a session (one seeded reply), so it
/// can be forked. The scripted adapter starts a fork's first turn from a copy
/// of that session, as `--fork-session` does.
fn forkable_source() -> (TestEnv, ProjectId, AgentId) {
    compactable_agent()
}

fn fork_request(
    project_id: ProjectId,
    agent_id: AgentId,
    text: &str,
    key: Option<&str>,
) -> ForkRequest {
    ForkRequest {
        project_id,
        agent_id,
        body: Body::Text {
            text: text.to_owned(),
        },
        idempotency_key: key.map(str::to_owned),
        attachments: Vec::new(),
    }
}

async fn fork_now(env: &TestEnv, request: ForkRequest) -> Result<ForkReceipt, AppError> {
    fork(
        env.handle(),
        env.home(),
        request,
        WaitBudget::default_wait(),
    )
    .await
}

fn spawn_fork(
    env: &TestEnv,
    request: ForkRequest,
) -> tokio::task::JoinHandle<Result<ForkReceipt, AppError>> {
    let (handle, home) = (env.handle(), env.home());
    tokio::spawn(async move { fork(handle, home, request, WaitBudget::default_wait()).await })
}

fn roster(env: &TestEnv, project_id: ProjectId) -> Vec<switchboard_core::AgentRecord> {
    list_project_agents_readonly_impl(&env.state, project_id).unwrap()
}

fn fork_of(receipt: &ForkReceipt) -> switchboard_core::AgentRecord {
    receipt.fork.clone().expect("the fork exists")
}

#[tokio::test]
async fn a_fork_of_an_idle_source_sends_and_holds_the_sources_turns_and_the_reply() {
    let (env, project_id, source) = forkable_source();
    let receipt = fork_now(&env, fork_request(project_id, source, "branch", None))
        .await
        .unwrap();
    let fork = fork_of(&receipt);
    assert_eq!(fork.name, "a-fork");
    assert_eq!(fork.project_id, project_id);
    assert!(fork.forked_from_session.is_some());
    assert_eq!(receipt.send.recipients.len(), 1);
    assert_eq!(receipt.send.recipients[0].agent_id, fork.id);
    assert_eq!(receipt.send.recipients[0].status, SendStatus::Accepted);
    assert_eq!(
        *lock(&env.sync.forks_carried),
        vec![SyncFork {
            fork_id: fork.id,
            source_agent_id: source
        }],
        "the readiness request carried the fork"
    );
    let done = wait_done(&env, project_id, receipt.send.send_id).await;
    assert_eq!(done.recipients[0].text.as_deref(), Some("ack: branch"));

    let conversation = read_conversation(&env.state, project_id, None, &env.home())
        .await
        .unwrap();
    let texts = |agent: AgentId| -> Vec<String> {
        conversation
            .items
            .iter()
            .filter_map(|item| match item {
                DigestItem::AgentTurn { agent_id, text, .. } if *agent_id == agent => {
                    Some(text.clone())
                }
                _ => None,
            })
            .collect()
    };
    assert_eq!(texts(fork.id), vec!["seeded", "ack: branch"]);
    assert_eq!(texts(source), vec!["seeded"], "the source is untouched");
}

#[tokio::test]
async fn a_busy_source_is_refused_and_nothing_is_created() {
    let (env, project_id, source) = forkable_source();
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("busy", &gate));
    let busy = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, vec![source], "work"),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, source).await;

    let err = fork_now(&env, fork_request(project_id, source, "branch", Some("k")))
        .await
        .unwrap_err();
    assert_eq!(error_code(&err), error_code::SOURCE_BUSY);
    assert_eq!(roster(&env, project_id).len(), 1);

    // The key was released: once the source finishes, the same request forks.
    gate.notify_one();
    wait_done(&env, project_id, busy.send_id).await;
    let receipt = fork_now(&env, fork_request(project_id, source, "branch", Some("k")))
        .await
        .unwrap();
    assert!(receipt.fork.is_some());
    assert_eq!(roster(&env, project_id).len(), 2);
}

#[tokio::test]
async fn a_source_without_a_session_gets_no_session() {
    let mut env = TestEnv::new();
    let project_id = env.project("p");
    let source = env.agent(project_id, "a");
    let err = fork_now(&env, fork_request(project_id, source, "branch", None))
        .await
        .unwrap_err();
    assert_eq!(error_code(&err), error_code::NO_SESSION);
    assert_eq!(roster(&env, project_id).len(), 1);
}

#[tokio::test]
async fn an_agent_of_a_harness_that_cannot_fork_gets_fork_unsupported() {
    let mut env = TestEnv::new();
    let project_id = env.project("p");
    let agy = crate::commands::create_agent_in_project_impl(
        &env.state,
        project_id,
        "g",
        switchboard_core::HarnessKind::Antigravity,
        switchboard_core::AgentSelection::default(),
    )
    .unwrap()
    .id;
    let err = fork_now(&env, fork_request(project_id, agy, "branch", None))
        .await
        .unwrap_err();
    assert_eq!(error_code(&err), error_code::FORK_UNSUPPORTED);
    assert_eq!(roster(&env, project_id).len(), 1);
}

#[tokio::test]
async fn an_agent_of_another_project_cannot_be_forked_into_this_one() {
    let (mut env, p1, elsewhere) = forkable_source();
    let p2 = env.project("other");
    let err = fork_now(&env, fork_request(p2, elsewhere, "branch", None))
        .await
        .unwrap_err();
    assert_eq!(error_code(&err), error_code::UNKNOWN_RECIPIENT);
    assert_eq!(roster(&env, p1).len(), 1);
}

#[tokio::test]
async fn a_repeat_with_the_same_key_returns_the_same_fork_and_concurrent_repeats_make_one() {
    let (env, project_id, source) = forkable_source();
    let hold = hold_window(&env);
    let first = spawn_fork(&env, fork_request(project_id, source, "branch", Some("k")));
    hold.entered.notified().await;
    // Joined at its claim: the key is taken and no fork exists yet.
    let second = spawn_fork(&env, fork_request(project_id, source, "branch", Some("k")));
    hold.release.notify_one();
    let (one, two) = (
        first.await.unwrap().unwrap(),
        second.await.unwrap().unwrap(),
    );
    assert_eq!(one, two);
    assert_eq!(one.send.recipients[0].status, SendStatus::Accepted);
    wait_done(&env, project_id, one.send.send_id).await;

    let later = fork_now(&env, fork_request(project_id, source, "branch", Some("k")))
        .await
        .unwrap();
    assert_eq!(later, one);
    assert_eq!(roster(&env, project_id).len(), 2);
    assert_eq!(lock(&env.adapter.prompts).len(), 1);

    // A different key is a different request: a second fork.
    hold.release.notify_one();
    let other = fork_now(&env, fork_request(project_id, source, "branch", Some("k2")))
        .await
        .unwrap();
    assert_ne!(fork_of(&other).id, fork_of(&one).id);
    assert_eq!(fork_of(&other).name, "a-fork-2");
    assert_eq!(roster(&env, project_id).len(), 3);
}

#[tokio::test]
async fn a_message_refused_after_creation_and_repeated_with_the_same_key_sends_once_to_the_one_fork()
 {
    let (env, project_id, source) = forkable_source();
    // The window does not hear the fork, so the message is refused at
    // dispatch, after the fork exists.
    *lock(&env.sync.hearing) = Hearing::Only(vec![source]);
    let refused = fork_now(&env, fork_request(project_id, source, "branch", Some("k")))
        .await
        .unwrap();
    let fork = fork_of(&refused);
    assert_eq!(refused.send.recipients[0].status, SendStatus::Rejected);
    assert_eq!(
        refused.send.recipients[0].code,
        Some(error_code::DESKTOP_UNAVAILABLE)
    );
    assert!(
        wait_done(&env, project_id, refused.send.send_id).await.done,
        "a wait on the refused attempt returns at once"
    );
    assert_eq!(roster(&env, project_id).len(), 2);

    *lock(&env.sync.hearing) = Hearing::Everyone;
    let resent = fork_now(&env, fork_request(project_id, source, "branch", Some("k")))
        .await
        .unwrap();
    assert_eq!(fork_of(&resent).id, fork.id);
    assert_ne!(resent.send.send_id, refused.send.send_id);
    assert_eq!(resent.send.recipients[0].status, SendStatus::Accepted);
    wait_done(&env, project_id, resent.send.send_id).await;
    assert_eq!(roster(&env, project_id).len(), 2);
    assert_eq!(lock(&env.adapter.prompts).len(), 1);

    // The message went out: a further repeat returns its receipt.
    let again = fork_now(&env, fork_request(project_id, source, "branch", Some("k")))
        .await
        .unwrap();
    assert_eq!(again.send.send_id, resent.send.send_id);
    assert_eq!(lock(&env.adapter.prompts).len(), 1);
}

#[tokio::test]
async fn a_message_cancelled_while_preparing_is_not_resent_by_a_repeat() {
    let (env, project_id, source) = forkable_source();
    let hold = hold_window(&env);
    let first = spawn_fork(&env, fork_request(project_id, source, "branch", Some("k")));
    hold.entered.notified().await;
    let send_id = env.state.ledger.send_id_for_key(project_id, "k").unwrap();
    cancel_send(&env.state, project_id, send_id, None).unwrap();
    hold.release.notify_one();
    let cancelled = first.await.unwrap().unwrap();
    assert!(
        cancelled.fork.is_some(),
        "the fork was created before the cancel"
    );
    assert_eq!(cancelled.send.recipients[0].status, SendStatus::Cancelled);

    let repeat = fork_now(&env, fork_request(project_id, source, "branch", Some("k")))
        .await
        .unwrap();
    assert_eq!(repeat, cancelled);
    assert!(lock(&env.adapter.prompts).is_empty());
    assert_eq!(roster(&env, project_id).len(), 2);
}

#[tokio::test]
async fn a_window_that_never_answers_reports_the_fork_and_the_repeat_sends_once_it_does() {
    let (env, project_id, source) = forkable_source();
    *lock(&env.sync.times_out) = true;
    let timed_out = fork_now(&env, fork_request(project_id, source, "branch", Some("k")))
        .await
        .unwrap();
    let fork = fork_of(&timed_out);
    assert_eq!(timed_out.send.recipients[0].status, SendStatus::Rejected);
    assert_eq!(
        timed_out.send.recipients[0].code,
        Some(error_code::PROJECT_LOAD_TIMEOUT)
    );
    assert!(roster(&env, project_id).iter().any(|a| a.id == fork.id));

    *lock(&env.sync.times_out) = false;
    let resent = fork_now(&env, fork_request(project_id, source, "branch", Some("k")))
        .await
        .unwrap();
    assert_eq!(fork_of(&resent).id, fork.id);
    assert_eq!(resent.send.recipients[0].status, SendStatus::Accepted);
    wait_done(&env, project_id, resent.send.send_id).await;
    assert_eq!(roster(&env, project_id).len(), 2);
}

#[tokio::test]
async fn a_repeat_after_the_refused_attempts_entry_was_dropped_still_returns_the_same_fork() {
    let (env, project_id, source) = forkable_source();
    *lock(&env.sync.times_out) = true;
    let refused = fork_now(&env, fork_request(project_id, source, "branch", Some("k")))
        .await
        .unwrap();
    let fork = fork_of(&refused);
    // The refused attempt is finished, so the count bound drops it first.
    for _ in 0..=super::ledger::MAX_ENTRIES {
        let id = Uuid::now_v7();
        env.state
            .ledger
            .register_send(id, project_id, &[], CancellationToken::new());
        env.state.ledger.settle(id);
    }
    assert!(
        env.state.ledger.send(refused.send.send_id).is_none(),
        "the refused attempt's entry was dropped"
    );

    *lock(&env.sync.times_out) = false;
    let resent = fork_now(&env, fork_request(project_id, source, "branch", Some("k")))
        .await
        .unwrap();
    assert_eq!(fork_of(&resent).id, fork.id);
    assert_eq!(resent.send.recipients[0].status, SendStatus::Accepted);
    assert_eq!(roster(&env, project_id).len(), 2);
}

#[tokio::test]
async fn a_repeat_that_joined_the_refused_attempt_gets_its_receipt_and_the_resumed_attempt_is_joinable()
 {
    let (env, project_id, source) = forkable_source();
    *lock(&env.sync.times_out) = true;
    let hold = hold_window(&env);
    let first = spawn_fork(&env, fork_request(project_id, source, "branch", Some("k")));
    hold.entered.notified().await;
    let joiner = spawn_fork(&env, fork_request(project_id, source, "branch", Some("k")));
    hold.release.notify_one();
    let (first, joiner) = (
        first.await.unwrap().unwrap(),
        joiner.await.unwrap().unwrap(),
    );
    assert_eq!(joiner, first);
    assert_eq!(first.send.recipients[0].status, SendStatus::Rejected);
    let fork = fork_of(&first);

    // The next repeat resumes; one arriving while it prepares joins it.
    *lock(&env.sync.times_out) = false;
    let resumed = spawn_fork(&env, fork_request(project_id, source, "branch", Some("k")));
    hold.entered.notified().await;
    let late = spawn_fork(&env, fork_request(project_id, source, "branch", Some("k")));
    hold.release.notify_one();
    let (resumed, late) = (
        resumed.await.unwrap().unwrap(),
        late.await.unwrap().unwrap(),
    );
    assert_eq!(late, resumed);
    assert_eq!(fork_of(&resumed).id, fork.id);
    assert_ne!(resumed.send.send_id, first.send.send_id);
    assert_eq!(resumed.send.recipients[0].status, SendStatus::Accepted);
    wait_done(&env, project_id, resumed.send.send_id).await;
    assert_eq!(roster(&env, project_id).len(), 2);
    assert_eq!(lock(&env.adapter.prompts).len(), 1);
}

#[tokio::test]
async fn stopping_the_server_after_the_fork_exists_leaves_it_with_nothing_sent() {
    let (env, project_id, source) = forkable_source();
    let hold = hold_window(&env);
    let stop = CancellationToken::new();
    let (handle, home) = (env.handle(), env.home());
    let budget = WaitBudget::default_wait().with_cancel(stop.clone());
    let task = tokio::spawn(async move {
        fork(
            handle,
            home,
            fork_request(project_id, source, "branch", Some("k")),
            budget,
        )
        .await
    });
    hold.entered.notified().await;
    stop.cancel();
    hold.release.notify_one();
    // The window wait ends on the stop and the task settles at once: the
    // caller gets the receipt naming the fork, its message refused.
    let receipt = task.await.unwrap().unwrap();
    let fork_record = fork_of(&receipt);
    assert_eq!(receipt.send.recipients[0].status, SendStatus::Rejected);
    assert!(lock(&env.adapter.prompts).is_empty(), "nothing was sent");
    assert!(
        roster(&env, project_id)
            .iter()
            .any(|a| a.id == fork_record.id)
    );

    // Access on again: the same request sends to the fork.
    hold.release.notify_one();
    let resent = fork_now(&env, fork_request(project_id, source, "branch", Some("k")))
        .await
        .unwrap();
    assert_eq!(fork_of(&resent).id, fork_record.id);
    assert_eq!(resent.send.recipients[0].status, SendStatus::Accepted);
    wait_done(&env, project_id, resent.send.send_id).await;
    assert_eq!(roster(&env, project_id).len(), 2);
}

#[tokio::test]
async fn the_readiness_request_carries_the_fork() {
    let (env, project_id, source) = forkable_source();
    let (env, sync, _window, mut requests) = env.with_window(Duration::from_mins(1));
    let task = spawn_fork(&env, fork_request(project_id, source, "branch", None));
    let request = requests.recv().await.unwrap();
    let carried = request.fork.expect("the request names the fork");
    assert_eq!(carried.source_agent_id, source);
    assert_eq!(sync.pending()[0].fork, Some(carried));
    assert!(
        roster(&env, project_id)
            .iter()
            .any(|a| a.id == carried.fork_id),
        "the fork exists when the window is asked"
    );
    sync.acknowledge(
        request.request_id,
        SyncAck::Ready {
            listening_agents: vec![source, carried.fork_id],
        },
    );
    let receipt = task.await.unwrap().unwrap();
    assert_eq!(fork_of(&receipt).id, carried.fork_id);
    assert_eq!(receipt.send.recipients[0].status, SendStatus::Accepted);
}

/// The parent and fork ids of the Codex fork test, as the app's fork tests
/// use them.
const CODEX_FORK_PARENT: &str = "01a12c22-a3f8-71e2-b51e-ca72562cfabb";
const CODEX_FORK_CHILD: &str = "01a12c22-df64-7103-9fc3-92f2e3595c02";

fn codex_fork_date() -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(2026, 10, 11).unwrap()
}

/// A stand-in for `codex exec fork`, as the app's fork tests use: it writes
/// the fork's rollout under `home` and prints `thread.started` with its id.
fn codex_fork_shim(home: &std::path::Path) -> PathBuf {
    let folder = home.join(".codex/sessions/2026/10/11");
    let script = format!(
        "#!/bin/sh\nmkdir -p '{folder}'\n: > '{folder}/rollout-2026-10-11T00-00-01-{CODEX_FORK_CHILD}.jsonl'\necho '{{\"type\":\"thread.started\",\"thread_id\":\"{CODEX_FORK_CHILD}\"}}'\n",
        folder = folder.display(),
    );
    let shim = home.join("codex");
    std::fs::write(&shim, script).unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    shim
}

/// A Codex agent whose rollout exists under the env's home, so it has a
/// session to fork.
fn codex_source(env: &TestEnv, project_id: ProjectId) -> AgentId {
    let agent = crate::commands::create_agent_in_project_impl(
        &env.state,
        project_id,
        "cx",
        switchboard_core::HarnessKind::Codex,
        switchboard_core::AgentSelection {
            model: Some("gpt-6-luna".to_owned()),
            effort: Some("low".to_owned()),
            ..switchboard_core::AgentSelection::default()
        },
    )
    .unwrap();
    crate::commands::set_agent_session_locator_impl(
        &env.state,
        agent.id,
        SessionLocator::Codex {
            thread_id: CODEX_FORK_PARENT.to_owned(),
            partition_date: codex_fork_date(),
        },
    )
    .unwrap();
    let folder = env.home().join(".codex/sessions/2026/10/11");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join(format!(
            "rollout-2026-10-11T00-00-00-{CODEX_FORK_PARENT}.jsonl"
        )),
        "",
    )
    .unwrap();
    agent.id
}

#[tokio::test]
async fn a_codex_source_is_forked_through_codex_and_the_fork_is_an_ordinary_codex_agent() {
    let env = TestEnv::new();
    let shim = codex_fork_shim(&env.home());
    let mut env = env.with_codex_fork_binary(shim);
    let project_id = env.project("p");
    let source = codex_source(&env, project_id);

    let receipt = fork_now(&env, fork_request(project_id, source, "branch", None))
        .await
        .unwrap();
    let fork = fork_of(&receipt);
    assert_eq!(fork.name, "cx-fork");
    assert_eq!(fork.harness, switchboard_core::HarnessKind::Codex);
    assert_eq!(fork.model.as_deref(), Some("gpt-6-luna"));
    assert_eq!(
        fork.session_locator,
        Some(SessionLocator::Codex {
            thread_id: CODEX_FORK_CHILD.to_owned(),
            partition_date: codex_fork_date(),
        })
    );
    assert_eq!(
        fork.forked_from_session, None,
        "an eager fork has nothing to materialize"
    );
    assert_eq!(receipt.send.recipients[0].status, SendStatus::Accepted);
    assert_eq!(lock(&env.sync.forks_carried).len(), 1);
    let done = wait_done(&env, project_id, receipt.send.send_id).await;
    assert_eq!(done.recipients[0].text.as_deref(), Some("ack: branch"));
    assert_eq!(roster(&env, project_id).len(), 2);
}

#[tokio::test]
async fn a_repeat_that_joined_the_refused_attempt_keeps_its_receipt_when_another_repeat_resumes() {
    // Ledger only, no tasks: the joiner's slot is inspected unpolled.
    let (env, project_id, _source) = forkable_source();
    let fork = roster(&env, project_id).remove(0);
    let ledger = &env.state.ledger;
    let ForkClaim::Fresh { send_id } =
        ledger.claim_fork(project_id, Some("k"), "h", CancellationToken::new())
    else {
        panic!("fresh");
    };
    ledger.attach_fork(send_id, &fork);
    let ForkClaim::Joined { result: joiner, .. } =
        ledger.claim_fork(project_id, Some("k"), "h", CancellationToken::new())
    else {
        panic!("joined while in flight");
    };
    ledger.reject_undispatched(send_id, error_code::PROJECT_LOAD_TIMEOUT, "late");
    ledger.settle(send_id).unwrap();

    let ForkClaim::Resume {
        send_id: resumed,
        fork: again,
    } = ledger.claim_fork(project_id, Some("k"), "h", CancellationToken::new())
    else {
        panic!("resumed");
    };
    assert_ne!(resumed, send_id);
    assert_eq!(again.id, fork.id);
    assert_eq!(ledger.send_id_for_key(project_id, "k"), Some(resumed));
    let held = joiner.borrow().clone();
    assert!(
        matches!(&held, Some(Ok(receipt)) if receipt.send_id == send_id
            && receipt.recipients[0].status == SendStatus::Rejected),
        "the joiner keeps the refused attempt's receipt: {held:?}"
    );
}

/// A source whose reply is complete but whose stream stays open, as Codex's
/// does while it reads its rollout: the actor still holds its session.
async fn source_holding_its_session_after_a_reply(
    env: &TestEnv,
    project_id: ProjectId,
    source: AgentId,
) -> Arc<Notify> {
    let drain = Arc::new(Notify::new());
    env.adapter.push(Script::reply("done").draining(&drain));
    let reply = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, vec![source], "work"),
    )
    .await
    .unwrap();
    let done = wait_done(env, project_id, reply.send_id).await;
    assert_eq!(done.recipients[0].status, SendStatus::Completed);
    assert!(
        env.state.dispatcher.has_pending_work(source).await,
        "the reply is complete but the session is still held"
    );
    drain
}

#[tokio::test(start_paused = true)]
async fn a_fork_requested_while_the_source_still_holds_its_session_waits_for_the_release() {
    let (env, project_id, source) = forkable_source();
    let drain = source_holding_its_session_after_a_reply(&env, project_id, source).await;

    let task = spawn_fork(&env, fork_request(project_id, source, "branch", Some("k")));
    // Under the paused clock time moves only while every task is parked: the
    // fork is waiting for the release, not refused and not created.
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(!task.is_finished(), "waiting for the release");
    assert_eq!(roster(&env, project_id).len(), 1);

    drain.notify_one();
    let receipt = task.await.unwrap().unwrap();
    assert_eq!(receipt.send.recipients[0].status, SendStatus::Accepted);
    let done = wait_done(&env, project_id, receipt.send.send_id).await;
    assert_eq!(done.recipients[0].text.as_deref(), Some("ack: branch"));
    assert_eq!(roster(&env, project_id).len(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_source_holding_its_session_past_the_budget_refuses_the_fork_and_the_same_key_forks_after_the_release()
 {
    let (env, project_id, source) = forkable_source();
    let drain = source_holding_its_session_after_a_reply(&env, project_id, source).await;

    let (handle, home) = (env.handle(), env.home());
    let err = fork(
        handle,
        home,
        fork_request(project_id, source, "branch", Some("k")),
        WaitBudget::new(Some(2), false),
    )
    .await
    .unwrap_err();
    assert_eq!(error_code(&err), error_code::SOURCE_BUSY);
    assert_eq!(roster(&env, project_id).len(), 1, "nothing was created");

    // The key was released: the same request forks once the source lets go.
    drain.notify_one();
    let receipt = fork_now(&env, fork_request(project_id, source, "branch", Some("k")))
        .await
        .unwrap();
    assert!(receipt.fork.is_some());
    assert_eq!(receipt.send.recipients[0].status, SendStatus::Accepted);
    assert_eq!(roster(&env, project_id).len(), 2);
}

/// Whether an error is the server's interruption, however it was wrapped on
/// the way out of a task.
fn is_interrupted(error: &AppError) -> bool {
    match error {
        AppError::Interrupted => true,
        AppError::Shared(inner) => is_interrupted(inner),
        _ => false,
    }
}

#[tokio::test(start_paused = true)]
async fn a_stop_while_the_sources_agent_is_preparing_a_turn_ends_the_forks_task_at_once() {
    let (env, project_id, source) = forkable_source();
    let held = Arc::new(Notify::new());
    env.adapter
        .push(Script::reply("ack").holding_dispatch(&held));
    // A send whose dispatch the adapter holds: the actor is preparing the
    // turn and answers no question.
    let send = tokio::spawn(send_now(
        env.handle(),
        env.home(),
        text_send(project_id, vec![source], "hi"),
    ));
    for _ in 0..500 {
        if !lock(&env.adapter.prompts).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        !lock(&env.adapter.prompts).is_empty(),
        "the actor reached the adapter"
    );

    let stop = CancellationToken::new();
    let (handle, home) = (env.handle(), env.home());
    let budget = WaitBudget::default_wait().with_cancel(stop.clone());
    let task = tokio::spawn(async move {
        fork(
            handle,
            home,
            fork_request(project_id, source, "branch", Some("k")),
            budget,
        )
        .await
    });
    // Under the paused clock a sleep returns only once every task is parked:
    // the fork's task is waiting on the actor's answer.
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(!task.is_finished(), "parked on the actor's answer");
    assert!(env.state.ledger.send_id_for_key(project_id, "k").is_some());

    stop.cancel();
    // The task's own interruption, not the call's fallback: its question to
    // the actor was cut short, and it released the key, while the actor is
    // still held.
    let err = task.await.unwrap().unwrap_err();
    assert!(is_interrupted(&err), "{err:?}");
    for _ in 0..500 {
        if env.state.ledger.send_id_for_key(project_id, "k").is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        env.state.ledger.send_id_for_key(project_id, "k").is_none(),
        "the task released the key"
    );
    assert_eq!(roster(&env, project_id).len(), 1, "nothing was created");

    held.notify_one();
    let receipt = send.await.unwrap().unwrap();
    wait_done(&env, project_id, receipt.send_id).await;
}

/// Wait for the send's dispatch phase to settle, by the ledger.
async fn settled(env: &TestEnv, send_id: Uuid) -> super::ledger::SendSnapshot {
    for _ in 0..500 {
        if env
            .state
            .ledger
            .send(send_id)
            .is_some_and(|snapshot| snapshot.settled)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let snapshot = env.state.ledger.send(send_id).unwrap();
    assert!(snapshot.settled, "the task settled its entry");
    snapshot
}

#[tokio::test(start_paused = true)]
async fn a_creation_that_outlasts_the_calls_budget_answers_request_in_progress_and_the_same_key_finds_the_fork()
 {
    let (env, project_id, source) = forkable_source();
    let seam = env.seam(TestSeam::ForkRegistration);
    let (handle, home) = (env.handle(), env.home());
    let task = tokio::spawn(async move {
        fork(
            handle,
            home,
            fork_request(project_id, source, "branch", Some("k")),
            WaitBudget::new(Some(2), false),
        )
        .await
    });
    seam.entered.notified().await;
    // Creation is still held when the budget runs out: the caller gets the
    // send id, the task keeps going.
    let err = task.await.unwrap().unwrap_err();
    let AppError::RequestInProgress { send_id } = err else {
        panic!("request in progress, got {err:?}");
    };
    assert_eq!(
        env.state.ledger.send_id_for_key(project_id, "k"),
        Some(send_id)
    );
    assert_eq!(roster(&env, project_id).len(), 1, "creation is still held");

    // The task's later phases are past the deadline too: the fork is
    // recorded and its message refused, which a wait on the id reports.
    seam.release.notify_one();
    let snapshot = settled(&env, send_id).await;
    assert_eq!(snapshot.recipients[0].status, SendStatus::Rejected);
    assert!(wait_done(&env, project_id, send_id).await.done);
    assert!(lock(&env.adapter.prompts).is_empty(), "nothing was sent");
    assert_eq!(roster(&env, project_id).len(), 2);

    // The same key resumes: the message goes to that fork, not a second.
    env.clear_seam(TestSeam::ForkRegistration);
    let resent = fork_now(&env, fork_request(project_id, source, "branch", Some("k")))
        .await
        .unwrap();
    assert_ne!(resent.send.send_id, send_id);
    assert_eq!(resent.send.recipients[0].status, SendStatus::Accepted);
    assert_eq!(
        fork_of(&resent).id,
        snapshot.recipients[0].agent_id,
        "the same fork"
    );
    wait_done(&env, project_id, resent.send.send_id).await;
    assert_eq!(roster(&env, project_id).len(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_stop_during_the_forks_creation_answers_request_in_progress_and_the_fork_is_recorded_with_nothing_sent()
 {
    let (env, project_id, source) = forkable_source();
    let seam = env.seam(TestSeam::ForkRegistration);
    let stop = CancellationToken::new();
    let (handle, home) = (env.handle(), env.home());
    let budget = WaitBudget::default_wait().with_cancel(stop.clone());
    let task = tokio::spawn(async move {
        fork(
            handle,
            home,
            fork_request(project_id, source, "branch", Some("k")),
            budget,
        )
        .await
    });
    seam.entered.notified().await;
    stop.cancel();
    // The allowance passes with creation still held: the send id, not a
    // bare interruption, since the fork is about to exist.
    let err = task.await.unwrap().unwrap_err();
    let AppError::RequestInProgress { send_id } = err else {
        panic!("request in progress, got {err:?}");
    };
    assert_eq!(
        env.state.ledger.send_id_for_key(project_id, "k"),
        Some(send_id)
    );
    assert_eq!(roster(&env, project_id).len(), 1, "creation is still held");

    // The task runs on: the fork is registered, its message refused, not sent.
    seam.release.notify_one();
    for _ in 0..500 {
        if env
            .state
            .ledger
            .send(send_id)
            .is_some_and(|snapshot| snapshot.settled)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let snapshot = env.state.ledger.send(send_id).unwrap();
    assert!(snapshot.settled, "the task settled its entry");
    assert_eq!(snapshot.recipients[0].status, SendStatus::Rejected);
    assert!(lock(&env.adapter.prompts).is_empty(), "nothing was sent");
    assert_eq!(roster(&env, project_id).len(), 2);

    env.clear_seam(TestSeam::ForkRegistration);
    let resent = fork_now(&env, fork_request(project_id, source, "branch", Some("k")))
        .await
        .unwrap();
    assert_eq!(resent.send.recipients[0].status, SendStatus::Accepted);
    assert_eq!(roster(&env, project_id).len(), 2, "the same fork, resumed");
}

/// A removal over agent access, held at its agent's drain: the agent's turn
/// has finished but its stream stays open until `drain` fires, as Codex's does
/// while it reads its rollout, so the removal waits there. Returns once the
/// removal has closed the agent, which a send can no longer reach: that is
/// the signal it is under way, not a guess at scheduling.
async fn removal_held_at_drain(
    env: &TestEnv,
    project_id: ProjectId,
    agent: AgentId,
    drain: &Arc<Notify>,
) -> tokio::task::JoinHandle<Result<super::change::RemovedAgent, AppError>> {
    env.adapter.push(Script::reply("done").draining(drain));
    let receipt = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, vec![agent], "go"),
    )
    .await
    .unwrap();
    assert_eq!(
        wait_done(env, project_id, receipt.send_id).await.recipients[0].status,
        SendStatus::Completed
    );
    assert!(
        env.state.dispatcher.accepts_now(agent).await,
        "idle in its drain"
    );
    let removal = tokio::spawn(super::change::remove_agent(env.handle(), project_id, agent));
    let started = tokio::time::Instant::now();
    while env.state.dispatcher.accepts_now(agent).await {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the removal never closed the agent"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    removal
}

fn listed(env: &TestEnv, project_id: ProjectId, agent: AgentId) -> bool {
    list_project_agents_readonly_impl(&env.state, project_id)
        .unwrap()
        .iter()
        .any(|a| a.id == agent)
}

#[tokio::test]
async fn a_removal_finishes_when_its_caller_is_cancelled() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let drain = Arc::new(Notify::new());
    let caller = removal_held_at_drain(&env, project_id, agents[0], &drain).await;
    // The tool call is cancelled mid-removal: its future is dropped.
    caller.abort();
    drain.notify_one();

    let started = tokio::time::Instant::now();
    while listed(&env, project_id, agents[0]) {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the removal stopped with its caller"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        !env.state.dispatcher.accepts_now(agents[0]).await,
        "nothing can start the removed agent"
    );
    let refused = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, vec![agents[0]], "again"),
    )
    .await;
    assert!(refused.is_err(), "a send to it is refused: {refused:?}");
    assert!(
        lock(&env.state.agent_removals).in_progress.is_empty(),
        "the project is free for workflows again"
    );
}

#[tokio::test]
async fn no_workflow_starts_while_an_agent_is_being_removed() {
    let (env, project_id, agents) = workflow_env().await;
    let drain = Arc::new(Notify::new());
    let removal = removal_held_at_drain(&env, project_id, agents[2], &drain).await;

    let refused = run_workflow(
        &env.state,
        &env.home(),
        pair_request(project_id, &agents),
        &WaitBudget::default_wait(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(refused, AppError::WorkflowBlockedByAgentRemoval { .. }),
        "{refused:?}"
    );

    drain.notify_one();
    let removed = removal.await.unwrap().unwrap();
    assert_eq!(removed.agent_id, agents[2]);
    assert!(lock(&env.state.agent_removals).in_progress.is_empty());
}

#[tokio::test]
async fn no_agent_is_removed_while_a_workflow_runs_and_the_run_is_untouched() {
    let (env, project_id, agents) = workflow_env().await;
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("greeting", &gate));
    let run_id = run_workflow(
        &env.state,
        &env.home(),
        pair_request(project_id, &agents),
        &WaitBudget::default_wait(),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, agents[0]).await;

    let refused = super::change::remove_agent(env.handle(), project_id, agents[2])
        .await
        .unwrap_err();
    assert!(
        matches!(refused, AppError::RemoveAgentWorkflowRunning { .. }),
        "{refused:?}"
    );
    assert!(listed(&env, project_id, agents[2]));
    assert!(
        lock(&env.state.agent_removals).in_progress.is_empty(),
        "a refused removal holds nothing"
    );
    gate.notify_one();
    let reply = wait_workflow(
        &env.state,
        project_id,
        run_id,
        &WaitBudget::new(Some(30), false),
    )
    .await
    .unwrap();
    assert_eq!(reply.status, WorkflowRunStatus::Completed);
}

#[cfg(unix)]
#[tokio::test]
async fn a_failed_record_deletion_leaves_the_agent_usable() {
    use std::os::unix::fs::PermissionsExt;

    let (env, project_id, agents) = project_with_agents(&["a"]);
    crate::commands::open_project_impl(&env.state, project_id).unwrap();
    let registry_dir = lock(&env.state.projects)
        .get(&project_id)
        .unwrap()
        .registry_path
        .parent()
        .unwrap()
        .to_owned();
    let writable = std::fs::metadata(&registry_dir).unwrap().permissions();
    std::fs::set_permissions(&registry_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let failed = super::change::remove_agent(env.handle(), project_id, agents[0]).await;
    std::fs::set_permissions(&registry_dir, writable).unwrap();

    assert!(
        failed.is_err(),
        "the registry could not be written: {failed:?}"
    );
    assert!(
        listed(&env, project_id, agents[0]),
        "the agent is still there"
    );
    let receipt = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, vec![agents[0]], "still here?"),
    )
    .await
    .unwrap();
    assert_eq!(
        wait_done(&env, project_id, receipt.send_id)
            .await
            .recipients[0]
            .status,
        SendStatus::Completed,
        "and it still works"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn an_unreadable_registry_leaves_the_agent_blocked_and_nothing_else_deleted() {
    use std::os::unix::fs::PermissionsExt;

    let (env, project_id, agents) = project_with_agents(&["a"]);
    crate::commands::open_project_impl(&env.state, project_id).unwrap();
    let registry = lock(&env.state.projects)
        .get(&project_id)
        .unwrap()
        .registry_path
        .clone();
    let readable = std::fs::metadata(&registry).unwrap().permissions();
    std::fs::set_permissions(&registry, std::fs::Permissions::from_mode(0o000)).unwrap();
    // The window's removal: it finds the agent in memory, so the registry is
    // first read by the deletion itself. (Agent access reads the roster from
    // the file first, and stops there with the read error.)
    let failed = crate::commands::remove_agent_impl(&env.state, agents[0]).await;
    std::fs::set_permissions(&registry, readable).unwrap();

    assert!(
        matches!(failed, Err(AppError::AgentRemovalUnconfirmed { .. })),
        "whether the record went could not be told: {failed:?}"
    );
    assert!(
        !env.state.dispatcher.accepts_now(agents[0]).await,
        "the agent is blocked rather than reopened on a guess"
    );
    assert!(
        listed(&env, project_id, agents[0]),
        "its record was never touched"
    );
    assert!(
        lock(&env.state.agents_by_id).contains_key(&agents[0]),
        "nothing else was deleted"
    );
    assert_eq!(
        lock(&env.state.agent_removals).deletions(project_id),
        0,
        "an unconfirmed deletion does not count as one"
    );
}

#[tokio::test]
async fn cancelling_one_recipient_leaves_the_others_running() {
    let (env, project_id, agents) = project_with_agents(&["a", "b"]);
    // One gate for both: which agent's turn takes which script depends on
    // which actor dispatches first.
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("reply", &gate));
    env.adapter.push(Script::parked("reply", &gate));
    let receipt = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "go"),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, agents[0]).await;
    await_turn_running(&env.state, agents[1]).await;

    let cancelled = cancel_send(&env.state, project_id, receipt.send_id, Some(agents[0])).unwrap();
    assert_eq!(cancelled.outcome, super::send::CancelOutcome::Cancelled);
    assert_eq!(cancelled.recipients, vec![agents[0]]);
    // Release the gate only once the cancelled reply has ended, so it cannot
    // take the release and finish instead.
    let started = tokio::time::Instant::now();
    loop {
        let snapshot = env.state.ledger.send(receipt.send_id).unwrap();
        if snapshot.recipients[0].status == SendStatus::Cancelled {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the reply never stopped"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    gate.notify_one();
    let done = wait_done(&env, project_id, receipt.send_id).await;
    let status_of = |agent: AgentId| {
        done.recipients
            .iter()
            .find(|r| r.agent_id == agent)
            .unwrap()
            .status
    };
    assert_eq!(status_of(agents[0]), SendStatus::Cancelled);
    assert_eq!(
        status_of(agents[1]),
        SendStatus::Completed,
        "the other recipient carried on"
    );

    let again = cancel_send(&env.state, project_id, receipt.send_id, Some(agents[0])).unwrap();
    assert_eq!(
        again.outcome,
        super::send::CancelOutcome::AlreadyFinished,
        "nothing left to cancel"
    );
    assert!(again.recipients.is_empty());
}

#[tokio::test]
async fn cancelling_one_agents_reply_to_a_workflow_send_ends_the_run() {
    let (env, project_id, agents) = workflow_env().await;
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("greeting", &gate));
    let run_id = run_workflow(
        &env.state,
        &env.home(),
        pair_request(project_id, &agents),
        &WaitBudget::default_wait(),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, agents[0]).await;
    // The run's first send, as an orchestrator reads it from wait_workflow.
    let mut send_id = None;
    for _ in 0..100 {
        let reply = wait_workflow(
            &env.state,
            project_id,
            run_id,
            &WaitBudget::new(Some(0), false),
        )
        .await
        .unwrap();
        if let Some(first) = reply.sends.first() {
            send_id = Some(first.send_id);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let send_id = send_id.expect("the run reported its first send");

    let cancelled = cancel_send(&env.state, project_id, send_id, Some(agents[0])).unwrap();
    assert_eq!(
        cancelled.outcome,
        super::send::CancelOutcome::Cancelled,
        "the ledger holds the workflow's send, so the cancel is checked against it"
    );
    assert_eq!(cancelled.recipients, vec![agents[0]]);
    let reply = wait_workflow(
        &env.state,
        project_id,
        run_id,
        &WaitBudget::new(Some(30), false),
    )
    .await
    .unwrap();
    assert_eq!(
        reply.status,
        WorkflowRunStatus::Cancelled,
        "the agent stopped that reply, and a cancelled reply ends the run"
    );
}

/// While a run waits on its agents, each agent of the current step carries
/// what `wait_send` gives an unfinished recipient. The step doing the waiting
/// is a wait step, which made no send of its own, so each agent reports the
/// run's newest send that reached it: one reviewer finished (completed, no
/// activity), the other still writing (running, with its activity and text so
/// far). A finished run carries none of it.
#[tokio::test]
async fn wait_workflow_shows_what_each_agent_of_the_current_step_is_doing() {
    let (env, project_id, agents) = workflow_env().await;
    let gate = Arc::new(Notify::new());
    env.adapter.push_for(agents[0], Script::reply("greeting"));
    env.adapter.push_for(agents[1], Script::reply("looks fine"));
    env.adapter
        .push_for(agents[2], Script::parked("still reading", &gate));
    let run_id = run_workflow(
        &env.state,
        &env.home(),
        pair_request(project_id, &agents),
        &WaitBudget::default_wait(),
    )
    .await
    .unwrap();
    // Content wakes no waiter, so look until the slow reviewer's text and the
    // quick one's completion have both landed.
    let mut waiting = None;
    for _ in 0..500 {
        let reply = wait_workflow(
            &env.state,
            project_id,
            run_id,
            &WaitBudget::new(Some(0), false),
        )
        .await
        .unwrap();
        let landed = |agent: AgentId, ready: &dyn Fn(&super::workflow::StepAgent) -> bool| {
            reply
                .recipients
                .iter()
                .any(|a| a.agent.agent_id == agent && ready(a))
        };
        if reply.label.as_deref() == Some("Reviewed")
            && landed(agents[1], &|a| a.status == Some(SendStatus::Completed))
            && landed(agents[2], &|a| a.recent_text.is_some())
        {
            waiting = Some(reply);
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let waiting = waiting.expect("the run reached its wait step with one reviewer done");
    let review = waiting.sends.iter().find(|s| s.step == 2).unwrap().send_id;
    let of = |agent: AgentId| {
        waiting
            .recipients
            .iter()
            .find(|a| a.agent.agent_id == agent)
            .unwrap()
    };

    let slow = of(agents[2]);
    assert_eq!(
        slow.send_id,
        Some(review),
        "the wait step's agent reports the newest send that reached it, the send step's"
    );
    assert_eq!(slow.status, Some(SendStatus::Running));
    assert!(slow.activity.is_some(), "a running turn's activity");
    assert_eq!(slow.recent_text.as_deref(), Some("still reading"));
    let shape = serde_json::to_value(slow).unwrap();
    assert_eq!(shape["agent_id"], agents[2].to_string(), "{shape}");
    assert_eq!(shape["name"], "r2", "{shape}");
    assert_eq!(shape["status"], "running", "{shape}");

    let quick = of(agents[1]);
    assert_eq!(quick.send_id, Some(review));
    assert_eq!(quick.status, Some(SendStatus::Completed));
    assert_eq!(quick.activity, None, "a finished reply has no activity");
    assert_eq!(quick.recent_text, None);

    gate.notify_waiters();
    let ended = wait_workflow(
        &env.state,
        project_id,
        run_id,
        &WaitBudget::new(Some(30), false),
    )
    .await
    .unwrap();
    assert_eq!(ended.status, WorkflowRunStatus::Completed);
    assert!(
        ended.recipients.iter().all(|a| a.send_id.is_none()
            && a.status.is_none()
            && a.activity.is_none()
            && a.recent_text.is_none()),
        "a finished run carries no step progress: {:?}",
        ended.recipients
    );
}

/// The run's first send, as an orchestrator reads it from `wait_workflow`
/// once the step has dispatched.
async fn first_workflow_send(env: &TestEnv, project_id: ProjectId, run_id: Uuid) -> Uuid {
    for _ in 0..500 {
        let reply = wait_workflow(
            &env.state,
            project_id,
            run_id,
            &WaitBudget::new(Some(0), false),
        )
        .await
        .unwrap();
        if let Some(first) = reply.sends.first() {
            return first.send_id;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the run never reported its first send");
}

/// The pair workflow started through the tools, its first step's reply
/// (`greeting`, to the coder) held running until the returned gate fires.
async fn workflow_with_a_running_first_send(
    env: &TestEnv,
    project_id: ProjectId,
    agents: &[AgentId],
) -> (Uuid, Uuid, Arc<Notify>) {
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("greeting", &gate));
    let run_id = run_workflow(
        &env.state,
        &env.home(),
        pair_request(project_id, agents),
        &WaitBudget::default_wait(),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, agents[0]).await;
    let send_id = first_workflow_send(env, project_id, run_id).await;
    (run_id, send_id, gate)
}

#[tokio::test]
async fn wait_send_on_a_running_workflow_send_reports_it_running_not_done() {
    let (env, project_id, agents) = workflow_env().await;
    let (_run_id, send_id, gate) =
        workflow_with_a_running_first_send(&env, project_id, &agents).await;

    let reply = wait_send(
        &env.state,
        project_id,
        send_id,
        &WaitBudget::new(Some(0), false),
        &env.home(),
    )
    .await
    .unwrap();
    assert!(
        !reply.done,
        "the workflow's first step is still running on its agent, so the send is not done"
    );
    assert_eq!(
        reply.source,
        WaitSource::Live,
        "no restart happened: the send is followed live, not read back from the journal"
    );
    assert_eq!(reply.recipients.len(), 1);
    assert_eq!(reply.recipients[0].agent_id, agents[0]);
    assert_eq!(
        reply.recipients[0].status,
        SendStatus::Running,
        "the agent's turn for this send is running, and no restart happened"
    );
    gate.notify_one();
}

#[tokio::test]
async fn wait_send_follows_a_workflow_send_to_its_reply() {
    let (env, project_id, agents) = workflow_env().await;
    let (run_id, send_id, gate) =
        workflow_with_a_running_first_send(&env, project_id, &agents).await;
    gate.notify_one();

    let reply = wait_done(&env, project_id, send_id).await;
    assert!(reply.done);
    assert_eq!(reply.source, WaitSource::Live);
    assert_eq!(reply.recipients[0].status, SendStatus::Completed);
    assert_eq!(reply.recipients[0].text.as_deref(), Some("greeting"));
    let ended = wait_workflow(
        &env.state,
        project_id,
        run_id,
        &WaitBudget::new(Some(30), false),
    )
    .await
    .unwrap();
    assert_eq!(ended.status, WorkflowRunStatus::Completed);
}

#[tokio::test]
async fn wait_any_send_follows_a_workflow_send_to_its_reply() {
    let (env, project_id, agents) = workflow_env().await;
    let (_run_id, send_id, gate) =
        workflow_with_a_running_first_send(&env, project_id, &agents).await;

    let running = super::send::wait_any_send(
        &env.state,
        project_id,
        vec![send_id],
        &WaitBudget::new(Some(0), false),
        &env.home(),
    )
    .await
    .unwrap();
    assert!(!running.any_done, "the step's reply is still running");
    assert_eq!(running.sends[0].source, WaitSource::Live);
    assert_eq!(running.sends[0].recipients[0].status, SendStatus::Running);

    gate.notify_one();
    let done = super::send::wait_any_send(
        &env.state,
        project_id,
        vec![send_id],
        &WaitBudget::new(Some(30), false),
        &env.home(),
    )
    .await
    .unwrap();
    assert!(done.any_done);
    assert_eq!(done.sends[0].source, WaitSource::Live);
    assert_eq!(done.sends[0].recipients[0].status, SendStatus::Completed);
    assert_eq!(
        done.sends[0].recipients[0].text.as_deref(),
        Some("greeting")
    );
}

#[tokio::test]
async fn read_reply_on_a_running_workflow_send_reports_it_running() {
    let (env, project_id, agents) = workflow_env().await;
    let (_run_id, send_id, gate) =
        workflow_with_a_running_first_send(&env, project_id, &agents).await;

    let page = read_reply(
        &env.state,
        project_id,
        send_id,
        agents[0],
        0,
        ReadFrom::Auto,
        OUTPUT_PAGE_CHARS,
        &env.home(),
    )
    .await
    .unwrap();
    assert_eq!(
        page.status,
        SendStatus::Running,
        "a running reply, not one a restart cut off"
    );
    assert_eq!(page.source, OutputSource::Live);
    gate.notify_one();
}

#[tokio::test]
async fn cancelling_a_whole_workflow_send_ends_the_run() {
    let (env, project_id, agents) = workflow_env().await;
    let (run_id, send_id, _gate) =
        workflow_with_a_running_first_send(&env, project_id, &agents).await;

    let cancelled = cancel_send(&env.state, project_id, send_id, None).unwrap();
    assert_eq!(cancelled.outcome, super::send::CancelOutcome::Cancelled);
    assert_eq!(cancelled.recipients, vec![agents[0]]);
    let reply = wait_done(&env, project_id, send_id).await;
    assert_eq!(reply.recipients[0].status, SendStatus::Cancelled);
    let ended = wait_workflow(
        &env.state,
        project_id,
        run_id,
        &WaitBudget::new(Some(30), false),
    )
    .await
    .unwrap();
    assert_eq!(
        ended.status,
        WorkflowRunStatus::Cancelled,
        "a cancelled reply ends the run, whichever form of cancel stopped it"
    );
}

/// The app's workflow provider for `project_id`, as a run started through the
/// tools builds it, for driving a send's dispatch hooks one at a time.
fn workflow_provider(env: &TestEnv, project_id: ProjectId) -> ProjectDispatchFactoryProvider {
    ProjectDispatchFactoryProvider::new(
        &env.state,
        &super::read::project_readonly(&env.state, project_id).unwrap(),
        &[],
        &env.home(),
        0,
    )
    .with_agent_access(true)
}

/// Poll until the adapter has been handed a prompt containing `needle`.
async fn await_prompt(env: &TestEnv, needle: &str) {
    for _ in 0..1000 {
        if lock(&env.adapter.prompts)
            .iter()
            .any(|p| p.contains(needle))
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("no prompt containing {needle:?} was dispatched");
}

/// The run's sends once it has made `count` of them.
async fn workflow_sends(
    env: &TestEnv,
    project_id: ProjectId,
    run_id: Uuid,
    count: usize,
) -> Vec<Uuid> {
    for _ in 0..1000 {
        let reply = wait_workflow(
            &env.state,
            project_id,
            run_id,
            &WaitBudget::new(Some(0), false),
        )
        .await
        .unwrap();
        if reply.sends.len() >= count {
            return reply.sends.iter().map(|send| send.send_id).collect();
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the run never made {count} sends");
}

/// Cancel the run as an orchestrator would: the whole run, or the whole of
/// its send `send_id`.
fn cancel_the_run(
    env: &TestEnv,
    project_id: ProjectId,
    run_id: Uuid,
    send_id: Uuid,
    whole_send: bool,
) {
    if whole_send {
        cancel_send(&env.state, project_id, send_id, None).unwrap();
    } else {
        super::workflow::cancel_workflow_run(&env.state, project_id, run_id).unwrap();
    }
}

/// The agent's answer once it is idle again.
async fn answer_once_idle(
    env: &TestEnv,
    project_id: ProjectId,
    agent_id: AgentId,
) -> super::read::LatestReply {
    let reply = wait_agent(
        &env.state,
        project_id,
        agent_id,
        &WaitBudget::new(Some(30), false),
        &env.home(),
    )
    .await
    .unwrap();
    assert_eq!(reply.status, AgentWaitStatus::Idle);
    reply.output.expect("an idle agent's answer is read")
}

#[tokio::test]
async fn a_cancelled_run_leaves_a_message_the_user_queued_behind_its_reply() {
    for whole_send in [false, true] {
        let (env, project_id, agents) = workflow_env().await;
        let (first, second) = (agents[1], agents[2]);
        let (first_gate, second_gate, user_gate) = (
            Arc::new(Notify::new()),
            Arc::new(Notify::new()),
            Arc::new(Notify::new()),
        );
        env.adapter.push(Script::reply("greeting"));
        env.adapter
            .push_for(first, Script::parked("first review", &first_gate));
        env.adapter
            .push_for(second, Script::parked("second review", &second_gate));
        let run_id = run_workflow(
            &env.state,
            &env.home(),
            pair_request(project_id, &agents),
            &WaitBudget::default_wait(),
        )
        .await
        .unwrap();
        await_turn_running(&env.state, first).await;
        await_turn_running(&env.state, second).await;
        // The user queues a message behind the second reviewer's reply, which
        // then finishes, so the second reviewer starts the user's message
        // while the run still waits on the first.
        env.adapter
            .push_for(second, Script::parked("the user's reply", &user_gate));
        send_message_impl(
            &env.state,
            second,
            "mine",
            Vec::new(),
            Uuid::now_v7(),
            &env.home(),
        )
        .await
        .unwrap();
        second_gate.notify_one();
        await_prompt(&env, "mine").await;
        await_turn_running(&env.state, second).await;
        let review = workflow_sends(&env, project_id, run_id, 2).await[1];

        cancel_the_run(&env, project_id, run_id, review, whole_send);
        let ended = wait_workflow(
            &env.state,
            project_id,
            run_id,
            &WaitBudget::new(Some(30), false),
        )
        .await
        .unwrap();
        assert_eq!(
            ended.status,
            WorkflowRunStatus::Cancelled,
            "whole send: {whole_send}"
        );

        user_gate.notify_one();
        let answer = answer_once_idle(&env, project_id, second).await;
        assert_eq!(
            answer.status,
            LatestReplyStatus::Completed,
            "the run's cleanup left the user's message alone (whole send: {whole_send})"
        );
        assert_eq!(answer.text.as_deref(), Some("the user's reply"));
    }
}

#[tokio::test]
async fn a_cancelled_run_leaves_a_reply_that_finished_and_is_draining_alone() {
    for whole_send in [false, true] {
        let (env, project_id, agents) = workflow_env().await;
        let (first, second) = (agents[1], agents[2]);
        let (first_gate, drain_gate, user_gate) = (
            Arc::new(Notify::new()),
            Arc::new(Notify::new()),
            Arc::new(Notify::new()),
        );
        env.adapter.push(Script::reply("greeting"));
        env.adapter
            .push_for(first, Script::parked("first review", &first_gate));
        // The second reviewer's reply ends at once, then its stream stays open:
        // finished, but the agent is not idle yet.
        env.adapter
            .push_for(second, Script::reply("second review").draining(&drain_gate));
        let run_id = run_workflow(
            &env.state,
            &env.home(),
            pair_request(project_id, &agents),
            &WaitBudget::default_wait(),
        )
        .await
        .unwrap();
        await_turn_running(&env.state, first).await;
        let review = workflow_sends(&env, project_id, run_id, 2).await[1];
        await_recipient_status(&env, review, second, SendStatus::Completed).await;
        env.adapter
            .push_for(second, Script::parked("the user's reply", &user_gate));
        send_message_impl(
            &env.state,
            second,
            "mine",
            Vec::new(),
            Uuid::now_v7(),
            &env.home(),
        )
        .await
        .unwrap();

        cancel_the_run(&env, project_id, run_id, review, whole_send);
        let ended = wait_workflow(
            &env.state,
            project_id,
            run_id,
            &WaitBudget::new(Some(30), false),
        )
        .await
        .unwrap();
        assert_eq!(
            ended.status,
            WorkflowRunStatus::Cancelled,
            "whole send: {whole_send}"
        );
        drain_gate.notify_one();
        await_prompt(&env, "mine").await;
        user_gate.notify_one();
        let answer = answer_once_idle(&env, project_id, second).await;
        assert_eq!(
            answer.text.as_deref(),
            Some("the user's reply"),
            "whole send: {whole_send}"
        );

        let statuses: Vec<SendStatus> = env
            .state
            .ledger
            .send(review)
            .unwrap()
            .recipients
            .iter()
            .map(|r| r.status)
            .collect();
        assert_eq!(
            statuses,
            vec![SendStatus::Cancelled, SendStatus::Completed],
            "the finished reply stays completed, and nothing of the run is left running (whole send: {whole_send})"
        );
        assert!(
            env.state
                .dispatcher
                .running_turn_kind(first)
                .await
                .is_none()
        );
    }
}

#[tokio::test]
async fn a_cancelled_run_never_starts_its_message_still_waiting_in_line() {
    let (env, project_id, agents) = workflow_env().await;
    let coder = agents[0];
    crate::commands::open_project_impl(&env.state, project_id).unwrap();
    // The coder is draining a reply of the user's: finished, not yet idle, so
    // the workflow's first message is accepted and waits in line behind it.
    let drain_gate = Arc::new(Notify::new());
    env.adapter
        .push(Script::reply("the user's reply").draining(&drain_gate));
    send_message_impl(
        &env.state,
        coder,
        "mine",
        Vec::new(),
        Uuid::now_v7(),
        &env.home(),
    )
    .await
    .unwrap();
    await_prompt(&env, "mine").await;
    for _ in 0..1000 {
        if env.state.dispatcher.accepts_now(coder).await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let run_id = run_workflow(
        &env.state,
        &env.home(),
        pair_request(project_id, &agents),
        &WaitBudget::default_wait(),
    )
    .await
    .unwrap();
    let greet = first_workflow_send(&env, project_id, run_id).await;

    super::workflow::cancel_workflow_run(&env.state, project_id, run_id).unwrap();
    let ended = wait_workflow(
        &env.state,
        project_id,
        run_id,
        &WaitBudget::new(Some(30), false),
    )
    .await
    .unwrap();
    assert_eq!(ended.status, WorkflowRunStatus::Cancelled);
    drain_gate.notify_one();
    answer_once_idle(&env, project_id, coder).await;
    assert!(
        !lock(&env.adapter.prompts)
            .iter()
            .any(|p| p.contains("Hi bob")),
        "the run's message was dropped from the line, never sent"
    );
    await_recipient_status(&env, greet, coder, SendStatus::Cancelled).await;
}

#[tokio::test]
async fn a_workflow_reply_cancelled_before_it_is_offered_is_never_sent() {
    let (env, project_id, agents) = workflow_env().await;
    // The workflow's send, registered and not yet offered: the step is
    // dispatching, and the cancel lands before the second recipient's turn.
    let send_id = Uuid::now_v7();
    let provider = workflow_provider(&env, project_id);
    provider.send_starting(send_id, &[agents[0], agents[1]]);

    let cancelled = cancel_send(&env.state, project_id, send_id, Some(agents[1])).unwrap();
    assert_eq!(cancelled.outcome, super::send::CancelOutcome::Cancelled);
    assert!(
        provider.admit(send_id, agents[0]).is_none(),
        "a cancelled reply ends the run, so nobody is offered the message"
    );
    // The engine stops the step as cancelled and says so.
    provider.send_finished(send_id, DispatchEnd::Cancelled);
    let held = env.state.ledger.send(send_id).unwrap();
    assert_eq!(
        held.recipients.iter().map(|r| r.status).collect::<Vec<_>>(),
        vec![SendStatus::Cancelled, SendStatus::Cancelled]
    );
}

#[tokio::test]
async fn cancelling_one_workflow_reply_stops_the_step_offering_the_rest() {
    let (env, project_id, agents) = workflow_env().await;
    let send_id = Uuid::now_v7();
    let provider = workflow_provider(&env, project_id);
    provider.send_starting(send_id, &agents);
    assert!(provider.admit(send_id, agents[0]).is_some());
    provider.offered(send_id, agents[0], true);
    assert!(
        !provider.send_cancelled(send_id),
        "nothing was cancelled yet"
    );

    // The first reply is cancelled while the step is still dispatching.
    cancel_send(&env.state, project_id, send_id, Some(agents[0])).unwrap();
    assert!(
        provider.send_cancelled(send_id),
        "a cancel of one recipient's reply reads as the send cancelled"
    );
    assert!(
        provider.admit(send_id, agents[1]).is_none(),
        "a cancel of another recipient's reply stops the fan-out too"
    );
    provider.send_finished(send_id, DispatchEnd::Cancelled);
    let held = env.state.ledger.send(send_id).unwrap();
    assert_eq!(held.recipients[1].status, SendStatus::Cancelled);
    assert_eq!(held.recipients[2].status, SendStatus::Cancelled);
}

#[tokio::test]
async fn a_cancel_wins_over_a_busy_answer_on_the_workflow_provider() {
    let (env, project_id, agents) = workflow_env().await;
    let send_id = Uuid::now_v7();
    let provider = workflow_provider(&env, project_id);
    provider.send_starting(send_id, &[agents[0], agents[1]]);
    assert!(provider.admit(send_id, agents[0]).is_some());
    provider.offered(send_id, agents[0], true);
    assert!(provider.admit(send_id, agents[1]).is_some());

    // The whole send is cancelled while the dispatcher answers for the
    // second recipient, which turns out busy.
    cancel_send(&env.state, project_id, send_id, None).unwrap();
    provider.offered(send_id, agents[1], false);
    assert!(
        provider.send_cancelled(send_id),
        "the engine reads the cancel, so the run ends cancelled, not failed"
    );
    provider.send_finished(send_id, DispatchEnd::Cancelled);
    let held = env.state.ledger.send(send_id).unwrap();
    assert_eq!(held.recipients[1].status, SendStatus::Rejected);
    assert_eq!(held.recipients[1].code, Some(error_code::AGENT_BUSY));
}

#[tokio::test]
async fn a_cancel_decided_when_the_step_stops_marks_the_rest_cancelled_not_refused() {
    let (env, project_id, agents) = workflow_env().await;
    let send_id = Uuid::now_v7();
    let provider = workflow_provider(&env, project_id);
    provider.send_starting(send_id, &agents);
    assert!(provider.admit(send_id, agents[0]).is_some());
    provider.offered(send_id, agents[0], true);

    // The send is cancelled while the second recipient's pre-dispatch check
    // runs; the check then refuses. The engine asks once, finds the cancel,
    // and tells the provider the step was cancelled, not that it was not
    // ready.
    cancel_send(&env.state, project_id, send_id, None).unwrap();
    assert!(provider.send_cancelled(send_id));
    provider.send_finished(send_id, DispatchEnd::Cancelled);

    let held = env.state.ledger.send(send_id).unwrap();
    for recipient in &held.recipients[1..] {
        assert_eq!(recipient.status, SendStatus::Cancelled);
        assert_eq!(
            recipient.code, None,
            "never refused with workflow_step_failed"
        );
    }
}

#[tokio::test]
async fn a_busy_refusal_leaves_later_workflow_recipients_not_dispatched() {
    let (env, project_id, agents) = workflow_env().await;
    let send_id = Uuid::now_v7();
    let provider = workflow_provider(&env, project_id);
    provider.send_starting(send_id, &agents);
    assert!(provider.admit(send_id, agents[0]).is_some());
    provider.offered(send_id, agents[0], true);
    assert!(provider.admit(send_id, agents[1]).is_some());
    provider.offered(send_id, agents[1], false);
    assert!(
        !provider.send_cancelled(send_id),
        "recording a busy refusal is not a cancel"
    );
    provider.send_finished(send_id, DispatchEnd::Ran);

    let held = env.state.ledger.send(send_id).unwrap();
    assert_eq!(held.recipients[1].status, SendStatus::Rejected);
    assert_eq!(held.recipients[1].code, Some(error_code::AGENT_BUSY));
    assert_eq!(
        held.recipients[2].status,
        SendStatus::NotDispatched,
        "an earlier recipient was refused, which is what not_dispatched means"
    );
    assert_eq!(held.recipients[2].code, None);
}

#[tokio::test]
async fn a_workflow_step_that_could_not_prepare_refuses_its_recipients_with_the_reason() {
    let (env, project_id, agents) = workflow_env().await;
    let send_id = Uuid::now_v7();
    let provider = workflow_provider(&env, project_id);
    provider.send_starting(send_id, &[agents[0], agents[1]]);
    provider.send_finished(send_id, DispatchEnd::NotReady("the prompt did not render"));

    let reply = wait_done(&env, project_id, send_id).await;
    assert!(reply.done);
    assert_eq!(reply.source, WaitSource::Live);
    for recipient in &reply.recipients {
        assert_eq!(recipient.status, SendStatus::Rejected);
        assert_eq!(recipient.code, Some(error_code::WORKFLOW_STEP_FAILED));
        assert_eq!(
            recipient.error.as_deref(),
            Some("the prompt did not render")
        );
    }
}

#[tokio::test]
async fn the_latest_answer_is_a_workflow_steps_reply_read_live() {
    let (env, project_id, agents) = workflow_env().await;
    let (run_id, send_id, gate) =
        workflow_with_a_running_first_send(&env, project_id, &agents).await;
    gate.notify_one();
    wait_workflow(
        &env.state,
        project_id,
        run_id,
        &WaitBudget::new(Some(30), false),
    )
    .await
    .unwrap();

    let output = read_latest_reply(&env.state, project_id, agents[0], &env.home())
        .await
        .unwrap();
    assert_eq!(output.status, LatestReplyStatus::Completed);
    assert_eq!(output.text.as_deref(), Some("greeting"));
    assert_eq!(output.send_id, Some(send_id));
    assert_eq!(
        output.source,
        OutputSource::Live,
        "the workflow's reply is read from what the app captured, not the session file"
    );
}

#[tokio::test]
async fn the_latest_answer_flags_a_workflow_step_still_preparing() {
    let (env, project_id, agents) = workflow_env().await;
    // The coder's turn is being prepared: no turn is open yet.
    let held = Arc::new(Notify::new());
    env.adapter
        .push(Script::reply("greeting").holding_dispatch(&held));
    let run_id = run_workflow(
        &env.state,
        &env.home(),
        pair_request(project_id, &agents),
        &WaitBudget::default_wait(),
    )
    .await
    .unwrap();
    first_workflow_send(&env, project_id, run_id).await;

    let output = read_latest_reply(&env.state, project_id, agents[0], &env.home())
        .await
        .unwrap();
    assert!(
        output.newer_turn_running,
        "the workflow's message is still to reach the agent"
    );
    held.notify_one();
}

#[tokio::test]
async fn after_a_restart_a_workflow_send_is_answered_from_the_journal() {
    let (env, project_id, agents) = workflow_env().await;
    let (run_id, send_id, gate) =
        workflow_with_a_running_first_send(&env, project_id, &agents).await;
    gate.notify_one();
    wait_workflow(
        &env.state,
        project_id,
        run_id,
        &WaitBudget::new(Some(30), false),
    )
    .await
    .unwrap();
    let env = env.restart();

    let reply = wait_done(&env, project_id, send_id).await;
    assert!(reply.done);
    assert_eq!(
        reply.source,
        WaitSource::Journal,
        "the ledger is empty after a restart, so the fallback answers as before"
    );
    assert_eq!(reply.recipients[0].status, SendStatus::Completed);
    assert_eq!(reply.recipients[0].text.as_deref(), Some("greeting"));
}

#[tokio::test]
async fn the_latest_answer_says_when_a_newer_turn_is_running() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let first = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "one"),
    )
    .await
    .unwrap();
    wait_done(&env, project_id, first.send_id).await;
    let settled = read_latest_reply(&env.state, project_id, agents[0], &env.home())
        .await
        .unwrap();
    assert!(!settled.newer_turn_running);

    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("two's answer", &gate));
    send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "two"),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, agents[0]).await;
    let mid_turn = read_latest_reply(&env.state, project_id, agents[0], &env.home())
        .await
        .unwrap();
    assert_eq!(
        mid_turn.text.as_deref(),
        Some("ack: one"),
        "still the previous answer"
    );
    assert!(
        mid_turn.newer_turn_running,
        "and it says a newer turn is running"
    );
    gate.notify_one();
}

/// Wait until the ledger shows `agent`'s reply to `send_id` as `status`.
async fn await_recipient_status(env: &TestEnv, send_id: Uuid, agent: AgentId, status: SendStatus) {
    let started = tokio::time::Instant::now();
    loop {
        let reached = env.state.ledger.send(send_id).is_some_and(|snapshot| {
            snapshot
                .recipients
                .iter()
                .any(|r| r.agent_id == agent && r.status == status)
        });
        if reached {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{agent} never reached {status:?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn a_one_recipient_cancel_lands_before_or_during_the_hand_off() {
    let (env, project_id, agents) = project_with_agents(&["a", "b", "c"]);
    let gate = Arc::new(Notify::new());
    for _ in 0..3 {
        env.adapter.push(Script::parked("reply", &gate));
    }
    let barrier = env.seam(TestSeam::OpsDispatch);
    let request = SendRequest {
        idempotency_key: Some("k".to_owned()),
        attachments: Vec::new(),
        ..text_send(project_id, agents.clone(), "hi")
    };
    let task = tokio::spawn(send_now(env.handle(), env.home(), request));
    // Held just before a's hand-off: a is marked accepted but not yet with
    // its agent, and b has not been offered the message at all.
    barrier.entered.notified().await;
    let send_id = env.state.ledger.send_id_for_key(project_id, "k").unwrap();
    for agent in [agents[0], agents[1]] {
        let cancelled = cancel_send(&env.state, project_id, send_id, Some(agent)).unwrap();
        assert_eq!(cancelled.outcome, super::send::CancelOutcome::Cancelled);
    }
    env.clear_seam(TestSeam::OpsDispatch);
    barrier.release.notify_one();

    let receipt = task.await.unwrap().unwrap();
    assert_eq!(
        receipt.recipients[1].status,
        SendStatus::Cancelled,
        "b was skipped: never offered the message"
    );
    // a was handed over during the cancel, and is cancelled once admitted.
    await_recipient_status(&env, send_id, agents[0], SendStatus::Cancelled).await;
    gate.notify_one();
    let done = wait_done(&env, project_id, send_id).await;
    let statuses: Vec<SendStatus> = done.recipients.iter().map(|r| r.status).collect();
    assert_eq!(
        statuses,
        vec![
            SendStatus::Cancelled,
            SendStatus::Cancelled,
            SendStatus::Completed
        ],
        "c, cancelled by nobody, carried on"
    );
    assert!(
        env.state
            .dispatcher
            .running_turn_kind(agents[1])
            .await
            .is_none()
    );
}

#[tokio::test]
async fn cancelling_a_waiting_forwards_only_recipient_stops_the_forward_at_once() {
    let (env, project_id, agents) = project_with_agents(&["source", "reader"]);
    let (source, reader) = (agents[0], agents[1]);
    let gate = Arc::new(Notify::new());
    env.adapter
        .push(Script::parked("the source's answer", &gate));
    let sources_reply = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, vec![source], "work"),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, source).await;
    // The forward waits for the busy source before it sends to anyone.
    let request = || ForwardRequest {
        project_id,
        sources: vec![latest_answer(project_id, source)],
        recipients: vec![reader],
        text: Some("see this".to_owned()),
        prompt: None,
        on_busy: OnBusyPolicy::Refuse,
        idempotency_key: Some("f".to_owned()),
        attachments: Vec::new(),
    };
    let task = tokio::spawn(forward(
        env.handle(),
        env.home(),
        request(),
        WaitBudget::new(Some(30), false),
    ));
    let started = tokio::time::Instant::now();
    let send_id = loop {
        if let Some(send_id) = env.state.ledger.send_id_for_key(project_id, "f") {
            break send_id;
        }
        assert!(started.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    let cancelled = cancel_send(&env.state, project_id, send_id, Some(reader)).unwrap();
    assert_eq!(cancelled.outcome, super::send::CancelOutcome::Cancelled);

    // The source is still running: the forward stops anyway, as the window's
    // cancel on a waiting forward does.
    let receipt = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("the forward stopped waiting")
        .unwrap()
        .expect("a cancelled send, not a failed one");
    assert_eq!(receipt.recipients[0].status, SendStatus::Cancelled);
    assert!(
        wait_send(
            &env.state,
            project_id,
            send_id,
            &WaitBudget::new(Some(0), false),
            &env.home()
        )
        .await
        .unwrap()
        .done,
        "a wait on it is done"
    );
    // A retry under the same key returns the cancellation; it does not send.
    let retried = forward(
        env.handle(),
        env.home(),
        request(),
        WaitBudget::default_wait(),
    )
    .await
    .unwrap();
    assert_eq!(retried.send_id, send_id);
    assert_eq!(retried.recipients[0].status, SendStatus::Cancelled);

    gate.notify_one();
    assert!(
        env.state.dispatcher.accepts_now(reader).await,
        "the reader never received the forward"
    );
    assert_eq!(
        wait_done(&env, project_id, sources_reply.send_id)
            .await
            .recipients[0]
            .status,
        SendStatus::Completed,
        "only the forward was cancelled: the source finished its reply"
    );
}

#[tokio::test]
async fn a_reply_the_user_started_is_not_the_orchestrators_to_cancel() {
    let (env, project_id, agents) = workflow_env().await;
    crate::commands::open_project_impl(&env.state, project_id).unwrap();
    // A message the user typed in the window.
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("the user's reply", &gate));
    let typed = Uuid::now_v7();
    send_message_impl(
        &env.state,
        agents[1],
        "mine",
        Vec::new(),
        typed,
        &env.home(),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, agents[1]).await;
    let refused = cancel_send(&env.state, project_id, typed, Some(agents[1])).unwrap_err();
    assert!(
        matches!(refused, AppError::SendNotCancellable { .. }),
        "{refused:?}"
    );
    assert!(
        env.state
            .dispatcher
            .running_turn_kind(agents[1])
            .await
            .is_some(),
        "the user's reply keeps running"
    );
    gate.notify_one();

    // A workflow the user started from the window.
    let park = Arc::new(Notify::new());
    env.adapter.push(Script::parked("greeting", &park));
    let run_id = crate::workflow_commands::invoke_workflow_impl(
        &env.state,
        project_id,
        "pair",
        false,
        // The window names agents by name.
        &BTreeMap::from([
            ("coder".to_owned(), InputValue::Text("coder".to_owned())),
            (
                "reviewers".to_owned(),
                InputValue::List(vec!["r1".to_owned(), "r2".to_owned()]),
            ),
            ("who".to_owned(), InputValue::Text("bob".to_owned())),
        ]),
        &env.home(),
    )
    .unwrap();
    await_turn_running(&env.state, agents[0]).await;
    let mut send_id = None;
    for _ in 0..100 {
        let reply = wait_workflow(
            &env.state,
            project_id,
            run_id,
            &WaitBudget::new(Some(0), false),
        )
        .await
        .unwrap();
        if let Some(first) = reply.sends.first() {
            send_id = Some(first.send_id);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let send_id = send_id.expect("the run reported its first send");
    let refused = cancel_send(&env.state, project_id, send_id, Some(agents[0])).unwrap_err();
    assert!(
        matches!(refused, AppError::ReplyInUserWorkflow { .. }),
        "{refused:?}"
    );
    // The ledger holds the send, but the whole-send form is refused too.
    let refused = cancel_send(&env.state, project_id, send_id, None).unwrap_err();
    assert!(
        matches!(refused, AppError::ReplyInUserWorkflow { .. }),
        "{refused:?}"
    );
    let refused = super::workflow::cancel_workflow_run(&env.state, project_id, run_id).unwrap_err();
    assert!(
        matches!(refused, AppError::RunStartedByUser { .. }),
        "{refused:?}"
    );
    park.notify_one();
}

#[tokio::test]
async fn the_latest_answer_flags_a_send_that_has_not_started_and_answers_at_once() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let first = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "one"),
    )
    .await
    .unwrap();
    wait_done(&env, project_id, first.send_id).await;
    // The agent's actor is still preparing the next turn: no turn is open,
    // and the actor answers no question until it is ready.
    let held = Arc::new(Notify::new());
    env.adapter
        .push(Script::reply("two's answer").holding_dispatch(&held));
    send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "two"),
    )
    .await
    .unwrap();
    let output = tokio::time::timeout(
        Duration::from_secs(2),
        read_latest_reply(&env.state, project_id, agents[0], &env.home()),
    )
    .await
    .expect("the read does not wait on the actor")
    .unwrap();
    assert_eq!(output.text.as_deref(), Some("ack: one"));
    assert!(
        output.newer_turn_running,
        "the orchestrator's own send is still to reach the agent"
    );
    held.notify_one();
}

const SEND_THEN_FAIL_WORKFLOW: &str = "name: send-then-fail
description: Sends to the first agent, then to the second, which is busy.
inputs:
  first: agent
  second: agent
steps:
  - label: First
    send:
      to: \"{{ first }}\"
      text: go
  - label: Second
    send:
      to: \"{{ second }}\"
      text: go
";

/// A workflow that sends to `first`, then fails at its next step because
/// `second` is busy, leaving `first`'s reply running. Returns the run id and
/// the gates holding both agents.
async fn failed_workflow_with_a_surviving_reply(
    env: &TestEnv,
    project_id: ProjectId,
    first: AgentId,
    second: AgentId,
    started_by_orchestrator: bool,
) -> (Uuid, Arc<Notify>) {
    let gate = Arc::new(Notify::new());
    // The second agent is busy with a reply of the user's.
    env.adapter.push(Script::parked("busy", &gate));
    send_message_impl(
        &env.state,
        second,
        "busy",
        Vec::new(),
        Uuid::now_v7(),
        &env.home(),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, second).await;
    env.adapter.push(Script::parked("first's reply", &gate));
    let run_id = if started_by_orchestrator {
        run_workflow(
            &env.state,
            &env.home(),
            RunWorkflowRequest {
                project_id,
                name: "send-then-fail".to_owned(),
                is_builtin: false,
                inputs: BTreeMap::from([
                    ("first".to_owned(), InputValue::Text(first.to_string())),
                    ("second".to_owned(), InputValue::Text(second.to_string())),
                ]),
                forward_sources: BTreeMap::new(),
            },
            &WaitBudget::default_wait(),
        )
        .await
        .unwrap()
    } else {
        crate::workflow_commands::invoke_workflow_impl(
            &env.state,
            project_id,
            "send-then-fail",
            false,
            &BTreeMap::from([
                ("first".to_owned(), InputValue::Text("coder".to_owned())),
                ("second".to_owned(), InputValue::Text("r1".to_owned())),
            ]),
            &env.home(),
        )
        .unwrap()
    };
    let ended = wait_workflow(
        &env.state,
        project_id,
        run_id,
        &WaitBudget::new(Some(30), false),
    )
    .await
    .unwrap();
    assert_eq!(
        ended.status,
        WorkflowRunStatus::Failed,
        "the second step found its agent busy"
    );
    assert!(
        env.state
            .dispatcher
            .running_turn_kind(first)
            .await
            .is_some(),
        "the reply the run already sent keeps running"
    );
    (run_id, gate)
}

#[tokio::test]
async fn a_reply_left_running_by_a_failed_workflow_is_still_the_orchestrators_to_cancel() {
    let (env, project_id, agents) = workflow_env().await;
    std::fs::write(
        env.workflows_dir().join("send-then-fail.yaml"),
        SEND_THEN_FAIL_WORKFLOW,
    )
    .unwrap();
    crate::commands::open_project_impl(&env.state, project_id).unwrap();
    let (run_id, gate) =
        failed_workflow_with_a_surviving_reply(&env, project_id, agents[0], agents[1], true).await;
    let ended = wait_workflow(
        &env.state,
        project_id,
        run_id,
        &WaitBudget::new(Some(0), false),
    )
    .await
    .unwrap();
    let first_send = ended.sends.first().expect("the run's first send").send_id;
    // Retiring the run's record does not revoke the right to cancel its reply.
    env.state
        .ledger
        .prune(chrono::Utc::now() + chrono::Duration::days(365));

    let cancelled = cancel_send(&env.state, project_id, first_send, Some(agents[0])).unwrap();
    assert_eq!(cancelled.outcome, super::send::CancelOutcome::Requested);
    let started = tokio::time::Instant::now();
    while env
        .state
        .dispatcher
        .running_turn_kind(agents[0])
        .await
        .is_some()
    {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the reply never stopped"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    gate.notify_waiters();
}

#[tokio::test]
async fn a_busy_agent_is_recorded_refused_and_its_users_reply_runs_on() {
    let (env, project_id, agents) = workflow_env().await;
    std::fs::write(
        env.workflows_dir().join("send-then-fail.yaml"),
        SEND_THEN_FAIL_WORKFLOW,
    )
    .unwrap();
    crate::commands::open_project_impl(&env.state, project_id).unwrap();
    let (first, second) = (agents[0], agents[1]);
    let (run_id, gate) =
        failed_workflow_with_a_surviving_reply(&env, project_id, first, second, true).await;
    let refused = workflow_sends(&env, project_id, run_id, 2).await[1];

    let reply = wait_done(&env, project_id, refused).await;
    assert_eq!(reply.source, WaitSource::Live);
    assert_eq!(reply.recipients[0].agent_id, second);
    assert_eq!(reply.recipients[0].status, SendStatus::Rejected);
    assert_eq!(reply.recipients[0].code, Some(error_code::AGENT_BUSY));
    assert!(
        env.state
            .dispatcher
            .running_turn_kind(second)
            .await
            .is_some(),
        "recording the refusal stopped nothing"
    );

    gate.notify_waiters();
    let answer = answer_once_idle(&env, project_id, second).await;
    assert_eq!(answer.status, LatestReplyStatus::Completed);
    assert_eq!(
        answer.text.as_deref(),
        Some("busy"),
        "the user's reply on the busy agent runs to its end"
    );
}

#[tokio::test]
async fn a_reply_left_running_by_a_failed_workflow_the_user_started_stays_theirs() {
    let (env, project_id, agents) = workflow_env().await;
    std::fs::write(
        env.workflows_dir().join("send-then-fail.yaml"),
        SEND_THEN_FAIL_WORKFLOW,
    )
    .unwrap();
    crate::commands::open_project_impl(&env.state, project_id).unwrap();
    let (run_id, gate) =
        failed_workflow_with_a_surviving_reply(&env, project_id, agents[0], agents[1], false).await;
    let ended = wait_workflow(
        &env.state,
        project_id,
        run_id,
        &WaitBudget::new(Some(0), false),
    )
    .await
    .unwrap();
    let first_send = ended.sends.first().expect("the run's first send").send_id;

    let refused = cancel_send(&env.state, project_id, first_send, Some(agents[0])).unwrap_err();
    assert!(
        matches!(refused, AppError::ReplyInUserWorkflow { .. }),
        "{refused:?}"
    );
    assert!(
        env.state
            .dispatcher
            .running_turn_kind(agents[0])
            .await
            .is_some()
    );
    gate.notify_waiters();
}

// --- the orchestrator's actions against the window's ---------------------
//
// Each pair runs the same situation through the window's command and through
// the agent tool, and checks the two end the same way. Where the two differ by
// design (a busy recipient, removal, whose work may be cancelled), the MCP
// plan's "Where agent access differs from the window" says why.

#[tokio::test]
async fn cancelling_one_reply_ends_the_same_from_the_window_and_the_tools() {
    for through_tools in [false, true] {
        let (env, project_id, agents) = project_with_agents(&["a", "b"]);
        let gate = Arc::new(Notify::new());
        env.adapter.push(Script::parked("reply", &gate));
        env.adapter.push(Script::parked("reply", &gate));
        let receipt = send_now(
            env.handle(),
            env.home(),
            text_send(project_id, agents.clone(), "go"),
        )
        .await
        .unwrap();
        await_turn_running(&env.state, agents[0]).await;
        await_turn_running(&env.state, agents[1]).await;
        if through_tools {
            cancel_send(&env.state, project_id, receipt.send_id, Some(agents[0])).unwrap();
        } else {
            // The stop control on one reply in the window.
            crate::commands::cancel_send_impl(&env.state, receipt.send_id, &[agents[0]]);
        }
        await_recipient_status(&env, receipt.send_id, agents[0], SendStatus::Cancelled).await;
        gate.notify_one();
        let done = wait_done(&env, project_id, receipt.send_id).await;
        let statuses: Vec<SendStatus> = done.recipients.iter().map(|r| r.status).collect();
        assert_eq!(
            statuses,
            vec![SendStatus::Cancelled, SendStatus::Completed],
            "through the tools: {through_tools}"
        );
    }
}

#[tokio::test]
async fn cancelling_a_workflows_send_ends_its_run_from_the_window_and_the_tools() {
    for through_tools in [false, true] {
        let (env, project_id, agents) = workflow_env().await;
        let (run_id, send_id, _gate) =
            workflow_with_a_running_first_send(&env, project_id, &agents).await;
        if through_tools {
            cancel_send(&env.state, project_id, send_id, None).unwrap();
        } else {
            // The stop control on the step's reply in the window.
            crate::commands::cancel_send_impl(&env.state, send_id, &[agents[0]]);
        }
        let ended = wait_workflow(
            &env.state,
            project_id,
            run_id,
            &WaitBudget::new(Some(30), false),
        )
        .await
        .unwrap();
        assert_eq!(
            ended.status,
            WorkflowRunStatus::Cancelled,
            "through the tools: {through_tools}"
        );
        let reply = wait_done(&env, project_id, send_id).await;
        assert_eq!(
            reply.recipients[0].status,
            SendStatus::Cancelled,
            "through the tools: {through_tools}"
        );
    }
}

#[tokio::test]
async fn cancelling_a_waiting_forward_sends_nothing_from_the_window_or_the_tools() {
    for through_tools in [false, true] {
        let (env, project_id, agents) = project_with_agents(&["source", "reader"]);
        let (source, reader) = (agents[0], agents[1]);
        let gate = Arc::new(Notify::new());
        env.adapter
            .push(Script::parked("the source's answer", &gate));
        send_now(
            env.handle(),
            env.home(),
            text_send(project_id, vec![source], "work"),
        )
        .await
        .unwrap();
        await_turn_running(&env.state, source).await;
        let sources = vec![ForwardSourceRef {
            agent_id: source,
            project_id,
        }];
        if through_tools {
            let request = ForwardRequest {
                project_id,
                sources: sources.iter().copied().map(ForwardFrom::from).collect(),
                recipients: vec![reader],
                text: Some("see this".to_owned()),
                prompt: None,
                on_busy: OnBusyPolicy::Refuse,
                idempotency_key: Some("f".to_owned()),
                attachments: Vec::new(),
            };
            let task = tokio::spawn(forward(
                env.handle(),
                env.home(),
                request,
                WaitBudget::new(Some(30), false),
            ));
            let send_id = loop {
                if let Some(send_id) = env.state.ledger.send_id_for_key(project_id, "f") {
                    break send_id;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            };
            cancel_send(&env.state, project_id, send_id, Some(reader)).unwrap();
            let receipt = tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .expect("stopped waiting")
                .unwrap()
                .unwrap();
            assert_eq!(receipt.recipients[0].status, SendStatus::Cancelled);
        } else {
            // The cancel control on the window's "waiting for source" row.
            let forward_id = Uuid::now_v7();
            let state = Arc::clone(&env.state);
            let home = env.home();
            let task = tokio::spawn(async move {
                crate::commands::forward_message_impl(
                    &state,
                    "see this".to_owned(),
                    sources,
                    forward_id,
                    &home,
                    project_id,
                )
                .await
            });
            while !lock(&env.state.forwards).contains_key(&forward_id) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            crate::commands::cancel_forward_impl(&env.state, forward_id);
            let outcome = tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .expect("stopped waiting")
                .unwrap()
                .unwrap();
            assert!(matches!(
                outcome,
                crate::commands::ForwardOutcome::Cancelled
            ));
        }
        gate.notify_one();
        assert!(
            env.state.dispatcher.accepts_now(reader).await,
            "the reader received nothing (through the tools: {through_tools})"
        );
    }
}

#[tokio::test]
async fn forking_a_busy_agent_is_refused_from_the_window_and_the_tools() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    // A first reply gives the agent a session to fork.
    let first = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "one"),
    )
    .await
    .unwrap();
    wait_done(&env, project_id, first.send_id).await;
    let gate = Arc::new(Notify::new());
    env.adapter.push(Script::parked("busy", &gate));
    send_now(
        env.handle(),
        env.home(),
        text_send(project_id, agents.clone(), "two"),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, agents[0]).await;

    let window = crate::commands::fork_agent_impl(
        &env.state,
        agents[0],
        &env.home(),
        std::path::Path::new("codex"),
    )
    .await
    .unwrap_err();
    let tools = super::fork::fork(
        env.handle(),
        env.home(),
        super::fork::ForkRequest {
            project_id,
            agent_id: agents[0],
            body: Body::Text {
                text: "aside".to_owned(),
            },
            idempotency_key: None,
            attachments: Vec::new(),
        },
        WaitBudget::default_wait(),
    )
    .await
    .unwrap_err();
    assert_eq!(error_code(&window), error_code::SOURCE_BUSY);
    assert_eq!(error_code(&tools), error_code::SOURCE_BUSY);
    gate.notify_one();
}

#[tokio::test]
async fn a_compaction_queues_behind_a_busy_agent_from_the_window_and_the_tools() {
    for through_tools in [false, true] {
        let (env, project_id, agents) = workflow_env().await;
        let coder = agents[0];
        // A finished reply gives the agent a session to compact.
        let first = send_now(
            env.handle(),
            env.home(),
            text_send(project_id, vec![coder], "one"),
        )
        .await
        .unwrap();
        wait_done(&env, project_id, first.send_id).await;
        let gate = Arc::new(Notify::new());
        env.adapter.push(Script::parked("busy", &gate));
        send_now(
            env.handle(),
            env.home(),
            text_send(project_id, vec![coder], "work"),
        )
        .await
        .unwrap();
        await_turn_running(&env.state, coder).await;
        if through_tools {
            let receipt = super::compact::compact(
                env.handle(),
                env.home(),
                super::compact::CompactRequest {
                    project_id,
                    agent_id: coder,
                    idempotency_key: None,
                },
                WaitBudget::default_wait(),
            )
            .await
            .expect("queued, not refused");
            assert_eq!(receipt.recipients[0].status, SendStatus::Accepted);
        } else {
            crate::commands::compact_agent_impl(&env.state, coder, Uuid::now_v7(), &env.home())
                .await
                .expect("queued, not refused");
        }
        gate.notify_one();
    }
}

// --- attachments ------------------------------------------------------------

/// Two files to attach, written outside the project: a screenshot and a log.
fn attachment_sources() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("shot.png");
    let log = dir.path().join("run.log");
    std::fs::write(&image, b"png bytes").unwrap();
    std::fs::write(&log, "line 1\nline 2\n").unwrap();
    (dir, image, log)
}

/// The attachments the journal recorded for `send_id`, one list per recipient.
fn journaled_attachments(
    env: &TestEnv,
    project_id: ProjectId,
    send_id: Uuid,
) -> Vec<Vec<switchboard_core::Attachment>> {
    switchboard_core::journal::read_records(&env.journal_path(project_id))
        .unwrap()
        .into_iter()
        .filter_map(|record| match record {
            JournalRecord::Send {
                send_id: id,
                attachments,
                ..
            } if id == send_id => Some(attachments),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_send_with_attachments_copies_them_in_and_gives_every_recipient_their_paths() {
    let (env, project_id, agents) = project_with_agents(&["a", "b"]);
    let (_sources, image, log) = attachment_sources();
    let mut request = text_send(project_id, agents, "look at these");
    request.attachments = vec![image.clone(), log.clone()];

    let receipt = send_now(env.handle(), env.home(), request).await.unwrap();
    assert!(
        receipt
            .recipients
            .iter()
            .all(|r| r.status == SendStatus::Accepted)
    );
    assert!(wait_done(&env, project_id, receipt.send_id).await.done);

    let journaled = journaled_attachments(&env, project_id, receipt.send_id);
    assert_eq!(journaled.len(), 2, "one journal send per recipient");
    assert_eq!(journaled[0], journaled[1], "both recipients share one copy");
    let attachments = &journaled[0];
    let described: Vec<_> = attachments
        .iter()
        .map(|a| (a.label.as_str(), a.kind, a.original_name.as_str()))
        .collect();
    assert_eq!(
        described,
        [
            (
                "image-1",
                switchboard_core::AttachmentKind::Image,
                "shot.png"
            ),
            ("text-1", switchboard_core::AttachmentKind::Text, "run.log"),
        ]
    );
    let attachments_dir = lock(&env.state.projects)[&project_id].attachments_dir();
    for (attachment, source) in attachments.iter().zip([&image, &log]) {
        let copy = PathBuf::from(&attachment.path);
        assert!(
            copy.starts_with(&attachments_dir),
            "a copy lives in the project"
        );
        assert_eq!(
            std::fs::read(&copy).unwrap(),
            std::fs::read(source).unwrap()
        );
    }

    let expected = switchboard_core::render_prompt_with_attachments("look at these", attachments);
    assert_eq!(
        *lock(&env.adapter.prompts),
        vec![expected.clone(), expected],
        "each agent gets the copies' paths after the message"
    );

    let events = env.emitter.snapshot();
    for kind in ["send_accepted", "user_message"] {
        let labels: Vec<Vec<String>> = events
            .iter()
            .filter(|(_, event)| event["type"] == kind)
            .map(|(_, event)| {
                event["attachments"]
                    .as_array()
                    .unwrap_or_else(|| panic!("{kind} carries the attachments"))
                    .iter()
                    .map(|a| a["label"].as_str().unwrap().to_owned())
                    .collect()
            })
            .collect();
        assert_eq!(
            labels,
            vec![vec!["image-1", "text-1"]; 2],
            "the window's {kind} row shows the attachments"
        );
    }
}

/// Opening a project deletes the attachment files no journaled send refers
/// to, and a send is journaled only when its turn starts. A send that copied
/// its files in before the window opened the project would lose them to that
/// pass; copying after the window answers keeps them.
#[tokio::test]
async fn attachments_copied_in_survive_the_cleanup_the_window_runs_when_it_opens_the_project() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let (env, sync, _window, mut requests) = env.with_window(Duration::from_mins(1));
    let (_sources, image, _log) = attachment_sources();
    let mut request = text_send(project_id, agents.clone(), "look");
    request.attachments = vec![image];
    let pending = tokio::spawn(send_now(env.handle(), env.home(), request));

    let sync_request = requests.recv().await.unwrap();
    crate::commands::reclaim_project_attachments_impl(&env.state, project_id, &[])
        .await
        .unwrap();
    assert!(sync.acknowledge(sync_request.request_id, ready(agents)));
    let receipt = pending.await.unwrap().unwrap();
    assert!(wait_done(&env, project_id, receipt.send_id).await.done);

    let journaled = journaled_attachments(&env, project_id, receipt.send_id);
    let copy = PathBuf::from(&journaled[0][0].path);
    assert!(copy.is_file(), "the copy the agent was given still exists");
    assert!(lock(&env.adapter.prompts)[0].contains(&journaled[0][0].path));
}

#[tokio::test]
async fn an_attachment_that_is_not_a_readable_file_sends_nothing() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let (sources, image, _log) = attachment_sources();
    let mut cases = vec![
        (PathBuf::from("shot.png"), "not an absolute path"),
        (sources.path().join("missing.png"), "cannot be read"),
        (sources.path().to_path_buf(), "is not a file"),
    ];
    if let Some(locked) = unreadable_file(sources.path()) {
        cases.push((locked, "cannot be read"));
    }
    for (bad, why) in cases {
        let mut request = text_send(project_id, agents.clone(), "look");
        request.attachments = vec![image.clone(), bad];
        let err = send_now(env.handle(), env.home(), request)
            .await
            .unwrap_err();
        assert_eq!(error_code(&err), error_code::INVALID_ARGUMENT);
        assert!(err.to_string().contains(why), "{err}");
    }
    assert!(lock(&env.adapter.prompts).is_empty());
    let attachments_dir = env.journal_path(project_id).with_file_name("attachments");
    assert!(
        std::fs::read_dir(&attachments_dir).map_or(true, |mut dir| dir.next().is_none()),
        "nothing was copied in"
    );
}

/// A file this process cannot open, or `None` where permissions do not stop
/// it (running as root), so the case is skipped rather than passing wrongly.
fn unreadable_file(dir: &std::path::Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.join("locked.png");
    std::fs::write(&path, b"secret").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    std::fs::File::open(&path).is_err().then_some(path)
}

/// A folder this process cannot write into, or `None` where permissions do
/// not stop it (running as root).
fn read_only_dir(dir: &std::path::Path) -> Option<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(dir).unwrap();
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let probe = dir.join("probe");
    if std::fs::write(&probe, b"x").is_ok() {
        let _ = std::fs::remove_file(probe);
        return None;
    }
    Some(())
}

fn writable_dir(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[tokio::test]
async fn a_retry_of_a_send_that_went_out_returns_its_receipt_after_the_file_is_deleted() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let (_sources, image, _log) = attachment_sources();
    let mut request = text_send(project_id, agents, "look");
    request.attachments = vec![image.clone()];
    request.idempotency_key = Some("k".to_owned());
    let first = send_now(env.handle(), env.home(), request.clone())
        .await
        .unwrap();
    wait_done(&env, project_id, first.send_id).await;

    std::fs::remove_file(&image).unwrap();
    let retry = send_now(env.handle(), env.home(), request)
        .await
        .expect("the retry returns the original receipt");
    assert_eq!(retry.send_id, first.send_id);
    assert_eq!(lock(&env.adapter.prompts).len(), 1, "nothing sent twice");
    let attachments_dir = env.journal_path(project_id).with_file_name("attachments");
    assert_eq!(
        std::fs::read_dir(attachments_dir).unwrap().count(),
        1,
        "nothing copied twice"
    );
}

#[tokio::test]
async fn a_forward_with_an_attachment_gives_the_recipient_the_forwarded_text_and_the_file() {
    let mut env = TestEnv::new();
    let project_id = env.project("proj");
    let recipient = env.agent(project_id, "r");
    let source = env.agent(project_id, "src");
    env.seed_claude_turn(project_id, source, "SOURCE OUTPUT", "m1");
    let env = env.restart();
    let (_sources, image, _log) = attachment_sources();
    let request = ForwardRequest {
        project_id,
        sources: vec![latest_answer(project_id, source)],
        recipients: vec![recipient],
        text: Some("Please review:".to_owned()),
        prompt: None,
        on_busy: OnBusyPolicy::Refuse,
        idempotency_key: None,
        attachments: vec![image],
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

    let journaled = journaled_attachments(&env, project_id, receipt.send_id);
    assert_eq!(journaled.len(), 1);
    assert_eq!(journaled[0][0].label, "image-1");
    let forwarded = "Please review:\n\n=== START forwarded from src ===\nSOURCE OUTPUT\n=== END forwarded from src ===";
    assert_eq!(
        lock(&env.adapter.prompts)[0],
        switchboard_core::render_prompt_with_attachments(forwarded, &journaled[0])
    );
}

fn fork_with_attachments(
    project_id: ProjectId,
    source: AgentId,
    key: Option<&str>,
    attachments: Vec<PathBuf>,
) -> ForkRequest {
    ForkRequest {
        attachments,
        ..fork_request(project_id, source, "branch", key)
    }
}

#[tokio::test]
async fn a_forks_first_message_carries_its_attachments() {
    let (env, project_id, source) = forkable_source();
    let (_sources, image, _log) = attachment_sources();
    let receipt = fork_now(
        &env,
        fork_with_attachments(project_id, source, None, vec![image]),
    )
    .await
    .unwrap();
    assert_eq!(receipt.send.recipients[0].status, SendStatus::Accepted);
    wait_done(&env, project_id, receipt.send.send_id).await;

    let journaled = journaled_attachments(&env, project_id, receipt.send.send_id);
    assert_eq!(journaled.len(), 1);
    assert_eq!(journaled[0][0].label, "image-1");
    assert_eq!(
        *lock(&env.adapter.prompts),
        vec![switchboard_core::render_prompt_with_attachments(
            "branch",
            &journaled[0]
        )]
    );
}

#[tokio::test]
async fn a_fork_with_an_unreadable_attachment_is_refused_and_no_fork_is_created() {
    let (env, project_id, source) = forkable_source();
    let (sources, _image, _log) = attachment_sources();
    let Some(locked) = unreadable_file(sources.path()) else {
        return;
    };
    let before = roster(&env, project_id).len();
    let err = fork_now(
        &env,
        fork_with_attachments(project_id, source, Some("k"), vec![locked]),
    )
    .await
    .unwrap_err();
    assert_eq!(error_code(&err), error_code::INVALID_ARGUMENT);
    assert!(err.to_string().contains("cannot be read"), "{err}");
    assert_eq!(roster(&env, project_id).len(), before, "no fork was made");
    assert!(lock(&env.adapter.prompts).is_empty());
}

#[tokio::test]
async fn a_fork_retried_with_the_same_key_and_different_files_is_refused() {
    let (env, project_id, source) = forkable_source();
    let (_sources, image, log) = attachment_sources();
    let first = fork_now(
        &env,
        fork_with_attachments(project_id, source, Some("k"), vec![image]),
    )
    .await
    .unwrap();
    wait_done(&env, project_id, first.send.send_id).await;

    let err = fork_now(
        &env,
        fork_with_attachments(project_id, source, Some("k"), vec![log]),
    )
    .await
    .unwrap_err();
    assert_eq!(error_code(&err), error_code::INVALID_ARGUMENT);
    assert!(err.to_string().contains("idempotency_key"), "{err}");
    assert_eq!(lock(&env.adapter.prompts).len(), 1);
}

#[tokio::test]
async fn a_fork_whose_copy_fails_keeps_the_fork_and_the_same_key_sends_it_the_message() {
    let (env, project_id, source) = forkable_source();
    let (_sources, image, _log) = attachment_sources();
    let attachments_dir = env.journal_path(project_id).with_file_name("attachments");
    if read_only_dir(&attachments_dir).is_none() {
        return;
    }
    let request = || fork_with_attachments(project_id, source, Some("k"), vec![image.clone()]);

    let refused = fork_now(&env, request()).await.unwrap();
    writable_dir(&attachments_dir);
    let fork = fork_of(&refused);
    assert_eq!(refused.send.recipients[0].status, SendStatus::Rejected);
    assert!(lock(&env.adapter.prompts).is_empty(), "nothing was sent");
    assert_eq!(roster(&env, project_id).len(), 2, "the fork exists");

    let resent = fork_now(&env, request()).await.unwrap();
    assert_eq!(fork_of(&resent).id, fork.id, "the same fork, not a second");
    assert_eq!(resent.send.recipients[0].status, SendStatus::Accepted);
    wait_done(&env, project_id, resent.send.send_id).await;
    assert_eq!(roster(&env, project_id).len(), 2);
    let journaled = journaled_attachments(&env, project_id, resent.send.send_id);
    assert_eq!(
        *lock(&env.adapter.prompts),
        vec![switchboard_core::render_prompt_with_attachments(
            "branch",
            &journaled[0]
        )]
    );
}

#[tokio::test]
async fn a_forks_attachments_survive_the_cleanup_the_window_runs_when_it_opens_the_project() {
    let (env, project_id, source) = forkable_source();
    let (env, sync, _window, mut requests) = env.with_window(Duration::from_mins(1));
    let (_sources, image, _log) = attachment_sources();
    let task = spawn_fork(
        &env,
        fork_with_attachments(project_id, source, None, vec![image]),
    );
    let request = requests.recv().await.unwrap();
    let carried = request.fork.expect("the request names the fork");
    crate::commands::reclaim_project_attachments_impl(&env.state, project_id, &[])
        .await
        .unwrap();
    sync.acknowledge(
        request.request_id,
        SyncAck::Ready {
            listening_agents: vec![source, carried.fork_id],
        },
    );
    let receipt = task.await.unwrap().unwrap();
    assert_eq!(receipt.send.recipients[0].status, SendStatus::Accepted);
    wait_done(&env, project_id, receipt.send.send_id).await;
    let journaled = journaled_attachments(&env, project_id, receipt.send.send_id);
    assert!(
        PathBuf::from(&journaled[0][0].path).is_file(),
        "the copy the fork was given still exists"
    );
}

/// How long a test waits on a held attachment step before failing instead of
/// hanging the suite.
const HELD: Duration = Duration::from_secs(5);

fn send_with_key_and_file(
    project_id: ProjectId,
    agents: Vec<AgentId>,
    file: PathBuf,
) -> SendRequest {
    let mut request = text_send(project_id, agents, "look");
    request.attachments = vec![file];
    request.idempotency_key = Some("k".to_owned());
    request
}

#[tokio::test]
async fn a_send_whose_file_check_never_returns_times_out_at_its_deadline_with_nothing_sent() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let (_sources, image, _log) = attachment_sources();
    let held = env.seam(TestSeam::AttachmentCheck);
    let (handle, home) = (env.handle(), env.home());
    let request = send_with_key_and_file(project_id, agents, image);
    let task = tokio::spawn(async move { send(handle, home, request, short()).await });
    tokio::time::timeout(HELD, held.entered.notified())
        .await
        .expect("the check reached its hold");

    let outcome = tokio::time::timeout(HELD, task).await;
    held.release.notify_one();
    let err = outcome
        .expect("the send answered while the check was still held")
        .unwrap()
        .unwrap_err();
    assert!(
        err.to_string().contains("checking the attachments"),
        "{err}"
    );
    assert!(lock(&env.adapter.prompts).is_empty());
    assert!(
        env.state.ledger.send_id_for_key(project_id, "k").is_none(),
        "the key is free for a retry"
    );
}

#[tokio::test]
async fn cancelling_a_send_while_its_file_check_is_stuck_ends_it_with_nothing_sent() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let (_sources, image, _log) = attachment_sources();
    let held = env.seam(TestSeam::AttachmentCheck);
    let task = tokio::spawn(send_now(
        env.handle(),
        env.home(),
        send_with_key_and_file(project_id, agents, image),
    ));
    tokio::time::timeout(HELD, held.entered.notified())
        .await
        .expect("the check reached its hold");
    let send_id = env.state.ledger.send_id_for_key(project_id, "k").unwrap();
    cancel_send(&env.state, project_id, send_id, None).unwrap();

    let outcome = tokio::time::timeout(HELD, task).await;
    held.release.notify_one();
    let receipt = outcome
        .expect("the cancel ended the send while the check was still held")
        .unwrap()
        .unwrap();
    assert_eq!(receipt.recipients[0].status, SendStatus::Cancelled);
    assert!(lock(&env.adapter.prompts).is_empty());
}

#[tokio::test]
async fn a_fork_whose_file_check_never_returns_times_out_and_creates_no_fork() {
    let (env, project_id, source) = forkable_source();
    let (_sources, image, _log) = attachment_sources();
    let held = env.seam(TestSeam::AttachmentCheck);
    let before = roster(&env, project_id).len();
    let (handle, home) = (env.handle(), env.home());
    let request = fork_with_attachments(project_id, source, Some("k"), vec![image]);
    let task = tokio::spawn(async move { fork(handle, home, request, short()).await });
    tokio::time::timeout(HELD, held.entered.notified())
        .await
        .expect("the check reached its hold");

    let outcome = tokio::time::timeout(HELD, task).await;
    held.release.notify_one();
    let err = outcome
        .expect("the fork answered while the check was still held")
        .unwrap()
        .unwrap_err();
    assert!(
        err.to_string().contains("checking the attachments"),
        "{err}"
    );
    assert_eq!(roster(&env, project_id).len(), before, "no fork was made");
    assert!(lock(&env.adapter.prompts).is_empty());
}

#[tokio::test]
async fn cancelling_a_send_while_its_copy_is_stuck_ends_it_with_nothing_sent() {
    let (env, project_id, agents) = project_with_agents(&["a"]);
    let (_sources, image, _log) = attachment_sources();
    let held = env.seam(TestSeam::AttachmentCopy);
    let task = tokio::spawn(send_now(
        env.handle(),
        env.home(),
        send_with_key_and_file(project_id, agents, image),
    ));
    tokio::time::timeout(HELD, held.entered.notified())
        .await
        .expect("the copy reached its hold");
    let send_id = env.state.ledger.send_id_for_key(project_id, "k").unwrap();
    cancel_send(&env.state, project_id, send_id, None).unwrap();

    let outcome = tokio::time::timeout(HELD, task).await;
    held.release.notify_one();
    let receipt = outcome
        .expect("the cancel ended the send while the copy was still held")
        .unwrap()
        .unwrap();
    assert_eq!(receipt.recipients[0].status, SendStatus::Cancelled);
    assert!(lock(&env.adapter.prompts).is_empty());
}

// --- forwarding a named reply -------------------------------------------------

/// A forward source naming one of the agent's replies.
fn named_reply(project_id: ProjectId, agent_id: AgentId, send_id: Uuid) -> ForwardFrom {
    ForwardFrom {
        project_id,
        agent_id,
        send_id: Some(send_id),
    }
}

fn forward_to(
    project_id: ProjectId,
    sources: Vec<ForwardFrom>,
    recipient: AgentId,
    key: Option<&str>,
) -> ForwardRequest {
    ForwardRequest {
        project_id,
        sources,
        recipients: vec![recipient],
        text: Some("see this".to_owned()),
        prompt: None,
        on_busy: OnBusyPolicy::Refuse,
        idempotency_key: key.map(str::to_owned),
        attachments: Vec::new(),
    }
}

/// Send `text` to `agent` alone, wait for its reply, and return the send.
async fn answered(env: &TestEnv, project_id: ProjectId, agent: AgentId, text: &str) -> Uuid {
    let receipt = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, vec![agent], text),
    )
    .await
    .unwrap();
    wait_done(env, project_id, receipt.send_id).await;
    receipt.send_id
}

/// Forward under a 50-second budget, failing the test if it takes over five
/// seconds: a forward naming a reply must never wait on the agent.
async fn forward_at_once(
    env: &TestEnv,
    request: ForwardRequest,
) -> Result<super::ledger::SendReceipt, AppError> {
    tokio::time::timeout(
        Duration::from_secs(5),
        forward(
            env.handle(),
            env.home(),
            request,
            WaitBudget::default_wait(),
        ),
    )
    .await
    .expect("a forward naming a reply does not wait")
}

fn last_prompt(env: &TestEnv) -> String {
    lock(&env.adapter.prompts).last().cloned().unwrap()
}

#[tokio::test]
async fn a_forward_naming_a_reply_sends_it_though_the_agent_has_answered_since() {
    let (env, project_id, agents) = project_with_agents(&["src", "reader"]);
    let (src, reader) = (agents[0], agents[1]);
    env.adapter.push_for(src, Script::reply("FIRST ANSWER"));
    env.adapter.push_for(src, Script::reply("SECOND ANSWER"));
    let first = answered(&env, project_id, src, "one").await;
    answered(&env, project_id, src, "two").await;

    let receipt = forward_at_once(
        &env,
        forward_to(
            project_id,
            vec![named_reply(project_id, src, first)],
            reader,
            None,
        ),
    )
    .await
    .unwrap();
    wait_done(&env, project_id, receipt.send_id).await;
    assert_eq!(
        last_prompt(&env),
        "see this\n\n=== START forwarded from src ===\nFIRST ANSWER\n=== END forwarded from src ===",
        "the named reply, verbatim, under the agent's usual label"
    );
}

#[tokio::test]
async fn a_forward_naming_a_finished_reply_goes_out_while_the_agent_is_busy() {
    let (env, project_id, agents) = project_with_agents(&["src", "reader"]);
    let (src, reader) = (agents[0], agents[1]);
    env.adapter.push_for(src, Script::reply("THE REPORT"));
    let report = answered(&env, project_id, src, "report").await;
    let gate = Arc::new(Notify::new());
    env.adapter
        .push_for(src, Script::parked("still going", &gate));
    send_now(
        env.handle(),
        env.home(),
        text_send(project_id, vec![src], "more work"),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, src).await;

    let receipt = forward_at_once(
        &env,
        forward_to(
            project_id,
            vec![named_reply(project_id, src, report)],
            reader,
            None,
        ),
    )
    .await
    .expect("a finished reply is forwarded without waiting for the running turn");
    wait_done(&env, project_id, receipt.send_id).await;
    assert!(last_prompt(&env).contains("THE REPORT"));
    gate.notify_waiters();
}

#[tokio::test]
async fn a_forward_naming_an_unfinished_reply_is_refused_at_once_and_sends_nothing() {
    let (env, project_id, agents) = project_with_agents(&["src", "reader"]);
    let (src, reader) = (agents[0], agents[1]);
    let gate = Arc::new(Notify::new());
    env.adapter
        .push_for(src, Script::parked("half done", &gate));
    let running = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, vec![src], "work"),
    )
    .await
    .unwrap()
    .send_id;
    await_turn_running(&env.state, src).await;
    let queued = send_now(
        env.handle(),
        env.home(),
        SendRequest {
            on_busy: OnBusyPolicy::Queue,
            ..text_send(project_id, vec![src], "after that")
        },
    )
    .await
    .unwrap()
    .send_id;
    let dispatched_before = lock(&env.adapter.prompts).len();

    for (send_id, state) in [(running, "running"), (queued, "waiting in line")] {
        let err = forward_at_once(
            &env,
            forward_to(
                project_id,
                vec![named_reply(project_id, src, send_id)],
                reader,
                Some("named"),
            ),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err.root(), AppError::ForwardReplyUnfinished { state: s, .. } if *s == state),
            "{err:?}"
        );
        assert_eq!(error_code(&err), error_code::SOURCE_BUSY);
    }
    assert_eq!(
        lock(&env.adapter.prompts).len(),
        dispatched_before,
        "nothing was sent to the reader"
    );

    gate.notify_waiters();
    wait_done(&env, project_id, queued).await;
    forward_at_once(
        &env,
        forward_to(
            project_id,
            vec![named_reply(project_id, src, running)],
            reader,
            Some("named"),
        ),
    )
    .await
    .expect("the key was released, so the retry once the reply finished goes out");
}

#[tokio::test]
async fn a_forward_naming_a_failed_or_cancelled_reply_is_refused() {
    let (env, project_id, agents) = project_with_agents(&["src", "reader"]);
    let (src, reader) = (agents[0], agents[1]);
    env.adapter.push_for(src, Script::failing("partial"));
    let failed = answered(&env, project_id, src, "try").await;
    let gate = Arc::new(Notify::new());
    env.adapter.push_for(src, Script::parked("cut off", &gate));
    let cancelled = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, vec![src], "again"),
    )
    .await
    .unwrap()
    .send_id;
    await_turn_running(&env.state, src).await;
    cancel_send(&env.state, project_id, cancelled, None).unwrap();
    wait_done(&env, project_id, cancelled).await;

    for (send_id, state) in [(failed, "failed"), (cancelled, "was cancelled")] {
        let err = forward_at_once(
            &env,
            forward_to(
                project_id,
                vec![named_reply(project_id, src, send_id)],
                reader,
                None,
            ),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err.root(), AppError::ForwardReplyNotCompleted { state: s, .. } if *s == state),
            "{err:?}"
        );
        assert_eq!(error_code(&err), error_code::INVALID_ARGUMENT);
    }
    assert!(
        !lock(&env.adapter.prompts)
            .iter()
            .any(|p| p.starts_with("see this")),
        "nothing was forwarded"
    );
}

#[tokio::test]
async fn a_forward_naming_a_send_its_source_did_not_receive_is_send_unknown() {
    let mut env = TestEnv::new();
    let p1 = env.project("one");
    let p2 = env.project("two");
    let a = env.agent(p1, "a");
    let b = env.agent(p1, "b");
    let reader = env.agent(p1, "reader");
    let elsewhere = env.agent(p2, "x");
    let env = env.restart();
    let to_a = answered(&env, p1, a, "only a").await;
    let in_p2 = answered(&env, p2, elsewhere, "in two").await;

    for source in [
        // Another agent's send.
        named_reply(p1, b, to_a),
        // A send made in another project than the source's.
        named_reply(p1, a, in_p2),
    ] {
        let err = forward_at_once(&env, forward_to(p1, vec![source], reader, None))
            .await
            .unwrap_err();
        assert!(
            matches!(err.root(), AppError::ForwardReplyUnknown { .. }),
            "{err:?}"
        );
        assert_eq!(error_code(&err), error_code::SEND_UNKNOWN);
    }
}

#[tokio::test]
async fn named_and_latest_sources_keep_their_declared_order() {
    let (env, project_id, agents) = project_with_agents(&["a", "b", "reader"]);
    let (a, b, reader) = (agents[0], agents[1], agents[2]);
    env.adapter.push_for(a, Script::reply("A FIRST"));
    env.adapter.push_for(a, Script::reply("A LATEST"));
    env.adapter.push_for(b, Script::reply("B LATEST"));
    let a_first = answered(&env, project_id, a, "one").await;
    answered(&env, project_id, a, "two").await;
    answered(&env, project_id, b, "one").await;

    let receipt = forward_at_once(
        &env,
        forward_to(
            project_id,
            vec![
                latest_answer(project_id, b),
                named_reply(project_id, a, a_first),
            ],
            reader,
            None,
        ),
    )
    .await
    .unwrap();
    wait_done(&env, project_id, receipt.send_id).await;
    assert_eq!(
        last_prompt(&env),
        "see this\n\n=== START forwarded from b ===\nB LATEST\n=== END forwarded from b ===\n\n\
         === START forwarded from a ===\nA FIRST\n=== END forwarded from a ==="
    );
}

#[tokio::test]
async fn a_prompt_argument_can_forward_a_named_reply() {
    let mut env = TestEnv::new().with_prompts_and_workflows();
    std::fs::write(env.prompts_dir().join("review.md"), REVIEW_PROMPT).unwrap();
    env.state.prompts.sync().await;
    let project_id = env.project("proj");
    let coder = env.agent(project_id, "coder");
    let reviewer = env.agent(project_id, "reviewer");
    env.adapter.push_for(coder, Script::reply("THE FIRST DIFF"));
    env.adapter.push_for(coder, Script::reply("SOMETHING ELSE"));
    let diff = answered(&env, project_id, coder, "diff").await;
    answered(&env, project_id, coder, "chat").await;

    let receipt = forward_at_once(
        &env,
        ForwardRequest {
            project_id,
            sources: Vec::new(),
            recipients: vec![reviewer],
            text: None,
            prompt: Some(ForwardPrompt {
                provider: "local".to_owned(),
                name: "review".to_owned(),
                args: BTreeMap::new(),
                forward_args: BTreeMap::from([(
                    "diff".to_owned(),
                    vec![named_reply(project_id, coder, diff)],
                )]),
            }),
            on_busy: OnBusyPolicy::Refuse,
            idempotency_key: None,
            attachments: Vec::new(),
        },
    )
    .await
    .unwrap();
    wait_done(&env, project_id, receipt.send_id).await;
    let prompt = last_prompt(&env);
    assert!(
        prompt.starts_with("Review this:\n=== START forwarded from coder ===\nTHE FIRST DIFF"),
        "{prompt}"
    );
}

#[tokio::test]
async fn a_retry_key_reused_with_another_named_reply_is_refused() {
    let (env, project_id, agents) = project_with_agents(&["src", "reader"]);
    let (src, reader) = (agents[0], agents[1]);
    let first = answered(&env, project_id, src, "one").await;
    let second = answered(&env, project_id, src, "two").await;
    let receipt = forward_at_once(
        &env,
        forward_to(
            project_id,
            vec![named_reply(project_id, src, first)],
            reader,
            Some("k"),
        ),
    )
    .await
    .unwrap();
    wait_done(&env, project_id, receipt.send_id).await;

    let err = forward_at_once(
        &env,
        forward_to(
            project_id,
            vec![named_reply(project_id, src, second)],
            reader,
            Some("k"),
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(error_code(&err), error_code::INVALID_ARGUMENT, "{err:?}");
}

#[tokio::test]
async fn a_forward_names_a_reply_from_before_a_restart() {
    let (env, project_id, agents) = project_with_agents(&["src", "reader"]);
    let (src, reader) = (agents[0], agents[1]);
    env.adapter
        .push_for(src, Script::reply("BEFORE THE RESTART"));
    let first = answered(&env, project_id, src, "one").await;
    answered(&env, project_id, src, "two").await;
    let env = env.restart();

    let receipt = forward_at_once(
        &env,
        forward_to(
            project_id,
            vec![named_reply(project_id, src, first)],
            reader,
            None,
        ),
    )
    .await
    .expect("read from the journal, through the reply's durable link");
    wait_done(&env, project_id, receipt.send_id).await;
    assert!(last_prompt(&env).contains("BEFORE THE RESTART"));
}

#[tokio::test]
async fn a_forward_names_a_workflow_steps_reply() {
    let (env, project_id, agents) = workflow_env().await;
    env.adapter.push_for(agents[1], Script::reply("R1 REVIEW"));
    let run_id = run_workflow(
        &env.state,
        &env.home(),
        pair_request(project_id, &agents),
        &WaitBudget::default_wait(),
    )
    .await
    .unwrap();
    let run = wait_workflow(
        &env.state,
        project_id,
        run_id,
        &WaitBudget::new(Some(30), false),
    )
    .await
    .unwrap();
    assert_eq!(run.status, WorkflowRunStatus::Completed);
    let review = run.sends[1].send_id;
    // r1's reply to the run's second send, named by the id wait_workflow lists.
    let receipt = forward_at_once(
        &env,
        forward_to(
            project_id,
            vec![named_reply(project_id, agents[1], review)],
            agents[0],
            None,
        ),
    )
    .await
    .unwrap();
    wait_done(&env, project_id, receipt.send_id).await;
    assert!(last_prompt(&env).contains("=== START forwarded from r1 ===\nR1 REVIEW"));
}

#[tokio::test]
async fn a_forward_naming_a_reply_its_send_has_not_reached_yet_is_unfinished() {
    let (env, project_id, agents) = project_with_agents(&["a", "b", "reader"]);
    let (a, b, reader) = (agents[0], agents[1], agents[2]);
    env.adapter.push_for(b, Script::reply("B ANSWER"));
    let request = SendRequest {
        idempotency_key: Some("fan".to_owned()),
        ..text_send(project_id, vec![a, b], "hi")
    };
    let barrier = env.seam(TestSeam::OpsDispatch);
    let task = tokio::spawn(send_now(env.handle(), env.home(), request.clone()));
    // Hold the send at its first recipient: a is being offered the message,
    // and b has not been reached.
    barrier.entered.notified().await;
    let err = send(env.handle(), env.home(), request, short())
        .await
        .unwrap_err();
    let AppError::RequestInProgress { send_id } = err else {
        panic!("the retry joins the send still handing out: {err:?}");
    };

    let err = forward_at_once(
        &env,
        forward_to(
            project_id,
            vec![named_reply(project_id, b, send_id)],
            reader,
            Some("named"),
        ),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            err.root(),
            AppError::ForwardReplyUnfinished {
                state: "waiting to be sent",
                ..
            }
        ),
        "not yet offered is not never offered: {err:?}"
    );
    assert_eq!(error_code(&err), error_code::SOURCE_BUSY);
    assert!(err.to_string().contains("wait_send"), "{err}");

    env.clear_seam(TestSeam::OpsDispatch);
    barrier.release.notify_one();
    task.await.unwrap().unwrap();
    wait_done(&env, project_id, send_id).await;
    let receipt = forward_at_once(
        &env,
        forward_to(
            project_id,
            vec![named_reply(project_id, b, send_id)],
            reader,
            Some("named"),
        ),
    )
    .await
    .expect("once the send reaches b and b answers, the retry goes out");
    wait_done(&env, project_id, receipt.send_id).await;
    assert!(last_prompt(&env).contains("B ANSWER"));
}

#[tokio::test]
async fn a_forward_refuses_a_reply_matched_to_its_send_only_by_position() {
    let mut env = TestEnv::new();
    let project_id = env.project("proj");
    let src = env.agent(project_id, "src");
    let reader = env.agent(project_id, "reader");
    // A send with no durable link to its reply, as a harness that records none
    // leaves it: the reply is matched to the send by position only.
    let send_id = Uuid::now_v7();
    let at = Utc::now() - chrono::Duration::minutes(1);
    switchboard_core::journal::append_record(
        &env.journal_path(project_id),
        &JournalRecord::Send {
            send_id,
            turn_id: Uuid::now_v7(),
            agent_id: src,
            prompt: "hi".to_owned(),
            attachments: Vec::new(),
            at,
        },
    )
    .unwrap();
    env.seed_claude_turn(project_id, src, "A GUESSED REPLY", "m1");
    let env = env.restart();

    let page = read_reply(
        &env.state,
        project_id,
        send_id,
        src,
        0,
        ReadFrom::Auto,
        OUTPUT_PAGE_CHARS,
        &env.home(),
    )
    .await
    .unwrap();
    assert_eq!(
        (page.source, page.text.as_str()),
        (OutputSource::Positional, "A GUESSED REPLY"),
        "the reply is readable, and matched only by position"
    );

    let err = forward_at_once(
        &env,
        forward_to(
            project_id,
            vec![named_reply(project_id, src, send_id)],
            reader,
            None,
        ),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err.root(), AppError::ForwardReplyUnconfirmed { .. }),
        "{err:?}"
    );
    assert_eq!(error_code(&err), error_code::INVALID_ARGUMENT);
    let message = err.to_string();
    assert!(
        message.contains("read_reply") && !message.contains("leave out send_id"),
        "{message}"
    );
    assert!(lock(&env.adapter.prompts).is_empty(), "nothing was sent");
}

#[tokio::test]
async fn a_named_reply_in_hand_is_forwarded_though_its_projects_journal_is_damaged() {
    let mut env = TestEnv::new();
    let p1 = env.project("one");
    let p2 = env.project("two");
    let reader = env.agent(p1, "reader");
    let src = env.agent(p2, "src");
    let env = env.restart();
    env.adapter.push_for(src, Script::reply("HELD REPLY"));
    let held = answered(&env, p2, src, "one").await;
    let mut journal = std::fs::OpenOptions::new()
        .append(true)
        .open(env.journal_path(p2))
        .unwrap();
    std::io::Write::write_all(&mut journal, b"not a journal record\n").unwrap();

    let receipt = forward_at_once(
        &env,
        forward_to(p1, vec![named_reply(p2, src, held)], reader, None),
    )
    .await
    .expect("a reply already in hand needs no journal");
    wait_done(&env, p1, receipt.send_id).await;
    assert!(last_prompt(&env).contains("HELD REPLY"));
}

// --- reading a workflow run's replies -----------------------------------------

/// Two rounds to the same reviewers, so each has answered two of the run's
/// sends by the end.
const ROUNDS_WORKFLOW: &str = "name: rounds
description: test
inputs:
  reviewers:
    type: [agent]
steps:
  - label: First round
    send:
      to: \"{{ reviewers }}\"
      text: first round
  - label: First received
    wait_for_all:
      agents: \"{{ reviewers }}\"
  - label: Second round
    send:
      to: \"{{ reviewers }}\"
      text: second round
  - label: Second received
    wait_for_all:
      agents: \"{{ reviewers }}\"
";

/// An env with the rounds workflow and two reviewers, `r1` and `r2`.
fn rounds_env() -> (TestEnv, ProjectId, Vec<AgentId>) {
    let mut env = TestEnv::new().with_prompts_and_workflows();
    std::fs::write(env.workflows_dir().join("rounds.yaml"), ROUNDS_WORKFLOW).unwrap();
    let project_id = env.project("proj");
    let agents = ["r1", "r2"]
        .iter()
        .map(|n| env.agent(project_id, n))
        .collect();
    (env, project_id, agents)
}

async fn run_rounds(env: &TestEnv, project_id: ProjectId, reviewers: &[AgentId]) -> Uuid {
    let run_id = run_workflow(
        &env.state,
        &env.home(),
        RunWorkflowRequest {
            project_id,
            name: "rounds".to_owned(),
            is_builtin: false,
            inputs: BTreeMap::from([(
                "reviewers".to_owned(),
                InputValue::List(reviewers.iter().map(ToString::to_string).collect()),
            )]),
            forward_sources: BTreeMap::new(),
        },
        &WaitBudget::default_wait(),
    )
    .await
    .unwrap();
    let run = wait_workflow(
        &env.state,
        project_id,
        run_id,
        &WaitBudget::new(Some(30), false),
    )
    .await
    .unwrap();
    assert_eq!(run.status, WorkflowRunStatus::Completed, "{run:?}");
    run_id
}

fn reply_texts(send: &RunSendReplies) -> Vec<(String, SendStatus, Option<String>)> {
    send.recipients
        .iter()
        .map(|r| (r.name.clone(), r.reply.status, r.reply.text.clone()))
        .collect()
}

#[tokio::test]
async fn read_workflow_run_returns_each_rounds_replies_under_its_own_step() {
    let (env, project_id, reviewers) = rounds_env();
    for (agent, tag) in reviewers.iter().zip(["R1", "R2"]) {
        env.adapter
            .push_for(*agent, Script::reply(&format!("{tag} FIRST")));
        env.adapter
            .push_for(*agent, Script::reply(&format!("{tag} SECOND")));
    }
    let run_id = run_rounds(&env, project_id, &reviewers).await;

    let run = read_workflow_run(&env.state, project_id, run_id, &env.home())
        .await
        .unwrap();
    assert_eq!(run.status, WorkflowRunStatus::Completed);
    assert!(run.done);
    assert!(!run.sends_unavailable);
    let steps: Vec<(usize, Option<&str>)> = run
        .sends
        .iter()
        .map(|s| (s.step, s.label.as_deref()))
        .collect();
    assert_eq!(steps, [(0, Some("First round")), (2, Some("Second round"))]);
    let completed = |text: &str| Some(text.to_owned());
    assert_eq!(
        reply_texts(&run.sends[0]),
        [
            (
                "r1".to_owned(),
                SendStatus::Completed,
                completed("R1 FIRST")
            ),
            (
                "r2".to_owned(),
                SendStatus::Completed,
                completed("R2 FIRST")
            ),
        ],
        "the first round, which each reviewer's latest answer no longer holds"
    );
    assert_eq!(
        reply_texts(&run.sends[1]),
        [
            (
                "r1".to_owned(),
                SendStatus::Completed,
                completed("R1 SECOND")
            ),
            (
                "r2".to_owned(),
                SendStatus::Completed,
                completed("R2 SECOND")
            ),
        ]
    );
    assert!(
        run.sends
            .iter()
            .all(|s| s.done && s.source == WaitSource::Live)
    );
}

#[tokio::test]
async fn read_workflow_run_on_a_running_run_reports_its_recipient_running_from_the_ledger() {
    let (env, project_id, agents) = workflow_env().await;
    let gate = Arc::new(Notify::new());
    env.adapter
        .push_for(agents[0], Script::parked("greeting", &gate));
    let run_id = run_workflow(
        &env.state,
        &env.home(),
        pair_request(project_id, &agents),
        &WaitBudget::default_wait(),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, agents[0]).await;

    let mut run = None;
    for _ in 0..100 {
        let read = read_workflow_run(&env.state, project_id, run_id, &env.home())
            .await
            .unwrap();
        if read
            .sends
            .first()
            .is_some_and(|s| s.recipients[0].reply.status == SendStatus::Running)
        {
            run = Some(read);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let run = run.expect("the coder's reply reads running while its turn runs");
    assert_eq!(run.status, WorkflowRunStatus::Running);
    assert!(!run.done);
    assert_eq!(run.sends.len(), 1, "the reviewers' step has not sent yet");
    assert_eq!(
        run.sends[0].source,
        WaitSource::Live,
        "not the restart path"
    );
    assert!(!run.sends[0].done);
    assert_eq!(run.sends[0].recipients[0].reply.text, None);

    gate.notify_waiters();
    wait_workflow(
        &env.state,
        project_id,
        run_id,
        &WaitBudget::new(Some(30), false),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn read_workflow_run_reports_a_failed_steps_reply_as_failed() {
    let (env, project_id, agents) = workflow_env().await;
    env.adapter.push_for(agents[0], Script::failing("crashed"));
    let run_id = run_workflow(
        &env.state,
        &env.home(),
        pair_request(project_id, &agents),
        &WaitBudget::default_wait(),
    )
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

    let run = read_workflow_run(&env.state, project_id, run_id, &env.home())
        .await
        .unwrap();
    assert_eq!(run.status, WorkflowRunStatus::Failed);
    assert!(run.reason.is_some());
    assert_eq!(run.sends.len(), 1, "the reviewers were never sent to");
    assert_eq!(run.sends[0].recipients[0].reply.status, SendStatus::Failed);
}

#[tokio::test]
async fn read_workflow_run_after_a_restart_knows_a_held_failed_run_but_not_its_sends() {
    let (env, project_id, agents) = workflow_env().await;
    env.adapter.push_for(agents[0], Script::failing("crashed"));
    let failed = run_workflow(
        &env.state,
        &env.home(),
        pair_request(project_id, &agents),
        &WaitBudget::default_wait(),
    )
    .await
    .unwrap();
    wait_workflow(
        &env.state,
        project_id,
        failed,
        &WaitBudget::new(Some(30), false),
    )
    .await
    .unwrap();
    let env = env.restart();

    let run = read_workflow_run(&env.state, project_id, failed, &env.home())
        .await
        .unwrap();
    assert_eq!(run.status, WorkflowRunStatus::Failed);
    assert!(run.sends_unavailable, "its sends are not known, not absent");
    assert!(run.sends.is_empty());
}

#[tokio::test]
async fn read_workflow_run_after_a_restart_does_not_know_a_completed_run() {
    let (env, project_id, reviewers) = rounds_env();
    let run_id = run_rounds(&env, project_id, &reviewers).await;
    let env = env.restart();
    let err = read_workflow_run(&env.state, project_id, run_id, &env.home())
        .await
        .unwrap_err();
    assert_eq!(error_code(&err), error_code::RUN_UNKNOWN, "{err:?}");
}

#[tokio::test]
async fn read_workflow_run_under_another_project_is_run_unknown() {
    let (mut env, project_id, reviewers) = rounds_env();
    let other = env.project("other");
    let run_id = run_rounds(&env, project_id, &reviewers).await;
    let err = read_workflow_run(&env.state, other, run_id, &env.home())
        .await
        .unwrap_err();
    assert_eq!(error_code(&err), error_code::RUN_UNKNOWN, "{err:?}");
}

#[tokio::test]
async fn read_workflow_run_cuts_long_replies_evenly_and_each_cut_reads_on() {
    let (env, project_id, reviewers) = rounds_env();
    let mut full = BTreeMap::new();
    for (agent, tag) in reviewers.iter().zip(["R1", "R2"]) {
        for round in ["FIRST", "SECOND"] {
            let text = format!("{tag} {round} {}", "x".repeat(30_000));
            env.adapter.push_for(*agent, Script::reply(&text));
            full.insert(format!("{tag} {round}"), text);
        }
    }
    let run_id = run_rounds(&env, project_id, &reviewers).await;
    let run = read_workflow_run(&env.state, project_id, run_id, &env.home())
        .await
        .unwrap();

    let budget = 45_000;
    let fitted = fit(
        serde_json::to_value(&run).unwrap(),
        &read_workflow_run_budget(project_id, budget),
    )
    .unwrap();
    assert!(fitted.serialized_chars() <= budget);
    assert_eq!(fitted.items_omitted, 0, "no send is dropped");
    assert_eq!(fitted.truncated.len(), 4, "every reply is cut");
    let kept: Vec<usize> = fitted.truncated.iter().map(|t| t.kept_chars).collect();
    assert!(kept.iter().all(|k| *k == kept[0]), "cut evenly: {kept:?}");
    for cut in &fitted.truncated {
        let call = cut.read_with.as_ref().unwrap();
        assert_eq!(call["tool"], "read_reply");
        let send_id: Uuid = serde_json::from_value(call["send_id"].clone()).unwrap();
        let agent_id: AgentId = serde_json::from_value(call["agent_id"].clone()).unwrap();
        let offset = usize::try_from(call["offset"].as_u64().unwrap()).unwrap();
        let page = read_reply(
            &env.state,
            project_id,
            send_id,
            agent_id,
            offset,
            ReadFrom::Auto,
            OUTPUT_PAGE_CHARS,
            &env.home(),
        )
        .await
        .unwrap();
        let tag = if agent_id == reviewers[0] { "R1" } else { "R2" };
        let round = if send_id == run.sends[0].send_id {
            "FIRST"
        } else {
            "SECOND"
        };
        let whole = &full[&format!("{tag} {round}")];
        let rest: String = whole.chars().skip(offset).collect();
        assert_eq!(page.text, rest, "the cut for {tag} {round} reads on");
    }
}

#[tokio::test]
async fn read_workflow_run_reads_a_send_it_no_longer_holds_from_disk() {
    let (env, project_id, agents) = workflow_env().await;
    let (coder, r1, r2) = (agents[0], agents[1], agents[2]);
    let coder_gate = Arc::new(Notify::new());
    env.adapter
        .push_for(coder, Script::parked("greeting", &coder_gate));
    env.adapter.push_for(r1, Script::reply("R1 REVIEW"));
    let r2_gate = Arc::new(Notify::new());
    env.adapter
        .push_for(r2, Script::parked("elsewhere", &r2_gate));
    let run_id = run_workflow(
        &env.state,
        &env.home(),
        pair_request(project_id, &agents),
        &WaitBudget::default_wait(),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, coder).await;
    // r2 is busy when the reviewers' step reaches it.
    send_now(
        env.handle(),
        env.home(),
        text_send(project_id, vec![r2], "other work"),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, r2).await;
    coder_gate.notify_waiters();
    let ended = wait_workflow(
        &env.state,
        project_id,
        run_id,
        &WaitBudget::new(Some(30), false),
    )
    .await
    .unwrap();
    assert_eq!(ended.status, WorkflowRunStatus::Failed, "{ended:?}");
    let review = ended.sends[1].send_id;
    wait_done(&env, project_id, review).await;

    let live = read_workflow_run(&env.state, project_id, run_id, &env.home())
        .await
        .unwrap();
    let r2_live = &live.sends[1].recipients[1].reply;
    assert_eq!(r2_live.status, SendStatus::Rejected, "{r2_live:?}");
    assert!(r2_live.code.is_some(), "the ledger says why");

    for send in &ended.sends {
        env.state.ledger.forget_send(send.send_id);
    }
    let read = read_workflow_run(&env.state, project_id, run_id, &env.home())
        .await
        .unwrap();
    assert!(!read.sends_unavailable, "the run itself is still held");
    assert!(read.sends.iter().all(|s| s.source == WaitSource::Journal));
    assert_eq!(
        read.sends[0].recipients[0].reply.status,
        SendStatus::Completed
    );
    assert_eq!(
        reply_texts(&read.sends[1]),
        [
            (
                "r1".to_owned(),
                SendStatus::Completed,
                Some("R1 REVIEW".to_owned())
            ),
            ("r2".to_owned(), SendStatus::NotDispatched, None),
        ],
        "a completed reply reads on from disk; the refusal's reason is not kept there"
    );
    r2_gate.notify_waiters();
}

// --- account usage on finished waits ------------------------------------------

/// Store a Claude reading at `claude_used` and a Codex one at `codex_used`
/// percent, observed `codex_age` ago, each window resetting tomorrow, as the
/// window saves them.
fn store_usage(env: &TestEnv, claude_used: f64, codex_used: u64, codex_age: chrono::Duration) {
    let resets = (Utc::now() + chrono::Duration::days(1)).timestamp();
    let claude_at = (Utc::now() - chrono::Duration::minutes(3)).to_rfc3339();
    let codex_at = (Utc::now() - codex_age).to_rfc3339();
    *lock(&env.state.harness_usage) = crate::harness_usage::HarnessUsage {
        harnesses: [
            (
                "claude_code".to_owned(),
                serde_json::json!({
                    "observed_at": claude_at,
                    "payload": {},
                    "windows": { "five_hour": {
                        "window": { "utilization": claude_used, "resetsAt": resets },
                        "observed_at": claude_at,
                    } },
                }),
            ),
            (
                "codex".to_owned(),
                serde_json::json!({
                    "observed_at": codex_at,
                    "payload": { "ordinaryUsageAllowed": true, "rateLimitsByLimitId": {
                        "codex": { "normalModelSlug": null, "planType": "pro",
                                   "credits": { "balance": "12" },
                                   "primary": { "usedPercent": codex_used, "resetsAt": resets,
                                                "windowDurationMins": 300 },
                                   "secondary": null } } },
                }),
            ),
        ]
        .into_iter()
        .collect(),
    };
}

/// Make the Codex account read count its calls and answer `used` percent.
fn counting_codex_read(env: &TestEnv, used: u64) -> Arc<std::sync::atomic::AtomicUsize> {
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let resets = (Utc::now() + chrono::Duration::days(1)).timestamp();
    env.state.codex_usage_reader.set_read(Arc::new({
        let reads = Arc::clone(&reads);
        move |_handle| {
            reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let usage: switchboard_harness::CodexAccountUsage =
                serde_json::from_value(serde_json::json!({
                    "ordinaryUsageAllowed": true,
                    "rateLimitsByLimitId": { "codex": {
                        "normalModelSlug": null, "planType": "pro",
                        "primary": { "usedPercent": used, "resetsAt": resets,
                                     "windowDurationMins": 300 },
                        "secondary": null } }
                }))
                .unwrap();
            Box::pin(async move { Some(usage) })
        }
    }));
    reads
}

/// The used fraction of one harness's first window in a wait's usage.
fn used(usage: Option<&Vec<super::usage::HarnessUsageReport>>, harness: &str) -> f64 {
    let report = usage
        .expect("a finished wait carries usage")
        .iter()
        .find(|r| r.harness == harness)
        .unwrap();
    report.windows[0].used_fraction
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Every wait, unfinished then finished, in one run.
async fn finished_waits_carry_the_stored_usage_and_never_read_the_codex_account() {
    let (env, project_id, agents) = workflow_env().await;
    let coder = agents[0];
    store_usage(&env, 0.25, 10, chrono::Duration::minutes(10));
    let reads = counting_codex_read(&env, 77);
    // Agent access's own Codex reading, newer than the window's stored one.
    super::usage::get_usage(
        env.handle(),
        Duration::from_secs(5),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1);
    // From here on every reading is past the reader's one-minute freshness
    // rule, so a wait that reached the reader would read the account.
    env.state
        .codex_usage_reader
        .set_clock(Arc::new(|| Utc::now() + chrono::Duration::minutes(2)));

    // Unfinished: no usage on any of the four waits.
    let gate = Arc::new(Notify::new());
    env.adapter
        .push_for(coder, Script::parked("working", &gate));
    let send_id = send_now(
        env.handle(),
        env.home(),
        text_send(project_id, vec![coder], "work"),
    )
    .await
    .unwrap()
    .send_id;
    await_turn_running(&env.state, coder).await;
    let now_only = WaitBudget::new(Some(0), false);
    let pending = wait_send(&env.state, project_id, send_id, &now_only, &env.home())
        .await
        .unwrap();
    assert!(!pending.done && pending.usage.is_none());
    let pending_any = wait_any_send(
        &env.state,
        project_id,
        vec![send_id],
        &now_only,
        &env.home(),
    )
    .await
    .unwrap();
    assert!(!pending_any.any_done && pending_any.usage.is_none());
    let busy = wait_agent(&env.state, project_id, coder, &now_only, &env.home())
        .await
        .unwrap();
    assert!(busy.status == AgentWaitStatus::Running && busy.usage.is_none());
    gate.notify_waiters();

    // Finished: the stored reading, with agent access's newer Codex one.
    let done = wait_done(&env, project_id, send_id).await;
    assert!((used(done.usage.as_ref(), "claude_code") - 0.25).abs() < f64::EPSILON);
    assert!(
        (used(done.usage.as_ref(), "codex") - 0.77).abs() < f64::EPSILON,
        "agent access's own reading is newer than the stored one"
    );
    let done_json = serde_json::to_value(&done).unwrap();
    let codex_window = &done_json["usage"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["harness"] == "codex")
        .unwrap()["windows"][0];
    assert!(
        codex_window.get("credits").is_none() && codex_window.get("duration_minutes").is_none(),
        "the compact form: {codex_window}"
    );
    let done_any = wait_any_send(
        &env.state,
        project_id,
        vec![send_id],
        &WaitBudget::new(Some(30), false),
        &env.home(),
    )
    .await
    .unwrap();
    assert!(done_any.any_done);
    assert!((used(done_any.usage.as_ref(), "codex") - 0.77).abs() < f64::EPSILON);
    let idle = wait_agent(
        &env.state,
        project_id,
        coder,
        &WaitBudget::new(Some(30), false),
        &env.home(),
    )
    .await
    .unwrap();
    assert_eq!(idle.status, AgentWaitStatus::Idle);
    assert!((used(idle.usage.as_ref(), "claude_code") - 0.25).abs() < f64::EPSILON);

    // A reading the window saves later than agent access's own is the one used.
    store_usage(&env, 0.5, 50, chrono::Duration::zero());
    let workflow_gate = Arc::new(Notify::new());
    env.adapter
        .push_for(coder, Script::parked("greeting", &workflow_gate));
    let run_id = run_workflow(
        &env.state,
        &env.home(),
        pair_request(project_id, &agents),
        &WaitBudget::default_wait(),
    )
    .await
    .unwrap();
    await_turn_running(&env.state, coder).await;
    let running = wait_workflow(&env.state, project_id, run_id, &now_only)
        .await
        .unwrap();
    assert!(!running.done && running.usage.is_none());
    workflow_gate.notify_waiters();
    let ended = wait_workflow(
        &env.state,
        project_id,
        run_id,
        &WaitBudget::new(Some(30), false),
    )
    .await
    .unwrap();
    assert!(ended.done);
    assert!(
        (used(ended.usage.as_ref(), "codex") - 0.5).abs() < f64::EPSILON,
        "the window's newer reading wins"
    );

    assert_eq!(
        reads.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "no wait read the Codex account"
    );
}

#[tokio::test]
async fn wait_any_send_carries_usage_once_at_its_top_level() {
    let (env, project_id, agents) = project_with_agents(&["a", "b"]);
    store_usage(&env, 0.25, 10, chrono::Duration::minutes(10));
    let first = answered(&env, project_id, agents[0], "one").await;
    let second = answered(&env, project_id, agents[1], "two").await;
    let reply = wait_any_send(
        &env.state,
        project_id,
        vec![first, second],
        &WaitBudget::new(Some(30), false),
        &env.home(),
    )
    .await
    .unwrap();
    let json = serde_json::to_value(&reply).unwrap();
    assert!(json["usage"].is_array(), "{json}");
    for send in json["sends"].as_array().unwrap() {
        assert!(send.get("usage").is_none(), "not repeated per send: {send}");
    }
}

#[tokio::test]
async fn the_first_finished_waits_carry_a_stale_codex_reading_without_reading_the_account() {
    let (env, project_id, agents) = workflow_env().await;
    let coder = agents[0];
    // Nothing fresh: the window's Codex reading is ten minutes old and agent
    // access has read nothing, as for the first wait after the app starts.
    store_usage(&env, 0.25, 10, chrono::Duration::minutes(10));
    let reads = counting_codex_read(&env, 77);
    let stale = |usage: Option<&Vec<super::usage::HarnessUsageReport>>| {
        (used(usage, "codex") - 0.10).abs() < f64::EPSILON
    };

    let send_id = answered(&env, project_id, coder, "work").await;
    let done = wait_done(&env, project_id, send_id).await;
    assert!(stale(done.usage.as_ref()), "{:?}", done.usage);
    let done_any = wait_any_send(
        &env.state,
        project_id,
        vec![send_id],
        &WaitBudget::new(Some(30), false),
        &env.home(),
    )
    .await
    .unwrap();
    assert!(stale(done_any.usage.as_ref()));
    let idle = wait_agent(
        &env.state,
        project_id,
        coder,
        &WaitBudget::new(Some(30), false),
        &env.home(),
    )
    .await
    .unwrap();
    assert!(stale(idle.usage.as_ref()));
    let run_id = run_workflow(
        &env.state,
        &env.home(),
        pair_request(project_id, &agents),
        &WaitBudget::default_wait(),
    )
    .await
    .unwrap();
    let ended = wait_workflow(
        &env.state,
        project_id,
        run_id,
        &WaitBudget::new(Some(30), false),
    )
    .await
    .unwrap();
    assert!(ended.done && stale(ended.usage.as_ref()));

    assert_eq!(
        reads.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "with nothing fresh, no wait read the Codex account"
    );
}
