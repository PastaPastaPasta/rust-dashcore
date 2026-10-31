//! ADD §2.6 case 6 and 6-D's members route, on real threads (EV addendum §2.6; CH7b-1 PLAN Q8):
//! NodeEvo's attempt asks for cycle K's members on V, its hooks called as the connector calls
//! them, while a caller reads K's row through route A on a thread of its own and a route-B call
//! for K's members and row waits in V's lane. A reader sees K's members and its row together or
//! not at all, every run ends within a bound, and each attempt ends as it ends alone. 6-D's scan
//! route and composite builders move to CH7b-2 and the builder slice (Q8). Case 7 interleaves a
//! route-A read of K's row with route B's write of it, in the runtime rows' section (PLAN F1).

use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use dash_consensus::Block;
use dash_dml::StoreError;
use dash_evo::llmq::QuorumSnapshot;
use dash_evo::types::ProTxHash;
use dash_primitives::{BlockHash, LlmqType};
use dash_types::Numeric as _;

use super::llmq::{WAIT, wait_until, waiters};
use super::llmq_members::{Fixture, ROTATED, World, pros, rotate, world};
use super::node_evo::{Evo, NoCoins, at, bodies, node_evo_with_snapshots};
use super::node_evo_lists::bls;
use super::support::{Mined, Rig, RigConfig, branch, with_tip_cx};
use crate::chain::reply::ActorReply;
use crate::chain::{
    AttemptOrigin, ChainCommand, ConnectCx, EvoTarget, Fatal, VContext, ViewTarget,
};
use crate::connect::{AttemptId, BlockAt, EvoHooks as _, SpecialTxRules, pre_dip3_special_txs};
use crate::evo::effects::{SnapshotApplier, StoreSnapshots};
use crate::llmq::capability::{Capability, Ungated};
use crate::llmq::facade::MembersFacade;
use crate::llmq::members::members;

/// The cycle every run reads: LLMQ_TEST_DIP0024's at 48, whose two quorums' base blocks are 48
/// and 49.
const K: u32 = 48;

/// How long a reply that must not come is waited for, once its waiter is seen stuck.
const HANG: Duration = Duration::from_secs(2);

/// How the attempt ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ends {
    /// A tip connect H4 passes, committed.
    Succeeds,
    /// A block H4 refuses, rolled back.
    Fails,
    /// A proposal (`fJustCheck`), rolled back.
    Proposal,
}

const ENDS: [Ends; 3] = [Ends::Succeeds, Ends::Fails, Ends::Proposal];

/// The entry route B's call hits once the attempt has cached K: quorum 0's, under its base
/// block's own key, or quorum 1's indexed entry, whose base block has no entry of its own.
#[derive(Clone, Copy, Debug)]
enum Hit {
    Base,
    Indexed,
}

impl Hit {
    fn quorum(self) -> usize {
        match self {
            Hit::Base => 0,
            Hit::Indexed => 1,
        }
    }

    fn height(self) -> u32 {
        K + u32::try_from(self.quorum()).expect("small")
    }
}

/// K's quorums and row as its rotation gives them (M1, S1), and the row a smaller list gives
/// (S0), standing for the one an earlier connect of K stored.
struct Expect {
    quorums: Vec<Vec<ProTxHash>>,
    s1: QuorumSnapshot,
    s0: QuorumSnapshot,
}

fn expect() -> Expect {
    let fx = Fixture::rotated();
    fx.ctx(|ctx| {
        let (quorums, s1) = rotate(&fx, ctx, &fx.source(), K);
        let smaller = (36..=K).map(|h| (h, (1..=6).collect())).collect();
        let (_, s0) = rotate(&fx, ctx, &fx.source_with(smaller), K);
        assert_ne!(s0, s1);
        Expect {
            quorums,
            s1,
            s0,
        }
    })
}

