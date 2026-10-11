//! Fixtures for the `ops` tests: a scripted harness adapter whose turns a test
//! can park and release, an `AppState` over a store that survives a "restart",
//! and a window-sync double.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use switchboard_core::{AgentId, AgentRecord, ProjectId, SessionLocator};
use switchboard_dispatcher::{EventEmitter, RecordingEmitter};
use switchboard_harness::{
    AdapterEvent, ContentKind, DispatchError, DispatchOptions, EventStream, HarnessAdapter, TurnId,
    TurnOutcome,
};
use tempfile::TempDir;
use tokio::sync::Notify;
use uuid::Uuid;

use crate::commands::{create_project_impl, set_agent_session_locator_impl};
use crate::error::AppError;
use crate::state::{AppState, lock};

use super::desktop_sync::{DesktopSync, ReadyReport};

/// One scripted turn.
#[derive(Clone)]
pub struct Script {
    /// The text the turn streams.
    pub text: String,
    /// Park after streaming the text until this fires (or the turn is
    /// cancelled), then end.
    pub park: Option<Arc<Notify>>,
    /// End with a failed terminal instead of a completed one.
    pub fail: bool,
    /// The hydration key the terminal carries, so the dispatcher journals a
    /// durable link for the turn.
    pub key: Option<String>,
    /// Keep the stream open past a cancellation until the gate fires, so a
    /// test can hold an agent in its closing state.
    pub ignore_cancel: bool,
}

impl Script {
    pub fn reply(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            park: None,
            fail: false,
            key: None,
            ignore_cancel: false,
        }
    }

    pub fn parked(text: &str, gate: &Arc<Notify>) -> Self {
        Self {
            park: Some(Arc::clone(gate)),
            ..Self::reply(text)
        }
    }

    pub fn failing(text: &str) -> Self {
        Self {
            fail: true,
            ..Self::reply(text)
        }
    }
}

/// A harness adapter that plays one [`Script`] per dispatch, in order, and
/// replies `ack: <prompt>` once the scripts run out. Records every prompt it
/// was given, and, like the real Claude adapter, leaves a completed turn in
/// the agent's session file under `home`, so disk reads (forwards from an
/// idle source, the restart fallback) see what the turn produced.
pub struct ScriptedAdapter {
    scripts: Mutex<VecDeque<Script>>,
    home: PathBuf,
    pub prompts: Mutex<Vec<String>>,
}

impl ScriptedAdapter {
    pub fn new(scripts: Vec<Script>, home: &Path) -> Self {
        Self {
            scripts: Mutex::new(scripts.into()),
            home: home.to_path_buf(),
            prompts: Mutex::new(Vec::new()),
        }
    }

    pub fn push(&self, script: Script) {
        lock(&self.scripts).push_back(script);
    }
}

#[async_trait]
impl HarnessAdapter for ScriptedAdapter {
    fn probe(&self) -> Result<(), DispatchError> {
        Ok(())
    }

    fn version(&self) -> Option<String> {
        None
    }

