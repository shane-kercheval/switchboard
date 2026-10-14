//! What the accounts' limits look like to an orchestrator: `get_usage`.
//!
//! **The window's stored map is the source.** The window writes the newest
//! reading per harness to `AppState.harness_usage` after every reading, and
//! owns the rule for which of several readings is newest, so this reads that
//! map and interprets nothing about recency. The vendor fields are read here
//! exactly as the meter reads them (`usageWindows.ts`), so a vendor change
//! breaks both at once and is noticed in the window. The rules copied, so a
//! change to one is made to the other: the Claude key list and each key's
//! scope; Codex's scope from `normalModelSlug`, including the skip when the
//! association is missing or unreadable; a window whose fraction cannot be
//! read is dropped (Claude: outside 0 to 1; Codex: `usedPercent` not a
//! number), and a Codex quota with both slots `null` contributes nothing; a
//! window whose reset time has passed is dropped (it describes the previous
//! cycle); and the missing-reset rule differs by harness: Claude drops a window
//! with no reset time (the store never prunes, so one could never be retired),
//! Codex keeps it with `resets_at: null`. Both copies run the cases in
//! `desktop/tests/fixtures/usage-readings.json`.
//!
//! **Freshness.** Claude reports its limits only with a reply, so its reading
//! is as old as the newest Claude reply anywhere in the app; each window
//! carries its own `observed_at`. Codex can be asked: the tool runs the same
//! account read the window's meter refreshes with, under the call's budget,
//! and returns it dated now, or the newer of the stored and its own last
//! reading when the read fails or does not finish in time. One read is in
//! flight at a time, shared by concurrent calls; the read is skipped when the
//! newer of the stored reading and this module's own is under a minute old;
//! and the result is never written back to the map, which the window replaces
//! whole on its next save (and the window deliberately runs one further read
//! after an in-flight one, which joining its calls to a shared read would
//! defeat; see `accountUsage.svelte.ts`).

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use switchboard_harness::CodexAccountUsage;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::error::AppError;
use crate::harness_usage::HarnessUsage;
use crate::ops::handle::AppStateHandle;
use crate::state::lock;

/// How long `get_usage` waits for a Codex account read before answering from
/// what is stored. Covers the read's own bound and the PATH wait before it,
/// inside every client's shortest call limit.
pub const CODEX_READ_BUDGET: Duration = Duration::from_secs(45);

/// A Codex reading younger than this is returned without a new read.
const FRESH_FOR: chrono::Duration = chrono::Duration::seconds(60);

/// Claude's limit windows, in the meter's order, with each key's scope and
/// the model family the key itself names. Keys outside this list are not
/// Claude Code limits on any plan we can probe and are dropped, as the meter
/// drops them.
const CLAUDE_WINDOWS: &[(&str, Scope, Option<&str>)] = &[
    ("five_hour", Scope::Account, None),
    ("seven_day", Scope::Account, None),
    ("seven_day_overage_included", Scope::Model, None),
    ("seven_day_opus", Scope::Model, Some("opus")),
    ("seven_day_sonnet", Scope::Model, Some("sonnet")),
];

/// One harness account's limits.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct HarnessUsageReport {
    pub harness: String,
    /// When the account's reading was taken (Codex: the whole reading; Claude:
    /// the newest reading, each window dating itself).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<DateTime<Utc>>,
    /// Codex's own statement of whether included usage may be drawn on now:
    /// an observed fact, not permission to incur charges.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ordinary_usage_allowed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    pub windows: Vec<UsageWindowReport>,
}

/// Whom a window's allowance governs.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Ordinary work on any model.
    Account,
    /// A reserve for one model or a set of models; its headroom is not the
    /// allowance that governs ordinary work.
    Model,
}

