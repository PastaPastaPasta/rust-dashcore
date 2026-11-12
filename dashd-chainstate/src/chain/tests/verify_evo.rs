//! CH5-1b (BL-581): production VerifyDB over recorded evo chains, real `NodeEvo` on the node's
//! connector (`evo_chain`'s recorded rig): dashd's masternode chain, `feature_asset_locks`, and
//! the V19 window's R1 (V19 at 212, legacy commitments below it and basic ones above)
//! and R2 (V20 and MN_RR at 100, V19 at 236, CbTx ChainLocks).

use std::sync::{Arc, Mutex};

use dash_consensus::{Block, BlsCtx};
use dash_evo::llmq::LlmqCommitment;
use dash_primitives::{BlockHash, SpecialPayload};
use dash_statedigest::Origin;

use super::evo_chain::{Kept, decode, linked, lookup_name, recorded_config_on};
use super::scratch_state_hash::{Log, Recorded, assert_scratch_matches, asset_locks, mn_chain};
use super::support::{Mined, Rig, RigConfig, install_accept_log, lines_on};
use super::v19_window::summary;
use super::v19_window::{R1_CHAIN, R1_SUMMARY, R2_CHAIN, R2_SUMMARY, last_time, params, scheme};
use crate::chain::{LocalShutdown, VerifyDbArgs, VerifyDbResult};

/// A V19-window recording's main chain: its summary's parameters, its blocks.
fn window_chain(summary_text: &str, chain: &str) -> Recorded {
    let s = summary(summary_text);
    let raw = decode(chain);
    let p = params(&s);
    let mocktime = last_time(&raw);
    let blocks = linked(&Mined::genesis(&p), raw);
    Recorded {
        p,
        mocktime,
        blocks,
    }
}

fn r1() -> Recorded {
    let r1 = window_chain(R1_SUMMARY, R1_CHAIN);
    assert_eq!(r1.blocks.len(), 259, "R1's M");
    r1
}

fn r2() -> Recorded {
    let r2 = window_chain(R2_SUMMARY, R2_CHAIN);
    assert_eq!(r2.blocks.len(), 290, "R2");
    r2
}

/// VerifyDB at `level` and `depth` through `verifychain`.
fn walk(rig: &Rig, level: i32, depth: i32) -> VerifyDbResult {
    rig.v
        .verify_db(VerifyDbArgs {
            level,
            depth,
        })
        .recv()
        .expect("V answers")
}

/// The non-null commitments `block` mines.
fn commitments(block: &Block) -> Vec<LlmqCommitment> {
    let txs = block.txs().iter().filter_map(|tx| match tx.payload() {
        Some(Ok(d)) => match d.payload() {
            SpecialPayload::QuorumCommitment(p) if !p.commitment().is_null() => {
                Some(p.commitment().clone())
            }
            _ => None,
        },
        _ => None,
    });
    txs.collect()
}

/// `chain`'s blocks connected in order on a rig writing to `log`: the rig, its stores and the tip
/// `C`s.
fn connected(chain: &Recorded, log: &Log, to: u32) -> (Rig, Kept) {
    let (rig, kept) = chain.rig(log);
    for m in &chain.blocks[..to as usize] {
        rig.accept(m);
    }
    kept.assert_no_abort(&rig);
    (rig, kept)
}

/// T1 on one chain: VerifyDB at (3,6), (4,6) and (4,0) over the whole connected chain is
/// `SUCCESS`, and each level-4 scratch `C` carries the block and the four evo state hashes of its
/// tip `C`. It returns the chain's non-null commitment count, the commitments the full-depth
/// walks undo and reconnect.
fn walks_reconnect_as_the_tip_did(chain: &Recorded) -> usize {
    let top = u32::try_from(chain.blocks.len()).expect("a height");
    let log = Log::default();
    let (rig, kept) = connected(chain, &log, top);
    let tip = log.connects(Origin::Tip);
    for (level, depth) in [(3, 6), (4, 6), (4, 0)] {
        let before = log.connects(Origin::Verifydb).len();
        assert_eq!(walk(&rig, level, depth), VerifyDbResult::Success, "({level}, {depth})");
        let scratch = log.connects(Origin::Verifydb).split_off(before);
        if level < 4 {
            assert!(scratch.is_empty(), "level 3 reconnects nothing");
            continue;
        }
        let from = match depth {
            0 => 1,
            d => top - u32::try_from(d).expect("a depth") + 1,
        };
        let heights: Vec<u32> = (from..=top).collect();
        assert_scratch_matches(&tip, &scratch, &heights);
    }
    kept.assert_no_abort(&rig);
    chain.blocks.iter().map(|m| commitments(&m.block).len()).sum()
}

/// T1 (CH5-1b): the production walk at levels 3 and 4 over every recorded evo chain. The
/// full-depth walks undo and reconnect every non-null commitment R1, R2 and `feature_asset_locks`
/// mined. Reconnect coverage only: whether the undo removed anything is T9's.
#[test]
fn the_walk_reconnects_each_recorded_chain_as_its_tip_connected() {
    assert_eq!(walks_reconnect_as_the_tip_did(&mn_chain()), 0);
    assert!(walks_reconnect_as_the_tip_did(&r1()) >= 10, "R1's, legacy and basic");
    assert!(walks_reconnect_as_the_tip_did(&r2()) > 0, "R2's");
    assert!(walks_reconnect_as_the_tip_did(&asset_locks()) > 0, "the asset-lock run's");
}

