//! `build_qrinfo` over a recording `mnlistdiff` fake whose diff is the `(base, target)` pair it
//! was asked for, so every base and target choice is pinned without building lists (PLAN-LQ3c
//! §4 LQ3c-2). Snapshot rows are [`MemEvoStore`]'s; the mined-commitment index is an in-test
//! reader whose commitments are opaque byte tags. Regtest at v24.0: `C` = 24.

use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use dash_consensus::ChainAncestry;
use dash_evo::llmq::{MinedCommitment, MinedIndexEntry};
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dash_primitives::QuorumHash;

use super::*;
use crate::mnlistdiff::{BlockStatus, PartialMerkleTree, check_request};
use crate::store::{MemEvoStore, MemEvoView, StoreError};
use crate::testkit::MemChain;

const T: LlmqType = LlmqType::LlmqTestDip0024;
const C: u32 = 24;

type Pair = (BlockHash, BlockHash);
type Info = QuorumRotationInfo<Pair>;
type Failure = QrInfoFailure<Pair, StoreError>;

/// A block tree, its evo store and the index facts the builder reads.
struct World {
    setup: ChainSetup,
    chain: MemChain,
    store: MemEvoStore,
    ids: BTreeMap<BlockHash, BlockId>,
    /// The active chain, by height.
    active: Vec<BlockId>,
    tip: Option<BlockId>,
    genesis: Option<BlockId>,
    no_data: BTreeSet<BlockId>,
    no_snapshot: BTreeSet<BlockId>,
    /// Core's rotated height index (`q_mcihi`): (quorum index, mined height newest first, mining
    /// block) to the base height.
    index: BTreeMap<(i16, Reverse<u32>, BlockId), u32>,
    /// Core's `q_mc`: every block that mined each quorum, on any branch.
    mined: BTreeMap<QuorumKey, Vec<BlockId>>,
    /// The diff target whose inner build fails with `BlockRead`.
    fail_target: Option<BlockHash>,
    snapshot_fault: bool,
    /// The quorum index whose index read fails.
    index_fault: Option<i16>,
}

/// The snapshot row of the cycle block at `height`; `stale` marks a fork block's row.
fn snapshot(height: u32, stale: bool) -> Arc<QuorumSnapshot> {
    Arc::new(QuorumSnapshot {
        active_quorum_members: vec![true, false, true],
        skip_list_mode: i32::from(stale),
        skip_list: vec![height as i32],
    })
}

/// The opaque byte tag standing for a quorum's serialized commitment.
fn tag(quorum: &QuorumKey) -> Vec<u8> {
    let mut tag = b"qc:".to_vec();
    tag.extend_from_slice(&quorum.1[..8]);
    tag
}

impl World {
    /// An active chain of `height + 1` blocks, with a snapshot row at every cycle block.
    fn new(height: u32) -> World {
        let chain = MemChain::new();
        let genesis = chain.genesis();
        let setup = ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
            .build()
            .expect("Core's regtest parameters");
        assert_eq!(setup.consensus.llmq_type_dip0024_instant_send, T);
        let params = setup.consensus.llmq(T).expect("configured");
        assert_eq!((u32::from(params.dkg_interval), params.signing_active_quorum_count), (C, 2));
        let mut world = World {
            setup,
            ids: BTreeMap::from([(chain.hash(genesis), genesis)]),
            chain,
            store: MemEvoStore::new(),
            active: vec![genesis],
            tip: Some(genesis),
            genesis: Some(genesis),
            no_data: BTreeSet::new(),
            no_snapshot: BTreeSet::new(),
            index: BTreeMap::new(),
            mined: BTreeMap::new(),
            fail_target: None,
            snapshot_fault: false,
            index_fault: None,
        };
        let mut tip = genesis;
        world.store.store_quorum_snapshot(
            T,
            world.chain.hash(genesis),
            (*snapshot(0, false)).clone(),
        );
        for _ in 0..height {
            tip = world.child(tip, false);
            world.active.push(tip);
        }
        world.tip = Some(tip);
        world
    }

    fn child(&mut self, parent: BlockId, stale: bool) -> BlockId {
        let id = self.chain.child(parent);
        let hash = self.chain.hash(id);
        self.ids.insert(hash, id);
        let height = self.chain.header(id).height;
        if height.is_multiple_of(C) {
            self.store.store_quorum_snapshot(T, hash, (*snapshot(height, stale)).clone());
        }
        id
    }

    /// A stale branch from the active block at `from` up to `to`: its blocks by height.
    fn fork(&mut self, from: u32, to: u32) -> BTreeMap<u32, BlockId> {
        let mut tip = self.at(from);
        (from + 1..=to)
            .map(|h| {
                tip = self.child(tip, true);
                (h, tip)
            })
            .collect()
    }

    fn at(&self, height: u32) -> BlockId {
        self.active[height as usize]
    }

    fn h(&self, height: u32) -> BlockHash {
        self.chain.hash(self.at(height))
    }

    fn g(&self) -> BlockHash {
        self.h(0)
    }

    /// The block `at(mined)` mines a rotated commitment for each `(index, base)`.
    fn mine(&mut self, mined: u32, quorums: &[(i16, BlockId)]) {
        let block = self.at(mined);
        for &(index, base) in quorums {
            self.index.insert((index, Reverse(mined), block), self.chain.header(base).height);
            self.mined.entry(self.key(base)).or_default().push(block);
        }
    }

    /// Both indices mined in each cycle `c` of `cycles` at `c + 12`, with bases `c` and `c + 1`.
    fn mine_cycles(&mut self, cycles: impl IntoIterator<Item = u32>) {
        for c in cycles {
            self.mine(c + 12, &[(0, self.at(c)), (1, self.at(c + 1))]);
        }
    }

    fn src(&self) -> Src<'_> {
        Src {
            world: self,
            view: self.store.view(&self.chain),
            reader: Reader(self),
            calls: RefCell::new(Vec::new()),
        }
    }

    fn key(&self, base: BlockId) -> QuorumKey {
        (T.to_base(), *self.chain.hash(base).as_bytes())
    }

    fn qc(&self, base: BlockId) -> Vec<u8> {
        tag(&self.key(base))
    }

    fn params(&self) -> QrInfoParams<'_> {
        QrInfoParams {
            llmq_type: T,
            params: self.setup.consensus.llmq(T).expect("configured"),
            genesis_hash: self.g(),
        }
    }

    fn build(&self, bases: &[BlockHash], block: BlockHash, extra: bool, legacy: bool) -> Built {
        let src = self.src();
        let req = QrInfoRequest {
            base_block_hashes: bases,
            block_request_hash: block,
            extra_share: extra,
        };
        let result = build_qrinfo(&src, &self.params(), &req, legacy);
        Built {
            result,
            calls: src.calls.into_inner(),
        }
    }
}

