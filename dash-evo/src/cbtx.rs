//! The coinbase special transaction (`CCbTx`): the checks of its list roots, the partial-merkle
//! proof that carries it in `mnlistdiff`, and the coinbase-ChainLock predicate (LQ1 §3.3.1).
//!
//! The roots are [`crate::sml::root`]'s. SPV checks an `mnlistdiff` against the CbTx through these
//! (SB0 §5.2 steps 2–4; dashpay/rust-dashcore#1117).

use std::fmt;

use dash_consensus::hash::sha256d;
use dash_consensus::primitives::{TxHash, TxRef};
use dash_consensus::tx::MAX_DIP0001_BLOCK_SIZE;
use dash_consensus::validation::{BlockValidationError, BlockValidationResult};
use dash_pkc::bls::BlsEagerSig;
use dash_primitives::{CoinbaseCommitment, MerkleRoot, SpecialPayload, TxType};
use dash_types::Numeric;

use crate::bls::BlsCtx;

/// `CCbTx::Version::INVALID`.
// Core: evo/cbtx.h:32-39
pub const CBTX_VERSION_INVALID: u16 = 0;

/// `CCbTx::Version::MERKLE_ROOT_QUORUMS`: the first version with `merkleRootQuorums`.
pub const CBTX_VERSION_MERKLE_ROOT_QUORUMS: u16 = 2;

/// `CCbTx::Version::CLSIG_AND_BALANCE`: the first version with the best ChainLock.
pub const CBTX_VERSION_CLSIG_AND_BALANCE: u16 = 3;

/// `CCbTx::Version::MERKLE_ROOT_ASSETUNLOCKS`: the first version with `merkleRootAssetUnlocks`.
pub const CBTX_VERSION_MERKLE_ROOT_ASSETUNLOCKS: u16 = 4;

/// `CCbTx::Version::UNKNOWN`: the first version Core does not know.
pub const CBTX_VERSION_UNKNOWN: u16 = 5;

/// The `merkleRootMNList` check of `ProcessSpecialTxsInBlock`: `calculated` is the root of the
/// list after the block ([`crate::sml::SmlList::merkle_root`]).
// Core: evo/specialtxman.cpp:930-936
pub fn check_mn_list_root(
    cbtx: &CoinbaseCommitment,
    calculated: &MerkleRoot,
) -> Result<(), BlockValidationError> {
    if cbtx.merkle_root_mn_list != *calculated {
        return Err(invalid("bad-cbtx-mnmerkleroot"));
    }
    Ok(())
}

/// The `merkleRootQuorums` check of `ProcessSpecialTxsInBlock`, from CbTx version 2:
/// `calculate` is [`crate::sml::merkle_root_quorums`] for the block, run only then.
// Core: evo/specialtxman.cpp:943-952
pub fn check_quorums_root(
    cbtx: &CoinbaseCommitment,
    calculate: impl FnOnce() -> Result<MerkleRoot, BlockValidationError>,
) -> Result<(), BlockValidationError> {
    if cbtx.version < CBTX_VERSION_MERKLE_ROOT_QUORUMS {
        return Ok(());
    }
    let calculated = calculate()?;
    if cbtx.merkle_root_quorums != Some(calculated) {
        return Err(invalid("bad-cbtx-quorummerkleroot"));
    }
    Ok(())
}

fn invalid(reason: &'static str) -> BlockValidationError {
    BlockValidationError::new(BlockValidationResult::Consensus, reason)
}

/// Core's `CPartialMerkleTree` as read from the wire: the block's transaction count, the hashes
/// and the flag bits, packed eight to a byte, least significant first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PartialMerkleTree {
    pub transactions: u32,
    pub hashes: Vec<[u8; 32]>,
    pub flags: Vec<u8>,
}

/// What [`PartialMerkleTree::extract_matches`] reads from a well-formed tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Matches {
    /// The merkle root the tree commits to.
    pub root: [u8; 32],
    /// The matched txids with their positions in the block, in order.
    pub matches: Vec<([u8; 32], u32)>,
}

impl PartialMerkleTree {
    /// Core's `ExtractMatches`: the root and the matched txids, or `None` where Core returns the
    /// zero root (an empty or oversized tree, too many hashes or too few bits, a traversal past
    /// the hashes or bits, equal sibling subtrees, or unused hashes or whole bytes of bits).
    // Core: merkleblock.cpp:166 CPartialMerkleTree::ExtractMatches
    pub fn extract_matches(&self) -> Option<Matches> {
        let transactions = self.transactions;
        let bits = self.flags.len() * 8;
        if transactions == 0
            // 60 bytes is the smallest serialized transaction.
            || transactions > MAX_DIP0001_BLOCK_SIZE / 60
            || self.hashes.len() > transactions as usize
            || bits < self.hashes.len()
        {
            return None;
        }
        let mut height = 0;
        while tree_width(transactions, height) > 1 {
            height += 1;
        }
        let mut walk = Traversal {
            tree: self,
            bits_used: 0,
            hashes_used: 0,
            bad: false,
            matches: Vec::new(),
        };
        let root = walk.extract(height, 0);
        // Every hash, and every byte of bits but the padding of the last, must be used.
        if walk.bad
            || walk.bits_used.div_ceil(8) != self.flags.len()
            || walk.hashes_used != self.hashes.len()
        {
            return None;
        }
        Some(Matches {
            root,
            matches: walk.matches,
        })
    }
}

// Core: merkleblock.h:70 CalcTreeWidth
fn tree_width(transactions: u32, height: u32) -> u32 {
    // At most 33,333 transactions: neither the sum nor the shift overflows.
    (transactions + (1 << height) - 1) >> height
}

