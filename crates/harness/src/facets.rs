//! Normalized tool-call facets.
//!
//! Each harness speaks its own tool vocabulary (Claude's `Edit`, Codex's
//! `file_change`/`apply_patch`, Antigravity's `replace_file_content`, …).
//! A [`ToolFacet`] describes *what kind of operation* a tool call is,
//! independent of that vocabulary, so the frontend can render one stable,
//! scannable verb per operation across all harnesses. Classification happens
//! once per harness, in Rust, where it is testable — never in a frontend
//! `switch (tool.name)`.
//!
//! The facet is **additive**: the raw `name` and `input` ride alongside it
//! unchanged on every event, and remain the provenance escape hatch. A tool
//! we have not mapped (including Claude's `Task` subagent dispatch,
//! deliberately unmapped) degrades to [`ToolFacet::Other`] and renders
//! through the generic path.
//!
//! Observed per-harness vocabularies and shapes: `docs/harness-behavior.md`
//! §3.6.

use serde::{Deserialize, Serialize};

/// Byte cap applied to each content-bearing facet string (`Write::content`,
/// [`EditPair`] sides). Prevents the facet from *duplicating* a large payload
/// on the wire — it does **not** bound the event, because the raw `input`
/// rides alongside uncapped; bounding what the renderer does with that is the
/// renderer's job.
pub const FACET_CONTENT_CAP: usize = 256 * 1024;

/// Maximum number of Unicode scalar values carried in an MCP mutation's
/// collapsed-row target. The complete identifying input remains available in
/// the raw tool input; bounding this duplicate keeps the always-mounted row
/// detail small without splitting a UTF-8 sequence.
pub const MCP_MUTATION_TARGET_CAP: usize = 240;

/// What kind of operation a tool call is. Serde-tagged like the surrounding
/// wire types; `#[non_exhaustive]` so a new operation kind is an additive
/// variant, not a wire break — TS consumers must default unknown
/// `facet_kind`s to the `Other` rendering path.
///
/// Contract notes that every consumer can rely on:
///
/// - **Paths are absolute** wherever the harness supplies them (probe result:
///   every harness already emits absolute paths — §3.6). Adapters with a
///   known cwd resolve relative spellings lexically before constructing the
///   facet; there is exactly one path field per file, and a project-relative
///   *display* path is derived at render time, never carried on the wire.
/// - **No line numbers.** Neither Claude's `Edit` input nor Codex's patches
///   carry absolute file positions, so edit rendering is snippet-scoped: the
///   reader sees the change, not its location. Deliberate accepted
///   limitation.
/// - The facet is computed at tool *start*; failure is carried by
///   `ToolCompleted::is_error`, so `Shell` carries no exit code.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "facet_kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolFacet {
    /// One or more files changed in place. A *list* of files because Codex's
    /// `apply_patch` touches several in one call; a list of [`EditPair`]s per
    /// file because one call can make several changes to the same file.
    /// Claude's `Edit` is the degenerate case: one file, one pair.
    Edit {
        files: Vec<EditedFile>,
    },
    /// A whole file written. Distinct from `Edit` even though a write is
    /// arguably an edit with an empty "before": the harness gives us the new
    /// content but *not* the prior content. The UI intentionally infers
    /// creation because dedicated writes overwhelmingly create files,
    /// accepting that rare overwrites appear as all-added diffs.
    Write {
        path: String,
        content: String,
        truncated: bool,
    },
    Read {
        path: String,
    },
    Shell {
        command: String,
        cwd: Option<String>,
    },
    Search {
        pattern: String,
        path: Option<String>,
    },
    Todo {
        items: Vec<TodoItem>,
    },
    Mcp {
        server: String,
        tool: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mutation: Option<Box<McpMutation>>,
    },
    /// Graceful degradation for any unmapped tool. Renders via the generic
    /// raw-input path.
    Other,
}

/// One file touched by an [`ToolFacet::Edit`].
///
/// `edits` **empty means content-unavailable**, not "no change": Codex's live
/// `file_change` item announces paths and change-kinds with no content (the
/// content exists only in its session file), so a live Codex edit facet
/// carries the file list with empty pairs and is upgraded once the turn's
/// post-terminal session-file read supplies the patch (§3.6).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditedFile {
    /// Absolute, normalized — see the path contract on [`ToolFacet`].
    pub path: String,
    pub change: EditChange,
    pub edits: Vec<EditPair>,
    pub truncated: bool,
}

