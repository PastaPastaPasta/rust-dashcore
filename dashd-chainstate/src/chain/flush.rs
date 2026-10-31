//! V's side of the flush (CH1-node §7.1, §7.2): the [`FlushSite`] seam CH2b's `FlushControl`
//! plugs into, what V lends it at each call ([`FlushHost`]), and the bell W rings so that every
//! wait of V answers its events (H3, BL-123).
//!
//! V calls the seam at Core's `FlushStateToDisk` sites: [`FlushSite::at_site`] decides there and
//! never freezes, and [`FlushSite::after_step`] runs the decision after the tip step, at the
//! post-step pair (D3). At each of its waits, in and out of holds, V runs
//! [`FlushSite::service`], which answers W's `PruneClears` and takes its releases; it never
//! freezes. V's orderly stop ends with [`FlushSite::stop`], and an `AbortNode` stop with
//! [`FlushSite::abort`] (D45).

use std::sync::{Arc, Mutex, PoisonError};

use crossbeam_channel::{Sender, TryRecvError};
use dash_chain::{BlockRef, ChainReader, HeaderChain, IndexWrite, Parker, Publish};
use dash_clock::NodeTime;
use dash_consensus::primitives::BlockHash;

use dash_consensus::coins::stats::CoinStatsHashType;

use super::index::NodeBlockIndex;
use super::seams::Fatal;
use super::signals::Queued;
use crate::coins::{CoinsDump, CoinsScan, CoinsScanJob, CoinsStatsJob};
use crate::flush::{FlushIndex, FlushSignals, PendingFreeze, stored_records};

pub use crate::flush::FlushStateMode;

/// CH2b's `FlushStateToDisk` at Core's call sites (§7.1); [`NoFlush`] until CH2b's W is wired.
pub trait FlushSite: Send {
    /// Once, as V starts: W's events ring `bell`.
    fn attach(&mut self, _bell: FlushBell) {}

    // Core: validation.cpp:2966-3047 Chainstate::FlushStateToDisk (the decision)
    /// The decision at Core's site in `mode`, with any wait it makes there for the job in
    /// flight. It never freezes.
    ///
    /// # Errors
    ///
    /// [`Fatal`]: the flush has failed, and V stops. The flush side has reported it, so V's
    /// sink does not (D45, L-3): a job can fail on W's thread while V waits on nothing.
    fn at_site(
        &mut self,
        mode: FlushStateMode,
        host: &mut FlushHost<'_>,
    ) -> Result<SiteDecision, Fatal>;

