//! App-side [`DispatchContextFactory`]: rebuilds a turn's dispatch bundle on
//! demand. The dispatcher hands this to an agent's actor at creation; the actor
//! owns it and calls `build` at the moment each turn starts — so per-dispatch
//! state is read *live*, never frozen at enqueue. The dispatcher crate sits
//! below the app and cannot construct app-typed per-dispatch objects
//! (`SessionMetaObservingEmitter`, `ProjectJournal`, `ProjectSessionLocatorSink`),
//! which is why this builder trait exists at all.
//!
//! Two pieces of per-dispatch state are read live at `build()` time, both from
//! shared `Arc<Mutex<…>>` handles rather than frozen copies:
//! - `is_first_dispatch_after_attach`, from `needs_session_meta`.
//! - the agent's **current `AgentRecord`**, from `agents_by_id`. This is what
//!   carries a runtime-captured `session_locator` (persisted mid-turn by the
//!   locator sink) into the *next* turn's dispatch input. A frozen clone would
//!   make a Codex/Antigravity agent re-create its session every turn.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use switchboard_core::{AgentId, AgentRecord, Project, ProjectId, SendId, SessionLocator};
use switchboard_dispatcher::{
    ConversationJournal, DispatchContext, DispatchContextFactory, Dispatcher, EventEmitter,
    MetadataCache, SelectionSnapshot, SessionLocatorSink, TurnPermit,
};
use switchboard_harness::{DispatchOptions, HarnessAdapter};

use crate::emitter::SessionMetaObservingEmitter;
use crate::journal::ProjectJournal;
use crate::locator_sink::ProjectSessionLocatorSink;
use crate::metadata::ProjectMetadataCache;
use crate::state::lock;

pub struct ProjectDispatchContextFactory {
    /// The agent's owning project — the source for cwd (`project.directory`),
    /// the journal path, the metadata-sidecar path, and the in-place
    /// `set_session_locator` write the locator sink performs.
    project: Project,
    agent_id: AgentId,
    /// Fallback record used only if `agents_by_id` somehow lacks this agent at
    /// `build()` time; the live cache copy is preferred (it reflects a
    /// runtime-captured locator).
    fallback_agent: AgentRecord,
    adapter: Arc<dyn HarnessAdapter>,
    base_emitter: Arc<dyn EventEmitter>,
    needs_session_meta: Arc<Mutex<HashSet<AgentId>>>,
    agents_by_id: Arc<Mutex<HashMap<AgentId, AgentRecord>>>,
    registry_write: Arc<Mutex<()>>,
    /// Needed by `preflight` only: the start-moment materializing-fork check
    /// asks the dispatcher whether the parent is running a turn *now*, and
    /// resolves the fork's own session file under this home.
    dispatcher: Weak<Dispatcher>,
    home_dir: PathBuf,
    /// Root for the cross-process session locks `preflight` takes.
    lock_root: PathBuf,
    /// Projects closed to new work. Shared with `AppState`, never snapshotted:
    /// this is the one gate that must observe a mark set *after* the turn was
    /// queued.
    maintenance: Arc<Mutex<HashSet<ProjectId>>>,
    /// Live lifecycle generations, and the value that belongs to `project`.
    ///
    /// **This is what makes the frozen `project` above safe to keep.** The
    /// factory is built once and owned by the actor for its lifetime, so its
    /// `project` — and therefore the `cwd` every `build` hands the adapter — is a
    /// snapshot. A re-point or delete completing afterwards makes that snapshot
    /// name a directory the project no longer uses, and neither the app's
    /// pre-enqueue check nor the maintenance mark can catch a turn already past
    /// them: a queued send resumes *after* the window closes and still carries
    /// this stale factory. Comparing the generation at turn start does catch it,
    /// however long the item waited.
    ///
    /// **`generation_at_capture` is supplied by the caller, never sampled here.**
    /// It has to be the generation as of the moment `project` was *read*, and
    /// that is not the moment this factory is constructed: `send_message_impl`
    /// resolves the project, then awaits the materializing-fork gate, and only
    /// then builds the factory. Reading the counter here would pair a stale
    /// `project` with a post-operation generation and compare it against itself —
    /// a check that passes exactly when it should fail. An earlier version of
    /// this code did that, under a comment claiming the two were captured
    /// together.
    project_generation: Arc<Mutex<HashMap<ProjectId, u64>>>,
    generation_at_capture: u64,
}

