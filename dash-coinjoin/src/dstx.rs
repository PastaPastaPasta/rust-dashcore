//! Mixing transactions (`dstx`): their structure, expiry and signature, the store Core keeps of
//! them (`CDSTXManager`), and the checks a node runs before it takes one to the mempool
//! (`ValidateDSTX`, `net_processing.cpp`).

use std::collections::BTreeMap;

use dash_consensus::hash::HashWriter;
use dash_consensus::primitives::{CoreSerialize as _, CoreWrite as _, Tx};
use dash_num::Hash256;
use dash_p2p_core::{DsTx, ProtocolVersion};
use dash_params::ChainParams;
use dash_pkc::bls::core::{BlsPkValue, BlsVerdict};
use dash_primitives::{Transaction, TxHash};
use dash_types::Numeric as _;

use crate::denom::{self, COIN, PROMOTION_RATIO_COUNT};
use crate::inouts::is_pay_to_public_key_hash;
use crate::pool::ENTRY_MAX_SIZE;
use crate::queue::{MixingCounters, MixingMasternode, verify_masternode_sig};

/// How many participants a session takes: the chain's `PoolMinParticipants()` and
/// `PoolMaxParticipants()`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PoolLimits {
    /// `GetMinPoolParticipants()`.
    pub min_participants: i32,
    /// `GetMaxPoolParticipants()`.
    pub max_participants: i32,
}

impl PoolLimits {
    /// The limits of `params`.
    pub fn of(params: &ChainParams) -> Self {
        Self {
            min_participants: params.pool_min_participants,
            max_participants: params.pool_max_participants,
        }
    }

    /// `GetMinPoolParticipants()` as a count.
    pub fn min(&self) -> usize {
        usize::try_from(self.min_participants).unwrap_or(0)
    }

    /// `GetMaxPoolParticipants()` as a count.
    pub fn max(&self) -> usize {
        usize::try_from(self.max_participants).unwrap_or(0)
    }

    /// `GetMaxPoolInputOutputCount()`: the most inputs or outputs of a pre-V24 pool.
    pub fn max_pool_in_outs(&self) -> usize {
        self.max() * ENTRY_MAX_SIZE
    }

    /// The most inputs or outputs of a post-V24 pool, all promotions or all demotions.
    pub fn max_pool_in_outs_post_v24(&self) -> usize {
        self.max() * PROMOTION_RATIO_COUNT
    }
}

/// [`is_valid_structure`]'s answer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DstxStructure {
    /// Valid under the rules at the tip.
    pub valid: bool,
    /// Pre-V24 at the tip: invalid now but valid under the post-V24 rules
    /// (`fPossiblyValidPostV24Ret`).
    pub possibly_valid_post_v24: bool,
}

// Core: coinjoin/coinjoin.cpp:100 CCoinJoinBroadcastTx::IsValidStructure
/// `IsValidStructure`: a masternode is named, at least the minimum participants' inputs,
/// denominated P2PKH outputs, and the counts the rules at the tip allow: equal and at most
/// [`PoolLimits::max_pool_in_outs`] before V24; after it within
/// [`PoolLimits::max_pool_in_outs_post_v24`], neither side more than
/// [`denom::PROMOTION_RATIO`] times the other, and a side difference that is a multiple of
/// `PROMOTION_RATIO - 1`.
///
/// A received DSTX never carries the masternode's outpoint, so only its ProRegTx hash names it.
pub fn is_valid_structure(dstx: &DsTx, limits: PoolLimits, v24_active: bool) -> DstxStructure {
    let invalid = DstxStructure {
        valid: false,
        possibly_valid_post_v24: false,
    };
    let tx = &dstx.tx;
    if dstx.pro_tx_hash.is_null() || tx.inputs.len() < limits.min() {
        return invalid;
    }
    let outputs_ok = tx.outputs.iter().all(|o| {
        denom::is_denominated_amount(o.value)
            && is_pay_to_public_key_hash(o.script_pubkey.as_bytes())
    });
    if !outputs_ok {
        return invalid;
    }

    let (ins, outs) = (tx.inputs.len(), tx.outputs.len());
    let ratio = PROMOTION_RATIO_COUNT;
    let valid_pre_v24 = is_balanced_within(tx, limits);
    let max_post = limits.max_pool_in_outs_post_v24();
    let valid_post_v24 = ins <= max_post
        && outs <= max_post
        && ins <= outs.saturating_mul(ratio)
        && outs <= ins.saturating_mul(ratio)
        && ins.abs_diff(outs) % (ratio - 1) == 0;

    if v24_active {
        return DstxStructure {
            valid: valid_post_v24,
            possibly_valid_post_v24: false,
        };
    }
    DstxStructure {
        valid: valid_pre_v24,
        possibly_valid_post_v24: !valid_pre_v24 && valid_post_v24,
    }
}

