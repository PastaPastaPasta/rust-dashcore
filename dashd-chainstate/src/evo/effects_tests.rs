use std::sync::{Arc, Mutex};

use dash_evo::EffectKind;
use dashd_store::KvRead as _;

use super::*;
use crate::evo::codec::{self, decode_quorum_snapshot, quorum_snapshot_key};
use crate::evo::testkit;

/// Every applier call, as the kind it served; `fail` makes that kind's applier fail.
#[derive(Clone, Default)]
struct Calls {
    seen: Arc<Mutex<Vec<EffectKind>>>,
    fail: Option<EffectKind>,
}

impl Calls {
    fn call(&self, kind: EffectKind) -> Result<(), Fatal> {
        self.seen.lock().expect("calls").push(kind);
        if self.fail == Some(kind) {
            return Err(Fatal::new(format!("{kind:?} failed")));
        }
        Ok(())
    }

    fn seen(&self) -> Vec<EffectKind> {
        self.seen.lock().expect("calls").clone()
    }
}

impl SnapshotApplier for Calls {
    fn write(&mut self, _: LlmqType, _: &BlockHash, _: &QuorumSnapshot) -> Result<(), Fatal> {
        self.call(EffectKind::StoreQuorumSnapshot)
    }
}

impl IsLockApplier for Calls {
    fn remove(&mut self, _: &Hash256) -> Result<(), Fatal> {
        self.call(EffectKind::RemoveConflictingIsLock)
    }
}

impl MineableApplier for Calls {
    fn erase(&mut self, _: LlmqType, _: &QuorumHash, _: &[u8; 32]) -> Result<(), Fatal> {
        self.call(EffectKind::EraseMineableCommitment)
    }

    fn reinsert(&mut self, _: &Arc<LlmqCommitment>) -> Result<(), Fatal> {
        self.call(EffectKind::ReinsertMineableCommitment)
    }
}

impl PlatformBanApplier for Calls {
    fn reset(&mut self, _: &ProTxHash, _: i32) -> Result<(), Fatal> {
        self.call(EffectKind::ResetPlatformBan)
    }
}

impl NotifyApplier for Calls {
    fn notify(&mut self, _: &BlockHash, _: bool) -> Result<(), Fatal> {
        self.call(EffectKind::NotifyMnListChanged)
    }
}

fn appliers(calls: &Calls) -> EffectAppliers {
    EffectAppliers {
        snapshot: Box::new(calls.clone()),
        is_lock: Box::new(calls.clone()),
        mineable: Box::new(calls.clone()),
        platform_ban: Box::new(calls.clone()),
        notify: Box::new(calls.clone()),
    }
}

fn snapshot() -> QuorumSnapshot {
    QuorumSnapshot {
        active_quorum_members: vec![true, false, true],
        skip_list_mode: 1,
        skip_list: vec![2, 5],
    }
}

/// One effect of every kind, in [`EffectKind::ALL`]'s order.
fn one_of_each() -> Vec<Effect> {
    let commitment = Arc::new(testkit::commitment(
        LlmqType::LlmqTest,
        testkit::quorum_hash(&testkit::genesis()),
        1,
    ));
    let log = vec![
        Effect::StoreQuorumSnapshot {
            llmq_type: LlmqType::LlmqTestDip0024,
            cycle_base: testkit::genesis(),
            snapshot: snapshot(),
        },
        Effect::RemoveConflictingIsLock {
            lock_hash: Hash256::default(),
        },
        Effect::EraseMineableCommitment {
            llmq_type: LlmqType::LlmqTest,
            quorum_hash: commitment.quorum_hash,
            commitment_hash: [7; 32],
        },
        Effect::ReinsertMineableCommitment {
            commitment,
        },
        Effect::ResetPlatformBan {
            pro_tx_hash: ProTxHash::default(),
            height: 9,
        },
        Effect::NotifyMnListChanged {
            at: testkit::genesis(),
            undo: true,
        },
    ];
    assert_eq!(log.iter().map(Effect::kind).collect::<Vec<_>>(), EffectKind::ALL);
    log
}

/// D12: every kind reaches its own applier, in the log's emission order, and only a whole log
/// mints `EffectsApplied`.
#[test]
fn the_log_is_applied_in_order_each_effect_by_its_owner() {
    let calls = Calls::default();
    let log = one_of_each();
    appliers(&calls).apply_log(&log).expect("applied");
    assert_eq!(calls.seen(), EffectKind::ALL);
    // Reversed, in the reversed order.
    let calls = Calls::default();
    let reversed: Vec<_> = log.iter().rev().cloned().collect();
    appliers(&calls).apply_log(&reversed).expect("applied");
    assert_eq!(calls.seen(), EffectKind::ALL.into_iter().rev().collect::<Vec<_>>());
}

