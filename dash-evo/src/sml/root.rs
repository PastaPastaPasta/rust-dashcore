//! The CbTx's two list roots: `merkleRootMNList` over the simplified masternode list and
//! `merkleRootQuorums` over the active quorum commitments.

use std::collections::{BTreeMap, BTreeSet};

use dash_consensus::hash::sha256d_64_many;
use dash_consensus::merkle::compute_merkle_root;
use dash_consensus::validation::{BlockValidationError, BlockValidationResult};
use dash_primitives::{LlmqType, MerkleRoot};
use dash_types::Numeric;

use super::entry::SmlEntry;
use crate::types::ProTxHash;

/// Core's `CSimplifiedMNList::CalcMerkleRoot` behind `CalcCbTxMerkleRootMNList`, computed in full:
/// the entries' hashes in proTxHash memcmp order, then `ComputeMerkleRoot`. An empty list gives
/// the zero root.
///
/// The reference for [`SmlList::merkle_root`], which caches the tree. Entries must have distinct
/// proTxHashes, as a list's do.
// Core: evo/simplifiedmns.cpp:73 CSimplifiedMNList (sort), :81 CalcMerkleRoot, :102 CalcCbTxMerkleRootMNList
pub fn merkle_root_mn_list<'a>(
    entries: impl IntoIterator<Item = &'a SmlEntry>,
) -> Result<MerkleRoot, BlockValidationError> {
    let mut sorted: Vec<&SmlEntry> = entries.into_iter().collect();
    sorted.sort_unstable_by_key(|entry| entry.pro_reg_tx_hash);
    merkle_root_mn_list_from_leaves(sorted.iter().map(|entry| entry.hash().to_lendian()).collect())
}

/// [`merkle_root_mn_list`] from the entries' hashes, already in proTxHash memcmp order.
// Core: evo/simplifiedmns.cpp:81 CalcMerkleRoot
pub fn merkle_root_mn_list_from_leaves(
    leaves: Vec<[u8; 32]>,
) -> Result<MerkleRoot, BlockValidationError> {
    checked_root(compute_merkle_root(leaves), MUTATED_MN_LIST)
}

// Core: evo/simplifiedmns.cpp:138. Core's process-wide cache of the last list gives
// "mutated-cached-calc-cb-mnmerkleroot" for a repeat of a mutated list; the result is the same.
const MUTATED_MN_LIST: &str = "mutated-calc-cb-mnmerkleroot";

/// `ComputeMerkleRoot`'s result as a root, or `reason` when the tree is mutated.
fn checked_root(
    (root, mutated): ([u8; 32], bool),
    reason: &'static str,
) -> Result<MerkleRoot, BlockValidationError> {
    if mutated {
        return Err(invalid(reason));
    }
    Ok(MerkleRoot::from_lendian(root))
}

/// A simplified masternode list (Core's `CSimplifiedMNList`): its entries in proTxHash memcmp
/// order, with the merkle tree over their hashes cached between roots.
///
/// [`SmlList::merkle_root`] rehashes only what changed since the last root: the changed leaves
/// and their paths when every change replaced an entry, and after an insertion or removal every
/// node over the leaves from the first that moved. It equals [`merkle_root_mn_list`] over the same entries.
#[derive(Clone, Debug)]
pub struct SmlList {
    /// Sorted by `pro_reg_tx_hash`, which is unique.
    entries: Vec<SmlEntry>,
    tree: MerkleCache,
}

impl Default for SmlList {
    fn default() -> Self {
        SmlList::new()
    }
}

impl SmlList {
    /// The empty list.
    pub fn new() -> Self {
        SmlList {
            entries: Vec::new(),
            tree: MerkleCache::new(Vec::new()),
        }
    }

    /// The list of `entries`; a later entry replaces an earlier one with the same proTxHash.
    pub fn from_entries(entries: impl IntoIterator<Item = SmlEntry>) -> Self {
        // `extend` inserts in order, so the later entry wins (`collect` does not promise which).
        let mut by_hash = BTreeMap::new();
        by_hash.extend(entries.into_iter().map(|entry| (entry.pro_reg_tx_hash, entry)));
        let entries: Vec<SmlEntry> = by_hash.into_values().collect();
        let leaves = entries.iter().map(|entry| entry.hash().to_lendian()).collect();
        SmlList {
            entries,
            tree: MerkleCache::new(leaves),
        }
    }

