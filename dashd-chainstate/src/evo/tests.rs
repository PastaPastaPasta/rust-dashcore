//! The evo store against dash-dml's `MemEvoStore` (CH7 §7.2): seeded random connects (new
//! blocks and reconnects), disconnects, freezes, B1 writes and effect writes, with every reader
//! compared after each operation. The by-hash rows are compared for every block ever connected;
//! the mined rows on the active chain only, as the node's are the active chain's (undo erases them,
//! CH7 Q1) while `MemEvoStore` keeps every branch's.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use dash_consensus::{BlockId, ChainAncestry};
use dash_dml::creditpool::{CreditBlockData, CreditPool, PoolError, PoolState};
use dash_dml::mnhf::EhfSignals;
use dash_dml::store::MinedCommitmentRecord;
use dash_dml::testkit::{MemChain, numbered};
use dash_dml::{DmlReader, EvoDelta, EvoStore, MemEvoStore, MnList, SameRecord};
use dash_evo::llmq::{MinedCommitmentReader, MinedFamily, MinedIndexEntry, QuorumSnapshot};
use dash_params::LlmqType;
use dash_primitives::{BlockHash, QuorumHash};
use dashd_store::{Commit, ErrorKind, KvBatch, KvEngine, KvRead};

use super::testkit::{self, credit_data, memory_store};
use super::{EvoLayer, EvoReadView, EvoWindowDelta, RocksEvoStore, codec};
use crate::flush::EvoWindow;
use crate::store::{GuardedEngine, GuardedSnapshot, RecordingSink};

/// xorshift64*: the sequences' seeded source.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

/// The families the sequences mine into: a non-rotated type, and a rotated one at two indexes.
const FAMILIES: [(LlmqType, Option<i16>); 3] = [
    (LlmqType::Llmq50_60, None),
    (LlmqType::LlmqTestDip0024, Some(0)),
    (LlmqType::LlmqTestDip0024, Some(1)),
];

struct Sim<E: KvEngine> {
    rng: Rng,
    chain: MemChain,
    store: RocksEvoStore<E>,
    layer: EvoLayer,
    mem: MemEvoStore,
    tip: BlockId,
    /// Each block's connect delta, which a reconnect commits again.
    deltas: BTreeMap<BlockId, EvoDelta>,
    /// The list after each connected block.
    lists: BTreeMap<BlockId, MnList>,
    /// Disconnected blocks, by parent.
    stale: BTreeMap<BlockId, Vec<BlockId>>,
    /// Every quorum ever mined, and every cycle base with a snapshot row.
    quorums: HashSet<(LlmqType, QuorumHash)>,
    snapshots: HashSet<(LlmqType, BlockHash)>,
    /// The window in flight, and the evo tip B1 writes with it (CH2's `new`).
    in_flight: Option<(EvoWindowDelta, Option<BlockHash>)>,
}

