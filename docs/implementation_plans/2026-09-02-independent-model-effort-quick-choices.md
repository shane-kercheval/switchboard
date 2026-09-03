# Independent model and effort quick choices

**Status:** proposed · **Created:** 2026-09-02

Replace the agent's bundled Primary/Secondary model profiles with two independent quick-choice
sets: one for models and one for reasoning efforts. Each configured set may contain one or more
values. A new agent starts on an explicit default from each applicable set, while an existing agent
exposes its current model and effort as separate controls in the sidebar.

The product gain is narrow and concrete: someone can switch `Sol → Terra` without also changing
`High → Medium`, or change only the effort while keeping the model. Selecting more than two quick
choices is supported because it falls naturally out of the same model, but the sidebar retains the
one-click toggle when exactly two choices are configured.

This plan deliberately removes the user-facing and persisted concept of a "profile." A profile is
only the current implementation's bundled `{ model, effort }` pair, named Primary or Secondary. It
has no independent product meaning once the two axes can be changed separately.

The whole plan is one focused feature PR. The milestones below are dependency-ordered implementation
units, not separate product releases.

## Required reading before implementing

Read these sources before changing code. The implementation must preserve their invariants; if the
current source or harness documentation contradicts this plan, stop and resolve the discrepancy
before proceeding.

- [`AGENTS.md`](../../AGENTS.md) — especially the required `make` targets, foreground execution of
  long-running checks, test vocabulary, and frontend component-test requirements.
- [`docs/system-design.md`](../system-design.md) §7 (sends, turns, and queued-send snapshots) and §9
  (per-harness model/effort behavior). Update both where this plan changes their profile language.
- [`docs/ui-conventions.md`](../ui-conventions.md) — semantic color roles, segmented-control styling,
  menus, tooltips, and the requirement to reuse UI primitives. In particular, green/status colors do
  not mean "default"; defaults need text or control semantics, not color alone.
- [`docs/harness-behavior.md`](../harness-behavior.md) §3.3–§3.4 — model and effort flags, omitted-value
  behavior, and Antigravity's model-dependent effort constraints.
- [`docs/implementation_plans/2026-05-30-per-agent-model-selection.md`](2026-05-30-per-agent-model-selection.md)
  — provenance for selected intent versus per-turn observed history. Its Primary/Secondary UI is
  superseded by this plan; its transcript-history distinction is not.
- The current implementations in `crates/core/src/agent.rs`, `crates/core/src/project.rs`,
  `crates/app/src/preferences.rs`, `crates/app/src/dispatch_context.rs`,
  `src/lib/agentSelection.ts`, `src/lib/components/AgentProfileEditor.svelte`,
  `src/lib/components/CreateAgentForm.svelte`, `src/lib/components/SettingsView.svelte`, and
  `src/lib/components/Sidebar.svelte`. Several load-bearing behaviors exist only in their comments
  and tests: attach leaves a session unpinned, fork inherits configuration, off-catalog selections
  survive editing, and queued sends retain the selection captured at submission.
