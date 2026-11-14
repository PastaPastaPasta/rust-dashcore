//! The node's flush side (CH2b b4 M1): CH1-node's [`FlushSite`] over [`FlushControl`], W's
//! [`ThreadDriver`] and CH3a's coins, as CH1-node D47 items 1–10 set it (with its D57, D60 and
//! D61). The items below are D47's.
//!
//! Each call takes the coins from their cell, builds the call's [`FlushCx`] from V's host
//! ([`FlushHost::tip`], item 4) and hands V the tips W released before it returns, `Err`
//! included (item 5). W rings V's bell for every event, `Released` too (items 1, D60), and V's
//! waits for W inside the flush run V's fanout and park on V's own wake (item 2, b4 D54). With no
//! tip at V, W's `PruneClears` are declined (item 6), and a flush is Core's at `Height() == -1`
//! (b4 D55).

use std::sync::Arc;
use std::time::Instant;

use dash_consensus::coins::stats::CoinStatsHashType;
use dashd_store::KvEngine;

use super::control::{EvoWindow, FlushControl, FlushCx, NoTipCx};
use super::driver::ThreadDriver;
use super::hub::SubscriberHub;
use super::machine::{Kept, abort_text, kept, latched_fatal};
use super::observer::{FlushObserver, LogCat, log};
use super::packet::{EffectLedger, StateDelta};
use super::policy::{FlushPolicy, FlushStateMode};
use super::prune::PruneHost;
#[cfg(feature = "test-support")]
use crate::chain::Quiet;
#[cfg(feature = "test-support")]
use crate::chain::StagedRecord;
use crate::chain::{
    AbortCause, CacheUsage, Fatal, FlushBell, FlushHost, FlushReleases, FlushSite, SiteDecision,
};
use crate::coins::{
    CoinLayerOps as _, CoinsDump, CoinsScan, CoinsScanJob, CoinsStatsJob, start_coins_scan,
    start_coins_stats,
};
use crate::connect::{ChainstateCell, lock_cell};
use crate::planted::planted;
#[cfg(feature = "test-support")]
use crate::store::{CHAINSTATE_SCHEMA, CoinsReadError, GuardedRead as _, Meta};
use crate::store::{Fatal as StoreFatal, GlobalFailStop};
#[cfg(feature = "test-support")]
use dash_consensus::primitives::BlockHash;

/// What the flush reads beside V's host (b4 D56): Core reads both under `cs_main` at its site.
pub trait FlushInputs: Send {
    // Core: validation.cpp:2961 at v24.0.0 (GetCoinsCacheSizeState)
    /// `m_mempool->DynamicMemoryUsage()`; 0 without a mempool.
    fn mempool_usage(&self) -> u64;

    // Core: validation.cpp:3000 at v24.0.0
    /// Core's `fReindex`: no prune while it is set.
    fn reindex(&self) -> bool;
}

/// No mempool, no reindex: the node until MP1 and CH6 wire theirs.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoInputs;

impl FlushInputs for NoInputs {
    fn mempool_usage(&self) -> u64 {
        0
    }

    fn reindex(&self) -> bool {
        false
    }
}

/// What [`NodeFlush`] is built from.
pub struct NodeFlushParts<E: KvEngine, D: StateDelta> {
    /// The active coins, the connector's too (CH3 D2).
    pub coins: ChainstateCell<E>,
    /// W, on its own thread over the coins' engine and the block files.
    pub driver: ThreadDriver<D>,
    pub policy: FlushPolicy,
    /// The ledger V's presumed effects are recorded in.
    pub ledger: Arc<EffectLedger>,
    /// The subscribers' hub, W's: step 0's locks, `FlushDue` and the prune-lock moveback.
    pub hub: Arc<SubscriberHub>,
    /// V's flush log lines.
    pub obs: Arc<dyn FlushObserver>,
    /// CH7a's evo window; [`NoEvo`](super::NoEvo) until then.
    pub evo: Box<dyn EvoWindow<D> + Send>,
    /// The block files' prune side: [`FilesPruneHost`](super::FilesPruneHost) under `-prune`,
    /// [`NoPrune`](super::NoPrune) otherwise.
    pub prune: Box<dyn PruneHost + Send>,
    pub inputs: Box<dyn FlushInputs>,
}

