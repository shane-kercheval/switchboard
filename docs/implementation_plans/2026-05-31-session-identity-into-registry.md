# Consolidate session-locator identity into the agent registry

Move every harness's session-locator identity (the IDs Switchboard uses to find and resume a harness's conversation) out of separate per-agent **session-link sidecar files** and into the **agent registry record**, then delete those sidecars. This removes a class of file whose only reason to exist was a workaround for an append-only registry, kills the inconsistent `<agent-id>.antigravity.jsonl` vs `<agent-id>.jsonl` naming, and makes session identity fsync-durable like the rest of the registry.

**Executes before** [`2026-05-31-per-message-metadata-and-context-formula.md`](2026-05-31-per-message-metadata-and-context-formula.md) — that plan's Milestone 4 adds a new per-agent file, and it should land in the cleaned-up `sessions/` structure this plan produces.

## Why this exists (the reframe)

Two harnesses pre-generate their session ID at agent creation and store it directly on the `AgentRecord` (Claude UUID v7, Gemini UUID v4) — **no sidecar**. Two harnesses (Codex, Antigravity) have their session ID *assigned by the harness at runtime*, so today Switchboard captures it inside the adapter's producer task and writes it to a per-agent append-only JSONL sidecar (`<agent-id>.jsonl` for Codex, `<agent-id>.antigravity.jsonl` for Antigravity).

That split exists for one reason: the registry is written append-only, and "update a record after creation" wasn't built — so the runtime-discovered ID got punted to a sidecar that *could* be appended. That's circular: **the sidecar is a workaround for a limitation the registry imposes on itself.** The session ID is agent *identity* — it belongs in the registry regardless of *when* it's learned. The governing rule (established here) is **the nature of the data, not its acquisition time**: consolidated identity → registry; temporal/per-turn telemetry → a sidecar. Session locators are identity.

The decisive facts (from the code audit):
- The registry is **local, gitignored runtime data** (`.gitignore` excludes `.switchboard/`; system-design §3 lists `registry.jsonl` as local runtime). So there's no "don't pollute a shared/committed file with machine-local IDs" reason to keep them out.
- The session-link sidecars are read **only via `read_latest`** — nothing consumes their append history. The per-dispatch log is debug-only; deleting it loses no functional data.
- Claude/Gemini already prove the target pattern: session ID lives wholly on the `AgentRecord`, and the adapter receives it as *input* to build resume args. Codex/Antigravity are the outliers doing their own persistence.

What this is **not**: it is not building agent reordering or a general registry-migration framework (out of scope — see below). It only adds the update-in-place the session-capture path needs; that the same op also unblocks future reordering is a bonus, not a goal.

## Required reading (before implementing)

- **The current persistence surface** — read these in full before changing anything:
  - `crates/core/src/agent.rs` (`AgentRecord`), `crates/core/src/project.rs` (register / remove / rename / list, and the per-harness `session_id` assignment), `crates/core/src/io.rs` (`append_jsonl` / `write_jsonl` / `read_jsonl` — note `write_jsonl` is already atomic temp+rename+fsync), `crates/core/src/paths.rs`.
  - `crates/harness/src/codex/sidecar.rs` + the write/read call sites in `crates/harness/src/codex/mod.rs` (capture on `thread.started`; resume `build_args`; post-terminal enrichment `emit_terminal_with_enrichment`).
  - `crates/harness/src/antigravity/sidecar.rs` + call sites in `crates/harness/src/antigravity/mod.rs` (conversation-id capture; resume `--conversation`).
  - `crates/harness/src/claude_code/mod.rs` + `crates/harness/src/gemini/mod.rs` `build_args` (the target pattern: locator read from `AgentRecord`).
  - The attach flows in `crates/app/src/commands.rs` (Codex/Antigravity write the sidecar *before* the registry commit; Claude/Gemini write only the registry) and `AppState` (`crates/app/src/state.rs`: `registry_write` mutex, `agents_by_id` cache).
  - The dispatcher's injected-sink pattern: `MetadataCache` / `ConversationJournal` traits and how `DispatchContext` carries them (`crates/dispatcher/src/lib.rs`). **This is the pattern the capture path reuses — do not invent a parallel one.**
