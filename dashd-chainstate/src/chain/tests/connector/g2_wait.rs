//! G2 on the tip path (CH3 §2.17, D31; DEC-1874): a refused block waits. V does not stop, the
//! block stays `HAVE_DATA`, unfailed and a candidate, and the tip stays at its parent until a
//! ChainLock covers it, through V's ChainLock path. Until V's tip, out of IBD, first is a best
//! header added since V started, G2 passes with bounds only, as Core's unfinished masternode sync
//! (DEC-1878, DEC-1890).

use dash_chain::Status;
use tracing::Level;

use super::super::abc::Q::{self, Connected, Tip};
use super::super::failure::tip;
use super::super::locks::{intake, lock, record, settle};
use super::*;
use crate::chain::ShutdownFlag as _;

/// The queued `BlockConnected`s and `UpdatedBlockTip`s, in order.
#[derive(Default)]
struct Signals(Mutex<Vec<Q>>);

impl Signals {
    fn watch(rig: &Rig) -> Arc<Signals> {
        rig.sync();
        let s = Arc::new(Signals::default());
        rig.signals.register(s.clone());
        s
    }

    fn take(&self, rig: &Rig) -> Vec<Q> {
        rig.sync();
        std::mem::take(&mut *self.0.lock().expect("signals"))
    }
}

impl ValidationInterface for Signals {
    fn updated_block_tip(&self, new: BlockId, fork: Option<BlockId>, ibd: bool) {
        self.0.lock().expect("signals").push(Tip(new, fork, ibd));
    }

    fn block_connected(&self, _: &Arc<Block>, id: BlockId) -> crate::chain::Cont {
        self.0.lock().expect("signals").push(Connected(id));
        crate::chain::Cont::Done
    }
}

/// `last` (on `parent`, the tip) waits on G2 with `msg`: its `BlockChecked` is the error, one
/// more warning on V's thread (`warned` before), nothing reported, no shutdown, and V still
/// serves.
fn assert_waiting(w: &World, last: &Mined, parent: &Mined, msg: &str, warned: usize) {
    let r = w.rig.submit(last);
    assert!(r.accepted, "{r:?}");
    assert_waits(w, last, parent, msg, warned + 1);
    let states = checked(&w.rig);
    assert_eq!(states.last(), Some(&(last.hash(), BlockState::Error(msg.to_owned()))));
}

/// The waiting state of `waiting` above `parent`, with `warned` warnings for it on V's thread.
fn assert_waits(w: &World, waiting: &Mined, parent: &Mined, msg: &str, warned: usize) {
    w.rig.assert_no_abort();
    assert!(w.sink.messages().is_empty(), "{:?}", w.sink.messages());
    assert!(!w.rig.shutdown.shutdown_requested(), "V is not stopping");
    let status = w.rig.status(waiting);
    assert!(status.has(Status::HAVE_DATA), "{status:?}");
    assert!(!status.has(Status::HAVE_UNDO) && !status.has(Status::FAILED_MASK), "{status:?}");
    assert_eq!(tip(&w.rig), w.rig.id(parent));
    assert_eq!(coins_at(&w.coins), Some(parent.hash()));
    let id = w.rig.id(waiting);
    assert!(w.rig.idx(move |ix| ix.candidates().any(|c| c == id)), "still a candidate");
    let line = format!("ConnectTip: ConnectBlock {} failed, {msg}", waiting.hash());
    assert_eq!(levels_on(w.rig.v_thread(), &line), vec![Level::WARN; warned]);
}

/// G2 at step 19: the interim helper's refusal waits.
#[test]
fn g2_waits_where_the_helper_cannot_judge() {
    install_accept_log();
    let p = regtest();
    let g = Mined::genesis(&p);
    let b1 = block_on(&p, &g, vec![]);
    let msg = g2_refusal_text(&p, &at(&b1), INTERIM_REFUSAL);
    assert!(msg.ends_with("(ALLOWED-DIFFS AD-CH3-4); waiting for a ChainLock at or above 1"));
    let w = world(Setup {
        helper: Scripted {
            interim_at: Some(1),
            ..Scripted::default()
        },
        node_stop: true,
        ..Setup::default()
    });
    assert_waiting(&w, &b1, &g, &msg, 0);
}

