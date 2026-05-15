//! Codex session-file lookup, parsing, and post-turn enrichment.
//!
//! After each turn's terminal stream event (`turn.completed` / `turn.failed`),
//! the Codex adapter reads the session file to fill in metadata the stream
//! omits. Per `docs/research/codex-cli-observed.md` M2.4-prep findings, the
//! session file is the **only** source for:
//! - `event_msg/task_started.payload.model_context_window` →
//!   `TurnEnd.usage.context_window` (per-turn).
//! - `event_msg/token_count.rate_limits` (non-null variant only) →
//!   `RateLimitEvent.info` (per-turn).
//! - `session_meta.payload.cli_version` → `SessionMeta.harness_version` (once
//!   per session).
//! - `turn_context.payload.model` (first one in file) → `SessionMeta.model`
//!   (once per session).
//!
//! ## ID-space distinction
//!
//! Switchboard `TurnId` is dispatcher-local (UUID v7 we generate). Codex
//! session-file `turn_id` is harness-local (UUID v7 Codex generates). The two
//! **never** match by design. Per-turn selection is by **last-record-in-file
//! at terminal-event time**, not by id — Codex writes the session file
//! synchronously, so by the time `turn.completed` arrives, the last
//! `task_started` is the current turn's. A future cleanup that "matches by
//! `turn_id`" would silently match nothing.
//!
//! ## Path resolution
//!
//! Codex session files live at
//! `<home>/.codex/sessions/<YYYY>/<MM>/<DD>/rollout-*-<session-uuid>.jsonl`.
//! The date partition is the **original spawn date** (`Utc::today()` at first
//! dispatch), not the current date — Codex appends to the original file even
//! on cross-day resumes. The sidecar's `original_start_date_utc` is the
//! authoritative date input; **never** call `Utc::today()` at enrichment
//! time. (`docs/research/codex-cli-observed.md`.)
//!
//! ## `raw` field policy
//!
//! `SessionMeta.raw` carries the `session_meta` line for future forward-compat
//! field promotion. Codex's `session_meta.payload.base_instructions.text` is
//! the entire model system prompt (5–20KB) — never UI-rendered, but included
//! in the unstripped raw it would dominate the IPC payload. Strip the `text`
//! field of `base_instructions` to a sentinel; preserve the rest of the
//! envelope verbatim so the surrounding shape stays observable.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::NaiveDate;
use serde_json::Value;

use crate::events::McpServerStatus;

/// Per-attempt backoff between session-file read tries. Codex writes the
/// session file synchronously per `docs/research/codex-cli-observed.md`; by
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
    /// The full `session_meta` line as JSON, with
    /// `payload.base_instructions.text` replaced by a sentinel. Used as
    /// `SessionMeta.raw`. `None` if line 1 isn't a `session_meta` record.
    pub session_meta_raw: Option<Value>,
}

/// Compute the canonical session-file path glob's parent directory for a
/// given start date. Layout: `<home>/.codex/sessions/YYYY/MM/DD/`.
/// `%Y` / `%m` / `%d` already zero-pad to the expected widths.
#[must_use]
pub fn session_directory(home_dir: &Path, original_start_date_utc: NaiveDate) -> PathBuf {
    home_dir
        .join(".codex")
        .join("sessions")
        .join(original_start_date_utc.format("%Y").to_string())
        .join(original_start_date_utc.format("%m").to_string())
        .join(original_start_date_utc.format("%d").to_string())
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
/// unavailable. The "newest wins" rule matches the M2.4 plan and avoids
/// silently enriching from a stale duplicate.
///
/// A `glob` crate dep is unnecessary for one suffix-match pattern — a single
/// `read_dir` + suffix filter is simpler, has no allocations beyond the
/// filename strings, and avoids pulling in a transitive dep tree.
#[must_use]
pub fn locate_session_file(
    home_dir: &Path,
    original_start_date_utc: NaiveDate,
    session_id: &str,
) -> Option<PathBuf> {
    let dir = session_directory(home_dir, original_start_date_utc);
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
#[must_use]
pub fn parse_session_content(content: &str) -> Enrichment {
    let mut enrichment = Enrichment::default();
    let mut model_set = false; // first-turn_context wins (set-once gate)

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
                if let Some(p) = payload
                    && let Some(version) = p.get("cli_version").and_then(Value::as_str)
                {
                    enrichment.cli_version = Some(version.to_owned());
                }
                enrichment.session_meta_raw = Some(strip_base_instructions(value));
            }
            "turn_context" => {
                if !model_set
                    && let Some(p) = payload
                    && let Some(model) = p.get("model").and_then(Value::as_str)
                {
                    enrichment.model = Some(model.to_owned());
                    model_set = true;
                }
            }
            "event_msg" => {
                let Some(p) = payload else { continue };
                let event_type = p.get("type").and_then(Value::as_str).unwrap_or("");
                match event_type {
                    "task_started" => {
                        // Last-task_started-wins. On resumed sessions the
                        // file accumulates one task_started per turn; the
                        // current turn's is the most recent.
                        if let Some(window) = p.get("model_context_window").and_then(Value::as_u64)
                        {
                            enrichment.context_window = u32::try_from(window).ok();
                        }
                    }
                    "token_count" => {
                        // Two variants share this type — only the one with
                        // non-null `rate_limits` is what M2.4 wants. The
                        // info-only variant is ignored (the stream's
                        // turn.completed.usage carries token totals).
                        // Last-rate-limit-bearing-record-wins.
                        if let Some(rate_limits) = p.get("rate_limits")
                            && !rate_limits.is_null()
                        {
                            enrichment.rate_limits = Some(rate_limits.clone());
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    enrichment
}

/// Strip `payload.base_instructions.text` from a `session_meta` record. The
/// surrounding envelope is preserved verbatim so M2.6+ can still introspect
/// the field's existence and the `base_instructions` table's other keys.
/// Returns a clone — the caller owns the result.
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
    original_start_date_utc: NaiveDate,
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
        if let Some(path) = locate_session_file(home_dir, original_start_date_utc, session_id) {
            return parse_session_file(&path);
        }
    }
    tracing::warn!(
        session_id = %session_id,
        date = %original_start_date_utc,
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
/// `tools` is always `vec![]` for Codex — no equivalent registry source per
/// M2.4-prep findings; kept implicit on the adapter side rather than carried
/// here.
pub struct SessionMetaFields {
    pub model: String,
    pub harness_version: String,
    pub mcp_servers: Vec<McpServerStatus>,
    pub skills: Vec<String>,
    pub raw: Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use std::sync::Mutex;
    use tempfile::TempDir;

    fn fixture_path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/codex")
            .join(name)
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
    fn parse_filters_token_count_info_only_variant() {
        // The info-only token_count (rate_limits: null) must be ignored; the
        // stream's turn.completed.usage carries token telemetry. Only the
        // rate-limits-bearing variant feeds RateLimitEvent.
        let content = r#"
{"type":"session_meta","payload":{"cli_version":"0.130.0"}}
{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{}},"rate_limits":null}}
"#;
        let enrichment = parse_session_content(content);
        assert!(
            enrichment.rate_limits.is_none(),
            "info-only token_count must not populate rate_limits"
        );
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
        // The surrounding shape is preserved so M2.6+ can introspect.
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
        // Cross-midnight test: sidecar's original_start_date_utc says May
        // 15; host clock would say May 16. Lookup must use the sidecar's
        // date, not Utc::today(), and find the file in May 15's directory.
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
}
