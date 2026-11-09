//! `invalidateblock`, `reconsiderblock` and `preciousblock` through V (N4, §4.6, §4.7), and
//! `LoadChainTip` at an invalid chainstate tip (DEC-196).

use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use dash_chain::Status;
use dash_consensus::{Block, BlockId, TxRef};
use dash_dml::MnList;
use dash_primitives::BlockHash;

use super::abc::Q::{Connected, Disconnected, Tip};
use super::abc::{Announced, Breaks, Mempool, Pool, Rejects, Ui, adversarial};
use super::actor::{Log, PanickingAbort, entries, logged};
use super::connect::Latched;
use super::connector::checked;
use super::flush::{Counts, InitCalls, ModelW, assert_init_dropped, coins};
use super::support::{
    Mined, Rig, RigConfig, SharedFiles, assert_logged_before, batch, branch, child,
    install_accept_log, lines_of, lines_on, regtest,
};
use crate::chain::actor::V;
use crate::chain::actor::tests::Stage;
use crate::chain::reply::{ActorReply, WaitError, reply};
use crate::chain::{
    AbortSink, BlockConnector, BlockNotFound, BlockStatusWriter, Blocking, ChainCommand,
    ChainHandle, ConnectCx, ConnectOutcome, DisconnectPool, DisconnectResult, Fatal, INVALID_TIP,
    LoadingFlags, LocalShutdown, MempoolSink, NodeBlockIndex, NullConnector, ProposalChecks,
    RecordingAbort, RemovalReason, ShutdownFlag, SyncState, VContext, VExit, ViewTarget,
    genesis_block, load_chain_tip,
};
use crate::store::held::fail_on_w;
use crate::store::{Failure, GlobalFailStop};

const SOON: Duration = Duration::from_secs(120);

fn failed(rig: &Rig, m: &Mined) -> bool {
    rig.status(m).has(Status::FAILED_VALID)
}

pub(super) fn tip(rig: &Rig) -> BlockId {
    rig.v.view().snap.tip.tip
}

fn best_header(rig: &Rig) -> BlockId {
    rig.v.query(|q| q.chain.best_header()).recv().expect("V serves")
}

/// §9.4's adversarial schedule (so [`Announced`] sees each signal on time), and `post` run on V
/// inside the next hold it releases (its `Stage::Release`): what `post` sends V reaches the
/// release point after it.
fn at_first_release(rig: &Rig, post: impl FnOnce(&ChainHandle) + Send + 'static) {
    let (signals, v) = (rig.signals.clone(), rig.v.clone());
    let post = Mutex::new(Some(post));
    rig.on_v(move |vv| {
        vv.release_hook = Some(Arc::new(move |stage| {
            if matches!(stage, Stage::Release | Stage::Spliced) {
                signals.sync(&mut Blocking).expect("the signals thread runs");
            }
            if stage == Stage::Release
                && let Some(post) = post.lock().expect("post").take()
            {
                post(&v);
            }
        }));
        Ok(())
    });
}

/// A reply `post` stashed for the test thread.
pub(super) type Stash<T> = Arc<Mutex<Vec<ActorReply<T>>>>;

/// [`at_first_release`] for `post`'s replies, which the test reads with [`take`].
pub(super) fn replies_at_first_release<T: Send + 'static>(
    rig: &Rig,
    post: impl FnOnce(&ChainHandle) -> Vec<ActorReply<T>> + Send + 'static,
) -> Stash<T> {
    let stash = Stash::default();
    let s = stash.clone();
    at_first_release(rig, move |v| s.lock().expect("stash").extend(post(v)));
    stash
}

pub(super) fn take<T>(stash: &Stash<T>) -> Vec<T> {
    let replies = std::mem::take(&mut *stash.lock().expect("stash"));
    replies.into_iter().map(|r| r.recv().expect("V replies")).collect()
}

/// Each iteration disconnects the tip in its own hold, published before the next (Core releases
/// `cs_main` between them, `validation.cpp:3941`, `:4003`). Every disconnected block is marked
/// `FAILED_VALID` (the `:3983` swap never applies after the first iteration, §4.6), the best
/// header walks back, and the final scope signals `UpdatedBlockTip` with no fork and notifies
/// the UI of the invalidated block's parent, after publication.
#[test]
fn invalidate_disconnects_one_block_per_hold_and_marks_each_failed() {
    let ui = Arc::new(Ui::default());
    let mut rig = Rig::with(RigConfig {
        ui: Some(ui.clone()),
        ..RigConfig::default()
    });
    let main = branch(&rig.p, &rig.genesis, 3, 0);
    for m in &main {
        rig.accept(m);
    }
    let [a1, a2, a3] = rig.ids_n(&main);
    assert!(ui.v.set(rig.v.clone()).is_ok());
    let adv = adversarial(&rig);
    let a = Announced::watch(&rig);

    assert_eq!(rig.v.invalidate_block(main[1].hash()).recv(), Ok(Ok(())));
    rig.sync();
    assert_eq!(a.take(), [Disconnected(a3), Disconnected(a2), Tip(a1, None, true)]);
    // Two iterations, the scope that finds a2 gone (:3942-3944), the final scope, then the
    // RPC's ABC with nothing to do.
    assert_eq!(adv.released(), [a3, a2, a1, a1, a1]);
    let step = [Stage::Release, Stage::Snapshot, Stage::Published, Stage::Spliced, Stage::Watched];
    let last = [Stage::Release, Stage::Snapshot, Stage::Published, Stage::Spliced, Stage::Ui];
    let idle = [Stage::Release, Stage::Snapshot, Stage::Published, Stage::Spliced];
    let stages = std::mem::take(&mut *adv.stages.lock().expect("stages"));
    assert_eq!(stages, [&step[..], &step, &idle, &last, &idle].concat(), "N1's order");
    assert_eq!(*ui.seen.lock().expect("ui"), [(SyncState::InitDownload, 1, 1)]);

    assert_eq!(tip(&rig), a1);
    assert_eq!(rig.v.tip_watch().get(), Some(main[0].hash()));
    assert!(failed(&rig, &main[1]) && failed(&rig, &main[2]));
    assert!(!rig.status(&main[1]).has(Status::FAILED_CHILD), "the swap never applied");
    assert!(!rig.status(&main[0]).has(Status::FAILED_MASK));
    assert_eq!(best_header(&rig), a1, "walked back twice (:3951, :3969)");
    assert_eq!(rig.idx(|ix| ix.best_invalid()), Some(a2));
    assert_eq!(rig.idx(|ix| ix.candidates().collect::<Vec<_>>()), [a1]);
    let mut exit = rig.stop();
    let failed_blocks =
        exit.chain.node_index().expect("node").failed_blocks().iter().collect::<Vec<_>>();
    assert_eq!(failed_blocks, [a2], "m_failed_blocks (:4019)");
}

/// Disconnecting adds the equal-or-more-work blocks of `candidate_blocks_by_work` as the tip
/// walks down (`validation.cpp:3991-3999`); the RPC's ABC then moves to the other branch.
#[test]
fn invalidate_moves_to_an_equal_work_branch() {
    let rig = Rig::new();
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    let side = branch(&rig.p, &rig.genesis, 2, 1);
    for m in main.iter().chain(&side) {
        rig.accept(m);
    }
    let [a1, a2] = rig.ids_n(&main);
    let [b1, b2] = rig.ids_n(&side);
    assert_eq!(tip(&rig), a2, "first seen");
    let a = Announced::watch(&rig);
    let _adv = adversarial(&rig);
    assert_eq!(rig.v.invalidate_block(main[0].hash()).recv(), Ok(Ok(())));
    rig.sync();
    let g = rig.id(&rig.genesis);
    assert_eq!(
        a.take(),
        [
            Disconnected(a2),
            Disconnected(a1),
            Tip(g, None, true),
            Connected(b1),
            Tip(b1, Some(g), true),
            Connected(b2),
            Tip(b2, Some(b1), true)
        ]
    );
    assert_eq!(tip(&rig), b2);
    assert!(failed(&rig, &main[0]) && failed(&rig, &main[1]));
}

/// Hooks 1 and 2 (`validation.cpp:3951-3954`, `:3969-3972`) set `m_best_invalid` to the best
/// header they walk back from, whatever its work, so it ends at the invalidated block even when it
/// named a header with more work; `InvalidChainFound` alone (`:2040-2042`) would keep that one.
#[test]
fn invalidate_moves_best_invalid_off_a_better_failed_header() {
    let rig = Rig::new();
    let main = branch(&rig.p, &rig.genesis, 3, 0);
    for m in &main {
        rig.accept(m);
    }
    let [a1, a2, a3] = rig.ids_n(&main);
    let side = branch(&rig.p, &rig.genesis, 4, 1);
    rig.headers(&side);
    let s4 = rig.id(&side[3]);
    assert_eq!(best_header(&rig), s4);
    assert_eq!(rig.v.invalidate_block(side[3].hash()).recv(), Ok(Ok(())));
    assert_eq!(rig.idx(|ix| ix.best_invalid()), Some(s4));
    assert_eq!(best_header(&rig), a3, "InvalidChainFound's walk (:2043-2044)");

    // Hook 2 walks a3 back in the first iteration, hook 1 a2 in the second.
    assert_eq!(rig.v.invalidate_block(main[1].hash()).recv(), Ok(Ok(())));
    assert_eq!(rig.idx(|ix| ix.best_invalid()), Some(a2));
    assert_eq!(best_header(&rig), a1);
}

/// A read queued during `InvalidateBlock` runs at the release point between iterations
/// (`validation.cpp:3937-3940`) and sees the first iteration's hook 1 (`:3951-3954`). The best
/// header is below the tip there: Core raises it only for a new header, so a reconsidered chain
/// reconnects above it.
#[test]
fn a_read_between_invalidate_iterations_sees_the_first_walk_back() {
    let rig = Rig::new();
    let main = branch(&rig.p, &rig.genesis, 3, 0);
    for m in &main {
        rig.accept(m);
    }
    let g = rig.id(&rig.genesis);
    let [a1, a2, a3] = rig.ids_n(&main);
    assert_eq!(rig.v.invalidate_block(main[1].hash()).recv(), Ok(Ok(())));
    assert_eq!(rig.v.reconsider_block(main[1].hash(), false).recv(), Ok(Ok(())));
    let best_invalid = rig.idx(|ix| ix.best_invalid());
    assert_eq!((tip(&rig), best_header(&rig), best_invalid), (a3, a1, None));

    let stash = replies_at_first_release(&rig, |v| {
        vec![v.query(|q| {
            let tip = q.chain.private_tip().map(|(t, _)| t.id);
            (tip, q.chain.best_header(), q.idx.best_invalid())
        })]
    });
    assert_eq!(rig.v.invalidate_block(main[0].hash()).recv(), Ok(Ok(())));
    assert_eq!(take(&stash), [(Some(a2), g, Some(a1))], "after the first iteration");
    assert_eq!(tip(&rig), g);
}

