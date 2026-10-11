//! One size budget for every result, applied to the serialized JSON at the
//! tool layer. The rules for each result type live beside that type in `ops`
//! (see `send::wait_send_budget`, `read::conversation_budget`,
//! `read::agent_output_budget`), so M1's tests exercise what the tools ship.
//!
//! **What may be cut.** Only fields that hold free text, named by key:
//! reply and message text, tool input and output, prompt bodies. Ids, names,
//! paths, statuses, error codes, and error messages are never shortened —
//! "shrink the longest string" would otherwise pick a long error message or a
//! path the caller needs whole, and a result that is valid JSON but names a
//! truncated id is useless for the next call.
//!
//! **How the budget is shared.** Text fields share it evenly: the helper finds
//! the largest per-field cap at which the whole envelope fits and cuts every
//! longer field to that cap. Cutting the longest field by the whole overflow
//! would empty one recipient's reply and leave the others whole.
//!
//! **Why the search walks intervals.** Every newly cut field adds its own
//! record to the envelope, so lowering the cap past a field's length can make
//! the result *bigger*: "fits at this cap" is not monotone across the whole
//! range, and a plain binary search over it can skip every cap that fits.
//! Between two consecutive field lengths the set of cut fields is fixed and
//! size only shrinks with the cap, so the search takes those intervals from
//! the largest caps down and binary-searches inside the first that can fit.
//! Sizes are computed from a model of the serializer — each field's escaped
//! length at any cut point, each record's length — not by serializing at every
//! step, which on one large item with hundreds of tool outputs would mean
//! hundreds of serializations of megabytes. One real serialization at the end
//! confirms the result; a disagreement with the model only steps the cap
//! down, never returns an oversized result.
//!
//! **Why the search is cheap for any input.** A record's length depends on
//! the cap only through its digit count, so each field's record is serialized
//! at most once per digit count. A size check visits only the fields that cap
//! cuts, longest first. And the walk stops as soon as the fields already cut —
//! each at its shortest possible record, with its text emptied — cannot fit:
//! lowering the cap only cuts more, so no smaller cap can fit either. Every
//! cut field costs at least 60 characters of record, so at most `budget / 60`
//! fields are ever examined, however many the result holds.
//!
//! **When cutting is not enough.** A result with a droppable list — a
//! conversation's items — keeps whole items newest-first while they fit and
//! omits the older ones, reporting how many. The newest item is always kept;
//! if it alone is over, its text is cut. A fan-out's recipients are never
//! droppable. A single element that cannot fit even with every text field
//! emptied is an error naming the call that reads it in pages.
//!
//! **What a cut carries.** Every cut field is listed in the envelope with its
//! path (a JSON pointer), its full length, the length kept, and, when the
//! caller can fetch the rest, the call that reads on with the offset the cut
//! ended at. The caller decides that per result type: an agent reply pages
//! through `read_send_output`; a user message or a tool output has no paging
//! tool, and the envelope says so.
//!
//! **Progress is guaranteed.** Fields are addressed by JSON pointer, which
//! escapes any key; a field that still cannot be addressed is treated as not
//! cuttable rather than retried forever.
//!
//! The envelope wraps the result rather than annotating it in place, so each
//! result type's own shape crosses unchanged.

use serde::Serialize;
use serde_json::{Map, Value};

use crate::error::AppError;

/// Keys whose string values may be shortened.
const CUTTABLE_KEYS: &[&str] = &["text", "output", "input", "prompt", "body"];

/// Where a cut field sits, for the caller to decide how its remainder is read.
pub struct CutSite<'a> {
    /// JSON pointer from the result root (`/items/3/items/7/output`).
    pub path: &'a str,
    /// The cut field's key.
    pub key: &'a str,
    /// Characters kept, which is where a page reading on would start.
    pub kept_chars: usize,
    /// The objects enclosing the field, root first, innermost last.
    pub ancestors: &'a [&'a Map<String, Value>],
}

