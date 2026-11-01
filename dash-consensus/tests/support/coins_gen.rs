//! Byte-level generators for coins-database and undo-record wire data, shared by the
//! `dash-consensus` coin properties and the `node-l1` twins (CS5a). Include it next to `gen.rs`.
//!
//! The bytes are laid out field by field from Core's formats, not through the codec under test,
//! so they also cover forms the encoder never writes: a special script stored raw, a non-zero
//! legacy version, an amount no `CompressAmount` produces, a script length past
//! `MAX_SCRIPT_SIZE`, an x coordinate off the curve.

#![allow(dead_code)]

use super::generate::{Rng, compact_size};

/// Core's `VARINT`.
pub fn varint(mut n: u64, out: &mut Vec<u8>) {
    let mut tmp = [0u8; 10];
    let mut len = 0;
    loop {
        tmp[len] = (n & 0x7f) as u8
            | if len > 0 {
                0x80
            } else {
                0
            };
        if n <= 0x7f {
            break;
        }
        n = (n >> 7) - 1;
        len += 1;
    }
    out.extend(tmp[..=len].iter().rev());
}

/// x coordinates of curve points (the generator's and 2G's), so codes 4 and 5 decompress more
/// often than a random x would (about half of all x are on the curve).
const ON_CURVE_X: [[u8; 32]; 2] = [
    [
        0x79, 0xbe, 0x66, 0x7e, 0xf9, 0xdc, 0xbb, 0xac, 0x55, 0xa0, 0x62, 0x95, 0xce, 0x87, 0x0b,
        0x07, 0x02, 0x9b, 0xfc, 0xdb, 0x2d, 0xce, 0x28, 0xd9, 0x59, 0xf2, 0x81, 0x5b, 0x16, 0xf8,
        0x17, 0x98,
    ],
    [
        0xc6, 0x04, 0x7f, 0x94, 0x41, 0xed, 0x7d, 0x6d, 0x30, 0x45, 0x40, 0x6e, 0x95, 0xc0, 0x7c,
        0xd8, 0x5c, 0x77, 0x8e, 0x4b, 0x8c, 0xef, 0x3c, 0xa7, 0xab, 0xac, 0x09, 0xb9, 0x5c, 0x70,
        0x9e, 0xe5,
    ],
];

/// A raw script of one of the shapes `ScriptCompression` treats specially, or near them.
pub fn raw_script(rng: &mut Rng) -> Vec<u8> {
    let hash = rng.bytes(20);
    let mut s = match rng.below(8) {
        0 => [&[0x76, 0xa9, 20][..], &hash, &[0x88, 0xac]].concat(),
        1 => [&[0xa9, 20][..], &hash, &[0x87]].concat(),
        2 => {
            let mut s = vec![33, rng.pick(&[0x02, 0x03])];
            s.extend(rng.bytes(32));
            s.push(0xac);
            s
        }
        3 => {
            let mut s = vec![65, 0x04];
            s.extend(rng.pick(&ON_CURVE_X));
            s.extend(rng.bytes(32));
            s.push(0xac);
            s
        }
        4 => vec![0x6a; rng.pick(&[0, 1, 9_999, 10_000, 10_001])],
        _ => {
            let len = rng.pick(&[0, 1, 22, 23, 24, 25, 26, 34, 35, 36, 66, 67, 68, 200]);
            rng.bytes(len)
        }
    };
    // Sometimes break the shape by one byte.
    if !s.is_empty() && rng.chance(4) {
        let i = rng.below(s.len() as u64) as usize;
        s[i] ^= 1 << rng.below(8);
    }
    s
}

/// `ScriptCompression` wire data: a special code with its bytes, or a raw length and script.
pub fn compressed_script(rng: &mut Rng, out: &mut Vec<u8>) {
    match rng.below(4) {
        0 => {
            let code = rng.below(6);
            varint(code, out);
            let len = if code < 2 {
                20
            } else {
                32
            };
            if code >= 4 && rng.chance(2) {
                out.extend(rng.pick(&ON_CURVE_X));
            } else {
                out.extend(rng.bytes(len));
            }
        }
        1 if rng.chance(8) => {
            // A raw length past MAX_SCRIPT_SIZE (or past u32), with or without its bytes.
            let len = rng.pick(&[
                10_001u64,
                10_002,
                70_000,
                u64::from(u32::MAX) - 6,
                u64::from(u32::MAX),
            ]);
            varint(len + 6, out);
            let present = rng.below(len.min(20_000) + 1) as usize;
            out.extend(vec![0x51; present]);
        }
        _ => {
            let script = raw_script(rng);
            varint(script.len() as u64 + 6, out);
            out.extend(script);
        }
    }
}

