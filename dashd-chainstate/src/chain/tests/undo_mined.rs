//! CH7 §4.1.4 (design Q1): H9 stages `UndoBlock`'s tombstones in the disconnect's evo
//! transaction, so a disconnected block's mined rows leave every level of the store, each in the
//! family its connect wrote. Core erases them in the undo's evodb transaction
//! (`llmq/blockprocessor.cpp` v24:536-548). And H9's snapshot write at a cycle base, which
//! route D makes, failed (OD-CP-14).

use std::sync::{Arc, Mutex};

use dash_consensus::primitives::Header;
use dash_consensus::{Block, BlockId, BlsCtx};
use dash_dml::blockprocessor::{QcHashCache, qc_hashes};
use dash_dml::store::MinedCommitmentRecord;
use dash_dml::{EvoDelta, StoreError};
use dash_evo::llmq::{CompleteChain, LlmqCommitment, LlmqCtx, MinedCommitmentReader as _};
use dash_evo::sml::QcHashes;
use dash_params::LlmqType;
use dash_primitives::{BlockHash, BlockHeader, QuorumHash, QuorumVvecHash};
use dashd_store::{ErrorKind, KvRead as _, MemoryEngine};

use super::llmq_members::{ROTATED, TEST, params};
use super::node_evo::{
    Recording, at, node_evo_over_blocks, node_evo_with_mineable, qc_tx, tx, undo_block,
    undo_block_on,
};
use super::on_v_binding::{CYCLE, CbTxBlocks, FORKS, list, seed};
use super::support::{Mined, Rig, RigConfig, branch, install_accept_log, lines_on, with_tip_cx};
use crate::chain::{Fatal, RecordingAbort};
use crate::evo::RocksEvoStore;
use crate::evo::codec::{index_key, mined_key, split_mined_key};
use crate::evo::effects::{MineableApplier, StoreSnapshots};
use crate::evo::ev1::Ev1NotBuilt;
use crate::evo::node::Ancestry;
use crate::evo::testkit;
use crate::flush::EffectLedger;
use crate::llmq::rows::RuntimeRows;
use crate::store::{CHAINSTATE_SCHEMA, GlobalFailStop, GuardedEngine};

/// The undone block: past its cycle base 48, so H9's rotated precompute asks nothing.
const MINED: u32 = 58;
/// The non-rotated quorum's base.
const PLAIN_BASE: u32 = 48;
/// The rotated quorum's index in cycle 48, and its base.
const INDEX: i16 = 1;
const ROTATED_BASE: u32 = 49;

/// A non-null commitment of `llmq_type` for the quorum based at `base`: Core's layout with a
/// nonzero vvec hash, which no null commitment has. Its index, from version 2, is `index`.
fn non_null(llmq_type: LlmqType, base: &Mined, index: Option<i16>) -> LlmqCommitment {
    let version = if index.is_some() {
        2
    } else {
        1
    };
    let mut qc = testkit::commitment(llmq_type, testkit::quorum_hash(&base.hash()), version);
    qc.quorum_index = index;
    qc.quorum_vvec_hash = QuorumVvecHash::from([1; 32]);
    assert!(!qc.is_null());
    qc
}

/// The rows `ProcessCommitment` wrote for `qc`, based at `base`.
fn record(qc: &LlmqCommitment, base: &Mined, rotation_index: Option<i16>) -> MinedCommitmentRecord {
    MinedCommitmentRecord {
        llmq_type: qc.llmq_type,
        quorum_hash: qc.quorum_hash,
        base_height: base.height,
        rotation_index,
        commitment: Arc::new(qc.clone()),
    }
}

/// A mineable pool that records the commitments the undo's log re-adds (`bp:556`), in order.
#[derive(Clone, Default)]
struct Readded(Arc<Mutex<Vec<LlmqCommitment>>>);

impl MineableApplier for Readded {
    fn erase(&mut self, _: LlmqType, _: &QuorumHash, _: &[u8; 32]) -> Result<(), Fatal> {
        Err(Fatal::new("an undo erases nothing from the pool"))
    }

