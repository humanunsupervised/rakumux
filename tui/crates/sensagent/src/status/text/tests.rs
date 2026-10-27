use serde::Deserialize;
use serde_json::value::RawValue;

use super::{SessionTitle, StatusStoreError, StatusText};

const SUCCESSES: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../contracts/session-status/fixtures/successes.json"
));
const BOUNDARIES: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../contracts/session-status/fixtures/boundaries.json"
));

#[derive(Deserialize)]
struct SuccessFixture {
    name: String,
    valid: bool,
    value: StatusValue,
}

#[derive(Deserialize)]
struct StatusValue {
    // Preserve invalid surrogate escapes until StatusText's own decoder sees them.
    #[serde(default, deserialize_with = "raw")]
    status_text: Option<Box<RawValue>>,
    // Likewise for SessionTitle's, keeping an explicit `null`.
    #[serde(default, deserialize_with = "raw")]
    title: Option<Box<RawValue>>,
}

fn raw<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Box<RawValue>>, D::Error> {
    Box::<RawValue>::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
struct BoundaryFixture {
    name: String,
    schema: String,
    field: String,
    text: String,
    count: usize,
    suffix: String,
    valid: bool,
}

fn assert_invalid_text(value: &str) {
    assert_eq!(
        StatusText::try_from(value.to_owned()),
        Err(StatusStoreError::InvalidData)
    );
    let json = serde_json::to_string(value).unwrap();
    assert_static_decode_error(&json);
}

fn assert_static_decode_error(json: &str) {
    let error = serde_json::from_str::<StatusText>(json).unwrap_err();
    let message = error.to_string();
    let static_message = StatusStoreError::InvalidData.to_string();
    assert!(
        message == static_message || message.starts_with(&format!("{static_message} at line ")),
        "unexpected decoder diagnostic: {message}"
    );
}

#[test]
fn unicode_validation_profile_compiles() {
    let patterns = super::TEXT_PATTERNS.as_ref().unwrap_or_else(|error| {
        panic!("static Unicode validation profile failed to compile: {error}")
    });
    assert!(patterns.meaningful.is_match("Checking"));
    assert!(!patterns.prohibited.is_match("Checking"));
}

#[test]
fn canonical_success_fixtures_validate_text_independently_of_envelope() {
    let fixtures: Vec<SuccessFixture> = serde_json::from_str(SUCCESSES).unwrap();
    assert_eq!(fixtures.len(), 48);
    for fixture in fixtures {
        // These envelopes fail only their other fields, not their status text.
        let text_valid = match fixture.name.as_str() {
            "tool_proposal" | "missing_request_id" | "wrong_version" => true,
            _ if fixture.value.title.is_some() => true,
            _ => fixture.valid,
        };
        let Some(json) = fixture.value.status_text.as_ref().map(|raw| raw.get()) else {
            assert_eq!(fixture.name, "title_without_status");
            continue;
        };
        let decoded = serde_json::from_str::<StatusText>(json);
        assert_eq!(decoded.is_ok(), text_valid, "{}", fixture.name);
        if !text_valid {
            assert_static_decode_error(json);
        }
        match serde_json::from_str::<String>(json) {
            Ok(text) => assert_eq!(
                StatusText::try_from(text).is_ok(),
                text_valid,
                "{}",
                fixture.name
            ),
            Err(_) => assert!(
                matches!(
                    fixture.name.as_str(),
                    "lone_surrogate" | "structured_status"
                ),
                "unexpected non-string fixture: {}",
                fixture.name
            ),
        }
    }
}

#[test]
fn canonical_status_boundaries_count_codepoints_not_bytes_units_or_graphemes() {
    let fixtures: Vec<BoundaryFixture> = serde_json::from_str(BOUNDARIES).unwrap();
    let mut checked = 0;
    for fixture in fixtures {
        if fixture.schema != "success" || fixture.field != "status_text" {
            continue;
        }
        let text = fixture.text.repeat(fixture.count) + &fixture.suffix;
        assert_eq!(text.chars().count(), if fixture.valid { 140 } else { 141 });
        let json = serde_json::to_string(&text).unwrap();
        assert_eq!(
            StatusText::try_from(text).is_ok(),
            fixture.valid,
            "{}",
            fixture.name
        );
        assert_eq!(
            serde_json::from_str::<StatusText>(&json).is_ok(),
            fixture.valid,
            "{}",
            fixture.name
        );
        checked += 1;
    }
    assert_eq!(checked, 6);
}

#[test]
fn canonical_title_fixtures_validate_the_title_independently_of_envelope() {
    let fixtures: Vec<SuccessFixture> = serde_json::from_str(SUCCESSES).unwrap();
    let mut checked = 0;
    for fixture in fixtures {
        let Some(json) = fixture.value.title.as_ref().map(|raw| raw.get()) else {
            continue;
        };
        // That envelope fails only its missing status text.
        let valid = fixture.valid || fixture.name == "title_without_status";
        assert_eq!(
            serde_json::from_str::<SessionTitle>(json).is_ok(),
            valid,
            "{}",
            fixture.name
        );
        match serde_json::from_str::<String>(json) {
            Ok(text) => assert_eq!(
                SessionTitle::try_from(text).is_ok(),
                valid,
                "{}",
                fixture.name
            ),
            Err(_) => assert!(
                matches!(
                    fixture.name.as_str(),
                    "title_lone_surrogate" | "structured_title" | "null_title"
                ),
                "unexpected non-string fixture: {}",
                fixture.name
            ),
        }
        checked += 1;
    }
    assert_eq!(checked, 22);
}

#[test]
fn canonical_title_boundaries_count_codepoints_not_bytes_units_or_graphemes() {
    let fixtures: Vec<BoundaryFixture> = serde_json::from_str(BOUNDARIES).unwrap();
    let mut checked = 0;
    for fixture in fixtures {
        if fixture.schema != "success" || fixture.field != "title" {
            continue;
        }
        let text = fixture.text.repeat(fixture.count) + &fixture.suffix;
        assert_eq!(text.chars().count(), if fixture.valid { 60 } else { 61 });
        assert_eq!(
            SessionTitle::try_from(text).is_ok(),
            fixture.valid,
            "{}",
            fixture.name
        );
        checked += 1;
    }
    assert_eq!(checked, 6);
}

#[test]
fn titles_end_without_whitespace_or_a_period_and_keep_their_text() {
    for value in [
        "Fix tests.",
        " Fix tests",
        "Fix tests ",
        "Fix tests\u{3000}",
        "...",
        "",
    ] {
        assert_eq!(
            SessionTitle::try_from(value.to_owned()),
            Err(StatusStoreError::InvalidData),
            "{value:?}"
        );
    }
    for value in ["Fix tests!", "v0.1.24", "Fix a.b", "Why?", "検証テスト"] {
        let title = SessionTitle::try_from(value.to_owned()).unwrap();
        assert_eq!(title.as_str(), value);
        assert_eq!(String::from(title.clone()), value);
        assert_eq!(format!("{title:?}"), "SessionTitle([REDACTED])");
    }
}

#[test]
fn meaningfulness_accepts_each_canonical_unicode_category() {
    for value in ["א", "界", "Ⅻ", "½", "١", "—", "!", "∑", "$", "😀"] {
        assert_eq!(
            StatusText::try_from(value.to_owned()).unwrap().as_str(),
            value
        );
    }
}

#[test]
fn empty_blank_and_mark_only_text_are_not_meaningful() {
    for value in [
        "",
        " ",
        "\u{a0}\u{1680}\u{2000}\u{2003}\u{202f}\u{205f}\u{3000}",
        "\u{301}",
        "\u{345}\u{5b0}",
        "\u{903}\u{20dd}",
        " \u{301}\u{a0}",
    ] {
        assert_invalid_text(value);
    }
}

#[test]
fn all_c0_del_and_c1_controls_are_rejected_even_with_meaningful_text() {
    for codepoint in (0..=0x1f).chain(0x7f..=0x9f) {
        let control = char::from_u32(codepoint).unwrap();
        assert_invalid_text(&format!("Checking{control}output"));
    }
}

#[test]
fn format_controls_bidi_and_line_separators_are_rejected() {
    for control in [
        '\u{600}',
        '\u{6dd}',
        '\u{70f}',
        '\u{61c}',
        '\u{200b}',
        '\u{200c}',
        '\u{200d}',
        '\u{200e}',
        '\u{200f}',
        '\u{2028}',
        '\u{2029}',
        '\u{202a}',
        '\u{202b}',
        '\u{202c}',
        '\u{202d}',
        '\u{202e}',
        '\u{2060}',
        '\u{2066}',
        '\u{2067}',
        '\u{2068}',
        '\u{2069}',
        '\u{feff}',
        '\u{1bca0}',
        '\u{1d173}',
        '\u{e0001}',
    ] {
        assert_invalid_text(&format!("Checking{control}output"));
    }
}

#[test]
fn default_ignorables_are_rejected_beyond_format_and_control_categories() {
    for invisible in [
        '\u{ad}',
        '\u{34f}',
        '\u{115f}',
        '\u{1160}',
        '\u{17b4}',
        '\u{17b5}',
        '\u{180b}',
        '\u{180f}',
        '\u{2065}',
        '\u{3164}',
        '\u{fe00}',
        '\u{fe0e}',
        '\u{fe0f}',
        '\u{ffa0}',
        '\u{fff0}',
        '\u{fff8}',
        '\u{e0000}',
        '\u{e0080}',
        '\u{e0100}',
        '\u{e01ef}',
        '\u{e0fff}',
    ] {
        assert_invalid_text(&format!("Checking{invisible}output"));
    }
    assert_invalid_text("👩\u{200d}💻");
    assert_invalid_text("❤\u{fe0f}");
}

#[test]
fn ordinary_rtl_combining_sequences_and_spaces_are_preserved_without_normalization() {
    for value in [
        "בדיקת אימות",
        "جارٍ التحقق",
        "  Checking cafe\u{301}\u{a0}\u{2003} ",
        "\u{301}a",
    ] {
        let text = StatusText::try_from(value.to_owned()).unwrap();
        assert_eq!(text.as_str(), value);
        assert_eq!(String::from(text.clone()), value);
        assert_eq!(text.clone(), text);
    }
    assert_ne!(
        StatusText::try_from("é".to_owned()).unwrap(),
        StatusText::try_from("e\u{301}".to_owned()).unwrap()
    );
}

#[test]
fn other_unicode_categories_are_not_blanket_rejected() {
    // The schema does not prohibit private-use or non-ignorable unassigned scalars.
    for value in ["Checking\u{e000}", "Checking\u{378}"] {
        assert_eq!(
            StatusText::try_from(value.to_owned()).unwrap().as_str(),
            value
        );
    }
}

#[test]
fn debug_redacts_content_in_all_formats() {
    for value in ["Synthetic sensitive status", "בדיקת אימות", "😀"] {
        let text = StatusText::try_from(value.to_owned()).unwrap();
        assert_eq!(format!("{text:?}"), "StatusText([REDACTED])");
        assert_eq!(format!("{text:#?}"), "StatusText([REDACTED])");
    }
}

#[test]
fn serde_roundtrip_uses_a_plain_string_and_preserves_unicode() {
    for value in ["Checking", "בדיקת אימות", " cafe\u{301} 😀 "] {
        let text = StatusText::try_from(value.to_owned()).unwrap();
        let json = serde_json::to_string(&text).unwrap();
        assert_eq!(serde_json::from_str::<String>(&json).unwrap(), value);
        assert_eq!(serde_json::from_str::<StatusText>(&json).unwrap(), text);
    }
}

#[test]
fn serde_rejects_non_strings_with_static_errors() {
    for json in [
        "null",
        "true",
        "42",
        "1.5",
        r#"["Synthetic sensitive status"]"#,
        r#"{"text":"Synthetic sensitive status"}"#,
    ] {
        assert_static_decode_error(json);
    }
}

#[test]
fn surrogate_category_has_no_rust_scalar_representation() {
    for codepoint in 0xd800..=0xdfff {
        assert!(char::from_u32(codepoint).is_none());
    }
}

#[test]
fn serde_rejects_lone_or_invalid_surrogates_and_accepts_valid_pairs() {
    for json in [
        r#""Checking\udfff""#,
        r#""Checking\ud800""#,
        r#""Checking\ud800a""#,
        r#""Checking\udc00\ud800""#,
        r#""Checking\ud800\ud800""#,
    ] {
        assert!(serde_json::from_str::<String>(json).is_err());
        assert_static_decode_error(json);
    }
    let text: StatusText = serde_json::from_str(r#""\ud83d\ude00""#).unwrap();
    assert_eq!(text.as_str(), "😀");
}