/// The app-wide handles every dispatch context needs, grouped so the factory's
/// constructor stays readable as it accumulates them (same reason `NewAgent`
/// exists for registration).
pub struct DispatchDeps {
    pub base_emitter: Arc<dyn EventEmitter>,
    pub needs_session_meta: Arc<Mutex<HashSet<AgentId>>>,
    pub agents_by_id: Arc<Mutex<HashMap<AgentId, AgentRecord>>>,
    pub registry_write: Arc<Mutex<()>>,
    /// `preflight` only: asks whether the fork's parent is running a turn *now*.
    ///
    /// **Weak, deliberately.** The trait contract forbids a factory holding
    /// `Arc<Dispatcher>`: the dispatcher owns each actor's command sender, that
    /// sender is what keeps the actor task parked rather than exiting, and the
    /// actor owns this factory — so a strong handle here closes a cycle and the
    /// dispatcher can never drop.
    pub dispatcher: Weak<Dispatcher>,
    /// `preflight` only: resolves whether the fork's own session file exists yet.
    pub home_dir: PathBuf,
    /// `preflight` only: where the cross-process session locks live.
    pub lock_root: PathBuf,
    /// `preflight` only: the lifecycle generations to re-check against.
    pub project_generation: Arc<Mutex<HashMap<ProjectId, u64>>>,
    /// `preflight` only: projects closed to new work, shared with `AppState`.
    pub maintenance: Arc<Mutex<HashSet<ProjectId>>>,
    /// The generation as of the moment the caller read the `Project` it is
    /// handing over — **not** as of now. See the field of the same name on
    /// [`ProjectDispatchContextFactory`] for why the distinction is the whole
    /// point.
    pub generation_at_capture: u64,
}

impl ProjectDispatchContextFactory {
    pub fn new(
        project: Project,
        agent: AgentRecord,
        adapter: Arc<dyn HarnessAdapter>,
        deps: DispatchDeps,
    ) -> Self {
        let DispatchDeps {
            base_emitter,
            needs_session_meta,
            agents_by_id,
            registry_write,
            dispatcher,
            home_dir,
            lock_root,
            maintenance,
            project_generation,
            generation_at_capture,
        } = deps;
        Self {
            project,
            agent_id: agent.id,
            fallback_agent: agent,
            adapter,
            base_emitter,
            needs_session_meta,
            agents_by_id,
            registry_write,
            dispatcher,
            home_dir,
            lock_root,
            maintenance,
            project_generation,
            generation_at_capture,
        }
    }

    /// Every harness session file this turn may write, as lock keys.
    /// Thin shim; the rule and its rationale live on [`session_lock_keys_for`].
    fn session_lock_keys(
        &self,
        agent: &AgentRecord,
    ) -> Result<BTreeSet<String>, crate::error::AppError> {
        session_lock_keys_for(agent, &self.project.directory, &self.home_dir)
    }
}

