//! [`NodeBlockIndex`]'s own rules: Core's load rules against what V built live (§7.3), C1 under
//! re-keying, `CheckBlockIndex`'s node half catching a broken table, and the prune's clears.

use std::cmp::Reverse;
use std::sync::Arc;

use dash_chain::{FlatFilePos, HeaderChain, Status};
use dash_clock::{Clock, SystemClock, format_iso8601_date_time};
use dash_consensus::BlockId;
use dash_params::{ChainParamsBuilder, ChainTxData, NetworkKind, Release};

use super::support::{
    Mined, Rig, RigConfig, SPACING, branch, cb_script, child, coinbase, exact_tables,
    install_accept_log, levels_here, lines_with, mine, regtest, sorted,
};
use crate::chain::check::check_block_index;
use crate::chain::{
    CacheUsage, ChainCommand, Fatal, FlushHost, FlushSite, FlushStateMode, NodeBlockIndex,
    SiteDecision, guess_verification_progress, load_chain_tip,
};

/// After restart Core rebuilds `nChainTx`, `m_blocks_unlinked`, the candidates and
/// `m_best_invalid` from the stored entries (`blockstorage.cpp:305-320`,
/// `validation.cpp:5351-5361`) and prunes the candidates at `LoadChainTip`. That rebuild equals
/// the live tables up to the sequence ids, which restart at 0.
#[test]
fn load_rules_rebuild_the_live_tables() {
    let mut rig = Rig::new();
    let main = branch(&rig.p, &rig.genesis, 4, 0);
    let fork = branch(&rig.p, &main[0], 2, 7);
    rig.headers(&main);
    rig.headers(&fork);
    // main[1] never arrives: main[2..] stay unlinked; the fork links through main[0].
    for m in [&main[3], &main[2], &main[0], &fork[1], &fork[0]] {
        rig.accept(m);
    }
    // A block that fails ContextualCheckBlock (BIP34): `m_best_invalid`.
    let p = &fork[1];
    let bad = Mined {
        block: mine(&rig.p, p.hash(), p.time() + SPACING, vec![coinbase(&cb_script(1, 0))]),
        height: p.height + 1,
    };
    assert!(!rig.submit(&bad).accepted);
    let bad_id = rig.id(&bad);
    assert_eq!(rig.idx(|ix| ix.best_invalid()), None, "AcceptBlock does not set it (N4 does)");

    let exit = rig.stop();
    assert!(exit.fatal.is_none());
    let (chain, live) = (exit.chain, exit.idx);
    let r = chain.reader();
    let tip = chain.private_tip().expect("genesis").0.id;

    let mut loaded = NodeBlockIndex::load(&chain);
    // LoadChainTip's prune, without re-initialising the (already active) tip.
    loaded.prune_block_index_candidates(&r, tip);
    for id in chain.ids() {
        assert_eq!(loaded.n_chain_tx(id), live.n_chain_tx(id), "nChainTx of {}", id.0);
        assert_eq!(loaded.sequence_id(id), 0);
    }
    assert_eq!(sorted(loaded.unlinked()), sorted(live.unlinked()));
    assert_eq!(sorted(loaded.candidates()), sorted(live.candidates()));
    assert_eq!(loaded.best_invalid(), Some(bad_id), "the load rule finds the failed entry");
    check_block_index(&chain, &loaded, false).expect("the loaded tables are consistent");
    // The live tables were checked after every step; once more at rest.
    check_block_index(&chain, &live, false).expect("the live tables are consistent");
}

/// `load_chain_tip` on the loaded tables leaves only candidates no worse than the tip.
#[test]
fn load_chain_tip_prunes_worse_candidates() {
    let mut rig = Rig::new();
    let main = branch(&rig.p, &rig.genesis, 3, 0);
    for m in &main {
        rig.accept(m);
    }
    let [_, tip, top] = rig.ids(&main)[..] else {
        unreachable!()
    };
    let mut exit = rig.stop();
    let mut loaded = NodeBlockIndex::load(&exit.chain);
    assert_eq!(loaded.candidates().count(), exit.chain.ids().count());
    load_chain_tip(&mut exit.chain, &mut loaded, tip).expect("LoadChainTip");
    assert_eq!(loaded.candidates().collect::<Vec<_>>(), [tip, top]);
    check_block_index(&exit.chain, &loaded, false).expect("consistent");
}

