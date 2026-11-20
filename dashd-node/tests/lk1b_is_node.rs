//! LK1b-s S1d on NodeSut (PLAN-LK1b §6 S1d): the IS store behind the node's load, the listener
//! over V and the restarts, driven by the main capture's own ops up to a point; and the S1f
//! carry-ins that need the node (the evo release failure through `IsFxSource`, the undo path, the
//! product digest log without `fx`).

#![cfg(feature = "test-support")]

#[allow(dead_code)]
mod lk1b_is;

use std::sync::Arc;
use std::time::Duration;

use dash_evo::locks::isdlock_hash;
use dash_kv::MemoryEngine;
use dash_primitives::BlockHash;
use dash_statedigest::{Effect, Hash256, Kind, Lines, Query, Record};
use dash_types::codec::BaseCodec;
use dashd_locks::is::LockedTx;
use dashd_node::sut::{Harness, Teardown};
use lk1b_is::node::NodeSide;
use lk1b_is::store::{Call, Tap, TapHost};
use lk1b_is::{Fixture, IsSide, Op, Rows, Step, attempts, clock, is_effects};

/// The rows of one IS table.
fn table<'a>(rows: &'a Rows, name: &str) -> Vec<&'a [u8]> {
    rows.iter()
        .filter(|(k, _)| lk1b_is::table(k) == Some(name))
        .map(|(k, _)| k.as_slice())
        .collect()
}

/// `fixture` driven with `harness` up to and including the first step `until` accepts, and the
/// clock it last set.
fn drive_to(fixture: &Fixture, harness: Harness, until: impl Fn(&Step) -> bool) -> (NodeSide, i64) {
    let mut side = NodeSide::new(fixture, harness).expect("the side");
    let mut now = None;
    for step in &fixture.steps {
        let t = clock(now, step);
        now = Some(t);
        side.set_time(t);
        side.apply(&step.op).unwrap_or_else(|e| panic!("step {}: {e}", step.step));
        if until(step) {
            return (side, t);
        }
    }
    panic!("no step matched");
}

/// The main capture driven up to and including the first step `until` accepts.
fn driven(until: impl Fn(&Step) -> bool) -> NodeSide {
    drive_to(&lk1b_is::capture("main"), Harness::default(), until).0
}

/// Scenario (a)'s lock, connected by its block: the listener's mined mark is in the store. A
/// SIGKILL restart keeps every committed row, and so does a clean one.
#[test]
fn restarts_keep_every_committed_lock_and_its_mined_mark() {
    let mut side = driven(|s| s.scenario == "a" && matches!(s.op, Op::Block { .. }));
    let rows = side.sut.is_rows();
    assert_eq!(table(&rows, "is_i").len(), 1, "(a)'s lock");
    assert_eq!(table(&rows, "is_m").len(), 1, "its block's mined mark");
    let killed = side.sut.restart_report(Teardown::Kill);
    assert_eq!(killed.is_rows, rows, "as the kill left them");
    assert_eq!(side.sut.is_rows(), rows, "after the load");
    let clean = side.sut.restart_report(Teardown::Clean);
    assert_eq!(clean.is_rows, rows);
    assert_eq!(side.sut.is_rows(), rows);
}

/// Scenario (e)'s `-maxtipage=1` restart (S1e Q4): the restarted process stays in IBD while its
/// tip is older than a second, and the next restart, with the default, leaves it.
#[test]
fn a_max_tip_age_restart_holds_ibd_until_the_next_restart() {
    let mut side = driven(|s| s.scenario == "a" && matches!(s.op, Op::Block { .. }));
    let ibd = |side: &NodeSide| side.sut.chain().expect("V runs").view_handle().ibd_service_cache();
    assert!(!ibd(&side), "a recent tip");
    let tip_time = side.sut.chain().unwrap().view_handle().view().snap.tip.tip_time;
    side.set_time(i64::from(tip_time) + 2);
    side.sut.restart_with_max_tip_age(Teardown::Clean, Duration::from_secs(1));
    assert!(ibd(&side), "the tip is two seconds old");
    side.sut.restart_report(Teardown::Clean);
    assert!(!ibd(&side), "the default six hours");
}

