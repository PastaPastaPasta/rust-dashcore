//! CH7b-3 C4's tests of step 10's verify, [`AttemptClVerifier`]: on the recorded run with its
//! census (V holds no best ChainLock, so each non-null CbTx ChainLock is verified), and X4's
//! mapping through a [`QuorumScanView`] double. Each is a behavioural control against the
//! mutant its plan row names.

use std::sync::Arc;

use dash_consensus::{BlockId, BlsCtx};
use dash_dml::StoreError;
use dash_dml::testkit::{operator_key, operator_sig};
use dash_evo::llmq::{LlmqCommitment, ScanCacheUse};
use dash_evo::locks::{
    LockError, LockReject, QuorumScan, QuorumScanView, QuorumStatus, ScanLookup, ScannedQuorum,
    chainlock_request_id, sign_hash, verify_chainlock_sig,
};
use dash_params::LlmqType;
use dash_pkc::bls::{BlsEagerPk, BlsEagerSig, BlsSchemeKind};
use dash_primitives::{BlockHash, QuorumHash};

use super::super::llmq::WAIT;
use super::super::support::{Rig, RigConfig, branch};
use super::{CHAINLOCKS, Run, cbtx_sig, remined, run_params, with_diff};
use crate::chain::Fatal;
use crate::evo::cl::{AttemptClVerifier, VerifyCensus, lock_verdict, scan_start};
use crate::evo::testkit;

/// `SIGN_HEIGHT_OFFSET` (`src/llmq/quorumsman.h:214`), written out so a changed constant shows.
const SIGN_OFFSET: u32 = 8;

/// A run whose step 10 verifies with [`AttemptClVerifier`], and its census.
fn verifying() -> (Run, Arc<VerifyCensus>) {
    let (verifier, census) = AttemptClVerifier::with_census();
    (Run::with(|_| Box::new(verifier)), census)
}

impl Run {
    /// The run's last height.
    fn tip(&self) -> u32 {
        u32::try_from(self.blocks.len()).expect("a height")
    }

    /// The census the run's CbTxs give from 1 to `to`: `(h, h - 1 - diff, Some(true))` for each
    /// non-null best ChainLock, in order. Coinbases before DIP0003 carry no payload, and CbTxs
    /// before v20 no ChainLock.
    fn expected(&self, to: u32) -> Vec<(u32, u32, Option<bool>)> {
        (1..=to)
            .filter_map(|h| {
                let payload = self.at(h).block.txs()[0].to_mutable().extra_payload;
                if payload.len() < 167 || payload[71..167] == [0; 96] {
                    return None;
                }
                assert!(payload[70] < 253, "a one-byte difference at {h}");
                Some((h, h - 1 - u32::from(payload[70]), Some(true)))
            })
            .collect()
    }
}

fn checks(census: &VerifyCensus) -> Vec<(u32, u32, Option<bool>)> {
    census.checks.lock().clone()
}

fn scan_starts(census: &VerifyCensus) -> Vec<u32> {
    census.scans.lock().iter().map(|&(start, _)| start).collect()
}

/// CH7b-3 C4: every CbTx ChainLock of the run verifies at Core's position, against the quorum
/// `SelectQuorumForSigning` selects (`clsig:253-261`), one check per block, in order.
#[test]
fn the_recorded_cbtx_chainlock_verifies() {
    let (run, census) = verifying();
    let tip = run.tip();
    run.connect_to(tip);
    let want = run.expected(tip);
    assert!(want.len() >= 22, "309 to 330 at least: {}", want.len());
    assert_eq!(checks(&census), want);
}

