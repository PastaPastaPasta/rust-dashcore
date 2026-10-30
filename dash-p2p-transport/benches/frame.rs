//! Frame encode and decode through the transports, per message size, and the V2 handshake.
//!
//! - `frame/v1_encode/<n>`, `frame/v2_encode/<n>`: `set_message_to_send` of an `n`-byte payload
//!   and draining its bytes (V1: header and checksum; V2: one encrypted packet, short ID);
//! - `frame/v1_decode/<n>`, `frame/v2_decode/<n>`: feeding those bytes to a receiver and taking
//!   the message (V2: length decryption, authentication, decryption);
//! - `frame/v2_handshake`: both sides from fresh keys to established V2 (ElligatorSwift ECDH,
//!   key derivation, empty garbage, version packets); Core's `BIP324_ECDH` covers the ECDH alone.
//!
//! The payloads are built outside the measurement, as Core's callers build `CSerializedNetMsg`.

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use dash_p2p_transport::cipher::SecretKey;
use dash_p2p_transport::{
    CommandString, Magic, OutMsg, ProtocolVersion, TransportKind, V1Transport, V2Transport,
};

const MAGIC: Magic = Magic::from_bytes([0xfc, 0xc1, 0xb7, 0xdc]);
const SIZES: &[usize] = &[32, 1024, 65536];
const COMMAND: CommandString = CommandString::from_static("block");

fn key(n: u8) -> SecretKey {
    SecretKey::from_secret_bytes([n; 32]).unwrap()
}

/// Hands `msg` to `t` and drains its bytes: the encoding of one message.
macro_rules! encode {
    ($t:expr, $msg:expr, $out:expr) => {{
        $t.set_message_to_send($msg).unwrap();
        loop {
            let data = $t.bytes_to_send(false).data;
            if data.is_empty() {
                break;
            }
            let n = data.len();
            $out.extend_from_slice(data);
            $t.mark_bytes_sent(n);
        }
    }};
}

/// Feeds `bytes` to `t` and takes the message they complete.
macro_rules! decode {
    ($t:expr, $bytes:expr) => {{
        let mut rest: &[u8] = $bytes;
        while !rest.is_empty() {
            $t.received_bytes(&mut rest).unwrap();
        }
        assert!($t.received_message_complete());
        $t.received_message(0).unwrap().payload.len()
    }};
}

/// Moves `from`'s pending bytes to `to`; whether there were any.
fn transfer(from: &mut V2Transport, to: &mut V2Transport) -> bool {
    let data = from.bytes_to_send(false).data.to_vec();
    from.mark_bytes_sent(data.len());
    let mut rest = &data[..];
    while !rest.is_empty() {
        to.received_bytes(&mut rest).unwrap();
    }
    !data.is_empty()
}

/// Moves bytes both ways until neither side has any.
fn pump(a: &mut V2Transport, b: &mut V2Transport) {
    while transfer(a, b) | transfer(b, a) {}
}

fn v2_pair() -> (V2Transport, V2Transport) {
    let mut a = V2Transport::with_keys(MAGIC, true, key(1), [2; 32], Vec::new());
    let mut b = V2Transport::with_keys(MAGIC, false, key(3), [4; 32], Vec::new());
    pump(&mut a, &mut b);
    a.set_peer_version(ProtocolVersion::V24_0);
    b.set_peer_version(ProtocolVersion::V24_0);
    (a, b)
}

fn frame(c: &mut Criterion) {
    let mut group = c.benchmark_group("frame");
    for &size in SIZES {
        let payload = vec![0x5a; size];
        let msg = || OutMsg::new(COMMAND, payload.clone());
        group.throughput(Throughput::Bytes(size as u64));

        let mut tx = V1Transport::new(MAGIC);
        let mut out = Vec::with_capacity(size + 64);
        group.bench_function(format!("v1_encode/{size}"), |b| {
            b.iter_batched(
                msg,
                |m| {
                    out.clear();
                    encode!(tx, m, out);
                    out.len()
                },
                BatchSize::SmallInput,
            )
        });
        let mut wire = Vec::new();
        encode!(tx, msg(), wire);
        let mut rx = V1Transport::new(MAGIC);
        group.bench_function(format!("v1_decode/{size}"), |b| b.iter(|| decode!(rx, &wire)));

        let (mut tx, _) = v2_pair();
        group.bench_function(format!("v2_encode/{size}"), |b| {
            b.iter_batched(
                msg,
                |m| {
                    out.clear();
                    encode!(tx, m, out);
                    out.len()
                },
                BatchSize::SmallInput,
            )
        });
        // A fresh pair, so that the receiver's packet counter follows the sender's: every
        // setup encrypts the next packet, and the routines decrypt them in order.
        let (mut tx, mut rx) = v2_pair();
        group.bench_function(format!("v2_decode/{size}"), |b| {
            b.iter_batched(
                || {
                    let mut wire = Vec::with_capacity(size + 64);
                    encode!(tx, msg(), wire);
                    wire
                },
                |wire| decode!(rx, &wire),
                BatchSize::SmallInput,
            )
        });
    }
    group.throughput(Throughput::Elements(1));
    group.bench_function("v2_handshake", |b| {
        b.iter_batched(
            || {
                (
                    V2Transport::with_keys(MAGIC, true, key(5), [6; 32], Vec::new()),
                    V2Transport::with_keys(MAGIC, false, key(7), [8; 32], Vec::new()),
                )
            },
            |(mut a, mut b)| {
                pump(&mut a, &mut b);
                assert_eq!(a.info().kind, TransportKind::V2);
                (a, b)
            },
            BatchSize::SmallInput,
        )
    });
    group.finish();
}

criterion_group!(benches, frame);
criterion_main!(benches);
