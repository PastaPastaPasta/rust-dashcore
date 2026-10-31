//! `diff_coinjoin` (O2a-CJ1) against Dash Core v24's in-process oracle: the boundary rows reach
//! the verdicts they are for, the node agrees with Core on them, on the target's public seeds
//! and on splitmix64 inputs of every case, and every case compares more than it skips.

use std::collections::BTreeMap;

use bitcoin_hashes::sha256;
use dash_oracle_sys::Pin;
use node_l1::coinjoin::{
    CASE_COLLATERAL, CASE_DSTX_STRUCTURE, CASE_ENTRY, CASE_HASHES, CASE_IN_OUTS, CASE_QUEUE_TIME,
    CASE_RULE, CASE_SIGNATURE, POOL_SIZE_BOUNDARIES, Writer, compare_coinjoin, finish_raw,
    seed_dsq, seed_dstx, seed_inputs,
};
use node_l1::{Outcome, target};

const PIN: Pin = Pin::V24_0;

fn diff(input: &[u8]) -> Outcome {
    compare_coinjoin(input, PIN).unwrap_or_else(|d| panic!("case {} diverges: {d}", input[0]))
}

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

fn wire<T: dash_types::codec::BaseCodec>(value: &T) -> Vec<u8> {
    let mut out = Vec::new();
    value.encode(&mut out);
    out
}

#[test]
fn registered_on_v24() {
    let t = target("diff_coinjoin").expect("registered");
    assert_eq!(t.pins, &[PIN]);
    assert_eq!((t.compare)(&[], PIN), Ok(Outcome::OutOfDomain));
}

/// Every rule on raw boundary values: the oracle's refusals (ints outside int32, the
/// promotion counts outside 0 ..= INT32_MAX / 2, negative sizes) are out of the domain, and
/// the node agrees on the rest.
#[test]
fn rule_rows() {
    let raw =
        |op: u8, args: &[i64]| args.iter().fold(Writer::case(CASE_RULE).u8(op), |w, &a| w.int(a)).0;
    // AmountToDenomination around each denomination and the extremes.
    for v in [i64::MIN, -1, 0, 100_000, 100_001, 100_002, 1_000_010_000, i64::MAX] {
        assert_eq!(diff(&raw(0, &[v])), Outcome::BothAccept);
    }
    // An int argument outside int32 is refused, one inside compared.
    assert_eq!(diff(&raw(1, &[i64::from(i32::MAX) + 1])), Outcome::OutOfDomain);
    assert_eq!(diff(&raw(1, &[i64::from(i32::MIN)])), Outcome::BothAccept);
    // ShouldPromote: the count range, and a promotion Core wants.
    assert_eq!(diff(&raw(8, &[-1, 0, 0, 0])), Outcome::OutOfDomain);
    assert_eq!(diff(&raw(8, &[i64::from(i32::MAX / 2) + 1, 0, 0, 0])), Outcome::OutOfDomain);
    assert_eq!(diff(&raw(8, &[100, 0, 20, 100])), Outcome::BothAccept);
    assert_eq!(diff(&raw(9, &[100, 0, 1, 100])), Outcome::BothAccept);
    // ValidateFinalTxComposition: a negative size is refused, larger counts above
    // u64::MAX / 10 are skipped (the node saturates where Core's size_t wraps), the largest
    // compared agree.
    let max = (u64::MAX / 10) as i64;
    assert_eq!(diff(&raw(10, &[-1, 0, 0, 0])), Outcome::OutOfDomain);
    assert_eq!(diff(&raw(10, &[0, 0, 0, max + 1])), Outcome::OutOfDomain);
    assert_eq!(diff(&raw(10, &[max, max, 0, max])), Outcome::BothAccept);
    assert_eq!(diff(&raw(10, &[max, max, max, 0])), Outcome::BothAccept);
    // The totals are never multiplied: compared up to i64::MAX.
    assert_eq!(diff(&raw(10, &[i64::MAX, i64::MAX, max, max])), Outcome::BothAccept);
}