impl<E: KvEngine> Sim<E>
where
    for<'a> GuardedSnapshot<'a, E>: Sync,
{
    fn new(seed: u64, store: RocksEvoStore<E>) -> Self {
        let chain = MemChain::new();
        let tip = chain.genesis();
        Self {
            rng: Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1),
            chain,
            store,
            layer: EvoLayer::new(),
            mem: MemEvoStore::new(),
            tip,
            deltas: BTreeMap::new(),
            lists: BTreeMap::new(),
            stale: BTreeMap::new(),
            quorums: HashSet::new(),
            snapshots: HashSet::new(),
            in_flight: None,
        }
    }

    fn active(&self) -> Vec<BlockId> {
        let mut blocks = Vec::new();
        let mut cursor = Some(self.tip);
        while let Some(id) = cursor {
            blocks.push(id);
            cursor = self.chain.prev(id);
        }
        blocks
    }

    /// The quorums mined on the active chain.
    fn mined_here(&self) -> HashSet<(LlmqType, QuorumHash)> {
        self.active()
            .iter()
            .filter_map(|id| self.deltas.get(id))
            .flat_map(|d| d.mined_commitments.iter().map(|r| (r.llmq_type, r.quorum_hash)))
            .collect()
    }

    fn pool(&mut self) -> PoolState {
        match self.rng.below(3) {
            0 => Err(PoolError::NegativeLimit(-(self.rng.below(100) as i64))),
            1 => Err(PoolError::PrevLocked),
            _ => {
                let mut pool = CreditPool {
                    locked: self.rng.below(1 << 40) as i64,
                    ..CreditPool::default()
                };
                for _ in 0..self.rng.below(3) {
                    pool.indexes.add(self.rng.below(50));
                }
                Ok(pool)
            }
        }
    }

    /// A new block on the tip: a list change, credit rows, signals and commitments at random.
    fn new_block(&mut self) -> BlockId {
        let id = self.chain.child(self.tip);
        let hash = self.chain.hash(id);
        let height = self.chain.header(id).height;
        let prev = self.lists.get(&self.tip).cloned().unwrap_or_else(MnList::default_pre_dip3);
        let mut next = prev.clone().with_block_hash(hash).with_height(height as i32);
        if self.rng.chance(50) || next.is_empty() {
            next.try_add(numbered(next.total_registered()), true).unwrap();
        } else if self.rng.chance(40) {
            let victim = next.iter().nth(self.rng.below(next.len() as u64) as usize);
            let victim = victim.unwrap().pro_tx_hash;
            next.remove(&victim).unwrap();
        }
        let mut d = testkit::delta(&self.chain, id);
        d.dml = Some(testkit::dml(&prev, &next, height == 1 || height.is_multiple_of(4)));
        if self.rng.chance(60) {
            d.credit_data = Some(CreditBlockData {
                balance: self.rng.below(1000) as i64,
                unlocked: self.rng.below(10) as i64,
            });
        }
        if self.rng.chance(40) {
            d.credit_pool = Some(Arc::new(self.pool()));
        }
        if self.rng.chance(30) {
            let bit = self.rng.below(29) as u8;
            d.mnhf_signals = Some(EhfSignals::from_iter([(bit, height as i32)]));
        }
        let mined = self.mined_here();
        for (llmq_type, rotation) in FAMILIES {
            if !self.rng.chance(35) {
                continue;
            }
            // A base on this chain, at or below the block, not mined on it yet (LQ2's
            // `bad-qc-dup`), nor twice in the block.
            let base_height = self.rng.below(u64::from(height)) as u32;
            let base = self.chain.ancestor(id, base_height).unwrap();
            let record = testkit::mined(&self.chain, llmq_type, base, rotation);
            let key = (llmq_type, record.quorum_hash);
            if mined.contains(&key)
                || d.mined_commitments.iter().any(|r| (r.llmq_type, r.quorum_hash) == key)
            {
                continue;
            }
            self.quorums.insert(key);
            d.mined_commitments.push(record);
        }
        self.lists.insert(id, next);
        self.deltas.insert(id, d);
        id
    }

    fn connect(&mut self) {
        let stale = self.stale.get_mut(&self.tip).filter(|s| !s.is_empty());
        let id = match stale {
            Some(children) if self.rng.chance(50) => {
                children.remove(self.rng.below(children.len() as u64) as usize)
            }
            _ => self.new_block(),
        };
        let d = self.deltas[&id].clone();
        self.store.commit(&mut self.layer, &d).unwrap();
        self.mem.apply(&d).unwrap();
        self.tip = id;
    }

    fn disconnect(&mut self) {
        let id = self.tip;
        let prev = self.chain.prev(id).unwrap();
        let mined: Vec<MinedCommitmentRecord> = self.deltas[&id].mined_commitments.clone();
        self.store.commit(&mut self.layer, &testkit::undo(&self.chain, id, &mined)).unwrap();
        self.mem.disconnect(&self.chain.hash(id), Some(self.chain.hash(prev))).unwrap();
        self.stale.entry(prev).or_default().push(id);
        self.tip = prev;
    }

    /// A freeze, or the in-flight window's B1 and release.
    fn flush_step(&mut self) {
        match self.in_flight.take() {
            None => {
                let tip = self.layer.evo_tip();
                self.in_flight = Some((self.layer.take(), tip));
            }
            Some((window, tip)) => {
                testkit::write_b1(&self.store, &window, tip);
                self.layer.release();
            }
        }
    }

    /// A rotation snapshot written as an effect, at a block of the active chain.
    fn snapshot_effect(&mut self) {
        let active = self.active();
        let base = self.chain.hash(active[self.rng.below(active.len() as u64) as usize]);
        let snapshot = QuorumSnapshot {
            active_quorum_members: (0..self.rng.below(12)).map(|_| self.rng.chance(50)).collect(),
            skip_list_mode: self.rng.below(4) as i32,
            skip_list: (0..self.rng.below(3)).map(|_| self.rng.below(9) as i32).collect(),
        };
        let t = LlmqType::LlmqTestDip0024;
        self.store.store_quorum_snapshot(t, &base, &snapshot).unwrap();
        self.mem.store_quorum_snapshot(t, base, snapshot);
        self.snapshots.insert((t, base));
    }

    fn step(&mut self) {
        let height = self.chain.header(self.tip).height;
        match self.rng.below(10) {
            0..=4 => self.connect(),
            5 | 6 if height >= 2 => self.disconnect(),
            7 | 8 => self.flush_step(),
            9 => self.snapshot_effect(),
            _ => self.connect(),
        }
    }

    /// Every reader of the node's view equals `MemEvoStore`'s.
    fn compare(&self) {
        let view = self.store.view(&self.layer, &self.chain);
        let mem = self.mem.view(&self.chain);
        assert_eq!(view.best_block().unwrap(), mem.best_block().unwrap(), "best block");
        for id in self.deltas.keys() {
            let h = self.chain.hash(*id);
            let (a, b) = (view.mn_list_snapshot(&h).unwrap(), mem.mn_list_snapshot(&h).unwrap());
            assert!(a.same_record(&b), "mn_snapshot at {id:?}");
            let (a, b) = (view.mn_list_diff(&h).unwrap(), mem.mn_list_diff(&h).unwrap());
            assert!(a.same_record(&b), "mn_diff at {id:?}");
            assert_eq!(view.credit_pool(&h).unwrap(), mem.credit_pool(&h).unwrap(), "pool");
            assert_eq!(view.credit_data(&h).unwrap(), mem.credit_data(&h).unwrap(), "data");
            assert_eq!(view.mnhf_signals(&h).unwrap(), mem.mnhf_signals(&h).unwrap(), "mnhf");
        }
        for (t, base) in &self.snapshots {
            assert_eq!(
                view.quorum_snapshot(*t, base).unwrap(),
                mem.quorum_snapshot(*t, base).unwrap()
            );
        }
        for at in self.active() {
            let h = self.chain.header(at).height;
            for (t, q) in &self.quorums {
                let a = view.mined_commitment(*t, q, at).unwrap().map(|(c, b)| ((*c).clone(), b));
                let b = mem.mined_commitment(*t, q, at).unwrap().map(|(c, b)| ((*c).clone(), b));
                assert_eq!(a, b, "mined commitment {t:?} at {at:?}");
            }
            for (t, rotation) in FAMILIES {
                let family =
                    rotation.map_or(MinedFamily::NonRotated, |quorum_index| MinedFamily::Rotated {
                        quorum_index,
                    });
                for cap in [h, h / 2, u32::MAX] {
                    for limit in [0, 1, 3, usize::MAX] {
                        let a = view.mined_index(t, family, at, cap, limit).unwrap();
                        let b = mem.mined_index(t, family, at, cap, limit).unwrap();
                        assert_eq!(a, b, "{t:?} {family:?} at {at:?} cap {cap} limit {limit}");
                    }
                }
            }
        }
    }
}

