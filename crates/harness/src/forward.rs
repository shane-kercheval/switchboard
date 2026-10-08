//! Forwarding helpers: turning an agent's completed output into forwardable
//! text, and composing one or more agents' outputs into the canonical
//! `=== START / END forwarded from <agent> ===` message body.
//!
//! A turn's forwardable text is its **answer content**: every non-empty
//! `Text`-kind block, plus the markdown of every code review the agent
//! delivered through a successful findings tool call ([`ToolFacet::Findings`]),
//! in turn order with `\n\n` between pieces. `Thinking` reasoning, ordinary tool
//! output, and a findings call that failed or never got a result are excluded.
//! A review counts because the tool's caller is told the host renders it, so the
//! model often writes nothing else.
//!
//! Two paths compute that text and must agree byte-for-byte:
//!
//! - [`TextCapture`] builds it from events while the turn streams. The
//!   dispatcher uses it for every turn, and hands the result to a forward or
//!   workflow that was waiting on a turn in flight.
//! - [`latest_completed_agent_text`] rebuilds it from the session file, for a
//!   source that was already idle.
//!
//! Both then compose via [`compose_forwarded_message`], so a manual forward and
//! a workflow `forward_from` produce byte-identical bodies — the system-design
//! §7 one-mechanism principle. The `\n\n` before a text block comes from the
//! live parser, which bakes it into the block's first chunk (`parser.rs`'s
//! `pending_separator`); the parser counts a successful review as earlier
//! answer content for that purpose. The `\n\n` before a review comes from
//! [`TextCapture`] itself.

use crate::events::{AdapterEvent, ContentKind};
use crate::facets::ToolFacet;
use crate::transcript::{Turn, TurnItem, TurnStatus};

/// The answer content of the most-recent **completed** agent turn in `turns`
/// (see the module docs for what counts), joined with `\n\n` in arrival
/// order. `None` when no completed agent turn exists; the returned string is
/// itself empty when the completed turn produced only thinking / tool items.
/// Callers treat both `None` and an empty / whitespace string as "no
/// forwardable output" — the empty-source case.
///
/// The disk half of the byte-for-byte contract with [`TextCapture`].
#[must_use]
pub fn latest_completed_agent_text(turns: &[Turn]) -> Option<String> {
    turns.iter().rev().find_map(|turn| match turn {
        Turn::Agent {
            status: TurnStatus::Complete,
            items,
            ..
        } => Some(concat_answer_items(items)),
        _ => None,
    })
}

/// Join a turn's answer pieces with `\n\n`, in order: non-empty `Text`-kind
/// items, and the markdown of each findings call that completed without error
/// (`is_error == Some(false)`; `None` means the call never got a result).
/// Separator only *between* pieces (no leading separator), and empty items
/// neither emit text nor produce a doubled separator — both matching the live
/// parser exactly (an empty block does not consume its `pending_separator`, so
/// live yields one `\n\n` there, not two).
fn concat_answer_items(items: &[TurnItem]) -> String {
    let mut out = String::new();
    for item in items {
        let piece = match item {
            TurnItem::Text {
                kind: ContentKind::Text,
                text,
            } => text.as_str(),
            TurnItem::Tool {
                facet: ToolFacet::Findings(report),
                is_error: Some(false),
                ..
            } => report.text.as_str(),
            _ => continue,
        };
        push_piece(&mut out, piece);
    }
    out
}

fn push_piece(out: &mut String, piece: &str) {
    if piece.is_empty() {
        return;
    }
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(piece);
}

/// The live half of the forwarded-text contract: accumulates a turn's answer
/// content from its adapter events, matching [`latest_completed_agent_text`]
/// on the same turn byte-for-byte.
///
/// Text chunks append verbatim (the parser has already baked in their
/// separators). A findings call takes its place in the text when it *starts*,
/// matching the disk read, which orders items by where each call began; it is
/// included only once its `ToolCompleted` reports success. So two reviews keep
/// their order even when the CLI (which runs them concurrently) completes them
/// in reverse, and a call the CLI rejected (which the model then retries) or
/// one cut off by a cancelled or failed turn contributes nothing.
#[derive(Debug, Default)]
pub struct TextCapture {
    pieces: Vec<CapturedPiece>,
}

