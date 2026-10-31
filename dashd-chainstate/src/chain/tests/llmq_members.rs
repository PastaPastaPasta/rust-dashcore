//! The facade's members call and precompute loop (CH7 §4.2.1-§4.2.2) inside attempts on a real V
//! loop, over LQ1's LLMQ_TEST_DIP0024 regtest fixture (`dash-evo/src/llmq/tests.rs`) with the
//! node's lists: Core's cache order, the precompute's targets, the semantic gate before any cache
//! (F4), and a differential against LQ1's model of the facade in its attempt (`ModelAttempt`).

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::{Arc, OnceLock, mpsc};

use dash_consensus::ctx::{Deployment, DeploymentView};
use dash_consensus::{BlockId, ChainAncestry, HeaderCtx};
use dash_dml::testkit::{MemChain, numbered};
use dash_dml::{Dmn, MnList, StoreError};
use dash_evo::Effect;
use dash_evo::bls_ctx_after;
use dash_evo::cbtx::CoinbaseFacts;
use dash_evo::llmq::reference::{ModelAttempt, ModelRows};
use dash_evo::llmq::{
    CompleteChain, LlmqChain, LlmqCtx, LlmqError, LlmqSource, MemberError, PrecomputeMode,
    QuorumMembers, QuorumSnapshot, SnapshotRows, compute_by_quarter_rotation,
};
use dash_evo::types::ProTxHash;
use dash_params::{ChainOption, ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_primitives::{BlockHash, LlmqType};
use dash_types::Numeric;
use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};

use super::llmq::appliers;
use super::support::Rig;
use crate::chain::Fatal;
use crate::chain::actor::V;
use crate::connect::AttemptId;
use crate::flush::EffectLedger;
use crate::llmq::facade::{AttemptHandle, MembersFacade};
use crate::llmq::members::{members, precompute};
use crate::llmq::rows::{RuntimeRows, SnapshotStore};
use crate::store::GlobalFailStop;

/// Size 4, one member a quarter, two quorums a cycle, interval 24.
pub(super) const ROTATED: LlmqType = LlmqType::LlmqTestDip0024;
/// Not rotated, interval 24.
pub(super) const TEST: LlmqType = LlmqType::LlmqTest;
pub(super) const TIP: u32 = 120;
pub(super) const NEVER: u32 = 10_000;

/// Regtest with DIP0003 and V19 from 2, and V20 (with MN_RR) and DIP0024 at the given heights.
pub(super) fn params(v20: u32, dip24: u32) -> ChainParams {
    [
        (ChainOption::Dip3Params, "2:2".to_owned()),
        (ChainOption::TestActivationHeight, format!("v20@{v20}")),
        (ChainOption::TestActivationHeight, format!("mn_rr@{v20}")),
        (ChainOption::TestActivationHeight, "v19@2".to_owned()),
        (ChainOption::TestActivationHeight, format!("dip0024@{dip24}")),
    ]
    .into_iter()
    .fold(ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0), |b, (o, v)| b.arg(o, v))
    .build()
    .expect("regtest parameters")
}

/// Buried deployments at their heights on `chain`.
struct Buried<'a> {
    chain: &'a MemChain,
    params: &'a ChainParams,
}

impl DeploymentView for Buried<'_> {
    fn active_after(&self, prev: Option<BlockId>, deployment: Deployment) -> bool {
        let next = prev.map_or(0, |p| self.chain.header(p).height + 1);
        match deployment {
            Deployment::Buried(d) => next >= self.params.consensus.deployment_height(d).to_u32(),
            Deployment::Versionbits(_) => false,
        }
    }

    fn active_at(&self, block: BlockId, deployment: Deployment) -> bool {
        self.active_after(self.chain.prev(block), deployment)
    }
}

/// The chain below `floor` withheld: every read of it fails, as a view that lost its ancestry.
struct Withholding<'a> {
    chain: CompleteChain<'a, StoreError>,
    floor: u32,
}

impl Withholding<'_> {
    fn held(&self, height: u32) -> Result<(), StoreError> {
        if height < self.floor {
            return Err(StoreError::read(format!("block {height} withheld")));
        }
        Ok(())
    }
}

impl LlmqChain for Withholding<'_> {
    type Error = StoreError;

    fn header(&self, id: BlockId) -> Result<HeaderCtx, StoreError> {
        let header = self.chain.header(id)?;
        self.held(header.height)?;
        Ok(header)
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Result<Option<BlockId>, StoreError> {
        self.held(height)?;
        self.chain.ancestor(id, height)
    }

    fn prev(&self, id: BlockId) -> Result<Option<BlockId>, StoreError> {
        let prev = self.chain.prev(id)?;
        if let Some(prev) = prev {
            self.held(self.chain.header(prev)?.height)?;
        }
        Ok(prev)
    }
}

