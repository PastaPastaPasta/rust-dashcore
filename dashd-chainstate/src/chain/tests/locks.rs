//! ChainLocks on V (N5, §5): intake, record and the mirror with M1 (V checks M1 after every
//! command, so each test also runs it), enforcement with its captured pair, the tip task, and
//! InstantSend's block conflicts (§5.6).
//!
//! The locks' signature bytes are arbitrary: V never reads them (D41), LK1a verifies them.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use dash_chain::Status;
use dash_consensus::BlockId;
use dash_num::Hash256;
use dash_primitives::BlockHash;
use dash_types::Numeric as _;

use super::abc::Q::{Connected, Disconnected, Tip};
use super::abc::{Announced, Breaks, Mempool, Pool, adversarial};
use super::failure::{RefusesDisconnect, tip};
use super::flush::ModelW;
use super::stop::DisconnectFails;
use super::support::{
    Mined, Rig, RigConfig, accept_levels, assert_logged_before, batch, branch, child,
    install_accept_log, lines_on,
};
use crate::chain::actor::Deferred;
use crate::chain::actor::tests::Stage;
use crate::chain::reply::{ActorReply, reply};
use crate::chain::{
    BlockConnector, ChainCommand, ChainHandle, ChainLockSig, ClReply, ClSigs, ClTrigger,
    IsConflicts, IsResolution, SyncState, UiNotifications, ValidationInterface, WaitError,
};

const SOON: Duration = Duration::from_secs(120);

/// A lock on `m`'s block claiming `height`.
fn lock_at(m: &Mined, height: i32) -> Arc<ChainLockSig> {
    Arc::new(ChainLockSig {
        height,
        block_hash: m.hash(),
        sig: [height as u8; 96],
    })
}

/// A lock on `m` at its height.
pub(super) fn lock(m: &Mined) -> Arc<ChainLockSig> {
    lock_at(m, m.height as i32)
}

/// The CLSIG's hash (P2b's `::SerializeHash(clsig)`): here any value distinct per lock.
fn sig_hash(sig: &ChainLockSig) -> Hash256 {
    let mut b = *sig.block_hash.as_bytes();
    b[0] ^= 0x5a;
    b[1] ^= sig.height as u8;
    Hash256::from_lendian(b)
}

/// The ChainLock notifications, queued and UI, and the UI's `NotifyBlockTip`s.
#[derive(Default)]
struct ClLog {
    queued: Mutex<Vec<(BlockId, Arc<ChainLockSig>)>>,
    ui: Mutex<Vec<(BlockHash, i32)>>,
    tips: Mutex<Vec<BlockHash>>,
}

impl ClLog {
    fn take(&self) -> Vec<(BlockId, Arc<ChainLockSig>)> {
        std::mem::take(&mut *self.queued.lock().expect("queued"))
    }
}

impl ValidationInterface for ClLog {
    fn notify_chain_lock(&self, id: BlockId, sig: &Arc<ChainLockSig>) {
        self.queued.lock().expect("queued").push((id, sig.clone()));
    }
}

impl UiNotifications for ClLog {
    fn notify_block_tip(&self, _: SyncState, hash: BlockHash, _: u32) {
        self.tips.lock().expect("tips").push(hash);
    }

    fn notify_chain_lock(&self, hash: BlockHash, height: i32) {
        self.ui.lock().expect("ui").push((hash, height));
    }
}

/// A rig with SPORK_19 on, which V observes at its first `CheckActiveState`, and `log` as its
/// listener and UI.
fn cl_rig(cfg: RigConfig) -> (Rig, Arc<ClLog>) {
    let log = Arc::new(ClLog::default());
    let rig = Rig::with(RigConfig {
        chainlocks: true,
        ui: Some(log.clone()),
        ..cfg
    });
    rig.signals.register(log.clone());
    (rig, log)
}

pub(super) fn intake(rig: &Rig) -> i32 {
    rig.v.cl_intake().recv().expect("V replies").best_height
}

/// `RecordChainLock`; whether to relay.
pub(super) fn record(rig: &Rig, sig: &Arc<ChainLockSig>) -> bool {
    let r = rig.v.record_chainlock(sig.clone(), sig_hash(sig), None).recv();
    r.expect("V replies").relay
}

fn cl(rig: &Rig) -> Arc<ClSigs> {
    rig.v.view_handle().cl()
}

fn floor(rig: &Rig) -> Option<BlockId> {
    rig.v.view().snap.cl_floor.map(|f| f.id)
}

/// Waits until V has run its deferred work and the signals thread has run what that queued. A
/// query is served at any release point, so the barrier is class C, repeated until it finds
/// nothing deferred: a release point may defer it ahead of a task it queues.
pub(super) fn settle(rig: &Rig) {
    loop {
        let idle = Arc::new(AtomicBool::new(false));
        let i = idle.clone();
        rig.on_v(move |v| {
            i.store(v.deferred_len() == 0, Ordering::SeqCst);
            Ok(())
        });
        if idle.load(Ordering::SeqCst) {
            break;
        }
    }
    rig.sync();
}

/// `notified` as (block, the signature's height).
fn heights(notified: &[(BlockId, Arc<ChainLockSig>)]) -> Vec<(BlockId, i32)> {
    notified.iter().map(|(id, s)| (*id, s.height)).collect()
}

/// §9.4 (f) `clsig_getdata_zero`: with no lock recorded, `GetChainLockByHash(0)` is the null sig
/// (Core answers `getdata(MSG_CLSIG, 0)` with a `clsig`); a recorded lock answers only for its
/// own hash; after an off → on reset, 0 answers again.
#[test]
fn clsig_getdata_zero() {
    let (rig, _) = cl_rig(RigConfig::default());
    let main = branch(&rig.p, &rig.genesis, 1, 0);
    rig.accept(&main[0]);
    let null = cl(&rig).by_hash(&Hash256::ZERO).expect("the null sig");
    assert_eq!(*null, ChainLockSig::null());
    assert_eq!(
        null.to_string(),
        format!("ChainLockSig(nHeight=-1, blockHash={})", BlockHash::from([0; 32]))
    );
    assert_eq!(cl(&rig).best_height(), -1);
    let a1 = lock(&main[0]);
    assert_eq!(cl(&rig).by_hash(&sig_hash(&a1)), None);

    assert_eq!(intake(&rig), -1, "off → on: Core's reset");
    // At or below the null sig's −1: stale, whatever block it names (`cl:110-113`).
    assert!(!record(&rig, &lock_at(&main[0], -1)));
    assert_eq!(cl(&rig).best, None);
    assert!(record(&rig, &a1));
    let c = cl(&rig);
    assert_eq!(c.by_hash(&Hash256::ZERO), None);
    assert_eq!(c.by_hash(&sig_hash(&a1)), Some(a1.clone()));
    assert_eq!(c.best_height(), 1);
    assert_eq!(rig.v.view().best_chainlock(), Some(&a1));

    let sporks = rig.sporks.as_ref().expect("a fresh rig");
    sporks.set(false);
    assert_eq!(intake(&rig), 1, "on → off clears nothing");
    assert!(!cl(&rig).enabled_observed);
    sporks.set(true);
    assert_eq!(intake(&rig), -1);
    let c = cl(&rig);
    assert_eq!(
        *c,
        ClSigs {
            best: None,
            best_hash: Hash256::ZERO,
            best_known: None,
            enabled_observed: true,
        }
    );
    assert_eq!(c.by_hash(&Hash256::ZERO).as_deref(), Some(&ChainLockSig::null()));
    assert_eq!(floor(&rig), None);
}