/// A block accepted at a release point between iterations (`validation.cpp:3937-3940`) with no
/// more work than the walk's tip is no candidate (`TryAddBlockIndexCandidate`, `:4252-4256`).
/// Once the walk is below it, the block is missing from the candidates, which Core's
/// `-checkblockindex` asserts (`:4006`, before the repair; Core calls the gap "essentially
/// unobservable"), so V aborts there too. Without the check, the final rescan (`:4028-4032`)
/// admits it and ABC connects it; the block's own ABC tail waits for the operation (§2.4).
#[test]
fn the_final_rescan_admits_a_block_accepted_during_the_walk() {
    for check in [true, false] {
        let mut rig = Rig::with(RigConfig {
            no_check_block_index: !check,
            ..RigConfig::default()
        });
        let main = branch(&rig.p, &rig.genesis, 3, 0);
        for m in &main {
            rig.accept(m);
        }
        let x2 = child(&rig.p, &main[0], 1);
        let a = Announced::watch(&rig);
        let block = x2.block.clone();
        let stash =
            replies_at_first_release(&rig, move |v| vec![v.process_block(block, true, None, true)]);
        let invalidated = rig.v.invalidate_block(main[1].hash()).recv();
        let x = rig.id(&x2);
        if check {
            assert_eq!(invalidated, Err(WaitError::Disconnected));
            assert_candidate_abort(&mut rig, x);
            continue;
        }
        assert_eq!(invalidated, Ok(Ok(())));
        let [r] = &take(&stash)[..] else {
            unreachable!()
        };
        assert!(r.accepted && r.state.is_valid(), "{r:?}");
        rig.sync();
        let [a1, a2, a3] = rig.ids_n(&main);
        assert_eq!(
            a.take(),
            [
                Disconnected(a3),
                Disconnected(a2),
                Tip(a1, None, true),
                Connected(x),
                Tip(x, Some(a1), true)
            ]
        );
    }
}

/// `preciousblock` and `reconsiderblock` sent during `InvalidateBlock` run their `cs_main` scope
/// at the release point between iterations; their ABC tails wait for the operation (§2.4).
/// The precious block, ahead of the walk's tip once its scope ran, is still connected only by
/// the RPC's ABC after the final scope.
#[test]
fn precious_and_reconsider_during_the_walk_defer_their_abc() {
    let rig = Rig::new();
    let main = branch(&rig.p, &rig.genesis, 3, 0);
    for m in &main {
        rig.accept(m);
    }
    let x2 = child(&rig.p, &main[0], 1);
    let b1 = child(&rig.p, &rig.genesis, 2);
    rig.accept(&x2);
    rig.accept(&b1);
    assert_eq!(rig.v.invalidate_block(b1.hash()).recv(), Ok(Ok(())));
    assert!(failed(&rig, &b1));
    let a = Announced::watch(&rig);
    let (x, b) = (x2.hash(), b1.hash());
    let stash = replies_at_first_release(&rig, move |v| {
        vec![v.precious_block(x), v.reconsider_block(b, false)]
    });
    assert_eq!(rig.v.invalidate_block(main[1].hash()).recv(), Ok(Ok(())));
    assert_eq!(take(&stash), [Ok(()), Ok(())]);
    rig.sync();
    let [a1, a2, a3] = rig.ids_n(&main);
    let x = rig.id(&x2);
    assert_eq!(
        a.take(),
        [
            Disconnected(a3),
            Disconnected(a2),
            Tip(a1, None, true),
            Connected(x),
            Tip(x, Some(a1), true)
        ]
    );
    assert_eq!(rig.idx(move |ix| ix.sequence_id(x)), -1, "PreciousBlock's scope ran");
    assert!(!rig.status(&b1).has(Status::FAILED_MASK), "ResetBlockFailureFlags ran");
}

/// A shutdown request ends the walk at the next iteration (`validation.cpp:3938`); the final
/// scope still marks the last disconnected block (`:4015-4044`). The RPC's ABC still runs one
/// step before it checks the request (`:3836-3840`): here, the reorg to a side branch the walk
/// made the best candidate.
#[test]
fn a_shutdown_during_the_walk_runs_the_final_scope_on_the_last_disconnected_block() {
    let mut rig = Rig::new();
    let main = branch(&rig.p, &rig.genesis, 3, 0);
    let side = branch(&rig.p, &main[0], 2, 1);
    for m in main.iter().chain(&side) {
        rig.accept(m);
    }
    let [a1, a2, a3] = rig.ids_n(&main);
    let [x2, x3] = rig.ids_n(&side);
    let a = Announced::watch(&rig);
    at_first_release(&rig, ChainHandle::shutdown);
    assert_eq!(rig.v.invalidate_block(main[0].hash()).recv(), Ok(Ok(())));
    rig.sync();
    let walk = [Disconnected(a3), Tip(a2, None, true)];
    let step = [Disconnected(a2), Connected(x2), Connected(x3), Tip(x3, Some(a1), true)];
    assert_eq!(a.take(), [&walk[..], &step].concat());
    let mut exit = rig.stop();
    let r = exit.chain.reader();
    let status = |id| r.entry(id).expect("indexed").status;
    assert!(status(a3).has(Status::FAILED_VALID));
    assert!(!status(a2).has(Status::FAILED_MASK) && !status(a1).has(Status::FAILED_MASK));
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.id), Some(x3));
    assert_eq!(exit.idx.best_invalid(), Some(a3));
    let failed_blocks =
        exit.chain.node_index().expect("node").failed_blocks().iter().collect::<Vec<_>>();
    assert_eq!(failed_blocks, [a3]);
}

/// Shutdown requested before the first iteration: the block is still in the active chain, so
/// `InvalidateBlock` returns `false` with no final scope (`validation.cpp:4010-4013`).
#[test]
fn a_shutdown_before_the_walk_leaves_the_block_unmarked() {
    let rig = Rig::new();
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    for m in &main {
        rig.accept(m);
    }
    let a = Announced::watch(&rig);
    let a1 = rig.id(&main[0]);
    let ran = Arc::new(Mutex::new(None));
    let r = ran.clone();
    rig.on_v(move |v| {
        v.request_shutdown();
        *r.lock().expect("ran") = Some(v.invalidate_block(a1, &mut false)?);
        Ok(())
    });
    assert_eq!(*ran.lock().expect("ran"), Some(false));
    rig.sync();
    assert_eq!(a.take(), []);
    assert!(!failed(&rig, &main[0]));
    assert_eq!(rig.idx(|ix| ix.best_invalid()), None);
}

/// `DisconnectBlock` refuses the listed block.
pub(super) struct RefusesDisconnect(pub(super) Arc<OnceLock<BlockHash>>);

impl BlockConnector for RefusesDisconnect {
    fn connect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        checked: bool,
        id: BlockId,
        st: &mut BlockStatusWriter<'_>,
    ) -> Result<ConnectOutcome, Fatal> {
        NullConnector.connect_block(cx, v, target, block, checked, id, st)
    }

    fn disconnect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        b: &Block,
        id: BlockId,
    ) -> Result<DisconnectResult, Fatal> {
        if self.0.get() == Some(&b.hash()) {
            return Ok(DisconnectResult::Failed);
        }
        NullConnector.disconnect_block(cx, v, target, b, id)
    }

    fn test_block_validity(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        checks: &dyn ProposalChecks,
        b: &Block,
        prev: BlockId,
        pow: bool,
        merkle: bool,
    ) -> Result<ConnectOutcome, Fatal> {
        NullConnector.test_block_validity(cx, v, checks, b, prev, pow, merkle)
    }
}

/// A failed `DisconnectTip` (`validation.cpp:3966`): no transactions go back to the mempool for
/// it (`:3965`), and `InvalidateBlock` returns `false` with no final scope, so the block above it
/// stays failed and the invalidated block does not. The state stays valid, so the RPC still runs
/// ABC and succeeds (`rpc:2015-2018`).
#[test]
fn a_failed_disconnect_ends_the_walk_without_the_final_scope() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let refused = Arc::new(OnceLock::new());
    let rig = Rig::with(RigConfig {
        connector: Some(Box::new(RefusesDisconnect(refused.clone()))),
        mempool: Some(Box::new(Mempool(log.clone()))),
        ..RigConfig::default()
    });
    let main = branch(&rig.p, &rig.genesis, 3, 0);
    for m in &main {
        rig.accept(m);
    }
    let [_, a2, a3] = rig.ids_n(&main);
    refused.set(main[1].hash()).expect("once");
    log.lock().expect("pool").clear();
    let a = Announced::watch(&rig);
    let Some(invalidated) = rig.v.invalidate_block(main[0].hash()).recv_timeout(SOON) else {
        // A walk that ignored the failure would retry a2 forever: leak the rig, as dropping it
        // would join V.
        std::mem::forget(rig);
        panic!("InvalidateBlock never returned");
    };
    assert_eq!(invalidated, Ok(Ok(())));
    rig.sync();
    let pool = [Pool::New, Pool::Update(true), Pool::New, Pool::Update(false)];
    assert_eq!(*log.lock().expect("pool"), pool);
    assert_eq!(a.take(), [Disconnected(a3)]);
    assert_eq!(tip(&rig), a2);
    assert!(failed(&rig, &main[2]));
    assert!(!rig.status(&main[1]).has(Status::FAILED_MASK));
    assert!(!rig.status(&main[0]).has(Status::FAILED_MASK), "no final scope");
}

/// `InvalidateBlock` never takes a ChainLock-conflicting block into `candidate_blocks_by_work`
/// (`validation.cpp:3916-3933`), so the walk does not admit it (`:3991-3999`) even with more
/// work than the new tip; a read at the next release point sees it out. Then `reconsiderblock`'s
/// `ignore_chainlocks` (`:4152-4231`): without it the block keeps its mark and stays out; with it
/// the mark is cleared and ABC connects the block.
#[test]
fn reconsider_clears_a_chainlock_conflict_only_when_told_to() {
    let rig = Rig::new();
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    let side = branch(&rig.p, &rig.genesis, 2, 1);
    for m in main.iter().chain(&side) {
        rig.accept(m);
    }
    let a1 = rig.id(&main[0]);
    let b2 = rig.id(&side[1]);
    rig.on_v(move |v| {
        let mut ix = v.chain.node_index()?;
        ix.modify(b2, |g| g.status = Status(g.status.bits() | Status::CONFLICT_CHAINLOCK));
        ix.mark_dirty(b2);
        Ok(())
    });
    let stash = replies_at_first_release(&rig, move |v| {
        vec![v.query(move |q| (q.chain.private_tip().map(|(t, _)| t.id), q.idx.is_candidate(b2)))]
    });
    assert_eq!(rig.v.invalidate_block(main[1].hash()).recv(), Ok(Ok(())));
    assert_eq!(take(&stash), [(Some(a1), false)], "b2 has more work than a1, but conflicts");
    assert_eq!(tip(&rig), a1);
    assert_eq!(rig.v.reconsider_block(side[1].hash(), false).recv(), Ok(Ok(())));
    assert!(rig.status(&side[1]).has(Status::CONFLICT_CHAINLOCK));
    assert_eq!(tip(&rig), a1);
    assert_eq!(rig.v.reconsider_block(side[1].hash(), true).recv(), Ok(Ok(())));
    assert!(!rig.status(&side[1]).has(Status::CONFLICT_CHAINLOCK));
    assert_eq!(tip(&rig), b2);
}

