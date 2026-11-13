//! SB4 A-8's oracle half (designs/SB4-dash-chain.md §15.1, Opus O-m6): Core's
//! `GetNextWorkRequired` over A-8's synthetic KGW chain equals the target recorded in
//! `dash-chain/tests/data/kgw-max-walk.json`, which dash-chain's `kgw_max_walk_{resident,cold}`
//! require of `complete()` and of the `RuleContext` at w = 10,000 and w = 1,000.
//!
//! The chain: mainnet, parent at 19,999, header `h` at time 1,000,000 + 150·h with bits
//! 0x1e010000. At exact spacing KGW never crosses its event horizon, so it reads its 4,032
//! contributing entries down to 15,968 and steps to 15,967 (`pprev`, never its header fields):
//! the 4,033 headers dash-chain's context holds.
//!
//! What the oracle shows: Core's target over the full window is the recorded one; it is the same
//! without 15,967 and with 15,967's time and bits changed (Core does not read its fields), and
//! different with 15,968's bits changed (15,968 contributes; the oracle refuses fewer than KGW's
//! 4,032 headers, so it cannot be left out instead). The step to 15,967 itself is not observable
//! here; dash-chain's `kgw_max_walk_{resident,cold}` assert that its rule context spans down to
//! 15,967 and that the walk reads nothing outside it.

mod common;

use common::header;
use dash_oracle_sys::{Chain, Oracle, Pin};
use serde_json::Value;

#[test]
fn kgw_max_walk_matches_the_recorded_target() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../dash-chain/tests/data/kgw-max-walk.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(v["network"], "main");
    let n = |k: &str| u32::try_from(v[k].as_u64().unwrap()).unwrap();
    let (first, parent) = (n("first_height"), n("parent_height"));
    let (base, spacing, bits) = (n("time_base"), n("spacing"), n("bits"));
    assert_eq!(parent - first + 1, 4_033, "KGW's longest walk and its guard read");
    let window: Vec<u8> = (first..=parent).flat_map(|h| header(base + spacing * h, bits)).collect();
    let next = header(base + spacing * (parent + 1), 0);
    let oracle = Oracle::new(Pin::V24_0);
    let core = |headers: &[u8], first: u32| {
        oracle.next_work_required(&Chain::main(), headers, first as i32, &next).unwrap()
    };
    assert_eq!(
        core(&window, first),
        n("next_bits"),
        "Core's GetNextWorkRequired at {}",
        parent + 1
    );
    // 15,967's fields are not read: without it, or with its time and bits changed, Core's
    // answer is the same.
    assert_eq!(core(&window[80..], first + 1), n("next_bits"), "without 15,967");
    let mut changed = window.clone();
    changed[..80].copy_from_slice(&header(base, 0x1e0f_ffff));
    assert_eq!(core(&changed, first), n("next_bits"), "15,967's fields are not read");
    // 15,968 contributes: with its bits changed the answer is not the same.
    let mut changed = window[80..].to_vec();
    changed[..80].copy_from_slice(&header(base + spacing * (first + 1), 0x1e0f_ffff));
    assert_ne!(core(&changed, first + 1), n("next_bits"), "15,968 contributes");
}
