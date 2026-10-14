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
    TurnOutcome, TurnUsage,
};
use tempfile::TempDir;
use tokio::sync::Notify;
use uuid::Uuid;

use crate::commands::{create_project_impl, set_agent_session_locator_impl};
use crate::error::AppError;
use crate::state::{AppState, lock};

use super::desktop_sync::{DesktopSync, Hearing, ReadyReport, WindowSync};

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
    /// Context figures the turn's end reports: tokens after the turn and the
    /// window.
    pub context: Option<(u64, u32)>,
    /// Hold `dispatch` itself, before it returns its stream, until the gate
    /// fires: the agent's actor is then preparing the turn and answers no
    /// busy check.
    pub hold_dispatch: Option<Arc<Notify>>,
}

impl Script {
    pub fn reply(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            park: None,
            fail: false,
            key: None,
            ignore_cancel: false,
            context: None,
            hold_dispatch: None,
        }
    }

    pub fn holding_dispatch(self, gate: &Arc<Notify>) -> Self {
        Self {
            hold_dispatch: Some(Arc::clone(gate)),
            ..self
        }
    }

    pub fn with_context(self, used_tokens: u64, window_tokens: u32) -> Self {
        Self {
            context: Some((used_tokens, window_tokens)),
            ..self
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
/// was given, and, like the real Claude adapter, leaves a completed or
/// cancelled turn's text in the agent's session file under `home`, so disk
/// reads (forwards from an idle source, the restart fallback, the
/// conversation) see what the turn produced.
pub struct ScriptedAdapter {
    scripts: Mutex<VecDeque<Script>>,
    home: PathBuf,
    pub prompts: Mutex<Vec<String>>,
    /// The agents whose compactions ran, in order.
    pub compactions: Mutex<Vec<AgentId>>,
}

impl ScriptedAdapter {
    pub fn new(scripts: Vec<Script>, home: &Path) -> Self {
        Self {
            scripts: Mutex::new(scripts.into()),
            home: home.to_path_buf(),
            prompts: Mutex::new(Vec::new()),
            compactions: Mutex::new(Vec::new()),
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
        if let Some(gate) = &script.hold_dispatch {
            gate.notified().await;
        }
        let session_file = match &agent.session_locator {
            Some(SessionLocator::Uuid(session)) => Some(
                switchboard_harness::claude_session_file_path(&self.home, cwd, session),
            ),
            _ => None,
        };
        let prompt = prompt.to_owned();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            let key = script
                .key
                .clone()
                .unwrap_or_else(|| format!("msg-{turn_id}"));
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
                        () = options.cancel_token.cancelled() => {
                            // Claude has already written what it streamed, so a
                            // cancelled turn leaves its partial reply on disk.
                            if let Some(path) = &session_file {
                                append_claude_turn(path, &prompt, &script.text, &key);
                            }
                            return;
                        }
                    }
                }
            }
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
                usage: script.context.map(|(used, window)| TurnUsage {
                    input_tokens: used,
                    output_tokens: 0,
                    cached_input_tokens: None,
                    cache_creation_input_tokens: None,
                    context_input_tokens: Some(used),
                    context_tokens_after_turn: Some(used),
                    reasoning_output_tokens: None,
                    context_window: Some(window),
                    total_cost_usd: None,
                }),
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

    /// A compaction: the next script decides whether it parks or fails, and
    /// its end carries the compacted size (18,000 of 200,000), the figure the
    /// mock harness reports. No content, no file write.
    async fn compact(
        &self,
        agent: &AgentRecord,
        _cwd: &Path,
        turn_id: TurnId,
        options: DispatchOptions,
    ) -> Result<EventStream, DispatchError> {
        lock(&self.compactions).push(agent.id);
        let script = lock(&self.scripts)
            .pop_front()
            .unwrap_or_else(|| Script::reply("compacted"));
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            if let Some(gate) = &script.park {
                tokio::select! {
                    () = gate.notified() => {}
                    () = options.cancel_token.cancelled() => return,
                }
            }
            let outcome = if script.fail {
                TurnOutcome::Failed {
                    kind: switchboard_harness::FailureKind::HarnessError,
                    message: "scripted failure".to_owned(),
                }
            } else {
                TurnOutcome::Completed
            };
            let _ = tx.send(AdapterEvent::TurnEnd {
                turn_id,
                outcome,
                ended_at: Utc::now(),
                usage: (!script.fail).then_some(TurnUsage {
                    input_tokens: 0,
                    output_tokens: 0,
                    cached_input_tokens: None,
                    cache_creation_input_tokens: None,
                    context_input_tokens: Some(120_000),
                    context_tokens_after_turn: Some(18_000),
                    reasoning_output_tokens: None,
                    context_window: Some(200_000),
                    total_cost_usd: None,
                }),
                context_window_source: None,
                stable_message_id: None,
                first_message_id: None,
                spend: None,
                model: None,
                effort: None,
            });
        });
        Ok(Box::pin(
            tokio_stream::wrappers::UnboundedReceiverStream::new(rx),
        ))
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
    /// Who the window hears; everyone unless a test says otherwise.
    pub hearing: Mutex<Hearing>,
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
            hearing: lock(&self.hearing).clone(),
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

impl StubDesktopSync {
    /// The window hears every agent of `roster` except `unheard`.
    pub fn hear_all_but(&self, roster: &[AgentId], unheard: &[AgentId]) {
        *lock(&self.hearing) = Hearing::Only(
            roster
                .iter()
                .copied()
                .filter(|a| !unheard.contains(a))
                .collect(),
        );
    }
}

/// One `desktop_sync_project` request as the window received it.
#[derive(Debug, Clone, Copy)]
pub struct SyncRequest {
    pub request_id: Uuid,
    pub project_id: ProjectId,
}

/// The window, as the production [`WindowSync`] reaches it: records every
/// event, and hands each sync request to the test to answer.
pub struct TestWindow {
    pub events: Mutex<Vec<(String, serde_json::Value)>>,
    requests: tokio::sync::mpsc::UnboundedSender<SyncRequest>,
}

impl TestWindow {
    pub fn names(&self) -> Vec<String> {
        lock(&self.events).iter().map(|(n, _)| n.clone()).collect()
    }
}

impl EventEmitter for TestWindow {
    fn emit(&self, name: &str, payload: serde_json::Value) {
        if name == "desktop_sync_project" {
            let field = |key: &str| {
                payload[key]
                    .as_str()
                    .and_then(|v| Uuid::parse_str(v).ok())
                    .unwrap()
            };
            let _ = self.requests.send(SyncRequest {
                request_id: field("request_id"),
                project_id: field("project_id"),
            });
        }
        lock(&self.events).push((name.to_owned(), payload));
    }
}

/// A store root and an `AppState` over it. `restart` builds a fresh state
/// over the same store, which is how a test starts from "a project the window
/// has not opened": the project exists on disk and nothing in memory knows it.
pub struct TestEnv {
    pub state: Arc<AppState>,
    pub store_root: TempDir,
    pub home: TempDir,
    /// The real home directory, for a live environment whose agents write
    /// their session files there.
    home_override: Option<PathBuf>,
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
        let (state, emitter, sync) = build_state(store_root.path(), &adapter, None, None);
        Self {
            state,
            store_root,
            home,
            home_override: None,
            library,
            adapter,
            emitter,
            sync,
            dirs: Vec::new(),
            library_enabled: false,
        }
    }

    /// An environment whose Claude and Codex agents are the real CLIs, for
    /// live tests: real harnesses, the real home directory for their session
    /// files, and no scripted adapter in the way.
    pub fn live() -> Self {
        let store_root = TempDir::new().unwrap();
        let library = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        let adapter = Arc::new(ScriptedAdapter::new(Vec::new(), home.path()));
        let real_home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("HOME is set");
        let (state, emitter, sync) = build_state_with(
            store_root.path(),
            Arc::new(switchboard_harness::ClaudeCodeAdapter::new()),
            Arc::new(switchboard_harness::CodexAdapter::new()),
            Arc::clone(&adapter) as Arc<dyn HarnessAdapter>,
            None,
            None,
            true,
        );
        Self {
            state,
            store_root,
            home,
            home_override: Some(real_home),
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
    pub fn restart(self) -> Self {
        self.restart_with(None)
    }

    /// [`TestEnv::restart`] with the production round trip, over a window the
    /// test drives: each sync request arrives on the returned receiver, and
    /// the test answers it through the returned [`WindowSync`] (or does not).
    pub fn with_window(
        self,
        deadline: Duration,
    ) -> (
        Self,
        Arc<WindowSync>,
        Arc<TestWindow>,
        tokio::sync::mpsc::UnboundedReceiver<SyncRequest>,
    ) {
        let (requests, received) = tokio::sync::mpsc::unbounded_channel();
        let window = Arc::new(TestWindow {
            events: Mutex::new(Vec::new()),
            requests,
        });
        let sync = Arc::new(WindowSync::with_deadline(
            Arc::clone(&window) as Arc<dyn EventEmitter>,
            deadline,
        ));
        let env = self.restart_with(Some(Arc::clone(&sync) as Arc<dyn DesktopSync>));
        (env, sync, window, received)
    }

    fn restart_with(mut self, desktop_sync: Option<Arc<dyn DesktopSync>>) -> Self {
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
        let (state, emitter, sync) =
            build_state(self.store_root.path(), &adapter, library, desktop_sync);
        self.state = state;
        self.emitter = emitter;
        self.sync = sync;
        self.adapter = adapter;
        self
    }

    pub fn home(&self) -> PathBuf {
        self.home_override
            .clone()
            .unwrap_or_else(|| self.home.path().to_path_buf())
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
        self.write_claude_session(project_id, agent_id, &claude_turn_lines(turns));
    }

    /// Rewrite the agent's current Claude session file (the one its locator
    /// names, without minting a new session) with these turns, through the
    /// store rather than the loaded-project map, so it works after a restart.
    pub fn overwrite_claude_turns(
        &self,
        project_id: ProjectId,
        agent_id: AgentId,
        turns: &[(&str, &str, &str, chrono::DateTime<Utc>)],
    ) {
        let project = super::read::project_readonly(&self.state, project_id).unwrap();
        let agent = project
            .list_agents()
            .unwrap()
            .into_iter()
            .find(|a| a.id == agent_id)
            .unwrap();
        let Some(SessionLocator::Uuid(session)) = agent.session_locator else {
            panic!("the agent has a Claude session");
        };
        let path = switchboard_harness::claude_session_file_path(
            self.home.path(),
            &project.directory,
            &session,
        );
        let body: String = claude_turn_lines(turns)
            .iter()
            .map(|l| serde_json::to_string(l).unwrap() + "\n")
            .collect();
        std::fs::write(&path, body).unwrap();
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

    /// Point the agent at a new Claude session file holding these records.
    pub fn write_claude_session(
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

/// The state under test, with `desktop_sync` as its window when given and the
/// stub otherwise.
fn build_state(
    store_root: &Path,
    adapter: &Arc<ScriptedAdapter>,
    library: Option<PathBuf>,
    desktop_sync: Option<Arc<dyn DesktopSync>>,
) -> (Arc<AppState>, Arc<RecordingEmitter>, Arc<StubDesktopSync>) {
    build_state_with(
        store_root,
        Arc::clone(adapter) as Arc<dyn HarnessAdapter>,
        Arc::clone(adapter) as Arc<dyn HarnessAdapter>,
        Arc::clone(adapter) as Arc<dyn HarnessAdapter>,
        library,
        desktop_sync,
        false,
    )
}

fn build_state_with(
    store_root: &Path,
    claude: Arc<dyn HarnessAdapter>,
    codex: Arc<dyn HarnessAdapter>,
    antigravity: Arc<dyn HarnessAdapter>,
    library: Option<PathBuf>,
    desktop_sync: Option<Arc<dyn DesktopSync>>,
    real_harnesses: bool,
) -> (Arc<AppState>, Arc<RecordingEmitter>, Arc<StubDesktopSync>) {
    let emitter = Arc::new(RecordingEmitter::new());
    let sync = Arc::new(StubDesktopSync::default());
    let mut state = AppState::new_for_test_at(
        store_root,
        claude,
        codex,
        antigravity,
        Arc::clone(&emitter) as Arc<dyn EventEmitter>,
    )
    .with_real_harnesses(real_harnesses)
    .with_desktop_sync(desktop_sync.unwrap_or_else(|| Arc::clone(&sync) as Arc<dyn DesktopSync>));
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
pub fn claude_turn_lines(
    turns: &[(&str, &str, &str, chrono::DateTime<Utc>)],
) -> Vec<serde_json::Value> {
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
    lines
}

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

/// A local saved prompt with one required argument, `who`.
pub const GREET_PROMPT: &str = "---\nname: greet\ndescription: Greeting.\narguments:\n  - name: who\n    required: true\n---\nHi {{ who }}\n";

/// A workflow that greets `coder` with the saved prompt, then `reviewers`,
/// waiting for each.
pub const PAIR_WORKFLOW: &str = "name: pair
description: test
inputs:
  coder:
    type: agent
  reviewers:
    type: [agent]
steps:
  - label: Greet
    send:
      to: \"{{ coder }}\"
      prompt: \"local:greet\"
  - label: Greeted
    wait_for:
      agent: \"{{ coder }}\"
  - label: Review
    send:
      to: \"{{ reviewers }}\"
      prompt: \"local:greet\"
  - label: Reviewed
    wait_for_all:
      agents: \"{{ reviewers }}\"
";
