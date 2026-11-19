//! The window's Antigravity model table, checked from Rust.
//!
//! The table is `desktop/src/lib/antigravityModels.json`. The frontend reads it
//! for the model and effort pickers, and these tests read the same file, so
//! there is one list and nothing here can drift from what the window offers.
//! The file holds the models the pickers offer and the models left out on
//! purpose. Google adds and retires Antigravity models without a CLI release,
//! so the live test compares the two lists together with `agy`'s own list: a
//! model Google adds or retires fails `make test-live-antigravity` instead of
//! a user's new agent.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use futures::StreamExt;
use serde::Deserialize;
use switchboard_core::{AgentRecord, HarnessKind, SessionLocator};
use switchboard_harness::{
    AdapterEvent, AntigravityAdapter, DispatchOptions, HarnessAdapter, Turn, TurnOutcome,
    load_antigravity_transcript,
};
use uuid::Uuid;

use crate::preferences::Preferences;

const TABLE: &str = include_str!("../../src/lib/antigravityModels.json");

/// The levels `agy --help` lists for `--effort` (1.4.2). A trailing
/// parenthetical in a model's label is a folded level only when it names one
/// of these; anything else, like 1.1.19's `(Thinking)`, is part of the name.
const CLI_EFFORT_LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// Each model's effort levels, `None` for a model that takes no `--effort`.
type Catalog = BTreeMap<String, Option<BTreeSet<String>>>;

#[derive(Deserialize)]
struct Table {
    /// The models the pickers offer.
    models: Vec<TableModel>,
    /// Models `agy` lists that Switchboard leaves out on purpose.
    left_out: Vec<LeftOutModel>,
}

#[derive(Deserialize)]
struct TableModel {
    value: String,
    label: String,
    efforts: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct LeftOutModel {
    #[serde(flatten)]
    model: TableModel,
    reason: String,
}

fn parsed_table() -> Table {
    serde_json::from_str(TABLE).expect("antigravityModels.json parses")
}

fn as_catalog<'a>(models: impl IntoIterator<Item = &'a TableModel>) -> Catalog {
    models
        .into_iter()
        .map(|model| {
            let efforts = model.efforts.as_ref().map(|e| e.iter().cloned().collect());
            (model.value.clone(), efforts)
        })
        .collect()
}

/// The offered and the left-out models together: every model the table
/// accounts for, which is what must equal `agy`'s list.
fn table() -> Catalog {
    let table = parsed_table();
    as_catalog(
        table
            .models
            .iter()
            .chain(table.left_out.iter().map(|left_out| &left_out.model)),
    )
}

#[derive(Deserialize)]
struct CliOutput {
    command: CliCommand,
}

#[derive(Deserialize)]
struct CliCommand {
    data: CliData,
}

#[derive(Deserialize)]
struct CliData {
    models: Vec<CliModel>,
}

#[derive(Deserialize)]
struct CliModel {
    id: String,
    label: String,
}

/// The models in `agy --output-format json models` output, by base model.
/// `agy` lists a model with effort levels once per level, the level folded
/// into both the id and the label (`gemini-3.8-flash-high`,
/// `Gemini 3.8 Flash (High)`), and a model without levels once, bare.
fn cli_catalog(stdout: &str) -> Result<Catalog, serde_json::Error> {
    let output: CliOutput = serde_json::from_str(stdout)?;
    let mut catalog = Catalog::new();
    for CliModel { id, label } in output.command.data.models {
        if let Some((base, level)) = folded_level(&id, &label) {
            catalog
                .entry(base.to_owned())
                .or_insert(None)
                .get_or_insert_with(BTreeSet::new)
                .insert(level);
        } else {
            catalog.entry(id).or_insert(None);
        }
    }
    Ok(catalog)
}

/// The base model and level of a folded entry, or `None` for a bare one.
fn folded_level<'a>(id: &'a str, label: &str) -> Option<(&'a str, String)> {
    let (_, suffix) = label.strip_suffix(')')?.rsplit_once(" (")?;
    let level = suffix.to_lowercase();
    if !CLI_EFFORT_LEVELS.contains(&level.as_str()) {
        return None;
    }
    let base = id.strip_suffix(level.as_str())?.strip_suffix('-')?;
    Some((base, level))
}

/// Every way `cli` differs from `table`, one line each, so a failure says what
/// to change.
fn differences(table: &Catalog, cli: &Catalog) -> Vec<String> {
    let mut lines = Vec::new();
    for (model, efforts) in table {
        match cli.get(model) {
            None => lines.push(format!("{model}: in the table, but agy does not list it")),
            Some(listed) if listed != efforts => lines.push(format!(
                "{model}: the table has efforts {efforts:?}, agy lists {listed:?}"
            )),
            Some(_) => {}
        }
    }
    for (model, efforts) in cli {
        if !table.contains_key(model) {
            lines.push(format!(
                "{model}: agy lists it with efforts {efforts:?}, the table lacks it"
            ));
        }
    }
    lines
}

