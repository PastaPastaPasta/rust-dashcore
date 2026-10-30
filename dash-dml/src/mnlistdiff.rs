//! The `mnlistdiff` message (Core `CSimplifiedMNListDiff`, `evo/smldiff.{h,cpp}`): its builder
//! over two masternode lists and the caller's quorum and block data, and its serialization in
//! every protocol band a peer can negotiate.
//!
//! The masternode half is Core's `BuildSimplifiedDiff` in Core's iteration order (quirk 41). The
//! quorum half reads what LQ2's block processor and the block store know through
//! [`MnListDiffSource`]; resolving the request's block hashes stays with the server, which checks
//! what it finds with [`check_request`] and then hands the builder the two lists and the target
//! block, all from one chain view. Decisions: `docs/node-program/designs/EV4-serve-notes.md`
//! D5–D8, D16–D18.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use dash_consensus::hash::sha256d;
use dash_consensus::primitives::{CoreWrite, write_compact_size};
use dash_evo::sml::{SmlEntry, write_net_info};
use dash_primitives::BlockHash;

use crate::{MnList, ProTxHash};

/// `BLS_SCHEME_PROTO_VERSION`: from here up to [`MNLISTDIFF_VERSION_ORDER`] the diff carries its
/// `nVersion` after `cbTx`.
pub const BLS_SCHEME_PROTO_VERSION: i32 = 70225;
/// `DMN_TYPE_PROTO_VERSION`: entries carry their type fields from here.
pub const DMN_TYPE_PROTO_VERSION: i32 = 70227;
/// `SMNLE_VERSIONED_PROTO_VERSION`: entries start with their `nVersion` from here.
pub const SMNLE_VERSIONED_PROTO_VERSION: i32 = 70228;
/// `MNLISTDIFF_VERSION_ORDER`: the diff starts with its `nVersion` from here.
pub const MNLISTDIFF_VERSION_ORDER: i32 = 70229;
/// `MNLISTDIFF_CHAINLOCKS_PROTO_VERSION`: the diff ends with `quorumsCLSigs` from here.
pub const MNLISTDIFF_CHAINLOCKS_PROTO_VERSION: i32 = 70230;

/// `CSimplifiedMNListDiff::CURRENT_VERSION`, the only `nVersion` Core writes.
const DIFF_VERSION: u16 = 1;

/// A BLS signature's 96 bytes; all zeros is Core's null (invalid) signature.
pub type BlsSignatureBytes = [u8; 96];

/// An `(LLMQType, quorumHash)` pair, ordered as Core's `std::set` orders it: by type, then by
/// the hash's bytes (`uint256::operator<` is a memcmp).
pub type QuorumKey = (u8, [u8; 32]);

/// One `quorumsCLSigs` entry: a signature and the indexes of the new quorums it covers.
pub type QuorumClSig = (BlsSignatureBytes, Vec<u16>);

/// A Core `CSimplifiedMNListDiff`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimplifiedMnListDiff {
    /// The base block hash as the peer sent it (null asks from genesis).
    pub base_block_hash: BlockHash,
    /// The target list's block hash (`to.GetBlockHash()`: null for a pre-DIP3 target).
    pub block_hash: BlockHash,
    /// The coinbase's merkle proof.
    pub cb_tx_merkle_tree: PartialMerkleTree,
    /// The target block's coinbase transaction, serialized.
    pub cb_tx: Vec<u8>,
    /// Masternodes in the base list and not in the target list, in the base list's order.
    pub deleted_mns: Vec<ProTxHash>,
    /// Masternodes new in the target list or whose entry changed, in the target list's order.
    pub mn_list: Vec<SmlEntry>,
    /// Quorums active at the base and not at the target.
    pub deleted_quorums: Vec<QuorumKey>,
    /// The mined commitments (serialized `CFinalCommitment`s) of the quorums active at the target
    /// and not at the base.
    pub new_quorums: Vec<Vec<u8>>,
    /// `quorumsCLSigs`: each ChainLock signature a new quorum's members were selected with, and
    /// the indexes into `new_quorums` it applies to, in Core's map order; empty before V20.
    pub quorums_cl_sigs: Vec<QuorumClSig>,
}