/// How an [`EditedFile`] changed. `#[non_exhaustive]`: an unrecognized
/// harness change-kind maps to `Modified` (the least-wrong reading — the
/// path was touched) rather than failing the event.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EditChange {
    Added,
    Modified,
    Deleted,
}

/// One before/after pair within a file. An added file is `old: ""`; a
/// deleted file is `new: ""`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditPair {
    pub old: String,
    pub new: String,
}

/// One checklist entry of a [`ToolFacet::Todo`]. `status` is the harness's
/// own vocabulary (`pending` / `in_progress` / `completed` / …), kept as an
/// opaque string so upstream additions pass through. `content` is best
/// effort: Claude's `TaskUpdate` carries only a task id and a status, so a
/// status-only update surfaces the id as its content.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TodoItem {
    pub content: String,
    pub status: String,
}

/// Input-derived description of a recognized MCP content mutation.
///
/// These remain nested under [`ToolFacet::Mcp`] rather than masquerading as
/// filesystem edits: remote records have no absolute-path contract and cannot
/// safely inherit editor or Git affordances. Recognition is deliberately
/// provider-neutral because harness server aliases are user-configurable and
/// the recorded call carries no stable provider identity; an exact tool name
/// and argument shape therefore receive the same semantic display under any
/// alias. The description reflects requested input only. Rendering must not
/// fetch remote state to imply an authoritative post-write snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "mutation_kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum McpMutation {
    TextEdit {
        target: String,
        target_truncated: bool,
        before: String,
        after: String,
        content_truncated: bool,
    },
    TextCreation {
        target: String,
        target_truncated: bool,
        content: String,
        content_truncated: bool,
    },
    RecordCreation {
        target: String,
        target_truncated: bool,
        fields: Vec<McpMutationField>,
        fields_truncated: bool,
    },
}

/// One ordered label/value pair in an MCP record-creation summary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpMutationField {
    pub label: String,
    pub value: String,
}

/// Build an MCP facet and enrich the five recognized content-mutation schemas.
/// Unknown or malformed inputs retain MCP provenance with no mutation rather
/// than degrading to [`ToolFacet::Other`].
// The shared classifier contract intentionally precedes its per-harness callers.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "shared classifier is consumed by harness adapters"
    )
)]
pub(crate) fn classify_mcp_tool_facet(
    server: &str,
    tool: &str,
    arguments: &serde_json::Value,
) -> ToolFacet {
    let mutation = if server.is_empty() || tool.is_empty() {
        None
    } else {
        match tool {
            "edit_content" => classify_content_edit(arguments),
            "edit_prompt_content" => classify_prompt_edit(arguments),
            "create_note" => classify_note_creation(arguments),
            "create_prompt" => classify_prompt_creation(arguments),
            "create_bookmark" => classify_bookmark_creation(arguments),
            _ => None,
        }
    }
    .map(Box::new);

    ToolFacet::Mcp {
        server: server.to_owned(),
        tool: tool.to_owned(),
        mutation,
    }
}

fn classify_content_edit(arguments: &serde_json::Value) -> Option<McpMutation> {
    let id = required_string(arguments, "id")?;
    let item_type = required_string(arguments, "type")?;
    if !matches!(item_type, "note" | "bookmark") {
        return None;
    }
    let before = required_string(arguments, "old_str")?;
    let after = required_string(arguments, "new_str")?;
    Some(text_edit_mutation(item_type, id, before, after))
}

fn classify_prompt_edit(arguments: &serde_json::Value) -> Option<McpMutation> {
    let name = required_string(arguments, "name")?;
    let before = required_string(arguments, "old_str")?;
    let after = required_string(arguments, "new_str")?;
    Some(text_edit_mutation("prompt", name, before, after))
}

fn classify_note_creation(arguments: &serde_json::Value) -> Option<McpMutation> {
    let title = required_string(arguments, "title")?;
    let content = match arguments.get("content") {
        None | Some(serde_json::Value::Null) => "",
        Some(serde_json::Value::String(content)) => content,
        Some(_) => return None,
    };
    Some(text_creation_mutation("note", title, content))
}

