//! The guarded establishment at a publication boundary (PLAN-r1 §2.11, P6, §4.6), the chain's
//! sync state (§2.4.4) and the subscribers' start.

use std::future::Future;
use std::sync::atomic::Ordering;
use std::time::Duration;

use dash_kv::faulty::{World, WorldConfig};
use tokio::task::JoinHandle;

use super::step::{bridged, mine, mine_bits, Forked, PEER, POW_LIMIT_BITS, SPACING};
use super::sub::{
    anchor, bound, next, open_rig, open_rig_with, reach, returned, spawn_next, At, Rig, Slot,
};
use super::*;
use crate::storage::store::crash::{open_store, Config};

/// A Candidate host (masternode sync on) on a memory store, its subscribers not started, and
/// its ingress.
async fn candidate() -> (Rig, Mem, HeaderIngress) {
    let (r, h, ingress, _) = with_replies().await;
    (r, h, ingress)
}

/// [`candidate`], with the ingress's replies.
async fn with_replies() -> (Rig, Mem, HeaderIngress, IngressReplies) {
    let dir = tempfile::tempdir().unwrap();
    let h = mem(&dir);
    let mut r = open_rig(h.clone(), dir, true, 1024, HEADER_WINDOW, |_| {}).await;
    let (ingress, replies) = r.host.take_ingress().unwrap();
    (r, h, ingress, replies)
}

/// A round asked at the host's published state.
fn round(r: &Rig, complete: bool, unknown_announced: bool) -> Round {
    Round {
        asked: r.host.status().borrow().asked(),
        complete,
        unknown_announced,
    }
}

/// An Established host with its subscribers started.
async fn started() -> (Rig, HeaderIngress) {
    let (r, _h, ingress) = candidate().await;
    assert!(matches!(r.host.handle().establish().await, GuardedEstablish::Established));
    r.host.start_subscribers().await.unwrap();
    (r, ingress)
}

async fn quiet(sub: &mut ChainSub) {
    let quiet = tokio::time::timeout(Duration::from_millis(200), sub.next()).await;
    assert!(quiet.is_err(), "{quiet:?}");
}

fn checks(sub: &ChainSub) -> usize {
    sub.hooks.checks.load(Ordering::SeqCst)
}

/// The host's state once `f` holds.
async fn when(r: &Rig, f: impl FnMut(&HeaderSyncState) -> bool) -> HeaderSyncState {
    let mut status = r.host.status();
    let s = tokio::time::timeout(Duration::from_secs(20), status.wait_for(f)).await;
    s.expect("within the test's bound").map(|g| *g).expect("the task runs")
}

/// The sync peer's reply `headers`, with `round`.
fn reply(ingress: &mut HeaderIngress, headers: &[Header], round: Round) {
    assert_eq!(ingress.reply(PEER, bridged(headers), round).unwrap(), Offered::Queued);
}

/// `headers`, a complete reply: the state that judges the chain covered, at their last.
async fn covered(r: &Rig, ingress: &mut HeaderIngress, headers: &[Header]) -> HeaderSyncState {
    reply(ingress, headers, round(r, true, false));
    let last = headers.last().expect("headers").hash();
    when(r, |s| s.caught_up && s.tip.hash == last).await
}

/// `cmd` on the host's handle, in a task of its own, once the command's send has completed and
/// before its reply.
async fn command<T, F>(r: &Rig, cmd: impl FnOnce(HeaderHandle) -> F) -> JoinHandle<T>
where
    T: Send + 'static,
    F: Future<Output = T> + Send + 'static,
{
    let handle = r.host.handle();
    let before = handle.sent();
    let task = tokio::spawn(cmd(handle));
    bound(async {
        while r.host.handle().sent() == before {
            tokio::task::yield_now().await;
        }
    })
    .await;
    task
}

fn guard(s: &HeaderSyncState) -> EstablishGuard {
    EstablishGuard {
        gen: s.gen,
        tip_seq: s.tip_seq,
    }
}

fn state(h: &Mem) -> AnchorState {
    meta(h).anchor_state
}

#[tokio::test]
async fn establish_guarded_established() {
    let (r, h, mut ingress) = candidate().await;
    let a = mine(anchor().hash, 1_000_000, 5, 1);
    let s = covered(&r, &mut ingress, &a).await;
    assert_eq!((s.gen, s.tip.hash, s.best_header.hash), (ChainGen(1), a[4].hash(), a[4].hash()));
    assert!(!s.behind && !s.established);
    let out = r.host.handle().establish_guarded(guard(&s)).await;
    assert!(matches!(out, GuardedEstablish::Established), "{out:?}");
    assert!(r.host.status().borrow().established, "the state says so before the reply");
    assert_eq!(state(&h), AnchorState::Established);
}

/// A step published after the state the caller judged: `Stale` with the newer state, nothing
/// written.
#[tokio::test]
async fn establish_guarded_stale_on_new_tip_seq() {
    let (r, h, mut ingress) = candidate().await;
    let a = mine(anchor().hash, 1_000_000, 6, 1);
    let s = covered(&r, &mut ingress, &a[..5]).await;
    let newer = r.send(&a[5..]).await.snap.tip_seq;
    let out = r.host.handle().establish_guarded(guard(&s)).await;
    let GuardedEstablish::Stale(now) = out else {
        panic!("{out:?}");
    };
    assert_eq!((now.tip_seq, now.caught_up, now.established), (newer, true, false));
    assert_eq!(state(&h), AnchorState::Candidate);
}

/// The chain was replaced since the state the caller judged: `Stale`, nothing written, though
/// the new generation is caught up at the same `tip_seq`.
#[tokio::test]
async fn establish_guarded_stale_on_new_gen() {
    let (r, h, mut ingress) = candidate().await;
    let old = covered(&r, &mut ingress, &mine(anchor().hash, 1_000_000, 5, 1)).await;
    let out = r.host.handle().replace(pack_anchor(&pack(10000, 1_000_000))).await;
    assert!(matches!(out, ReplaceOutcome::Replaced(ChainGen(2))), "{out:?}");
    let asked = round(&r, true, false);
    assert_eq!(asked.asked.gen, ChainGen(2));
    reply(&mut ingress, &[], asked);
    let now = when(&r, |s| s.gen == ChainGen(2) && s.caught_up).await;
    let stale = EstablishGuard {
        gen: old.gen,
        tip_seq: now.tip_seq,
    };
    let out = r.host.handle().establish_guarded(stale).await;
    assert!(matches!(out, GuardedEstablish::Stale(s) if s == now), "{out:?}");
    assert_eq!(state(&h), AnchorState::Candidate);
}