/// What a call does with no tip at V (item 6, b4 D55).
#[derive(Clone, Copy)]
enum NoTip {
    /// W's events.
    Service,
    /// Core's `FlushStateToDisk(mode)` at `Height() == -1`.
    Flush(FlushStateMode),
    /// The job in flight's release.
    Stop,
    /// `PruneAndFlush`: the check armed, then cleared.
    PruneAndFlush,
    /// The reindex's clear.
    ReindexClear,
}

/// CH2b's production [`FlushSite`] (b3 D13, b4 M1): V's flush over CH3a's coins and W's thread.
pub struct NodeFlush<E: KvEngine, D: StateDelta> {
    coins: ChainstateCell<E>,
    ctl: FlushControl<ThreadDriver<D>>,
    /// `ctl`'s `FlushSignals` port: the tips W released, handed to V's host at each return.
    releases: FlushReleases,
    hub: Arc<SubscriberHub>,
    obs: Arc<dyn FlushObserver>,
    /// The node latch, the coins' engine's: `abort` trips it without the cell (item 8).
    latch: GlobalFailStop,
    evo: Box<dyn EvoWindow<D> + Send>,
    prune: Box<dyn PruneHost + Send>,
    inputs: Box<dyn FlushInputs>,
    /// The next flush panics before its job (BL-674).
    #[cfg(feature = "test-support")]
    panic_before_job: bool,
}

impl<E: KvEngine + 'static, D: StateDelta + Send + 'static> NodeFlush<E, D> {
    /// # Panics
    ///
    /// If the coins' cell is held or poisoned: the adapter is built before V runs.
    pub fn new(parts: NodeFlushParts<E, D>) -> Self {
        let NodeFlushParts {
            coins,
            driver,
            policy,
            ledger,
            hub,
            obs,
            evo,
            prune,
            inputs,
        } = parts;
        let latch = lock_cell(&coins).expect("the coins cell is free").db().latch().clone();
        let releases = FlushReleases::default();
        let mut ctl =
            FlushControl::new(driver, policy, Arc::new(releases.clone()), ledger, Arc::clone(&obs));
        ctl.set_hub(Arc::clone(&hub));
        Self {
            coins,
            ctl,
            releases,
            hub,
            obs,
            latch,
            evo,
            prune,
            inputs,
            #[cfg(feature = "test-support")]
            panic_before_job: false,
        }
    }

    /// The tally a test's node books V's completed jobs in ([`FlushControl::set_tally`]).
    #[cfg(feature = "test-support")]
    pub fn set_tally(&mut self, tally: Arc<crate::flush::FlushTally>) {
        self.ctl.set_tally(tally);
    }

    /// When V takes W's releases from this open's start ([`FlushControl::set_w_schedule`]).
    #[cfg(feature = "test-support")]
    pub fn set_w_schedule(&mut self, schedule: Option<crate::flush::WSchedule>) {
        self.ctl.set_w_schedule(schedule);
    }

    /// Runs `f` with the call's [`FlushCx`] at V's tip, then hands V's host the tips W released
    /// meanwhile, whatever `f` returned (item 5). With no tip at V (item 6), `no_tip` runs
    /// instead, every `PruneClears` declined, and `f` does not: `Ok(None)`.
    fn with_cx<T>(
        &mut self,
        host: &mut FlushHost<'_>,
        no_tip: NoTip,
        f: impl FnOnce(
            &mut FlushControl<ThreadDriver<D>>,
            &mut FlushCx<'_, E, D>,
        ) -> Result<T, StoreFatal>,
    ) -> Result<Option<T>, Fatal> {
        let r = self.run(host, no_tip, f);
        self.releases.hand_to(host);
        r
    }