/// What Core's walk rolls back and T2 asserts unchanged by it: the active evo layer's best block
/// and rows, the active coins' best block, the published scheme, every block's status.
#[derive(Debug, PartialEq)]
struct RolledBack {
    evo_tip: Option<BlockHash>,
    layer: String,
    coins_best: Option<BlockHash>,
    scheme: BlsCtx,
    statuses: Vec<u32>,
}

fn rolled_back(rig: &Rig, kept: &Kept, blocks: &[Mined]) -> RolledBack {
    RolledBack {
        evo_tip: kept.evo_tip(),
        layer: kept.stored().1,
        coins_best: kept.coins_best(),
        scheme: scheme(rig),
        statuses: blocks.iter().map(|m| rig.status(m).bits()).collect(),
    }
}

/// V's `IsInitialBlockDownload()` latch now.
fn in_ibd(rig: &Rig) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    rig.on_v(move |v| {
        let _ = tx.send(v.is_ibd());
        Ok(())
    });
    rx.recv().expect("V answers")
}

/// Whether the mineable pool holds each of `qcs` for its quorum.
fn pooled(rig: &Rig, qcs: &[LlmqCommitment]) -> Vec<bool> {
    let pool = rig.v.query(|q| q.evo_view().mineable()).recv().expect("V serves");
    qcs.iter().map(|qc| pool.best_for(qc.llmq_type(), &qc.quorum_hash()).is_some()).collect()
}

/// T2 (CH5-1b): on R1's M at 258, the walk over the whole chain rolls back what Core's rolls
/// back, after level 3 alone and after level 4: the active evo layer, the coins' best block, the
/// published scheme and every block's status as they were, no `AbortNode`, and 259 connects
/// after. Of what Core does not roll back, its transitions: after level 3 the mineable pool holds
/// every commitment the walk undid (`UndoBlock`'s re-add, `llmq/blockprocessor.cpp:556` at
/// v24.0.0), after level 4 none (the reconnect's `ProcessCommitment` erases each, `:448-453`);
/// the IBD latch is not moved by level 3.
#[test]
fn the_walk_rolls_back_what_core_rolls_back() {
    let r1 = r1();
    let log = Log::default();
    let (rig, kept) = connected(&r1, &log, 258);
    let walked = &r1.blocks[..258];
    let qcs: Vec<LlmqCommitment> = walked.iter().flat_map(|m| commitments(&m.block)).collect();
    assert_eq!(qcs.len(), 10, "R1's non-null commitments below 259");
    let none = vec![false; qcs.len()];
    assert_eq!(pooled(&rig, &qcs), none, "before any walk");
    let before = rolled_back(&rig, &kept, walked);
    let ibd = in_ibd(&rig);

    assert_eq!(walk(&rig, 3, 0), VerifyDbResult::Success);
    assert_eq!(rolled_back(&rig, &kept, walked), before, "after level 3");
    assert_eq!(pooled(&rig, &qcs), vec![true; qcs.len()], "level 3 re-adds what it undid");
    assert_eq!(in_ibd(&rig), ibd, "level 3 leaves the latch");

    assert_eq!(walk(&rig, 4, 0), VerifyDbResult::Success);
    assert_eq!(rolled_back(&rig, &kept, walked), before, "after level 4");
    assert_eq!(pooled(&rig, &qcs), none, "level 4's reconnects erase each");
    kept.assert_no_abort(&rig);

    rig.accept(r1.at(259));
    assert_eq!(kept.evo_tip(), Some(r1.at(259).hash()), "259 connects after the walks");
    kept.assert_no_abort(&rig);
}

/// `UndoSpecialTxsInBlock`'s switch to legacy at V19's block (`evo/specialtxman.cpp:1065-1073`).
const US1: &str = "CSpecialTxProcessor::UndoSpecialTxsInBlock -- bls_legacy_scheme=1";
/// `ProcessSpecialTxsInBlock`'s switch back to basic (`:1043-1047`).
const PS0: &str = "CSpecialTxProcessor::ProcessSpecialTxsInBlock -- bls_legacy_scheme=0";
/// VerifyDB's guard restoring basic as the walk returns (`validation.cpp:162-167`).
const RV0: &str = "ScopedBLSLegacyScheme: reverted bls_legacy_scheme=0";
/// Core's summary line at the end of a walk.
const SUMMARY: &str = "Verification: No coin database inconsistencies in last ";

/// The lines V logged during a walk at `level` and `depth`, which must be `result`.
fn walk_lines(rig: &Rig, level: i32, depth: i32, result: VerifyDbResult) -> Vec<String> {
    let v = rig.v_thread();
    let mark = lines_on(v).len();
    assert_eq!(walk(rig, level, depth), result, "({level}, {depth})");
    lines_on(v).split_off(mark)
}

