//! The tools: one per `ops` operation, under the operation's own name.
//!
//! **One set of guidance, for every client.** The server instructions
//! describe the orchestration loop once; each tool's description states its
//! own rules (ids, `on_busy`, retries, the call-again loop, the result
//! budget). Where clients differ, the `wait_*` descriptions carry a hint per
//! client, and the server enforces every limit in [`WaitBudget`], so a model
//! that ignores a hint makes more calls, never a call it cannot recover from.
//!
//! **Arguments are parsed here, not by `rmcp`.** A malformed argument is a
//! tool result with `invalid_argument` and the parser's message, which the
//! client shows its model, rather than a protocol error, which some clients
//! swallow. Unknown fields are refused, so a misspelt `on_busy` cannot
//! silently fall back to its default.
//!
//! **Every result goes through the budget.** Each tool's result is fitted to
//! [`RESULT_BUDGET_CHARS`] with the rules that live beside its type in `ops`
//! (a fan-out's replies cut evenly, a conversation's oldest items dropped);
//! a result type with no rules of its own is never cut, only refused if it
//! cannot fit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rmcp::handler::server::common::schema_for_type;
use rmcp::model::{JsonObject, Meta, Tool, ToolAnnotations};
use rmcp::schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use switchboard_core::{AgentId, HarnessKind, ProjectId, SendId};
use switchboard_workflow::InputValue;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::commands::ForwardSourceRef;
use crate::error::AppError;
use crate::ops::RESULT_BUDGET_CHARS;
use crate::ops::budget::{BudgetSpec, Budgeted, fit};
use crate::ops::change::{self, SelectionChange};
use crate::ops::handle::AppStateHandle;
use crate::ops::read::{self, ReadFrom};
use crate::ops::send::{
    self, Body, ForwardPrompt, ForwardRequest, OnBusyPolicy, SendRequest, wait_send_budget,
};
use crate::ops::usage;
use crate::ops::wait::{ProgressHook, WaitBudget};
use crate::ops::workflow::{self, RunWorkflowRequest};
use crate::state::AppState;

/// The orchestration loop, sent once when a client connects.
pub const INSTRUCTIONS: &str = "\
Switchboard runs AI coding agents (Claude Code, Codex, Antigravity) on this Mac, grouped into \
projects; each project has a working folder and any number of agents. These tools direct them \
the way the user does from Switchboard's window. Every tool takes ids: call list_projects, then \
list_agents, and use the ids they return (names are for display and can change).

The loop: send a message to one or more agents with send (or pass agents' latest answers to \
others with forward). It returns a send_id at once. Call wait_send with that id until done is \
true; each call waits up to its timeout and returns the current state either way, so call it \
again. The replies come back in wait_send's final answer. read_conversation shows a project's \
whole history; read_agent_output shows an agent's latest answer, including turns you did not \
send.

Results are JSON: {\"result\": …, \"truncated\": […], \"items_omitted\": n}. A result is kept \
under 100,000 characters: long text may be cut, and each cut is listed in truncated with the \
call that reads the rest (read_with), when there is one.

The agents run without approval prompts and act on the user's files. The user sees every \
message you send, queued or running, in Switchboard's window and can cancel it there.";

/// How Claude Code is told to show a result directly rather than save one
/// over 50,000 characters to a file: every result is under this.
const MAX_RESULT_SIZE_META: &str = "anthropic/maxResultSizeChars";

/// Hints shared by every blocking tool.
const WAIT_HINTS: &str = "\
Timeouts: timeout_seconds defaults to 50. If your client sends a progress token with the call, \
you may ask for up to 3600 and get a progress notification every 60 seconds; without one, waits \
are capped at 50. In Claude Code, ask for a long timeout (e.g. 1800): the call moves to the \
background and its result arrives on its own when it settles. In Codex, stay under 300 seconds \
unless the user raised tool_timeout_sec. Otherwise use the default and call again.";

// --- the catalog ---------------------------------------------------------------

macro_rules! tools {
    ($( $variant:ident = $name:literal ($params:ty) [$kind:ident] $description:expr; )*) => {
        /// Every tool, by name.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum ToolName { $($variant,)* }

        impl ToolName {
            pub const ALL: &[ToolName] = &[$(ToolName::$variant,)*];

            #[must_use]
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $name,)* }
            }

            #[must_use]
            pub fn parse(name: &str) -> Option<Self> {
                match name { $($name => Some(Self::$variant),)* _ => None }
            }

            fn description(self) -> String {
                match self { $(Self::$variant => $description.to_string(),)* }
            }

            fn schema(self) -> Arc<JsonObject> {
                match self { $(Self::$variant => schema_for_type::<$params>(),)* }
            }

            fn kind(self) -> ToolKind {
                match self { $(Self::$variant => ToolKind::$kind,)* }
            }

            /// Whether a stopping server still takes this call. Only the
            /// cancels: they stop work and start none, and refusing one would
            /// leave running the very work a client was trying to stop.
            #[must_use]
            pub fn allowed_while_stopping(self) -> bool {
                self.kind() == ToolKind::Stops
            }
        }
    };
}

