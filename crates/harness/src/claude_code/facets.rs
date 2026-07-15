//! Claude Code tool-facet classification.
//!
//! One classifier serving both call sites — the live stream parser
//! (`parser.rs`) and the session-file parser (`session_file.rs`) — which see
//! byte-identical `{name, input}` `tool_use` blocks, so a single mapping keeps
//! the two paths from ever disagreeing.
//!
//! Shapes verified live @ claude 2.1.206 (fixtures
//! `tests/fixtures/claude/tool-vocabulary*.jsonl`; `docs/harness-behavior.md`
//! §3.6). `MultiEdit` and `TodoWrite` no longer exist in current Claude Code
//! (`Edit` absorbed multi-edit; the `Task*` family replaced todos) but are
//! still mapped from their documented shapes so *historical* session files
//! classify — the only unverified mappings here. A wrong legacy shape
//! degrades to `Other`, never errors.

use serde_json::Value;

use crate::facets::{
    EditChange, EditPair, EditedFile, ToolFacet, cap_content, classify_mcp_tool_facet,
    split_mcp_name,
};

/// Classify one Claude tool call. Missing or malformed required fields on
/// built-in tools fall to `Other`. MCP names retain their server/tool identity;
/// malformed mutation arguments only suppress semantic enrichment.
///
/// Paths are passed through as-is: Claude requires absolute paths in its
/// file tools (probe-verified), and the classifier has no cwd to resolve
/// against if that ever changes.
pub(crate) fn classify_claude_tool_facet(name: &str, input: &Value) -> ToolFacet {
    if let Some((server, tool)) = split_mcp_name(name) {
        return classify_mcp_tool_facet(&server, &tool, input);
    }
    match name {
        "Bash" => match str_field(input, "command") {
            Some(command) => ToolFacet::Shell { command, cwd: None },
            None => ToolFacet::Other,
        },
        "Read" => match str_field(input, "file_path") {
            Some(path) => ToolFacet::Read { path },
            None => ToolFacet::Other,
        },
        "Edit" => {
            let (Some(path), Some(old), Some(new)) = (
                str_field(input, "file_path"),
                str_field(input, "old_string"),
                str_field(input, "new_string"),
            ) else {
                return ToolFacet::Other;
            };
            ToolFacet::Edit {
                files: vec![edited_file(path, vec![(old, new)])],
            }
        }
        // Legacy (removed upstream by 2.1.206); documented shape:
        // `{file_path, edits: [{old_string, new_string}]}`.
        "MultiEdit" => {
            let (Some(path), Some(edits)) = (
                str_field(input, "file_path"),
                input.get("edits").and_then(Value::as_array),
            ) else {
                return ToolFacet::Other;
            };
            let pairs: Vec<(String, String)> = edits
                .iter()
                .filter_map(|e| Some((str_field(e, "old_string")?, str_field(e, "new_string")?)))
                .collect();
            if pairs.is_empty() {
                return ToolFacet::Other;
            }
            ToolFacet::Edit {
                files: vec![edited_file(path, pairs)],
            }
        }
        "Write" => {
            let (Some(path), Some(content)) =
                (str_field(input, "file_path"), str_field(input, "content"))
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
        "Grep" | "Glob" => match str_field(input, "pattern") {
            Some(pattern) => ToolFacet::Search {
                pattern,
                path: str_field(input, "path"),
            },
            None => ToolFacet::Other,
        },
        "TodoWrite" | "TaskCreate" | "TaskUpdate" => todo_facet(name, input),
        // Includes `Task` (subagent dispatch), deliberately unmapped — it
        // renders via the generic path; a facet for it is additive later.
        _ => ToolFacet::Other,
    }
}

/// The todo family. `TodoWrite` is the legacy whole-list shape
/// (`{todos: [{content, status, activeForm}]}` — removed upstream);
/// `TaskCreate`/`TaskUpdate` are its current single-item successors.
/// `TaskUpdate` is a status-only update — no task text on the wire, so the
/// id stands in as content (see `TodoItem` docs).
fn todo_facet(name: &str, input: &Value) -> ToolFacet {
    match name {
        "TodoWrite" => {
            let Some(todos) = input.get("todos").and_then(Value::as_array) else {
                return ToolFacet::Other;
            };
            let items: Vec<crate::facets::TodoItem> = todos
                .iter()
                .filter_map(|t| {
                    Some(crate::facets::TodoItem {
                        content: str_field(t, "content")?,
                        status: str_field(t, "status").unwrap_or_default(),
                    })
                })
                .collect();
            if items.is_empty() {
                return ToolFacet::Other;
            }
            ToolFacet::Todo { items }
        }
        "TaskCreate" => match str_field(input, "subject") {
            Some(subject) => ToolFacet::Todo {
                items: vec![crate::facets::TodoItem {
                    content: subject,
                    status: "pending".to_owned(),
                }],
            },
            None => ToolFacet::Other,
        },
        "TaskUpdate" => {
            let (Some(task_id), Some(status)) =
                (str_field(input, "taskId"), str_field(input, "status"))
            else {
                return ToolFacet::Other;
            };
            ToolFacet::Todo {
                items: vec![crate::facets::TodoItem {
                    content: task_id,
                    status,
                }],
            }
        }
        _ => ToolFacet::Other,
    }
}

fn edited_file(path: String, pairs: Vec<(String, String)>) -> EditedFile {
    let mut truncated = false;
    let edits = pairs
        .into_iter()
        .map(|(old, new)| {
            let (old, t1) = cap_content(&old);
            let (new, t2) = cap_content(&new);
            truncated |= t1 || t2;
            EditPair { old, new }
        })
        .collect();
    EditedFile {
        path,
        change: EditChange::Modified,
        edits,
        truncated,
    }
}

fn str_field(obj: &Value, key: &str) -> Option<String> {
    obj.get(key).and_then(Value::as_str).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bash_maps_to_shell() {
        let facet = classify_claude_tool_facet("Bash", &json!({"command": "ls -la"}));
        assert_eq!(
            facet,
            ToolFacet::Shell {
                command: "ls -la".to_owned(),
                cwd: None
            }
        );
    }

    #[test]
    fn edit_maps_to_single_file_single_pair() {
        let facet = classify_claude_tool_facet(
            "Edit",
            &json!({"file_path": "/tmp/a.txt", "old_string": "foo", "new_string": "bar", "replace_all": false}),
        );
        let ToolFacet::Edit { files } = facet else {
            panic!("expected Edit, got {facet:?}");
        };
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "/tmp/a.txt");
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
    fn multi_edit_maps_to_single_file_many_pairs() {
        let facet = classify_claude_tool_facet(
            "MultiEdit",
            &json!({"file_path": "/tmp/b.txt", "edits": [
                {"old_string": "one", "new_string": "1"},
                {"old_string": "three", "new_string": "3"}
            ]}),
        );
        let ToolFacet::Edit { files } = facet else {
            panic!("expected Edit, got {facet:?}");
        };
        assert_eq!(files[0].edits.len(), 2);
    }

    #[test]
    fn write_maps_and_caps_content() {
        let facet = classify_claude_tool_facet(
            "Write",
            &json!({"file_path": "/tmp/c.txt", "content": "hello world"}),
        );
        assert_eq!(
            facet,
            ToolFacet::Write {
                path: "/tmp/c.txt".to_owned(),
                content: "hello world".to_owned(),
                truncated: false
            }
        );
    }

    #[test]
    fn oversized_write_sets_truncated_without_blowing_up() {
        let big = "x".repeat(crate::facets::FACET_CONTENT_CAP + 1);
        let facet = classify_claude_tool_facet(
            "Write",
            &json!({"file_path": "/tmp/d.txt", "content": big}),
        );
        let ToolFacet::Write {
            content, truncated, ..
        } = facet
        else {
            panic!("expected Write");
        };
        assert!(truncated);
        assert_eq!(content.len(), crate::facets::FACET_CONTENT_CAP);
    }

    #[test]
    fn read_grep_glob_map() {
        assert_eq!(
            classify_claude_tool_facet("Read", &json!({"file_path": "/tmp/a.txt"})),
            ToolFacet::Read {
                path: "/tmp/a.txt".to_owned()
            }
        );
        assert_eq!(
            classify_claude_tool_facet("Grep", &json!({"pattern": "needle", "path": "/tmp"})),
            ToolFacet::Search {
                pattern: "needle".to_owned(),
                path: Some("/tmp".to_owned())
            }
        );
        assert_eq!(
            classify_claude_tool_facet("Glob", &json!({"pattern": "**/*.rs"})),
            ToolFacet::Search {
                pattern: "**/*.rs".to_owned(),
                path: None
            }
        );
    }

    #[test]
    fn todo_family_maps_to_todo() {
        let legacy = classify_claude_tool_facet(
            "TodoWrite",
            &json!({"todos": [{"content": "write tests", "status": "in_progress", "activeForm": "Writing tests"}]}),
        );
        let ToolFacet::Todo { items } = legacy else {
            panic!("expected Todo");
        };
        assert_eq!(items[0].content, "write tests");
        assert_eq!(items[0].status, "in_progress");

        let create = classify_claude_tool_facet(
            "TaskCreate",
            &json!({"subject": "Edit alpha.txt", "description": "Change foo to bar"}),
        );
        let ToolFacet::Todo { items } = create else {
            panic!("expected Todo");
        };
        assert_eq!(items[0].status, "pending");

        let update = classify_claude_tool_facet(
            "TaskUpdate",
            &json!({"taskId": "1", "status": "completed"}),
        );
        let ToolFacet::Todo { items } = update else {
            panic!("expected Todo");
        };
        assert_eq!(items[0].status, "completed");
    }

    #[test]
    fn mcp_name_maps_to_mcp() {
        assert_eq!(
            classify_claude_tool_facet("mcp__tiddly__search_items", &json!({"query": "x"})),
            ToolFacet::Mcp {
                server: "tiddly".to_owned(),
                tool: "search_items".to_owned(),
                mutation: None,
            }
        );
    }

    #[test]
    fn mcp_text_edit_uses_the_shared_mutation_classifier() {
        let facet = classify_claude_tool_facet(
            "mcp__notes_alias__edit_content",
            &json!({
                "id": "note-example",
                "type": "note",
                "old_str": "before text",
                "new_str": "after text"
            }),
        );

        let ToolFacet::Mcp {
            server,
            tool,
            mutation: Some(mutation),
        } = facet
        else {
            panic!("expected enriched MCP facet, got {facet:?}");
        };
        assert_eq!(server, "notes_alias");
        assert_eq!(tool, "edit_content");
        assert!(matches!(
            *mutation,
            crate::facets::McpMutation::TextEdit {
                ref target,
                ref before,
                ref after,
                content_truncated: false,
                ..
            } if target == "note · note-example"
                && before == "before text"
                && after == "after text"
        ));
    }

    #[test]
    fn mcp_creation_enrichment_is_independent_of_the_server_alias() {
        let input = json!({"name": "sample-prompt", "content": "Prompt body"});
        let first = classify_claude_tool_facet("mcp__prompts_one__create_prompt", &input);
        let second = classify_claude_tool_facet("mcp__renamed_server__create_prompt", &input);

        let (
            ToolFacet::Mcp {
                server: first_server,
                tool: first_tool,
                mutation: first_mutation,
            },
            ToolFacet::Mcp {
                server: second_server,
                tool: second_tool,
                mutation: second_mutation,
            },
        ) = (first, second)
        else {
            panic!("expected MCP facets");
        };
        assert_eq!(first_server, "prompts_one");
        assert_eq!(second_server, "renamed_server");
        assert_eq!(first_tool, "create_prompt");
        assert_eq!(second_tool, "create_prompt");
        assert_eq!(first_mutation, second_mutation);
        assert!(matches!(
            first_mutation.as_deref(),
            Some(crate::facets::McpMutation::TextCreation {
                target,
                content,
                content_truncated: false,
                ..
            }) if target == "prompt · sample-prompt" && content == "Prompt body"
        ));
    }

    #[test]
    fn malformed_known_mcp_tool_remains_a_basic_mcp_facet() {
        assert_eq!(
            classify_claude_tool_facet(
                "mcp__notes_alias__edit_content",
                &json!({
                    "id": "note-example",
                    "type": "note",
                    "old_str": "before text"
                }),
            ),
            ToolFacet::Mcp {
                server: "notes_alias".to_owned(),
                tool: "edit_content".to_owned(),
                mutation: None,
            }
        );
    }

    #[test]
    fn unknown_and_subagent_tools_map_to_other() {
        assert_eq!(
            classify_claude_tool_facet("Task", &json!({"prompt": "go"})),
            ToolFacet::Other
        );
        assert_eq!(
            classify_claude_tool_facet("SomeFutureTool", &json!({})),
            ToolFacet::Other
        );
    }

    // --- Fixture-driven coverage: recorded @ claude 2.1.206 (probe 2026-07-10) ---

    use std::path::{Path, PathBuf};

    use switchboard_core::AgentId;
    use uuid::Uuid;

    use crate::parser::{ParseOutcome, ParserState, parse_line};
    use crate::transcript::{Turn, TurnItem};
    use crate::{AdapterEvent, load_claude_transcript};

    fn fixture_path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/claude")
            .join(name)
    }

    /// `(tool_use_id, name, input, facet)` for every `ToolStarted` the live
    /// stream parser emits from a fixture.
    fn stream_tools(fixture: &str) -> Vec<(String, String, Value, ToolFacet)> {
        let content = std::fs::read_to_string(fixture_path(fixture)).unwrap();
        let turn_id = Uuid::now_v7();
        let agent_id = AgentId::now_v7();
        let mut state = ParserState::default();
        let mut out = Vec::new();
        for line in content.lines() {
            let events = match parse_line(line, turn_id, agent_id, &mut state) {
                ParseOutcome::Event(e) => vec![e],
                ParseOutcome::Events(es) => es,
                _ => continue,
            };
            for e in events {
                if let AdapterEvent::ToolStarted {
                    tool_use_id,
                    name,
                    input,
                    facet,
                    ..
                } = e
                {
                    out.push((tool_use_id, name, input, facet));
                }
            }
        }
        out
    }

    /// Same tuple, reconstructed by the session-file parser from an on-disk
    /// fixture.
    fn session_tools(fixture: &str) -> Vec<(String, String, Value, ToolFacet)> {
        let home = tempfile::TempDir::new().unwrap();
        let cwd = tempfile::TempDir::new().unwrap();
        let session_id = Uuid::now_v7();
        let canonical = cwd.path().canonicalize().unwrap();
        let path = crate::claude_session_file_path(home.path(), &canonical, &session_id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::copy(fixture_path(fixture), &path).unwrap();
        let loaded =
            load_claude_transcript(home.path(), cwd.path(), session_id, AgentId::now_v7()).unwrap();
        let mut out = Vec::new();
        for turn in loaded.turns {
            if let Turn::Agent { items, .. } = turn {
                for item in items {
                    if let TurnItem::Tool {
                        tool_use_id,
                        name,
                        input,
                        facet,
                        ..
                    } = item
                    {
                        out.push((tool_use_id, name, input, facet));
                    }
                }
            }
        }
        out
    }

    fn stream_tool_facets() -> Vec<(String, String, ToolFacet)> {
        stream_tools("tool-vocabulary.jsonl")
            .into_iter()
            .map(|(id, name, _, facet)| (id, name, facet))
            .collect()
    }

    fn session_tool_facets() -> Vec<(String, String, ToolFacet)> {
        session_tools("tool-vocabulary.session.jsonl")
            .into_iter()
            .map(|(id, name, _, facet)| (id, name, facet))
            .collect()
    }

    #[test]
    fn recorded_stream_vocabulary_maps_to_expected_facets() {
        let facets = stream_tool_facets();
        let kind_of = |name: &str| -> Vec<&ToolFacet> {
            facets
                .iter()
                .filter(|(_, n, _)| n == name)
                .map(|(_, _, f)| f)
                .collect()
        };
        assert!(matches!(kind_of("Bash")[0], ToolFacet::Shell { .. }));
        assert!(matches!(kind_of("Read")[0], ToolFacet::Read { .. }));
        assert!(matches!(kind_of("Write")[0], ToolFacet::Write { .. }));
        assert!(matches!(kind_of("Grep")[0], ToolFacet::Search { .. }));
        assert!(matches!(kind_of("TaskCreate")[0], ToolFacet::Todo { .. }));
        assert!(matches!(kind_of("TaskUpdate")[0], ToolFacet::Todo { .. }));
        let ToolFacet::Edit { files } = kind_of("Edit")[0] else {
            panic!("Edit must map to Edit facet");
        };
        assert_eq!(files[0].edits[0].old, "foo");
        assert_eq!(files[0].edits[0].new, "bar");
    }

    /// The two-call-site divergence guard: for every tool call present on
    /// both the live stream and the session file (joined by `tool_use_id`,
    /// as the frontend joins them), the two parsers must produce the same
    /// facet.
    #[test]
    fn stream_and_session_file_facets_agree_per_tool_use_id() {
        let stream = stream_tool_facets();
        let session = session_tool_facets();
        let mut compared = 0;
        for (id, name, stream_facet) in &stream {
            if let Some((_, _, session_facet)) = session.iter().find(|(sid, _, _)| sid == id) {
                assert_eq!(
                    stream_facet, session_facet,
                    "facet divergence for {name} ({id})"
                );
                compared += 1;
            }
        }
        assert!(
            compared >= 7,
            "expected the fixtures to share at least 7 tool calls, compared {compared}"
        );
    }

    #[test]
    fn mcp_mutation_fixture_agrees_live_and_after_reopen() {
        let stream = stream_tools("mcp-content-mutation.jsonl");
        let session = session_tools("mcp-content-mutation.session.jsonl");

        assert_eq!(stream.len(), 1);
        assert_eq!(session.len(), 1);
        assert_eq!(stream, session);

        let (_, name, input, facet) = &stream[0];
        assert_eq!(name, "mcp__notes_alias__edit_content");
        assert_eq!(
            input,
            &json!({
                "id": "note-example",
                "type": "note",
                "old_str": "before text",
                "new_str": "after text"
            })
        );
        assert!(matches!(
            facet,
            ToolFacet::Mcp {
                mutation: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn malformed_required_fields_degrade_to_other() {
        assert_eq!(
            classify_claude_tool_facet("Bash", &json!({})),
            ToolFacet::Other
        );
        assert_eq!(
            classify_claude_tool_facet("Edit", &json!({"file_path": "/tmp/a"})),
            ToolFacet::Other
        );
        assert_eq!(
            classify_claude_tool_facet("TodoWrite", &json!({"todos": "not-an-array"})),
            ToolFacet::Other
        );
        assert_eq!(
            classify_claude_tool_facet("Bash", &Value::Null),
            ToolFacet::Other
        );
    }
}
