//! A locked payment beaten by a spend in a plain block, swept once a
//! ChainLock makes that spend final; and a spent-output claimant beaten by a
//! final spend.

use dashcore::ephemerealdata::chain_lock::ChainLock;
use dashcore::ephemerealdata::instant_lock::InstantLock;
use dashcore::hashes::Hash;
use dashcore::{Address, BlockHash, OutPoint, Transaction, TxIn, Txid};

use crate::managed_account::managed_account_trait::ManagedAccountTrait;
use crate::test_utils::TestWalletContext;
use crate::transaction_checking::{BlockInfo, TransactionContext};
use crate::wallet::managed_wallet_info::dismissed::MAX_OBSERVED_SPENDERS;
use crate::wallet::managed_wallet_info::helpers::WalletConflictSweep;
use crate::wallet::managed_wallet_info::wallet_info_interface::{
    ApplyChainLockOutcome, ChainLockSweep, WalletInfoInterface,
};
use crate::Utxo;

fn block(height: u32) -> BlockInfo {
    BlockInfo::new(height, BlockHash::from_byte_array([height as u8; 32]), 1_700_000_000)
}

fn in_block(height: u32) -> TransactionContext {
    TransactionContext::InBlock(block(height))
}

fn locked() -> TransactionContext {
    TransactionContext::InstantSend(InstantLock::default())
}

/// A transaction spending `inputs`, paying `to` 140 000 when given.
fn spending(inputs: &[OutPoint], to: Option<&Address>) -> Transaction {
    Transaction {
        version: 1,
        lock_time: 0,
        input: inputs
            .iter()
            .map(|outpoint| TxIn {
                previous_output: *outpoint,
                ..Default::default()
            })
            .collect(),
        output: to.map_or_else(Vec::new, |to| Transaction::dummy(to, 30..31, &[140_000]).output),
        special_transaction_payload: None,
    }
}

fn chain_lock(ctx: &mut TestWalletContext, height: u32) -> ApplyChainLockOutcome {
    ctx.managed_wallet.update_synced_height(height);
    ctx.managed_wallet.apply_chain_lock(ChainLock::dummy(height))
}

fn claim_on(ctx: &TestWalletContext, outpoint: &OutPoint) -> Option<Option<Txid>> {
    ctx.managed_wallet.spent_claims.get(outpoint).copied()
}

fn holds(ctx: &TestWalletContext, outpoint: &OutPoint) -> bool {
    ctx.bip44_account().utxos.contains_key(outpoint)
}

/// Where the payment is: a record of this session, or after a restart only
/// claims on its coins and its stored change.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Shape {
    Recorded,
    Claimed,
}

/// The payment `P` of coins A and B, paying its change to the wallet.
struct Payment {
    ctx: TestWalletContext,
    a: OutPoint,
    b: OutPoint,
    payment: Transaction,
    change: OutPoint,
}

/// `P`, InstantSend-locked when `lock`, in `shape`; its coins are funded in
/// block 90.
async fn payment(shape: Shape, lock: bool) -> Payment {
    let mut ctx = TestWalletContext::new_random();
    let funding = Transaction::dummy(&ctx.receive_address, 10..11, &[150_000, 90_000]);
    let (a, b) = (OutPoint::new(funding.txid(), 0), OutPoint::new(funding.txid(), 1));
    let payment = spending(&[a, b], Some(&ctx.receive_address));
    let change = OutPoint::new(payment.txid(), 0);
    match shape {
        Shape::Recorded => {
            ctx.check_transaction(&funding, in_block(90)).await;
            let context = if lock {
                locked()
            } else {
                TransactionContext::Mempool
            };
            ctx.check_transaction(&payment, context).await;
        }
        Shape::Claimed => {
            let utxo =
                Utxo::new(change, payment.output[0].clone(), ctx.receive_address.clone(), 0, false);
            ctx.managed_wallet.restore_utxos(&ctx.wallet, [utxo]).expect("restores");
            if lock {
                ctx.managed_wallet
                    .mark_instant_send_utxos(&payment.txid(), &InstantLock::default());
            }
            let p = Some(payment.txid());
            ctx.managed_wallet.restore_spent_outpoints(&[(a, p), (b, p)]);
        }
    }
    assert!(holds(&ctx, &change), "P's change is held");
    Payment {
        ctx,
        a,
        b,
        payment,
        change,
    }
}

