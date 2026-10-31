//! Steps 1–6 over the regtest capture.

use std::collections::HashSet;

use dash_consensus::primitives::Tx;
use dash_evo::cbtx::CoinbaseFacts;
use dash_evo::llmq::{QuorumSnapshot, SIGN_HEIGHT_OFFSET, WORK_DIFF_DEPTH};
use dash_evo::locks::LockReject;
use dash_evo::sml::SmlList;
use dash_primitives::{Commitment, QuorumHash, TxHash};
use dash_types::codec::BaseCodec;

use super::*;
use crate::engine::{ROW_CANDIDATES, ROW_RETRIES};
use crate::view::HeldRow;
use crate::{
    Coverage, LockOutcome, MnReject, MnRequest, MnViewError, RequestKey, SetAt, Unavailable,
};

/// Below DIP3 (height 2 here) a block has no CbTx and Core answers a diff to it with a null
/// target: nothing authenticates it.
#[test]
fn a_pre_dip3_diff_is_refused() {
    let chain = FixtureChain::full();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    let d = step_diff(1);
    assert_eq!(d.block_hash, BlockHash::from([0; 32]));
    assert_eq!(engine.apply_diff(&d, &chain, config()), Err(MnReject::UnknownTarget(d.block_hash)));
}

/// Every step diff authenticates; the sets within tip − H_keep stay held (D28), the tip's list
/// whole, and every captured ChainLock quorum verifies.
#[test]
fn step_diffs_authenticate_every_list_and_verify_the_chainlock_quorums() {
    let chain = FixtureChain::full();
    let engine = stepped(&chain);
    for h in chain.tip - config().h_keep()..=chain.tip {
        assert!(engine.has_authenticated_set(chain.id(h)), "{h}");
    }
    assert!(engine.list(chain.id(chain.tip)).is_some());
    let quorums = core_quorums(CHAIN_LOCKS);
    assert!(!quorums.is_empty());
    for q in &quorums {
        let (qc, state) =
            engine.quorum(llmq(CHAIN_LOCKS), &QuorumHash::from(q.hash)).expect("held");
        assert!(matches!(state, QuorumState::Verified(_)), "{}: {state:?}", q.height);
        assert_eq!(
            qc.quorum_public_key.to_bytes(dash_pkc::bls::BlsSchemeKind::Basic).as_slice(),
            q.public_key.as_slice()
        );
    }
}

#[test]
fn rotated_quorums_wait_for_their_cycles_qrinfo_then_verify() {
    let chain = FixtureChain::full();
    let mut engine = stepped(&chain);
    let quorums = core_quorums(ROTATED);
    assert!(!quorums.is_empty());
    // Every list is held, no row is: each rotated commitment waits on its cycle's `qrinfo`
    // (`RowNotHeld`, never Core's absent row).
    let mut cycles: Vec<u32> = quorums.iter().map(|q| q.height - q.height % 24).collect();
    cycles.sort_unstable();
    cycles.dedup();
    for q in &quorums {
        let cycle = q.height - q.height % 24;
        assert_eq!(
            state(&engine, ROTATED, q.hash),
            QuorumState::Pending(RequestKey::QrInfo(chain.hash(cycle))),
            "{}",
            q.height
        );
    }
    let mut requests = engine.requests();
    requests.sort_by_key(|r| match r {
        MnRequest::QrInfo {
            request_height,
            ..
        } => *request_height,
        MnRequest::MnListDiff {
            ..
        } => 0,
    });
    assert_eq!(requests.len(), cycles.len(), "{requests:?}");
    for (r, cycle) in requests.iter().zip(&cycles) {
        let MnRequest::QrInfo {
            request_height,
            bases,
            ..
        } = r
        else {
            panic!("{r:?}");
        };
        assert_eq!(*request_height, *cycle);
        assert_eq!(bases.len(), 4);
    }
    apply_cycles(&mut engine, &chain);
    assert!(engine.requests().is_empty(), "{:?}", engine.requests());
    for q in &quorums {
        let state = state(&engine, ROTATED, q.hash);
        assert!(matches!(state, QuorumState::Verified(_)), "{}: {state:?}", q.height);
    }
}

fn flip<T: BaseCodec>(t: &T, offset_from_end: usize) -> T {
    let mut bytes = Vec::new();
    t.encode(&mut bytes);
    let at = bytes.len() - 1 - offset_from_end;
    bytes[at] ^= 0x01;
    T::decode(&mut &bytes[..]).expect("still decodes")
}

fn is_signature_reject(r: &Result<impl std::fmt::Debug, LockOutcome>) -> bool {
    matches!(
        r,
        Err(LockOutcome::Rejected(LockReject::InvalidSignature | LockReject::MalformedSignature))
    )
}

#[test]
fn every_captured_chainlock_verifies_and_a_changed_one_does_not() {
    let chain = FixtureChain::full();
    let mut engine = synced(&chain);
    let clsigs = clsigs();
    assert_eq!(clsigs.len(), 5);
    for (cl, core) in &clsigs {
        assert!(core, "Core verified it at capture");
        let v = engine.verify_chainlock(cl, &chain, config()).expect("Core's verdict");
        assert_eq!((v.height, v.block_hash), (cl.height, cl.block_hash));
        // Step 6 rejects a changed signature, or the same signature over another block.
        assert!(is_signature_reject(&engine.verify_chainlock(&flip(cl, 3), &chain, config())));
        let mut other = cl.clone();
        other.block_hash = chain.hash(5);
        assert_eq!(
            engine.verify_chainlock(&other, &chain, config()),
            Err(LockOutcome::Rejected(LockReject::InvalidSignature))
        );
    }
    assert!(engine.requests().is_empty());
}

/// BL-178 (a): each captured `isdlock` verifies, and a changed txid or signature does not, as
/// Core's `verifyislock` answered at capture.
#[test]
fn every_captured_islock_verifies_and_a_changed_one_does_not() {
    let chain = FixtureChain::full();
    let mut engine = synced(&chain);
    let islocks = isdlocks();
    assert_eq!(islocks.len(), 3);
    for (lock, core) in &islocks {
        assert!(core, "Core verified it at capture");
        let v = engine.verify_islock(lock, &chain, config()).expect("Core's verdict");
        assert_eq!(v.txid, lock.txid);
        assert!(is_signature_reject(&engine.verify_islock(&flip(lock, 3), &chain, config())));
        // Core's verdict on the second pass decides (`ProcessPendingISLocks`): an invalid
        // signature, or no quorum where that pass's scan is empty.
        let mut other = lock.clone();
        other.txid = TxHash::from([0x5a; 32]);
        assert!(matches!(
            engine.verify_islock(&other, &chain, config()),
            Err(LockOutcome::Rejected(LockReject::InvalidSignature | LockReject::NoQuorum))
        ));
    }
}

