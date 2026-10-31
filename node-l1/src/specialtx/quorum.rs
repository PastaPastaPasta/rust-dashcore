//! Types 7 and 9 in `diff_check_special_tx` (EV1 §6.2, EV1-5b-2): the quorum world.
//!
//! `oracle_check_special_tx` refuses MNHF signals and asset unlocks once DIP3 is active (their
//! checks read the quorum manager), so [`super::verdicts`] sends them here: Core's
//! `CheckSpecialTxInner` as `ProcessSpecialTxsInBlock` calls it (`oracle_check_special_tx_block`,
//! A-1), against dash-dml's `check_special_tx_inner` with the quorum seams bound.
//!
//! The world (plan Q3): regtest from genesis to [`TIP`] with `-dip3params=2:2` and V20 at 50, V19
//! from height 1 (regtest's default; at 200 in a [`super::FLAG_LEGACY`] world). The context starts
//! at genesis, so Core answers every quorum-hash and scan reject (A-1 refuses them in a context
//! that does not). Its quorums are [`QUORUMS`]: two of `llmqTypePlatform` and one of
//! `llmqTypeMnhf`, each mined 12 blocks after its base, each signed by three members the
//! context gives Core as the quorum's members, each with its own key. Two headers off the chain
//! are known: one on block 40 and one on the tip. A case adds a [`QuorumState`]: the quorums the
//! node did not mine and the credit pool's asset-unlock indexes.
//!
//! The paths: in a [`super::FLAG_BLOCK`] world ours runs the block path (the overloads bound to
//! the active chain, uncached, and the pool's indexes) and Core gets the indexes; otherwise ours
//! runs the mempool path (the cached overloads from a cold cache, no indexes) and Core gets the
//! empty set, which `Contains` reads as `CheckSpecialTx`'s `std::nullopt`.
//!
//! On our side the quorum manager is [`WorldQuorums`]: LQ1a's `scan_quorums_from` over the
//! world's mined commitments, with a materializer that finds a quorum exactly where Core's
//! `GetQuorum` does (its commitment mined in the chain).

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use dash_consensus::ctx::{CheckpointView, Deployment, DeploymentView, bls_ctx_for_block};
use dash_consensus::hash::sha256d;
use dash_consensus::primitives::Tx;
use dash_consensus::{BlockId, ChainAncestry, ConsensusCtx, decode_header};
use dash_dml::MnList;
use dash_dml::blockprocessor::{BlockLookup, QcBlockError};
use dash_dml::creditpool::RangesSet;
use dash_dml::netinfo::NetInfoRules;
use dash_dml::specialtx::qc::CommitmentVerifier;
use dash_dml::specialtx::quorum::{
    MAX_QUORUM_SCAN, QuorumKey, QuorumLookup, QuorumScan, SeamError, UnlockIndexes,
};
use dash_dml::specialtx::sig::SerialVerifier;
use dash_dml::specialtx::{QuorumSeams, SpecialTxCtx, SpecialTxRules, check_special_tx_inner};
use dash_dml::store::StoreError;
use dash_dml::testkit::replay::{Coins, Headers};
use dash_evo::ctx::EvoCtx;
use dash_evo::effect::{Effect, EffectSink};
use dash_evo::llmq::{
    CompleteChain, LlmqCommitment, LlmqCtx, LlmqError, MinedCommitment, MinedCommitmentReader,
    MinedFamily, MinedIndexEntry, QuorumMaterializer, ScanCache, ScanEntry, scan_quorums_from,
    scan_store_block,
};
use dash_evo::locks::sign_hash;
use dash_oracle_sys::{
    BlockContext, Chain, Checkpoints, CoinsView, Context, DEPLOYMENT_EVO_SHARES, DEPLOYMENT_V24,
    MinedCommitment as CoreMined, MnList as CoreList, Oracle, QuorumMembers, ResultCode,
    ThresholdState, UnlockIndexes as CoreIndexes, Verdict,
};
use dash_params::{ChainOption, ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_pkc::bls::{
    BlsEagerSig, BlsGlobalSig, BlsLazyPk, BlsPublicKey, BlsScChia, BlsScIetf, BlsSchemeKind,
    BlsSchemeSource, BlsSecretKey, BlsSigId, BlsSignature,
};
use dash_primitives::{AssetUnlock, BlockHash, LlmqType, MnHardFork, QuorumHash, TxType};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;

use super::{Case, Payload, Reject, Verdicts, World};
use crate::dml::{CORE_NET_INFO, mainnet};
use crate::inputs::oracle_failure;
use crate::{Divergence, check, divergence};

/// The world's tip: the checks are of block 65.
pub const TIP: u32 = 64;
/// `-testactivationheight` and `-dip3params` of the world's chain.
const V20: &str = "v20@50";
const V19_LATE: &str = "v19@200";
const DIP3: &str = "2:2";

/// A quorum of the world: its type (`llmqTypePlatform` or `llmqTypeMnhf`), base and mining
/// heights, and the key material of its quorum key.
#[derive(Clone, Copy, Debug)]
pub struct Quorum {
    pub platform: bool,
    pub base: u32,
    pub mined: u32,
    pub seed: u8,
}

/// The world's quorums: the platform type's at 24 and 48 (its scan, of
/// `signingActiveQuorumCount + 1`, returns both, newest first), and the MNHF type's at 48. Bases
/// are multiples of the types' DKG interval (24) and each is mined 12 blocks later, inside the
/// mining window.
pub const QUORUMS: [Quorum; 3] = [
    Quorum {
        platform: true,
        base: 24,
        mined: 36,
        seed: 0x61,
    },
    Quorum {
        platform: true,
        base: 48,
        mined: 60,
        seed: 0x62,
    },
    Quorum {
        platform: false,
        base: 48,
        mined: 60,
        seed: 0x63,
    },
];
/// Indexes into [`QUORUMS`].
pub const PLATFORM_OLD: usize = 0;
pub const PLATFORM_NEW: usize = 1;
pub const MNHF: usize = 2;

/// The key material of a key no quorum has (and of a second one, `STRANGER + 1`).
const STRANGER: u8 = 0x6d;
/// The masternodes the quorums have as their members, by key material.
const MEMBERS: [u8; 3] = [0x71, 0x72, 0x73];

/// A case's quorum state: the quorums of [`QUORUMS`] the node did not mine (by bit), whether
/// it mined the quorums of the type the case does not read too, and the credit pool's
/// asset-unlock indexes.
///
/// Core checks every commitment the block context gives it at each call (about 20 ms each), so
/// a case holds its own type's quorums only, unless a mutation names the other type's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QuorumState {
    pub unmined: u8,
    pub others: bool,
    pub indexes: Vec<u64>,
}