    fn reinsert(&mut self, commitment: &Arc<LlmqCommitment>) -> Result<(), Fatal> {
        self.0.lock().expect("readded").push((**commitment).clone());
        Ok(())
    }
}

/// The mined reads at one block, through the store's view of the committed layer: the two
/// commitments' mining blocks, the non-rotated index, the rotated index at [`INDEX`], and
/// `GetQcHashes` there (the active set `CalcCbTxMerkleRootQuorums` commits to).
#[derive(Debug, PartialEq)]
struct Reads {
    plain: Option<BlockHash>,
    rotated: Option<BlockHash>,
    until: Vec<BlockId>,
    by_index: Option<BlockId>,
    hashes: QcHashes,
}

/// One run's answers.
#[derive(Debug)]
struct Run {
    /// At the mining block before the undo, at it after, and at its parent after.
    before: Reads,
    after: Reads,
    parent: Reads,
    /// H9 undid the block, and the evo tip moved to its parent.
    undone: bool,
    tip_at_parent: bool,
    /// The four rows in RocksDB before the undo, and after it and a flush.
    durable_before: [bool; 4],
    durable_after: [bool; 4],
    /// The other family's height-index rows of the rotated type, at any point.
    other_family: bool,
    /// The pool's re-adds: the non-rotated commitment, then the rotated one (`GetCommitmentsFromBlock`
    /// orders by type).
    readded: Vec<LlmqCommitment>,
}