/// SB0 §5.2.1: an ISLock of the cycle at 264 delivered at tip 311, past that cycle's end, signs
/// at the cycle's last block (287), so it verifies from the sets held at or below it: with every
/// list from 288 dropped it still verifies, and asks for nothing. Lock D, of the cycle at 288,
/// signs at the tip, so it waits and asks.
#[test]
fn an_islock_past_its_cycle_signs_at_the_cycles_end() {
    let chain = FixtureChain::full();
    let mut engine = synced(&chain);
    let rotated = config().params().consensus.llmq_type_dip0024_instant_send;
    let c = u32::from(config().params().consensus.llmq(rotated).expect("configured").dkg_interval);
    let cycle_of = |lock: &IsdLock| {
        let hash = BlockHash::from(*lock.cycle_hash.as_bytes());
        chain.lookup(&hash).expect("ok").expect("a cycle").0
    };
    let locks = isdlocks();
    let end =
        locks.iter().map(|(l, _)| cycle_of(l) + c).filter(|e| *e <= chain.tip).min().expect("one");
    assert_eq!(end, 288);
    for h in end..=chain.tip {
        engine.forget_list(BlockId(h));
    }
    let mut past = 0;
    for (lock, _) in &locks {
        let before = engine.requests();
        let verdict = engine.verify_islock(lock, &chain, config());
        if cycle_of(lock) + c <= chain.tip {
            past += 1;
            assert_eq!(verdict.map(|v| v.txid), Ok(lock.txid));
            assert_eq!(engine.requests(), before);
        } else {
            assert_eq!(verdict, Err(LockOutcome::Rejected(LockReject::QuorumUnavailable)));
            assert!(engine.requests().len() > before.len());
        }
    }
    assert_eq!(past, 2, "locks A and B");
}

/// Without the sets a lock selects from, the check has no verdict and the engine asks for the
/// set at the scan's store block.
#[test]
fn a_lock_without_its_set_asks_for_it() {
    let chain = FixtureChain::full();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&genesis_diff(281), &chain, config()).expect("Core's diff");
    let (cl, _) = clsigs().into_iter().find(|(cl, _)| cl.height == 311).expect("the tip's");
    assert_eq!(
        engine.verify_chainlock(&cl, &chain, config()),
        Err(LockOutcome::Rejected(LockReject::QuorumUnavailable))
    );
    assert!(engine.requests().iter().any(|r| matches!(r, MnRequest::MnListDiff { .. })));
}

/// A ChainLock whose selection start block is above the header tip waits for the headers
/// (SB0 §5.2 step 7), and verifies once they arrive.
#[test]
fn a_chainlock_above_the_header_tip_is_held_until_headers_reach_it() {
    let full = FixtureChain::full();
    let mut engine = synced(&full);
    let short = FixtureChain::anchored(0, 300);
    let (cl, _) = clsigs().into_iter().find(|(cl, _)| cl.height == 311).expect("the tip's");
    assert_eq!(engine.verify_chainlock(&cl, &short, config()), Err(LockOutcome::Pending));
    assert_eq!(engine.take_pending_chainlock(&short), Ok(None));
    let held = engine.take_pending_chainlock(&full).expect("no stop").expect("ready");
    assert_eq!(held, cl);
    assert!(engine.verify_chainlock(&held, &full, config()).is_ok());
}

/// A mutated diff is refused at its step and changes nothing.
#[test]
fn a_mutated_diff_is_refused_at_its_step() {
    let chain = FixtureChain::full();
    let good = genesis_diff(288);
    let refuse = |d: MnListDiff| {
        let mut engine = MasternodeListEngine::new(Mode::Client);
        let r = engine.apply_diff(&d, &chain, config());
        assert!(engine.list(chain.id(288)).is_none() && engine.requests().is_empty());
        r.expect_err("refused")
    };

    // Step 1: an unknown target.
    let mut d = good.clone();
    d.block_hash = BlockHash::from([7; 32]);
    assert_eq!(refuse(d), MnReject::UnknownTarget(BlockHash::from([7; 32])));
    // A base the engine does not hold.
    let mut d = good.clone();
    d.base_block_hash = chain.hash(100);
    assert_eq!(refuse(d), MnReject::UnknownBase(chain.hash(100)));
    // Step 2: the proof.
    let mut d = good.clone();
    d.cb_tx_merkle_tree.transactions += 1;
    assert!(matches!(refuse(d), MnReject::CbTxProof(_)));
    // Another block's coinbase.
    let mut d = good.clone();
    d.cb_tx = genesis_diff(281).cb_tx;
    assert!(matches!(refuse(d), MnReject::CbTxProof(_)));
    // A coinbase that is not one, and a CbTx payload with bytes after it.
    let mut d = good.clone();
    d.cb_tx.inputs[0].prevout.index = 0;
    assert_eq!(refuse(d), MnReject::BadCoinbase("not a coinbase special transaction"));
    let mut d = good.clone();
    d.cb_tx.extra_payload.push(0);
    assert_eq!(refuse(d), MnReject::BadCoinbase("its payload does not decode"));
    // Step 3: the list root.
    let mut d = good.clone();
    d.mn_list[0].is_valid = !d.mn_list[0].is_valid;
    assert_eq!(refuse(d), MnReject::MnListRoot);
    let mut d = good.clone();
    d.mn_list.pop();
    assert_eq!(refuse(d), MnReject::MnListRoot);
    // Step 4: the quorums root.
    let mut d = good.clone();
    assert!(!d.new_quorums.is_empty());
    d.new_quorums.pop();
    assert_eq!(refuse(d), MnReject::QuorumsRoot);
    let mut d = good.clone();
    d.new_quorums[0].quorum_index = Some(d.new_quorums[0].quorum_index.unwrap_or(0) + 1);
    assert_eq!(refuse(d), MnReject::QuorumsRoot);

    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&good, &chain, config()).expect("Core's diff");
    assert!(engine.has_authenticated_set(chain.id(288)));
}

/// Core's reply for cycle 288 with H − C's row (cycle 264's) changed in every bit.
fn wrong_row_reply() -> QrInfo {
    wrong_h_c(qrinfo(288, &[QRINFO_BASE]))
}

/// Core's H − C row of the reply for cycle 288.
fn honest_h_c() -> QuorumSnapshot {
    crate::convert::snapshot(&qrinfo(288, &[QRINFO_BASE]).snapshot_h_c)
}

/// Core's H − C row with a skip list of `i`: a distinct row for each `i`.
fn nth_row(i: usize) -> QuorumSnapshot {
    QuorumSnapshot {
        skip_list: vec![i32::try_from(i).expect("small")],
        ..honest_h_c()
    }
}

/// A reply with its H − C row changed in every bit.
fn wrong_h_c(mut q: QrInfo) -> QrInfo {
    let bits = &mut q.snapshot_h_c.active_quorum_members;
    for b in &mut bits.data {
        *b ^= 0xff;
    }
    let last = bits.num_bits % 8;
    if last != 0 {
        *bits.data.last_mut().expect("a byte") &= (1u8 << last) - 1;
    }
    q
}

