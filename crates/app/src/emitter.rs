//! Per-dispatch emitter decorator that observes `SessionMeta` events to
//! clear the Codex-attach one-shot flag from `AppState::needs_session_meta`.
//!
//! **Why a decorator, not a dispatcher-side hook.** The dispatcher is
//! harness-agnostic and intentionally doesn't know about Tauri-layer state.
//! The attach-flow flag is owned at the app layer; pushing knowledge of
//! `needs_session_meta` (or `NormalizedEvent::SessionMeta`) into the
//! dispatcher would couple two layers that are otherwise cleanly separated.
//!
//! The decorator inspects the generic `serde_json::Value` payload (the
//! wire-format the dispatcher already emits) and matches on
//! `payload["type"] == "session_meta"` and `payload["agent_id"] == agent_id`.
//! No `NormalizedEvent` knowledge crosses the dispatcher boundary.
//!
//! **Why `Arc<Mutex<HashSet<AgentId>>>`.** The decorator is moved into the
//! dispatcher's spawned `'static` drain task; it can't borrow from
//! `AppState`. Cloning an `Arc` handle to the shared set is the simplest
//! shape that satisfies the `'static` requirement.
//!
//! **Read-don't-drain pairing.** `send_message_impl` reads the flag with
//! `contains` (not `remove`) before dispatching. The flag persists across
//! repeated dispatches that fail before `SessionMeta` is emitted; this
//! decorator clears it once emission is genuinely observed. The invariant
//! is pinned by the four-dispatch unit test in `commands.rs`.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use switchboard_core::AgentId;
use switchboard_dispatcher::EventEmitter;

use crate::state::lock;

/// Wraps an `EventEmitter` and, on every emit, looks for a `session_meta`
/// event payload for `agent_id`. Forwards every event to the inner emitter
/// regardless of whether it matched.
pub struct SessionMetaObservingEmitter {
    inner: Arc<dyn EventEmitter>,
    needs_session_meta: Arc<Mutex<HashSet<AgentId>>>,
    agent_id: AgentId,
    agent_id_str: String,
}

impl SessionMetaObservingEmitter {
    pub fn new(
        inner: Arc<dyn EventEmitter>,
        needs_session_meta: Arc<Mutex<HashSet<AgentId>>>,
        agent_id: AgentId,
    ) -> Self {
        let agent_id_str = agent_id.to_string();
        Self {
            inner,
            needs_session_meta,
            agent_id,
            agent_id_str,
        }
    }
}

impl EventEmitter for SessionMetaObservingEmitter {
    fn emit(&self, name: &str, payload: serde_json::Value) {
        let is_session_meta_for_agent = payload.get("type").and_then(serde_json::Value::as_str)
            == Some("session_meta")
            && payload.get("agent_id").and_then(serde_json::Value::as_str)
                == Some(&self.agent_id_str);
        if is_session_meta_for_agent {
            lock(&self.needs_session_meta).remove(&self.agent_id);
        }
        self.inner.emit(name, payload);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;
    use switchboard_dispatcher::RecordingEmitter;
    use uuid::Uuid;

    fn build(
        agent_id: AgentId,
    ) -> (
        SessionMetaObservingEmitter,
        Arc<RecordingEmitter>,
        Arc<Mutex<HashSet<AgentId>>>,
    ) {
        let inner = Arc::new(RecordingEmitter::new());
        let set = Arc::new(Mutex::new(HashSet::from([agent_id])));
        let decorator = SessionMetaObservingEmitter::new(
            Arc::clone(&inner) as Arc<dyn EventEmitter>,
            Arc::clone(&set),
            agent_id,
        );
        (decorator, inner, set)
    }

    #[test]
    fn matching_session_meta_clears_set_and_forwards() {
        let agent_id = Uuid::now_v7();
        let (decorator, inner, set) = build(agent_id);

        let payload = json!({
            "type": "session_meta",
            "agent_id": agent_id.to_string(),
            "model": "gpt-5",
            "harness_version": "0.130",
            "tools": [],
            "mcp_servers": [],
            "skills": [],
            "raw": {},
        });
        decorator.emit(&format!("agent:{agent_id}"), payload.clone());

        assert!(
            !lock(&set).contains(&agent_id),
            "matching session_meta must remove agent_id"
        );
        let recorded = inner.snapshot();
        assert_eq!(recorded.len(), 1, "event must still be forwarded");
        assert_eq!(recorded[0].1, payload);
    }

    #[test]
    fn session_meta_for_different_agent_does_not_clear() {
        let agent_id = Uuid::now_v7();
        let other_agent = Uuid::now_v7();
        let (decorator, _inner, set) = build(agent_id);

        let payload = json!({
            "type": "session_meta",
            "agent_id": other_agent.to_string(),
            "model": "gpt-5",
            "harness_version": "0.130",
            "tools": [],
            "mcp_servers": [],
            "skills": [],
            "raw": {},
        });
        decorator.emit("agent:wrong", payload);

        assert!(
            lock(&set).contains(&agent_id),
            "session_meta carrying a different agent_id must NOT clear our flag"
        );
    }

    #[test]
    fn non_session_meta_events_do_not_clear() {
        let agent_id = Uuid::now_v7();
        let (decorator, inner, set) = build(agent_id);

        for payload in [
            json!({"type": "turn_start", "turn_id": "abc", "started_at": "now"}),
            json!({"type": "content_chunk", "turn_id": "abc", "kind": "text", "text": "hi"}),
            json!({"type": "turn_end", "turn_id": "abc", "outcome": {"status": "completed"}, "ended_at": "now"}),
            json!({"type": "rate_limit_event", "agent_id": agent_id.to_string(), "info": {}}),
            json!({"type": "agent_idle", "agent_id": agent_id.to_string()}),
        ] {
            decorator.emit("agent:x", payload);
        }

        assert!(
            lock(&set).contains(&agent_id),
            "non-session_meta events must not clear the flag"
        );
        assert_eq!(inner.snapshot().len(), 5, "all events forwarded");
    }

    #[test]
    fn malformed_payload_is_forwarded_without_panic() {
        let agent_id = Uuid::now_v7();
        let (decorator, inner, set) = build(agent_id);

        // Missing `type`, `agent_id` is a number not a string, etc.
        decorator.emit("agent:x", json!({"agent_id": 42}));
        decorator.emit("agent:x", json!("not-an-object"));
        decorator.emit("agent:x", json!(null));

        assert!(lock(&set).contains(&agent_id));
        assert_eq!(inner.snapshot().len(), 3);
    }
}
