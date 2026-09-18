//! Extract the environment inventory from a Codex rollout's `world_state`
//! records and its `turn_context`.
//!
//! **This is a scraped format, like the Claude context report.** Codex writes
//! its loaded skills as a *markdown block* meant for the model to read
//! (`world_state.state.host_skills.body`), not as structured data — name,
//! description and a root-relative path per line, with the roots in a
//! separate table above. There is no other source: neither Codex's `--json`
//! stream nor its config files name the skills it actually loaded. So the
//! parser is fixture-driven and fails soft: an unrecognized line is counted
//! and skipped, never fatal, and a body we cannot read at all yields an empty
//! list rather than a wrong one.
//!
//! **`world_state` is a snapshot-plus-delta log, not a series of snapshots.**
//! A record with `full: true` carries the whole state; one with `full: false`
//! carries **only the keys that changed** (observed: `{"environments"}` alone,
//! mid-session). Last-*record*-wins would therefore erase `host_skills` and
//! `permissions` the moment any delta landed, so the reader accumulates
//! per-key: a delta updates the keys it names, and a full snapshot replaces
//! the accumulator outright (a key absent from a full snapshot is genuinely
//! gone).

use serde_json::{Map, Value};

use crate::events::{SettingPair, SkillEntry};

/// Accumulated `world_state.state`, folded across every `world_state` record
/// in the file. See the module doc for why this cannot be the last record's
/// `state` object.
#[derive(Debug, Default)]
pub(crate) struct WorldState {
    state: Map<String, Value>,
    seen: bool,
}

impl WorldState {
    /// Fold one `world_state` record's payload in. `full` records replace,
    /// deltas merge per key.
    pub(crate) fn absorb(&mut self, payload: &Value) {
        let Some(state) = payload.get("state").and_then(Value::as_object) else {
            return;
        };
        self.seen = true;
        if payload.get("full").and_then(Value::as_bool) == Some(true) {
            self.state.clone_from(state);
            return;
        }
        for (key, value) in state {
            self.state.insert(key.clone(), value.clone());
        }
    }

    /// Whether any `world_state` record was seen at all. A rollout written by
    /// a Codex that predates the record (observed: no `host_skills` or
    /// `permissions` on July rollouts) reports nothing, and every list must
    /// stay `None` so the config loaders still fill what they can.
    pub(crate) fn is_empty(&self) -> bool {
        !self.seen
    }

    /// The skills Codex loaded, parsed out of the `host_skills` markdown.
    ///
    /// `None` when the state carries no `host_skills` block; `Some([])` when it
    /// carries one we could not read a single skill from — that is an
    /// authoritative "Codex reported its skills and we failed", which must
    /// degrade to an empty section rather than silently handing the card back
    /// to the directory scanner, whose list is a different (and incomplete)
    /// thing.
    pub(crate) fn skills(&self) -> Option<Vec<SkillEntry>> {
        let body = self
            .state
            .get("host_skills")?
            .get("body")
            .and_then(Value::as_str)?;
        let parsed = parse_host_skills(body);
        if parsed.unparsed_lines > 0 {
            tracing::warn!(
                unparsed_lines = parsed.unparsed_lines,
                parsed_skills = parsed.skills.len(),
                "Codex world_state: unreadable lines in the host_skills block; skills list may be incomplete"
            );
        }
        Some(parsed.skills)
    }

    /// The user's approved-command allowlist. Codex stores each entry as an
    /// argv **prefix** (`["brew", "install", …]`); the display form is the
    /// argv joined back into one command line.
    pub(crate) fn approved_commands(&self) -> Option<Vec<String>> {
        let prefixes = self
            .state
            .get("permissions")?
            .get("approved_command_prefixes")?
            .as_array()?;
        Some(
            prefixes
                .iter()
                .filter_map(|entry| {
                    let argv: Vec<&str> =
                        entry.as_array()?.iter().filter_map(Value::as_str).collect();
                    (!argv.is_empty()).then(|| argv.join(" "))
                })
                .collect(),
        )
    }

    /// The local environment's shell. Keyed on the `local` environment
    /// deliberately: Codex models environments as a map and a remote one's
    /// shell would be a false statement about the machine the card describes.
    fn shell(&self) -> Option<&str> {
        self.state
            .get("environments")?
            .get("environments")?
            .get("local")?
            .get("shell")?
            .as_str()
    }
}