/// A UI whose ChainLock notification waits until the test opens it.
#[derive(Default)]
struct GatedUi {
    open: Mutex<bool>,
    opened: Condvar,
    entered: AtomicBool,
}

impl GatedUi {
    fn open(&self) {
        *self.open.lock().expect("gate") = true;
        self.opened.notify_all();
    }
}

impl UiNotifications for GatedUi {
    fn notify_chain_lock(&self, _: BlockHash, _: i32) {
        self.entered.store(true, Ordering::SeqCst);
        let open = self.open.lock().expect("gate");
        let (open, _) = self.opened.wait_timeout_while(open, SOON, |open| !*open).expect("gate");
        assert!(*open, "the test never opened the gate");
    }
}

/// §9.4 (g) `recorded_enforce_after_reply`: `RecordChainLock` for a known block on an idle V
/// replies (relay) with the lock and its floor published, before the `ClEnforce` it queued runs:
/// that enforcement waits in the UI here, so a reply sent after it would never arrive.
#[test]
fn recorded_enforce_after_reply() {
    let ui = Arc::new(GatedUi::default());
    let rig = Rig::with(RigConfig {
        chainlocks: true,
        ui: Some(ui.clone()),
        ..RigConfig::default()
    });
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    for m in &main {
        rig.accept(m);
    }
    intake(&rig);
    let a2 = lock(&main[1]);
    let reply = rig.v.record_chainlock(a2.clone(), sig_hash(&a2), None);
    let Some(reply) = reply.recv_timeout(SOON) else {
        ui.open();
        panic!("RecordChainLock replied only after its enforcement");
    };
    assert_eq!(
        reply,
        Ok(ClReply {
            relay: true,
        })
    );
    // The scope published before the reply.
    assert_eq!(cl(&rig).best_known, Some(a2));
    assert_eq!(floor(&rig), Some(rig.id(&main[1])));
    ui.open();
    settle(&rig);
    assert!(ui.entered.load(Ordering::SeqCst), "the queued enforcement ran and notified");
}

/// `UpdateBestChainlock`'s outcomes through the mirror (§5.1, §5.3): a pending lock and a height
/// mismatch move the best lock only; a stale one moves nothing and is not relayed; a known block
/// at its height moves the floor and queues the enforcement, which notifies once.
#[test]
fn record_outcomes_move_the_mirror_as_core() {
    let (rig, log) = cl_rig(RigConfig::default());
    let main = branch(&rig.p, &rig.genesis, 7, 0);
    for m in &main {
        rig.accept(m);
    }
    let unknown = child(&rig.p, &main[6], 9);
    assert_eq!(intake(&rig), -1);

    let pending = lock_at(&unknown, 5);
    assert!(record(&rig, &pending));
    let c = cl(&rig);
    assert_eq!(
        (c.best.clone(), c.best_hash, c.best_known.clone()),
        (Some(pending.clone()), sig_hash(&pending), None)
    );
    assert_eq!(floor(&rig), None);
    assert_eq!(intake(&rig), 5);

    for stale in
        [lock_at(&main[4], 5), lock_at(&main[2], 3), lock_at(&main[0], -1), lock_at(&main[0], -7)]
    {
        assert!(!record(&rig, &stale), "{stale}");
        assert_eq!(cl(&rig).best, Some(pending.clone()));
    }

    // a2 claims height 6: the best lock moves, unrelayed; the floor and known pair stay.
    let mismatch = lock_at(&main[1], 6);
    assert!(!record(&rig, &mismatch));
    let c = cl(&rig);
    assert_eq!(
        (c.best.clone(), c.best_hash, c.best_known.clone()),
        (Some(mismatch.clone()), sig_hash(&mismatch), None)
    );
    assert_eq!(floor(&rig), None);

    let a7 = lock(&main[6]);
    assert!(record(&rig, &a7));
    let c = cl(&rig);
    assert_eq!((c.best.clone(), c.best_known.clone()), (Some(a7.clone()), Some(a7.clone())));
    let id7 = rig.id(&main[6]);
    assert_eq!(floor(&rig), Some(id7));
    settle(&rig);
    assert_eq!(log.take(), [(id7, a7.clone())]);
    assert_eq!(*log.ui.lock().expect("ui"), [(main[6].hash(), 7)]);

    // The timer re-enforces the same pair: Core's `lastNotify` dedupe.
    rig.v.chainlock_timer().expect("V runs");
    settle(&rig);
    assert_eq!(log.take(), []);
}

/// `AcceptedBlockHeader` (`cl:152-172`, §5.3): a pending lock's header arriving in a batch moves
/// the floor and the known pair right after `ProcessHeaders`, before any enforcement, also when
/// the batch then stops; a header at another height than the lock claims moves nothing.
#[test]
fn a_late_header_moves_the_known_lock() {
    let (rig, log) = cl_rig(RigConfig::default());
    let main = branch(&rig.p, &rig.genesis, 4, 0);
    rig.accept(&main[0]);
    intake(&rig);
    let a1 = lock(&main[0]);
    assert!(record(&rig, &a1));
    settle(&rig);
    log.take();

    // A lock claiming height 9 for a3: when a3 arrives at height 3, the old pair stays.
    let wrong = lock_at(&main[2], 9);
    assert!(record(&rig, &wrong));
    rig.headers(&main[1..3]);
    let c = cl(&rig);
    assert_eq!((c.best.clone(), c.best_known.clone()), (Some(wrong), Some(a1)));
    assert_eq!(floor(&rig), Some(rig.id(&main[0])));

    // A lock on b12, whose batch then fails on an orphan: the pair moves anyway.
    let side = branch(&rig.p, &main[0], 11, 1);
    let b12 = lock(&side[10]);
    assert!(record(&rig, &b12));
    assert_eq!(cl(&rig).best_known.as_ref().map(|s| s.height), Some(1));
    let mut headers = side.clone();
    headers.push(child(&rig.p, &child(&rig.p, &side[10], 2), 2));
    let r = rig.v.process_headers(batch(&headers), None).recv().expect("V replies");
    assert!(r.stop.is_some(), "the orphan stops the batch");
    assert_eq!(cl(&rig).best_known, Some(b12));
    assert_eq!(floor(&rig), Some(rig.id(&side[10])));
    settle(&rig);
    assert_eq!(log.take(), [], "no enforcement ran");
}

