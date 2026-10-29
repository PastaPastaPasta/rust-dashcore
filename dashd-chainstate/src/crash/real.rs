//! Core mode's connect (`connect=core`, CH3a a3): CH2 §6.4's swap of [`ChainSut`]'s mini connect
//! for the real `ConnectBlock`, [`SerialCore`] with `NoEvo`'s evo side, on regtest blocks
//! ([`synth`](super::synth)'s core mode); and of its mini disconnect for the real
//! `DisconnectBlock` (CH3b b4a, CH3 §4.6's C3-real).
//!
//! V's arena is not a [`HeaderChain`], so each call loads the arena's path to the block into a
//! node header chain and builds what the core takes from V around it: the reader, CS4's
//! deployments over its complete ancestry with a fresh versionbits cache, the status writer, a
//! ChainLock view with none, and the bracket's commit of the overlay on `Connected` (on
//! `DISCONNECT_OK` for a disconnect), which [`ChainSut`] makes. A connected block's status and
//! undo position come back from the chain the core wrote them into.
//!
//! [`ChainSut`]: super::ChainSut

use std::cell::RefCell;
use std::sync::{Arc, LazyLock};

use dash_chain::{
    ChainConfig as HeaderConfig, ChainReader, FailStop, HeaderChain, Services, StaticSporks, Work,
};
use dash_clock::MockClock;
use dash_consensus::BlockId;
use dash_consensus::deployments::{Deployments, NoEhfSignals, VersionBitsCache};
use dash_consensus::primitives::BlockHash;
use dash_params::ChainParams;
use dashd_store::faulty::{Abort, FaultyEngine};

use super::stores::Stores;
use super::synth::{GenBlock, T0, regtest};
use super::world::WorldObserver;
use crate::chain::{
    AbortPath, AbortSink, AttemptOrigin, BlockStatusWriter, ChainLockView, ConnectCx,
    ConnectOutcome, DisconnectResult, Fatal, GuardScope,
};
use crate::checkqueue::CheckQueue;
use crate::coins::BlockOverlay;
use crate::connect::bench::NoEvoSteps;
use crate::connect::{
    DISK_SPACE_LOW, FLUSH_UNDO_FAILED, NodeHelper, SerialCore, WRITE_UNDO_FAILED,
};
use crate::sigcache::{CacheSizes, ScriptCaches};

/// The script caches every core-mode connect consults: `ConnectBlock` stores nothing in them
/// (`fCacheResults` is `fJustCheck`), so one small pair serves every run.
pub(super) static CACHES: LazyLock<Arc<ScriptCaches>> = LazyLock::new(|| {
    let sizes = CacheSizes {
        signature_cache_bytes: 1 << 16,
        script_execution_cache_bytes: 1 << 16,
    };
    Arc::new(ScriptCaches::new(sizes).expect("the script caches"))
});

/// The header chain's load pool: the arena's path is short.
pub(super) static POOL: LazyLock<rayon::ThreadPool> = LazyLock::new(|| {
    rayon::ThreadPoolBuilder::new().num_threads(1).build().expect("the load pool")
});

/// V's connect and disconnect through the real core.
pub(super) struct RealConnect {
    core: SerialCore,
}

/// A connect's outcome, and the block's status and undo position after it.
pub(super) type Connected = (ConnectOutcome, dash_chain::Status, u32);

impl RealConnect {
    /// The core over regtest's rules, with the node's helper, no `-assumevalid`, and V's abort
    /// path onto the world: `s`'s observer and the stores' latch.
    pub(super) fn new(s: &Stores) -> Result<RealConnect, Abort> {
        let abort = AbortPath::new(Arc::new(RealSink(s.obs.clone())), s.db.latch().clone());
        let helper = Box::new(NodeHelper);
        let queue = Arc::new(CheckQueue::serial());
        let core = SerialCore::new(&regtest(), CACHES.clone(), queue, helper, abort, None)
            .map_err(|f| Abort::new(f.0))?;
        #[cfg(feature = "planted-bugs")]
        let core = {
            let mut core = core;
            core.plant(s.cfg.planted);
            core
        };
        Ok(RealConnect {
            core,
        })
    }

    /// `ConnectBlock` of `g`, whose index records from genesis are `records` (its own last), over
    /// `ov`, its parent's coins: Core's `ConnectTip` call with a block read from disk (`fChecked`
    /// false, so `CheckBlock` runs again) and the attempt's bracket around it, but the commit.
    pub(super) fn connect(
        &mut self,
        s: &Stores,
        records: Vec<(BlockHash, Vec<u8>)>,
        ov: &mut BlockOverlay<'_, FaultyEngine>,
        g: &GenBlock,
    ) -> Result<Connected, Fatal> {
        at_block(records, &g.hash, |chain, cx, id| {
            let mut steps = NoEvoSteps::default();
            let out = {
                let cell = RefCell::new(&mut *chain);
                let mut st = BlockStatusWriter::new(&cell, id);
                self.core.connect(&mut steps, cx, ov, &*s.files, &g.block, false, id, &mut st)
            };
            let out = steps.finish(out)?;
            let e =
                chain.reader().entry(id).ok_or_else(|| Fatal::new("the block's entry is gone"))?;
            Ok((out, e.status, e.undo_pos))
        })
    }