pub type Pointer = Box<dyn Fn(&CutSite<'_>) -> Option<Value> + Send + Sync>;

/// How to fit one kind of result.
pub struct BudgetSpec {
    /// Characters the serialized envelope may hold.
    pub budget_chars: usize,
    /// A top-level key of the result whose array elements may be dropped
    /// oldest-first when cutting text is not enough.
    pub droppable_list: Option<&'static str>,
    /// The call to read it in pages, for the error when one element cannot fit.
    pub read_with: &'static str,
    /// For a cut field, the call that reads the rest, or `None` when nothing
    /// can. Its serialized length may depend on the cap only through the
    /// digit count of `kept_chars` (embedding the offset as a number does):
    /// the search caches record lengths per digit count, and the final real
    /// serialization catches a pointer that breaks this.
    pub pointer: Pointer,
}

impl BudgetSpec {
    /// A spec whose cut fields have no paging call.
    #[must_use]
    pub fn unpageable(
        budget_chars: usize,
        droppable_list: Option<&'static str>,
        read_with: &'static str,
    ) -> Self {
        Self {
            budget_chars,
            droppable_list,
            read_with,
            pointer: Box::new(|_| None),
        }
    }
}

/// One cut field.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Truncation {
    pub path: String,
    pub total_chars: usize,
    pub kept_chars: usize,
    /// The call that reads the rest, or `null` when nothing can.
    pub read_with: Option<Value>,
}

/// A result fitted to the budget.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Budgeted {
    pub result: Value,
    pub truncated: Vec<Truncation>,
    /// Elements dropped from the front of the droppable list.
    pub items_omitted: usize,
}

impl Budgeted {
    /// The serialized size the budget is measured on.
    #[must_use]
    pub fn serialized_chars(&self) -> usize {
        measure(self)
    }
}

