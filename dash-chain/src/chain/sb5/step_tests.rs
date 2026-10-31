//! SB5 F6b: the SPV header step's write set (`write_header_step`, §3.7) over SB4's SPV chain,
//! committed into `MemHeaderStore` as SB4 §9.1's recipe runs it: P3 on the real step, the
//! batches, the refusals, and SB4's A-33 for the batch step.

use std::sync::Arc;

use dash_consensus::primitives::Header;
use dash_params::{ChainSetup, Network};

use super::super::spv_tests::{ANCHOR, Spv, T0, mine_n, regtest_pack, resident};
use crate::error::{ChainError, FailStop as _, StoreError};
use crate::mem_store::MemHeaderStore;
use crate::step::{Durability, HeaderStepSink, StepRow};
use crate::testing::{Latch, batch, params, services};
use crate::tip::BlockRef;
use crate::{
    AnchorPack, AnchorState, BlockId, CHAIN_SCHEMA, ChainConfig, ChainMeta, ClPersisted,
    ForkTarget, HeaderChain, HeaderStore as _, NoStore, RecordKind, Services, Status, VirtualTip,
};

/// Stages 1–5 of an SPV header step (SB4 §9.1): accept, then [`stages`].
pub(super) fn real_step(chain: &mut Spv, store: &mut MemHeaderStore, headers: &[Header]) {
    if !headers.is_empty() {
        chain.accept(batch(headers), None).expect("accept");
    }
    stages(chain, store);
}

/// Stages 1–5 after the accept: the fork target, the step written and committed, the tip
/// moved (stage 4 runs when the tip stays too, applying the overlay), then stage 5.
pub(super) fn stages(chain: &mut Spv, store: &mut MemHeaderStore) {
    drive(chain, store, |_, _| {}, Stage::Done).expect("the step");
}

/// Stage 1 (SB4 §9.1) as SPV runs it, with no full plan (DEC-187): `fork_target` from a fresh
/// cursor at the tip, as a `BlockRef` when that is not the tip. Stage 3 reads the move's shape
/// from it (`tip_target`), and each sub-step of stage 4 plans its own.
pub(super) fn plan(chain: &mut Spv) -> Option<BlockRef> {
    let to = chain.fork_target(&chain.virtual_tip()).expect("a target").id;
    let tip = chain.private_tip().expect("a tip").0.id;
    (tip != to).then(|| chain.block_ref_of(to).expect("held"))
}

/// How far [`drive`] runs: SB4 §9.1's stages, then stage 5.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Stage {
    Compute,
    Capture,
    Commit,
    Activate,
    Done,
}

/// SB4 §9.1's recipe up to `stop` as SPV runs it, with no full plan (DEC-187): one
/// `VirtualTip` for the step's calls, the fork target; the rows; the commit;
/// `activate_segmented`; `records_committed`. A failed commit trips the latch, as SB13's
/// writer does, and ends the step.
pub(super) fn drive(
    chain: &mut Spv,
    store: &mut MemHeaderStore,
    calls: impl FnOnce(&mut Spv, &mut VirtualTip),
    stop: Stage,
) -> Result<(VirtualTip, ForkTarget), ChainError> {
    let mut vt = chain.virtual_tip();
    calls(chain, &mut vt);
    let target = chain.fork_target(&vt)?;
    let to = chain.block_ref_of(target.id)?;
    let moved = chain.private_tip().expect("a tip").0.id != target.id;
    if stop > Stage::Compute {
        store.begin_step();
        let tail = chain.write_header_step(moved.then_some(to), store)?;
        if stop > Stage::Capture {
            if let Err(e) = store.commit_step(&tail, Durability::Buffered) {
                chain.svc.fail.read_failed("the header step's commit");
                return Err(e.into());
            }
            if stop > Stage::Commit {
                chain.activate_segmented(to, &mut || {})?;
                if stop > Stage::Activate {
                    chain.records_committed();
                }
            }
        }
    }
    Ok((vt, target))
}

pub(super) fn mark(chain: &mut Spv, id: BlockId) {
    chain.set_status(id, |s| Status(s.bits() | Status::CONFLICT_CHAINLOCK)).expect("mark");
    chain.mark_dirty(id);
}

fn marked(store: &MemHeaderStore, id: BlockId) -> bool {
    store.by_id(id).expect("read").expect("stored").status.has(Status::CONFLICT_CHAINLOCK)
}

