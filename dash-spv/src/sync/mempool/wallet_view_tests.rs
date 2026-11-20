//! The mempool manager against real wallets: a transaction it holds or found
//! irrelevant is looked at again when the wallets' view changes, and a
//! verified lock reaches the wallet in either delivery order.

use std::net::SocketAddr;
use std::sync::Arc;

use dashcore::ephemerealdata::instant_lock::InstantLock;
use dashcore::hashes::Hash;
use dashcore::network::message::NetworkMessage;
use dashcore::network::message_blockdata::Inventory;
use dashcore::{Address, BlockHash, Network, OutPoint, Transaction, TxIn, TxOut, Txid};
use key_wallet::managed_account::managed_account_trait::ManagedAccountTrait;
use key_wallet::transaction_checking::{BlockInfo, TransactionContext};
use key_wallet::wallet::initialization::WalletAccountCreationOptions;
use key_wallet::wallet::managed_wallet_info::transaction_builder::TransactionSigner;
use key_wallet::wallet::managed_wallet_info::wallet_info_interface::WalletInfoInterface;
use key_wallet::wallet::managed_wallet_info::ManagedWalletInfo;
use key_wallet::Utxo;
use key_wallet_manager::{WalletEvent, WalletId, WalletManager};
use tokio::sync::{broadcast, mpsc, RwLock};

use super::{BroadcastConfig, MempoolManager};
use crate::client::config::MempoolStrategy;
use crate::network::{NetworkRequest, RequestSender};
use crate::sync::{SyncEvent, SyncManager, SyncState};
use crate::test_utils::{next_unused_receive_address, test_socket_address};

const MNEMONIC_A: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
const MNEMONIC_B: &str =
    "legal winner thank year wave sausage worth useful legal winner thank yellow";
const MNEMONIC_C: &str =
    "letter advice cage absurd amount doctor acoustic avoid letter advice cage above";

const COIN: u64 = 1_000_000;
const PAYMENT: u64 = 990_000;

type Wallets = Arc<RwLock<WalletManager<ManagedWalletInfo>>>;

struct Harness {
    wallets: Wallets,
    manager: MempoolManager<WalletManager<ManagedWalletInfo>>,
    requests: RequestSender,
    sent: mpsc::UnboundedReceiver<NetworkRequest>,
    peer: SocketAddr,
}

impl Harness {
    /// Wallets made from `mnemonics`, and a mempool manager relaying from one
    /// peer while filter sync still runs.
    async fn new(mnemonics: &[&str]) -> (Self, Vec<WalletId>) {
        let wallets: Wallets = Arc::new(RwLock::new(WalletManager::new(Network::Testnet)));
        let mut ids = Vec::new();
        for mnemonic in mnemonics {
            ids.push(register(&wallets, mnemonic).await);
        }
        let (tx, sent) = mpsc::unbounded_channel();
        let requests = RequestSender::new(tx);
        let mut manager = MempoolManager::new(
            wallets.clone(),
            MempoolStrategy::FetchAll,
            1000,
            0,
            BroadcastConfig::default(),
        )
        .with_activation_during_sync(true);
        let peer = test_socket_address(1);
        manager.handle_peer_connected(peer);
        manager.activate_peer(peer, &requests).await.unwrap();
        manager.set_state(SyncState::Synced);
        let mut harness = Self {
            wallets,
            manager,
            requests,
            sent,
            peer,
        };
        harness.sent_messages();
        (harness, ids)
    }

    fn sent_messages(&mut self) -> Vec<NetworkMessage> {
        let mut messages = Vec::new();
        while let Ok(request) = self.sent.try_recv() {
            if let NetworkRequest::SendMessageToPeer(message, _) = request {
                messages.push(message);
            }
        }
        messages
    }

    /// Announce `tx` and deliver it if the manager asks for it. Returns
    /// whether it asked.
    async fn announce(&mut self, tx: &Transaction) -> bool {
        self.manager
            .handle_inv(&[Inventory::Transaction(tx.txid())], self.peer, &self.requests)
            .await
            .unwrap();
        let asked = self.sent_messages().iter().any(|message| {
            matches!(message, NetworkMessage::GetData(items)
                if items.contains(&Inventory::Transaction(tx.txid())))
        });
        if asked {
            self.manager.handle_tx(tx.clone(), self.peer, &self.requests).await.unwrap();
        }
        asked
    }

    async fn filters_synced(&mut self) {
        let event = SyncEvent::FiltersSyncComplete {
            tip_height: 1000,
        };
        self.manager.handle_sync_event(&event, &self.requests).await.unwrap();
    }

    async fn total(&self, wallet_id: &WalletId) -> u64 {
        self.wallets.read().await.get_wallet_info(wallet_id).unwrap().balance().total()
    }

