//! ChainLocks on V (§5): `ClIntake` and `RecordChainLock` (class M), `ClEnforce` (class C), the
//! [`ClSigs`] mirror of Core's three `Chainlocks` fields, and V's `TipHost` for SB4's
//! `enforce_block`.
//!
//! SB4 keeps the floor (`bestChainLockBlockIndex`) and the best lock's height and hash; V keeps
//! the signatures. V is the only writer of both and reconciles the mirror at every SB4 call that
//! can move them, before any `scope_end`, reply or snapshot wrap (§5.3); invariant M1 is checked
//! after every command in debug builds.

use std::fmt;
use std::sync::Arc;

use dash_chain::{
    ApplyError, ChainError, ChainLockRef, ClActiveState, ClOutcome, HeaderChain, PeerId, TipHost,
};
use dash_consensus::BlockId;
use dash_num::Hash256;
use dash_primitives::BlockHash;
use dash_types::Numeric;

use super::abc::{InHold, Steps};
use super::actor::{Deferred, V};
use super::command::ChainCommand;
use super::failure::assert_succeeds;
use super::index::entry;
use super::reply::ReplySender;
use super::seams::{DisconnectPool, Fatal};
use super::signals::Queued;

/// The call EnforceBlock asserts on when it fails (`v:4064-4067`), as Core's line names it
/// (CH3b D214).
const MARK: &str = "Chainstate::EnforceBlock -- MarkConflictingBlock";

/// Core `chainlock::ChainLockSig` (`chainlock/chainlock.h:23-45`) as V keeps it: V never reads
/// the signature, which LK1a has verified (D41).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainLockSig {
    /// `nHeight`: −1 in the null sig.
    pub height: i32,
    pub block_hash: BlockHash,
    /// The serialized `CBLSSignature`.
    pub sig: [u8; 96],
}

impl ChainLockSig {
    /// Core's default `ChainLockSig{}`: height −1, the zero hash (`IsNull()`).
    pub fn null() -> ChainLockSig {
        ChainLockSig {
            height: -1,
            block_hash: BlockHash::from([0; 32]),
            sig: [0; 96],
        }
    }

    pub fn null_arc() -> Arc<ChainLockSig> {
        Arc::new(ChainLockSig::null())
    }
}

// Core: chainlock/chainlock.cpp:23-26 ChainLockSig::ToString
impl fmt::Display for ChainLockSig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ChainLockSig(nHeight={}, blockHash={})", self.height, self.block_hash)
    }
}

/// Core's `Chainlocks` signatures (§5.5), published in [`ChainView`](super::ChainView): the
/// read side P2b, RPC and the miner use (`ClReader`). `bestChainLockBlockIndex` is SB4's floor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClSigs {
    /// `bestChainLock`; `None` is Core's null sig.
    pub best: Option<Arc<ChainLockSig>>,
    /// `bestChainLockHash`: the zero hash when unset or reset (`cl:98-104`).
    pub best_hash: Hash256,
    /// `bestChainLockWithKnownBlock`.
    pub best_known: Option<Arc<ChainLockSig>>,
    /// The ChainLock handler's `isEnabled` at publication.
    pub enabled_observed: bool,
}

impl ClSigs {
    fn new(enabled_observed: bool) -> ClSigs {
        ClSigs {
            best: None,
            best_hash: Hash256::ZERO,
            best_known: None,
            enabled_observed,
        }
    }

    /// `GetBestChainLockHeight()`.
    pub fn best_height(&self) -> i32 {
        self.best.as_ref().map_or(-1, |s| s.height)
    }

    /// `GetChainLockByHash` (`cl:140-150`): `bestChainLock` iff `bestChainLockHash == h`. With no
    /// lock recorded that is the null sig for `h = 0`, as Core answers `getdata(MSG_CLSIG, 0)`
    /// with a `clsig` (`np:3041-3046`).
    pub fn by_hash(&self, h: &Hash256) -> Option<Arc<ChainLockSig>> {
        (self.best_hash == *h).then(|| self.best.clone().unwrap_or_else(ChainLockSig::null_arc))
    }
}

