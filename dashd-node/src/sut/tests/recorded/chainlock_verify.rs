//! LK1a-2b: [`NodeClVerify`] on the recorded `feature_llmq_chainlocks.py` chain, connected block
//! by block with step 10 verifying each CbTx ChainLock. At each tip the node verifier answers
//! for the run's ChainLocks as `clsig`s: the CbTx ChainLock of the next block as step 10 did, its
//! one-bit flip and wrong height, its signature re-encoded across the V19 switch, and every lock
//! at the start bounds `tip + 8` and `tip + 9` (`llmq/quorumsman.cpp:741-751` at v24.0.0).

use dash_evo::llmq::BlsSigValue;
use dash_p2p_core::ClSig;
use dash_pkc::bls::{BlsSchemeKind, BlsSigBytes};
use dashd_locks::chainlock::{ClVerdict, ClVerify as _, Read};

use super::*;
use crate::locks::NodeClVerify;

/// A CbTx's best ChainLock: the block carrying it, the locked height, hash and signature.
#[derive(Clone, Copy)]
struct CbtxLock {
    at: u32,
    height: u32,
    hash: BlockHash,
    sig: [u8; 96],
}

impl CbtxLock {
    fn clsig(&self, height: i32, sig: [u8; 96]) -> ClSig {
        ClSig::new(height, self.hash, BlsSigBytes::from(sig))
    }

    fn own_height(&self) -> i32 {
        i32::try_from(self.height).expect("a height")
    }
}

/// The run's non-null CbTx ChainLocks, by carrying block.
fn cbtx_locks(run: &Recording) -> Vec<CbtxLock> {
    (1..)
        .zip(run.after_genesis())
        .filter_map(|(at, b): (u32, _)| {
            let payload = b.txs()[0].to_mutable().extra_payload().clone();
            if payload.len() < 167 || u16::from_le_bytes([payload[0], payload[1]]) < 3 {
                return None;
            }
            let sig: [u8; 96] = payload[71..167].try_into().expect("96 bytes");
            if sig == [0; 96] {
                return None;
            }
            let height = at - 1 - u32::from(payload[70]);
            let hash = run.blocks[usize::try_from(height).expect("an index")].hash();
            Some(CbtxLock {
                at,
                height,
                hash,
                sig,
            })
        })
        .collect()
}

fn verify(verifier: &NodeClVerify, msg: ClSig, read: dash_consensus::BlsCtx) -> ClVerdict {
    verifier.verify(&Read {
        msg,
        read,
    })
}

fn flipped(sig: [u8; 96]) -> [u8; 96] {
    let mut sig = sig;
    sig[95] ^= 1;
    sig
}

#[test]
fn node_chainlock_verifier_answers_on_the_recorded_chain() {
    use dash_consensus::BlsCtx;

    let run = Recording::open(CHAINLOCKS);
    let locks = cbtx_locks(&run);
    assert_eq!(locks.first().map(|l| (l.at, l.height)), Some((309, 308)), "the run's first");
    let mut distinct: Vec<CbtxLock> = Vec::new();
    for lock in &locks {
        if distinct.last().is_none_or(|last| last.height != lock.height) {
            distinct.push(*lock);
        }
    }
    assert!(distinct.len() >= 2, "{}", distinct.len());

    let dir = tempfile::tempdir().unwrap();
    let mut sut = run.node(dir.path(), &["-assumevalid=0"], ui());
    imported(&mut sut);
    let verifier =
        NodeClVerify::new(Arc::new(sut.params.chain.clone()), sut.chain().unwrap().clone());
    let mut checked = Vec::new();
    let (mut valid_at_bound, mut no_quorum_past_bound) = (0, 0);
    for h in 1..run.blocks.len() {
        let tip = u32::try_from(h - 1).unwrap();
        for lock in distinct.iter().filter(|l| l.height == tip + 8) {
            let msg = lock.clsig(lock.own_height(), lock.sig);
            assert_eq!(verify(&verifier, msg, BlsCtx::BASIC), ClVerdict::Valid, "{tip}");
            valid_at_bound += 1;
        }
        for lock in distinct.iter().filter(|l| l.height == tip + 9) {
            let msg = lock.clsig(lock.own_height(), lock.sig);
            assert_eq!(verify(&verifier, msg, BlsCtx::BASIC), ClVerdict::NoQuorum, "{tip}");
            no_quorum_past_bound += 1;
        }
        if let Some(lock) = locks.iter().find(|l| usize::try_from(l.at).unwrap() == h) {
            let height = lock.own_height();
            let at = |sig, read| verify(&verifier, lock.clsig(height, sig), read);
            assert_eq!(at(lock.sig, BlsCtx::BASIC), ClVerdict::Valid, "{h}");
            assert_eq!(at(flipped(lock.sig), BlsCtx::BASIC), ClVerdict::Invalid, "{h}");
            let wrong = verify(&verifier, lock.clsig(height + 1, lock.sig), BlsCtx::BASIC);
            assert!(matches!(wrong, ClVerdict::Invalid | ClVerdict::NoQuorum), "{h}: {wrong:?}");
            // Read under the legacy scheme, as before V19; checked under the tip's scheme.
            let legacy = BlsSigValue::from_canonical(&lock.sig, BlsSchemeKind::Basic)
                .expect("canonical")
                .to_bytes(BlsSchemeKind::Legacy);
            assert_eq!(at(legacy, BlsCtx::LEGACY), ClVerdict::Valid, "{h}");
            assert_eq!(at(legacy, BlsCtx::BASIC), ClVerdict::Invalid, "{h}");
            checked.push((lock.at, lock.height, Some(true)));
        }
        connect(&sut, &run, h);
    }
    assert_eq!(valid_at_bound, distinct.len());
    assert_eq!(no_quorum_past_bound, distinct.len());
    assert_eq!(checked.len(), locks.len());
    assert_eq!(census(dir.path()), checked, "step 10's verifier agrees, lock by lock");

    // The node's view holds the scan at the computed start and nothing else, so no height reaches
    // the SPV-only `QuorumUnavailable`.
    let lock = distinct.last().unwrap();
    let tip = i32::try_from(run.blocks.len() - 1).unwrap();
    for height in [i32::MIN, -1, 0, 7, 8].into_iter().chain(tip - 20..=tip + 20).chain([i32::MAX]) {
        let verdict = verify(&verifier, lock.clsig(height, lock.sig), BlsCtx::BASIC);
        assert!(
            matches!(verdict, ClVerdict::Valid | ClVerdict::Invalid | ClVerdict::NoQuorum),
            "{height}: {verdict:?}"
        );
    }
}