fn classify_prompt_creation(arguments: &serde_json::Value) -> Option<McpMutation> {
    let name = required_string(arguments, "name")?;
    let content = required_string(arguments, "content")?;
    Some(text_creation_mutation("prompt", name, content))
}

fn classify_bookmark_creation(arguments: &serde_json::Value) -> Option<McpMutation> {
    let url = required_string(arguments, "url")?;
    let title = optional_string(arguments, "title");
    let description = optional_string(arguments, "description");
    let tags = optional_tags(arguments, "tags");
    let target_value = title.filter(|title| !title.is_empty()).unwrap_or(url);
    let (target, target_truncated) = bounded_target("bookmark", target_value);

    let mut fields = Vec::with_capacity(4);
    let mut fields_truncated = false;
    if let Some(title) = title {
        push_bounded_field(&mut fields, &mut fields_truncated, "Title", title);
    }
    push_bounded_field(&mut fields, &mut fields_truncated, "URL", url);
    if let Some(description) = description {
        push_bounded_field(
            &mut fields,
            &mut fields_truncated,
            "Description",
            description,
        );
    }
    if let Some((tags, truncated)) = tags {
        fields.push(McpMutationField {
            label: "Tags".to_owned(),
            value: tags,
        });
        fields_truncated |= truncated;
    }

    Some(McpMutation::RecordCreation {
        target,
        target_truncated,
        fields,
        fields_truncated,
    })
}

fn text_edit_mutation(kind: &str, target_value: &str, before: &str, after: &str) -> McpMutation {
    let (target, target_truncated) = bounded_target(kind, target_value);
    let (before, before_truncated) = cap_content(before);
    let (after, after_truncated) = cap_content(after);
    McpMutation::TextEdit {
        target,
        target_truncated,
        before,
        after,
        content_truncated: before_truncated || after_truncated,
    }
}

fn text_creation_mutation(kind: &str, target_value: &str, content: &str) -> McpMutation {
    let (target, target_truncated) = bounded_target(kind, target_value);
    let (content, content_truncated) = cap_content(content);
    McpMutation::TextCreation {
        target,
        target_truncated,
        content,
        content_truncated,
    }
}

fn required_string<'a>(arguments: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    arguments.get(key).and_then(serde_json::Value::as_str)
}

fn optional_string<'a>(arguments: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    match arguments.get(key) {
        Some(serde_json::Value::String(value)) => Some(value),
        _ => None,
    }
}

fn optional_tags(arguments: &serde_json::Value, key: &str) -> Option<(String, bool)> {
    let values = arguments.get(key)?.as_array()?;
    if !values.iter().all(serde_json::Value::is_string) {
        return None;
    }

    let mut joined = String::new();
    let mut truncated = false;
    for (index, value) in values
        .iter()
        .filter_map(serde_json::Value::as_str)
        .enumerate()
    {
        if index > 0 && !append_bounded(&mut joined, ", ") {
            truncated = true;
            break;
        }
        if !append_bounded(&mut joined, value) {
            truncated = true;
            break;
        }
    }
    Some((joined, truncated))
}

fn push_bounded_field(
    fields: &mut Vec<McpMutationField>,
    fields_truncated: &mut bool,
    label: &str,
    value: &str,
) {
    let (value, truncated) = cap_content(value);
    fields.push(McpMutationField {
        label: label.to_owned(),
        value,
    });
    *fields_truncated |= truncated;
}

fn bounded_target(kind: &str, value: &str) -> (String, bool) {
    let mut target = String::with_capacity(MCP_MUTATION_TARGET_CAP);
    target.push_str(kind);
    target.push_str(" · ");
    let remaining = MCP_MUTATION_TARGET_CAP.saturating_sub(target.chars().count());
    let mut chars = value.chars();
    target.extend(chars.by_ref().take(remaining));
    let truncated = chars.next().is_some();
    (target, truncated)
}

fn append_bounded(output: &mut String, value: &str) -> bool {
    let remaining = FACET_CONTENT_CAP.saturating_sub(output.len());
    if value.len() <= remaining {
        output.push_str(value);
        return true;
    }
    let mut end = remaining;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    output.push_str(&value[..end]);
    false
}

/// Cap `s` at [`FACET_CONTENT_CAP`] bytes (on a char boundary). Returns the
/// possibly-shortened string and whether truncation happened.
pub(crate) fn cap_content(s: &str) -> (String, bool) {
    if s.len() <= FACET_CONTENT_CAP {
        return (s.to_owned(), false);
    }
    let mut end = FACET_CONTENT_CAP;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_owned(), true)
}