/// One limit window.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct UsageWindowReport {
    pub key: String,
    pub scope: Scope,
    /// For a model-scoped window: the family the key names, or for Claude's
    /// `seven_day_overage_included` a model this limit covers (the one whose
    /// reply delivered the reading), when one was stored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 0 to 1, used.
    pub used_fraction: f64,
    /// `null` when Codex reported no reset time for the window.
    pub resets_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_minutes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<DateTime<Utc>>,
    /// Claude: whether the reading that delivered this window was a paid
    /// overage turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_using_overage: Option<bool>,
    /// Codex: the limit's credits block, as it arrived.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credits: Option<Value>,
}

/// The Codex account read, injected so tests can count and stall it.
pub type ReadFn = dyn Fn(Arc<dyn AppStateHandle>) -> Pin<Box<dyn Future<Output = Option<CodexAccountUsage>> + Send>>
    + Send
    + Sync;

/// A Codex reading and when it was taken.
pub type CodexReading = (CodexAccountUsage, DateTime<Utc>);

#[cfg(test)]
type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

struct ReaderInner {
    latest: Option<CodexReading>,
    /// The read in flight: `true` once it has finished.
    in_flight: Option<watch::Sender<bool>>,
}

/// Agent access's own Codex account reads: one in flight, the newest result
/// kept, never written to the window's map.
pub struct CodexUsageReader {
    read: Mutex<Arc<ReadFn>>,
    inner: Mutex<ReaderInner>,
    #[cfg(test)]
    clock: Mutex<Option<Clock>>,
}

impl CodexUsageReader {
    #[must_use]
    pub fn new(read: Arc<ReadFn>) -> Self {
        Self {
            read: Mutex::new(read),
            inner: Mutex::new(ReaderInner {
                latest: None,
                in_flight: None,
            }),
            #[cfg(test)]
            clock: Mutex::new(None),
        }
    }

    #[cfg(test)]
    pub fn set_read(&self, read: Arc<ReadFn>) {
        *lock(&self.read) = read;
    }

    #[cfg(test)]
    pub fn set_clock(&self, clock: Clock) {
        *lock(&self.clock) = Some(clock);
    }

    #[cfg_attr(not(test), allow(clippy::unused_self))] // The test clock reads `self`.
    fn now(&self) -> DateTime<Utc> {
        #[cfg(test)]
        if let Some(clock) = lock(&self.clock).as_ref() {
            return clock();
        }
        Utc::now()
    }

    /// This module's newest Codex reading after reading if the newer of
    /// `stored_at` (the window's reading) and its own is a minute old or more,
    /// waiting up to `budget` for a read in flight, its own or one joined.
    pub async fn reading(
        self: &Arc<Self>,
        handle: Arc<dyn AppStateHandle>,
        stored_at: Option<DateTime<Utc>>,
        budget: Duration,
        cancel: &CancellationToken,
    ) -> Option<CodexReading> {
        let now = self.now();
        let receiver = {
            let mut inner = lock(&self.inner);
            let own_at = inner.latest.as_ref().map(|(_, at)| *at);
            let newest = match (stored_at, own_at) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            };
            if newest.is_some_and(|at| now - at < FRESH_FOR) {
                return inner.latest.clone();
            }
            match &inner.in_flight {
                Some(done) if !*done.borrow() => done.subscribe(),
                _ => {
                    let (done, receiver) = watch::channel(false);
                    inner.in_flight = Some(done.clone());
                    let read = Arc::clone(&*lock(&self.read));
                    let reader = Arc::clone(self);
                    tokio::spawn(async move {
                        let result = read(handle).await;
                        let mut inner = lock(&reader.inner);
                        if let Some(usage) = result {
                            inner.latest = Some((usage, reader.now()));
                        }
                        inner.in_flight = None;
                        drop(inner);
                        done.send_replace(true);
                    });
                    receiver
                }
            }
        };
        let finished = async move {
            let mut receiver = receiver;
            let _ = receiver.wait_for(|done| *done).await;
        };
        tokio::select! {
            () = cancel.cancelled() => {}
            _ = tokio::time::timeout(budget, finished) => {}
        }
        lock(&self.inner).latest.clone()
    }
}

