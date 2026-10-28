//! TCP traffic per socket on Linux, from the kernel's socket diagnostics
//! (`NETLINK_SOCK_DIAG`, what `ss -ti` reads): each TCP socket's inode with
//! the bytes it received and the bytes the other end acknowledged
//! (`tcp_info`'s `tcpi_bytes_received` and `tcpi_bytes_acked`, Linux 4.2).
//! Any user may ask; the kernel answers for every socket and this keeps the
//! caller's own. Connections to loopback are left out, as the machine's
//! counters leave out the loopback interface.

use std::collections::HashMap;
use std::mem::size_of;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

/// What one socket carried so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Traffic {
    pub received: u64,
    pub sent: u64,
}

const SOCK_DIAG_BY_FAMILY: u16 = 20;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const NLM_F_REQUEST: u16 = 0x1;
const NLM_F_DUMP: u16 = 0x300;
/// `struct nlmsghdr`.
const HEADER_LEN: usize = 16;
/// `struct inet_diag_req_v2`: family, protocol, extensions, padding,
/// states, then a zeroed `inet_diag_sockid` (48 bytes).
const REQUEST_LEN: usize = 56;
/// `struct inet_diag_msg`, before its attributes.
const DIAG_MSG_LEN: usize = 72;
/// Offsets in `inet_diag_msg`: the remote address (in `id`), the owner's
/// uid and the inode.
const DST_AT: usize = 24;
const UID_AT: usize = 64;
const INODE_AT: usize = 68;
/// The attribute holding `struct tcp_info`.
const INET_DIAG_INFO: u16 = 2;
/// Offsets in `tcp_info`.
const BYTES_ACKED_AT: usize = 120;
const BYTES_RECEIVED_AT: usize = 128;
/// How long to wait for the kernel's answer.
const TIMEOUT_US: libc::suseconds_t = 500_000;

/// `uid`'s TCP sockets by inode. None when socket diagnostics can't be
/// opened at all; a family the kernel refuses (no IPv6) is left out.
pub(super) fn sockets(uid: u32) -> Option<HashMap<u64, Traffic>> {
    let socket = open()?;
    let mut found = HashMap::new();
    for (seq, family) in [(1, libc::AF_INET), (2, libc::AF_INET6)] {
        if send_request(&socket, seq, family).is_some() {
            read_dump(&socket, seq, uid, &mut found);
        }
    }
    Some(found)
}

fn open() -> Option<OwnedFd> {
    // SAFETY: plain socket call; the descriptor is owned below.
    let fd = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
            libc::NETLINK_SOCK_DIAG,
        )
    };
    if fd < 0 {
        return None;
    }
    // SAFETY: `fd` is a fresh descriptor nothing else owns.
    let socket = unsafe { OwnedFd::from_raw_fd(fd) };
    let timeout = libc::timeval {
        tv_sec: 0,
        tv_usec: TIMEOUT_US,
    };
    // SAFETY: `timeout` is valid for the length given.
    let rc = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            std::ptr::addr_of!(timeout).cast(),
            size_of::<libc::timeval>() as libc::socklen_t,
        )
    };
    (rc == 0).then_some(socket)
}

/// Ask for every TCP socket of `family` with its `tcp_info`.
fn send_request(socket: &OwnedFd, seq: u32, family: i32) -> Option<()> {
    let message = request(seq, u8::try_from(family).ok()?);
    // SAFETY: an all-zero sockaddr_nl is valid; the family is set below.
    let mut kernel: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    kernel.nl_family = libc::AF_NETLINK as libc::sa_family_t;
    // SAFETY: both buffers are valid for the lengths given.
    let sent = unsafe {
        libc::sendto(
            socket.as_raw_fd(),
            message.as_ptr().cast(),
            message.len(),
            0,
            std::ptr::addr_of!(kernel).cast(),
            size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        )
    };
    (usize::try_from(sent).ok() == Some(message.len())).then_some(())
}

fn request(seq: u32, family: u8) -> Vec<u8> {
    let mut message = Vec::with_capacity(HEADER_LEN + REQUEST_LEN);
    message.extend_from_slice(&((HEADER_LEN + REQUEST_LEN) as u32).to_ne_bytes());
    message.extend_from_slice(&SOCK_DIAG_BY_FAMILY.to_ne_bytes());
    message.extend_from_slice(&(NLM_F_REQUEST | NLM_F_DUMP).to_ne_bytes());
    message.extend_from_slice(&seq.to_ne_bytes());
    message.extend_from_slice(&0u32.to_ne_bytes());
    message.push(family);
    message.push(libc::IPPROTO_TCP as u8);
    message.push(1 << (INET_DIAG_INFO - 1));
    message.push(0);
    message.extend_from_slice(&u32::MAX.to_ne_bytes());
    message.resize(HEADER_LEN + REQUEST_LEN, 0);
    message
}

/// Read the answer to request `seq` until the kernel says it's done, or
/// fails, or stops answering.
fn read_dump(socket: &OwnedFd, seq: u32, uid: u32, found: &mut HashMap<u64, Traffic>) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        // SAFETY: `buf` is valid for its length.
        let read = unsafe { libc::recv(socket.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
        let Ok(read) = usize::try_from(read) else {
            return;
        };
        if read == 0 || parse_messages(&buf[..read], seq, uid, found) {
            return;
        }
    }
}

