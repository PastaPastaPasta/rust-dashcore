//! `diff_bls_arith` (BS11a-3) against Dash Core v24's in-process oracle: the node agrees with
//! Core on the target's public seeds (written by the `bls_arith_seeds` example from base-sdk's
//! Core-judged step scripts) and on random scripts.

use dash_oracle_sys::Pin;
use node_l1::bls_arith::{compare_bls_arith, read_script};
use node_l1::{Outcome, target};

const PIN: Pin = Pin::V24_0;

/// Splitmix64 bytes from `seed`.
fn bytes(seed: u64, n: usize) -> Vec<u8> {
    let mut state = seed;
    (0..n.div_ceil(8))
        .flat_map(|_| {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            (z ^ (z >> 31)).to_le_bytes()
        })
        .take(n)
        .collect()
}

#[test]
fn registered() {
    assert!(target("diff_bls_arith").is_some());
}

/// The node agrees with Core on every public seed (`node-fuzz/public-seeds/diff_bls_arith`).
#[test]
fn public_seeds_agree() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../node-fuzz/public-seeds/diff_bls_arith");
    let mut n = 0;
    for entry in std::fs::read_dir(dir).expect("the public seeds") {
        let path = entry.expect("an entry").path();
        let input = std::fs::read(&path).expect("a seed");
        assert!(!read_script(&input).code.is_empty(), "{}", path.display());
        assert_eq!(compare_bls_arith(&input, PIN), Ok(Outcome::BothAccept), "{}", path.display());
        n += 1;
    }
    assert!(n >= 60, "{n} seeds");
}

/// Random scripts: the first op a load, so that later ops find registers.
#[test]
fn random_scripts_agree() {
    for seed in 0..400 {
        let mut input = bytes(seed, 900);
        input[0] &= 0xf0 | (seed % 3) as u8;
        let outcome = compare_bls_arith(&input, PIN);
        assert!(
            matches!(outcome, Ok(Outcome::BothAccept | Outcome::OutOfDomain)),
            "seed {seed}: {outcome:?}"
        );
    }
}
