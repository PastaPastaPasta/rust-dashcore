//! Leg B's step (O2b-4 §2.3, §2.3a, D53): on S0p the node flushes in `Production`, on its own
//! causes, while Core's size flushes are off. After each op the node's bookings
//! ([`NodeSut::end_op`]) are matched against Core's by `(full, label)`; each one Core lacks is
//! mirrored to Core as a `FLUSH` under the node's label, and a mirror of a job frozen mid-op is
//! followed by a `FLUSH ALWAYS label=reconcile` on both sides. Each side's op step and its
//! auxiliary steps are then reduced to one step ([`reduce`]), which the runner judges.
//!
//! [`NodeSut::end_op`]: dashd_node::sut::NodeSut::end_op

use std::collections::BTreeMap;

use dash_oracle_sys::{ChainOp, FlushCounts, FlushMode, Post};
use dashd_chainstate::flush::{FlushCauses, JournalEntry};
use dashd_node::sut::{OpClass, OpEnd};
use serde_json::json;

use super::census::{LEGB_NATURAL_STEPS, LEGB_STEPS};
use super::diff::{Facts, NodeFacts};
use super::step::Step;

/// The label both sides' reconcile flush books under.
pub const RECONCILE: &str = "reconcile";

/// Why leg B's reduction ends the comparison of an op.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LegBEnd {
    /// A harness or protocol fault: the input stops, reported as unexpected.
    Unexpected(String),
    /// A refused load whose cleanup flushes differ between the sides (§2.3a): a D49-style
    /// partial with no credit, naming the first node booking left unmatched.
    RefusedOpenCleanup(String),
}

/// One booking of the node's that Core lacks, to be mirrored to Core.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mirror {
    pub full: bool,
    /// The node's label for it: a FLUSH op's label, or its causes' names.
    pub label: String,
    /// Its job's causes.
    pub causes: FlushCauses,
    /// Whether it was a FLUSH op's (an explicit flush), not the policy's.
    pub labelled: bool,
    pub class: OpClass,
}

/// The policy causes whose mirrors count toward the share (§2.7): LEFT_IBD, the forced and the
/// explicit flushes are left out.
const NATURAL: FlushCauses = FlushCauses::THRESHOLD
    .union(FlushCauses::CACHE_LARGE)
    .union(FlushCauses::CACHE_CRITICAL)
    .union(FlushCauses::DMAX)
    .union(FlushCauses::EMPTY);

/// The causes leg B mirrors an unlabelled unmatched booking for (§2.4 rule 2, OD-L3-131): the
/// node's own (the tally's `OWN`) and the size causes `-oracle-no-size-flush` turns off on Core.
/// Core flags every other cause on S0p itself, so a node booking of one that Core lacks is a
/// node-extra flush, left to diverge.
// Core: src/test/oracle/oracle_chain.cpp:497-505 at the pin
const MIRRORED: FlushCauses = FlushCauses::THRESHOLD
    .union(FlushCauses::DMAX)
    .union(FlushCauses::LEFT_IBD)
    .union(FlushCauses::EMPTY)
    .union(FlushCauses::CACHE_LARGE)
    .union(FlushCauses::CACHE_CRITICAL)
    .union(FlushCauses::EVO_CRITICAL);

/// Whether an unmatched booking of the current open is mirrored (§2.4 rule 2, amended by
/// OD-L3-131, OD-L3-133 and OD-L3-142): an unlabelled one whose causes are a non-empty subset of
/// [`MIRRORED`], or a forced seam job. An index-only job with no cause is Core's periodic write
/// (`PERIODIC_WRITE`), which Core makes on S0p too.
fn mirrored(entry: &JournalEntry) -> bool {
    entry.forced
        || (entry.label.is_none()
            && !entry.causes.is_empty()
            && entry.causes.difference(MIRRORED).is_empty())
}

/// The census rows one leg-B step reaches (§2.7): every step counts toward the share's
/// denominator, and once toward its numerator if some mirror is of a natural policy cause;
/// THRESHOLD, `CACHE_*` and LEFT_IBD mirrors count each; a reconcile counts as
/// `legb_reconcile_delayed`.
pub fn rows(plan: &Plan) -> Vec<&'static str> {
    let mut rows = vec![LEGB_STEPS];
    let policy = || plan.mirrors.iter().filter(|m| !m.labelled);
    if policy().any(|m| m.causes.intersects(NATURAL)) {
        rows.push(LEGB_NATURAL_STEPS);
    }
    for m in policy() {
        if m.causes.contains(FlushCauses::THRESHOLD) {
            rows.push("legb_mirror_threshold");
        }
        if m.causes.intersects(FlushCauses::CACHE_LARGE | FlushCauses::CACHE_CRITICAL) {
            rows.push("legb_mirror_cache");
        }
        if m.causes.contains(FlushCauses::LEFT_IBD) {
            rows.push("legb_left_ibd");
        }
    }
    if needs_reconcile(plan) {
        rows.push("legb_reconcile_delayed");
    }
    rows
}