/// A block outside the active chain is marked failed with no disconnect; Core still signals
/// `UpdatedBlockTip` (`:4035-4038`) but notifies no UI (`:4042`).
#[test]
fn invalidating_a_block_off_the_active_chain_marks_it_only() {
    let ui = Arc::new(Ui::default());
    let rig = Rig::with(RigConfig {
        ui: Some(ui.clone()),
        ..RigConfig::default()
    });
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    let side = child(&rig.p, &rig.genesis, 1);
    for m in main.iter().chain([&side]) {
        rig.accept(m);
    }
    assert!(ui.v.set(rig.v.clone()).is_ok());
    let a = Announced::watch(&rig);
    let a2 = rig.id(&main[1]);
    assert_eq!(rig.v.invalidate_block(side.hash()).recv(), Ok(Ok(())));
    rig.sync();
    assert_eq!(a.take(), [Tip(a2, None, true)]);
    assert!(ui.seen.lock().expect("ui").is_empty());
    assert_eq!(tip(&rig), a2);
    assert!(failed(&rig, &side));
    assert_eq!(rig.idx(|ix| ix.best_invalid()), Some(rig.id(&side)), "InvalidChainFound");
}

/// `MaybeUpdateMempoolForReorg(disconnectpool, ++disconnected <= 10 && ret)` (`:3965`): after
/// ten disconnects the transactions are no longer added back. No `check` runs, unlike ABC's step.
#[test]
fn a_deep_invalidation_stops_restoring_the_mempool_after_ten_blocks() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let rig = Rig::with(RigConfig {
        mempool: Some(Box::new(Mempool(log.clone()))),
        ..RigConfig::default()
    });
    let main = branch(&rig.p, &rig.genesis, 12, 0);
    for m in &main {
        rig.accept(m);
    }
    log.lock().expect("pool").clear();
    assert_eq!(rig.v.invalidate_block(main[0].hash()).recv(), Ok(Ok(())));
    let want: Vec<Pool> = (1..=12).flat_map(|n| [Pool::New, Pool::Update(n <= 10)]).collect();
    assert_eq!(*log.lock().expect("pool"), want);
}

/// Genesis cannot be invalidated (`validation.cpp:3895`): Core's `false` with a valid state, so
/// the RPC still runs ABC and succeeds. An unknown hash fails each RPC before any work.
#[test]
fn genesis_is_never_invalidated_and_unknown_blocks_are_not_found() {
    let rig = Rig::new();
    let main = branch(&rig.p, &rig.genesis, 1, 0);
    rig.accept(&main[0]);
    let a = Announced::watch(&rig);
    assert_eq!(rig.v.invalidate_block(rig.genesis.hash()).recv(), Ok(Ok(())));
    rig.sync();
    assert_eq!(a.take(), []);
    assert!(!rig.status(&rig.genesis).has(Status::FAILED_MASK));
    assert_eq!(tip(&rig), rig.id(&main[0]));

    let unknown = child(&rig.p, &main[0], 0).hash();
    assert_eq!(rig.v.invalidate_block(unknown).recv(), Ok(Err(BlockNotFound)));
    assert_eq!(rig.v.reconsider_block(unknown, false).recv(), Ok(Err(BlockNotFound)));
    assert_eq!(rig.v.precious_block(unknown).recv(), Ok(Err(BlockNotFound)));
}

/// `reconsiderblock` clears the flags of the block, its descendants and its failed ancestors
/// (`validation.cpp:4152-4231`) and ABC reconnects. A failed ancestor's other `FAILED_CHILD`
/// child is promoted to `FAILED_VALID` (`:4210-4217`) and stays out.
#[test]
fn reconsider_reconnects_and_promotes_a_failed_sibling() {
    let rig = Rig::new();
    let main = branch(&rig.p, &rig.genesis, 3, 0);
    let sibling = child(&rig.p, &main[1], 5);
    for m in main.iter().chain([&sibling]) {
        rig.accept(m);
    }
    let [_, _, a3] = rig.ids_n(&main);
    let x3 = rig.id(&sibling);
    assert_eq!(rig.v.invalidate_block(main[1].hash()).recv(), Ok(Ok(())));
    // x3 had a2's work + 1 and joined the candidates as a2 left; ABC found a2 failed below it.
    assert!(rig.status(&sibling).has(Status::FAILED_CHILD));
    assert!(!rig.idx(move |ix| ix.is_candidate(x3)));
    assert_eq!(rig.idx(|ix| ix.best_invalid()), Some(x3), "FindMostWorkChain (:3549-3551)");

    // The descendant: a3's own flags, then its ancestor a2's, whose FAILED_CHILD child x3 is
    // promoted.
    assert_eq!(rig.v.reconsider_block(main[2].hash(), false).recv(), Ok(Ok(())));
    assert_eq!(tip(&rig), a3);
    for m in &main {
        assert!(!rig.status(m).has(Status::FAILED_MASK), "height {}", m.height);
    }
    let x = rig.status(&sibling);
    assert!(x.has(Status::FAILED_VALID) && x.has(Status::FAILED_CHILD), "promoted");
    assert_eq!(rig.idx(|ix| ix.best_invalid()), Some(x3), "x3 was not reconsidered");
}

/// Reconsidering the block `m_best_invalid` names clears it (`:4186-4189`), and ABC reconnects.
#[test]
fn reconsider_clears_the_best_invalid_block() {
    let rig = Rig::new();
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    for m in &main {
        rig.accept(m);
    }
    let a2 = rig.id(&main[1]);
    assert_eq!(rig.v.invalidate_block(main[1].hash()).recv(), Ok(Ok(())));
    assert_eq!(rig.idx(|ix| ix.best_invalid()), Some(a2));
    assert_eq!(rig.v.reconsider_block(main[1].hash(), false).recv(), Ok(Ok(())));
    assert_eq!(rig.idx(|ix| ix.best_invalid()), None);
    assert_eq!(tip(&rig), a2);
}

/// `preciousblock` (`validation.cpp:3858-3887`): an equal-work block gets the next reverse
/// sequence id and ABC switches to it; a block with less work than the tip changes nothing;
/// once the tip has gained work the counter restarts at -1.
#[test]
fn precious_switches_between_equal_work_tips() {
    let rig = Rig::new();
    let a1 = child(&rig.p, &rig.genesis, 0);
    let b1 = child(&rig.p, &rig.genesis, 1);
    rig.accept(&a1);
    rig.accept(&b1);
    let (a, b) = (rig.id(&a1), rig.id(&b1));
    assert_eq!(tip(&rig), a, "first seen");

    assert_eq!(rig.v.precious_block(b1.hash()).recv(), Ok(Ok(())));
    assert_eq!(tip(&rig), b);
    assert_eq!(rig.v.precious_block(a1.hash()).recv(), Ok(Ok(())));
    assert_eq!(tip(&rig), a);
    let seqs = rig.idx(move |ix| (ix.sequence_id(a), ix.sequence_id(b), ix.reverse_sequence_id()));
    assert_eq!(seqs, (-2, -1, -3));

    let a2 = child(&rig.p, &a1, 0);
    rig.accept(&a2);
    let top = rig.id(&a2);
    // Less work than the tip: the early return (:3864-3867), no counter change.
    assert_eq!(rig.v.precious_block(b1.hash()).recv(), Ok(Ok(())));
    assert_eq!(tip(&rig), top);
    assert_eq!(rig.idx(|ix| ix.reverse_sequence_id()), -3);
    // The tip itself, after the chain gained work: the counter restarts (:3868-3871).
    assert_eq!(rig.v.precious_block(a2.hash()).recv(), Ok(Ok(())));
    let seqs = rig.idx(move |ix| (ix.sequence_id(top), ix.reverse_sequence_id()));
    assert_eq!(seqs, (-1, -2));
    assert_eq!(tip(&rig), top);
}

/// A rig whose block files outlive it, for a restart.
pub(super) fn restartable() -> (Rig, SharedFiles) {
    let files = SharedFiles::default();
    let rig = Rig::with(RigConfig {
        files: Some(Box::new(files.clone())),
        ..RigConfig::default()
    });
    (rig, files)
}

/// V refused to start with DEC-196's model message.
fn assert_refused(restarted: Result<Rig, Fatal>) {
    assert_eq!(restarted.err().map(|f| f.0).as_deref(), Some(INVALID_TIP));
}

/// DEC-196 (1): the index marks the chainstate tip failed (written after `invalidateblock`,
/// the coins before it) and no valid block has its work, so V refuses to start with the model
/// message.
#[test]
fn a_failed_tip_with_more_work_than_any_valid_block_refuses_to_start() {
    let (mut rig, files) = restartable();
    let main = branch(&rig.p, &rig.genesis, 3, 0);
    for m in &main {
        rig.accept(m);
    }
    let a3 = rig.id(&main[2]);
    assert_eq!(rig.v.invalidate_block(main[1].hash()).recv(), Ok(Ok(())));
    let exit = rig.stop();
    assert_refused(Rig::restart(exit, Box::new(files), a3));
}

/// DEC-196 (1) for a ChainLock-conflicting tip, which the load rules never admit either.
#[test]
fn a_conflicting_tip_with_more_work_than_any_valid_block_refuses_to_start() {
    let (mut rig, files) = restartable();
    let main = branch(&rig.p, &rig.genesis, 3, 0);
    for m in &main {
        rig.accept(m);
    }
    let ids = rig.ids(&main);
    let mut exit = rig.stop();
    let mut ix = exit.chain.node_index().expect("node");
    for &id in &ids[1..] {
        ix.modify(id, |g| g.status = Status(g.status.bits() | Status::CONFLICT_CHAINLOCK));
    }
    // The ChainLock reorg's index write left the chain at a1, the coins at a3.
    exit.chain.init_active_tip(ids[0]).expect("a1");
    assert_refused(Rig::restart(exit, Box::new(files), ids[2]));
}