fn swept(
    winner: &Transaction,
    height: u32,
    txids: Vec<Txid>,
    released: Vec<OutPoint>,
) -> ChainLockSweep {
    ChainLockSweep {
        winner: winner.txid(),
        winner_block: block(height),
        sweep: WalletConflictSweep {
            txids,
            released_outpoints: released,
        },
    }
}

fn assert_gone(p: &Payment, shape: Shape) {
    assert!(!holds(&p.ctx, &p.change), "P's change is gone");
    assert!(!p.ctx.bip44_account().has_transaction(&p.payment.txid()));
    assert!(p.ctx.managed_wallet.dismissed.contains(&p.payment.txid()));
    if shape == Shape::Claimed {
        assert_eq!(claim_on(&p.ctx, &p.a), None, "A's claim is released");
        assert_eq!(claim_on(&p.ctx, &p.b), Some(None), "B stays guarded");
    }
}

#[test_case::test_case(Shape::Recorded; "recorded")]
#[test_case::test_case(Shape::Claimed; "claimed after a restart")]
#[tokio::test]
async fn a_chainlock_sweeps_a_locked_payment_a_spend_in_a_plain_block_beat(shape: Shape) {
    let mut p = payment(shape, true).await;
    let winner = spending(&[p.b], None);

    let result = p.ctx.check_transaction(&winner, in_block(100)).await;
    assert!(result.swept_transactions.is_empty(), "a plain block does not overrule the lock");
    assert!(holds(&p.ctx, &p.change));
    assert!(chain_lock(&mut p.ctx, 99).sweeps.is_empty(), "below the spend's block");
    assert!(holds(&p.ctx, &p.change));

    let outcome = chain_lock(&mut p.ctx, 100);
    assert_eq!(outcome.sweeps, vec![swept(&winner, 100, vec![p.payment.txid()], vec![p.a])]);
    assert_gone(&p, shape);
    assert!(chain_lock(&mut p.ctx, 101).sweeps.is_empty(), "swept once");
}

#[test_case::test_case(Shape::Recorded; "recorded")]
#[test_case::test_case(Shape::Claimed; "claimed after a restart")]
#[tokio::test]
async fn a_chainlocked_spend_sweeps_a_locked_payment_on_arrival(shape: Shape) {
    let mut p = payment(shape, true).await;
    let winner = spending(&[p.b], None);

    let context = TransactionContext::InChainLockedBlock(block(100));
    let result = p.ctx.check_transaction(&winner, context).await;
    assert_eq!(result.swept_transactions, vec![p.payment.txid()]);
    assert_eq!(result.released_outpoints, vec![p.a]);
    assert_gone(&p, shape);
}

/// As a recorded payment not locked is, so is one known by its claims.
#[tokio::test]
async fn a_mined_spend_sweeps_a_claimant_not_locked() {
    let mut p = payment(Shape::Claimed, false).await;
    let winner = spending(&[p.b], None);

    let result = p.ctx.check_transaction(&winner, in_block(100)).await;
    assert_eq!(result.swept_transactions, vec![p.payment.txid()]);
    assert_eq!(result.released_outpoints, vec![p.a]);
    assert_gone(&p, Shape::Claimed);
}