- `docs/system-design.md` §3 / §3.2 (source-of-truth split; directory layout) and §10.3 (the resolved persistence-schema note that documents today's sidecar layout — this plan supersedes its sidecar half).
- `docs/research/same-session-parallel-invocation.md` (session-id uniqueness is enforced at the app layer — preserve it).
- `AGENTS.md` "Filesystem layout" + the "Switchboard-owned JSONL fails loud on corruption" invariant.

## Decisions established here (reused across milestones)

- **One session-locator field on `AgentRecord`, modeled as a harness-shaped enum.** Replace `session_id: Option<Uuid>` with a single optional locator whose shape is honest about reality: most harnesses identify a session by one UUID; Codex needs a string thread-id **plus** a local partition-date. Recommended model:

  ```rust
  pub enum SessionLocator {
      /// Claude, Gemini, Antigravity — a single session UUID
      /// (pre-generated at creation, or captured at runtime).
      Uuid(Uuid),
      /// Codex — the runtime thread-id (a String, NOT guaranteed a UUID)
      /// plus the LOCAL date its rollout file is partitioned under.
      Codex { thread_id: String, partition_date: NaiveDate },
  }
  ```

  Chosen over flat per-harness columns (`codex_thread_id`, `codex_partition_date`, …): those reintroduce exactly the harness-special-casing this refactor removes and allow invalid half-set states. The enum makes "what identifies this agent's session" one well-typed place and makes invalid states unrepresentable. (The agent should verify whether Codex's `thread_id` is ever a bare UUID; it does not change the model — the `Codex` variant carries a `String` regardless, because the partition-date must ride with it.)

- **The adapter interface converges to: locator *in*, capture *out*.** Every adapter receives the agent's current `Option<SessionLocator>` as dispatch input (Claude/Gemini already do, via the `AgentRecord`). Adapters that learn their locator at runtime (Codex, Antigravity) **stop doing their own sidecar I/O** and instead **emit a normalized capture event** carrying the locator. Claude/Gemini always have a locator and never emit one. After this, no adapter reads or writes a session-link file.

- **Capture persistence reuses the dispatcher's injected-sink pattern, but is load-bearing — not best-effort.** The dispatcher, on a capture event, persists the locator to the registry through an injected sink (parallel to `MetadataCache`/`ConversationJournal`, supplied by the app's `DispatchContext` factory). Unlike the metadata cache, a persist failure on capture must fail the turn (synthesize `AdapterFailure`), preserving today's Codex behavior (a lost locator means the next turn starts a fresh session and silently drops context). The adapter emits a capture event **whenever it learns or changes the locator** — first capture, and the Antigravity fork-and-heal case where a resume's conversation no longer exists server-side and `agy` mints a fresh one. Normal resumes (locator unchanged) read it from the registry and emit nothing; the per-dispatch re-append today's sidecars do is dropped with no functional loss. The rule is **persist on change, not "persist exactly once"** — a literal once-only rule would drop the fork-and-heal re-capture and leave the agent re-forking (and losing context) every turn.

- **Attach writes the registry inline; only runtime capture uses the event path.** The attach commands run in the app and already hold registry access, so they write the locator directly into the registry record (no event, no sidecar). This also removes the current "sidecar-before-registry" ordering dance.

- **The captured locator must reach the *next* dispatch's input.** The app's `DispatchContextFactory` currently freezes an `AgentRecord` clone at construction, so a locator persisted mid-turn would be invisible to the next turn (the agent would re-create its session every turn). The factory therefore reads the agent's **current** record live from the shared `agents_by_id` cache at `build()` time — the same live-read pattern it already uses for `needs_session_meta` — and the capture sink updates `agents_by_id` alongside the registry. This requires promoting `agents_by_id` and `registry_write` from `Mutex<…>` to `Arc<Mutex<…>>` (so the factory/sink can hold handles), mirroring `needs_session_meta`; `lock(&state.x)` keeps compiling via deref coercion. The `agents_by_id` doc invariant "`AgentRecord` is immutable after registration, so a cached copy never goes stale" is no longer true (`session_locator` mutates in place) and is rewritten accordingly.

- **`session_partition_date` invariant is preserved.** Codex's partition-date is captured **once** (local date at first dispatch) and **never recomputed** — it rides inside the `Codex` locator variant and is read back for both resume and post-terminal enrichment. The enrichment path (in the harness crate, which has no registry access) must receive the locator as **input** from the app/dispatcher rather than reading a file.

- **No functional consumer of sidecar history → safe to delete.** Confirmed: only `read_latest` is used. Deleting the sidecar modules loses debug-only history.

---

## Milestone 1 — The locator model + registry update-in-place (core; Claude/Gemini migrate)

### Goal & Outcome

`AgentRecord` carries a single harness-shaped `SessionLocator`, the registry supports updating one agent's locator in place, and the two pre-generating harnesses (Claude, Gemini) are migrated onto the new field with no behavior change. No sidecars are touched yet; this milestone establishes the model and the mutation op that Milestones 2–3 reuse.

Outcomes:
- An agent's session locator lives on its `AgentRecord` and round-trips through `registry.jsonl`.
- A new registry op sets one agent's locator in place (full-rewrite under the existing `registry_write` lock + in-memory `agents_by_id` cache update) — the mechanism later used to persist a runtime capture.
- Claude and Gemini behave exactly as before (pre-generated UUID → `SessionLocator::Uuid`; same resume/attach/hydration outcomes).
- No backwards-compatibility shim — this is a pre-release product with no external users. Existing on-disk registries are migrated by the final milestone (M4).

### Implementation Outline

- **Model.** Introduce `SessionLocator` (in core, beside `AgentRecord`). Replace `AgentRecord.session_id: Option<Uuid>` with `session_locator: Option<SessionLocator>`. Claude registration produces `Some(SessionLocator::Uuid(Uuid::now_v7()))`; Gemini `Some(SessionLocator::Uuid(Uuid::new_v4()))` (**keep v4** — Gemini's filename prefix collision constraint is load-bearing and pinned by a test). Codex/Antigravity registration stays `None`. Use `#[serde(default)]` on the field so old records missing it deserialize as `None` rather than erroring — this is enough to survive the window between M1 landing and the M4 one-time migration running.
- **Update-in-place op.** Add a core method to set one agent's locator (read registry → replace the matching record's locator → `write_jsonl` rewrite, reusing the existing atomic path). The app calls it under the `registry_write` lock and updates the `agents_by_id` cache in the same critical section, mirroring how remove/rename already serialize. Do **not** build reordering or a generic update API — just this op.
- **Migrate Claude/Gemini reads.** Their `build_args` and attach/hydration read the locator from the `AgentRecord`; switch from the old `session_id` field to pattern-matching `SessionLocator::Uuid`. No logic change — same `--session-id`/`--resume` decisions.

### Definition of Done

- **Core tests:** locator round-trips through `registry.jsonl` for each variant; the update-in-place op replaces only the target agent's locator and preserves others (and order); a record missing `session_locator` deserializes as `None` (the `#[serde(default)]` window). Corrupt-registry fail-loud behavior unchanged.
- **App tests:** the update op runs under `registry_write` and the `agents_by_id` cache reflects the new locator after it returns.
- **Harness tests:** Claude/Gemini `build_args` produce identical args to before for first-turn (`--session-id`) and resume (`--resume`) given an `AgentRecord` with a `Uuid` locator. Existing Claude/Gemini attach tests still pass.
- **Docs:** record the locator model + the "identity → registry, nature-not-timing" rule in the `AgentRecord`/`SessionLocator` docstring.
- **Known limitation (Codex/Antigravity sidecars):** Codex/Antigravity still write sidecars after this milestone (removed in M2/M3) — the field exists but isn't yet populated at runtime for them. State this explicitly so the half-migrated state isn't mistaken for a bug.
- **Known limitation (legacy-record window, accepted):** An existing on-disk record written before this change carries the old `session_id` key. `#[serde(default)]` makes it load without error, but serde ignores the unknown `session_id` key, so the record loads with `session_locator: None` — an existing Claude/Gemini agent loses resume continuity (starts a fresh session) until the M4 migration rewrites its file. This is the accepted consequence of the "no back-compat shim" decision, on the basis that the app is not run against existing projects during M1–M3. **Deliberately not pinned by a positive test** — a test asserting "old `session_id` → `None`" would read as blessing silent session loss as a desired contract; it's an accepted temporary migration-window risk, recovered by M4, documented here instead. (The `#[serde(default)]`-handles-a-missing-field behavior *is* a real forward contract and is tested.)

---

## Milestone 2 — Runtime-capture path + Antigravity convergence (delete its sidecar)

### Goal & Outcome

Establish the "capture → normalized event → dispatcher persists to registry" mechanism, and apply it to Antigravity first (its locator is a single UUID — the simplest convergence). The Antigravity session-link sidecar and its `.antigravity.jsonl` naming are deleted.

Outcomes:
- An Antigravity agent's conversation-id is captured at runtime, persisted to its `AgentRecord` locator, and used for resume on subsequent dispatches — with **no** `<agent-id>.antigravity.jsonl` file anywhere.
- The capture mechanism (event + dispatcher sink + app registry-updater) exists and is harness-agnostic, ready for Codex in M3.
- A first-capture persist failure fails the turn (preserving "a lost locator must not silently start a fresh session"); a logged-out/never-captured agent behaves as today.
- Antigravity attach writes the locator straight into the registry (no sidecar, no pre-gen-id ordering dance).

### Implementation Outline

- **Capture event.** The adapter emits a normalized event carrying the captured locator when it first learns it. Prefer extending an existing session-related normalized event if one is a natural carrier; otherwise add a new `#[non_exhaustive]` variant. The event crosses the harness→dispatcher boundary; it does **not** need to reach the frontend.
- **Dispatcher sink (load-bearing).** Add a trait the dispatcher calls on the capture event to persist the locator, injected via `DispatchContext` exactly like `MetadataCache`/`ConversationJournal`. **Distinction from the metadata cache:** a persist failure on first capture must terminate the turn with `AdapterFailure` (and the child be torn down per the existing cancellation mechanics). The app-side impl performs the M1 update-in-place op under the `registry_write` lock and updates `agents_by_id`. Determine the exact kill/terminate mechanics against the code; the **contract** is "first-capture persist failure ⇒ failed turn," matching today's sidecar-write-failure semantics.
- **Antigravity adapter.** Stop reading/writing the sidecar. Receive the agent's current locator as dispatch input (like Claude/Gemini); if present, resume via `--conversation <uuid>`; if absent, capture the conversation-id from the log/dir (unchanged capture logic) and emit the capture event instead of `persist_sidecar`. **Fork-and-heal re-capture:** when a resume's `--conversation` UUID no longer exists server-side, `agy` mints a fresh conversation; the adapter must emit the capture event with the *new* UUID (today it re-writes the sidecar) so the registry heals to the forked id and the next turn continues it — otherwise the agent re-forks and loses context every turn. The dispatcher persists every capture event (load-bearing on failure); the adapter no longer tracks `sidecar_write_failed` itself (that fatality moves to the dispatcher sink). Normal resumes (locator unchanged) emit nothing.
- **Antigravity attach.** Write the locator into the registry record directly (the conversation-id is a UUID → `SessionLocator::Uuid`); drop the sidecar write and the pre-generated-id ordering.
- **App-side read paths that consumed the sidecar move to `AgentRecord.session_locator`.** Antigravity hydration (`load_agent_transcript_raw`), the sidebar session-info (`agent_session_info_impl`), and the cross-directory uniqueness scan (`check_antigravity_session_id_unique`) read the conversation-id from the sidecar today; they now read it from the record's locator. The uniqueness scan simplifies to scanning `session_locator` (the same source Claude/Gemini already use). `delete_agent_sidecars` drops its session-link deletion (only `.meta.json` remains).
- **Remove the live path, retain a migration-only reader.** Stop the adapter reading/writing the Antigravity sidecar; no new `.antigravity.jsonl` is ever written. **Retain** the legacy read helper (`SessionLinkRecord` + `read_latest`), clearly marked legacy/migration-only — the final milestone consumes it to fold existing sidecars into the registry, then deletes it. Do **not** delete the sidecar module in this milestone.

### Definition of Done

- **Adapter tests (fixture-driven):** first Antigravity dispatch captures the conversation-id and emits the capture event with the right locator; a resume dispatch (locator already on the record) issues `--conversation <uuid>` and emits no capture; no `.antigravity.jsonl` is created. **Fork-and-heal:** adapt the existing `fork_and_heal_recaptures_new_conversation_and_next_dispatch_resumes_it` test — a stale-resume fork now emits a capture event carrying the *forked* UUID (instead of healing the sidecar), and the next dispatch (given a record holding the healed locator) resumes it.
- **Dispatcher tests:** a capture event invokes the sink once with the locator; a sink failure on capture yields a failed turn (`AdapterFailure`); a non-failing capture persists and the turn proceeds; a **second** capture event on the same agent (the fork case) re-invokes the sink and persists the new locator (not dropped as a duplicate).
- **App tests:** the registry-updater sink writes the locator under `registry_write` and updates `agents_by_id`; Antigravity attach writes the locator inline with no sidecar.
- **Cleanup:** the adapter no longer writes `.antigravity.jsonl`; a fresh Antigravity agent creates no session-link file. The only remaining reference to the legacy shape is the migration-only read helper (deleted in the final milestone).
- **Docs:** update system-design §3/§3.2 and `harness-behavior.md` — Antigravity session identity now lives in the registry; new agents write no `.antigravity.jsonl` (existing ones migrate in the final milestone).

---

## Milestone 3 — Codex convergence (delete its sidecar; reuse the M2 mechanism)

### Goal & Outcome

Apply the M2 capture mechanism to Codex — the harder case, because its locator is a thread-id **plus** a never-recomputed partition-date used by both resume and post-terminal enrichment. The Codex `<agent-id>.jsonl` session-link sidecar is deleted.

Outcomes:
- A Codex agent's `thread_id` + `session_partition_date` are captured on first dispatch, persisted to its `AgentRecord` locator, and used for resume and enrichment on subsequent dispatches — with **no** `<agent-id>.jsonl` session-link file.
- The partition-date is still captured once and never recomputed across resumes (cross-day resumes still read the original spawn-date's rollout file).
- Post-terminal enrichment locates `~/.codex/sessions/<date>/rollout-*.jsonl` from the locator passed in, not from a sidecar read.
- Codex attach parses `thread_id` + partition-date from the existing session file and writes them straight into the registry.

### Implementation Outline

- **Codex adapter.** Stop reading/writing the sidecar (`read_latest` pre-dispatch, `try_persist_sidecar` on `thread.started`, and the enrichment `read_latest`). Receive the agent's current locator as dispatch input: if it's a `Codex { thread_id, partition_date }`, build `exec resume <thread_id>` and pass `partition_date` into enrichment; if `None`, capture `thread_id` from the first `thread.started`, set `partition_date = chrono::Local::now().date_naive()` (the existing rule), and emit the capture event with the full `Codex` locator. Preserve: capture is load-bearing (first-capture persist failure fails the turn, as today); partition-date is set once and never recomputed.
- **Enrichment.** `emit_terminal_with_enrichment` takes the locator (thread-id + partition-date) as input instead of reading the sidecar, then calls the existing `session_file::load_with_retry`. The data it needs is the same; only the source changes (passed-in vs file-read).
- **Codex attach.** Keep `find_codex_session_file_for_attach` (it parses `thread_id` from the filename and partition-date from the directory); write the resulting `Codex` locator into the registry record directly; drop the sidecar write and pre-gen-id ordering.
- **App-side read paths that consumed the sidecar move to `AgentRecord.session_locator`.** Codex hydration (`load_agent_transcript_raw`), the sidebar session-info (`agent_session_info_impl`), and the cross-directory uniqueness scan (`check_codex_session_id_unique`) read `thread_id` + partition-date from the sidecar today; they now read them from the record's `Codex` locator. `delete_agent_sidecars` drops its Codex session-link deletion.
- **Remove the live path, retain a migration-only reader.** Stop the adapter reading/writing the Codex sidecar (pre-dispatch `read_latest`, `try_persist_sidecar`, enrichment `read_latest`); no new `<agent-id>.jsonl` is written. **Retain** the legacy `SessionLinkRecord` + `read_latest` for the final migration milestone, marked legacy/migration-only; full deletion happens there.
- **Verify the capture mechanism is genuinely shared** — Codex and Antigravity must persist through the *same* dispatcher event + sink + registry-updater from M2 (different locator variant, same path). No Codex-specific persistence branch; the dispatcher stays free of `match harness`.

### Definition of Done

- **Adapter tests (fixture-driven):** first Codex dispatch captures `thread_id`, stamps `partition_date` from the local date, and emits the capture event with a `Codex` locator; a resume dispatch (locator on the record) issues `exec resume <thread_id>` and emits no capture; enrichment given a `Codex` locator loads the correct date-partitioned rollout file; the partition-date is not recomputed on a simulated cross-day resume.
- **Dispatcher test:** a Codex capture event flows through the same sink as Antigravity's (assert one shared path; no harness branch).
- **App tests:** Codex attach writes the `Codex` locator inline (thread-id + partition-date parsed from the existing file), no sidecar; existing attach failure modes (missing session file, cross-project collision) preserved.
- **Live test (cost-bounded):** a `live_codex_*` test confirming resume reuses the session via the registry-stored locator (real `codex` round-trip), since the resume/enrichment contract depends on real CLI behavior — name it per the `live_<harness>_` convention.
- **Cleanup:** the adapter no longer writes `<agent-id>.jsonl`; a fresh Codex agent creates no session-link file. The only remaining reference is the migration-only legacy reader (deleted in the final milestone).
- **Docs:** update system-design §3/§3.2 and §10.3 (supersede the sidecar half — session locators are registry-resident; new agents write no session-link files), `harness-behavior.md`, and `AGENTS.md` if it references the session-link sidecars. Note in the metadata plan that M4's new file lands in this cleaned-up structure.

---

## Milestone 4 — One-time migration of existing dev projects + delete legacy code

### Goal & Outcome

The developer's existing on-disk projects are migrated to the new format in a single pass run by the implementing agent at the end of this plan. No migration infrastructure is shipped into the product — this is a pre-release codebase with no external users.

Outcomes:
- Every `registry.jsonl` across the developer's `.switchboard/` directories is rewritten with canonical `session_locator` records (no `session_id` field).
- Every legacy Codex `<agent-id>.jsonl` sidecar is read, its locator written into the matching registry record, and the sidecar file deleted.
- Every legacy Antigravity `<agent-id>.antigravity.jsonl` sidecar is handled the same way.
- After the migration pass the legacy sidecar modules are deleted from source; no session-link files remain anywhere.
- The `#[serde(default)]` shim on `session_locator` (added in M1 to survive the migration window) is removed — records without the field are now corruption, not a valid old shape.

### Implementation Outline

- **Migration pass (agent-executed, not product code).** The implementing agent:
  1. Finds every `registry.jsonl` under `~/.config/switchboard/` (or wherever the developer's workspace points) and under each bound working directory's `.switchboard/projects/`.
  2. For each agent record: if `session_locator` is already present, skip. If `session_id` is present (old Claude/Gemini shape), rewrite the record with `session_locator: {"uuid": "<uuid>"}` and no `session_id`. If neither is present (old Codex/Antigravity — `None`), check for the matching sidecar; if found, read its latest record and write the appropriate locator; if not found, leave `session_locator: null`.
  3. Deletes the sidecar file **only after** the registry write is confirmed.
  4. Reports any corrupt sidecar loudly rather than deleting it.
- **Delete the legacy code.** Once the migration pass is confirmed clean: remove the retained sidecar modules/structs from M2/M3, remove the `#[serde(default)]` shim from `session_locator`, and delete any sidecar-related test fixtures that are no longer needed. **Note:** dropping `#[serde(default)]` alone does **not** make a missing `session_locator` fail loud — serde fills a missing `Option` field with `None` implicitly. To actually surface an unmigrated record (old `session_id` key, no `session_locator`) instead of silently loading it as "no locator," the field carries a `#[serde(deserialize_with = …)]` that requires the key to be present (explicit `null` still allowed → `None`).

### Definition of Done

- **Migration confirmed:** the agent reports which projects/agents were migrated and the developer verifies the registry files look correct (spot-check a few records).
- **Grep:** no session-link sidecar modules remain; `sessions/` holds only `.meta.json` (plus the metadata plan's `.turnmeta.jsonl`); no `session_id` field appears in any `registry.jsonl`.
- **`#[serde(default)]` removed** from `session_locator` and replaced with a required-key `deserialize_with` (serde would otherwise still default a missing `Option` to `None`); a record missing the field now fails deserialization (fail-loud, as intended), while an explicit `null` still loads as `None`.
- **Docs:** `system-design.md` §10.3 + AGENTS.md updated to record that session-link sidecars are fully removed.
- **Known limitation (record):** no downgrade support — a pre-refactor build cannot read migrated projects. Acceptable.

---

## Out of scope (do not build)

- **Agent reordering** — the update-in-place op unblocks it, but no reorder op/UI is built here.
- **A generic registry migration framework** — only the one back-compat read for the locator field (M1) is in scope; not a versioned-schema migrator.
- **Moving per-turn/temporal data into the registry** — cost/overage/rate-limit stay in their sidecars (`.meta.json`, the metadata plan's `.turnmeta.jsonl`); the registry holds identity, not telemetry. This is the same nature-of-the-data rule, applied in reverse.
- **Hardening `append_jsonl`/sidecar fsync** — a known workspace-wide gap, tracked separately; this plan doesn't take it on (the registry write path is already atomic+fsync via `write_jsonl`).
- **Changing the registry file format** (e.g. JSONL → YAML list) — JSONL with the existing atomic rewrite is sufficient for the update op; reconsidering the format is a separate call.

## Decisions resolved during planning

- **No back-compat shim.** This is a pre-release product with no external users. M1 uses `#[serde(default)]` on `session_locator` only to survive the window between M1 landing and the M4 migration pass running. M4 removes the shim entirely and migrates the developer's existing files directly.