    /// The entries in proTxHash memcmp order.
    pub fn entries(&self) -> &[SmlEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entry with this proTxHash.
    pub fn get(&self, pro_reg_tx_hash: &ProTxHash) -> Option<&SmlEntry> {
        self.position(pro_reg_tx_hash).ok().map(|i| &self.entries[i])
    }

    /// Adds `entry`, or replaces the entry with its proTxHash.
    pub fn upsert(&mut self, entry: SmlEntry) {
        let leaf = entry.hash().to_lendian();
        match self.position(&entry.pro_reg_tx_hash) {
            Ok(i) => {
                self.entries[i] = entry;
                self.tree.replace(i, leaf);
            }
            Err(i) => {
                self.entries.insert(i, entry);
                self.tree.insert(i, leaf);
            }
        }
    }

    /// Removes and returns the entry with this proTxHash.
    pub fn remove(&mut self, pro_reg_tx_hash: &ProTxHash) -> Option<SmlEntry> {
        let i = self.position(pro_reg_tx_hash).ok()?;
        self.tree.remove(i);
        Some(self.entries.remove(i))
    }

    /// `merkleRootMNList` of the list: [`merkle_root_mn_list`]'s value, from the cached tree.
    // Core: evo/simplifiedmns.cpp:81 CalcMerkleRoot, :102 CalcCbTxMerkleRootMNList
    pub fn merkle_root(&mut self) -> Result<MerkleRoot, BlockValidationError> {
        checked_root(self.tree.root(), MUTATED_MN_LIST)
    }

    fn position(&self, pro_reg_tx_hash: &ProTxHash) -> Result<usize, usize> {
        self.entries.binary_search_by(|entry| entry.pro_reg_tx_hash.cmp(pro_reg_tx_hash))
    }
}

impl PartialEq for SmlList {
    /// Core's `CSimplifiedMNList::operator==`: the same entries in the same order.
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

impl Eq for SmlList {}

/// Every level of `ComputeMerkleRoot`'s tree over a list's leaves, kept between roots.
///
/// `levels[0]` holds the leaves and each next level the parents of the one below, without the
/// duplicate Core appends to an odd level; the top level holds the root (none for no leaves).
#[derive(Clone, Debug)]
struct MerkleCache {
    levels: Vec<Vec<[u8; 32]>>,
    /// Leaves replaced since the last root.
    dirty: BTreeSet<usize>,
    /// The first leaf inserted or removed since the last root: every node over the leaves from
    /// there on is stale.
    reshaped_from: Option<usize>,
    /// `(level, pair)` for each pair of real siblings that are equal: Core's `mutated`.
    equal_pairs: BTreeSet<(usize, usize)>,
}

impl MerkleCache {
    fn new(leaves: Vec<[u8; 32]>) -> Self {
        MerkleCache {
            levels: vec![leaves],
            dirty: BTreeSet::new(),
            reshaped_from: Some(0),
            equal_pairs: BTreeSet::new(),
        }
    }

    fn replace(&mut self, i: usize, leaf: [u8; 32]) {
        if self.levels[0][i] != leaf {
            self.levels[0][i] = leaf;
            self.dirty.insert(i);
        }
    }

    fn insert(&mut self, i: usize, leaf: [u8; 32]) {
        self.levels[0].insert(i, leaf);
        self.reshape_at(i);
    }

    fn remove(&mut self, i: usize) {
        self.levels[0].remove(i);
        self.reshape_at(i);
    }

    fn reshape_at(&mut self, i: usize) {
        self.reshaped_from = Some(self.reshaped_from.map_or(i, |from| from.min(i)));
    }

    /// The root and Core's `mutated` flag (CVE-2012-2459), as `ComputeMerkleRoot` gives them.
    fn root(&mut self) -> ([u8; 32], bool) {
        if let Some(from) = self.reshaped_from {
            // A replaced leaf left of the first move is still at its position; one right of it is
            // inside the rebuilt part.
            let first_dirty = self.dirty.first().copied().unwrap_or(from);
            self.rebuild_from(from.min(first_dirty));
        } else if !self.dirty.is_empty() {
            self.update();
        }
        let root = self.levels.last().and_then(|top| top.first()).copied().unwrap_or([0; 32]);
        (root, !self.equal_pairs.is_empty())
    }

