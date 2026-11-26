//! CJ1-W6 (D17): the CoinJoin server's mempool over the node's live pool and V: the collateral
//! test-accept and its fee cap, the final mix's priority delta, and the pool-then-tip coin view.

use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use dash_coinjoin::collateral::{MAX_RAW_TX_FEE, MempoolAccept, test_accept_if_sane_fee};
use dash_coinjoin::inouts::CoinView;
use dashd_coinjoin::env::AsyncMempool;

use super::mp1a9::{COIN, FEE, PAID, live, mature, pool, spend_to};
use super::*;
use crate::coinjoin::mempool::NodeMempool;

/// The server's mempool as `Node::start` builds it.
fn server_mempool(sut: &NodeSut) -> NodeMempool {
    sut.coinjoin_mempool().expect("loaded")
}

/// Polls `f` to completion on this thread.
fn wait<F: Future>(f: F) -> F::Output {
    struct Unpark(std::thread::Thread);
    impl std::task::Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut f = pin!(f);
    loop {
        if let Poll::Ready(out) = f.as_mut().poll(&mut Context::from_waker(&waker)) {
            return out;
        }
        std::thread::park();
    }
}

const SCRIPT: &[u8] = &[0x51; 40];

/// A collateral the pool takes gives its base fee, blocking or awaited; one over Core's
/// `maxTxFee` gives its fee and fails the sane-fee check; a spend of a missing coin gives none.
/// Nothing is added to the pool.
// Core: coinjoin/coinjoin.cpp:440-456, :444
#[test]
fn the_server_mempool_test_accepts_with_the_base_fee_under_the_fee_cap() {
    let dir = tempfile::tempdir().unwrap();
    let (mut sut, _) = live(dir.path(), &[]);
    imported(&mut sut);
    let blocks = mature(&sut, 102);
    let mempool = server_mempool(&sut);
    let paying = spend_to(&blocks[0], SCRIPT).raw().clone();
    assert_eq!(MempoolAccept::test_accept(&mempool, &paying), Some(FEE));
    assert_eq!(wait(AsyncMempool::test_accept(&mempool, paying.clone())), Some(Some(FEE)));
    assert!(test_accept_if_sane_fee(&mempool, &paying));

    let input = TxIn::new(OutPoint::new(blocks[1].txs()[0].txid(), 0), Vec::new().into(), 0);
    let excessive = tx(vec![input], &[(PAID - MAX_RAW_TX_FEE - 1, SCRIPT)]).raw().clone();
    assert_eq!(MempoolAccept::test_accept(&mempool, &excessive), Some(MAX_RAW_TX_FEE + 1));
    assert!(!test_accept_if_sane_fee(&mempool, &excessive));

    let missing = TxIn::new(OutPoint::new(TxHash::from([7; 32]), 0), Vec::new().into(), 0);
    let missing = tx(vec![missing], &[(COIN, SCRIPT)]).raw().clone();
    assert_eq!(MempoolAccept::test_accept(&mempool, &missing), None);
    assert_eq!(wait(AsyncMempool::test_accept(&mempool, missing)), Some(None));
    assert_eq!(pool(&sut).size(), 0);
}

/// A mix paying no fee is under the relay floor: refused until the server's +0.1 DASH
/// prioritisation, then admitted with its base fee still zero.
// Core: coinjoin/server.cpp:481-487; validation.cpp:597-598 (CheckFeeRate on the modified fee)
#[test]
fn a_zero_fee_mix_is_admitted_only_after_the_priority_delta() {
    let dir = tempfile::tempdir().unwrap();
    let (mut sut, _) = live(dir.path(), &[]);
    imported(&mut sut);
    let blocks = mature(&sut, 101);
    let mempool = server_mempool(&sut);
    let input = TxIn::new(OutPoint::new(blocks[0].txs()[0].txid(), 0), Vec::new().into(), 0);
    let mix = tx(vec![input], &[(PAID, SCRIPT)]).raw().clone();
    let id = dash_coinjoin::dstx::txid(&mix);
    assert_eq!(MempoolAccept::test_accept(&mempool, &mix), None);
    assert_eq!(wait(AsyncMempool::submit(&mempool, mix.clone())), Some(false));
    assert_eq!(wait(AsyncMempool::prioritise(&mempool, id, COIN / 10)), Some(()));
    assert_eq!(wait(AsyncMempool::test_accept(&mempool, mix.clone())), Some(Some(0)));
    assert_eq!(wait(AsyncMempool::submit(&mempool, mix)), Some(true));
    assert!(pool(&sut).read().exists(&id));
}

/// Core's `CCoinsViewMemPool`: a pool transaction's output is a mempool coin; a tip coin is read
/// from V's coins even when a pool transaction spends it; a missing coin is none.
// Core: txmempool.cpp:1954-1975; coinjoin/coinjoin.cpp:478-483
#[test]
fn the_server_coin_view_reads_the_pool_then_the_tip() {
    let dir = tempfile::tempdir().unwrap();
    let (mut sut, _) = live(dir.path(), &[]);
    imported(&mut sut);
    let blocks = mature(&sut, 102);
    let mempool = server_mempool(&sut);
    let pooled = spend_to(&blocks[0], SCRIPT);
    let handle = sut.steps.mempool().unwrap().handle();
    super::mp1a9::assert_valid(&handle.accept(pooled.clone(), false).expect("V replies"));
    let coinbase = |n: usize| OutPoint::new(blocks[n].txs()[0].txid(), 0);
    let outpoints = vec![
        OutPoint::new(pooled.txid(), 0),
        coinbase(1),
        coinbase(0),
        OutPoint::new(pooled.txid(), 1),
        OutPoint::new(TxHash::from([7; 32]), 0),
    ];
    let coins = wait(mempool.coins(outpoints.clone())).expect("V answers");
    let seen = |c: &Option<dash_coinjoin::inouts::ViewCoin>| {
        c.as_ref().map(|c| (c.out.value(), c.out.script_pubkey().as_bytes().to_vec(), c.in_mempool))
    };
    let seen: Vec<_> = coins.iter().map(seen).collect();
    assert_eq!(
        seen,
        vec![
            Some((PAID - FEE, SCRIPT.to_vec(), true)),
            Some((PAID, vec![0x51], false)),
            Some((PAID, vec![0x51], false)),
            None,
            None,
        ]
    );
    let blocking: Vec<_> = outpoints.iter().map(|o| seen_one(&mempool, o)).collect();
    assert_eq!(blocking, seen);
    assert!(!mempool.is_instantsend_locked(&pooled.txid()));
}

fn seen_one(mempool: &NodeMempool, outpoint: &OutPoint) -> Option<(i64, Vec<u8>, bool)> {
    let coin = mempool.coin(outpoint)?;
    Some((coin.out.value(), coin.out.script_pubkey().as_bytes().to_vec(), coin.in_mempool))
}
