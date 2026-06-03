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
use switchboard_core::{AgentRecord, HarnessKind, SessionLocator};
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
        session_locator: Some(SessionLocator::Uuid(Uuid::now_v7())),
        created_at: chrono::Utc::now(),
    }
}

fn codex_agent() -> AgentRecord {
    AgentRecord {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "tool-use-codex".to_owned(),
        harness: HarnessKind::Codex,
        session_locator: None,
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
        session_locator: Some(SessionLocator::Uuid(Uuid::new_v4())),
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
        session_locator: None,
        created_at: chrono::Utc::now(),
    }
}

#[tokio::test]
#[ignore = "requires agy authenticated (run `agy`) — run with: make test-live"]
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
    // Non-hidden tempdir prefix: `agy` refuses to add a *hidden* (dot-prefixed)
    // directory as a workspace ("is hidden: ignore uri"), and `tempfile`'s
    // default prefix is `.tmp`. A real project dir is never hidden, so this
    // matches production; with a `.tmp`-prefixed dir `--add-dir` is silently
    // dropped and the model runs tools against $HOME instead of the project.
    let tmp = tempfile::Builder::new()
        .prefix("agy-tool-test")
        .tempdir()
        .expect("tempdir");
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

/// Collect every `tool_use` id the subagent emitted into its sidecar(s) for
/// a given session id. Returns an empty `Vec` if no sidecar directory was
/// created (no delegation happened) or no `tool_use` blocks were recorded.
///
/// Used for two assertions in the live test:
/// - **Drift guard** (`.len() >= 1`): confirms the run exercised the bug
///   path. A subagent that answers without using any tools wouldn't have
///   leaked anything in either a bug-fixed *or* bug-present parser, so
///   the rest of the test would pass spuriously.
/// - **Cross-check**: none of these ids may appear in any parent-stream
///   `ToolStarted` / `ToolCompleted`. If one did, that's a leaked
///   subagent-internal call — the exact M1 bug.
///
/// We glob `~/.claude/projects/*/<session-id>/subagents/*.jsonl` rather
/// than reconstructing the encoded-cwd path because session ids are
/// globally unique and the glob sidesteps any cwd canonicalization quirks
/// (e.g. `/tmp` → `/private/tmp` on macOS).
fn collect_subagent_tool_use_ids(session_id: uuid::Uuid) -> Vec<String> {
    let home = std::env::var_os("HOME").expect("HOME must be set for live tests");
    let projects_dir = std::path::PathBuf::from(home)
        .join(".claude")
        .join("projects");
    let session_dir_name = session_id.to_string();
    let mut ids = Vec::new();
    let Ok(projects) = std::fs::read_dir(&projects_dir) else {
        return ids;
    };
    for project in projects.flatten() {
        let subagents = project.path().join(&session_dir_name).join("subagents");
        let Ok(entries) = std::fs::read_dir(&subagents) else {
            continue;
        };
        for sidecar in entries.flatten() {
            let Ok(content) = std::fs::read_to_string(sidecar.path()) else {
                continue;
            };
            for line in content.lines() {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                let Some(blocks) = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(serde_json::Value::as_array)
                else {
                    continue;
                };
                for block in blocks {
                    if block.get("type").and_then(serde_json::Value::as_str) == Some("tool_use")
                        && let Some(id) = block.get("id").and_then(serde_json::Value::as_str)
                    {
                        ids.push(id.to_owned());
                    }
                }
            }
        }
    }
    ids
}

