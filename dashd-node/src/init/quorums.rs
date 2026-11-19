//! Core's `CQuorumManager` as init builds it (`LLMQContext::qman`) for a node that is neither a
//! masternode nor watching quorums, and its `q-cache` thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use dash_params::ChainSetup;
use dashd_chainstate::chain::ChainHandle;
use dashd_chainstate::llmq::quorum::SharedQuorumHook;
use dashd_quorums::{
    ChainQuorums, MemKeyStore, NoDkgSessions, NotMasternode, PlatformQuorums, QuorumManager,
    WarmExecutor,
};

use crate::chainstate::QuorumThreadStart;
use crate::host::trace_thread;

/// rdashd's `CQuorumManager`: the node's contribution store and absent DKG and masternode seams.
/// Its type does not depend on the executor init passes it.
pub type NodeQuorumManager =
    QuorumManager<MemKeyStore, NoDkgSessions, NotMasternode, Arc<dyn WarmExecutor + Send + Sync>>;

/// What init keeps of the quorum manager: the manager, its Platform lookup, the chain seams
/// [`bind_chain`](Self::bind_chain) binds once V runs, and, from [`init_node_quorums`], its
/// `q-cache` thread. Dropping it interrupts and joins that thread, as `~CQuorumManager` does,
/// whatever other handles to the manager live.
pub struct NodeQuorums {
    pub manager: Arc<NodeQuorumManager>,
    pub platform: Arc<dyn PlatformQuorums + Send + Sync>,
    chain: Arc<ChainQuorums>,
    qcache: Option<Arc<QCacheThread>>,
}

impl NodeQuorums {
    /// Binds the manager's lookup, cleanup scan and share-seam view to V; before it each lookup
    /// is an error and each cleanup erases nothing. A second bind is ignored.
    pub fn bind_chain(&self, chain: &ChainHandle) {
        self.chain.bind(chain.clone());
    }

    #[cfg(test)]
    pub(crate) fn qcache(&self) -> &Arc<QCacheThread> {
        self.qcache.as_ref().expect("init_node_quorums made it")
    }
}

impl Drop for NodeQuorums {
    // Core: v24.0.0 src/init.cpp:462 node.llmq_ctx.reset(), src/llmq/quorumsman.cpp:50-56
    fn drop(&mut self) {
        if let Some(thread) = &self.qcache {
            thread.interrupt_and_join();
        }
    }
}

/// [`init_quorums`] with an unstarted [`QCacheThread`] as the executor, kept in the result:
/// the start goes into `start`, which the load runs after the BLS check threads start (Core's
/// `LLMQContext` builds `qman` after its block processor), and dropping the result joins it.
// Core: v24.0.0 src/llmq/context.cpp:19-22, src/llmq/quorumsman.cpp:45-46
pub fn init_node_quorums(
    chain: &Arc<ChainSetup>,
    hook: &mut Option<SharedQuorumHook>,
    start: &mut Option<QuorumThreadStart>,
) -> NodeQuorums {
    let thread = QCacheThread::new();
    let mut quorums = init_quorums(chain, thread.clone(), hook);
    let starter = Arc::clone(&thread);
    *start = Some(Box::new(move || starter.start()));
    quorums.qcache = Some(thread);
    quorums
}

