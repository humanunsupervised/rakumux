//! Byte framing on the daemon socket.
//!
//! Every frame is `[u32 BE length][u8 kind][payload]`, where `length` counts
//! the kind byte and the payload.
//!
//! - kind 0, **control**: UTF-8 JSON (`api::Request`, `api::Response`,
//!   `api::StreamCommand`, `api::StreamEvent`, `api::EventEnvelope`).
//! - kind 1, **data**: raw terminal bytes. Daemon → client output frames
//!   start with a `u64 BE` output offset; client → daemon input frames are
//!   just the bytes.

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::io::{self, Read, Write};

pub const MAX_FRAME: usize = 8 * 1024 * 1024;

const KIND_CONTROL: u8 = 0;
const KIND_DATA: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Control(Vec<u8>),
    Data(Vec<u8>),
}

fn write_frame<W: Write>(w: &mut W, kind: u8, parts: &[&[u8]]) -> io::Result<()> {
    let len: usize = 1 + parts.iter().map(|p| p.len()).sum::<usize>();
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("frame of {len} bytes exceeds {MAX_FRAME}"),
        ));
    }
    let mut buf = Vec::with_capacity(4 + len);
    buf.extend_from_slice(&(len as u32).to_be_bytes());
    buf.push(kind);
    for part in parts {
        buf.extend_from_slice(part);
    }
    w.write_all(&buf)?;
    w.flush()
}

/// Serialize `value` as JSON into one control frame.
pub fn write_control<W: Write, T: Serialize>(w: &mut W, value: &T) -> io::Result<()> {
    let json = serde_json::to_vec(value).map_err(io::Error::other)?;
    write_frame(w, KIND_CONTROL, &[&json])
}

/// Client → daemon input bytes.
pub fn write_input<W: Write>(w: &mut W, bytes: &[u8]) -> io::Result<()> {
    write_frame(w, KIND_DATA, &[bytes])
}

/// Daemon → client output bytes starting at `offset`.
pub fn write_output<W: Write>(w: &mut W, offset: u64, bytes: &[u8]) -> io::Result<()> {
    write_frame(w, KIND_DATA, &[&offset.to_be_bytes(), bytes])
}

/// Read one frame. `Ok(None)` on clean EOF at a frame boundary.
pub fn read_frame<R: Read>(r: &mut R) -> io::Result<Option<Frame>> {
    let mut header = [0u8; 4];
    match r.read_exact(&mut header) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(header) as usize;
    if len == 0 || len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid frame length {len}"),
        ));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    let kind = body[0];
    body.remove(0);
    match kind {
        KIND_CONTROL => Ok(Some(Frame::Control(body))),
        KIND_DATA => Ok(Some(Frame::Data(body))),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown frame kind {other}"),
        )),
    }
}

/// Decode a control frame payload.
pub fn parse_control<T: DeserializeOwned>(payload: &[u8]) -> io::Result<T> {
    serde_json::from_slice(payload).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Split a daemon → client output payload into `(offset, bytes)`.
pub fn split_output(payload: &[u8]) -> io::Result<(u64, &[u8])> {
    if payload.len() < 8 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "output frame shorter than its offset",
        ));
    }
    let offset = u64::from_be_bytes(payload[..8].try_into().expect("8 bytes"));
    Ok((offset, &payload[8..]))
}

/// Read the next frame and require it to be a control frame of type `T`.
pub fn read_control<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<T> {
    match read_frame(r)? {
        Some(Frame::Control(payload)) => parse_control(&payload),
        Some(Frame::Data(_)) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected a control frame, got data",
        )),
        None => Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "connection closed",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Request, Response};

    #[test]
    fn control_and_data_frames_round_trip() {
        let mut buf = Vec::new();
        write_control(&mut buf, &Request::Ping).unwrap();
        write_output(&mut buf, 42, b"hello").unwrap();
        write_input(&mut buf, b"\x1b[A").unwrap();

        let mut r = &buf[..];
        let req: Request = read_control(&mut r).unwrap();
        assert_eq!(req, Request::Ping);
        match read_frame(&mut r).unwrap().unwrap() {
            Frame::Data(p) => assert_eq!(split_output(&p).unwrap(), (42, &b"hello"[..])),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            read_frame(&mut r).unwrap(),
            Some(Frame::Data(b"\x1b[A".to_vec()))
        );
        assert_eq!(read_frame(&mut r).unwrap(), None);
    }

    #[test]
    fn rejects_oversized_and_unknown_frames() {
        let mut bad = Vec::new();
        bad.extend_from_slice(&((MAX_FRAME + 1) as u32).to_be_bytes());
        assert!(read_frame(&mut &bad[..]).is_err());

        let unknown = [0, 0, 0, 1, 9u8];
        assert!(read_frame(&mut &unknown[..]).is_err());

        let mut buf = Vec::new();
        write_control(&mut buf, &Response::Pong).unwrap();
        assert!(read_control::<_, Request>(&mut &buf[..]).is_err());
    }
}