/// The locked header first arrives inside `ProcessBlock` (`AcceptBlockHeader`), whether the
/// block is then accepted or not.
#[test]
fn a_late_header_inside_a_block_moves_the_known_lock() {
    let (rig, _) = cl_rig(RigConfig::default());
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    rig.accept(&main[0]);
    intake(&rig);
    let a2 = lock(&main[1]);
    assert!(record(&rig, &a2));
    assert_eq!(cl(&rig).best_known, None);
    rig.accept(&main[1]);
    assert_eq!(cl(&rig).best_known, Some(a2));
    assert_eq!(floor(&rig), Some(rig.id(&main[1])));
}

/// Enforcement (§5.2, §5.4): the lock on a lower-work branch marks the active chain's sibling
/// conflicting, disconnecting it a block at a time, then activates the locked branch and
/// notifies once with the captured signature. Each mark updates the mempool once whether it
/// disconnected or not, and signals `UpdatedBlockTip(tip, null)` after publication (N1); only a
/// mark that moved the tip notifies the UI.
#[test]
fn enforcement_marks_the_conflicting_chain_and_activates_the_lock() {
    let pool = Arc::new(Mutex::new(Vec::new()));
    let (rig, log) = cl_rig(RigConfig {
        mempool: Some(Box::new(Mempool(pool.clone()))),
        ..RigConfig::default()
    });
    let main = branch(&rig.p, &rig.genesis, 3, 0);
    let side = branch(&rig.p, &rig.genesis, 2, 1);
    let other = branch(&rig.p, &rig.genesis, 1, 2);
    for m in main.iter().chain(&side).chain(&other) {
        rig.accept(m);
    }
    let g = rig.id(&rig.genesis);
    let [a1, a2, a3] = rig.ids_n(&main);
    let [b1, b2] = rig.ids_n(&side);
    assert_eq!(tip(&rig), a3);
    intake(&rig);
    let _adv = adversarial(&rig);
    let a = Announced::watch(&rig);
    pool.lock().expect("pool").clear();
    log.tips.lock().expect("tips").clear();

    let lock_b2 = lock(&side[1]);
    assert!(record(&rig, &lock_b2));
    settle(&rig);
    assert_eq!(tip(&rig), b2);
    for m in main.iter().chain(&other) {
        assert!(rig.status(m).has(Status::CONFLICT_CHAINLOCK), "{}", m.height);
    }
    assert!(!rig.status(&side[0]).has(Status::CONFLICT_CHAINLOCK));
    // Regtest a day past genesis: IBD throughout.
    let ibd = true;
    assert_eq!(
        a.take(),
        [
            Disconnected(a3),
            Disconnected(a2),
            Disconnected(a1),
            Tip(g, None, ibd),
            Tip(g, None, ibd),
            Connected(b1),
            Tip(b1, Some(g), ibd),
            Connected(b2),
            Tip(b2, Some(b1), ibd),
        ]
    );
    assert_eq!(log.take(), [(b2, lock_b2.clone())]);
    // Marks in id order (D9): a1 (disconnects), then c1 (in no chain).
    assert_eq!(
        pool.lock().expect("pool")[..4],
        [Pool::New, Pool::Update(true), Pool::New, Pool::Update(true)]
    );
    // a1's mark moved the tip; c1's, in no chain, notifies nothing.
    let tips = [&rig.genesis, &side[0], &side[1]].map(|m| m.hash());
    assert_eq!(*log.tips.lock().expect("tips"), tips);
    assert_eq!(rig.idx(move |ix| ix.candidates().collect::<Vec<_>>()), [b2]);
}

/// EnforceBlock on a failed locked block (`v:4079-4082`): `ResetBlockFailureFlags` with the lock
/// token, then V's node half puts the reset chain back in the candidates and ABC reaches it.
#[test]
fn enforcement_resets_an_invalidated_locked_chain() {
    let (rig, log) = cl_rig(RigConfig::default());
    let main = branch(&rig.p, &rig.genesis, 3, 0);
    for m in &main {
        rig.accept(m);
    }
    assert_eq!(rig.v.invalidate_block(main[0].hash()).recv(), Ok(Ok(())));
    assert_eq!(tip(&rig), rig.id(&rig.genesis));
    intake(&rig);
    let a2 = lock(&main[1]);
    assert!(record(&rig, &a2));
    settle(&rig);
    assert_eq!(tip(&rig), rig.id(&main[2]));
    for m in &main {
        assert!(!rig.status(m).has(Status::FAILED_MASK), "{}", m.height);
    }
    assert_eq!(log.take(), [(rig.id(&main[1]), a2)]);
}

/// Outside IBD, a lock on a block whose data is missing (`h:299-309`): enforcement marks the
/// active sibling (`v:4059-4068`), ABC cannot reach the floor, and nothing is notified; when the
/// block connects, ABC's `UpdatedBlockTip` queues the tip task, which notifies once.
#[test]
fn a_lock_on_a_missing_block_notifies_when_it_connects() {
    let (rig, log) = cl_rig(RigConfig {
        after_genesis: Some(60),
        ..RigConfig::default()
    });
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    let b2 = child(&rig.p, &main[0], 1);
    rig.accept(&main[0]);
    rig.headers(&main[1..]);
    rig.accept(&b2);
    assert_eq!(tip(&rig), rig.id(&b2));
    intake(&rig);
    let a2 = lock(&main[1]);
    assert!(record(&rig, &a2));
    settle(&rig);
    assert!(rig.status(&b2).has(Status::CONFLICT_CHAINLOCK));
    assert_eq!(tip(&rig), rig.id(&main[0]));
    assert_eq!(log.take(), []);
    rig.accept(&main[1]);
    settle(&rig);
    assert_eq!(log.take(), [(rig.id(&main[1]), a2)]);
}