// Core: primitives/transaction.cpp:115 CTransaction::ComputeHash
/// `tx.GetHash()`: the txid from dash-consensus, the sole consensus encoder (A1 §1.3), never
/// from base-sdk's encoder.
pub fn txid(tx: &Transaction) -> TxHash {
    Tx::new(tx.clone()).txid()
}

// Core: coinjoin/coinjoin.cpp:77 CCoinJoinBroadcastTx::GetSignatureHash
/// `CCoinJoinBroadcastTx::GetSignatureHash()`: the message as written without the signature
/// (`SER_GETHASH`), its time included. The transaction goes through dash-consensus's encoder
/// (A1 §1.3), not base-sdk's, as [`DsTx::signature_hash`] does.
pub fn dstx_signature_hash(dstx: &DsTx) -> Hash256 {
    let mut w = HashWriter::new();
    dstx.tx.core_write(&mut w);
    w.write(&dstx.pro_tx_hash.to_lendian());
    w.write(&dstx.sig_time.to_le_bytes());
    Hash256::from_lendian(w.finish())
}

// Core: coinjoin/coinjoin.cpp:82 CCoinJoinBroadcastTx::CheckSignature
/// `CCoinJoinBroadcastTx::CheckSignature`: the operator's signature over
/// [`dstx_signature_hash`].
pub fn check_dstx_signature(dstx: &DsTx, operator_key: &BlsPkValue) -> BlsVerdict {
    verify_masternode_sig(&dstx.sig, operator_key, &dstx_signature_hash(dstx))
}

/// How many blocks after its confirmation a DSTX is kept at most.
pub const DSTX_KEEP_DEPTH: i32 = 24;

// Core: coinjoin/coinjoin.cpp:92 CCoinJoinBroadcastTx::IsExpired
/// `IsExpired`: confirmed at or below the tip, and either more than [`DSTX_KEEP_DEPTH`] blocks
/// deep or under a ChainLock (`tip_chainlocked`: Core's `HasChainLock(tip height, tip hash)`).
pub const fn is_expired(
    confirmed_height: Option<i32>,
    tip_height: i32,
    tip_chainlocked: bool,
) -> bool {
    let Some(confirmed) = confirmed_height else {
        return false;
    };
    if tip_height < confirmed {
        return false;
    }
    if tip_height as i64 - confirmed as i64 > DSTX_KEEP_DEPTH as i64 {
        return true;
    }
    tip_chainlocked
}

/// A DSTX as Core's `CDSTXManager` keeps it.
#[derive(Clone, Debug)]
pub struct StoredDstx {
    /// The message.
    pub dstx: DsTx,
    /// The height of the block that holds its transaction (memory only); `None` while
    /// unconfirmed or conflicted.
    pub confirmed_height: Option<i32>,
}

// Core: coinjoin/coinjoin.h:541 CDSTXManager
/// Core's `CDSTXManager`: the DSTXes this node accepted, by txid (`tx->GetHash()`). A DSTX here
/// is announced and served as `MSG_DSTX`.
#[derive(Clone, Debug, Default)]
pub struct DstxManager {
    dstxes: BTreeMap<TxHash, StoredDstx>,
}