/// `lines`' BLS scheme lines.
fn scheme_lines(lines: &[String]) -> Vec<&str> {
    let scheme = |l: &&String| {
        (l.starts_with("ScopedBLSLegacyScheme:") || l.starts_with("CSpecialTxProcessor::"))
            && l.contains("bls_legacy_scheme=")
    };
    lines.iter().filter(scheme).map(String::as_str).collect()
}

fn position(lines: &[String], f: impl Fn(&str) -> bool) -> usize {
    lines.iter().position(|l| f(l)).unwrap_or_else(|| panic!("{lines:#?}"))
}

/// T5 (CH5-1b): R1's walk from 259 across V19 (212). Level 3: the undo of 212 switches to
/// legacy once, before the walk's summary, and the guard reverts to basic once after it. Level 4:
/// the reconnect of 212 switches back to basic before the summary, and the guard has nothing to
/// revert. A walk that stays above 212 logs no scheme line. The published scheme stays basic.
// Core: validation.cpp:162-167, :5056; evo/specialtxman.cpp:1043-1047, :1065-1073 at v24.0.0
#[test]
fn the_walk_across_v19_logs_cores_scheme_lines() {
    install_accept_log();
    let r1 = r1();
    let log = Log::default();
    let (rig, kept) = connected(&r1, &log, 259);
    assert_eq!(scheme(&rig), BlsCtx::BASIC);

    let l3 = walk_lines(&rig, 3, 0, VerifyDbResult::Success);
    assert_eq!(scheme_lines(&l3), [US1, RV0], "{l3:#?}");
    let summary = position(&l3, |l| l.starts_with(SUMMARY));
    assert!(position(&l3, |l| l == US1) < summary && summary < position(&l3, |l| l == RV0));
    assert_eq!(scheme(&rig), BlsCtx::BASIC, "after level 3");

    let l4 = walk_lines(&rig, 4, 0, VerifyDbResult::Success);
    assert_eq!(scheme_lines(&l4), [US1, PS0], "{l4:#?}");
    assert!(position(&l4, |l| l == PS0) < position(&l4, |l| l.starts_with(SUMMARY)));
    assert_eq!(scheme(&rig), BlsCtx::BASIC, "after level 4");

    let above = walk_lines(&rig, 4, 6, VerifyDbResult::Success);
    assert!(scheme_lines(&above).is_empty(), "{above:#?}");
    kept.assert_no_abort(&rig);
}

/// A state-hash sink that asks for the node's shutdown once it has recorded the VerifyDB attempt
/// of the kind `connect` names (a reconnect, or else an undo) at `height`, as a shutdown
/// requested between two of the walk's blocks.
struct ShutdownAfter {
    inner: Box<dyn crate::connect::DigestSink>,
    flag: Arc<LocalShutdown>,
    connect: bool,
    height: u32,
}

impl crate::connect::DigestSink for ShutdownAfter {
    fn record(
        &mut self,
        cx: &crate::chain::ConnectCx<'_>,
        attempt: &crate::connect::DigestAttempt<'_>,
    ) -> Result<(), crate::chain::Fatal> {
        use crate::chain::{AttemptOrigin, ShutdownFlag as _};
        use crate::connect::DigestAttempt;

        self.inner.record(cx, attempt)?;
        let (connect, at) = match attempt {
            DigestAttempt::Connect(c) => (true, c.at),
            DigestAttempt::Disconnect(d) => (false, d.at),
        };
        if cx.origin == AttemptOrigin::VerifyDb
            && connect == self.connect
            && at.height == self.height
        {
            self.flag.start_shutdown();
        }
        Ok(())
    }
}

/// R1 connected to `to` on a rig whose node shutdown is asked for after the walk's undo (or
/// reconnect, if `connect`) of `height`.
fn interrupted_r1(to: u32, connect: bool, height: u32) -> (Recorded, Rig, Kept) {
    let r1 = r1();
    let flag = Arc::new(LocalShutdown::default());
    let sink = ShutdownAfter {
        inner: Log::default().sink(),
        flag: flag.clone(),
        connect,
        height,
    };
    let cfg = RigConfig {
        params: Some(r1.p.clone()),
        shutdown: Some(flag),
        ..RigConfig::default()
    };
    let (cfg, kept) = recorded_config_on(cfg, r1.p.clone(), r1.mocktime, Some(Box::new(sink)));
    let rig = Rig::with(cfg);
    for m in &r1.blocks[..to as usize] {
        rig.accept(m);
    }
    kept.assert_no_abort(&rig);
    (r1, rig, kept)
}

/// T7 (a) (CH5-1b): a shutdown asked for between two evo blocks of the walk from 258, after level
/// 3's second undo (257) and, separately, after level 4's third reconnect (255): `INTERRUPTED`, no
/// VerifyDB line after the last block's (no summary, no skip), and T2's rolled-back state.
/// Startup's mapping of `INTERRUPTED` to "Block verification was interrupted" is
/// `verifydb_results_map_to_cores_statuses`'.
// Core: validation.cpp:5140, :5176-5178 at v24.0.0
#[test]
fn a_shutdown_between_evo_blocks_interrupts_the_walk() {
    install_accept_log();
    for (connect, height, level) in [(false, 257, 3), (true, 255, 4)] {
        let (r1, rig, kept) = interrupted_r1(258, connect, height);
        let walked = &r1.blocks[..258];
        let before = rolled_back(&rig, &kept, walked);
        let lines = walk_lines(&rig, level, 6, VerifyDbResult::Interrupted);
        let after_walk = |l: &String| l.starts_with(SUMMARY) || l.starts_with("Skipped verif");
        assert!(!lines.iter().any(after_walk), "level {level}: {lines:#?}");
        assert_eq!(rolled_back(&rig, &kept, walked), before, "level {level}");
        assert!(kept.reports().is_empty(), "{:?}", kept.reports());
    }
}

