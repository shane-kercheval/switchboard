//! Keeps macOS from napping the app while agent access listens.
//!
//! **Why.** Before an orchestrator's message reaches an agent, the backend asks
//! the window to load the project and waits up to ten seconds for its answer.
//! The window is usually hidden when an orchestrator runs unattended, and App
//! Nap slows an app nobody is looking at: lower priority, timers batched. The
//! risky moment is the first message after a quiet stretch, before any turn
//! runs, so the wake lock (held only while turns run) does not cover it. The
//! Mac may still sleep; only the app's own throttling is off. The cost is a
//! little more energy while access is on.
//!
//! **What it does not cover.** The exemption applies to this process only.
//! The window's page runs in the web engine's own content process, which the web
//! engine may throttle while the window is hidden, separately from App Nap. A
//! development build's hidden window answered the readiness round trip in
//! 268 ms after 30 minutes; the installed app is checked by hand with its
//! window closed and left alone for 15 minutes.
//!
//! **Why a thread.** The activity is held through `NSProcessInfo`'s
//! block-scoped call, which ends it when the block returns: ending one any other
//! way is an unsafe call, and this crate forbids unsafe code. The block waits
//! on a channel, so dropping the exemption ends the activity.

use std::sync::mpsc;
use std::thread::JoinHandle;

/// Held while the listener runs; dropping it ends the exemption.
pub struct NapExemption {
    _end: mpsc::Sender<()>,
    /// The thread holding the activity; it finishes once the activity ends.
    /// Read only by the tests, which join it to see the activity end.
    #[cfg_attr(not(test), allow(dead_code))]
    holder: Option<JoinHandle<()>>,
}

impl NapExemption {
    /// Begin the exemption. Best effort: if macOS cannot be asked, the app
    /// runs as before and the failure is logged.
    #[must_use]
    pub fn begin() -> Self {
        Self::begin_with(|| {})
    }

    /// [`Self::begin`], calling `on_hold` once the activity is in effect,
    /// just before the hold waits to be dropped. Tests use it to see the
    /// hold begin.
    fn begin_with(on_hold: impl Fn() + Send + 'static) -> Self {
        let (end, ended) = mpsc::channel::<()>();
        Self {
            _end: end,
            holder: hold_until_dropped(ended, on_hold),
        }
    }
}

#[cfg(target_os = "macos")]
fn hold_until_dropped(
    ended: mpsc::Receiver<()>,
    on_hold: impl Fn() + Send + 'static,
) -> Option<JoinHandle<()>> {
    use block2::RcBlock;
    use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};

    let spawned = std::thread::Builder::new()
        .name("agent-access-app-nap".to_owned())
        .spawn(move || {
            let reason = NSString::from_str("Agent access is listening for MCP clients");
            // Returns once the exemption is dropped, which ends the activity.
            let block = RcBlock::new(move || {
                on_hold();
                let _ = ended.recv();
            });
            NSProcessInfo::processInfo().performActivityWithOptions_reason_usingBlock(
                NSActivityOptions::UserInitiatedAllowingIdleSystemSleep,
                &reason,
                &block,
            );
        });
    match spawned {
        Ok(holder) => Some(holder),
        Err(error) => {
            tracing::warn!(%error, "agent access could not exempt the app from App Nap");
            None
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn hold_until_dropped(
    _ended: mpsc::Receiver<()>,
    _on_hold: impl Fn() + Send + 'static,
) -> Option<JoinHandle<()>> {
    None
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use super::NapExemption;

    /// Proves the holding call stays inside the activity until the exemption
    /// is dropped, and leaves it then. Whether macOS honors the activity can
    /// only be seen from outside (Activity Monitor's App Nap column).
    #[test]
    fn the_holding_call_runs_until_the_exemption_is_dropped() {
        // Twice: a second exemption after the first ended is the listener's
        // restart on a new port or token.
        for _ in 0..2 {
            let (entered_tx, entered) = mpsc::channel();
            let mut exemption = NapExemption::begin_with(move || {
                let _ = entered_tx.send(());
            });
            let Some(holder) = exemption.holder.take() else {
                // No holding thread on this platform.
                return;
            };
            entered
                .recv_timeout(Duration::from_secs(10))
                .expect("the hold began");
            assert!(
                !holder.is_finished(),
                "the hold lasts while the exemption lives"
            );

            drop(exemption);
            let deadline = Instant::now() + Duration::from_secs(10);
            while !holder.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(
                holder.is_finished(),
                "dropping the exemption ended the hold"
            );
            holder.join().expect("the holding thread did not panic");
        }
    }
}