/// What leg B does after an op: the mirrors, in the node's completion order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub mirrors: Vec<Mirror>,
}

/// Core's bookings as a multiset keyed by `(full, label)`.
type Bag = BTreeMap<(bool, String), u64>;

fn bag(counts: &FlushCounts) -> Bag {
    let mut out = Bag::new();
    for (full, map) in [(true, &counts.full), (false, &counts.index_only)] {
        for (name, &n) in map {
            if n > 0 {
                *out.entry((full, name.clone())).or_default() += n;
            }
        }
    }
    out
}

fn minus(mut a: Bag, b: &Bag) -> Bag {
    for (k, n) in b {
        if let Some(have) = a.get_mut(k) {
            *have = have.saturating_sub(*n);
        }
    }
    a.retain(|_, n| *n > 0);
    a
}

/// Takes one `(full, label)` from `bag`: whether it held one.
fn take(bag: &mut Bag, full: bool, label: &str) -> bool {
    match bag.get_mut(&(full, label.to_owned())) {
        Some(n) if *n > 0 => {
            *n -= 1;
            true
        }
        _ => false,
    }
}

/// The §2.3/§2.3a boundaries of one op. `reload` is a `RESTART` or a restore; `core` is Core's
/// op step; `node_down` says whether the node's post is down; `end` is the node's bookings.
///
/// - An ordinary op: each booking, in completion order, is matched against Core's
///   `flush_delta`; each unmatched one is mirrored.
/// - A load that succeeded on both sides: the bookings of the op's first open (`g0`, the
///   teardown's) are matched against Core's pre-load bookings (`flush_delta − flushes`), and any
///   unmatched one is unexpected; those of the new open are matched against Core's `flushes`
///   and mirrored if unmatched (after Core's load).
/// - A load refused on both sides: every booking (`g0` and the refused open's) must equal Core's
///   `flush_delta` as a multiset, else [`LegBEnd::RefusedOpenCleanup`]; nothing is mirrored.
/// - Sides that disagree on `down`: no mirror; the judgement reports the divergence.
///
/// Only a booking of the current open (`OpEnd` or `MidOp`) can be mirrored: an unmatched
/// `Retired` one is unexpected. Of those, only `mirrored` ones are: any other is a node-extra
/// flush, left unmirrored so that the step's `flush_delta` diverges.
pub fn boundaries(
    reload: bool,
    core: &Step,
    node_down: bool,
    end: &OpEnd,
) -> Result<Plan, LegBEnd> {
    let delta = bag(&core.post.flush_delta);
    if core.post.down != node_down {
        return Ok(Plan::default());
    }
    if reload && core.post.down {
        let mut left = delta;
        for (entry, _) in &end.entries {
            if !take(&mut left, entry.full, &entry.cause) {
                return Err(LegBEnd::RefusedOpenCleanup(entry.cause.clone()));
            }
        }
        if let Some(((_, label), _)) = left.into_iter().find(|&(_, n)| n > 0) {
            return Err(LegBEnd::RefusedOpenCleanup(label));
        }
        return Ok(Plan::default());
    }
    let (mut before, mut after) = if reload {
        let flushes = core.post.flushes.as_ref().map(bag).unwrap_or_default();
        (minus(delta, &flushes), flushes)
    } else {
        (Bag::new(), delta)
    };
    let mut plan = Plan::default();
    for (entry, class) in &end.entries {
        if reload && entry.booked_in_generation == end.g0 && end.g0 != end.g1 {
            if !take(&mut before, entry.full, &entry.cause) {
                return Err(LegBEnd::Unexpected("legb: unmatched pre-load flush".into()));
            }
            continue;
        }
        if take(&mut after, entry.full, &entry.cause) {
            continue;
        }
        if *class == OpClass::Retired {
            return Err(LegBEnd::Unexpected(format!(
                "legb: unmatched retired flush {}",
                entry.cause
            )));
        }
        if !mirrored(entry) {
            continue;
        }
        plan.mirrors.push(Mirror {
            full: entry.full,
            label: entry.cause.clone(),
            causes: entry.causes,
            labelled: entry.label.is_some(),
            class: *class,
        });
    }
    Ok(plan)
}

