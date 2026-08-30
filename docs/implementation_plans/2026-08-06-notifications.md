# Notifications — turn-completion alerts, sound, and click-to-focus

## Goal

Tell the user when their agents are done, when they've walked away from the app.

Today Switchboard notifies in exactly one place: workflow run completion/failure (`workflow_commands.rs`, via `TauriNotifier` in `lib.rs`). This plan extends that to **ordinary sends**, gives every notification a **sound**, puts it behind a **user setting**, and makes **clicking a notification focus the app** — and, on the way, moves macOS delivery off a deprecated Apple API onto the current one.

Scope is macOS. The app is macOS-only (`Makefile`, `docs/implementation_plans/2026-05-30-macos-release-distribution.md`); this plan does not preserve a cross-platform notification path.

## What the user asked for

1. A **sound** when a response completes — for a fan-out send, once when *all* recipients finish. For a workflow, once when the *whole run* finishes, never per step.
2. A **notification banner** in the macOS Notification Center. Clicking it brings Switchboard to the foreground and focuses it.
3. Sound and banner independently on/off. **Revised during planning to a single in-app toggle** — see D2. The independence survives; it moves to macOS System Settings, which is where it actually lives.
4. Notifications **only when the app is not focused**. **Revised mid-implementation at the user's request** — see D3. Having used it, they asked for background projects to notify while working in the app, and when the planner argued against adding a setting for it, overruled that too: some people won't want the interruption, which is what the setting is for. The default preserves the original behavior exactly.

---

## Decisions made during planning

These are the discussion-dependent choices. They cannot be recovered by reading the code, and the rationale must survive into the code (module docs / comments), not just live here.

### D1. Deliver via `UNUserNotificationCenter` directly; drop `tauri-plugin-notification`

**Chosen over:** (a) keeping the plugin as-is, (b) flipping `notify-rust`'s `preview-macos-un` feature on via Cargo feature unification.

The plugin's macOS path is `notify-rust` → `mac-notification-sys` → **`NSUserNotification`**, deprecated by Apple since macOS 11 — five major versions ago, on a product about to be distributed. That deprecation is the decision's load-bearing reason. One further problem: **dev builds impersonate Terminal** (`tauri-plugin-notification-2.3.3/src/desktop.rs`, when `tauri::is_dev()`, posts under `com.apple.Terminal`), which is why the existing workflow notification has been effectively invisible during development — it arrives branded as Terminal.

**A rationale this plan previously gave, now known to be wrong:** that the plugin's `let _ = notification.show();` discards the click handle and therefore makes click-to-focus unreachable. D6 establishes that click-to-focus needs no response handle on *either* API — macOS activates the owning app on the default action by itself. Click handling is not a differentiator between the two paths, and the plugin was not blocking it.

**And the honest cost of moving.** The legacy path had *looser* runtime requirements: the very first probe in this investigation delivered an `NSUserNotification` from an unsigned bare binary in `/tmp`. The modern path adds two hard preconditions (ad-hoc signature, real install location — see the table below) that did not exist before. This decision trades permissive runtime requirements for a non-deprecated API. It is still the right trade: both preconditions are already met or met by one config line on the app's existing install path, and the alternative is building new work on an API Apple has been signalling the end of since 2020. But it is a trade, not a free upgrade, and if the preconditions ever become a problem this is the paragraph to reread.

Option (b) was rejected explicitly: Cargo feature unification would flip the plugin's *internal* backend from a switch declared in our crate, with nothing at the call site indicating it. A future plugin version touching a legacy-only item would break the build with an error unrelated to the line that caused it. And it still wouldn't deliver click handling.

So: depend on **`mac-usernotifications`** (0.3.1) directly and delete the plugin. `notify-rust` is not needed — it is a cross-platform wrapper, and there is no second platform.

**Requirements this introduces — read carefully, the first was initially planned wrong:**

- The process must have a **bundle identifier**, i.e. run as a `.app`. `make dev` runs a bare binary and therefore cannot deliver notifications at all — see D7.
- The bundle must be **code-signed**. The crate's own top-level docs are explicit: *"this crate requires that the binary is bundled and be code-signed, an ad-hoc signature is sufficient."* An **ad-hoc signature is free and requires no Apple Developer account** — this is a different thing from the Developer ID signing planned in `2026-05-30-macos-release-distribution.md`, which is about Gatekeeper on *distributed* builds and remains unrelated to this work.

**This is not satisfied today.** `codesign -dv /Applications/Switchboard.app` on the current build reports `adhoc, linker-signed`, `Info.plist=not bound`, `Sealed Resources=none` — that is the linker's automatic signature on the executable, not a signed bundle. `crates/app/tauri.conf.json` declares no signing identity.

#### Verified empirically — do not re-litigate this

A standalone probe (a minimal CLI in a hand-built `.app`, calling `check_bundle` → `get_notification_settings` → `request_auth` → `send`) was run across five configurations on macOS 26.5.2. Results:

| # | Location | Signature | Launch | Outcome |
|---|---|---|---|---|
| A | `/tmp` | linker-signed only | direct exec | `request_auth → Ok(false)`, `send → Err(NotificationRejected)`, all settings `NotSupported` |
| B | `/tmp` | ad-hoc (`--verify --deep --strict` passes) | direct exec | identical failure |
| C | `/tmp` | ad-hoc | `open` | identical failure |
| D | `~/Applications` + `lsregister` | ad-hoc | `open` | **works** — prompt shown, `request_auth → Ok(true)`, `send → Ok(<uuid>)`, settings become `Authorized` / `alert_enabled: Enabled` |
| E | `~/Applications` + `lsregister` | linker-signed only | `open` | fails, **no prompt at all**, immediate `Ok(false)` |