/// A V serving route B on a facade whose rows hold S0 for K and whose caches hold nothing, and
/// the block an attempt connects.
struct Run {
    w: World,
    rig: Rig,
    blocks: Vec<Mined>,
}

impl Run {
    fn new(e: &Expect) -> Run {
        let run = Run::cold(e);
        run.w.rows.write(ROTATED, &k_hash(), &e.s0).expect("S0");
        run
    }

    /// [`Run::new`], S0 on disk only.
    fn cold(e: &Expect) -> Run {
        let fx = Fixture::rotated();
        let w = world(&fx.params);
        w.store.rows.lock().insert((ROTATED.to_base(), k_hash()), e.s0.clone());
        let rig = Rig::with(RigConfig {
            facade: Some(w.facade.clone()),
            ..RigConfig::default()
        });
        let blocks = branch(&rig.p, &rig.genesis, 1, 11);
        rig.headers(&blocks);
        Run {
            w,
            rig,
            blocks,
        }
    }

    /// K's row on disk.
    fn on_disk(&self) -> Option<QuorumSnapshot> {
        self.w.store.rows.lock().get(&(ROTATED.to_base(), k_hash())).cloned()
    }

    /// K's quorum-0 members, cached under K.
    fn cached(&self) -> Option<Vec<ProTxHash>> {
        self.w.facade.test_caches().by_base.peek(ROTATED, &k_hash()).map(|m| pros(m))
    }

    /// Asserts that `reply`'s command waits in V's lane: accepted, not started, unanswered.
    fn queued<T>(&self, reply: &ActorReply<T>, what: &str) {
        assert_eq!(self.rig.v.bounded_lane().len(), 1, "{what} waits in V's lane");
        assert_eq!(started(&self.w.facade), 0, "V started {what}");
        assert!(reply.try_recv().is_none(), "{what} was answered");
    }
}

fn k_hash() -> BlockHash {
    Fixture::rotated().hash(K)
}

fn started(facade: &MembersFacade) -> usize {
    facade.hooks.on_v_started.load(SeqCst)
}

/// A pause point, run once on the paused thread, and the test's side of it.
type Point = Box<dyn Fn() + Send + Sync>;

struct Pause {
    reached: mpsc::Receiver<()>,
    resume: mpsc::Sender<()>,
}

fn pause() -> (Pause, Point) {
    let (at_point, reached) = mpsc::channel();
    let (resume, go) = mpsc::channel();
    let go = Mutex::new(go);
    let point: Point = Box::new(move || {
        at_point.send(()).expect("the test waits");
        go.lock().expect("one point").recv().expect("the test resumes it");
    });
    (
        Pause {
            reached,
            resume,
        },
        point,
    )
}

impl Pause {
    fn reached(&self, what: &str) {
        self.reached.recv_timeout(WAIT).unwrap_or_else(|e| panic!("{what}: {e}"));
    }

    fn resume(&self) {
        self.resume.send(()).expect("paused");
    }
}

/// The production snapshot applier, pausing once at `before` before it applies a row.
struct PausedRows {
    inner: StoreSnapshots,
    before: Option<Point>,
}

impl SnapshotApplier for PausedRows {
    fn write(
        &mut self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
        snapshot: &QuorumSnapshot,
    ) -> Result<(), Fatal> {
        if let Some(before) = self.before.take() {
            before();
        }
        self.inner.write(llmq_type, cycle_base, snapshot)
    }
}

/// What an attempt did: H4's verdict, the members of K it was answered, and the rows its call
/// logged.
#[derive(Debug, PartialEq, Eq)]
struct Attempted {
    valid: bool,
    members: Vec<ProTxHash>,
    stores: usize,
}

impl Attempted {
    /// What the attempt answered, whoever computed K.
    fn result(&self) -> (bool, &[ProTxHash]) {
        (self.valid, &self.members)
    }
}