/// The chain to 60 with each DIP0003 block's list; block [`MINED`] also mines a non-rotated and a
/// rotated commitment, and its body carries them. H9 undoes it through `NodeEvo`. With `flushed`
/// the connect's rows are durable before the undo, so its tombstones hide RocksDB's rows; else
/// they meet the active index's. A flush after the undo shows what reaches RocksDB.
fn run(flushed: bool) -> Run {
    let p = Arc::new(params(FORKS, FORKS));
    let blocks = branch(&p, &Mined::genesis(&p), 60, 11);
    let rig = Rig::with(RigConfig {
        params: Some(p.clone()),
        ..RigConfig::default()
    });
    rig.headers(&blocks);
    let ids = rig.ids(&blocks);
    let n = MINED as usize - 1;
    let (plain_base, rotated_base) =
        (blocks[PLAIN_BASE as usize - 1].clone(), blocks[ROTATED_BASE as usize - 1].clone());
    let plain = non_null(TEST, &plain_base, None);
    let rotated = non_null(ROTATED, &rotated_base, Some(INDEX));
    let keys = [
        mined_key(TEST, &plain.quorum_hash),
        mined_key(ROTATED, &rotated.quorum_hash),
        index_key(TEST, None, MINED),
        index_key(ROTATED, Some(INDEX), MINED),
    ];
    let other_keys = [index_key(ROTATED, None, MINED), index_key(ROTATED, Some(0), MINED)];
    let body = Block::new(
        Header::new(BlockHeader::default()),
        vec![tx(1, 0, true, 0), qc_tx(MINED, &plain), qc_tx(MINED, &rotated)],
    );
    let readded = Readded::default();
    let pool = readded.clone();
    let out = Arc::new(Mutex::new(None));
    let seen = out.clone();
    rig.on_v(move |v| {
        let (mut evo, layer, store) =
            node_evo_with_mineable(&p, BlsCtx::BASIC, Recording, Box::new(pool));
        seed(&store, &layer, &blocks[..n]);
        let m = &blocks[n];
        let delta = EvoDelta {
            mined_commitments: vec![
                record(&plain, &plain_base, None),
                record(&rotated, &rotated_base, Some(INDEX)),
            ],
            ..EvoDelta {
                block: m.hash(),
                height: m.height,
                dml: Some(testkit::dml(
                    &list(blocks[n - 1].hash(), m.height - 1),
                    &list(m.hash(), m.height),
                    false,
                )),
                mined_commitments: Vec::new(),
                credit_data: None,
                credit_pool: None,
                mnhf_signals: None,
                erased_mined: Vec::new(),
                evo_tip: Some(m.hash()),
            }
        };
        store.commit(&mut layer.lock(), &delta).map_err(|e| Fatal::new(e.to_string()))?;
        if flushed {
            testkit::flush(&store, &mut layer.lock());
        }
        let durable = |keys: &[Vec<u8>]| -> Vec<bool> {
            keys.iter()
                .map(|k| {
                    let (cf, key) = split_mined_key(k);
                    store.db().get(cf.cf(), key).expect("a healthy read").is_some()
                })
                .collect()
        };
        let durable_before = durable(&keys);
        let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
            let chain = Ancestry(cx.reader);
            let complete = CompleteChain::<StoreError>::new(&chain);
            let llmq = LlmqCtx::new(&p, &complete, cx.deployments, BlsCtx::BASIC)
                .map_err(|e| Fatal::new(format!("{e:?}")))?;
            let reads = |at: BlockId| -> Reads {
                let view = store.view(&layer.lock(), &chain);
                let mined = |t, q| view.mined_commitment(t, q, at).expect("a read").map(|m| m.1);
                Reads {
                    plain: mined(TEST, &plain.quorum_hash),
                    rotated: mined(ROTATED, &rotated.quorum_hash),
                    until: view.mined_until(TEST, at, 8).expect("a read"),
                    by_index: view.nth_mined_by_index(ROTATED, at, INDEX, 0).expect("a read"),
                    hashes: qc_hashes(&llmq, &view, &mut QcHashCache::default(), at)
                        .expect("the active set"),
                }
            };
            let before = reads(ids[n]);
            let undone = undo_block(
                &mut evo,
                vcx,
                cx,
                &body,
                &at(m, ids[n], ids[n - 1]),
                &blocks[n - 1].hash(),
            )?;
            Ok((before, undone, reads(ids[n]), reads(ids[n - 1])))
        })?;
        let (before, undone, after, parent) = r;
        let tip_at_parent = layer.lock().evo_tip() == Some(blocks[n - 1].hash());
        testkit::flush(&store, &mut layer.lock());
        let durable_after = durable(&keys);
        let other_family = durable(&other_keys).contains(&true);
        *seen.lock().expect("seen") = Some(Run {
            before,
            after,
            parent,
            undone: matches!(undone, Ok(true)),
            tip_at_parent,
            durable_before: durable_before.try_into().expect("four keys"),
            durable_after: durable_after.try_into().expect("four keys"),
            other_family,
            readded: readded.0.lock().expect("readded").clone(),
        });
        Ok(())
    });
    rig.assert_no_abort();
    out.lock().expect("out").take().expect("V ran it")
}

/// Before the undo the two rows answer at the mining block: both mining blocks, the non-rotated
/// base in `GetMinedCommitmentsUntilBlock`, the rotated one at its index, and both in the active
/// set's hashes. After it, every read at that block equals the parent's, which never saw them,
/// and a flush leaves no row in RocksDB, in either family. Run with the connect's rows in the
/// active index and with them durable.
#[test]
fn an_undo_erases_its_blocks_mined_rows_at_every_level() {
    for flushed in [false, true] {
        let r = run(flushed);
        assert!(r.undone && r.tip_at_parent, "flushed {flushed}: {r:?}");
        let mined_at = r.before.plain.expect("the plain row before the undo");
        assert_eq!(r.before.rotated, Some(mined_at), "flushed {flushed}");
        assert_eq!(r.before.until.len(), 1, "flushed {flushed}: {:?}", r.before.until);
        assert!(r.before.by_index.is_some(), "flushed {flushed}");
        assert_ne!(r.before.hashes, r.parent.hashes, "flushed {flushed}: the rows are active");
        assert_eq!(r.after, r.parent, "flushed {flushed}");
        assert_eq!(r.durable_before, [flushed; 4], "flushed {flushed}");
        assert_eq!(r.durable_after, [false; 4], "flushed {flushed}");
        assert!(!r.other_family, "flushed {flushed}");
        assert_eq!(r.readded.len(), 2, "flushed {flushed}");
        assert_eq!(
            (r.readded[0].llmq_type, r.readded[1].llmq_type),
            (TEST, ROTATED),
            "flushed {flushed}"
        );
    }
}

