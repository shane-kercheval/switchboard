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
//! the largest per-field cap at which the whole envelope fits (binary search,
//! one serialization per step) and cuts every field to that cap. Cutting the
//! longest field by the whole overflow would empty one recipient's reply and
//! leave the others whole.
//!
//! **When cutting is not enough.** A result with a droppable list — a
//! conversation's items — keeps whole items newest-first while they fit, cuts
//! the text of the first that does not, and omits the rest, reporting how
//! many. A fan-out's recipients are never droppable. A single element that
//! cannot fit even with every text field emptied is an error naming the call
//! that reads it in pages.
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
//! The envelope wraps the result rather than annotating it in place, so the
//! shapes the desktop renders (`ConversationItem`) cross unchanged.

use std::sync::atomic::{AtomicUsize, Ordering};

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
    /// can.
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

/// Serializations performed, for tests that bound the helper's cost without
/// a clock.
static SERIALIZATIONS: AtomicUsize = AtomicUsize::new(0);

fn measure<T: Serialize>(value: &T) -> usize {
    SERIALIZATIONS.fetch_add(1, Ordering::Relaxed);
    serde_json::to_string(value).map_or(0, |s| s.chars().count())
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
/// fits; drop the rest and record how many. The oldest kept element is the
/// one `cut_evenly` will trim if the whole set is still over.
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
/// cuttable field to it.
fn cut_evenly(budgeted: &mut Budgeted, spec: &BudgetSpec) -> Result<(), AppError> {
    let mut candidates = Vec::new();
    collect_candidates(&budgeted.result, "", &mut candidates);
    // Fields that cannot be addressed are not cuttable: skipping them is what
    // keeps every pass from retrying the same field forever.
    candidates.retain(|c| budgeted.result.pointer(&c.path).is_some());
    if candidates.is_empty() {
        return if budgeted.serialized_chars() <= spec.budget_chars {
            Ok(())
        } else {
            Err(AppError::ResultTooLarge {
                read_with: spec.read_with,
            })
        };
    }
    let longest = candidates.iter().map(|c| c.chars).max().unwrap_or(0);
    let original = budgeted.result.clone();
    // Binary search the cap: `fits(cap)` is monotone in `cap`.
    let (mut low, mut high) = (0usize, longest);
    if !apply_cap(budgeted, &original, &candidates, high, spec) {
        while low < high {
            let mid = low + (high - low).div_ceil(2);
            if apply_cap(budgeted, &original, &candidates, mid, spec) {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        if !apply_cap(budgeted, &original, &candidates, low, spec) {
            return Err(AppError::ResultTooLarge {
                read_with: spec.read_with,
            });
        }
    }
    Ok(())
}

/// Cut every candidate to `cap` characters on a fresh copy of the original,
/// rebuild the truncation records, and report whether the envelope fits.
fn apply_cap(
    budgeted: &mut Budgeted,
    original: &Value,
    candidates: &[Candidate],
    cap: usize,
    spec: &BudgetSpec,
) -> bool {
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
        let full = if candidate.structured {
            serde_json::to_string(slot).unwrap_or_default()
        } else {
            slot.as_str().unwrap_or_default().to_owned()
        };
        *slot = Value::String(full.chars().take(cap).collect());
        budgeted.truncated.push(Truncation {
            path: candidate.path.clone(),
            total_chars: candidate.chars,
            kept_chars: cap,
            read_with,
        });
    }
    budgeted.serialized_chars() <= spec.budget_chars
}

struct Candidate {
    /// JSON pointer.
    path: String,
    key: String,
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
                            structured: false,
                        }),
                        Value::Null => {}
                        other => out.push(Candidate {
                            path: child_path,
                            key: key.clone(),
                            chars: serde_json::to_string(other).map_or(0, |s| s.chars().count()),
                            structured: true,
                        }),
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
    SERIALIZATIONS.load(Ordering::Relaxed)
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
        // One envelope measure, one per element for the newest-first fill,
        // and a logarithmic number of cap probes — never one per cut field.
        assert!(used < 5_000 + 40, "{used} serializations");
    }
}
