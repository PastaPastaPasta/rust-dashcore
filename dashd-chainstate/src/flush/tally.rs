//! A test's tally of V's flushes by cause (CH3a a10-2; O2b §4.5's `flushes` and `flush_delta`):
//! each job booked once, when W reports it durable, as the shim books Core's after its write.
//! Leg B (O2b-4 §2.2) also journals each booking with the stamp its job took at its freeze.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::policy::FlushCauses;

/// Core's causes: the ones its count names. Not [`FlushCauses::CORE_POINT`]: the size causes and
/// PERIODIC_FLUSH carry Core's names, but the node's measure or timer can differ from Core's, so
/// its take at one keeps Core's dirt (CH3b D242, DEC-1451 Q2, DEC-1500).
// Core: validation.cpp:3180-3194 at the pin
const CORE: FlushCauses = FlushCauses::ALWAYS
    .union(FlushCauses::CACHE_LARGE)
    .union(FlushCauses::CACHE_CRITICAL)
    .union(FlushCauses::EVO_CRITICAL)
    .union(FlushCauses::PERIODIC_FLUSH)
    .union(FlushCauses::PRUNE);

/// The node's own causes, Production's alone (the ones `CoreCausesOnly` removes).
const OWN: FlushCauses = FlushCauses::THRESHOLD
    .union(FlushCauses::DMAX)
    .union(FlushCauses::LEFT_IBD)
    .union(FlushCauses::EMPTY);

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

/// Where V's index stood when a job froze its data (O2b-4 §2.2, D43): the open it ran in, and
/// that chain's index writes so far. Two stamps are the same point only in one open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stamp {
    /// [`FlushTally::generation`] at the freeze.
    pub generation: u64,
    /// The chain's `mutation_seq` at the freeze.
    pub mutation_seq: u64,
}

/// One booking, in completion order (O2b-4 §2.2 item 4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JournalEntry {
    /// A full flush, or the block index alone.
    pub full: bool,
    /// What it was booked under: the label, or the causes' names (as [`FlushTally::book`]).
    pub cause: String,
    /// Its causes, a label's job's too.
    pub causes: FlushCauses,
    /// The FLUSH op's label it was booked under, if any.
    pub label: Option<Arc<str>>,
    /// Its freeze's stamp.
    pub stamp: Stamp,
    /// The generation it was booked in: its release's.
    pub booked_in_generation: u64,
    /// A seam job a test forced ([`super::FlushControl::force_index_only`]), which leg B
    /// mirrors whatever its cause (O2b-4 §2.4 rule 2).
    pub forced: bool,
    /// Its job's take kept Core's dirt (CH3b D242), so its mirror needs a reconcile (DEC-1447).
    pub kept: bool,
}

#[derive(Debug, Default)]
struct Counts {
    since_open: CauseCounts,
    delta: CauseCounts,
    non_core: FlushCauses,
    generation: u64,
    op_generation: u64,
    retired_through: u64,
    journal: Vec<JournalEntry>,
}

/// The flushes V completed: since the last open, and since the op began (O2b §2.3). The node
/// that supplies it resets each at its own boundary; V books.
#[derive(Debug, Default)]
pub struct FlushTally(Mutex<Counts>);

impl FlushTally {
    // Core: validation.cpp:3180-3194, src/test/oracle/oracle_chain.cpp:497-505 at the pin
    /// A completed job: full, or the block index alone, by `causes` joined by `|` in
    /// [`FlushCauses`]' bit order, `PERIODIC_WRITE` for an index-only write with none; `label`,
    /// a FLUSH op's, replaces them. A job with one of Core's causes is named by its causes less
    /// the node's own (THRESHOLD, DMAX, LEFT_IBD, EMPTY): Core flushed there for its cause alone,
    /// so leg B (O2b-4 §2.3) matches the booking by that name; the journal keeps every cause. `leg_a`: the job's policy ran
    /// [`FlushRegime::CoreCausesOnly`](super::FlushRegime::CoreCausesOnly). `stamp`: the job's
    /// at its freeze, and `kept`, whether its take kept Core's dirt, journalled with the booking.
    pub fn book(
        &self,
        full: bool,
        causes: FlushCauses,
        label: Option<&Arc<str>>,
        leg_a: bool,
        stamp: Stamp,
        kept: bool,
    ) {
        self.book_marked(full, causes, label, leg_a, stamp, false, kept);
    }

