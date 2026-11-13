//! No header is hashed twice from the wire into `dash-consensus` (SB12a, DEC-51a, DEC-224).
//!
//! The internal `__x11-count` feature counts every X11 dashcore computes (`dashcore_hashes`) and
//! every one `dash-consensus` computes, in debug builds only. The counts are per thread and
//! everything here runs on the test's thread. The gate runs these with `--features __x11-count`
//! (`[package.metadata.gate]`).
//!
//! Each received header is hashed once, by `dash-consensus`, the only place its hash there can
//! come from (DEC-224); dash-spv takes its header and hash from the result, which hashes
//! nothing. What the captures' headers are checked against (dashcore's own hashing) is computed
//! outside the counted spans.

use dash_consensus::hash::x11_calls as consensus_x11;
use dashcore::consensus::deserialize;
use dashcore::hashes::hash_x11::x11_calls as dashcore_x11;
use dashcore::network::message::{NetworkMessage, RawNetworkMessage};
use dashcore::network::message_headers2::CompressionState;
use dashcore::Block;

use super::headers::{
    block_from_consensus, block_to_consensus, hashed_header_to_consensus, headers_from_consensus,
    headers_to_consensus,
};
use super::messages::headers2_to_consensus;
use super::messages::tests::with_prev_hash;
use crate::types::{HashedBlock, HashedBlockHeader};

/// X11 computations so far on this thread: dashcore's and dash-consensus's.
fn counts() -> (u64, u64) {
    (dashcore_x11::count(), consensus_x11::count())
}

/// The counts since `before`.
fn since(before: (u64, u64)) -> (u64, u64) {
    let now = counts();
    (now.0 - before.0, now.1 - before.1)
}

/// A capture's payload decoded as a peer's message is, which hashes nothing.
fn decode(command: &str, payload: &[u8]) -> NetworkMessage {
    let mut raw = vec![0; 4];
    let mut name = [0u8; 12];
    name[..command.len()].copy_from_slice(command.as_bytes());
    raw.extend_from_slice(&name);
    raw.extend_from_slice(&u32::try_from(payload.len()).expect("small").to_le_bytes());
    raw.extend_from_slice(&dash_consensus::hash::sha256d(payload)[..4]);
    raw.extend_from_slice(payload);
    deserialize::<RawNetworkMessage>(&raw).expect("dashcore decodes the capture").payload
}

/// Core's 2,000-header mainnet `headers` reply.
fn mainnet_headers() -> Vec<dashcore::block::Header> {
    let payload = include_bytes!("../../tests/data/bridge/mainnet/headers-2080001.bin");
    let NetworkMessage::Headers(headers) = decode("headers", payload) else {
        unreachable!()
    };
    headers
}

/// A 2,000-header `headers` reply: decoded, crossed into `dash-consensus` (one X11 per header,
/// there), and dash-spv's hashed headers taken from the result. Nothing else hashes.
#[test]
fn a_headers_batch_is_hashed_once() {
    let headers = mainnet_headers();
    assert_eq!(headers.len(), 2000);

    let before = counts();
    let consensus = headers_to_consensus(&headers);
    let hashed = headers_from_consensus(&consensus);
    let linked = consensus.windows(2).all(|pair| pair[1].prev_hash() == pair[0].hash());
    assert_eq!(since(before), (0, 2000));
    assert!(linked, "Core's reply is a chain");

    let expected: Vec<HashedBlockHeader> = headers.iter().map(HashedBlockHeader::from).collect();
    assert!(hashed
        .iter()
        .zip(&expected)
        .all(|(a, b)| a.header() == b.header() && a.hash() == b.hash()));
}

/// A `headers2` reply: decompressed into `dash-consensus`, each header hashed there once (the
/// hashes the decompression needs are those), and dash-spv's headers taken from the result.
#[test]
fn a_headers2_batch_is_hashed_once() {
    for payload in [
        &include_bytes!("../../tests/data/bridge/mainnet/headers2-2080001.bin")[..],
        &include_bytes!("../../tests/data/bridge/testnet/headers2-1376001.bin")[..],
    ] {
        let NetworkMessage::Headers2(mut message) = decode("headers2", payload) else {
            unreachable!()
        };
        // One header carries a prev hash the decompression could restore, so it asks for one
        // hash fewer, and that header is hashed after.
        let decompressed = CompressionState::new()
            .process_headers_with_hashes(&message.headers)
            .expect("dashcore decompresses Core's headers2");
        let headers: Vec<_> = decompressed.iter().map(|(header, _)| *header).collect();
        with_prev_hash(&mut message, &headers, 4000);
        // A full reply: Core's `MAX_HEADERS_COMPRESSED_RESULT`.
        let count = message.headers.len();
        assert_eq!(count, 8000);

        let before = counts();
        let consensus = headers2_to_consensus(&message).expect("crosses");
        let hashed = headers_from_consensus(&consensus);
        assert_eq!(since(before), (0, count as u64));

        let expected = CompressionState::new()
            .process_headers_with_hashes(&message.headers)
            .expect("dashcore decompresses Core's headers2");
        assert_eq!(expected, decompressed);
        assert_eq!(hashed.len(), expected.len());
        assert!(hashed
            .iter()
            .zip(&expected)
            .all(|(a, (header, hash))| a.header() == header && a.hash() == hash));
    }
}

/// A block crosses with the one hash `dash-consensus` computes for it, and back with that hash.
#[test]
fn a_block_is_hashed_once() {
    let block: Block =
        deserialize(include_bytes!("../../tests/data/bridge/mainnet/block-2089979.bin"))
            .expect("dashcore decodes the block");
    let before = counts();
    let consensus = block_to_consensus(&block).expect("crosses");
    let back = block_from_consensus(&consensus).expect("crosses back");
    assert_eq!(since(before), (0, 1));
    assert_eq!(back.hash(), HashedBlock::from(&block).hash());
    assert_eq!(back.block(), &block);
}

/// The price of a hash that came with its header (storage, dash-spv's own hashing): one more
/// X11 per header, to check it.
#[test]
fn a_header_with_a_hash_is_hashed_again() {
    let headers = mainnet_headers();
    let stored: Vec<HashedBlockHeader> = headers.iter().map(HashedBlockHeader::from).collect();
    let before = counts();
    for header in &stored {
        hashed_header_to_consensus(header).expect("the pair is the header's own");
    }
    assert_eq!(since(before), (0, stored.len() as u64));
}
