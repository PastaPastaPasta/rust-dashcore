use dash_consensus::TxRef;
use dash_dml::MnList;

use super::super::PoolState;
use crate::types::MemPoolRemovalReason;

impl PoolState {
    /// Core v24.0.0 `removeForBlock`: process each mined transaction in block order.
    pub fn remove_for_block(&mut self, mined: &[TxRef], tip_list: &MnList) {
        for tx in mined {
            let id = tx.txid();
            if self.entries.contains_key(&id) {
                let roots = std::collections::BTreeSet::from([id]);
                self.remove_staged(&roots, true, MemPoolRemovalReason::Block);
            }
            self.remove_conflicts(tx);
            self.remove_pro_tx_conflicts(tx, tip_list);
            self.remove_asset_unlock_conflicts(tx);
            self.clear_prioritisation(&id);
        }
    }
}