/// Codex's run settings as display pairs, drawn from the last `turn_context`
/// (the current turn's selections) and the accumulated `world_state` (the
/// shell, which `turn_context` does not carry).
///
/// `None` when neither source yielded a single readable setting, so the card
/// draws no settings line at all instead of an empty one.
pub(crate) fn settings(
    world: &WorldState,
    turn_context: Option<&Value>,
) -> Option<Vec<SettingPair>> {
    let mut pairs: Vec<SettingPair> = Vec::new();
    let mut push = |label: &str, value: Option<&str>| {
        if let Some(value) = value.filter(|v| !v.is_empty()) {
            pairs.push(SettingPair {
                label: label.to_owned(),
                value: value.to_owned(),
            });
        }
    };
    // `sandbox_policy` is a tagged object (`{"type": "read-only"}`); the tag is
    // the whole display value.
    push(
        "Sandbox",
        turn_context
            .and_then(|c| c.get("sandbox_policy"))
            .and_then(|p| p.get("type"))
            .and_then(Value::as_str),
    );
    push(
        "Approval policy",
        turn_context
            .and_then(|c| c.get("approval_policy"))
            .and_then(Value::as_str),
    );
    push(
        "Personality",
        turn_context
            .and_then(|c| c.get("personality"))
            .and_then(Value::as_str),
    );
    push("Shell", world.shell());
    push(
        "Timezone",
        turn_context
            .and_then(|c| c.get("timezone"))
            .and_then(Value::as_str),
    );
    (!pairs.is_empty()).then_some(pairs)
}

/// Outcome of reading the `host_skills` markdown block.
struct HostSkills {
    skills: Vec<SkillEntry>,
    /// Bullet lines under the skills heading that did not match the expected
    /// shape. Counted rather than ignored so a format change is visible: the
    /// whole block silently parsing to zero skills is what a rename of the
    /// heading would look like.
    unparsed_lines: usize,
}

/// Heading that opens the root-abbreviation table.
const ROOTS_HEADING: &str = "### Skill roots";
/// Heading that opens the skill list.
const SKILLS_HEADING: &str = "### Available skills";
/// Separator between a skill's name and its description. The space matters:
/// namespaced skill names carry a bare colon (`data-analytics:build-report`),
/// so splitting on `:` alone would truncate every one of them.
const NAME_SEPARATOR: &str = ": ";
/// Opens the root-relative path suffix, e.g. ` (file: r3/x/SKILL.md)`.
const PATH_PREFIX: &str = " (file: ";

/// Parse the `host_skills` body:
///
/// ```text
/// ### Skill roots
/// - `r0` = `/Users/me/.codex/skills/.system`
/// ### Available skills
/// - imagegen: Generate or edit raster images … (file: r0/imagegen/SKILL.md)
/// ```
fn parse_host_skills(body: &str) -> HostSkills {
    let mut roots: Vec<(String, String)> = Vec::new();
    let mut skills: Vec<SkillEntry> = Vec::new();
    let mut unparsed_lines = 0usize;
    let mut section = Section::Preamble;

    for line in body.lines() {
        let line = line.trim();
        if line == ROOTS_HEADING {
            section = Section::Roots;
            continue;
        }
        if line == SKILLS_HEADING {
            section = Section::Skills;
            continue;
        }
        // Any other heading closes the section we were in — the block's prose
        // preamble and a future trailing section must not be read as entries.
        if line.starts_with('#') {
            section = Section::Preamble;
            continue;
        }
        let Some(entry) = line.strip_prefix("- ") else {
            continue;
        };
        match section {
            Section::Preamble => {}
            Section::Roots => match parse_root(entry) {
                Some(root) => roots.push(root),
                None => unparsed_lines += 1,
            },
            Section::Skills => match parse_skill(entry, &roots) {
                Some(skill) => skills.push(skill),
                None => unparsed_lines += 1,
            },
        }
    }

    // A block with a skills heading but nothing readable under it is the
    // format-moved case, and is reported even though no individual line
    // failed: zero entries from a non-trivial body is itself the signal.
    if skills.is_empty() && body.contains(SKILLS_HEADING) {
        unparsed_lines += 1;
    }

    HostSkills {
        skills,
        unparsed_lines,
    }
}