/// `load_chain_tip` logs Core's line (`validation.cpp:5012-5016`; feature_init.py:88's
/// "Loaded best chain:", BL-272) at info, its progress from the system time: with the
/// parameters' estimate dated now, 3 of 1,000 transactions is 0.003000, where the rig's mock time
/// (a day after regtest's 2014 genesis) would make the estimate negative.
#[test]
fn load_chain_tip_logs_core_s_line() {
    install_accept_log();
    let mut p = (*regtest()).clone();
    p.chain_tx_data = ChainTxData {
        timestamp: SystemClock.node_now().secs(),
        tx_count: 1000,
        tx_rate: 1e-3,
    };
    let mut rig = Rig::with(RigConfig {
        params: Some(Arc::new(p)),
        ..RigConfig::default()
    });
    let main = branch(&rig.p, &rig.genesis, 3, 0);
    for m in &main {
        rig.accept(m);
    }
    // Below V's tip: at its own tip, `LoadChainTip` returns at once and logs nothing.
    let tip = rig.id(&main[1]);
    let mut exit = rig.stop();
    let mut loaded = NodeBlockIndex::load(&exit.chain);
    assert_eq!(loaded.n_chain_tx(tip), 3);
    load_chain_tip(&mut exit.chain, &mut loaded, tip).expect("LoadChainTip");
    let date = format_iso8601_date_time(i64::from(main[1].time()));
    let line = format!(
        "Loaded best chain: hashBestChain={} height=2 date={date} progress=0.003000",
        main[1].hash()
    );
    assert_eq!(levels_here(&line), [tracing::Level::INFO], "{:?}", lines_with("Loaded best chain"));
}

/// `GuessVerificationProgress` on mainnet's `ChainTxData` (`chainparams.cpp:355-360`), both
/// branches and the clamp, against C's `%f` of the same doubles.
#[test]
fn verification_progress_is_core_s() {
    let p = ChainParamsBuilder::new(NetworkKind::Mainnet, Release::V24_0).build().expect("mainnet");
    let data = &p.chain_tx_data;
    let (t0, n0) = (data.timestamp, u32::try_from(data.tx_count).expect("fits"));
    let at = |chain_tx, time, now| {
        format!("{:.6}", guess_verification_progress(data, chain_tx, time, now))
    };
    // Up to the parameters' count: against their estimate at `now`.
    assert_eq!(at(n0 / 2, 0, t0), "0.500000");
    assert_eq!(at(n0 / 2, 0, t0 + 30 * 86_400), "0.497101");
    assert_eq!(at(n0, 0, t0), "1.000000");
    // Past it: against the block's own count plus the rate since its time.
    assert_eq!(at(n0 + 1000, 1_781_400_000, 1_781_400_000), "1.000000");
    assert_eq!(at(n0 + 1000, 1_781_400_000, 1_781_410_000), "0.999978");
    // A clock behind the estimate's date or the block's time: clamped at 1 (1.000194, 1.000001).
    assert_eq!(at(n0, 0, t0 - 86_400), "1.000000");
    assert_eq!(at(n0 + 1000, 1_781_400_000, 1_781_399_400), "1.000000");
}

/// A flush side that decides nothing and reports fixed caches.
struct Caches(CacheUsage);

impl FlushSite for Caches {
    fn at_site(&mut self, _: FlushStateMode, _: &mut FlushHost<'_>) -> Result<SiteDecision, Fatal> {
        Ok(SiteDecision::nothing())
    }

    fn after_step(&mut self, _: SiteDecision, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        Ok(())
    }

    fn service(&mut self, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        Ok(())
    }

    fn cache_usage(&self) -> CacheUsage {
        self.0
    }
}

/// `UpdateTip` logs Core's line (`validation.cpp:3178-3187`, BL-281): regtest's blocks add 2
/// work each, so the tip at height 3 has 8 and `log2_work=3.000000`; its 4 of 1,000 transactions
/// dated now is 0.004000; the caches are the flush side's, in MiB to one place. Without a flush
/// side they are 0.
#[test]
fn update_tip_logs_core_s_line() {
    install_accept_log();
    let mut p = (*regtest()).clone();
    p.chain_tx_data = ChainTxData {
        timestamp: SystemClock.node_now().secs(),
        tx_count: 1000,
        tx_rate: 1e-3,
    };
    let p = Arc::new(p);
    let usage = CacheUsage {
        coins_bytes: 7 << 19,
        coins: 17,
        evo_bytes: 3 << 19,
    };
    for (salt, flush, cache) in [
        (281, Some(usage), "cache=3.5MiB(17utxo) evodb_cache=1.5MiB"),
        (282, None, "cache=0.0MiB(0utxo) evodb_cache=0.0MiB"),
    ] {
        let rig = Rig::with(RigConfig {
            params: Some(p.clone()),
            flush: flush.map(|u| Box::new(Caches(u)) as Box<dyn FlushSite>),
            ..RigConfig::default()
        });
        let main = branch(&rig.p, &rig.genesis, 3, salt);
        for m in &main {
            rig.accept(m);
        }
        let tip = &main[2];
        let date = format_iso8601_date_time(i64::from(tip.time()));
        let line = format!(
            "UpdateTip: new best={} height=3 version=0x{:08x} log2_work=3.000000 tx=4 \
             date='{date}' progress=0.004000 {cache}",
            tip.hash(),
            tip.header().version(),
        );
        assert_eq!(lines_with(&format!("UpdateTip: new best={} ", tip.hash())), [line]);
    }
}

