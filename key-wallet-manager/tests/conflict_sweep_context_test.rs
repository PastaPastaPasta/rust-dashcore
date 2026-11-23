//! Which context a block conflict sweeps a wallet's unconfirmed records
//! with, and what is kept for a later ChainLock: an InstantSend-locked
//! record goes only once a ChainLock covers its conflict's block, whatever
//! the other fetched wallets' ChainLocks and after a fork unmines it.

mod common;

use common::{funding_tx, process_block_for, spend_tx};
use dashcore::blockdata::block::Block;
use dashcore::blockdata::transaction::OutPoint;
use dashcore::ephemerealdata::chain_lock::ChainLock;
use dashcore::ephemerealdata::instant_lock::InstantLock;
use dashcore::{Network, Transaction};
use key_wallet::wallet::initialization::WalletAccountCreationOptions;
use key_wallet::wallet::managed_wallet_info::wallet_info_interface::WalletInfoInterface;
use key_wallet::wallet::managed_wallet_info::ManagedWalletInfo;
use key_wallet_manager::{WalletId, WalletInterface, WalletManager};
use std::collections::BTreeSet;

const FUNDED_AT: u32 = 100;

fn wallet(manager: &mut WalletManager<ManagedWalletInfo>) -> (WalletId, dashcore::Address) {
    let id =
        manager.create_wallet_with_random_mnemonic(WalletAccountCreationOptions::Default).unwrap();
    let address = manager.get_wallet_info(&id).unwrap().monitored_addresses()[0].clone();
    (id, address)
}

fn set(ids: &[WalletId]) -> BTreeSet<WalletId> {
    ids.iter().copied().collect()
}

fn records(manager: &WalletManager<ManagedWalletInfo>, id: &WalletId, tx: &Transaction) -> bool {
    let txid = tx.txid();
    manager
        .get_wallet_info(id)
        .unwrap()
        .accounts()
        .all_accounts()
        .into_iter()
        .any(|account| account.has_transaction(&txid))
}

fn lock_for(tx: &Transaction) -> InstantLock {
    InstantLock {
        txid: tx.txid(),
        inputs: tx.input.iter().map(|input| input.previous_output).collect(),
        ..InstantLock::default()
    }
}

/// A locked L of wallet A, a coin it spends, and A funded at [`FUNDED_AT`].
async fn locked_spend(
    manager: &mut WalletManager<ManagedWalletInfo>,
) -> (WalletId, OutPoint, Transaction) {
    let (a, address) = wallet(manager);
    let funding = funding_tx(&address, 1_000_000, 0xC0);
    let coin = OutPoint::new(funding.txid(), 0);
    process_block_for(manager, &Block::dummy(FUNDED_AT, vec![funding]), FUNDED_AT, &set(&[a]))
        .await;
    let locked = spend_tx(coin, 990_000, 1);
    manager.process_mempool_transaction(&locked, Some(lock_for(&locked))).await;
    assert!(records(manager, &a, &locked), "L is A's");
    (a, coin, locked)
}

/// A's ChainLock covers D's block but N, fetched with it, lags: the block
/// sweeps both with a plain block context, so A's locked L stays, and the
/// next ChainLock overturns it.
#[tokio::test]
async fn a_locked_spend_goes_at_the_next_chain_lock_when_another_fetched_wallet_lags() {
    let mut manager = WalletManager::<ManagedWalletInfo>::new(Network::Testnet);
    let (a, coin, locked) = locked_spend(&mut manager).await;
    let height = FUNDED_AT + 2;
    manager.apply_chain_lock(ChainLock::dummy(height));
    let (n, _) = wallet(&mut manager);
    assert!(manager.get_wallet_info(&n).unwrap().last_applied_chain_lock().is_none());

    let conflict = spend_tx(coin, 980_000, 2);
    process_block_for(&mut manager, &Block::dummy(height, vec![conflict]), height, &set(&[a, n]))
        .await;
    assert!(records(&manager, &a, &locked), "swept as a plain block, which overturns no lock");

    manager.apply_chain_lock(ChainLock::dummy(height + 1));
    assert!(!records(&manager, &a, &locked), "the ChainLock covers D's block");
}

/// L, locked, is mined and a fork unmines it; D spends L's coin in a plain
/// block of the new branch. L stays until a ChainLock covers D's block.
#[tokio::test]
async fn a_locked_spend_a_fork_unmines_stays_against_a_plain_block_until_a_chain_lock() {
    let mut manager = WalletManager::<ManagedWalletInfo>::new(Network::Testnet);
    let (a, coin, locked) = locked_spend(&mut manager).await;
    let height = FUNDED_AT + 5;
    let only = set(&[a]);
    process_block_for(&mut manager, &Block::dummy(height, vec![locked.clone()]), height, &only)
        .await;
    manager.truncate_above(height - 1).unwrap();
    assert!(records(&manager, &a, &locked), "the fork keeps L, unconfirmed");

    let conflict = spend_tx(coin, 980_000, 2);
    process_block_for(&mut manager, &Block::dummy(height, vec![conflict]), height, &only).await;
    assert!(records(&manager, &a, &locked), "a plain block overturns no lock");

    manager.apply_chain_lock(ChainLock::dummy(height));
    assert!(!records(&manager, &a, &locked), "the ChainLock covers D's block");
}