#[cfg(test)]
thread_local! {
    /// Serializations this thread performed — whole results and truncation
    /// records alike — and fields the size model visited, for tests that bound
    /// the helper's cost without a clock. Thread-local so tests running in
    /// parallel do not count each other's work.
    static SERIALIZATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static FIELD_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn measure<T: Serialize>(value: &T) -> usize {
    #[cfg(test)]
    SERIALIZATIONS.with(|count| count.set(count.get() + 1));
    serde_json::to_string(value).map_or(0, |s| s.chars().count())
}

fn count_field_visit() {
    #[cfg(test)]
    FIELD_VISITS.with(|count| count.set(count.get() + 1));
}

/// Decimal digits in `n`, which is all a record's length depends on.
fn decimal_digits(n: usize) -> u32 {
    n.checked_ilog10().map_or(1, |log| log + 1)
}

/// Characters `c` occupies in `serde_json`'s output: quotes, backslashes, and
/// the short-escaped control characters take two, other control characters
/// take six (`\u00XX`), everything else one. Shared with paging, which sizes a
/// page by what it serializes to.
pub(crate) fn json_escaped_len(c: char) -> usize {
    match c {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        c if (c as u32) < 0x20 => 6,
        _ => 1,
    }
}

/// Fit `value` to `spec`.
pub fn fit(value: Value, spec: &BudgetSpec) -> Result<Budgeted, AppError> {
    let mut budgeted = Budgeted {
        result: value,
        truncated: Vec::new(),
        items_omitted: 0,
    };
    if budgeted.serialized_chars() <= spec.budget_chars {
        return Ok(budgeted);
    }
    if let Some(key) = spec.droppable_list
        && budgeted
            .result
            .get(key)
            .is_some_and(|list| list.as_array().is_some_and(|a| a.len() > 1))
    {
        keep_newest(&mut budgeted, spec, key);
    }
    cut_evenly(&mut budgeted, spec)?;
    Ok(budgeted)
}

/// Keep whole elements of the droppable list newest-first while the envelope
/// fits; drop the older ones and record how many. The newest element is
/// always kept, and `cut_evenly` trims it if it alone is over.
fn keep_newest(budgeted: &mut Budgeted, spec: &BudgetSpec, key: &str) {
    let Some(items) = budgeted.result.get_mut(key).and_then(Value::as_array_mut) else {
        return;
    };
    let all: Vec<Value> = std::mem::take(items);
    let total = all.len();
    // The envelope with an empty list is the fixed cost; each element is
    // measured once.
    let base = measure(&budgeted.result) + 60;
    let mut used = base;
    let mut keep_from = total;
    for (index, item) in all.iter().enumerate().rev() {
        let cost = measure(item) + 1;
        if used + cost > spec.budget_chars && keep_from < total {
            break;
        }
        used += cost;
        keep_from = index;
    }
    // Always keep at least one element, even if it alone is over budget; the
    // text cut and the single-item error below handle it.
    let kept: Vec<Value> = all[keep_from..].to_vec();
    budgeted.items_omitted = keep_from;
    if let Some(items) = budgeted.result.get_mut(key).and_then(Value::as_array_mut) {
        *items = kept;
    }
}

/// Find the largest per-field cap at which the envelope fits and cut every
/// longer field to it. See the module doc for why the search walks intervals.
fn cut_evenly(budgeted: &mut Budgeted, spec: &BudgetSpec) -> Result<(), AppError> {
    let mut candidates = Vec::new();
    collect_candidates(&budgeted.result, "", &mut candidates);
    // A field that cannot be addressed is not cuttable: dropping it here is
    // what keeps the search from retrying the same field forever.
    candidates.retain(|c| budgeted.result.pointer(&c.path).is_some());
    let base = measure(budgeted);
    // Dropping old items may already have made it fit.
    if base <= spec.budget_chars {
        return Ok(());
    }
    let cap = SizeModel::new(&budgeted.result, &candidates, spec, base)
        .largest_fitting_cap(spec.budget_chars);
    let Some(mut cap) = cap else {
        return Err(AppError::ResultTooLarge {
            read_with: spec.read_with,
        });
    };
    let original = budgeted.result.clone();
    loop {
        apply_cap(budgeted, &original, &candidates, cap, spec);
        let size = budgeted.serialized_chars();
        if size <= spec.budget_chars {
            return Ok(());
        }
        // The model disagreed with the serializer. Step down by the overflow
        // rather than return an oversized result.
        if cap == 0 {
            return Err(AppError::ResultTooLarge {
                read_with: spec.read_with,
            });
        }
        cap = cap.saturating_sub((size - spec.budget_chars).max(1));
    }
}

/// The envelope's size at any cap, computed without serializing it. Sizes
/// are summed from the parts, never adjusted by subtraction: a cut structured
/// field (a tool input object) can serialize *longer* than the object it
/// replaced, because its quotes escape.
struct SizeModel<'a> {
    /// The envelope with every candidate field's value and every record
    /// removed.
    fixed: usize,
    /// Longest first, so the fields a cap cuts are a prefix.
    fields: Vec<FieldModel<'a>>,
    /// `uncut_after[i]`: what `fields[i..]` cost uncut.
    uncut_after: Vec<usize>,
    spec: &'a BudgetSpec,
    /// Distinct field lengths, longest first.
    lengths: Vec<usize>,
}

struct FieldModel<'a> {
    candidate: &'a Candidate,
    ancestors: Vec<&'a Map<String, Value>>,
    /// What the field costs in the uncut envelope.
    full_cost: usize,
    /// Positions (in characters) of the field's characters that escape to
    /// more than one, each with the running total of extra cost up to and
    /// including it, so the escaped length of any prefix is a binary search.
    escapes: Vec<(usize, usize)>,
    /// The field's record length, by the cap's digit count, as measured.
    records: Vec<(u32, usize)>,
    /// The shortest the field's record can be at any cap, once measured.
    min_record: Option<usize>,
}

