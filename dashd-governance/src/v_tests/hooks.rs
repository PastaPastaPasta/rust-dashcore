//! S4b on a real V: the queued tip hook and the init hook of the production listener, the
//! reorg and null-fork rules, and the list hook on undo.

use std::sync::atomic::AtomicUsize;
use std::sync::mpsc;

use dash_governance::manager::Effects;
use dash_governance::trigger::Status;
use dashd_chainstate::chain::{COMMAND_CAPACITY, NoWaitGuard};

use super::*;

/// The governance state a tip command can change, without the generation.
fn snapshot(rig: &Rig) -> String {
    let m = rig.handle.state_for_test();
    let view = m.trigger_view();
    format!(
        "{:?}",
        (view.cached_height, &view.superblocks, &view.objects, m.postponed(), m.export_state())
    )
}

/// The commands' events between `Enter(id)` and `Exit(id)`.
fn span(log: &[Ev], id: u64) -> &[Ev] {
    &log[position(log, Ev::Enter(id))..position(log, Ev::Exit(id))]
}

/// Runs on V before the governance listener's init: V freed a slot on its bounded lane when it
/// took the init command, and this waits until a waiting filler has taken it back.
struct Refilled(Arc<AtomicUsize>);

impl ValidationInterface for Refilled {
    fn initialize_current_block_tip(&self, _: Option<BlockId>, _: bool) {
        wait_until("a filler refills the lane", || {
            self.0.load(Ordering::SeqCst) == COMMAND_CAPACITY
        });
    }
}

/// (0) The init hook fires on V while V's bounded lane is full: its post takes the marked lane,
/// so V answers the init, and the tip command runs once the lane drains. It is the one command
/// the init adds, and its effects reach the sink once.
#[test]
fn the_init_post_does_not_block_v_on_its_full_lane() {
    let rig = Rig::with(Cfg {
        register: true,
        tip_at_start: false,
        ..Cfg::default()
    });
    assert_eq!(rig.cached(), 0);
    let sent = Arc::new(AtomicUsize::new(0));
    let listeners: [Arc<dyn ValidationInterface>; 2] = [rig.listener.clone(), rig.probe.clone()];
    for l in &listeners {
        rig.signals.unregister(l);
    }
    rig.signals.register(Arc::new(Refilled(sent.clone())));
    for l in listeners {
        rig.signals.register(l);
    }
    let (entered, entered_rx) = mpsc::channel();
    let (go, go_rx) = mpsc::channel::<()>();
    let held = rig.v.query(move |_| {
        entered.send(()).expect("the test waits");
        let _ = go_rx.recv();
    });
    entered_rx.recv_timeout(Duration::from_secs(60)).expect("V runs the held query");
    let first = rig.exec.next_id();
    let init = rig.v.initialize_current_block_tip();
    let fillers: Vec<_> = (0..4)
        .map(|_| {
            let (v, sent) = (rig.v.clone(), sent.clone());
            std::thread::spawn(move || {
                for _ in 0..COMMAND_CAPACITY / 4 + 2 {
                    drop(v.query(|_| ()));
                    sent.fetch_add(1, Ordering::SeqCst);
                }
            })
        })
        .collect();
    // The init and these fill the lane; the fillers' other sends wait on it.
    wait_until("the lane is full", || sent.load(Ordering::SeqCst) == COMMAND_CAPACITY - 1);
    go.send(()).expect("V waits");

    let Some(answer) = init.recv_timeout(Duration::from_secs(60)) else {
        std::mem::forget(rig);
        panic!("V blocked on its own lane at the init post");
    };
    answer.expect("V answered the init");
    held.recv().expect("the held query");
    for filler in fillers {
        filler.join().expect("a filler");
    }
    wait_until("the init's tip command runs", || rig.exec.idle());
    assert_eq!(rig.exec.next_id(), first + 1, "the init posts one command");
    let log = rig.log.take();
    position(&log, Ev::Exit(first));
    assert_eq!(rig.cached(), (SB - 2) as i32);
    assert_eq!(rig.sink.take().len(), 1, "the init's effects, once");
    rig.assert_running();
}

