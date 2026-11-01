//! CS0's benchmarks (CS0 §9.6), each matched to a Dash Core `bench_dash` row on Core's benchmark
//! block block813851:
//!
//! - `block_decode_hash`: `decode_block`, i.e. the per-transaction comparison and hashes, the
//!   header's X11 and the merkle root. Core: `DeserializeBlockHashMerkle` (a local bench, DEC-30
//!   N4: deserialize, `GetHash()`, `BlockMerkleRoot`).
//! - `block_txs_decode`: the work of Core's `DeserializeBlockTest`: the header and count decoded,
//!   every transaction decoded (with its hash) into a vector of shared references, dropped at the
//!   end of the iteration; no X11, no merkle root.
//! - `merkle_root_9001`: Core's `MerkleRoot` shape: 9,001 leaves, copied into the call each
//!   iteration as Core copies its vector.
//!
//! Every iteration decodes and drops, as Core constructs and destroys its `CBlock`. Wall-clock
//! comparisons with Core run only in a manager-reserved quiet window (DEC-23(b)).

#[path = "../tests/support/gen.rs"]
mod generate;

use std::hint::black_box;
use std::sync::Arc;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use dash_consensus::merkle::compute_merkle_root;
use dash_consensus::primitives::BlockHeader;
use dash_consensus::{TxRef, decode_block, decode_tx};
use dash_core_vectors::read_core_file;
use dash_types::CompactSize;
use dash_types::codec::BaseCodec;

use generate::Rng;

fn block813851() -> Vec<u8> {
    read_core_file("src/bench/data/block813851.raw")
}

fn block_decode_hash(c: &mut Criterion) {
    let raw = block813851();
    let mut group = c.benchmark_group("block813851");
    group.throughput(Throughput::Bytes(raw.len() as u64));
    group.bench_function("block_decode_hash", |b| {
        b.iter(|| decode_block(&mut black_box(&raw[..])).expect("decodes"))
    });
    group.bench_function("block_txs_decode", |b| {
        b.iter(|| {
            let mut cursor = black_box(&raw[..]);
            let header = BlockHeader::decode(&mut cursor).expect("header");
            let count = CompactSize::decode(&mut cursor)
                .expect("count")
                .into_len(usize::MAX)
                .expect("count");
            let mut txs: Vec<TxRef> = Vec::with_capacity(count);
            for _ in 0..count {
                txs.push(Arc::new(decode_tx(&mut cursor).expect("tx").into_value()));
            }
            (header, txs)
        })
    });
    group.finish();
}

fn merkle_root_9001(c: &mut Criterion) {
    // Deterministic leaves, prepared outside the timed loop.
    let mut rng = Rng::new(0);
    let leaves: Vec<[u8; 32]> = (0..9001)
        .map(|_| {
            let mut leaf = [0u8; 32];
            for chunk in leaf.chunks_mut(8) {
                chunk.copy_from_slice(&rng.next_u64().to_le_bytes());
            }
            leaf
        })
        .collect();
    c.bench_function("merkle_root_9001", |b| {
        b.iter(|| compute_merkle_root(black_box(leaves.clone())))
    });
}

criterion_group!(benches, block_decode_hash, merkle_root_9001);
criterion_main!(benches);
