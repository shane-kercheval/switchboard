//! Keeps macOS from napping the app while agent access listens.
//!
//! **Why.** Before an orchestrator's message reaches an agent, the backend asks
//! the window to load the project and waits up to ten seconds for its answer.
//! The window is usually hidden when an orchestrator runs unattended, and App
//! Nap slows an app nobody is looking at: lower priority, timers batched. The
//! risky moment is the first message after a quiet stretch, before any turn
//! runs, so the wake lock (held only while turns run) does not cover it.
//! Exempting the app while the server listens removes the risk rather than
//! testing for a slowdown that might appear only after a long idle. The Mac may
//! still sleep; only the app's own throttling is off. The cost is a little more
//! energy while access is on.
//!
//! This does not stop the web engine from slowing timers in a hidden page,
//! which is a separate mechanism inside the window.
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
        let (end, ended) = mpsc::channel::<()>();
        Self {
            _end: end,
            holder: hold_until_dropped(ended),
        }
    }
}

#[cfg(target_os = "macos")]
fn hold_until_dropped(ended: mpsc::Receiver<()>) -> Option<JoinHandle<()>> {
    use block2::RcBlock;
    use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};

    let spawned = std::thread::Builder::new()
        .name("agent-access-app-nap".to_owned())
        .spawn(move || {
            let reason = NSString::from_str("Agent access is listening for MCP clients");
            // Returns once the exemption is dropped, which ends the activity.
            let block = RcBlock::new(move || {
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
fn hold_until_dropped(_ended: mpsc::Receiver<()>) -> Option<JoinHandle<()>> {
    None
}

#[cfg(test)]
mod tests {
    use super::NapExemption;

    #[test]
    fn dropping_the_exemption_ends_the_activity() {
        // Twice: a second exemption after the first ended is the listener's
        // restart on a new port or token.
        for _ in 0..2 {
            let mut exemption = NapExemption::begin();
            let holder = exemption.holder.take();
            drop(exemption);
            if let Some(holder) = holder {
                holder.join().expect("the activity's block returned");
            }
        }
    }
}