/// (iv) An `invalidateblock` sent while the tip command waits in `find_tx` runs after it: that
/// command saw the collateral's block on the active chain, and the null-fork tip command the
/// invalidate leads to sees it off the chain, 0 confirmations, so the postponed proposal goes.
#[test]
fn an_invalidate_sent_during_a_tip_command_runs_after_it() {
    let mut rig = Rig::with(Cfg {
        register: true,
        ..Cfg::default()
    });
    let (obj, tx) = proposal(1);
    let (key, hash) = (txid(&obj), object::hash(&obj));
    let funding = rig.at(SB - 2).hash();
    rig.index.put(&tx, funding);
    assert!(rig.handle.admit(obj).wait().expect("ran").outcome.expect("admit").postponed);
    rig.list_at(&[SB - 2, SB - 1]);
    rig.extend(0, OUTPUT_VALUE);
    assert_eq!(rig.settle(1), [(SB - 1, Some(SB - 2), false)]);
    assert!(rig.handle.state_for_test().postponed().contains_key(&hash));
    rig.log.take();

    let (entered, release) = rig.index.hold(key);
    let first = rig.exec.next_id();
    let tip = std::thread::spawn({
        let handle = Arc::clone(&rig.handle);
        move || handle.tip((SB - 1) as i32).wait().expect("ran")
    });
    entered.recv_timeout(Duration::from_secs(60)).expect("the pass reached the held lookup");
    let invalidated = rig.v.invalidate_block(funding);
    release.wait();
    tip.join().expect("tip").result.expect("tip");
    assert_eq!(invalidated.recv().expect("V"), Ok(()));
    assert_eq!(rig.settle(1), [(SB - 3, None, false)]);

    let log = rig.log.take();
    let (t, null) = (first, first + 1);
    assert!(position(&log, Ev::Exit(t)) < position(&log, Ev::Undone(SB - 1)), "{log:?}");
    assert!(position(&log, Ev::Undone(SB - 2)) < position(&log, Ev::Enter(null)), "{log:?}");
    assert!(span(&log, t).contains(&Ev::Active(funding, Some(SB - 2), SB - 1)), "{log:?}");
    assert!(span(&log, null).contains(&Ev::Active(funding, None, SB - 3)), "{log:?}");
    let state = rig.handle.state_for_test();
    assert!(state.postponed().is_empty() && state.object(&hash).is_none());
    drop(state);
    assert_eq!(rig.cached(), (SB - 3) as i32);
    rig.assert_lock_order();
    rig.assert_running();
}