/// T7 (b) (CH5-1b): on R1, a shutdown asked for once the walk from 258 has undone 212, V19's
/// block, and 211, so the walk's scheme is legacy as it returns `INTERRUPTED`: the guard reverts
/// to basic with Core's line, and the published scheme afterwards is the one the walk entered
/// with.
// Core: validation.cpp:162-167, :5056, :5140 at v24.0.0
#[test]
fn an_interrupt_inside_the_v19_window_restores_the_entry_scheme() {
    install_accept_log();
    let (_r1, rig, kept) = interrupted_r1(258, false, 211);
    assert_eq!(scheme(&rig), BlsCtx::BASIC);
    let lines = walk_lines(&rig, 3, 0, VerifyDbResult::Interrupted);
    assert_eq!(scheme_lines(&lines), [US1, RV0], "{lines:#?}");
    assert_eq!(scheme(&rig), BlsCtx::BASIC, "the entry scheme");
    assert!(kept.reports().is_empty(), "{:?}", kept.reports());
}

/// T9 (ruling 3, CH5-1b): production `NodeEvo` on R1's M at 251, which mines two basic
/// commitments (`llmq_test` and `llmq_test_instantsend`); V's walk's scratch disconnect of 251
/// (`verify_scratch` over the tip, H9 and the commit as the walk runs them). Right after the undo,
/// before any reconnect, the scratch layer's live index is read for each commitment's
/// `llmq_mined` key and its `llmq_mined_h` height-index key, both derived from the commitment:
/// each is a tombstone there, while the active layer still holds every row.
// Core: llmq/blockprocessor.cpp:536-548 at v24.0.0
#[test]
fn the_scratch_undo_tombstones_the_mined_commitment_and_its_height_index() {
    use crate::chain::seams::{HeldChainLocks, ScratchParent};
    use crate::chain::{AttemptOrigin, ConnectCx, DisconnectResult, Fatal, ViewTarget};
    use crate::evo::codec::{index_key, mined_key};

    let r1 = r1();
    let log = Log::default();
    let (rig, kept) = connected(&r1, &log, 251);
    let m = r1.at(251).clone();
    let qcs = commitments(&m.block);
    assert_eq!(qcs.len(), 2, "two at 251");
    let keys: Vec<Vec<u8>> = qcs
        .iter()
        .flat_map(|qc| {
            let mined = mined_key(qc.llmq_type(), &qc.quorum_hash()).to_vec();
            [mined, index_key(qc.llmq_type(), None, 251).to_vec()]
        })
        .collect();
    for key in &keys {
        assert_eq!(kept.active_mined(key)[0], "live", "the active row before the undo");
    }
    let id = rig.id(&m);
    let read = Arc::new(Mutex::new(None));
    let put = read.clone();
    let probe = keys.clone();
    rig.on_v(move |v| {
        let mut view = v
            .connector
            .verify_scratch(ScratchParent::Tip)?
            .ok_or_else(|| Fatal::new("a connector without a scratch view"))?;
        let deployments = v.deployments.view();
        let is_ibd = v.chain.is_ibd_private(v.now(), true);
        let best = v.cl.sigs.best.clone();
        let chain = std::cell::RefCell::new(&mut v.chain);
        let best_header = chain.borrow().best_header();
        let chainlocks = HeldChainLocks::new(&chain, best);
        let cx = ConnectCx {
            params: &v.params,
            reader: &v.reader,
            deployments: &deployments,
            best_header,
            min_chain_work: v.reader.min_chain_work(),
            is_ibd,
            chainlocks: &chainlocks,
            guard: v.guard,
            origin: AttemptOrigin::VerifyDb,
        };
        let target = ViewTarget::Scratch(&mut *view);
        let undone = v.connector.disconnect_block(&cx, &mut v.vcx, target, &m.block, id)?;
        let (_, evo) = view.split();
        let layer = evo.layer().ok_or_else(|| Fatal::new("no scratch evo layer"))?;
        let (live, _) = layer.indexes();
        let levels: Vec<&str> = probe.iter().map(|k| lookup_name(live.mined(k))).collect();
        *put.lock().expect("read") = Some((undone, levels));
        Ok(())
    });
    let (undone, levels) = read.lock().expect("read").take().expect("V ran it");
    assert_eq!(undone, DisconnectResult::Ok);
    for (key, top) in keys.iter().zip(levels) {
        assert_eq!(top, "erased", "the scratch layer's live index at {key:02x?}");
        assert_eq!(kept.active_mined(key)[0], "live", "the active row after the undo");
    }
    kept.assert_no_abort(&rig);
}