/// An anchored SPV chain at w = 10 whose first step, the pack, went through
/// `write_header_step` (SB4's `anchored` writes no `active` rows).
pub(super) fn committed(
    p: &Arc<ChainSetup>,
    pack: &AnchorPack,
    svc: Services,
) -> (Spv, MemHeaderStore) {
    committed_w(p, pack, svc, 10)
}

/// [`committed`] at window `w`.
pub(super) fn committed_w(
    p: &Arc<ChainSetup>,
    pack: &AnchorPack,
    svc: Services,
    w: u32,
) -> (Spv, MemHeaderStore) {
    let mut store = MemHeaderStore::new();
    let mut chain = Spv::new_anchored(p.clone(), ChainConfig::spv(p, w), svc, store.clone(), pack)
        .expect("a valid pack");
    store.reset(meta(pack.hash, pack.height)).expect("reset");
    stages(&mut chain, &mut store);
    (chain, store)
}

/// After a committed step: every issued id is stored with the chain's status; `hdr_leaf` is the
/// accepted records with no accepted child (P3); the `active` rows are the snapshot's accepted
/// heights; `meta` is the chain's.
pub(super) fn check(chain: &Spv, store: &MemHeaderStore) {
    let recs = store.from_height(0).expect("read");
    assert_eq!(recs.len(), chain.next_id as usize, "every issued id is stored");
    for r in &recs {
        let e = chain.reader().try_entry(r.id).expect("read").expect("held");
        assert_eq!((e.hash, e.status), (r.hash, r.status), "{:?}", r.id);
    }
    let accepted: Vec<_> = recs.iter().filter(|r| r.kind == RecordKind::Accepted).collect();
    let is_accepted = |id| accepted.iter().any(|r| r.id == id);
    let mut brute: Vec<_> = accepted
        .iter()
        .filter(|r| !accepted.iter().any(|c| c.header.prev_hash == r.hash))
        .map(|r| r.id)
        .collect();
    brute.sort();
    let mut leaves: Vec<_> = store.leaves().expect("read").iter().map(|r| r.id).collect();
    leaves.sort();
    assert_eq!(leaves, brute, "P3: hdr_leaf");

    let s = chain.feeds().feed_snapshot();
    let active: Vec<_> = s
        .range(0, u32::MAX)
        .expect("read")
        .iter()
        .filter(|b| is_accepted(b.id))
        .map(|b| (b.height, b.id))
        .collect();
    assert_eq!(store.active_from(0).expect("read"), active, "the active rows");
    let meta = store.chain_meta().expect("read").expect("a meta row");
    let tip = chain.private_tip().expect("a tip").0.id;
    assert_eq!(
        (meta.next_id, meta.tip, meta.best_header),
        (BlockId(chain.next_id), tip, chain.best_header())
    );
}

/// P3 on the real step over an anchored SPV chain at w = 10 (D-C4-8): the first step writes the
/// pack, stale branches fork on resident and cold blocks, marks land on resident and cold
/// records, and two reorgs (one deep, one back) rewrite the `active` rows. After each step the
/// store is the chain's.
#[test]
fn write_header_step_p3_real_steps() {
    let p = params(Network::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = committed(&p, &pack, services(&p));
    check(&chain, &store);
    assert_eq!(store.active_from(0).expect("read"), [(ANCHOR, BlockId(24))], "the anchor");

    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 60);
    let at = |i: usize| T0 + 150 * (26 + i as u32);
    let steps: Vec<Vec<Header>> = vec![
        trunk[..10].to_vec(),
        mine_n(&p, trunk[8].hash(), at(9) + 7, 2),
        trunk[10..30].to_vec(),
        mine_n(&p, trunk[5].hash(), at(6) + 9, 3),
        mine_n(&p, trunk[25].hash(), at(26) + 11, 1),
    ];
    for s in &steps {
        real_step(&mut chain, &mut store, s);
        check(&chain, &store);
    }
    // A resident stale block and a cold active one: a mark on an active block above the fork
    // of a later reorg back would hold the fork target off it.
    let (warm, cold) = (BlockId(chain.next_id - 1), BlockId(27));
    assert!(resident(&chain, warm) && !resident(&chain, cold));
    mark(&mut chain, warm);
    mark(&mut chain, cold);
    real_step(&mut chain, &mut store, &trunk[30..35]);
    check(&chain, &store);
    assert!(marked(&store, warm) && marked(&store, cold));

    // Marks in steps that leave the tip, on stale blocks: on the largest stored id (a resident
    // group, which stage 4 must still update), then on a new id.
    real_step(&mut chain, &mut store, &mine_n(&p, trunk[33].hash(), at(34) + 5, 1));
    let last = BlockId(chain.next_id - 1);
    mark(&mut chain, last);
    real_step(&mut chain, &mut store, &[]);
    check(&chain, &store);
    assert!(marked(&store, last));
    chain.accept(batch(&mine_n(&p, trunk[32].hash(), at(33) + 5, 1)), None).expect("accept");
    let new = BlockId(chain.next_id - 1);
    mark(&mut chain, new);
    stages(&mut chain, &mut store);
    check(&chain, &store);
    assert!(marked(&store, new));

    // A heavier branch from trunk[24], then the trunk back over it.
    let branch = mine_n(&p, trunk[24].hash(), at(25) + 3, 20);
    real_step(&mut chain, &mut store, &branch);
    check(&chain, &store);
    assert_eq!(chain.feeds().feed_snapshot().read().tip.tip_hash, branch[19].hash());
    real_step(&mut chain, &mut store, &trunk[35..]);
    check(&chain, &store);
    assert_eq!(chain.feeds().feed_snapshot().read().tip.tip_hash, trunk[59].hash());
}

