//! Session names: validation and short readable generated names
//! ("brisk-otter"), unique within a scope.

use session_core::api::{ApiError, ErrorCode};
use session_core::ids::Token;

/// Longest accepted name, in characters.
pub(crate) const MAX_NAME_CHARS: usize = 64;

const ADJECTIVES: &[&str] = &[
    "amber", "bold", "brisk", "calm", "clever", "cosmic", "crisp", "dapper", "eager", "fancy",
    "fuzzy", "gentle", "glad", "golden", "happy", "hazy", "humble", "jolly", "keen", "lively",
    "lucky", "mellow", "merry", "misty", "nimble", "noble", "plucky", "polite", "proud", "quick",
    "quiet", "rapid", "rosy", "rustic", "shiny", "silent", "snowy", "sunny", "swift", "tidy",
    "vivid", "warm", "witty", "zesty",
];

const NOUNS: &[&str] = &[
    "badger", "beacon", "bison", "breeze", "brook", "cedar", "comet", "coral", "crane", "delta",
    "falcon", "fern", "finch", "fjord", "gecko", "glacier", "harbor", "heron", "island", "lagoon",
    "lantern", "lynx", "maple", "meadow", "meteor", "otter", "panda", "pebble", "pine", "quartz",
    "raven", "reef", "river", "sparrow", "summit", "thistle", "tiger", "tundra", "walrus",
    "willow", "wren", "yak", "zephyr",
];

/// Trim and check a user-supplied name.
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

/// A two-word name for which `taken` is false. After a few random attempts
/// a numeric suffix guarantees termination.
pub(crate) fn generate(mut taken: impl FnMut(&str) -> bool) -> String {
    let mut random = RandomWords::new();
    for _ in 0..32 {
        let name = random.pair();
        if !taken(&name) {
            return name;
        }
    }
    let base = random.pair();
    (2u64..)
        .map(|n| format!("{base}-{n}"))
        .find(|name| !taken(name))
        .unwrap_or(base)
}

/// Word picks from the OS random source (via a fresh token).
struct RandomWords {
    bytes: Vec<u8>,
    pos: usize,
}

impl RandomWords {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            pos: 0,
        }
    }

    fn next_u16(&mut self) -> usize {
        if self.pos + 2 > self.bytes.len() {
            self.bytes = hex_bytes(Token::generate().as_str());
            self.pos = 0;
        }
        let value = u16::from_be_bytes([self.bytes[self.pos], self.bytes[self.pos + 1]]);
        self.pos += 2;
        value as usize
    }

    fn pair(&mut self) -> String {
        let adjective = ADJECTIVES[self.next_u16() % ADJECTIVES.len()];
        let noun = NOUNS[self.next_u16() % NOUNS.len()];
        format!("{adjective}-{noun}")
    }
}

fn hex_bytes(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .chunks(2)
        .filter_map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect()
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
    fn generated_names_are_two_words_and_avoid_taken_ones() {
        let name = generate(|_| false);
        let words: Vec<_> = name.split('-').collect();
        assert_eq!(words.len(), 2);
        assert!(ADJECTIVES.contains(&words[0]) && NOUNS.contains(&words[1]));

        let mut seen = HashSet::new();
        for _ in 0..200 {
            let name = generate(|candidate| seen.contains(candidate));
            assert!(seen.insert(name));
        }
    }

    #[test]
    fn falls_back_to_a_numeric_suffix_when_everything_is_taken() {
        let name = generate(|candidate| candidate.split('-').count() == 2);
        assert!(name.ends_with("-2"), "{name}");
    }
}