/// Plan §4 (b) on the DIP3 rig: `invalidateblock` with no replacement sends the tip signals with
/// a null fork, so the cached height and the DMN tip move down to 40, and the tip work runs
/// there over 40's list: masternode 3, gone from it, loses its vote, the proposal whose
/// collateral left the chain leaves the postponed set, and the trigger at 40 is executed. Every
/// list a hook or a command read was V's own DMN-tip list at that point, the undos' included:
/// each `InvalidateBlock` iteration's adding drain moved the DMN tip, and the next iteration's
/// undo hook read it published.
// Core: validation.cpp v24.0.2:3953-4052; dsnotificationinterface.cpp v24.0.2:55-72
#[test]
fn a_null_fork_invalidate_runs_the_tip_work_at_the_post_invalidate_tip() {
    let mut rig = Rig::with(Cfg {
        register: true,
        dip3: true,
        ..Cfg::default()
    });
    rig.list_at(&(1..=SB + 3).collect::<Vec<_>>());
    for _ in SB - 1..=SB + 3 {
        rig.extend(0, OUTPUT_VALUE);
    }
    assert_eq!(rig.settle(5).last(), Some(&(SB + 3, Some(SB + 2), false)));
    let trigger = crate::tests::trigger(1, [9; 32], SB as i32);
    let hash = object::hash(&trigger);
    assert!(rig.handle.admit(trigger).wait().expect("ran").outcome.expect("admit").held);
    let three = masternodes(3);
    for (n, signal) in [(1, vote::SIGNAL_FUNDING), (3, vote::SIGNAL_VALID)] {
        let (outcome, _) = rig.handle.process_vote(&trigger_vote(n, hash, signal), &three);
        assert_eq!(outcome.result, Ok(true), "vote {n}");
    }
    let (obj, tx) = proposal(1);
    rig.index.put(&tx, rig.at(SB + 2).hash());
    assert!(rig.handle.admit(obj.clone()).wait().expect("ran").outcome.expect("admit").postponed);
    assert_eq!(rig.cached(), (SB + 3) as i32);
    assert_eq!(rig.handle.published().superblocks.get(&hash).expect("held").status, Status::Valid);
    rig.log.take();

    assert_eq!(rig.v.invalidate_block(rig.at(MN3_FROM).hash()).recv().expect("V"), Ok(()));
    assert_eq!(rig.settle(1), [(SB, None, false)]);

    let log = rig.log.take();
    let read: Vec<_> = log
        .iter()
        .filter_map(|e| match e {
            Ev::HookList(read, own) | Ev::CommandList(read, own) => Some((*read, *own)),
            _ => None,
        })
        .collect();
    assert!(!read.is_empty() && read.iter().all(|(read, own)| read == own), "{log:?}");
    let undone: Vec<_> = log
        .windows(2)
        .filter_map(|w| match w {
            [Ev::Undone(h), Ev::HookList(read, _)] => Some((*h, *read)),
            _ => None,
        })
        .collect();
    let undone_at = |h: u32| (h, rig.at(h).hash());
    assert_eq!(undone, [undone_at(SB + 3), undone_at(SB + 2), undone_at(SB + 1)], "{log:?}");
    let commands: Vec<_> = log
        .iter()
        .filter_map(|e| match e {
            Ev::CommandList(read, _) => Some(*read),
            _ => None,
        })
        .collect();
    assert_eq!(commands, [rig.at(SB).hash()], "{log:?}");
    assert_eq!(log.last(), Some(&Ev::Cached(SB as i32)), "{log:?}");

    let view = rig.v.view();
    assert_eq!(view.dmn_tip, Some(rig.id(SB)));
    assert_eq!(*view.mn_list.block_hash(), rig.at(SB).hash());
    let state = rig.handle.state_for_test();
    assert_eq!(state.cached_height(), SB as i32);
    assert_eq!(state.object(&hash).expect("held").file.len(), 1, "masternode 3's vote went");
    assert!(state.postponed().is_empty() && state.object(&object::hash(&obj)).is_none());
    drop(state);
    let published = rig.handle.published();
    assert_eq!(published.superblocks.get(&hash).expect("held").status, Status::Executed);
    rig.assert_running();
}

/// S4a r1 F4: an undo raises the production listener's list hook, as Core signals on
/// disconnect, and its clean erases the trigger the superblock query marked for deletion. No
/// command runs: the hook is inline on V.
// Core: validation.cpp v24.0.2:2379-2382; dsnotificationinterface.cpp v24.0.2:90-96
#[test]
fn the_list_hook_cleans_on_undo() {
    let mut rig = Rig::start(true);
    rig.extend(0, OUTPUT_VALUE);
    let hash = rig.admit_trigger();
    cast(&rig.handle, 1, hash, vote::SIGNAL_DELETE);
    cast(&rig.handle, 2, hash, vote::SIGNAL_DELETE);
    rig.extend(0, OUTPUT_VALUE);
    assert!(rig.live(&hash).expect("held").cached_delete);
    rig.list_at(&[SB]);
    let later = clock().now + 600;
    rig.env.set(GovClock::new(later, later), true, true);
    let commands = rig.exec.next_id();
    rig.sink.take();
    rig.log.take();

    assert_eq!(rig.v.invalidate_block(rig.at(SB).hash()).recv().expect("V"), Ok(()));
    let log = rig.log.take();
    assert!(log.contains(&Ev::Undone(SB)), "{log:?}");
    assert_eq!(rig.exec.next_id(), commands);
    assert!(rig.live(&hash).is_none());
    assert!(rig.handle.published().superblocks.get(&hash).is_none());
    let [hook] = &rig.sink.take()[..] else {
        panic!("one submission, from the hook");
    };
    assert!(hook.meta.contains(&MetaOp::RemoveObject(hash)), "{:?}", hook.meta);
    rig.assert_running();
}