/// A linear chain `0 ..= TIP` and its parameters.
pub(super) struct Fixture {
    pub(super) params: ChainParams,
    chain: MemChain,
    tip: BlockId,
}

impl Fixture {
    pub(super) fn new(v20: u32, dip24: u32) -> Fixture {
        let mut chain = MemChain::new();
        let tip = chain.extend(chain.genesis(), TIP);
        Fixture {
            params: params(v20, dip24),
            chain,
            tip,
        }
    }

    /// LLMQ_TEST_DIP0024 rotating from DIP0024 at 2, before V20 (LQ1's `rotated_fixture`).
    pub(super) fn rotated() -> Fixture {
        Fixture::new(NEVER, 2)
    }

    pub(super) fn at(&self, height: u32) -> BlockId {
        self.chain.ancestor(self.tip, height).expect("on the chain")
    }

    pub(super) fn hash(&self, height: u32) -> BlockHash {
        self.chain.hash(self.at(height))
    }

    /// `f` over the fixture's LLMQ context, with the tip's scheme.
    pub(super) fn ctx<R>(&self, f: impl FnOnce(&LlmqCtx<'_, StoreError>) -> R) -> R {
        let chain = CompleteChain::<StoreError>::new(&self.chain);
        self.ctx_over(&chain, f)
    }

    fn ctx_over<R>(
        &self,
        chain: &dyn LlmqChain<Error = StoreError>,
        f: impl FnOnce(&LlmqCtx<'_, StoreError>) -> R,
    ) -> R {
        let deployments = Buried {
            chain: &self.chain,
            params: &self.params,
        };
        let bls = bls_ctx_after(&deployments, Some(self.tip));
        f(&LlmqCtx::new(&self.params, chain, &deployments, bls).expect("in domain"))
    }

    /// Nine masternodes at every height.
    pub(super) fn source(&self) -> Source<'_> {
        self.source_with(BTreeMap::new())
    }

    /// Nine masternodes at every height but those `lists` names.
    pub(super) fn source_with(&self, lists: BTreeMap<u32, Vec<u32>>) -> Source<'_> {
        Source {
            chain: &self.chain,
            lists,
            built: RefCell::default(),
            list_reads: RefCell::default(),
            coinbase_reads: Cell::new(0),
        }
    }
}

/// Masternodes `1 ..= 9`, built once.
fn masternodes() -> &'static [Arc<Dmn>] {
    static MNS: OnceLock<Vec<Arc<Dmn>>> = OnceLock::new();
    MNS.get_or_init(|| (1..=9).map(numbered).collect())
}

/// The lists of the fixture's blocks, and the reads it served. Blocks have no transactions, so
/// the modifier is the hash form.
pub(super) struct Source<'a> {
    chain: &'a MemChain,
    lists: BTreeMap<u32, Vec<u32>>,
    built: RefCell<BTreeMap<u32, Arc<MnList>>>,
    pub(super) list_reads: RefCell<Vec<u32>>,
    coinbase_reads: Cell<usize>,
}

impl Source<'_> {
    /// Every read served so far.
    pub(super) fn reads(&self) -> usize {
        self.list_reads.borrow().len() + self.coinbase_reads.get()
    }
}

impl LlmqSource<MnList> for Source<'_> {
    type Error = StoreError;

    fn mn_list(&self, at: BlockId) -> Result<Option<Arc<MnList>>, StoreError> {
        let height = self.chain.header(at).height;
        self.list_reads.borrow_mut().push(height);
        let mut built = self.built.borrow_mut();
        let list = built.entry(height).or_insert_with(|| {
            let ids = self.lists.get(&height).cloned().unwrap_or_else(|| (1..=9).collect());
            let at_height = i32::try_from(height).expect("a fixture height");
            let mut list = MnList::new(self.chain.hash(at), at_height, 0);
            for i in ids {
                let dmn = masternodes()[usize::try_from(i - 1).expect("small")].clone();
                list.try_add(dmn, true).expect("distinct masternodes");
            }
            Arc::new(list)
        });
        Ok(Some(list.clone()))
    }

    fn coinbase_facts(&self, _: BlockId) -> Result<Option<CoinbaseFacts>, StoreError> {
        self.coinbase_reads.set(self.coinbase_reads.get() + 1);
        Ok(Some(CoinbaseFacts::NoTransactions))
    }
}

