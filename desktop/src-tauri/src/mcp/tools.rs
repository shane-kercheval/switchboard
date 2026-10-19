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
use crate::ops::budget::{BudgetSpec, Budgeted, fit};
use crate::ops::change::{self, SelectionChange};
use crate::ops::compact::{self, CompactRequest};
use crate::ops::fork::{self, ForkRequest};
use crate::ops::handle::AppStateHandle;
use crate::ops::read::{self, ReadFrom};
use crate::ops::send::{
    self, Body, ForwardFrom, ForwardPrompt, ForwardRequest, OnBusyPolicy, SendRequest,
    wait_send_budget,
};
use crate::ops::usage;
use crate::ops::wait::{ProgressHook, WaitBudget};
use crate::ops::workflow::{self, RunWorkflowRequest};
use crate::ops::{RESULT_BUDGET_CHARS, output_page_chars, result_budget_chars};
use crate::state::AppState;

/// What Switchboard is and how to name things, sent first when a client
/// connects.
const INSTRUCTIONS_INTRO: &str = "\
Switchboard runs AI coding agents (Claude Code, Codex, Antigravity) on this Mac, grouped into \
projects; each project has a working folder and any number of agents. These tools direct them \
the way the user does from Switchboard's window. Every tool takes ids: call list_projects, \
then list_agents, and use the ids they return (names are for display and can change).";

/// The orchestration loop, sent after the tool list. Each tool's own rules
/// live in its description; this says only what spans tools.
const INSTRUCTIONS_LOOP: &str = "\
An agent is a persistent session of its harness, run in the project's folder with the user's \
own configuration for that harness (the folder's CLAUDE.md or AGENTS.md included) and the \
harness's full tools: it reads and edits files, runs commands, and uses git. A project's \
agents share its folder, so each sees the others' uncommitted changes. An agent receives your \
message as if the user had typed it and cannot tell you from the user. Each send continues its \
conversation: it keeps earlier sends and its own replies until its context is compacted, so a \
follow-up need not repeat what it already knows. Its reply is the text it writes; tool calls \
and thinking are not returned. These tools read none of the project's files: to learn how the \
code works, ask an agent.

