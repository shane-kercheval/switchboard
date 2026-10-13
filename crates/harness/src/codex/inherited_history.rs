//! A Codex fork's inherited history.
//!
//! A Codex fork's rollout does not copy its parent's conversation. Its
//! `session_meta.history_base` points into the parent's rollout instead:
//! `{thread_id, end_byte_offset}`, where `thread_id` is the parent's **rollout**
//! id and `end_byte_offset` is how much of the parent's own file the fork
//! inherited (`docs/harness-behavior.md` §3.5, probes 4 and 5). Codex re-reads
//! that prefix on every fork turn. Without following the same pointer, a fork's
//! pane would show only the turns it ran itself.
//!
//! **Each file is rebuilt on its own, then the turns are joined**, oldest
//! ancestor first. Records are never spliced into one stream, because each file
//! declares its own `history_mode` and the reconstruction picks its ingest path
//! from it. A parent that is itself a fork carries its own `history_base`, so
//! the walk repeats up the chain.
//!
//! **All or nothing.** If any link of the chain can't be read exactly as Codex
//! recorded it, none of the inherited turns are shown, and the fork gets a
//! [`SystemMarker::InheritedHistoryUnavailable`] instead. A partial history, or
//! a guess at a file that has changed since the fork, would read as the real
//! conversation.
//!
//! **Only turns are inherited.** The agent's current state (model, usage,
//! context window, settings, environment) comes from the fork's own file, which
//! `parse_codex_transcript_content` already reads; nothing here touches it.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;
use switchboard_core::AgentId;

use super::session_file::parse_codex_transcript_content;
use crate::transcript::{InheritedHistoryGap, ParseWarning, Turn};

/// How many ancestors the walk follows before giving up. Real chains are a
/// handful deep; the cap only bounds a pathological or corrupted one.
pub(crate) const MAX_FORK_DEPTH: usize = 32;

/// What a rollout inherits, as the loader needs it.
pub(crate) enum InheritedHistory {
    /// The rollout is not a fork: its `session_meta` has no `history_base`.
    NotAFork,
    /// The whole chain loaded. `turns` run oldest ancestor first.
    Loaded {
        turns: Vec<Turn>,
        warnings: Vec<ParseWarning>,
    },
    /// The chain broke at `rollout_id`. `created_at` is the fork's own
    /// `session_meta` timestamp, where the marker belongs.
    Unavailable {
        reason: InheritedHistoryGap,
        rollout_id: String,
        created_at: Option<DateTime<Utc>>,
    },
}

/// A `history_base` pointer.
struct Pointer {
    rollout_id: String,
    end_byte_offset: u64,
}

/// Follow `own_content`'s `history_base` chain and rebuild the turns it
/// inherits.
///
/// `own_path` is the fork's own rollout, used only to recognise a chain that
/// loops back to it.
pub(crate) fn load_inherited_history(
    home_dir: &Path,
    own_path: &Path,
    own_content: &str,
    agent_id: AgentId,
) -> InheritedHistory {
    // The fork's own header only has to be a session header: a pointer in it
    // is reason enough to follow. The stricter `readable_header` rule is for
    // files reached through a pointer, where "no pointer" must be something
    // the header says.
    let Some(own_meta) = session_meta_record(own_content) else {
        return InheritedHistory::NotAFork;
    };
    let created_at = meta_timestamp(&own_meta);
    let mut pointer = match pointer_of(&own_meta) {
        PointerField::Absent => return InheritedHistory::NotAFork,
        PointerField::Unreadable(rollout_id) => {
            return InheritedHistory::Unavailable {
                reason: InheritedHistoryGap::Changed,
                rollout_id,
                created_at,
            };
        }
        PointerField::Present(pointer) => pointer,
    };

    let mut visited: HashSet<String> = own_path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(rollout_file_id)
        .map(|(id, _)| HashSet::from([id.to_owned()]))
        .unwrap_or_default();
    let mut segments: Vec<(String, crate::transcript::LoadedTranscript)> = Vec::new();
    // Built on the first lookup, so only a fork pays for the walk, and only
    // once however long its chain is.
    let index = RolloutIndex::build(home_dir);
    loop {
        let fail = |reason, rollout_id: &str| InheritedHistory::Unavailable {
            reason,
            rollout_id: rollout_id.to_owned(),
            created_at,
        };
        if segments.len() == MAX_FORK_DEPTH {
            return fail(InheritedHistoryGap::TooDeep, &pointer.rollout_id);
        }
        if !visited.insert(pointer.rollout_id.clone()) {
            return fail(InheritedHistoryGap::Cycle, &pointer.rollout_id);
        }
        let path = match index.find(&pointer.rollout_id) {
            Ok(path) => path,
            Err(reason) => return fail(reason, &pointer.rollout_id),
        };
        let prefix = match read_prefix(&path, pointer.end_byte_offset) {
            Ok(prefix) => prefix,
            Err(reason) => return fail(reason, &pointer.rollout_id),
        };
        // A file reached through a pointer must open with a readable header.
        // Without one, nothing says whether it continues the chain, and
        // treating it as the chain's start would show part of the history as
        // all of it.
        let Some(meta) = readable_header(&prefix) else {
            return fail(InheritedHistoryGap::Changed, &pointer.rollout_id);
        };
        let next = match pointer_of(&meta) {
            PointerField::Absent => None,
            PointerField::Unreadable(rollout_id) => {
                return fail(InheritedHistoryGap::Changed, &rollout_id);
            }
            PointerField::Present(next) => Some(next),
        };
        segments.push((
            pointer.rollout_id,
            parse_codex_transcript_content(&prefix, agent_id),
        ));
        match next {
            Some(next) => pointer = next,
            None => break,
        }
    }

    let mut turns = Vec::new();
    let mut warnings = Vec::new();
    for (rollout_id, segment) in segments.into_iter().rev() {
        turns.extend(segment.turns);
        warnings.extend(segment.warnings.into_iter().map(|warning| ParseWarning {
            line_number: warning.line_number,
            reason: format!("in inherited history ({rollout_id}): {}", warning.reason),
        }));
    }
    InheritedHistory::Loaded { turns, warnings }
}