/// The cross-process session-lock keys one dispatch of `agent` must hold.
///
/// Two keys, not one, when the agent is a **fork that has not materialized
/// yet**: that dispatch reads the *parent's* session file to branch from it
/// while carrying its own freshly generated uuid, so keying only on the
/// dispatching agent would lock a file nobody contends on and leave the
/// contended one open. It writes its own file too, hence "in addition to,"
/// not "instead of."
///
/// **Materialization is resolved from the fork's own session file, and
/// staleness from `forked_from_session` — never from `busy_fork_source`.**
/// That function answers a different question: it returns the parent only
/// when the parent is busy *in this process*, and `None` for "not a fork,
/// already materialized, parent gone, or parent idle." Keying off it would
/// invert the coverage — the parent would be locked only in the case the
/// in-process gate above was already refusing. "Parent gone" is the same trap
/// mirrored: it yields `None` because *our* process is not writing that
/// session, while the file is still on disk, still read by the fork, and
/// possibly live in the other build.
///
/// **An agent with no locator yet takes no lock, and that exception is
/// safe for a reason that is not a check anywhere in this codebase.** A
/// Codex or Antigravity first turn has no session *yet* — the harness mints
/// the id during this dispatch, and the locator sink persists it, so the
/// *next* turn locks it. Nothing can be contending for a conversation that
/// does not exist. The assumption this rests on, stated so it can be
/// re-checked when a harness changes: **harness-assigned conversation ids are
/// unique per conversation** (recorded in `docs/harness-behavior.md`).
///
/// An earlier version of this comment credited a "post-capture uniqueness
/// scan" instead. There is no such scan — `check_claude_session_id_unique`
/// and its siblings run at *attach*, and
/// [`crate::locator_sink::ProjectSessionLocatorSink::persist`] writes the
/// captured locator straight through. The exception was always sound; its
/// stated reason was invented.
///
/// **Keys name where the session *file* is, not where the agent now lives.** A
/// Claude lock key includes the working directory (its session ids are
/// cwd-namespaced), so keying a moved agent by its current project would mint a
/// key for a session that does not exist there and silently stop contending
/// with whatever is driving the real transcript from its recorded home — the
/// protection disappears with nothing failing. The agent's *spawn* cwd is
/// unaffected; only the key follows the record.
///
/// **The fork-parent key uses the dispatching agent's own session directory,
/// which is exact only while that directory is also where the parent's
/// transcript lives.** It diverges in two shapes, both requiring a move and so
/// unreachable until one exists: a fork *created after* its parent moved (the
/// child belongs to the parent's new project, the parent's transcript is still
/// in the old one), and a fork whose own agent has since been moved. The
/// parent's location cannot be recovered here — an unmoved parent records no
/// home, and resolving its project needs the store this factory does not carry
/// — so it is fixed at the source instead: the move milestone records the
/// parent's effective session directory as immutable fork provenance when the
/// fork is created, and this key reads it. Deliberately **not** resolved by
/// searching the agent cache for a matching session id: ids are unique only per
/// directory, so an unscoped search can lock an unrelated conversation while
/// leaving the real one unguarded (the same trap `busy_fork_source` documents,
/// which is why its own lookup is scoped to the fork's project).
fn session_lock_keys_for(
    agent: &AgentRecord,
    project_directory: &Path,
    home_dir: &Path,
) -> Result<BTreeSet<String>, crate::error::AppError> {
    let mut keys = BTreeSet::new();
    let session_cwd = agent.effective_session_directory(project_directory);
    if let Some(locator) = &agent.session_locator {
        keys.insert(crate::session_lock::session_lock_key(
            agent.harness,
            locator,
            session_cwd,
        )?);
    }
    if let Some(parent) = agent.forked_from_session
        && crate::commands::resolve_session_file(agent, project_directory, home_dir).is_none()
    {
        keys.insert(crate::session_lock::session_lock_key(
            agent.harness,
            &SessionLocator::Uuid(parent),
            session_cwd,
        )?);
    }
    Ok(keys)
}

impl DispatchContextFactory for ProjectDispatchContextFactory {
    fn selection_snapshot(&self) -> Option<SelectionSnapshot> {
        let agent = lock(&self.agents_by_id)
            .get(&self.agent_id)
            .cloned()
            .unwrap_or_else(|| self.fallback_agent.clone());
        let selected = agent.active_profile();
        Some(SelectionSnapshot {
            model: selected.model.clone(),
            effort: selected.effort.clone(),
        })
    }

