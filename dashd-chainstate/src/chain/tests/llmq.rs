//! CH7b's LLMQ seams over V's state (CH7 §4.2): `NodeLlmqSource`'s reads, and the members
//! facade's gate and attempt handle on a real V loop (EV addendum §2.6).

use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use dash_chain::{ChainReader, FlatFilePos};
use dash_consensus::BlockId;
use dash_consensus::ctx::{Deployment, DeploymentView};
use dash_consensus::primitives::Block;
use dash_dml::store::{LruListCache, MemEvoStore};
use dash_dml::testkit::MemChain;
use dash_dml::{MnList, StoreError};
use dash_evo::Effect;
use dash_evo::llmq::{LlmqSource, QuorumSnapshot};
use dash_primitives::{BlockHash, LlmqType};

use super::support::{Rig, RigConfig, SharedFiles, child, mine, regtest};
use crate::chain::seams::BlockFiles;
use crate::connect::AttemptId;
use crate::evo::effects::{
    EffectAppliers, MemPlatformBans, MnListSignal, StoreSnapshots, UnreachableIsLock,
    UnreachableMineable,
};
use crate::evo::node::{EvoBlocks, FileBlocks};
use crate::evo::testkit;
use crate::flush::EffectLedger;
use crate::llmq::facade::MembersFacade;
use crate::llmq::rows::RuntimeRows;
use crate::llmq::source::NodeLlmqSource;
use crate::store::GlobalFailStop;

/// DIP0003 active everywhere, or nowhere.
struct Dip3(bool);

impl DeploymentView for Dip3 {
    fn active_after(&self, _: Option<BlockId>, _: Deployment) -> bool {
        self.0
    }

    fn active_at(&self, _: BlockId, _: Deployment) -> bool {
        self.0
    }
}

/// Block files whose every read fails, as an I/O or decode error does.
struct FailingReads;

impl BlockFiles for FailingReads {
    fn save_block(
        &mut self,
        _: &Arc<Block>,
        _: u32,
        _: Option<FlatFilePos>,
    ) -> Result<Option<FlatFilePos>, String> {
        Err("read-only".into())
    }

    fn scan_already_pruned_files(&mut self, _: bool) -> Result<(), String> {
        Err("read-only".into())
    }

    fn read_block(&self, _: FlatFilePos, _: &BlockHash) -> Result<Arc<Block>, String> {
        Err("planted read fault".into())
    }

    fn open_block_file(&self, _: i32) -> bool {
        true
    }

    fn current_usage(&self) -> u64 {
        0
    }
}

/// The block reads of a pruned block: no data.
struct Pruned;

impl EvoBlocks for Pruned {
    fn read(&self, _: &ChainReader, _: BlockId) -> Option<Arc<Block>> {
        None
    }
}

/// One block, whatever is asked.
struct Held(Arc<Block>);

impl EvoBlocks for Held {
    fn read(&self, _: &ChainReader, _: BlockId) -> Option<Arc<Block>> {
        Some(self.0.clone())
    }
}

