//! App-side [`DispatchContextFactory`]: rebuilds a turn's dispatch bundle on
//! demand. The dispatcher hands this to an agent's actor at creation; the actor
//! owns it and calls `build` at the moment each turn starts — so per-dispatch
//! state (e.g. `is_first_dispatch_after_attach`) is read *live*, never frozen at
//! enqueue. The dispatcher crate sits below the app and cannot construct
//! app-typed per-dispatch objects (`SessionMetaObservingEmitter`,
//! `ProjectJournal`), which is why this builder trait exists at all.
//!
//! Captures only agent-lifetime-stable data (agent record, cwd, journal path,
//! harness-selected adapter) plus live `Arc` handles to the base emitter and
//! `needs_session_meta`. It deliberately does not capture `Arc<Dispatcher>` —
//! it doesn't need it, so there is no reference cycle.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use switchboard_core::{AgentId, AgentRecord, SendId};
use switchboard_dispatcher::{
    ConversationJournal, DispatchContext, DispatchContextFactory, EventEmitter,
};
use switchboard_harness::{DispatchOptions, HarnessAdapter};

use crate::emitter::SessionMetaObservingEmitter;
use crate::journal::ProjectJournal;
use crate::state::lock;

pub struct ProjectDispatchContextFactory {
    agent: AgentRecord,
    cwd: PathBuf,
    journal_path: PathBuf,
    adapter: Arc<dyn HarnessAdapter>,
    base_emitter: Arc<dyn EventEmitter>,
    needs_session_meta: Arc<Mutex<HashSet<AgentId>>>,
}

impl ProjectDispatchContextFactory {
    pub fn new(
        agent: AgentRecord,
        cwd: PathBuf,
        journal_path: PathBuf,
        adapter: Arc<dyn HarnessAdapter>,
        base_emitter: Arc<dyn EventEmitter>,
        needs_session_meta: Arc<Mutex<HashSet<AgentId>>>,
    ) -> Self {
        Self {
            agent,
            cwd,
            journal_path,
            adapter,
            base_emitter,
            needs_session_meta,
        }
    }
}

impl DispatchContextFactory for ProjectDispatchContextFactory {
    fn build(&self, send_id: SendId) -> DispatchContext {
        let agent_id = self.agent.id;
        // Read (don't drain) the attach-flow flag *now* — the per-dispatch
        // emitter decorator clears it iff a `session_meta` event is observed.
        // Reading it here (per turn) is what keeps queued turns correct.
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
            Arc::new(ProjectJournal::new(self.journal_path.clone(), send_id));
        DispatchContext {
            adapter: Arc::clone(&self.adapter),
            cwd: self.cwd.clone(),
            agent: self.agent.clone(),
            emitter,
            options,
            journal,
        }
    }

    fn idle_emitter(&self) -> Arc<dyn EventEmitter> {
        Arc::clone(&self.base_emitter)
    }
}
