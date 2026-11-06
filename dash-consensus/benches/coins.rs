//! CS5a's benchmarks (BG: crit coin decode): the coins database and undo decoders, each matched
//! to a Dash Core `bench_dash` row doing the same work on the same bytes. The Core rows are a
//! local patch of `src/bench/ccoins_caching.cpp` on the Core branch `node/cs5a-bench` (DEC-16):
//!
//! - `coin_decode/{p2pkh, p2sh, p2pk_compressed, p2pk_uncompressed, raw40}`: `Coin::decode` of
//!   one coin, dropped. Core: `CoinDecode<Kind>`, `stream >> coin` into a fresh `Coin` from a
//!   stream rewound each iteration. The uncompressed P2PK coin decompresses its key on both sides.
//! - `coin_ref_decode/<kind>`: the borrowed decoder, `CoinRef::decode`, on the same bytes: what the
//!   coins cache does on every access. It does not decompress a special form, so it does less
//!   than Core's row (no key decompression for uncompressed P2PK); it has no Core counterpart and
//!   is reported beside `coin_decode`.
//! - `coin_encode/<kind>`: `coin_bytes` of the coin's output into a reused buffer. Core:
//!   `CoinSerialize<Kind>`, `Coin::Serialize` into a cleared stream.
//! - `coin_to_undo/<kind>`: the cache's transcoding, database bytes to an undo entry:
//!   `CoinRef::decode`, then `write_undo` into a reused buffer. Core: `CoinToUndo<Kind>`, `stream >>
//!   coin`, then `TxInUndoFormatter::Ser` into a cleared stream. End to end, because the two sides
//!   pay an uncompressed key's decompression at different steps (Core when reading, the borrowed
//!   coin when writing).
//! - `block_undo_decode`: `BlockUndo::decode` of a synthetic record (500 transactions of 4 inputs;
//!   of every 20 coins 14 are P2PKH, 4 P2SH, 1 compressed P2PK, 1 a raw 40-byte script: an
//!   assumed mix, not a measured one). Core: `BlockUndoDecode`. `block_undo_ref_decode` is the
//!   borrowed `BlockUndoRef::decode` (no Core counterpart). `block_undo_decode_bound` is
//!   `decode_block_undo`, which also decides `wire_is_canonical` by re-encoding into a comparison;
//!   Core has no such step.
//!
//! Each coin is built from the same fields on both sides and serialized by each side's own
//! encoder; the L1 comparisons show the bytes agree. `REPORT.md` (CS5a section) has the numbers.
//!
//! `coins --count <iterations> <name>` runs one workload a fixed number of times without
//! criterion, so `perf stat` can take the per-iteration instruction count as the slope between
//! two counts.

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group};
use dash_consensus::coins::{
    BlockUndo, BlockUndoRef, Coin, CoinRef, TxUndo, coin_bytes, decode_block_undo,
};
use dash_consensus::primitives::TxOut;

const GX: [u8; 32] = [
    0x79, 0xbe, 0x66, 0x7e, 0xf9, 0xdc, 0xbb, 0xac, 0x55, 0xa0, 0x62, 0x95, 0xce, 0x87, 0x0b, 0x07,
    0x02, 0x9b, 0xfc, 0xdb, 0x2d, 0xce, 0x28, 0xd9, 0x59, 0xf2, 0x81, 0x5b, 0x16, 0xf8, 0x17, 0x98,
];
const GY: [u8; 32] = [
    0x48, 0x3a, 0xda, 0x77, 0x26, 0xa3, 0xc4, 0x65, 0x5d, 0xa4, 0xfb, 0xfc, 0x0e, 0x11, 0x08, 0xa8,
    0xfd, 0x17, 0xb4, 0x48, 0xa6, 0x85, 0x54, 0x19, 0x9c, 0x47, 0xd0, 0x8f, 0xfb, 0x10, 0xd4, 0xb8,
];

fn p2pkh(fill: u8) -> Vec<u8> {
    [&[0x76, 0xa9, 20][..], &[fill; 20], &[0x88, 0xac]].concat()
}

fn p2sh(fill: u8) -> Vec<u8> {
    [&[0xa9, 20][..], &[fill; 20], &[0x87]].concat()
}

fn p2pk_compressed() -> Vec<u8> {
    [&[33, 0x02][..], &GX, &[0xac]].concat()
}

fn p2pk_uncompressed() -> Vec<u8> {
    [&[65, 0x04][..], &GX, &GY, &[0xac]].concat()
}

fn raw40() -> Vec<u8> {
    vec![0x51; 40]
}

