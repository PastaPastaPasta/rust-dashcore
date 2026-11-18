//! ADD §2.6 6-D's scan rows, on real threads (EV addendum §2.6; CH7-notes D87; CH7b-2 C13):
//! route B's `ScanQuorums` of the rotated type from V's tip finds index 0's quorum of cycle 24,
//! mined at 36, with no quorum object cached, so its build's members call recomputes the cycle
//! and writes its row inline; an attempt's members call asks for the same cycle through route C,
//! in CH7b-1's harness ([`send_attempt_on`]). At each of the three positions, with each way the
//! attempt ends, both finish within a bound and the attempt and the scan each answer as they do
//! alone. With the scan paused before its inline write, a route-A reader sees no row, and nothing
//! is cached: no roster, no quorum object.

use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::{Arc, mpsc};

use dash_consensus::{BlockId, BlsCtx};
use dash_dml::StoreError;
use dash_evo::Effect;
use dash_evo::llmq::{CompleteChain, LlmqCtx, QuorumSnapshot};
use dash_evo::types::ProTxHash;
use dash_types::Numeric as _;

use super::llmq::{WAIT, wait_until, waiters};
use super::llmq_members::{ROTATED, pros};
use super::on_v_binding::{CYCLE, Lists, Setup, TIP, scan_b};
use super::routes::{
    Ask, Attempted, ENDS, Ends, Point, pause, queued, read_row, row, send_attempt_on,
};
use crate::chain::reply::ActorReply;
use crate::chain::{ConnectCx, Fatal};
use crate::evo::node::Ancestry;
use crate::llmq::facade::AttemptHandle;
use crate::llmq::members::members;
use crate::llmq::quorum::QuorumPtr;

/// What a scan answered: each quorum's base and roster, newest first.
type Answer = Vec<(BlockId, Vec<ProTxHash>)>;

/// Cycle 24's row and roster as LQ1's model gives them over V's chain, and the scan's answer.
struct Expect {
    row: QuorumSnapshot,
    roster: Vec<ProTxHash>,
    scan: Answer,
}

fn expect() -> Expect {
    let s = Setup::new();
    let (mut rosters, rows) = s.model(vec![(ROTATED, s.at(CYCLE))]);
    let row = rows.0.get(&(ROTATED.to_base(), s.hash(CYCLE))).cloned().expect("the model's row");
    let roster = rosters.pop().expect("one call");
    Expect {
        row,
        scan: vec![(s.at(CYCLE), roster.clone())],
        roster,
    }
}

/// The attempt's call: cycle 24's members on V's chain, over the seeded lists and the published
/// scheme, as the scan's build asks for them.
fn ask_cycle(cycle: BlockId) -> Ask {
    Box::new(move |handle: &AttemptHandle<'_>, cx: &ConnectCx<'_>, log: &mut Vec<Effect>| {
        let chain = Ancestry(cx.reader);
        let complete = CompleteChain::<StoreError>::new(&chain);
        let ctx =
            LlmqCtx::new(cx.params, &complete, cx.deployments, BlsCtx::BASIC).expect("in domain");
        let src = Lists {
            reader: cx.reader,
        };
        let got = members(handle, &ctx, &src, log, ROTATED, cycle, false);
        Ok(pros(&got.map_err(|e| Fatal::new(format!("{e:?}")))?))
    })
}

/// Sends `s`'s V an attempt ended as `ends` that asks for cycle 24.
fn send_attempt(
    s: &Setup,
    ends: Ends,
    open: Option<Point>,
) -> (mpsc::Receiver<Result<Attempted, Fatal>>, Arc<AtomicBool>) {
    let ask = ask_cycle(s.at(CYCLE));
    send_attempt_on(&s.rig, &s.w, &s.blocks[0], ends, open, None, ask)
}

