//! BL-564's census counters (CH7b-2 C8) through `NodeEvo`'s H4 on a real V: each row is one
//! hook call, its verdict and how far each of the five counts moved, as `evo::edges` places
//! them. Regtest with DIP0003 at [`DIP3`] and V20 two blocks later; every row's height is before
//! the first LLMQ mining window, so an empty body needs no commitment.

use std::sync::{Arc, Mutex};

use dash_consensus::coins::CoinsView;
use dash_consensus::primitives::{Header, TxRef};
use dash_consensus::tx::TRANSACTION_MNHF_SIGNAL;
use dash_consensus::{Block, BlockId, BlsCtx, Tx, TxValidationError};
use dash_dml::specialtx::QuorumSeams;
use dash_dml::{EvoDelta, MnList};
use dash_primitives::{BlockHeader, CoinbaseCommitment, TxType};
use dashd_store::MemoryEngine;

use super::node_evo::{
    at, attempt_facade, connect_on, core_pro_up_serv, node_evo_on, special_cbtx, tx, undo_block,
};
use super::payloads::{DIP3, regtest, special};
use super::support::{Mined, Rig, RigConfig, branch, with_tip_cx};
use crate::chain::{ConnectCx, Fatal, VContext, ValidationSignals};
use crate::connect::{BlockAt, SpecialTxFailure};
use crate::evo::ev1::{DmlChecks, Ev1Checks, SpecialTxCtx};
use crate::evo::gates::EvoEdgeCounts;
use crate::evo::node::{NodeEvo, PROPOSAL_PAST_DIP3};
use crate::evo::testkit;
use crate::store::CoinsReadError;

/// V20, and MN_RR with it: the block after the first past DIP0003.
const V20: u32 = DIP3 + 2;

/// [`DmlChecks`], whose step 4 passes MNHF signals and service updates: their checks need a
/// quorum's signature or a registered masternode, which the fixture does not build, and the rows
/// are about steps 6, 7 and 11.
struct PassSignals;

impl Ev1Checks for PassSignals {
    type Payloads<'b> = <DmlChecks as Ev1Checks>::Payloads<'b>;

    fn payloads<'b>(&self, block: &'b Block) -> Self::Payloads<'b> {
        DmlChecks.payloads(block)
    }

    fn check_cbtx(
        &self,
        block: &Block,
        payloads: &Self::Payloads<'_>,
        cx: SpecialTxCtx<'_>,
    ) -> Result<Result<CoinbaseCommitment, SpecialTxFailure>, Fatal> {
        DmlChecks.check_cbtx(block, payloads, cx)
    }

    fn check_tx<'a>(
        &self,
        block: &Block,
        payloads: &Self::Payloads<'_>,
        i: usize,
        cx: SpecialTxCtx<'a>,
        quorum: Option<QuorumSeams<'a>>,
    ) -> Result<Result<(), SpecialTxFailure>, Fatal> {
        let passed = block.txs().get(i).is_some_and(|tx| {
            matches!(tx.tx_type(), TxType::MnhfSignal | TxType::ProviderUpdateService)
        });
        if passed {
            return Ok(Ok(()));
        }
        DmlChecks.check_tx(block, payloads, i, cx, quorum)
    }

    fn shared_collateral_spends(
        &self,
        tx: &Tx,
        view: &dyn CoinsView<Error = CoinsReadError>,
    ) -> Result<Result<(), TxValidationError>, Fatal> {
        DmlChecks.shared_collateral_spends(tx, view)
    }
}

type Evo = NodeEvo<MemoryEngine, PassSignals>;

/// An MNHF signal for `bit` (`MNHFTxPayload` version 1, quorum `[7; 32]`, a null signature).
/// No regtest deployment uses bits 5 and 6, so either is a valid activation (`mnhftx.cpp` quirk
/// 31: an unknown bit is).
fn signal(bit: u8) -> TxRef {
    let mut payload = vec![1, bit];
    payload.extend([7; 32]);
    payload.extend([0; 96]);
    special(TRANSACTION_MNHF_SIGNAL, payload)
}

