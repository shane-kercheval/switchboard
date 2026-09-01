//! Moving an agent between projects: admission, surgery, and recovery.
//!
//! The operation relocates an agent's registry row, journal records, pins and
//! metadata sidecars from one project to another, stamping `session_home` when
//! the move crosses working directories. It is multi-file surgery on state
//! other agents write concurrently, so everything here is shaped by two rules:
//! both projects are closed to new work for the duration (`MoveAdmission` /
//! `MoveRecoveryBarrier` in `commands`), and **every surgery step is
//! idempotent**, so an interrupted move is finished by re-driving the whole
//! sequence rather than tracking how far it got.
//!
//! Recovery runs in two places with one implementation: immediately, when a
//! step fails in-app (both projects still blocked); and at startup, **before
//! any project can open**, draining every intent file in the store's `moves/`
//! directory. That ordering is load-bearing — recovery must complete a move
//! before an open-time consistency check could wall it off. A recovery that
//! itself fails blocks exactly the two named projects with a repair error
//! naming the intent file; everything else keeps working, and the next launch
//! retries.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use switchboard_core::name::canonicalize_for_uniqueness;
use switchboard_core::store::MoveIntent;
use switchboard_core::{
    AgentId, AgentRecord, CoreError, HarnessKind, JournalRecord, Project, ProjectId, Store,
    copy_file_durable, journal, pins,
};
use switchboard_harness::CancelSource;

use crate::commands::{
    acquire_project_lock, activate_project, begin_move_barrier, ensure_projects_quiescent,
    lookup_agent, reject_if_under_maintenance, resolve_session_file,
};
use crate::error::AppError;
use crate::state::{AppState, lock};

/// The last surgery step to run — a test seam for reproducing every crash
/// point. Production and recovery always pass `None` (run everything); tests
/// pass `Some(step)` to simulate a crash immediately after that step, then
/// invoke recovery and assert the result equals an uninterrupted move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum MoveStep {
    /// (a) moved records appended to the target journal.
    TargetJournal,
    /// (b) metadata sidecars copied to the target.
    Sidecars,
    /// (c) registry row adopted into the target.
    TargetRegistry,
    /// (d) source registry rewritten without the row — the commit point.
    SourceRegistry,
    /// (e) source journal rewritten without the moved records.
    SourceJournal,
    /// (f) pins partitioned between the two projects.
    Pins,
}

/// Move `agent_id` into `target_project`. Returns the adopted record.
///
/// Refuses rather than improvises: while another move runs, while either
/// project has work outstanding, on a canonical name collision in the target,
/// when the target is open in another process, or when either directory fails
/// to resolve. Running work is never cancelled to make a move happen.
pub(crate) async fn move_agent_impl(
    state: &AppState,
    agent_id: AgentId,
    target_project: ProjectId,
    home_dir: &Path,
) -> Result<AgentRecord, AppError> {
    // Serializes moves in this process only, and that is sufficient: the
    // store's files are covered cross-process by the two projects'
    // `instance.lock`s (held because both projects are open in this process —
    // the cold-target activation below acquires the target's), and the harness
    // session file by the cross-process session lock. A second process with the
    // target open could otherwise append to its journal mid-surgery — the same
    // append-loss hazard the quiescence gate closes in-process.
    let _serial = state
        .move_mutex
        .try_lock()
        .map_err(|_| AppError::MoveInProgress)?;

    let (source, agent) = lookup_agent(state, agent_id)?;
    if source.id == target_project {
        return Err(AppError::MoveSourceIsTarget);
    }
    reject_if_under_maintenance(state, target_project)?;
    // The picker's read-only browse deliberately never locks, so the commit
    // path goes through the real open: a cold target is activated here, which
    // acquires its `instance.lock` (`ProjectLocked` if another process holds
    // it) and resolves its working directory (typed failure if it cannot).
    let target = {
        let loaded = lock(&state.projects).get(&target_project).cloned();
        if let Some(project) = loaded {
            project
        } else {
            let project = state.store.open_project(target_project)?;
            activate_project(state, project.clone())?;
            project
        }
    };

    // Marks first, then verify: quiescence checked before the marks exist can
    // go stale by the time they do. `admission` releases the marks on any `?`
    // below until the intent record is durable.
    let admission = begin_move_barrier(state, source.id, target.id)?;
    ensure_projects_quiescent(state, [source.id, target.id]).await?;
    // Draining an idle actor cancels nothing (quiescence just verified); it
    // exists because the actor's dispatch factory captured the source project
    // at creation and would journal a post-move turn into it. The next send
    // lazily recreates the actor against the target.
    state
        .dispatcher
        .shutdown_agent(agent_id, CancelSource::Shutdown)
        .await;

    // Sync phase. `registry_write` is held continuously from the name check
    // through the surgery, which is what makes check-then-write atomic against
    // a concurrent register/rename in the target — chosen over widening the
    // maintenance flag to roster mutations. No `.await` below this line.
    let _write = lock(&state.registry_write);
    let canonical = canonicalize_for_uniqueness(&agent.name);
    if let Some(existing) = target
        .list_agents()?
        .iter()
        .find(|a| canonicalize_for_uniqueness(&a.name) == canonical)
    {
        return Err(CoreError::DuplicateAgentName {
            name: agent.name.clone(),
            existing: existing.name.clone(),
        }
        .into());
    }
    let intent = MoveIntent {
        agent_id,
        source_project: source.id,
        target_project: target.id,
    };
    // A failed intent write drops `admission`, whose `Drop` un-marks both
    // projects — the by-value hand-off below is what makes that automatic, so
    // do not change it to borrow.
    let intent_path = state.store.write_move_intent(&intent)?;
    let barrier = admission.into_recovery_barrier();

    let first_attempt = apply_move_steps(&state.store, &intent, home_dir, None);
    let outcome = match first_attempt {
        Ok(adopted) => Ok(adopted),
        Err(first) => {
            // The intent record is not crash-only: an in-app failure triggers
            // immediate recovery while both projects stay blocked, under the
            // same `registry_write` hold. One re-drive — every step is
            // idempotent, so a transient failure heals and a persistent one
            // fails again here.
            tracing::warn!(error = %first, "agent move failed mid-surgery — recovering in place");
            apply_move_steps(&state.store, &intent, home_dir, None)
        }
    };
    match outcome {
        Ok(Some(adopted)) => {
            lock(&state.agents_by_id).insert(agent_id, adopted.clone());
            remove_intent_file(&intent_path);
            barrier.release();
            Ok(adopted)
        }
        // `stop_after` was `None`, so the surgery always reaches adoption.
        Ok(None) => unreachable!("a full surgery pass always yields the adopted record"),
        Err(second) => {
            // Recovery failed too: exactly these two projects stay blocked
            // (dropping `barrier` keeps the marks — it has no auto-release),
            // with the refusal upgraded to name the intent file. Everything
            // else keeps working; the next launch retries.
            tracing::error!(error = %second, intent = %intent_path.display(),
                "agent move recovery failed — blocking both projects until repaired");
            let mut repairs = lock(&state.move_repairs);
            repairs.insert(source.id, intent_path.clone());
            repairs.insert(target.id, intent_path.clone());
            drop(barrier);
            Err(AppError::MoveRepairRequired {
                project_id: source.id,
                intent: intent_path,
            })
        }
    }
}

