//! The window's Antigravity model table, checked from Rust.
//!
//! The table is `desktop/src/lib/antigravityModels.json`. The frontend reads it
//! for the model and effort pickers, and these tests read the same file, so
//! there is one list and nothing here can drift from what the window offers.
//! Google adds and retires Antigravity models without a CLI release, so the
//! live test compares the file with `agy`'s own list: a model Google retires
//! fails `make test-live-antigravity` instead of a user's new agent.

use std::collections::{BTreeMap, BTreeSet};
use std::process::{Command, Stdio};

use serde::Deserialize;
use switchboard_core::HarnessKind;

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
    models: Vec<TableModel>,
}

#[derive(Deserialize)]
struct TableModel {
    value: String,
    efforts: Option<Vec<String>>,
}

fn table() -> Catalog {
    let table: Table = serde_json::from_str(TABLE).expect("antigravityModels.json parses");
    table
        .models
        .into_iter()
        .map(|model| (model.value, model.efforts.map(|e| e.into_iter().collect())))
        .collect()
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
fn cli_catalog(stdout: &str) -> Catalog {
    let output: CliOutput = serde_json::from_str(stdout).unwrap_or_else(|e| {
        panic!("agy's model list is not the JSON shape this test reads ({e}): {stdout}")
    });
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
    catalog
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
        cli_catalog(stdout),
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

#[test]
fn the_built_in_antigravity_defaults_name_only_models_in_the_table() {
    let table = table();
    let preferences = Preferences::default();
    let defaults = &preferences.agent_defaults[&HarnessKind::Antigravity];

    for model in &defaults.model_choices {
        assert!(
            table.contains_key(model),
            "built-in quick choice {model} is not in antigravityModels.json"
        );
    }
    let default_model = defaults.default_model.as_deref().unwrap();
    let default_effort = defaults.default_effort.as_deref().unwrap();
    assert!(
        table[default_model]
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

    let differences = differences(
        &table(),
        &cli_catalog(&String::from_utf8_lossy(&output.stdout)),
    );
    assert!(
        differences.is_empty(),
        "desktop/src/lib/antigravityModels.json no longer matches agy's model list. Update \
         it, and any default that names a model agy dropped (DEFAULT_AGENT_SELECTIONS in \
         agentSelection.ts, default_agent_defaults in preferences.rs), then record the \
         change in harness-behavior.md §3.3/§3.4:\n{}",
        differences.join("\n")
    );
}