impl SimplifiedMnListDiff {
    /// The message payload as Core sends it to a peer whose common version is `proto_version`
    /// (`msgMaker(pfrom.GetCommonVersion())`).
    // Core: evo/smldiff.h:64 CSimplifiedMNListDiff SERIALIZE_METHODS
    pub fn encode(&self, proto_version: i32, w: &mut impl CoreWrite) {
        if proto_version >= MNLISTDIFF_VERSION_ORDER {
            w.write(&DIFF_VERSION.to_le_bytes());
        }
        w.write(self.base_block_hash.as_bytes());
        w.write(self.block_hash.as_bytes());
        self.cb_tx_merkle_tree.encode(w);
        w.write(&self.cb_tx);
        if (BLS_SCHEME_PROTO_VERSION..MNLISTDIFF_VERSION_ORDER).contains(&proto_version) {
            w.write(&DIFF_VERSION.to_le_bytes());
        }
        write_compact_size(self.deleted_mns.len() as u64, w);
        for pro_tx in &self.deleted_mns {
            w.write(pro_tx.as_bytes());
        }
        write_compact_size(self.mn_list.len() as u64, w);
        for entry in &self.mn_list {
            write_sml_entry(entry, proto_version, w);
        }
        write_compact_size(self.deleted_quorums.len() as u64, w);
        for (llmq_type, hash) in &self.deleted_quorums {
            w.write(&[*llmq_type]);
            w.write(hash);
        }
        write_compact_size(self.new_quorums.len() as u64, w);
        for commitment in &self.new_quorums {
            w.write(commitment);
        }
        if proto_version >= MNLISTDIFF_CHAINLOCKS_PROTO_VERSION {
            write_compact_size(self.quorums_cl_sigs.len() as u64, w);
            for (sig, indexes) in &self.quorums_cl_sigs {
                w.write(sig);
                write_compact_size(indexes.len() as u64, w);
                for index in indexes {
                    w.write(&index.to_le_bytes());
                }
            }
        }
    }

    /// [`SimplifiedMnListDiff::encode`] into a new buffer.
    pub fn to_bytes(&self, proto_version: i32) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(proto_version, &mut out);
        out
    }
}

/// An entry as `mnlistdiff` carries it to a peer at `proto_version`: its `nVersion` first from
/// [`SMNLE_VERSIONED_PROTO_VERSION`], and nothing after `isValid` below [`DMN_TYPE_PROTO_VERSION`].
/// From there on the body is [`SmlEntry::write_hashed`]'s.
// Core: evo/simplifiedmns.h:72 CSimplifiedMNListEntry SERIALIZE_METHODS (SER_NETWORK)
pub fn write_sml_entry(entry: &SmlEntry, proto_version: i32, w: &mut impl CoreWrite) {
    if proto_version >= SMNLE_VERSIONED_PROTO_VERSION {
        w.write(&entry.version.to_le_bytes());
    }
    if proto_version >= DMN_TYPE_PROTO_VERSION {
        entry.write_hashed(w);
        return;
    }
    w.write(entry.pro_reg_tx_hash.as_bytes());
    w.write(entry.confirmed_hash.as_bytes());
    write_net_info(&entry.net_info, w);
    w.write(&entry.operator_key);
    w.write(&entry.voting_key_id);
    w.write(&[u8::from(entry.is_valid)]);
}

/// Core's `CPartialMerkleTree` (BIP37's partial merkle tree).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartialMerkleTree {
    /// `nTransactions`.
    pub transactions: u32,
    /// `vHash`, in depth-first order.
    pub hashes: Vec<[u8; 32]>,
    /// `vBits`, in depth-first order.
    pub bits: Vec<bool>,
}

impl PartialMerkleTree {
    /// `CPartialMerkleTree(vTxid, vMatch)` with only the first transaction (the coinbase)
    /// matched, as `BuildSimplifiedMNListDiff` builds it. `txids` is the block's, in block order
    /// and internal byte order; it is never empty for a block.
    // Core: merkleblock.cpp CPartialMerkleTree::CPartialMerkleTree; evo/smldiff.cpp:235-244
    pub fn coinbase_only(txids: &[[u8; 32]]) -> Self {
        let mut tree = PartialMerkleTree {
            transactions: txids.len() as u32,
            hashes: Vec::new(),
            bits: Vec::new(),
        };
        let mut height = 0;
        while tree.width(height) > 1 {
            height += 1;
        }
        tree.traverse_and_build(height, 0, txids);
        tree
    }

    /// `CalcTreeWidth`.
    fn width(&self, height: u32) -> u32 {
        (self.transactions + (1 << height) - 1) >> height
    }

    /// `TraverseAndBuild`; only position 0 matches, so a node is a parent of a match iff it is
    /// the leftmost of its level.
    fn traverse_and_build(&mut self, height: u32, pos: u32, txids: &[[u8; 32]]) {
        let parent_of_match = pos == 0;
        self.bits.push(parent_of_match);
        if height == 0 || !parent_of_match {
            let hash = self.calc_hash(height, pos, txids);
            self.hashes.push(hash);
        } else {
            self.traverse_and_build(height - 1, pos * 2, txids);
            if pos * 2 + 1 < self.width(height - 1) {
                self.traverse_and_build(height - 1, pos * 2 + 1, txids);
            }
        }
    }

    /// `CalcHash`: the node at `(height, pos)`, an odd level's last node paired with itself.
    fn calc_hash(&self, height: u32, pos: u32, txids: &[[u8; 32]]) -> [u8; 32] {
        if height == 0 {
            return txids[pos as usize];
        }
        let left = self.calc_hash(height - 1, pos * 2, txids);
        let right = if pos * 2 + 1 < self.width(height - 1) {
            self.calc_hash(height - 1, pos * 2 + 1, txids)
        } else {
            left
        };
        let mut both = [0u8; 64];
        both[..32].copy_from_slice(&left);
        both[32..].copy_from_slice(&right);
        sha256d(&both)
    }

