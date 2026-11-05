//! LQ1a's quorum member selection (`dash_evo::llmq`) against Core v24's LLMQ contexts (O2a-LQ1,
//! ABI 11): the oracle targets of LQ1 §6.2, the non-rotated route (LQ1a) and DIP0024's rotation
//! (LQ1b).
//!
//! Every input builds one regtest scenario ([`Scenario::read`]) that both sides load: a chain of
//! ground headers from genesis to a tip, the activation heights, a masternode list at every block
//! drawn from a small pool (so the work block and the base read different lists), and every
//! block's coinbase. Core's coinbases carry no ChainLock, so post-V20 modifiers take the hash
//! form here; the ChainLock form is compared over the recorded chains (`l1_llmq`).
//!
//! Input layouts:
//! - `diff_quorum_members`: the scenario, then up to [`MAX_CALLS`] calls of three bytes (type,
//!   base height, reset flag), each `GetAllQuorumMembers(type, base, reset_cache)` against
//!   `dash_evo::llmq::reference::ModelMembersFacade` over the same steps. Compared per call: the
//!   refusal class, Core's trace outcome, and the members (proTxHash and the stored masternode,
//!   in order), Core's trace (each snapshot row by its key and bytes), and after the call the
//!   digest of every `llmq_S` row against the model's rows. The calls share each side's caches
//!   and rows, so hits, resets, evictions and a cycle reading rows earlier calls stored are
//!   compared too.
//! - `diff_type_enabled`: the scenario, then up to [`MAX_CALLS`] queries of three bytes (see
//!   [`compare_type_enabled`]). Compared: the answer, or both refusing.
//! - `diff_scan_quorums` and `diff_select_quorum`: the scenario, then mined commitments
//!   ([`MinedSpec::read`]), then up to [`MAX_CALLS`] calls of six bytes ([`QuorumCall`]):
//!   `ScanQuorums` (either overload), members calls and restarts, or `SelectQuorumForSigning`,
//!   scans and restarts, against `dash_evo::llmq::reference::ModelQuorumManager`. Compared per
//!   call: the refusal class, the result (base hashes in order), Core's trace (rows, members,
//!   `GetQuorum` and scan events, field by field), and the rows digest. A restart keeps both
//!   sides' rows (evodb's).
//!
//! - `diff_quorum_members_rot`: a rotated scenario ([`Scenario::read_rot`]) to [`MAX_ROT_TIP`],
//!   then up to [`MAX_ROT_CALLS`] calls ([`RotCall`]): members calls, precomputes, restarts and
//!   seeded rows, compared as `diff_quorum_members` is (see [`compare_quorum_members_rot`]).
//!
//! The scenario layouts are [`Scenario::read`]'s and [`Scenario::read_rot`]'s.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock};

use dash_consensus::ctx::{Deployment, DeploymentView};
use dash_consensus::hash::sha256d;
use dash_consensus::primitives::write_compact_size;
use dash_consensus::{BlockId, ChainAncestry, HeaderCtx, decode_header, decode_tx_exact};
use dash_dml::testkit::{dmn_bytes, fresh, list_disk_bytes, numbered, numbered_operator_ikm};
use dash_dml::{Dmn, MnHot, MnList};
use dash_evo::MinedCommitmentReader;
use dash_evo::bls_ctx_after;
use dash_evo::cbtx::CoinbaseFacts;
use dash_evo::llmq::reference::{
    GetQuorumOutcome, MembersOutcome, ModelEvent, ModelMembersFacade, ModelQuorumManager,
    ModelRows, ModelViews, RuntimeRows,
};
use dash_evo::llmq::{
    BrokenInvariant, LlmqChain, LlmqCommitment, LlmqCtx, LlmqError, LlmqSource, MemberError,
    MinedCommitment, MinedFamily, MinedIndexEntry, PrecomputeMode, QuorumSnapshot, ScanCacheUse,
    SnapshotRows, TypeEnabledOverrides, is_quorum_rotation_enabled, is_quorum_type_enabled_with,
};
use dash_num::{Arith256, CompactTarget};
use dash_oracle_sys::{Chain, ErrorKind, LlmqContext, Member, Oracle, Pin, Precompute, TraceEvent};
use dash_params::LlmqType;
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, Network, Release};
use dash_pkc::bls::{BlsPublicKey, BlsScIetf, BlsSecretKey, BlsSigId, BlsSignature};
use dash_primitives::{BlockHash, QuorumHash};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;

use crate::{Compared, Divergence, Outcome, check, divergence};

/// The highest tip a scenario builds.
pub const MAX_TIP: u32 = 120;
/// The highest tip a rotated scenario builds: LLMQ_TEST_DIP0024's cycles to 264.
pub const MAX_ROT_TIP: u32 = 24 + 255;
/// Calls per input.
pub const MAX_CALLS: usize = 12;
/// Pooled masternodes per scenario.
pub const MAX_POOL: usize = 15;
/// Mined commitments per scenario.
pub const MAX_MINED: usize = 12;
/// Pooled masternodes per rotated scenario.
pub const MAX_ROT_POOL: usize = 24;
/// Calls per rotated input.
pub const MAX_ROT_CALLS: usize = 24;

/// The types a call picks from: regtest's five, then three Core refuses (two configured on other
/// chains only, so its `GetLLMQ` assertion, and one it does not know, so its throw).
pub const TYPES: [u8; 8] = [100, 102, 103, 104, 106, 1, 101, 0xfe];

/// The regtest headers `0 ..= MAX_ROT_TIP`, each on the previous one, with regtest proof of work (a
/// context's block reads check it): the same for every scenario.
pub fn headers() -> &'static [([u8; 80], BlockHash)] {
    static HEADERS: OnceLock<Vec<([u8; 80], BlockHash)>> = OnceLock::new();
    HEADERS.get_or_init(|| {
        let mut prev = [0u8; 32];
        (0..=MAX_ROT_TIP)
            .map(|height| {
                let (header, hash) = ground_header(prev, height);
                prev = *hash.as_bytes();
                (header, hash)
            })
            .collect()
    })
}

/// A header's hash.
pub fn header_hash(header: &[u8; 80]) -> Option<BlockHash> {
    let mut cursor = &header[..];
    Some(decode_header(&mut cursor).ok()?.value().hash())
}

/// The headers' version and bits (regtest's proof-of-work limit).
const VERSION: i32 = 0x2000_0000;
const BITS: u32 = 0x207f_ffff;

/// A harness header's context: its height and hash, the headers' version and bits.
fn header_ctx(height: u32, hash: BlockHash) -> HeaderCtx {
    HeaderCtx {
        height,
        hash,
        time: 0,
        bits: CompactTarget::new(BITS),
        version: VERSION,
        mtp: 0,
        chain_work: Arith256::ZERO,
    }
}

fn ground_header(prev: [u8; 32], height: u32) -> ([u8; 80], BlockHash) {
    let mut h = [0u8; 80];
    h[..4].copy_from_slice(&VERSION.to_le_bytes());
    h[4..36].copy_from_slice(&prev);
    h[36..40].copy_from_slice(&height.to_le_bytes());
    h[68..72].copy_from_slice(&(1_417_713_337u32 + 150 * height).to_le_bytes());
    h[72..76].copy_from_slice(&BITS.to_le_bytes());
    for nonce in 0u32.. {
        h[76..].copy_from_slice(&nonce.to_le_bytes());
        // 0x207fffff: the hash, as a big-endian number, below 0x80 00 .. 00.
        if let Some(hash) = header_hash(&h).filter(|hash| hash.as_bytes()[31] < 0x80) {
            return (h, hash);
        }
    }
    unreachable!("half of all hashes meet the regtest target")
}

/// The harness's view errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewError {
    /// A block the harness's chain does not hold.
    NotHeld(u32),
    /// The mined-commitment reader's broken ancestry invariant.
    Broken(BrokenInvariant),
}

impl From<BrokenInvariant> for ViewError {
    fn from(e: BrokenInvariant) -> Self {
        ViewError::Broken(e)
    }
}

/// A linear chain holding heights `first ..`; `BlockId(h)` is the block at height `h`. Reads of
/// a block below `first` or above the tip are [`ViewError::NotHeld`], never Core's null (EV addendum §2.2).
/// [`HeldChain::indexed_from`] extends the ancestry (not the headers) below `first`.
pub struct HeldChain {
    first: u32,
    indexed_from: u32,
    hashes: Vec<BlockHash>,
}

impl HeldChain {
    /// The chain of `hashes`, the first at height `first`.
    pub fn new(first: u32, hashes: impl IntoIterator<Item = BlockHash>) -> Self {
        HeldChain {
            first,
            indexed_from: first,
            hashes: hashes.into_iter().collect(),
        }
    }

    /// The chain with its block index (ancestry, heights) from `height`, its headers still from
    /// `first`: a recorded sample's blocks below its headers, whose hashes it lacks.
    pub fn indexed_from(self, height: u32) -> Self {
        HeldChain {
            indexed_from: height.min(self.first),
            ..self
        }
    }

    /// The block at `height`.
    pub fn at(&self, height: u32) -> BlockId {
        BlockId(height)
    }

    /// The tip's height.
    pub fn tip(&self) -> u32 {
        self.first + u32::try_from(self.hashes.len()).unwrap_or(u32::MAX).saturating_sub(1)
    }

    /// The hash of the block at `height`, if held.
    pub fn hash(&self, height: u32) -> Option<BlockHash> {
        self.hashes.get(usize::try_from(height.checked_sub(self.first)?).ok()?).copied()
    }

    fn held(&self, id: BlockId) -> Result<BlockId, ViewError> {
        if (self.indexed_from..=self.tip()).contains(&id.0) {
            Ok(id)
        } else {
            Err(ViewError::NotHeld(id.0))
        }
    }
}

impl LlmqChain for HeldChain {
    type Error = ViewError;

    fn header(&self, id: BlockId) -> Result<HeaderCtx, ViewError> {
        Ok(header_ctx(id.0, self.hash(id.0).ok_or(ViewError::NotHeld(id.0))?))
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Result<Option<BlockId>, ViewError> {
        self.held(id)?;
        if height > id.0 {
            return Ok(None);
        }
        self.held(BlockId(height)).map(Some)
    }

    fn prev(&self, id: BlockId) -> Result<Option<BlockId>, ViewError> {
        self.held(id)?;
        match id.0.checked_sub(1) {
            None => Ok(None),
            Some(h) => self.held(BlockId(h)).map(Some),
        }
    }
}

/// Buried deployments at their heights on a chain whose `BlockId` is the height; no versionbits
/// deployment is active (no header signals one).
pub struct HeightDeployments<'a> {
    pub params: &'a ChainSetup,
}

impl DeploymentView for HeightDeployments<'_> {
    fn active_after(&self, prev: Option<BlockId>, deployment: Deployment) -> bool {
        let next = prev.map_or(0, |p| p.0 + 1);
        match deployment {
            Deployment::Buried(d) => next >= self.params.consensus.deployment_height(d).to_u32(),
            Deployment::Versionbits(_) => false,
        }
    }

    fn active_at(&self, block: BlockId, deployment: Deployment) -> bool {
        self.active_after(block.0.checked_sub(1).map(BlockId), deployment)
    }
}