/// A row store that counts its reads and writes, and fails its writes on demand.
#[derive(Default)]
pub(super) struct Counting {
    pub(super) rows: parking_lot::Mutex<BTreeMap<(u8, BlockHash), QuorumSnapshot>>,
    pub(super) reads: AtomicUsize,
    pub(super) writes: AtomicUsize,
    pub(super) fail_writes: std::sync::atomic::AtomicBool,
    /// Runs as a write enters the store.
    pub(super) on_write: parking_lot::Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl SnapshotStore for Counting {
    fn read_snapshot(
        &self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
    ) -> Result<Option<QuorumSnapshot>, StoreError> {
        self.reads.fetch_add(1, SeqCst);
        Ok(self.rows.lock().get(&(llmq_type.to_base(), *cycle_base)).cloned())
    }

    fn write_snapshot(
        &self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
        snapshot: &QuorumSnapshot,
    ) -> Result<(), StoreError> {
        self.writes.fetch_add(1, SeqCst);
        if let Some(hook) = &*self.on_write.lock() {
            hook();
        }
        if self.fail_writes.load(SeqCst) {
            return Err(StoreError::read("planted write fault"));
        }
        self.rows.lock().insert((llmq_type.to_base(), *cycle_base), snapshot.clone());
        Ok(())
    }
}

/// A facade over a counting row store, with its rows and its own latch.
pub(super) struct World {
    pub(super) facade: Arc<MembersFacade>,
    pub(super) rows: Arc<RuntimeRows>,
    pub(super) store: Arc<Counting>,
    pub(super) latch: GlobalFailStop,
}

pub(super) fn world(params: &ChainParams) -> World {
    let store = Arc::new(Counting::default());
    let rows = Arc::new(RuntimeRows::new(store.clone(), Arc::new(EffectLedger::default())));
    let latch = GlobalFailStop::new();
    let facade = MembersFacade::new(params, rows.clone(), latch.clone()).expect("in domain");
    World {
        facade: Arc::new(facade),
        rows,
        store,
        latch,
    }
}

/// `f` inside one attempt: its log is then applied and the attempt released. The log, too.
pub(super) fn in_attempt<R>(
    v: &mut V,
    world: &World,
    f: impl FnOnce(&AttemptHandle<'_>, &mut Vec<Effect>) -> R,
) -> Result<(R, Vec<Effect>), Fatal> {
    let id = AttemptId::next();
    let handle = world.facade.begin_attempt(&mut v.vcx, id);
    let mut log = Vec::new();
    let r = f(&handle, &mut log);
    let applied = appliers(&world.rows).apply_log(id, &log)?;
    world.facade.release_attempt(handle, applied)?;
    assert!(world.latch.tripped().is_none(), "{:?}", world.latch.tripped());
    Ok((r, log))
}

/// Runs `f` on a fresh V, re-raising its panic here.
pub(super) fn on_v<R: Send + 'static>(f: impl FnOnce(&mut V) -> R + Send + 'static) -> R {
    let rig = Rig::new();
    let (send, got) = mpsc::channel();
    rig.on_v(move |v| {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(v)));
        send.send(r).expect("the test waits");
        Ok(())
    });
    got.recv().expect("V ran it").unwrap_or_else(|p| std::panic::resume_unwind(p))
}

pub(super) fn pros(members: &[Arc<Dmn>]) -> Vec<ProTxHash> {
    members.iter().map(|m| m.pro_tx_hash).collect()
}

/// A members call inside `h`'s attempt.
#[allow(clippy::too_many_arguments)]
pub(super) fn call(
    h: &AttemptHandle<'_>,
    ctx: &LlmqCtx<'_, StoreError>,
    src: &Source<'_>,
    log: &mut Vec<Effect>,
    llmq_type: LlmqType,
    base: BlockId,
    reset: bool,
) -> QuorumMembers<Arc<Dmn>> {
    members(h, ctx, src, log, llmq_type, base, reset).expect("held")
}

/// Rows over a map; a write is visible.
#[derive(Default)]
struct MapRows(ModelRows);

impl SnapshotRows for MapRows {
    type Error = StoreError;

    fn get(&self, t: LlmqType, b: &BlockHash) -> Result<Option<QuorumSnapshot>, StoreError> {
        Ok(self.0.get(&(t.to_base(), *b)).cloned())
    }

    fn store(&mut self, t: LlmqType, b: &BlockHash, s: &QuorumSnapshot) -> Result<(), StoreError> {
        self.0.insert((t.to_base(), *b), s.clone());
        Ok(())
    }
}