/// Split a Claude/Gemini-convention MCP tool name (`mcp__<server>__<tool>`)
/// into `(server, tool)`. `None` when the name doesn't carry both parts —
/// callers fall back to [`ToolFacet::Other`] rather than guessing.
pub(crate) fn split_mcp_name(name: &str) -> Option<(String, String)> {
    let rest = name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server.to_owned(), tool.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn facet_serializes_with_facet_kind_tag() {
        let facet = ToolFacet::Shell {
            command: "ls".to_owned(),
            cwd: None,
        };
        let value = serde_json::to_value(&facet).unwrap();
        assert_eq!(value["facet_kind"], "shell");
        assert_eq!(value["command"], "ls");
        assert!(value["cwd"].is_null());
    }

    #[test]
    fn edit_facet_round_trips() {
        let facet = ToolFacet::Edit {
            files: vec![EditedFile {
                path: "/tmp/a.txt".to_owned(),
                change: EditChange::Modified,
                edits: vec![EditPair {
                    old: "foo".to_owned(),
                    new: "bar".to_owned(),
                }],
                truncated: false,
            }],
        };
        let value = serde_json::to_value(&facet).unwrap();
        assert_eq!(value["facet_kind"], "edit");
        assert_eq!(value["files"][0]["change"], "modified");
        let back: ToolFacet = serde_json::from_value(value).unwrap();
        assert_eq!(back, facet);
    }

    #[test]
    fn unknown_facet_kind_fails_deserialization_not_panics() {
        // The Rust side never receives foreign facets today (it only
        // produces them), but the tag must at least error cleanly.
        let result = serde_json::from_value::<ToolFacet>(json!({"facet_kind": "hologram"}));
        assert!(result.is_err());
    }

    #[test]
    fn cap_content_under_cap_is_unchanged() {
        let (s, truncated) = cap_content("hello");
        assert_eq!(s, "hello");
        assert!(!truncated);
    }

    #[test]
    fn cap_content_over_cap_truncates_on_char_boundary() {
        // Multi-byte char straddling the cap must not split.
        let body = "é".repeat(FACET_CONTENT_CAP); // 2 bytes each → over cap
        let (s, truncated) = cap_content(&body);
        assert!(truncated);
        assert!(s.len() <= FACET_CONTENT_CAP);
        assert!(s.chars().all(|c| c == 'é'));
    }

    #[test]
    fn split_mcp_name_extracts_server_and_tool() {
        assert_eq!(
            split_mcp_name("mcp__tiddly__search_items"),
            Some(("tiddly".to_owned(), "search_items".to_owned()))
        );
    }

    #[test]
    fn split_mcp_name_rejects_incomplete_names() {
        assert_eq!(split_mcp_name("mcp__loneserver"), None);
        assert_eq!(split_mcp_name("mcp____"), None);
        assert_eq!(split_mcp_name("Bash"), None);
    }

    fn mutation(tool: &str, arguments: &Value) -> McpMutation {
        let ToolFacet::Mcp {
            mutation: Some(mutation),
            ..
        } = classify_mcp_tool_facet("example_server", tool, arguments)
        else {
            panic!("expected an enriched MCP facet for {tool}");
        };
        *mutation
    }

    fn assert_basic_mcp(tool: &str, arguments: &Value) {
        assert_eq!(
            classify_mcp_tool_facet("example_server", tool, arguments),
            ToolFacet::Mcp {
                server: "example_server".to_owned(),
                tool: tool.to_owned(),
                mutation: None,
            }
        );
    }

    #[test]
    fn recognized_text_edits_have_exact_targets() {
        assert_eq!(
            mutation(
                "edit_content",
                &json!({
                    "id": "note-123",
                    "type": "note",
                    "old_str": "before",
                    "new_str": "after"
                }),
            ),
            McpMutation::TextEdit {
                target: "note · note-123".to_owned(),
                target_truncated: false,
                before: "before".to_owned(),
                after: "after".to_owned(),
                content_truncated: false,
            }
        );
        assert_eq!(
            mutation(
                "edit_content",
                &json!({
                    "id": "bookmark-456",
                    "type": "bookmark",
                    "old_str": "old",
                    "new_str": "new"
                }),
            ),
            McpMutation::TextEdit {
                target: "bookmark · bookmark-456".to_owned(),
                target_truncated: false,
                before: "old".to_owned(),
                after: "new".to_owned(),
                content_truncated: false,
            }
        );
        assert_eq!(
            mutation(
                "edit_prompt_content",
                &json!({"name": "review", "old_str": "one", "new_str": "two"}),
            ),
            McpMutation::TextEdit {
                target: "prompt · review".to_owned(),
                target_truncated: false,
                before: "one".to_owned(),
                after: "two".to_owned(),
                content_truncated: false,
            }
        );
    }

    #[test]
    fn recognized_text_creations_have_exact_targets_and_content() {
        assert_eq!(
            mutation(
                "create_note",
                &json!({"title": "Release notes", "content": "Shipped"}),
            ),
            McpMutation::TextCreation {
                target: "note · Release notes".to_owned(),
                target_truncated: false,
                content: "Shipped".to_owned(),
                content_truncated: false,
            }
        );
        assert_eq!(
            mutation(
                "create_prompt",
                &json!({"name": "summarize", "content": "Summarize {{ input }}"}),
            ),
            McpMutation::TextCreation {
                target: "prompt · summarize".to_owned(),
                target_truncated: false,
                content: "Summarize {{ input }}".to_owned(),
                content_truncated: false,
            }
        );
    }

    #[test]
    fn bookmark_creation_has_stable_ordered_fields() {
        assert_eq!(
            mutation(
                "create_bookmark",
                &json!({
                    "url": "https://example.com",
                    "title": "Example",
                    "description": "Reference",
                    "tags": ["docs", "saved"]
                }),
            ),
            McpMutation::RecordCreation {
                target: "bookmark · Example".to_owned(),
                target_truncated: false,
                fields: vec![
                    McpMutationField {
                        label: "Title".to_owned(),
                        value: "Example".to_owned(),
                    },
                    McpMutationField {
                        label: "URL".to_owned(),
                        value: "https://example.com".to_owned(),
                    },
                    McpMutationField {
                        label: "Description".to_owned(),
                        value: "Reference".to_owned(),
                    },
                    McpMutationField {
                        label: "Tags".to_owned(),
                        value: "docs, saved".to_owned(),
                    },
                ],
                fields_truncated: false,
            }
        );
    }

    #[test]
    fn classification_is_alias_independent_but_preserves_aliases() {
        let arguments = json!({"name": "review", "old_str": "a", "new_str": "b"});
        let first = classify_mcp_tool_facet("personal_prompts", "edit_prompt_content", &arguments);
        let second = classify_mcp_tool_facet("neutral_server", "edit_prompt_content", &arguments);
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
            panic!("classifier always returns MCP facets");
        };
        assert_eq!(first_server, "personal_prompts");
        assert_eq!(second_server, "neutral_server");
        assert_eq!(first_tool, second_tool);
        assert_eq!(first_mutation, second_mutation);
    }

    #[test]
    fn target_cap_counts_unicode_scalars_without_affecting_content_truncation() {
        let name = "🦀".repeat(MCP_MUTATION_TARGET_CAP + 10);
        let McpMutation::TextEdit {
            target,
            target_truncated,
            content_truncated,
            ..
        } = mutation(
            "edit_prompt_content",
            &json!({"name": name, "old_str": "a", "new_str": "b"}),
        )
        else {
            panic!("expected text edit");
        };
        assert_eq!(target.chars().count(), MCP_MUTATION_TARGET_CAP);
        assert!(target.is_char_boundary(target.len()));
        assert!(target_truncated);
        assert!(!content_truncated);

        let exact_name = "界".repeat(MCP_MUTATION_TARGET_CAP - "prompt · ".chars().count());
        let McpMutation::TextCreation {
            target,
            target_truncated,
            content_truncated,
            ..
        } = mutation(
            "create_prompt",
            &json!({"name": exact_name, "content": "body"}),
        )
        else {
            panic!("expected text creation");
        };
        assert_eq!(target.chars().count(), MCP_MUTATION_TARGET_CAP);
        assert!(!target_truncated);
        assert!(!content_truncated);
    }

    #[test]
    fn text_edit_caps_both_sides_on_utf8_boundaries() {
        let before = "é".repeat(FACET_CONTENT_CAP / 2 + 1);
        let after = "🦀".repeat(FACET_CONTENT_CAP / 4 + 1);
        let McpMutation::TextEdit {
            before,
            after,
            content_truncated,
            target_truncated,
            ..
        } = mutation(
            "edit_prompt_content",
            &json!({"name": "large", "old_str": before, "new_str": after}),
        )
        else {
            panic!("expected text edit");
        };
        assert_eq!(before.len(), FACET_CONTENT_CAP);
        assert_eq!(after.len(), FACET_CONTENT_CAP);
        assert!(before.is_char_boundary(before.len()));
        assert!(after.is_char_boundary(after.len()));
        assert!(content_truncated);
        assert!(!target_truncated);
    }

    #[test]
    fn text_creation_and_record_fields_are_bounded_independently() {
        let large = "é".repeat(FACET_CONTENT_CAP / 2 + 1);
        let McpMutation::TextCreation {
            content,
            content_truncated,
            target_truncated,
            ..
        } = mutation("create_note", &json!({"title": "small", "content": large}))
        else {
            panic!("expected text creation");
        };
        assert_eq!(content.len(), FACET_CONTENT_CAP);
        assert!(content_truncated);
        assert!(!target_truncated);

        let large_description = "🦀".repeat(FACET_CONTENT_CAP / 4 + 1);
        let McpMutation::RecordCreation {
            fields,
            fields_truncated,
            target_truncated,
            ..
        } = mutation(
            "create_bookmark",
            &json!({"url": "https://example.com", "description": large_description}),
        )
        else {
            panic!("expected record creation");
        };
        assert_eq!(fields[1].label, "Description");
        assert_eq!(fields[1].value.len(), FACET_CONTENT_CAP);
        assert!(fields_truncated);
        assert!(!target_truncated);
    }

    #[test]
    fn joined_bookmark_tags_are_bounded_without_an_unbounded_intermediate() {
        let large_tag = "x".repeat(FACET_CONTENT_CAP + 1);
        let McpMutation::RecordCreation {
            fields,
            fields_truncated,
            ..
        } = mutation(
            "create_bookmark",
            &json!({"url": "https://example.com", "tags": [large_tag, "later"]}),
        )
        else {
            panic!("expected record creation");
        };
        let tags = fields.iter().find(|field| field.label == "Tags").unwrap();
        assert_eq!(tags.value.len(), FACET_CONTENT_CAP);
        assert!(fields_truncated);
    }

    #[test]
    fn malformed_required_fields_keep_a_basic_mcp_facet() {
        for arguments in [
            json!({"type": "note", "old_str": "a", "new_str": "b"}),
            json!({"id": null, "type": "note", "old_str": "a", "new_str": "b"}),
            json!({"id": 42, "type": "note", "old_str": "a", "new_str": "b"}),
            json!({"id": "x", "type": "task", "old_str": "a", "new_str": "b"}),
            json!({"id": "x", "type": "note", "old_str": [], "new_str": "b"}),
        ] {
            assert_basic_mcp("edit_content", &arguments);
        }

        for arguments in [
            json!({"old_str": "a", "new_str": "b"}),
            json!({"name": null, "old_str": "a", "new_str": "b"}),
            json!({"name": 42, "old_str": "a", "new_str": "b"}),
            json!({"name": "x", "new_str": "b"}),
            json!({"name": "x", "old_str": null, "new_str": "b"}),
            json!({"name": "x", "old_str": 42, "new_str": "b"}),
            json!({"name": "x", "old_str": "a"}),
            json!({"name": "x", "old_str": "a", "new_str": null}),
            json!({"name": "x", "old_str": "a", "new_str": 42}),
        ] {
            assert_basic_mcp("edit_prompt_content", &arguments);
        }

        for arguments in [
            json!({"content": "body"}),
            json!({"title": null, "content": "body"}),
            json!({"title": 42, "content": "body"}),
            json!({"title": "x", "content": 42}),
        ] {
            assert_basic_mcp("create_note", &arguments);
        }

        for arguments in [
            json!({"content": "body"}),
            json!({"name": null, "content": "body"}),
            json!({"name": 42, "content": "body"}),
            json!({"name": "x"}),
            json!({"name": "x", "content": null}),
            json!({"name": "x", "content": 42}),
        ] {
            assert_basic_mcp("create_prompt", &arguments);
        }

        for arguments in [json!({}), json!({"url": null}), json!({"url": 42})] {
            assert_basic_mcp("create_bookmark", &arguments);
        }
    }

    #[test]
    fn bookmark_optional_fields_degrade_independently() {
        let McpMutation::RecordCreation {
            target,
            fields,
            fields_truncated,
            ..
        } = mutation(
            "create_bookmark",
            &json!({
                "url": "https://example.com",
                "title": null,
                "description": 12,
                "tags": ["valid", 3]
            }),
        )
        else {
            panic!("expected record creation");
        };
        assert_eq!(target, "bookmark · https://example.com");
        assert_eq!(
            fields,
            vec![McpMutationField {
                label: "URL".to_owned(),
                value: "https://example.com".to_owned(),
            }]
        );
        assert!(!fields_truncated);

        let McpMutation::RecordCreation { target, fields, .. } = mutation(
            "create_bookmark",
            &json!({"url": "https://example.com", "title": ""}),
        ) else {
            panic!("expected record creation");
        };
        assert_eq!(target, "bookmark · https://example.com");
        assert_eq!(fields[0].label, "Title");
        assert_eq!(fields[0].value, "");
        assert_eq!(fields[1].label, "URL");
    }

    #[test]
    fn empty_text_creation_bodies_are_valid() {
        assert!(matches!(
            mutation("create_note", &json!({"title": "Empty"})),
            McpMutation::TextCreation {
                content,
                content_truncated: false,
                ..
            } if content.is_empty()
        ));
        assert!(matches!(
            mutation(
                "create_note",
                &json!({"title": "Null", "content": null}),
            ),
            McpMutation::TextCreation {
                content,
                content_truncated: false,
                ..
            } if content.is_empty()
        ));
        assert!(matches!(
            mutation(
                "create_prompt",
                &json!({"name": "empty", "content": ""}),
            ),
            McpMutation::TextCreation {
                content,
                content_truncated: false,
                ..
            } if content.is_empty()
        ));
    }

    #[test]
    fn excluded_and_unknown_tools_remain_basic_mcp_facets() {
        assert_basic_mcp("update_item", &json!({"id": "x", "content": "replacement"}));
        assert_basic_mcp(
            "update_prompt",
            &json!({"name": "x", "content": "replacement"}),
        );
        assert_basic_mcp("delete_item", &json!({"id": "x"}));
        assert_basic_mcp("search_items", &json!({"query": "x"}));
    }

    #[test]
    fn incomplete_mcp_identity_is_preserved_without_enrichment() {
        assert_eq!(
            classify_mcp_tool_facet(
                "",
                "create_note",
                &json!({"title": "Would otherwise match"}),
            ),
            ToolFacet::Mcp {
                server: String::new(),
                tool: "create_note".to_owned(),
                mutation: None,
            }
        );
        assert_eq!(
            classify_mcp_tool_facet("server", "", &json!({})),
            ToolFacet::Mcp {
                server: "server".to_owned(),
                tool: String::new(),
                mutation: None,
            }
        );
    }

    #[test]
    fn mcp_mutation_serialization_pins_wire_shape_and_round_trips() {
        let facet = classify_mcp_tool_facet(
            "server",
            "edit_prompt_content",
            &json!({"name": "review", "old_str": "a", "new_str": "b"}),
        );
        let value = serde_json::to_value(&facet).unwrap();
        assert_eq!(value["facet_kind"], "mcp");
        assert_eq!(value["server"], "server");
        assert_eq!(value["tool"], "edit_prompt_content");
        assert_eq!(value["mutation"]["mutation_kind"], "text_edit");
        assert_eq!(value["mutation"]["target"], "prompt · review");
        assert_eq!(value["mutation"]["before"], "a");
        assert_eq!(value["mutation"]["after"], "b");
        let round_trip: ToolFacet = serde_json::from_value(value).unwrap();
        assert_eq!(round_trip, facet);

        let basic = classify_mcp_tool_facet("server", "search_items", &json!({"query": "x"}));
        let basic_value = serde_json::to_value(&basic).unwrap();
        assert!(!basic_value.as_object().unwrap().contains_key("mutation"));
        let legacy: ToolFacet = serde_json::from_value(json!({
            "facet_kind": "mcp",
            "server": "server",
            "tool": "search_items"
        }))
        .unwrap();
        assert_eq!(legacy, basic);
    }
}