    /// Runs `decided` at V's private tip and its `seq` (the post-step pair, D3): right after the
    /// tip step at an IF_NEEDED site, at once elsewhere. A freeze that is waited for is waited
    /// for here.
    ///
    /// # Errors
    ///
    /// As [`at_site`](Self::at_site).
    fn after_step(&mut self, decided: SiteDecision, host: &mut FlushHost<'_>) -> Result<(), Fatal>;

    /// W's events: its `PruneClears` answered, its releases taken. It never freezes. V calls it
    /// at its stop and a coins attach; its waits call [`answer_due`](Self::answer_due).
    ///
    /// # Errors
    ///
    /// As [`at_site`](Self::at_site).
    fn service(&mut self, host: &mut FlushHost<'_>) -> Result<(), Fatal>;

    /// W's events at one of V's waits (D45 (a)–(d)), then a subscriber's `FlushDue` answered
    /// (D47 item 10, D57): with no job in flight and a tip, a cause a site kept for the job that
    /// was in flight (CH2b b3 D24), then a pending `FlushDue`, each one freeze at
    /// `(host.tip(), the last seq)`, an empty one if nothing changed. V never calls it between a
    /// connector's commit and its tip step, or at its stop or a coins attach.
    ///
    /// # Errors
    ///
    /// As [`at_site`](Self::at_site).
    fn answer_due(&mut self, host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.service(host)
    }

    // Core: validation.cpp:3292-3301 (DisconnectTip's prune-lock moveback)
    /// DisconnectTip of the block at `height` moves the prune locks above `height − 1` back
    /// (D61), after `DisconnectBlock`'s commit and before the IF_NEEDED site. CH2b's adapter
    /// forwards it to `SubscriberHub::on_disconnect_tip` and logs each lock it moved under
    /// `BCLog::PRUNE`, "%s prune lock moved back to %d" (CH2b b3 D8).
    fn move_prune_locks_back(&mut self, _height: u32) {}

    // Core: validation.cpp:3122-3128 ForceFlushStateToDisk (ALWAYS), :4977-4983
    // PruneBlockFilesManual (NONE with its height)
    /// A flush V's commands ask for, with no tip step: the decision and its freeze at once, at
    /// `(host.tip(), the last seq)`, waited for where Core's is (ALWAYS, a manual prune). The
    /// entry for N7's `Flush` command (CH1-node D45, ND1 2b M3); `manual_prune_height` is
    /// `pruneblockchain`'s height. PruneAndFlush is [`prune_and_flush`](Self::prune_and_flush).
    /// With no tip, Core's flush at `Height() == -1`: the block index alone (CH2b b4 D55).
    ///
    /// # Errors
    ///
    /// As [`at_site`](Self::at_site).
    fn flush(
        &mut self,
        _mode: FlushStateMode,
        _manual_prune_height: Option<u32>,
        _host: &mut FlushHost<'_>,
    ) -> Result<(), Fatal> {
        Ok(())
    }

    // Core: validation.cpp:3141-3148 PruneAndFlush
    /// `gettxoutsetinfo`'s `ForceFlushStateToDisk()` and the read of the coins it scans, in one
    /// turn (RP1a D2): an ALWAYS [`flush`](Self::flush), then a scan of the coins' database,
    /// returned once its snapshot is taken, which also writes `dump` (`dumptxoutset`). `None`
    /// when the site has no database (the tests' sites) or the scan cannot start.
    ///
    /// # Errors
    ///
    /// As [`at_site`](Self::at_site).
    // Core: rpc/blockchain.cpp:1212-1220
    fn coins_stats(
        &mut self,
        _hash_type: CoinStatsHashType,
        _interrupted: Arc<dyn Fn() -> bool + Send + Sync>,
        _dump: Option<CoinsDump>,
        _host: &mut FlushHost<'_>,
    ) -> Result<Option<CoinsStatsJob>, Fatal> {
        Ok(None)
    }

    /// `scantxoutset`'s `ForceFlushStateToDisk()` and cursor, in one turn: as
    /// [`coins_stats`](Self::coins_stats), a walk for `scan` instead of the statistics.
    ///
    /// # Errors
    ///
    /// As [`at_site`](Self::at_site).
    // Core: rpc/blockchain.cpp:2766-2774
    fn coins_scan(
        &mut self,
        _scan: CoinsScan,
        _host: &mut FlushHost<'_>,
    ) -> Result<Option<CoinsScanJob>, Fatal> {
        Ok(None)
    }

    /// `PruneAndFlush`, init step 10 under `-prune` (ND1 2b M3): the automatic prune check armed,
    /// then a NONE flush, returning once it is done, as Core's synchronous one does: the files it
    /// selected are unlinked. A job in flight is waited for first, as Core's flushes run one at
    /// a time. With no tip the check is cleared and nothing selected (CH2b b4 D55, D57).
    ///
    /// # Errors
    ///
    /// As [`at_site`](Self::at_site).
    fn prune_and_flush(&mut self, _host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        Ok(())
    }

    /// V's orderly stop, after its last [`service`](Self::service) (D45): while a job is in
    /// flight, W's `wait_release`, which answers its `PruneClears` inline, so a prune W selects
    /// after `Shutdown` is taken is applied, as Core's shutdown flush prunes synchronously.
    ///
    /// # Errors
    ///
    /// As [`at_site`](Self::at_site).
    fn stop(&mut self, _host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        Ok(())
    }

    /// V stops on an `AbortNode` or a panic (D45), before V's sink reports. It must not panic.
    /// On V's panic the store latch is tripped, without a report of its own, so W writes nothing
    /// more and runs no queued job, as Core's failed `assert` ends the process with no flush. An
    /// `AbortNode` leaves the store open (D62, DEC-241): Core's only requests shutdown
    /// (`shutdown.cpp:27-37`), and the shutdown flushes still run; V then stops the flush side
    /// in D45's order, unless the latch had already tripped.
    ///
    /// `true` if the latch had already tripped: that failure has been reported, by the flush
    /// side or the store that tripped it, so V's sink stays silent and the node sees one
    /// `AbortNode` (L-3). Every [`Fatal`] the flush side returns is such a failure.
    ///
    /// V calls it again at its end when it stopped where an `AbortNode` arose (CH3b D213); for an
    /// `AbortNode` it only reads the latch.
    fn abort(&mut self, _cause: AbortCause, _host: &mut FlushHost<'_>) -> bool {
        false
    }

    // Core: validation.cpp:3178-3187 UpdateTipLog (cache=, evodb_cache=)
    /// What `UpdateTip`'s line reports of the caches the decision measures (BL-281): the coins'
    /// `DynamicMemoryUsage()` and `GetCacheSize()`, and `m_evoDb.GetMemoryUsage()`. None of each
    /// unless the flush side has them.
    fn cache_usage(&self) -> CacheUsage {
        CacheUsage::default()
    }
}

/// `UpdateTip`'s cache fields ([`FlushSite::cache_usage`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheUsage {
    /// The coins' `DynamicMemoryUsage()`, in bytes.
    pub coins_bytes: u64,
    /// The coins' `GetCacheSize()`: the coins the cache holds.
    pub coins: u64,
    /// `m_evoDb.GetMemoryUsage()`, in bytes.
    pub evo_bytes: u64,
}

/// Why V stops serving, as [`FlushSite::abort`] hears it (D62).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AbortCause {
    /// V's `AbortNode`: the store stays open for the shutdown flushes (DEC-241).
    AbortNode,
    /// V panicked, or failed one of Core's `assert`s ([`Fatal::assert`], D62a): the store latch
    /// trips, as Core's `assert` ends the process with no flush.
    Panic,
}