/// What the rotation of the cycle at `cycle_base` gives over no rows: its quorums and its row.
pub(super) fn rotate(
    fx: &Fixture,
    ctx: &LlmqCtx<'_, StoreError>,
    src: &Source<'_>,
    cycle_base: u32,
) -> (Vec<Vec<ProTxHash>>, QuorumSnapshot) {
    let mut rows = MapRows::default();
    let quorums =
        compute_by_quarter_rotation(ctx, src, &mut rows, ROTATED, fx.at(cycle_base)).expect("held");
    let row = rows.0.remove(&(ROTATED.to_base(), fx.hash(cycle_base))).expect("stored");
    (quorums.iter().map(|q| pros(q)).collect(), row)
}

fn base_entry(w: &World, t: LlmqType, hash: &BlockHash) -> Option<Vec<ProTxHash>> {
    w.facade.test_caches().by_base.peek(t, hash).map(|m| pros(m))
}

fn indexed_entry(w: &World, t: LlmqType, key: (BlockHash, u16)) -> Option<Vec<ProTxHash>> {
    w.facade.test_caches().by_cycle_index.peek(t, &key).map(|m| pros(m))
}

/// A base hit answers from the cache: no list, coinbase or row read.
#[test]
fn a_base_hit_computes_nothing() {
    on_v(|v| {
        let fx = Fixture::rotated();
        let w = world(&fx.params);
        fx.ctx(|ctx| {
            let src = fx.source();
            in_attempt(v, &w, |h, log| {
                for (t, base) in [(ROTATED, 48), (TEST, 48)] {
                    let first = call(h, ctx, &src, log, t, fx.at(base), false);
                    let (reads, rows) = (src.reads(), w.store.reads.load(SeqCst));
                    assert!(reads > 0, "{t:?}: computed");
                    let second = call(h, ctx, &src, log, t, fx.at(base), false);
                    assert_eq!(pros(&second), pros(&first), "{t:?}");
                    assert_eq!((src.reads(), w.store.reads.load(SeqCst)), (reads, rows), "{t:?}");
                }
            })
        })
        .expect("released");
    });
}

/// A rotated miss inserts every index of the cycle; an indexed hit answers with no computation
/// and is copied into the base cache (M11, M12, M13).
#[test]
fn a_rotated_miss_inserts_every_index_and_an_indexed_hit_is_copied_to_base() {
    on_v(|v| {
        let fx = Fixture::rotated();
        let w = world(&fx.params);
        fx.ctx(|ctx| {
            let src = fx.source();
            let (quorums, _) = rotate(&fx, ctx, &fx.source(), 48);
            assert_eq!(quorums.len(), 2);
            assert_ne!(quorums[0], quorums[1], "discrimination");
            in_attempt(v, &w, |h, log| {
                let m48 = call(h, ctx, &src, log, ROTATED, fx.at(48), false);
                assert_eq!(pros(&m48), quorums[0]);
                for (i, quorum) in quorums.iter().enumerate() {
                    let i = u16::try_from(i).unwrap();
                    let got = indexed_entry(&w, ROTATED, (fx.hash(48), i));
                    assert_eq!(got.as_ref(), Some(quorum), "index {i}");
                }
                assert_eq!(base_entry(&w, ROTATED, &fx.hash(49)), None);
                let reads = src.reads();
                let m49 = call(h, ctx, &src, log, ROTATED, fx.at(49), false);
                assert_eq!(pros(&m49), quorums[1]);
                assert_eq!(src.reads(), reads, "an indexed hit computes nothing");
                assert_eq!(base_entry(&w, ROTATED, &fx.hash(49)).as_ref(), Some(&quorums[1]));
                assert_eq!(base_entry(&w, ROTATED, &fx.hash(48)).as_ref(), Some(&quorums[0]));
            })
        })
        .expect("released");
    });
}

/// A base hit restamps its entry (`llmq/utils.cpp:657`, `unordered_lru_cache.h:61-70`), so the
/// next truncation keeps it (`:93-114`; Opus r1 m-4, R2): LLMQ_TEST keeps 3, and past 6 entries
/// only the 3 newest stamps stay. Six bases fill the cache, the oldest is hit, a seventh
/// truncates; the hit base stays, and a later call to it hits where a stale stamp recomputes.
#[test]
fn a_base_hit_restamps_so_a_truncation_keeps_it() {
    on_v(|v| {
        let fx = Fixture::rotated();
        let w = world(&fx.params);
        let max = w.facade.test_caches().by_base.cache(TEST).expect("configured").max_size();
        assert_eq!(max, 3);
        fx.ctx(|ctx| {
            let src = fx.source();
            in_attempt(v, &w, |h, log| {
                let bases = [24, 25, 26, 27, 28, 29, 30];
                for base in &bases[..6] {
                    call(h, ctx, &src, log, TEST, fx.at(*base), false);
                }
                call(h, ctx, &src, log, TEST, fx.at(24), false);
                call(h, ctx, &src, log, TEST, fx.at(30), false);
                let kept: Vec<u32> = bases
                    .into_iter()
                    .filter(|base| base_entry(&w, TEST, &fx.hash(*base)).is_some())
                    .collect();
                assert_eq!(kept, [24, 29, 30], "the hit base outlives the truncation");
                let reads = src.reads();
                call(h, ctx, &src, log, TEST, fx.at(24), false);
                assert_eq!(src.reads(), reads, "the hit base hits again");
            })
        })
        .expect("released");
    });
}

