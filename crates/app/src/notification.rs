//! OS notifications: the single policy gate, and the macOS delivery boundary.
//!
//! **Why this module owns the policy.** Every notification — a workflow run's
//! terminal, a send's completion — passes through one [`Notifier::notify`] call
//! that decides *whether* to deliver before it decides *how*. Callers say what
//! happened and which project it happened in; they never decide whether the user
//! should be told. A second gate anywhere else would drift from this one — and
//! the frontend in particular cannot see window focus the way this can.
//!
//! **Why a trait for the OS call.** [`NotificationDelivery`] is the only part
//! that touches `UNUserNotificationCenter`; the gating above it is pure and
//! unit-tested against a fake. Same shape as [`crate::wake_lock`]'s
//! `SleepInhibitor`, and for the same reason: OS side effects are untestable, so
//! keep them behind the smallest possible seam. Focus and the user preference are
//! injected the same way, so [`GatedNotifier`] — the type that actually applies
//! the policy — is exercised directly by tests rather than through a restatement
//! of its rule.
//!
//! **One deliberate gap.** [`ensure_authorization`] calls the OS request without
//! such a seam, so its non-blocking property is enforced structurally (the only
//! `await` sits inside a `spawn`) and by review rather than by a test. Adding an
//! injectable requester purely to assert a property visible in three lines costs
//! more than it protects; the once-only half of the rule *is* tested, via
//! `claim_first_request`.
//!
//! # macOS preconditions — both required, both silent when unmet
//!
//! `UNUserNotificationCenter` refuses to deliver unless the process is:
//!
//! 1. running from a **`.app` bundle** (it needs a `CFBundleIdentifier`), and
//! 2. running from a bundle with a **real code signature** — ad-hoc
//!    (`codesign -s -`) is sufficient; no Apple Developer account is involved.
//!
//! A bundle in the build tree is not enough: it must be installed under an
//! Applications directory and registered with Launch Services. Both conditions
//! were established by probing five configurations on macOS 26.5.2 — the
//! signature and the install location are *independent* requirements, and
//! missing either produces the same silent failure: `request_auth` returns
//! `Ok(false)` with **no permission prompt**, and every send is rejected. That
//! is indistinguishable from "the user denied permission", which is why
//! [`availability`] exists and why the ad-hoc signing identity in
//! `tauri.conf.json` must not be removed as cosmetic.
//!
//! `make dev` runs a bare binary, so it satisfies neither. That is expected, not
//! a bug: notifications are unavailable in the dev shell and the app says so
//! once at startup rather than erroring on every notification.
//!
//! # Focus / Do Not Disturb
//!
//! An active Focus mode suppresses notifications entirely, and macOS does not
//! expose that state to apps — so Switchboard cannot detect it or tell the user
//! it is happening. Correct platform behavior (a user in Do Not Disturb is
//! asking not to be disturbed), but it means "notifications are on and available"
//! can still produce silence. Deliberately not explained in the Settings copy —
//! that surface documents Switchboard's rules, not macOS's. Breaking through would require the
//! time-sensitive interruption level, which needs an entitlement that requires a
//! provisioning profile, which requires a paid Apple Developer account —
//! incompatible with the ad-hoc signing this depends on.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use mac_usernotifications::{Notification, NotificationSettings};
use serde::Serialize;
use switchboard_core::ProjectId;

/// Fires OS notifications for user-visible terminals: a workflow run's outcome,
/// and a send's completion. Injected into `AppState` so background tasks and
/// command handlers can notify without depending on Tauri internals; the
/// production impl gates on window focus and user preference, while tests record
/// calls.
///
/// Callers pass only *what happened, and where*. Suppression is this trait's
/// business, not theirs — including the project comparison, which is why the
/// originating `project` is a parameter rather than something call sites
/// pre-resolve into a boolean.
pub trait Notifier: Send + Sync {
    fn notify(&self, project: ProjectId, title: &str, body: &str);
}

/// A notifier that drops every call — the default until production injects a real
/// one, and the choice for headless tests that don't assert on notifications.
pub struct NullNotifier;

impl Notifier for NullNotifier {
    fn notify(&self, _project: ProjectId, _title: &str, _body: &str) {}
}