- [WAI-ARIA Button Pattern](https://www.w3.org/WAI/ARIA/apg/patterns/button/) and
  [Menu Button Pattern](https://www.w3.org/WAI/ARIA/apg/patterns/menu-button/) — the multi-select
  choices are toggle buttons (or equivalent checkbox controls), while a 3+-choice sidebar chip is a
  menu button. Do not reuse radio-group semantics for multi-select controls.

## Decisions settled in discussion

These decisions are inputs to the implementation, not questions for the implementing agent to
re-open silently.

1. **Model and effort are independent axes.** The configuration is not a list of model/effort
   combinations and does not generate named profiles. A model change preserves the current effort
   when that pair is valid; an effort change never changes the model.
2. **Each axis has a quick-choice set.** The set may contain one or more curated values. Supporting
   more than two is not a general favorites system or a new catalog mechanism; it is only
   multi-selection from the existing per-harness curated lists.
3. **Settings owns defaults for new agents.** Each harness has selected model choices, selected
   effort choices, a default model, and a default effort. These values are copied into a new agent.
   Changing Settings later does not silently reconfigure existing agents and is not consulted as a
   fallback when an existing agent switches models.
4. **A default control is enabled only when more than one option is selected.** With exactly one
   selected option, that option is implicitly the default. Show a muted `Default` label or a disabled
   representation for clarity, but no interactive default picker. Settings never permits zero
   selected options on an axis it configures.
5. **No double-click gesture and no green default state.** Double-click is undiscoverable and poorly
   accessible, and green is reserved for existing semantic/status roles. Multi-select state uses the
   established selected-control treatment; default identity is explicit in text/control state.
6. **The sidebar has one control per axis.** With one configured choice the chip is static; with
   exactly two it is a one-click toggle with a swap affordance; with three or more it is a menu button
   with a chevron. The one-choice exception is an existing agent whose current value is null or
   different: that chip is a one-click adoption action until the choice becomes current. A 3+-choice
   chip does not cycle because catalog order is not a meaningful user decision and repeated clicks
   are a poor way to reach a specific value.
7. **Existing agents own their copied choices.** The existing `Model settings…` action edits that
   agent's quick choices and current selections atomically. It does not edit global defaults. A
   `Reset to defaults` feature was mentioned only as a possible convenience and is **not in scope**
   for this change.
8. **Attach remains unpinned.** Attaching an existing session does not apply the global defaults or
   submit model/effort flags. It creates an agent with empty quick-choice sets and no current
   selection, preserving the current `Harness/session default` behavior until the user explicitly
   configures the agent. This is a backend invariant: attach APIs do not accept a selection payload.
9. **Fork inherits the complete agent configuration.** A fork receives the source's quick-choice sets
   and current model/effort, just as it currently inherits both profiles and the active slot.
10. **Selection changes remain future-send intent.** A successful sidebar or settings-dialog change
    affects sends submitted afterward. In-flight and already queued sends retain the model/effort
    snapshot captured when they were submitted.
11. **Per-turn transcript metadata is unchanged.** Transcript footers continue to show the model and
    effort actually reported for that historical turn. Sidebar chips show the configuration that
    will be used for a future send. Do not conflate or reconcile the two.
12. **Antigravity is the one intentional dependency between axes.** Its valid effort choices are a
    function of the current model, and some models have no effort axis. The UI must resolve a model
    change into a valid current pair and persist that pair atomically; the core must not duplicate the
    frontend's changing model catalog.
13. **Curated catalogs remain suggestions, not backend allow-lists.** Preserve off-catalog persisted
    values and reactive harness validation. This feature changes which values are convenient to
    switch among; it does not add model discovery, account-plan validation, or a second catalog.
14. **Unknown Antigravity models are not no-effort models.** Frontend catalog knowledge distinguishes
    models with known effort levels, models explicitly known to have no effort axis, and unknown
    models. Only the explicit no-effort state clears effort. An unknown model preserves current and
    configured effort values and relies on reactive harness validation.

## Domain contract established by Milestone 1

Later milestones must use this contract rather than inventing parallel shapes.

### Persisted agent state

An agent stores four concepts with these wire-field names:

- `model`: current model (`Option<String>`), used when capturing a new send;
- `effort`: current effort (`Option<String>`), used when capturing a new send;
- `model_choices`: the model quick-choice values;
- `effort_choices`: the effort quick-choice values.

Keep `model` and `effort` as the dispatch-ready pair instead of deriving them through a slot or an
index. Retaining those field names lets an older build degrade to treating the current pair as its
Primary selection rather than losing the selection entirely. Remove the persisted Primary/Secondary
structures and active-slot enum after adding backward deserialization. Rust/TypeScript type names
remain implementation-local, but there must be one wire shape shared by create, edit, fork, state
replacement, and sidebar switching.

The persistence boundary owns these structural invariants:

- trim values and remove empty strings;
- de-duplicate each quick-choice set while preserving a deterministic order;
- when a current value is non-null, include it in its corresponding quick-choice set so the record
  never points at an unavailable quick choice;
- capability gates still reject an axis the harness cannot drive;
- an Antigravity effort without a current model remains structurally invalid;
- empty quick-choice sets with null current values are valid for attached and pre-feature agents;
- do not validate catalog membership or per-model effort values in core.

All mutations replace the complete selection configuration with one registry rewrite. Sidebar model
changes may also need to change effort for Antigravity, so separate `set model` then `set effort`
writes are forbidden: a send could otherwise snapshot an invalid intermediate pair.

### Persisted per-harness defaults

Settings stores, per harness:

- selected model quick choices and a default model;
- selected effort quick choices and a default effort.

The default is always a member of its selected set. The one-choice implicit-default rule is a UI
rule, not a different persisted shape: the single selected value is still stored as the default so
creation and auto-seeding have one simple contract.

### Antigravity model-change resolution

Frontend catalog lookup represents three states rather than overloading an empty effort list:

- known effort levels;
- explicitly known to have no effort axis;
- unknown model.

The shared frontend selection logic resolves a requested Antigravity model change in this order:

1. If the new model is explicitly known to have no effort axis, set the current effort to `null` and
   hide the effort chip.
2. If the new model is unknown, preserve the current effort and configured choices rather than
   inferring that the model has no effort axis; reactive harness validation remains authoritative.
3. If the current effort is valid for the new model and is still in the agent's effort quick-choice
   set, preserve it.
4. Otherwise use the first configured quick effort valid for the new model, in the existing curated
   effort-option order.
5. If an effort is required and no configured quick effort is valid, refuse the switch in the UI
   with an inline/tooltip explanation and direct the user to `Model settings…`; do not persist an
   invalid pair and do not silently add a quick choice.

Effort activation uses the mirror rule for a known current model: its activatable set is the
intersection of configured effort choices and the effort levels valid for that model. Presentation
mode still uses the configured set's size so the chip does not change shape merely because the model
changes. An incompatible two-choice target or 3+-choice menu item is disabled and explained; the
current model is never changed as a side effect of effort activation. For an unknown model, all
configured effort choices remain activatable and reactive harness validation remains authoritative.

Settings and the create-agent editor must prevent saving a configuration whose default Antigravity
model requires effort but has no valid selected effort. The existing-agent editor applies the same
rule to its current model/current effort, including when an attached agent starts with empty sets.
Settings must refuse an interaction that would create an invalid default pair before mutating its
optimistic local preference state; merely skipping that one save is insufficient because a later
unrelated whole-preferences save could persist the invalid pair. These editors need not require every
selected model to have a compatible selected effort: a currently unusable quick model can remain
selected and is disabled/explained in the sidebar until the user adds a compatible effort.
Automatically modifying the effort quick-choice set when a model is selected is intentionally
avoided because it would make the supposedly independent multi-select controls surprising.

The resolution order is non-obvious and must survive in a concise comment/docstring beside the
shared resolver, including the unknown-versus-explicit-no-effort distinction, why Settings is not an
input for existing agents, and why core does not duplicate the catalog.

### Backward migration

Read old records and preferences, but write only the new shape. No standalone migration command or
schema-version framework is warranted for this local, structurally derivable change.

Legacy registry discrimination is presence-based. A row containing `profiles` is the bundled-profile
shape, because its flat `model`/`effort` fields represent Primary. A row without `profiles` and
without the new choice arrays is the older pre-profile shape; each non-null flat current value seeds
a one-element choice set. New serialization always emits the choice arrays and never emits
`profiles`. This rule must remain documented at the serde boundary so a future field addition cannot
silently reinterpret stored rows.

For an old bundled-profile agent record:

- model choices are the stable de-duplicated non-null union of Primary then Secondary model;
- effort choices are the stable de-duplicated non-null union of Primary then Secondary effort;
- current model and effort come from the formerly active profile;
- if the old active slot names a missing Secondary profile, fall back to Primary, matching the old
  `active_profile()` behavior rather than manufacturing an error during upgrade;
- a primary-only record becomes one choice on each populated axis;
- an attached/unpinned record with null values becomes empty choice sets and null current values;
- off-catalog strings are preserved exactly after whitespace normalization.

For old per-harness Settings:

- choices are the same Primary-then-Secondary non-null unions;
- Primary supplies the default for each populated axis;
- if Primary is null but Secondary contains a value, use that sole/first migrated choice as the
  default;
- if an axis has no migrated value, restore that harness's built-in default for the axis, because
  Settings must always be capable of creating a configured agent;
- preserve unknown harness keys and unknown nested preference keys through the existing
  opaque-load/recursive-save behavior.

The recursive YAML merge preserves keys absent from the new serialization, so the save path must
explicitly delete the known obsolete `primary` and `secondary` keys from each recognized harness
mapping before merging the new fields. This targeted deletion must not remove unknown sibling keys,
unknown harness entries, or unrelated top-level preferences. Its rationale belongs beside the merge:
known retired schema is removed while future data remains lossless.

Newly appended registry rows use only the new shape, and obsolete profile keys disappear from all
rows the next time the registry is rewritten by an agent mutation. The next preferences save removes
its obsolete keys. Delete the old profile types, commands, errors, helpers, tests, and user-facing
copy in the same feature rather than retaining two in-memory or API representations indefinitely.

---

## Milestone 1 — Backend selection contract and persistence migration

### Goal & Outcome

Replace profile slots with independent quick-choice sets across the core, preferences, app commands,
and dispatch snapshot boundary while keeping existing registries and preferences readable. These
backend changes form one atomic implementation milestone: its ordered components may temporarily
break downstream compilation, so the workspace-wide gate applies after all three are complete rather
than after an intermediate incompatible type deletion.

- New and existing agents have separate current model, current effort, model choices, and effort
  choices.
- Old Primary/Secondary registry rows load into the new representation without losing the active
  selection or either old option.
- Invalid structural state is rejected before a registry write.
- Forking carries the full new configuration.
- Preferences durably store choices plus defaults and remove their obsolete profile fields on write.
- Creation, replacement, attach, and dispatch use one coherent backend contract.
- New registry writes contain no profile or active-slot fields.

### Implementation Outline

Implement the following components in order. Do not retain a transitional dual representation merely
to make the workspace compile between components; complete the milestone before applying its full
gate.

#### A. Core agent model and registry compatibility

1. Change the core agent representation to the domain contract above. Use presence-aware explicit
   compatibility deserialization: `profiles` present means the bundled-profile shape whose flat
   `model`/`effort` values represent Primary; `profiles` and the new choice arrays absent means the
   pre-profile flat shape. A plain serde default would silently select the wrong values for agents
   that were active on Secondary.
2. Centralize normalization and validation at the existing project persistence chokepoint. Apply it
   identically during registration and later replacement so a direct core caller cannot bypass the
   invariants.
3. Replace profile-specific registration and mutation inputs with the complete independent
   selection configuration. Keep one atomic mutation for all four fields. Remove the active-slot
   mutation entirely.
4. Update fork registration to clone both quick-choice sets and the current pair. Preserve every
   existing fork lifecycle and session-locator rule; this milestone changes only inherited
   selection state.
5. Make attach registration construct the valid empty/null state internally. Remove selection
   arguments from the Claude and Codex attached-registration signatures so a direct core caller
   cannot create a pinned attachment. Correct stale comments that still claim a currently supported
   harness cannot select model/effort.
6. Remove profile-only errors once no code path can request an absent Secondary slot. Retain the
   capability and effort-without-model errors that still describe real structural failures.

The compatibility-deserialization rationale must remain documented beside the serde boundary. It
is not obvious from the final new struct why the legacy flat values cannot be used directly.

#### B. Preferences and default migration

Give Settings one durable, backward-compatible source of truth for the quick choices and defaults
used to create agents.

- Every harness has model choices/default and effort choices/default in preferences.
- Existing Primary/Secondary preferences migrate predictably.
- The single-choice default remains explicit in data even though its UI control will be disabled.
- Future/unknown preference data continues to survive a load/save cycle.

1. Replace the per-harness Primary/Secondary preference shape with the defaults contract above and
   update the built-in values by mechanically translating today's built-in profiles: Primary values
   become defaults and the Primary/Secondary unions become selected choices.
2. Extend the existing custom preference deserialization to recognize and translate the old shape.
   Preserve its current posture: missing/corrupt preferences degrade to built-ins, missing harnesses
   are filled, recognized strings are normalized, and unknown YAML survives recursive saves.
3. Normalize each axis atomically. The resulting selected set must be non-empty, de-duplicated, and
   contain its default. A manually edited invalid/missing default falls back to the first migrated
   selected value; an empty axis falls back to the harness built-in.
4. Keep default preferences independent from existing agent records. Loading or saving preferences
   must not walk registries or rewrite agents.
5. Update the frontend preference mirror and fallback constants to exactly match the backend wire
   shape. There must not be separate frontend and backend interpretations of what the default means.
6. Before recursively merging a recognized harness mapping, delete only its obsolete `primary` and
   `secondary` keys. Then merge the new fields normally so unknown sibling keys, unknown harnesses,
   and unrelated top-level preferences continue to survive.

The backward YAML preservation behavior is load-bearing and should retain an explanatory comment:
model catalogs and preference shapes will change over time, saving a known field must not erase
future fields written by a newer Switchboard, and the targeted legacy-key deletion is deliberately
narrower than the otherwise lossless merge.

#### C. App commands, creation, attach, and dispatch snapshots

Expose one atomic backend API for independent selections and preserve the exact send-time behavior
users already rely on.

- Agent creation persists quick choices and the starting current model/effort in one operation.
- Editing or quick-switching replaces the complete selection state atomically.
- A newly submitted send captures the current model and effort independently.
- In-flight and queued sends are unaffected by later changes.
- Attach cannot accept or persist a selection override.
- Profile-specific IPC commands and wire types no longer exist.

1. Replace the create command's primary/secondary arguments with one typed selection-configuration
   payload matching Milestone 1. Avoid another positional collection of adjacent model/effort/list
   arguments; the API should make transposition difficult and serve creation, state caching, and
   tests consistently.
2. Replace `set_agent_profiles` and `set_active_agent_profile` with one atomic command that accepts
   the complete new configuration and returns the updated record. Keep the current registry-write
   serialization and shared-cache replacement behavior.
3. Update dispatch-context selection capture to read the current model and current effort directly.
   Preserve the existing timing: capture occurs when the send is accepted, not when a queued turn
   eventually starts.
4. Remove model/effort selection arguments from the frontend attach API, Tauri command wrapper,
   `attach_agent_impl`, and the core attached-registration methods. Attach receives the empty/null
   state constructed by core; omitting fields only in the form is not sufficient enforcement.
5. Audit non-UI constructors and fixtures, including workflow/app test records, so none silently
   fabricate the old profile shape.
6. Remove old Tauri registrations, API wrappers, exports, and the Secondary-missing error path in
   the same milestone. Do not retain deprecated aliases; the app and frontend ship together.

No harness adapter should need a behavioral change: adapters already receive one model and effort
for a turn. If implementation reaches into adapter argument construction beyond type fallout, stop
and verify that scope against `harness-behavior.md` before proceeding.

### Definition of Done

#### Core and registry

- Core unit tests cover new-shape serialization/round-trip with one, two, and 3+ choices; stable
  de-duplication; whitespace normalization; and current-value inclusion.
- Fixture-style deserialization tests cover old primary-only, active-Primary with Secondary,
  active-Secondary, duplicate values across profiles, null/unpinned, missing-Secondary-with-active-
  Secondary, and off-catalog values. Include both a pre-profile row with only flat `model` and no
  `profiles` or choice arrays, and a bundled-profile row whose active profile has null effort while
  its inactive profile supplies the sole effort choice. Assert both the current pair and choice sets.
- A serialization assertion proves a migrated record writes no `profiles`, `primary`, `secondary`,
  or active-slot field and always emits both choice arrays.
- A normalization test proves a current value omitted from its submitted quick choices is added to
  the set deterministically. A rejection test proves Antigravity effort without a model leaves the
  registry byte-for-byte/logically unchanged. Preserve the capability-gate tests where the current
  harness enum makes an unsupported branch representable; do not fabricate a fake harness only for
  coverage.
- Fork tests prove the fork inherits choices and current values without sharing mutable state or
  changing the existing fork provenance/session behavior.
- Core attach tests prove all supported harness paths remain unpinned with empty choices and null
  current values.
- Existing profile-specific tests are rewritten around behavior or deleted; no compatibility-only
  production API remains.

#### Preferences

- Preference tests cover default construction for every harness and exact migration from today's
  built-in Primary/Secondary examples.
- Migration tests cover identical Primary/Secondary values (one de-duplicated choice), Primary null
  with Secondary populated, an absent axis, whitespace/duplicates, and an off-catalog value.
- Normalization tests prove default membership, non-empty configured axes, and deterministic fallback
  from a missing/invalid default.
- An exact legacy YAML fixture proves `primary` and `secondary` disappear from every recognized
  harness on save while unknown sibling keys, unknown harnesses, and unrelated top-level data
  survive with the same meaning.
- Frontend preference load/reset tests prove fallback values are cloned rather than shared and match
  the new wire shape.

#### Commands and dispatch

- Command tests prove create and replace update both choices and the current pair in the registry and
  shared cache, with persistence failure leaving neither half updated.
- Direct IPC-boundary tests cover blank/duplicate normalization and structural validation errors.
- A queue regression test submits work under model A/effort High, changes only effort, submits again,
  changes only model, and proves each accepted/queued turn runs with the pair captured at its own
  submission. Include an Antigravity-shaped atomic model+effort adjustment case so no intermediate
  invalid pair can be observed.
- API invocation-shape, app-command, and core registration tests prove every supported attach path
  accepts no selection override and stores null current values with empty choice sets.
- A repository-wide search finds no callable `set_agent_profiles`, `set_active_agent_profile`,
  `AgentProfileSlot`, or equivalent Primary/Secondary execution API outside legacy deserialization
  fixtures.
- Existing adapter and live-test behavior remains unchanged; no new quota-spending live test is
  required because no CLI contract changes.
- `make fmt`, `make lint`, and `make test` pass after all three backend components are complete.

---

## Milestone 2 — Shared frontend selection model and editor

### Goal & Outcome

Establish one frontend interaction model that Settings, Add Agent, and the existing-agent dialog can
reuse without drifting.

- Curated options can be independently included/excluded per axis.
- A context-specific selector shows `Default`, `Start with`, or `Current` as appropriate.
- `Default` is interactive only with 2+ selected choices; an existing agent can still adopt a sole
  configured choice when its current value is null or different.
- Antigravity transitions resolve through one tested function.
- Effort controls never activate a value invalid for the current known Antigravity model.
- Off-catalog and unpinned agents remain editable without silent mutation.

### Implementation Outline

1. Replace profile-oriented frontend types/helpers with the Milestone 1 wire type plus pure helpers
   for normalization, display labels, option ordering, presentation mode, default/current validity,
   Antigravity's three catalog-knowledge states, model-change resolution, and the activatable effort
   intersection for a current known model.
2. Replace the profile editor with a shared independent-axis editor. Each axis contains:
   - multi-select toggle controls over the existing curated options;
   - an explicit context label (`Default`, `Start with`, or `Current`);
   - a single-value selector restricted to the selected quick choices;
   - a non-interactive implicit-default presentation when exactly one choice is selected;
   - in existing-agent context, an explicit way to adopt the sole choice when it differs from the
     current value, including a null current value migrated from an old or attached agent.
3. Use real multi-select semantics (`button` + `aria-pressed` or checkbox semantics), not the current
   radio-group primitive. Reuse the existing semantic color classes so selected choices look like
   selected controls, and communicate default/current with text and accessible state rather than a
   color-only distinction.
4. Enforce the zero/one/many behavior in the shared editor:
   - Settings/create configuration cannot deselect the last choice on an axis; an existing agent
     may begin empty because it was attached, but once the user configures an axis its last choice
     is likewise not removable through this UI;
   - the toggle for the current default/start/current value cannot be turned off while it owns that role;
     the user first chooses another selected value in the explicit selector, then may remove the old
     value. Give the locked control accessible disabled semantics and a tooltip or helper explaining
     that another value must assume the role before removal. This avoids an arbitrary automatic
     replacement and never leaves an invalid intermediate;
   - selecting the first choice for an empty attached-agent axis makes it current;
   - with one choice equal to the current/default value, the selector cannot be focused or changed
     because there is no decision to make;
   - with one choice different from an existing agent's current value, the Current control allows
     adopting that choice; this exception does not make a one-choice Settings Default interactive;
   - with 2+ choices, the selector becomes enabled and lists only selected choices. For a known
     Antigravity model, effort values outside the activatable intersection are disabled and explained.
5. Preserve an off-catalog current value by including it in the editor's selected choices and
   presenting its full string. Do not silently replace it with the first curated option merely by
   opening or saving the dialog.
6. Keep `null` as the real "no override" value for attached/pre-feature agents, not a fake catalog
   entry. Opening and closing an untouched agent editor must round-trip that state unchanged.
7. In existing-agent context, prevent save when the current Antigravity model is known to require
   effort but the current effort is null or incompatible. This includes an attached agent whose
   quick-choice sets began empty; explain the required correction inline rather than allowing the
   next send to fail.

The editor may be parameterized for its three contexts, but do not build a generic preference-form
framework. Its shared responsibility is only model/effort quick choices and the associated selected
value.

### Definition of Done

- Pure helper tests cover one/two/3+ choice presentation, catalog ordering, stable off-catalog
  inclusion, de-duplication, and the one-choice implicit selector state.
- Antigravity resolver tests cover: preserve a valid effort; use the first configured valid effort;
  explicitly known no-effort model clears effort; required-effort model with no compatible
  configured choice refuses without mutation; unknown model preserves effort; and Settings changes
  are not an input to existing-agent resolution.
- Effort-side helper tests prove only the configured/valid intersection is activatable for a known
  model while presentation mode still reflects the complete configured set.
- Shared-editor component tests cover keyboard and pointer toggling, accessible pressed/checked
  state, inability to remove the last Settings/create choice, default/current membership, first
  selection from an empty attached state, sole-choice adoption when current is null, the one-choice
  implicit Default rule, incompatible effort explanations, and accessible explanation for a locked
  current/default choice.
- Component tests prove opening/saving an untouched null/off-catalog configuration changes nothing.
- Keep the toggle implementation scoped to the shared editor unless implementation reveals an
  independent reuse boundary that warrants a low-level UI primitive. No new raw palette colors or
  hand-rolled menu behavior are introduced.
- `make fmt`, `make lint`, and `make test` pass before proceeding.

---

## Milestone 3 — Settings, Add Agent, auto-seeding, and naming

### Goal & Outcome

Apply the shared editor to every creation/default surface so new agents receive predictable,
independent quick choices.

- Settings lets the user choose one or many models and efforts per harness and choose defaults only
  when a real choice exists.
- Add Agent begins from those defaults, allows per-agent customization, and submits one atomic
  independent configuration.
- New-project auto-seeding uses the same persisted defaults.
- Attach remains behaviorally unpinned, and its API accepts no selection configuration.
- Automatic agent names remain truthful when an agent can switch either axis.

### Implementation Outline

1. Replace the Agent Defaults profile copy with two quick-choice sections per harness. The explanatory
   copy should say these are initial choices for new agents/projects and that existing agents are not
   changed.
2. Render multi-select choices using the shared editor. When one value is selected, show it as
   implicitly `Default` and keep the default control disabled/non-interactive. When 2+ are selected,
   enable the explicit default selector. Save the complete per-harness defaults atomically through
   the existing optimistic preference path; never persist a transient missing default between two
   clicks. For Antigravity, refuse a model or effort interaction that would make the default pair
   invalid before mutating local preference state. Do not rely on suppressing only that save because
   a later unrelated whole-preferences update would otherwise carry the invalid pair.
3. In create mode, seed the editor from the selected harness's preferences. Label the chosen values
   `Start with`, not `Default`, because the submitted agent stores current selections. Preserve the
   existing rules that switching harness resets harness-specific configuration, while a temporary
   create→attach→create mode change preserves the user's create draft.
4. Continue hiding all selection controls in attach mode and submitting no selection payload. Do not
   use the existence of global quick choices as permission to pin an imported session.
5. Update automatic naming: if either axis has more than one quick choice, use the stable harness
   name because a model/effort-derived name would become stale after a switch. If both axes have at
   most one choice, retain the current model/effort-derived naming behavior. As today, once the user
   edits the name manually, later selection or harness changes never overwrite it.
6. Update new-project seeding to copy the complete default choice sets and start on the explicit
   default model/effort. For Antigravity, resolve the starting effort against the default model using
   the shared rules; Settings should normally guarantee this path is valid, while seeding still
   reports a clear creation failure rather than panicking on manually corrupted preferences.

### Definition of Done

- Settings component tests cover one, two, and 3+ selected choices; the default selector disabled at
  one and enabled at 2+; changing defaults without changing membership; preventing the last option's
  removal; atomic preference payloads; save failure copy; and every harness accordion.
- Settings tests cover invalid Antigravity default-pair interactions and prove they are explained,
  do not mutate local preferences, issue no save, and cannot leak through a later unrelated settings
  change.
- Create-form tests prove preferences prefill both choice sets and starting values, per-agent edits
  submit exactly, harness changes reset correctly, and create/attach draft behavior is preserved.
- Attach tests prove the controls stay hidden and no default/selection fields are submitted.
- Auto-seeding tests prove every installed harness receives copied choices and starts on the
  configured defaults, including an Antigravity valid pair.
- Naming tests cover one choice on both axes, multiple models only, multiple efforts only, multiple
  on both, harness switching, and the manual-name freeze.
- Existing preference-loading gates and non-dismissible creation/save behavior remain covered.
- `make fmt`, `make lint`, and `make test` pass before proceeding.

---

## Milestone 4 — Existing-agent dialog and adaptive sidebar controls

### Goal & Outcome

Deliver the independent quick-switch interaction on existing agents without losing the current
one-click two-option workflow.

- The agent card shows separate model and effort chips for future-send intent.
- Two choices toggle in one click; 3+ choices open a direct menu; one choice is static unless it is
  the sole configured value that differs from the current value and therefore needs an adoption path.
- Model and effort can be switched independently.
- Antigravity model changes visibly and atomically adjust effort only when required.
- The existing `Model settings…` dialog edits the agent's own choices/current state.
- No user-facing Primary, Secondary, or profile terminology remains.

### Implementation Outline

1. Replace the combined `model · effort` line and profile swap button with two compact chips using
   friendly catalog labels; retain the raw/full value in the existing tooltip treatment when useful.
   Both chips need axis-specific accessible names (`Current model …`, `Current reasoning effort …`).
2. Give each chip an explicit presentation based on its configured quick choices:
   - one choice equal to current: static metadata styling, no button semantics;
   - one choice different from current, including null current: button semantics that adopt that
     exact choice in one click, then return the chip to static presentation;
   - exactly two: button with swap affordance; activation switches to the other value immediately,
     and its tooltip/accessibility label names the exact target;
   - three or more: menu button with a chevron, using the existing dropdown primitives; the current
     item is identified and choosing an item closes the menu and saves it.
3. Keep each chip's interaction axis-local. An effort activation preserves model. A model activation
   preserves effort except for Antigravity resolution. For an Antigravity choice that requires an
   adjustment, show the resulting effort in the model menu/tooltip before activation (for example,
   `GPT-OSS 120B — effort becomes Medium`). Disable and explain a model whose required effort has no
   compatible configured quick choice. For a known current Antigravity model, disable and explain an
   effort target outside the configured/valid intersection; do not persist it and do not change the
   model. Keep the chip's 1/2/3+ presentation based on configured choices so unavailable targets do
   not make the control unpredictably change shape.
4. Submit the full resolved state through the atomic command and replace the roster record only on
   success. The complete selection configuration is the busy unit: disable both chips and the
   `Model settings…` action for that agent until the request settles so two full-state writes cannot
   race from the same stale record and revert each other. Other agents remain independently usable.
   Surface a failure on that agent card, retain the last acknowledged record, and re-enable all three
   controls after failure.
5. Adapt `Model settings…` to the shared editor's existing-agent context. It edits choices and
   `Current` values, preserves null/off-catalog state, blocks a known invalid Antigravity current
   pair with inline correction copy, and keeps the dialog open/non-dismissible during save exactly
   as today.
6. Hide the effort chip only when the current Antigravity model is explicitly known to have no
   effort axis. An unknown model preserves and displays configured/current effort rather than being
   treated as no-effort. If the current model or effort is null, retain `Harness/session default` as
   subdued intent copy until the user makes a selection; do not substitute observed transcript
   metadata for a current selection.
7. Remove profile terminology from README copy, tooltips, test names, component names where they are
   genuinely profile-specific, comments, and system-design. Do not rename unrelated uses of
   `primary`/`secondary` such as Codex quota windows or button variants.

The adaptive 1/2/3+ behavior and its visual distinction must survive in comments beside the sidebar
presentation helper: the two-choice special case exists to preserve the current one-click workflow,
while the menu avoids arbitrary cycling for larger sets.

### Definition of Done

- Sidebar component tests cover both axes independently for one, two, and 3+ choices, including
  static semantics, sole-choice adoption from null/different current, exact two-choice target copy,
  direct menu selection, keyboard activation, and busy/failed saves.
- A regression sequence starts at Model A/Effort High, switches only effort, then only model, and
  proves each chip and backend payload preserves the untouched axis.
- A deferred-promise regression attempts rapid cross-axis changes from the same record and proves
  the second interaction is unavailable until the first whole-configuration save settles; success
  and failure both restore the complete control group without losing an acknowledged change.
- Antigravity component tests cover valid-effort preservation, announced automatic adjustment,
  explicitly known no-effort model hiding the chip, unknown model preserving effort, disabled
  incompatible model and effort targets with explanations, and a single atomic persistence call.
- Existing-agent dialog tests cover adding/removing choices, refusal to remove the current choice
  until another is explicitly made current, one-choice Current adoption versus one-choice implicit
  Default, null/unpinned round-trip, off-catalog preservation, required-effort validation for an
  attached Antigravity agent, and save failure.
- Transcript tests continue to prove historical per-turn model/effort render independently from the
  sidebar's current values.
- A focused browser test is required only if the final two-chip layout or 3+-choice menu introduces
  behavior dependent on measured WebKit geometry (clipping, overflow, anchoring). Do not add a
  browser test for interactions fully covered by jsdom.
- Update `README.md`, `docs/system-design.md`, and `docs/harness-behavior.md` to describe independent
  model/effort quick choices and preserve the selected-intent versus observed-history distinction.
- A repository-wide search finds no user-facing `Primary profile`, `Secondary profile`, `secondary
  configuration`, or `switch profile` copy. Unrelated primary/secondary vocabulary remains intact.
- Run `make check` in the foreground and wait for completion. Report it as passing only if the full
  target completes. No live suite is required because adapter/harness behavior is unchanged.

---

## Explicitly out of scope

- Fetching model catalogs from harnesses or validating account/plan availability.
- Arbitrary user-authored model strings beyond preserving already persisted off-catalog values.
- Named profiles, saved combinations, presets, favorites, ordering/reordering choices, or keyboard
  shortcuts for model/effort switching.
- A reset-existing-agent-to-global-defaults action.
- Applying changed global defaults retroactively to existing agents.
- Showing defaults on the agent card; the sidebar shows current future-send intent only.
- Changing an in-flight or already queued send's captured selection.
- Backend duplication of Antigravity's per-model effort catalog.
- Any transcript or harness-adapter protocol change.

## Delivery discipline

- Implement milestones in order because later wire/UI work depends on the new core and preference
  contracts.
- Keep the feature in one PR, with milestone-sized commits if commits are requested. Do not commit or
  push unless explicitly authorized.
- Run the repository's `make` targets exactly as documented in `AGENTS.md`; do not substitute
  hand-rolled Cargo flag sets.
- After each milestone, inspect the diff for stale profile terminology and accidental edits to
  unrelated `primary`/`secondary` concepts. The final repository-wide search is a guard, not a
  license for blind replacement.
