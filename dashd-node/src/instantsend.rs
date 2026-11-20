//! InstantSend's node wiring (LK1b-s S1d): the `llmq/isdb` store, the parts one load's connector,
//! evo applier and listener share, and the listener's reads of V's tree and its one fault report.
//!
//! The store is its own database, outside the node latch ([`GuardedEngine`]), as Core's isdb is
//! its own `CDBWrapper` that no `AbortNode` closes (`instantsend/db.cpp:29-33`, `init.cpp:2033`):
//! its writes land whatever the chainstate's state, and the connector's removal batch commits
//! inline in `ConnectBlock` before anything after the IS loop can fail (PLAN-LK1b §2.4, §5.3).
//!
//! [`GuardedEngine`]: dashd_chainstate::store::GuardedEngine

use std::path::Path;
use std::sync::Arc;

use dash_chain::ChainReader;
use dash_consensus::BlockId;
use dash_evo::spork::SporkId;
use dash_kv::{ErrorKind, Failure, GlobalFailStop, KvEngine, StoreError};
use dash_params::ChainSetup;
use dashd_chainstate::chain::{LoadingFlags, ValidationInterface};
use dashd_chainstate::connect::{ChainHelper, FxSource};
use dashd_chainstate::evo::effects::IsLockApplier;
use dashd_chainstate::store::AbortSink;
use dashd_locks::is::chain::{
    GateSource, IsAttemptLedger, IsHelper, IsLedgerApplier, SharedLedger,
};
use dashd_locks::is::db::{IsDb, SCHEMA};
use dashd_locks::is::fx::IsFxSource;
use dashd_locks::is::listener::{BlockTree, IsFaults, IsListener, IsListenerParts};
use dashd_locks::is::seams::{IsStore, NoRecSigs};
use dashd_locks::is::{IsGate, IsTipCache};
use dashd_masternode::sporks::SporkManager;
use dashd_store::{RocksConfig, RocksEngine};

/// The store's directory under the network's data directory.
pub const ISDB_DIR: &str = "llmq/isdb";

/// The site a listener's failed store call latches the node at.
const ISDB_SITE: &str = "llmq/isdb write";

/// Opens the IS store for a load. `wipe` (`-reindex` or `-reindex-chainstate`) empties it, as
/// Core's `CDBWrapper` wipe does.
pub type IsOpener = Box<dyn FnOnce(bool) -> Result<Arc<dyn IsStore>, StoreError> + Send>;

/// rdashd's opener: a RocksDB database at `<data_dir>/llmq/isdb`.
// Core: instantsend/db.cpp:29-33; init.cpp:2033
pub fn rocks_opener(data_dir: &Path) -> IsOpener {
    let path = data_dir.join(ISDB_DIR);
    Box::new(move |wipe| {
        let (engine, _) = RocksEngine::open(&path, &SCHEMA, &RocksConfig::default())?;
        engine_opener(Arc::new(engine))(wipe)
    })
}

/// An opener over an engine the caller holds: a test's, kept across restarts.
pub fn engine_opener<E: KvEngine + 'static>(engine: Arc<E>) -> IsOpener {
    Box::new(move |wipe| Ok(Arc::new(IsDb::open_wiping(engine, wipe)?) as Arc<dyn IsStore>))
}

/// What one load keeps for its listener and lock intake: the store, Core's
/// `IsInstantSendEnabled` inputs and the tip cache. Never the attempt ledger: only V's own
/// objects hold that ([`IsVParts`]).
#[derive(Clone)]
pub struct IsParts {
    pub store: Arc<dyn IsStore>,
    pub gate: GateSource,
    pub tip: IsTipCache,
}

/// The connector's InstantSend parts for one attach: the shared parts and a fresh attempt
/// ledger, which only the helper, applier and `fx` source built from it hold (PLAN-LK1b §5.3
/// G1-b). The attach drops it once they are built, so V's unwind drops the ledger, and the gate
/// hold it keeps latches the store and wakes every parked caller.
pub struct IsVParts {
    parts: IsParts,
    ledger: SharedLedger,
}

impl IsParts {
    /// The parts over `store`, gated on `sporks`' SPORK_2 and V's `loading` flags.
    pub fn new(
        store: Arc<dyn IsStore>,
        sporks: Arc<SporkManager>,
        loading: Arc<LoadingFlags>,
    ) -> Self {
        let gate: GateSource = Arc::new(move || {
            IsGate::new(
                sporks.is_active(SporkId::SPORK_2_INSTANTSEND_ENABLED),
                loading.reindex(),
                loading.importing(),
            )
        });
        IsParts {
            store,
            gate,
            tip: IsTipCache::new(),
        }
    }

