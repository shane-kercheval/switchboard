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
//! MCP calls were re-probed read-only @ agy 1.1.2 on 2026-07-14. The
//! transcript records the raw dispatcher name `call_mcp_tool` with encoded
//! `ServerName`, `ToolName`, and `Arguments` fields rather than exposing the
//! underlying tool as the raw call. The raw wrapper remains the tool name/input
//! for provenance; only kind and facet use the decoded identity and arguments.
//! The sanitized wrapper record, with synthetic surrounding result/answer
//! records, is `tests/fixtures/antigravity/mcp-tool-wrapper.transcript.jsonl`.
//!
//! **Arg encoding.** In compact `transcript.jsonl`, every arg value is a
//! string containing a JSON literal (`"StartLine": "1"`,
//! `"TargetContent": "\"foo\""`). `transcript_full.jsonl` carries native
//! strings instead. Callers identify the source explicitly so source content
//! that happens to be valid JSON is never decoded twice.

use serde_json::Value;

use crate::events::ToolKind;
use crate::facets::{
    EditChange, EditPair, EditedFile, ToolFacet, cap_content, classify_mcp_tool_facet,
};

#[derive(Clone, Copy)]
pub(crate) enum ArgumentEncoding {
    CompactJsonStrings,
    Native,
}

/// Classify one raw Antigravity tool call. A valid `call_mcp_tool` wrapper is
/// normalized to MCP identity and semantic arguments; malformed wrappers and
/// native tools retain the existing builtin/generic behavior.
pub(crate) fn classify_antigravity_tool(name: &str, args: &Value) -> (ToolKind, ToolFacet) {
    classify_antigravity_tool_with_encoding(name, args, ArgumentEncoding::CompactJsonStrings)
}

pub(crate) fn classify_antigravity_tool_with_encoding(
    name: &str,
    args: &Value,
    encoding: ArgumentEncoding,
) -> (ToolKind, ToolFacet) {
    if let Some((server, tool, arguments)) = decode_mcp_wrapper(name, args) {
        return (
            ToolKind::Mcp,
            classify_mcp_tool_facet(&server, &tool, &arguments),
        );
    }
    let facet = match encoding {
        ArgumentEncoding::CompactJsonStrings => classify_antigravity_tool_facet(name, args),
        ArgumentEncoding::Native => {
            classify_antigravity_tool_facet_with_encoding(name, args, encoding)
        }
    };
    (ToolKind::Builtin, facet)
}

fn decode_mcp_wrapper(name: &str, args: &Value) -> Option<(String, String, Value)> {
    if name != "call_mcp_tool" {
        return None;
    }
    let server = arg_str(args, "ServerName", ArgumentEncoding::CompactJsonStrings)?;
    let tool = arg_str(args, "ToolName", ArgumentEncoding::CompactJsonStrings)?;
    if server.trim().is_empty() || tool.trim().is_empty() {
        return None;
    }
    let arguments = match args.get("Arguments")? {
        Value::String(raw) => serde_json::from_str::<Value>(raw).ok()?,
        Value::Object(object) => Value::Object(object.clone()),
        _ => return None,
    };
    if !arguments.is_object() {
        return None;
    }
    Some((server, tool, arguments))
}

/// Classify one Antigravity tool call. Missing required args → `Other`
/// (never fabricate a facet from a partial shape). Paths pass through as-is:
/// observed args are absolute, and the transcript offers no cwd to resolve
/// against beyond `run_command`'s own `Cwd` argument.
pub(crate) fn classify_antigravity_tool_facet(name: &str, args: &Value) -> ToolFacet {
    classify_antigravity_tool_facet_with_encoding(name, args, ArgumentEncoding::CompactJsonStrings)
}