The loop: send a message to one or more agents with send (or pass agents' replies to others \
with forward). It returns a send_id at once. Call wait_send with that id until done is true; \
each call waits up to its timeout and returns the current state either way. The replies come \
back in wait_send's final answer. The user's saved prompts (list_prompts, read_prompt) \
and workflows (list_workflows) are their standing instructions and procedures, such as a code \
review: use one when it fits rather than writing your own, and check list_workflows before \
running a multi-step exchange across agents by hand (review rounds, a fan-out and reconcile).

Results are JSON: {\"result\": …, \"truncated\": […], \"items_omitted\": n}, under 100,000 \
characters, or 45,000 for any result that can be cut when your client sends no progress token \
(Claude Desktop). Each cut is listed in truncated with the call that reads the rest \
(read_with), when there is one. max_chars sets the limit for one call, and the call a cut \
lists keeps it: Codex keeps about 10,000 tokens of a result and cuts the middle, so pass 25000 \
there. An error is \
{\"code\": …, \"message\": …}; the message says what to do.

Some conditions need the user at the Mac, not a retry: a turn that failed with failure_kind \
usage_limit or auth_failure, or whose error says a usage limit was reached (Claude and \
Antigravity report one as harness_error); prompt_needs_sign_in; directory_missing; \
project_locked (another Switchboard holds the project). Report them and pause the work that \
needs them. agent_busy_elsewhere means another process holds the agent's session: wait and \
resend, and report it if it persists.

The agents run without approval prompts and act on the user's files. The user sees every \
message you send, queued or running, in Switchboard's window and can cancel it there.";

/// The instructions a client receives: what Switchboard is, then every tool's
/// name grouped by what the caller is doing, then the loop. The list comes
/// early because a client that loads tool definitions on demand (Claude
/// Code's tool search) knows what exists only from it, and Claude Code keeps
/// only the start of instructions longer than its limit.
#[must_use]
pub fn instructions() -> String {
    let groups: Vec<String> = ToolGroup::ALL
        .iter()
        .map(|&group| {
            let names: Vec<&str> = ToolName::ALL
                .iter()
                .filter(|tool| tool.group() == group)
                .map(|tool| tool.as_str())
                .collect();
            format!("- {}: {}.", group.label(), names.join(", "))
        })
        .collect();
    format!(
        "{INSTRUCTIONS_INTRO}\n\nThe tools, by what you are doing:\n{}\n\n{INSTRUCTIONS_LOOP}",
        groups.join("\n")
    )
}

/// How Claude Code is told to show a result directly rather than save one
/// over 50,000 characters to a file: every result is under this.
const MAX_RESULT_SIZE_META: &str = "anthropic/maxResultSizeChars";

/// Hints shared by every blocking tool.
const WAIT_HINTS: &str = "\
Timeouts: timeout_seconds defaults to 50. If your client sends a progress token with the call, \
you may ask for up to 3600 and get a progress notification every 60 seconds; without one, waits \
are capped at 50, and the reply says so (capped_to_seconds). In the Claude Code CLI, ask for a \
long timeout (e.g. 600): the call moves to the background and its result arrives on its own \
when it settles. Claude Desktop, its Code tab included, sends no token: use the default and call \
again. In Codex, stay under 300 seconds unless the user raised tool_timeout_sec.";

/// What a finished wait's `usage` is, shared by every wait's description.
const USAGE_NOTE: &str = "\
carries usage: each harness account's limits as get_usage reports them, without each window's \
credits and duration_minutes, from the last reading Switchboard holds; the wait reads no \
account. A Claude reading is as old as the newest Claude reply anywhere in the app. A Codex \
reading is as old as Switchboard's last account read, which it starts after each Codex turn \
ends, so it can predate the turn that just finished. Each window's observed_at says. Call \
get_usage for a fresh Codex reading.";

// --- the catalog ---------------------------------------------------------------

macro_rules! tools {
    ($( $variant:ident = $name:literal ($params:ty) [$kind:ident, $group:ident] $description:expr; )*) => {
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

            fn group(self) -> ToolGroup {
                match self { $(Self::$variant => ToolGroup::$group,)* }
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

/// What a caller is doing when it reaches for a tool, for the grouped list in
/// the server instructions. Every tool names one, so a new tool cannot be left
/// out of the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolGroup {
    Look,
    SetUp,
    Act,
    Stop,
    Wait,
}

impl ToolGroup {
    const ALL: [ToolGroup; 5] = [Self::Look, Self::SetUp, Self::Act, Self::Stop, Self::Wait];

    fn label(self) -> &'static str {
        match self {
            Self::Look => "Look (changes nothing)",
            Self::SetUp => "Set up and tidy",
            Self::Act => "Start work",
            Self::Stop => "Stop work",
            Self::Wait => "Wait for work",
        }
    }
}

/// What a tool does to the Mac, for the clients' approval prompts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolKind {
    /// Reads, waits: changes nothing.
    Read,
    /// Creates or renames; nothing is lost.
    Additive,
    /// Starts agents, removes one, or deletes a run's record.
    Acts,
    /// Stops work and starts none: the cancels.
    Stops,
}

tools! {
    ListProjects = "list_projects" (ListProjectsArgs) [Read, Look]
        "The user's projects: id, name, working folder, whether the folder exists, archived, and \
         status (busy when an agent has running or queued work or a workflow is running, else \
         idle). Start here: every other tool takes a project_id. Projects the user archived are \
         left out, as the window's project list leaves them out; pass include_archived: true to \
         list them too. An archived project's id still works in every other tool.";
    ListAgents = "list_agents" (ProjectArgs) [Read, Look]
        "A project's agents: id, name, harness, model, effort, the quick-choice lists of models \
         and efforts, busy, and context. busy is an observation: the user or a workflow can occupy \
         an idle agent before your next send. context is the agent's context fullness \
         ({used_tokens, window_tokens, fraction}) after its newest finished turn, or after a newer \
         compaction, the same figure Switchboard's sidebar shows; null when nothing has reported \
         it or the harness reports no window (Antigravity). It is the latest Switchboard observed: \
         a turn the agent ran outside Switchboard (continued in a terminal) shows after \
         Switchboard's next turn for it or the window's next reread. context_pending is true \
         while the agent's session file is still being read for it: call again. activity \
         describes the agent's running turn: seconds_since_output, since the turn started or \
         the agent last wrote text, started or finished a tool call, or heard from a \
         background task (what the window's \"No response\" counter measures); \
         open_tool_calls, each with its name and running_seconds; and open_background_tasks, \
         each with its description, activity (what it last reported doing, absent until it \
         reports), and running_seconds. A tool call open is work in progress: a test suite \
         produces nothing until it ends. Work inside a sub-agent the agent delegated to, or a \
         command it runs in the background, does not show as a tool call and is reported \
         under open_background_tasks. Absent while the agent has no turn running.";
    GetUsage = "get_usage" (NoArgs) [Read, Look]
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
    ReadConversation = "read_conversation" (ReadConversationArgs) [Read, Look]
        "A project's conversation in time order: each message sent (text and recipients), each \
         agent turn's answer text with its send_id and status (complete, running, failed, \
         cancelled), failures with their reason, and a compacted marker where an agent's context \
         was compacted (it may have lost earlier instructions there). Tool calls and thinking are \
         left out; the answer says what the agent did. last_n_items keeps only the newest items. \
         When the whole would pass the budget (100,000 characters, 45,000 without a progress \
         token, or max_chars), older items are left out whole (items_omitted says how many); a single answer too long on its own is cut, and \
         truncated names the read_reply call that reads on.";
    ReadReply = "read_reply" (ReadReplyArgs) [Read, Look]
        "One recipient's reply to one send, a page at a time, starting at offset (in \
         characters); a page holds up to 95,000 characters, less under max_chars. The way to \
         read a reply that another result cut: pass the arguments its \
         truncated entry's read_with gives, read_from included. next_offset is where the next \
         page starts and is absent on the last page. Also reads a reply still running (the text \
         so far), failed, or cancelled.";
    ReadLatestReply = "read_latest_reply" (ReadLatestReplyArgs) [Read, Look]
        "An agent's latest finished answer, including turns you did not send (the user's, a \
         workflow's), or the reason its latest turn failed or was cancelled. Carries the send_id \
         when known, so a long answer can be paged with read_reply, and the agent's context \
         after its newest finished turn (as list_agents reports it). newer_turn_running is true \
         when the agent has a turn running, or a send made through these tools is still to reach \
         it: this answer is then not its newest. false does not prove it current (a message the \
         user typed may be starting). To follow a message you sent, use wait_send; to wait for a \
         running agent, wait_agent.";
    ListPrompts = "list_prompts" (NoArgs) [Read, Look]
        "The user's saved prompts, local and from prompt servers: provider, name, description, \
         and arguments. They are the user's standing instructions (a code review, a security \
         review, an analysis of reviewers' findings): when one fits the task, send it rather than \
         writing your own. Use one in send or forward as prompt: {provider, name, args}. To \
         see what one says before sending it, read_prompt.";
    ReadPrompt = "read_prompt" (ReadPromptArgs) [Read, Look]
        "One saved prompt, to see what it says before sending it: its description and arguments \
         as list_prompts reports them, and template.text, the template with its placeholders \
         intact, for a builtin or local prompt. A prompt from a prompt server has no template \
         here: the server keeps it and gives out only finished text, and template_note says \
         so. Pass args (use {} for a prompt with no arguments) to also get rendered.text, the \
         text a send of the prompt with those args would deliver, from any provider. With args \
         the prompt renders as a send would even when Switchboard has no listing of it from its \
         server; arguments is then null and listing_note says why. A missing or refused \
         argument is invalid_argument, and a server that needs the user to sign in is \
         prompt_needs_sign_in. Text over the result budget (100,000 characters, 45,000 \
         without a progress token, or max_chars) is cut and listed in truncated; nothing reads \
         on. To use the prompt, send it as prompt: {provider, name, args}, not as text.";
    ListWorkflows = "list_workflows" (NoArgs) [Read, Look]
        "The user's workflows, saved multi-agent procedures (send to several reviewers and gather \
         their findings, for example): name, is_builtin, description, declared inputs, and \
         invocable: false when this server cannot run it (a pause_for_user or for_each step, or a \
         file that failed to parse, with its parse_error). Skip one that is not invocable and say \
         so. Prefer one to running its steps by hand. Call describe_workflow before run_workflow: it also lists the arguments the \
         workflow's saved prompts need.";
    DescribeWorkflow = "describe_workflow" (DescribeWorkflowArgs) [Read, Look]
        "Everything run_workflow needs for one workflow: its declared inputs (an agent input \
         takes an agent id, a list input a list) and the arguments derived from its saved \
         prompts, with which are required.";
    ListWorkflowRuns = "list_workflow_runs" (ProjectArgs) [Read, Look]
        "A project's workflow runs: running ones, and failed or interrupted ones still held. A \
         held run blocks every new workflow in the project until dismiss_workflow_run removes \
         it.";
    ReadWorkflowRun = "read_workflow_run" (ReadWorkflowRunArgs) [Read, Look]
        "Every reply a workflow run produced, in step order, without waiting: its status and \
         done, then each send with its step, label, send_id, done, and source, and each \
         recipient as wait_send reports it, with its agent's name and, once that recipient \
         completed, its reply text. A reviewer's first round comes back under its own step, \
         which the agent's latest answer no longer holds. On a running run it gives the \
         replies so far and the others' statuses (a running one with activity and \
         recent_text). started appears only with source journal: after a restart the reply's \
         outcome could not be confirmed. Long replies share the result evenly (100,000 \
         characters, 45,000 without a progress token, or max_chars), each cut listed in \
         truncated with the read_reply call that reads on. sends_unavailable is true \
         once Switchboard no longer holds the run's record (after a restart, or once it \
         pruned the run): the replies are then in read_conversation. Once Switchboard no \
         longer holds the run, a completed or cancelled run is run_unknown. A send \
         Switchboard no longer holds is read from disk (source journal), where a recipient \
         that was refused reads not_dispatched.";
    CreateProject = "create_project" (CreateProjectArgs) [Additive, SetUp]
        "Create a project for a folder on this Mac. It starts with no agents; add them with \
         create_agent. Avoid the folder this session runs in: the agents Switchboard launches \
         there would read the same MCP configuration and get these tools too. A folder may \
         hold several projects, and list_projects leaves out the ones the user archived: \
         before creating one, check list_projects with include_archived: true for a project the \
         user already has for this folder, whose agents and history a new one would not have.";
    RenameProject = "rename_project" (RenameProjectArgs) [Additive, SetUp]
        "Rename a project. Its id does not change.";
    CreateAgent = "create_agent" (CreateAgentArgs) [Additive, SetUp]
        "Add an agent to a project. harness is claude_code, codex, or antigravity. The model and \
         effort start from the user's defaults for that harness; pass either to override it, in \
         the harness's own naming. A value the harness does not know is not caught here: the \
         agent's next turn fails with the harness's error. list_agents shows the quick-choice \
         lists of known values. To change the model or effort later, use \
         set_agent_model_and_effort.";
    RenameAgent = "rename_agent" (RenameAgentArgs) [Additive, SetUp]
        "Rename an agent. Its id does not change.";
    SetAgentModelAndEffort = "set_agent_model_and_effort" (SetAgentModelAndEffortArgs) [Additive, SetUp]
        "Change an agent's model, effort, or both; the change applies from the agent's next \
         turn. Omit a field to leave it as it is; pass null to reset it to the harness's \
         default. Values are in the harness's own naming and are not checked here: a value the \
         harness does not know fails the agent's next turn. list_agents shows the quick-choice \
         lists of known values.";
    RemoveAgent = "remove_agent" (AgentArgs) [Acts, SetUp]
        "Remove an agent from its project for good: Switchboard's record of it goes, and its \
         card and conversation leave the window. Nothing brings it back; only the harness's own \
         session file stays on disk, outside Switchboard. Remove only an agent you created in \
         this session whose work is committed or passed on, and that will not be needed again; \
         ask the user before removing any other. Refused with agent_busy while the agent has a turn \
         running or queued (wait for it with wait_agent, or cancel a reply you sent with \
         cancel_send and agent_id; if a reply the user started keeps it busy, ask the user) or a \
         workflow runs in its project. A message that reaches the agent while it is being removed is cancelled or \
         refused, and no workflow can start in the project until the removal ends. Once begun, \
         it finishes even if this call is cancelled. Returns the removed agent's agent_id and \
         name.";
    DismissWorkflowRun = "dismiss_workflow_run" (RunArgs) [Acts, SetUp]
        "Delete a failed or interrupted workflow run's record, which otherwise blocks every new \
         workflow in the project. Returns the step that failed and why, which this removes from \
         the window, so read it. Refuses a running or completed run.";
    CompactAgent = "compact_agent" (CompactArgs) [Acts, Act]
        "Compact an agent's conversation: its harness summarizes the history so far and continues \
         from the summary, freeing context. Read context from list_agents to decide when: the \
         harness compacts on its own once the context fills, possibly mid-task, so compact between \
         tasks while context is high. Claude Code and Codex agents only (compaction_unsupported \
         otherwise); the agent must have replied \
         at least once (no_session), and a Claude fork must have had its first turn \
         (fork_not_materialized). The compaction waits behind the agent's running and queued work \
         and returns a send_id at once: call wait_send until done (the recipient carries no \
         text), and cancel_send cancels it while queued. idempotency_key as in send. While a \
         compaction is queued or running for the agent, another request returns its send_id \
         instead of queuing a second. The user sees it in Switchboard's window as a compaction \
         row and can cancel it there.";
    ForkAgent = "fork_agent" (ForkArgs) [Acts, Act]
        "Branch an agent's conversation into a new agent that starts with the source's \
         conversation so far, and send it a message: for a side question or a rabbit hole that \
         must not disturb the source. The fork is named <source>-fork and is an agent like any \
         other from then on (its id is the agent_id for every later call); the source is \
         untouched. Returns the fork's record and a send_id: call wait_send until done, then \
         read_latest_reply or read_conversation. Claude Code and Codex agents only \
         (fork_unsupported otherwise); the source must have replied at least once (no_session) \
         and must be idle (source_busy: wait_agent on it first). The message is text, or a saved \
         prompt with text appended, as in send. idempotency_key as in send, with one more rule: \
         once the fork exists it is kept under the key, so if the message was refused before it \
         started (recipients[0] is rejected, with a code) a retry with the same key sends it \
         again to the same fork rather than making a second, and a different key makes another \
         fork. The user sees the fork in its own pane in Switchboard's window, with its earlier \
         conversation. If the message started and then failed (wait_send says failed), the fork \
         exists: send to it by its agent_id rather than forking again; a Claude fork then branches \
         from the source's conversation as it is at that send. After request_in_progress, wait_send \
         on its send_id: once it settles, its recipient is the fork (list_agents shows it); retry \
         with the same key only if you gave one, since a retry without one makes a second fork. \
         Until a Claude fork's first reply ends, sends to its source fail with \
         agent_busy_elsewhere: wait for that reply, then resend. A fork stays until someone \
         removes it (remove_agent): fork for a reason, not per message.";
    Send = "send" (SendArgs) [Acts, Act]
        "Send one message to one or more agents of a project, as the user's next message in each \
         one's conversation. Returns at once with a send_id and each recipient's status at \
         dispatch: accepted, rejected (with a code), or not_dispatched. Then call wait_send until \
         done.\n\
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
    Forward = "forward" (ForwardArgs) [Acts, Act]
        format!(
            "Send agents' answers to other agents, composed the way the user's own forwards \
             are: your text first, then each source's answer in a labeled block. Sources may be \
             in other projects. Prefer this to copying a reply into send. A source without \
             send_id sends its latest answer: one still mid-turn is waited for, up to \
             timeout_seconds; if it is still running then, the forward fails with source_busy \
             and nothing is sent: call wait_agent on the source, then retry. A source whose \
             latest turn failed or was cancelled is still forwarded, as a note saying so in \
             place of an answer: check a source's status first. A source with send_id sends its \
             reply to that send instead, verbatim, whatever the agent has said since, and never \
             waits: a reply still to be sent, waiting in line, or running refuses the forward \
             at once (source_busy: wait_send on that send, then retry); one that failed, was \
             cancelled, or never reached the agent refuses it (invalid_argument), as does one \
             Switchboard cannot tie to the send (it may still be running, the app may have \
             restarted, or it cannot tell which turn answered: read it with read_reply \
             and send its text yourself); a send the agent did not receive in the source's \
             project, or whose reply has not started, is send_unknown. A source with no answer text refuses the forward (invalid_argument). \
             A refused forward sends nothing. With a saved prompt, forward_args fills a prompt \
             argument from sources instead. on_busy, idempotency_key, and the receipt work as \
             in send.\n\
             {WAIT_HINTS}"
        );
    RunWorkflow = "run_workflow" (RunWorkflowArgs) [Acts, Act]
        "Start a workflow in a project and return its run_id; then call wait_workflow until \
         done. Pass inputs as describe_workflow lists them: an agent input takes an agent id, a \
         list input a list. forward_sources fills an input with agents' latest answers. \
         wait_workflow lists every send the run made with its send_id: follow one with wait_send \
         as you would your own. read_workflow_run returns every reply in step order; read it \
         rather than read_latest_reply, since an agent's latest answer holds only its last \
         step. Refused with \
         workflow_already_running while another run is in progress in the project, with \
         workflow_run_requires_dismissal while a failed run is held there, and while an agent \
         is being removed from it (start again in a moment).";
    CancelSend = "cancel_send" (CancelSendArgs) [Stops, Stop]
        "Cancel a send. Without agent_id, every recipient still queued or running is cancelled, \
         and a send still being prepared stops before reaching anyone else. With agent_id, only \
         that agent's reply to the send is cancelled, queued or running (what the stop control \
         on that reply does in the window), and the other recipients carry on, except in a \
         workflow. Both work for sends you made since the app \
         last started, and for the sends of a workflow started through these tools \
         (wait_workflow lists their send_ids); there a cancelled reply cancels the whole run, \
         which stops the other recipients' replies too. To unstick one agent, pass its \
         agent_id. A reply to a message the user typed, or inside a workflow the user \
         started from the window, is refused: tell the user instead. outcome is cancelled; \
         already_finished when there was nothing left to cancel; or, with agent_id, requested \
         for a workflow's send older than Switchboard's record of it: the agent stops that \
         reply if it is still queued or running.";
    StopAgent = "stop_agent" (AgentArgs) [Stops, Stop]
        "Stop the agent's current turn and drop every message queued for it, including ones the \
         user or a workflow queued: what Stop agent does in Switchboard's window. It stops the \
         user's work too: use it only when the user asks. To unstick one reply without dropping \
         anything else, use cancel_send with agent_id.";
    CancelWorkflowRun = "cancel_workflow_run" (RunArgs) [Stops, Stop]
        "Cancel a running workflow run started through these tools. A run the user started from \
         Switchboard's window is refused: tell the user instead.";
    WaitSend = "wait_send" (WaitSendArgs) [Read, Wait]
        format!(
            "Wait for a send's replies: one you made, or one a workflow made (wait_workflow \
             lists those). Returns when every recipient has finished or timeout_seconds passes, \
             with the current state either way: call again until done is true. While not done, \
             recipients carry statuses, and each running one its activity (as list_agents \
             describes it) and recent_text: the last 500 characters it has written in this turn \
             so far (its answer text, not tool output; absent until it writes some). An accepted \
             recipient is waiting in line behind another turn and carries no activity: \
             list_agents shows that other turn's. When done, each completed recipient carries its \
             reply text and its agent's context after the turn (as list_agents reports it), for \
             deciding whether to compact before the next step. Statuses: accepted (waiting in \
             line), running, completed, failed, cancelled, rejected, not_dispatched; and only \
             with source journal, started: the send started but its outcome could not be \
             confirmed (often a restart cut the reply off), so ask the agent where it got to. \
             source is live, journal (read after a restart, or for a send older than \
             Switchboard's record of it), or unknown: this project has no record of the send, so \
             check project_id and send_id; a message still waiting in line when the app restarted \
             was never sent and reads this way too. Long replies share the result evenly, each cut \
             one listed in truncated with the read_reply call that reads on. When done, \
             the result also {USAGE_NOTE}\n{WAIT_HINTS}"
        );
    WaitAnySend = "wait_any_send" (WaitAnySendArgs) [Read, Wait]
        format!(
            "Wait on several sends at once, for agents working in parallel: returns as soon as \
             any one is done, or when timeout_seconds passes, with every send's current state \
             either way. sends lists each send once, in the order given, shaped as wait_send \
             returns it: a done one carries its replies, the others their recipients' statuses, \
             activity, and recent_text. any_done says whether one finished. Call again with the \
             sends still running; a send already done returns at once. Long replies share the \
             result evenly, each cut one listed in truncated with the read_reply call \
             that reads on. When any_done, the result also, once at its top level, \
             {USAGE_NOTE}\n{WAIT_HINTS}"
        );
    WaitAgent = "wait_agent" (WaitAgentArgs) [Read, Wait]
        format!(
            "Wait until an agent would accept a new send, then return idle with its latest \
             reply in output, as read_latest_reply returns it; or running once timeout_seconds \
             passes (call again), with the agent's \
             activity (as list_agents describes it). idle is an \
             observation: the user or a workflow can occupy the agent before your next send, \
             which then returns agent_busy. When idle, the result also {USAGE_NOTE}\n{WAIT_HINTS}"
        );
    WaitWorkflow = "wait_workflow" (WaitWorkflowArgs) [Read, Wait]
        format!(
            "Wait for a workflow run to finish. Returns running once timeout_seconds passes \
             (call again), or the final status (completed, failed, cancelled, interrupted) with \
             the step that failed and why. Either way it gives the current step (counted from \
             0, its label, and its agents as {{agent_id, name}}) and sends. While the run is \
             unfinished, each of the step's agents also carries the send_id of the run's \
             newest send that reached it and, while Switchboard holds that send, its status, \
             activity, and recent_text as wait_send reports them. On a wait step that is the \
             send step's turn, usually the one the step waits on. sends lists every send the run's \
             send steps made so far, each with its step, label, send_id, and recipients \
             (dispatched false for one a failed step never reached). Follow any of those sends \
             with wait_send. read_workflow_run returns all their replies in step order, even \
             after an agent answered a later step: an agent's latest answer holds only its last \
             one. A run with more sends than one reply holds lists only the newest: items_omitted \
             says how many of the earliest were left out, and read_workflow_run still lists \
             them. sends_unavailable is true once Switchboard no longer holds the \
             run's record (after a restart, or once it pruned the run): its sends are no longer \
             known. When done, the result also {USAGE_NOTE}\n{WAIT_HINTS}"
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
                ToolKind::Acts | ToolKind::Stops => {
                    ToolAnnotations::new().read_only(false).destructive(true)
                }
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
pub struct ListProjectsArgs {
    /// Also list the projects the user archived. False by default.
    #[serde(default)]
    include_archived: bool,
}

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
pub struct ReadLatestReplyArgs {
    #[schemars(with = "String")]
    project_id: ProjectId,
    #[schemars(with = "String")]
    agent_id: AgentId,
    /// Keep the result under this many characters: 10,000 to 100,000. By
    /// default 100,000, or 45,000 when the client sends no progress token.
    #[serde(default)]
    max_chars: Option<usize>,
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
    /// Keep the result under this many characters: 10,000 to 100,000. By
    /// default 100,000, or 45,000 when the client sends no progress token.
    #[serde(default)]
    max_chars: Option<usize>,
}

/// Which copy of a reply to page.
#[derive(Deserialize, JsonSchema, Default, Clone, Copy)]
#[schemars(crate = "rmcp::schemars")]
#[serde(rename_all = "snake_case")]
enum ReadFromArg {
    /// The text captured as the reply streamed when the app holds it, else
    /// the agent's session file. What `wait_send` and `read_latest_reply` show.
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
pub struct ReadReplyArgs {
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
    /// Keep the result under this many characters: 10,000 to 100,000. By
    /// default 100,000, or 45,000 when the client sends no progress token.
    #[serde(default)]
    max_chars: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct ReadWorkflowRunArgs {
    #[schemars(with = "String")]
    project_id: ProjectId,
    /// The run id `run_workflow` or `list_workflow_runs` returned.
    #[schemars(with = "String")]
    run_id: Uuid,
    /// Keep the result under this many characters: 10,000 to 100,000. By
    /// default 100,000, or 45,000 when the client sends no progress token.
    #[serde(default)]
    max_chars: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct ReadPromptArgs {
    /// The prompt's provider, as `list_prompts` reports it.
    provider: String,
    /// The prompt's name within that provider.
    name: String,
    /// Argument values by name, as `list_prompts` declares them. When given,
    /// even as `{}`, the result also holds the rendering a send would deliver.
    #[serde(default)]
    args: Option<BTreeMap<String, String>>,
    /// Keep the result under this many characters: 10,000 to 100,000. By
    /// default 100,000, or 45,000 when the client sends no progress token.
    #[serde(default)]
    max_chars: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct DescribeWorkflowArgs {
    #[schemars(with = "String")]
    project_id: ProjectId,
    /// The workflow's name, as `list_workflows` reports it.
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
    /// Letters, digits, `_` and `-`; unique among projects.
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
    /// The new name: letters, digits, `_` and `-`.
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
    /// Letters, digits, `_` and `-`; must not match another agent's name in
    /// the project (case and `-`/`_` do not distinguish names).
    name: String,
    harness: HarnessArg,
    /// The model, in the harness's own naming; `list_agents` shows the known
    /// values. Omit for the user's default.
    #[serde(default)]
    model: Option<String>,
    /// The effort, in the harness's own naming. Omit for the user's default.
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
    /// The new name: letters, digits, `_` and `-`.
    name: String,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct SetAgentModelAndEffortArgs {
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
    /// The run id `run_workflow` or `list_workflow_runs` returned.
    #[schemars(with = "String")]
    run_id: Uuid,
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
    /// The prompt's provider, as `list_prompts` reports it.
    provider: String,
    /// The prompt's name within that provider.
    name: String,
    /// Argument values by name, as `list_prompts` declares them.
    #[serde(default)]
    args: BTreeMap<String, String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct CompactArgs {
    #[schemars(with = "String")]
    project_id: ProjectId,
    #[schemars(with = "String")]
    agent_id: AgentId,
    /// Any string unique to this request; a retry passes the same one.
    #[serde(default)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct ForkArgs {
    #[schemars(with = "String")]
    project_id: ProjectId,
    /// The agent whose conversation is branched.
    #[schemars(with = "String")]
    agent_id: AgentId,
    /// The fork's first message; with a prompt, the text appended after it.
    #[serde(default)]
    text: Option<String>,
    /// A saved prompt to send, rendered with its args.
    #[serde(default)]
    prompt: Option<PromptArg>,
    /// Any string unique to this request; a retry passes the same one.
    #[serde(default)]
    idempotency_key: Option<String>,
    /// Files on this Mac to attach, as absolute paths: a screenshot, a log.
    /// Each is copied into the project, and every recipient gets the copies'
    /// paths after the message, as with a file attached in the window.
    #[serde(default)]
    attachments: Vec<String>,
}

/// A message from its two optional parts: a saved prompt with text appended,
/// or text alone.
fn body_of(prompt: Option<PromptArg>, text: Option<String>) -> Result<Body, AppError> {
    match (prompt, text) {
        (Some(prompt), appended_text) => Ok(Body::Prompt {
            provider: prompt.provider,
            name: prompt.name,
            args: prompt.args,
            appended_text,
        }),
        (None, Some(text)) => Ok(Body::Text { text }),
        (None, None) => Err(AppError::InvalidArgument(
            "a send needs text, a prompt, or both".to_owned(),
        )),
    }
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
    /// Files on this Mac to attach, as absolute paths: a screenshot, a log.
    /// Each is copied into the project, and every recipient gets the copies'
    /// paths after the message, as with a file attached in the window.
    #[serde(default)]
    attachments: Vec<String>,
}

/// A forward's source: an agent whose latest answer is forwarded, or with
/// `send_id`, one of its earlier replies. It may be in another project.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[schemars(crate = "rmcp::schemars", inline)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_field_names)] // The three ids that name a source's reply.
struct SourceArg {
    /// The source's project; leave it out for an agent in the call's own
    /// project.
    #[serde(default)]
    #[schemars(with = "Option<String>")]
    project_id: Option<ProjectId>,
    #[schemars(with = "String")]
    agent_id: AgentId,
    /// Forward this agent's reply to this send (a `send_id` from `send`,
    /// `forward`, or `wait_workflow`'s sends) instead of its latest answer,
    /// without waiting on the agent. The reply must be finished and completed.
    #[serde(default)]
    #[schemars(with = "Option<String>")]
    send_id: Option<SendId>,
}

impl SourceArg {
    fn in_project(self, project_id: ProjectId) -> ForwardFrom {
        ForwardFrom {
            project_id: self.project_id.unwrap_or(project_id),
            agent_id: self.agent_id,
            send_id: self.send_id,
        }
    }
}

/// The sources as the call names them, a source without a project taken to
/// be in the call's own `project_id`.
fn sources(args: Vec<SourceArg>, project_id: ProjectId) -> Vec<ForwardFrom> {
    args.into_iter()
        .map(|arg| arg.in_project(project_id))
        .collect()
}

fn source_map(
    args: BTreeMap<String, Vec<SourceArg>>,
    project_id: ProjectId,
) -> BTreeMap<String, Vec<ForwardFrom>> {
    args.into_iter()
        .map(|(name, list)| (name, sources(list, project_id)))
        .collect()
}

/// An agent whose latest answer fills a workflow input. It may be in another
/// project. Unlike a forward's source it cannot name an earlier reply: a
/// workflow's forwarded inputs are read from each agent's latest completed
/// answer when the run starts.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[schemars(crate = "rmcp::schemars", inline)]
#[serde(deny_unknown_fields)]
struct WorkflowSourceArg {
    /// The source's project; leave it out for an agent in the call's own
    /// project.
    #[serde(default)]
    #[schemars(with = "Option<String>")]
    project_id: Option<ProjectId>,
    #[schemars(with = "String")]
    agent_id: AgentId,
}

fn workflow_source_map(
    args: BTreeMap<String, Vec<WorkflowSourceArg>>,
    project_id: ProjectId,
) -> BTreeMap<String, Vec<ForwardSourceRef>> {
    args.into_iter()
        .map(|(name, list)| {
            let refs = list
                .into_iter()
                .map(|arg| ForwardSourceRef {
                    project_id: arg.project_id.unwrap_or(project_id),
                    agent_id: arg.agent_id,
                })
                .collect();
            (name, refs)
        })
        .collect()
}

/// A saved prompt in a forward.
#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ForwardPromptArg {
    /// The prompt's provider, as `list_prompts` reports it.
    provider: String,
    /// The prompt's name within that provider.
    name: String,
    /// Argument values by name, as `list_prompts` declares them.
    #[serde(default)]
    args: BTreeMap<String, String>,
    /// Prompt arguments filled from sources, each source `{agent_id}`, with
    /// `project_id` for an agent in another project and `send_id` for one of
    /// its earlier replies: each named argument becomes its typed text (from
    /// args) followed by the sources' answers.
    #[serde(default)]
    forward_args: BTreeMap<String, Vec<SourceArg>>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct ForwardArgs {
    #[schemars(with = "String")]
    project_id: ProjectId,
    /// Agents whose answers are appended after the text, each as
    /// `{agent_id}`, with `project_id` for an agent in another project and
    /// `send_id` to forward one of its earlier replies instead of its latest.
    #[serde(default)]
    sources: Vec<SourceArg>,
    /// Agent ids in this project. At least one; none repeated.
    #[schemars(with = "Vec<String>")]
    recipients: Vec<AgentId>,
    /// Text placed before the forwarded answers.
    #[serde(default)]
    text: Option<String>,
    /// A saved prompt to send, rendered with its args; its `forward_args` fill
    /// arguments from sources.
    #[serde(default)]
    prompt: Option<ForwardPromptArg>,
    #[serde(default)]
    on_busy: OnBusyArg,
    /// Any string unique to this request; a retry passes the same one.
    #[serde(default)]
    idempotency_key: Option<String>,
    /// Seconds to wait for a source still mid-turn. 50 by default.
    #[serde(default)]
    timeout_seconds: Option<u64>,
    /// Files on this Mac to attach, as absolute paths: a screenshot, a log.
    /// Each is copied into the project, and every recipient gets the copies'
    /// paths after the message, as with a file attached in the window.
    #[serde(default)]
    attachments: Vec<String>,
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
    /// The workflow's name, as `list_workflows` reports it.
    name: String,
    /// Whether the workflow is one of Switchboard's built-ins, as
    /// `list_workflows` reports it.
    #[serde(default)]
    is_builtin: bool,
    /// Declared inputs and prompt arguments by name. Agent inputs take
    /// agent ids.
    #[serde(default)]
    inputs: BTreeMap<String, InputArg>,
    /// Inputs filled with agents' latest answers, by input name; each source
    /// is `{agent_id}`, with `project_id` for an agent in another project.
    #[serde(default)]
    forward_sources: BTreeMap<String, Vec<WorkflowSourceArg>>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct WaitSendArgs {
    #[schemars(with = "String")]
    project_id: ProjectId,
    /// The `send_id` that `send`, `forward`, `compact_agent`, or `fork_agent`
    /// returned.
    #[schemars(with = "String")]
    send_id: SendId,
    /// Seconds to wait before returning the current state. 50 by default.
    #[serde(default)]
    timeout_seconds: Option<u64>,
    /// Keep the result under this many characters: 10,000 to 100,000. By
    /// default 100,000, or 45,000 when the client sends no progress token.
    #[serde(default)]
    max_chars: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_field_names)] // The three ids a single-recipient cancel names.
pub struct CancelSendArgs {
    #[schemars(with = "String")]
    project_id: ProjectId,
    /// The send to cancel, as `send`, `forward`, `compact_agent`, `fork_agent`,
    /// or `wait_workflow` reported it.
    #[schemars(with = "String")]
    send_id: SendId,
    /// Cancel only this agent's reply to the send; the other recipients carry
    /// on. Omit to cancel every recipient.
    #[serde(default)]
    #[schemars(with = "Option<String>")]
    agent_id: Option<AgentId>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct WaitAnySendArgs {
    #[schemars(with = "String")]
    project_id: ProjectId,
    /// The `send_id`s to wait on, each as `send`, `forward`, `compact_agent`,
    /// or `fork_agent` returned it, all made in `project_id`. At least one.
    #[schemars(with = "Vec<String>")]
    send_ids: Vec<SendId>,
    /// Seconds to wait before returning the current state. 50 by default.
    #[serde(default)]
    timeout_seconds: Option<u64>,
    /// Keep the result under this many characters: 10,000 to 100,000. By
    /// default 100,000, or 45,000 when the client sends no progress token.
    #[serde(default)]
    max_chars: Option<usize>,
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
    /// Keep the result under this many characters: 10,000 to 100,000. By
    /// default 100,000, or 45,000 when the client sends no progress token.
    #[serde(default)]
    max_chars: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub struct WaitWorkflowArgs {
    #[schemars(with = "String")]
    project_id: ProjectId,
    /// The run id `run_workflow` or `list_workflow_runs` returned.
    #[schemars(with = "String")]
    run_id: Uuid,
    /// Seconds to wait before returning the current state. 50 by default.
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

fn parse<P: DeserializeOwned>(args: JsonObject) -> Result<P, AppError> {
    serde_json::from_value(Value::Object(args)).map_err(|e| {
        let hint = if e.to_string().contains("UUID") {
            " (ids come from list_projects, list_agents, and the send and run results; names are \
             not accepted)"
        } else {
            ""
        };
        AppError::InvalidArgument(format!("invalid arguments: {e}{hint}"))
    })
}

// --- calling ---------------------------------------------------------------------

/// The result budget for a client that sends no progress token. Claude
/// Desktop is the one measured client without one, and its Code tab shows a
/// result inline only under 50,000 characters, saving a larger one to a file.
const NO_PROGRESS_RESULT_CHARS: usize = 45_000;

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

    /// The result budget for this call: the caller's `max_chars`, or the
    /// full budget, or [`NO_PROGRESS_RESULT_CHARS`] for a client that sends no
    /// progress token, so Claude Desktop gets results it shows inline without
    /// the model having to know which client it runs in.
    fn result_chars(&self, max_chars: Option<usize>) -> usize {
        result_budget_chars(
            max_chars.or((!self.can_keep_alive).then_some(NO_PROGRESS_RESULT_CHARS)),
        )
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
            let a: ListProjectsArgs = parse(args)?;
            Reply::plain(&read::list_projects(state, a.include_archived, &cx.cancel).await?)
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
            Reply::fitted(
                &digest,
                read::conversation_budget(a.project_id, cx.result_chars(a.max_chars)),
            )
        }
        ToolName::ReadReply => {
            let a: ReadReplyArgs = parse(args)?;
            Reply::plain(
                &read::read_reply(
                    state,
                    a.project_id,
                    a.send_id,
                    a.agent_id,
                    a.offset,
                    a.read_from.into(),
                    output_page_chars(cx.result_chars(a.max_chars)),
                    cx.home(),
                )
                .await?,
            )
        }
        ToolName::ReadLatestReply => {
            let a: ReadLatestReplyArgs = parse(args)?;
            let output =
                read::read_latest_reply(state, a.project_id, a.agent_id, cx.home()).await?;
            Reply::fitted(
                &output,
                read::latest_reply_budget(a.project_id, cx.result_chars(a.max_chars)),
            )
        }
        ToolName::ListPrompts => {
            parse::<NoArgs>(args)?;
            Reply::plain(&read::list_prompts(state))
        }
        ToolName::ReadPrompt => {
            let a: ReadPromptArgs = parse(args)?;
            let reading = read::read_prompt(
                state,
                &a.provider,
                &a.name,
                a.args.as_ref(),
                &cx.budget(None),
            )
            .await?;
            Reply::fitted(&reading, read::prompt_budget(cx.result_chars(a.max_chars)))
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
        ToolName::ReadWorkflowRun => {
            let a: ReadWorkflowRunArgs = parse(args)?;
            let replies =
                workflow::read_workflow_run(state, a.project_id, a.run_id, cx.home()).await?;
            Reply::fitted(
                &replies,
                workflow::read_workflow_run_budget(a.project_id, cx.result_chars(a.max_chars)),
            )
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
        ToolName::SetAgentModelAndEffort => {
            let a: SetAgentModelAndEffortArgs = parse(args)?;
            Reply::plain(&change::set_agent_model_and_effort(
                state,
                a.project_id,
                a.agent_id,
                &a.model,
                &a.effort,
            )?)
        }
        ToolName::RemoveAgent => {
            let a: AgentArgs = parse(args)?;
            Reply::plain(
                &change::remove_agent(Arc::clone(&cx.handle), a.project_id, a.agent_id).await?,
            )
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
            let request = SendRequest {
                project_id: a.project_id,
                recipients: a.recipients,
                body: body_of(a.prompt, a.text)?,
                on_busy: a.on_busy.into(),
                idempotency_key: a.idempotency_key,
                attachments: a.attachments.into_iter().map(PathBuf::from).collect(),
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
        ToolName::CompactAgent => {
            let a: CompactArgs = parse(args)?;
            let receipt = compact::compact(
                Arc::clone(&cx.handle),
                cx.home.clone(),
                CompactRequest {
                    project_id: a.project_id,
                    agent_id: a.agent_id,
                    idempotency_key: a.idempotency_key,
                },
                cx.budget(None),
            )
            .await?;
            Reply::plain(&receipt)
        }
        ToolName::ForkAgent => {
            let a: ForkArgs = parse(args)?;
            let receipt = fork::fork(
                Arc::clone(&cx.handle),
                cx.home.clone(),
                ForkRequest {
                    project_id: a.project_id,
                    agent_id: a.agent_id,
                    body: body_of(a.prompt, a.text)?,
                    idempotency_key: a.idempotency_key,
                    attachments: a.attachments.into_iter().map(PathBuf::from).collect(),
                },
                cx.budget(None),
            )
            .await?;
            Reply::plain(&receipt)
        }
        ToolName::Forward => {
            let a: ForwardArgs = parse(args)?;
            let request = ForwardRequest {
                project_id: a.project_id,
                sources: sources(a.sources, a.project_id),
                recipients: a.recipients,
                text: a.text,
                prompt: a.prompt.map(|p| ForwardPrompt {
                    provider: p.provider,
                    name: p.name,
                    args: p.args,
                    forward_args: source_map(p.forward_args, a.project_id),
                }),
                on_busy: a.on_busy.into(),
                idempotency_key: a.idempotency_key,
                attachments: a.attachments.into_iter().map(PathBuf::from).collect(),
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
                forward_sources: workflow_source_map(a.forward_sources, a.project_id),
            };
            let run_id =
                workflow::run_workflow(state, cx.home(), request, &cx.budget(None)).await?;
            Reply::plain(&serde_json::json!({ "run_id": run_id }))
        }
        ToolName::CancelSend => {
            let a: CancelSendArgs = parse(args)?;
            Reply::plain(&send::cancel_send(
                state,
                a.project_id,
                a.send_id,
                a.agent_id,
            )?)
        }
        ToolName::StopAgent => {
            let a: AgentArgs = parse(args)?;
            Reply::plain(&send::stop_agent(state, a.project_id, a.agent_id)?)
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
            Reply::fitted(
                &reply,
                wait_send_budget(a.project_id, a.send_id, cx.result_chars(a.max_chars)),
            )
        }
        ToolName::WaitAnySend => {
            let a: WaitAnySendArgs = parse(args)?;
            let budget = cx.budget(a.timeout_seconds);
            let reply =
                send::wait_any_send(state, a.project_id, a.send_ids, &budget, cx.home()).await?;
            Reply::fitted(
                &reply,
                send::wait_any_send_budget(a.project_id, cx.result_chars(a.max_chars)),
            )
        }
        ToolName::WaitAgent => {
            let a: WaitAgentArgs = parse(args)?;
            let budget = cx.budget(a.timeout_seconds);
            let reply =
                send::wait_agent(state, a.project_id, a.agent_id, &budget, cx.home()).await?;
            Reply::fitted(
                &reply,
                read::latest_reply_budget(a.project_id, cx.result_chars(a.max_chars)),
            )
        }
        ToolName::WaitWorkflow => {
            let a: WaitWorkflowArgs = parse(args)?;
            let budget = cx.budget(a.timeout_seconds);
            let reply = workflow::wait_workflow(state, a.project_id, a.run_id, &budget).await?;
            Reply::fitted(
                &reply,
                workflow::wait_workflow_budget(cx.result_chars(None)),
            )
        }
    }
}
