//! LQ1's unit cases (EV addendum §2.1, §2.5, §2.6, §4.2, §4.3): the modifier forms (quirk 29), the
//! not-held rule and §2.5's negative controls over a chain view with a floor, Core's LRU, the
//! uncached composition against the model facade, and the attempt model's conformance cases 1–6
//! over the real rotation (LQ1b-E). The rotation's own cases are in `rotation_tests`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};

use dash_consensus::BlockId;
use dash_consensus::HeaderCtx;
use dash_consensus::ctx::{Deployment, DeploymentView};
use dash_consensus::hash::sha256d;
use dash_consensus::primitives::{Tx, TxRef, decode_tx_exact};
use dash_num::{Arith256, CompactTarget};
use dash_params::{ChainOption, ChainParams, ChainParamsBuilder, LlmqParams, NetworkKind, Release};
use dash_primitives::{BlockHash, LlmqType, OutPoint, TxHash};
use dash_types::Numeric;

use super::members::members_uncached;
use super::reference::{
    GateClosed, LatchTripped, LruCache, MembersOutcome, ModelAttempt, ModelMembersFacade,
    ModelQuorumManager, ModelRows, PerTypeCache,
};
use super::*;
use crate::bls::{BlsCtx, bls_ctx_after};
use crate::cbtx::CoinbaseFacts;
use crate::effect::Effect;
use crate::testkit::pro_tx;
use crate::types::ProTxHash;

const TEST: LlmqType = LlmqType::LlmqTest;
const ROTATED: LlmqType = LlmqType::LlmqTestDip0024;
const TIP: u32 = 120;
const NEVER: u32 = 10_000;

// ---- a list, a chain view with a floor, deployments, a source ----------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    pro_tx_hash: ProTxHash,
    collateral: OutPoint,
    confirmed_hash_with_protx: [u8; 32],
    evo: bool,
}

impl MnEntryView for Entry {
    fn pro_tx_hash(&self) -> &ProTxHash {
        &self.pro_tx_hash
    }
    fn collateral(&self) -> &OutPoint {
        &self.collateral
    }
    fn is_evo(&self) -> bool {
        self.evo
    }
    fn is_banned(&self) -> bool {
        false
    }
    fn confirmed_hash_is_null(&self) -> bool {
        false
    }
    fn confirmed_hash_with_protx(&self) -> &[u8; 32] {
        &self.confirmed_hash_with_protx
    }
}

fn entry(n: u8) -> Arc<Entry> {
    Arc::new(Entry {
        pro_tx_hash: pro_tx(n),
        collateral: OutPoint {
            hash: TxHash::from([n; 32]),
            index: 0,
        },
        confirmed_hash_with_protx: sha256d(&[n]),
        evo: n.is_multiple_of(3),
    })
}

#[derive(Clone, Default)]
struct List(Vec<Arc<Entry>>);

impl MnListView for List {
    type Entry = Arc<Entry>;
    type Scratch = List;
    fn counts(&self) -> ListCounts {
        ListCounts {
            total: self.0.len(),
            enabled: self.0.len(),
        }
    }
    fn contains(&self, p: &ProTxHash) -> bool {
        self.0.iter().any(|e| e.pro_tx_hash == *p)
    }
    fn is_pose_banned(&self, _: &ProTxHash) -> bool {
        false
    }
    fn for_each(&self, _: bool, f: &mut dyn FnMut(&Arc<Entry>)) {
        self.0.iter().for_each(f);
    }
    fn new_scratch(&self) -> List {
        List::default()
    }
}

impl ScratchMnList for List {
    type Entry = Arc<Entry>;
    fn try_add(&mut self, e: &Arc<Entry>) -> Result<(), AddMnRejected> {
        // The proTxHash, and the collateral standing in for the unique properties.
        if self.0.iter().any(|o| o.pro_tx_hash == e.pro_tx_hash || o.collateral == e.collateral) {
            return Err(AddMnRejected);
        }
        self.0.push(Arc::clone(e));
        Ok(())
    }
    fn contains(&self, p: &ProTxHash) -> bool {
        MnListView::contains(self, p)
    }
    fn total(&self) -> usize {
        self.0.len()
    }
    fn for_each(&self, _: bool, f: &mut dyn FnMut(&Arc<Entry>)) {
        self.0.iter().for_each(f);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NotHeld(u32);

/// A linear chain `0 ..= TIP` (`BlockId(h)` at height `h`) that holds blocks from `floor`, as an
/// anchored client's view; `faults` are ancestor reads that fail (a read fault on a held block),
/// `voids` ancestor reads answered `Ok(None)` (a broken view).
struct Chain {
    floor: u32,
    faults: BTreeSet<u32>,
    voids: BTreeSet<u32>,
}

fn hash(height: u32) -> BlockHash {
    let mut h = [0u8; 32];
    h[..4].copy_from_slice(&height.to_le_bytes());
    h[31] = 0xe0;
    BlockHash::from(h)
}

impl Chain {
    fn with_floor(floor: u32) -> Chain {
        Chain {
            floor,
            faults: BTreeSet::new(),
            voids: BTreeSet::new(),
        }
    }

    fn held(&self, height: u32) -> Result<BlockId, NotHeld> {
        if (self.floor..=TIP).contains(&height) {
            Ok(BlockId(height))
        } else {
            Err(NotHeld(height))
        }
    }
}

impl LlmqChain for Chain {
    type Error = NotHeld;
    fn header(&self, id: BlockId) -> Result<HeaderCtx, NotHeld> {
        self.held(id.0)?;
        Ok(HeaderCtx {
            height: id.0,
            hash: hash(id.0),
            time: 0,
            bits: CompactTarget::new(0x207f_ffff),
            version: 0x2000_0000,
            mtp: 0,
            chain_work: Arith256::ZERO,
        })
    }
    fn ancestor(&self, id: BlockId, height: u32) -> Result<Option<BlockId>, NotHeld> {
        self.held(id.0)?;
        if height > id.0 {
            return Ok(None);
        }
        if self.faults.contains(&height) {
            return Err(NotHeld(height));
        }
        if self.voids.contains(&height) {
            return Ok(None);
        }
        self.held(height).map(Some)
    }
    fn prev(&self, id: BlockId) -> Result<Option<BlockId>, NotHeld> {
        self.held(id.0)?;
        id.0.checked_sub(1).map(|h| self.held(h)).transpose()
    }
}

/// Buried deployments at their heights; no versionbits deployment.
struct Heights(ChainParams);

impl DeploymentView for Heights {
    fn active_after(&self, prev: Option<BlockId>, deployment: Deployment) -> bool {
        let next = prev.map_or(0, |p| p.0 + 1);
        match deployment {
            Deployment::Buried(d) => next >= self.0.consensus.deployment_height(d).to_u32(),
            Deployment::Versionbits(_) => false,
        }
    }
    fn active_at(&self, block: BlockId, deployment: Deployment) -> bool {
        self.active_after(block.0.checked_sub(1).map(BlockId), deployment)
    }
}

/// Regtest, DIP3 from 2, V19 from 2, with V20 (and MN_RR) and DIP0024 at the given heights.
fn params(v20: u32, dip24: u32) -> ChainParams {
    params_v19(2, v20, dip24)
}

/// Regtest, DIP3 from 2, with V19, V20 (and MN_RR) and DIP0024 at the given heights.
fn params_v19(v19: u32, v20: u32, dip24: u32) -> ChainParams {
    let options = [
        (ChainOption::Dip3Params, "2:2".to_string()),
        (ChainOption::TestActivationHeight, format!("v20@{v20}")),
        (ChainOption::TestActivationHeight, format!("mn_rr@{v20}")),
        (ChainOption::TestActivationHeight, format!("v19@{v19}")),
        (ChainOption::TestActivationHeight, format!("dip0024@{dip24}")),
    ];
    let builder = options
        .into_iter()
        .fold(ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0), |b, (o, v)| {
            b.arg(o, v)
        });
    builder.build().expect("regtest parameters")
}

/// SB7's testnet block 1,569,000, whose CbTx carries a valid ChainLock (basic scheme).
fn fixture_block() -> &'static serde_json::Value {
    static BLOCK: OnceLock<serde_json::Value> = OnceLock::new();
    BLOCK.get_or_init(|| {
        let fixtures: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/data/cbtx_testnet.json")).unwrap();
        let blocks = fixtures["blocks"].as_array().unwrap();
        blocks.iter().find(|b| b["height"] == 1_569_000).unwrap().clone()
    })
}