impl QuorumState {
    /// Whether the node holds quorum `i` for a case of type 9 (`platform`) or 7.
    pub fn held(&self, i: usize, platform: bool) -> bool {
        self.unmined & 1 << i == 0 && (QUORUMS[i].platform == platform || self.others)
    }

    /// The bytes [`Case::fingerprint`] records.
    pub fn fingerprint(&self) -> Vec<u8> {
        let mut bytes = vec![self.unmined, u8::from(self.others)];
        for index in &self.indexes {
            bytes.extend(index.to_le_bytes());
        }
        bytes
    }
}

/// The time of the header at `height`: ten minutes apart.
fn block_time(height: u32) -> u32 {
    1_417_713_337 + 600 * height
}

fn header(prev: &[u8; 32], time: u32) -> [u8; 80] {
    let mut h = [0u8; 80];
    h[0..4].copy_from_slice(&4i32.to_le_bytes());
    h[4..36].copy_from_slice(prev);
    h[68..72].copy_from_slice(&time.to_le_bytes());
    h[72..76].copy_from_slice(&0x207f_ffffu32.to_le_bytes());
    h
}

fn hash_of(header: &[u8; 80]) -> Option<BlockHash> {
    Some(decode_header(&mut &header[..]).ok()?.value().hash())
}

/// The BLS key from key material `seed`, in either scheme.
fn secret(seed: u8) -> Option<BlsSecretKey<BlsScIetf>> {
    BlsSecretKey::<BlsScIetf>::from_ikm(&[seed; 32]).ok()
}

/// `hash` signed by key `seed`, as the bytes of the scheme `legacy` names.
fn sign_bytes(seed: u8, hash: &[u8; 32], legacy: bool) -> Option<[u8; 96]> {
    let basic = secret(seed)?;
    if legacy {
        let chia = BlsSecretKey::<BlsScChia>::from_bytes(&basic.to_bytes()).ok()?;
        Some(chia.sign_digest(hash).to_bytes())
    } else {
        Some(basic.sign_with(hash, BlsSigId::Basic).to_bytes())
    }
}