    /// [`FlushTally::book`], journalling whether the job was a forced seam job.
    #[allow(clippy::too_many_arguments)]
    pub fn book_marked(
        &self,
        full: bool,
        causes: FlushCauses,
        label: Option<&Arc<str>>,
        leg_a: bool,
        stamp: Stamp,
        forced: bool,
        kept: bool,
    ) {
        let names = || {
            if causes.is_empty() {
                debug_assert!(!full, "a full flush has a cause");
                return "PERIODIC_WRITE".to_owned();
            }
            let named = if causes.intersects(CORE) {
                causes.difference(OWN)
            } else {
                causes
            };
            named.iter_names().map(|(name, _)| name).collect::<Vec<_>>().join("|")
        };
        let cause = label.map_or_else(names, |l| l.to_string());
        let mut counts = self.counts();
        counts.since_open.add(full, &cause);
        counts.delta.add(full, &cause);
        if leg_a {
            counts.non_core |= causes.difference(CORE);
        }
        let booked_in_generation = counts.generation;
        counts.journal.push(JournalEntry {
            full,
            cause,
            causes,
            label: label.cloned(),
            stamp,
            booked_in_generation,
            forced,
            kept,
        });
    }

    /// A load starts: `since_open` is empty, and a new generation begins (each load attempt,
    /// a refused one too).
    // Core: src/test/oracle/oracle_chain.cpp:798 at the pin
    pub fn opened(&self) {
        let mut counts = self.counts();
        counts.since_open = CauseCounts::default();
        counts.generation += 1;
    }

    /// An op starts: `delta` and the journal are empty; they span a teardown and the reopen.
    // Core: src/test/oracle/oracle_chain.cpp:928-930 at the pin
    pub fn begin_op(&self) {
        let mut counts = self.counts();
        counts.delta = CauseCounts::default();
        counts.journal.clear();
        counts.op_generation = counts.generation;
    }

    /// The open generation: 0 before the first load, then one more at each.
    pub fn generation(&self) -> u64 {
        self.counts().generation
    }

    /// The generation the op began in (`g0`, O2b-4 §2.3).
    pub fn op_generation(&self) -> u64 {
        self.counts().op_generation
    }

    /// Generation `g` and every earlier one have ended: a refused open's, once its shutdown has
    /// run (O2b-4 §2.3a). A retired generation's stamps are never op-end.
    pub fn retire(&self, g: u64) {
        let mut counts = self.counts();
        counts.retired_through = counts.retired_through.max(g);
    }

    /// Whether `g` is retired.
    pub fn is_retired(&self, g: u64) -> bool {
        g <= self.counts().retired_through
    }

    /// The bookings since the op began, in completion order.
    pub fn journal(&self) -> Vec<JournalEntry> {
        self.counts().journal.clone()
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
        tally.book(
            true,
            FlushCauses::PRUNE | FlushCauses::ALWAYS,
            None,
            true,
            Stamp::default(),
            false,
        );
        tally.book(false, FlushCauses::empty(), None, true, Stamp::default(), false);
        tally.book(false, FlushCauses::ALWAYS, None, true, Stamp::default(), false);
        tally.book(
            true,
            FlushCauses::ALWAYS,
            Some(&Arc::from("mirror")),
            true,
            Stamp::default(),
            false,
        );
        let counts =
            |list: &[(&str, u64)]| list.iter().map(|(c, n)| ((*c).to_owned(), *n)).collect();
        let want = CauseCounts {
            full: counts(&[("ALWAYS|PRUNE", 1), ("mirror", 1)]),
            index_only: counts(&[("ALWAYS", 1), ("PERIODIC_WRITE", 1)]),
        };
        assert_eq!(tally.since_open(), want);
        assert_eq!(tally.delta(), want);
        assert!(tally.non_core().is_empty());
        tally.book(
            true,
            FlushCauses::LEFT_IBD | FlushCauses::ALWAYS,
            None,
            false,
            Stamp::default(),
            false,
        );
        assert!(tally.non_core().is_empty(), "Production's");
        tally.book(
            true,
            FlushCauses::THRESHOLD | FlushCauses::CACHE_LARGE,
            None,
            true,
            Stamp::default(),
            false,
        );
        assert_eq!(tally.non_core(), FlushCauses::THRESHOLD);
    }