**Two independent preconditions, both required.** D vs E isolates the signature (same location, same launch): an ad-hoc signature is genuinely required, and a linker-signed-only bundle is silently rejected with no prompt. B/C vs D isolates location: a properly signed bundle under `/tmp` still fails, so a real install location plus Launch Services registration is also required — this is what made B and C misleading, and it is why "signing didn't help" is the wrong reading of them.

**For Switchboard this means one change.** The app is already installed to `/Applications` and launched via `open` (`make install-app` / `make deploy`), so the location precondition is met. The signature is the only gap: set `bundle.macOS.signingIdentity` to `"-"`. Tauri's ad-hoc signing guidance: <https://v2.tauri.app/distribute/sign/macos/#ad-hoc-signing>.

**Two incidental findings from the same probe**, both load-bearing elsewhere in this plan:

- The failure mode is **silent** — `Ok(false)`, no dialog, no error — which is indistinguishable from "user denied." This is why M2's status must expose more than `Denied` (it also confirms the `alert_enabled: NotSupported` → `Enabled` transition that status derives from), and why the ad-hoc signing config must not be dropped later as apparently-cosmetic.
- The probe was a plain CLI with **no `NSApplication`** and delivery still worked, so Tauri's GUI app needs no special run-loop handling for the send path.

### D2. No sound toggle; the sound rides on the notification and macOS owns the banner/sound split

**Chosen over** two in-app toggles (banner, sound) with Switchboard playing the sound itself. That was the plan's earlier design; it is deliberately abandoned, and the reasoning matters because the earlier design looks like the more featureful one.

The constraint that decides it: **banner presentation is not ours to control.** Whether a delivered notification shows as a banner is the user's macOS per-app alert-style setting. So an in-app "banner off" toggle could only ever mean *don't post a notification at all* — which necessarily takes the sound with it. Honoring "sound on, banner off" would have required Switchboard to play audio itself, out-of-band, in parallel with a notification system that already does exactly this.

macOS already models these independently and per-app: Apple's notification settings expose alert style (None / Banners / Alerts) separately from "Play sound for notifications," and the settings API returns `alert_enabled` and `sound_enabled` as independent fields (confirmed in the D1 probe output). A user who wants sound without a banner sets alert style to None and leaves sound on. That configuration is the OS's job, not ours to reimplement.

So: **sound is not a Switchboard setting at all.** When notifications are on, post one with the default sound attached and let macOS apply the user's per-app choices. This deletes an entire subsystem the earlier design needed: no sound-player abstraction, no `afplay`, no divergence between our sound behavior and the system's.