enum Section {
    Preamble,
    Roots,
    Skills,
}

/// `` `r0` = `/abs/path` `` → `("r0", "/abs/path")`.
fn parse_root(entry: &str) -> Option<(String, String)> {
    let (abbrev, path) = entry.split_once(" = ")?;
    Some((
        unquote(abbrev)?.to_owned(),
        unquote(path.trim())?.to_owned(),
    ))
}

fn unquote(value: &str) -> Option<&str> {
    value.strip_prefix('`')?.strip_suffix('`')
}

/// `name: description (file: r0/rest)` → a [`SkillEntry`] with the root
/// abbreviation expanded back to an absolute path.
fn parse_skill(entry: &str, roots: &[(String, String)]) -> Option<SkillEntry> {
    let (name, rest) = entry.split_once(NAME_SEPARATOR)?;
    // Taken from the **last** occurrence: a description is free text and may
    // legitimately contain the literal `(file: `.
    let (description, path) = match rest.rfind(PATH_PREFIX) {
        Some(at) => {
            let path = rest[at + PATH_PREFIX.len()..].strip_suffix(')')?;
            (&rest[..at], Some(expand_root(path, roots)))
        }
        // A skill with no path suffix still has a name and a description,
        // which is most of what the card shows — keep it.
        None => (rest, None),
    };
    Some(SkillEntry {
        name: name.to_owned(),
        description: (!description.is_empty()).then(|| description.to_owned()),
        path,
    })
}