/// `verifychain`'s answer at `level` and `depth` on `rig`, which is `replied`, then V stopped: its
/// exit, and the lines V logged from the call on.
fn walk_then_stop(
    mut rig: Rig,
    level: i32,
    depth: i32,
    replied: Result<VerifyDbResult, crate::chain::WaitError>,
) -> (crate::chain::VExit, Vec<String>) {
    let v = rig.v_thread();
    let mark = lines_on(v).len();
    assert_eq!(
        rig.v
            .verify_db(VerifyDbArgs {
                level,
                depth
            })
            .recv(),
        replied
    );
    let exit = rig.stop();
    (exit, lines_on(v).split_off(mark))
}

/// [`walk_then_stop`] where V stops in the walk.
fn walk_stopping(rig: Rig, level: i32, depth: i32) -> (crate::chain::VExit, Vec<String>) {
    walk_then_stop(rig, level, depth, Err(crate::chain::WaitError::Disconnected))
}

/// T3 (a) (V-c, CH5-1b): R1's M at 258 with the evo layer flushed and `meta.evo_tip` naming 257,
/// so the evo best block is not the tip. `verifychain`'s undo of 258 fails `DisconnectBlock`'s
/// EvoDB check (H2): exactly one `AbortNode`, Core's EvoDB text, which trips the node latch and
/// asks for the node's shutdown as Core's `StartShutdown`; the walk answers `CORRUPTED_BLOCK_DB`,
/// and its lines are Core's: the abort's, then VerifyDB's irrecoverable line for 258.
// Core: validation.cpp:2299-2302, :5126-5129 at v24.0.0
#[test]
fn an_evo_best_block_below_the_tip_aborts_the_node_once() {
    use crate::connect::EVODB_INCONSISTENCY;
    use crate::store::EVO_TIP;
    use dashd_store::DbSchema;

    install_accept_log();
    let r1 = r1();
    let log = Log::default();
    let (rig, kept) = connected(&r1, &log, 258);
    kept.flush_evo();
    kept.put_row(DbSchema::META, EVO_TIP, r1.at(257).hash().as_bytes());
    let (_, lines) = walk_then_stop(rig, 3, 6, Ok(VerifyDbResult::CorruptedBlockDb));
    assert_eq!(kept.reports(), [(EVODB_INCONSISTENCY.to_owned(), None)], "H2's report alone");
    assert!(kept.tripped() && kept.shutdown_requested(), "AbortNode's latch and shutdown");
    let abort = position(&lines, |l| l == format!("*** {EVODB_INCONSISTENCY}"));
    let line = format!(
        "Verification error: irrecoverable inconsistency in block data at 258, hash={}",
        r1.at(258).hash()
    );
    assert!(abort < position(&lines, |l| l == line), "{lines:#?}");
    assert!(!lines.iter().any(|l| l.starts_with(SUMMARY)), "{lines:#?}");
}

/// T3 (b) (V-c, CH5-1b): R1's M at 258 with 257's coinbase coin rewritten to another value (Core's
/// L3 fixture): `verifychain` (3,6) finds the unclean undo of that evo block and answers
/// `CORRUPTED_BLOCK_DB` with Core's line, no skip and no report.
// Core: validation.cpp:5130-5136, :5143-5146 at v24.0.0
#[test]
fn a_damaged_coin_under_an_evo_block_is_a_corrupted_database() {
    install_accept_log();
    let r1 = r1();
    let log = Log::default();
    let (rig, kept) = connected(&r1, &log, 258);
    let cb = &r1.at(257).block.txs()[0];
    let out = &cb.outputs()[0];
    let changed = dash_primitives::TxOut::from_duffs(out.value() + 1, out.script_pubkey().clone());
    let op = dash_primitives::OutPoint::new(cb.txid(), 0);
    kept.rewrite_coin(&op, &changed, 257, true);
    let lines = walk_lines(&rig, 3, 6, VerifyDbResult::CorruptedBlockDb);
    let found = "Verification error: coin database inconsistencies found (last 2 blocks, ";
    assert!(lines.iter().any(|l| l.starts_with(found)), "{lines:#?}");
    assert!(!lines.iter().any(|l| l.starts_with("Skipped verif")), "{lines:#?}");
    assert!(kept.reports().is_empty(), "{:?}", kept.reports());
}

/// A connector that reads the published scheme after each of its scratch attempts: VerifyDB's
/// undos and reconnects.
struct SchemeProbe {
    inner: Box<dyn crate::chain::BlockConnector>,
    seen: Arc<Mutex<Vec<Option<BlsCtx>>>>,
}

impl SchemeProbe {
    fn saw(&self) {
        self.seen.lock().expect("seen").push(self.inner.published_bls());
    }
}

impl crate::chain::BlockConnector for SchemeProbe {
    fn connect_block(
        &mut self,
        cx: &crate::chain::ConnectCx<'_>,
        v: &mut crate::chain::VContext,
        target: crate::chain::ViewTarget<'_>,
        block: &Block,
        checked: bool,
        id: dash_consensus::BlockId,
        st: &mut crate::chain::BlockStatusWriter<'_>,
    ) -> Result<crate::chain::ConnectOutcome, crate::chain::Fatal> {
        let scratch = matches!(target, crate::chain::ViewTarget::Scratch(_));
        let r = self.inner.connect_block(cx, v, target, block, checked, id, st);
        if scratch {
            self.saw();
        }
        r
    }

