//! The change tier: create, rename, model and effort, and dismissing a failed
//! workflow run. Each opens the project in Rust first (a no-op when loaded),
//! then notifies the window without waiting on it.

use serde::{Deserialize, Serialize};
use switchboard_core::{
    AgentId, AgentRecord, AgentSelection, HarnessKind, ProjectId, ProjectSummary,
};
use uuid::Uuid;

use crate::commands::{
    ProjectListing, create_agent_in_project_impl, create_project_impl, open_project_impl,
    rename_agent_impl, rename_project_impl, update_agent_selection_impl,
};
use crate::error::AppError;
use crate::state::{AppState, lock};
use crate::workflow_commands::{abandon_workflow_run_impl, classify_run_file, read_run_files};

use super::read::agent_in_project;

/// Create a project for `directory`. Seeds no agents: the desktop seeds one
/// per installed harness, but an orchestrator adds the agents it wants.
pub fn create_project(
    state: &AppState,
    name: &str,
    directory: &str,
) -> Result<ProjectSummary, AppError> {
    let summary = create_project_impl(state, name, directory)?;
    state.desktop_sync.projects_changed();
    Ok(summary)
}

/// Rename a project. Store-only, like the desktop's rename: it needs neither
/// the working directory nor the agent cache, so it deliberately does not
/// open the project — opening would refuse a rename while another process
/// holds the project, for nothing in return.
pub fn rename_project(
    state: &AppState,
    project_id: ProjectId,
    name: &str,
) -> Result<ProjectListing, AppError> {
    let listing = rename_project_impl(state, project_id, name)?;
    state.desktop_sync.projects_changed();
    Ok(listing)
}

/// Create an agent in `project_id`. Starts from the user's per-harness
/// defaults in Settings — default model, default effort, both quick-choice
/// lists — exactly as the desktop seeds a new agent, then applies the model
/// and effort the caller gave. The values are harness-native strings, not
/// validated against a catalog: a bad one fails the next turn with the
/// harness's own error.
pub fn create_agent(
    state: &AppState,
    project_id: ProjectId,
    name: &str,
    harness: HarnessKind,
    model: Option<String>,
    effort: Option<String>,
) -> Result<AgentRecord, AppError> {
    open_project_impl(state, project_id)?;
    let defaults = lock(&state.preferences)
        .agent_defaults
        .get(&harness)
        .cloned()
        .unwrap_or_default();
    let selection = AgentSelection {
        model: model.or(defaults.default_model),
        effort: effort.or(defaults.default_effort),
        model_choices: defaults.model_choices,
        effort_choices: defaults.effort_choices,
    };
    let record = create_agent_in_project_impl(state, project_id, name, harness, selection)?;
    state.desktop_sync.roster_changed(project_id);
    Ok(record)
}

pub fn rename_agent(
    state: &AppState,
    project_id: ProjectId,
    agent_id: AgentId,
    name: &str,
) -> Result<AgentRecord, AppError> {
    open_project_impl(state, project_id)?;
    agent_in_project(state, project_id, agent_id)?;
    let record = rename_agent_impl(state, agent_id, name)?;
    state.desktop_sync.roster_changed(project_id);
    Ok(record)
}

/// One axis of an agent's selection as a caller states it. Absent means
/// unchanged; an explicit `null` means reset to the harness's default. The
/// distinction matters because a record with no model means "use the harness
/// default", so a caller changing only the effort must not wipe the model.
///
/// Deserialized by hand: serde reads a JSON `null` on `Option<Option<T>>` as
/// the outer `None`, which would make `null` mean "unchanged". Here any value
/// that is present, `null` included, is `Some(..)`; the field it sits in must
/// carry `#[serde(default)]` so an omitted field is `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct SelectionChange(pub Option<Option<String>>);

impl<'de> Deserialize<'de> for SelectionChange {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let present: Option<String> = Option::deserialize(deserializer)?;
        Ok(Self(Some(present)))
    }
}

impl SelectionChange {
    #[must_use]
    pub fn unchanged() -> Self {
        Self(None)
    }

    #[must_use]
    pub fn set(value: impl Into<String>) -> Self {
        Self(Some(Some(value.into())))
    }

    #[must_use]
    pub fn reset() -> Self {
        Self(Some(None))
    }

    fn apply(&self, current: Option<String>) -> Option<String> {
        match &self.0 {
            None => current,
            Some(value) => value.clone(),
        }
    }
}

/// The parts of an agent's selection a caller changed; every absent part is
/// left as it is. Callers send only what the user (or orchestrator) changed,
/// so two clients changing different settings at once both keep their
/// change: a whole-selection write would put back the stale copy of the
/// setting the writer did not touch. Changes to the same setting resolve as
/// last write wins.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct SelectionChanges {
    #[serde(default)]
    pub model: SelectionChange,
    #[serde(default)]
    pub effort: SelectionChange,
    #[serde(default)]
    pub model_choices: Option<Vec<String>>,
    #[serde(default)]
    pub effort_choices: Option<Vec<String>>,
}

