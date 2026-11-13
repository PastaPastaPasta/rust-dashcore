//! L1 for EV2b's payee selection: `mnlist_payee_queue` (`payee`, `projected_payees`) against
//! Core's `GetMNPayee` and `GetProjectedMNPayees`, on random operation sequences and on EV2 §6.2's
//! fixtures written as inputs.

use std::collections::BTreeMap;

use dash_oracle_sys::Pin;
use node_l1::Outcome;
use node_l1::dml_payee::{MAX_OPS, compare_payee_queue, fixtures};

fn tally(outcomes: &mut BTreeMap<String, usize>, outcome: Outcome) {
    *outcomes.entry(format!("{outcome:?}")).or_default() += 1;
}

/// splitmix64.
fn next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[test]
fn random_lists_match_the_oracle() {
    let mut state = 0x0e2b_0002;
    let mut outcomes = BTreeMap::new();
    for round in 0..3000 {
        let ops = (next(&mut state) % (MAX_OPS as u64 + 1)) as usize;
        let input: Vec<u8> = (0..3 + 6 * ops).map(|_| next(&mut state) as u8).collect();
        let outcome = compare_payee_queue(&input, Pin::V24_0)
            .unwrap_or_else(|d| panic!("round {round} {input:02x?}: {d}"));
        tally(&mut outcomes, outcome);
    }
    eprintln!("{outcomes:?}");
    assert!(outcomes.get("BothAccept").is_some_and(|&n| n > 1000), "{outcomes:?}");
}

/// EV2 §6.2's payee fixtures (`dml_payee::fixtures`) are compared whole.
#[test]
fn design_fixtures_match_the_oracle() {
    for (name, input) in fixtures() {
        let outcome =
            compare_payee_queue(&input, Pin::V24_0).unwrap_or_else(|d| panic!("{name}: {d}"));
        assert_eq!(format!("{outcome:?}"), "BothAccept", "{name}");
    }
}

/// `mnlist_payee_queue`'s public seeds (`node-fuzz/public-seeds/mnlist_payee_queue`) agree.
#[test]
fn public_seeds_match_the_oracle() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../node-fuzz/public-seeds/mnlist_payee_queue");
    let mut outcomes = BTreeMap::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let input = std::fs::read(&path).unwrap();
        let outcome = compare_payee_queue(&input, Pin::V24_0)
            .unwrap_or_else(|d| panic!("{} diverges: {d}", path.display()));
        tally(&mut outcomes, outcome);
    }
    eprintln!("{outcomes:?}");
    assert!(outcomes.get("BothAccept").is_some_and(|&n| n >= 20), "{outcomes:?}");
}