/// A chain from genesis: the first step's `active` rows start at height 0.
#[test]
fn write_header_step_from_genesis() {
    let p = params(Network::Regtest);
    let mut store = MemHeaderStore::new();
    let mut chain =
        Spv::new_genesis(p.clone(), ChainConfig::spv(&p, 10), services(&p), store.clone())
            .expect("genesis");
    let genesis = Header::new(dash_consensus::params::genesis_block(&p).header);
    store.reset(meta(genesis.hash(), 0)).expect("reset");
    let trunk = mine_n(&p, genesis.hash(), T0, 30);
    real_step(&mut chain, &mut store, &trunk[..12]);
    check(&chain, &store);
    real_step(&mut chain, &mut store, &trunk[12..]);
    check(&chain, &store);
    assert_eq!(store.active_from(0).expect("read").len(), 31);
}

pub(super) fn meta(anchor: dash_primitives::BlockHash, height: u32) -> ChainMeta {
    ChainMeta {
        schema: CHAIN_SCHEMA,
        anchor: (anchor, height),
        anchor_state: AnchorState::Candidate,
        next_id: BlockId(0),
        tip: BlockId(0),
        best_header: BlockId(0),
        sync_seq: 0,
        cl: ClPersisted {
            best: None,
            floor: None,
            enabled_observed: false,
        },
    }
}

/// A sink that records its batches.
#[derive(Default)]
struct Batches(Vec<Vec<StepRow>>);

impl HeaderStepSink for Batches {
    fn rows(&mut self, batch: &[StepRow]) -> Result<(), StoreError> {
        self.0.push(batch.to_vec());
        Ok(())
    }
}

/// The write set streams in batches of exactly `write_batch` rows but the last; the records
/// with their leaf deltas come first, then the overlay's status rows. The dirty set and the
/// overlay stay for stage 5 (D-C4-11).
#[test]
fn write_header_step_batches() {
    let p = params(Network::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = committed(&p, &pack, services(&p));
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 30);
    real_step(&mut chain, &mut store, &trunk[..20]);
    chain.cfg.feeds.write_batch = 4;
    chain.accept(batch(&trunk[20..]), None).expect("accept");
    mark(&mut chain, BlockId(26));
    let target = plan(&mut chain);
    let dirty = chain.dirty.ids().to_vec();
    let mut sink = Batches::default();
    let tail = chain.write_header_step(target, &mut sink).expect("stage 3");

    let sizes: Vec<_> = sink.0.iter().map(Vec::len).collect();
    let rows: Vec<_> = sink.0.concat();
    // Each record, its LeafAdd and its parent's LeafRemove, then the mark.
    assert_eq!(sizes, [4, 4, 4, 4, 4, 4, 4, 3]);
    assert!(matches!(rows[0], StepRow::Record(ref r) if r.id == BlockId(45)));
    assert_eq!(
        rows[30],
        StepRow::Status(BlockId(26), chain.writer_status(BlockId(26)).expect("marked"))
    );
    assert_eq!(chain.dirty.ids(), dirty, "not drained");
    assert_eq!(chain.overlay_marks().len(), 1);
    let d = tail.active.expect("the tip moves");
    assert_eq!(
        (d.from_height, d.new_height, d.old_height),
        (ANCHOR + 21, ANCHOR + 30, ANCHOR + 20)
    );
    assert_eq!(tail.cl, Some(chain.cl_state()), "every step writes the ChainLock state");
    assert_eq!(tail.meta.tip, target.expect("a target").id);
}