impl SelectionChanges {
    /// The whole selection to write, given the record as it is now. Called
    /// under the registry lock, so `current` is the latest write.
    #[must_use]
    pub fn apply(&self, current: &AgentRecord) -> AgentSelection {
        AgentSelection {
            model: self.model.apply(current.model.clone()),
            effort: self.effort.apply(current.effort.clone()),
            model_choices: self
                .model_choices
                .clone()
                .unwrap_or_else(|| current.model_choices.clone()),
            effort_choices: self
                .effort_choices
                .clone()
                .unwrap_or_else(|| current.effort_choices.clone()),
        }
    }
}

/// Change an agent's model and/or effort. The quick-choice lists are kept,
/// and a new value joins them, because the core library appends the current
/// value to its list on every write so every dispatch-ready value is also
/// reachable from the desktop's picker. The read of the current record and
/// the write happen under one registry lock, so a window edit landing in
/// between is not undone.
pub fn set_agent_selection(
    state: &AppState,
    project_id: ProjectId,
    agent_id: AgentId,
    model: &SelectionChange,
    effort: &SelectionChange,
) -> Result<AgentRecord, AppError> {
    open_project_impl(state, project_id)?;
    agent_in_project(state, project_id, agent_id)?;
    let changes = SelectionChanges {
        model: model.clone(),
        effort: effort.clone(),
        ..SelectionChanges::default()
    };
    let record = update_agent_selection_impl(state, agent_id, |current| changes.apply(current))?;
    state.desktop_sync.roster_changed(project_id);
    Ok(record)
}

/// What a dismissed run was, returned and logged because dismissal removes
/// the window's record of why the run failed.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DismissedRun {
    pub run_id: Uuid,
    pub workflow: String,
    /// `failed` or `interrupted`.
    pub status: &'static str,
    pub step: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Delete a failed or interrupted run's status file, which otherwise blocks
/// every further workflow in the project. Refuses a live run, and a run whose
/// file classifies as anything else: a run leaves the live registry before
/// retention deletes a completed run's file, so for a moment a completed run
/// is neither live nor held, and "not live" alone would let that file go.
pub fn dismiss_workflow_run(
    state: &AppState,
    project_id: ProjectId,
    run_id: Uuid,
) -> Result<DismissedRun, AppError> {
    open_project_impl(state, project_id)?;
    if lock(&state.workflow_runs).contains_key(&run_id) {
        return Err(AppError::WorkflowRunActive { run_id });
    }
    let project = lock(&state.projects)
        .get(&project_id)
        .cloned()
        .ok_or(AppError::ProjectNotLoaded(project_id))?;
    let records = read_run_files(&project.runs_dir())
        .into_iter()
        .find(|(id, _)| *id == run_id)
        .map(|(_, records)| records)
        .ok_or(AppError::WorkflowRunNotFound { run_id })?;
    let info = classify_run_file(run_id, &records).ok_or_else(|| {
        AppError::InvalidArgument(format!(
            "workflow run {run_id} is not failed or interrupted, so there is nothing to dismiss"
        ))
    })?;
    abandon_workflow_run_impl(state, project_id, run_id)?;
    tracing::info!(
        %project_id,
        %run_id,
        workflow = %info.workflow,
        status = info.status,
        step = info.step,
        reason = ?info.reason,
        "workflow run dismissed"
    );
    state.desktop_sync.runs_changed(project_id);
    Ok(DismissedRun {
        run_id,
        workflow: info.workflow,
        status: info.status,
        step: info.step,
        reason: info.reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Args {
        #[serde(default)]
        model: SelectionChange,
    }

    #[test]
    fn omitted_null_and_value_decode_to_unchanged_reset_and_set() {
        let omitted: Args = serde_json::from_str("{}").unwrap();
        assert_eq!(omitted.model, SelectionChange::unchanged());
        assert_eq!(
            omitted.model.apply(Some("opus".to_owned())).as_deref(),
            Some("opus")
        );
        let null: Args = serde_json::from_str(r#"{"model": null}"#).unwrap();
        assert_eq!(null.model, SelectionChange::reset());
        assert_eq!(null.model.apply(Some("opus".to_owned())), None);
        let set: Args = serde_json::from_str(r#"{"model": "haiku"}"#).unwrap();
        assert_eq!(set.model, SelectionChange::set("haiku"));
        assert_eq!(
            set.model.apply(Some("opus".to_owned())).as_deref(),
            Some("haiku")
        );
    }
}
