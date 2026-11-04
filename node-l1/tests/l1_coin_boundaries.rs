//! The coin and undo codecs' boundary table (CS5a), compared live against Dash Core's oracle on
//! every linked pin.
//!
//! Each row's input is built here, deterministically, and passed to the shared `node-l1`
//! comparisons; Core's answers come from the oracle at test time and are never recorded
//! (CS0 §9.1). A failure names the area, the row's index, the field and the pin; when
//! `DASH_EMBARGO_DIR` is set the input goes to the private sink (CS0 §9.5).

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::use_debug,
    clippy::dbg_macro,
    clippy::print_stdout
)]

use std::collections::BTreeMap;

use dash_consensus::coins::compress::{MAX_SCRIPT_SIZE, special_script_size};
use dash_consensus::coins::{BlockUndo, Coin, TxUndo};
use dash_consensus::primitives::{write_compact_size, write_varint};
use dash_core_vectors::hex::parse_hex;
use dash_oracle_sys::Pin;
use node_l1::coins::{
    compare_amount_compress, compare_amount_decompress, compare_coin_encode,
    compare_compact_size_decode, compare_compact_size_encode, compare_script_compress,
    compare_script_decompress, compare_tx_undo, compare_varint_decode, compare_varint_encode,
};
use node_l1::{Compared, Outcome, compare_block_undo, compare_coin_decode};
use node_sink::sink::record_for_test;

const COIN: u64 = 100_000_000;
const MAX_MONEY: u64 = 21_000_000 * COIN;
const MAX_SIZE: u64 = dash_consensus::primitives::MAX_SIZE as u64;

/// The rows' results by area: failures are named, and each area must get the outcomes it
/// expects.
#[derive(Default)]
struct Table {
    rows: usize,
    /// Per area: rows, and rows every pin accepted.
    areas: BTreeMap<String, (usize, usize)>,
    failures: Vec<String>,
}

impl Table {
    fn row(&mut self, area: &str, bytes: &[u8], compare: impl Fn(Pin) -> Compared) {
        let index = self.rows;
        self.rows += 1;
        let mut accepted = true;
        for pin in Pin::ALL {
            match compare(pin) {
                Ok(Outcome::BothAccept) => {}
                Ok(
                    Outcome::BothReject
                    | Outcome::OutOfDomain
                    | Outcome::StrictFlag
                    | Outcome::Discarded,
                ) => accepted = false,
                Err(divergence) => {
                    accepted = false;
                    let _ =
                        record_for_test("l1_coin_boundaries", bytes, pin.name(), divergence.field);
                    self.failures.push(format!("{area} #{index}: {divergence} ({})", pin.name()));
                }
            }
        }
        let counts = self.areas.entry(area.to_owned()).or_default();
        counts.0 += 1;
        counts.1 += usize::from(accepted);
    }

    fn bytes(&mut self, area: &str, compare: fn(&[u8], Pin) -> Compared, bytes: &[u8]) {
        self.row(area, bytes, |pin| compare(bytes, pin));
    }

    fn number(&mut self, area: &str, compare: fn(u64, Pin) -> Compared, n: u64) {
        self.row(area, &n.to_le_bytes(), |pin| compare(n, pin));
    }

    /// Fails on any divergence, on an area in `reject_only` with an accepted row, and on any
    /// other area without one: a builder bug that made a whole area undecodable would otherwise
    /// pass as agreement.
    fn finish(self, table: &str, reject_only: &[&str]) {
        let accepted: usize = self.areas.values().map(|(_, a)| a).sum();
        node_sink::report(&format!(
            "{table}: {} rows, {accepted} accepted on every linked pin, {} failures",
            self.rows,
            self.failures.len()
        ));
        assert!(self.failures.is_empty(), "{table}: {}", self.failures.join("; "));
        for (area, &(_, accepted)) in &self.areas {
            let expect_accept = !reject_only.contains(&area.as_str());
            assert!(
                (accepted > 0) == expect_accept,
                "{table}: area {area} has {accepted} accepted rows"
            );
        }
        for area in reject_only {
            assert!(self.areas.contains_key(*area), "{table}: no row in area {area}");
        }
    }
}