fn catalog(entries: &[(&str, Option<&[&str]>)]) -> Catalog {
    entries
        .iter()
        .map(|(model, efforts)| {
            let efforts = efforts.map(|levels| levels.iter().map(|&l| l.to_owned()).collect());
            ((*model).to_owned(), efforts)
        })
        .collect()
}

#[test]
fn the_cli_model_list_groups_folded_levels_by_base_model() {
    // Shaped like agy 1.4.2's output. The two Claude 4.6 entries are as
    // `agy models` listed them at 1.1.19: bare, with `(Thinking)` in the label,
    // and for Opus in the id too.
    let stdout = r#"{"status":"SUCCESS","command":{"name":"models","data":{"models":[
        {"id":"gemini-3.8-flash-high","label":"Gemini 3.8 Flash (High)"},
        {"id":"gemini-3.8-flash-low","label":"Gemini 3.8 Flash (Low)"},
        {"id":"gpt-oss-120b-medium","label":"GPT-OSS 120B (Medium)"},
        {"id":"claude-sonnet-4-6","label":"Claude Sonnet 4.6 (Thinking)"},
        {"id":"claude-opus-4-6-thinking","label":"Claude Opus 4.6 (Thinking)"}
    ]}}}"#;

    assert_eq!(
        cli_catalog(stdout).unwrap(),
        catalog(&[
            ("gemini-3.8-flash", Some(&["high", "low"])),
            ("gpt-oss-120b", Some(&["medium"])),
            ("claude-sonnet-4-6", None),
            ("claude-opus-4-6-thinking", None),
        ])
    );
}

#[test]
fn every_difference_names_the_model_and_what_changed() {
    let table = catalog(&[
        ("kept", Some(&["low"])),
        ("retired", Some(&["low"])),
        ("relevelled", Some(&["low", "high"])),
    ]);
    let cli = catalog(&[
        ("kept", Some(&["low"])),
        ("relevelled", Some(&["medium"])),
        ("launched", None),
    ]);

    assert_eq!(
        differences(&table, &cli),
        [
            r#"relevelled: the table has efforts Some({"high", "low"}), agy lists Some({"medium"})"#,
            "retired: in the table, but agy does not list it",
            "launched: agy lists it with efforts None, the table lacks it",
        ]
    );
    assert!(differences(&table, &table).is_empty());
}

/// The pickers key their buttons by model id, and [`table`] folds the file into
/// a map, so a repeated id, including one in both lists, would pass the live
/// comparison while breaking the window.
#[test]
fn each_model_id_appears_once_in_the_table() {
    let table = parsed_table();
    let mut seen = BTreeSet::new();
    let left_out = table.left_out.iter().map(|left_out| &left_out.model);
    for model in table.models.iter().chain(left_out) {
        assert!(
            seen.insert(model.value.as_str()),
            "{} appears more than once in antigravityModels.json",
            model.value
        );
    }
}

/// Leaving a model out is a product decision, and the file is where a reader
/// of the table finds out why.
#[test]
fn every_left_out_model_says_why() {
    for left_out in parsed_table().left_out {
        assert!(
            !left_out.reason.trim().is_empty(),
            "{} is left out of antigravityModels.json with no reason",
            left_out.model.value
        );
    }
}

#[test]
fn the_built_in_antigravity_defaults_name_only_offered_models() {
    let offered = as_catalog(&parsed_table().models);
    let preferences = Preferences::default();
    let defaults = &preferences.agent_defaults[&HarnessKind::Antigravity];

    for model in &defaults.model_choices {
        assert!(
            offered.contains_key(model),
            "built-in quick choice {model} is not offered in antigravityModels.json"
        );
    }
    let default_model = defaults.default_model.as_deref().unwrap();
    let default_effort = defaults.default_effort.as_deref().unwrap();
    assert!(
        offered[default_model]
            .as_ref()
            .is_some_and(|levels| levels.contains(default_effort)),
        "the built-in default {default_model} does not take {default_effort}"
    );
}