impl FieldModel<'_> {
    /// The field's serialized cost when cut to `cap` characters: a string of
    /// that prefix, quotes included.
    fn cut_cost(&self, cap: usize) -> usize {
        let extra = match self.escapes.partition_point(|(pos, _)| *pos < cap) {
            0 => 0,
            n => self.escapes[n - 1].1,
        };
        2 + cap + extra
    }

    /// The field's truncation record at `cap`, serialized once per digit count.
    fn record_len(&mut self, cap: usize, spec: &BudgetSpec) -> usize {
        let digits = decimal_digits(cap);
        if let Some(&(_, len)) = self.records.iter().find(|(d, _)| *d == digits) {
            return len;
        }
        let len = measure(&Truncation {
            path: self.candidate.path.clone(),
            total_chars: self.candidate.chars,
            kept_chars: cap,
            read_with: (spec.pointer)(&CutSite {
                path: &self.candidate.path,
                key: &self.candidate.key,
                kept_chars: cap,
                ancestors: &self.ancestors,
            }),
        });
        self.records.push((digits, len));
        len
    }

    /// A lower bound on the field's record at any cap, whatever the pointer
    /// returns: one digit kept, and a one-character `read_with` (shorter than
    /// any pointer, and than `null`).
    fn min_record(&mut self) -> usize {
        *self.min_record.get_or_insert_with(|| {
            measure(&Truncation {
                path: self.candidate.path.clone(),
                total_chars: self.candidate.chars,
                kept_chars: 0,
                read_with: Some(Value::from(0)),
            })
        })
    }
}

impl<'a> SizeModel<'a> {
    fn new(
        root: &'a Value,
        candidates: &'a [Candidate],
        spec: &'a BudgetSpec,
        base: usize,
    ) -> Self {
        let mut fields: Vec<FieldModel<'a>> = candidates
            .iter()
            .map(|candidate| {
                let mut ancestors = Vec::new();
                collect_ancestors(root, &candidate.path, &mut ancestors);
                let mut escapes = Vec::new();
                let mut extra = 0;
                for (pos, c) in candidate.text.chars().enumerate() {
                    let cost = json_escaped_len(c);
                    if cost > 1 {
                        extra += cost - 1;
                        escapes.push((pos, extra));
                    }
                }
                // A string field costs its escaped text plus quotes; a
                // structured field (a tool input object) costs its JSON text.
                let full_cost = if candidate.structured {
                    candidate.chars
                } else {
                    candidate.chars + extra + 2
                };
                FieldModel {
                    candidate,
                    ancestors,
                    full_cost,
                    escapes,
                    records: Vec::new(),
                    min_record: None,
                }
            })
            .collect();
        fields.sort_by_key(|f| std::cmp::Reverse(f.candidate.chars));
        let mut uncut_after = vec![0; fields.len() + 1];
        for index in (0..fields.len()).rev() {
            uncut_after[index] = uncut_after[index + 1] + fields[index].full_cost;
        }
        // An empty field is never cut, so it bounds no interval.
        let mut lengths: Vec<usize> = fields
            .iter()
            .map(|f| f.candidate.chars)
            .filter(|chars| *chars > 0)
            .collect();
        lengths.dedup();
        Self {
            fixed: base.saturating_sub(uncut_after[0]),
            fields,
            uncut_after,
            spec,
            lengths,
        }
    }

    /// How many fields `cap` cuts: those longer than it, a prefix.
    fn cut_count(&self, cap: usize) -> usize {
        self.fields.partition_point(|f| f.candidate.chars > cap)
    }

    /// The envelope's size with every field longer than `cap` cut to it.
    fn size(&mut self, cap: usize) -> usize {
        let spec = self.spec;
        let cut = self.cut_count(cap);
        let mut size = self.fixed + self.uncut_after[cut];
        for field in &mut self.fields[..cut] {
            count_field_visit();
            size += field.cut_cost(cap) + field.record_len(cap, spec);
        }
        // Commas between records; the brackets are already in `fixed`.
        size + cut.saturating_sub(1)
    }

