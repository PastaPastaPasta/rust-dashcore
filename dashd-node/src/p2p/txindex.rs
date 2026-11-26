//! The transaction index's term in a transaction's `AlreadyHave`: Core's `g_txindex->HasTx`.
// Core: v24.0.2 src/net_processing.cpp:2410-2413, src/index/txindex.cpp:108-112

use dash_primitives::TxHash;
use dashd_indexes::TxIndexReader as _;
use dashd_p2p::processing::seams::TxIndexLookup;

use crate::indexes::TxIndexHandle;

/// The processor's [`TxIndexLookup`] over the node's txindex handle, which reads no index before
/// step 8 opens it or after shutdown closes it.
pub(crate) struct NodeTxIndex(pub(crate) TxIndexHandle);

impl TxIndexLookup for NodeTxIndex {
    fn has_tx(&self, hash: &TxHash) -> bool {
        self.0.has_tx(hash).unwrap_or_else(|e| {
            tracing::debug!(target: "net", "txindex: {e}");
            false
        })
    }
}
