//! InstantSend's node wiring (LK1b-s S1d, S4): the `llmq/isdb` store, the parts one load's
//! connector, evo applier and listener share, S2's manager, and the listener's reads of V's tree,
//! the ChainLocks and the sync state, with its one fault report.
//!
//! The store is its own database, outside the node latch ([`GuardedEngine`]), as Core's isdb is
//! its own `CDBWrapper` that no `AbortNode` closes (`instantsend/db.cpp:29-33`, `init.cpp:2033`):
//! its writes land whatever the chainstate's state, and the connector's removal batch commits
//! inline in `ConnectBlock` before anything after the IS loop can fail (PLAN-LK1b §2.4, §5.3).
//!
//! [`GuardedEngine`]: dashd_chainstate::store::GuardedEngine

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use dash_chain::{ChainReader, SporkView};
use dash_clock::Clock;
use dash_consensus::BlockId;
use dash_consensus::Tx;
use dash_evo::spork::SporkId;
use dash_kv::{ErrorKind, Failure, GlobalFailStop, KvEngine, StoreError};
use dash_num::Hash256;
use dash_params::ChainSetup;
use dash_primitives::BlockHash;
use dashd_chainstate::chain::{ChainViewHandle, IsConflicts, LoadingFlags, ValidationInterface};
use dashd_chainstate::connect::{ChainHelper, FxSource};
use dashd_chainstate::evo::effects::IsLockApplier;
use dashd_chainstate::store::AbortSink;
use dashd_locks::is::chain::{
    GateSource, IsAttemptLedger, IsHelper, IsLedgerApplier, SharedLedger,
};
use dashd_locks::is::conflicts::{IsBlockConflicts, IsConflictsParts};
use dashd_locks::is::db::{IsDb, SCHEMA};
use dashd_locks::is::fx::IsFxSource;
use dashd_locks::is::listener::{
    BlockTree, IsFaults, IsListener, IsListenerParts, IsListenerSigner,
};
use dashd_locks::is::manager::IsManager;
use dashd_locks::is::mempool::IsAcceptView;
use dashd_locks::is::process::ResolvingLocks;
use dashd_locks::is::seams::{ChainLockReads, IsManagerStore, IsSignerSeam, SyncView};
use dashd_locks::is::{IsGate, IsTipCache, LockedTx};
use dashd_masternode::sporks::SporkManager;
use dashd_masternode::sync::MasternodeSync;
use dashd_mempool::is::{IsAcceptHooks, IsMempool};
use dashd_store::{RocksConfig, RocksEngine};

use crate::signing::LateRecSigs;

/// The store's directory under the network's data directory.
pub const ISDB_DIR: &str = "llmq/isdb";

/// The site a listener's failed store call latches the node at.
const ISDB_SITE: &str = "llmq/isdb write";

/// Opens the IS store for a load. `wipe` (`-reindex` or `-reindex-chainstate`) empties it, as
/// Core's `CDBWrapper` wipe does.
pub type IsOpener = Box<dyn FnOnce(bool) -> Result<Arc<dyn IsManagerStore>, StoreError> + Send>;

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
    Box::new(move |wipe| Ok(Arc::new(IsDb::open_wiping(engine, wipe)?) as Arc<dyn IsManagerStore>))
}

/// What one load keeps for its listener and lock intake: the store, Core's
/// `IsInstantSendEnabled` inputs, the tip cache, S2's manager and the signing manager's slot.
/// Never the attempt ledger: only V's own objects hold that ([`IsVParts`]).
#[derive(Clone)]
pub struct IsParts {
    pub store: Arc<dyn IsManagerStore>,
    pub gate: GateSource,
    pub tip: IsTipCache,
    pub manager: Arc<IsManager>,
    /// The locks V is resolving, shared by the processor and V's conflict half (S4).
    pub resolving: Arc<ResolvingLocks>,
    /// The signing manager's truncation, bound at step 7c.
    pub recsigs: Arc<LateRecSigs>,
    /// `m_signer` for the processor and V's conflict half: set at step 7c on a masternode.
    pub signer: SignerSlot,
}