    /// Rehashes every node over the leaves from `start` on, and keeps those left of it: their
    /// leaves neither moved nor changed. A level's first stale node is half the one below's, and
    /// a node the level gained or lost is right of it.
    // Core: consensus/merkle.cpp:45 ComputeMerkleRoot, level by level
    fn rebuild_from(&mut self, start: usize) {
        self.dirty.clear();
        self.reshaped_from = None;
        let (mut level, mut start) = (0, start);
        while self.levels[level].len() > 1 {
            if self.levels.len() == level + 1 {
                self.levels.push(Vec::new());
            }
            let (lower, upper) = self.levels.split_at_mut(level + 1);
            let (below, above) = (&lower[level], &mut upper[0]);
            // Only a level pushed just now is shorter, and there `start / 2` is already 0.
            let first = (start / 2).min(above.len());
            let stale = self.equal_pairs.split_off(&(level, first));
            self.equal_pairs.extend(stale.into_iter().filter(|&(l, _)| l != level));
            for pair in first..below.len() / 2 {
                if below[2 * pair] == below[2 * pair + 1] {
                    self.equal_pairs.insert((level, pair));
                }
            }
            above.truncate(first);
            above.extend(hash_pairs(below, first..below.len().div_ceil(2)));
            start = first;
            level += 1;
        }
        self.levels.truncate(level + 1);
        self.equal_pairs.split_off(&(level, 0));
    }

    /// Rehashes the paths from the replaced leaves to the root. The shape is unchanged, so every
    /// parent keeps its position.
    fn update(&mut self) {
        let mut changed: Vec<usize> = std::mem::take(&mut self.dirty).into_iter().collect();
        for level in 0..self.levels.len() - 1 {
            let mut pairs: Vec<usize> = changed.iter().map(|i| i / 2).collect();
            pairs.dedup();
            let below = &self.levels[level];
            for &pair in &pairs {
                let equal = below.get(2 * pair + 1).is_some_and(|right| *right == below[2 * pair]);
                if equal {
                    self.equal_pairs.insert((level, pair));
                } else {
                    self.equal_pairs.remove(&(level, pair));
                }
            }
            let parents = hash_pairs(below, pairs.iter().copied());
            for (&pair, parent) in pairs.iter().zip(parents) {
                self.levels[level + 1][pair] = parent;
            }
            changed = pairs;
        }
    }
}

/// The parent of each pair in `pairs` (ascending), the last node of an odd level paired with
/// itself, through Core's batched `SHA256D64`.
fn hash_pairs(level: &[[u8; 32]], pairs: impl IntoIterator<Item = usize>) -> Vec<[u8; 32]> {
    let inputs: Vec<[u8; 64]> = pairs
        .into_iter()
        .map(|pair| {
            let left = level[2 * pair];
            let right = level.get(2 * pair + 1).copied().unwrap_or(left);
            let mut both = [0; 64];
            both[..32].copy_from_slice(&left);
            both[32..].copy_from_slice(&right);
            both
        })
        .collect();
    let mut outputs = vec![[0; 32]; inputs.len()];
    sha256d_64_many(&mut outputs, &inputs);
    outputs
}

/// What `CalcCbTxMerkleRootQuorums` needs to know about one LLMQ type at the previous block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuorumRootParams {
    /// `signingActiveQuorumCount`.
    pub signing_active_quorum_count: usize,
    /// `IsQuorumRotationEnabled(params, pindexPrev)`.
    pub rotation: bool,
}

/// The active commitment hashes at the previous block (Core's `GetQcHashes(pindexPrev)`): per
/// type, the non-rotated ones newest first (`QcHashMap`) and the rotated ones by quorum index
/// (`QcIndexedHashMap`). Each hash is `SerializeHash` of the commitment. Where `GetQcHashes`
/// fails, the caller rejects with Core's "commitment-not-found" instead.
///
/// Keyed by the type's number, as Core's `std::map<Consensus::LLMQType, …>` is ordered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QcHashes {
    pub plain: BTreeMap<u8, Vec<[u8; 32]>>,
    pub indexed: BTreeMap<u8, BTreeMap<i16, [u8; 32]>>,
}

impl QcHashes {
    /// Adds a non-rotated hash after those of its type already held (older ones go last).
    pub fn push_plain(&mut self, llmq_type: LlmqType, hash: [u8; 32]) {
        self.plain.entry(llmq_type.to_base()).or_default().push(hash);
    }