    /// Everything this turn must be admitted past, and everything it must hold
    /// while it runs, in one call — see [`DispatchContextFactory::preflight`] for
    /// why the two are inseparable.
    ///
    /// Three gates, cheapest and most-likely-to-refuse first, with the
    /// acquisition last so a refused turn never creates or touches a lock file:
    ///
    /// 1. **Lifecycle staleness.** This factory's `project` is frozen; a
    ///    re-point or delete that completed while this item sat in the backlog
    ///    invalidates it. See the field docs on `project_generation`.
    /// 2. **The authoritative materializing-fork check.** `send_message_impl`
    ///    runs the same policy before enqueuing so the common refusal is
    ///    immediate and friendly, but a send to a busy agent queues — and a fork
    ///    whose first turn failed without writing its session file is
    ///    materialized by whatever ordinary send runs next. That send may have
    ///    been queued while the parent was idle and pop long after the parent
    ///    started working. This is the freshest judgement available: immediately
    ///    before the journal write and spawn, with no unrelated await in between.
    ///    It guards *this* process only.
    /// 3. **The cross-process session lock**, which guards the other process —
    ///    the concurrently-running dev or release build. Gate 2 is a look; this
    ///    is the lock, and together they close `harness-behavior.md` §3.5's
    ///    residual. See `crate::session_lock`.
    ///
    /// **Gate 2 is not made redundant by gate 3, and deleting it would lose three
    /// things.** It names the specific parent ("alice is working") where gate 3
    /// can only report generic contention; it runs before any lock file is
    /// opened, so the common in-process refusal touches no filesystem; and it is
    /// the only one that catches self-referential provenance — an agent whose
    /// `forked_from_session` names its own session produces one key, not two, so
    /// gate 3 deduplicates and simply acquires it. (Deliberately *not* asserted
    /// as a fourth reason: whether an actor future dropped rather than shut down
    /// releases the permit while its subprocess is still alive. Plausible,
    /// unverified, so it is not written as an argument.)
    fn preflight<'a>(
        &'a self,
        agent: &'a AgentRecord,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<TurnPermit, String>> + Send + 'a>>
    {
        Box::pin(async move {
            // **The authoritative maintenance check, and not a duplicate of the
            // admission-time one.** A send passes admission, sits in the queue,
            // and a move then marks the project and observes an empty queue —
            // the queued send would start mid-surgery. Only this check runs at
            // the instant the turn actually begins, which is what bounds that
            // race. Refusing here leaves no durable trace (no journal write has
            // happened yet), which is exactly what a blocked turn wants.
            if lock(&self.maintenance).contains(&self.project.id) {
                return Err(
                    crate::error::AppError::ProjectUnderMaintenance(self.project.id).to_string(),
                );
            }
            let generation_now = lock(&self.project_generation)
                .get(&self.project.id)
                .copied()
                .unwrap_or(0);
            if generation_now != self.generation_at_capture {
                return Err(crate::error::AppError::ProjectViewStale(self.project.id).to_string());
            }
            // Fail **closed** when the dispatcher is gone. Upgrade only fails
            // once teardown has begun, and actors can still be draining work
            // then — so "nothing can be running" is not a safe inference.
            // Refusing a turn during shutdown is harmless; forking a busy parent
            // is not. (Contrast the absent-parent case inside `busy_fork_source`,
            // which allows: no parent means nothing of ours is writing that
            // session, which is a fact rather than an unknown.)
            let Some(dispatcher) = self.dispatcher.upgrade() else {
                return Err("Switchboard is shutting down".to_owned());
            };
            if let Some(parent) = crate::commands::busy_fork_source(
                &self.agents_by_id,
                &dispatcher,
                agent,
                &self.project.directory,
                &self.home_dir,
            )
            .await
            {
                return Err(
                    crate::error::AppError::ForkSourceBusy { name: parent.name }.to_string()
                );
            }
            let keys = self.session_lock_keys(agent).map_err(|e| e.to_string())?;
            // **Off the async worker.** Acquisition opens files and, when
            // contended, sleeps through a backoff — and contention is no longer
            // exceptional: an unmaterialized branch deterministically holds its
            // parent's key for a whole turn, so every send to that parent walks
            // the full wait before refusing. Parking a runtime worker for that
            // delays unrelated agents' events, cancellations, and lifecycle
            // drains. (`session_lock_keys`' single `exists()` stat above stays
            // inline on purpose — one stat is not worth a task hop.)
            let lock_root = self.lock_root.clone();
            tokio::task::spawn_blocking(move || {
                crate::session_lock::acquire_session_locks(&lock_root, &keys)
            })
            .await
            .map_err(|e| format!("session lock task failed: {e}"))?
            .map_err(|e| e.to_string())
        })
    }