/// Outside IBD, a mark's and `InvalidateBlock`'s `UpdatedBlockTip(tip, null)` queue the tip task
/// too (`v:4147`, `v:4042`): here no ABC moves the tip after them.
#[test]
fn marks_and_invalidations_queue_the_tip_task_outside_ibd() {
    let non_ibd = || {
        cl_rig(RigConfig {
            after_genesis: Some(60),
            ..RigConfig::default()
        })
    };
    let (rig, _) = non_ibd();
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    for m in &main {
        rig.accept(m);
    }
    let a2 = rig.id(&main[1]);
    rig.on_v(move |v| {
        let n = v.deferred_len();
        assert!(v.invalidate_block(a2)?);
        assert!(v.cl.tip_scheduled, "InvalidateBlock's final scope");
        assert_eq!(v.deferred_len(), n + 1);
        Ok(())
    });

    let (rig, _) = non_ibd();
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    let side = branch(&rig.p, &rig.genesis, 1, 1);
    for m in &main {
        rig.accept(m);
    }
    // b1's data is missing, so the enforcement's ABC moves nothing after the mark.
    rig.headers(&side);
    intake(&rig);
    let b1 = lock(&side[0]);
    let g = rig.id(&rig.genesis);
    rig.on_v(move |v| {
        let (tx, _) = reply();
        v.record_chainlock(b1.clone(), sig_hash(&b1), None, tx)?;
        let n = v.deferred_len();
        v.enforce_best_chainlock()?;
        assert_eq!(v.tip(), Some(g), "a1's mark disconnected a2 and a1");
        assert!(v.cl.tip_scheduled, "the mark's UpdatedBlockTip");
        assert_eq!(v.deferred_len(), n + 1);
        Ok(())
    });
}

/// A failed `DisconnectTip` inside a mark is Core's `assert(false)` (`v:4070-4074`): V aborts
/// with Core's line after giving the mark's pool back unadded, as on its panic (D62a).
#[test]
fn a_failed_disconnect_inside_a_mark_aborts() {
    let refused = Arc::new(OnceLock::new());
    let pool = Arc::new(Mutex::new(Vec::new()));
    let w = ModelW::default();
    let (mut rig, _) = cl_rig(RigConfig {
        flush: Some(w.site()),
        connector: Some(Box::new(RefusesDisconnect(refused.clone()))),
        mempool: Some(Box::new(Mempool(pool.clone()))),
        ..RigConfig::default()
    });
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    let side = branch(&rig.p, &rig.genesis, 1, 1);
    for m in main.iter().chain(&side) {
        rig.accept(m);
    }
    refused.set(main[1].hash()).expect("once");
    intake(&rig);
    pool.lock().expect("pool").clear();
    assert!(record(&rig, &lock(&side[0])));
    let exit = rig.stop();
    assert_eq!(
        exit.fatal.map(|f| f.0).as_deref(),
        Some("Chainstate::EnforceBlock -- MarkConflictingBlock failed: Valid")
    );
    assert_eq!(*pool.lock().expect("pool"), [Pool::New, Pool::Update(false)]);
    w.assert_stopped_as_by_an_assert();
}

/// CH3b D214: an `AbortNode` under the mark's `DisconnectTip` ends in EnforceBlock's assert too
/// (`v:4070-4073`): V stops where it arises and reports it (D213), the mark's pool goes back
/// unadded (`v:4110`), and Core's line carries the `AbortNode`'s message.
#[test]
fn an_abort_node_inside_a_mark_is_asserted_after_its_report() {
    install_accept_log();
    let p = super::support::regtest();
    let genesis = Mined::genesis(&p);
    let (main, side) = (branch(&p, &genesis, 2, 0), branch(&p, &genesis, 1, 1));
    let pool = Arc::new(Mutex::new(Vec::new()));
    let w = ModelW::default();
    let (rig, _) = cl_rig(RigConfig {
        flush: Some(w.site()),
        connector: Some(Box::new(DisconnectFails(main[1].hash()))),
        mempool: Some(Box::new(Mempool(pool.clone()))),
        ..RigConfig::default()
    });
    for m in main.iter().chain(&side) {
        rig.accept(m);
    }
    intake(&rig);
    pool.lock().expect("pool").clear();
    assert!(record(&rig, &lock(&side[0])));
    // The enforcement V deferred runs before it would serve this.
    assert_eq!(rig.v.query(|_| ()).recv_timeout(SOON), Some(Err(WaitError::Disconnected)));
    assert_eq!(*pool.lock().expect("pool"), [Pool::New, Pool::Update(false)]);
    let line = "Chainstate::EnforceBlock -- MarkConflictingBlock failed: disk on fire";
    assert_asserted_after_the_report(rig, &w, line);
}

/// §9.4's ChainLock schedule (a) (R1-M2): enforcement captures b2 and its ABC starts; at a
/// release point `ClIntake` and `RecordChainLock(b3)` run. The notification is still b2's, with
/// b2's signature (`h:283`, `h:318`), and b3's enforcement follows.
#[test]
fn the_captured_pair_survives_a_lock_recorded_during_the_activation() {
    let (rig, log) = cl_rig(RigConfig::default());
    let main = branch(&rig.p, &rig.genesis, 4, 0);
    let side = branch(&rig.p, &rig.genesis, 3, 1);
    for m in main.iter().chain(&side) {
        rig.accept(m);
    }
    intake(&rig);
    let [_, b2, b3] = rig.ids_n(&side);
    let (lock_b2, lock_b3) = (lock(&side[1]), lock(&side[2]));
    let posted = Arc::new(Mutex::new(Vec::new()));
    let (v, p, l3) = (rig.v.clone(), posted.clone(), lock_b3.clone());
    let once = AtomicBool::new(false);
    rig.on_v(move |vv| {
        vv.release_hook = Some(Arc::new(move |stage| {
            // The first hold is the activation's: enforcement's own scope has none.
            if stage == Stage::Release && !once.swap(true, Ordering::SeqCst) {
                let intake = v.cl_intake();
                let rec = v.record_chainlock(l3.clone(), sig_hash(&l3), None);
                p.lock().expect("posted").push((intake, rec));
            }
        }));
        Ok(())
    });
    assert!(record(&rig, &lock_b2));
    settle(&rig);
    let (intake_r, rec_r) = posted.lock().expect("posted").pop().expect("posted at the release");
    assert_eq!(intake_r.recv().expect("V").best_height, 2, "b2 was the best lock");
    assert!(rec_r.recv().expect("V").relay);
    assert_eq!(heights(&log.take()), [(b2, 2), (b3, 3)]);
    assert_eq!(tip(&rig), b3);
    assert_eq!(cl(&rig).best_known, Some(lock_b3));
}