    /// Sets the rotated hash at `quorum_index`.
    pub fn insert_indexed(&mut self, llmq_type: LlmqType, quorum_index: i16, hash: [u8; 32]) {
        self.indexed.entry(llmq_type.to_base()).or_default().insert(quorum_index, hash);
    }
}

/// One quorum-commitment transaction of the block, as `CalcCbTxMerkleRootQuorums` reads it
/// (special version, type `TRANSACTION_QUORUM_COMMITMENT`), in block order after the coinbase
/// (Core's loop starts at `vtx[1]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockQc {
    /// `GetTxPayload<CFinalCommitmentTxPayload>` failed.
    BadPayload,
    /// A null commitment, which the root skips.
    Null,
    /// A commitment, with its `SerializeHash`.
    Commitment {
        llmq_type: LlmqType,
        quorum_index: i16,
        hash: [u8; 32],
    },
}

/// Core's `CalcCbTxMerkleRootQuorums`: the active commitments at the previous block plus the
/// block's own, the oldest non-rotated one of a type dropped for a new one, byte-sorted and
/// merkle-hashed.
///
/// `params` answers for every LLMQ type the chain configures, enabled or not (stale types still
/// count; A1 quirk 18); `None` is Core's unknown type. Reasons are Core's.
// Core: evo/cbtx.cpp:65 CalcCbTxMerkleRootQuorums
pub fn merkle_root_quorums(
    active: QcHashes,
    block_qcs: impl IntoIterator<Item = BlockQc>,
    params: impl Fn(LlmqType) -> Option<QuorumRootParams>,
) -> Result<MerkleRoot, BlockValidationError> {
    let QcHashes {
        mut plain,
        mut indexed,
    } = active;
    for qc in block_qcs {
        let (llmq_type, quorum_index, hash) = match qc {
            BlockQc::BadPayload => {
                return Err(invalid("bad-qc-payload-calc-cbtx-quorummerkleroot"));
            }
            BlockQc::Null => continue,
            BlockQc::Commitment {
                llmq_type,
                quorum_index,
                hash,
            } => (llmq_type, quorum_index, hash),
        };
        let llmq = params(llmq_type)
            .ok_or_else(|| invalid("bad-qc-commitment-type-calc-cbtx-quorummerkleroot"))?;
        if llmq.rotation {
            indexed.entry(llmq_type.to_base()).or_default().insert(quorum_index, hash);
        } else {
            let hashes = plain.entry(llmq_type.to_base()).or_default();
            // The oldest is last; a block mines at most one commitment per non-rotated type.
            if hashes.len() == llmq.signing_active_quorum_count {
                hashes.pop();
            }
            hashes.push(hash);
        }
    }
    for (llmq_type, by_index) in indexed {
        plain.entry(llmq_type).or_default().extend(by_index.into_values());
    }
    let mut leaves = Vec::with_capacity(plain.values().map(Vec::len).sum());
    for (llmq_type, hashes) in plain {
        // Core asserts the type's parameters exist: every active type is a configured one.
        let max = params(LlmqType::from_base(llmq_type))
            .map_or(0, |llmq| llmq.signing_active_quorum_count);
        if hashes.len() > max {
            return Err(invalid("excess-quorums-calc-cbtx-quorummerkleroot"));
        }
        leaves.extend(hashes);
    }
    merkle_root_quorums_from_hashes(leaves)
}

/// The last step of [`merkle_root_quorums`]: the commitment hashes byte-sorted (`uint256`'s
/// `operator<`) and merkle-hashed. An `mnlistdiff` client holding the quorum set after a diff
/// checks it with this (SB0 §5.2 step 4).
// Core: evo/cbtx.cpp:141-153
pub fn merkle_root_quorums_from_hashes(
    mut hashes: Vec<[u8; 32]>,
) -> Result<MerkleRoot, BlockValidationError> {
    hashes.sort_unstable();
    checked_root(compute_merkle_root(hashes), "mutated-calc-cbtx-quorummerkleroot")
}