/// DEC-196 (2): a valid block ties the failed chainstate tip. Whichever wins the loaded
/// entries' tie-break (all sequence ids are 0, so the id decides), V keeps it and reorgs to it.
#[test]
fn a_failed_tip_tied_by_a_valid_block_reorgs_to_it() {
    for side_first in [false, true] {
        let (mut rig, files) = restartable();
        let main = branch(&rig.p, &rig.genesis, 2, 0);
        let side = branch(&rig.p, &rig.genesis, 2, 1);
        if side_first {
            // The side's headers first: its ids are lower, so it wins the tie-break.
            rig.headers(&side);
        }
        for m in main.iter().chain(&side) {
            rig.accept(m);
        }
        let (a2, b2) = (rig.id(&main[1]), rig.id(&side[1]));
        assert_eq!(b2 < a2, side_first);
        assert_eq!(rig.v.invalidate_block(main[0].hash()).recv(), Ok(Ok(())));
        assert_eq!(tip(&rig), b2);
        let exit = rig.stop();
        let mut rig = Rig::restart(exit, Box::new(files), a2).expect("V starts");
        assert_eq!(tip(&rig), b2, "side first: {side_first}");
        assert!(failed(&rig, &main[1]));
        rig.stop();
    }
}

/// Forks `[main, side, other]` of `n` blocks from genesis, every block accepted and main's the
/// tip. The side's headers go first and the other's last, so after a load (all sequence ids 0)
/// the side's blocks outrank the main's at the same height, which outrank the other's.
pub(super) fn three_forks(rig: &Rig, n: u32) -> [Vec<Mined>; 3] {
    let forks = [0, 1, 2].map(|salt| branch(&rig.p, &rig.genesis, n, salt));
    let [main, side, other] = &forks;
    for headers in [side, main, other] {
        rig.headers(headers);
    }
    for m in forks.iter().flatten() {
        rig.accept(m);
    }
    forks
}

/// V aborted on `CheckBlockIndex`'s candidate rule (`v:5755`) at entry `id`.
fn assert_candidate_abort(rig: &mut Rig, id: BlockId) {
    let what = "a valid block at least as good as the tip must be a candidate";
    let checked = format!("CheckBlockIndex: {what} at entry {}", id.0);
    assert_eq!(rig.stop().fatal.map(|f| f.0).as_deref(), Some(&*checked));
    assert_eq!(rig.abort.messages(), [checked]);
}

/// DEC-196 (2) keeps every candidate with the failed tip's work, whatever the tie-break between
/// it and the tip, so when the block ABC picks first fails to connect the next is still there
/// (Core's prune would have dropped those the tip outranks, leaving V no candidate, and its tip
/// none, once the first failed).
#[test]
fn a_tied_fallback_survives_the_first_tied_block_failing() {
    let (mut rig, files) = restartable();
    let [main, side, other] = three_forks(&rig, 2);
    let [a2, b2, c2] = [&main[1], &side[1], &other[1]].map(|m| rig.id(m));
    assert!(b2 < a2 && a2 < c2);
    assert_eq!(rig.v.invalidate_block(main[0].hash()).recv(), Ok(Ok(())));
    let exit = rig.stop();
    let cfg = RigConfig {
        connector: Some(Box::new(Rejects([side[1].hash()].into()))),
        ..RigConfig::default()
    };
    let mut rig = Rig::restart_with(exit, Box::new(files), a2, cfg).expect("V starts");
    assert!(failed(&rig, &side[1]), "b2 failed to connect");
    assert_eq!(tip(&rig), c2);
    rig.stop();
}

/// D35: a header-only fork with more work than the failed tip is not a valid block, as Core
/// never makes it a candidate, so V refuses to start (DEC-196 (1)).
#[test]
fn a_header_only_fork_with_more_work_does_not_count_as_valid() {
    let (mut rig, files) = restartable();
    let main = branch(&rig.p, &rig.genesis, 3, 0);
    for m in &main {
        rig.accept(m);
    }
    let a3 = rig.id(&main[2]);
    assert_eq!(rig.v.invalidate_block(main[1].hash()).recv(), Ok(Ok(())));
    rig.headers(&branch(&rig.p, &rig.genesis, 4, 1));
    let exit = rig.stop();
    assert_refused(Rig::restart(exit, Box::new(files), a3));
}

/// D38: after DEC-196 (2), V holds every command but startup's own until that ABC completes, so
/// none sees c2, a candidate ranked below the failed tip a2. The header's `-checkblockindex`
/// would abort V on c2; once the ABC has reorged to b2, it passes, and the `preciousblock`
/// moves the tip to c2.
#[test]
fn commands_wait_for_startups_abc_after_a_tied_fallback() {
    let (mut rig, files) = restartable();
    let [main, side, other] = three_forks(&rig, 2);
    let [a2, b2, c2] = [&main[1], &side[1], &other[1]].map(|m| rig.id(m));
    assert!(b2 < a2 && a2 < c2);
    assert_eq!(rig.v.invalidate_block(main[0].hash()).recv(), Ok(Ok(())));
    let exit = rig.stop();
    let (sent, replies) = std::sync::mpsc::channel();
    let c3 = batch(&[child(&rig.p, &other[1], 2)]);
    let c2_hash = other[1].hash();
    let cfg = RigConfig {
        before_init: Some(Box::new(move |v: &ChainHandle| {
            let headers = v.process_headers(c3, None);
            sent.send((headers, v.precious_block(c2_hash))).expect("the test waits");
        })),
        ..RigConfig::default()
    };
    let mut rig = Rig::restart_with(exit, Box::new(files), a2, cfg).expect("V starts");
    let (headers, precious) = replies.recv().expect("sent before init's ABC");
    let headers = headers.recv().expect("V serves the header");
    assert!(headers.stop.is_none(), "{:?}", headers.stop);
    assert_eq!(precious.recv(), Ok(Ok(())));
    assert_eq!(tip(&rig), c2);
    assert!(rig.stop().fatal.is_none());
}

/// BL-123 (D45 (a)): D38's hold holds commands, never W's events. Before startup's ABC, a
/// query is held while W's clears are answered.
#[test]
fn w_is_answered_while_commands_wait_for_startups_abc() {
    let (mut rig, files) = restartable();
    let [main, ..] = three_forks(&rig, 2);
    let a2 = rig.id(&main[1]);
    assert_eq!(rig.v.invalidate_block(main[0].hash()).recv(), Ok(Ok(())));
    let exit = rig.stop();
    let w = ModelW::default();
    let wm = w.clone();
    let (sent, answers) = std::sync::mpsc::channel();
    let cfg = RigConfig {
        flush: Some(w.site()),
        before_init: Some(Box::new(move |v: &ChainHandle| {
            let held = v.query(|_| ());
            let answer = wm.send(&[], 0).recv_timeout(Duration::from_secs(120));
            sent.send((held.try_recv().is_none(), answer)).expect("the test waits");
        })),
        ..RigConfig::default()
    };
    let mut rig = Rig::restart_with(exit, Box::new(files), a2, cfg).expect("V starts");
    let (held, answer) = answers.recv().expect("sent before init's ABC");
    assert!(held, "the query waited for startup's ABC");
    assert!(matches!(answer, Some(Ok(Ok(_)))), "W was answered: {answer:?}");
    assert!(rig.stop().fatal.is_none());
}

/// D51 with D38: the coins attach while V holds commands for startup's ABC, which needs them.
/// The attach is answered before the ABC is sent, and the ABC's reorg to b2 connects on the
/// attached connector.
#[test]
fn the_coins_attach_while_commands_wait_for_startups_abc() {
    let (mut rig, files) = restartable();
    let [main, ..] = three_forks(&rig, 2);
    let a2 = rig.id(&main[1]);
    assert_eq!(rig.v.invalidate_block(main[0].hash()).recv(), Ok(Ok(())));
    let exit = rig.stop();
    let (w, connects) = (ModelW::default(), Counts::default());
    let attach = coins(&w, &connects);
    let (sent, answers) = std::sync::mpsc::channel();
    let cfg = RigConfig {
        before_init: Some(Box::new(move |v: &ChainHandle| {
            let held = v.query(|_| ());
            let attached = v.attach_coins(attach).recv_timeout(SOON);
            sent.send((held.try_recv().is_none(), attached)).expect("the test waits");
        })),
        ..RigConfig::default()
    };
    let mut rig = Rig::restart_with(exit, Box::new(files), a2, cfg).expect("V starts");
    let (held, attached) = answers.recv().expect("sent before init's ABC");
    assert!(held, "the query waited for startup's ABC");
    assert_eq!(attached, Some(Ok(())));
    assert_eq!(connects.get(), 2, "b1 and b2");
    assert!(rig.stop().fatal.is_none());
}

/// D38: a command held until startup's ABC completed is older than any in the lanes, so a later
/// class C operation's release point serves it first (Sol r4 #2).
#[test]
fn held_commands_run_at_a_deferred_commands_release_point() {
    let mut rig = Rig::new();
    let log = Log::default();
    let (held, lane) = (logged(&log, "held"), logged(&log, "lane"));
    let v = rig.v.clone();
    rig.on_v(move |vv| {
        vv.stage_held(held);
        v.send(lane).expect("V runs");
        vv.release_point()
    });
    // A release point that skipped `held` would leave it to the serve loop, after `lane`.
    assert_eq!(entries(&log), ["held", "lane"]);
    assert!(rig.stop().fatal.is_none());
}

/// D39: when every block that ties the failed tip fails to connect, Core's startup ABC ends on
/// the last block it connected (c1), which is no candidate, and returns without
/// `CheckBlockIndex` (`v:3812`). Core runs on from that valid chain until a block extends it;
/// with `-checkblockindex` it asserts at the next check (`v:5755`, here a header's `v:4737`),
/// and so does V. Both c1 and b1, which ABC connected and then left and which ties c1 and sorts
/// above it, break the rule; V's walk reaches b1 first. (This is Core at `-checklevel` 2 or
/// less: at 3, `VerifyLoadedChainstate` first reconsiders the failed tip, `cs:463-465`.)
#[test]
fn a_tied_fallback_whose_blocks_all_fail_runs_on_from_the_last_connected_block() {
    for check in [true, false] {
        let (mut rig, files) = restartable();
        let [main, side, other] = three_forks(&rig, 2);
        let [a2, b1, c1] = [&main[1], &side[0], &other[0]].map(|m| rig.id(m));
        assert_eq!(rig.v.invalidate_block(main[0].hash()).recv(), Ok(Ok(())));
        let exit = rig.stop();
        let cfg = RigConfig {
            connector: Some(Box::new(Rejects([side[1].hash(), other[1].hash()].into()))),
            no_check_block_index: !check,
            ..RigConfig::default()
        };
        let mut rig = Rig::restart_with(exit, Box::new(files), a2, cfg).expect("V starts");
        assert!(failed(&rig, &side[1]) && failed(&rig, &other[1]));
        assert_eq!(tip(&rig), c1);
        assert_eq!(rig.tables(&[]).2, [], "no candidate, not even the tip");
        let d2 = child(&rig.p, &other[0], 3);
        if check {
            let _ = rig.v.process_headers(batch(&[d2]), None).recv();
            assert_candidate_abort(&mut rig, b1);
        } else {
            rig.accept(&d2);
            assert_eq!(tip(&rig), rig.id(&d2));
            assert!(rig.stop().fatal.is_none());
        }
    }
}

