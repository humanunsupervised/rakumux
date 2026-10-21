use super::*;
use crate::auth::model::DeviceResponse;
use crate::auth::tests::{config, device_json};

fn prompt() -> VerificationPrompt {
    serde_json::from_value::<DeviceResponse>(device_json())
        .unwrap()
        .validate(&config())
        .unwrap()
        .prompt
}

fn website() -> AccountWebsite {
    AccountWebsite::new("https://rakumux.example").unwrap()
}

#[test]
fn continuation_contains_only_the_public_code_and_original_provider_link() {
    let prompt = prompt();
    let url = website().connect_url(&prompt).unwrap();
    assert_eq!(
        url.origin().ascii_serialization(),
        "https://rakumux.example"
    );
    assert_eq!(url.path(), "/connect");
    let fields: Vec<_> = url.query_pairs().collect();
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0], ("user_code".into(), "BCDF-GHJK".into()));
    assert_eq!(
        fields[1],
        (
            "verification".into(),
            prompt.verification_uri_complete().unwrap().as_str().into()
        )
    );
    for private in [
        "PRIVATE-DEVICE-CODE",
        "PRIVATE-ACCESS-TOKEN",
        "PRIVATE-REFRESH-TOKEN",
        "return_to",
        "device_code",
    ] {
        assert!(!url.as_str().contains(private));
    }
    assert!(url.fragment().is_none());
}

#[test]
fn continuation_rejects_unsafe_website_configuration() {
    for origin in [
        "http://rakumux.example",
        "https://rakumux.example/connect",
        "https://user:pass@rakumux.example",
        "https://rakumux.example/?return_to=foreign",
        "https://rakumux.example/#fragment",
        " https://rakumux.example",
        "https://rakumux.example\n",
    ] {
        assert!(AccountWebsite::new(origin).is_err(), "{origin:?}");
    }
}

#[test]
fn continuation_does_not_invent_a_provider_convenience_link() {
    let mut prompt = prompt();
    prompt.verification_uri_complete = None;
    assert_eq!(
        website().connect_url(&prompt),
        Err(BrowserContinuationError::MissingProviderLink)
    );
    assert_eq!(
        prompt.verification_uri().as_str(),
        "https://accounts.example.com/device"
    );
    assert_eq!(prompt.user_code(), "BCDF-GHJK");
}

#[test]
fn continuation_preserves_native_code_case_hyphens_and_boundaries() {
    for code in ["a".to_owned(), "bcdf--GHJK-".to_owned(), "A".repeat(128)] {
        let mut prompt = prompt();
        prompt.user_code = code.clone();
        prompt.verification_uri_complete = Some(
            Url::parse(&format!(
                "https://accounts.example.com/device?user_code={code}"
            ))
            .unwrap(),
        );
        let url = website().connect_url(&prompt).unwrap();
        assert_eq!(url.query_pairs().next().unwrap().1, code);
    }
}

#[test]
fn continuation_rejects_foreign_private_or_unsupported_provider_context() {
    for link in [
        "https://evil.example/device?user_code=BCDF-GHJK",
        "https://accounts.example.com/other?user_code=BCDF-GHJK",
        "https://accounts.example.com/device?user_code=WRONG",
        "https://accounts.example.com/device?user_code=BCDF-GHJK&token=private",
        "https://accounts.example.com/device?user_code=BCDF-GHJK&return_to=foreign",
        "https://accounts.example.com/device?user_code=BCDF-GHJK&user_code=BCDF-GHJK",
        "https://accounts.example.com/device?%75ser_code=BCDF-GHJK",
        "https://accounts.example.com/device?user_code=%42CDF-GHJK",
        "https://user:pass@accounts.example.com/device?user_code=BCDF-GHJK",
        "https://accounts.example.com/device?user_code=BCDF-GHJK#fragment",
    ] {
        let mut prompt = prompt();
        prompt.verification_uri_complete = Some(Url::parse(link).unwrap());
        let before = prompt.clone();
        let error = website().connect_url(&prompt).unwrap_err();
        assert_eq!(error, BrowserContinuationError::UnsupportedProviderLink);
        assert_eq!(prompt, before, "fallback must not be modified");
        assert!(!error.to_string().contains("private"));
    }
}

#[test]
fn continuation_rejects_oversized_links_instead_of_truncating_public_context() {
    for (path, expected) in [
        ("a".repeat(2048), BrowserContinuationError::LinkTooLong),
        ("!".repeat(1950), BrowserContinuationError::LinkTooLong),
    ] {
        let mut prompt = prompt();
        prompt.verification_uri =
            Url::parse(&format!("https://accounts.example.com/{path}")).unwrap();
        prompt.verification_uri_complete = Some(
            Url::parse(&format!(
                "https://accounts.example.com/{path}?user_code=BCDF-GHJK"
            ))
            .unwrap(),
        );
        assert_eq!(website().connect_url(&prompt), Err(expected));
    }
}