/// The content's first record, if it is a `session_meta`. Codex writes one
/// first in every rollout.
fn session_meta_record(content: &str) -> Option<Value> {
    let first = content.lines().find(|line| !line.trim().is_empty())?;
    let record: Value = serde_json::from_str(first).ok()?;
    (record.get("type").and_then(Value::as_str) == Some("session_meta")).then_some(record)
}

/// The content's first record, if it is a `session_meta` whose payload names
/// its session id. Every Codex rollout's header does, legacy ones included
/// (`SessionMeta.id` is required). Used for a file reached through a pointer:
/// there, a header with no pointer ends the chain, so it has to be a header
/// that says so, not one that parses and says nothing
/// (`{"type":"session_meta","payload":null}`).
fn readable_header(content: &str) -> Option<Value> {
    session_meta_record(content)
        .filter(|record| record.pointer("/payload/id").is_some_and(Value::is_string))
}

/// When the session was created: the payload's own timestamp, else the
/// record's.
fn meta_timestamp(meta: &Value) -> Option<DateTime<Utc>> {
    [meta.pointer("/payload/timestamp"), meta.get("timestamp")]
        .into_iter()
        .flatten()
        .find_map(|value| value.as_str()?.parse().ok())
}

enum PointerField {
    Absent,
    /// A `history_base` that is there but can't be read. Carries whatever id
    /// it names, so the marker can say which pointer was broken. Treated as a
    /// broken chain rather than "not a fork": silently dropping the inherited
    /// history on a format change is the failure this module exists to avoid.
    Unreadable(String),
    Present(Pointer),
}

fn pointer_of(meta: &Value) -> PointerField {
    let Some(base) = meta
        .pointer("/payload/history_base")
        .filter(|base| !base.is_null())
    else {
        return PointerField::Absent;
    };
    let rollout_id = base.get("thread_id").and_then(Value::as_str);
    let end_byte_offset = base.get("end_byte_offset").and_then(Value::as_u64);
    match (rollout_id, end_byte_offset) {
        (Some(rollout_id), Some(end_byte_offset)) => PointerField::Present(Pointer {
            rollout_id: rollout_id.to_owned(),
            end_byte_offset,
        }),
        (rollout_id, _) => PointerField::Unreadable(rollout_id.unwrap_or_default().to_owned()),
    }
}

/// The rollout id in a Codex rollout filename, and whether the file is
/// compressed.
///
/// Codex writes `rollout-<YYYY-MM-DDTHH-MM-SS>-<id>.jsonl`, where the one id is
/// both thread id and rollout id. A thread rolled back with Codex's "revert"
/// gets `rollout-<time>-<thread-id>_<rollout-id>.jsonl`, and a compressed file
/// adds `.zst` (`codex-rs/rollout/src/rollout_file_name.rs` and
/// `compression.rs` at `rust-v0.162.1`). A `history_base` names the rollout id,
/// which in the two-id form is the part after `_`, so the `-<id>.jsonl` suffix
/// match the rest of this crate uses would miss it.
fn rollout_file_id(name: &str) -> Option<(&str, bool)> {
    let (core, compressed) = match name.strip_suffix(".jsonl.zst") {
        Some(core) => (core, true),
        None => (name.strip_suffix(".jsonl")?, false),
    };
    let core = core.strip_prefix("rollout-")?;
    // The timestamp is 19 characters, then a `-`, then the ids.
    if core.as_bytes().get(19) != Some(&b'-') {
        return None;
    }
    let ids = core.get(20..).filter(|ids| !ids.is_empty())?;
    let rollout_id = ids
        .split_once('_')
        .map_or(ids, |(_, rollout_id)| rollout_id);
    Some((rollout_id, compressed))
}