/// OD-CP-14, OD-CP-18: H9 at a rotation cycle base past DIP0003, over the nine masternodes of
/// each list, computes the cycle's rotated members again (`bp:523`, a reset) and stores its
/// snapshot through route D. The write fails in the guarded engine (an injected I/O failure): the
/// rows' store trips the latch and logs `HandleError`'s lines, but reports nothing, as Core's raw
/// write only throws; the attempt's `Fatal` is unreported and the evo tip stays at the block. The
/// connector turns it into Core's failed disconnect: on ABC's path "Failed to disconnect block" is
/// the one report (`connector::disconnect`'s `a_failed_log_application_fails_the_disconnect`), and
/// any other caller reports the store's own failure as it stops V (OD-CP-30, CH7 D103).
#[test]
fn an_undo_whose_snapshot_write_fails_leaves_the_report_to_the_disconnect() {
    install_accept_log();
    let p = Arc::new(params(FORKS, FORKS));
    let blocks = branch(&p, &Mined::genesis(&p), CYCLE, 13);
    let rig = Rig::with(RigConfig {
        params: Some(p.clone()),
        ..RigConfig::default()
    });
    rig.headers(&blocks);
    let ids = rig.ids(&blocks);
    let engine = MemoryEngine::new(&CHAINSTATE_SCHEMA);
    engine.inject_failure(ErrorKind::Io);
    let sink = Arc::new(RecordingAbort::default());
    let latch = GlobalFailStop::new();
    let db = Arc::new(GuardedEngine::new(engine, latch.clone(), sink.clone()));
    let rows = Arc::new(RocksEvoStore::new(db, testkit::codec(), testkit::genesis()));
    let snapshots = StoreSnapshots {
        rows: Arc::new(RuntimeRows::new(rows, Arc::new(EffectLedger::default()))),
    };
    let out = Arc::new(Mutex::new(None));
    let seen = out.clone();
    rig.on_v(move |v| {
        let blocks_read = Box::new(CbTxBlocks);
        let (mut evo, layer, store) = node_evo_over_blocks(
            &p,
            BlsCtx::BASIC,
            Ev1NotBuilt,
            blocks_read,
            Some(Box::new(snapshots)),
        );
        seed(&store, &layer, &blocks);
        let n = blocks.len() - 1;
        let (m, parent) = (&blocks[n], blocks[n - 1].hash());
        let r = with_tip_cx(v, |vcx, cx| {
            undo_block(&mut evo, vcx, cx, &m.block, &at(m, ids[n], ids[n - 1]), &parent)
        });
        let tip = layer.lock().evo_tip() == Some(m.hash());
        *seen.lock().expect("seen") = Some((r.map(|undone| undone.is_ok()), tip));
        Ok(())
    });
    let (r, tip) = out.lock().expect("out").take().expect("V ran it");
    let failure = latch.tripped().expect("the guard latched it").clone();
    assert_eq!(failure.site, "chainstate write");
    let text = format!("System error while flushing: {}", failure.message);
    let f = r.expect_err("the log's snapshot write fails");
    assert_eq!(f.0, text);
    assert!(f.reported_by_v(false), "unreported");
    assert_eq!(sink.reports(), [], "the store reports nothing");
    let debug = "You can use -debug=leveldb to get more complete diagnostic messages";
    let lines = lines_on(rig.v_thread());
    assert_eq!(lines.iter().filter(|l| *l == debug).count(), 1, "HandleError's lines: {lines:#?}");
    assert!(tip, "the evo tip stays at the block");
    rig.assert_no_abort();
}