/// Not caught up, or behind: `Stale`, nothing written.
#[tokio::test]
async fn establish_guarded_stale_when_behind() {
    let (r, h, mut ingress) = candidate().await;
    let tip_seq = r.send(&mine(anchor().hash, 1_000_000, 5, 1)).await.snap.tip_seq;
    let at = EstablishGuard {
        gen: ChainGen(1),
        tip_seq,
    };
    let out = r.host.handle().establish_guarded(at).await;
    assert!(matches!(out, GuardedEstablish::Stale(s) if !s.caught_up), "{out:?}");
    for (complete, unknown) in [(false, false), (true, true)] {
        reply(&mut ingress, &[], round(&r, complete, unknown));
        let s = when(&r, |s| s.behind).await;
        let out = r.host.handle().establish_guarded(guard(&s)).await;
        assert!(matches!(out, GuardedEstablish::Stale(s) if s.behind), "{out:?}");
        reply(&mut ingress, &[], round(&r, true, false));
        when(&r, |s| s.caught_up).await;
    }
    assert_eq!(state(&h), AnchorState::Candidate);
}

/// A covered host whose Durable establish fails, its ingress kept: `Failed` with the store's
/// typed error, the fail-stop tripped, and the source `Stopped`.
async fn stopped_by_a_failed_establish() -> (Rig, HeaderIngress) {
    let world = World::new(WorldConfig::default());
    let h = Arc::new(open_store(&world, &Config::default()).unwrap_or_else(|_| panic!("opens")));
    let dir = tempfile::tempdir().unwrap();
    let mut r = open_rig(h.clone(), dir, true, 1024, HEADER_WINDOW, |_| {}).await;
    let (mut ingress, _) = r.host.take_ingress().unwrap();
    let s = covered(&r, &mut ingress, &mine(anchor().hash, 1_000_000, 5, 1)).await;
    let frozen = world.clone();
    h.interleave(Point::EstablishUnlocked, move |_| frozen.freeze());
    let out = r.host.handle().establish_guarded(guard(&s)).await;
    assert!(matches!(out, GuardedEstablish::Failed(SpvError::Storage(_))), "{out:?}");
    assert!(r.setup.fail.is_tripped());
    let mut source = r.host.source();
    let stopped = source.wait_for(|s| matches!(s.state, SourceState::Stopped));
    tokio::time::timeout(Duration::from_secs(20), stopped).await.unwrap().unwrap();
    (r, ingress)
}

/// The Durable establish fails: `Failed` with the store's typed error, the fail-stop tripped and
/// the task stopped.
#[tokio::test]
async fn establish_guarded_failed_carries_storage_error_and_trips_fail_stop() {
    let (r, _ingress) = stopped_by_a_failed_establish().await;
    assert!(!r.host.status().borrow().established);
}

/// S5d2 final pass (DEC-1982): the ingress does not keep a stopped task's state open. With the
/// ingress alive past `Stopped`, the state's watch closes within 20 s, and an unknown hash
/// observed after that publishes nothing.
#[tokio::test]
async fn a_stopped_task_closes_its_state_while_the_ingress_lives() {
    let (r, ingress) = stopped_by_a_failed_establish().await;
    let mut status = r.host.status();
    let last = *status.borrow_and_update();
    let closed = tokio::time::timeout(Duration::from_secs(20), status.changed()).await;
    assert!(closed.expect("the watch closes within 20 s").is_err());
    ingress.unknown_hash();
    assert!(status.has_changed().is_err(), "still closed");
    assert_eq!(*status.borrow(), last, "nothing published after the stop");
}

/// With masternode sync off, the establish checks nothing: a chain neither caught up nor
/// behind is Established.
#[tokio::test]
async fn establish_unguarded_mn_off() {
    let (r, h, _ingress) = candidate().await;
    let out = r.host.handle().establish().await;
    assert!(matches!(out, GuardedEstablish::Established), "{out:?}");
    let s = *r.host.status().borrow();
    assert!(s.established && !s.caught_up && !s.behind);
    assert_eq!(state(&h), AnchorState::Established);
}

/// Subscribers start only on an Established chain; before that a subscriber returns nothing.
#[tokio::test]
async fn start_subscribers_refuses_unless_established() {
    let (r, _h, _ingress) = candidate().await;
    let mut sub = r.host.subscribe(SubKind::Filters);
    assert!(r.host.start_subscribers().await.is_err());
    let early = tokio::time::timeout(Duration::from_millis(200), sub.next()).await;
    assert!(early.is_err(), "{early:?}");
    assert!(r.host.subs.live(ChainGen(1)).is_empty());
    assert!(matches!(r.host.handle().establish().await, GuardedEstablish::Established));
    r.host.start_subscribers().await.unwrap();
    let n = next(&mut sub).await;
    assert!(
        matches!(n, ChainNotice::Resync { why: ResyncWhy::Start, sync, .. } if sync.established),
        "{n:?}"
    );
}

/// PLAN-r1 §4.6: a subscriber whose feed was closed across the caught-up transition gets its
/// `Resync` with the state that says so.
#[tokio::test]
async fn resync_carries_caught_up() {
    let (r, _h, mut ingress) = candidate().await;
    assert!(matches!(r.host.handle().establish().await, GuardedEstablish::Established));
    r.host.start_subscribers().await.unwrap();
    let mut sub = r.host.subscribe(SubKind::Masternodes);
    let n = next(&mut sub).await;
    assert!(matches!(n, ChainNotice::Resync { sync, .. } if !sync.caught_up), "{n:?}");
    for e in r.host.subs.live(ChainGen(1)) {
        e.close();
    }
    let s = covered(&r, &mut ingress, &mine(anchor().hash, 1_000_000, 5, 1)).await;
    let n = next(&mut sub).await;
    let ChainNotice::Resync {
        pin,
        sync,
        why: ResyncWhy::Lag,
    } = &n
    else {
        panic!("{n:?}");
    };
    assert_eq!((*sync, pin.snap.tip_seq), (s, s.tip_seq));
}

