//! The mempool manager against real wallets: a transaction it holds or found
//! irrelevant is looked at again when the wallets' view changes, and a
//! verified lock reaches the wallet in either delivery order.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::Arc;

use dashcore::ephemerealdata::instant_lock::InstantLock;
use dashcore::hashes::Hash;
use dashcore::network::message::NetworkMessage;
use dashcore::network::message_blockdata::Inventory;
use dashcore::{Address, Block, BlockHash, Network, OutPoint, Transaction, TxIn, TxOut, Txid};
use key_wallet::managed_account::managed_account_trait::ManagedAccountTrait;
use key_wallet::transaction_checking::{BlockInfo, TransactionContext};
use key_wallet::wallet::initialization::WalletAccountCreationOptions;
use key_wallet::wallet::managed_wallet_info::transaction_builder::TransactionSigner;
use key_wallet::wallet::managed_wallet_info::wallet_info_interface::WalletInfoInterface;
use key_wallet::wallet::managed_wallet_info::ManagedWalletInfo;
use key_wallet::Utxo;
use key_wallet_manager::{WalletEvent, WalletId, WalletInterface, WalletManager};
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
const MNEMONIC_D: &str = "zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo wrong";

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
            crate::sync::RebroadcastStops::new(),
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
    let output = TxOut {
        value: PAYMENT,
        script_pubkey: to.script_pubkey(),
    };
    signed_spend_of(wallets, owner, from, OutPoint::new(funding.txid(), 0), funding, vec![output])
        .await
}

/// A spend of `outpoint`, an output of `funding` paying `from`, to
/// `outputs`, signed by `owner`.
async fn signed_spend_of(
    wallets: &Wallets,
    owner: &WalletId,
    from: &Address,
    outpoint: OutPoint,
    funding: &Transaction,
    outputs: Vec<TxOut>,
) -> Transaction {
    let spend = Transaction {
        version: 2,
        lock_time: 0,
        input: vec![TxIn {
            previous_output: outpoint,
            ..Default::default()
        }],
        output: outputs,
        special_transaction_payload: None,
    };
    let wallets = wallets.read().await;
    let account = wallets.get_wallet_info(owner).unwrap().first_bip44_managed_account().unwrap();
    let path = account.address_derivation_path(from).unwrap();
    let prevout = funding.output[outpoint.vout as usize].clone();
    let coin = Utxo::new(outpoint, prevout, from.clone(), 0, false);
    let wallet = wallets.get_wallet(owner).unwrap();
    wallet.sign_tx(spend, vec![coin], move |_| Some(path.clone())).await.unwrap()
}

