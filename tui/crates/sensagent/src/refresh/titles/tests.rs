use std::os::unix::fs::PermissionsExt;

use session_core::model::{ScopeRoot, SessionStatus, SessionView};

use super::*;
use crate::source::fake::{live_view, FakeSessionApi};

const FOLDER: &str = "/synthetic/titles-folder";

fn id(number: u8) -> SessionId {
    SessionId::from_string(format!("synthetic-title-session-{number}"))
}

fn view(number: u8, name: &str, auto_named: bool) -> SessionView {
    let mut view = live_view(&id(number), FOLDER, 4000 + u32::from(number));
    view.name = name.into();
    view.auto_named = auto_named;
    view
}

fn api(views: Vec<SessionView>) -> FakeSessionApi {
    let api = FakeSessionApi::user(views[0].clone(), "");
    api.set_listed(views);
    api
}

fn retitle(title: &str, asked: bool, previous: Option<&str>) -> Retitle {
    Retitle {
        session: id(1),
        title: SessionTitle::try_from(title.to_owned()).unwrap(),
        asked,
        previous: previous.map(Into::into),
    }
}

fn renames(api: &FakeSessionApi) -> Vec<String> {
    api.calls()
        .into_iter()
        .filter(|call| call.starts_with("rename "))
        .collect()
}

#[test]
fn a_generated_name_takes_the_title() {
    let api = api(vec![view(1, "brisk-otter", true)]);
    let done = retitle("Fix the login redirect", false, None).apply(&api);
    assert_eq!(
        done.outcome,
        RetitleOutcome::Named("Fix the login redirect".into())
    );
    assert_eq!(
        renames(&api),
        [format!("rename {} Fix the login redirect", id(1))]
    );
}

#[test]
fn a_name_the_user_gave_is_kept() {
    let api = api(vec![view(1, "my login work", false)]);
    let done = retitle("Fix the login redirect", false, None).apply(&api);
    assert_eq!(done.outcome, RetitleOutcome::Kept);
    assert!(renames(&api).is_empty());
    // Not even when it was the service's title once, then renamed.
    let done = retitle("Fix the login redirect", false, Some("Login tests")).apply(&api);
    assert_eq!(done.outcome, RetitleOutcome::Kept);
    assert!(renames(&api).is_empty());
}

#[test]
fn the_services_last_title_is_replaced() {
    let api = api(vec![view(1, "Login tests", false)]);
    let done = retitle("Fix the login redirect", false, Some("Login tests")).apply(&api);
    assert_eq!(
        done.outcome,
        RetitleOutcome::Named("Fix the login redirect".into())
    );
}

#[test]
fn a_title_asked_for_replaces_any_name() {
    let api = api(vec![view(1, "my login work", false)]);
    let done = retitle("Fix the login redirect", true, None).apply(&api);
    assert_eq!(
        done.outcome,
        RetitleOutcome::Named("Fix the login redirect".into())
    );
}

#[test]
fn a_title_another_session_of_the_folder_has_gets_a_number() {
    let mut elsewhere = view(4, "Fix tests 3", false);
    elsewhere.scope = ScopeRoot::from_canonical("/synthetic/another-folder".into());
    let mut dead = view(3, "Fix tests 2", false);
    dead.status = SessionStatus::Dead;
    let api = api(vec![
        view(1, "brisk-otter", true),
        view(2, "Fix tests", false),
        dead,
        elsewhere,
    ]);
    let done = retitle("Fix tests", false, None).apply(&api);
    assert_eq!(done.outcome, RetitleOutcome::Named("Fix tests 3".into()));
}

#[test]
fn a_session_already_named_its_title_is_not_renamed() {
    let api = api(vec![
        view(1, "Fix tests 2", false),
        view(2, "Fix tests", false),
    ]);
    let done = retitle("Fix tests", false, Some("Fix tests 2")).apply(&api);
    assert_eq!(done.outcome, RetitleOutcome::Named("Fix tests 2".into()));
    assert!(renames(&api).is_empty());
}

#[test]
fn every_number_taken_or_an_unreadable_session_fails() {
    let mut views = vec![view(1, "brisk-otter", true), view(2, "Fix", false)];
    for n in 2..=MAX_SUFFIX {
        views.push(view(n as u8 + 1, &format!("Fix {n}"), false));
    }
    let full = api(views);
    assert_eq!(
        retitle("Fix", false, None).apply(&full).outcome,
        RetitleOutcome::Failed
    );
    let gone = api(vec![view(2, "someone-else", true)]);
    gone.script(vec![Err(session_core::api::ApiError::new(
        ErrorCode::NotFound,
        "synthetic-gone",
    ))]);
    assert_eq!(
        retitle("Fix", false, None).apply(&gone).outcome,
        RetitleOutcome::Failed
    );
}

#[test]
fn the_longest_title_with_a_number_still_fits_a_name() {
    let longest = "a".repeat(crate::status::MAX_TITLE_CHARS);
    let numbered = format!("{longest} {MAX_SUFFIX}");
    assert!(numbered.chars().count() <= MAX_NAME_CHARS);
}

#[test]
fn titles_are_remembered_in_a_private_file_and_forgotten_with_their_session() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("titles.json");
    let mut titles = Titles::load(Some(path.clone()));
    assert_eq!(titles.get(&id(1)), None);
    titles.set(&id(1), "Fix tests".into());
    titles.set(&id(2), "Write docs".into());
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    let mut again = Titles::load(Some(path.clone()));
    assert_eq!(again.get(&id(1)).as_deref(), Some("Fix tests"));
    again.forget(&id(1));
    let again = Titles::load(Some(path.clone()));
    assert_eq!(again.get(&id(1)), None);
    assert_eq!(again.get(&id(2)).as_deref(), Some("Write docs"));

    std::fs::write(&path, b"not json").unwrap();
    assert_eq!(Titles::load(Some(path)).get(&id(2)), None);
}

#[test]
fn only_the_most_recent_titles_are_kept() {
    let mut titles = Titles::load(None);
    for n in 0..=MAX_TITLES {
        titles.set(
            &SessionId::from_string(format!("synthetic-{n}")),
            format!("Title {n}"),
        );
    }
    assert_eq!(titles.entries.len(), MAX_TITLES);
    assert_eq!(titles.get(&SessionId::from_string("synthetic-0")), None);
    assert!(titles
        .get(&SessionId::from_string(format!("synthetic-{MAX_TITLES}")))
        .is_some());
}

#[test]
fn diagnostics_never_show_titles() {
    let retitle = retitle("Synthetic private title", false, Some("Older title"));
    assert!(!format!("{retitle:?}").contains("private"));
    let mut titles = Titles::load(None);
    titles.set(&id(1), "Synthetic private title".into());
    assert!(!format!("{titles:?}").contains("private"));
}