    /// `nTransactions`, `vHash`, then `vBits` packed least significant bit first (`BitsToBytes`).
    // Core: merkleblock.h:88 CPartialMerkleTree SERIALIZE_METHODS
    pub fn encode(&self, w: &mut impl CoreWrite) {
        w.write(&self.transactions.to_le_bytes());
        write_compact_size(self.hashes.len() as u64, w);
        for hash in &self.hashes {
            w.write(hash);
        }
        let mut bytes = vec![0u8; self.bits.len().div_ceil(8)];
        for (i, bit) in self.bits.iter().enumerate() {
            bytes[i / 8] |= u8::from(*bit) << (i % 8);
        }
        write_compact_size(bytes.len() as u64, w);
        w.write(&bytes);
    }
}

/// `BuildSimplifiedDiff(from, to, extended = false)`: the masternodes `to` added or whose entry
/// changed, in `to`'s Core order, and the ones it dropped, in `from`'s Core order (quirk 41).
/// An entry changed iff any field Core's `CSimplifiedMNListEntry::operator==` compares differs,
/// which is every field of [`SmlEntry`].
// Core: evo/smldiff.cpp:121 BuildSimplifiedDiff
pub fn build_simplified_diff(from: &MnList, to: &MnList) -> (Vec<ProTxHash>, Vec<SmlEntry>) {
    let mut mn_list = Vec::new();
    for pro_tx in to.core_order().iter() {
        let dmn = to.get(pro_tx).expect("core_order holds the list's keys");
        // A shared `Dmn` never changes, so its entry is equal without projecting it.
        match from.get(pro_tx) {
            Some(old) if Arc::ptr_eq(old, dmn) => {}
            Some(old) => {
                let new = dmn.to_sml_entry();
                if old.to_sml_entry() != new {
                    mn_list.push(new);
                }
            }
            None => mn_list.push(dmn.to_sml_entry()),
        }
    }
    let deleted =
        from.core_order().iter().filter(|pro_tx| to.get(pro_tx).is_none()).copied().collect();
    (deleted, mn_list)
}

/// What `BuildQuorumChainlockInfo` reads from a new quorum's `CQuorum` object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuorumBase {
    /// The height of the quorum's base block, the block `quorumHash` names
    /// (`m_quorum_base_block_index->nHeight`).
    pub base_height: i32,
    /// The commitment's `quorumIndex`: 0 for commitment versions 1 and 3, which carry none.
    pub quorum_index: i16,
}

/// What the builder reads beyond the two lists (EV4-serve D6, D7). The node implements it over
/// LQ2's block processor and the block store, all read from the one chain view the request was
/// resolved in and the lists were read from (EV4-serve notes §2): Core holds `cs_main` from the
/// lookups to the reply (`net_processing.cpp` v24:5615-5622).
pub trait MnListDiffSource {
    /// A storage failure.
    type Error;

    /// `GetMinedAndActiveCommitmentsUntilBlock(block)` as a set of `(LLMQType, quorumHash)`.
    fn active_quorums(&self, block: &BlockHash) -> Result<BTreeSet<QuorumKey>, Self::Error>;

    /// `GetMinedCommitment(type, hash)`: the serialized `CFinalCommitment`, or `None` when no
    /// commitment was mined (`minedBlockHash == uint256::ZERO`).
    fn mined_commitment(&self, quorum: &QuorumKey) -> Result<Option<Vec<u8>>, Self::Error>;

    /// `GetQuorum(type, hash)`, read only from V20: the quorum's base height and index, or `None`
    /// when Core would have no quorum object (an unknown base block, `HasQuorum` false, or
    /// `BuildQuorumFromCommitment` failing), which fails the request.
    // Core: evo/smldiff.cpp:79-86; llmq/quorums.cpp CQuorumManager::GetQuorum
    fn quorum(&self, quorum: &QuorumKey) -> Result<Option<QuorumBase>, Self::Error>;

    /// `GetNonNullCoinbaseChainlock` of `block`'s ancestor at `height`, found by walking back
    /// from `block` (`GetAncestor`), never by the active chain's block at that height: the
    /// coinbase's `bestCLSignature` when it is valid and the CbTx qualifies. `None`, and the reply
    /// goes out with the null signature, for a negative height (no ancestor), a block before V20
    /// and any failure to read the work block's data (pruned or not): none of these is an error.
    /// `Err` is for failures outside that read (the block index, quorum storage).
    // Core: evo/smldiff.cpp:87-98; evo/cbtx.cpp:181-211
    fn coinbase_chainlock(
        &self,
        block: &BlockHash,
        height: i32,
    ) -> Result<Option<BlsSignatureBytes>, Self::Error>;