    /// V's parts for one attach, over a fresh attempt ledger.
    pub fn for_v(&self) -> IsVParts {
        IsVParts {
            parts: self.clone(),
            ledger: IsAttemptLedger::shared(),
        }
    }

    /// NetInstantSend's listener over V's tree `reader`, with ChainLocks read from `sporks` and
    /// failures reported once through `latch` and `abort`.
    pub fn listener(
        &self,
        params: &ChainSetup,
        reader: ChainReader,
        sporks: Arc<SporkManager>,
        latch: GlobalFailStop,
        abort: Arc<dyn AbortSink>,
    ) -> Arc<dyn ValidationInterface> {
        Arc::new(IsListener::new(
            params,
            IsListenerParts {
                store: Arc::clone(&self.store),
                tip: self.tip.clone(),
                gate: Arc::clone(&self.gate),
                tree: Arc::new(ChainTree(reader)),
                chainlocks_enabled: Arc::new(move || {
                    sporks.is_active(SporkId::SPORK_19_CHAINLOCKS_ENABLED)
                }),
                // SG1's recovered-signature store.
                recsigs: Arc::new(NoRecSigs),
                faults: Arc::new(NodeIsFaults {
                    latch,
                    abort,
                }),
            },
        ))
    }
}

impl IsVParts {
    /// The connector's helper: `inner` with the IS store's answers.
    pub fn helper<H: ChainHelper>(&self, inner: H) -> IsHelper<H> {
        IsHelper::live(
            inner,
            Arc::clone(&self.parts.store),
            Arc::clone(&self.ledger),
            Arc::clone(&self.parts.gate),
            self.parts.tip.clone(),
        )
    }

    /// The evo hooks' lock applier, over the helper's store and ledger.
    pub fn applier(&self) -> Box<dyn IsLockApplier> {
        Box::new(IsLedgerApplier::new(Arc::clone(&self.parts.store), Arc::clone(&self.ledger)))
    }

    /// A digest log's `fx` source over the attempt ledger.
    pub fn fx(&self) -> Box<dyn FxSource> {
        Box::new(IsFxSource(Arc::clone(&self.ledger)))
    }
}

/// V's block tree, read past V.
struct ChainTree(ChainReader);

impl BlockTree for ChainTree {
    fn height(&self, id: BlockId) -> Option<u32> {
        self.0.entry(id).map(|e| e.height)
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        self.0.ancestor(id, height)
    }
}

/// A listener store failure. Core's thrown `dbwrapper_error` ends its process from the scheduler
/// thread; here it trips the node latch and, when that trip won, aborts the node once. A failure
/// after another has latched reports nothing more.
struct NodeIsFaults {
    latch: GlobalFailStop,
    abort: Arc<dyn AbortSink>,
}

