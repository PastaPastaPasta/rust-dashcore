//! dash-spv's header path into `dash-consensus` (SB12a; PF0 `spv-headers-*`, the BG rows SB12 and
//! SB14 must not make slower): Core's mainnet replies crossed through `dash-spv::bridge`, each
//! header hashed once, by `dash-consensus` (DEC-224).
//!
//! - `headers/bridge/2000`: the crossing alone, back: dash-spv's hashed headers taken from
//!   2,000 `dash-consensus` headers (no hashing).
//! - `headers/hash_and_bridge/2000`: 2,000 received headers into `dash-consensus` (one X11 each)
//!   and dash-spv's hashed headers taken from them.
//! - `headers2/decompress_and_bridge/8000`: a full `headers2` reply decompressed into
//!   `dash-consensus` (one X11 per header) and dash-spv's hashed headers taken from it.

use criterion::{criterion_group, criterion_main, Criterion};
use dash_consensus::Header;
use dash_spv::bridge::headers::{headers_from_consensus, headers_to_consensus};
use dash_spv::bridge::messages::headers2_to_consensus;
use dashcore::consensus::deserialize;
use dashcore::hashes::{sha256d, Hash};
use dashcore::network::message::{NetworkMessage, RawNetworkMessage};
use dashcore::network::message_headers2::Headers2Message;
use std::hint::black_box;

fn decode(command: &str, payload: &[u8]) -> NetworkMessage {
    let mut raw = vec![0; 4];
    let mut name = [0u8; 12];
    name[..command.len()].copy_from_slice(command.as_bytes());
    raw.extend_from_slice(&name);
    raw.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    raw.extend_from_slice(&sha256d::Hash::hash(payload)[..4]);
    raw.extend_from_slice(payload);
    deserialize::<RawNetworkMessage>(&raw).expect("Core's reply decodes").payload
}

fn mainnet_headers() -> Vec<dashcore::block::Header> {
    let payload = include_bytes!("../tests/data/bridge/mainnet/headers-2080001.bin");
    let NetworkMessage::Headers(headers) = decode("headers", payload) else {
        unreachable!()
    };
    headers
}

fn mainnet_headers2() -> Headers2Message {
    let payload = include_bytes!("../tests/data/bridge/mainnet/headers2-2080001.bin");
    let NetworkMessage::Headers2(message) = decode("headers2", payload) else {
        unreachable!()
    };
    message
}

// Fixtures are built inside each closure, so a workload run alone (PF0's shim) builds only its own.
fn bench_headers(c: &mut Criterion) {
    let mut headers = None;
    let mut consensus: Option<Vec<Header>> = None;
    c.bench_function("headers/bridge/2000", |b| {
        let consensus = consensus.get_or_insert_with(|| {
            headers_to_consensus(headers.get_or_insert_with(mainnet_headers))
        });
        b.iter(|| headers_from_consensus(black_box(consensus)))
    });
    c.bench_function("headers/hash_and_bridge/2000", |b| {
        let headers = headers.get_or_insert_with(mainnet_headers);
        b.iter(|| headers_from_consensus(&headers_to_consensus(black_box(headers))))
    });

    let mut message = None;
    c.bench_function("headers2/decompress_and_bridge/8000", |b| {
        let message = message.get_or_insert_with(mainnet_headers2);
        b.iter(|| {
            let consensus =
                headers2_to_consensus(black_box(message)).expect("Core's reply crosses");
            headers_from_consensus(&consensus)
        })
    });
}

criterion_group!(benches, bench_headers);
criterion_main!(benches);