/// CH7b-3 C4 (V-2): 309 carrying 330's ChainLock signature is `bad-cbtx-invalid-clsig`
/// (`stm:202-205`; `Invalid`, `qm:826`).
#[test]
fn a_flipped_sig_is_bad_cbtx_invalid_clsig() {
    let (run, census) = verifying();
    run.connect_to(308);
    let other = cbtx_sig(run.at(330));
    assert_ne!(other, cbtx_sig(run.at(309)));
    let flipped =
        remined(&run.p, run.at(309), |cb| cb.extra_payload[71..167].copy_from_slice(&other));
    assert_eq!(run.submit(&flipped), "bad-cbtx-invalid-clsig");
    assert_eq!(checks(&census), [(309, 308, Some(false))]);
    run.kept.assert_no_abort(&run.rig);
}

/// CH7b-3 C4 (V-1): 309's ChainLock moved to 56, before the run mines any quorum, so the
/// scan at 48 selects none: Core's `NoQuorum` (`qm:821-823`), `bad-cbtx-invalid-clsig`, not a
/// stop.
#[test]
fn no_quorum_is_bad_cbtx_invalid_clsig() {
    let (run, census) = verifying();
    run.connect_to(308);
    let early = with_diff(&run.p, run.at(309), 252);
    assert_eq!(run.submit(&early), "bad-cbtx-invalid-clsig");
    assert_eq!(checks(&census), [(309, 56, Some(false))]);
    assert_eq!(scan_starts(&census), [48], "the one scan");
    run.kept.assert_no_abort(&run.rig);
}

/// CH7b-3 C4 (V-3): 310 to 329 carry 308's ChainLock at differences 1 to 20. Each check is at
/// 308, the ChainLock's height (its request id, `clsig:248-251`, and sign height), and passes.
#[test]
fn the_request_id_is_the_cl_height() {
    let (run, census) = verifying();
    run.connect_to(329);
    let older: Vec<_> = checks(&census).into_iter().filter(|&(h, ..)| h >= 310).collect();
    let want: Vec<_> = (310..=329).map(|h| (h, 308, Some(true))).collect();
    assert_eq!(older, want);
}

/// CH7b-3 C4 (V-6): each check's scan starts `SIGN_HEIGHT_OFFSET` below its ChainLock's height
/// (`SelectQuorumForSigningStartBlock`).
#[test]
fn selection_uses_the_sign_offset() {
    let (run, census) = verifying();
    run.connect_to(330);
    let want: Vec<u32> = checks(&census).iter().map(|&(_, cl, _)| cl - SIGN_OFFSET).collect();
    assert_eq!(want.len(), 22, "309 to 330");
    assert_eq!(scan_starts(&census), want);
}

/// CH7b-3 C4 (V-4; X1, OD-CP-1): step 10's scan is Core's cached overload, the only one that
/// reads and writes the scan cache (`qm:224-241`, `qm:283-290`). 309 and 310 both lock 308, so
/// both scan at 300: 309's misses and writes the facade's cache, and 310's hits it. The run
/// holds fewer quorums there than the type signs with, so 310's hit continues the scan below
/// the cached quorums (`qm:236-240`), finds no more, and writes again.
#[test]
fn the_scan_uses_the_cached_overload() {
    let (run, census) = verifying();
    run.connect_to(310);
    let scans = census.scans.lock().clone();
    assert_eq!(scans.iter().map(|&(start, _)| start).collect::<Vec<_>>(), [300, 300]);
    let first = scans[0].1.expect("a store block");
    let second = scans[1].1.expect("a store block");
    assert_eq!((first.cache, first.cache_written), (ScanCacheUse::Miss, true));
    assert_eq!(second.store, first.store);
    assert_eq!((second.cache, second.cache_written), (ScanCacheUse::HitContinued, true));
    assert_eq!(second.n, first.n, "the continuation finds no older quorum");
}