/// D20: a wrong row is not refused (no root commits to rows), and the commitments it feeds are
/// never `Unverified` over it: step 5 asks for their cycle's `qrinfo` again, and an honest reply
/// recovers them, its row a second candidate. The lists stay held throughout.
#[test]
fn a_wrong_row_is_disputed_until_an_honest_reply_recovers_its_cycle() {
    let chain = FixtureChain::full();
    let mut engine = stepped(&chain);
    let key = RequestKey::QrInfo(chain.hash(288));
    engine.apply_qrinfo(&wrong_row_reply(), &chain, config()).expect("rows are not authenticated");
    for q in core_quorums(ROTATED).iter().filter(|q| q.height >= 288) {
        assert_eq!(state(&engine, ROTATED, q.hash), QuorumState::Pending(key), "{}", q.height);
        let scan = engine.scan_quorum(&chain, config(), llmq(ROTATED), &q.hash.into());
        assert_eq!(scan, Some(dash_evo::locks::QuorumStatus::Unavailable), "{}", q.height);
    }
    assert!(engine.requests().iter().any(|r| r.key() == key), "asked again");
    assert!(engine.list(chain.id(280)).is_some());

    engine.apply_qrinfo(&qrinfo(284, &[]), &chain, config()).expect("Core's qrinfo");
    engine.apply_qrinfo(&qrinfo(288, &[QRINFO_BASE]), &chain, config()).expect("Core's qrinfo");
    for q in core_quorums(ROTATED) {
        let state = state(&engine, ROTATED, q.hash);
        assert!(matches!(state, QuorumState::Verified(_)), "{}: {state:?}", q.height);
    }
    assert!(matches!(
        engine.held_row(llmq(ROTATED), chain.hash(264)),
        Some(HeldRow::Candidates(rows)) if rows.len() == 2
    ));
}

/// D20: the cycle's `qrinfo` is asked again `ROW_RETRIES` times; past them its commitments are
/// `Unavailable(RowsDisputed)`, still never `Unverified`, until a new row reopens them.
#[test]
fn disputed_rows_are_asked_for_a_bounded_number_of_times() {
    let chain = FixtureChain::full();
    let mut engine = stepped(&chain);
    let key = RequestKey::QrInfo(chain.hash(288));
    let wrong = wrong_row_reply();
    for _ in 0..=ROW_RETRIES {
        assert!(engine.requests().iter().any(|r| r.key() == key));
        engine.apply_qrinfo(&wrong, &chain, config()).expect("rows are not authenticated");
    }
    assert!(!engine.requests().iter().any(|r| r.key() == key));
    let later: Vec<_> = core_quorums(ROTATED).into_iter().filter(|q| q.height >= 288).collect();
    for q in &later {
        assert_eq!(
            state(&engine, ROTATED, q.hash),
            QuorumState::Unavailable(Unavailable::RowsDisputed),
            "{}",
            q.height
        );
    }
    // An unrequested reply holds no row; a row another requested reply carries reopens them.
    engine.apply_qrinfo(&qrinfo(288, &[QRINFO_BASE]), &chain, config()).expect("Core's qrinfo");
    assert_eq!(
        state(&engine, ROTATED, later[0].hash),
        QuorumState::Unavailable(Unavailable::RowsDisputed)
    );
    let honest = honest_h_c();
    engine.offer_row(&chain, config(), llmq(ROTATED), chain.hash(264), honest);
    for q in &later {
        assert!(
            matches!(state(&engine, ROTATED, q.hash), QuorumState::Verified(_)),
            "{}",
            q.height
        );
    }
}

/// D20: a requested reply for another cycle carrying a new row reopens the commitments disputed
/// over that cycle's row. Cycle 264's quorums read row 240 as their H − C; cycle 288's reply
/// carries it as its H − 2C.
#[test]
fn another_cycles_reply_reopens_a_disputed_row() {
    let chain = FixtureChain::full();
    let mut engine = stepped(&chain);
    let wrong = wrong_h_c(qrinfo(284, &[]));
    for _ in 0..=ROW_RETRIES {
        engine.apply_qrinfo(&wrong, &chain, config()).expect("rows are not authenticated");
    }
    for q in core_quorums(ROTATED).iter().filter(|q| q.height < 288) {
        assert_eq!(
            state(&engine, ROTATED, q.hash),
            QuorumState::Unavailable(Unavailable::RowsDisputed),
            "{}",
            q.height
        );
    }
    engine.apply_qrinfo(&qrinfo(288, &[QRINFO_BASE]), &chain, config()).expect("Core's qrinfo");
    for q in core_quorums(ROTATED) {
        let state = state(&engine, ROTATED, q.hash);
        assert!(matches!(state, QuorumState::Verified(_)), "{}: {state:?}", q.height);
    }
}

/// D20: a cycle holds each distinct row once, and the newest `ROW_CANDIDATES` of them.
#[test]
fn a_cycles_candidates_are_distinct_and_bounded() {
    let chain = FixtureChain::full();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    let cycle = chain.hash(264);
    engine.hold_candidate(llmq(ROTATED), cycle, nth_row(0));
    engine.hold_candidate(llmq(ROTATED), cycle, nth_row(0));
    assert_eq!(engine.held_row(llmq(ROTATED), cycle), Some(&HeldRow::Candidates(vec![nth_row(0)])));
    for i in 1..=ROW_CANDIDATES {
        engine.hold_candidate(llmq(ROTATED), cycle, nth_row(i));
    }
    assert_eq!(
        engine.held_row(llmq(ROTATED), cycle),
        Some(&HeldRow::Candidates((1..=ROW_CANDIDATES).map(nth_row).collect()))
    );
}

/// D20 (Sol r2 nit 1): a repeated row counts as the newest again, so the next distinct row
/// evicts the oldest other one.
#[test]
fn a_repeated_candidate_is_the_newest_again() {
    let chain = FixtureChain::full();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    let cycle = chain.hash(264);
    for i in 0..ROW_CANDIDATES {
        engine.hold_candidate(llmq(ROTATED), cycle, nth_row(i));
    }
    engine.hold_candidate(llmq(ROTATED), cycle, nth_row(0));
    engine.hold_candidate(llmq(ROTATED), cycle, nth_row(ROW_CANDIDATES));
    let kept: Vec<_> = (2..ROW_CANDIDATES).chain([0, ROW_CANDIDATES]).map(nth_row).collect();
    assert_eq!(engine.held_row(llmq(ROTATED), cycle), Some(&HeldRow::Candidates(kept)));
}