/// PLAN-r1 §4.6: a short reply with an unknown announced hash is not caught up; once that
/// block's batch is stepped and the round complete, the chain is caught up at its tip; a later
/// full reply is behind.
#[tokio::test]
async fn caught_up_means_peers_best_header() {
    let (r, _h, mut ingress) = candidate().await;
    let a = mine(anchor().hash, 1_000_000, 6, 1);
    reply(&mut ingress, &a[..5], round(&r, true, true));
    let s = when(&r, |s| s.behind).await;
    assert!(!s.caught_up);
    assert_eq!(s.tip.hash, a[4].hash());
    reply(&mut ingress, &a[5..], round(&r, true, false));
    let s = when(&r, |s| s.caught_up).await;
    assert_eq!((s.behind, s.tip.hash), (false, a[5].hash()));
    reply(&mut ingress, &[], round(&r, false, false));
    let s = when(&r, |s| s.behind).await;
    assert!(!s.caught_up);
}

/// A state change reaches a subscriber as `Sync`, after the notice of the tip it names; a step
/// alone changes no fact and brings no `Sync`.
#[tokio::test]
async fn sync_follows_the_tip_it_names() {
    let (r, _h, mut ingress) = candidate().await;
    assert!(matches!(r.host.handle().establish().await, GuardedEstablish::Established));
    r.host.start_subscribers().await.unwrap();
    let mut sub = r.host.subscribe(SubKind::Blocks);
    next(&mut sub).await;
    let a = mine(anchor().hash, 1_000_000, 6, 1);
    r.send(&a[..5]).await;
    assert!(matches!(next(&mut sub).await, ChainNotice::Advanced { .. }));
    let quiet = tokio::time::timeout(Duration::from_millis(200), sub.next()).await;
    assert!(quiet.is_err(), "a step alone: {quiet:?}");
    let s = covered(&r, &mut ingress, &a[5..]).await;
    let n = next(&mut sub).await;
    assert!(matches!(&n, ChainNotice::Advanced { to_seq, .. } if *to_seq == s.tip_seq), "{n:?}");
    let item = tokio::time::timeout(Duration::from_secs(20), sub.next()).await.unwrap();
    assert!(matches!(item, Ok(SubItem::Sync(got)) if got == s), "{item:?}");
    let quiet = tokio::time::timeout(Duration::from_millis(200), sub.next()).await;
    assert!(quiet.is_err(), "{quiet:?}");
}

/// S5d r1 major 3: a round binds to the generation that took its batch. A replacement waits
/// while the sync peer's complete reply runs on generation 1; generation 2 starts neither caught
/// up nor behind, and a guard on it is `Stale`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_round_never_crosses_a_replacement() {
    let (r, h, mut ingress) = candidate().await;
    let held = r.host.subs.task.hold_batch();
    reply(&mut ingress, &mine(anchor().hash, 1_000_000, 5, 1), round(&r, true, false));
    bound(held.reached.notified()).await;
    let replace =
        command(&r, |h| async move { h.replace(pack_anchor(&pack(10000, 1_000_000))).await }).await;
    held.release.notify_one();
    let out = bound(replace).await.unwrap();
    assert!(matches!(out, ReplaceOutcome::Replaced(ChainGen(2))), "{out:?}");
    let s = when(&r, |s| s.gen == ChainGen(2)).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let now = *r.host.status().borrow();
    assert_eq!(now, s);
    assert!(!now.caught_up && !now.behind, "{now:?}");
    let out = r.host.handle().establish_guarded(guard(&now)).await;
    assert!(matches!(out, GuardedEstablish::Stale(_)), "{out:?}");
    assert_eq!(state(&h), AnchorState::Candidate);
}

/// S5d r1 major 5: a reply and its round reach the state before any command. Caught up at T
/// with a guard on it, the sync peer sends a full reply of headers the chain holds, and an
/// Establish waits while it runs: the reply's round is applied first, so the guard is `Stale`,
/// behind at the same tip, and nothing is written.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_reply_reaches_the_guard_before_establish() {
    let (r, h, mut ingress) = candidate().await;
    let a = mine(anchor().hash, 1_000_000, 5, 1);
    let s = covered(&r, &mut ingress, &a).await;
    let held = r.host.subs.task.hold_batch();
    reply(&mut ingress, &a, round(&r, false, false));
    bound(held.reached.notified()).await;
    let establish = command(&r, |h| async move { h.establish_guarded(guard(&s)).await }).await;
    held.release.notify_one();
    let out = bound(establish).await.unwrap();
    let GuardedEstablish::Stale(now) = out else {
        panic!("{out:?}");
    };
    assert!(now.behind && !now.caught_up, "{now:?}");
    assert_eq!(now.tip_seq, s.tip_seq);
    assert_eq!(state(&h), AnchorState::Candidate);
}

/// S5d r1 major 4, r2 major 4: a `Resync`'s state is the one published at its pin's tip. The
/// sub hands off at T and is held before it reads the state while a small reply moves the chain
/// to U and its round makes it caught up: the state past the pin ends that attempt, and the
/// retry hands off at U with U's state. No `Sync` follows.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_resync_never_names_a_later_state() {
    let (r, mut ingress) = started().await;
    let t = r.host.status().borrow().tip_seq;
    let sub = r.host.subscribe(SubKind::Blocks);
    let held = sub.hooks.hold(At::BeforeRecheck);
    let mut task = spawn_next(sub);
    reach(&held.reached, &mut task).await;
    let a = mine(anchor().hash, 1_000_000, 2, 1);
    let u = covered(&r, &mut ingress, &a).await;
    assert!(u.tip_seq > t);
    held.release.notify_one();
    let (mut sub, out) = returned(task).await;
    let Ok(SubItem::Notice(ChainNotice::Resync {
        pin,
        sync,
        ..
    })) = out
    else {
        panic!("{out:?}");
    };
    assert_eq!((pin.snap.tip_seq, sync), (u.tip_seq, u));
    assert_eq!(checks(&sub), 2, "the read past T, then U's");
    quiet(&mut sub).await;
}