struct Built {
    result: Result<Info, Failure>,
    calls: Vec<Pair>,
}

impl Built {
    fn ok(self) -> Info {
        self.result.expect("answered")
    }

    fn err(self) -> Failure {
        self.result.expect_err("refused")
    }
}

/// The mined-commitment index over [`World`]'s rows: the rows whose mining block is `chain_end`
/// or one of its ancestors, as `MemEvoView::mined_index` and Core's active-chain index give them.
struct Reader<'a>(&'a World);

impl MinedCommitmentReader for Reader<'_> {
    type Error = StoreError;

    fn chain(&self) -> &dyn ChainAncestry {
        &self.0.chain
    }

    fn mined_commitment(
        &self,
        _: LlmqType,
        _: &QuorumHash,
        _: BlockId,
    ) -> Result<Option<MinedCommitment>, StoreError> {
        unreachable!("the builder reads commitments through QrInfoSource::commitment")
    }

    fn mined_index(
        &self,
        llmq_type: LlmqType,
        family: MinedFamily,
        chain_end: BlockId,
        max_mined_height: u32,
        limit: usize,
    ) -> Result<Vec<MinedIndexEntry>, StoreError> {
        assert_eq!(llmq_type, T);
        let MinedFamily::Rotated {
            quorum_index,
        } = family
        else {
            return Ok(Vec::new());
        };
        if self.0.index_fault == Some(quorum_index) {
            return Err(StoreError::read("index"));
        }
        let chain = &self.0.chain;
        let cap = max_mined_height.min(chain.header(chain_end).height);
        if cap == 0 {
            return Ok(Vec::new());
        }
        let range = (quorum_index, Reverse(cap), BlockId(0))
            ..=(quorum_index, Reverse(1), BlockId(u32::MAX));
        Ok(self
            .0
            .index
            .range(range)
            .filter(|((_, Reverse(mined), block), _)| {
                chain.ancestor(chain_end, *mined) == Some(*block)
            })
            .take(limit)
            .map(|((_, Reverse(mined_height), _), base_height)| MinedIndexEntry {
                mined_height: *mined_height,
                base_height: *base_height,
            })
            .collect())
    }
}

/// The `QrInfoSource` the tests drive: index facts from [`World`], snapshot rows from
/// [`MemEvoView`], the mined index from [`Reader`], and a recording `mnlistdiff` that runs Core's
/// request checks.
struct Src<'a> {
    world: &'a World,
    view: MemEvoView<'a>,
    reader: Reader<'a>,
    calls: RefCell<Vec<Pair>>,
}

impl Src<'_> {
    fn status(&self, hash: &BlockHash) -> Option<BlockStatus> {
        self.lookup(hash).map(|id| BlockStatus {
            height: self.world.chain.header(id).height as i32,
            in_active_chain: self.in_active_chain(id),
            have_data: self.have_data(id),
        })
    }
}

impl QrInfoSource for Src<'_> {
    type Error = StoreError;
    type Diff = Pair;

    fn lookup(&self, hash: &BlockHash) -> Option<BlockId> {
        self.world.ids.get(hash).copied()
    }

    fn active_genesis(&self) -> Option<BlockId> {
        self.world.genesis
    }

    fn active_tip(&self) -> Option<BlockId> {
        self.world.tip
    }

    fn in_active_chain(&self, id: BlockId) -> bool {
        let height = self.world.chain.header(id).height as usize;
        self.world.active.get(height) == Some(&id)
    }

    fn have_data(&self, id: BlockId) -> bool {
        !self.world.no_data.contains(&id)
    }

    fn quorum_snapshot(
        &self,
        llmq_type: LlmqType,
        cycle: &BlockHash,
    ) -> Result<Option<Arc<QuorumSnapshot>>, StoreError> {
        if self.world.snapshot_fault {
            return Err(StoreError::read("injected"));
        }
        if self.lookup(cycle).is_some_and(|id| self.world.no_snapshot.contains(&id)) {
            return Ok(None);
        }
        crate::EvoStore::quorum_snapshot(&self.view, llmq_type, cycle)
    }

    fn mined(&self) -> &dyn MinedCommitmentReader<Error = StoreError> {
        &self.reader
    }

    /// `GetMinedCommitment` at the tip: the tag when a block of the active chain mined it.
    fn commitment(&self, quorum: &QuorumKey) -> Result<Option<Vec<u8>>, StoreError> {
        assert_eq!(quorum.0, T.to_base());
        let tip = self.world.tip.expect("a tip");
        let chain = &self.world.chain;
        let on_tip =
            |block: &BlockId| chain.ancestor(tip, chain.header(*block).height) == Some(*block);
        let mined = self.world.mined.get(quorum).is_some_and(|blocks| blocks.iter().any(on_tip));
        Ok(mined.then(|| tag(quorum)))
    }

    fn mnlistdiff(
        &self,
        base: &BlockHash,
        block: &BlockHash,
    ) -> Result<Pair, MnListDiffError<StoreError>> {
        self.calls.borrow_mut().push((*base, *block));
        check_request(base, self.status(base), block, self.status(block))?;
        if self.world.fail_target == Some(*block) {
            return Err(MnListDiffError::BlockRead(*block));
        }
        Ok((*base, *block))
    }
}

/// Every diff of an answer, in Core's build order.
fn build_order(info: &Info) -> Vec<Pair> {
    let mut order = Vec::new();
    info.clone()
        .try_map(|d| {
            order.push(d);
            Ok::<_, ()>(d)
        })
        .expect("infallible");
    assert_eq!(order.len(), info.cycles.len() + 2 + info.mn_list_diff_list.len());
    order
}

/// The standard world: an active chain to `7C + 15`, both indices mined in every cycle from
/// `C` to `7C` at `c + 12`.
fn standard() -> World {
    let mut world = World::new(7 * C + 15);
    world.mine_cycles((1..=7).map(|k| k * C));
    world
}