fn varint(n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    write_varint(n, &mut out);
    out
}

fn compact(n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    write_compact_size(n, &mut out);
    out
}

/// Values at the edges of each integer codec's length classes and checks.
const NUMBERS: &[u64] = &[
    0,
    1,
    0x7f,
    0x80,
    252,
    253,
    0x407f,
    0x4080,
    0xffff,
    0x1_0000,
    0x20_407f,
    0x20_4080,
    MAX_SIZE,
    MAX_SIZE + 1,
    0xffff_ffff,
    0x1_0000_0000,
    MAX_MONEY,
    MAX_MONEY + 1,
    i64::MAX as u64,
    1 << 63,
    u64::MAX - 1,
    u64::MAX,
];

#[test]
fn integer_codecs() {
    let mut t = Table::default();
    for &n in NUMBERS {
        t.number("compact size encode", compare_compact_size_encode, n);
        t.bytes("compact size decode", compare_compact_size_decode, &compact(n));
        t.number("varint encode", compare_varint_encode, n);
        t.bytes("varint decode", compare_varint_decode, &varint(n));
        // Every proper prefix is truncated.
        for cut in 0..varint(n).len() {
            t.bytes("varint prefix", compare_varint_decode, &varint(n)[..cut]);
        }
    }
    // Non-canonical compact sizes, one per prefix form.
    for bytes in [
        &[253u8, 252, 0][..],
        &[253, 0, 0],
        &[254, 0xff, 0xff, 0, 0],
        &[255, 0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0],
    ] {
        t.bytes("compact size non-canonical", compare_compact_size_decode, bytes);
    }
    // VARINT overflow: one more digit after the largest u64, and ten 0xff digits.
    let mut over = varint(u64::MAX);
    if let Some(last) = over.last_mut() {
        *last |= 0x80;
    }
    over.push(0);
    t.bytes("varint overflow", compare_varint_decode, &over);
    t.bytes("varint overflow", compare_varint_decode, &[0xff; 10]);
    t.bytes("varint overflow", compare_varint_decode, &[0x80; 11]);
    t.finish("integer codecs", &["varint prefix", "compact size non-canonical", "varint overflow"]);
}

#[test]
fn amounts() {
    let mut t = Table::default();
    let mut values: Vec<u64> = NUMBERS.to_vec();
    // Core's compress_amounts fixed points and every power of ten.
    values.extend([1_000_000, COIN, 50 * COIN, 60_000_000_000, 110_397]);
    values.extend((0..20).map(|e| 10u64.pow(e)));
    values.extend((0..20).map(|e| 7 * 10u64.pow(e.min(18))));
    // -1 and i64::MIN as Core's uint64_t conversion sees them.
    values.extend([(-1i64) as u64, i64::MIN as u64]);
    for &n in &values {
        t.number("compress amount", compare_amount_compress, n);
        t.number("decompress amount", compare_amount_decompress, n);
    }
    // Small codes cover every exponent and digit.
    for n in 0..200 {
        t.number("decompress amount", compare_amount_decompress, n);
    }
    t.finish("amounts", &[]);
}

/// The secp256k1 generator's coordinates.
const GX: &str = "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
const GY: &str = "483ada7726a3c4655da4fbfc0e1108a8fd17b448a68554199c47d08ffb10d4b8";