struct Traversal<'a> {
    tree: &'a PartialMerkleTree,
    bits_used: usize,
    hashes_used: usize,
    bad: bool,
    matches: Vec<([u8; 32], u32)>,
}

impl Traversal<'_> {
    fn bit(&self, i: usize) -> bool {
        self.tree.flags[i / 8] & (1 << (i % 8)) != 0
    }

    // Core: merkleblock.cpp:112 CPartialMerkleTree::TraverseAndExtract. The depth is at most 16.
    fn extract(&mut self, height: u32, pos: u32) -> [u8; 32] {
        if self.bits_used >= self.tree.flags.len() * 8 {
            self.bad = true;
            return [0; 32];
        }
        let parent_of_match = self.bit(self.bits_used);
        self.bits_used += 1;
        if height == 0 || !parent_of_match {
            let Some(&hash) = self.tree.hashes.get(self.hashes_used) else {
                self.bad = true;
                return [0; 32];
            };
            self.hashes_used += 1;
            if height == 0 && parent_of_match {
                self.matches.push((hash, pos));
            }
            return hash;
        }
        let left = self.extract(height - 1, pos * 2);
        let right = if pos * 2 + 1 < tree_width(self.tree.transactions, height - 1) {
            let right = self.extract(height - 1, pos * 2 + 1);
            // Two subtrees over distinct transactions never hash alike.
            if right == left {
                self.bad = true;
            }
            right
        } else {
            left
        };
        let mut both = [0; 64];
        both[..32].copy_from_slice(&left);
        both[32..].copy_from_slice(&right);
        sha256d(&both)
    }
}

/// Why an `mnlistdiff`'s CbTx proof fails (SB0 §5.2 step 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CbTxProofError {
    /// `ExtractMatches` failed.
    Malformed,
    /// The tree's root is not the block's merkle root.
    RootMismatch,
    /// The tree does not match exactly the coinbase transaction at position 0.
    NotTheCoinbase,
}

impl fmt::Display for CbTxProofError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CbTxProofError::Malformed => "malformed partial merkle tree",
            CbTxProofError::RootMismatch => "partial merkle tree root is not the block's",
            CbTxProofError::NotTheCoinbase => {
                "partial merkle tree does not match exactly the coinbase"
            }
        })
    }
}

impl std::error::Error for CbTxProofError {}

/// Checks that `tree` proves `cbtx_txid` is the first transaction of the block whose header
/// carries `merkle_root`: the tree reproduces the root and matches that txid at position 0 and
/// nothing else.
pub fn verify_cbtx_proof(
    tree: &PartialMerkleTree,
    merkle_root: &MerkleRoot,
    cbtx_txid: &TxHash,
) -> Result<(), CbTxProofError> {
    let extracted = tree.extract_matches().ok_or(CbTxProofError::Malformed)?;
    if extracted.root != merkle_root.to_lendian() {
        return Err(CbTxProofError::RootMismatch);
    }
    if extracted.matches != [(cbtx_txid.to_lendian(), 0)] {
        return Err(CbTxProofError::NotTheCoinbase);
    }
    Ok(())
}

/// The block data `GetNonNullCoinbaseChainlock` reads (LQ1 §3.3.1).
#[derive(Clone)]
pub enum CoinbaseFacts {
    /// `ReadBlockFromDisk` failed: the block's data is not held.
    BlockDataUnavailable,
    /// The block has no transactions.
    NoTransactions,
    /// The block's first transaction, coinbase or not.
    First(TxRef),
}

impl fmt::Debug for CoinbaseFacts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoinbaseFacts::BlockDataUnavailable => f.write_str("BlockDataUnavailable"),
            CoinbaseFacts::NoTransactions => f.write_str("NoTransactions"),
            CoinbaseFacts::First(tx) => write!(f, "First({})", tx.txid()),
        }
    }
}

/// Core's `GetNonNullCoinbaseChainlock(pindex)` at v24: the best ChainLock a block's CbTx
/// carries, with its height difference, when the block is at or after V20 and its CbTx has a
/// valid one.
///
/// None unless: V20 is active at the block; its data is held; its first transaction is a
/// coinbase of type `TRANSACTION_COINBASE` whose payload decodes with no bytes left over; the
/// CbTx is version 3 or later at `height`; its signature is valid under `bls` (Core's global
/// flag); and the difference is below `height`. Core reads the difference as a compact size,
/// which throws above `MAX_SIZE` as the payload decoder does; a value set by hand above
/// `u32::MAX` gives None too.
// Core: evo/cbtx.cpp:181 GetNonNullCoinbaseChainlock(pindex), :200 (block, height)
pub fn nonnull_coinbase_chainlock(
    facts: &CoinbaseFacts,
    height: u32,
    v20_active_at: bool,
    bls: BlsCtx,
) -> Option<(BlsEagerSig, u32)> {
    if !v20_active_at {
        return None;
    }
    let CoinbaseFacts::First(tx) = facts else {
        return None;
    };
    if !tx.is_coinbase() || tx.tx_type() != TxType::CoinbaseCommitment {
        return None;
    }
    let decoded = tx.payload()?.ok()?;
    let SpecialPayload::CoinbaseCommitment(cbtx) = &decoded.payload else {
        return None;
    };
    if !decoded.is_complete()
        || cbtx.version < CBTX_VERSION_CLSIG_AND_BALANCE
        || i64::from(cbtx.height) != i64::from(height)
    {
        return None;
    }
    let signature = cbtx.best_cl_signature.as_ref()?.get(bls);
    let diff = u32::try_from(cbtx.best_cl_height_diff?).ok()?;
    if !signature.is_valid() || diff >= height {
        return None;
    }
    Some((signature, diff))
}