fn snaps(heights: &[u32]) -> Vec<Arc<QuorumSnapshot>> {
    heights.iter().map(|h| snapshot(*h, false)).collect()
}

fn cycle_snaps(info: &Info) -> Vec<Arc<QuorumSnapshot>> {
    info.cycles.iter().map(|c| Arc::clone(&c.snapshot)).collect()
}

fn cycle_diffs(info: &Info) -> Vec<Pair> {
    info.cycles.iter().map(|c| c.diff).collect()
}

/// Rows 6, 9-16, 21-24 with no request base: the efficient path builds each target cycle's diff
/// oldest first on the work block before it, the legacy path every diff from genesis.
#[test]
fn no_bases_use_genesis_and_each_path_keeps_its_order() {
    let w = standard();
    let d = |b: u32, t: u32| (w.h(b), w.h(t));
    let tip = w.h(7 * C + 15);

    let built = w.build(&[], tip, false, false);
    let calls = built.calls.clone();
    let info = built.ok();
    assert_eq!(calls, [d(0, 88), d(88, 112), d(112, 136), d(136, 160), d(160, 183)]);
    assert_eq!(cycle_diffs(&info), [d(112, 136), d(88, 112), d(0, 88)]);
    assert_eq!(cycle_snaps(&info), snaps(&[144, 120, 96]));
    assert_eq!((info.diff_h, info.diff_tip), (d(136, 160), d(160, 183)));
    assert_eq!(info.last_commitment_per_index, [w.qc(w.at(168)), w.qc(w.at(169))]);
    assert!(info.quorum_snapshot_list.is_empty() && info.mn_list_diff_list.is_empty());
    assert!(!info.extra_share);
    assert_eq!(build_order(&info), calls);

    let built = w.build(&[], tip, false, true);
    let calls = built.calls.clone();
    let info = built.ok();
    assert_eq!(calls, [d(0, 183), d(0, 160), d(0, 136), d(0, 112), d(0, 88)]);
    assert_eq!(cycle_diffs(&info), [d(0, 136), d(0, 112), d(0, 88)]);
    assert_eq!((info.diff_tip, info.diff_h), (d(0, 183), d(0, 160)));
    assert_eq!(build_order(&info), calls);
}

/// Rows 7, 8, 10 and 21-24 with unsorted, duplicated request bases: sorted by height, the legacy
/// tip diff from the highest, and the efficient path inserting each work block after its diff.
#[test]
fn many_unsorted_duplicated_bases_are_sorted_and_chosen_by_height() {
    let w = standard();
    let d = |b: u32, t: u32| (w.h(b), w.h(t));
    let bases = [w.h(100), w.h(50), w.h(150), w.h(100)];
    let tip = w.h(183);

    let built = w.build(&bases, tip, false, false);
    assert_eq!(built.calls, [d(50, 88), d(100, 112), d(112, 136), d(150, 160), d(160, 183)]);

    let built = w.build(&bases, tip, false, true);
    assert_eq!(built.calls, [d(150, 183), d(150, 160), d(100, 136), d(100, 112), d(50, 88)]);

    // One base.
    let built = w.build(&[w.h(100)], tip, false, true);
    assert_eq!(built.calls, [d(100, 183), d(100, 160), d(100, 136), d(100, 112), (w.g(), w.h(88))]);
}

/// Row 4: a base at exactly a work block's height is that work block's base (Core's
/// `upper_bound`), and a work block below every base takes the genesis hash, not the first base.
#[test]
fn an_equal_height_base_is_taken_and_genesis_is_the_fallback() {
    let w = standard();
    let d = |b: u32, t: u32| (w.h(b), w.h(t));
    let tip = w.h(183);
    let bases = [w.h(136)];

    let built = w.build(&bases, tip, false, false);
    assert_eq!(built.calls, [(w.g(), w.h(88)), d(88, 112), d(136, 136), d(136, 160), d(160, 183)]);

    let built = w.build(&bases, tip, false, true);
    assert_eq!(
        built.calls,
        [d(136, 183), d(136, 160), d(136, 136), (w.g(), w.h(112)), (w.g(), w.h(88))]
    );
}

/// Row 15: `extraShare` adds the `H−4C` cycle, built first on the efficient path.
#[test]
fn extra_share_adds_the_fourth_cycle() {
    let w = standard();
    let d = |b: u32, t: u32| (w.h(b), w.h(t));
    let built = w.build(&[], w.h(183), true, false);
    assert_eq!(
        built.calls,
        [d(0, 64), d(64, 88), d(88, 112), d(112, 136), d(136, 160), d(160, 183)]
    );
    let info = built.ok();
    assert!(info.extra_share);
    assert_eq!(cycle_snaps(&info), snaps(&[144, 120, 96, 72]));
    assert_eq!(cycle_diffs(&info), [d(112, 136), d(88, 112), d(64, 88), d(0, 64)]);

    let info = w.build(&[], w.h(183), true, true).ok();
    assert_eq!(info.cycles.len(), 4);
    assert_eq!(info.cycles[3].diff, d(0, 64));
}

/// `GetCycles` includes `H−4C` with `extraShare`, so its height is erased from the needed set
/// too (`snap:208-210`): at a request block whose last commitments need it, no list entry is
/// built for it.
#[test]
fn the_fourth_target_height_is_erased_with_extra_share() {
    let w = standard();
    let d = |b: u32, t: u32| (w.h(b), w.h(t));
    let built = w.build(&[], w.h(170), true, false);
    assert_eq!(
        built.calls,
        [d(0, 64), d(64, 88), d(88, 112), d(112, 136), d(136, 160), d(160, 183)]
    );
    let info = built.ok();
    assert_eq!(info.last_commitment_per_index, [w.qc(w.at(144)), w.qc(w.at(145))]);
    assert_eq!(cycle_snaps(&info), snaps(&[144, 120, 96, 72]));
    assert!(info.quorum_snapshot_list.is_empty() && info.mn_list_diff_list.is_empty());
}