    /// `ReadBlockFromDisk(target)`: its coinbase and txids, or `None` when the read fails although
    /// the index says the data is on disk ([`MnListDiffError::BlockRead`], scored). Infallible
    /// otherwise: Core's read catches every failure of its own (open, I/O, decode, hash mismatch)
    /// and returns false, so each is `None`, never a storage error. The builder asks last, after
    /// the quorum half, as Core reads the block last: a refusal found earlier (an unscored one
    /// included) wins over a failed read.
    // Core: evo/smldiff.cpp:229-233; node/blockstorage.cpp:805-848
    fn target_block(&self, block: &BlockHash) -> Option<TargetBlock>;
}

/// What the reply carries of the target block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetBlock {
    /// The coinbase transaction, serialized.
    pub coinbase: Vec<u8>,
    /// Every txid, in block order (internal byte order).
    pub txids: Vec<[u8; 32]>,
}

/// One `getmnlistd` request, its hashes already resolved by the server.
#[derive(Clone, Copy, Debug)]
pub struct MnListDiffRequest<'a> {
    /// The base block hash as the peer sent it (echoed).
    pub requested_base: BlockHash,
    /// The base block (genesis when the request's base is null).
    pub base_block: BlockHash,
    /// The base block's list (`GetListForBlock(base)`).
    pub base_list: &'a MnList,
    /// The target block.
    pub block: BlockHash,
    /// The target block's list: its own block hash, or the null hash below DIP3 (Core's default
    /// list, `deterministicmns.cpp` v24:828-832), never an empty list at the target.
    pub list: &'a MnList,
    /// `DeploymentActiveAfter(block, V20)`: whether to build `quorumsCLSigs`.
    pub v20_active_after: bool,
}

/// What the block index knows of a request's block, for [`check_request`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockStatus {
    /// The block's height.
    pub height: i32,
    /// Whether the active chain contains it.
    pub in_active_chain: bool,
    /// Whether its data is on disk (`BLOCK_HAVE_DATA`).
    pub have_data: bool,
}

/// Why a `getmnlistd` request was not answered. Core answers none of these and treats them two
/// ways (`net_processing.cpp` v24:5622-5633, see [`MnListDiffError::peer_outcome`]): data this
/// node lacks is logged and dropped, anything else scores the peer. The `Display` text is Core's
/// `errorRet`.
#[derive(Debug, PartialEq, Eq)]
pub enum MnListDiffError<E> {
    /// The block index does not hold this base or target hash (a null target included).
    UnknownBlock(BlockHash),
    /// The base or the target is not in the active chain (a side branch, an invalid block); the
    /// base hash as the peer sent it.
    NotInSameChain {
        base: BlockHash,
        block: BlockHash,
    },
    /// The base is higher than the target.
    BaseHigherThanTarget {
        base: BlockHash,
        block: BlockHash,
    },
    /// The target's block data is not on disk (pruned, below an unvalidated snapshot base).
    BlockDataUnavailable(BlockHash),
    /// A list cannot be rebuilt from data this node has (Core's `BlockDataUnavailableError`).
    ListUnavailable(BlockHash),
    /// A quorum new at the target has no mined commitment.
    QuorumsDiff(QuorumKey),
    /// A quorum new at the target has no quorum object.
    QuorumChainlockInfo(QuorumKey),
    /// The target block could not be read although the index says its data is on disk.
    BlockRead(BlockHash),
    /// The node's own storage failed. Core's equivalents (a corrupt list, a failing database)
    /// throw out of the message handler, which logs them and scores no one.
    Source(E),
}

/// What Core does to the requesting peer after a refusal; it sends nothing either way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerOutcome {
    /// Logged and dropped: the request was plausible, the node lacks the data (Core's
    /// `BLOCK_DATA_UNAVAILABLE_SUFFIX` sentinel) or failed itself.
    Unscored,
    /// `Misbehaving(peer, score, ...)`.
    Misbehaving(i32),
}

impl<E> MnListDiffError<E> {
    /// What Core does to the peer: an error carrying the unavailable-data sentinel, or the
    /// node's own failure, is not the peer's; every other refusal scores 1.
    // Core: net_processing.cpp:5622-5633, :5940-5941; evo/smldiff.cpp:185-207,249-252
    pub fn peer_outcome(&self) -> PeerOutcome {
        match self {
            MnListDiffError::BlockDataUnavailable(_)
            | MnListDiffError::ListUnavailable(_)
            | MnListDiffError::Source(_) => PeerOutcome::Unscored,
            _ => PeerOutcome::Misbehaving(1),
        }
    }
}

/// Core's `BLOCK_DATA_UNAVAILABLE_SUFFIX`.
// Core: evo/deterministicmns.h:742-743
const BLOCK_DATA_UNAVAILABLE_SUFFIX: &str =
    "is not available (pruned or below an unvalidated snapshot base)";

/// A block hash in Core's `ToString()` (RPC) order.
struct Hex<'a>(&'a BlockHash);