/// D20: requested replies filling every candidate place with wrong rows hold no cycle for good.
/// Past the retries a later honest row takes the oldest one's place and reopens the commitments.
#[test]
fn an_honest_row_after_every_place_holds_a_wrong_one_recovers_its_cycle() {
    let chain = FixtureChain::full();
    let mut engine = stepped(&chain);
    assert_eq!(ROW_CANDIDATES, usize::from(ROW_RETRIES) + 1, "one place per reply");
    for i in 0..ROW_CANDIDATES {
        let mut wrong = wrong_row_reply();
        wrong.snapshot_h_c.skip_list = vec![i32::try_from(i).expect("small")];
        engine.apply_qrinfo(&wrong, &chain, config()).expect("rows are not authenticated");
    }
    assert!(matches!(
        engine.held_row(llmq(ROTATED), chain.hash(264)),
        Some(HeldRow::Candidates(rows)) if rows.len() == ROW_CANDIDATES
    ));
    let later: Vec<_> = core_quorums(ROTATED).into_iter().filter(|q| q.height >= 288).collect();
    for q in &later {
        assert_eq!(
            state(&engine, ROTATED, q.hash),
            QuorumState::Unavailable(Unavailable::RowsDisputed)
        );
    }
    engine.offer_row(&chain, config(), llmq(ROTATED), chain.hash(264), honest_h_c());
    for q in &later {
        let state = state(&engine, ROTATED, q.hash);
        assert!(matches!(state, QuorumState::Verified(_)), "{}: {state:?}", q.height);
    }
}

/// D20: when the host gives up on a cycle asked for again over its rows, the commitments stay
/// disputed, not `FetchFailed`, over later payloads too (no retry is spent on them): a new row
/// reopens them.
#[test]
fn a_failed_retry_leaves_the_rows_disputed() {
    let chain = FixtureChain::full();
    let mut engine = stepped(&chain);
    let key = RequestKey::QrInfo(chain.hash(288));
    engine.apply_qrinfo(&wrong_row_reply(), &chain, config()).expect("rows are not authenticated");
    engine.request_failed(key);
    engine.apply_diff(&genesis_diff(chain.tip), &chain, config()).expect("Core's diff");
    assert!(!engine.requests().iter().any(|r| r.key() == key));
    let later: Vec<_> = core_quorums(ROTATED).into_iter().filter(|q| q.height >= 288).collect();
    for q in &later {
        assert_eq!(
            state(&engine, ROTATED, q.hash),
            QuorumState::Unavailable(Unavailable::RowsDisputed)
        );
    }
    engine.offer_row(&chain, config(), llmq(ROTATED), chain.hash(264), honest_h_c());
    for q in &later {
        assert!(matches!(state(&engine, ROTATED, q.hash), QuorumState::Verified(_)));
    }
}

/// D20: a header read failing while a candidate row is computed stops the engine, as every
/// storage failure does.
#[test]
fn a_storage_failure_while_confirming_a_row_stops() {
    let chain = FixtureChain::full();
    let mut engine = stepped(&chain);
    engine.apply_qrinfo(&wrong_row_reply(), &chain, config()).expect("rows are not authenticated");
    let failing = FixtureChain {
        fail_header: Some(256),
        ..FixtureChain::full()
    };
    assert!(matches!(
        engine.confirm_held_rows(&failing, config()),
        Err(MnReject::View(MnViewError::Storage(_)))
    ));
}

fn oldest(q: &mut QrInfo) -> &mut MnListDiff {
    &mut q.cycle_h_4c.as_mut().expect("asked with extraShare").diff
}

/// A mutated `qrinfo` is refused whole: a diff failing its step, or a row without its cycle.
#[test]
fn a_mutated_qrinfo_is_refused_whole() {
    let chain = FixtureChain::full();
    let good = qrinfo(288, &[]);
    let refuse = |q: QrInfo| {
        let mut engine = MasternodeListEngine::new(Mode::Client);
        let r = engine.apply_qrinfo(&q, &chain, config());
        assert!(engine.list(chain.id(280)).is_none());
        r.expect_err("refused")
    };
    // Without bases, the oldest cycle's diff is from genesis: the whole list and set.
    let mut q = good.clone();
    oldest(&mut q).mn_list[0].is_valid = !oldest(&mut q).mn_list[0].is_valid;
    assert_eq!(refuse(q), MnReject::MnListRoot);
    let mut q = good.clone();
    oldest(&mut q).new_quorums.pop().expect("a quorum");
    assert_eq!(refuse(q), MnReject::QuorumsRoot);
    let mut q = good.clone();
    q.diff_h.cb_tx_merkle_tree.transactions += 1;
    assert!(matches!(refuse(q), MnReject::CbTxProof(_)));
    // Cycles out of Core's order: a diff's base is not held yet.
    let mut q = good.clone();
    std::mem::swap(&mut q.diff_h_c, &mut q.diff_h_2c);
    assert!(matches!(refuse(q), MnReject::UnknownBase(_)));
    // A row whose diff targets no cycle's work block, above or below H's, or a later cycle's.
    let not_earlier =
        MnReject::QrInfo("a row's diff does not target an earlier cycle's work block");
    for (good, extra) in [(&good, 281), (&good, 264), (&qrinfo(284, &[]), 280)] {
        let mut q = good.clone();
        q.mn_list_diff_list.insert(0, genesis_diff(extra));
        q.quorum_snapshot_list.insert(0, q.snapshot_h_c.clone());
        assert_eq!(refuse(q), not_earlier, "{extra}");
    }
    let mut q = good.clone();
    q.quorum_snapshot_list.push(q.snapshot_h_c.clone());
    assert!(matches!(refuse(q), MnReject::QrInfo(_)));
}

/// EV addendum §2.4: a withheld list above the floor is asked for once, and the quorum verifies
/// when it arrives.
#[test]
fn a_withheld_list_is_asked_for_once_then_verifies() {
    let chain = FixtureChain::full();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&genesis_diff(281), &chain, config()).expect("Core's diff");
    let q = core_quorums(CHAIN_LOCKS).into_iter().find(|q| q.height == 264).expect("a quorum");
    // V20 is active: the members are the work block's (`GetAllQuorumMembers`).
    let work = chain.hash(q.height - 8);
    let key = RequestKey::MnListDiff(work);
    assert_eq!(state(&engine, CHAIN_LOCKS, q.hash), QuorumState::Pending(key));
    let asks =
        |e: &MasternodeListEngine| e.requests().into_iter().filter(|r| r.key() == key).count();
    assert_eq!(asks(&engine), 1);
    assert!(engine.requests().contains(&MnRequest::MnListDiff {
        base: None,
        target: work,
        target_height: q.height - 8,
    }));
    engine.apply_diff(&genesis_diff(280), &chain, config()).expect("Core's diff");
    assert_eq!(asks(&engine), 1);
    engine.apply_diff(&genesis_diff(256), &chain, config()).expect("Core's diff");
    assert_eq!(asks(&engine), 0);
    assert!(matches!(state(&engine, CHAIN_LOCKS, q.hash), QuorumState::Verified(_)));
}

