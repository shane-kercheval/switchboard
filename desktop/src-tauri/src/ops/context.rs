//! Each agent's context fullness for callers that are not the window: the
//! figures of its newest finished turn, or of a newer compaction, as the
//! sidebar's context bar shows them.
//!
//! **One rule, copied from the sidebar** (`contextOccupancy` in
//! `Sidebar.svelte`): the newest finished turn with a usage record, whatever
//! its status, superseded by a newer compaction's compacted size (with the
//! compaction's own window, or the superseded turn's when it has none). The
//! figures are `None` when that turn's are missing or impossible (used above
//! the window), with no fall back to an older turn, exactly when the sidebar
//! hides its bar. Both copies run the cases in
//! `desktop/tests/fixtures/context-occupancy.json`.
//!
//! **Two sources, one store point each.** Live figures come from every
//! `turn_end` the ledger's emitter decorator sees, for every turn whatever its
//! origin; a completed compaction is a turn whose figures are the compacted
//! size. Disk figures come from every transcript parse that records figures (the
//! window's project load, the read tier's loads, and this module's own
//! fallback read), all run through [`ContextStore::parse_recording`] under one
//! guard: the store captures the agent's live generation before the parse
//! runs, and the result lands only if no live figure arrived meanwhile and it
//! is not older than what is held; equal times keep the live fact. A large
//! file can take seconds to parse, so a read can finish after the reply or
//! compaction it did not see. The capture happens inside the store, so a parse
//! site cannot take it late.
//!
//! **The fallback read.** An agent nothing has parsed since launch is read
//! once, the way `read_latest_reply` reads it, by the first listing that needs
//! it; listings that arrive while that read runs join it. These reads run one
//! at a time, because each parses a whole file into memory (the largest local
//! files, 107 MB and 218 MB, parse in about 1 and 3.5 seconds and peak near
//! 800 MB). A listing waits [`CONTEXT_READ_WAIT`] for the reads it needs and
//! reports `context_pending` for any still running, which finishes on its own.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use switchboard_core::AgentId;
use switchboard_harness::{
    CompactedContext, LoadedTranscript, NormalizedEvent, SystemMarker, Turn, TurnStatus, TurnUsage,
};
use tokio::sync::{Semaphore, watch};

use crate::error::AppError;
use crate::state::lock;

/// How long a listing waits for the session-file reads it started or joined
/// before reporting them as pending. Covers the largest local files with room
/// to spare, and stays well inside every client's shortest call limit.
pub const CONTEXT_READ_WAIT: Duration = Duration::from_secs(5);

/// An agent's context fullness: tokens in use, the window, and the share.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ContextFigures {
    pub used_tokens: u64,
    pub window_tokens: u32,
    pub fraction: f64,
}

/// What the store knows about an agent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ContextState {
    /// Nothing has reported for the agent since launch and no read has
    /// finished.
    Unknown,
    /// The newest figures, or `None` when the newest usage-bearing turn has no
    /// usable figures (or the file holds no turn with usage).
    Known(Option<ContextFigures>),
}

/// The newest usage-bearing turn or compaction for an agent.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Fact {
    at: DateTime<Utc>,
    used: Option<u64>,
    window: Option<u32>,
    /// From the live event stream rather than a file; wins a tie on `at`.
    live: bool,
}

