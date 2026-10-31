//! The boundary table of CS0 §9.1, compared live against Dash Core's oracle (v24).
//!
//! Each row's input is built here, deterministically, and passed to the shared `node-l1`
//! comparisons; Core's answers come from the oracle at test time and are never recorded
//! (CS0 §9.1, P2-B1). A failure names the area, the row's index, the field and the pin; when
//! `DASH_EMBARGO_DIR` is set the input goes to the private sink (CS0 §9.5).

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::use_debug,
    clippy::dbg_macro,
    clippy::print_stdout
)]

#[path = "support/spec.rs"]
mod spec;

use std::sync::Arc;

use dash_consensus::primitives::core_serialize;
use dash_consensus::{Block, decode_block, decode_header, decode_tx};
use dash_core_vectors::read_core_file;
use dash_oracle_sys::{Oracle, Pin};
use node_l1::{Compared, compare_block, compare_header, compare_tx, compare_tx_exact};
use node_sink::sink::record_for_test;

use spec::{In, MAX_MONEY, TxSpec, asset_unlock_v2, block_of, header, unlock_payload};

const MAX_SIZE: u64 = 0x0200_0000;

/// The rows' results: failures are named.
#[derive(Default)]
struct Table {
    rows: usize,
    failures: Vec<String>,
}

impl Table {
    fn row(&mut self, area: &str, compare: fn(&[u8], Pin) -> Compared, bytes: &[u8]) {
        let index = self.rows;
        self.rows += 1;
        for pin in Pin::ALL {
            if let Err(divergence) = compare(bytes, pin) {
                let _ = record_for_test("l1_boundaries", bytes, pin.name(), divergence.field);
                self.failures.push(format!("{area} #{index}: {divergence} ({})", pin.name()));
            }
        }
    }

    fn tx(&mut self, area: &str, bytes: &[u8]) {
        self.row(area, compare_tx, bytes);
        self.row(area, compare_tx_exact, bytes);
    }

    fn check(&mut self, area: &str, ok: bool, what: &str) {
        self.rows += 1;
        if !ok {
            self.failures.push(format!("{area}: {what}"));
        }
    }

    fn finish(self, area: &str) {
        node_sink::report(&format!("{area}: {} rows, {} failures", self.rows, self.failures.len()));
        assert!(self.failures.is_empty(), "{area}: {}", self.failures.join("; "));
    }
}

#[test]
fn compact_sizes() {
    let mut t = Table::default();
    let counts = [0u64, 1, 252, 253, 0xffff, 0x1_0000];
    for &n in &counts {
        // Input count, output count, both script lengths, payload length.
        t.tx(
            "input count",
            &TxSpec {
                inputs: vec![In::spend(7); n as usize],
                ..TxSpec::plain()
            }
            .bytes(),
        );
        t.tx(
            "output count",
            &TxSpec {
                outputs: vec![(1, vec![0x51]); n as usize],
                ..TxSpec::plain()
            }
            .bytes(),
        );
        let mut input = In::spend(7);
        input.script = vec![0x51; n as usize];
        t.tx(
            "script_sig length",
            &TxSpec {
                inputs: vec![input],
                ..TxSpec::plain()
            }
            .bytes(),
        );
        t.tx(
            "script_pubkey length",
            &TxSpec {
                outputs: vec![(1, vec![0x51; n as usize])],
                ..TxSpec::plain()
            }
            .bytes(),
        );
        t.tx("payload length", &TxSpec::special(5, vec![0; n as usize]).bytes());
        let txs = vec![TxSpec::plain().bytes(); n as usize];
        t.row("block transaction count", compare_block, &block_of(&txs));
    }
    // Encodings of values too large to build, non-minimal encodings and MAX_SIZE, as the prefix
    // of each length-prefixed field.
    let prefixes: [&[u8]; 8] = [
        &[0xfe, 0xff, 0xff, 0xff, 0xff],
        &[0xff, 0, 0, 0, 0, 1, 0, 0, 0],
        &[0xfd, 0x05, 0x00],
        &[0xfe, 0x05, 0, 0, 0],
        &[0xff, 0x05, 0, 0, 0, 0, 0, 0, 0],
        &[0xfd, 0xfc, 0x00],
        &[0xfe, 0x00, 0x00, 0x00, 0x02],
        &[0xfe, 0x01, 0x00, 0x00, 0x02],
    ];
    for prefix in prefixes {
        let mut count = 1i16.to_le_bytes().to_vec();
        count.extend_from_slice(&0u16.to_le_bytes());
        count.extend_from_slice(prefix);
        t.tx("input count encoding", &count);
        let mut tx = TxSpec::plain().bytes();
        // Drop the lock time, the output and its count: 4 + 10 + 1 bytes.
        tx.truncate(tx.len() - 15);
        tx.extend_from_slice(prefix);
        t.tx("output count encoding", &tx);
        let mut block = header();
        block.extend_from_slice(prefix);
        t.row("block count encoding", compare_block, &block);
        let mut special = TxSpec::special(5, vec![]).bytes();
        special.pop();
        special.extend_from_slice(prefix);
        t.tx("payload length encoding", &special);
    }
    let mut at_max = header();
    dash_consensus::primitives::write_compact_size(MAX_SIZE, &mut at_max);
    t.row("block count at MAX_SIZE", compare_block, &at_max);
    t.finish("compact sizes");
}