    /// The largest cap at which the envelope fits, walking the intervals in
    /// which the set of cut fields is fixed, from the largest caps down, and
    /// stopping once no smaller cap can fit.
    fn largest_fitting_cap(&mut self, budget: usize) -> Option<usize> {
        // A lower bound on the size at every cap below the current interval's
        // top: the fields cut there, emptied, each with its shortest record.
        let mut floor = self.fixed;
        let mut counted = 0;
        for index in 0..self.lengths.len() {
            let length = self.lengths[index];
            let high = length - 1;
            let low = self.lengths.get(index + 1).copied().unwrap_or(0);
            let cut = self.cut_count(high);
            for field in &mut self.fields[counted..cut] {
                count_field_visit();
                floor += 2 + field.min_record();
            }
            counted = cut;
            if floor + cut.saturating_sub(1) > budget {
                return None;
            }
            if self.size(low) > budget {
                continue;
            }
            // Within the interval, size never shrinks as the cap grows.
            let (mut lo, mut hi) = (low, high);
            while lo < hi {
                let mid = lo + (hi - lo).div_ceil(2);
                if self.size(mid) <= budget {
                    lo = mid;
                } else {
                    hi = mid - 1;
                }
            }
            return Some(lo);
        }
        None
    }
}

/// Cut every candidate longer than `cap` on a fresh copy of the original and
/// rebuild the truncation records.
fn apply_cap(
    budgeted: &mut Budgeted,
    original: &Value,
    candidates: &[Candidate],
    cap: usize,
    spec: &BudgetSpec,
) {
    budgeted.result = original.clone();
    budgeted.truncated.clear();
    for candidate in candidates {
        if candidate.chars <= cap {
            continue;
        }
        let mut ancestors: Vec<&Map<String, Value>> = Vec::new();
        collect_ancestors(original, &candidate.path, &mut ancestors);
        let read_with = (spec.pointer)(&CutSite {
            path: &candidate.path,
            key: &candidate.key,
            kept_chars: cap,
            ancestors: &ancestors,
        });
        let Some(slot) = budgeted.result.pointer_mut(&candidate.path) else {
            continue;
        };
        *slot = Value::String(candidate.text.chars().take(cap).collect());
        budgeted.truncated.push(Truncation {
            path: candidate.path.clone(),
            total_chars: candidate.chars,
            kept_chars: cap,
            read_with,
        });
    }
}

struct Candidate {
    /// JSON pointer.
    path: String,
    key: String,
    /// The field's text: the string itself, or a structured field's JSON.
    text: String,
    chars: usize,
    /// The field holds a non-string value (a tool input object), which is cut
    /// as its JSON text.
    structured: bool,
}

fn escape_pointer_token(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

fn collect_candidates(value: &Value, path: &str, out: &mut Vec<Candidate>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let child_path = format!("{path}/{}", escape_pointer_token(key));
                if CUTTABLE_KEYS.contains(&key.as_str()) {
                    match child {
                        Value::String(text) => out.push(Candidate {
                            path: child_path,
                            key: key.clone(),
                            chars: text.chars().count(),
                            text: text.clone(),
                            structured: false,
                        }),
                        Value::Null => {}
                        other => {
                            let text = serde_json::to_string(other).unwrap_or_default();
                            out.push(Candidate {
                                path: child_path,
                                key: key.clone(),
                                chars: text.chars().count(),
                                text,
                                structured: true,
                            });
                        }
                    }
                } else {
                    collect_candidates(child, &child_path, out);
                }
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                collect_candidates(child, &format!("{path}/{index}"), out);
            }
        }
        _ => {}
    }
}

/// The objects along a JSON pointer, root first, excluding the leaf.
fn collect_ancestors<'a>(root: &'a Value, path: &str, out: &mut Vec<&'a Map<String, Value>>) {
    let mut current = root;
    let tokens: Vec<&str> = path.split('/').skip(1).collect();
    for (i, token) in tokens.iter().enumerate() {
        let is_leaf = i + 1 == tokens.len();
        match current {
            Value::Object(map) => {
                out.push(map);
                if is_leaf {
                    return;
                }
                let key = token.replace("~1", "/").replace("~0", "~");
                let Some(next) = map.get(&key) else { return };
                current = next;
            }
            Value::Array(items) => {
                let Some(next) = token.parse::<usize>().ok().and_then(|i| items.get(i)) else {
                    return;
                };
                current = next;
            }
            _ => return,
        }
    }
}

