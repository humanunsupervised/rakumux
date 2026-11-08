//! Session names: validation, and the name a session gets when none is
//! given ("New session", "New session 2", …), unique within a scope, until
//! its AI title (or the user) names it.

use session_core::api::{ApiError, ErrorCode};

/// Longest accepted name, in characters.
pub(crate) const MAX_NAME_CHARS: usize = 64;

/// The name of a session started without one.
pub(crate) const NEW_SESSION: &str = "New session";

pub(crate) fn validate(raw: &str) -> Result<String, ApiError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "session name must not be empty",
        ));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            format!("session name is longer than {MAX_NAME_CHARS} characters"),
        ));
    }
    if name.chars().any(char::is_control) {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "session name must not contain control characters",
        ));
    }
    Ok(name.to_string())
}

/// [`NEW_SESSION`], or with the first number from 2 that `taken` is false
/// for (`New session 2`), as an AI title another session has gets.
pub(crate) fn generate(mut taken: impl FnMut(&str) -> bool) -> String {
    if !taken(NEW_SESSION) {
        return NEW_SESSION.to_string();
    }
    (2u64..)
        .map(|n| format!("{NEW_SESSION} {n}"))
        .find(|name| !taken(name))
        .unwrap_or_else(|| NEW_SESSION.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn validation_trims_and_rejects_bad_names() {
        assert_eq!(validate("  api work ").unwrap(), "api work");
        assert!(validate("   ").is_err());
        assert!(validate("a\tb").is_err());
        assert!(validate(&"x".repeat(MAX_NAME_CHARS + 1)).is_err());
        assert!(validate(&"é".repeat(MAX_NAME_CHARS)).is_ok());
    }

    #[test]
    fn a_session_without_a_name_is_new_session_numbered_past_taken_ones() {
        assert_eq!(generate(|_| false), "New session");
        let mut taken = HashSet::new();
        for expected in ["New session", "New session 2", "New session 3"] {
            let name = generate(|candidate| taken.contains(candidate));
            assert_eq!(name, expected);
            taken.insert(name);
        }
        // A gap is filled first.
        taken.remove("New session 2");
        assert_eq!(
            generate(|candidate| taken.contains(candidate)),
            "New session 2"
        );
        assert!(validate(&generate(|candidate| taken.contains(candidate))).is_ok());
    }
}
