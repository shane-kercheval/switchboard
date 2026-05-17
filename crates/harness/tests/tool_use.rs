//! Live tool-event coverage for both adapters.
//!
//! Each test prompts the real CLI to use a file-read / shell tool and asserts
//! that `ToolStarted` is followed by a matching `ToolCompleted` (correlated by
//! `tool_use_id`, `is_error: false`, output contains the staged sentinel).
//! Tool *names* differ across harnesses (Claude builtins like `Read`; Codex's
//! normalized `command_execution`); the matching keys off `tool_use_id`, not
//! name strings, so a CLI rename of an underlying tool stays non-brittle.
//!
//! Run with: `make test-live`. Both tests are `#[ignore]`-gated.

use futures::StreamExt;
use switchboard_core::{AgentRecord, HarnessKind};
use switchboard_harness::{
    AdapterEvent, ClaudeCodeAdapter, CodexAdapter, DispatchOptions, HarnessAdapter, TurnOutcome,
};
use uuid::Uuid;

const CLAUDE_TOKEN: &str = "SWITCHBOARD_TOOL_LIVE_F2A98C";
const CODEX_TOKEN: &str = "SWITCHBOARD_TOOL_LIVE_C0D3X1";

fn claude_agent() -> AgentRecord {
    AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "tool-use-claude".to_owned(),
        harness: HarnessKind::ClaudeCode,
        session_id: Some(Uuid::now_v7()),
        created_at: chrono::Utc::now(),
    }
}

fn codex_agent() -> AgentRecord {
    AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "tool-use-codex".to_owned(),
        harness: HarnessKind::Codex,
        session_id: None,
        created_at: chrono::Utc::now(),
    }
}

/// Find a `ToolCompleted` whose `output` carries `sentinel` and is not an
/// error, then return it paired with the `ToolStarted` sharing its
/// `tool_use_id`. Searching the *completion* first (not the first
/// `ToolStarted`) makes the test robust against a CLI emitting preliminary
/// tools (e.g., Claude using `TodoWrite` before the real Read) — those
/// would shadow the file-reading tool if we paired off the first start.
fn tool_call_with_output<'a>(
    events: &'a [AdapterEvent],
    sentinel: &str,
) -> Option<(&'a AdapterEvent, &'a AdapterEvent)> {
    let completed = events.iter().find(|e| {
        matches!(
            e,
            AdapterEvent::ToolCompleted { output, is_error, .. }
                if !*is_error && output.contains(sentinel)
        )
    })?;
    let AdapterEvent::ToolCompleted {
        tool_use_id: completed_id,
        ..
    } = completed
    else {
        unreachable!("filter above guarantees the variant");
    };
    let started = events.iter().find(|e| {
        matches!(e, AdapterEvent::ToolStarted { tool_use_id, .. } if tool_use_id == completed_id)
    })?;
    Some((started, completed))
}

#[tokio::test]
#[ignore = "requires claude installed — run with: make test-live"]
async fn live_claude_emits_tool_started_and_tool_completed_for_file_read() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(tmp.path().join("MARKER.txt"), CLAUDE_TOKEN).expect("write marker");

    let adapter = ClaudeCodeAdapter::new();
    let agent = claude_agent();
    let turn_id = Uuid::now_v7();
    let stream = adapter
        .dispatch(
            &agent,
            tmp.path(),
            "Read the file MARKER.txt in the current directory using your Read tool. \
             Reply with only the file's contents and nothing else.",
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed with real claude");
    let events: Vec<AdapterEvent> = stream.collect().await;

    let (started, _completed) = tool_call_with_output(&events, CLAUDE_TOKEN).unwrap_or_else(|| {
        panic!(
            "expected a non-error ToolCompleted whose output contains {CLAUDE_TOKEN:?}; \
                 got events: {events:?}"
        )
    });
    let AdapterEvent::ToolStarted { name, .. } = started else {
        unreachable!();
    };
    assert!(!name.is_empty(), "ToolStarted.name must be non-empty");

    let terminal = events
        .iter()
        .find(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .expect("must observe a terminal TurnEnd");
    assert!(
        matches!(
            terminal,
            AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Completed,
                ..
            }
        ),
        "expected TurnEnd(Completed); got: {terminal:?}"
    );
}

#[tokio::test]
#[ignore = "requires codex installed — run with: make test-live"]
async fn live_codex_emits_tool_started_and_tool_completed_for_shell_command() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(tmp.path().join("MARKER.txt"), CODEX_TOKEN).expect("write marker");

    let adapter = CodexAdapter::new();
    let agent = codex_agent();
    let turn_id = Uuid::now_v7();
    let stream = adapter
        .dispatch(
            &agent,
            tmp.path(),
            "Use your shell tool to run `cat MARKER.txt` in the current directory. \
             Reply with only the file's contents and nothing else.",
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed with real codex");
    let events: Vec<AdapterEvent> = stream.collect().await;

    let (started, _completed) = tool_call_with_output(&events, CODEX_TOKEN).unwrap_or_else(|| {
        panic!(
            "expected a non-error ToolCompleted whose output contains {CODEX_TOKEN:?}; \
                 got events: {events:?}"
        )
    });
    let AdapterEvent::ToolStarted { name, .. } = started else {
        unreachable!();
    };
    assert!(!name.is_empty(), "ToolStarted.name must be non-empty");

    let terminal = events
        .iter()
        .find(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .expect("must observe a terminal TurnEnd");
    assert!(
        matches!(
            terminal,
            AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Completed,
                ..
            }
        ),
        "expected TurnEnd(Completed); got: {terminal:?}"
    );
}