/// Route B's scan of the rotated type from the tip, for one quorum: cycle 24's index 0.
fn scan(s: &Setup) -> ActorReply<Result<Vec<QuorumPtr>, String>> {
    scan_b(s, ROTATED, s.at(TIP), 1)
}

fn answer(quorums: &[QuorumPtr]) -> Answer {
    quorums.iter().map(|q| (q.base, pros(&q.members))).collect()
}

/// Cycle 24's row on disk, the roster cached under its base, and whether its quorum object is
/// cached.
fn state(s: &Setup) -> (Option<QuorumSnapshot>, Option<Vec<ProTxHash>>, bool) {
    let cycle = s.hash(CYCLE);
    let on_disk = s.w.store.rows.lock().get(&(ROTATED.to_base(), cycle)).cloned();
    let cached = s.w.facade.test_caches().by_base.peek(ROTATED, &cycle).map(|m| pros(m));
    let object = s.w.facade.test_quorums().expect("free").objects.peek(ROTATED, &cycle).is_some();
    (on_disk, cached, object)
}

impl Expect {
    /// The state before a run: no row, no roster, no object.
    fn before() -> (Option<QuorumSnapshot>, Option<Vec<ProTxHash>>, bool) {
        (None, None, false)
    }

    /// The state after a run: the row, the roster and the scan's object.
    fn after(&self) -> (Option<QuorumSnapshot>, Option<Vec<ProTxHash>>, bool) {
        (Some(self.row.clone()), Some(self.roster.clone()), true)
    }
}

/// The scan alone: it computes the cycle, writes its row and caches the roster and the object;
/// each attempt alone answers the roster and logs the row.
fn alone(e: &Expect) -> [Attempted; 3] {
    let s = Setup::new();
    assert_eq!(state(&s), Expect::before());
    let got = scan(&s).recv_timeout(WAIT).expect("the scan is served").expect("answered");
    assert_eq!(answer(&got.expect("no fault")), e.scan);
    assert!(s.reads.load(SeqCst) > 0, "the scan computed the cycle");
    assert_eq!((state(&s), s.w.store.writes.load(SeqCst)), (e.after(), 1));
    ENDS.map(|ends| {
        let s = Setup::new();
        let (outcome, _) = send_attempt(&s, ends, None);
        let alone = outcome.recv_timeout(WAIT).expect("the attempt ends").expect("no fault");
        assert_eq!(alone, Attempted::alone(ends, e.roster.clone()), "{ends:?}");
        let (row, roster, object) = state(&s);
        assert_eq!((row, roster), (Some(e.row.clone()), Some(e.roster.clone())), "{ends:?}");
        assert!(!object, "a members call builds no quorum object");
        alone
    })
}

/// 6-D (a), scan route: a scan dispatched while the attempt is open waits in V's lane until the
/// attempt is released; the attempt ends as it does alone, and the scan's build then hits the
/// roster it cached, reading no source and writing no row, and answers as it does alone.
#[test]
fn case_6d_a_a_scan_dispatched_into_an_open_attempt_builds_on_its_roster() {
    let e = expect();
    let alone = alone(&e);
    for (ends, alone) in ENDS.into_iter().zip(&alone) {
        let s = Setup::new();
        let (pause, open) = pause();
        let (outcome, _) = send_attempt(&s, ends, Some(open));
        pause.reached("the attempt opens");
        assert!(s.w.facade.gate().is_locked_exclusive(), "the attempt's gate, {ends:?}");
        let reads = s.reads.load(SeqCst);
        let b = scan(&s);
        queued(&s.rig, &s.w.facade, &b, "the scan");
        pause.resume();
        let attempted = outcome.recv_timeout(WAIT).expect("the attempt ends").expect("no fault");
        assert_eq!(attempted, *alone, "{ends:?}");
        let got = b.recv_timeout(WAIT).expect("the scan is served").expect("answered");
        assert_eq!(answer(&got.expect("no fault")), e.scan, "{ends:?}");
        assert_eq!(s.reads.load(SeqCst), reads, "the scan's build read a source");
        assert_eq!((state(&s), s.w.store.writes.load(SeqCst)), (e.after(), 1), "{ends:?}");
    }
}