/// A valid masternode as evodb stores it (`CDeterministicMN`, SER_DISK): proTxHash `[seed; 32]`,
/// internal id `id`, a version-2 state (basic-scheme operator key `secret(seed)`) registered at
/// 0, with the confirmed hash `CalculateQuorum()` needs, service 1.1.1.`seed`:9999, regular.
/// (`dash-oracle-sys/tests/common/quorum.rs`'s `member_dmn`, which A-1's tests run.)
fn member_dmn(id: u8, seed: u8) -> Option<Vec<u8>> {
    let pro_tx_hash = [seed; 32];
    let confirmed = [seed ^ 0xc0; 32];
    let mut state = Vec::new();
    for v in [2i32, 0, 0, 0, 0, -1, -1] {
        state.extend(v.to_le_bytes());
    }
    state.extend(0u16.to_le_bytes());
    state.extend(confirmed);
    state.extend(sha256d(&[pro_tx_hash, confirmed].concat()));
    state.extend([seed; 20]);
    state.extend(secret(seed)?.public_key().to_bytes());
    state.extend([seed; 20]);
    let mut service = [0u8; 18];
    service[10..12].copy_from_slice(&[0xff, 0xff]);
    service[12..16].copy_from_slice(&[1, 1, 1, seed]);
    service[16..].copy_from_slice(&9999u16.to_be_bytes());
    state.extend(service);
    state.extend([0, 0]);
    state.extend([0; 20 + 4]);
    let mut out = pro_tx_hash.to_vec();
    out.push(id);
    out.extend([id + 1; 32]);
    out.extend(0u32.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out.extend(state);
    out.extend(0u16.to_le_bytes());
    Some(out)
}

/// The `CFinalCommitment` (network serialization, not indexed) of quorum `(llmq_type, base)` of
/// `size` with [`MEMBERS`] valid and signing: version 3 (basic), or 1 (legacy) in a world
/// before V19, quorum key `secret(seed)`, vvec hash `[0x11; 32]`, quorumSig by the quorum key
/// and membersSig the members' secure aggregate, both over `BuildCommitmentHash()`.
fn commitment(
    llmq_type: u8,
    base: &BlockHash,
    size: usize,
    seed: u8,
    legacy: bool,
) -> Option<Vec<u8>> {
    let mut bitset = vec![u8::try_from(size).ok()?];
    bitset.resize(1 + size.div_ceil(8), 0);
    for i in 0..MEMBERS.len() {
        bitset[1 + i / 8] |= 1 << (i % 8);
    }
    let quorum_key = secret(seed)?;
    let public_key = if legacy {
        BlsSecretKey::<BlsScChia>::from_bytes(&quorum_key.to_bytes()).ok()?.public_key().to_bytes()
    } else {
        quorum_key.public_key().to_bytes()
    };
    let vvec_hash = [0x11; 32];
    let hash =
        sha256d(&[&[llmq_type][..], base.as_bytes(), &bitset, &public_key, &vvec_hash].concat());
    let members_sig = if legacy {
        let keys: Vec<BlsSecretKey<BlsScChia>> = (MEMBERS.iter())
            .map(|&s| BlsSecretKey::from_bytes(&secret(s)?.to_bytes()).ok())
            .collect::<Option<_>>()?;
        let sigs: Vec<BlsSignature<BlsScChia>> =
            keys.iter().map(|k| k.sign_digest(&hash)).collect();
        let pks: Vec<BlsPublicKey<BlsScChia>> = keys.iter().map(|k| k.public_key()).collect();
        BlsSignature::secure_aggregate(
            &sigs.iter().collect::<Vec<_>>(),
            &pks.iter().collect::<Vec<_>>(),
        )
        .ok()?
        .to_bytes()
    } else {
        let keys: Vec<BlsSecretKey<BlsScIetf>> =
            MEMBERS.iter().map(|&s| secret(s)).collect::<Option<_>>()?;
        let sigs: Vec<BlsSignature<BlsScIetf>> =
            keys.iter().map(|k| k.sign_with(&hash, BlsSigId::Basic)).collect();
        let pks: Vec<BlsPublicKey<BlsScIetf>> = keys.iter().map(|k| k.public_key()).collect();
        BlsSignature::secure_aggregate(
            &sigs.iter().collect::<Vec<_>>(),
            &pks.iter().collect::<Vec<_>>(),
        )
        .ok()?
        .to_bytes()
    };
    let mut qc = if legacy {
        1u16
    } else {
        3u16
    }
    .to_le_bytes()
    .to_vec();
    qc.push(llmq_type);
    qc.extend(base.as_bytes());
    qc.extend(&bitset);
    qc.extend(&bitset);
    qc.extend(public_key);
    qc.extend(vvec_hash);
    qc.extend(sign_bytes(seed, &hash, legacy)?);
    qc.extend(members_sig);
    Some(qc)
}

/// One mined quorum of the world, as both sides hold it.
struct Mined {
    llmq_type: LlmqType,
    base: u32,
    mined: u32,
    bytes: Vec<u8>,
    qc: Arc<LlmqCommitment>,
    key: BlsLazyPk,
}

/// The world of one global flag, built once.
pub struct QuorumWorld {
    pub params: ChainParams,
    legacy: bool,
    /// The chain's headers from genesis to [`TIP`], and their hashes.
    headers: Vec<[u8; 80]>,
    hashes: Vec<BlockHash>,
    ours: Headers,
    /// The known headers off the chain: on block 40, and on the tip.
    forks: [[u8; 80]; 2],
    fork_hashes: [BlockHash; 2],
    /// [`QUORUMS`]' commitments, in order.
    mined: Vec<Mined>,
    /// The tip's median time past.
    mtp: i64,
    index: Index,
}

/// `LookupBlockIndex`: the chain's headers (id = height) and the forks (ids past the tip).
struct Index(BTreeMap<BlockHash, (BlockId, u32)>);

impl BlockLookup for Index {
    fn lookup(&self, hash: &BlockHash) -> Option<(BlockId, u32)> {
        self.0.get(hash).copied()
    }
}

impl QuorumWorld {
    fn build(legacy: bool) -> Option<QuorumWorld> {
        let mut builder = ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0)
            .arg(ChainOption::Dip3Params, DIP3)
            .arg(ChainOption::TestActivationHeight, V20);
        if legacy {
            builder = builder.arg(ChainOption::TestActivationHeight, V19_LATE);
        }
        let params = builder.build().ok()?;
        let mut headers = Vec::new();
        let mut hashes = Vec::new();
        let mut prev = [0u8; 32];
        for height in 0..=TIP {
            let h = header(&prev, block_time(height));
            let hash = hash_of(&h)?;
            prev = *hash.as_bytes();
            headers.push(h);
            hashes.push(hash);
        }
        let forks = [
            header(hashes[40].as_bytes(), block_time(41) + 1),
            header(hashes[TIP as usize].as_bytes(), block_time(TIP + 1)),
        ];
        let fork_hashes = [hash_of(&forks[0])?, hash_of(&forks[1])?];
        let ours = Headers::new(0, &headers)?;
        let mut index: BTreeMap<BlockHash, (BlockId, u32)> =
            (0..=TIP).map(|h| (hashes[h as usize], (BlockId(h), h))).collect();
        index.insert(fork_hashes[0], (BlockId(TIP + 1), 41));
        index.insert(fork_hashes[1], (BlockId(TIP + 2), TIP + 1));
        let mut mined = Vec::new();
        for q in QUORUMS {
            let llmq_type = if q.platform {
                params.consensus.llmq_type_platform
            } else {
                params.consensus.llmq_type_mnhf
            };
            let size = usize::try_from(params.consensus.llmq(llmq_type)?.size).ok()?;
            let base = hashes[q.base as usize];
            let bytes = commitment(llmq_type.to_base(), &base, size, q.seed, legacy)?;
            let qc = LlmqCommitment::decode(&mut &bytes[..]).ok()?;
            let scheme = if legacy {
                BlsSchemeKind::Legacy
            } else {
                BlsSchemeKind::Basic
            };
            let key = BlsLazyPk::from_value(qc.quorum_public_key.value().clone(), scheme);
            mined.push(Mined {
                llmq_type,
                base: q.base,
                mined: q.mined,
                bytes,
                qc: Arc::new(qc),
                key,
            });
        }
        let mut times: Vec<i64> =
            (TIP.saturating_sub(10)..=TIP).map(|h| i64::from(block_time(h))).collect();
        times.sort_unstable();
        Some(QuorumWorld {
            params,
            legacy,
            headers,
            hashes,
            ours,
            forks,
            fork_hashes,
            mtp: times[times.len() / 2],
            mined,
            index: Index(index),
        })
    }

    /// The world of `world`'s global flag.
    pub fn of(world: &World) -> Option<&'static QuorumWorld> {
        static BASIC: OnceLock<Option<QuorumWorld>> = OnceLock::new();
        static LEGACY: OnceLock<Option<QuorumWorld>> = OnceLock::new();
        if world.v19 {
            BASIC.get_or_init(|| QuorumWorld::build(false)).as_ref()
        } else {
            LEGACY.get_or_init(|| QuorumWorld::build(true)).as_ref()
        }
    }

    /// The hash of the chain's block at `height`.
    pub fn hash(&self, height: u32) -> QuorumHash {
        QuorumHash::from(*self.hashes[height as usize].as_bytes())
    }

    /// The hash of quorum `i` of [`QUORUMS`].
    pub fn quorum_hash(&self, i: usize) -> QuorumHash {
        self.hash(QUORUMS[i].base)
    }

    /// The hash of known fork header `i` (0: on block 40, 1: on the tip).
    pub fn fork_hash(&self, i: usize) -> QuorumHash {
        QuorumHash::from(*self.fork_hashes[i].as_bytes())
    }

    /// The quorum of type `platform` named `hash`, if the world has one.
    fn quorum_named(&self, platform: bool, hash: &QuorumHash) -> Option<usize> {
        (0..QUORUMS.len())
            .find(|&i| QUORUMS[i].platform == platform && self.quorum_hash(i) == *hash)
    }

    pub fn mnhf_type(&self) -> LlmqType {
        self.params.consensus.llmq_type_mnhf
    }

    pub fn platform_type(&self) -> LlmqType {
        self.params.consensus.llmq_type_platform
    }

    fn chain(&self) -> Chain {
        let mut args = vec![format!("-dip3params={DIP3}"), format!("-testactivationheight={V20}")];
        if self.legacy {
            args.push(format!("-testactivationheight={V19_LATE}"));
        }
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        Chain::new("regtest", &refs)
    }

    /// Core's context: the headers from genesis, the global flag, the deployments, no coins, an
    /// empty list, and the members of each quorum the node mined.
    fn context(
        &self,
        world: &World,
        state: &QuorumState,
        platform: bool,
    ) -> Result<Context, Divergence> {
        let mut list = vec![0u8; 32];
        list.extend((-1i32).to_le_bytes());
        list.extend([0; 5]);
        let mut quorums = Vec::new();
        for (i, m) in self.mined.iter().enumerate().filter(|(i, _)| state.held(*i, platform)) {
            let members = (MEMBERS.iter().enumerate())
                .map(|(id, &seed)| member_dmn(id as u8, seed))
                .collect::<Option<Vec<_>>>()
                .ok_or(divergence("members"))?;
            quorums.push(QuorumMembers {
                llmq_type: m.llmq_type.to_base(),
                quorum_hash: *self.quorum_hash(i).as_bytes(),
                members,
            });
        }
        Ok(Context::new(
            0,
            self.headers.clone(),
            self.legacy,
            world_deployments(world),
            1_500_000_000,
            Checkpoints::Disabled,
            CoinsView::Unchecked(vec![]),
            CoreList::Supplied(list),
            quorums,
        ))
    }

    /// Core's block context: the forks, the commitments the node mined, and in a block-path
    /// world the indexes.
    fn block(&self, world: &World, state: &QuorumState, platform: bool) -> BlockContext {
        BlockContext {
            fork_headers: self.forks.to_vec(),
            mined: (self.mined.iter().enumerate())
                .filter(|(i, _)| state.held(*i, platform))
                .map(|(_, m)| CoreMined {
                    llmq_type: m.llmq_type.to_base(),
                    commitment: m.bytes.clone(),
                    mined_height: m.mined as i32,
                })
                .collect(),
            unlock_indexes: if world.block {
                CoreIndexes::from_indexes(state.indexes.iter().copied())
            } else {
                CoreIndexes::default()
            },
        }
    }
}

