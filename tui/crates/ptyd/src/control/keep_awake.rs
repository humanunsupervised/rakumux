//! Keep a Sprite running while its agents work. A Sprite pauses about 30 s
//! after its last connection closes, freezing everything in it, so an agent
//! left working with nobody connected would stop mid-turn. Fly's Tasks API
//! (`/.sprite/api.sock`) holds it running: while an agent works, and wrote
//! output in the last [`QUIET_MS`], the daemon keeps a task alive; when none
//! does, it lets go. Elsewhere (no socket) this does nothing.

use super::service::Inner;
use crate::{log_debug, log_info, log_warn};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

/// The Sprite's own API, inside every Sprite.
const SOCKET: &str = "/.sprite/api.sock";
const TASK: &str = "rakumux-agents";
/// How often the agents are looked at.
const EVERY: Duration = Duration::from_secs(15);
/// The task outlives a missed renewal or two, and ends by itself if the
/// daemon goes.
const EXPIRE: &str = "5m";
const RENEW: Duration = Duration::from_secs(60);
/// A working agent that wrote nothing for this long no longer holds the
/// Sprite: a hung one mustn't keep it billing.
pub(super) const QUIET_MS: i64 = 10 * 60 * 1000;

pub(super) fn start(inner: &Arc<Inner>) {
    let socket = PathBuf::from(SOCKET);
    if !socket.exists() {
        return;
    }
    let weak = Arc::downgrade(inner);
    let spawned = std::thread::Builder::new()
        .name("ptyd-keep-awake".into())
        .spawn(move || run(weak, &socket));
    match spawned {
        Ok(_) => log_info!("keep-awake: holding this Sprite while agents work"),
        Err(error) => log_warn!("keep-awake: cannot start: {error}"),
    }
}

fn run(inner: Weak<Inner>, socket: &Path) {
    let mut held: Option<Instant> = None;
    loop {
        std::thread::sleep(EVERY);
        let Some(inner) = inner.upgrade() else {
            let _ = release(socket);
            return;
        };
        if inner.stop.load(Ordering::SeqCst) {
            let _ = release(socket);
            return;
        }
        let busy = inner.busy_agents(QUIET_MS);
        drop(inner);
        if busy > 0 {
            if held.is_none_or(|at| at.elapsed() >= RENEW) {
                match hold(socket) {
                    Ok(()) => {
                        if held.is_none() {
                            log_debug!("keep-awake: {busy} agent(s) working, holding the Sprite");
                        }
                        held = Some(Instant::now());
                    }
                    Err(error) => log_warn!("keep-awake: {error}"),
                }
            }
        } else if held.take().is_some() {
            match release(socket) {
                Ok(()) => log_debug!("keep-awake: no agent working, letting the Sprite pause"),
                Err(error) => log_warn!("keep-awake: {error}"),
            }
        }
    }
}

fn hold(socket: &Path) -> std::io::Result<()> {
    let body = format!(r#"{{"expire":"{EXPIRE}"}}"#);
    expect_ok(request(socket, "PUT", &format!("/v1/tasks/{TASK}"), &body)?)
}

fn release(socket: &Path) -> std::io::Result<()> {
    match request(socket, "DELETE", &format!("/v1/tasks/{TASK}"), "")? {
        404 => Ok(()),
        status => expect_ok(status),
    }
}

fn expect_ok(status: u16) -> std::io::Result<()> {
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "the Sprite's API answered {status}"
        )))
    }
}

/// One HTTP/1.1 request on the Sprite's socket; its status code.
fn request(socket: &Path, method: &str, path: &str, body: &str) -> std::io::Result<u16> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: sprite\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n") && head.len() < 256 {
        if stream.read(&mut byte)? == 0 {
            break;
        }
        head.push(byte[0]);
    }
    let line = String::from_utf8_lossy(&head);
    line.split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| std::io::Error::other(format!("the Sprite's API answered {line:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    /// Answer one request with `status`, returning what was asked: its head
    /// and its `Content-Length` of body, however many reads they take.
    fn serve_once(listener: UnixListener, status: u16) -> std::thread::JoinHandle<String> {
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut asked = Vec::new();
            let mut buf = [0u8; 1024];
            loop {
                let text = String::from_utf8_lossy(&asked).into_owned();
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("Content-Length: "))
                        .map_or(0, |n| n.parse::<usize>().unwrap());
                    if body.len() >= length {
                        break;
                    }
                }
                let n = stream.read(&mut buf).unwrap();
                assert!(n > 0, "the request ended early: {text:?}");
                asked.extend_from_slice(&buf[..n]);
            }
            write!(stream, "HTTP/1.1 {status} X\r\nContent-Length: 0\r\n\r\n").unwrap();
            String::from_utf8_lossy(&asked).into_owned()
        })
    }

    #[test]
    fn holds_and_releases_through_the_tasks_api() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("api.sock");

        let asked = serve_once(UnixListener::bind(&socket).unwrap(), 200);
        hold(&socket).unwrap();
        let asked = asked.join().unwrap();
        assert!(asked.starts_with("PUT /v1/tasks/rakumux-agents HTTP/1.1\r\nHost: sprite\r\n"));
        assert!(asked.ends_with(r#"{"expire":"5m"}"#));

        std::fs::remove_file(&socket).unwrap();
        let asked = serve_once(UnixListener::bind(&socket).unwrap(), 404);
        release(&socket).unwrap();
        assert!(asked
            .join()
            .unwrap()
            .starts_with("DELETE /v1/tasks/rakumux-agents "));

        std::fs::remove_file(&socket).unwrap();
        let asked = serve_once(UnixListener::bind(&socket).unwrap(), 500);
        assert!(hold(&socket).is_err());
        asked.join().unwrap();
    }
}