/// No flush: the seam until CH2b's W is wired (D-CH1-3).
#[derive(Clone, Copy, Debug, Default)]
pub struct NoFlush;

impl FlushSite for NoFlush {
    fn at_site(&mut self, _: FlushStateMode, _: &mut FlushHost<'_>) -> Result<SiteDecision, Fatal> {
        Ok(SiteDecision::nothing())
    }

    fn after_step(&mut self, _: SiteDecision, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        Ok(())
    }

    fn service(&mut self, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        Ok(())
    }
}

/// A decision made at Core's site, carried across the tip step.
#[must_use = "a decision runs in FlushSite::after_step"]
#[derive(Debug)]
pub struct SiteDecision(pub Option<PendingFreeze>);

impl SiteDecision {
    /// Nothing to run after the step.
    pub fn nothing() -> SiteDecision {
        SiteDecision(None)
    }
}

/// W's bell as V's selects take it (§7.1): a ring, never a reply.
#[expect(clippy::disallowed_types, reason = "V's own bell; V carries no marker")]
pub(super) type Bell = crossbeam_channel::Receiver<()>;

/// What one of V's waits `got` from W's bell: `true` for a ring. A bell whose sender is gone (no
/// W, or W has ended) is dropped, since a disconnected receiver is always ready and V's waits
/// would spin on it.
pub(super) fn take_ring(bell: &mut Option<Bell>, got: Result<(), TryRecvError>) -> bool {
    match got {
        Ok(()) => true,
        Err(TryRecvError::Empty) => false,
        Err(TryRecvError::Disconnected) => {
            *bell = None;
            false
        }
    }
}