/// Regtest's chain where heights 2 to 9 are inside an old-budget payment window
/// (`masternode/payments.cpp:262-295`): governance heights.
fn budget_from_2() -> Arc<ChainSetup> {
    let mut p = (*regtest()).clone();
    p.consensus.budget_payments_start_block = 2u32.into();
    let p = Arc::new(p);
    assert!(!is_governance_height(&p, 1) && is_governance_height(&p, 2));
    assert!(is_governance_height(&p, 3));
    p
}

/// `NodeHelper` under `guard`, ten minutes after genesis (its tip younger than -maxtipage: out
/// of IBD) unless `after_genesis` says otherwise, wired to stop as the node does.
fn node_setup(
    p: &Arc<ChainSetup>,
    guard: GuardScope,
    after_genesis: Option<i64>,
    stores: Option<Stores>,
) -> Setup {
    Setup {
        params: Some(p.clone()),
        node_helper: true,
        guard: Some(guard),
        after_genesis,
        stores,
        node_stop: true,
        ..Setup::default()
    }
}

/// A world with SPORK_19 on.
fn cl_world(setup: Setup) -> World {
    world_with(setup, |cfg| RigConfig {
        chainlocks: true,
        ..cfg
    })
}

/// G2 through the production helper (CH3 §2.17, D31): `NodeHelper` refuses a governance height
/// no ChainLock covers on a live network out of IBD, and the block waits; the same block connects
/// under the declared regime (regtest's) and in IBD.
#[test]
fn g2_through_node_helper_waits_only_live_and_out_of_ibd() {
    install_accept_log();
    let p = budget_from_2();
    let g = Mined::genesis(&p);
    let b1 = block_on(&p, &g, vec![]);
    let b2 = block_on(&p, &b1, vec![]);
    let msg = g2_refusal_text(&p, &at(&b2), INTERIM_REFUSAL);
    let w = world(node_setup(&p, GuardScope::Live, Some(600), None));
    w.rig.accept(&b1);
    assert_waiting(&w, &b2, &b1, &msg, 0);
    for (guard, after_genesis) in [(GuardScope::Declared, Some(600)), (GuardScope::Live, None)] {
        let w = world(node_setup(&p, guard, after_genesis, None));
        w.rig.accept(&b1);
        w.rig.accept(&b2);
        w.rig.assert_no_abort();
    }
}

/// DEC-1874 (a), (b): b2 waits at a governance height. b2 again, and b3 on it, retry b2 under
/// the same tip, ChainLock and IBD: it waits again without a `ConnectBlock` (no `BlockChecked`)
/// or another warning. A ChainLock on b2, recorded through V's ChainLock path while b2 is off the
/// active chain, retries it through EnforceBestChainLock's ABC: b2 connects as mined, and b3, a
/// governance height above the lock, waits in turn until a ChainLock on b3.
#[test]
fn g2_waits_for_a_chainlock_then_connects() {
    install_accept_log();
    let p = budget_from_2();
    let w = cl_world(node_setup(&p, GuardScope::Live, Some(600), None));
    let g = &w.rig.genesis;
    let b1 = block_on(&p, g, vec![]);
    let b2 = block_on(&p, &b1, vec![]);
    let b3 = block_on(&p, &b2, vec![]);
    let msg2 = g2_refusal_text(&p, &at(&b2), INTERIM_REFUSAL);
    let msg3 = g2_refusal_text(&p, &at(&b3), INTERIM_REFUSAL);
    w.rig.accept(&b1);
    assert_waiting(&w, &b2, &b1, &msg2, 0);
    // A duplicate and a new block re-run ABC, which meets b2 again.
    for m in [&b2, &b3] {
        assert!(w.rig.submit(m).accepted);
        assert_waits(&w, &b2, &b1, &msg2, 1);
        assert_eq!(checked(&w.rig), [], "no ConnectBlock");
    }
    assert!(w.rig.status(&b3).has(Status::HAVE_DATA));
    intake(&w.rig);
    assert!(record(&w.rig, &lock(&b2)), "the lock on an inactive block is recorded");
    settle(&w.rig);
    assert_eq!(tip(&w.rig), w.rig.id(&b2));
    assert_eq!(coins_at(&w.coins), Some(b2.hash()));
    assert!(w.rig.status(&b2).has(Status::HAVE_UNDO));
    assert_waits(&w, &b3, &b2, &msg3, 1);
    assert!(record(&w.rig, &lock(&b3)));
    settle(&w.rig);
    assert_eq!(tip(&w.rig), w.rig.id(&b3));
    assert_eq!(coins_at(&w.coins), Some(b3.hash()));
    w.rig.assert_no_abort();
    assert!(w.sink.messages().is_empty(), "{:?}", w.sink.messages());
}

