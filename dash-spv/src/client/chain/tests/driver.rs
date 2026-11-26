//! The driver against dash-chain's reference step (PLAN-r1 §4.2): the same inputs as
//! `SpvRig::step_batch`, a fork target the reader does not hold, and a mark no stage 4 applied.

use std::cell::Cell;

use dash_chain::test_util::{SpvRig, RIG_ANCHOR};
use dash_chain::{
    BlockId, BlockRef, ChainConfig, ChainLockRef, ChainMeta, ClPersisted, Durability, HeaderChain,
    HeaderStop, Parker, Services, StaticSporks, Status, CHAIN_SCHEMA,
};
use dash_clock::NodeSeconds;
use dash_params::{ChainParamsBuilder, Network as Net, Release};

use super::step::{batch, bridged, mine, PEER, SPACING};
use super::*;
use crate::client::chain::step::{self, StepInput};

const RIG_NOW: NodeSeconds = NodeSeconds(1_700_000_000);

/// What a step publishes that both drivers decide.
fn facts(c: &Chain<Mem>) -> (BlockId, BlockHash, Option<BlockRef>, bool) {
    let s = c.snapshot();
    (s.tip.tip, s.tip.tip_hash, s.cl_floor, s.cl_enabled_observed)
}

fn rig_facts(rig: &SpvRig) -> (BlockId, BlockHash, Option<BlockRef>, bool) {
    let s = rig.chain.snapshot();
    (s.tip.tip, s.tip.tip_hash, s.cl_floor, s.cl_enabled_observed)
}

/// The rows a store holds from the anchor's context on, its `meta` without `sync_seq`.
fn rows(s: &impl HeaderStore) -> (Vec<HeaderRecord>, Vec<(u32, BlockId)>, ChainMeta) {
    let lo = RIG_ANCHOR - 24;
    let mut meta = s.chain_meta().unwrap().expect("a meta row");
    meta.sync_seq = 0;
    let mut recs = s.from_height(lo).unwrap();
    recs.sort_by_key(|r| r.id);
    (recs, s.active_from(lo).unwrap(), meta)
}

/// The rig's anchored chain over the SPV store, built as `build` builds one.
fn spv_twin(rig: &SpvRig, h: &Mem, cfg: ChainConfig) -> Chain<Mem> {
    let recs = rig.store.from_height(RIG_ANCHOR - 24).unwrap();
    let pack = AnchorPack {
        height: RIG_ANCHOR,
        hash: rig.anchor.hash(),
        chain_work: recs[24].chain_work,
        headers: recs.iter().map(|r| r.header).collect::<Vec<_>>().try_into().unwrap(),
    };
    h.reset(ChainMeta {
        schema: CHAIN_SCHEMA,
        anchor: (pack.hash, pack.height),
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
    })
    .unwrap();
    let svc = Services {
        clock: Arc::new(MockClock::at(RIG_NOW.0)),
        sporks: Arc::new(StaticSporks::for_network(&rig.params, Some(true))),
        fail: SpvFailStop::shared(),
    };
    let store = SpvChainStore(h.clone());
    let mut chain = HeaderChain::new_anchored(rig.params.clone(), cfg, svc, store, &pack).unwrap();
    let (tip, _) = chain.private_tip().unwrap();
    let mut step = h.begin_step().unwrap();
    let tail = chain.write_header_step(None, &mut step).unwrap();
    step.commit(&tail, Durability::Durable).unwrap();
    chain.activate_segmented(tip, &mut || {}).unwrap();
    chain.records_committed();
    chain
}

/// PLAN-r1 §4.2: batches, a lock and ticks through the SPV step and through
/// `SpvRig::step_batch` publish the same tip, floor and spork state, and leave the same rows.
/// The tick row compares a quiet step, and no input moves the ChainLock floor through a late
/// header, so the Headers arm's enforce is not compared.
#[test]
fn step_matches_spv_rig() {
    let params = Arc::new(ChainParamsBuilder::new(Net::Regtest, Release::V24_0).build().unwrap());
    let cfg = ChainConfig::spv(&params, HEADER_WINDOW);
    let mut rig = SpvRig::new(HEADER_WINDOW, cfg.feeds, cfg.cold_cache, true);
    let dir = tempfile::tempdir().unwrap();
    let h = mem(&dir);
    let mut chain = spv_twin(&rig, &h, cfg);
    assert_eq!(rows(&SpvChainStore(h.clone())), rows(&rig.store), "the anchor's step");

    let a = rig.mine_on_anchor(0, 6);
    let b = rig.mine(a[1].hash(), a[2].time() + 7, 2);
    let c = rig.mine(a[5].hash(), a[5].time() + 150, 2);
    let d = rig.mine(b[1].hash(), b[1].time() + 150, 2);
    let lock = ChainLockRef {
        height: RIG_ANCHOR + 4,
        hash: b[1].hash(),
    };
    let inputs: [(&str, Vec<Header>, Option<ChainLockRef>); 6] = [
        ("a", a, None),
        ("b", b, None),
        ("lock", vec![], Some(lock)),
        ("tick", vec![], None),
        ("c", c, None),
        ("d", d, None),
    ];
    let parker = Parker::new();
    for (what, headers, lock) in inputs {
        let input = match (&headers[..], lock) {
            ([], Some(cl)) => StepInput::ChainLock(cl),
            ([], None) => StepInput::Tick,
            (hs, _) => batch(hs),
        };
        let out = step::step(&mut chain, &h, input, &parker, RIG_NOW).unwrap();
        let stop = out.stop.map(|stop| match stop {
            HeaderStop::Invalid {
                reject,
                ..
            } => reject.reject_reason().to_owned(),
            other => format!("{other:?}"),
        });
        let behind_the_lock = (what == "c").then(|| "bad-prevblk-chainlock".to_owned());
        assert_eq!(stop, behind_the_lock, "{what}");
        if what == "tick" {
            rig.chain.check_cl_active_state();
        }
        rig.step(&headers, lock.as_slice(), &mut |_| {}).unwrap();
        assert_eq!(facts(&chain), rig_facts(&rig), "{what}");
        assert_eq!(rows(&SpvChainStore(h.clone())), rows(&rig.store), "{what}");
    }
    assert_eq!(chain.snapshot().tip.tip_hash, rig.chain.snapshot().tip.tip_hash);
}