/// Where Codex keeps rollouts, in the order it searches them when following a
/// `history_base`: its `find_rollout_path_by_rollout_id`
/// (`codex-rs/rollout/src/list.rs` at `rust-v0.162.1`) walks `sessions`, then
/// `archived_sessions`, and takes the first match. Archiving a session in Codex
/// moves its file into the second, where it is stored flat rather than in date
/// folders, and a fork of it keeps working.
const ROLLOUT_TIERS: [&str; 2] = ["sessions", "archived_sessions"];

/// The copies of one rollout within one tier.
#[derive(Default)]
struct Copies {
    plain: Vec<PathBuf>,
    compressed: bool,
}

/// Every rollout file under Codex's session folders, by tier and rollout id.
struct RolloutIndex {
    tiers: Vec<HashMap<String, Copies>>,
}

impl RolloutIndex {
    /// Walk each tier recursively, as Codex does, so the archive's flat layout
    /// and the dated `sessions` layout are read the same way. A folder that
    /// can't be read is skipped rather than failing the whole lookup.
    fn build(home_dir: &Path) -> Self {
        let codex = home_dir.join(".codex");
        let tiers = ROLLOUT_TIERS
            .iter()
            .map(|tier| {
                let mut by_id: HashMap<String, Copies> = HashMap::new();
                let mut folders = vec![codex.join(tier)];
                while let Some(folder) = folders.pop() {
                    let Ok(entries) = std::fs::read_dir(&folder) else {
                        continue;
                    };
                    for entry in entries.flatten() {
                        let Ok(file_type) = entry.file_type() else {
                            continue;
                        };
                        let path = entry.path();
                        if file_type.is_dir() {
                            folders.push(path);
                            continue;
                        }
                        // Regular files only, as Codex: a symlink, valid or
                        // broken, is not a rollout it would read, so one named
                        // like the original must not hide an archived copy.
                        if !file_type.is_file() {
                            continue;
                        }
                        let Some((id, compressed)) = path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .and_then(rollout_file_id)
                        else {
                            continue;
                        };
                        let copies = by_id.entry(id.to_owned()).or_default();
                        if compressed {
                            copies.compressed = true;
                        } else {
                            copies.plain.push(path);
                        }
                    }
                }
                by_id
            })
            .collect();
        Self { tiers }
    }

    /// The rollout carrying `rollout_id`, from the first tier that has one.
    ///
    /// Within that tier, more than one plain copy is refused, as the attach
    /// lookup refuses it, because two copies could hold different
    /// conversations. A copy in an earlier tier simply wins over a later one:
    /// that is Codex's own order, so it is not a duplicate. A tier holding
    /// only a compressed copy decides the answer too, as it would for Codex.
    fn find(&self, rollout_id: &str) -> Result<PathBuf, InheritedHistoryGap> {
        for tier in &self.tiers {
            let Some(copies) = tier.get(rollout_id) else {
                continue;
            };
            return match copies.plain.as_slice() {
                [only] => Ok(only.clone()),
                [] => Err(InheritedHistoryGap::Compressed),
                _ => Err(InheritedHistoryGap::Ambiguous),
            };
        }
        Err(InheritedHistoryGap::NotFound)
    }
}