fn classify_antigravity_tool_facet_with_encoding(
    name: &str,
    args: &Value,
    encoding: ArgumentEncoding,
) -> ToolFacet {
    match name {
        "run_command" => match arg_str(args, "CommandLine", encoding) {
            Some(command) => ToolFacet::Shell {
                command,
                cwd: arg_str(args, "Cwd", encoding),
            },
            None => ToolFacet::Other,
        },
        "view_file" => match arg_str(args, "AbsolutePath", encoding) {
            Some(path) => ToolFacet::Read { path },
            None => ToolFacet::Other,
        },
        "replace_file_content" => {
            let (Some(path), Some(old), Some(new)) = (
                arg_str(args, "TargetFile", encoding),
                content_arg(args, "TargetContent", encoding),
                content_arg(args, "ReplacementContent", encoding),
            ) else {
                return ToolFacet::Other;
            };
            let source_truncated = old.source_truncated || new.source_truncated;
            let (old, t1) = cap_content(&old.value);
            let (new, t2) = cap_content(&new.value);
            ToolFacet::Edit {
                files: vec![EditedFile {
                    path,
                    change: EditChange::Modified,
                    edits: vec![EditPair { old, new }],
                    truncated: source_truncated || t1 || t2,
                }],
            }
        }
        "write_to_file" => {
            let (Some(path), Some(content)) = (
                arg_str(args, "TargetFile", encoding),
                content_arg(args, "CodeContent", encoding),
            ) else {
                return ToolFacet::Other;
            };
            let (content_value, truncated) = cap_content(&content.value);
            ToolFacet::Write {
                path,
                content: content_value,
                truncated: content.source_truncated || truncated,
            }
        }
        "grep_search" => match arg_str(args, "Query", encoding) {
            Some(pattern) => ToolFacet::Search {
                pattern,
                path: arg_str(args, "SearchPath", encoding),
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
fn arg_str(args: &Value, key: &str, encoding: ArgumentEncoding) -> Option<String> {
    content_arg(args, key, encoding).map(|arg| arg.value)
}

struct ContentArg {
    value: String,
    source_truncated: bool,
}

fn content_arg(args: &Value, key: &str, encoding: ArgumentEncoding) -> Option<ContentArg> {
    let raw = args.get(key).and_then(Value::as_str)?;
    if matches!(encoding, ArgumentEncoding::Native) {
        return Some(ContentArg {
            value: raw.to_owned(),
            source_truncated: false,
        });
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::String(inner)) => Some(ContentArg {
            value: inner,
            source_truncated: false,
        }),
        _ => decode_compact_truncated_string(raw).or_else(|| {
            Some(ContentArg {
                value: raw.to_owned(),
                source_truncated: false,
            })
        }),
    }
}

/// `transcript.jsonl` clips large encoded values before their closing quote,
/// then appends a literal newline plus `<truncated N bytes>`. Recover the
/// complete JSON-decodable prefix so fallback reads show real line breaks and
/// an honest truncated diff rather than escaped source plus the vendor marker.
fn decode_compact_truncated_string(raw: &str) -> Option<ContentArg> {
    if !raw.starts_with('"') {
        return None;
    }
    let (encoded_prefix, marker) = raw.rsplit_once("\n<truncated ")?;
    let omitted = marker.strip_suffix(" bytes>")?;
    omitted.parse::<usize>().ok()?;

    let mut end = encoded_prefix.len();
    // A clipped JSON escape needs at most six characters removed; a clipped
    // low-surrogate escape may also require removing its preceding high
    // surrogate before serde accepts the recovered string.
    for _ in 0..=12 {
        let candidate = format!("{}\"", &encoded_prefix[..end]);
        if let Ok(value) = serde_json::from_str::<String>(&candidate) {
            return Some(ContentArg {
                value,
                source_truncated: true,
            });
        }
        let (previous, _) = encoded_prefix[..end].char_indices().next_back()?;
        end = previous;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mcp_wrapper(server: &str, tool: &str, arguments: &Value) -> Value {
        json!({
            "ServerName": serde_json::to_string(server).unwrap(),
            "ToolName": serde_json::to_string(tool).unwrap(),
            "Arguments": serde_json::to_string(arguments).unwrap(),
            "toolAction": "\"Synthetic action\"",
            "toolSummary": "\"Synthetic summary\""
        })
    }

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
    fn mcp_wrapper_decodes_generic_read_only_call() {
        let (kind, facet) = classify_antigravity_tool(
            "call_mcp_tool",
            &mcp_wrapper("notes_alias", "get_context", &json!({})),
        );
        assert_eq!(kind, ToolKind::Mcp);
        assert_eq!(
            facet,
            ToolFacet::Mcp {
                server: "notes_alias".to_owned(),
                tool: "get_context".to_owned(),
                mutation: None,
            }
        );
    }

    #[test]
    fn mcp_wrapper_accepts_full_transcript_typed_arguments() {
        let (kind, facet) = classify_antigravity_tool(
            "call_mcp_tool",
            &json!({
                "ServerName": "notes_alias",
                "ToolName": "edit_content",
                "Arguments": {
                    "id": "note-example",
                    "type": "note",
                    "old_str": "before",
                    "new_str": "after"
                }
            }),
        );
        assert_eq!(kind, ToolKind::Mcp);
        assert!(matches!(
            facet,
            ToolFacet::Mcp {
                mutation: Some(mutation),
                ..
            } if matches!(mutation.as_ref(), crate::facets::McpMutation::TextEdit { .. })
        ));
    }

    #[test]
    fn mcp_wrapper_enriches_all_mutation_families() {
        let (_, edit) = classify_antigravity_tool(
            "call_mcp_tool",
            &mcp_wrapper(
                "notes_alias",
                "edit_content",
                &json!({
                    "id": "note-example",
                    "type": "note",
                    "old_str": "before text",
                    "new_str": "after text"
                }),
            ),
        );
        assert!(matches!(
            edit,
            ToolFacet::Mcp {
                mutation: Some(mutation),
                ..
            } if matches!(mutation.as_ref(), crate::facets::McpMutation::TextEdit { .. })
        ));

        let (_, creation) = classify_antigravity_tool(
            "call_mcp_tool",
            &mcp_wrapper(
                "prompts_alias",
                "create_prompt",
                &json!({"name": "sample-prompt", "content": "Prompt body"}),
            ),
        );
        assert!(matches!(
            creation,
            ToolFacet::Mcp {
                mutation: Some(mutation),
                ..
            } if matches!(mutation.as_ref(), crate::facets::McpMutation::TextCreation { .. })
        ));

        let (_, record) = classify_antigravity_tool(
            "call_mcp_tool",
            &mcp_wrapper(
                "notes_alias",
                "create_bookmark",
                &json!({"url": "https://example.com", "title": "Example"}),
            ),
        );
        assert!(matches!(
            record,
            ToolFacet::Mcp {
                mutation: Some(mutation),
                ..
            } if matches!(mutation.as_ref(), crate::facets::McpMutation::RecordCreation { .. })
        ));
    }

    #[test]
    fn malformed_mcp_wrappers_degrade_to_builtin_other() {
        let cases = [
            json!({
                "ToolName": "\"get_context\"",
                "Arguments": "{}"
            }),
            json!({
                "ServerName": "\"\"",
                "ToolName": "\"get_context\"",
                "Arguments": "{}"
            }),
            json!({
                "ServerName": "\"notes_alias\"",
                "Arguments": "{}"
            }),
            json!({
                "ServerName": "\"notes_alias\"",
                "ToolName": "\"\"",
                "Arguments": "{}"
            }),
            json!({
                "ServerName": "\"notes_alias\"",
                "ToolName": "\"get_context\"",
                "Arguments": "not-json"
            }),
            json!({
                "ServerName": "\"notes_alias\"",
                "ToolName": "\"get_context\"",
                "Arguments": "[]"
            }),
        ];

        for args in cases {
            assert_eq!(
                classify_antigravity_tool("call_mcp_tool", &args),
                (ToolKind::Builtin, ToolFacet::Other)
            );
        }
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
    fn compact_truncated_edit_decodes_prefix_and_marks_facet_truncated() {
        let facet = classify_antigravity_tool_facet(
            "replace_file_content",
            &json!({
                "TargetFile": "\"/tmp/large.txt\"",
                "TargetContent": "\"before\"",
                "ReplacementContent": "\"Line 1\\nLine 2\\nLine 3\n<truncated 4154 bytes>"
            }),
        );
        let ToolFacet::Edit { files } = facet else {
            panic!("expected Edit");
        };
        assert_eq!(files[0].edits[0].new, "Line 1\nLine 2\nLine 3");
        assert!(files[0].truncated);
    }

    #[test]
    fn full_transcript_native_content_that_looks_encoded_is_preserved() {
        let (_, facet) = classify_antigravity_tool_with_encoding(
            "replace_file_content",
            &json!({
                "TargetFile": "/tmp/quoted.txt",
                "TargetContent": "before",
                "ReplacementContent": "\"literal quotes\""
            }),
            ArgumentEncoding::Native,
        );
        let ToolFacet::Edit { files } = facet else {
            panic!("expected Edit");
        };
        assert_eq!(files[0].edits[0].new, "\"literal quotes\"");
    }

    #[test]
    fn compact_truncation_inside_an_escape_keeps_only_valid_decoded_content() {
        let decoded =
            decode_compact_truncated_string("\"Line 1\\nLine 2\\u00\n<truncated 2 bytes>").unwrap();
        assert_eq!(decoded.value, "Line 1\nLine 2");
        assert!(decoded.source_truncated);
    }

    #[test]
    fn compact_truncation_inside_a_surrogate_pair_drops_the_incomplete_pair() {
        let decoded =
            decode_compact_truncated_string("\"Line 1\\nLine 2\\ud83d\\ud8\n<truncated 4 bytes>")
                .unwrap();
        assert_eq!(decoded.value, "Line 1\nLine 2");
        assert!(decoded.source_truncated);
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

    #[test]
    fn current_mcp_wrapper_fixture_matches_live_and_hydrated_facets() {
        use super::super::parser::{
            AntigravityParserState, TranscriptRecord, record_to_live_events,
        };
        use super::super::session_file::parse_antigravity_transcript_content;
        use crate::events::AdapterEvent;
        use crate::transcript::{Turn, TurnItem};

        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/antigravity/mcp-tool-wrapper.transcript.jsonl");
        let content = std::fs::read_to_string(path).unwrap();
        let mut state = AntigravityParserState::default();
        let turn_id = uuid::Uuid::now_v7();
        let mut live_facet = None;
        let mut live_completed = None;
        for line in content.lines().filter(|line| !line.trim().is_empty()) {
            let record: TranscriptRecord = serde_json::from_str(line).unwrap();
            for event in record_to_live_events(&record, turn_id, &mut state) {
                match event {
                    AdapterEvent::ToolStarted {
                        kind,
                        name,
                        input,
                        facet,
                        ..
                    } => {
                        assert_eq!(kind, ToolKind::Mcp);
                        assert_eq!(name, "call_mcp_tool");
                        assert_eq!(input["ServerName"], "\"notes_alias\"");
                        live_facet = Some(facet);
                    }
                    AdapterEvent::ToolCompleted {
                        output, is_error, ..
                    } => live_completed = Some((output, is_error)),
                    _ => {}
                }
            }
        }

        let transcript = parse_antigravity_transcript_content(&content, uuid::Uuid::now_v7());
        let hydrated = transcript
            .turns
            .iter()
            .find_map(|turn| match turn {
                Turn::Agent { items, .. } => items.iter().find_map(|item| match item {
                    TurnItem::Tool {
                        facet,
                        output,
                        is_error,
                        ..
                    } => Some((facet, output, is_error)),
                    TurnItem::Text { .. } => None,
                }),
                Turn::User { .. } | Turn::System { .. } => None,
            })
            .expect("hydrated MCP tool");

        assert_eq!(live_facet.as_ref(), Some(hydrated.0));
        assert_eq!(
            live_completed,
            Some(("context available".to_owned(), false))
        );
        assert_eq!(hydrated.1.as_deref(), Some("context available"));
        assert_eq!(*hydrated.2, Some(false));
    }
}