    /// Production's own causes are left out of a name that holds one of Core's, and kept in the
    /// journal; a job with none of Core's keeps all of them.
    #[test]
    fn a_core_cause_names_a_job_without_the_node_s_own() {
        let tally = FlushTally::default();
        let stamp = Stamp::default();
        tally.book(true, FlushCauses::ALWAYS | FlushCauses::LEFT_IBD, None, false, stamp, false);
        tally.book(
            true,
            FlushCauses::THRESHOLD | FlushCauses::CACHE_LARGE,
            None,
            false,
            stamp,
            false,
        );
        tally.book(true, FlushCauses::THRESHOLD | FlushCauses::DMAX, None, false, stamp, false);
        tally.book(
            true,
            FlushCauses::PERIODIC_FLUSH | FlushCauses::SCRIPTED,
            None,
            false,
            stamp,
            false,
        );
        let full: BTreeMap<String, u64> =
            ["ALWAYS", "CACHE_LARGE", "THRESHOLD|DMAX", "PERIODIC_FLUSH|SCRIPTED"]
                .iter()
                .map(|c| ((*c).to_owned(), 1))
                .collect();
        assert_eq!(tally.delta().full, full);
        let journal = tally.journal();
        assert_eq!(journal[0].causes, FlushCauses::ALWAYS | FlushCauses::LEFT_IBD);
        assert_eq!(journal[0].cause, "ALWAYS");
    }

    #[test]
    fn an_open_and_an_op_reset_their_own_counts() {
        let tally = FlushTally::default();
        tally.book(true, FlushCauses::ALWAYS, None, true, Stamp::default(), false);
        tally.opened();
        assert_eq!(tally.since_open(), CauseCounts::default());
        assert_eq!(tally.delta().full.get("ALWAYS"), Some(&1), "the delta spans the reopen");
        tally.book(true, FlushCauses::ALWAYS, None, true, Stamp::default(), false);
        tally.begin_op();
        assert_eq!(tally.delta(), CauseCounts::default());
        assert_eq!(tally.since_open().full.get("ALWAYS"), Some(&1));
    }

    #[test]
    fn the_journal_keeps_each_booking_with_its_stamp_and_generation() {
        let tally = FlushTally::default();
        assert_eq!(tally.generation(), 0);
        tally.opened();
        let g1 = tally.generation();
        assert_eq!(g1, 1);
        let at = |mutation_seq| Stamp {
            generation: g1,
            mutation_seq,
        };
        tally.book(true, FlushCauses::THRESHOLD, None, false, at(7), true);
        tally.book(false, FlushCauses::empty(), Some(&Arc::from("PERIODIC")), false, at(9), false);
        let journal = tally.journal();
        assert_eq!(journal.len(), 2);
        assert_eq!(
            (
                journal[0].full,
                journal[0].cause.as_str(),
                journal[0].stamp,
                journal[0].label.clone()
            ),
            (true, "THRESHOLD", at(7), None)
        );
        assert_eq!(
            (journal[1].cause.as_str(), journal[1].label.as_deref()),
            ("PERIODIC", Some("PERIODIC"))
        );
        assert!(journal.iter().all(|e| e.booked_in_generation == g1));
        assert_eq!((journal[0].kept, journal[1].kept), (true, false), "journalled as booked");
        // A reload: a job stamped in g1 and released after the load books in g2.
        tally.opened();
        tally.book(true, FlushCauses::ALWAYS, None, false, at(9), false);
        assert_eq!(tally.journal()[2].booked_in_generation, 2, "the release's generation");
        assert_eq!(tally.journal()[2].stamp.generation, 1, "the freeze's");
        assert!(!tally.is_retired(2));
        tally.retire(2);
        assert!(tally.is_retired(1) && tally.is_retired(2) && !tally.is_retired(3));
        tally.begin_op();
        assert!(tally.journal().is_empty(), "an op starts with an empty journal");
        assert_eq!(tally.generation(), 2, "an op opens no generation");
        assert_eq!(tally.op_generation(), 2, "g0");
    }
}
