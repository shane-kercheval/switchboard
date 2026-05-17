//! Live tool-event coverage for both adapters.
//!
//! Each test prompts the real CLI to use a file-read / shell tool and asserts
//! that `ToolStarted` is followed by a matching `ToolCompleted` (correlated by
//! `tool_use_id`, `is_error: false`). Tool *names* differ across harnesses
//! (Claude builtins like `Read`; Codex's normalized `command_execution`); the
//! assertions key off `tool_use_id` correlation, not name strings, so a CLI
//! rename of an underlying tool doesn't make these tests brittle.
//!
//! Run with: `make test-live`. Both tests are `#[ignore]`-gated.

use futures::StreamExt;
use switchboard_core::{AgentRecord, HarnessKind};
use switchboard_harness::{
    AdapterEvent, ClaudeCodeAdapter, CodexAdapter, DispatchOptions, HarnessAdapter, TurnOutcome,
};
use uuid::Uuid;

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

/// Look for any `ToolStarted` and its matching `ToolCompleted` in `events`.
/// Returns the matching pair, or `None` if no started/completed pair shares a
/// `tool_use_id`. Keys off the id so the harness's normalized tool *name*
/// can drift (e.g., Codex renaming `command_execution`) without invalidating
/// the live-test contract.
fn paired_tool_call(events: &[AdapterEvent]) -> Option<(AdapterEvent, AdapterEvent)> {
    for ev in events {
        if let AdapterEvent::ToolStarted { tool_use_id, .. } = ev {
            let id = tool_use_id.clone();
            if let Some(completed) = events.iter().find(|e| {
                matches!(e, AdapterEvent::ToolCompleted { tool_use_id, .. } if tool_use_id == &id)
            }) {
                return Some((ev.clone(), completed.clone()));
            }
        }
    }
    None
}

#[tokio::test]
#[ignore = "requires claude installed — run with: make test-live"]
async fn live_claude_emits_tool_started_and_tool_completed_for_file_read() {
    // Stage a known file under a tempdir and prompt Claude to read it.
    // Asserts the full tool-event lifecycle:
    //   - ToolStarted with a stable tool_use_id
    //   - matching ToolCompleted with the same tool_use_id
    //   - is_error: false
    //   - terminal TurnEnd(Completed)
    // The token in the file is a unique sentinel so any reasonable model
    // response that quotes file content will include it.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let token = "SWITCHBOARD_TOOL_LIVE_F2A98C";
    std::fs::write(tmp.path().join("MARKER.txt"), token).expect("write marker");

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

    let (started, completed) = paired_tool_call(&events).unwrap_or_else(|| {
        panic!("expected a ToolStarted/ToolCompleted pair; got events: {events:?}")
    });

    match (&started, &completed) {
        (
            AdapterEvent::ToolStarted {
                tool_use_id: start_id,
                name,
                ..
            },
            AdapterEvent::ToolCompleted {
                tool_use_id: end_id,
                is_error,
                ..
            },
        ) => {
            assert_eq!(start_id, end_id, "tool_use_id must correlate");
            assert!(
                !*is_error,
                "successful file read must report is_error: false"
            );
            assert!(!name.is_empty(), "ToolStarted.name must be non-empty");
        }
        _ => unreachable!(),
    }

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
    // Codex's most reliably-triggered tool is `command_execution` (shell).
    // Plant a known file and prompt Codex to read it via its shell tool;
    // the adapter normalizes the underlying CLI item type into a
    // ToolStarted/ToolCompleted pair. Assertions key off tool_use_id
    // correlation so renaming the internal item type doesn't break this
    // test — a true regression would be the pair vanishing entirely.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let token = "SWITCHBOARD_TOOL_LIVE_C0D3X1";
    std::fs::write(tmp.path().join("MARKER.txt"), token).expect("write marker");

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

    let (started, completed) = paired_tool_call(&events).unwrap_or_else(|| {
        panic!("expected a ToolStarted/ToolCompleted pair; got events: {events:?}")
    });

    match (&started, &completed) {
        (
            AdapterEvent::ToolStarted {
                tool_use_id: start_id,
                name,
                ..
            },
            AdapterEvent::ToolCompleted {
                tool_use_id: end_id,
                is_error,
                ..
            },
        ) => {
            assert_eq!(start_id, end_id, "tool_use_id must correlate");
            assert!(
                !*is_error,
                "successful `cat MARKER.txt` must report is_error: false"
            );
            assert!(!name.is_empty(), "ToolStarted.name must be non-empty");
        }
        _ => unreachable!(),
    }

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