/// Drain every intent file in the store's `moves/` directory, completing
/// interrupted moves before any project can open. Call during startup, before
/// the first command can run; on a fresh launch nothing is open, so recovery
/// cannot race a user action.
pub(crate) fn recover_pending_moves_at_startup(state: &AppState, home_dir: &Path) {
    let intents = match state.store.list_move_intents() {
        Ok(intents) => intents,
        Err(e) => {
            tracing::error!(error = %e, "could not read pending move intents — recovery skipped");
            return;
        }
    };
    for (path, intent) in intents {
        // Same cross-process rule as the move itself: both projects'
        // `instance.lock`s before any file is touched. Held elsewhere → defer
        // without touching files; the pair stays blocked locally and the next
        // launch retries. Roots are store-relative, so no directory resolution
        // is needed to lock.
        let locks: Result<Vec<_>, _> = [intent.source_project, intent.target_project]
            .into_iter()
            .map(|id| acquire_project_lock(id, &state.store.project_root(id)))
            .collect();
        let outcome = locks
            .and_then(|_held| apply_move_steps(&state.store, &intent, home_dir, None).map(|_| ()));
        match outcome {
            Ok(()) => remove_intent_file(&path),
            Err(e) => {
                tracing::error!(error = %e, intent = %path.display(),
                    "move recovery failed at startup — blocking both projects until repaired");
                let mut marks = lock(&state.maintenance);
                marks.insert(intent.source_project);
                marks.insert(intent.target_project);
                drop(marks);
                let mut repairs = lock(&state.move_repairs);
                repairs.insert(intent.source_project, path.clone());
                repairs.insert(intent.target_project, path.clone());
            }
        }
    }
}

/// Best-effort: the marks are already released by the time this runs, and a
/// leftover intent is re-driven as a no-op at next launch, which then deletes
/// it. Never worth failing a completed move over.
fn remove_intent_file(path: &Path) {
    if let Err(e) = std::fs::remove_file(path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), error = %e, "completed move's intent file not deleted");
    }
}

/// The surgery: steps (a)–(g) in commit order — appends before rewrites,
/// target before source. Every step is a no-op when already applied, so
/// recovery re-drives the whole sequence from any interruption point. Returns
/// the adopted record (always `Some` when `stop_after` is `None`).
///
/// The caller owns the barrier, the `registry_write` hold, and both projects'
/// `instance.lock`s; this function only touches files.
pub(crate) fn apply_move_steps(
    store: &Store,
    intent: &MoveIntent,
    home_dir: &Path,
    stop_after: Option<MoveStep>,
) -> Result<Option<AgentRecord>, AppError> {
    let source = store.open_project(intent.source_project)?;
    let target = store.open_project(intent.target_project)?;
    let agent_id = intent.agent_id;
    let stop = |step: MoveStep| stop_after == Some(step);

    // (a) Append the moved agent's records to the target journal. Re-reads and
    // dedups by (variant, turn_id), so a re-drive appends only what is missing.
    let source_records = journal::read_records(&source.journal_path())?;
    let (moved, remaining) = journal::partition_for_agent(source_records, agent_id);
    journal::append_missing(&target.journal_path(), &moved)?;
    if stop(MoveStep::TargetJournal) {
        return Ok(None);
    }

    // (b) Copy metadata sidecars. The durable copy leaves either a finalized
    // destination or none (a crash mid-copy leaves only the temp file), which
    // is the sole reason "destination exists" is a sound skip predicate here —
    // and the skip is required, not an optimization: after step (g) the source
    // copy is gone, and a re-drive must not fail on it.
    for (src, dst) in sidecar_pairs(&source, &target, agent_id) {
        // `is_file`, not `exists`: only a finalized regular file counts as
        // copied. Anything else at the path is not a copy we made.
        if !dst.is_file() && src.is_file() {
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent).map_err(|e| AppError::MoveIo {
                    path: parent.to_owned(),
                    source: e,
                })?;
            }
            copy_file_durable(&src, &dst)?;
        }
    }
    if stop(MoveStep::Sidecars) {
        return Ok(None);
    }

    // (c) Adopt the registry row. Pre-commit the record comes from the source
    // registry; post-commit it is already in the target, and adoption's
    // same-id no-op returns it. Neither having it means the agent was lost to
    // something recovery cannot explain — surface it, never pick a winner.
    let source_record = source.list_agents()?.into_iter().find(|a| a.id == agent_id);
    let adopted = match source_record {
        Some(record) => {
            let proposal = session_home_proposal(store, &source, &target, &record, home_dir)?;
            target.adopt_agent(&record, proposal)?
        }
        None => target
            .list_agents()?
            .into_iter()
            .find(|a| a.id == agent_id)
            .ok_or(AppError::AgentNotFound(agent_id))?,
    };
    if stop(MoveStep::TargetRegistry) {
        return Ok(Some(adopted));
    }

    // (d) Rewrite the source registry without the row — the commit point.
    // `remove_agent` returns false when already absent, which is the re-drive.
    source.remove_agent(agent_id)?;
    if stop(MoveStep::SourceRegistry) {
        return Ok(Some(adopted));
    }

    // (e) Rewrite the source journal without the moved records. Rewriting to a
    // set that already excludes them is naturally idempotent.
    journal::rewrite_records(&source.journal_path(), &remaining)?;
    if stop(MoveStep::SourceJournal) {
        return Ok(Some(adopted));
    }

    // (f) Partition pins. Send-id ownership is derived by *agent* filters, not
    // file state, so the same computation is correct on every re-drive
    // regardless of which journal rewrite has landed.
    partition_and_move_pins(&source, &target, agent_id)?;
    if stop(MoveStep::Pins) {
        return Ok(Some(adopted));
    }

    // (g) Remove the source sidecars. Missing is the re-drive.
    for (src, _dst) in sidecar_pairs(&source, &target, agent_id) {
        if let Err(e) = std::fs::remove_file(&src)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %src.display(), error = %e, "moved agent's source sidecar not deleted");
        }
    }
    Ok(Some(adopted))
}

/// `(source, target)` paths for both per-agent sidecars.
fn sidecar_pairs(source: &Project, target: &Project, agent_id: AgentId) -> [(PathBuf, PathBuf); 2] {
    [
        (
            switchboard_harness::meta_sidecar::meta_sidecar_path(&source.root, agent_id),
            switchboard_harness::meta_sidecar::meta_sidecar_path(&target.root, agent_id),
        ),
        (
            switchboard_harness::turnmeta_sidecar::turnmeta_sidecar_path(&source.root, agent_id),
            switchboard_harness::turnmeta_sidecar::turnmeta_sidecar_path(&target.root, agent_id),
        ),
    ]
}

