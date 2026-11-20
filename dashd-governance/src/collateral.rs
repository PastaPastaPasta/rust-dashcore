//! Proposal collateral, read at use inside the governance command, through IX1's transaction
//! index and V's active chain.

use std::collections::{BTreeMap, BTreeSet};

use dash_consensus::Tx;
use dash_governance::GovParams;
use dash_governance::manager::{CollateralLookup, Manager};
use dash_governance::object::{self, Check, FoundCollateral, PROPOSAL};
use dash_governance::strings::hash_hex;
use dash_primitives::{GovObject, Transaction};
use dashd_indexes::{IndexError, LocatedTx, TxIndexReader, Txid};

use crate::exec::GovChain;

/// `FindTx`, then the block's active-chain height and the tip from the same view. An index
/// error and a miss are both absent: Core logs the miss and the check fails. An index error is
/// also logged at error level, as our fault.
// Core: governance/object.cpp:798-807, 855-860
pub fn locate(index: &dyn TxIndexReader, chain: &dyn GovChain, txid: &Txid) -> Option<LocatedTx> {
    try_locate(index, chain, txid).ok().flatten()
}

/// [`locate`], keeping an index fault apart from a miss. Logs the fault.
fn try_locate(
    index: &dyn TxIndexReader,
    chain: &dyn GovChain,
    txid: &Txid,
) -> Result<Option<LocatedTx>, IndexError> {
    let found = index.find_tx(txid).inspect_err(|e| {
        tracing::error!(
            "governance collateral {}: transaction index failed: {e}",
            hash_hex(&object::txid_bytes(txid))
        );
    })?;
    Ok(found.map(|(tx, block_hash)| {
        let active_height = chain.active_height(&block_hash);
        LocatedTx::new(tx, block_hash, active_height, chain.tip_height())
    }))
}

/// The collateral answers for one manager call, located in the command that makes the call.
/// Its keys are the collateral txids that call reads; nothing is kept across commands. A key
/// the index failed on answers absent, as a miss does, and is counted as a fault.
#[derive(Default)]
pub struct AtUseCollateral {
    keys: BTreeSet<[u8; 32]>,
    found: BTreeMap<[u8; 32], (Tx, i32)>,
    faulted: bool,
}

impl AtUseCollateral {
    /// For `process_object_at(obj, …)` on `manager`: nothing for an object it has seen, else the
    /// object's collateral. The manager reads it for every object but uses it only for a
    /// proposal whose data passes, so only that one is looked up, as `IsValidLocally` checks
    /// the data before `IsCollateralValid` and checks no collateral for a trigger.
    // Core: governance/governance.cpp:234-238; governance/object.cpp:719-772
    pub fn for_object(
        obj: &GovObject,
        manager: &Manager,
        params: &GovParams,
        index: &dyn TxIndexReader,
        chain: &dyn GovChain,
    ) -> Self {
        let mut at_use = Self::default();
        let hash = object::hash(obj);
        if manager.have_object_for_hash(&hash) || manager.is_erased(&hash) {
            return at_use;
        }
        if object::object_type(obj) == PROPOSAL
            && object::is_valid_locally(obj, false, Check::NoCollateral, params).valid
        {
            at_use.locate(obj, index, chain);
        } else {
            at_use.keys.insert(object::txid_bytes(&obj.collateral_hash()));
        }
        at_use
    }

    /// For the postponed pass on `manager` (`check_postponed_objects_at` or
    /// `updated_block_tip`): every postponed object's collateral, or nothing when not synced.
    // Core: governance/governance.cpp:885-910
    pub fn for_postponed(
        manager: &Manager,
        synced: bool,
        index: &dyn TxIndexReader,
        chain: &dyn GovChain,
    ) -> Self {
        let mut at_use = Self::default();
        if synced {
            for obj in manager.postponed().values() {
                at_use.locate(obj, index, chain);
            }
        }
        at_use
    }

    /// The collateral txids the following manager call reads.
    pub fn keys(&self) -> &BTreeSet<[u8; 32]> {
        &self.keys
    }

    /// Whether the index failed on a key: our fault, not the object's.
    pub fn index_fault(&self) -> bool {
        self.faulted
    }

    fn locate(&mut self, obj: &GovObject, index: &dyn TxIndexReader, chain: &dyn GovChain) {
        let txid = obj.collateral_hash();
        let key = object::txid_bytes(&txid);
        if !self.keys.insert(key) {
            return;
        }
        match try_locate(index, chain, &txid) {
            Ok(Some(located)) => {
                let confirmations = i32::try_from(located.confirmations).unwrap_or(i32::MAX);
                self.found.insert(key, (Tx::new(Transaction::clone(&located.tx)), confirmations));
            }
            Ok(None) => {}
            Err(_) => self.faulted = true,
        }
    }
}

impl CollateralLookup for AtUseCollateral {
    fn find_collateral(&self, txid: &[u8; 32]) -> Option<FoundCollateral<'_>> {
        debug_assert!(self.keys.contains(txid), "collateral read outside the located keys");
        self.found.get(txid).map(|(tx, confirmations)| FoundCollateral {
            tx,
            // v24 reads the txindex only, which holds mined transactions.
            mined: true,
            confirmations: *confirmations,
        })
    }
}