/// (c)'s ChainLock of the conflicting block: its enforcement reconnects the block, whose IS loop
/// removes the lock its spend conflicts with.
fn c_s_removal(main: &Fixture) -> (&Step, Hash256) {
    let step = main
        .steps
        .iter()
        .find(|s| s.scenario == "c" && s.label == "ChainLock of the conflicting block")
        .expect("(c)'s ChainLock");
    let Op::ClRecord {
        hash,
        ..
    } = &step.op
    else {
        panic!("step {} is a {}", step.step, step.op.tag())
    };
    (step, *hash)
}

/// The block's connect attempts in `records`: each one's `fx`.
fn connects_of(records: &[Record], block: Hash256) -> Vec<Option<Vec<Effect>>> {
    attempts(records)
        .into_iter()
        .filter(|a| a.id.b == block && matches!(a.kind, Kind::Connect | Kind::Reject))
        .map(|a| a.fx)
        .collect()
}

/// Each attempt in `records`: its kind, height, block, origin and `fx`.
fn summary(records: &[Record]) -> Vec<String> {
    attempts(records)
        .iter()
        .map(|a| {
            format!("{:?} h{} {} {:?} fx {:?}", a.kind, a.id.h, a.id.b.to_hex(), a.id.org, a.fx)
        })
        .collect()
}

/// The main capture driven with `harness` through (c)'s removal: the side after it, the IS rows
/// before it and its records.
fn through_c_s_removal(harness: Harness) -> (NodeSide, Rows, Vec<Record>, Hash256) {
    let main = lk1b_is::capture("main");
    let (at, block) = c_s_removal(&main);
    let (mut side, t) = drive_to(&main, harness, |s| s.step + 1 == at.step);
    let before = side.sut.is_rows();
    side.set_time(clock(Some(t), at));
    let records = side.apply(&at.op).expect("the ChainLock").records;
    (side, before, records, block)
}

/// S1f D1 end to end (PLAN-LK1b §5.3 item 7): NodeEvo applies the reconnect's whole log, the
/// removal committing to `llmq/isdb`, then fails in `release_attempt`. The finish failed, so the
/// attempt's record carries no `fx`, though `IsFxSource` holds the committed removal's
/// observation; the node latch holds the facade's refusal. The store is outside that latch: the
/// removal stays committed, as in the twin without the fault, whose record renders it as `is_rm`
/// (and whose connected block's ChainLock then confirms its other locks).
#[test]
fn a_failed_release_leaves_fx_absent_and_its_twin_renders_is_rm() {
    let (twin, before, records, block) = through_c_s_removal(Harness::default());
    assert_eq!(twin.sut.latched(), None);
    let fx = connects_of(&records, block);
    let [Some(fx)] = fx.as_slice() else {
        panic!("one connect with fx: {fx:?}; {:#?}", summary(&records))
    };
    let effects = is_effects(fx);
    let [
        Effect::IsRm {
            islock,
            ..
        },
    ] = effects.as_slice()
    else {
        panic!("one is_rm: {fx:?}")
    };
    let lock = islock.to_wire_bytes();
    let stored = |rows: &Rows| table(rows, "is_i").iter().any(|k| k.ends_with(&lock));
    assert!(stored(&before) && !stored(&twin.sut.is_rows()), "the twin removed the lock");

    let harness = Harness {
        release_fault_on_is_removal: true,
        ..Harness::default()
    };
    let (failed, before_failed, records, _) = through_c_s_removal(harness);
    assert_eq!(before_failed, before, "the same store before the op");
    let latched = failed.sut.latched().expect("the release fault latched the node");
    assert!(latched.message.contains("released with attempt"), "{latched:?}");
    let fx = connects_of(&records, block);
    assert!(!fx.is_empty() && fx.iter().all(Option::is_none), "{:#?}", summary(&records));
    // The block never connected, so its listener confirmed nothing; the removal stands.
    assert!(!stored(&failed.sut.is_rows()), "the removal committed");
}