/// Lists and coinbase facts by height, as a node's evo view holds them; a height absent from a
/// map is not held.
#[derive(Default)]
pub struct MapSource {
    /// Lists by height.
    pub lists: HashMap<u32, Arc<MnList>>,
    /// The facts `GetNonNullCoinbaseChainlock` reads, by height.
    pub coinbases: HashMap<u32, CoinbaseFacts>,
}

impl LlmqSource<MnList> for MapSource {
    type Error = ViewError;

    fn mn_list(&self, at: BlockId) -> Result<Option<Arc<MnList>>, ViewError> {
        Ok(self.lists.get(&at.0).cloned())
    }

    fn coinbase_facts(&self, at: BlockId) -> Result<Option<CoinbaseFacts>, ViewError> {
        Ok(self.coinbases.get(&at.0).cloned())
    }
}

/// No snapshot rows: the non-rotated route reads and writes none.
pub struct NoRows;

impl SnapshotRows for NoRows {
    type Error = ViewError;

    fn get(&self, _: LlmqType, _: &BlockHash) -> Result<Option<QuorumSnapshot>, ViewError> {
        Ok(None)
    }

    fn store(&mut self, _: LlmqType, _: &BlockHash, _: &QuorumSnapshot) -> Result<(), ViewError> {
        Ok(())
    }
}

/// A snapshot row as Core serializes it (`CQuorumSnapshot`): the mode (`int`), the bitset (its
/// bit count, then bit `i` in byte `i / 8`), the skip list (`int`s).
// Core: v24 llmq/snapshot.h:61 CQuorumSnapshot::Serialize
pub fn snapshot_bytes(snapshot: &QuorumSnapshot) -> Vec<u8> {
    let active = &snapshot.active_quorum_members;
    let mut out = snapshot.skip_list_mode.to_le_bytes().to_vec();
    write_compact_size(active.len() as u64, &mut out);
    out.extend(
        active.chunks(8).map(|c| c.iter().rev().fold(0u8, |byte, &bit| byte << 1 | u8::from(bit))),
    );
    write_compact_size(snapshot.skip_list.len() as u64, &mut out);
    for skip in &snapshot.skip_list {
        out.extend(skip.to_le_bytes());
    }
    out
}

/// The evodb key of `(llmq_type, cycle_base)`'s row: `("llmq_S", SerializeHash(type, hash))`
/// serialized.
// Core: v24 llmq/snapshot.cpp:25 DB_QUORUM_SNAPSHOT, :321 StoreSnapshotForBlock
pub fn snapshot_key(
    llmq_type: u8,
    cycle_base: &BlockHash,
) -> [u8; dash_oracle_sys::SNAPSHOT_KEY_LEN] {
    let mut preimage = vec![llmq_type];
    preimage.extend(cycle_base.as_bytes());
    let mut key = [0u8; dash_oracle_sys::SNAPSHOT_KEY_LEN];
    key[0] = 6;
    key[1..7].copy_from_slice(b"llmq_S");
    key[7..].copy_from_slice(&sha256d(&preimage));
    key
}

/// The Rust twin of `oracle_llmq_rows_digest` over the model's rows: SHA-256 of the row count,
/// then each row in key order as its key and its bytes, each with its CompactSize length.
pub fn rows_digest(rows: &ModelRows) -> [u8; 32] {
    let keyed: BTreeMap<_, _> = rows
        .iter()
        .map(|((llmq_type, cycle_base), row)| (snapshot_key(*llmq_type, cycle_base), row))
        .collect();
    let mut preimage = Vec::new();
    write_compact_size(keyed.len() as u64, &mut preimage);
    for (key, row) in keyed {
        let row = snapshot_bytes(row);
        write_compact_size(key.len() as u64, &mut preimage);
        preimage.extend(key);
        write_compact_size(row.len() as u64, &mut preimage);
        preimage.extend(row);
    }
    bitcoin_hashes::sha256::Hash::hash(&preimage).to_byte_array()
}

/// Core's rows against the model's, by digest.
fn compare_rows(core: &mut LlmqContext, rows: &ModelRows) -> Result<(), Divergence> {
    let theirs = core.rows_digest().map_err(|_| divergence("rows digest"))?;
    check(theirs == rows_digest(rows), "snapshot rows")
}

/// A version-1 coinbase: the null prevout, scriptSig OP_1 OP_0, one OP_TRUE output of 0.
pub fn plain_coinbase() -> Vec<u8> {
    let mut tx = vec![1, 0, 0, 0, 1];
    tx.extend([0; 32]);
    tx.extend([0xff; 4]);
    tx.extend([2, 0x51, 0x00]);
    tx.extend([0xff; 4]);
    tx.push(1);
    tx.extend(0u64.to_le_bytes());
    tx.extend([1, 0x51]);
    tx.extend([0; 4]);
    tx
}

/// A block's coinbase facts as the oracle's `set_coinbase(coinbase, have_data)` gives Core them:
/// its first transaction, or, without `have_data`, a block Core cannot read. `None` when
/// `coinbase` is not one transaction.
pub fn coinbase_facts(coinbase: &[u8], have_data: bool) -> Option<CoinbaseFacts> {
    if !have_data {
        return Some(CoinbaseFacts::BlockDataUnavailable);
    }
    let tx = decode_tx_exact(coinbase).ok()?.into_value();
    Some(CoinbaseFacts::First(Arc::new(tx)))
}

/// One input's regtest scenario.
pub struct Scenario {
    /// The chain options, shared by both sides.
    pub options: Vec<(ChainOption, String)>,
    /// The tip's height.
    pub tip: u32,
    /// Whether Core can read each block's coinbase.
    pub have_data: bool,
    /// The pooled masternodes.
    pub pool: Vec<Pooled>,
}

/// A pooled masternode and the heights whose lists hold it.
pub struct Pooled {
    pub dmn: Arc<Dmn>,
    /// The height mod 3 at which it is absent (3: never).
    pub phase: u8,
    /// The first height it is in a list (its registration).
    pub from: u32,
    /// The first height it is gone from the lists again, if any.
    pub until: Option<u32>,
    /// From a height on, the lists hold this state of it instead.
    pub changed: Option<(u32, Arc<Dmn>)>,
}

impl Pooled {
    /// Its state in the list at `height`.
    fn at(&self, height: u32) -> &Arc<Dmn> {
        match &self.changed {
            Some((from, changed)) if height >= *from => changed,
            _ => &self.dmn,
        }
    }
}

/// `dmn` changed as [`Scenario::read_rot`]'s change byte says (`kind`): PoSe-banned at `at`, its
/// service moved to a fresh one or to `first`'s, or a fresh operator key.
fn changed_dmn(dmn: &Arc<Dmn>, id: u8, kind: u8, at: u32, first: Option<&Arc<Dmn>>) -> Arc<Dmn> {
    let mut changed = (**dmn).clone();
    let fresh_state = numbered(256 + u32::from(id));
    let mut cold = (*changed.cold).clone();
    match kind {
        0 => changed.hot.ban_if_not_banned(at as i32),
        1 => cold.net_info = Arc::clone(&fresh_state.cold.net_info),
        2 => cold.net_info = Arc::clone(&first.unwrap_or(&fresh_state).cold.net_info),
        _ => cold.pub_key_operator = fresh_state.cold.pub_key_operator.clone(),
    }
    changed.cold = Arc::new(fresh(cold));
    Arc::new(changed)
}

/// Masternode `numbered(id)` registered at `from`, as `flags` (bits 2-4) of [`Scenario::read`]
/// make it, and with `reuse` bits 5-7 of [`Scenario::read_rot`]'s too, copied from `first`.
fn pooled_dmn(id: u8, flags: u8, from: u32, first: Option<&Arc<Dmn>>, reuse: bool) -> Arc<Dmn> {
    let mut dmn = (*numbered(u32::from(id))).clone();
    // Never paid: every height a list holds is at or below the list's.
    dmn.hot = MnHot::default();
    if flags & 0x04 != 0 {
        dmn.hot.ban_if_not_banned(1);
    }
    let mut cold = (*dmn.cold).clone();
    cold.registered_height = from as i32;
    if flags & 0x08 != 0 {
        cold.confirmed_hash = Default::default();
    }
    if let Some(first) = first {
        if flags & 0x10 != 0 {
            cold.confirmed_hash_with_pro_tx = first.cold.confirmed_hash_with_pro_tx;
        }
        if reuse && flags & 0x20 != 0 {
            cold.net_info = Arc::clone(&first.cold.net_info);
        }
        if reuse && flags & 0x40 != 0 {
            cold.pub_key_operator = first.cold.pub_key_operator.clone();
        }
        if reuse && flags & 0x80 != 0 {
            cold.key_id_owner = first.cold.key_id_owner;
        }
    }
    dmn.cold = Arc::new(fresh(cold));
    Arc::new(dmn)
}

impl Scenario {
    /// Reads a scenario from the front of `input`.
    ///
    /// - Byte 0: V20 (and MN_RR) at `[2, 40, 70, 1000][b & 3]`, V19 at `[2, 30, 60, 1000][b >> 2 & 3]`,
    ///   DIP0024 at `[2, 30, 60, 1000][b >> 4 & 3]`; bit 6 `-llmqtestparams=5:3`.
    /// - Byte 1: the tip, `24 + b % (MAX_TIP - 23)`.
    /// - Byte 2: bits 0-3 the pool size (up to [`MAX_POOL`]); bit 7 Core can read the coinbases.
    /// - Two bytes per pooled masternode: `numbered(id)` registered at height 1, then flags: bits 0-1 the height mod 3
    ///   at which it is absent, bit 2 banned, bit 3 unconfirmed, bit 4 the stored
    ///   `confirmedHashWithProRegTxHash` of the pool's first masternode (equal scores, so the
    ///   collateral decides).
    pub fn read(input: &mut &[u8]) -> Option<Scenario> {
        let (&[config, tip, pool_byte], rest) = input.split_first_chunk::<3>()?;
        *input = rest;
        let height = |table: [u32; 4], shift: u32| table[usize::from(config >> shift & 3)];
        let v20 = height([2, 40, 70, 1000], 0);
        // Core asserts MN_RR at or above V20 (and V20 at or above DIP0003).
        let mut options = vec![
            (ChainOption::Dip3Params, String::from("2:2")),
            (ChainOption::TestActivationHeight, format!("v20@{v20}")),
            (ChainOption::TestActivationHeight, format!("mn_rr@{v20}")),
            (ChainOption::TestActivationHeight, format!("v19@{}", height([2, 30, 60, 1000], 2))),
            (
                ChainOption::TestActivationHeight,
                format!("dip0024@{}", height([2, 30, 60, 1000], 4)),
            ),
        ];
        if config & 0x40 != 0 {
            options.push((ChainOption::LlmqTestParams, String::from("5:3")));
        }
        let mut pool: Vec<Pooled> = Vec::new();
        for _ in 0..usize::from(pool_byte & 0x0f).min(MAX_POOL) {
            let Some((&[id, flags], rest)) = input.split_first_chunk::<2>() else {
                break;
            };
            *input = rest;
            let first = pool.first().map(|p| &p.dmn);
            pool.push(Pooled {
                dmn: pooled_dmn(id, flags, 1, first, false),
                phase: flags & 3,
                from: 1,
                until: None,
                changed: None,
            });
        }
        Some(Scenario {
            options,
            tip: 24 + u32::from(tip) % (MAX_TIP - 23),
            have_data: pool_byte & 0x80 != 0,
            pool,
        })
    }