/// V's ChainLock state beside SB4's (§5.5). Nothing is persisted: Core starts empty.
pub(crate) struct ClState {
    pub(crate) sigs: Arc<ClSigs>,
    /// `lastNotifyChainLockBlockIndex`.
    pub(crate) last_notify: Option<BlockId>,
    /// `tryLockChainTipScheduled`.
    pub(crate) tip_scheduled: bool,
}

impl ClState {
    pub(crate) fn new(enabled_observed: bool) -> ClState {
        ClState {
            sigs: Arc::new(ClSigs::new(enabled_observed)),
            last_notify: None,
            tip_scheduled: false,
        }
    }
}

/// Which of Core's scheduler tasks a `ClEnforce` runs (§5.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClTrigger {
    /// The 5 s timer (`h:63-72`), posted by ND1's scheduler.
    Timer,
    /// After `RecordChainLock` moved the floor (`h:169-176`).
    Recorded,
    /// After an `UpdatedBlockTip` (`h:185-207`).
    TipScheduled,
}

/// `RecordChainLock`'s reply: `ProcessNewChainLock` returns the inv iff `UpdateBestChainlock`
/// succeeded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClReply {
    pub relay: bool,
}

/// `ClIntake`'s reply: `GetBestChainLockHeight()` after `CheckActiveState` (`h:146`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClIntakeReply {
    pub best_height: i32,
}

impl V {
    /// Core `CheckActiveState` (`h:209-222`) with the mirror reconciled (§5.3): on off → on,
    /// `ResetChainlock` and `lastNotify = null`.
    pub(super) fn cl_active_state(&mut self) -> ClActiveState {
        let state = self.chain.check_cl_active_state();
        let enabled = self.chain.cl_enabled_observed();
        if state == ClActiveState::TurnedOnReset {
            self.cl.sigs = Arc::new(ClSigs::new(enabled));
            self.cl.last_notify = None;
        } else if self.cl.sigs.enabled_observed != enabled {
            Arc::make_mut(&mut self.cl.sigs).enabled_observed = enabled;
        }
        state
    }

    /// `ClIntake` (§5.1, step 1): `CheckActiveState` before anything can return early, so a
    /// completed rejection is never observable before Core's reset (R1-M3).
    pub(super) fn cl_intake(&mut self, reply: ReplySender<ClIntakeReply>) {
        self.cl_active_state();
        self.scope_end();
        reply.send(ClIntakeReply {
            best_height: self.cl.sigs.best_height(),
        });
    }

    /// `RecordChainLock` (§5.1, step 5): `UpdateBestChainlock` with the lookup at `h:166`. A
    /// moved floor queues `ClEnforce { Recorded }`, which runs only after the reply (OH-m3).
    pub(super) fn record_chainlock(
        &mut self,
        sig: Arc<ChainLockSig>,
        sig_hash: Hash256,
        from: Option<PeerId>,
        reply: ReplySender<ClReply>,
    ) -> Result<(), Fatal> {
        // A negative height is at or below a cleared best lock's −1: stale (`cl:110-113`).
        let outcome = match u32::try_from(sig.height) {
            Ok(height) => self.chain.node_index()?.record_chainlock(ChainLockRef {
                height,
                hash: sig.block_hash,
            }),
            Err(_) => ClOutcome::Stale,
        };
        let relay = match outcome {
            ClOutcome::Stale => false,
            ClOutcome::HeightMismatch => {
                // cl:115-125: the best lock moves; it is not relayed.
                tracing::info!(
                    "Chainlocks::UpdateBestChainlock -- height of CLSIG ({sig}) does not match \
                     the specified block's height ({})",
                    self.reader
                        .lookup(&sig.block_hash)
                        .map_or(-1, |id| i64::from(entry(&self.reader, id).height))
                );
                self.set_best_chainlock(sig, sig_hash, false);
                false
            }
            ClOutcome::Pending => {
                self.set_best_chainlock(sig, sig_hash, false);
                true
            }
            ClOutcome::Floor {
                ..
            } => {
                tracing::debug!(
                    "ChainlockHandler::ProcessNewChainLock -- processed new CLSIG ({sig}), peer={}",
                    from.map_or(-1, |p| p.0)
                );
                self.set_best_chainlock(sig, sig_hash, true);
                self.defer(Deferred::Command(ChainCommand::ClEnforce {
                    source: ClTrigger::Recorded,
                }));
                true
            }
        };
        self.scope_end();
        reply.send(ClReply {
            relay,
        });
        Ok(())
    }