fn world_deployments(world: &World) -> Vec<(u8, ThresholdState)> {
    let state = |on: bool| {
        if on {
            ThresholdState::Active
        } else {
            ThresholdState::Defined
        }
    };
    vec![(DEPLOYMENT_V24, state(world.v24)), (DEPLOYMENT_EVO_SHARES, state(world.evo_shares))]
}

// ---- our side ----------------------------------------------------------------------------------

/// The node's mined commitments: the quorums `state` holds for a case of type 9 (`platform`) or
/// 7.
struct Reader<'a> {
    world: &'a QuorumWorld,
    state: &'a QuorumState,
    platform: bool,
}

impl Reader<'_> {
    fn rows(&self) -> impl Iterator<Item = &Mined> {
        (self.world.mined.iter().enumerate())
            .filter(|(i, _)| self.state.held(*i, self.platform))
            .map(|(_, m)| m)
    }
}

impl MinedCommitmentReader for Reader<'_> {
    type Error = StoreError;

    fn chain(&self) -> &dyn ChainAncestry {
        &self.world.ours
    }

    fn mined_commitment(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
        at: BlockId,
    ) -> Result<Option<MinedCommitment>, StoreError> {
        let at = self.world.ours.header(at).height;
        let row = self.rows().find(|m| {
            m.llmq_type == llmq_type && m.qc.quorum_hash == *quorum_hash && m.mined <= at
        });
        Ok(row.map(|m| (Arc::clone(&m.qc), self.world.hashes[m.mined as usize])))
    }

    fn mined_index(
        &self,
        llmq_type: LlmqType,
        family: MinedFamily,
        chain_end: BlockId,
        max_mined_height: u32,
        limit: usize,
    ) -> Result<Vec<MinedIndexEntry>, StoreError> {
        let cap = max_mined_height.min(self.world.ours.header(chain_end).height);
        let mut rows: Vec<MinedIndexEntry> = self
            .rows()
            .filter(|m| {
                m.llmq_type == llmq_type
                    && family == MinedFamily::NonRotated
                    && (1..=cap).contains(&m.mined)
            })
            .map(|m| MinedIndexEntry {
                mined_height: m.mined,
                base_height: m.base,
            })
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.mined_height));
        rows.truncate(limit);
        Ok(rows)
    }
}