    fn run<T>(
        &mut self,
        host: &mut FlushHost<'_>,
        no_tip: NoTip,
        f: impl FnOnce(
            &mut FlushControl<ThreadDriver<D>>,
            &mut FlushCx<'_, E, D>,
        ) -> Result<T, StoreFatal>,
    ) -> Result<Option<T>, Fatal> {
        let mut coins = lock_cell(&self.coins)?;
        let Some((tip, last_seq)) = host.tip() else {
            let mut cx = NoTipCx {
                now: Instant::now(),
                coins: &mut coins,
                mempool_usage: self.inputs.mempool_usage(),
                index: host,
                evo: &mut *self.evo,
                prune: &mut *self.prune,
                reindex: self.inputs.reindex(),
            };
            match no_tip {
                NoTip::Service => self.ctl.service_without_tip(&mut cx),
                NoTip::Flush(mode) => self.ctl.flush_without_tip(mode, &mut cx),
                NoTip::Stop => self.ctl.stop_without_tip(&mut cx),
                NoTip::PruneAndFlush => self.ctl.prune_and_flush_without_tip(&mut cx),
                NoTip::ReindexClear => self.ctl.reindex_clear_without_tip(&mut cx),
            }
            .map_err(reported)?;
            return Ok(None);
        };
        let is_ibd = host.is_ibd();
        let mut cx = FlushCx {
            now: Instant::now(),
            coins: &mut coins,
            mempool_usage: self.inputs.mempool_usage(),
            tip,
            last_seq,
            index: host,
            evo: &mut *self.evo,
            prune: &mut *self.prune,
            manual_prune_height: None,
            reindex: self.inputs.reindex(),
            is_ibd,
            left_ibd: !is_ibd,
            scripted: false,
        };
        f(&mut self.ctl, &mut cx).map(Some).map_err(reported)
    }
}

/// A flush-side failure: whoever tripped the latch has reported it (L-3), so V's stop does not.
/// Its text is that report's, Core's `state.ToString()` after the flush's `AbortNode` (CH3b
/// D221), whichever of V's waits read the latch first. A failure Core's next flush meets again
/// ([`kept`] is not `Nothing`: its database keeps the status, or the flush's own check repeats)
/// is Core's `FlushStateToDisk` `false` ([`Fatal::flush_failed`]); another store's failure, or
/// the simulated crash, is not, as Core's flush would write and succeed (BL-559); nor is one of
/// Core's asserts, which stays an assert ([`latched_fatal`]). The gate follows [`kept`]'s class,
/// which unknown sites still widen beyond "Core's next flush fails too".
// Core: validation.cpp:2250-2254, :3127-3128 at v24.0.0
fn reported(f: StoreFatal) -> Fatal {
    match kept(&f.0) {
        Kept::Store | Kept::Flush => Fatal::flush_failed(abort_text(f.0)),
        Kept::Nothing | Kept::Assert => latched_fatal(f.0),
    }
}

impl<E: KvEngine + 'static, D: StateDelta + Send + 'static> FlushSite for NodeFlush<E, D> {
    /// Items 1, 2 and 10: W rings `bell` for every event and at its end, a subscriber's
    /// `FlushDue` rings it, and V's waits inside the flush park on the wake it rings.
    fn attach(&mut self, bell: FlushBell) {
        let (due, wake) = (bell.clone(), bell.wake());
        self.ctl.driver().set_bell(move || bell.ring(), wake);
        self.hub.set_due_wake(move || due.ring());
    }

    fn at_site(
        &mut self,
        mode: FlushStateMode,
        host: &mut FlushHost<'_>,
    ) -> Result<SiteDecision, Fatal> {
        // With no tip, Core's flush at `Height() == -1` runs here, before the step.
        let decided = self.with_cx(host, NoTip::Flush(mode), |ctl, cx| ctl.at_site(mode, cx))?;
        Ok(SiteDecision(decided))
    }

