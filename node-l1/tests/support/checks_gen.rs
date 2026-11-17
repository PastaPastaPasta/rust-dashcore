//! Structured inputs for the CS3 targets (`node_l1::check_inputs` formats), from the shared
//! byte generators: blocks with a coinbase first, a merkle root that usually matches and a
//! height push that usually does, positions near the activation heights, and header candidates
//! near the time and difficulty boundaries. Some are then mutated, so every rule is reached.
//!
//! The including test declares `generate` (`dash-consensus/tests/support/gen.rs`).

#![allow(dead_code, reason = "each includer uses a subset")]
#![allow(clippy::expect_used, reason = "generators build what they then parse")]

use dash_consensus::decode_tx_exact;
use dash_consensus::merkle::compute_merkle_root;
use node_l1::check_inputs::{HEADER_INPUT_LEN, Network, Position, height_push};

use crate::generate::{Rng, compact_size, mutate, tx_bytes};

/// A v4 CbTx payload (with the asset-unlock root), or some bytes.
fn cbtx_payload(rng: &mut Rng, height: u32) -> Vec<u8> {
    if rng.chance(3) {
        let len = rng.below(220) as usize;
        return rng.bytes(len);
    }
    let version: u16 = rng.pick(&[1, 2, 3, 4, 4, 4, 5]);
    let mut out = version.to_le_bytes().to_vec();
    out.extend(height.to_le_bytes());
    out.extend([0u8; 32]);
    if version >= 2 {
        out.extend([0u8; 32]);
    }
    if version >= 3 {
        out.push(0);
        out.extend([0u8; 96]);
        out.extend(0i64.to_le_bytes());
    }
    if version >= 4 {
        // Usually the root of no asset unlocks (zero).
        out.extend(if rng.chance(4) {
            rng.bytes(32)
        } else {
            vec![0; 32]
        });
    }
    out
}

/// A coinbase transaction for a block at `height`.
pub fn coinbase(rng: &mut Rng, height: u32) -> Vec<u8> {
    let special = rng.chance(2);
    let (version, tx_type): (i16, u16) = if special {
        (
            3,
            if rng.chance(8) {
                0
            } else {
                5
            },
        )
    } else {
        (rng.pick(&[1, 2]), 0)
    };
    let mut out = version.to_le_bytes().to_vec();
    out.extend(tx_type.to_le_bytes());
    compact_size(1, &mut out);
    out.extend([0u8; 32]);
    out.extend(u32::MAX.to_le_bytes());
    let mut script = if rng.chance(4) {
        Vec::new()
    } else {
        height_push(height)
    };
    let pad = rng.pick(&[0usize, 1, 2, 4, 20, 99, 100, 101]);
    script.extend(rng.bytes(pad.saturating_sub(script.len())));
    compact_size(script.len() as u64, &mut out);
    out.extend(&script);
    out.extend(rng.field_u32().to_le_bytes());
    let outputs = rng.below(3);
    compact_size(outputs, &mut out);
    for _ in 0..outputs {
        out.extend(rng.field_amount().to_le_bytes());
        let len = rng.below(30) as usize;
        let script = rng.bytes(len);
        compact_size(script.len() as u64, &mut out);
        out.extend(script);
    }
    out.extend(
        if rng.chance(4) {
            rng.field_u32()
        } else {
            0
        }
        .to_le_bytes(),
    );
    if version >= 3 && tx_type != 0 {
        let payload = cbtx_payload(rng, height);
        compact_size(payload.len() as u64, &mut out);
        out.extend(payload);
    }
    out
}