#[cfg(test)]
pub(crate) fn serializations() -> usize {
    SERIALIZATIONS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn field_visits() -> usize {
    FIELD_VISITS.with(std::cell::Cell::get)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(budget: usize, droppable: Option<&'static str>) -> BudgetSpec {
        BudgetSpec::unpageable(budget, droppable, "read_send_output")
    }

    #[test]
    fn a_result_under_budget_is_untouched() {
        let value = json!({ "text": "short", "id": "abc" });
        let fitted = fit(value.clone(), &spec(10_000, None)).unwrap();
        assert_eq!(fitted.result, value);
        assert!(fitted.truncated.is_empty());
        assert_eq!(fitted.items_omitted, 0);
    }

    #[test]
    fn text_fields_share_the_budget_evenly_and_ids_survive() {
        let id = "0199c0a4-1b2c-7d3e-8f4a-5b6c7d8e9f01";
        let value = json!({
            "send_id": id,
            "recipients": [
                { "agent_id": id, "status": "completed", "text": "a".repeat(15_000), "error": "long error ".repeat(50) },
                { "agent_id": id, "status": "completed", "text": "b".repeat(15_000) },
                { "agent_id": id, "status": "completed", "text": "c".repeat(15_000) },
            ],
        });
        let budget = 25_000;
        let fitted = fit(value, &spec(budget, None)).unwrap();
        assert!(
            fitted.serialized_chars() <= budget,
            "{}",
            fitted.serialized_chars()
        );
        assert_eq!(fitted.result["send_id"], id);
        let kept: Vec<usize> = fitted.result["recipients"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["text"].as_str().unwrap().len())
            .collect();
        assert_eq!(kept[0], kept[1]);
        assert_eq!(kept[1], kept[2]);
        assert!(kept[0] > 5_000, "each reply keeps a real share: {kept:?}");
        for recipient in fitted.result["recipients"].as_array().unwrap() {
            assert_eq!(recipient["agent_id"], id);
            assert_eq!(recipient["status"], "completed");
        }
        assert_eq!(
            fitted.result["recipients"][0]["error"]
                .as_str()
                .unwrap()
                .len(),
            "long error ".repeat(50).len(),
            "an error message is never cut"
        );
        assert_eq!(fitted.truncated.len(), 3);
        assert_eq!(fitted.truncated[0].total_chars, 15_000);
        assert_eq!(fitted.truncated[0].kept_chars, kept[0]);
    }

    #[test]
    fn a_cut_field_carries_the_callers_pointer_with_the_kept_length() {
        let value = json!({ "recipients": [{ "agent_id": "a1", "text": "x".repeat(2_000) }] });
        let budget = BudgetSpec {
            budget_chars: 500,
            droppable_list: None,
            read_with: "read_send_output",
            pointer: Box::new(|site: &CutSite<'_>| {
                let agent = site.ancestors.last()?.get("agent_id")?.clone();
                Some(json!({
                    "tool": "read_send_output",
                    "agent_id": agent,
                    "key": site.key,
                    "offset": site.kept_chars,
                }))
            }),
        };
        let fitted = fit(value, &budget).unwrap();
        let cut = &fitted.truncated[0];
        assert_eq!(cut.path, "/recipients/0/text");
        let read_with = cut.read_with.as_ref().unwrap();
        assert_eq!(read_with["agent_id"], "a1");
        assert_eq!(read_with["key"], "text");
        assert_eq!(read_with["offset"], cut.kept_chars);
        assert_eq!(
            fitted.result["recipients"][0]["text"]
                .as_str()
                .unwrap()
                .len(),
            cut.kept_chars
        );
    }

    #[test]
    fn a_field_with_no_paging_tool_says_so() {
        let value = json!({ "items": [{ "kind": "tool", "output": "o".repeat(2_000) }] });
        let fitted = fit(value, &spec(500, None)).unwrap();
        assert_eq!(fitted.truncated[0].read_with, None);
    }

    #[test]
    fn structure_over_budget_keeps_the_newest_items_with_their_text() {
        let items: Vec<Value> = (0..50)
            .map(|i| json!({ "kind": "agent_turn", "agent_id": format!("agent-{i:02}"), "text": "t".repeat(100) }))
            .collect();
        let value = json!({ "items": items });
        let fitted = fit(value, &spec(1_200, Some("items"))).unwrap();
        assert!(fitted.serialized_chars() <= 1_200);
        assert!(fitted.items_omitted > 0);
        let remaining = fitted.result["items"].as_array().unwrap();
        assert_eq!(remaining.len() + fitted.items_omitted, 50);
        assert_eq!(remaining.last().unwrap()["agent_id"], "agent-49");
        for item in remaining {
            assert_eq!(
                item["text"].as_str().unwrap().len(),
                100,
                "kept items keep their text"
            );
        }
        assert!(fitted.truncated.is_empty());
    }

    #[test]
    fn a_single_item_that_cannot_fit_is_an_error() {
        let value = json!({ "items": [{ "id": "i".repeat(600) }] });
        let err = fit(value, &spec(500, Some("items"))).unwrap_err();
        assert!(matches!(
            err,
            AppError::ResultTooLarge {
                read_with: "read_send_output"
            }
        ));
        let value = json!({ "name": "n".repeat(600) });
        assert!(fit(value, &spec(500, None)).is_err());
    }

    #[test]
    fn a_structured_tool_input_is_cut_as_its_json_text() {
        let value =
            json!({ "items": [{ "input": { "content": "c".repeat(2_000) }, "name": "Write" }] });
        let fitted = fit(value, &spec(500, None)).unwrap();
        assert!(fitted.result["items"][0]["input"].is_string());
        assert_eq!(fitted.result["items"][0]["name"], "Write");
        assert_eq!(fitted.truncated[0].path, "/items/0/input");
    }

    #[test]
    fn keys_with_dots_slashes_and_brackets_terminate_and_are_cut() {
        let value = json!({ "a.b/c[0]": { "text": "x".repeat(2_000) }, "id": "keep" });
        let fitted = fit(value, &spec(300, None)).unwrap();
        assert!(fitted.serialized_chars() <= 300);
        assert_eq!(fitted.truncated[0].path, "/a.b~1c[0]/text");
        assert_eq!(fitted.result["id"], "keep");
    }

    #[test]
    fn cost_is_logarithmic_in_the_text_not_linear_in_the_fields() {
        let items: Vec<Value> = (0..5_000)
            .map(|i| json!({ "agent_id": format!("a{i}"), "text": "t".repeat(40) }))
            .collect();
        let value = json!({ "items": items });
        let before = serializations();
        let fitted = fit(value, &spec(20_000, Some("items"))).unwrap();
        assert!(fitted.serialized_chars() <= 20_000);
        let used = serializations() - before;
        // One envelope measure, one per element the newest-first fill visits,
        // and a few more — never one per element of the input.
        let kept = fitted.result["items"].as_array().unwrap().len();
        assert!(
            used <= kept + 6,
            "{used} serializations for {kept} kept items"
        );
    }
}

