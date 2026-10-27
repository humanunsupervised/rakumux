use serde_json::{json, Value};

use super::*;

#[derive(Deserialize)]
struct Fixture {
    name: String,
    valid: bool,
    value: Box<RawValue>,
}

fn fixture_source(name: &str) -> &'static str {
    match name {
        "request" => include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../contracts/session-status/fixtures/requests.json"
        )),
        "success" => include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../contracts/session-status/fixtures/successes.json"
        )),
        "error" => include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../contracts/session-status/fixtures/errors.json"
        )),
        _ => panic!("unknown synthetic fixture"),
    }
}

fn base(name: &str) -> Value {
    let fixtures: Vec<Fixture> = serde_json::from_str(fixture_source(name)).unwrap();
    serde_json::from_str(fixtures[0].value.get()).unwrap()
}

fn accepts(schema: &str, body: &[u8]) -> bool {
    match schema {
        "request" => ValidatedStatusRequest::from_slice(body).is_ok(),
        "success" => {
            let id = serde_json::from_slice::<Value>(body)
                .ok()
                .and_then(|value| value["request_id"].as_str().map(str::to_owned))
                .unwrap_or_default();
            decode_success(body, &id).is_ok()
        }
        "error" => decode_error(body).is_ok(),
        _ => panic!("unknown synthetic schema"),
    }
}

#[test]
fn all_shared_request_success_and_error_fixtures() {
    for schema in ["request", "success", "error"] {
        // RawValue preserves lone-surrogate fixtures rather than failing while
        // parsing the fixture envelope, before the actual wire decoder runs.
        let fixtures: Vec<Fixture> = serde_json::from_str(fixture_source(schema)).unwrap();
        for fixture in fixtures {
            assert_eq!(
                accepts(schema, fixture.value.get().as_bytes()),
                fixture.valid,
                "{schema}: {}",
                fixture.name,
            );
            if schema == "request" {
                if let Ok(value) = serde_json::from_str::<Value>(fixture.value.get()) {
                    let original = value.clone();
                    assert_eq!(
                        ValidatedStatusRequest::from_value(&value).is_ok(),
                        fixture.valid,
                        "{}",
                        fixture.name
                    );
                    assert_eq!(value, original);
                }
            }
        }
    }
}

#[test]
fn all_shared_utf8_codepoint_and_error_boundary_recipes() {
    let recipes: Vec<Value> = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../contracts/session-status/fixtures/boundaries.json"
    )))
    .unwrap();
    for recipe in recipes {
        let schema = recipe["schema"].as_str().unwrap();
        let field = recipe["field"].as_str().unwrap();
        let text = recipe["text"]
            .as_str()
            .unwrap()
            .repeat(recipe["count"].as_u64().unwrap() as usize)
            + recipe["suffix"].as_str().unwrap();
        let mut value = base(schema);
        let target = match schema {
            "request" => &mut value["context"],
            "success" => &mut value,
            "error" => &mut value["error"],
            _ => unreachable!(),
        };
        target[field] = Value::String(text);
        assert_eq!(
            accepts(schema, &serde_json::to_vec(&value).unwrap()),
            recipe["valid"].as_bool().unwrap(),
            "{}",
            recipe["name"]
        );
    }
}

#[test]
fn all_shared_raw_body_and_original_byte_bound_recipes() {
    let recipes: Vec<Value> = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../contracts/session-status/fixtures/bodies.json"
    )))
    .unwrap();
    for recipe in recipes {
        let body = if let Some(bytes) = recipe["bytes"].as_array() {
            bytes
                .iter()
                .map(|byte| byte.as_u64().unwrap() as u8)
                .collect()
        } else {
            recipe["body"].as_str().unwrap().as_bytes().to_vec()
        };
        assert_eq!(
            ValidatedStatusRequest::from_slice(&body).is_ok(),
            recipe["valid"].as_bool().unwrap(),
            "{}",
            recipe["name"]
        );
    }
    let recipes: Vec<Value> = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../contracts/session-status/fixtures/body-bounds.json"
    )))
    .unwrap();
    for recipe in recipes {
        let mut value = base("request");
        if let Some(count) = recipe["output_count"].as_u64() {
            value["context"] =
                json!({"lifecycle":"working", "recent_output":"a".repeat(count as usize)});
        }
        assert!(ValidatedStatusRequest::from_value(&value).is_ok());
        let mut body = serde_json::to_string(&value).unwrap();
        if recipe["encoding"] == "escape_ascii" {
            body = body.replace('a', "\\u0061");
        }
        if let Some(size) = recipe["pad_to_bytes"].as_u64() {
            body.push_str(&" ".repeat(size as usize - body.len()));
        }
        let result = ValidatedStatusRequest::from_slice(body.as_bytes());
        assert_eq!(
            result.is_ok(),
            recipe["valid"].as_bool().unwrap(),
            "{}",
            recipe["name"]
        );
        if let Ok(request) = result {
            assert_eq!(
                request.body(),
                body.as_bytes(),
                "original bytes must survive"
            );
        }
    }
}