/// DEC-196 (3): a failed chainstate tip with less work than a valid block: Core's prune keeps
/// the better candidates and ABC reorgs.
#[test]
fn a_failed_tip_with_less_work_reorgs_normally() {
    let (mut rig, files) = restartable();
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    let side = branch(&rig.p, &rig.genesis, 3, 1);
    rig.headers(&side);
    for m in &main {
        rig.accept(m);
    }
    let a2 = rig.id(&main[1]);
    assert_eq!(rig.v.invalidate_block(main[0].hash()).recv(), Ok(Ok(())));
    for m in &side {
        rig.accept(m);
    }
    let b3 = rig.id(&side[2]);
    assert_eq!(tip(&rig), b3);
    let exit = rig.stop();
    let mut rig = Rig::restart(exit, Box::new(files), a2).expect("V starts");
    assert_eq!(tip(&rig), b3);
    rig.stop();
}

/// D36's scope (DEC-196 (2)): only a failed tip's tie keeps every tied candidate. A load at a
/// valid tip runs Core's `PruneBlockIndexCandidates` unchanged: of three tied forks, b2, which
/// outranks the tip a2, stays, and c2, which a2 outranks, goes, with no hold (D38).
#[test]
fn a_normal_load_with_tied_candidates_prunes_as_core() {
    let mut rig = Rig::new();
    let [main, side, other] = three_forks(&rig, 2);
    let [a2, b2, c2] = [&main[1], &side[1], &other[1]].map(|m| rig.id(m));
    assert!(b2 < a2 && a2 < c2);
    assert_eq!(tip(&rig), a2);
    let genesis = rig.id(&rig.genesis);
    let VExit {
        mut chain,
        ..
    } = rig.stop();
    // A reload has no active chain yet; at a2 already, `LoadChainTip` returns first (`v:4999-5001`).
    chain.init_active_tip(genesis).expect("genesis");
    let mut idx = NodeBlockIndex::load(&chain);
    assert!(idx.is_candidate(c2), "every tied block is a candidate before the prune");
    load_chain_tip(&mut chain, &mut idx, a2).expect("loads");
    assert_eq!(idx.candidates().collect::<Vec<_>>(), [a2, b2], "worst first");
    assert!(!idx.take_tied_fallback());
}

/// What `read` reads as V closes a reply: its waker reads it, on the thread that closes the
/// reply.
struct ReadAtClose<R> {
    read: Box<dyn Fn() -> R + Send + Sync>,
    seen: std::sync::mpsc::Sender<R>,
}

impl<R: Send + 'static> std::task::Wake for ReadAtClose<R> {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let _ = self.seen.send((self.read)());
    }
}

/// `reply`, polled once so its waker is [`ReadAtClose`]'s, then awaited: its outcome and what
/// `read` read as it closed. The closing thread wakes the waker after `recv` may return, so this
/// waits for its reading too.
fn read_at_close<T, R: Send + 'static>(
    reply: ActorReply<T>,
    read: impl Fn() -> R + Send + Sync + 'static,
) -> impl FnOnce() -> (Result<T, WaitError>, Option<R>) {
    let (seen, at_close) = std::sync::mpsc::channel();
    let waker = std::task::Waker::from(Arc::new(ReadAtClose {
        read: Box::new(read),
        seen,
    }));
    let mut reply = reply;
    let polled = std::pin::Pin::new(&mut reply).poll(&mut std::task::Context::from_waker(&waker));
    assert!(polled.is_pending(), "the reply is open");
    move || {
        let r = reply.recv();
        (r, at_close.recv_timeout(SOON).ok())
    }
}

/// [`read_at_close`] of whether the node's flag was set as `reply` closed.
pub(super) fn flag_at_close<T>(
    reply: ActorReply<T>,
    shutdown: &Arc<LocalShutdown>,
) -> impl FnOnce() -> (Result<T, WaitError>, Option<bool>) {
    let shutdown = shutdown.clone();
    read_at_close(reply, move || shutdown.shutdown_requested())
}

/// [`at_close`]'s outcome: the reply's, then the node's flag, `fImporting` and the report count
/// as it closed.
pub(super) type AtClose<T> = (Result<T, WaitError>, Option<(bool, bool, usize)>);

/// [`read_at_close`] of the node's flag, `fImporting` and the reports in `sink` as `reply`
/// closed.
pub(super) fn at_close<T>(
    reply: ActorReply<T>,
    shutdown: &Arc<LocalShutdown>,
    loading: &Arc<LoadingFlags>,
    sink: &Arc<RecordingAbort>,
) -> impl FnOnce() -> AtClose<T> + use<T> {
    let (shutdown, loading, sink) = (shutdown.clone(), loading.clone(), sink.clone());
    read_at_close(reply, move || {
        (shutdown.shutdown_requested(), loading.importing(), sink.messages().len())
    })
}

/// D55 (Q-B, manager 10-14): a command that goes Fatal disconnects its reply only after
/// `stop_serving`'s sink has run and the node's flag is set, so `Disconnected` means the node is
/// stopping. Each reply's waker reads the flag on V's thread as the reply closes: ABC's (a block
/// whose connect is an `AbortNode`) and a command's queued behind it, with ND1's sink setting
/// the flag and with no sink call (a W that reported its own failure, L-3, so only V's stop sets
/// it); and a misused `attach_coins`.
#[test]
fn a_failed_commands_reply_arrives_after_the_nodes_flag_is_set() {
    let p = regtest();
    let genesis = Mined {
        block: genesis_block(&p),
        height: 0,
    };
    let a1 = child(&p, &genesis, 0);
    for w_reported in [false, true] {
        let w = if w_reported {
            ModelW::default().tripped()
        } else {
            ModelW::default()
        };
        let mut rig = Rig::with(RigConfig {
            connector: Some(Box::new(Breaks {
                at: a1.hash(),
                abort: true,
            })),
            flush: Some(w.site()),
            ..RigConfig::default()
        });
        let resume = pause(&rig.v);
        let processed =
            flag_at_close(rig.v.process_block(a1.block.clone(), true, None, true), &rig.shutdown);
        let behind = flag_at_close(rig.v.activate_best_chain(), &rig.shutdown);
        resume.send(()).expect("V paused");
        assert_eq!(
            processed(),
            (Err(WaitError::Disconnected), Some(true)),
            "W reported {w_reported}"
        );
        assert_eq!(behind(), (Err(WaitError::Disconnected), Some(true)), "W reported {w_reported}");
        assert_eq!(rig.stop().fatal.map(|f| f.0).as_deref(), Some("disk on fire"));
        let sunk: &[&str] = if w_reported {
            &[]
        } else {
            &["disk on fire"]
        };
        assert_eq!(rig.abort.messages(), sunk);
    }

    let mut rig = Rig::new();
    let resume = pause(&rig.v);
    let attached = flag_at_close(
        rig.v.attach_coins(coins(&ModelW::default(), &Counts::default())),
        &rig.shutdown,
    );
    resume.send(()).expect("V paused");
    assert_eq!(attached(), (Err(WaitError::Disconnected), Some(true)));
    assert!(rig.stop().fatal.is_some());
}

/// [`Breaks`] whose failure is one of Core's asserts.
struct Asserts(Breaks);

impl BlockConnector for Asserts {
    fn connect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        checked: bool,
        id: BlockId,
        st: &mut BlockStatusWriter<'_>,
    ) -> Result<ConnectOutcome, Fatal> {
        self.0.connect_block(cx, v, target, block, checked, id, st).map_err(|f| Fatal::assert(f.0))
    }

    fn disconnect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        b: &Block,
        id: BlockId,
    ) -> Result<DisconnectResult, Fatal> {
        self.0.disconnect_block(cx, v, target, b, id)
    }

    fn test_block_validity(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        checks: &dyn ProposalChecks,
        b: &Block,
        prev: BlockId,
        pow: bool,
        merkle: bool,
    ) -> Result<ConnectOutcome, Fatal> {
        self.0.test_block_validity(cx, v, checks, b, prev, pow, merkle)
    }

    fn exit_scheme_guard(&mut self, log: bool) {
        self.0.exit_scheme_guard(log);
    }
}

/// D55 for ABC's reply (BL-472): an `AbortNode` in the ABC answers it `Err` with the state, which
/// the node reads only once its flag is set; one of Core's asserts there ends Core's process, so
/// the reply disconnects, after the flag too.
#[test]
fn an_abcs_abort_node_answers_its_state_after_the_nodes_flag_is_set() {
    for assert in [false, true] {
        let p = regtest();
        let genesis = Mined::genesis(&p);
        let (a1, b1) = (child(&p, &genesis, 0), child(&p, &genesis, 1));
        let breaks = Breaks {
            at: b1.hash(),
            abort: true,
        };
        let connector: Box<dyn BlockConnector> = if assert {
            Box::new(Asserts(breaks))
        } else {
            Box::new(breaks)
        };
        let mut rig = Rig::with(RigConfig {
            connector: Some(connector),
            ..RigConfig::default()
        });
        rig.accept(&a1);
        rig.accept(&b1);
        let id = rig.id(&a1);
        let resume = pause(&rig.v);
        // b1 ties a1: with a1 invalid, the ABC connects b1, whose connect fails.
        let invalidated = ChainCommand::TestUnderV(Box::new(move |v| {
            assert!(v.invalidate_block(id, &mut false)?, "a1 is disconnected");
            Ok(())
        }));
        rig.v.send(invalidated).expect("sent");
        let abc = flag_at_close(rig.v.activate_best_chain(), &rig.shutdown);
        resume.send(()).expect("V paused");
        let want = if assert {
            Err(WaitError::Disconnected)
        } else {
            Ok(Err("disk on fire".to_owned()))
        };
        assert_eq!(abc(), (want, Some(true)), "assert {assert}");
        assert_eq!(rig.stop().fatal.map(|f| f.0).as_deref(), Some("disk on fire"));
        assert_eq!(rig.abort.messages(), ["disk on fire"], "assert {assert}: one report");
    }
}