fn run<E: KvEngine>(seed: u64, steps: usize, store: RocksEvoStore<E>)
where
    for<'a> GuardedSnapshot<'a, E>: Sync,
{
    let mut sim = Sim::new(seed, store);
    for _ in 0..steps {
        sim.step();
        sim.compare();
    }
    assert!(sim.chain.header(sim.tip).height > 0, "seed {seed} connected blocks");
}

#[test]
fn the_store_reads_as_mem_evo_store_over_random_sequences() {
    for seed in 0..48 {
        run(seed, 80, memory_store());
    }
}

#[cfg(feature = "rocksdb")]
#[test]
fn the_rocksdb_store_reads_as_mem_evo_store_over_random_sequences() {
    for seed in 0..6 {
        let dir = tempfile::tempdir().unwrap();
        run(seed, 80, testkit::rocks_store(dir.path()));
    }
}

/// A view is pinned (D22): commits, a freeze, B1 and the release after it change nothing it reads,
/// and a new view sees each change.
#[test]
fn a_view_is_pinned_at_its_moment() {
    let mut chain = MemChain::new();
    let b2 = chain.extend(chain.genesis(), 2);
    let b1 = chain.prev(b2).unwrap();
    let store = memory_store();
    let mut layer = EvoLayer::new();
    let mut d1 = testkit::delta(&chain, b1);
    d1.credit_data = Some(credit_data(1));
    store.commit(&mut layer, &d1).unwrap();
    let (h1, h2) = (chain.hash(b1), chain.hash(b2));
    {
        let pinned = store.view(&layer, &chain);
        let mut d2 = testkit::delta(&chain, b2);
        d2.credit_data = Some(credit_data(2));
        store.commit(&mut layer, &d2).unwrap();
        assert_eq!(pinned.credit_data(&h2).unwrap(), None);
        assert_eq!(pinned.best_block().unwrap(), Some(h1));
        assert_eq!(store.view(&layer, &chain).credit_data(&h2).unwrap(), Some(credit_data(2)));
    }
    let window = layer.take();
    let in_flight = store.view(&layer, &chain);
    assert_eq!(in_flight.credit_data(&h1).unwrap(), Some(credit_data(1)), "the frozen index");
    testkit::write_b1(&store, &window, Some(h2));
    layer.release();
    assert_eq!(in_flight.best_block().unwrap(), Some(h2));
    let after = store.view(&layer, &chain);
    assert_eq!(after.credit_data(&h1).unwrap(), Some(credit_data(1)), "the DB");
    assert_eq!(after.best_block().unwrap(), Some(h2), "meta.evo_tip");
}

