use super::*;
use crate::source::fake::{live_view, FakeSessionApi};

/// The daemon features of the next resync `follow` sends, past its
/// disconnects; `None` when none comes within 5 s.
fn next_resync(inbox: &Receiver<Message>) -> Option<DaemonFeatures> {
    loop {
        match inbox.recv_timeout(Duration::from_secs(5)) {
            Ok(Message::Resync(_, daemon)) => return Some(daemon),
            Ok(Message::Disconnected) => {}
            Ok(_) | Err(_) => return None,
        }
    }
}

#[test]
fn the_daemons_features_are_read_again_at_each_resubscribe() {
    let view = live_view(&SessionId::from_string("S"), "/home/me/rakumux", 1);
    let api = FakeSessionApi::user(view.clone(), "");
    api.set_listed(vec![view]);
    // A daemon from before the guard (updated with `U`, not restarted).
    api.set_capabilities(&[CAPABILITY_RESOURCES]);
    let first = api.stream();
    let (owner, inbox) = mpsc::channel();
    let stop = AtomicBool::new(false);
    let (before, after) = thread::scope(|scope| {
        scope.spawn(|| follow(&api, &owner, &stop));
        let before = next_resync(&inbox);
        // Restarted in place on this build: its stream ends, and the hello
        // of the connection that subscribes again announces the guard.
        api.set_capabilities(&[
            CAPABILITY_RESOURCES,
            CAPABILITY_SUBMIT_BLOCKED,
            CAPABILITY_ANSWER,
        ]);
        let _second = api.stream();
        drop(first);
        let after = next_resync(&inbox);
        // Stopped before anything is checked, so a failure never leaves it running.
        stop.store(true, Ordering::SeqCst);
        (before, after)
    });
    assert_eq!(
        before,
        Some(DaemonFeatures {
            resources: true,
            guards_prompts: false,
            answers: false,
        })
    );
    assert_eq!(
        after,
        Some(DaemonFeatures {
            resources: true,
            guards_prompts: true,
            answers: true,
        })
    );
    // Capabilities not known (an adapter, a fake): neither.
    assert_eq!(DaemonFeatures::of(None), DaemonFeatures::default());
}
