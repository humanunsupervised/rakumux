use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;

use super::*;

const NOW: Duration = Duration::from_secs(100);

fn token(url: &str) -> RealtimeToken {
    RealtimeToken {
        url: url.into(),
        token: "v1.synthetic-claims.synthetic-signature".into(),
    }
}

#[test]
fn a_token_is_asked_for_once_on_and_a_connection_follows() {
    let mut pokes = PokeSync::new();
    assert!(!pokes.next_request(NOW), "not before the feed is on");
    assert!(!pokes.set_enabled(true, NOW));
    assert!(pokes.next_request(NOW));
    assert!(!pokes.next_request(NOW), "one request at a time");
    let (got, generation) = pokes
        .token_done(true, Ok(token("wss://realtime.example/v1/connect")), NOW)
        .unwrap();
    assert_eq!(got.url, "wss://realtime.example/v1/connect");
    assert!(pokes.current(generation));
    assert!(!pokes.next_request(NOW), "not while connected");
    pokes.connected();
    // Closed: connected again a second later, then backing off.
    pokes.closed(NOW);
    assert!(!pokes.current(generation));
    assert!(!pokes.next_request(NOW));
    assert!(pokes.next_request(NOW + RETRY_MIN));
    let (_, again) = pokes
        .token_done(true, Ok(token("wss://realtime.example/v1/connect")), NOW)
        .unwrap();
    assert!(again > generation);
    pokes.closed(NOW);
    assert!(!pokes.next_request(NOW + RETRY_MIN));
    assert!(pokes.next_request(NOW + 2 * RETRY_MIN));
}

#[test]
fn a_cloud_without_pokes_is_asked_again_much_later() {
    let mut pokes = PokeSync::new();
    pokes.set_enabled(true, NOW);
    assert!(pokes.next_request(NOW));
    assert_eq!(pokes.token_done(true, Err(FeedFailure::Invalid), NOW), None);
    assert!(!pokes.next_request(NOW + RETRY_MAX));
    assert!(pokes.next_request(NOW + NOT_OFFERED_WAIT));
}

#[test]
fn turning_off_stops_the_connection_and_its_events_count_no_more() {
    let mut pokes = PokeSync::new();
    pokes.set_enabled(true, NOW);
    pokes.next_request(NOW);
    let (_, generation) = pokes
        .token_done(true, Ok(token("wss://realtime.example/v1/connect")), NOW)
        .unwrap();
    assert!(
        pokes.set_enabled(false, NOW),
        "the running connection stops"
    );
    assert!(!pokes.current(generation));
    assert!(!pokes.next_request(NOW));
    // A token asked for by an account signed out since is dropped.
    pokes.set_enabled(true, NOW);
    pokes.next_request(NOW);
    assert_eq!(
        pokes.token_done(false, Ok(token("wss://realtime.example/v1/connect")), NOW),
        None
    );
}

#[test]
fn only_a_poke_is_a_poke() {
    assert!(is_poke(r#"{"version":1,"type":"poke"}"#));
    assert!(!is_poke(r#"{"version":1,"type":"poke","text":"a reply"}"#));
    assert!(!is_poke(r#"{"version":2,"type":"poke"}"#));
    assert!(!is_poke("pong"));
}

#[test]
fn a_connection_hears_pokes_until_the_server_closes_it() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (seen, header) = mpsc::channel();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut socket = tungstenite::accept_hdr(
            stream,
            |request: &tungstenite::handshake::server::Request, response| {
                let authorization = request
                    .headers()
                    .get("authorization")
                    .map(|v| v.to_str().unwrap().to_owned());
                seen.send((request.uri().path().to_owned(), authorization))
                    .unwrap();
                Ok(response)
            },
        )
        .unwrap();
        socket.send(tungstenite::Message::text("pong")).unwrap();
        socket
            .send(tungstenite::Message::text(r#"{"version":1,"type":"poke"}"#))
            .unwrap();
        socket
            .send(tungstenite::Message::text(r#"{"version":1,"type":"poke"}"#))
            .unwrap();
        socket
            .close(Some(tungstenite::protocol::CloseFrame {
                code: tungstenite::protocol::frame::coding::CloseCode::Library(4000),
                reason: "replaced".into(),
            }))
            .unwrap();
        // Until the client answers the close.
        while socket.read().is_ok() {}
    });
    let stop = AtomicBool::new(false);
    let mut events = Vec::new();
    listen(
        &token(&format!("ws://127.0.0.1:{port}/v1/connect")),
        &stop,
        |event| {
            events.push(event);
            true
        },
    );
    server.join().unwrap();
    assert_eq!(
        header.recv().unwrap(),
        (
            "/v1/connect".to_owned(),
            Some("Bearer v1.synthetic-claims.synthetic-signature".to_owned())
        ),
        "the token goes as the bearer, never in the URL"
    );
    assert_eq!(
        events,
        [
            PokeEvent::Connected,
            PokeEvent::Poked,
            PokeEvent::Poked,
            PokeEvent::Closed
        ]
    );
}

#[test]
fn a_connection_stops_when_asked_and_says_nothing_more() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();
        while socket.read().is_ok() {}
    });
    let stop = Arc::new(AtomicBool::new(false));
    let stopper = stop.clone();
    let mut events = Vec::new();
    listen(
        &token(&format!("ws://127.0.0.1:{port}/v1/connect")),
        &stop,
        |event| {
            events.push(event);
            stopper.store(true, Ordering::SeqCst);
            true
        },
    );
    server.join().unwrap();
    assert_eq!(events, [PokeEvent::Connected, PokeEvent::Closed]);
}

#[test]
fn nowhere_to_connect_is_closed_at_once() {
    let stop = AtomicBool::new(false);
    for url in [
        "ws://realtime.example/v1/connect",
        "https://realtime.example/v1/connect",
    ] {
        let mut events = Vec::new();
        listen(&token(url), &stop, |event| {
            events.push(event);
            true
        });
        assert_eq!(events, [PokeEvent::Closed], "{url}");
    }
    // Nothing listens there.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut events = Vec::new();
    listen(
        &token(&format!("ws://127.0.0.1:{port}/v1/connect")),
        &stop,
        |event| {
            events.push(event);
            true
        },
    );
    assert_eq!(events, [PokeEvent::Closed]);
}

/// Against a realtime worker running on this machine, with a token for it
/// (`RAKUMUX_REALTIME_TEST_URL`, `RAKUMUX_REALTIME_TEST_TOKEN`): poke its
/// account within 30 s of starting. Run with `--ignored`.
#[test]
#[ignore]
fn a_running_worker_pokes() {
    let token = RealtimeToken {
        url: std::env::var("RAKUMUX_REALTIME_TEST_URL").unwrap(),
        token: std::env::var("RAKUMUX_REALTIME_TEST_TOKEN").unwrap(),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let watchdog = stop.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(30));
        watchdog.store(true, Ordering::SeqCst);
    });
    let mut events = Vec::new();
    listen(&token, &stop, |event| {
        events.push(event);
        if event == PokeEvent::Poked {
            stop.store(true, Ordering::SeqCst);
        }
        true
    });
    assert_eq!(
        events,
        [PokeEvent::Connected, PokeEvent::Poked, PokeEvent::Closed]
    );
}