/// Every pair of picks on the two-argument rule and on each four-argument rule's first two.
#[test]
fn rule_picks() {
    let mut outcomes = BTreeMap::<String, usize>::new();
    for op in [6u8, 8, 9, 10] {
        for a in 0..32u8 {
            for b in 0..32u8 {
                let mut w = Writer::case(CASE_RULE).u8(op).u8(2 * a).u8(2 * b);
                if op != 6 {
                    w = w.u8(2 * ((a + b) % 32)).u8(2 * (a ^ b));
                }
                *outcomes.entry(format!("{op} {:?}", diff(&w.0))).or_default() += 1;
            }
        }
    }
    eprintln!("{outcomes:?}");
}

/// The seeds that `seed_inputs` builds for the session cases reach the verdicts they are for.
/// The in/outs seeds at session denomination 0 (denom byte 0) are out of the domain
/// (`denomination_zero_rows`); the rest are compared.
#[test]
fn session_rows() {
    let seeds = seed_inputs();
    let denomination_zero = |s: &[u8]| s[0] == CASE_IN_OUTS && s[3] == 0;
    assert_eq!(seeds.iter().filter(|s| denomination_zero(s)).count(), 8);
    let of = |case: u8| seeds.iter().filter(move |s| s[0] == case && !denomination_zero(s));
    let tally = |case: u8| {
        let mut counts = BTreeMap::<String, usize>::new();
        for seed in of(case) {
            *counts.entry(format!("{:?}", diff(seed))).or_default() += 1;
        }
        counts
    };
    for case in [CASE_ENTRY, CASE_IN_OUTS, CASE_COLLATERAL] {
        let counts = tally(case);
        eprintln!("case {case}: {counts:?}");
        assert!(counts.get("BothAccept").copied().unwrap_or(0) > 0, "case {case}: {counts:?}");
        assert!(counts.get("BothReject").copied().unwrap_or(0) > 0, "case {case}: {counts:?}");
        assert_eq!(counts.get("OutOfDomain"), None, "case {case}: {counts:?}");
    }
}

/// A collateral (mainnet, the mempool accepting) over chain coins `coins` (outpoint byte, raw
/// value), spending `inputs`, paying raw `outputs` (value, script byte).
fn collateral(coins: &[(u8, i64)], inputs: &[u8], outputs: &[(i64, u8)]) -> Vec<u8> {
    collateral_with(coins, &[0, 0], inputs, outputs)
}

/// [`collateral`] with the view's mempool transactions and lock mask written as `mempool`.
fn collateral_with(
    coins: &[(u8, i64)],
    mempool: &[u8],
    inputs: &[u8],
    outputs: &[(i64, u8)],
) -> Vec<u8> {
    let mut w = Writer::case(CASE_COLLATERAL).u8(0).u8(1).u8(coins.len() as u8);
    for &(outpoint, value) in coins {
        w = w.u8(outpoint).u8(2).int_raw(value).u8(0x70);
    }
    w = w.bytes(mempool).u8(inputs.len() as u8).bytes(inputs).u8(outputs.len() as u8);
    for &(value, script) in outputs {
        w = w.u8(2).int_raw(value).u8(script);
    }
    w.0
}