/// The `session_home` to propose for the adopted record.
///
/// The value is the source's **effective** session directory —
/// `record.session_home ?? source project directory` — never the source
/// project's directory unqualified: for an agent being moved a second time the
/// transcript is still under the directory the first move recorded.
/// (`adopt_agent` refuses a contradicting proposal independently; that backstop
/// surfaces a miscomputation here, it does not license one.)
///
/// Proposed only when the move crosses working directories, the agent is
/// Claude (the one harness that namespaces sessions by directory), and a
/// session file actually exists under that effective directory — checked with
/// the no-recanonicalize lookup, because the flagship case is moving an agent
/// out of a worktree the user has already pruned. A never-dispatched agent or
/// an unmaterialized fork has no file, gets no home, and correctly starts or
/// materializes its session native to the target.
///
/// Deterministic across recovery re-drives: both projects are gated, so the
/// directory identities and file existence this reads cannot change between
/// attempts.
fn session_home_proposal(
    store: &Store,
    source: &Project,
    target: &Project,
    record: &AgentRecord,
    home_dir: &Path,
) -> Result<Option<PathBuf>, AppError> {
    if record.harness != HarnessKind::ClaudeCode {
        return Ok(None);
    }
    let entries = store.list_projects()?;
    let directory_of = |id: ProjectId| {
        entries
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.directory_id)
            .ok_or(CoreError::ProjectNotFound(id))
    };
    if directory_of(source.id)? == directory_of(target.id)? {
        return Ok(None);
    }
    let has_session = resolve_session_file(record, &source.directory, home_dir).is_some();
    Ok(has_session.then(|| {
        record
            .effective_session_directory(&source.directory)
            .to_owned()
    }))
}

/// Move the agent's pins from the source's `pins.jsonl` to the target's:
/// agent-keyed pins whose id matches, and `user:send` pins whose send the
/// moved agent was the **sole** recipient of. Everything else — other agents'
/// pins, fan-out user pins with remaining recipients, keys that don't parse —
/// stays exactly where it is. **Pins are never deleted**: an unrecognized key
/// is retained, and nothing here prunes.
///
/// Two deliberate asymmetries, recorded so nobody "fixes" them: a fan-out
/// user pin stays in the source even though the send's records now span two
/// journals (each project renders its own copy grouped by `send_id`, so the
/// pinned message still renders there); and a fan-out send that included the
/// moved agent lands in *both* journals by design — cross-journal dedup is
/// not wanted.
fn partition_and_move_pins(
    source: &Project,
    target: &Project,
    agent_id: AgentId,
) -> Result<(), AppError> {
    let source_pins = pins::read_pins(&source.pins_path())?;
    if source_pins.is_empty() {
        return Ok(());
    }
    // Send ownership by agent filter, not by file state: the moved agent's
    // sends are read from the *target* journal (step (a) put them there on
    // every drive), the other agents' from the source — so the answer is the
    // same before and after step (e) rewrites the source.
    let moved_sends: HashSet<String> = journal::read_records(&target.journal_path())?
        .iter()
        .filter(|r| r.agent_id() == agent_id)
        .filter_map(send_id_of)
        .collect();
    let remaining_sends: HashSet<String> = journal::read_records(&source.journal_path())?
        .iter()
        .filter(|r| r.agent_id() != agent_id)
        .filter_map(send_id_of)
        .collect();

    let (moving, staying): (Vec<_>, Vec<_>) = source_pins.into_iter().partition(|pin| {
        if agent_id_for_pin_key(&pin.key) == Some(agent_id) {
            return true;
        }
        match user_send_id_of_pin_key(&pin.key) {
            Some(send_id) => moved_sends.contains(&send_id) && !remaining_sends.contains(&send_id),
            None => false,
        }
    });
    if moving.is_empty() {
        return Ok(());
    }
    let mut target_pins = pins::read_pins(&target.pins_path())?;
    let present: HashSet<String> = target_pins.iter().map(|p| p.key.clone()).collect();
    let mut appended = false;
    for pin in moving {
        if !present.contains(&pin.key) {
            target_pins.push(pin);
            appended = true;
        }
    }
    if appended {
        pins::write_pins(&target.pins_path(), &target_pins)?;
    }
    pins::write_pins(&source.pins_path(), &staying)?;
    Ok(())
}

fn send_id_of(record: &JournalRecord) -> Option<String> {
    match record {
        JournalRecord::Send { send_id, .. }
        | JournalRecord::Outcome { send_id, .. }
        | JournalRecord::TurnLink { send_id, .. } => Some(send_id.to_string()),
        // `JournalRecord` is #[non_exhaustive]: a future variant defines its own
        // send affiliation (or none) rather than inheriting one here.
        _ => None,
    }
}

/// The agent a pin belongs to, mirroring `messageIdentity.ts`'s
/// `agentIdForMessageKey` exactly — **both** key shapes. The canonical shape
/// for any hydrated turn (essentially every durable pin) is
/// `agent:hydration:<agent_id>:<hydration_key>`, agent id in position 2; the
/// temporary/migration alias is `agent:send:<send_id>:<agent_id>`, position 3.
/// Matching only the alias shape would classify real pins "unrecognized" and
/// silently leave them all behind. `None` for anything else — including keys
/// that don't parse, which callers must retain untouched.
pub(crate) fn agent_id_for_pin_key(key: &str) -> Option<AgentId> {
    let parts: Vec<&str> = key.split(':').collect();
    if parts.len() != 4 || parts[0] != "agent" {
        return None;
    }
    let raw = match parts[1] {
        "hydration" => parts[2],
        "send" => parts[3],
        _ => return None,
    };
    percent_decode(raw).and_then(|s| s.parse().ok())
}

/// The send id of a `user:send:<send_id>` pin key, `None` for any other shape.
fn user_send_id_of_pin_key(key: &str) -> Option<String> {
    let parts: Vec<&str> = key.split(':').collect();
    if parts.len() == 3 && parts[0] == "user" && parts[1] == "send" {
        return percent_decode(parts[2]);
    }
    None
}

