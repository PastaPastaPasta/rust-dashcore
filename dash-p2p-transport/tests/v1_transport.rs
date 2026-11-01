//! V1 framing errors (P2a behaviour 10), as Dash Core v24.0.0-rc.3's `V1Transport` handles them:
//! a bad checksum or invalid command drops the message and keeps the peer; a wrong message start
//! or a payload above 3 MiB closes the connection.

mod common;

use common::{mainnet, regtest};
use dash_p2p_transport::{
    CommandString, NetMessage, OutMsg, Reject, RejectedMessage, Transport, TransportError,
    TransportKind, V1Transport,
};

const MAX_PROTOCOL_MESSAGE_LENGTH: u32 = 3 * 1024 * 1024;

fn cmd(s: &[u8]) -> CommandString {
    let mut b = [0u8; 12];
    b[..s.len()].copy_from_slice(s);
    CommandString::from_bytes(b)
}

/// The wire bytes our V1 sender produces for a message.
fn frame(command: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut t = V1Transport::new(regtest());
    t.set_message_to_send(OutMsg::new(cmd(command), payload.to_vec())).unwrap();
    let mut out = Vec::new();
    loop {
        let data = t.bytes_to_send(false).data;
        if data.is_empty() {
            return out;
        }
        let n = data.len();
        out.extend_from_slice(data);
        t.mark_bytes_sent(n);
    }
}

fn receive(
    t: &mut Transport,
    bytes: &[u8],
) -> Result<Vec<Result<NetMessage, RejectedMessage>>, TransportError> {
    let mut got = Vec::new();
    t.receive(bytes, 42, |m| got.push(m))?;
    Ok(got)
}

#[test]
fn header_layout_is_core_s() {
    // magic, command, LE32 length, first four bytes of SHA256d(payload).
    let f = frame(b"verack", &[]);
    assert_eq!(&f[..4], regtest().as_bytes());
    assert_eq!(&f[4..16], b"verack\0\0\0\0\0\0");
    assert_eq!(&f[16..20], &[0, 0, 0, 0]);
    // SHA256d("") starts 5df6e0e2 (Core's empty-payload checksum).
    assert_eq!(&f[20..24], &[0x5d, 0xf6, 0xe0, 0xe2]);
}

#[test]
fn bad_checksum_is_dropped_and_the_peer_kept() {
    let mut t = Transport::v1(regtest());
    let mut bad = frame(b"ping", &[1; 8]);
    bad[20] ^= 1;
    let good = frame(b"pong", &[2; 8]);
    let got = receive(&mut t, &[bad, good].concat()).unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(
        got[0],
        Err(RejectedMessage {
            raw_size: 32,
            reason: Reject::BadChecksum
        })
    );
    let m = got[1].as_ref().unwrap();
    assert_eq!(
        (m.command, &m.payload[..], m.time_us, m.raw_size),
        (cmd(b"pong"), &[2u8; 8][..], 42, 32)
    );
}

#[test]
fn invalid_command_is_dropped_and_the_peer_kept() {
    for command in [&b"pi\x01g"[..], b"pi\x7fg", b"pi\0g", b"pi\x80g"] {
        let mut t = Transport::v1(regtest());
        let got = receive(&mut t, &[frame(command, &[]), frame(b"verack", &[])].concat()).unwrap();
        assert_eq!(
            got[0],
            Err(RejectedMessage {
                raw_size: 24,
                reason: Reject::InvalidCommand
            })
        );
        assert_eq!(got[1].as_ref().unwrap().command, cmd(b"verack"));
    }
    // Every printable byte, and an all-zero (empty) command, are valid.
    let mut t = Transport::v1(regtest());
    let got = receive(&mut t, &[frame(b" ~", &[]), frame(b"", &[])].concat()).unwrap();
    assert!(got.iter().all(Result::is_ok));
}

#[test]
fn wrong_message_start_disconnects() {
    let mut t = Transport::v1(regtest());
    let mut f = frame(b"ping", &[0; 8]);
    f[..4].copy_from_slice(mainnet().as_bytes());
    assert_eq!(receive(&mut t, &f).unwrap_err(), TransportError::WrongMessageStart(mainnet()));
}

#[test]
fn oversize_disconnects_at_the_header() {
    let mut t = Transport::v1(regtest());
    let mut f = frame(b"block", &[]);
    f[16..20].copy_from_slice(&(MAX_PROTOCOL_MESSAGE_LENGTH + 1).to_le_bytes());
    // Only the header is needed to refuse it.
    assert_eq!(
        receive(&mut t, &f[..24]).unwrap_err(),
        TransportError::MessageTooLarge {
            command: cmd(b"block"),
            size: MAX_PROTOCOL_MESSAGE_LENGTH + 1
        }
    );
}

/// The oversize error's text sanitizes the unchecked command as Core's `readHeader` does
/// (`SanitizeString(hdr.GetCommand())`): up to the first NUL, only letters, digits and
/// ` .,;-_/:?@()`. The error keeps the raw bytes.
#[test]
fn oversize_error_sanitizes_the_command() {
    let cases: &[(&[u8; 12], &str)] = &[
        (b"ping\nFORGED!", "pingFORGED"),
        (b"a\x1b[31mb\x1b[0m\0", "a31mb0m"),
        (b"\xffping\xc3\xa9 (x)!", "ping (x)"),
        (b"ver\0sion\nxyz", "ver"),
    ];
    for &(raw, shown) in cases {
        let mut f = frame(b"block", &[]);
        f[4..16].copy_from_slice(raw);
        f[16..20].copy_from_slice(&(MAX_PROTOCOL_MESSAGE_LENGTH + 1).to_le_bytes());
        let err = receive(&mut Transport::v1(regtest()), &f[..24]).unwrap_err();
        assert_eq!(
            err,
            TransportError::MessageTooLarge {
                command: CommandString::from_bytes(*raw),
                size: MAX_PROTOCOL_MESSAGE_LENGTH + 1
            }
        );
        assert_eq!(
            err.to_string(),
            format!("Header error: Size too large ({shown}, 3145729 bytes)")
        );
        let debug = format!("{err:?}");
        assert!(!debug.bytes().any(|b| b.is_ascii_control() || !b.is_ascii()), "{debug}");
    }
}

#[test]
fn three_mib_payload_is_accepted() {
    let payload: Vec<u8> = (0..MAX_PROTOCOL_MESSAGE_LENGTH).map(|i| i as u8).collect();
    let f = frame(b"block", &payload);
    let mut t = Transport::v1(regtest());
    // In odd-sized chunks, so the 256 KiB reserve-ahead grows the buffer several times.
    let mut got = Vec::new();
    for chunk in f.chunks(100_003) {
        t.receive(chunk, 0, |m| got.push(m)).unwrap();
    }
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].as_ref().unwrap().payload, payload);
}

#[test]
fn transport_reports_v1() {
    let t = Transport::v1(regtest());
    assert_eq!(t.info().kind, TransportKind::V1);
    assert_eq!(t.info().session_id, None);
    assert!(!t.should_reconnect_v1());
}
