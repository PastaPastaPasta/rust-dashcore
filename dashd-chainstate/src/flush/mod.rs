//! The flush (CH2 §2.7, §3.3, §4.1–§4.2; CH2b): what V decides, freezes and hands W, and W's
//! protocol that makes it durable.
//!
//! - [`FlushPolicy`]: Core's `FlushStateToDisk` trigger table on V (§3.3), with our causes.
//! - [`FreezePacket`]: what V hands W at a freeze, with the window's [`StateDelta`] and the
//!   [`EffectLedger`]'s sequence at the freeze (§4.1).
//! - [`PruneHost`]: the block files' side of step 0's prune decision (§4.5; CH2b b2 completes it).
//! - [`FlushMachine`]: W's flush of a [`FreezePacket`] or an [`IndexOnlyWrite`], one store-call
//!   group per step (§4.2), with its marks to a [`FlushObserver`] and `T_ack` to the
//!   [`SubscriberHub`].
//! - [`FlushDriver`]: what runs W's machines: [`ThreadDriver`] on W's thread, [`InlineDriver`] on
//!   the caller's (the seam).
//! - [`FlushControl`]: V's side: the decision at Core's call site, the freeze after the tip step
//!   ([`PendingFreeze`], E-b1.2), the release, and Core's `ChainStateFlushed` to the
//!   [`FlushSignals`] port (E-b1.4).

mod clears;
mod control;
mod driver;
mod hub;
mod machine;
mod observer;
mod packet;
mod policy;
mod prune;
mod signals;
mod site;
#[cfg(test)]
mod test_files;

pub use clears::{ClearsAnswer, PruneClears};
#[cfg(feature = "crash")]
pub(crate) use control::arm_and_take_without_tip;
pub use control::{EvoWindow, FlushControl, FlushCx, FlushIndex, NoEvo, NoTipCx, PendingFreeze};
pub use driver::{DriverEvent, FlushDriver, InlineDriver, ThreadDriver, WThread};
pub use hub::{
    CHAIN_STATE_FLUSHED, CommitTarget, FLUSH_COMMIT, FlushCommit, Flushed, Outstanding,
    REWIND_REQUEST, Registration, RewindRequest, SubscriberHandle, SubscriberHub,
};
pub use machine::{
    Abort, CoinsWritten, CrashRoll, DISK_BYTES_PER_ENTRY, FlushConfig, FlushEnv, FlushMachine,
    FlushReport, FlushStep, IndexOnlyWrite, Step,
};
pub(crate) use machine::{CRASH_MESSAGE, log_batch};
pub(crate) use observer::log;
pub use observer::{FlushObserver, LogCat, NullObserver, ObsMark, RecordingObserver};
pub use packet::{EffectLedger, FreezePacket, FrozenIndex, StateDelta};
pub use policy::{
    DATABASE_FLUSH_INTERVAL, DATABASE_WRITE_INTERVAL, EVO_CRITICAL_BYTES, FlushAction, FlushCauses,
    FlushPolicy, FlushStateMode, PolicyCtx,
};
pub use prune::{
    FilesPruneHost, NO_PRUNE_LOCK, NoPrune, PRUNE_LOCK_BUFFER, PruneDecision, PruneDryRun,
    PruneHost, PruneRule, prune_lock_bound,
};
pub use signals::{FlushSignals, NullSignals, RecordingSignals};
pub use site::{FlushInputs, NoInputs, NodeFlush, NodeFlushParts};