/// NetInstantSend's `m_signer` for the parts built before step 7c: Core's null `m_signer` until a
/// masternode's composition sets it, and again once its signers stop, so no clone of these parts
/// keeps a stopped signer.
#[derive(Clone, Default)]
pub struct SignerSlot(Arc<Mutex<Option<Arc<IsListenerSigner>>>>);

impl SignerSlot {
    /// Sets the signer; a second set is ignored.
    pub fn set(&self, signer: IsListenerSigner) {
        self.lock().get_or_insert_with(|| Arc::new(signer));
    }

    /// Back to the null `m_signer`.
    pub fn clear(&self) {
        let signer = self.lock().take();
        drop(signer);
    }

    fn lock(&self) -> MutexGuard<'_, Option<Arc<IsListenerSigner>>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn seam(&self) -> Option<Arc<dyn IsSignerSeam>> {
        Some(Arc::new(self.clone()))
    }

    fn get(&self) -> Option<Arc<IsListenerSigner>> {
        self.lock().clone()
    }

    #[cfg(test)]
    pub(crate) fn is_set(&self) -> bool {
        self.lock().is_some()
    }
}

impl IsSignerSeam for SignerSlot {
    fn process_txs(&self, txs: Vec<Arc<Tx>>, retroactive: bool) {
        if let Some(signer) = self.get() {
            signer.process_txs(txs, retroactive);
        }
    }

    fn retry(&self, txs: Vec<Arc<Tx>>) {
        if let Some(signer) = self.get() {
            signer.retry(txs);
        }
    }

    fn clear_inputs(&self, ids: Vec<[u8; 32]>) {
        if let Some(signer) = self.get() {
            signer.clear_inputs(ids);
        }
    }

    fn clear_lock(&self, lock: Arc<LockedTx>) {
        if let Some(signer) = self.get() {
            signer.clear_lock(lock);
        }
    }
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
    /// The parts over `store`, gated on `sporks`' SPORK_2 and V's `loading` flags, the manager's
    /// timings on `clock`.
    pub fn new(
        store: Arc<dyn IsManagerStore>,
        sporks: Arc<SporkManager>,
        loading: Arc<LoadingFlags>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let gate: GateSource = Arc::new(move || {
            IsGate::new(
                sporks.is_active(SporkId::SPORK_2_INSTANTSEND_ENABLED),
                loading.reindex(),
                loading.importing(),
            )
        });
        IsParts {
            manager: Arc::new(IsManager::new(Arc::clone(&store), Arc::clone(&gate), clock)),
            store,
            gate,
            tip: IsTipCache::new(),
            resolving: Arc::default(),
            recsigs: Arc::default(),
            signer: SignerSlot::default(),
        }
    }

    /// Installs V's InstantSend half of `ResolveBlockConflicts` in `slot`, over these parts,
    /// with a removal's store failure reported once through `latch` and `abort`.
    pub fn install_conflicts(
        &self,
        slot: &IsConflictsSlot,
        latch: GlobalFailStop,
        abort: Arc<dyn AbortSink>,
    ) {
        let is = IsBlockConflicts::new(IsConflictsParts {
            store: Arc::clone(&self.store) as _,
            manager: Arc::clone(&self.manager),
            tip: self.tip.clone(),
            signer: self.signer.seam(),
            resolving: Arc::clone(&self.resolving),
            faults: Arc::new(NodeIsFaults {
                latch,
                abort,
            }),
        });
        assert!(slot.0.set(Mutex::new(is)).is_ok(), "one install per load");
    }

    /// V's parts for one attach, over a fresh attempt ledger.
    pub fn for_v(&self) -> IsVParts {
        IsVParts {
            parts: self.clone(),
            ledger: IsAttemptLedger::shared(),
        }
    }

