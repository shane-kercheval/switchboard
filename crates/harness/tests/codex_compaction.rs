//! Process-level tests for `CodexAdapter::compact`: drive it against the
//! `fake_codex` fixture binary in `app-server` mode and assert on the event
//! stream, the requests it writes, and the teardown.
//!
//! The success stream is a sanitized capture of a real compaction (codex-cli
//! 0.162.1, 2026-10-12). The other streams are built here from the same
//! messages, because the orderings and failures they cover can't be produced on
//! demand from the real CLI. The live tests (`live_codex_compact_*` in
//! `live.rs`) prove the real CLI still sends what these assume.

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::StreamExt;
use switchboard_core::{AgentRecord, HarnessKind, SessionLocator};
use switchboard_harness::codex::parser::CODEX_AUTH_MESSAGE;
use switchboard_harness::{
    AdapterEvent, CodexAdapter, DispatchError, DispatchOptions, FailureKind, HarnessAdapter,
    TurnOutcome,
};
use uuid::Uuid;

#[cfg(unix)]
mod common;
#[cfg(unix)]
use common::{assert_group_reaped, wait_for_pgid};
#[cfg(unix)]
use tokio_util::sync::CancellationToken;

const FAKE_CODEX: &str = env!("CARGO_BIN_EXE_fake_codex");
const RECORDED: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/codex/app-server-compact.jsonl"
);

/// The thread and compaction turn in the recorded stream.
const THREAD: &str = "01a13353-52b1-7fd1-b3e5-6ed192aef91d";
const TURN: &str = "01a13353-903c-7e10-b66f-9116739d6a74";

/// The bound used by the tests that want the startup to time out. Long enough
/// for the fixture binary to start and reach its directives on a cold run (the
/// quota read's tests saw two seconds fail there), short enough to keep the
/// file quick.
const WANTS_TIMEOUT: Duration = Duration::from_secs(5);

/// A scratch home holding the agent's rollout, a working directory, and a
/// `codex` shim that replays one fixture.
struct Staged {
    _root: tempfile::TempDir,
    home: PathBuf,
    cwd: PathBuf,
    shim: PathBuf,
}

impl Staged {
    /// `compact` refuses an agent with no rollout, so one is staged where the
    /// adapter looks for it. Its contents don't matter: the fake answers from
    /// the fixture.
    fn new(fixture: &str) -> Self {
        let root = tempfile::TempDir::new().unwrap();
        let home = root.path().join("home");
        let sessions = home.join(".codex/sessions/2026/10/12");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(
            sessions.join(format!("rollout-2026-10-12T22-34-21-{THREAD}.jsonl")),
            "{}\n",
        )
        .unwrap();
        let cwd = root.path().join("work");
        std::fs::create_dir_all(&cwd).unwrap();

        // `compact` runs `<binary> app-server` with no fixture argument, as the
        // real CLI takes none, so the shim appends one. `exec` keeps the pid and
        // process group the spawn assigned, which the teardown checks rely on.
        let fixture_path = root.path().join("fixture.jsonl");
        std::fs::write(&fixture_path, fixture).unwrap();
        let shim = root.path().join("codex");
        std::fs::write(
            &shim,
            format!(
                "#!/bin/sh\nexec {FAKE_CODEX} \"$@\" {}\n",
                fixture_path.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self {
            _root: root,
            home,
            cwd,
            shim,
        }
    }

    fn adapter(&self) -> CodexAdapter {
        CodexAdapter::with_binary_and_home(&self.shim, &self.home)
    }

    async fn compact(&self) -> Vec<AdapterEvent> {
        self.compact_with(self.adapter()).await
    }

    async fn compact_with(&self, adapter: CodexAdapter) -> Vec<AdapterEvent> {
        let Ok(stream) = adapter
            .compact(
                &agent(),
                &self.cwd,
                Uuid::now_v7(),
                DispatchOptions::default(),
            )
            .await
        else {
            panic!("compact should start");
        };
        stream.collect().await
    }
}

fn agent() -> AgentRecord {
    AgentRecord {
        model: Some("gpt-6-luna".to_owned()),
        effort: Some("medium".to_owned()),
        model_choices: Vec::new(),
        effort_choices: Vec::new(),
        forked_from_session: None,
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "test-codex".to_owned(),
        harness: HarnessKind::Codex,
        session_locator: Some(SessionLocator::Codex {
            thread_id: THREAD.to_owned(),
            partition_date: chrono::NaiveDate::from_ymd_opt(2026, 10, 12).unwrap(),
        }),
        created_at: chrono::Utc::now(),
    }
}

/// The stream's single terminal, asserted to be its last event.
fn terminal(events: &[AdapterEvent]) -> &TurnOutcome {
    let ends: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .collect();
    assert_eq!(ends.len(), 1, "exactly one terminal; events: {events:?}");
    let Some(AdapterEvent::TurnEnd { outcome, .. }) = events.last() else {
        panic!("the terminal must be the last event; events: {events:?}");
    };
    outcome
}

fn failure(events: &[AdapterEvent]) -> (FailureKind, String) {
    match terminal(events) {
        TurnOutcome::Failed { kind, message } => (kind.clone(), message.clone()),
        other => panic!("expected a failure, got {other:?}"),
    }
}

// --- Messages, in the shapes the recorded stream carries ---

fn init_ok() -> String {
    r#"{"id":0,"result":{"userAgent":"switchboard/0.162.1","codexHome":"/home/example/.codex","platformFamily":"unix","platformOs":"macos"}}"#.to_owned()
}

fn resume_ok() -> String {
    serde_json::json!({"id": 1, "result": {"thread": {"id": THREAD, "turns": []}}}).to_string()
}

fn compact_ok() -> String {
    r#"{"id":2,"result":{}}"#.to_owned()
}

fn rpc_error(id: i64, message: &str) -> String {
    serde_json::json!({"id": id, "error": {"code": -32600, "message": message}}).to_string()
}

fn notification(method: &str, params: &serde_json::Value) -> String {
    serde_json::json!({"method": method, "params": params, "emittedAtMs": 1_791_862_476_866_i64})
        .to_string()
}

fn turn(status: &str, error: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "id": TURN, "rootTurnId": TURN, "items": [], "itemsView": "notLoaded",
        "status": status, "error": error, "startedAt": 1_791_862_476,
        "completedAt": null, "durationMs": null,
    })
}