    /// Reads a rotated scenario ([`compare_quorum_members_rot`]) from the front of `input`.
    ///
    /// - Bytes 0-2: V20 (and MN_RR), V19 and DIP0024 at `2 + b`, 0xff never: any alignment to
    ///   LLMQ_TEST_DIP0024's cycles of 24.
    /// - Byte 3: the tip, `24 + b` (to [`MAX_ROT_TIP`]).
    /// - Byte 4: bits 0-4 the pool size (up to [`MAX_ROT_POOL`]); bit 5 `-llmqtestparams=5:3`;
    ///   bit 7 Core can read the coinbases.
    /// - Four bytes per pooled masternode: `numbered(id)`; flags as [`Scenario::read`]'s, and
    ///   bits 5-7 the pool's first masternode's service, operator key and owner key (a key
    ///   reuse: the later one is refused while the first is listed); its life: registered at
    ///   `16 * (b & 15)` (at least 1), gone `24 * (b >> 4)` blocks later (0: never); and a state
    ///   change from height `8 * (b & 63)` (0: none) on, by `b >> 6`: PoSe-banned there, its
    ///   service moved to a fresh one, its service moved to the first masternode's, or a fresh
    ///   operator key (so a previous cycle's member can collide with a later one).
    pub fn read_rot(input: &mut &[u8]) -> Option<Scenario> {
        let (&[v20, v19, dip24, tip, pool_byte], rest) = input.split_first_chunk::<5>()?;
        *input = rest;
        let height = |b: u8| {
            if b == 0xff {
                1000
            } else {
                2 + u32::from(b)
            }
        };
        let v20 = height(v20);
        let mut options = vec![
            (ChainOption::Dip3Params, String::from("2:2")),
            (ChainOption::TestActivationHeight, format!("v20@{v20}")),
            (ChainOption::TestActivationHeight, format!("mn_rr@{v20}")),
            (ChainOption::TestActivationHeight, format!("v19@{}", height(v19))),
            (ChainOption::TestActivationHeight, format!("dip0024@{}", height(dip24))),
        ];
        if pool_byte & 0x20 != 0 {
            options.push((ChainOption::LlmqTestParams, String::from("5:3")));
        }
        let mut pool: Vec<Pooled> = Vec::new();
        for _ in 0..usize::from(pool_byte & 0x1f).min(MAX_ROT_POOL) {
            let Some((&[id, flags, life, change], rest)) = input.split_first_chunk::<4>() else {
                break;
            };
            *input = rest;
            let from = (16 * u32::from(life & 0x0f)).max(1);
            let first = pool.first().map(|p| &p.dmn);
            let dmn = pooled_dmn(id, flags, from, first, true);
            let at = 8 * u32::from(change & 0x3f);
            let changed = (at != 0).then(|| (at, changed_dmn(&dmn, id, change >> 6, at, first)));
            pool.push(Pooled {
                dmn,
                phase: flags & 3,
                from,
                until: (life >> 4 != 0).then(|| from + 24 * u32::from(life >> 4)),
                changed,
            });
        }
        Some(Scenario {
            options,
            tip: 24 + u32::from(tip),
            have_data: pool_byte & 0x80 != 0,
            pool,
        })
    }

    /// The chain parameters both sides use.
    pub fn params(&self) -> Option<ChainSetup> {
        self.options
            .iter()
            .fold(ChainParamsBuilder::new(Network::Regtest, Release::V24_0), |b, (o, v)| {
                b.arg(*o, v.clone())
            })
            .build()
            .ok()
    }

    /// The oracle's chain.
    pub fn chain(&self) -> Chain {
        let args: Vec<String> =
            self.options.iter().map(|(o, v)| format!("{}={v}", o.name())).collect();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        Chain::new("regtest", &args)
    }

    /// The list after the block at `height`: each pooled masternode registered at or below
    /// `height`, not gone and not absent at `height`, that the list accepts, in pool order (a
    /// repeated pool entry or a reused key is refused, as `AddMN` refuses it).
    pub fn list_at(&self, height: u32, hash: BlockHash) -> MnList {
        let mut list = MnList::new(hash, height as i32, 0);
        for p in &self.pool {
            let live = p.from <= height && p.until.is_none_or(|until| height < until);
            if live && u32::from(p.phase) != height % 3 {
                let _ = list.try_add(Arc::clone(p.at(height)), true);
            }
        }
        list
    }

    /// The node's view of the scenario, and Core's context loaded with it. `Err` is the
    /// comparison's result when Core refuses the scenario ([`load_refusal`]).
    pub fn load(&self, oracle: &Oracle) -> Result<(HeldChain, MapSource, LlmqContext), Compared> {
        let headers = &headers()[..=self.tip as usize];
        let chain = HeldChain::new(0, headers.iter().map(|(_, hash)| *hash));
        let mut source = MapSource::default();
        let mut ctx = oracle.llmq_context(&self.chain()).map_err(load_refusal)?;
        let coinbase = plain_coinbase();
        let facts =
            coinbase_facts(&coinbase, self.have_data).ok_or(Err(divergence("harness coinbase")))?;
        for (height, (header, hash)) in (0u32..).zip(headers) {
            let list = self.list_at(height, *hash);
            let seeded = ctx
                .add_header(header, height as i32)
                .and_then(|()| ctx.set_mn_list(hash.as_bytes(), &list_disk_bytes(&list)))
                .and_then(|()| ctx.set_coinbase(hash.as_bytes(), &coinbase, self.have_data));
            seeded.map_err(load_refusal)?;
            source.lists.insert(height, Arc::new(list));
            source.coinbases.insert(height, facts.clone());
        }
        Ok((chain, source, ctx))
    }
}

/// The harness's own chain view failed a read: never a refusal.
const HARNESS_VIEW: Divergence = divergence("harness view");

/// Core's refusal of a scenario: outside its domain for an argument or context refusal; any other
/// failure (a context the thread already holds, a poisoned pin) is the harness's.
fn load_refusal(e: dash_oracle_sys::Error) -> Compared {
    match e.kind {
        ErrorKind::Argument | ErrorKind::Context => Ok(Outcome::OutOfDomain),
        _ => Err(divergence("oracle load")),
    }
}

/// [`Scenario::read`]'s config byte: V20, V19 and DIP0024 at index 0 (height 2) .. 3 (never) of
/// their tables, and LLMQ_TEST of 5.
pub fn config_byte(v20: u8, v19: u8, dip24: u8, size5: bool) -> u8 {
    v20 | v19 << 2 | dip24 << 4 | u8::from(size5) << 6
}

/// [`Scenario::read`]'s bytes for a tip of `24 ..= MAX_TIP` and a pool of `(id, flags)`.
pub fn scenario_bytes(config: u8, tip: u32, have_data: bool, pool: &[(u8, u8)]) -> Vec<u8> {
    let mut input = vec![config, (tip - 24) as u8, pool.len() as u8 | u8::from(have_data) << 7];
    for (id, flags) in pool {
        input.extend([*id, *flags]);
    }
    input
}

/// Nine always-present masternodes.
pub fn nine() -> Vec<(u8, u8)> {
    (1..=9).map(|i| (i, 3)).collect()
}

/// Nine masternodes, each absent at the heights `≡ i (mod 3)`: lists change every block.
pub fn nine_phased() -> Vec<(u8, u8)> {
    (1..=9).map(|i| (i, i % 3)).collect()
}

/// A scenario's parameters, and both sides loaded with it ([`open`]).
pub struct Loaded {
    pub params: ChainSetup,
    pub chain: HeldChain,
    pub source: MapSource,
    pub core: LlmqContext,
}

/// Reads a scenario from the front of `input` and loads both sides with it; `Err` is the
/// comparison's result when that fails: out of domain when both sides refuse the scenario, a
/// divergence when one does.
pub fn open(input: &mut &[u8], pin: Pin) -> Result<Loaded, Compared> {
    open_with(input, pin, Scenario::read)
}

/// [`open`] with the scenario `read` reads.
pub fn open_with(
    input: &mut &[u8],
    pin: Pin,
    read: fn(&mut &[u8]) -> Option<Scenario>,
) -> Result<Loaded, Compared> {
    let scenario = read(input).ok_or(Ok(Outcome::OutOfDomain))?;
    let loaded = scenario.load(&Oracle::new(pin));
    match (scenario.params(), loaded) {
        (Some(params), Ok((chain, source, core))) => Ok(Loaded {
            params,
            chain,
            source,
            core,
        }),
        (None, Err(Ok(Outcome::OutOfDomain))) => Err(Ok(Outcome::OutOfDomain)),
        (_, Err(Err(e))) => Err(Err(e)),
        _ => Err(Err(divergence("scenario refusal"))),
    }
}

/// What a comparison's calls came to: [`Outcome::BothAccept`] when both sides answered a call
/// alike; else, for a call both refused, [`Outcome::BothReject`] when Core threw and
/// [`Outcome::OutOfDomain`] when it refused the arguments; else nothing was compared.
#[derive(Default)]
struct Tally {
    accepted: bool,
    refused: Option<ErrorKind>,
}

impl Tally {
    /// Both sides refused a call, Core as `expected` ([`core_refusal`]). The caller stops there:
    /// Core refuses its arguments before it touches a cache (and a throw poisons its context),
    /// where the model may already have changed one.
    fn refused(
        &mut self,
        e: &dash_oracle_sys::Error,
        expected: ErrorKind,
    ) -> Result<(), Divergence> {
        check(e.kind == expected, "refusal")?;
        self.refused = Some(e.kind);
        Ok(())
    }

    fn outcome(&self) -> Compared {
        Ok(if self.accepted {
            Outcome::BothAccept
        } else if self.refused == Some(ErrorKind::Exception) {
            Outcome::BothReject
        } else {
            Outcome::OutOfDomain
        })
    }
}