/// Sends V one NodeEvo attempt over `run`'s facade, ended as `ends`, whose log goes through the
/// production snapshot applier over the facade's rows. `open` runs once the attempt holds the
/// gate; the applier runs `before_rows` before it applies the log's row. The attempt's outcome,
/// and whether V has started it.
fn send_attempt(
    run: &Run,
    ends: Ends,
    open: Option<Point>,
    before_rows: Option<Point>,
) -> (mpsc::Receiver<Result<Attempted, Fatal>>, Arc<AtomicBool>) {
    let (done, outcome) = mpsc::channel();
    let started = Arc::new(AtomicBool::new(false));
    let (p, facade, rows, began) =
        (run.rig.p.clone(), run.w.facade.clone(), run.w.rows.clone(), started.clone());
    let (gid, id) = (run.rig.id(&run.rig.genesis), run.rig.id(&run.blocks[0]));
    let tip = at(&run.blocks[0], id, gid);
    let (block, at) = match ends {
        Ends::Succeeds => (run.blocks[0].block.clone(), tip),
        Ends::Fails => {
            let invalid = bodies().into_iter().find(|b| pre_dip3_special_txs(b, true).is_err());
            (Arc::new(invalid.expect("one")), tip)
        }
        Ends::Proposal => (
            run.blocks[0].block.clone(),
            BlockAt {
                id: None,
                ..tip
            },
        ),
    };
    let cmd = ChainCommand::TestUnderV(Box::new(move |v| {
        began.store(true, SeqCst);
        let applier = PausedRows {
            inner: StoreSnapshots {
                rows,
            },
            before: before_rows,
        };
        let (mut evo, _, _) = node_evo_with_snapshots(&p, bls(&p), Box::new(applier));
        let r = with_tip_cx(v, |vcx, cx| attempt(&mut evo, &facade, vcx, cx, &block, &at, open));
        done.send(r).expect("the test waits");
        Ok(())
    }));
    run.rig.v.send(cmd).expect("V runs");
    (outcome, started)
}

/// One attempt in the connector's order (CH3 §3.1): H10, H1, H4, K's members as CH7b-2's H4
/// will ask for them, then, for a tip connect H4 passed, H6, H1's commit and H10's commit, else
/// the rollback. A block without an index entry is a proposal's.
fn attempt(
    evo: &mut Evo,
    facade: &MembersFacade,
    vcx: &mut VContext,
    cx: &ConnectCx<'_>,
    block: &Block,
    at: &BlockAt,
    open: Option<Point>,
) -> Result<Attempted, Fatal> {
    let origin = if at.id.is_none() {
        AttemptOrigin::Proposal
    } else {
        AttemptOrigin::Tip
    };
    let mut att =
        evo.begin_attempt(facade, vcx, cx, AttemptId::next(), origin, &ViewTarget::Active)?;
    if let Some(open) = open {
        open();
    }
    let mut target = EvoTarget::Active;
    evo.begin_tx(&mut att, &mut target)?;
    evo.bls_ctx(&mut att, &mut target, cx.params, at);
    let just_check = origin == AttemptOrigin::Proposal;
    let rules = SpecialTxRules::default();
    let h4 = evo.process_special_txs(
        &mut att,
        &mut target,
        block,
        at,
        rules,
        &NoCoins,
        0,
        just_check,
        true,
    )?;
    let (handle, sink) = att.capability();
    let (fx, mut effects) = (Fixture::rotated(), Vec::new());
    let got =
        fx.ctx(|ctx| members(handle, ctx, &fx.source(), &mut effects, ROTATED, fx.at(K), false));
    let members = pros(&got.map_err(|e| Fatal::new(format!("{e:?}")))?);
    let stores = effects.len();
    for e in effects {
        sink.emit(e);
    }
    let commit = origin == AttemptOrigin::Tip && h4.is_ok();
    if commit {
        evo.set_best_block(&mut att, &mut target, &at.hash)?;
        evo.commit_bls_scheme(&mut att, &mut target);
    }
    evo.finish_attempt(att)?;
    if commit {
        evo.commit_tx(&mut target)?;
    } else {
        evo.rollback_tx(&mut target);
    }
    Ok(Attempted {
        valid: h4.is_ok(),
        members,
        stores,
    })
}

