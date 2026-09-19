//! Claude's `/context` breakdown — what is occupying an agent's context window.
//!
//! The CLI reports the same breakdown twice per run: as a **structured object**
//! (`context_usage` live, `contextUsage` on disk) and as the **markdown** it
//! prints. [`decode`] reads the object and keeps the markdown only as a
//! fallback, because the object is exact where the text is rounded — a skill the
//! markdown renders as `~30` is `31` in the object, and `4k` is `4026`.
//!
//! The markdown parser is therefore a **scraped-format** parser in the same
//! sense as the Codex skills block: a rendering meant for a human, with no
//! stability contract, kept only so an older CLI that predates the object still
//! produces a panel. Its failure mode is bounded by construction — the raw text
//! is retained on every path, so a report nothing can parse still shows the user
//! exactly what the CLI printed.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One row of the breakdown: a label, an optional secondary label, and a token
/// count. One type for four lists (MCP tools, memory files, custom agents,
/// skills) because the CLI gives all four the same shape — a name, one
/// qualifier, and tokens — and the panel renders them identically. What the
/// qualifier *means* differs per list and is documented on each field of
/// [`ContextReport`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextItem {
    pub name: String,
    /// The row's qualifier — the MCP server, the memory file's type, the
    /// agent's or skill's source. `None` when the CLI reported none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub tokens: u64,
    /// The CLI rounded this count (`~30`, `< 20`) and the exact value is lost.
    /// **Only the markdown fallback can set it** — the structured object is
    /// exact — so it doubles as a signal that a report came from the scraped
    /// path.
    #[serde(default, skip_serializing_if = "is_false")]
    pub approximate: bool,
}

/// One category row of the "estimated usage by category" table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextCategory {
    pub name: String,
    pub tokens: u64,
    /// The CLI's own classification — observed `used`, `deferred`, `buffer`,
    /// `free`. Deliberately an opaque string, for the reason
    /// [`crate::events::McpServerStatus::status`] is one and unknown rate-limit
    /// window keys are dropped rather than relabelled: the set is the CLI's, not
    /// ours, and a category we cannot classify must still render under its own
    /// name rather than be invented a meaning for. Empty when the markdown
    /// fallback produced the row, which carries no kind.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    /// See [`ContextItem::approximate`].
    #[serde(default, skip_serializing_if = "is_false")]
    pub approximate: bool,
}

/// A decoded `/context` report.
///
/// **No percentage field, on purpose.** The CLI's own `percentage` is a rounded
/// whole number (a 1.48%-full window reports `1`), and every percentage the
/// panel shows — the header and every category row — is `tokens / max_tokens`,
/// the same arithmetic the CLI does to print its table. Deriving at render
/// keeps one formula instead of two that can disagree, and a category row
/// carries no percentage in the structured object at all.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ContextReport {
    /// The model whose window this measures, as the CLI named it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Tokens occupied, and the window's size. `None` when neither decoder
    /// found them — the panel then renders the raw text alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// In the CLI's own order, which is not the markdown's: the object lists
    /// the autocompact buffer before free space and the table prints it after.
    /// The object's order wins because it is the one we read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<ContextCategory>,
    /// `detail` is the MCP server the tool belongs to; the panel groups on it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_tools: Vec<ContextItem>,
    /// `name` is the file's full path, `detail` its type (`User`, `Project`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory_files: Vec<ContextItem>,
    /// `detail` is where the agent was defined (`userSettings`, `builtin`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<ContextItem>,
    /// `detail` is where the skill came from (`userSettings`, `builtin`,
    /// `claudeAiSync`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<ContextItem>,
    /// The markdown the CLI printed, verbatim and always retained — the safety
    /// net that makes a decode failure survivable instead of a blank panel.
    pub raw: String,
    /// Neither decoder produced anything usable. The panel shows `raw` and says
    /// so; the turn still completes (a report Switchboard cannot read is not a
    /// harness failure).
    #[serde(default, skip_serializing_if = "is_false")]
    pub unparsed: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
}

