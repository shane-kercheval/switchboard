//! Claude Code's `ReportFindings` tool: a code review delivered as tool input
//! instead of reply text.
//!
//! The tool's reply to the model is only `N findings reported.`, and the model
//! is told the host renders the findings and not to repeat them as text — so
//! the `tool_use` input is often the only copy of the review. This module turns
//! that input into a [`ToolFacet::Findings`], including the markdown that copy,
//! forward and workflow output carry.
//!
//! Input shape verified live @ claude 2.1.289 (fixtures
//! `tests/fixtures/claude/report-findings*.jsonl`; `docs/harness-behavior.md`
//! §3.6). The tool has no public documentation; its schema comes from the
//! CLI's own definition. Parsing mirrors Claude Desktop's renderer: lenient per
//! finding, and a call whose findings are all unusable falls to `Other` so its
//! raw input stays visible instead of an empty card.

use serde_json::Value;

use crate::facets::{Finding, FindingOutcome, FindingVerdict, FindingsReport, ToolFacet};

/// The tool's name in Claude Code's vocabulary.
pub(crate) const REPORT_FINDINGS_TOOL: &str = "ReportFindings";

/// Classify a `ReportFindings` input. `findings` must be an array; each entry
/// needs non-empty string `file` and `summary`, and any other malformed field
/// is treated as absent rather than dropping the finding. An empty array is a
/// valid "no findings" review.
pub(crate) fn report_findings_facet(input: &Value) -> ToolFacet {
    let Some(raw) = input.get("findings").and_then(Value::as_array) else {
        return ToolFacet::Other;
    };
    let findings: Vec<Finding> = raw.iter().filter_map(parse_finding).collect();
    if !raw.is_empty() && findings.is_empty() {
        return ToolFacet::Other;
    }
    let level = non_empty_str(input, "level");
    let text = findings_markdown(level.as_deref(), &findings);
    ToolFacet::Findings(Box::new(FindingsReport {
        level,
        findings,
        text,
    }))
}

fn parse_finding(raw: &Value) -> Option<Finding> {
    Some(Finding {
        file: non_empty_str(raw, "file")?,
        line: raw
            .get("line")
            .and_then(Value::as_u64)
            .filter(|line| *line >= 1)
            .and_then(|line| u32::try_from(line).ok()),
        summary: non_empty_str(raw, "summary")?,
        short_summary: non_empty_str(raw, "short_summary"),
        failure_scenario: non_empty_str(raw, "failure_scenario").unwrap_or_default(),
        category: non_empty_str(raw, "category"),
        verdict: match raw.get("verdict").and_then(Value::as_str) {
            Some("CONFIRMED") => Some(FindingVerdict::Confirmed),
            Some("PLAUSIBLE") => Some(FindingVerdict::Plausible),
            _ => None,
        },
        outcome: match raw.get("outcome").and_then(Value::as_str) {
            Some("fixed") => Some(FindingOutcome::Fixed),
            Some("skipped") => Some(FindingOutcome::Skipped),
            Some("no_change_needed") => Some(FindingOutcome::NoChangeNeeded),
            _ => None,
        },
    })
}

fn non_empty_str(obj: &Value, key: &str) -> Option<String> {
    obj.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
}

