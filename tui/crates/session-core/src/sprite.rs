//! The Fly Sprite this runs on, if it runs on one: the Sprite's own API, a
//! Unix socket inside every Sprite. A Sprite pauses moments after its last
//! connection closes, freezing everything in it, unless a task (Fly's Tasks
//! API) holds it running; the daemon holds one while agents work, the account
//! service one after waking, while it reads and types replies.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

/// The Sprite's own API, inside every Sprite; absent elsewhere.
pub const SOCKET: &str = "/.sprite/api.sock";

/// Hold the Sprite running with task `name` for `expire` (a Fly duration,
/// such as `"5m"`) from now: the task ends by itself then, unless held again.
pub fn hold(socket: &Path, name: &str, expire: &str) -> io::Result<()> {
    let body = format!(r#"{{"expire":"{expire}"}}"#);
    expect_ok(request(socket, "PUT", &format!("/v1/tasks/{name}"), &body)?)
}

/// End task `name`; one that's gone already is no error.
pub fn release(socket: &Path, name: &str) -> io::Result<()> {
    match request(socket, "DELETE", &format!("/v1/tasks/{name}"), "")? {
        404 => Ok(()),
        status => expect_ok(status),
    }
}

fn expect_ok(status: u16) -> io::Result<()> {
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "the Sprite's API answered {status}"
        )))
    }
}

/// One HTTP/1.1 request on the Sprite's socket; its status code.
fn request(socket: &Path, method: &str, path: &str, body: &str) -> io::Result<u16> {
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
        .ok_or_else(|| io::Error::other(format!("the Sprite's API answered {line:?}")))
}