/// Plan §4 (a): a reorg's `UpdatedBlockTip(new, fork)` moves the cached height to the new tip:
/// up to 39 when a longer branch from 36 takes over, and down to 38 when that branch is
/// invalidated and the old one comes back. The invalidate's own null-fork tip is at 36, and the
/// ABC after it returns at each block that beats 36's work, so 37 and 38 come one at a time.
// Core: validation.cpp v24.0.2:3679-3683, :3833-3836
#[test]
fn a_reorg_moves_the_cached_height_to_the_new_tip_up_and_down() {
    let rig = Rig::with(Cfg {
        register: true,
        ..Cfg::default()
    });
    let a36 = rig.at(SB - 4).clone();
    let b37 = rig.on(&a36, SB - 3, 1, OUTPUT_VALUE);
    let b38 = rig.on(&b37, SB - 2, 1, OUTPUT_VALUE);
    let b39 = rig.on(&b38, SB - 1, 1, OUTPUT_VALUE);
    assert_eq!(rig.submit(&b37), Some(true));
    assert_eq!(rig.submit(&b38), Some(true));
    assert_eq!(rig.tip(), rig.at(SB - 2).hash());
    rig.connect(&b39);
    assert_eq!(rig.settle(1), [(SB - 1, Some(SB - 4), false)]);
    assert_eq!(rig.cached(), (SB - 1) as i32);

    assert_eq!(rig.v.invalidate_block(b37.hash()).recv().expect("V"), Ok(()));
    let back =
        [(SB - 4, None, false), (SB - 3, Some(SB - 4), false), (SB - 2, Some(SB - 3), false)];
    assert_eq!(rig.settle(3), back);
    assert_eq!(rig.tip(), rig.at(SB - 2).hash());
    assert_eq!(rig.cached(), (SB - 2) as i32);
    rig.assert_running();
}

/// Core's callback guard: a fork equal to the new tip (blocks disconnected, none connected)
/// posts nothing, so the cached height and the DMN tip stay. V never sends one (its ABC
/// guards it), so the listener gets it directly, on a marked thread as the signals thread is.
// Core: dsnotificationinterface.cpp v24.0.2:57-58
#[test]
fn a_fork_equal_to_the_new_tip_changes_nothing() {
    let rig = Rig::with(Cfg {
        register: true,
        ..Cfg::default()
    });
    let parent = rig.id(SB - 3);
    let dmn_tip = rig.v.view().dmn_tip;
    let first = rig.exec.next_id();
    {
        let _marked = NoWaitGuard::set();
        rig.listener.updated_block_tip(parent, Some(parent), false);
    }
    rig.drain_v();
    assert_eq!(rig.exec.next_id(), first);
    assert_eq!(rig.cached(), (SB - 2) as i32);
    assert_eq!(rig.v.view().dmn_tip, dmn_tip);
}

/// In IBD both tip hooks return before posting; out of it the queued hook posts.
#[test]
fn the_tip_hooks_return_in_ibd() {
    let rig = Rig::with(Cfg {
        register: true,
        ..Cfg::default()
    });
    let (parent, fork) = (rig.id(SB - 3), rig.id(SB - 4));
    let first = rig.exec.next_id();
    {
        let _marked = NoWaitGuard::set();
        rig.listener.updated_block_tip(parent, Some(fork), true);
        rig.listener.initialize_current_block_tip(Some(parent), true);
    }
    rig.drain_v();
    assert_eq!(rig.exec.next_id(), first);
    assert_eq!(rig.cached(), (SB - 2) as i32);

    {
        let _marked = NoWaitGuard::set();
        rig.listener.updated_block_tip(parent, Some(fork), false);
    }
    wait_until("the tip command runs", || rig.exec.idle());
    assert_eq!(rig.exec.next_id(), first + 1);
    assert_eq!(rig.cached(), (SB - 3) as i32);
}

/// Governance not loaded (`IsValid` false): a connect's queued tip and the init post nothing.
#[test]
fn disabled_governance_posts_from_neither_tip_hook() {
    let mut rig = Rig::with(Cfg {
        loaded: false,
        register: true,
        tip_at_start: false,
        ..Cfg::default()
    });
    let first = rig.exec.next_id();
    rig.extend(0, OUTPUT_VALUE);
    assert_eq!(rig.settle(1), [(SB - 1, Some(SB - 2), false)]);
    rig.v.initialize_current_block_tip().recv().expect("V");
    rig.drain_v();
    assert_eq!(rig.exec.next_id(), first);
    assert_eq!(rig.cached(), 0);
    assert!(rig.sink.take().is_empty());
    rig.assert_running();
}