/// Collaterals on which Core's sums overflow are refused by the oracle exactly where the
/// harness predicts (any other refusal, or a missing one, diverges); the sums Core never
/// reaches are compared.
#[test]
fn collateral_overflow_rows() {
    const P2PKH: u8 = 0x71;
    const EMPTY: u8 = 0xc1;
    let funded = [(0u8, 30_000i64)];
    // The output sum.
    assert_eq!(
        diff(&collateral(&funded, &[0], &[(i64::MAX, P2PKH), (1, P2PKH)])),
        Outcome::OutOfDomain
    );
    // Not reached: the first output's script fails before the second add.
    assert_eq!(
        diff(&collateral(&funded, &[0], &[(i64::MAX, EMPTY), (1, P2PKH)])),
        Outcome::BothReject
    );
    // The input sum, and not reached after a missing input.
    let huge = [(1u8, i64::MAX), (2, i64::MAX)];
    assert_eq!(diff(&collateral(&huge, &[1, 2, 0], &[(1, P2PKH)])), Outcome::OutOfDomain);
    assert_eq!(diff(&collateral(&huge, &[0, 1, 2], &[(1, P2PKH)])), Outcome::BothReject);
    // The difference, and a difference of i64::MIN (the log line would negate it).
    assert_eq!(diff(&collateral(&[(1, i64::MAX)], &[1], &[(-1, P2PKH)])), Outcome::OutOfDomain);
    assert_eq!(diff(&collateral(&[(1, i64::MIN)], &[1], &[(0, P2PKH)])), Outcome::OutOfDomain);
    assert_eq!(diff(&collateral(&[(1, i64::MIN + 1)], &[1], &[(0, P2PKH)])), Outcome::BothReject);
    // A mempool coin first: unusable while unlocked (Core stops there), added once locked.
    // One mempool transaction spending chain outpoint 0x70 and paying two collaterals.
    let parent = |mask: u8| [1, 0, 0x70, 1, 1 | 2 << 2, 0x75, mask];
    let spend = [0x80, 1, 2];
    assert_eq!(
        diff(&collateral_with(&huge, &parent(0), &spend, &[(1, P2PKH)])),
        Outcome::BothReject
    );
    assert_eq!(
        diff(&collateral_with(&huge, &parent(1), &spend, &[(1, P2PKH)])),
        Outcome::OutOfDomain
    );
    // Not reached: no outputs.
    assert_eq!(diff(&collateral(&[(1, i64::MIN)], &[1], &[])), Outcome::BothReject);
}

/// Opus-h r1 F0: at session denomination 0 Core's fee sums add unchecked amounts (an output of
/// i64::MIN overflows them), so the oracle refuses every such call and the node must not panic.
#[test]
fn denomination_zero_rows() {
    for network in 0..4u8 {
        let session = || Writer::case(CASE_IN_OUTS).u8(network).u8(0).u8(0);
        let overflow = finish_raw(session(), &[(0, 30_000)], &[0], &[(i64::MIN, 0x20)]);
        assert_eq!(diff(&overflow), Outcome::OutOfDomain);
        let preserving = finish_raw(session(), &[(0, 12_345)], &[0], &[(12_345, 0x20)]);
        assert_eq!(diff(&preserving), Outcome::OutOfDomain);
        // The same entry at a real denomination is compared.
        let session = || Writer::case(CASE_IN_OUTS).u8(network).u8(0).u8(5);
        let smallest = finish_raw(session(), &[(0, 100_001)], &[0], &[(100_001, 0x20)]);
        assert_eq!(diff(&smallest), Outcome::BothAccept);
    }
}

/// Opus-h r1 F1 and F2: the max-fee bound (a fee of exactly 0.1 DASH is valid, one duff more is
/// not), and dash-coinjoin's own MoneyRange checks, which must refuse before the mempool is
/// asked (an input coin above MAX_MONEY, a negative output).
#[test]
fn collateral_bound_rows() {
    for network in 0..4u8 {
        let collateral = || Writer::case(CASE_COLLATERAL).u8(network).u8(1);
        let fee = |coin: i64| finish_raw(collateral(), &[(0, coin)], &[0], &[(10_000, 0x77)]);
        assert_eq!(diff(&fee(10_010_000)), Outcome::BothAccept);
        assert_eq!(diff(&fee(10_010_001)), Outcome::BothReject);
        assert_eq!(diff(&fee(21_000_000 * 100_000_000 + 1)), Outcome::BothReject);
        let negative =
            finish_raw(collateral(), &[(0, 20_000)], &[0], &[(-10_000, 0x77), (0, 0x78)]);
        assert_eq!(diff(&negative), Outcome::BothReject);
    }
}

