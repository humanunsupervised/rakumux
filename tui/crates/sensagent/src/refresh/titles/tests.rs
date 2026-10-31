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
    let mut again = Titles::load(Some(path.clone()));
    again.keep_only(&HashSet::from([id(3).as_str()]));
    assert_eq!(Titles::load(Some(path.clone())).get(&id(2)), None);

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

fn chats(titled: &TitledChats, session: &SessionId) -> Vec<String> {
    let mut chats: Vec<String> = titled
        .iter()
        .filter(|(id, _)| id == session)
        .map(|(_, chat)| chat.to_owned())
        .collect();
    chats.sort();
    chats
}

#[test]
fn titled_chats_are_remembered_in_a_private_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("titled-chats.json");
    let mut titled = TitledChats::load(Some(path.clone()));
    assert_eq!(titled.iter().count(), 0);
    titled.add(&id(1), "synthetic-chat-a");
    titled.add(&id(1), "synthetic-chat-a");
    titled.add(&id(1), "");
    titled.add(&id(2), "synthetic-chat-b");
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);

    let mut again = TitledChats::load(Some(path.clone()));
    assert_eq!(chats(&again, &id(1)), ["", "synthetic-chat-a"]);
    assert_eq!(chats(&again, &id(2)), ["synthetic-chat-b"]);
    // Deleted by the daemon, or not among the sessions it has: forgotten.
    again.forget(&id(1));
    let mut again = TitledChats::load(Some(path.clone()));
    assert!(chats(&again, &id(1)).is_empty());
    again.add(&id(3), "synthetic-chat-c");
    again.keep_only(&HashSet::from([id(3).as_str()]));
    let mut again = TitledChats::load(Some(path.clone()));
    assert!(chats(&again, &id(2)).is_empty());
    assert_eq!(chats(&again, &id(3)), ["synthetic-chat-c"]);
    // Sharing stopped: every session is titled again once it resumes.
    again.clear();
    assert_eq!(TitledChats::load(Some(path.clone())).iter().count(), 0);

    std::fs::write(&path, b"not json").unwrap();
    assert_eq!(TitledChats::load(Some(path)).iter().count(), 0);
}

#[test]
fn a_sessions_newest_chats_and_the_most_recent_sessions_are_kept() {
    let mut titled = TitledChats::load(None);
    for n in 0..=MAX_CHATS {
        titled.add(&id(1), &format!("synthetic-chat-{n:02}"));
    }
    let kept = chats(&titled, &id(1));
    assert_eq!(kept.len(), MAX_CHATS);
    assert!(!kept.contains(&"synthetic-chat-00".to_owned()));
    assert!(kept.contains(&format!("synthetic-chat-{MAX_CHATS:02}")));
    // An id longer than any conversation's isn't kept.
    titled.add(&id(2), &"x".repeat(MAX_CHAT_CHARS + 1));
    assert!(chats(&titled, &id(2)).is_empty());

    for n in 0..=MAX_TITLES {
        titled.add(&SessionId::from_string(format!("synthetic-{n}")), "chat");
    }
    assert_eq!(titled.sessions.len(), MAX_TITLES);
    assert!(chats(&titled, &SessionId::from_string("synthetic-0")).is_empty());
}

#[test]
fn diagnostics_never_show_titles() {
    let retitle = retitle("Synthetic private title", false, Some("Older title"));
    assert!(!format!("{retitle:?}").contains("private"));
    let mut titles = Titles::load(None);
    titles.set(&id(1), "Synthetic private title".into());
    assert!(!format!("{titles:?}").contains("private"));
    let mut titled = TitledChats::load(None);
    titled.add(&id(1), "synthetic-private-chat");
    assert!(!format!("{titled:?}").contains("private"));
}
