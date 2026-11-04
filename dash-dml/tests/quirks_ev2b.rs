//! Quirks 13, 24, 25 and 42 (`docs/node-program/quirks.toml`) against Core-produced vectors
//! (EV2b D25): blocks over crafted contexts with `oracle_dml_apply`'s result
//! (`tests/data/ev2b-quirks.txt`, written by node-l1's `dml_quirk_vectors` example and re-checked
//! against Core by `l1_dml_apply`), replayed through `build_new_list_from_block` and the reference
//! twin without the oracle.

use dash_consensus::primitives::{Tx, decode_block};
use dash_dml::codec::core_disk;
use dash_dml::testkit::replay::{CoreVerdict, Vector, read_vectors, replay};
use dash_dml::{DmlBuildError, DmlEvent, FAILED_PROCSPECTXSINBLOCK, MnList};
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dash_primitives::{SpecialPayload, TxHash};

const VECTORS: &str = include_str!("data/ev2b-quirks.txt");

/// The crafted contexts' chain: regtest at v24.0.
fn regtest() -> ChainSetup {
    ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().expect("Core's parameters")
}

/// The vectors of `quirk`, each replayed with its trace: our result is Core's, with Core's
/// payee, and the twin agrees with the build.
fn replayed(quirk: u32) -> Vec<(Vector, Result<MnList, DmlBuildError>, Vec<DmlEvent>)> {
    let params = regtest();
    let vectors: Vec<Vector> = read_vectors(VECTORS)
        .expect("the vector file")
        .into_iter()
        .filter(|v| v.quirk == quirk)
        .collect();
    assert!(!vectors.is_empty(), "no vector of quirk {quirk}");
    vectors
        .into_iter()
        .map(|v| {
            let mut events = Vec::new();
            let ours = replay(&params, &v.ctx, &v.block, &mut events)
                .unwrap_or_else(|e| panic!("{}: {e}", v.name));
            assert!(
                ours.agrees_with(&v.core, v.core_payee),
                "{}: ours {:?}, payee {:?}",
                v.name,
                ours.result.as_ref().err(),
                ours.payee
            );
            assert_eq!(ours.twin, Ok(()), "{}: twin", v.name);
            (v, ours.result, events)
        })
        .collect()
}

fn prev_list(v: &Vector) -> MnList {
    core_disk::decode_list_s3(&mut &v.ctx.list[..]).expect("Core's list")
}

/// Quirk 13, the list's half: an all-banned list has no payee (Core's `nullptr`); on a tie at
/// one pay height the smallest proTxHash is paid, unconfirmed or not. That such a block accepts
/// any coinbase payee and still credits the pool is the payment check's half (EV3, CH7a).
#[test]
fn quirk_13_payee_order_and_all_banned() {
    let vectors = replayed(13);
    let banned = vectors
        .iter()
        .find(|(v, _, _)| {
            let list = prev_list(v);
            !list.is_empty() && list.iter().all(|d| d.hot.is_banned())
        })
        .expect("an all-banned vector");
    assert_eq!(banned.0.core_payee, None);
    let (tie, _, _) =
        vectors.iter().find(|(v, _, _)| v.core_payee.is_some()).expect("a tie vector");
    let list = prev_list(tie);
    assert!(list.len() >= 2, "a tie needs two masternodes");
    let heights: Vec<i32> = list.iter().map(|d| d.pay_height()).collect();
    assert!(heights.iter().all(|h| *h == heights[0]), "one pay height: {heights:?}");
    assert!(list.iter().all(|d| d.is_valid() && d.cold.confirmed_hash.is_null()));
    let smallest = list.iter().map(|d| d.pro_tx_hash).min().expect("masternodes");
    assert_eq!(tie.core_payee, Some(*smallest.as_bytes()));
}