/// Core's mirror ops (rule 2), in the node's completion order: a full booking as `FLUSH
/// ALWAYS`, an index-only one as `FLUSH INDEX_ONLY`, each under the node's label.
pub fn mirrors(plan: &Plan) -> Vec<ChainOp> {
    plan.mirrors
        .iter()
        .map(|m| ChainOp::Flush {
            mode: if m.full {
                FlushMode::Always
            } else {
                FlushMode::IndexOnly
            },
            label: m.label.clone(),
        })
        .collect()
}

/// Whether a reconcile must follow (rule 3): some mirror is of a job frozen before the op's last
/// index write, so its mirror wrote a state the node's job did not.
pub fn needs_reconcile(plan: &Plan) -> bool {
    plan.mirrors.iter().any(|m| m.class != OpClass::OpEnd)
}

/// The reconcile flush both sides run.
pub fn reconcile_op() -> ChainOp {
    ChainOp::Flush {
        mode: FlushMode::Always,
        label: RECONCILE.into(),
    }
}

/// Whether an event is a flush's signal: leg B's mirrors move them, so neither side's counts.
fn flush_signal(name: &str) -> bool {
    name == "ChainStateFlushed"
}

fn add(into: &mut FlushCounts, from: &FlushCounts) {
    for (to, map) in [(&mut into.full, &from.full), (&mut into.index_only, &from.index_only)] {
        for (name, n) in map {
            *to.entry(name.clone()).or_default() += n;
        }
    }
}

/// Whether two posts hold the same live state: everything but the flush and storage fields.
fn same_live(a: &Post, b: &Post) -> bool {
    a.down == b.down
        && a.tip == b.tip
        && a.best_header == b.best_header
        && a.best_invalid == b.best_invalid
        && a.candidates == b.candidates
        && a.utxo == b.utxo
        && a.index == b.index
        && a.ibd == b.ibd
        && a.best_cl == b.best_cl
        && a.sporks == b.sporks
        && a.tips == b.tips
        && a.deployments == b.deployments
}

/// One side's op step `o` and its auxiliary steps `aux` (mirrors, reconcile) as one step (D53),
/// the same function for both sides:
///
/// - each aux step must be a FLUSH's success with no records and no `persisted`, else it fails
///   closed ([`LegBEnd::Unexpected`]);
/// - the reply and records are the op's; the events are the op's then the aux steps', with flush
///   signals removed;
/// - `persisted` is the op's (the load's view of the storage);
/// - `flush_delta` is `delta` if given (the node's tally, which spans the op and its reconcile),
///   else the op's and the aux steps' summed (Core's);
/// - the rest of the post is the last step's; every aux step's live state must be the op's: a
///   flush moves nothing live;
/// - `tie` is the union.
pub fn reduce(o: &Step, aux: &[Step], delta: Option<&FlushCounts>) -> Result<Step, LegBEnd> {
    let success = Ok(json!({"ok": true, "state": "Valid"}));
    for a in aux {
        if a.reply.outcome != success || !a.reply.checked.is_empty() {
            return Err(LegBEnd::Unexpected("legb aux reply".into()));
        }
        if !a.records.is_empty() || a.post.persisted.is_some() {
            return Err(LegBEnd::Unexpected("legb aux step".into()));
        }
        if !same_live(&a.post, &o.post) {
            return Err(LegBEnd::Unexpected("legb aux moved the live post".into()));
        }
    }
    let last = aux.last().unwrap_or(o);
    let mut post = last.post.clone();
    post.persisted = o.post.persisted.clone();
    post.flush_delta = match delta {
        Some(d) => d.clone(),
        None => {
            let mut sum = o.post.flush_delta.clone();
            for a in aux {
                add(&mut sum, &a.post.flush_delta);
            }
            sum
        }
    };
    let mut tie: Vec<u32> =
        o.post.tie.iter().chain(aux.iter().flat_map(|a| &a.post.tie)).copied().collect();
    tie.sort_unstable();
    tie.dedup();
    post.tie = tie;
    let events = o
        .events
        .iter()
        .chain(aux.iter().flat_map(|a| &a.events))
        .filter(|e| !flush_signal(&e.name))
        .cloned()
        .collect();
    Ok(Step {
        reply: o.reply.clone(),
        records: o.records.clone(),
        events,
        post,
    })
}

