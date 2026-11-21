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

    async fn answer_fetch(&mut self, tx: &Transaction) -> bool {
        self.manager.send_queued(&self.requests).await.unwrap();
        let asked = asked_for(&self.sent_messages(), tx.txid());
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
/// change in C. C is ignored unlocked until P comes back with its lock, which
/// has C fetched again; C is then credited, and its own lock or block settles
/// it.
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
    assert!(h.answer_fetch(&child).await, "P's lock has C fetched again");
    assert_eq!(h.total(&a).await, 0, "P's change is not offered once C is credited");

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

/// P was abandoned and C, spending P's change, ignored. A block mining P, with
/// no lock, has C fetched again; when P's lock came first, C is fetched once.
#[test_case::test_case(false; "mined without a lock")]
#[test_case::test_case(true; "locked then mined")]
#[tokio::test]
async fn a_spend_of_an_abandoned_payment_is_fetched_again_once_its_parent_is_mined(
    locked_first: bool,
) {
    let (mut h, ids) = Harness::new(&[MNEMONIC_A]).await;
    let a = ids[0];
    h.filters_synced().await;
    h.manager.tick(&h.requests).await.unwrap();
    let to_a = next_unused_receive_address(&h.wallets, &a).await;
    let change_a = next_unused_receive_address(&h.wallets, &a).await;
    let elsewhere = address_of_unregistered(MNEMONIC_B).await;
    let funding = Transaction::dummy(&to_a, 53..54, &[COIN]);
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

    if locked_first {
        h.manager.process_instant_send(lock_for(&payment)).await;
        h.manager.send_queued(&h.requests).await.unwrap();
        assert!(asked_for(&h.sent_messages(), payment.txid()));
        h.manager.handle_tx(payment.clone(), h.peer, &h.requests).await.unwrap();
        assert!(h.answer_fetch(&child).await, "P's lock has C fetched again");
    }

    let block = Block::dummy(970, vec![payment.clone()]);
    let wallets = BTreeSet::from([a]);
    let result = h
        .wallets
        .write()
        .await
        .process_block_for_wallets(&block, block.block_hash(), 970, &wallets)
        .await;
    let event = SyncEvent::BlockProcessed {
        block_hash: block.block_hash(),
        height: 970,
        wallets,
        new_scripts: Default::default(),
        confirmed_txids: vec![payment.txid()],
        released_dismissals: result.released_dismissals,
    };
    h.manager.handle_sync_event(&event, &h.requests).await.unwrap();
    if locked_first {
        assert!(!h.answer_fetch(&child).await, "C is fetched once");
    } else {
        assert!(h.answer_fetch(&child).await, "P's block has C fetched again");
    }
    assert!(h.context_of(&a, &child.txid()).await.is_some(), "C is credited");
    assert_eq!(h.total(&a).await, 0, "P's change is not offered once C is credited");
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

/// A spend of `parent`'s output `vout` paying `outputs`, with no signature.
fn unsigned_spend(parent: &Transaction, vout: u32, outputs: Vec<TxOut>) -> Transaction {
    Transaction {
        version: 2,
        lock_time: 0,
        input: vec![TxIn {
            previous_output: OutPoint::new(parent.txid(), vout),
            ..Default::default()
        }],
        output: outputs,
        special_transaction_payload: None,
    }
}

fn pays(value: u64, to: &Address) -> TxOut {
    TxOut {
        value,
        script_pubkey: to.script_pubkey(),
    }
}

/// Wallet `a` funded with [`COIN`] in a block, then its signed payment of
/// [`PAYMENT`] elsewhere, seen unlocked and abandoned by the user although it
/// is live.
async fn abandoned_payment(h: &Harness, a: &WalletId, seed: u8) -> Transaction {
    let to_a = next_unused_receive_address(&h.wallets, a).await;
    let elsewhere = address_of_unregistered(MNEMONIC_C).await;
    let funding = Transaction::dummy(&to_a, seed..seed + 1, &[COIN]);
    let mut wallets = h.wallets.write().await;
    wallets.check_transaction_in_all_wallets(&funding, in_block(960), true, true).await;
    drop(wallets);
    let payment = signed_spend(&h.wallets, a, &to_a, &funding, &elsewhere).await;
    let mut wallets = h.wallets.write().await;
    wallets.process_mempool_transaction(&payment, None).await;
    let info = wallets.get_wallet_info_mut(a).unwrap();
    info.abandon_transaction(payment.txid());
    info.update_balance();
    wallets.check_transaction_in_all_wallets(&funding, in_block(960), true, true).await;
    payment
}

/// More unlocked spends of an abandoned payment than the wallet keeps marks
/// for leave the abandon itself in place: the payment delivered again
/// unlocked still takes nothing.
#[tokio::test]
async fn a_flood_of_spends_of_an_abandoned_payment_leaves_the_abandon_in_place() {
    let (h, ids) = Harness::new(&[MNEMONIC_A]).await;
    let a = ids[0];
    let payment = abandoned_payment(&h, &a, 101).await;
    let elsewhere = address_of_unregistered(MNEMONIC_C).await;
    assert_eq!(h.total(&a).await, COIN);

    let mut wallets = h.wallets.write().await;
    for n in 0..5_000u64 {
        let spend = unsigned_spend(&payment, 0, vec![pays(1_000 + n, &elsewhere)]);
        assert!(!wallets.process_mempool_transaction(&spend, None).await.is_relevant);
    }
    let again = wallets.process_mempool_transaction(&payment, None).await;
    drop(wallets);
    assert!(!again.is_relevant, "the abandon outlasts the flood");
    assert_eq!(h.total(&a).await, COIN);
}

/// A spend of an abandoned, live payment, ignored unlocked, is credited when
/// delivered again unlocked once the payment is mined.
#[tokio::test]
async fn a_spend_of_an_abandoned_payment_delivered_again_after_its_block_is_credited() {
    let (h, ids) = Harness::new(&[MNEMONIC_A]).await;
    let a = ids[0];
    let payment = abandoned_payment(&h, &a, 102).await;
    let to_a = next_unused_receive_address(&h.wallets, &a).await;
    let child = unsigned_spend(&payment, 0, vec![pays(500_000, &to_a)]);

    let mut wallets = h.wallets.write().await;
    assert!(!wallets.process_mempool_transaction(&child, None).await.is_relevant);
    let block = Block::dummy(970, vec![payment.clone()]);
    wallets.process_block_for_wallets(&block, block.block_hash(), 970, &BTreeSet::from([a])).await;
    let credited = wallets.process_mempool_transaction(&child, None).await;
    drop(wallets);
    assert!(credited.is_relevant, "its parent is mined");
    assert_eq!(h.total(&a).await, 500_000);
}

/// A spend of a spend of an abandoned, live payment, ignored unlocked before
/// either is mined, is credited when delivered again unlocked once both are,
/// to the wallet that abandoned the payment or to another.
#[test_case::test_case(false; "to the same wallet")]
#[test_case::test_case(true; "to another wallet")]
#[tokio::test]
async fn a_grandchild_seen_before_its_parent_is_mined_is_credited_once_it_is(other: bool) {
    let (h, ids) = Harness::new(&[MNEMONIC_A, MNEMONIC_B]).await;
    let (a, b) = (ids[0], ids[1]);
    let payment = abandoned_payment(
        &h,
        &a,
        if other {
            103
        } else {
            104
        },
    )
    .await;
    let payee = if other {
        b
    } else {
        a
    };
    let to_payee = next_unused_receive_address(&h.wallets, &payee).await;
    let elsewhere = address_of_unregistered(MNEMONIC_C).await;
    let child = unsigned_spend(&payment, 0, vec![pays(980_000, &elsewhere)]);
    let grandchild = unsigned_spend(&child, 0, vec![pays(970_000, &to_payee)]);

    let mut wallets = h.wallets.write().await;
    assert!(!wallets.process_mempool_transaction(&child, None).await.is_relevant);
    assert!(!wallets.process_mempool_transaction(&grandchild, None).await.is_relevant);
    let block = Block::dummy(970, vec![payment.clone(), child.clone()]);
    wallets.process_block_for_wallets(&block, block.block_hash(), 970, &BTreeSet::from([a])).await;
    drop(wallets);
    let before = h.total(&payee).await;
    let mut wallets = h.wallets.write().await;
    let credited = wallets.process_mempool_transaction(&grandchild, None).await;
    drop(wallets);
    assert!(credited.is_relevant, "its parent is mined");
    assert_eq!(h.total(&payee).await, before + 970_000);
}

/// The user abandoned P, which is in fact live; another device spent P's
/// change in C. C's lock comes first and has C fetched while P's change is
/// still gone, so C matches no wallet; once P is back, by its lock or its
/// block, P's change is not offered.
#[test_case::test_case(true; "back by its lock")]
#[test_case::test_case(false; "back by its block")]
#[tokio::test]
async fn a_locked_spend_fetched_before_its_parent_is_back_keeps_the_change_spent(by_lock: bool) {
    let (mut h, ids) = Harness::new(&[MNEMONIC_A]).await;
    let a = ids[0];
    h.filters_synced().await;
    h.manager.tick(&h.requests).await.unwrap();
    let to_a = next_unused_receive_address(&h.wallets, &a).await;
    let change_a = next_unused_receive_address(&h.wallets, &a).await;
    let elsewhere = address_of_unregistered(MNEMONIC_B).await;
    let funding = Transaction::dummy(&to_a, 105..106, &[COIN]);
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
        vec![pays(300_000, &elsewhere), pays(change, &change_a)],
    )
    .await;
    let child = signed_spend_of(
        &h.wallets,
        &a,
        &change_a,
        OutPoint::new(payment.txid(), 1),
        &payment,
        vec![pays(change - 10_000, &elsewhere)],
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

    h.manager.process_instant_send(lock_for(&child)).await;
    h.manager.send_queued(&h.requests).await.unwrap();
    assert!(asked_for(&h.sent_messages(), child.txid()), "C's lock has it fetched");
    h.manager.handle_tx(child.clone(), h.peer, &h.requests).await.unwrap();
    assert!(!h.manager.transactions.contains_key(&child.txid()), "C matches no wallet yet");

    if by_lock {
        h.manager.process_instant_send(lock_for(&payment)).await;
        h.manager.send_queued(&h.requests).await.unwrap();
        assert!(asked_for(&h.sent_messages(), payment.txid()));
        h.manager.handle_tx(payment.clone(), h.peer, &h.requests).await.unwrap();
    } else {
        let block = Block::dummy(970, vec![payment.clone()]);
        let wallets = BTreeSet::from([a]);
        let result = h
            .wallets
            .write()
            .await
            .process_block_for_wallets(&block, block.block_hash(), 970, &wallets)
            .await;
        let event = SyncEvent::BlockProcessed {
            block_hash: block.block_hash(),
            height: 970,
            wallets,
            new_scripts: Default::default(),
            confirmed_txids: vec![payment.txid()],
            released_dismissals: result.released_dismissals,
        };
        h.manager.handle_sync_event(&event, &h.requests).await.unwrap();
    }
    assert_eq!(h.total(&a).await, 0, "C, locked, spends P's change");
}

/// The user abandoned P, which is in fact live; C spends P's change and pays
/// the wallet, so it arrives dismissed with P. P's verified lock settles P,
/// and C delivered again unlocked is credited. A lock not yet verified
/// settles nothing.
#[test_case::test_case(true; "by a verified lock")]
#[test_case::test_case(false; "not by an unverified lock")]
#[tokio::test]
async fn a_spend_of_an_abandoned_payment_is_credited_once_its_lock_settles_it(validated: bool) {
    let (mut h, ids) = Harness::new(&[MNEMONIC_A]).await;
    let a = ids[0];
    h.filters_synced().await;
    h.manager.tick(&h.requests).await.unwrap();
    let to_a = next_unused_receive_address(&h.wallets, &a).await;
    let change_a = next_unused_receive_address(&h.wallets, &a).await;
    let back_to_a = next_unused_receive_address(&h.wallets, &a).await;
    let elsewhere = address_of_unregistered(MNEMONIC_B).await;
    let funding = Transaction::dummy(&to_a, 107..108, &[COIN]);
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
        vec![pays(300_000, &elsewhere), pays(change, &change_a)],
    )
    .await;
    let child = signed_spend_of(
        &h.wallets,
        &a,
        &change_a,
        OutPoint::new(payment.txid(), 1),
        &payment,
        vec![pays(change - 10_000, &back_to_a)],
    )
    .await;
    {
        let mut wallets = h.wallets.write().await;
        wallets.process_mempool_transaction(&payment, None).await;
        let info = wallets.get_wallet_info_mut(&a).unwrap();
        info.abandon_transaction(payment.txid());
        info.update_balance();
        wallets.check_transaction_in_all_wallets(&funding, in_block(960), true, true).await;
        assert!(!wallets.process_mempool_transaction(&child, None).await.is_relevant);
    }

    let event = SyncEvent::InstantLockReceived {
        instant_lock: lock_for(&payment),
        validated,
    };
    h.manager.handle_sync_event(&event, &h.requests).await.unwrap();
    h.manager.handle_tx(payment.clone(), h.peer, &h.requests).await.unwrap();
    let credited = h.wallets.write().await.process_mempool_transaction(&child, None).await;

    assert_eq!(credited.is_relevant, validated, "P is settled");
    let expected = if validated {
        change - 10_000
    } else {
        COIN
    };
    assert_eq!(h.total(&a).await, expected);
}