/// Quirk 24: a QcTx punishes masternodes that are banned already: their penalty grows and their
/// ban height stays. The cap, `max(100, total)`, is the list's as rebuilt so far: after the
/// block's ProRegTx over 160 masternodes it is 161, and the penalty `CalcPenalty(66)` of it.
#[test]
fn quirk_24_pose_penalty() {
    let (mut banned, mut rebuilt) = (0, 0);
    for (v, ours, events) in replayed(24) {
        let prev = prev_list(&v);
        let new = ours.expect("Core accepts it");
        let grown: Vec<_> = prev
            .iter()
            .filter(|d| d.hot.is_banned())
            .filter_map(|old| Some((old, new.get(&old.pro_tx_hash)?)))
            .filter(|(old, new)| new.hot.pose_penalty > old.hot.pose_penalty)
            .collect();
        for (old, new) in &grown {
            assert_eq!(new.hot.banned_height(), old.hot.banned_height(), "{}", v.name);
        }
        banned += grown.len();
        let prev_max = 100.max(prev.counts().total() as i32);
        for e in &events {
            if let DmlEvent::Punished {
                from,
                to,
                max,
                ..
            } = *e
                && max > prev_max
            {
                assert_eq!(max, prev_max + 1, "{}: one ProRegTx before the QcTx", v.name);
                assert_eq!(to, (from + max * 66 / 100).min(max), "{}", v.name);
                rebuilt += 1;
            }
        }
    }
    assert!(banned > 0, "no banned masternode punished");
    assert!(rebuilt > 0, "no penalty from the list as rebuilt");
}

/// Quirk 25: the build applies a block's transactions in order (a block that updates one
/// masternode twice gives Core another result with only those two transactions swapped, and
/// ours follows), and a ProRegTx whose collateral a masternode holds replaces it (the old one is
/// gone, the new one holds the collateral).
#[test]
fn quirk_25_dmn_update_order() {
    let vectors = replayed(25);
    let replaced = vectors.iter().any(|(v, ours, _)| {
        let prev = prev_list(v);
        let Ok(new) = ours else {
            return false;
        };
        prev.iter().any(|old| {
            new.get(&old.pro_tx_hash).is_none()
                && new
                    .get_by_collateral(&old.collateral)
                    .is_some_and(|d| d.pro_tx_hash != old.pro_tx_hash)
        })
    });
    assert!(replaced, "no vector replaces a masternode by collateral");
    let twice = |order: &str| {
        let (v, _, _) = vectors
            .iter()
            .find(|(v, _, _)| v.name.contains(&format!("updated twice, {order}")))
            .unwrap_or_else(|| panic!("no double update {order}"));
        v
    };
    let (in_order, swapped) = (twice("in block order"), twice("swapped"));
    assert!(matches!(in_order.core, CoreVerdict::Accept(_)), "{}", in_order.name);
    assert_ne!(in_order.core, swapped.core, "the order changes Core's result");
    // The blocks differ only in two updates of one masternode, swapped.
    let txs = |v: &Vector| -> Vec<(TxHash, Option<TxHash>)> {
        let block = decode_block(&mut &v.block[..]).expect("the block").into_value();
        block.txs().iter().map(|tx| (tx.txid(), updated(tx))).collect()
    };
    let (a, b) = (txs(in_order), txs(swapped));
    assert_eq!(a.len(), b.len(), "the same transactions");
    let moved: Vec<_> = a.iter().zip(&b).filter(|(x, y)| x != y).collect();
    let [(a1, b1), (a2, b2)] = moved[..] else {
        panic!("not two transactions swapped: {moved:?}");
    };
    assert_eq!((a1, a2), (b2, b1));
    assert!(a1.1.is_some() && a1.1 == a2.1, "two updates of one masternode");
}

/// The masternode a ProUpServTx, ProUpRegTx or ProUpRevTx updates.
fn updated(tx: &Tx) -> Option<TxHash> {
    match &tx.payload()?.ok()?.payload {
        SpecialPayload::ProviderUpdateService(p) => Some(p.pro_tx_hash),
        SpecialPayload::ProviderUpdateRegistrar(p) => Some(p.pro_tx_hash),
        SpecialPayload::ProviderUpdateRevoke(p) => Some(p.pro_tx_hash),
        _ => None,
    }
}

/// Quirk 42: an exception inside the build is `failed-procspectxsinblock`, and which result a
/// block gets depends on its transactions' order: the same transactions reversed give Core
/// another result, and ours follows.
#[test]
fn quirk_42_in_block_list_exceptions() {
    let vectors = replayed(42);
    let thrown: Vec<_> =
        vectors.iter().filter(|(v, _, _)| matches!(v.core, CoreVerdict::Exception(_))).collect();
    assert!(!thrown.is_empty());
    for (v, ours, _) in thrown {
        let reason = ours.as_ref().err().and_then(|e| e.reject_reason());
        assert_eq!(reason, Some(FAILED_PROCSPECTXSINBLOCK), "{}", v.name);
    }
    for pair in vectors.chunks(2) {
        let [(a, _, _), (b, _, _)] = pair else {
            panic!("quirk 42 vectors come in pairs");
        };
        assert_ne!(a.core, b.core, "{} / {}", a.name, b.name);
    }
}
