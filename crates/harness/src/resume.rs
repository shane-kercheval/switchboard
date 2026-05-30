//! Interactive (TUI) resume-command construction, per harness.
//!
//! Switchboard drives harnesses headlessly (`claude -p`, `codex exec`, …). This
//! module produces the *interactive* command a user would run in their own
//! terminal to pick a session back up by hand: the headless print/exec mode is
//! dropped, but the harness's dangerous-skip flag is kept and no `--model` is
//! passed (the adapters pass none, so resume uses the harness default — matching
//! what Switchboard ran). The output is display/copy-only; Switchboard never
//! executes it, which keeps it clear of the same-session-parallel-invocation
//! hazard the app otherwise guards against.
//!
//! Flag knowledge lives here (with the adapters), not in the app layer.

use std::path::Path;

use switchboard_core::HarnessKind;

/// The interactive resume command for `harness`, as program + args, given the
/// harness's session/conversation id (`session_ref`) and the agent's working
/// directory (`cwd`). `None` for a harness with no usable resume-by-id form.
///
/// The returned tokens are unquoted; the caller shell-quotes and prepends a `cd`
/// into `cwd` (Claude/Codex/Gemini resolve the session file from the cwd, so the
/// `cd` is all they need). **Antigravity additionally needs `--add-dir <cwd>`**:
/// `cd` alone leaves it with "no active workspace," running file/command tools
/// against `$HOME` — so the interactive command mirrors the headless dispatch's
/// `--add-dir` (see `antigravity::build_args`).
#[must_use]
pub fn interactive_resume_command(
    harness: HarnessKind,
    session_ref: &str,
    cwd: &Path,
) -> Option<Vec<String>> {
    let id = session_ref.to_owned();
    let tokens = match harness {
        HarnessKind::ClaudeCode => vec![
            "claude".to_owned(),
            "--resume".to_owned(),
            id,
            "--dangerously-skip-permissions".to_owned(),
        ],
        HarnessKind::Codex => vec![
            "codex".to_owned(),
            "resume".to_owned(),
            id,
            "--dangerously-bypass-approvals-and-sandbox".to_owned(),
        ],
        HarnessKind::Gemini => vec![
            "gemini".to_owned(),
            "--resume".to_owned(),
            id,
            "--skip-trust".to_owned(),
        ],
        HarnessKind::Antigravity => vec![
            "agy".to_owned(),
            "--conversation".to_owned(),
            id,
            "--add-dir".to_owned(),
            cwd.to_string_lossy().into_owned(),
            "--dangerously-skip-permissions".to_owned(),
        ],
        _ => return None,
    };
    Some(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CWD: &str = "/work/proj";

    #[test]
    fn claude_resume_uses_resume_flag_and_skip_permissions() {
        assert_eq!(
            interactive_resume_command(HarnessKind::ClaudeCode, "abc", Path::new(CWD)),
            Some(vec![
                "claude".to_owned(),
                "--resume".to_owned(),
                "abc".to_owned(),
                "--dangerously-skip-permissions".to_owned(),
            ])
        );
    }

    #[test]
    fn codex_resume_uses_top_level_resume_subcommand() {
        assert_eq!(
            interactive_resume_command(HarnessKind::Codex, "sid", Path::new(CWD)),
            Some(vec![
                "codex".to_owned(),
                "resume".to_owned(),
                "sid".to_owned(),
                "--dangerously-bypass-approvals-and-sandbox".to_owned(),
            ])
        );
    }

    #[test]
    fn gemini_resume_uses_resume_flag() {
        // `--session-id <existing>` errors ("already exists; use --resume");
        // interactive resume takes `--resume <uuid>`, mirroring the adapter's
        // resume-turn args.
        assert_eq!(
            interactive_resume_command(HarnessKind::Gemini, "uuid", Path::new(CWD)),
            Some(vec![
                "gemini".to_owned(),
                "--resume".to_owned(),
                "uuid".to_owned(),
                "--skip-trust".to_owned(),
            ])
        );
    }

    #[test]
    fn antigravity_resume_passes_conversation_and_add_dir() {
        // `--add-dir <cwd>` establishes the workspace so the interactive resume
        // runs tools in the project dir, not $HOME (mirrors the headless dispatch).
        assert_eq!(
            interactive_resume_command(HarnessKind::Antigravity, "conv", Path::new(CWD)),
            Some(vec![
                "agy".to_owned(),
                "--conversation".to_owned(),
                "conv".to_owned(),
                "--add-dir".to_owned(),
                CWD.to_owned(),
                "--dangerously-skip-permissions".to_owned(),
            ])
        );
    }
}