/// A rotated miss inserts its cycle's indexes in Core's order, 0 first (`llmq/utils.cpp:711-713`;
/// Opus r1 m-4, R1): LLMQ_TEST_DIP0024 keeps 4 with two quorums a cycle, so the fifth cycle's first
/// insert truncates to the 4 newest stamps, and the oldest cycle left, 72, keeps its index 1
/// alone. A later call for 73, which no base entry holds, hits it; index 0 kept instead, it
/// would recompute.
#[test]
fn a_truncation_keeps_the_last_inserted_index_of_a_cycle() {
    on_v(|v| {
        let fx = Fixture::rotated();
        let w = world(&fx.params);
        let caches = w.facade.test_caches();
        let max = caches.by_cycle_index.cache(ROTATED).expect("configured").max_size();
        drop(caches);
        assert_eq!(max, 4);
        fx.ctx(|ctx| {
            let src = fx.source();
            in_attempt(v, &w, |h, log| {
                for cycle in [24, 48, 72, 96, 120] {
                    call(h, ctx, &src, log, ROTATED, fx.at(cycle), false);
                }
                let kept =
                    |c: u32| [0, 1].map(|i| indexed_entry(&w, ROTATED, (fx.hash(c), i)).is_some());
                let cycles = [24, 48, 72, 96, 120].map(kept);
                let want =
                    [[false, false], [false, false], [false, true], [true, true], [true, true]];
                assert_eq!(cycles, want, "72 keeps the index inserted last");
                let reads = src.reads();
                call(h, ctx, &src, log, ROTATED, fx.at(73), false);
                assert_eq!(src.reads(), reads, "73 hits its cycle's index 1");
            })
        })
        .expect("released");
    });
}

/// A reset clears the type's base and indexed caches, and recomputes (M14a, M14b).
#[test]
fn a_reset_clears_the_types_caches_and_recomputes() {
    on_v(|v| {
        let fx = Fixture::rotated();
        let w = world(&fx.params);
        fx.ctx(|ctx| {
            let src = fx.source();
            in_attempt(v, &w, |h, log| {
                call(h, ctx, &src, log, ROTATED, fx.at(48), false);
                call(h, ctx, &src, log, ROTATED, fx.at(72), false);
                call(h, ctx, &src, log, ROTATED, fx.at(73), false);
                let reads = src.reads();
                call(h, ctx, &src, log, ROTATED, fx.at(48), true);
                assert!(src.reads() > reads, "the reset recomputed");
                assert_eq!(base_entry(&w, ROTATED, &fx.hash(72)), None);
                assert_eq!(base_entry(&w, ROTATED, &fx.hash(73)), None);
                assert_eq!(indexed_entry(&w, ROTATED, (fx.hash(72), 0)), None);
                assert_eq!(indexed_entry(&w, ROTATED, (fx.hash(72), 1)), None);
                assert!(base_entry(&w, ROTATED, &fx.hash(48)).is_some(), "refilled");
                assert!(indexed_entry(&w, ROTATED, (fx.hash(48), 1)).is_some(), "refilled");
            })
        })
        .expect("released");
    });
}

/// An index at or above `signingActiveQuorumCount` answers no members before any indexed reset:
/// a reset there clears the base cache only (M15).
#[test]
fn an_out_of_range_index_returns_before_the_indexed_reset() {
    on_v(|v| {
        let fx = Fixture::rotated();
        let w = world(&fx.params);
        fx.ctx(|ctx| {
            let src = fx.source();
            in_attempt(v, &w, |h, log| {
                call(h, ctx, &src, log, ROTATED, fx.at(48), false);
                let reads = src.reads();
                let out = call(h, ctx, &src, log, ROTATED, fx.at(50), true);
                assert!(out.is_empty());
                assert_eq!(src.reads(), reads, "nothing computed");
                assert_eq!(base_entry(&w, ROTATED, &fx.hash(48)), None, "the base reset ran");
                assert!(indexed_entry(&w, ROTATED, (fx.hash(48), 0)).is_some());
                assert!(indexed_entry(&w, ROTATED, (fx.hash(48), 1)).is_some());
                assert_eq!(base_entry(&w, ROTATED, &fx.hash(50)), None, "nothing inserted");
            })
        })
        .expect("released");
    });
}