/// The fixture block's coinbase.
fn fixture_coinbase() -> Tx {
    let raw = unhex(fixture_block()["coinbase_hex"].as_str().unwrap());
    decode_tx_exact(&raw).unwrap().into_value()
}

fn unhex(hex: &str) -> Vec<u8> {
    (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect()
}

/// The fixture coinbase with its CbTx height set to `height` (payload bytes 2..6).
fn chainlock_coinbase(height: u32) -> TxRef {
    let mut raw = fixture_coinbase().to_mutable();
    raw.extra_payload[2..6].copy_from_slice(&height.to_le_bytes());
    Arc::new(Tx::new(raw))
}

/// The fixture's ChainLock signature as Core reports it (`cbTx.bestCLSignature`, basic scheme).
fn signature() -> [u8; 96] {
    unhex(fixture_block()["cbTx"]["bestCLSignature"].as_str().unwrap()).try_into().unwrap()
}

/// Nine masternodes at every height (3, 6 and 9 Evo) unless `lists` names others; withheld lists
/// and coinbases are not held; a coinbase
/// carries the fixture's ChainLock at its height in `chainlocks`, the facts in `facts`, and none
/// elsewhere.
#[derive(Default)]
struct Source {
    lists: BTreeMap<u32, Vec<u8>>,
    /// Lists iterate in reverse (LQ1 §6.5: the order of `for_each` never matters).
    reversed: bool,
    withheld_lists: BTreeSet<u32>,
    withheld_coinbases: BTreeSet<u32>,
    chainlocks: BTreeSet<u32>,
    facts: BTreeMap<u32, CoinbaseFacts>,
}

impl LlmqSource<List> for Source {
    type Error = NotHeld;
    fn mn_list(&self, at: BlockId) -> Result<Option<Arc<List>>, NotHeld> {
        let mut ids = self.lists.get(&at.0).cloned().unwrap_or_else(|| (1..=9).collect());
        if self.reversed {
            ids.reverse();
        }
        Ok((!self.withheld_lists.contains(&at.0))
            .then(|| Arc::new(List(ids.into_iter().map(entry).collect()))))
    }
    fn coinbase_facts(&self, at: BlockId) -> Result<Option<CoinbaseFacts>, NotHeld> {
        if self.withheld_coinbases.contains(&at.0) {
            return Ok(None);
        }
        if let Some(facts) = self.facts.get(&at.0) {
            return Ok(Some(facts.clone()));
        }
        Ok(Some(if self.chainlocks.contains(&at.0) {
            CoinbaseFacts::First(chainlock_coinbase(at.0))
        } else {
            CoinbaseFacts::NoTransactions
        }))
    }
}

/// One fixture: parameters, a chain view and its deployments.
struct Fixture {
    params: ChainParams,
    chain: Chain,
    deployments: Heights,
}

impl Fixture {
    fn new(v20: u32, dip24: u32, floor: u32) -> Fixture {
        let params = params(v20, dip24);
        Fixture {
            deployments: Heights(params.clone()),
            params,
            chain: Chain::with_floor(floor),
        }
    }

    fn ctx(&self) -> LlmqCtx<'_, NotHeld> {
        let bls = bls_ctx_after(&self.deployments, Some(BlockId(TIP)));
        LlmqCtx::new(&self.params, &self.chain, &self.deployments, bls).expect("in domain")
    }
}

fn type_and_hash(t: LlmqType, h: BlockHash) -> QuorumModifier {
    let mut data = vec![t.to_base()];
    data.extend(h.as_bytes());
    QuorumModifier(sha256d(&data))
}

// ---- quirk 29: the modifier's forms -------------------------------------------------------------

#[test]
fn modifier_forms() {
    let src = Source {
        chainlocks: BTreeSet::from([64]),
        ..Source::default()
    };
    let pre = Fixture::new(60, 2, 0);
    let ctx = pre.ctx();
    let test = ctx.llmq(TEST).unwrap();
    let rotated = ctx.llmq(ROTATED).unwrap();
    // Before V20: a non-rotated type hashes the cycle base, a rotated one the work block.
    assert_eq!(hash_modifier(&ctx, &src, test, BlockId(48)), Ok(type_and_hash(TEST, hash(48))));
    assert_eq!(
        hash_modifier(&ctx, &src, rotated, BlockId(48)),
        Ok(type_and_hash(ROTATED, hash(40)))
    );
    // After V20 (active after the work block 64): the ChainLock form, or the work block's hash.
    let mut data = vec![TEST.to_base()];
    data.extend(64u32.to_le_bytes());
    data.extend(signature());
    let post = Fixture::new(40, 2, 0);
    let ctx = post.ctx();
    let test = ctx.llmq(TEST).unwrap();
    assert_eq!(hash_modifier(&ctx, &src, test, BlockId(72)), Ok(QuorumModifier(sha256d(&data))));
    assert_eq!(
        hash_modifier(&ctx, &Source::default(), test, BlockId(72)),
        Ok(type_and_hash(TEST, hash(64)))
    );
    // V20 active after the work block but not at it: Core reads no coinbase, so even a withheld
    // one gives the hash form.
    let edge = Fixture::new(65, 2, 0);
    let ctx = edge.ctx();
    let withheld = Source {
        withheld_coinbases: BTreeSet::from([64]),
        ..Source::default()
    };
    assert_eq!(
        hash_modifier(&ctx, &withheld, ctx.llmq(TEST).unwrap(), BlockId(72)),
        Ok(type_and_hash(TEST, hash(64)))
    );
}

/// LQ1 applies Core's predicate to the work block's facts: a valid ChainLock in a CbTx of another
/// height, a block without transactions, or a block Core cannot read give the hash form.
#[test]
fn the_modifier_applies_the_chainlock_predicate() {
    let post = Fixture::new(40, 2, 0);
    let ctx = post.ctx();
    let test = ctx.llmq(TEST).unwrap();
    let hash_form = Ok(type_and_hash(TEST, hash(64)));
    for facts in [
        CoinbaseFacts::First(chainlock_coinbase(65)),
        CoinbaseFacts::NoTransactions,
        CoinbaseFacts::BlockDataUnavailable,
    ] {
        let src = Source {
            facts: BTreeMap::from([(64, facts.clone())]),
            ..Source::default()
        };
        assert_eq!(hash_modifier(&ctx, &src, test, BlockId(72)), hash_form, "{facts:?}");
    }
    // Control: the same CbTx at its own height gives the ChainLock form.
    let src = Source {
        facts: BTreeMap::from([(64, CoinbaseFacts::First(chainlock_coinbase(64)))]),
        ..Source::default()
    };
    assert_ne!(hash_modifier(&ctx, &src, test, BlockId(72)), hash_form);
    // Under the legacy scheme Core's global flag reads the basic-scheme signature as invalid.
    let legacy = LlmqCtx::new(&post.params, &post.chain, &post.deployments, BlsCtx::LEGACY)
        .expect("in domain");
    assert_eq!(hash_modifier(&legacy, &src, test, BlockId(72)), hash_form);
}

