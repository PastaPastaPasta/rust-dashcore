use super::manager::MEMPOOL_TX_EXPIRY;
use crate::error::SyncResult;
use crate::network::{Message, MessageType, NetworkEvent, RequestSender};
use crate::sync::{
    ManagerIdentifier, MempoolManager, SyncEvent, SyncManager, SyncManagerProgress, SyncState,
};
use async_trait::async_trait;
use dashcore::network::message::NetworkMessage;
use key_wallet_manager::WalletInterface;

#[async_trait]
impl<W: WalletInterface + 'static> SyncManager for MempoolManager<W> {
    fn identifier(&self) -> ManagerIdentifier {
        ManagerIdentifier::Mempool
    }

    fn state(&self) -> SyncState {
        self.progress.state()
    }

    fn set_state(&mut self, state: SyncState) {
        self.progress.set_state(state);
    }

    fn wanted_message_types(&self) -> &'static [MessageType] {
        &[MessageType::Inv, MessageType::Tx]
    }

    async fn start_sync(&mut self, requests: &RequestSender) -> SyncResult<Vec<SyncEvent>> {
        // After a full disconnect, re-activate mempool on all connected peers
        self.activate_all_peers(requests).await?;
        let has_activated = self.peers.values().any(|v| v.is_some());
        if has_activated {
            self.set_state(SyncState::Synced);
            tracing::info!("Mempool manager re-activated after disconnect recovery");
        }
        // If no peers could be activated, stay in WaitingForConnections so the
        // next PeersUpdated event will retry activation.
        Ok(vec![])
    }

    fn on_disconnect(&mut self) {
        self.clear_pending();
    }

    async fn handle_message(
        &mut self,
        msg: Message,
        requests: &RequestSender,
    ) -> SyncResult<Vec<SyncEvent>> {
        match msg.inner() {
            NetworkMessage::Inv(inv) => self.handle_inv(inv, msg.peer_address(), requests).await,
            NetworkMessage::Tx(tx) => {
                self.handle_tx((**tx).clone(), msg.peer_address(), requests).await
            }
            _ => Ok(vec![]),
        }
    }

    async fn handle_sync_event(
        &mut self,
        event: &SyncEvent,
        requests: &RequestSender,
    ) -> SyncResult<Vec<SyncEvent>> {
        match event {
            // Activate as soon as filter sync completes — the wallet's address
            // and UTXO set is fully populated at this point.
            SyncEvent::FiltersSyncComplete {
                ..
            } => {
                let first = !self.filters_synced;
                self.filters_synced = true;
                if self.state() == SyncState::Synced && first && self.activate_during_sync {
                    self.refresh_after_filter_sync(requests).await?;
                    return Ok(vec![]);
                }
                if self.state() != SyncState::Synced {
                    self.activate_all_peers(requests).await?;
                    let has_activated = self.peers.values().any(|v| v.is_some());
                    if has_activated {
                        self.set_state(SyncState::Synced);
                        tracing::info!("Mempool manager activated after filter sync");
                        return Ok(vec![]);
                    } else {
                        tracing::warn!(
                            "Filter sync complete but no peers available for mempool activation"
                        );
                    }
                }
                Ok(vec![])
            }
            SyncEvent::BlockProcessed {
                confirmed_txids,
                ..
            } => {
                // Remove confirmed transactions from mempool.
                // Bloom filter rebuild is handled by the tick's revision check.
                if !confirmed_txids.is_empty() {
                    return Ok(self.remove_confirmed(confirmed_txids));
                }
                Ok(vec![])
            }
            SyncEvent::InstantLockReceived {
                instant_lock,
                ..
            } => Ok(self.process_instant_send(instant_lock.clone()).await),
            _ => Ok(vec![]),
        }
    }

    async fn tick(&mut self, requests: &RequestSender) -> SyncResult<Vec<SyncEvent>> {
        // Broadcast bookkeeping runs regardless of sync state: broadcasts can
        // be initiated (and time out) before the mempool phase is synced.
        let events = self.expire_broadcasts();
        self.rebroadcast_if_due(requests).await;

        if self.state() != SyncState::Synced {
            return Ok(events);
        }

        // Prune expired transactions periodically
        self.prune_expired(MEMPOOL_TX_EXPIRY);

        // Prune pending requests that never received a response
        self.prune_pending_requests();

        // Send queued getdata requests now that slots may have freed up
        self.send_queued(requests).await?;

        // Rebuild bloom filter if the wallet's monitored set has changed.
        //
        // We poll the revision counter rather than using push-based wallet events
        // for simplicity: the revision lives on `ManagedCoreFundsAccount` and auto-bumps
        // on address generation and UTXO mutations, giving us a single source of
        // truth without needing event emission after every wallet operation.
        // Adding a push-based approach would require a new `select!` branch in the
        // shared `SyncManager::run` loop or a `WalletEvent` bridge — complexity
        // that isn't justified given the 100ms tick latency is negligible for bloom
        // filter rebuilds and the read lock is non-contending.
        //
        // Activated during sync, the filter is rebuilt once filter sync
        // completes (`refresh_after_filter_sync`), not on every block the
        // scan credits.
        if self.activate_during_sync && !self.filters_synced {
            return Ok(events);
        }
        let current_revision = self.wallet.read().await.monitor_revision();
        if current_revision != self.last_monitor_revision {
            tracing::info!("Wallet monitor revision changed, rebuilding bloom filter");
            self.rebuild_filter(requests).await?;
            self.last_monitor_revision = current_revision;
        }

        Ok(events)
    }

    async fn handle_network_event(
        &mut self,
        event: &NetworkEvent,
        requests: &RequestSender,
    ) -> SyncResult<Vec<SyncEvent>> {
        match event {
            NetworkEvent::PeerConnected {
                address,
            } => {
                self.handle_peer_connected(*address);
                // If synced, or activating during sync, activate the new peer immediately
                if (self.state() == SyncState::Synced || self.activate_during_sync)
                    && self.peers.get(address).is_some_and(|v| v.is_none())
                {
                    tracing::info!("Activating mempool on newly connected peer {}", address);
                    self.activate_peer(*address, requests).await?;
                    self.set_state(SyncState::Synced);
                }
            }
            NetworkEvent::PeerDisconnected {
                address,
            } => {
                self.handle_peer_disconnected(*address);
            }
            NetworkEvent::PeersUpdated {
                connected_count,
                best_height,
                ..
            } => {
                if let Some(best_height) = best_height {
                    self.update_target_height(*best_height);
                }
                if *connected_count == 0 {
                    self.stop_sync();
                } else if self.state() == SyncState::WaitingForConnections
                    || (self.activate_during_sync && self.state() != SyncState::Synced)
                {
                    return self.start_sync(requests).await;
                }
            }
        }
        Ok(vec![])
    }

    fn progress(&self) -> SyncManagerProgress {
        SyncManagerProgress::Mempool(self.progress.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::config::MempoolStrategy;
    use crate::network::NetworkRequest;
    use crate::sync::BroadcastConfig;
    use crate::test_utils::test_socket_address;
    use dashcore::hashes::Hash;
    use key_wallet_manager::test_utils::MockWallet;
    use std::collections::{BTreeMap, BTreeSet};
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tokio::sync::{mpsc, RwLock};

    fn create_test_manager(
    ) -> (MempoolManager<MockWallet>, RequestSender, mpsc::UnboundedReceiver<NetworkRequest>) {
        let wallet = Arc::new(RwLock::new(MockWallet::new()));
        let (tx, rx) = mpsc::unbounded_channel::<NetworkRequest>();
        let requests = RequestSender::new(tx);

        let manager = MempoolManager::new(
            wallet,
            MempoolStrategy::FetchAll,
            1000,
            0,
            BroadcastConfig::default(),
        );

        (manager, requests, rx)
    }

    #[test]
    fn test_sync_manager_trait_basics() {
        let (mut manager, _, _rx) = create_test_manager();

        assert_eq!(manager.identifier(), ManagerIdentifier::Mempool);
        assert_eq!(manager.state(), SyncState::WaitForEvents);

        let types = manager.wanted_message_types();
        assert!(types.contains(&MessageType::Inv));
        assert!(types.contains(&MessageType::Tx));
        assert_eq!(types.len(), 2);

        manager.set_state(SyncState::Synced);
        assert_eq!(manager.state(), SyncState::Synced);

        assert!(matches!(manager.progress(), SyncManagerProgress::Mempool(_)));
    }

    #[tokio::test]
    async fn test_filters_sync_complete_activates() {
        let (mut manager, requests, _rx) = create_test_manager();
        let peer = crate::test_utils::test_socket_address(1);
        manager.handle_peer_connected(peer);

        let event = SyncEvent::FiltersSyncComplete {
            tip_height: 1000,
        };

        let events = manager.handle_sync_event(&event, &requests).await.unwrap();
        assert!(events.is_empty());
        assert_eq!(manager.state(), SyncState::Synced);
        assert!(matches!(manager.peers.get(&peer), Some(Some(_))));
    }

    #[tokio::test]
    async fn test_filters_sync_complete_subsequent_is_noop() {
        let (mut manager, requests, _rx) = create_test_manager();
        manager.handle_peer_connected(crate::test_utils::test_socket_address(1));

        // Activate first
        let event0 = SyncEvent::FiltersSyncComplete {
            tip_height: 1000,
        };
        manager.handle_sync_event(&event0, &requests).await.unwrap();

        // Subsequent filter sync completions should not change state
        let event1 = SyncEvent::FiltersSyncComplete {
            tip_height: 1001,
        };
        let events = manager.handle_sync_event(&event1, &requests).await.unwrap();
        assert!(events.is_empty());
        assert_eq!(manager.state(), SyncState::Synced);
    }

    #[tokio::test]
    async fn test_reactivation_after_disconnect() {
        let (mut manager, requests, _rx) = create_test_manager();
        let peer = test_socket_address(1);
        manager.handle_peer_connected(peer);

        // Initial activation
        let event = SyncEvent::FiltersSyncComplete {
            tip_height: 1000,
        };
        let events = manager.handle_sync_event(&event, &requests).await.unwrap();
        assert!(events.is_empty());
        assert_eq!(manager.state(), SyncState::Synced);

        // Simulate disconnect by resetting state
        manager.set_state(SyncState::WaitForEvents);

        // Re-sync should re-activate
        let event = SyncEvent::FiltersSyncComplete {
            tip_height: 1001,
        };
        let events = manager.handle_sync_event(&event, &requests).await.unwrap();
        assert!(events.is_empty());
        assert_eq!(manager.state(), SyncState::Synced);
    }

    #[tokio::test]
    async fn test_peer_connect_activates_when_synced() {
        let (mut manager, requests, _rx) = create_test_manager();
        let peer1 = test_socket_address(1);
        manager.handle_peer_connected(peer1);

        // Activate via SyncComplete
        let event = SyncEvent::FiltersSyncComplete {
            tip_height: 1000,
        };
        manager.handle_sync_event(&event, &requests).await.unwrap();
        assert!(matches!(manager.peers.get(&peer1), Some(Some(_))));

        // New peer connects while synced => should activate immediately
        let peer2 = test_socket_address(2);
        let connect = NetworkEvent::PeerConnected {
            address: peer2,
        };
        let events = manager.handle_network_event(&connect, &requests).await.unwrap();
        assert!(events.is_empty());
        assert!(matches!(manager.peers.get(&peer2), Some(Some(_))));
    }

    /// A manager that activates peers before filter sync completes, its
    /// request receiver, drained of nothing yet.
    fn create_during_sync_manager(
    ) -> (MempoolManager<MockWallet>, RequestSender, mpsc::UnboundedReceiver<NetworkRequest>) {
        let (manager, requests, rx) = create_test_manager();
        (manager.with_activation_during_sync(true), requests, rx)
    }

    fn drain(rx: &mut mpsc::UnboundedReceiver<NetworkRequest>) -> Vec<NetworkRequest> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    fn requested_mempool(sent: &[NetworkRequest], peer: SocketAddr) -> bool {
        sent.iter().any(|r| {
            matches!(r, NetworkRequest::SendMessageToPeer(NetworkMessage::MemPool, p) if *p == peer)
        })
    }

    fn requested_tx(sent: &[NetworkRequest], peer: SocketAddr, txid: dashcore::Txid) -> bool {
        sent.iter().any(|r| match r {
            NetworkRequest::SendMessageToPeer(NetworkMessage::GetData(inv), p) => {
                *p == peer
                    && inv.contains(&dashcore::network::message_blockdata::Inventory::Transaction(
                        txid,
                    ))
            }
            _ => false,
        })
    }

    #[tokio::test]
    async fn test_peer_connect_before_filter_sync_waits_by_default() {
        let (mut manager, requests, mut rx) = create_test_manager();
        let peer = test_socket_address(1);
        let connect = NetworkEvent::PeerConnected {
            address: peer,
        };
        manager.handle_network_event(&connect, &requests).await.unwrap();
        let update = NetworkEvent::PeersUpdated {
            connected_count: 1,
            addresses: vec![peer],
            best_height: Some(1000),
        };
        manager.handle_network_event(&update, &requests).await.unwrap();

        assert_eq!(manager.state(), SyncState::WaitForEvents);
        assert!(matches!(manager.peers.get(&peer), Some(None)));
        assert!(!requested_mempool(&drain(&mut rx), peer));

        let txid = dashcore::Txid::from_byte_array([7; 32]);
        let inv = [dashcore::network::message_blockdata::Inventory::Transaction(txid)];
        manager.handle_inv(&inv, peer, &requests).await.unwrap();
        assert!(!requested_tx(&drain(&mut rx), peer, txid), "no relay before filter sync");
    }

    #[tokio::test]
    async fn test_peer_connect_during_sync_activates_and_fetches_announced_tx() {
        let (mut manager, requests, mut rx) = create_during_sync_manager();
        let peer = test_socket_address(1);
        let connect = NetworkEvent::PeerConnected {
            address: peer,
        };
        manager.handle_network_event(&connect, &requests).await.unwrap();

        assert_eq!(manager.state(), SyncState::Synced);
        assert!(matches!(manager.peers.get(&peer), Some(Some(_))));
        assert!(requested_mempool(&drain(&mut rx), peer));

        let txid = dashcore::Txid::from_byte_array([7; 32]);
        let inv = [dashcore::network::message_blockdata::Inventory::Transaction(txid)];
        manager.handle_inv(&inv, peer, &requests).await.unwrap();
        assert!(requested_tx(&drain(&mut rx), peer, txid));

        // Filter sync completing later asks for the mempool again, once.
        let event = SyncEvent::FiltersSyncComplete {
            tip_height: 1000,
        };
        manager.handle_sync_event(&event, &requests).await.unwrap();
        assert_eq!(manager.state(), SyncState::Synced);
        assert!(requested_mempool(&drain(&mut rx), peer));
        let event = SyncEvent::FiltersSyncComplete {
            tip_height: 1001,
        };
        manager.handle_sync_event(&event, &requests).await.unwrap();
        assert!(drain(&mut rx).is_empty(), "only the first completion asks again");
    }

    /// A transaction downloaded during sync and found irrelevant (the wallet
    /// had not derived its address yet) is fetched again once filter sync
    /// completes.
    #[tokio::test]
    async fn test_tx_irrelevant_during_sync_is_fetched_again_after_filter_sync() {
        let (mut manager, requests, mut rx) = create_during_sync_manager();
        let peer = test_socket_address(1);
        let connect = NetworkEvent::PeerConnected {
            address: peer,
        };
        manager.handle_network_event(&connect, &requests).await.unwrap();
        let tx = dashcore::Transaction {
            version: 1,
            lock_time: 0,
            input: vec![],
            output: vec![],
            special_transaction_payload: None,
        };
        let txid = tx.txid();
        let inv = [dashcore::network::message_blockdata::Inventory::Transaction(txid)];
        manager.handle_inv(&inv, peer, &requests).await.unwrap();
        manager.handle_tx(tx, peer, &requests).await.unwrap();
        assert!(!manager.transactions.contains_key(&txid), "irrelevant to the mock wallet");
        drain(&mut rx);
        manager.handle_inv(&inv, peer, &requests).await.unwrap();
        assert!(!requested_tx(&drain(&mut rx), peer, txid), "deduplicated while syncing");

        let event = SyncEvent::FiltersSyncComplete {
            tip_height: 1000,
        };
        manager.handle_sync_event(&event, &requests).await.unwrap();
        assert!(requested_mempool(&drain(&mut rx), peer));
        manager.handle_inv(&inv, peer, &requests).await.unwrap();
        assert!(requested_tx(&drain(&mut rx), peer, txid), "fetched again after the scan");
    }

    /// Activated during sync with a bloom filter, the scan's address and coin
    /// changes do not reload the filter on every tick; filter sync completing
    /// reloads it once and asks for the mempool again.
    #[tokio::test]
    async fn test_bloom_rebuild_waits_for_filter_sync_when_activated_during_sync() {
        let address = |byte: u8| {
            let mut script = vec![0x76, 0xa9, 0x14];
            script.extend([byte; 20]);
            script.extend([0x88, 0xac]);
            dashcore::Address::from_script(
                &dashcore::ScriptBuf::from_bytes(script),
                dashcore::Network::Testnet,
            )
            .unwrap()
        };
        let mut mock = MockWallet::new();
        mock.set_addresses(vec![address(0xab)]);
        let revision = mock.monitor_revision();
        let wallet = Arc::new(RwLock::new(mock));
        let (tx, mut rx) = mpsc::unbounded_channel::<NetworkRequest>();
        let requests = RequestSender::new(tx);
        let mut manager = MempoolManager::new(
            wallet.clone(),
            MempoolStrategy::BloomFilter,
            1000,
            revision,
            BroadcastConfig::default(),
        )
        .with_activation_during_sync(true);
        let peer = test_socket_address(1);
        let connect = NetworkEvent::PeerConnected {
            address: peer,
        };
        manager.handle_network_event(&connect, &requests).await.unwrap();
        drain(&mut rx);

        wallet.write().await.set_addresses(vec![address(0xab), address(0xcd)]);
        manager.tick(&requests).await.unwrap();
        assert!(drain(&mut rx).is_empty(), "no reload while the filters sync");

        let event = SyncEvent::FiltersSyncComplete {
            tip_height: 1000,
        };
        manager.handle_sync_event(&event, &requests).await.unwrap();
        let sent = drain(&mut rx);
        assert!(sent.iter().any(|r| matches!(
            r,
            NetworkRequest::SendMessageToPeer(NetworkMessage::FilterLoad(_), p) if *p == peer
        )));
        assert!(requested_mempool(&sent, peer));
        manager.tick(&requests).await.unwrap();
        assert!(drain(&mut rx).is_empty(), "the reload took the new revision");
    }

    #[tokio::test]
    async fn test_peers_updated_during_sync_activates_known_peers() {
        let (mut manager, requests, mut rx) = create_during_sync_manager();
        let peer = test_socket_address(1);
        manager.handle_peer_connected(peer);
        let update = NetworkEvent::PeersUpdated {
            connected_count: 1,
            addresses: vec![peer],
            best_height: Some(1000),
        };
        manager.handle_network_event(&update, &requests).await.unwrap();

        assert_eq!(manager.state(), SyncState::Synced);
        assert!(matches!(manager.peers.get(&peer), Some(Some(_))));
        assert!(requested_mempool(&drain(&mut rx), peer));
    }

    #[tokio::test]
    async fn test_reconnect_during_sync_activates_again() {
        let (mut manager, requests, mut rx) = create_during_sync_manager();
        let peer = test_socket_address(1);
        let connect = NetworkEvent::PeerConnected {
            address: peer,
        };
        manager.handle_network_event(&connect, &requests).await.unwrap();
        let disconnect = NetworkEvent::PeerDisconnected {
            address: peer,
        };
        manager.handle_network_event(&disconnect, &requests).await.unwrap();
        let none = NetworkEvent::PeersUpdated {
            connected_count: 0,
            addresses: vec![],
            best_height: None,
        };
        manager.handle_network_event(&none, &requests).await.unwrap();
        assert_eq!(manager.state(), SyncState::WaitingForConnections);
        drain(&mut rx);

        manager.handle_network_event(&connect, &requests).await.unwrap();
        assert_eq!(manager.state(), SyncState::Synced);
        assert!(requested_mempool(&drain(&mut rx), peer));
    }

    #[tokio::test]
    async fn test_network_event_peer_connect_disconnect() {
        let (mut manager, requests, _rx) = create_test_manager();

        let peer1 = test_socket_address(1);
        let peer2 = test_socket_address(2);

        // Connecting peers should return empty events (not synced yet)
        let connect1 = NetworkEvent::PeerConnected {
            address: peer1,
        };
        let events = manager.handle_network_event(&connect1, &requests).await.unwrap();
        assert!(events.is_empty());
        assert!(manager.peers.contains_key(&peer1));

        let connect2 = NetworkEvent::PeerConnected {
            address: peer2,
        };
        let events = manager.handle_network_event(&connect2, &requests).await.unwrap();
        assert!(events.is_empty());
        assert_eq!(manager.peers.len(), 2);

        let disconnect1 = NetworkEvent::PeerDisconnected {
            address: peer1,
        };
        let events = manager.handle_network_event(&disconnect1, &requests).await.unwrap();
        assert!(events.is_empty());

        // Still have peer2 available
        assert!(manager.peers.contains_key(&peer2));
        assert_eq!(manager.peers.len(), 1);

        // Disconnecting an already-disconnected peer should not error
        let events = manager.handle_network_event(&disconnect1, &requests).await.unwrap();
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn test_block_processed_removes_confirmed_txids() {
        let (mut manager, requests, _rx) = create_test_manager();
        let peer = test_socket_address(1);
        manager.handle_peer_connected(peer);

        // Activate
        let sync = SyncEvent::FiltersSyncComplete {
            tip_height: 1000,
        };
        manager.handle_sync_event(&sync, &requests).await.unwrap();

        // Add transactions to mempool
        let mut txids = Vec::new();
        for i in 0..2u32 {
            let tx = dashcore::Transaction {
                version: 1,
                lock_time: i,
                input: vec![],
                output: vec![],
                special_transaction_payload: None,
            };
            let txid = tx.txid();
            txids.push(txid);
            manager.transactions.insert(
                txid,
                crate::types::UnconfirmedTransaction::new(
                    tx,
                    dashcore::Amount::from_sat(0),
                    false,
                    false,
                    Vec::new(),
                    0,
                ),
            );
        }

        let event = SyncEvent::BlockProcessed {
            block_hash: dashcore::BlockHash::all_zeros(),
            height: 1001,
            wallets: BTreeSet::new(),
            new_scripts: BTreeMap::new(),
            confirmed_txids: txids.clone(),
        };
        let events = manager.handle_sync_event(&event, &requests).await.unwrap();
        assert!(events.is_empty());

        assert!(manager.transactions.is_empty());
    }

    #[tokio::test]
    async fn test_instant_lock_received_marks_transaction() {
        let (mut manager, requests, _rx) = create_test_manager();
        let peer = test_socket_address(1);
        manager.handle_peer_connected(peer);

        // Activate
        let sync = SyncEvent::FiltersSyncComplete {
            tip_height: 1000,
        };
        manager.handle_sync_event(&sync, &requests).await.unwrap();

        // Add a transaction to mempool
        let tx = dashcore::Transaction {
            version: 1,
            lock_time: 0,
            input: vec![],
            output: vec![],
            special_transaction_payload: None,
        };
        let txid = tx.txid();
        manager.transactions.insert(
            txid,
            crate::types::UnconfirmedTransaction::new(
                tx,
                dashcore::Amount::from_sat(0),
                false,
                false,
                Vec::new(),
                0,
            ),
        );

        // Fire InstantLockReceived with a lock whose txid matches
        let mut is_lock = dashcore::InstantLock::dummy(0..1);
        is_lock.txid = txid;

        let event = SyncEvent::InstantLockReceived {
            instant_lock: is_lock,
            validated: true,
        };
        let events = manager.handle_sync_event(&event, &requests).await.unwrap();
        assert!(events.is_empty());

        assert!(manager.transactions.get(&txid).unwrap().is_instant_send);

        use dashcore::{InstantLock, OutPoint, Transaction, TxOut};
        use key_wallet::account::StandardAccountType;
        use key_wallet::managed_account::managed_account_trait::ManagedAccountTrait;
        use key_wallet::managed_account::transaction_record::TransactionDirection;
        use key_wallet::test_utils::TestWalletContext;
        use key_wallet::transaction_checking::TransactionContext;
        use key_wallet::wallet::managed_wallet_info::managed_account_operations::ManagedAccountOperations;
        use key_wallet::wallet::ManagedWalletInfo;
        use key_wallet::AccountType;
        use key_wallet_manager::{WalletEvent, WalletManager};

        let mut ctx = TestWalletContext::new_random();
        let account_type = AccountType::Standard {
            index: 1,
            standard_account_type: StandardAccountType::BIP44Account,
        };
        ctx.wallet.add_account(account_type, None).unwrap();
        let mut preview =
            ManagedWalletInfo::from_wallet_with_name(&ctx.wallet, "preview".into(), 0);
        let address = preview
            .bip44_managed_account_at_index_mut(1)
            .unwrap()
            .next_receive_address(None, true)
            .unwrap();
        let mut funding = Transaction::dummy(&ctx.receive_address, 0..1, &[100_000]);
        funding.output.push(TxOut {
            value: 50_000,
            script_pubkey: address.script_pubkey(),
        });
        let mut spender = Transaction::dummy(&address, 1..2, &[40_000]);
        let outpoint = OutPoint::new(funding.txid(), 1);
        spender.input[0].previous_output = outpoint;

        let mut wallet = WalletManager::new(dashcore::Network::Testnet);
        let wallet_id = wallet.insert_wallet(ctx.wallet, ctx.managed_wallet).unwrap();
        let wallet = Arc::new(RwLock::new(wallet));
        let mut manager = MempoolManager::new(
            wallet.clone(),
            MempoolStrategy::FetchAll,
            1000,
            0,
            BroadcastConfig::default(),
        );
        manager
            .handle_message(
                Message::new(peer, NetworkMessage::Tx(funding.clone().into())),
                &requests,
            )
            .await
            .unwrap();
        {
            let mut wallet = wallet.write().await;
            let (inner, info) = wallet.get_wallet_and_info_mut(&wallet_id).unwrap();
            info.add_managed_account(inner, account_type).unwrap();
        }
        manager
            .handle_message(
                Message::new(peer, NetworkMessage::Tx(spender.clone().into())),
                &requests,
            )
            .await
            .unwrap();
        let mut rx = wallet.read().await.subscribe_events();
        let lock = InstantLock {
            txid: funding.txid(),
            ..InstantLock::default()
        };
        let event = SyncEvent::InstantLockReceived {
            instant_lock: lock,
            validated: true,
        };
        manager.handle_sync_event(&event, &requests).await.unwrap();
        let mut corrections = Vec::new();
        let mut backfilled = 0;
        let mut locked = 0;
        while let Ok(event) = rx.try_recv() {
            match event {
                WalletEvent::TransactionDetected {
                    record,
                    ..
                } if record.txid == spender.txid() => {
                    corrections.push(record);
                }
                WalletEvent::TransactionDetected {
                    record,
                    ..
                } if record.txid == funding.txid() => {
                    assert_eq!(record.account_type, account_type);
                    backfilled += 1;
                }
                WalletEvent::TransactionInstantLocked {
                    txid,
                    ..
                } => {
                    assert_eq!(txid, funding.txid(), "funding lock must not lock the child");
                    locked += 1;
                }
                _ => {}
            }
        }
        assert_eq!(backfilled, 1);
        assert_eq!(locked, 1);
        assert_eq!(corrections.len(), 1);
        let corrected = &corrections[0];
        assert_eq!(corrected.net_amount, -10_000);
        assert_eq!(corrected.direction, TransactionDirection::Internal);
        assert_eq!(corrected.context, TransactionContext::Mempool);
        assert_eq!(corrected.input_details.len(), 1);
        {
            let wallet = wallet.read().await;
            let account = wallet
                .get_wallet_info(&wallet_id)
                .unwrap()
                .bip44_managed_account_at_index(1)
                .unwrap();
            assert_eq!(account.transactions()[&spender.txid()].net_amount, -10_000);
            assert!(!account.utxos.contains_key(&outpoint));
        }
        manager.handle_sync_event(&event, &requests).await.unwrap();
        manager
            .handle_message(
                Message::new(peer, NetworkMessage::Tx(funding.clone().into())),
                &requests,
            )
            .await
            .unwrap();
        assert!(rx.try_recv().is_err(), "duplicate lock/transaction must not emit events");

        // Both accounts already know these transactions; block processing can precede mempool cleanup.
        for confirmed in [false, true] {
            let mut tx = Transaction::dummy(&address, 10 + u8::from(confirmed)..12, &[50_000]);
            tx.output = funding.output.clone();
            manager
                .handle_message(
                    Message::new(peer, NetworkMessage::Tx(tx.clone().into())),
                    &requests,
                )
                .await
                .unwrap();
            let block_context =
                TransactionContext::InBlock(key_wallet::transaction_checking::BlockInfo::new(
                    1001,
                    dashcore::BlockHash::from_byte_array([0xab; 32]),
                    1700000000,
                ));
            if confirmed {
                wallet
                    .write()
                    .await
                    .check_transaction_in_all_wallets(&tx, block_context.clone(), true, true)
                    .await;
            }
            while rx.try_recv().is_ok() {}
            let event = SyncEvent::InstantLockReceived {
                instant_lock: InstantLock {
                    txid: tx.txid(),
                    ..InstantLock::default()
                },
                validated: true,
            };
            manager.handle_sync_event(&event, &requests).await.unwrap();
            let mut locks = 0;
            while let Ok(event) = rx.try_recv() {
                assert!(
                    matches!(event, WalletEvent::TransactionInstantLocked { txid, .. } if txid == tx.txid())
                );
                locks += 1;
            }
            assert_eq!(locks, 1, "one lock event per wallet, including after confirmation");
            {
                let wallet = wallet.read().await;
                for index in [0, 1] {
                    let account = wallet
                        .get_wallet_info(&wallet_id)
                        .unwrap()
                        .bip44_managed_account_at_index(index)
                        .unwrap();
                    let record = &account.transactions()[&tx.txid()];
                    if confirmed {
                        assert_eq!(record.context, block_context);
                    } else {
                        assert!(matches!(record.context, TransactionContext::InstantSend(_)));
                    }
                    assert!(account.utxos[&OutPoint::new(tx.txid(), index)].is_instantlocked);
                }
            }
            manager.handle_sync_event(&event, &requests).await.unwrap();
            assert!(rx.try_recv().is_err());
        }

        let foreign = dashcore::Address::dummy(dashcore::Network::Testnet, 999);
        let mut outgoing = Transaction::dummy(&foreign, 20..21, &[99_000]);
        outgoing.input[0].previous_output = OutPoint::new(funding.txid(), 0);
        manager
            .handle_message(
                Message::new(peer, NetworkMessage::Tx(outgoing.clone().into())),
                &requests,
            )
            .await
            .unwrap();
        while rx.try_recv().is_ok() {}
        let event = SyncEvent::InstantLockReceived {
            instant_lock: InstantLock {
                txid: outgoing.txid(),
                ..InstantLock::default()
            },
            validated: true,
        };
        manager.handle_sync_event(&event, &requests).await.unwrap();
        assert!(
            matches!(rx.try_recv().unwrap(), WalletEvent::TransactionInstantLocked { txid, .. } if txid == outgoing.txid())
        );
        assert!(rx.try_recv().is_err());
        let wallet = wallet.read().await;
        let account =
            wallet.get_wallet_info(&wallet_id).unwrap().first_bip44_managed_account().unwrap();
        assert!(matches!(
            account.transactions()[&outgoing.txid()].context,
            TransactionContext::InstantSend(_)
        ));
    }

    #[tokio::test]
    async fn test_peer_disconnect_removes_from_peers() {
        let (mut manager, requests, _rx) = create_test_manager();
        let peer = test_socket_address(1);
        manager.handle_peer_connected(peer);

        // Activate
        let sync = SyncEvent::FiltersSyncComplete {
            tip_height: 1000,
        };
        manager.handle_sync_event(&sync, &requests).await.unwrap();

        // Disconnect the only peer
        let disconnect = NetworkEvent::PeerDisconnected {
            address: peer,
        };
        let events = manager.handle_network_event(&disconnect, &requests).await.unwrap();
        assert!(events.is_empty());
        assert!(manager.peers.is_empty());
    }

    #[tokio::test]
    async fn test_sync_complete_no_peers_stays_inactive() {
        let (mut manager, requests, _rx) = create_test_manager();

        let event = SyncEvent::FiltersSyncComplete {
            tip_height: 1000,
        };
        let events = manager.handle_sync_event(&event, &requests).await.unwrap();

        assert!(events.is_empty());
        assert_eq!(manager.state(), SyncState::WaitForEvents);
        assert!(manager.peers.is_empty());
    }

    #[tokio::test]
    async fn test_start_sync_no_peers_stays_waiting() {
        let (mut manager, requests, _rx) = create_test_manager();

        // Simulate full disconnect setting state to WaitingForConnections
        manager.set_state(SyncState::WaitingForConnections);

        // start_sync with no peers should stay in WaitingForConnections
        let events = manager.start_sync(&requests).await.unwrap();
        assert!(events.is_empty());
        assert_eq!(manager.state(), SyncState::WaitingForConnections);
    }

    #[tokio::test]
    async fn test_disconnect_recovery_reactivates_on_reconnect() {
        let (mut manager, requests, _rx) = create_test_manager();
        let peer = test_socket_address(1);
        manager.handle_peer_connected(peer);

        // Activate via SyncComplete
        let event = SyncEvent::FiltersSyncComplete {
            tip_height: 1000,
        };
        manager.handle_sync_event(&event, &requests).await.unwrap();
        assert_eq!(manager.state(), SyncState::Synced);

        // Disconnect peer
        let disconnect = NetworkEvent::PeerDisconnected {
            address: peer,
        };
        manager.handle_network_event(&disconnect, &requests).await.unwrap();

        // PeersUpdated with 0 triggers stop_sync
        let update = NetworkEvent::PeersUpdated {
            connected_count: 0,
            addresses: vec![],
            best_height: None,
        };
        manager.handle_network_event(&update, &requests).await.unwrap();
        assert_eq!(manager.state(), SyncState::WaitingForConnections);

        // PeersUpdated with 1 but no peers tracked yet: stays WaitingForConnections
        let update = NetworkEvent::PeersUpdated {
            connected_count: 1,
            addresses: vec![peer],
            best_height: Some(1000),
        };
        manager.handle_network_event(&update, &requests).await.unwrap();
        assert_eq!(manager.state(), SyncState::WaitingForConnections);

        // Peer reconnects and PeersUpdated fires again
        manager.handle_peer_connected(peer);
        let update = NetworkEvent::PeersUpdated {
            connected_count: 1,
            addresses: vec![peer],
            best_height: Some(1000),
        };
        manager.handle_network_event(&update, &requests).await.unwrap();
        assert_eq!(manager.state(), SyncState::Synced);
        assert!(matches!(manager.peers.get(&peer), Some(Some(_))));
    }

    #[tokio::test]
    async fn test_block_processed_confirmed_txids_does_not_eagerly_rebuild() {
        let mut mock = MockWallet::new();
        let script = dashcore::ScriptBuf::from_bytes(vec![
            0x76, 0xa9, 0x14, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab,
            0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0x88, 0xac,
        ]);
        let addr = dashcore::Address::from_script(&script, dashcore::Network::Testnet).unwrap();
        mock.set_addresses(vec![addr]);
        let wallet = Arc::new(RwLock::new(mock));
        let (tx, mut rx) = mpsc::unbounded_channel::<NetworkRequest>();
        let requests = RequestSender::new(tx);

        let mut manager = MempoolManager::new(
            wallet,
            MempoolStrategy::BloomFilter,
            1000,
            0,
            BroadcastConfig::default(),
        );

        let peer = test_socket_address(1);
        manager.handle_peer_connected(peer);

        // Activate
        let sync = SyncEvent::FiltersSyncComplete {
            tip_height: 1000,
        };
        manager.handle_sync_event(&sync, &requests).await.unwrap();

        // Drain activation messages
        while rx.try_recv().is_ok() {}

        // BlockProcessed does not eagerly rebuild — the tick handles it via
        // the revision check. Verify no FilterLoad is sent from the event handler.
        let event = SyncEvent::BlockProcessed {
            block_hash: dashcore::BlockHash::all_zeros(),
            height: 1001,
            wallets: BTreeSet::new(),
            new_scripts: BTreeMap::new(),
            confirmed_txids: vec![dashcore::Txid::all_zeros()],
        };
        manager.handle_sync_event(&event, &requests).await.unwrap();

        let has_filter_load = std::iter::from_fn(|| rx.try_recv().ok()).any(|req| {
            matches!(req, NetworkRequest::SendMessageToPeer(NetworkMessage::FilterLoad(_), _))
        });
        assert!(!has_filter_load, "BlockProcessed should not eagerly rebuild filter");
    }

    #[tokio::test]
    async fn test_block_processed_no_changes_no_rebuild_flag() {
        let (mut manager, requests, _rx) = create_test_manager();
        let peer = test_socket_address(1);
        manager.handle_peer_connected(peer);

        let sync = SyncEvent::FiltersSyncComplete {
            tip_height: 1000,
        };
        manager.handle_sync_event(&sync, &requests).await.unwrap();

        // BlockProcessed with no confirmed txids and no new addresses
        let event = SyncEvent::BlockProcessed {
            block_hash: dashcore::BlockHash::all_zeros(),
            height: 1001,
            wallets: BTreeSet::new(),
            new_scripts: BTreeMap::new(),
            confirmed_txids: vec![],
        };
        manager.handle_sync_event(&event, &requests).await.unwrap();
    }

    #[tokio::test]
    async fn test_tick_rebuilds_filter_when_monitor_revision_changes() {
        let addr = {
            let script = dashcore::ScriptBuf::from_bytes(vec![
                0x76, 0xa9, 0x14, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab,
                0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0x88, 0xac,
            ]);
            dashcore::Address::from_script(&script, dashcore::Network::Testnet).unwrap()
        };

        let mut mock = MockWallet::new();
        mock.set_addresses(vec![addr.clone()]);
        let initial_revision = mock.monitor_revision();
        let wallet = Arc::new(RwLock::new(mock));
        let (tx, mut rx) = mpsc::unbounded_channel::<NetworkRequest>();
        let requests = RequestSender::new(tx);

        let mut manager = MempoolManager::new(
            wallet.clone(),
            MempoolStrategy::BloomFilter,
            1000,
            initial_revision,
            BroadcastConfig::default(),
        );

        let peer = test_socket_address(1);
        manager.handle_peer_connected(peer);

        // Activate — this snapshots the monitor revision
        let sync = SyncEvent::FiltersSyncComplete {
            tip_height: 1000,
        };
        manager.handle_sync_event(&sync, &requests).await.unwrap();
        assert_eq!(manager.state(), SyncState::Synced);

        // Drain activation messages
        while rx.try_recv().is_ok() {}

        // tick with unchanged revision should not rebuild
        manager.tick(&requests).await.unwrap();
        assert!(rx.try_recv().is_err(), "no messages expected when revision unchanged");

        // Simulate wallet adding new addresses (bumps revision)
        {
            let mut w = wallet.write().await;
            let addr2 = dashcore::Address::from_script(
                &dashcore::ScriptBuf::from_bytes(vec![
                    0x76, 0xa9, 0x14, 0xcd, 0xcd, 0xcd, 0xcd, 0xcd, 0xcd, 0xcd, 0xcd, 0xcd, 0xcd,
                    0xcd, 0xcd, 0xcd, 0xcd, 0xcd, 0xcd, 0xcd, 0xcd, 0xcd, 0xcd, 0x88, 0xac,
                ]),
                dashcore::Network::Testnet,
            )
            .unwrap();
            w.set_addresses(vec![addr, addr2]);
        }

        // tick should detect stale filter and rebuild
        manager.tick(&requests).await.unwrap();

        let mut found_filter_load = false;
        while let Ok(msg) = rx.try_recv() {
            if matches!(msg, NetworkRequest::SendMessageToPeer(NetworkMessage::FilterLoad(_), _)) {
                found_filter_load = true;
            }
        }
        assert!(found_filter_load, "expected FilterLoad after monitor revision change");

        // Subsequent tick should not rebuild again (revision was snapshotted)
        manager.tick(&requests).await.unwrap();
        assert!(rx.try_recv().is_err(), "no messages expected after revision re-snapshot");
    }

    #[tokio::test]
    async fn test_tick_skips_rebuild_for_fetch_all_strategy() {
        let wallet = Arc::new(RwLock::new(MockWallet::new()));
        let (tx, mut rx) = mpsc::unbounded_channel::<NetworkRequest>();
        let requests = RequestSender::new(tx);

        let mut manager = MempoolManager::new(
            wallet.clone(),
            MempoolStrategy::FetchAll,
            1000,
            0,
            BroadcastConfig::default(),
        );

        let peer = test_socket_address(1);
        manager.handle_peer_connected(peer);

        let sync = SyncEvent::FiltersSyncComplete {
            tip_height: 1000,
        };
        manager.handle_sync_event(&sync, &requests).await.unwrap();
        while rx.try_recv().is_ok() {}

        // Bump revision
        {
            let mut w = wallet.write().await;
            w.set_addresses(vec![dashcore::Address::dummy(dashcore::Network::Testnet, 0)]);
        }

        // tick should not send any filter messages for FetchAll
        manager.tick(&requests).await.unwrap();
        let mut found_filter = false;
        while let Ok(msg) = rx.try_recv() {
            if matches!(
                msg,
                NetworkRequest::SendMessageToPeer(NetworkMessage::FilterLoad(_), _)
                    | NetworkRequest::SendMessageToPeer(NetworkMessage::FilterClear, _)
            ) {
                found_filter = true;
            }
        }
        assert!(!found_filter, "FetchAll should not send filter messages on revision change");
    }

    #[tokio::test]
    async fn test_tick_rebuilds_filter_when_outpoints_change() {
        let addr = {
            let script = dashcore::ScriptBuf::from_bytes(vec![
                0x76, 0xa9, 0x14, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab,
                0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0x88, 0xac,
            ]);
            dashcore::Address::from_script(&script, dashcore::Network::Testnet).unwrap()
        };

        let mut mock = MockWallet::new();
        mock.set_addresses(vec![addr]);
        let initial_revision = mock.monitor_revision();
        let wallet = Arc::new(RwLock::new(mock));
        let (tx, mut rx) = mpsc::unbounded_channel::<NetworkRequest>();
        let requests = RequestSender::new(tx);

        let mut manager = MempoolManager::new(
            wallet.clone(),
            MempoolStrategy::BloomFilter,
            1000,
            initial_revision,
            BroadcastConfig::default(),
        );

        let peer = test_socket_address(1);
        manager.handle_peer_connected(peer);

        let sync = SyncEvent::FiltersSyncComplete {
            tip_height: 1000,
        };
        manager.handle_sync_event(&sync, &requests).await.unwrap();
        while rx.try_recv().is_ok() {}

        // Simulate UTXO set change (new outpoint added)
        {
            let mut w = wallet.write().await;
            w.set_outpoints(vec![dashcore::OutPoint {
                txid: dashcore::Txid::from_byte_array([0xee; 32]),
                vout: 0,
            }]);
        }

        // tick should detect the revision change and rebuild
        manager.tick(&requests).await.unwrap();

        let found_filter_load = std::iter::from_fn(|| rx.try_recv().ok()).any(|msg| {
            matches!(msg, NetworkRequest::SendMessageToPeer(NetworkMessage::FilterLoad(_), _))
        });
        assert!(found_filter_load, "expected FilterLoad after outpoint change");
    }

    #[tokio::test]
    async fn test_handle_tx_does_not_eagerly_rebuild_filter() {
        let mut mock = MockWallet::new();
        mock.set_mempool_relevant(true);
        let script = dashcore::ScriptBuf::from_bytes(vec![
            0x76, 0xa9, 0x14, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab,
            0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0x88, 0xac,
        ]);
        let addr = dashcore::Address::from_script(&script, dashcore::Network::Testnet).unwrap();
        mock.set_addresses(vec![addr]);
        let initial_revision = mock.monitor_revision();
        let wallet = Arc::new(RwLock::new(mock));
        let (tx_chan, mut rx) = mpsc::unbounded_channel::<NetworkRequest>();
        let requests = RequestSender::new(tx_chan);

        let mut manager = MempoolManager::new(
            wallet.clone(),
            MempoolStrategy::BloomFilter,
            1000,
            initial_revision,
            BroadcastConfig::default(),
        );

        let peer = test_socket_address(1);
        manager.handle_peer_connected(peer);

        let sync = SyncEvent::FiltersSyncComplete {
            tip_height: 1000,
        };
        manager.handle_sync_event(&sync, &requests).await.unwrap();
        while rx.try_recv().is_ok() {}

        // handle_tx with a relevant transaction should NOT eagerly rebuild
        let tx = dashcore::Transaction {
            version: 1,
            lock_time: 0,
            input: vec![],
            output: vec![],
            special_transaction_payload: None,
        };
        manager.handle_tx(tx, test_socket_address(1), &requests).await.unwrap();

        let has_filter_load = std::iter::from_fn(|| rx.try_recv().ok()).any(|msg| {
            matches!(msg, NetworkRequest::SendMessageToPeer(NetworkMessage::FilterLoad(_), _))
        });
        assert!(!has_filter_load, "handle_tx should not eagerly rebuild filter");

        // But the next tick should catch it if the wallet revision changed
        // (MockWallet bumps revision when set_mempool_relevant triggers processing)
        {
            let mut w = wallet.write().await;
            w.set_addresses(vec![dashcore::Address::dummy(dashcore::Network::Testnet, 0)]);
        }
        manager.tick(&requests).await.unwrap();

        let found_filter_load = std::iter::from_fn(|| rx.try_recv().ok()).any(|msg| {
            matches!(msg, NetworkRequest::SendMessageToPeer(NetworkMessage::FilterLoad(_), _))
        });
        assert!(found_filter_load, "tick should rebuild after revision change");
    }
}