    async fn dispatch(
        &self,
        agent: &AgentRecord,
        cwd: &Path,
        prompt: &str,
        turn_id: TurnId,
        options: DispatchOptions,
    ) -> Result<EventStream, DispatchError> {
        lock(&self.prompts).push(prompt.to_owned());
        let script = lock(&self.scripts)
            .pop_front()
            .unwrap_or_else(|| Script::reply(&format!("ack: {prompt}")));
        let session_file = match &agent.session_locator {
            Some(SessionLocator::Uuid(session)) => Some(
                switchboard_harness::claude_session_file_path(&self.home, cwd, session),
            ),
            _ => None,
        };
        let prompt = prompt.to_owned();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            let _ = tx.send(AdapterEvent::ContentChunk {
                turn_id,
                kind: ContentKind::Text,
                text: script.text.clone(),
            });
            if let Some(gate) = &script.park {
                if script.ignore_cancel {
                    gate.notified().await;
                } else {
                    tokio::select! {
                        () = gate.notified() => {}
                        () = options.cancel_token.cancelled() => return,
                    }
                }
            }
            let key = script
                .key
                .clone()
                .unwrap_or_else(|| format!("msg-{turn_id}"));
            let outcome = if script.fail {
                TurnOutcome::Failed {
                    kind: switchboard_harness::FailureKind::HarnessError,
                    message: "scripted failure".to_owned(),
                }
            } else {
                if let Some(path) = &session_file {
                    append_claude_turn(path, &prompt, &script.text, &key);
                }
                TurnOutcome::Completed
            };
            let _ = tx.send(AdapterEvent::TurnEnd {
                turn_id,
                outcome,
                ended_at: Utc::now(),
                usage: None,
                context_window_source: None,
                stable_message_id: Some(key.clone()),
                first_message_id: Some(key),
                spend: None,
                model: None,
                effort: None,
            });
        });
        Ok(Box::pin(
            tokio_stream::wrappers::UnboundedReceiverStream::new(rx),
        ))
    }

    async fn compact(
        &self,
        agent: &AgentRecord,
        _cwd: &Path,
        _turn_id: TurnId,
        _options: DispatchOptions,
    ) -> Result<EventStream, DispatchError> {
        Err(DispatchError::UnsupportedOperation {
            harness: agent.harness,
            operation: "compact",
        })
    }

    async fn context_report(
        &self,
        agent: &AgentRecord,
        _cwd: &Path,
        _turn_id: TurnId,
        _options: DispatchOptions,
    ) -> Result<EventStream, DispatchError> {
        Err(DispatchError::UnsupportedOperation {
            harness: agent.harness,
            operation: "context_report",
        })
    }
}

/// A window-sync double: answers with a configured report, after an optional
/// delay, or with a load timeout; counts the notifications it received.
#[derive(Default)]
pub struct StubDesktopSync {
    pub unsubscribed: Mutex<Vec<AgentId>>,
    pub delay: Mutex<Option<Duration>>,
    pub times_out: Mutex<bool>,
    pub ready_calls: Mutex<Vec<ProjectId>>,
    pub roster_changed: Mutex<Vec<ProjectId>>,
    pub projects_changed: Mutex<usize>,
    pub runs_changed: Mutex<Vec<ProjectId>>,
}

#[async_trait]
impl DesktopSync for StubDesktopSync {
    async fn ensure_ready(&self, project_id: ProjectId) -> Result<ReadyReport, AppError> {
        lock(&self.ready_calls).push(project_id);
        let delay = *lock(&self.delay);
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        if *lock(&self.times_out) {
            return Err(AppError::ProjectLoadTimeout(project_id));
        }
        Ok(ReadyReport {
            unsubscribed_agents: lock(&self.unsubscribed).clone(),
        })
    }

    fn roster_changed(&self, project_id: ProjectId) {
        lock(&self.roster_changed).push(project_id);
    }

    fn projects_changed(&self) {
        *lock(&self.projects_changed) += 1;
    }

    fn runs_changed(&self, project_id: ProjectId) {
        lock(&self.runs_changed).push(project_id);
    }
}

/// A store root and an `AppState` over it. `restart` builds a fresh state
/// over the same store, which is how a test starts from "a project the window
/// has not opened": the project exists on disk and nothing in memory knows it.
pub struct TestEnv {
    pub state: Arc<AppState>,
    pub store_root: TempDir,
    pub home: TempDir,
    /// `prompts/` and `workflows/` for a state built with
    /// [`TestEnv::with_prompts_and_workflows`].
    pub library: TempDir,
    pub adapter: Arc<ScriptedAdapter>,
    pub emitter: Arc<RecordingEmitter>,
    pub sync: Arc<StubDesktopSync>,
    /// Working directories created for projects, kept alive with the env.
    pub dirs: Vec<TempDir>,
    library_enabled: bool,
}