/// `n` empty blocks on `parent`.
fn chain_on(p: &ChainSetup, parent: &Mined, n: usize) -> Vec<Mined> {
    let mut blocks: Vec<Mined> = Vec::new();
    for _ in 0..n {
        let next = block_on(p, blocks.last().unwrap_or(parent), vec![]);
        blocks.push(next);
    }
    blocks
}

/// `w`'s live node stopped cleanly and restarted on its datadir with its coins at `tip`: init's
/// ABC has run, then init's `InitializeCurrentBlockTip`.
fn restart(mut w: World, p: &Arc<ChainSetup>, tip: &Mined) -> World {
    let tip = w.rig.id(tip);
    let exit = w.rig.stop();
    assert_eq!(fatal_text(&exit), None, "a clean stop");
    let stores = Some(w.stores.clone());
    let (cfg, _, sink, seen) = node(node_setup(p, GuardScope::Live, Some(600), stores));
    let rig =
        Rig::restart_with(exit, Box::new(w.stores.blocks.clone()), tip, cfg).expect("V starts");
    assert_eq!(rig.init, Ok(Ok(())), "init's ABC connects");
    assert_eq!(rig.v.initialize_current_block_tip().recv(), Ok(()));
    World {
        rig,
        sink,
        seen,
        coins: w.coins,
        stores: w.stores,
    }
}

/// `blocks` from `from`, all at governance heights, but the last arrive, headers first, then
/// their blocks: the tip moves to each, with bounds only. At the last but one the tip is a best
/// header this process added, so the last waits.
fn catch_up_then_wait(w: &World, p: &ChainSetup, blocks: &[Mined], from: usize) {
    assert!(blocks[from..].iter().all(|m| is_governance_height(p, at(m).height)));
    let (last, done) = blocks.split_last().expect("blocks");
    w.rig.headers(&done[from..]);
    for m in &done[from..] {
        assert!(w.rig.submit(m).accepted);
    }
    let top = done.last().expect("a block before the last");
    assert_eq!(tip(&w.rig), w.rig.id(top));
    assert_eq!(coins_at(&w.coins), Some(top.hash()));
    let msg = g2_refusal_text(p, &at(last), INTERIM_REFUSAL);
    assert_waiting(w, last, top, &msg, 0);
}

/// DEC-1878 (c): a node stopped while b2 waits, its tip b1, blocks b2 to b12 stored and the best
/// header b20, well past what a ChainLock it could still verify covers (`SIGN_HEIGHT_OFFSET`),
/// restarts with no ChainLock: its tip is behind the best header, so G2 passes with bounds only
/// (Core's unfinished masternode sync): init's ABC connects b2 to b12 without stopping, and the
/// rest connect as they arrive.
#[test]
fn g2_passes_after_a_restart_with_the_best_header_ahead() {
    install_accept_log();
    let p = budget_from_2();
    let w = world(node_setup(&p, GuardScope::Live, Some(600), None));
    let g = w.rig.genesis.clone();
    let blocks = chain_on(&p, &g, 20);
    let (b1, b2) = (&blocks[0], &blocks[1]);
    let msg = g2_refusal_text(&p, &at(b2), INTERIM_REFUSAL);
    w.rig.accept(b1);
    assert_waiting(&w, b2, b1, &msg, 0);
    w.rig.headers(&blocks[2..]);
    for m in &blocks[2..12] {
        assert!(w.rig.submit(m).accepted);
    }
    assert_waits(&w, b2, b1, &msg, 1);

    let w = restart(w, &p, b1);
    assert_eq!(tip(&w.rig), w.rig.id(&blocks[11]));
    for m in &blocks[12..] {
        w.rig.accept(m);
    }
    assert_eq!(tip(&w.rig), w.rig.id(&blocks[19]));
    assert_eq!(coins_at(&w.coins), Some(blocks[19].hash()));
    w.rig.assert_no_abort();
    assert!(!w.rig.shutdown.shutdown_requested());
    let line = format!("ConnectTip: ConnectBlock {} failed, {msg}", b2.hash());
    assert!(levels_on(w.rig.v_thread(), &line).is_empty(), "no wait after the restart");
}