/// D23, D31: a mined row is read through the ancestry of the block a read names, at every level. A
/// view pinned before a reorg still holds the old branch's row; reads at the other branch's blocks
/// never see it, and a later view sees only the new branch's row.
#[test]
fn mined_reads_keep_to_the_ancestry_they_name() {
    let mut chain = MemChain::new();
    let b4 = chain.extend(chain.genesis(), 4);
    let b3 = chain.prev(b4).unwrap();
    let c4 = chain.child(b3);
    let (b1, b2) = (chain.ancestor(b4, 1).unwrap(), chain.ancestor(b4, 2).unwrap());
    let llmq_type = LlmqType::Llmq50_60;
    let old = testkit::mined(&chain, llmq_type, b1, None);
    let new = testkit::mined(&chain, llmq_type, b2, None);
    let entry = |base_height| MinedIndexEntry {
        mined_height: 4,
        base_height,
    };
    let store = memory_store();
    let mut layer = EvoLayer::new();
    for id in [b1, b2, b3] {
        store.commit(&mut layer, &testkit::delta(&chain, id)).unwrap();
    }
    let mut d4 = testkit::delta(&chain, b4);
    d4.mined_commitments = vec![old.clone()];
    store.commit(&mut layer, &d4).unwrap();

    // On the old branch only, in the active index, then on disk.
    for flushed in [false, true] {
        if flushed {
            testkit::flush(&store, &mut layer);
        }
        let view = store.view(&layer, &chain);
        let read = |at| view.mined_commitment(llmq_type, &old.quorum_hash, at).unwrap().is_some();
        let index = |end| view.mined_index(llmq_type, MinedFamily::NonRotated, end, 9, 8).unwrap();
        assert_eq!((read(b4), read(c4), read(b3)), (true, false, false), "flushed {flushed}");
        assert_eq!((index(b4), index(c4)), (vec![entry(1)], vec![]), "flushed {flushed}");
    }

    // The reorg to c4, which mines another commitment of the type at the same height.
    let pinned = store.view(&layer, &chain);
    store.commit(&mut layer, &testkit::undo(&chain, b4, std::slice::from_ref(&old))).unwrap();
    let mut e4 = testkit::delta(&chain, c4);
    e4.mined_commitments = vec![new.clone()];
    store.commit(&mut layer, &e4).unwrap();
    let after = store.view(&layer, &chain);
    for (view, at, want) in [
        (&pinned, b4, vec![entry(1)]),
        (&pinned, c4, vec![]),
        (&after, c4, vec![entry(2)]),
        (&after, b4, vec![]),
    ] {
        let got = view.mined_index(llmq_type, MinedFamily::NonRotated, at, 9, 8).unwrap();
        assert_eq!(got, want, "at {at:?}");
    }
    let qc = |view: &EvoReadView<'_, _>, q: &MinedCommitmentRecord, at| {
        view.mined_commitment(llmq_type, &q.quorum_hash, at).unwrap().is_some()
    };
    assert_eq!((qc(&pinned, &old, b4), qc(&pinned, &new, c4)), (true, false));
    assert_eq!(
        (qc(&after, &old, b4), qc(&after, &new, c4), qc(&after, &new, b4)),
        (false, true, false)
    );
}