/// What spends the claimant's change goes with it.
#[tokio::test]
async fn a_swept_claimant_takes_what_spends_its_change() {
    let mut p = payment(Shape::Claimed, true).await;
    let child = spending(&[p.change], Some(&p.ctx.receive_address));
    let child = p.ctx.signed(child, &[&p.payment]).await;
    p.ctx.check_transaction(&child, TransactionContext::Mempool).await;
    assert!(p.ctx.bip44_account().has_transaction(&child.txid()));

    let winner = spending(&[p.b], None);
    p.ctx.check_transaction(&winner, in_block(100)).await;
    let outcome = chain_lock(&mut p.ctx, 100);

    let mut txids = vec![p.payment.txid(), child.txid()];
    txids.sort_unstable();
    assert_eq!(outcome.sweeps, vec![swept(&winner, 100, txids, vec![p.a])]);
    assert!(!p.ctx.bip44_account().has_transaction(&child.txid()));
    assert!(!holds(&p.ctx, &OutPoint::new(child.txid(), 0)));
    assert_gone(&p, Shape::Claimed);
}

/// A locked child of the swept payment goes with it: the ChainLock overrules
/// its lock as it does its parent's.
#[test_case::test_case(Shape::Recorded; "recorded")]
#[test_case::test_case(Shape::Claimed; "claimed after a restart")]
#[tokio::test]
async fn a_chainlock_sweeps_a_locked_child_with_its_payment(shape: Shape) {
    let mut p = payment(shape, true).await;
    let child = spending(&[p.change], Some(&p.ctx.receive_address));
    let child = p.ctx.signed(child, &[&p.payment]).await;
    p.ctx.check_transaction(&child, locked()).await;
    assert!(p.ctx.bip44_account().has_transaction(&child.txid()));

    let winner = spending(&[p.b], None);
    p.ctx.check_transaction(&winner, in_block(100)).await;
    let outcome = chain_lock(&mut p.ctx, 100);

    let mut txids = vec![p.payment.txid(), child.txid()];
    txids.sort_unstable();
    assert_eq!(outcome.sweeps, vec![swept(&winner, 100, txids, vec![p.a])]);
    assert!(!p.ctx.bip44_account().has_transaction(&child.txid()));
    assert!(!holds(&p.ctx, &OutPoint::new(child.txid(), 0)));
    assert_gone(&p, shape);
}

#[test_case::test_case(Shape::Recorded; "recorded")]
#[test_case::test_case(Shape::Claimed; "claimed after a restart")]
#[tokio::test]
async fn a_payment_mined_itself_is_never_swept(shape: Shape) {
    let mut p = payment(shape, true).await;

    let result = p.ctx.check_transaction(&p.payment, in_block(100)).await;
    assert!(result.swept_transactions.is_empty());
    let outcome = chain_lock(&mut p.ctx, 100);

    assert!(outcome.sweeps.is_empty());
    assert!(holds(&p.ctx, &p.change), "its change stays");
    assert!(!p.ctx.managed_wallet.dismissed.contains(&p.payment.txid()));
    if shape == Shape::Claimed {
        assert_eq!(claim_on(&p.ctx, &p.a), Some(None));
        assert_eq!(claim_on(&p.ctx, &p.b), Some(None));
    }
}

/// A claim whose claimant was disputed names no transaction to sweep.
#[tokio::test]
async fn a_disputed_claim_only_guards() {
    for on_arrival in [false, true] {
        let mut p = payment(Shape::Claimed, true).await;
        p.ctx.managed_wallet.restore_spent_outpoints(&[(p.b, Some(Txid::from([0x99; 32])))]);
        assert_eq!(claim_on(&p.ctx, &p.b), Some(None), "disputed");
        let winner = spending(&[p.b], None);

        let context = if on_arrival {
            TransactionContext::InChainLockedBlock(block(100))
        } else {
            in_block(100)
        };
        let result = p.ctx.check_transaction(&winner, context).await;
        let outcome = chain_lock(&mut p.ctx, 100);

        assert!(result.swept_transactions.is_empty());
        assert!(outcome.sweeps.is_empty());
        assert!(holds(&p.ctx, &p.change));
        assert_eq!(claim_on(&p.ctx, &p.a), Some(Some(p.payment.txid())));
        assert_eq!(claim_on(&p.ctx, &p.b), Some(None));
    }
}