/// What a tool does to the Mac, for the clients' approval prompts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolKind {
    /// Reads, waits: changes nothing.
    Read,
    /// Creates or renames; nothing is lost.
    Additive,
    /// Starts agents or deletes a run's record.
    Acts,
    /// Stops work and starts none: the cancels.
    Stops,
}

tools! {
    ListProjects = "list_projects" (NoArgs) [Read]
        "Every project: id, name, working folder, whether the folder exists, and status (busy \
         when an agent has running or queued work or a workflow is running, else idle). Start \
         here: every other tool takes a project_id.";
    ListAgents = "list_agents" (ProjectArgs) [Read]
        "A project's agents: id, name, harness, model, effort, the quick-choice lists of models \
         and efforts, busy, and context. busy is an observation: the user or a workflow can occupy \
         an idle agent before your next send. context is the agent's context fullness \
         ({used_tokens, window_tokens, fraction}) after its newest finished turn, or after a newer \
         compaction, the same figure Switchboard's sidebar shows; null when nothing has reported \
         it or the harness reports no window (Antigravity). It is the latest Switchboard observed: \
         a turn the agent ran outside Switchboard (continued in a terminal) shows after \
         Switchboard's next turn for it or the window's next reread. context_pending is true \
         while the agent's session file is still being read for it: call again.";
    GetUsage = "get_usage" (NoArgs) [Read]
        "How much of each harness account's limits is used, for choosing a model and effort for \
         the work at hand. One entry per account the app has a reading for: {harness, \
         observed_at, ordinary_usage_allowed, plan, windows}, each window {key, scope, model, \
         used_fraction (0 to 1), resets_at, duration_minutes, observed_at, is_using_overage, \
         credits}, fields a reading lacks left out. scope is account (ordinary work on any model) \
         or model (a reserve for one model or a set, named in model when known): do not read a \
         reserve's headroom as the allowance governing ordinary work. ordinary_usage_allowed and \
         is_using_overage are observed facts, not permission to incur charges. Freshness: Claude \
         reports its limits only with a reply, so its reading is as old as the newest Claude \
         reply in the app (each window's observed_at says); Codex is read from the account on \
         request, unless a reading under a minute old is held or a read failed under a minute \
         ago, and when the read does not finish in time the newest stored reading is returned \
         with its own observed_at. Empty when no account has reported.";
    ReadConversation = "read_conversation" (ReadConversationArgs) [Read]
        "A project's conversation in time order: each message sent (text and recipients), each \
         agent turn's answer text with its send_id and status (complete, running, failed, \
         cancelled), failures with their reason, and a compacted marker where an agent's context \
         was compacted (it may have lost earlier instructions there). Tool calls and thinking are \
         left out; the answer says what the agent did. last_n_items keeps only the newest items. \
         When the whole would pass 100,000 characters, older items are left out whole \
         (items_omitted says how many); a single answer too long on its own is cut, and \
         truncated names the read_send_output call that reads on.";
    ReadSendOutput = "read_send_output" (ReadSendOutputArgs) [Read]
        "One recipient's reply to one send, a page at a time, starting at offset (in \
         characters). The way to read a reply that another result cut: pass the arguments its \
         truncated entry's read_with gives, read_from included. next_offset is where the next \
         page starts and is absent on the last page. Also reads a reply still running (the text \
         so far), failed, or cancelled.";
    ReadAgentOutput = "read_agent_output" (AgentArgs) [Read]
        "An agent's latest completed answer, including turns you did not send (the user's, a \
         workflow's), or the reason its latest turn failed or was cancelled. Carries the send_id \
         when known, so a long answer can be paged with read_send_output. To wait for a running \
         agent first, use wait_agent.";
    ListPrompts = "list_prompts" (NoArgs) [Read]
        "The user's saved prompts, local and from prompt servers: provider, name, description, \
         and arguments. Use one in send or forward as prompt: {provider, name, args}.";
    ListWorkflows = "list_workflows" (NoArgs) [Read]
        "The user's workflows: name, is_builtin, description, declared inputs, and invocable \
         (false when a step needs the user at the Mac). Call describe_workflow before \
         run_workflow: it also lists the arguments the workflow's saved prompts need.";
    DescribeWorkflow = "describe_workflow" (DescribeWorkflowArgs) [Read]
        "Everything run_workflow needs for one workflow: its declared inputs (an agent input \
         takes an agent id, a list input a list) and the arguments derived from its saved \
         prompts, with which are required.";
    ListWorkflowRuns = "list_workflow_runs" (ProjectArgs) [Read]
        "A project's workflow runs: running ones, and failed or interrupted ones still held. A \
         held run blocks every new workflow in the project until dismiss_workflow_run removes \
         it.";
    CreateProject = "create_project" (CreateProjectArgs) [Additive]
        "Create a project for a folder on this Mac. It starts with no agents; add them with \
         create_agent. Avoid the folder this session runs in: the agents Switchboard launches \
         there would read the same MCP configuration and get these tools too.";
    RenameProject = "rename_project" (RenameProjectArgs) [Additive]
        "Rename a project. Its id does not change.";
    CreateAgent = "create_agent" (CreateAgentArgs) [Additive]
        "Add an agent to a project. harness is claude_code, codex, or antigravity. The model and \
         effort start from the user's defaults for that harness; pass either to override it, in \
         the harness's own naming. A value the harness does not know is not caught here: the \
         agent's next turn fails with the harness's error. list_agents shows the quick-choice \
         lists of known values.";
    RenameAgent = "rename_agent" (RenameAgentArgs) [Additive]
        "Rename an agent. Its id does not change.";
    SetAgentSelection = "set_agent_selection" (SetAgentSelectionArgs) [Additive]
        "Change an agent's model or effort; it applies from the agent's next turn. Omit a field \
         to leave it as it is; pass null to reset it to the harness's default.";
    DismissWorkflowRun = "dismiss_workflow_run" (RunArgs) [Acts]
        "Delete a failed or interrupted workflow run's record, which otherwise blocks every new \
         workflow in the project. Returns the step that failed and why, which this removes from \
         the window, so read it. Refuses a running or completed run.";
    Send = "send" (SendArgs) [Acts]
        "Send one message to one or more agents of a project. Returns at once with a send_id and \
         each recipient's status at dispatch: accepted, rejected (with a code), or \
         not_dispatched. Then call wait_send until done.\n\
         The message is text, or a saved prompt ({provider, name, args}) with text appended \
         after it.\n\
         on_busy: refuse (the default) sends to no one if any recipient is busy; if one becomes \
         busy during dispatch it is rejected, the recipients before it keep running, and the \
         ones after it are not_dispatched. queue lines the message up behind the running turn \
         instead.\n\
         idempotency_key: pass one whenever you might retry. A retry with the same key and the \
         same request returns the original receipt rather than sending twice; the same key with \
         a different request is invalid_argument. A retry that finds the original still running \
         returns request_in_progress with its send_id: call again later with the same key.\n\
         The user sees the message in Switchboard's window, queued or running, and can cancel \
         it there.";
    Forward = "forward" (ForwardArgs) [Acts]
        format!(
            "Send agents' latest answers to other agents, composed the way the user's own \
             forwards are: your text first, then each source's latest answer in a labeled block. \
             Sources may be in other projects. Prefer this to copying a reply into send. A \
             source still mid-turn is waited for, up to timeout_seconds; if it is still running \
             then, the forward fails with source_busy and nothing is sent: call wait_agent on the \
             source, then retry. With a saved prompt, forward_args fills a prompt argument from \
             sources instead. on_busy, idempotency_key, and the receipt work as in send.\n\
             {WAIT_HINTS}"
        );
    RunWorkflow = "run_workflow" (RunWorkflowArgs) [Acts]
        "Start a workflow in a project and return its run_id; then call wait_workflow until \
         done. Pass inputs as describe_workflow lists them: an agent input takes an agent id, a \
         list input a list. forward_sources fills an input with agents' latest answers. The run \
         reports only its status: read outputs afterwards with read_agent_output. Refused with \
         workflow_run_requires_dismissal while a failed run is held in the project.";
    CancelSend = "cancel_send" (SendIdArgs) [Stops]
        "Cancel a send: every recipient still queued or running is cancelled, and a send still \
         being prepared stops before reaching anyone else. Works for sends made since the app \
         last started.";
    CancelAgent = "cancel_agent" (AgentArgs) [Stops]
        "Stop an agent: cancel its running turn and drop every message queued for it, including \
         ones the user or a workflow queued.";
    CancelWorkflowRun = "cancel_workflow_run" (RunArgs) [Stops]
        "Cancel a running workflow run.";
    WaitSend = "wait_send" (WaitSendArgs) [Read]
        format!(
            "Wait for a send's replies. Returns when every recipient has finished or \
             timeout_seconds passes, with the current state either way: call again until done \
             is true. While not done, recipients carry statuses only; reply text comes with \
             done. Statuses: accepted (queued), running, completed, failed, cancelled, \
             rejected, not_dispatched; after an app restart, started or unknown, read from \
             the journal. Long replies share the result evenly, each cut one listed in \
             truncated with the read_send_output call that reads on.\n{WAIT_HINTS}"
        );
    WaitAgent = "wait_agent" (WaitAgentArgs) [Read]
        format!(
            "Wait until an agent would accept a new send, then return idle with its latest \
             output; or running once timeout_seconds passes (call again). idle is an \
             observation: the user or a workflow can occupy the agent before your next send, \
             which then returns agent_busy.\n{WAIT_HINTS}"
        );
    WaitWorkflow = "wait_workflow" (WaitWorkflowArgs) [Read]
        format!(
            "Wait for a workflow run to finish. Returns running with the current step once \
             timeout_seconds passes (call again), or the final status (completed, failed, \
             cancelled, interrupted) with the step that failed and why.\n{WAIT_HINTS}"
        );
}