impl fmt::Display for Hex<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.as_bytes().iter().rev().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

impl<E: fmt::Display> fmt::Display for MnListDiffError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Core: evo/smldiff.cpp:166,173
            MnListDiffError::UnknownBlock(block) => write!(f, "block {} not found", Hex(block)),
            // Core: evo/smldiff.cpp:178
            MnListDiffError::NotInSameChain {
                base,
                block,
            } => {
                write!(f, "block {} and {} are not in the same chain", Hex(base), Hex(block))
            }
            // Core: evo/smldiff.cpp:182
            MnListDiffError::BaseHigherThanTarget {
                base,
                block,
            } => {
                write!(f, "base block {} is higher then block {}", Hex(base), Hex(block))
            }
            // Core: evo/smldiff.cpp:191-192
            MnListDiffError::BlockDataUnavailable(block) => {
                write!(f, "block data for block {} {BLOCK_DATA_UNAVAILABLE_SUFFIX}", Hex(block))
            }
            // Core: evo/deterministicmns.cpp:878-880
            MnListDiffError::ListUnavailable(block) => write!(
                f,
                "CDeterministicMNManager::GetListForBlockInternal -- masternode list diff for \
                 block {} {BLOCK_DATA_UNAVAILABLE_SUFFIX}",
                Hex(block)
            ),
            // Core: evo/smldiff.cpp:217
            MnListDiffError::QuorumsDiff(_) => f.write_str("failed to build quorums diff"),
            // Core: evo/smldiff.cpp:223
            MnListDiffError::QuorumChainlockInfo(_) => {
                f.write_str("failed to build quorum chainlock info")
            }
            // Core: evo/smldiff.cpp:231
            MnListDiffError::BlockRead(block) => {
                write!(f, "failed to read block {} from disk", Hex(block))
            }
            MnListDiffError::Source(e) => e.fmt(f),
        }
    }
}

/// `BuildSimplifiedMNListDiff`'s checks before it reads a list, in Core's order: the base
/// (`None` when unknown; the genesis block's status when the peer sent a null base) and the
/// target must be known, both in the active chain, the base no higher than the target, and the
/// target's data on disk. The server looks the statuses up in the chain view it then builds from.
// Core: evo/smldiff.cpp:161-193
pub fn check_request<E>(
    requested_base: &BlockHash,
    base: Option<BlockStatus>,
    requested_block: &BlockHash,
    block: Option<BlockStatus>,
) -> Result<(), MnListDiffError<E>> {
    let base = base.ok_or(MnListDiffError::UnknownBlock(*requested_base))?;
    let block = block.ok_or(MnListDiffError::UnknownBlock(*requested_block))?;
    if !base.in_active_chain || !block.in_active_chain {
        return Err(MnListDiffError::NotInSameChain {
            base: *requested_base,
            block: *requested_block,
        });
    }
    if base.height > block.height {
        return Err(MnListDiffError::BaseHigherThanTarget {
            base: *requested_base,
            block: *requested_block,
        });
    }
    if !block.have_data {
        return Err(MnListDiffError::BlockDataUnavailable(*requested_block));
    }
    Ok(())
}

/// `BuildSimplifiedMNListDiff` after its block lookups ([`check_request`]) and list reads
/// (EV4-serve D5–D8): the list half, the quorum half, then the target block, in Core's order.
// Core: evo/smldiff.cpp:154 BuildSimplifiedMNListDiff
pub fn build_mnlistdiff<S: MnListDiffSource>(
    request: &MnListDiffRequest<'_>,
    source: &S,
) -> Result<SimplifiedMnListDiff, MnListDiffError<S::Error>> {
    debug_assert!(
        *request.list.block_hash() == request.block || request.list.is_default(),
        "the target list is neither the target's nor Core's default list"
    );
    let (deleted_mns, mn_list) = build_simplified_diff(request.base_list, request.list);

    // Core: evo/smldiff.cpp:31 CSimplifiedMNListDiff::BuildQuorumsDiff
    let base_quorums =
        source.active_quorums(&request.base_block).map_err(MnListDiffError::Source)?;
    let quorums = source.active_quorums(&request.block).map_err(MnListDiffError::Source)?;
    let deleted_quorums = base_quorums.difference(&quorums).copied().collect();
    let added: Vec<QuorumKey> = quorums.difference(&base_quorums).copied().collect();
    let mut new_quorums = Vec::new();
    for quorum in &added {
        let qc = source
            .mined_commitment(quorum)
            .map_err(MnListDiffError::Source)?
            .ok_or(MnListDiffError::QuorumsDiff(*quorum))?;
        new_quorums.push(qc);
    }

    // Core: evo/smldiff.cpp:221
    let quorums_cl_sigs = if request.v20_active_after {
        quorum_chainlock_info(&request.block, &added, source)?
    } else {
        Vec::new()
    };

    // Core: evo/smldiff.cpp:229-244
    let target =
        source.target_block(&request.block).ok_or(MnListDiffError::BlockRead(request.block))?;
    Ok(SimplifiedMnListDiff {
        base_block_hash: request.requested_base,
        block_hash: *request.list.block_hash(),
        cb_tx_merkle_tree: PartialMerkleTree::coinbase_only(&target.txids),
        cb_tx: target.coinbase,
        deleted_mns,
        mn_list,
        deleted_quorums,
        new_quorums,
        quorums_cl_sigs,
    })
}