/// A non-rotated base reads the list of `base - 8` once V20 is active after it, and its own
/// before.
#[test]
fn the_non_rotated_work_block_is_eight_below_from_v20() {
    on_v(|v| {
        let fx = Fixture::new(40, NEVER);
        let w = world(&fx.params);
        fx.ctx(|ctx| {
            let src = fx.source();
            in_attempt(v, &w, |h, log| {
                for (base, work) in [(24, 24), (72, 64)] {
                    src.list_reads.borrow_mut().clear();
                    let m = call(h, ctx, &src, log, TEST, fx.at(base), false);
                    assert_eq!(m.len(), 3, "LLMQ_TEST's size");
                    assert_eq!(src.list_reads.borrow().first(), Some(&work), "base {base}");
                }
            })
        })
        .expect("released");
    });
}

/// The precompute computes at a rotated cycle base only: nothing at an in-range index that is
/// not one, the cycle at one (M20b).
#[test]
fn the_precompute_runs_at_cycle_bases_only() {
    on_v(|v| {
        let fx = Fixture::rotated();
        let w = world(&fx.params);
        fx.ctx(|ctx| {
            let src = fx.source();
            in_attempt(v, &w, |h, log| {
                for height in [47, 49, 50] {
                    precompute(h, ctx, &src, log, fx.at(height), PrecomputeMode::Connect)
                        .expect("held");
                }
                assert_eq!(src.reads(), 0, "no cycle base");
                assert!(log.is_empty());
                precompute(h, ctx, &src, log, fx.at(48), PrecomputeMode::Connect).expect("held");
                assert!(indexed_entry(&w, ROTATED, (fx.hash(48), 1)).is_some());
                assert_eq!(log.len(), 1, "the cycle's row");
            })
        })
        .expect("released");
    });
}

/// At DIP0024's activation edge: a cycle base whose parent is past it rotates; the one before
/// does not, though the type is enabled at its successor (M20a, M21).
#[test]
fn the_precompute_reads_its_predicates_where_core_does() {
    on_v(|v| {
        // DIP0024 from 40: cycle base 24 is before it, 48 after it.
        let fx = Fixture::new(NEVER, 40);
        let w = world(&fx.params);
        fx.ctx(|ctx| {
            let src = fx.source();
            in_attempt(v, &w, |h, log| {
                precompute(h, ctx, &src, log, fx.at(24), PrecomputeMode::Connect).expect("held");
                assert_eq!(src.reads(), 0, "not rotating at 24");
                precompute(h, ctx, &src, log, fx.at(48), PrecomputeMode::Connect).expect("held");
                assert!(indexed_entry(&w, ROTATED, (fx.hash(48), 0)).is_some(), "rotating at 48");
            })
        })
        .expect("released");
    });
}

/// The undo precompute resets: warm caches are recomputed, and the type's other entries go
/// (M22).
#[test]
fn the_undo_precompute_resets() {
    on_v(|v| {
        let fx = Fixture::rotated();
        let w = world(&fx.params);
        fx.ctx(|ctx| {
            let src = fx.source();
            in_attempt(v, &w, |h, log| {
                precompute(h, ctx, &src, log, fx.at(48), PrecomputeMode::Connect).expect("held");
                call(h, ctx, &src, log, ROTATED, fx.at(49), false);
                let reads = src.reads();
                precompute(h, ctx, &src, log, fx.at(48), PrecomputeMode::Connect).expect("held");
                assert_eq!(src.reads(), reads, "a connect precompute hits");
                precompute(h, ctx, &src, log, fx.at(48), PrecomputeMode::Undo).expect("held");
                assert!(src.reads() > reads, "the undo recomputed");
                assert_eq!(base_entry(&w, ROTATED, &fx.hash(49)), None, "the reset cleared it");
            })
        })
        .expect("released");
    });
}

/// Negative control: a view that withholds the ancestry gives `View(_)`, never an answer.
#[test]
fn a_withholding_view_is_a_view_error() {
    on_v(|v| {
        let fx = Fixture::rotated();
        let w = world(&fx.params);
        let complete = CompleteChain::<StoreError>::new(&fx.chain);
        let withheld = Withholding {
            chain: complete,
            floor: 30,
        };
        fx.ctx_over(&withheld, |ctx| {
            let src = fx.source();
            in_attempt(v, &w, |h, log| {
                let got = members(h, ctx, &src, log, ROTATED, fx.at(48), false);
                assert!(matches!(got, Err(MemberError::View(_))), "{:?}", got.map(|m| m.len()));
                let got = precompute(h, ctx, &src, log, fx.at(48), PrecomputeMode::Connect);
                assert!(matches!(got, Err(MemberError::View(_))), "{got:?}");
                assert_eq!(base_entry(&w, ROTATED, &fx.hash(48)), None, "nothing inserted");
            })
        })
        .expect("released");
    });
}

