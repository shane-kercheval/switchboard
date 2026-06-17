//! Forwarding helpers: turning an agent's completed output into forwardable
//! text, and composing one or more agents' outputs into the canonical
//! `=== START / END forwarded from <agent> ===` message body.
//!
//! Both the manual compose-bar forward and the workflow runtime resolve a
//! source's text from the dispatcher's live capture for a turn that was in
//! flight, and fall back to [`latest_completed_agent_text`] (a disk read) only
//! for a source that was already idle — its file is settled, so the same
//! `Text`-kind filter applied live and on disk yields the same string. Both then
//! compose via [`compose_forwarded_message`], so a manual forward and a workflow
//! `forward_from` produce byte-identical bodies — the system-design §7
//! one-mechanism principle. (`Thinking` reasoning and tool output are excluded
//! everywhere.)

use crate::events::ContentKind;
use crate::transcript::{Turn, TurnItem, TurnStatus};

/// The text-only output of the most-recent **completed** agent turn in `turns`:
/// its `Text`-kind items concatenated in arrival order, excluding `Thinking`
/// reasoning and tool output. `None` when no completed agent turn exists; the
/// returned string is itself empty when the completed turn produced only
/// thinking / tool items. Callers treat both `None` and an empty / whitespace
/// string as "no forwardable output" — the empty-source case.
///
/// Mirrors the dispatcher's live `captured_text` filter (which accumulates the
/// same `Text`-kind `ContentChunk`s with no separators) so the disk-read manual
/// path and the live-captured workflow path forward the same text for the same
/// turn.
#[must_use]
pub fn latest_completed_agent_text(turns: &[Turn]) -> Option<String> {
    turns.iter().rev().find_map(|turn| match turn {
        Turn::Agent {
            status: TurnStatus::Complete,
            items,
            ..
        } => Some(concat_text_items(items)),
        _ => None,
    })
}

/// Concatenate a turn's `Text`-kind items in order, dropping `Thinking` and
/// tool entries. No separator between items — consecutive on-disk text blocks
/// reconstruct the same string the live stream accumulated chunk-by-chunk.
fn concat_text_items(items: &[TurnItem]) -> String {
    let mut out = String::new();
    for item in items {
        if let TurnItem::Text {
            kind: ContentKind::Text,
            text,
        } = item
        {
            out.push_str(text);
        }
    }
    out
}

/// One forwarded source: the agent's display name and its resolved output text.
/// Borrowed for the lifetime of the compose call — the caller owns both.
#[derive(Debug, Clone, Copy)]
pub struct ForwardedBlock<'a> {
    pub agent_name: &'a str,
    pub text: &'a str,
}