/// What a route-B call saw: the members of `hit`'s base block, then K's row, and the source
/// reads it made.
#[derive(Debug, PartialEq, Eq)]
struct Seen {
    members: Result<Vec<ProTxHash>, String>,
    row: Result<Option<QuorumSnapshot>, String>,
    reads: usize,
}

/// Route B's reads through `cap`: `hit`'s members, then K's row.
fn read_pair(cap: &impl Capability, hit: Hit) -> Seen {
    let fx = Fixture::rotated();
    let src = fx.source();
    fx.ctx(|ctx| {
        let mut effects = Vec::new();
        let got = members(cap, ctx, &src, &mut effects, ROTATED, fx.at(hit.height()), false);
        let row = cap.rows(&mut effects).get(ROTATED, &k_hash());
        Seen {
            members: got.map(|m| pros(&m)).map_err(|e| format!("{e:?}")),
            row: row.map_err(|e| e.to_string()),
            reads: src.reads(),
        }
    })
}

/// Dispatches route B's reads to V.
fn route_b(run: &Run, hit: Hit) -> ActorReply<Seen> {
    run.rig.v.on_v(move |o, _| read_pair(o, hit))
}

type Read = Result<Result<Option<QuorumSnapshot>, StoreError>, Fatal>;

/// Route A's read of K's row, on a thread of its own.
fn route_a(run: &Run) -> JoinHandle<Read> {
    let facade = run.w.facade.clone();
    std::thread::spawn(move || {
        facade.external().map(|t| t.rows().snapshot_for_block(ROTATED, &k_hash()))
    })
}

fn row(read: Read) -> Option<QuorumSnapshot> {
    read.expect("the gate").expect("the read")
}

/// Each way of ending, run alone: the result a concurrent run must match.
fn solo(e: &Expect) -> [Attempted; 3] {
    ENDS.map(|ends| {
        let run = Run::new(e);
        let (outcome, _) = send_attempt(&run, ends, None, None);
        let alone = outcome.recv_timeout(WAIT).expect("the attempt ends").expect("no fault");
        let valid = ends != Ends::Fails;
        assert_eq!(
            alone,
            Attempted {
                valid,
                members: e.quorums[0].clone(),
                stores: 1
            }
        );
        assert_eq!((run.on_disk(), run.cached()), (Some(e.s1.clone()), Some(e.quorums[0].clone())));
        alone
    })
}

/// Case 6: the attempt recomputes K to (M1, S1) over S0 and waits before its row lands, with
/// M1 cached and the gate held. A route-B call dispatched then waits in V's lane, unstarted and
/// unanswered, and a route-A read of K's row parks on the gate. Once the attempt is released,
/// route A reads S1 and route B gets (M1, S1), for each way the attempt ends and whichever entry
/// route B hits (M1, M2, M5).
#[test]
fn case_6_both_routes_wait_for_the_attempts_row() {
    let e = expect();
    for ends in ENDS {
        for hit in [Hit::Base, Hit::Indexed] {
            let run = Run::new(&e);
            let (pause, before_rows) = pause();
            let (outcome, _) = send_attempt(&run, ends, None, Some(before_rows));
            pause.reached("the attempt reaches its row");
            assert!(run.w.facade.gate().is_locked_exclusive(), "the attempt's gate, {ends:?}");
            let window = (run.cached(), run.on_disk());
            assert_eq!(window, (Some(e.quorums[0].clone()), Some(e.s0.clone())), "M1 with S0");

            let b = route_b(&run, hit);
            run.queued(&b, "route B");
            let a = route_a(&run);
            wait_until("route A parks on the gate", || {
                waiters(&run.w.facade) == 1 || a.is_finished()
            });
            assert!(!a.is_finished(), "route A read in the attempt: {:?}", a.join().map(row));
            run.queued(&b, "route B");

            pause.resume();
            let attempted =
                outcome.recv_timeout(WAIT).expect("the attempt ends").expect("no fault");
            assert_eq!(attempted.members, e.quorums[0], "{ends:?}");
            assert_eq!(attempted.stores, 1);
            assert_eq!(row(a.join().expect("route A")), Some(e.s1.clone()), "{ends:?} {hit:?}");
            let seen = b.recv_timeout(WAIT).expect("route B is served").expect("answered");
            let m1 = e.quorums[hit.quorum()].clone();
            assert_eq!(
                seen,
                Seen {
                    members: Ok(m1),
                    row: Ok(Some(e.s1.clone())),
                    reads: 0
                }
            );
            assert!(run.w.latch.tripped().is_none());
        }
    }
}