/// Core's `LLMQContext::qman`, built inside `LoadChainstate` before the block index loads and
/// before VerifyDB, with Core's null `m_qdkgsman` and `m_handler`: each build reads the store and
/// never builds or writes contributions. The build hook goes into `hook`, which the coins' attach
/// installs on the members facade, so every route's builds (VerifyDB's included) run it.
///
/// `executor` is the `q-cache` thread's stand-in; the caller owns it, and nothing here stops it.
/// It must not run jobs on V's thread: a job that reads V through the chain seams would deadlock.
///
/// rdashd builds the manager just before the chainstate opens, where Core builds it after the
/// block tree database opens (node/chainstate.cpp:153-161), and starts its thread after the
/// open and the BLS check threads, so a load that fails before them starts no thread, as Core's
/// does not. No build queues a job in between: the members facade is attached later.
// Core: v24.0.0 src/node/chainstate.cpp:157-161, src/llmq/context.cpp:20-21,
// src/llmq/quorumsman.cpp:31-46 CQuorumManager::CQuorumManager
pub fn init_quorums(
    chain: &Arc<ChainSetup>,
    executor: Arc<dyn WarmExecutor + Send + Sync>,
    hook: &mut Option<SharedQuorumHook>,
) -> NodeQuorums {
    let seams = Arc::new(ChainQuorums::new());
    let manager = QuorumManager::new(
        Arc::clone(chain),
        MemKeyStore::new(),
        NoDkgSessions,
        NotMasternode,
        executor,
    )
    .with_quorum_lookup(seams.clone())
    .with_cleanup_facade(seams.clone());
    #[cfg(feature = "test-hooks")]
    let manager = manager.with_share_seam_view(seams.clone());
    let manager = Arc::new(manager);
    *hook = Some(manager.clone());
    NodeQuorums {
        platform: manager.clone(),
        manager,
        chain: seams,
        qcache: None,
    }
}

type Job = Box<dyn FnOnce() + Send>;

/// Core's `q-cache` thread: one `TraceThread` running the warming jobs in the order they come.
/// [`new`](Self::new) makes it unstarted, with jobs waiting in its queue;
/// [`start`](Self::start) spawns the thread; [`interrupt_and_join`](Self::interrupt_and_join) is
/// `~CQuorumManager`'s stop: queued jobs never run and the thread is joined. A running job is
/// not interrupted, where Core's warm loop checks the interrupt before each member
/// (quorumsman.cpp:654-660); no warm is queued until quorums have contributions (DK1). Dropping
/// the last handle stops it too.
// Core: v24.0.0 src/llmq/quorumsman.cpp:45-56, :632-668 CacheWarmingThreadMain
pub struct QCacheThread {
    state: Mutex<QCacheState>,
    interrupted: Arc<AtomicBool>,
    #[cfg(test)]
    fail_start: AtomicBool,
}

struct QCacheState {
    jobs: Option<Sender<Job>>,
    queue: Option<Receiver<Job>>,
    thread: Option<JoinHandle<()>>,
}

impl QCacheThread {
    /// An unstarted thread.
    pub fn new() -> Arc<Self> {
        let (jobs, queue) = channel();
        Arc::new(Self {
            state: Mutex::new(QCacheState {
                jobs: Some(jobs),
                queue: Some(queue),
                thread: None,
            }),
            interrupted: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            fail_start: AtomicBool::new(false),
        })
    }

