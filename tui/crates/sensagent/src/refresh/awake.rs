//! Staying up after waking, on a Sprite. A paused Sprite runs again for a
//! request to its URL (rakumux.com sends one when a reply is written to it),
//! but only while that request lasts: too short to read its replies and type
//! them in. The owner notices it was paused, its tick late, and holds the
//! Sprite running for a while ([`HOLD`]) with a task of its own
//! (`session_core::sprite`), held again for each reply typed in, so the
//! daemon's own hold (while the agent works) takes over. Elsewhere (no
//! Sprite socket) holding does nothing.

use std::path::PathBuf;
use std::time::Duration;

use session_core::sprite;

/// A tick this late means the machine was paused (a Sprite) or asleep.
pub(super) const PAUSED: Duration = Duration::from_secs(5);
/// How long the Sprite stays up after waking, or after a reply is typed in:
/// a new access token, a replies read, typing, and the daemon seeing the
/// agent work (it looks every 15 s).
const HOLD: Duration = Duration::from_secs(20);
const TASK: &str = "rakumux-replies";
/// The task ends by itself if the service goes.
const EXPIRE: &str = "1m";

/// The owner's hold on the Sprite, if it runs on one.
pub(super) struct Awake {
    socket: Option<PathBuf>,
    /// Held until then (owner time).
    until: Option<Duration>,
}

impl Awake {
    /// On a Sprite when its socket is there.
    pub(super) fn new() -> Self {
        let socket = PathBuf::from(sprite::SOCKET);
        Self::at(socket.exists().then_some(socket))
    }

    fn at(socket: Option<PathBuf>) -> Self {
        Self {
            socket,
            until: None,
        }
    }

    /// Hold the Sprite up for [`HOLD`] from `now` (longer if held already).
    pub(super) fn hold(&mut self, now: Duration) {
        let Some(socket) = &self.socket else {
            return;
        };
        let until = now + HOLD;
        if self.until.is_some_and(|held| held >= until) {
            return;
        }
        match sprite::hold(socket, TASK, EXPIRE) {
            Ok(()) => self.until = Some(until),
            // Static text only (see `refresh`).
            Err(_) => eprintln!("rakumux-status: holding the Sprite up failed"),
        }
    }

    /// Let it pause once the hold is over.
    pub(super) fn tick(&mut self, now: Duration) {
        let (Some(socket), Some(until)) = (&self.socket, self.until) else {
            return;
        };
        if now < until {
            return;
        }
        self.until = None;
        if sprite::release(socket, TASK).is_err() {
            eprintln!("rakumux-status: letting the Sprite pause failed");
        }
    }

    /// Whether `now` comes so long after the last tick that the machine was
    /// paused or asleep meanwhile.
    pub(super) fn woke(last_tick: Duration, now: Duration) -> bool {
        now.saturating_sub(last_tick) > PAUSED
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;

    #[test]
    fn a_late_tick_means_a_pause() {
        let at = Duration::from_secs(100);
        assert!(!Awake::woke(at, at + Duration::from_secs(1)));
        assert!(!Awake::woke(at, at + PAUSED));
        assert!(Awake::woke(at, at + Duration::from_secs(90)));
    }

    #[test]
    fn off_a_sprite_holding_does_nothing() {
        let mut awake = Awake::at(None);
        awake.hold(Duration::ZERO);
        assert_eq!(awake.until, None);
        awake.tick(Duration::from_secs(60));
    }

    #[test]
    fn the_sprite_is_held_after_waking_then_let_go() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("api.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let asked = std::thread::spawn(move || {
            let mut asked = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                // The whole head, however many reads it takes.
                let mut head = Vec::new();
                let mut buf = [0u8; 512];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = stream.read(&mut buf).unwrap();
                    if n == 0 {
                        break;
                    }
                    head.extend_from_slice(&buf[..n]);
                }
                let head = String::from_utf8_lossy(&head).into_owned();
                asked.push(head.lines().next().unwrap_or_default().to_owned());
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").unwrap();
            }
            asked
        });
        let mut awake = Awake::at(Some(socket));
        awake.hold(Duration::ZERO);
        // Held as long already: not asked again.
        awake.hold(Duration::ZERO);
        awake.tick(Duration::from_secs(19));
        awake.tick(HOLD);
        assert_eq!(
            asked.join().unwrap(),
            [
                "PUT /v1/tasks/rakumux-replies HTTP/1.1",
                "DELETE /v1/tasks/rakumux-replies HTTP/1.1"
            ]
        );
    }
}