    /// `bestChainLockHash` and `bestChainLock`, and with `known` `bestChainLockWithKnownBlock`.
    fn set_best_chainlock(&mut self, sig: Arc<ChainLockSig>, sig_hash: Hash256, known: bool) {
        let sigs = Arc::make_mut(&mut self.cl.sigs);
        if known {
            sigs.best_known = Some(sig.clone());
        }
        sigs.best = Some(sig);
        sigs.best_hash = sig_hash;
    }

    /// Core `Chainlocks::AcceptedBlockHeader` (`cl:152-172`): SB4 reports the floor it moved to
    /// a late header, also inside a batch that later stops; a height mismatch moves nothing.
    pub(super) fn apply_cl_late_header(&mut self, moved: Option<BlockId>) {
        let Some(id) = moved else {
            return;
        };
        let sigs = Arc::make_mut(&mut self.cl.sigs);
        let hash = entry(&self.reader, id).hash;
        debug_assert!(
            sigs.best.as_ref().is_some_and(|s| s.block_hash == hash),
            "the floor moved to the best lock's block"
        );
        tracing::debug!(
            "Chainlocks::AcceptedBlockHeader -- block header {hash} came in late, updating and \
             enforcing"
        );
        sigs.best_known = sigs.best.clone();
    }

    /// Queues `ClEnforce { TipScheduled }` unless one is queued (`tryLockChainTipScheduled`).
    pub(super) fn cl_schedule_tip(&mut self) {
        if !self.cl.tip_scheduled {
            self.cl.tip_scheduled = true;
            self.defer(Deferred::Command(ChainCommand::ClEnforce {
                source: ClTrigger::TipScheduled,
            }));
        }
    }

    /// `ClEnforce` (§5.2): one of Core's scheduler tasks, as a class C operation.
    pub(super) fn cl_enforce(&mut self, source: ClTrigger) -> Result<(), Fatal> {
        let r = self.composite(V::enforce_best_chainlock);
        if source == ClTrigger::TipScheduled {
            // h:203
            self.cl.tip_scheduled = false;
        }
        r
    }

