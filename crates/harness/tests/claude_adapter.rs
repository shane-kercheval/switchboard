use std::path::Path;

use futures::StreamExt;
use switchboard_core::{AgentRecord, HarnessKind};
use switchboard_harness::{
    AdapterEvent, ClaudeCodeAdapter, ContentKind, DispatchError, DispatchOptions, FailureKind,
    HarnessAdapter, ToolKind, TurnOutcome,
};
use uuid::Uuid;

#[cfg(unix)]
use nix::unistd::{Pid, getpgid};
#[cfg(unix)]
use std::time::Duration;
#[cfg(unix)]
use tokio_util::sync::CancellationToken;

const FAKE_CLAUDE: &str = env!("CARGO_BIN_EXE_fake_claude");
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/claude");

fn fixture(name: &str) -> String {
    format!("{FIXTURES}/{name}.jsonl")
}

fn fake_agent() -> AgentRecord {
    AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "test-agent".to_owned(),
        harness: HarnessKind::ClaudeCode,
        session_id: Some(Uuid::now_v7()),
        created_at: chrono::Utc::now(),
    }
}

fn adapter() -> ClaudeCodeAdapter {
    ClaudeCodeAdapter::with_binary_path(FAKE_CLAUDE)
}

async fn collect_events(
    adapter: &ClaudeCodeAdapter,
    agent: &AgentRecord,
    fixture_path: &str,
) -> Vec<AdapterEvent> {
    let turn_id = Uuid::now_v7();
    let stream = adapter
        .dispatch(
            agent,
            Path::new("/tmp"),
            fixture_path,
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed");
    stream.collect().await
}

#[tokio::test]
async fn text_only_fixture_yields_chunks_then_completed() {
    let agent = fake_agent();
    let events = collect_events(&adapter(), &agent, &fixture("text-only")).await;

    let chunks: Vec<&str> = events
        .iter()
        .filter_map(|e| {
            if let AdapterEvent::ContentChunk { text, .. } = e {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect();
    assert!(!chunks.is_empty(), "expected at least one ContentChunk");
    let joined = chunks.join("");
    assert!(
        joined.contains('4'),
        "expected '4' in joined text, got: {joined:?}"
    );

    // Exactly one terminal event, and it must be Completed.
    let terminals: Vec<&AdapterEvent> = events
        .iter()
        .filter(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .collect();
    assert_eq!(terminals.len(), 1, "expected exactly one TurnEnd");
    assert!(
        matches!(
            terminals[0],
            AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Completed,
                ..
            }
        ),
        "expected TurnEnd(Completed), got: {terminals:?}"
    );
}

#[tokio::test]
async fn text_only_no_double_emit_from_assistant_message() {
    // The terminal `assistant` message carries the same text as the deltas.
    // The parser must emit text ONLY from text_delta events, never from the
    // assistant-message wrapper — otherwise the text would appear twice.
    let agent = fake_agent();
    let events = collect_events(&adapter(), &agent, &fixture("text-only")).await;

    let chunk_text: String = events
        .iter()
        .filter_map(|e| {
            if let AdapterEvent::ContentChunk { text, .. } = e {
                Some(text.clone())
            } else {
                None
            }
        })
        .collect();

    // The fixture has 5 deltas that join to "Two plus two equals 4."
    // If double-emit were happening, we'd see that text repeated.
    let expected = "Two plus two equals 4.";
    assert_eq!(
        chunk_text, expected,
        "ContentChunk text should match delta stream exactly (no double-emit)"
    );
}

#[tokio::test]
async fn tool_use_fixture_yields_text_chunk_and_completed() {
    // The tool-use fixture's only text content comes from a single
    // `text_delta` ("Done."); tool input_json_delta events at the parser
    // layer are skipped. ToolStarted/ToolCompleted come from the
    // assistant/user envelopes — asserted in a dedicated test below.
    let agent = fake_agent();
    let events = collect_events(&adapter(), &agent, &fixture("tool-use")).await;

    let chunks: Vec<&str> = events
        .iter()
        .filter_map(|e| {
            if let AdapterEvent::ContentChunk { text, .. } = e {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(chunks, vec!["Done."], "expected only the final text chunk");

    let terminals: Vec<&AdapterEvent> = events
        .iter()
        .filter(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .collect();
    assert_eq!(terminals.len(), 1);
    assert!(matches!(
        terminals[0],
        AdapterEvent::TurnEnd {
            outcome: TurnOutcome::Completed,
            ..
        }
    ));
}

#[tokio::test]
async fn failed_turn_fixture_yields_harness_error() {
    let agent = fake_agent();
    let events = collect_events(&adapter(), &agent, &fixture("failed-turn")).await;

    let terminals: Vec<&AdapterEvent> = events
        .iter()
        .filter(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .collect();
    assert_eq!(terminals.len(), 1);
    assert!(
        matches!(
            terminals[0],
            AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Failed {
                    kind: FailureKind::HarnessError,
                    ..
                },
                ..
            }
        ),
        "expected TurnEnd(Failed(HarnessError))"
    );
}

#[tokio::test]
async fn truncated_stream_synthesizes_adapter_failure() {
    let agent = fake_agent();
    let events = collect_events(&adapter(), &agent, &fixture("truncated")).await;

    let terminals: Vec<&AdapterEvent> = events
        .iter()
        .filter(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .collect();
    assert_eq!(
        terminals.len(),
        1,
        "adapter must synthesize exactly one TurnEnd"
    );
    assert!(
        matches!(
            terminals[0],
            AdapterEvent::TurnEnd {
                outcome: TurnOutcome::Failed {
                    kind: FailureKind::AdapterFailure,
                    ..
                },
                ..
            }
        ),
        "expected TurnEnd(Failed(AdapterFailure)) for truncated stream"
    );
}

#[tokio::test]
async fn malformed_json_synthesizes_adapter_failure() {
    let agent = fake_agent();
    let events = collect_events(&adapter(), &agent, &fixture("malformed-json")).await;

    let terminals: Vec<&AdapterEvent> = events
        .iter()
        .filter(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .collect();
    assert_eq!(terminals.len(), 1);
    match terminals[0] {
        AdapterEvent::TurnEnd {
            outcome:
                TurnOutcome::Failed {
                    kind: FailureKind::AdapterFailure,
                    message,
                },
            ..
        } => {
            assert!(
                message.contains("malformed JSON"),
                "message should mention malformed JSON, got: {message:?}"
            );
        }
        other => panic!("expected TurnEnd(Failed(AdapterFailure)), got: {other:?}"),
    }
}

#[tokio::test]
async fn exit1_after_completed_does_not_re_emit() {
    // After observing TurnEnd(Completed), a non-zero subprocess exit must be
    // logged but NOT cause a second TurnEnd to be emitted. Consumers see exactly
    // one terminal event: the Completed one.
    let agent = fake_agent();
    let events = collect_events(&adapter(), &agent, &fixture("exit1-after-completed")).await;

    let terminals: Vec<&AdapterEvent> = events
        .iter()
        .filter(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .collect();
    assert_eq!(
        terminals.len(),
        1,
        "must not re-emit a second TurnEnd after reconciliation"
    );
    assert!(matches!(
        terminals[0],
        AdapterEvent::TurnEnd {
            outcome: TurnOutcome::Completed,
            ..
        }
    ));
}

#[tokio::test]
async fn binary_not_found_returns_dispatch_error() {
    let bad_adapter =
        ClaudeCodeAdapter::with_binary_path("/nonexistent/path/to/claude-does-not-exist");
    let agent = fake_agent();
    let turn_id = Uuid::now_v7();
    let result = bad_adapter
        .dispatch(
            &agent,
            Path::new("/tmp"),
            "hi",
            turn_id,
            DispatchOptions::default(),
        )
        .await;
    match result {
        Err(DispatchError::BinaryNotFound) => {}
        Err(other) => panic!("expected BinaryNotFound, got: {other}"),
        Ok(_) => panic!("expected Err(BinaryNotFound), got Ok"),
    }
}

#[tokio::test]
async fn text_only_fixture_emits_session_meta_and_rate_limit_event() {
    // The text-only fixture starts with `system/init` (→ SessionMeta) and
    // includes one `rate_limit_event` line (→ RateLimitEvent). Both events
    // are first-class emissions.
    let agent = fake_agent();
    let events = collect_events(&adapter(), &agent, &fixture("text-only")).await;

    let session_meta = events
        .iter()
        .find(|e| matches!(e, AdapterEvent::SessionMeta { .. }));
    assert!(
        session_meta.is_some(),
        "expected SessionMeta from system/init line"
    );
    if let Some(AdapterEvent::SessionMeta {
        model,
        harness_version,
        ..
    }) = session_meta
    {
        assert!(!model.is_empty(), "model should be populated");
        assert!(
            !harness_version.is_empty(),
            "harness_version should be populated"
        );
    }

    let rate_limit = events
        .iter()
        .find(|e| matches!(e, AdapterEvent::RateLimitEvent { .. }));
    assert!(
        rate_limit.is_some(),
        "expected RateLimitEvent from in-stream rate_limit_event line"
    );
}

#[tokio::test]
async fn tool_use_fixture_emits_tool_started_and_tool_completed() {
    // The tool-use fixture has an `assistant` envelope carrying a `tool_use`
    // block (→ ToolStarted) and a `user` envelope carrying a `tool_result`
    // (→ ToolCompleted). Both must surface alongside the existing
    // ContentChunk ("Done.") and TurnEnd events.
    let agent = fake_agent();
    let events = collect_events(&adapter(), &agent, &fixture("tool-use")).await;

    let tool_started = events.iter().find_map(|e| {
        if let AdapterEvent::ToolStarted {
            tool_use_id,
            name,
            kind,
            ..
        } = e
        {
            Some((tool_use_id.clone(), name.clone(), *kind))
        } else {
            None
        }
    });
    let (started_id, name, kind) = tool_started.expect("expected ToolStarted from tool_use block");
    assert_eq!(name, "Bash");
    assert_eq!(kind, ToolKind::Builtin);

    let tool_completed = events.iter().find_map(|e| {
        if let AdapterEvent::ToolCompleted {
            tool_use_id,
            output,
            is_error,
            ..
        } = e
        {
            Some((tool_use_id.clone(), output.clone(), *is_error))
        } else {
            None
        }
    });
    let (completed_id, output, is_error) =
        tool_completed.expect("expected ToolCompleted from tool_result block");
    assert_eq!(
        completed_id, started_id,
        "tool_use_id should pair across started/completed"
    );
    assert!(output.contains("hello"), "got output: {output:?}");
    assert!(!is_error);
}

#[tokio::test]
async fn with_usage_fixture_populates_turn_end_usage() {
    let agent = fake_agent();
    let events = collect_events(&adapter(), &agent, &fixture("with-usage")).await;

    let turn_end = events
        .iter()
        .find(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .expect("expected a TurnEnd");
    let AdapterEvent::TurnEnd {
        outcome,
        usage: Some(usage),
        ..
    } = turn_end
    else {
        panic!("expected TurnEnd with Some(usage), got: {turn_end:?}");
    };
    assert!(matches!(outcome, TurnOutcome::Completed));
    assert!(usage.input_tokens > 0);
    assert!(usage.output_tokens > 0);
    assert!(usage.context_window.is_some_and(|c| c > 0));
    assert!(usage.total_cost_usd.is_some_and(|c| c > 0.0));
}

#[tokio::test]
async fn text_only_content_chunks_carry_text_kind() {
    // Invariant: no ContentChunk emitted carries kind: Thinking — all
    // real-fixture chunks are ContentKind::Text.
    let agent = fake_agent();
    let events = collect_events(&adapter(), &agent, &fixture("text-only")).await;

    let any_non_text = events.iter().any(|e| {
        matches!(
            e,
            AdapterEvent::ContentChunk {
                kind: ContentKind::Thinking,
                ..
            }
        )
    });
    assert!(!any_non_text, "no ContentChunk should carry kind=Thinking");
}

#[cfg(unix)]
#[tokio::test]
async fn adapter_spawns_child_in_its_own_process_group() {
    // Process-group spawn invariant — future cancellation work needs the
    // child in its own process group for a `killpg`-style teardown. This
    // test routes through `ClaudeCodeAdapter::dispatch` (NOT a direct
    // Command spawn) so it actually guards
    // `claude_code.rs:command.process_group(0)`. fake_claude honors a
    // `// pgid_to:<path>` directive, writing its own pgid; the test reads
    // the file and asserts it differs from the parent's pgid.
    let parent_pgid = getpgid(None).expect("getpgid(self) should succeed");

    let tempdir = tempfile::TempDir::new().expect("tempdir");
    let pgid_path = tempdir.path().join("child.pgid");
    let fixture_path = tempdir.path().join("pgid-fixture.jsonl");

    let base = std::fs::read_to_string(fixture("text-only")).expect("read text-only fixture");
    let fixture_with_directive = format!("// pgid_to:{}\n{base}", pgid_path.display());
    std::fs::write(&fixture_path, fixture_with_directive).expect("write pgid fixture");

    let agent = fake_agent();
    let _events = collect_events(
        &adapter(),
        &agent,
        fixture_path.to_str().expect("utf-8 path"),
    )
    .await;

    let child_pgid_raw = std::fs::read_to_string(&pgid_path)
        .expect("fake_claude should have written its pgid")
        .trim()
        .parse::<i32>()
        .expect("pgid file should contain a single decimal integer");
    let child_pgid = Pid::from_raw(child_pgid_raw);

    assert_ne!(
        child_pgid, parent_pgid,
        "child should be in its own process group (parent_pgid={parent_pgid}, child_pgid={child_pgid})"
    );
}

#[tokio::test]
async fn child_with_blocking_stdin_read_still_terminates() {
    // Property test for the existing Stdio::null() convention. The fixture
    // starts with `// read_stdin`, which makes fake_claude block on stdin to
    // EOF before streaming. The adapter MUST set Stdio::null() so the read
    // returns immediately; otherwise this test hangs.
    let agent = fake_agent();
    let events = collect_events(&adapter(), &agent, &fixture("stdin-reader")).await;

    let terminal = events
        .iter()
        .find(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
        .expect("subprocess should have terminated cleanly with a TurnEnd");
    assert!(matches!(
        terminal,
        AdapterEvent::TurnEnd {
            outcome: TurnOutcome::Completed,
            ..
        }
    ));
}

#[tokio::test]
async fn stderr_drain_no_deadlock() {
    // fake_claude always writes to stderr. This test verifies the adapter
    // completes (no deadlock) when the subprocess produces stderr output.
    // Uses the text-only fixture so we know the stream completes cleanly.
    let agent = fake_agent();
    let events = collect_events(&adapter(), &agent, &fixture("text-only")).await;
    assert!(
        !events.is_empty(),
        "should have received events despite stderr output"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AdapterEvent::TurnEnd { .. }))
    );
}

/// Poll until `path` holds a parseable pgid (the fake wrote it on spawn),
/// returning that pid. Bounded so a spawn failure surfaces as a panic, not a
/// hang.
#[cfg(unix)]
async fn wait_for_pgid(path: &Path) -> Pid {
    for _ in 0..200 {
        if let Ok(s) = std::fs::read_to_string(path)
            && let Ok(n) = s.trim().parse::<i32>()
        {
            return Pid::from_raw(n);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("fake_claude never wrote its pgid at {}", path.display());
}

/// Poll until the process group's leader is gone (`getpgid` → `ESRCH`),
/// proving the adapter's `killpg` reaped the whole tree. Bounded.
#[cfg(unix)]
async fn assert_group_reaped(leader: Pid) {
    for _ in 0..200 {
        if getpgid(Some(leader)).is_err() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("process group {leader} still alive after cancel");
}

/// Build a fixture (in `tempdir`) that writes its pgid, optionally emits the
/// first real stream line, then `// hang`s (parked read). Dispatch it with a
/// fresh token, wait for the child to spawn, fire the token, and return the
/// events the adapter emitted before the stream closed.
#[cfg(unix)]
async fn dispatch_then_cancel(emit_a_line: bool) -> Vec<AdapterEvent> {
    let tempdir = tempfile::TempDir::new().expect("tempdir");
    let pgid_path = tempdir.path().join("child.pgid");
    let fixture_path = tempdir.path().join("cancel-fixture.jsonl");

    let mut body = format!("// pgid_to:{}\n", pgid_path.display());
    if emit_a_line {
        let base = std::fs::read_to_string(fixture("text-only")).expect("read fixture");
        let first = base.lines().find(|l| !l.trim().is_empty()).expect("a line");
        body.push_str(first);
        body.push('\n');
    }
    body.push_str("// hang\n");
    std::fs::write(&fixture_path, body).expect("write cancel fixture");

    let agent = fake_agent();
    let token = CancellationToken::new();
    let options = DispatchOptions {
        cancel_token: token.clone(),
        ..Default::default()
    };
    let stream = adapter()
        .dispatch(
            &agent,
            Path::new("/tmp"),
            fixture_path.to_str().expect("utf-8 path"),
            Uuid::now_v7(),
            options,
        )
        .await
        .expect("dispatch should succeed");

    // Cancel only once the child is live and parked.
    let leader = wait_for_pgid(&pgid_path).await;
    token.cancel();

    let events: Vec<AdapterEvent> = tokio::time::timeout(Duration::from_secs(15), stream.collect())
        .await
        .expect("stream must end promptly after cancel, not hang");

    assert_group_reaped(leader).await;
    events
}

#[cfg(unix)]
#[tokio::test]
async fn cancel_mid_stream_kills_group_and_emits_no_terminal() {
    // The adapter's contract on cancel: kill the process group and end the
    // stream with NO terminal event of any kind (the dispatcher synthesizes
    // `Cancelled`). Anything terminal here would be the adapter violating its
    // own contract.
    let events = dispatch_then_cancel(/* emit_a_line */ true).await;
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AdapterEvent::TurnEnd { .. })),
        "adapter must emit no terminal event on cancel; got: {events:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn cancel_pre_first_output_kills_group_and_emits_no_terminal() {
    // Cancellation before the harness produced any output: the read is parked,
    // and `select!` must still observe the token. No events at all, group reaped.
    let events = dispatch_then_cancel(/* emit_a_line */ false).await;
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AdapterEvent::TurnEnd { .. })),
        "adapter must emit no terminal event on pre-output cancel; got: {events:?}"
    );
}