/// Schedule (b)'s reset: an off → on `CheckActiveState` between the activation's steps clears
/// the mirror and `lastNotify`; the running enforcement still notifies with its captured pair,
/// as Core's.
#[test]
fn a_reset_during_the_activation_keeps_the_captured_notification() {
    let (rig, log) = cl_rig(RigConfig::default());
    let main = branch(&rig.p, &rig.genesis, 4, 0);
    let side = branch(&rig.p, &rig.genesis, 3, 1);
    for m in main.iter().chain(&side) {
        rig.accept(m);
    }
    intake(&rig);
    let b3 = rig.id(&side[2]);
    let sporks = rig.sporks.clone().expect("a fresh rig");
    let posted = Arc::new(Mutex::new(Vec::new()));
    let (v, p) = (rig.v.clone(), posted.clone());
    let releases = std::sync::atomic::AtomicU32::new(0);
    rig.on_v(move |vv| {
        vv.release_hook = Some(Arc::new(move |stage| {
            // b1's and b2's steps: each intake runs at the release point after them.
            if stage == Stage::Release {
                let n = releases.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    sporks.set(n == 1);
                    p.lock().expect("posted").push(v.cl_intake());
                }
            }
        }));
        Ok(())
    });
    let lock_b3 = lock(&side[2]);
    assert!(record(&rig, &lock_b3));
    settle(&rig);
    let seen: Vec<_> =
        posted.lock().expect("posted").drain(..).map(|r| r.recv().expect("V")).collect();
    assert_eq!(seen.iter().map(|r| r.best_height).collect::<Vec<_>>(), [3, -1]);
    assert_eq!(log.take(), [(b3, lock_b3)]);
    assert_eq!(tip(&rig), b3);
    assert_eq!(cl(&rig).best, None);
    assert_eq!(floor(&rig), None);
}

/// Core's reset clears `lastNotify` (`h:217`): the same lock recorded again after an off → on
/// flip notifies again.
#[test]
fn a_reset_clears_the_last_notification() {
    let (rig, log) = cl_rig(RigConfig::default());
    let main = branch(&rig.p, &rig.genesis, 1, 0);
    rig.accept(&main[0]);
    intake(&rig);
    let a1 = lock(&main[0]);
    let id = rig.id(&main[0]);
    assert!(record(&rig, &a1));
    settle(&rig);
    assert_eq!(log.take(), [(id, a1.clone())]);
    let sporks = rig.sporks.as_ref().expect("a fresh rig");
    sporks.set(false);
    intake(&rig);
    sporks.set(true);
    assert_eq!(intake(&rig), -1);
    assert!(record(&rig, &a1));
    settle(&rig);
    assert_eq!(log.take(), [(id, a1)]);
}

/// The tip task (`h:185-207`, D12): an `UpdatedBlockTip` outside IBD queues one `ClEnforce
/// { TipScheduled }` while none is queued, and the task clears the flag; in IBD it queues none.
/// The task's `CheckActiveState` is what shows it ran.
#[test]
fn updated_block_tip_queues_one_enforcement_outside_ibd() {
    for after_genesis in [None, Some(60)] {
        let (rig, _) = cl_rig(RigConfig {
            after_genesis,
            ..RigConfig::default()
        });
        let ibd = after_genesis.is_none();
        let main = branch(&rig.p, &rig.genesis, 2, 0);
        rig.accept(&main[0]);
        intake(&rig);
        assert!(record(&rig, &lock(&main[0])));
        settle(&rig);
        let sporks = rig.sporks.as_ref().expect("a fresh rig");
        sporks.set(false);
        intake(&rig);
        sporks.set(true);
        rig.accept(&main[1]);
        settle(&rig);
        assert_eq!(cl(&rig).best.is_none(), !ibd, "ibd {ibd}: the task reset the locks");
        rig.on_v(|v| {
            assert!(!v.cl.tip_scheduled);
            let n = v.deferred_len();
            v.cl_schedule_tip();
            v.cl_schedule_tip();
            assert!(v.cl.tip_scheduled);
            assert_eq!(v.deferred_len(), n + 1);
            Ok(())
        });
        settle(&rig);
        rig.on_v(|v| {
            assert!(!v.cl.tip_scheduled, "the task cleared its flag");
            Ok(())
        });
    }
}

/// A timer or recorded task leaves the tip task's flag alone.
#[test]
fn only_the_tip_task_clears_its_flag() {
    let (rig, _) = cl_rig(RigConfig::default());
    let flag = |rig: &Rig, set: bool| {
        let seen = Arc::new(AtomicBool::new(false));
        let s = seen.clone();
        rig.on_v(move |v| {
            s.store(v.cl.tip_scheduled, Ordering::SeqCst);
            v.cl.tip_scheduled = set;
            Ok(())
        });
        seen.load(Ordering::SeqCst)
    };
    flag(&rig, true);
    rig.v.chainlock_timer().expect("V runs");
    settle(&rig);
    assert!(flag(&rig, true), "Timer");
    rig.on_v(|v| {
        v.defer(Deferred::Command(ChainCommand::ClEnforce {
            source: ClTrigger::Recorded,
        }));
        Ok(())
    });
    settle(&rig);
    assert!(flag(&rig, false), "Recorded");
}

/// InstantSend's half of `ResolveBlockConflicts` (§5.6), scripted.
#[derive(Clone, Default)]
struct FakeIs {
    conflicts: Vec<BlockHash>,
    has_tx: bool,
    calls: Arc<Mutex<Vec<String>>>,
}

impl FakeIs {
    fn log(&self, call: String) {
        self.calls.lock().expect("calls").push(call);
    }
}

impl IsConflicts for FakeIs {
    fn retrieve_conflicts(&mut self, lock: &Hash256) -> Vec<BlockHash> {
        self.log(format!("retrieve {lock}"));
        self.conflicts.clone()
    }

    fn has_tx_for_lock(&mut self, _: &Hash256) -> bool {
        self.log("has_tx".into());
        self.has_tx
    }

    fn clear_conflicting(&mut self, _: &Hash256, block: &BlockHash) {
        self.log(format!("clear {block}"));
    }

    fn remove_conflicting_lock(&mut self, _: &Hash256) {
        self.log("remove".into());
    }
}

const ISLOCK: Hash256 = Hash256::from_bendian([7; 32]);
const TXID: Hash256 = Hash256::from_bendian([8; 32]);