/// S5d r2 major 4 (MGR), S5d2 r1 major 4 and minor 2: a state behind the pin is waited for,
/// never projected onto it, with the slot released. Caught up at T, the sync peer's full reply
/// moves the chain to U, and its round is held: U is published while the state still names T. A
/// sub attaching then hands off at U, reads the state once, releases its slot and waits, as its
/// hooks acknowledge, the wait polled pending, with no read until the round's update, the one
/// publication; its retry's
/// `Resync` at U is behind. The live sub gets U's notice and then `Sync` behind: no caught-up
/// names U.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_resync_waits_for_its_tips_round() {
    let (r, mut ingress) = started().await;
    let mut live = r.host.subscribe(SubKind::Blocks);
    next(&mut live).await;
    let a = mine(anchor().hash, 1_000_000, 4, 1);
    let t = covered(&r, &mut ingress, &a[..2]).await;
    next(&mut live).await;
    assert!(matches!(bound(live.next()).await, Ok(SubItem::Sync(s)) if s == t));
    let held = r.host.subs.task.hold_settle();
    reply(&mut ingress, &a[2..], round(&r, false, false));
    bound(held.reached.notified()).await;
    assert_eq!(*r.host.status().borrow(), t);
    let n = next(&mut live).await;
    let ChainNotice::Advanced {
        to_seq: u,
        ..
    } = n
    else {
        panic!("{n:?}");
    };
    let sub = r.host.subscribe(SubKind::Filters);
    let hooks = Arc::clone(&sub.hooks);
    let trailing = hooks.pass(At::Trailing);
    let mut task = spawn_next(sub);
    reach(&trailing, &mut task).await;
    let [Slot::Reg(a_id), Slot::Dereg(b_id)] = hooks.log.lock()[..] else {
        panic!("{:?}", hooks.log.lock());
    };
    assert_eq!(a_id, b_id, "the slot is released before the wait");
    bound(hooks.waits.notified()).await;
    assert!(!task.is_finished());
    assert_eq!(hooks.checks.load(Ordering::SeqCst), 1, "a wait, not a retry");
    quiet(&mut live).await;
    held.release.notify_one();
    let (sub, out) = returned(task).await;
    let Ok(SubItem::Notice(ChainNotice::Resync {
        pin,
        sync,
        why: ResyncWhy::Start,
    })) = out
    else {
        panic!("{out:?}");
    };
    assert_eq!((pin.snap.tip_seq, sync.tip_seq, pin.snap.tip.tip_hash), (u, u, a[3].hash()));
    assert!(sync.behind && !sync.caught_up, "{sync:?}");
    assert_eq!(checks(&sub), 2, "one read per publication");
    let item = bound(live.next()).await;
    assert!(matches!(item, Ok(SubItem::Sync(s)) if s == sync), "{item:?}");
}

/// S5d r2 major 4 (MGR): a sub that saw caught up at T and reattaches while an announcement
/// moves the chain to U, with no round, gets its `Resync` at U, caught up as before, and no
/// `Sync` after it: no caught-up transition happened. Its first attempt, at T, ends on the
/// state at U, the one publication, and the retry hands off there.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reattach_across_an_announcement_repeats_no_transition() {
    let (r, mut ingress) = started().await;
    let mut sub = r.host.subscribe(SubKind::Blocks);
    next(&mut sub).await;
    let a = mine(anchor().hash, 1_000_000, 3, 1);
    let t = covered(&r, &mut ingress, &a[..2]).await;
    next(&mut sub).await;
    assert!(matches!(bound(sub.next()).await, Ok(SubItem::Sync(s)) if s == t));
    for e in r.host.subs.live(ChainGen(1)) {
        e.close();
    }
    let held = sub.hooks.hold(At::BeforeRecheck);
    let before = checks(&sub);
    let mut task = spawn_next(sub);
    reach(&held.reached, &mut task).await;
    let u = r.send(&a[2..]).await.snap.tip_seq;
    let s = when(&r, |s| s.tip_seq == u).await;
    assert!(s.caught_up && !s.behind, "{s:?}");
    held.release.notify_one();
    let (mut sub, out) = returned(task).await;
    let Ok(SubItem::Notice(ChainNotice::Resync {
        pin,
        sync,
        why: ResyncWhy::Lag,
    })) = out
    else {
        panic!("{out:?}");
    };
    assert_eq!((pin.snap.tip_seq, sync), (u, s));
    assert_eq!(checks(&sub) - before, 2, "the read past T, then U's");
    quiet(&mut sub).await;
}

/// S5d r2 major 2: a round binds to the generation it asked. The sync peer's empty short reply
/// to generation 1's locator is queued behind a held batch while a Replace waits: the Replace
/// goes first, and generation 2 takes the reply without its round, so it is neither caught up
/// nor behind, and a guard on it is `Stale`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_round_of_a_replaced_generation_is_dropped() {
    let (r, h, mut ingress, mut replies) = with_replies().await;
    let held = r.host.subs.task.hold_batch();
    assert_eq!(ingress.offer(PEER, bridged(&[])).unwrap(), Offered::Queued);
    bound(held.reached.notified()).await;
    reply(&mut ingress, &[], round(&r, true, false));
    let replace =
        command(&r, |h| async move { h.replace(pack_anchor(&pack(10000, 1_000_000))).await }).await;
    held.release.notify_one();
    let out = bound(replace).await.unwrap();
    assert!(matches!(out, ReplaceOutcome::Replaced(ChainGen(2))), "{out:?}");
    let gens: Vec<_> = [bound(replies.recv()).await, bound(replies.recv()).await]
        .map(|got| got.expect("a reply").reply.expect("taken").gen)
        .to_vec();
    assert_eq!(gens, [ChainGen(1), ChainGen(2)]);
    let now = *r.host.status().borrow();
    assert_eq!(now.gen, ChainGen(2));
    assert!(!now.caught_up && !now.behind, "{now:?}");
    let out = r.host.handle().establish_guarded(guard(&now)).await;
    assert!(matches!(out, GuardedEstablish::Stale(_)), "{out:?}");
    assert_eq!(state(&h), AnchorState::Candidate);
}