/// Every tool as `tools/list` reports it.
#[must_use]
pub fn catalog() -> Vec<Tool> {
    ToolName::ALL
        .iter()
        .map(|tool| {
            let mut meta = Meta::new();
            meta.0.insert(
                MAX_RESULT_SIZE_META.to_owned(),
                Value::from(RESULT_BUDGET_CHARS),
            );
            let annotations = match tool.kind() {
                ToolKind::Read => ToolAnnotations::new().read_only(true),
                ToolKind::Additive => ToolAnnotations::new().read_only(false).destructive(false),
                ToolKind::Acts | ToolKind::Stops => ToolAnnotations::new().read_only(false),
            };
            Tool::new(tool.as_str(), tool.description(), tool.schema())
                .with_annotations(annotations)
                .with_meta(meta)
        })
        .collect()
}

// --- arguments -------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct NoArgs {}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct ProjectArgs {
    #[schemars(with = "String")]
    project_id: ProjectId,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct AgentArgs {
    #[schemars(with = "String")]
    project_id: ProjectId,
    #[schemars(with = "String")]
    agent_id: AgentId,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct ReadConversationArgs {
    #[schemars(with = "String")]
    project_id: ProjectId,
    /// Keep only this many of the newest items.
    #[serde(default)]
    last_n_items: Option<usize>,
}

/// Which copy of a reply to page.
#[derive(Deserialize, JsonSchema, Default, Clone, Copy)]
#[schemars(crate = "rmcp::schemars")]
#[serde(rename_all = "snake_case")]
enum ReadFromArg {
    /// The text captured as the reply streamed when the app holds it, else
    /// the agent's session file. What `wait_send` and `read_agent_output` show.
    #[default]
    Auto,
    /// The agent's session file, as `read_conversation` shows the reply.
    Transcript,
}

impl From<ReadFromArg> for ReadFrom {
    fn from(arg: ReadFromArg) -> Self {
        match arg {
            ReadFromArg::Auto => Self::Auto,
            ReadFromArg::Transcript => Self::Transcript,
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct ReadSendOutputArgs {
    #[schemars(with = "String")]
    project_id: ProjectId,
    #[schemars(with = "String")]
    send_id: SendId,
    #[schemars(with = "String")]
    agent_id: AgentId,
    /// Where the page starts, in characters. 0 by default.
    #[serde(default)]
    offset: usize,
    /// Which copy to page: keep to the one a cut's `read_with` names, because
    /// an offset only holds in the copy it came from.
    #[serde(default)]
    read_from: ReadFromArg,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct DescribeWorkflowArgs {
    #[schemars(with = "String")]
    project_id: ProjectId,
    name: String,
    /// Whether the workflow is one of Switchboard's built-ins, as
    /// `list_workflows` reports it.
    #[serde(default)]
    is_builtin: bool,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct CreateProjectArgs {
    name: String,
    /// An existing folder on this Mac, as an absolute path.
    directory: String,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct RenameProjectArgs {
    #[schemars(with = "String")]
    project_id: ProjectId,
    name: String,
}

#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[schemars(crate = "rmcp::schemars")]
#[serde(rename_all = "snake_case")]
enum HarnessArg {
    ClaudeCode,
    Codex,
    Antigravity,
}

impl From<HarnessArg> for HarnessKind {
    fn from(arg: HarnessArg) -> Self {
        match arg {
            HarnessArg::ClaudeCode => Self::ClaudeCode,
            HarnessArg::Codex => Self::Codex,
            HarnessArg::Antigravity => Self::Antigravity,
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct CreateAgentArgs {
    #[schemars(with = "String")]
    project_id: ProjectId,
    name: String,
    harness: HarnessArg,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct RenameAgentArgs {
    #[schemars(with = "String")]
    project_id: ProjectId,
    #[schemars(with = "String")]
    agent_id: AgentId,
    name: String,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct SetAgentSelectionArgs {
    #[schemars(with = "String")]
    project_id: ProjectId,
    #[schemars(with = "String")]
    agent_id: AgentId,
    /// The model, in the harness's own naming. Omit to keep; null resets to
    /// the harness's default.
    #[serde(default)]
    #[schemars(with = "Option<String>")]
    model: SelectionChange,
    /// The effort, in the harness's own naming. Omit to keep; null resets to
    /// the harness's default.
    #[serde(default)]
    #[schemars(with = "Option<String>")]
    effort: SelectionChange,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct RunArgs {
    #[schemars(with = "String")]
    project_id: ProjectId,
    #[schemars(with = "String")]
    run_id: Uuid,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct SendIdArgs {
    #[schemars(with = "String")]
    project_id: ProjectId,
    #[schemars(with = "String")]
    send_id: SendId,
}

/// What to do with a recipient that is busy.
#[derive(Deserialize, JsonSchema, Default, Clone, Copy)]
#[schemars(crate = "rmcp::schemars")]
#[serde(rename_all = "snake_case")]
enum OnBusyArg {
    /// Send to no one if any recipient is busy; stop at the first refusal.
    #[default]
    Refuse,
    /// Queue behind the running turn.
    Queue,
}

impl From<OnBusyArg> for OnBusyPolicy {
    fn from(arg: OnBusyArg) -> Self {
        match arg {
            OnBusyArg::Refuse => Self::Refuse,
            OnBusyArg::Queue => Self::Queue,
        }
    }
}

/// A saved prompt, as `list_prompts` names it.
#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct PromptArg {
    provider: String,
    name: String,
    #[serde(default)]
    args: BTreeMap<String, String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct SendArgs {
    #[schemars(with = "String")]
    project_id: ProjectId,
    /// Agent ids, all in this project. At least one; none repeated.
    #[schemars(with = "Vec<String>")]
    recipients: Vec<AgentId>,
    /// The message; with a prompt, the text appended after it.
    #[serde(default)]
    text: Option<String>,
    /// A saved prompt to send, rendered with its args.
    #[serde(default)]
    prompt: Option<PromptArg>,
    #[serde(default)]
    on_busy: OnBusyArg,
    /// Any string unique to this request; a retry passes the same one.
    #[serde(default)]
    idempotency_key: Option<String>,
}

/// An agent whose latest answer is forwarded. It may be in another project.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct SourceArg {
    #[schemars(with = "String")]
    project_id: ProjectId,
    #[schemars(with = "String")]
    agent_id: AgentId,
}

impl From<SourceArg> for ForwardSourceRef {
    fn from(arg: SourceArg) -> Self {
        Self {
            project_id: arg.project_id,
            agent_id: arg.agent_id,
        }
    }
}

fn sources(args: Vec<SourceArg>) -> Vec<ForwardSourceRef> {
    args.into_iter().map(ForwardSourceRef::from).collect()
}

fn source_map(args: BTreeMap<String, Vec<SourceArg>>) -> BTreeMap<String, Vec<ForwardSourceRef>> {
    args.into_iter()
        .map(|(name, list)| (name, sources(list)))
        .collect()
}

/// A saved prompt in a forward.
#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ForwardPromptArg {
    provider: String,
    name: String,
    #[serde(default)]
    args: BTreeMap<String, String>,
    /// Prompt arguments filled from sources: each named argument becomes its
    /// typed text (from args) followed by the sources' answers.
    #[serde(default)]
    forward_args: BTreeMap<String, Vec<SourceArg>>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct ForwardArgs {
    #[schemars(with = "String")]
    project_id: ProjectId,
    /// Agents whose latest answers are appended after the text.
    #[serde(default)]
    sources: Vec<SourceArg>,
    /// Agent ids in this project. At least one; none repeated.
    #[schemars(with = "Vec<String>")]
    recipients: Vec<AgentId>,
    /// Text placed before the forwarded answers.
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    prompt: Option<ForwardPromptArg>,
    #[serde(default)]
    on_busy: OnBusyArg,
    #[serde(default)]
    idempotency_key: Option<String>,
    /// Seconds to wait for a source still mid-turn. 50 by default.
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

/// A workflow input: a string, or a list of strings for a list input.
#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(untagged)]
enum InputArg {
    Text(String),
    List(Vec<String>),
}

impl From<InputArg> for InputValue {
    fn from(arg: InputArg) -> Self {
        match arg {
            InputArg::Text(text) => Self::Text(text),
            InputArg::List(list) => Self::List(list),
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct RunWorkflowArgs {
    #[schemars(with = "String")]
    project_id: ProjectId,
    name: String,
    #[serde(default)]
    is_builtin: bool,
    /// Declared inputs and prompt arguments by name. Agent inputs take
    /// agent ids.
    #[serde(default)]
    inputs: BTreeMap<String, InputArg>,
    /// Inputs filled with agents' latest answers, by input name.
    #[serde(default)]
    forward_sources: BTreeMap<String, Vec<SourceArg>>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct WaitSendArgs {
    #[schemars(with = "String")]
    project_id: ProjectId,
    #[schemars(with = "String")]
    send_id: SendId,
    /// Seconds to wait before returning the current state. 50 by default.
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct WaitAgentArgs {
    #[schemars(with = "String")]
    project_id: ProjectId,
    #[schemars(with = "String")]
    agent_id: AgentId,
    /// Seconds to wait before returning the current state. 50 by default.
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct WaitWorkflowArgs {
    #[schemars(with = "String")]
    project_id: ProjectId,
    #[schemars(with = "String")]
    run_id: Uuid,
    /// Seconds to wait before returning the current state. 50 by default.
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

fn parse<P: DeserializeOwned>(args: JsonObject) -> Result<P, AppError> {
    serde_json::from_value(Value::Object(args))
        .map_err(|e| AppError::InvalidArgument(format!("invalid arguments: {e}")))
}

// --- calling ---------------------------------------------------------------------

/// What one tool call runs with.
pub struct CallContext {
    pub handle: Arc<dyn AppStateHandle>,
    pub home: PathBuf,
    /// The client sent a progress token, so progress notifications can keep
    /// the call alive past 50 seconds.
    pub can_keep_alive: bool,
    /// The server's shutdown: ends any wait at once.
    pub cancel: CancellationToken,
    pub progress: Option<ProgressHook>,
}

impl CallContext {
    fn state(&self) -> &AppState {
        self.handle.app_state()
    }

    fn home(&self) -> &Path {
        &self.home
    }

    /// A budget whose deadline starts now.
    fn budget(&self, timeout_seconds: Option<u64>) -> WaitBudget {
        let budget =
            WaitBudget::new(timeout_seconds, self.can_keep_alive).with_cancel(self.cancel.clone());
        match &self.progress {
            Some(hook) => budget.with_progress(Arc::clone(hook)),
            None => budget,
        }
    }
}

/// A tool's result and the rules for fitting it to the budget.
pub struct Reply {
    value: Value,
    spec: Option<BudgetSpec>,
}

impl Reply {
    fn plain<T: Serialize>(value: &T) -> Result<Self, AppError> {
        Ok(Self {
            value: to_value(value)?,
            spec: None,
        })
    }

    fn fitted<T: Serialize>(value: &T, spec: BudgetSpec) -> Result<Self, AppError> {
        Ok(Self {
            value: to_value(value)?,
            spec: Some(spec),
        })
    }

    /// Fit the result to the budget. A result type without rules of its own
    /// has no field that may be cut, so it fits whole or is refused, without
    /// advice to read it in pages it does not have.
    pub fn fit(self) -> Result<Budgeted, AppError> {
        let spec = self
            .spec
            .unwrap_or_else(|| BudgetSpec::whole(RESULT_BUDGET_CHARS));
        fit(self.value, &spec)
    }
}

fn to_value<T: Serialize>(value: &T) -> Result<Value, AppError> {
    serde_json::to_value(value)
        .map_err(|e| AppError::Internal(format!("could not serialize a result: {e}")))
}

/// Run one tool.
#[allow(clippy::too_many_lines)] // One arm per tool: a flat table, linear.
pub async fn call(tool: ToolName, args: JsonObject, cx: &CallContext) -> Result<Reply, AppError> {
    let state = cx.state();
    match tool {
        ToolName::ListProjects => {
            parse::<NoArgs>(args)?;
            Reply::plain(&read::list_projects(state, &cx.cancel).await?)
        }
        ToolName::ListAgents => {
            let a: ProjectArgs = parse(args)?;
            Reply::plain(&read::list_agents(state, a.project_id, cx.home(), &cx.cancel).await?)
        }
        ToolName::GetUsage => {
            parse::<NoArgs>(args)?;
            Reply::plain(
                &usage::get_usage(Arc::clone(&cx.handle), usage::CODEX_READ_BUDGET, &cx.cancel)
                    .await?,
            )
        }
        ToolName::ReadConversation => {
            let a: ReadConversationArgs = parse(args)?;
            let digest =
                read::read_conversation(state, a.project_id, a.last_n_items, cx.home()).await?;
            Reply::fitted(&digest, read::conversation_budget(a.project_id))
        }
        ToolName::ReadSendOutput => {
            let a: ReadSendOutputArgs = parse(args)?;
            Reply::plain(
                &read::read_send_output(
                    state,
                    a.project_id,
                    a.send_id,
                    a.agent_id,
                    a.offset,
                    a.read_from.into(),
                    cx.home(),
                )
                .await?,
            )
        }
        ToolName::ReadAgentOutput => {
            let a: AgentArgs = parse(args)?;
            let output =
                read::read_agent_output(state, a.project_id, a.agent_id, cx.home()).await?;
            Reply::fitted(&output, read::agent_output_budget(a.project_id))
        }
        ToolName::ListPrompts => {
            parse::<NoArgs>(args)?;
            Reply::plain(&read::list_prompts(state))
        }
        ToolName::ListWorkflows => {
            parse::<NoArgs>(args)?;
            Reply::plain(&read::list_workflows(state))
        }
        ToolName::DescribeWorkflow => {
            let a: DescribeWorkflowArgs = parse(args)?;
            Reply::plain(
                &workflow::describe_workflow(state, a.project_id, &a.name, a.is_builtin).await?,
            )
        }
        ToolName::ListWorkflowRuns => {
            let a: ProjectArgs = parse(args)?;
            Reply::plain(&workflow::list_workflow_runs(state, a.project_id)?)
        }
        ToolName::CreateProject => {
            let a: CreateProjectArgs = parse(args)?;
            Reply::plain(&change::create_project(state, &a.name, &a.directory)?)
        }
        ToolName::RenameProject => {
            let a: RenameProjectArgs = parse(args)?;
            Reply::plain(&change::rename_project(state, a.project_id, &a.name)?)
        }
        ToolName::CreateAgent => {
            let a: CreateAgentArgs = parse(args)?;
            Reply::plain(&change::create_agent(
                state,
                a.project_id,
                &a.name,
                a.harness.into(),
                a.model,
                a.effort,
            )?)
        }
        ToolName::RenameAgent => {
            let a: RenameAgentArgs = parse(args)?;
            Reply::plain(&change::rename_agent(
                state,
                a.project_id,
                a.agent_id,
                &a.name,
            )?)
        }
        ToolName::SetAgentSelection => {
            let a: SetAgentSelectionArgs = parse(args)?;
            Reply::plain(&change::set_agent_selection(
                state,
                a.project_id,
                a.agent_id,
                &a.model,
                &a.effort,
            )?)
        }
        ToolName::DismissWorkflowRun => {
            let a: RunArgs = parse(args)?;
            Reply::plain(&change::dismiss_workflow_run(
                state,
                a.project_id,
                a.run_id,
            )?)
        }
        ToolName::Send => {
            let a: SendArgs = parse(args)?;
            let body = match (a.prompt, a.text) {
                (Some(prompt), appended_text) => Body::Prompt {
                    provider: prompt.provider,
                    name: prompt.name,
                    args: prompt.args,
                    appended_text,
                },
                (None, Some(text)) => Body::Text { text },
                (None, None) => {
                    return Err(AppError::InvalidArgument(
                        "a send needs text, a prompt, or both".to_owned(),
                    ));
                }
            };
            let request = SendRequest {
                project_id: a.project_id,
                recipients: a.recipients,
                body,
                on_busy: a.on_busy.into(),
                idempotency_key: a.idempotency_key,
            };
            let receipt = send::send(
                Arc::clone(&cx.handle),
                cx.home.clone(),
                request,
                cx.budget(None),
            )
            .await?;
            Reply::plain(&receipt)
        }
        ToolName::Forward => {
            let a: ForwardArgs = parse(args)?;
            let request = ForwardRequest {
                project_id: a.project_id,
                sources: sources(a.sources),
                recipients: a.recipients,
                text: a.text,
                prompt: a.prompt.map(|p| ForwardPrompt {
                    provider: p.provider,
                    name: p.name,
                    args: p.args,
                    forward_args: source_map(p.forward_args),
                }),
                on_busy: a.on_busy.into(),
                idempotency_key: a.idempotency_key,
            };
            let receipt = send::forward(
                Arc::clone(&cx.handle),
                cx.home.clone(),
                request,
                cx.budget(a.timeout_seconds),
            )
            .await?;
            Reply::plain(&receipt)
        }
        ToolName::RunWorkflow => {
            let a: RunWorkflowArgs = parse(args)?;
            let request = RunWorkflowRequest {
                project_id: a.project_id,
                name: a.name,
                is_builtin: a.is_builtin,
                inputs: a
                    .inputs
                    .into_iter()
                    .map(|(name, value)| (name, value.into()))
                    .collect(),
                forward_sources: source_map(a.forward_sources),
            };
            let run_id =
                workflow::run_workflow(state, cx.home(), request, &cx.budget(None)).await?;
            Reply::plain(&serde_json::json!({ "run_id": run_id }))
        }
        ToolName::CancelSend => {
            let a: SendIdArgs = parse(args)?;
            Reply::plain(&send::cancel_send(state, a.project_id, a.send_id)?)
        }
        ToolName::CancelAgent => {
            let a: AgentArgs = parse(args)?;
            Reply::plain(&send::cancel_agent(state, a.project_id, a.agent_id)?)
        }
        ToolName::CancelWorkflowRun => {
            let a: RunArgs = parse(args)?;
            workflow::cancel_workflow_run(state, a.project_id, a.run_id)?;
            Reply::plain(&serde_json::json!({ "run_id": a.run_id, "cancel_requested": true }))
        }
        ToolName::WaitSend => {
            let a: WaitSendArgs = parse(args)?;
            let budget = cx.budget(a.timeout_seconds);
            let reply = send::wait_send(state, a.project_id, a.send_id, &budget, cx.home()).await?;
            Reply::fitted(&reply, wait_send_budget(a.project_id, a.send_id))
        }
        ToolName::WaitAgent => {
            let a: WaitAgentArgs = parse(args)?;
            let budget = cx.budget(a.timeout_seconds);
            let reply =
                send::wait_agent(state, a.project_id, a.agent_id, &budget, cx.home()).await?;
            Reply::fitted(&reply, read::agent_output_budget(a.project_id))
        }
        ToolName::WaitWorkflow => {
            let a: WaitWorkflowArgs = parse(args)?;
            let budget = cx.budget(a.timeout_seconds);
            Reply::plain(&workflow::wait_workflow(state, a.project_id, a.run_id, &budget).await?)
        }
    }
}