    fn disconnect_block(
        &mut self,
        cx: &crate::chain::ConnectCx<'_>,
        v: &mut crate::chain::VContext,
        target: crate::chain::ViewTarget<'_>,
        block: &Block,
        id: dash_consensus::BlockId,
    ) -> Result<crate::chain::DisconnectResult, crate::chain::Fatal> {
        let scratch = matches!(target, crate::chain::ViewTarget::Scratch(_));
        let r = self.inner.disconnect_block(cx, v, target, block, id);
        if scratch {
            self.saw();
        }
        r
    }

    fn test_block_validity(
        &mut self,
        cx: &crate::chain::ConnectCx<'_>,
        v: &mut crate::chain::VContext,
        checks: &dyn crate::chain::ProposalChecks,
        block: &Block,
        prev: dash_consensus::BlockId,
        check_pow: bool,
        check_merkle: bool,
    ) -> Result<crate::chain::ConnectOutcome, crate::chain::Fatal> {
        self.inner.test_block_validity(cx, v, checks, block, prev, check_pow, check_merkle)
    }

    fn published_bls(&self) -> Option<BlsCtx> {
        self.inner.published_bls()
    }

    fn evo_reads(&self) -> &dyn crate::chain::EvoReads {
        self.inner.evo_reads()
    }

    fn with_tip_coins(
        &self,
        f: &mut crate::chain::TipCoinsCall<'_>,
    ) -> Result<Option<()>, crate::chain::Fatal> {
        self.inner.with_tip_coins(f)
    }

    fn ehf_rows(&self) -> Option<Arc<dyn crate::chain::EhfRows>> {
        self.inner.ehf_rows()
    }

    fn verify_scratch(
        &mut self,
        parent: crate::chain::seams::ScratchParent,
    ) -> Result<Option<Box<dyn crate::chain::ScratchView>>, crate::chain::Fatal> {
        self.inner.verify_scratch(parent)
    }

    fn take_scratch_refusal(&mut self) -> Option<String> {
        self.inner.take_scratch_refusal()
    }

    fn take_undo_fault(&mut self) -> Option<crate::chain::Fatal> {
        self.inner.take_undo_fault()
    }

    fn open_scheme_guard(&mut self, holder: crate::chain::SchemeGuardHolder) {
        self.inner.open_scheme_guard(holder);
    }

    fn commit_scheme_guard(&mut self) {
        self.inner.commit_scheme_guard();
    }

    fn exit_scheme_guard(&mut self, log: bool) {
        self.inner.exit_scheme_guard(log);
    }
}

/// `chain` connected to `to` on a rig whose connector is wrapped in a [`SchemeProbe`]: the rig,
/// its stores and what the probe saw.
fn probed(chain: &Recorded, to: u32) -> (Rig, Kept, Arc<Mutex<Vec<Option<BlsCtx>>>>) {
    let cfg = RigConfig {
        params: Some(chain.p.clone()),
        ..RigConfig::default()
    };
    let (mut cfg, kept) = recorded_config_on(cfg, chain.p.clone(), chain.mocktime, None);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let inner = cfg.connector.take().expect("the node's connector");
    cfg.connector = Some(Box::new(SchemeProbe {
        inner,
        seen: seen.clone(),
    }));
    let rig = Rig::with(cfg);
    for m in &chain.blocks[..to as usize] {
        rig.accept(m);
    }
    kept.assert_no_abort(&rig);
    (rig, kept, seen)
}

/// T6 (CH5-1b): the published scheme read after every scratch undo and reconnect of a production
/// walk is the one published before it: VerifyDB's attempts switch the walk's own scheme
/// (`EvoTarget::Active`'s filter, `evo/node.rs`), never the node's. On R1 at (4,0) from 259,
/// whose walk crosses V19 at 212, and on R2 at (4,0) from 290 across 236.
#[test]
fn the_walks_attempts_never_publish_a_scheme() {
    for (name, chain, top, crossing) in [("R1", r1(), 259, 212), ("R2", r2(), 290, 236)] {
        let (rig, kept, seen) = probed(&chain, top);
        let before = scheme(&rig);
        assert_eq!(before, BlsCtx::BASIC, "{name}");
        assert_eq!(walk(&rig, 4, 0), VerifyDbResult::Success, "{name}");
        let seen = std::mem::take(&mut *seen.lock().expect("seen"));
        assert_eq!(seen.len(), 2 * top as usize, "{name}: an undo and a reconnect per block");
        assert!(seen.iter().all(|s| *s == Some(before)), "{name}: {seen:?}");
        assert!(top > crossing, "{name}");
        assert_eq!(scheme(&rig), before, "{name}: after the walk");
        kept.assert_no_abort(&rig);
    }
}

