//! The O1c targets (`diff_addrman`, `diff_p2p_multi_*`) agree on scripted inputs and treat
//! inputs with no script as out of domain.

use dash_oracle_sys::Pin;
use node_l1::{Outcome, TARGETS};

const NAMES: [&str; 4] =
    ["diff_addrman", "diff_p2p_multi_handshake", "diff_p2p_multi_headers", "diff_p2p_multi_relay"];

/// Deterministic pseudo-random scripts (splitmix64).
fn inputs() -> impl Iterator<Item = Vec<u8>> {
    (0u64..6).map(|seed| {
        let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        (0..64 + 64 * seed as usize)
            .map(|_| {
                x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
                let mut z = x;
                z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
                (z ^ (z >> 31)) as u8
            })
            .collect()
    })
}

#[test]
fn o1c_targets_replay_core() {
    for pin in Pin::ALL {
        for name in NAMES {
            let target = TARGETS.iter().find(|t| t.name == name).expect("registered");
            assert_eq!((target.compare)(&[], pin), Ok(Outcome::OutOfDomain), "{name}");
            let mut accepted = 0;
            for input in inputs() {
                match (target.compare)(&input, pin) {
                    Ok(Outcome::BothAccept) => accepted += 1,
                    Ok(Outcome::OutOfDomain) => {}
                    other => panic!("{name}: {other:?}"),
                }
            }
            assert!(accepted >= 4, "{name}: {accepted} scripts ran");
        }
    }
}