/// `BuildQuorumChainlockInfo`: each new quorum's work block is the target's ancestor at
/// `baseHeight − quorumIndex − 8`; quorums group under that block's coinbase ChainLock (the null
/// signature when there is none), and groups with one signature merge. Core keys the map by
/// `CBLSSignature`, whose `operator<` compares `SerializeHash`es.
// Core: evo/smldiff.cpp:69 CSimplifiedMNListDiff::BuildQuorumChainlockInfo
fn quorum_chainlock_info<S: MnListDiffSource>(
    block: &BlockHash,
    added: &[QuorumKey],
    source: &S,
) -> Result<Vec<QuorumClSig>, MnListDiffError<S::Error>> {
    let mut by_work_height: BTreeMap<i32, BTreeSet<u16>> = BTreeMap::new();
    for (index, key) in added.iter().enumerate() {
        let quorum = source
            .quorum(key)
            .map_err(MnListDiffError::Source)?
            .ok_or(MnListDiffError::QuorumChainlockInfo(*key))?;
        let work_height = quorum.base_height - i32::from(quorum.quorum_index) - 8;
        by_work_height.entry(work_height).or_default().insert(index as u16);
    }
    let mut by_sig: BTreeMap<[u8; 32], (BlsSignatureBytes, BTreeSet<u16>)> = BTreeMap::new();
    for (height, indexes) in by_work_height {
        let sig = source
            .coinbase_chainlock(block, height)
            .map_err(MnListDiffError::Source)?
            .unwrap_or([0; 96]);
        by_sig.entry(sha256d(&sig)).or_insert_with(|| (sig, BTreeSet::new())).1.extend(indexes);
    }
    Ok(by_sig.into_values().map(|(sig, indexes)| (sig, indexes.into_iter().collect())).collect())
}

#[cfg(test)]
mod tests {
    use dash_consensus::merkle::compute_merkle_root;
    use dash_evo::cbtx;

    use super::*;
    use crate::testkit::numbered;

    fn list(range: std::ops::Range<u32>) -> MnList {
        let mut list = MnList::new(BlockHash::from([5; 32]), 2_000, 0);
        for i in range {
            list.try_add(numbered(i), true).unwrap();
        }
        list
    }

    /// `deletedMNs` follows the base's Core order and `mnList` the target's, which are not the
    /// proTxHash order (quirk 41).
    #[test]
    fn the_list_half_is_in_core_order() {
        let from = list(0..64);
        let mut to = list(32..96);
        let changed = numbered(40).pro_tx_hash;
        let mut hot = to.get(&changed).unwrap().hot;
        hot.ban_if_not_banned(2_001);
        to.update_hot(&changed, hot).unwrap();
        let (deleted, mn_list) = build_simplified_diff(&from, &to);

        let expected: Vec<ProTxHash> =
            from.core_order().iter().filter(|p| to.get(p).is_none()).copied().collect();
        assert_eq!(deleted, expected);
        assert_eq!(deleted.len(), 32);
        let mut sorted = deleted.clone();
        sorted.sort();
        assert_ne!(deleted, sorted, "the case must tell Core order from memcmp");

        let listed: Vec<ProTxHash> = mn_list.iter().map(|e| e.pro_reg_tx_hash).collect();
        let expected: Vec<ProTxHash> = to
            .core_order()
            .iter()
            .filter(|p| from.get(p).is_none() || **p == changed)
            .copied()
            .collect();
        assert_eq!(listed, expected);
        assert_eq!(listed.len(), 33);
        assert!(!mn_list.iter().find(|e| e.pro_reg_tx_hash == changed).unwrap().is_valid);
    }

    /// A source with one new quorum at the target, its commitment and quorum object optional,
    /// its storage and the target block read able to fail; it logs the calls it answers.
    #[derive(Default)]
    struct OneQuorum {
        commitment: bool,
        quorum: bool,
        storage_fails: bool,
        block: bool,
        calls: std::cell::RefCell<Vec<&'static str>>,
    }

    fn source(commitment: bool, quorum: bool) -> OneQuorum {
        OneQuorum {
            commitment,
            quorum,
            block: true,
            ..OneQuorum::default()
        }
    }

    const QUORUM: QuorumKey = (100, [9; 32]);