impl DstxManager {
    // Core: coinjoin/coinjoin.cpp:563 CDSTXManager::AddDSTX
    /// `AddDSTX`: keep `dstx` under its transaction's hash, unless one is already kept.
    pub fn add(&mut self, dstx: DsTx) {
        self.dstxes.entry(txid(&dstx.tx)).or_insert(StoredDstx {
            dstx,
            confirmed_height: None,
        });
    }

    /// `GetDSTX`.
    pub fn get(&self, txid: &TxHash) -> Option<&StoredDstx> {
        self.dstxes.get(txid)
    }

    /// Whether `txid` is kept (`static_cast<bool>(GetDSTX(txid))`).
    pub fn contains(&self, txid: &TxHash) -> bool {
        self.dstxes.contains_key(txid)
    }

    /// How many are kept.
    pub fn len(&self) -> usize {
        self.dstxes.len()
    }

    /// Whether none is kept.
    pub fn is_empty(&self) -> bool {
        self.dstxes.is_empty()
    }

    // Core: coinjoin/coinjoin.cpp:583 CDSTXManager::CheckDSTXes
    /// `CheckDSTXes` (on `UpdatedBlockTip` and `NotifyChainLock`): drop the expired ones at the
    /// tip. `tip_chainlocked` is evaluated once: it does not depend on the DSTX.
    pub fn check(&mut self, tip_height: i32, tip_chainlocked: bool) {
        self.dstxes.retain(|_, d| !is_expired(d.confirmed_height, tip_height, tip_chainlocked));
    }

    // Core: coinjoin/coinjoin.cpp:612 CDSTXManager::UpdateDSTXConfirmedHeight
    /// `UpdateDSTXConfirmedHeight`: `TransactionAddedToMempool` and `BlockDisconnected` give
    /// `None`, `BlockConnected` the block's height, for each transaction.
    pub fn update_confirmed_height(&mut self, txid: &TxHash, height: Option<i32>) {
        if let Some(d) = self.dstxes.get_mut(txid) {
            d.confirmed_height = height;
        }
    }
}

/// A peer's misbehaviour for a DSTX that failed [`validate_dstx`] (Core's
/// `DSTXValidationScore`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DstxScore {
    /// No penalty.
    None,
    /// The masternode was not found within [`DSTX_KEEP_DEPTH`] blocks: 1.
    UnknownMasternode,
    /// Valid only under the post-V24 rules, one block before V24 activates here: 1.
    Premature,
    /// Malformed or badly signed: 10.
    Invalid,
}

impl DstxScore {
    /// The misbehaviour score.
    pub const fn score(self) -> i32 {
        match self {
            Self::None => 0,
            Self::UnknownMasternode | Self::Premature => 1,
            Self::Invalid => 10,
        }
    }
}

/// [`validate_dstx`]'s answer (Core's `DSTXValidationResult`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DstxValidation {
    /// Stop processing the message; penalise the peer unless the score is
    /// [`DstxScore::None`].
    Stop(DstxScore),
    /// Go on to the mempool. Core raised the transaction's priority by `priority_delta`
    /// (`PrioritiseTransaction(txid, 0.1 * COIN)`) and counted it against the masternode
    /// (already applied to the [`MixingCounters`]). On acceptance the node keeps it in its
    /// [`DstxManager`].
    Continue {
        /// The fee delta to apply to the mempool entry.
        priority_delta: i64,
        /// The signing masternode.
        masternode: TxHash,
    },
}

/// The fee delta a DSTX's transaction gets: Core's `0.1*COIN`, a double converted to `CAmount`.
pub const DSTX_PRIORITY_DELTA: i64 = COIN / 10;

/// The chain as [`validate_dstx`] reads it.
pub trait DstxChain {
    /// The active tip's height, `None` without a tip.
    fn tip_height(&self) -> Option<i32>;

    /// `DeploymentActiveAt(tip, V24)`.
    fn v24_active_at_tip(&self) -> bool;