impl TestEnv {
    pub fn new() -> Self {
        Self::with_scripts(Vec::new())
    }

    pub fn with_scripts(scripts: Vec<Script>) -> Self {
        let store_root = TempDir::new().unwrap();
        let library = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        let adapter = Arc::new(ScriptedAdapter::new(scripts, home.path()));
        let (state, emitter, sync) = build_state(store_root.path(), &adapter, None);
        Self {
            state,
            store_root,
            home,
            library,
            adapter,
            emitter,
            sync,
            dirs: Vec::new(),
            library_enabled: false,
        }
    }

    /// Rebuild the state with a local prompt store and a workflows folder,
    /// both under [`TestEnv::library`]. Call before creating projects.
    pub fn with_prompts_and_workflows(mut self) -> Self {
        self.library_enabled = true;
        self.restart()
    }

    pub fn prompts_dir(&self) -> PathBuf {
        self.library.path().join("prompts")
    }

    pub fn workflows_dir(&self) -> PathBuf {
        self.library.path().join("workflows")
    }

    /// A fresh state over the same store and home: nothing loaded, the
    /// ledger empty, the scripts cleared. The old state is dropped first so
    /// its project locks are released.
    pub fn restart(mut self) -> Self {
        let adapter = Arc::new(ScriptedAdapter::new(Vec::new(), self.home.path()));
        let old = std::mem::replace(
            &mut self.state,
            Arc::new(AppState::new_for_test(
                Arc::clone(&adapter) as Arc<dyn HarnessAdapter>,
                Arc::clone(&adapter) as Arc<dyn HarnessAdapter>,
                Arc::clone(&adapter) as Arc<dyn HarnessAdapter>,
                Arc::new(RecordingEmitter::new()) as Arc<dyn EventEmitter>,
            )),
        );
        drop(old);
        let library = self
            .library_enabled
            .then(|| self.library.path().to_path_buf());
        let (state, emitter, sync) = build_state(self.store_root.path(), &adapter, library);
        self.state = state;
        self.emitter = emitter;
        self.sync = sync;
        self.adapter = adapter;
        self
    }

    pub fn home(&self) -> PathBuf {
        self.home.path().to_path_buf()
    }

    pub fn handle(&self) -> Arc<dyn super::handle::AppStateHandle> {
        Arc::new(Arc::clone(&self.state))
    }

    /// Install a barrier at `seam`, returning it for the test to drive.
    pub fn seam(&self, seam: crate::state::TestSeam) -> Arc<crate::state::MaintenanceBarrier> {
        let barrier = Arc::new(crate::state::MaintenanceBarrier::default());
        lock(&self.state.test_seams).insert(seam, Arc::clone(&barrier));
        barrier
    }

    pub fn clear_seam(&self, seam: crate::state::TestSeam) {
        lock(&self.state.test_seams).remove(&seam);
    }

    /// Create a project in a fresh working directory, loaded in this state.
    pub fn project(&mut self, name: &str) -> ProjectId {
        let dir = TempDir::new().unwrap();
        let summary = create_project_impl(&self.state, name, dir.path().to_str().unwrap()).unwrap();
        self.dirs.push(dir);
        summary.id
    }

    /// Create a Claude agent in a loaded project through the command layer.
    pub fn agent(&self, project_id: ProjectId, name: &str) -> AgentId {
        crate::commands::create_agent_in_project_impl(
            &self.state,
            project_id,
            name,
            switchboard_core::HarnessKind::ClaudeCode,
            switchboard_core::AgentSelection::default(),
        )
        .unwrap()
        .id
    }

    /// Point the agent at a new Claude session and return its file's path,
    /// with the directory created.
    fn new_claude_session(&self, project_id: ProjectId, agent_id: AgentId) -> PathBuf {
        let session = Uuid::now_v7();
        set_agent_session_locator_impl(&self.state, agent_id, SessionLocator::Uuid(session))
            .unwrap();
        let directory = lock(&self.state.projects)
            .get(&project_id)
            .unwrap()
            .directory
            .clone();
        let path =
            switchboard_harness::claude_session_file_path(self.home.path(), &directory, &session);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        path
    }

