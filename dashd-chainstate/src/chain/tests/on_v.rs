//! Route B (EV addendum §2.6, CH7 §4.6): `ChainCommand::OnV` on a real V loop. The capability is
//! minted only there, between attempts, with the facade's gate held shared; it is refused after
//! the latch trips and without a facade (D74, D77); it is served FIFO with the lane and, as a
//! class M command, at ABC's release points (OD-CP-5); and a rotation through it writes its row
//! inline (§4.3).

use std::sync::atomic::Ordering::SeqCst;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::{Arc, Mutex, mpsc};

use dash_dml::StoreError;
use dash_evo::llmq::MemberError;
use dash_kv::ErrorKind;
use dash_types::Numeric;

use super::failure::pause;
use super::llmq_members::{Fixture, ROTATED, pros, rotate, world};
use super::support::{Rig, RigConfig, branch, regtest};
use crate::chain::actor::tests::Stage;
use crate::llmq::members::members;
use crate::store::Failure;

fn with_facade(w: &super::llmq_members::World) -> Rig {
    Rig::with(RigConfig {
        facade: Some(w.facade.clone()),
        ..RigConfig::default()
    })
}

/// An `OnV` command runs on V between attempts, with the gate held shared, in lane order with
/// the commands around it (M26).
#[test]
fn an_on_v_command_runs_between_attempts_in_lane_order() {
    let w = world(&regtest());
    let rig = with_facade(&w);
    let order = Arc::new(Mutex::new(Vec::new()));
    let resume = pause(&rig.v);
    let push = |n: u8| {
        let order = order.clone();
        move || order.lock().expect("order").push(n)
    };
    let (first, last) = (push(1), push(3));
    let first = rig.v.query(move |_| first());
    let (second, facade) = (push(2), w.facade.clone());
    let second = rig.v.on_v(move |_, _| {
        second();
        (facade.gate().is_locked(), facade.gate().is_locked_exclusive())
    });
    let last = rig.v.query(move |_| last());
    resume.send(()).expect("V paused");
    first.recv().expect("served");
    assert_eq!(second.recv().expect("served"), (true, false), "the gate, shared, and no attempt");
    last.recv().expect("served");
    assert_eq!(*order.lock().expect("order"), [1, 2, 3]);
    assert!(!w.facade.gate().is_locked(), "released with the command");
}

/// Once the latch has tripped, V mints no capability: the closure is dropped unrun, its reply
/// disconnects, and V serves on (D74, M4b).
#[test]
fn a_tripped_latch_refuses_the_command() {
    let w = world(&regtest());
    let rig = with_facade(&w);
    w.latch.trip(Failure {
        kind: ErrorKind::Other,
        site: "test",
        message: "tripped".to_owned(),
    });
    let ran = Arc::new(AtomicBool::new(false));
    let r = ran.clone();
    assert!(rig.v.on_v(move |_, _| r.store(true, SeqCst)).recv().is_err(), "disconnected");
    assert!(!ran.load(SeqCst), "the closure ran");
    rig.v.query(|_| ()).recv().expect("V serves on");
}

/// A V without a facade (a node without evo state) drops the closure unrun (D77).
#[test]
fn without_a_facade_the_command_is_dropped() {
    let rig = Rig::new();
    let ran = Arc::new(AtomicBool::new(false));
    let r = ran.clone();
    assert!(rig.v.on_v(move |_, _| r.store(true, SeqCst)).recv().is_err(), "disconnected");
    assert!(!ran.load(SeqCst), "the closure ran");
    rig.v.query(|_| ()).recv().expect("V serves on");
}

/// V never sends itself an `OnV` command (X15, M27).
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "V sent itself an OnV command")]
fn an_on_v_send_from_v_panics() {
    let rig = Rig::new();
    let (v, (send, got)) = (rig.v.clone(), mpsc::channel());
    rig.on_v(move |_| {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            drop(v.on_v(|_, _| ()));
        }));
        send.send(r).expect("the test waits");
        Ok(())
    });
    if let Err(panic) = got.recv().expect("V ran it") {
        std::panic::resume_unwind(panic);
    }
}