/// The review as standalone markdown. Uses the full `summary` (never
/// `short_summary`) plus the failure scenario, because a forwarded review is
/// all the receiving agent sees. Example:
///
/// ```text
/// **Code review · low · 2 findings**
///
/// 1. `src/app.py:12` · correctness · Confirmed
///    Off-by-one in the loop bound.
///    Failure scenario: A list of 3 items processes only 2.
/// 2. `README.md`
///    Outdated install command.
/// ```
fn findings_markdown(level: Option<&str>, findings: &[Finding]) -> String {
    let count = match findings.len() {
        0 => "no findings".to_owned(),
        1 => "1 finding".to_owned(),
        n => format!("{n} findings"),
    };
    let header = match level {
        Some(level) => format!("**Code review · {level} · {count}**"),
        None => format!("**Code review · {count}**"),
    };
    if findings.is_empty() {
        return header;
    }
    let mut out = header;
    out.push_str("\n\n");
    for (index, finding) in findings.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let marker = format!("{}. ", index + 1);
        // Continuation lines are indented by the marker's own width: CommonMark
        // ends a list item at a line indented less than its content column
        // after a blank line, so a fixed 3 spaces breaks items 10 and up.
        let indent = " ".repeat(marker.len());
        out.push_str(&marker);
        out.push_str(&finding_heading(finding));
        push_indented(&mut out, &indent, &finding.summary);
        if !finding.failure_scenario.is_empty() {
            push_indented(
                &mut out,
                &indent,
                &format!("Failure scenario: {}", finding.failure_scenario),
            );
        }
    }
    out
}

/// `` `file:line` · category · Confirmed · Fixed `` — whichever labels exist.
fn finding_heading(finding: &Finding) -> String {
    let location = match finding.line {
        Some(line) => format!("{}:{line}", finding.file),
        None => finding.file.clone(),
    };
    let mut parts = vec![code_span(&location)];
    if let Some(category) = &finding.category {
        parts.push(category.clone());
    }
    if let Some(verdict) = finding.verdict {
        parts.push(
            match verdict {
                FindingVerdict::Confirmed => "Confirmed",
                FindingVerdict::Plausible => "Plausible",
            }
            .to_owned(),
        );
    }
    if let Some(outcome) = finding.outcome {
        parts.push(
            match outcome {
                FindingOutcome::Fixed => "Fixed",
                FindingOutcome::Skipped => "Skipped",
                FindingOutcome::NoChangeNeeded => "No change needed",
            }
            .to_owned(),
        );
    }
    parts.join(" · ")
}

/// Append `value` on new lines under the current list item. Blank lines stay
/// empty (no trailing spaces); every other line gets the item's indent.
fn push_indented(out: &mut String, indent: &str, value: &str) {
    for line in value.lines() {
        out.push('\n');
        if !line.trim().is_empty() {
            out.push_str(indent);
            out.push_str(line);
        }
    }
}

