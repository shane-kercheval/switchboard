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

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use switchboard_core::{AgentId, AgentRecord, Project, SendId};
use switchboard_dispatcher::{
    ConversationJournal, DispatchContext, DispatchContextFactory, EventEmitter, MetadataCache,
    SessionLocatorSink,
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
}

impl ProjectDispatchContextFactory {
    pub fn new(
        project: Project,
        agent: AgentRecord,
        adapter: Arc<dyn HarnessAdapter>,
        base_emitter: Arc<dyn EventEmitter>,
        needs_session_meta: Arc<Mutex<HashSet<AgentId>>>,
        agents_by_id: Arc<Mutex<HashMap<AgentId, AgentRecord>>>,
        registry_write: Arc<Mutex<()>>,
    ) -> Self {
        Self {
            project,
            agent_id: agent.id,
            fallback_agent: agent,
            adapter,
            base_emitter,
            needs_session_meta,
            agents_by_id,
            registry_write,
        }
    }
}

impl DispatchContextFactory for ProjectDispatchContextFactory {
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
        let sidecar_path = switchboard_harness::meta_sidecar::meta_sidecar_path(
            &self.project.directory,
            self.project.id,
            agent_id,
        );
        let turnmeta_path = switchboard_harness::turnmeta_sidecar::turnmeta_sidecar_path(
            &self.project.directory,
            self.project.id,
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
