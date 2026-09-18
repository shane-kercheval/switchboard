use std::path::Path;

use async_trait::async_trait;
use chrono::Utc;
use switchboard_core::{AgentRecord, SessionLocator};
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::adapter::{DispatchError, EventStream, HarnessAdapter};
use crate::events::{
    AdapterEvent, ContentKind, ContextWindowSource, TurnId, TurnOutcome, TurnUsage,
};

/// Controls the behaviour of `MockHarnessAdapter`.
///
/// Fault-injection scenarios (`Panic`, `TruncatedStream`) intentionally
/// violate the stream contract — in production, a missing terminal event
/// is an adapter bug, not an acceptable outcome. They exist solely to
/// exercise the dispatcher's state-recovery path.
pub enum MockScenario {
    /// Emits three `ContentChunk`s followed by `TurnEnd(Completed)`. Used for
    /// dev-time UI iteration (no real `claude` needed) and as the standard
    /// test double for the dispatcher.
    Streaming,

    /// Emits a `Text` chunk, a `Thinking` chunk, another `Text` chunk, then
    /// `TurnEnd(Completed)`. The vehicle for the completion-signal test that the
    /// captured text excludes reasoning: the awaited `CompletionResult.text`
    /// must concatenate only the two `Text` chunks, never the `Thinking` one.
    StreamingWithThinking,

    /// Completes with **no content at all** — just `TurnEnd(Completed)`. Models
    /// a real harness turn that produced only thinking/tool items (or whose
    /// text was lost to a parse gap): completed, but nothing forwardable. The
    /// vehicle for the any-empty forward policy tests.
    CompletesEmpty,

    /// Intentionally violates the stream contract — panics mid-stream before
    /// `TurnEnd`. The **only** legitimate use is testing the dispatcher's
    /// `AgentIdleGuard` Drop path under producer-task panic. Never use in
    /// production code paths.
    Panic,

    /// Intentionally violates the stream contract — emits two `ContentChunk`s
    /// and then drops the sender without a terminal event. Distinct from
    /// `Panic` in that the producer exits cleanly; only the contract is
    /// violated. Used to validate the dispatcher's drain loop on truncated
    /// streams *without* relying on a panic side-effect. Never use in
    /// production code paths.
    TruncatedStream,

    /// Returns `Err(DispatchError::BinaryNotFound)` from `dispatch()` before
    /// any stream is established. Used to exercise the dispatcher's
    /// pre-stream failure path: the `AgentIdleGuard` must drop on early
    /// return so agent state restores to `Idle`, and no `TurnStart` is
    /// emitted (the wire stays clean — consumers see the `DispatcherError`
    /// from `send_message`, never a half-stream).
    DispatchFails,

    /// Emits one `ContentChunk` then a terminal `TurnEnd { Failed }`
    /// (`AdapterFailure`). Used to exercise the dispatcher's handling of an
    /// adapter-emitted *failed* terminal — including journaling the failed
    /// outcome and clearing the cancellation token — which the other
    /// scenarios (completed / cancelled) don't cover.
    Fails,

    /// Emits one `ContentChunk`, then **awaits the cancellation token** and,
    /// once fired, ends the stream **without** a terminal event — mirroring a
    /// real adapter's cancel path (kill the subprocess, drop the stream, let
    /// the dispatcher synthesize `TurnEnd { Cancelled }`). The deterministic
    /// vehicle for the dispatcher's cancellation tests: the producer parks
    /// until the test fires the token, so there is no timing race.
    AwaitCancellation,

    /// Emits one `ContentChunk`, **awaits the cancellation token**, then emits
    /// a *second* `ContentChunk` and a real `TurnEnd { Completed }`, and ends —
    /// simulating a harness whose buffered output and result event lost the
    /// race with cancellation. Exercises two invariants: (1) the cancellation
    /// *latch* drops the late real terminal so the synthesized `Cancelled`
    /// wins, and (2) buffered content the agent already produced is still
    /// emitted (partial output stays visible past a cancel, per system-design
    /// §7) — only the terminal is suppressed, not the content.
    TerminalAfterCancel,

    /// Emits `ContentChunk("real answer") → TurnEnd(Completed) →
    /// ContentChunk("stray") → TurnEnd(Completed)` — a **contract-violating**
    /// adapter that emits two terminals (the Claude multi-`result`
    /// background-agent regression was exactly this bug class). The vehicle
    /// for the dispatcher's duplicate-terminal guard: the second `TurnEnd`
    /// must be dropped — not forwarded, not re-firing waiters, not
    /// overwriting the post-terminal stash that `WaitForCurrentTurn` answers
    /// from.
    DuplicateTerminal,