/// A node stopped at b1, its tip the best header it stored, restarts out of IBD: that best
/// header is not one this process added, so neither init nor its ABC moves V live (DEC-1878,
/// "since process start"). Headers b2 to b8 then arrive with their blocks and connect with
/// bounds only, though all are governance heights; b9 waits.
#[test]
fn g2_passes_a_catch_up_from_the_stored_best_header() {
    install_accept_log();
    let p = budget_from_2();
    let w = world(node_setup(&p, GuardScope::Live, Some(600), None));
    let g = w.rig.genesis.clone();
    let blocks = chain_on(&p, &g, 9);
    w.rig.accept(&blocks[0]);
    let w = restart(w, &p, &blocks[0]);
    assert_eq!(tip(&w.rig), w.rig.id(&blocks[0]));
    catch_up_then_wait(&w, &p, &blocks, 1);
}

/// A node stopped while b2 waits, b3 and b4 stored and its best header b4, restarts out of IBD:
/// init's ABC connects b2 to b4, a tip that is the stored best header, which this process did
/// not accept, so V stays syncing. Headers b5 to b8 then arrive with their blocks and connect
/// with bounds only, though all are governance heights; b9 waits.
#[test]
fn g2_passes_a_catch_up_past_what_init_connected() {
    install_accept_log();
    let p = budget_from_2();
    let w = world(node_setup(&p, GuardScope::Live, Some(600), None));
    let g = w.rig.genesis.clone();
    let blocks = chain_on(&p, &g, 9);
    let (b1, b2) = (&blocks[0], &blocks[1]);
    let msg = g2_refusal_text(&p, &at(b2), INTERIM_REFUSAL);
    w.rig.accept(b1);
    assert_waiting(&w, b2, b1, &msg, 0);
    w.rig.headers(&blocks[2..4]);
    for m in &blocks[2..4] {
        assert!(w.rig.submit(m).accepted);
    }
    assert_waits(&w, b2, b1, &msg, 1);
    let w = restart(w, &p, b1);
    assert_eq!(tip(&w.rig), w.rig.id(&blocks[3]));
    catch_up_then_wait(&w, &p, &blocks, 4);
}

/// DEC-1878: a catch-up out of IBD through governance heights, its tip behind the best header,
/// connects them with bounds only; once the tip has reached the best header, the next governance
/// height waits for its ChainLock, which connects it. V starts a day past genesis, in IBD (so
/// init's genesis does not reach the best header out of IBD), and the clock then moves back to
/// ten minutes past it.
#[test]
fn g2_passes_a_catch_up_then_waits_at_the_live_tip() {
    install_accept_log();
    let p = budget_from_2();
    let w = cl_world(node_setup(&p, GuardScope::Live, None, None));
    let g = w.rig.genesis.clone();
    w.rig.clock.set_mocktime(i64::from(g.time()) + 600);
    let blocks = chain_on(&p, &g, 6);
    w.rig.headers(&blocks[..5]);
    for m in &blocks[..5] {
        w.rig.accept(m);
    }
    assert_eq!(tip(&w.rig), w.rig.id(&blocks[4]));
    let (b5, b6) = (&blocks[4], &blocks[5]);
    let msg = g2_refusal_text(&p, &at(b6), INTERIM_REFUSAL);
    assert_waiting(&w, b6, b5, &msg, 0);
    intake(&w.rig);
    assert!(record(&w.rig, &lock(b6)));
    settle(&w.rig);
    assert_eq!(tip(&w.rig), w.rig.id(b6));
    w.rig.assert_no_abort();
}