/// A classical coinbase, unless `txs` brings its own, then `txs`.
fn body(txs: Vec<TxRef>) -> Block {
    let mut all = vec![tx(1, 0, true, 0)];
    if txs.first().is_some_and(|t| t.is_coinbase()) {
        all.clear();
    }
    all.extend(txs);
    Block::new(Header::new(BlockHeader::default()), all)
}

/// How H4 is called.
#[derive(Clone, Copy, Debug)]
enum Mode {
    /// Without the CbTx checks, rolled back.
    Block,
    /// With them, rolled back.
    Checked,
    /// Without them, committed.
    Commit,
    /// `fJustCheck` without an index entry (`TestBlockValidity`).
    Proposal,
}

/// The five counts in `EvoEdgeCounts`' order: `w.quorum`, `g.quorum`, `g.dmn`, `w.mnhf`, `g.mnhf`.
fn five(c: EvoEdgeCounts) -> [u64; 5] {
    [c.w_quorum, c.g_quorum, c.g_dmn, c.w_mnhf, c.g_mnhf]
}

/// The chain and the hooks, with DIP0003's block committed over Core's empty list as a connect
/// stores it, so the rows past DIP0003 have a parent list.
struct Rows<'r> {
    blocks: &'r [Mined],
    ids: &'r [BlockId],
}

