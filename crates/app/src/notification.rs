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
//! [`AuthorizationGate`] follows the same shape for the permission request, so
//! its coordination — one request shared by concurrent callers, delivery held
//! until it resolves, retryable after a failure — is tested against a fake
//! requester rather than a real dialog.
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
pub struct UserNotificationDelivery {
    gate: Arc<AuthorizationGate>,
}

impl UserNotificationDelivery {
    pub fn new(gate: Arc<AuthorizationGate>) -> Self {
        Self { gate }
    }
}

impl NotificationDelivery for UserNotificationDelivery {
    fn deliver(&self, title: &str, body: &str) {
        if !bundled() {
            return;
        }
        let (title, body) = (title.to_owned(), body.to_owned());
        let gate = Arc::clone(&self.gate);
        // Spawned rather than blocked on. The crate's blocking wrapper routes
        // through the main run loop and errors out if the main thread happens not
        // to be pumping at that instant — a needless failure mode when nothing
        // here needs an answer.
        tauri::async_runtime::spawn(async move {
            // Wait for permission to have been *asked for* — never post into a
            // `NotDetermined` state, which macOS drops silently. This does not
            // decide whether to post: the attempt below is unconditional, so a
            // user who fixes a denial in System Settings mid-session is not
            // blocked by anything we cached.
            gate.ensure().await;
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

/// Requests notification permission from the OS. Injected so the gate's
/// coordination can be tested without a real permission dialog.
#[async_trait::async_trait]
pub trait AuthorizationRequester: Send + Sync {
    /// Resolve when the user has answered. The `bool` is *not* stored — see
    /// [`AuthorizationGate`] — it exists only for logging.
    async fn request(&self) -> Result<bool, String>;
}

/// The real request.
pub struct OsAuthorizationRequester;

#[async_trait::async_trait]
impl AuthorizationRequester for OsAuthorizationRequester {
    async fn request(&self) -> Result<bool, String> {
        mac_usernotifications::request_auth()
            .await
            .map_err(|e| e.to_string())
    }
}

/// Makes sure permission has been *asked for* before anything is posted.
///
/// **A barrier, not a cached decision — and the distinction is the whole point.**
/// The cell holds `()`, never the answer. macOS refuses to deliver while
/// authorization is still `NotDetermined`, so posting before the request resolves
/// silently drops the notification; awaiting this first closes that window. But
/// authorization can *change* in System Settings while Switchboard runs — which
/// the Settings copy actively tells users to do — so caching "denied" and
/// skipping delivery on the strength of it would leave notifications dead until
/// restart even after the user re-enabled them. Delivery therefore always
/// attempts the post once the barrier resolves, and lets macOS apply its current
/// settings. Storing a `bool` here would put that mistake one `if` away.
///
/// A failed request leaves the barrier unresolved so a later attempt can retry;
/// only a completed request (granted *or* denied) satisfies it.
pub struct AuthorizationGate {
    requested: tokio::sync::OnceCell<()>,
    requester: Arc<dyn AuthorizationRequester>,
}

impl AuthorizationGate {
    pub fn new(requester: Arc<dyn AuthorizationRequester>) -> Self {
        Self {
            requested: tokio::sync::OnceCell::new(),
            requester,
        }
    }

    /// Resolve the barrier, requesting permission if nobody has yet. Concurrent
    /// callers share the single in-flight request rather than stacking prompts.
    pub async fn ensure(&self) {
        let _ = self
            .requested
            .get_or_try_init(|| async {
                match self.requester.request().await {
                    Ok(granted) => {
                        tracing::info!(granted, "notification authorization resolved");
                        Ok(())
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "notification authorization request failed");
                        Err(())
                    }
                }
            })
            .await;
    }
}

/// Ask for permission ahead of the first notification, so the prompt arrives with
/// context the user recognizes — their first dispatch, or switching the setting
/// on — rather than at startup or attached to a banner they haven't seen yet.
///
/// Fire-and-forget: the dialog may sit unanswered indefinitely, so nothing waits
/// on it here. Delivery awaits the same gate, so a notification that beats the
/// answer is held rather than dropped.
///
/// **`enabled` gates the request.** Prompting someone who just turned
/// notifications off is a credibility problem — and because the gate is only
/// resolved by an actual request, skipping leaves it untouched, so enabling the
/// setting later still prompts.
pub fn warm_authorization(gate: &Arc<AuthorizationGate>, enabled: bool) {
    if !enabled || !bundled() {
        return;
    }
    let gate = Arc::clone(gate);
    tauri::async_runtime::spawn(async move { gate.ensure().await });
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
    use std::sync::atomic::{AtomicBool, Ordering};

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

    /// A requester the test drives: counts calls, can be held pending, and can be
    /// made to fail.
    struct FakeRequester {
        calls: std::sync::atomic::AtomicUsize,
        /// Released by the test to let the "request" resolve.
        release: tokio::sync::Semaphore,
        result: Mutex<Result<bool, String>>,
    }

    impl FakeRequester {
        fn new(result: Result<bool, String>) -> Self {
            Self {
                calls: std::sync::atomic::AtomicUsize::new(0),
                release: tokio::sync::Semaphore::new(usize::MAX >> 4),
                result: Mutex::new(result),
            }
        }

        fn pending() -> Self {
            let f = Self::new(Ok(true));
            f.release.forget_permits(usize::MAX >> 4);
            f
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl AuthorizationRequester for FakeRequester {
        async fn request(&self) -> Result<bool, String> {
            let permit = self.release.acquire().await.expect("semaphore closed");
            permit.forget();
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.result.lock().expect("result lock").clone()
        }
    }

    #[tokio::test]
    async fn the_gate_requests_once_and_is_shared_by_concurrent_callers() {
        // Two notifications landing together must not stack two permission
        // prompts on the user.
        let fake = Arc::new(FakeRequester::new(Ok(true)));
        let gate = AuthorizationGate::new(Arc::clone(&fake) as Arc<dyn AuthorizationRequester>);
        tokio::join!(gate.ensure(), gate.ensure());
        gate.ensure().await;
        assert_eq!(fake.calls(), 1);
    }

    #[tokio::test]
    async fn delivery_waits_for_a_pending_request_rather_than_posting_early() {
        // The bug this gate exists for: macOS silently drops a notification posted
        // while authorization is still undetermined, so a send that finishes
        // before the user answers must wait, not vanish.
        let fake = Arc::new(FakeRequester::pending());
        let gate = Arc::new(AuthorizationGate::new(
            Arc::clone(&fake) as Arc<dyn AuthorizationRequester>
        ));
        let waiting = tokio::spawn({
            let gate = Arc::clone(&gate);
            async move { gate.ensure().await }
        });

        tokio::task::yield_now().await;
        assert!(
            !waiting.is_finished(),
            "held while the request is unanswered"
        );

        fake.release.add_permits(1);
        tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .expect("gate should resolve once the request answers")
            .expect("task panicked");
    }

    #[tokio::test]
    async fn a_denial_resolves_the_gate_without_recording_a_verdict() {
        // The gate holds `()`, never the answer — so a user who denies, then
        // re-enables notifications in System Settings without restarting, is not
        // blocked by anything cached here. Delivery attempts the post regardless
        // and lets macOS apply its current settings.
        let fake = Arc::new(FakeRequester::new(Ok(false)));
        let gate = AuthorizationGate::new(Arc::clone(&fake) as Arc<dyn AuthorizationRequester>);
        gate.ensure().await;
        gate.ensure().await;
        assert_eq!(fake.calls(), 1, "denial satisfies the barrier");
    }

    #[tokio::test]
    async fn a_failed_request_leaves_the_gate_retryable() {
        // A transient failure must not latch permanently — otherwise one bad
        // moment costs notifications for the rest of the session.
        let fake = Arc::new(FakeRequester::new(Err("transient".to_owned())));
        let gate = AuthorizationGate::new(Arc::clone(&fake) as Arc<dyn AuthorizationRequester>);
        gate.ensure().await;
        assert_eq!(fake.calls(), 1);

        *fake.result.lock().unwrap() = Ok(true);
        gate.ensure().await;
        assert_eq!(fake.calls(), 2, "retried after the failure");

        gate.ensure().await;
        assert_eq!(fake.calls(), 2, "settled once it succeeded");
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