#[derive(Debug)]
enum CapturedPiece {
    Text(String),
    Review {
        tool_use_id: String,
        text: String,
        status: ReviewStatus,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewStatus {
    Pending,
    Accepted,
    Dropped,
}

impl TextCapture {
    /// Feed one adapter event of the turn. Events that carry no answer content
    /// are ignored.
    pub fn observe(&mut self, event: &AdapterEvent) {
        match event {
            AdapterEvent::ContentChunk {
                kind: ContentKind::Text,
                text,
                ..
            } => {
                if let Some(CapturedPiece::Text(last)) = self.pieces.last_mut() {
                    last.push_str(text);
                } else {
                    self.pieces.push(CapturedPiece::Text(text.clone()));
                }
            }
            AdapterEvent::ToolStarted {
                tool_use_id,
                facet: ToolFacet::Findings(report),
                ..
            } => self.pieces.push(CapturedPiece::Review {
                tool_use_id: tool_use_id.clone(),
                text: report.text.clone(),
                status: ReviewStatus::Pending,
            }),
            AdapterEvent::ToolCompleted {
                tool_use_id: completed_id,
                is_error,
                ..
            } => {
                for piece in &mut self.pieces {
                    if let CapturedPiece::Review {
                        tool_use_id,
                        status: status @ ReviewStatus::Pending,
                        ..
                    } = piece
                        && tool_use_id == completed_id
                    {
                        *status = if *is_error {
                            ReviewStatus::Dropped
                        } else {
                            ReviewStatus::Accepted
                        };
                    }
                }
            }
            _ => {}
        }
    }

    /// The turn's answer content: text verbatim, plus each accepted review
    /// joined with the same rule as the disk read. A review still pending here
    /// never got a result, so it is left out.
    #[must_use]
    pub fn finish(self) -> String {
        let mut out = String::new();
        for piece in self.pieces {
            match piece {
                CapturedPiece::Text(text) => out.push_str(&text),
                CapturedPiece::Review {
                    text,
                    status: ReviewStatus::Accepted,
                    ..
                } => push_piece(&mut out, &text),
                CapturedPiece::Review { .. } => {}
            }
        }
        out
    }
}

/// Whether resolved text counts as forwardable — the single emptiness rule
/// behind the any-empty forward policy. Every enforcement site (manual message
/// forward, prompt forward, workflow-field resolution, and the workflow
/// runtime's `forward_from`) routes through this predicate so "forwardable"
/// cannot drift between paths.
#[must_use]
pub fn is_forwardable_text(text: &str) -> bool {
    !text.trim().is_empty()
}

/// The user-facing reason for an any-empty invalidation, shared by every
/// enforcement site so the copy cannot drift either. Neutral by design: it
/// names the source and states that it has no forwardable text — never "had no
/// output" (asserting a fact about the agent we don't know) or "could not be
/// read" (asserting a cause we can't distinguish).
#[must_use]
pub fn empty_sources_reason(names: &[String]) -> String {
    let list = names.join(", ");
    let has = if names.len() == 1 { "has" } else { "have" };
    format!("{list} {has} no forwardable text available; nothing was sent")
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
        // SYNCHRONIZED WIRE SHAPE: the frontend bands this block by string-matching
        // `=== START forwarded from … ===` (`FORWARD_SENTINEL` in
        // `desktop/src/lib/state/heldForwards.svelte.ts`, `QUOTED_BLOCK` in
        // `desktop/src/lib/components/UnifiedTranscript.svelte`). Changing this string
        // breaks transcript styling unless both languages change together.
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
            facet: crate::facets::ToolFacet::Other,
            name: "Bash".to_owned(),
            input: serde_json::json!({"cmd": "ls"}),
            output: Some("file output that must not be forwarded".to_owned()),
            is_error: Some(false),
            warnings: Vec::new(),
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
            continuation_of: None,
            stable_message_id: None,
        }
    }

    fn user_turn(text: &str) -> Turn {
        Turn::User {
            turn_id: Uuid::now_v7(),
            agent_id: Uuid::now_v7(),
            started_at: Utc::now(),
            text: text.to_owned(),
            source: crate::transcript::UserPromptSource::Unknown,
        }
    }

    #[test]
    fn latest_text_joins_blocks_with_paragraph_separator() {
        // Each on-disk `Text` item is one content block; live bakes `\n\n`
        // between blocks, so the disk join must too. (Intentional behavior
        // change from the old glue-with-nothing join, which produced
        // "…running.The codebase…" run-togethers that live never emits.)
        let turns = vec![agent_turn(
            TurnStatus::Complete,
            vec![text_item("hello "), text_item("world")],
        )];
        assert_eq!(
            latest_completed_agent_text(&turns).as_deref(),
            Some("hello \n\nworld")
        );
    }