#[cfg(test)]
mod round_three {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_short_and_a_long_field_fit_when_a_cap_exists() {
        // Cutting the short field too adds a second record, which costs more
        // than the characters it removes: the fitting caps form a window, and
        // the search must find it.
        let value = json!({ "body": "b".repeat(20), "text": "t".repeat(1_000) });
        let fitted = fit(
            value,
            &BudgetSpec::unpageable(180, None, "read_send_output"),
        )
        .expect("caps 20 through 27 fit");
        assert!(fitted.serialized_chars() <= 180);
        assert_eq!(fitted.truncated.len(), 1, "only the long field is cut");
        assert_eq!(
            fitted.truncated[0].kept_chars, 27,
            "the largest cap that fits"
        );
    }
}

#[cfg(test)]
mod size_model {
    use super::*;
    use serde_json::json;

    #[test]
    fn one_large_item_with_many_fields_serializes_each_record_a_few_times() {
        // One agent turn with 100 tool outputs of different lengths: dropping
        // items cannot shrink it, so every cut is decided inside the item.
        // (Hundreds more would not fit at all — each cut field's own record
        // costs about 80 characters — which is the single-item error.)
        let items: Vec<Value> = (0..100)
            .map(|i| json!({ "item_kind": "tool", "output": "o".repeat(1_000 + i * 7) }))
            .collect();
        let value = json!({ "items": [{ "kind": "agent_turn", "items": items }] });
        let before = serializations();
        let fitted = fit(value, &BudgetSpec::unpageable(25_000, Some("items"), "x")).unwrap();
        assert!(fitted.serialized_chars() <= 25_000);
        let used = serializations() - before;
        // A handful of whole-result serializations; per field, its shortest
        // record and one record per digit count of the caps probed (at most
        // four here). Never a whole-result serialization per probe.
        assert!(
            used <= 6 + 100 * 5,
            "{used} serializations: the model, not the serializer, searches"
        );
    }