/// A request answered without the input is `NotServed`; one the host gives up on is
/// `FetchFailed`.
#[test]
fn an_unserved_or_failed_request_makes_the_quorum_unavailable() {
    let chain = FixtureChain::full();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&genesis_diff(281), &chain, config()).expect("Core's diff");
    let q264 = core_quorums(CHAIN_LOCKS).into_iter().find(|q| q.height == 264).expect("a quorum");
    let q168 = core_quorums(CHAIN_LOCKS).into_iter().find(|q| q.height == 168).expect("a quorum");
    engine.request_failed(RequestKey::MnListDiff(chain.hash(256)));
    assert_eq!(
        state(&engine, CHAIN_LOCKS, q264.hash),
        QuorumState::Unavailable(Unavailable::FetchFailed)
    );
    assert_eq!(
        state(&engine, CHAIN_LOCKS, q168.hash),
        QuorumState::Pending(RequestKey::MnListDiff(chain.hash(160)))
    );
    // A `qrinfo` for cycle 264 answers nothing about block 160, and the diff to 160 arrives.
    engine.apply_diff(&genesis_diff(160), &chain, config()).expect("Core's diff");
    assert!(matches!(state(&engine, CHAIN_LOCKS, q168.hash), QuorumState::Verified(_)));
}

/// EV addendum §2.4: below the floor nothing is asked for. A quorum whose base is below it is
/// `UnknownBase`. A rotated quorum is `BelowFloor` when its cycle's `qrinfo` reaches below it:
/// exactly, from the set at the cycle block when that is held (Core's reply reaches its extra
/// cycle H − 4C's work block), else when H − 3C's work block is. Otherwise it is asked for, and a
/// reply reaching below the floor is refused whole (D15).
#[test]
fn below_the_floor_nothing_is_asked_for() {
    let floor = 200;
    let chain = FixtureChain::anchored(floor, 311);
    let below = |held: &[u32]| {
        let mut engine = MasternodeListEngine::new(Mode::Client);
        for h in held {
            engine.apply_diff(&genesis_diff(*h), &chain, config()).expect("Core's diff");
        }
        engine
    };
    // The set at 281 holds cycle 264's rotated quorums, the set at 311 cycle 288's.
    let mut engine = below(&[281, 311]);
    for q in core_quorums(CHAIN_LOCKS).iter().filter(|q| q.height < floor) {
        if let Some((_, s)) = engine.quorum(llmq(CHAIN_LOCKS), &QuorumHash::from(q.hash)) {
            assert_eq!(s, QuorumState::Unavailable(Unavailable::UnknownBase), "{}", q.height);
        }
    }
    let key = RequestKey::QrInfo(chain.hash(288));
    for q in core_quorums(ROTATED) {
        let expected = if q.height < 288 {
            // H − 3C's work block, 184.
            QuorumState::Unavailable(Unavailable::BelowFloor {
                height: 264 - 3 * 24 - 8,
            })
        } else {
            // H − 3C's work block is 208: asked for.
            QuorumState::Pending(key)
        };
        assert_eq!(state(&engine, ROTATED, q.hash), expected, "{}", q.height);
    }
    // Core's reply for cycle 288 carries H − 4C's work block (184), below this floor: refused
    // whole, as any payload naming a header the chain does not hold, and the host gives up.
    assert!(matches!(
        engine.apply_qrinfo(&qrinfo(288, &[QRINFO_BASE]), &chain, config()),
        Err(MnReject::UnknownTarget(_))
    ));
    engine.request_failed(key);
    for q in core_quorums(ROTATED).iter().filter(|q| q.height >= 288) {
        assert_eq!(
            state(&engine, ROTATED, q.hash),
            QuorumState::Unavailable(Unavailable::FetchFailed)
        );
    }

    // With the set at 288 held, the preflight is exact: its extra cycle H − 4C reaches 184.
    let engine = below(&[281, 288, 311]);
    for q in core_quorums(ROTATED).iter().filter(|q| q.height >= 288) {
        assert_eq!(
            state(&engine, ROTATED, q.hash),
            QuorumState::Unavailable(Unavailable::BelowFloor {
                height: 288 - 4 * 24 - 8
            }),
            "{}",
            q.height
        );
    }
    assert!(!engine.requests().iter().any(|r| matches!(r, MnRequest::QrInfo { .. })));

    // A floor at H − 3C's work block itself: cycle 264 is asked for.
    let chain = FixtureChain::anchored(184, 311);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&genesis_diff(281), &chain, config()).expect("Core's diff");
    for q in core_quorums(ROTATED).iter().filter(|q| q.height < 288) {
        assert_eq!(
            state(&engine, ROTATED, q.hash),
            QuorumState::Pending(RequestKey::QrInfo(chain.hash(264))),
            "{}",
            q.height
        );
    }
}

/// EV addendum §2.4: a rotated commitment whose cycle's H−3C work block is not held asks for one
/// `qrinfo` for its cycle, and verifies when it arrives.
#[test]
fn a_rotated_commitment_missing_its_cycles_lists_asks_for_one_qrinfo() {
    let chain = FixtureChain::full();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&genesis_diff(311), &chain, config()).expect("Core's diff");
    let key = RequestKey::QrInfo(chain.hash(288));
    let quorums: Vec<_> = core_quorums(ROTATED).into_iter().filter(|q| q.height >= 288).collect();
    assert_eq!(quorums.len(), 2);
    for q in &quorums {
        assert_eq!(state(&engine, ROTATED, q.hash), QuorumState::Pending(key));
    }
    let asks: Vec<_> = engine.requests().into_iter().filter(|r| r.key() == key).collect();
    assert_eq!(
        asks,
        [MnRequest::QrInfo {
            request: chain.hash(288),
            request_height: 288,
            bases: Vec::new(),
        }]
    );
    engine.apply_qrinfo(&qrinfo(288, &[]), &chain, config()).expect("Core's qrinfo");
    assert!(!engine.requests().iter().any(|r| r.key() == key));
    for q in &quorums {
        assert!(matches!(state(&engine, ROTATED, q.hash), QuorumState::Verified(_)));
    }
}

/// BL-178 (b): the members computed over the SML view sign with Core's operator keys
/// (`quorum info`'s members): the same multiset, and in Core's order. Every list is kept, as the
/// members are read long after step 5 read them.
#[test]
fn the_members_signing_keys_are_cores() {
    let chain = FixtureChain::full();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.keep_every_list();
    apply_to(&mut engine, &chain, FIRST, FIRST + 1..=chain.tip);
    apply_cycles(&mut engine, &chain);
    for t in [CHAIN_LOCKS, ROTATED] {
        for q in core_quorums(t) {
            let ours: Vec<Vec<u8>> = engine
                .members(&chain, config(), llmq(t), chain.id(q.height), true)
                .expect("the members")
                .iter()
                .map(|m| m.operator_key.to_vec())
                .collect();
            assert_eq!(ours, q.operator_keys, "type {t} at {}", q.height);
            let (mut a, mut b) = (ours, q.operator_keys.clone());
            a.sort();
            b.sort();
            assert_eq!(a, b);
        }
    }
}

