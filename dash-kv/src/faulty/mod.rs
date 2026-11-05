//! ST3a's power-loss model (feature `faulty`; design `designs/ST3a-power-loss.md`, DEC-74).
//!
//! - [`World`]: the stores of one system under test, its events, marks and trace, and crash
//!   images with lazily decided cells (§3.2, §4.3).
//! - [`FaultyEngine`]: [`KvEngine`](crate::KvEngine) with subset semantics: between completed
//!   barriers any subset of buffered batches survives a power loss, each atomically (§2.4,
//!   DEC-62).
//! - [`FaultyFlatFiles`]: [`FlatFs`](crate::FlatFs) with per-sector temporal prefixes,
//!   reverting sizes, independent directory operations and fsyncgate poisoning (§2.2).
//! - The checks (§5) and the standing dependency checker D1 (§5.3).
//! - [`micro`] and [`spv`]: systems under test of their own, `MicroLog` (§6.1) and SB10's SPV
//!   store in miniature (SB0 §9.5).
//!
//! The node's systems under test over block files (`ToyChain`, §6.2) are in `dashd-store`'s
//! `faulty::toy`, which re-exports this module.
//!
//! Test-only: no runtime crate may enable this feature (§3.1).

mod check;
mod decide;
mod engine;
mod fs;
mod harness;
pub mod micro;
mod repro;
pub mod spv;
mod world;

pub use check::{
    Durability, FileRef, InvariantId, Peek, RefExtractor, Requirement, Scope, Violation,
    WorldDurableView, hex,
};
pub use decide::{Dfs, Fixed, Seeded, SeededDecider};
pub use engine::{FaultyEngine, Recovery};
pub use fs::{DATADIR, FaultyFlatFiles};
pub use harness::{
    Abort, CrashWindow, FatalCause, FaultPlan, Finding, Minimized, Mode, Nested, NestedTotals,
    OverCap, R7aCounts, Reference, Report, Scenario, Shard, Sut, allowed_tips, explore,
    explore_shard, forward_trace, minimize, minimize_decisions, replay, replay_trace, reproduces,
    shards, sweep,
};
pub use repro::Repro;
pub use world::{
    Adversary, AllSurvive, AllocateMode, CommitKind, CrashImage, CrashKind, Decider, EventHook,
    EventId, EventInfo, EventSel, FS_STORE, Fault, FaultKind, FaultOutcome, FlushCause,
    Granularity, Hash, InheritedDecision, LocatorKind, Mark, MarkKind, Observation, OpKind,
    PruneDue, ScriptCheck, Start, StoreSel, Trace, TraceEntry, Unavailable, Var, VarId, VarKind,
    WitnessSet, Work, World, WorldConfig,
};
