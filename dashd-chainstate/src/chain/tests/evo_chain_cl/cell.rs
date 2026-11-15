//! CH7b-3 C3's and C3b's tests that read step 10's cell itself (behavioural controls: the cell
//! is new in C3), each against the mutant its plan row names.

use std::sync::atomic::Ordering;

use dash_consensus::BlockId;
use dash_consensus::ctx::bls_ctx_for_block;
use dash_pkc::bls::BlsGlobalSig;

use super::super::failure::tip;
use super::super::support::Mined;
use super::{Run, cbtx_sig, nulled, remined, with_diff};
use crate::evo::cl::CbTxClCell;

impl Run {
    fn cell(&self) -> CbTxClCell {
        self.facade.cbtx_cl().lock().clone()
    }

    /// The cell `m`'s CbTx writes for the index entry `id`.
    fn cell_of(&self, id: BlockId, m: &Mined, diff: u32) -> CbTxClCell {
        let sig = BlsGlobalSig::decode(&cbtx_sig(m)).expect("readable");
        CbTxClCell {
            entry: Some((id, sig.get(bls_ctx_for_block(&self.p, m.height)), diff)),
        }
    }
}

/// CH7b-3 C3: the cell after the run's own passes holds each block's ChainLock and difference.
#[test]
fn the_cell_holds_the_passing_cbtxs_chainlock() {
    let run = Run::new();
    run.connect_to(308);
    assert_eq!(run.cell(), CbTxClCell::default(), "null ChainLocks write nothing");
    run.rig.accept(run.at(309));
    assert_eq!(run.cell(), run.cell_of(run.id(309), run.at(309), 0));
    run.rig.accept(run.at(310));
    assert_eq!(run.cell(), run.cell_of(run.id(310), run.at(310), 1));
}

/// CH7b-3 C3 (K-4): step 10's own rejects leave the cell where the last pass wrote it.
#[test]
fn step_10_rejects_leave_the_cell() {
    let run = Run::new();
    run.connect_to(309);
    let at_309 = run.cell_of(run.id(309), run.at(309), 0);
    // `stm:176`: three is more than one block older than 309's 0.
    let older = with_diff(&run.p, run.at(310), 3);
    assert_eq!(run.submit(&older), "bad-cbtx-older-clsig");
    assert_eq!(run.cell(), at_309);
    // `stm:202-205`: the verify fails.
    run.valid.store(false, Ordering::SeqCst);
    assert_eq!(run.submit(run.at(310)), "bad-cbtx-invalid-clsig");
    assert_eq!(run.census(), [(309, 308), (310, 308)]);
    assert_eq!(run.cell(), at_309);
}

/// CH7b-3 C3 (K-5): a disconnect neither clears nor rewrites the cell.
#[test]
fn undo_leaves_the_cell() {
    let run = Run::new();
    run.connect_to(310);
    let at_310 = run.cell_of(run.id(310), run.at(310), 1);
    assert_eq!(run.cell(), at_310);
    assert_eq!(run.rig.v.invalidate_block(run.at(310).hash()).recv(), Ok(Ok(())));
    assert_eq!(tip(&run.rig), run.id(309));
    assert_eq!(run.cell(), at_310);
}

/// CH7b-3 C3 (K-7): a cell another parent's CbTx wrote misses. With 310 disconnected, the cell is
/// still 310's; a nulled 310 on 309 reads 309, unreadable, so no ChainLock bounds it and it
/// connects, Core's miss (`cbtx:192-195`).
#[test]
fn a_cell_for_another_parent_misses() {
    let run = Run::new();
    run.connect_to(310);
    assert_eq!(run.rig.v.invalidate_block(run.at(310).hash()).recv(), Ok(Ok(())));
    run.unreadable(309);
    run.reads_of(309);
    let b = nulled(&run.p, run.at(310));
    assert_eq!(run.submit(&b), "ok");
    assert_eq!(tip(&run.rig), run.rig.id(&b));
    assert_eq!(run.reads_of(309), 1, "the parent's block, read");
    assert_eq!(run.cell(), run.cell_of(run.id(310), run.at(310), 1), "no write");
}

/// CH7b-3 C3 (K-6; Sol r1 #1, OD-CP-39): step 10 writes the cell in the attempt, outside the evo
/// transaction, so an attempt that fails after it keeps the write. B, 310 paying one duff more
/// than allowed, passes step 10 (its verify) and step 11 and then gets `bad-cb-amount`
/// (`v:2857-2860` at v24.0.0, after `ProcessSpecialTxsInBlock` at `v:2674-2675` at v24.0.0): the
/// cell is B's. B′, a nulled 310, finds a cell for another parent, reads A = 309, unreadable, and
/// connects.
#[test]
fn a_later_reject_keeps_the_cells_write() {
    let run = Run::new();
    run.connect_to(309);
    run.census();
    let b = remined(&run.p, run.at(310), |cb| {
        let mut outputs = cb.outputs().clone();
        outputs[0] = dash_primitives::TxOut::from_duffs(
            outputs[0].value() + 1,
            outputs[0].script_pubkey().clone(),
        );
        *cb = dash_primitives::Transaction::new(
            cb.version(),
            cb.tx_type(),
            cb.inputs().clone(),
            outputs,
            cb.lock_time(),
            cb.extra_payload().clone(),
        );
    });
    let before = run.counts.root_checks().steps.len();
    assert_eq!(run.submit(&b), "bad-cb-amount");
    assert_eq!(run.census(), [(310, 308)], "B passed its verify");
    assert_eq!(run.counts.root_checks().steps[before..], [(310, 10), (310, 11)]);

    run.unreadable(309);
    let b2 = nulled(&run.p, run.at(310));
    assert_eq!(run.submit(&b2), "ok", "B′ connects: the cell is not A's");
    assert_eq!(tip(&run.rig), run.rig.id(&b2));
    let b_id = run.rig.id(&b);
    let kept = "B's write kept, and B′'s null lock writes nothing";
    assert_eq!(run.cell(), run.cell_of(b_id, &b, 1), "{kept}");

    // The control: without B the cell is A's, and B′ meets A's ChainLock in it.
    let ctl = Run::new();
    ctl.connect_to(309);
    ctl.unreadable(309);
    assert_eq!(ctl.submit(&nulled(&ctl.p, ctl.at(310))), "bad-cbtx-null-clsig");
}