/// A lock check's request outlives an applied diff: only a request commitments stopped waiting on
/// is dropped (D12; validator r2).
#[test]
fn a_lock_checks_request_survives_an_applied_diff() {
    let chain = FixtureChain::full();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&genesis_diff(281), &chain, config()).expect("Core's diff");
    let (cl, _) = clsigs().into_iter().find(|(cl, _)| cl.height == 311).expect("the tip's");
    assert_eq!(
        engine.verify_chainlock(&cl, &chain, config()),
        Err(LockOutcome::Rejected(LockReject::QuorumUnavailable))
    );
    let asked = engine.requests();
    assert!(asked.iter().any(|r| matches!(r, MnRequest::MnListDiff { .. })), "{asked:?}");
    engine.apply_diff(&step_diff(282), &chain, config()).expect("Core's diff");
    let after = engine.requests();
    for r in &asked {
        assert!(after.contains(r), "{r:?} dropped");
    }
}

/// D12, D20 (validator r2): a disputed cycle's `qrinfo` names a block off the active chain after
/// a reorg below it: nothing is asked and its commitments are `Unavailable(OffChain)`. Back on
/// the chain it is asked again without counting a retry, as no peer answered it, however often
/// the chain goes away and back.
#[test]
fn a_reorg_away_and_back_counts_no_row_retry() {
    let mut chain = FixtureChain {
        fork: Some(285),
        ..FixtureChain::full()
    };
    let mut engine = stepped(&chain);
    let key = RequestKey::QrInfo(chain.hash(288));
    engine.apply_qrinfo(&wrong_row_reply(), &chain, config()).expect("rows are not authenticated");
    let later: Vec<_> = core_quorums(ROTATED).into_iter().filter(|q| q.height >= 288).collect();
    for _ in 0..=ROW_RETRIES {
        chain.fork_active = true;
        engine.apply_diff(&genesis_diff(280), &chain, config()).expect("Core's diff");
        assert!(!engine.requests().iter().any(|r| r.key() == key), "off the active chain");
        for q in &later {
            assert_eq!(
                state(&engine, ROTATED, q.hash),
                QuorumState::Unavailable(Unavailable::OffChain),
                "{}",
                q.height
            );
        }
        chain.fork_active = false;
        engine.apply_diff(&genesis_diff(280), &chain, config()).expect("Core's diff");
        for q in &later {
            assert_eq!(state(&engine, ROTATED, q.hash), QuorumState::Pending(key), "{}", q.height);
        }
    }
}

/// A lock check's request for a block a reorg leaves off the active chain is dropped, as Core
/// serves no payload for it; its requests on the chain stay (D12; validator r2).
#[test]
fn a_lock_checks_request_off_the_active_chain_is_dropped() {
    let mut chain = FixtureChain {
        fork: Some(290),
        ..FixtureChain::full()
    };
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&genesis_diff(281), &chain, config()).expect("Core's diff");
    let (cl, _) = clsigs().into_iter().find(|(cl, _)| cl.height == 311).expect("the tip's");
    assert_eq!(
        engine.verify_chainlock(&cl, &chain, config()),
        Err(LockOutcome::Rejected(LockReject::QuorumUnavailable))
    );
    let asked = engine.requests();
    let above = |r: &MnRequest| matches!(r, MnRequest::MnListDiff { target_height, .. } if *target_height > 290);
    assert!(asked.iter().any(above), "{asked:?}");
    chain.fork_active = true;
    engine.apply_diff(&step_diff(282), &chain, config()).expect("Core's diff");
    let after = engine.requests();
    for r in &asked {
        assert_eq!(after.contains(r), !above(r), "{r:?}");
    }
}

/// `set_at` (D24): a block in the horizon on a branch off the active chain is `OutsideHorizon`,
/// never fetched; the active chain's block at its height is fetched.
#[test]
fn set_at_a_block_off_the_active_chain_is_outside_the_horizon() {
    let chain = FixtureChain {
        fork: Some(300),
        ..FixtureChain::full()
    };
    let engine = MasternodeListEngine::new(Mode::Client);
    let h = chain.tip - 2;
    assert_eq!(engine.set_at(BlockId(FORK + h), &chain, config()), Ok(SetAt::OutsideHorizon));
    assert!(matches!(engine.set_at(chain.id(h), &chain, config()), Ok(SetAt::Fetch { .. })));
}

/// The capture's chain with headers above its tip that carry only their height: a later tip for
/// locks that read the capture's sets.
struct Taller {
    chain: FixtureChain,
    tip: u32,
}

impl Taller {
    fn above(&self, height: u32) -> bool {
        height > self.chain.tip && height < FORK
    }

    fn hash_above(height: u32) -> BlockHash {
        let mut bytes = [0x5a; 32];
        bytes[..4].copy_from_slice(&height.to_le_bytes());
        BlockHash::from(bytes)
    }
}

impl AnchoredAncestry for Taller {
    fn floor(&self) -> u32 {
        self.chain.floor
    }

    fn header(&self, id: BlockId) -> Result<HeaderCtx, AncestryError> {
        if !self.above(id.0) {
            return self.chain.header(id);
        }
        assert!(id.0 <= self.tip, "an id the chain never issued");
        let mut header = self.chain.header(BlockId(self.chain.tip))?;
        header.height = id.0;
        header.hash = Self::hash_above(id.0);
        Ok(header)
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Result<Option<BlockId>, AncestryError> {
        if !self.above(id.0) {
            return self.chain.ancestor(id, height);
        }
        if height > id.0 {
            Ok(None)
        } else if self.above(height) {
            Ok(Some(BlockId(height)))
        } else {
            self.chain.ancestor(BlockId(self.chain.tip), height)
        }
    }

    fn prev(&self, id: BlockId) -> Result<Option<BlockId>, AncestryError> {
        match id.0.checked_sub(1) {
            Some(h) if self.above(id.0) => self.ancestor(id, h),
            _ => self.chain.prev(id),
        }
    }
}

impl MnChain for Taller {
    fn ancestry(&self) -> &dyn AnchoredAncestry {
        self
    }

    fn lookup(&self, hash: &BlockHash) -> Result<Option<BlockId>, AncestryError> {
        self.chain.lookup(hash)
    }

    fn active_tip(&self) -> Result<BlockRef, AncestryError> {
        Ok(BlockRef {
            id: BlockId(self.tip),
            hash: Self::hash_above(self.tip),
            height: self.tip,
        })
    }