/// A block at `height`: usually a coinbase then up to three transactions, under a header whose
/// merkle root matches and whose bits are easy (regtest's limit) half the time.
pub fn block(rng: &mut Rng, height: u32) -> Vec<u8> {
    let mut txs = vec![coinbase(rng, height)];
    for _ in 0..rng.below(4) {
        txs.push(if rng.chance(6) {
            coinbase(rng, height)
        } else {
            tx_bytes(rng)
        });
    }
    match rng.below(10) {
        0 => txs.reverse(),
        1 => txs.push(txs.last().expect("a coinbase").clone()),
        2 => txs.clear(),
        _ => {}
    }
    let leaves: Option<Vec<[u8; 32]>> = txs
        .iter()
        .map(|tx| decode_tx_exact(tx).ok().map(|tx| *tx.value().txid().as_bytes()))
        .collect();
    let root = match leaves {
        Some(leaves) if !rng.chance(8) => compute_merkle_root(leaves).0.to_vec(),
        _ => rng.bytes(32),
    };
    let mut out = (rng.pick(&[1i32, 2, 3, 4, 0x2000_0000])).to_le_bytes().to_vec();
    out.extend(rng.bytes(32));
    out.extend(root);
    out.extend(rng.field_u32().to_le_bytes());
    let any_bits = rng.field_u32();
    out.extend(rng.pick(&[0x207f_ffffu32, 0x1e0f_fff0, 0x1d00_ffff, any_bits]).to_le_bytes());
    out.extend(rng.field_u32().to_le_bytes());
    compact_size(txs.len() as u64, &mut out);
    for tx in txs {
        out.extend(tx);
    }
    out
}

/// A `diff_check_block` input.
pub fn check_block_bytes(rng: &mut Rng) -> Vec<u8> {
    let mut out = vec![rng.next_u64() as u8];
    let height = rng.below(3_000_000) as u32;
    out.extend(block(rng, height));
    if rng.chance(8) {
        mutate(rng, out)
    } else {
        out
    }
}

/// A [`Position`] near one of its network's anchors.
fn position(rng: &mut Rng) -> Vec<u8> {
    let delta = if rng.chance(4) {
        rng.field_i16()
    } else {
        rng.below(7) as i16 - 3
    };
    Position::encode(
        Network::from_byte(rng.next_u64() as u8),
        rng.next_u64() as u8,
        delta,
        rng.next_u64(),
    )
}

/// The height of the block a contextual input checks.
fn checked_height(prefix: &[u8]) -> u32 {
    Position::parse(prefix).expect("a full position").tip + 1
}

/// A `diff_contextual_check_tx` input.
pub fn contextual_tx_bytes(rng: &mut Rng) -> Vec<u8> {
    let mut out = position(rng);
    out.push(rng.next_u64() as u8);
    let height = checked_height(&out);
    out.extend(if rng.chance(4) {
        coinbase(rng, height)
    } else {
        tx_bytes(rng)
    });
    if rng.chance(8) {
        mutate(rng, out)
    } else {
        out
    }
}

/// A `diff_contextual_check_block` input.
pub fn contextual_block_bytes(rng: &mut Rng) -> Vec<u8> {
    let mut out = position(rng);
    out.push(rng.next_u64() as u8);
    let height = checked_height(&out);
    let height = if rng.chance(8) {
        height + 1
    } else {
        height
    };
    out.extend(block(rng, height));
    if rng.chance(8) {
        mutate(rng, out)
    } else {
        out
    }
}

/// A `diff_contextual_check_block_header` input: mostly short windows (the long ones hash
/// thousands of headers), a time near the median time past or the future limit, and the
/// expected bits.
pub fn contextual_header_bytes(rng: &mut Rng) -> Vec<u8> {
    let mut out = position(rng);
    out.push(if rng.chance(16) {
        rng.next_u64() as u8
    } else {
        rng.below(6) as u8
    });
    out.push(rng.next_u64() as u8);
    let time_delta: i32 = match rng.below(4) {
        0 => rng.field_u32() as i32,
        1 => rng.below(41) as i32 - 20,
        _ => rng.below(1200) as i32 - 300,
    };
    out.extend(time_delta.to_le_bytes());
    out.push(if rng.chance(4) {
        rng.next_u64() as u8
    } else {
        0
    });
    out.extend(
        if rng.chance(2) {
            rng.below(4) as u32
        } else {
            rng.field_u32()
        }
        .to_le_bytes(),
    );
    let clock: i16 = if rng.chance(2) {
        rng.below(5) as i16 - 2
    } else {
        rng.field_i16()
    };
    out.extend(clock.to_le_bytes());
    out.push(rng.next_u64() as u8);
    out.extend(rng.field_u32().to_le_bytes());
    debug_assert_eq!(out.len(), HEADER_INPUT_LEN);
    out
}
