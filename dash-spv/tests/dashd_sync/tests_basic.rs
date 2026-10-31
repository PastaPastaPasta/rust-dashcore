use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use dash_spv::sync::{ProgressPercentage, SyncEvent};
use dash_spv::Network;
use dashcore::OutPoint;
use key_wallet::gap_limit::DEFAULT_EXTERNAL_GAP_LIMIT;
use key_wallet::wallet::managed_wallet_info::wallet_info_interface::WalletInfoInterface;
use key_wallet::wallet::managed_wallet_info::ManagedWalletInfo;
use key_wallet_manager::{WalletEvent, WalletManager};
use tokio::sync::broadcast::error::TryRecvError;
use tokio::sync::RwLock;

use super::helpers::{
    coins_owned_by_records, count_wallet_transactions, get_spendable_balance, owned_outputs,
    wait_for_sync, EMPTY_MNEMONIC,
};
use super::setup::{create_and_start_client, TestContext};
use dash_spv::test_utils::{create_test_wallet, TestChain};

/// The generator's mining wallet ("default") received its faucet split as one
/// transaction paying 50 consecutive fresh addresses, more than the receive
/// gap window holds. Its balance must match dashd's, and the records its
/// events first emit must already own all its coins: those records are what a
/// persisting consumer stores, so a coin the wallet holds but no stored record
/// owns is gone after the consumer's next load. A record corrected later (an
/// `updated` one) would reach only the consumers that store those too.
///
/// This pins the first-sighting fix only: the split's outputs are all found
/// in one pass over its own block, and no `updated` record is collected.
/// `test_rescan_re_emits_a_record_grown_past_its_first_window` covers the
/// correction a filter rescan emits.
#[tokio::test]
async fn test_sync_wallet_paid_past_the_gap_window_in_one_transaction() {
    let Some(ctx) = TestContext::new(TestChain::Minimal).await else {
        return;
    };
    let default = ctx.dashd.wallet_file("default");
    let (wallet, wallet_id) = create_test_wallet(&default.mnemonic, Network::Regtest);
    let mut events = wallet.write().await.take_persistence_receiver().expect("first consumer");

    let mut client_handle = create_and_start_client(&ctx.client_config, Arc::clone(&wallet)).await;
    wait_for_sync(&mut client_handle.progress_receiver, ctx.dashd.initial_height).await;
    client_handle.stop().await;

    let wallet_read = wallet.read().await;
    let info = wallet_read.get_wallet_info(&wallet_id).expect("Wallet info not found");
    assert_eq!(info.balance().spendable(), default.utxo_balance(), "balance differs from dashd's");

    let mut records = Vec::new();
    while let Ok(event) = events.try_recv() {
        match event {
            WalletEvent::TransactionDetected {
                record,
                ..
            } => records.push(*record),
            WalletEvent::BlockProcessed {
                inserted,
                ..
            } => records.extend(inserted),
            _ => {}
        }
    }
    let widest = records.iter().map(|r| owned_outputs(r).count()).max().unwrap_or(0);
    assert!(
        widest > DEFAULT_EXTERNAL_GAP_LIMIT as usize,
        "the fixture's split owns more outputs than a window: {widest}"
    );
    let held: BTreeSet<OutPoint> = info.utxos().into_iter().map(|u| u.outpoint).collect();
    let from_events = coins_owned_by_records(&records);
    let unrecorded: Vec<_> = held.difference(&from_events).collect();
    assert!(unrecorded.is_empty(), "coins no emitted record owns: {unrecorded:?}");
    assert_eq!(from_events, held);
}

#[tokio::test]
async fn test_wallet_sync() {
    let Some(ctx) = TestContext::new(TestChain::Full).await else {
        return;
    };

    let mut client_handle = ctx.spawn_new_client().await;
    wait_for_sync(&mut client_handle.progress_receiver, ctx.dashd.initial_height).await;

    client_handle.stop().await;
    ctx.assert_synced(&client_handle.client.progress().await).await;
}

/// Verify that syncing with a wallet that has no on-chain activity results in zero
/// transactions and zero balance, while headers and filters sync fully.
#[tokio::test]
async fn test_sync_empty_wallet() {
    let Some(ctx) = TestContext::new(TestChain::Full).await else {
        return;
    };

    let (empty_wallet, empty_wallet_id) = create_test_wallet(EMPTY_MNEMONIC, Network::Regtest);

    tracing::info!("Starting sync with empty wallet");
    let mut client_handle =
        create_and_start_client(&ctx.client_config, Arc::clone(&empty_wallet)).await;
    wait_for_sync(&mut client_handle.progress_receiver, ctx.dashd.initial_height).await;

    client_handle.stop().await;

    // Verify headers and filter headers synced fully
    let final_progress = client_handle.client.progress().await;
    let header_height = final_progress.headers().unwrap().current_height();
    let filter_header_height = final_progress.filter_headers().unwrap().current_height();

    assert_eq!(header_height, ctx.dashd.initial_height, "Header height mismatch");
    assert_eq!(filter_header_height, ctx.dashd.initial_height, "Filter header height mismatch");

    // Verify zero transactions and zero balance
    let tx_count = count_wallet_transactions(&empty_wallet, &empty_wallet_id).await;
    let balance = get_spendable_balance(&empty_wallet, &empty_wallet_id).await;

    assert_eq!(tx_count, 0, "Empty wallet should have 0 transactions, got {}", tx_count);
    assert_eq!(balance, 0, "Empty wallet should have 0 balance, got {}", balance);

    tracing::info!(
        "Empty wallet sync complete: headers={}, filters={}, txs={}, balance={}",
        header_height,
        filter_header_height,
        tx_count,
        balance
    );
}

/// A `WalletManager` with no wallets reaches `Synced` and stays there:
/// the post-sync window must be free of `FiltersSyncComplete` events, since
/// the rescan trigger has nothing to act on.
#[tokio::test]
async fn test_empty_wallet_manager_no_spurious_rescan() {
    let Some(ctx) = TestContext::new(TestChain::Minimal).await else {
        return;
    };

    let wallet = Arc::new(RwLock::new(WalletManager::<ManagedWalletInfo>::new(Network::Regtest)));
    let mut client_handle = create_and_start_client(&ctx.client_config, Arc::clone(&wallet)).await;
    wait_for_sync(&mut client_handle.progress_receiver, ctx.dashd.initial_height).await;

    loop {
        match client_handle.sync_event_receiver.try_recv() {
            Ok(_) => {}
            Err(TryRecvError::Empty) => break,
            Err(err) => panic!("sync event channel failure while draining: {err}"),
        }
    }

    let watch_window = Duration::from_secs(2);
    let deadline = tokio::time::Instant::now() + watch_window;
    let mut spurious = 0;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, client_handle.sync_event_receiver.recv()).await {
            Ok(Ok(SyncEvent::FiltersSyncComplete {
                ..
            })) => spurious += 1,
            Ok(Ok(_)) => {}
            Ok(Err(err)) => panic!("sync event channel failure during empty-manager watch: {err}"),
            Err(_) => break,
        }
    }
    assert_eq!(
        spurious, 0,
        "empty wallet manager emitted {} FiltersSyncComplete events in {:?}",
        spurious, watch_window
    );
    assert!(
        client_handle.client.progress().await.is_synced(),
        "client should remain Synced after the empty-manager watch window"
    );

    client_handle.stop().await;
}