/// Every harness account the app has a reading for, with Codex's refreshed
/// when it can be. An empty store is an empty list.
pub async fn get_usage(
    handle: Arc<dyn AppStateHandle>,
    budget: Duration,
    cancel: &CancellationToken,
) -> Result<Vec<HarnessUsageReport>, AppError> {
    let state = handle.app_state();
    let stored = lock(&state.harness_usage).clone();
    let stored_codex_at = stored
        .harnesses
        .get("codex")
        .and_then(|reading| reading.get("observed_at"))
        .and_then(parse_instant);
    let fresh = state
        .codex_usage_reader
        .reading(Arc::clone(&handle), stored_codex_at, budget, cancel)
        .await;
    Ok(report(&stored, fresh.as_ref(), Utc::now()))
}

/// The report for a stored map, taking `fresh` for Codex when it is newer than
/// the stored reading.
#[must_use]
pub fn report(
    stored: &HarnessUsage,
    fresh: Option<&CodexReading>,
    now: DateTime<Utc>,
) -> Vec<HarnessUsageReport> {
    let mut out = Vec::new();
    let mut codex_done = false;
    for (harness, reading) in &stored.harnesses {
        match harness.as_str() {
            "claude_code" => {
                if let Some(report) = claude_report(reading, now) {
                    out.push(report);
                }
            }
            "codex" => {
                codex_done = true;
                let stored_at = reading.get("observed_at").and_then(parse_instant);
                let (payload, observed_at) = match fresh {
                    Some((usage, at)) if stored_at.is_none_or(|stored_at| *at > stored_at) => {
                        (serde_json::to_value(usage).ok(), Some(*at))
                    }
                    _ => (reading.get("payload").cloned(), stored_at),
                };
                if let Some(report) = payload
                    .as_ref()
                    .and_then(|payload| codex_report(payload, observed_at, now))
                {
                    out.push(report);
                }
            }
            _ => {}
        }
    }
    if !codex_done
        && let Some((usage, at)) = fresh
        && let Some(payload) = serde_json::to_value(usage).ok()
        && let Some(report) = codex_report(&payload, Some(*at), now)
    {
        out.push(report);
    }
    out
}

fn claude_report(reading: &Value, now: DateTime<Utc>) -> Option<HarnessUsageReport> {
    let held = reading.get("windows")?.as_object()?;
    let mut windows = Vec::new();
    for (key, scope, named_model) in CLAUDE_WINDOWS {
        let Some(window) = held.get(*key).and_then(Value::as_object) else {
            continue;
        };
        let Some(vendor) = window.get("window").and_then(Value::as_object) else {
            continue;
        };
        let Some(utilization) = vendor.get("utilization").and_then(Value::as_f64) else {
            continue;
        };
        if !(0.0..=1.0).contains(&utilization) {
            continue;
        }
        // Dropped without a reset rather than kept with `null`, unlike Codex:
        // the store never prunes, so a window held without one could never be
        // retired.
        let Some(resets_at) = vendor.get("resetsAt").and_then(instant_from_seconds) else {
            continue;
        };
        if resets_at <= now {
            continue;
        }
        let model = match *key {
            "seven_day_overage_included" => window
                .get("model")
                .and_then(Value::as_str)
                .filter(|m| !m.is_empty())
                .map(str::to_owned),
            _ => named_model.map(str::to_owned),
        };
        windows.push(UsageWindowReport {
            key: (*key).to_owned(),
            scope: *scope,
            model,
            used_fraction: utilization,
            resets_at: Some(resets_at),
            duration_minutes: None,
            observed_at: window.get("observed_at").and_then(parse_instant),
            is_using_overage: window.get("is_using_overage").and_then(Value::as_bool),
            credits: None,
        });
    }
    Some(HarnessUsageReport {
        harness: "claude_code".to_owned(),
        observed_at: reading.get("observed_at").and_then(parse_instant),
        ordinary_usage_allowed: None,
        plan: None,
        windows,
    })
}