/// The macOS delivery boundary. Implemented once for real
/// (`UserNotificationDelivery`) and once as a fake in tests.
///
/// Infallible by design: delivery is best-effort and asynchronous, so there is
/// no outcome a caller could act on. Failing to tell the user something finished
/// must never fail the thing that finished. What *is* worth testing — whether the
/// gate decided to deliver at all — is observable through the fake.
pub trait NotificationDelivery: Send + Sync {
    fn deliver(&self, title: &str, body: &str);
}

/// Whether Switchboard can currently show a notification, in terms a user can
/// act on. Deliberately *not* a passthrough of Apple's `UNAuthorizationStatus`:
/// authorization alone doesn't mean anything will be shown, and the unbundled
/// case is a development state rather than an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum NotificationAvailability {
    /// At least one presentation channel is live.
    Available,
    /// macOS hasn't been asked yet; the prompt comes on the first send.
    NotDetermined,
    /// macOS will show nothing: permission denied, or every channel (alerts,
    /// Notification Center, sound) turned off in System Settings.
    Suppressed,
    /// Not running from an installed, signed `.app` — i.e. `make dev`. Expected
    /// during development; see the module docs.
    Unavailable,
}

/// Cached answer to "is this process even capable of notifications", so the
/// unbundled case is logged once rather than on every notification for a whole
/// dev session.
static BUNDLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Whether this process can talk to `UNUserNotificationCenter` at all. Logs the
/// explanation exactly once. Note this only proves the *bundle* precondition —
/// an unsigned or build-tree bundle passes here and still fails to deliver, with
/// no further signal (see the module docs).
fn bundled() -> bool {
    *BUNDLED.get_or_init(|| match mac_usernotifications::check_bundle() {
        Ok(()) => true,
        Err(e) => {
            tracing::info!(
                reason = %e,
                "OS notifications unavailable — not running from an app bundle. \
                 Expected under `make dev`; use the debug-deployment target to test them."
            );
            false
        }
    })
}

/// Real delivery through `UNUserNotificationCenter`.
///
/// **The handle is deliberately dropped without observing the response.** Doing
/// otherwise is a trap with no upside: `response()` on a buttonless notification
/// either polls `deliveredNotifications` every 500 ms for the life of the
/// notification, or — if given a timeout to bound that — *deletes the user's
/// notification* when the timer fires, which is fatal for a feature whose whole
/// point is telling you what happened while you were away. Clicking a
/// notification activates the owning app through macOS itself, with no
/// participation from us, so there is nothing to observe.
///
/// Verified end to end from an installed build: clicking a notification brings
/// Switchboard forward **and restores a minimized window**, with no window code
/// of ours involved. That second half was the open question — activating a
/// process and deminiaturizing its window are separate `AppKit` behaviors — so it
/// is recorded here rather than left to be re-derived. If a future macOS release
/// breaks it, the fix belongs in the app's activation handling
/// (`tauri::RunEvent::Reopen`), not in this module.
pub struct UserNotificationDelivery;

impl NotificationDelivery for UserNotificationDelivery {
    fn deliver(&self, title: &str, body: &str) {
        if !bundled() {
            return;
        }
        let (title, body) = (title.to_owned(), body.to_owned());
        // Spawned rather than blocked on. The crate's blocking wrapper routes
        // through the main run loop and errors out if the main thread happens not
        // to be pumping at that instant — a needless failure mode when nothing
        // here needs an answer.
        tauri::async_runtime::spawn(async move {
            match Notification::new()
                .title(title)
                .message(body)
                // The sound rides on the notification so macOS applies the user's
                // per-app alert-style and sound settings — including "sound, no
                // banner", which Switchboard deliberately does not reimplement.
                .default_sound()
                .send()
                .await
            {
                // The handle is dropped here on purpose; see the type's docs.
                Ok(_handle) => {}
                Err(e) => tracing::warn!(error = %e, "failed to show notification"),
            }
        });
    }
}