/// Minimal `decodeURIComponent` counterpart for pin-key components. Uuids
/// survive `encodeURIComponent` unchanged, so this exists for exactness with
/// the frontend encoder rather than for a case observed in practice.
fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            let value = u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
            out.push(value);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;

    use chrono::Utc;
    use switchboard_core::{AgentProfiles, MessagePin, SessionLocator, append_jsonl};
    use switchboard_dispatcher::{EventEmitter, RecordingEmitter};
    use switchboard_harness::{HarnessAdapter, MockHarnessAdapter};
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::*;
    use crate::state::AppState;

    pub(crate) fn state_at(root: &Path) -> AppState {
        let mock: Arc<dyn HarnessAdapter> = Arc::new(MockHarnessAdapter::new());
        let emitter = Arc::new(RecordingEmitter::new());
        AppState::new_for_test_at(
            root,
            Arc::clone(&mock),
            Arc::clone(&mock),
            mock,
            emitter as Arc<dyn EventEmitter>,
        )
    }

    pub(crate) fn claude_record(project_id: ProjectId, name: &str, session: Uuid) -> AgentRecord {
        AgentRecord {
            id: Uuid::now_v7(),
            project_id,
            name: name.to_owned(),
            harness: HarnessKind::ClaudeCode,
            session_locator: Some(SessionLocator::Uuid(session)),
            model: None,
            effort: None,
            profiles: AgentProfiles::default(),
            forked_from_session: None,
            forked_from_session_home: None,
            session_home: None,
            created_at: Utc::now(),
        }
    }

    pub(crate) fn send(
        agent: &AgentRecord,
        send_id: Uuid,
        turn_id: Uuid,
        prompt: &str,
    ) -> JournalRecord {
        JournalRecord::Send {
            send_id,
            turn_id,
            agent_id: agent.id,
            prompt: prompt.to_owned(),
            attachments: Vec::new(),
            at: Utc::now(),
        }
    }

    /// Everything a move touches, seeded deterministically so the post-move
    /// state can be asserted exactly rather than compared to a control run.
    pub(crate) struct Fixture {
        pub(crate) source: Project,
        pub(crate) target: Project,
        pub(crate) mover: AgentRecord,
        pub(crate) bystander: AgentRecord,
        /// The mover's records, in source-journal order.
        pub(crate) mover_records: Vec<JournalRecord>,
        /// The bystander's records, in source-journal order.
        pub(crate) bystander_records: Vec<JournalRecord>,
        /// Pins expected to move (mover's hydration pin, sole-recipient user pin).
        pub(crate) moving_pins: Vec<MessagePin>,
        /// Pins expected to stay (bystander's, fan-out user pin, unparseable).
        pub(crate) staying_pins: Vec<MessagePin>,
        pub(crate) sidecar_meta: Vec<u8>,
        pub(crate) intent: MoveIntent,
        pub(crate) claude_home: TempDir,
    }

    /// Two projects in two different working directories, a mover with a real
    /// Claude session file (so the move stamps a session home), a bystander
    /// whose records must survive untouched, pins of every partition class, and
    /// a metadata sidecar.
    #[allow(clippy::too_many_lines)] // A fixture enumerating every artifact a move touches.
    pub(crate) fn seeded(state: &AppState, dirs: &(TempDir, TempDir)) -> Fixture {
        let source_dir = state.store.add_directory(dirs.0.path()).unwrap();
        let target_dir = state.store.add_directory(dirs.1.path()).unwrap();
        let source = state
            .store
            .create_project(source_dir.directory_id, "source")
            .unwrap();
        let target = state
            .store
            .create_project(target_dir.directory_id, "target")
            .unwrap();

        let session = Uuid::now_v7();
        let mover = claude_record(source.id, "mover", session);
        let bystander = claude_record(source.id, "bystander", Uuid::now_v7());
        append_jsonl(&source.registry_path, &mover).unwrap();
        append_jsonl(&source.registry_path, &bystander).unwrap();

        // A real transcript under the source directory, so the session-home
        // decision sees a materialized Claude session.
        let claude_home = TempDir::new().unwrap();
        let transcript = switchboard_harness::claude_session_file_path(
            claude_home.path(),
            &source.directory,
            &session,
        );
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(&transcript, "{}\n").unwrap();

        // Journal: interleaved mover/bystander records, one fan-out send shared
        // by both, one attachment-bearing send for the path-untouched assertion.
        let solo_send = Uuid::now_v7();
        let fanout_send = Uuid::now_v7();
        let attachment_send = Uuid::now_v7();
        let attach_record = JournalRecord::Send {
            send_id: attachment_send,
            turn_id: Uuid::now_v7(),
            agent_id: mover.id,
            prompt: "look at this".to_owned(),
            attachments: vec![switchboard_core::Attachment {
                label: "image-1".to_owned(),
                kind: switchboard_core::AttachmentKind::Image,
                path: "/store/projects/src/attachments/u__x.png".to_owned(),
                dispatched_path: None,
                original_name: "x.png".to_owned(),
            }],
            at: Utc::now(),
        };
        let mover_records = vec![
            send(&mover, solo_send, Uuid::now_v7(), "hello"),
            send(&mover, fanout_send, Uuid::now_v7(), "to both"),
            attach_record,
            JournalRecord::TurnLink {
                send_id: solo_send,
                turn_id: Uuid::now_v7(),
                agent_id: mover.id,
                hydration_key: "msg_mover_1".to_owned(),
                at: Utc::now(),
            },
        ];
        let bystander_records = vec![
            send(&bystander, fanout_send, Uuid::now_v7(), "to both"),
            send(&bystander, Uuid::now_v7(), Uuid::now_v7(), "unrelated"),
        ];
        // Interleave to prove order within each half survives partition.
        for r in [
            &mover_records[0],
            &bystander_records[0],
            &mover_records[1],
            &mover_records[2],
            &bystander_records[1],
            &mover_records[3],
        ] {
            journal::append_record(&source.journal_path(), r).unwrap();
        }

        let moving_pins = vec![
            MessagePin {
                key: format!("agent:hydration:{}:msg_mover_1", mover.id),
                pinned_at: Utc::now(),
            },
            MessagePin {
                key: format!("agent:send:{solo_send}:{}", mover.id),
                pinned_at: Utc::now(),
            },
            // Sole recipient: only the mover sent under this send id.
            MessagePin {
                key: format!("user:send:{solo_send}"),
                pinned_at: Utc::now(),
            },
        ];
        let staying_pins = vec![
            MessagePin {
                key: format!("agent:hydration:{}:msg_by_1", bystander.id),
                pinned_at: Utc::now(),
            },
            // Fan-out: the bystander also sent under this id, so the user's
            // message legitimately still renders in the source.
            MessagePin {
                key: format!("user:send:{fanout_send}"),
                pinned_at: Utc::now(),
            },
            MessagePin {
                key: "something:unrecognized".to_owned(),
                pinned_at: Utc::now(),
            },
        ];
        let mut all_pins = moving_pins.clone();
        all_pins.extend(staying_pins.clone());
        pins::write_pins(&source.pins_path(), &all_pins).unwrap();

        let sidecar_meta = b"{\"meta\":true}".to_vec();
        let meta = switchboard_harness::meta_sidecar::meta_sidecar_path(&source.root, mover.id);
        std::fs::create_dir_all(meta.parent().unwrap()).unwrap();
        std::fs::write(&meta, &sidecar_meta).unwrap();

        let intent = MoveIntent {
            agent_id: mover.id,
            source_project: source.id,
            target_project: target.id,
        };
        Fixture {
            source,
            target,
            mover,
            bystander,
            mover_records,
            bystander_records,
            moving_pins,
            staying_pins,
            sidecar_meta,
            intent,
            claude_home,
        }
    }

    /// The exact end state an uninterrupted move produces. Asserting it after
    /// recovery from any interruption point is the "equivalent to an
    /// uninterrupted move, nothing duplicated" contract, stated absolutely
    /// rather than via a control run.
    pub(crate) fn assert_fully_moved(f: &Fixture) {
        let target_journal = journal::read_records(&f.target.journal_path()).unwrap();
        assert_eq!(
            target_journal, f.mover_records,
            "target journal holds exactly the mover's records, in order"
        );
        let source_journal = journal::read_records(&f.source.journal_path()).unwrap();
        assert_eq!(
            source_journal, f.bystander_records,
            "source journal holds exactly the bystander's records, in order"
        );
        // Attachment paths ride along byte-identical — the move must not touch
        // them (send↔turn correlation reconstructs the dispatched text).
        match &target_journal[2] {
            JournalRecord::Send { attachments, .. } => {
                assert_eq!(
                    attachments[0].path,
                    "/store/projects/src/attachments/u__x.png"
                );
                assert_eq!(attachments[0].dispatched_path, None);
            }
            other => panic!("expected the attachment send, got {other:?}"),
        }

        let target_agents = f.target.list_agents().unwrap();
        assert_eq!(target_agents.len(), 1, "no duplicated registry row");
        let adopted = &target_agents[0];
        assert_eq!(adopted.id, f.mover.id);
        assert_eq!(adopted.project_id, f.target.id);
        assert_eq!(adopted.session_locator, f.mover.session_locator);
        assert_eq!(
            adopted.session_home.as_deref(),
            Some(f.source.directory.as_path()),
            "a cross-directory move stamps the source's effective session directory"
        );
        let source_agents = f.source.list_agents().unwrap();
        assert_eq!(source_agents.len(), 1);
        assert_eq!(source_agents[0].id, f.bystander.id);

        let source_pins = pins::read_pins(&f.source.pins_path()).unwrap();
        assert_eq!(
            source_pins, f.staying_pins,
            "no pin deleted, none duplicated"
        );
        let target_pins = pins::read_pins(&f.target.pins_path()).unwrap();
        assert_eq!(target_pins, f.moving_pins);

        let src_meta =
            switchboard_harness::meta_sidecar::meta_sidecar_path(&f.source.root, f.mover.id);
        let dst_meta =
            switchboard_harness::meta_sidecar::meta_sidecar_path(&f.target.root, f.mover.id);
        assert!(!src_meta.exists(), "source sidecar removed");
        assert_eq!(std::fs::read(&dst_meta).unwrap(), f.sidecar_meta);
    }

    const ALL_STOPS: [MoveStep; 6] = [
        MoveStep::TargetJournal,
        MoveStep::Sidecars,
        MoveStep::TargetRegistry,
        MoveStep::SourceRegistry,
        MoveStep::SourceJournal,
        MoveStep::Pins,
    ];

    /// Crash after every step, recovered **in-process** (the in-app failure
    /// path re-drives the same function under the same hold).
    #[test]
    fn recovery_completes_the_move_from_every_interruption_point() {
        for stop in ALL_STOPS {
            let root = TempDir::new().unwrap();
            let state = state_at(root.path());
            let dirs = (TempDir::new().unwrap(), TempDir::new().unwrap());
            let f = seeded(&state, &dirs);
            state.store.write_move_intent(&f.intent).unwrap();

            apply_move_steps(&state.store, &f.intent, f.claude_home.path(), Some(stop))
                .unwrap_or_else(|e| panic!("surgery to {stop:?} failed: {e}"));
            apply_move_steps(&state.store, &f.intent, f.claude_home.path(), None)
                .unwrap_or_else(|e| panic!("recovery after {stop:?} failed: {e}"));

            assert_fully_moved(&f);
        }
    }

    /// Crash after every step, recovered at **startup** by a fresh process view
    /// draining `moves/` — the restart path, including intent-file cleanup.
    #[test]
    fn startup_recovery_completes_the_move_from_every_interruption_point() {
        for stop in ALL_STOPS {
            let root = TempDir::new().unwrap();
            let dirs = (TempDir::new().unwrap(), TempDir::new().unwrap());
            let f;
            {
                let state = state_at(root.path());
                f = seeded(&state, &dirs);
                let _intent_path = state.store.write_move_intent(&f.intent).unwrap();
                apply_move_steps(&state.store, &f.intent, f.claude_home.path(), Some(stop))
                    .unwrap_or_else(|e| panic!("surgery to {stop:?} failed: {e}"));
                // state dropped here: the crash. Its instance locks (none were
                // taken — apply operates on files) and store handle go away.
            }
            let restarted = state_at(root.path());
            recover_pending_moves_at_startup(&restarted, f.claude_home.path());

            assert_fully_moved(&f);
            assert!(
                restarted.store.list_move_intents().unwrap().is_empty(),
                "a recovered move's intent file is deleted"
            );
            assert!(
                lock(&restarted.maintenance).is_empty(),
                "successful recovery leaves nothing blocked"
            );
        }
    }

    /// A second pass over an already-complete move changes nothing — the
    /// "crash after step (g), before the intent delete" shape.
    #[test]
    fn recovery_over_a_completed_move_is_a_no_op() {
        let root = TempDir::new().unwrap();
        let state = state_at(root.path());
        let dirs = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let f = seeded(&state, &dirs);
        state.store.write_move_intent(&f.intent).unwrap();

        apply_move_steps(&state.store, &f.intent, f.claude_home.path(), None).unwrap();
        assert_fully_moved(&f);

        let restarted = state_at(root.path());
        recover_pending_moves_at_startup(&restarted, f.claude_home.path());
        assert_fully_moved(&f);
        assert!(restarted.store.list_move_intents().unwrap().is_empty());
    }

    /// The intra-step sidecar injection: a crash between the temp write and the
    /// rename leaves only the temp file — recovery re-copies and the content
    /// matches. A crash after the rename is the finalized-destination no-op.
    #[test]
    fn sidecar_recovery_ignores_a_stale_temp_and_keeps_a_finalized_copy() {
        let root = TempDir::new().unwrap();
        let state = state_at(root.path());
        let dirs = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let f = seeded(&state, &dirs);
        state.store.write_move_intent(&f.intent).unwrap();

        // Crash between temp-write and rename: a truncated temp, no dst.
        let dst = switchboard_harness::meta_sidecar::meta_sidecar_path(&f.target.root, f.mover.id);
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        let tmp = dst.with_extension("json.tmp");
        std::fs::write(&tmp, b"{\"trunc").unwrap();

        apply_move_steps(&state.store, &f.intent, f.claude_home.path(), None).unwrap();
        assert_fully_moved(&f);

        // Crash after the rename: dst finalized; a re-drive must not disturb it.
        apply_move_steps(&state.store, &f.intent, f.claude_home.path(), None).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), f.sidecar_meta);
    }

    /// Two interrupted moves across different project pairs: both intents are
    /// drained at startup.
    #[test]
    fn startup_recovery_drains_every_intent_file() {
        let root = TempDir::new().unwrap();
        let dirs_a = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let dirs_b = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let (fa, fb);
        {
            let state = state_at(root.path());
            fa = seeded(&state, &dirs_a);
            fb = seeded(&state, &dirs_b);
            state.store.write_move_intent(&fa.intent).unwrap();
            state.store.write_move_intent(&fb.intent).unwrap();
            apply_move_steps(
                &state.store,
                &fa.intent,
                fa.claude_home.path(),
                Some(MoveStep::Sidecars),
            )
            .unwrap();
            apply_move_steps(
                &state.store,
                &fb.intent,
                fb.claude_home.path(),
                Some(MoveStep::SourceRegistry),
            )
            .unwrap();
        }
        let restarted = state_at(root.path());
        // One home dir per fixture would be more faithful, but the session-home
        // decision for fixture B resolves under B's directories, which A's home
        // does not contain — so run recovery once per home to mirror what a
        // single real $HOME would hold. Assert both completed either way.
        recover_pending_moves_at_startup(&restarted, fa.claude_home.path());
        recover_pending_moves_at_startup(&restarted, fb.claude_home.path());

        assert!(restarted.store.list_move_intents().unwrap().is_empty());
    }

    /// Recovery defers — touching nothing — while another process holds either
    /// project's instance lock, leaving the pair blocked for the next launch.
    #[test]
    fn startup_recovery_defers_when_a_project_is_locked_elsewhere() {
        let root = TempDir::new().unwrap();
        let state = state_at(root.path());
        let dirs = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let f = seeded(&state, &dirs);
        state.store.write_move_intent(&f.intent).unwrap();
        apply_move_steps(
            &state.store,
            &f.intent,
            f.claude_home.path(),
            Some(MoveStep::TargetJournal),
        )
        .unwrap();
        let journal_before = std::fs::read(f.source.journal_path()).unwrap();

        // "Another process": an independent flock on the target's root.
        let other = acquire_project_lock(f.target.id, &state.store.project_root(f.target.id))
            .expect("first lock");

        let restarted = state_at(root.path());
        recover_pending_moves_at_startup(&restarted, f.claude_home.path());

        assert_eq!(
            std::fs::read(f.source.journal_path()).unwrap(),
            journal_before,
            "deferred recovery must not touch files"
        );
        assert_eq!(restarted.store.list_move_intents().unwrap().len(), 1);
        assert!(lock(&restarted.maintenance).contains(&f.source.id));
        assert!(lock(&restarted.maintenance).contains(&f.target.id));
        assert!(lock(&restarted.move_repairs).contains_key(&f.source.id));
        drop(other);
    }

    /// A recovery that fails blocks exactly the two named projects with the
    /// repair error; other projects open normally, and the refusal names the
    /// intent file.
    #[test]
    fn failed_startup_recovery_blocks_exactly_the_named_pair() {
        let root = TempDir::new().unwrap();
        let dirs = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let f;
        let unrelated;
        {
            let state = state_at(root.path());
            f = seeded(&state, &dirs);
            let extra_dir = TempDir::new().unwrap();
            let d = state.store.add_directory(extra_dir.path()).unwrap();
            unrelated = state
                .store
                .create_project(d.directory_id, "unrelated")
                .unwrap();
            state.store.write_move_intent(&f.intent).unwrap();
            // Poison the target: a different record already occupies the
            // mover's id, which adoption refuses — persistently.
            let impostor = AgentRecord {
                project_id: f.target.id,
                name: "impostor".to_owned(),
                ..claude_record(f.target.id, "impostor", Uuid::now_v7())
            };
            let impostor = AgentRecord {
                id: f.mover.id,
                ..impostor
            };
            append_jsonl(&f.target.registry_path, &impostor).unwrap();
        }
        let restarted = state_at(root.path());
        recover_pending_moves_at_startup(&restarted, f.claude_home.path());

        assert!(lock(&restarted.maintenance).contains(&f.source.id));
        assert!(lock(&restarted.maintenance).contains(&f.target.id));
        assert!(!lock(&restarted.maintenance).contains(&unrelated.id));
        let err = crate::commands::reject_if_under_maintenance(&restarted, f.source.id)
            .expect_err("a blocked project must refuse");
        assert!(
            matches!(err, AppError::MoveRepairRequired { .. }),
            "the refusal names the repair, got {err:?}"
        );
        assert!(
            restarted.store.open_project(unrelated.id).is_ok(),
            "unrelated projects stay usable"
        );
    }

    // ---- pin-key parsing (mirrors messageIdentity.ts) ----

    #[test]
    fn pin_key_parsing_matches_both_agent_shapes_and_rejects_the_rest() {
        let id = Uuid::now_v7();
        assert_eq!(
            agent_id_for_pin_key(&format!("agent:hydration:{id}:msg_01")),
            Some(id),
            "canonical shape: agent id in position 2"
        );
        assert_eq!(
            agent_id_for_pin_key(&format!("agent:send:{}:{id}", Uuid::now_v7())),
            Some(id),
            "alias shape: agent id in position 3"
        );
        assert_eq!(agent_id_for_pin_key(&format!("user:send:{id}")), None);
        assert_eq!(agent_id_for_pin_key("agent:mystery:a:b"), None);
        assert_eq!(agent_id_for_pin_key("not a key"), None);
        assert_eq!(agent_id_for_pin_key("agent:hydration:not-a-uuid:k"), None);
    }
}