    /// Spawns the thread; after a start or a stop it does nothing.
    pub fn start(&self) -> std::io::Result<()> {
        let mut state = self.lock();
        let Some(queue) = state.queue.take() else {
            return Ok(());
        };
        #[cfg(test)]
        if self.fail_start.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("injected q-cache spawn failure"));
        }
        let interrupted = Arc::clone(&self.interrupted);
        state.thread = Some(trace_thread("q-cache", move || {
            for job in queue {
                if interrupted.load(Ordering::Acquire) {
                    break;
                }
                job();
            }
        })?);
        Ok(())
    }

    /// Core's `m_cache_interrupt(); m_cache_thread.join();`. On `d-q-cache` itself, which a job
    /// dropping the last handle can be, it interrupts without joining.
    pub fn interrupt_and_join(&self) {
        self.interrupted.store(true, Ordering::Release);
        let (jobs, queue, thread) = {
            let mut state = self.lock();
            let on_thread = state
                .thread
                .as_ref()
                .is_some_and(|thread| thread.thread().id() == std::thread::current().id());
            (state.jobs.take(), state.queue.take(), state.thread.take().filter(|_| !on_thread))
        };
        drop((jobs, queue));
        if let Some(thread) = thread {
            let _ = thread.join();
        }
    }

    fn lock(&self) -> MutexGuard<'_, QCacheState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[cfg(test)]
    pub(crate) fn fail_next_start(&self) {
        self.fail_start.store(true, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn is_started(&self) -> bool {
        self.lock().thread.is_some()
    }
}

impl Drop for QCacheThread {
    fn drop(&mut self) {
        self.interrupt_and_join();
    }
}

impl WarmExecutor for QCacheThread {
    // Core: v24.0.0 src/llmq/quorumsman.cpp:670-676 QueueQuorumForWarming
    fn spawn_warm(&self, job: Box<dyn FnOnce() + Send>) {
        let jobs = self.lock().jobs.clone();
        if let Some(jobs) = jobs {
            let _ = jobs.send(job);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use std::time::Duration;

    use dash_params::LlmqType;
    use dash_primitives::QuorumHash;

    use super::*;

    /// Waits for a job on `d-q-cache` to start, and holds it there until the returned sender
    /// sends or drops; the job then sets the flag 100 ms later, so a join that returns before
    /// the flag is set did not wait for the job.
    fn hold(thread: &QCacheThread) -> (Sender<()>, Arc<AtomicBool>) {
        let (entered, started) = channel();
        let (release, gate) = channel::<()>();
        let finished = Arc::new(AtomicBool::new(false));
        let done = Arc::clone(&finished);
        thread.spawn_warm(Box::new(move || {
            entered.send(()).unwrap();
            let _ = gate.recv();
            std::thread::sleep(Duration::from_millis(100));
            done.store(true, SeqCst);
        }));
        started.recv_timeout(Duration::from_secs(30)).expect("the job runs");
        (release, finished)
    }

    /// Jobs wait until `start`, then run on `d-q-cache` in order; a second start does nothing.
    #[test]
    fn qcache_thread_runs_jobs_on_its_thread_once_started() {
        let thread = QCacheThread::new();
        let (tx, rx) = channel();
        for n in 0..3 {
            let tx = tx.clone();
            thread.spawn_warm(Box::new(move || {
                tx.send((n, std::thread::current().name().map(str::to_owned))).unwrap();
            }));
        }
        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err(), "nothing runs unstarted");
        assert!(!thread.is_started());
        thread.start().unwrap();
        thread.start().unwrap();
        for n in 0..3 {
            let got = rx.recv_timeout(Duration::from_secs(30)).unwrap();
            assert_eq!(got, (n, Some("d-q-cache".to_owned())));
        }
    }

    /// `~CQuorumManager`'s stop: the running job finishes, queued and later jobs never run, and
    /// the thread is joined while other handles live.
    #[test]
    fn interrupt_and_join_finishes_the_running_job_and_drops_the_queue() {
        let thread = QCacheThread::new();
        thread.start().unwrap();
        let ran = Arc::new(AtomicUsize::new(0));
        let (release, finished) = hold(&thread);
        let queued = Arc::clone(&ran);
        thread.spawn_warm(Box::new(move || {
            queued.fetch_add(1, SeqCst);
        }));
        let kept = Arc::clone(&thread);
        let stopper = std::thread::spawn(move || kept.interrupt_and_join());
        while !thread.interrupted.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        assert!(!stopper.is_finished(), "the join waits for the running job");
        drop(release);
        stopper.join().unwrap();
        assert!(finished.load(SeqCst), "the join waited for the running job");
        assert!(!thread.is_started(), "joined");
        let later = Arc::clone(&ran);
        thread.spawn_warm(Box::new(move || {
            later.fetch_add(1, SeqCst);
        }));
        thread.start().unwrap();
        assert_eq!(ran.load(SeqCst), 0);
        assert!(!thread.is_started(), "a stopped thread does not restart");
    }

    /// Dropping `NodeQuorums` joins its thread although the manager, which holds the executor,
    /// outlives it; a job dropping the last handle on `d-q-cache` does not join itself.
    #[test]
    fn dropping_node_quorums_joins_and_a_self_drop_does_not_deadlock() {
        let chain = crate::chain::tests::regtest();
        let (mut hook, mut start) = (None, None);
        let quorums = init_node_quorums(&chain, &mut hook, &mut start);
        assert!(!quorums.qcache().is_started(), "init_node_quorums starts nothing");
        start.take().expect("the start goes into the slot")().unwrap();
        let thread = Arc::clone(quorums.qcache());
        assert!(thread.is_started());
        let manager = Arc::clone(&quorums.manager);
        let (release, finished) = hold(&thread);
        let dropper = std::thread::spawn(move || drop(quorums));
        while !thread.interrupted.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        assert!(!dropper.is_finished(), "the drop waits for the running job");
        drop(release);
        dropper.join().unwrap();
        assert!(finished.load(SeqCst), "the drop waited for the running job");
        assert!(!thread.is_started(), "joined while the manager lives");
        drop((manager, hook, thread));

        let thread = QCacheThread::new();
        thread.start().unwrap();
        let (done, finished) = channel();
        let last = Mutex::new(Some(Arc::clone(&thread)));
        let (release, _) = hold(&thread);
        thread.spawn_warm(Box::new(move || {
            drop(last.lock().unwrap().take());
            done.send(()).unwrap();
        }));
        drop(thread);
        drop(release);
        finished.recv_timeout(Duration::from_secs(30)).expect("d-q-cache does not join itself");
    }

    /// An executor the test holds, counting its jobs.
    #[derive(Default)]
    struct Jobs(AtomicUsize);

    impl WarmExecutor for Jobs {
        fn spawn_warm(&self, job: Box<dyn FnOnce() + Send>) {
            self.0.fetch_add(1, SeqCst);
            job();
        }
    }

    /// The executor is `init_quorums`' argument (a test's here), kept by its caller; the hook
    /// goes into the slot; before the bind a Platform lookup is an error, not a panic.
    #[test]
    fn init_quorums_takes_the_callers_executor_and_fills_the_hook_slot() {
        let jobs = Arc::new(Jobs::default());
        let chain = crate::chain::tests::regtest();
        let mut hook = None;
        let quorums = init_quorums(&chain, jobs.clone(), &mut hook);
        let hook = hook.expect("the slot holds the manager's hook");
        let manager: SharedQuorumHook = quorums.manager.clone();
        assert!(Arc::ptr_eq(&hook, &manager), "the slot's hook is the kept manager");
        assert_eq!(Arc::strong_count(&jobs), 2, "the manager holds the caller's executor");
        let unbound = quorums.platform.quorum(LlmqType::LlmqTest, &QuorumHash::from([1; 32]));
        assert!(unbound.is_err(), "{unbound:?}");
        assert_eq!(jobs.0.load(SeqCst), 0);
    }

    /// Need (c): the caller owns the executor. Init's support and flag steps and the shutdown
    /// path never name the manager or its thread; node.rs never names the thread, and its
    /// shutdown touches the quorums on one line.
    #[test]
    fn no_stop_of_the_quorum_executor_exists_outside_its_caller() {
        for (name, text) in [
            ("init/support.rs", include_str!("support.rs")),
            ("init/flags.rs", include_str!("flags.rs")),
            ("shutdown.rs", include_str!("../shutdown.rs")),
        ] {
            for needle in [
                "QCacheThread",
                "NodeQuorums",
                "init_quorums",
                "init_node_quorums",
                "WarmExecutor",
                "quorums = None",
                "quorums.take()",
            ] {
                assert!(!text.contains(needle), "{name} names {needle}");
            }
        }
        let (node, _) = include_str!("../node.rs").split_once("\nmod tests {").expect("node tests");
        for needle in ["QCacheThread", "spawn_warm", "WarmExecutor"] {
            assert!(!node.contains(needle), "node.rs names {needle}");
        }
        let (_, shutdown) = node.split_once("pub fn shutdown(&mut self) {").expect("shutdown");
        let (shutdown, _) = shutdown.split_once("\n    }\n").expect("its end");
        let lines: Vec<_> =
            shutdown.lines().map(str::trim).filter(|l| l.contains("quorums")).collect();
        assert_eq!(lines, ["self.quorums = None;"]);
    }
}
