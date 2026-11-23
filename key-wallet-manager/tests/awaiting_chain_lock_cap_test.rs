//! A block transaction that would overturn an InstantSend-locked record once
//! a ChainLock covers its block is kept only so many at a time: should
//! ChainLocks stall, the one in the lowest block goes first, and the record
//! it would have overturned stays, as before such transactions were kept.

mod common;

use common::{funding_tx, process_block_for, spend_tx};
use dashcore::blockdata::block::Block;
use dashcore::blockdata::transaction::OutPoint;
use dashcore::ephemerealdata::chain_lock::ChainLock;
use dashcore::ephemerealdata::instant_lock::InstantLock;
use dashcore::Network;
use key_wallet::wallet::initialization::WalletAccountCreationOptions;
use key_wallet::wallet::managed_wallet_info::wallet_info_interface::WalletInfoInterface;
use key_wallet::wallet::managed_wallet_info::ManagedWalletInfo;
use key_wallet_manager::{WalletId, WalletInterface, WalletManager, MAX_AWAITING_CHAIN_LOCK};
use std::collections::BTreeSet;

const FUNDED_AT: u32 = 100;

fn records(
    manager: &WalletManager<ManagedWalletInfo>,
    wallet: &WalletId,
    txid: &dashcore::Txid,
) -> bool {
    manager
        .get_wallet_info(wallet)
        .unwrap()
        .accounts()
        .all_accounts()
        .into_iter()
        .any(|account| account.has_transaction(txid))
}

#[tokio::test]
async fn conflicts_awaiting_a_chain_lock_are_capped_lowest_block_first() {
    let mut manager = WalletManager::<ManagedWalletInfo>::new(Network::Testnet);
    let wallet =
        manager.create_wallet_with_random_mnemonic(WalletAccountCreationOptions::Default).unwrap();
    let address = manager.monitored_addresses().first().cloned().unwrap();
    let only: BTreeSet<WalletId> = [wallet].into_iter().collect();
    let funding = funding_tx(&address, 1_000_000, 0xC0);
    let coin = OutPoint::new(funding.txid(), 0);
    process_block_for(&mut manager, &Block::dummy(FUNDED_AT, vec![funding]), FUNDED_AT, &only)
        .await;

    let locked = spend_tx(coin, 990_000, 1);
    let lock = InstantLock {
        txid: locked.txid(),
        inputs: vec![coin],
        ..InstantLock::default()
    };
    manager.process_mempool_transaction(&locked, Some(lock)).await;
    assert!(records(&manager, &wallet, &locked.txid()), "L is the wallet's");

    let first = FUNDED_AT + 1;
    for n in 0..=MAX_AWAITING_CHAIN_LOCK as u32 {
        let conflict = spend_tx(coin, 980_000 - u64::from(n), 2);
        let height = first + n;
        process_block_for(&mut manager, &Block::dummy(height, vec![conflict]), height, &only).await;
    }
    assert!(records(&manager, &wallet, &locked.txid()), "a plain block overturns no lock");

    manager.apply_chain_lock(ChainLock::dummy(first));
    assert!(
        records(&manager, &wallet, &locked.txid()),
        "the conflict in the lowest block went, so L stays"
    );

    manager.apply_chain_lock(ChainLock::dummy(first + MAX_AWAITING_CHAIN_LOCK as u32));
    assert!(!records(&manager, &wallet, &locked.txid()), "the rest are kept");
}