/// CH7b-3 C4 (V-5): with no quorum object cached, the checks build their scans' quorums through
/// the attempt's handle. A dispatch to V from inside the attempt would wait on V itself, so each
/// block's reply arriving within the bound shows none was made.
#[test]
fn the_verifier_never_dispatches_on_v() {
    let (run, census) = verifying();
    run.connect_to(308);
    let t = run.p.consensus.llmq_type_chain_locks;
    let objects = || {
        let quorums = run.facade.test_quorums().expect("free");
        quorums.objects.cache(t).map_or(0, |cache| cache.len())
    };
    assert_eq!(objects(), 0, "cold");
    for h in 309..=330 {
        let reply = run.rig.v.process_block(run.at(h).block.clone(), true, None, true);
        let r = reply.recv_timeout(WAIT).expect("V answers within the bound").expect("V replies");
        assert!(r.accepted && r.state.is_valid(), "{h}: {r:?}");
    }
    assert!(objects() > 0, "the checks built their quorums");
    assert_eq!(checks(&census).len(), 22);
    run.kept.assert_no_abort(&run.rig);
}

/// CH7b-3 C4 (V-7; X4): a read fault in step 10's scan is a store failure (D5, `evo/node.rs`
/// `store_failure`): one `AbortNode`, never a verdict.
#[test]
fn a_view_fault_is_a_store_failure_not_a_verdict() {
    let (run, census) = verifying();
    run.connect_to(308);
    *census.fault_at.lock() = Some(309);
    run.rig.submit_stopping(run.at(309), "V stops on the fault");
    let text = "VerifyChainLock: evo store read failed: an injected read fault in step 10's scan";
    assert_eq!(run.kept.reports(), [(text.to_owned(), None)]);
    assert_eq!(checks(&census), [(309, 308, None)]);
}

/// CH7b-3 (S-1; Opus r1 m-2): step 10's scan starts at the parent's ancestor
/// (`SelectQuorumForSigningStartBlock`, `qm:741-752`, whose active chain ends at the parent while
/// the block connects). The reader holds two branches that differ from 5: the main one, V's
/// active chain and best header, and a side one with the parent at 9. The scan at 6 starts on the
/// side branch, not at the block V's chain and best header hold there; below the fork, and for a
/// parent on the main branch, it starts at the main block. Synthetic blocks: the start reads only
/// the reader.
#[test]
fn the_scan_starts_on_the_parents_branch() {
    let rig = Rig::with(RigConfig::default());
    let main = branch(&rig.p, &rig.genesis, 12, 0);
    let side = branch(&rig.p, &main[3], 5, 1);
    for m in main.iter().chain(&side) {
        rig.accept(m);
    }
    let reader = rig.v.reader();
    let snap = reader.snapshot();
    let (main_6, side_6, prev) = (rig.id(&main[5]), rig.id(&side[1]), rig.id(&side[4]));
    assert_eq!((side[1].height, side[4].height), (6, 9));
    assert_ne!(side_6, main_6);
    assert_eq!(snap.active.at(6), Some(main_6), "V's active chain holds the main block");
    assert_eq!(
        reader.ancestor(snap.tip.best_header, 6),
        Some(main_6),
        "and so does its best header"
    );

    assert_eq!(scan_start(reader, prev, 6), Ok(side_6), "the parent's branch");
    assert_eq!(scan_start(reader, prev, 4), Ok(rig.id(&main[3])), "below the fork");
    assert_eq!(scan_start(reader, rig.id(&main[8]), 6), Ok(main_6), "a parent on the main branch");
}

/// A view that answers every scan with one result; a ChainLock check reads nothing else.
struct Scans(Result<ScanLookup, StoreError>);

impl QuorumScanView for Scans {
    type Error = StoreError;

    fn tip_height(&self) -> u32 {
        320
    }

    fn scan(&self, _: LlmqType, _: u32) -> Result<ScanLookup, StoreError> {
        self.0.clone()
    }

    fn scan_at_tip(&self, _: LlmqType, _: usize) -> Result<ScanLookup, StoreError> {
        unreachable!("a ChainLock check selects at its sign height")
    }

    fn block_height(&self, _: &BlockHash) -> Result<Option<u32>, StoreError> {
        unreachable!("a ChainLock check names no block")
    }