    impl MnListDiffSource for OneQuorum {
        type Error = String;
        fn active_quorums(&self, block: &BlockHash) -> Result<BTreeSet<QuorumKey>, String> {
            Ok(if *block == BlockHash::from([2; 32]) {
                [QUORUM].into()
            } else {
                BTreeSet::new()
            })
        }
        fn mined_commitment(&self, _: &QuorumKey) -> Result<Option<Vec<u8>>, String> {
            self.calls.borrow_mut().push("mined_commitment");
            if self.storage_fails {
                return Err("evo store".into());
            }
            Ok(self.commitment.then(|| vec![1, 2, 3]))
        }
        fn quorum(&self, _: &QuorumKey) -> Result<Option<QuorumBase>, String> {
            Ok(self.quorum.then_some(QuorumBase {
                base_height: 4,
                quorum_index: 2,
            }))
        }
        fn coinbase_chainlock(
            &self,
            block: &BlockHash,
            height: i32,
        ) -> Result<Option<BlsSignatureBytes>, String> {
            assert_eq!(*block, BlockHash::from([2; 32]), "the work block is the target's ancestor");
            assert_eq!(height, -6, "work height = base height - quorumIndex - 8");
            self.calls.borrow_mut().push("coinbase_chainlock");
            Ok(None)
        }
        fn target_block(&self, block: &BlockHash) -> Option<TargetBlock> {
            assert_eq!(*block, BlockHash::from([2; 32]));
            self.calls.borrow_mut().push("target_block");
            self.block.then(|| TargetBlock {
                coinbase: vec![],
                txids: vec![[0; 32]],
            })
        }
    }

    /// Core's two refusals, the V20 condition, and a pre-DIP3 target's null block hash.
    // Core: evo/smldiff.cpp:58-61, :79-86, :216-225
    #[test]
    fn the_quorum_half_fails_as_core_does() {
        let (base, target) = (MnList::default_pre_dip3(), MnList::default_pre_dip3());
        let request = |v20| pre_dip3_request(&base, &target, v20);

        let missing = build_mnlistdiff(&request(true), &source(false, true)).unwrap_err();
        assert_eq!(missing, MnListDiffError::QuorumsDiff(QUORUM));
        assert_eq!(missing.to_string(), "failed to build quorums diff");
        let no_object = build_mnlistdiff(&request(true), &source(true, false)).unwrap_err();
        assert_eq!(no_object, MnListDiffError::QuorumChainlockInfo(QUORUM));
        assert_eq!(no_object.to_string(), "failed to build quorum chainlock info");

        // Before V20 the quorum object is not read.
        let diff = build_mnlistdiff(&request(false), &source(true, false)).unwrap();
        assert_eq!(diff.new_quorums, [vec![1, 2, 3]]);
        assert!(diff.quorums_cl_sigs.is_empty());
        assert_eq!(diff.block_hash, BlockHash::from([0; 32]), "a pre-DIP3 list's hash is null");

        // A work block below genesis gives the null signature.
        let diff = build_mnlistdiff(&request(true), &source(true, true)).unwrap();
        assert_eq!(diff.quorums_cl_sigs, [([0; 96], vec![0])]);
    }

    /// The target block is read last, as Core reads it (`smldiff.cpp` v24:229-233, after
    /// :196-226): a failed read is reported only when nothing before it failed, so a storage
    /// failure (not scored) or a quorum refusal wins over it (Opus-h r1 minor 1, DEC-331).
    #[test]
    fn the_target_block_is_read_last() {
        let (base, target) = (MnList::default_pre_dip3(), MnList::default_pre_dip3());
        let request = pre_dip3_request(&base, &target, true);
        let unreadable = |commitment, quorum, storage_fails| OneQuorum {
            commitment,
            quorum,
            storage_fails,
            ..OneQuorum::default()
        };

        let source = unreadable(true, true, false);
        let read = build_mnlistdiff(&request, &source).unwrap_err();
        assert_eq!(read, MnListDiffError::BlockRead(BlockHash::from([2; 32])));
        assert_eq!(read.peer_outcome(), PeerOutcome::Misbehaving(1));
        assert_eq!(
            *source.calls.borrow(),
            ["mined_commitment", "coinbase_chainlock", "target_block"]
        );

        let source = unreadable(true, true, true);
        let storage = build_mnlistdiff(&request, &source).unwrap_err();
        assert_eq!(storage, MnListDiffError::Source("evo store".into()));
        assert_eq!(storage.peer_outcome(), PeerOutcome::Unscored);
        assert!(!source.calls.borrow().contains(&"target_block"));

        for (commitment, quorum, refusal) in [
            (false, true, MnListDiffError::QuorumsDiff(QUORUM)),
            (true, false, MnListDiffError::QuorumChainlockInfo(QUORUM)),
        ] {
            let source = unreadable(commitment, quorum, false);
            assert_eq!(build_mnlistdiff(&request, &source).unwrap_err(), refusal);
            assert!(!source.calls.borrow().contains(&"target_block"));
        }

        // Before V20 the quorum object is not read, and the failed read is reported.
        let request = pre_dip3_request(&base, &target, false);
        let source = unreadable(true, false, false);
        let read = build_mnlistdiff(&request, &source).unwrap_err();
        assert_eq!(read, MnListDiffError::BlockRead(BlockHash::from([2; 32])));
        assert_eq!(*source.calls.borrow(), ["mined_commitment", "target_block"]);
    }

