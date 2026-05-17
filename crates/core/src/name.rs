use crate::error::{CoreError, Result};

/// Canonicalize a name for the *uniqueness check only* per system-design §3 + §4 P1:
/// hyphens → underscores, lowercased. The original (verbatim) name is what gets stored.
///
/// Applied uniformly to agent names (within a project) and project names (within a
/// directory).
pub fn canonicalize_for_uniqueness(name: &str) -> String {
    name.chars()
        .map(|c| if c == '-' { '_' } else { c })
        .collect::<String>()
        .to_lowercase()
}

/// Validates that a name matches `^[A-Za-z0-9_-]+$` and is non-empty.
///
/// No leading-character constraint — digit-first, hyphen-first, and
/// underscore-first names are all accepted. Empty / whitespace-only is
/// rejected.
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(CoreError::InvalidName {
            name: name.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalize_lowercases_and_normalizes_hyphens() {
        assert_eq!(canonicalize_for_uniqueness("Feature-A"), "feature_a");
        assert_eq!(canonicalize_for_uniqueness("feature_a"), "feature_a");
        assert_eq!(canonicalize_for_uniqueness("FEATURE-A"), "feature_a");
        assert_eq!(canonicalize_for_uniqueness("reviewer-a"), "reviewer_a");
        assert_eq!(canonicalize_for_uniqueness("reviewer_a"), "reviewer_a");
    }

    #[test]
    fn validate_accepts_well_formed_names() {
        for ok in ["assistant", "agent-1", "agent_1", "A", "a", "0", "_", "-"] {
            assert!(validate_name(ok).is_ok(), "{ok:?} should be valid");
        }
    }

    #[test]
    fn validate_accepts_leading_digit_hyphen_underscore() {
        // Plan explicitly says no leading-character constraint.
        assert!(validate_name("1agent").is_ok());
        assert!(validate_name("-agent").is_ok());
        assert!(validate_name("_agent").is_ok());
    }

    #[test]
    fn validate_rejects_empty_and_whitespace() {
        assert!(matches!(
            validate_name(""),
            Err(CoreError::InvalidName { .. })
        ));
        assert!(matches!(
            validate_name(" "),
            Err(CoreError::InvalidName { .. })
        ));
        assert!(matches!(
            validate_name("\t\n"),
            Err(CoreError::InvalidName { .. })
        ));
    }

    #[test]
    fn validate_rejects_reserved_characters() {
        for bad in [
            "agent.1", "agent 1", "agent/1", "agent:1", "agent!", "café", "🤖",
        ] {
            assert!(
                matches!(validate_name(bad), Err(CoreError::InvalidName { .. })),
                "{bad:?} should be invalid"
            );
        }
    }
}
