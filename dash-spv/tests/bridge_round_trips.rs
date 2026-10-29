//! `dash-spv::bridge` over Core's own messages (SB12a): every capture of every kind crosses to
//! base-sdk (or `dash-consensus`) and back, and both sides write exactly the captured bytes.
//!
//! The captures are mainnet and testnet messages from the reference oracles
//! (`tests/data/bridge/`, `contrib/node/sb12a-captures.py`, `manifest.json`), plus the existing
//! `mnlistdiff` and `qrinfo` wire captures under `dash/tests/data/test_DML_diffs/` and
//! `dash/artifacts/` (their `PROVENANCE.md`).

use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};

use dash_spv::bridge::headers::{
    block_from_consensus, block_to_consensus, hashed_block_to_consensus,
    hashed_header_to_consensus, header_from_consensus, headers_to_consensus,
};
use dash_spv::bridge::messages::*;
use dash_spv::types::{HashedBlock, HashedBlockHeader};
use dash_types::codec::BaseCodec;
use dashcore::consensus::{deserialize, serialize};
use dashcore::hashes::{sha256, sha256d, Hash};
use dashcore::network::message::{NetworkMessage, RawNetworkMessage};
use dashcore::network::message_headers2::CompressionState;
use dashcore::Block;

/// dashcore's decode of a payload, through its message framing as a peer's message would go.
fn decode_message(command: &str, payload: &[u8]) -> NetworkMessage {
    let mut raw = vec![0; 4];
    let mut name = [0u8; 12];
    name[..command.len()].copy_from_slice(command.as_bytes());
    raw.extend_from_slice(&name);
    raw.extend_from_slice(&u32::try_from(payload.len()).expect("small").to_le_bytes());
    raw.extend_from_slice(&sha256d::Hash::hash(payload)[..4]);
    raw.extend_from_slice(payload);
    let message: RawNetworkMessage = deserialize(&raw).expect("dashcore decodes the capture");
    assert_eq!(message.cmd(), command);
    message.payload
}

/// The payload dashcore writes for a message.
fn payload(message: NetworkMessage) -> Vec<u8> {
    serialize(&RawNetworkMessage {
        magic: 0,
        payload: message,
    })[24..]
        .to_vec()
}

fn base_bytes(value: &impl BaseCodec) -> Vec<u8> {
    let mut bytes = Vec::new();
    value.encode(&mut bytes);
    bytes
}

fn base_decode<B: BaseCodec>(bytes: &[u8]) -> B {
    let mut cursor = bytes;
    let value = B::decode(&mut cursor).expect("base-sdk decodes the capture");
    assert!(cursor.is_empty(), "base-sdk leaves {} bytes", cursor.len());
    value
}

/// A value that crosses as Core's serialization: base-sdk's value is the one base-sdk decodes
/// from the capture, and each side writes the capture's bytes.
fn round_trip<C: PartialEq + Debug, B: BaseCodec + PartialEq + Debug, E: Debug>(
    bytes: &[u8],
    core: C,
    to_base: impl Fn(&C) -> Result<B, E>,
    from_base: impl Fn(&B) -> Result<C, E>,
    wrap: impl Fn(C) -> NetworkMessage,
) {
    let base = to_base(&core).expect("crosses to base-sdk");
    assert_eq!(base_bytes(&base), bytes, "base-sdk writes other bytes");
    assert_eq!(base, base_decode::<B>(bytes), "not the value base-sdk decodes");
    let back = from_base(&base).expect("crosses back");
    assert_eq!(back, core);
    assert_eq!(payload(wrap(back)), bytes, "dashcore writes other bytes");
}

fn check_headers(bytes: &[u8]) {
    let NetworkMessage::Headers(headers) = decode_message("headers", bytes) else {
        unreachable!()
    };
    let base = headers_to_base(&headers);
    assert_eq!(base_bytes(&base), bytes);
    assert_eq!(base, base_decode(bytes));
    let back = headers_from_base(&base);
    assert_eq!(back, headers);
    assert_eq!(payload(NetworkMessage::Headers(back)), bytes);

    // Into dash-consensus and back: each header comes back with dashcore's hash for it, and
    // dash-spv's own pair crosses to the same header.
    let hashed: Vec<HashedBlockHeader> = headers.iter().map(HashedBlockHeader::from).collect();
    let consensus = headers_to_consensus(&headers);
    // A full reply's count (2,000) is a 3-byte compact size; each entry is then the 80-byte
    // header and an empty transaction count.
    assert_eq!(bytes[0], 0xfd, "a full reply");
    for ((header, hashed), at) in consensus.iter().zip(&hashed).zip(bytes[3..].chunks(81)) {
        assert_eq!(dash_consensus::core_serialize(header), at[..80]);
        let back = header_from_consensus(header);
        assert_eq!(back.header(), hashed.header());
        assert_eq!(back.hash(), hashed.hash());
        assert_eq!(hashed_header_to_consensus(hashed).as_ref(), Ok(header));
    }
}