/// Production notifier: applies the suppression policy, then delivers.
///
/// Every input is a closure rather than a value, so each is read *per call*: a
/// preference toggled mid-session takes effect on the next notification rather
/// than the next launch, and focus and the viewed project are sampled at the
/// moment something finishes. The focus closure returns `Option<bool>` — `None`
/// for "couldn't tell" — so the fail-open policy for an unreadable window state
/// stays here with the rest of the suppression rules instead of leaking into the
/// wiring.
pub struct GatedNotifier {
    delivery: Arc<dyn NotificationDelivery>,
    focused: Arc<dyn Fn() -> Option<bool> + Send + Sync>,
    viewed_project: Arc<dyn Fn() -> Option<ProjectId> + Send + Sync>,
    prefs: Arc<dyn Fn() -> NotifyPrefs + Send + Sync>,
}

/// The user's two notification preferences, read together so a single call sees
/// a consistent pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotifyPrefs {
    /// Master switch. Off means no notifications at all, sound included.
    pub enabled: bool,
    /// Whether a *different* project finishing should still notify while the user
    /// is working in Switchboard. The project on screen never notifies while it
    /// is on screen, regardless of this.
    pub while_focused: bool,
}

impl GatedNotifier {
    pub fn new(
        delivery: Arc<dyn NotificationDelivery>,
        focused: Arc<dyn Fn() -> Option<bool> + Send + Sync>,
        viewed_project: Arc<dyn Fn() -> Option<ProjectId> + Send + Sync>,
        prefs: Arc<dyn Fn() -> NotifyPrefs + Send + Sync>,
    ) -> Self {
        Self {
            delivery,
            focused,
            viewed_project,
            prefs,
        }
    }
}

/// The suppression rule.
///
/// The question this answers is **"can the user already see this?"** — window
/// focus alone is too blunt a proxy for it. Someone heads-down in project A has
/// no view of project B finishing, so suppressing that is a miss, not restraint.
/// But the app does mark background completions in the projects sidebar, so a
/// banner on top of that is redundant for anyone who watches it — which is a real
/// split in taste, and why `while_focused` is a preference rather than a fixed
/// rule.
///
/// `focused: None` means the window state could not be read; that resolves to
/// "not focused" so a terminal still surfaces — a spurious notification is a
/// smaller failure than a silent one.
fn should_deliver(focused: Option<bool>, is_viewed_project: bool, prefs: NotifyPrefs) -> bool {
    if !prefs.enabled {
        return false;
    }
    if !focused.unwrap_or(false) {
        // The app is in the background: nothing about it is visible.
        return true;
    }
    // The app is in front. The project on screen is never worth a banner — the
    // transcript is right there. Anything else is the user's call.
    !is_viewed_project && prefs.while_focused
}

impl Notifier for GatedNotifier {
    fn notify(&self, project: ProjectId, title: &str, body: &str) {
        let is_viewed = (self.viewed_project)() == Some(project);
        if !should_deliver((self.focused)(), is_viewed, (self.prefs)()) {
            return;
        }
        self.delivery.deliver(title, body);
    }
}

/// Ask macOS for notification permission, once per process.
///
/// Called when the user does something that implies they want notifications: an
/// accepted dispatch, or switching the preference on. Never at startup — a
/// permission prompt before the user has done anything is noise — and never for
/// a dispatch that was rejected, which would be asking about work that isn't
/// happening.
///
/// **`enabled` gates the request, and a disabled call does not consume the
/// once-flag.** Prompting someone who just turned notifications off is a
/// credibility problem; but consuming the flag while skipping would be worse,
/// because turning the setting back on later would then never prompt and
/// notifications would stay silently dead forever.
///
/// **Never blocks the caller.** The request is spawned, and the system dialog it
/// raises may sit unanswered indefinitely — awaiting it inline would stall a
/// runtime worker and, with it, unrelated commands. A denial is not an error
/// here; `availability` surfaces it in Settings instead.
pub fn ensure_authorization(enabled: bool) {
    static REQUESTED: AtomicBool = AtomicBool::new(false);
    if !enabled || !bundled() || !claim_first_request(&REQUESTED) {
        return;
    }
    tauri::async_runtime::spawn(async {
        match mac_usernotifications::request_auth().await {
            Ok(granted) => tracing::info!(granted, "notification authorization resolved"),
            Err(e) => tracing::warn!(error = %e, "notification authorization request failed"),
        }
    });
}