    async fn context_of(&self, wallet_id: &WalletId, txid: &Txid) -> Option<TransactionContext> {
        let wallets = self.wallets.read().await;
        let info = wallets.get_wallet_info(wallet_id).unwrap();
        let account = info.first_bip44_managed_account().unwrap();
        account.transactions().get(txid).map(|record| record.context.clone())
    }

    async fn events(&self) -> broadcast::Receiver<WalletEvent> {
        self.wallets.read().await.subscribe_events()
    }
}

async fn register(wallets: &Wallets, mnemonic: &str) -> WalletId {
    let options = WalletAccountCreationOptions::Default;
    wallets.write().await.create_wallet_from_mnemonic(mnemonic, 0, options).unwrap()
}

/// The first receive address of the wallet `mnemonic` makes, without
/// registering it with `wallets`.
async fn address_of_unregistered(mnemonic: &str) -> Address {
    let elsewhere: Wallets = Arc::new(RwLock::new(WalletManager::new(Network::Testnet)));
    let id = register(&elsewhere, mnemonic).await;
    next_unused_receive_address(&elsewhere, &id).await
}

fn lock_for(tx: &Transaction) -> InstantLock {
    InstantLock {
        txid: tx.txid(),
        inputs: tx.input.iter().map(|input| input.previous_output).collect(),
        ..InstantLock::default()
    }
}

fn drain(rx: &mut broadcast::Receiver<WalletEvent>) -> Vec<WalletEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

fn locks_announced(events: &[WalletEvent]) -> Vec<(WalletId, InstantLock)> {
    events
        .iter()
        .filter_map(|event| match event {
            WalletEvent::TransactionInstantLocked {
                wallet_id,
                instant_lock,
                ..
            } => Some((*wallet_id, instant_lock.clone())),
            _ => None,
        })
        .collect()
}

fn detections_for(events: &[WalletEvent], wallet: &WalletId) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(event, WalletEvent::TransactionDetected { wallet_id, .. } if wallet_id == wallet)
        })
        .count()
}

/// A spend of `funding`'s first output, which pays `owner`'s `from`, paying
/// `to`, signed with `owner`'s key.
async fn signed_spend(
    wallets: &Wallets,
    owner: &WalletId,
    from: &Address,
    funding: &Transaction,
    to: &Address,
) -> Transaction {
    let outpoint = OutPoint::new(funding.txid(), 0);
    let spend = Transaction {
        version: 2,
        lock_time: 0,
        input: vec![TxIn {
            previous_output: outpoint,
            ..Default::default()
        }],
        output: vec![TxOut {
            value: PAYMENT,
            script_pubkey: to.script_pubkey(),
        }],
        special_transaction_payload: None,
    };
    let wallets = wallets.read().await;
    let account = wallets.get_wallet_info(owner).unwrap().first_bip44_managed_account().unwrap();
    let path = account.address_derivation_path(from).unwrap();
    let coin = Utxo::new(outpoint, funding.output[0].clone(), from.clone(), 0, false);
    let wallet = wallets.get_wallet(owner).unwrap();
    wallet.sign_tx(spend, vec![coin], move |_| Some(path.clone())).await.unwrap()
}

fn in_block(height: u32) -> TransactionContext {
    TransactionContext::InBlock(BlockInfo::new(
        height,
        BlockHash::from_byte_array([height as u8; 32]),
        1_650_000_000 + height,
    ))
}

#[tokio::test]
async fn a_held_spend_reaches_a_wallet_whose_scan_finds_the_coin_later() {
    let (mut h, ids) = Harness::new(&[MNEMONIC_A, MNEMONIC_B]).await;
    let (a, b) = (ids[0], ids[1]);
    let to_a = next_unused_receive_address(&h.wallets, &a).await;
    let to_b = next_unused_receive_address(&h.wallets, &b).await;

    // B's coin is in a block B's scan has not reached; a spend of it, signed
    // by B on another device, pays A and is InstantSend-locked.
    let funding = Transaction::dummy(&to_b, 0..1, &[COIN]);
    let spend = signed_spend(&h.wallets, &b, &to_b, &funding, &to_a).await;
    let lock = lock_for(&spend);
    assert!(h.announce(&spend).await);
    h.manager.process_instant_send(lock.clone()).await;
    assert_eq!(h.total(&a).await, PAYMENT);
    assert!(!h.announce(&spend).await, "a held transaction is not fetched again");

    // B's scan finds the coin: B now shows it as available.
    h.wallets
        .write()
        .await
        .check_transaction_in_all_wallets(&funding, in_block(900), true, true)
        .await;
    assert_eq!(h.total(&b).await, COIN);

    let mut events = h.events().await;
    h.filters_synced().await;
    let events = drain(&mut events);

    assert_eq!(h.total(&b).await, 0, "B's coin is spent by the held transaction");
    assert!(
        matches!(h.context_of(&b, &spend.txid()).await, Some(TransactionContext::InstantSend(_))),
        "the held lock comes with the transaction"
    );
    assert_eq!(locks_announced(&events), vec![(b, lock)]);
    assert_eq!(h.total(&a).await, PAYMENT, "A's output is not credited twice");
    assert_eq!(detections_for(&events, &a), 0, "A is told nothing new: {:?}", events);
}

