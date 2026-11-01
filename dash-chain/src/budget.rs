//! The SPV step budget (SB5 §6.2): the cap derived from the configuration, and the account of
//! what the segmented step charges against it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use dash_consensus::BlockId;

use crate::batch::AcceptOne;
use crate::error::ChainError;
use crate::feed::{FeedConfig, Registry, RegistryEntry};
use crate::snapshot::ChainReadSnapshot;
use crate::status_feed::StatusFacts;
use crate::step::StepRow;
use crate::tip::{BlockRef, TipChange};

/// The step budget's account (§6.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StepBudgetStats {
    /// [`step_cap`] for the chain's configuration.
    pub cap: usize,
    /// The largest sub-step charge seen: its legs, plus their pending copy when a target lacked
    /// them.
    pub peak: usize,
    /// The marks term's peak: the status overlay's table at a release, in bytes (§6.2's M(m),
    /// uncapped by design, DEC-157 (3)).
    pub marks_peak: usize,
}

/// The fixed term of §6.2's cap: the two containers a release publishes, each an `Arc`'s
/// counts and its value: the snapshot and the status facts (DEC-187 amendment 3). The registry
/// and the active chain's paths have their own terms (amendment 2 (a)). A pending segment's
/// `BlockRef`s are the legs term's pending copy, and its slot in the backlog is reserved with
/// the feed table, so the term holds no segment.
pub const STEP_FIXED_BYTES: usize = arc_bytes::<ChainReadSnapshot>() + arc_bytes::<StatusFacts>();

/// An `Arc`'s allocation, laid out as std's `ArcInner`: the two counts, then the value, padded
/// to the larger alignment.
#[repr(C)]
struct ArcAlloc<T> {
    strong: usize,
    weak: usize,
    value: T,
}

const fn arc_bytes<T>() -> usize {
    size_of::<ArcAlloc<T>>()
}

/// §6.2's registry term for `max_feeds` (at most [`MAX_FEEDS`](crate::MAX_FEEDS)): a step's
/// retires republish the registry, each building one while the last is published, every one an
/// `Arc`'s counts, the [`Registry`] and `max_feeds` reserved entries. A retired registry a
/// reader still holds is that reader's (§6.1).
pub const fn registry_bytes(max_feeds: u32) -> usize {
    let entries = max_feeds as usize * size_of::<RegistryEntry>();
    2 * (arc_bytes::<Registry>() + entries)
}

/// §6.2's path term: the active chain's copy-on-write. A step's pushes and pops copy the tip's
/// root-to-leaf path out of the published view, and a disconnect below the window's base the
/// base's; each original stays until the release drops the view.
pub const ACTIVE_PATHS_BYTES: usize = 2 * crate::active::PATH_COPY_BYTES;

/// §6.2's step cap, from the real sizes: the accept results (B), one write batch (W), a
/// sub-step's plan legs, `TipChange` legs and pending copy (3 × E), the cold invalidation list
/// (C), the registry republish, the active chain's two path copies, and the fixed term.
/// 433,104 B on desktop and 202,704 B on mobile at x86-64's sizes.
pub const fn step_cap(feeds: &FeedConfig, cold_cache: u32) -> usize {
    size_of::<AcceptOne>() * feeds.step_headers as usize
        + size_of::<StepRow>() * feeds.write_batch as usize
        + 3 * size_of::<BlockRef>() * feeds.segment_events as usize
        + size_of::<BlockId>() * cold_cache as usize
        + registry_bytes(feeds.max_feeds)
        + ACTIVE_PATHS_BYTES
        + STEP_FIXED_BYTES
}

/// The writer's peaks, kept beside the feeds (read without the tip lock).
#[derive(Default)]
pub(crate) struct StepBudget {
    peak: AtomicUsize,
    marks_peak: AtomicUsize,
}

impl StepBudget {
    /// Charges a sub-step: its legs, and their pending copy if a target lacked them (`copied`).
    pub(crate) fn sub_step(&self, change: &TipChange, copied: bool) {
        self.peak.fetch_max(sub_step_bytes(change, copied), Ordering::Relaxed);
    }

    /// Records the marks term at a release: the status overlay's table, sized from its own
    /// entry type.
    pub(crate) fn marks<K, V, H>(&self, overlay: &HashMap<K, V, H>) {
        self.marks_peak.fetch_max(table_bytes(overlay), Ordering::Relaxed);
    }

    pub(crate) fn peak(&self) -> usize {
        self.peak.load(Ordering::Relaxed)
    }

    pub(crate) fn marks_peak(&self) -> usize {
        self.marks_peak.load(Ordering::Relaxed)
    }
}

/// §6.2's sub-step term, `3 · size_of::<BlockRef>() · E`: a change whose legs and pending copy
/// could pass it is `Internal`. SB4 builds the legs at exact capacity and `tip_plan_bounded`
/// keeps them at most E, so this never fires on a sound plan.
pub(crate) fn check_sub_step(change: &TipChange, e: u32) -> Result<(), ChainError> {
    if sub_step_bytes(change, true) > 3 * size_of::<BlockRef>() * e as usize {
        return Err(ChainError::Internal("step budget: a sub-step past its term"));
    }
    Ok(())
}

/// A sub-step's legs, and their pending copy if a target lacked them (`copied`).
fn sub_step_bytes(change: &TipChange, copied: bool) -> usize {
    let legs = change.disconnected.capacity() + change.connected.capacity();
    let pending = if copied {
        change.disconnected.len() + change.connected.len()
    } else {
        0
    };
    size_of::<BlockRef>() * (legs + pending)
}

/// A hash table's bytes: hashbrown's buckets for its capacity, an entry and a control byte
/// each, plus one group.
fn table_bytes<K, V, H>(map: &HashMap<K, V, H>) -> usize {
    let capacity = map.capacity();
    let buckets = match capacity {
        0 => return 0,
        1..8 => (capacity + 1).next_power_of_two(),
        _ => (capacity * 8 / 7).next_power_of_two(),
    };
    buckets * (size_of::<(K, V)>() + 1) + 16
}
