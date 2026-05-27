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

use switchboard_core::HarnessKind;

/// The interactive resume command for `harness`, as program + args, given the
/// harness's session/conversation id (`session_ref`). `None` for a harness with
/// no usable resume-by-id form.
///
/// The returned tokens are unquoted; the caller shell-quotes and may prepend a
/// `cd` into the working directory (some harnesses resolve the session-file path
/// from the cwd).
#[must_use]
pub fn interactive_resume_command(harness: HarnessKind, session_ref: &str) -> Option<Vec<String>> {
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
            "--dangerously-skip-permissions".to_owned(),
        ],
        _ => return None,
    };
    Some(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_resume_uses_resume_flag_and_skip_permissions() {
        assert_eq!(
            interactive_resume_command(HarnessKind::ClaudeCode, "abc"),
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
            interactive_resume_command(HarnessKind::Codex, "sid"),
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
            interactive_resume_command(HarnessKind::Gemini, "uuid"),
            Some(vec![
                "gemini".to_owned(),
                "--resume".to_owned(),
                "uuid".to_owned(),
                "--skip-trust".to_owned(),
            ])
        );
    }

    #[test]
    fn antigravity_resume_uses_conversation_flag() {
        assert_eq!(
            interactive_resume_command(HarnessKind::Antigravity, "conv"),
            Some(vec![
                "agy".to_owned(),
                "--conversation".to_owned(),
                "conv".to_owned(),
                "--dangerously-skip-permissions".to_owned(),
            ])
        );
    }
}