    /// `DisconnectBlock` of `g`, the tip, whose index records from genesis are `records` (its own
    /// last), over `ov`, its coins: Core's `DisconnectTip` call with its BLS scheme guard, the
    /// undo record read from the block files, and the attempt's bracket around it but the
    /// commit, which `DisconnectTip` makes only on `DISCONNECT_OK` (validation.cpp:3281-3287).
    pub(super) fn disconnect(
        &mut self,
        s: &Stores,
        records: Vec<(BlockHash, Vec<u8>)>,
        ov: &mut BlockOverlay<'_, FaultyEngine>,
        g: &GenBlock,
    ) -> Result<DisconnectResult, Fatal> {
        at_block(records, &g.hash, |_, cx, id| {
            let mut steps = NoEvoSteps::default();
            self.core.disconnect(&mut steps, cx, ov, &*s.files, &g.block, id, true)
        })
    }
}

/// `f` on the node header chain loaded from `records`, with the connector's context for the
/// block `hash` and its id there.
fn at_block<R>(
    records: Vec<(BlockHash, Vec<u8>)>,
    hash: &BlockHash,
    f: impl FnOnce(&mut HeaderChain, &ConnectCx<'_>, BlockId) -> Result<R, Fatal>,
) -> Result<R, Fatal> {
    let params = regtest();
    let svc = services(&params);
    let records = records.into_iter().map(Ok);
    let mut chain: HeaderChain =
        HeaderChain::load(params.clone(), HeaderConfig::node(&params), svc, records, &POOL)
            .map_err(|e| Fatal::new(format!("the arena's headers: {e:?}")))?;
    let reader = chain.reader();
    let id = reader.lookup(hash).ok_or_else(|| Fatal::new("an unindexed block"))?;
    let ancestry = chain.complete_ancestry().ok_or_else(|| Fatal::new("a windowed chain"))?;
    let cache = VersionBitsCache::new();
    let deployments = Deployments::new(&params, &ancestry, &NoEhfSignals, &cache);
    f(&mut chain, &cx(&params, &reader, &deployments, id), id)
}

/// The header chain's services: after every generated block's time, which nothing here reads.
pub(super) fn services(params: &ChainParams) -> Services {
    Services {
        clock: Arc::new(MockClock::at(i64::from(T0) + 86_400)),
        sporks: Arc::new(StaticSporks::for_network(params, None)),
        fail: Arc::new(InMemory),
    }
}

/// The connector's context for the block `id`, as core mode builds it.
pub(super) fn cx<'a>(
    params: &'a ChainParams,
    reader: &'a ChainReader,
    deployments: &'a Deployments<'a>,
    id: BlockId,
) -> ConnectCx<'a> {
    ConnectCx {
        params,
        reader,
        deployments,
        // Read only under `-assumevalid`, which is off.
        best_header: id,
        min_chain_work: Work::default(),
        // Changes no outcome here: it gates the IS-conflict loop, where `NodeHelper` reports
        // none, and G2's interim refusal, which only V's `Live` scope applies.
        is_ibd: false,
        chainlocks: &NoChainLocks,
        guard: GuardScope::of(params),
        origin: AttemptOrigin::Tip,
    }
}

/// V's stop on a connect's [`Fatal`]: reported here unless its reporter did (CH3a D60), and the
/// op's abort carries the epoch's first report.
pub(super) fn fatal_abort(s: &Stores, f: Fatal) -> Abort {
    let latched = s.db.latch().tripped().is_some();
    if f.reported_by_v(latched) {
        s.stop(None, &f.0)
    } else {
        Abort::new(s.obs.aborted().unwrap_or(f.0))
    }
}

/// The core's abort path onto the world: an undo write's failure is the `write_undo` store
/// call's (`StoreFailed`, as [`Stores::files_failed`] marks the mini connect's), any other
/// `AbortNode` V's own.
struct RealSink(Arc<WorldObserver>);

impl AbortSink for RealSink {
    fn abort_node(&self, log_msg: &str, user_msg: Option<&str>) {
        let store = [WRITE_UNDO_FAILED, FLUSH_UNDO_FAILED, DISK_SPACE_LOW].contains(&log_msg);
        self.0.abort(store.then_some("write_undo"), user_msg.unwrap_or(log_msg));
    }

    fn read_failed(&self, _what: &str) {
        self.0.abort(Some("read"), super::world::DB_READ_FAILED);
    }
}

/// The loaded chain's latch: it is in memory, so nothing reads through it.
pub(super) struct InMemory;

impl FailStop for InMemory {
    fn read_failed(&self, what: &str) {
        unreachable!("an in-memory header chain read failed: {what}");
    }

    fn is_tripped(&self) -> bool {
        false
    }
}

/// No ChainLock: the CRASH model has no signer.
pub(super) struct NoChainLocks;

impl ChainLockView for NoChainLocks {
    fn has_chainlock(&self, _: u32, _: &BlockHash) -> Result<bool, Fatal> {
        Ok(false)
    }

    fn has_conflicting_chainlock(&self, _: u32, _: &BlockHash) -> Result<bool, Fatal> {
        Ok(false)
    }

    fn best_height(&self) -> i32 {
        -1
    }
}