/// Raw scripts at and around each special form.
fn scripts() -> Vec<Vec<u8>> {
    let hash = [0x5au8; 20];
    let p2pkh = [&[0x76, 0xa9, 20][..], &hash, &[0x88, 0xac]].concat();
    let p2sh = [&[0xa9, 20][..], &hash, &[0x87]].concat();
    let p2pk = |prefix: u8, x: &[u8]| [&[33, prefix][..], x, &[0xac]].concat();
    let p2pk_u = |prefix: u8, x: &[u8], y: &[u8]| [&[65, prefix][..], x, y, &[0xac]].concat();
    let (gx, gy) = (parse_hex(GX), parse_hex(GY));
    let mut off_curve = vec![0u8; 32];
    off_curve[31] = 5;
    let mut out = vec![
        vec![],
        vec![0x51],
        p2pkh.clone(),
        p2sh.clone(),
        p2pk(0x02, &gx),
        p2pk(0x03, &gx),
        // Compressed keys are taken without a check.
        p2pk(0x02, &off_curve),
        p2pk_u(0x04, &gx, &gy),
        // An odd y: the generator's negation.
        p2pk_u(
            0x04,
            &gx,
            &parse_hex("b7c52588d95c3b9aa25b0403f1eef75702e84bb7597aabe663b82f6f04ef2777"),
        ),
        // Off the curve, and the hybrid headers, stay raw.
        p2pk_u(0x04, &gx, &gx),
        p2pk_u(0x06, &gx, &gy),
        p2pk_u(0x07, &gx, &gy),
        p2pk(0x04, &gx),
        vec![0x6a; MAX_SCRIPT_SIZE],
        vec![0x51; MAX_SCRIPT_SIZE + 1],
    ];
    // One-byte breaks and length changes of each special form.
    for special in [p2pkh, p2sh, p2pk(0x02, &gx), p2pk_u(0x04, &gx, &gy)] {
        for i in 0..special.len() {
            let mut broken = special.clone();
            broken[i] ^= 0x01;
            out.push(broken);
        }
        out.push(special[..special.len() - 1].to_vec());
        out.push([&special[..], &[0xac]].concat());
    }
    out
}

/// `ScriptCompression` wire data at and around each code.
fn compressed_scripts() -> Vec<Vec<u8>> {
    let gx = parse_hex(GX);
    let mut off_curve = vec![0u8; 32];
    off_curve[31] = 5;
    let mut out = Vec::new();
    for code in 0..6u8 {
        let len = special_script_size(u32::from(code));
        out.push([&[code][..], &[0x33; 32][..len]].concat());
        out.push([&[code][..], &[0x33; 32][..len - 1]].concat());
    }
    for code in [4u8, 5] {
        out.push([&[code][..], &gx].concat());
        out.push([&[code][..], &off_curve].concat());
    }
    for (size, present) in [
        (0, 0),
        (1, 1),
        (1, 0),
        (25, 25),
        (MAX_SCRIPT_SIZE, MAX_SCRIPT_SIZE),
        (MAX_SCRIPT_SIZE + 1, MAX_SCRIPT_SIZE + 1),
        (MAX_SCRIPT_SIZE + 1, MAX_SCRIPT_SIZE),
        (3_000_000_000, 64),
        (u32::MAX as usize - 6, 0),
    ] {
        let mut bytes = varint(size as u64 + 6);
        bytes.extend(vec![0x6a; present]);
        out.push(bytes);
    }
    // A raw size whose code overflows `unsigned int`.
    out.push(varint(u64::from(u32::MAX) + 1));
    out
}