/// Rows 17-22 for a request block before its cycle's mining window ends: the index is read
/// under `h(S)`, so the last commitments are the previous cycle's, whose needed heights keep
/// `H−4C` after the target heights are erased; snapshot and diff lists pair up.
#[test]
fn needed_heights_are_read_under_the_request_block_and_erased() {
    let w = standard();
    let d = |b: u32, t: u32| (w.h(b), w.h(t));
    let block = w.h(170);

    let built = w.build(&[], block, false, false);
    assert_eq!(
        built.calls,
        [d(0, 64), d(64, 88), d(88, 112), d(112, 136), d(136, 160), d(160, 183)]
    );
    let calls = built.calls.clone();
    let info = built.ok();
    assert_eq!(build_order(&info), calls);
    assert_eq!(info.last_commitment_per_index, [w.qc(w.at(144)), w.qc(w.at(145))]);
    assert_eq!(info.quorum_snapshot_list, snaps(&[72]));
    assert_eq!(info.mn_list_diff_list, [d(0, 64)]);
    assert_eq!(cycle_diffs(&info), [d(112, 136), d(88, 112), d(64, 88)]);

    // Legacy: the needed diff is built too, from the request bases only.
    let built = w.build(&[], block, false, true);
    assert_eq!(built.calls, [d(0, 183), d(0, 160), d(0, 136), d(0, 112), d(0, 88), d(0, 64)]);
    let calls = built.calls.clone();
    let info = built.ok();
    assert_eq!(build_order(&info), calls);
    assert_eq!(info.quorum_snapshot_list, snaps(&[72]));
    assert_eq!(info.mn_list_diff_list, [d(0, 64)]);
}

/// Scenario (d) (owner fix r2.2): three needed heights survive the erase and a request base sits
/// at the first needed work block's height. Legacy takes every needed diff's base from the
/// request bases alone; the efficient path inserts each needed work block.
#[test]
fn legacy_needed_diffs_take_request_bases_only() {
    let mut w = World::new(7 * C + 15);
    w.mine(7 * C + 12, &[(0, w.at(7 * C))]);
    w.mine(4 * C + 12, &[(1, w.at(4 * C + 1))]);
    let d = |b: u32, t: u32| (w.h(b), w.h(t));
    let bases = [w.h(16)];

    let built = w.build(&bases, w.h(183), false, true);
    assert_eq!(
        built.calls,
        [
            d(16, 183),
            d(16, 160),
            d(16, 136),
            d(16, 112),
            d(16, 88),
            d(16, 16),
            d(16, 40),
            d(16, 64)
        ]
    );
    let info = built.ok();
    assert_eq!(info.quorum_snapshot_list, snaps(&[24, 48, 72]));
    assert_eq!(info.last_commitment_per_index, [w.qc(w.at(168)), w.qc(w.at(97))]);

    let built = w.build(&bases, w.h(183), false, false);
    assert_eq!(
        built.calls,
        [
            d(16, 16),
            d(16, 40),
            d(40, 64),
            d(64, 88),
            d(88, 112),
            d(112, 136),
            d(136, 160),
            d(160, 183)
        ]
    );
    assert_eq!(built.ok().mn_list_diff_list, [d(16, 16), d(16, 40), d(40, 64)]);

    // A failure in the last needed diff: `built` holds every diff before it, in build order.
    let mut w = w;
    w.fail_target = Some(w.h(64));
    let d = |b: u32, t: u32| (w.h(b), w.h(t));
    let failure = w.build(&bases, w.h(183), false, true).err();
    assert_eq!(
        failure.built,
        [d(16, 183), d(16, 160), d(16, 136), d(16, 112), d(16, 88), d(16, 16), d(16, 40)]
    );
    assert_eq!(failure.error, QrInfoBuildError::Diff(MnListDiffError::BlockRead(w.h(64))));
    let failure = w.build(&bases, w.h(183), false, false).err();
    assert_eq!(failure.built, [d(16, 16), d(16, 40)]);
}

/// Scenario (c): one index last mined in cycle `C`, the other in `4C`, so `{−2C, −C, 0}`
/// survive the erase. Core visits `−2C` first (a `std::set<int>`) and fails `Cannot find block`.
#[test]
fn negative_needed_heights_fail_in_ascending_order() {
    let mut w = World::new(4 * C + 15);
    w.mine(C + 12, &[(0, w.at(C))]);
    w.mine(4 * C + 12, &[(1, w.at(4 * C + 1))]);
    let d = |t: u32| (w.g(), w.h(t));

    let failure = w.build(&[], w.h(4 * C + 15), false, false).err();
    assert_eq!(failure.error, QrInfoBuildError::CannotFindBlock);
    assert!(failure.built.is_empty());

    let failure = w.build(&[], w.h(4 * C + 15), false, true).err();
    assert_eq!(failure.error, QrInfoBuildError::CannotFindBlock);
    assert_eq!(failure.built, [d(111), d(88), d(64), d(40), d(16)]);
}

/// Row 13 at a request block in the first cycle: the cycle base is genesis, whose work block
/// would be at −8.
#[test]
fn a_first_cycle_request_has_no_work_block() {
    let w = standard();
    let failure = w.build(&[], w.h(10), false, false).err();
    assert_eq!(failure.error, QrInfoBuildError::CannotFindWorkBlock);
    assert!(failure.built.is_empty());
    let failure = w.build(&[], w.h(10), false, true).err();
    assert_eq!(failure.error, QrInfoBuildError::CannotFindWorkBlock);
    assert_eq!(failure.built, [(w.g(), w.h(183))]);
}

/// Row 11 (mutant 18): a stale request block below its cycle's mining window end is answered,
/// identically to its active sibling, on both paths. Core never checks it against the active
/// chain.
#[test]
fn a_stale_request_block_is_answered_like_its_active_sibling() {
    let mut w = standard();
    let stale = w.fork(165, 181);
    for legacy in [false, true] {
        let active = w.build(&[], w.h(170), false, legacy).ok();
        let answered = w.build(&[], w.chain.hash(stale[&170]), false, legacy).ok();
        assert_eq!(answered, active);
    }
}