/// Decode a report from whichever source the CLI gave us.
///
/// `structured` is `context_usage` (live) or `contextUsage` (on disk);
/// `raw` is the printed markdown with any `<local-command-stdout>` wrapper
/// already stripped. Structured first, markdown second, and — when both fail —
/// a report that carries only the raw text with [`ContextReport::unparsed`] set.
/// **Never returns `None`**: every call site's correct behaviour on a decode
/// failure is to show the user the text, not to drop the report or fail the
/// turn.
#[must_use]
pub fn decode(structured: Option<&Value>, raw: &str) -> ContextReport {
    if let Some(report) = structured.and_then(|value| from_structured(value, raw)) {
        return report;
    }
    if let Some(report) = from_markdown(raw) {
        return report;
    }
    ContextReport {
        raw: raw.to_owned(),
        unparsed: true,
        ..ContextReport::default()
    }
}

/// Read one token count from the structured object.
///
/// **A count this cannot read fails the whole decode rather than becoming a
/// zero**, which is the difference between the panel falling back to the
/// printed table and the panel confidently rendering every row as `0`. The
/// second is the worse outcome by far: nothing about it says anything went
/// wrong, and it is indistinguishable from a genuinely empty context.
///
/// A whole-number float is *accepted* rather than rejected. JSON has a single
/// number type, so a CLI that starts emitting `4026.0` has not changed what it
/// means — falling back to the rounded markdown there would discard an exact
/// value we can plainly read. A fractional count (`4026.7`) is refused: tokens
/// are not fractional, so rounding one and presenting it as measured would
/// re-introduce the invented number this function exists to prevent.
fn token_count(row: &Value, key: &str) -> Option<u64> {
    let value = row.get(key)?;
    if let Some(exact) = value.as_u64() {
        return Some(exact);
    }
    let number = value.as_f64()?;
    // Three separate refusals, each with its own job: not a number at all, not a
    // whole one (rounding it and calling it measured is the invented number this
    // function exists to refuse), and outside what `u64` can hold — where the
    // cast below would silently saturate to a wrong count.
    #[allow(clippy::cast_precision_loss)]
    // Strictly less than: `u64::MAX as f64` rounds *up* to 2^64, so `<=` would
    // admit exactly 2^64 and the cast below would saturate to `u64::MAX` — the
    // one outcome this guard exists to refuse.
    let in_range = number >= 0.0 && number < u64::MAX as f64;
    if !number.is_finite() || number.fract() != 0.0 || !in_range {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(number as u64)
}

/// Read the structured object. `None` when it carries neither a window size nor
/// a single category — an object that empty tells the user nothing the markdown
/// would not tell them better, so the fallback should get its turn — and `None`
/// when any row's token count is unreadable (see [`token_count`]).
fn from_structured(value: &Value, raw: &str) -> Option<ContextReport> {
    let object = value.as_object()?;
    let categories: Vec<ContextCategory> = match object.get("categories") {
        Some(rows) => rows
            .as_array()?
            .iter()
            // Two different rejections, in two stages: a nameless row is
            // dropped and the rest of the list still decodes, while an
            // unreadable count fails the collect and takes the whole decode
            // with it.
            .filter(|row| row.get("name").and_then(Value::as_str).is_some())
            .map(structured_category)
            .collect::<Option<Vec<ContextCategory>>>()?,
        None => Vec::new(),
    };
    // An absent `raw_max_tokens` is "the CLI did not report a window"; a present
    // but unreadable one is a shape change, and the two must not collapse — the
    // second fails the decode so the markdown gets its turn.
    let max_tokens = match object.get("raw_max_tokens") {
        Some(_) => Some(token_count(value, "raw_max_tokens")?),
        None => None,
    };
    let total_tokens = match object.get("total_tokens") {
        Some(_) => Some(token_count(value, "total_tokens")?),
        None => None,
    };
    if categories.is_empty() && max_tokens.is_none() {
        return None;
    }
    Some(ContextReport {
        model: object
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned),
        total_tokens,
        max_tokens,
        categories,
        mcp_tools: structured_items(object.get("mcp_tools"), "name", "server_name")?,
        memory_files: structured_items(object.get("memory_files"), "path", "type")?,
        agents: structured_items(object.get("agents"), "agent_type", "source")?,
        skills: structured_items(object.get("skills"), "name", "source")?,
        raw: raw.to_owned(),
        unparsed: false,
    })
}