#[test]
fn scripts_and_coins() {
    let mut t = Table::default();
    for script in scripts() {
        t.bytes("compress script", compare_script_compress, &script);
        for (height, coinbase, value) in [
            (0, false, 0),
            (1, true, 50 * COIN as i64),
            (203_998, false, MAX_MONEY as i64),
            ((1 << 31) - 1, true, 1),
            (1 << 31, false, -2),
            (u32::MAX, true, i64::MIN),
            (7, false, i64::MAX),
        ] {
            t.row("coin encode", &script, |pin| {
                compare_coin_encode(height, coinbase, value, &script, pin)
            });
        }
    }
    for bytes in compressed_scripts() {
        t.bytes("decompress script", compare_script_decompress, &bytes);
        // The same script behind a coin's code and amount.
        let coin = [&varint(2 * 1000 + 1)[..], &varint(9), &bytes].concat();
        t.bytes("coin decode", compare_coin_decode, &coin);
    }
    // Core's ccoins_serialization inputs (src/test/coins_tests.cpp).
    for hex in [
        "97f23c835800816115944e077fe7c803cfa57f29b36bf87c1d35",
        "8ddf77bbd123008c988f1a4a4de2161e0f50aac7f17e7f9555caa4",
        "000006",
        "000007",
        "00008a95c0bb00",
    ] {
        t.bytes("coin decode (Core's)", compare_coin_decode, &parse_hex(hex));
    }
    // Codes at the 31-bit height boundary and past u32.
    for code in [0, 1, 2, 3, u64::from(u32::MAX) - 1, u64::from(u32::MAX), u64::from(u32::MAX) + 1]
    {
        let coin = [&varint(code)[..], &varint(9), &[0x06][..]].concat();
        t.bytes("coin code", compare_coin_decode, &coin);
    }
    t.finish("scripts and coins", &[]);
}

/// One legacy-format undo entry: a non-zero height with `version` after the code.
fn legacy_entry(height: u32, version: u64) -> Vec<u8> {
    [&varint(u64::from(height) * 2)[..], &varint(version), &varint(9), &[0x07, 0x51]].concat()
}

#[test]
fn undo_records() {
    let mut t = Table::default();
    let p2sh = [&[0xa9, 20][..], &[1u8; 20], &[0x87]].concat();
    let coins = [
        Coin::new(5_000, &p2sh, 10, false),
        Coin::new(7, &[0x51], 11, true),
        // Height 0: an entry of a release before 0.15's format (no version byte).
        Coin::new(1, &[], 0, false),
        Coin::new(MAX_MONEY as i64, &[0x6a; 300], (1 << 31) - 1, true),
    ];
    let mut records = vec![
        BlockUndo::default(),
        BlockUndo {
            txs: vec![TxUndo::default()],
        },
        BlockUndo {
            txs: vec![
                TxUndo {
                    coins: coins.to_vec(),
                },
                TxUndo::default(),
                TxUndo {
                    coins: coins[..2].to_vec(),
                },
            ],
        },
    ];
    // Enough entries for multi-byte counts.
    records.push(BlockUndo {
        txs: vec![
            TxUndo {
                coins: vec![coins[1].clone(); 253],
            };
            2
        ],
    });
    for record in &records {
        let bytes = record.core_bytes();
        let bytes = bytes.as_slice();
        t.bytes("block undo", compare_block_undo, bytes);
        // With trailing bytes (the rev file's checksum), and truncated.
        t.bytes("block undo + suffix", compare_block_undo, &[bytes, &[0xee; 32]].concat());
        for cut in [1, 2, bytes.len() / 2, bytes.len().saturating_sub(1)] {
            t.bytes("block undo prefix", compare_block_undo, &bytes[..cut.min(bytes.len())]);
        }
    }
    for tx in &records[2].txs {
        let mut bytes = Vec::new();
        tx.write(&mut bytes);
        t.bytes("tx undo", compare_tx_undo, &bytes);
    }
    // Legacy versions: zero, non-zero, a long VARINT, an overflowing one.
    for version in [0, 1, 0x7f, 0x80, u64::from(u32::MAX), u64::from(u32::MAX) + 1] {
        let tx = [&[0x01][..], &legacy_entry(5, version)].concat();
        t.bytes("legacy version", compare_tx_undo, &tx);
        t.bytes("legacy version", compare_block_undo, &[&[0x01][..], &tx].concat());
    }
    // Counts at and past MAX_SIZE with nothing behind them.
    for n in [MAX_SIZE, MAX_SIZE + 1] {
        t.bytes("undo count", compare_block_undo, &compact(n));
        t.bytes("undo count", compare_tx_undo, &compact(n));
    }
    t.finish("undo records", &["undo count"]);
}