#[test]
fn all_shared_correlation_pairs_are_exact_and_case_sensitive() {
    let fixtures: Vec<Value> = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../contracts/session-status/fixtures/correlation.json"
    )))
    .unwrap();
    for fixture in fixtures {
        let request = ValidatedStatusRequest::from_value(&fixture["request"]).unwrap();
        let body = serde_json::to_vec(&fixture["response"]).unwrap();
        let result = decode_success(&body, request.request_id());
        assert_eq!(
            result.is_ok(),
            fixture["valid"].as_bool().unwrap(),
            "{}",
            fixture["name"]
        );
        if !fixture["valid"].as_bool().unwrap() {
            assert_eq!(result.unwrap_err(), CloudAIError::CorrelationMismatch);
        }
    }
}

#[test]
fn constants_patterns_fields_and_messages_match_canonical_schemas() {
    let request: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../contracts/session-status/request.schema.json"
    )))
    .unwrap();
    let success: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../contracts/session-status/success.schema.json"
    )))
    .unwrap();
    let error: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../contracts/session-status/error.schema.json"
    )))
    .unwrap();
    let common: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../contracts/session-status/common.schema.json"
    )))
    .unwrap();
    assert_eq!(request["x-maxBodyBytes"], MAX_BODY_BYTES);
    assert_eq!(
        request["properties"]["version"],
        json!({"type":"integer", "const":1})
    );
    assert_eq!(
        success["properties"]["version"],
        request["properties"]["version"]
    );
    let context = &request["properties"]["context"]["properties"];
    assert_eq!(context["reported_text"]["maxUtf8Bytes"], MAX_REPORT_BYTES);
    assert_eq!(context["recent_output"]["maxUtf8Bytes"], MAX_OUTPUT_BYTES);
    assert_eq!(context["lifecycle"]["enum"], json!(LIFECYCLES));
    assert_eq!(context["activity_phase"]["enum"], json!(ACTIVITY_PHASES));
    assert_eq!(common["$defs"]["requestId"]["maxLength"], 64);
    assert_eq!(common["$defs"]["requestId"]["minLength"], 1);
    assert_eq!(common["$defs"]["requestId"]["pattern"], "^[A-Za-z0-9]");
    assert_eq!(
        common["$defs"]["requestId"]["not"]["pattern"],
        "[^A-Za-z0-9._-]"
    );
    assert_eq!(common["$defs"]["statusText"]["maxLength"], 140);
    assert_eq!(
        common["$defs"]["sessionTitle"]["maxLength"],
        crate::status::MAX_TITLE_CHARS
    );
    assert_eq!(
        common["$defs"]["sessionTitle"]["maxUtf8Bytes"],
        crate::status::MAX_TITLE_BYTES
    );
    assert_eq!(
        common["$defs"]["sessionTitle"]["pattern"],
        common["$defs"]["statusText"]["pattern"]
    );
    let patterns = CONTEXT_PATTERNS.as_ref().unwrap();
    assert_eq!(
        patterns.meaningful.as_str(),
        common["$defs"]["meaningfulText"]["pattern"]
            .as_str()
            .unwrap()
    );
    assert_eq!(common["$defs"]["contextText"]["not"]["pattern"], "[\u{0000}-\u{0008}\u{000b}\u{000c}\u{000e}-\u{001f}\u{007f}-\u{009f}\\p{Cf}\\p{Cs}\\p{Default_Ignorable_Code_Point}]");
    assert_eq!(
        error["properties"]["error"]["properties"]["retry_after_seconds"],
        json!({"type":"integer", "minimum":1, "maximum":3600})
    );
    let codes = [
        RemoteErrorCode::InvalidRequest,
        RemoteErrorCode::Unauthenticated,
        RemoteErrorCode::Forbidden,
        RemoteErrorCode::RateLimited,
        RemoteErrorCode::UpstreamUnavailable,
        RemoteErrorCode::InvalidOutput,
    ];
    let code_schema = &error["properties"]["error"];
    assert_eq!(
        code_schema["properties"]["code"]["enum"],
        json!(codes.map(RemoteErrorCode::as_str))
    );
    for (code, choice) in codes.iter().zip(code_schema["oneOf"].as_array().unwrap()) {
        assert_eq!(choice["properties"]["code"]["const"], code.as_str());
        assert_eq!(choice["properties"]["message"]["const"], code.message());
        assert!(code.message().len() <= 96);
    }
    for (object, fields) in [
        (&request, vec!["context", "request_id", "version"]),
        (
            &request["properties"]["context"],
            vec![
                "activity_phase",
                "lifecycle",
                "recent_output",
                "reported_text",
            ],
        ),
        (
            &success,
            vec!["request_id", "status_text", "title", "version"],
        ),
        (&error, vec!["error"]),
        (code_schema, vec!["code", "message", "retry_after_seconds"]),
    ] {
        assert_eq!(object["additionalProperties"], false);
        let mut actual: Vec<_> = object["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        actual.sort();
        assert_eq!(actual, fields);
    }
}