/// Refusals: the node's mode, a hold, an unknown target and a target at the wrong height.
#[test]
fn write_header_step_refusals() {
    let p = params(Network::Regtest);
    let mut node = HeaderChain::<NoStore>::new_genesis(
        p.clone(),
        ChainConfig::node(&p),
        services(&p),
        NoStore,
    )
    .expect("a valid configuration");
    let mut sink = Batches::default();
    assert!(matches!(node.write_header_step(None, &mut sink), Err(ChainError::Config(_))));

    let pack = regtest_pack(&p);
    let (mut chain, _store) = committed(&p, &pack, services(&p));
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 3);
    chain.accept(batch(&trunk), None).expect("accept");
    let mut hold = chain.hold_tip();
    let held = hold.chain().write_header_step(None, &mut sink);
    assert!(matches!(held, Err(ChainError::Internal(_))), "{held:?}");
    drop(hold);

    let end = chain.block_ref_of(chain.best_header()).expect("resident");
    let unknown = BlockRef {
        id: BlockId(chain.next_id),
        ..end
    };
    let r = chain.write_header_step(Some(unknown), &mut sink);
    assert!(matches!(r, Err(ChainError::UnknownBlock(_))), "{r:?}");
    let wrong = BlockRef {
        height: end.height + 1,
        ..end
    };
    let r = chain.write_header_step(Some(wrong), &mut sink);
    assert!(matches!(r, Err(ChainError::Internal(_))), "{r:?}");
    assert!(sink.0.is_empty(), "a refusal writes nothing");

    // No plan is needed: a step to any header writes its `active` change.
    let mid = chain.block_ref_of(BlockId(26)).expect("resident");
    let d = chain.write_header_step(Some(mid), &mut sink).expect("a step").active.expect("moves");
    assert_eq!((d.from_height, d.new_height, d.old_height), (ANCHOR + 1, ANCHOR + 2, ANCHOR));
    assert!(chain.write_header_step(None, &mut sink).expect("the tip stays").active.is_none());
}

/// A fresh store's first step: the pack and the first headers in one step, the target checked.
#[test]
fn write_header_step_first_step_moves_the_tip() {
    let p = params(Network::Regtest);
    let pack = regtest_pack(&p);
    let mut store = MemHeaderStore::new();
    let mut chain =
        Spv::new_anchored(p.clone(), ChainConfig::spv(&p, 10), services(&p), store.clone(), &pack)
            .expect("a valid pack");
    store.reset(meta(pack.hash, pack.height)).expect("reset");
    chain.accept(batch(&mine_n(&p, pack.hash, T0 + 150 * 25, 5)), None).expect("accept");
    let mut sink = Batches::default();
    let tip = chain.private_tip().expect("a tip").0;
    for wrong in [
        BlockRef {
            height: tip.height + 1,
            ..tip
        },
        BlockRef {
            height: ANCHOR + 4,
            ..chain.block_ref_of(chain.best_header()).expect("resident")
        },
    ] {
        let r = chain.write_header_step(Some(wrong), &mut sink);
        assert!(matches!(r, Err(ChainError::Internal(_))), "{r:?}");
    }
    assert!(sink.0.is_empty(), "a refusal writes nothing");
    stages(&mut chain, &mut store);
    check(&chain, &store);
    assert_eq!(store.active_from(0).expect("read").first(), Some(&(ANCHOR, BlockId(24))));
    assert_eq!(store.active_from(0).expect("read").len(), 6);
}

/// The new ids must be every id issued since the store's largest: an id marked dirty past
/// `next_id` is refused.
#[test]
fn write_header_step_new_ids_dense() {
    let p = params(Network::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, _store) = committed(&p, &pack, services(&p));
    chain.mark_dirty(BlockId(chain.next_id));
    let r = chain.write_header_step(None, &mut Batches::default());
    assert!(matches!(r, Err(ChainError::Internal(_))), "{r:?}");
}

/// The row buffer sends full batches as they fill and never an empty one.
#[test]
fn row_buffer_batches() {
    for (n, want) in [(0, vec![]), (8, vec![4, 4]), (9, vec![4, 4, 1])] {
        let mut sink = Batches::default();
        let mut out = super::RowBuffer {
            rows: Vec::new(),
            cap: 4,
            sink: &mut sink,
        };
        for i in 0..n {
            out.push(StepRow::LeafRemove(BlockId(i))).expect("push");
        }
        out.flush().expect("flush");
        assert_eq!(sink.0.iter().map(Vec::len).collect::<Vec<_>>(), want, "{n} rows");
    }
}