/// BL-673: `ConnectTip`'s failure line follows a connector's `AbortNode` (Core's `false` after
/// it, `v:3447`) and never one of Core's asserts, which ends Core's process inside
/// `ConnectBlock`. Either way V reports once and stops: the `AbortNode` where it arose, before
/// the line (BL-710), and the assert in `run`, as on V's panic (D62a).
// Core: validation.cpp:3442-3448; util/check.h:49-51; shutdown.cpp:27-38
#[test]
fn a_connect_tip_line_follows_only_an_abort_node() {
    install_accept_log();
    for assert in [false, true] {
        let w = ModelW::default();
        let p = regtest();
        let genesis = Mined::genesis(&p);
        let (a1, b1) = (child(&p, &genesis, 0), child(&p, &genesis, 1));
        let breaks = Breaks {
            at: b1.hash(),
            abort: true,
        };
        let connector: Box<dyn BlockConnector> = if assert {
            Box::new(Asserts(breaks))
        } else {
            Box::new(breaks)
        };
        let mut rig = Rig::with(RigConfig {
            connector: Some(connector),
            flush: Some(w.site()),
            ..RigConfig::default()
        });
        let v = rig.v_thread();
        rig.accept(&a1);
        rig.accept(&b1);
        let id = rig.id(&a1);
        // b1 ties a1: with a1 invalid, the ABC connects b1, whose connect fails.
        let invalidated = ChainCommand::TestUnderV(Box::new(move |v| {
            assert!(v.invalidate_block(id, &mut false)?, "a1 is disconnected");
            Ok(())
        }));
        rig.v.send(invalidated).expect("sent");
        let _ = rig.v.activate_best_chain().recv();
        let fatal = rig.stop().fatal.expect("V stopped on the connect");
        assert_eq!(fatal.0, "disk on fire");
        assert_eq!(fatal.is_abort_node(), !assert, "assert {assert}");
        assert_eq!(rig.abort.messages(), ["disk on fire"], "assert {assert}: one report");
        let tip_line = format!("ConnectTip: ConnectBlock {} failed, disk on fire", b1.hash());
        let lines = lines_on(v);
        let logged = lines.iter().filter(|l| l.starts_with("ConnectTip:")).collect::<Vec<_>>();
        let want = if assert {
            vec![]
        } else {
            vec![&tip_line]
        };
        assert_eq!(logged, want, "assert {assert}: {lines:#?}");
        if assert {
            w.assert_stopped_as_by_an_assert();
        } else {
            // BL-710: V reports the `Fatal` nobody reported where it arose, before the line, as
            // Core's `AbortNode` reports inside `ConnectBlock` (`shutdown.cpp:27-38`).
            assert_logged_before(&lines, "*** disk on fire", &tip_line);
            w.assert_stopped_on_abort_node_reads(3);
        }
    }
}

/// Fails `at`'s connect with `fatal`; every other block connects as [`NullConnector`]'s.
struct FailsWith {
    at: BlockHash,
    fatal: Fatal,
}

impl BlockConnector for FailsWith {
    fn connect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        checked: bool,
        id: BlockId,
        st: &mut BlockStatusWriter<'_>,
    ) -> Result<ConnectOutcome, Fatal> {
        if block.hash() == self.at {
            return Err(self.fatal.clone());
        }
        NullConnector.connect_block(cx, v, target, block, checked, id, st)
    }

    fn disconnect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        b: &Block,
        id: BlockId,
    ) -> Result<DisconnectResult, Fatal> {
        NullConnector.disconnect_block(cx, v, target, b, id)
    }

    fn test_block_validity(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        checks: &dyn ProposalChecks,
        b: &Block,
        prev: BlockId,
        pow: bool,
        merkle: bool,
    ) -> Result<ConnectOutcome, Fatal> {
        NullConnector.test_block_validity(cx, v, checks, b, prev, pow, merkle)
    }
}

/// The `ConnectTip` and `ProcessNewBlock` failure lines for `m`'s connect failing with `msg`.
pub(super) fn connect_lines(m: &Mined, msg: &str) -> (String, String) {
    (
        format!("ConnectTip: ConnectBlock {} failed, {msg}", m.hash()),
        format!("ProcessNewBlock: ActivateBestChain failed ({msg})"),
    )
}

/// BL-710: for each kind of `Fatal` nobody reported that `ConnectBlock` returns (V's unless the
/// latch tripped, V's whatever was reported, and an SB4 contract failure), V reports it where it
/// arose, before `ConnectTip`'s line and `ProcessNewBlock`'s: one report. A contract failure still
/// drops init's command (BL-271).
// Core: validation.cpp:3442-3448, :4910-4912; shutdown.cpp:27-38
#[test]
fn a_connect_tip_line_follows_vs_report_for_each_unreported_class() {
    install_accept_log();
    let contract = Fatal::from(dash_chain::ChainError::Internal("broken in the connect"));
    for (kind, fatal) in [
        ("unless latched", Fatal::new("disk on fire")),
        ("always", Fatal::abort_node("disk on fire")),
        ("contract", contract),
    ] {
        let p = regtest();
        let a1 = child(&p, &Mined::genesis(&p), 0);
        let msg = fatal.0.clone();
        let calls = Arc::new(InitCalls::default());
        let mut rig = Rig::with(RigConfig {
            connector: Some(Box::new(FailsWith {
                at: a1.hash(),
                fatal: fatal.clone(),
            })),
            listener: Some(calls.clone()),
            ..RigConfig::default()
        });
        let v = rig.v_thread();
        let r = rig.v.process_block(a1.block.clone(), true, None, true).recv();
        assert_eq!(r, Err(WaitError::Disconnected), "{kind}");
        if fatal.is_contract() {
            assert_init_dropped(&rig, &calls);
        }
        assert_eq!(rig.stop().fatal.map(|f| f.0), Some(msg.clone()), "{kind}");
        assert_eq!(rig.abort.messages(), [msg.as_str()], "{kind}: one report");
        let lines = lines_on(v);
        let (tip_line, pnb) = connect_lines(&a1, &msg);
        assert_logged_before(&lines, &format!("*** {msg}"), &tip_line);
        assert_logged_before(&lines, &tip_line, &pnb);
        // BL-750: none is an `AbortNode(state, ...)` of Core's `ConnectBlock`, so no
        // `BlockChecked` follows.
        assert_eq!(checked(&rig), [], "{kind}: no BlockChecked");
    }
}

/// BL-750's control for BL-685's other exception: `FreshMisapplied` from the overlay's commit
/// (`connect/connector.rs`'s `ov.commit(best).map_err(coins_failed)`), Core's `std::logic_error`
/// in `view.Flush()` (`v:3457`). The `Fatal` is injected, the commit's own: the real path cannot
/// reach it, as the overlay's add checks the same tip cache its merge reads, and the overlay
/// holds the tip for the whole connect. It stays in the `Err` arm: one report, before
/// `ConnectTip`'s line, and no `BlockChecked` (the `Valid` one Core signals first is BL-685's).
// Core: coins.cpp:217-223; validation.cpp:3443, :3457
#[test]
fn a_fresh_misapplied_commit_checks_no_block() {
    install_accept_log();
    let p = regtest();
    let a1 = child(&p, &Mined::genesis(&p), 0);
    let fatal = Fatal::new(format!("coins: {}", crate::error::CoinsError::FreshMisapplied));
    let msg = fatal.0.clone();
    let mut rig = Rig::with(RigConfig {
        connector: Some(Box::new(FailsWith {
            at: a1.hash(),
            fatal,
        })),
        ..RigConfig::default()
    });
    let v = rig.v_thread();
    let r = rig.v.process_block(a1.block.clone(), true, None, true).recv();
    assert_eq!(r, Err(WaitError::Disconnected));
    assert_eq!(checked(&rig), [], "no BlockChecked");
    assert_eq!(rig.stop().fatal.map(|f| f.0), Some(msg.clone()));
    assert_eq!(rig.abort.messages(), [msg.as_str()], "one report");
    let (tip_line, pnb) = connect_lines(&a1, &msg);
    let lines = lines_on(v);
    assert_logged_before(&lines, &format!("*** {msg}"), &tip_line);
    assert_logged_before(&lines, &tip_line, &pnb);
}

/// BL-684: the ChainLock floor's missing ancestor inside `ConnectBlock` is Core's assert in
/// `HasConflictingChainLock` (`chainlock/chainlock.cpp:94`), which ends Core's process there: no
/// `ConnectTip` line and no `ProcessNewBlock` failure line, one report (V's, of the assert), the
/// reply disconnected, and V stopped as on its panic (D62a).
// Core: chainlock/chainlock.cpp:94; util/check.h:49-51
#[test]
fn a_floor_ancestor_in_connect_block_stops_v_as_cores_assert() {
    install_accept_log();
    let p = regtest();
    let a1 = child(&p, &Mined::genesis(&p), 0);
    let w = ModelW::default();
    let fatal = Fatal::from(dash_chain::ChainError::FloorAncestor);
    let msg = fatal.0.clone();
    let mut rig = Rig::with(RigConfig {
        connector: Some(Box::new(FailsWith {
            at: a1.hash(),
            fatal,
        })),
        flush: Some(w.site()),
        ..RigConfig::default()
    });
    let v = rig.v_thread();
    let r = rig.v.process_block(a1.block.clone(), true, None, true).recv();
    assert_eq!(r, Err(WaitError::Disconnected));
    let stopped = rig.stop().fatal.expect("V stopped on the assert");
    assert_eq!(stopped.0, msg);
    assert!(!stopped.is_abort_node(), "{stopped:?}");
    assert_eq!(rig.abort.messages(), [msg], "one report");
    let lines = lines_on(v);
    assert!(!lines.iter().any(|l| l.starts_with("ConnectTip:")), "{lines:#?}");
    assert!(!lines.iter().any(|l| l.starts_with("ProcessNewBlock:")), "{lines:#?}");
    w.assert_stopped_as_by_an_assert();
}

/// BL-717's rig: ModelW, `-checkblockindex` on (regtest's default, BL-525), and headers A1..A3
/// alone, so each entry is `VALID_TREE` and `AcceptBlockHeader`'s `m_failed_blocks` loop runs on
/// their children. Then `f` on V with A1..A3's ids; A4 (on A3) is the next header. Returns the
/// rig, its W, A4, and how many lines V had logged by then.
fn sb4_assert_rig(
    f: impl FnOnce(&mut V, [BlockId; 3]) -> Result<(), Fatal> + Send + 'static,
) -> (Rig, ModelW, Mined, usize) {
    install_accept_log();
    let w = ModelW::default();
    let rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        ..RigConfig::default()
    });
    let a = branch(&rig.p, &rig.genesis, 4, 0);
    rig.headers(&a[..3]);
    let ids = rig.ids_n(&a[..3]);
    let (send, checks) = std::sync::mpsc::channel();
    rig.on_v(move |v| {
        send.send((v.check_index, v.chain.config().consistency_checks)).expect("the test waits");
        f(v, ids)
    });
    assert_eq!(checks.recv(), Ok((true, true)), "-checkblockindex is on, in V and SB4");
    let lines = lines_on(rig.v_thread());
    // A batch V takes logs the line, so its absence after the stop is the stop's.
    assert!(lines.iter().any(|l| l.starts_with("Synchronizing blockheaders")), "{lines:#?}");
    let logged = lines.len();
    (rig, w, a[3].clone(), logged)
}