/// A DB fault in a mined read is the read's error, never a shorter answer (D5): the range read and
/// the point read over a probed chainstate, first healthy, then failing.
#[test]
fn a_db_fault_fails_the_mined_reads() {
    let mut chain = MemChain::new();
    let b2 = chain.extend(chain.genesis(), 2);
    let base = chain.prev(b2).unwrap();
    let llmq_type = LlmqType::Llmq50_60;
    let record = testkit::mined(&chain, llmq_type, base, None);
    let db = Arc::new(GuardedEngine::probe(Arc::new(RecordingSink::default())));
    let store = RocksEvoStore::new(db.clone(), testkit::codec(), testkit::genesis());
    let mut layer = EvoLayer::new();
    store.commit(&mut layer, &testkit::delta(&chain, base)).unwrap();
    let mut d2 = testkit::delta(&chain, b2);
    d2.mined_commitments = vec![record.clone()];
    store.commit(&mut layer, &d2).unwrap();
    testkit::flush(&store, &mut layer);

    let view = store.view(&layer, &chain);
    let index = || view.mined_index(llmq_type, MinedFamily::NonRotated, b2, 9, 8);
    let entry = MinedIndexEntry {
        mined_height: 2,
        base_height: 1,
    };
    assert_eq!(index().unwrap(), [entry], "read through the probe's snapshot");
    assert!(view.mined_commitment(llmq_type, &record.quorum_hash, b2).unwrap().is_some());
    // The engine's seek too: the type's height-index row, from the newest height down.
    let key = codec::index_key(llmq_type, None, 2);
    let (cf, key) = codec::split_mined_key(&key);
    let rows: Vec<_> = db.iter_prefix_from(cf.cf(), &key[..4], key).collect();
    assert_eq!(rows.len(), 1);
    db.inner().fail_reads(Some(ErrorKind::Io));
    assert!(db.iter_prefix_from(cf.cf(), &key[..4], key).any(|r| r.is_err()));
    assert!(index().is_err());
    assert!(view.mined_commitment(llmq_type, &record.quorum_hash, b2).is_err());
}

/// D7, D38: an evo tip naming genesis reads as absent at every level, not as the level below:
/// `meta.evo_tip` as B1 writes it after a flush at genesis, and the active and frozen indexes after
/// block 1's disconnect, whose tip is its parent, over a `meta.evo_tip` at block 1. CH2's freeze
/// check still holds that flush to genesis (D37).
#[test]
fn a_genesis_evo_tip_reads_as_absent() {
    let mut chain = MemChain::new();
    let b1 = chain.child(chain.genesis());
    let (genesis, h1) = (chain.hash(chain.genesis()), chain.hash(b1));
    let store = memory_store();
    let mut layer = EvoLayer::new();
    let best = |layer: &EvoLayer| store.view(layer, &chain).best_block().unwrap();
    testkit::write_b1(&store, &layer.take(), Some(genesis));
    layer.release();
    assert_eq!(best(&layer), None, "meta.evo_tip");
    store.commit(&mut layer, &testkit::delta(&chain, b1)).unwrap();
    assert_eq!(best(&layer), Some(h1), "the active index");
    testkit::flush(&store, &mut layer);
    assert_eq!(best(&layer), Some(h1), "meta.evo_tip at block 1");
    store.commit(&mut layer, &testkit::undo(&chain, b1, &[])).unwrap();
    assert_eq!(best(&layer), None, "the active index at genesis");
    let window = layer.take();
    assert_eq!(best(&layer), None, "the frozen index at genesis");
    assert_eq!(layer.best_block(), Some(genesis), "the freeze check's tip");
    testkit::write_b1(&store, &window, Some(genesis));
    layer.release();
    assert_eq!(best(&layer), None, "meta.evo_tip after the disconnect");
}

