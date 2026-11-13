use std::cell::Cell;
use std::sync::Arc;

use dash_consensus::primitives::{Tx, TxRef, decode_tx_exact};
use dash_core_vectors::hex::try_parse_hex as unhex;
use dash_evo::bls::BlsCtx;
use dash_evo::cbtx::{CoinbaseFacts, nonnull_coinbase_chainlock};
use dash_evo::locks::{ClsigHashSchemes, clsig_hash};
use dash_p2p_core::ClSig;
use dash_primitives::{BlockHash, Transaction, TxType};
use dashd_locks::chainlock::Read;
use dashd_locks::chainlock::coinbase::coinbase_clsig;
use serde_json::Value;

fn original() -> TxRef {
    let fixtures: Value =
        serde_json::from_str(include_str!("../../dash-evo/tests/data/cbtx_testnet.json")).unwrap();
    let fixture =
        fixtures["blocks"].as_array().unwrap().iter().find(|b| b["height"] == 1_569_000).unwrap();
    Arc::new(
        decode_tx_exact(&unhex(fixture["coinbase_hex"].as_str().unwrap()).unwrap())
            .unwrap()
            .into_value(),
    )
}

fn edited(edit: impl FnOnce(&mut Vec<u8>)) -> CoinbaseFacts {
    let tx = original();
    let mut extra = tx.extra_payload().to_vec();
    edit(&mut extra);
    CoinbaseFacts::First(Arc::new(Tx::new(Transaction::new(
        tx.raw().version(),
        tx.tx_type(),
        tx.raw().inputs().clone(),
        tx.raw().outputs().clone(),
        tx.raw().lock_time(),
        extra,
    ))))
}

fn extract(facts: &CoinbaseFacts, height: u32, best: i32) -> Option<(Read<ClSig>, [u8; 32])> {
    coinbase_clsig(
        facts,
        height,
        true,
        true,
        true,
        best,
        |_| Some(BlockHash::from([7; 32])),
        ClsigHashSchemes::both(BlsCtx::BASIC),
    )
}

#[test]
fn guard_boundaries() {
    let facts = CoinbaseFacts::First(original());
    let calls = Cell::new(0);
    for (name, synced, enabled, v20, height, best, ancestor, want) in [
        ("valid", true, true, true, 10, 8, true, true),
        ("not synced", false, true, true, 10, 8, true, false),
        ("disabled", true, false, true, 10, 8, true, false),
        ("before v20", true, true, false, 10, 8, true, false),
        ("height zero", true, true, true, 0, -1, true, false),
        ("height one", true, true, true, 1, -1, true, true),
        ("equal best", true, true, true, 10, 9, true, false),
        ("below best", true, true, true, 10, 10, true, false),
        ("missing ancestor", true, true, true, 10, 8, false, false),
        ("max signed height", true, true, true, i32::MAX as u32, -1, true, true),
        ("outside Core height", true, true, true, i32::MAX as u32 + 1, -1, true, false),
    ] {
        calls.set(0);
        let got = coinbase_clsig(
            &facts,
            height,
            v20,
            synced,
            enabled,
            best,
            |h| {
                calls.set(calls.get() + 1);
                assert_eq!(h, height - 1);
                ancestor.then(|| BlockHash::from([7; 32]))
            },
            ClsigHashSchemes::both(BlsCtx::BASIC),
        );
        assert_eq!(got.is_some(), want, "{name}");
        assert_eq!(calls.get(), usize::from(want || name == "missing ancestor"), "{name}");
        if let Some((sig, hash)) = got {
            assert_eq!(sig.msg.height(), height as i32 - 1, "{name}");
            assert_eq!(sig.msg.block_hash(), BlockHash::from([7; 32]));
            assert_eq!(clsig_hash(&sig.msg, ClsigHashSchemes::both(BlsCtx::BASIC)), Some(hash));
        }
    }
}

#[test]
fn payload_and_signature_guard_boundaries() {
    for (name, facts, want) in [
        ("unavailable", CoinbaseFacts::BlockDataUnavailable, false),
        ("empty block", CoinbaseFacts::NoTransactions, false),
        ("empty payload", edited(|p| p.clear()), false),
        (
            "truncated payload",
            edited(|p| {
                p.pop();
            }),
            false,
        ),
        ("trailing payload", edited(|p| p.push(0)), false),
        (
            "version 2",
            edited(|p| {
                p[..2].copy_from_slice(&2_u16.to_le_bytes());
                p.truncate(70);
            }),
            false,
        ),
        ("version 3", edited(|_| {}), true),
        (
            "version 4",
            edited(|p| {
                p[..2].copy_from_slice(&4_u16.to_le_bytes());
                p.extend([0; 32]);
            }),
            true,
        ),
        ("null signature", edited(|p| p[71..167].fill(0)), false),
        ("bad signature", edited(|p| p[71..167].fill(0xff)), false),
    ] {
        assert_eq!(extract(&facts, 10, -1).is_some(), want, "{name}");
    }
}