#[test]
fn integer_fields() {
    let mut t = Table::default();
    let versions = [i16::MIN, i16::MIN + 1, -1, 0, 1, 2, 3, 4, i16::MAX - 1, i16::MAX];
    let types: Vec<u16> = (0..=13).chain([0x7fff, 0x8000, 0xfffe, 0xffff]).collect();
    for &version in &versions {
        for &tx_type in &types {
            for payload in [None, Some(vec![]), Some(vec![1, 2, 3])] {
                let spec = TxSpec {
                    version,
                    tx_type,
                    payload,
                    ..TxSpec::plain()
                };
                t.tx("version and type", &spec.bytes());
            }
        }
    }
    for lock_time in [0, 1, 499_999_999, 500_000_000, u32::MAX] {
        t.tx(
            "lock time",
            &TxSpec {
                lock_time,
                ..TxSpec::plain()
            }
            .bytes(),
        );
    }
    for (index, sequence, hash) in [
        (0, 0, [7; 32]),
        (u32::MAX, u32::MAX, [7; 32]),
        (u32::MAX, 0, [0; 32]),
        (0, u32::MAX, [0; 32]),
    ] {
        let input = In {
            hash,
            index,
            script: vec![],
            sequence,
        };
        t.tx(
            "prevout and sequence",
            &TxSpec {
                inputs: vec![input],
                ..TxSpec::plain()
            }
            .bytes(),
        );
    }
    for value in [i64::MIN, -1, 0, 1, MAX_MONEY, MAX_MONEY + 1, i64::MAX] {
        t.tx(
            "output value",
            &TxSpec {
                outputs: vec![(value, vec![0x51])],
                ..TxSpec::plain()
            }
            .bytes(),
        );
    }
    t.finish("integer fields");
}

#[test]
fn shapes() {
    let mut t = Table::default();
    t.tx(
        "empty inputs",
        &TxSpec {
            inputs: vec![],
            ..TxSpec::plain()
        }
        .bytes(),
    );
    t.tx(
        "empty outputs",
        &TxSpec {
            outputs: vec![],
            ..TxSpec::plain()
        }
        .bytes(),
    );
    t.tx(
        "empty inputs and outputs",
        &TxSpec {
            inputs: vec![],
            outputs: vec![],
            ..TxSpec::plain()
        }
        .bytes(),
    );
    t.tx("coinbase", &TxSpec::coinbase().bytes());
    for len in [0usize, 1, 10_000, 10_240, 10_241] {
        t.tx("payload length", &TxSpec::special(5, vec![0; len]).bytes());
    }
    // Near Core's decoder bound: the largest payload length Core reads, with the bytes present.
    let near_bound = MAX_SIZE as usize - 64;
    t.tx("payload near MAX_SIZE", &TxSpec::special(5, vec![0; near_bound]).bytes());
    t.row("empty block", compare_block, &block_of(&[]));
    t.finish("shapes");
}