#[cfg(test)]
mod semantics_tests {
    use std::sync::Arc;

    use chrono::Utc;
    use switchboard_core::{AgentProfiles, SessionLocator, append_jsonl};
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::tests::*;
    use super::*;
    use crate::state::AppState;

    /// The actor-lifecycle regression: after the move, the very next send must
    /// journal into the **target** — the pre-move actor captured the source
    /// project at creation, and only its teardown makes the replacement pick
    /// the target up.
    #[tokio::test]
    async fn a_full_move_relocates_the_agent_and_the_next_send_journals_into_the_target() {
        let root = TempDir::new().unwrap();
        let state = state_at(root.path());
        let dirs = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let f = seeded(&state, &dirs);
        activate_project(&state, f.source.clone()).unwrap();

        // A pre-move send, so the mover's actor exists and has the source
        // project baked into its factory.
        let pre = crate::commands::send_message_impl(
            &state,
            f.mover.id,
            "before the move",
            Vec::new(),
            Uuid::now_v7(),
            f.claude_home.path(),
        )
        .await;
        assert!(pre.is_ok(), "pre-move send failed: {pre:?}");
        wait_for_idle(&state, f.mover.id).await;

        let adopted = move_agent_impl(&state, f.mover.id, f.target.id, f.claude_home.path())
            .await
            .expect("move should succeed");

        assert_eq!(adopted.project_id, f.target.id);
        assert_eq!(
            lock(&state.agents_by_id)
                .get(&f.mover.id)
                .unwrap()
                .project_id,
            f.target.id,
            "the cache serves the adopted record"
        );
        assert!(lock(&state.maintenance).is_empty(), "both projects resume");
        assert!(state.store.list_move_intents().unwrap().is_empty());

        let target_before = journal::read_records(&f.target.journal_path())
            .unwrap()
            .len();
        let sent = crate::commands::send_message_impl(
            &state,
            f.mover.id,
            "after the move",
            Vec::new(),
            Uuid::now_v7(),
            f.claude_home.path(),
        )
        .await;
        assert!(sent.is_ok(), "post-move send failed: {sent:?}");
        wait_for_idle(&state, f.mover.id).await;

        let target_after = journal::read_records(&f.target.journal_path()).unwrap();
        assert!(
            target_after.len() > target_before
                && target_after
                    .iter()
                    .any(|r| matches!(r, JournalRecord::Send { prompt, .. } if prompt == "after the move")),
            "the post-move turn must journal into the target"
        );
        let source_journal = journal::read_records(&f.source.journal_path()).unwrap();
        assert!(
            !source_journal.iter().any(
                |r| matches!(r, JournalRecord::Send { prompt, .. } if prompt == "after the move")
            ),
            "nothing lands in the source after the move"
        );
    }