/// C1: a re-keyed member keeps exactly one key; inserting a member again replaces its key (D2).
#[test]
fn candidates_keep_one_key_per_member() {
    let mut rig = Rig::new();
    let a = branch(&rig.p, &rig.genesis, 2, 0);
    for m in &a {
        rig.accept(m);
    }
    let top = rig.id(&a[1]);
    let exit = rig.stop();
    let r = exit.chain.reader();
    let mut ix = exit.idx;
    let n = ix.candidates().count();
    ix.set_sequence_id(&r, top, -1);
    assert_eq!(ix.candidates().count(), n);
    assert_eq!(ix.key_of(&r, top).seq, Reverse(-1));
    exact_tables(&exit.chain, &ix, false).expect("the set holds the member's live key");
    assert_eq!(ix.best_candidate(), Some(top));
    ix.candidates_insert(&r, top);
    assert_eq!(ix.candidates().count(), n);
    ix.check_c1().expect("C1");
    ix.candidates_erase(top);
    assert!(!ix.is_candidate(top));
    ix.check_c1().expect("C1");
}

/// Sol cp1 M5: re-keying a member moves it in the order (`blockstorage.cpp:32-49`): an
/// equal-work sibling given a lower sequence id than the tip's becomes the best candidate.
#[test]
fn a_re_keyed_candidate_moves_in_the_order() {
    let mut rig = Rig::new();
    let (a, s) = (child(&rig.p, &rig.genesis, 0), child(&rig.p, &rig.genesis, 1));
    rig.accept(&a);
    rig.accept(&s);
    let (a, s) = (rig.id(&a), rig.id(&s));
    let exit = rig.stop();
    let r = exit.chain.reader();
    let mut ix = exit.idx;
    assert_eq!(ix.candidates().collect::<Vec<_>>(), [a], "S ties A and came later");
    ix.candidates_insert(&r, s);
    assert_eq!(ix.best_candidate(), Some(a));
    ix.set_sequence_id(&r, s, -1);
    assert_eq!(ix.best_candidate(), Some(s), "-1 sorts before A's sequence id");
    exact_tables(&exit.chain, &ix, false).expect("the re-keyed member's key is live");
}

/// Sol cp1 M3: `TryAddBlockIndexCandidate` never admits a ChainLock-conflicting block
/// (`validation.cpp:4237-4243`). B2 waits unlinked under B1 with more work than the tip A1;
/// B1's data links it through Core's queue (`:4278-4298`), which admits it unless EnforceBlock
/// marked the B branch conflicting (`:4115-4121`). A1's ABC would drop a conflicting candidate
/// again (`:3545-3564`), so this runs the admission alone on V's stopped tables.
#[test]
fn chainlock_conflicting_blocks_are_never_admitted() {
    for conflicting in [false, true] {
        let mut rig = Rig::new();
        rig.accept(&child(&rig.p, &rig.genesis, 0));
        let side = branch(&rig.p, &rig.genesis, 2, 1);
        rig.headers(&side);
        rig.accept(&side[1]);
        let (b1, b2) = (rig.id(&side[0]), rig.id(&side[1]));
        let mut exit = rig.stop();
        if conflicting {
            let mut nx = exit.chain.node_index().expect("the node's index");
            for id in [b1, b2] {
                nx.modify(id, |g| g.status = Status(g.status.bits() | Status::CONFLICT_CHAINLOCK));
                nx.mark_dirty(id);
            }
        }
        let r = exit.chain.reader();
        let n_tx = u32::try_from(side[0].block.txs().len()).expect("one");
        let pos = FlatFilePos {
            file: 0,
            pos: 0,
        };
        exit.idx.received_block_transactions(&mut exit.chain, &r, b1, n_tx, pos).expect("RBT");
        assert_ne!(exit.idx.n_chain_tx(b2), 0, "the queue linked B2");
        assert_eq!(exit.idx.is_candidate(b2), !conflicting, "conflicting: {conflicting}");
        assert!(!exit.idx.is_candidate(b1), "B1 ties the tip and came later");
    }
}