    /// Emits a Codex-shaped post-terminal enrichment sequence:
    /// `ContentChunk → TurnEnd(Completed) → RateLimitEvent → SessionMeta`.
    /// Used in the dispatcher's `agent_idle_is_last_after_codex_post_terminal_enrichment_sequence`
    /// test to pin that the dispatcher preserves adapter event order and
    /// emits `AgentIdle` strictly after all post-terminal events. Real
    /// Codex emits this shape via `emit_terminal_with_enrichment` in
    /// `crates/harness/src/codex/mod.rs`; this scenario stands in without
    /// requiring a subprocess.
    CodexPostTerminalEnrichment,

    /// Emits `ContentChunk → TurnEnd(Completed) → RateLimitEvent → SessionMeta`
    /// with matching durability sources. The vehicle for the dispatcher's
    /// rate-limit persistence gate and its rate-before-model repair: run once
    /// with `StreamOnly` (must persist, then pair the model) and once with
    /// `SessionFileBacked` (must not), asserting the injected `MetadataCache`.
    RateLimitWithSource(crate::events::RateLimitSource),

    /// Emits Claude's ordinary ordering: `SessionMeta → RateLimitEvent →
    /// TurnEnd(Completed)`, with the same model on the metadata and terminal.
    /// The dispatcher should persist the complete snapshot once rather than
    /// rewriting it when the terminal repeats the already-known model.
    RateLimitAfterModel,

    /// Emits `ContentChunk → TurnEnd(Completed) → SessionMeta` whose
    /// inventory names one MCP server, tagged with the given
    /// [`SessionMetaSource`]. The inventory counterpart of
    /// [`Self::RateLimitWithSource`]: run once with `StreamOnly` (must
    /// persist) and once with `SessionFileBacked` (must not).
    SessionMetaWithSource(crate::events::SessionMetaSource),

    /// Emits `ContentChunk → TurnEnd(Completed)` whose `usage` carries the given
    /// context occupancy and `context_window`, tagged with the given
    /// [`ContextWindowSource`]. The
    /// vehicle for the dispatcher's context-window persistence gate: run with
    /// `StreamOnly` (must persist) and `SessionFileBacked` (must not).
    CompletesWithContextWindow {
        context_window: u32,
        context_tokens_after_turn: Option<u64>,
        stable_message_id: Option<String>,
        source: ContextWindowSource,
    },

    /// Emits `ContentChunk → TurnEnd(Completed)` whose `usage.total_cost_usd`,
    /// `spend`, and `stable_message_id` are the given values. The vehicle for
    /// the dispatcher's per-turn cost/overage persistence gate: run with a
    /// real-spend `spend` + `Some(message_id)` (must persist) and with
    /// `real_spend == false` or `stable_message_id == None` (must not).
    CompletesWithSpend {
        total_cost_usd: Option<f64>,
        spend: Option<crate::events::TurnSpend>,
        stable_message_id: Option<String>,
    },

    /// Emits `ContentChunk → TurnEnd` carrying the given `first_message_id`, with
    /// the outcome `Completed` (`fail == false`) or `Failed` (`fail == true`). The
    /// vehicle for the dispatcher's durable send↔turn link gate: a terminal with a
    /// key must write a `TurnLink` for **both** `Completed` and a
    /// crash-truncated-with-content `Failed`; a terminal with `first_message_id ==
    /// None` must write none.
    TerminatesWithKey {
        fail: bool,
        first_message_id: Option<String>,
    },

    /// Emits `ContentChunk → TurnIdentity(key) → TurnEnd(Completed,
    /// first_message_id: Some(key))` — the Claude shape, where the turn's
    /// dedup identity is announced mid-stream and repeated on the terminal.
    /// The vehicle for the dispatcher's early-link gate: the `TurnIdentity`
    /// must journal the `TurnLink` immediately, and the terminal carrying the
    /// same key must NOT write a second one.
    IdentityThenTerminalWithKey { message_id: String },

    /// Emits `ContentChunk → TurnIdentity(identity_key) → TurnEnd(Completed,
    /// first_message_id: Some(terminal_key))` with **differing** keys — an
    /// adapter contract violation (both values come from one field in every
    /// real adapter). The vehicle for the dispatcher's disagreement policy:
    /// keep the persisted early link, warn, and never write the second (two
    /// different keys naming one send is not a poison-detectable conflict, so
    /// a second write would be a silent arbitrary claim-once pick).
    IdentityThenConflictingTerminalKey {
        identity_key: String,
        terminal_key: String,
    },