    fn build(&self, send_id: SendId) -> DispatchContext {
        let agent_id = self.agent_id;
        // Live-read the current record: a locator captured on a prior turn was
        // written to `agents_by_id` by the sink, and this dispatch must pass it
        // to the adapter as resume input.
        let agent = lock(&self.agents_by_id)
            .get(&agent_id)
            .cloned()
            .unwrap_or_else(|| self.fallback_agent.clone());
        // Read (don't drain) the attach-flow flag *now* — the per-dispatch
        // emitter decorator clears it iff a `session_meta` event is observed.
        let is_first_dispatch_after_attach = lock(&self.needs_session_meta).contains(&agent_id);
        let options = DispatchOptions {
            is_first_dispatch_after_attach,
            // The dispatcher overwrites `cancel_token` with the turn's token.
            ..Default::default()
        };
        let emitter: Arc<dyn EventEmitter> = Arc::new(SessionMetaObservingEmitter::new(
            Arc::clone(&self.base_emitter),
            Arc::clone(&self.needs_session_meta),
            agent_id,
        ));
        let journal: Arc<dyn ConversationJournal> =
            Arc::new(ProjectJournal::new(self.project.journal_path(), send_id));
        let sidecar_path =
            switchboard_harness::meta_sidecar::meta_sidecar_path(&self.project.root, agent_id);
        let turnmeta_path = switchboard_harness::turnmeta_sidecar::turnmeta_sidecar_path(
            &self.project.root,
            agent_id,
        );
        let metadata: Arc<dyn MetadataCache> = Arc::new(ProjectMetadataCache::new(
            agent_id,
            sidecar_path,
            turnmeta_path,
        ));
        let locator_sink: Arc<dyn SessionLocatorSink> = Arc::new(ProjectSessionLocatorSink::new(
            self.project.clone(),
            Arc::clone(&self.registry_write),
            Arc::clone(&self.agents_by_id),
        ));
        DispatchContext {
            adapter: Arc::clone(&self.adapter),
            cwd: self.project.directory.clone(),
            agent,
            emitter,
            options,
            journal,
            metadata,
            locator_sink,
        }
    }

    fn idle_emitter(&self) -> Arc<dyn EventEmitter> {
        Arc::clone(&self.base_emitter)
    }
}

#[cfg(test)]
mod session_lock_key_tests {
    use std::path::{Path, PathBuf};

    use switchboard_core::{AgentProfiles, AgentRecord, HarnessKind, SessionLocator};
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::session_lock_keys_for;

    fn claude_agent(session: Uuid) -> AgentRecord {
        AgentRecord {
            session_home: None,
            id: Uuid::now_v7(),
            project_id: Uuid::now_v7(),
            name: "agent".to_owned(),
            harness: HarnessKind::ClaudeCode,
            session_locator: Some(SessionLocator::Uuid(session)),
            model: None,
            effort: None,
            profiles: AgentProfiles::default(),
            forked_from_session: None,
            created_at: chrono::Utc::now(),
        }
    }

    /// The contention guarantee: a moved agent must take the *same* lock as
    /// anything driving that session from the directory the transcript actually
    /// lives in. Keying it by its new project would mint a different key, and
    /// two processes could then write one session file with nothing failing.
    #[test]
    fn a_moved_agent_locks_the_same_key_as_its_session_home() {
        let home_dir = TempDir::new().unwrap();
        let session = Uuid::now_v7();
        let session_home = PathBuf::from("/work/original-checkout");
        let new_project = Path::new("/work/somewhere-else");

        let moved = AgentRecord {
            session_home: Some(session_home.clone()),
            ..claude_agent(session)
        };
        let unmoved_at_home = claude_agent(session);

        assert_eq!(
            session_lock_keys_for(&moved, new_project, home_dir.path()).unwrap(),
            session_lock_keys_for(&unmoved_at_home, &session_home, home_dir.path()).unwrap(),
            "a moved agent must contend with its session's real location"
        );
    }