/// Take in one datagram of netlink messages; true once the dump is over.
fn parse_messages(mut bytes: &[u8], seq: u32, uid: u32, found: &mut HashMap<u64, Traffic>) -> bool {
    while bytes.len() >= HEADER_LEN {
        let len = read_u32(bytes, 0).unwrap_or(0) as usize;
        if len < HEADER_LEN || len > bytes.len() {
            return true;
        }
        let kind = read_u16(bytes, 4).unwrap_or(0);
        let message_seq = read_u32(bytes, 8).unwrap_or(0);
        match kind {
            NLMSG_DONE | NLMSG_ERROR if message_seq == seq => return true,
            SOCK_DIAG_BY_FAMILY if message_seq == seq => {
                if let Some((inode, traffic)) = socket_of(&bytes[HEADER_LEN..len], uid) {
                    found.insert(inode, traffic);
                }
            }
            _ => {}
        }
        bytes = bytes.get(align(len)..).unwrap_or(&[]);
    }
    false
}

/// An `inet_diag_msg`'s inode and traffic, when the socket is `uid`'s, has
/// an inode and a `tcp_info`, and isn't connected to loopback.
fn socket_of(message: &[u8], uid: u32) -> Option<(u64, Traffic)> {
    if message.len() < DIAG_MSG_LEN || read_u32(message, UID_AT)? != uid {
        return None;
    }
    let inode = u64::from(read_u32(message, INODE_AT)?);
    let remote = message.get(DST_AT..DST_AT + 16)?;
    if inode == 0 || is_loopback(i32::from(message[0]), remote) {
        return None;
    }
    let mut attributes = &message[DIAG_MSG_LEN..];
    while attributes.len() >= 4 {
        let len = usize::from(read_u16(attributes, 0)?);
        if len < 4 || len > attributes.len() {
            return None;
        }
        if read_u16(attributes, 2)? == INET_DIAG_INFO {
            let info = &attributes[4..len];
            return Some((
                inode,
                Traffic {
                    received: read_u64(info, BYTES_RECEIVED_AT)?,
                    sent: read_u64(info, BYTES_ACKED_AT)?,
                },
            ));
        }
        attributes = attributes.get(align(len)..).unwrap_or(&[]);
    }
    None
}

/// `remote` is the first 16 bytes of `inet_diag_sockid::idiag_dst`
/// (network order): 127.0.0.0/8, `::1` or `::ffff:127.x.x.x`.
fn is_loopback(family: i32, remote: &[u8]) -> bool {
    match family {
        libc::AF_INET => remote[0] == 127,
        libc::AF_INET6 => {
            let v4_mapped = remote[..10].iter().all(|b| *b == 0) && remote[10..12] == [0xff, 0xff];
            (remote[..15].iter().all(|b| *b == 0) && remote[15] == 1)
                || (v4_mapped && remote[12] == 127)
        }
        _ => false,
    }
}

/// Netlink messages and attributes start on 4-byte boundaries.
fn align(len: usize) -> usize {
    (len + 3) & !3
}