/// Core's `IsQuorumRotationEnabled(llmq_params, pindexStart)` (`llmq/quorumsman.cpp:763` at
/// v24.0.0) at the scan start, on `feature_llmq_rotation.py`'s chain. No network's ChainLock type
/// rotates (`-llmqchainlocks` refuses one), so the verifier's params name the run's rotated
/// InstantSend type as the ChainLock type: its scan at the tip carries rotation enabled, and a
/// scan whose start is before the first cycle does not. The run's own ChainLock type never does,
/// and each of the run's CbTx ChainLocks verifies at the tip.
#[test]
fn node_chainlock_scan_carries_rotation_at_its_start() {
    let run = Recording::open("feature_llmq_rotation");
    let dir = tempfile::tempdir().unwrap();
    let (sut, _) = run.assumed_on(dir.path(), ui(), &run.segments(), None);
    let chain = sut.chain().unwrap().clone();
    let ordinary = sut.params.chain.clone();
    let mut rotated = ordinary.clone();
    rotated.consensus.llmq_type_chain_locks = ordinary.consensus.llmq_type_dip0024_instant_send;
    assert_ne!(rotated.consensus.llmq_type_chain_locks, ordinary.consensus.llmq_type_chain_locks);

    let tip = i32::try_from(run.blocks.len() - 1).unwrap();
    let at = |params: &dash_params::ChainSetup, height: i32| {
        let msg = ClSig::new(height, BlockHash::from([7; 32]), BlsSigBytes::from([0; 96]));
        let scan = NodeClVerify::new(Arc::new(params.clone()), chain.clone()).scan_of(&msg);
        scan.map(|scan| scan.map(|(rotation, _)| rotation))
    };
    assert_eq!(at(&rotated, tip + 8), Ok(Some(true)), "the start is the tip");
    assert_eq!(at(&rotated, 9), Ok(Some(false)), "the start's cycle base is below 1");
    assert_eq!(at(&rotated, tip + 9), Ok(None), "no scan past the tip");
    assert_eq!(at(&ordinary, tip + 8), Ok(Some(false)), "the ChainLock type does not rotate");

    // A quorum mined at the start block is in the scan from it.
    let verifier = NodeClVerify::new(Arc::new(ordinary.clone()), chain.clone());
    let chain_locks = ordinary.consensus.llmq_type_chain_locks;
    let mined: Vec<_> =
        run.quorum_infos().into_iter().filter(|q| q.llmq_type == chain_locks).collect();
    assert!(mined.len() >= 2, "{}", mined.len());
    for quorum in mined {
        let at = run.blocks.iter().position(|b| b.hash() == quorum.mined).unwrap();
        let height = i32::try_from(at).unwrap() + 8;
        let msg = ClSig::new(height, BlockHash::from([7; 32]), BlsSigBytes::from([0; 96]));
        let (_, bases) = verifier.scan_of(&msg).unwrap().expect("a scan");
        assert!(bases.contains(&quorum.base), "mined at {at}: {bases:?}");
    }

    // A scan from a start block reads only the chain up to it, so at the run's tip each of its
    // CbTx ChainLocks verifies against the quorum dashd's signers selected at its own start.
    let mut heights = Vec::new();
    for lock in cbtx_locks(&run) {
        if heights.last() == Some(&lock.height) {
            continue;
        }
        let msg = lock.clsig(lock.own_height(), lock.sig);
        assert_eq!(
            verify(&verifier, msg, dash_consensus::BlsCtx::BASIC),
            ClVerdict::Valid,
            "{}",
            lock.height
        );
        heights.push(lock.height);
    }
    assert_eq!(heights.len(), 60, "the run's distinct CbTx ChainLocks");
}
