//! C10 (Q3 / X10, D94): route A's mined getters on a caller-thread view. A reader thread holds the
//! facade's gate shared and reads the committed layer's mined rows through
//! `RawRows::with_mined`, with its own `ChainReader`, at the tip it pinned, while the committer
//! disconnects the blocks that mined two quorums and flushes them, as V's commit and flush do
//! after an attempt's release. The view is built before the disconnects commit, between the
//! flush's write and its release, or after the release, and every read is one state's answer:
//! the layer before the disconnects, or after them, never a mix. A reader parked inside its
//! view's build, between the index clone and the DB snapshot, holds the committer off. Read at
//! 58, which the disconnects leave the layer without, the state after answers at their common
//! ancestor, 33 (D94).

use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use dash_chain::ChainReader;
use dash_consensus::{BlockId, BlsCtx};
use dash_dml::creditpool::CreditPool;
use dash_dml::store::MinedErase;
use dash_dml::{EvoDelta, StoreError};
use dash_primitives::BlockHash;
use dashd_store::MemoryEngine;

use super::llmq_members::{TEST, params, world};
use super::node_evo::{Store, node_evo_over_blocks};
use super::on_v_binding::{CbTxBlocks, FORKS, list, mined_row, seed_mined};
use super::support::{Mined, Rig, RigConfig, branch};
use crate::evo::ev1::Ev1NotBuilt;
use crate::evo::node::{CommittedMinedViews, SharedEvoLayer};
use crate::evo::testkit;
use crate::flush::EffectLedger;
use crate::flush::EvoWindow as _;
use crate::llmq::facade::MembersFacade;
use crate::llmq::rows::RuntimeRows;
use crate::store::GlobalFailStop;

/// The quorum based at 24 and mined at 34: durable when the reader starts.
const DURABLE: (u32, u32) = (24, 34);
/// The quorum based at 48 and mined at 58, the reader's tip: in the active layer.
const ACTIVE: (u32, u32) = (48, 58);
const TIP: u32 = 58;
/// How long a committer the reader holds off must still be waiting.
const HOLD: Duration = Duration::from_secs(2);

/// The reader's answers at the tip it pinned: each quorum's mining block, and `mined_until`'s
/// bases.
#[derive(Debug, PartialEq)]
struct Reads {
    durable: Option<BlockHash>,
    active: Option<BlockHash>,
    until: Vec<BlockId>,
}

/// The committed layer to 58 with both quorums' rows, 34's durable and 58's active, the facade
/// whose route A reads it, and a `ChainReader` of V's.
struct Fixture {
    blocks: Vec<Mined>,
    ids: Vec<BlockId>,
    reader: ChainReader,
    store: Store,
    layer: SharedEvoLayer,
    views: Arc<CommittedMinedViews<MemoryEngine>>,
    facade: Arc<MembersFacade>,
    _rig: Rig,
}

impl Fixture {
    fn new() -> Fixture {
        let p = Arc::new(params(FORKS, FORKS));
        let blocks = branch(&p, &Mined::genesis(&p), TIP, 3);
        let rig = Rig::with(RigConfig {
            params: Some(p.clone()),
            ..RigConfig::default()
        });
        rig.headers(&blocks);
        let ids = rig.ids(&blocks);
        let (send, got) = mpsc::channel();
        rig.on_v(move |v| {
            send.send(v.reader.clone()).expect("the test waits");
            Ok(())
        });
        let reader = got.recv().expect("V sent its reader");
        let (_, layer, store) =
            node_evo_over_blocks(&p, BlsCtx::BASIC, Ev1NotBuilt, Box::new(CbTxBlocks), None);
        seed_mined(&store, &layer, &blocks[..ix(TIP)], &[(TEST, DURABLE.0, DURABLE.1)]);
        testkit::flush(&store, &mut layer.lock());
        let (prev, tip) = (&blocks[ix(TIP - 1)], &blocks[ix(TIP)]);
        let delta = EvoDelta {
            block: tip.hash(),
            height: TIP,
            dml: Some(testkit::dml(&list(prev.hash(), TIP - 1), &list(tip.hash(), TIP), false)),
            mined_commitments: vec![mined_row(TEST, &blocks[ix(ACTIVE.0)])],
            credit_data: None,
            credit_pool: Some(Arc::new(Ok(CreditPool::default()))),
            mnhf_signals: None,
            erased_mined: Vec::new(),
            evo_tip: Some(tip.hash()),
        };
        store.commit(&mut layer.lock(), &delta).expect("block 58 commits");
        let views = Arc::new(CommittedMinedViews::new(store.clone(), layer.clone()));
        let rows = Arc::new(RuntimeRows::new(store.clone(), Arc::new(EffectLedger::default())));
        let facade = MembersFacade::new(&p, rows, GlobalFailStop::new())
            .expect("in domain")
            .with_mined_views(views.clone());
        Fixture {
            blocks,
            ids,
            reader,
            store,
            layer,
            views,
            facade: Arc::new(facade),
            _rig: rig,
        }
    }