/// A route-B members miss on a rotated cycle writes its row to the runtime rows inline, while
/// the rotation runs, before any of its cache entries is visible; it logs no effect.
#[test]
fn a_route_b_rotation_writes_its_row_inline_before_its_cache_entries() {
    let fx = Fixture::rotated();
    let w = world(&fx.params);
    let at_write = Arc::new(Mutex::new(Vec::new()));
    {
        let (facade, at_write) = (Arc::downgrade(&w.facade), at_write.clone());
        *w.store.on_write.lock() = Some(Box::new(move || {
            let facade = facade.upgrade().expect("the facade lives");
            let caches = facade.test_caches();
            let len = |n: Option<usize>| n.unwrap_or(0);
            at_write.lock().expect("seen").push((
                len(caches.by_base.cache(ROTATED).map(|c| c.len())),
                len(caches.by_cycle_index.cache(ROTATED).map(|c| c.len())),
            ));
        }));
    }
    let rig = with_facade(&w);
    let (got, effects, (quorums, row)) = rig
        .v
        .on_v(|o, _| {
            let fx = Fixture::rotated();
            fx.ctx(|ctx| {
                let mut effects = Vec::new();
                let got = members(o, ctx, &fx.source(), &mut effects, ROTATED, fx.at(48), false);
                (got.map(|m| pros(&m)), effects, rotate(&fx, ctx, &fx.source(), 48))
            })
        })
        .recv()
        .expect("V ran it");
    assert_eq!(got, Ok(quorums[0].clone()));
    assert!(effects.is_empty(), "{effects:?}");
    assert_eq!(*at_write.lock().expect("seen"), [(0, 0)], "one write, before any entry");
    let hash = fx.hash(48);
    assert_eq!(w.store.rows.lock().get(&(ROTATED.to_base(), hash)), Some(&row), "on disk");
    assert_eq!(w.rows.peek(ROTATED, &hash), Some(row), "in the rows' cache");
    let caches = w.facade.test_caches();
    for (i, quorum) in quorums.iter().enumerate() {
        let i = u16::try_from(i).unwrap();
        let entry = caches.by_cycle_index.peek(ROTATED, &(hash, i)).map(|m| pros(m));
        assert_eq!(entry.as_ref(), Some(quorum), "index {i}");
    }
    assert!(w.latch.tripped().is_none());
}

/// A failed inline write trips the latch and answers no members: nothing is cached (D4).
#[test]
fn a_failed_inline_write_trips_the_latch_and_caches_nothing() {
    let fx = Fixture::rotated();
    let w = world(&fx.params);
    w.store.fail_writes.store(true, SeqCst);
    let rig = with_facade(&w);
    let got = rig
        .v
        .on_v(|o, _| {
            let fx = Fixture::rotated();
            fx.ctx(|ctx| {
                let got = members(o, ctx, &fx.source(), &mut Vec::new(), ROTATED, fx.at(48), false);
                got.map(|m| m.len())
            })
        })
        .recv()
        .expect("V ran it");
    assert!(matches!(got, Err(MemberError::View(StoreError::Read { .. }))), "{got:?}");
    let failure = w.latch.tripped().expect("tripped");
    assert!(failure.message.contains("a quorum snapshot write"), "{}", failure.message);
    let hash = fx.hash(48);
    assert_eq!(w.rows.peek(ROTATED, &hash), None);
    let caches = w.facade.test_caches();
    assert!(caches.by_base.cache(ROTATED).is_none_or(|c| c.is_empty()));
    assert!(caches.by_cycle_index.cache(ROTATED).is_none_or(|c| c.is_empty()));
}

/// Q7 (OD-CP-5): an `OnV` queued during an ABC run of two holds runs at the release point
/// between them, as a class M command, while ABC is still active: no attempt is open, it sees
/// the tip and the published scheme the first hold left, and ABC then connects the second
/// (M28). The command reaches the lane inside the first hold's release, after the release point
/// that opened the run.
#[test]
fn an_on_v_command_runs_at_the_release_point_between_holds() {
    let w = world(&regtest());
    let rig = with_facade(&w);
    let blocks = branch(&rig.p, &rig.genesis, 2, 0);
    let (released, at_release) = mpsc::channel();
    let (queued, wait_queued) = mpsc::channel::<()>();
    let first_release = Mutex::new(Some((released, wait_queued)));
    rig.on_v(move |vv| {
        vv.release_hook = Some(Arc::new(move |stage| {
            let Some((released, queued)) =
                first_release.lock().expect("once").take_if(|_| stage == Stage::Release)
            else {
                return;
            };
            released.send(()).expect("the test waits");
            queued.recv().expect("the test queues the command");
        }));
        Ok(())
    });
    let resume = pause(&rig.v);
    let first = rig.v.process_block(blocks[0].block.clone(), true, None, true);
    let second = rig.v.process_block(blocks[1].block.clone(), true, None, true);
    resume.send(()).expect("V paused");
    at_release.recv().expect("the first hold releases");
    let runs = Arc::new(AtomicUsize::new(0));
    let (facade, v, r) = (w.facade.clone(), rig.v.clone(), runs.clone());
    let seen = rig.v.on_v(move |_, q| {
        r.fetch_add(1, SeqCst);
        let view = v.view();
        (
            facade.gate().is_locked_exclusive(),
            q.evo_view().tip,
            view.snap.tip.tip,
            q.evo.bls == view.bls,
        )
    });
    queued.send(()).expect("V waits in the release");
    assert!(first.recv().expect("processed").accepted);
    assert!(second.recv().expect("processed").accepted);
    let (exclusive, tip, published, scheme) = seen.recv().expect("served");
    let [a1, a2] = rig.ids_n::<2>(&blocks);
    assert!(!exclusive, "an attempt was open");
    assert_eq!((tip, published), (Some(a1), a1), "the first hold's tip, before the second");
    assert!(scheme, "the published scheme");
    assert_eq!(rig.v.view().snap.tip.tip, a2, "ABC went on to the second hold");
    assert_eq!(runs.load(SeqCst), 1);
}