/// Rows 17-18 (mutants 8 and 20), the store.rs M2 shape at a stale request block above its
/// cycle's mining height: the active index's newest row maps through `S` to a fork block, which
/// has no mined commitment on the active chain, so Core refuses with an empty error.
///
/// The kill of mutant 8 proves the trait's contract: `mined_index(.., tip, h(S), 1)` keeps only
/// the rows whose mining block is on the active chain (the ancestry filter at `chain_end`), where
/// `nth_mined_by_index(S, ..)` reads `S`'s own ancestry.
#[test]
fn a_stale_request_block_above_the_mining_height_has_no_commitment() {
    let mut w = standard();
    let stale = w.fork(165, 181);
    let block = w.chain.hash(stale[&181]);
    let key = w.key(stale[&168]);

    let failure = w.build(&[], block, false, false).err();
    assert_eq!(failure.error, QrInfoBuildError::NoMinedCommitment(key));
    assert!(failure.built.is_empty());
    assert_eq!(failure.error.to_string(), "");

    let failure = w.build(&[], block, false, true).err();
    assert_eq!(failure.error, QrInfoBuildError::NoMinedCommitment(key));
    assert_eq!(failure.built.len(), 5);
}

/// The store.rs M2 scenario (`tests/store.rs`, EV0 API review r1) through the builder: the stale
/// branch mined its own commitment, which `S`'s ancestry would find, but Core reads the active
/// index under `h(S)` and maps the base through `S`.
#[test]
fn the_store_m2_scenario_through_the_builder() {
    //                    /- A105 - A106 - A107   (active; A105 mines base 102, A107 base A106)
    // ... - 102 - 103 - 104
    //                    \- S105 - S106 - S107   (stale; S105 mines base 103)
    let mut w = World::new(104);
    let s105 = w.child(w.at(104), true);
    let s106 = w.child(s105, true);
    let s107 = w.child(s106, true);
    for _ in 0..3 {
        let next = w.child(*w.active.last().unwrap(), false);
        w.active.push(next);
    }
    w.tip = Some(w.at(107));
    // The stale branch mined first, then the reorg.
    w.active[105] = s105;
    w.mine(105, &[(0, w.at(103))]);
    let a105 = w.chain.ancestor(w.at(107), 105).unwrap();
    w.active[105] = a105;
    w.mine(105, &[(0, w.at(102))]);
    w.mine(107, &[(0, w.at(106))]);

    let info = w.build(&[], w.chain.hash(s106), false, false).ok();
    assert_eq!(info.last_commitment_per_index, [w.qc(w.at(102))]);
    assert!(info.quorum_snapshot_list.is_empty());

    let failure = w.build(&[], w.chain.hash(s107), false, false).err();
    assert_eq!(failure.error, QrInfoBuildError::NoMinedCommitment(w.key(s106)));
}

/// Row 13 (mutant 6): the cycle base comes from the request block's ancestry. A stale request
/// block whose work block is a fork block fails that diff `NotInSameChain`.
#[test]
fn the_cycle_base_comes_from_the_request_block() {
    let mut w = standard();
    let stale = w.fork(150, 175);
    let block = w.chain.hash(stale[&170]);
    let work = w.chain.hash(stale[&160]);
    let not_same = QrInfoBuildError::Diff(MnListDiffError::NotInSameChain {
        base: w.h(136),
        block: work,
    });

    let failure = w.build(&[], block, false, false).err();
    assert_eq!(failure.error, not_same);
    let d = |b: u32, t: u32| (w.h(b), w.h(t));
    assert_eq!(failure.built, [d(0, 64), d(64, 88), d(88, 112), d(112, 136)]);

    let failure = w.build(&[], block, false, true).err();
    assert_eq!(
        failure.error,
        QrInfoBuildError::Diff(MnListDiffError::NotInSameChain {
            base: w.g(),
            block: work,
        })
    );
    assert_eq!(failure.built, [d(0, 183)]);
}

/// Row 16 (mutant 7): target cycles come from the tip's ancestry. A deep fork's cycle block at
/// `H−C` has no snapshot row here; Core never reads it and refuses on the commitment instead.
#[test]
fn target_cycles_come_from_the_tip() {
    let mut w = standard();
    let stale = w.fork(130, 175);
    w.no_snapshot.insert(stale[&144]);
    let failure = w.build(&[], w.chain.hash(stale[&170]), false, false).err();
    assert_eq!(failure.error, QrInfoBuildError::NoMinedCommitment(w.key(stale[&144])));
}

/// Row 16 (mutant 7) on a store Core can reach: every cycle block keeps its snapshot row and
/// nothing is mined. Core's targets come from the tip and it fails at the H diff, whose work
/// block is on the request block's stale branch; targets read from the request block fail
/// earlier, at the stale `H−C` work block.
#[test]
fn target_cycles_from_the_tip_fail_at_the_stale_h_diff() {
    let mut w = World::new(7 * C + 15);
    let stale = w.fork(130, 175);
    let d = |b: u32, t: u32| (w.h(b), w.h(t));
    let failure = w.build(&[], w.chain.hash(stale[&170]), false, false).err();
    assert_eq!(
        failure.error,
        QrInfoBuildError::Diff(MnListDiffError::NotInSameChain {
            base: w.h(136),
            block: w.chain.hash(stale[&160]),
        })
    );
    assert_eq!(failure.built, [(w.g(), w.h(88)), d(88, 112), d(112, 136)]);
}

/// Core checks no request base's data (`snap:109-112`) and, of a cycle, only its work block's
/// (`snap:55`): a base, a target cycle block and the request block's cycle block without data
/// change nothing on either path.
#[test]
fn bases_and_cycle_blocks_need_no_block_data() {
    let mut w = standard();
    let d = |w: &World, b: u32, t: u32| (w.h(b), w.h(t));
    let bases = [w.h(100)];
    let tip = w.h(183);
    let full = [false, true].map(|legacy| w.build(&bases, tip, false, legacy).calls);
    assert_eq!(
        full[0],
        [(w.g(), w.h(88)), d(&w, 100, 112), d(&w, 112, 136), d(&w, 136, 160), d(&w, 160, 183)]
    );
    assert_eq!(
        full[1],
        [d(&w, 100, 183), d(&w, 100, 160), d(&w, 100, 136), d(&w, 100, 112), (w.g(), w.h(88))]
    );
    for height in [100, 144, 168] {
        w.no_data.insert(w.at(height));
    }
    for (legacy, calls) in [false, true].into_iter().zip(full) {
        let built = w.build(&bases, tip, false, legacy);
        assert_eq!(built.calls, calls, "legacy {legacy}");
        built.ok();
    }
}