#[tokio::test]
async fn a_wallet_registered_after_sync_gets_payments_already_in_the_mempool() {
    let (mut h, _) = Harness::new(&[MNEMONIC_A]).await;
    h.filters_synced().await;
    h.manager.tick(&h.requests).await.unwrap();
    h.sent_messages();

    let to_c = address_of_unregistered(MNEMONIC_C).await;
    let payment = Transaction::dummy(&to_c, 7..8, &[COIN]);
    assert!(h.announce(&payment).await);
    assert!(!h.manager.transactions.contains_key(&payment.txid()), "no wallet matches it yet");

    let c = register(&h.wallets, MNEMONIC_C).await;
    h.manager.tick(&h.requests).await.unwrap();
    assert!(
        h.sent_messages().iter().any(|message| matches!(message, NetworkMessage::MemPool)),
        "the peer is asked for its mempool again"
    );
    assert!(h.announce(&payment).await, "the earlier non-match is fetched again");
    assert_eq!(h.total(&c).await, COIN);
}

#[tokio::test]
async fn a_lock_that_came_before_an_unmatched_transaction_survives_its_retry() {
    let (mut h, _) = Harness::new(&[MNEMONIC_A]).await;
    h.filters_synced().await;
    h.manager.tick(&h.requests).await.unwrap();

    let to_c = address_of_unregistered(MNEMONIC_C).await;
    let payment = Transaction::dummy(&to_c, 9..10, &[COIN]);
    let lock = lock_for(&payment);
    h.manager.process_instant_send(lock.clone()).await;
    assert!(h.announce(&payment).await);
    assert!(!h.manager.transactions.contains_key(&payment.txid()));

    let c = register(&h.wallets, MNEMONIC_C).await;
    let mut events = h.events().await;
    h.manager.tick(&h.requests).await.unwrap();
    assert!(h.announce(&payment).await);

    assert!(matches!(
        h.context_of(&c, &payment.txid()).await,
        Some(TransactionContext::InstantSend(_))
    ));
    assert_eq!(locks_announced(&drain(&mut events)), vec![(c, lock)]);
}

#[tokio::test]
async fn a_wallet_registered_after_sync_is_rechecked_when_its_scan_completes() {
    let (mut h, ids) = Harness::new(&[MNEMONIC_A]).await;
    let a = ids[0];
    h.filters_synced().await;
    h.manager.tick(&h.requests).await.unwrap();

    // A spend, signed by C, of a coin C's scan has not found yet, paying A.
    let to_a = next_unused_receive_address(&h.wallets, &a).await;
    let c = register(&h.wallets, MNEMONIC_C).await;
    let to_c = next_unused_receive_address(&h.wallets, &c).await;
    let funding = Transaction::dummy(&to_c, 11..12, &[COIN]);
    let spend = signed_spend(&h.wallets, &c, &to_c, &funding, &to_a).await;
    h.manager.tick(&h.requests).await.unwrap();
    assert!(h.announce(&spend).await);

    h.wallets
        .write()
        .await
        .check_transaction_in_all_wallets(&funding, in_block(950), true, true)
        .await;
    assert_eq!(h.total(&c).await, COIN);
    h.filters_synced().await;

    assert_eq!(h.total(&c).await, 0, "C's scan completing re-checks the held spend");
    assert_eq!(h.total(&a).await, PAYMENT);
}

#[test_case::test_case(true; "lock before transaction")]
#[test_case::test_case(false; "transaction before lock")]
#[tokio::test]
async fn a_verified_lock_reaches_the_wallet_for_storage_in_either_order(lock_first: bool) {
    let (mut h, ids) = Harness::new(&[MNEMONIC_A]).await;
    let a = ids[0];
    let to_a = next_unused_receive_address(&h.wallets, &a).await;
    let payment = Transaction::dummy(&to_a, 13..14, &[COIN]);
    let lock = lock_for(&payment);
    let mut events = h.events().await;

    if lock_first {
        h.manager.process_instant_send(lock.clone()).await;
        assert!(h.announce(&payment).await);
    } else {
        assert!(h.announce(&payment).await);
        h.manager.process_instant_send(lock.clone()).await;
    }

    assert_eq!(locks_announced(&drain(&mut events)), vec![(a, lock)]);
    assert!(h.manager.transactions[&payment.txid()].is_instant_send);
}