/// BL-717's stop: V stopped on SB4's port of Core's assert `what`, as on its panic (D62a): one
/// report, the text SB4's (neither V's own `CheckBlockIndex` nor a panic's), none of `absent`
/// among V's lines since `logged`, and the shutdown flushes refused.
fn assert_sb4_assert_stop(mut rig: Rig, w: &ModelW, logged: usize, what: &str, absent: &[&str]) {
    let v = rig.v_thread();
    let msg = format!("chain: internal: {what}");
    let stopped = rig.stop().fatal.expect("V stopped on the assert");
    assert_eq!(stopped.0, msg);
    assert!(!stopped.is_abort_node(), "{stopped:?}");
    assert_eq!(rig.abort.messages(), [msg], "one report");
    let lines = lines_on(v).split_off(logged);
    for line in absent {
        assert!(!lines.iter().any(|l| l.starts_with(line)), "{line}: {lines:#?}");
    }
    w.assert_stopped_as_by_an_assert();
}

/// BL-717: `CheckBlockIndex` after a header (`validation.cpp:4748`) on an inconsistent index is
/// Core's assert (`:5626-5874`), in every build: SB4's per-header check stops V as on its panic
/// before V's own once-per-batch check, with no `Synchronizing blockheaders` line and the reply
/// disconnected.
// Core: validation.cpp:4748, :5626-5874; util/check.h:49-51
#[test]
fn an_inconsistent_index_in_process_headers_stops_v_as_cores_assert() {
    let (rig, w, a4, logged) = sb4_assert_rig(|v, [_, a2, _]| {
        // Core: "The failed mask cannot be set for blocks without invalid parents".
        v.chain
            .node_index()?
            .modify(a2, |g| g.status = Status(g.status.bits() | Status::FAILED_CHILD));
        Ok(())
    });
    let r = rig.v.process_headers(batch(&[a4]), None).recv();
    assert_eq!(r.map(|_| ()), Err(WaitError::Disconnected));
    let what = "CheckBlockIndex found an inconsistent index";
    assert_sb4_assert_stop(rig, &w, logged, what, &["Synchronizing blockheaders"]);
}

/// BL-717: a block in `m_failed_blocks` without `FAILED_VALID` is Core's assert in
/// `AcceptBlockHeader` (`validation.cpp:4691`), on the header path: V stops as on its panic,
/// with the reply disconnected.
// Core: validation.cpp:4686-4697; util/check.h:49-51
#[test]
fn a_failed_block_without_failed_valid_in_process_headers_stops_v_as_cores_assert() {
    let (rig, w, a4, logged) = sb4_assert_rig(|v, [a1, ..]| {
        v.chain.node_index()?.failed_blocks().insert(a1);
        Ok(())
    });
    let r = rig.v.process_headers(batch(&[a4]), None).recv();
    assert_eq!(r.map(|_| ()), Err(WaitError::Disconnected));
    let what = "m_failed_blocks holds a block not FAILED_VALID";
    assert_sb4_assert_stop(rig, &w, logged, what, &["Synchronizing blockheaders"]);
}

/// BL-717: the same assert in `AcceptBlock`'s `AcceptBlockHeader` (`validation.cpp:4788`) ends
/// Core's process there: no `ProcessNewBlock` line (neither `AcceptBlock FAILED` nor `ACCEPTED`).
// Core: validation.cpp:4691, :4788; util/check.h:49-51
#[test]
fn a_failed_block_without_failed_valid_in_accept_block_stops_v_as_cores_assert() {
    let (rig, w, a4, logged) = sb4_assert_rig(|v, [a1, ..]| {
        v.chain.node_index()?.failed_blocks().insert(a1);
        Ok(())
    });
    let r = rig.v.process_block(a4.block.clone(), true, None, true).recv();
    assert_eq!(r.map(|_| ()), Err(WaitError::Disconnected));
    let what = "m_failed_blocks holds a block not FAILED_VALID";
    assert_sb4_assert_stop(rig, &w, logged, what, &["ProcessNewBlock"]);
}

/// A mempool whose step drain logs `drain add=<add>` on V's thread, then runs its hook.
struct OnDrain<F>(F);

impl<F: FnMut() + Send> MempoolSink for OnDrain<F> {
    fn new_disconnect_pool(&mut self) -> Box<dyn DisconnectPool> {
        Box::new(())
    }

    fn has_mempool(&self) -> bool {
        true
    }

    fn maybe_update_for_reorg_at(
        &mut self,
        _: Box<dyn DisconnectPool>,
        list: Option<&Arc<MnList>>,
    ) {
        // A target the test log captures.
        tracing::info!(target: "dashd_chainstate::chain::abc", "drain add={}", list.is_some());
        (self.0)();
    }

    fn remove_for_block_at(&mut self, _: &[TxRef], _: u32, _: &Arc<MnList>) {}

    fn remove_expired_asset_unlock(&mut self, _: u32) {}

    fn remove_recursive(&mut self, _: &TxRef, _: RemovalReason) {}

    fn add_transactions_updated(&mut self, _: u32) {}

    fn check(&mut self, _: u32) {}
}

/// BL-710: V's report of a `Fatal` from `ConnectBlock` precedes the step's drain, as Core's
/// `AbortNode` inside `ConnectBlock` precedes `ConnectTip`'s line and `ActivateBestChainStep`'s
/// `MaybeUpdateMempoolForReorg(disconnectpool, false)`; `ProcessNewBlock`'s line follows.
// Core: validation.cpp:3442-3448, :3667-3677 (v24.0.0), :4910-4912; shutdown.cpp:27-38
#[test]
fn vs_report_precedes_the_steps_drain() {
    install_accept_log();
    let p = regtest();
    let a1 = child(&p, &Mined::genesis(&p), 0);
    let mut rig = Rig::with(RigConfig {
        connector: Some(Box::new(Breaks {
            at: a1.hash(),
            abort: true,
        })),
        mempool: Some(Box::new(OnDrain(|| {}))),
        ..RigConfig::default()
    });
    let v = rig.v_thread();
    let r = rig.v.process_block(a1.block.clone(), true, None, true).recv();
    assert_eq!(r, Err(WaitError::Disconnected));
    assert!(rig.stop().fatal.is_some());
    assert_eq!(rig.abort.messages(), ["disk on fire"], "one report");
    let lines = lines_on(v);
    let drains = lines.iter().filter(|l| l.starts_with("drain add=")).collect::<Vec<_>>();
    assert_eq!(drains, ["drain add=false"], "{lines:#?}");
    let (tip_line, pnb) = connect_lines(&a1, "disk on fire");
    assert_logged_before(&lines, "*** disk on fire", &tip_line);
    assert_logged_before(&lines, &tip_line, "drain add=false");
    assert_logged_before(&lines, "drain add=false", &pnb);
}

/// BL-710 under a store failure W latched and reported first (L-3): a `Fatal` V reports whatever
/// was reported before it (`Fatal::abort_node`, as `v:3632`'s after `DisconnectBlock`'s) is
/// reported where it arose, after W's report: two reports, W's then V's, both before
/// `ConnectTip`'s line and `ProcessNewBlock`'s.
// Core: validation.cpp:3442-3448, :4910-4912; shutdown.cpp:27-38
#[test]
fn an_always_report_follows_a_latched_store_failures_before_the_lines() {
    const W_FAILED: &str = "Failed to write to coin database";
    install_accept_log();
    let p = regtest();
    let a1 = child(&p, &Mined::genesis(&p), 0);
    // W's failure has latched; W reports it before the connect.
    let w = ModelW::default().tripped();
    let mut rig = Rig::with(RigConfig {
        connector: Some(Box::new(FailsWith {
            at: a1.hash(),
            fatal: Fatal::abort_node("disk on fire"),
        })),
        flush: Some(w.site()),
        ..RigConfig::default()
    });
    let v = rig.v_thread();
    rig.abort.abort_node(W_FAILED, None);
    let r = rig.v.process_block(a1.block.clone(), true, None, true).recv();
    assert_eq!(r, Err(WaitError::Disconnected));
    assert!(rig.stop().fatal.is_some());
    assert_eq!(rig.abort.messages(), [W_FAILED, "disk on fire"], "W's, then V's");
    let lines = lines_of(&[std::thread::current().id(), v]);
    let (tip_line, pnb) = connect_lines(&a1, "disk on fire");
    assert_logged_before(&lines, &format!("*** {W_FAILED}"), "*** disk on fire");
    assert_logged_before(&lines, "*** disk on fire", &tip_line);
    assert_logged_before(&lines, &tip_line, &pnb);
}

/// W's report of the store failure B-710f's drain stages.
const DRAIN_W_REPORTED: &str = "System error while flushing: injected W write";

/// BL-710 and R-1: V's latch read where `ConnectBlock`'s `Fatal` arose finds the latch open, so V
/// reports it there; W's store failure then latches in the step's drain, before the operation's
/// exit, and W reports it. Two failures, each reported once (L-3), in the order of their reports:
/// V's before `ConnectTip`'s line and the drain, W's after them, and neither again at the
/// operation's exit or in `run` (V's marked reported, and the latch tripped by then).
/// `ProcessNewBlock`'s line follows both.
// Core: validation.cpp:3442-3448, :3667-3677 (v24.0.0), :4910-4912; shutdown.cpp:27-38
#[test]
fn a_store_failure_latched_after_vs_read_is_reported_after_vs_report() {
    install_accept_log();
    let p = regtest();
    let a1 = child(&p, &Mined::genesis(&p), 0);
    let latch = GlobalFailStop::new();
    let sink = Arc::new(RecordingAbort::default());
    let w = Arc::new(Mutex::new(None));
    // R-1's later trip: W's store failure latches in the drain, after V's latch read at the fault,
    // on W's own thread, and W reports it.
    let (w_latch, w_sink, w_ran) = (latch.clone(), sink.clone(), w.clone());
    let trips_w = OnDrain(move || {
        let failure = Failure {
            kind: dashd_store::ErrorKind::Io,
            site: "chainstate write",
            message: "injected W write".to_owned(),
        };
        let report = DRAIN_W_REPORTED.to_owned();
        *w_ran.lock().expect("w") = Some(fail_on_w(&w_latch, w_sink.clone(), failure, report));
    });
    let mut rig = Rig::with(RigConfig {
        connector: Some(Box::new(Breaks {
            at: a1.hash(),
            abort: true,
        })),
        mempool: Some(Box::new(trips_w)),
        flush: Some(Box::new(Latched(latch))),
        abort: Some(sink.clone()),
        ..RigConfig::default()
    });
    let v = rig.v_thread();
    let r = rig.v.process_block(a1.block.clone(), true, None, true).recv();
    assert_eq!(r, Err(WaitError::Disconnected));
    let w = w.lock().expect("w").take().expect("the drain ran");
    let w_thread = w.thread().id();
    w.join().expect("W reports");
    assert!(rig.stop().fatal.is_some());
    assert_eq!(sink.messages(), ["disk on fire", DRAIN_W_REPORTED], "V's, then W's; no repeat");
    let lines = lines_of(&[w_thread, v]);
    let (tip_line, pnb) = connect_lines(&a1, "disk on fire");
    assert_logged_before(&lines, "*** disk on fire", &tip_line);
    assert_logged_before(&lines, &tip_line, "drain add=false");
    assert_logged_before(&lines, "drain add=false", &format!("*** {DRAIN_W_REPORTED}"));
    assert_logged_before(&lines, &format!("*** {DRAIN_W_REPORTED}"), &pnb);
}