/// Compose the canonical forwarded-message body
/// (`docs/workflow-spec.md` §`send` "Canonical composition with `forward_from`"):
/// the leading `body` (the user's typed text, or a rendered prompt/text, if any)
/// first, then each source's output in its own
/// `=== START forwarded from <agent> === / === END forwarded from <agent> ===`
/// block, in the given order, separated by blank lines.
///
/// - `body` empty ⇒ the composition is the blocks alone (no leading content,
///   no leading blank line) — the "only `forward_from` is set" case.
/// - `blocks` empty ⇒ just `body`. A caller that would compose zero blocks
///   should instead apply its empty-source policy *before* calling (the manual
///   path fails an all-empty forward; it never dispatches a bare body it framed
///   as a forward).
///
/// This is **wire content** the receiving agent reads — plain text by design,
/// identical between the manual forward and a workflow `forward_from`.
#[must_use]
pub fn compose_forwarded_message(body: &str, blocks: &[ForwardedBlock<'_>]) -> String {
    let mut sections: Vec<String> = Vec::new();
    if !body.is_empty() {
        sections.push(body.to_owned());
    }
    for block in blocks {
        sections.push(format!(
            "=== START forwarded from {name} ===\n{text}\n=== END forwarded from {name} ===",
            name = block.agent_name,
            text = block.text,
        ));
    }
    sections.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::ToolKind;
    use chrono::Utc;
    use uuid::Uuid;

    fn text_item(text: &str) -> TurnItem {
        TurnItem::Text {
            kind: ContentKind::Text,
            text: text.to_owned(),
        }
    }

    fn thinking_item(text: &str) -> TurnItem {
        TurnItem::Text {
            kind: ContentKind::Thinking,
            text: text.to_owned(),
        }
    }

    fn tool_item() -> TurnItem {
        TurnItem::Tool {
            tool_use_id: "t1".to_owned(),
            kind: ToolKind::Builtin,
            name: "Bash".to_owned(),
            input: serde_json::json!({"cmd": "ls"}),
            output: Some("file output that must not be forwarded".to_owned()),
            is_error: Some(false),
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
        }
    }

    fn agent_turn(status: TurnStatus, items: Vec<TurnItem>) -> Turn {
        Turn::Agent {
            turn_id: Uuid::now_v7(),
            agent_id: Uuid::now_v7(),
            started_at: Utc::now(),
            ended_at: Some(Utc::now()),
            status,
            items,
            usage: None,
            model: None,
            effort: None,
            spend: None,
            hydration_key: None,
            stable_message_id: None,
        }
    }

    fn user_turn(text: &str) -> Turn {
        Turn::User {
            turn_id: Uuid::now_v7(),
            agent_id: Uuid::now_v7(),
            started_at: Utc::now(),
            text: text.to_owned(),
        }
    }

    #[test]
    fn latest_text_is_text_only_turn_concatenated() {
        let turns = vec![agent_turn(
            TurnStatus::Complete,
            vec![text_item("hello "), text_item("world")],
        )];
        assert_eq!(
            latest_completed_agent_text(&turns).as_deref(),
            Some("hello world")
        );
    }

    #[test]
    fn latest_text_excludes_thinking_and_tool_output() {
        // A turn with interleaved thinking + tool calls: only `Text`-kind items
        // survive, concatenated across the intervening tool call.
        let turns = vec![agent_turn(
            TurnStatus::Complete,
            vec![
                thinking_item("secret reasoning"),
                text_item("visible before tool. "),
                tool_item(),
                text_item("visible after tool."),
            ],
        )];
        assert_eq!(
            latest_completed_agent_text(&turns).as_deref(),
            Some("visible before tool. visible after tool.")
        );
    }

    #[test]
    fn latest_text_picks_most_recent_completed_agent_turn() {
        let turns = vec![
            agent_turn(TurnStatus::Complete, vec![text_item("first")]),
            user_turn("a follow-up"),
            agent_turn(TurnStatus::Complete, vec![text_item("second")]),
        ];
        assert_eq!(
            latest_completed_agent_text(&turns).as_deref(),
            Some("second")
        );
    }

    #[test]
    fn latest_text_skips_non_completed_turns() {
        // A streaming/failed turn after the last completed one is not forwarded —
        // the most-recent *completed* turn wins.
        let turns = vec![
            agent_turn(TurnStatus::Complete, vec![text_item("done output")]),
            agent_turn(TurnStatus::Streaming, vec![text_item("in flight")]),
            agent_turn(TurnStatus::Failed, vec![text_item("partial")]),
        ];
        assert_eq!(
            latest_completed_agent_text(&turns).as_deref(),
            Some("done output")
        );
    }

    #[test]
    fn latest_text_none_when_no_completed_agent_turn() {
        let turns = vec![
            user_turn("just asked"),
            agent_turn(TurnStatus::Streaming, vec![text_item("working")]),
        ];
        assert_eq!(latest_completed_agent_text(&turns), None);
    }

    #[test]
    fn latest_text_empty_string_when_completed_turn_has_no_text() {
        // Completed, but produced only thinking / tool items: `Some("")` so the
        // caller's empty-source policy (not "no completed turn") applies.
        let turns = vec![agent_turn(
            TurnStatus::Complete,
            vec![thinking_item("only reasoning"), tool_item()],
        )];
        assert_eq!(latest_completed_agent_text(&turns).as_deref(), Some(""));
    }

    #[test]
    fn compose_single_block_with_body() {
        let out = compose_forwarded_message(
            "Please aggregate:",
            &[ForwardedBlock {
                agent_name: "reviewer-1",
                text: "LGTM with nits",
            }],
        );
        assert_eq!(
            out,
            "Please aggregate:\n\n\
             === START forwarded from reviewer-1 ===\n\
             LGTM with nits\n\
             === END forwarded from reviewer-1 ==="
        );
    }

    #[test]
    fn compose_multiple_blocks_in_declared_order() {
        let out = compose_forwarded_message(
            "",
            &[
                ForwardedBlock {
                    agent_name: "reviewer-1",
                    text: "first review",
                },
                ForwardedBlock {
                    agent_name: "reviewer-2",
                    text: "second review",
                },
            ],
        );
        assert_eq!(
            out,
            "=== START forwarded from reviewer-1 ===\n\
             first review\n\
             === END forwarded from reviewer-1 ===\n\n\
             === START forwarded from reviewer-2 ===\n\
             second review\n\
             === END forwarded from reviewer-2 ==="
        );
    }

    #[test]
    fn compose_empty_body_has_no_leading_blank_line() {
        let out = compose_forwarded_message(
            "",
            &[ForwardedBlock {
                agent_name: "agent-a",
                text: "output",
            }],
        );
        assert!(
            out.starts_with("=== START forwarded from agent-a ==="),
            "no leading content or blank line when body is empty: {out:?}"
        );
    }
}