/// Row 2: a target cycle above the block it is read from (the tip) is `Cannot find block`.
#[test]
fn a_target_cycle_above_the_tip_cannot_be_found() {
    let mut w = standard();
    let stale = w.fork(175, 220);
    let failure = w.build(&[], w.chain.hash(stale[&220]), false, false).err();
    assert_eq!(failure.error, QrInfoBuildError::CannotFindBlock);
    assert!(failure.built.is_empty());
}

/// Scenario (a): legacy, a target cycle's work block without data and that cycle's snapshot
/// absent. Core checks the data first (`snap:55` before `:56-63`).
#[test]
fn legacy_checks_work_block_data_before_the_snapshot() {
    let mut w = standard();
    w.no_data.insert(w.at(112));
    w.no_snapshot.insert(w.at(120));
    let failure = w.build(&[], w.h(183), false, true).err();
    assert_eq!(failure.error, QrInfoBuildError::BlockDataUnavailable(w.h(112)));
    assert_eq!(failure.error.peer_outcome(), PeerOutcome::Unscored);
    assert_eq!(failure.built, [(w.g(), w.h(183)), (w.g(), w.h(160)), (w.g(), w.h(136))]);

    let failure = w.build(&[], w.h(183), false, false).err();
    assert_eq!(failure.error, QrInfoBuildError::NoSnapshot);
}

/// Scenario (b): efficient, `work_H` without data and a last commitment missing. The efficient
/// path checks no work block's data, and the commitment read comes before the H diff.
#[test]
fn efficient_reads_commitments_before_any_work_block_data() {
    let mut w = standard();
    let stale = w.fork(165, 181);
    w.no_data.insert(w.at(160));
    let block = w.chain.hash(stale[&181]);
    let failure = w.build(&[], block, false, false).err();
    assert_eq!(failure.error, QrInfoBuildError::NoMinedCommitment(w.key(stale[&168])));
    assert_eq!(failure.error.peer_outcome(), PeerOutcome::Misbehaving(1));

    let failure = w.build(&[], block, false, true).err();
    assert_eq!(failure.error, QrInfoBuildError::BlockDataUnavailable(w.h(160)));
    assert_eq!(failure.built, [(w.g(), w.h(183))]);
}

/// Rows 9 and 11: the tip's and the request block's data are checked, Unavailable.
#[test]
fn tip_and_request_block_data_are_checked() {
    let mut w = standard();
    w.no_data.insert(w.at(183));
    let failure = w.build(&[], BlockHash::from([9; 32]), false, true).err();
    assert_eq!(failure.error, QrInfoBuildError::BlockDataUnavailable(w.h(183)));
    assert!(failure.built.is_empty());

    let mut w = standard();
    w.no_data.insert(w.at(170));
    for legacy in [false, true] {
        let failure = w.build(&[], w.h(170), false, legacy).err();
        assert_eq!(failure.error, QrInfoBuildError::BlockDataUnavailable(w.h(170)));
        assert_eq!(failure.built.len(), usize::from(legacy));
    }
}

/// Rows 5-11's order: the base checks in request order before the tip, the legacy tip diff
/// before the request block's lookup.
#[test]
fn refusals_come_in_core_order() {
    let mut w = standard();
    let stale = w.fork(100, 110);
    let unknown = BlockHash::from([9; 32]);
    let stale_hash = w.chain.hash(stale[&105]);
    w.tip = None;
    let err = |w: &World, bases: &[BlockHash]| w.build(bases, unknown, false, false).err().error;

    assert_eq!(err(&w, &vec![unknown; 4097]), QrInfoBuildError::TooManyBases(4097));
    assert_eq!(err(&w, &vec![unknown; 4096]), QrInfoBuildError::BaseNotFound(unknown));
    assert_eq!(err(&w, &[stale_hash, unknown]), QrInfoBuildError::BaseNotActive(stale_hash));
    assert_eq!(err(&w, &[unknown, stale_hash]), QrInfoBuildError::BaseNotFound(unknown));
    assert_eq!(err(&w, &[w.h(5)]), QrInfoBuildError::TipNotFound);
    assert_eq!(err(&w, &[]), QrInfoBuildError::TipNotFound);
    w.genesis = None;
    assert_eq!(err(&w, &[]), QrInfoBuildError::GenesisNotFound);
    assert_eq!(err(&w, &[w.h(5)]), QrInfoBuildError::TipNotFound);

    let mut w = standard();
    assert_eq!(err(&w, &[]), QrInfoBuildError::RequestNotFound);
    w.fail_target = Some(w.h(183));
    let failure = w.build(&[], unknown, false, true).err();
    assert_eq!(failure.error, QrInfoBuildError::Diff(MnListDiffError::BlockRead(w.h(183))));
    assert!(failure.built.is_empty());
    assert_eq!(err(&w, &[]), QrInfoBuildError::RequestNotFound);
}

/// Row 17: Core reads every quorum index's row before any commitment (`bp:786-791`, then
/// `snap:195-199`), so a fault reading index 1 wins over index 0's missing commitment.
#[test]
fn every_index_row_is_read_before_any_commitment() {
    let mut w = standard();
    let stale = w.fork(165, 181);
    w.index_fault = Some(1);
    let failure = w.build(&[], w.chain.hash(stale[&181]), false, false).err();
    assert_eq!(failure.error, QrInfoBuildError::Source(StoreError::read("index")));
}

/// A snapshot read fault is the source's error, unscored.
#[test]
fn a_store_fault_is_the_source_error() {
    let mut w = standard();
    w.snapshot_fault = true;
    let failure = w.build(&[], w.h(183), false, false).err();
    assert_eq!(failure.error, QrInfoBuildError::Source(StoreError::read("injected")));
    assert_eq!(failure.error.peer_outcome(), PeerOutcome::Unscored);
}

/// `try_map` maps in Core's build order and stops at the first error.
#[test]
fn try_map_follows_the_build_order_and_stops_at_the_first_error() {
    let w = standard();
    for legacy in [false, true] {
        let built = w.build(&[w.h(16)], w.h(170), true, legacy);
        let calls = built.calls.clone();
        let info = built.ok();
        assert_eq!(build_order(&info), calls);
        let mapped = info.clone().try_map(|(b, t)| Ok::<_, ()>((t, b))).unwrap();
        assert_eq!(mapped.diff_tip, (info.diff_tip.1, info.diff_tip.0));
        assert_eq!(mapped.cycles[3].diff, (info.cycles[3].diff.1, info.cycles[3].diff.0));
        assert_eq!(mapped.mn_list_diff_list.len(), info.mn_list_diff_list.len());
        let mut seen = 0;
        let stopped = info.try_map(|d| {
            seen += 1;
            if seen == 2 {
                Err(d)
            } else {
                Ok(d)
            }
        });
        assert_eq!(stopped, Err(calls[1]));
        assert_eq!(seen, 2);
    }
}