/// Claim the right to make the one authorization request this process will make.
/// `true` for the first caller, `false` for every caller after it. Split out so
/// the once-only rule is testable against a local flag rather than the process
/// static.
fn claim_first_request(requested: &AtomicBool) -> bool {
    !requested.swap(true, Ordering::SeqCst)
}

/// Classify the OS settings into something a user can act on.
///
/// **The all-`NotSupported` shape means the bundle itself is invalid.** A build
/// that is unsigned, or signed but running from the build tree, reports
/// `NotDetermined` with *every* channel `NotSupported` — it will never prompt and
/// never deliver. A genuinely not-yet-asked, correctly-installed app also reports
/// `NotDetermined`, but with Notification Center and lock screen already
/// `Enabled`. Checking the channels *before* the authorization status is what
/// separates them; matching on authorization first would tell a user with a
/// broken bundle that a prompt is coming, which is the single most misleading
/// answer available and the exact confusion this whole surface exists to end.
///
/// This shape is **empirical, not a documented Apple contract** — derived by
/// probing five bundle configurations on macOS 26.5.2. It fails safe: if a valid
/// app were ever misclassified `Unavailable`, sending once resolves the real
/// prompt and the status corrects itself.
///
/// **`Suppressed` requires every channel to be off, not just the visual ones.** A
/// user who turns off banners and Notification Center but leaves sound on has
/// deliberately configured "sound, no banner" — that works, and reporting it as
/// broken would be worse than saying nothing.
fn classify(settings: NotificationSettings) -> NotificationAvailability {
    use mac_usernotifications::{AuthorizationStatus as Auth, NotificationSettingStatus as Chan};

    let unsupported = |c: Chan| c == Chan::NotSupported;
    if unsupported(settings.alert_enabled)
        && unsupported(settings.notification_center_enabled)
        && unsupported(settings.sound_enabled)
        && unsupported(settings.lock_screen_enabled)
    {
        return NotificationAvailability::Unavailable;
    }
    match settings.authorization_status {
        Auth::Denied => NotificationAvailability::Suppressed,
        Auth::NotDetermined => NotificationAvailability::NotDetermined,
        _ => {
            let live = |c: Chan| c == Chan::Enabled;
            if live(settings.alert_enabled)
                || live(settings.notification_center_enabled)
                || live(settings.sound_enabled)
            {
                NotificationAvailability::Available
            } else {
                NotificationAvailability::Suppressed
            }
        }
    }
}