/// A compressed amount: what `CompressAmount` gives for in-range amounts, or any `u64`.
fn compressed_amount(rng: &mut Rng) -> u64 {
    match rng.below(3) {
        0 => rng.next_u64(),
        1 => rng.pick(&[0, 1, 7, 9, 0x32, 0x1406f40, u64::MAX, u64::MAX - 1, 1 << 63]),
        _ => rng.below(0x1406f41),
    }
}

/// The `nHeight * 2 + fCoinBase` code, usually a plausible height.
fn code(rng: &mut Rng) -> u64 {
    match rng.below(8) {
        0 => u64::from(rng.field_u32()),
        1 => rng.next_u64(),
        2 => rng.below(2),
        _ => rng.below(5_000_000),
    }
}

/// `Coin::Serialize` wire data.
pub fn coin_bytes(rng: &mut Rng) -> Vec<u8> {
    let mut out = Vec::new();
    varint(code(rng), &mut out);
    varint(compressed_amount(rng), &mut out);
    compressed_script(rng, &mut out);
    out
}

/// A `TxInUndoFormatter` entry: as a coin, plus the legacy version after a non-zero height.
fn undo_coin(rng: &mut Rng, out: &mut Vec<u8>) {
    let code = code(rng);
    varint(code, out);
    if code >> 1 != 0 {
        let version = match rng.below(8) {
            0 => rng.next_u64(),
            1 => rng.below(4),
            _ => 0,
        };
        varint(version, out);
    }
    varint(compressed_amount(rng), out);
    compressed_script(rng, out);
}

fn count(rng: &mut Rng) -> u64 {
    match rng.below(16) {
        0 => rng.pick(&[253, 0xffff, 0x0200_0000, 0x0200_0001, u64::MAX]),
        1..=3 => 0,
        _ => rng.below(5),
    }
}

/// `CTxUndo` wire data.
pub fn tx_undo_bytes(rng: &mut Rng) -> Vec<u8> {
    let mut out = Vec::new();
    tx_undo(rng, &mut out);
    out
}

fn tx_undo(rng: &mut Rng, out: &mut Vec<u8>) {
    let n = count(rng);
    compact_size(n, out);
    for _ in 0..n.min(6) {
        undo_coin(rng, out);
    }
}

/// `CBlockUndo` wire data.
pub fn block_undo_bytes(rng: &mut Rng) -> Vec<u8> {
    let mut out = Vec::new();
    let n = count(rng);
    compact_size(n, &mut out);
    for _ in 0..n.min(6) {
        tx_undo(rng, &mut out);
    }
    out
}

/// A `u64` operand: half uniform, half from the boundaries of the integer codecs.
fn operand(rng: &mut Rng) -> u64 {
    if rng.chance(2) {
        return rng.next_u64() >> rng.below(64);
    }
    rng.pick(&[
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
        0x0200_0000,
        0x0200_0001,
        0xffff_ffff,
        0x1_0000_0000,
        21_000_000 * 100_000_000,
        u64::MAX,
    ])
}

/// `diff_codec_primitives` input: an operation selector, then that operation's input
/// (`node_l1::coins::compare_codec_primitives`).
pub fn codec_primitives_bytes(rng: &mut Rng) -> Vec<u8> {
    let op = rng.below(12) as u8;
    let mut out = vec![op];
    match op {
        0 => compact_size(operand(rng), &mut out),
        2 => varint(operand(rng), &mut out),
        1 | 3 | 4 | 5 => out.extend(operand(rng).to_le_bytes()),
        6 => out.extend(raw_script(rng)),
        7 => compressed_script(rng, &mut out),
        8 => {
            out.extend(rng.field_u32().to_le_bytes());
            out.push(rng.below(2) as u8);
            out.extend(rng.field_amount().to_le_bytes());
            out.extend(raw_script(rng));
        }
        9 => out.extend(coin_bytes(rng)),
        10 => out.extend(tx_undo_bytes(rng)),
        _ => out.extend(block_undo_bytes(rng)),
    }
    out
}