/// How Core refuses a call the node refused with `e`: a type its `IsQuorumTypeEnabled` does not
/// know throws; a known one that is enabled but not configured (Core asserts), and a work block
/// below genesis, are the oracle's argument refusals.
fn core_refusal<E>(ctx: &LlmqCtx<'_, E>, chain: &HeldChain, e: &LlmqError<E>) -> ErrorKind {
    let unknown = |t: u8| {
        let genesis = Some(chain.at(0));
        let enabled = is_quorum_type_enabled_with(
            ctx,
            LlmqType::from_base(t),
            genesis,
            TypeEnabledOverrides::default(),
        );
        matches!(enabled, Err(MemberError::UnknownLlmqType(_)))
    };
    // Every other refusal pairs with Core's one argument-refusal class. That is sound only while
    // `Missing` can mean nothing but a work block below genesis (a base's, or a rotated cycle's):
    // these scenarios hold every list and coinbase, and the model's rows are Core's (compared by
    // digest after every call), so a row Core lacks is one the model lacks too, which the rotation
    // reads as absent, never as `Missing`.
    match e {
        MemberError::UnknownLlmqType(t) if unknown(*t) => ErrorKind::Exception,
        _ => ErrorKind::Argument,
    }
}

/// The oracle's code for a model outcome.
fn outcome_code(outcome: MembersOutcome) -> dash_oracle_sys::MembersOutcome {
    use dash_oracle_sys::MembersOutcome as Core;
    match outcome {
        MembersOutcome::Gate => Core::Gate,
        MembersOutcome::BaseHit => Core::BaseHit,
        MembersOutcome::IndexOutOfRange => Core::IndexOutOfRange,
        MembersOutcome::IndexHit => Core::IndexHit,
        MembersOutcome::Computed => Core::Computed,
    }
}

/// Core's one MEMBERS event of a members call, with its member count.
pub fn core_members_event(events: &[TraceEvent]) -> Option<(dash_oracle_sys::MembersOutcome, u64)> {
    let mut found = events.iter().filter_map(|e| match e {
        TraceEvent::Members {
            outcome,
            n,
            ..
        } => Some((*outcome, *n)),
        _ => None,
    });
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

/// The members, in order: each proTxHash and the masternode as evodb stores it.
pub fn compare_members(ours: &[Arc<Dmn>], theirs: &[Member]) -> Result<(), Divergence> {
    check(ours.len() == theirs.len(), "member count")?;
    for (ours, theirs) in ours.iter().zip(theirs) {
        check(ours.pro_tx_hash.as_bytes() == &theirs.pro_tx_hash, "member proTxHash")?;
        check(dmn_bytes(ours) == theirs.dmn, "member state")?;
    }
    Ok(())
}

/// `diff_quorum_members`; see the module documentation.
pub fn compare_quorum_members(input: &[u8], pin: Pin) -> Compared {
    let mut input = input;
    let Loaded {
        params,
        chain,
        source,
        mut core,
    } = match open(&mut input, pin) {
        Ok(loaded) => loaded,
        Err(compared) => return compared,
    };
    let deployments = HeightDeployments {
        params: &params,
    };
    let bls = bls_ctx_after(&deployments, Some(chain.at(chain.tip())));
    let Ok(ctx) = LlmqCtx::new(&params, &chain, &deployments, bls) else {
        return Ok(Outcome::OutOfDomain);
    };
    let Some(index) = seed_mined(&[], &params, &ctx, &chain, &source, &mut core)? else {
        return Ok(Outcome::OutOfDomain);
    };
    let views = ModelViews {
        ctx: &ctx,
        src: &source,
        reader: &index,
        tip: chain.at(chain.tip()),
    };
    let mut manager = ModelQuorumManager::<Arc<Dmn>>::new(&params);
    let mut rows = index.snapshot_rows().clone();
    let mut tally = Tally::default();
    for call in input.as_chunks::<3>().0.iter().take(MAX_CALLS) {
        let llmq_type = TYPES[usize::from(call[0]) % TYPES.len()];
        let base = chain.at(u32::from(call[1]) % (chain.tip() + 1));
        let reset = call[2] & 1 != 0;
        let ours = manager.members(
            &views,
            &mut RuntimeRows::new(&mut rows),
            LlmqType::from_base(llmq_type),
            base,
            reset,
        );
        let events = manager.take_events();
        let base_hash = chain.hash(base.0).ok_or(HARNESS_VIEW)?;
        let theirs = core.members(llmq_type, base_hash.as_bytes(), bls.is_legacy(), reset);
        match (ours, theirs) {
            (Err(ours @ (MemberError::UnknownLlmqType(_) | MemberError::Missing(_))), Err(e)) => {
                tally.refused(&e, core_refusal(&ctx, &chain, &ours))?;
                break;
            }
            (Err(MemberError::View(_)), _) => return Err(HARNESS_VIEW),
            (Err(_), Ok(_)) | (Ok(_), Err(_)) => return Err(divergence("refusal")),
            (Ok(members), Ok((theirs, trace))) => {
                compare_members(&members, &theirs)?;
                compare_events(&events, &trace)?;
                compare_rows(&mut core, &rows)?;
                tally.accepted = true;
            }
        }
    }
    tally.outcome()
}

/// `diff_type_enabled`; see the module documentation. A query is three bytes: the type (an index
/// into the call types, or with bit 7 of the third byte the raw byte), the previous block's
/// height (above the tip: none, Core's null), then the overrides: bits 0-1 of the third byte the
/// DIP0024 one, bits 2-3 the quorums one (1: false, 2: true, else none).
pub fn compare_type_enabled(input: &[u8], pin: Pin) -> Compared {
    let mut input = input;
    let Loaded {
        params,
        chain,
        mut core,
        ..
    } = match open(&mut input, pin) {
        Ok(loaded) => loaded,
        Err(compared) => return compared,
    };
    let deployments = HeightDeployments {
        params: &params,
    };
    let bls = bls_ctx_after(&deployments, None);
    let Ok(ctx) = LlmqCtx::new(&params, &chain, &deployments, bls) else {
        return Ok(Outcome::OutOfDomain);
    };
    let flag = |bits: u8| match bits & 3 {
        1 => Some(false),
        2 => Some(true),
        _ => None,
    };
    let mut tally = Tally::default();
    for query in input.as_chunks::<3>().0.iter().take(MAX_CALLS) {
        let llmq_type = if query[2] & 0x80 != 0 {
            query[0]
        } else {
            TYPES[usize::from(query[0]) % TYPES.len()]
        };
        let prev = (u32::from(query[1]) <= chain.tip()).then(|| chain.at(u32::from(query[1])));
        let overrides = TypeEnabledOverrides {
            dip24_active: flag(query[2]),
            have_dip24_quorums: flag(query[2] >> 2),
        };
        let ours =
            is_quorum_type_enabled_with(&ctx, LlmqType::from_base(llmq_type), prev, overrides);
        let prev_hash = prev.and_then(|p| chain.hash(p.0)).map(|h| *h.as_bytes());
        let theirs = core.type_enabled(
            llmq_type,
            prev_hash.as_ref(),
            overrides.dip24_active,
            overrides.have_dip24_quorums,
        );
        match (ours, theirs) {
            (Ok(ours), Ok(theirs)) => {
                check(ours == theirs, "enabled")?;
                tally.accepted = true;
            }
            (Err(MemberError::UnknownLlmqType(_)), Err(e)) => {
                tally.refused(&e, ErrorKind::Exception)?;
                break;
            }
            _ => return Err(divergence("refusal")),
        }
    }
    tally.outcome()
}

// ---- scans and selection ---------------------------------------------------------------------

/// One mined commitment of an input: its type, base and mining heights, and quorum index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MinedSpec {
    /// The type's byte.
    pub llmq_type: u8,
    /// The base's height.
    pub base: u32,
    /// The mining block's height, above the base.
    pub mined: u32,
    /// The quorum index (an indexed commitment's).
    pub index: i16,
}

impl MinedSpec {
    /// Reads the mined commitments from the front of `input`: a count byte (up to [`MAX_MINED`]),
    /// then three bytes each. Byte 0: the type, one of regtest's five. Byte 1: the base height
    /// (mod the tip + 1). Byte 2: bits 0-4 the mining height's distance above the base, less 1;
    /// bits 5-6 the quorum index; bit 7 aligns the base to its cycle (plus the index for a
    /// rotated type), where a DKG mines it. A commitment mined above the tip is dropped.
    pub fn read(input: &mut &[u8], params: &ChainSetup, tip: u32) -> Vec<MinedSpec> {
        let Some((&count, rest)) = input.split_first() else {
            return Vec::new();
        };
        *input = rest;
        let mut specs = Vec::new();
        for _ in 0..usize::from(count) % (MAX_MINED + 1) {
            let Some((&[t, base, flags], rest)) = input.split_first_chunk::<3>() else {
                break;
            };
            *input = rest;
            let llmq_type = TYPES[usize::from(t) % 5];
            let Some(llmq) = params.consensus.llmq(LlmqType::from_base(llmq_type)) else {
                continue;
            };
            let index = i16::from(flags >> 5 & 3);
            let mut base = u32::from(base) % (tip + 1);
            if flags & 0x80 != 0 {
                let interval = u32::from(llmq.dkg_interval).max(1);
                base = base - base % interval
                    + if llmq.use_rotation {
                        index as u32
                    } else {
                        0
                    };
            }
            let mined = base + 1 + u32::from(flags & 0x1f);
            if mined <= tip {
                specs.push(MinedSpec {
                    llmq_type,
                    base,
                    mined,
                    index,
                });
            }
        }
        specs
    }

    /// The `CFinalCommitment` a DKG of `members` (their operator keys' material, in member
    /// order) mines, as Core and the node read it: version 3, or the indexed version 4 when
    /// `indexed`, of `size`-bit bitsets with the members' bits set (valid and signing), the
    /// quorum key from `[1; 32]`, vvec hash `[0x11; 32]`, quorumSig by that key and membersSig
    /// the members' secure aggregate, both over `BuildCommitmentHash()`: what `ProcessBlock()`'s
    /// signature checks and `CFinalCommitment::Verify` accept with those members. `None` without
    /// members, or if a key derivation fails.
    pub fn commitment(
        &self,
        base: &BlockHash,
        size: i32,
        indexed: bool,
        members: &[[u8; 32]],
    ) -> Option<Vec<u8>> {
        let size = usize::try_from(size).unwrap_or(0).min(0xfc);
        let mut bitset = vec![size as u8];
        bitset.resize(1 + size.div_ceil(8), 0);
        for i in 0..members.len().min(size) {
            bitset[1 + i / 8] |= 1 << (i % 8);
        }
        let quorum_key = BlsSecretKey::<BlsScIetf>::from_ikm(&[1; 32]).ok()?;
        let public_key = quorum_key.public_key().to_bytes();
        let vvec_hash = [0x11; 32];
        let hash = sha256d(
            &[&[self.llmq_type][..], base.as_bytes(), &bitset, &public_key, &vvec_hash].concat(),
        );
        let keys: Vec<BlsSecretKey<BlsScIetf>> =
            members.iter().map(|ikm| BlsSecretKey::from_ikm(ikm).ok()).collect::<Option<_>>()?;
        let sigs: Vec<BlsSignature<BlsScIetf>> =
            keys.iter().map(|k| k.sign_with(&hash, BlsSigId::Basic)).collect();
        let pks: Vec<BlsPublicKey<BlsScIetf>> = keys.iter().map(|k| k.public_key()).collect();
        let members_sig = BlsSignature::secure_aggregate(
            &sigs.iter().collect::<Vec<_>>(),
            &pks.iter().collect::<Vec<_>>(),
        )
        .ok()?;
        let mut qc = Vec::new();
        qc.extend((3 + u16::from(indexed)).to_le_bytes());
        qc.push(self.llmq_type);
        qc.extend(base.as_bytes());
        if indexed {
            qc.extend(self.index.to_le_bytes());
        }
        qc.extend(&bitset); // signers
        qc.extend(&bitset); // validMembers
        qc.extend(public_key);
        qc.extend(vvec_hash);
        qc.extend(quorum_key.sign_with(&hash, BlsSigId::Basic).to_bytes());
        qc.extend(members_sig.to_bytes());
        Some(qc)
    }
}

