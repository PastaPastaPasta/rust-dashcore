//! The node latch's sites: where a failure that trips it came from, as [`Failure::site`] records
//! it. Every production trip names one of these, so a site cannot drift out of its class by a
//! typo (BL-407).
//!
//! [`Failure::site`]: super::Failure::site

/// Where a chainstate write fails, as the latch records it (the flush side's text, CH3b D221).
pub const WRITE_SITE: &str = "chainstate write";
/// Where a chainstate row V writes outside the flush fails: the quorum-snapshot row
/// (`GuardedEngine::write_raw`). A failed evo snapshot write stops the node with one report (EV3
/// D21: store faults are not consensus rejects; CH7 D102).
pub const RAW_WRITE_SITE: &str = "chainstate raw write";
/// Where a chainstate barrier (a WAL sync or a memtable flush) fails.
pub const BARRIER_SITE: &str = "chainstate barrier";
/// A chainstate read's engine error, or a stored coin that does not decode.
pub const READ_SITE: &str = "chainstate read";
/// The flush's own failures: W's failed step (its space check), W's panic, V's panic stop, and
/// V's `AbortNode` in the flush.
pub const FLUSH_SITE: &str = "chainstate flush";
/// The connector's `AbortNode` (`AbortPath`, CH3 D52).
pub const VALIDATION_SITE: &str = "chainstate validation";
/// An EV attempt that unwound (`AttemptUnwind`, CH3a D71), which reports nothing itself: the tip
/// hold reports it on the tip path, and nothing does on the paths with no tip hold
/// (`TestBlockValidity`, VerifyDB). The shutdown flushes keep it loud until those paths report it
/// themselves (BL-601).
pub const UNWIND_SITE: &str = "chainstate unwind";
/// A block-file or undo-file mutation, with its reporter's `AbortNode` text.
pub const BLOCK_FILES_WRITE_SITE: &str = "block files write";
/// A block-file read's I/O failure (recovery's replay).
pub const BLOCK_FILES_READ_SITE: &str = "block files read";
/// `-dbcrashratio`'s simulated crash.
pub const CRASH_SITE: &str = "dbcrashratio";