/// S5d r2 major 3: a round's `complete` stands only for a batch taken whole. Caught up at T,
/// the sync peer's short reply is refused, its parent unknown, or stops partway, at a header
/// with the wrong bits: either way the chain is behind, not caught up, and a guard on it is
/// `Stale`.
#[tokio::test]
async fn a_short_reply_not_taken_whole_is_not_caught_up() {
    for stops in [false, true] {
        let (r, h, mut ingress, mut replies) = with_replies().await;
        let a = mine(anchor().hash, 1_000_000, 2, 1);
        let t = covered(&r, &mut ingress, &a).await;
        let time = 1_000_000 + 2 * SPACING;
        let bad = match stops {
            false => mine(BlockHash::from([7; 32]), time, 2, 2),
            true => {
                let first = mine(a[1].hash(), time, 1, 2)[0];
                let wrong = mine_bits(first.hash(), time + SPACING, 1, 2, POW_LIMIT_BITS - 1)[0];
                vec![first, wrong, mine(wrong.hash(), time + 2 * SPACING, 1, 2)[0]]
            }
        };
        reply(&mut ingress, &bad, round(&r, true, false));
        bound(replies.recv()).await.expect("covered's reply");
        let got = bound(replies.recv()).await.expect("a reply").reply.expect("taken");
        assert_eq!((got.refused.is_some(), got.stop.is_some()), (!stops, stops), "{got:?}");
        let now = *r.host.status().borrow();
        assert_eq!(now.tip_seq, got.snap.tip_seq);
        assert!(now.behind && !now.caught_up, "{now:?}");
        let observed = t.observed + u64::from(!stops);
        let outstanding = (!stops).then_some(observed);
        assert_eq!(
            (now.observed, now.outstanding),
            (observed, outstanding),
            "S5d3: refused, observed"
        );
        let out = r.host.handle().establish_guarded(guard(&now)).await;
        assert!(matches!(out, GuardedEstablish::Stale(_)), "{out:?}");
        assert_eq!(state(&h), AnchorState::Candidate);
    }
}

/// The ingress of `f`'s host, with its replies.
fn forked_ingress(f: &mut Forked) -> (HeaderIngress, IngressReplies) {
    f.host.take_ingress().expect("the ingress")
}

/// The sync peer's reply `headers`, with `round`; the task's answer.
async fn answered(
    ingress: &mut HeaderIngress,
    replies: &mut IngressReplies,
    headers: &[Header],
    round: Round,
) -> HeadersReply {
    assert_eq!(ingress.reply(PEER, bridged(headers), round).unwrap(), Offered::Queued);
    bound(replies.recv()).await.expect("a reply").reply.expect("taken")
}

/// S5d2 r1 major 1 (DEC-1955): a round names the tip its locator was built from. Neither caught
/// up nor behind at A6, the sync peer is asked at A6; before its empty short reply is taken, a
/// ChainLock on B4, or a heavier branch on B4, moves the tip in the same generation. The round is
/// dropped: B is not caught up, and a guard on it is `Stale`. A later reply to A6 that extends
/// A past B still has its headers stepped.
#[tokio::test]
async fn a_reply_to_a_former_tip_is_not_its_round() {
    for by_lock in [true, false] {
        let mut f = Forked::new(None).await;
        let (mut ingress, mut replies) = forked_ingress(&mut f);
        let a6 = *f.host.status().borrow();
        assert_eq!(a6.tip.hash, f.a[5].hash());
        assert!(!a6.caught_up && !a6.behind, "{a6:?}");
        let asked = Round {
            asked: a6.asked(),
            complete: true,
            unknown_announced: false,
        };
        let b = match by_lock {
            true => {
                f.host.handle().apply_chainlock(f.lock_on_b()).await.unwrap();
                f.b[1].hash()
            }
            false => {
                let more = mine(f.b[1].hash(), 1_000_000 + 4 * SPACING, 3, 2);
                f.host.handle().headers(PEER, bridged(&more)).await.unwrap();
                more[2].hash()
            }
        };
        let moved = *f.host.status().borrow();
        assert_eq!((moved.gen, moved.tip.hash), (ChainGen(1), b));
        let got = answered(&mut ingress, &mut replies, &[], asked).await;
        assert_eq!(got.snap.tip.tip_hash, b);
        let now = *f.host.status().borrow();
        assert!(!now.caught_up, "{now:?}");
        let out = f.host.handle().establish_guarded(guard(&now)).await;
        assert!(matches!(out, GuardedEstablish::Stale(s) if !s.caught_up), "{out:?}");
        if !by_lock {
            let past = mine(f.a[5].hash(), 1_000_000 + 6 * SPACING, 3, 1);
            let got = answered(&mut ingress, &mut replies, &past, asked).await;
            assert_eq!(got.snap.tip.tip_hash, past[2].hash(), "the batch is stepped");
            let now = *f.host.status().borrow();
            assert!(!now.caught_up && !now.behind, "{now:?}");
        }
    }
}

/// S5d2 r1 major 2 (DEC-1955): a reorg drops coverage. A short round completes at A6, then a
/// ChainLock selects B4: the state at B4 is neither caught up nor behind, a guard on it is
/// `Stale`, and a new subscriber's `Resync` at B4 does not carry caught up.
#[tokio::test]
async fn a_reorg_off_the_covered_tip_drops_coverage() {
    let mut f = Forked::new(None).await;
    let (mut ingress, mut replies) = forked_ingress(&mut f);
    let asked = Round {
        asked: f.host.status().borrow().asked(),
        complete: true,
        unknown_announced: false,
    };
    answered(&mut ingress, &mut replies, &[], asked).await;
    let a6 = *f.host.status().borrow();
    assert!(a6.caught_up && a6.tip.hash == f.a[5].hash(), "{a6:?}");
    f.host.handle().apply_chainlock(f.lock_on_b()).await.unwrap();
    let b4 = *f.host.status().borrow();
    assert_eq!(b4.tip.hash, f.b[1].hash());
    assert!(!b4.caught_up && !b4.behind, "{b4:?}");
    let out = f.host.handle().establish_guarded(guard(&b4)).await;
    assert!(matches!(out, GuardedEstablish::Stale(s) if s == b4), "{out:?}");
    f.host.start_subscribers().await.unwrap();
    let mut sub = f.host.subscribe(SubKind::Blocks);
    let n = next(&mut sub).await;
    let ChainNotice::Resync {
        pin,
        sync,
        ..
    } = &n
    else {
        panic!("{n:?}");
    };
    assert_eq!((pin.snap.tip.tip_hash, *sync), (f.b[1].hash(), b4));
}

