//! Codex session-file lookup, parsing, and post-turn enrichment.
//!
//! After each turn's terminal stream event (`turn.completed` / `turn.failed`),
//! the Codex adapter reads the session file to fill in metadata the stream
//! omits. Per `docs/research/archive/codex-cli-observed.md`, the session file is
//! the **only** source for:
//! - `event_msg/task_started.payload.model_context_window` →
//!   `TurnEnd.usage.context_window` (per-turn).
//! - `event_msg/token_count.rate_limits` (non-null variant only) →
//!   `RateLimitEvent.info` (per-turn).
//! - `event_msg/token_count.info.last_token_usage` → per-turn token usage
//!   overlaid onto `TurnEnd.usage`. The stream's `turn.completed.usage` is
//!   **not** per-turn — codex-rs populates it from the thread-cumulative
//!   `total_token_usage` counter, restored from the rollout on resume
//!   (`exec/src/event_processor_with_jsonl_output.rs::usage_from_last_total`),
//!   so its numbers grow without bound across sends.
//! - `session_meta.payload.cli_version` → `SessionMeta.harness_version` (once
//!   per session).
//! - `turn_context.payload.model` (first one in file) → `SessionMeta.model`
//!   (once per session).
//! - Tool records for hydration: legacy `function_call` / standalone
//!   `apply_patch` calls, newer `exec` wrappers, and the structured
//!   `event_msg/patch_apply_end` shape shared by both rollout generations.
//!
//! ## ID-space distinction
//!
//! Switchboard `TurnId` is dispatcher-local (UUID v7 we generate). Codex
//! session-file `turn_id` is harness-local (UUID v7 Codex generates). The two
//! **never** match by design. Live post-turn enrichment still selects the
//! latest file turn rather than comparing either id space. Transcript
//! reconstruction separately uses Codex's file-local relation between
//! `item_completed.turn_id` and `task_started.turn_id` to route asynchronous
//! tool detail; that relation never becomes a frontend or dispatcher identity.
//!
//! ## Path resolution
//!
//! Codex session files live at
//! `<home>/.codex/sessions/<YYYY>/<MM>/<DD>/rollout-*-<session-uuid>.jsonl`.
//! **Codex partitions by local date, not UTC** — the directory key is
//! captured from `chrono::Local::now().date_naive()` at first dispatch and
//! stored in the sidecar as `session_partition_date`. Codex appends to the
//! original-partition file even on cross-day resumes; the stored date is
//! authoritative across local-date boundaries. **Never recompute the date
//! from any wall-clock function at enrichment time** — always read from
//! the sidecar. See `docs/research/archive/codex-cli-observed.md` for the
//! verification evidence and the fallback path if Codex ever changes
//! partition behavior.
//!
//! ## `raw` field policy
//!
//! `SessionMeta.raw` carries the `session_meta` line for future forward-compat
//! field promotion. Codex's `session_meta.payload.base_instructions.text` is
//! the entire model system prompt (5–20KB) — never UI-rendered, but included
//! in the unstripped raw it would dominate the IPC payload. Strip the `text`
//! field of `base_instructions` to a sentinel; preserve the rest of the
//! envelope verbatim so the surrounding shape stays observable.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::Value;
use switchboard_core::AgentId;
use uuid::Uuid;

use crate::events::{ContentKind, McpServerStatus, ToolKind, TurnId, TurnUsage};
use crate::transcript::{
    LoadTranscriptError, LoadedTranscript, ParseWarning, SessionMetaInfo, Turn, TurnItem,
    TurnStatus, merge_meta_with_loaders,
};

use super::config::load_mcp_servers;
use super::skills::load_skills;

/// Per-attempt backoff between session-file read tries. Codex writes the
/// session file synchronously per `docs/research/archive/codex-cli-observed.md`; by
/// the time the terminal stream event arrives, the file should already be
/// on disk. The first attempt fires **immediately** — the backoff applies
/// only between failed attempts, so a typical turn pays zero latency. Two
/// backoffs across three total attempts cap worst-case enrichment latency at
/// 400ms before giving up. Tune downward only with empirical evidence.
pub const ENRICHMENT_RETRY_DELAY_MS: u64 = 200;

/// What enrichment extracted from the session file. All fields optional —
/// any subset may be missing if the file isn't readable or doesn't carry the
/// expected records. The adapter degrades gracefully per-field.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Enrichment {
    /// From the last `event_msg/task_started` record in the file. Used to
    /// fill `TurnEnd.usage.context_window`.
    pub context_window: Option<u32>,
    /// From the last `event_msg/token_count` record with non-null
    /// `rate_limits`. Used as `RateLimitEvent.info`. Carried as raw JSON
    /// because the rate-limit shape is "opaque to consumers" per
    /// `docs/system-design.md`.
    pub rate_limits: Option<Value>,
    /// From `session_meta.payload.cli_version` (line 1). Used for
    /// `SessionMeta.harness_version`.
    pub cli_version: Option<String>,
    /// From the first `turn_context.payload.model` in the file. Used for
    /// `SessionMeta.model`. Codex supports per-turn model overrides; the
    /// first-turn model is the authoritative session-level snapshot for
    /// `SessionMeta`.
    pub model: Option<String>,
    /// From the **last** `turn_context.payload.model` / `.effort` in the file —
    /// the *current* turn's selection, used to stamp the live per-turn
    /// `TurnEnd.{model,effort}`. Distinct from `model` (first-wins, agent-scoped
    /// `SessionMeta`). The readback effort field is `effort`, not
    /// `model_reasoning_effort` (verified @ codex 0.137.0).
    pub current_turn_model: Option<String>,
    pub current_turn_effort: Option<String>,
    /// The **current turn's** `turn_context.turn_id` — the durable per-turn key,
    /// the same field the reload parser stamps on `Turn::Agent.hydration_key`.
    /// The live adapter stamps it on `TurnEnd.first_message_id` so the dispatcher
    /// writes a `TurnLink` for the send↔turn key-join; because it is read from the
    /// same on-disk record the parser reads, live-key == parsed-key holds **by
    /// construction** (no live-stream parity gamble — the live stream carries
    /// `task_started.turn_id`, a different id the parser deliberately rejects).
    /// **Turn-scoped:** reset to `None` at each `task_started` (like
    /// [`Self::per_turn_usage`]), so a turn that writes no `turn_context` reads as
    /// `None`, never a predecessor turn's id — a stale key would mis-link a new
    /// turn to an old send.
    pub current_turn_id: Option<String>,
    /// The **last `task_started` record's timestamp** — when the file's current
    /// (tail) turn began. The reset at `task_started` protects
    /// [`Self::current_turn_id`] only once *this* turn's `task_started` has hit
    /// the file; on a resumed session read before that (the cancel path's
    /// fast-cancel window), the tail turn is the *previous* dispatch and its
    /// key is still resident. This timestamp is what lets the cancel path prove
    /// the tail turn is its own — the key is trusted only when the tail turn
    /// started at or after this dispatch. `None` when no `task_started` exists
    /// or the record carries no parseable timestamp (warned — a format change
    /// there would otherwise silently retire cancel-path identity recovery).
    pub current_turn_started_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The full `session_meta` line as JSON, with
    /// `payload.base_instructions.text` replaced by a sentinel. Used as
    /// `SessionMeta.raw`. `None` if line 1 isn't a `session_meta` record.
    pub session_meta_raw: Option<Value>,
    /// From the last `event_msg/token_count` record with non-null `info`
    /// **within the current turn** (reset at each `task_started`, mirroring
    /// the reload path's per-turn builder) — `info.last_token_usage` is the
    /// final request's usage, i.e. the true per-turn context occupancy (see
    /// module docs for why the stream's `turn.completed.usage` cannot
    /// serve). Same source the reload path uses, so live and reloaded turns
    /// carry identical telemetry. `None` when the current turn wrote no
    /// parseable usage record — never a predecessor turn's value.
    /// `context_window` is left `None` here; the adapter overlays it
    /// separately from the `task_started`-derived [`Self::context_window`].
    pub per_turn_usage: Option<TurnUsage>,
    /// The **current turn's** content-bearing `Edit` facets, in record order —
    /// mode-selected single source: legacy `apply_patch` calls /
    /// `patch_apply_end` events on legacy rollouts, `item_completed/FileChange`
    /// items on paginated ones.
    /// Turn-scoped (reset at each `task_started`, like
    /// [`Self::per_turn_usage`]). The adapter zips these onto the turn's live
    /// `file_change` tool ids and emits `ToolFacetUpdated` — rollout records are
    /// the only place a Codex edit's content exists (harness-behavior §3.6).
    pub patch_facets: Vec<crate::facets::ToolFacet>,
}

/// Compute the canonical session-file path glob's parent directory for a
/// given start date. Layout: `<home>/.codex/sessions/YYYY/MM/DD/`.
/// `%Y` / `%m` / `%d` already zero-pad to the expected widths.
#[must_use]
pub fn session_directory(home_dir: &Path, session_partition_date: NaiveDate) -> PathBuf {
    home_dir
        .join(".codex")
        .join("sessions")
        .join(session_partition_date.format("%Y").to_string())
        .join(session_partition_date.format("%m").to_string())
        .join(session_partition_date.format("%d").to_string())
}

/// Locate the session file for `session_id` under `home_dir` for the given
/// original-start-date. Codex's filenames are
/// `rollout-<timestamp>-<session-uuid>.jsonl`; the only unknown is the
/// timestamp, so we match by suffix. Returns `None` if the directory or file
/// is absent.
///
/// On multi-match (very rare — would require a backup/rename or Codex bug,
/// since session UUIDs are unique by construction), picks the file with the
/// **latest mtime**, falling back to lexicographic order if mtime is
/// unavailable. The "newest wins" rule avoids silently enriching from a
/// stale duplicate.
///
/// A `glob` crate dep is unnecessary for one suffix-match pattern — a single
/// `read_dir` + suffix filter is simpler, has no allocations beyond the
/// filename strings, and avoids pulling in a transitive dep tree.
#[must_use]
pub fn locate_session_file(
    home_dir: &Path,
    session_partition_date: NaiveDate,
    session_id: &str,
) -> Option<PathBuf> {
    let dir = session_directory(home_dir, session_partition_date);
    let entries = std::fs::read_dir(&dir).ok()?;
    let suffix = format!("-{session_id}.jsonl");
    let mut matches: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && name.starts_with("rollout-")
            && name.ends_with(&suffix)
        {
            matches.push(path);
        }
    }
    pick_newest(matches)
}

/// Choose the most-recent path by mtime. Falls back to the lexicographically
/// largest filename if mtime can't be read (filesystems without timestamp
/// support, permission edge cases) — Codex's `rollout-<timestamp>-` filename
/// prefix happens to make lex-largest correlate with newest in practice.
fn pick_newest(mut matches: Vec<PathBuf>) -> Option<PathBuf> {
    match matches.len() {
        0 => None,
        1 => matches.pop(),
        _ => {
            matches.sort_by(|a, b| {
                let mtime_a = a.metadata().and_then(|m| m.modified()).ok();
                let mtime_b = b.metadata().and_then(|m| m.modified()).ok();
                match (mtime_a, mtime_b) {
                    (Some(ma), Some(mb)) => ma.cmp(&mb),
                    _ => a.file_name().cmp(&b.file_name()),
                }
            });
            matches.pop() // largest after ascending sort
        }
    }
}

/// Error from `find_codex_session_file_for_attach`. Distinct from
/// `locate_session_file`'s "newest-mtime-wins" silent contract because the
/// attach flow commits a Switchboard agent to one specific session file for
/// its lifetime — picking arbitrarily on a multi-match (or silently failing
/// on a miss) would bind to the wrong harness session and violate the
/// session-id-uniqueness invariant.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AttachLookupError {
    /// No `rollout-*-<session_id>.jsonl` file exists under
    /// `~/.codex/sessions/*/*/*/`.
    #[error("no Codex session file found for session_id {session_id}")]
    NotFound { session_id: String },
    /// More than one `rollout-*-<session_id>.jsonl` file exists across the
    /// date partitions. Impossible by Codex's design (UUIDs are unique by
    /// construction); a real occurrence implies external anomaly (manual copy,
    /// FS corruption). Surface to the user rather than picking arbitrarily.
    #[error("ambiguous Codex session file for session_id {session_id}: {} candidates", paths.len())]
    Ambiguous {
        session_id: String,
        paths: Vec<PathBuf>,
    },
}

/// Locate the Codex session file for an *existing* `session_id`, scanning
/// **all** date partitions under `~/.codex/sessions/`. Returns the file path
/// and the parsed `YYYY-MM-DD` from the directory tree (load-bearing for the
/// attach-flow sidecar's `session_partition_date`).
///
/// **Distinct from `locate_session_file`.** `locate_session_file` is used by
/// post-turn enrichment, where the agent has already committed to a
/// `session_id` + date pair (the sidecar carries both); silently picking
/// newest-mtime on a duplicate is acceptable. This attach helper is used
/// **before** registration commits, and the user is choosing which file to
/// bind to — silent dup resolution would bind to the wrong file. Fail loud.
///
/// Scan strategy: `read_dir × 3` over `<home>/.codex/sessions/YYYY/MM/DD/`.
/// Non-numeric directory names are silently skipped (defensive against
/// `.DS_Store` and similar). The whole scan errors only if the root
/// `~/.codex/sessions/` directory is unreadable; per-leaf read failures are
/// skipped so a single permission-denied date dir doesn't blanket-fail the
/// lookup.
pub fn find_codex_session_file_for_attach(
    home_dir: &Path,
    session_id: &str,
) -> Result<(PathBuf, NaiveDate), AttachLookupError> {
    let root = home_dir.join(".codex").join("sessions");
    let suffix = format!("-{session_id}.jsonl");
    let mut matches: Vec<(PathBuf, NaiveDate)> = Vec::new();

    let Ok(year_entries) = std::fs::read_dir(&root) else {
        return Err(AttachLookupError::NotFound {
            session_id: session_id.to_owned(),
        });
    };
    for year_entry in year_entries.flatten() {
        let Some(year) = parse_numeric_dir(&year_entry, 4) else {
            continue;
        };
        let Ok(month_entries) = std::fs::read_dir(year_entry.path()) else {
            continue;
        };
        for month_entry in month_entries.flatten() {
            let Some(month) = parse_numeric_dir(&month_entry, 2) else {
                continue;
            };
            let Ok(day_entries) = std::fs::read_dir(month_entry.path()) else {
                continue;
            };
            for day_entry in day_entries.flatten() {
                let Some(day) = parse_numeric_dir(&day_entry, 2) else {
                    continue;
                };
                let Some(date) =
                    NaiveDate::from_ymd_opt(i32::from(year), u32::from(month), u32::from(day))
                else {
                    continue;
                };
                let Ok(file_entries) = std::fs::read_dir(day_entry.path()) else {
                    continue;
                };
                for file_entry in file_entries.flatten() {
                    let path = file_entry.path();
                    if let Some(name) = path.file_name().and_then(|n| n.to_str())
                        && name.starts_with("rollout-")
                        && name.ends_with(&suffix)
                    {
                        matches.push((path, date));
                    }
                }
            }
        }
    }

    match matches.len() {
        0 => Err(AttachLookupError::NotFound {
            session_id: session_id.to_owned(),
        }),
        1 => Ok(matches.into_iter().next().expect("len==1 guaranteed")),
        _ => {
            // Sort for stable error output.
            matches.sort_by(|a, b| a.0.cmp(&b.0));
            Err(AttachLookupError::Ambiguous {
                session_id: session_id.to_owned(),
                paths: matches.into_iter().map(|(p, _)| p).collect(),
            })
        }
    }
}

/// Parse a directory-entry name as a fixed-width zero-padded numeric (year=4,
/// month/day=2). Returns None for non-numeric names (`.DS_Store`, `Thumbs.db`,
/// stray files, etc.) and for unexpected widths. `u16` accommodates 4-digit
/// years through 9999 — well past any realistic session date.
fn parse_numeric_dir(entry: &std::fs::DirEntry, expected_width: usize) -> Option<u16> {
    let name = entry.file_name();
    let name_str = name.to_str()?;
    if name_str.len() != expected_width || !name_str.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    name_str.parse().ok()
}

/// Read and parse the session file. Returns `Enrichment::default()` (all
/// `None`) on any IO error or top-level corruption — per the harness-owned
/// file skip-with-warning invariant in `AGENTS.md`. Individual malformed
/// lines are warned-and-skipped, valid lines preserved.
#[must_use]
pub fn parse_session_file(path: &Path) -> Enrichment {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Codex session-file read failed; enrichment degraded"
            );
            return Enrichment::default();
        }
    };
    parse_session_content(&content)
}

/// Parse already-loaded session-file content. Exposed for testing without
/// the FS read.
/// Extract a `turn_context` record's `turn_id` — Codex's durable per-turn key.
///
/// **Load-bearing that this is the single source.** Both keying paths read it:
/// the reload parser stamps it on `Turn::Agent.hydration_key`, and the live
/// enrichment stamps it on `TurnEnd.first_message_id` (via
/// [`Enrichment::current_turn_id`]). The M3 design's "live-key == parsed-key by
/// construction" guarantee holds only because both call *this* one function — if
/// the two extractions ever diverged, a Codex turn would silently mis-link to the
/// wrong send. Deliberately **not** `task_started.turn_id` (a different id whose
/// per-turn uniqueness is unconfirmed).
fn turn_context_turn_id(payload: &Value) -> Option<String> {
    payload
        .get("turn_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Flatten the text of a paginated `UserMessage` / `AgentMessage` item's
/// content blocks. `None` means the record is malformed — `content` missing or
/// not an array — which the caller warns on; `Some` is the flattened text,
/// possibly empty.
///
/// **Reads each block's `text` field rather than gating on its `type` tag.**
/// Codex is not consistent about that tag's casing — a `UserMessage` block is
/// `{"type":"text"}` while an `AgentMessage` block is `{"type":"Text"}`, in the
/// same file — so matching on it would silently drop one side of every
/// conversation. Non-text blocks (images, audio, skills) carry no `text` field
/// and contribute nothing, which needs no tag inventory to stay correct.
///
/// Blocks are joined with **no separator**, matching Codex's own canonical
/// flattening (`UserMessageItem::message()` is a `.join("")`) — the legacy
/// `user_message`/`agent_message` records are generated from that same
/// flattening, so any separator here would make the two generations hydrate
/// differently.
///
/// The trust boundary is deliberate: only a missing/non-array `content` reads
/// as malformed. An array whose blocks are all unrecognized shapes flattens to
/// `Some("")` — indistinguishable from genuinely-empty, consistent with the
/// parser's unknown-types-skip-silently posture.
fn item_message_text(item: &Value) -> Option<String> {
    let blocks = item.get("content").and_then(Value::as_array)?;
    Some(
        blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .concat(),
    )
}

/// Which generation of rollout a session file is, read from
/// `session_meta.history_mode`. This field — never a CLI-version comparison —
/// is the durable predicate: a pagination-capable Codex still writes `Legacy`
/// files whenever its store rejects pagination, and resuming a pre-flip thread
/// keeps writing legacy records indefinitely.
///
/// [`Missing`](Self::Missing) and [`Unknown`](Self::Unknown) are deliberately
/// distinct even though both parse through the legacy path. Missing means "file
/// predates the field", which is the overwhelmingly common case and entirely
/// unremarkable. Unknown means Codex has introduced a *third* persistence
/// contract — and if that contract drops the records the legacy path reads, the
/// way `Paginated` dropped them, hydration silently returns nothing. Collapsing
/// the two would reproduce exactly the failure this whole module exists to fix,
/// so `Unknown` always leaves a `ParseWarning` behind.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HistoryMode {
    /// No `history_mode` field — a rollout written before Codex added it.
    Missing,
    Legacy,
    Paginated,
    /// A value this build does not recognize. Parsed as legacy (the
    /// conservative guess: never fabricate a reading of an unknown contract),
    /// but always surfaced.
    Unknown(String),
}

impl HistoryMode {
    /// Read from a `session_meta` record's payload.
    ///
    /// Only a genuinely **absent** field is `Missing`. Upstream declares the
    /// persisted field as a non-optional `#[serde(default)] ThreadHistoryMode`
    /// — Codex writes a string or (pre-field versions) nothing at all, never
    /// `null` — so a present-but-non-string value, `null` included, is a
    /// changed contract and must take the warned `Unknown` path. Classifying
    /// it as `Missing` would let a representation change slip past the
    /// tripwire exactly the way the paginated flip did.
    fn from_session_meta(payload: &Value) -> Self {
        match payload.get("history_mode") {
            None => Self::Missing,
            Some(Value::String(s)) => match s.as_str() {
                "legacy" => Self::Legacy,
                "paginated" => Self::Paginated,
                other => Self::Unknown(other.to_owned()),
            },
            Some(other) => Self::Unknown(other.to_string()),
        }
    }

    /// Whether `event_msg/item_completed` carries this file's prompt, answer,
    /// and tool detail. Only `Paginated` routes through that channel; every
    /// other mode (including `Unknown`) reads the legacy `event_msg` records.
    fn reads_item_completed(&self) -> bool {
        matches!(self, Self::Paginated)
    }
}

/// Read the enrichment-relevant fields off a `session_meta` payload and return
/// the file's [`HistoryMode`] — the patch-facet extraction below branches on it.
///
/// This reader has no `ParseWarning` channel (it feeds live turn-end
/// enrichment, not a `LoadedTranscript`), so an unrecognized `history_mode` is
/// surfaced through tracing instead. Same rationale as the reconstruction path:
/// an unknown persistence contract must never pass silently.
fn absorb_session_meta(payload: &Value, enrichment: &mut Enrichment) -> HistoryMode {
    if let Some(version) = payload.get("cli_version").and_then(Value::as_str) {
        enrichment.cli_version = Some(version.to_owned());
    }
    let mode = HistoryMode::from_session_meta(payload);
    if let HistoryMode::Unknown(value) = &mode {
        tracing::warn!(
            history_mode = %value,
            "Codex session-file: unrecognized history_mode; \
             reading as legacy — enrichment may be incomplete"
        );
    }
    mode
}

/// The record-level timestamp of a `task_started` line — the freshness proof
/// for cancel-path identity recovery ([`Enrichment::current_turn_started_at`]).
/// Absent/unparseable reads as `None` — **fail-closed** (the freshness check
/// rejects, no identity is recovered). Deliberately silent here: this parse
/// runs on every enrichment read, where freshness is never consulted; the one
/// consumer that consults it (the Codex cancel path) owns the
/// missing-timestamp breadcrumb, so the warn fires exactly when missing data
/// prevented an otherwise-possible recovery — never about values nothing read.
fn task_started_timestamp(value: &Value) -> Option<chrono::DateTime<chrono::Utc>> {
    value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<chrono::DateTime<chrono::Utc>>().ok())
}