/// T7 (b) (CH5-1b), the failing undo: on R1 at 258 with the evo rows flushed, the engine fails
/// the first read of 211's list diff, which comes once the walk has undone 212, V19's block, and
/// switched to legacy. The guarded engine takes the read fault as the node's (`AbortNode`, one
/// report) and V stops in the walk; the walk's guard still reverts to the basic scheme the walk
/// entered with, last. (Core's `dbwrapper_error` would reach `UndoSpecialTxsInBlock`'s catch,
/// `evo/specialtxman.cpp:1086-1090` at v24.0.0; a store read error is a fault here, not that
/// catch's verdict (EV3 D21, OD-CP-276). The catch's restore and FAILURE lines are
/// `connector/verify_db.rs`'s, on the test hooks.)
// Core: validation.cpp:162-167, :5056 at v24.0.0
#[test]
fn a_failing_undo_inside_the_v19_window_restores_the_entry_scheme() {
    use super::read_faults::Fault;
    use crate::evo::codec::EvoCf;

    install_accept_log();
    let r1 = r1();
    let log = Log::default();
    let (rig, kept) = connected(&r1, &log, 258);
    kept.flush_evo();
    let failing = r1.at(211).hash();
    kept.faults().arm(EvoCf::MnDiff.cf(), EvoCf::MnDiff.key(&failing).as_bytes(), Fault::Io);
    let (exit, lines) = walk_stopping(rig, 3, 0);
    assert!(kept.faults().hits() > 0, "211's diff read from the engine");
    let read = "*** Error reading from database: memory error (Io): injected read failure";
    assert_eq!(scheme_lines(&lines), [US1, RV0], "{lines:#?}");
    let switched = position(&lines, |l| l == US1);
    let failed = position(&lines, |l| l == read);
    assert!(switched < failed && failed < position(&lines, |l| l == RV0), "{lines:#?}");
    assert_eq!(kept.reports().len(), 1, "the read's report alone: {:?}", kept.reports());
    assert!(exit.fatal.is_some(), "V stops on it");
    assert!(!lines.iter().any(|l| l.starts_with(SUMMARY)), "{lines:#?}");
}

/// The node's V seams over a recorded chain, as [`recorded_config_on`] wires them, for a reopen.
fn node_seams() -> super::evo_chain::OpenSeams<crate::evo::ev1::DmlChecks> {
    super::evo_chain::OpenSeams {
        ev1: crate::evo::ev1::DmlChecks,
        cl: Box::new(crate::evo::cl::AttemptClVerifier::default()),
        helper: Box::new(crate::connect::NodeHelper),
        blocks: Box::new(|b| b),
        bls_checks: Arc::new(dash_evo::llmq::SerialBlsChecks),
        script_threads: 0,
    }
}

/// `feature_asset_locks`' run to 190, V stopped, every evo row and coin flushed, 188's MNHF row
/// deleted from the engine when `deleted`, then V restarted on the stores: its MNHF manager
/// cold. The reads of 188's row from then on are counted.
fn cold_at_190(deleted: bool) -> (Recorded, Rig, Kept) {
    use crate::evo::codec::EvoCf;

    let locks = asset_locks();
    let log = Log::default();
    let (mut rig, kept) = connected(&locks, &log, 190);
    let at = rig.id(locks.at(190));
    let exit = rig.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
    kept.flush_evo();
    kept.flush_coins();
    let key = EvoCf::Mnhf.key(&locks.at(188).hash());
    assert!(kept.row(EvoCf::Mnhf.cf(), key.as_bytes()).is_some(), "188's connect wrote its row");
    if deleted {
        kept.delete_row(EvoCf::Mnhf.cf(), key.as_bytes());
    }
    let mut cfg = kept.reopen(RigConfig::default(), 191, node_seams());
    let files = cfg.files.take().expect("the node's block files");
    let rig = Rig::restart_with(exit, files, at, cfg).expect("V starts");
    kept.assert_no_abort(&rig);
    kept.faults().watch(EvoCf::Mnhf.cf(), key.as_bytes());
    (locks, rig, kept)
}

/// T8 (CH5-1b): `feature_asset_locks`' 188 mines an MNHF signal (V20 at 100). A restart at 190
/// leaves the MNHF manager cold, and `verifychain` (3,3) undoes 190, 189 and 188: the undo of 188
/// reads its row from the engine (the strict read, `dash-dml/src/mnhf.rs` `undo_block`), no
/// earlier read having warmed the manager. (i) The row there: `SUCCESS`. (ii) The row deleted:
/// rdashd's answer is the strict read's store fault, where Core's `GetForBlock` rebuilds the
/// signals from disk and succeeds (`evo/mnhftx.cpp:285-321` at v24.0.0). The node's rows are a
/// connect's (EV3 D17): a missing row past V20 is not a datadir this build writes (CH5 D29).
/// BL-874 owes Core's rebuild before `-importcoredatadir` serves; (ii) then flips.
// Core: evo/mnhftx.cpp:264-283, :285-321 at v24.0.0
#[test]
fn the_walks_undo_of_an_mnhf_signal_reads_its_row_cold() {
    let (_locks, rig, kept) = cold_at_190(false);
    assert_eq!(walk(&rig, 3, 3), VerifyDbResult::Success);
    assert!(kept.faults().hits() > 0, "the undo of 188 read its row from the engine");
    kept.assert_no_abort(&rig);

    install_accept_log();
    let (_locks, rig, kept) = cold_at_190(true);
    let (exit, lines) = walk_stopping(rig, 3, 3);
    assert!(kept.faults().hits() > 0, "the undo of 188 looked for its row in the engine");
    let text = "CMNHFManager::UndoBlock: Found EvoDB inconsistency, you must reindex: no MNHF row \
                for a block where V20 is active";
    assert_eq!(kept.reports(), [(text.to_owned(), None)], "the strict read's store fault");
    assert_eq!(exit.fatal.as_ref().map(|f| f.0.as_str()), Some(text));
    assert_eq!(lines.last().map(String::as_str), Some(format!("*** {text}").as_str()));
    assert!(!lines.iter().any(|l| l.starts_with(SUMMARY)), "{lines:#?}");
}