/// A transaction with a length-prefixed field at `MAX_SIZE` or one byte over, all its bytes
/// present, alone and in a block after a coinbase. Core's `ReadCompactSize` checks the length
/// before it reads the field. The cursor is checked with the transaction read after a coinbase,
/// and with it and the block followed by a suffix: a decode moves the cursor past exactly its
/// bytes, and a refusal leaves it where it was.
fn at_max_size(t: &mut Table, area: &str, tx: &[u8]) {
    let coinbase = TxSpec::coinbase().bytes();
    let suffix = [0xde, 0xad, 0xbe, 0xef];
    t.tx(area, tx);
    let mut block = block_of(&[coinbase.clone(), tx.to_vec()]);
    t.row(&format!("{area} in a block"), compare_block, &block);

    let mut stream = coinbase;
    stream.extend_from_slice(tx);
    stream.extend_from_slice(&suffix);
    let mut cursor = &stream[..];
    let first = decode_tx(&mut cursor).is_ok() && cursor.len() == tx.len() + suffix.len();
    let left = if decode_tx(&mut cursor).is_ok() {
        suffix.len()
    } else {
        tx.len() + suffix.len()
    };
    t.check(area, first && cursor.len() == left, "the cursor after the transaction");

    block.extend_from_slice(&suffix);
    let mut cursor = &block[..];
    let left = if decode_block(&mut cursor).is_ok() {
        suffix.len()
    } else {
        block.len()
    };
    t.check(area, cursor.len() == left, "the cursor after the block");
}

/// Scripts at `MAX_SIZE` and one byte over ([`at_max_size`]), as a scriptSig and as a
/// scriptPubKey.
#[test]
fn scripts_at_max_size() {
    let mut t = Table::default();
    for len in [MAX_SIZE, MAX_SIZE + 1] {
        let script = vec![0x51; len as usize];
        let mut input = In::spend(7);
        input.script = script.clone();
        let txs = [
            (
                "script_sig",
                TxSpec {
                    inputs: vec![input],
                    ..TxSpec::plain()
                },
            ),
            (
                "script_pubkey",
                TxSpec {
                    outputs: vec![(1, script)],
                    ..TxSpec::plain()
                },
            ),
        ];
        for (field, spec) in txs {
            at_max_size(&mut t, &format!("{field} of {len} bytes"), &spec.bytes());
        }
    }
    t.finish("scripts at MAX_SIZE");
}

/// A special transaction's extra payload at `MAX_SIZE` and one byte over ([`at_max_size`]).
#[test]
fn payloads_at_max_size() {
    let mut t = Table::default();
    for len in [MAX_SIZE, MAX_SIZE + 1] {
        let tx = TxSpec::special(5, vec![0; len as usize]).bytes();
        at_max_size(&mut t, &format!("extra payload of {len} bytes"), &tx);
    }
    t.finish("payloads at MAX_SIZE");
}