fn codex_report(
    payload: &Value,
    observed_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<HarnessUsageReport> {
    let payload = payload.as_object()?;
    let buckets = payload.get("rateLimitsByLimitId")?.as_object()?;
    let mut windows = Vec::new();
    let mut plan = None;
    for (limit_id, bucket) in buckets {
        let Some(bucket) = bucket.as_object() else {
            continue;
        };
        if plan.is_none() {
            plan = bucket
                .get("planType")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        // A quota that does not say which model it is tied to is skipped, not
        // taken to be account-wide: that would present a reserve as the
        // allowance governing ordinary work.
        let (scope, model) = match bucket.get("normalModelSlug") {
            Some(Value::Null) => (Scope::Account, None),
            Some(Value::String(slug)) => (Scope::Model, Some(slug.clone())),
            _ => continue,
        };
        let credits = bucket.get("credits").filter(|c| !c.is_null()).cloned();
        for slot in ["primary", "secondary"] {
            let Some(window) = bucket.get(slot).and_then(Value::as_object) else {
                continue;
            };
            let Some(used_percent) = window.get("usedPercent").and_then(Value::as_f64) else {
                continue;
            };
            let resets_at = match window.get("resetsAt").and_then(instant_from_seconds) {
                Some(at) if at <= now => continue,
                other => other,
            };
            windows.push(UsageWindowReport {
                key: format!("{limit_id}.{slot}"),
                scope,
                model: model.clone(),
                used_fraction: used_percent / 100.0,
                resets_at,
                duration_minutes: window.get("windowDurationMins").and_then(Value::as_u64),
                observed_at,
                is_using_overage: None,
                credits: credits.clone(),
            });
        }
    }
    Some(HarnessUsageReport {
        harness: "codex".to_owned(),
        observed_at,
        ordinary_usage_allowed: payload.get("ordinaryUsageAllowed").and_then(Value::as_bool),
        plan,
        windows,
    })
}

fn parse_instant(value: &Value) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value.as_str()?)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

/// A vendor reset time, epoch seconds as either harness stores it.
fn instant_from_seconds(value: &Value) -> Option<DateTime<Utc>> {
    let seconds = value.as_f64()?;
    #[allow(clippy::cast_possible_truncation)] // Epoch seconds, far below i64.
    let whole = seconds.floor() as i64;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let nanos = ((seconds - seconds.floor()) * 1e9) as u32;
    DateTime::from_timestamp(whole, nanos)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::{Value, json};
    use tokio::sync::Notify;

    use super::*;
    use crate::ops::test_support::TestEnv;

    const FIXTURE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tests/fixtures/usage-readings.json"
    ));

    fn fixture() -> (HarnessUsage, DateTime<Utc>, Value) {
        let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
        let stored = HarnessUsage {
            harnesses: serde_json::from_value(fixture["harnesses"].clone()).unwrap(),
        };
        let now = DateTime::parse_from_rfc3339(fixture["now"].as_str().unwrap())
            .unwrap()
            .with_timezone(&Utc);
        (stored, now, fixture["expected"].clone())
    }

    #[test]
    fn the_report_over_the_shared_fixture_is_what_both_readers_expect() {
        let (stored, now, expected) = fixture();
        let report = report(&stored, None, now);
        assert_eq!(report.len(), 2);
        assert_eq!(
            serde_json::to_value(&report[0]).unwrap(),
            expected["claude_code"]
        );
        assert_eq!(serde_json::to_value(&report[1]).unwrap(), expected["codex"]);
    }

    #[test]
    fn an_empty_store_is_an_empty_list_and_a_harness_without_windows_is_an_empty_entry() {
        assert!(report(&HarnessUsage::default(), None, Utc::now()).is_empty());
        let stored = HarnessUsage {
            harnesses: [(
                "claude_code".to_owned(),
                json!({ "observed_at": "2026-10-14T00:00:00Z", "payload": {}, "windows": {} }),
            )]
            .into_iter()
            .collect(),
        };
        let report = report(&stored, None, Utc::now());
        assert_eq!(report.len(), 1);
        assert!(report[0].windows.is_empty());
    }

    fn codex_usage(used_percent: u64) -> CodexAccountUsage {
        serde_json::from_value(json!({
            "ordinaryUsageAllowed": true,
            "rateLimitsByLimitId": {
                "codex": { "normalModelSlug": null, "planType": "pro",
                           "primary": { "usedPercent": used_percent, "windowDurationMins": 300 },
                           "secondary": null }
            }
        }))
        .unwrap()
    }

    #[test]
    fn a_fresh_codex_reading_is_used_only_when_newer_than_the_stored_one() {
        let (stored, now, _) = fixture();
        let newer = now - chrono::Duration::seconds(30);
        let report_newer = report(&stored, Some(&(codex_usage(42), newer)), now);
        let codex = report_newer.iter().find(|r| r.harness == "codex").unwrap();
        assert_eq!(codex.observed_at, Some(newer));
        assert_eq!(codex.windows.len(), 1);
        assert!((codex.windows[0].used_fraction - 0.42).abs() < f64::EPSILON);
        assert_eq!(codex.plan.as_deref(), Some("pro"));

        let older = now - chrono::Duration::hours(1);
        let report_older = report(&stored, Some(&(codex_usage(42), older)), now);
        let codex = report_older.iter().find(|r| r.harness == "codex").unwrap();
        assert_eq!(codex.windows.len(), 4, "the stored reading stands");

        // With nothing stored for Codex, a fresh reading is still reported.
        let only_fresh = report(&HarnessUsage::default(), Some(&(codex_usage(7), now)), now);
        assert_eq!(only_fresh.len(), 1);
        assert_eq!(only_fresh[0].harness, "codex");
    }

    /// A reader whose read counts its runs and waits at `gate` when given one.
    fn counting_reader(
        gate: Option<Arc<Notify>>,
        result: Option<CodexAccountUsage>,
    ) -> (Arc<CodexUsageReader>, Arc<AtomicUsize>) {
        let runs = Arc::new(AtomicUsize::new(0));
        let reader = CodexUsageReader::new(Arc::new({
            let runs = Arc::clone(&runs);
            move |_handle| {
                runs.fetch_add(1, Ordering::SeqCst);
                let gate = gate.clone();
                let result = result.clone();
                Box::pin(async move {
                    if let Some(gate) = gate {
                        gate.notified().await;
                    }
                    result
                })
            }
        }));
        (Arc::new(reader), runs)
    }

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-14T01:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + chrono::Duration::seconds(seconds)
    }

    #[tokio::test]
    async fn two_concurrent_calls_run_one_codex_process() {
        let env = TestEnv::new();
        let gate = Arc::new(Notify::new());
        let (reader, runs) = counting_reader(Some(Arc::clone(&gate)), Some(codex_usage(5)));
        let cancel = CancellationToken::new();
        let first = tokio::spawn({
            let (reader, handle, cancel) = (Arc::clone(&reader), env.handle(), cancel.clone());
            async move {
                reader
                    .reading(handle, None, Duration::from_secs(5), &cancel)
                    .await
            }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        let second = tokio::spawn({
            let (reader, handle, cancel) = (Arc::clone(&reader), env.handle(), cancel.clone());
            async move {
                reader
                    .reading(handle, None, Duration::from_secs(5), &cancel)
                    .await
            }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        gate.notify_waiters();
        let (first, second) = (first.await.unwrap(), second.await.unwrap());
        assert_eq!(first.map(|(u, _)| u), Some(codex_usage(5)));
        assert_eq!(second.map(|(u, _)| u), Some(codex_usage(5)));
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_reading_under_a_minute_old_is_returned_without_a_read() {
        let env = TestEnv::new();
        let (reader, runs) = counting_reader(None, Some(codex_usage(5)));
        let clock = Arc::new(Mutex::new(at(0)));
        reader.set_clock(Arc::new({
            let clock = Arc::clone(&clock);
            move || *lock(&clock)
        }));
        let cancel = CancellationToken::new();
        let first = reader
            .reading(env.handle(), None, Duration::from_secs(5), &cancel)
            .await;
        assert_eq!(first.as_ref().map(|(_, at)| *at), Some(at(0)));
        assert_eq!(runs.load(Ordering::SeqCst), 1);

        *lock(&clock) = at(30);
        let again = reader
            .reading(env.handle(), None, Duration::from_secs(5), &cancel)
            .await;
        assert_eq!(again, first, "thirty seconds later, the same reading");
        assert_eq!(runs.load(Ordering::SeqCst), 1);

        // The window's own reading counts too: a stored reading fresher than a
        // minute skips the read.
        *lock(&clock) = at(120);
        let stored_fresh = reader
            .reading(env.handle(), Some(at(100)), Duration::from_secs(5), &cancel)
            .await;
        assert_eq!(stored_fresh, first);
        assert_eq!(runs.load(Ordering::SeqCst), 1);

        // Both a minute old: read again.
        *lock(&clock) = at(200);
        let fresh = reader
            .reading(env.handle(), Some(at(100)), Duration::from_secs(5), &cancel)
            .await;
        assert_eq!(fresh.map(|(_, at)| at), Some(at(200)));
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_failed_read_keeps_the_previous_reading_and_a_slow_one_finishes_later() {
        let env = TestEnv::new();
        let cancel = CancellationToken::new();
        let (reader, runs) = counting_reader(None, None);
        assert_eq!(
            reader
                .reading(env.handle(), None, Duration::from_secs(5), &cancel)
                .await,
            None
        );
        assert_eq!(runs.load(Ordering::SeqCst), 1);

        let gate = Arc::new(Notify::new());
        let (reader, runs) = counting_reader(Some(Arc::clone(&gate)), Some(codex_usage(9)));
        let slow = reader
            .reading(env.handle(), None, Duration::from_millis(50), &cancel)
            .await;
        assert_eq!(slow, None, "nothing held yet, so nothing to fall back to");
        gate.notify_waiters();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let landed = reader
            .reading(env.handle(), None, Duration::from_secs(5), &cancel)
            .await;
        assert_eq!(landed.map(|(u, _)| u), Some(codex_usage(9)));
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "the slow read's result was kept"
        );
    }

    #[tokio::test]
    async fn get_usage_falls_back_to_the_stored_codex_reading_when_the_read_fails() {
        let env = TestEnv::new();
        let observed = Utc::now() - chrono::Duration::minutes(5);
        let resets = (Utc::now() + chrono::Duration::days(1)).timestamp();
        *lock(&env.state.harness_usage) = HarnessUsage {
            harnesses: [(
                "codex".to_owned(),
                json!({
                    "observed_at": observed.to_rfc3339(),
                    "payload": { "ordinaryUsageAllowed": true, "rateLimitsByLimitId": {
                        "codex": { "normalModelSlug": null, "planType": "plus",
                                   "primary": { "usedPercent": 12, "resetsAt": resets, "windowDurationMins": 300 },
                                   "secondary": null } } },
                }),
            )]
            .into_iter()
            .collect(),
        };
        env.state
            .codex_usage_reader
            .set_read(Arc::new(|_handle| Box::pin(async { None })));
        let report = get_usage(
            env.handle(),
            Duration::from_secs(5),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].harness, "codex");
        assert_eq!(report[0].observed_at, Some(observed));
        assert_eq!(report[0].windows[0].key, "codex.primary");
        assert!((report[0].windows[0].used_fraction - 0.12).abs() < f64::EPSILON);
    }
}