/// A quorum a scan returns: its base and hash.
#[derive(Clone)]
struct Found {
    base: BlockId,
    hash: QuorumHash,
}

impl ScanEntry for Found {
    fn base(&self) -> BlockId {
        self.base
    }
    fn quorum_hash(&self) -> QuorumHash {
        self.hash
    }
    fn quorum_index(&self) -> i16 {
        0
    }
}

/// Core's `GetQuorum(type, pindexBase)` as the scan calls it: the quorum, if its commitment is
/// mined in the chain (the world's members are always there, so `BuildQuorumFromCommitment`
/// never fails).
struct Materializer<'r, 'a> {
    reader: &'r Reader<'a>,
}

impl QuorumMaterializer for Materializer<'_, '_> {
    type Quorum = Found;
    type Error = StoreError;

    fn get_quorum(
        &mut self,
        llmq_type: LlmqType,
        base: BlockId,
        _populate_cache: bool,
    ) -> Result<Option<Found>, LlmqError<StoreError>> {
        let hash = QuorumHash::from(*self.reader.world.ours.header(base).hash.as_bytes());
        let mined = self.reader.mined_commitment(llmq_type, &hash, BlockId(TIP));
        let mined = mined.map_err(dash_evo::llmq::MemberError::View)?;
        Ok(mined.map(|_| Found {
            base,
            hash,
        }))
    }
}

/// Our quorum manager over the world (see the module documentation).
struct WorldQuorums<'a> {
    reader: Reader<'a>,
    /// The block path: the chain overloads, which bypass the scan cache.
    chain_overload: bool,
    deployments: &'a dyn DeploymentView,
}

fn seam_error(e: LlmqError<StoreError>) -> SeamError {
    SeamError::Store(StoreError::Read {
        what: format!("quorum world: {e:?}").into(),
    })
}

impl QuorumLookup for WorldQuorums<'_> {
    // Core: llmq/quorumsman.cpp GetQuorum(type, quorumHash[, chain])
    fn get_quorum(
        &mut self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
        _fx: &mut dyn EffectSink,
    ) -> Result<Option<QuorumKey>, SeamError> {
        let world = self.reader.world;
        if world.index.lookup(&BlockHash::from(*quorum_hash.as_bytes())).is_none() {
            return Ok(None);
        }
        let mined = (self.reader.mined_commitment(llmq_type, quorum_hash, BlockId(TIP)))
            .map_err(SeamError::Store)?;
        Ok(mined.and_then(|(qc, _)| {
            let m = world.mined.iter().find(|m| Arc::ptr_eq(&m.qc, &qc))?;
            Some(QuorumKey {
                quorum_hash: *quorum_hash,
                public_key: m.key.clone(),
            })
        }))
    }

    // Core: llmq/quorumsman.cpp ScanQuorums(type, pindexStart, n[, chain])
    fn scan_quorums(
        &mut self,
        llmq_type: LlmqType,
        at: BlockId,
        n: usize,
        out: &mut QuorumScan,
        _fx: &mut dyn EffectSink,
    ) -> Result<(), SeamError> {
        if n > MAX_QUORUM_SCAN {
            return Err(SeamError::ScanCapacity);
        }
        out.clear();
        let world = self.reader.world;
        let complete = CompleteChain::<StoreError>::new(&world.ours);
        let bls = bls_ctx_for_block(&world.params, TIP + 1);
        let ctx = LlmqCtx::new(&world.params, &complete, self.deployments, bls).map_err(|e| {
            SeamError::Store(StoreError::Read {
                what: format!("quorum world params: {e:?}").into(),
            })
        })?;
        let Some(store) = scan_store_block(&ctx, llmq_type, at, n).map_err(seam_error)? else {
            return Ok(());
        };
        let cache = if self.chain_overload {
            ScanCache::Bypass
        } else {
            ScanCache::Miss
        };
        let mut mat = Materializer {
            reader: &self.reader,
        };
        let outcome = scan_quorums_from(&ctx, &self.reader, llmq_type, store, n, cache, &mut mat)
            .map_err(seam_error)?;
        for q in outcome.result {
            out.push(q.hash)?;
        }
        Ok(())
    }
}

/// Buried deployments by height on the world's chain (id = height); no versionbits deployment
/// is active by signals (V24 and EVO_SHARES are rules, [`SpecialTxRules`]).
struct Buried<'a> {
    params: &'a ChainParams,
}

impl DeploymentView for Buried<'_> {
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

struct NoCheckpoints;

impl CheckpointView for NoCheckpoints {
    fn highest_checkpoint_in_index(&self) -> Option<u32> {
        None
    }
}

/// Type 6 never comes here.
struct NoCommitments;

impl CommitmentVerifier for NoCommitments {
    fn verify_null(&self, _: &LlmqCommitment) -> bool {
        false
    }

    fn verify(
        &self,
        _: &LlmqCommitment,
        _: BlockId,
        _: &mut dyn EffectSink,
    ) -> Result<bool, QcBlockError> {
        Ok(false)
    }
}