    /// NetInstantSend's listener over V's tree `reader`, with ChainLocks enabled by `sporks` and
    /// read from `chainlocks`, the sync state from `sync`, the pool's InstantSend surface from
    /// `mempool` and failures reported once through `latch` and `abort`, driving `signer` on a
    /// masternode.
    #[expect(clippy::too_many_arguments, reason = "the listener's node inputs")]
    pub fn listener(
        &self,
        params: &ChainSetup,
        reader: ChainReader,
        sporks: Arc<SporkManager>,
        chainlocks: Arc<dyn ChainLockReads>,
        sync: Arc<dyn SyncView>,
        mempool: Option<Arc<dyn IsMempool>>,
        latch: GlobalFailStop,
        abort: Arc<dyn AbortSink>,
        signer: Option<IsListenerSigner>,
    ) -> Arc<dyn ValidationInterface> {
        let listener = IsListener::new(
            params,
            IsListenerParts {
                store: Arc::clone(&self.store) as _,
                tip: self.tip.clone(),
                gate: Arc::clone(&self.gate),
                tree: Arc::new(ChainTree(reader)),
                chainlocks,
                chainlocks_enabled: Arc::new(move || {
                    sporks.is_active(SporkId::SPORK_19_CHAINLOCKS_ENABLED)
                }),
                recsigs: Arc::clone(&self.recsigs) as _,
                faults: Arc::new(NodeIsFaults {
                    latch,
                    abort,
                }),
                manager: Arc::clone(&self.manager),
                sync,
            },
        );
        let listener = match mempool {
            Some(mempool) => listener.with_mempool(mempool),
            None => listener,
        };
        Arc::new(match signer {
            Some(signer) => listener.with_signer(signer),
            None => listener,
        })
    }

    /// The pool's accept-time reads of these parts (`CChainstateHelper`'s InstantSend half): a
    /// store failure is reported once through `latch` and `abort`.
    // Core: v24.0.0 src/evo/chainhelper.cpp:79-91; src/validation.cpp:902-923
    pub fn accept_hooks(
        &self,
        latch: GlobalFailStop,
        abort: Arc<dyn AbortSink>,
    ) -> Arc<dyn IsAcceptHooks> {
        Arc::new(IsAcceptView {
            manager: Arc::clone(&self.manager),
            faults: Arc::new(NodeIsFaults {
                latch,
                abort,
            }),
        })
    }
}

impl IsVParts {
    /// The connector's helper: `inner` with the IS store's answers.
    pub fn helper<H: ChainHelper>(&self, inner: H) -> IsHelper<H> {
        IsHelper::live(
            inner,
            Arc::clone(&self.parts.store) as _,
            Arc::clone(&self.ledger),
            Arc::clone(&self.parts.gate),
            self.parts.tip.clone(),
        )
    }

    /// The evo hooks' lock applier, over the helper's store and ledger.
    pub fn applier(&self) -> Box<dyn IsLockApplier> {
        Box::new(IsLedgerApplier::new(Arc::clone(&self.parts.store) as _, Arc::clone(&self.ledger)))
    }

    /// A digest log's `fx` source over the attempt ledger.
    pub fn fx(&self) -> Box<dyn FxSource> {
        Box::new(IsFxSource(Arc::clone(&self.ledger)))
    }
}

/// V's [`IsConflicts`], which the attach installs ([`IsParts::install_conflicts`]) after V
/// starts: no lock is processed before that, and the empty slot answers as `NoInstantSend`.
#[derive(Clone, Default)]
pub struct IsConflictsSlot(Arc<OnceLock<Mutex<IsBlockConflicts>>>);

impl IsConflictsSlot {
    fn with<T>(&self, f: impl FnOnce(&mut IsBlockConflicts) -> T, none: T) -> T {
        match self.0.get() {
            Some(is) => f(&mut is.lock().unwrap_or_else(PoisonError::into_inner)),
            None => none,
        }
    }
}

impl IsConflicts for IsConflictsSlot {
    fn retrieve_conflicts(&mut self, lock: &Hash256) -> Vec<BlockHash> {
        self.with(|is| is.retrieve_conflicts(lock), Vec::new())
    }

    fn has_tx_for_lock(&mut self, lock: &Hash256) -> bool {
        self.with(|is| is.has_tx_for_lock(lock), false)
    }

    fn clear_conflicting(&mut self, lock: &Hash256, block: &BlockHash) {
        self.with(|is| is.clear_conflicting(lock, block), ());
    }

    fn remove_conflicting_lock(&mut self, lock: &Hash256) {
        self.with(|is| is.remove_conflicting_lock(lock), ());
    }
}