fn turn_started() -> String {
    notification(
        "turn/started",
        &serde_json::json!({"threadId": THREAD, "turn": turn("inProgress", &serde_json::Value::Null)}),
    )
}

fn compaction_item(method: &str) -> String {
    notification(
        method,
        &serde_json::json!({
            "item": {"type": "contextCompaction", "id": "01a13353-921f-79d0-940c-1d077f9de30c"},
            "threadId": THREAD, "turnId": TURN,
        }),
    )
}

fn turn_completed(status: &str, error: &serde_json::Value) -> String {
    notification(
        "turn/completed",
        &serde_json::json!({"threadId": THREAD, "turn": turn(status, error)}),
    )
}

fn token_usage() -> String {
    notification(
        "thread/tokenUsage/updated",
        &serde_json::json!({
            "threadId": THREAD, "turnId": TURN,
            "tokenUsage": {"last": {"totalTokens": 9686}, "modelContextWindow": 258_400},
        }),
    )
}

/// A compaction whose turn ends with `status` and `error`, every reply
/// successful.
fn ended(status: &str, error: &serde_json::Value) -> String {
    [
        init_ok(),
        resume_ok(),
        compact_ok(),
        turn_started(),
        compaction_item("item/started"),
        turn_completed(status, error),
    ]
    .join("\n")
}

fn lines(lines: &[String]) -> String {
    lines.join("\n")
}

// --- Success ---

#[tokio::test]
async fn the_recorded_compaction_completes_with_no_usage_after_liveness() {
    let staged = Staged::new(&std::fs::read_to_string(RECORDED).unwrap());

    let events = staged.compact().await;

    assert!(
        matches!(terminal(&events), TurnOutcome::Completed),
        "events: {events:?}"
    );
    let Some(AdapterEvent::TurnEnd {
        usage,
        model,
        effort,
        stable_message_id,
        first_message_id,
        context_window_source,
        spend,
        ..
    }) = events.last()
    else {
        unreachable!("checked by `terminal`");
    };
    // The compaction's own token figure leaves out Codex's fixed instructions
    // and tools, so the context bar must not see it.
    assert!(usage.is_none());
    assert!(context_window_source.is_none());
    assert!(spend.is_none());
    assert!(model.is_none() && effort.is_none());
    assert!(stable_message_id.is_none() && first_message_id.is_none());
    assert!(
        events[..events.len() - 1]
            .iter()
            .all(|e| matches!(e, AdapterEvent::Liveness { .. })),
        "only liveness before the terminal; events: {events:?}"
    );
    assert!(
        events.len() > 1,
        "the running compaction must show activity before it ends"
    );
}