/// Regtest's chain where only every third height is inside an old-budget payment window:
/// governance heights 3, 6 and 9.
fn budget_every_3() -> Arc<ChainSetup> {
    let mut p = (*regtest()).clone();
    p.consensus.budget_payments_start_block = 1u32.into();
    p.consensus.budget_payments_cycle_blocks = 3;
    p.consensus.budget_payments_window_blocks = 1;
    let p = Arc::new(p);
    let governance: Vec<u32> = (1..=6).filter(|&h| is_governance_height(&p, h)).collect();
    assert_eq!(governance, [3, 6]);
    p
}

/// DEC-1874 (m2): b2 and b3 in one ABC, b3 waiting: b2 stays connected, with its
/// `BlockConnected` and `UpdatedBlockTip`, and the tip is b2. b1 connected first, so V's tip has
/// reached a best header it accepted (DEC-1878).
#[test]
fn g2_keeps_what_its_abc_connected_before_it() {
    install_accept_log();
    let p = budget_every_3();
    let w = world(node_setup(&p, GuardScope::Live, Some(600), None));
    let blocks = chain_on(&p, &w.rig.genesis, 3);
    let (b1, b2, b3) = (&blocks[0], &blocks[1], &blocks[2]);
    let msg = g2_refusal_text(&p, &at(b3), INTERIM_REFUSAL);
    w.rig.accept(b1);
    w.rig.headers(&blocks[1..]);
    // Its parent's data is missing: stored, not connected.
    assert!(w.rig.submit(b3).accepted);
    assert_eq!(tip(&w.rig), w.rig.id(b1));
    let a = Signals::watch(&w.rig);
    assert!(w.rig.submit(b2).accepted);
    let [i1, i2] = w.rig.ids_n(&blocks[..2]);
    assert_eq!(a.take(&w.rig), [Connected(i2), Tip(i2, Some(i1), false)]);
    assert_waits(&w, b3, b2, &msg, 1);
}

/// DEC-1874 (m2): a lock above the waiting block. While b2 waits, a ChainLock on b3 connects b2
/// and b3 in one ABC, EnforceBestChainLock's.
#[test]
fn g2_waits_until_a_lock_above_it_connects_both() {
    install_accept_log();
    let p = budget_from_2();
    let w = cl_world(node_setup(&p, GuardScope::Live, Some(600), None));
    let g = w.rig.genesis.clone();
    let b1 = block_on(&p, &g, vec![]);
    let b2 = block_on(&p, &b1, vec![]);
    let b3 = block_on(&p, &b2, vec![]);
    let msg = g2_refusal_text(&p, &at(&b2), INTERIM_REFUSAL);
    w.rig.accept(&b1);
    assert_waiting(&w, &b2, &b1, &msg, 0);
    assert!(w.rig.submit(&b3).accepted);
    intake(&w.rig);
    let a = Signals::watch(&w.rig);
    assert!(record(&w.rig, &lock(&b3)));
    settle(&w.rig);
    let [i1, i2, i3] = w.rig.ids_n(&[b1, b2, b3.clone()]);
    // One ABC, a step per block: each raises the work (`v:3676-3679` at v24.0.0).
    let want = [Connected(i2), Tip(i2, Some(i1), false), Connected(i3), Tip(i3, Some(i2), false)];
    assert_eq!(a.take(&w.rig), want);
    assert_eq!(tip(&w.rig), i3);
    assert_eq!(coins_at(&w.coins), Some(b3.hash()));
    w.rig.assert_no_abort();
}

/// `n` empty blocks on `parent` that are not [`chain_on`]'s: each coinbase's script carries a salt.
fn side_on(p: &ChainSetup, parent: &Mined, n: usize) -> Vec<Mined> {
    let null = OutPoint::new(TxHash::from([0; 32]), u32::MAX);
    let mut blocks: Vec<Mined> = Vec::new();
    for _ in 0..n {
        let prev = blocks.last().unwrap_or(parent);
        let cb = tx(
            1,
            0,
            vec![input(null, cb_script(prev.height + 1, 1), u32::MAX)],
            vec![out(COIN, &[OP_TRUE])],
        );
        let next = with_coinbase(p, prev, cb, vec![]);
        blocks.push(next);
    }
    blocks
}