    /// `CheckActiveState` then `EnforceBestChainLock` (`h:271-322`) with the pair `(clsig,
    /// bestChainLockBlockIndex)` captured once (`h:283`) and used for the enforcement, the
    /// activation check, the dedupe and the notification (R1-M2): a lock recorded or a reset
    /// during the activation changes none of them.
    pub(super) fn enforce_best_chainlock(&mut self) -> Result<(), Fatal> {
        // Either flip is published before the task goes on (or returns on off).
        if self.cl_active_state() != ClActiveState::Unchanged {
            self.scope_end();
        }
        // h:275-277, h:283-287
        if !self.chain.cl_enabled_observed() {
            return Ok(());
        }
        let (Some(floor), Some(sig)) = (self.chain.cl_floor(), self.cl.sigs.best_known.clone())
        else {
            return Ok(());
        };
        tracing::debug!(
            "ChainlockHandler::EnforceBestChainLock -- enforcing block {} via CLSIG ({sig})",
            entry(&self.reader, floor).hash
        );
        // h:296: EnforceBlock, one critical section of the tip mutex with SB4's snapshot at its
        // end; V's half of each mark is staged for after it (N1).
        let (report, released) = {
            let (chain, h) = self.lend();
            let mut host = VHost {
                h: Some(h),
                pool: None,
                walk: Vec::new(),
            };
            let report = chain.enforce_best_chainlock(&mut host);
            let h = host.h.expect("between calls");
            // A mark's pool is still open only if SB4 failed between its disconnects and
            // `conflicting_marked`. Core's steps there cannot fail, and it drains the pool on
            // every way out (`v:4101-4105`, `:4126`), so the pool goes the way of a failed
            // `DisconnectTip` before the error does.
            if let Some(pool) = host.pool {
                h.mempool.maybe_update_for_reorg(pool, false);
            }
            (report, h.emitted())
        };
        let report = match report {
            Ok(report) => report.expect("observed on, with a floor"),
            Err(ApplyError::Chain(e)) => return Err(Fatal::from(e)),
            // Only the mark's `DisconnectTip` fails a host call. EnforceBlock asserts on it
            // (`v:4064-4067`), after V has stopped on an `AbortNode` (CH3b D213, D214).
            Err(ApplyError::Host(f)) => {
                let marked = self.abort_at_fault(Err(f));
                return assert_succeeds(MARK, marked);
            }
        };
        debug_assert_eq!(report.floor, floor, "nothing ran between the capture and EnforceBlock");
        if let Some(reset) = &report.reset {
            self.after_reset(reset);
        }
        // `enforce_block` stored its snapshot on return (§5.4); the view follows it.
        self.after_release(released);
        // h:299-310 on V's private tip, Core's ActiveTip().
        let height = entry(&self.reader, floor).height;
        let on_floor = |v: &V| v.tip().and_then(|t| v.reader.ancestor(t, height)) == Some(floor);
        if !on_floor(self) {
            // Core's `false` follows an AbortNode, which has stopped V (CH3b D213).
            self.activate_best_chain(None).inspect_err(|f| {
                if f.is_abort_node() {
                    tracing::info!(
                        "ChainlockHandler::EnforceBestChainLock -- ActivateBestChain failed: {f}"
                    );
                }
            })?;
            if !on_floor(self) {
                return Ok(());
            }
        }
        // h:313-320, after the scope's publication (N1).
        if self.cl.last_notify == Some(floor) {
            return Ok(());
        }
        self.cl.last_notify = Some(floor);
        self.signals.enqueue(Queued::NotifyChainLock {
            id: floor,
            sig: sig.clone(),
        });
        self.ui.notify_chain_lock(sig.block_hash, sig.height);
        Ok(())
    }

    /// Invariant M1 (§5.3): the mirror and SB4's state are Core's three fields.
    pub(super) fn check_m1(&self) -> Result<(), String> {
        let sb4 = self.chain.cl_state();
        let sigs = &self.cl.sigs;
        let best = sigs.best.as_ref().map(|s| (i64::from(s.height), s.block_hash));
        let sb4_best = sb4.best.map(|b| (i64::from(b.height), b.hash));
        let floor = sb4.floor.map(|f| entry(&self.reader, f).hash);
        let known = sigs.best_known.as_ref().map(|s| s.block_hash);
        if sigs.best.is_none() != (sigs.best_hash == Hash256::ZERO) {
            return Err(format!("M1: best {best:?} with best hash {}", sigs.best_hash));
        }
        if best != sb4_best || floor != known || sigs.enabled_observed != sb4.enabled_observed {
            return Err(format!(
                "M1: the mirror's best {best:?}, known {known:?}, observed {} vs SB4's best \
                 {sb4_best:?}, floor {floor:?}, observed {}",
                sigs.enabled_observed, sb4.enabled_observed
            ));
        }
        Ok(())
    }
}