    fn merkle_root(&self, id: BlockId) -> Result<Option<MerkleRoot>, AncestryError> {
        self.chain.merkle_root(id)
    }
}

/// An ISLock only its second pass verifies (SB0 §5.2.1, DEC-384). It is lock B's own signature,
/// by a quorum of the cycle at 264, with its cycle hash moved to 288. The signature covers the
/// request id and txid, not the cycle hash, so Core's verifier takes it; Core's signer never sends
/// it, as it names the selected quorum's own cycle (`instantsend/signing.cpp` rc.3:460-462).
/// While the cycle at 288 is recent (tip ≤ 288 + C) the lock signs at the tip
/// (`net_instantsend.cpp` rc.3:142-152): at 311 and 312 the first pass, from the tip, selects a
/// quorum of the cycle at 288 and fails, and the second, from tip − C (:339-353,
/// `SelectQuorumForSigningStartBlock` with that offset), selects the cycle at 264's and verifies,
/// well within the horizon. Once the cycle is old the lock signs at 288 + C − 1 = 311 and its
/// second pass starts at 287, the cycle − 1 (no `SIGN_HEIGHT_OFFSET`): it verifies up to tip
/// 287 + H_keep and is `OutsideHorizon` one block later, asking for nothing. Lock B itself, whose
/// first pass starts at 287 too, has the same last tip.
#[test]
fn an_islock_verified_by_its_second_pass_within_the_horizon() {
    let full = FixtureChain::full();
    let mut engine = synced(&full);
    let cycle_of = |lock: &IsdLock| {
        let hash = BlockHash::from(*lock.cycle_hash.as_bytes());
        full.lookup(&hash).expect("ok").expect("a cycle").0
    };
    let (honest, core) = isdlocks().into_iter().find(|(l, _)| cycle_of(l) == 264).expect("lock B");
    assert!(core, "Core verified it");
    let mut lock = honest.clone();
    lock.cycle_hash = full.hash(288).as_bytes().to_owned().into();
    assert_eq!(cycle_of(&lock), 288);
    let at = |engine: &mut MasternodeListEngine, lock: &IsdLock, tip: u32| {
        let chain = Taller {
            chain: FixtureChain::full(),
            tip,
        };
        engine.verify_islock(lock, &chain, config()).map(|v| v.txid)
    };
    let c = 24;
    let last = 287 + config().h_keep();
    assert_eq!((full.tip, last), (311, 343));
    // The recent cycle: the second pass from tip − C.
    for tip in [full.tip, 288 + c] {
        assert_eq!(at(&mut engine, &lock, tip), Ok(lock.txid), "{tip}");
    }
    // The old cycle: the second pass from 287, to the horizon's edge.
    for tip in [288 + c + 1, last] {
        assert_eq!(at(&mut engine, &lock, tip), Ok(lock.txid), "{tip}");
        assert_eq!(at(&mut engine, &honest, tip), Ok(honest.txid), "{tip}");
    }
    let requests = engine.requests();
    for l in [&lock, &honest] {
        assert_eq!(
            at(&mut engine, l, last + 1),
            Err(LockOutcome::Rejected(LockReject::OutsideHorizon))
        );
    }
    assert_eq!(engine.requests(), requests);
    // Without the second pass's sets (stored at 284, the window's end) the first pass alone
    // does not verify it.
    let mut first_only = synced(&full);
    for h in 284..=287 {
        first_only.forget_list(BlockId(h));
    }
    assert_eq!(
        at(&mut first_only, &lock, full.tip),
        Err(LockOutcome::Rejected(LockReject::QuorumUnavailable))
    );
}

/// The height of Core's diff that mines the first ChainLock-type commitment, and the commitment.
fn first_chainlock_mine() -> (u32, Commitment) {
    let cl_type = llmq(CHAIN_LOCKS);
    (FIRST + 1..=FixtureChain::full().tip)
        .find_map(|h| {
            let mined = step_diff(h).new_quorums.into_iter().find(|qc| qc.llmq_type == cl_type);
            mined.map(|qc| (h, qc))
        })
        .expect("a ChainLock-type commitment mined")
}

/// PLAN SB11 row item 3, step 5 (D52, slice 8): Core's diff that mines the first ChainLock-type
/// commitment, applied over its work block's list with the members' operator keys rotated (an
/// input step 5 reads, which the diff does not carry), is accepted and its set kept; the
/// commitment is `Unverified`. Over Core's list it verifies (the control). A changed commitment
/// itself never reaches step 5: the quorums root refuses the diff (step 4).
#[test]
fn a_commitment_failing_step_5_is_unverified_and_its_diff_kept() {
    let full = FixtureChain::full();
    let (mined, qc) = first_chainlock_mine();
    let base =
        full.lookup(&BlockHash::from(*qc.quorum_hash.as_bytes())).expect("ok").expect("a base");
    let work = base.0 - WORK_DIFF_DEPTH;
    let before = FixtureChain::anchored(0, mined - 1);
    for rotated in [false, true] {
        let mut engine = applied(&before, FIRST, FIRST + 1..=mined - 1);
        let held = engine.list(BlockId(work)).expect("the work block's list, whole").clone();
        if rotated {
            let mut entries = held.entries().to_vec();
            let keys: Vec<_> = entries.iter().map(|e| e.operator_key).collect();
            for (i, e) in entries.iter_mut().enumerate() {
                e.operator_key = keys[(i + 1) % keys.len()];
            }
            let d = step_diff(work);
            let coinbase = CoinbaseFacts::First(Arc::new(Tx::new(d.cb_tx)));
            engine.hold(
                BlockId(work),
                work,
                full.hash(work),
                SmlList::from_entries(entries),
                coinbase,
            );
        }
        let at = FixtureChain::anchored(0, mined);
        engine.apply_diff(&step_diff(mined), &at, config()).expect("the diff is accepted");
        assert!(engine.has_authenticated_set(BlockId(mined)), "its set kept");
        let state = state(&engine, CHAIN_LOCKS, qc.quorum_hash);
        if rotated {
            assert_eq!(state, QuorumState::Unverified);
        } else {
            assert!(matches!(state, QuorumState::Verified(_)), "{state:?}");
        }
    }
}

/// PLAN SB11 row item 9e (D52, slice 8): the first commitment mined, from genesis. The lists are
/// applied one block at a time as the header tip reaches each, until Core's diff that mines the
/// first ChainLock-type commitment. Before it the type's sets are empty: a ChainLock whose scan
/// reads the last of them (Core's first captured lock, moved there) gets `NoQuorum` and asks for
/// nothing. The diff mines the quorum, `Verified` by step 5 as it applies. Coverage passes with it
/// in the domain's newer end, and counts it: with both ends holding it, a floor above its work
/// block leaves it uncovered. The engine, stepped on, verifies Core's lock at its own height.
#[test]
fn the_first_commitment_mined_from_genesis_verifies_and_covers() {
    let full = FixtureChain::full();
    let cl_type = llmq(CHAIN_LOCKS);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    let step = |engine: &mut MasternodeListEngine, h: u32| {
        let chain = FixtureChain::anchored(0, h);
        engine.apply_diff(&step_diff(h), &chain, config()).unwrap_or_else(|e| panic!("{h}: {e}"));
    };
    let start = FixtureChain::anchored(0, FIRST);
    engine.apply_diff(&genesis_diff(FIRST), &start, config()).expect("Core's diff");
    let (mined, first) = first_chainlock_mine();
    let hash = first.quorum_hash;
    for h in FIRST + 1..mined {
        step(&mut engine, h);
    }
    assert!(engine.quorum(cl_type, &hash).is_none());

    let (core, _) = clsigs().into_iter().min_by_key(|(cl, _)| cl.height).expect("one");
    let locked = u32::try_from(core.height).expect("a height");
    let early_height = mined - 1 + SIGN_HEIGHT_OFFSET;
    let mut early = core.clone();
    early.height = i32::try_from(early_height).expect("small");
    early.block_hash = full.hash(early_height);
    let at = FixtureChain::anchored(0, early_height);
    assert_eq!(
        engine.verify_chainlock(&early, &at, config()),
        Err(LockOutcome::Rejected(LockReject::NoQuorum)),
        "the set at {} is empty",
        mined - 1
    );
    assert!(engine.requests().is_empty());

    step(&mut engine, mined);
    assert!(matches!(state(&engine, CHAIN_LOCKS, hash), QuorumState::Verified(_)));
    let newer = mined + SIGN_HEIGHT_OFFSET;
    for h in mined + 1..=newer {
        step(&mut engine, h);
    }
    let tip = FixtureChain::anchored(0, newer);
    assert_eq!(engine.coverage(&tip, config()), Ok(Coverage::Covered));
    assert!(engine.requests().is_empty());
    // Coverage reads it: with the domain's oldest end just above its work block, both ends hold
    // it and a floor above that block leaves it uncovered.
    let base = full.lookup(&BlockHash::from(*hash.as_bytes())).expect("ok").expect("a base").0;
    let work = base - WORK_DIFF_DEPTH;
    let deep = work + 1 + config().h_keep();
    for h in newer + 1..=deep {
        step(&mut engine, h);
    }
    assert_eq!(
        engine.coverage(&FixtureChain::anchored(work, deep), config()),
        Ok(Coverage::Covered)
    );
    assert_eq!(
        engine.coverage(&FixtureChain::anchored(work + 1, deep), config()),
        Ok(Coverage::Below {
            lowest: Some(work)
        })
    );

    for h in deep + 1..=locked {
        step(&mut engine, h);
    }
    let at = FixtureChain::anchored(0, locked);
    let v = engine.verify_chainlock(&core, &at, config()).expect("Core's verdict");
    assert_eq!((v.height, v.block_hash), (core.height, core.block_hash));
}

/// Core's reply to each request `engine` lists that the capture answers: a diff from genesis to a
/// null-based target, Core's step diffs from a base to its target, and a cycle's `qrinfo` (asked
/// at a block of the cycle, with no base). How many it answered.
fn answer_from_capture(engine: &mut MasternodeListEngine, chain: &FixtureChain) -> usize {
    let from_genesis: Vec<u32> = fixture()["diffs"]["from_genesis"]
        .as_array()
        .expect("diffs")
        .iter()
        .map(|d| u32_of(&d["height"]))
        .collect();
    let mut answered = 0;
    for r in engine.requests() {
        match r {
            MnRequest::MnListDiff {
                base: None,
                target_height,
                ..
            } if from_genesis.contains(&target_height) => {
                engine.apply_diff(&genesis_diff(target_height), chain, config()).expect("Core's");
            }
            MnRequest::MnListDiff {
                base: Some(base),
                target_height,
                ..
            } => {
                let from = chain.lookup(&base).expect("ok").expect("a block").0;
                for h in from + 1..=target_height {
                    engine.apply_diff(&step_diff(h), chain, config()).expect("Core's");
                }
            }
            MnRequest::QrInfo {
                request_height,
                ..
            } => {
                let cycle = request_height - request_height % 24;
                let Some(q) = fixture()["qrinfos"].as_array().expect("qrinfos").iter().find(|q| {
                    let h = u32_of(&q["request_height"]);
                    h - h % 24 == cycle && q["base_heights"].as_array().is_some_and(Vec::is_empty)
                }) else {
                    continue;
                };
                engine.apply_qrinfo(&decode(&q["payload"]), chain, config()).expect("Core's");
            }
            _ => continue,
        }
        answered += 1;
    }
    answered
}

/// PLAN SB11 row item 8d (SB0 §5.2.1, D52, slice 8): a fresh start on the new side of each
/// boundary fetches the earlier set once and verifies. The fresh engine holds Core's diff from
/// genesis to the tip, 311, past the ChainLock-type mine at 299 and the rotated cycle's start at
/// 288. Core's ChainLock for 288 selects at 280, before the mine: that block is the work block of
/// the quorum based at 288, so step 5's request for it serves the lock too (a lock asking for its
/// own set is lock B's, and 8a's after its set is dropped). Lock B, of the cycle at 264 and
/// delivered past its end, signs at 287, before the cycle boundary, with Core's `qrinfo` at the
/// tip as a fresh start takes it. Each check asks once however often it runs, nothing is asked
/// twice, and the lock verifies once Core's replies to the listed requests arrive.
#[test]
fn a_fresh_start_past_a_boundary_fetches_the_earlier_set_once() {
    let chain = FixtureChain::full();
    let fresh = |rotated: bool| {
        let mut engine = MasternodeListEngine::new(Mode::Client);
        engine.apply_diff(&genesis_diff(chain.tip), &chain, config()).expect("Core's diff");
        if rotated {
            engine.apply_qrinfo(&qrinfo(chain.tip, &[]), &chain, config()).expect("Core's qrinfo");
        }
        engine
    };
    let (cl, _) = clsigs().into_iter().find(|(cl, _)| cl.height == 288).expect("captured");
    let b = fixture()["provenance"]["locks"]
        .as_array()
        .expect("locks")
        .iter()
        .find(|l| l["name"] == "B")
        .map(|l| TxHash::from(rpc(l["txid"].as_str().expect("a txid"))))
        .expect("lock B");
    let (lock, _) = isdlocks().into_iter().find(|(l, _)| l.txid == b).expect("captured");
    for rotated in [false, true] {
        let mut engine = fresh(rotated);
        let verifies = |e: &mut MasternodeListEngine| {
            if rotated {
                e.verify_islock(&lock, &chain, config()).is_ok()
            } else {
                e.verify_chainlock(&cl, &chain, config()).is_ok()
            }
        };
        let before = engine.requests();
        assert!(!verifies(&mut engine), "rotated {rotated}: the set is not held");
        let asked = engine.requests();
        if rotated {
            assert!(asked.len() > before.len(), "the lock asks for its set");
        } else {
            // The lock's set is the work block of the quorum based at 288, which step 5 asked for
            // already: one request serves both.
            let s = u32::try_from(cl.height).expect("a height") - SIGN_HEIGHT_OFFSET;
            assert_eq!(asked, before);
            assert!(asked.iter().any(|r| r.key() == RequestKey::MnListDiff(chain.hash(s))));
        }
        assert!(!verifies(&mut engine));
        assert_eq!(engine.requests(), asked, "rotated {rotated}: asked once");
        let mut seen = HashSet::new();
        let mut rounds = 0;
        while !verifies(&mut engine) {
            rounds += 1;
            assert!(rounds <= 3, "rotated {rotated}: {:?}", engine.requests());
            for r in engine.requests() {
                assert!(seen.insert(r.key()), "rotated {rotated}: {r:?} asked again");
            }
            assert!(answer_from_capture(&mut engine, &chain) > 0);
        }
    }
}