/// The production stand-in reports no conflict, and nothing changes.
#[test]
fn no_instantsend_reports_no_conflict() {
    let rig = Rig::new();
    rig.accept(&branch(&rig.p, &rig.genesis, 1, 0)[0]);
    let before = tip(&rig);
    assert_eq!(rig.v.is_conflict_invalidate(ISLOCK, TXID).recv(), Ok(IsResolution::NoConflict));
    assert_eq!(tip(&rig), before);
}

/// A rig over a1..a3 and b1..b2 with `is` scripted to report `conflicts`.
fn is_rig(
    conflicts: impl Fn(&[Mined], &[Mined]) -> Vec<BlockHash>,
    has_tx: bool,
) -> (Rig, Vec<Mined>, Vec<Mined>, FakeIs) {
    let p = super::support::regtest();
    let genesis = Mined {
        block: crate::chain::genesis_block(&p),
        height: 0,
    };
    let main = branch(&p, &genesis, 3, 0);
    let side = branch(&p, &genesis, 2, 1);
    let is = FakeIs {
        conflicts: conflicts(&main, &side),
        has_tx,
        ..FakeIs::default()
    };
    let (rig, _) = cl_rig(RigConfig {
        is: Some(Box::new(is.clone())),
        ..RigConfig::default()
    });
    for m in main.iter().chain(&side) {
        rig.accept(m);
    }
    intake(&rig);
    (rig, main, side, is)
}

/// A conflict mined into a ChainLocked block: ChainLocks win, the lock goes and no block is
/// invalidated (`is:587-606`). With SPORK_19 off `HasChainLock` is false, so the block goes.
#[test]
fn a_chainlocked_conflict_removes_the_instantsend_lock() {
    let (rig, main, _, is) = is_rig(|m, _| vec![m[0].hash()], false);
    assert!(record(&rig, &lock(&main[1])));
    settle(&rig);
    is.calls.lock().expect("calls").clear();
    assert_eq!(rig.v.is_conflict_invalidate(ISLOCK, TXID).recv(), Ok(IsResolution::LockRemoved));
    assert_eq!(*is.calls.lock().expect("calls"), [format!("retrieve {ISLOCK}"), "remove".into()]);
    assert_eq!(tip(&rig), rig.id(&main[2]));
    assert!(!rig.status(&main[0]).has(Status::FAILED_MASK));

    rig.sporks.as_ref().expect("a fresh rig").set(false);
    let r = rig.v.is_conflict_invalidate(ISLOCK, TXID).recv();
    assert_eq!(
        r,
        Ok(IsResolution::Invalidated {
            activated: false,
        })
    );
}

/// Without the lock's transaction each conflicting block is invalidated, then reset, and no ABC
/// runs: the tip stays below it until the next activation (`is:611-631`).
#[test]
fn conflicts_without_the_transaction_are_invalidated_then_reset() {
    let (rig, main, _, is) = is_rig(|m, _| vec![m[1].hash()], false);
    is.calls.lock().expect("calls").clear();
    let r = rig.v.is_conflict_invalidate(ISLOCK, TXID).recv();
    assert_eq!(
        r,
        Ok(IsResolution::Invalidated {
            activated: false,
        })
    );
    assert_eq!(
        *is.calls.lock().expect("calls"),
        [format!("retrieve {ISLOCK}"), "has_tx".into(), format!("clear {}", main[1].hash()),]
    );
    assert_eq!(tip(&rig), rig.id(&main[0]));
    assert!(!rig.status(&main[1]).has(Status::FAILED_MASK));
    assert!(!rig.status(&main[2]).has(Status::FAILED_MASK));
    assert_eq!(rig.v.activate_best_chain().recv(), Ok(Ok(())));
    assert_eq!(tip(&rig), rig.id(&main[2]));
}

/// Two conflicts, one off the active chain: each is cleared, then invalidated and reset, in
/// turn; the tip ends below the first.
#[test]
fn each_conflict_is_invalidated_and_reset_in_turn() {
    let (rig, main, side, is) = is_rig(|m, s| vec![m[1].hash(), s[0].hash()], false);
    is.calls.lock().expect("calls").clear();
    let r = rig.v.is_conflict_invalidate(ISLOCK, TXID).recv();
    assert_eq!(
        r,
        Ok(IsResolution::Invalidated {
            activated: false,
        })
    );
    assert_eq!(
        *is.calls.lock().expect("calls"),
        [
            format!("retrieve {ISLOCK}"),
            "has_tx".into(),
            format!("clear {}", main[1].hash()),
            format!("clear {}", side[0].hash()),
        ]
    );
    assert_eq!(tip(&rig), rig.id(&main[0]));
    for m in main.iter().chain(&side) {
        assert!(!rig.status(m).has(Status::FAILED_MASK), "{}", m.height);
    }
}

/// With the lock's transaction the blocks stay invalid and ABC moves past them (`is:625-641`).
#[test]
fn conflicts_with_the_transaction_are_invalidated_and_activated_past() {
    let (rig, main, side, _) = is_rig(|m, _| vec![m[1].hash()], true);
    let r = rig.v.is_conflict_invalidate(ISLOCK, TXID).recv();
    assert_eq!(
        r,
        Ok(IsResolution::Invalidated {
            activated: true,
        })
    );
    assert_eq!(tip(&rig), rig.id(&side[1]));
    assert!(rig.status(&main[1]).has(Status::FAILED_VALID));
    assert!(rig.status(&main[2]).has(Status::FAILED_MASK));
}

/// A conflict in a block V does not know is a broken contract: V aborts.
#[test]
fn a_conflict_in_an_unknown_block_aborts() {
    let (mut rig, ..) =
        is_rig(|m, _| vec![child(&super::support::regtest(), &m[2], 9).hash()], false);
    let _ = rig.v.is_conflict_invalidate(ISLOCK, TXID).recv();
    let fatal = rig.stop().fatal.expect("V aborted").0;
    assert!(fatal.starts_with("an InstantSend conflict in unknown block"), "{fatal}");
}

/// `CheckActiveState` inside `ClEnforce` (`h:273-277`): an on → off flip the timer is first to
/// see is published before the task returns.
#[test]
fn an_off_flip_seen_by_enforcement_is_published() {
    let (rig, _) = cl_rig(RigConfig::default());
    intake(&rig);
    assert!(rig.v.view().snap.cl_enabled_observed);
    rig.sporks.as_ref().expect("a fresh rig").set(false);
    rig.v.chainlock_timer().expect("V runs");
    settle(&rig);
    assert!(!cl(&rig).enabled_observed);
    assert!(!rig.v.view().snap.cl_enabled_observed);
}