/// Plan §4 (d): the same tip delivered again changes nothing: the executed trigger stays
/// executed, and the second command's effects are empty.
#[test]
fn the_same_tip_twice_changes_nothing_the_second_time() {
    let mut rig = Rig::with(Cfg {
        register: true,
        ..Cfg::default()
    });
    let a39 = rig.extend(0, OUTPUT_VALUE);
    rig.settle(1);
    let hash = rig.admit_trigger();
    cast(&rig.handle, 1, hash, vote::SIGNAL_FUNDING);
    let payments = rig.best_payments().expect("a best superblock");
    rig.connect(&rig.superblock_on(&a39, 3, &payments));
    assert_eq!(rig.settle(1), [(SB, Some(SB - 1), false)]);
    assert_eq!(
        rig.handle.published().superblocks.get(&hash).expect("held").status,
        Status::Executed
    );
    let once = snapshot(&rig);
    rig.sink.take();

    let (tip, fork) = (rig.v.view().snap.tip.tip, rig.id(SB - 1));
    {
        let _marked = NoWaitGuard::set();
        rig.listener.updated_block_tip(tip, Some(fork), false);
    }
    wait_until("the tip command runs", || rig.exec.idle());
    assert_eq!(snapshot(&rig), once);
    let [again] = &rig.sink.take()[..] else {
        panic!("one submission, from the second tip");
    };
    assert_eq!(format!("{again:?}"), format!("{:?}", Effects::default()));
    rig.assert_running();
}

/// After a restart-shaped start (cached height 0), the init hook sets the cached height.
#[test]
fn the_init_hook_sets_the_cached_height_after_a_restart() {
    let rig = Rig::with(Cfg {
        register: true,
        tip_at_start: false,
        ..Cfg::default()
    });
    assert_eq!(rig.cached(), 0);
    let first = rig.exec.next_id();
    rig.v.initialize_current_block_tip().recv().expect("V");
    wait_until("the init's tip command runs", || rig.exec.idle());
    assert_eq!(rig.exec.next_id(), first + 1);
    assert_eq!(rig.cached(), (SB - 2) as i32);
    assert_eq!(rig.probe.len(), 0, "the init is not a queued signal");
    rig.assert_running();
}

/// S4a r2 m1 on a real V: a trigger funded under the full list keeps its funding flag once the
/// list has no valid weighted masternode (the sentinel update returns early), so it still
/// reports triggered, but no trigger has a yes vote, so there is no best superblock and a
/// block paying the old payments fails "bad-cb-amount", as in Core. V keeps running.
// Core: governance/object.cpp v24.0.2:970-971; governance/superblock.cpp v24.0.2:564-575,
// :609-613, :627-630
#[test]
fn a_trigger_funded_earlier_with_no_weighted_masternodes_fails_bad_cb_amount() {
    let mut rig = Rig::start(true);
    let a39 = rig.extend(0, OUTPUT_VALUE);
    let hash = rig.admit_trigger();
    cast(&rig.handle, 1, hash, vote::SIGNAL_FUNDING);
    let payments = rig.best_payments().expect("a best superblock under the full list");
    let paid = rig.superblock_on(&a39, 3, &payments);
    assert_eq!(rig.verdict(&paid), None);
    assert!(rig.live(&hash).expect("held").cached_funding);

    rig.fixed.set(crate::tests::empty_list());
    assert!(rig.best_payments().is_none());
    rig.log.take();
    rig.submit(&paid);
    assert_eq!(rig.tip(), a39.hash());
    let log = rig.log.take();
    assert!(log.contains(&Ev::Asked(SB, true)), "{log:?}");
    assert!(rig.live(&hash).expect("held").cached_funding);
    assert_eq!(rig.verdict(&paid).as_deref(), Some("bad-cb-amount"));
    rig.assert_running();
}