    /// Emits one `ContentChunk`, **awaits the cancellation token**, then emits
    /// `TurnIdentity(key)` and ends the stream **without** a terminal event —
    /// the Codex cancel shape, where the adapter recovers the turn's durable
    /// id from the session file after killing the subprocess and surfaces it
    /// during the post-cancel drain. The vehicle for the dispatcher writing a
    /// `TurnLink` for a cancelled turn (whose synthesized terminal never can).
    CancelsWithLateIdentity { message_id: String },

    /// Emits `ContentChunk → SessionLocatorCaptured(locator) → TurnEnd(Completed)`.
    /// The vehicle for the dispatcher's runtime-capture tests: drives the
    /// internal capture event so the dispatcher's injected `SessionLocatorSink`
    /// fires (and, with a failing sink, the turn fails). Stands in for a
    /// Codex/Antigravity adapter without a subprocess.
    CapturesLocator(SessionLocator),

    /// Emits `SessionLocatorCaptured(locator) → ContentChunk → TurnEnd(Completed)`
    /// — content and a terminal **after** the capture. Models Antigravity's
    /// post-exit drain (capture, then more transcript content + terminal). The
    /// vehicle for the persist-failure suppression test: with a failing sink the
    /// dispatcher force-fails on the capture, and nothing after it may forward.
    CapturesLocatorThenContent(SessionLocator),

    /// Emits one `ContentChunk` (`"fresh-live-output"`), then **awaits an external
    /// [`tokio::sync::Notify`]** the test controls, then emits
    /// `TurnEnd(Completed)` and ends. The deterministic vehicle for proving an
    /// in-flight turn's text is captured live and handed to a current-turn waiter:
    /// the producer parks after emitting content (so a `wait_for_current_turn`
    /// registers mid-turn), and completes only when the test releases it — unlike
    /// the cancellation scenarios, whose synthesized terminal is `Cancelled`.
    CompletesOnSignal(std::sync::Arc<tokio::sync::Notify>),

    /// Emits `ContentChunk → TurnEnd(Completed)` immediately, then **holds the
    /// stream open** (awaiting an external [`tokio::sync::Notify`]) before ending.
    /// The terminal has fired, so the actor is parked *inside* `drain_turn`'s
    /// post-terminal enrichment-drain window until the test releases it — the
    /// deterministic vehicle for exercising the `FailFast` post-terminal-drain
    /// accept (a back-to-back same-agent re-send accepted while the agent's own
    /// turn is terminal but still draining) and the backlog-drop-fires-completion
    /// paths during that window. Distinct from [`Self::CompletesOnSignal`], which
    /// parks *before* the terminal (mid-turn).
    CompletesThenHolds(std::sync::Arc<tokio::sync::Notify>),

    /// A **compaction** stream (served by [`MockHarnessAdapter::compact`], never
    /// by `dispatch`): `Liveness → TurnEnd(Completed)` carrying the before/after
    /// context occupancy a real compaction reports. No content chunks — a
    /// compaction streams no answer. The standard compaction double.
    CompactsSuccessfully,

    /// A **compaction** stream that ends `Liveness → TurnEnd(Failed {
    /// HarnessError })` — the harness declining to compact (its verdict said no,
    /// which the parser turns into a failed turn even though the process exited
    /// cleanly). Usage is withheld, as it is for a real refusal.
    CompactionFails,

    /// A **compaction** stream that emits one `Liveness`, then **awaits the
    /// cancellation token** and ends without a terminal — the compaction mirror
    /// of [`Self::AwaitCancellation`], so the dispatcher synthesizes
    /// `TurnEnd { Cancelled }`.
    CompactionAwaitsCancellation,

    /// A **compaction** stream driven by two external signals: emit one
    /// `Liveness`, park until `start_terminal`, emit the terminal (`Completed`,
    /// or `Failed { HarnessError }` when `fail`), then park until `end_stream`
    /// before ending.
    ///
    /// The two signals are what make the compaction waiter contract testable —
    /// they split "mid-turn", "past the terminal", and "the stream has drained"
    /// into three windows a test can step through, where a self-driving stream
    /// collapses all three into one instant.
    CompactionOnSignals {
        start_terminal: std::sync::Arc<tokio::sync::Notify>,
        end_stream: std::sync::Arc<tokio::sync::Notify>,
        fail: bool,
    },

    /// A **context report** stream (served by
    /// [`MockHarnessAdapter::context_report`], never by `dispatch`):
    /// `ContextReport → TurnEnd(Completed)`. No content chunks and no usage — a
    /// report streams no answer and makes no model call.
    ReportsContext,

    /// A **context report** stream that ends `Liveness → TurnEnd(Failed {
    /// HarnessError })` — the CLI failing to produce a report at all. Distinct
    /// from an *unreadable* report, which completes successfully carrying raw
    /// text (that path is the parser's, not the dispatcher's).
    ContextReportFails,