/// `None` when the row's token count is unreadable, which fails the whole
/// decode at the caller's collect. Nameless rows are filtered out before this
/// runs, so the only rejection here is the count.
fn structured_category(row: &Value) -> Option<ContextCategory> {
    Some(ContextCategory {
        name: row.get("name").and_then(Value::as_str)?.to_owned(),
        tokens: token_count(row, "tokens")?,
        kind: row
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        approximate: false,
    })
}

/// Project one of the object's four item lists. `name_key` and `detail_key`
/// differ per list because the CLI names the same two columns differently in
/// each; a row missing its name is dropped rather than rendered nameless.
///
/// **An absent list is an empty one; a present list with an unreadable count is
/// a decode failure.** The distinction matters because an account with no MCP
/// servers and a CLI whose shape moved must not look the same.
fn structured_items(
    list: Option<&Value>,
    name_key: &str,
    detail_key: &str,
) -> Option<Vec<ContextItem>> {
    let Some(list) = list else {
        return Some(Vec::new());
    };
    // A present list that is not an array is a shape change, not an empty
    // account: failing the decode here is what sends the reader to the printed
    // table, which may well carry the section perfectly. Returning an empty vec
    // would delete the section from a report still marked as read successfully —
    // the same silent loss the strict count read above exists to prevent.
    let rows = list.as_array()?;
    rows.iter()
        // Same two-stage rejection as the category list above.
        .filter(|row| row.get(name_key).and_then(Value::as_str).is_some())
        .map(|row| {
            Some(ContextItem {
                name: row.get(name_key).and_then(Value::as_str)?.to_owned(),
                detail: row
                    .get(detail_key)
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                    .map(str::to_owned),
                tokens: token_count(row, "tokens")?,
                approximate: false,
            })
        })
        .collect()
}

const CATEGORY_HEADING: &str = "### Estimated usage by category";
const MCP_HEADING: &str = "### MCP Tools";
const AGENTS_HEADING: &str = "### Custom Agents";
const MEMORY_HEADING: &str = "### Memory Files";
const SKILLS_HEADING: &str = "### Skills";
const MODEL_PREFIX: &str = "**Model:**";
const TOKENS_PREFIX: &str = "**Tokens:**";

/// Read the printed table. `None` when the text yields neither a window size nor
/// a category — which is how [`decode`] tells "an older CLI's report" apart from
/// "not a report at all" and reaches for the raw-text fallback.
fn from_markdown(raw: &str) -> Option<ContextReport> {
    let (total_tokens, max_tokens) = markdown_totals(raw).unzip();
    let categories: Vec<ContextCategory> = table_rows(raw, CATEGORY_HEADING)
        .into_iter()
        .filter_map(|cells| {
            let (tokens, approximate) = parse_token_cell(cells.get(1)?)?;
            Some(ContextCategory {
                name: (*cells.first()?).to_owned(),
                tokens,
                kind: String::new(),
                approximate,
            })
        })
        .collect();
    if categories.is_empty() && max_tokens.is_none() {
        return None;
    }
    Some(ContextReport {
        model: markdown_field(raw, MODEL_PREFIX),
        total_tokens,
        max_tokens,
        categories,
        mcp_tools: markdown_items(raw, MCP_HEADING, 0, 1, 2),
        memory_files: markdown_items(raw, MEMORY_HEADING, 1, 0, 2),
        agents: markdown_items(raw, AGENTS_HEADING, 0, 1, 2),
        skills: markdown_items(raw, SKILLS_HEADING, 0, 1, 2),
        raw: raw.to_owned(),
        unparsed: false,
    })
}

