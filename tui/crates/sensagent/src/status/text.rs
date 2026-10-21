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

#[cfg(test)]
mod tests;