/// Signed messages: hashes compared, the signature valid under the key in either scheme's
/// encoding and invalid under another key or a changed message, the queue's time bound at
/// ±30 s and ±31 s.
#[test]
fn message_rows() {
    let dsq = wire(&seed_dsq(true));
    let dstx = wire(&seed_dstx(3, 3));
    assert_eq!(diff(&Writer::case(CASE_HASHES).u8(0).bytes(&dsq).0), Outcome::BothAccept);
    assert_eq!(diff(&Writer::case(CASE_HASHES).u8(1).bytes(&dstx).0), Outcome::BothAccept);
    for (kind, msg) in [(0u8, &dsq), (1, &dstx)] {
        for legacy in [0u8, 2] {
            let sig = |key: u8, msg: &[u8]| {
                Writer::case(CASE_SIGNATURE).u8(kind | legacy).u8(key).bytes(msg).0
            };
            assert_eq!(diff(&sig(0, msg)), Outcome::BothAccept, "{kind} {legacy}");
            assert_eq!(diff(&sig(1, msg)), Outcome::BothReject, "{kind} {legacy}");
            let mut changed = msg.clone();
            // The time's low byte (queue) or sigTime's (broadcast): both signed.
            let at = if kind == 0 {
                4 + 32
            } else {
                changed.len() - 8
            };
            changed[at] ^= 1;
            assert_eq!(diff(&sig(0, &changed)), Outcome::BothReject, "{kind} {legacy}");
        }
    }
    // A message with bytes after it, and one cut short.
    for kind in [0u8, 1] {
        let msg = if kind == 0 {
            &dsq
        } else {
            &dstx
        };
        let long = [&msg[..], &[0]].concat();
        assert_eq!(diff(&Writer::case(CASE_HASHES).u8(kind).bytes(&long).0), Outcome::BothReject);
        let short = &msg[..msg.len() - 1];
        assert_eq!(diff(&Writer::case(CASE_HASHES).u8(kind).bytes(short).0), Outcome::BothReject);
    }
    for delta in [-31i8, -30, 30, 31] {
        let input = Writer::case(CASE_QUEUE_TIME).u8(((delta << 1) as u8) | 1).bytes(&dsq).0;
        assert_eq!(diff(&input), Outcome::BothAccept);
    }
    for now in [i64::MIN, -1, 0, i64::MAX] {
        let input = Writer::case(CASE_QUEUE_TIME).u8(0).int_raw(now).bytes(&dsq).0;
        assert_eq!(diff(&input), Outcome::BothAccept);
    }
}

/// The broadcast structure on every network, V24 state and period position, for a balanced,
/// a promotion-shaped and an invalid broadcast.
#[test]
fn structure_rows() {
    let mut outcomes = BTreeMap::<String, usize>::new();
    for (ins, outs) in [(3u16, 3u16), (10, 1), (1, 10), (2, 3), (19, 1)] {
        let dstx = wire(&seed_dstx(ins, outs));
        for network in 0..4u8 {
            for state in 0..5u8 {
                for height in [0u16, 1, 99, 119, 199, 239, 249, 499, 4030, 4031, 4032, 65535] {
                    let input = Writer::case(CASE_DSTX_STRUCTURE)
                        .u8(network)
                        .u8(state)
                        .bytes(&height.to_le_bytes())
                        .bytes(&dstx)
                        .0;
                    *outcomes.entry(format!("{:?}", diff(&input))).or_default() += 1;
                }
            }
        }
    }
    eprintln!("{outcomes:?}");
    assert!(outcomes.get("BothAccept").copied().unwrap_or(0) > 0);
    assert!(outcomes.get("BothReject").copied().unwrap_or(0) > 0);
    assert_eq!(outcomes.get("OutOfDomain"), None);
}

