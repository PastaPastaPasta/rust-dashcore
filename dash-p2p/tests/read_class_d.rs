//! Ported class-D handler boundary tests for the dormant node reader.

use dash_p2p::read::{self, HandlerRead, PeerCtx, ReadFail};
use dash_p2p_core::{
    BatchedSigSharesMsg, CommandString, Headers, Headers2, QuorumDataRequest, SigSesAnnMsg,
    SigShareMsg, SigSharesInvMsg,
};
use dash_pkc::bls::BlsCtx;
use dash_types::CompactSize;
use dash_types::codec::BaseCodec;

fn ctx(version: i32) -> PeerCtx {
    PeerCtx::new(dash_p2p::ProtocolVersion::new(version), BlsCtx::BASIC)
}
fn count(n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    CompactSize::from(n).encode(&mut out);
    out
}

fn payload(prefix: &[u8], count: usize, entry: &[u8], suffix: &[u8]) -> Vec<u8> {
    let mut encoded_count = Vec::new();
    CompactSize::from(count).encode(&mut encoded_count);
    [prefix, &encoded_count, &entry.repeat(count), suffix].concat()
}

fn command(name: &str) -> CommandString {
    let mut bytes = [0; 12];
    bytes[..name.len()].copy_from_slice(name.as_bytes());
    CommandString::from_bytes(bytes)
}

const INVENTORY: [u8; 36] = {
    let mut entry = [0; 36];
    entry[0] = 1;
    entry
};
const ADDR: [u8; 30] = [0; 30];
const ADDRV2: [u8; 13] = [0, 0, 0, 0, 0, 1, 4, 1, 2, 3, 4, 0x27, 0x0f];
const HASH: [u8; 32] = [0; 32];
const VERSION: [u8; 4] = 70_240i32.to_le_bytes();

#[test]
fn count_limits_refuse_before_entries() {
    let over = count(2_001);
    assert!(matches!(
        Headers::read(&mut over.as_slice(), &ctx(70242)),
        Err(ReadFail::TooManyEntries { .. })
    ));
    let over = count(8_001);
    assert!(matches!(
        Headers2::read(&mut over.as_slice(), &ctx(70242)),
        Err(ReadFail::TooManyEntries { .. })
    ));
    let over = count(101);
    assert!(matches!(
        SigSesAnnMsg::read(&mut over.as_slice(), &ctx(70242)),
        Err(ReadFail::TooManyEntries { .. })
    ));
    let over = count(201);
    assert!(matches!(
        SigSharesInvMsg::read(&mut over.as_slice(), &ctx(70242)),
        Err(ReadFail::TooManyEntries { .. })
    ));
    let over = count(401);
    assert!(matches!(
        BatchedSigSharesMsg::read(&mut over.as_slice(), &ctx(70242)),
        Err(ReadFail::TooManyEntries { .. })
    ));
    let over = count(33);
    assert!(matches!(
        SigShareMsg::read(&mut over.as_slice(), &ctx(70242)),
        Err(ReadFail::TooManyEntries { .. })
    ));
}

#[test]
fn qgetdata_and_dkg_size_limits_are_exact() {
    for size in [67usize, 68] {
        let data = vec![0; size];
        let result = QuorumDataRequest::read(&mut data.as_slice(), &ctx(70242));
        assert_eq!(result.is_err(), size == 68);
    }
}

#[test]
fn list_handler_reads_past_base_codec_limits() {
    let cases = [
        ("inv", &[][..], &INVENTORY[..], &[][..], 50_001usize),
        ("getdata", &[][..], &INVENTORY[..], &[][..], 50_001),
        ("notfound", &[][..], &INVENTORY[..], &[][..], 50_001),
        ("addr", &[][..], &ADDR[..], &[][..], 1_001),
        ("addrv2", &[][..], &ADDRV2[..], &[][..], 1_001),
        ("getheaders", &VERSION[..], &HASH[..], &HASH[..], 102),
        ("getheaders2", &VERSION[..], &HASH[..], &HASH[..], 102),
        ("getblocks", &VERSION[..], &HASH[..], &HASH[..], 102),
    ];
    for (name, prefix, entry, suffix, count) in cases {
        let bytes = payload(prefix, count, entry, suffix);
        let decoded = read::decode(&command(name), &bytes, &ctx(70242))
            .unwrap_or_else(|error| panic!("{name} count {count}: {error}"));
        assert_eq!(decoded.consumed, bytes.len(), "{name}");
    }
}

#[test]
fn unknown_and_empty_commands_keep_their_payload_policy() {
    let unknown = command("classdtest");
    let decoded = read::decode(&unknown, &[0, 1, 2], &ctx(70242)).unwrap();
    assert_eq!(decoded.consumed, 3);
    assert!(
        matches!(decoded.msg, read::Msg::Unknown { command, payload } if command == unknown && payload == [0, 1, 2])
    );

    let empty = read::decode(&CommandString::VERACK, &[0, 1], &ctx(70242)).unwrap();
    assert_eq!(empty.consumed, 0);
    assert!(matches!(empty.msg, read::Msg::Known(dash_p2p_core::P2pMsg::Verack)));
}