/// The platform type takes Evo masternodes only once V19 is active after the base; before, every
/// type, so a regular masternode can be a member.
#[test]
fn platform_members_are_evo_only_from_v19() {
    let platform = LlmqType::LlmqTestPlatform;
    let members = |v19: u32| -> Vec<Arc<Entry>> {
        let p = params_v19(v19, NEVER, NEVER);
        let f = Fixture {
            deployments: Heights(p.clone()),
            params: p,
            chain: Chain::with_floor(0),
        };
        let ctx = f.ctx();
        compute_quorum_members(&ctx, &Source::default(), platform, BlockId(48))
            .expect("held")
            .to_vec()
    };
    let after = members(2);
    assert_eq!(after.len(), 3);
    assert!(after.iter().all(|e| e.evo), "from V19: Evo only");
    let before = members(NEVER);
    assert_eq!(before.len(), 3);
    assert!(before.iter().any(|e| !e.evo), "before V19: a regular masternode is a member");
}

// ---- §2.1: the not-held rule --------------------------------------------------------------------

#[test]
fn a_withheld_input_is_missing_never_a_value() {
    let post = Fixture::new(40, 2, 0);
    let ctx = post.ctx();
    let test = ctx.llmq(TEST).unwrap();
    let no_coinbase = Source {
        withheld_coinbases: BTreeSet::from([64]),
        ..Source::default()
    };
    assert_eq!(
        hash_modifier(&ctx, &no_coinbase, test, BlockId(72)),
        Err(MemberError::Missing(BlockId(64)))
    );
    assert_eq!(
        compute_quorum_members(&ctx, &no_coinbase, TEST, BlockId(72)).map(|m| m.len()),
        Err(MemberError::Missing(BlockId(64)))
    );
    let no_work_list = Source {
        withheld_lists: BTreeSet::from([64]),
        ..Source::default()
    };
    assert_eq!(
        compute_quorum_members(&ctx, &no_work_list, TEST, BlockId(72)).map(|m| m.len()),
        Err(MemberError::Missing(BlockId(64)))
    );
    let pre = Fixture::new(NEVER, 2, 0);
    let no_base_list = Source {
        withheld_lists: BTreeSet::from([72]),
        ..Source::default()
    };
    assert_eq!(
        compute_quorum_members(&pre.ctx(), &no_base_list, TEST, BlockId(72)).map(|m| m.len()),
        Err(MemberError::Missing(BlockId(72)))
    );
    // A work block below the view's floor is the view's error.
    let anchored = Fixture::new(40, 2, 70);
    assert_eq!(
        compute_quorum_members(&anchored.ctx(), &Source::default(), TEST, BlockId(72))
            .map(|m| m.len()),
        Err(MemberError::View(NotHeld(64)))
    );
    // Control: held, all three compute.
    assert_eq!(
        compute_quorum_members(&ctx, &Source::default(), TEST, BlockId(72)).map(|m| m.len()),
        Ok(3)
    );
}

// ---- §2.5: negative controls (1) and (3), and Core's positive values ----------------------------

#[test]
fn an_unavailable_cycle_predecessor_is_an_error_never_unrotated() {
    // (1) The cycle predecessor (cycleBase - 1 = 47) below the view's floor.
    let anchored = Fixture::new(NEVER, 2, 48);
    let ctx = anchored.ctx();
    let rotated = ctx.llmq(ROTATED).unwrap();
    assert_eq!(
        is_quorum_rotation_enabled(&ctx, rotated, BlockId(49)),
        Err(MemberError::View(NotHeld(47)))
    );
    assert_eq!(members_route(&ctx, ROTATED, BlockId(49)), Err(MemberError::View(NotHeld(47))));

    // In Core's precompute loop (EV addendum §2.5) at block 48, whose parent 47 is held but whose
    // predecessor read faults.
    let mut faulting = Fixture::new(NEVER, 2, 0);
    faulting.chain.faults.insert(47);
    let ctx = faulting.ctx();
    let precompute = |ctx: &LlmqCtx<'_, NotHeld>| -> Result<Vec<LlmqType>, LlmqError<NotHeld>> {
        let block = BlockId(48);
        let mut computed = Vec::new();
        for p in enabled_quorum_params(ctx, BlockId(47))? {
            if is_quorum_rotation_enabled(ctx, p, block)? && 48 % p.dkg_interval.unsigned_abs() == 0
            {
                computed.push(p.llmq_type);
            }
        }
        Ok(computed)
    };
    assert_eq!(precompute(&ctx), Err(MemberError::View(NotHeld(47))));
    // A view answering `Ok(None)` for the held predecessor breaks its contract: not held.
    let mut broken = Fixture::new(NEVER, 2, 0);
    broken.chain.voids.insert(47);
    let ctx = broken.ctx();
    assert_eq!(precompute(&ctx), Err(MemberError::Missing(BlockId(48))));
    // Control: the same loop over a sound view computes the rotated types.
    let sound = Fixture::new(NEVER, 2, 0);
    assert_eq!(precompute(&sound.ctx()), Ok(vec![ROTATED]));

    // Core's value: a cycle base below 1 is not rotated, with no read below the floor.
    let young = Fixture::new(NEVER, 2, 5);
    let ctx = young.ctx();
    assert_eq!(is_quorum_rotation_enabled(&ctx, ctx.llmq(ROTATED).unwrap(), BlockId(5)), Ok(false));
}

#[test]
fn a_gate_parent_at_the_floor_is_an_error_never_gated() {
    // (3) The base's parent below the view's floor.
    let anchored = Fixture::new(NEVER, 2, 72);
    assert_eq!(
        members_gate(&anchored.ctx(), TEST, BlockId(72)),
        Err(MemberError::View(NotHeld(71)))
    );
    // Core's value: genesis has no parent.
    let full = Fixture::new(NEVER, 2, 0);
    assert_eq!(members_gate(&full.ctx(), TEST, BlockId(0)), Ok(false));
    assert_eq!(members_gate(&full.ctx(), TEST, BlockId(72)), Ok(true));
}

#[test]
fn the_context_checks_the_parameters_domain() {
    let f = Fixture::new(NEVER, NEVER, 0);
    let bls = bls_ctx_after(&f.deployments, None);
    assert!(LlmqCtx::new(&f.params, &f.chain, &f.deployments, bls).is_ok());
    let outside: [fn(&mut LlmqParams); 10] = [
        |p| p.dkg_interval = 0,
        |p| p.keep_old_connections = 0,
        |p| p.keep_old_keys = 0,
        |p| p.signing_active_quorum_count = 0,
        |p| p.signing_active_quorum_count = -1,
        |p| p.dkg_mining_window_end = p.dkg_mining_window_start,
        // A scan-cache size past Core's `int`.
        |p| p.dkg_mining_window_end = i32::MAX,
        |p| {
            p.use_rotation = true;
            p.signing_active_quorum_count = 65_537;
        },
        // A negative rotated size: Core's quarter size would be a huge `size_t`.
        |p| {
            p.use_rotation = true;
            p.size = -4;
        },
        // Fewer connections kept than a rotated cycle's quorums: no scan-cache cycle.
        |p| {
            p.use_rotation = true;
            p.keep_old_connections = p.signing_active_quorum_count - 1;
        },
    ];
    for edit in outside {
        let mut params = f.params.clone();
        let test = params.consensus.llmqs.iter_mut().find(|p| p.llmq_type == TEST);
        edit(test.expect("configured"));
        let ctx = LlmqCtx::new(&params, &f.chain, &f.deployments, bls);
        assert_eq!(ctx.err(), Some(ParamsOutOfDomain(TEST)));
    }
}

// ---- Core's unordered_lru_cache and PerLlmqTypeCache --------------------------------------------