    #[test]
    fn records_that_alone_overflow_stop_the_search_early() {
        // 2,000 fields of distinct lengths whose structure fits on its own, so
        // the walk runs: every cap either leaves megabytes uncut or cuts so
        // many fields that their records alone overflow.
        let budget = 25_000;
        let recipients = |text: &dyn Fn(usize) -> String| -> Value {
            let list: Vec<Value> = (0..2_000).map(|i| json!({ "text": text(i) })).collect();
            json!({ "recipients": list })
        };
        let structure = serde_json::to_string(&Budgeted {
            result: recipients(&|_| String::new()),
            truncated: Vec::new(),
            items_omitted: 0,
        })
        .unwrap();
        assert!(structure.len() < budget, "the structure alone fits");
        let value = recipients(&|i| "o".repeat(1_000 + i));
        let (serialized_before, visits_before) = (serializations(), field_visits());
        let err = fit(value, &BudgetSpec::unpageable(budget, None, "x")).unwrap_err();
        assert!(matches!(err, AppError::ResultTooLarge { read_with: "x" }));
        let serialized = serializations() - serialized_before;
        let visits = field_visits() - visits_before;
        // Every cut field costs at least 60 characters of record, so the walk
        // examines at most `budget / 60` fields before it stops, and each
        // probe visits only the fields it cuts: bounded by the budget, not by
        // the 2,000 fields (whose square is 4,000,000).
        let examined = budget / 60;
        assert!(
            visits <= examined * examined,
            "{visits} field visits for at most {examined} examined fields"
        );
        assert!(
            serialized <= 4 + examined * 3,
            "{serialized} serializations for at most {examined} examined fields"
        );
    }

    #[test]
    fn an_empty_text_field_does_not_break_the_search() {
        let value = json!({ "items": [
            { "agent_id": "a", "text": "" },
            { "agent_id": "b", "text": "x".repeat(2_000) },
        ] });
        let fitted = fit(value, &BudgetSpec::unpageable(300, None, "x")).unwrap();
        assert!(fitted.serialized_chars() <= 300);
        assert_eq!(fitted.truncated.len(), 1);
        assert_eq!(fitted.result["items"][0]["text"], "");
    }

    #[test]
    fn the_model_agrees_with_the_serializer_on_escaped_text() {
        // Quotes, backslashes, newlines, and other control characters all
        // escape to more than one character; the chosen cap must still fit on
        // the first real serialization.
        let tricky: String = "a\"b\\c\nd\u{1}e\u{8}f\u{c}g".repeat(400);
        let value = json!({ "recipients": [
            { "agent_id": "x", "text": tricky.clone() },
            { "agent_id": "y", "text": "plain".repeat(800) },
        ] });
        let fitted = fit(value, &BudgetSpec::unpageable(3_000, None, "x")).unwrap();
        assert!(fitted.serialized_chars() <= 3_000);
        let kept = fitted.truncated[0].kept_chars;
        assert!(kept > 300, "a real share survives the escapes: {kept}");
    }
}