/// The stamps of a type's entries for `keys`, base then indexed.
fn stamps(w: &World, t: LlmqType, base: &BlockHash) -> (Option<i64>, Option<i64>) {
    let caches = w.facade.test_caches();
    let b = caches.by_base.cache(t).and_then(|c| c.stamp(base));
    let i = caches.by_cycle_index.cache(t).and_then(|c| c.stamp(&(*base, 0)));
    (b, i)
}

/// F4: Core's semantic gate (`utils.cpp:644-647`) comes before any cache. At a parentless base,
/// and at a base whose type is not enabled at its parent, a call with warm caches returns no
/// members, with or without a reset, and leaves every entry and stamp as it was, reading nothing
/// (S1, S2, S3).
#[test]
fn the_semantic_gate_comes_before_any_cache() {
    on_v(|v| {
        // DIP0024 from 40: LLMQ_TEST_DIP0024 is not enabled at 23, base 24's parent.
        let fx = Fixture::new(NEVER, 40);
        let w = world(&fx.params);
        let preloaded: QuorumMembers<Arc<Dmn>> = masternodes()[..2].to_vec().into();
        let gated = [(ROTATED, 0), (ROTATED, 24)];
        {
            let mut caches = w.facade.test_caches();
            for (t, h) in gated.into_iter().chain([(TEST, 0), (TEST, 24)]) {
                caches.by_base.insert(t, fx.hash(h), preloaded.clone());
                caches.by_cycle_index.insert(t, (fx.hash(h), 0), preloaded.clone());
            }
        }
        let before: Vec<_> = [(ROTATED, 0), (ROTATED, 24), (TEST, 0), (TEST, 24)]
            .into_iter()
            .map(|(t, h)| stamps(&w, t, &fx.hash(h)))
            .collect();
        fx.ctx(|ctx| {
            let src = fx.source();
            in_attempt(v, &w, |h, log| {
                for (t, base) in gated {
                    for reset in [false, true] {
                        let got = call(h, ctx, &src, log, t, fx.at(base), reset);
                        assert!(got.is_empty(), "{t:?} at {base}, reset {reset}");
                    }
                }
                assert_eq!(src.reads(), 0, "no list or coinbase read");
                assert_eq!(w.store.reads.load(SeqCst), 0, "no row read");
                assert!(log.is_empty());
            })
        })
        .expect("released");
        let after: Vec<_> = [(ROTATED, 0), (ROTATED, 24), (TEST, 0), (TEST, 24)]
            .into_iter()
            .map(|(t, h)| stamps(&w, t, &fx.hash(h)))
            .collect();
        assert_eq!(after, before, "every entry and stamp as it was");
        assert!(before.iter().all(|(b, i)| b.is_some() && i.is_some()));
    });
}

/// F4: a reset at an enabled base clears that type's caches only; another type's entries and
/// stamps stay (M14c).
#[test]
fn a_reset_leaves_other_types_alone() {
    on_v(|v| {
        let fx = Fixture::rotated();
        let w = world(&fx.params);
        fx.ctx(|ctx| {
            let src = fx.source();
            in_attempt(v, &w, |h, log| {
                call(h, ctx, &src, log, TEST, fx.at(24), false);
                call(h, ctx, &src, log, ROTATED, fx.at(48), false);
                w.facade.test_caches().by_cycle_index.insert(
                    TEST,
                    (fx.hash(24), 0),
                    masternodes()[..1].to_vec().into(),
                );
                let before = stamps(&w, TEST, &fx.hash(24));
                call(h, ctx, &src, log, ROTATED, fx.at(72), true);
                assert_eq!(base_entry(&w, ROTATED, &fx.hash(48)), None);
                assert_eq!(indexed_entry(&w, ROTATED, (fx.hash(48), 0)), None);
                assert_eq!(stamps(&w, TEST, &fx.hash(24)), before);
                assert!(before.0.is_some() && before.1.is_some());
            })
        })
        .expect("released");
    });
}

/// One step of the differential.
#[derive(Clone, Debug)]
enum Op {
    Members {
        rotated: bool,
        height: u32,
        reset: bool,
    },
    Precompute {
        height: u32,
        undo: bool,
    },
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (any::<bool>(), 0..=TIP, any::<bool>()).prop_map(|(rotated, height, reset)| Op::Members {
            rotated,
            height,
            reset,
        }),
        (1..=TIP, any::<bool>()).prop_map(|(height, undo)| Op::Precompute {
            height,
            undo
        }),
    ]
}