/// PLAN-LK1b §5.3 item 4: the undo path never carries the IS removal. Invalidating (c)'s
/// reconnected block disconnects it through NodeEvo's real undo log: no applier is asked for a
/// removal (which would be `Fatal` without its observation), no record carries `is_rm`, and the
/// removed lock stays removed and archived, as Core's disconnect restores nothing.
#[test]
fn disconnecting_the_removal_s_block_carries_no_is_removal() {
    let (side, _, _, block) = through_c_s_removal(Harness::default());
    let rows = side.sut.is_rows();
    let chain = side.sut.chain().expect("V runs");
    let hash = BlockHash::from(block.to_wire_bytes());
    chain.invalidate_block(hash).recv().expect("V answers").expect("a known block");
    side.sut.settle();
    assert_eq!(side.sut.latched(), None);
    let records = side.sut.take_records().unwrap();
    let all = attempts(&records);
    assert!(all.iter().any(|a| a.kind == Kind::Disconnect && a.id.b == block), "{all:?}");
    assert!(
        all.iter().all(|a| is_effects(a.fx.as_deref().unwrap_or(&[])).is_empty()),
        "{:#?}",
        summary(&records)
    );
    let after = side.sut.is_rows();
    for name in ["is_i", "is_tx", "is_in", "is_a1", "is_a2"] {
        assert_eq!(table(&after, name), table(&rows, name), "{name}");
    }
}

/// CP Q9(a): rdashd's own `-statedigestlog` keeps no `fx` source in LK1b-s. (c)'s removal, which
/// the records tap renders as `is_rm`, leaves that log's record of the same connect without `fx`
/// (a connector takes one digest sink, so each runs on its own).
#[test]
fn the_product_digest_log_writes_no_fx_where_the_tap_renders_is_rm() {
    let (_, _, tapped, block) = through_c_s_removal(Harness::default());
    let fx = connects_of(&tapped, block);
    assert!(matches!(fx.as_slice(), [Some(fx)] if is_effects(fx).len() == 1), "{fx:?}");

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("digest.jsonl");
    let harness = Harness {
        statedigestlog: Some(path.clone()),
        ..Harness::default()
    };
    let (side, _, untapped, _) = through_c_s_removal(harness);
    assert_eq!(untapped, [], "no tap");
    // The log is written out as the process ends.
    drop(side);
    let product = std::fs::read(&path).unwrap();
    let records: Vec<Record> = Lines::new(&product)
        .map(|(line, r)| r.unwrap_or_else(|e| panic!("line {line}: {e:?}")))
        .collect();
    let fx = connects_of(&records, block);
    assert!(!fx.is_empty() && fx.iter().all(Option::is_none), "{:#?}", summary(&records));
}

/// MN0 ruling 2 (2026-11-18): a restart between (c)'s `conflict-tx-lock` rejection and its
/// ChainLock reconnects the rejected block during startup's import. The node is in IBD there, and
/// Core's ConnectBlock runs its InstantSend conflict check only out of IBD, so the connect asks no
/// `is_conflict`, removes no lock and leaves the IS rows as they were.
// Core: validation.cpp:2810-2812
#[test]
fn a_restart_after_c_s_rejection_reconnects_its_block_in_ibd_with_no_is_check() {
    let main = lk1b_is::capture("main");
    let (cl, block) = c_s_removal(&main);
    let rejection = &main.steps[cl.step - 2];
    assert!(
        matches!(rejection.op, Op::Block { hash, .. } if hash == block),
        "step {} delivers (c)'s conflicting block",
        rejection.step
    );
    let (mut side, _) = drive_to(&main, Harness::default(), |s| s.step == rejection.step);
    let tip = |side: &NodeSide| side.sut.chain().expect("V runs").view_handle().view().snap.tip;
    assert_ne!(tip(&side).tip_hash.as_bytes(), &block.to_wire_bytes(), "rejected");
    let rows = side.sut.is_rows();
    assert!(!table(&rows, "is_i").is_empty(), "(c)'s locks are stored");

    let restarted = side.sut.restart_report(Teardown::Clean);
    side.sut.settle();
    let records = side.sut.take_records().unwrap();
    let connects: Vec<_> = attempts(&records)
        .into_iter()
        .filter(|a| a.id.b == block)
        .map(|a| (a.kind, a.id.org))
        .collect();
    assert!(matches!(connects.as_slice(), [(Kind::Connect, _)]), "{:#?}", summary(&records));
    let asked: Vec<_> = records
        .iter()
        .filter_map(|r| match r {
            Record::Attempt(a) if a.id.b == block => Some(a.q.clone()),
            _ => None,
        })
        .collect();
    let [asked] = asked.as_slice() else {
        panic!("one A for the block: {asked:?}")
    };
    assert!(asked.contains(&Query::Ibd(true)), "in IBD: {asked:?}");
    assert!(!asked.iter().any(|q| matches!(q, Query::IsConflict { .. })), "no IS check: {asked:?}");
    assert_eq!(tip(&side).tip_hash.as_bytes(), &block.to_wire_bytes(), "the block is the tip");
    assert_eq!(restarted.is_rows, rows, "the teardown kept the rows");
    assert_eq!(side.sut.is_rows(), rows, "no lock removed, no mark written");
}