#[tokio::test]
async fn the_compaction_turn_starting_before_its_reply_still_completes() {
    let staged = Staged::new(&lines(&[
        init_ok(),
        resume_ok(),
        turn_started(),
        compact_ok(),
        compaction_item("item/started"),
        compaction_item("item/completed"),
        turn_completed("completed", &serde_json::Value::Null),
    ]));

    assert!(matches!(
        terminal(&staged.compact().await),
        TurnOutcome::Completed
    ));
}

#[tokio::test]
async fn the_whole_compaction_turn_before_its_reply_completes_once_the_reply_arrives() {
    // Codex queues the compaction before it answers the request, so the turn
    // can finish first.
    let staged = Staged::new(&lines(&[
        init_ok(),
        resume_ok(),
        turn_started(),
        compaction_item("item/started"),
        compaction_item("item/completed"),
        turn_completed("completed", &serde_json::Value::Null),
        compact_ok(),
    ]));

    assert!(matches!(
        terminal(&staged.compact().await),
        TurnOutcome::Completed
    ));
}

#[tokio::test]
async fn a_finished_turn_still_needs_the_reply_to_its_request() {
    // The counterpart to the test above: success needs the reply too, so a
    // reply that never comes runs out the startup bound rather than reading
    // as a success.
    let staged = Staged::new(&lines(&[
        init_ok(),
        resume_ok(),
        turn_started(),
        compaction_item("item/started"),
        compaction_item("item/completed"),
        turn_completed("completed", &serde_json::Value::Null),
        "// hang".to_owned(),
    ]));

    let events = staged
        .compact_with(
            staged
                .adapter()
                .with_compaction_startup_bound(WANTS_TIMEOUT),
        )
        .await;

    // The liveness proves the fixture ran before the bound expired, so this
    // is the missing reply and not a slow start.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AdapterEvent::Liveness { .. }))
    );
    let (kind, message) = failure(&events);
    assert_eq!(kind, FailureKind::AdapterFailure);
    assert!(message.contains("did not answer"), "{message}");
}

#[tokio::test]
async fn interleaved_notifications_and_server_requests_dont_shift_the_reply_match() {
    // A request from the server carries an `id`, here one of ours. Taking it
    // for the reply to `thread/compact/start` would end the read on a
    // message with no result.
    let staged = Staged::new(&lines(&[
        notification(
            "remoteControl/status/changed",
            &serde_json::json!({"status": "disabled"}),
        ),
        init_ok(),
        notification(
            "mcpServer/startupStatus/updated",
            &serde_json::json!({"threadId": THREAD, "name": "example", "status": "starting"}),
        ),
        r#"{"id":2,"method":"item/tool/requestUserInput","params":{}}"#.to_owned(),
        resume_ok(),
        token_usage(),
        notification(
            "thread/status/changed",
            &serde_json::json!({"threadId": THREAD, "status": {"type": "active"}}),
        ),
        compact_ok(),
        turn_started(),
        compaction_item("item/started"),
        token_usage(),
        compaction_item("item/completed"),
        turn_completed("completed", &serde_json::Value::Null),
    ]));

    assert!(matches!(
        terminal(&staged.compact().await),
        TurnOutcome::Completed
    ));
}