fn check_headers2(bytes: &[u8]) {
    let NetworkMessage::Headers2(message) = decode_message("headers2", bytes) else {
        unreachable!()
    };
    let decompressed = CompressionState::new()
        .process_headers_with_hashes(&message.headers)
        .expect("dashcore decompresses Core's headers2");
    let base = headers2_to_base(&message, &decompressed).expect("crosses to base-sdk");
    assert_eq!(base_bytes(&base), bytes);
    assert_eq!(base, base_decode(bytes));
    let back = headers2_from_base(&base).expect("crosses back");
    assert_eq!(payload(NetworkMessage::Headers2(back)), bytes);
    assert!(!decompressed.is_empty());

    // Into dash-consensus: the decompressed headers, each with its hash.
    let consensus = headers2_to_consensus(&message).expect("crosses to dash-consensus");
    assert_eq!(consensus.len(), decompressed.len());
    for (header, (core, hash)) in consensus.iter().zip(&decompressed) {
        let back = header_from_consensus(header);
        assert_eq!((back.header(), back.hash()), (core, hash));
    }
}

fn check_block(bytes: &[u8]) {
    let block: Block = deserialize(bytes).expect("dashcore decodes the block");
    let hashed = HashedBlock::from(&block);
    let consensus = block_to_consensus(&block).expect("crosses to dash-consensus");
    let checked = hashed_block_to_consensus(&hashed).expect("dash-spv's pair crosses");
    assert_eq!(dash_consensus::core_serialize(&checked), bytes);
    assert_eq!(checked.hash(), consensus.hash());
    assert_eq!(dash_consensus::core_serialize(&consensus), bytes);
    let mut cursor = bytes;
    let decoded = dash_consensus::decode_block(&mut cursor).expect("dash-consensus decodes it");
    assert_eq!(consensus.hash(), decoded.value().hash());
    assert_eq!(consensus.merkle(), decoded.value().merkle());
    let back = block_from_consensus(&consensus).expect("crosses back");
    assert_eq!(back.hash(), hashed.hash());
    assert_eq!(serialize(back.block()), bytes);
}

fn check(kind: &str, bytes: &[u8]) {
    match kind {
        "headers" => check_headers(bytes),
        "headers2" => check_headers2(bytes),
        "block" => check_block(bytes),
        "cfilter" => {
            let NetworkMessage::CFilter(value) = decode_message(kind, bytes) else {
                unreachable!()
            };
            round_trip(bytes, value, cfilter_to_base, cfilter_from_base, NetworkMessage::CFilter);
        }
        "cfheaders" => {
            let NetworkMessage::CFHeaders(value) = decode_message(kind, bytes) else {
                unreachable!()
            };
            round_trip(
                bytes,
                value,
                cfheaders_to_base,
                cfheaders_from_base,
                NetworkMessage::CFHeaders,
            );
        }
        "mnlistdiff" => {
            let NetworkMessage::MnListDiff(value) = decode_message(kind, bytes) else {
                unreachable!()
            };
            round_trip(bytes, *value, mnlistdiff_to_base, mnlistdiff_from_base, |diff| {
                NetworkMessage::MnListDiff(Box::new(diff))
            });
        }
        "qrinfo" => {
            let NetworkMessage::QRInfo(value) = decode_message(kind, bytes) else {
                unreachable!()
            };
            round_trip(bytes, *value, qrinfo_to_base, qrinfo_from_base, |info| {
                NetworkMessage::QRInfo(Box::new(info))
            });
        }
        "clsig" => {
            let NetworkMessage::CLSig(value) = decode_message(kind, bytes) else {
                unreachable!()
            };
            round_trip(bytes, value, clsig_to_base, clsig_from_base, NetworkMessage::CLSig);
        }
        "isdlock" => {
            let NetworkMessage::ISLock(value) = decode_message(kind, bytes) else {
                unreachable!()
            };
            round_trip(bytes, value, isdlock_to_base, isdlock_from_base, NetworkMessage::ISLock);
        }
        other => panic!("no check for {other}"),
    }
}