/// Every refusal's text is Core's `errorRet`, and its peer outcome Core's classification.
#[test]
fn every_refusal_has_core_text_and_outcome() {
    let mut hash = [0u8; 32];
    hash[0] = 0xab;
    hash[31] = 0x01;
    let h = BlockHash::from(hash);
    let hex = "01000000000000000000000000000000000000000000000000000000000000ab";
    let suffix = "is not available (pruned or below an unvalidated snapshot base)";
    let scored = PeerOutcome::Misbehaving(1);
    let cases: Vec<(QrInfoBuildError<StoreError>, String, PeerOutcome)> = vec![
        (
            QrInfoBuildError::TooManyBases(4097),
            "too many baseBlockHashes: 4097 > 4096".into(),
            scored,
        ),
        (QrInfoBuildError::GenesisNotFound, "genesis block not found".into(), scored),
        (QrInfoBuildError::BaseNotFound(h), format!("block {hex} not found"), scored),
        (
            QrInfoBuildError::BaseNotActive(h),
            format!("block {hex} is not in the active chain"),
            scored,
        ),
        (QrInfoBuildError::TipNotFound, "tip block not found".into(), scored),
        (
            QrInfoBuildError::BlockDataUnavailable(h),
            format!("block data for block {hex} {suffix}"),
            PeerOutcome::Unscored,
        ),
        (QrInfoBuildError::RequestNotFound, "block not found".into(), scored),
        (QrInfoBuildError::CannotFindBlock, "Cannot find block".into(), scored),
        (QrInfoBuildError::CannotFindWorkBlock, "Cannot find work block".into(), scored),
        (QrInfoBuildError::NoSnapshot, "Cannot find quorum snapshot".into(), scored),
        (QrInfoBuildError::NoMinedCommitment((T.to_base(), hash)), String::new(), scored),
        (
            QrInfoBuildError::Diff(MnListDiffError::QuorumsDiff((T.to_base(), hash))),
            "failed to build quorums diff".into(),
            scored,
        ),
        (
            QrInfoBuildError::Diff(MnListDiffError::NotInSameChain {
                base: h,
                block: h,
            }),
            format!("block {hex} and {hex} are not in the same chain"),
            scored,
        ),
        (
            QrInfoBuildError::Diff(MnListDiffError::BlockDataUnavailable(h)),
            format!("block data for block {hex} {suffix}"),
            PeerOutcome::Unscored,
        ),
        (
            QrInfoBuildError::Diff(MnListDiffError::ListUnavailable(h)),
            format!(
                "CDeterministicMNManager::GetListForBlockInternal -- masternode list diff for \
                 block {hex} {suffix}"
            ),
            PeerOutcome::Unscored,
        ),
        (
            QrInfoBuildError::Diff(MnListDiffError::Source(StoreError::read("x"))),
            "evo store read failed: x".into(),
            PeerOutcome::Unscored,
        ),
        (
            QrInfoBuildError::Source(StoreError::inconsistent("y")),
            "Found EvoDB inconsistency, you must reindex: y".into(),
            PeerOutcome::Unscored,
        ),
    ];
    for (error, text, outcome) in cases {
        assert_eq!(error.to_string(), text, "{error:?}");
        assert_eq!(error.peer_outcome(), outcome, "{error:?}");
        // The node's mapping: Unavailable exactly when the text carries Core's sentinel or Core
        // throws.
        let sentinel = text.contains(suffix) || matches!(error, QrInfoBuildError::Source(_));
        let diff_source = matches!(error, QrInfoBuildError::Diff(MnListDiffError::Source(_)));
        assert_eq!(outcome == PeerOutcome::Unscored, sentinel || diff_source, "{error:?}");
    }
    assert_eq!(MAX_BASE_BLOCK_HASHES, dash_p2p_core::MAX_QRINFO_BASE_HASHES);
}

/// A diff with no entries and a one-transaction block, at `block`.
fn wire_diff(base: u8, block: u8) -> SimplifiedMnListDiff {
    // Version 1, one null-prevout input, one empty zero-value output, lock time 0.
    let mut cb_tx = vec![1, 0, 0, 0, 1];
    cb_tx.extend_from_slice(&[0; 32]);
    cb_tx.extend_from_slice(&[0xff; 4]);
    cb_tx.extend_from_slice(&[1, 0x51]);
    cb_tx.extend_from_slice(&[0xff; 4]);
    cb_tx.push(1);
    cb_tx.extend_from_slice(&[0; 8 + 1 + 4]);
    SimplifiedMnListDiff {
        base_block_hash: BlockHash::from([base; 32]),
        block_hash: BlockHash::from([block; 32]),
        cb_tx_merkle_tree: PartialMerkleTree::coinbase_only(&[[block; 32]]),
        cb_tx,
        deleted_mns: Vec::new(),
        mn_list: Vec::new(),
        deleted_quorums: Vec::new(),
        new_quorums: Vec::new(),
        quorums_cl_sigs: Vec::new(),
    }
}

fn wire_snapshot(mode: i32, members: &[bool], skip: &[i32]) -> Arc<QuorumSnapshot> {
    Arc::new(QuorumSnapshot {
        active_quorum_members: members.to_vec(),
        skip_list_mode: mode,
        skip_list: skip.to_vec(),
    })
}