/// CH7b-3 C3 (K-9; Opus r1 m-1): the cell lives for one open, as Core's statics
/// (`stm:147-149`) live for one process. The node stops at 309 with the cell 309's and restarts
/// on its stores: its new facade's cell is empty, so a nulled 310, with 309 unreadable, reads
/// 309's block once, finds no ChainLock and connects (`stm:162-166`). Without the restart the
/// cell answers instead and 310 meets 309's ChainLock: `bad-cbtx-null-clsig`
/// (`a_cell_hit_is_the_previous_chainlock`).
#[test]
fn a_restart_starts_with_an_empty_cell() {
    let mut run = Run::new();
    run.connect_to(309);
    assert_eq!(run.cell(), run.cell_of(run.id(309), run.at(309), 0), "309's verify wrote it");
    run.reopen(309);
    run.census();
    run.unreadable(309);
    run.reads_of(309);
    let b = nulled(&run.p, run.at(310));
    assert_eq!(run.submit(&b), "ok", "no ChainLock bounds it");
    assert_eq!(tip(&run.rig), run.rig.id(&b));
    assert_eq!(run.reads_of(309), 1, "the parent's block, read once");
    assert!(run.census().is_empty(), "a null lock is not verified");
    assert_eq!(run.cell(), CbTxClCell::default(), "the new cell, empty, and B's null writes none");
}

/// CH7b-3 C3b (K-8, K-8b; OD-CP-42; `v:4955-4958` at v24.0.0): a proposal that passes step 10
/// writes as Core's does, under its dummy index, which no later block's parent can be: the cell is
/// cleared, keeping neither a `DUMMY_ID` entry nor 309's. The block proposed, connected next,
/// then misses the cell and reads 309's block, where without the proposal it hits the cell.
#[test]
fn a_proposal_clears_the_cell() {
    let ctl = Run::new();
    ctl.connect_to(309);
    ctl.reads_of(309);
    ctl.rig.accept(ctl.at(310));
    assert_eq!(ctl.reads_of(309), 0, "the control: 310 hits 309's cell");

    let run = Run::new();
    run.connect_to(309);
    run.census();
    let block = run.at(310).block.clone();
    let tested = run.rig.v.test_block_validity(block, true, true).recv().expect("V replies");
    assert!(tested.as_ref().is_ok_and(|state| state.is_valid()), "{tested:?}");
    assert_eq!(run.census(), [(310, 308)], "the proposal passed step 10 with a verify");
    assert_eq!(run.cell(), CbTxClCell::default(), "the proposal's write clears the cell");
    run.reads_of(309);
    run.rig.accept(run.at(310));
    assert_eq!(run.reads_of(309), 1, "310 takes the store path");
    assert_eq!(run.census(), [(310, 308)]);
    assert_eq!(run.cell(), run.cell_of(run.id(310), run.at(310), 1));
}

/// MN0-2b-2's proposal sweep, row P6 (PLAN-MN0 §10.10 item 3): a proposal whose parent block is
/// unreadable, cold (a restarted V's empty cell). Core's `GetBlockTxs` read gives `nullopt`
/// (rc.5 `cbtx.cpp:192-195`), read as "the parent had no ChainLock" (CH7b-3 plan §3.3 C3 step 4;
/// `a_cell_hit_is_the_previous_chainlock` is the warm control): no fault, no report, the null
/// ChainLock passes and V runs on; the proposal leaves no cell entry, and the next block
/// connects.
#[test]
fn a_proposal_over_an_unreadable_parent_reads_no_chainlock() {
    let mut run = Run::new();
    run.connect_to(309);
    run.reopen(309);
    run.census();
    run.unreadable(309);
    run.reads_of(309);
    let b = nulled(&run.p, run.at(310));
    let tested = run.rig.v.test_block_validity(b.block.clone(), true, true).recv().expect("V");
    assert!(tested.as_ref().is_ok_and(|state| state.is_valid()), "{tested:?}");
    assert_eq!(run.reads_of(309), 1, "the parent's block, read once");
    assert_eq!(run.cell(), CbTxClCell::default(), "no cell entry");
    assert!(!run.kept.tripped(), "no latch");
    run.kept.assert_no_abort(&run.rig);
    run.rig.accept(run.at(310));
    assert_eq!(tip(&run.rig), run.id(310));
    run.kept.assert_no_abort(&run.rig);
}