impl Fact {
    fn figures(self) -> Option<ContextFigures> {
        let window = self.window.filter(|w| *w != 0)?;
        let used = self.used?;
        if used > u64::from(window) {
            return None;
        }
        #[allow(clippy::cast_precision_loss)] // Token counts are far below 2^53.
        let fraction = used as f64 / f64::from(window);
        Some(ContextFigures {
            used_tokens: used,
            window_tokens: window,
            fraction,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Known {
    Unknown,
    Settled(Option<Fact>),
}

struct AgentContext {
    /// Bumped on every live fact, so a disk read started before it cannot
    /// overwrite it.
    generation: u64,
    known: Known,
    /// The fallback read in flight, if any: `true` once it has finished.
    pending: Option<watch::Sender<bool>>,
}

impl Default for AgentContext {
    fn default() -> Self {
        Self {
            generation: 0,
            known: Known::Unknown,
            pending: None,
        }
    }
}

pub struct ContextStore {
    agents: Mutex<HashMap<AgentId, AgentContext>>,
    /// One fallback read at a time.
    reads: Semaphore,
}

impl Default for ContextStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            agents: Mutex::new(HashMap::new()),
            reads: Semaphore::new(1),
        }
    }

    /// The agent's live generation, captured before a parse.
    fn generation(&self, agent_id: AgentId) -> u64 {
        lock(&self.agents)
            .get(&agent_id)
            .map_or(0, |a| a.generation)
    }

    /// What is known about the agent now.
    #[must_use]
    pub fn lookup(&self, agent_id: AgentId) -> ContextState {
        match lock(&self.agents).get(&agent_id).map(|a| a.known) {
            None | Some(Known::Unknown) => ContextState::Unknown,
            Some(Known::Settled(fact)) => ContextState::Known(fact.and_then(Fact::figures)),
        }
    }

    /// Feed one emitted event: a `turn_end` on an agent channel that carries
    /// usage becomes the agent's newest live fact.
    pub fn observe(&self, channel: &str, payload: &serde_json::Value) {
        let Some(agent_id) = channel
            .strip_prefix("agent:")
            .and_then(|id| id.parse::<uuid::Uuid>().ok())
        else {
            return;
        };
        if payload.get("type").and_then(serde_json::Value::as_str) != Some("turn_end") {
            return;
        }
        if let Ok(NormalizedEvent::TurnEnd {
            ended_at,
            usage: Some(usage),
            ..
        }) = serde_json::from_value::<NormalizedEvent>(payload.clone())
        {
            self.note_live(agent_id, ended_at, &usage);
        }
    }

    /// A turn ended live with a usage record.
    pub fn note_live(&self, agent_id: AgentId, at: DateTime<Utc>, usage: &TurnUsage) {
        let mut agents = lock(&self.agents);
        let entry = agents.entry(agent_id).or_default();
        entry.generation += 1;
        entry.known = Known::Settled(Some(Fact {
            at,
            used: usage.context_tokens_after_turn,
            window: usage.context_window,
            live: true,
        }));
    }

    /// Run a transcript parse and record what it found. The agent's live
    /// generation is captured here, before `read` runs; the result lands only
    /// if no live fact arrived since, and only if it is not older than what is
    /// held (a tie keeps a live fact). A failed parse settles an agent nothing
    /// has reported for as having no figures, so a later listing does not run
    /// one more read that fails the same way. Every parse that records figures goes through
    /// this, so the capture cannot come late.
    pub async fn parse_recording<F>(
        &self,
        agent_id: AgentId,
        read: F,
    ) -> Result<LoadedTranscript, AppError>
    where
        F: Future<Output = Result<LoadedTranscript, AppError>>,
    {
        let generation = self.generation(agent_id);
        match read.await {
            Ok(transcript) => {
                self.record_from_transcript(agent_id, generation, &transcript);
                Ok(transcript)
            }
            Err(error) => {
                self.record_read_failed(agent_id, generation);
                Err(error)
            }
        }
    }

    /// Record what a transcript parse found under the generation captured
    /// before it ran.
    fn record_from_transcript(
        &self,
        agent_id: AgentId,
        generation_before: u64,
        transcript: &LoadedTranscript,
    ) {
        let disk = fact_from_transcript(transcript);
        let mut agents = lock(&self.agents);
        let entry = agents.entry(agent_id).or_default();
        if entry.generation != generation_before {
            return;
        }
        match (entry.known, disk) {
            (Known::Settled(Some(held)), Some(found)) => {
                if found.at > held.at || (found.at == held.at && !held.live) {
                    entry.known = Known::Settled(Some(found));
                }
            }
            // A file with nothing in it cannot clear figures already held.
            (Known::Settled(Some(_)), None) => {}
            (Known::Unknown | Known::Settled(None), found) => {
                entry.known = Known::Settled(found);
            }
        }
    }

    /// A fallback read failed: the agent settles as having no figures, unless
    /// a fact arrived meanwhile.
    fn record_read_failed(&self, agent_id: AgentId, generation_before: u64) {
        let mut agents = lock(&self.agents);
        let entry = agents.entry(agent_id).or_default();
        if entry.generation == generation_before && entry.known == Known::Unknown {
            entry.known = Known::Settled(None);
        }
    }

    /// Make sure the agent's file is being read, starting `read` as a task if
    /// no read is in flight, and return a receiver that turns `true` when the
    /// read (this one or the one joined) has finished and recorded its result.
    /// `read` must be lazy (an `async` block), so it starts only once the
    /// read's turn comes.
    pub fn ensure_reading<F>(self: &Arc<Self>, agent_id: AgentId, read: F) -> watch::Receiver<bool>
    where
        F: Future<Output = Result<LoadedTranscript, AppError>> + Send + 'static,
    {
        let mut agents = lock(&self.agents);
        let entry = agents.entry(agent_id).or_default();
        if let Some(pending) = &entry.pending
            && !*pending.borrow()
        {
            return pending.subscribe();
        }
        let (done, receiver) = watch::channel(false);
        entry.pending = Some(done.clone());
        drop(agents);
        let store = Arc::clone(self);
        tokio::spawn(async move {
            // Taken before the generation is captured, so a live fact that
            // lands while this read waits its turn is seen by the capture.
            let _permit = store.reads.acquire().await;
            if let Err(error) = store.parse_recording(agent_id, read).await {
                tracing::warn!(%agent_id, %error, "context read of the agent's session file failed");
            }
            let mut agents = lock(&store.agents);
            if let Some(entry) = agents.get_mut(&agent_id) {
                entry.pending = None;
            }
            drop(agents);
            done.send_replace(true);
        });
        receiver
    }
}

/// The newest usage-bearing turn or compaction in a parsed transcript, by the
/// sidebar's rule. `None` when the file holds neither.
fn fact_from_transcript(transcript: &LoadedTranscript) -> Option<Fact> {
    let latest_turn = transcript.turns.iter().rev().find_map(|turn| match turn {
        Turn::Agent {
            status,
            usage: Some(usage),
            started_at,
            ended_at,
            ..
        } if *status != TurnStatus::Streaming => Some((ended_at.unwrap_or(*started_at), usage)),
        _ => None,
    });
    let mut compacted: Option<(DateTime<Utc>, CompactedContext)> = None;
    for turn in &transcript.turns {
        if let Turn::System {
            started_at,
            marker:
                SystemMarker::Compaction {
                    context_after: Some(context),
                    ..
                },
            ..
        } = turn
            && compacted.is_none_or(|(at, _)| *started_at > at)
        {
            compacted = Some((*started_at, *context));
        }
    }
    match (latest_turn, compacted) {
        (turn, Some((at, context))) if turn.is_none_or(|(turn_at, _)| at > turn_at) => Some(Fact {
            at,
            used: Some(context.tokens),
            window: context
                .window
                .or_else(|| turn.and_then(|(_, usage)| usage.context_window)),
            live: false,
        }),
        (Some((at, usage)), _) => Some(Fact {
            at,
            used: usage.context_tokens_after_turn,
            window: usage.context_window,
            live: false,
        }),
        (None, _) => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::{Value, json};
    use tokio::sync::Notify;
    use uuid::Uuid;

    use super::*;

    const FIXTURE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tests/fixtures/context-occupancy.json"
    ));