#[must_use]
pub fn parse_session_content(content: &str) -> Enrichment {
    let mut enrichment = Enrichment::default();
    let mut model_set = false; // first-turn_context wins (set-once gate)
    let mut patch_call_ids: Vec<Option<String>> = Vec::new();
    // Codex writes `session_meta` first, so the mode is known before any patch
    // record arrives. Patch facets are single-sourced per mode, mirroring the
    // reconstruction path's text gates.
    let mut history_mode = HistoryMode::Missing;
    // Running shell cwd (turn_context precedes the turn's tool records) —
    // resolves relative apply_patch paths; observed paths are absolute.
    let mut current_cwd: Option<std::path::PathBuf> = None;

    for (idx, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    line = idx + 1,
                    error = %e,
                    "Codex session-file: malformed JSON line; skipping"
                );
                continue;
            }
        };

        let record_type = value.get("type").and_then(Value::as_str).unwrap_or("");
        let payload = value.get("payload");

        match record_type {
            "session_meta" => {
                if let Some(p) = payload {
                    history_mode = absorb_session_meta(p, &mut enrichment);
                }
                enrichment.session_meta_raw = Some(strip_base_instructions(value));
            }
            "turn_context" => {
                if let Some(p) = payload {
                    let model = p.get("model").and_then(Value::as_str);
                    // First-wins → agent-scoped `SessionMeta.model`.
                    if let Some(m) = model
                        && !model_set
                    {
                        enrichment.model = Some(m.to_owned());
                        model_set = true;
                    }
                    // Per-turn carrier = **exactly this record's** values (reset,
                    // not carry-until-overwritten) so a turn can never inherit a
                    // prior turn's selection. Readback effort field is `effort`,
                    // not `model_reasoning_effort`. Codex currently always writes
                    // both, but absence must mean `None`, not stale.
                    enrichment.current_turn_model = model.map(str::to_owned);
                    enrichment.current_turn_effort =
                        p.get("effort").and_then(Value::as_str).map(str::to_owned);
                    // The durable per-turn key — same field (and same helper) the
                    // reload parser reads for `hydration_key`. Set here, reset at
                    // `task_started` below.
                    enrichment.current_turn_id = turn_context_turn_id(p);
                    current_cwd = p
                        .get("cwd")
                        .and_then(Value::as_str)
                        .map(std::path::PathBuf::from);
                }
            }
            // Gated like the reconstruction path's text arms: patch facets
            // are single-sourced per mode. On a paginated file the
            // `item_completed/FileChange` items below are canonical; letting a
            // standalone legacy-shaped `apply_patch` call also contribute
            // would double a patch into the ordinal facet list.
            "response_item" if !history_mode.reads_item_completed() => {
                capture_legacy_patch_facet(
                    payload,
                    current_cwd.as_deref(),
                    &mut enrichment,
                    &mut patch_call_ids,
                );
            }
            "event_msg" => {
                let Some(p) = payload else { continue };
                match p.get("type").and_then(Value::as_str).unwrap_or("") {
                    "task_started" => {
                        // Last-task_started-wins. On resumed sessions the
                        // file accumulates one task_started per turn; the
                        // current turn's is the most recent.
                        if let Some(window) = p.get("model_context_window").and_then(Value::as_u64)
                        {
                            enrichment.context_window = u32::try_from(window).ok();
                        }
                        // `per_turn_usage` is turn-scoped: a turn that writes
                        // no parseable token_count must read as "unknown",
                        // never inherit its predecessor's numbers. The reset
                        // also covers the flush race (enrichment reading after
                        // task_started but before this turn's token_count
                        // lands). Window/rate-limits deliberately stay
                        // whole-file last-wins — they are session-level
                        // "latest known" state, not per-turn telemetry.
                        enrichment.per_turn_usage = None;
                        // Turn-scoped, same reason: a turn with no `turn_context`
                        // must read as no-key, never inherit a predecessor's id
                        // (a stale key would mis-link a new turn to an old send).
                        // **Correctness depends on record order:** this reset works
                        // only because `turn_context` is written *after* `task_started`
                        // within a turn — if Codex ever reordered them, the reset would
                        // wipe the current turn's key and every Codex turn would go
                        // keyless → positional. Guarded live by
                        // `live_codex_hydration_key_matches_live_turn_end`.
                        enrichment.current_turn_id = None;
                        // When the file's current turn began — the freshness
                        // proof the cancel path's identity recovery compares
                        // against its dispatch instant (see the field doc).
                        enrichment.current_turn_started_at = task_started_timestamp(&value);
                        // Turn-scoped for the same reason: the facet upgrade
                        // must never replay a *previous* turn's patches onto
                        // this turn's file_change rows.
                        enrichment.patch_facets.clear();
                        patch_call_ids.clear();
                    }
                    "token_count" => {
                        // Two variants share this type; each feeds a different
                        // enrichment field and either may be null on a given
                        // record. Last-record-wins for both, independently.
                        if let Some(rate_limits) = p.get("rate_limits")
                            && !rate_limits.is_null()
                        {
                            enrichment.rate_limits = Some(rate_limits.clone());
                        }
                        if let Some(usage) = p
                            .get("info")
                            .filter(|v| !v.is_null())
                            .and_then(|info| turn_usage_from_token_count_info(info, None))
                        {
                            enrichment.per_turn_usage = Some(usage);
                        }
                    }
                    "patch_apply_end" if !history_mode.reads_item_completed() => {
                        capture_patch_apply_end_facet(p, &mut enrichment, &mut patch_call_ids);
                    }
                    "item_completed" if history_mode.reads_item_completed() => {
                        capture_paginated_patch_facet(p, &mut enrichment, &mut patch_call_ids);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    enrichment
}

/// Legacy rollouts' edit-content source: `patch_apply_end`, upserted by
/// `call_id` so a generation-1 standalone `apply_patch` call's facet is
/// replaced (not duplicated) when its structured result arrives.
fn capture_patch_apply_end_facet(
    payload: &Value,
    enrichment: &mut Enrichment,
    patch_call_ids: &mut Vec<Option<String>>,
) {
    let facet = super::facets::patch_apply_end_facet(payload);
    if matches!(facet, crate::facets::ToolFacet::Edit { .. }) {
        upsert_patch_facet(
            &mut enrichment.patch_facets,
            patch_call_ids,
            payload.get("call_id").and_then(Value::as_str),
            facet,
        );
    }
}

/// Paginated rollouts' edit-content source: an `item_completed/FileChange`
/// carries the same `changes` map `patch_apply_end` did — same facet builder,
/// so both generations upgrade the live row identically. Appended in record
/// order: `emit_facet_upgrades` pairs live rows to these ordinally with a
/// path-set guard, never by id. Counts stay aligned with the live rows because
/// a failed `apply_patch` emits neither a live `file_change` row nor a
/// `FileChange` item (fact 3 — the failure lives only on the wrapper output).
/// Whether a *declined* patch emits an item or a live row is unprobed; if the
/// counts ever desync, `emit_facet_upgrades`' path-set fallback is the
/// backstop (fail-soft, matching legacy's exposure). The converse gap — a
/// paginated file carrying only a legacy-shaped standalone `apply_patch` call
/// — deliberately yields zero facets here (the legacy source is mode-gated
/// off), leaving the live row paths-only until reopen recovers the content.
fn capture_paginated_patch_facet(
    payload: &Value,
    enrichment: &mut Enrichment,
    patch_call_ids: &mut Vec<Option<String>>,
) {
    let Some(item) = payload.get("item") else {
        return;
    };
    if item.get("type").and_then(Value::as_str) != Some("FileChange") {
        return;
    }
    let facet = super::facets::patch_apply_end_facet(item);
    if matches!(facet, crate::facets::ToolFacet::Edit { .. }) {
        enrichment.patch_facets.push(facet);
        patch_call_ids.push(None);
    }
}

fn capture_legacy_patch_facet(
    payload: Option<&Value>,
    current_cwd: Option<&std::path::Path>,
    enrichment: &mut Enrichment,
    patch_call_ids: &mut Vec<Option<String>>,
) {
    let Some(payload) = payload else {
        return;
    };
    if payload.get("type").and_then(Value::as_str) != Some("custom_tool_call")
        || payload.get("name").and_then(Value::as_str) != Some("apply_patch")
    {
        return;
    }
    let Some(input) = payload.get("input").and_then(Value::as_str) else {
        return;
    };
    let Some(files) = super::facets::parse_apply_patch(input, current_cwd) else {
        return;
    };
    upsert_patch_facet(
        &mut enrichment.patch_facets,
        patch_call_ids,
        payload.get("call_id").and_then(Value::as_str),
        crate::facets::ToolFacet::Edit { files },
    );
}

fn upsert_patch_facet(
    facets: &mut Vec<crate::facets::ToolFacet>,
    call_ids: &mut Vec<Option<String>>,
    call_id: Option<&str>,
    facet: crate::facets::ToolFacet,
) {
    if let Some(call_id) = call_id
        && let Some(index) = call_ids
            .iter()
            .position(|existing| existing.as_deref() == Some(call_id))
    {
        facets[index] = facet;
        return;
    }
    facets.push(facet);
    call_ids.push(call_id.map(str::to_owned));
}

/// Build a per-turn `TurnUsage` from a `token_count` record's non-null
/// `info`. `info.last_token_usage` is the final request of the turn, so its
/// input side IS the context occupancy — Codex's `cached_input_tokens` is a
/// subset of `input_tokens`, so no summation (see `TurnUsage` docs). The
/// fall-back to reading token fields off `info` itself is a defensive
/// legacy branch inherited from the reload parser (which reads historical
/// rollouts): no current record carries token fields at `info`'s top level,
/// so absent `last_token_usage` this returns `None` in practice. Shared by
/// the reload path and post-turn enrichment so both derive identical
/// telemetry.
///
/// Missing/non-numeric `input_tokens` or `output_tokens` → `None` — same
/// "no fabricated zero-Some" contract as the live parser. Load-bearing for
/// the enrichment overlay: a degenerate record must not replace genuine
/// stream telemetry with zeros.
fn turn_usage_from_token_count_info(
    info: &Value,
    context_window: Option<u32>,
) -> Option<TurnUsage> {
    let last = info.get("last_token_usage").unwrap_or(info);
    let input = last.get("input_tokens").and_then(Value::as_u64)?;
    let output = last.get("output_tokens").and_then(Value::as_u64)?;
    let context_tokens_after_turn = input.checked_add(output);
    if context_tokens_after_turn.is_none() {
        tracing::warn!(
            "Codex session context-token arithmetic overflow — context utilization unavailable"
        );
    }
    Some(TurnUsage {
        input_tokens: input,
        output_tokens: output,
        cached_input_tokens: last.get("cached_input_tokens").and_then(Value::as_u64),
        cache_creation_input_tokens: None,
        context_input_tokens: Some(input),
        context_tokens_after_turn,
        reasoning_output_tokens: last.get("reasoning_output_tokens").and_then(Value::as_u64),
        context_window,
        total_cost_usd: None,
    })
}

/// Strip `payload.base_instructions.text` from a `session_meta` record. The
/// surrounding envelope is preserved verbatim so future consumers can still
/// introspect the field's existence and the `base_instructions` table's
/// other keys. Returns a clone — the caller owns the result.
fn strip_base_instructions(mut value: Value) -> Value {
    if let Some(payload) = value.get_mut("payload")
        && let Some(base) = payload.get_mut("base_instructions")
        && let Some(text) = base.get_mut("text")
        && text.is_string()
    {
        *text = Value::String("<stripped — see codex-cli-observed.md>".to_owned());
    }
    value
}

/// Hook trait for the retry loop's sleep. Production uses `TokioSleeper`;
/// tests inject a no-op or a counter to assert retry behavior without
/// wall-clock waits. Trivial trait — kept inline so the surface is local.
#[async_trait::async_trait]
pub trait Sleeper: Send + Sync {
    async fn sleep(&self, duration: Duration);
}

/// Production [`Sleeper`] — wraps `tokio::time::sleep`.
pub struct TokioSleeper;

#[async_trait::async_trait]
impl Sleeper for TokioSleeper {
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

/// Locate and parse the session file with bounded retries. Attempts the
/// read immediately; on miss, sleeps `ENRICHMENT_RETRY_DELAY_MS` and retries,
/// up to a total of three attempts (two backoffs). On all-miss returns
/// `Enrichment::default()` and logs a warning — the adapter then emits
/// `TurnEnd` with `context_window: None` and no enrichment-derived events.
///
/// **Typical-case latency: 0ms.** Codex writes the session file
/// synchronously and the file is usually on disk by terminal-event time;
/// only flush-latency edge cases trigger the retries.
pub async fn load_with_retry(
    home_dir: &Path,
    session_partition_date: NaiveDate,
    session_id: &str,
    sleeper: &dyn Sleeper,
) -> Enrichment {
    const ATTEMPTS: usize = 3;
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            sleeper
                .sleep(Duration::from_millis(ENRICHMENT_RETRY_DELAY_MS))
                .await;
        }
        if let Some(path) = locate_session_file(home_dir, session_partition_date, session_id) {
            return parse_session_file(&path);
        }
    }
    tracing::warn!(
        session_id = %session_id,
        date = %session_partition_date,
        "Codex session file not found after retry; TurnEnd will lack enriched fields"
    );
    Enrichment::default()
}

/// Project the enrichment + scoped registries onto a `SessionMeta` event
/// payload. Returns `None` if neither model nor `cli_version` was extracted
/// (the file was unreadable or contained no recognizable records) — emitting
/// a `SessionMeta` with both required fields empty would carry no information.
#[must_use]
pub fn build_session_meta_fields(
    enrichment: &Enrichment,
    mcp_servers: Vec<McpServerStatus>,
    skills: Vec<String>,
) -> Option<SessionMetaFields> {
    if enrichment.model.is_none() && enrichment.cli_version.is_none() {
        return None;
    }
    Some(SessionMetaFields {
        model: enrichment.model.clone().unwrap_or_default(),
        harness_version: enrichment.cli_version.clone().unwrap_or_default(),
        mcp_servers,
        skills,
        raw: enrichment.session_meta_raw.clone().unwrap_or(Value::Null),
    })
}

/// Fields ready to plug into [`crate::events::AdapterEvent::SessionMeta`].
/// `tools` is always `vec![]` for Codex — no equivalent registry source on
/// disk; kept implicit on the adapter side rather than carried here.
pub struct SessionMetaFields {
    pub model: String,
    pub harness_version: String,
    pub mcp_servers: Vec<McpServerStatus>,
    pub skills: Vec<String>,
    pub raw: Value,
}

/// Load a Codex session file and project it into a
/// [`crate::transcript::LoadedTranscript`]. Used by transcript hydration on
/// project open and on attach.
///
/// `session_partition_date` MUST come from the agent's registry locator
/// (`SessionLocator::Codex { partition_date, .. }`). Codex partitions session
/// files by **local date** at first dispatch and resumes append to the
/// original-partition file across local-date boundaries; the stored date is
/// authoritative — never recompute from `Local::today()`.
///
/// `cwd` is the user's bound working directory, used for project-scoped
/// MCP config and skill loaders (the same loaders live dispatch uses).
///
/// **Stale-locator case**: if `session_partition_date` is present but no
/// session file lives at the recorded path (user deleted it, external
/// rotation), returns `Ok(LoadedTranscript { turns: vec![], warnings: vec![<stale warning>] })`.
/// **Never-dispatched case** (agent created, no locator yet): caller passes
/// `None` for the date — returns `Ok(LoadedTranscript::default())` with no
/// warning.
pub fn load_codex_transcript(
    home_dir: &Path,
    cwd: &Path,
    session_id: &str,
    session_partition_date: Option<NaiveDate>,
    agent_id: AgentId,
) -> Result<LoadedTranscript, LoadTranscriptError> {
    let Some(date) = session_partition_date else {
        // Agent has no sidecar yet — created but never dispatched.
        // Surface meta (loaded from config files) even with empty turns
        // so the sidebar's model / registries populate the moment the
        // agent is selected.
        return Ok(LoadedTranscript {
            meta: Some(merge_meta_with_loaders(
                None,
                load_mcp_servers(home_dir, cwd),
                load_skills(home_dir, cwd),
            )),
            ..LoadedTranscript::default()
        });
    };

    let Some(path) = locate_session_file(home_dir, date, session_id) else {
        return Err(LoadTranscriptError::RecordedSessionUnavailable);
    };

    let content =
        std::fs::read_to_string(&path).map_err(|e| LoadTranscriptError::Io { path, source: e })?;

    let mut transcript = parse_codex_transcript_content(&content, agent_id);
    transcript.meta = Some(merge_meta_with_loaders(
        transcript.meta.take(),
        load_mcp_servers(home_dir, cwd),
        load_skills(home_dir, cwd),
    ));
    Ok(transcript)
}

/// Parse Codex session-file content into a `LoadedTranscript` (no FS access).
///
/// Paginated tool completions are associated in a file-local prepass, then
/// replayed beside their canonical wrapper call. This keeps wrapper rows as
/// the audit record while letting the existing interval handlers apply richer
/// status/output even when Codex persisted that detail late. Unprovable
/// ownership is warned and omitted rather than guessed.
/// The prepass retains only source-line slices and compact association facts;
/// large rollouts are never held as a second in-memory JSON document tree.
///
/// Exposed `pub(crate)` for unit tests that want to drive the parser without
/// staging a temp file.
pub(crate) fn parse_codex_transcript_content(content: &str, agent_id: AgentId) -> LoadedTranscript {
    let mut state = CodexReconstruction::new(agent_id);
    if initial_session_is_paginated(content) {
        ingest_paginated_content(content, &mut state);
    } else {
        ingest_streaming_content(content, &mut state);
    }
    let mut t = state.finalize();
    // Associated completions are dispatched beside their canonical wrapper,
    // but warnings retain source-file order for stable diagnostics.
    t.warnings.sort_by_key(|warning| warning.line_number);
    // Use the existing enrichment parser to extract model/cli_version/last
    // rate_limits, then merge into our LoadedTranscript shape. Single source
    // of truth for meta fields.
    let enrichment = parse_session_content(content);
    t.last_rate_limit = enrichment.rate_limits;
    t.meta = Some(SessionMetaInfo {
        model: enrichment.model.unwrap_or_default(),
        harness_version: enrichment.cli_version.unwrap_or_default(),
        tools: vec![],
        mcp_servers: vec![],
        skills: vec![],
    });
    t
}

/// Codex writes `session_meta` first. Only a confirmed paginated first record
/// pays for the association prepass; every other shape keeps the established
/// streaming fallback and lets reconstruction report malformed or unknown
/// metadata normally.
fn initial_session_is_paginated(content: &str) -> bool {
    let Some(first_record) = content.lines().find(|line| !line.trim().is_empty()) else {
        return false;
    };
    let Ok(record) = serde_json::from_str::<Value>(first_record) else {
        return false;
    };
    record.get("type").and_then(Value::as_str) == Some("session_meta")
        && record
            .get("payload")
            .and_then(|payload| payload.get("history_mode"))
            .and_then(Value::as_str)
            == Some("paginated")
}

fn ingest_streaming_content(content: &str, state: &mut CodexReconstruction) {
    for (idx, line) in content.lines().enumerate() {
        let line_number = idx + 1;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(record) => state.ingest(line_number, &record),
            Err(error) => state.warn(line_number, format!("malformed JSON: {error}")),
        }
    }
}

fn ingest_paginated_content(content: &str, state: &mut CodexReconstruction) {
    let lines: Vec<SessionLine<'_>> = content
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            let line_number = idx + 1;
            (!line.trim().is_empty()).then_some(SessionLine {
                line_number,
                source: line,
            })
        })
        .collect();
    let dispatch = ToolCompletionDispatch::build(&lines);

    for (record_index, line) in lines.iter().enumerate() {
        if let Some(disposition) = dispatch.completions.get(&record_index) {
            match disposition {
                ToolCompletionDisposition::Assigned => continue,
                ToolCompletionDisposition::Unowned(item_type) => {
                    state.warn(
                        line.line_number,
                        format!("{item_type} item outside any exec wrapper interval"),
                    );
                    continue;
                }
            }
        }
        let record = match line.parse() {
            Ok(record) => record,
            Err(error) => {
                state.warn(line.line_number, format!("malformed JSON: {error}"));
                continue;
            }
        };
        state.ingest(line.line_number, &record);
        if let Some(completions) = dispatch.after_wrapper_call.get(&record_index) {
            for &completion_index in completions {
                let completion_line = &lines[completion_index];
                if let Ok(completion) = completion_line.parse() {
                    state.ingest(completion_line.line_number, &completion);
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
struct SessionLine<'a> {
    line_number: usize,
    source: &'a str,
}

impl SessionLine<'_> {
    fn parse(&self) -> serde_json::Result<Value> {
        serde_json::from_str(self.source)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StructuredToolKind {
    CommandExecution,
    FileChange,
    McpToolCall,
}

impl StructuredToolKind {
    const fn label(self) -> &'static str {
        match self {
            Self::CommandExecution => "CommandExecution",
            Self::FileChange => "FileChange",
            Self::McpToolCall => "McpToolCall",
        }
    }

    const fn admits_post_output_fallback(self) -> bool {
        matches!(self, Self::CommandExecution | Self::McpToolCall)
    }
}

struct AssociationTurn {
    task_id: Option<String>,
    context_id: Option<String>,
    context_conflict: bool,
}

struct AssociationWrapper {
    record_index: usize,
    turn_index: usize,
    command_signature: Option<CommandSignature>,
    /// Planning-time slot state only. Replay still passes through
    /// `OpenWrapper::command_slot_taken`, whose mutation-time check remains
    /// authoritative and must not be merged with this prepass approximation.
    command_slot_taken: bool,
    attached_children: usize,
}

struct AssociationCompletion {
    record_index: usize,
    active_turn: Option<usize>,
    physical_wrapper: Option<usize>,
    positional_wrapper: Option<usize>,
    producer_turn_id: Option<String>,
    item_id: Option<String>,
    kind: StructuredToolKind,
    command_signature: Option<CommandSignature>,
}

struct AssociationFacts {
    turns: Vec<AssociationTurn>,
    wrappers: Vec<AssociationWrapper>,
    completions: Vec<AssociationCompletion>,
}

impl AssociationFacts {
    fn collect(lines: &[SessionLine<'_>]) -> Self {
        let mut turns = Vec::<AssociationTurn>::new();
        let mut wrappers = Vec::<AssociationWrapper>::new();
        let mut completions = Vec::<AssociationCompletion>::new();
        let mut wrapper_by_call_id = HashMap::<String, usize>::new();
        let mut current_turn = None;
        let mut physical_wrapper = None;
        let mut positional_wrapper = None;

        for (record_index, line) in lines.iter().enumerate() {
            let Ok(record) = line.parse() else {
                positional_wrapper = None;
                continue;
            };
            if is_task_started(&record) {
                turns.push(association_turn(&record));
                current_turn = Some(turns.len() - 1);
                physical_wrapper = None;
                positional_wrapper = None;
                continue;
            }
            if is_task_complete(&record) {
                current_turn = None;
                physical_wrapper = None;
                positional_wrapper = None;
                continue;
            }
            if let Some(context_id) = record_turn_context_id(&record) {
                if let Some(turn_index) = current_turn {
                    let turn = &mut turns[turn_index];
                    if turn
                        .context_id
                        .as_ref()
                        .is_some_and(|existing| existing != context_id)
                    {
                        turn.context_conflict = true;
                    } else {
                        turn.context_id = Some(context_id.to_owned());
                    }
                }
                positional_wrapper = None;
                continue;
            }
            if let Some((call_id, input)) = exec_wrapper_call(&record) {
                positional_wrapper = None;
                let Some(turn_index) = current_turn else {
                    physical_wrapper = None;
                    continue;
                };
                let command_signature =
                    decode_single_exec_wrapper(input).map(|decoded| decoded.command_signature);
                wrappers.push(AssociationWrapper {
                    record_index,
                    turn_index,
                    command_signature,
                    command_slot_taken: false,
                    attached_children: 0,
                });
                let wrapper_index = wrappers.len() - 1;
                wrapper_by_call_id.insert(call_id.to_owned(), wrapper_index);
                physical_wrapper = Some(wrapper_index);
                continue;
            }
            if let Some(call_id) = wrapper_output_call_id(&record) {
                positional_wrapper = wrapper_by_call_id.get(call_id).copied();
                if let Some(wrapper_index) = positional_wrapper
                    && physical_wrapper == Some(wrapper_index)
                {
                    physical_wrapper = None;
                }
                continue;
            }
            if let Some((kind, payload, item)) = structured_tool_completion(&record) {
                completions.push(AssociationCompletion {
                    record_index,
                    active_turn: current_turn,
                    physical_wrapper,
                    positional_wrapper,
                    producer_turn_id: payload
                        .get("turn_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    item_id: item.get("id").and_then(Value::as_str).map(str::to_owned),
                    command_signature: (kind == StructuredToolKind::CommandExecution)
                        .then(|| command_execution_signature(item))
                        .flatten(),
                    kind,
                });
                positional_wrapper = None;
                continue;
            }
            if !record_allows_post_output_candidate(&record) {
                positional_wrapper = None;
            }
        }

        Self {
            turns,
            wrappers,
            completions,
        }
    }
}

fn association_turn(record: &Value) -> AssociationTurn {
    AssociationTurn {
        task_id: record
            .get("payload")
            .and_then(|payload| payload.get("turn_id"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        context_id: None,
        context_conflict: false,
    }
}

enum ToolCompletionDisposition {
    Assigned,
    Unowned(&'static str),
}

#[derive(Default)]
struct ToolCompletionDispatch {
    after_wrapper_call: HashMap<usize, Vec<usize>>,
    completions: HashMap<usize, ToolCompletionDisposition>,
}

impl ToolCompletionDispatch {
    /// Associate paginated structured tool completions before reconstruction.
    ///
    /// Codex persists wrapper rows and structured results on asynchronous
    /// channels, so file order alone is not an ownership key. The resolver is
    /// deliberately file-local and fail-closed: producer turn ids must select
    /// one lifecycle, command identity is exact (never fuzzy), and unsupported
    /// ambiguity leaves the canonical wrapper untouched. Assigned completions
    /// are replayed immediately after their wrapper call so the existing
    /// `WrapperSlot` and structured-result handlers remain the only row
    /// mutation path.
    fn build(lines: &[SessionLine<'_>]) -> Self {
        let AssociationFacts {
            turns,
            mut wrappers,
            completions,
        } = AssociationFacts::collect(lines);

        let mut wrappers_by_command = HashMap::<(usize, String), Vec<usize>>::new();
        for (wrapper_index, wrapper) in wrappers.iter().enumerate() {
            if let Some(signature) = wrapper.command_signature.as_ref() {
                wrappers_by_command
                    .entry((wrapper.turn_index, signature.command.clone()))
                    .or_default()
                    .push(wrapper_index);
            }
        }

        let mut task_id_counts = HashMap::<&str, usize>::new();
        for turn in &turns {
            if let Some(task_id) = turn.task_id.as_deref() {
                *task_id_counts.entry(task_id).or_default() += 1;
            }
        }
        let mut turn_by_task_id = HashMap::<&str, usize>::new();
        for (turn_index, turn) in turns.iter().enumerate() {
            let Some(task_id) = turn.task_id.as_deref() else {
                continue;
            };
            if task_id_counts.get(task_id) == Some(&1)
                && !turn.context_conflict
                && turn
                    .context_id
                    .as_deref()
                    .is_none_or(|context_id| context_id == task_id)
            {
                turn_by_task_id.insert(task_id, turn_index);
            }
        }

        let mut dispatch = Self::default();
        let mut seen_completion_ids = HashSet::<String>::new();
        for completion in completions {
            let duplicate = completion
                .item_id
                .as_ref()
                .is_some_and(|item_id| !seen_completion_ids.insert(item_id.clone()));
            let target_turn = if duplicate {
                None
            } else if let Some(producer_turn_id) = completion.producer_turn_id.as_deref() {
                turn_by_task_id.get(producer_turn_id).copied()
            } else {
                completion.active_turn
            };

            let selected = target_turn.and_then(|turn_index| {
                select_completion_wrapper(&completion, turn_index, &wrappers, &wrappers_by_command)
            });
            let disposition = if let Some(wrapper_index) = selected {
                let wrapper = &mut wrappers[wrapper_index];
                wrapper.attached_children += 1;
                if completion.kind == StructuredToolKind::CommandExecution
                    && wrapper.command_signature.is_some()
                    && !wrapper.command_slot_taken
                {
                    wrapper.command_slot_taken = true;
                }
                dispatch
                    .after_wrapper_call
                    .entry(wrapper.record_index)
                    .or_default()
                    .push(completion.record_index);
                ToolCompletionDisposition::Assigned
            } else {
                ToolCompletionDisposition::Unowned(completion.kind.label())
            };
            dispatch
                .completions
                .insert(completion.record_index, disposition);
        }
        dispatch
    }
}

fn select_completion_wrapper(
    completion: &AssociationCompletion,
    turn_index: usize,
    wrappers: &[AssociationWrapper],
    wrappers_by_command: &HashMap<(usize, String), Vec<usize>>,
) -> Option<usize> {
    let mut semantic_ambiguity = false;
    if let Some(signature) = completion.command_signature.as_ref() {
        let exact: Vec<usize> = wrappers_by_command
            .get(&(turn_index, signature.command.clone()))
            .into_iter()
            .flatten()
            .copied()
            .filter(|&wrapper_index| {
                let wrapper = &wrappers[wrapper_index];
                wrapper.record_index < completion.record_index
                    && !wrapper.command_slot_taken
                    && wrapper
                        .command_signature
                        .as_ref()
                        .is_some_and(|candidate| command_signatures_match(candidate, signature))
            })
            .collect();
        if exact.len() == 1 {
            return exact.first().copied();
        }
        semantic_ambiguity = exact.len() > 1;
    }

    if let Some(wrapper_index) = completion.physical_wrapper {
        let wrapper = &wrappers[wrapper_index];
        let semantic_mismatch = completion
            .command_signature
            .as_ref()
            .is_some_and(|signature| {
                wrapper
                    .command_signature
                    .as_ref()
                    .is_some_and(|candidate| !command_signatures_match(candidate, signature))
            });
        let command_slot_unavailable = completion.kind == StructuredToolKind::CommandExecution
            && wrapper.command_signature.is_some()
            && wrapper.command_slot_taken;
        if wrapper.turn_index == turn_index
            && !semantic_ambiguity
            && !semantic_mismatch
            && !command_slot_unavailable
        {
            return Some(wrapper_index);
        }
    }

    if !completion.kind.admits_post_output_fallback() {
        return None;
    }
    // Once two wrappers expose the same exact command identity, neither an
    // open interval nor recency after output can prove which asynchronous
    // completion this is.
    if semantic_ambiguity {
        return None;
    }
    let wrapper_index = completion.positional_wrapper?;
    let wrapper = &wrappers[wrapper_index];
    let semantic_mismatch = completion
        .command_signature
        .as_ref()
        .is_some_and(|signature| {
            wrapper
                .command_signature
                .as_ref()
                .is_some_and(|candidate| !command_signatures_match(candidate, signature))
        });
    let command_slot_unavailable = completion.kind == StructuredToolKind::CommandExecution
        && wrapper.command_signature.is_some()
        && wrapper.command_slot_taken;
    (wrapper.turn_index == turn_index
        && wrapper.attached_children == 0
        && !semantic_mismatch
        && !command_slot_unavailable)
        .then_some(wrapper_index)
}

fn is_task_started(record: &Value) -> bool {
    record.get("type").and_then(Value::as_str) == Some("event_msg")
        && record
            .get("payload")
            .and_then(|payload| payload.get("type"))
            .and_then(Value::as_str)
            == Some("task_started")
}

fn is_task_complete(record: &Value) -> bool {
    record.get("type").and_then(Value::as_str) == Some("event_msg")
        && record
            .get("payload")
            .and_then(|payload| payload.get("type"))
            .and_then(Value::as_str)
            == Some("task_complete")
}

fn record_turn_context_id(record: &Value) -> Option<&str> {
    (record.get("type").and_then(Value::as_str) == Some("turn_context"))
        .then(|| record.get("payload")?.get("turn_id")?.as_str())
        .flatten()
}

fn exec_wrapper_call(record: &Value) -> Option<(&str, &str)> {
    let payload = record.get("payload")?;
    (record.get("type")?.as_str()? == "response_item"
        && payload.get("type")?.as_str()? == "custom_tool_call"
        && payload.get("name")?.as_str()? == "exec")
        .then(|| {
            Some((
                payload.get("call_id")?.as_str()?,
                payload.get("input")?.as_str()?,
            ))
        })
        .flatten()
}

fn wrapper_output_call_id(record: &Value) -> Option<&str> {
    let payload = record.get("payload")?;
    let item_type = payload.get("type")?.as_str()?;
    (record.get("type")?.as_str()? == "response_item"
        && matches!(
            item_type,
            "function_call_output" | "custom_tool_call_output"
        ))
    .then(|| payload.get("call_id")?.as_str())
    .flatten()
}

fn structured_tool_completion(record: &Value) -> Option<(StructuredToolKind, &Value, &Value)> {
    let payload = record.get("payload")?;
    if record.get("type")?.as_str()? != "event_msg"
        || payload.get("type")?.as_str()? != "item_completed"
    {
        return None;
    }
    let item = payload.get("item")?;
    let kind = match item.get("type")?.as_str()? {
        "CommandExecution" => StructuredToolKind::CommandExecution,
        "FileChange" => StructuredToolKind::FileChange,
        "McpToolCall" => StructuredToolKind::McpToolCall,
        _ => return None,
    };
    Some((kind, payload, item))
}

fn record_allows_post_output_candidate(record: &Value) -> bool {
    let record_type = record.get("type").and_then(Value::as_str);
    let payload = record.get("payload");
    match record_type {
        Some("event_msg") => match payload
            .and_then(|value| value.get("type"))
            .and_then(Value::as_str)
        {
            Some("token_count") => true,
            Some("item_completed") => {
                payload
                    .and_then(|value| value.get("item"))
                    .and_then(|item| item.get("type"))
                    .and_then(Value::as_str)
                    == Some("Reasoning")
            }
            _ => false,
        },
        Some("response_item") => {
            payload
                .and_then(|value| value.get("type"))
                .and_then(Value::as_str)
                == Some("reasoning")
        }
        _ => false,
    }
}

/// In-progress reconstruction state. Walks records in order, opening agent
/// turns on `task_started` and closing on `task_complete` or EOF.
struct CodexReconstruction {
    agent_id: AgentId,
    turns: Vec<Turn>,
    current_agent: Option<CodexAgentBuilder>,
    warnings: Vec<ParseWarning>,
    /// Model + effort from the most-recent `turn_context` record — Codex writes
    /// one per turn (at turn start), so when a turn closes these hold that
    /// turn's selection. Stamped onto each `Turn::Agent`. The effort readback
    /// field is `effort` (verified @ codex 0.137.0). Separate from the
    /// agent-scoped first-wins model that feeds `SessionMeta`.
    current_model: Option<String>,
    current_effort: Option<String>,
    /// The shell cwd from the most-recent `turn_context` — resolves relative
    /// `apply_patch` section paths to the absolute paths the facet contract
    /// requires (observed paths are already absolute; this is the defensive
    /// lexical join).
    current_cwd: Option<std::path::PathBuf>,
    /// Which rollout generation this file is (see [`HistoryMode`]). Set from
    /// `session_meta`, which Codex writes as the first record; a file with no
    /// `session_meta` at all keeps the `Missing` default and reads as legacy.
    history_mode: HistoryMode,
    /// The `exec` wrapper whose `custom_tool_call → custom_tool_call_output`
    /// interval the reconstruction replay is currently inside (paginated
    /// files only). The association prepass routes structured completions by a
    /// validated producer turn, exact command identity where available, and a
    /// narrow observed positional fallback. It then replays every owned item
    /// inside this interval so row mutation and collapse remain single-sourced.
    /// The structured item's synthetic id still matches neither wrapper id;
    /// unresolved ownership therefore keeps the canonical wrapper and never
    /// reaches this state through a guessed association.
    ///
    /// Only `exec`-named wrappers open an interval — deliberate: if a future
    /// paginated Codex emits standalone tool shapes (a bare `apply_patch`
    /// call, MCP as a direct `function_call`), their items warn "outside any
    /// exec wrapper interval" and only the *enrichment* is lost; the
    /// `response_item` row itself still hydrates.
    ///
    /// That is also why an orphaned item is warned and **dropped** rather than
    /// rendered standalone — a chosen tradeoff, resting on `response_item`
    /// being the canonical record: every tool call already has a row, so an
    /// item's content is never the only copy, and no probe has ever produced a
    /// call-less item — orphans can only arise from contract drift, exactly
    /// when pairing rules are least trustworthy. Rendering a late item as its
    /// own row would duplicate the operation its (already-closed) wrapper row
    /// shows. The cost is developer-visible only (a `ParseWarning`); the user
    /// sees an unenriched row.
    open_wrapper: Option<OpenWrapper>,
}

/// See [`CodexReconstruction::open_wrapper`]. The single-vs-batched dispatch is
/// **new logic composed from two existing primitives**: `decode_single_exec_wrapper`
/// already proves whether a wrapper is exactly one canonical `exec_command`
/// call, and `handle_patch_apply_end` already established match-else-push-new-row
/// as the child mechanism. A wrapper proved single-command is enriched **in
/// place** by its one `CommandExecution` item (so an ordinary shell command
/// renders as one row, not a wrapper row plus a duplicate child); anything else
/// — batched scripts, dynamic scripts, and (by construction, since the decoder
/// recognizes only `exec_command`) a lone `apply_patch` wrapper — gets each
/// item pushed as its **own row**, and the wrapper row is then **superseded**:
/// its children already render every operation it performed, so keeping it
/// would show each operation twice and expose the raw script the live stream
/// never surfaces.
///
/// A wrapper that attaches **no** child keeps its row — it is then the only
/// record that the operation happened, and the only failure evidence: a call
/// rejected before execution emits no item at all, and an uncaught failure's
/// diagnostic lives solely in the wrapper output. A wrapper **enriched in
/// place** keeps its row for the opposite reason: it is no longer a container
/// but one of the operations, so no later sibling may supersede it.
/// Where one arriving tool item lands (see [`CodexReconstruction::claim_wrapper_slot`]).
enum WrapperSlot {
    /// First item of a proved single-command wrapper: enrich the wrapper's own
    /// row at this index.
    EnrichInPlace(usize),
    /// Batched/dynamic wrapper (or a surprise extra item): push a new child row.
    /// `Some(index)` supersedes the wrapper row; `None` leaves it alone because
    /// it has itself become an operation row (enriched in place).
    OwnRow(Option<usize>),
    /// No wrapper interval open; warned and dropped. The association prepass
    /// normally filters this case before replay; this remains a defensive
    /// boundary for malformed or future shapes.
    Orphaned,
}

struct OpenWrapper {
    call_id: String,
    /// Index of the wrapper's row in the open builder's `items`.
    row_index: usize,
    /// `decode_single_exec_wrapper` succeeded: the wrapper is exactly one
    /// canonical `exec_command` call.
    single_command: bool,
    /// The in-place slot has been consumed. Tracked separately from "any child
    /// arrived": only a `CommandExecution` is ever eligible for the slot
    /// (kind-gated in `claim_wrapper_slot`), so a `FileChange`/`McpToolCall`
    /// arriving first takes its own row *without* eating the command's
    /// enrichment — otherwise the command would fall through to a child row
    /// and duplicate the wrapper, the exact regression the single-command
    /// test pins.
    command_slot_taken: bool,
}

struct CodexAgentBuilder {
    turn_id: TurnId,
    agent_id: AgentId,
    started_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
    items: Vec<TurnItem>,
    /// Indices into `items` of `exec` wrapper rows whose operations are each
    /// rendered by a child row of their own; dropped when the turn closes (see
    /// [`WrapperSlot::OwnRow`]). Recorded rather than removed eagerly because
    /// every open `row_index` is an index into this same vector.
    superseded_rows: Vec<usize>,
    usage: Option<TurnUsage>,
    context_window: Option<u32>,
    pending_mcp_results: HashMap<String, McpResult>,
    /// Codex's harness-local per-turn id from this turn's `turn_context.turn_id`
    /// — re-parse-stable, so it serves as the hydration key (distinct from our
    /// own `turn_id`, minted fresh each parse). Set when the turn's
    /// `turn_context` arrives; `None` for a turn that writes none.
    hydration_key: Option<String>,
}

/// Drop the `exec` wrapper rows their own children already render. Applied at
/// turn close, once every index is final.
///
/// A **failed** wrapper is kept even when it has children. Its children only
/// cover the operations that got far enough to emit an item, so a batch whose
/// second operation failed hard shows one successful child and nothing else —
/// the wrapper's `Script error:` output is then the sole record that anything
/// went wrong. A redundant failed row costs the reader a second look; a
/// silently dropped failure costs them the failure.
fn drop_superseded_rows(items: Vec<TurnItem>, superseded: &[usize]) -> Vec<TurnItem> {
    if superseded.is_empty() {
        return items;
    }
    items
        .into_iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let failed = matches!(
                &item,
                TurnItem::Tool {
                    is_error: Some(true),
                    ..
                }
            );
            (failed || !superseded.contains(&index)).then_some(item)
        })
        .collect()
}

impl CodexAgentBuilder {
    /// Mark an `exec` wrapper row as rendered by its children. Idempotent: a
    /// batched wrapper supersedes the same row once per child.
    fn supersede_wrapper_row(&mut self, row_index: usize) {
        if !self.superseded_rows.contains(&row_index) {
            self.superseded_rows.push(row_index);
        }
    }

    /// Reinstate a wrapper row enriched in place: it now *is* one of the
    /// operations, so a sibling child that superseded it first (a `FileChange`
    /// can arrive before the `CommandExecution` that claims the slot) must not
    /// take the command down with it.
    fn keep_wrapper_row(&mut self, row_index: usize) {
        self.superseded_rows.retain(|&index| index != row_index);
    }
}

/// Captured `mcp_tool_call_end` payload — applied to the matching
/// `function_call` item when both have been observed.
struct McpResult {
    server: String,
    tool: String,
    output: String,
    is_error: bool,
    completed_at: Option<DateTime<Utc>>,
}

impl CodexReconstruction {
    fn new(agent_id: AgentId) -> Self {
        Self {
            agent_id,
            turns: Vec::new(),
            current_agent: None,
            warnings: Vec::new(),
            current_model: None,
            current_effort: None,
            current_cwd: None,
            history_mode: HistoryMode::Missing,
            open_wrapper: None,
        }
    }

    fn warn(&mut self, line_number: usize, reason: impl Into<String>) {
        self.warnings.push(ParseWarning {
            line_number,
            reason: reason.into(),
        });
    }

    fn ingest(&mut self, line_number: usize, record: &Value) {
        let record_type = record.get("type").and_then(Value::as_str).unwrap_or("");
        let payload = record.get("payload");
        let timestamp = record
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc));

        match record_type {
            // Codex writes `session_meta` first, so the mode is known before any
            // content record arrives. An unrecognized value is surfaced here
            // rather than at the point content goes missing: by then the parser
            // has nothing to attribute the loss to, which is precisely how the
            // paginated switch went unnoticed.
            "session_meta" => {
                if let Some(p) = payload {
                    self.history_mode = HistoryMode::from_session_meta(p);
                    if let HistoryMode::Unknown(value) = &self.history_mode {
                        let value = value.clone();
                        self.warn(
                            line_number,
                            format!(
                                "unrecognized session_meta.history_mode {value:?}; \
                                 reading as legacy — transcript content may be incomplete"
                            ),
                        );
                    }
                }
            }
            "event_msg" => self.handle_event_msg(line_number, payload, timestamp),
            "response_item" => self.handle_response_item(line_number, payload, timestamp),
            // Codex writes a `turn_context` at each turn's start carrying that
            // turn's model + effort. Reset to **exactly this record's** values
            // (not carry-until-overwritten) so `close_current_agent` stamps the
            // turn with its own selection and never inherits a prior turn's.
            // Effort readback field is `effort`, not `model_reasoning_effort`
            // (verified @ 0.137.0). Codex currently always writes both, but
            // absence must mean `None`, not stale.
            //
            // `turn_context.turn_id` remains the stable hydration key exposed
            // to the frontend. The association prepass may validate the
            // separate, file-local `task_started.turn_id` namespace against it
            // to route persisted tool detail, but never promotes that routing
            // id into frontend identity. A non-unique hydration key would
            // silently drop a new turn during merge (see the builder field).
            "turn_context" => {
                if let Some(p) = payload {
                    self.current_model = p.get("model").and_then(Value::as_str).map(str::to_owned);
                    self.current_effort =
                        p.get("effort").and_then(Value::as_str).map(str::to_owned);
                    self.current_cwd = p
                        .get("cwd")
                        .and_then(Value::as_str)
                        .map(std::path::PathBuf::from);
                    if let Some(builder) = self.current_agent.as_mut() {
                        // Same helper as the live enrichment — the two must read the
                        // identical field or a Codex turn mis-links (see
                        // `turn_context_turn_id`).
                        builder.hydration_key = turn_context_turn_id(p);
                    }
                }
            }
            _ => {}
        }
    }

    #[allow(clippy::too_many_lines)]
    fn handle_event_msg(
        &mut self,
        line_number: usize,
        payload: Option<&Value>,
        timestamp: Option<DateTime<Utc>>,
    ) {
        let Some(p) = payload else { return };
        let event_type = p.get("type").and_then(Value::as_str).unwrap_or("");
        match event_type {
            "task_started" => {
                // Open a fresh agent turn. Close any predecessor first
                // (defensive against missing task_complete records).
                self.close_current_agent(TurnStatus::Failed);
                let started_at = timestamp.unwrap_or_else(Utc::now);
                let context_window = p
                    .get("model_context_window")
                    .and_then(Value::as_u64)
                    .and_then(|v| u32::try_from(v).ok());
                // A new turn can never inherit a prior turn's wrapper interval.
                self.open_wrapper = None;
                self.current_agent = Some(CodexAgentBuilder {
                    turn_id: Uuid::now_v7(),
                    agent_id: self.agent_id,
                    started_at,
                    last_seen_at: started_at,
                    items: Vec::new(),
                    superseded_rows: Vec::new(),
                    usage: None,
                    context_window,
                    pending_mcp_results: HashMap::new(),
                    // Set when this turn's `turn_context` arrives (below). A fresh
                    // builder per turn means the key is reset by construction —
                    // it can never inherit a prior turn's id. That matters more
                    // than for model/effort: a *stale* dedup key is non-unique
                    // across turns, and the merge would then silently drop a
                    // genuinely-new turn as "already seen" (lost output), which
                    // is worse than the duplication this key exists to prevent.
                    hydration_key: None,
                });
            }
            "task_complete" => {
                self.close_current_agent(TurnStatus::Complete);
            }
            // Prompt and answer text is **single-sourced per mode**: legacy
            // rollouts carry `user_message`/`agent_message`, paginated ones carry
            // the same content inside `item_completed`. The guards make that
            // structural rather than trusted — the two channels are not observed
            // to coexist, but if they ever did, an ungated parser would render
            // every message twice.
            "user_message" if !self.history_mode.reads_item_completed() => {
                // Push to `self.turns` directly, not into `builder.items`:
                // Codex emits `task_started` BEFORE `user_message`, so the
                // agent builder is already open here. Anchor the user turn to
                // that task start so timestamp-sorted imported transcripts keep
                // the prompt directly above the reply. For Switchboard-dispatched
                // turns, the journal Send is written before the Codex task starts,
                // so this anchor still remains inside the journaled send window.
                let Some(message) = p.get("message").and_then(Value::as_str) else {
                    return;
                };
                let started_at = self.current_agent.as_ref().map_or_else(
                    || timestamp.unwrap_or_else(Utc::now),
                    |builder| builder.started_at,
                );
                let user_turn = Turn::User {
                    turn_id: Uuid::now_v7(),
                    agent_id: self.agent_id,
                    started_at,
                    text: message.to_owned(),
                    source: crate::transcript::UserPromptSource::Unknown,
                };
                self.turns.push(user_turn);
            }
            "agent_message" if !self.history_mode.reads_item_completed() => {
                let Some(message) = p.get("message").and_then(Value::as_str) else {
                    return;
                };
                if let Some(builder) = self.current_agent.as_mut() {
                    builder.items.push(TurnItem::Text {
                        kind: ContentKind::Text,
                        text: message.to_owned(),
                    });
                    if let Some(t) = timestamp {
                        builder.last_seen_at = t;
                    }
                }
            }
            "token_count" => {
                // `info.last_token_usage` carries per-turn tokens. `info` is
                // null on the rate-limits-only variant — skip those.
                let Some(builder) = self.current_agent.as_mut() else {
                    return;
                };
                let Some(info) = p.get("info").filter(|v| !v.is_null()) else {
                    return;
                };
                if let Some(usage) = turn_usage_from_token_count_info(info, builder.context_window)
                {
                    builder.usage = Some(usage);
                }
            }
            // Paginated rollouts carry prompt, answer, and tool detail here
            // instead of on the legacy `event_msg` records. Gated on the mode
            // because legacy files also emit `item_completed`, but only for
            // item types this parser does not consume (`Plan`, extensions) —
            // ungated, the arm would be harmless today and wrong the moment
            // that set widens.
            "item_completed" if self.history_mode.reads_item_completed() => {
                self.handle_item_completed(line_number, p, timestamp);
            }
            // Gated like the text arms above: edit/MCP enrichment is
            // single-sourced per mode. On a paginated file the canonical
            // source is `item_completed`; a rogue legacy record here would
            // otherwise push a row duplicating the FileChange/McpToolCall
            // child already created (match-else-push never matches the
            // synthetic child ids).
            "patch_apply_end" if !self.history_mode.reads_item_completed() => {
                self.handle_patch_apply_end(line_number, p, timestamp);
            }
            "mcp_tool_call_end" if !self.history_mode.reads_item_completed() => {
                let Some(call_id) = p.get("call_id").and_then(Value::as_str) else {
                    self.warn(line_number, "mcp_tool_call_end missing call_id");
                    return;
                };
                let invocation = p.get("invocation");
                let server = invocation
                    .and_then(|i| i.get("server"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let tool = invocation
                    .and_then(|i| i.get("tool"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let (output, is_error) = decode_mcp_result(p.get("result"));
                let result = McpResult {
                    server,
                    tool,
                    output,
                    is_error,
                    completed_at: timestamp,
                };
                if let Some(builder) = self.current_agent.as_mut() {
                    let matched = apply_mcp_result(&mut builder.items, call_id, &result);
                    // Only stash for late-arrival pairing if the eager apply
                    // didn't match (rare — Codex emits function_call first).
                    // Stashing on match would leak unused entries.
                    if !matched {
                        builder
                            .pending_mcp_results
                            .insert(call_id.to_owned(), result);
                    }
                }
            }
            _ => {}
        }
    }

    /// Paginated rollouts' replacement for the legacy `user_message` /
    /// `agent_message` / `patch_apply_end` / `mcp_tool_call_end` records.
    /// `item.type` is a Codex `TurnItem` variant; each arm mirrors the contract
    /// of the legacy record it supersedes so both generations hydrate
    /// identically.
    ///
    /// Tool variants arrive here inside the wrapper interval selected by the
    /// association prepass (see [`OpenWrapper`]). They *enrich or extend* the
    /// tool rows, they do not replace them: rows come from `response_item`, the
    /// only complete record of tool activity, since a failed call can emit no
    /// `item_completed` at all. Unknown variants fall through silently,
    /// matching the parser's existing posture toward record types it does not
    /// consume.
    fn handle_item_completed(
        &mut self,
        line_number: usize,
        payload: &Value,
        timestamp: Option<DateTime<Utc>>,
    ) {
        let Some(item) = payload.get("item") else {
            return;
        };
        match item.get("type").and_then(Value::as_str).unwrap_or("") {
            "UserMessage" => {
                // A recognized message item with unreadable content is warned,
                // not skipped — a silent drop here is a miniature rerun of the
                // silent loss this module exists to fix.
                let Some(text) = item_message_text(item) else {
                    self.warn(line_number, "UserMessage item missing content array");
                    return;
                };
                // Pushed even when the text is empty: the prompt may have been
                // image/audio/skill-only (`UserInput` has non-text variants), and
                // the turn *boundary* is real even though the attachment is not
                // representable. Inferred parity with the legacy path, which
                // flattens the same prompt to an empty `message` string and
                // pushes the turn (what legacy Codex actually writes for an
                // attachment-only prompt is uncaptured). This restores the
                // chronology, not the attachment itself.
                //
                // Same anchoring as the legacy `user_message` arm: Codex writes
                // `task_started` before the prompt, so a builder is already open
                // and the user turn is anchored to that task start — which keeps
                // a timestamp-sorted transcript rendering the prompt directly
                // above its reply.
                let started_at = self.current_agent.as_ref().map_or_else(
                    || timestamp.unwrap_or_else(Utc::now),
                    |builder| builder.started_at,
                );
                self.turns.push(Turn::User {
                    turn_id: Uuid::now_v7(),
                    agent_id: self.agent_id,
                    started_at,
                    text,
                    source: crate::transcript::UserPromptSource::Unknown,
                });
            }
            "AgentMessage" => {
                let Some(text) = item_message_text(item) else {
                    self.warn(line_number, "AgentMessage item missing content array");
                    return;
                };
                // Empty answer text is skipped — deliberately asymmetric with
                // the UserMessage arm above (an agent item has no attachment
                // variants whose boundary is worth preserving) and matching the
                // live stream parser, whose `empty_agent_message_text_is_skipped`
                // test pins the same policy. Live and reopened transcripts agree.
                if text.is_empty() {
                    return;
                }
                if let Some(builder) = self.current_agent.as_mut() {
                    builder.items.push(TurnItem::Text {
                        kind: ContentKind::Text,
                        text,
                    });
                    if let Some(t) = timestamp {
                        builder.last_seen_at = t;
                    }
                }
            }
            "CommandExecution" => self.attach_command_execution(line_number, item, timestamp),
            "FileChange" => self.attach_file_change(line_number, item, timestamp),
            "McpToolCall" => self.attach_mcp_tool_call(line_number, item, timestamp),
            // Reasoning is captured but carries no renderable prose — the
            // summary is empty and the raw content is encrypted (§3.2). Skipped
            // for the same reason Codex reasoning has always been skipped, not
            // as an oversight.
            _ => {}
        }
    }

    /// Take the open wrapper for one arriving tool item, or warn. `Orphaned`
    /// means no wrapper interval is open — the item is dropped with a warning
    /// rather than guessed onto some other row (a mis-attached exit code or
    /// diff is a plausible-looking wrong answer, strictly worse than a visibly
    /// missing one; full rationale on [`CodexReconstruction::open_wrapper`]).
    ///
    /// The in-place slot is **kind-gated**: only a `CommandExecution` can
    /// enrich the wrapper row, because "single-command" is `decode_single_exec_wrapper`'s
    /// proof of exactly one `exec_command` call — structural, not trusted from
    /// capture history. Any other kind always takes its own row and leaves the
    /// slot for the command.
    fn claim_wrapper_slot(&mut self, line_number: usize, item_type: &str) -> WrapperSlot {
        let (single_command, slot_taken, row_index) = {
            let Some(wrapper) = self.open_wrapper.as_mut() else {
                self.warn(
                    line_number,
                    format!("{item_type} item outside any exec wrapper interval"),
                );
                return WrapperSlot::Orphaned;
            };
            if item_type == "CommandExecution"
                && wrapper.single_command
                && !wrapper.command_slot_taken
            {
                wrapper.command_slot_taken = true;
                return WrapperSlot::EnrichInPlace(wrapper.row_index);
            }
            (
                wrapper.single_command,
                wrapper.command_slot_taken,
                wrapper.row_index,
            )
        };
        // A wrapper `decode_single_exec_wrapper` accepted is structurally proved
        // to be exactly one `tools.exec_command` call, so its one expected item
        // is that call's own `CommandExecution`. Anything else — a second
        // command, a `FileChange`, an `McpToolCall` — is unaccounted for by the
        // proof, and is warned in **either record order**: the anomaly is the
        // shape, not the sequencing, and a tripwire that fires only when the
        // stray item happens to arrive second reports the order instead of the
        // thing it watches for. Corpus evidence for treating this as drift
        // rather than ordinary shell behaviour is dated in `harness-behavior.md`
        // §3.6 — a real-world frequency, not a proof it cannot happen.
        //
        // Batched wrappers are untouched: `single_command` is false there, so
        // every child still supersedes the container as usual.
        if single_command {
            self.warn(
                line_number,
                format!("{item_type} item on an exec wrapper proved to be a single command"),
            );
        }
        if slot_taken {
            // The wrapper row now *holds* the command — an operation row, not a
            // container. Superseding it would delete a real shell call whose
            // only offence was that another item followed it.
            return WrapperSlot::OwnRow(None);
        }
        WrapperSlot::OwnRow(Some(row_index))
    }

    /// `CommandExecution`: the structured shell record paginated rollouts
    /// added — legacy persisted none, which is why the legacy path resorts to
    /// sniffing `Script failed` strings. The structured status/`exit_code` is
    /// authoritative for `is_error` (set here, so the wrapper output's
    /// string-sniffing fallback — which only fills `None` — never overrides it;
    /// an *unreadable* status deliberately leaves `None`, degrading to exactly
    /// that legacy heuristic plus a warning).
    fn attach_command_execution(
        &mut self,
        line_number: usize,
        item: &Value,
        timestamp: Option<DateTime<Utc>>,
    ) {
        let warning_start = self.warnings.len();
        let slot = self.claim_wrapper_slot(line_number, "CommandExecution");
        if matches!(slot, WrapperSlot::Orphaned) {
            return;
        }
        let exit_failed = self.read_exit_code_failed(line_number, item);
        let is_error = self.command_execution_is_error(line_number, item, exit_failed);
        let facet = command_execution_item_facet(item);
        // Output precedence: `aggregated_output`, else structured
        // stdout/stderr. Written even when empty **only on affirmative
        // success** — an empty string is then the command's true output, and
        // blank beats the wrapper's "Script completed / Wall time…"
        // boilerplate. Failure *and unknown* write only non-empty output,
        // leaving the slot `None` so the wrapper's `Script error:` diagnostic
        // (often the only failure evidence) can still fill it — an unknown
        // outcome must not suppress the one place its explanation may live.
        let structured_output = command_execution_output(item);
        let output = if is_error == Some(false) {
            Some(structured_output)
        } else {
            (!structured_output.is_empty()).then_some(structured_output)
        };
        let row_id = self.item_row_id(line_number, item, "command");
        let owned_warnings = self.warnings[warning_start..].to_vec();
        let Some(builder) = self.current_agent.as_mut() else {
            return;
        };
        if let WrapperSlot::EnrichInPlace(row_index) = slot {
            // Single-command wrapper: this item *is* the wrapper's one call.
            if let Some(TurnItem::Tool {
                facet: row_facet,
                is_error: row_error,
                output: row_output,
                warnings: row_warnings,
                completed_at,
                ..
            }) = builder.items.get_mut(row_index)
            {
                if !matches!(facet, crate::facets::ToolFacet::Other) {
                    *row_facet = facet;
                }
                *row_error = is_error;
                if let Some(text) = output {
                    *row_output = Some(text);
                }
                row_warnings.extend(owned_warnings);
                *completed_at = timestamp;
            }
            builder.keep_wrapper_row(row_index);
            return;
        }
        if let WrapperSlot::OwnRow(Some(wrapper_row)) = slot {
            builder.supersede_wrapper_row(wrapper_row);
        }
        builder.items.push(TurnItem::Tool {
            tool_use_id: row_id,
            kind: ToolKind::Builtin,
            facet,
            name: "exec_command".to_owned(),
            input: item.get("command").cloned().unwrap_or(Value::Null),
            // The `Option` is pushed through un-flattened: no output-record
            // pairing ever reaches a child row (its id matches no `call_id`),
            // so `None` here stays `None` — "no output recorded", honest for
            // the failure/unknown cases above.
            output,
            is_error,
            warnings: owned_warnings,
            started_at: timestamp.unwrap_or(builder.last_seen_at),
            completed_at: timestamp,
        });
    }

    /// Whether the structured `exit_code` signals failure. `None` when absent
    /// (the field is optional upstream — a declined command never ran) — or
    /// **present but non-numeric**, which warns: a present field in an
    /// unreadable shape is upstream contract drift, not a success.
    fn read_exit_code_failed(&mut self, line_number: usize, item: &Value) -> Option<bool> {
        let value = match item.get("exit_code") {
            None | Some(Value::Null) => return None,
            Some(value) => value,
        };
        if let Some(code) = value.as_i64() {
            return Some(code != 0);
        }
        self.warn(
            line_number,
            format!("CommandExecution exit_code is not numeric: {value}"),
        );
        None
    }

    /// `is_error` for a `CommandExecution`, from its `status` OR-combined with
    /// the exit code — a `failed`/`declined` status is not erased by a zero
    /// exit, and a nonzero exit is not erased by a stale `completed`.
    /// `declined` (the user refused the tool) must read as unsuccessful:
    /// asserting a declined command ran is actively misleading. An
    /// unrecognized status warns and yields `None` — never fabricate a reading
    /// of an unknown contract (the `HistoryMode::Unknown` posture); the
    /// in-place path then degrades to the wrapper-output string sniff, while a
    /// child row keeps `None` permanently (no pairing reaches it). Today the
    /// frontend renders `None` and `Some(false)` identically (`toolRowState`
    /// fails only on `is_error === true`), so `None` costs nothing visible and
    /// keeps the wire honest for a future distinct "unknown" rendering.
    fn command_execution_is_error(
        &mut self,
        line_number: usize,
        item: &Value,
        exit_failed: Option<bool>,
    ) -> Option<bool> {
        // Matched on the raw value first, like `HistoryMode::from_session_meta`:
        // a present-but-non-string status is the same schema drift as an
        // unrecognized string and must take the same warned-unknown path —
        // folding it into "missing" would assert success on malformed data.
        let status = match item.get("status") {
            // `status` is a required field upstream — absence is schema drift.
            None | Some(Value::Null) => {
                self.warn(line_number, "CommandExecution item missing status");
                return exit_failed;
            }
            Some(Value::String(s)) => s.as_str(),
            Some(other) => {
                self.warn(
                    line_number,
                    format!("CommandExecution status is not a string: {other}"),
                );
                return if exit_failed == Some(true) {
                    Some(true)
                } else {
                    None
                };
            }
        };
        match status {
            "completed" => Some(exit_failed == Some(true)),
            "failed" | "declined" => Some(true),
            "in_progress" => {
                // Inside an `item_completed` record this means the file was
                // truncated mid-operation — incomplete, not successful.
                self.warn(
                    line_number,
                    "CommandExecution item_completed with in_progress status",
                );
                Some(true)
            }
            other => {
                self.warn(
                    line_number,
                    format!("CommandExecution has unrecognized status {other:?}"),
                );
                // Asymmetric on purpose: a readable nonzero exit convicts
                // (positive failure evidence from a field we *can* read is not
                // discarded because a different field went unreadable), but a
                // zero exit does not acquit — an unknown status could be a
                // declined-like state where the command never ran at all.
                if exit_failed == Some(true) {
                    Some(true)
                } else {
                    None
                }
            }
        }
    }

    /// Deterministic row id for a paginated tool item. The `id` field is
    /// required upstream, so absence warns as schema drift; the fallback is
    /// derived from the line number — a fresh UUID would make two parses of
    /// the same file produce different transcripts, hiding the drift and
    /// breaking parse determinism.
    fn item_row_id(&mut self, line_number: usize, item: &Value, kind: &str) -> String {
        if let Some(id) = item.get("id").and_then(Value::as_str) {
            return id.to_owned();
        }
        self.warn(line_number, format!("{kind} item missing id"));
        format!("item-missing-id-{kind}-line-{line_number}")
    }

    /// `FileChange`: same `changes` map as legacy `patch_apply_end` (fact 6),
    /// so the same facet builder serves both generations — both render
    /// identically. Always its own row: the decoder recognizes only
    /// `exec_command`, so an edit's wrapper is never "single-command" and the
    /// in-place slot cannot fire — matching the legacy generation-2
    /// presentation (wrapper row + separate `apply_patch` row).
    fn attach_file_change(
        &mut self,
        line_number: usize,
        item: &Value,
        timestamp: Option<DateTime<Utc>>,
    ) {
        let warning_start = self.warnings.len();
        let slot = self.claim_wrapper_slot(line_number, "FileChange");
        if matches!(slot, WrapperSlot::Orphaned) {
            return;
        }
        let facet = super::facets::patch_apply_end_facet(item);
        if !matches!(facet, crate::facets::ToolFacet::Edit { .. }) {
            self.warn(line_number, "FileChange item missing structured changes");
            return;
        }
        // `status` is *optional* upstream (unlike CommandExecution's), so
        // absence reads as completed without a warning. `declined` — the user
        // refused the edit — must read as unsuccessful; an unrecognized string
        // or a non-string value warns and stays `None` (unknown, not guessed)
        // — the same raw-value-first match as the command and MCP handlers, so
        // schema drift cannot masquerade as an absent optional field.
        let is_error = match item.get("status") {
            None | Some(Value::Null) => Some(false),
            Some(Value::String(status)) => match status.as_str() {
                "completed" => Some(false),
                "failed" | "declined" => Some(true),
                other => {
                    self.warn(
                        line_number,
                        format!("FileChange has unrecognized status {other:?}"),
                    );
                    None
                }
            },
            Some(other) => {
                self.warn(
                    line_number,
                    format!("FileChange status is not a string: {other}"),
                );
                None
            }
        };
        let output = patch_apply_end_output(item);
        let row_id = self.item_row_id(line_number, item, "file-change");
        let owned_warnings = self.warnings[warning_start..].to_vec();
        let Some(builder) = self.current_agent.as_mut() else {
            return;
        };
        if let WrapperSlot::OwnRow(Some(wrapper_row)) = slot {
            builder.supersede_wrapper_row(wrapper_row);
        }
        builder.items.push(TurnItem::Tool {
            tool_use_id: row_id,
            kind: ToolKind::Builtin,
            facet,
            name: "apply_patch".to_owned(),
            input: item.get("changes").cloned().unwrap_or(Value::Null),
            output: Some(output),
            is_error,
            warnings: owned_warnings,
            started_at: timestamp.unwrap_or(builder.last_seen_at),
            completed_at: timestamp,
        });
    }

    /// `McpToolCall`: MCP calls ride the same wrapper as shell/edit work (M1
    /// capture) — same attachment, own row. Three result envelopes: success;
    /// tool-reported error (`status: "failed"` + `result.isError`); transport
    /// failure (`result: null` + top-level `error` — **source-derived** from
    /// upstream's `McpToolCallError`, no live capture exists). The live stream
    /// parser distinguishes all three; hydration must match or protocol
    /// failures reopen with a failed status and no diagnostic.
    fn attach_mcp_tool_call(
        &mut self,
        line_number: usize,
        item: &Value,
        timestamp: Option<DateTime<Utc>>,
    ) {
        let warning_start = self.warnings.len();
        let slot = self.claim_wrapper_slot(line_number, "McpToolCall");
        if matches!(slot, WrapperSlot::Orphaned) {
            return;
        }
        let server = item.get("server").and_then(Value::as_str).unwrap_or("");
        let tool = item.get("tool").and_then(Value::as_str).unwrap_or("");
        let arguments = item.get("arguments").cloned().unwrap_or(Value::Null);
        let error = item.get("error").filter(|v| !v.is_null());
        let result = item.get("result");
        // Result/error evidence convicts regardless of status: a tool-reported
        // error under a `completed` status is still a failure. Shared with the
        // live stream parser so the two surfaces agree on what counts as
        // failure, not just on output extraction.
        let evidence_failed = super::parser::mcp_result_indicates_error(result, error);
        // Same explicit matrix as `command_execution_is_error`, minus exit
        // codes (MCP has none) — an unreadable or future status must warn and
        // stay unknown, never silently read as success. `McpToolCallStatus`
        // serializes camelCase ("inProgress"), unlike the shell statuses.
        let is_error = match item.get("status") {
            None | Some(Value::Null) => {
                self.warn(line_number, "McpToolCall item missing status");
                if evidence_failed { Some(true) } else { None }
            }
            Some(Value::String(s)) => match s.as_str() {
                "completed" => Some(evidence_failed),
                "failed" => Some(true),
                "inProgress" => {
                    self.warn(
                        line_number,
                        "McpToolCall item_completed with inProgress status",
                    );
                    Some(true)
                }
                other => {
                    self.warn(
                        line_number,
                        format!("McpToolCall has unrecognized status {other:?}"),
                    );
                    if evidence_failed { Some(true) } else { None }
                }
            },
            Some(other) => {
                self.warn(
                    line_number,
                    format!("McpToolCall status is not a string: {other}"),
                );
                if evidence_failed { Some(true) } else { None }
            }
        };
        // The live parser's extractor, shared so the two paths cannot drift:
        // non-text results get its placeholder, empty/null results fall to the
        // error field — a transcript must not show an MCP image result as
        // nothing on reopen while the live view showed a placeholder.
        let output = super::parser::extract_mcp_output(result, error);
        let facet = crate::facets::classify_mcp_tool_facet(server, tool, &arguments);
        let row_id = self.item_row_id(line_number, item, "mcp");
        let owned_warnings = self.warnings[warning_start..].to_vec();
        let Some(builder) = self.current_agent.as_mut() else {
            return;
        };
        if let WrapperSlot::OwnRow(Some(wrapper_row)) = slot {
            builder.supersede_wrapper_row(wrapper_row);
        }
        builder.items.push(TurnItem::Tool {
            tool_use_id: row_id,
            kind: ToolKind::Mcp,
            facet,
            name: format!("{server}.{tool}"),
            input: arguments,
            output: Some(output),
            is_error,
            warnings: owned_warnings,
            started_at: timestamp.unwrap_or(builder.last_seen_at),
            completed_at: timestamp,
        });
    }

    fn handle_response_item(
        &mut self,
        line_number: usize,
        payload: Option<&Value>,
        timestamp: Option<DateTime<Utc>>,
    ) {
        let Some(p) = payload else { return };
        let item_type = p.get("type").and_then(Value::as_str).unwrap_or("");
        match item_type {
            "function_call" => {
                let Some(call_id) = p.get("call_id").and_then(Value::as_str) else {
                    self.warn(line_number, "function_call missing call_id");
                    return;
                };
                let raw_name = p.get("name").and_then(Value::as_str).unwrap_or("");
                let arguments = p
                    .get("arguments")
                    .and_then(Value::as_str)
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                    .unwrap_or(Value::Null);
                let namespace = p.get("namespace").and_then(Value::as_str);
                let (kind, name) = classify_codex_function_call(raw_name, namespace);
                let facet = match codex_mcp_server(namespace) {
                    Some(server) => {
                        crate::facets::classify_mcp_tool_facet(server, raw_name, &arguments)
                    }
                    _ if raw_name == "exec_command" => {
                        super::facets::exec_command_facet(&arguments)
                    }
                    _ => crate::facets::ToolFacet::Other,
                };
                let started_at = timestamp.unwrap_or_else(Utc::now);
                let Some(builder) = self.current_agent.as_mut() else {
                    return;
                };
                let item = TurnItem::Tool {
                    tool_use_id: call_id.to_owned(),
                    kind,
                    facet,
                    name,
                    input: arguments,
                    output: None,
                    is_error: None,
                    warnings: Vec::new(),
                    started_at,
                    completed_at: None,
                };
                builder.items.push(item);
                // If the matching mcp_tool_call_end already arrived, apply
                // it now (shouldn't happen in practice — Codex writes the
                // function_call before the end event — but defensive).
                if let Some(result) = builder.pending_mcp_results.remove(call_id) {
                    let _ = apply_mcp_result(&mut builder.items, call_id, &result);
                }
            }
            "custom_tool_call" => self.handle_custom_tool_call(line_number, p, timestamp),
            // Same `{call_id, output}` pairing shape as function_call_output.
            "function_call_output" | "custom_tool_call_output" => {
                let Some(call_id) = p.get("call_id").and_then(Value::as_str) else {
                    self.warn(line_number, "function_call_output missing call_id");
                    return;
                };
                // The wrapper's output record closes its attachment interval
                // (fact: a tool's items land strictly before it).
                if self
                    .open_wrapper
                    .as_ref()
                    .is_some_and(|w| w.call_id == call_id)
                {
                    self.open_wrapper = None;
                }
                let output = decode_function_call_output(p.get("output"));
                let completed_at = timestamp;
                let Some(builder) = self.current_agent.as_mut() else {
                    return;
                };
                let mut matched = false;
                for item in &mut builder.items {
                    if let TurnItem::Tool {
                        tool_use_id,
                        input,
                        output: out,
                        is_error,
                        completed_at: cat,
                        ..
                    } = item
                        && tool_use_id == call_id
                    {
                        // Don't overwrite an MCP-result-supplied output.
                        if out.is_none() {
                            *out = Some(output.clone());
                            *cat = completed_at;
                        }
                        // Structured result events (notably patch_apply_end)
                        // are authoritative over this format-sensitive
                        // fallback, regardless of which record arrived first.
                        if is_error.is_none() {
                            *is_error = Some(function_call_output_is_error(&output, input));
                        }
                        matched = true;
                        break;
                    }
                }
                if !matched {
                    self.warn(
                        line_number,
                        format!("function_call_output for {call_id} did not match any open tool"),
                    );
                }
            }
            // `response_item/message` carries the structured model-API form
            // of the conversation content (`content: [{type:"input_text",
            // text:"..."}]`). We never parse it, in either generation: text is
            // **single-sourced per mode** — legacy files supply it through
            // `event_msg/user_message` / `agent_message`, paginated files
            // through `event_msg/item_completed` — and this record duplicates
            // whichever of those is present, so consuming it would double-count
            // every message.
            //
            // This arm once claimed the legacy records "flow alongside in every
            // observed Codex session". That stopped being true at Codex 0.148,
            // and the fixture suite did not notice because fixtures replay the
            // shape they were recorded from (G30). The durable guard is the
            // live suite, not these tests: `make test-live-codex` runs against
            // whatever the installed CLI actually writes.
            _ => {}
        }
    }

    /// Legacy Codex edit channel: `apply_patch` arrives as a `custom_tool_call`
    /// whose `input` is raw patch text. Newer rollouts use an `exec` wrapper;
    /// edit content remains available through `patch_apply_end`, handled by
    /// `handle_patch_apply_end`.
    fn handle_custom_tool_call(
        &mut self,
        line_number: usize,
        p: &Value,
        timestamp: Option<DateTime<Utc>>,
    ) {
        let Some(call_id) = p.get("call_id").and_then(Value::as_str) else {
            self.warn(line_number, "custom_tool_call missing call_id");
            return;
        };
        let raw_name = p.get("name").and_then(Value::as_str).unwrap_or("");
        let input = p.get("input").and_then(Value::as_str).unwrap_or("");
        let facet = match raw_name {
            "apply_patch" => super::facets::apply_patch_facet(input, self.current_cwd.as_deref()),
            "exec" => decode_single_exec_wrapper(input)
                .map_or(crate::facets::ToolFacet::Other, |decoded| decoded.facet),
            _ => crate::facets::ToolFacet::Other,
        };
        let started_at = timestamp.unwrap_or_else(Utc::now);
        let single_command = raw_name == "exec" && decode_single_exec_wrapper(input).is_some();
        let Some(builder) = self.current_agent.as_mut() else {
            return;
        };
        builder.items.push(TurnItem::Tool {
            tool_use_id: call_id.to_owned(),
            kind: ToolKind::Builtin,
            facet,
            name: raw_name.to_owned(),
            input: Value::String(input.to_owned()),
            output: None,
            is_error: None,
            warnings: Vec::new(),
            started_at,
            completed_at: None,
        });
        // Paginated files interleave the wrapper's tool items between this
        // record and its output record; open the attachment interval. Legacy
        // files never emit those items, so the state is inert there — but it is
        // only *read* under the paginated gate, keeping the modes structurally
        // separate.
        if raw_name == "exec" && self.history_mode.reads_item_completed() {
            self.open_wrapper = Some(OpenWrapper {
                call_id: call_id.to_owned(),
                row_index: builder.items.len() - 1,
                single_command,
                command_slot_taken: false,
            });
        }
    }

    fn handle_patch_apply_end(
        &mut self,
        line_number: usize,
        payload: &Value,
        timestamp: Option<DateTime<Utc>>,
    ) {
        let Some(call_id) = payload.get("call_id").and_then(Value::as_str) else {
            self.warn(line_number, "patch_apply_end missing call_id");
            return;
        };
        let facet = super::facets::patch_apply_end_facet(payload);
        if !matches!(facet, crate::facets::ToolFacet::Edit { .. }) {
            self.warn(line_number, "patch_apply_end missing structured changes");
            return;
        }
        let is_error = payload.get("success").and_then(Value::as_bool) == Some(false)
            || payload.get("status").and_then(Value::as_str) == Some("failed");
        let Some(builder) = self.current_agent.as_mut() else {
            return;
        };
        if let Some(TurnItem::Tool {
            facet: existing_facet,
            is_error: existing_error,
            completed_at,
            ..
        }) = builder.items.iter_mut().find(
            |item| matches!(item, TurnItem::Tool { tool_use_id, .. } if tool_use_id == call_id),
        ) {
            *existing_facet = facet;
            *existing_error = Some(is_error);
            *completed_at = timestamp;
            return;
        }

        let output = patch_apply_end_output(payload);
        builder.items.push(TurnItem::Tool {
            tool_use_id: call_id.to_owned(),
            kind: ToolKind::Builtin,
            facet,
            name: "apply_patch".to_owned(),
            input: payload.get("changes").cloned().unwrap_or(Value::Null),
            output: Some(output),
            is_error: Some(is_error),
            warnings: Vec::new(),
            started_at: timestamp.unwrap_or(builder.last_seen_at),
            completed_at: timestamp,
        });
    }

    fn close_current_agent(&mut self, status: TurnStatus) {
        // A replay interval cannot outlive its turn. Post-turn completions are
        // routed and replayed into their producer turn by the prepass; leaving
        // this slot open would only let malformed future input claim stale
        // reconstruction state.
        self.open_wrapper = None;
        let Some(builder) = self.current_agent.take() else {
            return;
        };
        self.turns.push(Turn::Agent {
            turn_id: builder.turn_id,
            agent_id: builder.agent_id,
            started_at: builder.started_at,
            ended_at: Some(builder.last_seen_at),
            status,
            items: drop_superseded_rows(builder.items, &builder.superseded_rows),
            usage: builder.usage,
            // Per-turn model + effort from this turn's `turn_context` (last-wins
            // up to this close). Distinct from the first-wins `meta.model`.
            model: self.current_model.clone(),
            effort: self.current_effort.clone(),
            // Codex has no cost/overage and no Claude-style `stable_message_id`
            // cost-join key, but its `turn_context.turn_id` is a re-parse-stable
            // per-turn hydration key. The live adapter now emits the same id on
            // `TurnEnd` (sourced from the post-terminal enrichment re-read — the
            // durable send↔turn `TurnLink`), so link-eligibility is probe-verified.
            // It is **not** refresh-eligible: the live key arrives only at
            // terminal, or on the cancel path's session-file read — never
            // mid-stream during a live turn — so `supports_refresh` stays off.
            spend: None,
            hydration_key: builder.hydration_key,
            continuation_of: None,
            stable_message_id: None,
        });
    }

    fn finalize(mut self) -> LoadedTranscript {
        // Any in-progress agent turn at EOF is truncated — no task_complete
        // observed before EOF. **Asymmetric with Claude on purpose**: Codex
        // emits an explicit `event_msg/task_complete` per turn, so a missing
        // one means genuine truncation. Claude's session file has no
        // analogous terminal marker; its `finalize` defaults to Complete
        // instead. See `crates/harness/src/claude_code/session_file.rs::
        // ReconstructionState::finalize` for the other side of the
        // asymmetry.
        self.close_current_agent(TurnStatus::Failed);
        LoadedTranscript {
            turns: self.turns,
            meta: None,
            last_rate_limit: None,
            last_rate_limit_as_of: None,
            warnings: self.warnings,
        }
    }
}

/// Structured output of a `CommandExecution` item: `aggregated_output`, else
/// non-empty stdout/stderr joined. Callers decide whether an empty result is
/// written (success: yes — blank beats wrapper boilerplate) or left for the
/// wrapper output to fill (failure: the wrapper often holds the only
/// diagnostic).
fn command_execution_output(item: &Value) -> String {
    if let Some(aggregated) = item
        .get("aggregated_output")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
    {
        return aggregated.to_owned();
    }
    ["stdout", "stderr"]
        .iter()
        .filter_map(|field| item.get(*field).and_then(Value::as_str))
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Shell facet for a `CommandExecution` item. The `command` array is the real
/// argv (`["/bin/zsh", "-lc", "<cmd>"]` in every capture); unwrap the standard
/// shell wrapper for display, else join the argv. `cwd` is a `file://` URI.
fn command_execution_item_facet(item: &Value) -> crate::facets::ToolFacet {
    let Some(argv) = item.get("command").and_then(Value::as_array) else {
        return crate::facets::ToolFacet::Other;
    };
    let parts: Vec<&str> = argv.iter().filter_map(Value::as_str).collect();
    if parts.len() != argv.len() || parts.is_empty() {
        return crate::facets::ToolFacet::Other;
    }
    let command = if parts.len() == 3 && (parts[1] == "-lc" || parts[1] == "-c") {
        parts[2].to_owned()
    } else {
        parts.join(" ")
    };
    let cwd = item
        .get("cwd")
        .and_then(Value::as_str)
        .map(|uri| uri.strip_prefix("file://").unwrap_or(uri).to_owned());
    crate::facets::ToolFacet::Shell { command, cwd }
}

fn command_execution_signature(item: &Value) -> Option<CommandSignature> {
    CommandSignature::from_facet(&command_execution_item_facet(item))
}

/// Decode a **legacy** `mcp_tool_call_end.result` envelope:
/// - `{"Ok": {"content": [...], "isError": false}}`
/// - `{"Err": "error message"}`
///
/// Returns `(output_string, is_error)`. The `Ok` payload routes through the
/// live parser's [`super::parser::extract_mcp_output`] — the same function the
/// paginated `McpToolCall` path calls directly — so all three surfaces (live
/// stream, legacy disk, paginated disk) decode content identically.
///
/// **Deliberate legacy behavior change (parity fix):** a legacy `Ok` payload
/// whose content is all-non-text or empty previously hydrated as `""`; it now
/// yields the live path's `[non-text tool result omitted]` placeholder. Live
/// streams never carried the legacy envelope, so legacy threads had the exact
/// live/disk divergence this fixes — the unchanged legacy fixtures are not
/// evidence of a frozen legacy path here, they just carry no non-text MCP
/// content.
fn decode_mcp_result(result: Option<&Value>) -> (String, bool) {
    let Some(result) = result.filter(|v| !v.is_null()) else {
        return (String::new(), false);
    };
    if let Some(ok) = result.get("Ok") {
        let is_error = ok.get("isError").and_then(Value::as_bool).unwrap_or(false);
        (super::parser::extract_mcp_output(Some(ok), None), is_error)
    } else if let Some(err) = result.get("Err") {
        let msg = err.as_str().unwrap_or("").to_owned();
        (msg, true)
    } else {
        (String::new(), false)
    }
}

fn decode_function_call_output(output: Option<&Value>) -> String {
    let Some(output) = output else {
        return String::new();
    };
    if let Some(text) = output.as_str() {
        return text.to_owned();
    }
    output.as_array().map_or_else(
        || serde_json::to_string(output).unwrap_or_default(),
        |blocks| {
            blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<String>()
        },
    )
}

fn patch_apply_end_output(payload: &Value) -> String {
    ["stdout", "stderr"]
        .into_iter()
        .filter_map(|field| payload.get(field).and_then(Value::as_str))
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CommandSignature {
    command: String,
    cwd: Option<String>,
}

impl CommandSignature {
    fn from_facet(facet: &crate::facets::ToolFacet) -> Option<Self> {
        let crate::facets::ToolFacet::Shell { command, cwd } = facet else {
            return None;
        };
        Some(Self {
            command: command.clone(),
            cwd: cwd.clone(),
        })
    }
}

/// Exact semantic compatibility for one canonical command.
///
/// Command text is never normalized fuzzily. A cwd rejects a match only when
/// both records supply directly comparable values; if one side omits it, the
/// command may still identify one wrapper, while multiple compatible wrappers
/// remain ambiguous in the resolver.
fn command_signatures_match(left: &CommandSignature, right: &CommandSignature) -> bool {
    left.command == right.command
        && match (&left.cwd, &right.cwd) {
            (Some(left), Some(right)) => left == right,
            _ => true,
        }
}

struct DecodedExecWrapper {
    facet: crate::facets::ToolFacet,
    command_signature: CommandSignature,
    emits_full_result: bool,
}

/// Decode only the canonical single-call wrapper emitted by Codex. Arbitrary
/// JavaScript, dynamic arguments, and wrappers that batch operations stay
/// generic because one durable call id/output cannot represent their nested
/// operations faithfully.
fn decode_single_exec_wrapper(script: &str) -> Option<DecodedExecWrapper> {
    let rest = script.trim().strip_prefix("const ")?;
    let binding_end = rest
        .find(|character: char| !is_javascript_identifier_continue(character))
        .unwrap_or(rest.len());
    let binding = rest.get(..binding_end)?;
    if !binding
        .chars()
        .next()
        .is_some_and(is_javascript_identifier_start)
    {
        return None;
    }

    let rest = rest.get(binding_end..)?.trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    let rest = rest.strip_prefix("await")?.trim_start();
    let rest = rest.strip_prefix("tools.exec_command(")?;
    let arguments_end = js_object_span(rest)?;
    let arguments = parse_js_object(rest.get(..arguments_end)?)?;
    let rest = rest.get(arguments_end..)?.trim_start();
    let rest = rest.strip_prefix(')')?.trim_start();
    let rest = rest.strip_prefix(';')?.trim_start();

    let full_result = format!("text({binding});");
    let output_only = format!("text({binding}.output);");
    let emits_full_result = if rest == full_result {
        true
    } else if rest == output_only {
        false
    } else {
        return None;
    };

    let facet = super::facets::exec_command_facet(&arguments);
    let command_signature = CommandSignature::from_facet(&facet)?;
    Some(DecodedExecWrapper {
        facet,
        command_signature,
        emits_full_result,
    })
}

/// Byte offset just past the `{…}` (or `[…]`) literal starting at `source[0]`,
/// tracking string state so a brace inside a quoted value doesn't close it.
/// `None` if `source` doesn't open with a literal or the literal is unclosed.
fn js_object_span(source: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, character) in source.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '{' | '[' => depth += 1,
            '}' | ']' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index + character.len_utf8());
                }
            }
            // Anything before the opening brace means this isn't a literal.
            _ if depth == 0 && !character.is_whitespace() => return None,
            _ => {}
        }
    }
    None
}

/// Parse the argument literal of a `tools.exec_command(…)` call.
///
/// Codex writes a **JavaScript** object literal, not JSON, and the shape has
/// changed across releases: 0.149 writes bare identifier keys over several
/// lines (`{\n  cmd: "…",\n  workdir: "…"\n}`) where earlier releases wrote
/// single-line quoted JSON (`{"cmd":"…"}`). Both must decode — a rollout is
/// read long after the CLI that wrote it, and a decode failure here is silent
/// (the wrapper degrades to an unrecognized tool showing raw script text), so
/// it cannot be left to whichever form the fixtures happen to carry.
fn parse_js_object(source: &str) -> Option<Value> {
    serde_json::from_str::<Value>(source)
        .or_else(|_| serde_json::from_str::<Value>(&quote_bare_keys(source)))
        .ok()
}

/// Rewrite bare identifier keys as quoted JSON keys, leaving strings untouched.
///
/// A key is only quoted when the identifier is followed by `:` — without that
/// check the `false` in `[true, false]` would be quoted into a string, since it
/// sits in the same after-a-comma position a key does.
fn quote_bare_keys(source: &str) -> String {
    let mut out = String::with_capacity(source.len() + 16);
    let mut characters = source.char_indices().peekable();
    let mut in_string = false;
    let mut escaped = false;
    let mut at_key_position = false;
    while let Some((index, character)) = characters.next() {
        if in_string {
            out.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        if character.is_whitespace() {
            out.push(character);
            continue;
        }
        match character {
            '"' => {
                in_string = true;
                at_key_position = false;
                out.push(character);
            }
            '{' | ',' => {
                at_key_position = true;
                out.push(character);
            }
            _ if at_key_position && is_javascript_identifier_start(character) => {
                let mut end = index + character.len_utf8();
                while let Some(&(next_index, next)) = characters.peek() {
                    if is_javascript_identifier_continue(next) {
                        end = next_index + next.len_utf8();
                        characters.next();
                    } else {
                        break;
                    }
                }
                let identifier = &source[index..end];
                if source[end..].trim_start().starts_with(':') {
                    out.push('"');
                    out.push_str(identifier);
                    out.push('"');
                } else {
                    out.push_str(identifier);
                }
                at_key_position = false;
            }
            _ => {
                at_key_position = false;
                out.push(character);
            }
        }
    }
    out
}

fn is_javascript_identifier_start(character: char) -> bool {
    character == '_' || character == '$' || character.is_ascii_alphabetic()
}

fn is_javascript_identifier_continue(character: char) -> bool {
    is_javascript_identifier_start(character) || character.is_ascii_digit()
}

fn function_call_output_is_error(output: &str, input: &Value) -> bool {
    if output.lines().next() == Some("Script failed") {
        return true;
    }
    if output_exit_code(output).is_some_and(|code| code != 0) {
        return true;
    }
    input.as_str().is_some_and(|script| {
        decode_single_exec_wrapper(script).is_some_and(|decoded| decoded.emits_full_result)
    }) && structured_script_exit_code(output).is_some_and(|code| code != 0)
}

fn structured_script_exit_code(output: &str) -> Option<i64> {
    let body = output.split_once("\nOutput:\n")?.1;
    body.lines().find_map(|line| {
        serde_json::from_str::<Value>(line)
            .ok()?
            .get("exit_code")?
            .as_i64()
    })
}

fn output_exit_code(output: &str) -> Option<i64> {
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed == "Output:" {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("Process exited with code ")
            && let Some(code) = rest.split_whitespace().next()
            && let Ok(parsed) = code.parse()
        {
            return Some(parsed);
        }
    }
    None
}

/// Discriminate built-in vs MCP function-call name. MCP calls carry a
/// `namespace: "mcp__<server>__"` field; the surfaced name is
/// `<server>.<tool>` (matching the stream-side emission).
fn classify_codex_function_call(name: &str, namespace: Option<&str>) -> (ToolKind, String) {
    if let Some(server) = codex_mcp_server(namespace) {
        return (ToolKind::Mcp, format!("{server}.{name}"));
    }
    (ToolKind::Builtin, name.to_owned())
}

fn codex_mcp_server(namespace: Option<&str>) -> Option<&str> {
    namespace
        .and_then(|value| value.strip_prefix("mcp__"))
        .map(|value| value.trim_end_matches("__"))
}

/// Apply an MCP completion to the matching open tool item. Returns `true`
/// when a matching tool was found and patched.
fn apply_mcp_result(items: &mut [TurnItem], call_id: &str, result: &McpResult) -> bool {
    for item in items {
        if let TurnItem::Tool {
            tool_use_id,
            kind,
            facet,
            name,
            input,
            output,
            is_error,
            completed_at,
            ..
        } = item
            && tool_use_id == call_id
        {
            *kind = ToolKind::Mcp;
            if !result.server.is_empty() && !result.tool.is_empty() {
                *name = format!("{}.{}", result.server, result.tool);
                // A namespace-less function call learns its MCP identity only
                // from this result. Reclassify the retained arguments so that
                // correcting provenance cannot discard mutation semantics.
                *facet =
                    crate::facets::classify_mcp_tool_facet(&result.server, &result.tool, input);
            }
            *output = Some(result.output.clone());
            *is_error = Some(result.is_error);
            *completed_at = result.completed_at;
            return true;
        }
    }
    false
}

// ## Codex rollout fixtures
//
// Two rollout generations exist on disk, distinguished by
// `session_meta.history_mode`. Files with **no** `history_mode` field predate
// it and are legacy by definition; the field itself is the only durable
// predicate (never a CLI-version comparison — a paginated-capable CLI still
// writes legacy files when its store rejects pagination).
//
// Legacy fixtures (`exec-wrapper`, `apply-patch`, `mcp-content-mutations`, …)
// carry the `event_msg` prompt/answer/edit/MCP records. The `paginated-*`
// fixtures below were captured from real codex-cli 0.149.0 sessions, where
// those records are no longer written and the same content arrives on
// `event_msg/item_completed` instead. Every record carries a synthetic
// `timestamp`, as the legacy fixtures do: the parser falls back to `Utc::now()`
// on absence, which would make any chronology assertion wall-clock dependent.
//
// Each fixture pins one shape:
//
// - `paginated-text-only` — prompt + answer via `UserMessage`/`AgentMessage`,
//   each alongside the `response_item/message` twin that carries the same
//   content, so a parser that reads both double-counts. Also pins the
//   inconsistent block casing (`UserMessage` uses `"text"`, `AgentMessage`
//   uses `"Text"`) — read the block's `text` field, never gate on the tag.
// - `paginated-single-command` — one `exec` wrapper containing exactly one
//   command. Pins that this renders as **one** row, not a wrapper row plus a
//   child row.
// - `paginated-batched-wrapper` — one `exec` wrapper whose script calls
//   `apply_patch` *and* `exec_command`, emitting two `item_completed` items
//   against a single `call_id`. Pins that each operation gets its own row;
//   the item ids (`exec-<uuid>`) match neither the wrapper's `id` (`ctc_…`)
//   nor its `call_id`, so the association cannot be an id join.
// - `paginated-mixed-batch` — one wrapper where the first operation succeeds
//   (emitting a child) and the second fails **uncaught**, aborting the script.
//   The failure exists only on the wrapper's output (`Script failed` + the
//   diagnostic), so this is the fixture that pins *retaining the wrapper
//   alongside its children*. Ordering is load-bearing and was established by
//   probe: a failing command still emits its own failed item, and a *caught*
//   failure vanishes entirely (no item, and the wrapper reports success), so
//   neither of those shapes can pin this rule.
// - `paginated-failed-tool` — a failed `apply_patch` wrapper that emits **no**
//   `item_completed` at all; the failure survives only on the wrapper's
//   `custom_tool_call_output`. This is why `response_item` stays the canonical
//   source for tool rows: `item_completed` is not a complete record of tool
//   activity.
// - `paginated-mcp` — MCP calls ride the same `exec` wrapper as everything
//   else (two calls, one wrapper), covering all three envelopes: success
//   (`status: "completed"`), a tool-reported error (`status: "failed"` with
//   `result.isError: true`), and a transport failure (`result: null` with a
//   top-level `error`). The live stream parser already distinguishes the last
//   two; disk hydration must match it or protocol failures reopen blank. The
//   transport record is **source-derived** from upstream's `McpToolCallError`
//   — a real transport failure could not be forced within the live-test cost
//   discipline — while the other two are captured.
// - `unknown-history-mode` — an unrecognized mode whose records the legacy
//   fallback *can* still read. Pins that an unknown mode warns even when
//   nothing is lost, and that a backward-compatible future format still
//   hydrates through the fallback.
// - `unknown-history-mode-degraded` — the same unrecognized mode over
//   `item_completed`-only content, so the fallback genuinely finds no text.
//   Pins the outcome the warning exists for: boundaries/usage/model survive,
//   text is empty, and a warning is present. Deliberately synthetic — no such
//   format exists yet; it models the next one behaving as paginated did.
// - `legacy-explicit-mode` — `history_mode: "legacy"` stated outright, as
//   opposed to the absent-field case every older fixture covers.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::facets::ToolFacet;
    use chrono::NaiveDate;
    use std::sync::Mutex;
    use tempfile::TempDir;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct ToolSnapshot {
        tool_use_id: String,
        name: String,
        input: Value,
        facet: crate::facets::ToolFacet,
        output: Option<String>,
        is_error: Option<bool>,
    }

    fn fixture_path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/codex")
            .join(name)
    }

    fn hydrated_tool_snapshots(content: &str) -> Vec<ToolSnapshot> {
        parse_codex_transcript_content(content, Uuid::now_v7())
            .turns
            .into_iter()
            .filter_map(|turn| match turn {
                Turn::Agent { items, .. } => Some(items),
                Turn::User { .. } | Turn::System { .. } => None,
            })
            .flatten()
            .filter_map(|item| match item {
                TurnItem::Tool {
                    tool_use_id,
                    name,
                    input,
                    facet,
                    output,
                    is_error,
                    ..
                } => Some(ToolSnapshot {
                    tool_use_id,
                    name,
                    input,
                    facet,
                    output,
                    is_error,
                }),
                TurnItem::Text { .. } => None,
            })
            .collect()
    }

    fn apply_live_tool_event(
        snapshots: &mut Vec<ToolSnapshot>,
        event: crate::events::AdapterEvent,
    ) {
        match event {
            crate::events::AdapterEvent::ToolStarted {
                tool_use_id,
                name,
                input,
                facet,
                ..
            } => snapshots.push(ToolSnapshot {
                tool_use_id,
                name,
                input,
                facet,
                output: None,
                is_error: None,
            }),
            crate::events::AdapterEvent::ToolCompleted {
                tool_use_id,
                output,
                is_error,
                ..
            } => {
                let snapshot = snapshots
                    .iter_mut()
                    .find(|snapshot| snapshot.tool_use_id == tool_use_id)
                    .unwrap_or_else(|| panic!("completion without start for {tool_use_id}"));
                snapshot.output = Some(output);
                snapshot.is_error = Some(is_error);
            }
            _ => {}
        }
    }

    fn live_tool_snapshots(content: &str) -> Vec<ToolSnapshot> {
        let mut state = crate::codex::parser::CodexParserState::default();
        let turn_id = Uuid::now_v7();
        let mut snapshots = Vec::new();
        for line in content.lines().filter(|line| !line.trim().is_empty()) {
            match crate::codex::parser::parse_line(line, turn_id, &mut state) {
                crate::parser::ParseOutcome::Event(event) => {
                    apply_live_tool_event(&mut snapshots, event);
                }
                crate::parser::ParseOutcome::Events(events) => {
                    for event in events {
                        apply_live_tool_event(&mut snapshots, event);
                    }
                }
                crate::parser::ParseOutcome::Skip => {}
                crate::parser::ParseOutcome::Error(error) => {
                    panic!("stream fixture parse failed: {error}");
                }
            }
        }
        snapshots
    }

    #[test]
    fn parse_rate_limits_fixture_extracts_all_four_fields() {
        let content = std::fs::read_to_string(fixture_path("rate-limits.session.jsonl")).unwrap();
        let enrichment = parse_session_content(&content);

        assert_eq!(
            enrichment.context_window,
            Some(258_400),
            "task_started.model_context_window must be extracted"
        );
        assert_eq!(
            enrichment.cli_version.as_deref(),
            Some("0.130.0"),
            "session_meta.cli_version must be extracted"
        );
        assert!(enrichment.rate_limits.is_some(), "rate_limits extracted");
        assert!(
            enrichment.session_meta_raw.is_some(),
            "session_meta line preserved as raw"
        );
        // The fixture has no turn_context records — model stays None.
        assert!(
            enrichment.model.is_none(),
            "no turn_context in fixture → model is None"
        );
    }

    #[test]
    fn parse_extracts_model_from_first_turn_context() {
        let content = r#"
{"type":"session_meta","payload":{"cli_version":"0.130.0"}}
{"type":"turn_context","payload":{"model":"gpt-5.5","cwd":"/x"}}
{"type":"turn_context","payload":{"model":"gpt-5.6","cwd":"/x"}}
"#;
        let enrichment = parse_session_content(content);
        assert_eq!(
            enrichment.model.as_deref(),
            Some("gpt-5.5"),
            "first turn_context.model wins"
        );
    }

    #[test]
    fn enrichment_current_turn_id_is_this_turns_turn_context_turn_id() {
        // The durable send↔turn key: `current_turn_id` is the CURRENT turn's
        // `turn_context.turn_id` — the tail turn's, matching what the reload parser
        // stamps on that turn's `hydration_key`. On a resumed multi-turn file the
        // last turn_context (this turn's) wins.
        let content = r#"
{"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}
{"type":"turn_context","payload":{"model":"gpt-5.5","turn_id":"turn-one"}}
{"type":"event_msg","payload":{"type":"task_started","turn_id":"t-2"}}
{"type":"turn_context","payload":{"model":"gpt-5.5","turn_id":"turn-two"}}
"#;
        let enrichment = parse_session_content(content);
        assert_eq!(
            enrichment.current_turn_id.as_deref(),
            Some("turn-two"),
            "current_turn_id is the tail turn's turn_context.turn_id"
        );
    }

    #[test]
    fn enrichment_current_turn_id_resets_when_current_turn_has_no_turn_context() {
        // Stale-key guard (load-bearing): a turn that opens (`task_started`) but
        // writes NO `turn_context` must read as no-key — never inherit the prior
        // turn's id. A stale key would mis-link this turn to an old send. The reset
        // at `task_started` (mirroring the parser's fresh-per-turn builder) is what
        // guarantees live-key == parsed-key by construction.
        let content = r#"
{"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}
{"type":"turn_context","payload":{"model":"gpt-5.5","turn_id":"turn-one"}}
{"type":"event_msg","payload":{"type":"task_started","turn_id":"t-2"}}
"#;
        let enrichment = parse_session_content(content);
        assert_eq!(
            enrichment.current_turn_id, None,
            "a turn with no turn_context must read no-key, not the predecessor's id"
        );
    }

    #[test]
    fn parse_filters_token_count_info_only_variant() {
        // The info-only token_count (rate_limits: null) must not populate
        // rate_limits; only the rate-limits-bearing variant feeds
        // RateLimitEvent. A degenerate info (no parseable token fields) must
        // not fabricate a zero-valued per_turn_usage either — that would
        // replace genuine stream telemetry in the adapter overlay.
        let content = r#"
{"type":"session_meta","payload":{"cli_version":"0.130.0"}}
{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{}},"rate_limits":null}}
"#;
        let enrichment = parse_session_content(content);
        assert!(
            enrichment.rate_limits.is_none(),
            "info-only token_count must not populate rate_limits"
        );
        assert!(
            enrichment.per_turn_usage.is_none(),
            "degenerate info must not fabricate zero-valued per-turn usage"
        );
    }

    #[test]
    fn parse_extracts_per_turn_usage_from_last_token_usage_not_totals() {
        // `info.last_token_usage` (final request = context occupancy) is the
        // per-turn source; `total_token_usage` is the thread-cumulative
        // counter and must be ignored. Last info-bearing record wins.
        let content = r#"
{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":2411599,"output_tokens":17372},"last_token_usage":{"input_tokens":141496,"cached_input_tokens":137600,"output_tokens":709,"reasoning_output_tokens":417}},"rate_limits":null}}
{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":2555875,"output_tokens":18095},"last_token_usage":{"input_tokens":144276,"cached_input_tokens":141184,"output_tokens":723,"reasoning_output_tokens":298}},"rate_limits":null}}
"#;
        let enrichment = parse_session_content(content);
        let usage = enrichment.per_turn_usage.expect("per-turn usage captured");
        assert_eq!(usage.input_tokens, 144_276, "last record wins");
        assert_eq!(usage.output_tokens, 723);
        assert_eq!(usage.cached_input_tokens, Some(141_184));
        assert_eq!(usage.reasoning_output_tokens, Some(298));
        assert_eq!(
            usage.context_input_tokens,
            Some(144_276),
            "occupancy is the final request's input side, not the cumulative total"
        );
        assert_eq!(
            usage.context_window, None,
            "window is overlaid separately from task_started"
        );
    }

    #[test]
    fn token_count_overflow_preserves_raw_tokens_and_hides_derived_occupancy() {
        let info = serde_json::json!({
            "last_token_usage": {
                "input_tokens": u64::MAX,
                "output_tokens": 1
            }
        });

        let usage = turn_usage_from_token_count_info(&info, Some(200_000))
            .expect("raw token fields remain valid");

        assert_eq!(usage.input_tokens, u64::MAX);
        assert_eq!(usage.output_tokens, 1);
        assert_eq!(usage.context_input_tokens, Some(u64::MAX));
        assert_eq!(usage.context_tokens_after_turn, None);
        assert_eq!(usage.context_window, Some(200_000));
    }

    #[test]
    fn parse_resets_per_turn_usage_at_task_started_boundary() {
        // A new turn that has written no parseable token_count yet must read
        // as "unknown" — never inherit the previous turn's usage (which would
        // stamp stale telemetry onto the current TurnEnd, and mask the flush
        // race where enrichment reads before this turn's token_count lands).
        let content = r#"
{"type":"event_msg","payload":{"type":"task_started","model_context_window":258400}}
{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":141496,"output_tokens":709}},"rate_limits":null}}
{"type":"event_msg","payload":{"type":"task_started","model_context_window":258400}}
"#;
        let enrichment = parse_session_content(content);
        assert!(
            enrichment.per_turn_usage.is_none(),
            "task_started must reset per_turn_usage — a prior turn's usage must not survive the boundary"
        );
    }

    #[test]
    fn parse_repopulates_per_turn_usage_after_task_started_reset() {
        // The current turn's own token_count (after the boundary reset)
        // populates the field with the current turn's values.
        let content = r#"
{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":141496,"output_tokens":709}},"rate_limits":null}}
{"type":"event_msg","payload":{"type":"task_started","model_context_window":258400}}
{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":144276,"output_tokens":723}},"rate_limits":null}}
"#;
        let enrichment = parse_session_content(content);
        let usage = enrichment
            .per_turn_usage
            .expect("current turn's usage captured");
        assert_eq!(
            usage.input_tokens, 144_276,
            "current turn's record, not the pre-boundary one"
        );
        assert_eq!(usage.output_tokens, 723);
    }

    #[test]
    fn parse_takes_last_task_started_for_context_window() {
        // Resumed-session file: two task_started records, second turn's
        // model_context_window is what we want for the current turn.
        let content = r#"
{"type":"session_meta","payload":{"cli_version":"0.130.0"}}
{"type":"event_msg","payload":{"type":"task_started","model_context_window":200000}}
{"type":"event_msg","payload":{"type":"task_started","model_context_window":300000}}
"#;
        let enrichment = parse_session_content(content);
        assert_eq!(
            enrichment.context_window,
            Some(300_000),
            "last task_started wins"
        );
    }

    #[test]
    fn parse_tracks_last_task_started_timestamp_and_fails_closed_without_one() {
        // The freshness proof for cancel-path identity recovery: the LAST
        // task_started's record-level timestamp, and `None` (fail-closed —
        // the cancel path's guard rejects) when that record carries no
        // parseable timestamp, even if an earlier one did.
        let with_ts = r#"
{"timestamp":"2026-01-01T12:00:00.100Z","type":"event_msg","payload":{"type":"task_started"}}
{"timestamp":"2026-01-01T12:00:05.200Z","type":"event_msg","payload":{"type":"task_started"}}
"#;
        assert_eq!(
            parse_session_content(with_ts).current_turn_started_at,
            Some("2026-01-01T12:00:05.200Z".parse().unwrap()),
            "the last task_started's timestamp is the current turn's start"
        );

        let tail_missing_ts = r#"
{"timestamp":"2026-01-01T12:00:00.100Z","type":"event_msg","payload":{"type":"task_started"}}
{"type":"event_msg","payload":{"type":"task_started"}}
"#;
        assert_eq!(
            parse_session_content(tail_missing_ts).current_turn_started_at,
            None,
            "a tail task_started without a timestamp reads as no-proof (fail-closed), never an earlier record's value"
        );
    }

    #[test]
    fn parse_takes_last_rate_limit_bearing_token_count() {
        let content = r#"
{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":10.0}}}}
{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":50.0}}}}
"#;
        let enrichment = parse_session_content(content);
        let rate_limits = enrichment.rate_limits.expect("rate_limits captured");
        // The second record's percent must win.
        assert_eq!(
            rate_limits.pointer("/primary/used_percent"),
            Some(&Value::from(50.0))
        );
    }

    #[test]
    fn parse_strips_base_instructions_text_from_raw() {
        let content = r#"
{"type":"session_meta","payload":{"cli_version":"0.130.0","base_instructions":{"text":"this is a very long system prompt that would bloat IPC"}}}
"#;
        let enrichment = parse_session_content(content);
        let raw = enrichment.session_meta_raw.expect("raw captured");
        assert_eq!(
            raw.pointer("/payload/base_instructions/text"),
            Some(&Value::String(
                "<stripped — see codex-cli-observed.md>".to_owned()
            )),
            "base_instructions.text must be stripped"
        );
        // The surrounding shape is preserved so future consumers can introspect.
        assert_eq!(
            raw.pointer("/payload/cli_version"),
            Some(&Value::String("0.130.0".to_owned())),
            "non-stripped fields preserved"
        );
    }

    #[test]
    fn parse_handles_missing_base_instructions_gracefully() {
        // No base_instructions table at all — must not panic.
        let content = r#"{"type":"session_meta","payload":{"cli_version":"0.130.0"}}"#;
        let enrichment = parse_session_content(content);
        let raw = enrichment.session_meta_raw.expect("raw captured");
        assert!(raw.pointer("/payload/cli_version").is_some());
    }

    #[test]
    fn parse_skips_malformed_lines_keeps_valid() {
        let content = r#"
{"type":"session_meta","payload":{"cli_version":"0.130.0"}}
not valid json
{"type":"event_msg","payload":{"type":"task_started","model_context_window":100}}
"#;
        let enrichment = parse_session_content(content);
        assert_eq!(enrichment.cli_version.as_deref(), Some("0.130.0"));
        assert_eq!(enrichment.context_window, Some(100));
    }

    #[test]
    fn locate_session_file_finds_matching_suffix() {
        let tmp = TempDir::new().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 5, 15).unwrap();
        let session_id = "019e2c5f-aaaa-7000-8000-000000000001";
        let dir = session_directory(tmp.path(), date);
        std::fs::create_dir_all(&dir).unwrap();
        // The actual file with the matching suffix.
        let target = dir.join(format!("rollout-1747000000000-{session_id}.jsonl"));
        std::fs::write(&target, "{}\n").unwrap();
        // A decoy file with a different suffix.
        std::fs::write(
            dir.join("rollout-1747000000000-other-session.jsonl"),
            "{}\n",
        )
        .unwrap();

        let found = locate_session_file(tmp.path(), date, session_id);
        assert_eq!(found.as_deref(), Some(target.as_path()));
    }

    #[test]
    fn locate_session_file_returns_none_when_directory_missing() {
        let tmp = TempDir::new().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 5, 15).unwrap();
        // No directory exists at all.
        assert!(locate_session_file(tmp.path(), date, "any-id").is_none());
    }

    #[test]
    fn locate_session_file_picks_newest_mtime_on_multi_match() {
        // Real Codex would never produce two rollouts with the same session
        // UUID, but a backup/rename script could. The plan says "if
        // multiple matches, pick most recent" — pin that against the
        // `read_dir`-order ambiguity.
        let tmp = TempDir::new().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 5, 15).unwrap();
        let session_id = "019e2c5f-aaaa-7000-8000-0000000000aa";
        let dir = session_directory(tmp.path(), date);
        std::fs::create_dir_all(&dir).unwrap();
        let older = dir.join(format!("rollout-1000-{session_id}.jsonl"));
        let newer = dir.join(format!("rollout-9999-{session_id}.jsonl"));
        std::fs::write(&older, "older").unwrap();
        // Sleep just enough to give the newer file a distinct mtime on
        // filesystems with second-resolution timestamps. macOS HFS+ is
        // second-resolution; APFS / ext4 are nanosecond. 1100ms is the
        // tightest cross-platform guarantee.
        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(&newer, "newer").unwrap();

        let found = locate_session_file(tmp.path(), date, session_id);
        assert_eq!(
            found.as_deref(),
            Some(newer.as_path()),
            "newest mtime wins on multi-match"
        );
    }

    #[test]
    fn locate_session_file_ignores_non_rollout_files() {
        let tmp = TempDir::new().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 5, 15).unwrap();
        let session_id = "019e2c5f-aaaa-7000-8000-000000000001";
        let dir = session_directory(tmp.path(), date);
        std::fs::create_dir_all(&dir).unwrap();
        // File ends in the right suffix but isn't a rollout file.
        std::fs::write(dir.join(format!("other-{session_id}.jsonl")), "{}\n").unwrap();

        assert!(locate_session_file(tmp.path(), date, session_id).is_none());
    }

    #[test]
    fn locate_session_file_finds_cross_day_when_pointed_at_yesterday() {
        // Cross-midnight test: sidecar's session_partition_date says May
        // 15; host clock would say May 16. Lookup must use the sidecar's
        // stored date (never recompute from any clock) and find the file
        // in May 15's directory.
        let tmp = TempDir::new().unwrap();
        let yesterday = NaiveDate::from_ymd_opt(2026, 5, 15).unwrap();
        let session_id = "019e2c5f-aaaa-7000-8000-000000000001";
        let yesterday_dir = session_directory(tmp.path(), yesterday);
        std::fs::create_dir_all(&yesterday_dir).unwrap();
        let target = yesterday_dir.join(format!("rollout-x-{session_id}.jsonl"));
        std::fs::write(&target, "{}\n").unwrap();

        // Pointed at yesterday → found.
        assert_eq!(
            locate_session_file(tmp.path(), yesterday, session_id).as_deref(),
            Some(target.as_path())
        );
        // Pointed at today → not found (file is in yesterday's dir).
        let today = NaiveDate::from_ymd_opt(2026, 5, 16).unwrap();
        assert!(locate_session_file(tmp.path(), today, session_id).is_none());
    }

    fn write_rollout(tmp: &Path, date: NaiveDate, session_id: &str) -> PathBuf {
        let dir = session_directory(tmp, date);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-1747000000000-{session_id}.jsonl"));
        std::fs::write(&path, "{}\n").unwrap();
        path
    }

    #[test]
    fn find_for_attach_returns_path_and_parsed_date_on_single_match() {
        let tmp = TempDir::new().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 5, 15).unwrap();
        let session_id = "019e2c5f-aaaa-7000-8000-000000000001";
        let target = write_rollout(tmp.path(), date, session_id);

        let (path, parsed_date) =
            find_codex_session_file_for_attach(tmp.path(), session_id).unwrap();
        assert_eq!(path, target);
        assert_eq!(parsed_date, date);
    }

    #[test]
    fn find_for_attach_scans_all_date_partitions() {
        // The caller doesn't know the original spawn date; we walk the
        // YYYY/MM/DD tree to find any match.
        let tmp = TempDir::new().unwrap();
        let session_id = "019e2c5f-bbbb-7000-8000-000000000002";
        let date_old = NaiveDate::from_ymd_opt(2025, 12, 1).unwrap();
        let _decoy = write_rollout(tmp.path(), date_old, "different-session-id");
        let date_target = NaiveDate::from_ymd_opt(2026, 4, 20).unwrap();
        let target = write_rollout(tmp.path(), date_target, session_id);

        let (path, parsed_date) =
            find_codex_session_file_for_attach(tmp.path(), session_id).unwrap();
        assert_eq!(path, target);
        assert_eq!(parsed_date, date_target);
    }

    #[test]
    fn find_for_attach_returns_not_found_when_no_match() {
        let tmp = TempDir::new().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 5, 15).unwrap();
        let _other = write_rollout(tmp.path(), date, "different-session-id");

        let err = find_codex_session_file_for_attach(tmp.path(), "nope-session-id").unwrap_err();
        assert!(
            matches!(err, AttachLookupError::NotFound { ref session_id } if session_id == "nope-session-id")
        );
    }

    #[test]
    fn find_for_attach_returns_not_found_when_sessions_root_missing() {
        // Empty tmp dir, no ~/.codex/sessions/ at all.
        let tmp = TempDir::new().unwrap();
        let err = find_codex_session_file_for_attach(tmp.path(), "any-id").unwrap_err();
        assert!(matches!(err, AttachLookupError::NotFound { .. }));
    }

    #[test]
    fn find_for_attach_fails_loud_on_ambiguous_match() {
        // Same session_id under two date partitions — impossible by Codex's
        // design (UUIDs are unique), but if it happens (manual copy, FS
        // weirdness), attach must surface it rather than binding arbitrarily.
        let tmp = TempDir::new().unwrap();
        let session_id = "019e2c5f-cccc-7000-8000-000000000003";
        let date_a = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let date_b = NaiveDate::from_ymd_opt(2026, 2, 2).unwrap();
        let path_a = write_rollout(tmp.path(), date_a, session_id);
        let path_b = write_rollout(tmp.path(), date_b, session_id);

        let err = find_codex_session_file_for_attach(tmp.path(), session_id).unwrap_err();
        match err {
            AttachLookupError::Ambiguous {
                session_id: id,
                paths,
            } => {
                assert_eq!(id, session_id);
                assert!(paths.contains(&path_a));
                assert!(paths.contains(&path_b));
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn find_for_attach_skips_non_numeric_directory_entries() {
        // Defensive: macOS .DS_Store at year/month/day levels must not break
        // the scan. The valid rollout under a real numeric tree still resolves.
        let tmp = TempDir::new().unwrap();
        let sessions = tmp.path().join(".codex").join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(sessions.join(".DS_Store"), b"junk").unwrap();
        std::fs::create_dir_all(sessions.join("not-a-year")).unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 5, 15).unwrap();
        let session_id = "019e2c5f-dddd-7000-8000-000000000004";
        let target = write_rollout(tmp.path(), date, session_id);

        let (path, parsed_date) =
            find_codex_session_file_for_attach(tmp.path(), session_id).unwrap();
        assert_eq!(path, target);
        assert_eq!(parsed_date, date);
    }

    /// Test sleeper that records each requested sleep duration without
    /// actually sleeping. Lets enrichment retry tests run instantly.
    struct RecordingSleeper(Mutex<Vec<Duration>>);

    impl RecordingSleeper {
        fn new() -> Self {
            Self(Mutex::new(Vec::new()))
        }

        fn recorded(&self) -> Vec<Duration> {
            self.0.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl Sleeper for RecordingSleeper {
        async fn sleep(&self, duration: Duration) {
            self.0.lock().unwrap().push(duration);
        }
    }

    #[tokio::test]
    async fn load_with_retry_returns_default_after_all_attempts_miss() {
        // File never appears — three attempts, two inter-attempt sleeps,
        // total 400ms worst case before default.
        let tmp = TempDir::new().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 5, 15).unwrap();
        let sleeper = RecordingSleeper::new();

        let result = load_with_retry(tmp.path(), date, "no-such-session", &sleeper).await;
        assert_eq!(result, Enrichment::default());

        let sleeps = sleeper.recorded();
        assert_eq!(
            sleeps.len(),
            2,
            "two backoffs between three attempts on all-miss"
        );
        for sleep in &sleeps {
            assert_eq!(*sleep, Duration::from_millis(ENRICHMENT_RETRY_DELAY_MS));
        }
    }

    #[tokio::test]
    async fn load_with_retry_succeeds_on_first_attempt_with_zero_sleeps() {
        // Codex writes synchronously, so the file is normally already on
        // disk. This pins the "typical case pays zero latency" contract —
        // a regression that re-introduced a pre-attempt sleep would
        // surface here as a non-empty recorded list.
        let tmp = TempDir::new().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 5, 15).unwrap();
        let session_id = "019e2c5f-aaaa-7000-8000-000000000001";
        let dir = session_directory(tmp.path(), date);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("rollout-1-{session_id}.jsonl")),
            r#"{"type":"session_meta","payload":{"cli_version":"0.130.0"}}"#,
        )
        .unwrap();
        let sleeper = RecordingSleeper::new();

        let result = load_with_retry(tmp.path(), date, session_id, &sleeper).await;
        assert_eq!(result.cli_version.as_deref(), Some("0.130.0"));
        assert!(
            sleeper.recorded().is_empty(),
            "first-attempt success pays zero latency"
        );
    }

    /// Sleeper that materializes a target file on its first `sleep` call —
    /// simulates "writer was mid-flush during attempt 1, flushed by
    /// attempt 2." Records each requested duration like
    /// [`RecordingSleeper`] for assertion.
    struct StagingSleeper {
        target: PathBuf,
        content: String,
        recorded: Mutex<Vec<Duration>>,
    }

    #[async_trait::async_trait]
    impl Sleeper for StagingSleeper {
        async fn sleep(&self, duration: Duration) {
            self.recorded.lock().unwrap().push(duration);
            if !self.target.exists() {
                std::fs::write(&self.target, &self.content).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn load_with_retry_succeeds_on_second_attempt_with_one_sleep() {
        // The retry exists to defend against filesystem-flush latency on
        // slow disks — file absent on the first try, present by the
        // second. One backoff before success.
        let tmp = TempDir::new().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 5, 15).unwrap();
        let session_id = "019e2c5f-aaaa-7000-8000-000000000002";
        let dir = session_directory(tmp.path(), date);
        std::fs::create_dir_all(&dir).unwrap();
        let sleeper = StagingSleeper {
            target: dir.join(format!("rollout-2-{session_id}.jsonl")),
            content: r#"{"type":"session_meta","payload":{"cli_version":"0.130.1"}}"#.to_owned(),
            recorded: Mutex::new(Vec::new()),
        };

        let result = load_with_retry(tmp.path(), date, session_id, &sleeper).await;
        assert_eq!(result.cli_version.as_deref(), Some("0.130.1"));
        assert_eq!(
            sleeper.recorded.lock().unwrap().len(),
            1,
            "one backoff before second-attempt success"
        );
    }

    #[test]
    fn build_session_meta_fields_returns_none_when_no_model_and_no_version() {
        // Empty enrichment → no SessionMeta worth emitting.
        let result = build_session_meta_fields(&Enrichment::default(), vec![], vec![]);
        assert!(result.is_none());
    }

    #[test]
    fn build_session_meta_fields_returns_some_when_cli_version_present() {
        let e = Enrichment {
            cli_version: Some("0.130.0".to_owned()),
            ..Default::default()
        };
        let result = build_session_meta_fields(&e, vec![], vec![]);
        let fields = result.expect("Some");
        assert_eq!(fields.harness_version, "0.130.0");
        assert_eq!(fields.model, "", "missing model becomes empty string");
    }

    #[test]
    fn build_session_meta_fields_returns_some_when_model_present() {
        let e = Enrichment {
            model: Some("gpt-5.5".to_owned()),
            ..Default::default()
        };
        let result = build_session_meta_fields(&e, vec![], vec![]);
        assert!(result.is_some());
    }

    #[test]
    fn load_codex_transcript_with_no_partition_date_returns_meta_only_empty() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let agent_id = Uuid::now_v7();
        let result =
            load_codex_transcript(home.path(), cwd.path(), "any-session", None, agent_id).unwrap();
        assert!(result.turns.is_empty());
        assert!(result.warnings.is_empty());
        // meta is populated from config loaders (empty here since no config files).
        assert!(result.meta.is_some());
    }

    #[test]
    fn load_codex_transcript_with_missing_recorded_file_fails_hydration() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let agent_id = Uuid::now_v7();
        let date = NaiveDate::from_ymd_opt(2026, 5, 14).unwrap();
        let error = load_codex_transcript(
            home.path(),
            cwd.path(),
            "no-such-session-id",
            Some(date),
            agent_id,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            LoadTranscriptError::RecordedSessionUnavailable
        ));
    }

    fn write_session_at(home: &Path, date: NaiveDate, session_id: &str, content: &str) -> PathBuf {
        let dir = session_directory(home, date);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-2026-{session_id}.jsonl"));
        std::fs::write(&path, content).unwrap();
        path
    }

    fn jsonl_lines(records: &[Value]) -> String {
        records
            .iter()
            .map(|r| serde_json::to_string(r).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn task_started(turn_id: &str, ts: &str, window: u64) -> Value {
        serde_json::json!({
            "timestamp": ts,
            "type": "event_msg",
            "payload": {
                "type": "task_started",
                "turn_id": turn_id,
                "model_context_window": window
            }
        })
    }

    fn user_message(text: &str, ts: &str) -> Value {
        serde_json::json!({
            "timestamp": ts,
            "type": "event_msg",
            "payload": { "type": "user_message", "message": text }
        })
    }

    fn agent_message(text: &str, ts: &str) -> Value {
        serde_json::json!({
            "timestamp": ts,
            "type": "event_msg",
            "payload": { "type": "agent_message", "message": text }
        })
    }

    fn task_complete(turn_id: &str, ts: &str) -> Value {
        serde_json::json!({
            "timestamp": ts,
            "type": "event_msg",
            "payload": { "type": "task_complete", "turn_id": turn_id }
        })
    }

    fn turn_context(model: &str, ts: &str) -> Value {
        serde_json::json!({
            "timestamp": ts,
            "type": "turn_context",
            "payload": { "model": model }
        })
    }

    fn turn_context_with_effort(model: &str, effort: &str, ts: &str) -> Value {
        serde_json::json!({
            "timestamp": ts,
            "type": "turn_context",
            "payload": { "model": model, "effort": effort }
        })
    }

    fn turn_context_with_turn_id(model: &str, turn_id: &str, ts: &str) -> Value {
        serde_json::json!({
            "timestamp": ts,
            "type": "turn_context",
            "payload": { "model": model, "turn_id": turn_id }
        })
    }

    fn hydration_keys(content: &str, agent_id: AgentId) -> Vec<Option<String>> {
        parse_codex_transcript_content(content, agent_id)
            .turns
            .into_iter()
            .filter_map(|t| match t {
                Turn::Agent { hydration_key, .. } => Some(hydration_key),
                Turn::User { .. } | Turn::System { .. } => None,
            })
            .collect()
    }

    #[test]
    fn hydration_key_is_stable_across_reparses_from_turn_context_turn_id() {
        // Re-parsing the same content yields a turn whose `hydration_key` is
        // identical across parses (Codex's `turn_context.turn_id`), even though
        // our own `turn_id` is freshly minted each parse. The merge dedups on
        // the stable key so a re-read never duplicates the turn.
        let agent_id = Uuid::now_v7();
        let content = jsonl_lines(&[
            task_started("thread", "2026-05-14T19:33:20Z", 258_400),
            turn_context_with_turn_id("gpt-5.5", "codex-turn-7", "2026-05-14T19:33:20Z"),
            agent_message("hi", "2026-05-14T19:33:22Z"),
            task_complete("thread", "2026-05-14T19:33:23Z"),
        ]);
        let parse = || {
            parse_codex_transcript_content(&content, agent_id)
                .turns
                .into_iter()
                .find_map(|t| match t {
                    Turn::Agent {
                        turn_id,
                        hydration_key,
                        ..
                    } => Some((turn_id, hydration_key)),
                    Turn::User { .. } | Turn::System { .. } => None,
                })
                .expect("one agent turn")
        };
        let (turn_id_a, key_a) = parse();
        let (turn_id_b, key_b) = parse();
        assert_eq!(
            key_a.as_deref(),
            Some("codex-turn-7"),
            "the hydration key is the per-turn turn_context.turn_id"
        );
        assert_eq!(key_a, key_b, "hydration_key must be parse-invariant");
        assert_ne!(
            turn_id_a, turn_id_b,
            "our turn_id is freshly minted each parse"
        );
    }

    #[test]
    fn hydration_keys_are_distinct_across_two_turns() {
        // The dedup key must be per-turn-*unique*, not merely stable: two
        // distinct turns whose `turn_context` carries distinct `turn_id`s must
        // yield distinct keys (and stable across reparse). Both `task_started`
        // records here reuse the SAME id ("thread") — so a key sourced from
        // `task_started.turn_id` would collide the two turns and the merge would
        // silently drop the second on a re-read; the `turn_context`-sourced key
        // does not. (A handcrafted fixture proves the *parser* keys per-turn;
        // that real Codex varies `turn_context.turn_id` per turn, and that the
        // live stream carries the same id, is confirmed by the live multi-turn
        // probe, not here — Codex refresh stays gated until then.)
        let agent_id = Uuid::now_v7();
        let content = jsonl_lines(&[
            task_started("thread", "2026-05-14T19:33:20Z", 258_400),
            turn_context_with_turn_id("gpt-5.5", "codex-turn-1", "2026-05-14T19:33:20Z"),
            agent_message("a", "2026-05-14T19:33:22Z"),
            task_complete("thread", "2026-05-14T19:33:23Z"),
            task_started("thread", "2026-05-14T19:34:20Z", 258_400),
            turn_context_with_turn_id("gpt-5.5", "codex-turn-2", "2026-05-14T19:34:20Z"),
            agent_message("b", "2026-05-14T19:34:22Z"),
            task_complete("thread", "2026-05-14T19:34:23Z"),
        ]);
        let keys = hydration_keys(&content, agent_id);
        assert_eq!(
            keys,
            vec![
                Some("codex-turn-1".to_owned()),
                Some("codex-turn-2".to_owned()),
            ],
            "distinct turns get distinct per-turn keys"
        );
        assert_eq!(
            hydration_keys(&content, agent_id),
            keys,
            "and the keys are identical on re-parse"
        );
    }

    #[test]
    fn hydration_key_is_none_for_a_turn_with_no_turn_context() {
        // A turn that writes no `turn_context` has no per-turn id → `None` (the
        // merge falls back to `turn_id`). Crucially it must NOT inherit the
        // prior turn's key — that non-uniqueness is the silent-drop bug. The
        // builder is fresh per turn, so this holds by construction.
        let agent_id = Uuid::now_v7();
        let content = jsonl_lines(&[
            task_started("thread", "2026-05-14T19:33:20Z", 258_400),
            turn_context_with_turn_id("gpt-5.5", "codex-turn-1", "2026-05-14T19:33:20Z"),
            agent_message("a", "2026-05-14T19:33:22Z"),
            task_complete("thread", "2026-05-14T19:33:23Z"),
            // Turn 2: no turn_context at all.
            task_started("thread", "2026-05-14T19:34:20Z", 258_400),
            agent_message("b", "2026-05-14T19:34:22Z"),
            task_complete("thread", "2026-05-14T19:34:23Z"),
        ]);
        assert_eq!(
            hydration_keys(&content, agent_id),
            vec![Some("codex-turn-1".to_owned()), None],
            "turn 2 has no turn_context → None, never the prior turn's id"
        );
    }

    #[test]
    fn hydrate_turn_without_effort_readback_is_none_not_stale() {
        // A turn whose `turn_context` omits `effort` must hydrate `effort: None`
        // — never inheriting the prior turn's. (Codex currently always writes
        // `effort`, so this is a hand-crafted contract guard, not a live shape.)
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let agent_id = Uuid::now_v7();
        let date = NaiveDate::from_ymd_opt(2026, 5, 14).unwrap();
        let session_id = "019e27fa-ae19-7022-97a2-356e6e5f3360";
        let content = jsonl_lines(&[
            turn_context_with_effort("gpt-5.5", "high", "2026-05-14T19:33:20Z"),
            task_started(session_id, "2026-05-14T19:33:20Z", 258_400),
            agent_message("a", "2026-05-14T19:33:22Z"),
            task_complete(session_id, "2026-05-14T19:33:23Z"),
            // Turn 2: model present, effort omitted.
            turn_context("gpt-5.5", "2026-05-14T19:34:20Z"),
            task_started(session_id, "2026-05-14T19:34:20Z", 258_400),
            agent_message("b", "2026-05-14T19:34:22Z"),
            task_complete(session_id, "2026-05-14T19:34:23Z"),
        ]);
        write_session_at(home.path(), date, session_id, &content);

        let result =
            load_codex_transcript(home.path(), cwd.path(), session_id, Some(date), agent_id)
                .unwrap();
        let efforts: Vec<_> = result
            .turns
            .iter()
            .filter_map(|t| match t {
                Turn::Agent { effort, .. } => Some(effort.clone()),
                Turn::User { .. } | Turn::System { .. } => None,
            })
            .collect();
        assert_eq!(
            efforts,
            vec![Some("high".to_owned()), None],
            "turn 2 omits effort → None, not the prior turn's 'high'"
        );
    }

    #[test]
    fn hydrate_stamps_per_turn_model_and_effort_from_turn_context() {
        // Two turns on different model + effort → two hydrated agent turns whose
        // values differ. The readback effort field is `effort` (verified @
        // codex 0.137.0). SessionMeta.model stays first-wins (separate path).
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let agent_id = Uuid::now_v7();
        let date = NaiveDate::from_ymd_opt(2026, 5, 14).unwrap();
        let session_id = "019e27fa-ae19-7022-97a2-356e6e5f3360";
        let content = jsonl_lines(&[
            turn_context_with_effort("gpt-5.5", "medium", "2026-05-14T19:33:20Z"),
            task_started(session_id, "2026-05-14T19:33:20Z", 258_400),
            user_message("hi", "2026-05-14T19:33:21Z"),
            agent_message("a", "2026-05-14T19:33:22Z"),
            task_complete(session_id, "2026-05-14T19:33:23Z"),
            turn_context_with_effort("gpt-5.6", "high", "2026-05-14T19:34:20Z"),
            task_started(session_id, "2026-05-14T19:34:20Z", 258_400),
            user_message("again", "2026-05-14T19:34:21Z"),
            agent_message("b", "2026-05-14T19:34:22Z"),
            task_complete(session_id, "2026-05-14T19:34:23Z"),
        ]);
        write_session_at(home.path(), date, session_id, &content);

        let result =
            load_codex_transcript(home.path(), cwd.path(), session_id, Some(date), agent_id)
                .unwrap();

        let agent_turns: Vec<_> = result
            .turns
            .iter()
            .filter_map(|t| match t {
                Turn::Agent { model, effort, .. } => Some((model.clone(), effort.clone())),
                Turn::User { .. } | Turn::System { .. } => None,
            })
            .collect();
        assert_eq!(
            agent_turns,
            vec![
                (Some("gpt-5.5".to_owned()), Some("medium".to_owned())),
                (Some("gpt-5.6".to_owned()), Some("high".to_owned())),
            ]
        );
        // SessionMeta keeps the first model (agent-scoped representative).
        assert_eq!(result.meta.unwrap().model, "gpt-5.5");
    }

    #[test]
    fn load_codex_transcript_text_only_turn_produces_user_and_agent() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let agent_id = Uuid::now_v7();
        let date = NaiveDate::from_ymd_opt(2026, 5, 14).unwrap();
        let session_id = "019e27fa-ae19-7022-97a2-356e6e5f3360";
        let content = jsonl_lines(&[
            task_started(session_id, "2026-05-14T19:33:20Z", 258_400),
            turn_context("gpt-5.4", "2026-05-14T19:33:20Z"),
            user_message("hi", "2026-05-14T19:33:21Z"),
            agent_message("hello", "2026-05-14T19:33:22Z"),
            task_complete(session_id, "2026-05-14T19:33:23Z"),
        ]);
        write_session_at(home.path(), date, session_id, &content);

        let result =
            load_codex_transcript(home.path(), cwd.path(), session_id, Some(date), agent_id)
                .unwrap();

        assert_eq!(result.turns.len(), 2);
        let user_started_at = match &result.turns[0] {
            Turn::User {
                text, started_at, ..
            } => {
                assert_eq!(text, "hi");
                *started_at
            }
            other => panic!("expected User turn, got {other:?}"),
        };
        match &result.turns[1] {
            Turn::Agent {
                items,
                status,
                started_at,
                ..
            } => {
                assert_eq!(
                    user_started_at, *started_at,
                    "Codex writes user_message after task_started, but the imported prompt must \
                     share the task anchor so timestamp-sorted views keep prompt before reply"
                );
                assert!(matches!(status, TurnStatus::Complete));
                assert_eq!(items.len(), 1);
                assert!(matches!(&items[0], TurnItem::Text { text, .. } if text == "hello"));
            }
            _ => panic!("expected Agent turn"),
        }
        let meta = result.meta.unwrap();
        assert_eq!(meta.model, "gpt-5.4");
    }

    #[test]
    fn user_message_without_open_task_uses_its_own_timestamp() {
        let agent_id = Uuid::now_v7();
        let content = jsonl_lines(&[user_message("orphan prompt", "2026-05-14T19:33:21Z")]);

        let result = parse_codex_transcript_content(&content, agent_id);

        assert_eq!(result.turns.len(), 1);
        match &result.turns[0] {
            Turn::User {
                text, started_at, ..
            } => {
                assert_eq!(text, "orphan prompt");
                assert_eq!(started_at.to_rfc3339(), "2026-05-14T19:33:21+00:00");
            }
            other => panic!("expected User turn, got {other:?}"),
        }
    }

    #[test]
    fn load_codex_transcript_function_call_pairs_with_output() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let agent_id = Uuid::now_v7();
        let date = NaiveDate::from_ymd_opt(2026, 5, 14).unwrap();
        let session_id = "019e27fa-ae19-7022-97a2-356e6e5f3361";
        let function_call = serde_json::json!({
            "timestamp": "2026-05-14T19:33:22Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "exec_command",
                "call_id": "call_xyz",
                "arguments": r#"{"cmd":"ls"}"#
            }
        });
        let function_call_output = serde_json::json!({
            "timestamp": "2026-05-14T19:33:23Z",
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": "call_xyz",
                "output": "stdout: ok"
            }
        });
        let content = jsonl_lines(&[
            task_started(session_id, "2026-05-14T19:33:20Z", 258_400),
            turn_context("gpt-5.4", "2026-05-14T19:33:20Z"),
            user_message("run", "2026-05-14T19:33:21Z"),
            function_call,
            function_call_output,
            task_complete(session_id, "2026-05-14T19:33:24Z"),
        ]);
        write_session_at(home.path(), date, session_id, &content);

        let result =
            load_codex_transcript(home.path(), cwd.path(), session_id, Some(date), agent_id)
                .unwrap();
        // A function_call + function_call_output folds into the agent turn — it
        // does NOT open a spurious user turn (tool results are `response_item`s,
        // not `user_message`s). The conversation merge's order-correlation of
        // imported prompts relies on this 1:1 user/agent alternation, so pin it.
        assert_eq!(
            result.turns.len(),
            2,
            "tool call folds into the agent turn; no extra user turn"
        );
        let Turn::Agent { items, .. } = &result.turns[1] else {
            panic!("expected Agent turn");
        };
        assert_eq!(items.len(), 1);
        match &items[0] {
            TurnItem::Tool {
                tool_use_id,
                kind,
                name,
                output,
                is_error,
                ..
            } => {
                assert_eq!(tool_use_id, "call_xyz");
                assert_eq!(*kind, ToolKind::Builtin);
                assert_eq!(name, "exec_command");
                assert_eq!(output.as_deref(), Some("stdout: ok"));
                assert_eq!(*is_error, Some(false));
            }
            _ => panic!("expected Tool item"),
        }
    }

    #[test]
    fn load_codex_transcript_function_call_output_nonzero_exit_is_error() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let agent_id = Uuid::now_v7();
        let date = NaiveDate::from_ymd_opt(2026, 5, 14).unwrap();
        let session_id = "019e27fa-ae19-7022-97a2-356e6e5f3361";
        let function_call = serde_json::json!({
            "timestamp": "2026-05-14T19:33:22Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "exec_command",
                "call_id": "call_xyz",
                "arguments": r#"{"cmd":"git status"}"#
            }
        });
        let function_call_output = serde_json::json!({
            "timestamp": "2026-05-14T19:33:23Z",
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": "call_xyz",
                "output": "Chunk ID: abc\nWall time: 0.0000 seconds\nProcess exited with code 128\nOutput:\nfatal: not a git repository\n"
            }
        });
        let content = jsonl_lines(&[
            task_started(session_id, "2026-05-14T19:33:20Z", 258_400),
            turn_context("gpt-5.4", "2026-05-14T19:33:20Z"),
            user_message("run", "2026-05-14T19:33:21Z"),
            function_call,
            function_call_output,
            task_complete(session_id, "2026-05-14T19:33:24Z"),
        ]);
        write_session_at(home.path(), date, session_id, &content);

        let result =
            load_codex_transcript(home.path(), cwd.path(), session_id, Some(date), agent_id)
                .unwrap();
        let Turn::Agent { items, .. } = &result.turns[1] else {
            panic!("expected Agent turn");
        };
        assert!(matches!(
            &items[0],
            TurnItem::Tool {
                is_error: Some(true),
                output: Some(output),
                ..
            } if output.contains("Process exited with code 128")
        ));
    }

    #[test]
    fn load_codex_transcript_exit_code_phrase_in_output_body_is_not_error() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let agent_id = Uuid::now_v7();
        let date = NaiveDate::from_ymd_opt(2026, 5, 14).unwrap();
        let session_id = "019e27fa-ae19-7022-97a2-356e6e5f3361";
        let function_call = serde_json::json!({
            "timestamp": "2026-05-14T19:33:22Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "exec_command",
                "call_id": "call_xyz",
                "arguments": r#"{"cmd":"echo"}"#
            }
        });
        let function_call_output = serde_json::json!({
            "timestamp": "2026-05-14T19:33:23Z",
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": "call_xyz",
                "output": "Chunk ID: abc\nWall time: 0.0000 seconds\nProcess exited with code 0\nOutput:\nProcess exited with code 128\n"
            }
        });
        let content = jsonl_lines(&[
            task_started(session_id, "2026-05-14T19:33:20Z", 258_400),
            turn_context("gpt-5.4", "2026-05-14T19:33:20Z"),
            user_message("run", "2026-05-14T19:33:21Z"),
            function_call,
            function_call_output,
            task_complete(session_id, "2026-05-14T19:33:24Z"),
        ]);
        write_session_at(home.path(), date, session_id, &content);

        let result =
            load_codex_transcript(home.path(), cwd.path(), session_id, Some(date), agent_id)
                .unwrap();
        let Turn::Agent { items, .. } = &result.turns[1] else {
            panic!("expected Agent turn");
        };
        assert!(matches!(
            &items[0],
            TurnItem::Tool {
                is_error: Some(false),
                ..
            }
        ));
    }

    #[test]
    fn load_codex_transcript_function_call_with_mcp_namespace_classifies_as_mcp() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let agent_id = Uuid::now_v7();
        let date = NaiveDate::from_ymd_opt(2026, 5, 14).unwrap();
        let session_id = "019e27fa-ae19-7022-97a2-356e6e5f3362";
        let function_call = serde_json::json!({
            "timestamp": "2026-05-14T19:33:22Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "create_note",
                "namespace": "mcp__tiddly_notes_bookmarks__",
                "call_id": "call_mcp1",
                "arguments": "{}"
            }
        });
        let mcp_end = serde_json::json!({
            "timestamp": "2026-05-14T19:33:23Z",
            "type": "event_msg",
            "payload": {
                "type": "mcp_tool_call_end",
                "call_id": "call_mcp1",
                "invocation": { "server": "tiddly_notes_bookmarks", "tool": "create_note" },
                "result": { "Ok": { "content": [{"type":"text","text":"ok"}], "isError": false } }
            }
        });
        let content = jsonl_lines(&[
            task_started(session_id, "2026-05-14T19:33:20Z", 258_400),
            turn_context("gpt-5.4", "2026-05-14T19:33:20Z"),
            user_message("mcp call", "2026-05-14T19:33:21Z"),
            function_call,
            mcp_end,
            task_complete(session_id, "2026-05-14T19:33:24Z"),
        ]);
        write_session_at(home.path(), date, session_id, &content);

        let result =
            load_codex_transcript(home.path(), cwd.path(), session_id, Some(date), agent_id)
                .unwrap();
        let Turn::Agent { items, .. } = &result.turns[1] else {
            panic!("expected Agent turn");
        };
        match &items[0] {
            TurnItem::Tool {
                kind,
                name,
                output,
                is_error,
                ..
            } => {
                assert_eq!(*kind, ToolKind::Mcp);
                assert_eq!(name, "tiddly_notes_bookmarks.create_note");
                assert_eq!(output.as_deref(), Some("ok"));
                assert_eq!(*is_error, Some(false));
            }
            _ => panic!("expected Tool item"),
        }
    }

    #[test]
    fn mcp_mutation_fixture_reclassifies_late_identity_by_call_id() {
        let content =
            std::fs::read_to_string(fixture_path("mcp-content-mutations.session.jsonl")).unwrap();
        let tools = hydrated_tool_snapshots(&content);

        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].tool_use_id, "item_edit");
        assert_eq!(tools[0].name, "notes_alias.edit_content");
        assert_eq!(tools[0].output.as_deref(), Some("edit ok"));
        assert_eq!(tools[0].is_error, Some(false));
        assert!(matches!(
            &tools[0].facet,
            crate::facets::ToolFacet::Mcp {
                mutation: Some(mutation),
                ..
            } if matches!(
                mutation.as_ref(),
                crate::facets::McpMutation::TextEdit {
                    target,
                    before,
                    after,
                    ..
                } if target == "note · note-example"
                    && before == "before text"
                    && after == "after text"
            )
        ));

        assert_eq!(tools[1].tool_use_id, "item_create");
        assert_eq!(tools[1].name, "prompts_alias.create_prompt");
        assert_eq!(tools[1].output.as_deref(), Some("creation rejected"));
        assert_eq!(tools[1].is_error, Some(true));
        assert!(matches!(
            &tools[1].facet,
            crate::facets::ToolFacet::Mcp {
                server,
                tool,
                mutation: Some(mutation),
            } if server == "prompts_alias"
                && tool == "create_prompt"
                && matches!(
                    mutation.as_ref(),
                    crate::facets::McpMutation::TextCreation {
                        target,
                        content,
                        ..
                    } if target == "prompt · sample-prompt" && content == "Prompt body"
                )
        ));
    }

    #[test]
    fn mcp_mutation_fixture_matches_live_and_hydrated_representations() {
        let live = std::fs::read_to_string(fixture_path("mcp-content-mutations.jsonl")).unwrap();
        let session =
            std::fs::read_to_string(fixture_path("mcp-content-mutations.session.jsonl")).unwrap();

        assert_eq!(
            live_tool_snapshots(&live),
            hydrated_tool_snapshots(&session)
        );
    }

    #[test]
    fn load_codex_transcript_token_count_populates_usage() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let agent_id = Uuid::now_v7();
        let date = NaiveDate::from_ymd_opt(2026, 5, 14).unwrap();
        let session_id = "019e27fa-ae19-7022-97a2-356e6e5f3363";
        let token_count = serde_json::json!({
            "timestamp": "2026-05-14T19:33:23Z",
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {
                    "last_token_usage": {
                        "input_tokens": 100,
                        "output_tokens": 50,
                        "cached_input_tokens": 20,
                        "reasoning_output_tokens": 5
                    }
                }
            }
        });
        let content = jsonl_lines(&[
            task_started(session_id, "2026-05-14T19:33:20Z", 258_400),
            turn_context("gpt-5.4", "2026-05-14T19:33:20Z"),
            user_message("hi", "2026-05-14T19:33:21Z"),
            agent_message("hello", "2026-05-14T19:33:22Z"),
            token_count,
            task_complete(session_id, "2026-05-14T19:33:24Z"),
        ]);
        write_session_at(home.path(), date, session_id, &content);

        let result =
            load_codex_transcript(home.path(), cwd.path(), session_id, Some(date), agent_id)
                .unwrap();
        let Turn::Agent { usage, .. } = &result.turns[1] else {
            panic!("expected Agent turn");
        };
        let usage = usage.as_ref().expect("usage populated");
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cached_input_tokens, Some(20));
        assert_eq!(usage.context_window, Some(258_400));
    }

    #[test]
    fn load_codex_transcript_degenerate_token_count_keeps_prior_usage() {
        // A token_count whose info carries no parseable token fields must not
        // clobber the turn's already-captured usage with zeros (the reload
        // path used to fabricate a zero-Some here; the shared strict builder
        // now skips the record — last-good-wins).
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let agent_id = Uuid::now_v7();
        let date = NaiveDate::from_ymd_opt(2026, 5, 14).unwrap();
        let session_id = "019e27fa-ae19-7022-97a2-356e6e5f3365";
        let valid_token_count = serde_json::json!({
            "timestamp": "2026-05-14T19:33:23Z",
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {
                    "last_token_usage": {
                        "input_tokens": 100,
                        "output_tokens": 50
                    }
                }
            }
        });
        let degenerate_token_count = serde_json::json!({
            "timestamp": "2026-05-14T19:33:24Z",
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": { "total_token_usage": {} }
            }
        });
        let content = jsonl_lines(&[
            task_started(session_id, "2026-05-14T19:33:20Z", 258_400),
            turn_context("gpt-5.4", "2026-05-14T19:33:20Z"),
            user_message("hi", "2026-05-14T19:33:21Z"),
            agent_message("hello", "2026-05-14T19:33:22Z"),
            valid_token_count,
            degenerate_token_count,
            task_complete(session_id, "2026-05-14T19:33:25Z"),
        ]);
        write_session_at(home.path(), date, session_id, &content);

        let result =
            load_codex_transcript(home.path(), cwd.path(), session_id, Some(date), agent_id)
                .unwrap();
        let Turn::Agent { usage, .. } = &result.turns[1] else {
            panic!("expected Agent turn");
        };
        let usage = usage
            .as_ref()
            .expect("usage populated from the valid record");
        assert_eq!(usage.input_tokens, 100, "valid record's value survives");
        assert_eq!(usage.output_tokens, 50);
    }

    #[test]
    fn load_codex_transcript_truncated_mid_turn_marks_failed() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let agent_id = Uuid::now_v7();
        let date = NaiveDate::from_ymd_opt(2026, 5, 14).unwrap();
        let session_id = "019e27fa-ae19-7022-97a2-356e6e5f3364";
        let content = jsonl_lines(&[
            task_started(session_id, "2026-05-14T19:33:20Z", 258_400),
            turn_context("gpt-5.4", "2026-05-14T19:33:20Z"),
            user_message("hi", "2026-05-14T19:33:21Z"),
            agent_message("hello", "2026-05-14T19:33:22Z"),
            // No task_complete — truncated.
        ]);
        write_session_at(home.path(), date, session_id, &content);

        let result =
            load_codex_transcript(home.path(), cwd.path(), session_id, Some(date), agent_id)
                .unwrap();
        let Turn::Agent { status, .. } = &result.turns[1] else {
            panic!("expected Agent turn");
        };
        assert!(matches!(status, TurnStatus::Failed));
    }

    #[test]
    fn load_codex_transcript_malformed_line_is_skipped_with_warning() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let agent_id = Uuid::now_v7();
        let date = NaiveDate::from_ymd_opt(2026, 5, 14).unwrap();
        let session_id = "019e27fa-ae19-7022-97a2-356e6e5f3365";
        let content = format!(
            "{}\n{{ not valid\n{}\n{}",
            serde_json::to_string(&task_started(session_id, "2026-05-14T19:33:20Z", 258_400))
                .unwrap(),
            serde_json::to_string(&agent_message("hello", "2026-05-14T19:33:22Z")).unwrap(),
            serde_json::to_string(&task_complete(session_id, "2026-05-14T19:33:23Z")).unwrap(),
        );
        write_session_at(home.path(), date, session_id, &content);

        let result =
            load_codex_transcript(home.path(), cwd.path(), session_id, Some(date), agent_id)
                .unwrap();
        assert!(!result.warnings.is_empty(), "warning emitted for bad line");
        assert_eq!(result.warnings[0].line_number, 2);
    }

    #[test]
    fn load_codex_transcript_propagates_rate_limits_to_last_rate_limit() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let agent_id = Uuid::now_v7();
        let date = NaiveDate::from_ymd_opt(2026, 5, 14).unwrap();
        let session_id = "019e27fa-ae19-7022-97a2-356e6e5f3366";
        let rate_limit_record = serde_json::json!({
            "timestamp": "2026-05-14T19:33:23Z",
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": null,
                "rate_limits": { "primary": { "used_percent": 10.0 } }
            }
        });
        let content = jsonl_lines(&[
            task_started(session_id, "2026-05-14T19:33:20Z", 258_400),
            turn_context("gpt-5.4", "2026-05-14T19:33:20Z"),
            user_message("hi", "2026-05-14T19:33:21Z"),
            agent_message("ok", "2026-05-14T19:33:22Z"),
            rate_limit_record,
            task_complete(session_id, "2026-05-14T19:33:24Z"),
        ]);
        write_session_at(home.path(), date, session_id, &content);

        let result =
            load_codex_transcript(home.path(), cwd.path(), session_id, Some(date), agent_id)
                .unwrap();
        let rl = result.last_rate_limit.unwrap();
        assert_eq!(rl["primary"]["used_percent"].as_f64(), Some(10.0));
    }

    #[test]
    fn load_codex_transcript_ignores_response_item_message_uses_agent_message() {
        // Pin the canonical text source: even when a session file ALSO
        // carries a `response_item/message` record with the structured
        // model-API content, we extract the agent's text from
        // `event_msg/agent_message`. Consuming both would double-count;
        // this test fails loud if a future change parses `response_item/
        // message` as a fallback.
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let agent_id = Uuid::now_v7();
        let date = NaiveDate::from_ymd_opt(2026, 5, 14).unwrap();
        let session_id = "019e27fa-ae19-7022-97a2-356e6e5f3367";
        let response_item_message = serde_json::json!({
            "timestamp": "2026-05-14T19:33:22Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "should-be-ignored" }]
            }
        });
        let content = jsonl_lines(&[
            task_started(session_id, "2026-05-14T19:33:20Z", 258_400),
            turn_context("gpt-5.4", "2026-05-14T19:33:20Z"),
            user_message("hi", "2026-05-14T19:33:21Z"),
            response_item_message,
            agent_message("from-event-msg", "2026-05-14T19:33:22Z"),
            task_complete(session_id, "2026-05-14T19:33:24Z"),
        ]);
        write_session_at(home.path(), date, session_id, &content);

        let result =
            load_codex_transcript(home.path(), cwd.path(), session_id, Some(date), agent_id)
                .unwrap();
        let Turn::Agent { items, .. } = &result.turns[1] else {
            panic!("expected Agent turn");
        };
        assert_eq!(items.len(), 1, "exactly one text item — no duplication");
        match &items[0] {
            TurnItem::Text { text, .. } => {
                assert_eq!(text, "from-event-msg");
            }
            _ => panic!("expected Text item"),
        }
    }

    // --- Fixture-driven facet coverage ---

    #[test]
    fn exec_wrapper_fixture_hydrates_output_failure_and_structured_edit() {
        use crate::facets::ToolFacet;
        let agent_id = Uuid::now_v7();
        let content = std::fs::read_to_string(fixture_path("exec-wrapper.session.jsonl")).unwrap();

        let enrichment = parse_session_content(&content);
        assert_eq!(
            enrichment.patch_facets.len(),
            1,
            "patch_apply_end must feed live facet enrichment without a standalone apply_patch call"
        );

        let result = parse_codex_transcript_content(&content, agent_id);
        let Turn::Agent { items, .. } = result
            .turns
            .iter()
            .find(|turn| matches!(turn, Turn::Agent { .. }))
            .expect("agent turn")
        else {
            unreachable!();
        };
        let exec = items
            .iter()
            .find(|item| matches!(item, TurnItem::Tool { name, .. } if name == "exec"))
            .expect("exec wrapper tool");
        assert!(matches!(
            exec,
            TurnItem::Tool {
                facet: ToolFacet::Shell { command, cwd },
                output: Some(output),
                is_error: Some(true),
                ..
            } if command == "cat alpha.txt"
                && cwd.as_deref() == Some("/private/tmp/facet-probe/scratch")
                && output.contains("failure-marker")
        ));

        let edit = items
            .iter()
            .find(|item| matches!(item, TurnItem::Tool { name, .. } if name == "apply_patch"))
            .expect("patch_apply_end edit tool");
        let TurnItem::Tool {
            facet: ToolFacet::Edit { files },
            output,
            is_error,
            ..
        } = edit
        else {
            panic!("expected content-bearing Edit facet");
        };
        assert_eq!(files[0].edits[0].old, "foo");
        assert_eq!(files[0].edits[0].new, "bar");
        assert!(
            output
                .as_deref()
                .is_some_and(|text| text.contains("Success"))
        );
        assert_eq!(*is_error, Some(false));
    }

    #[test]
    fn ambiguous_exec_wrappers_remain_generic_and_do_not_borrow_nested_exit_codes() {
        let agent_id = Uuid::now_v7();
        let content =
            std::fs::read_to_string(fixture_path("exec-wrapper-ambiguous.session.jsonl")).unwrap();
        let result = parse_codex_transcript_content(&content, agent_id);
        let Turn::Agent { items, .. } = result
            .turns
            .iter()
            .find(|turn| matches!(turn, Turn::Agent { .. }))
            .expect("agent turn")
        else {
            unreachable!();
        };
        let tools: Vec<_> = items
            .iter()
            .filter_map(|item| match item {
                TurnItem::Tool {
                    facet, is_error, ..
                } => Some((facet, is_error)),
                TurnItem::Text { .. } => None,
            })
            .collect();
        assert_eq!(tools.len(), 3);
        for (facet, is_error) in tools {
            assert_eq!(*facet, crate::facets::ToolFacet::Other);
            assert_eq!(*is_error, Some(false));
        }
    }

    #[test]
    fn explicit_patch_failure_wins_over_output_heuristic_in_both_record_orders() {
        let call = serde_json::json!({
            "timestamp": "2026-07-13T20:00:03Z",
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call",
                "call_id": "call_patch_failed",
                "name": "apply_patch",
                "input": "*** Begin Patch\n*** Update File: /tmp/alpha.txt\n@@\n-old\n+new\n*** End Patch\n"
            }
        });
        let patch_failed = serde_json::json!({
            "timestamp": "2026-07-13T20:00:05Z",
            "type": "event_msg",
            "payload": {
                "type": "patch_apply_end",
                "call_id": "call_patch_failed",
                "success": false,
                "status": "failed",
                "stdout": "",
                "stderr": "patch rejected",
                "changes": {
                    "/tmp/alpha.txt": {
                        "type": "update",
                        "unified_diff": "@@ -1 +1 @@\n-old\n+new\n"
                    }
                }
            }
        });
        let output = serde_json::json!({
            "timestamp": "2026-07-13T20:00:04Z",
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call_output",
                "call_id": "call_patch_failed",
                "output": "Exit code: 0\nOutput:\npatch rejected"
            }
        });

        for output_first in [false, true] {
            let mut records = vec![
                task_started("turn_patch_failed", "2026-07-13T20:00:00Z", 258_400),
                turn_context("gpt-5.6-sol", "2026-07-13T20:00:01Z"),
                user_message("apply a patch", "2026-07-13T20:00:02Z"),
                call.clone(),
            ];
            if output_first {
                records.extend([output.clone(), patch_failed.clone()]);
            } else {
                records.extend([patch_failed.clone(), output.clone()]);
            }
            records.push(task_complete("turn_patch_failed", "2026-07-13T20:00:06Z"));

            let result = parse_codex_transcript_content(&jsonl_lines(&records), Uuid::now_v7());
            let Turn::Agent { items, .. } = &result.turns[1] else {
                panic!("expected agent turn");
            };
            assert!(matches!(
                &items[0],
                TurnItem::Tool {
                    facet: crate::facets::ToolFacet::Edit { .. },
                    output: Some(output),
                    is_error: Some(true),
                    ..
                } if output.contains("patch rejected")
            ));
        }
    }

    #[test]
    fn patch_apply_end_replaces_legacy_apply_patch_enrichment_instead_of_duplicating() {
        let content = std::fs::read_to_string(fixture_path("apply-patch.session.jsonl")).unwrap();
        let enrichment = parse_session_content(&content);
        assert_eq!(
            enrichment.patch_facets.len(),
            1,
            "the legacy apply_patch call and its patch_apply_end share one call_id"
        );
    }

    // Recorded @ codex 0.143.0 (probe 2026-07-10).

    #[test]
    fn apply_patch_fixture_hydrates_edit_facet_with_content() {
        use crate::facets::{EditChange, ToolFacet};
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let agent_id = Uuid::now_v7();
        let date = NaiveDate::from_ymd_opt(2026, 7, 10).unwrap();
        let session_id = "00000000-0000-7000-8000-000000000031";
        let content = std::fs::read_to_string(fixture_path("apply-patch.session.jsonl")).unwrap();
        write_session_at(home.path(), date, session_id, &content);

        let result =
            load_codex_transcript(home.path(), cwd.path(), session_id, Some(date), agent_id)
                .unwrap();

        let tools: Vec<_> = result
            .turns
            .iter()
            .filter_map(|t| match t {
                Turn::Agent { items, .. } => Some(items.iter().filter_map(|i| match i {
                    TurnItem::Tool {
                        name,
                        facet,
                        output,
                        ..
                    } => Some((name.clone(), facet.clone(), output.clone())),
                    TurnItem::Text { .. } => None,
                })),
                _ => None,
            })
            .flatten()
            .collect();

        let (_, patch_facet, patch_output) = tools
            .iter()
            .find(|(name, _, _)| name == "apply_patch")
            .expect("apply_patch tool item");
        let ToolFacet::Edit { files } = patch_facet else {
            panic!("expected Edit facet, got {patch_facet:?}");
        };
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].change, EditChange::Modified);
        assert_eq!(files[0].edits[0].old, "foo");
        assert_eq!(files[0].edits[0].new, "bar");
        assert_eq!(files[1].change, EditChange::Added);
        assert_eq!(files[1].edits[0].new, "hello world");
        // custom_tool_call_output paired by call_id.
        assert!(
            patch_output
                .as_deref()
                .is_some_and(|o| o.starts_with("Exit code: 0")),
            "custom_tool_call_output must pair onto the apply_patch item; got {patch_output:?}"
        );

        let (_, exec_facet, _) = tools
            .iter()
            .find(|(name, _, _)| name == "exec_command")
            .expect("exec_command tool item");
        let ToolFacet::Shell { command, cwd } = exec_facet else {
            panic!("expected Shell facet, got {exec_facet:?}");
        };
        assert!(!command.is_empty());
        assert!(cwd.is_some(), "disk exec_command carries workdir");
    }

    /// Codex's equivalence contract is files + change-kinds, not content:
    /// the live `file_change` structurally cannot carry the edit text (it
    /// exists only in the session file), so the two channels must agree on
    /// *which files changed and how* — the same predicate the adapter's
    /// facet-upgrade path guard uses before replacing a live facet.
    #[test]
    fn codex_stream_and_session_edit_facets_agree_on_files_and_kinds() {
        use crate::facets::ToolFacet;
        // Disk side: the enrichment read collects the turn's patch facets.
        let content = std::fs::read_to_string(fixture_path("apply-patch.session.jsonl")).unwrap();
        let enrichment = parse_session_content(&content);
        assert_eq!(enrichment.patch_facets.len(), 1);
        let ToolFacet::Edit { files: disk } = &enrichment.patch_facets[0] else {
            panic!("expected Edit facet");
        };

        // Live side: the stream fixture's file_change item (recorded from
        // the same probe turn).
        let stream = std::fs::read_to_string(fixture_path("file-change.jsonl")).unwrap();
        let mut state = crate::codex::parser::CodexParserState::default();
        let turn_id = Uuid::now_v7();
        let mut live: Option<Vec<(String, crate::facets::EditChange)>> = None;
        for line in stream.lines().filter(|l| !l.trim().is_empty()) {
            if let crate::parser::ParseOutcome::Event(crate::events::AdapterEvent::ToolStarted {
                name,
                facet: ToolFacet::Edit { files },
                ..
            }) = crate::codex::parser::parse_line(line, turn_id, &mut state)
                && name == "file_change"
            {
                live = Some(files.iter().map(|f| (f.path.clone(), f.change)).collect());
            }
        }
        let live = live.expect("live file_change Edit facet");

        let disk_set: std::collections::HashSet<_> =
            disk.iter().map(|f| (f.path.clone(), f.change)).collect();
        let live_set: std::collections::HashSet<_> = live.into_iter().collect();
        assert_eq!(
            disk_set, live_set,
            "live and disk must agree on files + change kinds — this equality is also the adapter's upgrade-path guard"
        );
        // And the content asymmetry is real: disk has pairs, live had none.
        assert!(disk.iter().any(|f| !f.edits.is_empty()));
    }

    // --- Paginated rollouts (history_mode) ---
    //
    // These cover the generation Codex 0.148+ writes, where prompt and answer
    // text moved from `event_msg/user_message` / `agent_message` onto
    // `event_msg/item_completed`. See the fixture inventory above this module.

    fn agent_text(turn: &Turn) -> String {
        let Turn::Agent { items, .. } = turn else {
            panic!("expected an agent turn");
        };
        items
            .iter()
            .filter_map(|item| match item {
                TurnItem::Text {
                    kind: ContentKind::Text,
                    text,
                } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    #[test]
    fn paginated_rollout_hydrates_prompt_and_answer_exactly_once() {
        let content =
            std::fs::read_to_string(fixture_path("paginated-text-only.session.jsonl")).unwrap();
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());

        let users: Vec<_> = result
            .turns
            .iter()
            .filter(|t| matches!(t, Turn::User { .. }))
            .collect();
        let agents: Vec<_> = result
            .turns
            .iter()
            .filter(|t| matches!(t, Turn::Agent { .. }))
            .collect();

        // Exactly one of each: the `response_item/message` twins carry the same
        // content and must not be counted a second time.
        assert_eq!(
            users.len(),
            1,
            "prompt must render once, not once per channel"
        );
        assert_eq!(agents.len(), 1);
        let Turn::User { text, .. } = users[0] else {
            unreachable!()
        };
        assert_eq!(text, "say ack");
        assert_eq!(agent_text(agents[0]), "ack");
        assert!(
            result.warnings.is_empty(),
            "a known mode must parse silently"
        );
    }

    /// The regression test for the bug that motivated this work: forwarding
    /// from an idle Codex agent reads the transcript from disk, so an empty
    /// hydration silently reported "<agent> had no output" while the answer sat
    /// in the rollout.
    #[test]
    fn paginated_rollout_yields_forwardable_agent_text() {
        let content =
            std::fs::read_to_string(fixture_path("paginated-text-only.session.jsonl")).unwrap();
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());

        let forwarded = crate::forward::latest_completed_agent_text(&result.turns);
        assert_eq!(forwarded.as_deref(), Some("ack"));
    }

    #[test]
    fn paginated_rollout_tolerates_both_text_block_casings() {
        // `UserMessage` blocks are tagged `"text"`, `AgentMessage` blocks
        // `"Text"`, in the same file. Gating on the tag would drop one side.
        let content = jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.149.0","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"UserMessage","id":"u1","content":[{"type":"text","text":"lower"}]}}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"AgentMessage","id":"a1","content":[{"type":"Text","text":"upper"}]}}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());

        let Some(Turn::User { text, .. }) =
            result.turns.iter().find(|t| matches!(t, Turn::User { .. }))
        else {
            panic!("lowercase-tagged prompt block was dropped");
        };
        assert_eq!(text, "lower");
        let agent = result
            .turns
            .iter()
            .find(|t| matches!(t, Turn::Agent { .. }))
            .expect("agent turn");
        assert_eq!(agent_text(agent), "upper");
    }

    #[test]
    fn only_initial_paginated_session_metadata_enables_association() {
        let paginated = serde_json::json!({
            "type":"session_meta","payload":{"history_mode":"paginated"}
        });
        let legacy = serde_json::json!({
            "type":"session_meta","payload":{"history_mode":"legacy"}
        });
        let unknown = serde_json::json!({
            "type":"session_meta","payload":{"history_mode":"future"}
        });
        let missing = serde_json::json!({
            "type":"session_meta","payload":{"cli_version":"0.146.0"}
        });
        let unrelated = serde_json::json!({
            "type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}
        });

        assert!(initial_session_is_paginated(&format!("\n{paginated}\n")));
        for content in [
            legacy.to_string(),
            unknown.to_string(),
            missing.to_string(),
            "not json".to_owned(),
            format!("{unrelated}\n{paginated}"),
        ] {
            assert!(!initial_session_is_paginated(&content), "{content}");
        }
    }

    #[test]
    fn legacy_rollout_with_explicit_mode_still_reads_legacy_records() {
        let content =
            std::fs::read_to_string(fixture_path("legacy-explicit-mode.session.jsonl")).unwrap();
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());

        // Both halves, exactly once: the mode gating added to the legacy
        // `agent_message` arm is precisely the change that could regress the
        // answer side while the prompt still hydrates.
        let users: Vec<_> = result
            .turns
            .iter()
            .filter(|t| matches!(t, Turn::User { .. }))
            .collect();
        let agents: Vec<_> = result
            .turns
            .iter()
            .filter(|t| matches!(t, Turn::Agent { .. }))
            .collect();
        assert_eq!(users.len(), 1);
        assert_eq!(agents.len(), 1);
        let Turn::User { text, .. } = users[0] else {
            unreachable!()
        };
        assert_eq!(text, "say ack");
        assert_eq!(agent_text(agents[0]), "ack");
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn absent_history_mode_reads_legacy_and_stays_silent() {
        // The overwhelmingly common case: every rollout written before Codex
        // added the field. It must not warn — a warning here would cry wolf on
        // most of the corpus.
        let content = std::fs::read_to_string(fixture_path("exec-wrapper.session.jsonl")).unwrap();
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());

        assert!(
            result.turns.iter().any(|t| matches!(t, Turn::User { .. })),
            "legacy records must still hydrate when no mode is declared"
        );
        assert!(
            result.warnings.is_empty(),
            "an absent mode is unremarkable, not a warning: {:?}",
            result.warnings
        );
    }

    #[test]
    fn unknown_history_mode_reads_legacy_and_warns() {
        let content =
            std::fs::read_to_string(fixture_path("unknown-history-mode.session.jsonl")).unwrap();
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());

        // Backward-compatible unknown format: content survives the fallback...
        let Some(Turn::User { text, .. }) =
            result.turns.iter().find(|t| matches!(t, Turn::User { .. }))
        else {
            panic!("legacy-readable records must still hydrate under an unknown mode");
        };
        assert_eq!(text, "say ack");
        // ...but the unrecognized contract is still surfaced.
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.reason.contains("some_future_mode")),
            "unknown mode must name the value it did not recognize: {:?}",
            result.warnings
        );
    }

    #[test]
    fn unknown_history_mode_warns_when_the_fallback_finds_no_text() {
        // The scenario the warning exists for: a future format that, like
        // paginated did to legacy, stops writing the records the fallback
        // reads. Hydration degrades — but it must not degrade *silently*.
        let content =
            std::fs::read_to_string(fixture_path("unknown-history-mode-degraded.session.jsonl"))
                .unwrap();
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());

        assert!(
            !result.turns.iter().any(|t| matches!(t, Turn::User { .. })),
            "an unknown mode must not be read as paginated on a guess"
        );
        let agent = result
            .turns
            .iter()
            .find(|t| matches!(t, Turn::Agent { .. }))
            .expect("turn boundaries survive: they come from records every mode writes");
        assert_eq!(agent_text(agent), "", "text is genuinely unavailable here");
        assert!(
            !result.warnings.is_empty(),
            "silent empty hydration is the exact failure this warning prevents"
        );
    }

    #[test]
    fn paginated_attachment_only_prompt_preserves_the_user_turn() {
        // `UserInput` has image/audio/skill variants; an attachment-only prompt
        // flattens to empty text but the turn boundary is real. Dropping it
        // would leave the agent's answer orphaned with no prompt above it.
        let content = jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.149.0","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"UserMessage","id":"u1","content":[{"type":"image","image_url":"data:image/png;base64,AAAA"}]}}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"AgentMessage","id":"a1","content":[{"type":"Text","text":"a red square"}]}}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());

        let Some(Turn::User { text, .. }) =
            result.turns.iter().find(|t| matches!(t, Turn::User { .. }))
        else {
            panic!("attachment-only prompt must keep its turn boundary");
        };
        assert_eq!(text, "", "the attachment is not representable; the turn is");
        assert!(result.warnings.is_empty(), "a valid record must not warn");
    }

    #[test]
    fn paginated_message_item_missing_content_warns() {
        let content = jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.149.0","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"UserMessage","id":"u1"}}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"AgentMessage","id":"a1","content":"not-an-array"}}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());

        // A recognized message item whose content cannot be read is a warned
        // parse gap, never a silent skip.
        assert_eq!(result.warnings.len(), 2, "{:?}", result.warnings);
        assert!(result.warnings[0].reason.contains("UserMessage"));
        assert!(result.warnings[1].reason.contains("AgentMessage"));
    }

    #[test]
    fn paginated_multi_block_text_flattens_without_separator() {
        // Codex's own flattening (`UserMessageItem::message()`) is a
        // `.join("")`, and the legacy records are generated from it — so the
        // paginated path must concatenate identically, not insert separators.
        let content = jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.149.0","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"UserMessage","id":"u1","content":[
                    {"type":"text","text":"first"},
                    {"type":"image","image_url":"data:image/png;base64,AAAA"},
                    {"type":"text","text":"second"}]}}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());

        let Some(Turn::User { text, .. }) =
            result.turns.iter().find(|t| matches!(t, Turn::User { .. }))
        else {
            panic!("user turn missing");
        };
        assert_eq!(text, "firstsecond");
    }

    #[test]
    fn non_string_history_mode_reads_legacy_and_warns() {
        // Upstream persists `history_mode` as a non-optional enum — a string,
        // or absent on pre-field rollouts, never any other shape. A present
        // non-string value (null included) is a changed contract and must trip
        // the warning; classifying it as "missing" would let a representation
        // change slip past the tripwire exactly the way the paginated flip did.
        for mode in [
            serde_json::json!(2),
            serde_json::json!(null),
            serde_json::json!({"mode": "paginated"}),
        ] {
            let content = jsonl_lines(&[
                serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.199.0","history_mode":mode}}),
                serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
                serde_json::json!({"type":"event_msg","payload":{"type":"user_message","message":"hi"}}),
                serde_json::json!({"type":"event_msg","payload":{"type":"agent_message","message":"hello"}}),
                serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}),
            ]);
            let result = parse_codex_transcript_content(&content, Uuid::now_v7());

            assert!(
                result.turns.iter().any(|t| matches!(t, Turn::User { .. })),
                "legacy fallback must still hydrate under mode {mode}"
            );
            assert!(
                !result.warnings.is_empty(),
                "a present non-string history_mode must warn, got none for {mode}"
            );
        }
    }

    // --- Paginated tool items (M3: wrapper-children attachment) ---

    fn tool_rows(turns: &[Turn]) -> Vec<ToolSnapshot> {
        let Some(Turn::Agent { items, .. }) =
            turns.iter().find(|t| matches!(t, Turn::Agent { .. }))
        else {
            panic!("agent turn missing");
        };
        items
            .iter()
            .filter_map(|item| match item {
                TurnItem::Tool {
                    tool_use_id,
                    name,
                    input,
                    facet,
                    output,
                    is_error,
                    ..
                } => Some(ToolSnapshot {
                    tool_use_id: tool_use_id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                    facet: facet.clone(),
                    output: output.clone(),
                    is_error: *is_error,
                }),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn paginated_single_command_wrapper_is_one_row_with_structured_exit() {
        let content =
            std::fs::read_to_string(fixture_path("paginated-single-command.session.jsonl"))
                .unwrap();
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());

        let rows = tool_rows(&result.turns);
        // The duplicate-row regression: the CommandExecution item must enrich
        // the wrapper in place, not add a second row for the same command.
        assert_eq!(rows.len(), 1, "{rows:?}");
        let row = &rows[0];
        assert_eq!(row.name, "exec");
        assert!(matches!(
            &row.facet,
            crate::facets::ToolFacet::Shell { command, .. } if command == "echo hi"
        ));
        assert_eq!(row.is_error, Some(false));
        assert!(row.output.as_deref().is_some_and(|o| o.contains("hi")));
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    }

    #[test]
    fn wrapper_script_decodes_in_both_argument_literal_forms() {
        // The argument literal is JavaScript, not JSON, and its shape changed
        // across releases: 0.149 writes bare identifier keys over several lines
        // (`bare`, verbatim from a captured rollout) where earlier releases
        // wrote single-line quoted JSON (`quoted`, the form the legacy
        // `exec-wrapper` fixtures carry). A rollout is read long after the CLI
        // that wrote it, so both must decode to the same call.
        //
        // This decode fails **silently** — nothing is logged — which is how the
        // bare-key form went unnoticed across a version bump: both fixtures
        // pinning the collapse path carried the quoted form, so the path had a
        // passing test and no production coverage. The cost of a failed decode
        // is uneven (see `harness-behavior.md` §3.6): a wrapper whose children
        // are well-formed is unaffected, since they supersede it and carry
        // their own facets; the visible cost falls on **childless** wrappers,
        // which render as raw script text. One narrow compound path reaches
        // past that — a decode failure can also erase a *child-bearing*
        // wrapper's failure record — pinned both ways by
        // `decode_failure_with_a_blind_child_loses_the_failure_flag`, which
        // carries the four conditions it needs.
        let bare = "const r = await tools.exec_command({\n  cmd: \"pwd\",\n               workdir: \"/tmp/scratch\",\n  yield_time_ms: 10000,\n               max_output_tokens: 2000\n});\ntext(r);\n";
        let quoted = "const r = await tools.exec_command({\"cmd\":\"pwd\",\
             \"workdir\":\"/tmp/scratch\"});\ntext(r);\n";

        for script in [bare, quoted] {
            let decoded = decode_single_exec_wrapper(script)
                .unwrap_or_else(|| panic!("must decode: {script:?}"));
            assert!(
                matches!(
                    &decoded.facet,
                    crate::facets::ToolFacet::Shell { command, cwd }
                        if command == "pwd" && cwd.as_deref() == Some("/tmp/scratch")
                ),
                "{:?} from {script:?}",
                decoded.facet
            );
            assert!(decoded.emits_full_result, "{script:?}");
        }
    }

    #[test]
    fn bare_key_quoting_leaves_non_key_identifiers_alone() {
        // `false` sits in the same after-a-comma position a key does; quoting
        // it would turn a boolean into the string "false".
        let source = r#"{cmd: "x", flags: [true, false, null], "already": 1, nested: {inner: 2}}"#;
        let value = parse_js_object(source).expect("decodes");
        assert_eq!(value["cmd"], "x");
        assert_eq!(value["flags"], serde_json::json!([true, false, null]));
        assert_eq!(value["already"], 1);
        assert_eq!(value["nested"]["inner"], 2);
    }

    #[test]
    fn paginated_exit_code_outranks_wrapper_output_sniffing() {
        // A command can fail while its wrapper script completes (the script
        // printed the output and moved on) — the wrapper output then reads
        // "Script completed" and the legacy string-sniff would call it a
        // success. The structured exit_code is authoritative.
        let content = jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.149.0","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call","id":"ctc_1","status":"completed","call_id":"call_1","name":"exec","input":"const r = await tools.exec_command({\"cmd\":\"false\"});\ntext(r.output);\n"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"CommandExecution","id":"exec-x","command":["/bin/zsh","-lc","false"],
                "status":"failed","stdout":"","stderr":"","aggregated_output":"","exit_code":1}}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call_output","id":"ctco_1","call_id":"call_1","output":[{"type":"input_text","text":"Script completed\nWall time 0.0 seconds\nOutput:\n"}]}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());

        let rows = tool_rows(&result.turns);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].is_error,
            Some(true),
            "exit_code 1 must survive a 'Script completed' wrapper output"
        );
    }

    #[test]
    fn paginated_batched_wrapper_gets_a_row_per_operation() {
        let content =
            std::fs::read_to_string(fixture_path("paginated-batched-wrapper.session.jsonl"))
                .unwrap();
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());

        let rows = tool_rows(&result.turns);
        // One row per operation — the succeeded wrapper is superseded by the
        // children that render its work, exactly as the live stream shows it.
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(
            !rows.iter().any(|r| r.name == "exec"),
            "wrapper row must not survive alongside its children: {rows:?}"
        );
        let edit = rows
            .iter()
            .find(|r| r.name == "apply_patch")
            .expect("edit row");
        let crate::facets::ToolFacet::Edit { files } = &edit.facet else {
            panic!("edit child must carry a content-bearing Edit facet");
        };
        assert_eq!(files[0].edits[0].old, "foo");
        assert_eq!(files[0].edits[0].new, "bar");
        assert_eq!(edit.is_error, Some(false));
        let shell = rows
            .iter()
            .find(|r| r.name == "exec_command")
            .expect("shell row");
        assert!(matches!(
            &shell.facet,
            crate::facets::ToolFacet::Shell { command, .. } if command == "ls"
        ));
        assert_eq!(shell.is_error, Some(false));
        assert_eq!(shell.output.as_deref(), Some("alpha.txt\n"));
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    }

    #[test]
    fn paginated_mixed_batch_keeps_wrapper_failure_evidence_alongside_child() {
        let content =
            std::fs::read_to_string(fixture_path("paginated-mixed-batch.session.jsonl")).unwrap();
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());

        let rows = tool_rows(&result.turns);
        assert_eq!(rows.len(), 2, "{rows:?}");
        // The successful operation is its own row...
        let shell = rows
            .iter()
            .find(|r| r.name == "exec_command")
            .expect("shell child");
        assert_eq!(shell.is_error, Some(false));
        // ...and the wrapper retains the uncaught failure's diagnostic, which
        // exists nowhere else (the failed patch emitted no item).
        let wrapper = rows.iter().find(|r| r.name == "exec").expect("wrapper row");
        assert_eq!(wrapper.is_error, Some(true));
        assert!(
            wrapper
                .output
                .as_deref()
                .is_some_and(|o| o.contains("apply_patch verification failed")),
            "wrapper output is the only failure evidence: {:?}",
            wrapper.output
        );
    }

    #[test]
    fn paginated_failed_wrapper_with_no_items_hydrates_unchanged() {
        let content =
            std::fs::read_to_string(fixture_path("paginated-failed-tool.session.jsonl")).unwrap();
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());

        let rows = tool_rows(&result.turns);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].is_error, Some(true));
        assert!(
            rows[0]
                .output
                .as_deref()
                .is_some_and(|o| o.contains("apply_patch verification failed"))
        );
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    }

    #[test]
    fn paginated_mcp_children_carry_all_three_result_envelopes() {
        let content = std::fs::read_to_string(fixture_path("paginated-mcp.session.jsonl")).unwrap();
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());

        let rows = tool_rows(&result.turns);
        // Success + tool-reported error + transport failure; the succeeded
        // wrapper that carried them is superseded by its children.
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert!(
            !rows.iter().any(|r| r.name == "exec"),
            "wrapper row must not survive alongside its children: {rows:?}"
        );

        let ok = rows
            .iter()
            .find(|r| r.name == "probe_server.list_filters")
            .expect("mcp success row");
        assert_eq!(ok.is_error, Some(false));
        assert!(
            ok.output
                .as_deref()
                .is_some_and(|o| o.contains("All Notes"))
        );

        // Tool-reported error: status failed + result.isError, diagnostic in content.
        let tool_err = rows
            .iter()
            .find(|r| r.name == "probe_server.get_item")
            .expect("tool-error row");
        assert_eq!(tool_err.is_error, Some(true));
        assert!(
            tool_err
                .output
                .as_deref()
                .is_some_and(|o| o.contains("validation errors"))
        );
        // Transport failure: result null + top-level error — the diagnostic must
        // not reopen blank.
        let transport = rows
            .iter()
            .find(|r| r.name == "probe_server.search_items")
            .expect("transport-failure row");
        assert_eq!(transport.is_error, Some(true));
        assert!(
            transport
                .output
                .as_deref()
                .is_some_and(|o| o.contains("transport error"))
        );
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    }

    #[test]
    fn paginated_tool_item_outside_any_wrapper_warns_and_never_misattaches() {
        // An item with no open wrapper interval must not be guessed onto some
        // other row — a plausible-looking wrong attachment is strictly worse
        // than a visibly missing one.
        let content = jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.149.0","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"CommandExecution","id":"exec-orphan","command":["/bin/zsh","-lc","ls"],
                "status":"completed","aggregated_output":"x\n","exit_code":0}}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());

        assert!(tool_rows(&result.turns).is_empty());
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.reason.contains("outside any exec wrapper")),
            "{:?}",
            result.warnings
        );
    }

    fn association_exec_call(call_id: &str, command: &str, cwd: Option<&str>) -> Value {
        let mut arguments = serde_json::json!({"cmd": command});
        if let Some(cwd) = cwd {
            arguments["workdir"] = Value::String(cwd.to_owned());
        }
        let input = format!(
            "const r = await tools.exec_command({});\ntext(r.output);\n",
            serde_json::to_string(&arguments).expect("arguments serialize")
        );
        serde_json::json!({
            "type":"response_item",
            "payload":{
                "type":"custom_tool_call","id":format!("ctc-{call_id}"),
                "status":"completed","call_id":call_id,"name":"exec","input":input
            }
        })
    }

    fn association_exec_output(call_id: &str, output: &str) -> Value {
        serde_json::json!({
            "type":"response_item",
            "payload":{
                "type":"custom_tool_call_output","id":format!("ctco-{call_id}"),
                "call_id":call_id,
                "output":[{"type":"input_text","text":output}]
            }
        })
    }

    fn association_command_completion(
        producer_turn_id: Option<&str>,
        item_id: &str,
        command: &str,
        cwd: Option<&str>,
        output: &str,
    ) -> Value {
        let cwd = cwd.map(|path| format!("file://{path}"));
        let mut payload = serde_json::json!({
            "type":"item_completed",
            "item":{
                "type":"CommandExecution","id":item_id,
                "command":["/bin/zsh","-lc",command],"cwd":cwd,
                "status":"completed","exit_code":0,"aggregated_output":output
            }
        });
        if let Some(turn_id) = producer_turn_id {
            payload["turn_id"] = Value::String(turn_id.to_owned());
        }
        serde_json::json!({"type":"event_msg","payload":payload})
    }

    fn paginated_association_header() -> Value {
        serde_json::json!({
            "type":"session_meta",
            "payload":{"cli_version":"0.149.0","history_mode":"paginated"}
        })
    }

    fn association_task_started(turn_id: &str) -> Value {
        serde_json::json!({
            "type":"event_msg",
            "payload":{"type":"task_started","turn_id":turn_id}
        })
    }

    fn association_turn_context(turn_id: &str) -> Value {
        serde_json::json!({
            "type":"turn_context",
            "payload":{"turn_id":turn_id,"model":"gpt-5.5"}
        })
    }

    fn association_task_complete(turn_id: &str) -> Value {
        serde_json::json!({
            "type":"event_msg",
            "payload":{"type":"task_complete","turn_id":turn_id}
        })
    }

    #[test]
    fn late_commands_cross_only_the_observed_bookkeeping_sequences() {
        for includes_reasoning_pair in [false, true] {
            let mut records = vec![
                paginated_association_header(),
                association_task_started("turn-a"),
                association_turn_context("turn-a"),
                association_exec_call("call-a", "printf late", Some("/tmp/a")),
                association_exec_output("call-a", "wrapper output"),
                serde_json::json!({"type":"event_msg","payload":{"type":"token_count","info":null}}),
            ];
            if includes_reasoning_pair {
                records.push(serde_json::json!({
                    "type":"event_msg","payload":{
                        "type":"item_completed","turn_id":"turn-a",
                        "item":{"type":"Reasoning","id":"reason-a"}
                    }
                }));
                records.push(serde_json::json!({
                    "type":"response_item","payload":{"type":"reasoning","id":"reason-a"}
                }));
            }
            records.push(association_command_completion(
                Some("turn-a"),
                "exec-a",
                "printf late",
                Some("/tmp/a"),
                "late structured output",
            ));
            records.push(association_task_complete("turn-a"));

            let result = parse_codex_transcript_content(&jsonl_lines(&records), Uuid::now_v7());
            let rows = tool_rows(&result.turns);
            assert_eq!(rows.len(), 1, "{rows:?}");
            assert_eq!(rows[0].output.as_deref(), Some("late structured output"));
            assert!(result.warnings.is_empty(), "{:?}", result.warnings);
        }
    }

    #[test]
    fn late_command_failure_overrides_successful_wrapper_text() {
        let content = jsonl_lines(&[
            paginated_association_header(),
            association_task_started("turn-a"),
            association_turn_context("turn-a"),
            association_exec_call("call-a", "exit 7", None),
            association_exec_output("call-a", "Script completed"),
            serde_json::json!({"type":"event_msg","payload":{"type":"token_count","info":null}}),
            serde_json::json!({
                "type":"event_msg","payload":{
                    "type":"item_completed","turn_id":"turn-a","item":{
                        "type":"CommandExecution","id":"exec-a",
                        "command":["/bin/zsh","-lc","exit 7"],"status":"failed",
                        "exit_code":7,"aggregated_output":"structured failure"
                    }
                }
            }),
            association_task_complete("turn-a"),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        let rows = tool_rows(&result.turns);

        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].output.as_deref(), Some("structured failure"));
        assert_eq!(rows[0].is_error, Some(true));
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    }

    #[test]
    fn late_mcp_completion_uses_the_observed_bounded_fallback() {
        let content = jsonl_lines(&[
            paginated_association_header(),
            association_task_started("turn-a"),
            association_turn_context("turn-a"),
            serde_json::json!({
                "type":"response_item","payload":{
                    "type":"custom_tool_call","id":"ctc-a","status":"completed",
                    "call_id":"call-a","name":"exec","input":"dynamic code mode wrapper"
                }
            }),
            association_exec_output("call-a", "cell still running"),
            serde_json::json!({"type":"event_msg","payload":{"type":"token_count","info":null}}),
            serde_json::json!({
                "type":"event_msg","payload":{
                    "type":"item_completed","turn_id":"turn-a","item":{
                        "type":"McpToolCall","id":"mcp-a","server":"srv","tool":"lookup",
                        "arguments":{"id":"x"},"status":"completed",
                        "result":{"content":[{"type":"text","text":"found"}],"isError":false}
                    }
                }
            }),
            association_task_complete("turn-a"),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        let rows = tool_rows(&result.turns);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].name, "srv.lookup");
        assert_eq!(rows[0].output.as_deref(), Some("found"));
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    }

    #[test]
    fn sanitized_late_completion_fixture_recovers_command_and_mcp() {
        let content =
            std::fs::read_to_string(fixture_path("paginated-late-tool-completion.session.jsonl"))
                .expect("sanitized late-completion fixture");
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        let rows = hydrated_tool_snapshots(&content);

        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(
            rows.iter()
                .any(|row| row.output.as_deref() == Some("structured command output"))
        );
        assert!(rows.iter().any(|row| {
            row.name == "sanitized.lookup" && row.output.as_deref() == Some("structured MCP output")
        }));
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    }

    fn cross_wrapper_completion_order(a_before_b_output: bool) -> LoadedTranscript {
        let mut records = vec![
            paginated_association_header(),
            association_task_started("turn-a"),
            association_turn_context("turn-a"),
            association_exec_call("call-a", "printf a", None),
            association_exec_output("call-a", "wrapper a"),
            association_exec_call("call-b", "printf b", None),
        ];
        if a_before_b_output {
            records.push(association_command_completion(
                Some("turn-a"),
                "exec-a",
                "printf a",
                None,
                "structured a",
            ));
        }
        records.push(association_exec_output("call-b", "wrapper b"));
        if !a_before_b_output {
            records.push(association_command_completion(
                Some("turn-a"),
                "exec-a",
                "printf a",
                None,
                "structured a",
            ));
        }
        records.push(association_command_completion(
            Some("turn-a"),
            "exec-b",
            "printf b",
            None,
            "structured b",
        ));
        records.push(association_task_complete("turn-a"));
        parse_codex_transcript_content(&jsonl_lines(&records), Uuid::now_v7())
    }

    #[test]
    fn exact_identity_recovers_cross_wrapper_completions_in_both_orders() {
        for a_before_b_output in [false, true] {
            let result = cross_wrapper_completion_order(a_before_b_output);
            let rows = tool_rows(&result.turns);
            assert_eq!(rows.len(), 2, "{rows:?}");
            assert!(rows.iter().any(|row| {
                row.output.as_deref() == Some("structured a")
                    && matches!(&row.facet, ToolFacet::Shell { command, .. } if command == "printf a")
            }));
            assert!(rows.iter().any(|row| {
                row.output.as_deref() == Some("structured b")
                    && matches!(&row.facet, ToolFacet::Shell { command, .. } if command == "printf b")
            }));
            assert!(result.warnings.is_empty(), "{:?}", result.warnings);
        }
    }

    #[test]
    fn repeated_command_after_two_outputs_is_left_unowned() {
        let content = jsonl_lines(&[
            paginated_association_header(),
            association_task_started("turn-a"),
            association_turn_context("turn-a"),
            association_exec_call("call-a", "same", None),
            association_exec_output("call-a", "wrapper a"),
            association_exec_call("call-b", "same", None),
            association_exec_output("call-b", "wrapper b"),
            association_command_completion(
                Some("turn-a"),
                "exec-a",
                "same",
                None,
                "must not be guessed",
            ),
            association_task_complete("turn-a"),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        let rows = tool_rows(&result.turns);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(
            rows.iter()
                .all(|row| row.output.as_deref() != Some("must not be guessed"))
        );
        assert_eq!(result.warnings.len(), 1, "{:?}", result.warnings);
    }

    #[test]
    fn post_task_completion_enriches_the_original_turn() {
        let content = jsonl_lines(&[
            paginated_association_header(),
            association_task_started("turn-a"),
            association_turn_context("turn-a"),
            association_exec_call("call-a", "printf a", None),
            association_exec_output("call-a", "wrapper a"),
            association_task_complete("turn-a"),
            association_command_completion(
                Some("turn-a"),
                "exec-a",
                "printf a",
                None,
                "post-turn a",
            ),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        let rows = tool_rows(&result.turns);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].output.as_deref(), Some("post-turn a"));
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    }

    #[test]
    fn prior_turn_completion_does_not_touch_the_new_turn() {
        let content = jsonl_lines(&[
            paginated_association_header(),
            association_task_started("turn-a"),
            association_turn_context("turn-a"),
            association_exec_call("call-a", "printf a", None),
            association_exec_output("call-a", "wrapper a"),
            association_task_complete("turn-a"),
            association_task_started("turn-b"),
            association_turn_context("turn-b"),
            association_exec_call("call-b", "printf b", None),
            association_command_completion(
                Some("turn-a"),
                "exec-a",
                "printf a",
                None,
                "cross-turn a",
            ),
            association_command_completion(Some("turn-b"), "exec-b", "printf b", None, "current b"),
            association_exec_output("call-b", "wrapper b"),
            association_task_complete("turn-b"),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        let agent_rows: Vec<Vec<ToolSnapshot>> = result
            .turns
            .iter()
            .filter_map(|turn| match turn {
                Turn::Agent { items, .. } => Some(
                    items
                        .iter()
                        .filter_map(|item| match item {
                            TurnItem::Tool {
                                tool_use_id,
                                name,
                                input,
                                facet,
                                output,
                                is_error,
                                ..
                            } => Some(ToolSnapshot {
                                tool_use_id: tool_use_id.clone(),
                                name: name.clone(),
                                input: input.clone(),
                                facet: facet.clone(),
                                output: output.clone(),
                                is_error: *is_error,
                            }),
                            _ => None,
                        })
                        .collect(),
                ),
                _ => None,
            })
            .collect();
        assert_eq!(agent_rows.len(), 2, "{agent_rows:?}");
        assert_eq!(agent_rows[0][0].output.as_deref(), Some("cross-turn a"));
        assert_eq!(agent_rows[1][0].output.as_deref(), Some("current b"));
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    }

    #[test]
    fn invalid_producer_turn_relations_never_associate() {
        let cases = [
            jsonl_lines(&[
                paginated_association_header(),
                association_task_started("turn-a"),
                association_turn_context("turn-a"),
                association_exec_call("call-a", "x", None),
                association_command_completion(Some("unknown"), "exec-a", "x", None, "wrong"),
                association_exec_output("call-a", "wrapper"),
                association_task_complete("turn-a"),
            ]),
            jsonl_lines(&[
                paginated_association_header(),
                association_task_started("turn-a"),
                association_turn_context("different-context"),
                association_exec_call("call-a", "x", None),
                association_command_completion(Some("turn-a"), "exec-a", "x", None, "wrong"),
                association_exec_output("call-a", "wrapper"),
                association_task_complete("turn-a"),
            ]),
            jsonl_lines(&[
                paginated_association_header(),
                association_task_started("turn-a"),
                association_exec_call("call-a", "x", None),
                association_exec_output("call-a", "wrapper"),
                association_task_complete("turn-a"),
                association_command_completion(None, "exec-a", "x", None, "wrong"),
            ]),
            jsonl_lines(&[
                paginated_association_header(),
                association_task_started("duplicate"),
                association_task_complete("duplicate"),
                association_task_started("duplicate"),
                association_exec_call("call-a", "x", None),
                association_command_completion(Some("duplicate"), "exec-a", "x", None, "wrong"),
                association_exec_output("call-a", "wrapper"),
                association_task_complete("duplicate"),
            ]),
        ];
        for content in cases {
            let result = parse_codex_transcript_content(&content, Uuid::now_v7());
            assert!(
                tool_rows(&result.turns)
                    .iter()
                    .all(|row| row.output.as_deref() != Some("wrong")),
                "{:?}",
                result.turns
            );
            assert_eq!(result.warnings.len(), 1, "{:?}", result.warnings);
        }
    }

    #[test]
    fn duplicate_completion_record_is_single_use() {
        let completion =
            association_command_completion(Some("turn-a"), "same-item", "x", None, "structured");
        let content = jsonl_lines(&[
            paginated_association_header(),
            association_task_started("turn-a"),
            association_turn_context("turn-a"),
            association_exec_call("call-a", "x", None),
            association_exec_output("call-a", "wrapper"),
            completion.clone(),
            completion,
            association_task_complete("turn-a"),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        let rows = tool_rows(&result.turns);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].output.as_deref(), Some("structured"));
        assert_eq!(result.warnings.len(), 1, "{:?}", result.warnings);
    }

    #[test]
    fn distinct_command_completions_cannot_reuse_a_single_command_slot() {
        let content = jsonl_lines(&[
            paginated_association_header(),
            association_task_started("turn-a"),
            association_turn_context("turn-a"),
            association_exec_call("call-a", "printf once", None),
            association_command_completion(
                Some("turn-a"),
                "exec-a-1",
                "printf once",
                None,
                "first result",
            ),
            association_command_completion(
                Some("turn-a"),
                "exec-a-2",
                "printf once",
                None,
                "invented duplicate",
            ),
            association_exec_output("call-a", "wrapper output"),
            association_task_complete("turn-a"),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        let rows = tool_rows(&result.turns);

        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].output.as_deref(), Some("first result"));
        assert!(
            rows.iter()
                .all(|row| row.output.as_deref() != Some("invented duplicate"))
        );
        assert_eq!(result.warnings.len(), 1, "{:?}", result.warnings);
    }

    #[test]
    fn identical_pending_commands_make_physical_adjacency_ambiguous_only_locally() {
        let content = jsonl_lines(&[
            paginated_association_header(),
            association_task_started("turn-a"),
            association_turn_context("turn-a"),
            association_exec_call("call-a", "same", None),
            association_exec_output("call-a", "wrapper a"),
            association_exec_call("call-b", "same", None),
            association_command_completion(
                Some("turn-a"),
                "exec-ambiguous",
                "same",
                None,
                "must not attach",
            ),
            association_exec_output("call-b", "wrapper b"),
            association_exec_call("call-c", "distinct", None),
            association_command_completion(
                Some("turn-a"),
                "exec-c",
                "distinct",
                None,
                "structured c",
            ),
            association_exec_output("call-c", "wrapper c"),
            association_task_complete("turn-a"),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        let rows = tool_rows(&result.turns);

        assert_eq!(rows.len(), 3, "{rows:?}");
        assert!(
            rows.iter()
                .any(|row| row.output.as_deref() == Some("wrapper a"))
        );
        assert!(
            rows.iter()
                .any(|row| row.output.as_deref() == Some("wrapper b"))
        );
        assert!(
            rows.iter()
                .any(|row| row.output.as_deref() == Some("structured c"))
        );
        assert!(
            rows.iter()
                .all(|row| row.output.as_deref() != Some("must not attach"))
        );
        assert_eq!(result.warnings.len(), 1, "{:?}", result.warnings);
    }

    #[test]
    fn childless_wrapper_does_not_disable_later_tools() {
        let content = jsonl_lines(&[
            paginated_association_header(),
            association_task_started("turn-a"),
            association_turn_context("turn-a"),
            serde_json::json!({
                "type":"response_item","payload":{
                    "type":"custom_tool_call","id":"ctc-childless","status":"completed",
                    "call_id":"childless","name":"exec","input":"dynamic childless wrapper"
                }
            }),
            association_exec_output("childless", "declined before execution"),
            association_exec_call("call-b", "printf b", None),
            association_command_completion(
                Some("turn-a"),
                "exec-b",
                "printf b",
                None,
                "structured b",
            ),
            association_exec_output("call-b", "wrapper b"),
            serde_json::json!({
                "type":"response_item","payload":{
                    "type":"custom_tool_call","id":"ctc-edit","status":"completed",
                    "call_id":"call-edit","name":"exec","input":"dynamic edit wrapper"
                }
            }),
            serde_json::json!({
                "type":"event_msg","payload":{
                    "type":"item_completed","turn_id":"turn-a","item":{
                        "type":"FileChange","id":"edit-a","status":"completed",
                        "changes":{"/tmp/a.txt":{
                            "type":"update","unified_diff":"@@ -1 +1 @@\n-a\n+b\n","move_path":null
                        }}
                    }
                }
            }),
            association_exec_output("call-edit", "edit wrapper"),
            association_task_complete("turn-a"),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        let rows = tool_rows(&result.turns);
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert!(
            rows.iter()
                .any(|row| row.output.as_deref() == Some("structured b"))
        );
        assert!(rows.iter().any(|row| row.name == "apply_patch"));
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    }

    #[test]
    fn late_file_change_is_not_positionally_attached() {
        let content = jsonl_lines(&[
            paginated_association_header(),
            association_task_started("turn-a"),
            association_turn_context("turn-a"),
            serde_json::json!({
                "type":"response_item","payload":{
                    "type":"custom_tool_call","id":"ctc-edit","status":"completed",
                    "call_id":"call-edit","name":"exec","input":"dynamic edit wrapper"
                }
            }),
            association_exec_output("call-edit", "canonical wrapper"),
            serde_json::json!({"type":"event_msg","payload":{"type":"token_count","info":null}}),
            serde_json::json!({
                "type":"event_msg","payload":{
                    "type":"item_completed","turn_id":"turn-a","item":{
                        "type":"FileChange","id":"edit-a","status":"completed",
                        "changes":{"/tmp/a.txt":{
                            "type":"update","unified_diff":"@@ -1 +1 @@\n-a\n+b\n","move_path":null
                        }}
                    }
                }
            }),
            association_task_complete("turn-a"),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        let rows = tool_rows(&result.turns);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].output.as_deref(), Some("canonical wrapper"));
        assert_eq!(result.warnings.len(), 1, "{:?}", result.warnings);
    }

    #[test]
    fn command_signature_matching_is_exact_and_cwd_aware() {
        let wrapper = decode_single_exec_wrapper(
            "const r = await tools.exec_command({\"cmd\":\"pwd\",\"workdir\":\"/tmp/a\"});\ntext(r.output);\n",
        )
        .expect("canonical wrapper")
        .command_signature;
        let same = CommandSignature {
            command: "pwd".to_owned(),
            cwd: Some("/tmp/a".to_owned()),
        };
        let different_command = CommandSignature {
            command: "pwd -P".to_owned(),
            cwd: Some("/tmp/a".to_owned()),
        };
        let different_cwd = CommandSignature {
            command: "pwd".to_owned(),
            cwd: Some("/tmp/b".to_owned()),
        };
        let missing_cwd = CommandSignature {
            command: "pwd".to_owned(),
            cwd: None,
        };
        assert!(command_signatures_match(&wrapper, &same));
        assert!(!command_signatures_match(&wrapper, &different_command));
        assert!(!command_signatures_match(&wrapper, &different_cwd));
        assert!(command_signatures_match(&wrapper, &missing_cwd));
        assert!(decode_single_exec_wrapper("dynamic").is_none());
    }

    #[test]
    fn paginated_malformed_tool_items_degrade_without_panicking_or_misreading() {
        // Adversarial shapes: non-numeric exit_code must not read as success
        // when status says failed; a FileChange without structured changes
        // warns instead of fabricating an edit.
        let content = jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.149.0","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call","id":"ctc_1","status":"completed","call_id":"call_1","name":"exec","input":"dynamic script"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"CommandExecution","id":"exec-bad","command":["/bin/zsh","-lc","x"],
                "status":"failed","exit_code":"not-a-number"}}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"FileChange","id":"exec-noedit","status":"completed"}}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call_output","id":"ctco_1","call_id":"call_1","output":[{"type":"input_text","text":"Script failed\n"}]}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());

        let rows = tool_rows(&result.turns);
        // Wrapper + the degraded shell child; the changeless FileChange warned.
        assert_eq!(rows.len(), 2, "{rows:?}");
        let shell = rows
            .iter()
            .find(|r| r.name == "exec_command")
            .expect("shell child");
        assert_eq!(
            shell.is_error,
            Some(true),
            "status: failed must carry when exit_code is unreadable"
        );
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.reason.contains("exit_code is not numeric")),
            "a present-but-unreadable exit_code is contract drift and must warn: {:?}",
            result.warnings
        );
        let owned_warning = result.turns.iter().any(|turn| match turn {
            Turn::Agent { items, .. } => items.iter().any(|item| {
                matches!(
                    item,
                    TurnItem::Tool { name, warnings, .. }
                        if name == "exec_command"
                            && warnings.iter().any(|warning| warning.reason.contains("exit_code is not numeric"))
                )
            }),
            _ => false,
        });
        assert!(owned_warning, "the warning must travel with its tool row");
        assert!(
            result.warnings.iter().any(|w| w
                .reason
                .contains("FileChange item missing structured changes")),
            "{:?}",
            result.warnings
        );
    }

    fn paginated_shell_lines(item: &serde_json::Value) -> String {
        jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.149.0","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call","id":"ctc_1","status":"completed","call_id":"call_1","name":"exec","input":"const r = await tools.exec_command({\"cmd\":\"x\"});\ntext(r.output);\n"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":item.clone()}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call_output","id":"ctco_1","call_id":"call_1","output":[{"type":"input_text","text":"Script completed\nWall time 0.0 seconds\nOutput:\nwrapper-printed-text"}]}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}),
        ])
    }

    #[test]
    fn declined_command_and_edit_read_as_unsuccessful() {
        // The user refused the tool. Upstream statuses include "declined" for
        // both commands and patches; a declined record typically has no exit
        // code, so recognizing only "failed" reported it as a success.
        let content = paginated_shell_lines(&serde_json::json!({
            "type":"CommandExecution","id":"exec-declined",
            "command":["/bin/zsh","-lc","x"],"status":"declined"}));
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        assert_eq!(tool_rows(&result.turns)[0].is_error, Some(true));

        let content = jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.149.0","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call","id":"ctc_1","status":"completed","call_id":"call_1","name":"exec","input":"dynamic"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"FileChange","id":"exec-fc-declined","status":"declined",
                "changes":{"/tmp/a.txt":{"type":"update","unified_diff":"@@ -1 +1 @@\n-a\n+b\n","move_path":null}}}}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call_output","id":"ctco_1","call_id":"call_1","output":[{"type":"input_text","text":"Script failed\n"}]}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        let edit = tool_rows(&result.turns)
            .into_iter()
            .find(|r| r.name == "apply_patch")
            .expect("edit row");
        assert_eq!(edit.is_error, Some(true));
    }

    #[test]
    fn status_and_exit_code_combine_rather_than_override() {
        // failed status + exit 0: the status is not erased by the exit code.
        let content = paginated_shell_lines(&serde_json::json!({
            "type":"CommandExecution","id":"exec-a",
            "command":["/bin/zsh","-lc","x"],"status":"failed","exit_code":0}));
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        assert_eq!(tool_rows(&result.turns)[0].is_error, Some(true));

        // completed status + nonzero exit: the exit code is not erased either.
        let content = paginated_shell_lines(&serde_json::json!({
            "type":"CommandExecution","id":"exec-b",
            "command":["/bin/zsh","-lc","x"],"status":"completed","exit_code":3}));
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        assert_eq!(tool_rows(&result.turns)[0].is_error, Some(true));
    }

    #[test]
    fn unrecognized_command_status_warns_and_defers_to_wrapper_sniff() {
        // Never fabricate a reading of an unknown contract: is_error stays
        // None, so the wrapper output's string sniff (which only fills None)
        // decides — the legacy heuristic plus a warning.
        let content = paginated_shell_lines(&serde_json::json!({
            "type":"CommandExecution","id":"exec-c",
            "command":["/bin/zsh","-lc","x"],"status":"timed_out"}));
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        let row = &tool_rows(&result.turns)[0];
        assert_eq!(
            row.is_error,
            Some(false),
            "wrapper said 'Script completed' and the status was unreadable"
        );
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.reason.contains("unrecognized status")),
            "{:?}",
            result.warnings
        );
    }

    #[test]
    fn missing_command_status_warns_but_missing_file_change_status_does_not() {
        // CommandExecution.status is required upstream — absence is drift.
        let content = paginated_shell_lines(&serde_json::json!({
            "type":"CommandExecution","id":"exec-d",
            "command":["/bin/zsh","-lc","x"],"exit_code":0}));
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.reason.contains("missing status")),
            "{:?}",
            result.warnings
        );

        // FileChange.status is Option upstream — absence is normal.
        let content = jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.149.0","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call","id":"ctc_1","status":"completed","call_id":"call_1","name":"exec","input":"dynamic"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"FileChange","id":"exec-fc-nostatus",
                "changes":{"/tmp/a.txt":{"type":"update","unified_diff":"@@ -1 +1 @@\n-a\n+b\n","move_path":null}}}}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call_output","id":"ctco_1","call_id":"call_1","output":[{"type":"input_text","text":"ok"}]}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    }

    #[test]
    fn structured_output_beats_wrapper_boilerplate_on_success() {
        // The structured record's output differs from what the wrapper script
        // printed — the row must show the command's real output.
        let content = paginated_shell_lines(&serde_json::json!({
            "type":"CommandExecution","id":"exec-e","command":["/bin/zsh","-lc","x"],
            "status":"completed","exit_code":0,"aggregated_output":"real-command-output"}));
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        assert_eq!(
            tool_rows(&result.turns)[0].output.as_deref(),
            Some("real-command-output")
        );

        // Success with genuinely empty output: blank is the true output; the
        // wrapper's "Script completed / Wall time…" noise must not replace it.
        let content = paginated_shell_lines(&serde_json::json!({
            "type":"CommandExecution","id":"exec-f","command":["/bin/zsh","-lc","x"],
            "status":"completed","exit_code":0,"aggregated_output":""}));
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        assert_eq!(tool_rows(&result.turns)[0].output.as_deref(), Some(""));
    }

    #[test]
    fn failed_command_with_empty_output_keeps_wrapper_diagnostic() {
        // On failure the wrapper output often holds the only diagnostic; an
        // empty structured output must leave the slot open for it.
        let content = jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.149.0","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call","id":"ctc_1","status":"completed","call_id":"call_1","name":"exec","input":"const r = await tools.exec_command({\"cmd\":\"x\"});\ntext(r.output);\n"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"CommandExecution","id":"exec-g","command":["/bin/zsh","-lc","x"],
                "status":"failed","exit_code":1,"aggregated_output":""}}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call_output","id":"ctco_1","call_id":"call_1","output":[{"type":"input_text","text":"Script failed\nScript error:\nthe-only-diagnostic"}]}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        let row = &tool_rows(&result.turns)[0];
        assert_eq!(row.is_error, Some(true));
        assert!(
            row.output
                .as_deref()
                .is_some_and(|o| o.contains("the-only-diagnostic")),
            "{:?}",
            row.output
        );
    }

    #[test]
    fn file_change_first_does_not_consume_the_command_in_place_slot() {
        // Kind-gated slot: a single-command wrapper whose first item is a
        // FileChange (e.g. apply_patch run *through* exec_command) must still
        // enrich the wrapper with the command item — otherwise the command
        // falls to a child row and duplicates the wrapper.
        let content = jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.149.0","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call","id":"ctc_1","status":"completed","call_id":"call_1","name":"exec","input":"const r = await tools.exec_command({\"cmd\":\"apply_patch <<EOF\"});\ntext(r.output);\n"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"FileChange","id":"exec-fc-first","status":"completed",
                "changes":{"/tmp/a.txt":{"type":"update","unified_diff":"@@ -1 +1 @@\n-a\n+b\n","move_path":null}}}}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"CommandExecution","id":"exec-cmd-second","command":["/bin/zsh","-lc","apply_patch <<EOF"],
                "status":"completed","exit_code":0,"aggregated_output":"Done!"}}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call_output","id":"ctco_1","call_id":"call_1","output":[{"type":"input_text","text":"Script completed\n"}]}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        let rows = tool_rows(&result.turns);
        // Wrapper (enriched in place by the command) + edit child. NOT three.
        assert_eq!(rows.len(), 2, "{rows:?}");
        let wrapper = rows.iter().find(|r| r.name == "exec").expect("wrapper");
        assert_eq!(wrapper.output.as_deref(), Some("Done!"));
        assert!(rows.iter().any(|r| r.name == "apply_patch"));
        // The stray item warns here too, arriving *before* the command that
        // claims the slot — the order-independent half of the pair asserted in
        // `sibling_after_the_command_does_not_delete_the_enriched_row`.
        assert_eq!(result.warnings.len(), 1, "{:?}", result.warnings);
        assert!(
            result.warnings[0].reason.contains("single command"),
            "{:?}",
            result.warnings[0]
        );
    }

    /// A single-command wrapper whose `CommandExecution` lands **first**, then
    /// a sibling item. Mirror of `file_change_first_does_not_consume_the_command_in_place_slot`
    /// — that one pins the ordering that worked; this pins the one that did not.
    fn single_command_wrapper_then_sibling(sibling: &serde_json::Value) -> LoadedTranscript {
        let content = jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.149.0","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call","id":"ctc_1","status":"completed","call_id":"call_1","name":"exec","input":"const r = await tools.exec_command({\n  cmd: \"apply_patch <<EOF\"\n});\ntext(r.output);\n"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"CommandExecution","id":"exec-cmd-first","command":["/bin/zsh","-lc","apply_patch <<EOF"],
                "status":"completed","exit_code":0,"aggregated_output":"Done!"}}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":sibling}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call_output","id":"ctco_1","call_id":"call_1","output":[{"type":"input_text","text":"Script completed\n"}]}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}),
        ]);
        parse_codex_transcript_content(&content, Uuid::now_v7())
    }

    #[test]
    fn sibling_after_the_command_does_not_delete_the_enriched_row() {
        // The content-loss regression: once the command has enriched the
        // wrapper, the wrapper row *is* the shell operation. A later sibling
        // superseding it dropped a real command from the reopened transcript,
        // decided purely by which record Codex wrote second.
        let result = single_command_wrapper_then_sibling(&serde_json::json!({
            "type":"FileChange","id":"exec-fc-second","status":"completed",
            "changes":{"/tmp/a.txt":{"type":"update","unified_diff":"@@ -1 +1 @@\n-a\n+b\n","move_path":null}}
        }));

        let rows = tool_rows(&result.turns);
        assert_eq!(rows.len(), 2, "{rows:?}");
        let command = rows.iter().find(|r| r.name == "exec").expect("command row");
        assert_eq!(command.output.as_deref(), Some("Done!"));
        assert!(rows.iter().any(|r| r.name == "apply_patch"), "{rows:?}");
        // Same anomaly, same single warning as the file-change-*first* ordering
        // (`file_change_first_does_not_consume_the_command_in_place_slot`) —
        // that pair is what pins the tripwire to the shape rather than to which
        // record Codex happened to write second.
        assert_eq!(result.warnings.len(), 1, "{:?}", result.warnings);
        assert!(
            result.warnings[0].reason.contains("single command"),
            "{:?}",
            result.warnings[0]
        );
    }

    #[test]
    fn second_command_item_is_left_unowned_and_warns() {
        // A repeated command record contradicts the wrapper's single-call
        // proof. It cannot become a standalone row because there is no
        // canonical wrapper proving that a second operation ran.
        let result = single_command_wrapper_then_sibling(&serde_json::json!({
            "type":"CommandExecution","id":"exec-cmd-duplicate","command":["/bin/zsh","-lc","apply_patch <<EOF"],
            "status":"completed","exit_code":0,"aggregated_output":"Done again!"
        }));

        let rows = tool_rows(&result.turns);
        assert_eq!(rows.len(), 1, "{rows:?}");
        let enriched = rows
            .iter()
            .find(|r| r.name == "exec")
            .expect("enriched row");
        assert_eq!(enriched.output.as_deref(), Some("Done!"));
        assert!(
            rows.iter()
                .all(|row| row.output.as_deref() != Some("Done again!")),
            "{rows:?}"
        );
        assert_eq!(result.warnings.len(), 1, "{:?}", result.warnings);
        assert!(
            result.warnings[0]
                .reason
                .contains("outside any exec wrapper interval"),
            "{:?}",
            result.warnings[0]
        );
    }

    #[test]
    fn mcp_item_on_a_single_command_wrapper_warns() {
        // Unlike a file change, an MCP call cannot come from a shell command —
        // the proved script contains no MCP call to make one.
        let result = single_command_wrapper_then_sibling(&serde_json::json!({
            "type":"McpToolCall","id":"exec-mcp-surprise","server":"srv","tool":"do","arguments":{},
            "status":"completed","result":{"content":[{"type":"text","text":"ok"}],"isError":false}
        }));

        let rows = tool_rows(&result.turns);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(rows.iter().any(|r| r.name == "exec"), "{rows:?}");
        assert_eq!(result.warnings.len(), 1, "{:?}", result.warnings);
    }

    /// One `exec` wrapper whose script decodes or not, plus a child blinded to
    /// its own outcome, with the failure recorded **only** in the wrapper's
    /// full-result exit code.
    fn blind_child_transcript(script: &str) -> LoadedTranscript {
        let content = jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.149.0","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call","id":"ctc_1","status":"completed","call_id":"call_1","name":"exec","input":script}}),
            // Both of the child's own outcome signals are blinded: a non-string
            // `status`, and no `exit_code` at all. Either one alone still
            // convicts (`command_execution_is_error` falls back to the exit
            // code), so the scenario needs both.
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"CommandExecution","id":"exec-blind","command":["/bin/zsh","-lc","false"],
                "status":{"weird":"shape"},"aggregated_output":""}}}),
            // The wrapper output carries neither plain-text failure marker
            // (`Script failed` header, process-exit line) — the nonzero exit
            // survives only inside the full-result JSON, which
            // `structured_script_exit_code` reads and a failed decode forfeits.
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call_output","id":"ctco_1","call_id":"call_1","output":[{"type":"input_text","text":"Script completed\nWall time 0.1 seconds\nOutput:\n{\"output\":\"\",\"exit_code\":1}"}]}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}),
        ]);
        parse_codex_transcript_content(&content, Uuid::now_v7())
    }

    #[test]
    fn decode_failure_with_a_blind_child_loses_the_failure_flag() {
        // The compound path §3.6 documents, pinned from both sides so the doc
        // claim stays checkable. Four independent conditions must align, each
        // owned by a different function — wrapper decode
        // (`decode_single_exec_wrapper`), the child's two own signals
        // (`command_execution_is_error`), and the wrapper's plain-text sniff
        // (`function_call_output_is_error`). Any one of them changing behaviour
        // moves this, which is exactly why it is pinned rather than described:
        // whoever breaks the chain is told, and the doc is updated in the same
        // commit rather than quietly going stale.
        //
        // Rescue path — the wrapper decodes, so its full-result exit code is
        // read and the wrapper is marked failed. `drop_superseded_rows` keeps a
        // failed row regardless of supersession, so the failure survives.
        let decodable = "const r = await tools.exec_command({\"cmd\":\"false\"});\ntext(r);\n";
        let rescued = blind_child_transcript(decodable);
        assert!(
            tool_rows(&rescued.turns)
                .iter()
                .any(|row| row.is_error == Some(true)),
            "a decodable wrapper must keep the failure: {:?}",
            tool_rows(&rescued.turns)
        );

        // Blind spot — the same transcript with a dynamic script the decoder
        // cannot read. Nothing else changes, and no row is marked failed.
        let dynamic = "const opts = build(); const r = await tools.exec_command(opts); text(r);";
        let lost = blind_child_transcript(dynamic);
        let rows = tool_rows(&lost.turns);
        assert!(
            !rows.iter().any(|row| row.is_error == Some(true)),
            "documented blind spot changed — update §3.6 in this commit: {rows:?}"
        );
        // What remains is developer-visible only. No transcript row reports the
        // failure, so nothing restores it for the user.
        assert!(
            lost.warnings
                .iter()
                .any(|warning| warning.reason.contains("status is not a string")),
            "{:?}",
            lost.warnings
        );
    }

    #[test]
    fn stray_item_after_task_complete_warns_instead_of_vanishing() {
        // Truncated-file edge: the wrapper output never arrived, the turn
        // closed, and a stray item follows. The interval must not survive the
        // turn — the item warns as orphaned rather than silently claiming a
        // stale slot and vanishing at the no-builder check.
        let content = jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.149.0","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call","id":"ctc_1","status":"completed","call_id":"call_1","name":"exec","input":"const r = await tools.exec_command({\"cmd\":\"x\"});\ntext(r.output);\n"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"CommandExecution","id":"exec-late","command":["/bin/zsh","-lc","x"],
                "status":"completed","exit_code":0}}}),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.reason.contains("outside any exec wrapper")),
            "{:?}",
            result.warnings
        );
    }

    #[test]
    fn missing_item_id_warns_and_stays_deterministic_across_parses() {
        // A fresh UUID here would make two parses of the same file produce
        // different transcripts — hiding upstream drift (`id` is required)
        // and breaking parse determinism. Compare the affected row's id only:
        // turn ids are legitimately minted per parse.
        let content = jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.149.0","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call","id":"ctc_1","status":"completed","call_id":"call_1","name":"exec","input":"dynamic"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"CommandExecution","command":["/bin/zsh","-lc","x"],
                "status":"completed","exit_code":0}}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call_output","id":"ctco_1","call_id":"call_1","output":[{"type":"input_text","text":"ok"}]}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}),
        ]);
        let first = parse_codex_transcript_content(&content, Uuid::now_v7());
        let second = parse_codex_transcript_content(&content, Uuid::now_v7());

        let id_of = |result: &LoadedTranscript| {
            tool_rows(&result.turns)
                .into_iter()
                .find(|r| r.name == "exec_command")
                .expect("child row")
                .tool_use_id
        };
        assert_eq!(id_of(&first), id_of(&second));
        assert!(id_of(&first).starts_with("item-missing-id-command-line-"));
        assert!(
            first
                .warnings
                .iter()
                .any(|w| w.reason.contains("missing id")),
            "{:?}",
            first.warnings
        );
    }

    #[test]
    fn mcp_item_decoding_matches_live_extractor_on_edge_envelopes() {
        // Parity with the live parser: all-non-text content yields its
        // placeholder (not ""), and a null result with a structured error
        // yields the stringified error.
        let content = jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.149.0","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call","id":"ctc_1","status":"completed","call_id":"call_1","name":"exec","input":"dynamic"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"McpToolCall","id":"exec-img","server":"srv","tool":"render","arguments":{},
                "status":"completed","result":{"content":[{"type":"image","data":"AAAA"}],"isError":false}}}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"McpToolCall","id":"exec-objerr","server":"srv","tool":"boom","arguments":{},
                "status":"failed","result":null,"error":{"code":42,"reason":"nope"}}}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call_output","id":"ctco_1","call_id":"call_1","output":[{"type":"input_text","text":"done"}]}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        let rows = tool_rows(&result.turns);

        let image = rows
            .iter()
            .find(|r| r.name == "srv.render")
            .expect("image row");
        assert_eq!(
            image.output.as_deref(),
            Some("[non-text tool result omitted]")
        );
        assert_eq!(image.is_error, Some(false));

        let objerr = rows
            .iter()
            .find(|r| r.name == "srv.boom")
            .expect("error row");
        assert_eq!(objerr.is_error, Some(true));
        assert!(
            objerr.output.as_deref().is_some_and(|o| o.contains("nope")),
            "a structured error must not reopen blank: {:?}",
            objerr.output
        );
    }

    #[test]
    fn unknown_status_with_empty_output_keeps_wrapper_diagnostic() {
        // The cell the status matrix and output rule disagreed on: an unknown
        // status with empty structured output must NOT write Some("") — that
        // would block the wrapper's `Script error:` fill, blanking the only
        // diagnostic in exactly the scenario "honest unknown" was built for.
        let content = jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.149.0","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call","id":"ctc_1","status":"completed","call_id":"call_1","name":"exec","input":"const r = await tools.exec_command({\"cmd\":\"x\"});\ntext(r.output);\n"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"CommandExecution","id":"exec-h","command":["/bin/zsh","-lc","x"],
                "status":"timed_out","aggregated_output":""}}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call_output","id":"ctco_1","call_id":"call_1","output":[{"type":"input_text","text":"Script failed\nScript error:\ntimeout-diagnostic"}]}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        let row = &tool_rows(&result.turns)[0];
        assert_eq!(row.is_error, Some(true), "the wrapper sniff decides");
        assert!(
            row.output
                .as_deref()
                .is_some_and(|o| o.contains("timeout-diagnostic")),
            "unknown outcome must not suppress the diagnostic: {:?}",
            row.output
        );
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.reason.contains("unrecognized status")),
            "{:?}",
            result.warnings
        );
    }

    #[test]
    fn non_string_command_status_warns_and_defers_to_wrapper_sniff() {
        // Present-but-wrong-type is the same schema drift as an unrecognized
        // string; folding it into "missing" asserted success at exit 0.
        let content = paginated_shell_lines(&serde_json::json!({
            "type":"CommandExecution","id":"exec-i",
            "command":["/bin/zsh","-lc","x"],"status":42,"exit_code":0}));
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        let row = &tool_rows(&result.turns)[0];
        assert_eq!(
            row.is_error,
            Some(false),
            "wrapper said 'Script completed'; the sniff fills the unknown"
        );
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.reason.contains("status is not a string")),
            "{:?}",
            result.warnings
        );

        // With positive failure evidence the conviction survives the bad type.
        let content = paginated_shell_lines(&serde_json::json!({
            "type":"CommandExecution","id":"exec-j",
            "command":["/bin/zsh","-lc","x"],"status":42,"exit_code":3}));
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        assert_eq!(tool_rows(&result.turns)[0].is_error, Some(true));
    }

    #[test]
    fn mcp_unknown_or_incomplete_status_warns_instead_of_reading_as_success() {
        let content = jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.149.0","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call","id":"ctc_1","status":"completed","call_id":"call_1","name":"exec","input":"dynamic"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"McpToolCall","id":"exec-m1","server":"srv","tool":"a","arguments":{},
                "status":"inProgress","result":null}}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"McpToolCall","id":"exec-m2","server":"srv","tool":"b","arguments":{},
                "status":"cancelled","result":{"content":[{"type":"text","text":"partial"}],"isError":false}}}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call_output","id":"ctco_1","call_id":"call_1","output":[{"type":"input_text","text":"done"}]}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        let rows = tool_rows(&result.turns);

        // Truncated mid-call: incomplete, not successful.
        let incomplete = rows.iter().find(|r| r.name == "srv.a").expect("row");
        assert_eq!(incomplete.is_error, Some(true));
        // Unknown status with a clean result: honest unknown, warned — a child
        // row keeps None (no pairing reaches it; renders as "done" today).
        let unknown = rows.iter().find(|r| r.name == "srv.b").expect("row");
        assert_eq!(unknown.is_error, None);
        assert_eq!(
            result
                .warnings
                .iter()
                .filter(|w| w.reason.contains("McpToolCall"))
                .count(),
            2,
            "{:?}",
            result.warnings
        );
    }

    #[test]
    fn command_output_falls_back_to_stdout_stderr_when_aggregated_absent() {
        let content = paginated_shell_lines(&serde_json::json!({
            "type":"CommandExecution","id":"exec-k","command":["/bin/zsh","-lc","x"],
            "status":"completed","exit_code":0,"stdout":"out-line","stderr":"err-line"}));
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        assert_eq!(
            tool_rows(&result.turns)[0].output.as_deref(),
            Some("out-line\nerr-line")
        );
    }

    #[test]
    fn legacy_ok_envelope_with_non_text_content_gets_live_placeholder() {
        // The disclosed legacy behavior change: a legacy `Ok` payload whose
        // content is all-non-text now yields the live parser's placeholder
        // instead of hydrating as "" — the same extractor serves all surfaces.
        let (output, is_error) = decode_mcp_result(Some(&serde_json::json!({
            "Ok": {"content": [{"type":"image","data":"AAAA"}], "isError": false}
        })));
        assert_eq!(output, "[non-text tool result omitted]");
        assert!(!is_error);
    }

    #[test]
    fn batched_unknown_status_child_keeps_output_and_error_none() {
        // Pins the child-row Option pass-through: an own-row command child with
        // unknown status and empty structured output must carry output: None
        // ("no output recorded" — no pairing ever reaches a child) and
        // is_error: None. Reintroducing `unwrap_or_default()` on the child
        // path would flatten output to Some("") and pass every other test.
        let content = jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.149.0","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call","id":"ctc_1","status":"completed","call_id":"call_1","name":"exec","input":"dynamic batched script"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"CommandExecution","id":"exec-child-unknown","command":["/bin/zsh","-lc","x"],
                "status":"timed_out","aggregated_output":""}}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call_output","id":"ctco_1","call_id":"call_1","output":[{"type":"input_text","text":"Script failed\n"}]}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        let child = tool_rows(&result.turns)
            .into_iter()
            .find(|r| r.name == "exec_command")
            .expect("child row");
        assert_eq!(child.output, None);
        assert_eq!(child.is_error, None);
    }

    #[test]
    fn mcp_result_evidence_convicts_under_unknown_status() {
        // An unreadable status does not launder a tool-reported error into
        // "unknown": result.isError convicts regardless.
        let content = jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.149.0","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call","id":"ctc_1","status":"completed","call_id":"call_1","name":"exec","input":"dynamic"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"McpToolCall","id":"exec-me","server":"srv","tool":"c","arguments":{},
                "status":"cancelled","result":{"content":[{"type":"text","text":"boom"}],"isError":true}}}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call_output","id":"ctco_1","call_id":"call_1","output":[{"type":"input_text","text":"done"}]}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        let row = tool_rows(&result.turns)
            .into_iter()
            .find(|r| r.name == "srv.c")
            .expect("row");
        assert_eq!(row.is_error, Some(true));
    }

    #[test]
    fn non_string_file_change_status_warns_and_stays_unknown() {
        // The same raw-value rule as commands and MCP: a numeric status is
        // schema drift, not an absent optional field asserting success.
        let content = jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.149.0","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call","id":"ctc_1","status":"completed","call_id":"call_1","name":"exec","input":"dynamic"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"FileChange","id":"exec-fc-42","status":42,
                "changes":{"/tmp/a.txt":{"type":"update","unified_diff":"@@ -1 +1 @@\n-a\n+b\n","move_path":null}}}}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call_output","id":"ctco_1","call_id":"call_1","output":[{"type":"input_text","text":"ok"}]}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        let edit = tool_rows(&result.turns)
            .into_iter()
            .find(|r| r.name == "apply_patch")
            .expect("edit row");
        assert_eq!(edit.is_error, None);
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.reason.contains("FileChange status is not a string")),
            "{:?}",
            result.warnings
        );
    }

    // --- Paginated live-enrichment patch facets (M4) ---

    #[test]
    fn paginated_enrichment_collects_file_change_patch_facets_in_order() {
        use crate::facets::ToolFacet;
        let content =
            std::fs::read_to_string(fixture_path("paginated-batched-wrapper.session.jsonl"))
                .unwrap();
        let enrichment = parse_session_content(&content);
        assert_eq!(enrichment.patch_facets.len(), 1);
        let ToolFacet::Edit { files } = &enrichment.patch_facets[0] else {
            panic!("content-bearing Edit facet expected");
        };
        assert_eq!(files[0].edits[0].old, "foo");
        assert_eq!(files[0].edits[0].new, "bar");

        // Order is load-bearing, and one facet cannot prove it: two edits to
        // the SAME path have identical path sets, so the ordinal pairing is
        // the only thing keeping each live row matched to its own diff — a
        // reversal or dedup here would swap the diffs with no warning.
        let edit = |old: &str, new: &str, id: &str| {
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"FileChange","id":id,"status":"completed",
                "changes":{"/tmp/a.txt":{"type":"update",
                    "unified_diff":format!("@@ -1 +1 @@\n-{old}\n+{new}\n"),"move_path":null}}}}})
        };
        let content = jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.149.0","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            edit("foo", "bar", "exec-first"),
            edit("bar", "baz", "exec-second"),
        ]);
        let enrichment = parse_session_content(&content);
        assert_eq!(enrichment.patch_facets.len(), 2);
        let transitions: Vec<(String, String)> = enrichment
            .patch_facets
            .iter()
            .map(|f| {
                let ToolFacet::Edit { files } = f else {
                    panic!("edit facet expected");
                };
                (files[0].edits[0].old.clone(), files[0].edits[0].new.clone())
            })
            .collect();
        assert_eq!(
            transitions,
            vec![
                ("foo".to_owned(), "bar".to_owned()),
                ("bar".to_owned(), "baz".to_owned())
            ],
            "record order must be preserved"
        );
    }

    #[test]
    fn paginated_enrichment_single_sources_patch_facets() {
        // A paginated file carrying BOTH a legacy-shaped standalone
        // `apply_patch` call and the canonical FileChange item must yield one
        // facet, not two — a doubled facet would desync the ordinal pairing
        // against the live rows.
        let patch = "*** Begin Patch\n*** Update File: /tmp/a.txt\n@@\n-a\n+b\n*** End Patch\n";
        let content = jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.149.0","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call","id":"ctc_1","status":"completed","call_id":"call_1","name":"apply_patch","input":patch}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"FileChange","id":"exec-fc","status":"completed",
                "changes":{"/tmp/a.txt":{"type":"update","unified_diff":"@@ -1 +1 @@\n-a\n+b\n","move_path":null}}}}}),
        ]);
        let enrichment = parse_session_content(&content);
        assert_eq!(enrichment.patch_facets.len(), 1, "single-sourced per mode");
    }

    #[test]
    fn paginated_enrichment_patch_facets_are_turn_scoped() {
        // A new task_started must clear the prior turn's facets — the upgrade
        // must never replay a previous turn's patches onto this turn's rows.
        let content = jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.149.0","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"FileChange","id":"exec-old","status":"completed",
                "changes":{"/tmp/old.txt":{"type":"update","unified_diff":"@@ -1 +1 @@\n-x\n+y\n","move_path":null}}}}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-2"}}),
        ]);
        let enrichment = parse_session_content(&content);
        assert!(
            enrichment.patch_facets.is_empty(),
            "prior turn's facet leaked"
        );
    }

    #[test]
    fn rogue_legacy_mcp_end_on_paginated_file_does_not_touch_rows() {
        // The other half of the reconstruction-side gate: a contract-violating
        // legacy `mcp_tool_call_end` on a paginated file must neither mutate
        // the wrapper row nor add a second MCP row — the paginated
        // `McpToolCall` item is the single source, and its result must win.
        let content = jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.149.0","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call","id":"ctc_1","status":"completed","call_id":"call_1","name":"exec","input":"dynamic"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"McpToolCall","id":"exec-mcp","server":"srv","tool":"do","arguments":{},
                "status":"completed","result":{"content":[{"type":"text","text":"paginated-result"}],"isError":false}}}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"mcp_tool_call_end","call_id":"call_1",
                "invocation":{"server":"srv","tool":"do"},
                "result":{"Ok":{"content":[{"type":"text","text":"rogue-legacy-result"}],"isError":true}}}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call_output","id":"ctco_1","call_id":"call_1","output":[{"type":"input_text","text":"done"}]}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        let rows = tool_rows(&result.turns);

        let mcp: Vec<_> = rows.iter().filter(|r| r.name == "srv.do").collect();
        assert_eq!(
            mcp.len(),
            1,
            "one MCP row, not one per generation: {rows:?}"
        );
        assert_eq!(mcp[0].is_error, Some(false), "the paginated result decides");
        assert!(
            mcp[0]
                .output
                .as_deref()
                .is_some_and(|o| o.contains("paginated-result")),
            "{:?}",
            mcp[0].output
        );
        // The rogue record targeted the wrapper's call_id, so assert against
        // every row rather than the wrapper alone — the wrapper is superseded
        // by its MCP child here, and a leak must be caught wherever it lands.
        assert!(
            !rows.iter().any(|r| r
                .output
                .as_deref()
                .is_some_and(|o| o.contains("rogue-legacy-result"))),
            "{rows:?}"
        );
    }

    #[test]
    fn rogue_legacy_records_on_paginated_file_do_not_duplicate_rows() {
        // The reconstruction-side single-source gate: a contract-violating
        // legacy `patch_apply_end` on a paginated file must not push a second
        // edit row next to the FileChange child (match-else-push would never
        // match the child's synthetic id).
        let content = jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.149.0","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call","id":"ctc_1","status":"completed","call_id":"call_1","name":"exec","input":"dynamic"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"FileChange","id":"exec-fc","status":"completed",
                "changes":{"/tmp/a.txt":{"type":"update","unified_diff":"@@ -1 +1 @@\n-a\n+b\n","move_path":null}}}}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"patch_apply_end","call_id":"exec-rogue","success":true,"status":"completed",
                "changes":{"/tmp/a.txt":{"type":"update","unified_diff":"@@ -1 +1 @@\n-a\n+b\n","move_path":null}}}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call_output","id":"ctco_1","call_id":"call_1","output":[{"type":"input_text","text":"ok"}]}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}),
        ]);
        let result = parse_codex_transcript_content(&content, Uuid::now_v7());
        let edits: Vec<_> = tool_rows(&result.turns)
            .into_iter()
            .filter(|r| r.name == "apply_patch")
            .collect();
        assert_eq!(
            edits.len(),
            1,
            "one edit, not one per generation: {edits:?}"
        );
    }

    #[test]
    fn rename_move_path_survives_hydration_in_both_generations() {
        // Loader-level, not helper-level: proves the mode routing delivers the
        // rename to `patch_apply_end_facet` in each generation — a gating or
        // routing regression could drop it from one while helper tests stay
        // green.
        let change = serde_json::json!({"/tmp/old.txt": {
            "type": "update", "unified_diff": "@@ -1 +1 @@\n-x\n+y\n",
            "move_path": "/tmp/new.txt"}});

        // Legacy: `patch_apply_end` record.
        let legacy = jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.146.0","history_mode":"legacy"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"patch_apply_end","call_id":"exec-p1","success":true,"status":"completed","changes":change}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}),
        ]);
        let result = parse_codex_transcript_content(&legacy, Uuid::now_v7());
        let rows = tool_rows(&result.turns);
        let crate::facets::ToolFacet::Edit { files } = &rows[0].facet else {
            panic!("legacy edit facet expected: {rows:?}");
        };
        assert_eq!(files[0].moved_to.as_deref(), Some("/tmp/new.txt"));

        // Paginated: `item_completed/FileChange` child.
        let paginated = jsonl_lines(&[
            serde_json::json!({"type":"session_meta","payload":{"cli_version":"0.149.0","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"t-1"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call","id":"ctc_1","status":"completed","call_id":"call_1","name":"exec","input":"dynamic"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"FileChange","id":"exec-fc","status":"completed","changes":change}}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call_output","id":"ctco_1","call_id":"call_1","output":[{"type":"input_text","text":"ok"}]}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t-1"}}),
        ]);
        let result = parse_codex_transcript_content(&paginated, Uuid::now_v7());
        let edit = tool_rows(&result.turns)
            .into_iter()
            .find(|r| r.name == "apply_patch")
            .expect("paginated edit row");
        let crate::facets::ToolFacet::Edit { files } = &edit.facet else {
            panic!("paginated edit facet expected");
        };
        assert_eq!(files[0].moved_to.as_deref(), Some("/tmp/new.txt"));
    }
}