/// Expand `r3/x/SKILL.md` using the roots table. An abbreviation the table
/// does not define yields the path verbatim — a short path still locates the
/// skill relative to a root the user can see, and dropping the skill over a
/// missing table row would lose more than it protects.
fn expand_root(path: &str, roots: &[(String, String)]) -> String {
    let Some((abbrev, rest)) = path.split_once('/') else {
        return path.to_owned();
    };
    match roots.iter().find(|(name, _)| name == abbrev) {
        Some((_, root)) => format!("{root}/{rest}"),
        None => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The `host_skills` body shape, recorded from codex 0.154.0. The real
    /// block carries ~30 skills across 7 roots; three across two roots
    /// exercise every branch.
    const BODY: &str = "\n## Skills\nA skill is a set of local instructions to follow.\n### Skill roots\n- `r0` = `/home/me/.codex/skills/.system`\n- `r3` = `/home/me/.codex/plugins/cache/data-analytics/1.0.9/skills`\n### Available skills\n- imagegen: Generate or edit raster images. (file: r0/imagegen/SKILL.md)\n- skill-creator: Create or update a Codex skill. (file: r0/skill-creator/SKILL.md)\n- data-analytics:build-report: Build polished analytical reports. (file: r3/build-report/SKILL.md)\n";

    fn world_with(state: &Value) -> WorldState {
        let mut world = WorldState::default();
        world.absorb(&json!({"full": true, "state": state}));
        world
    }

    #[test]
    fn host_skills_yields_name_description_and_an_absolute_path() {
        let world = world_with(&json!({"host_skills": {"body": BODY}}));
        let skills = world.skills().expect("host_skills reported");
        assert_eq!(skills.len(), 3);
        assert_eq!(skills[0].name, "imagegen");
        assert_eq!(
            skills[0].description.as_deref(),
            Some("Generate or edit raster images.")
        );
        assert_eq!(
            skills[0].path.as_deref(),
            Some("/home/me/.codex/skills/.system/imagegen/SKILL.md"),
            "the root abbreviation must expand back to an absolute path"
        );
        // A namespaced name carries a bare colon, so splitting on `:` rather
        // than `: ` would truncate it to "data-analytics".
        assert_eq!(skills[2].name, "data-analytics:build-report");
        assert_eq!(
            skills[2].path.as_deref(),
            Some("/home/me/.codex/plugins/cache/data-analytics/1.0.9/skills/build-report/SKILL.md"),
            "each entry expands against its own root, not the first"
        );
    }

    #[test]
    fn a_description_containing_the_path_marker_keeps_its_text() {
        // The suffix is taken from the last occurrence: a description is free
        // text and may legitimately contain the literal marker.
        let body =
            "### Available skills\n- odd: Reads a (file: x) reference. (file: r0/odd/SKILL.md)\n";
        let skills = world_with(&json!({"host_skills": {"body": body}}))
            .skills()
            .expect("reported");
        assert_eq!(
            skills[0].description.as_deref(),
            Some("Reads a (file: x) reference.")
        );
        assert_eq!(skills[0].path.as_deref(), Some("r0/odd/SKILL.md"));
    }

    #[test]
    fn an_unknown_root_keeps_the_short_path_rather_than_dropping_the_skill() {
        let body = "### Skill roots\n- `r0` = `/known`\n### Available skills\n- a: One. (file: r9/a/SKILL.md)\n";
        let skills = world_with(&json!({"host_skills": {"body": body}}))
            .skills()
            .expect("reported");
        assert_eq!(skills.len(), 1, "the skill survives a missing roots row");
        assert_eq!(skills[0].path.as_deref(), Some("r9/a/SKILL.md"));
    }

    #[test]
    fn a_skill_with_no_path_suffix_keeps_its_name_and_description() {
        let body = "### Available skills\n- bare: Just a description\n";
        let skills = world_with(&json!({"host_skills": {"body": body}}))
            .skills()
            .expect("reported");
        assert_eq!(skills[0].name, "bare");
        assert_eq!(skills[0].description.as_deref(), Some("Just a description"));
        assert_eq!(skills[0].path, None);
    }

    #[test]
    fn bullets_outside_the_skills_section_are_not_skills() {
        // The roots table's rows are bullets, and the block's prose preamble
        // may grow them — only lines under the skills heading are entries.
        let body = "\n## Skills\n- a note: about how skills work\n### Skill roots\n- `r0` = `/known`\n### Available skills\n- real: The only entry. (file: r0/real/SKILL.md)\n";
        let skills = world_with(&json!({"host_skills": {"body": body}}))
            .skills()
            .expect("reported");
        assert_eq!(
            skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["real"],
            "only the skills section supplies entries: {skills:?}"
        );
    }

    #[test]
    fn an_unreadable_block_reports_an_empty_list_not_an_absent_one() {
        // The format-moved case. `Some([])` is deliberate: Codex reported its
        // skills and we failed to read them, which must show as an empty
        // section rather than silently handing the card back to the directory
        // scanner, whose list means something different.
        let world =
            world_with(&json!({"host_skills": {"body": "### Available skills\nnot a bullet\n"}}));
        assert_eq!(world.skills(), Some(vec![]));
    }

    #[test]
    fn a_trailing_section_closes_the_skills_list() {
        // The block has grown sections before (roots were added after the
        // list). A heading after the entries must end them, not have its own
        // bullets read as skills.
        let body = "### Available skills\n- real: An entry. (file: r0/real/SKILL.md)\n### Something Else\n- notaskill: Not an entry.\n";
        let skills = world_with(&json!({"host_skills": {"body": body}}))
            .skills()
            .expect("reported");
        assert_eq!(
            skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["real"]
        );
    }

    #[test]
    fn unreadable_lines_are_counted_so_a_format_change_is_loud() {
        // The count is what drives the `tracing::warn!` — the only signal a
        // reword produces, since a display-only registry must not surface an
        // error row on the card. Asserted directly rather than through the
        // log, so the condition is pinned even though the emission is not.
        assert_eq!(
            parse_host_skills("### Available skills\n- ok: Fine. (file: r0/ok/SKILL.md)\n")
                .unparsed_lines,
            0,
            "a clean block must warn about nothing"
        );
        let mixed = parse_host_skills(
            "### Available skills\n- ok: Fine. (file: r0/ok/SKILL.md)\n- no separator here\n",
        );
        assert_eq!(mixed.skills.len(), 1, "the readable entry survives");
        assert_eq!(mixed.unparsed_lines, 1, "the unreadable one is counted");
        // Zero entries from a block that *has* the heading is itself the
        // signal, even though no individual line failed to match.
        assert_eq!(
            parse_host_skills("### Available skills\nnot a bullet at all\n").unparsed_lines,
            1
        );
    }

    #[test]
    fn no_host_skills_block_reports_nothing() {
        // An older Codex writes `world_state` without `host_skills`; the
        // config-file scanner must still be allowed to fill the list.
        assert_eq!(world_with(&json!({"environments": {}})).skills(), None);
    }

    #[test]
    fn approved_command_prefixes_join_into_command_lines() {
        let world = world_with(&json!({
            "permissions": {"approved_command_prefixes": [
                ["brew", "install", "jq"], ["ls"], [], ["cat", 7]
            ]}
        }));
        assert_eq!(
            world.approved_commands(),
            Some(vec![
                "brew install jq".to_owned(),
                "ls".to_owned(),
                // A non-string argv element is skipped, not fatal.
                "cat".to_owned(),
            ]),
            "an empty prefix renders nothing; the rest join with spaces"
        );
    }

    #[test]
    fn an_empty_allowlist_is_reported_as_empty() {
        let world = world_with(&json!({"permissions": {"approved_command_prefixes": []}}));
        assert_eq!(world.approved_commands(), Some(vec![]));
        let absent = world_with(&json!({"permissions": {}}));
        assert_eq!(absent.approved_commands(), None);
    }

    #[test]
    fn settings_come_from_the_turn_context_and_the_local_environment() {
        let world = world_with(&json!({
            "environments": {"environments": {"local": {"shell": "zsh"}}}
        }));
        let turn_context = json!({
            "sandbox_policy": {"type": "read-only"},
            "approval_policy": "never",
            "personality": "pragmatic",
            "timezone": "America/Los_Angeles",
        });
        let pairs = settings(&world, Some(&turn_context)).expect("settings reported");
        let rendered: Vec<(&str, &str)> = pairs
            .iter()
            .map(|p| (p.label.as_str(), p.value.as_str()))
            .collect();
        assert_eq!(
            rendered,
            vec![
                ("Sandbox", "read-only"),
                ("Approval policy", "never"),
                ("Personality", "pragmatic"),
                ("Shell", "zsh"),
                ("Timezone", "America/Los_Angeles"),
            ]
        );
    }

    #[test]
    fn settings_skip_what_neither_source_reports() {
        let world = world_with(&json!({"environments": {}}));
        let pairs = settings(&world, Some(&json!({"approval_policy": "on-request"})))
            .expect("one readable setting");
        assert_eq!(pairs.len(), 1, "absent settings render no row: {pairs:?}");
        assert_eq!(pairs[0].label, "Approval policy");
        assert_eq!(
            settings(&world, None),
            None,
            "nothing readable → no settings line at all"
        );
    }

    #[test]
    fn a_remote_environment_does_not_supply_the_shell() {
        // Keyed on `local` deliberately: a remote environment's shell would be
        // a false statement about the machine the card describes.
        let world = world_with(&json!({
            "environments": {"environments": {"remote": {"shell": "bash"}}}
        }));
        assert_eq!(settings(&world, None), None);
    }

    #[test]
    fn a_delta_record_updates_only_the_keys_it_names() {
        // 77 of 1,222 probed rollouts carry more than one `world_state`, and
        // the later ones are routinely `{"full": false, "state":
        // {"environments": …}}`. Taking the last record's `state` would erase
        // `host_skills` and `permissions` the moment any delta landed.
        let mut world = world_with(&json!({
            "host_skills": {"body": BODY},
            "permissions": {"approved_command_prefixes": [["ls"]]},
        }));
        world.absorb(&json!({
            "full": false,
            "state": {"environments": {"environments": {"local": {"shell": "fish"}}}}
        }));
        assert_eq!(
            world.skills().map(|s| s.len()),
            Some(3),
            "a delta must not erase a key it does not mention"
        );
        assert_eq!(world.approved_commands(), Some(vec!["ls".to_owned()]));
        assert_eq!(
            world.shell(),
            Some("fish"),
            "the delta's own key is applied"
        );
    }

    #[test]
    fn a_full_snapshot_replaces_the_accumulator() {
        // The other direction: a key absent from a *full* snapshot is
        // genuinely gone, so a full record must not merge.
        let mut world = world_with(&json!({"host_skills": {"body": BODY}}));
        world.absorb(&json!({"full": true, "state": {"environments": {}}}));
        assert_eq!(world.skills(), None);
    }

    #[test]
    fn a_rollout_with_no_world_state_reports_nothing() {
        let world = WorldState::default();
        assert!(world.is_empty());
        // A record whose payload carries no `state` object is not evidence of
        // anything and must not flip `is_empty`.
        let mut malformed = WorldState::default();
        malformed.absorb(&json!({"full": true}));
        assert!(malformed.is_empty());
    }
}