/// [`MinedSpec::read`]'s bytes: `(type index, base height, mined distance, quorum index)`, each
/// aligned to its cycle as a DKG mines it.
pub fn mined_bytes(mined: &[(u8, u32, u8, u8)]) -> Vec<u8> {
    let mut bytes = vec![mined.len() as u8];
    for (ty, base, distance, index) in mined {
        bytes.extend([*ty, *base as u8, 0x80 | index << 5 | (distance - 1)]);
    }
    bytes
}

/// One row of the node's mined-commitment index.
struct MinedRow {
    llmq_type: LlmqType,
    family: MinedFamily,
    mined_height: u32,
    base_height: u32,
    qc: Arc<LlmqCommitment>,
    mined_hash: BlockHash,
}

/// The node's mined commitments over the scenario's linear chain: the commitments Core accepted,
/// each in the family Core keys it by (rotation enabled at its base or not), and the snapshot
/// rows the node stored while seeding them.
pub struct MinedIndex {
    hashes: Vec<BlockHash>,
    rows: Vec<MinedRow>,
    snapshot_rows: ModelRows,
}

impl MinedIndex {
    /// A recorded chain's mined commitments `(type, commitment, mined height)` of every type
    /// `params` configures without rotation, as a node's index holds them after connecting the
    /// chain (LQ3b-3). Rotated ones are left out: their family needs the rotation flag at their
    /// base, which needs the snapshot rows a recording does not hold (SB11). `None` for a broken
    /// recording: a chain not held from genesis, a commitment that does not decode exactly, a
    /// second commitment of one quorum, or a base or mining block `chain` does not hold.
    pub fn recorded(
        chain: &HeldChain,
        params: &ChainSetup,
        mined: &[(u8, Vec<u8>, i32)],
    ) -> Option<Self> {
        // Index = height: the chain must be held from genesis.
        let hashes: Vec<BlockHash> =
            (0..=chain.tip()).map(|h| chain.hash(h)).collect::<Option<_>>()?;
        let height_of: HashMap<[u8; 32], u32> =
            hashes.iter().map(|h| *h.as_bytes()).zip(0..).collect();
        let rows = mined
            .iter()
            .filter(|(t, ..)| {
                params.consensus.llmq(LlmqType::from_base(*t)).is_some_and(|l| !l.use_rotation)
            })
            .map(|(t, bytes, mined_height)| {
                let mut input = &bytes[..];
                let qc = LlmqCommitment::decode(&mut input).ok().filter(|_| input.is_empty())?;
                let mined_height = u32::try_from(*mined_height).ok()?;
                Some(MinedRow {
                    llmq_type: LlmqType::from_base(*t),
                    family: MinedFamily::NonRotated,
                    mined_height,
                    base_height: *height_of.get(qc.quorum_hash.as_bytes())?,
                    mined_hash: *hashes.get(usize::try_from(mined_height).ok()?)?,
                    qc: Arc::new(qc),
                })
            })
            .collect::<Option<Vec<MinedRow>>>()?;
        let mut quorums: Vec<(LlmqType, QuorumHash)> =
            rows.iter().map(|r| (r.llmq_type, r.qc.quorum_hash)).collect();
        quorums.sort_by_key(|(t, h)| (t.to_base(), *h.as_bytes()));
        if quorums.windows(2).any(|w| w[0] == w[1]) {
            return None;
        }
        Some(MinedIndex {
            hashes,
            rows,
            snapshot_rows: ModelRows::new(),
        })
    }

    /// The snapshot rows the node stored seeding the commitments; equal to Core's by digest.
    pub fn snapshot_rows(&self) -> &ModelRows {
        &self.snapshot_rows
    }
}

impl ChainAncestry for MinedIndex {
    fn header(&self, id: BlockId) -> HeaderCtx {
        header_ctx(id.0, self.hashes[id.0 as usize])
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        (height <= id.0).then_some(BlockId(height))
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        id.0.checked_sub(1).map(BlockId)
    }
}

impl MinedCommitmentReader for MinedIndex {
    type Error = ViewError;

    fn chain(&self) -> &dyn ChainAncestry {
        self
    }

    fn mined_commitment(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
        at: BlockId,
    ) -> Result<Option<MinedCommitment>, ViewError> {
        let row = self.rows.iter().find(|r| {
            r.llmq_type == llmq_type && r.qc.quorum_hash == *quorum_hash && r.mined_height <= at.0
        });
        Ok(row.map(|r| (Arc::clone(&r.qc), r.mined_hash)))
    }

    fn mined_index(
        &self,
        llmq_type: LlmqType,
        family: MinedFamily,
        chain_end: BlockId,
        max_mined_height: u32,
        limit: usize,
    ) -> Result<Vec<MinedIndexEntry>, ViewError> {
        let cap = max_mined_height.min(chain_end.0);
        let mut rows: Vec<MinedIndexEntry> = self
            .rows
            .iter()
            .filter(|r| {
                r.llmq_type == llmq_type
                    && r.family == family
                    && (1..=cap).contains(&r.mined_height)
            })
            .map(|r| MinedIndexEntry {
                mined_height: r.mined_height,
                base_height: r.base_height,
            })
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.mined_height));
        rows.truncate(limit);
        Ok(rows)
    }
}

/// The operator key material of the masternodes a scenario pools (`numbered(0..=255)`), by
/// proTxHash.
fn operator_ikm(pro_tx_hash: &[u8; 32]) -> Option<[u8; 32]> {
    static IKMS: OnceLock<HashMap<[u8; 32], [u8; 32]>> = OnceLock::new();
    IKMS.get_or_init(|| {
        (0..=255).map(|i| (*numbered(i).pro_tx_hash.as_bytes(), numbered_operator_ikm(i))).collect()
    })
    .get(pro_tx_hash)
    .copied()
}

/// Seeds `specs` into Core, each signed by its quorum's members, and into the node's index the
/// ones Core accepts (Core refuses what block validation never stores: a commitment outside its
/// cycle's mining window, of another base or version, too few members, a second commitment of
/// a quorum, or of a type and index at one mining height). A non-rotated quorum's members are
/// the node's. Before the first rotated commitment, both sides connect every block to the tip,
/// once, as a node connects them before mining it (Core's `PreComputeQuorumMembers`, which
/// stores the snapshot rows rotated members read). Then, for each rotated commitment, both compute
/// its members, which must agree, and restart (LQ1b D38). The node's rows are kept in the index,
/// and must equal Core's by digest. `None` (out of domain) if both sides refuse connecting a
/// block or computing the members; a refusal on one side only diverges. A commitment signed by
/// the members Core refuses for its signatures or member count diverges: the two sides' members
/// differ.
pub fn seed_mined(
    specs: &[MinedSpec],
    params: &ChainSetup,
    ctx: &LlmqCtx<'_, ViewError>,
    chain: &HeldChain,
    source: &MapSource,
    core: &mut LlmqContext,
) -> Result<Option<MinedIndex>, Divergence> {
    let hashes: Vec<BlockHash> = (0..=chain.tip()).filter_map(|h| chain.hash(h)).collect();
    let legacy_bls = ctx.bls().is_legacy();
    // Core makes no members call for a non-rotated commitment, so only the rotated facade
    // follows Core's caches.
    let mut unrotated = ModelMembersFacade::new(params);
    let mut rotated = ModelMembersFacade::new(params);
    let mut snapshot_rows = ModelRows::new();
    let mut connected = false;
    let mut rows = Vec::new();
    for spec in specs {
        let llmq_type = LlmqType::from_base(spec.llmq_type);
        let llmq = ctx.llmq(llmq_type).map_err(|_| divergence("harness types"))?;
        let at = chain.at(spec.base);
        let base = hashes[spec.base as usize];
        let rotation = is_quorum_rotation_enabled(ctx, llmq, at).map_err(|_| HARNESS_VIEW)?;
        let members: Option<Vec<[u8; 32]>> = if rotation {
            if !connected {
                for (height, hash) in (1..).zip(&hashes[1..]) {
                    let ours = rotated.precompute(
                        ctx,
                        source,
                        &mut RuntimeRows::new(&mut snapshot_rows),
                        chain.at(height),
                        PrecomputeMode::Connect,
                    );
                    let theirs = core.precompute(hash.as_bytes(), legacy_bls, Precompute::Connect);
                    match (ours, theirs) {
                        (
                            Err(ours @ (MemberError::Missing(_) | MemberError::UnknownLlmqType(_))),
                            Err(e),
                        ) if e.kind == ErrorKind::Context
                            || e.kind == core_refusal(ctx, chain, &ours) =>
                        {
                            return Ok(None);
                        }
                        (Err(MemberError::View(_)), _) => return Err(HARNESS_VIEW),
                        (Ok(()), Ok(_)) => {}
                        _ => return Err(divergence("precompute")),
                    }
                }
                connected = true;
            }
            let ours = rotated.members(
                ctx,
                source,
                &mut RuntimeRows::new(&mut snapshot_rows),
                llmq_type,
                at,
                false,
            );
            let theirs = core.members(spec.llmq_type, base.as_bytes(), legacy_bls, false);
            let members = match (ours, theirs) {
                (Err(MemberError::View(_)), _) => return Err(HARNESS_VIEW),
                (Ok((ours, outcome)), Ok((theirs, trace))) => {
                    compare_members(&ours, &theirs)?;
                    // A computed call stores the cycle's row, a cache hit stores none.
                    let theirs_outcome = core_members_event(&trace).map(|(o, _)| o);
                    check(theirs_outcome == Some(outcome_code(outcome)), "members outcome")?;
                    Some(theirs.iter().map(|m| m.pro_tx_hash).collect())
                }
                (
                    Err(ours @ (MemberError::Missing(_) | MemberError::UnknownLlmqType(_))),
                    Err(e),
                ) => {
                    if e.kind == ErrorKind::Context {
                        return Ok(None);
                    }
                    // Where Core would assert: no members to sign with.
                    check(e.kind == core_refusal(ctx, chain, &ours), "rotated members")?;
                    None
                }
                _ => return Err(divergence("rotated members")),
            };
            core.restart().map_err(|_| divergence("restart"))?;
            // Core's caches are gone; so are the model's (its rows stay, as Core's evodb).
            rotated = ModelMembersFacade::new(params);
            compare_rows(core, &snapshot_rows)?;
            members
        } else {
            match unrotated.members(ctx, source, &mut NoRows, llmq_type, at, false) {
                Ok((members, _)) => {
                    Some(members.iter().map(|m| *m.pro_tx_hash.as_bytes()).collect())
                }
                // No members to sign with; the comparison meets the refusal itself.
                Err(MemberError::Missing(_) | MemberError::UnknownLlmqType(_)) => None,
                Err(MemberError::View(_)) => return Err(HARNESS_VIEW),
            }
        };
        let Some(ikms) = members
            .and_then(|members| members.iter().map(operator_ikm).collect::<Option<Vec<_>>>())
        else {
            continue;
        };
        let Some(bytes) = spec.commitment(&base, i32::from(llmq.size), rotation, &ikms) else {
            continue;
        };
        if let Err(e) = core.set_mined(spec.llmq_type, &bytes, spec.mined as i32) {
            check(e.kind == ErrorKind::Argument, "seeding")?;
            // The two refusals only the members decide (oracle.h oracle_llmq_set_mined), by
            // their messages: every other check runs before them.
            check(
                !e.message.contains("against its quorum's members")
                    && !e.message.contains("at or beyond the quorum's member count"),
                "members",
            )?;
            continue;
        }
        let mut input = &bytes[..];
        let qc = LlmqCommitment::decode(&mut input).map_err(|_| divergence("commitment"))?;
        let family = if rotation {
            MinedFamily::Rotated {
                quorum_index: spec.index,
            }
        } else {
            MinedFamily::NonRotated
        };
        rows.push(MinedRow {
            llmq_type,
            family,
            mined_height: spec.mined,
            base_height: spec.base,
            qc: Arc::new(qc),
            mined_hash: hashes[spec.mined as usize],
        });
    }
    Ok(Some(MinedIndex {
        hashes,
        rows,
        snapshot_rows,
    }))
}