/// `InvalidateBlock` returning `false` for a conflict is Core's `assert(false)` (`is:620-624`):
/// here its disconnect fails, and V aborts with Core's line and drops the reply, as on its
/// panic (D62a).
#[test]
fn a_failed_invalidation_of_a_conflict_aborts() {
    let refused = Arc::new(OnceLock::new());
    let p = super::support::regtest();
    let genesis = Mined {
        block: crate::chain::genesis_block(&p),
        height: 0,
    };
    let main = branch(&p, &genesis, 2, 0);
    let is = FakeIs {
        conflicts: vec![main[1].hash()],
        ..FakeIs::default()
    };
    let w = ModelW::default();
    let (mut rig, _) = cl_rig(RigConfig {
        flush: Some(w.site()),
        is: Some(Box::new(is)),
        connector: Some(Box::new(RefusesDisconnect(refused.clone()))),
        ..RigConfig::default()
    });
    for m in &main {
        rig.accept(m);
    }
    refused.set(main[1].hash()).expect("once");
    let reply = rig.v.is_conflict_invalidate(ISLOCK, TXID);
    // V aborts on its own, before any shutdown could make InvalidateBlock return early.
    let dropped = reply.recv_timeout(SOON).expect("V answers or aborts in time");
    assert!(dropped.is_err(), "the reply is dropped");
    let fatal = rig.stop().fatal.expect("V aborted").0;
    assert_eq!(fatal, "NetInstantSend::ResolveBlockConflicts -- InvalidateBlock failed: Valid");
    w.assert_stopped_as_by_an_assert();
}

/// CH3b D214: an `AbortNode` under `InvalidateBlock` for a conflict (`is:620-624`) ends in Core's
/// assert too: V stops where it arises and reports it (D213), and Core's line carries its message.
#[test]
fn an_abort_node_under_a_conflicts_invalidation_is_asserted_after_its_report() {
    an_abort_node_under_resolve_is_asserted("InvalidateBlock", false, |main, _| {
        Box::new(DisconnectFails(main[2].hash()))
    });
}

/// CH3b D214: as above, for the `ActivateBestChain` past the conflicts (`is:634-641`), whose
/// connect of b1 fails.
#[test]
fn an_abort_node_under_the_activation_past_conflicts_is_asserted_after_its_report() {
    an_abort_node_under_resolve_is_asserted("ActivateBestChain", true, |_, side| {
        Box::new(Breaks {
            at: side[0].hash(),
            abort: true,
        })
    });
}

/// `ResolveBlockConflicts` for a conflict in a2, over a1..a3 and b1..b2, with `connector` failing
/// `call` with an `AbortNode`. V reports it once, before Core's line, whose state is the
/// `AbortNode`'s message (`v:2245-2249`). The assert then stops V as on its panic (D62a): the flush
/// side hears `Panic` after the `AbortNode`, the latch trips and the shutdown flushes are refused.
fn an_abort_node_under_resolve_is_asserted(
    call: &str,
    has_tx: bool,
    connector: impl FnOnce(&[Mined], &[Mined]) -> Box<dyn BlockConnector>,
) {
    install_accept_log();
    let p = super::support::regtest();
    let genesis = Mined::genesis(&p);
    let (main, side) = (branch(&p, &genesis, 3, 0), branch(&p, &genesis, 2, 1));
    let is = FakeIs {
        conflicts: vec![main[1].hash()],
        has_tx,
        ..FakeIs::default()
    };
    let w = ModelW::default();
    let (rig, _) = cl_rig(RigConfig {
        flush: Some(w.site()),
        is: Some(Box::new(is)),
        connector: Some(connector(&main, &side)),
        ..RigConfig::default()
    });
    for m in main.iter().chain(&side) {
        rig.accept(m);
    }
    let reply = rig.v.is_conflict_invalidate(ISLOCK, TXID);
    let dropped = reply.recv_timeout(SOON).expect("V answers or aborts in time");
    assert!(dropped.is_err(), "{call}: the reply is dropped");
    let line = format!("NetInstantSend::ResolveBlockConflicts -- {call} failed: disk on fire");
    assert_asserted_after_the_report(rig, &w, &line);
}

/// CH3b D214 once V has stopped on "disk on fire" and Core's assert after it: the `AbortNode`
/// reported once, `line` logged after the report, and the flush side as after such an assert.
fn assert_asserted_after_the_report(mut rig: Rig, w: &ModelW, line: &str) {
    let thread = rig.v_thread();
    assert_eq!(rig.stop().fatal.map(|f| f.0).as_deref(), Some(line));
    assert_eq!(rig.abort.messages(), ["disk on fire"], "one report");
    assert_logged_before(&lines_on(thread), "*** disk on fire", line);
    w.assert_asserted_after_an_abort_node();
}

/// AcceptedBlockHeader's mismatch (`cl:156-164`): the best lock's header arriving at another
/// height than the lock claims logs Core's line, in a batch or inside a block, and moves nothing.
#[test]
fn a_late_header_at_another_height_logs_cores_line() {
    install_accept_log();
    let (rig, _) = cl_rig(RigConfig {
        after_genesis: Some(600),
        ..RigConfig::default()
    });
    // Salts no other test mines with, as the log is the process's.
    let main = branch(&rig.p, &rig.genesis, 2, 5_151);
    rig.accept(&main[0]);
    intake(&rig);
    let m = &main[1];
    let wrong = lock_at(m, 7);
    assert!(record(&rig, &wrong));
    rig.headers(std::slice::from_ref(m));
    let line = format!(
        "Chainlocks::AcceptedBlockHeader -- height of CLSIG ({wrong}) does not match the \
         specified block's height ({})",
        m.height
    );
    assert_eq!(accept_levels(&line), [tracing::Level::INFO], "in a batch");
    assert_eq!(cl(&rig).best_known, None);
    assert_eq!(floor(&rig), None);
    let b = child(&rig.p, &main[0], 5_152);
    let wrong = lock_at(&b, 9);
    assert!(record(&rig, &wrong));
    rig.accept(&b);
    let line = format!(
        "Chainlocks::AcceptedBlockHeader -- height of CLSIG ({wrong}) does not match the \
         specified block's height ({})",
        b.height
    );
    assert_eq!(accept_levels(&line), [tracing::Level::INFO], "inside ProcessBlock");
}

/// The published status of the probe, the published tip, and the candidates query.
type Probed = (Status, BlockId, ActorReply<Vec<BlockId>>);