/// 6-D (b), scan route: the scan's build computes the cycle and waits before its inline write,
/// holding the gate shared, with an attempt queued behind it in V's lane. A route-A read then
/// finds no row, and neither the roster nor the object is cached. Once the scan writes, it
/// answers as it does alone, and the attempt, served after it, hits the roster with the result
/// it has alone, logging no row.
#[test]
fn case_6d_b_an_attempt_queued_behind_a_paused_scan_hits_its_roster() {
    let e = expect();
    let alone = alone(&e);
    for (ends, alone) in ENDS.into_iter().zip(&alone) {
        let s = Setup::new();
        let (pause, before_write) = pause();
        *s.w.facade.hooks.before_inline_write.lock() = Some(before_write);
        let b = scan(&s);
        pause.reached("the scan reaches its inline write");
        assert!(s.w.facade.gate().is_locked() && !s.w.facade.gate().is_locked_exclusive());
        let (outcome, attempt_started) = send_attempt(&s, ends, None);
        assert_eq!(s.rig.v.bounded_lane().len(), 1, "the attempt waits in V's lane");
        assert!(!attempt_started.load(SeqCst), "V started the attempt");
        let read = row(read_row(&s.w.facade, s.hash(CYCLE)).join().expect("route A"));
        assert_eq!(read, None, "route A, before the scan's write");
        assert_eq!(state(&s), Expect::before(), "the roster or object before the row");

        pause.resume();
        let got = b.recv_timeout(WAIT).expect("the scan is served").expect("answered");
        assert_eq!(answer(&got.expect("no fault")), e.scan, "{ends:?}");
        let attempted = outcome.recv_timeout(WAIT).expect("the attempt ends").expect("no fault");
        assert_eq!(attempted.result(), alone.result(), "{ends:?}");
        assert_eq!(attempted.stores, 0, "a hit logs no row");
        assert_eq!((state(&s), s.w.store.writes.load(SeqCst)), (e.after(), 1), "{ends:?}");
    }
}

/// 6-D (c), scan route: a caller holds a route-A token while an attempt queues on the gate and a
/// scan queues behind the attempt. The token finds no row, and nothing is cached; once it is
/// dropped, the attempt ends as it does alone, and the scan's build hits its roster and answers
/// as it does alone.
#[test]
fn case_6d_c_a_scan_queued_behind_an_attempt_on_a_held_token_runs_once_it_drops() {
    let e = expect();
    let alone = alone(&e);
    for (ends, alone) in ENDS.into_iter().zip(&alone) {
        let s = Setup::new();
        let token = s.w.facade.external().expect("untripped");
        let (outcome, attempt_started) = send_attempt(&s, ends, None);
        wait_until("the attempt parks on the gate", || {
            attempt_started.load(SeqCst) && waiters(&s.w.facade) == 1
        });
        let reads = s.reads.load(SeqCst);
        let b = scan(&s);
        queued(&s.rig, &s.w.facade, &b, "the scan");
        let read = token.rows().snapshot_for_block(ROTATED, &s.hash(CYCLE));
        assert_eq!(read.expect("the read"), None, "the token's read");
        assert_eq!(state(&s), Expect::before());

        drop(token);
        let attempted = outcome.recv_timeout(WAIT).expect("the attempt ends").expect("no fault");
        assert_eq!(attempted, *alone, "{ends:?}");
        let got = b.recv_timeout(WAIT).expect("the scan is served").expect("answered");
        assert_eq!(answer(&got.expect("no fault")), e.scan, "{ends:?}");
        assert_eq!(s.reads.load(SeqCst), reads, "the scan's build read a source");
        assert_eq!((state(&s), s.w.store.writes.load(SeqCst)), (e.after(), 1), "{ends:?}");
    }
}