/// What a call of the scan and selection targets does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuorumOp {
    /// `ScanQuorums(type, start, count)`, or its chain overload.
    Scan,
    /// `GetAllQuorumMembers(type, base, reset_cache)`.
    Members,
    /// `SelectQuorumForSigning(type, active chain, selectionHash, signHeight, signOffset)`.
    Select,
    /// A restarted node: fresh managers and caches over the same databases.
    Restart,
}

/// `diff_scan_quorums`' operations.
pub const SCAN_OPS: [QuorumOp; 3] = [QuorumOp::Scan, QuorumOp::Members, QuorumOp::Restart];
/// `diff_select_quorum`'s operations.
pub const SELECT_OPS: [QuorumOp; 3] = [QuorumOp::Select, QuorumOp::Scan, QuorumOp::Restart];

/// One call of six bytes.
///
/// - Byte 0: bits 0-2 the type (`TYPES`), bits 3-4 the operation (mod the target's), bit 5 the
///   chain overload of a scan or a members call's reset.
/// - Byte 1: a scan's start or a members call's base (mod the tip + 1); a selection's sign
///   height, 0xff the tip (-1), else mod the tip + 12 (so above the tip too).
/// - Byte 2: a scan's count, with bit 7 the low bits (up to 127), else mod 9; a selection's
///   offset, as a signed byte.
/// - Bytes 3-5: a selection's hash: `SHA256d(bytes 3-4)`, its last byte replaced by byte 5 (the
///   rotated signer's bits).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuorumCall {
    pub op: QuorumOp,
    pub llmq_type: u8,
    pub height: i32,
    pub flag: bool,
    pub count: usize,
    pub offset: i32,
    pub selection: [u8; 32],
}

impl QuorumCall {
    /// The call `bytes` encode for a target of `ops` on a chain to `tip`.
    pub fn read(bytes: &[u8; 6], ops: &[QuorumOp], tip: u32) -> QuorumCall {
        let op = ops[usize::from(bytes[0] >> 3 & 3) % ops.len()];
        let height = if op == QuorumOp::Select {
            if bytes[1] == 0xff {
                -1
            } else {
                i32::from(bytes[1]) % (tip as i32 + 12)
            }
        } else {
            (u32::from(bytes[1]) % (tip + 1)) as i32
        };
        let count = if bytes[2] & 0x80 != 0 {
            usize::from(bytes[2] & 0x7f)
        } else {
            usize::from(bytes[2] % 9)
        };
        let mut selection = sha256d(&bytes[3..5]);
        selection[31] = bytes[5];
        QuorumCall {
            op,
            llmq_type: TYPES[usize::from(bytes[0] & 7)],
            height,
            flag: bytes[0] & 0x20 != 0,
            count,
            offset: i32::from(bytes[2] as i8),
            selection,
        }
    }

    /// The block a scan or members call reads; a selection reads its sign height instead, and
    /// the tip stands in.
    pub fn block(&self, tip: u32) -> u32 {
        if self.op == QuorumOp::Select {
            tip
        } else {
            self.height as u32
        }
    }
}

/// [`QuorumCall::read`]'s bytes: the operation (an index into the target's), the type (into
/// [`TYPES`]), the flag, the height, the count or offset, and the selection bytes.
pub fn call_bytes(
    op: u8,
    ty: u8,
    flag: bool,
    height: u8,
    count: u8,
    selection: [u8; 3],
) -> [u8; 6] {
    [ty | op << 3 | u8::from(flag) << 5, height, count, selection[0], selection[1], selection[2]]
}

/// The oracle's code for a model scan-cache use.
fn scan_cache_code(cache: ScanCacheUse) -> dash_oracle_sys::ScanCache {
    use dash_oracle_sys::ScanCache as Core;
    match cache {
        ScanCacheUse::Bypass => Core::Bypass,
        ScanCacheUse::Miss => Core::Miss,
        ScanCacheUse::HitSatisfied => Core::HitSatisfied,
        ScanCacheUse::HitContinued => Core::HitContinued,
        ScanCacheUse::ContinuationBail => Core::ContinuationBail,
    }
}

/// The oracle's code for a model `GetQuorum` outcome.
fn get_quorum_code(outcome: GetQuorumOutcome) -> dash_oracle_sys::GetQuorumOutcome {
    use dash_oracle_sys::GetQuorumOutcome as Core;
    match outcome {
        GetQuorumOutcome::NotMined => Core::NotMined,
        GetQuorumOutcome::CacheHit => Core::CacheHit,
        GetQuorumOutcome::Built => Core::Built,
        GetQuorumOutcome::NoMinedCommitment => Core::NoMinedCommitment,
    }
}

/// A model event as the oracle traces it.
pub fn core_event(event: &ModelEvent) -> TraceEvent {
    match *event {
        ModelEvent::Row {
            llmq_type,
            cycle_base,
            ref snapshot,
        } => TraceEvent::Row {
            key: snapshot_key(llmq_type.to_base(), &cycle_base),
            snapshot: snapshot_bytes(snapshot),
        },
        ModelEvent::Members {
            llmq_type,
            base,
            reset,
            outcome,
            n,
        } => TraceEvent::Members {
            llmq_type: llmq_type.to_base(),
            base: *base.as_bytes(),
            reset,
            outcome: outcome_code(outcome),
            n: n as u64,
        },
        ModelEvent::GetQuorum {
            llmq_type,
            base,
            populate_cache,
            chain_overload,
            outcome,
        } => TraceEvent::GetQuorum {
            llmq_type: llmq_type.to_base(),
            base: *base.as_bytes(),
            populate_cache,
            chain_overload,
            outcome: get_quorum_code(outcome),
        },
        ModelEvent::Scan {
            llmq_type,
            store,
            cache,
            cache_written,
            n,
        } => TraceEvent::Scan {
            llmq_type: llmq_type.to_base(),
            store: *store.as_bytes(),
            cache: scan_cache_code(cache),
            cache_written,
            n: n as u64,
        },
    }
}

/// The model's events against Core's trace, in order.
fn compare_events(ours: &[ModelEvent], theirs: &[TraceEvent]) -> Result<(), Divergence> {
    check(ours.len() == theirs.len(), "trace length")?;
    for (ours, theirs) in ours.iter().zip(theirs) {
        check(core_event(ours) == *theirs, "trace event")?;
    }
    Ok(())
}

/// A call's answer: the quorums' bases, or the members.
pub enum Answer<B, M> {
    Bases(B),
    Members(M),
}

/// Core's answer to a call: the quorums' base hashes, or the members.
pub type CoreAnswer = Answer<Vec<[u8; 32]>, Vec<Member>>;

/// Core's answer to `call` (not a restart) at the block `at` (a selection reads none), and its
/// trace.
pub fn core_call(
    core: &mut LlmqContext,
    call: &QuorumCall,
    at: &[u8; 32],
    legacy_bls: bool,
) -> dash_oracle_sys::Result<(CoreAnswer, Vec<TraceEvent>)> {
    match call.op {
        QuorumOp::Scan => core
            .scan(call.llmq_type, at, call.count as u32, call.flag)
            .map(|(bases, trace)| (Answer::Bases(bases), trace)),
        QuorumOp::Members => core
            .members(call.llmq_type, at, legacy_bls, call.flag)
            .map(|(members, trace)| (Answer::Members(members), trace)),
        QuorumOp::Select => core
            .select(
                call.llmq_type,
                &call.selection,
                (call.height != -1).then_some(call.height),
                call.offset,
            )
            .map(|(base, trace)| (Answer::Bases(base.into_iter().collect()), trace)),
        QuorumOp::Restart => unreachable!("a restart is no call"),
    }
}