/// `f` over a source whose evo reads are an empty store over `chain`, with `dip3`.
fn with_source<R>(
    reader: &ChainReader,
    blocks: &dyn EvoBlocks,
    dip3: bool,
    f: impl FnOnce(&NodeLlmqSource<'_>, &MemChain) -> R,
) -> R {
    let chain = MemChain::new();
    let store = MemEvoStore::new();
    let view = store.view(&chain);
    let lists = LruListCache::default();
    let deployments = Dip3(dip3);
    let source = NodeLlmqSource {
        chain: &chain,
        deployments: &deployments,
        view: &view,
        lists: &lists,
        blocks,
        reader,
    };
    f(&source, &chain)
}

fn facts(source: &NodeLlmqSource<'_>, at: BlockId) -> String {
    format!("{:?}", source.coinbase_facts(at))
}

/// OD-CP-4: `GetNonNullCoinbaseChainlock`'s read. A block on disk gives its first transaction; a
/// pruned block and a failed read are both Core's `nullopt`, `BlockDataUnavailable`, never the
/// source's `None`; a block with no transactions is `NoTransactions`.
#[test]
fn coinbase_facts_are_cores_block_reads() {
    let files = SharedFiles::default();
    let rig = Rig::with(RigConfig {
        files: Some(Box::new(files.clone())),
        ..RigConfig::default()
    });
    let b1 = child(&rig.p, &rig.genesis, 1);
    rig.accept(&b1);
    let (hash, txid) = (b1.hash(), b1.block.txs()[0].txid());
    let empty = mine(&rig.p, b1.hash(), b1.time() + 1, Vec::new());
    let (send, got) = mpsc::channel();
    rig.on_v(move |v| {
        let id = v.reader.lookup(&hash).expect("indexed");
        let on_disk = FileBlocks(files);
        let failing = FileBlocks(FailingReads);
        let held = Held(empty);
        let mut out = Vec::new();
        for blocks in [&on_disk as &dyn EvoBlocks, &failing, &Pruned, &held] {
            out.push(with_source(&v.reader, blocks, true, |s, _| facts(s, id)));
        }
        send.send(out).expect("the test waits");
        Ok(())
    });
    assert_eq!(
        got.recv().expect("V ran it"),
        [
            format!("Ok(Some(First({txid})))"),
            "Ok(Some(BlockDataUnavailable))".to_string(),
            "Ok(Some(BlockDataUnavailable))".to_string(),
            "Ok(Some(NoTransactions))".to_string(),
        ]
    );
}

/// `GetListForBlock`: below DIP0003 Core's empty list; a DIP0003 block the store holds no record
/// for is an inconsistency, a fault, and never the source's `None` (EV addendum §2.1).
#[test]
fn a_list_is_resolved_or_a_fault() {
    let rig = Rig::new();
    let (send, got) = mpsc::channel();
    rig.on_v(move |v| {
        let pre = with_source(&v.reader, &Pruned, false, |s, chain| s.mn_list(chain.genesis()));
        let missing = with_source(&v.reader, &Pruned, true, |s, chain| s.mn_list(chain.genesis()));
        send.send((pre, missing)).expect("the test waits");
        Ok(())
    });
    let (pre, missing) = got.recv().expect("V ran it");
    let pre = pre.expect("Core's empty list").expect("a list");
    let shape = |l: &MnList| (*l.block_hash(), l.height(), l.total_registered(), l.len());
    assert_eq!(shape(&pre), shape(&MnList::default_pre_dip3()));
    assert!(matches!(missing, Err(StoreError::Inconsistent { .. })), "{missing:?}");
}

const T: LlmqType = LlmqType::LlmqTestDip0024;

/// How long a test waits for another thread to reach a point before it fails.
pub(super) const WAIT: Duration = Duration::from_secs(60);

fn hash(n: u8) -> BlockHash {
    BlockHash::from([n; 32])
}

fn row(mode: i32) -> QuorumSnapshot {
    QuorumSnapshot {
        active_quorum_members: vec![true, false],
        skip_list_mode: mode,
        skip_list: Vec::new(),
    }
}

/// A facade over a memory evo store's rows, with a latch of its own (not the rig's).
fn facade() -> (Arc<MembersFacade>, Arc<RuntimeRows>, GlobalFailStop) {
    let store = Arc::new(testkit::memory_store());
    let rows = Arc::new(RuntimeRows::new(store, Arc::new(EffectLedger::default())));
    let latch = GlobalFailStop::new();
    let facade = MembersFacade::new(&regtest(), rows.clone(), latch.clone()).expect("in domain");
    (Arc::new(facade), rows, latch)
}

/// The production appliers over `rows`, for logs of snapshot writes.
pub(super) fn appliers(rows: &Arc<RuntimeRows>) -> EffectAppliers {
    EffectAppliers {
        snapshot: Box::new(StoreSnapshots {
            rows: rows.clone(),
        }),
        is_lock: Box::new(UnreachableIsLock),
        mineable: Box::new(UnreachableMineable),
        platform_ban: Box::new(MemPlatformBans::default()),
        notify: Box::new(MnListSignal::default().applier()),
    }
}

pub(super) fn waiters(facade: &MembersFacade) -> usize {
    facade.hooks.gate_waiters.load(SeqCst)
}

pub(super) fn wait_until(what: &str, ready: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !ready() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        std::thread::yield_now();
    }
}