thread_local! {
    static HIDE_TARGET: Cell<bool> = const { Cell::new(false) };
}

/// Whether this thread's reads of the fork target answer that it is not held.
pub(in crate::client::chain) fn target_hidden() -> bool {
    HIDE_TARGET.get()
}

/// A store whose jobs read no fork target.
#[derive(Clone)]
struct Hiding(Mem);

impl StoreHandle for Hiding {
    type Engine = MemoryEngine;
    type Io = <Mem as StoreHandle>::Io;

    fn store(&self) -> &Store<MemoryEngine, Self::Io> {
        self.0.store()
    }

    fn job<T: Send + 'static>(
        &self,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> impl std::future::Future<Output = Result<T, SpvError>> + Send {
        self.0.job(move || {
            HIDE_TARGET.set(true);
            let out = f();
            HIDE_TARGET.set(false);
            out
        })
    }
}

/// PLAN-r1 §2.2.2: a fork target `try_entry` does not hold is fatal before stage 3: the latch
/// trips, nothing is committed or published, and the task stops.
#[tokio::test]
async fn fork_target_missing_entry_is_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let h = mem(&dir);
    let s = setup(false, None);
    let host = SpvChainHost::open_on(Hiding(h.clone()), s.clone()).await.unwrap();
    let mut source = host.source();
    let SourceState::Live {
        reader,
        ..
    } = source.borrow().state.clone()
    else {
        panic!("not live");
    };
    let (epoch, hi) = (reader.snapshot().epoch, h.view().active.hi);
    let headers = mine(pack(20000, 1_000_000).hash, 1_000_000, 3, 1);
    let out = host.handle().headers(PEER, bridged(&headers)).await;
    assert!(out.is_err(), "{out:?}");
    let why = s.fail.reason().expect("tripped");
    assert!(why.contains("the fork target is not held"), "{why}");
    assert_eq!((reader.snapshot().epoch, h.view().active.hi), (epoch, hi), "nothing published");
    assert!(h.by_hash(&headers[0].hash()).unwrap().is_none(), "nothing committed");
    source.wait_for(|now| matches!(now.state, SourceState::Stopped)).await.unwrap();
}

/// SB5's kept mark: a lock step whose driver commits A3's mark with the tip kept and skips stage
/// 4 (`records_committed`'s third case). Readers keep A3 unmarked; the next step, a tick, writes
/// the mark again and its stage 4 applies it with the move to the lock.
#[test]
fn kept_mark_applied_by_next_step() {
    let dir = tempfile::tempdir().unwrap();
    let h = mem(&dir);
    let s = setup(false, None);
    let (mut chain, _) = open_chain(&h, &s).unwrap();
    let (parker, now) = (Parker::new(), NodeSeconds(2_000_000));
    let a = mine(pack(20000, 1_000_000).hash, 1_000_000, 6, 1);
    let b = mine(a[1].hash(), 1_000_000 + 2 * SPACING, 2, 2);
    for hs in [&a, &b] {
        step::step(&mut chain, &h, batch(hs), &parker, now).unwrap();
    }
    let reader = chain.reader();
    let id = |hash: BlockHash| reader.try_lookup(&hash).unwrap().expect("held");
    let status = |hash| reader.try_entry(id(hash)).unwrap().expect("held").status;
    let lock = ChainLockRef {
        height: 20004,
        hash: b[1].hash(),
    };
    let mut vt = chain.virtual_tip();
    chain.apply_chainlock(lock, &mut vt).unwrap();
    let mut st = h.begin_step().unwrap();
    let tail = chain.write_header_step(None, &mut st).unwrap();
    st.commit(&tail, Durability::Buffered).unwrap();
    chain.records_committed();
    let stored = h.by_hash(&a[2].hash()).unwrap().unwrap().status;
    assert!(stored.has(Status::CONFLICT_CHAINLOCK), "committed at stage 3");
    assert!(!status(a[2].hash()).has(Status::CONFLICT_CHAINLOCK), "kept, not applied");
    assert_eq!(chain.snapshot().tip.tip_hash, a[5].hash());

    let epoch = chain.snapshot().epoch;
    step::step(&mut chain, &h, StepInput::Tick, &parker, now).unwrap();
    assert!(status(a[2].hash()).has(Status::CONFLICT_CHAINLOCK), "applied by the next step");
    let snap = chain.snapshot();
    assert_eq!((snap.tip.tip_hash, snap.epoch), (b[1].hash(), epoch + 1));
    assert!(!s.fail.is_tripped());
}