    #[test]
    fn a_moved_agent_does_not_lock_its_new_projects_key() {
        let home_dir = TempDir::new().unwrap();
        let session = Uuid::now_v7();
        let new_project = Path::new("/work/somewhere-else");

        let moved = AgentRecord {
            session_home: Some(PathBuf::from("/work/original-checkout")),
            ..claude_agent(session)
        };

        assert_ne!(
            session_lock_keys_for(&moved, new_project, home_dir.path()).unwrap(),
            session_lock_keys_for(&claude_agent(session), new_project, home_dir.path()).unwrap(),
            "keying by the new project would name a session that isn't there"
        );
    }

    #[test]
    fn an_agent_with_no_recorded_home_is_unchanged() {
        let home_dir = TempDir::new().unwrap();
        let session = Uuid::now_v7();
        let project = Path::new("/work/project");

        let keys = session_lock_keys_for(&claude_agent(session), project, home_dir.path()).unwrap();

        assert_eq!(
            keys,
            std::iter::once(
                crate::session_lock::session_lock_key(
                    HarnessKind::ClaudeCode,
                    &SessionLocator::Uuid(session),
                    project,
                )
                .unwrap()
            )
            .collect::<std::collections::BTreeSet<_>>()
        );
    }

    /// A materializing fork locks its parent under its own session directory,
    /// which is exact while the two share one. The divergent shapes all require
    /// a move and are closed at the source by fork provenance (see the M4 rule
    /// in the move plan), never by searching the cache for a matching id.
    #[test]
    fn a_materializing_fork_locks_the_parent_under_its_own_session_directory() {
        let home_dir = TempDir::new().unwrap();
        let parent_session = Uuid::now_v7();
        let project = Path::new("/work/project");

        let child = AgentRecord {
            forked_from_session: Some(parent_session),
            ..claude_agent(Uuid::now_v7())
        };

        let keys = session_lock_keys_for(&child, project, home_dir.path()).unwrap();
        let parent_key = crate::session_lock::session_lock_key(
            HarnessKind::ClaudeCode,
            &SessionLocator::Uuid(parent_session),
            project,
        )
        .unwrap();

        assert!(keys.contains(&parent_key), "got {keys:?}");
        assert_eq!(keys.len(), 2, "own session plus the parent's: {keys:?}");
    }

    /// Regression guard for re-introducing an unscoped search for the parent:
    /// Claude session ids are unique only per directory, so an agent elsewhere
    /// carrying the same id is a different conversation. Locking *its*
    /// directory would guard the wrong file and leave the real parent open.
    #[test]
    fn an_unrelated_agent_sharing_the_parents_session_id_does_not_move_the_key() {
        let home_dir = TempDir::new().unwrap();
        let parent_session = Uuid::now_v7();
        let project = Path::new("/work/project");

        let child = AgentRecord {
            forked_from_session: Some(parent_session),
            ..claude_agent(Uuid::now_v7())
        };
        // The same session id, in another project, with a home of its own —
        // exactly what an unscoped cache search would have latched onto.
        let _impostor = AgentRecord {
            session_home: Some(PathBuf::from("/work/unrelated-checkout")),
            ..claude_agent(parent_session)
        };

        let keys = session_lock_keys_for(&child, project, home_dir.path()).unwrap();

        assert!(
            keys.contains(
                &crate::session_lock::session_lock_key(
                    HarnessKind::ClaudeCode,
                    &SessionLocator::Uuid(parent_session),
                    project,
                )
                .unwrap()
            ),
            "the key must derive from this dispatch alone, got {keys:?}"
        );
    }
}