/// Whether the manager sent a request for `txid`.
fn asked_for(messages: &[NetworkMessage], txid: Txid) -> bool {
    messages.iter().any(|message| {
        matches!(message, NetworkMessage::GetData(items)
            if items.contains(&Inventory::Transaction(txid)))
    })
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

/// A held payment the wallet dropped as dead, beaten by a locked rival or
/// abandoned, is not credited again when another wallet's registration
/// replays what the manager holds.
#[tokio::test]
async fn a_replay_brings_back_neither_a_beaten_nor_an_abandoned_payment() {
    let (mut h, ids) = Harness::new(&[MNEMONIC_A]).await;
    let a = ids[0];
    h.filters_synced().await;
    h.manager.tick(&h.requests).await.unwrap();
    let to_a = next_unused_receive_address(&h.wallets, &a).await;
    let elsewhere = address_of_unregistered(MNEMONIC_B).await;

    let beaten = Transaction::dummy(&to_a, 11..12, &[COIN]);
    let abandoned = Transaction::dummy(&to_a, 12..13, &[COIN]);
    assert!(h.announce(&beaten).await);
    assert!(h.announce(&abandoned).await);
    assert_eq!(h.total(&a).await, 2 * COIN);

    let mut winner = Transaction::dummy(&elsewhere, 0..1, &[COIN]);
    winner.input = beaten.input.clone();
    h.wallets.write().await.process_mempool_transaction(&winner, Some(lock_for(&winner))).await;
    {
        let mut wallets = h.wallets.write().await;
        let info = wallets.get_wallet_info_mut(&a).unwrap();
        info.abandon_transaction(abandoned.txid());
        info.update_balance();
    }
    assert_eq!(h.total(&a).await, 0, "the precondition");

    register(&h.wallets, MNEMONIC_C).await;
    h.manager.tick(&h.requests).await.unwrap();

    assert_eq!(h.total(&a).await, 0);
    assert_eq!(h.context_of(&a, &beaten.txid()).await, None);
    assert_eq!(h.context_of(&a, &abandoned.txid()).await, None);
}

/// A spend the user abandoned that is in fact live: a peer's relay of it is
/// ignored, but its verified lock has it fetched again, and it is then
/// credited locked with its input spent.
#[tokio::test]
async fn an_abandoned_spend_a_lock_settles_is_fetched_again_and_credited() {
    let (mut h, ids) = Harness::new(&[MNEMONIC_A]).await;
    let a = ids[0];
    h.filters_synced().await;
    h.manager.tick(&h.requests).await.unwrap();
    let to_a = next_unused_receive_address(&h.wallets, &a).await;
    let elsewhere = address_of_unregistered(MNEMONIC_B).await;
    let funding = Transaction::dummy(&to_a, 31..32, &[COIN]);
    h.wallets
        .write()
        .await
        .check_transaction_in_all_wallets(&funding, in_block(960), true, true)
        .await;
    let spend = signed_spend(&h.wallets, &a, &to_a, &funding, &elsewhere).await;
    {
        let mut wallets = h.wallets.write().await;
        wallets.process_mempool_transaction(&spend, None).await;
        let info = wallets.get_wallet_info_mut(&a).unwrap();
        info.abandon_transaction(spend.txid());
        info.update_balance();
        wallets.check_transaction_in_all_wallets(&funding, in_block(960), true, true).await;
    }
    assert_eq!(h.total(&a).await, COIN, "the rescan after the abandon finds the coin");

    assert!(h.announce(&spend).await);
    assert!(!h.manager.transactions.contains_key(&spend.txid()), "ignored as abandoned");

    h.manager.process_instant_send(lock_for(&spend)).await;
    h.manager.send_queued(&h.requests).await.unwrap();
    let asked = h.sent_messages().iter().any(|message| {
        matches!(message, NetworkMessage::GetData(items)
            if items.contains(&Inventory::Transaction(spend.txid())))
    });
    assert!(asked, "the lock has the abandoned spend fetched again");
    h.manager.handle_tx(spend.clone(), h.peer, &h.requests).await.unwrap();

    assert!(matches!(
        h.context_of(&a, &spend.txid()).await,
        Some(TransactionContext::InstantSend(_))
    ));
    let wallets = h.wallets.read().await;
    let account = wallets.get_wallet_info(&a).unwrap().first_bip44_managed_account().unwrap();
    assert!(!account.utxos.contains_key(&OutPoint::new(funding.txid(), 0)), "not offered");
    assert_eq!(wallets.get_wallet_info(&a).unwrap().balance().total(), 0);
}

/// A payment one wallet abandoned is not credited to a wallet registered
/// later by the replay; that wallet gets it once a lock or a block settles it.
#[test_case::test_case(true; "settled by a lock")]
#[test_case::test_case(false; "settled by a block")]
#[tokio::test]
async fn a_wallet_registered_after_an_abandon_gets_the_payment_only_once_settled(by_lock: bool) {
    let (mut h, ids) = Harness::new(&[MNEMONIC_A]).await;
    let a = ids[0];
    h.filters_synced().await;
    h.manager.tick(&h.requests).await.unwrap();
    let to_a = next_unused_receive_address(&h.wallets, &a).await;
    let to_b = address_of_unregistered(MNEMONIC_B).await;
    let mut payment = Transaction::dummy(&to_a, 41..42, &[COIN]);
    payment.output.push(TxOut {
        value: COIN,
        script_pubkey: to_b.script_pubkey(),
    });
    assert!(h.announce(&payment).await);
    {
        let mut wallets = h.wallets.write().await;
        let info = wallets.get_wallet_info_mut(&a).unwrap();
        info.abandon_transaction(payment.txid());
        info.update_balance();
    }

    let b = register(&h.wallets, MNEMONIC_B).await;
    h.manager.tick(&h.requests).await.unwrap();
    assert_eq!(h.total(&b).await, 0, "the replay does not hand B what A dropped as dead");

    if by_lock {
        h.manager.process_instant_send(lock_for(&payment)).await;
    } else {
        h.wallets
            .write()
            .await
            .check_transaction_in_all_wallets(&payment, in_block(970), true, true)
            .await;
    }
    assert_eq!(h.total(&b).await, COIN);
}

/// The user abandoned P, which is in fact live; another device spent P's
/// change in C, which is ignored unlocked. P comes back with its lock, which
/// releases C: delivered again, C is credited, and its own lock or block
/// settles it.
#[test_case::test_case(true; "settled by a lock")]
#[test_case::test_case(false; "settled by a block")]
#[tokio::test]
async fn a_spend_of_an_abandoned_payment_comes_back_once_settled(by_lock: bool) {
    let (mut h, ids) = Harness::new(&[MNEMONIC_A]).await;
    let a = ids[0];
    h.filters_synced().await;
    h.manager.tick(&h.requests).await.unwrap();
    let to_a = next_unused_receive_address(&h.wallets, &a).await;
    let change_a = next_unused_receive_address(&h.wallets, &a).await;
    let elsewhere = address_of_unregistered(MNEMONIC_B).await;
    let funding = Transaction::dummy(&to_a, 51..52, &[COIN]);
    h.wallets
        .write()
        .await
        .check_transaction_in_all_wallets(&funding, in_block(960), true, true)
        .await;
    let change = 690_000;
    let payment = signed_spend_of(
        &h.wallets,
        &a,
        &to_a,
        OutPoint::new(funding.txid(), 0),
        &funding,
        vec![
            TxOut {
                value: 300_000,
                script_pubkey: elsewhere.script_pubkey(),
            },
            TxOut {
                value: change,
                script_pubkey: change_a.script_pubkey(),
            },
        ],
    )
    .await;
    let child = signed_spend_of(
        &h.wallets,
        &a,
        &change_a,
        OutPoint::new(payment.txid(), 1),
        &payment,
        vec![TxOut {
            value: change - 10_000,
            script_pubkey: elsewhere.script_pubkey(),
        }],
    )
    .await;
    {
        let mut wallets = h.wallets.write().await;
        wallets.process_mempool_transaction(&payment, None).await;
        let info = wallets.get_wallet_info_mut(&a).unwrap();
        info.abandon_transaction(payment.txid());
        info.update_balance();
        wallets.check_transaction_in_all_wallets(&funding, in_block(960), true, true).await;
    }
    assert!(h.announce(&payment).await);
    assert!(h.announce(&child).await);
    assert!(!h.manager.transactions.contains_key(&child.txid()), "ignored unlocked");

    h.manager.process_instant_send(lock_for(&payment)).await;
    h.manager.send_queued(&h.requests).await.unwrap();
    assert!(asked_for(&h.sent_messages(), payment.txid()));
    h.manager.handle_tx(payment.clone(), h.peer, &h.requests).await.unwrap();
    assert_eq!(h.total(&a).await, change, "P is back with its change");

    h.manager.refresh_wallet_view(&h.requests).await.unwrap();
    h.sent_messages();
    assert!(h.announce(&child).await, "a peer sends C again");
    assert!(h.manager.transactions.contains_key(&child.txid()), "P is locked, so C is live");
    assert_eq!(h.total(&a).await, 0);

    if by_lock {
        h.manager.process_instant_send(lock_for(&child)).await;
        assert!(matches!(
            h.context_of(&a, &child.txid()).await,
            Some(TransactionContext::InstantSend(_))
        ));
    } else {
        h.wallets
            .write()
            .await
            .check_transaction_in_all_wallets(&child, in_block(970), true, true)
            .await;
    }
    assert_eq!(h.total(&a).await, 0, "P's change is no longer offered");
}

/// A lock for a dismissed transaction whose seen entry is gone still has it
/// fetched again.
#[tokio::test]
async fn a_lock_for_a_dismissed_transaction_no_longer_seen_fetches_it_again() {
    let (mut h, ids) = Harness::new(&[MNEMONIC_A]).await;
    let a = ids[0];
    h.filters_synced().await;
    h.manager.tick(&h.requests).await.unwrap();
    let to_a = next_unused_receive_address(&h.wallets, &a).await;
    let payment = Transaction::dummy(&to_a, 61..62, &[COIN]);
    {
        let mut wallets = h.wallets.write().await;
        wallets.process_mempool_transaction(&payment, None).await;
        let info = wallets.get_wallet_info_mut(&a).unwrap();
        info.abandon_transaction(payment.txid());
        info.update_balance();
    }
    assert!(h.announce(&payment).await);
    assert!(!h.manager.transactions.contains_key(&payment.txid()), "ignored as abandoned");
    h.manager.refresh_wallet_view(&h.requests).await.unwrap();
    h.sent_messages();

    h.manager.process_instant_send(lock_for(&payment)).await;
    h.manager.send_queued(&h.requests).await.unwrap();
    assert!(asked_for(&h.sent_messages(), payment.txid()), "the lock fetches it again");
    h.manager.handle_tx(payment.clone(), h.peer, &h.requests).await.unwrap();
    assert_eq!(h.total(&a).await, COIN);
}

/// Wallet A abandoned P, which is in fact live. C spends P's payment and pays
/// wallet B, so A dismisses it on arrival; C is then mined in a block only B
/// matched. An unlocked spend of C paying B is credited to B.
#[tokio::test]
async fn a_dismissal_ends_when_a_block_mines_the_transaction_for_any_wallet() {
    let (h, ids) = Harness::new(&[MNEMONIC_A, MNEMONIC_B]).await;
    let (a, b) = (ids[0], ids[1]);
    let to_a = next_unused_receive_address(&h.wallets, &a).await;
    let to_b = next_unused_receive_address(&h.wallets, &b).await;
    let elsewhere = address_of_unregistered(MNEMONIC_C).await;
    let funding = Transaction::dummy(&to_a, 71..72, &[COIN]);
    let mut wallets = h.wallets.write().await;
    wallets.check_transaction_in_all_wallets(&funding, in_block(960), true, true).await;
    drop(wallets);
    let payment = signed_spend(&h.wallets, &a, &to_a, &funding, &elsewhere).await;
    let spend_of = |parent: &Transaction, vout: u32, outputs: Vec<TxOut>| Transaction {
        version: 2,
        lock_time: 0,
        input: vec![TxIn {
            previous_output: OutPoint::new(parent.txid(), vout),
            ..Default::default()
        }],
        output: outputs,
        special_transaction_payload: None,
    };
    let pays = |value: u64, to: &Address| TxOut {
        value,
        script_pubkey: to.script_pubkey(),
    };
    let child = spend_of(&payment, 0, vec![pays(400_000, &to_b), pays(580_000, &elsewhere)]);
    let grandchild = spend_of(&child, 1, vec![pays(570_000, &to_b)]);

    let mut wallets = h.wallets.write().await;
    wallets.process_mempool_transaction(&payment, None).await;
    let info = wallets.get_wallet_info_mut(&a).unwrap();
    info.abandon_transaction(payment.txid());
    info.update_balance();
    let ignored = wallets.process_mempool_transaction(&child, None).await;
    assert!(!ignored.is_relevant, "C spends what A abandoned");

    let block = Block::dummy(970, vec![child.clone()]);
    wallets.process_block_for_wallets(&block, block.block_hash(), 970, &BTreeSet::from([b])).await;
    drop(wallets);
    assert_eq!(h.total(&b).await, 400_000);

    let credited = h.wallets.write().await.process_mempool_transaction(&grandchild, None).await;
    assert!(credited.is_relevant, "C is mined, so its spend is live");
    assert_eq!(h.total(&b).await, 970_000);
    assert!(!h.wallets.read().await.is_dismissed(&child.txid()));
}

/// A payment of wallet `a`'s coin to `elsewhere` with change back to `a`, and
/// the change address. The payment is seen unlocked, held by the mempool
/// manager if `held`, and then abandoned.
async fn abandoned_payment_with_change(
    h: &mut Harness,
    a: &WalletId,
    elsewhere: &Address,
    funding_inputs: std::ops::Range<u8>,
    held: bool,
) -> (Transaction, Address) {
    let to_a = next_unused_receive_address(&h.wallets, a).await;
    let change_a = next_unused_receive_address(&h.wallets, a).await;
    let funding = Transaction::dummy(&to_a, funding_inputs, &[COIN]);
    h.wallets
        .write()
        .await
        .check_transaction_in_all_wallets(&funding, in_block(960), true, true)
        .await;
    let payment = signed_spend_of(
        &h.wallets,
        a,
        &to_a,
        OutPoint::new(funding.txid(), 0),
        &funding,
        vec![
            TxOut {
                value: 300_000,
                script_pubkey: elsewhere.script_pubkey(),
            },
            TxOut {
                value: CHANGE,
                script_pubkey: change_a.script_pubkey(),
            },
        ],
    )
    .await;
    if held {
        assert!(h.announce(&payment).await);
        assert!(h.manager.transactions.contains_key(&payment.txid()));
    }
    let mut wallets = h.wallets.write().await;
    wallets.process_mempool_transaction(&payment, None).await;
    let info = wallets.get_wallet_info_mut(a).unwrap();
    info.abandon_transaction(payment.txid());
    info.update_balance();
    wallets.check_transaction_in_all_wallets(&funding, in_block(960), true, true).await;
    (payment, change_a)
}

const CHANGE: u64 = 690_000;

/// Unlocked transactions spending a transaction a wallet abandoned are
/// recorded only as hints for their lock. However many arrive, they do not
/// push the wallet's own abandons out.
#[tokio::test]
async fn spends_of_a_dismissed_transaction_do_not_push_out_an_abandon() {
    let (h, ids) = Harness::new(&[MNEMONIC_A]).await;
    let a = ids[0];
    let to_a = next_unused_receive_address(&h.wallets, &a).await;
    let elsewhere = address_of_unregistered(MNEMONIC_B).await;
    let first = Transaction::dummy(&to_a, 81..82, &[COIN]);
    let second = Transaction::dummy(&to_a, 82..83, &[COIN]);
    let mut wallets = h.wallets.write().await;
    for payment in [&first, &second] {
        wallets.process_mempool_transaction(payment, None).await;
        let info = wallets.get_wallet_info_mut(&a).unwrap();
        info.abandon_transaction(payment.txid());
        info.update_balance();
    }

    // More than a wallet keeps dismissals of its own.
    for vout in 0..4_096 {
        let spam = Transaction {
            version: 2,
            lock_time: 0,
            input: vec![TxIn {
                previous_output: OutPoint::new(second.txid(), vout),
                ..Default::default()
            }],
            output: vec![TxOut {
                value: 1_000,
                script_pubkey: elsewhere.script_pubkey(),
            }],
            special_transaction_payload: None,
        };
        wallets.process_mempool_transaction(&spam, None).await;
    }

    assert!(wallets.is_dismissed(&first.txid()), "the abandon is kept");
    assert!(wallets.is_dismissed(&second.txid()));
    let again = wallets.process_mempool_transaction(&first, None).await;
    assert!(!again.is_relevant, "the abandoned payment stays out unlocked");
    drop(wallets);
    assert_eq!(h.total(&a).await, 0);
}

/// An unlocked spend of an abandoned payment, ignored on arrival, is credited
/// when it is delivered again after the payment comes back, by its lock or a
/// block.
#[test_case::test_case(true; "parent settled by a lock")]
#[test_case::test_case(false; "parent settled by a block")]
#[tokio::test]
async fn a_spend_delivered_again_after_its_abandoned_parent_settles_is_credited(by_lock: bool) {
    let (mut h, ids) = Harness::new(&[MNEMONIC_A]).await;
    let a = ids[0];
    let elsewhere = address_of_unregistered(MNEMONIC_B).await;
    let (payment, change_a) =
        abandoned_payment_with_change(&mut h, &a, &elsewhere, 91..92, false).await;
    let to_a = next_unused_receive_address(&h.wallets, &a).await;
    let child = signed_spend_of(
        &h.wallets,
        &a,
        &change_a,
        OutPoint::new(payment.txid(), 1),
        &payment,
        vec![TxOut {
            value: CHANGE - 10_000,
            script_pubkey: to_a.script_pubkey(),
        }],
    )
    .await;
    let mut wallets = h.wallets.write().await;
    let ignored = wallets.process_mempool_transaction(&child, None).await;
    assert!(!ignored.is_relevant, "it spends what A abandoned");

    if by_lock {
        wallets.process_mempool_transaction(&payment, Some(lock_for(&payment))).await;
    } else {
        let block = Block::dummy(970, vec![payment.clone()]);
        let scope = BTreeSet::from([a]);
        wallets.process_block_for_wallets(&block, block.block_hash(), 970, &scope).await;
    }
    drop(wallets);
    assert_eq!(h.total(&a).await, CHANGE, "the payment's change is back");

    let credited = h.wallets.write().await.process_mempool_transaction(&child, None).await;
    assert!(credited.is_relevant, "its parent settled, so it is live");
    assert_eq!(h.total(&a).await, CHANGE - 10_000);
    assert!(!h.wallets.read().await.is_dismissed(&child.txid()));
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ComesBack {
    /// By its lock, fetched again.
    Lock,
    /// By its lock, while the mempool manager still holds it.
    LockWhileHeld,
    /// By a block.
    Block,
}

/// The user abandoned P, which is in fact live; another device spent P's
/// change in C, paying only elsewhere. C's lock arrives first, so C is
/// fetched while P's change is unknown and matches no wallet. Once P comes
/// back, C is credited and P's change is not offered.
#[test_case::test_case(ComesBack::Lock; "parent settled by a lock")]
#[test_case::test_case(ComesBack::LockWhileHeld; "parent held and settled by a lock")]
#[test_case::test_case(ComesBack::Block; "parent settled by a block")]
#[tokio::test]
async fn a_locked_spend_fetched_before_its_abandoned_parent_comes_back_is_credited(
    comes_back: ComesBack,
) {
    let (mut h, ids) = Harness::new(&[MNEMONIC_A]).await;
    let a = ids[0];
    h.filters_synced().await;
    h.manager.tick(&h.requests).await.unwrap();
    let elsewhere = address_of_unregistered(MNEMONIC_B).await;
    let held = comes_back == ComesBack::LockWhileHeld;
    let (payment, change_a) =
        abandoned_payment_with_change(&mut h, &a, &elsewhere, 101..102, held).await;
    let child = signed_spend_of(
        &h.wallets,
        &a,
        &change_a,
        OutPoint::new(payment.txid(), 1),
        &payment,
        vec![TxOut {
            value: CHANGE - 10_000,
            script_pubkey: elsewhere.script_pubkey(),
        }],
    )
    .await;
    assert_eq!(h.announce(&payment).await, !held);
    assert!(h.announce(&child).await);
    assert!(!h.manager.transactions.contains_key(&child.txid()), "ignored unlocked");

    h.manager.process_instant_send(lock_for(&child)).await;
    h.manager.send_queued(&h.requests).await.unwrap();
    assert!(asked_for(&h.sent_messages(), child.txid()), "C's lock has it fetched again");
    h.manager.handle_tx(child.clone(), h.peer, &h.requests).await.unwrap();
    assert_eq!(h.total(&a).await, COIN, "P is abandoned, so A holds its input");

    if comes_back == ComesBack::LockWhileHeld {
        h.manager.process_instant_send(lock_for(&payment)).await;
    } else if comes_back == ComesBack::Lock {
        h.manager.process_instant_send(lock_for(&payment)).await;
        h.manager.send_queued(&h.requests).await.unwrap();
        assert!(asked_for(&h.sent_messages(), payment.txid()));
        h.manager.handle_tx(payment.clone(), h.peer, &h.requests).await.unwrap();
    } else {
        let block = Block::dummy(970, vec![payment.clone()]);
        let result = h
            .wallets
            .write()
            .await
            .process_block_for_wallets(&block, block.block_hash(), 970, &BTreeSet::from([a]))
            .await;
        let event = SyncEvent::BlockProcessed {
            block_hash: block.block_hash(),
            height: 970,
            wallets: BTreeSet::from([a]),
            new_scripts: Default::default(),
            confirmed_txids: result.relevant_txids().cloned().collect(),
        };
        h.manager.handle_sync_event(&event, &h.requests).await.unwrap();
    }
    assert!(
        matches!(h.context_of(&a, &child.txid()).await, Some(TransactionContext::InstantSend(_))),
        "C is credited with its lock"
    );
    assert_eq!(h.total(&a).await, 0, "P's change is spent by C");
}

/// A spend of a payment that the wallet abandoned with the payment keeps its
/// own mark when the payment comes back, by its lock or a block: only spends
/// held back with a dismissed transaction are released.
#[test_case::test_case(true; "parent settled by a lock")]
#[test_case::test_case(false; "parent settled by a block")]
#[tokio::test]
async fn an_abandoned_spend_stays_out_when_its_parent_settles(by_lock: bool) {
    let (h, ids) = Harness::new(&[MNEMONIC_A]).await;
    let a = ids[0];
    let to_a = next_unused_receive_address(&h.wallets, &a).await;
    let change_a = next_unused_receive_address(&h.wallets, &a).await;
    let elsewhere = address_of_unregistered(MNEMONIC_B).await;
    let funding = Transaction::dummy(&to_a, 111..112, &[COIN]);
    h.wallets
        .write()
        .await
        .check_transaction_in_all_wallets(&funding, in_block(960), true, true)
        .await;
    let payment = signed_spend_of(
        &h.wallets,
        &a,
        &to_a,
        OutPoint::new(funding.txid(), 0),
        &funding,
        vec![
            TxOut {
                value: 300_000,
                script_pubkey: elsewhere.script_pubkey(),
            },
            TxOut {
                value: CHANGE,
                script_pubkey: change_a.script_pubkey(),
            },
        ],
    )
    .await;
    let child = signed_spend_of(
        &h.wallets,
        &a,
        &change_a,
        OutPoint::new(payment.txid(), 1),
        &payment,
        vec![TxOut {
            value: CHANGE - 10_000,
            script_pubkey: elsewhere.script_pubkey(),
        }],
    )
    .await;
    let mut wallets = h.wallets.write().await;
    wallets.process_mempool_transaction(&payment, None).await;
    assert!(wallets.process_mempool_transaction(&child, None).await.is_relevant);
    let info = wallets.get_wallet_info_mut(&a).unwrap();
    let outcome = info.abandon_transaction(payment.txid());
    assert!(outcome.abandoned.contains(&child.txid()), "abandoned with the payment");
    info.update_balance();
    wallets.check_transaction_in_all_wallets(&funding, in_block(960), true, true).await;

    if by_lock {
        wallets.process_mempool_transaction(&payment, Some(lock_for(&payment))).await;
    } else {
        let block = Block::dummy(970, vec![payment.clone()]);
        let scope = BTreeSet::from([a]);
        wallets.process_block_for_wallets(&block, block.block_hash(), 970, &scope).await;
    }
    assert!(!wallets.is_dismissed(&payment.txid()));
    assert!(wallets.is_dismissed(&child.txid()), "the abandon of the spend stays");
    let again = wallets.process_mempool_transaction(&child, None).await;
    assert!(!again.is_relevant, "the abandoned spend stays out unlocked");
    drop(wallets);
    assert_eq!(h.total(&a).await, CHANGE, "the payment's change is offered");
}

/// The user abandoned P, which is in fact live. Another device spent P's
/// change in S, ignored unlocked, then S's change in C, locked. P comes back
/// by its lock, which releases S. C, fetched with its lock before S comes
/// back, matches no wallet. Once S comes back, C is credited and S's change
/// is not offered.
#[tokio::test]
async fn a_locked_spend_of_a_released_spend_is_credited_when_that_spend_comes_back() {
    let (mut h, ids) = Harness::new(&[MNEMONIC_A]).await;
    let a = ids[0];
    h.filters_synced().await;
    h.manager.tick(&h.requests).await.unwrap();
    let elsewhere = address_of_unregistered(MNEMONIC_B).await;
    let (payment, change_a) =
        abandoned_payment_with_change(&mut h, &a, &elsewhere, 121..122, false).await;
    let spend_change_a = next_unused_receive_address(&h.wallets, &a).await;
    assert_ne!(spend_change_a, change_a);
    let spend_change = 380_000;
    let spend = signed_spend_of(
        &h.wallets,
        &a,
        &change_a,
        OutPoint::new(payment.txid(), 1),
        &payment,
        vec![
            TxOut {
                value: CHANGE - spend_change - 10_000,
                script_pubkey: elsewhere.script_pubkey(),
            },
            TxOut {
                value: spend_change,
                script_pubkey: spend_change_a.script_pubkey(),
            },
        ],
    )
    .await;
    let child = signed_spend_of(
        &h.wallets,
        &a,
        &spend_change_a,
        OutPoint::new(spend.txid(), 1),
        &spend,
        vec![TxOut {
            value: spend_change - 10_000,
            script_pubkey: elsewhere.script_pubkey(),
        }],
    )
    .await;
    assert!(h.announce(&payment).await);
    assert!(h.announce(&spend).await);
    assert!(h.wallets.read().await.is_dismissed(&spend.txid()), "S is held with P");

    h.manager.process_instant_send(lock_for(&payment)).await;
    h.manager.send_queued(&h.requests).await.unwrap();
    assert!(asked_for(&h.sent_messages(), payment.txid()));
    h.manager.handle_tx(payment.clone(), h.peer, &h.requests).await.unwrap();
    assert_eq!(h.total(&a).await, CHANGE, "P is back with its change");
    assert!(!h.wallets.read().await.is_dismissed(&spend.txid()), "P's lock releases S");

    h.manager.process_instant_send(lock_for(&child)).await;
    assert!(h.announce(&child).await);
    assert!(!h.manager.transactions.contains_key(&child.txid()), "S's change is unknown");

    h.manager.process_instant_send(lock_for(&spend)).await;
    h.manager.send_queued(&h.requests).await.unwrap();
    assert!(asked_for(&h.sent_messages(), spend.txid()), "S's lock has it fetched again");
    h.manager.handle_tx(spend.clone(), h.peer, &h.requests).await.unwrap();
    assert!(
        matches!(h.context_of(&a, &child.txid()).await, Some(TransactionContext::InstantSend(_))),
        "C is credited with its lock"
    );
    assert_eq!(h.total(&a).await, 0, "S's change is spent by C");
}

/// Wallet A abandoned P, which is in fact live; C spends P's change and pays
/// wallet B, and arrives with its lock before P comes back. C matches only B,
/// yet once P comes back, by its lock or a block, A does not offer the
/// change C spends.
#[test_case::test_case(true; "parent settled by a lock")]
#[test_case::test_case(false; "parent settled by a block")]
#[tokio::test]
async fn a_locked_spend_paying_another_wallet_keeps_the_abandoned_change_spent(by_lock: bool) {
    let (mut h, ids) = Harness::new(&[MNEMONIC_A, MNEMONIC_B]).await;
    let (a, b) = (ids[0], ids[1]);
    h.filters_synced().await;
    h.manager.tick(&h.requests).await.unwrap();
    let elsewhere = address_of_unregistered(MNEMONIC_C).await;
    let (payment, change_a) =
        abandoned_payment_with_change(&mut h, &a, &elsewhere, 131..132, false).await;
    let to_b = next_unused_receive_address(&h.wallets, &b).await;
    let child = signed_spend_of(
        &h.wallets,
        &a,
        &change_a,
        OutPoint::new(payment.txid(), 1),
        &payment,
        vec![TxOut {
            value: CHANGE - 10_000,
            script_pubkey: to_b.script_pubkey(),
        }],
    )
    .await;
    assert!(h.announce(&payment).await);
    h.manager.process_instant_send(lock_for(&child)).await;
    assert!(h.announce(&child).await);
    assert!(h.manager.transactions.contains_key(&child.txid()), "C pays B");
    assert_eq!(h.total(&b).await, CHANGE - 10_000);

    if by_lock {
        h.manager.process_instant_send(lock_for(&payment)).await;
        h.manager.send_queued(&h.requests).await.unwrap();
        assert!(asked_for(&h.sent_messages(), payment.txid()));
        h.manager.handle_tx(payment.clone(), h.peer, &h.requests).await.unwrap();
    } else {
        let block = Block::dummy(970, vec![payment.clone()]);
        let result = h
            .wallets
            .write()
            .await
            .process_block_for_wallets(&block, block.block_hash(), 970, &BTreeSet::from([a]))
            .await;
        let event = SyncEvent::BlockProcessed {
            block_hash: block.block_hash(),
            height: 970,
            wallets: BTreeSet::from([a]),
            new_scripts: Default::default(),
            confirmed_txids: result.relevant_txids().cloned().collect(),
        };
        h.manager.handle_sync_event(&event, &h.requests).await.unwrap();
    }
    assert!(h.context_of(&a, &payment.txid()).await.is_some(), "P is back in A");
    let change = OutPoint::new(payment.txid(), 1);
    let wallets = h.wallets.read().await;
    let info = wallets.get_wallet_info(&a).unwrap();
    assert!(!info.utxos().iter().any(|utxo| utxo.outpoint == change), "C spends P's change");
    drop(wallets);
    assert_eq!(h.total(&a).await, 0);
    assert_eq!(h.total(&b).await, CHANGE - 10_000);
}

/// Wallet A abandoned P, which is in fact live, and C, a locked spend of P's
/// change that matched no wallet, arrived before P came back. By P's lock,
/// the call that restores P passes C to the wallet under the same write
/// guard, so P's change is already spent when it returns. By a block, P is
/// restored as the block is processed, and C follows when the mempool
/// manager handles that block: until then P's change shows.
#[test_case::test_case(true; "parent settled by a lock")]
#[test_case::test_case(false; "parent settled by a block")]
#[tokio::test]
async fn restoring_a_dismissed_transaction_passes_a_kept_locked_spend_of_its_change_again(
    by_lock: bool,
) {
    let (mut h, ids) = Harness::new(&[MNEMONIC_A]).await;
    let a = ids[0];
    h.filters_synced().await;
    h.manager.tick(&h.requests).await.unwrap();
    let elsewhere = address_of_unregistered(MNEMONIC_B).await;
    let (payment, change_a) =
        abandoned_payment_with_change(&mut h, &a, &elsewhere, 141..142, false).await;
    let child = signed_spend_of(
        &h.wallets,
        &a,
        &change_a,
        OutPoint::new(payment.txid(), 1),
        &payment,
        vec![TxOut {
            value: CHANGE - 10_000,
            script_pubkey: elsewhere.script_pubkey(),
        }],
    )
    .await;
    h.manager.process_instant_send(lock_for(&child)).await;
    assert!(h.announce(&child).await);
    assert!(h.manager.locked_spends.contains_key(&child.txid()), "P's change is unknown");
    let change = OutPoint::new(payment.txid(), 1);
    let shows = |wallets: &WalletManager<ManagedWalletInfo>| {
        wallets.get_wallet_info(&a).unwrap().utxos().iter().any(|utxo| utxo.outpoint == change)
    };

    if by_lock {
        h.manager.process_instant_send(lock_for(&payment)).await;
        h.manager.send_queued(&h.requests).await.unwrap();
        assert!(asked_for(&h.sent_messages(), payment.txid()));
        h.manager.handle_tx(payment.clone(), h.peer, &h.requests).await.unwrap();
        assert!(!shows(&*h.wallets.read().await), "C spends P's change");
    } else {
        let block = Block::dummy(970, vec![payment.clone()]);
        let scope = BTreeSet::from([a]);
        let result = h
            .wallets
            .write()
            .await
            .process_block_for_wallets(&block, block.block_hash(), 970, &scope)
            .await;
        assert!(shows(&*h.wallets.read().await), "C is not passed again yet");
        let event = SyncEvent::BlockProcessed {
            block_hash: block.block_hash(),
            height: 970,
            wallets: scope,
            new_scripts: Default::default(),
            confirmed_txids: result.relevant_txids().cloned().collect(),
        };
        h.manager.handle_sync_event(&event, &h.requests).await.unwrap();
        assert!(!shows(&*h.wallets.read().await), "C spends P's change");
    }
    assert!(h.context_of(&a, &payment.txid()).await.is_some(), "P is back in A");
    assert!(h.context_of(&a, &child.txid()).await.is_some(), "C is recorded in A");
    assert!(!h.manager.locked_spends.contains_key(&child.txid()), "C is no longer kept");
    assert_eq!(h.total(&a).await, 0);
}

/// Wallet A abandoned P, which is in fact live. C spends P's change in a
/// block A's scan reaches before P comes back, with no InstantSend lock. A
/// fork then drops C's block: P's change is A's again at once.
#[test_case::test_case(true; "parent back by its lock")]
#[test_case::test_case(false; "parent back in a block")]
#[tokio::test]
async fn a_fork_dropping_a_mined_spend_gives_back_the_abandoned_change(by_lock: bool) {
    let (mut h, ids) = Harness::new(&[MNEMONIC_A]).await;
    let a = ids[0];
    let elsewhere = address_of_unregistered(MNEMONIC_B).await;
    let (payment, change_a) =
        abandoned_payment_with_change(&mut h, &a, &elsewhere, 151..152, false).await;
    let child = signed_spend_of(
        &h.wallets,
        &a,
        &change_a,
        OutPoint::new(payment.txid(), 1),
        &payment,
        vec![TxOut {
            value: CHANGE - 10_000,
            script_pubkey: elsewhere.script_pubkey(),
        }],
    )
    .await;
    let scope = BTreeSet::from([a]);
    let mut wallets = h.wallets.write().await;
    let spend_block = Block::dummy(970, vec![child.clone()]);
    wallets.process_block_for_wallets(&spend_block, spend_block.block_hash(), 970, &scope).await;
    if by_lock {
        wallets.process_mempool_transaction(&payment, Some(lock_for(&payment))).await;
    } else {
        let block = Block::dummy(965, vec![payment.clone()]);
        wallets.process_block_for_wallets(&block, block.block_hash(), 965, &scope).await;
    }
    let change = OutPoint::new(payment.txid(), 1);
    let shows = |wallets: &WalletManager<ManagedWalletInfo>| {
        wallets.get_wallet_info(&a).unwrap().utxos().iter().any(|utxo| utxo.outpoint == change)
    };
    assert!(!shows(&wallets), "C spends P's change");

    wallets.truncate_above(969).unwrap();
    assert!(shows(&wallets), "the fork dropped C");
    drop(wallets);
    assert_eq!(h.total(&a).await, CHANGE);
}

/// Wallet A abandoned P, which is in fact live. C spends P's change and a
/// coin of someone else's, pays wallet B and is InstantSend-locked; P comes
/// back by its lock. D, which spends that other coin but not P's change, is
/// then mined in a ChainLocked block, so C can never confirm: P's change is
/// A's again.
#[tokio::test]
async fn a_chain_locked_conflict_beating_a_locked_spend_gives_back_the_abandoned_change() {
    let (mut h, ids) = Harness::new(&[MNEMONIC_A, MNEMONIC_B]).await;
    let (a, b) = (ids[0], ids[1]);
    h.filters_synced().await;
    h.manager.tick(&h.requests).await.unwrap();
    let elsewhere = address_of_unregistered(MNEMONIC_C).await;
    let (payment, _) = abandoned_payment_with_change(&mut h, &a, &elsewhere, 161..162, false).await;
    let to_b = next_unused_receive_address(&h.wallets, &b).await;
    let other = OutPoint::new(Txid::from_byte_array([0x58; 32]), 0);
    let child = Transaction {
        version: 2,
        lock_time: 0,
        input: vec![
            TxIn {
                previous_output: OutPoint::new(payment.txid(), 1),
                ..Default::default()
            },
            TxIn {
                previous_output: other,
                ..Default::default()
            },
        ],
        output: vec![TxOut {
            value: CHANGE,
            script_pubkey: to_b.script_pubkey(),
        }],
        special_transaction_payload: None,
    };
    h.manager.process_instant_send(lock_for(&child)).await;
    assert!(h.announce(&child).await);
    assert!(h.manager.transactions.contains_key(&child.txid()), "C pays B");
    h.manager.process_instant_send(lock_for(&payment)).await;
    h.manager.send_queued(&h.requests).await.unwrap();
    assert!(asked_for(&h.sent_messages(), payment.txid()));
    h.manager.handle_tx(payment.clone(), h.peer, &h.requests).await.unwrap();
    let change = OutPoint::new(payment.txid(), 1);
    let shows = |wallets: &WalletManager<ManagedWalletInfo>| {
        wallets.get_wallet_info(&a).unwrap().utxos().iter().any(|utxo| utxo.outpoint == change)
    };
    assert!(!shows(&*h.wallets.read().await), "C spends P's change");

    let winner = Transaction {
        version: 2,
        lock_time: 0,
        input: vec![TxIn {
            previous_output: other,
            ..Default::default()
        }],
        output: vec![TxOut {
            value: 500_000,
            script_pubkey: elsewhere.script_pubkey(),
        }],
        special_transaction_payload: None,
    };
    let block = Block::dummy(971, vec![winner]);
    let mut wallets = h.wallets.write().await;
    wallets.apply_chain_lock(dashcore::ChainLock::dummy(971));
    wallets
        .process_block_for_wallets(&block, block.block_hash(), 971, &BTreeSet::from([a, b]))
        .await;
    assert!(shows(&wallets), "D beat C, so P's change is unspent");
    drop(wallets);
    assert_eq!(h.total(&a).await, CHANGE);
}

/// The mempool manager keeps a locked transaction a wallet took in for
/// replay only when it spends a transaction another wallet dismissed; it
/// lets it go once that wallet has it, or once it is mined.
#[tokio::test]
async fn a_locked_spend_is_kept_for_another_wallet_only_while_that_wallet_needs_it() {
    let (mut h, ids) = Harness::new(&[MNEMONIC_A, MNEMONIC_B]).await;
    let (a, b) = (ids[0], ids[1]);
    h.filters_synced().await;
    h.manager.tick(&h.requests).await.unwrap();
    let elsewhere = address_of_unregistered(MNEMONIC_C).await;
    let (payment, _) = abandoned_payment_with_change(&mut h, &a, &elsewhere, 171..172, false).await;
    let to_a = next_unused_receive_address(&h.wallets, &a).await;
    let to_b = next_unused_receive_address(&h.wallets, &b).await;
    let spend_of_change = |to: &Address, value: u64| Transaction {
        version: 2,
        lock_time: value as u32,
        input: vec![TxIn {
            previous_output: OutPoint::new(payment.txid(), 1),
            ..Default::default()
        }],
        output: vec![TxOut {
            value,
            script_pubkey: to.script_pubkey(),
        }],
        special_transaction_payload: None,
    };

    // Locked, paying B, spending what A dismissed: kept.
    let kept = spend_of_change(&to_b, CHANGE - 10_000);
    h.manager.process_instant_send(lock_for(&kept)).await;
    assert!(h.announce(&kept).await);
    assert!(h.manager.transactions.contains_key(&kept.txid()));
    assert!(h.manager.locked_spends.contains_key(&kept.txid()), "A needs it");

    // Paying A itself: A has it, so nothing is kept.
    let to_self = spend_of_change(&to_a, CHANGE - 20_000);
    h.manager.process_instant_send(lock_for(&to_self)).await;
    assert!(h.announce(&to_self).await);
    assert!(!h.manager.locked_spends.contains_key(&to_self.txid()), "A recorded it");

    // Locked and paying B, but spending nothing dismissed: not kept.
    let funding = Transaction::dummy(&to_b, 173..174, &[COIN]);
    let unrelated = Transaction {
        version: 2,
        lock_time: 0,
        input: vec![TxIn {
            previous_output: OutPoint::new(funding.txid(), 0),
            ..Default::default()
        }],
        output: vec![TxOut {
            value: PAYMENT,
            script_pubkey: to_b.script_pubkey(),
        }],
        special_transaction_payload: None,
    };
    h.manager.process_instant_send(lock_for(&unrelated)).await;
    assert!(h.announce(&unrelated).await);
    assert!(h.manager.transactions.contains_key(&unrelated.txid()));
    assert!(!h.manager.locked_spends.contains_key(&unrelated.txid()), "nothing dismissed");

    // Unlocked, paying B, spending what A dismissed: held back, not kept.
    let unlocked = spend_of_change(&to_b, CHANGE - 30_000);
    h.announce(&unlocked).await;
    assert!(!h.manager.locked_spends.contains_key(&unlocked.txid()), "no lock");

    // Mined in a block B matched: dropped.
    let block = Block::dummy(975, vec![kept.clone()]);
    let scope = BTreeSet::from([b]);
    let result = h
        .wallets
        .write()
        .await
        .process_block_for_wallets(&block, block.block_hash(), 975, &scope)
        .await;
    let event = SyncEvent::BlockProcessed {
        block_hash: block.block_hash(),
        height: 975,
        wallets: scope,
        new_scripts: Default::default(),
        confirmed_txids: result.relevant_txids().cloned().collect(),
    };
    h.manager.handle_sync_event(&event, &h.requests).await.unwrap();
    assert!(!h.manager.locked_spends.contains_key(&kept.txid()), "C is mined");
}

/// A kept locked spend paying wallet B is passed to wallet A once A's
/// abandoned payment it spends comes back, and is let go then.
#[tokio::test]
async fn a_locked_spend_kept_for_another_wallet_is_let_go_once_replayed() {
    let (mut h, ids) = Harness::new(&[MNEMONIC_A, MNEMONIC_B]).await;
    let (a, b) = (ids[0], ids[1]);
    h.filters_synced().await;
    h.manager.tick(&h.requests).await.unwrap();
    let elsewhere = address_of_unregistered(MNEMONIC_C).await;
    let (payment, change_a) =
        abandoned_payment_with_change(&mut h, &a, &elsewhere, 181..182, false).await;
    let to_b = next_unused_receive_address(&h.wallets, &b).await;
    let child = signed_spend_of(
        &h.wallets,
        &a,
        &change_a,
        OutPoint::new(payment.txid(), 1),
        &payment,
        vec![TxOut {
            value: CHANGE - 10_000,
            script_pubkey: to_b.script_pubkey(),
        }],
    )
    .await;
    h.manager.process_instant_send(lock_for(&child)).await;
    assert!(h.announce(&child).await);
    assert!(h.manager.locked_spends.contains_key(&child.txid()));
    let block = Block::dummy(970, vec![payment.clone()]);
    let scope = BTreeSet::from([a]);
    let result = h
        .wallets
        .write()
        .await
        .process_block_for_wallets(&block, block.block_hash(), 970, &scope)
        .await;
    let event = SyncEvent::BlockProcessed {
        block_hash: block.block_hash(),
        height: 970,
        wallets: scope,
        new_scripts: Default::default(),
        confirmed_txids: result.relevant_txids().cloned().collect(),
    };
    h.manager.handle_sync_event(&event, &h.requests).await.unwrap();
    assert!(h.context_of(&a, &child.txid()).await.is_some(), "C is recorded in A");
    assert!(!h.manager.locked_spends.contains_key(&child.txid()), "P is mined");
}

/// Known gap: C, held by the mempool manager because it pays B, spends A's
/// abandoned change and a coin no loaded wallet holds; D beats it in a block
/// no wallet fetches. A wallet removed and restored from its recovery phrase
/// in the same session is passed C again once its scan completes, so the
/// change stays spent until C leaves the mempool (24 hours after it was
/// first seen) or Dash Wallet restarts.
#[tokio::test]
async fn a_wallet_restored_in_the_same_session_is_passed_a_beaten_held_spend_again() {
    let (mut h, ids) = Harness::new(&[MNEMONIC_A, MNEMONIC_B]).await;
    let (a, b) = (ids[0], ids[1]);
    h.filters_synced().await;
    h.manager.tick(&h.requests).await.unwrap();
    let elsewhere = address_of_unregistered(MNEMONIC_C).await;
    let (payment, _) = abandoned_payment_with_change(&mut h, &a, &elsewhere, 201..202, false).await;
    let to_b = next_unused_receive_address(&h.wallets, &b).await;
    let other = OutPoint::new(Txid::from_byte_array([0x5a; 32]), 0);
    let child = Transaction {
        version: 2,
        lock_time: 0,
        input: vec![
            TxIn {
                previous_output: OutPoint::new(payment.txid(), 1),
                ..Default::default()
            },
            TxIn {
                previous_output: other,
                ..Default::default()
            },
        ],
        output: vec![TxOut {
            value: CHANGE,
            script_pubkey: to_b.script_pubkey(),
        }],
        special_transaction_payload: None,
    };
    h.manager.process_instant_send(lock_for(&child)).await;
    assert!(h.announce(&child).await);
    h.manager.process_instant_send(lock_for(&payment)).await;
    h.manager.send_queued(&h.requests).await.unwrap();
    assert!(asked_for(&h.sent_messages(), payment.txid()));
    h.manager.handle_tx(payment.clone(), h.peer, &h.requests).await.unwrap();
    let change = OutPoint::new(payment.txid(), 1);
    let shows = |wallets: &WalletManager<ManagedWalletInfo>, id: &WalletId| {
        wallets.get_wallet_info(id).unwrap().utxos().iter().any(|utxo| utxo.outpoint == change)
    };
    assert!(!shows(&*h.wallets.read().await, &a), "C spends P's change");

    let funding = {
        let wallets = h.wallets.read().await;
        let account = wallets.get_wallet_info(&a).unwrap().first_bip44_managed_account().unwrap();
        let funding_txid = payment.input[0].previous_output.txid;
        account.transactions().get(&funding_txid).unwrap().transaction.clone()
    };
    h.wallets.write().await.remove_wallet(&a).unwrap();
    let a = register(&h.wallets, MNEMONIC_A).await;
    h.manager.tick(&h.requests).await.unwrap();
    {
        let mut wallets = h.wallets.write().await;
        wallets.check_transaction_in_all_wallets(&funding, in_block(960), true, true).await;
        wallets.check_transaction_in_all_wallets(&payment, in_block(972), true, true).await;
        assert!(shows(&wallets, &a), "the restored wallet's scan finds P and not C");
    }
    h.filters_synced().await;
    assert!(
        !shows(&*h.wallets.read().await, &a),
        "known gap: the held C is passed to the restored wallet again"
    );
}

/// How a locked spend that a ChainLocked conflict overturned arrives again.
#[derive(Clone, Copy, PartialEq)]
enum Again {
    Replayed,
    Relayed,
}

/// Where D's ChainLock overturns C.
#[derive(Clone, Copy, PartialEq)]
enum Overturned {
    /// D's block, which only B fetches, comes first, then its ChainLock.
    AtTheChainLock,
    /// D's ChainLock comes first, then its block, which both wallets fetch.
    InAFetchedBlock,
    /// D's ChainLock comes first, then its block, which only a third
    /// wallet fetches.
    InABlockNeitherWalletFetched,
}

/// Wallet A abandoned P, which is in fact live. C, locked, spends P's change
/// and wallet B's X and pays B; it is kept for A. D spends X, and D's
/// ChainLock overturns C as `overturned` says. C then arrives again with
/// its lock: replayed to A once P comes back, or relayed by a peer. Neither
/// wallet takes it: A offers P's change and B holds nothing of C. Only C's
/// own txid is refused: a later payment from P's change, and an unrelated
/// payment to A, are taken in.
#[test_case::test_case(Overturned::AtTheChainLock, Again::Replayed; "at the chain lock, replayed")]
#[test_case::test_case(Overturned::AtTheChainLock, Again::Relayed; "at the chain lock, relayed")]
#[test_case::test_case(Overturned::InAFetchedBlock, Again::Replayed; "in a fetched block, replayed")]
#[test_case::test_case(Overturned::InAFetchedBlock, Again::Relayed; "in a fetched block, relayed")]
#[test_case::test_case(Overturned::InABlockNeitherWalletFetched, Again::Replayed; "in a block neither wallet fetched, replayed")]
#[test_case::test_case(Overturned::InABlockNeitherWalletFetched, Again::Relayed; "in a block neither wallet fetched, relayed")]
#[tokio::test]
async fn a_locked_spend_a_chain_lock_overturned_is_refused_when_it_arrives_again(
    overturned: Overturned,
    again: Again,
) {
    let (mut h, ids) = Harness::new(&[MNEMONIC_A, MNEMONIC_B, MNEMONIC_D]).await;
    let (a, b, third) = (ids[0], ids[1], ids[2]);
    h.filters_synced().await;
    h.manager.tick(&h.requests).await.unwrap();
    let elsewhere = address_of_unregistered(MNEMONIC_C).await;
    let (payment, change_a) =
        abandoned_payment_with_change(&mut h, &a, &elsewhere, 181..182, false).await;
    let to_b = next_unused_receive_address(&h.wallets, &b).await;
    let holder_of_x = next_unused_receive_address(&h.wallets, &b).await;
    let funding_x = Transaction::dummy(&holder_of_x, 182..183, &[COIN]);
    let x = OutPoint::new(funding_x.txid(), 0);
    h.wallets
        .write()
        .await
        .check_transaction_in_all_wallets(&funding_x, in_block(960), true, true)
        .await;
    let change = OutPoint::new(payment.txid(), 1);
    let spending = |inputs: &[OutPoint], to: &Address, value: u64| Transaction {
        version: 2,
        lock_time: 0,
        input: inputs
            .iter()
            .map(|previous_output| TxIn {
                previous_output: *previous_output,
                ..Default::default()
            })
            .collect(),
        output: vec![TxOut {
            value,
            script_pubkey: to.script_pubkey(),
        }],
        special_transaction_payload: None,
    };
    let child = spending(&[change, x], &to_b, CHANGE);
    h.manager.process_instant_send(lock_for(&child)).await;
    assert!(h.announce(&child).await);
    assert!(h.manager.locked_spends.contains_key(&child.txid()), "kept for A");
    assert_eq!(h.total(&b).await, CHANGE, "C pays B");

    let winner = spending(&[x], &elsewhere, 500_000);
    let block = Block::dummy(971, vec![winner]);
    {
        let mut wallets = h.wallets.write().await;
        let fetched_for = match overturned {
            Overturned::AtTheChainLock => BTreeSet::from([b]),
            Overturned::InAFetchedBlock => BTreeSet::from([a, b]),
            Overturned::InABlockNeitherWalletFetched => BTreeSet::from([third]),
        };
        if overturned != Overturned::AtTheChainLock {
            wallets.apply_chain_lock(dashcore::ChainLock::dummy(971));
        }
        wallets.process_block_for_wallets(&block, block.block_hash(), 971, &fetched_for).await;
        if overturned == Overturned::AtTheChainLock {
            wallets.apply_chain_lock(dashcore::ChainLock::dummy(971));
        }
    }
    assert!(h.context_of(&b, &child.txid()).await.is_none(), "D's ChainLock overturns C in B");
    assert_eq!(h.total(&b).await, 0);

    match again {
        Again::Replayed => {
            h.manager.process_instant_send(lock_for(&payment)).await;
            h.manager.send_queued(&h.requests).await.unwrap();
            assert!(asked_for(&h.sent_messages(), payment.txid()));
            h.manager.handle_tx(payment.clone(), h.peer, &h.requests).await.unwrap();
            assert!(h.context_of(&a, &payment.txid()).await.is_some(), "P is back in A");
        }
        Again::Relayed => {
            h.manager.process_instant_send(lock_for(&child)).await;
            h.manager.handle_tx(child.clone(), h.peer, &h.requests).await.unwrap();
        }
    }
    assert!(h.context_of(&a, &child.txid()).await.is_none(), "A refuses C");
    assert!(h.context_of(&b, &child.txid()).await.is_none(), "B refuses C");
    assert_eq!(h.total(&b).await, 0, "C's output to B never existed");
    if again == Again::Replayed {
        assert_eq!(h.total(&a).await, CHANGE, "P's change is offered");
    }

    if again == Again::Replayed {
        let later = signed_spend_of(
            &h.wallets,
            &a,
            &change_a,
            change,
            &payment,
            vec![TxOut {
                value: CHANGE - 1_000,
                script_pubkey: elsewhere.script_pubkey(),
            }],
        )
        .await;
        let taken = h.wallets.write().await.process_mempool_transaction(&later, None).await;
        assert!(taken.is_relevant, "a later payment from P's change is taken in");
        assert!(h.context_of(&a, &later.txid()).await.is_some());
        assert_eq!(h.total(&a).await, 0, "it spends P's change");
    }

    let to_a = next_unused_receive_address(&h.wallets, &a).await;
    let unrelated = Transaction::dummy(&to_a, 183..184, &[50_000]);
    let taken = h.wallets.write().await.process_mempool_transaction(&unrelated, None).await;
    assert!(taken.is_relevant, "an unrelated payment to A is taken in");
}
