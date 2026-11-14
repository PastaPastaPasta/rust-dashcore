//! Host services used by the CoinJoin runtime. Core citations are Dash Core v24.0.0.

use dash_coinjoin::collateral::MempoolAccept;
use dash_coinjoin::dstx::DstxChain;
use dash_coinjoin::inouts::CoinView;
use dash_coinjoin::queue::{MixingMasternode, TipMasternodes};
use dash_num::Hash256;
use dash_p2p::{OutMsg, PeerId, Service};
use dash_p2p_core::{DsQueue, Inventory};
use dash_primitives::{BlockHash, OutPoint, Transaction, TxHash};

/// The active masternode's public identity and operator signing service.
pub trait ActiveMn {
    /// `CActiveMasternodeManager::GetOutPoint`, called at `src/coinjoin/server.cpp:83`.
    fn outpoint(&self) -> OutPoint;

    /// `CActiveMasternodeManager::GetProTxHash`, called at `src/coinjoin/server.cpp:496`.
    fn pro_tx_hash(&self) -> TxHash;

    /// `CActiveMasternodeManager::SignBasic`, called at `src/coinjoin/server.cpp:498`.
    fn sign_basic(&mut self, hash: &Hash256) -> Vec<u8>;
}

/// Masternode lookups. Inherited [`TipMasternodes::valid_mn`] is Core's
/// `GetListAtChainTip().GetValidMN` (`src/coinjoin/server.cpp:149-151`);
/// [`TipMasternodes::enabled_count`] is `GetCounts().enabled()` (`src/coinjoin/server.cpp:101`).
pub trait MnLists: TipMasternodes {
    /// `GetListAtChainTip().GetValidMNByCollateral`, `src/coinjoin/server.cpp:82-83`.
    fn valid_by_collateral(&self, outpoint: &OutPoint) -> Option<MixingMasternode>;

    /// `GetListForBlock(pindex).GetMN`, `src/net_processing.cpp:3675-3682`.
    /// `depth` is blocks below the active tip (0 is the tip); includes invalid masternodes.
    fn mn_at_depth(&self, depth: i32, pro_tx_hash: &TxHash) -> Option<MixingMasternode>;
}

/// The mempool and coins view. Inherited [`MempoolAccept::test_accept`] is
/// `ProcessTransaction(tx, true)` (`src/coinjoin/coinjoin.cpp:444`), returning base fees in
/// duffs (`i64`), and [`CoinView::coin`] is `CCoinsViewMemPool::GetCoin`
/// (`src/coinjoin/coinjoin.cpp:478-483`). [`CoinView::is_instantsend_locked`] is
/// `CInstantSendManager::IsLocked` (`src/coinjoin/coinjoin.cpp:483`).
pub trait Mempool: MempoolAccept + CoinView {
    /// `ProcessTransaction(tx, false)` in `ATMPIfSaneFee`, `src/coinjoin/coinjoin.cpp:455`.
    /// Submission only: the caller performs test-accept and fee sanity first.
    fn submit(&mut self, tx: &Transaction) -> bool;

    /// `CTxMemPool::PrioritiseTransaction`, `src/net_processing.cpp:3716` and
    /// `src/coinjoin/server.cpp:481`; `fee_delta` is in duffs.
    fn prioritise(&mut self, txid: &TxHash, fee_delta: i64);
}

/// A listener for accepted mempool transactions, separate from the admission service.
pub trait MempoolListener {
    /// `CDSNotificationInterface::TransactionAddedToMempool`,
    /// `src/dsnotificationinterface.cpp:74-78`, forwarding to
    /// `CDSTXManager::TransactionAddedToMempool`, `src/coinjoin/coinjoin.cpp:626-631`.
    fn transaction_added_to_mempool(&self, tx: &Transaction);
}

/// The InstantSend lock set.
pub trait IsLocks {
    /// `CInstantSendManager::IsLocked`, `src/coinjoin/coinjoin.cpp:483`.
    fn is_locked(&self, txid: &TxHash) -> bool;
}

impl<T: CoinView + ?Sized> IsLocks for T {
    fn is_locked(&self, txid: &TxHash) -> bool {
        self.is_instantsend_locked(txid)
    }
}

/// The active chain, deployment status, ChainLocks and masternode sync status.
pub trait ChainView {
    /// `ActiveChain().Tip()`, `src/net_processing.cpp:3656`; height and block hash,
    /// or `None` without an active tip.
    fn tip(&self) -> Option<(i32, BlockHash)>;