/// Case 6's negative control: the same hit and row read with neither V's dispatch nor the gate
/// ([`Ungated`]), made while the attempt waits before its row, see the torn pair (M1, S0).
#[test]
fn case_6_without_the_dispatch_and_the_gate_a_read_sees_m1_with_s0() {
    let e = expect();
    for hit in [Hit::Base, Hit::Indexed] {
        let run = Run::new(&e);
        let (pause, before_rows) = pause();
        let (outcome, _) = send_attempt(&run, Ends::Succeeds, None, Some(before_rows));
        pause.reached("the attempt reaches its row");
        let seen = read_pair(&Ungated(&run.w.facade), hit);
        assert_eq!(
            seen,
            Seen {
                members: Ok(e.quorums[hit.quorum()].clone()),
                row: Ok(Some(e.s0.clone())),
                reads: 0
            }
        );
        pause.resume();
        outcome.recv_timeout(WAIT).expect("the attempt ends").expect("no fault");
    }
}

/// 6-D (a): a route-B call dispatched while the attempt is open waits in V's lane until the
/// attempt is released, then reads the attempt's (M1, S1); the attempt ends as it does alone.
#[test]
fn case_6d_a_a_call_dispatched_into_an_open_attempt_reads_its_members_with_its_row() {
    let e = expect();
    let alone = solo(&e);
    for (ends, alone) in ENDS.into_iter().zip(&alone) {
        let run = Run::new(&e);
        let (pause, open) = pause();
        let (outcome, _) = send_attempt(&run, ends, Some(open), None);
        pause.reached("the attempt opens");
        assert!(run.w.facade.gate().is_locked_exclusive(), "the attempt's gate, {ends:?}");
        let b = route_b(&run, Hit::Base);
        run.queued(&b, "route B");
        pause.resume();
        let attempted = outcome.recv_timeout(WAIT).expect("the attempt ends").expect("no fault");
        assert_eq!(attempted, *alone, "{ends:?}");
        let seen = b.recv_timeout(WAIT).expect("route B is served").expect("answered");
        assert_eq!(
            seen,
            Seen {
                members: Ok(e.quorums[0].clone()),
                row: Ok(Some(e.s1.clone())),
                reads: 0
            }
        );
    }
}