#[test]
fn mathematical_integer_spellings_are_exact_not_floating_point_rounded() {
    for number in [
        "1",
        "1.0",
        "1e0",
        "1E+0",
        "10e-1",
        "0.1e1",
        "100.00e-2",
        "1e0000000000000000000000000",
    ] {
        let body = format!(
            r#"{{"version":{number},"request_id":"public","context":{{"lifecycle":"working","reported_text":"Checking"}}}}"#
        );
        assert!(
            ValidatedStatusRequest::from_slice(body.as_bytes()).is_ok(),
            "{number}"
        );
        let response =
            format!(r#"{{"version":{number},"request_id":"public","status_text":"Checking"}}"#);
        assert!(
            decode_success(response.as_bytes(), "public").is_ok(),
            "{number}"
        );
    }
    for number in [
        "0",
        "-1",
        "1.00000000000000000001",
        "0.99999999999999999999",
        "1e-99999",
        "1e99999",
        "true",
        "null",
        "\"1\"",
        "1e-1",
        "2",
    ] {
        let mut body = serde_json::to_string(&base("request")).unwrap();
        body = body.replace("\"version\":1", &format!("\"version\":{number}"));
        assert!(
            ValidatedStatusRequest::from_slice(body.as_bytes()).is_err(),
            "{number}"
        );
    }
    for (number, expected) in [
        ("1.0", Some(1)),
        ("36e2", Some(3600)),
        ("3.600e3", Some(3600)),
        ("3600.000000000000001", None),
        ("1.000000000000000001", None),
        ("0", None),
        ("null", None),
        ("3601", None),
    ] {
        let body = format!(
            r#"{{"error":{{"code":"rate_limited","message":"Too many AI session-status requests.","retry_after_seconds":{number}}}}}"#
        );
        let result = decode_error(body.as_bytes());
        assert_eq!(
            result
                .ok()
                .and_then(|(_, delay)| delay.map(|delay| delay.as_secs())),
            expected,
            "{number}"
        );
    }
}

#[test]
fn optional_nulls_and_duplicate_decoded_members_are_rejected() {
    for field in ["activity_phase", "reported_text", "recent_output"] {
        let mut value = base("request");
        value["context"][field] = Value::Null;
        assert!(
            ValidatedStatusRequest::from_value(&value).is_err(),
            "{field}"
        );
    }
    for body in [
        r#"{"version":1,"version":1,"request_id":"public","context":{"lifecycle":"working","reported_text":"Checking"}}"#,
        r#"{"version":1,"request_id":"public","request_\u0069d":"public","context":{"lifecycle":"working","reported_text":"Checking"}}"#,
        r#"{"version":1,"request_id":"public","context":{"lifecycle":"working","reported_text":"Checking","reported_\u0074ext":"Checking"}}"#,
        r#"{"version":1,"request_id":"public","context":{"lifecycle":"working","activity_phase":"working","activity_phase":"working","reported_text":"Checking"}}"#,
    ] {
        assert!(ValidatedStatusRequest::from_slice(body.as_bytes()).is_err());
    }
    for body in [
        r#"{"version":1,"request_id":"public","request_\u0069d":"public","status_text":"Checking"}"#,
        r#"{"version":1,"request_id":"public","status_text":"Checking","status_\u0074ext":"Checking"}"#,
    ] {
        assert!(decode_success(body.as_bytes(), "public").is_err());
    }
    for body in [
        r#"{"error":{"code":"rate_limited","message":"Too many AI session-status requests.","retry_after_seconds":null}}"#,
        r#"{"error":{"code":"invalid_request","co\u0064e":"invalid_request","message":"Invalid session-status request."}}"#,
        r#"{"error":{"code":"invalid_request","message":"Invalid session-status request."},"\u0065rror":{"code":"invalid_request","message":"Invalid session-status request."}}"#,
        r#"{"error":{"code":"rate_limited","message":"Too many AI session-status requests.","retry_after_seconds":1,"retry_after_\u0073econds":1}}"#,
    ] {
        assert!(decode_error(body.as_bytes()).is_err());
    }
}

#[test]
fn every_schema_object_rejects_positional_array_forms() {
    let context = json!({"lifecycle":"working", "reported_text":"Checking"});
    for value in [
        json!([1, "public", context]),
        json!({"version":1,"request_id":"public","context":["working","running_tool","Checking","Checking"]}),
    ] {
        assert!(ValidatedStatusRequest::from_value(&value).is_err());
    }
    assert!(decode_success(br#"[1,"public","Checking"]"#, "public").is_err());
    for value in [
        json!([{"code":"invalid_request","message":"Invalid session-status request."}]),
        json!({"error":["rate_limited","Too many AI session-status requests.",1]}),
        json!({"error":{"code":{"invalid_request":null},"message":"Invalid session-status request."}}),
    ] {
        assert!(decode_error(&serde_json::to_vec(&value).unwrap()).is_err());
    }
}

#[test]
fn all_enums_request_ids_and_prohibited_private_fields() {
    for lifecycle in LIFECYCLES {
        let mut value = base("request");
        value["context"]["lifecycle"] = json!(lifecycle);
        assert!(ValidatedStatusRequest::from_value(&value).is_ok());
    }
    for phase in ACTIVITY_PHASES {
        let mut value = base("request");
        value["context"]["activity_phase"] = json!(phase);
        assert!(ValidatedStatusRequest::from_value(&value).is_ok());
    }
    for (id, valid) in [
        (json!("a"), true),
        (json!("a".repeat(64)), true),
        (json!("a".repeat(65)), false),
        (json!("A0-_.z"), true),
        (json!(""), false),
        (json!("_public"), false),
        (json!("public id"), false),
        (json!("public/id"), false),
        (json!("publiqué"), false),
        (json!("public\n"), false),
        (json!(1), false),
    ] {
        for schema in ["request", "success"] {
            let mut value = base(schema);
            value["request_id"] = id.clone();
            assert_eq!(accepts(schema, &serde_json::to_vec(&value).unwrap()), valid);
        }
    }
    for field in [
        "token",
        "access_token",
        "refresh_token",
        "id_token",
        "session_token",
        "session_id",
        "user_id",
        "organization_id",
        "customer_id",
        "billing",
        "path",
        "cwd",
        "scope",
        "database",
        "provider_url",
        "model",
        "tools",
        "messages",
        "__proto__",
        "constructor",
        "prototype",
    ] {
        for (schema, nested) in [
            ("request", None),
            ("request", Some("context")),
            ("success", None),
            ("error", None),
            ("error", Some("error")),
        ] {
            let mut value = base(schema);
            let target = match nested {
                Some(key) => &mut value[key],
                None => &mut value,
            };
            target[field] = json!("synthetic-rejected-value");
            assert!(
                !accepts(schema, &serde_json::to_vec(&value).unwrap()),
                "{schema} {field}"
            );
        }
    }
}

#[test]
fn canonical_context_controls_meaningfulness_and_both_source_limits() {
    for codepoint in (0..32).chain(127..=159) {
        let character = char::from_u32(codepoint).unwrap();
        for field in ["reported_text", "recent_output"] {
            let mut value = base("request");
            value["context"][field] = json!(format!("Checking{character}"));
            assert_eq!(
                ValidatedStatusRequest::from_value(&value).is_ok(),
                matches!(character, '\t' | '\n' | '\r'),
                "{field} {codepoint}"
            );
        }
        let mut value = base("success");
        value["status_text"] = json!(format!("Checking{character}"));
        assert!(!accepts("success", &serde_json::to_vec(&value).unwrap()));
    }
    for codepoint in [
        0x034f, 0x061c, 0x115f, 0x1160, 0x17b4, 0x180e, 0x200b, 0x200c, 0x200d, 0x200e, 0x200f,
        0x202a, 0x202b, 0x202c, 0x202d, 0x202e, 0x2060, 0x2067, 0x2068, 0x2069, 0xfe0f, 0xfeff,
        0xffa0, 0x1bca0, 0xe0001, 0xe0100,
    ] {
        let mut value = base("request");
        value["context"]["recent_output"] =
            json!(format!("Checking{}", char::from_u32(codepoint).unwrap()));
        assert!(
            ValidatedStatusRequest::from_value(&value).is_err(),
            "{codepoint}"
        );
    }
    for (text, valid) in [
        ("!", true),
        ("€", true),
        ("Ⅻ", true),
        ("界", true),
        ("\u{0301}", false),
        ("\u{e000}", false),
        (" \t\r\n", false),
        ("Checking\u{2028}\u{2029}", true),
    ] {
        let mut value = base("request");
        value["context"]["reported_text"] = json!(text);
        assert_eq!(ValidatedStatusRequest::from_value(&value).is_ok(), valid);
    }
    let mut value = base("request");
    value["context"]["reported_text"] = json!("a".repeat(MAX_REPORT_BYTES));
    value["context"]["recent_output"] = json!("😀".repeat(MAX_OUTPUT_BYTES / 4));
    assert!(ValidatedStatusRequest::from_value(&value).is_ok());
}

#[test]
fn response_bounds_utf8_surrogates_and_diagnostics_never_bypass_validation() {
    for schema in ["success", "error"] {
        let mut body = serde_json::to_vec(&base(schema)).unwrap();
        body.resize(MAX_BODY_BYTES, b' ');
        assert!(accepts(schema, &body));
        body.push(b' ');
        assert!(!accepts(schema, &body));
        assert!(!accepts(schema, &[0xff]));
        assert!(!accepts(schema, b"\xef\xbb\xbf{}"));
        assert!(!accepts(schema, b"{}{}"));
    }
    for surrogate in [r"\ud800", r"\udfff", r"\ud800x", r"\ud800\ud800"] {
        let body =
            format!(r#"{{"version":1,"request_id":"public","status_text":"Checking{surrogate}"}}"#);
        assert!(decode_success(body.as_bytes(), "public").is_err());
    }
    assert!(decode_success(
        br#"{"version":1,"request_id":"public","status_text":"Checking\ud83d\ude00"}"#,
        "public"
    )
    .is_ok());
    let mut oversized = base("request");
    oversized["context"]["recent_output"] = json!("a".repeat(MAX_BODY_BYTES * 2));
    assert_eq!(
        ValidatedStatusRequest::from_value(&oversized).unwrap_err(),
        CloudAIError::InvalidRequest
    );
    let request = ValidatedStatusRequest::from_value(&base("request")).unwrap();
    let response = decode_success(
        &serde_json::to_vec(&base("success")).unwrap(),
        request.request_id(),
    )
    .unwrap();
    for debug in [format!("{request:?}"), format!("{response:?}")] {
        assert!(debug.contains("REDACTED"));
        assert!(!debug.contains("synthetic-001"));
        assert!(!debug.contains("Adding validation tests"));
    }
    assert_eq!(response.request_id(), request.request_id());
    assert_eq!(response.text().as_str(), "Adding validation tests");
    assert_eq!(response.into_text().as_str(), "Adding validation tests");
}

#[test]
fn a_title_is_optional_and_kept_exactly() {
    let titled = decode_success(
        br#"{"version":1,"request_id":"public","status_text":"Checking","title":"Fix the login redirect"}"#,
        "public",
    )
    .unwrap();
    assert_eq!(
        titled.title().map(SessionTitle::as_str),
        Some("Fix the login redirect")
    );
    assert!(!format!("{titled:?}").contains("login"));
    let (text, title) = titled.into_parts();
    assert_eq!(text.as_str(), "Checking");
    assert!(title.is_some());
    // An older service sends none.
    let untitled = decode_success(
        br#"{"version":1,"request_id":"public","status_text":"Checking"}"#,
        "public",
    )
    .unwrap();
    assert!(untitled.title().is_none());
    for body in [
        &br#"{"version":1,"request_id":"public","status_text":"Checking","title":null}"#[..],
        br#"{"version":1,"request_id":"public","status_text":"Checking","title":"Fix it."}"#,
        br#"{"version":1,"request_id":"public","status_text":"Checking","title":"Fix","title":"Fix"}"#,
    ] {
        assert_eq!(
            decode_success(body, "public").unwrap_err(),
            CloudAIError::InvalidResponse
        );
    }
}