#[test]
fn truncation_and_suffixes() {
    let mut t = Table::default();
    let representative = [
        TxSpec::plain().bytes(),
        TxSpec::coinbase().bytes(),
        TxSpec::special(5, vec![1, 2, 3]).bytes(),
        TxSpec {
            inputs: vec![],
            outputs: vec![],
            ..TxSpec::plain()
        }
        .bytes(),
    ];
    for tx in &representative {
        for end in 0..tx.len() {
            t.tx("transaction prefix", &tx[..end]);
        }
        let mut suffixed = tx.clone();
        suffixed.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        t.tx("transaction with a suffix", &suffixed);
        let mut cursor = &suffixed[..];
        let decoded = decode_tx(&mut cursor);
        t.check(
            "suffix",
            decoded.is_ok() && cursor == [0xde, 0xad, 0xbe, 0xef],
            "the suffix is left",
        );
        let mut two = tx.clone();
        two.extend_from_slice(&representative[0]);
        let mut cursor = &two[..];
        let first = decode_tx(&mut cursor).map(|d| d.wire().len());
        let second = decode_tx(&mut cursor).map(|d| d.wire().len());
        t.check(
            "back to back",
            first == Ok(tx.len()) && second == Ok(representative[0].len()) && cursor.is_empty(),
            "two transactions decode in turn",
        );
    }
    let header = header();
    for end in 0..header.len() {
        t.row("header prefix", compare_header, &header[..end]);
    }
    let mut suffixed = header.clone();
    suffixed.push(0);
    t.row("header with a suffix", compare_header, &suffixed);
    let mut cursor = &suffixed[..];
    t.check(
        "header suffix",
        decode_header(&mut cursor).is_ok() && cursor == [0],
        "the suffix is left",
    );

    let small = block_of(&representative[..3]);
    for end in 0..small.len() {
        t.row("block prefix", compare_block, &small[..end]);
    }
    let mut suffixed = small.clone();
    suffixed.push(0);
    t.row("block with a suffix", compare_block, &suffixed);
    let mut cursor = &suffixed[..];
    t.check(
        "block suffix",
        decode_block(&mut cursor).is_ok() && cursor == [0],
        "the suffix is left",
    );

    // block813851 at every transaction boundary, and one byte either side.
    let raw = read_core_file("src/bench/data/block813851.raw");
    let mut cursor = &raw[..];
    let Ok(block) = decode_block(&mut cursor) else {
        panic!("block813851 decodes");
    };
    let mut offset = 80 + 3; // header and a 3-byte count (999 transactions)
    let mut ends = Vec::new();
    for tx in block.value().txs() {
        offset += tx.size() as usize;
        ends.push(offset);
    }
    for end in ends {
        for cut in [end - 1, end, end + 1] {
            if cut <= raw.len() {
                t.row("block813851 prefix", compare_block, &raw[..cut]);
            }
        }
    }
    t.finish("truncation and suffixes");
}

/// Header fields at their extremes, alone and combined.
#[test]
fn header_fields() {
    let mut t = Table::default();
    let fields = |version: i32, time: u32, bits: u32, nonce: u32| {
        let mut out = Vec::with_capacity(80);
        out.extend_from_slice(&version.to_le_bytes());
        out.extend_from_slice(&[0x11; 32]);
        out.extend_from_slice(&[0x22; 32]);
        out.extend_from_slice(&time.to_le_bytes());
        out.extend_from_slice(&bits.to_le_bytes());
        out.extend_from_slice(&nonce.to_le_bytes());
        out
    };
    for version in [i32::MIN, -1, 0, 1, 4, 0x2000_0000, i32::MAX] {
        for value in [0, 1, 0x7fff_ffff, 0x8000_0000, u32::MAX] {
            t.row(
                "header version and time",
                compare_header,
                &fields(version, value, 0x207f_ffff, 0),
            );
            t.row("header version and bits", compare_header, &fields(version, 1, value, 0));
            t.row(
                "header version and nonce",
                compare_header,
                &fields(version, 1, 0x207f_ffff, value),
            );
            let block = {
                let mut b = fields(version, value, value, value);
                b.push(1);
                b.extend(TxSpec::coinbase().bytes());
                b
            };
            t.row("block header fields", compare_block, &block);
        }
    }
    t.finish("header fields");
}