/// Our verdict on `tx` in `case`'s world.
fn ours(world: &QuorumWorld, case: &Case, tx: &Tx) -> Result<Option<Reject>, Divergence> {
    let params = &world.params;
    let deployments = Buried {
        params,
    };
    let height = TIP + 1;
    let bls = bls_ctx_for_block(params, height);
    check(bls.is_legacy() == world.legacy, "the world's global flag")?;
    let cons = ConsensusCtx {
        params,
        height,
        prev: Some(BlockId(TIP)),
        chain: &world.ours,
        deployments: &deployments,
        checkpoints: &NoCheckpoints,
        bls,
        median_time_past: world.mtp,
        header_check_time: 0,
    };
    let complete = CompleteChain::<StoreError>::new(&world.ours);
    let llmq = LlmqCtx::new(params, &complete, &deployments, bls)
        .map_err(|_| divergence("quorum world LLMQ context"))?;
    let mut quorums = WorldQuorums {
        reader: Reader {
            world,
            state: &case.quorum,
            platform: case.tx_type() == TxType::AssetUnlock,
        },
        chain_overload: case.world.block,
        deployments: &deployments,
    };
    let mut effects: Vec<Effect> = Vec::new();
    let mut ranges = RangesSet::default();
    for &index in &case.quorum.indexes {
        ranges.add(index);
    }
    let list = MnList::new(world.hashes[TIP as usize], TIP as i32, 1);
    let coins = Coins::new(&[], None);
    let mut cx = SpecialTxCtx::<std::convert::Infallible> {
        evo: EvoCtx {
            cons: &cons,
            check_sigs: case.world.sigs,
        },
        rules: SpecialTxRules {
            v24: case.world.v24,
            evo_shares: case.world.evo_shares,
        },
        list: &list,
        coins: &coins,
        net_info: NetInfoRules::new(params, mainnet(), CORE_NET_INFO),
        verifier: &SerialVerifier,
        quorum: Some(QuorumSeams {
            llmq,
            index: &world.index,
            commitments: &NoCommitments,
            quorums: &mut quorums,
            effects: &mut effects,
        }),
        unlock_indexes: case.world.block.then_some(&ranges as &dyn UnlockIndexes),
    };
    match check_special_tx_inner(tx, tx.payload(), &mut cx) {
        Ok(verdict) => Ok(verdict.err().map(|e| (e.result() as i32, e.reject_reason().to_owned()))),
        // Every seam of the world answers and the serial verifier always has a verdict.
        Err(_) => Err(divergence("check fault")),
    }
}

/// Core's verdict and ours on a type 7 or 9 `case`; `Ok(None)` when the oracle refuses the
/// context.
pub fn verdicts(oracle: &Oracle, case: &Case) -> Result<Option<Verdicts>, Divergence> {
    let world = QuorumWorld::of(&case.world).ok_or(divergence("quorum world"))?;
    let tx = case.tx();
    let mut raw = Vec::new();
    tx.raw().encode(&mut raw);
    let platform = case.tx_type() == TxType::AssetUnlock;
    let ctx = world.context(&case.world, &case.quorum, platform)?;
    let block = world.block(&case.world, &case.quorum, platform);
    let core =
        match oracle.check_special_tx_block(&world.chain(), &raw, &ctx, &block, case.world.sigs) {
            Ok(Verdict::Accept) => None,
            Ok(Verdict::Reject(r)) => {
                let ResultCode::Tx(class) = r.result else {
                    return Err(divergence("core result class"));
                };
                Some((class, r.reason))
            }
            Err(e) => {
                oracle_failure(&e)?;
                return Ok(None);
            }
        };
    Ok(Some(Verdicts {
        core,
        ours: ours(world, case, &tx)?,
    }))
}

// ---- signing -----------------------------------------------------------------------------------

/// The key material that signs for the quorum `hash` names, of type `platform`: its own, or
/// [`STRANGER`]'s when it names none of the world's; `forged`, another key in either case (so a
/// forged signature differs from the one a case without a quorum carries).
fn signer(world: &QuorumWorld, platform: bool, hash: &QuorumHash, forged: bool) -> u8 {
    match (world.quorum_named(platform, hash), forged) {
        (Some(i), false) => QUORUMS[i].seed,
        (Some(_), true) | (None, false) => STRANGER,
        (None, true) => STRANGER + 1,
    }
}

/// Signs a type 7 or 9 payload as Core verifies it: an MNHF signal over the txid with a null
/// signature (`mnhftx.cpp:139-153`, always in the basic encoding), an asset unlock over the whole
/// serialization with a null signature (`assetlocktx.cpp:222-229`, in the global flag's).
pub(super) fn sign(case: &mut Case, unsigned: &Tx) {
    let Some(world) = QuorumWorld::of(&case.world) else {
        return;
    };
    let forged = case.forged;
    match &mut case.payload {
        Payload::Mnhf(p) => {
            let msg = sha256d(&p.sig_hash_bytes(unsigned.raw()));
            let hash =
                sign_hash(world.mnhf_type(), &p.quorum_hash, p.request_id().as_bytes(), &msg);
            let seed = signer(world, false, &p.quorum_hash, forged);
            let Some(bytes) = sign_bytes(seed, &hash, world.legacy) else {
                return;
            };
            let scheme = if world.legacy {
                BlsSchemeKind::Legacy
            } else {
                BlsSchemeKind::Basic
            };
            // The payload writes it in the basic encoding whatever the flag.
            if let Ok(sig) = BlsEagerSig::decode(&bytes, BlsSchemeSource::Version(scheme)) {
                p.sig = sig;
            }
        }
        Payload::Unlock(p) => {
            let msg = sha256d(&p.sig_hash_bytes(unsigned.raw()));
            let hash =
                sign_hash(world.platform_type(), &p.quorum_hash, p.request_id().as_bytes(), &msg);
            let seed = signer(world, true, &p.quorum_hash, forged);
            if let Some(sig) =
                sign_bytes(seed, &hash, world.legacy).and_then(|b| BlsGlobalSig::decode(&b).ok())
            {
                p.quorum_sig = sig;
            }
        }
        _ => {}
    }
}