/// OD-CP-29, OD-CP-30 (CH7 D102, D103) on `NodeEvo`: route B's inline write fails first in the
/// node's rows (an injected I/O failure in a guarded engine on the node's latch and sink, the
/// facade on the same latch, as dashd-node wires them), and its store reports it. H9 at the cycle
/// base then stores its snapshot through route D, whose undo write the latch refuses: the
/// attempt's `Fatal` is reported already, and `AttemptHandle::fail`, whose trip latches nothing
/// more, passes it through. The store's text stays the one report.
#[test]
fn an_undo_after_route_bs_failed_write_passes_the_stores_report_through() {
    use dash_evo::llmq::QuorumSnapshot;

    use crate::llmq::capability::Capability as _;
    use crate::llmq::facade::MembersFacade;

    install_accept_log();
    let p = Arc::new(params(FORKS, FORKS));
    let blocks = branch(&p, &Mined::genesis(&p), CYCLE, 14);
    let engine = MemoryEngine::new(&CHAINSTATE_SCHEMA);
    engine.inject_failure(ErrorKind::Io);
    let sink = Arc::new(RecordingAbort::default());
    let latch = GlobalFailStop::new();
    let db = Arc::new(GuardedEngine::new(engine, latch.clone(), sink.clone()));
    let rows = Arc::new(RocksEvoStore::new(db, testkit::codec(), testkit::genesis()));
    let rows = Arc::new(RuntimeRows::new(rows, Arc::new(EffectLedger::default())));
    let facade = MembersFacade::new(&p, Arc::clone(&rows), latch.clone()).expect("in domain");
    let facade = Arc::new(facade);
    let rig = Rig::with(RigConfig {
        params: Some(p.clone()),
        facade: Some(Arc::clone(&facade)),
        ..RigConfig::default()
    });
    rig.headers(&blocks);
    let ids = rig.ids(&blocks);
    let base = blocks[0].hash();
    let wrote = rig.v.on_v(move |on_v, _| {
        let snapshot = QuorumSnapshot {
            active_quorum_members: vec![true, false],
            skip_list_mode: 0,
            skip_list: vec![],
        };
        let mut effects = Vec::new();
        let mut rows = on_v.rows(&mut effects);
        rows.store(LlmqType::LlmqTestDip0024, &base, &snapshot).map_err(|e| e.to_string())
    });
    let err = wrote.recv().expect("V serves it").expect_err("route B's write fails");
    let failure = latch.tripped().expect("the guard latched it").clone();
    assert_eq!(failure.site, "chainstate write");
    let text = format!("System error while flushing: {}", failure.message);
    assert!(err.contains(&text), "{err}");
    assert_eq!(sink.reports(), [(text.clone(), None)], "the store reported it");
    let snapshots = StoreSnapshots {
        rows,
    };
    let out = Arc::new(Mutex::new(None));
    let seen = out.clone();
    rig.on_v(move |v| {
        let blocks_read = Box::new(CbTxBlocks);
        let (mut evo, layer, store) = node_evo_over_blocks(
            &p,
            BlsCtx::BASIC,
            Ev1NotBuilt,
            blocks_read,
            Some(Box::new(snapshots)),
        );
        seed(&store, &layer, &blocks);
        let n = blocks.len() - 1;
        let (m, parent) = (&blocks[n], blocks[n - 1].hash());
        let r = with_tip_cx(v, |vcx, cx| {
            let at = at(m, ids[n], ids[n - 1]);
            undo_block_on(&mut evo, &facade, vcx, cx, &m.block, &at, &parent)
        });
        *seen.lock().expect("seen") = Some(r.map(|undone| undone.is_ok()));
        Ok(())
    });
    let r = out.lock().expect("out").take().expect("V ran it");
    let f = r.expect_err("the undo's snapshot write is refused");
    assert_eq!(f.0, text);
    assert!(!f.reported_by_v(false), "reported already");
    assert_eq!(sink.reports(), [(text, None)], "the store's report, and only it");
    assert_eq!(latch.tripped().map(|f| f.site), Some("chainstate write"), "the store's latch");
    rig.assert_no_abort();
}