/// Balanced broadcasts at the pool sizes: 180 is valid everywhere and 201 nowhere; 181 and 200
/// are valid exactly where V24 is active (never on mainnet, where it is NEVER_ACTIVE, so its rows
/// are the pre-V24 ones), and somewhere. The node agrees on each, `fPossiblyValidPostV24`
/// included (181 and 200 before V24).
#[test]
fn pool_size_rows() {
    let mut accepted = BTreeMap::<u16, Vec<(u8, u8)>>::new();
    let mut rows = 0;
    for n in POOL_SIZE_BOUNDARIES {
        let dstx = wire(&seed_dstx(n, n));
        for network in 0..4u8 {
            for state in 0..5u8 {
                for height in [1u16, 4030] {
                    let input = Writer::case(CASE_DSTX_STRUCTURE)
                        .u8(network)
                        .u8(state)
                        .bytes(&height.to_le_bytes())
                        .bytes(&dstx)
                        .0;
                    match diff(&input) {
                        Outcome::BothAccept => {
                            accepted.entry(n).or_default().push((network, state))
                        }
                        Outcome::BothReject => {}
                        other => panic!("{n}/{n} on {network} in {state} at {height}: {other:?}"),
                    }
                    rows += 1;
                }
            }
        }
    }
    let all: Vec<(u8, u8)> =
        (0..4u8).flat_map(|n| (0..5u8).flat_map(move |s| [(n, s), (n, s)])).collect();
    assert_eq!(accepted.get(&180), Some(&all), "180/180 is valid before and after V24");
    assert_eq!(accepted.get(&201), None, "201/201 is valid nowhere");
    let post_v24 = accepted.get(&181).expect("181/181 is valid after V24 somewhere");
    assert_eq!(accepted.get(&200), Some(post_v24), "181 and 200 are valid exactly after V24");
    assert!(post_v24.iter().all(|&(network, _)| network != 0), "mainnet's V24 is NEVER_ACTIVE");
    assert!(post_v24.len() < all.len(), "and some rows are before V24");
    assert_eq!(rows, 160);
}

/// Splitmix64 inputs of every case: no divergence, and each case compares more than it skips.
#[test]
fn random_inputs() {
    let mut outcomes = BTreeMap::<(u8, String), usize>::new();
    for seed in 0..1600u64 {
        let case = (seed % 8) as u8;
        let mut input = bytes(seed, 64 + (seed as usize * 37) % 960);
        input[0] = case;
        *outcomes.entry((case, format!("{:?}", diff(&input)))).or_default() += 1;
    }
    eprintln!("{outcomes:?}");
    for case in 0..8u8 {
        let count = |o: &str| outcomes.get(&(case, o.to_string())).copied().unwrap_or(0);
        assert!(
            count("BothAccept") + count("BothReject") > count("OutOfDomain"),
            "case {case}: {outcomes:?}"
        );
    }
}

/// `diff_coinjoin` agrees with Core on each of its public seeds (`node-fuzz/public-seeds`,
/// written by the `coinjoin_seeds` example), and each seed is current.
#[test]
fn fuzz_target_agrees_on_its_public_seeds() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../node-fuzz/public-seeds/diff_coinjoin");
    let seeds = seed_inputs();
    let mut names = std::collections::BTreeSet::new();
    for input in &seeds {
        let digest = sha256::Hash::hash(input).to_byte_array();
        let name: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
        let path = format!("{dir}/{name}");
        assert_eq!(std::fs::read(&path).ok().as_ref(), Some(input), "{path}: rerun coinjoin_seeds");
        diff(input);
        names.insert(name);
    }
    let on_disk = std::fs::read_dir(dir).expect("the seed directory").count();
    assert_eq!(on_disk, names.len(), "stale seeds in {dir}: rerun coinjoin_seeds");
}