/// S5d2 r1 major 3 (DEC-1955): a batch without a round that is refused clears `caught_up`.
/// Caught up at T, a peer announces a short batch whose parent is unknown, and an Establish
/// with T's guard waits while it runs: the batch is refused, and the guard is `Stale`, not caught
/// up, with nothing written.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unconnecting_announcement_reaches_the_guard_before_establish() {
    let (r, h, mut ingress, mut replies) = with_replies().await;
    let s = covered(&r, &mut ingress, &mine(anchor().hash, 1_000_000, 2, 1)).await;
    bound(replies.recv()).await.expect("covered's reply");
    let held = r.host.subs.task.hold_batch();
    let unconnecting = mine(BlockHash::from([7; 32]), 1_000_000 + 2 * SPACING, 2, 2);
    assert_eq!(ingress.offer(PEER, bridged(&unconnecting)).unwrap(), Offered::Queued);
    bound(held.reached.notified()).await;
    let establish = command(&r, |h| async move { h.establish_guarded(guard(&s)).await }).await;
    held.release.notify_one();
    let got = bound(replies.recv()).await.expect("a reply").reply.expect("taken");
    assert!(got.refused.is_some(), "{got:?}");
    let out = bound(establish).await.unwrap();
    let GuardedEstablish::Stale(now) = out else {
        panic!("{out:?}");
    };
    let observed = s.observed + 1;
    assert_eq!(
        (now.caught_up, now.behind, now.observed, now.outstanding, now.tip_seq),
        (false, true, observed, Some(observed), s.tip_seq)
    );
    assert_eq!(state(&h), AnchorState::Candidate);
}

/// S5d2 r1 major 3 (DEC-1955): an unknown-hash observation is never dropped. Caught up at T,
/// the sync peer is paused behind a full ingress, its next batch dropped; the observation made
/// then still reaches the task before the Establish waiting with T's guard, which is `Stale`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unknown_hash_while_paused_blocks_establish() {
    let (r, h, mut ingress) = candidate().await;
    let s = covered(&r, &mut ingress, &mine(anchor().hash, 1_000_000, 2, 1)).await;
    let held = r.host.subs.task.hold_batch();
    assert_eq!(ingress.offer(PEER, bridged(&[])).unwrap(), Offered::Queued);
    bound(held.reached.notified()).await;
    for _ in 0..INGRESS_CAP {
        assert_eq!(ingress.offer(PEER, bridged(&[])).unwrap(), Offered::Queued);
    }
    assert_eq!(ingress.offer(PEER, bridged(&[])).unwrap(), Offered::Paused);
    assert_eq!(ingress.offer(PEER, bridged(&[])).unwrap(), Offered::Dropped);
    let establish = command(&r, |h| async move { h.establish_guarded(guard(&s)).await }).await;
    ingress.unknown_hash();
    held.release.notify_one();
    let out = bound(establish).await.unwrap();
    let GuardedEstablish::Stale(now) = out else {
        panic!("{out:?}");
    };
    assert_eq!(
        (now.caught_up, now.behind, now.observed, now.tip_seq),
        (false, true, s.observed + 1, s.tip_seq)
    );
    assert_eq!(state(&h), AnchorState::Candidate);
}

/// S5d2 r1 major 3 (DEC-1955): Establish reads an observation published before it. Caught up at
/// T and idle, an unknown hash is observed: the guard on T is `Stale`, behind.
#[tokio::test]
async fn establish_takes_an_unknown_hash_first() {
    let (r, h, mut ingress) = candidate().await;
    let s = covered(&r, &mut ingress, &mine(anchor().hash, 1_000_000, 2, 1)).await;
    ingress.unknown_hash();
    let out = r.host.handle().establish_guarded(guard(&s)).await;
    let GuardedEstablish::Stale(now) = out else {
        panic!("{out:?}");
    };
    assert_eq!((now.caught_up, now.behind, now.observed), (false, true, s.observed + 1));
    assert_eq!(state(&h), AnchorState::Candidate);
}

/// S5d2 r1 major 3 (DEC-1955, amended): only a round asked after an observation ends it. Caught
/// up at T, an unknown hash is observed, and a batch runs. A complete reply to a round
/// asked before that, at the old `observed`, is behind; one asked after is caught up, and a
/// guard on it establishes.
#[tokio::test]
async fn only_a_round_asked_after_an_observation_ends_it() {
    let (r, h, mut ingress, mut replies) = with_replies().await;
    let s = covered(&r, &mut ingress, &mine(anchor().hash, 1_000_000, 2, 1)).await;
    bound(replies.recv()).await.expect("covered's reply");
    let old = round(&r, true, false);
    ingress.unknown_hash();
    assert_eq!(ingress.offer(PEER, bridged(&[])).unwrap(), Offered::Queued);
    bound(replies.recv()).await.expect("a reply");
    let taken = *r.host.status().borrow();
    assert_eq!((taken.caught_up, taken.observed), (false, s.observed + 1));
    answered(&mut ingress, &mut replies, &[], old).await;
    let now = *r.host.status().borrow();
    assert!(now.behind && !now.caught_up, "{now:?}");
    assert_eq!(now.outstanding, Some(s.observed + 1));
    answered(&mut ingress, &mut replies, &[], round(&r, true, false)).await;
    let now = *r.host.status().borrow();
    assert!(now.caught_up && !now.behind, "{now:?}");
    assert_eq!(now.outstanding, None, "S5d3: the recovering round ends it");
    let out = r.host.handle().establish_guarded(guard(&now)).await;
    assert!(matches!(out, GuardedEstablish::Established), "{out:?}");
    assert_eq!(state(&h), AnchorState::Established);
}