    /// `IsPromotionDemotionActive(fNextBlock=true)`: V24 active for the block after the tip.
    fn v24_active_after_tip(&self) -> bool;

    /// `GetListForBlock(block).GetMN(pro_tx_hash)` (any masternode, valid or not) for the
    /// active chain's block `depth` blocks below the tip (0 is the tip).
    fn mn_at_depth(&self, depth: i32, pro_tx_hash: &TxHash) -> Option<MixingMasternode>;
}

// Core: net_processing.cpp:3637 ValidateDSTX
/// `ValidateDSTX` (v24.0.0-rc.3 `net_processing.cpp:3637`): what a node does with a received
/// DSTX before the mempool. `txid` is the transaction's hash, [`txid`]`(&dstx.tx)` (Core's caller
/// passes `tx.GetHash()`, `net_processing.cpp:4913-4914`).
pub fn validate_dstx<C: DstxChain + ?Sized>(
    dstxes: &DstxManager,
    counters: &mut MixingCounters,
    chain: &C,
    limits: PoolLimits,
    dstx: &DsTx,
    txid: &TxHash,
) -> DstxValidation {
    if dstxes.contains(txid) {
        return DstxValidation::Stop(DstxScore::None);
    }

    let structure =
        is_valid_structure(dstx, limits, chain.tip_height().is_some() && chain.v24_active_at_tip());
    if !structure.valid {
        // A masternode one block ahead of us at V24's activation may sign a post-V24 DSTX:
        // tolerate that much skew, as the 24-block masternode scan does.
        if structure.possibly_valid_post_v24 && chain.v24_active_after_tip() {
            return DstxValidation::Stop(DstxScore::Premature);
        }
        return DstxValidation::Stop(DstxScore::Invalid);
    }

    // The masternode may have left the list with its DSTX not yet mined: look 24 blocks back.
    let mn = chain.tip_height().and_then(|tip| {
        (0..DSTX_KEEP_DEPTH.min(tip.saturating_add(1)))
            .find_map(|depth| chain.mn_at_depth(depth, &dstx.pro_tx_hash))
    });
    let Some(mn) = mn else {
        return DstxValidation::Stop(DstxScore::UnknownMasternode);
    };

    if !counters.is_valid_for_mixing_txes(&mn.pro_tx_hash) {
        return DstxValidation::Stop(DstxScore::None);
    }

    match check_dstx_signature(dstx, &mn.operator_key) {
        BlsVerdict::Valid => {}
        BlsVerdict::Invalid => return DstxValidation::Stop(DstxScore::Invalid),
        // Not provably the sender's fault (CJ0 notes D9).
        BlsVerdict::Unsupported => return DstxValidation::Stop(DstxScore::None),
    }

    counters.disallow_mixing(&mn.pro_tx_hash);
    DstxValidation::Continue {
        priority_delta: DSTX_PRIORITY_DELTA,
        masternode: mn.pro_tx_hash,
    }
}

/// Whether `tx` is a CoinJoin transaction a pre-V24 peer could read as one: equal counts within
/// the pre-V24 pool size.
fn is_balanced_within(tx: &Transaction, limits: PoolLimits) -> bool {
    tx.inputs.len() == tx.outputs.len() && tx.inputs.len() <= limits.max_pool_in_outs()
}

// Core: net_processing.cpp:3612 CanAnnounceDstxTo
/// `CanAnnounceDstxTo`: whether a kept DSTX may be announced as `MSG_DSTX` to a peer at the
/// negotiated `peer_version`; otherwise it is announced as `MSG_TX`. Unbalanced (rebalance) and
/// oversized ones are structurally invalid to peers before
/// [`ProtocolVersion::COINJOIN_REBALANCE`].
pub fn can_announce_dstx_to(
    tx: &Transaction,
    limits: PoolLimits,
    peer_version: ProtocolVersion,
) -> bool {
    is_balanced_within(tx, limits) || peer_version >= ProtocolVersion::COINJOIN_REBALANCE
}