/// The node's facts for its reduced step (D53): `facts` are its op step `o`'s, whose events
/// `reduced` holds without their flush signals. The queued count is the reduced step's, the
/// basis Core's reduced step is counted on ([`super::diff::refused`]); the teardown prefix
/// counts only the teardown events the reduced step kept; Core's physical queue,
/// `core_unreduced`, goes only to the drain order ([`super::diff::as_judged`]).
pub fn reduce_facts(
    o: &Step,
    facts: NodeFacts,
    reduced: &Step,
    core_unreduced: &Step,
) -> NodeFacts {
    let kept = facts.teardown.min(o.events.len());
    NodeFacts {
        queued: Facts::core(reduced).queued,
        teardown: o.events[..kept].iter().filter(|e| !flush_signal(&e.name)).count(),
        core_queued_unreduced: Some(Facts::core(core_unreduced).queued),
        ..facts
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dash_oracle_sys::{
        BlockAt, ChainHash, CheckResult, Checked, Event, Persisted, Post, Reply, SporkState,
    };
    use dashd_chainstate::flush::{FlushCauses, FlushTally, Stamp};
    use serde_json::{Value, json};

    use super::*;
    use crate::Divergence;
    use crate::chain::diff::{Judgement, QUEUE_LIMIT, judge};
    use crate::chain::node::NodeStep;
    use crate::chain::run::Refused;

    fn counts(full: &[(&str, u64)], index_only: &[(&str, u64)]) -> FlushCounts {
        let map = |v: &[(&str, u64)]| v.iter().map(|&(k, n)| (k.to_owned(), n)).collect();
        FlushCounts {
            full: map(full),
            index_only: map(index_only),
        }
    }

    fn post(delta: FlushCounts) -> Post {
        serde_json::from_value(json!({
            "down": false, "flush_delta": {"full": {}, "index_only": {}}, "tie": [],
        }))
        .map(|mut p: Post| {
            p.flush_delta = delta;
            p
        })
        .expect("a post")
    }

    fn step(outcome: Value, delta: FlushCounts) -> Step {
        Step {
            reply: Reply {
                checked: Vec::new(),
                outcome: Ok(outcome),
            },
            records: Vec::new(),
            events: Vec::new(),
            post: post(delta),
        }
    }

    fn flushed(label: &str) -> Step {
        step(json!({"ok": true, "state": "Valid"}), counts(&[(label, 1)], &[]))
    }

    fn entry(full: bool, cause: &str, booked: u64, class: OpClass) -> (JournalEntry, OpClass) {
        (
            JournalEntry {
                full,
                cause: cause.into(),
                causes: cause
                    .split('|')
                    .filter_map(FlushCauses::from_name)
                    .fold(FlushCauses::empty(), |a, c| a | c),
                label: None::<Arc<str>>,
                stamp: Stamp::default(),
                booked_in_generation: booked,
                forced: false,
            },
            class,
        )
    }

    fn forced(mut e: (JournalEntry, OpClass)) -> (JournalEntry, OpClass) {
        e.0.forced = true;
        e
    }

    fn end(g0: u64, g1: u64, entries: Vec<(JournalEntry, OpClass)>) -> OpEnd {
        OpEnd {
            g0,
            g1,
            entries,
        }
    }

    fn mirror(causes: FlushCauses, labelled: bool, class: OpClass) -> Mirror {
        Mirror {
            full: true,
            label: "x".into(),
            causes,
            labelled,
            class,
        }
    }

    /// Every step counts toward the share's denominator; only a policy mirror of a natural cause
    /// counts toward its numerator, once however many; LEFT_IBD and explicit flushes are left
    /// out of it; a mid-op mirror counts its reconcile.
    #[test]
    fn rows_count_the_share_and_the_mirrors() {
        let rows = |mirrors| {
            rows(&Plan {
                mirrors,
            })
        };
        assert_eq!(rows(Vec::new()), [LEGB_STEPS]);
        assert_eq!(
            rows(vec![
                mirror(FlushCauses::THRESHOLD, false, OpClass::OpEnd),
                mirror(FlushCauses::CACHE_CRITICAL | FlushCauses::DMAX, false, OpClass::OpEnd),
            ]),
            [LEGB_STEPS, LEGB_NATURAL_STEPS, "legb_mirror_threshold", "legb_mirror_cache"]
        );
        assert_eq!(
            rows(vec![mirror(FlushCauses::LEFT_IBD, false, OpClass::OpEnd)]),
            [LEGB_STEPS, "legb_left_ibd"]
        );
        assert_eq!(
            rows(vec![mirror(FlushCauses::THRESHOLD | FlushCauses::ALWAYS, true, OpClass::MidOp)]),
            [LEGB_STEPS, "legb_reconcile_delayed"]
        );
        assert_eq!(
            rows(vec![mirror(FlushCauses::EMPTY, false, OpClass::MidOp)]),
            [LEGB_STEPS, LEGB_NATURAL_STEPS, "legb_reconcile_delayed"]
        );
    }

    /// A booking Core also made is matched, one Core lacks is mirrored under the node's label in
    /// completion order, and only a mid-op one asks for a reconcile.
    #[test]
    fn unmatched_bookings_are_mirrored_in_order() {
        let core = step(Value::Null, counts(&[("ALWAYS", 1)], &[]));
        let e = end(
            1,
            1,
            vec![
                entry(true, "THRESHOLD", 1, OpClass::MidOp),
                entry(true, "ALWAYS", 1, OpClass::OpEnd),
                forced(entry(false, "PERIODIC_FLUSH", 1, OpClass::OpEnd)),
            ],
        );
        let plan = boundaries(false, &core, false, &e).expect("a plan");
        assert_eq!(
            mirrors(&plan),
            [
                ChainOp::Flush {
                    mode: FlushMode::Always,
                    label: "THRESHOLD".into()
                },
                ChainOp::Flush {
                    mode: FlushMode::IndexOnly,
                    label: "PERIODIC_FLUSH".into()
                },
            ]
        );
        assert!(needs_reconcile(&plan));
        let op_end = end(1, 1, vec![entry(true, "THRESHOLD", 1, OpClass::OpEnd)]);
        assert!(!needs_reconcile(&boundaries(false, &core, false, &op_end).expect("a plan")));
    }

    /// A node-extra booking of a cause Core flags on S0p itself (or a FLUSH op's) is never
    /// mirrored, so the step's `flush_delta` diverges (§2.4 rule 2, OD-L3-131; Opus r1 1).
    #[test]
    fn a_node_extra_core_cause_flush_is_left_to_diverge() {
        let core = step(Value::Null, counts(&[], &[]));
        let mut labelled = entry(true, "THRESHOLD", 1, OpClass::OpEnd);
        labelled.0.label = Some(Arc::from("x"));
        for e in [entry(true, "ALWAYS", 1, OpClass::OpEnd), labelled] {
            let plan =
                boundaries(false, &core, false, &end(1, 1, vec![e.clone()])).expect("a plan");
            assert_eq!(plan, Plan::default(), "{}", e.0.cause);
        }
        let mut node = core.clone();
        node.post.flush_delta = counts(&[("ALWAYS", 1)], &[]);
        let reduced_core = reduce(&core, &[], None).expect("Core reduced");
        let reduced = reduce(&node, &[], Some(&node.post.flush_delta)).expect("node reduced");
        let judged = judge(&reduced_core, &node_step(reduced, NodeFacts::default()), &[]);
        assert!(matches!(judged, Judgement::Diverged(_)), "{judged:?}");
        let plan = boundaries(
            false,
            &core,
            false,
            &end(1, 1, vec![entry(true, "THRESHOLD|CACHE_LARGE", 1, OpClass::OpEnd)]),
        )
        .expect("a plan");
        assert_eq!(plan.mirrors.len(), 1, "an own or size cause is mirrored");
    }

    /// A booking with any cause outside the mirrored set is a node-extra flush however many of
    /// its causes are in it: mirroring it would make the comparison differ (OD-L3-133).
    #[test]
    fn a_booking_with_an_unmirrored_cause_is_not_mirrored() {
        let core = step(Value::Null, counts(&[], &[]));
        for cause in ["ALWAYS|LEFT_IBD", "PERIODIC_FLUSH|THRESHOLD"] {
            let e = entry(true, cause, 1, OpClass::OpEnd);
            assert_eq!(e.0.causes.iter().count(), 2, "{cause} parses");
            let plan = boundaries(false, &core, false, &end(1, 1, vec![e])).expect("a plan");
            assert_eq!(plan, Plan::default(), "{cause}");
        }
    }

    /// An index-only job with no cause, as the tally books it, is Core's periodic write: unforced
    /// it is a node-extra flush, left unmirrored so that the step diverges; the same job forced is
    /// mirrored (OD-L3-142 A).
    #[test]
    fn an_unforced_periodic_write_is_not_mirrored() {
        let booked = |forced: bool| {
            let tally = FlushTally::default();
            tally.opened();
            tally.begin_op();
            let stamp = Stamp {
                generation: tally.generation(),
                mutation_seq: 1,
            };
            tally.book_marked(false, FlushCauses::empty(), None, false, stamp, forced);
            let journal = tally.journal();
            assert_eq!(journal.len(), 1);
            assert_eq!(journal[0].cause, "PERIODIC_WRITE");
            assert_eq!(journal[0].forced, forced);
            (journal[0].clone(), OpClass::OpEnd)
        };
        let core = step(Value::Null, counts(&[], &[]));
        let g = booked(false).0.booked_in_generation;
        let plan =
            boundaries(false, &core, false, &end(g, g, vec![booked(false)])).expect("a plan");
        assert_eq!(plan, Plan::default(), "an unforced periodic write is not mirrored");
        let mut node = core.clone();
        node.post.flush_delta = counts(&[], &[("PERIODIC_WRITE", 1)]);
        let reduced_core = reduce(&core, &[], None).expect("Core reduced");
        let reduced = reduce(&node, &[], Some(&node.post.flush_delta)).expect("node reduced");
        let judged = judge(&reduced_core, &node_step(reduced, NodeFacts::default()), &[]);
        assert!(matches!(judged, Judgement::Diverged(_)), "{judged:?}");
        let plan = boundaries(false, &core, false, &end(g, g, vec![booked(true)])).expect("a plan");
        assert_eq!(
            mirrors(&plan),
            [ChainOp::Flush {
                mode: FlushMode::IndexOnly,
                label: "PERIODIC_WRITE".into()
            }],
            "the same job forced is mirrored"
        );
    }

    /// A load: the teardown's bookings match Core's pre-load ones or the op is unexpected; the
    /// new open's are mirrored after Core's load.
    #[test]
    fn a_load_matches_its_teardown_and_mirrors_the_new_open() {
        let mut core = step(Value::Null, counts(&[("ALWAYS", 2)], &[]));
        core.post.flushes = Some(counts(&[], &[]));
        let e = end(
            1,
            2,
            vec![
                entry(true, "ALWAYS", 1, OpClass::Retired),
                entry(true, "ALWAYS", 1, OpClass::Retired),
                entry(true, "LEFT_IBD", 2, OpClass::OpEnd),
            ],
        );
        let plan = boundaries(true, &core, false, &e).expect("a plan");
        assert_eq!(plan.mirrors.len(), 1);
        assert_eq!(plan.mirrors[0].label, "LEFT_IBD");
        let extra = end(1, 2, vec![entry(true, "THRESHOLD", 1, OpClass::Retired)]);
        assert_eq!(
            boundaries(true, &core, false, &extra),
            Err(LegBEnd::Unexpected("legb: unmatched pre-load flush".into()))
        );
    }

    /// A refused load: the sides' bookings must be the same multiset, else the partial names
    /// the first left over; nothing is mirrored.
    #[test]
    fn a_refused_load_compares_its_cleanup_exactly() {
        let mut core = step(Value::Null, counts(&[("ALWAYS", 4)], &[]));
        core.post.down = true;
        let four = |extra: Option<&str>| {
            let mut v: Vec<_> =
                (0..4).map(|i| entry(true, "ALWAYS", 1 + i / 2, OpClass::Retired)).collect();
            if let Some(label) = extra {
                v.push(entry(true, label, 2, OpClass::Retired));
            }
            end(1, 2, v)
        };
        assert_eq!(boundaries(true, &core, true, &four(None)), Ok(Plan::default()));
        assert_eq!(
            boundaries(true, &core, true, &four(Some("LEFT_IBD"))),
            Err(LegBEnd::RefusedOpenCleanup("LEFT_IBD".into()))
        );
        core.post.flush_delta = counts(&[("ALWAYS", 5)], &[]);
        assert_eq!(
            boundaries(true, &core, true, &four(None)),
            Err(LegBEnd::RefusedOpenCleanup("ALWAYS".into()))
        );
    }

    /// The same `reduce` on both sides: Core's deltas summed, the node's given; flush signals
    /// dropped; `persisted` the op's.
    #[test]
    fn reduce_sums_core_s_deltas_and_drops_flush_signals() {
        let mut o = step(Value::Null, counts(&[("ALWAYS", 1)], &[]));
        o.events = vec![Event {
            name: "ChainStateFlushed".into(),
            args: Vec::new(),
        }];
        let mut mirror = flushed("THRESHOLD");
        mirror.events = vec![
            Event {
                name: "ChainStateFlushed".into(),
                args: Vec::new(),
            },
            Event {
                name: "UpdatedBlockTip".into(),
                args: Vec::new(),
            },
        ];
        let r = reduce(&o, &[mirror], None).expect("reduced");
        assert_eq!(r.post.flush_delta, counts(&[("ALWAYS", 1), ("THRESHOLD", 1)], &[]));
        assert_eq!(
            r.events.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            ["UpdatedBlockTip"]
        );
        let tally = counts(&[("THRESHOLD", 1)], &[]);
        assert_eq!(reduce(&o, &[], Some(&tally)).expect("reduced").post.flush_delta, tally);
        // A reload's op with a mirror after it: the reduced step keeps the op's `persisted`, as
        // an aux step has none.
        o.post.persisted = Some(Persisted {
            best_block: None,
            evo_best: None,
            failed: Vec::new(),
            head_blocks: None,
            index: ChainHash([1; 32]),
            index_entries: 1,
        });
        let r = reduce(&o, &[flushed("LEFT_IBD")], None).expect("reduced");
        assert_eq!(r.post.persisted, o.post.persisted, "the op's");
    }

    /// Ruling (a): a bad aux reply, an aux step with records or `persisted`, or an aux step
    /// that moved the live post fails closed; an extra non-flush aux event reaches the
    /// judgement (and diverges at `events` there).
    #[test]
    fn reduce_fails_closed_on_a_bad_aux_step() {
        let o = step(Value::Null, counts(&[], &[]));
        let bad = step(json!({"ok": false}), counts(&[], &[]));
        assert_eq!(reduce(&o, &[bad], None), Err(LegBEnd::Unexpected("legb aux reply".into())));
        let mut persisted = flushed("THRESHOLD");
        persisted.post.persisted = Some(Persisted {
            best_block: None,
            evo_best: None,
            failed: Vec::new(),
            head_blocks: None,
            index: ChainHash([0; 32]),
            index_entries: 0,
        });
        assert!(reduce(&o, &[persisted], None).is_err());
        let mut moved = flushed("THRESHOLD");
        moved.post.ibd = Some(true);
        assert_eq!(
            reduce(&o, &[moved], None),
            Err(LegBEnd::Unexpected("legb aux moved the live post".into()))
        );
        // A flush moves no ChainLock and no spork either (C-4c-fx-n2).
        let mut moved_cl = flushed("THRESHOLD");
        moved_cl.post.best_cl = Some(Some(BlockAt(1, ChainHash([3; 32]))));
        assert_eq!(
            reduce(&o, &[moved_cl], None),
            Err(LegBEnd::Unexpected("legb aux moved the live post".into()))
        );
        let mut moved_spork = flushed("THRESHOLD");
        moved_spork.post.sporks = Some(vec![SporkState(19, 0, true)]);
        assert_eq!(
            reduce(&o, &[moved_spork], None),
            Err(LegBEnd::Unexpected("legb aux moved the live post".into()))
        );
        let mut extra = flushed("THRESHOLD");
        extra.events.push(Event {
            name: "BlockConnected".into(),
            args: Vec::new(),
        });
        let r = reduce(&o, &[extra], None).expect("reduced");
        assert_eq!(r.events.len(), 1, "the extra event is kept for the judgement");
        let mut recorded = flushed("THRESHOLD");
        recorded.records.push(dash_statedigest::Record::Sparse(
            dash_statedigest::Sparse::new(
                1,
                dash_statedigest::Hash256::from_display_bytes([1; 32]),
                "getblock",
                Vec::new(),
                dash_statedigest::Value::Null,
            )
            .expect("a record"),
        ));
        assert_eq!(reduce(&o, &[recorded], None), Err(LegBEnd::Unexpected("legb aux step".into())));
        let mut checked = flushed("THRESHOLD");
        checked.reply.checked.push(Checked(ChainHash([2; 32]), CheckResult::Valid, None, None));
        assert_eq!(reduce(&o, &[checked], None), Err(LegBEnd::Unexpected("legb aux reply".into())));
    }

    /// Every aux step's live post is held to the op's, not only the last's: a first mirror that
    /// moved `ibd` and a later one that moved it back fail closed (Sol r1 2).
    #[test]
    fn reduce_checks_every_aux_step_s_live_post() {
        let o = step(Value::Null, counts(&[], &[]));
        let mut first = flushed("THRESHOLD");
        first.post.ibd = Some(true);
        let mut later = flushed("reconcile");
        later.post.ibd = o.post.ibd;
        assert_eq!(
            reduce(&o, &[first, later.clone()], None),
            Err(LegBEnd::Unexpected("legb aux moved the live post".into()))
        );
        assert!(reduce(&o, &[later], None).is_ok());
    }

    fn node_step(step: Step, facts: NodeFacts) -> NodeStep {
        NodeStep {
            step,
            facts,
            proposal_class: None,
            legb: None,
        }
    }

    fn signal(name: &str, n: i64) -> Event {
        Event {
            name: name.into(),
            args: vec![json!(n)],
        }
    }

    /// The reduced steps' queue facts are counted on one basis, the retained events (Sol r1 1):
    /// twelve callbacks and a mirrored flush on both sides are a queue drain both show, where the
    /// node's raw count (the flush signal kept) would have judged it the same.
    #[test]
    fn a_mirrored_flush_over_the_limit_is_a_queue_drain_on_both_sides() {
        let mut op = step(Value::Null, counts(&[], &[]));
        op.events = (0..12).map(|i| signal("BlockConnected", i)).collect();
        let mut node = op.clone();
        node.events.push(signal("ChainStateFlushed", 0));
        let core = reduce(&op, &[flushed("THRESHOLD")], None).expect("Core reduced");
        let delta = counts(&[("THRESHOLD", 1)], &[]);
        let reduced = reduce(&node, &[], Some(&delta)).expect("node reduced");
        let raw = NodeFacts {
            queued: Facts::core(&node).queued,
            ..NodeFacts::default()
        };
        let facts = reduce_facts(&node, raw, &reduced, &op);
        assert_eq!((raw.queued, facts.queued, Facts::core(&core).queued), (13, 12, 12));
        let judged = judge(&core, &node_step(reduced, facts), &[]);
        assert!(matches!(judged, Judgement::Refused(Refused::QueueDrain, _)), "{judged:?}");
    }

    /// The teardown prefix counts only the teardown events the reduction kept (Sol r1 1): with
    /// its two flush signals gone, a reload whose Core interleaved its callbacks with the sync
    /// signals is the node's drained-at-end order, not an event divergence.
    #[test]
    fn the_teardown_prefix_counts_retained_events_only() {
        let teardown = [signal("ChainStateFlushed", 0), signal("ChainStateFlushed", 1)];
        let callbacks: Vec<Event> = (0..12).map(|i| signal("BlockConnected", i)).collect();
        let (sync1, sync2) = (signal("AcceptedBlockHeader", 1), signal("AcceptedBlockHeader", 2));
        let mut core = step(Value::Null, counts(&[], &[]));
        core.events = teardown.to_vec();
        core.events.extend([sync1.clone(), callbacks[0].clone(), sync2.clone()]);
        core.events.extend(callbacks[1..].iter().cloned());
        let mut node = core.clone();
        node.events = teardown.to_vec();
        node.events.extend([sync1, sync2]);
        node.events.extend(callbacks);
        let raw = NodeFacts {
            queued: 14,
            teardown: 2,
            ..NodeFacts::default()
        };
        let reduced_core = reduce(&core, &[], None).expect("Core reduced");
        let reduced = reduce(&node, &[], None).expect("node reduced");
        let facts = reduce_facts(&node, raw, &reduced, &core);
        assert_eq!((facts.queued, facts.teardown, facts.core_queued_unreduced), (12, 0, Some(14)));
        let judged = judge(&reduced_core, &node_step(reduced.clone(), facts), &[]);
        assert!(matches!(judged, Judgement::Refused(Refused::QueueDrain, _)), "{judged:?}");
        let stale = NodeFacts {
            teardown: 2,
            ..facts
        };
        let judged = judge(&reduced_core, &node_step(reduced, stale), &[]);
        assert!(matches!(judged, Judgement::Diverged(_)), "the stale prefix: {judged:?}");
    }

    /// The drain order follows Core's physical queue, not its reduced count (OD-L3-142 C): ten
    /// callbacks and a flush signal are past [`QUEUE_LIMIT`] in Core's op step, so a callback it
    /// heard before a sync signal is a drain inside the op, while the reduced steps' ten are not
    /// a refusal on either side.
    #[test]
    fn the_drain_order_reads_cores_queue_before_the_reduction() {
        let callbacks: Vec<Event> = (0..10).map(|i| signal("BlockConnected", i)).collect();
        let sync = signal("AcceptedBlockHeader", 1);
        let mut core = step(Value::Null, counts(&[], &[]));
        core.events = vec![callbacks[0].clone(), sync.clone()];
        core.events.extend(callbacks[1..].iter().cloned());
        core.events.push(signal("ChainStateFlushed", 0));
        let mut node = core.clone();
        node.events = [vec![sync], callbacks].concat();
        let reduced_core = reduce(&core, &[], None).expect("Core reduced");
        let reduced = reduce(&node, &[], None).expect("node reduced");
        let raw = NodeFacts {
            queued: Facts::core(&node).queued,
            ..NodeFacts::default()
        };
        let facts = reduce_facts(&node, raw, &reduced, &core);
        assert_eq!(
            (Facts::core(&core).queued, Facts::core(&reduced_core).queued, facts.queued),
            (QUEUE_LIMIT + 1, QUEUE_LIMIT, QUEUE_LIMIT)
        );
        assert_eq!(facts.core_queued_unreduced, Some(QUEUE_LIMIT + 1));
        let judged = judge(&reduced_core, &node_step(reduced.clone(), facts), &[]);
        assert!(matches!(judged, Judgement::Same(_)), "{judged:?}");
        let unreduced_unknown = NodeFacts {
            core_queued_unreduced: None,
            ..facts
        };
        let judged = judge(&reduced_core, &node_step(reduced, unreduced_unknown), &[]);
        assert_eq!(
            judged,
            Judgement::Diverged(Divergence {
                field: "events"
            })
        );
    }
}