impl Rows<'_> {
    fn at(&self, h: u32, proposal: bool) -> BlockAt {
        let n = h as usize - 1;
        let at = at(&self.blocks[n], self.ids[n], self.ids[n - 1]);
        BlockAt {
            id: (!proposal).then_some(at.id).flatten(),
            ..at
        }
    }

    /// One H4 call: its verdict, and how far each count moved.
    fn h4<C: Ev1Checks + Send>(
        &self,
        evo: &mut NodeEvo<MemoryEngine, C>,
        (vcx, cx): (&mut VContext, &ConnectCx<'_>),
        h: u32,
        block: &Block,
        mode: Mode,
    ) -> Result<(String, [u64; 5]), Fatal> {
        let (just_check, check, commit) = match mode {
            Mode::Block => (false, false, false),
            Mode::Checked => (false, true, false),
            Mode::Commit => (false, false, true),
            Mode::Proposal => (true, false, false),
        };
        let at = self.at(h, matches!(mode, Mode::Proposal));
        let before = five(evo.edge_counts());
        let facade = attempt_facade(cx.params);
        let got = connect_on(evo, &facade, vcx, cx, block, &at, just_check, check, commit)?;
        let verdict = match got {
            Ok(Ok(_)) => "connects".to_owned(),
            Ok(Err(SpecialTxFailure::NotReady(stub))) => format!("stub: {stub}"),
            Ok(Err(SpecialTxFailure::Invalid(e))) => e.reject_reason().to_owned(),
            other => format!("{other:?}"),
        };
        let after = five(evo.edge_counts());
        Ok((verdict, std::array::from_fn(|i| after[i] - before[i])))
    }

    /// One H9 call on `block` at `h`: whether it undid it, and how far each count moved.
    fn h9<C: Ev1Checks + Send>(
        &self,
        evo: &mut NodeEvo<MemoryEngine, C>,
        (vcx, cx): (&mut VContext, &ConnectCx<'_>),
        h: u32,
        block: &Block,
    ) -> Result<(String, [u64; 5]), Fatal> {
        let before = five(evo.edge_counts());
        let parent = self.blocks[h as usize - 2].hash();
        let undone = undo_block(evo, vcx, cx, block, &self.at(h, false), &parent)?;
        let after = five(evo.edge_counts());
        let verdict = format!("undone {undone:?}");
        Ok((verdict, std::array::from_fn(|i| after[i] - before[i])))
    }
}

/// `f` on V over a fresh `NodeEvo` over `ev1` and the chain to `V20 + 1`, DIP0003's block
/// committed.
fn run<C: Ev1Checks + Send + 'static, T: Send + 'static>(
    ev1: C,
    f: impl FnOnce(
        &Rows<'_>,
        &mut NodeEvo<MemoryEngine, C>,
        (&mut VContext, &ConnectCx<'_>),
    ) -> Result<T, Fatal>
    + Send
    + 'static,
) -> T {
    let moved = [format!("v20@{V20}"), format!("mn_rr@{V20}")];
    let p = Arc::new(regtest(&moved.each_ref().map(String::as_str)));
    let blocks = branch(&p, &Mined::genesis(&p), V20 + 1, 0);
    let rig = Rig::with(RigConfig {
        params: Some(p.clone()),
        ..RigConfig::default()
    });
    rig.headers(&blocks);
    let ids = rig.ids(&blocks);
    let out = Arc::new(Mutex::new(None));
    let put = out.clone();
    rig.on_v(move |v| {
        let signals = ValidationSignals::default();
        let (mut evo, layer, store) = node_evo_on(&p, BlsCtx::BASIC, ev1, &signals);
        let dip3 = &blocks[DIP3 as usize - 1];
        let empty = MnList::default_pre_dip3();
        let height = i32::try_from(dip3.height).expect("small");
        let list = empty.clone().with_block_hash(dip3.hash()).with_height(height);
        let delta = EvoDelta {
            block: dip3.hash(),
            height: dip3.height,
            dml: Some(testkit::dml(&empty, &list, true)),
            mined_commitments: Vec::new(),
            credit_data: None,
            credit_pool: None,
            mnhf_signals: None,
            erased_mined: Vec::new(),
            evo_tip: Some(dip3.hash()),
        };
        store.commit(&mut layer.lock(), &delta).map_err(|e| Fatal::new(e.to_string()))?;
        let rows = Rows {
            blocks: &blocks,
            ids: &ids,
        };
        let r = with_tip_cx(v, |vcx, cx| f(&rows, &mut evo, (vcx, cx)))?;
        *put.lock().expect("out") = Some(r);
        Ok(())
    });
    let r = out.lock().expect("out").take().expect("V ran it");
    rig.assert_no_abort();
    r
}

/// The plan's rows (C8): each count moves where Core's call is made, whatever follows it.
/// - Below DIP0003 both wrappers count and no body does, a proposal included (`w.mnhf` is
///   counted before the proposal skips MNHF).
/// - Step 4's reject is before every edge.
/// - Steps 7 and 8 reject a block that entered steps 6 and 7 but never reached step 11:
///   `BuildNewListFromBlock`'s `bad-protx-hash` still counts `g.dmn`.
/// - A proposal past DIP0003 stops at step 6 (`PROPOSAL_PAST_DIP3`, X7) after `g.quorum`.
/// - `g.mnhf` counts a block whose signals extract, before `bad-mnhf-duplicate`, and not one
///   whose two signals share a bit (`bad-mnhf-duplicates-in-block`).
/// - H9 counts nothing; the reconnect counts as any block.
#[test]
fn h4_counts_each_edge_on_entry() {
    let rows = run(PassSignals, |rows, evo, (vcx, cx)| {
        let mut seen = Vec::new();
        let mut row = |evo: &mut Evo,
                       vcx: &mut VContext,
                       h: u32,
                       txs: Vec<TxRef>,
                       mode: Mode|
         -> Result<(), Fatal> {
            seen.push(rows.h4(evo, (vcx, cx), h, &body(txs), mode)?);
            Ok(())
        };
        let h = DIP3 + 1;
        row(evo, vcx, 10, vec![], Mode::Block)?;
        row(evo, vcx, 10, vec![], Mode::Proposal)?;
        row(evo, vcx, h, vec![special(1, vec![1, 0])], Mode::Block)?;
        row(evo, vcx, h, vec![core_pro_up_serv()], Mode::Block)?;
        row(evo, vcx, h, vec![special_cbtx(2, h, 1, 0)], Mode::Checked)?;
        row(evo, vcx, h, vec![], Mode::Proposal)?;
        row(evo, vcx, h, vec![], Mode::Commit)?;
        row(evo, vcx, V20, vec![signal(5)], Mode::Commit)?;
        row(evo, vcx, V20 + 1, vec![signal(5)], Mode::Block)?;
        row(evo, vcx, V20 + 1, vec![signal(6), signal(6)], Mode::Block)?;
        let last = body(vec![]);
        seen.push(rows.h4(evo, (vcx, cx), V20 + 1, &last, Mode::Commit)?);
        seen.push(rows.h9(evo, (vcx, cx), V20 + 1, &last)?);
        seen.push(rows.h4(evo, (vcx, cx), V20 + 1, &last, Mode::Commit)?);
        Ok(seen)
    });
    let row = |verdict: &str, counts: [u64; 5]| (verdict.to_owned(), counts);
    let want = [
        row("connects", [1, 0, 0, 1, 0]),
        row("connects", [1, 0, 0, 1, 0]),
        row("bad-protx-payload", [0; 5]),
        row("bad-protx-hash", [1, 1, 1, 0, 0]),
        row("bad-cbtx-mnmerkleroot", [1, 1, 1, 0, 0]),
        row(&format!("stub: {PROPOSAL_PAST_DIP3}"), [1, 1, 0, 0, 0]),
        row("connects", [1, 1, 1, 1, 0]),
        row("connects", [1, 1, 1, 1, 1]),
        row("bad-mnhf-duplicate", [1, 1, 1, 1, 1]),
        row("bad-mnhf-duplicates-in-block", [1, 1, 1, 1, 0]),
        row("connects", [1, 1, 1, 1, 0]),
        row("undone Ok(true)", [0; 5]),
        row("connects", [1, 1, 1, 1, 0]),
    ];
    assert_eq!(rows, want);
}

/// The counts belong to the `NodeEvo` and only grow: three committed blocks leave each wrapper
/// and body at three, and no attempt resets them.
#[test]
fn the_counts_only_grow() {
    let totals = run(PassSignals, |rows, evo, (vcx, cx)| {
        let mut totals = vec![five(evo.edge_counts())];
        for h in DIP3 + 1..=V20 + 1 {
            rows.h4(evo, (vcx, cx), h, &body(vec![]), Mode::Commit)?;
            totals.push(five(evo.edge_counts()));
        }
        Ok(totals)
    });
    assert_eq!(totals, [[0; 5], [1, 1, 1, 1, 0], [2, 2, 2, 2, 0], [3, 3, 3, 3, 0]]);
}

/// BL-564 (C8b): through L3's `Counting`, H4 counts its five keys in the cells
/// `Counting::registered` put under the data directory, so `counts_under` reads them back with
/// the EV1 seam's six. A block below DIP0003 moves the two wrappers and a DIP0003 block the
/// wrappers and the quorum and DMN bodies, beside step 4's `d.stx_hook` and `w.stx` for its
/// coinbase. A restart registers new cells, all eleven keys from zero together as the six always
/// did, and within each registration no count goes back (L3's M-3).
#[cfg(feature = "test-support")]
#[test]
fn the_edges_read_back_through_counts_under() {
    use crate::evo::gates::{Counting, KEYS, counts_under};

    let leg = std::path::Path::new("/ch7b2-c8b-test/leg");
    let read = move || counts_under(leg).expect("registered").snapshot();
    let mut loads = Vec::new();
    for load in ["load-0", "load-1"] {
        let counting = Counting::registered(&leg.join(load).join("regtest/chainstate"));
        loads.push(run(counting, move |rows, evo, (vcx, cx)| {
            let mut seen = vec![read()];
            rows.h4(evo, (vcx, cx), 10, &body(vec![]), Mode::Block)?;
            seen.push(read());
            rows.h4(evo, (vcx, cx), DIP3 + 1, &body(vec![]), Mode::Commit)?;
            seen.push(read());
            Ok(seen)
        }));
        assert!(counts_under(leg).is_none(), "dead with its hooks");
    }
    let counts = |pairs: &[(&'static str, u64)]| {
        let mut all: std::collections::BTreeMap<_, _> = KEYS.iter().map(|k| (*k, 0)).collect();
        all.extend(pairs.iter().copied());
        all
    };
    let one_load = [
        counts(&[]),
        counts(&[("d.stx_hook", 1), ("w.stx", 1), ("w.quorum", 1), ("w.mnhf", 1)]),
        counts(&[
            ("d.stx_hook", 2),
            ("w.stx", 2),
            ("w.quorum", 2),
            ("g.quorum", 1),
            ("g.dmn", 1),
            ("w.mnhf", 2),
        ]),
    ];
    assert_eq!(loads, [one_load.clone(), one_load]);
}