    /// Give the agent a Claude session file holding one completed turn whose
    /// assistant text is `text` and whose message id (the hydration key) is
    /// `key`.
    pub fn seed_claude_turn(
        &self,
        project_id: ProjectId,
        agent_id: AgentId,
        text: &str,
        key: &str,
    ) {
        let path = self.new_claude_session(project_id, agent_id);
        std::fs::write(&path, claude_session_jsonl("hi", text, key)).unwrap();
    }

    /// Give the agent a Claude session file that cannot be read: invalid
    /// UTF-8, which the loader reports as an I/O error rather than salvaging
    /// line by line.
    pub fn seed_unreadable_session(&self, project_id: ProjectId, agent_id: AgentId) {
        let path = self.new_claude_session(project_id, agent_id);
        std::fs::write(&path, [0xff, 0xfe, 0xfd]).unwrap();
    }

    /// Give the agent a Claude session file holding these completed turns, in
    /// order: `(prompt, answer, message id, when)`.
    pub fn seed_claude_turns(
        &self,
        project_id: ProjectId,
        agent_id: AgentId,
        turns: &[(&str, &str, &str, chrono::DateTime<Utc>)],
    ) {
        let mut lines = Vec::new();
        for (prompt, answer, key, at) in turns {
            lines.push(serde_json::json!({
                "type": "user",
                "message": { "role": "user", "content": prompt },
                "timestamp": at.to_rfc3339(),
            }));
            lines.push(serde_json::json!({
                "type": "assistant",
                "message": {
                    "id": key,
                    "model": "claude-sonnet-4-6",
                    "role": "assistant",
                    "content": [{ "type": "text", "text": answer }],
                    "stop_reason": "end_turn",
                    "usage": { "input_tokens": 10, "output_tokens": 5 },
                },
                "timestamp": (*at + chrono::Duration::milliseconds(500)).to_rfc3339(),
            }));
        }
        self.write_claude_session(project_id, agent_id, &lines);
    }

    /// Give the agent a Claude session file holding one completed turn that
    /// edits a file `edits` times, each edit replacing 200 characters, before
    /// answering `answer`. The first message id, the hydration key, is `key`.
    pub fn seed_claude_editing_turn(
        &self,
        project_id: ProjectId,
        agent_id: AgentId,
        edits: usize,
        answer: &str,
        key: &str,
        at: chrono::DateTime<Utc>,
    ) {
        let stamp = |step: usize| {
            (at + chrono::Duration::milliseconds(i64::try_from(step).unwrap())).to_rfc3339()
        };
        let mut lines = vec![serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": "edit" },
            "timestamp": stamp(0),
        })];
        let assistant = |id: String, content: serde_json::Value, stop: &str, step: usize| {
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "id": id,
                    "model": "claude-sonnet-4-6",
                    "role": "assistant",
                    "content": content,
                    "stop_reason": stop,
                    "usage": { "input_tokens": 10, "output_tokens": 5 },
                },
                "timestamp": stamp(step),
            })
        };
        for edit in 0..edits {
            let id = if edit == 0 {
                key.to_owned()
            } else {
                format!("{key}-{edit}")
            };
            let tool = format!("toolu_{edit}");
            let (old, new) = ("o".repeat(200), "n".repeat(200));
            lines.push(assistant(
                id,
                serde_json::json!([{
                    "type": "tool_use",
                    "id": tool,
                    "name": "Edit",
                    "input": { "file_path": "/w/src/lib.rs", "old_string": old, "new_string": new },
                }]),
                "tool_use",
                1 + edit * 2,
            ));
            lines.push(serde_json::json!({
                "type": "user",
                "message": { "role": "user", "content": [{
                    "type": "tool_result",
                    "tool_use_id": tool,
                    "content": "The file /w/src/lib.rs has been updated successfully.",
                }] },
                "timestamp": stamp(2 + edit * 2),
                "toolUseResult": { "filePath": "/w/src/lib.rs", "oldString": old, "newString": new },
            }));
        }
        lines.push(assistant(
            format!("{key}-answer"),
            serde_json::json!([{ "type": "text", "text": answer }]),
            "end_turn",
            1 + edits * 2,
        ));
        self.write_claude_session(project_id, agent_id, &lines);
    }

    fn write_claude_session(
        &self,
        project_id: ProjectId,
        agent_id: AgentId,
        lines: &[serde_json::Value],
    ) {
        let path = self.new_claude_session(project_id, agent_id);
        let body: String = lines
            .iter()
            .map(|l| serde_json::to_string(l).unwrap() + "\n")
            .collect();
        std::fs::write(&path, body).unwrap();
    }

    pub fn journal_path(&self, project_id: ProjectId) -> PathBuf {
        self.state
            .store
            .root()
            .join("projects")
            .join(project_id.to_string())
            .join("journal.jsonl")
    }
}