/// `blocks` arrive one at a time and each becomes the tip.
fn connect_each(w: &World, blocks: &[Mined]) {
    for m in blocks {
        w.rig.accept(m);
        assert_eq!(tip(&w.rig), w.rig.id(m), "{} connects", m.height);
    }
}

/// DEC-1890 (BL-939): a node stopped while b2 waits restarts out of IBD; init's ABC connects b2,
/// the stored best header, which this process did not add, so V stays syncing. `invalidateblock`
/// of b2 walks the tip and the best header back to b1, an older entry, so V still stays syncing:
/// b2', a governance height and the first header added since start, connects with bounds only,
/// and once the tip has reached it, b3' waits. Core would set `fReachedBestHeader` already at the
/// walk-back's tip signal (masternode/sync.cpp:147-155 at v24.0.2); not counting a loaded entry is
/// DEC-1878's stricter rule (CH3a D69).
#[test]
fn g2_passes_after_an_invalidateblock_walks_back_to_an_older_header() {
    install_accept_log();
    let p = budget_from_2();
    let w = world(node_setup(&p, GuardScope::Live, Some(600), None));
    let g = w.rig.genesis.clone();
    let main = chain_on(&p, &g, 2);
    let (b1, b2) = (&main[0], &main[1]);
    let msg = g2_refusal_text(&p, &at(b2), INTERIM_REFUSAL);
    w.rig.accept(b1);
    assert_waiting(&w, b2, b1, &msg, 0);
    let w = restart(w, &p, b1);
    assert_eq!(tip(&w.rig), w.rig.id(b2));
    super::disconnect::invalidate(&w, b2);
    assert_eq!(tip(&w.rig), w.rig.id(b1));
    let side = side_on(&p, b1, 2);
    assert!(side.iter().all(|m| is_governance_height(&p, m.height)));
    connect_each(&w, &side[..1]);
    let msg = g2_refusal_text(&p, &at(&side[1]), INTERIM_REFUSAL);
    assert_waiting(&w, &side[1], &side[0], &msg, 0);
}

/// DEC-1890 (BL-939, G2-wait fix check m-1): a node stopped while b2 waits, with b3 stored and
/// invalid (it spends a coin that does not exist) and its best header b3, restarts: init's ABC
/// connects b2 with bounds only, b3 fails, and `InvalidChainFound` walks the best header back to
/// the tip, b2, an older entry. An `invalidateblock` of b3 then gives a tip signal at that best
/// header, which does not move V. b3', a governance height and the first header added since start,
/// connects with bounds only, and once the tip has reached it, b4' waits.
#[test]
fn g2_passes_after_init_finds_the_loaded_best_header_invalid() {
    install_accept_log();
    let p = budget_from_2();
    let w = world(node_setup(&p, GuardScope::Live, Some(600), None));
    let g = w.rig.genesis.clone();
    let b1 = block_on(&p, &g, vec![]);
    let b2 = block_on(&p, &b1, vec![]);
    let b3 = block_on(&p, &b2, vec![spend(&[seed_op(9)], COIN)]);
    let msg = g2_refusal_text(&p, &at(&b2), INTERIM_REFUSAL);
    w.rig.accept(&b1);
    assert_waiting(&w, &b2, &b1, &msg, 0);
    assert!(w.rig.submit(&b3).accepted);
    assert_waits(&w, &b2, &b1, &msg, 1);
    let w = restart(w, &p, &b1);
    assert_eq!(tip(&w.rig), w.rig.id(&b2));
    assert!(w.rig.status(&b3).has(Status::FAILED_MASK), "init's ABC found b3 invalid");
    super::disconnect::invalidate(&w, &b3);
    assert_eq!(tip(&w.rig), w.rig.id(&b2));
    let side = side_on(&p, &b2, 2);
    assert!(side.iter().all(|m| is_governance_height(&p, m.height)));
    connect_each(&w, &side[..1]);
    let msg = g2_refusal_text(&p, &at(&side[1]), INTERIM_REFUSAL);
    assert_waiting(&w, &side[1], &side[0], &msg, 0);
}