/// W's bell to V (BL-123): a ring wakes V's lane select and its in-hold parks, so the wait
/// answers W. The driver rings it for every event it hands V, and when the latch trips or W
/// ends.
#[derive(Clone)]
pub struct FlushBell {
    tx: Sender<()>,
    wake: Arc<Parker>,
}

impl FlushBell {
    /// The bell over V's in-hold wake, and the receiver V's lane select takes.
    pub(crate) fn new(wake: Arc<Parker>) -> (FlushBell, Bell) {
        let (tx, rx) = crossbeam_channel::bounded(1);
        (
            FlushBell {
                tx,
                wake,
            },
            rx,
        )
    }

    pub fn ring(&self) {
        // A full slot is a ring V has not taken yet; a closed one, a stopped V.
        let _ = self.tx.try_send(());
        self.wake.notify();
    }

    /// The wake the bell rings, a subscriber's room too: V's waits for W in CH2b's flush park
    /// on it (b4 D54).
    pub(crate) fn wake(&self) -> Arc<Parker> {
        Arc::clone(&self.wake)
    }
}

/// The chain as V lends it to the flush: inside a hold, SB5's pending fanout drains through the
/// hold (§2.5).
pub trait FlushChain {
    fn chain(&mut self) -> &mut HeaderChain;

    /// SB5's writer rule: every wait for W runs it, so that a subscriber W waits on gets the
    /// step's event (`-dbcrashratio`'s completion wait, §7.1).
    fn retry_pending(&mut self) -> Publish;
}

impl FlushChain for HeaderChain {
    fn chain(&mut self) -> &mut HeaderChain {
        self
    }

    fn retry_pending(&mut self) -> Publish {
        HeaderChain::retry_pending(self)
    }
}

/// CH2b's `FlushSignals` port as V's flush side plugs it in: `FlushControl` hands it the tips
/// W released, and the [`FlushSite`] hands them to V's [`FlushHost`] before its call returns, so
/// a release observed inside a hold is staged with the hold's callbacks (N1, DEC-166). A port
/// that enqueued them itself would reach listeners before the hold's snapshot.
#[derive(Clone, Debug, Default)]
pub struct FlushReleases(Arc<Mutex<Vec<BlockRef>>>);

impl FlushReleases {
    /// The tips released since the last call, in order, to V.
    pub fn hand_to(&self, host: &mut FlushHost<'_>) {
        let tips = std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner));
        for tip in tips {
            host.chain_state_flushed(tip);
        }
    }
}

impl FlushSignals for FlushReleases {
    fn chain_state_flushed(&self, tip: BlockRef) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).push(tip);
    }
}

/// What V lends its flush side at a call: its chain, its block index and `m_have_pruned`. It
/// collects the `ChainStateFlushed` tips W released during the call, which V queues, or stages
/// inside a hold (DEC-166, N1).
pub struct FlushHost<'a> {
    chain: &'a mut dyn FlushChain,
    idx: &'a mut NodeBlockIndex,
    have_pruned: &'a mut bool,
    /// The node clock's `now` and `LoadingBlocks()` at the call: `IsInitialBlockDownload`'s
    /// inputs.
    now: NodeTime,
    loading: bool,
    flushed: Vec<BlockRef>,
}

impl<'a> FlushHost<'a> {
    pub(crate) fn new(
        chain: &'a mut dyn FlushChain,
        idx: &'a mut NodeBlockIndex,
        have_pruned: &'a mut bool,
        now: NodeTime,
        loading: bool,
    ) -> Self {
        Self {
            chain,
            idx,
            have_pruned,
            now,
            loading,
            flushed: Vec::new(),
        }
    }

    // Core: validation.cpp:3012 (FindFilesToPrune's IsInitialBlockDownload())
    /// Core's `IsInitialBlockDownload()` under `cs_main`, on V's private tip (CH1-A8), the
    /// working one inside a hold: the flush's prune selection and its leave-IBD cause read it.
    pub fn is_ibd(&mut self) -> bool {
        let (now, loading) = (self.now, self.loading);
        self.chain.chain().is_ibd_private(now, loading)
    }