    fn after_step(&mut self, decided: SiteDecision, host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        let SiteDecision(Some(pending)) = decided else {
            // Nothing decided (no tip at the site): W's events only.
            return self.service(host);
        };
        self.with_cx(host, NoTip::Service, |ctl, cx| {
            let (tip, seq) = (cx.tip, cx.last_seq);
            pending.freeze_after_step(ctl, cx, tip, seq)
        })?;
        Ok(())
    }

    fn service(&mut self, host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.with_cx(host, NoTip::Service, |ctl, cx| ctl.service(cx))?;
        Ok(())
    }

    /// Item 10 (CH1-node D57): W's events, a cause kept for the job that was in flight, then a pending
    /// `FlushDue`. V's in-hold drain does not go through `backpressure_wait`, so planted X15
    /// (a wait that does not answer `FlushDue`) is honoured here.
    fn answer_due(&mut self, host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.with_cx(host, NoTip::Service, |ctl, cx| {
            if planted!(cx.coins.db().planted(), X15) {
                ctl.service(cx)
            } else {
                ctl.answer_due(cx)
            }
        })?;
        Ok(())
    }

    // Core: validation.cpp:3303-3312 at v24.0.0
    /// CH1-node D61: the hub's moveback, each lock it moved logged under `BCLog::PRUNE`.
    fn move_prune_locks_back(&mut self, height: u32) {
        for (name, moved) in self.hub.on_disconnect_tip(height) {
            log(&*self.obs, LogCat::Prune, format!("{name} prune lock moved back to {moved}"));
        }
    }

    /// M3's entry (b4 D57): the decision and its freeze at once, `pruneblockchain`'s height
    /// with it.
    fn flush(
        &mut self,
        mode: FlushStateMode,
        manual_prune_height: Option<u32>,
        host: &mut FlushHost<'_>,
    ) -> Result<(), Fatal> {
        #[cfg(feature = "test-support")]
        if std::mem::take(&mut self.panic_before_job) {
            panic!("{}", super::control::PLANTED_FLUSH_PANIC);
        }
        // `pruneblockchain` is served once init has connected genesis (D55).
        debug_assert!(
            host.tip().is_some() || manual_prune_height.is_none(),
            "a manual prune with no tip"
        );
        self.with_cx(host, NoTip::Flush(mode), |ctl, cx| {
            cx.manual_prune_height = manual_prune_height;
            // CH5-5 §6.2's `flush` hook: this call is the scripted flush it asked for.
            #[cfg(feature = "test-hooks")]
            {
                let has_best_block = cx.coins.best_block().is_some();
                cx.scripted = dashd_store::livecopy::COUNTERS.take_scripted_flush(has_best_block);
            }
            ctl.flush(mode, cx)
        })?;
        Ok(())
    }

    /// The copy point's ALWAYS flush (E-b1.8: the hook before the release, so the snapshot pins
    /// none of the memtables Bk filled), then the scan's snapshot of the database it wrote, at
    /// the epoch W released. A scan whose thread cannot start is `None`, which the RPC answers
    /// as an unreadable set.
    fn coins_stats(
        &mut self,
        hash_type: CoinStatsHashType,
        interrupted: Arc<dyn Fn() -> bool + Send + Sync>,
        dump: Option<CoinsDump>,
        host: &mut FlushHost<'_>,
    ) -> Result<Option<CoinsStatsJob>, Fatal> {
        let mode = FlushStateMode::Always;
        let released = self.with_cx(host, NoTip::Flush(mode), |ctl, cx| {
            let released = ctl.copy_point_flush(cx)?;
            // CH5-5 §2.2: the copy point's counter snapshot travels with its release.
            #[cfg(feature = "test-hooks")]
            let released = (released, ctl.take_copy_point_snapshot());
            Ok(released)
        })?;
        #[cfg(feature = "test-hooks")]
        let (released, live_copy) = released.map_or((None, None), |(r, s)| (Some(r), s));
        let db = lock_cell(&self.coins)?.db_handle();
        #[cfg_attr(not(feature = "test-hooks"), expect(unused_mut))]
        let Ok(mut job) = start_coins_stats(db, hash_type, interrupted, dump) else {
            return Ok(None);
        };
        #[cfg(feature = "test-hooks")]
        job.set_live_copy(live_copy);
        // With no tip there was no copy point (a plain no-tip flush), and nothing to check.
        if let (Some(released), Some(taken)) = (released, job.snapshot_epoch()) {
            // W wrote nothing after the release, and V submitted nothing since.
            assert_eq!(taken, released, "the scan's snapshot is at its copy point");
        }
        Ok(Some(job))
    }