/// 6-D (b): route B's call computes K and waits before its inline write, holding the gate
/// shared, with an attempt queued behind it. Route A meanwhile reads S0, and no members of K
/// are cached; once the call writes, it reads (M1, S1), and the attempt, served after it, hits
/// K's entry with the result it has alone, logging no row.
#[test]
fn case_6d_b_an_attempt_queued_behind_a_paused_call_hits_its_entry() {
    let e = expect();
    let alone = solo(&e);
    for (ends, alone) in ENDS.into_iter().zip(&alone) {
        let run = Run::new(&e);
        let (pause, before_write) = pause();
        *run.w.facade.hooks.before_inline_write.lock() = Some(before_write);
        let b = route_b(&run, Hit::Base);
        pause.reached("route B reaches its inline write");
        assert!(run.w.facade.gate().is_locked() && !run.w.facade.gate().is_locked_exclusive());
        let (outcome, attempt_started) = send_attempt(&run, ends, None, None);
        assert_eq!(run.rig.v.bounded_lane().len(), 1, "the attempt waits in V's lane");
        assert!(!attempt_started.load(SeqCst), "V started the attempt");
        assert_eq!(row(route_a(&run).join().expect("route A")), Some(e.s0.clone()));
        assert_eq!(run.cached(), None, "K's members before its row");

        pause.resume();
        let seen = b.recv_timeout(WAIT).expect("route B is served").expect("answered");
        assert_eq!((seen.members, seen.row), (Ok(e.quorums[0].clone()), Ok(Some(e.s1.clone()))));
        assert!(seen.reads > 0, "route B computed K");
        let attempted = outcome.recv_timeout(WAIT).expect("the attempt ends").expect("no fault");
        assert_eq!(attempted.result(), alone.result(), "{ends:?}");
        assert_eq!(attempted.stores, 0, "a hit logs no row");
        assert_eq!((run.on_disk(), run.cached()), (Some(e.s1.clone()), Some(e.quorums[0].clone())));
    }
}

/// 6-D (c): a caller holds a route-A token while an attempt queues on the gate and a route-B
/// call queues behind the attempt. The token reads S0 with no members of K cached; once it is
/// dropped, the attempt ends as it does alone and route B reads its (M1, S1).
#[test]
fn case_6d_c_an_attempt_queued_on_a_held_token_runs_once_it_drops() {
    let e = expect();
    let alone = solo(&e);
    for (ends, alone) in ENDS.into_iter().zip(&alone) {
        let run = Run::new(&e);
        let token = run.w.facade.external().expect("untripped");
        let (outcome, attempt_started) = send_attempt(&run, ends, None, None);
        wait_until("the attempt parks on the gate", || {
            attempt_started.load(SeqCst) && waiters(&run.w.facade) == 1
        });
        let b = route_b(&run, Hit::Indexed);
        run.queued(&b, "route B");
        let read = token.rows().snapshot_for_block(ROTATED, &k_hash());
        assert_eq!(read.expect("the read"), Some(e.s0.clone()));
        assert_eq!(run.cached(), None, "K's members before its row");

        drop(token);
        let attempted = outcome.recv_timeout(WAIT).expect("the attempt ends").expect("no fault");
        assert_eq!(attempted, *alone, "{ends:?}");
        let seen = b.recv_timeout(WAIT).expect("route B is served").expect("answered");
        assert_eq!(
            seen,
            Seen {
                members: Ok(e.quorums[1].clone()),
                row: Ok(Some(e.s1.clone())),
                reads: 0
            }
        );
    }
}

/// 6-D's queue-after-token control: a caller that holds a route-A token and dispatches a
/// route-B call behind a queued attempt waits forever, as the attempt waits on the caller's
/// token and the call waits behind the attempt. A bounded wait for the reply detects it; the
/// test's escape, dropping the token, lets both finish.
#[test]
fn case_6d_a_call_dispatched_under_a_token_behind_an_attempt_hangs() {
    let e = expect();
    let run = Run::new(&e);
    let token = run.w.facade.external().expect("untripped");
    let (outcome, attempt_started) = send_attempt(&run, Ends::Succeeds, None, None);
    wait_until("the attempt parks on the gate", || {
        attempt_started.load(SeqCst) && waiters(&run.w.facade) == 1
    });
    let b = route_b(&run, Hit::Base);
    assert!(b.recv_timeout(HANG).is_none(), "answered under the token");
    assert_eq!(waiters(&run.w.facade), 1, "the attempt left the gate");
    run.queued(&b, "route B");

    drop(token);
    outcome.recv_timeout(WAIT).expect("the attempt ends").expect("no fault");
    b.recv_timeout(WAIT).expect("route B is served").expect("answered");
    assert_eq!(started(&run.w.facade), 1);
}