    async fn wait_for_idle(state: &AppState, agent_id: AgentId) {
        for _ in 0..200 {
            if !state.dispatcher.has_pending_work(agent_id).await {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("agent never went idle");
    }

    #[tokio::test]
    async fn a_move_is_refused_while_a_workflow_run_is_active_and_leaves_no_trace() {
        let root = TempDir::new().unwrap();
        let state = state_at(root.path());
        let dirs = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let f = seeded(&state, &dirs);
        activate_project(&state, f.source.clone()).unwrap();
        lock(&state.workflow_runs).insert(
            Uuid::now_v7(),
            crate::state::ActiveRun {
                cancel: tokio_util::sync::CancellationToken::new(),
                project_id: f.source.id,
                workflow: "nightly".to_owned(),
                snapshot: crate::state::RunSnapshot {
                    total_steps: 1,
                    current_step: 0,
                },
                steps: Vec::new(),
                done: Arc::new(tokio::sync::Notify::new()),
            },
        );

        let err = move_agent_impl(&state, f.mover.id, f.target.id, f.claude_home.path())
            .await
            .expect_err("an active run must refuse the move");

        assert!(
            matches!(err, AppError::ProjectNotQuiescent { .. }),
            "got {err:?}"
        );
        assert!(
            lock(&state.maintenance).is_empty(),
            "a pre-surgery refusal blocks nothing"
        );
        assert!(
            state.store.list_move_intents().unwrap().is_empty(),
            "no intent is written for a refused move"
        );
        assert_eq!(f.source.list_agents().unwrap().len(), 2, "roster untouched");
    }

    #[tokio::test]
    async fn a_second_move_is_refused_while_one_is_in_progress() {
        let root = TempDir::new().unwrap();
        let state = state_at(root.path());
        let dirs = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let f = seeded(&state, &dirs);
        activate_project(&state, f.source.clone()).unwrap();

        let _held = state.move_mutex.try_lock().unwrap();
        let err = move_agent_impl(&state, f.mover.id, f.target.id, f.claude_home.path())
            .await
            .expect_err("the mutex must refuse a concurrent move");
        assert!(matches!(err, AppError::MoveInProgress), "got {err:?}");
    }

    #[tokio::test]
    async fn a_move_into_a_name_collision_is_refused_with_both_rosters_unchanged() {
        let root = TempDir::new().unwrap();
        let state = state_at(root.path());
        let dirs = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let f = seeded(&state, &dirs);
        activate_project(&state, f.source.clone()).unwrap();
        // Canonical collision: hyphen/underscore and case fold together.
        let resident = AgentRecord {
            project_id: f.target.id,
            name: "MOVER".to_owned(),
            ..claude_record_for(f.target.id, "MOVER")
        };
        append_jsonl(&f.target.registry_path, &resident).unwrap();

        let err = move_agent_impl(&state, f.mover.id, f.target.id, f.claude_home.path())
            .await
            .expect_err("a canonical name collision must refuse the move");

        assert!(
            matches!(&err, AppError::Core(CoreError::DuplicateAgentName { .. })),
            "got {err:?}"
        );
        assert_eq!(f.source.list_agents().unwrap().len(), 2);
        assert_eq!(f.target.list_agents().unwrap().len(), 1);
        assert!(lock(&state.maintenance).is_empty());
        assert!(state.store.list_move_intents().unwrap().is_empty());
    }

    fn claude_record_for(project_id: ProjectId, name: &str) -> AgentRecord {
        AgentRecord {
            id: Uuid::now_v7(),
            project_id,
            name: name.to_owned(),
            harness: HarnessKind::ClaudeCode,
            session_locator: Some(SessionLocator::Uuid(Uuid::now_v7())),
            model: None,
            effort: None,
            profiles: AgentProfiles::default(),
            forked_from_session: None,
            forked_from_session_home: None,
            session_home: None,
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn a_move_to_the_same_project_is_refused() {
        let root = TempDir::new().unwrap();
        let state = state_at(root.path());
        let dirs = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let f = seeded(&state, &dirs);
        activate_project(&state, f.source.clone()).unwrap();

        let err = move_agent_impl(&state, f.mover.id, f.source.id, f.claude_home.path())
            .await
            .expect_err("moving into the same project is meaningless");
        assert!(matches!(err, AppError::MoveSourceIsTarget), "got {err:?}");
    }

    /// A cold target open in another process refuses with the lock error and
    /// writes nothing — the picker deliberately never locks, so the commit
    /// path's real open is where contention surfaces.
    #[tokio::test]
    async fn a_move_into_a_project_locked_elsewhere_is_refused() {
        let root = TempDir::new().unwrap();
        let state = state_at(root.path());
        let dirs = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let f = seeded(&state, &dirs);
        activate_project(&state, f.source.clone()).unwrap();
        let _other = acquire_project_lock(f.target.id, &state.store.project_root(f.target.id))
            .expect("simulated other process");

        let err = move_agent_impl(&state, f.mover.id, f.target.id, f.claude_home.path())
            .await
            .expect_err("a target held elsewhere must refuse");

        assert!(
            matches!(err, AppError::ProjectLocked(id) if id == f.target.id),
            "got {err:?}"
        );
        assert!(state.store.list_move_intents().unwrap().is_empty());
        assert_eq!(f.target.list_agents().unwrap().len(), 0, "nothing written");
    }

    /// In-app failure whose immediate recovery also fails: exactly the two
    /// projects stay blocked with the repair error, and the intent file
    /// survives for the next launch.
    #[tokio::test]
    async fn a_persistently_failing_move_blocks_the_pair_with_the_repair_error() {
        let root = TempDir::new().unwrap();
        let state = state_at(root.path());
        let dirs = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let f = seeded(&state, &dirs);
        activate_project(&state, f.source.clone()).unwrap();
        // A persistent filesystem failure inside the surgery: the target
        // sidecar's destination path is occupied by a directory, so the durable
        // copy's rename fails — on the first attempt and on the in-app
        // recovery's re-drive alike.
        let dst = switchboard_harness::meta_sidecar::meta_sidecar_path(&f.target.root, f.mover.id);
        std::fs::create_dir_all(&dst).unwrap();

        let err = move_agent_impl(&state, f.mover.id, f.target.id, f.claude_home.path())
            .await
            .expect_err("a poisoned target must fail the move");

        assert!(
            matches!(err, AppError::MoveRepairRequired { .. }),
            "got {err:?}"
        );
        assert!(lock(&state.maintenance).contains(&f.source.id));
        assert!(lock(&state.maintenance).contains(&f.target.id));
        assert_eq!(state.store.list_move_intents().unwrap().len(), 1);
        let refusal = crate::commands::reject_if_under_maintenance(&state, f.target.id)
            .expect_err("the blocked project refuses work");
        assert!(matches!(refusal, AppError::MoveRepairRequired { .. }));
    }

    /// The belt-and-braces detector: half-moved state with **no intent file**
    /// (the one shape recovery cannot see — e.g. the file lost to disk failure)
    /// surfaces as a typed cross-project conflict at activation, never a silent
    /// pick-a-winner. Discovered rather than designed here: this fires on
    /// *any* attempt to load both projects, which is exactly where a user
    /// would first touch the anomaly.
    #[tokio::test]
    async fn half_moved_state_without_an_intent_surfaces_the_typed_conflict() {
        let root = TempDir::new().unwrap();
        let state = state_at(root.path());
        let dirs = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let f = seeded(&state, &dirs);
        // Simulate a lost intent after step (c): the row exists in both
        // registries and nothing on disk says a move was in flight.
        state.store.write_move_intent(&f.intent).unwrap();
        apply_move_steps(
            &state.store,
            &f.intent,
            f.claude_home.path(),
            Some(MoveStep::TargetRegistry),
        )
        .unwrap();
        for (path, _) in state.store.list_move_intents().unwrap() {
            std::fs::remove_file(path).unwrap();
        }

        activate_project(&state, f.source.clone()).unwrap();
        let err = activate_project(&state, f.target.clone())
            .expect_err("the duplicated row must surface, never silently resolve");

        assert!(
            matches!(err, AppError::AgentProjectConflict { agent_id, .. } if agent_id == f.mover.id),
            "got {err:?}"
        );
    }

    /// Through the real hydration path: the moved history renders in the
    /// target's conversation and not the source's — the merge partitions on
    /// journal contents, which is exactly what the surgery relocated. (No
    /// session files exist here, so agent transcripts degrade to empty and the
    /// journal side is what renders — the partition under test.)
    #[tokio::test]
    async fn hydration_renders_the_moved_history_in_the_target_only() {
        let root = TempDir::new().unwrap();
        let state = state_at(root.path());
        let dirs = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let f = seeded(&state, &dirs);
        state.store.write_move_intent(&f.intent).unwrap();
        apply_move_steps(&state.store, &f.intent, f.claude_home.path(), None).unwrap();

        let target_view = crate::commands::load_project_conversation_impl(
            &state,
            f.target.id,
            f.claude_home.path(),
            &[],
        )
        .await
        .unwrap();
        let source_view = crate::commands::load_project_conversation_impl(
            &state,
            f.source.id,
            f.claude_home.path(),
            &[],
        )
        .await
        .unwrap();

        let target_json = serde_json::to_string(&target_view).unwrap();
        let source_json = serde_json::to_string(&source_view).unwrap();
        assert!(
            target_json.contains("hello"),
            "moved history renders in the target"
        );
        assert!(
            !source_json.contains("hello"),
            "the source no longer renders the moved agent's turns"
        );
        assert!(
            source_json.contains("to both") && target_json.contains("to both"),
            "a fan-out send renders in both projects, each from its own journal"
        );
        assert!(
            target_json.contains("u__x.png"),
            "attachment-bearing turns still render with their original paths"
        );
    }

    /// An unresolvable working directory refuses the move rather than guessing.
    #[tokio::test]
    async fn a_move_into_a_project_with_an_unresolvable_directory_is_refused() {
        let root = TempDir::new().unwrap();
        let state = state_at(root.path());
        let dirs = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let f = seeded(&state, &dirs);
        activate_project(&state, f.source.clone()).unwrap();
        // Damage the catalog: drop the target directory's row.
        let catalog = state.store.root().join("directories.jsonl");
        let kept: Vec<String> = std::fs::read_to_string(&catalog)
            .unwrap()
            .lines()
            .filter(|l| {
                l.contains(
                    &dirs
                        .0
                        .path()
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .to_string(),
                )
            })
            .map(str::to_owned)
            .collect();
        std::fs::write(&catalog, kept.join("\n") + "\n").unwrap();

        let err = move_agent_impl(&state, f.mover.id, f.target.id, f.claude_home.path())
            .await
            .expect_err("an unresolvable target directory must refuse");

        assert!(
            matches!(err, AppError::Core(CoreError::DirectoryNotFound(_))),
            "got {err:?}"
        );
        assert!(state.store.list_move_intents().unwrap().is_empty());
    }

    // ---- session-home decision variants, through the real surgery ----

    #[tokio::test]
    async fn a_same_directory_move_stamps_no_session_home() {
        let root = TempDir::new().unwrap();
        let state = state_at(root.path());
        let dir = TempDir::new().unwrap();
        let d = state.store.add_directory(dir.path()).unwrap();
        let source = state
            .store
            .create_project(d.directory_id, "source")
            .unwrap();
        let target = state
            .store
            .create_project(d.directory_id, "target")
            .unwrap();
        let mover = claude_record_for(source.id, "mover");
        append_jsonl(&source.registry_path, &mover).unwrap();
        let intent = MoveIntent {
            agent_id: mover.id,
            source_project: source.id,
            target_project: target.id,
        };
        state.store.write_move_intent(&intent).unwrap();
        let home = TempDir::new().unwrap();

        let adopted = apply_move_steps(&state.store, &intent, home.path(), None)
            .unwrap()
            .unwrap();

        assert_eq!(adopted.session_home, None);
    }

    #[tokio::test]
    async fn a_cross_directory_move_of_an_undispatched_agent_stamps_no_home() {
        // No session file exists, so the agent's session will be created native
        // to the target — stamping the old directory would strand it there.
        let root = TempDir::new().unwrap();
        let state = state_at(root.path());
        let dirs = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let f = seeded(&state, &dirs);
        let fresh = claude_record_for(f.source.id, "undispatched");
        append_jsonl(&f.source.registry_path, &fresh).unwrap();
        let intent = MoveIntent {
            agent_id: fresh.id,
            source_project: f.source.id,
            target_project: f.target.id,
        };
        state.store.write_move_intent(&intent).unwrap();

        let adopted = apply_move_steps(&state.store, &intent, f.claude_home.path(), None)
            .unwrap()
            .unwrap();

        assert_eq!(adopted.session_home, None);
    }

    #[tokio::test]
    async fn a_second_cross_directory_move_re_stamps_the_original_home() {
        // The transcript never left the first directory, so recovery's
        // recomputation proposes the same value and adoption accepts it as
        // identical — the recompute-determinism contract, end to end.
        let root = TempDir::new().unwrap();
        let state = state_at(root.path());
        let dirs = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let f = seeded(&state, &dirs);
        let intent = f.intent.clone();
        state.store.write_move_intent(&intent).unwrap();
        let first = apply_move_steps(&state.store, &intent, f.claude_home.path(), None)
            .unwrap()
            .unwrap();
        assert_eq!(
            first.session_home.as_deref(),
            Some(f.source.directory.as_path())
        );

        let extra_dir = TempDir::new().unwrap();
        let d3 = state.store.add_directory(extra_dir.path()).unwrap();
        let third = state
            .store
            .create_project(d3.directory_id, "third")
            .unwrap();
        let second_intent = MoveIntent {
            agent_id: f.mover.id,
            source_project: f.target.id,
            target_project: third.id,
        };
        state.store.write_move_intent(&second_intent).unwrap();

        let second = apply_move_steps(&state.store, &second_intent, f.claude_home.path(), None)
            .unwrap()
            .unwrap();

        assert_eq!(
            second.session_home.as_deref(),
            Some(f.source.directory.as_path()),
            "the home recorded by the first move survives the second"
        );
    }
}