type Read = Result<Result<Option<QuorumSnapshot>, StoreError>, crate::chain::Fatal>;

/// Route A's read of `(T, hash(1))` on a thread of its own; `done` once it returns.
fn spawn_reader(
    facade: &Arc<MembersFacade>,
    done: &Arc<AtomicBool>,
) -> std::thread::JoinHandle<Read> {
    let (facade, done) = (facade.clone(), done.clone());
    std::thread::spawn(move || {
        let read = facade.external().map(|token| token.rows().snapshot_for_block(T, &hash(1)));
        done.store(true, SeqCst);
        read
    })
}

/// An attempt holds the gate exclusively until its log is applied and it is released: route A
/// waits on it, then reads the row the log wrote (routes C and D, then A).
#[test]
fn an_attempt_holds_the_gate_until_its_log_is_applied() {
    let (facade, rows, latch) = facade();
    rows.write(T, &hash(1), &row(0)).unwrap();
    let rig = Rig::new();
    let (send, got) = mpsc::channel();
    let f = facade.clone();
    rig.on_v(move |v| {
        let id = AttemptId::next();
        let handle = f.begin_attempt(&mut v.vcx, id);
        let done = Arc::new(AtomicBool::new(false));
        let reader = spawn_reader(&f, &done);
        wait_until("the reader waits on the gate", || waiters(&f) == 1);
        let log = [Effect::StoreQuorumSnapshot {
            llmq_type: T,
            cycle_base: hash(1),
            snapshot: row(1),
        }];
        let applied = appliers(&rows).apply_log(id, &log)?;
        let waited = waiters(&f) == 1 && !done.load(SeqCst);
        f.release_attempt(handle, applied)?;
        send.send((waited, reader.join().expect("the reader returns"))).expect("the test waits");
        Ok(())
    });
    let (waited, read) = got.recv().expect("V ran it");
    assert!(waited, "the reader read inside the attempt");
    assert_eq!(read.expect("no trip").expect("no read fault"), Some(row(1)));
    assert!(latch.tripped().is_none(), "{:?}", latch.tripped());
    assert!(!facade.gate().is_locked());
}

/// The gate prefers its writer: once V's attempt waits behind a reader, a new reader waits
/// behind the attempt, and gets in only after its release.
#[test]
fn a_waiting_attempt_holds_off_new_readers() {
    let (facade, rows, _) = facade();
    let rig = Rig::new();
    let (held_tx, held) = mpsc::channel();
    let (leave, leave_rx) = mpsc::channel::<()>();
    let first = {
        let f = facade.clone();
        std::thread::spawn(move || {
            let token = f.external().expect("no trip");
            held_tx.send(()).unwrap();
            leave_rx.recv().unwrap();
            drop(token);
        })
    };
    held.recv_timeout(WAIT).expect("the first reader holds the gate");
    let second_done = Arc::new(AtomicBool::new(false));
    let helper = {
        let (f, done) = (facade.clone(), second_done.clone());
        std::thread::spawn(move || {
            wait_until("V's attempt waits on the gate", || waiters(&f) == 1);
            let second = spawn_reader(&f, &done);
            wait_until("the second reader waits or reads", || {
                waiters(&f) == 2 || done.load(SeqCst)
            });
            leave.send(()).unwrap();
            second
        })
    };
    let (send, got) = mpsc::channel();
    let (f, done) = (facade.clone(), second_done.clone());
    rig.on_v(move |v| {
        let id = AttemptId::next();
        let handle = f.begin_attempt(&mut v.vcx, id);
        let ahead = !done.load(SeqCst) && waiters(&f) == 1;
        f.release_attempt(handle, appliers(&rows).apply_log(id, &[])?)?;
        send.send(ahead).expect("the test waits");
        Ok(())
    });
    assert!(got.recv().expect("V ran it"), "a new reader went ahead of the waiting attempt");
    first.join().unwrap();
    let second = helper.join().unwrap().join().unwrap();
    assert_eq!(second.expect("no trip").expect("no read fault"), None);
}