(D3 later adds a *second* preference, but for a different question — whether a background project should interrupt you while you're in the app. It is not a sound toggle, and this decision still stands.)

**Two consequences to record in the code**, because both are surprising if undocumented:

- Turning notifications off suppresses the sound too. There is no "silent notification" state in Switchboard; that state exists in System Settings.
- We get the system default notification sound, not a Switchboard-specific one. A custom sound is possible later (the API resolves a sound file bundled in the app's resources) but nothing has asked for it.

#### Known limitation this design introduces: Focus / Do Not Disturb

An active Focus mode or Do Not Disturb suppresses the notification entirely — no banner, no sound — and **Switchboard cannot detect that this is happening.** `UNNotificationSettings` exposes the static per-app settings but not transient system Focus state; Apple deliberately withholds it. So M2's status will report "available" while the user gets nothing, for as long as the Focus mode is active.

This is a **real regression against the abandoned two-toggle design**, which played its own audio and would therefore have sounded through a Focus mode. Naming it as a tradeoff rather than an oversight matters, because it reproduces the exact symptom that motivated this entire plan — "I never see notifications" — for a developer who runs a Focus mode while working, which is a common habit.

Accepted, for two reasons: it is the correct platform behavior (a user in Do Not Disturb is asking not to be disturbed), and the alternative meant Switchboard deliberately overriding a system-wide user intent. Record it as a **known limitation in the module docs and here** — deliberately *not* in the Settings copy, per M2: explaining Do Not Disturb inside Switchboard's settings teaches macOS at the cost of burying the app-specific rules that actually need saying. Someone in a Focus mode already knows they are in one.

The real mechanism for a notification to break through Focus is `UNMutableNotificationContent.interruptionLevel = .timeSensitive`, which the crate supports. It is **not** adopted here: time-sensitive delivery requires the `com.apple.developer.usernotifications.time-sensitive` entitlement, which requires a provisioning profile and therefore an Apple Developer account — incompatible with the ad-hoc signing this plan depends on. Noted so it is not rediscovered from scratch; revisit only if Developer ID enrollment lands and this limitation has proven to matter in practice.

### D3. Suppression is project-aware, and "notify me anyway" is a preference

The gate lives in the notifier — one place, applied to every notification regardless of origin. That part is unchanged, and the frontend must not re-implement it: it cannot see window focus the way the backend can, and a second gate would drift.

**What changed: the rule itself — and who changed it.** This revision came from the user during implementation, after using the feature; it is not a planner's reinterpretation of requirement 4. They also rejected the planner's argument that no setting was needed. Recorded explicitly because a plan that reads as "an agent overrode a stated requirement" is a liability the next time anyone reviews it cold.

The original rule was "suppress whenever the window is focused." That was a proxy for the real question — *can the user already see this?* — and the two come apart in the case that matters most: someone heads-down in project A has no view of project B finishing, so suppressing that is a miss, not restraint.

But the app *does* already mark background completions in the projects sidebar (`backgroundCompletedProjectIds`), so for a user who watches that, an OS banner on top is redundant noise. That is a genuine split in taste, not a right answer — which is what earns a preference here, having just argued against one in D2. The distinction: D2's second toggle would have duplicated something macOS already owns; this one encodes a judgment only the user can make.

**The rule, in order:**

1. Master preference off → nothing, ever.
2. App not focused → notify. Nothing about the app is visible, so the project doesn't matter.
3. App focused, and the finished project is the one on screen → silent. The transcript is right there.
4. App focused, different project → notify only if `notify_while_focused` is on. **Defaults off**, the less startling default; the Settings copy points anyone who wants it at the switch.

"No project on screen" (Settings, the Git view, a project still loading) counts as *not* the viewed project — there is nothing the user can already see.

**Which project is "on screen" is its own piece of state, not `active_project_id`.** That field answers a different question — which project a backend action targets — and stays set while the user is in Settings or the Git view, so reusing it silently suppresses exactly the completions this rule exists to surface. The frontend derives visibility from what it renders and pushes it to a dedicated `visible_project`, carrying a monotonic sequence number so a slow write cannot land after a newer view. Deriving it (rather than pushing from each navigation handler) is what keeps a future navigation path from forgetting to update it.

**Consequence for the `Notifier` contract:** `notify` takes the originating `ProjectId`. Callers say what happened *and where*; the notifier does the comparison. Resolving "is this the viewed project" at the call site would put policy back in the callers, which is what this decision exists to prevent.

### D4. Workflow sends are excluded structurally, by only tracking sends the frontend originates

The send-completion trigger must not fire per workflow step. The exclusion is **structural**: M3's tracker registers a send at the moment the *frontend* dispatches it, so a workflow send — which the backend originates and the frontend only ever observes — is never registered and cannot notify. Nothing needs to detect or filter it. The whole-run notification for that workflow still fires from `workflow_commands.rs` as it does today.

An event-based discriminator also exists and is worth knowing as a cross-check, but is **not** the mechanism: `Dispatcher` carries an `emit_user_message: bool` per queued send, set `true` only by `send_workflow_message_awaiting_completion` and `false` by both `send_message` and `send_message_awaiting_completion` (`crates/dispatcher/src/lib.rs`). So a `user_message` event does imply a backend-originated send. Do not build on it — structural exclusion is stronger, because it cannot be defeated by a future backend send path that forgets to set the flag.

> **Amended 2026-08-29 — see `2026-08-29-reading-mode.md` (M2).** The structural exclusion of workflow *steps* still holds exactly as written above. What changed is the last sentence of the first paragraph: the whole-**run** notification no longer fires from `workflow_commands.rs`. `notify_run_terminal` is deleted, and the run's terminal is reported by the frontend completion tracker at the project-idle boundary alongside manual sends. Reason: firing at run-terminal produced two notifications when a run finished while the user's own sends were still in flight, and only the frontend can see every outcome (a pre-dispatch IPC rejection never reaches the backend event stream at all).

### D5. Notification-authorization request moves to first send

The modern API has a real authorization prompt (the legacy one had none — `isPermissionGranted`/`requestPermission` in `ComposeBar.svelte` are effectively no-ops today and get deleted).

`docs/system-design.md` §7 currently commits to requesting permission "contextually at first invoke, not at cold startup," where "invoke" meant *workflow* invoke. That is now the wrong hook: notifications apply to ordinary sends, which is the far more common action. **New rule: request once per session, on the first send or workflow invoke, whichever comes first.** It must be non-blocking — never delay a dispatch — and idempotent.

Startup was rejected for the reason system-design already gives: a permission prompt at cold start, before the user has done anything, is bad. First-send keeps the prompt in a moment where the user is looking at the app.

**This supersedes the system-design text; update it (see "Documentation").**

### D6. Post and forget — never call `response()`, never set a timeout

Click-to-focus needs **no response observation at all**. Verified empirically: a probe posted a notification, dropped the handle immediately without calling `response()`, and exited. Clicking the banner caused macOS to launch the owning app — activation is the platform's default behavior for the default action, independent of whether the app observes the response.

This matters because the obvious alternative is a trap, and the plan briefly fell into it. Observing `response()` forces a choice between two bad outcomes, both in `mac-usernotifications-0.3.1/src/send.rs`:

- **Without a timeout**, a buttonless notification falls into `poll_until_dismissed`, which queries all delivered notification IDs every 500 ms, per handle, until that notification leaves Notification Center. An ignored banner keeps a task and a poll loop alive indefinitely.
- **With a timeout**, the crate calls `close_delivered` when the timer fires — it **deletes the user's notification**. For a feature whose entire purpose is "tell me what happened while I was away," silently expiring the thing the user came back to read destroys the value. A short TTL defeats the feature; a long one merely bounds the leak.

Not observing avoids both: no task, no poller, no expiry, and the notification persists in Notification Center until the user clears it — which is the correct behavior for this feature.

**Consequences to note in the code**, since "we deliberately drop this handle" reads like an oversight:

- Dropping the handle stops observation but leaves the notification delivered. That is intended.
- Because Switchboard is already running when a notification is clicked, macOS activates the existing instance rather than launching a new one; `tauri-plugin-single-instance` (release builds only) covers the launch case. Neither path needs code from us.
- **Verified from an installed build, including the case that was in doubt:** clicking a notification brings Switchboard forward *and* restores a minimized window. Activating a process and deminiaturizing its window are separate `AppKit` behaviors, so this was the one thing the fire-and-forget design rested on that a probe without a window could not prove. No `RunEvent::Reopen` handler is needed. Should a future macOS release break it, that is where the fix belongs — unminimize and focus unconditionally, *not* gated on `has_visible_windows`, which counts a minimized window as visible.

### D7. Testing requires an installed bundle; add a debug-deployment target

Notifications cannot work under `make dev` (D1), so verifying them needs a `.app` — and, per D1's table, a `.app` that is **both ad-hoc signed and installed under an Applications directory**. Building alone is not enough: a correctly signed bundle sitting in the build tree is probe case B/C, which failed.

`pnpm tauri build --debug --bundles app` supplies the build half at debug speed (losing the frontend hot-reload of `make dev`); the target wraps it with signature verification, install, Launch Services registration, and launch. M1 specifies the rest, including using a distinct bundle identifier so the debug app cannot clobber the installed one.

This is a **development-workflow cost, not an architectural one**, and it was explicitly decided that it must not influence the production choice.

### D8. Which turn outcomes notify

Mirror the policy already established for workflows in `docs/system-design.md` §7:

- **Completed** → notify.
- **Failed** → notify (distinguishable from success in the notification text).
- **Cancelled by the user** → silent. Cancellation is intentional; the user was there.

For a fan-out: notify once, when the last recipient settles. If *every* recipient was cancelled, stay silent; if at least one completed or failed, notify.

---

## Required reading before implementing

Read these before writing code. Several encode constraints that are not obvious from the crate APIs.

- Tauri notification plugin (what is being removed, and why the dev-mode bundle swap exists): <https://v2.tauri.app/plugin/notification/>
- `mac-usernotifications` crate docs — the delivery, authorization, and response APIs: <https://docs.rs/mac-usernotifications/0.3.1/mac_usernotifications/>
- Apple, `UNUserNotificationCenter`: <https://developer.apple.com/documentation/usernotifications/unusernotificationcenter>
- Apple, asking permission to use notifications: <https://developer.apple.com/documentation/usernotifications/asking-permission-to-use-notifications>
- Apple, `CFBundleIdentifier` (the bundle requirement in D1): <https://developer.apple.com/documentation/bundleresources/information-property-list/cfbundleidentifier>
- Tauri CLI `build` reference (for the `--debug --bundles app` target in D7): <https://v2.tauri.app/reference/cli/#build>
- Tauri ad-hoc macOS signing (for the `signingIdentity: "-"` change in D1): <https://v2.tauri.app/distribute/sign/macos/#ad-hoc-signing>

And in this repo:

- `crates/app/src/lib.rs` — `TauriNotifier`, the plugin registration, the single-instance plugin.
- `crates/app/src/wake_lock.rs` — **the pattern to follow** for "one trait wrapping an OS call, everything else pure and unit-tested against a fake." The notification work reuses this shape once, for the OS delivery call.
- `crates/app/src/workflow_commands.rs` — the `Notifier` trait, `NullNotifier`, and the two existing notify call sites.
- `crates/app/src/preferences.rs` + `src/lib/preferences.svelte.ts` + `src/lib/components/SettingsView.svelte` — the full preference round-trip, including the `show_builtins` toggle, which is the template for the new ones.
- `src/lib/state/index.svelte.ts` — the single per-agent event-listener boundary, and `failSendStart`, the non-event outcome path that M3's tracker must also consume.
- `src/lib/state/liveSends.ts` and the `previousLiveProjectSendPairs` observer in `src/lib/state/workspace.svelte.ts` — the existing liveness selector and the existing transition observer over it. M3 explains why send-completion is *not* derived from these; read both before disagreeing.
- `src/lib/components/ComposeBar.svelte` — the dispatch path M3 registers into (`dispatchUserTurn` → per-recipient `sendMessage` → `recordSendAccepted` / `failSendStart`).
- `docs/system-design.md` §7 ("Watching a workflow run") — the existing notification policy this extends.

---

## Milestone 0 — Verify current behavior in a bundle

Small; no production code.

A throwaway workflow already exists at `<config-dir>/workflows/notification-test.yaml` (present in both `switchboard-dev` and `switchboard`). Two trivial steps against one agent.

`make deploy`, run it, switch to another app immediately. Record two facts in the PR description or a scratch note:

- Does it fire **once at the end**, or once per step?
- Is it suppressed when the app is focused, and delivered when it is not?

**Why bother, given D1 is already decided.** Both answers describe *policy*, not mechanism, and both must survive the API swap unchanged. Capturing them against the current implementation means a later behavior change can be attributed to the milestone that caused it rather than argued about. This is the only thing M0 is for — the questions it originally also carried (does delivery work at all, does clicking focus the app) are settled empirically in D1 and D6 and should not be re-run here.

Note that this milestone exercises the **legacy** path, which has none of the modern path's signing or install-location preconditions. A pass here says nothing about whether M1's delivery will work; that is M1's own manual check.

Delete `notification-test.yaml` from both config dirs when the milestone is done. (Done — the file is not part of the repo; it lived in the user-global config dirs and has been removed.)

---

## Milestone 1 — Notification delivery: modern API, click-to-focus, sound

### Goal & Outcome

Replace the macOS delivery mechanism with the current Apple API, on the terms D1 sets out. Little user-visible change yet — the *triggers* are still workflow-only until M3; what changes is the mechanism underneath, plus the preconditions it now depends on.

Once complete:

- Workflow completion/failure notifications are delivered through `UNUserNotificationCenter`, not the deprecated API.
- Clicking a notification brings Switchboard to the front, from anywhere — via macOS activation, with no click-handling code of our own (D6).
- The notification carries the default sound, so macOS applies the user's per-app alert-style and sound choices (D2).
- macOS asks for notification permission once, on the user's first send or workflow invoke of a session — never at startup, never blocking the dispatch.
- Running under `make dev` degrades cleanly: no notifications, one log line explaining why, no crash and no error surfaced to the user.
- A single Makefile target builds a debug `.app` for testing notifications, and both it and `make deploy` produce an ad-hoc-signed bundle that passes `codesign --verify --deep --strict`.

### Implementation Outline

**Step 0 — ad-hoc signing config.** The spike that would have justified this is already done; its results are the table in D1, and they are not worth reproducing. Set `bundle.macOS.signingIdentity` to `"-"` and confirm `codesign --verify --deep --strict` passes on the output of both `make build` and the new debug-deployment target.

Two things to carry into the code, because both are invisible at the call site and expensive to rediscover: **why** the identity is set (without it `UNUserNotificationCenter` rejects every request silently, with no prompt and no error), and that ad-hoc signing changes `make build` / `make deploy` output for everyone rather than just this feature. The second belongs in the commit message; an unsigned bundle was never a deliberate choice, but it is a real change to a shared path.

**Dependency changes.** Add `mac-usernotifications` with `cargo add` (per AGENTS.md — never hand-edit version strings). Remove `tauri-plugin-notification` from `crates/app/Cargo.toml`, `@tauri-apps/plugin-notification` from `package.json`, the plugin registration in `lib.rs`, the `notification:default` entry in `crates/app/capabilities/default.json`, and the `ensureNotificationPermission` helper and its call site in `ComposeBar.svelte`. Commit manifest + lockfile together.

**New module owning delivery.** Move the `Notifier` trait out of `workflow_commands.rs` into this module — it is no longer workflow-specific — and leave `workflow_commands.rs` importing it. Keep the trait's shape minimal: callers say *what happened*, the notifier decides *whether and how to deliver*. Keep `NullNotifier` for tests.

The notifier is the single policy gate. In order, per call:

1. Focus check (existing logic, unchanged — D3).
2. Preference check (lands in M2; leave the seam).
3. Post the notification, **with the default sound attached** (D2). Switchboard does not play audio itself; macOS applies the user's per-app sound and alert-style settings.

The OS-delivery call belongs behind a **trait with a fake**, following `wake_lock.rs`'s `SleepInhibitor` precedent: the gating logic is pure and unit-testable, and only the thin real implementation touches the OS. This is the shared pattern for this plan — do not invent a second one in M2 or M3.

**Click-to-focus.** Nothing to implement (D6). Post the notification, drop the returned handle, do not call `response()`, and do not set a timeout. macOS activates the app when the user clicks. Carry D6's rationale into a comment at the drop site — without it, the next reader sees an ignored `Result`-shaped handle and "fixes" it back into a poller or an expiring notification.

**Authorization (D5).** One idempotent helper, called from both the manual send command and the workflow invoke command, that never delays either. **Use the crate's `async` `request_auth()`, not the `blocking::` wrapper** — the blocking variant parks the calling thread until the user answers the system dialog, which may be never, and doing that inside an async Tauri command stalls a shared worker thread and with it unrelated concurrent commands. The same applies to the settings query M2 adds. (The crate notes that `UNUserNotificationCenter` delivers callbacks on the main thread's run loop and the caller must pump it; a Tauri GUI app does this already, so no extra handling is needed — but say so in a comment, since the crate's docs raise it as a caveat for CLI callers.)

A `Denied` result is not an error — log it and continue; M2 surfaces it in Settings.

**Unbundled degradation.** Both `Notification::send()` and `request_auth()` call `check_bundle()?` internally, so an unbundled process gets an `Err`, not the crash the raw Apple API would produce — that is what makes `make dev` safe. Do not rely on that as the user-facing behavior, though: call `check_bundle()` **once** at startup and hold the result, so the unbundled case is logged a single time at info with a message naming the cause and the fix, rather than producing a repeated error on every notification for the entire dev session. Record *why* in a comment: the modern API requires a bundle identifier, and `make dev` runs a bare binary.

**Makefile.** One target for the debug **deployment**, not merely a debug build (D7). D1 established two independent preconditions, and building a signed bundle into `target/debug/bundle/macos` satisfies only one of them — a bundle sitting in the build tree is in the same category as probe cases B and C, which failed *despite* a valid signature. So the target must build, verify the signature, install and Launch-Services-register the bundle under an Applications directory, and launch it with `open`.

Give the debug bundle a **distinct product name and bundle identifier** so it cannot clobber the installed `/Applications/Switchboard.app` or inherit its notification authorization — a developer must be able to keep the real app installed while testing. Document how to remove it. Note this consequence in the target's comment: because authorization is keyed per bundle identifier, the debug app prompts for permission separately from the real one, and appears as its own entry in System Settings.

### Definition of Done

- **Unit tests** against the fake delivery, covering the gate: focused → nothing delivered; unfocused → delivered, carrying a sound; focus query failure → delivered (the documented fail-open).
- **Unit test** that the authorization helper runs at most once per session, that a denial does not prevent subsequent notify calls from being attempted, and that a dispatch proceeds while the authorization call is still outstanding (the non-blocking contract in D5 — this is the test that catches someone reaching for the `blocking::` variant).
- **No unit test for "carries no timeout / spawns no observer" (D6).** `mac_usernotifications::Notification` exposes no getters, so such a test would assert against a hand-rolled mock of the crate rather than what is actually sent — coverage theater, and against this repo's "test behavior, not implementation" rule. The guarantee is structural and recorded in the doc comment at the drop site instead. Same for `ensure_authorization`'s non-blocking property: testable in principle behind an injectable requester seam, but that seam costs more than it protects for a property visible in three lines. Its once-only half *is* tested.
- **Signing check** wired into the verification path: both `make build` and the debug-deployment target produce bundles that pass `codesign --verify --deep --strict`.
- The existing `workflow_commands.rs` tests that assert notification behavior on run terminals still pass unchanged — the trait boundary is what makes this true, and it is the regression check that the swap changed the mechanism and not the policy.
- **Manual verification** (the OS delivery itself is not unit-testable; record results in the PR): from a debug bundle, a workflow terminal produces a banner attributed to **Switchboard**, plays a sound, and clicking it focuses the window from another app, including from a **minimized** window. Confirm the permission prompt appears once, at first send. If no prompt appears and notifications silently do nothing, the signature is the first thing to check — that is the exact signature of the failure mode in D1's table, not a bug in this code.
- `make dev` still runs, produces no notifications, and logs the one explanatory line.
- Known limitations to record explicitly in the module docs: notifications do not work under `make dev`, and why; and a Focus mode or Do Not Disturb suppresses them undetectably (D2).

---

## Milestone 2 — Preferences and Settings UI

### Goal & Outcome

Put notifications behind user control, and make an OS-level suppression visible rather than silent.

Once complete:

- Settings has a Notifications section with **two** toggles: a master switch (**on** by default), and a nested "also notify me about other projects while I'm using Switchboard" (**off** by default), disabled while the master is off.
- The master switch governs **all** notifications, workflow terminals included. (Today's workflow notification is unconditional; that is inconsistent with the promise of "configurable" and is fixed here.)
- Turning the master off produces no notifications of any kind, sound included.
- The nested switch is the user half of D3's project-aware rule: it never affects the project on screen, only other projects while Switchboard is in front.
- If macOS is suppressing notifications — permission denied, *or* permission granted but every presentation channel turned off — Settings says so and points at where to change it, instead of the toggle silently doing nothing.

  **This is not hypothetical.** During M1 verification the installed `/Applications/Switchboard.app` was observed sitting at **Off** in System Settings → Notifications, alongside the debug build showing "Sounds, Desktop". The release app almost certainly acquired that entry back when the legacy plugin posted under `com.switchboard.desktop`. So the very first user of this feature — us — already has an OS-level suppression that no amount of in-app work can override, and the status line is the only thing that will explain it. Whoever ships this should check that toggle on the real app before concluding notifications are broken.
- Both settings persist across restarts in `config.yaml` alongside the existing preferences.

### Implementation Outline

Two new `Preferences` fields. Follow the existing conventions in `preferences.rs` exactly: `#[serde(default)]` per field so an older `config.yaml` loads with them defaulted, and the merge-not-overwrite save that protects the prompt-provider sections sharing the file. Mirror them into the frontend `Preferences` type and the `DEFAULTS` in `preferences.svelte.ts`.

Read both preferences **together, per call** (one closure returning a pair) rather than separately or captured at construction: a toggle must take effect on the next notification without a restart, and reading them as a pair keeps a single decision from mixing values sampled at different instants.

**Watch the field-by-field copy trap.** `updatePreferences`, `loadPreferences` and the test reset historically assigned each preference individually, so a newly added field type-checks everywhere and silently never reaches the reactive store — presenting as a toggle that won't move rather than as a compile error. Assign in bulk instead. (Found the hard way while adding the first of these fields.)

Settings UI follows the `show_builtins` section: heading, short explanatory paragraphs, and a `role="switch"` toggle each. **Keep the copy to Switchboard's own behavior.** The repo's convention is precision over brevity, but that is not licence to teach macOS: explaining alert styles, sound settings, or Do Not Disturb here buries the app-specific rules in platform documentation the user does not need from us. Two things earn their place, because a user cannot infer them and will otherwise read them as bugs:

- **The project on screen never notifies while you are looking at it**, and other projects stay quiet too unless the second toggle is on. Without this, someone testing the toggle while watching the app concludes it is broken — precisely the confusion that motivated this work.
- **Where banner-versus-sound presentation is configured** (System Settings → Notifications → Applications → Switchboard). One clause naming the location, not a tutorial on what to set.

**Notification availability status.** Add a command exposing an **app-level status**, not a raw passthrough of the OS enum. `AuthorizationStatus::Authorized` alone does not mean a banner will appear: the same settings query also returns `alert_enabled` and `notification_center_enabled`, and macOS can report the app authorized while both are off. Surfacing only `Denied` would leave the exact silent-failure this section exists to prevent.

Derive a small closed set: available; not yet asked; suppressed; and unavailable-because-unbundled.

**"Suppressed" must mean every channel is off, not just the visual ones.** Denied is suppressed. But an authorized app with alerts and Notification Center disabled and **sound still enabled** is *not* suppressed — that is a working sound-without-banner setup. Deriving suppression from the visual channels alone would make Settings warn that notifications are broken while they work exactly as configured, which is worse than saying nothing. Suppressed therefore requires `alert_enabled`, `notification_center_enabled`, **and** `sound_enabled` all unavailable.

**Only the suppressed case renders a hint.** "Not yet asked" needs none (the prompt is coming) and "available" needs none. `unavailable` renders nothing either — but the status still has to *exist*, because its job is to stop the suppressed warning from firing in a dev build, where "macOS is blocking notifications" would be simply wrong. A status that earns its keep by suppressing a message is easy to mistake for dead code; say so where it is defined.

*(The status hint is a small addition beyond the original request, included because the motivating symptom of this whole plan was "I've never seen a notification," and a silently-denied permission reproduces exactly that symptom.)*

### Definition of Done

- **Rust unit tests:** both defaults (master on, background-projects off); a `config.yaml` missing either key loads with the default (the forward/backward-compat contract already pinned for `show_builtins`); save/load round-trip; saving preserves the co-owned prompt sections.
- **Rust unit tests** for the gate across the D3 rule: viewed project while focused → silent; other project while focused → follows the nested preference; backgrounded → notifies for any project; master off → silent in every combination; unreadable focus → notifies; no project on screen → treated as not-the-viewed-project.
- **Rust unit tests** for the visible-project sequence guard: a stale `seq` is dropped, an equal-or-newer one applies.
- **Frontend tests** that the derived visible-project value is `null` for Settings, the Git view, and an unloaded roster, and the active project otherwise.
- **Rust/frontend tests for the authorization gate**: a disabled preference does not consume the once-flag (so enabling later still prompts), and enabling the preference requests authorization without waiting for another dispatch.
- **Frontend tests for save-failure placement**: a failed notification save renders beside the notification toggles and not under Git View, and vice versa; a later successful save clears an earlier failure (correct, because writes are whole-object over optimistically-updated memory).
- **Rust unit test** that the master switch governs a *workflow* terminal, driven through a real run rather than the gate in isolation — the regression that matters is a notify path that bypasses the notifier, which a gate unit test cannot see.
- **Frontend component tests:** each toggle reflects state and persists a flip; the nested toggle is disabled when the master is off; the section states the two rules above; the suppressed hint renders for denied but **not** for sound-only (a working configuration), and not for `unavailable` or a failed probe.
- `docs/ui-conventions.md` needs no change if the section reuses existing primitives; if it introduces anything new, that doc is the place it gets recorded.

---

## Milestone 3 — Notify when a send completes

### Goal & Outcome

The feature the user actually asked for: know when your agents are done.

Once complete:

- Finishing a send while Switchboard is in the background produces one notification (with sound, per the user's macOS settings).
- A fan-out send to N agents produces **one** notification, when the last recipient settles — not N.
- A workflow produces one notification when the whole run ends, and **none** for its individual steps.
- A send whose turns failed notifies, and the notification says so.
- A send the user cancelled notifies nothing.
- The notification names the project and the agent(s), so a user with several projects open knows which one wants them.

### Implementation Outline

**Where this lives: the frontend.** The "have all recipients of this send finished?" question is answered by frontend state and by nothing in the backend — `Dispatcher` is keyed per agent and has no cross-agent send aggregation.

**Track an explicit send lifecycle. Do not derive completion from `buildLiveSendsMap`.** This was the plan's original approach and it is wrong; the reasons are specific and worth stating so nobody re-derives it:

- That selector reports only *who is live right now*. It retains neither the original recipient set nor any outcome, so by the time a send is "done" the information needed to decide **whether** to notify (D8: completed/failed yes, all-cancelled no) and **what to say** (which agents) has already been discarded.
- A pre-dispatch IPC rejection never reaches the event stream at all. `ComposeBar.svelte` catches a failing `sendMessage` and calls `failSendStart`, which prunes the pending entry locally. A send whose IPC rejects for every recipient would shrink out of the map with no event to observe — silently never notifying, in exactly the failure case D8 says must notify.
- `cancel_requested` removes a pending send from the selector *before* the backend emits its cancellation outcome, so liveness disappears ahead of the information needed to classify it.
- There is already a liveness-transition observer over this same selector in `src/lib/state/workspace.svelte.ts` (the project-activity marker, with its `previousLiveProjectSendPairs` diff). A second independent one is duplicate bookkeeping that can disagree with the first.

**The tracker.** Register a send when the frontend dispatches it, carrying `{send_id, project, recipients}` — the full recipient set, captured **before** the per-recipient IPC calls begin, so a rejection cannot erase a recipient that was supposed to be there. Then settle recipients individually from the explicit outcome signals: `failSendStart` (pre-dispatch IPC rejection), and the `message_failed`, `message_cancelled`, and `turn_end` events. Retain each recipient's outcome. When the last registered recipient settles, apply D8, notify at most once, and drop the entry.

This makes **workflow exclusion structural** (D4): only frontend-originated sends are ever registered, so no `user_message` inspection is needed anywhere.

**Where it hooks in.** The event-sourced half must be driven from the **existing** per-agent listener boundary in `src/lib/state/index.svelte.ts`, which every event already passes through — do not register a second `listen` per agent. That file documents a one-listener-per-agent invariant with an explicit double-registration guard. The `failSendStart` half is a direct call, not an event, which is precisely why the tracker cannot live on the event stream alone.

Coordinate with the existing project-activity observer rather than running blind alongside it: both now answer "did this project's work finish," and they must not be able to disagree. Reconciling them into one transition source is preferable if it can be done without disturbing the activity marker's behavior; if not, say in a comment why they are separate.

Note the lifetime consequence, and that it is correct: listeners are registered at project-open and never torn down, so a send in a project the user has navigated away from still notifies. That is the intent — the whole point is being told about work you are not watching.

**Authorization must be coordinated with delivery, not just requested early.** macOS silently drops a notification posted while authorization is still undetermined, so the delivery path has to *await* the request rather than assume it resolved. Two cases make this load-bearing rather than theoretical: a send whose dispatch is rejected for every recipient never reaches the post-acceptance warm-up at all, and a fast turn can finish while the permission dialog is still on screen. Both are the silent failure this milestone exists to prevent.

The barrier must not cache the *answer*. Authorization can change in System Settings mid-session — the Settings copy tells users to do exactly that — so a stored "denied" would keep notifications dead until restart. Hold a resolution barrier, attempt the post unconditionally once it resolves, and let macOS apply its live settings.

**Delivery.** One Tauri command the frontend calls with the assembled notification text. Composing the user-facing string is a frontend concern (it is UI copy); the **policy** — focus suppression, preference gating, sound — stays in the backend notifier from M1, so the frontend cannot bypass it. Keep the command's surface to what this feature needs.

**Content.** Distinguish success from failure, and name the project and the agent(s) involved. Exact phrasing is a copy decision; follow the repo's precision-over-brevity convention.

**Every notification names its project — send completions and workflow terminals alike.** Since a notification can arrive while the user is working in a *different* project (D3), one that doesn't say which project it came from forces a guess. Both paths use the same body shape: `<project>: <detail>`.

### Definition of Done

- **Frontend unit tests** for the tracker, driven by realistic sequences: single-agent send; fan-out where recipients settle at different times (asserting one notification, on the last); a failed turn; a user-cancelled send (silence); a mixed send where one recipient fails and another completes (notifies); a workflow send (silence, by never being registered); and a repeat-signal case asserting no double-notify.
- **Frontend unit test for the pre-dispatch rejection path specifically**: every recipient's `sendMessage` IPC rejects, no agent event is ever emitted, and the send still notifies as failed. This is the case the original derived-liveness design got wrong; it is the regression test for the whole redesign.
- **Frontend unit test** that a recipient cancelled via `cancel_requested` settles from its cancellation outcome rather than from disappearing out of liveness.
- **Component-level test** with mocked `invoke`/`listen` per the AGENTS.md testing convention: capture the listener callback and drive it, including an event arriving before the send's IPC reply resolves. This is the ordering race the convention exists for, and it is the likeliest place for a real bug.
- **Manual verification from a bundled build**, recorded in the PR: a real fan-out send notifies once with the app backgrounded; a workflow run notifies once at the end and not per step; nothing fires with the app focused.
- **Rust tests for the authorization gate**, against an injectable requester: concurrent callers share one request; delivery is held while a request is unanswered; a denial satisfies the barrier without being cached as a verdict; a failed request stays retryable.
- **Frontend tests for teardown**, at least one driven through `unregisterAgents` rather than the tracker directly — the settlement logic was already correct, the missing lifecycle wiring was the defect.
- Known limitation to record: notifications only cover projects opened in the current session, because listeners register at project-open. This is inherent to how sends originate and is not a gap to fix.

**A note on what the manual pass can and cannot prove.** The "totally rejected send still notifies" case cannot be staged through the UI — a harness failure is *accepted* first and fails later via `message_failed`, which requests authorization on the way and therefore exercises a different path entirely. That ordering is proven deterministically by the gate tests instead. Do not record it as manually verified unless the setup genuinely prevented every dispatch from being accepted.

---

## Documentation

- **`docs/system-design.md` §7** — the "Watching a workflow run" paragraph is the current source of truth for notification policy and is now incomplete and partly wrong. Update it to cover send-completion notifications, the single user toggle and why the banner/sound split lives in macOS rather than in Switchboard (D2), and the revised authorization timing (D5). §10.5's resolved note should point at the current mechanism rather than the Tauri plugin.
- **`README.md`** — add a user-facing line only if the bundle requirement is something a user would hit. It is not (users run the installed `.app`), so this is likely no change. The *developer*-facing consequence belongs in the new module's docs and the Makefile target, not the README.
- **`AGENTS.md`** — no change. Per its own stated scope, implementation detail does not belong there.
- Per repo convention: **no milestone references in code.** Describe rules directly; chronology lives in git.

---

## Out of scope

- Cross-platform notifications. The app is macOS-only, and the plugin removal makes this explicit rather than notionally portable.
- Per-project or per-agent notification settings. One global toggle is the agreed surface.
- Notification actions beyond the body click (reply, buttons, "view transcript" deep-links). The API supports them; nothing has asked for them.
- Rate limiting or coalescing when several sends finish at once. Not discussed, and not obviously a problem at realistic send volumes — revisit if it becomes one.
- Notifying on anything other than send/run terminals (idle agents, rate limits, harness auth failures).
- **Developer ID** signing and notarization — already planned in `2026-05-30-macos-release-distribution.md` and unrelated to this work. Note that **ad-hoc bundle signing is explicitly in scope and load-bearing** (D1); only the paid, distribution-facing signing is excluded. The two must not be conflated when this section is read later.

## Assumptions, settled

Two questions were raised during planning and left open. Both shipped as stated in D8 and were confirmed in use:

1. **Failure notifies.** A failed turn produces a notification, distinguished from success. Silence on failure would hide the case you most want to be pulled back for.
2. **One notification per send, not per agent.** A four-recipient fan-out notifies once, when the last agent lands. The tradeoff — no signal that agents one through three are already readable — was accepted as the quieter of the two behaviors.

## Status

All milestones implemented and verified against an installed build: send completions, fan-out coalescing, workflow-only run terminals, the suppression rules, the settings, and click-to-focus including a minimized window.

**One known gap, deliberately not closed:** clicking a notification brings Switchboard forward but does not navigate to the project that finished. The delivery module documents what it would take and why the available route was rejected.