    /// As [`coins_stats`](Self::coins_stats): the copy point's ALWAYS flush, then the walk's
    /// snapshot at the epoch W released.
    fn coins_scan(
        &mut self,
        scan: CoinsScan,
        host: &mut FlushHost<'_>,
    ) -> Result<Option<CoinsScanJob>, Fatal> {
        let mode = FlushStateMode::Always;
        let released =
            self.with_cx(host, NoTip::Flush(mode), |ctl, cx| ctl.copy_point_flush(cx))?;
        let db = lock_cell(&self.coins)?.db_handle();
        let Ok(job) = start_coins_scan(db, scan) else {
            return Ok(None);
        };
        if let (Some(released), Some(taken)) = (released, job.snapshot_epoch()) {
            assert_eq!(taken, released, "the walk's snapshot is at its copy point");
        }
        Ok(Some(job))
    }

    /// As [`coins_stats`](Self::coins_stats): `key`'s value read before, the copy point's ALWAYS
    /// flush, then the read-back's snapshot at the epoch W released, dropped before it returns,
    /// so a V parked after it pins no memtables.
    #[cfg(feature = "test-support")]
    fn staged_record(
        &mut self,
        key: &BlockHash,
        host: &mut FlushHost<'_>,
    ) -> Result<Option<StagedRecord>, Fatal> {
        let cf = CHAINSTATE_SCHEMA.cf("block_index").expect("the chainstate schema has it");
        let read = |e: CoinsReadError| Fatal::reported(e.to_string());
        let db = lock_cell(&self.coins)?.db_handle();
        // The guarded engine, not V's index: what a kill now would leave.
        let before = db.read(cf, key.as_bytes()).map_err(read)?;
        let mode = FlushStateMode::Always;
        let released =
            self.with_cx(host, NoTip::Flush(mode), |ctl, cx| ctl.copy_point_flush(cx))?;
        let tip = host.tip().map(|(tip, _)| tip.hash);
        let snapshot = db.snapshot();
        if let Some(released) = released {
            // W wrote nothing after the release, and V submitted nothing since.
            assert_eq!(snapshot.epoch(), released, "the read-back is at its copy point");
        }
        let meta = Meta::read(&snapshot).map_err(read)?;
        let record = snapshot.read(cf, key.as_bytes()).map_err(read)?;
        Ok(Some(StagedRecord {
            tip,
            before,
            released_epoch: released,
            snapshot_epoch: snapshot.epoch(),
            coins_tip: meta.tip,
            head_blocks: meta.head_blocks,
            record,
        }))
    }

    /// b4 D57: the job in flight's release, the check armed, the NONE flush unlinking before
    /// its release, and that release.
    fn prune_and_flush(&mut self, host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.with_cx(host, NoTip::PruneAndFlush, |ctl, cx| ctl.prune_and_flush(cx))?;
        Ok(())
    }

    /// OD-CH5-57: [`FlushControl::reindex_clear`], its own job's failure split from one met
    /// before it ran (OD-CH5-77).
    fn reindex_clear(&mut self, host: &mut FlushHost<'_>) -> Result<Result<(), Fatal>, Fatal> {
        let cleared = self.with_cx(host, NoTip::ReindexClear, |ctl, cx| ctl.reindex_clear(cx));
        match cleared {
            Ok(_) => Ok(Ok(())),
            Err(own) if self.ctl.take_clear_failed() => Ok(Err(own)),
            Err(other) => Err(other),
        }
    }

