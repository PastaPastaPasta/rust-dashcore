//! `diff_credit_pool` (EV3-2): dash-dml's credit pool against `oracle_credit_pool_step` on the
//! payments fixture's credit steps, mined and edited.

use dash_oracle_sys::{Pin, Verdict};
use node_l1::credit_pool::{
    Case, case_from_bytes, compare_case, core_step, seed_inputs, step_index, window_data,
};
use node_l1::{Outcome, target};

fn step(name: &str) -> usize {
    step_index(name).unwrap_or_else(|| panic!("credit step {name}"))
}

const COIN: i64 = 100_000_000;

/// Every mined step accepts, and the pool after each is `getcreditpoolinfo`'s.
#[test]
fn mined_steps_agree() {
    for name in ["v20-activation", "unlock", "testnet-v20-early"] {
        let case = Case::mined(step(name));
        assert_eq!(compare_case(&case), Ok(Outcome::BothAccept), "{name}");
        let core = core_step(&case).unwrap().unwrap().unwrap();
        let pool = core.pool.expect("an accepted step");
        let s = case.step();
        assert_eq!((pool.locked, pool.current_limit, pool.lately_unlocked), s.expected, "{name}");
    }
}

/// A crafted input: the edits `ops` on `name` with `flags`.
fn input(name: &str, flags: u8, ops: &[[u8; 3]]) -> Vec<u8> {
    let mut bytes = vec![step(name) as u8, flags];
    ops.iter().for_each(|op| bytes.extend(op));
    bytes
}

fn compared(bytes: &[u8]) -> Outcome {
    let case = case_from_bytes(bytes).expect("an input");
    compare_case(&case).unwrap_or_else(|d| panic!("{bytes:?}: diverges at {}", d.field))
}

/// Each of Core's rejects and exceptions from the unlock step.
#[test]
fn rejects_and_exceptions() {
    for (ops, flags) in [
        // bad-cbtx-assetlocked-amount
        (vec![[10, 0, 0]], 0),
        // failed-creditpool-unlock-too-much
        (vec![[2, 0, 1]], 0),
        // failed-creditpool-unlock-duplicated-index, from the pool and within the block
        (vec![[4, 0, 0]], 0),
        (vec![[7, 0, 1]], 0),
        // A version-2 unlock (V24's stable txid): the text names the txid, not the instance hash.
        (vec![[12, 0, 3], [4, 0, 0]], 0),
        // failed-creditpool-unlock-payload
        (vec![[13, 0, 0]], 0),
        // failed-creditpool-lock-invalid (a crafted lock whose burn is gone)
        (vec![[15, 0, 0], [11, 0, 1]], 0),
        // The coinbase is no CbTx: no check, and the parent's balance makes Core throw.
        (vec![[14, 0, 0]], 0),
        // Another special type with the CbTx's bytes: Core reads the balance, then the pool
        // throws on a used index.
        (vec![[4, 0, 0], [14, 0, 1]], 0),
    ] {
        let bytes = input("unlock", flags, &ops);
        assert_eq!(compared(&bytes), Outcome::BothReject, "{ops:?}");
    }
}

/// Edits that keep the block valid, with the CbTx rebalanced: the pool after it agrees.
#[test]
fn rebalanced_edits_accept() {
    for (name, ops) in [
        ("unlock", vec![[15, 0x81, 9]]),
        ("unlock", vec![[6, 0, 0]]),
        ("unlock", vec![[12, 0, 0]]),
        ("unlock", vec![[12, 0, 3]]),
        // CONS-10: the two top indexes used, one wrapped range.
        ("unlock", vec![[5, 0, 1], [5, 0, 2]]),
        ("unlock", vec![[3, 0x81, 1]]),
        ("v20-activation", vec![[15, 0x81, 200]]),
    ] {
        let bytes = input(name, 2, &ops);
        assert_eq!(compared(&bytes), Outcome::BothAccept, "{name} {ops:?}");
        // V24's limit (Core ignores the state on mainnet).
        let bytes = input(name, 3, &ops);
        assert_eq!(compared(&bytes), Outcome::BothAccept, "{name} {ops:?} v24");
    }
}