type Answer = Result<Vec<ProTxHash>, String>;

fn answer(r: Result<QuorumMembers<Arc<Dmn>>, LlmqError<StoreError>>) -> Answer {
    r.map(|m| pros(&m)).map_err(|e| e.to_string())
}

/// LQ1m (CH7 §10 item 8): over random members, precompute and undo sequences in one attempt, the
/// facade answers as LQ1's model of it in its attempt (`ModelAttempt`), with the same cache
/// contents, the same effect log and the same runtime rows after the release (M10-M15, M23-M25).
#[test]
fn the_facade_matches_the_model_in_an_attempt() {
    on_v(|v| {
        let fx = Fixture::new(40, 2);
        let types = [ROTATED, TEST];
        let mut runner = TestRunner::new(Config {
            cases: 48,
            ..Config::default()
        });
        fx.ctx(|ctx| {
            let src = fx.source();
            let strategy = proptest::collection::vec(op(), 1..12);
            // The runner takes an `Fn`; each case borrows V in turn.
            let v = RefCell::new(v);
            // Non-vacuity: rotated rosters answered, and rows written, across the run.
            let (rosters, rows_written) = (Cell::new(0), Cell::new(0));
            let result = runner.run(&strategy, |ops| {
                let w = world(&fx.params);
                let mut model = ModelAttempt::<Arc<Dmn>>::new(&fx.params);
                let id = model.begin().expect("open");
                let mut theirs = Vec::new();
                let (ours, log) = in_attempt(&mut v.borrow_mut(), &w, |h, log| {
                    let mut ours = Vec::new();
                    for op in &ops {
                        match *op {
                            Op::Members {
                                rotated,
                                height,
                                reset,
                            } => {
                                let t = if rotated {
                                    ROTATED
                                } else {
                                    TEST
                                };
                                let at = fx.at(height);
                                let got = answer(members(h, ctx, &src, log, t, at, reset));
                                if rotated && got.as_ref().is_ok_and(|m| !m.is_empty()) {
                                    rosters.set(rosters.get() + 1);
                                }
                                ours.push(got);
                                let call = model.attempt_members(id, ctx, &src, t, at, reset);
                                theirs.push(answer(call.map(|(m, _)| m)));
                            }
                            Op::Precompute {
                                height,
                                undo,
                            } => {
                                let mode = if undo {
                                    PrecomputeMode::Undo
                                } else {
                                    PrecomputeMode::Connect
                                };
                                let at = fx.at(height);
                                let got = precompute(h, ctx, &src, log, at, mode);
                                ours.push(got.map(|()| Vec::new()).map_err(|e| e.to_string()));
                                let got = model.attempt_precompute(id, ctx, &src, at, mode);
                                theirs.push(got.map(|()| Vec::new()).map_err(|e| e.to_string()));
                            }
                        }
                    }
                    ours
                })
                .expect("released");
                prop_assert_eq!(&ours, &theirs);
                prop_assert_eq!(&log, model.log(id));
                rows_written.set(rows_written.get() + log.len());
                for t in types {
                    let caches = w.facade.test_caches();
                    let (base, indexed) =
                        (model.facade().base_cache(), model.facade().indexed_cache());
                    let len = |c: Option<usize>| c.unwrap_or(0);
                    prop_assert_eq!(
                        len(caches.by_base.cache(t).map(|c| c.len())),
                        len(base.cache(t).map(|c| c.len()))
                    );
                    prop_assert_eq!(
                        len(caches.by_cycle_index.cache(t).map(|c| c.len())),
                        len(indexed.cache(t).map(|c| c.len()))
                    );
                    for height in 0..=TIP {
                        let hash = fx.hash(height);
                        let ours = caches.by_base.peek(t, &hash).map(|m| pros(m));
                        prop_assert_eq!(ours, base.peek(t, &hash).map(|m| pros(m)));
                        for i in 0..2 {
                            let ours = caches.by_cycle_index.peek(t, &(hash, i)).map(|m| pros(m));
                            prop_assert_eq!(ours, indexed.peek(t, &(hash, i)).map(|m| pros(m)));
                        }
                    }
                }
                model.release(id, &mut |_| false).expect("applied");
                let rows: ModelRows = w.store.rows.lock().clone();
                prop_assert_eq!(&rows, model.rows());
                Ok(())
            });
            if let Err(e) = result {
                panic!("{e}");
            }
            assert!(rosters.get() > 0 && rows_written.get() > 0, "a vacuous run");
        });
    });
}