/// The coins of the `coin_decode` rows, as Core's `CoinDecode<Kind>` builds them.
fn coins() -> [(&'static str, Coin); 5] {
    [
        ("p2pkh", Coin::new(123_456_789, &p2pkh(0x11), 2_000_000, false)),
        ("p2sh", Coin::new(5_000_000_000, &p2sh(0x22), 2_000_001, false)),
        ("p2pk_compressed", Coin::new(5_000_000_000, &p2pk_compressed(), 100, true)),
        ("p2pk_uncompressed", Coin::new(50_000_000_000, &p2pk_uncompressed(), 1, true)),
        ("raw40", Coin::new(0, &raw40(), 1_500_000, false)),
    ]
}

/// Core's `MainnetShapedUndo` (a synthetic mix).
fn mainnet_shaped_undo() -> Vec<u8> {
    let undo = BlockUndo {
        txs: (0..500u32)
            .map(|t| TxUndo {
                coins: (0..4)
                    .map(|k| {
                        let i = t * 4 + k;
                        let fill = i as u8;
                        let script = match i % 20 {
                            0..14 => p2pkh(fill),
                            14..18 => p2sh(fill),
                            18 => p2pk_compressed(),
                            _ => raw40(),
                        };
                        Coin::new(1000 + i64::from(i) * 7919, &script, 1_900_000 + i, i % 50 == 0)
                    })
                    .collect(),
            })
            .collect(),
    };
    undo.core_bytes().as_slice().to_vec()
}

/// One workload's iteration, by name.
/// One workload's iteration, by name.
fn workload(name: &str) -> Option<Box<dyn FnMut()>> {
    match name {
        "block_undo_decode" => {
            let bytes = mainnet_shaped_undo();
            return Some(Box::new(move || {
                black_box(BlockUndo::decode(&mut black_box(&bytes[..])).expect("decodes"));
            }));
        }
        "block_undo_ref_decode" => {
            let bytes = mainnet_shaped_undo();
            return Some(Box::new(move || {
                black_box(BlockUndoRef::decode(&mut black_box(&bytes[..])).expect("decodes"));
            }));
        }
        "block_undo_decode_bound" => {
            let bytes = mainnet_shaped_undo();
            return Some(Box::new(move || {
                black_box(decode_block_undo(&mut black_box(&bytes[..])).expect("decodes"));
            }));
        }
        _ => {}
    }
    let (row, kind) = name.split_once('/')?;
    let (_, coin) = coins().into_iter().find(|(k, _)| *k == kind)?;
    let bytes = coin.serialize();
    let mut out = Vec::with_capacity(128);
    match row {
        "coin_decode" => Some(Box::new(move || {
            black_box(Coin::decode(&mut black_box(&bytes[..])).expect("decodes"));
        })),
        "coin_ref_decode" => Some(Box::new(move || {
            black_box(CoinRef::decode(&mut black_box(&bytes[..])).expect("decodes"));
        })),
        "coin_encode" => {
            let txout = TxOut {
                value: coin.value(),
                script_pubkey: coin.script_pubkey().to_vec().into(),
            };
            let (height, coinbase) = (coin.height(), coin.is_coinbase());
            Some(Box::new(move || {
                out.clear();
                coin_bytes(black_box(&txout), height, coinbase, &mut out);
                black_box(&out);
            }))
        }
        "coin_to_undo" => Some(Box::new(move || {
            out.clear();
            let coin = CoinRef::decode(&mut black_box(&bytes[..])).expect("decodes");
            coin.write_undo(&mut out);
            black_box(&out);
        })),
        _ => None,
    }
}

fn coin_decode(c: &mut Criterion) {
    for row in ["coin_decode", "coin_ref_decode", "coin_encode", "coin_to_undo"] {
        let mut group = c.benchmark_group(row);
        for (kind, _) in coins() {
            let mut run = workload(&format!("{row}/{kind}")).expect("a workload");
            group.bench_function(kind, |b| b.iter(&mut run));
        }
        group.finish();
    }

    let mut group = c.benchmark_group("block_undo");
    group.throughput(Throughput::Elements(2000));
    for name in ["block_undo_decode", "block_undo_ref_decode", "block_undo_decode_bound"] {
        let mut run = workload(name).expect("a workload");
        group.bench_function(name, |b| b.iter(&mut run));
    }
    group.finish();
}

criterion_group!(benches, coin_decode);

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(at) = args.iter().position(|a| a == "--count") {
        let count: u64 = args.get(at + 1).and_then(|n| n.parse().ok()).expect("--count <n> <name>");
        let name = args.get(at + 2).expect("--count <n> <name>");
        let mut run = workload(name).expect("a workload name");
        for _ in 0..count {
            run();
        }
        return;
    }
    benches();
    Criterion::default().configure_from_args().final_summary();
}