/// S5d2 r1 major 4 (DEC-1955): a handoff that finds the state behind its pin releases its slot
/// before it waits. One header a step, one event a slot and a segment, one subscriber: the sync
/// peer's three-header reply runs with its round, and the sub hands off just after the first
/// step, while the state still names the anchor. Its slot released, the second and third steps
/// run without waiting on it, well inside the time budget, with no close; the sub's retry gets
/// the round's state, and it goes on live.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_handoff_inside_a_round_never_holds_up_its_steps() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = open_rig_with(mem(&dir), dir, false, 1, HEADER_WINDOW, |s| {
        s.feeds.step_headers = 1;
    })
    .await;
    r.host.subs.start();
    let (mut ingress, mut replies) = r.host.take_ingress().unwrap();
    let a = mine(anchor().hash, 1_000_000, 4, 1);
    let step = r.host.subs.task.hold_step();
    reply(&mut ingress, &a[..3], round(&r, true, false));
    bound(step.reached.notified()).await;
    let sub = r.host.subscribe(SubKind::Blocks);
    let trailing = sub.hooks.pass(At::Trailing);
    let mut task = spawn_next(sub);
    reach(&trailing, &mut task).await;
    step.release.notify_one();
    let got = bound(replies.recv()).await.expect("a reply").reply.expect("taken");
    assert_eq!(got.snap.tip.tip_hash, a[2].hash());
    let (mut sub, out) = returned(task).await;
    let Ok(SubItem::Notice(ChainNotice::Resync {
        pin,
        sync,
        ..
    })) = out
    else {
        panic!("{out:?}");
    };
    assert_eq!((pin.snap.tip_seq, sync.tip_seq), (got.snap.tip_seq, got.snap.tip_seq));
    assert!(sync.caught_up, "{sync:?}");
    let most = r.host.subs.lag.most();
    assert!(most < r.setup.lag.time / 5, "parked {most:?}");
    let tip = r.send(&a[3..]).await.snap.tip_seq;
    let n = next(&mut sub).await;
    assert!(matches!(n, ChainNotice::Advanced { to_seq, .. } if to_seq == tip), "{n:?}");
    assert!(r.host.subs.lag.closes().is_empty(), "{:?}", r.host.subs.lag.closes());
}

/// S5d2 r2 major 1 (DEC-1972): an Unconnecting refusal is an unknown-hash observation. Caught up
/// at T, the sync peer's empty short reply asked at T is queued behind a held batch; a peer's
/// short batch whose parent is unknown, a command, goes first and is refused. The old reply then
/// leaves T behind, not caught up, and a guard on T is `Stale`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unconnecting_refusal_outlives_an_older_round() {
    let (r, h, mut ingress, mut replies) = with_replies().await;
    let t = covered(&r, &mut ingress, &mine(anchor().hash, 1_000_000, 2, 1)).await;
    bound(replies.recv()).await.expect("covered's reply");
    let old = round(&r, true, false);
    let held = r.host.subs.task.hold_batch();
    assert_eq!(ingress.offer(PEER, bridged(&[])).unwrap(), Offered::Queued);
    bound(held.reached.notified()).await;
    reply(&mut ingress, &[], old);
    let unconnecting = mine(BlockHash::from([7; 32]), 1_000_000 + 2 * SPACING, 2, 2);
    let refused =
        command(&r, |h| async move { h.headers(PEER, bridged(&unconnecting)).await }).await;
    held.release.notify_one();
    let got = bound(refused).await.unwrap().expect("taken");
    assert!(matches!(got.refused, Some(Refused::Unconnecting { .. })), "{got:?}");
    for _ in 0..2 {
        bound(replies.recv()).await.expect("a reply").reply.expect("taken");
    }
    let now = *r.host.status().borrow();
    let observed = t.observed + 1;
    assert_eq!(
        (now.tip_seq, now.caught_up, now.behind, now.observed, now.outstanding),
        (t.tip_seq, false, true, observed, Some(observed))
    );
    let out = r.host.handle().establish_guarded(guard(&t)).await;
    assert!(matches!(out, GuardedEstablish::Stale(_)), "{out:?}");
    assert_eq!(state(&h), AnchorState::Candidate);
}

/// S5d2 r2 major 2 (DEC-1972): `unknown_hash` publishes its observation before it returns.
/// Established, caught up at T and idle, an unknown hash is observed: with no batch or command
/// after it, a new sub's `Resync` at T carries behind, not caught up.
#[tokio::test]
async fn an_idle_unknown_hash_reaches_a_new_sub() {
    let (r, mut ingress) = started().await;
    let t = covered(&r, &mut ingress, &mine(anchor().hash, 1_000_000, 2, 1)).await;
    ingress.unknown_hash();
    let mut sub = r.host.subscribe(SubKind::Blocks);
    let n = next(&mut sub).await;
    let ChainNotice::Resync {
        pin,
        sync,
        ..
    } = &n
    else {
        panic!("{n:?}");
    };
    assert_eq!(pin.snap.tip_seq, t.tip_seq);
    let observed = t.observed + 1;
    assert_eq!(
        (sync.caught_up, sync.behind, sync.observed, sync.outstanding),
        (false, true, observed, Some(observed))
    );
}