#[test]
fn height_difference_boundaries() {
    for (diff, want_height) in
        [(0, Some(9)), (8, Some(1)), (9, Some(0)), (10, None), (11, None), (252, None)]
    {
        let facts = edited(|p| p[70] = diff);
        assert_eq!(
            extract(&facts, 10, -1).map(|(sig, _)| sig.msg.height()),
            want_height,
            "diff {diff}"
        );
    }
    let too_large = edited(|p| {
        p.splice(70..71, [0xfe, 0xff, 0xff, 0xff, 0xff]);
    });
    assert!(extract(&too_large, 10, -1).is_none());
    let noncanonical = edited(|p| {
        p.splice(70..71, [0xfd, 0, 0]);
    });
    assert!(extract(&noncanonical, 10, -1).is_none());
}

#[test]
fn handler_ignores_payload_height_that_nonnull_helper_checks() {
    let facts = CoinbaseFacts::First(original());
    assert!(nonnull_coinbase_chainlock(&facts, 1_569_000, true, BlsCtx::BASIC).is_some());
    assert!(nonnull_coinbase_chainlock(&facts, 1_569_001, true, BlsCtx::BASIC).is_none());
    assert_eq!(extract(&facts, 1_569_001, -1).unwrap().0.msg.height(), 1_569_000);
}

#[test]
fn handler_checks_type_but_does_not_require_coinbase_inputs() {
    let tx = original();
    let make = |kind| {
        CoinbaseFacts::First(Arc::new(Tx::new(Transaction::new(
            tx.raw().version(),
            kind,
            vec![],
            tx.raw().outputs().clone(),
            tx.raw().lock_time(),
            tx.extra_payload().to_vec(),
        ))))
    };
    let facts = make(TxType::CoinbaseCommitment);
    let CoinbaseFacts::First(first) = &facts else {
        panic!("first tx")
    };
    assert!(!first.is_coinbase());
    assert!(extract(&facts, 10, -1).is_some());
    assert!(nonnull_coinbase_chainlock(&facts, 10, true, BlsCtx::BASIC).is_none());
    assert!(extract(&make(TxType::new(0)), 10, -1).is_none());
}

#[test]
fn coinbase_signature_uses_the_explicit_bls_context() {
    let facts = CoinbaseFacts::First(original());
    let (signature, _) =
        nonnull_coinbase_chainlock(&facts, 1_569_000, true, BlsCtx::BASIC).unwrap();
    let legacy = signature.to_bytes(BlsCtx::LEGACY.scheme());
    let facts = edited(|p| p[71..167].copy_from_slice(&legacy));
    let (sig, hash) = coinbase_clsig(
        &facts,
        10,
        true,
        true,
        true,
        -1,
        |_| Some(BlockHash::from([7; 32])),
        ClsigHashSchemes::both(BlsCtx::LEGACY),
    )
    .unwrap();
    assert_eq!(sig.read, BlsCtx::LEGACY);
    assert_eq!(sig.msg.sig().as_bytes(), &legacy);
    assert_eq!(clsig_hash(&sig.msg, ClsigHashSchemes::both(BlsCtx::LEGACY)), Some(hash));
    assert_ne!(sig.msg.sig().as_bytes(), &signature.to_bytes(BlsCtx::BASIC.scheme()));
}

#[test]
fn coinbase_hash_can_write_a_different_scheme_than_payload_read() {
    let facts = CoinbaseFacts::First(original());
    let schemes = ClsigHashSchemes {
        read: BlsCtx::BASIC,
        write: BlsCtx::LEGACY,
    };
    let (sig, hash) = coinbase_clsig(
        &facts,
        10,
        true,
        true,
        true,
        -1,
        |_| Some(BlockHash::from([7; 32])),
        schemes,
    )
    .unwrap();
    assert_eq!(sig.read, schemes.read);
    assert_eq!(clsig_hash(&sig.msg, schemes), Some(hash));
    assert_ne!(clsig_hash(&sig.msg, ClsigHashSchemes::both(BlsCtx::BASIC)), Some(hash));
}
