//! Antigravity CLI harness module.
//!
//! Antigravity (binary `agy`) is Google's replacement for the Gemini CLI on
//! free / Google AI Pro / Ultra tiers. It is a Go-based client for a
//! server-side agent, with a distinct contract from Gemini CLI:
//!
//! - No structured stream output (`agy -p` writes plain markdown to stdout).
//! - Conversation UUID is assigned server-side and captured post-spawn.
//! - Primary store is encrypted protobuf; only the `transcript.jsonl`
//!   sidecar is parseable.
//! - Auth lives in the macOS Keychain (service `gemini`, account
//!   `antigravity`).
//!
//! The adapter wiring lands in a subsequent change; this module currently
//! exposes only the building blocks an adapter would need: the transcript
//! path constant (`paths`) and the per-agent sidecar that records the
//! server-assigned conversation UUID (`sidecar`).
//!
//! Ground-truth reference: `docs/research/antigravity-cli-observed.md`.

pub mod paths;
pub mod sidecar;

use crate::DispatchError;

/// The binary name on PATH. Centralized so the future adapter and the
/// pre-adapter binary probe agree on the name.
pub const BINARY_NAME: &str = "agy";

/// Verify the `agy` binary is on PATH.
///
/// Mirrors what `AntigravityAdapter::probe` will do when the adapter
/// lands; kept as a free function for the period where no adapter exists
/// in `AppState`. The future adapter's `probe()` body can call this
/// directly — no rewrite, just relocate.
pub fn probe_binary() -> Result<(), DispatchError> {
    probe_binary_by_name(BINARY_NAME)
}

/// Implementation detail of [`probe_binary`]: takes the binary name as a
/// parameter so the negative-path test can pass a synthetic name without
/// mutating the test process's `PATH`.
fn probe_binary_by_name(name: &str) -> Result<(), DispatchError> {
    which::which(name)
        .map(|_| ())
        .map_err(|_| DispatchError::BinaryNotFound)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_binary_returns_binary_not_found_for_nonexistent_name() {
        // Synthetic name guaranteed not to exist on any PATH; exercises
        // the negative arm without mutating process env.
        let result = probe_binary_by_name(
            "switchboard-antigravity-probe-binary-that-definitely-does-not-exist",
        );
        assert!(matches!(result, Err(DispatchError::BinaryNotFound)));
    }
}