/// T11 (ruling 5, CH5-1b): a negative production scratch verification. R2's 250 with its CbTx's
/// best-ChainLock signature replaced by 255's, a valid point that signs another height's lock, its
/// other fields kept, mined again: no ChainLock is held, so neither of step 10's shortcuts
/// applies, and the scratch connect on production `NodeEvo` is rejected with
/// `bad-cbtx-invalid-clsig`, the checks before the ChainLock's having passed. The census holds
/// exactly that check, invalid.
// Core: evo/specialtxman.cpp:139-215 CheckCbTxBestChainlock at v24.0.0
#[test]
fn a_forged_cbtx_chainlock_fails_the_scratch_connect() {
    use super::evo_chain::node_config;
    use super::evo_chain_cl::remined;
    use super::v19_window::cbtx_cl;
    use crate::chain::ViewTarget;
    use crate::chain::seams::{HeldChainLocks, ScratchParent};
    use crate::chain::{AttemptOrigin, BlockStatusWriter, ConnectCx, ConnectOutcome, Fatal};
    use crate::evo::cl::AttemptClVerifier;

    let r2 = r2();
    let n = 250_u32;
    let (verifier, census) = AttemptClVerifier::with_census();
    let after_genesis = r2.mocktime - i64::from(Mined::genesis(&r2.p).time()) + 86_400;
    let cfg = RigConfig {
        params: Some(r2.p.clone()),
        ..RigConfig::default()
    };
    let (cfg, kept) = node_config(
        r2.p.clone(),
        after_genesis,
        cfg,
        crate::evo::ev1::DmlChecks,
        Box::new(verifier),
        Box::new(crate::connect::NodeHelper),
        |b| b,
    );
    let rig = Rig::with(cfg);
    for m in &r2.blocks[..n as usize - 1] {
        rig.accept(m);
    }
    kept.assert_no_abort(&rig);
    let (diff, sig) = cbtx_cl(r2.at(n)).expect("a v3 CbTx");
    let (_, other) = cbtx_cl(r2.at(n + 5)).expect("a v3 CbTx");
    assert!(sig != [0; 96] && other != [0; 96] && other != sig);
    let forged = remined(&r2.p, r2.at(n), |cb| {
        let mut extra_payload = cb.extra_payload().clone();
        extra_payload[71..167].copy_from_slice(&other);
        *cb = dash_primitives::Transaction::new(
            cb.version(),
            cb.tx_type(),
            cb.inputs().clone(),
            cb.outputs().clone(),
            cb.lock_time(),
            extra_payload,
        );
    });
    assert_eq!(cbtx_cl(&forged), Some((diff, other)), "the other fields kept");
    census.checks.lock().clear();

    rig.headers(std::slice::from_ref(&forged));
    let id = rig.id(&forged);
    let out = Arc::new(Mutex::new(None));
    let put = out.clone();
    let m = forged.clone();
    rig.on_v(move |v| {
        let pos = v.files.save_block(&m.block, m.height, None).map_err(Fatal::new)?;
        let pos = pos.ok_or_else(|| Fatal::new("no position for the block"))?;
        let n_tx = u32::try_from(m.block.txs().len()).expect("a count");
        v.idx.received_block_transactions(&mut v.chain, &v.reader, id, n_tx, pos)?;
        let mut view = v
            .connector
            .verify_scratch(ScratchParent::Tip)?
            .ok_or_else(|| Fatal::new("a connector without a scratch view"))?;
        let deployments = v.deployments.view();
        let is_ibd = v.is_ibd();
        let best = v.cl.sigs.best.clone();
        let chain = std::cell::RefCell::new(&mut v.chain);
        let best_header = chain.borrow().best_header();
        let chainlocks = HeldChainLocks::new(&chain, best);
        let cx = ConnectCx {
            params: &v.params,
            reader: &v.reader,
            deployments: &deployments,
            best_header,
            min_chain_work: v.reader.min_chain_work(),
            is_ibd,
            chainlocks: &chainlocks,
            guard: v.guard,
            origin: AttemptOrigin::VerifyDb,
        };
        let mut st = BlockStatusWriter::new(&chain, id);
        let target = ViewTarget::Scratch(&mut *view);
        let r = v.connector.connect_block(&cx, &mut v.vcx, target, &m.block, false, id, &mut st);
        *put.lock().expect("out") = Some(r);
        Ok(())
    });
    let r = out.lock().expect("out").take().expect("V ran it");
    match r {
        Ok(ConnectOutcome::Invalid(e)) => {
            assert_eq!(e.reject_reason(), "bad-cbtx-invalid-clsig", "{e:?}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(*census.checks.lock(), [(n, n - 1 - diff, Some(false))]);
    kept.assert_no_abort(&rig);
}
