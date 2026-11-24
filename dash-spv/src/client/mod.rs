//! High-level client API for the Dash SPV client.
//!
//! Provides `DashSpvClient`, the main entry point for SPV operations including
//! sync orchestration, mempool tracking, peer/masternode queries, and transaction
//! broadcasting.
//!
//! ## Module Structure
//!
//! - `config.rs` - Client configuration
//! - `core.rs` - Core `DashSpvClient` struct definition and simple accessors
//! - `lifecycle.rs` - Client lifecycle (new, stop)
//! - `events.rs` - Event emission and progress tracking receivers
//! - `queries.rs` - Peer, masternode, and balance queries
//! - `transactions.rs` - Transaction operations (e.g., broadcast)
//! - `sync_coordinator.rs` - Sync orchestration and network monitoring

pub mod anchor;
pub mod config;
pub mod devnet;
pub mod event_handler;
pub mod mn_status;

mod core;
mod events;
mod fail_stop;
mod lifecycle;
mod llmq;
mod queries;
pub mod quorum_lookup;
#[cfg(test)]
mod rig_owner;
mod sync_coordinator;
mod transactions;

pub(crate) use lifecycle::effective_chainlocks_enabled;

// Re-export public types from extracted modules
pub use config::{default_chainlocks_enabled, ClientConfig};
pub use devnet::DevnetConfig;
pub use event_handler::EventHandler;
pub use fail_stop::SpvFailStop;

// Re-export the main client struct
pub use core::DashSpvClient;

#[cfg(test)]
mod config_test;

#[cfg(test)]
mod tests {
    use super::{ClientConfig, DashSpvClient};
    use crate::client::config::MempoolStrategy;
    use crate::test_utils::MockNetworkManager;
    use crate::Network;
    use dash_chain::FailStop;
    use key_wallet::wallet::initialization::WalletAccountCreationOptions;
    use key_wallet::wallet::managed_wallet_info::ManagedWalletInfo;
    use key_wallet_manager::WalletManager;
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::sync::RwLock;
    use tokio::time::{sleep, timeout};

    const TEST_MNEMONIC: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    /// Construct a mainnet client with the given wallet birth height and optional
    /// explicit `start_from_height`, then return the height and hash its chain is
    /// anchored at.
    async fn anchored_tip(
        birth_height: u32,
        start_from_height: Option<u32>,
    ) -> (u32, Option<dashcore::BlockHash>) {
        let mut wallet_manager = WalletManager::<ManagedWalletInfo>::new(Network::Mainnet);
        wallet_manager
            .create_wallet_from_mnemonic(
                TEST_MNEMONIC,
                birth_height,
                WalletAccountCreationOptions::Default,
            )
            .expect("wallet creation must succeed");
        let wallet = Arc::new(RwLock::new(wallet_manager));

        let temp_dir = TempDir::new().unwrap();
        let mut config = ClientConfig::mainnet()
            .without_filters()
            .without_masternodes()
            .with_storage_path(temp_dir.path());
        config.start_from_height = start_from_height;

        let client =
            DashSpvClient::new(config, MockNetworkManager::new(), wallet, vec![Arc::new(())])
                .await
                .expect("client construction must succeed");
        let tip = client.tip_header().await;
        (tip.as_ref().map_or(0, |tip| tip.height()), tip.map(|tip| *tip.hash()))
    }

    #[tokio::test]
    async fn birth_height_anchors_chain_to_nearest_checkpoint() {
        // A wallet birth height below mainnet's HD/BIP39 activation floor is clamped up to
        // the floor, which anchors at the nearest checkpoint at or before it: the 200_000
        // checkpoint. The stored tip carries the trusted checkpoint hash (what the next
        // header's `prev_blockhash` is validated against), not a hash recomputed from a header.
        // 120_000 is below mainnet's HD/BIP39 sync floor, so it is clamped up.
        let (height, hash) = anchored_tip(120_000, None).await;
        assert_eq!(height, 200_000);
        let expected: dashcore::BlockHash =
            "000000000004d0615ff622ec78457ca211dc63fc9c62cca9d9d9af7206be721b".parse().unwrap();
        assert_eq!(hash, Some(expected));

        // Birth height 0 no longer drags mainnet sync to genesis; it floors at HD activation.
        assert_eq!(anchored_tip(0, None).await.0, 200_000);

        // A birth height above the floor is unaffected and anchors at the nearest
        // checkpoint at or below it.
        assert_eq!(anchored_tip(560_000, None).await.0, 550_000);

        // Explicit `start_from_height` wins over the wallet birth height and is honored
        // even below the floor.
        assert_eq!(anchored_tip(120_000, Some(60_000)).await.0, 50_000);
    }