/// `m_mn_sync` as NetInstantSend reads it.
pub struct NodeSyncView(pub Arc<MasternodeSync>);

impl SyncView for NodeSyncView {
    fn is_blockchain_synced(&self) -> bool {
        self.0.is_blockchain_synced()
    }
}

/// A load without step 7's sync (unit tests): never synced, so nothing is tracked.
pub struct NeverSynced;

impl SyncView for NeverSynced {
    fn is_blockchain_synced(&self) -> bool {
        false
    }
}

/// `m_chainlocks.HasChainLock` over V's published ChainLock floor and SPORK_19, as CoinJoin's
/// chain view reads it.
pub struct NodeChainLocks {
    pub view: ChainViewHandle,
    pub sporks: Arc<SporkManager>,
}

impl ChainLockReads for NodeChainLocks {
    fn has_chain_lock(&self, height: i32, hash: &BlockHash) -> bool {
        let (enabled, floor) = (self.sporks.chainlocks_enabled(), self.view.view().snap.cl_floor);
        let ancestor = crate::coinjoin::adapters::floor_ancestor_hash(&self.view);
        crate::coinjoin::adapters::has_chain_lock(enabled, floor, height, hash, ancestor)
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

    struct NoChainLocks;

    impl ChainLockReads for NoChainLocks {
        fn has_chain_lock(&self, _: i32, _: &BlockHash) -> bool {
            false
        }
    }

    /// Before the attach installs it the slot answers as `NoInstantSend`; after, V's half reads
    /// the manager's graph for the lock being resolved.
    // Core: instantsend/instantsend.cpp:304-340
    #[test]
    fn the_conflicts_slot_answers_nothing_until_installed() {
        let p = crate::chain::tests::regtest();
        let clock = crate::chain::tests::clock_at(&p);
        let args = crate::init::test_args(&[]);
        let sporks = crate::sporks::spork_manager(&args, &p, Arc::clone(&clock)).expect("sporks");
        let db = Arc::new(IsDb::open(Arc::new(MemoryEngine::new(&SCHEMA))).unwrap());
        let parts = IsParts::new(db as _, sporks, Arc::default(), clock as _);
        let (hash, object) = lock(3);
        let mined = dashd_locks::is::manager::MinedBlock {
            id: BlockId(1),
            height: 1,
            hash: BlockHash::from([1; 32]),
        };
        parts.manager.add_non_locked_tx(&Arc::new(conflicting(3)), Some(mined));
        parts.manager.add_pending_is_lock(hash, Arc::new(object), 1, false);
        let lock = Hash256::from(hash);
        let mut slot = IsConflictsSlot::default();
        assert!(slot.retrieve_conflicts(&lock).is_empty());
        assert!(!slot.has_tx_for_lock(&lock));

        parts.install_conflicts(&slot, GlobalFailStop::new(), Arc::new(RecordingSink::default()));
        assert!(slot.retrieve_conflicts(&lock).is_empty(), "not being processed");
        assert!(!slot.has_tx_for_lock(&lock), "parked without its transaction");
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
        let parts = IsParts::new(
            Arc::clone(&db) as _,
            Arc::clone(&sporks),
            Arc::default(),
            Arc::clone(&clock) as _,
        );

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
        let listener = parts.listener(
            &p,
            tree.reader(),
            sporks,
            Arc::new(NoChainLocks),
            Arc::new(NeverSynced),
            None,
            latch.clone(),
            Arc::clone(&sink) as _,
            None,
        );
        let signals = thread::spawn(move || listener.block_connected(&genesis, id));
        let deadline = Instant::now() + Duration::from_secs(60);
        while db.gate_waiters() == 0 && !signals.is_finished() {
            assert!(Instant::now() < deadline, "the listener's call never reached the gate");
            thread::yield_now();
        }
        assert!(!signals.is_finished(), "the listener's call waits for V's stage");

        unwind.send(()).unwrap();
        assert!(v.join().is_err(), "V unwound");
        let cont = signals.join().expect("the listener returns");
        assert!(matches!(cont, dashd_chainstate::chain::Cont::Done));
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