fn data(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(path)
}

/// The oracle captures, each checked against the manifest's sha256 first.
fn oracle_captures() -> Vec<(String, String, Vec<u8>)> {
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(data("tests/data/bridge/manifest.json")).expect("the manifest"),
    )
    .expect("JSON");
    let files = manifest["files"].as_array().expect("files");
    let mut captures = Vec::new();
    for file in files {
        let path = file["file"].as_str().expect("file");
        let bytes = fs::read(data(&format!("tests/data/bridge/{path}"))).expect("the capture");
        assert_eq!(sha256::Hash::hash(&bytes).to_string(), file["sha256"], "{path} changed");
        let net = file["net"].as_str().expect("net").to_string();
        captures.push((net, file["kind"].as_str().expect("kind").to_string(), bytes));
    }
    captures
}

/// The `mnlistdiff` and `qrinfo` wire captures the masternode tests already use (mainnet), plus
/// the testnet whole-list diff.
fn dml_captures() -> Vec<(String, String, Vec<u8>)> {
    let wire = "../dash/tests/data/test_DML_diffs/core_wire";
    let mut files = vec![
        ("mainnet", "mnlistdiff", "../dash/tests/data/test_DML_diffs/mn_list_diff_0_2227096.bin"),
        (
            "mainnet",
            "mnlistdiff",
            "../dash/tests/data/test_DML_diffs/mn_list_diff_2227096_2241332.bin",
        ),
        ("testnet", "mnlistdiff", "../dash/artifacts/mn_list_diff_testnet_0_1296600.bin"),
    ];
    let mut wire_files: Vec<String> = Vec::new();
    for name in ["qrinfo_2240504.bin", "qrinfo_2518986.bin", "mnlistdiff_tip_2518986.bin"] {
        wire_files.push(format!("{wire}/{name}"));
    }
    for (from, to) in MNLISTDIFF_RANGES {
        wire_files.push(format!("{wire}/mnlistdiff_{from}_{to}.bin"));
    }
    for path in &wire_files {
        let kind = if path.contains("/qrinfo_") {
            "qrinfo"
        } else {
            "mnlistdiff"
        };
        files.push(("mainnet", kind, path.as_str()));
    }
    files
        .into_iter()
        .map(|(net, kind, path)| {
            (net.into(), kind.into(), fs::read(data(path)).expect("the capture"))
        })
        .collect()
}

/// The `core_wire` `mnlistdiff` captures, by base and target height.
const MNLISTDIFF_RANGES: [(u32, u32); 31] = [
    (2227096, 2239192),
    (2227096, 2239480),
    (2227096, 2239768),
    (2227096, 2240056),
    (2227096, 2240344),
    (2227096, 2240504),
    (2239480, 2239768),
    (2239768, 2239912),
    (2239912, 2239936),
    (2239936, 2239960),
    (2239960, 2239984),
    (2239984, 2240008),
    (2240008, 2240032),
    (2240032, 2240056),
    (2240056, 2240080),
    (2240080, 2240104),
    (2240104, 2240128),
    (2240128, 2240152),
    (2240152, 2240176),
    (2240176, 2240200),
    (2240200, 2240224),
    (2240224, 2240248),
    (2240248, 2240272),
    (2240272, 2240296),
    (2240296, 2240320),
    (2240320, 2240344),
    (2240344, 2240368),
    (2240368, 2240392),
    (2240392, 2240416),
    (2240416, 2240440),
    (2240440, 2240464),
];

#[test]
fn every_capture_round_trips_byte_equal() {
    let mut seen = std::collections::BTreeMap::<(String, String), usize>::new();
    for (net, kind, bytes) in oracle_captures().into_iter().chain(dml_captures()) {
        check(&kind, &bytes);
        *seen.entry((net, kind)).or_default() += 1;
    }
    for kind in ["headers", "headers2", "block", "cfilter", "cfheaders", "mnlistdiff", "clsig"] {
        for net in ["mainnet", "testnet"] {
            assert!(seen.contains_key(&(net.into(), kind.into())), "no {net} {kind} capture");
        }
    }
    // Two kinds have one net only (SB12a notes, D9): the mainnet oracle is still in IBD and relays
    // no ISLock, and a testnet `qrinfo` needs `getqrinfo`, which is not a pure read on the oracle.
    assert!(seen.contains_key(&("testnet".into(), "isdlock".into())));
    assert!(seen.contains_key(&("mainnet".into(), "qrinfo".into())));
    eprintln!("{seen:?}");
}
