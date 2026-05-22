//! Live tool-event coverage across all four adapters.
//!
//! Each test prompts the real CLI to use a file-read / shell tool and asserts
//! that `ToolStarted` is followed by a matching `ToolCompleted`. Pairing
//! shape is per-harness:
//!
//! - Claude / Codex: sentinel-in-output (`ToolCompleted.output` contains the
//!   staged sentinel, `is_error: false`), correlated by `tool_use_id`. A
//!   CLI-side tool rename (`Read` → something else, normalized
//!   `command_execution` → other) stays non-brittle because the matching
//!   keys off the id, not the name.
//! - Gemini: lifecycle-only — pair from the `ToolStarted` side by matching
//!   the prompt's staged path against the `input` JSON. Gemini's stream
//!   emits `tool_result.output = ""` for read-like tools (the real content
//!   lives in the session file, surfaced via transcript hydration), so
//!   sentinel-in-output pairing doesn't apply.
//!
//! Run with: `make test-live`. All tests are `#[ignore]`-gated.

use futures::StreamExt;
use switchboard_core::{AgentRecord, HarnessKind};
use switchboard_harness::{
    AdapterEvent, AntigravityAdapter, ClaudeCodeAdapter, CodexAdapter, ContentKind,
    DispatchOptions, GeminiAdapter, HarnessAdapter, TurnOutcome,
};
use uuid::Uuid;

const CLAUDE_TOKEN: &str = "SWITCHBOARD_TOOL_LIVE_F2A98C";
const CODEX_TOKEN: &str = "SWITCHBOARD_TOOL_LIVE_C0D3X1";
const GEMINI_TOKEN: &str = "SWITCHBOARD_TOOL_LIVE_GEM1N1";
const ANTIGRAVITY_TOKEN: &str = "SWITCHBOARD_TOOL_LIVE_AGY001";

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

fn gemini_agent() -> AgentRecord {
    AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "tool-use-gemini".to_owned(),
        harness: HarnessKind::Gemini,
        // UUID v4 for Gemini session IDs (8-char-prefix filename collision
        // hazard under v7 — see `gemini-cli-observed.md`).
        session_id: Some(Uuid::new_v4()),
        created_at: chrono::Utc::now(),
    }
}

/// Lifecycle-only tool-use pairing. Gemini's stream emits
/// `tool_result.output = ""` for `read_file` and likely other read-like
/// tools (the real content lives in the session file, surfaced via
/// transcript hydration), so sentinel-in-output pairing (the
/// `tool_call_with_output` helper) doesn't apply.
///
/// Pairs from the `ToolStarted` side by matching `input_contains`
/// against the stringified `input` JSON. The caller-supplied substring
/// (typically the file path the prompt references) is the strongest
/// available signal because it survives a tool rename (`read_file` →
/// `ReadFile`, etc.) — the user-supplied file path is the load-bearing
/// invariant, not the tool name. Returns the started/completed pair on
/// success.
fn read_tool_lifecycle<'a>(
    events: &'a [AdapterEvent],
    input_contains: &str,
) -> Option<(&'a AdapterEvent, &'a AdapterEvent)> {
    let started = events.iter().find(|e| match e {
        AdapterEvent::ToolStarted { input, .. } => {
            serde_json::to_string(input).is_ok_and(|s| s.contains(input_contains))
        }
        _ => false,
    })?;
    let AdapterEvent::ToolStarted {
        tool_use_id: started_id,
        ..
    } = started
    else {
        unreachable!("filter above guarantees the variant");
    };
    let completed = events.iter().find(|e| {
        matches!(
            e,
            AdapterEvent::ToolCompleted { tool_use_id, is_error: false, .. }
                if tool_use_id == started_id
        )
    })?;
    Some((started, completed))
}