#[tokio::test]
async fn the_requests_resume_the_thread_with_the_agents_settings_then_compact_it() {
    let recorded = std::fs::read_to_string(RECORDED).unwrap();
    let root = tempfile::TempDir::new().unwrap();
    let requests_path = root.path().join("requests.jsonl");
    let staged = Staged::new(&format!(
        "// stdin_lines_to:3:{}\n{recorded}",
        requests_path.display()
    ));

    assert!(matches!(
        terminal(&staged.compact().await),
        TurnOutcome::Completed
    ));

    let requests: Vec<serde_json::Value> = std::fs::read_to_string(&requests_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let methods: Vec<&str> = requests
        .iter()
        .map(|r| r["method"].as_str().unwrap())
        .collect();
    assert_eq!(
        methods,
        ["initialize", "thread/resume", "thread/compact/start"]
    );
    assert_eq!(requests[0]["params"]["clientInfo"]["name"], "switchboard");

    let resume = &requests[1]["params"];
    assert_eq!(resume["threadId"], THREAD);
    assert_eq!(resume["cwd"], staged.cwd.to_str().unwrap());
    assert_eq!(resume["excludeTurns"], true);
    assert_eq!(resume["model"], "gpt-6-luna");
    assert_eq!(resume["config"]["model_reasoning_effort"], "medium");
    // What `--dangerously-bypass-approvals-and-sandbox` gives `codex exec`, so
    // the compaction turn records the agent's real permissions.
    assert_eq!(resume["sandbox"], "danger-full-access");
    assert_eq!(resume["approvalPolicy"], "never");

    assert_eq!(requests[2]["params"]["threadId"], THREAD);
}

#[tokio::test]
async fn an_agent_with_no_model_or_effort_sends_neither() {
    // As `codex exec` gets no `-m` or `-c` for them, so Codex picks.
    let root = tempfile::TempDir::new().unwrap();
    let requests_path = root.path().join("requests.jsonl");
    let staged = Staged::new(&format!(
        "// stdin_lines_to:3:{}\n{}",
        requests_path.display(),
        ended("completed", &serde_json::Value::Null)
    ));
    let agent = AgentRecord {
        model: None,
        effort: None,
        ..agent()
    };

    let Ok(stream) = staged
        .adapter()
        .compact(
            &agent,
            &staged.cwd,
            Uuid::now_v7(),
            DispatchOptions::default(),
        )
        .await
    else {
        panic!("compact should start");
    };
    let _: Vec<AdapterEvent> = stream.collect().await;

    let resume: serde_json::Value = serde_json::from_str(
        std::fs::read_to_string(&requests_path)
            .unwrap()
            .lines()
            .nth(1)
            .unwrap(),
    )
    .unwrap();
    assert!(resume["params"].get("model").is_none());
    assert!(resume["params"].get("config").is_none());
}

// --- Codex reports a failure ---

#[tokio::test]
async fn a_failed_compaction_turn_reports_codexs_unwrapped_text() {
    let staged = Staged::new(&ended(
        "failed",
        &serde_json::json!({
            "message": r#"{"error":{"message":"stream disconnected before completion"}}"#,
            "codexErrorInfo": "other",
        }),
    ));

    let (kind, message) = failure(&staged.compact().await);

    assert_eq!(kind, FailureKind::HarnessError);
    assert_eq!(message, "stream disconnected before completion");
}

#[tokio::test]
async fn a_usage_limit_is_reported_as_a_spent_quota_with_codexs_text() {
    let text = "You've hit your usage limit. Try again at 8:00 PM.";
    let staged = Staged::new(&ended(
        "failed",
        &serde_json::json!({"message": text, "codexErrorInfo": "usageLimitExceeded"}),
    ));

    let (kind, message) = failure(&staged.compact().await);

    assert_eq!(kind, FailureKind::UsageLimit);
    assert_eq!(message, text);
}

#[tokio::test]
async fn an_unauthorized_turn_asks_the_user_to_sign_in_as_a_send_does() {
    let staged = Staged::new(&ended(
        "failed",
        &serde_json::json!({"message": "401 Unauthorized", "codexErrorInfo": "unauthorized"}),
    ));

    let (kind, message) = failure(&staged.compact().await);

    assert_eq!(kind, FailureKind::AuthFailure);
    assert_eq!(message, CODEX_AUTH_MESSAGE);
}

#[tokio::test]
async fn an_interruption_switchboard_didnt_ask_for_is_a_failure() {
    let with_text = Staged::new(&ended(
        "interrupted",
        &serde_json::json!({"message": "the server is shutting down"}),
    ));
    let without_text = Staged::new(&ended("interrupted", &serde_json::Value::Null));

    assert_eq!(
        failure(&with_text.compact().await),
        (
            FailureKind::HarnessError,
            "the server is shutting down".to_owned()
        )
    );
    assert_eq!(
        failure(&without_text.compact().await),
        (
            FailureKind::HarnessError,
            "compaction was interrupted".to_owned()
        )
    );
}

#[tokio::test]
async fn a_completed_turn_with_no_compaction_item_fails_closed() {
    let staged = Staged::new(&ended("completed", &serde_json::Value::Null));

    assert_eq!(
        failure(&staged.compact().await),
        (
            FailureKind::HarnessError,
            "compaction did not run".to_owned()
        )
    );
}

#[tokio::test]
async fn an_unknown_turn_status_is_reported_rather_than_read_as_success() {
    let staged = Staged::new(&lines(&[
        init_ok(),
        resume_ok(),
        compact_ok(),
        turn_started(),
        compaction_item("item/completed"),
        turn_completed("paused", &serde_json::Value::Null),
    ]));

    let (kind, message) = failure(&staged.compact().await);

    assert_eq!(kind, FailureKind::AdapterFailure);
    assert!(message.contains("paused"), "{message}");
}

#[tokio::test]
async fn an_unknown_thread_is_reported_in_codexs_words() {
    let staged = Staged::new(&lines(&[
        init_ok(),
        resume_ok(),
        rpc_error(2, &format!("thread not found: {THREAD}")),
    ]));

    let (kind, message) = failure(&staged.compact().await);

    assert_eq!(kind, FailureKind::HarnessError);
    assert!(message.contains("thread not found"), "{message}");
}

#[tokio::test]
async fn a_failed_resume_is_reported_not_the_compaction_it_doomed() {
    // Captured from 0.162.1 with an unknown thread id: both requests fail, and
    // the compaction's error names only the consequence.
    let staged = Staged::new(&lines(&[
        init_ok(),
        rpc_error(1, &format!("no rollout found for thread id {THREAD}")),
        rpc_error(2, &format!("thread not found: {THREAD}")),
    ]));

    let (kind, message) = failure(&staged.compact().await);

    assert_eq!(kind, FailureKind::HarnessError);
    assert_eq!(message, format!("no rollout found for thread id {THREAD}"));
}

#[tokio::test]
async fn the_earliest_failed_request_wins_whatever_order_the_replies_come_in() {
    let lineage = format!("invalid paginated history lineage for {THREAD}: missing source rollout");
    let staged = Staged::new(&lines(&[
        rpc_error(2, &format!("thread not found: {THREAD}")),
        init_ok(),
        rpc_error(1, &lineage),
    ]));

    assert_eq!(
        failure(&staged.compact().await),
        (FailureKind::HarnessError, lineage)
    );
}

#[tokio::test]
async fn a_rejected_handshake_is_an_adapter_failure() {
    // Our request is malformed against a protocol that moved, not Codex
    // declining the work.
    let staged = Staged::new(&lines(&[
        rpc_error(0, "Invalid request: missing field `clientInfo`"),
        rpc_error(1, "Not initialized"),
        rpc_error(2, "Not initialized"),
    ]));

    let (kind, message) = failure(&staged.compact().await);

    assert_eq!(kind, FailureKind::AdapterFailure);
    assert!(message.contains("clientInfo"), "{message}");
}

// --- The server goes away, hangs, or is stopped ---

#[tokio::test]
async fn a_server_that_exits_mid_compaction_is_an_adapter_failure_with_its_last_words() {
    let staged = Staged::new(&lines(&[
        init_ok(),
        resume_ok(),
        compact_ok(),
        turn_started(),
        compaction_item("item/started"),
        "// stderr:connection to the model was lost".to_owned(),
        "// exit:1".to_owned(),
    ]));

    let (kind, message) = failure(&staged.compact().await);

    assert_eq!(kind, FailureKind::AdapterFailure);
    assert!(
        message.contains("exited before the compaction finished"),
        "{message}"
    );
    assert!(
        message.contains("connection to the model was lost"),
        "{message}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn an_unanswered_startup_fails_within_the_bound_and_leaves_no_process() {
    let root = tempfile::TempDir::new().unwrap();
    let pgid_path = root.path().join("pgid");
    let staged = Staged::new(&format!(
        "// pgid_to:{}\n// spawn_child_holding_stderr\n// stderr:wedged before answering\n// hang\n",
        pgid_path.display()
    ));

    let started = std::time::Instant::now();
    let events = staged
        .compact_with(
            staged
                .adapter()
                .with_compaction_startup_bound(WANTS_TIMEOUT),
        )
        .await;
    let elapsed = started.elapsed();

    let (kind, message) = failure(&events);
    assert_eq!(kind, FailureKind::AdapterFailure);
    assert!(message.contains("did not answer"), "{message}");
    assert!(message.contains("wedged before answering"), "{message}");
    assert!(
        elapsed < WANTS_TIMEOUT + Duration::from_secs(1),
        "took {elapsed:?}"
    );
    assert_group_reaped(wait_for_pgid(&pgid_path).await).await;
}

#[cfg(unix)]
#[tokio::test]
async fn cancelling_a_running_compaction_ends_the_stream_with_no_terminal_and_no_process() {
    let root = tempfile::TempDir::new().unwrap();
    let pgid_path = root.path().join("pgid");
    let staged = Staged::new(&format!(
        "// pgid_to:{}\n// spawn_child_holding_stderr\n{}\n// hang\n",
        pgid_path.display(),
        lines(&[
            init_ok(),
            resume_ok(),
            compact_ok(),
            turn_started(),
            compaction_item("item/started"),
        ])
    ));
    let cancel = CancellationToken::new();

    let Ok(mut stream) = staged
        .adapter()
        .compact(
            &agent(),
            &staged.cwd,
            Uuid::now_v7(),
            DispatchOptions {
                cancel_token: cancel.clone(),
                ..DispatchOptions::default()
            },
        )
        .await
    else {
        panic!("compact should start");
    };
    let first = tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .expect("the compaction shows activity");
    assert!(matches!(first, Some(AdapterEvent::Liveness { .. })));
    cancel.cancel();
    let rest: Vec<AdapterEvent> = tokio::time::timeout(Duration::from_secs(10), stream.collect())
        .await
        .expect("cancelling ends the stream");

    // The dispatcher synthesizes `Cancelled` from the token, as for every
    // turn.
    assert!(
        !rest
            .iter()
            .any(|e| matches!(e, AdapterEvent::TurnEnd { .. })),
        "events after the cancel: {rest:?}"
    );
    assert_group_reaped(wait_for_pgid(&pgid_path).await).await;
}

#[cfg(unix)]
#[tokio::test]
async fn the_server_is_torn_down_after_a_successful_compaction_too() {
    // The real server answers and then waits for more requests, so success is
    // the path that would leak a process if it didn't kill.
    let root = tempfile::TempDir::new().unwrap();
    let pgid_path = root.path().join("pgid");
    let staged = Staged::new(&format!(
        "// pgid_to:{}\n// spawn_child_holding_stderr\n{}\n// hang\n",
        pgid_path.display(),
        std::fs::read_to_string(RECORDED).unwrap()
    ));

    assert!(matches!(
        terminal(&staged.compact().await),
        TurnOutcome::Completed
    ));
    assert_group_reaped(wait_for_pgid(&pgid_path).await).await;
}

// --- Refused before spawning ---

async fn refusal(agent: &AgentRecord, home: &Path) -> DispatchError {
    let adapter = CodexAdapter::with_binary_and_home(FAKE_CODEX, home);
    let result = adapter
        .compact(agent, home, Uuid::now_v7(), DispatchOptions::default())
        .await;
    // `EventStream` is not `Debug`, so unwrap the error by hand.
    let Err(err) = result else {
        panic!("compact must refuse before spawning");
    };
    err
}

#[tokio::test]
async fn compact_refuses_an_agent_with_no_session_locator() {
    let home = tempfile::TempDir::new().unwrap();
    let agent = AgentRecord {
        session_locator: None,
        ..agent()
    };

    let err = refusal(&agent, home.path()).await;

    assert!(
        matches!(err, DispatchError::InvalidAgentState(_)),
        "expected InvalidAgentState, got {err:?}"
    );
}

#[tokio::test]
async fn compact_refuses_an_agent_with_no_session_file() {
    // Codex itself would answer `thread not found`; refusing first gives the
    // same answer a Claude Code agent gets.
    let home = tempfile::TempDir::new().unwrap();

    let err = refusal(&agent(), home.path()).await;

    assert!(
        matches!(err, DispatchError::InvalidAgentState(_)),
        "expected InvalidAgentState, got {err:?}"
    );
}