/// `diff_scan_quorums` and `diff_select_quorum` (`ops`); see the module documentation.
pub fn compare_quorum_calls(input: &[u8], pin: Pin, ops: &[QuorumOp]) -> Compared {
    let mut input = input;
    let Loaded {
        params,
        chain,
        source,
        mut core,
    } = match open(&mut input, pin) {
        Ok(loaded) => loaded,
        Err(compared) => return compared,
    };
    let deployments = HeightDeployments {
        params: &params,
    };
    let tip = chain.at(chain.tip());
    let bls = bls_ctx_after(&deployments, Some(tip));
    let Ok(ctx) = LlmqCtx::new(&params, &chain, &deployments, bls) else {
        return Ok(Outcome::OutOfDomain);
    };
    let specs = MinedSpec::read(&mut input, &params, chain.tip());
    let Some(index) = seed_mined(&specs, &params, &ctx, &chain, &source, &mut core)? else {
        return Ok(Outcome::OutOfDomain);
    };
    let views = ModelViews {
        ctx: &ctx,
        src: &source,
        reader: &index,
        tip,
    };
    let mut manager = ModelQuorumManager::<Arc<Dmn>>::new(&params);
    let mut rows = index.snapshot_rows().clone();
    let mut tally = Tally::default();
    for bytes in input.as_chunks::<6>().0.iter().take(MAX_CALLS) {
        let call = QuorumCall::read(bytes, ops, chain.tip());
        let llmq_type = LlmqType::from_base(call.llmq_type);
        if call.op == QuorumOp::Restart {
            core.restart().map_err(|_| divergence("restart"))?;
            manager = ModelQuorumManager::new(&params);
            continue;
        }
        let at = chain.at(call.block(chain.tip()));
        let call_rows = &mut RuntimeRows::new(&mut rows);
        let ours = match call.op {
            QuorumOp::Scan => manager
                .scan_quorums(&views, call_rows, llmq_type, at, call.count, call.flag)
                .map(|q| Answer::Bases(q.iter().map(|q| q.base).collect::<Vec<_>>())),
            QuorumOp::Members => {
                manager.members(&views, call_rows, llmq_type, at, call.flag).map(Answer::Members)
            }
            QuorumOp::Select => manager
                .select_quorum_for_signing(
                    &views,
                    call_rows,
                    llmq_type,
                    &call.selection,
                    call.height,
                    call.offset,
                )
                .map(|q| Answer::Bases(q.iter().map(|q| q.base).collect())),
            QuorumOp::Restart => unreachable!("handled above"),
        };
        let events = manager.take_events();
        let at_hash = chain.hash(at.0).ok_or(HARNESS_VIEW)?;
        let theirs = core_call(&mut core, &call, at_hash.as_bytes(), bls.is_legacy());
        match (ours, theirs) {
            (Err(ours @ (MemberError::UnknownLlmqType(_) | MemberError::Missing(_))), Err(e)) => {
                // A selection asserts the type is configured before any gate.
                let expected = if call.op == QuorumOp::Select {
                    ErrorKind::Argument
                } else {
                    core_refusal(&ctx, &chain, &ours)
                };
                tally.refused(&e, expected)?;
                break;
            }
            (Err(MemberError::View(_)), _) => return Err(HARNESS_VIEW),
            (Err(_), Ok(_)) | (Ok(_), Err(_)) => return Err(divergence("refusal")),
            (Ok(ours), Ok((theirs, trace))) => {
                match (ours, theirs) {
                    (Answer::Bases(ours), Answer::Bases(theirs)) => {
                        let ours: Option<Vec<[u8; 32]>> =
                            ours.iter().map(|b| chain.hash(b.0).map(|h| *h.as_bytes())).collect();
                        check(ours.ok_or(HARNESS_VIEW)? == theirs, "quorums")?;
                    }
                    (Answer::Members(ours), Answer::Members(theirs)) => {
                        compare_members(&ours, &theirs)?;
                    }
                    _ => return Err(divergence("harness answer")),
                }
                compare_events(&events, &trace)?;
                compare_rows(&mut core, &rows)?;
                tally.accepted = true;
            }
        }
    }
    tally.outcome()
}

/// `diff_scan_quorums`.
pub fn compare_scan_quorums(input: &[u8], pin: Pin) -> Compared {
    compare_quorum_calls(input, pin, &SCAN_OPS)
}

/// `diff_select_quorum`.
pub fn compare_select_quorum(input: &[u8], pin: Pin) -> Compared {
    compare_quorum_calls(input, pin, &SELECT_OPS)
}

// ---- the rotated target (LQ1b) ----

/// What a call of `diff_quorum_members_rot` does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RotOp {
    /// `GetAllQuorumMembers(type, block, reset_cache)`.
    Members,
    /// `PreComputeQuorumMembers(block, reset_cache)`, as Core connects (or undoes) the block.
    Precompute,
    /// A restarted node: fresh managers and caches over the same databases (rows kept).
    Restart,
    /// A snapshot row both sides hold before computing (a row an earlier node, or a peer,
    /// wrote): only while seeding is open, from a cycle whose work block is held.
    SeedRow,
}

/// The modes a seeded row takes: Core's four, then two it reads as none.
const SEEDED_MODES: [i32; 6] = [0, 1, 2, 3, 4, -1];

/// One call of four bytes.
///
/// - Byte 0: bits 0-2 the type ([`TYPES`]), bits 3-4 the operation ([`RotOp`]'s order), bit 5 a
///   members call's reset or a precompute's undo.
/// - Bytes 1-2: the block's height, little endian, mod the tip + 1 (a precompute at genesis is
///   skipped); a seeded row's cycle is the type's cycle base at or below it.
/// - Byte 3: a seeded row ([`seeded_row`]): its mode (`b % 6` into Core's four, 4 and -1), bits
///   beyond those Core reads (`b >> 3 & 3`), skip entries (`b >> 5`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RotCall {
    pub op: RotOp,
    pub llmq_type: u8,
    pub height: u32,
    pub flag: bool,
    pub row: u8,
    /// The splitmix64 seed of a seeded row's bits and skip entries: the call's bytes.
    pub seed: u64,
}

impl RotCall {
    /// The call `bytes` encode on a chain to `tip`.
    pub fn read(bytes: &[u8; 4], tip: u32) -> RotCall {
        let ops = [RotOp::Members, RotOp::Precompute, RotOp::Restart, RotOp::SeedRow];
        RotCall {
            op: ops[usize::from(bytes[0] >> 3 & 3)],
            llmq_type: TYPES[usize::from(bytes[0] & 7)],
            height: u32::from(u16::from_le_bytes([bytes[1], bytes[2]])) % (tip + 1),
            flag: bytes[0] & 0x20 != 0,
            row: bytes[3],
            seed: u64::from(u32::from_le_bytes(*bytes)),
        }
    }
}

/// [`RotCall::read`]'s bytes: the operation (an index into [`RotOp`]'s order), the type (into
/// [`TYPES`]), the flag, the height and the row byte.
pub fn rot_call_bytes(op: u8, llmq_type: u8, flag: bool, height: u16, row: u8) -> [u8; 4] {
    let [lo, hi] = height.to_le_bytes();
    [llmq_type | op << 3 | u8::from(flag) << 5, lo, hi, row]
}

/// The row a seed call stores for a cycle whose work block lists `eligible` masternodes Core
/// reads a bit for (valid, confirmed; the oracle refuses a shorter bitset, past which Core would
/// read): the mode, `eligible` and up to 3 more bits, and skip entries in `-3 .. bits + 3`
/// (absolute and relative ones, repeats and out-of-range ones included), splitmix64 from the
/// call.
pub fn seeded_row(call: &RotCall, eligible: usize) -> QuorumSnapshot {
    let bits = eligible + usize::from(call.row >> 3 & 3);
    let skips = usize::from(call.row >> 5);
    let random = splitmix(call.seed, bits + 4 * skips);
    let (active, skip_list) = random.split_at(bits);
    let span = bits as i64 + 6;
    QuorumSnapshot {
        active_quorum_members: active.iter().map(|b| b & 1 != 0).collect(),
        skip_list_mode: SEEDED_MODES[usize::from(call.row) % SEEDED_MODES.len()],
        skip_list: skip_list
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| (i64::from(u32::from_le_bytes(*c)) % span - 3) as i32)
            .collect(),
    }
}

/// The masternodes of `list` Core reads a snapshot bit for: the valid, confirmed ones.
fn snapshot_eligible(list: &MnList) -> usize {
    list.iter_valid().filter(|d| d.cold.confirmed_hash != Default::default()).count()
}

/// `diff_quorum_members_rot`: a rotated scenario ([`Scenario::read_rot`]: chains to
/// [`MAX_ROT_TIP`], up to ten LLMQ_TEST_DIP0024 cycles, activations at any alignment, lists that
/// change by registration, removal, phase and key reuse), then up to [`MAX_ROT_CALLS`] calls
/// ([`RotCall`]): members calls, precomputes (connect or undo), restarts, and rows seeded while
/// seeding is open (before the first computing call, or after a restart). Against
/// `ModelQuorumManager` over one set of rows; compared per call: the refusal class, the members,
/// Core's trace (rows by key and bytes), and the rows digest.
pub fn compare_quorum_members_rot(input: &[u8], pin: Pin) -> Compared {
    let mut input = input;
    let Loaded {
        params,
        chain,
        source,
        mut core,
    } = match open_with(&mut input, pin, Scenario::read_rot) {
        Ok(loaded) => loaded,
        Err(compared) => return compared,
    };
    let deployments = HeightDeployments {
        params: &params,
    };
    let tip = chain.at(chain.tip());
    let bls = bls_ctx_after(&deployments, Some(tip));
    let Ok(ctx) = LlmqCtx::new(&params, &chain, &deployments, bls) else {
        return Ok(Outcome::OutOfDomain);
    };
    let Some(index) = seed_mined(&[], &params, &ctx, &chain, &source, &mut core)? else {
        return Ok(Outcome::OutOfDomain);
    };
    let views = ModelViews {
        ctx: &ctx,
        src: &source,
        reader: &index,
        tip,
    };
    let mut manager = ModelQuorumManager::<Arc<Dmn>>::new(&params);
    let mut rows = index.snapshot_rows().clone();
    let mut seeding = true;
    let mut tally = Tally::default();
    for bytes in input.as_chunks::<4>().0.iter().take(MAX_ROT_CALLS) {
        let call = RotCall::read(bytes, chain.tip());
        let llmq_type = LlmqType::from_base(call.llmq_type);
        let block = chain.at(call.height);
        let hash = chain.hash(call.height).ok_or(HARNESS_VIEW)?;
        let (ours, theirs) = match call.op {
            RotOp::Restart => {
                core.restart().map_err(|_| divergence("restart"))?;
                manager = ModelQuorumManager::new(&params);
                seeding = true;
                continue;
            }
            RotOp::SeedRow => {
                let Ok(llmq) = ctx.llmq(llmq_type) else {
                    continue;
                };
                let interval = u32::from(llmq.dkg_interval).max(1);
                let cycle = call.height - call.height % interval;
                // A cycle's work block below genesis: Core dereferences null reading the row.
                let Some(work) = cycle.checked_sub(8).filter(|_| seeding) else {
                    continue;
                };
                let list = source.lists.get(&work).ok_or(HARNESS_VIEW)?;
                let row = seeded_row(&call, snapshot_eligible(list));
                let cycle_hash = chain.hash(cycle).ok_or(HARNESS_VIEW)?;
                core.set_snapshot(call.llmq_type, cycle_hash.as_bytes(), &snapshot_bytes(&row))
                    .map_err(|_| divergence("seeding"))?;
                rows.insert((call.llmq_type, cycle_hash), row);
                compare_rows(&mut core, &rows)?;
                continue;
            }
            RotOp::Members => {
                let ours = manager
                    .members(&views, &mut RuntimeRows::new(&mut rows), llmq_type, block, call.flag)
                    .map(Some);
                let theirs = core
                    .members(call.llmq_type, hash.as_bytes(), bls.is_legacy(), call.flag)
                    .map(|(members, trace)| (Some(members), trace));
                (ours, theirs)
            }
            // Genesis is never connected through block processing, and Core dereferences its
            // null `pprev` there (the oracle refuses it); the model enables no type.
            RotOp::Precompute if call.height == 0 => continue,
            RotOp::Precompute => {
                let (mode, core_mode) = if call.flag {
                    (PrecomputeMode::Undo, Precompute::Undo)
                } else {
                    (PrecomputeMode::Connect, Precompute::Connect)
                };
                let ours = manager
                    .precompute(&views, &mut RuntimeRows::new(&mut rows), block, mode)
                    .map(|()| None);
                let theirs = core
                    .precompute(hash.as_bytes(), bls.is_legacy(), core_mode)
                    .map(|trace| (None, trace));
                (ours, theirs)
            }
        };
        seeding = false;
        let events = manager.take_events();
        match (ours, theirs) {
            (Err(ours @ (MemberError::UnknownLlmqType(_) | MemberError::Missing(_))), Err(e)) => {
                tally.refused(&e, core_refusal(&ctx, &chain, &ours))?;
                break;
            }
            (Err(MemberError::View(_)), _) => return Err(HARNESS_VIEW),
            (Err(_), Ok(_)) | (Ok(_), Err(_)) => return Err(divergence("refusal")),
            (Ok(ours), Ok((theirs, trace))) => {
                if let (Some(ours), Some(theirs)) = (&ours, &theirs) {
                    compare_members(ours, theirs)?;
                }
                compare_events(&events, &trace)?;
                compare_rows(&mut core, &rows)?;
                tally.accepted = true;
            }
        }
    }
    tally.outcome()
}