/// A handle dropped without its release, as an unwind drops it, trips the latch while it still
/// holds the gate (F3's sync point), then frees the gate: the reader waiting on it wakes to the
/// failure and reads nothing (M7, M8, M4).
#[test]
fn a_dropped_attempt_trips_the_latch_before_it_frees_the_gate() {
    let (facade, rows, latch) = facade();
    rows.write(T, &hash(1), &row(0)).unwrap();
    let rig = Rig::new();
    let (at_sync, sync) = mpsc::channel();
    let (go, go_rx) = mpsc::channel::<()>();
    let go_rx = Mutex::new(go_rx);
    *facade.hooks.after_trip.lock() = Some(Box::new(move || {
        at_sync.send(()).unwrap();
        go_rx.lock().unwrap().recv().unwrap();
    }));
    let done = Arc::new(AtomicBool::new(false));
    let observer = {
        let (f, done) = (facade.clone(), done.clone());
        std::thread::spawn(move || {
            sync.recv_timeout(WAIT).expect("the drop reached its sync point");
            let held = f.gate().is_locked_exclusive();
            if !held {
                // Unlocked before the trip: let the reader in before the trip lands.
                wait_until("the reader returns", || done.load(SeqCst));
            }
            go.send(()).unwrap();
            held
        })
    };
    let (send, got) = mpsc::channel();
    let f = facade.clone();
    rig.on_v(move |v| {
        let handle = f.begin_attempt(&mut v.vcx, AttemptId::next());
        let reader = spawn_reader(&f, &done);
        wait_until("the reader waits on the gate", || waiters(&f) == 1);
        drop(handle);
        send.send(reader.join().expect("the reader returns")).expect("the test waits");
        Ok(())
    });
    let read = got.recv().expect("V ran it");
    assert!(observer.join().unwrap(), "the gate was free at the trip's sync point");
    assert!(read.is_err(), "the woken reader read: {read:?}");
    assert_eq!(
        latch.tripped().map(|f| f.message.as_str()),
        Some("an EV attempt dropped without its release")
    );
    assert!(!facade.gate().is_locked());
    assert!(facade.external().is_err());
    assert_eq!(rows.peek(T, &hash(1)), Some(row(0)));
}

/// A release with another attempt's proof, or through another facade, is refused: it trips the
/// latch, and the gate is free after it (D75, M6).
#[test]
fn a_release_with_another_attempts_effects_is_refused() {
    let (facade, rows, latch) = facade();
    let (other, _, other_latch) = self::facade();
    let rig = Rig::new();
    let (send, got) = mpsc::channel();
    let (f, o) = (facade.clone(), other.clone());
    rig.on_v(move |v| {
        let mut appliers = appliers(&rows);
        let (a, b) = (AttemptId::next(), AttemptId::next());
        let handle = f.begin_attempt(&mut v.vcx, a);
        let wrong_id = f.release_attempt(handle, appliers.apply_log(b, &[])?);
        let handle = o.begin_attempt(&mut v.vcx, a);
        let wrong_facade = f.release_attempt(handle, appliers.apply_log(a, &[])?);
        send.send(((a.get(), b.get()), wrong_id, wrong_facade)).expect("the test waits");
        Ok(())
    });
    let ((a, b), wrong_id, wrong_facade) = got.recv().expect("V ran it");
    let message = format!("EV attempt {a} released with attempt {b}'s effects");
    assert_eq!(wrong_id.map_err(|e| e.0), Err(message.clone()));
    assert_eq!(latch.tripped().map(|f| f.message.clone()), Some(message));
    assert_eq!(
        wrong_facade.map_err(|e| e.0),
        Err("an EV attempt released through another facade".to_owned())
    );
    assert_eq!(
        other_latch.tripped().map(|f| f.message.as_str()),
        Some("an EV attempt dropped without its release")
    );
    assert!(!facade.gate().is_locked() && !other.gate().is_locked());
}
