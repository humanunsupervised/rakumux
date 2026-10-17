//! Bounded raw output replay addressed by absolute output offsets.

use std::collections::VecDeque;

/// The most recent `capacity` bytes of a terminal's output. Offsets count
/// every byte the terminal ever produced, so a client that knows its position
/// can tell whether the gap since then is still available.
#[derive(Debug)]
pub(crate) struct ReplayRing {
    buf: VecDeque<u8>,
    capacity: usize,
    /// Offset of `buf[0]`.
    start: u64,
}

impl ReplayRing {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(capacity.min(64 * 1024)),
            capacity,
            start: 0,
        }
    }

    /// Offset just past the newest byte (= total bytes ever pushed).
    pub(crate) fn end(&self) -> u64 {
        self.start + self.buf.len() as u64
    }

    pub(crate) fn push(&mut self, bytes: &[u8]) {
        let end = self.end() + bytes.len() as u64;
        if bytes.len() >= self.capacity {
            self.buf.clear();
            self.buf
                .extend(&bytes[bytes.len() - self.capacity..bytes.len()]);
        } else {
            let overflow = (self.buf.len() + bytes.len()).saturating_sub(self.capacity);
            self.buf.drain(..overflow);
            self.buf.extend(bytes);
        }
        self.start = end - self.buf.len() as u64;
    }

    /// Bytes from `offset` to the end. `None` when `offset` is older than
    /// the retained window (the gap was truncated) or beyond the end.
    pub(crate) fn since(&self, offset: u64) -> Option<Vec<u8>> {
        if offset < self.start || offset > self.end() {
            return None;
        }
        let skip = (offset - self.start) as usize;
        Some(self.buf.iter().skip(skip).copied().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_newest_bytes_with_absolute_offsets() {
        let mut ring = ReplayRing::new(8);
        ring.push(b"hello");
        assert_eq!(ring.end(), 5);
        assert_eq!(ring.since(0).unwrap(), b"hello");
        assert_eq!(ring.since(3).unwrap(), b"lo");
        assert_eq!(ring.since(5).unwrap(), b"");
        assert_eq!(ring.since(6), None);

        ring.push(b" world");
        assert_eq!(ring.end(), 11);
        // Only the last 8 bytes remain: offsets 3..11.
        assert_eq!(ring.since(2), None);
        assert_eq!(ring.since(3).unwrap(), b"lo world");
    }

    #[test]
    fn oversized_pushes_keep_their_tail() {
        let mut ring = ReplayRing::new(4);
        ring.push(b"ab");
        ring.push(b"0123456789");
        assert_eq!(ring.end(), 12);
        assert_eq!(ring.since(8).unwrap(), b"6789");
        assert_eq!(ring.since(7), None);

        let mut empty = ReplayRing::new(0);
        empty.push(b"xyz");
        assert_eq!(empty.end(), 3);
        assert_eq!(empty.since(3).unwrap(), b"");
        assert_eq!(empty.since(2), None);
    }
}