/// Every public seed, through the registered target: no divergence, and each outcome occurs.
#[test]
fn every_seed_agrees() {
    let t = target("diff_credit_pool").expect("registered");
    let mut counts = [0usize; 3];
    for seed in seed_inputs() {
        match (t.compare)(&seed, Pin::V24_0) {
            Ok(Outcome::BothAccept) => counts[0] += 1,
            Ok(Outcome::BothReject) => counts[1] += 1,
            Ok(Outcome::OutOfDomain) => counts[2] += 1,
            Ok(other) => panic!("{seed:?}: {other:?}"),
            Err(d) => panic!("{seed:?}: diverges at {}", d.field),
        }
    }
    eprintln!("accept, reject, out of domain: {counts:?}");
    assert!(counts.iter().all(|&n| n > 0), "{counts:?}");
}

/// V24's 2000 DASH floor and its clamp to the balance, against Core (BL-260): soon after
/// testnet's V20 the window start holds little, so with V24 the limit is the whole balance, here
/// raised past 2000 DASH by nine 256 DASH locks. Without V24 the block is before WITHDRAWALS:
/// a tenth of the balance.
#[test]
fn v24_floor_and_clamp_soon_after_v20() {
    let s = step("testnet-v20-early");
    assert!(window_data(s).unwrap_or_default().balance < 10_000 * COIN);
    let limit = |flags, ops: &[[u8; 3]]| {
        let bytes = input("testnet-v20-early", flags, ops);
        assert_eq!(compared(&bytes), Outcome::BothAccept, "{flags} {ops:?}");
        let case = case_from_bytes(&bytes).expect("an input");
        let pool = core_step(&case).unwrap().unwrap().unwrap().pool.expect("an accepted step");
        (pool.locked, pool.current_limit)
    };
    let (small, small_limit) = limit(3, &[]);
    assert_eq!(small_limit, small);
    // Distinct locks (`a` picks the credit script), so no txid repeats.
    let locks: Vec<[u8; 3]> = (0..9).map(|i| [15, 0x80 | i, 255]).collect();
    let (big, big_limit) = limit(3, &locks);
    assert!(big > 2000 * COIN);
    assert_eq!(big_limit, big);
    assert_eq!(limit(2, &locks), (big, big / 10));
}

/// Core's reject reason for an input, which must agree with the node's.
fn core_reason(bytes: &[u8]) -> String {
    assert_eq!(compared(bytes), Outcome::BothReject, "{bytes:?}");
    let case = case_from_bytes(bytes).expect("an input");
    let Verdict::Reject(rej) = core_step(&case).unwrap().unwrap().unwrap().verdict else {
        panic!("{bytes:?}: Core accepts");
    };
    rej.reason
}

/// CE4-05: rc.4's checked sums (`creditpool.cpp` rc.4:46-50, 297-305, 316-320, 336-337), each
/// at the edge where the sum leaves `int64` and one step inside it.
#[test]
fn checked_sums_agree() {
    // The total: the parent's balance at `int64`'s bottom, less the block's unlocks; at its top,
    // plus a crafted lock (the early testnet block unlocks nothing).
    let reason = core_reason(&input("unlock", 0, &[[1, 0x40, 0]]));
    assert_eq!(reason, "failed-checkcreditpooldiff");
    let reason = core_reason(&input("testnet-v20-early", 0, &[[0, 0x40, 0], [15, 0, 0]]));
    assert_eq!(reason, "failed-checkcreditpooldiff");
    let reason = core_reason(&input("testnet-v20-early", 0, &[[0, 0x40, 1], [15, 0, 0]]));
    assert_eq!(reason, "bad-cbtx-assetlocked-amount");
    // The session's locks: 4393 of `MAX_MONEY`, against 4392.
    let reason = core_reason(&input("unlock", 0, &[[15, 0x40, 1]]));
    assert_eq!(reason, "failed-creditpool-lock-overflow");
    assert_ne!(core_reason(&input("unlock", 0, &[[15, 0x40, 0]])), reason);
    // The session's unlocks under the widest limit: an overflowing sum is too much.
    let reason = core_reason(&input("unlock", 0, &[[2, 0x40, 3], [7, 0x40, 1]]));
    assert_eq!(reason, "failed-creditpool-unlock-too-much");
    assert_ne!(core_reason(&input("unlock", 0, &[[2, 0x40, 3], [7, 0x40, 0]])), reason);
}
