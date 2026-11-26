//! MP1b-R on the node's live pool: ChainLock enforcement disconnects the block conflicting with
//! the lock and puts its transactions back through `AcceptToMemoryPool`.

use super::mp1a9::{COIN, FEE, PAID, assert_valid, live, mature, pool, spend_to};
use super::*;

/// `EnforceBlock` → `MarkConflictingBlock` (`validation.cpp:4089-4140` at v24.0.0) over the live
/// pool: the marked block's transactions are re-added (`MaybeUpdateMempoolForReorg(disconnectpool,
/// true)`, `:4137`), the coinbase excepted, and a re-added parent takes in its in-pool child
/// (`UpdateTransactionsFromBlock`, `txmempool.cpp:108-161`). When the locked block connects, its
/// spend of the coin a re-added transaction spent removes that one as a conflict
/// (`removeForBlock`, `txmempool.cpp:1321-1353`).
#[test]
fn a_chainlock_reorg_readds_the_marked_block_and_drops_the_conflicting_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let (mut sut, _) = live(dir.path(), &[SPORK_KEY]);
    loaded(&sut);
    imported(&mut sut);
    let sporks = Arc::clone(sut.sporks().expect("loaded"));
    sporks.update_spork(SporkId::SPORK_19_CHAINLOCKS_ENABLED, 0).expect("signed");
    let blocks = mature(&sut, 103);
    let p = sut.params.chain.clone();
    let base = *blocks.last().expect("blocks").header();

    // The kept transaction's first output is a lone `OP_TRUE`, which its child spends with a
    // clean stack.
    let coinbase = TxIn::new(OutPoint::new(blocks[0].txs()[0].txid(), 0), Vec::new().into(), !0);
    let kept = tx(vec![coinbase], &[(PAID - FEE - COIN, &[0x51]), (COIN, &[0x51; 40])]);
    let conflicted = spend_to(&blocks[2], &[0x51; 40]);
    let marked = mine(
        &p,
        &base,
        104,
        base.time() + 1,
        &[(PAID, &[0x51])],
        vec![kept.clone(), conflicted.clone()],
    );
    submit(&sut, &marked);
    let input = TxIn::new(OutPoint::new(kept.txid(), 0), Vec::new().into(), u32::MAX);
    let kept_child = tx(vec![input], &[(PAID - 2 * FEE - COIN, &[0x51; 40])]);
    let handle = sut.steps.mempool().unwrap().handle();
    assert_valid(&handle.accept(kept_child.clone(), false).expect("V replies"));
    let rival = spend_to(&blocks[2], &[0x51; 41]);
    let locked = mine(&p, &base, 104, base.time() + 2, &[(PAID, &[0x51])], vec![rival.clone()]);
    submit(&sut, &locked);
    let v = sut.chain().expect("V runs").clone();
    assert_eq!(v.tip_watch().get(), Some(marked.hash()), "the rival block is stored");

    assert_eq!(sut.cl_record(104, locked.hash(), [0; 96]), Some(ClRecorded::Accepted));
    v.chainlock_timer().expect("V serves");
    assert!(sut.settle().is_some());
    assert_eq!(v.tip_watch().get(), Some(locked.hash()), "the enforcement");

    let pool = pool(&sut);
    let state = pool.read();
    let held: Vec<_> = state.entries().keys().copied().collect();
    let mut expected = vec![kept.txid(), kept_child.txid()];
    expected.sort();
    assert_eq!(held, expected, "kept and its child; not the coinbase, the conflict or the rival");
    assert_eq!(state.get_iter(&kept_child.txid()).expect("held").ancestors().count, 2);
    assert_eq!(state.get_iter(&kept.txid()).expect("held").descendants().count, 2);
    state.check();
}