/// The column indices differ per table: Memory Files prints `| Type | Path |`
/// while the other three print the name first, so the caller states which cell
/// is which rather than the parser guessing from the header text.
fn markdown_items(
    raw: &str,
    heading: &str,
    name_column: usize,
    detail_column: usize,
    token_column: usize,
) -> Vec<ContextItem> {
    table_rows(raw, heading)
        .into_iter()
        .filter_map(|cells| {
            let (tokens, approximate) = parse_token_cell(cells.get(token_column)?)?;
            Some(ContextItem {
                name: (*cells.get(name_column)?).to_owned(),
                detail: cells
                    .get(detail_column)
                    .filter(|text| !text.is_empty())
                    .map(|text| (*text).to_owned()),
                tokens,
                approximate,
            })
        })
        .collect()
}

/// The value after a `**Label:**` line, with the CLI's trailing hard-break
/// spaces removed.
fn markdown_field(raw: &str, prefix: &str) -> Option<String> {
    raw.lines()
        .find_map(|line| line.trim().strip_prefix(prefix))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// `**Tokens:** 14.8k / 1m (1%)` → `(14800, 1000000)`. Both halves must parse:
/// a used count with no window is not a meter, and reporting one without the
/// other would render a bar with no scale.
fn markdown_totals(raw: &str) -> Option<(u64, u64)> {
    let line = markdown_field(raw, TOKENS_PREFIX)?;
    let figures = line.split('(').next().unwrap_or(&line);
    let (used, max) = figures.split_once('/')?;
    let (used, _) = parse_token_cell(used.trim())?;
    let (max, _) = parse_token_cell(max.trim())?;
    Some((used, max))
}

/// The rows of the pipe table that follows `heading`, each split into trimmed
/// cells. Stops at the next `### ` heading, so a table never absorbs the one
/// below it; skips the header row and the `|---|` rule.
fn table_rows<'a>(raw: &'a str, heading: &str) -> Vec<Vec<&'a str>> {
    let mut lines = raw.lines().skip_while(|line| line.trim() != heading);
    if lines.next().is_none() {
        return Vec::new();
    }
    lines
        .take_while(|line| !line.trim_start().starts_with("### "))
        .map(str::trim)
        .filter(|line| line.starts_with('|'))
        .map(|line| {
            line.trim_matches('|')
                .split('|')
                .map(str::trim)
                .collect::<Vec<&str>>()
        })
        .filter(|cells| {
            // The rule row (`|------|------|`) and the header row, dropped by
            // shape rather than by position: a table whose header the CLI
            // reworded still parses, and a rule row can never be mistaken for
            // data because no real cell is all dashes.
            !cells
                .iter()
                .all(|cell| cell.chars().all(|c| c == '-' || c == ':') && !cell.is_empty())
                && cells.iter().any(|cell| parse_token_cell(cell).is_some())
        })
        .collect()
}

