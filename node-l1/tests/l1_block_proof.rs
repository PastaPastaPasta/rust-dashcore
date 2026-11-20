//! `dash_consensus::pow::block_proof` against Core's `GetBlockProof` (SB4 C1a, §15.3), live on
//! both pins: an exponent × mantissa sweep with the sign and overflow edges, every mainnet and
//! testnet era's typical `nBits`, and a deterministic pseudo-random sample. Core's answers come
//! from the oracle at test time and are never recorded.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::dbg_macro, clippy::print_stdout)]

use dash_consensus::pow::block_proof;
use dash_num::{Arith256, CompactTarget};
use dash_oracle_sys::{Oracle, Pin};
use dash_types::Numeric as _;

/// SplitMix64, for a reproducible sample.
fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

fn cases() -> Vec<u32> {
    let mut bits: Vec<u32> = (0u32..=0x24)
        .flat_map(|e| {
            [
                0, 0x01, 0xff, 0x0100, 0x8000, 0xffff, 0x01_0000, 0x7f_ffff, 0x80_0000, 0x80_0001,
                0xff_ffff,
            ]
            .map(move |m| (e << 24) | m)
        })
        .collect();
    // Mainnet genesis and powLimit, regtest powLimit, and mid-range targets.
    bits.extend([
        0x1e0f_fff0,
        0x1e0f_ffff,
        0x1b04_04cb,
        0x1c0f_d70a,
        0x1924_6e1c,
        0x1e01_0000,
        0x207f_ffff,
    ]);
    let mut state = 0x5b4_c1a;
    bits.extend((0..4096).map(|_| splitmix(&mut state) as u32));
    bits
}

#[test]
fn block_proof_matches_core() {
    let mut failures = Vec::new();
    for pin in Pin::ALL {
        let oracle = Oracle::new(pin);
        for bits in cases() {
            let core = match oracle.block_proof(bits) {
                Ok(proof) => Arith256::from_lendian(proof),
                Err(e) => {
                    failures.push(format!("{bits:#010x} ({}): oracle error {e}", pin.name()));
                    continue;
                }
            };
            if block_proof(CompactTarget::new(bits)) != core {
                failures.push(format!("{bits:#010x} ({})", pin.name()));
            }
        }
    }
    assert!(failures.is_empty(), "GetBlockProof differs: {failures:?}");
}