    /// A **context report** stream that emits one `Liveness`, then awaits the
    /// cancellation token and ends without a terminal — the report mirror of
    /// [`Self::CompactionAwaitsCancellation`].
    ContextReportAwaitsCancellation,
}

impl MockScenario {
    /// Whether this scenario scripts a **compaction** stream rather than a send.
    /// `dispatch` refuses these and `compact` serves only these, so a mis-wired
    /// test fails at the call instead of getting a stream the scenario never
    /// described.
    fn is_compaction(&self) -> bool {
        matches!(
            self,
            Self::CompactsSuccessfully
                | Self::CompactionFails
                | Self::CompactionAwaitsCancellation
                | Self::CompactionOnSignals { .. }
        )
    }

    /// Whether this scenario scripts a **context report** stream rather than a
    /// send. Same mis-wiring guard as [`Self::is_compaction`].
    fn is_context_report(&self) -> bool {
        matches!(
            self,
            Self::ReportsContext | Self::ContextReportFails | Self::ContextReportAwaitsCancellation
        )
    }
}

/// The report a mock context report carries: enough structure for a consumer to
/// render a panel, and deliberately exact (no `approximate` rows) — the mock
/// stands in for the structured path, which is the one production prefers.
fn mock_context_report() -> crate::context_report::ContextReport {
    crate::context_report::ContextReport {
        model: Some("mock-model".to_owned()),
        total_tokens: Some(48_000),
        max_tokens: Some(200_000),
        categories: vec![
            crate::context_report::ContextCategory {
                name: "System prompt".to_owned(),
                tokens: 4_000,
                kind: "used".to_owned(),
                approximate: false,
            },
            crate::context_report::ContextCategory {
                name: "Messages".to_owned(),
                tokens: 44_000,
                kind: "used".to_owned(),
                approximate: false,
            },
        ],
        mcp_tools: Vec::new(),
        memory_files: Vec::new(),
        agents: Vec::new(),
        skills: Vec::new(),
        raw: "## Context Usage".to_owned(),
        unparsed: false,
    }
}

/// The usage a successful compaction reports: the context occupancy before and
/// after the summary replaced the history, against the model's window. A real
/// compaction makes no assistant call, so these are the turn's only token
/// numbers — input/output are zero, not merely unknown.
fn compaction_usage() -> TurnUsage {
    TurnUsage {
        input_tokens: 0,
        output_tokens: 0,
        cached_input_tokens: None,
        cache_creation_input_tokens: None,
        context_input_tokens: Some(120_000),
        context_tokens_after_turn: Some(18_000),
        reasoning_output_tokens: None,
        context_window: Some(200_000),
        total_cost_usd: None,
    }
}

/// The terminal a mock compaction ends on. Shares one shape across the
/// compaction scenarios so they differ only in what they are testing.
fn compaction_terminal(turn_id: TurnId, fail: bool) -> AdapterEvent {
    AdapterEvent::TurnEnd {
        turn_id,
        outcome: if fail {
            TurnOutcome::Failed {
                kind: crate::events::FailureKind::HarnessError,
                message: "the harness declined to compact this conversation".to_owned(),
            }
        } else {
            TurnOutcome::Completed
        },
        ended_at: Utc::now(),
        // A refused compaction's usage is withheld by the parser — its `result`
        // reports an empty `modelUsage` that would otherwise blank the context
        // bar after a compaction that changed nothing.
        usage: (!fail).then(compaction_usage),
        context_window_source: None,
        // A compaction makes no assistant call, so it has neither key.
        stable_message_id: None,
        first_message_id: None,
        spend: None,
        model: None,
        effort: None,
    }
}

/// The terminal a mock context report ends on. Carries **no usage**: the report
/// makes no model call, so a zero-valued usage record would make the newest
/// usage-bearing turn a free one with no context window.
fn context_report_terminal(turn_id: TurnId, fail: bool) -> AdapterEvent {
    AdapterEvent::TurnEnd {
        turn_id,
        outcome: if fail {
            TurnOutcome::Failed {
                kind: crate::events::FailureKind::HarnessError,
                message: "the harness could not produce a context report".to_owned(),
            }
        } else {
            TurnOutcome::Completed
        },
        ended_at: Utc::now(),
        usage: None,
        context_window_source: None,
        stable_message_id: None,
        first_message_id: None,
        spend: None,
        model: None,
        effort: None,
    }
}

/// A `HarnessAdapter` that produces canned events without spawning any subprocess.
/// Selected at runtime via `SWITCHBOARD_HARNESS=mock`.
pub struct MockHarnessAdapter {
    scenario: MockScenario,
}