/// `366` → `(366, false)`; `4k` → `(4000, false)`; `952.1k` → `(952100, false)`;
/// `1m` → `(1000000, false)`; `~30` → `(30, true)`; `< 20` → `(20, true)`.
///
/// The rounding the CLI applies here is one-way and lossy, which is the whole
/// reason the structured object is preferred: `4k` could be anything from 3950
/// to 4049.
fn parse_token_cell(cell: &str) -> Option<(u64, bool)> {
    let cell = cell.trim();
    let (digits, approximate) = match cell.strip_prefix('~').or_else(|| cell.strip_prefix('<')) {
        Some(rest) => (rest.trim(), true),
        None => (cell, false),
    };
    let (number, scale) = match digits.strip_suffix(['k', 'K']) {
        Some(number) => (number, 1_000.0_f64),
        None => match digits.strip_suffix(['m', 'M']) {
            Some(number) => (number, 1_000_000.0_f64),
            None => (digits, 1.0_f64),
        },
    };
    let value: f64 = number.trim().replace(',', "").parse().ok()?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    let scaled = (value * scale).round();
    // A percentage cell (`0.4%`) fails the parse above on its suffix, so the
    // only way to land here is a real count; the cast is bounded by the guard.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some((scaled as u64, approximate))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The `/context` run recorded from claude 2.1.274, truncated to a handful
    /// of MCP tools across two servers and four skills (one exact, one `~N`,
    /// one `< N` in the markdown).
    const SESSION_FIXTURE: &str =
        include_str!("../tests/fixtures/claude/context-report.session.jsonl");

    /// The structured object and the markdown from the fixture's
    /// `system/local_command` record, as the disk parser will hand them over.
    fn fixture_parts() -> (Value, String) {
        let record: Value = SESSION_FIXTURE
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|record| record.get("contextUsage").is_some())
            .expect("fixture carries a contextUsage record");
        let content = record
            .get("content")
            .and_then(Value::as_str)
            .expect("string content");
        let raw = content
            .strip_prefix("<local-command-stdout>")
            .and_then(|text| text.strip_suffix("</local-command-stdout>"))
            .expect("wrapped output");
        (record["contextUsage"].clone(), raw.to_owned())
    }

    #[test]
    fn the_structured_object_decodes_every_list() {
        let (structured, raw) = fixture_parts();
        let report = decode(Some(&structured), &raw);

        assert!(!report.unparsed);
        assert_eq!(report.model.as_deref(), Some("claude-fable-5-1"));
        assert_eq!(report.max_tokens, Some(1_000_000));
        assert_eq!(report.total_tokens, Some(25_081));
        assert_eq!(report.categories.len(), 11);
        assert_eq!(report.mcp_tools.len(), 4);
        assert_eq!(report.memory_files.len(), 1);
        assert_eq!(report.agents.len(), 1);
        assert_eq!(report.skills.len(), 4);
        assert_eq!(report.raw, raw);
    }

    #[test]
    fn the_structured_object_keeps_the_clis_category_order_and_kinds() {
        let (structured, raw) = fixture_parts();
        let report = decode(Some(&structured), &raw);

        let names: Vec<&str> = report
            .categories
            .iter()
            .map(|category| category.name.as_str())
            .collect();
        assert_eq!(names.first().copied(), Some("System prompt"));
        // The object ends buffer-then-free; the printed table ends free-then-buffer.
        // Reading the object means the panel shows the object's order.
        assert_eq!(
            names[names.len() - 2..],
            ["Autocompact buffer", "Free space"]
        );

        let kinds: Vec<&str> = report
            .categories
            .iter()
            .map(|category| category.kind.as_str())
            .collect();
        assert!(kinds.contains(&"used"));
        assert!(kinds.contains(&"deferred"));
        assert!(kinds.contains(&"buffer"));
        assert!(kinds.contains(&"free"));
    }

    #[test]
    fn the_structured_object_carries_each_lists_own_qualifier() {
        let (structured, raw) = fixture_parts();
        let report = decode(Some(&structured), &raw);

        let tool = &report.mcp_tools[0];
        assert_eq!(tool.name, "mcp__docs__read");
        assert_eq!(tool.detail.as_deref(), Some("docs"));
        assert_eq!(tool.tokens, 300);

        let memory = &report.memory_files[0];
        assert_eq!(memory.name, "/Users/example/.claude/CLAUDE.md");
        assert_eq!(memory.detail.as_deref(), Some("User"));

        let agent = &report.agents[0];
        assert_eq!(agent.name, "Jenny");
        assert_eq!(agent.detail.as_deref(), Some("userSettings"));
    }

    #[test]
    fn the_structured_object_is_exact_where_the_markdown_rounds() {
        let (structured, raw) = fixture_parts();
        let structured_report = decode(Some(&structured), &raw);
        let markdown_report = decode(None, &raw);

        let exact = &structured_report.skills[1];
        assert_eq!(exact.name, "deep-research");
        assert_eq!(exact.tokens, 16, "the object carries the real count");
        assert!(!exact.approximate);

        let rounded = &markdown_report.skills[1];
        assert_eq!(rounded.name, "deep-research");
        assert_eq!(rounded.tokens, 20, "the table printed `< 20`");
        assert!(
            rounded.approximate,
            "a count the CLI rounded must say so, or the panel presents 20 as measured"
        );
    }

    #[test]
    fn the_markdown_fallback_decodes_the_whole_report() {
        let (_, raw) = fixture_parts();
        let report = decode(None, &raw);

        assert!(!report.unparsed);
        assert_eq!(report.model.as_deref(), Some("claude-fable-5-1"));
        assert_eq!(report.max_tokens, Some(1_000_000));
        assert_eq!(
            report.total_tokens,
            Some(25_100),
            "`25.1k`, rounded by the CLI"
        );
        assert_eq!(report.categories.len(), 11);
        assert_eq!(report.mcp_tools.len(), 4);
        assert_eq!(report.agents.len(), 1);
        assert_eq!(report.skills.len(), 4);
    }

    #[test]
    fn the_markdown_memory_table_reads_its_reversed_columns() {
        let (_, raw) = fixture_parts();
        let report = decode(None, &raw);

        // `| Type | Path | Tokens |` — the only table that does not lead with
        // the name, so a positional parser that assumed column 0 was the name
        // would label every memory file "User".
        let memory = &report.memory_files[0];
        assert_eq!(memory.name, "/Users/example/.claude/CLAUDE.md");
        assert_eq!(memory.detail.as_deref(), Some("User"));
        assert_eq!(memory.tokens, 167);
    }

    #[test]
    fn the_markdown_tables_do_not_absorb_the_section_below_them() {
        let (_, raw) = fixture_parts();
        let report = decode(None, &raw);

        assert_eq!(report.mcp_tools.len(), 4);
        assert!(
            report
                .mcp_tools
                .iter()
                .all(|tool| tool.name.starts_with("mcp__")),
            "the Custom Agents rows must not land in the MCP list: {:?}",
            report.mcp_tools
        );
    }

    #[test]
    fn a_structured_object_missing_its_lists_still_decodes() {
        let report = decode(
            Some(&json!({
                "model": "claude-opus-5",
                "total_tokens": 100,
                "raw_max_tokens": 200_000,
                "categories": [{"name": "Messages", "tokens": 100, "kind": "used"}],
            })),
            "## Context Usage",
        );

        assert!(!report.unparsed);
        assert_eq!(report.max_tokens, Some(200_000));
        assert!(report.mcp_tools.is_empty());
        assert!(report.skills.is_empty());
    }

    #[test]
    fn an_unknown_category_kind_is_kept_verbatim() {
        let report = decode(
            Some(&json!({
                "raw_max_tokens": 200_000,
                "categories": [{"name": "Attachments", "tokens": 42, "kind": "pinned"}],
            })),
            "",
        );

        assert_eq!(report.categories[0].kind, "pinned");
        assert_eq!(report.categories[0].name, "Attachments");
    }

    #[test]
    fn a_category_with_no_kind_is_kept_rather_than_dropped() {
        let report = decode(
            Some(&json!({
                "raw_max_tokens": 200_000,
                "categories": [{"name": "Messages", "tokens": 42}],
            })),
            "",
        );

        assert_eq!(report.categories.len(), 1);
        assert_eq!(report.categories[0].kind, "");
    }

    #[test]
    fn a_garbled_object_falls_through_to_the_markdown() {
        let (_, raw) = fixture_parts();
        let report = decode(Some(&json!({"categories": "not-a-list"})), &raw);

        assert!(!report.unparsed);
        assert_eq!(report.categories.len(), 11);
        assert!(
            report.skills.iter().any(|skill| skill.approximate),
            "the markdown path ran, so its rounded skill counts must be flagged"
        );
    }

    #[test]
    fn a_report_neither_path_can_read_keeps_its_raw_text() {
        let raw = "## Context Usage\n\nthe CLI printed something else entirely\n";
        let report = decode(Some(&json!({"categories": []})), raw);

        assert!(report.unparsed);
        assert_eq!(report.raw, raw);
        assert!(report.categories.is_empty());
        assert_eq!(report.total_tokens, None);
    }

    #[test]
    fn a_row_with_no_name_is_dropped_rather_than_rendered_nameless() {
        let report = decode(
            Some(&json!({
                "raw_max_tokens": 200_000,
                "categories": [{"name": "Messages", "tokens": 1, "kind": "used"}],
                "skills": [{"source": "userSettings", "tokens": 31}, {"name": "kept", "tokens": 5}],
            })),
            "",
        );

        assert_eq!(report.skills.len(), 1);
        assert_eq!(report.skills[0].name, "kept");
    }

    #[test]
    fn an_unreadable_category_count_falls_through_to_the_markdown() {
        // The failure this prevents is silent: with a zero default every row
        // renders `0`, nothing is flagged, and the printed table that would
        // have parsed is never consulted.
        let (_, raw) = fixture_parts();
        let report = decode(
            Some(&json!({
                "raw_max_tokens": 1_000_000,
                "categories": [{"name": "Messages", "tokens": "4,026", "kind": "used"}],
            })),
            &raw,
        );

        assert!(!report.unparsed, "the markdown rescued it");
        assert_eq!(report.categories.len(), 11);
        assert!(
            report.skills.iter().any(|skill| skill.approximate),
            "the markdown path ran: {report:?}"
        );
    }

    #[test]
    fn an_unreadable_item_count_falls_through_too() {
        let (_, raw) = fixture_parts();
        let report = decode(
            Some(&json!({
                "raw_max_tokens": 1_000_000,
                "categories": [{"name": "Messages", "tokens": 10, "kind": "used"}],
                "skills": [{"name": "dataviz", "tokens": null}],
            })),
            &raw,
        );

        assert_eq!(
            report.categories.len(),
            11,
            "decoded from the table instead"
        );
    }

    #[test]
    fn an_unreadable_window_size_falls_through_rather_than_vanishing() {
        // Reading the header totals leniently while the rows are strict would
        // leave the panel with meters and no scale.
        let (_, raw) = fixture_parts();
        let report = decode(
            Some(&json!({
                "raw_max_tokens": "1m",
                "categories": [{"name": "Messages", "tokens": 10, "kind": "used"}],
            })),
            &raw,
        );

        assert!(!report.unparsed);
        assert_eq!(report.max_tokens, Some(1_000_000));
        assert_eq!(report.categories.len(), 11);
    }

    #[test]
    fn a_report_with_neither_a_readable_object_nor_readable_text_is_unparsed() {
        let report = decode(
            Some(&json!({
                "raw_max_tokens": 1_000_000,
                "categories": [{"name": "Messages", "tokens": "4,026"}],
            })),
            "nothing parseable here",
        );

        assert!(report.unparsed);
        assert_eq!(report.raw, "nothing parseable here");
    }

    #[test]
    fn a_whole_number_float_is_read_rather_than_refused() {
        // JSON has one number type, so `4026.0` has not changed what the CLI
        // means — falling back to the rounded table would discard an exact
        // value we can plainly read.
        let report = decode(
            Some(&json!({
                "raw_max_tokens": 200_000.0,
                "total_tokens": 4_026.0,
                "categories": [{"name": "Messages", "tokens": 4_026.0, "kind": "used"}],
            })),
            "",
        );

        assert!(!report.unparsed);
        assert_eq!(report.max_tokens, Some(200_000));
        assert_eq!(report.total_tokens, Some(4_026));
        assert_eq!(report.categories[0].tokens, 4_026);
        assert!(!report.categories[0].approximate);
    }

    #[test]
    fn a_fractional_count_is_refused_rather_than_rounded() {
        // Tokens are not fractional. Rounding one and presenting it as measured
        // is the same invented number the zero default produced.
        let report = decode(
            Some(&json!({
                "raw_max_tokens": 200_000,
                "categories": [{"name": "Messages", "tokens": 4_026.7, "kind": "used"}],
            })),
            "",
        );

        assert!(report.unparsed);
    }

    #[test]
    fn a_count_too_large_for_the_type_is_refused_rather_than_saturated() {
        // The cast would clamp to `u64::MAX` and present it as the measurement.
        let report = decode(
            Some(&json!({
                "raw_max_tokens": 200_000,
                "categories": [{"name": "Messages", "tokens": 1e30, "kind": "used"}],
            })),
            "",
        );

        assert!(report.unparsed);
    }

    #[test]
    fn a_genuine_zero_stays_a_measured_zero() {
        let report = decode(
            Some(&json!({
                "raw_max_tokens": 200_000,
                "categories": [{"name": "Messages", "tokens": 0, "kind": "used"}],
            })),
            "",
        );

        assert!(!report.unparsed);
        assert_eq!(report.categories[0].tokens, 0);
    }

    #[test]
    fn a_list_that_is_not_an_array_falls_through_to_the_markdown() {
        // The section would otherwise vanish from a report still marked as read
        // successfully, with the table that carries it never consulted.
        let (_, raw) = fixture_parts();
        let report = decode(
            Some(&json!({
                "raw_max_tokens": 1_000_000,
                "categories": [{"name": "Messages", "tokens": 10, "kind": "used"}],
                "mcp_tools": {"docs": ["read", "update"]},
            })),
            &raw,
        );

        assert!(!report.unparsed, "the markdown rescued it");
        assert_eq!(
            report.mcp_tools.len(),
            4,
            "the table's MCP rows are what the panel shows: {report:?}"
        );
        assert_eq!(report.categories.len(), 11);
    }

    #[test]
    fn a_count_of_exactly_two_to_the_sixty_fourth_is_refused() {
        // The boundary the range guard exists for: `u64::MAX as f64` rounds up
        // to this value, so a `<=` comparison would admit it and the cast would
        // saturate to `u64::MAX`.
        let report = decode(
            Some(&json!({
                "raw_max_tokens": 200_000,
                "categories": [{"name": "Messages", "tokens": 18_446_744_073_709_551_616.0_f64,
                                "kind": "used"}],
            })),
            "",
        );

        assert!(report.unparsed);
    }

    #[test]
    fn an_absent_list_is_empty_while_an_unreadable_one_is_a_failure() {
        // An account with no MCP servers and a CLI whose shape moved must not
        // look the same.
        let absent = decode(
            Some(&json!({
                "raw_max_tokens": 200_000,
                "categories": [{"name": "Messages", "tokens": 1, "kind": "used"}],
            })),
            "",
        );
        assert!(!absent.unparsed);
        assert!(absent.mcp_tools.is_empty());

        let unreadable = decode(
            Some(&json!({
                "raw_max_tokens": 200_000,
                "categories": [{"name": "Messages", "tokens": 1, "kind": "used"}],
                "mcp_tools": [{"name": "t", "server_name": "s", "tokens": {}}],
            })),
            "",
        );
        assert!(unreadable.unparsed);
    }

    #[test]
    fn token_cells_cover_every_form_the_cli_prints() {
        assert_eq!(parse_token_cell("366"), Some((366, false)));
        assert_eq!(parse_token_cell("4k"), Some((4_000, false)));
        assert_eq!(parse_token_cell("1.9k"), Some((1_900, false)));
        assert_eq!(parse_token_cell("952.1k"), Some((952_100, false)));
        assert_eq!(parse_token_cell("1m"), Some((1_000_000, false)));
        assert_eq!(parse_token_cell("~30"), Some((30, true)));
        assert_eq!(parse_token_cell("< 20"), Some((20, true)));
        assert_eq!(
            parse_token_cell("0.4%"),
            None,
            "a percentage is not a count"
        );
        assert_eq!(parse_token_cell("Tokens"), None);
        assert_eq!(parse_token_cell(""), None);
    }

    #[test]
    fn a_tokens_line_missing_its_window_size_yields_no_totals() {
        // Half a meter is worse than none: a used count with no window renders
        // a bar with no scale.
        let raw = "## Context Usage\n\n**Tokens:** 14.8k\n\n### Estimated usage by category\n\n\
                   | Category | Tokens |\n|---|---|\n| Messages | 10 |\n";
        let report = decode(None, raw);

        assert_eq!(report.total_tokens, None);
        assert_eq!(report.max_tokens, None);
        assert_eq!(report.categories.len(), 1);
    }
}