/// Row 26: the encoder against hand-assembled bytes, both `extraShare` values, and base-sdk's
/// `QrInfo` decoding the same layout. The commitments are opaque tags, which the encoder writes
/// as they are; the decode cross-check carries none.
#[test]
fn the_encoder_writes_core_order() {
    let nine = [true, false, true, true, false, false, false, false, true];
    let snap_a = wire_snapshot(1, &nine, &[3, -1]);
    let snap_a_bytes: &[u8] = &[1, 0, 0, 0, 9, 0x0d, 0x01, 2, 3, 0, 0, 0, 0xff, 0xff, 0xff, 0xff];
    let snap_b = wire_snapshot(0, &[], &[]);
    let snap_b_bytes: &[u8] = &[0, 0, 0, 0, 0, 0];
    let commitment = tag(&(T.to_base(), [4; 32]));

    for extra in [false, true] {
        let cycles = (0..3 + usize::from(extra))
            .map(|i| CycleData {
                snapshot: if i == 1 {
                    Arc::clone(&snap_a)
                } else {
                    Arc::clone(&snap_b)
                },
                diff: wire_diff(1, 10 + i as u8),
            })
            .collect();
        let mut info = QuorumRotationInfo {
            extra_share: extra,
            cycles,
            diff_tip: wire_diff(1, 2),
            diff_h: wire_diff(1, 3),
            last_commitment_per_index: vec![commitment.clone()],
            quorum_snapshot_list: vec![Arc::clone(&snap_a)],
            mn_list_diff_list: vec![wire_diff(1, 20)],
            legacy: false,
        };
        for proto in [70221, 70228, 70242] {
            let mut expected = Vec::new();
            for s in [snap_b_bytes, snap_a_bytes, snap_b_bytes] {
                expected.extend_from_slice(s);
            }
            for block in [2, 3, 10, 11, 12] {
                expected.extend(wire_diff(1, block).to_bytes(proto));
            }
            expected.push(u8::from(extra));
            if extra {
                expected.extend_from_slice(snap_b_bytes);
                expected.extend(wire_diff(1, 13).to_bytes(proto));
            }
            expected.push(1);
            expected.extend_from_slice(&commitment);
            expected.push(1);
            expected.extend_from_slice(snap_a_bytes);
            expected.push(1);
            expected.extend(wire_diff(1, 20).to_bytes(proto));
            let bytes = info.to_bytes(proto);
            assert_eq!(bytes, expected, "extraShare {extra}, proto {proto}");

            info.last_commitment_per_index.clear();
            let bytes = info.to_bytes(proto);
            info.last_commitment_per_index.push(commitment.clone());
            let version = <dash_p2p_core::ProtocolVersion as Numeric>::from_base(proto);
            let format = dash_p2p_core::MnListFormat::for_version(version);
            let mut data = &bytes[..];
            let decoded = dash_p2p_core::QrInfo::decode_as(&mut data, format).expect("decodes");
            assert!(data.is_empty());
            assert_eq!(decoded.cycle_h_4c().is_some(), extra);
            assert_eq!(decoded.snapshot_h_2c().skip_list(), &[3, -1]);
            assert!(decoded.last_commitment_per_index().is_empty());
            assert_eq!(decoded.quorum_snapshot_list()[0].skip_list(), &[3, -1]);
            assert_eq!(decoded.diff_h().block_hash(), BlockHash::from([3; 32]));
            assert_eq!(decoded.mn_list_diff_list().len(), 1);
        }
    }
}

/// A default `CSimplifiedMNListDiff` as Core sends it at `proto`: null hashes, an empty merkle
/// tree, a version 2 transaction with no inputs or outputs, and empty lists.
fn default_diff_bytes(proto: i32) -> Vec<u8> {
    let mut out = Vec::new();
    if proto >= 70229 {
        out.extend_from_slice(&[1, 0]);
    }
    out.extend_from_slice(&[0; 64]);
    out.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    out.extend_from_slice(&[2, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    if (70225..70229).contains(&proto) {
        out.extend_from_slice(&[1, 0]);
    }
    out.extend_from_slice(&[0; 4]);
    if proto >= 70230 {
        out.push(0);
    }
    out
}

/// Core writes three cycles always and `cycleHMinus4C.value_or(CycleData{})` with `extraShare`
/// (`snap.h:147-167`): a missing cycle is written as a default snapshot and a default diff, so
/// the message always carries what its `extraShare` byte announces.
#[test]
fn the_encoder_writes_core_defaults_for_missing_cycles() {
    let snap = wire_snapshot(1, &[true, false, true], &[7]);
    let snap_bytes: &[u8] = &[1, 0, 0, 0, 3, 0x05, 1, 7, 0, 0, 0];
    let default_snap: &[u8] = &[0, 0, 0, 0, 0, 0];
    let full = (0..3)
        .map(|i| CycleData {
            snapshot: Arc::clone(&snap),
            diff: wire_diff(1, 10 + i),
        })
        .collect::<Vec<_>>();
    for (cycles, extra) in [(full.clone(), true), (full[..1].to_vec(), false), (Vec::new(), true)] {
        let present = cycles.len();
        let info = QuorumRotationInfo {
            extra_share: extra,
            cycles,
            diff_tip: wire_diff(1, 2),
            diff_h: wire_diff(1, 3),
            last_commitment_per_index: Vec::new(),
            quorum_snapshot_list: Vec::new(),
            mn_list_diff_list: Vec::new(),
            legacy: false,
        };
        for proto in [70221, 70228, 70242] {
            let mut expected = Vec::new();
            for i in 0..3 {
                expected.extend_from_slice(if i < present {
                    snap_bytes
                } else {
                    default_snap
                });
            }
            expected.extend(wire_diff(1, 2).to_bytes(proto));
            expected.extend(wire_diff(1, 3).to_bytes(proto));
            for i in 0..3 {
                if i < present {
                    expected.extend(wire_diff(1, 10 + i as u8).to_bytes(proto));
                } else {
                    expected.extend(default_diff_bytes(proto));
                }
            }
            expected.push(u8::from(extra));
            if extra {
                expected.extend_from_slice(default_snap);
                expected.extend(default_diff_bytes(proto));
            }
            expected.extend_from_slice(&[0, 0, 0]);
            let bytes = info.to_bytes(proto);
            assert_eq!(bytes, expected, "{present} cycles, extraShare {extra}, proto {proto}");

            let version = <dash_p2p_core::ProtocolVersion as Numeric>::from_base(proto);
            let format = dash_p2p_core::MnListFormat::for_version(version);
            let mut data = &bytes[..];
            let decoded = dash_p2p_core::QrInfo::decode_as(&mut data, format).expect("decodes");
            assert!(data.is_empty());
            assert_eq!(decoded.cycle_h_4c().is_some(), extra);
        }
    }
}