#[test]
fn lru_get_stamps_on_a_hit_only_and_truncation_keeps_the_newest() {
    let mut cache = LruCache::new(2);
    for k in 1..=4 {
        cache.insert(k, k * 10);
    }
    let mut missed = cache.clone();
    assert_eq!(missed.get(&9), None);
    assert!(!missed.exists(&9));
    assert_eq!(missed, cache, "a miss stamps nothing");
    // A hit stamps 1 as the newest; the fifth insert truncates to the two newest.
    assert_eq!(cache.get(&1), Some(10));
    cache.insert(5, 50);
    assert_eq!(cache.len(), 2);
    assert_eq!((cache.peek(&1), cache.peek(&5)), (Some(&10), Some(&50)));
    // `exists` stamps as `get` does; `peek` does not.
    let mut a = cache.clone();
    let mut b = cache.clone();
    assert!(a.exists(&1));
    assert_eq!(b.peek(&1), Some(&10));
    for c in [&mut a, &mut b] {
        c.insert(6, 60);
        c.insert(7, 70);
        c.insert(8, 80);
    }
    assert!(a.peek(&1).is_none() && b.peek(&1).is_none());
    let mut c = cache.clone();
    assert!(c.exists(&1));
    c.insert(6, 60);
    c.insert(7, 70);
    assert_eq!(c.peek(&1), Some(&10), "the stamped entry is among the newest");
    // Over twice the size, exactly the size remains.
    let mut d = LruCache::new(3);
    for k in 0..7 {
        d.insert(k, ());
    }
    assert_eq!(d.len(), 3);
    assert!((4..7).all(|k| d.peek(&k).is_some()));
}

#[test]
fn per_type_cache_holds_configured_types_only() {
    let p = params(NEVER, 2);
    let mut cache: PerTypeCache<u32, u32> = PerTypeCache::new(&p, true);
    let test = p.consensus.llmq(TEST).unwrap();
    assert_eq!(
        cache.cache(TEST).unwrap().max_size(),
        usize::try_from(test.keep_old_connections).unwrap()
    );
    cache.insert(LlmqType::Llmq400_60, 1, 1);
    assert_eq!(
        cache.get(LlmqType::Llmq400_60, &1),
        None,
        "an unconfigured type's write is dropped"
    );
    cache.insert(TEST, 1, 1);
    assert_eq!(cache.get(TEST, &1), Some(1));
    cache.clear(TEST);
    assert_eq!(cache.get(TEST, &1), None);
}

#[test]
fn the_quorum_manager_sizes_its_caches_as_core() {
    // Core's `mapQuorumsCache` by `keepOldKeys` and `scanQuorumsCache` by
    // `max_cycles(keepOldConnections) * (dkgMiningWindowEnd - dkgMiningWindowStart)`.
    let p = params(NEVER, 2);
    let manager: ModelQuorumManager<Arc<Entry>> = ModelQuorumManager::new(&p);
    // LLMQ_TEST: keepOldKeys 4, keepOldConnections 3, window 10..18.
    assert_eq!(manager.cache_sizes(TEST), Some((4, 24)));
    // LLMQ_TEST_DIP0024: keepOldKeys 4, keepOldConnections 4 over 2 signing quorums (2 cycles),
    // window 12..20.
    assert_eq!(manager.cache_sizes(ROTATED), Some((4, 16)));
    assert_eq!(manager.cache_sizes(LlmqType::Llmq400_60), None);
}

// ---- §4.2: members_uncached against the model facade --------------------------------------------

#[derive(Default)]
struct Rows(ModelRows);

impl SnapshotRows for Rows {
    type Error = NotHeld;
    fn get(&self, t: LlmqType, b: &BlockHash) -> Result<Option<QuorumSnapshot>, NotHeld> {
        Ok(self.0.get(&(t.to_base(), *b)).cloned())
    }
    fn store(&mut self, t: LlmqType, b: &BlockHash, s: &QuorumSnapshot) -> Result<(), NotHeld> {
        self.0.insert((t.to_base(), *b), s.clone());
        Ok(())
    }
}

#[test]
fn members_uncached_equals_the_cold_facade() {
    for v20 in [40, NEVER] {
        let f = Fixture::new(v20, NEVER, 0);
        let ctx = f.ctx();
        let src = Source {
            chainlocks: BTreeSet::from([64]),
            ..Source::default()
        };
        for (t, base) in [
            (TEST, 0),
            (TEST, 24),
            (TEST, 25),
            (TEST, 72),
            (TEST, 96),
            (LlmqType::LlmqTestPlatform, 48),
        ] {
            let uncached =
                members_uncached(&ctx, &src, &mut Rows::default(), t, BlockId(base)).expect("held");
            let mut cold = ModelMembersFacade::new(&f.params);
            let (facade, _) = cold
                .members(&ctx, &src, &mut Rows::default(), t, BlockId(base), false)
                .expect("held");
            assert_eq!(uncached, facade, "{t:?} at {base}");
        }
    }
}

// ---- §2.6: the attempt model's conformance cases 1-6, over the real rotation -------------------

type Members = QuorumMembers<Arc<Entry>>;

fn row(t: &ModelAttempt<Arc<Entry>>, cycle_base: u32) -> Option<QuorumSnapshot> {
    t.rows().get(&(ROTATED.to_base(), hash(cycle_base))).cloned()
}

/// LLMQ_TEST_DIP0024 (size 4, one member a quarter, two quorums a cycle, interval 24) rotating
/// from DIP0024 at 2, before V20.
fn rotated_fixture() -> Fixture {
    Fixture::new(NEVER, 2, 0)
}

/// A source whose work block of cycle 72 (block 64) holds only masternodes `ids`: a different
/// input for that cycle's rotation, so a different row.
fn work_64(ids: &[u8]) -> Source {
    Source {
        lists: BTreeMap::from([(64, ids.to_vec())]),
        ..Source::default()
    }
}

/// What the rotation at `cycle_base` gives over `rows` (a copy: the caller's rows are untouched):
/// its quorums and the row it stores.
fn rotate(
    ctx: &LlmqCtx<'_, NotHeld>,
    src: &Source,
    rows: &ModelRows,
    cycle_base: u32,
) -> (Vec<Members>, Option<QuorumSnapshot>) {
    let mut rows = Rows(rows.clone());
    let quorums =
        compute_by_quarter_rotation(ctx, src, &mut rows, ROTATED, BlockId(cycle_base)).unwrap();
    (quorums, rows.0.remove(&(ROTATED.to_base(), hash(cycle_base))))
}