fn build_state(
    store_root: &Path,
    adapter: &Arc<ScriptedAdapter>,
    library: Option<PathBuf>,
) -> (Arc<AppState>, Arc<RecordingEmitter>, Arc<StubDesktopSync>) {
    let emitter = Arc::new(RecordingEmitter::new());
    let sync = Arc::new(StubDesktopSync::default());
    let mut state = AppState::new_for_test_at(
        store_root,
        Arc::clone(adapter) as Arc<dyn HarnessAdapter>,
        Arc::clone(adapter) as Arc<dyn HarnessAdapter>,
        Arc::clone(adapter) as Arc<dyn HarnessAdapter>,
        Arc::clone(&emitter) as Arc<dyn EventEmitter>,
    )
    .with_desktop_sync(Arc::clone(&sync) as Arc<dyn DesktopSync>);
    if let Some(library) = library {
        let prompts_dir = library.join("prompts");
        let workflows_dir = library.join("workflows");
        std::fs::create_dir_all(&prompts_dir).unwrap();
        std::fs::create_dir_all(&workflows_dir).unwrap();
        let service = switchboard_prompts::PromptService::new(
            library.join("config.yaml"),
            prompts_dir,
            None,
            Arc::new(switchboard_prompts::InMemorySecretStore::new()),
        );
        state = state
            .with_prompts(service)
            .with_workflows_dir(workflows_dir);
    }
    (Arc::new(state), emitter, sync)
}

/// One completed Claude turn as the session file records it, stamped now so
/// it falls inside any journaled send's dispatch window.
pub fn claude_session_jsonl(prompt: &str, text: &str, key: &str) -> String {
    let now = Utc::now();
    [
        serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": prompt },
            "timestamp": now.to_rfc3339(),
        }),
        serde_json::json!({
            "type": "assistant",
            "message": {
                "id": key,
                "model": "claude-sonnet-4-6",
                "role": "assistant",
                "content": [{ "type": "text", "text": text }],
                "stop_reason": "end_turn",
                "usage": { "input_tokens": 10, "output_tokens": 5 },
            },
            "timestamp": (now + chrono::Duration::milliseconds(1)).to_rfc3339(),
        }),
    ]
    .iter()
    .map(|r| serde_json::to_string(r).unwrap())
    .collect::<Vec<_>>()
    .join("\n")
        + "\n"
}

fn append_claude_turn(path: &Path, prompt: &str, text: &str, key: &str) {
    use std::io::Write as _;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(claude_session_jsonl(prompt, text, key).as_bytes());
    }
}

/// Poll until the agent's turn is running (a send is accepted before its
/// turn starts), or panic.
pub async fn await_turn_running(state: &AppState, agent_id: AgentId) {
    for _ in 0..500 {
        if state.dispatcher.running_turn_kind(agent_id).await.is_some() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("agent {agent_id} never started its turn");
}