    /// V's private tip and the `seq` of its last step: Core's `m_chain.Tip()`, the working tip
    /// inside a hold. It is `FlushCx::tip`, which DEC-211's check at `PruneClears` reads, never a
    /// published view. `None` before genesis is connected (Core's `Height() == -1`), at
    /// genesis's own ConnectTip site and the devnet step's AcceptBlock: no flush can be in
    /// flight, and a `PruneClears` there is declined, as `FindFilesToPrune` returns early.
    pub fn tip(&mut self) -> Option<(BlockRef, u64)> {
        self.chain.chain().private_tip()
    }

    /// The tip height DEC-211's check of a `PruneClears` reads: the lower of V's working tip and
    /// the published one (L-1). A clear is visible to view readers at once, so a selection the
    /// working tip of a forward hold allows but the published tip does not is declined, and the
    /// next flush selects again. `None` while either has no tip: the request is declined.
    pub fn prune_check_height(&mut self) -> Option<u32> {
        let working = self.tip()?.0.height;
        let published = self.chain.chain().snapshot();
        published.has_tip().then(|| working.min(published.tip.tip_height))
    }

    /// SB5's pending fanout: each wait for W runs it.
    pub fn retry_pending(&mut self) -> Publish {
        self.chain.retry_pending()
    }

    /// Core's `ChainStateFlushed` for a flush whose B1 W has released (DEC-166): `tip` is the
    /// locator's tip captured at the site.
    pub fn chain_state_flushed(&mut self, tip: BlockRef) {
        self.flushed.push(tip);
    }

    /// Runs `f` on `site` with this host lent, then hands `sink` a `ChainStateFlushed` with the
    /// locator of each tip W released meanwhile (DEC-166): V queues them, a hold stages them (N1).
    pub(crate) fn lend<T>(
        mut self,
        site: &mut dyn FlushSite,
        reader: &ChainReader,
        mut sink: impl FnMut(Queued),
        f: impl FnOnce(&mut dyn FlushSite, &mut FlushHost<'_>) -> Result<T, Fatal>,
    ) -> Result<T, Fatal> {
        let r = f(site, &mut self);
        for tip in self.flushed {
            sink(Queued::ChainStateFlushed(reader.locator(Some(tip.id))));
        }
        r
    }
}

impl FlushIndex for FlushHost<'_> {
    fn take_dirty_into(&mut self, put: &mut dyn FnMut(&BlockHash, &[u8])) {
        self.chain.chain().node_index().expect("V's chain is the node's").take_dirty_into(put)
    }

    fn dirty_bound(&mut self) -> Option<(usize, usize)> {
        stored_records(
            self.chain.chain().node_index().expect("V's chain is the node's").dirty_len(),
        )
    }

    fn prune_clear(&mut self, files: &[u32]) -> Result<Vec<IndexWrite>, String> {
        let cleared =
            self.idx.prune_clear(self.chain.chain(), files).expect("V's chain is the node's");
        // :3019-3023: Core sets it for a non-empty selection, in the same flush, before its next
        // CheckBlockIndex.
        if !files.is_empty() {
            *self.have_pruned = true;
        }
        Ok(cleared)
    }

    /// D47 item 9: the lower of V's working and published tips (L-1).
    fn clears_check_height(&mut self, _working: u32) -> Option<u32> {
        FlushHost::prune_check_height(self)
    }

    /// D47 item 2: SB5's writer rule at CH2b's waits for W.
    fn pump_fanout(&mut self) {
        #[cfg(test)]
        AT_PUMP.with_borrow(|hook| {
            if let Some(hook) = hook {
                hook();
            }
        });
        self.retry_pending();
    }
}

#[cfg(test)]
thread_local! {
    /// The tests' mark that V is in a wait for W: run on V's thread at each fanout pump there,
    /// which V makes before every park (BL-354).
    pub(crate) static AT_PUMP: std::cell::RefCell<Option<Box<dyn Fn()>>> =
        const { std::cell::RefCell::new(None) };
}