    /// A failed sync loop is stopped by a task of its own, and `stop` waits for
    /// it, so it no longer holds the storage once `stop` returns.
    #[tokio::test]
    async fn stop_waits_for_the_stop_of_a_failed_loop() {
        let temp_dir = TempDir::new().unwrap();
        let config = ClientConfig::testnet()
            .without_filters()
            .without_masternodes()
            .with_storage_path(temp_dir.path());
        let wallet = Arc::new(RwLock::new(WalletManager::<ManagedWalletInfo>::new(config.network)));
        let client =
            DashSpvClient::new(config, MockNetworkManager::new(), wallet, vec![Arc::new(())])
                .await
                .expect("client construction must succeed");

        client.run().await.expect("client must start");
        client.network.close_network_events().await;

        timeout(Duration::from_secs(5), async {
            while client.detached_tasks.lock().await.is_empty() {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the failed loop must be stopped by a detached task");

        client.stop().await;
        assert_eq!(Arc::strong_count(&client.storage), 1);
    }

    /// BL-911: a blocking store job a task left running when it was aborted, as the sync
    /// coordinator's shutdown aborts its managers, ends before `stop` returns, and its write is
    /// kept.
    #[tokio::test]
    async fn stop_waits_for_a_store_job_an_aborted_task_left_running() {
        let temp_dir = TempDir::new().unwrap();
        let config = ClientConfig::testnet()
            .without_filters()
            .without_masternodes()
            .with_storage_path(temp_dir.path());
        let wallet = Arc::new(RwLock::new(WalletManager::<ManagedWalletInfo>::new(config.network)));
        let client = DashSpvClient::new(
            config.clone(),
            MockNetworkManager::new(),
            wallet,
            vec![Arc::new(())],
        )
        .await
        .expect("client construction must succeed");
        client.run().await.expect("client must start");

        let store = client.storage.lock().await.store().clone();
        let (started, running) = tokio::sync::oneshot::channel();
        let (release, held) = std::sync::mpsc::channel::<()>();
        let job = tokio::spawn(async move {
            store
                .run(move |s| {
                    let _ = started.send(());
                    let _ = held.recv();
                    s.put_metadata("bl911", b"written")
                })
                .await
        });
        running.await.unwrap();
        job.abort();

        {
            let stop = client.try_stop();
            tokio::pin!(stop);
            assert!(
                timeout(Duration::from_secs(2), &mut stop).await.is_err(),
                "stop returned while a store job was still running"
            );
            release.send(()).unwrap();
            stop.await.unwrap();
        }
        drop(client);

        let reopened = crate::storage::DiskStorageManager::new(&config).await.unwrap();
        let stored = reopened.store().run(|s| s.metadata("bl911")).await.unwrap();
        assert_eq!(stored, Some(b"written".to_vec()));
    }

    #[tokio::test]
    async fn client_exposes_shared_wallet_manager() {
        let temp_dir = TempDir::new().unwrap();
        let config = ClientConfig::mainnet()
            .without_filters()
            .without_masternodes()
            .with_mempool_tracking(MempoolStrategy::FetchAll)
            .with_storage_path(temp_dir.path());

        let network_manager = MockNetworkManager::new();
        let wallet = Arc::new(RwLock::new(WalletManager::<ManagedWalletInfo>::new(config.network)));

        let client = DashSpvClient::new(config, network_manager, wallet, vec![Arc::new(())])
            .await
            .expect("client construction must succeed");

        // Verify the wallet is accessible
        let wallet_ref = client.wallet();
        let _wallet_guard = wallet_ref.read().await;
    }

    #[tokio::test]
    async fn each_client_has_its_own_shared_fail_stop_latch() {
        let make_client = |path: &std::path::Path| {
            let path = path.to_path_buf();
            async move {
                let config = ClientConfig::mainnet()
                    .without_filters()
                    .without_masternodes()
                    .with_storage_path(path);
                let wallet =
                    Arc::new(RwLock::new(WalletManager::<ManagedWalletInfo>::new(config.network)));
                DashSpvClient::new(config, MockNetworkManager::new(), wallet, vec![Arc::new(())])
                    .await
                    .expect("client construction must succeed")
            }
        };

        let first_dir = TempDir::new().unwrap();
        let second_dir = TempDir::new().unwrap();
        let first = make_client(first_dir.path()).await;
        let second = make_client(second_dir.path()).await;
        assert!(!Arc::ptr_eq(first.fail_stop(), second.fail_stop()));
        assert!(!first.fail_stop().is_tripped());
        assert!(!second.fail_stop().is_tripped());

        let clone = first.clone();
        assert!(Arc::ptr_eq(first.fail_stop(), clone.fail_stop()));
        first.fail_stop().trip("first client failed");
        assert!(first.fail_stop().is_tripped());
        assert!(clone.fail_stop().is_tripped());
        assert!(!second.fail_stop().is_tripped());
    }

    /// A lagged ChainLock dispatch reads the switch once per run, so it is fixed for the
    /// client's life: only a change that keeps ChainLocks on or off is accepted.
    #[tokio::test]
    async fn update_config_rejects_a_chainlocks_switch_change() {
        async fn update(
            config: ClientConfig,
            chainlocks_enabled: Option<bool>,
        ) -> crate::error::Result<()> {
            let temp_dir = TempDir::new().unwrap();
            let config =
                config.without_filters().without_masternodes().with_storage_path(temp_dir.path());
            let wallet =
                Arc::new(RwLock::new(WalletManager::<ManagedWalletInfo>::new(config.network)));
            let client =
                DashSpvClient::new(config.clone(), MockNetworkManager::new(), wallet, vec![])
                    .await
                    .expect("client construction must succeed");
            client.update_config(config.with_chainlocks_enabled(chainlocks_enabled)).await
        }

        let error = update(ClientConfig::regtest(), Some(true)).await.unwrap_err();
        assert!(error.to_string().contains("chainlocks_enabled"));
        update(ClientConfig::regtest(), Some(false)).await.unwrap();
        update(ClientConfig::mainnet(), Some(false)).await.unwrap();
        let error = update(ClientConfig::testnet(), Some(false)).await.unwrap_err();
        assert!(error.to_string().contains("chainlocks_enabled"));
    }

    #[tokio::test]
    async fn update_config_rejects_chain_setup_changes() {
        let temp_dir = TempDir::new().unwrap();
        let config = ClientConfig::new(Network::Devnet)
            .with_devnet(super::DevnetConfig::new("f6"))
            .without_filters()
            .without_masternodes()
            .with_storage_path(temp_dir.path());
        let wallet = Arc::new(RwLock::new(WalletManager::<ManagedWalletInfo>::new(config.network)));
        let client = DashSpvClient::new(config.clone(), MockNetworkManager::new(), wallet, vec![])
            .await
            .expect("client construction must succeed");

        let mut changed_chain_args = config.clone();
        changed_chain_args.chain_args = vec![(dash_params::ChainOption::LlmqChainLocks, None)];
        let error = client.update_config(changed_chain_args).await.unwrap_err();
        assert!(error.to_string().contains("chain_args"));

        let mut changed_devnet = config;
        changed_devnet.devnet = Some(super::DevnetConfig::new("other"));
        let error = client.update_config(changed_devnet).await.unwrap_err();
        assert!(error.to_string().contains("devnet"));
    }
}