// ---- public seeds ----

/// Splitmix64 bytes from `seed`.
fn splitmix(seed: u64, n: usize) -> Vec<u8> {
    let mut state = seed;
    (0..n.div_ceil(8))
        .flat_map(|_| {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            (z ^ (z >> 31)).to_le_bytes()
        })
        .take(n)
        .collect()
}

/// The LQ1a targets' public seeds (written by node-l1's `llmq_seeds` example), by target: scenarios
/// across the activation tables (V20, V19 and DIP0024 at 2, 40 and never; LLMQ_TEST of 3 and of
/// 5), with nine masternodes always present or each absent every third block, and per target
/// structured calls (the boundary rows' shapes) and splitmix64 call bytes.
pub fn seed_inputs() -> Vec<(&'static str, Vec<u8>)> {
    // Never; V20 at 40; everything at 2; never with LLMQ_TEST of 5; V19 and DIP0024 at 2.
    let configs = [
        config_byte(3, 3, 3, false),
        config_byte(1, 0, 3, false),
        config_byte(0, 0, 0, false),
        config_byte(3, 3, 3, true),
        config_byte(3, 0, 0, false),
    ];
    // LLMQ_TEST at 24, 48 and 72, LLMQ_TEST_DIP0024 at 48 and 72 (indexes 0 and 1),
    // LLMQ_TEST_PLATFORM at 48, each mined 12 blocks later.
    let mined = mined_bytes(
        &[
            (0, 24, 0),
            (0, 48, 0),
            (0, 72, 0),
            (2, 48, 0),
            (2, 48, 1),
            (2, 72, 0),
            (2, 72, 1),
            (4, 48, 0),
        ]
        .map(|(t, base, index)| (t, base, 12, index)),
    );
    let mut seeds = Vec::new();
    let mut seed = 0x11a1_0000u64;
    for (c, &config) in configs.iter().enumerate() {
        for pool in [nine(), nine_phased()] {
            let base = scenario_bytes(config, 96, true, &pool);
            let with_mined = [&base[..], &mined].concat();
            // Members: types 0-4 at cycle bases and between, a reset, a refusal last.
            let mut members = base.clone();
            for call in [
                [0, 24, 0],
                [0, 24, 0],
                [0, 48, 0],
                [1, 48, 0],
                [3, 72, 0],
                [4, 49, 0],
                [0, 24, 1],
                [2, 48, 0],
                [6, 48, 0],
            ] {
                members.extend(call);
            }
            seeds.push(("diff_quorum_members", members));
            seed += 1;
            seeds.push((
                "diff_quorum_members",
                [base.clone(), splitmix(seed, 3 * MAX_CALLS)].concat(),
            ));
            // Scans: (op, type, flag, height, count) in `QuorumCall`'s layout.
            let mut scans = with_mined.clone();
            for (op, t, flag, height, count) in [
                (0u8, 0u8, 0u8, 96u8, 2u8),
                (0, 0, 0, 96, 1),
                (0, 0, 0, 96, 5),
                (0, 0, 1, 96, 2),
                (1, 0, 0, 72, 0),
                (2, 0, 0, 0, 0),
                (0, 0, 0, 89, 0x83),
                (0, 4, 0, 96, 2),
                (0, 2, 0, 96, 2),
            ] {
                scans.extend(call_bytes(op, t, flag != 0, height, count, [0; 3]));
            }
            seeds.push(("diff_scan_quorums", scans));
            let mut selects = with_mined.clone();
            for (op, t, height, offset, last) in [
                (0u8, 0u8, 0xffu8, 8u8, 0u8),
                (0, 0, 0xff, 8, 0x40),
                (0, 0, 60, 8, 0),
                (1, 0, 88, 2, 0),
                (0, 0, 0xff, 0xff, 0),
                (0, 4, 0xff, 8, 0),
                (0, 2, 0xff, 8, 0x40),
            ] {
                selects.extend(call_bytes(op, t, false, height, offset, [1, c as u8, last]));
            }
            seeds.push(("diff_select_quorum", selects));
            for target in ["diff_scan_quorums", "diff_select_quorum"] {
                seed += 1;
                seeds.push((target, [&with_mined[..], &splitmix(seed, 6 * MAX_CALLS)].concat()));
            }
            // Type queries: four types after genesis, after 29, and with no previous block.
            let mut queries = base.clone();
            for t in [0u8, 2, 3, 5] {
                for prev in [0u8, 29, 200] {
                    queries.extend([t, prev, 0]);
                }
            }
            seeds.push(("diff_type_enabled", queries));
            seed += 1;
            seeds.push(("diff_type_enabled", [base, splitmix(seed, 3 * MAX_CALLS)].concat()));
        }
    }
    seeds.extend(rot_seed_inputs());
    seeds
}

/// [`Scenario::read_rot`]'s bytes: V20, V19 and DIP0024 bytes (`2 + b`, 0xff never), a tip of
/// `24 ..= MAX_ROT_TIP`, and a pool of `[id, flags, life, change]`.
pub fn rot_scenario_bytes(
    activations: [u8; 3],
    tip: u32,
    size5: bool,
    have_data: bool,
    pool: &[[u8; 4]],
) -> Vec<u8> {
    let pool_byte = pool.len() as u8 | u8::from(size5) << 5 | u8::from(have_data) << 7;
    let mut input = [&activations[..], &[(tip - 24) as u8, pool_byte]].concat();
    for entry in pool {
        input.extend(entry);
    }
    input
}

/// Twelve masternodes, listed from height 1 on.
pub fn twelve() -> Vec<[u8; 4]> {
    (1..=12).map(|i| [i, 3, 0, 0]).collect()
}

/// Sixteen masternodes whose lists change: registered at 0 .. 80, some gone 24 .. 72 blocks
/// later, absent every third block, banned, unconfirmed, four reusing the first one's service,
/// operator key or owner key, and from 96 .. 160 on: the first one's service moved (so the one
/// reusing it is listed with a previous cycle's member's service), others banned, moved onto
/// the first's service, or given a fresh operator key.
pub fn evolving() -> Vec<[u8; 4]> {
    (1..=16u8)
        .map(|i| {
            let mut flags = if i % 4 == 0 {
                i % 3
            } else {
                3
            };
            flags |= match i {
                5 => 0x04,
                7 => 0x08,
                9 => 0x20,
                10 => 0x40,
                11 => 0x80,
                12 => 0x30,
                _ => 0,
            };
            let change = match i {
                1 => 1 << 6 | 12,
                2 | 6 => 14,
                3 => 2 << 6 | 16,
                13 => 3 << 6 | 18,
                _ => 0,
            };
            [i, flags, (i % 6) | ((i % 4) << 4), change]
        })
        .collect()
}

/// `diff_quorum_members_rot`'s public seeds: activation alignments (V20 never, at 2 and at the
/// cycle of 48; V19 at 2, 73 and 74, either side of the cycle of 72's `height + 1` test;
/// DIP0024 at 2 and 48; LLMQ_TEST of 5), the two pools, and call shapes: precomputes connecting
/// every cycle base and members calls after them, members calls out of cycle order with a
/// restart (rows absent, then present), seeded rows of every mode with an undo, and splitmix64
/// call bytes.
fn rot_seed_inputs() -> Vec<(&'static str, Vec<u8>)> {
    const NAME: &str = "diff_quorum_members_rot";
    let (members, precompute, restart, seed_row) = (0, 1, 2, 3);
    let dip24 = 2;
    let configs = [
        ([0xff, 0, 0], false),
        ([0, 0, 0], false),
        ([46, 0, 0], false),
        ([0xff, 71, 0], false),
        ([0xff, 72, 0], false),
        ([0xff, 0, 46], false),
        ([0, 0, 0], true),
    ];
    let mut seeds = Vec::new();
    let mut seed = 0x11b1_0000u64;
    for (activations, size5) in configs {
        for pool in [twelve(), evolving()] {
            let base = rot_scenario_bytes(activations, MAX_ROT_TIP, size5, true, &pool);
            let mut connect = base.clone();
            for cycle in (24..=264u16).step_by(24) {
                connect.extend(rot_call_bytes(precompute, dip24, false, cycle, 0));
            }
            for (height, reset) in [(265, false), (264, false), (241, false), (120, true)] {
                connect.extend(rot_call_bytes(members, dip24, reset, height, 0));
            }
            connect.extend(rot_call_bytes(members, 0, false, 264, 0));
            seeds.push((NAME, connect));

            let mut out_of_order = base.clone();
            for height in [192, 96, 120, 144, 168, 193] {
                out_of_order.extend(rot_call_bytes(members, dip24, false, height, 0));
            }
            out_of_order.extend(rot_call_bytes(restart, 0, false, 0, 0));
            for height in [192, 216, 217] {
                out_of_order.extend(rot_call_bytes(members, dip24, false, height, 0));
            }
            seeds.push((NAME, out_of_order));

            let mut seeded = base.clone();
            for (height, row) in [(48, 0x00), (72, 0x29), (96, 0x4a), (120, 0x8b), (144, 0x0c)] {
                seeded.extend(rot_call_bytes(seed_row, dip24, false, height, row));
            }
            seeded.extend(rot_call_bytes(seed_row, dip24, false, 168, 0x65));
            for (op, height, flag) in [
                (members, 168, false),
                (members, 192, false),
                (precompute, 216, false),
                (precompute, 216, true),
                (members, 217, false),
            ] {
                seeded.extend(rot_call_bytes(op, dip24, flag, height, 0));
            }
            seeds.push((NAME, seeded));
            seed += 1;
            seeds.push((NAME, [base, splitmix(seed, 4 * MAX_ROT_CALLS)].concat()));
        }
    }
    seeds
}