/// D39: a height-index row whose base is above its own mining block is a store fault, in both
/// families, where Core's `GetAncestor` assert fires; a base at the mining height reads.
#[test]
fn an_index_row_based_above_its_mining_block_is_a_fault() {
    let mut chain = MemChain::new();
    let b4 = chain.extend(chain.genesis(), 4);
    let llmq_type = LlmqType::LlmqTestDip0024;
    let store = memory_store();
    let layer = EvoLayer::new();
    let families = [
        (None, MinedFamily::NonRotated),
        (
            Some(1),
            MinedFamily::Rotated {
                quorum_index: 1,
            },
        ),
    ];
    for (rotation, family) in families {
        for (base_height, reads) in [(4, true), (5, false)] {
            let key = codec::index_key(llmq_type, rotation, 4);
            let (cf, key) = codec::split_mined_key(&key);
            let mut b = store.db().batch();
            b.put(cf.cf(), key, &codec::encode_index(base_height, &chain.hash(b4)));
            store.db().write(b, Commit::Durable).unwrap();
            let got = store.view(&layer, &chain).mined_index(llmq_type, family, b4, 9, 8);
            let want = MinedIndexEntry {
                mined_height: 4,
                base_height,
            };
            assert_eq!(got.ok(), reads.then(|| vec![want]), "{family:?}, base {base_height}");
        }
    }
}

/// D22: an upper level's tombstone hides the row below it from both mined reads, at a block whose
/// ancestry still holds the erased row's mining block: the active index over the frozen one, the
/// active index over the DB, and the frozen index over the DB.
#[test]
fn a_tombstone_hides_the_row_below_from_the_mined_reads() {
    let mut chain = MemChain::new();
    let b4 = chain.extend(chain.genesis(), 4);
    let b1 = chain.ancestor(b4, 1).unwrap();
    let llmq_type = LlmqType::Llmq50_60;
    let record = testkit::mined(&chain, llmq_type, b1, None);
    let entry = MinedIndexEntry {
        mined_height: 4,
        base_height: 1,
    };
    for placement in ["active over frozen", "active over the DB", "frozen over the DB"] {
        let store = memory_store();
        let mut layer = EvoLayer::new();
        for height in 1..4 {
            let id = chain.ancestor(b4, height).unwrap();
            store.commit(&mut layer, &testkit::delta(&chain, id)).unwrap();
        }
        let mut d4 = testkit::delta(&chain, b4);
        d4.mined_commitments = vec![record.clone()];
        store.commit(&mut layer, &d4).unwrap();
        let reads = |layer: &EvoLayer| {
            let view = store.view(layer, &chain);
            let index = view.mined_index(llmq_type, MinedFamily::NonRotated, b4, 9, 8).unwrap();
            let qc = view.mined_commitment(llmq_type, &record.quorum_hash, b4).unwrap();
            (index, qc.is_some())
        };
        if placement == "active over frozen" {
            layer.take();
        } else {
            testkit::flush(&store, &mut layer);
        }
        assert_eq!(reads(&layer), (vec![entry], true), "{placement}: before the undo");
        store
            .commit(&mut layer, &testkit::undo(&chain, b4, std::slice::from_ref(&record)))
            .unwrap();
        if placement == "frozen over the DB" {
            layer.take();
        }
        assert_eq!(reads(&layer), (vec![], false), "{placement}");
    }
}