/// The mark's UI tip (`v:4152-4154`) on V's thread: the published status of `probe` then, and
/// the candidates a query sees at the next release point, ABC's first (`v:3759`).
struct ProbeUi {
    v: OnceLock<ChainHandle>,
    at: BlockHash,
    probe: OnceLock<BlockId>,
    seen: Mutex<Vec<Probed>>,
}

impl UiNotifications for ProbeUi {
    fn notify_block_tip(&self, _: SyncState, hash: BlockHash, _: u32) {
        let (Some(v), Some(&probe)) = (self.v.get(), self.probe.get()) else {
            return;
        };
        if hash == self.at && self.seen.lock().expect("seen").is_empty() {
            let status = v.reader().entry(probe).expect("indexed").status;
            let published = v.view().snap.tip.tip;
            let candidates = v.query(|q| q.idx.candidates().collect());
            self.seen.lock().expect("seen").push((status, published, candidates));
        }
    }
}

/// A mark's statuses are published, and its disconnected tips and the marked block are out of
/// the candidates (`v:4122-4129`), before anything else runs: what the release point at the
/// start of the enforcement's ABC serves sees Core's state, not the one `FindMostWorkChain`
/// would tidy later.
#[test]
fn a_marks_state_is_visible_at_the_next_release_point() {
    let p = super::support::regtest();
    let genesis = Mined {
        block: crate::chain::genesis_block(&p),
        height: 0,
    };
    let main = branch(&p, &genesis, 3, 0);
    let side = branch(&p, &genesis, 2, 1);
    let ui = Arc::new(ProbeUi {
        v: OnceLock::new(),
        at: genesis.hash(),
        probe: OnceLock::new(),
        seen: Mutex::new(Vec::new()),
    });
    let rig = Rig::with(RigConfig {
        chainlocks: true,
        ui: Some(ui.clone()),
        ..RigConfig::default()
    });
    ui.v.set(rig.v.clone()).ok().expect("once");
    for m in main.iter().chain(&side) {
        rig.accept(m);
    }
    ui.probe.set(rig.id(&main[0])).expect("once");
    intake(&rig);
    assert!(record(&rig, &lock(&side[1])));
    settle(&rig);
    let (status, published, candidates) =
        ui.seen.lock().expect("seen").pop().expect("a1's mark notified");
    assert!(status.has(Status::CONFLICT_CHAINLOCK), "published before the release");
    assert_eq!(published, rig.id(&genesis), "the view shows the mark's tip (N1)");
    let mut candidates = candidates.recv().expect("served at ABC's release point");
    candidates.sort();
    // The new tip is a candidate too (`v:4135-4143`).
    assert_eq!(candidates, rig.ids([&genesis].into_iter().chain(&side)));
}

/// A later mark erases a block an earlier mark's rescan re-added (`v:4122-4129`): with a1 the
/// tip and b1, c1 its siblings, a lock on c1's child marks a1 (re-adding b1 and c1 at genesis),
/// then b1.
#[test]
fn a_later_mark_erases_a_re_added_candidate() {
    let p = super::support::regtest();
    let genesis = Mined {
        block: crate::chain::genesis_block(&p),
        height: 0,
    };
    let [a1, b1, c1] = [0, 1, 2].map(|salt| child(&p, &genesis, salt));
    let c2 = child(&p, &c1, 3);
    let ui = Arc::new(ProbeUi {
        v: OnceLock::new(),
        at: genesis.hash(),
        probe: OnceLock::new(),
        seen: Mutex::new(Vec::new()),
    });
    let rig = Rig::with(RigConfig {
        chainlocks: true,
        ui: Some(ui.clone()),
        ..RigConfig::default()
    });
    ui.v.set(rig.v.clone()).ok().expect("once");
    for m in [&a1, &b1, &c1] {
        rig.accept(m);
    }
    rig.headers(std::slice::from_ref(&c2));
    assert_eq!(tip(&rig), rig.id(&a1));
    ui.probe.set(rig.id(&a1)).expect("once");
    intake(&rig);
    assert!(record(&rig, &lock(&c2)));
    settle(&rig);
    assert!(rig.status(&b1).has(Status::CONFLICT_CHAINLOCK));
    let (_, published, candidates) =
        ui.seen.lock().expect("seen").pop().expect("a1's mark notified");
    assert_eq!(published, rig.id(&genesis));
    let mut candidates = candidates.recv().expect("served at ABC's release point");
    candidates.sort();
    assert_eq!(candidates, rig.ids([&genesis, &c1]));
}

/// With SPORK_19 off, enforcement does nothing (`h:275-277`): the floor that stays from before
/// the flip marks nothing.
#[test]
fn enforcement_is_off_while_the_spork_is_off() {
    let (rig, log) = cl_rig(RigConfig::default());
    let main = branch(&rig.p, &rig.genesis, 1, 0);
    let side = branch(&rig.p, &rig.genesis, 2, 1);
    rig.accept(&main[0]);
    intake(&rig);
    assert!(record(&rig, &lock(&main[0])));
    settle(&rig);
    log.take();
    rig.sporks.as_ref().expect("a fresh rig").set(false);
    intake(&rig);
    for m in &side {
        rig.accept(m);
    }
    assert_eq!(tip(&rig), rig.id(&side[1]));
    rig.v.chainlock_timer().expect("V runs");
    settle(&rig);
    assert_eq!(tip(&rig), rig.id(&side[1]));
    assert!(!rig.status(&side[0]).has(Status::CONFLICT_CHAINLOCK));
    assert_eq!(floor(&rig), Some(rig.id(&main[0])), "on → off clears nothing");
    assert_eq!(log.take(), []);
}

/// The reset after an IS invalidation keeps ChainLock conflicts (`is:629`, `ResetBlockFailureFlags`
/// without the lock token): a conflict in a block a ChainLock already marked stays marked.
#[test]
fn the_reset_after_an_instantsend_conflict_keeps_chainlock_conflicts() {
    let (rig, main, side, _) = is_rig(|m, _| vec![m[1].hash()], false);
    assert!(record(&rig, &lock(&side[1])));
    settle(&rig);
    assert_eq!(tip(&rig), rig.id(&side[1]));
    assert!(rig.status(&main[1]).has(Status::CONFLICT_CHAINLOCK));
    let r = rig.v.is_conflict_invalidate(ISLOCK, TXID).recv();
    assert_eq!(
        r,
        Ok(IsResolution::Invalidated {
            activated: false,
        })
    );
    assert!(rig.status(&main[1]).has(Status::CONFLICT_CHAINLOCK));
    assert!(!rig.status(&main[1]).has(Status::FAILED_MASK));
    assert_eq!(tip(&rig), rig.id(&side[1]));
}