/// D55 on success (Opus m-3): a reply that a step drops unsent disconnects once that step
/// returns `Ok`, while V serves on, not when V exits.
#[test]
fn a_reply_dropped_by_a_step_that_succeeds_disconnects_after_the_step() {
    let mut rig = Rig::new();
    let (unsent, dropped) = reply::<()>();
    rig.v
        .send(ChainCommand::TestUnderV(Box::new(move |_| {
            drop(unsent);
            Ok(())
        })))
        .expect("sent");
    let resume = pause(&rig.v);
    assert_eq!(dropped.recv_timeout(SOON), Some(Err(WaitError::Disconnected)), "V is paused");
    resume.send(()).expect("V paused");
    assert!(rig.stop().fatal.is_none());
}

/// D55 for D38's held commands (Opus m-3): a command held for startup's ABC when a step goes
/// Fatal disconnects its reply after the node's flag is set.
#[test]
fn a_held_commands_reply_disconnects_after_the_nodes_flag_is_set() {
    let mut rig = Rig::new();
    let (tx, held) = reply::<()>();
    let held = flag_at_close(held, &rig.shutdown);
    rig.v
        .send(ChainCommand::TestUnderV(Box::new(move |vv| {
            vv.stage_held(ChainCommand::TestUnderV(Box::new(move |_| {
                tx.send(());
                Ok(())
            })));
            Err(Fatal::new("disk on fire"))
        })))
        .expect("sent");
    assert_eq!(held(), (Err(WaitError::Disconnected), Some(true)));
    assert_eq!(rig.stop().fatal.map(|f| f.0).as_deref(), Some("disk on fire"));
}

/// D55 for the lanes (Opus m-3): a command still in a lane when a step goes Fatal drops on the
/// reaper, which gets the lanes only after the node's flag is set. W has reported its own
/// failure, so no sink call sets the flag: only V's stop does.
#[test]
fn a_queued_commands_reply_disconnects_on_the_reaper_after_the_nodes_flag_is_set() {
    let w = ModelW::default().tripped();
    let mut rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        ..RigConfig::default()
    });
    let resume = pause(&rig.v);
    let failing = ChainCommand::TestUnderV(Box::new(|_| Err(Fatal::new("disk on fire"))));
    rig.v.send(failing).expect("sent");
    let queued = flag_at_close(rig.v.query(|_| ()), &rig.shutdown);
    resume.send(()).expect("V paused");
    assert_eq!(queued(), (Err(WaitError::Disconnected), Some(true)));
    assert_eq!(rig.stop().fatal.map(|f| f.0).as_deref(), Some("disk on fire"));
    assert_eq!(rig.abort.messages(), [] as [&str; 0], "no sink call");
}

/// D55's panics (Opus m-3): a reply dropped as a step panics, or as a Fatal step's sink panics,
/// disconnects after the node's flag is set: V's stop sets it if the sink did not.
#[test]
fn a_reply_dropped_as_a_step_or_sink_panics_disconnects_after_the_nodes_flag_is_set() {
    for (step_panics, sink_panics) in [(true, false), (true, true), (false, true)] {
        let case = format!("step panics {step_panics}, sink panics {sink_panics}");
        let mut rig = Rig::with(RigConfig {
            abort: sink_panics.then(|| Arc::new(PanickingAbort) as Arc<dyn AbortSink>),
            ..RigConfig::default()
        });
        let (tx, failed) = reply::<()>();
        let failed = flag_at_close(failed, &rig.shutdown);
        rig.v
            .send(ChainCommand::TestUnderV(Box::new(move |_| {
                let _unsent = tx;
                if step_panics {
                    panic!("V's step failed");
                }
                Err(Fatal::new("disk on fire"))
            })))
            .expect("sent");
        assert_eq!(failed(), (Err(WaitError::Disconnected), Some(true)), "{case}");
        let joined = rig.join.take().expect("running").join().expect("an unmarked joiner");
        assert!(joined.is_err(), "the panic reaches V's joiner: {case}");
        if !sink_panics {
            assert_eq!(rig.abort.messages().len(), 1, "{case}");
        }
    }
}

/// Pauses V until the returned sender sends.
pub(super) fn pause(v: &ChainHandle) -> std::sync::mpsc::Sender<()> {
    let (resume, paused) = std::sync::mpsc::channel();
    let on_v = ChainCommand::TestUnderV(Box::new(move |_| {
        paused.recv().expect("the test resumes V");
        Ok(())
    }));
    v.send(on_v).expect("V runs");
    resume
}

/// O2b-4 P3b's risk check (T1): the iterations' marks are the only ones the walked blocks get, so
/// with the plant only the invalidated block reaches the index writes (`validation.cpp:3990-3991`),
/// while all three are `FAILED_VALID` in memory either way.
#[cfg(feature = "planted-bugs")]
#[test]
fn p3b_skips_the_walked_blocks_dirty_marks() {
    let dirty_after = |plant: Option<crate::Planted>| {
        let rig = Rig::with(RigConfig::default());
        let main = branch(&rig.p, &rig.genesis, 4, 0);
        for m in &main {
            rig.accept(m);
        }
        rig.on_v(|v| {
            v.chain.node_index()?.take_dirty();
            Ok(())
        });
        rig.v.plant(plant);
        assert_eq!(rig.v.invalidate_block(main[1].hash()).recv(), Ok(Ok(())));
        rig.sync();
        for m in &main[1..] {
            assert!(rig.status(m).has(Status::FAILED_VALID), "{plant:?}: failed in memory");
        }
        let got = Arc::new(Mutex::new(Vec::new()));
        let g = got.clone();
        rig.on_v(move |v| {
            let writes = v.chain.node_index()?.take_dirty();
            g.lock().expect("got").extend(writes.into_iter().map(|w| w.hash));
            Ok(())
        });
        let mut got = std::mem::take(&mut *got.lock().expect("got"));
        got.sort();
        let want = |ms: &[&Mined]| {
            let mut w: Vec<BlockHash> = ms.iter().map(|m| m.hash()).collect();
            w.sort();
            w
        };
        (got, want(&[&main[1], &main[2], &main[3]]), want(&[&main[1]]))
    };
    let (unplanted, walked, _) = dirty_after(None);
    assert_eq!(unplanted, walked, "each walked block marked once, nothing else");
    let (planted, _, target) = dirty_after(Some(crate::Planted::P3b));
    assert_eq!(planted, target, "P3b: only the target is marked");
}

/// O2b-4 P3b's arming is per operation: P3b armed at the first iteration's release, while the
/// walk still has blocks to disconnect, changes none of that invalidate's later iterations, which
/// keep the plant (none) it read at its start.
#[cfg(feature = "planted-bugs")]
#[test]
fn arming_p3b_mid_invalidate_keeps_the_walk_s_plant() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let rig = Rig::with(RigConfig::default());
    let main = branch(&rig.p, &rig.genesis, 4, 0);
    for m in &main {
        rig.accept(m);
    }
    rig.on_v(|v| {
        v.chain.node_index()?.take_dirty();
        Ok(())
    });
    let armed = Arc::new(AtomicBool::new(false));
    let a = armed.clone();
    at_first_release(&rig, move |v| {
        v.plant(Some(crate::Planted::P3b));
        a.store(true, Ordering::SeqCst);
    });
    assert_eq!(rig.v.invalidate_block(main[1].hash()).recv(), Ok(Ok(())));
    rig.sync();
    assert!(armed.load(Ordering::SeqCst), "armed inside the walk");
    let got = Arc::new(Mutex::new(Vec::new()));
    let g = got.clone();
    rig.on_v(move |v| {
        let writes = v.chain.node_index()?.take_dirty();
        g.lock().expect("got").extend(writes.into_iter().map(|w| w.hash));
        Ok(())
    });
    let mut got = std::mem::take(&mut *got.lock().expect("got"));
    got.sort();
    let mut walked: Vec<BlockHash> = main[1..].iter().map(|m| m.hash()).collect();
    walked.sort();
    assert_eq!(got, walked, "the walk ran unplanted to its end");
}

/// BL-781: a coins read fault is an `AbortNode` to V's stops, its flush side, a release point and
/// an admission, as a reported store fault is, and only its callers' lines tell it apart: Core's
/// process aborts in the read (`coins.cpp:356-369` at v24.0.0; CH3b D214, D231).
#[test]
fn a_coins_read_fault_is_an_abort_node_after_which_no_caller_logs() {
    use crate::chain::flush::AbortCause;

    let read = Fatal::read_failed("coins read: injected");
    let store = Fatal::reported("coins read: injected");
    for (f, case) in [(&read, "read"), (&store, "store")] {
        assert_eq!(f.cause(), AbortCause::AbortNode, "{case}");
        assert!(f.is_abort_node() && f.carried_past() && f.is_reported(), "{case}");
        assert!(!f.is_contract() && !f.is_reported_assert(), "{case}");
        assert_eq!(f.reported_by_v(false), None, "{case}");
        let own = f.clone().own();
        assert!(own.is_abort_node() && !own.carried_past(), "{case}: V's own at an admission");
    }
    assert!(read.is_read() && !read.caller_logs());
    assert!(!store.is_read() && store.caller_logs());
    assert!(read.clone().into_reported().is_read(), "V's stop keeps the class");
    assert!(!Fatal::assert("x").caller_logs());
}

/// BL-781: an assert's line is reported as any assert while the latch is open, and once a report
/// has tripped the latch is the stop's to log, not to report (CH3b D214).
#[test]
fn an_asserts_line_is_reported_unless_latched() {
    let line = Fatal::assert_line("Chainstate::EnforceBlock -- MarkConflictingBlock failed: Valid");
    let plain = Fatal::assert("Chainstate::EnforceBlock -- MarkConflictingBlock failed: Valid");
    for (f, case) in [(&line, "line"), (&plain, "plain")] {
        assert!(!f.is_abort_node() && !f.is_reported(), "{case}");
        assert_eq!(f.reported_by_v(false), Some(f.0.as_str()), "{case}: the latch open");
        assert_eq!(f.reported_by_v(true), None, "{case}: the latch tripped");
        assert!(f.clone().latched_unreported().reported_by_v(true).is_some(), "{case}");
        assert!(f.clone().into_reported().is_reported_assert(), "{case}");
    }
}