/// S5d2 r2 major 2 (DEC-1972): an observation between the steps of a batch without a round holds
/// for its later steps. One header a step, caught up at T, a peer's three-header announcement
/// runs; an unknown hash is observed after its first step, and its second step publishes
/// behind, not caught up, as does its last.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unknown_hash_between_steps_holds_for_the_later_steps() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = open_rig_with(mem(&dir), dir, true, 1024, HEADER_WINDOW, |s| {
        s.feeds.step_headers = 1;
    })
    .await;
    let (mut ingress, mut replies) = r.host.take_ingress().unwrap();
    let a = mine(anchor().hash, 1_000_000, 5, 1);
    let t = covered(&r, &mut ingress, &a[..2]).await;
    bound(replies.recv()).await.expect("covered's reply");
    let first = r.host.subs.task.hold_step();
    assert_eq!(ingress.offer(PEER, bridged(&a[2..])).unwrap(), Offered::Queued);
    bound(first.reached.notified()).await;
    let one = *r.host.status().borrow();
    assert!(one.caught_up && one.tip.hash == a[2].hash(), "{one:?}");
    ingress.unknown_hash();
    let second = r.host.subs.task.hold_step();
    first.release.notify_one();
    bound(second.reached.notified()).await;
    let two = *r.host.status().borrow();
    assert_eq!((two.tip.hash, two.caught_up, two.behind), (a[3].hash(), false, true), "{two:?}");
    second.release.notify_one();
    let got = bound(replies.recv()).await.expect("a reply").reply.expect("taken");
    let now = *r.host.status().borrow();
    assert_eq!(
        (now.tip_seq, now.caught_up, now.behind, now.observed),
        (got.snap.tip_seq, false, true, t.observed + 1)
    );
}

/// S5d2 r2 major 2 (DEC-1972): a round's update reads the outstanding observation as it
/// publishes. Caught up at T, the sync peer's complete reply asked at T is held as it settles,
/// after its step; an unknown hash observed then leaves its tip behind, not caught up, and a
/// guard on it is `Stale`. The task reads the observation in the closure of the publication
/// itself, so no hook fits between that read and the publish.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unknown_hash_as_a_round_settles_holds() {
    let (r, h, mut ingress, mut replies) = with_replies().await;
    let a = mine(anchor().hash, 1_000_000, 3, 1);
    let t = covered(&r, &mut ingress, &a[..2]).await;
    bound(replies.recv()).await.expect("covered's reply");
    let held = r.host.subs.task.hold_settle();
    reply(&mut ingress, &a[2..], round(&r, true, false));
    bound(held.reached.notified()).await;
    ingress.unknown_hash();
    held.release.notify_one();
    bound(replies.recv()).await.expect("a reply").reply.expect("taken");
    let now = *r.host.status().borrow();
    assert_eq!(
        (now.tip.hash, now.caught_up, now.behind, now.outstanding),
        (a[2].hash(), false, true, Some(t.observed + 1))
    );
    let out = r.host.handle().establish_guarded(guard(&now)).await;
    assert!(matches!(out, GuardedEstablish::Stale(_)), "{out:?}");
    assert_eq!(state(&h), AnchorState::Candidate);
}

/// S5d2 r2 major 3 (DEC-1972): the step that ends a batch without a round publishes how it
/// ended. Caught up at T, a peer's batch extends T to U in its one step and stops at the next
/// header, whose bits are wrong. Held as the batch settles, the state at U is not caught up, and
/// behind as it was, with no observation; settling publishes nothing new.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stopping_step_publishes_how_it_ended() {
    let (r, _h, mut ingress, mut replies) = with_replies().await;
    let a = mine(anchor().hash, 1_000_000, 2, 1);
    let t = covered(&r, &mut ingress, &a).await;
    bound(replies.recv()).await.expect("covered's reply");
    let time = 1_000_000 + 2 * SPACING;
    let u = mine(a[1].hash(), time, 1, 2)[0];
    let wrong = mine_bits(u.hash(), time + SPACING, 1, 2, POW_LIMIT_BITS - 1)[0];
    let after = mine(wrong.hash(), time + 2 * SPACING, 1, 2)[0];
    let held = r.host.subs.task.hold_settle();
    let mut status = r.host.status();
    assert_eq!(ingress.offer(PEER, bridged(&[u, wrong, after])).unwrap(), Offered::Queued);
    bound(held.reached.notified()).await;
    let at_u = *status.borrow_and_update();
    assert_eq!(
        (at_u.tip.hash, at_u.caught_up, at_u.behind, at_u.observed, at_u.outstanding),
        (u.hash(), false, false, t.observed, None)
    );
    held.release.notify_one();
    let got = bound(replies.recv()).await.expect("a reply").reply.expect("taken");
    assert!(got.stop.is_some() && got.refused.is_none(), "{got:?}");
    assert!(!status.has_changed().unwrap(), "settling publishes nothing new");
}

/// S5d2 r2 major 4 (DEC-1972): an outstanding observation keeps the chain behind across a step
/// that disconnects the tip. Neither caught up nor behind at A6, an unknown hash is observed;
/// a ChainLock on B4 then selects B: the state at B4 is still behind, not caught up, with the
/// observation outstanding.
#[tokio::test]
async fn an_outstanding_observation_stays_behind_across_a_reorg() {
    let mut f = Forked::new(None).await;
    let (ingress, _replies) = forked_ingress(&mut f);
    let a6 = *f.host.status().borrow();
    assert!(!a6.caught_up && !a6.behind, "{a6:?}");
    ingress.unknown_hash();
    f.host.handle().apply_chainlock(f.lock_on_b()).await.unwrap();
    let b4 = *f.host.status().borrow();
    let observed = a6.observed + 1;
    assert_eq!(
        (b4.tip.hash, b4.caught_up, b4.behind, b4.outstanding),
        (f.b[1].hash(), false, true, Some(observed))
    );
}

/// S5d3 (S5d2 scoped minor 2): an outstanding observation survives a Replace. A Candidate host
/// observes an unknown hash, then is replaced: generation 2's first state keeps the observation
/// and its count, behind and not caught up.
#[tokio::test]
async fn an_outstanding_observation_survives_a_replace() {
    let (r, _h, ingress) = candidate().await;
    let before = *r.host.status().borrow();
    ingress.unknown_hash();
    let out = r.host.handle().replace(pack_anchor(&pack(10000, 1_000_000))).await;
    assert!(matches!(out, ReplaceOutcome::Replaced(ChainGen(2))), "{out:?}");
    let now = *r.host.status().borrow();
    let observed = before.observed + 1;
    assert_eq!(
        (now.gen, now.observed, now.outstanding, now.caught_up, now.behind),
        (ChainGen(2), observed, Some(observed), false, true)
    );
}