    fn quorum(&self, _: LlmqType, _: &QuorumHash) -> Result<Option<QuorumStatus>, StoreError> {
        unreachable!("a ChainLock check names no quorum")
    }
}

/// `r` is X4's `Fatal`: V's own `AbortNode` with `text`, which V reports latched or not.
fn assert_fatal(r: Result<Result<bool, StoreError>, Fatal>, text: &str) {
    match r {
        Err(f) => {
            assert_eq!(f, Fatal::abort_node(text));
            assert!(f.carried_past(), "an AbortNode, not an assert: {text}");
            assert_eq!(f.reported_by_v(true), Some(text), "V reports it, latched or not");
        }
        other => panic!("{text}: {other:?} is a verdict or a store failure"),
    }
}

/// CH7b-3 C4 (V-1, V-7, V-8; X4): each row of X4's table, with its exact result. Through a view
/// double: Core's `Valid`, `NoQuorum` (`qm:821-823`) and `Invalid` (`qm:826`), the node's
/// unavailable and out-of-horizon rejections, and a view fault. Through the mapping alone, the
/// rows no view reaches: an unknown type (the params configure the ChainLock type), a signature
/// outside the BLS domain (the pinned dash-pkc never answers `Unsupported`, BL-400) and the
/// InstantSend and signature-form rejections.
#[test]
fn the_lock_error_mapping_is_cores() {
    let p = run_params(CHAINLOCKS);
    let t = p.consensus.llmq_type_chain_locks;
    let (h, hash) = (308, BlockHash::from([7; 32]));
    let mut qc: LlmqCommitment = testkit::commitment(t, QuorumHash::from([5; 32]), 1);
    let scheme = BlsSchemeKind::Basic;
    qc.quorum_public_key = BlsEagerPk::new(operator_key(9, scheme).get(), scheme);
    let id = chainlock_request_id(h);
    let signed = operator_sig(9, &sign_hash(t, &qc.quorum_hash, &id, hash.as_bytes()));
    let wrong = operator_sig(9, &[1; 32]);
    let held = |entries| {
        Ok(ScanLookup::Held(QuorumScan {
            rotation_enabled: false,
            entries,
        }))
    };
    let entry = || vec![ScannedQuorum::mined(BlockId(1), &qc)];
    let check = |scan, sig: &BlsEagerSig| {
        lock_verdict(verify_chainlock_sig(&p, &Scans(scan), BlsCtx::BASIC, h, &hash, sig.value()))
    };

    assert_eq!(check(held(entry()), &signed), Ok(Ok(true)), "Valid");
    assert_eq!(check(held(vec![]), &signed), Ok(Ok(false)), "NoQuorum");
    assert_eq!(check(held(entry()), &wrong), Ok(Ok(false)), "Invalid");
    let fault = StoreError::read("a fault");
    assert_eq!(check(Err(fault.clone()), &signed), Ok(Err(fault)), "a view fault");
    let rejected = "VerifyChainLock: no verdict: lock rejected:";
    let unavailable = format!("{rejected} QuorumUnavailable");
    assert_fatal(check(Ok(ScanLookup::Unavailable), &signed), &unavailable);
    let horizon = format!("{rejected} OutsideHorizon");
    assert_fatal(check(Ok(ScanLookup::OutsideHorizon), &signed), &horizon);

    let rejects = [
        LockReject::MalformedSignature,
        LockReject::Malformed,
        LockReject::UnknownCycle,
        LockReject::InvalidCycle,
    ];
    let no_view = [
        (LockError::BlsUnsupported, "lock signature outside the BLS domain".to_owned()),
        (LockError::UnknownLlmqType(7), "unknown LLMQ type 7".to_owned()),
    ]
    .into_iter()
    .chain(rejects.map(|r| (LockError::Rejected(r), format!("lock rejected: {r:?}"))));
    for (e, text) in no_view {
        assert_fatal(lock_verdict(Err(e)), &format!("VerifyChainLock: no verdict: {text}"));
    }
}