impl MockHarnessAdapter {
    pub fn new() -> Self {
        Self::with_scenario(MockScenario::Streaming)
    }

    pub fn with_scenario(scenario: MockScenario) -> Self {
        Self { scenario }
    }
}

impl Default for MockHarnessAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HarnessAdapter for MockHarnessAdapter {
    fn probe(&self) -> Result<(), DispatchError> {
        Ok(())
    }

    fn version(&self) -> Option<String> {
        None
    }

    // One arm per `MockScenario`; the body is a flat match where each arm
    // spawns a small canned producer. It reads top-to-bottom as a catalog of
    // scenarios — splitting it into per-scenario helpers would scatter that
    // catalog for no real gain — so the length lint is allowed here.
    #[allow(clippy::too_many_lines)]
    async fn dispatch(
        &self,
        agent: &AgentRecord,
        _cwd: &Path,
        prompt: &str,
        turn_id: TurnId,
        options: crate::DispatchOptions,
    ) -> Result<EventStream, DispatchError> {
        if matches!(self.scenario, MockScenario::DispatchFails) {
            return Err(DispatchError::BinaryNotFound);
        }
        if self.scenario.is_compaction() {
            return Err(DispatchError::UnsupportedOperation {
                harness: agent.harness,
                operation: "a send against a compaction-only mock scenario",
            });
        }
        if self.scenario.is_context_report() {
            return Err(DispatchError::UnsupportedOperation {
                harness: agent.harness,
                operation: "a send against a context-report-only mock scenario",
            });
        }

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let agent_id = agent.id;

        match self.scenario {
            MockScenario::Streaming => {
                let prompt = prompt.to_owned();
                tokio::spawn(async move {
                    let chunks: [String; 3] = [
                        "Mock response to: ".to_owned(),
                        prompt,
                        " — replied by mock harness.".to_owned(),
                    ];
                    for chunk in chunks {
                        let _ = tx.send(AdapterEvent::ContentChunk {
                            turn_id,
                            kind: ContentKind::Text,
                            text: chunk,
                        });
                    }
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                        stable_message_id: Some("mock-message".to_owned()),
                        first_message_id: None,
                        spend: None,
                        model: None,
                        effort: None,
                    });
                });
            }
            MockScenario::CompletesEmpty => {
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                        stable_message_id: Some("mock-empty".to_owned()),
                        first_message_id: None,
                        spend: None,
                        model: None,
                        effort: None,
                    });
                });
            }
            MockScenario::StreamingWithThinking => {
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "visible-one ".to_owned(),
                    });
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Thinking,
                        text: "secret reasoning".to_owned(),
                    });
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "visible-two".to_owned(),
                    });
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                        stable_message_id: None,
                        first_message_id: None,
                        spend: None,
                        model: None,
                        effort: None,
                    });
                });
            }
            MockScenario::Panic => {
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "partial".to_owned(),
                    });
                    panic!("MockScenario::Panic — intentional, for AgentIdleGuard drop test");
                });
            }
            MockScenario::TruncatedStream => {
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "partial-one".to_owned(),
                    });
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "partial-two".to_owned(),
                    });
                    // Drop tx without emitting TurnEnd — stream closes silently.
                });
            }
            MockScenario::Fails => {
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "partial-before-failure".to_owned(),
                    });
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Failed {
                            kind: crate::events::FailureKind::AdapterFailure,
                            message: "mock failure".to_owned(),
                        },
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                        stable_message_id: None,
                        first_message_id: None,
                        spend: None,
                        model: None,
                        effort: None,
                    });
                });
            }
            MockScenario::AwaitCancellation => {
                let cancel_token = options.cancel_token.clone();
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "partial-before-cancel".to_owned(),
                    });
                    // Park until cancelled, then end the stream with no
                    // terminal event — the dispatcher synthesizes Cancelled.
                    cancel_token.cancelled().await;
                });
            }
            MockScenario::TerminalAfterCancel => {
                let cancel_token = options.cancel_token.clone();
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "before-cancel".to_owned(),
                    });
                    cancel_token.cancelled().await;
                    // Buffered content the agent produced before the kill — it
                    // should still be emitted (partial output stays visible).
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "after-cancel".to_owned(),
                    });
                    // Agent-scoped enrichment buffered behind the cancel — it
                    // reflects real agent state and is forwarded as-is (only the
                    // terminal is the dispatcher's to synthesize).
                    let _ = tx.send(AdapterEvent::RateLimitEvent {
                        agent_id,
                        info: serde_json::json!({"primary": {"used_percent": 50.0}}),
                        // Codex-shaped enrichment → session-file-backed.
                        source: crate::events::RateLimitSource::SessionFileBacked,
                    });
                    // A real terminal that lost the race with cancellation —
                    // the dispatcher must drop this in favor of Cancelled.
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                        stable_message_id: None,
                        first_message_id: None,
                        spend: None,
                        model: None,
                        effort: None,
                    });
                });
            }
            MockScenario::DuplicateTerminal => {
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "real answer".to_owned(),
                    });
                    let completed = AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                        stable_message_id: None,
                        first_message_id: None,
                        spend: None,
                        model: None,
                        effort: None,
                    };
                    let _ = tx.send(completed.clone());
                    // Contract violation from here on: content after the
                    // terminal, then a second terminal.
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "stray".to_owned(),
                    });
                    let _ = tx.send(completed);
                });
            }
            MockScenario::CodexPostTerminalEnrichment => {
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "ack".to_owned(),
                    });
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                        stable_message_id: None,
                        first_message_id: None,
                        spend: None,
                        model: None,
                        effort: None,
                    });
                    let _ = tx.send(AdapterEvent::RateLimitEvent {
                        agent_id,
                        info: serde_json::json!({"primary": {"used_percent": 12.5}}),
                        // Codex-shaped enrichment → session-file-backed.
                        source: crate::events::RateLimitSource::SessionFileBacked,
                    });
                    let _ = tx.send(AdapterEvent::SessionMeta {
                        agent_id,
                        model: "gpt-test".to_owned(),
                        harness_version: "0.130.0".to_owned(),
                        inventory: crate::events::SessionInventory {
                            mcp_servers: Some(vec![crate::events::McpServerStatus {
                                name: "fs".to_owned(),
                                status: "connected".to_owned(),
                                source: None,
                            }]),
                            skills: Some(vec![]),
                            ..crate::events::SessionInventory::default()
                        },
                        raw: serde_json::Value::Null,
                        source: crate::events::SessionMetaSource::SessionFileBacked,
                    });
                });
            }
            MockScenario::RateLimitWithSource(source) => {
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "ack".to_owned(),
                    });
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                        stable_message_id: None,
                        first_message_id: None,
                        spend: None,
                        model: None,
                        effort: None,
                    });
                    let _ = tx.send(AdapterEvent::RateLimitEvent {
                        agent_id,
                        info: serde_json::json!({"primary": {"used_percent": 42.0}}),
                        source,
                    });
                    let meta_source = if source == crate::events::RateLimitSource::StreamOnly {
                        crate::events::SessionMetaSource::StreamOnly
                    } else {
                        crate::events::SessionMetaSource::SessionFileBacked
                    };
                    let _ = tx.send(AdapterEvent::SessionMeta {
                        agent_id,
                        model: "mock-fable".to_owned(),
                        harness_version: "test".to_owned(),
                        inventory: crate::events::SessionInventory::default(),
                        raw: serde_json::Value::Null,
                        source: meta_source,
                    });
                });
            }
            MockScenario::RateLimitAfterModel => {
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::SessionMeta {
                        agent_id,
                        model: "mock-fable".to_owned(),
                        harness_version: "test".to_owned(),
                        inventory: crate::events::SessionInventory::default(),
                        raw: serde_json::Value::Null,
                        source: crate::events::SessionMetaSource::StreamOnly,
                    });
                    let _ = tx.send(AdapterEvent::RateLimitEvent {
                        agent_id,
                        info: serde_json::json!({"primary": {"used_percent": 42.0}}),
                        source: crate::events::RateLimitSource::StreamOnly,
                    });
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "ack".to_owned(),
                    });
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                        stable_message_id: None,
                        first_message_id: None,
                        spend: None,
                        model: Some("mock-fable".to_owned()),
                        effort: None,
                    });
                });
            }
            MockScenario::SessionMetaWithSource(source) => {
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "ack".to_owned(),
                    });
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                        stable_message_id: None,
                        first_message_id: None,
                        spend: None,
                        model: None,
                        effort: None,
                    });
                    let _ = tx.send(AdapterEvent::SessionMeta {
                        agent_id,
                        model: "test-model".to_owned(),
                        harness_version: "0.0.0".to_owned(),
                        inventory: crate::events::SessionInventory {
                            mcp_servers: Some(vec![crate::events::McpServerStatus {
                                name: "tiddly".to_owned(),
                                status: "needs-auth".to_owned(),
                                source: None,
                            }]),
                            ..crate::events::SessionInventory::default()
                        },
                        raw: serde_json::Value::Null,
                        source,
                    });
                });
            }
            MockScenario::CompletesWithContextWindow {
                context_window,
                context_tokens_after_turn,
                ref stable_message_id,
                ref source,
            } => {
                let source = source.clone();
                let stable_message_id = stable_message_id.clone();
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "ack".to_owned(),
                    });
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                        usage: Some(TurnUsage {
                            input_tokens: 100,
                            output_tokens: 25,
                            cached_input_tokens: None,
                            cache_creation_input_tokens: None,
                            context_input_tokens: Some(100),
                            context_tokens_after_turn,
                            reasoning_output_tokens: None,
                            context_window: Some(context_window),
                            total_cost_usd: None,
                        }),
                        context_window_source: Some(source),
                        spend: None,
                        model: None,
                        effort: None,
                        stable_message_id,
                        first_message_id: None,
                    });
                });
            }
            MockScenario::CompletesWithSpend {
                total_cost_usd,
                ref spend,
                ref stable_message_id,
            } => {
                let spend = spend.clone();
                let stable_message_id = stable_message_id.clone();
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "ack".to_owned(),
                    });
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                        usage: Some(TurnUsage {
                            input_tokens: 100,
                            output_tokens: 25,
                            cached_input_tokens: None,
                            cache_creation_input_tokens: None,
                            context_input_tokens: Some(100),
                            context_tokens_after_turn: Some(125),
                            reasoning_output_tokens: None,
                            context_window: None,
                            total_cost_usd,
                        }),
                        context_window_source: None,
                        spend,
                        model: None,
                        effort: None,
                        stable_message_id,
                        first_message_id: None,
                    });
                });
            }
            MockScenario::TerminatesWithKey {
                fail,
                ref first_message_id,
            } => {
                let first_message_id = first_message_id.clone();
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "ack".to_owned(),
                    });
                    let outcome = if fail {
                        TurnOutcome::Failed {
                            kind: crate::events::FailureKind::AdapterFailure,
                            message: "mock failure with partial content".to_owned(),
                        }
                    } else {
                        TurnOutcome::Completed
                    };
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome,
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                        spend: None,
                        model: None,
                        effort: None,
                        stable_message_id: None,
                        first_message_id,
                    });
                });
            }
            MockScenario::IdentityThenTerminalWithKey { ref message_id } => {
                let message_id = message_id.clone();
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "ack".to_owned(),
                    });
                    let _ = tx.send(AdapterEvent::TurnIdentity {
                        turn_id,
                        message_id: message_id.clone(),
                    });
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                        spend: None,
                        model: None,
                        effort: None,
                        stable_message_id: None,
                        first_message_id: Some(message_id),
                    });
                });
            }
            MockScenario::IdentityThenConflictingTerminalKey {
                ref identity_key,
                ref terminal_key,
            } => {
                let identity_key = identity_key.clone();
                let terminal_key = terminal_key.clone();
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "ack".to_owned(),
                    });
                    let _ = tx.send(AdapterEvent::TurnIdentity {
                        turn_id,
                        message_id: identity_key,
                    });
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                        spend: None,
                        model: None,
                        effort: None,
                        stable_message_id: None,
                        first_message_id: Some(terminal_key),
                    });
                });
            }
            MockScenario::CancelsWithLateIdentity { ref message_id } => {
                let message_id = message_id.clone();
                let cancel_token = options.cancel_token.clone();
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "partial-before-cancel".to_owned(),
                    });
                    cancel_token.cancelled().await;
                    // Post-cancel identity recovery (the Codex session-file
                    // read), then end the stream with no terminal — the
                    // dispatcher synthesizes Cancelled after the drain.
                    let _ = tx.send(AdapterEvent::TurnIdentity {
                        turn_id,
                        message_id,
                    });
                });
            }
            MockScenario::CapturesLocator(ref locator) => {
                let locator = locator.clone();
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "ack".to_owned(),
                    });
                    let _ = tx.send(AdapterEvent::SessionLocatorCaptured { locator });
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                        stable_message_id: None,
                        first_message_id: None,
                        spend: None,
                        model: None,
                        effort: None,
                    });
                });
            }
            MockScenario::CapturesLocatorThenContent(ref locator) => {
                let locator = locator.clone();
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::SessionLocatorCaptured { locator });
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "post-capture content".to_owned(),
                    });
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                        stable_message_id: None,
                        first_message_id: None,
                        spend: None,
                        model: None,
                        effort: None,
                    });
                });
            }
            MockScenario::CompletesOnSignal(ref signal) => {
                let signal = std::sync::Arc::clone(signal);
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "fresh-live-output".to_owned(),
                    });
                    // Park mid-turn until the test releases us, then complete.
                    signal.notified().await;
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                        stable_message_id: None,
                        first_message_id: None,
                        spend: None,
                        model: None,
                        effort: None,
                    });
                });
            }
            MockScenario::CompletesThenHolds(ref signal) => {
                let signal = std::sync::Arc::clone(signal);
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContentChunk {
                        turn_id,
                        kind: ContentKind::Text,
                        text: "held-output".to_owned(),
                    });
                    let _ = tx.send(AdapterEvent::TurnEnd {
                        turn_id,
                        outcome: TurnOutcome::Completed,
                        ended_at: Utc::now(),
                        usage: None,
                        context_window_source: None,
                        stable_message_id: None,
                        first_message_id: None,
                        spend: None,
                        model: None,
                        effort: None,
                    });
                    // Hold the stream open *after* the terminal so the actor stays
                    // inside drain_turn's post-terminal window; ending happens when
                    // the test releases us (dropping `tx` closes the stream).
                    signal.notified().await;
                });
            }
            MockScenario::DispatchFails
            | MockScenario::CompactsSuccessfully
            | MockScenario::CompactionFails
            | MockScenario::CompactionAwaitsCancellation
            | MockScenario::CompactionOnSignals { .. }
            | MockScenario::ReportsContext
            | MockScenario::ContextReportFails
            | MockScenario::ContextReportAwaitsCancellation => {
                // Handled by the early returns above.
                unreachable!()
            }
        }

        Ok(Box::pin(UnboundedReceiverStream::new(rx)))
    }

    async fn compact(
        &self,
        agent: &AgentRecord,
        _cwd: &Path,
        turn_id: TurnId,
        options: crate::DispatchOptions,
    ) -> Result<EventStream, DispatchError> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        match self.scenario {
            MockScenario::CompactsSuccessfully | MockScenario::CompactionFails => {
                let fail = matches!(self.scenario, MockScenario::CompactionFails);
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::Liveness { turn_id });
                    let _ = tx.send(compaction_terminal(turn_id, fail));
                });
            }
            MockScenario::CompactionAwaitsCancellation => {
                let cancel_token = options.cancel_token.clone();
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::Liveness { turn_id });
                    // Park until cancelled, then end the stream with no terminal
                    // event — the dispatcher synthesizes Cancelled.
                    cancel_token.cancelled().await;
                });
            }
            MockScenario::CompactionOnSignals {
                ref start_terminal,
                ref end_stream,
                fail,
            } => {
                let start_terminal = std::sync::Arc::clone(start_terminal);
                let end_stream = std::sync::Arc::clone(end_stream);
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::Liveness { turn_id });
                    start_terminal.notified().await;
                    let _ = tx.send(compaction_terminal(turn_id, fail));
                    // Hold the stream open past the terminal until released;
                    // dropping `tx` on return closes it.
                    end_stream.notified().await;
                });
            }
            // Every other scenario models a *send*. Overriding this method
            // replaces the trait's refusing default, so reproduce it here — a
            // harness Switchboard cannot drive to compact is exactly what that
            // default answers.
            _ => {
                return Err(DispatchError::UnsupportedOperation {
                    harness: agent.harness,
                    operation: "manual context compaction",
                });
            }
        }

        Ok(Box::pin(UnboundedReceiverStream::new(rx)))
    }

    async fn context_report(
        &self,
        agent: &AgentRecord,
        _cwd: &Path,
        turn_id: TurnId,
        options: crate::DispatchOptions,
    ) -> Result<EventStream, DispatchError> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let agent_id = agent.id;

        match self.scenario {
            MockScenario::ReportsContext => {
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::ContextReport {
                        agent_id,
                        report: mock_context_report(),
                        at: Utc::now(),
                    });
                    let _ = tx.send(context_report_terminal(turn_id, false));
                });
            }
            MockScenario::ContextReportFails => {
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::Liveness { turn_id });
                    let _ = tx.send(context_report_terminal(turn_id, true));
                });
            }
            MockScenario::ContextReportAwaitsCancellation => {
                let cancel_token = options.cancel_token.clone();
                tokio::spawn(async move {
                    let _ = tx.send(AdapterEvent::Liveness { turn_id });
                    // Park until cancelled, then end the stream with no terminal
                    // event — the dispatcher synthesizes Cancelled.
                    cancel_token.cancelled().await;
                });
            }
            // Same reasoning as `compact` above: overriding the method replaces
            // the trait's refusing default, so reproduce it for every scenario
            // that does not script a report.
            _ => {
                return Err(DispatchError::UnsupportedOperation {
                    harness: agent.harness,
                    operation: "context breakdown",
                });
            }
        }

        Ok(Box::pin(UnboundedReceiverStream::new(rx)))
    }
}
