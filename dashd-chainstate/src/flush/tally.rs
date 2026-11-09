//! A test's tally of V's flushes by cause (CH3a a10-2; O2b §4.5's `flushes` and `flush_delta`):
//! each job booked once, when W reports it durable, as the shim books Core's after its write.

use std::collections::BTreeMap;
use std::sync::Mutex;

use super::policy::FlushCauses;

/// Core's causes: the ones its count names.
// Core: validation.cpp:3180-3194 at the pin
const CORE: FlushCauses = FlushCauses::ALWAYS
    .union(FlushCauses::CACHE_LARGE)
    .union(FlushCauses::CACHE_CRITICAL)
    .union(FlushCauses::EVO_CRITICAL)
    .union(FlushCauses::PERIODIC_FLUSH)
    .union(FlushCauses::PRUNE);

/// Flushes by cause, full and index-only, as the shim's `FlushCounts` keeps them.
// Core: src/test/oracle/oracle_chain.cpp:194-209 at the pin
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CauseCounts {
    pub full: BTreeMap<String, u64>,
    pub index_only: BTreeMap<String, u64>,
}

impl CauseCounts {
    fn add(&mut self, full: bool, cause: &str) {
        let counts = if full {
            &mut self.full
        } else {
            &mut self.index_only
        };
        *counts.entry(cause.to_owned()).or_default() += 1;
    }
}

#[derive(Debug, Default)]
struct Counts {
    since_open: CauseCounts,
    delta: CauseCounts,
    non_core: FlushCauses,
}

/// The flushes V completed: since the last open, and since the op began (O2b §2.3). The node
/// that supplies it resets each at its own boundary; V books.
#[derive(Debug, Default)]
pub struct FlushTally(Mutex<Counts>);

impl FlushTally {
    // Core: validation.cpp:3180-3194, src/test/oracle/oracle_chain.cpp:497-505 at the pin
    /// A completed job: full, or the block index alone, by `causes` joined by `|` in
    /// [`FlushCauses`]' bit order, `PERIODIC_WRITE` for an index-only write with none; `label`,
    /// a FLUSH op's, replaces them. `leg_a`: the job's policy ran
    /// [`FlushRegime::CoreCausesOnly`](super::FlushRegime::CoreCausesOnly).
    pub fn book(&self, full: bool, causes: FlushCauses, label: Option<&str>, leg_a: bool) {
        let names = || {
            if causes.is_empty() {
                debug_assert!(!full, "a full flush has a cause");
                return "PERIODIC_WRITE".to_owned();
            }
            causes.iter_names().map(|(name, _)| name).collect::<Vec<_>>().join("|")
        };
        let cause = label.map_or_else(names, str::to_owned);
        let mut counts = self.counts();
        counts.since_open.add(full, &cause);
        counts.delta.add(full, &cause);
        if leg_a {
            counts.non_core |= causes.difference(CORE);
        }
    }

    /// A load starts: `since_open` is empty.
    // Core: src/test/oracle/oracle_chain.cpp:763 at the pin
    pub fn opened(&self) {
        self.counts().since_open = CauseCounts::default();
    }

    /// An op starts: `delta` is empty; it spans a teardown and the reopen.
    // Core: src/test/oracle/oracle_chain.cpp:928-930 at the pin
    pub fn begin_op(&self) {
        self.counts().delta = CauseCounts::default();
    }

    /// The flushes since the last load (`post.flushes`).
    pub fn since_open(&self) -> CauseCounts {
        self.counts().since_open.clone()
    }

    /// The flushes since the op began (`post.flush_delta`).
    pub fn delta(&self) -> CauseCounts {
        self.counts().delta.clone()
    }

    /// The causes outside Core's that any job booked under
    /// [`FlushRegime::CoreCausesOnly`](super::FlushRegime::CoreCausesOnly) had: none, as its
    /// policy removes them before they start a job.
    pub fn non_core(&self) -> FlushCauses {
        self.counts().non_core
    }

    fn counts(&self) -> std::sync::MutexGuard<'_, Counts> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn causes_join_in_bit_order_and_a_label_replaces_them() {
        let tally = FlushTally::default();
        tally.book(true, FlushCauses::PRUNE | FlushCauses::ALWAYS, None, true);
        tally.book(false, FlushCauses::empty(), None, true);
        tally.book(false, FlushCauses::ALWAYS, None, true);
        tally.book(true, FlushCauses::ALWAYS, Some("mirror"), true);
        let counts =
            |list: &[(&str, u64)]| list.iter().map(|(c, n)| ((*c).to_owned(), *n)).collect();
        let want = CauseCounts {
            full: counts(&[("ALWAYS|PRUNE", 1), ("mirror", 1)]),
            index_only: counts(&[("ALWAYS", 1), ("PERIODIC_WRITE", 1)]),
        };
        assert_eq!(tally.since_open(), want);
        assert_eq!(tally.delta(), want);
        assert!(tally.non_core().is_empty());
        tally.book(true, FlushCauses::LEFT_IBD | FlushCauses::ALWAYS, None, false);
        assert!(tally.non_core().is_empty(), "Production's");
        tally.book(true, FlushCauses::THRESHOLD | FlushCauses::CACHE_LARGE, None, true);
        assert_eq!(tally.non_core(), FlushCauses::THRESHOLD);
    }

    #[test]
    fn an_open_and_an_op_reset_their_own_counts() {
        let tally = FlushTally::default();
        tally.book(true, FlushCauses::ALWAYS, None, true);
        tally.opened();
        assert_eq!(tally.since_open(), CauseCounts::default());
        assert_eq!(tally.delta().full.get("ALWAYS"), Some(&1), "the delta spans the reopen");
        tally.book(true, FlushCauses::ALWAYS, None, true);
        tally.begin_op();
        assert_eq!(tally.delta(), CauseCounts::default());
        assert_eq!(tally.since_open().full.get("ALWAYS"), Some(&1));
    }
}