/// Probes the `parent_tool_use_id` short-circuit in `parser.rs`: when a
/// Claude turn delegates via the `Agent` tool, the parent stream carries
/// subagent-internal `tool_use` / `tool_result` records tagged with
/// `parent_tool_use_id = <Agent tool_use id>`. Those must be suppressed at
/// the parent turn level so the live transcript collapses to one
/// `ToolStarted{Agent}` + one matching `ToolCompleted` for the delegation
/// — matching what the rehydrated session-file view shows.
///
/// **Assertions are deliberately split.** Claude may legitimately invoke
/// preliminary tools (e.g. `TodoWrite`) at the parent level before
/// delegating; counting all `ToolStarted`s would falsely fail on those
/// runs. But naively allow-listing by name (e.g. "exactly one `Agent`
/// start") would let a *bug-leaked* `Bash` slip through unnoticed, because
/// the leaked call's name is `Bash`, not `Agent`. The check therefore
/// pairs two assertions:
///
/// 1. Exactly one parent-turn `ToolStarted` with `name == "Agent"` plus
///    its matching `ToolCompleted` — that's the delegation contract.
/// 2. No parent-stream `ToolStarted` / `ToolCompleted` carries a
///    `tool_use_id` that appears in any subagent sidecar — that's the
///    bug-detection contract, grounded in real subagent-emitted ids.
#[tokio::test]
#[ignore = "requires claude installed — run with: make test-live"]
async fn live_claude_subagent_collapses_to_parent_tool_call() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let adapter = ClaudeCodeAdapter::new();
    let agent = claude_agent();
    let session_id = match &agent.session_locator {
        Some(SessionLocator::Uuid(id)) => *id,
        _ => panic!("Claude agents pre-mint a UUID session locator"),
    };
    let turn_id = Uuid::now_v7();
    let stream = adapter
        .dispatch(
            &agent,
            tmp.path(),
            "Use the Agent tool to launch exactly one general-purpose subagent whose \
             instruction is: run the bash command 'echo hello-from-subagent' and report \
             its exact output. After it returns, reply with the single word done.",
            turn_id,
            DispatchOptions::default(),
        )
        .await
        .expect("dispatch should succeed with real claude");
    let events: Vec<AdapterEvent> = stream.collect().await;

    // Drift guard: passing the assertions below would be vacuous if Claude
    // delegated but the subagent answered without using any internal tools
    // — no tagged tool records would have existed for a bug-present parser
    // to leak. Confirm the subagent sidecar holds at least one tool_use.
    let subagent_tool_use_ids = collect_subagent_tool_use_ids(session_id);
    assert!(
        !subagent_tool_use_ids.is_empty(),
        "subagent didn't run any internal tools (sidecar tool_use count = 0); \
         this run didn't exercise the bug path. The prompt asked for a bash command — Claude may \
         have answered without delegating, or the subagent decided to answer without using its \
         tools. Adjust the prompt to be more explicit, or re-run.",
    );

    // (1) Exactly one parent-turn ToolStarted with name == "Agent", plus
    // its matching ToolCompleted. Tolerates incidental parent-level tools
    // (TodoWrite etc.) — we don't constrain the total count.
    let agent_starts: Vec<&AdapterEvent> = events
        .iter()
        .filter(|e| matches!(e, AdapterEvent::ToolStarted { name, .. } if name == "Agent"))
        .collect();
    assert_eq!(
        agent_starts.len(),
        1,
        "expected exactly one ToolStarted with name=\"Agent\" (the delegation); got {}. Events: {:?}",
        agent_starts.len(),
        events,
    );
    let AdapterEvent::ToolStarted {
        tool_use_id: agent_call_id,
        ..
    } = agent_starts[0]
    else {
        unreachable!();
    };
    let agent_completed = events.iter().find(|e| {
        matches!(
            e,
            AdapterEvent::ToolCompleted { tool_use_id, .. } if tool_use_id == agent_call_id
        )
    });
    assert!(
        agent_completed.is_some(),
        "Agent ToolStarted ({agent_call_id}) had no matching ToolCompleted. Events: {events:?}",
    );

    // (2) No parent-stream ToolStarted/ToolCompleted may carry a tool_use_id
    // that was emitted inside a subagent. A match here is the M1 bug —
    // a subagent-internal call leaking into the parent turn.
    let leaked: Vec<(&str, &str)> = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::ToolStarted { tool_use_id, .. } => subagent_tool_use_ids
                .iter()
                .find(|sid| sid.as_str() == tool_use_id)
                .map(|sid| ("ToolStarted", sid.as_str())),
            AdapterEvent::ToolCompleted { tool_use_id, .. } => subagent_tool_use_ids
                .iter()
                .find(|sid| sid.as_str() == tool_use_id)
                .map(|sid| ("ToolCompleted", sid.as_str())),
            _ => None,
        })
        .collect();
    assert!(
        leaked.is_empty(),
        "subagent-internal tool_use_id(s) leaked into the parent stream — the M1 bug. \
         Leaked: {leaked:?}. Subagent ids: {subagent_tool_use_ids:?}. Events: {events:?}",
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