    /// `DeploymentActiveAt(tip, V24)` in `IsPromotionDemotionActive(false)`,
    /// `src/coinjoin/coinjoin.cpp:255-261`.
    fn v24_active_at_tip(&self) -> bool;

    /// `DeploymentActiveAfter(tip, V24)` in `IsPromotionDemotionActive(true)`,
    /// `src/coinjoin/coinjoin.cpp:255-261` (called at `src/net_processing.cpp:3668`).
    fn v24_active_after_tip(&self) -> bool;

    /// `chainlock::Chainlocks::HasChainLock`, `src/coinjoin/coinjoin.cpp:97`.
    fn has_chain_lock(&self, height: i32, hash: &BlockHash) -> bool;

    /// `CMasternodeSync::IsBlockchainSynced`, `src/coinjoin/server.cpp:53` and
    /// `src/dsnotificationinterface.cpp:65`.
    fn blockchain_synced(&self) -> bool;
}

/// Combines chain and historical masternode lookups for the existing pure [`DstxChain`] seam.
pub struct DstxChainAdapter<'a, C: ?Sized, M: ?Sized> {
    /// The active chain and deployment status.
    pub chain: &'a C,
    /// The historical masternode lists.
    pub mns: &'a M,
}

impl<C: ChainView + ?Sized, M: MnLists + ?Sized> DstxChain for DstxChainAdapter<'_, C, M> {
    fn tip_height(&self) -> Option<i32> {
        self.chain.tip().map(|(height, _)| height)
    }

    fn v24_active_at_tip(&self) -> bool {
        self.chain.v24_active_at_tip()
    }

    fn v24_active_after_tip(&self) -> bool {
        self.chain.v24_active_after_tip()
    }

    fn mn_at_depth(&self, depth: i32, pro_tx_hash: &TxHash) -> Option<MixingMasternode> {
        self.mns.mn_at_depth(depth, pro_tx_hash)
    }
}

/// Peer effects, used by message handlers and scheduler ticks.
pub trait PeerSink {
    /// `connman.PushMessage`, `src/coinjoin/server.cpp:1328` (`PushStatus`).
    fn push_to(&mut self, peer: PeerId, msg: OutMsg);

    /// `connman.ForNode(CService, ...)` with `PushMessage`,
    /// `src/coinjoin/server.cpp:1312-1315,1375-1378`; false when no participant is connected.
    fn push_to_addr(&mut self, addr: &Service, msg: OutMsg) -> bool;

    /// Whether `connman.ForNode(addr, ...)` would find a connected peer for `addr`, without
    /// sending anything: the participant check of `src/coinjoin/server.cpp:1311-1322` and
    /// `:1339-1364`, which the pure session reads through `ServerEnv::is_connected` before it
    /// returns its effects.
    fn is_connected(&self, addr: &Service) -> bool;

    /// `PeerMisbehaving`, `src/net_processing.cpp:6919-6923`.
    fn misbehaving(&mut self, peer: PeerId, score: i32, reason: &str);

    /// `PeerEraseObjectRequest`, `src/net_processing.cpp:6930-6935`: complete this peer's
    /// request only.
    fn erase_object_request(&mut self, peer: PeerId, inv: Inventory);

    /// `PeerForgetObjectRequest`, `src/net_processing.cpp:6974-6977`: forget across peers.
    fn forget_object_request(&mut self, inv: Inventory);

    /// `PeerRelayInv`, `src/net_processing.cpp:6984-6987`, for an inventory that is not a
    /// transaction.
    fn relay_inv(&mut self, inv: Inventory);

    /// `PeerRelayInv`, `src/net_processing.cpp:6984-6987`, for an `MSG_TX` or `MSG_DSTX`
    /// inventory: `PushInv` skips a peer whose announcement clock is unset and queues the
    /// hash in its transaction inventory set (`src/net_processing.cpp:1239-1256`), unlike
    /// [`PeerSink::relay_inv`]. Not `RelayTransaction` ([`PeerSink::relay_transaction`]).
    fn relay_tx_inv(&mut self, inv: Inventory);

    /// `PeerRelayDSQ`, `src/net_processing.cpp:6999-7002`, forwarding to
    /// `RelayDSQ`, `src/net_processing.cpp:2560-2585`.
    fn relay_dsq(&mut self, dsq: &DsQueue);

    /// `PeerRelayTransaction`, `src/net_processing.cpp:7004-7007`, forwarding to
    /// `RelayTransaction`, `src/net_processing.cpp:2650`.
    fn relay_transaction(&mut self, txid: &TxHash);
}