/// The node half fails where Core's asserts would: the tip missing from the candidates
/// (`validation.cpp:5767`), and tables that never received genesis's transactions although
/// its entry has them (`:5722`).
#[test]
fn check_block_index_catches_broken_tables() {
    let mut rig = Rig::new();
    let a = branch(&rig.p, &rig.genesis, 1, 0);
    rig.accept(&a[0]);
    let g = rig.id(&rig.genesis);
    let exit = rig.stop();
    let r = exit.chain.reader();
    let tip = exit.chain.private_tip().expect("a tip").0.id;
    check_block_index(&exit.chain, &exit.idx, false).expect("consistent");

    let mut no_tip = exit.idx.clone();
    no_tip.candidates_erase(tip);
    let e = check_block_index(&exit.chain, &no_tip, false).expect_err("the tip is missing");
    assert_eq!(e.id, tip);
    assert_eq!(e.what, "a valid block at least as good as the tip must be a candidate");

    // The walk reaches genesis first.
    assert!(r.entry(g).expect("e").status.has(Status::HAVE_DATA));
    let mut fresh = NodeBlockIndex::new();
    fresh.candidates_insert(&r, tip);
    let e = check_block_index(&exit.chain, &fresh, false).expect_err("nChainTx is missing");
    assert_eq!(e.id, g);
    assert_eq!(e.what, "no never-processed ancestor is equivalent to HaveTxsDownloaded()");
}

/// `-checkblockindex=0` (BL-525): V's `CheckBlockIndex` returns at once, as Core's does when
/// `ShouldCheckBlockIndex()` is false; on, it fails on the same broken tables (the tip missing
/// from the candidates, restored before V's own checks run).
// Core: validation.cpp:5613-5617
#[test]
fn check_block_index_off_skips_the_check() {
    let run = |no_check_block_index: bool| {
        let mut rig = Rig::with(RigConfig {
            no_check_block_index,
            ..RigConfig::default()
        });
        rig.accept(&branch(&rig.p, &rig.genesis, 1, 0)[0]);
        let (send, checked) = std::sync::mpsc::channel();
        rig.v
            .send(ChainCommand::TestOnV(Box::new(move |v| {
                let tip = v.chain.private_tip().expect("a tip").0.id;
                v.idx.candidates_erase(tip);
                let outcome = v.check_block_index().map_err(|e| e.0);
                v.idx.candidates_insert(&v.reader, tip);
                send.send(outcome).expect("the test waits");
                Ok(())
            })))
            .expect("sent");
        let outcome = checked.recv().expect("V ran it");
        assert!(rig.stop().fatal.is_none());
        outcome
    };
    let e = run(false).expect_err("the tip is missing");
    assert!(e.contains("a valid block at least as good as the tip must be a candidate"));
    assert_eq!(run(true), Ok(()));
}

/// A chain of four blocks whose second never arrives: main[2] waits under main[1], with a
/// duplicate pair (OH-m8), and main[3] under main[2]. Genesis and main[0] have their data in
/// file 0, main[2] in file 1 and main[3] in file 2. Returns genesis's id and main's.
fn pruning_rig() -> (HeaderChain, NodeBlockIndex, BlockId, Vec<BlockId>) {
    let mut rig = Rig::new();
    let main = branch(&rig.p, &rig.genesis, 4, 0);
    rig.headers(&main);
    for m in [&main[3], &main[2], &main[0]] {
        rig.accept(m);
    }
    let g = rig.id(&rig.genesis);
    let ids: Vec<BlockId> = main.iter().map(|m| rig.id(m)).collect();
    let exit = rig.stop();
    assert!(exit.fatal.is_none());
    let (mut chain, mut idx) = (exit.chain, exit.idx);
    let mut ix = chain.node_index().expect("the node's");
    ix.modify(ids[2], |g| g.file = Some(1));
    ix.modify(ids[3], |g| g.file = Some(2));
    ix.take_dirty();
    idx.unlinked_insert(ids[1], ids[2]);
    assert_eq!(idx.unlinked_children(ids[1]), [ids[2], ids[2]]);
    assert_eq!(idx.unlinked_children(ids[2]), [ids[3]]);
    check_block_index(&chain, &idx, false).expect("consistent before the prune");
    (chain, idx, g, ids)
}