/// v24's stable asset-unlock txid (A1 §2.25 row 5): the shape's boundaries,
/// and Core's own cases (`evo_assetlocks_tests.cpp:525-545`).
#[test]
fn stable_txid() {
    let mut t = Table::default();
    let coinbase = TxSpec::coinbase().bytes();
    for tx_version in [2i16, 3, 4] {
        for tx_type in [9u16, 8] {
            for len in [144usize, 145, 146] {
                for payload_version in [1u8, 2, 3, 255] {
                    let spec = TxSpec {
                        version: tx_version,
                        tx_type,
                        inputs: vec![],
                        payload: Some(unlock_payload(payload_version, len)),
                        ..TxSpec::plain()
                    };
                    let tx = spec.bytes();
                    t.tx("stable txid", &tx);
                    t.row(
                        "stable txid in a block",
                        compare_block,
                        &block_of(&[coinbase.clone(), tx]),
                    );
                }
            }
        }
    }

    // Core's cases: the payload is version (1) | index (8) | fee (4) | requestedHeight (4) |
    // quorumHash (32) | quorumSig (96). Re-signing changes only the last three; another
    // withdrawal changes the index or the fee.
    let ids = |spec: &TxSpec, pin: Pin| {
        let bytes = spec.bytes();
        let core = Oracle::new(pin).tx_decode(&bytes).ok().map(|d| (d.txid, d.instance_hash));
        let ours = decode_tx(&mut &bytes[..]).ok().map(|d| {
            let tx = d.into_value();
            (*tx.txid().as_bytes(), *tx.instance_hash().as_bytes())
        });
        (core, ours)
    };
    let changed = |version: u8, at: usize| {
        let mut spec = asset_unlock_v2();
        if let Some(p) = spec.payload.as_mut() {
            p[0] = version;
            p[at] ^= 0xff;
        }
        spec
    };
    let fields = [
        ("index", 1usize, false),
        ("fee", 9, false),
        ("requestedHeight", 13, true),
        ("quorumHash", 17, true),
        ("quorumSig", 49, true),
        ("quorumSig end", 144, true),
    ];
    for pin in Pin::ALL {
        for version in [1u8, 2] {
            let mut base = asset_unlock_v2();
            if let Some(p) = base.payload.as_mut() {
                p[0] = version;
            }
            let (core_base, ours_base) = ids(&base, pin);
            t.check("stable txid", core_base.is_some() && core_base == ours_base, "base ids");
            let Some(b) = core_base else {
                continue;
            };
            for (name, at, signing_info) in fields {
                let (core, ours) = ids(&changed(version, at), pin);
                t.check("stable txid", core == ours, name);
                let Some(c) = core else {
                    continue;
                };
                // A v2+ re-sign keeps its txid; every other change, and every change to a v1
                // payload, moves it.
                let keeps_txid = signing_info && version >= 2;
                t.check("stable txid", (b.0 == c.0) == keeps_txid, name);
                t.check("stable txid", b.1 != c.1, name);
            }
        }
    }
    t.finish("stable txid");
}

/// A block assembled from separately decoded transactions gives the oracle's root (CS0 §5.1).
#[test]
fn assembled_block_root() {
    let mut t = Table::default();
    let coinbase = TxSpec::coinbase().bytes();
    let unlock = asset_unlock_v2().bytes();
    let txs = [&coinbase, &unlock]
        .map(|bytes| decode_tx(&mut &bytes[..]).ok().map(|d| Arc::new(d.into_value())));
    let [Some(cb), Some(au)] = txs else {
        panic!("the fixture transactions decode");
    };
    let header_bytes = header();
    let Ok(header) = decode_header(&mut &header_bytes[..]) else {
        panic!("the header decodes");
    };
    assert_ne!(au.txid(), au.instance_hash(), "the fixture asset unlock has a stable txid");
    let block = Block::new(header.into_value(), vec![cb, au]);
    let serialized = core_serialize(&block);
    for pin in Pin::ALL {
        let core =
            Oracle::new(pin).block_decode(&serialized).ok().map(|d| (d.merkle_root, d.mutated));
        let (root, mutated) = block.merkle();
        t.check(
            "assembled block root",
            core == Some((*root.as_bytes(), mutated)),
            "assembled block root",
        );
    }
    t.finish("assembled block root");
}