    fn instant(value: &Value) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value.as_str().unwrap())
            .unwrap()
            .with_timezone(&Utc)
    }

    fn usage(after: Option<u64>, window: Option<u32>) -> TurnUsage {
        TurnUsage {
            input_tokens: 0,
            output_tokens: 0,
            cached_input_tokens: None,
            cache_creation_input_tokens: None,
            context_input_tokens: None,
            context_tokens_after_turn: after,
            reasoning_output_tokens: None,
            context_window: window,
            total_cost_usd: None,
        }
    }

    /// One fixture case as a parsed transcript.
    fn transcript(case: &Value, agent_id: AgentId) -> LoadedTranscript {
        let mut turns = Vec::new();
        for turn in case["turns"].as_array().unwrap() {
            let started_at = instant(&turn["started_at"]);
            if turn["role"] == "user" {
                turns.push(Turn::User {
                    turn_id: Uuid::now_v7(),
                    agent_id,
                    started_at,
                    text: "hi".to_owned(),
                    source: switchboard_harness::UserPromptSource::default(),
                });
                continue;
            }
            let status = match turn["status"].as_str().unwrap() {
                "complete" => TurnStatus::Complete,
                "failed" => TurnStatus::Failed,
                "streaming" => TurnStatus::Streaming,
                other => panic!("unknown status {other}"),
            };
            let usage = turn["usage"].as_object().map(|u| {
                usage(
                    u.get("context_tokens_after_turn").and_then(Value::as_u64),
                    u.get("context_window")
                        .and_then(Value::as_u64)
                        .map(|w| u32::try_from(w).unwrap()),
                )
            });
            turns.push(Turn::Agent {
                turn_id: Uuid::now_v7(),
                agent_id,
                started_at,
                ended_at: turn["ended_at"]
                    .as_str()
                    .map(|_| instant(&turn["ended_at"])),
                status,
                items: Vec::new(),
                usage,
                model: None,
                effort: None,
                spend: None,
                hydration_key: None,
                continuation_of: None,
                stable_message_id: None,
            });
        }
        if let Some(compaction) = case["compaction"].as_object() {
            turns.push(Turn::System {
                turn_id: Uuid::now_v7(),
                agent_id,
                started_at: instant(&compaction["at"]),
                marker: SystemMarker::Compaction {
                    summary: None,
                    context_after: Some(CompactedContext {
                        tokens: compaction["tokens"].as_u64().unwrap(),
                        window: compaction["window"]
                            .as_u64()
                            .map(|w| u32::try_from(w).unwrap()),
                    }),
                    record_id: None,
                },
            });
        }
        LoadedTranscript {
            turns,
            ..LoadedTranscript::default()
        }
    }

    fn expected(case: &Value) -> Option<ContextFigures> {
        case["expected"].as_object().map(|e| ContextFigures {
            used_tokens: e["used_tokens"].as_u64().unwrap(),
            window_tokens: u32::try_from(e["window_tokens"].as_u64().unwrap()).unwrap(),
            fraction: e["fraction"].as_f64().unwrap(),
        })
    }

    #[test]
    fn the_sidebars_cases_hold_for_the_disk_rule() {
        let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
        let cases = fixture["cases"].as_array().unwrap();
        assert!(cases.len() >= 16, "the fixture holds the cases");
        for case in cases {
            let agent_id = Uuid::now_v7();
            let found = fact_from_transcript(&transcript(case, agent_id)).and_then(Fact::figures);
            assert_eq!(found, expected(case), "{}", case["name"]);
        }
    }

    fn turn_end(ended_at: &str, usage: Option<&Value>) -> Value {
        json!({
            "type": "turn_end",
            "turn_id": Uuid::now_v7(),
            "outcome": { "status": "completed" },
            "ended_at": ended_at,
            "usage": usage, "spend": null, "model": null, "effort": null, "hydration_key": null,
        })
    }

    fn usage_json(after: u64, window: u32) -> Value {
        json!({
            "input_tokens": 1, "output_tokens": 1, "cached_input_tokens": null,
            "cache_creation_input_tokens": null, "context_input_tokens": null,
            "context_tokens_after_turn": after, "reasoning_output_tokens": null,
            "context_window": window, "total_cost_usd": null,
        })
    }

    #[allow(clippy::cast_precision_loss)] // Test figures are small.
    fn figures(used: u64, window: u32) -> ContextFigures {
        ContextFigures {
            used_tokens: used,
            window_tokens: window,
            fraction: used as f64 / f64::from(window),
        }
    }

    #[test]
    fn a_turn_end_with_usage_on_the_agents_channel_is_its_newest_fact() {
        let store = ContextStore::new();
        let agent_id = Uuid::now_v7();
        let channel = format!("agent:{agent_id}");
        assert_eq!(store.lookup(agent_id), ContextState::Unknown);

        store.observe(
            &channel,
            &turn_end("2026-10-13T10:00:00Z", Some(&usage_json(120_000, 200_000))),
        );
        assert_eq!(
            store.lookup(agent_id),
            ContextState::Known(Some(figures(120_000, 200_000)))
        );
        // A completed compaction is a turn whose figures are the compacted size.
        store.observe(
            &channel,
            &turn_end("2026-10-13T10:05:00Z", Some(&usage_json(18_000, 200_000))),
        );
        assert_eq!(
            store.lookup(agent_id),
            ContextState::Known(Some(figures(18_000, 200_000)))
        );
        // A turn without a usage record changes nothing.
        store.observe(&channel, &turn_end("2026-10-13T10:06:00Z", None));
        assert_eq!(
            store.lookup(agent_id),
            ContextState::Known(Some(figures(18_000, 200_000)))
        );
        // Other channels and other events are not facts.
        store.observe(
            "workflow:x",
            &turn_end("2026-10-13T10:07:00Z", Some(&usage_json(1, 2))),
        );
        store.observe(
            &channel,
            &json!({ "type": "agent_idle", "agent_id": agent_id }),
        );
        assert_eq!(
            store.lookup(agent_id),
            ContextState::Known(Some(figures(18_000, 200_000)))
        );
    }

    #[test]
    fn a_live_fact_without_usable_figures_reads_as_none_with_no_fall_back() {
        let store = ContextStore::new();
        let agent_id = Uuid::now_v7();
        store.note_live(agent_id, Utc::now(), &usage(Some(100), Some(200)));
        store.note_live(agent_id, Utc::now(), &usage(Some(100), None));
        assert_eq!(store.lookup(agent_id), ContextState::Known(None));
    }

    fn file_with(at: &str, used: u64, window: u32) -> LoadedTranscript {
        transcript(
            &json!({
                "turns": [{ "role": "agent", "status": "complete", "started_at": at, "ended_at": at,
                            "usage": { "context_tokens_after_turn": used, "context_window": window } }],
                "compaction": null,
            }),
            Uuid::now_v7(),
        )
    }

    #[test]
    fn a_disk_result_lands_only_for_the_generation_it_read_under_and_never_over_a_newer_fact() {
        let store = ContextStore::new();
        let agent_id = Uuid::now_v7();
        let live_at = instant(&json!("2026-10-13T10:05:00Z"));

        // Captured before the live fact: dropped, however new its figures.
        let stale_generation = store.generation(agent_id);
        store.note_live(agent_id, live_at, &usage(Some(500), Some(1000)));
        store.record_from_transcript(
            agent_id,
            stale_generation,
            &file_with("2026-10-13T11:00:00Z", 900, 1000),
        );
        assert_eq!(
            store.lookup(agent_id),
            ContextState::Known(Some(figures(500, 1000)))
        );

        // Captured after it, but older than the live fact: dropped.
        let generation = store.generation(agent_id);
        store.record_from_transcript(
            agent_id,
            generation,
            &file_with("2026-10-13T10:00:00Z", 900, 1000),
        );
        // The same instant: the live fact stays.
        store.record_from_transcript(
            agent_id,
            generation,
            &file_with("2026-10-13T10:05:00Z", 900, 1000),
        );
        assert_eq!(
            store.lookup(agent_id),
            ContextState::Known(Some(figures(500, 1000)))
        );

        // Newer than the live fact (a turn run in a terminal): taken.
        store.record_from_transcript(
            agent_id,
            generation,
            &file_with("2026-10-13T10:30:00Z", 700, 1000),
        );
        assert_eq!(
            store.lookup(agent_id),
            ContextState::Known(Some(figures(700, 1000)))
        );
        // A later disk fact at the same instant replaces a disk fact.
        store.record_from_transcript(
            agent_id,
            generation,
            &file_with("2026-10-13T10:30:00Z", 750, 1000),
        );
        assert_eq!(
            store.lookup(agent_id),
            ContextState::Known(Some(figures(750, 1000)))
        );
        // A file with nothing in it cannot clear figures.
        store.record_from_transcript(agent_id, generation, &LoadedTranscript::default());
        assert_eq!(
            store.lookup(agent_id),
            ContextState::Known(Some(figures(750, 1000)))
        );
    }

    #[test]
    fn an_empty_file_settles_an_unknown_agent_and_a_failed_read_settles_only_an_unknown_one() {
        let store = ContextStore::new();
        let agent_id = Uuid::now_v7();
        let generation = store.generation(agent_id);
        store.record_from_transcript(agent_id, generation, &LoadedTranscript::default());
        assert_eq!(store.lookup(agent_id), ContextState::Known(None));

        let other = Uuid::now_v7();
        let generation = store.generation(other);
        store.record_read_failed(other, generation);
        assert_eq!(store.lookup(other), ContextState::Known(None));

        let third = Uuid::now_v7();
        let generation = store.generation(third);
        store.note_live(third, Utc::now(), &usage(Some(10), Some(100)));
        store.record_read_failed(third, generation);
        assert_eq!(
            store.lookup(third),
            ContextState::Known(Some(figures(10, 100)))
        );
    }

    #[tokio::test]
    async fn concurrent_requests_share_one_read_and_reads_run_one_at_a_time() {
        let store = Arc::new(ContextStore::new());
        let started = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(Notify::new());
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        let read = |at: &'static str| {
            let started = Arc::clone(&started);
            let gate = Arc::clone(&gate);
            async move {
                started.fetch_add(1, Ordering::SeqCst);
                gate.notified().await;
                Ok(file_with(at, 100, 1000))
            }
        };
        let mut first = store.ensure_reading(a, read("2026-10-13T10:00:00Z"));
        tokio::task::yield_now().await;
        let mut joined = store.ensure_reading(a, read("2026-10-13T10:00:00Z"));
        let mut second = store.ensure_reading(b, read("2026-10-13T11:00:00Z"));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            started.load(Ordering::SeqCst),
            1,
            "one read for the agent, and the other agent's waits its turn"
        );
        gate.notify_waiters();
        first.wait_for(|done| *done).await.unwrap();
        joined.wait_for(|done| *done).await.unwrap();
        assert_eq!(
            store.lookup(a),
            ContextState::Known(Some(figures(100, 1000)))
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            started.load(Ordering::SeqCst),
            2,
            "the second agent's read started"
        );
        gate.notify_waiters();
        second.wait_for(|done| *done).await.unwrap();
        assert_eq!(
            store.lookup(b),
            ContextState::Known(Some(figures(100, 1000)))
        );
    }

    #[tokio::test]
    async fn a_read_held_open_across_a_live_turn_does_not_overwrite_its_figures() {
        let store = Arc::new(ContextStore::new());
        let gate = Arc::new(Notify::new());
        let agent_id = Uuid::now_v7();
        let mut done = store.ensure_reading(agent_id, {
            let gate = Arc::clone(&gate);
            async move {
                gate.notified().await;
                Ok(file_with("2026-10-13T12:00:00Z", 900, 1000))
            }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        // The compaction lands while the file is still being parsed.
        store.note_live(
            agent_id,
            instant(&json!("2026-10-13T10:00:00Z")),
            &usage(Some(18), Some(1000)),
        );
        gate.notify_waiters();
        done.wait_for(|done| *done).await.unwrap();
        assert_eq!(
            store.lookup(agent_id),
            ContextState::Known(Some(figures(18, 1000)))
        );
    }

    #[tokio::test]
    async fn a_failed_read_settles_pending_without_figures() {
        let store = Arc::new(ContextStore::new());
        let agent_id = Uuid::now_v7();
        let mut done = store.ensure_reading(agent_id, async {
            Err(AppError::Internal("unreadable".to_owned()))
        });
        done.wait_for(|done| *done).await.unwrap();
        assert_eq!(store.lookup(agent_id), ContextState::Known(None));
    }

    #[tokio::test]
    async fn a_parse_through_the_store_records_its_result_and_settles_a_failure() {
        let store = ContextStore::new();
        let agent_id = Uuid::now_v7();
        let parsed = store
            .parse_recording(agent_id, async {
                Ok(file_with("2026-10-13T10:00:00Z", 100, 1000))
            })
            .await
            .unwrap();
        assert_eq!(
            parsed.turns.len(),
            1,
            "the transcript comes back to the caller"
        );
        assert_eq!(
            store.lookup(agent_id),
            ContextState::Known(Some(figures(100, 1000)))
        );

        let other = Uuid::now_v7();
        let failed = store
            .parse_recording(other, async {
                Err(AppError::Internal("unreadable".to_owned()))
            })
            .await;
        assert!(failed.is_err(), "the error reaches the caller");
        assert_eq!(store.lookup(other), ContextState::Known(None));
    }
}