/// Step 7's clears (CH1-node §7.2; Core's `PruneOneBlockFile`): the entries with data in the
/// pruned files lose it and are dirtied, every unlinked pair of theirs goes, duplicates included,
/// and Core's `nFile` of 0 for an entry with no data puts its pairs in file 0's erase, though it
/// is not dirtied.
#[test]
fn prune_clear_clears_the_files_entries_and_their_unlinked_pairs() {
    let (mut chain, mut idx, g, ids) = pruning_rig();
    let cleared = idx.prune_clear(&mut chain, &[1]).expect("the node's");
    let e = chain.reader().entry(ids[2]).expect("held");
    assert!(!e.status.has(Status::HAVE_DATA) && !e.status.has(Status::HAVE_UNDO));
    assert_eq!((e.file, e.data_pos, e.undo_pos), (None, 0, 0));
    assert_eq!(e.n_tx, 1, "Core keeps nTx");
    assert!(idx.unlinked_children(ids[1]).is_empty(), "both duplicates go");
    assert_eq!(idx.unlinked_children(ids[2]), [ids[3]], "file 2's entry stays");
    let mut ix = chain.node_index().expect("the node's");
    assert_eq!(cleared, [ix.record(ids[2])]);
    assert_eq!(ix.take_dirty(), cleared, "dirty for the next B1");
    check_block_index(&chain, &idx, true).expect("consistent after the prune");

    // An entry with no data is in Core's file 0: its pair goes with file 0's entries, but its
    // record, unchanged, is not dirtied.
    idx.unlinked_insert(ids[0], ids[1]);
    let cleared = idx.prune_clear(&mut chain, &[0]).expect("the node's");
    assert!(idx.unlinked_children(ids[0]).is_empty());
    assert_eq!(idx.unlinked_children(ids[2]), [ids[3]], "file 2's entry stays");
    let mut ix = chain.node_index().expect("the node's");
    assert_eq!(cleared, [ix.record(g), ix.record(ids[0])], "in id order");
    assert_eq!(ix.take_dirty(), cleared);
    assert!(!chain.reader().entry(g).expect("held").status.has(Status::HAVE_DATA));
    check_block_index(&chain, &idx, true).expect("consistent after file 0's prune");
}

/// A chain that is not the node's fails `prune_clear` before anything changes: file 0's erase
/// would take the pair of an entry with no data (Core's `nFile` 0).
#[test]
fn a_failed_prune_clear_changes_nothing() {
    use std::sync::Arc;

    use dash_chain::{ChainConfig, Services, StaticSporks};
    use dash_clock::MockClock;

    let p = super::support::regtest();
    let svc = Services {
        clock: Arc::new(MockClock::at(0)),
        sporks: Arc::new(StaticSporks::for_network(&p, Some(false))),
        fail: Arc::new(super::support::Panics),
    };
    let cfg = ChainConfig::spv(&p, 10);
    let mut spv = HeaderChain::new_genesis(p, cfg, svc, dash_chain::NoStore).expect("genesis");
    let g = spv.best_header();
    let mut idx = NodeBlockIndex::new();
    idx.unlinked_insert(g, g);
    assert!(idx.prune_clear(&mut spv, &[0]).is_err());
    assert_eq!(idx.unlinked_children(g), [g], "the pair stays");
}

/// Two files in one request clear as each would alone: every entry of either, in id order, and
/// the pairs of both.
#[test]
fn prune_clear_of_several_files_clears_each() {
    let (mut chain, mut idx, g, ids) = pruning_rig();
    let cleared = idx.prune_clear(&mut chain, &[1, 0]).expect("the node's");
    let mut ix = chain.node_index().expect("the node's");
    assert_eq!(cleared, [ix.record(g), ix.record(ids[0]), ix.record(ids[2])], "in id order");
    assert_eq!(ix.take_dirty(), cleared);
    assert!(idx.unlinked_children(ids[1]).is_empty());
    assert_eq!(idx.unlinked_children(ids[2]), [ids[3]], "file 2's entry stays");
    let r = chain.reader();
    for id in [g, ids[0], ids[2]] {
        assert!(!r.entry(id).expect("held").status.has(Status::HAVE_DATA));
    }
    assert!(r.entry(ids[3]).expect("held").status.has(Status::HAVE_DATA));
    check_block_index(&chain, &idx, true).expect("consistent after the prune");
}