/// A signature the seeds start from, before signing.
pub(super) fn placeholder_sig() -> Option<[u8; 96]> {
    sign_bytes(STRANGER, &[0; 32], false)
}

pub(super) fn mnhf_seed(world: &World, version_bit: u8) -> Option<MnHardFork> {
    let q = QuorumWorld::of(world)?;
    Some(MnHardFork {
        version: 1,
        version_bit,
        quorum_hash: q.quorum_hash(MNHF),
        sig: BlsEagerSig::decode(
            &placeholder_sig()?,
            BlsSchemeSource::Version(BlsSchemeKind::Basic),
        )
        .ok()?,
    })
}

pub(super) fn unlock_seed(
    world: &World,
    version: u8,
    quorum: usize,
    requested_height: u32,
    index: u64,
) -> Option<AssetUnlock> {
    let q = QuorumWorld::of(world)?;
    Some(AssetUnlock {
        version,
        index,
        fee: 1_000,
        requested_height,
        quorum_hash: q.quorum_hash(quorum),
        quorum_sig: BlsGlobalSig::decode(&placeholder_sig()?).ok()?,
    })
}

/// The type a case of this module is.
pub(super) fn is_quorum_type(tx_type: TxType) -> bool {
    matches!(tx_type, TxType::MnhfSignal | TxType::AssetUnlock)
}

// ---- seeds and mutations -----------------------------------------------------------------------

/// The bits a seed signals: the first no deployment uses (`IsValidMNActivation` holds for it),
/// then each EHF deployment's.
fn seed_bits(params: &ChainParams) -> Vec<u8> {
    let deployments = &params.consensus.deployments;
    let free = (0u8..).find(|b| deployments.iter().all(|d| d.bit != i32::from(*b)));
    free.into_iter()
        .chain(deployments.iter().filter(|d| d.use_ehf).filter_map(|d| u8::try_from(d.bit).ok()))
        .collect()
}

/// A bit a deployment without `useEHF` uses, inside its window at the tip's median time past.
fn non_ehf_bit(world: &QuorumWorld) -> Option<u8> {
    let d = (world.params.consensus.deployments.iter())
        .find(|d| !d.use_ehf && d.start_time <= world.mtp && world.mtp < d.timeout)?;
    u8::try_from(d.bit).ok()
}

/// The seeds' variants of type 7 and 9. A variant byte at or above the count is read modulo it,
/// so that a fuzzed byte names a seed.
pub(super) const MNHF_VARIANTS: u8 = 3;
pub(super) const UNLOCK_VARIANTS: u8 = 12;

/// The requested heights an unlock seed takes: the tip, and inside the window below it.
const REQUESTED: [u32; 3] = [TIP, TIP - 20, TIP - 47];

/// Variant `v`: an MNHF signal for the `v`-th of [`seed_bits`], against the MNHF quorum.
pub(super) fn seed_mnhf(world: World, variant: u8) -> Option<Case> {
    let variant = variant % MNHF_VARIANTS;
    let q = QuorumWorld::of(&world)?;
    let bit = *seed_bits(&q.params).get(usize::from(variant))?;
    let payload = mnhf_seed(&world, bit)?;
    Some(Case::new(world, Payload::Mnhf(payload), vec![], vec![]))
}

/// Variant `v`: an asset unlock of version 1, or 2 under V24 (bit 0), against the older or
/// newer platform quorum (bit 1), requested at one of [`REQUESTED`] (`v / 4`), with 1 to 3
/// withdrawals.
pub(super) fn seed_unlock(world: World, variant: u8) -> Option<Case> {
    let variant = variant % UNLOCK_VARIANTS;
    let version = 1 + (variant & 1);
    if version == 2 && !world.v24 {
        return None;
    }
    let quorum = if variant & 2 == 0 {
        PLATFORM_OLD
    } else {
        PLATFORM_NEW
    };
    let requested = *REQUESTED.get(usize::from(variant / 4))?;
    let payload = unlock_seed(&world, version, quorum, requested, 7 + u64::from(variant))?;
    let outputs = (0..=usize::from(variant % 3))
        .map(|i| super::out(10_000 + i as i64, super::p2pkh(super::id(9, i as u8))))
        .collect();
    Some(Case::new(world, Payload::Unlock(payload), vec![], outputs))
}

impl Case {
    pub(super) fn mnhf(&mut self) -> Option<&mut MnHardFork> {
        match &mut self.payload {
            Payload::Mnhf(p) => Some(p),
            _ => None,
        }
    }

    pub(super) fn unlock(&mut self) -> Option<&mut AssetUnlock> {
        match &mut self.payload {
            Payload::Unlock(p) => Some(p),
            _ => None,
        }
    }

    fn quorum_world(&self) -> Option<&'static QuorumWorld> {
        QuorumWorld::of(&self.world)
    }
}

/// A payload Core's decode refuses: a byte after it, or its last byte dropped.
fn short_or_long(c: &mut Case, arg: u8) -> Option<Option<u8>> {
    if arg & 1 == 0 {
        c.trailing = Some(arg >> 1);
    } else {
        c.cut = true;
    }
    super::DONE
}

/// A hash no header of the world has.
fn unknown_hash(arg: u8) -> QuorumHash {
    QuorumHash::from([0xa0 | arg & 0x0f; 32])
}

use super::{DONE, Mutation, m};
use dash_dml::specialtx::reasons::*;

/// `MNHFTxPayload::IsTriviallyValid` (§3.12).
// Core: evo/mnhftx.cpp:94-104
pub(super) const MNHF_TRIVIAL: &[Mutation] = &[
    m("7t:1", &[BAD_MNHF_VERSION], |c, arg| {
        let p = c.mnhf()?;
        p.version = match arg % 3 {
            0 => 0,
            1 => 2,
            _ => 2u8.saturating_add(arg),
        };
        DONE
    }),
    m("7t:2", &[BAD_MNHF_NBIT_OUT_OF_BOUNDS], |c, arg| {
        let num_bits = u8::try_from(dash_consensus::deployments::VERSIONBITS_NUM_BITS).ok()?;
        c.mnhf()?.version_bit = num_bits.saturating_add(arg % 8);
        DONE
    }),
];