#[tokio::test]
#[ignore = "requires gemini installed — run with: make test-live"]
async fn live_gemini_emits_tool_started_and_tool_completed_for_file_read() {
    // **Lifecycle assertion only — not sentinel-in-output.** Gemini's
    // stream emits `tool_result.output = ""` for read-like tools (the
    // real content lives in the session file, surfaced via transcript
    // hydration on project reopen). The sentinel-in-output assertion
    // moves to `transcript_load.rs`'s
    // `live_gemini_transcript_load_hydrates_tool_items` where it can
    // actually be checked.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(tmp.path().join("MARKER.txt"), GEMINI_TOKEN).expect("write marker");

    let adapter = GeminiAdapter::new();
    let agent = gemini_agent();
    let turn_id = Uuid::now_v7();
    let stream = adapter
        .dispatch(
            &agent,
            tmp.path(),
            "Read the file MARKER.txt in the current directory and reply with only its contents.",
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed with real gemini");
    let events: Vec<AdapterEvent> = stream.collect().await;

    let (started, _completed) = read_tool_lifecycle(&events, "MARKER.txt").unwrap_or_else(|| {
        panic!(
            "expected a ToolStarted reading MARKER.txt paired with a non-error ToolCompleted; \
             got events: {events:?}"
        )
    });
    let AdapterEvent::ToolStarted { name, .. } = started else {
        unreachable!();
    };
    assert!(!name.is_empty(), "ToolStarted.name must be non-empty");

    // Gemini also reads the file via its `read_file` builtin, and the
    // final assistant message echoes the contents. So we still see the
    // sentinel in the *content_chunk* stream — just not in the tool
    // output field.
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::ContentChunk { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        text.contains(GEMINI_TOKEN),
        "Gemini's reply text should echo the file contents; got: {text:?}"
    );

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

fn antigravity_agent() -> AgentRecord {
    AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "tool-use-antigravity".to_owned(),
        harness: HarnessKind::Antigravity,
        session_id: None,
        created_at: chrono::Utc::now(),
    }
}

#[tokio::test]
#[ignore = "requires agy authed via Antigravity desktop app — run with: make test-live"]
async fn live_antigravity_emits_tool_started_and_tool_completed_for_file_read() {
    // Antigravity's tool lifecycle AND answer text both come from tailing
    // `transcript.jsonl` (stdout replays the whole conversation on resume, so
    // it's a control channel only). Its `view_file` tool result record embeds
    // the file content (with line-number prefixes) in the `content` blob — so
    // unlike Gemini, sentinel-in-output pairing works: the token survives
    // inside the rendered `ToolCompleted.output`. The `TurnEnd(Completed)`
    // assertion below also validates the agentic-turn case for the new outcome
    // rule: a tool-using turn must still produce a transcript terminal answer
    // (`saw_terminal_answer`), or `classify_outcome` would now fail it loud.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(tmp.path().join("MARKER.txt"), ANTIGRAVITY_TOKEN).expect("write marker");

    let adapter = AntigravityAdapter::new();
    let agent = antigravity_agent();
    let turn_id = Uuid::now_v7();
    let stream = adapter
        .dispatch(
            &agent,
            tmp.path(),
            "Read the file MARKER.txt in the current directory and reply with only its contents.",
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed with real agy");
    let events: Vec<AdapterEvent> = stream.collect().await;

    let (started, _completed) =
        tool_call_with_output(&events, ANTIGRAVITY_TOKEN).unwrap_or_else(|| {
            panic!(
                "expected a non-error ToolCompleted whose output contains {ANTIGRAVITY_TOKEN:?}; \
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

    // The agentic turn must surface a non-empty assistant answer — the
    // transcript-derived terminal text. This asserts directly what
    // `TurnEnd(Completed)` proves only transitively under the current
    // classifier (Completed requires a transcript terminal answer), guarding
    // against a future classifier/parser refactor weakening that coupling.
    assert!(
        events.iter().any(|e| matches!(
            e,
            AdapterEvent::ContentChunk { kind: ContentKind::Text, text, .. } if !text.trim().is_empty()
        )),
        "expected a non-empty Text answer chunk on a tool-using turn; got: {events:?}"
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