    fn at(&self, height: u32) -> BlockId {
        self.ids[ix(height)]
    }

    fn hash(&self, height: u32) -> BlockHash {
        self.blocks[ix(height)].hash()
    }

    /// What a read at 58 answers before the disconnects: both quorums, newest first.
    fn before(&self) -> Reads {
        Reads {
            durable: Some(self.hash(DURABLE.1)),
            active: Some(self.hash(ACTIVE.1)),
            until: vec![self.at(ACTIVE.0), self.at(DURABLE.0)],
        }
    }

    fn disconnect(&self) {
        disconnect(&self.blocks, &self.store, &self.layer);
    }

    fn take_and_write(&self) {
        take_and_write(&self.store, &self.layer);
    }

    fn release(&self) {
        self.layer.lock().release();
    }

    /// The committer on its own thread, as V: the disconnects, the flush's write and its
    /// release, then a word on `finished`.
    fn commit_on_thread(&self, finished: mpsc::Sender<()>) -> thread::JoinHandle<()> {
        let (blocks, store, layer) = (self.blocks.clone(), self.store.clone(), self.layer.clone());
        thread::spawn(move || {
            disconnect(&blocks, &store, &layer);
            take_and_write(&store, &layer);
            layer.lock().release();
            finished.send(()).expect("the test waits");
        })
    }

    /// The reader: on its own thread, under the facade's gate, one view at 58.
    fn read(&self) -> thread::JoinHandle<Reads> {
        let (facade, reader, tip) = (self.facade.clone(), self.reader.clone(), self.at(TIP));
        let q = |base: u32| testkit::quorum_hash(&self.hash(base));
        let (durable, active) = (q(DURABLE.0), q(ACTIVE.0));
        thread::spawn(move || {
            let token = facade.external().expect("no failure latched");
            let rows = token.rows();
            let got = rows.with_mined(&reader, |m| {
                let mined = |q| m.mined_commitment(TEST, q, tip).expect("a read").map(|r| r.1);
                Reads {
                    durable: mined(&durable),
                    active: mined(&active),
                    until: m.mined_until(TEST, tip, 3).expect("a read"),
                }
            });
            got.expect("the facade reads the layer")
        })
    }

    /// Runs `step` on the reader's thread once its next view is built, before it reads.
    fn after_view(&self, step: impl FnOnce() + Send + 'static) {
        let mut hook = self.views.after_view.lock().expect("the hook");
        *hook = Some(Box::new(step));
    }

    /// Runs `step` on the reader's thread inside its next view's build, with the layer's lock
    /// held: after the index clone, before the DB snapshot.
    fn in_view(&self, step: impl FnOnce() + Send + 'static) {
        let mut hook = self.views.in_view.lock().expect("the hook");
        *hook = Some(Box::new(step));
    }
}

fn ix(height: u32) -> usize {
    usize::try_from(height - 1).expect("a small height")
}

/// The committer's disconnects, 58 down to 34, each erasing the rows its block mined.
fn disconnect(blocks: &[Mined], store: &Store, layer: &SharedEvoLayer) {
    for height in (DURABLE.1..=TIP).rev() {
        let base = [DURABLE, ACTIVE].into_iter().find(|q| q.1 == height).map(|q| q.0);
        let delta = undo(blocks, height, base);
        store.commit(&mut layer.lock(), &delta).expect("the undo commits");
    }
}

/// The flush's window taken and written durably, not yet released.
fn take_and_write(store: &Store, layer: &SharedEvoLayer) {
    let tip = layer.lock().evo_tip();
    let window = layer.lock().take();
    testkit::write_b1(store, &window, tip);
}