/// `CheckMNHFTx` (§3.12) past its type check, in Core's order.
// Core: evo/mnhftx.cpp:107-157
pub(super) const MNHF_CHECK: &[Mutation] = &[
    m("7c:2", &[BAD_MNHF_PAYLOAD], |c, arg| {
        c.mnhf()?;
        short_or_long(c, arg)
    }),
    m("7c:4", &[BAD_MNHF_NON_EHF], |c, _| {
        let bit = non_ehf_bit(c.quorum_world()?)?;
        c.mnhf()?.version_bit = bit;
        DONE
    }),
    m("7c:5a", &[BAD_MNHF_QUORUM_HASH], |c, arg| {
        c.mnhf()?.quorum_hash = unknown_hash(arg);
        DONE
    }),
    m("7c:5b", &[BAD_MNHF_QUORUM_HASH], |c, arg| {
        let hash = c.quorum_world()?.fork_hash(usize::from(arg & 1));
        c.mnhf()?.quorum_hash = hash;
        DONE
    }),
    // Not mined, or a block on the chain with no MNHF quorum: a platform quorum's, which the node
    // holds.
    m("7c:7", &[BAD_MNHF_MISSING_QUORUM], |c, arg| {
        let hash = c.quorum_world()?.quorum_hash(PLATFORM_OLD);
        let p = c.mnhf()?;
        if arg & 1 == 0 {
            c.quorum.unmined |= 1 << MNHF;
        } else {
            p.quorum_hash = hash;
            c.quorum.others = true;
        }
        DONE
    }),
    m("7c:8", &[BAD_MNHF_INVALID], |c, _| {
        c.mnhf()?;
        c.forged = true;
        DONE
    }),
];

/// `CheckAssetUnlockTx` and `VerifyAssetUnlockSig` (§3.12) past the type check, in Core's order.
// Core: evo/assetlocktx.cpp:106-153, 177-232
pub(super) const UNLOCK_CHECK: &[Mutation] = &[
    m("9c:2", &[BAD_ASSETUNLOCKTX_HAVE_INPUT], |c, arg| {
        c.unlock()?;
        c.inputs.extend(super::funding(1 + usize::from(arg % 2)));
        DONE
    }),
    m("9c:3", &[BAD_ASSETUNLOCKTX_TOO_MANY_OUTS], |c, arg| {
        c.unlock()?;
        let max = dash_dml::specialtx::asset::MAXIMUM_WITHDRAWALS;
        let more = max + 1 + usize::from(arg % 3) - c.outputs.len().min(max);
        for i in 0..more {
            c.outputs.push(super::out(20_000 + i as i64, super::p2pkh(super::id(10, i as u8))));
        }
        DONE
    }),
    m("9c:4", &[BAD_ASSETUNLOCKTX_PAYLOAD], |c, arg| {
        c.unlock()?;
        short_or_long(c, arg)
    }),
    m("9c:5", &[BAD_ASSETUNLOCKTX_VERSION], |c, arg| {
        c.unlock()?.version = match arg % 3 {
            0 => 0,
            1 => 3,
            _ => 3u8.saturating_add(arg),
        };
        DONE
    }),
    m("9c:6", &[BAD_ASSETUNLOCKTX_VERSION_2], |c, _| {
        if c.world.v24 {
            return None;
        }
        c.unlock()?.version = 2;
        DONE
    }),
    // The block path only: the mempool path passes no indexes.
    m("9c:7", &[BAD_ASSETUNLOCK_DUPLICATED_INDEX], |c, arg| {
        if !c.world.block {
            return None;
        }
        let index = c.unlock()?.index;
        c.quorum.indexes.push(index);
        if arg & 1 != 0 {
            c.quorum.indexes.push(index.wrapping_add(1));
        }
        c.quorum.indexes.sort_unstable();
        c.quorum.indexes.dedup();
        DONE
    }),
    m("9c:8", &[BAD_ASSETUNLOCK_QUORUM_HASH], |c, arg| {
        c.unlock()?.quorum_hash = unknown_hash(arg);
        DONE
    }),
    // A known block whose platform quorum the scan does not return: off the chain, on it with
    // none, or one the node did not mine.
    m("9c:11", &[BAD_ASSETUNLOCK_TOO_OLD_QUORUM], |c, arg| {
        let q = c.quorum_world()?;
        let hash = match arg % 3 {
            0 => q.fork_hash(usize::from(arg >> 2 & 1)),
            1 => q.hash(30 + u32::from(arg >> 2 & 7)),
            _ => {
                let p = c.unlock()?;
                let i = q.quorum_named(true, &p.quorum_hash)?;
                c.quorum.unmined |= 1 << i;
                return DONE;
            }
        };
        c.unlock()?.quorum_hash = hash;
        DONE
    }),
    // Requested above `pindexPrev`, or at least 48 blocks below it.
    m("9c:12", &[BAD_ASSETUNLOCK_TOO_LATE], |c, arg| {
        let expiring = dash_dml::specialtx::asset::HEIGHT_DIFF_EXPIRING;
        c.unlock()?.requested_height = if arg & 1 == 0 {
            TIP + 1 + u32::from(arg >> 1)
        } else {
            (TIP - expiring).saturating_sub(u32::from(arg >> 1))
        };
        DONE
    }),
    m("9c:14", &[BAD_ASSETUNLOCK_NOT_VERIFIED], |c, _| {
        c.unlock()?;
        c.forged = true;
        DONE
    }),
];
