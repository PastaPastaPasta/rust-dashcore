use dash_evo::cbtx::null_signature;
use dash_primitives::{CoinbaseCommitment, MerkleRoot};

use super::*;

/// The CbTx payload dashd v24.0.0-rc.4 mined at 432 on a default no-masternode regtest chain
/// (`tests/data/mn0-evo-regtest.json`, chain "default"): version 3, both roots null, a null best
/// ChainLock and the first platform reward as the credit pool's balance.
const DASHD_CBTX_432: &str = concat!(
    "0300",                                                             // nVersion 3
    "b0010000",                                                         // nHeight 432
    "0000000000000000000000000000000000000000000000000000000000000000", // merkleRootMNList
    "0000000000000000000000000000000000000000000000000000000000000000", // merkleRootQuorums
    "00",                                                               // bestCLHeightDiff
    "000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    "000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    "442d3a0700000000", // creditPoolBalance 1.21253188
);

fn unhex(s: &str) -> Vec<u8> {
    dash_dml::testkit::replay::unhex(s).expect("hex")
}

/// The null best ChainLock signature is the 96 bytes dashd writes for a default `CBLSSignature`.
#[test]
fn null_chainlock_is_dashds() {
    let dashd = unhex(DASHD_CBTX_432);
    let decoded = CoinbaseCommitment::decode(&mut &dashd[..]).expect("dashd's CbTx");
    let signature = decoded.best_cl_signature().as_ref().expect("v3");
    assert_eq!(signature.to_bytes(), null_signature().to_bytes());

    let ours = CoinbaseCommitment::new(
        dash_primitives::CbCommitmentVersion::new(3),
        432_u32.into(),
        MerkleRoot::default(),
        Some(MerkleRoot::default()),
        Some(0),
        Some(null_signature()),
        Some(121_253_188),
        None,
    );
    let mut bytes = Vec::new();
    ours.encode(&mut bytes);
    assert_eq!(bytes, dashd);
}

/// `CCbTx::nVersion`: V24 counts only with V20; V20 over DIP0008; DIP0008 over version 1.
#[test]
fn cbtx_version_follows_cores_order() {
    let eras = |dip8, v20, v24| Eras {
        dip3: true,
        dip8,
        v20,
        mn_rr: false,
        v24,
    };
    let table = [
        ((false, false, false), 1),
        ((false, false, true), 1),
        ((true, false, false), 2),
        ((true, false, true), 2),
        ((true, true, false), 3),
        ((false, true, false), 3),
        ((true, true, true), 4),
        ((false, true, true), 4),
    ];
    for ((dip8, v20, v24), version) in table {
        assert_eq!(eras(dip8, v20, v24).cbtx_version(), version, "{dip8} {v20} {v24}");
    }
}

const V19: i32 = 100;

/// `CalcCbTxBestChainlock` over a best ChainLock at `best` (signature "best") and a parent
/// reader answering `parent` (signature "parent"): the difference and signature, and how often
/// the parent was read.
fn select(
    best: Option<i32>,
    tip: i32,
    parent: Option<u32>,
) -> (Option<(u32, &'static str)>, usize) {
    let reads = std::cell::Cell::new(0);
    let read = || {
        reads.set(reads.get() + 1);
        Ok::<_, ()>(parent.map(|diff| ("parent", diff)))
    };
    let got = best_chainlock(best.map(|h| (h, &"best")), V19, tip, &read).expect("no fault");
    (got, reads.get())
}

/// E-cl (OD-CP-124 B1): every branch of `CalcCbTxBestChainlock` (`node/miner.cpp:117-170`), with
/// the parent read only after the best-is-the-tip return (OD-CP-125 R3).
#[test]
fn best_chainlock_takes_cores_branches() {
    let tip = 300;
    // :120-124: a best ChainLock below V19 is the null sig.
    assert_eq!(select(Some(V19 - 1), tip, None), (None, 1));
    assert_eq!(select(Some(V19 - 1), tip, Some(4)), (Some((5, "parent")), 1));
    assert_eq!(select(Some(V19), tip, None), (Some(((tip - V19) as u32, "best")), 1));
    // :125-131: the tip's own ChainLock, the parent never read.
    assert_eq!(select(Some(tip), tip, Some(4)), (Some((0, "best")), 0));
    assert_eq!(select(Some(tip), tip, None), (Some((0, "best")), 0));
    // :134-139: a parent CL and no best: carried, one block further back.
    assert_eq!(select(None, tip, Some(4)), (Some((5, "parent")), 1));
    // :143-149: the best older than the parent's: carried.
    assert_eq!(select(Some(tip - 6), tip, Some(4)), (Some((5, "parent")), 1));
    // :150-154: a tie (the parent's own height) takes the best.
    assert_eq!(select(Some(tip - 5), tip, Some(4)), (Some((5, "best")), 1));
    // The best newer than the parent's.
    assert_eq!(select(Some(tip - 2), tip, Some(4)), (Some((2, "best")), 1));
    // :160-166: no parent CL and no best: a null CL.
    assert_eq!(select(None, tip, None), (None, 1));
    // :168-172: no parent CL, the best inserted.
    assert_eq!(select(Some(tip - 7), tip, None), (Some((7, "best")), 1));
}

/// E-cl: Core's `int` differences stored into `uint32_t`. A best ChainLock above the tip (a
/// template on a tip that is not the best chain's) wraps, with or without a parent CL; a parent
/// difference of `UINT32_MAX` plus one wraps to 0.
#[test]
fn best_chainlock_wraps_as_cores_uint32() {
    let tip = 300;
    assert_eq!(select(Some(tip + 2), tip, None), (Some((u32::MAX - 1, "best")), 1));
    assert_eq!(select(Some(tip + 2), tip, Some(4)), (Some((u32::MAX - 1, "best")), 1));
    assert_eq!(select(None, tip, Some(u32::MAX)), (Some((0, "parent")), 1));
}

/// E-cl: a parent read that faults ends the selection with its error, unless the best ChainLock
/// is the tip's.
#[test]
fn best_chainlock_passes_a_parent_fault_on() {
    let fault = || Err::<Option<(&str, u32)>, _>("read");
    assert_eq!(best_chainlock(Some((290, &"best")), V19, 300, &fault), Err("read"));
    assert_eq!(best_chainlock(None, V19, 300, &fault), Err("read"));
    assert_eq!(best_chainlock(Some((300, &"best")), V19, 300, &fault), Ok(Some((0, "best"))));
}