/// SB4 A-33 for the header-batch step: a stage-3 failure, in the rows or the commit, publishes
/// nothing, evicts nothing and leaves the store as it was; a failed write trips the latch. The
/// timer, received-lock, late-header and reset-only steps are SB4 C6's.
#[test]
fn spv_step_commit_failure_batch() {
    let p = params(Network::Regtest);
    let pack = regtest_pack(&p);
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 40);
    for rows_fail in [true, false] {
        let latch = Arc::new(Latch::default());
        let svc = Services {
            fail: latch.clone(),
            ..services(&p)
        };
        let (mut chain, mut store) = committed(&p, &pack, svc);
        real_step(&mut chain, &mut store, &trunk[..20]);
        let (image, snap) = (store.image(), chain.snapshot());
        let (resident_ids, epoch) = (chain.resident.clone(), chain.residency_epoch);

        chain.accept(batch(&trunk[20..]), None).expect("accept");
        mark(&mut chain, BlockId(26));
        let target = plan(&mut chain);
        assert!(target.is_some(), "the step would move the tip, and evict");
        let (dirty, marks) = (chain.dirty.ids().to_vec(), chain.overlay_marks().len());
        chain.cfg.feeds.write_batch = 4;
        store.begin_step();
        match rows_fail {
            true => {
                // Mid-stream: the first batch is written.
                store.faults(|f| f.fail_rows(2));
                let r = chain.write_header_step(target, &mut store);
                assert!(matches!(r, Err(ChainError::Storage(_))), "{r:?}");
                assert!(latch.is_tripped());
            }
            false => {
                store.faults(|f| f.fail_commit(1));
                let tail = chain.write_header_step(target, &mut store).expect("stage 3's rows");
                assert!(store.commit_step(&tail, Durability::Buffered).is_err());
            }
        }
        // The recipe stops: no stage 4 or 5.
        assert_eq!(store.image(), image, "rows_fail = {rows_fail}");
        assert!(Arc::ptr_eq(&chain.snapshot(), &snap), "nothing published");
        assert!(resident_ids.is_subset(&chain.resident), "nothing evicted");
        assert_eq!(chain.residency_epoch, epoch);
        assert_eq!((chain.dirty.ids(), chain.overlay_marks().len()), (&dirty[..], marks));
    }
}

/// `test_util::append_spv_records` leaves the store the header steps leave: a trunk stepped in
/// chunks, then two stale branches, one of them past the window, against the same headers
/// appended in one step with their X11 hashes. Then a second call: a heavier branch from a
/// stored record (a reorg), and an equal-work sibling of its tip, which the lower id beats.
#[cfg(feature = "test-util")]
#[test]
fn append_spv_records_is_the_steps_image() {
    let p = params(Network::Regtest);
    let pack = regtest_pack(&p);
    let t = T0 + 150 * 25;
    let trunk = mine_n(&p, pack.hash, t, 70);
    let s1 = mine_n(&p, trunk[9].hash(), t + 150 * 10 + 7, 2);
    let s2 = mine_n(&p, trunk[64].hash(), t + 150 * 65 + 7, 3);
    let (mut chain, mut steps) = committed(&p, &pack, services(&p));
    for c in trunk.chunks(16) {
        real_step(&mut chain, &mut steps, c);
    }
    real_step(&mut chain, &mut steps, &s1);
    real_step(&mut chain, &mut steps, &s2);

    let (_, mut appended) = committed(&p, &pack, services(&p));
    let all: Vec<_> = trunk.iter().chain(&s1).chain(&s2).map(|h| (*h.raw(), h.hash())).collect();
    crate::test_util::append_spv_records(&mut appended, &all).expect("append");
    assert_eq!(appended.image(), steps.image());

    let b = mine_n(&p, trunk[59].hash(), t + 150 * 60 + 5, 12);
    let tie = mine_n(&p, b[10].hash(), t + 150 * 71 + 9, 1);
    real_step(&mut chain, &mut steps, &b);
    real_step(&mut chain, &mut steps, &tie);
    let b12 = steps.by_hash(&b[11].hash()).expect("read").expect("stored").id;
    assert_eq!(chain.private_tip().expect("a tip").0.id, b12, "the reorg; the tie stays out");
    let all: Vec<_> = b.iter().chain(&tie).map(|h| (*h.raw(), h.hash())).collect();
    crate::test_util::append_spv_records(&mut appended, &all).expect("append");
    assert_eq!(appended.image(), steps.image());
}