/// Core's ISDLOCK handler asks `AlreadyHave` only of a lock that passed its message checks, and
/// `KnownInstantSendLock` caches a miss: a lock the checks reject leaves the IS caches as they
/// were, with no entry for its hash. A stored lock is still `known`.
// Core: instantsend/net_instantsend.cpp:263-285; instantsend/db.cpp:235-240, :265-283
#[test]
fn a_lock_the_message_checks_reject_leaves_no_cache_entry() {
    struct Through;
    impl Tap for Through {
        fn around(&self, mut call: Call, inner: &mut dyn FnMut(&mut Call)) {
            inner(&mut call);
        }
    }
    let engine = Arc::new(MemoryEngine::new(&dashd_locks::is::db::SCHEMA));
    let host = Arc::new(TapHost::new(engine, Arc::new(Through)));
    let harness = Harness {
        is_engine: Some(Arc::clone(&host) as _),
        ..Harness::default()
    };
    let main = lk1b_is::capture("main");
    let is_lock = |s: &Step| matches!(s.op, Op::IsLock { .. });
    let Op::IsLock {
        bytes,
        ..
    } = &main.steps.iter().find(|s| is_lock(s)).expect("an ISLOCK").op
    else {
        unreachable!()
    };
    let (side, _) = drive_to(&main, harness, is_lock);
    let tip = side.sut.chain().expect("V runs").view_handle().view().snap.tip;
    let db = host.db().expect("the load's IsDb");
    let stored = LockedTx::decode(&mut &bytes[..]).expect("the capture's lock");
    assert!(db.known(&isdlock_hash(&stored)).unwrap(), "the first ISLOCK is stored");

    let with = |inputs: Vec<_>, cycle| {
        LockedTx::new(stored.version(), inputs, stored.txid(), cycle, stored.sig())
    };
    let cases = [
        ("no inputs", with(vec![], stored.cycle_hash()), "Malformed"),
        (
            "an unknown cycle",
            with(stored.inputs().clone(), BlockHash::from([7; 32])),
            "UnknownCycle",
        ),
        (
            "a cycle block off the dkgInterval",
            with(stored.inputs().clone(), tip.tip_hash),
            "InvalidCycle",
        ),
    ];
    for (what, lock, reject) in cases {
        let before = db.cache_snapshot();
        let mut encoded = Vec::new();
        lock.encode(&mut encoded);
        let refused = side.sut.is_lock(&encoded, None).expect_err(what);
        assert!(refused.contains(reject), "{what}: {refused}");
        let after = db.cache_snapshot();
        assert_eq!(after, before, "{what}: the caches as they were");
        assert!(after.locks.iter().all(|(hash, _)| *hash != isdlock_hash(&lock)), "{what}");
    }
    assert_eq!(side.sut.is_lock(bytes, None), Err("known".to_owned()));
}