/// §3.8: an applier's fault stops the application there; nothing after it is applied, and no
/// `EffectsApplied` is minted.
#[test]
fn an_applier_fault_stops_the_log() {
    for (i, kind) in EffectKind::ALL.into_iter().enumerate() {
        let calls = Calls {
            fail: Some(kind),
            ..Calls::default()
        };
        let r = appliers(&calls).apply_log(&one_of_each());
        assert_eq!(r.map(|_| ()), Err(Fatal::new(format!("{kind:?} failed"))));
        assert_eq!(calls.seen(), EffectKind::ALL[..=i]);
    }
}

/// §6.2: the snapshot applier writes the row Core's `StoreSnapshotForBlock` writes, then records
/// a presumed effect; a second write for the same key overwrites it.
#[test]
fn a_snapshot_is_written_then_recorded_as_presumed() {
    let store = Arc::new(testkit::memory_store());
    let ledger = Arc::new(EffectLedger::default());
    let mut applier = StoreSnapshots {
        store: Arc::clone(&store),
        ledger: Arc::clone(&ledger),
    };
    let (t, base) = (LlmqType::LlmqTestDip0024, testkit::genesis());
    let read = || {
        store
            .db()
            .get(codec::cf("llmq_snapshot"), &quorum_snapshot_key(t, &base))
            .expect("read")
            .map(|b| decode_quorum_snapshot(&b).expect("decodes"))
    };
    assert_eq!(read(), None);
    applier.write(t, &base, &snapshot()).expect("written");
    assert_eq!((read(), ledger.current()), (Some(snapshot()), 1));
    let other = QuorumSnapshot {
        skip_list: vec![],
        ..snapshot()
    };
    applier.write(t, &base, &other).expect("written");
    assert_eq!((read(), ledger.current()), (Some(other), 2));
}

/// Q5: the stand-ins of owners not landed fail closed, except the platform bans, recorded; so
/// does the notify applier with no queued lists (D15).
#[test]
fn the_stand_ins_fail_closed_and_platform_bans_are_recorded() {
    let log = one_of_each();
    let bans = MemPlatformBans::default();
    let mut appliers = EffectAppliers {
        snapshot: Box::new(Calls::default()),
        is_lock: Box::new(UnreachableIsLock),
        mineable: Box::new(UnreachableMineable),
        platform_ban: Box::new(bans.clone()),
        notify: Box::new(MnListSignal::default().applier()),
    };
    for e in &log {
        let r = appliers.apply(e);
        match e.kind() {
            EffectKind::StoreQuorumSnapshot | EffectKind::ResetPlatformBan => r.expect("applied"),
            kind => assert!(r.is_err(), "{kind:?}"),
        }
    }
    assert_eq!(bans.resets(), [(ProTxHash::default(), 9)]);
}

/// SP1c-1 A3 (DEC-782): the platform-ban applier is reached only through the log's
/// `ResetPlatformBan` effects, each one in emission order with its masternode and height (a
/// repeat too: Core calls `ResetPlatformBan` once per ProUpServTx), and a log without one never
/// reaches it, whatever else it holds.
#[test]
fn platform_ban_resets_come_only_from_the_log() {
    let (calls, resets) = (Calls::default(), MemPlatformBans::default());
    let mut appliers = EffectAppliers {
        platform_ban: Box::new(resets.clone()),
        ..appliers(&calls)
    };
    let without: Vec<_> =
        one_of_each().into_iter().filter(|e| e.kind() != EffectKind::ResetPlatformBan).collect();
    appliers.apply_log(&without).expect("applied");
    assert_eq!(calls.seen().len(), EffectKind::ALL.len() - 1);
    assert!(resets.resets().is_empty());
    let (a, b) = (ProTxHash::from([1; 32]), ProTxHash::from([2; 32]));
    let reset = |pro_tx_hash, height| Effect::ResetPlatformBan {
        pro_tx_hash,
        height,
    };
    let mut log = without;
    log.insert(1, reset(a, 5));
    log.push(reset(b, 5));
    log.push(reset(a, 5));
    appliers.apply_log(&log).expect("applied");
    assert_eq!(resets.resets(), [(a, 5), (b, 5), (a, 5)]);
}