/// Case 7 (PLAN F1's schedule): route B's members call for K waits before its inline write,
/// past its previous-cycle reads, and a cold route-A read of K then reads S0 from disk and
/// waits, holding the rows' section. Resumed, B waits on the section, and the store takes no
/// write while A owns it. A then inserts S0 and leaves, and B writes and inserts S1: S1 on disk
/// and cached, and the next read returns S1 (M17, M18b).
#[test]
fn case_7_a_cold_read_cannot_cache_its_row_over_a_write_it_overlapped() {
    let e = expect();
    let run = Run::cold(&e);
    let (b_pause, before_write) = pause();
    *run.w.facade.hooks.before_inline_write.lock() = Some(before_write);
    let b = route_b(&run, Hit::Base);
    b_pause.reached("route B reaches its inline write");
    let (a_pause, after_read) = pause();
    *run.w.rows.hooks.after_read.lock() = Some(after_read);
    let a = route_a(&run);
    a_pause.reached("route A reads K's row");
    let (rows, store) = (&run.w.rows, &run.w.store);
    let writes = store.writes.load(SeqCst);

    b_pause.resume();
    wait_until("route B waits on the section, or writes", || {
        rows.hooks.section_waiters.load(SeqCst) == 1 || store.writes.load(SeqCst) > writes
    });
    assert_eq!(store.writes.load(SeqCst), writes, "written while route A owns the section");
    assert!(b.try_recv().is_none(), "route B was answered");

    a_pause.resume();
    assert_eq!(row(a.join().expect("route A")), Some(e.s0.clone()), "route A's read");
    let seen = b.recv_timeout(WAIT).expect("route B is served").expect("answered");
    assert_eq!((seen.members, seen.row), (Ok(e.quorums[0].clone()), Ok(Some(e.s1.clone()))));
    assert_eq!(run.on_disk(), Some(e.s1.clone()), "on disk");
    assert_eq!(rows.peek(ROTATED, &k_hash()), Some(e.s1.clone()), "cached");
    assert_eq!(rows.snapshot_for_block(ROTATED, &k_hash()).expect("the read"), Some(e.s1));
}

/// Case 7's negative control: a getter that drops the section around its disk read lets B's
/// write and insert of S1 land between A's read and A's insert, so A caches S0 over S1 on disk.
#[test]
fn case_7_a_getter_unlocked_around_its_read_caches_s0_over_s1() {
    let e = expect();
    let run = Run::cold(&e);
    let (rows, store) = (&run.w.rows, &run.w.store);
    rows.hooks.unlocked_read.store(true, SeqCst);
    let (b_pause, before_write) = pause();
    *run.w.facade.hooks.before_inline_write.lock() = Some(before_write);
    let b = route_b(&run, Hit::Base);
    b_pause.reached("route B reaches its inline write");
    let (a_pause, after_read) = pause();
    *rows.hooks.after_read.lock() = Some(after_read);
    let a = route_a(&run);
    a_pause.reached("route A reads K's row");
    let writes = store.writes.load(SeqCst);

    b_pause.resume();
    b.recv_timeout(WAIT).expect("route B is served").expect("answered");
    assert_eq!(store.writes.load(SeqCst), writes + 1, "route B's write");
    assert_eq!(rows.peek(ROTATED, &k_hash()), Some(e.s1.clone()), "route B's insert");

    a_pause.resume();
    assert_eq!(row(a.join().expect("route A")), Some(e.s0.clone()), "route A's read");
    assert_eq!(run.on_disk(), Some(e.s1), "on disk");
    assert_eq!(rows.peek(ROTATED, &k_hash()), Some(e.s0), "cached");
}