/// A markdown code span that survives backticks inside `text`: the fence is
/// one backtick longer than the longest run inside, padded with spaces when
/// the text starts or ends with a backtick.
fn code_span(text: &str) -> String {
    let longest_run = text
        .split(|c| c != '`')
        .map(str::len)
        .max()
        .unwrap_or_default();
    let fence = "`".repeat(longest_run + 1);
    if longest_run > 0 && (text.starts_with('`') || text.ends_with('`')) {
        format!("{fence} {text} {fence}")
    } else {
        format!("{fence}{text}{fence}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn full_finding() -> Value {
        json!({
            "file": "src/app.py",
            "line": 12,
            "summary": "Off-by-one in the loop bound.",
            "short_summary": "Off-by-one in loop bound",
            "failure_scenario": "A list of 3 items processes only 2.",
            "category": "correctness",
            "verdict": "CONFIRMED",
        })
    }

    fn required_only_finding() -> Value {
        json!({
            "file": "README.md",
            "summary": "Outdated install command.",
            "failure_scenario": "Following the README fails on a fresh machine.",
        })
    }

    fn report_of(facet: &ToolFacet) -> &FindingsReport {
        let ToolFacet::Findings(report) = facet else {
            panic!("expected a Findings facet, got {facet:?}");
        };
        report
    }

    fn findings_of(facet: &ToolFacet) -> &[Finding] {
        &report_of(facet).findings
    }

    fn text_of(facet: &ToolFacet) -> &str {
        &report_of(facet).text
    }

    #[test]
    fn full_and_required_only_findings_parse() {
        let facet = report_findings_facet(
            &json!({"level": "low", "findings": [full_finding(), required_only_finding()]}),
        );
        let FindingsReport {
            level, findings, ..
        } = report_of(&facet);
        assert_eq!(level.as_deref(), Some("low"));
        assert_eq!(
            findings[0],
            Finding {
                file: "src/app.py".to_owned(),
                line: Some(12),
                summary: "Off-by-one in the loop bound.".to_owned(),
                short_summary: Some("Off-by-one in loop bound".to_owned()),
                failure_scenario: "A list of 3 items processes only 2.".to_owned(),
                category: Some("correctness".to_owned()),
                verdict: Some(FindingVerdict::Confirmed),
                outcome: None,
            }
        );
        assert_eq!(
            findings[1],
            Finding {
                file: "README.md".to_owned(),
                line: None,
                summary: "Outdated install command.".to_owned(),
                short_summary: None,
                failure_scenario: "Following the README fails on a fresh machine.".to_owned(),
                category: None,
                verdict: None,
                outcome: None,
            }
        );
    }

    #[test]
    fn zero_findings_is_a_valid_review() {
        let facet = report_findings_facet(&json!({"level": "medium", "findings": []}));
        assert!(findings_of(&facet).is_empty());
        assert_eq!(text_of(&facet), "**Code review · medium · no findings**");
    }

    #[test]
    fn missing_or_non_array_findings_falls_to_other() {
        assert_eq!(report_findings_facet(&json!({})), ToolFacet::Other);
        assert_eq!(
            report_findings_facet(&json!({"findings": "none"})),
            ToolFacet::Other
        );
        assert_eq!(report_findings_facet(&Value::Null), ToolFacet::Other);
    }

    #[test]
    fn all_findings_malformed_falls_to_other() {
        let facet = report_findings_facet(&json!({"findings": [
            {"summary": "no file"},
            {"file": "a.rs"},
            {"file": "  ", "summary": "blank file"},
            "not an object",
        ]}));
        assert_eq!(facet, ToolFacet::Other);
    }

    #[test]
    fn a_malformed_finding_is_dropped_and_the_rest_kept() {
        let facet = report_findings_facet(&json!({"findings": [
            {"summary": "no file"},
            required_only_finding(),
        ]}));
        let findings = findings_of(&facet);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, "README.md");
    }

    #[test]
    fn unknown_verdict_and_outcome_are_absent_not_fatal() {
        let facet = report_findings_facet(&json!({"findings": [{
            "file": "a.rs",
            "summary": "s",
            "failure_scenario": "f",
            "verdict": "confirmed",
            "outcome": "wontfix",
        }]}));
        let finding = &findings_of(&facet)[0];
        assert_eq!(finding.verdict, None);
        assert_eq!(finding.outcome, None);
    }

    #[test]
    fn unusable_line_numbers_are_absent() {
        for line in [
            json!(0),
            json!("12"),
            json!(-3),
            json!(1.5),
            json!(u64::MAX),
        ] {
            let facet = report_findings_facet(&json!({"findings": [{
                "file": "a.rs", "summary": "s", "failure_scenario": "f", "line": line,
            }]}));
            assert_eq!(findings_of(&facet)[0].line, None, "line {line}");
        }
    }

    #[test]
    fn known_outcomes_parse() {
        let facet = report_findings_facet(&json!({"findings": [
            {"file": "a.rs", "summary": "s", "failure_scenario": "f", "outcome": "fixed"},
            {"file": "b.rs", "summary": "s", "failure_scenario": "f", "outcome": "skipped"},
            {"file": "c.rs", "summary": "s", "failure_scenario": "f", "outcome": "no_change_needed"},
        ]}));
        let outcomes: Vec<_> = findings_of(&facet).iter().map(|f| f.outcome).collect();
        assert_eq!(
            outcomes,
            vec![
                Some(FindingOutcome::Fixed),
                Some(FindingOutcome::Skipped),
                Some(FindingOutcome::NoChangeNeeded),
            ]
        );
    }

    #[test]
    fn text_lists_every_finding_with_its_scenario() {
        let facet = report_findings_facet(
            &json!({"level": "low", "findings": [full_finding(), required_only_finding()]}),
        );
        assert_eq!(
            text_of(&facet),
            "**Code review · low · 2 findings**\n\
             \n\
             1. `src/app.py:12` · correctness · Confirmed\n   \
             Off-by-one in the loop bound.\n   \
             Failure scenario: A list of 3 items processes only 2.\n\
             2. `README.md`\n   \
             Outdated install command.\n   \
             Failure scenario: Following the README fails on a fresh machine."
        );
    }

    #[test]
    fn header_without_level_and_singular_count() {
        let facet = report_findings_facet(&json!({"findings": [required_only_finding()]}));
        assert!(
            text_of(&facet).starts_with("**Code review · 1 finding**\n\n1. `README.md`\n"),
            "{}",
            text_of(&facet)
        );
        let none = report_findings_facet(&json!({"findings": []}));
        assert_eq!(text_of(&none), "**Code review · no findings**");
    }

    #[test]
    fn heading_labels_follow_category_verdict_outcome_order() {
        let facet = report_findings_facet(&json!({"findings": [
            {"file": "a.rs", "line": 3, "summary": "s", "failure_scenario": "f",
             "category": "efficiency", "verdict": "PLAUSIBLE", "outcome": "no_change_needed"},
            {"file": "b.rs", "summary": "s", "failure_scenario": "f", "outcome": "skipped"},
            {"file": "c.rs", "summary": "s", "failure_scenario": "f", "verdict": "CONFIRMED",
             "outcome": "fixed"},
        ]}));
        let headings: Vec<&str> = text_of(&facet)
            .lines()
            .filter(|l| l.starts_with(|c: char| c.is_ascii_digit()))
            .collect();
        assert_eq!(
            headings,
            vec![
                "1. `a.rs:3` · efficiency · Plausible · No change needed",
                "2. `b.rs` · Skipped",
                "3. `c.rs` · Confirmed · Fixed",
            ]
        );
    }

    #[test]
    fn missing_failure_scenario_omits_its_line() {
        let facet = report_findings_facet(&json!({"findings": [{"file": "a.rs", "summary": "s"}]}));
        assert_eq!(
            text_of(&facet),
            "**Code review · 1 finding**\n\n1. `a.rs`\n   s"
        );
    }

    #[test]
    fn multi_line_values_stay_inside_their_item() {
        let facet = report_findings_facet(&json!({"findings": [{
            "file": "a.rs",
            "summary": "s",
            "failure_scenario": "Step one.\n\nStep two.",
        }]}));
        assert_eq!(
            text_of(&facet),
            "**Code review · 1 finding**\n\n1. `a.rs`\n   s\n   \
             Failure scenario: Step one.\n\n   Step two."
        );
    }

    /// Items 10 and up have a four-character marker, so their continuation
    /// lines need four spaces to stay inside the item after a blank line.
    #[test]
    fn continuation_indent_matches_a_two_digit_marker() {
        let mut raw: Vec<Value> = (1..=9)
            .map(|n| json!({"file": format!("f{n}.rs"), "summary": "s", "failure_scenario": "f"}))
            .collect();
        raw.push(json!({
            "file": "ten.rs",
            "summary": "Summary first para.",
            "failure_scenario": "Line one.\n\nAfter a blank line.",
        }));
        let facet = report_findings_facet(&json!({"findings": raw}));
        let text = text_of(&facet);
        let tenth = &text[text.find("10. ").unwrap()..];
        assert_eq!(
            tenth,
            "10. `ten.rs`\n    Summary first para.\n    \
             Failure scenario: Line one.\n\n    After a blank line."
        );
    }

    #[test]
    fn code_span_survives_backticks_in_the_path() {
        assert_eq!(code_span("plain.rs"), "`plain.rs`");
        assert_eq!(code_span("a`b.rs"), "``a`b.rs``");
        assert_eq!(code_span("`edge`"), "`` `edge` ``");
    }
}
