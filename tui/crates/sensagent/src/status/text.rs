use std::fmt;
use std::sync::LazyLock;

use regex::Regex;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use super::StatusStoreError;

struct TextPatterns {
    meaningful: Regex,
    prohibited: Regex,
}

static TEXT_PATTERNS: LazyLock<Result<TextPatterns, regex::Error>> = LazyLock::new(|| {
    Ok(TextPatterns {
        meaningful: Regex::new(r"[\p{L}\p{N}\p{P}\p{S}]")?,
        // `Cs` has no Rust scalar values, and regex rejects that property name.
        // String decoding rejects lone surrogates before these scalar patterns run.
        prohibited: Regex::new(r"[\p{Cc}\p{Cf}\p{Zl}\p{Zp}\p{Default_Ignorable_Code_Point}]")?,
    })
});

/// Canonical advisory status text: 1–140 Unicode codepoints without prohibited controls.
///
/// Requires a Unicode letter, number, punctuation mark or symbol. Rejects categories
/// `Cc`, `Cf`, `Cs`, `Zl`, `Zp` and `Default_Ignorable_Code_Point`, including joiners
/// and variation selectors. Rust strings exclude surrogates. Validation preserves
/// the original text without trimming or Unicode normalization.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct StatusText(String);

impl StatusText {
    /// Borrow the validated text without modifying its Unicode representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for StatusText {
    type Error = StatusStoreError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if !(1..=140).contains(&value.chars().take(141).count()) {
            return Err(StatusStoreError::InvalidData);
        }
        let patterns = TEXT_PATTERNS
            .as_ref()
            .map_err(|_| StatusStoreError::InvalidData)?;
        if !patterns.meaningful.is_match(&value) || patterns.prohibited.is_match(&value) {
            return Err(StatusStoreError::InvalidData);
        }
        Ok(Self(value))
    }
}

impl From<StatusText> for String {
    fn from(value: StatusText) -> Self {
        value.0
    }
}

impl<'de> Deserialize<'de> for StatusText {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)
            .map_err(|_| D::Error::custom(StatusStoreError::InvalidData))?;
        Self::try_from(value).map_err(D::Error::custom)
    }
}

impl fmt::Debug for StatusText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StatusText([REDACTED])")
    }
}

/// Longest title, in Unicode codepoints (`sessionTitle` in the contract).
pub const MAX_TITLE_CHARS: usize = 60;
/// Longest title, in UTF-8 bytes.
pub const MAX_TITLE_BYTES: usize = 240;

/// A short AI title for a session: one line of 1–60 Unicode codepoints and at
/// most 240 UTF-8 bytes, with a letter, number, punctuation mark or symbol, no
/// whitespace at either end, no trailing period (`.`), and none of
/// [`StatusText`]'s prohibited controls. It names a session only under the
/// owner's rules, never over a name the user gave. Validation preserves the
/// original text.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct SessionTitle(String);

impl SessionTitle {
    /// Borrow the validated title.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for SessionTitle {
    type Error = StatusStoreError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let patterns = TEXT_PATTERNS
            .as_ref()
            .map_err(|_| StatusStoreError::InvalidData)?;
        if !(1..=MAX_TITLE_CHARS).contains(&value.chars().take(MAX_TITLE_CHARS + 1).count())
            || value.len() > MAX_TITLE_BYTES
            || !patterns.meaningful.is_match(&value)
            || patterns.prohibited.is_match(&value)
            || value.starts_with(char::is_whitespace)
            || value.ends_with(char::is_whitespace)
            || value.ends_with('.')
        {
            return Err(StatusStoreError::InvalidData);
        }
        Ok(Self(value))
    }
}

impl From<SessionTitle> for String {
    fn from(value: SessionTitle) -> Self {
        value.0
    }
}

impl<'de> Deserialize<'de> for SessionTitle {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)
            .map_err(|_| D::Error::custom(StatusStoreError::InvalidData))?;
        Self::try_from(value).map_err(D::Error::custom)
    }
}

impl fmt::Debug for SessionTitle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SessionTitle([REDACTED])")
    }
}

#[cfg(test)]
mod tests;