fn read_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_ne_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn read_u64(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_ne_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// A `SOCK_DIAG_BY_FAMILY` message for one socket with a `tcp_info`.
    fn diag_message(seq: u32, family: u8, remote: [u8; 16], uid: u32, inode: u32) -> Vec<u8> {
        let mut body = vec![0u8; DIAG_MSG_LEN];
        body[0] = family;
        body[DST_AT..DST_AT + 16].copy_from_slice(&remote);
        body[UID_AT..UID_AT + 4].copy_from_slice(&uid.to_ne_bytes());
        body[INODE_AT..INODE_AT + 4].copy_from_slice(&inode.to_ne_bytes());
        let mut info = vec![0u8; 232];
        info[BYTES_ACKED_AT..BYTES_ACKED_AT + 8].copy_from_slice(&700u64.to_ne_bytes());
        info[BYTES_RECEIVED_AT..BYTES_RECEIVED_AT + 8].copy_from_slice(&9000u64.to_ne_bytes());
        // An attribute before it that isn't the one wanted.
        body.extend_from_slice(&5u16.to_ne_bytes());
        body.extend_from_slice(&1u16.to_ne_bytes());
        body.extend_from_slice(&[0, 0, 0, 0]);
        body.extend_from_slice(&((4 + info.len()) as u16).to_ne_bytes());
        body.extend_from_slice(&INET_DIAG_INFO.to_ne_bytes());
        body.extend_from_slice(&info);
        let mut message = Vec::new();
        message.extend_from_slice(&((HEADER_LEN + body.len()) as u32).to_ne_bytes());
        message.extend_from_slice(&SOCK_DIAG_BY_FAMILY.to_ne_bytes());
        message.extend_from_slice(&0u16.to_ne_bytes());
        message.extend_from_slice(&seq.to_ne_bytes());
        message.extend_from_slice(&0u32.to_ne_bytes());
        message.extend_from_slice(&body);
        message
    }

    fn done(seq: u32) -> Vec<u8> {
        let mut message = Vec::new();
        message.extend_from_slice(&20u32.to_ne_bytes());
        message.extend_from_slice(&NLMSG_DONE.to_ne_bytes());
        message.extend_from_slice(&0u16.to_ne_bytes());
        message.extend_from_slice(&seq.to_ne_bytes());
        message.extend_from_slice(&[0; 8]);
        message
    }

    fn v4(a: [u8; 4]) -> [u8; 16] {
        let mut remote = [0u8; 16];
        remote[..4].copy_from_slice(&a);
        remote
    }

    #[test]
    fn dumps_keep_the_users_remote_sockets() {
        let mut datagram = diag_message(1, libc::AF_INET as u8, v4([93, 184, 216, 34]), 1000, 77);
        // Someone else's, one on loopback and one without an inode.
        datagram.extend(diag_message(
            1,
            libc::AF_INET as u8,
            v4([1, 1, 1, 1]),
            0,
            78,
        ));
        datagram.extend(diag_message(
            1,
            libc::AF_INET as u8,
            v4([127, 0, 0, 1]),
            1000,
            79,
        ));
        datagram.extend(diag_message(
            1,
            libc::AF_INET as u8,
            v4([1, 1, 1, 1]),
            1000,
            0,
        ));
        let mut found = HashMap::new();
        assert!(!parse_messages(&datagram, 1, 1000, &mut found));
        assert_eq!(
            found,
            HashMap::from([(
                77,
                Traffic {
                    received: 9000,
                    sent: 700
                }
            )])
        );
        assert!(parse_messages(&done(1), 1, 1000, &mut found));
        // Another request's messages are ignored.
        assert!(!parse_messages(&done(2), 1, 1000, &mut found));
    }

    #[test]
    fn loopback_is_recognised_in_both_families() {
        let v6 = |last: u8| {
            let mut remote = [0u8; 16];
            remote[15] = last;
            remote
        };
        let mut mapped = [0u8; 16];
        mapped[10..12].copy_from_slice(&[0xff, 0xff]);
        mapped[12..].copy_from_slice(&[127, 0, 0, 1]);
        assert!(is_loopback(libc::AF_INET, &v4([127, 1, 2, 3])));
        assert!(!is_loopback(libc::AF_INET, &v4([10, 0, 0, 1])));
        assert!(is_loopback(libc::AF_INET6, &v6(1)));
        assert!(!is_loopback(libc::AF_INET6, &v6(2)));
        assert!(is_loopback(libc::AF_INET6, &mapped));
        mapped[12] = 8;
        assert!(!is_loopback(libc::AF_INET6, &mapped));
    }

    #[test]
    fn requests_ask_for_tcp_info_of_every_state() {
        let message = request(7, libc::AF_INET6 as u8);
        assert_eq!(message.len(), 72);
        assert_eq!(read_u32(&message, 0), Some(72));
        assert_eq!(read_u16(&message, 4), Some(SOCK_DIAG_BY_FAMILY));
        assert_eq!(read_u32(&message, 8), Some(7));
        assert_eq!(&message[16..20], &[libc::AF_INET6 as u8, 6, 2, 0]);
        assert_eq!(read_u32(&message, 20), Some(u32::MAX));
    }

    /// The kernel counts what a real connection carries. Loopback is left
    /// out of the answer, so the sockets are found by inode without that
    /// rule: the counters are what's checked here.
    #[test]
    fn a_live_connection_is_counted() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        client.write_all(&[7u8; 5000]).unwrap();
        let mut got = vec![0u8; 5000];
        server.read_exact(&mut got).unwrap();
        let inode = {
            use std::os::unix::fs::MetadataExt;
            let fd = client.as_raw_fd();
            std::fs::metadata(format!("/proc/self/fd/{fd}"))
                .unwrap()
                .ino()
        };
        let socket = open().expect("socket diagnostics");
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        let mut buf = vec![0u8; 64 * 1024];
        let mut traffic = None;
        send_request(&socket, 1, libc::AF_INET).unwrap();
        'dump: loop {
            // SAFETY: `buf` is valid for its length.
            let read =
                unsafe { libc::recv(socket.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
            let mut bytes = &buf[..usize::try_from(read).unwrap()];
            while bytes.len() >= HEADER_LEN {
                let len = read_u32(bytes, 0).unwrap() as usize;
                if read_u16(bytes, 4) == Some(NLMSG_DONE) {
                    break 'dump;
                }
                let mut body = bytes[HEADER_LEN..len].to_vec();
                // Pretend it's remote so `socket_of` keeps it.
                body[DST_AT] = 10;
                if let Some((found, t)) = socket_of(&body, uid) {
                    if found == inode {
                        traffic = Some(t);
                    }
                }
                bytes = &bytes[align(len).min(bytes.len())..];
            }
        }
        let traffic = traffic.expect("the client socket is listed");
        assert!(traffic.sent >= 5000, "{traffic:?}");
        // And the real reader leaves loopback out.
        assert!(!sockets(uid).unwrap().contains_key(&inode));
    }
}
