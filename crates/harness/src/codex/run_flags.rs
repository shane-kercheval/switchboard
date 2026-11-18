//! The flags a Codex `exec` run adds for the user's own configuration, decided
//! in one place for a turn, a resume and a fork.

use std::path::Path;

use super::own_server::{CodexConfigSources, codex_own_server_overrides};
use super::trust::should_pass_codex_trust;
use crate::OwnMcpServer;

/// The first Codex release whose `exec` accepts `--trust`. Older ones refuse
/// the whole run ("unexpected argument '--trust' found") and save trust on
/// their own.
const FIRST_VERSION_WITH_TRUST: [u64; 3] = [0, 169, 0];

/// What a Codex `exec` run in one folder adds to its command line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexRunFlags {
    /// `-c` pairs that turn Switchboard's own MCP server off for the run.
    pub own_server_overrides: Vec<String>,
    /// Whether the run passes `--trust`.
    pub trust: bool,
}

impl CodexRunFlags {
    /// The flags for a run in `cwd`. `own` is Switchboard's own MCP server, or
    /// `None` when agent access is off. `codex_version` is the installed
    /// Codex's version, as [`crate::HarnessAdapter::version`] reports it.
    #[must_use]
    pub fn for_run(
        sources: &CodexConfigSources,
        home_dir: &Path,
        cwd: &Path,
        own: Option<OwnMcpServer>,
        codex_version: Option<&str>,
    ) -> Self {
        Self {
            own_server_overrides: own_server_overrides(sources, home_dir, cwd, own),
            trust: accepts_trust(codex_version) && should_pass_codex_trust(sources, home_dir, cwd),
        }
    }
}

/// The `-c` pairs that turn Switchboard's own MCP server off for a run in
/// `cwd`, or none when `own` is `None`. Compaction takes only these.
pub(crate) fn own_server_overrides(
    sources: &CodexConfigSources,
    home_dir: &Path,
    cwd: &Path,
    own: Option<OwnMcpServer>,
) -> Vec<String> {
    own.map(|own| codex_own_server_overrides(own, sources, home_dir, cwd))
        .unwrap_or_default()
}

/// Whether a Codex reporting `version` accepts `--trust`. A version that is
/// missing or can't be read counts as current: a refused run says why, where
/// a missing flag would skip the project's config without a word. A
/// pre-release counts as the release it precedes.
fn accepts_trust(version: Option<&str>) -> bool {
    let Some(numbers) = version.and_then(release_numbers) else {
        return true;
    };
    if numbers < FIRST_VERSION_WITH_TRUST {
        tracing::debug!(
            version = version.unwrap_or_default(),
            "this Codex predates --trust; running Codex without it"
        );
        return false;
    }
    true
}

/// `major.minor.patch` from a version such as `0.170.0` or `0.169.0-alpha.2`.
/// A missing patch reads as 0.
fn release_numbers(version: &str) -> Option<[u64; 3]> {
    let release = version
        .split(|c: char| !c.is_ascii_digit() && c != '.')
        .next()?;
    let mut parts = release.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = match parts.next() {
        Some(patch) => patch.parse().ok()?,
        None => 0,
    };
    Some([major, minor, patch])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_needs_codex_0_169_0_or_newer() {
        for version in ["0.169.0", "0.170.0", "0.169.1", "1.0.0", "0.169"] {
            assert!(accepts_trust(Some(version)), "{version}");
        }
        for version in ["0.168.0", "0.168.9", "0.165.0", "0.99.0"] {
            assert!(!accepts_trust(Some(version)), "{version}");
        }
    }

    #[test]
    fn an_unreadable_version_counts_as_current() {
        for version in [None, Some(""), Some("dev"), Some("0"), Some("0.x.1")] {
            assert!(accepts_trust(version), "{version:?}");
        }
    }

    #[test]
    fn a_pre_release_counts_as_its_release() {
        assert!(accepts_trust(Some("0.169.0-alpha.2")));
        assert!(!accepts_trust(Some("0.168.0-alpha.9")));
    }
}