fn stores(log: &[Effect]) -> Vec<(BlockHash, QuorumSnapshot)> {
    log.iter()
        .filter_map(|e| match e {
            Effect::StoreQuorumSnapshot {
                cycle_base,
                snapshot,
                ..
            } => Some((*cycle_base, snapshot.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn case_1_two_calls_one_attempt() {
    let f = rotated_fixture();
    let ctx = f.ctx();
    let src = Source::default();
    let run = |per_call: bool| {
        let mut model = ModelAttempt::new(&f.params);
        let id = model.begin().unwrap();
        model.attempt_members(id, &ctx, &src, ROTATED, BlockId(48), false).unwrap();
        let call = if per_call {
            ModelAttempt::per_call_members
        } else {
            ModelAttempt::attempt_members
        };
        let (members, outcome) =
            call(&mut model, id, &ctx, &src, ROTATED, BlockId(72), false).unwrap();
        assert_eq!(outcome, MembersOutcome::Computed);
        // The row call 2 stored, from the attempt's log (the control's overlay is gone).
        let stored = stores(model.log(id)).into_iter().rev().find(|(b, _)| *b == hash(72));
        (members, stored.expect("call 2 stored its row").1)
    };
    let (members, stored) = run(false);
    // Call 2 read call 1's row: cycle 72's quorums carry cycle 48's quarter.
    let row_48 = rotate(&ctx, &src, &ModelRows::new(), 48).1.unwrap();
    let with_48 = ModelRows::from([((ROTATED.to_base(), hash(48)), row_48)]);
    let (expected, expected_row) = rotate(&ctx, &src, &with_48, 72);
    assert_eq!((members.clone(), stored.clone()), (expected[0].clone(), expected_row.unwrap()));
    assert_eq!(members.len(), 2, "H-C's quarter and the new one");
    // Negative control: a per-call overlay reads no row, and the case fails.
    let (control, control_row) = run(true);
    assert_eq!(control.len(), 1, "no previous cycle");
    assert_ne!((members, stored), (control, control_row));
}

#[test]
fn case_2_overwrite() {
    let f = rotated_fixture();
    let ctx = f.ctx();
    let (first, second) = (work_64(&[1, 2, 3, 4, 5, 6, 7, 8, 9]), work_64(&[1, 2, 3, 4, 5, 6]));
    let mut model = ModelAttempt::new(&f.params);
    let id = model.begin().unwrap();
    model.attempt_members(id, &ctx, &first, ROTATED, BlockId(72), false).unwrap();
    // The undo path's reset recomputes over the changed inputs and overwrites the row.
    model.attempt_members(id, &ctx, &second, ROTATED, BlockId(72), true).unwrap();
    let r1 = rotate(&ctx, &first, &ModelRows::new(), 72).1.unwrap();
    let r2 = rotate(&ctx, &second, &ModelRows::new(), 72).1.unwrap();
    assert_ne!(r1, r2, "discrimination");
    assert_eq!(model.attempt_snapshot(id, ROTATED, &hash(72)), Some(r2.clone()));
    assert_eq!(stores(model.log(id)), vec![(hash(72), r1), (hash(72), r2.clone())]);
    model.release(id, &mut |_| false).unwrap();
    assert_eq!(row(&model, 72), Some(r2));
}

#[test]
fn case_3_attempt_isolation() {
    let f = rotated_fixture();
    let ctx = f.ctx();
    let src = Source::default();
    let mut model = ModelAttempt::new(&f.params);
    let first = model.begin().unwrap();
    model.attempt_members(first, &ctx, &src, ROTATED, BlockId(48), false).unwrap();
    assert_eq!(model.begin(), Err(GateClosed::Blocked), "one attempt at a time");
    assert_eq!(row(&model, 48), None, "not in the runtime rows before the log applies");
    model.release(first, &mut |_| false).unwrap();
    let r48 = rotate(&ctx, &src, &ModelRows::new(), 48).1;
    assert_eq!(row(&model, 48), r48);
    let second = model.begin().unwrap();
    assert!(model.log(second).is_empty());
    assert_eq!(model.attempt_snapshot(second, ROTATED, &hash(48)), r48);
    // The second attempt's computation reads the first's row from backing.
    model.attempt_members(second, &ctx, &src, ROTATED, BlockId(72), false).unwrap();
    let with_48 = model.rows().clone();
    assert_eq!(
        model.attempt_snapshot(second, ROTATED, &hash(72)),
        rotate(&ctx, &src, &with_48, 72).1
    );
    assert_ne!(
        rotate(&ctx, &src, &with_48, 72),
        rotate(&ctx, &src, &ModelRows::new(), 72),
        "discrimination: the row was read"
    );
}

#[test]
fn case_4_failed_and_check_only_attempts_still_apply() {
    let f = rotated_fixture();
    let ctx = f.ctx();
    let src = Source::default();
    // The model applies the log whatever the outcome; the outcome names the run.
    for outcome in ["fails after the store", "fJustCheck", "GBT proposal"] {
        let mut model = ModelAttempt::new(&f.params);
        let id = model.begin().unwrap();
        // LQ2's process_block precomputes first, before any commitment check can fail.
        model.attempt_precompute(id, &ctx, &src, BlockId(48), PrecomputeMode::Connect).unwrap();
        assert_eq!(stores(model.log(id)).len(), 1, "{outcome}: the row is an effect");
        model.release(id, &mut |_| false).unwrap();
        let (quorums, r48) = rotate(&ctx, &src, &ModelRows::new(), 48);
        assert_eq!(row(&model, 48), r48, "{outcome}");
        for (base, quorum) in [(48, &quorums[0]), (49, &quorums[1])] {
            assert_eq!(
                model.facade().peek(&ctx, ROTATED, BlockId(base)),
                Ok(Some(quorum.clone())),
                "{outcome}"
            );
        }
    }
}

#[test]
fn case_5_a_runtime_write_failure_trips_the_latch_once() {
    let f = rotated_fixture();
    let ctx = f.ctx();
    let src = Source::default();
    let mut model = ModelAttempt::new(&f.params);
    let id = model.begin().unwrap();
    model.attempt_members(id, &ctx, &src, ROTATED, BlockId(48), false).unwrap();
    model.attempt_members(id, &ctx, &src, ROTATED, BlockId(72), false).unwrap();
    assert_eq!(stores(model.log(id)).len(), 2);
    let mut writes = 0;
    assert_eq!(
        model.release(id, &mut |_| {
            writes += 1;
            true
        }),
        Err(LatchTripped)
    );
    assert_eq!((model.aborts(), writes), (1, 1), "one abort_node, no later write");
    assert_eq!((row(&model, 48), row(&model, 72)), (None, None));
    assert_eq!(model.begin(), Err(GateClosed::Latched));
    assert_eq!(model.external_snapshot(ROTATED, &hash(48)), Err(GateClosed::Latched));
}

#[test]
fn case_6_an_external_reader_waits_for_the_open_attempt() {
    let f = rotated_fixture();
    let ctx = f.ctx();
    let src = Source::default();
    // S0: a row the rotation never writes (an older write with other inputs).
    let s0 = QuorumSnapshot {
        active_quorum_members: vec![false; 2],
        skip_list_mode: 9,
        skip_list: vec![],
    };
    for outcome in ["succeeds", "fails", "GBT proposal"] {
        // hit: the attempt's own base (base cache), or index 1's base, whose base entry is absent.
        for (base, hit) in [(48, MembersOutcome::BaseHit), (49, MembersOutcome::IndexHit)] {
            let what = format!("{outcome}, {hit:?}");
            // Cycle 48 has row S0 and cold caches; the attempt recomputes it to (M1, S1).
            let mut model = ModelAttempt::new(&f.params);
            model.seed_row(ROTATED, hash(48), s0.clone());
            let id = model.begin().unwrap();
            model.attempt_members(id, &ctx, &src, ROTATED, BlockId(48), false).unwrap();
            let s1 = model.attempt_snapshot(id, ROTATED, &hash(48)).unwrap();
            let m1 = model.facade().peek(&ctx, ROTATED, BlockId(base)).unwrap().expect("published");
            assert_ne!(s1, s0);

            // Negative control: no gate exposes (M1, S0).
            let mut control = model.clone();
            let (members, _) =
                control.direct_members(&ctx, &src, ROTATED, BlockId(base), false).unwrap();
            assert_eq!(
                (members, control.direct_snapshot(ROTATED, &hash(48))),
                (m1.clone(), Some(s0.clone())),
                "{what}"
            );

            // The external call and the route-A observer wait while the attempt is open.
            let blocked =
                model.external_members(&ctx, &src, ROTATED, BlockId(base), false).map(|_| ());
            assert_eq!(blocked, Err(GateClosed::Blocked), "{what}");
            assert_eq!(
                model.external_snapshot(ROTATED, &hash(48)),
                Err(GateClosed::Blocked),
                "{what}"
            );
            let base_entry = model.facade().base_cache().peek(ROTATED, &hash(base)).is_some();
            assert_eq!(base_entry, base == 48, "{what}: no base insert while waiting");

            model.release(id, &mut |_| false).unwrap();
            let (members, outcome) =
                model.external_members(&ctx, &src, ROTATED, BlockId(base), false).unwrap().unwrap();
            assert_eq!((members, outcome), (m1, hit), "{what}");
            assert_eq!(model.external_snapshot(ROTATED, &hash(48)), Ok(Some(s1)), "{what}");
        }
    }
}

// ---- §4.3: a warm cache outlives the work block's data ------------------------------------------

#[test]
fn a_cached_roster_survives_the_work_block_data_going_away() {
    let f = Fixture::new(40, NEVER, 0);
    let ctx = f.ctx();
    let with_chainlock = Source {
        chainlocks: BTreeSet::from([64]),
        ..Source::default()
    };
    let mut facade = ModelMembersFacade::new(&f.params);
    let (cached, _) = facade
        .members(&ctx, &with_chainlock, &mut Rows::default(), TEST, BlockId(72), false)
        .unwrap();
    // The block's data goes away: Core's predicate then gives no ChainLock (the hash form).
    let pruned = Source::default();
    let before = facade.clone();
    assert_eq!(facade.peek(&ctx, TEST, BlockId(72)), Ok(Some(cached.clone())), "the hit path");
    assert_eq!(facade, before, "peek stamps and inserts nothing");
    let fresh = members_uncached(&ctx, &pruned, &mut Rows::default(), TEST, BlockId(72)).unwrap();
    assert_ne!(fresh, cached, "discrimination: a fresh computation differs");
    // Cold: a miss, for the uncached path.
    assert_eq!(
        ModelMembersFacade::<Arc<Entry>>::new(&f.params).peek(&ctx, TEST, BlockId(72)),
        Ok(None)
    );
}

// ---- LQ1b: the rotation's own cases ---------------------------------------------------------

/// A deterministic generator for the exhaustive-or-sampled cases below (no new dev-dependency).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 =
            self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
}

fn encode(indices: &[usize]) -> Vec<i32> {
    let mut encoder = SkipListEncoder::default();
    indices.iter().for_each(|&i| encoder.push(i));
    encoder.finish()
}

#[test]
fn the_skip_list_decoder_inverts_the_encoder() {
    // Every sequence of up to four skips over small indices, including skips at 0 before the
    // first non-zero one and indices below the first (the cursor wrapped).
    let domain = [0usize, 1, 2, 5, 7];
    let mut sequences = vec![vec![]];
    for _ in 0..4 {
        let longer: Vec<Vec<usize>> = sequences
            .iter()
            .flat_map(|s| domain.iter().map(move |&d| [s.clone(), vec![d]].concat()))
            .collect();
        sequences.extend(longer.into_iter().filter(|s| s.len() <= 4));
        sequences.sort();
        sequences.dedup();
    }
    assert_eq!(sequences.len(), 1 + 5 + 25 + 125 + 625);
    let mut rng = Lcg(28);
    for _ in 0..2_000 {
        let len = (rng.next() % 64) as usize;
        sequences.push((0..len).map(|_| (rng.next() % (1 << 31)) as usize).collect());
    }
    for indices in sequences {
        let decoded = decode_skip_list(&encode(&indices));
        let expected: Vec<i32> = indices.iter().map(|&i| i as i32).collect();
        assert_eq!(decoded, expected, "{indices:?}");
    }
    // 28c: an entry below the first is negative.
    assert_eq!(encode(&[5, 7, 2]), vec![5, 2, -3]);
    assert_eq!(encode(&[0, 0, 3, 4]), vec![0, 0, 3, 1]);
}

fn snapshot(bits: &[bool], mode: i32, skip_list: &[i32]) -> QuorumSnapshot {
    QuorumSnapshot {
        active_quorum_members: bits.to_vec(),
        skip_list_mode: mode,
        skip_list: skip_list.to_vec(),
    }
}

#[test]
fn quarters_by_snapshot_fill_by_mode_and_terminate_on_any_row() {
    let f = rotated_fixture();
    let ctx = f.ctx();
    let src = Source::default();
    let params = ctx.llmq(ROTATED).unwrap();
    let quarters = |s: &QuorumSnapshot| {
        quarters_by_snapshot(&ctx, &src, params, BlockId(48), s).expect("held")
    };
    // 28g: modes 2, 3 and unknown values give two empty quarters.
    for mode in [2, 3, 4, -1, i32::MAX] {
        assert_eq!(quarters(&snapshot(&[false; 9], mode, &[0])), vec![vec![], vec![]], "{mode}");
    }
    // Mode 0: one cursor, one member a quarter.
    let mode0 = quarters(&snapshot(&[false; 9], 0, &[]));
    assert_eq!(mode0.iter().map(Vec::len).collect::<Vec<_>>(), vec![1, 1]);
    assert_ne!(mode0[0], mode0[1]);
    // A row shorter than the list: the bits past its end are unused (LQ1b D18), so an empty
    // bitset reads as all-unused.
    assert_eq!(quarters(&snapshot(&[], 0, &[])), mode0);
    // Mode 1: skipping index 0 shifts both quarters by one.
    let skip0 = quarters(&snapshot(&[false; 9], 1, &[0]));
    assert_eq!(skip0[0], mode0[1]);
    // Any skip list terminates with full quarters (each step pushes or consumes an entry, A-m5):
    // garbage, negative, unsorted and out-of-range entries included.
    let mut rng = Lcg(29);
    for _ in 0..500 {
        let len = (rng.next() % 12) as usize;
        let skips: Vec<i32> = (0..len).map(|_| (rng.next() as i32 % 24) - 6).collect();
        let bits: Vec<bool> =
            (0..(rng.next() % 12)).map(|_| rng.next().is_multiple_of(2)).collect();
        let q = quarters(&snapshot(&bits, 1, &skips));
        assert_eq!(q.iter().map(Vec::len).collect::<Vec<_>>(), vec![1, 1], "{skips:?} {bits:?}");
    }
    // Every masternode used: the used ones still fill the quarters (the combined list's tail).
    assert_eq!(quarters(&snapshot(&[true; 9], 0, &[])).iter().map(Vec::len).sum::<usize>(), 2);
}

#[test]
fn a_row_rebuilds_the_new_quarters_it_was_built_from() {
    // Cycle 72 over a four-masternode work list, with previous quarters chosen to force skips:
    // the row's reconstruction gives back the new quarters wherever they are full (28f aside).
    let f = rotated_fixture();
    let ctx = f.ctx();
    let src = work_64(&[1, 2, 3, 4]);
    let params = ctx.llmq(ROTATED).unwrap();
    let modifier = hash_modifier(&ctx, &src, params, BlockId(72)).unwrap();
    let all = src.mn_list(BlockId(64)).unwrap().unwrap();
    let (mut checked, mut with_skips) = (0, 0);
    let ids = [1u8, 2, 3, 4];
    for a in ids {
        for b in ids {
            for c in ids {
                for d in ids {
                    let previous: [CycleQuarters<Arc<Entry>>; PREVIOUS_CYCLES] = [
                        vec![vec![entry(a)], vec![entry(b)]],
                        vec![vec![entry(c)], vec![entry(d)]],
                        vec![vec![], vec![]],
                    ];
                    let new = new_quarters(params, &*all, &previous, &modifier, true);
                    let Some((used, skip_list)) = new.snapshot_inputs else {
                        continue;
                    };
                    if new.quarters.iter().any(|q| q.len() != 1) {
                        continue;
                    }
                    with_skips += usize::from(!skip_list.is_empty());
                    let row = build_quorum_snapshot(&*all, &used, skip_list, &modifier);
                    assert_eq!(row.active_quorum_members.len(), 4);
                    let rebuilt =
                        quarters_by_snapshot(&ctx, &src, params, BlockId(72), &row).expect("held");
                    assert_eq!(rebuilt, new.quarters, "{a} {b} {c} {d}");
                    checked += 1;
                }
            }
        }
    }
    assert!(
        checked > 100 && with_skips > 20,
        "non-vacuous: {checked} checked, {with_skips} skipping"
    );
}

#[test]
fn new_quarters_return_early_without_a_row() {
    let f = rotated_fixture();
    let ctx = f.ctx();
    let params = ctx.llmq(ROTATED).unwrap();
    let empty: [CycleQuarters<Arc<Entry>>; PREVIOUS_CYCLES] =
        std::array::from_fn(|_| vec![vec![], vec![]]);
    let modifier = QuorumModifier([7; 32]);
    // 28e-1: fewer enabled masternodes than a quarter.
    let new = new_quarters(params, &List::default(), &empty, &modifier, true);
    assert!(new.snapshot_inputs.is_none());
    assert_eq!(new.quarters, vec![Vec::<Arc<Entry>>::new(), vec![]]);
    // 28e-2: a full loop over the combined list adds no one. Index 0 used 1, and 2 has taken
    // 1's collateral (a unique property): the combined list is 2 then 1, 1 is index 0's and
    // adding 2 collides, so Core returns no quarters at all and stores no row.
    let taken = Arc::new(Entry {
        collateral: entry(1).collateral,
        ..(*entry(2)).clone()
    });
    let used_by_0: [CycleQuarters<Arc<Entry>>; PREVIOUS_CYCLES] =
        [vec![vec![entry(1)], vec![]], vec![vec![], vec![]], vec![vec![], vec![]]];
    let new = new_quarters(params, &List(vec![entry(1), taken]), &used_by_0, &modifier, true);
    assert!(new.snapshot_inputs.is_none(), "no row");
    assert_eq!(new.quarters, vec![Vec::<Arc<Entry>>::new(), vec![]]);
    // The guard, unlike 28e-2: one masternode used by both indices admits no step, and the row
    // is stored.
    let used_by_both: [CycleQuarters<Arc<Entry>>; PREVIOUS_CYCLES] =
        [vec![vec![entry(1)], vec![entry(1)]], vec![vec![], vec![]], vec![vec![], vec![]]];
    let new = new_quarters(params, &List(vec![entry(1)]), &used_by_both, &modifier, true);
    assert_eq!(new.quarters, vec![Vec::<Arc<Entry>>::new(), vec![]]);
    assert_eq!(new.snapshot_inputs.map(|(_, s)| s), Some(vec![]), "the guard stops: a row");
    // Control: a fresh masternode fills both quarters.
    let new = new_quarters(params, &List(vec![entry(1), entry(2)]), &empty, &modifier, true);
    assert_eq!(new.quarters.iter().map(Vec::len).collect::<Vec<_>>(), vec![1, 1]);
}

#[test]
fn removed_masternodes_are_skipped_from_v19_and_always_on_testnet() {
    // 28d: Core's `skipRemovedMNs` tests the cycle base's height + 1 against V19, except on
    // testnet. Only the full testnet replay (LQ1b L2) reaches the testnet clause otherwise.
    let skip = super::rotation::skip_removed_mns;
    let build = |net| ChainParamsBuilder::new(net, Release::V24_0).build().expect("parameters");
    let (mainnet, testnet) = (build(NetworkKind::Mainnet), build(NetworkKind::Testnet));
    assert!(!skip(&mainnet, 1_898_784) && skip(&mainnet, 1_899_072), "mainnet: V19 1,899,072");
    assert!(!skip(&mainnet, 1_899_070) && skip(&mainnet, 1_899_071), "the cycle base + 1");
    assert!(skip(&testnet, 769_824) && skip(&testnet, 0), "testnet: DIP0024 on, before V19");
    let regtest = params_v19(500, NEVER, 2);
    assert!(!skip(&regtest, 498) && skip(&regtest, 499), "regtest follows V19");
}

#[test]
fn rotated_members_do_not_depend_on_list_order() {
    let f = rotated_fixture();
    let ctx = f.ctx();
    let (forward, reversed) = (
        Source::default(),
        Source {
            reversed: true,
            ..Source::default()
        },
    );
    let (mut a, mut b) = (Rows::default(), Rows::default());
    for cycle in [24, 48, 72, 96] {
        let qa = compute_by_quarter_rotation(&ctx, &forward, &mut a, ROTATED, BlockId(cycle));
        let qb = compute_by_quarter_rotation(&ctx, &reversed, &mut b, ROTATED, BlockId(cycle));
        assert_eq!(qa, qb, "cycle {cycle}");
    }
    assert_eq!(a.0, b.0);
    assert_eq!(a.0.len(), 4);
}

// ---- LQ1b-D: the rotated reads' negative controls (EV addendum §2.5 (2), §2.1) -----------------

/// Rows that do not hold the rows of `withheld` cycle bases: an `Err`, never Core's absence.
struct WithheldRows(ModelRows, BTreeSet<u32>);

impl SnapshotRows for WithheldRows {
    type Error = NotHeld;
    fn get(&self, t: LlmqType, b: &BlockHash) -> Result<Option<QuorumSnapshot>, NotHeld> {
        if let Some(&h) = self.1.iter().find(|&&h| hash(h) == *b) {
            return Err(NotHeld(h));
        }
        Ok(self.0.get(&(t.to_base(), *b)).cloned())
    }
    fn store(&mut self, t: LlmqType, b: &BlockHash, s: &QuorumSnapshot) -> Result<(), NotHeld> {
        self.0.insert((t.to_base(), *b), s.clone());
        Ok(())
    }
}

/// The rows Core holds after cycles 24 and 48 computed.
fn rows_through_48(ctx: &LlmqCtx<'_, NotHeld>) -> ModelRows {
    let mut rows = Rows::default();
    for cycle in [24, 48] {
        compute_by_quarter_rotation(ctx, &Source::default(), &mut rows, ROTATED, BlockId(cycle))
            .unwrap();
    }
    rows.0
}

#[test]
fn a_snapshot_row_not_held_is_an_error_never_a_break() {
    let f = rotated_fixture();
    let ctx = f.ctx();
    let src = Source::default();
    let held = rows_through_48(&ctx);
    let mut not_held = WithheldRows(held.clone(), BTreeSet::from([48]));
    assert_eq!(
        compute_by_quarter_rotation(&ctx, &src, &mut not_held, ROTATED, BlockId(72)),
        Err(MemberError::View(NotHeld(48)))
    );
    assert_eq!(not_held.0, held, "no row stored");
    let mut not_held = WithheldRows(held.clone(), BTreeSet::from([48]));
    assert_eq!(
        members_uncached(&ctx, &src, &mut not_held, ROTATED, BlockId(73)),
        Err(MemberError::View(NotHeld(48)))
    );
    // Controls: held, the quorum carries H-C's and H-2C's quarters; absent (Core's value), the
    // rotation breaks at H-C and the quorum has the new quarter only.
    let mut rows = Rows(held.clone());
    assert_eq!(
        members_uncached(&ctx, &src, &mut rows, ROTATED, BlockId(73)).map(|m| m.len()),
        Ok(3)
    );
    let mut absent = held;
    absent.remove(&(ROTATED.to_base(), hash(48)));
    let mut rows = Rows(absent);
    assert_eq!(
        members_uncached(&ctx, &src, &mut rows, ROTATED, BlockId(73)).map(|m| m.len()),
        Ok(1)
    );
}

#[test]
fn a_previous_cycle_not_held_is_an_error_never_a_break() {
    let full = rotated_fixture();
    let held = rows_through_48(&full.ctx());
    let src = Source::default();
    // (2) The previous cycle's block below an anchored view's floor (the work block 64 and the
    // cycle predecessor 71 are held).
    let anchored = Fixture::new(NEVER, 2, 50);
    let ctx = anchored.ctx();
    assert_eq!(
        compute_by_quarter_rotation(&ctx, &src, &mut Rows(held.clone()), ROTATED, BlockId(72)),
        Err(MemberError::View(NotHeld(48)))
    );
    // A read fault on a held previous cycle.
    let mut faulting = rotated_fixture();
    faulting.chain.faults.insert(48);
    assert_eq!(
        compute_by_quarter_rotation(
            &faulting.ctx(),
            &src,
            &mut Rows(held.clone()),
            ROTATED,
            BlockId(72)
        ),
        Err(MemberError::View(NotHeld(48)))
    );
    // A view answering `Ok(None)` for it breaks its contract: not held, for the work block read
    // from.
    let mut broken = rotated_fixture();
    broken.chain.voids.insert(48);
    assert_eq!(
        compute_by_quarter_rotation(
            &broken.ctx(),
            &src,
            &mut Rows(held.clone()),
            ROTATED,
            BlockId(72)
        ),
        Err(MemberError::Missing(BlockId(64)))
    );
    // A previous cycle's work list withheld.
    let no_list = Source {
        withheld_lists: BTreeSet::from([40]),
        ..Source::default()
    };
    assert_eq!(
        compute_by_quarter_rotation(
            &full.ctx(),
            &no_list,
            &mut Rows(held.clone()),
            ROTATED,
            BlockId(72)
        ),
        Err(MemberError::Missing(BlockId(40)))
    );
    // Core's values: an absent row breaks the walk (cycle 48 finds no row at 24), and a previous
    // cycle at a negative height breaks it with no view call. Reaching one needs a row at
    // genesis, whose work block is below genesis: Core dereferences null there, here it is
    // `Missing(genesis)` (LQ1b D17).
    let ctx = full.ctx();
    let quorums =
        compute_by_quarter_rotation(&ctx, &src, &mut Rows::default(), ROTATED, BlockId(48))
            .expect("held");
    assert_eq!(quorums.iter().map(|q| q.len()).collect::<Vec<_>>(), vec![1, 1]);
    let genesis_row = ModelRows::from([((ROTATED.to_base(), hash(0)), snapshot(&[], 0, &[]))]);
    assert_eq!(
        compute_by_quarter_rotation(&ctx, &src, &mut Rows(genesis_row), ROTATED, BlockId(24)),
        Err(MemberError::Missing(BlockId(0)))
    );
    // Control: held throughout, cycle 72 reads both previous rows.
    let quorums = compute_by_quarter_rotation(&ctx, &src, &mut Rows(held), ROTATED, BlockId(72))
        .expect("held");
    assert_eq!(quorums.iter().map(|q| q.len()).collect::<Vec<_>>(), vec![3, 3]);
}

// ---- LQ1b: precompute, the predictor and the uncached composition ---------------------------

#[test]
fn precompute_computes_rotated_cycle_bases_only() {
    let f = rotated_fixture();
    let ctx = f.ctx();
    let src = Source::default();
    let mut facade = ModelMembersFacade::new(&f.params);
    let mut store = ModelRows::new();
    for block in 1..=TIP {
        let mut rows = reference::RuntimeRows::new(&mut store);
        facade.precompute(&ctx, &src, &mut rows, BlockId(block), PrecomputeMode::Connect).unwrap();
    }
    let written: Vec<BlockHash> = store.keys().map(|(_, h)| *h).collect();
    assert_eq!(written, [24, 48, 72, 96, 120].map(hash).to_vec());
    // Undo at a cycle base resets the type's caches and recomputes over the current rows; at any
    // other height it touches nothing.
    let before = facade.clone();
    let mut rows = reference::RuntimeRows::new(&mut store);
    facade.precompute(&ctx, &src, &mut rows, BlockId(97), PrecomputeMode::Undo).unwrap();
    assert_eq!(facade, before);
    let mut rows = reference::RuntimeRows::new(&mut store);
    facade.precompute(&ctx, &src, &mut rows, BlockId(96), PrecomputeMode::Undo).unwrap();
    assert_eq!(facade.indexed_cache().cache(ROTATED).map(|c| c.len()), Some(2));
    assert_eq!(facade.base_cache().cache(ROTATED).map(|c| c.len()), Some(1));
}

#[test]
fn the_predictor_matches_the_rotation_and_stores_nothing() {
    let f = Fixture::new(40, 2, 0);
    let ctx = f.ctx();
    let src = Source::default();
    let mut rows = Rows::default();
    for cycle in [24, 48, 72] {
        compute_by_quarter_rotation(&ctx, &src, &mut rows, ROTATED, BlockId(cycle)).unwrap();
    }
    let before = rows.0.clone();
    let mut computed = Rows(rows.0.clone());
    let cycle =
        compute_by_quarter_rotation(&ctx, &src, &mut computed, ROTATED, BlockId(96)).unwrap();
    for (height, index) in [(96, 0), (97, 1)] {
        let predicted =
            compute_quorum_members_from_work_block(&ctx, &src, &rows, ROTATED, BlockId(88), height);
        assert_eq!(predicted, Ok(Some(cycle[index].clone())), "{height}");
    }
    assert_eq!(rows.0, before, "the predictor stores no row");
    // Non-rotated: the work block's list with the work block's modifier.
    let predicted =
        compute_quorum_members_from_work_block(&ctx, &src, &rows, TEST, BlockId(88), 96).unwrap();
    assert_eq!(predicted, Some(compute_quorum_members(&ctx, &src, TEST, BlockId(96)).unwrap()));
    // Core's nullopts.
    for (t, work, height, why) in [
        (ROTATED, 88, 98, "index out of range"),
        (TEST, 88, 97, "a non-rotated index above 0"),
        (ROTATED, 87, 96, "not the work block"),
        (ROTATED, 88, -96, "a negative height"),
        (LlmqType::Llmq400_60, 88, 96, "an unconfigured type"),
        (ROTATED, 64, 72, "cycle 72 reads cycle 0's absent row"),
    ] {
        let predicted =
            compute_quorum_members_from_work_block(&ctx, &src, &rows, t, BlockId(work), height);
        assert_eq!(predicted, Ok(None), "{why}");
    }
    let pre_v20 = rotated_fixture();
    let predicted = compute_quorum_members_from_work_block(
        &pre_v20.ctx(),
        &src,
        &rows,
        ROTATED,
        BlockId(88),
        96,
    );
    assert_eq!(predicted, Ok(None), "before V20");
}

#[test]
fn rotated_members_uncached_equals_the_cold_facade_and_leaves_state_untouched() {
    let mut warm = 0;
    for v20 in [40, NEVER] {
        let f = Fixture::new(v20, 2, 0);
        let ctx = f.ctx();
        let src = Source {
            chainlocks: BTreeSet::from([16, 40, 64, 88]),
            ..Source::default()
        };
        // A warm model: precomputed to the tip, then a few calls.
        let mut model = ModelAttempt::new(&f.params);
        for block in 1..=TIP {
            let id = model.begin().unwrap();
            model
                .attempt_precompute(id, &ctx, &src, BlockId(block), PrecomputeMode::Connect)
                .unwrap();
            model.release(id, &mut |_| false).unwrap();
        }
        for base in [24, 25, 26, 48, 49, 72, 73, 96, 97, 120, 121] {
            let (facade_before, rows_before) = (model.facade().clone(), model.rows().clone());
            let mut shadow = Rows(rows_before.clone());
            let uncached = members_uncached(&ctx, &src, &mut shadow, ROTATED, BlockId(base));
            assert_eq!(
                (model.facade(), model.rows()),
                (&facade_before, &rows_before),
                "untouched at {base}"
            );
            // A cold facade over the same rows computes the same members and the same row bytes.
            let mut cold = ModelMembersFacade::new(&f.params);
            let mut cold_rows = Rows(rows_before.clone());
            let facade = cold.members(&ctx, &src, &mut cold_rows, ROTATED, BlockId(base), false);
            assert_eq!(uncached, facade.map(|(m, _)| m), "V20 {v20}, base {base}");
            assert_eq!(shadow.0, cold_rows.0, "V20 {v20}, base {base}");
            // And it agrees with what the warm cache published (a miss computes what a hit holds).
            if let Ok(Some(cached)) = model.facade().peek(&ctx, ROTATED, BlockId(base)) {
                assert_eq!(uncached, Ok(cached), "V20 {v20}, base {base}");
                warm += 1;
            }
        }
    }
    assert!(warm >= 14, "non-vacuous: {warm} warm-cache comparisons");
}
