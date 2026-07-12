//! Antigravity tool-facet classification.
//!
//! One classifier for both call sites (the live tail in `parser.rs` and the
//! reopen hydrate in `session_file.rs`) — both consume the same
//! `transcript.jsonl` `tool_calls` records, so agreement is structural.
//!
//! Vocabulary probed live @ agy 1.0.16 (fixture
//! `tests/fixtures/antigravity/tool-vocabulary.transcript.jsonl`;
//! `docs/harness-behavior.md` §3.6): `run_command`, `view_file`,
//! `replace_file_content`, `write_to_file`, `grep_search`, `list_dir` — a
//! full structured vocabulary with content and absolute paths, superseding
//! the older shell-only capture.
//!
//! **Arg encoding.** In `transcript.jsonl` every arg value is a *string
//! containing a JSON literal* (`"StartLine": "1"`, `"TargetContent":
//! "\"foo\""`). [`arg_str`] decodes one level when the inner literal is a
//! string and falls back to the raw value otherwise, which also transparently
//! handles the raw-typed args of `transcript_full.jsonl` should the source
//! ever switch.

use serde_json::Value;

use crate::facets::{EditChange, EditPair, EditedFile, ToolFacet, cap_content};

/// Classify one Antigravity tool call. Missing required args → `Other`
/// (never fabricate a facet from a partial shape). Paths pass through as-is:
/// observed args are absolute, and the transcript offers no cwd to resolve
/// against beyond `run_command`'s own `Cwd` argument.
pub(crate) fn classify_antigravity_tool_facet(name: &str, args: &Value) -> ToolFacet {
    match name {
        "run_command" => match arg_str(args, "CommandLine") {
            Some(command) => ToolFacet::Shell {
                command,
                cwd: arg_str(args, "Cwd"),
            },
            None => ToolFacet::Other,
        },
        "view_file" => match arg_str(args, "AbsolutePath") {
            Some(path) => ToolFacet::Read { path },
            None => ToolFacet::Other,
        },
        "replace_file_content" => {
            let (Some(path), Some(old), Some(new)) = (
                arg_str(args, "TargetFile"),
                arg_str(args, "TargetContent"),
                arg_str(args, "ReplacementContent"),
            ) else {
                return ToolFacet::Other;
            };
            let (old, t1) = cap_content(&old);
            let (new, t2) = cap_content(&new);
            ToolFacet::Edit {
                files: vec![EditedFile {
                    path,
                    change: EditChange::Modified,
                    edits: vec![EditPair { old, new }],
                    truncated: t1 || t2,
                }],
            }
        }
        "write_to_file" => {
            let (Some(path), Some(content)) =
                (arg_str(args, "TargetFile"), arg_str(args, "CodeContent"))
            else {
                return ToolFacet::Other;
            };
            let (content, truncated) = cap_content(&content);
            ToolFacet::Write {
                path,
                content,
                truncated,
            }
        }
        "grep_search" => match arg_str(args, "Query") {
            Some(pattern) => ToolFacet::Search {
                pattern,
                path: arg_str(args, "SearchPath"),
            },
            None => ToolFacet::Other,
        },
        // `list_dir` and anything unobserved render via the generic path.
        _ => ToolFacet::Other,
    }
}