/// `agy models` sends no prompt: it fetches the account's model list, so this
/// costs no quota. The global `--output-format` goes before the subcommand;
/// `agy models --output-format json` is a usage error.
#[test]
#[ignore = "requires antigravity installed — run with: make test-live"]
#[serial_test::serial(antigravity)]
fn live_antigravity_model_table_matches_the_cli_model_list() {
    let output = Command::new("agy")
        .args(["--output-format", "json", "models"])
        .stdin(Stdio::null())
        .output()
        .expect("agy must be on PATH");
    assert!(
        output.status.success(),
        "agy models failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let cli = cli_catalog(&stdout).unwrap_or_else(|e| {
        panic!(
            "agy's model list is not the JSON shape this test reads ({e}); if agy is not \
             signed in, run `agy` first. stdout: {stdout} stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    let differences = differences(&table(), &cli);
    assert!(
        differences.is_empty(),
        "desktop/src/lib/antigravityModels.json no longer matches agy's model list for the \
         signed-in account. Business and Google Cloud sign-ins can hide models, so before \
         deleting a model reported missing, confirm it is gone: dispatch it bare with \
         `agy -p ack --model <id>` and no `--effort` (it then reports `not recognized as a \
         known model`; with `--effort` agy misreports an unknown model as `--effort is not \
         supported`), or check from a second account. A model agy newly lists goes in \
         `models` (offered) or `left_out` (with a reason); which is a product decision. \
         Then update the file, and any default that names a model agy dropped \
         (DEFAULT_AGENT_SELECTIONS in agentSelection.ts, default_agent_defaults in \
         preferences.rs), and record the change in harness-behavior.md §3.3/§3.4:\n{}",
        differences.join("\n")
    );
}

/// What one dispatch of an offered model reported, live and on reopen.
struct DispatchReport {
    outcome: Option<TurnOutcome>,
    live: Option<(Option<String>, Option<String>)>,
    reopened: Option<(Option<String>, Option<String>)>,
}

async fn dispatch_ack(
    adapter: &AntigravityAdapter,
    model: &str,
    effort: Option<&str>,
) -> DispatchReport {
    let cwd = tempfile::TempDir::new().expect("tempdir");
    let agent = AgentRecord {
        model: Some(model.to_owned()),
        effort: effort.map(str::to_owned),
        model_choices: Vec::new(),
        effort_choices: Vec::new(),
        forked_from_session: None,
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "model-table-agy".to_owned(),
        harness: HarnessKind::Antigravity,
        session_locator: None,
        created_at: chrono::Utc::now(),
    };
    let stream = adapter
        .dispatch(
            &agent,
            cwd.path(),
            "Reply with only the word ack.",
            Uuid::now_v7(),
            DispatchOptions::default(),
        )
        .await
        .expect("agy must be on PATH");
    let events: Vec<AdapterEvent> = stream.collect().await;

    let mut report = DispatchReport {
        outcome: None,
        live: None,
        reopened: None,
    };
    let mut conversation = None;
    for event in events {
        match event {
            AdapterEvent::TurnEnd {
                outcome,
                model,
                effort,
                ..
            } => {
                report.outcome = Some(outcome);
                report.live = Some((model, effort));
            }
            AdapterEvent::SessionLocatorCaptured {
                locator: SessionLocator::Uuid(uuid),
            } => conversation = Some(uuid),
            _ => {}
        }
    }
    if let Some(conversation) = conversation {
        let home = PathBuf::from(std::env::var("HOME").expect("HOME set"));
        let transcript =
            load_antigravity_transcript(&home, cwd.path(), Some(conversation), agent.id)
                .expect("the conversation reopens");
        report.reopened = transcript
            .turns
            .into_iter()
            .rev()
            .find_map(|turn| match turn {
                Turn::Agent { model, effort, .. } => Some((model, effort)),
                _ => None,
            });
    }
    report
}

/// Dispatches every offered model at every level the table lists for it, and
/// checks that the turn footer and the reopened history name the model by its
/// picker label and the level the agent was set to. `agy` announces the pair as
/// one display name, `Gemini 4 Neon (XHigh)`, and the adapter splits the level
/// off only when it names a real one, so a level the split does not know, or a
/// label that drifts from agy's name, shows here. Eight `ack` turns at
/// agy 1.5.12; every mismatch is collected so one run reports them all.
#[tokio::test]
#[ignore = "requires antigravity installed — run with: make test-live"]
#[serial_test::serial(antigravity)]
async fn live_antigravity_offered_models_dispatch_at_every_level() {
    let adapter = AntigravityAdapter::new();
    let mut failures = Vec::new();
    for model in parsed_table().models {
        let levels: Vec<Option<String>> = match &model.efforts {
            Some(levels) => levels.iter().cloned().map(Some).collect(),
            None => vec![None],
        };
        for level in levels {
            let pair = format!(
                "{} {}",
                model.value,
                level.as_deref().unwrap_or("(no effort)")
            );
            let report = dispatch_ack(&adapter, &model.value, level.as_deref()).await;
            let expected = Some((Some(model.label.clone()), level.clone()));
            if !matches!(report.outcome, Some(TurnOutcome::Completed)) {
                failures.push(format!("{pair}: the turn ended {:?}", report.outcome));
            }
            if report.live != expected {
                failures.push(format!(
                    "{pair}: live (model, effort) was {:?}, expected {expected:?}",
                    report.live
                ));
            }
            if report.reopened != expected {
                failures.push(format!(
                    "{pair}: reopened (model, effort) was {:?}, expected {expected:?}",
                    report.reopened
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
