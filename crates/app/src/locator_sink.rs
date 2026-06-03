//! App-side runtime session-locator sink.
//!
//! Implements the dispatcher's [`SessionLocatorSink`] by persisting a captured
//! locator to the agent's registry record (`Project::set_session_locator`)
//! under the app's `registry_write` mutex, then refreshing the `agents_by_id`
//! cache so the next dispatch's `DispatchContext` reads the new locator. Built
//! per dispatch in the dispatch-context factory, mirroring
//! [`crate::journal::ProjectJournal`] / [`crate::metadata::ProjectMetadataCache`].
//!
//! **Load-bearing**, unlike the metadata cache: the dispatcher fails the turn
//! when `persist` returns `Err`, because a lost locator means the next turn
//! starts a fresh session and silently drops context (the old
//! sidecar-write-failure semantics).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use switchboard_core::{AgentId, AgentRecord, Project, SessionLocator};
use switchboard_dispatcher::{SessionLocatorError, SessionLocatorSink};

use crate::state::lock;

/// Persists a captured locator into one project's registry and the shared
/// `agents_by_id` cache. Holds the owning `Project` (for the in-place update)
/// plus the app's shared `registry_write` guard and `agents_by_id` cache, so
/// the write serializes against every other registry mutation exactly like
/// `set_agent_session_locator_impl`.
pub struct ProjectSessionLocatorSink {
    project: Project,
    registry_write: Arc<Mutex<()>>,
    agents_by_id: Arc<Mutex<HashMap<AgentId, AgentRecord>>>,
}

impl ProjectSessionLocatorSink {
    pub fn new(
        project: Project,
        registry_write: Arc<Mutex<()>>,
        agents_by_id: Arc<Mutex<HashMap<AgentId, AgentRecord>>>,
    ) -> Self {
        Self {
            project,
            registry_write,
            agents_by_id,
        }
    }
}

impl SessionLocatorSink for ProjectSessionLocatorSink {
    fn persist(
        &self,
        agent_id: AgentId,
        locator: SessionLocator,
    ) -> Result<(), SessionLocatorError> {
        let _write = lock(&self.registry_write);
        let updated = self
            .project
            .set_session_locator(agent_id, locator)
            .map_err(|e| SessionLocatorError(Box::new(e)))?;
        lock(&self.agents_by_id).insert(agent_id, updated);
        Ok(())
    }
}