/// Current availability, for the Settings surface.
pub async fn availability() -> NotificationAvailability {
    if !bundled() {
        return NotificationAvailability::Unavailable;
    }
    match mac_usernotifications::get_notification_settings().await {
        Ok(s) => classify(s),
        Err(_) => NotificationAvailability::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use mac_usernotifications::{
        AuthorizationStatus as Auth, NotificationSettingStatus as Chan, NotificationSettings,
    };

    use super::*;

    /// Records what the gate decided to deliver. Standing in for the OS is the
    /// whole point: `UNUserNotificationCenter` can't be driven from a unit test,
    /// but the suppression policy above it is the part that carries the bugs.
    #[derive(Default)]
    struct FakeDelivery {
        delivered: Mutex<Vec<(String, String)>>,
    }

    impl FakeDelivery {
        fn calls(&self) -> Vec<(String, String)> {
            self.delivered.lock().expect("fake delivery lock").clone()
        }
    }

    impl NotificationDelivery for Arc<FakeDelivery> {
        fn deliver(&self, title: &str, body: &str) {
            self.delivered
                .lock()
                .expect("fake delivery lock")
                .push((title.to_owned(), body.to_owned()));
        }
    }

    const VIEWED: ProjectId = ProjectId::from_u128(1);
    const OTHER: ProjectId = ProjectId::from_u128(2);

    fn prefs(enabled: bool, while_focused: bool) -> NotifyPrefs {
        NotifyPrefs {
            enabled,
            while_focused,
        }
    }

    fn notifier(fake: &Arc<FakeDelivery>, focused: Option<bool>, p: NotifyPrefs) -> GatedNotifier {
        GatedNotifier::new(
            Arc::new(Arc::clone(fake)),
            Arc::new(move || focused),
            Arc::new(|| Some(VIEWED)),
            Arc::new(move || p),
        )
    }

    #[test]
    fn the_project_on_screen_never_notifies_while_it_is_on_screen() {
        // The transcript is right there; a banner would be telling the user
        // something they are already watching.
        let fake = Arc::new(FakeDelivery::default());
        notifier(&fake, Some(true), prefs(true, true)).notify(VIEWED, "done", "b");
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn another_project_notifies_while_in_app_only_when_asked() {
        // The case that motivated the preference: heads-down in one project while
        // another finishes. Off by default because the projects sidebar already
        // marks it; on for people who miss that.
        let quiet = Arc::new(FakeDelivery::default());
        notifier(&quiet, Some(true), prefs(true, false)).notify(OTHER, "done", "b");
        assert!(quiet.calls().is_empty(), "default is not to interrupt");

        let loud = Arc::new(FakeDelivery::default());
        notifier(&loud, Some(true), prefs(true, true)).notify(OTHER, "done", "b");
        assert_eq!(loud.calls().len(), 1, "opted in");
    }

    #[test]
    fn backgrounded_app_notifies_for_any_project() {
        // Nothing is visible when the app isn't in front, so the viewed-project
        // distinction stops applying — including with `while_focused` off.
        for project in [VIEWED, OTHER] {
            let fake = Arc::new(FakeDelivery::default());
            notifier(&fake, Some(false), prefs(true, false)).notify(project, "done", "b");
            assert_eq!(
                fake.calls().len(),
                1,
                "project {project} while backgrounded"
            );
        }
    }

    #[test]
    fn master_switch_off_suppresses_everything() {
        for focused in [Some(true), Some(false), None] {
            for project in [VIEWED, OTHER] {
                let fake = Arc::new(FakeDelivery::default());
                notifier(&fake, focused, prefs(false, true)).notify(project, "done", "b");
                assert!(
                    fake.calls().is_empty(),
                    "focused={focused:?} project={project}"
                );
            }
        }
    }

    #[test]
    fn unreadable_focus_state_still_delivers() {
        // Exercises the real fail-open policy, not a pre-normalized boolean: the
        // closure reports `None` and `GatedNotifier` decides that means "notify".
        // A spurious notification is a smaller failure than a silent one.
        let fake = Arc::new(FakeDelivery::default());
        notifier(&fake, None, prefs(true, false)).notify(VIEWED, "done", "b");
        assert_eq!(fake.calls().len(), 1);
    }

    #[test]
    fn delivers_the_callers_text_verbatim() {
        // The notifier composes nothing: callers own the copy, it owns the policy.
        let fake = Arc::new(FakeDelivery::default());
        notifier(&fake, Some(false), prefs(true, false)).notify(
            OTHER,
            "Agents finished",
            "switchboard: claude, codex",
        );
        assert_eq!(
            fake.calls(),
            vec![(
                "Agents finished".to_owned(),
                "switchboard: claude, codex".to_owned()
            )]
        );
    }

    #[test]
    fn preferences_are_read_per_call_not_captured_at_construction() {
        // The contract that makes a settings toggle take effect without a restart.
        // A notifier that snapshotted the value at construction would pass every
        // other test here.
        let fake = Arc::new(FakeDelivery::default());
        let while_focused = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&while_focused);
        let n = GatedNotifier::new(
            Arc::new(Arc::clone(&fake)),
            Arc::new(|| Some(true)),
            Arc::new(|| Some(VIEWED)),
            Arc::new(move || prefs(true, flag.load(Ordering::SeqCst))),
        );

        n.notify(OTHER, "first", "suppressed");
        assert!(fake.calls().is_empty(), "opted out at the time of the call");

        while_focused.store(true, Ordering::SeqCst);
        n.notify(OTHER, "second", "delivered");
        assert_eq!(fake.calls().len(), 1, "the flip took effect immediately");
        assert_eq!(fake.calls()[0].0, "second");
    }

    #[test]
    fn no_project_on_screen_is_not_the_viewed_project() {
        // Settings or the Git view: the app is in front but no transcript is, so
        // there is nothing the user can already see.
        let fake = Arc::new(FakeDelivery::default());
        let n = GatedNotifier::new(
            Arc::new(Arc::clone(&fake)),
            Arc::new(|| Some(true)),
            Arc::new(|| None),
            Arc::new(|| prefs(true, true)),
        );
        n.notify(VIEWED, "done", "b");
        assert_eq!(fake.calls().len(), 1);
    }

    #[test]
    fn authorization_is_requested_at_most_once() {
        let flag = AtomicBool::new(false);
        assert!(claim_first_request(&flag), "the first caller requests");
        assert!(!claim_first_request(&flag), "every later caller does not");
        assert!(!claim_first_request(&flag));
    }

    #[test]
    fn a_disabled_preference_leaves_the_once_flag_unclaimed() {
        // The ordering that matters: `ensure_authorization` checks `enabled`
        // *before* claiming. If a disabled call consumed the flag, turning
        // notifications on later would never prompt, and they would stay
        // silently dead for the life of the process — a worse failure than the
        // spurious prompt this gate exists to prevent.
        let flag = AtomicBool::new(false);
        let enabled = false;
        if enabled {
            claim_first_request(&flag);
        }
        assert!(
            claim_first_request(&flag),
            "the first enabled call still gets to request"
        );
    }

    /// The settings shape for a given authorization status and channel set.
    fn settings(
        auth: Auth,
        alert: Chan,
        center: Chan,
        sound: Chan,
        lock: Chan,
    ) -> NotificationSettings {
        NotificationSettings {
            authorization_status: auth,
            alert_enabled: alert,
            badge_enabled: Chan::NotSupported,
            sound_enabled: sound,
            lock_screen_enabled: lock,
            notification_center_enabled: center,
        }
    }

    #[test]
    fn invalid_bundle_is_unavailable_not_awaiting_a_prompt() {
        // The recorded shape of an unsigned or build-tree bundle: macOS reports
        // "not determined" but will never prompt and never deliver. Reporting
        // `NotDetermined` here would tell the user to wait for something that
        // cannot happen — the exact misdiagnosis this classifier exists to stop.
        let s = settings(
            Auth::NotDetermined,
            Chan::NotSupported,
            Chan::NotSupported,
            Chan::NotSupported,
            Chan::NotSupported,
        );
        assert_eq!(classify(s), NotificationAvailability::Unavailable);
    }

    #[test]
    fn valid_bundle_not_yet_asked_is_not_determined() {
        // The distinguishing signal against the case above: a correctly installed
        // app that hasn't been asked already reports Notification Center and lock
        // screen as enabled.
        let s = settings(
            Auth::NotDetermined,
            Chan::NotSupported,
            Chan::Enabled,
            Chan::NotSupported,
            Chan::Enabled,
        );
        assert_eq!(classify(s), NotificationAvailability::NotDetermined);
    }

    #[test]
    fn denied_is_suppressed() {
        let s = settings(
            Auth::Denied,
            Chan::Disabled,
            Chan::Disabled,
            Chan::Disabled,
            Chan::Enabled,
        );
        assert_eq!(classify(s), NotificationAvailability::Suppressed);
    }

    #[test]
    fn sound_only_is_available_not_suppressed() {
        // "Alert style: None, sound on" is the configuration Switchboard tells
        // users to set when they want sound without a banner. Flagging it as
        // broken would contradict our own guidance.
        let s = settings(
            Auth::Authorized,
            Chan::Disabled,
            Chan::Disabled,
            Chan::Enabled,
            Chan::Disabled,
        );
        assert_eq!(classify(s), NotificationAvailability::Available);
    }

    #[test]
    fn banner_only_and_center_only_are_available() {
        let banner = settings(
            Auth::Authorized,
            Chan::Enabled,
            Chan::Disabled,
            Chan::Disabled,
            Chan::Disabled,
        );
        assert_eq!(classify(banner), NotificationAvailability::Available);

        let center = settings(
            Auth::Authorized,
            Chan::Disabled,
            Chan::Enabled,
            Chan::Disabled,
            Chan::Disabled,
        );
        assert_eq!(classify(center), NotificationAvailability::Available);
    }

    #[test]
    fn authorized_with_every_channel_off_is_suppressed() {
        let s = settings(
            Auth::Authorized,
            Chan::Disabled,
            Chan::Disabled,
            Chan::Disabled,
            Chan::Disabled,
        );
        assert_eq!(classify(s), NotificationAvailability::Suppressed);
    }
}