    fn pre_dip3_request<'a>(
        base: &'a MnList,
        target: &'a MnList,
        v20: bool,
    ) -> MnListDiffRequest<'a> {
        MnListDiffRequest {
            requested_base: BlockHash::from([0; 32]),
            base_block: BlockHash::from([1; 32]),
            base_list: base,
            block: BlockHash::from([2; 32]),
            list: target,
            v20_active_after: v20,
        }
    }

    /// The request checks in Core's order with Core's texts, and each refusal's peer outcome:
    /// unavailable data and the node's own failures are not scored, the rest score 1. The
    /// texts and scores rc.3 gave on regtest are in `dash-coreimport/tests/evodb_serve.rs`.
    // Core: evo/smldiff.cpp:161-193; net_processing.cpp:5622-5633
    #[test]
    fn refusals_are_classified_as_core_scores_them() {
        type Error = MnListDiffError<String>;
        let (base, block) = (BlockHash::from([1; 32]), BlockHash::from([2; 32]));
        let status = |height, in_active_chain, have_data| {
            Some(BlockStatus {
                height,
                in_active_chain,
                have_data,
            })
        };
        let check = |b, t| check_request::<String>(&base, b, &block, t);

        assert_eq!(check(status(5, true, false), status(9, true, true)), Ok(()));
        assert_eq!(check(status(9, true, true), status(9, true, true)), Ok(()));
        assert_eq!(
            check(None, None),
            Err(Error::UnknownBlock(base)),
            "the base is looked up first"
        );
        assert_eq!(check(status(5, true, true), None), Err(Error::UnknownBlock(block)));
        let off_chain = Err(Error::NotInSameChain {
            base,
            block,
        });
        assert_eq!(check(status(9, false, true), status(5, true, false)), off_chain);
        assert_eq!(check(status(5, true, true), status(9, false, true)), off_chain);
        assert_eq!(
            check(status(9, true, true), status(5, true, false)),
            Err(Error::BaseHigherThanTarget {
                base,
                block
            })
        );
        assert_eq!(
            check(status(5, true, true), status(9, true, false)),
            Err(Error::BlockDataUnavailable(block))
        );

        let scored = PeerOutcome::Misbehaving(1);
        for (error, outcome) in [
            (Error::UnknownBlock(block), scored),
            (
                Error::NotInSameChain {
                    base,
                    block,
                },
                scored,
            ),
            (
                Error::BaseHigherThanTarget {
                    base,
                    block,
                },
                scored,
            ),
            (Error::BlockDataUnavailable(block), PeerOutcome::Unscored),
            (Error::ListUnavailable(block), PeerOutcome::Unscored),
            (Error::QuorumsDiff(QUORUM), scored),
            (Error::QuorumChainlockInfo(QUORUM), scored),
            (Error::BlockRead(block), scored),
            (Error::Source("disk".into()), PeerOutcome::Unscored),
        ] {
            assert_eq!(error.peer_outcome(), outcome, "{error}");
            // Core tells them apart by the sentinel in the text alone.
            let sentinel = error.to_string().contains(BLOCK_DATA_UNAVAILABLE_SUFFIX);
            assert_eq!(
                sentinel,
                matches!(error, Error::BlockDataUnavailable(_) | Error::ListUnavailable(_))
            );
        }
        let hex = "0202020202020202020202020202020202020202020202020202020202020202";
        assert_eq!(
            Error::BlockDataUnavailable(block).to_string(),
            format!(
                "block data for block {hex} is not available (pruned or below an unvalidated snapshot base)"
            )
        );
    }

    /// The coinbase-only tree of 1 to 33 transactions reads back, through SB7's
    /// `ExtractMatches`, as the block's merkle root with the coinbase as its one match.
    #[test]
    fn the_coinbase_only_tree_proves_the_coinbase() {
        for n in 1..=33u32 {
            let txids: Vec<[u8; 32]> = (0..n).map(|i| sha256d(&i.to_le_bytes())).collect();
            let tree = PartialMerkleTree::coinbase_only(&txids);
            let mut bytes = Vec::new();
            tree.encode(&mut bytes);
            let flags_at = 4 + 1 + 32 * tree.hashes.len() + 1;
            let wire = cbtx::PartialMerkleTree {
                transactions: tree.transactions,
                hashes: tree.hashes.clone(),
                flags: bytes[flags_at..].to_vec(),
            };
            let matches = wire.extract_matches().unwrap_or_else(|| panic!("{n} transactions"));
            assert_eq!(matches.root, compute_merkle_root(txids.clone()).0, "{n} transactions");
            assert_eq!(matches.matches, [(txids[0], 0)], "{n} transactions");
        }
    }
}