fn invalid(reason: &'static str) -> BlockValidationError {
    BlockValidationError::new(BlockValidationResult::Consensus, reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small deterministic source; leaves come from a small alphabet so equal siblings occur.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            self.0 >> 33
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    fn leaf(n: usize) -> [u8; 32] {
        [n as u8; 32]
    }

    #[test]
    fn cache_equals_compute_merkle_root_under_edits() {
        let mut rng = Lcg(7);
        for _ in 0..200 {
            let start: Vec<[u8; 32]> = (0..rng.below(40)).map(|_| leaf(rng.below(6))).collect();
            let mut cache = MerkleCache::new(start);
            for _ in 0..30 {
                let len = cache.levels[0].len();
                match rng.below(4) {
                    0 if len > 0 => cache.remove(rng.below(len)),
                    1 => cache.insert(rng.below(len + 1), leaf(rng.below(6))),
                    _ if len > 0 => {
                        // Several replacements between roots, some of them no-ops.
                        for _ in 0..=rng.below(3) {
                            cache.replace(rng.below(len), leaf(rng.below(6)));
                        }
                    }
                    _ => {}
                }
                if rng.below(2) == 0 {
                    assert_eq!(cache.root(), compute_merkle_root(cache.levels[0].clone()));
                }
            }
            assert_eq!(cache.root(), compute_merkle_root(cache.levels[0].clone()));
        }
    }

    #[test]
    fn cache_tracks_mutation_through_replacements() {
        // [a, b, a, b]: the level above has two equal hashes.
        let (a, b, c) = (leaf(1), leaf(2), leaf(3));
        let mut cache = MerkleCache::new(vec![a, b, a, c]);
        assert!(!cache.root().1);
        cache.replace(3, b);
        assert!(cache.root().1);
        cache.replace(2, c);
        assert!(!cache.root().1);
        // Equal leaves; the duplicate of an odd level's last is never compared.
        cache.replace(1, a);
        assert!(cache.root().1);
        let mut odd = MerkleCache::new(vec![a, b, c]);
        assert!(!odd.root().1);
    }

    #[test]
    fn cache_drops_mutation_when_the_tree_shrinks() {
        let (a, b) = (leaf(1), leaf(2));
        // An equal pair on level 1, gone with the level.
        let mut cache = MerkleCache::new(vec![a, b, a, b]);
        assert!(cache.root().1);
        cache.remove(3);
        cache.remove(2);
        assert_eq!(cache.root(), compute_merkle_root(vec![a, b]));
        // An equal pair of leaves, down to one leaf.
        let mut pair = MerkleCache::new(vec![a, a]);
        assert!(pair.root().1);
        pair.remove(1);
        assert_eq!(pair.root(), (a, false));
    }

    /// An entry among 16 proTxHashes, so edits hit present and absent ones.
    fn sml_entry(rng: &mut Lcg) -> SmlEntry {
        use dash_consensus::primitives::BlockHash;
        use dash_primitives::{AddrV1, NetInfo, NetInfoV1, PlatformNodeId, ServiceV1};
        SmlEntry {
            version: 2,
            pro_reg_tx_hash: ProTxHash::from([rng.below(16) as u8; 32]),
            confirmed_hash: BlockHash::from_lendian([rng.below(4) as u8; 32]),
            net_info: NetInfo::Legacy(NetInfoV1(ServiceV1 {
                addr: AddrV1([0; 16]),
                port: 0,
            })),
            operator_key: [0; 48],
            voting_key_id: [0; 20],
            is_valid: rng.below(2) == 0,
            mn_type: crate::types::RawMnType::REGULAR,
            platform_http_port: 0,
            platform_node_id: PlatformNodeId::from_lendian([0; 20]),
        }
    }

    #[test]
    fn list_equals_model_and_full_root_under_edits() {
        let mut rng = Lcg(11);
        for _ in 0..200 {
            let start: Vec<SmlEntry> = (0..rng.below(12)).map(|_| sml_entry(&mut rng)).collect();
            let mut model: BTreeMap<ProTxHash, SmlEntry> =
                start.iter().map(|entry| (entry.pro_reg_tx_hash, entry.clone())).collect();
            let mut list = SmlList::from_entries(start);
            for _ in 0..30 {
                if rng.below(3) == 0 {
                    let pro_tx = ProTxHash::from([rng.below(16) as u8; 32]);
                    assert_eq!(list.remove(&pro_tx), model.remove(&pro_tx));
                } else {
                    let entry = sml_entry(&mut rng);
                    model.insert(entry.pro_reg_tx_hash, entry.clone());
                    list.upsert(entry);
                }
                if rng.below(2) == 0 {
                    let full = merkle_root_mn_list(model.values()).unwrap();
                    assert_eq!(list.merkle_root().unwrap(), full);
                }
            }
            assert!(list.entries().iter().eq(model.values()));
            for (pro_tx, entry) in &model {
                assert_eq!(list.get(pro_tx), Some(entry));
            }
            assert_eq!(list.merkle_root().unwrap(), merkle_root_mn_list(model.values()).unwrap());
        }
    }

    const TYPE_A: LlmqType = LlmqType::Llmq50_60;
    const TYPE_R: LlmqType = LlmqType::Llmq60_75;

    fn params(llmq_type: LlmqType) -> Option<QuorumRootParams> {
        match llmq_type {
            TYPE_A => Some(QuorumRootParams {
                signing_active_quorum_count: 2,
                rotation: false,
            }),
            TYPE_R => Some(QuorumRootParams {
                signing_active_quorum_count: 2,
                rotation: true,
            }),
            _ => None,
        }
    }

    fn reason(result: Result<MerkleRoot, BlockValidationError>) -> String {
        result.unwrap_err().reject_reason().to_owned()
    }

    fn commitment(llmq_type: LlmqType, quorum_index: i16, hash: u8) -> BlockQc {
        BlockQc::Commitment {
            llmq_type,
            quorum_index,
            hash: leaf(usize::from(hash)),
        }
    }

    #[test]
    fn quorums_root_assembly() {
        let mut active = QcHashes::default();
        // Newest first.
        active.push_plain(TYPE_A, leaf(20));
        active.push_plain(TYPE_A, leaf(10));
        active.insert_indexed(TYPE_R, 0, leaf(30));
        active.insert_indexed(TYPE_R, 1, leaf(31));
        let root = |qcs: Vec<BlockQc>| merkle_root_quorums(active.clone(), qcs, params);

        let none = root(vec![]).unwrap();
        assert_eq!(
            none,
            merkle_root_quorums_from_hashes(vec![leaf(31), leaf(10), leaf(30), leaf(20)]).unwrap()
        );

        // A new non-rotated commitment drops the oldest (last), and null ones are skipped.
        let mined = root(vec![BlockQc::Null, commitment(TYPE_A, 0, 40)]).unwrap();
        assert_eq!(
            mined,
            merkle_root_quorums_from_hashes(vec![leaf(20), leaf(40), leaf(30), leaf(31)]).unwrap()
        );

        // A rotated one replaces its index.
        let rotated = root(vec![commitment(TYPE_R, 1, 41)]).unwrap();
        assert_eq!(
            rotated,
            merkle_root_quorums_from_hashes(vec![leaf(20), leaf(10), leaf(30), leaf(41)]).unwrap()
        );

        // Core's rejects.
        assert_eq!(
            reason(root(vec![BlockQc::BadPayload])),
            "bad-qc-payload-calc-cbtx-quorummerkleroot"
        );
        assert_eq!(
            reason(root(vec![commitment(LlmqType::Llmq400_60, 0, 50)])),
            "bad-qc-commitment-type-calc-cbtx-quorummerkleroot"
        );
        assert_eq!(
            reason(root(vec![commitment(TYPE_R, 2, 42)])),
            "excess-quorums-calc-cbtx-quorummerkleroot"
        );
        // A second non-rotated commitment of a type in one block (which block rules forbid)
        // pops the first, as in Core.
        let twice = root(vec![commitment(TYPE_A, 0, 40), commitment(TYPE_A, 0, 41)]).unwrap();
        assert_eq!(
            twice,
            merkle_root_quorums_from_hashes(vec![leaf(20), leaf(41), leaf(30), leaf(31)]).unwrap()
        );
        // Equal hashes side by side after sorting.
        assert_eq!(
            reason(root(vec![commitment(TYPE_R, 1, 30)])),
            "mutated-calc-cbtx-quorummerkleroot"
        );
        // An error stops at the first bad transaction, in block order.
        assert_eq!(
            reason(root(vec![BlockQc::BadPayload, commitment(LlmqType::Llmq400_60, 0, 50)])),
            "bad-qc-payload-calc-cbtx-quorummerkleroot"
        );

        // No commitments at all: the zero root.
        let empty = merkle_root_quorums(QcHashes::default(), [], params).unwrap();
        assert_eq!(empty, MerkleRoot::from_lendian([0; 32]));
    }
}