    /// Item 7: the release of the job in flight, its `PruneClears` answered inline.
    fn stop(&mut self, host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.with_cx(host, NoTip::Stop, |ctl, cx| ctl.stop(cx))?;
        Ok(())
    }

    /// Item 8 (CH1-node D62): on V's panic the latch trips without a report; an `AbortNode`
    /// leaves it open for the shutdown flushes (DEC-241). The coins' cell is not taken, since V
    /// may be unwinding from a panic that poisoned it.
    fn abort(&mut self, cause: AbortCause, _host: &mut FlushHost<'_>) -> bool {
        match cause {
            AbortCause::Panic => self.ctl.trip_latch(&self.latch, "the chain actor panicked"),
            AbortCause::AbortNode => self.latch.check().is_err(),
        }
    }

    fn latched(&self) -> Option<Fatal> {
        self.latch.tripped_now().cloned().map(latched_fatal)
    }

    /// BL-678: the latch's waiting read (`tripped`, as `check` makes it), not `tripped_now`.
    fn admit(&self) -> Result<(), Fatal> {
        self.latch.tripped().map_or(Ok(()), |f| Err(latched_fatal(f.clone())))
    }

    /// CH3a a10-2: [`FlushControl::set_label`].
    #[cfg(feature = "test-support")]
    fn label(&mut self, label: Option<Arc<str>>) {
        self.ctl.set_label(label);
    }

    /// O2b-4 §2.2's seams on the flush control.
    #[cfg(feature = "test-support")]
    fn arm(&mut self, arm: crate::chain::FlushArm) {
        match arm {
            crate::chain::FlushArm::IndexOnly(causes) => self.ctl.force_index_only(causes),
            crate::chain::FlushArm::HoldNextRelease => self.ctl.hold_next_release(),
            crate::chain::FlushArm::WSchedule(schedule) => self.ctl.set_w_schedule(schedule),
        }
    }

    /// BL-586: [`ThreadDriver::panic_at_next_step`].
    #[cfg(feature = "test-support")]
    fn panic_w_at_next_step(&mut self) {
        self.ctl.driver().panic_at_next_step();
    }

    /// BL-674: before the job, here; with the job in flight, in `FlushControl::submit`.
    #[cfg(feature = "test-support")]
    fn panic_at_next_flush(&mut self, at: crate::chain::FlushPanic) {
        match at {
            crate::chain::FlushPanic::BeforeJob => self.panic_before_job = true,
            crate::chain::FlushPanic::JobInFlight(then) => self.ctl.panic_after_submit(then),
        }
    }

    /// CH3a a10 Q2: [`FlushControl::settle`], then what is left. With no tip, the job in
    /// flight's release alone (nothing is kept or due without one).
    #[cfg(feature = "test-support")]
    fn settle(&mut self, host: &mut FlushHost<'_>) -> Result<Quiet, Fatal> {
        let rounds = self.with_cx(host, NoTip::Stop, |ctl, cx| ctl.settle(cx))?;
        Ok(Quiet {
            rounds: rounds.unwrap_or_default(),
            in_flight: self.ctl.in_flight(),
            pending: self.ctl.pending(),
            deferred: 0,
            suppressed: self.ctl.suppressed(),
        })
    }

    /// `UpdateTip`'s caches (BL-281, CH1-node D69) as Core's stand after the step's flush: the
    /// active layer alone, since F's coins left Core's cache at its flush and Core prefetches
    /// nothing, and the evo window's writes not yet committed. The bytes are the native ledger's,
    /// not Core's `DynamicMemoryUsage()` formula. A held or poisoned cell reports no coins:
    /// `lock_cell` only tries the lock.
    fn cache_usage(&self) -> CacheUsage {
        let (coins_bytes, coins) = lock_cell(&self.coins).map_or((0, 0), |c| {
            let active = c.active();
            (active.allocated_bytes(), active.len() as u64)
        });
        CacheUsage {
            coins_bytes,
            coins,
            evo_bytes: self.evo.bytes(),
        }
    }
}