    #[test]
    fn latest_text_excludes_thinking_and_tool_output() {
        // A turn with interleaved thinking + tool calls: only `Text`-kind items
        // survive, and the block separator persists across the intervening
        // tool call — mirroring live's `pending_separator`, which a tool block
        // does not reset.
        let turns = vec![agent_turn(
            TurnStatus::Complete,
            vec![
                thinking_item("secret reasoning"),
                text_item("visible before tool."),
                tool_item(),
                text_item("visible after tool."),
            ],
        )];
        assert_eq!(
            latest_completed_agent_text(&turns).as_deref(),
            Some("visible before tool.\n\nvisible after tool.")
        );
    }

    #[test]
    fn latest_text_sentence_boundary_not_run_together() {
        // The user-visible defect the separator fixes: consecutive response
        // segments forwarded from disk must not glue into
        // "…running.The codebase…".
        let turns = vec![agent_turn(
            TurnStatus::Complete,
            vec![
                text_item("Both research agents are running."),
                text_item("The codebase inventory is complete."),
            ],
        )];
        assert_eq!(
            latest_completed_agent_text(&turns).as_deref(),
            Some("Both research agents are running.\n\nThe codebase inventory is complete."),
        );
    }

    #[test]
    fn latest_text_empty_block_does_not_double_the_separator() {
        // Parity with live's `empty_text_block_does_not_consume_pending_separator`:
        // an empty text block between two non-empty ones yields ONE `\n\n`,
        // not two — and no leading separator when the first block is empty.
        let turns = vec![agent_turn(
            TurnStatus::Complete,
            vec![
                text_item(""),
                text_item("first"),
                text_item(""),
                text_item("second"),
            ],
        )];
        assert_eq!(
            latest_completed_agent_text(&turns).as_deref(),
            Some("first\n\nsecond")
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

    fn findings_facet(text: &str) -> ToolFacet {
        ToolFacet::Findings(Box::new(crate::facets::FindingsReport {
            level: None,
            findings: Vec::new(),
            text: text.to_owned(),
        }))
    }

    fn findings_item(id: &str, text: &str, is_error: Option<bool>) -> TurnItem {
        TurnItem::Tool {
            tool_use_id: id.to_owned(),
            kind: ToolKind::Builtin,
            facet: findings_facet(text),
            name: "ReportFindings".to_owned(),
            input: serde_json::json!({}),
            output: is_error.map(|_| "1 finding reported.".to_owned()),
            is_error,
            warnings: Vec::new(),
            started_at: Utc::now(),
            completed_at: is_error.map(|_| Utc::now()),
        }
    }

    fn disk_text(items: Vec<TurnItem>) -> String {
        latest_completed_agent_text(&[agent_turn(TurnStatus::Complete, items)]).unwrap()
    }

    #[test]
    fn disk_text_places_a_review_between_text_blocks() {
        let text = disk_text(vec![
            text_item("Intro."),
            findings_item("f1", "REVIEW", Some(false)),
            text_item("Outro."),
        ]);
        assert_eq!(text, "Intro.\n\nREVIEW\n\nOutro.");
    }

    #[test]
    fn disk_text_of_a_review_only_turn_is_the_review() {
        assert_eq!(
            disk_text(vec![findings_item("f1", "REVIEW", Some(false))]),
            "REVIEW"
        );
        assert_eq!(
            disk_text(vec![
                findings_item("f1", "REVIEW", Some(false)),
                text_item("ack")
            ]),
            "REVIEW\n\nack"
        );
    }

    #[test]
    fn disk_text_excludes_failed_and_unanswered_reviews() {
        // A rejected call (the model then retries) and a call that never got a
        // result both contribute nothing, so the review appears once.
        let text = disk_text(vec![
            findings_item("f1", "REVIEW", Some(true)),
            findings_item("f2", "REVIEW", Some(false)),
            findings_item("f3", "UNANSWERED", None),
        ]);
        assert_eq!(text, "REVIEW");
    }

    #[test]
    fn disk_text_joins_two_reviews() {
        let text = disk_text(vec![
            findings_item("f1", "FIRST", Some(false)),
            findings_item("f2", "SECOND", Some(false)),
        ]);
        assert_eq!(text, "FIRST\n\nSECOND");
    }

    #[test]
    fn a_review_only_turn_is_forwardable() {
        let text = disk_text(vec![findings_item("f1", "REVIEW", Some(false))]);
        assert!(is_forwardable_text(&text));
    }

    fn chunk(text: &str) -> AdapterEvent {
        AdapterEvent::ContentChunk {
            turn_id: Uuid::now_v7(),
            kind: ContentKind::Text,
            text: text.to_owned(),
        }
    }

    fn started(id: &str, facet: ToolFacet) -> AdapterEvent {
        AdapterEvent::ToolStarted {
            turn_id: Uuid::now_v7(),
            tool_use_id: id.to_owned(),
            kind: ToolKind::Builtin,
            name: "ReportFindings".to_owned(),
            input: serde_json::json!({}),
            facet,
        }
    }

    fn completed(id: &str, is_error: bool) -> AdapterEvent {
        AdapterEvent::ToolCompleted {
            turn_id: Uuid::now_v7(),
            tool_use_id: id.to_owned(),
            output: "tool output that must not be forwarded".to_owned(),
            is_error,
        }
    }

    /// Text chunks are given as the live parser emits them: a block after
    /// earlier answer content already carries its `\n\n`.
    fn capture(events: &[AdapterEvent]) -> String {
        let mut capture = TextCapture::default();
        for event in events {
            capture.observe(event);
        }
        capture.finish()
    }

    #[test]
    fn capture_places_a_review_between_text_blocks() {
        let text = capture(&[
            chunk("Intro."),
            started("f1", findings_facet("REVIEW")),
            completed("f1", false),
            chunk("\n\nOutro."),
        ]);
        assert_eq!(text, "Intro.\n\nREVIEW\n\nOutro.");
    }

    #[test]
    fn capture_of_a_review_only_turn_is_the_review() {
        assert_eq!(
            capture(&[
                started("f1", findings_facet("REVIEW")),
                completed("f1", false)
            ]),
            "REVIEW"
        );
        assert_eq!(
            capture(&[
                started("f1", findings_facet("REVIEW")),
                completed("f1", false),
                chunk("\n\nack"),
            ]),
            "REVIEW\n\nack"
        );
    }

    #[test]
    fn capture_includes_a_retried_review_once() {
        let text = capture(&[
            started("f1", findings_facet("REVIEW")),
            completed("f1", true),
            started("f2", findings_facet("REVIEW")),
            completed("f2", false),
        ]);
        assert_eq!(text, "REVIEW");
    }

    #[test]
    fn capture_excludes_a_review_that_never_completes() {
        let text = capture(&[started("f1", findings_facet("UNANSWERED")), chunk("done")]);
        assert_eq!(text, "done");
    }

    #[test]
    fn capture_joins_two_reviews() {
        let text = capture(&[
            started("f1", findings_facet("FIRST")),
            completed("f1", false),
            started("f2", findings_facet("SECOND")),
            completed("f2", false),
        ]);
        assert_eq!(text, "FIRST\n\nSECOND");
    }

    /// Two reviews in one reply may complete in either order (the CLI runs
    /// them concurrently). Position comes from the call's start, as on disk.
    #[test]
    fn capture_keeps_start_order_when_reviews_complete_in_reverse() {
        let text = capture(&[
            started("a", findings_facet("A")),
            started("b", findings_facet("B")),
            completed("b", false),
            completed("a", false),
            chunk("\n\nack"),
        ]);
        assert_eq!(text, "A\n\nB\n\nack");
    }

    #[test]
    fn capture_drops_an_earlier_review_that_fails_after_a_later_one_succeeds() {
        let text = capture(&[
            started("a", findings_facet("A")),
            started("b", findings_facet("B")),
            completed("b", false),
            completed("a", true),
            chunk("\n\nack"),
        ]);
        assert_eq!(text, "B\n\nack");
    }

    #[test]
    fn disk_text_keeps_start_order_and_drops_a_failed_earlier_review() {
        let both = disk_text(vec![
            findings_item("a", "A", Some(false)),
            findings_item("b", "B", Some(false)),
            text_item("ack"),
        ]);
        assert_eq!(both, "A\n\nB\n\nack");
        let later_only = disk_text(vec![
            findings_item("a", "A", Some(true)),
            findings_item("b", "B", Some(false)),
            text_item("ack"),
        ]);
        assert_eq!(later_only, "B\n\nack");
    }

    #[test]
    fn capture_excludes_thinking_and_ordinary_tool_output() {
        let text = capture(&[
            AdapterEvent::ContentChunk {
                turn_id: Uuid::now_v7(),
                kind: ContentKind::Thinking,
                text: "secret reasoning".to_owned(),
            },
            chunk("visible."),
            started("t1", ToolFacet::Other),
            completed("t1", false),
        ]);
        assert_eq!(text, "visible.");
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