/// The spends kept for a ChainLock are bounded; one that goes first is
/// forgotten, and what it beat stays.
#[tokio::test]
async fn a_spend_forgotten_at_capacity_sweeps_nothing() {
    let mut p = payment(Shape::Recorded, true).await;
    let winner = spending(&[p.b], None);
    p.ctx.check_transaction(&winner, in_block(100)).await;

    for n in 0..MAX_OBSERVED_SPENDERS as u32 {
        let mut bytes = [0xee; 32];
        bytes[..4].copy_from_slice(&n.to_le_bytes());
        let spent = OutPoint::new(Txid::from_byte_array(bytes), 0);
        p.ctx.managed_wallet.observed_spenders.insert(
            spent,
            Txid::from([0x77; 32]),
            block(100),
            None,
        );
    }
    let outcome = chain_lock(&mut p.ctx, 100);

    assert!(outcome.sweeps.is_empty());
    assert!(holds(&p.ctx, &p.change), "P stays");
    assert!(p.ctx.bip44_account().has_transaction(&p.payment.txid()));
}

/// Only spends that beat a locked transaction are kept for a ChainLock, so
/// a block full of other spends crowds out none of them.
#[tokio::test]
async fn other_spends_in_blocks_keep_none_from_its_chain_lock() {
    let mut p = payment(Shape::Recorded, true).await;
    let winner = spending(&[p.b], None);
    p.ctx.check_transaction(&winner, in_block(100)).await;

    let others: Vec<OutPoint> = (0..=MAX_OBSERVED_SPENDERS as u32)
        .map(|n| {
            let mut bytes = [0xdd; 32];
            bytes[..4].copy_from_slice(&n.to_le_bytes());
            OutPoint::new(Txid::from_byte_array(bytes), 0)
        })
        .collect();
    p.ctx.check_transaction(&spending(&others, None), in_block(101)).await;
    let outcome = chain_lock(&mut p.ctx, 101);

    assert_eq!(outcome.sweeps, vec![swept(&winner, 100, vec![p.payment.txid()], vec![p.a])]);
    assert_gone(&p, Shape::Recorded);
}

/// A fork dropping the spend's block drops the spend with it.
#[tokio::test]
async fn a_fork_dropping_the_spends_block_sweeps_nothing() {
    let mut p = payment(Shape::Recorded, true).await;
    let winner = spending(&[p.b], None);
    p.ctx.check_transaction(&winner, in_block(100)).await;

    p.ctx.managed_wallet.truncate_above(99).expect("no ChainLock yet");
    let outcome = chain_lock(&mut p.ctx, 100);

    assert!(outcome.sweeps.is_empty());
    assert!(holds(&p.ctx, &p.change));
}

/// A coin of the payment that another spend took in a block is not released
/// when the payment is swept: the store would hand it back unspent.
#[tokio::test]
async fn a_coin_another_block_spent_is_not_released_with_the_payment() {
    for on_arrival in [false, true] {
        let mut p = payment(Shape::Recorded, true).await;
        let on_a = spending(&[p.a], None);
        p.ctx.check_transaction(&on_a, in_block(100)).await;
        let winner = spending(&[p.b], None);

        let sweeps = if on_arrival {
            let context = TransactionContext::InChainLockedBlock(block(101));
            let result = p.ctx.check_transaction(&winner, context).await;
            vec![(result.swept_transactions, result.released_outpoints)]
        } else {
            p.ctx.check_transaction(&winner, in_block(101)).await;
            let outcome = chain_lock(&mut p.ctx, 101);
            outcome
                .sweeps
                .into_iter()
                .map(|s| (s.sweep.txids, s.sweep.released_outpoints))
                .collect()
        };

        assert_eq!(sweeps.len(), 1, "{sweeps:?}");
        assert_eq!(sweeps[0], (vec![p.payment.txid()], Vec::new()), "nothing is released");
        assert_gone(&p, Shape::Recorded);
    }
}