impl IsFaults for NodeIsFaults {
    fn failed(&self, site: &'static str, error: &StoreError) {
        let message = format!("{site}: {error}");
        let won = self.latch.trip(Failure {
            kind: error.kind().unwrap_or(ErrorKind::Other),
            site: ISDB_SITE,
            message: message.clone(),
        });
        if won {
            self.abort.abort_node(&message, None);
        } else {
            self.latch.settle();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::thread;
    use std::time::{Duration, Instant};

    use dash_chain::{ChainConfig, FailStop, HeaderChain, NoStore, Services, StaticSporks};
    use dash_consensus::Tx;
    use dash_consensus::primitives::{OutPoint, Transaction, TxHash, TxIn, TxType};
    use dash_evo::locks::isdlock_hash;
    use dash_kv::{AbortMark, MemoryEngine, RecordingSink};
    use dashd_chainstate::chain::genesis_block;
    use dashd_chainstate::connect::NodeHelper;
    use dashd_locks::is::LockedTx;

    use super::*;

    fn lock(tag: u8) -> (dashd_locks::is::IsLockHash, LockedTx) {
        let input = OutPoint::new(TxHash::from([tag; 32]), 0);
        let txid = TxHash::from([tag.wrapping_add(1); 32]);
        let lock = LockedTx::new(1, vec![input], txid, Default::default(), [0; 96].into());
        (isdlock_hash(&lock), lock)
    }

    /// A block transaction spending `lock(tag)`'s input: it conflicts with that lock.
    fn conflicting(tag: u8) -> Tx {
        let input = TxIn::new(OutPoint::new(TxHash::from([tag; 32]), 0), Default::default(), 0);
        Tx::new(Transaction::new(1, TxType::new(0), vec![input], vec![], 0, vec![]))
    }

    /// The listener's tree: nothing reads headers back.
    struct NoReads;

    impl FailStop for NoReads {
        fn read_failed(&self, what: &str) {
            panic!("the header latch tripped: {what}");
        }

        fn is_tripped(&self) -> bool {
            false
        }
    }

    /// PLAN-LK1b §5.3 G1-b in the node's own types, outside any tip hold: the parts a load keeps
    /// ([`IsParts`], alive for the whole case as `ChainSteps` keeps them) hold no attempt ledger.
    /// So when a V-like thread stages two removals through [`IsVParts`]' helper, commits the first
    /// through its applier and panics before `end_attempt`, its unwind drops the ledger, and the
    /// gate hold latches `llmq/isdb` and wakes the listener's `BlockConnected` parked on the gate.
    /// The listener gets the latched error and reports once; only the first stage landed.
    ///
    /// In the node a V panic inside ConnectBlock aborts the process first (`Hold::drop`'s
    /// `read_failed`, `CoreAbortSink`'s abort), so this pins the ownership invariant itself.
    #[test]
    fn a_v_unwind_after_a_stage_wakes_the_parked_listener_with_one_report() {
        let bound = Duration::from_secs(120);
        let (done, result) = mpsc::channel();
        thread::spawn(move || {
            v_unwinds_with_a_listener_parked();
            let _ = done.send(());
        });
        match result.recv_timeout(bound) {
            Ok(()) => {}
            Err(RecvTimeoutError::Timeout) => {
                panic!("the parked listener was not woken within {bound:?}: the gate stayed held")
            }
            Err(RecvTimeoutError::Disconnected) => panic!("the case failed (see above)"),
        }
    }

    fn v_unwinds_with_a_listener_parked() {
        let p = crate::chain::tests::regtest();
        let clock = crate::chain::tests::clock_at(&p);
        let args = crate::init::test_args(&[
            "-sporkkey=cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK",
        ]);
        let sporks = crate::sporks::spork_manager(&args, &p, Arc::clone(&clock)).expect("sporks");
        sporks.update_spork(SporkId::SPORK_2_INSTANTSEND_ENABLED, 0).expect("signed");
        let engine = Arc::new(MemoryEngine::new(&SCHEMA));
        let db = Arc::new(IsDb::open(Arc::clone(&engine)).unwrap());
        // Unchained: `lock(3)`'s txid is no input of `lock(7)`.
        let (locks, spends): (Vec<_>, Vec<_>) =
            [3, 7].map(|t| (lock(t), conflicting(t))).into_iter().unzip();
        for (hash, object) in &locks {
            db.write_new(hash, object).unwrap();
        }
        let parts = IsParts::new(Arc::clone(&db) as _, Arc::clone(&sporks), Arc::default());

        let v = parts.for_v();
        let (mut helper, mut applier, fx) = (v.helper(NodeHelper), v.applier(), v.fx());
        drop(v);
        let (staged, at_stage) = mpsc::channel();
        let (unwind, unwinding) = mpsc::channel::<()>();
        let v = thread::spawn(move || {
            let _fx = fx;
            let mut effects = Vec::new();
            let mut found = Vec::new();
            for tx in &spends {
                let lock = helper.is_conflict(tx).expect("no fault").expect("a conflict");
                helper.remove_conflicting_is_lock(&lock, &mut effects);
                found.push(lock.hash);
            }
            applier.remove(&found[0]).expect("the first stage commits");
            staged.send(()).unwrap();
            let _ = unwinding.recv();
            panic!("injected: V unwinds between a stage and end_attempt");
        });
        at_stage.recv().expect("V staged both removals");

        let latch = GlobalFailStop::new();
        let sink = Arc::new(RecordingSink::default());
        let svc = Services {
            clock,
            sporks: Arc::new(StaticSporks::for_network(&p, None)),
            fail: Arc::new(NoReads),
        };
        let tree: HeaderChain =
            HeaderChain::new_genesis(Arc::clone(&p), ChainConfig::node(&p), svc, NoStore)
                .expect("genesis");
        let genesis = genesis_block(&p);
        let id = tree.reader().lookup(&genesis.hash()).expect("the genesis entry");
        let listener =
            parts.listener(&p, tree.reader(), sporks, latch.clone(), Arc::clone(&sink) as _);
        let signals = thread::spawn(move || listener.block_connected(&genesis, id));
        let deadline = Instant::now() + Duration::from_secs(60);
        while db.gate_waiters() == 0 && !signals.is_finished() {
            assert!(Instant::now() < deadline, "the listener's call never reached the gate");
            thread::yield_now();
        }
        assert!(!signals.is_finished(), "the listener's call waits for V's stage");

        unwind.send(()).unwrap();
        assert!(v.join().is_err(), "V unwound");
        signals.join().expect("the listener returns");
        let tripped = latch.tripped().expect("the listener's report latched the node");
        assert_eq!(tripped.site, ISDB_SITE);
        assert!(tripped.message.starts_with("InstantSend BlockConnected: "), "{tripped:?}");
        assert!(tripped.message.contains("ended with its stages pending"), "{tripped:?}");
        let marks = sink.marks();
        assert_eq!(marks.len(), 1, "{marks:?}");
        assert!(matches!(&marks[0], AbortMark::AbortNode { log, .. } if *log == tripped.message));
        assert!(db.known(&locks[1].0).is_err(), "the hold's drop latched the store");
        drop((db, parts));
        let reopened = IsDb::open(engine).unwrap();
        assert_eq!(reopened.by_hash(&locks[0].0, false).unwrap(), None, "the committed stage");
        let second = reopened.by_hash(&locks[1].0, false).unwrap();
        assert_eq!(second, Some(locks[1].1.clone()), "the pending stage never landed");
    }

    /// A process kill keeps every committed lock (RocksDB's log is written at each commit), a
    /// reopen without the reindex flags keeps them, and `-reindex`'s wipe empties the store.
    #[test]
    fn llmq_isdb_keeps_committed_locks_across_a_kill_and_a_wipe_empties_it() {
        let dir = tempfile::tempdir().unwrap();
        let (hash, object) = lock(7);
        let store = rocks_opener(dir.path())(false).unwrap();
        store.write_new(&hash, &object).unwrap();
        store.write_mined(&hash, 5).unwrap();
        // No barrier: the store goes as a killed process's would.
        drop(store);
        assert!(dir.path().join(ISDB_DIR).join("CURRENT").is_file(), "under <datadir>/llmq/isdb");
        let store = rocks_opener(dir.path())(false).unwrap();
        assert_eq!(store.by_hash(&hash, false).unwrap(), Some(object));
        drop(store);
        let store = rocks_opener(dir.path())(true).unwrap();
        assert!(!store.known(&hash).unwrap(), "the wipe");
    }

    /// The listener's failures report once: the first trips the node latch and aborts the node,
    /// a later one finds the latch tripped and reports nothing more.
    ///
    /// The one report can name the listener's site for a failure that was V's. When V's
    /// `commit_staged` write fails, the IsDb latches and wakes the gate's waiters before V's finish
    /// trips the node latch. A listener call parked on the gate then wakes with the latched copy of
    /// V's engine error and can win the trip, as the first failure here does, so the report reads
    /// `InstantSend BlockConnected: <V's error>` and V's own `UnlessLatched` Fatal stays silent.
    /// There is still exactly one report.
    #[test]
    fn a_listener_store_failure_trips_the_latch_and_aborts_once() {
        let latch = GlobalFailStop::new();
        let sink = Arc::new(RecordingSink::default());
        let faults = NodeIsFaults {
            latch: latch.clone(),
            abort: Arc::clone(&sink) as _,
        };
        let error = StoreError::engine("test", ErrorKind::Io, "injected");
        faults.failed("InstantSend BlockConnected", &error);
        faults.failed("InstantSend HandleFullyConfirmedBlock", &error);
        let tripped = latch.tripped().expect("latched");
        assert_eq!((tripped.kind, tripped.site), (ErrorKind::Io, ISDB_SITE));
        assert!(tripped.message.starts_with("InstantSend BlockConnected: "), "{tripped:?}");
        let marks = sink.marks();
        assert_eq!(marks.len(), 1, "exactly one report, whichever site won: {marks:?}");
        assert!(
            matches!(&marks[0], AbortMark::AbortNode { log, user: None } if *log == tripped.message)
        );
    }
}