/// The disconnect delta of the block at `height`: the tombstone of the row of the quorum it
/// mined, based at `base`, if any; the evo tip back at its parent.
fn undo(blocks: &[Mined], height: u32, base: Option<u32>) -> EvoDelta {
    let (prev, block) = (&blocks[ix(height - 1)], &blocks[ix(height)]);
    let erased_mined = base.map(|base| MinedErase {
        llmq_type: TEST,
        quorum_hash: testkit::quorum_hash(&blocks[ix(base)].hash()),
        mined_height: height,
        rotation_index: None,
    });
    EvoDelta {
        block: block.hash(),
        height,
        dml: None,
        mined_commitments: Vec::new(),
        credit_data: None,
        credit_pool: None,
        mnhf_signals: None,
        erased_mined: erased_mined.into_iter().collect(),
        evo_tip: Some(prev.hash()),
    }
}

/// Built before the disconnects commit, the view reads the layer before them, whatever the
/// committer then commits, writes and releases before the reader reads; a view built after
/// reads the layer after them.
#[test]
fn a_view_built_before_the_commit_reads_the_state_before_it() {
    let f = Fixture::new();
    let (go, wait_go) = mpsc::channel();
    let (done, wait_done) = mpsc::channel();
    f.after_view(move || {
        go.send(()).expect("the committer waits");
        wait_done.recv().expect("the committer ran");
    });
    let reader = f.read();
    wait_go.recv().expect("the view is built");
    f.disconnect();
    f.take_and_write();
    f.release();
    done.send(()).expect("the reader waits");
    assert_eq!(reader.join().expect("the reader"), f.before());
    assert_eq!(f.read().join().expect("the reader"), at_a_dropped_tip(), "a new view");
}

/// Built between the flush's write and its release, the view reads the layer after the
/// disconnects: the frozen window over the written rows, and the written rows once released.
#[test]
fn a_view_built_between_the_write_and_the_release_reads_the_state_after() {
    let f = Fixture::new();
    assert_eq!(f.read().join().expect("the reader"), f.before());
    f.disconnect();
    f.take_and_write();
    let (go, wait_go) = mpsc::channel();
    let (done, wait_done) = mpsc::channel();
    f.after_view(move || {
        go.send(()).expect("the committer waits");
        wait_done.recv().expect("the committer ran");
    });
    let reader = f.read();
    wait_go.recv().expect("the view is built");
    f.release();
    done.send(()).expect("the reader waits");
    assert_eq!(reader.join().expect("the reader"), at_a_dropped_tip());
}

/// Built after the release, the view reads the written rows alone.
#[test]
fn a_view_built_after_the_release_reads_the_state_after() {
    let f = Fixture::new();
    f.disconnect();
    f.take_and_write();
    f.release();
    assert_eq!(f.read().join().expect("the reader"), at_a_dropped_tip());
}

/// Parked inside its view's build, between the index clone and the DB snapshot, the reader holds
/// the layer's lock (D94), so the committer cannot commit the disconnects until it resumes, and
/// the view reads the layer before them. A lock released between the two would let the committer
/// commit, write and release there, and the snapshot would read durable 34 gone beside active 58
/// present: neither state.
#[test]
fn a_reader_holds_the_layer_from_its_index_clone_to_its_snapshot() {
    let f = Fixture::new();
    let (go, wait_go) = mpsc::channel();
    let (resume, wait_resume) = mpsc::channel();
    f.in_view(move || {
        go.send(()).expect("the test waits");
        wait_resume.recv().expect("the test resumes the reader");
    });
    let reader = f.read();
    wait_go.recv().expect("the reader cloned the indexes");
    let (finished, wait_finished) = mpsc::channel();
    let committer = f.commit_on_thread(finished);
    let held = wait_finished.recv_timeout(HOLD);
    assert_eq!(held, Err(RecvTimeoutError::Timeout), "the committer waits for the layer");
    resume.send(()).expect("the reader waits");
    assert_eq!(reader.join().expect("the reader"), f.before());
    committer.join().expect("the committer");
    assert_eq!(wait_finished.try_recv(), Ok(()));
    assert_eq!(f.read().join().expect("the reader"), at_a_dropped_tip(), "a new view");
}

/// A facade built without the layer's mined rows has none to read.
#[test]
fn a_facade_without_the_layer_reads_no_mined_rows() {
    let f = Fixture::new();
    let p = params(FORKS, FORKS);
    let w = world(&p);
    let token = w.facade.external().expect("no failure latched");
    let got = token.rows().with_mined(&f.reader, |_| ());
    assert!(matches!(got, Err(StoreError::NotBuilt(_))), "{got:?}");
}

/// What a read pinned at 58 answers once the disconnects leave the layer without 58: the answers
/// at their common ancestor, 33, where neither quorum is mined.
fn at_a_dropped_tip() -> Reads {
    Reads {
        durable: None,
        active: None,
        until: Vec::new(),
    }
}
