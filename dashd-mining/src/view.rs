//! Read-only mining seams. Implementations own the locks and state; the assembler only sees the
//! already-held package view.

use dash_consensus::primitives::{Tx, TxHash, TxRef};
use dash_dml::MnList;
use dash_evo::types::ProTxHash;

/// A stable handle into a mempool read guard (`txiter` in Core).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntryId(pub u64);

/// The fields `CreateNewBlock` reads from one mempool entry.
pub struct MiningEntry<'a> {
    pub tx: &'a TxRef,
    pub txid: TxHash,
    pub fee: i64,
    pub modified_fee: i64,
    pub tx_size: u64,
    pub sigops: u32,
    pub size_with_ancestors: u64,
    pub mod_fees_with_ancestors: i64,
    pub sigops_with_ancestors: u32,
    pub count_with_ancestors: u64,
}

/// The R2–R9 package-selection read surface.
pub trait PackageView: Send + Sync {
    fn ancestor_score_iter(&self) -> Box<dyn Iterator<Item = EntryId> + '_>;
    fn mining_entry(&self, id: EntryId) -> MiningEntry<'_>;
    fn ancestors_no_limits(&self, id: EntryId) -> Vec<EntryId>;
    fn descendants(&self, id: EntryId) -> Vec<EntryId>;
    fn service_updates_before_key_change(
        &self,
        key_change: EntryId,
        dmn_tip_list: &MnList,
    ) -> Vec<EntryId>;
    fn is_unorderable_service_update(
        &self,
        tx: &Tx,
        ancestors: &[EntryId],
        dmn_tip_list: &MnList,
    ) -> bool;
    fn key_change_target(&self, id: EntryId, dmn_tip_list: &MnList) -> Option<ProTxHash>;
    fn entry_id(&self, txid: &TxHash) -> Option<EntryId>;
}

/// The mempool owner surface. `with_read` calls its closure while the package view is held.
pub trait MiningMempool: Send + Sync {
    fn with_read(&self, f: &mut dyn FnMut(&dyn PackageView));
    /// Core's lock-free `nTransactionsUpdated` SeqCst read.
    fn transactions_updated(&self) -> u32;
}

/// ChainLock/InstantSend safety state used by package validation.
pub trait MiningSafety: Send + Sync {
    fn is_tx_safe_for_mining(&self, txid: &TxHash) -> bool;
}

/// Node state read by the GBT engine.
pub trait NodeStatus: Send + Sync {
    fn has_connman(&self) -> bool;
    fn peer_count(&self) -> usize;
    fn mn_synced(&self) -> bool;
    fn rpc_running(&self) -> bool;
}