/// Exactly the first `end_byte_offset` bytes of `path`, as Codex recorded them.
///
/// Codex treats the prefix as immutable, so a file shorter than the offset, or
/// an offset that no longer ends on a line break, means the file is not the one
/// Codex forked. Nothing is rendered from it.
fn read_prefix(path: &Path, end_byte_offset: u64) -> Result<String, InheritedHistoryGap> {
    let file = std::fs::File::open(path).map_err(|_| InheritedHistoryGap::NotFound)?;
    let mut bytes = Vec::new();
    file.take(end_byte_offset)
        .read_to_end(&mut bytes)
        .map_err(|_| InheritedHistoryGap::Changed)?;
    if u64::try_from(bytes.len()).ok() != Some(end_byte_offset) || bytes.last() != Some(&b'\n') {
        return Err(InheritedHistoryGap::Changed);
    }
    String::from_utf8(bytes).map_err(|_| InheritedHistoryGap::Changed)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    use chrono::NaiveDate;

    use super::*;
    use crate::codex::session_file::load_codex_transcript;
    use crate::transcript::{LoadedTranscript, SystemMarker};

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/codex");
    // The real probe chain of 2026-10-11 (harness-behavior §3.5), trimmed to
    // the records the loader reads: a parent told PELICAN-42 and then, after
    // the fork, OTTER-7; a fork of it with two turns of its own; and a fork of
    // that fork with one.
    const PARENT: &str = "01a12c22-a3f8-71e2-b51e-ca72562cfabb";
    const CHILD: &str = "01a12c22-df64-7103-9fc3-92f2e3595c02";
    const GRANDCHILD: &str = "01a12c82-0ff8-7410-9426-7cf26aa1711a";
    const PELICAN: &str = "Remember this code word: PELICAN-42. Reply with only: ok";
    const OTTER: &str = "Also remember a second code word: OTTER-7. Reply with only: ok";
    const CHILD_FIRST: &str =
        "What code word did I ask you to remember? Reply with only the code word.";
    const GRANDCHILD_ONLY: &str = "List every code word I asked you to remember, comma-separated.";
    /// The child's own `session_meta` timestamp: when the fork was created.
    const CHILD_CREATED: &str = "2026-10-11T18:04:05.348Z";

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!("{FIXTURES}/{name}.session.jsonl")).unwrap()
    }

    fn date(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, day).unwrap()
    }

    /// A temporary home with a `.codex/sessions` tree to stage rollouts in.
    struct Home {
        dir: tempfile::TempDir,
        agent_id: AgentId,
    }

    impl Home {
        fn new() -> Self {
            Self {
                dir: tempfile::TempDir::new().unwrap(),
                agent_id: uuid::Uuid::now_v7(),
            }
        }

        /// Write `content` as `rollout-<time>-<ids><extension>` in the
        /// 2026-10-`day` folder. `ids` is one id, or `<thread>_<rollout>`.
        fn stage(&self, day: u32, ids: &str, extension: &str, content: &str) -> PathBuf {
            let folder = self
                .dir
                .path()
                .join(format!(".codex/sessions/2026/10/{day:02}"));
            std::fs::create_dir_all(&folder).unwrap();
            let path = folder.join(format!(
                "rollout-2026-10-{day:02}T00-00-00-{ids}{extension}"
            ));
            std::fs::write(&path, content).unwrap();
            path
        }

        fn rollout(&self, day: u32, id: &str, content: &str) {
            self.stage(day, id, ".jsonl", content);
        }

        /// Write `content` into `~/.codex/archived_sessions`, which Codex keeps
        /// flat rather than in date folders.
        fn archive(&self, ids: &str, extension: &str, content: &str) {
            let folder = self.dir.path().join(".codex/archived_sessions");
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(
                folder.join(format!("rollout-2026-10-11T00-00-00-{ids}{extension}")),
                content,
            )
            .unwrap();
        }

        fn load(&self, own_id: &str, day: u32) -> LoadedTranscript {
            load_codex_transcript(
                self.dir.path(),
                self.dir.path(),
                own_id,
                Some(date(day)),
                self.agent_id,
            )
            .unwrap()
        }
    }

    fn user_texts(transcript: &LoadedTranscript) -> Vec<&str> {
        transcript
            .turns
            .iter()
            .filter_map(|turn| match turn {
                Turn::User { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    fn markers(transcript: &LoadedTranscript) -> Vec<(InheritedHistoryGap, &str)> {
        transcript
            .turns
            .iter()
            .filter_map(|turn| match turn {
                Turn::System {
                    marker: SystemMarker::InheritedHistoryUnavailable { reason, rollout_id },
                    ..
                } => Some((*reason, rollout_id.as_str())),
                _ => None,
            })
            .collect()
    }

    /// A one-line rollout holding only a `session_meta`, optionally pointing
    /// at `(rollout id, end_byte_offset)`.
    fn meta_line(id: &str, base: Option<(&str, usize)>) -> String {
        let history_base = base.map_or(Value::Null, |(rollout_id, offset)| {
            serde_json::json!({"thread_id": rollout_id, "end_ordinal_exclusive": 1, "end_byte_offset": offset})
        });
        let record = serde_json::json!({
            "timestamp": "2026-10-11T00:00:00.000Z",
            "type": "session_meta",
            "payload": {"id": id, "timestamp": "2026-10-11T00:00:00.000Z", "history_mode": "paginated", "history_base": history_base},
        });
        format!("{record}\n")
    }

    #[test]
    fn a_fork_shows_its_parents_turns_up_to_the_fork_point_then_its_own() {
        let home = Home::new();
        home.rollout(11, PARENT, &fixture("fork-parent"));
        home.rollout(11, CHILD, &fixture("fork-child"));

        let transcript = home.load(CHILD, 11);

        assert_eq!(
            user_texts(&transcript)[..2],
            [PELICAN, CHILD_FIRST],
            "the inherited prompt comes first, then the fork's own"
        );
        assert_eq!(user_texts(&transcript).len(), 3);
        assert!(
            !user_texts(&transcript).contains(&OTTER),
            "the parent's turn after the fork point must not reach the fork"
        );
        assert!(markers(&transcript).is_empty());
        assert!(transcript.turns.iter().all(|turn| match turn {
            Turn::User { agent_id, .. } | Turn::Agent { agent_id, .. } =>
                *agent_id == home.agent_id,
            _ => true,
        }));
    }

    #[test]
    fn a_fork_of_a_fork_shows_the_whole_chain_oldest_first() {
        // The parent is in another day's folder: the lookup finds a rollout
        // wherever Codex filed it, not only beside the fork.
        let home = Home::new();
        home.rollout(10, PARENT, &fixture("fork-parent"));
        home.rollout(11, CHILD, &fixture("fork-child"));
        home.rollout(11, GRANDCHILD, &fixture("fork-grandchild"));

        let transcript = home.load(GRANDCHILD, 11);

        let texts = user_texts(&transcript);
        assert_eq!(texts.len(), 4, "{texts:?}");
        assert_eq!(texts[0], PELICAN);
        assert_eq!(texts[1], CHILD_FIRST);
        assert_eq!(texts[3], GRANDCHILD_ONLY);
        assert!(markers(&transcript).is_empty());
    }

    #[test]
    fn a_missing_parent_shows_the_forks_own_turns_and_a_marker_at_its_creation() {
        let home = Home::new();
        home.rollout(11, CHILD, &fixture("fork-child"));

        let transcript = home.load(CHILD, 11);

        assert_eq!(user_texts(&transcript)[0], CHILD_FIRST);
        assert_eq!(
            markers(&transcript),
            [(InheritedHistoryGap::NotFound, PARENT)]
        );
        let Some(Turn::System { started_at, .. }) = transcript.turns.first() else {
            panic!("the marker must lead the fork's turns");
        };
        assert_eq!(*started_at, CHILD_CREATED.parse::<DateTime<Utc>>().unwrap());
    }

    #[test]
    fn a_break_anywhere_in_the_chain_shows_no_inherited_turns_at_all() {
        // The parent is present, the grandparent is not: showing only the
        // parent's part would read as the whole history.
        let home = Home::new();
        home.rollout(11, CHILD, &fixture("fork-child"));
        home.rollout(11, GRANDCHILD, &fixture("fork-grandchild"));

        let transcript = home.load(GRANDCHILD, 11);

        assert_eq!(user_texts(&transcript), [GRANDCHILD_ONLY]);
        assert_eq!(
            markers(&transcript),
            [(InheritedHistoryGap::NotFound, PARENT)]
        );
    }

    #[test]
    fn a_compressed_parent_is_told_apart_from_a_missing_one() {
        let home = Home::new();
        home.stage(11, PARENT, ".jsonl.zst", "not really zstd");
        home.rollout(11, CHILD, &fixture("fork-child"));

        let transcript = home.load(CHILD, 11);

        assert_eq!(
            markers(&transcript),
            [(InheritedHistoryGap::Compressed, PARENT)]
        );
        assert_eq!(user_texts(&transcript)[0], CHILD_FIRST);
    }

    #[test]
    fn a_parent_shorter_than_the_recorded_cut_off_is_not_guessed_at() {
        let home = Home::new();
        let parent = fixture("fork-parent");
        home.rollout(11, PARENT, &parent[..3000]);
        home.rollout(11, CHILD, &fixture("fork-child"));

        let transcript = home.load(CHILD, 11);

        assert_eq!(
            markers(&transcript),
            [(InheritedHistoryGap::Changed, PARENT)]
        );
        assert!(!user_texts(&transcript).contains(&PELICAN));
    }

    #[test]
    fn a_cut_off_that_splits_a_line_is_not_guessed_at() {
        let home = Home::new();
        home.rollout(11, PARENT, &fixture("fork-parent"));
        let child = fixture("fork-child").replacen(
            r#""end_byte_offset":3499"#,
            r#""end_byte_offset":3400"#,
            1,
        );
        assert_ne!(child, fixture("fork-child"), "the fixture's cut-off moved");
        home.rollout(11, CHILD, &child);

        let transcript = home.load(CHILD, 11);

        assert_eq!(
            markers(&transcript),
            [(InheritedHistoryGap::Changed, PARENT)]
        );
    }

    #[test]
    fn a_parent_in_the_reverted_filename_form_is_found_by_its_rollout_id() {
        // Codex names a rolled-back thread's file `<thread-id>_<rollout-id>`,
        // and its `session_meta.id` stays the thread id. The fork's pointer
        // names the rollout id.
        const ROLLOUT: &str = "01a12c22-ffff-7000-8000-00000000beef";
        let home = Home::new();
        home.stage(
            11,
            &format!("{PARENT}_{ROLLOUT}"),
            ".jsonl",
            &fixture("fork-parent"),
        );
        home.rollout(11, CHILD, &fixture("fork-child").replace(PARENT, ROLLOUT));

        let transcript = home.load(CHILD, 11);

        assert!(
            markers(&transcript).is_empty(),
            "{:?}",
            markers(&transcript)
        );
        assert_eq!(user_texts(&transcript)[0], PELICAN);
    }

    #[test]
    fn a_legacy_mode_ancestor_is_read_through_the_legacy_path() {
        // The legacy fixture's turn lives in `event_msg/user_message` records,
        // which the paginated path ignores, so splicing its records into the
        // paginated fork's stream would lose it.
        let home = Home::new();
        let parent = fixture("legacy-explicit-mode");
        home.rollout(11, PARENT, &parent);
        home.rollout(11, CHILD, &meta_line(CHILD, Some((PARENT, parent.len()))));

        let transcript = home.load(CHILD, 11);

        assert_eq!(user_texts(&transcript), ["say ack"]);
    }

    #[test]
    fn a_promptless_fork_shows_only_inherited_turns_and_keeps_its_own_state() {
        // A fork created with no prompt holds just its `session_meta`. Its
        // pane shows the inherited conversation, but the agent's state (here
        // the model) comes from its own file, which names none yet.
        let home = Home::new();
        home.rollout(11, PARENT, &fixture("fork-parent"));
        let own_meta = fixture("fork-child").lines().next().unwrap().to_owned() + "\n";
        home.rollout(11, CHILD, &own_meta);

        let transcript = home.load(CHILD, 11);

        assert_eq!(user_texts(&transcript), [PELICAN]);
        assert_eq!(
            transcript.meta.as_ref().map(|meta| meta.model.as_str()),
            Some(""),
            "the parent's gpt-6-luna must not become the fork's model"
        );
    }

    #[test]
    fn a_fork_of_a_compacted_conversation_shows_the_originals_marker() {
        // The original holds a reply, a compaction run as its own turn, a
        // cancelled compaction and a second reply. Its segment goes through the
        // same reader as any rollout, so the fork shows the marker in place and
        // no empty reply for either compaction.
        let home = Home::new();
        let parent = fixture("paginated-compaction");
        home.rollout(11, PARENT, &parent);
        let own_turn = [
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"fork-1"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"UserMessage","id":"fu1","content":[{"type":"text","text":"And now?","text_elements":[]}]}}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
                "type":"AgentMessage","id":"fa1","content":[{"type":"Text","text":"HERON"}]}}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete"}}),
        ]
        .iter()
        .map(|record| record.to_string() + "\n")
        .collect::<String>();
        home.rollout(
            11,
            CHILD,
            &(meta_line(CHILD, Some((PARENT, parent.len()))) + &own_turn),
        );

        let transcript = home.load(CHILD, 11);

        let shapes: Vec<String> = transcript
            .turns
            .iter()
            .map(|turn| match turn {
                Turn::User { .. } => "user".to_owned(),
                Turn::Agent { status, .. } => format!("agent:{status:?}"),
                Turn::System { marker, .. } => match marker {
                    SystemMarker::Compaction { summary: None, .. } => "compaction".to_owned(),
                    other => format!("{other:?}"),
                },
            })
            .collect();
        assert_eq!(
            shapes,
            [
                "user",
                "agent:Complete",
                "compaction",
                "user",
                "agent:Complete",
                "user",
                "agent:Complete"
            ]
        );
        assert_eq!(user_texts(&transcript)[2], "And now?");
    }

    #[test]
    fn a_chain_that_loops_back_is_refused() {
        // The fork points at A, and A points back at the fork.
        const FORK: &str = "00000000-0000-7000-8000-00000000f0f0";
        const A: &str = "00000000-0000-7000-8000-00000000a0a0";
        let mut fork_len = 0;
        let mut a = String::new();
        for _ in 0..3 {
            a = meta_line(A, Some((FORK, fork_len)));
            fork_len = meta_line(FORK, Some((A, a.len()))).len();
        }
        let home = Home::new();
        home.rollout(11, A, &a);
        home.rollout(11, FORK, &meta_line(FORK, Some((A, a.len()))));

        let transcript = home.load(FORK, 11);

        assert_eq!(markers(&transcript), [(InheritedHistoryGap::Cycle, FORK)]);
    }

    /// Stage a chain of `ancestors` single-line rollouts above a fork with id
    /// `own`, each pointing at the next, and return the fork's transcript.
    fn load_chain(ancestors: usize) -> LoadedTranscript {
        let home = Home::new();
        let id = |n: usize| format!("00000000-0000-7000-8000-{n:012}");
        let mut next: Option<(String, usize)> = None;
        for n in (1..=ancestors).rev() {
            let line = meta_line(&id(n), next.as_ref().map(|(i, len)| (i.as_str(), *len)));
            home.rollout(11, &id(n), &line);
            next = Some((id(n), line.len()));
        }
        let own = id(0);
        home.rollout(
            11,
            &own,
            &meta_line(&own, next.as_ref().map(|(i, len)| (i.as_str(), *len))),
        );
        home.load(&own, 11)
    }

    #[test]
    fn a_chain_at_the_depth_cap_loads_and_one_past_it_is_refused() {
        assert!(markers(&load_chain(MAX_FORK_DEPTH)).is_empty());
        let too_deep = load_chain(MAX_FORK_DEPTH + 1);
        assert_eq!(markers(&too_deep).len(), 1);
        assert_eq!(markers(&too_deep)[0].0, InheritedHistoryGap::TooDeep);
    }

    #[test]
    fn two_files_carrying_the_parents_id_are_refused_rather_than_picked_from() {
        // One copy sits beside the fork, where a shortcut would find it first;
        // the other is in another day. Either could hold a different
        // conversation, so neither is shown.
        let home = Home::new();
        home.rollout(10, PARENT, &fixture("fork-parent"));
        home.rollout(11, PARENT, &fixture("fork-parent"));
        home.rollout(11, CHILD, &fixture("fork-child"));

        let transcript = home.load(CHILD, 11);

        assert_eq!(
            markers(&transcript),
            [(InheritedHistoryGap::Ambiguous, PARENT)]
        );
    }

    #[test]
    fn an_original_archived_in_codex_still_shows_its_history() {
        // Codex follows a fork's pointer into `archived_sessions` too, so the
        // fork keeps working; it must not be reported as missing.
        let home = Home::new();
        home.archive(PARENT, ".jsonl", &fixture("fork-parent"));
        home.rollout(11, CHILD, &fixture("fork-child"));

        let transcript = home.load(CHILD, 11);

        assert!(
            markers(&transcript).is_empty(),
            "{:?}",
            markers(&transcript)
        );
        assert_eq!(user_texts(&transcript)[..2], [PELICAN, CHILD_FIRST]);
    }

    #[test]
    fn an_archived_grandparent_completes_the_chain() {
        let home = Home::new();
        home.archive(PARENT, ".jsonl", &fixture("fork-parent"));
        home.rollout(11, CHILD, &fixture("fork-child"));
        home.rollout(11, GRANDCHILD, &fixture("fork-grandchild"));

        let transcript = home.load(GRANDCHILD, 11);

        assert!(
            markers(&transcript).is_empty(),
            "{:?}",
            markers(&transcript)
        );
        assert_eq!(user_texts(&transcript).len(), 4);
        assert_eq!(user_texts(&transcript)[0], PELICAN);
    }

    #[test]
    fn a_copy_in_sessions_wins_over_an_archived_copy_without_counting_as_a_duplicate() {
        // Codex searches `sessions` first and stops there. The archived copy
        // here is damaged, so reading it would produce a "changed" marker.
        let home = Home::new();
        home.rollout(10, PARENT, &fixture("fork-parent"));
        home.archive(PARENT, ".jsonl", "not the parent\n");
        home.rollout(11, CHILD, &fixture("fork-child"));

        let transcript = home.load(CHILD, 11);

        assert!(
            markers(&transcript).is_empty(),
            "{:?}",
            markers(&transcript)
        );
        assert_eq!(user_texts(&transcript)[0], PELICAN);
    }

    #[test]
    fn a_compressed_copy_in_sessions_decides_even_when_the_archive_has_a_plain_one() {
        // Codex would read the compressed copy it finds first, so the fork
        // works; Switchboard can't read it, and must say so rather than show
        // the archived copy as if it were the one Codex uses.
        let home = Home::new();
        home.stage(10, PARENT, ".jsonl.zst", "not really zstd");
        home.archive(PARENT, ".jsonl", &fixture("fork-parent"));
        home.rollout(11, CHILD, &fixture("fork-child"));

        let transcript = home.load(CHILD, 11);

        assert_eq!(
            markers(&transcript),
            [(InheritedHistoryGap::Compressed, PARENT)]
        );
    }

    #[test]
    fn a_compressed_archived_original_is_told_apart_from_a_missing_one() {
        let home = Home::new();
        home.archive(PARENT, ".jsonl.zst", "not really zstd");
        home.rollout(11, CHILD, &fixture("fork-child"));

        let transcript = home.load(CHILD, 11);

        assert_eq!(
            markers(&transcript),
            [(InheritedHistoryGap::Compressed, PARENT)]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_named_like_the_original_does_not_hide_an_archived_copy() {
        // Codex reads regular files only, so it runs this fork from the
        // archived copy; the broken link in `sessions` must not stand in front
        // of it and turn into a false "missing" line.
        let home = Home::new();
        let folder = home.dir.path().join(".codex/sessions/2026/10/11");
        std::fs::create_dir_all(&folder).unwrap();
        symlink(
            home.dir.path().join("nowhere.jsonl"),
            folder.join(format!("rollout-2026-10-11T00-00-00-{PARENT}.jsonl")),
        )
        .unwrap();
        home.archive(PARENT, ".jsonl", &fixture("fork-parent"));
        home.rollout(11, CHILD, &fixture("fork-child"));

        let transcript = home.load(CHILD, 11);

        assert!(
            markers(&transcript).is_empty(),
            "{:?}",
            markers(&transcript)
        );
        assert_eq!(user_texts(&transcript)[0], PELICAN);
    }

    #[test]
    fn a_forks_own_header_needs_only_its_pointer_to_be_followed() {
        // The stricter "header names its session" rule is for files reached
        // through a pointer. The fork's own header carries the pointer, which
        // is enough.
        let home = Home::new();
        home.rollout(11, PARENT, &fixture("fork-parent"));
        let child = fixture("fork-child");
        let without_id = child.replacen(&format!(r#""id":"{CHILD}","#), "", 1);
        assert_ne!(without_id, child, "the fixture's header lost its id field");
        home.rollout(11, CHILD, &without_id);

        let transcript = home.load(CHILD, 11);

        assert!(
            markers(&transcript).is_empty(),
            "{:?}",
            markers(&transcript)
        );
        assert_eq!(user_texts(&transcript)[..2], [PELICAN, CHILD_FIRST]);
    }

    /// The fork-child fixture with its header line replaced by `header`,
    /// padded with spaces to the same byte length, so the grandchild's
    /// cut-off (the whole file) still matches and ends on a line break.
    fn child_with_header(header: &str) -> String {
        let child = fixture("fork-child");
        let (first, rest) = child.split_once('\n').unwrap();
        assert!(header.len() <= first.len());
        format!("{header}{}\n{rest}", " ".repeat(first.len() - header.len()))
    }

    #[test]
    fn a_middle_fork_whose_header_is_unreadable_breaks_the_chain() {
        // Without its header, nothing says the middle fork continues the
        // chain; showing its turns as the start would hide the parent's.
        let home = Home::new();
        home.rollout(11, PARENT, &fixture("fork-parent"));
        home.rollout(11, CHILD, &child_with_header("not json at all"));
        home.rollout(11, GRANDCHILD, &fixture("fork-grandchild"));

        let transcript = home.load(GRANDCHILD, 11);

        assert_eq!(
            markers(&transcript),
            [(InheritedHistoryGap::Changed, CHILD)]
        );
        assert_eq!(user_texts(&transcript), [GRANDCHILD_ONLY]);
    }

    #[test]
    fn a_middle_fork_whose_header_parses_but_says_nothing_breaks_the_chain() {
        // Valid JSON of the right type, but no session in it: "no pointer"
        // must come from a header that says so, not from an empty one.
        let home = Home::new();
        home.rollout(11, PARENT, &fixture("fork-parent"));
        home.rollout(
            11,
            CHILD,
            &child_with_header(r#"{"type":"session_meta","payload":null}"#),
        );
        home.rollout(11, GRANDCHILD, &fixture("fork-grandchild"));

        let transcript = home.load(GRANDCHILD, 11);

        assert_eq!(
            markers(&transcript),
            [(InheritedHistoryGap::Changed, CHILD)]
        );
        assert_eq!(user_texts(&transcript), [GRANDCHILD_ONLY]);
    }

    #[test]
    fn a_rollout_that_is_not_a_fork_is_unchanged() {
        let home = Home::new();
        home.rollout(11, PARENT, &fixture("fork-parent"));

        let transcript = home.load(PARENT, 11);

        assert_eq!(user_texts(&transcript), [PELICAN, OTTER]);
        assert!(markers(&transcript).is_empty());
    }

    #[test]
    fn a_plain_rollout_name_yields_its_one_id() {
        assert_eq!(
            rollout_file_id(
                "rollout-2026-10-11T13-03-50-01a12c22-a3f8-71e2-b51e-ca72562cfabb.jsonl"
            ),
            Some(("01a12c22-a3f8-71e2-b51e-ca72562cfabb", false))
        );
    }

    #[test]
    fn a_reverted_rollout_name_yields_the_id_after_the_underscore() {
        assert_eq!(
            rollout_file_id("rollout-2026-10-11T13-03-50-thread-1111_rollout-2222.jsonl"),
            Some(("rollout-2222", false))
        );
    }

    #[test]
    fn a_compressed_rollout_name_is_recognised() {
        assert_eq!(
            rollout_file_id("rollout-2026-10-11T13-03-50-0000-1111.jsonl.zst"),
            Some(("0000-1111", true))
        );
    }

    #[test]
    fn names_that_are_not_rollouts_yield_nothing() {
        for name in [
            ".DS_Store",
            "rollout-2026-10-11T13-03-50-.jsonl",
            "rollout-short-id.jsonl",
            "session-2026-10-11T13-03-50-0000.jsonl",
            "rollout-2026-10-11T13-03-50-0000.json",
        ] {
            assert_eq!(rollout_file_id(name), None, "{name}");
        }
    }
}