/// V's `TipHost` inside SB4's `enforce_block` (§5.4): V's parts with the chain lent per call, the
/// mark's mempool pool (`v:4090-4091`), and the tips the mark disconnected.
struct VHost<'v> {
    /// `None` only during a call.
    h: Option<InHold<'v, 'static>>,
    pool: Option<Box<dyn DisconnectPool>>,
    walk: Vec<BlockId>,
}

impl<'v> VHost<'v> {
    /// Runs `f` on V's parts over the lent chain.
    fn on<T>(
        &mut self,
        chain: &mut HeaderChain,
        f: impl FnOnce(
            &mut InHold<'v, '_>,
            &mut Option<Box<dyn DisconnectPool>>,
            &mut Vec<BlockId>,
        ) -> T,
    ) -> T {
        let (mut h, _) =
            self.h.take().expect("one call at a time").with_steps(Steps::Enforce(chain));
        let r = f(&mut h, &mut self.pool, &mut self.walk);
        self.h = Some(h.with_steps(Steps::Lent).0);
        r
    }
}

impl TipHost<dash_chain::NoStore, dash_chain::Permanent> for VHost<'_> {
    type Error = Fatal;

    fn tip(&self, chain: &HeaderChain) -> BlockId {
        chain.private_tip().expect("EnforceBlock runs after init").0.id
    }

    fn contains(&self, chain: &HeaderChain, id: BlockId) -> Result<bool, ChainError> {
        Ok(chain.active_contains_private(id))
    }

    /// `MarkConflictingBlock`'s `DisconnectTip` (`v:4096-4103`), §4.5 with the mark's pool.
    fn disconnect_tip(&mut self, chain: &mut HeaderChain) -> Result<(), Fatal> {
        self.on(chain, |h, pool, walk| {
            let tip = h.tip().expect("a tip above the marked block");
            let p = pool.get_or_insert_with(|| h.mempool.new_disconnect_pool());
            match h.disconnect_tip(Some(p.as_mut())) {
                Ok(true) => {
                    walk.push(tip);
                    Ok(())
                }
                r => {
                    // v:4099-4102, then EnforceBlock's assert (v:4064-4068).
                    h.mempool.maybe_update_for_reorg(pool.take().expect("opened"), false);
                    r?;
                    assert_succeeds(MARK, Ok(false))
                }
            }
        })
    }

    fn conflicting_marked(
        &mut self,
        chain: &mut HeaderChain,
        marked: BlockId,
        was_in_chain: bool,
    ) -> Result<(), Fatal> {
        self.on(chain, |h, pool, walk| {
            let pool = pool.take().unwrap_or_else(|| h.mempool.new_disconnect_pool());
            h.conflicting_marked(marked, was_in_chain, pool, std::mem::take(walk))
        })
    }
}

impl InHold<'_, '_> {
    /// `MarkConflictingBlock` after SB4's status writes (`v:4116-4148`), in Core's order.
    fn conflicting_marked(
        &mut self,
        marked: BlockId,
        was_in_chain: bool,
        pool: Box<dyn DisconnectPool>,
        walk: Vec<BlockId>,
    ) -> Result<(), Fatal> {
        // v:4116-4123: the disconnected tips and the marked block leave the candidates.
        for w in walk {
            self.idx.candidates_erase(w);
        }
        self.idx.candidates_erase(marked);
        // v:4125
        self.mempool.maybe_update_for_reorg(pool, true);
        // v:4129-4137
        let tip = self.tip().expect("genesis is never marked");
        self.readd_candidates(tip);
        self.conflicting_chain_found(marked, tip);
        // v:4140-4143
        let ibd = self.is_ibd();
        self.updated_block_tip(tip, None, ibd);
        // v:4146-4148
        if was_in_chain {
            let prev = entry(self.reader, marked).prev.expect("genesis is never marked");
            self.ui.push((self.loading.sync_state(ibd), prev));
        }
        // v:4069-4070
        tracing::info!(
            "Chainstate::EnforceBlock -- marked block {} as conflicting",
            entry(self.reader, marked).hash
        );
        Ok(())
    }
}