/// Extract a string arg, decoding transcript.jsonl's one level of JSON
/// string-encoding (`"\"foo\"" → foo`); a value that isn't an encoded string
/// is returned verbatim (`"pwd" → pwd`, and raw-typed `transcript_full`
/// strings pass through unchanged).
fn arg_str(args: &Value, key: &str) -> Option<String> {
    let raw = args.get(key).and_then(Value::as_str)?;
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::String(inner)) => Some(inner),
        _ => Some(raw.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Args exactly as transcript.jsonl encodes them (JSON-encoded strings).
    #[test]
    fn run_command_decodes_encoded_args() {
        let facet = classify_antigravity_tool_facet(
            "run_command",
            &json!({"CommandLine": "\"ls -F\"", "Cwd": "\"/tmp/agy-probe\"", "WaitMsBeforeAsync": "0"}),
        );
        assert_eq!(
            facet,
            ToolFacet::Shell {
                command: "ls -F".to_owned(),
                cwd: Some("/tmp/agy-probe".to_owned())
            }
        );
    }

    // And raw args (transcript_full.jsonl style) pass through unchanged.
    #[test]
    fn run_command_accepts_raw_string_args() {
        let facet = classify_antigravity_tool_facet("run_command", &json!({"CommandLine": "pwd"}));
        assert_eq!(
            facet,
            ToolFacet::Shell {
                command: "pwd".to_owned(),
                cwd: None
            }
        );
    }

    #[test]
    fn replace_file_content_maps_to_edit_with_content() {
        let facet = classify_antigravity_tool_facet(
            "replace_file_content",
            &json!({
                "TargetFile": "\"/tmp/alpha.txt\"",
                "TargetContent": "\"foo\"",
                "ReplacementContent": "\"bar\"",
                "StartLine": "1", "EndLine": "1", "AllowMultiple": "false"
            }),
        );
        let ToolFacet::Edit { files } = facet else {
            panic!("expected Edit");
        };
        assert_eq!(files[0].path, "/tmp/alpha.txt");
        assert_eq!(files[0].change, EditChange::Modified);
        assert_eq!(
            files[0].edits,
            vec![EditPair {
                old: "foo".to_owned(),
                new: "bar".to_owned()
            }]
        );
    }

    #[test]
    fn view_write_grep_map() {
        assert_eq!(
            classify_antigravity_tool_facet(
                "view_file",
                &json!({"AbsolutePath": "\"/tmp/a.txt\""})
            ),
            ToolFacet::Read {
                path: "/tmp/a.txt".to_owned()
            }
        );
        assert_eq!(
            classify_antigravity_tool_facet(
                "write_to_file",
                &json!({"TargetFile": "\"/tmp/theta.txt\"", "CodeContent": "\"hello world\"", "Overwrite": "false"})
            ),
            ToolFacet::Write {
                path: "/tmp/theta.txt".to_owned(),
                content: "hello world".to_owned(),
                truncated: false
            }
        );
        assert_eq!(
            classify_antigravity_tool_facet(
                "grep_search",
                &json!({"Query": "\"needle\"", "SearchPath": "\"/tmp\"", "IsRegex": "false"})
            ),
            ToolFacet::Search {
                pattern: "needle".to_owned(),
                path: Some("/tmp".to_owned())
            }
        );
    }

    #[test]
    fn list_dir_and_unknown_map_to_other() {
        assert_eq!(
            classify_antigravity_tool_facet("list_dir", &json!({"DirectoryPath": "\"/tmp\""})),
            ToolFacet::Other
        );
        assert_eq!(
            classify_antigravity_tool_facet("future_tool", &json!({})),
            ToolFacet::Other
        );
    }

    #[test]
    fn missing_required_args_degrade_to_other() {
        assert_eq!(
            classify_antigravity_tool_facet("run_command", &json!({})),
            ToolFacet::Other
        );
        assert_eq!(
            classify_antigravity_tool_facet(
                "replace_file_content",
                &json!({"TargetFile": "\"/tmp/a\""})
            ),
            ToolFacet::Other
        );
        assert_eq!(
            classify_antigravity_tool_facet("run_command", &Value::Null),
            ToolFacet::Other
        );
    }

    // --- Fixture-driven coverage: recorded @ agy 1.0.16 (probe 2026-07-10) ---

    /// Drives the real live-parser path (`record_to_live_events`) over the
    /// recorded transcript and asserts each observed tool maps to its facet
    /// with the string-encoded args decoded. The reopen path consumes the
    /// same records through the same classifier, so live/disk agreement is
    /// structural for this harness.
    #[test]
    fn recorded_transcript_vocabulary_maps_to_expected_facets() {
        use super::super::parser::{
            AntigravityParserState, TranscriptRecord, record_to_live_events,
        };
        use crate::events::AdapterEvent;
        use crate::facets::{EditChange, ToolFacet};

        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/antigravity/tool-vocabulary.transcript.jsonl");
        let content = std::fs::read_to_string(&path).unwrap();
        let mut state = AntigravityParserState::default();
        let turn_id = uuid::Uuid::now_v7();
        let mut facets: Vec<(String, ToolFacet)> = Vec::new();
        for line in content.lines().filter(|l| !l.trim().is_empty()) {
            let Ok(rec) = serde_json::from_str::<TranscriptRecord>(line) else {
                continue;
            };
            for event in record_to_live_events(&rec, turn_id, &mut state) {
                if let AdapterEvent::ToolStarted { name, facet, .. } = event {
                    facets.push((name, facet));
                }
            }
        }

        let facet_for = |tool: &str| -> &ToolFacet {
            &facets
                .iter()
                .find(|(n, _)| n == tool)
                .unwrap_or_else(|| panic!("fixture must contain a {tool} call"))
                .1
        };

        assert!(matches!(
            facet_for("run_command"),
            ToolFacet::Shell { cwd: Some(_), .. }
        ));
        assert!(matches!(facet_for("view_file"), ToolFacet::Read { .. }));
        let ToolFacet::Edit { files } = facet_for("replace_file_content") else {
            panic!("replace_file_content must map to Edit");
        };
        assert_eq!(files[0].change, EditChange::Modified);
        assert_eq!(files[0].edits[0].old, "foo");
        assert_eq!(files[0].edits[0].new, "bar");
        let ToolFacet::Write { content: c, .. } = facet_for("write_to_file") else {
            panic!("write_to_file must map to Write");
        };
        assert_eq!(c, "hello world");
        assert!(matches!(facet_for("grep_search"), ToolFacet::Search { .. }));
        assert_eq!(*facet_for("list_dir"), ToolFacet::Other);
    }
}
