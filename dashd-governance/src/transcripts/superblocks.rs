//! S4c: dashd v24.0.0's R6–R8 and R10 superblock recordings (S0, `PROVENANCE.md`) replayed as S3b's
//! R1–R4 are, with the blocks on a real V (GV1b plan §6 Replay): the node's connector over
//! `NodeEvo`, `NodeHelper` holding S4a's back end and the masternode sync state, the governance commands
//! on V through [`ChainExec`], and the hooks from V's own signals, S4b's governance listener
//! after the sync's. Each `submitblock` answer and block lookup must be dashd's, besides everything
//! S3b compares.
//!
//! The sim's chain follows V: each tip signal V sends reaches the sim's peers as Core's
//! `PeerManager::UpdatedBlockTip`, and a `headers` delivery reaches V too. The transaction index
//! takes each block V connects (`BlockConnected`), and `submitblock`'s answer is the RPC's:
//! `duplicate` from the index, else the `BlockChecked` state of the block.
//!
//! The node's mempool runs on V too: R10's `tx` deliveries are admitted to it, and V's block
//! connects remove what they mine.
//!
//! What the recordings hold that this rig does not run is named and counted in [`VCounts`].
//!
//! R9 runs on dashd-node's process instead ([`NodeHost`], [`r9_restart`]): its V, its governance
//! with the store, the load and the listener, across a clean restart.
// Core: rpc/mining.cpp v24.0.2:1007-1066; net_processing.cpp v24.0.2:2278-2313

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use dash_chain::{ChainConfig, HeaderBatch, HeaderChain, Services, StaticSporks, Status};
use dash_clock::Clock;
use dash_consensus::BlockId;
use dash_consensus::hash::sha256d;
use dash_consensus::header::HeaderParams;
use dash_consensus::primitives::{Block, Header, decode_tx_exact};
use dash_dml::ProTxHash;
use dash_governance::GovParams;
use dash_governance::strings::{
    Destination, decode_destination, hash_hex, hex_str, parse_hash_str,
};
use dash_kv::MemoryEngine;
use dash_num::Hash256;
use dash_p2p::NetCore;
use dash_p2p_core::ClSig;
use dash_primitives::{BlockHash, BlockHeader};
use dash_types::Numeric as _;
use dash_types::codec::BaseCodec as _;
use dashd_chainstate::chain::Fatal;
use dashd_chainstate::chain::{
    AbortPath, BlockState, Blocking, ChainHandle, ChainLockSig, ChainViewHandle, Cont, CoreChecks,
    GuardScope, LocalShutdown, NoFlush, NoInstantSend, NoUi, NodeBlockIndex, NodeJoinHandle,
    ReadAheadSetup, RecordingAbort, SignalsThread, VExit, VParts, ValidationInterface,
    ValidationSignals, load_genesis_block, spawn,
};
use dashd_chainstate::checkqueue::CheckQueue;
use dashd_chainstate::coins::{CoinsBudget, CoinsCache, Ratio, Salt};
use dashd_chainstate::connect::{ConnectorParts, CoreConnector, NodeHelper};
use dashd_chainstate::evo::RocksEvoStore;
use dashd_chainstate::evo::cl::AttemptClVerifier;
use dashd_chainstate::evo::effects::{
    EffectAppliers, FacadeMineable, MnListSignal, PlatformBanApplier, StoreSnapshots,
    UnreachableIsLock,
};
use dashd_chainstate::evo::ev1::DmlChecks;
use dashd_chainstate::evo::node::{
    FileBlocks, NodeEvo, NodeEvoParts, SharedEvoLayer, node_codec, node_net_info,
};
use dashd_chainstate::flush::EffectLedger;
use dashd_chainstate::llmq::facade::MembersFacade;
use dashd_chainstate::llmq::rows::RuntimeRows;
use dashd_chainstate::sigcache::{CacheSizes, ScriptCaches};
use dashd_chainstate::store::{
    CHAINSTATE_SCHEMA, GlobalFailStop, GuardedEngine, GuardedFiles, NodeBlockFiles,
};
use dashd_indexes::TxIndexReader;
use dashd_masternode::netfulfilled::NetFulfilledRequestManager;
use dashd_masternode::sync::{BlockRef, MasternodeSync};
use dashd_masternode::sync_net::CoreSyncNet;
use dashd_mempool::api::MempoolHandle as _;
use dashd_mempool::options::MemPoolOptions;
use dashd_mempool::types::MempoolAcceptResultType;
use dashd_mempool::{Mempool, PoolSink};
use dashd_p2p::processing::peerman::Shared as PeerShared;
use dashd_p2p::sim::Sim;
use dashd_p2p::sim::transcript::unhex;
use dashd_store::flatfile::{BlockFiles, BlockFilesConfig, BlockFilesState, OsFs};
use serde_json::{Value, json};

use super::{
    Blocks, ChainTip, CoreDiffs, DIR, Env, Exec, Node, Outcome, Replayer, Scratch, Script, Segment,
    Tweaks, block, fixture, play, run_on, script, sim_with, sync_net, tick_seam, wire,
};
use crate::chain::ChainExec;
use crate::exec::{GovCommand, GovExec};
use crate::handle::GovernanceHandle;
use crate::helper::GovernanceBackend;
use crate::net::NetGovernance;
use crate::rand::{Draw, ReplayRng};
use crate::sync::DmnTip;
use crate::test_support::FakeIndex;
use crate::test_support::NoReads;
use crate::tip::GovernanceListener;

/// Core's `MSG_BLOCK` and `MSG_SPORK` inventory types.
const MSG_BLOCK: u32 = 2;
const MSG_SPORK: u32 = 6;
/// `SPORK_19_CHAINLOCKS_ENABLED`.
const SPORK_19: i32 = 10018;

/// [`ChainExec`], counting the commands handed to V so the replay can wait for them.
#[derive(Clone, Default)]
pub(super) struct VExec {
    inner: ChainExec,
    posted: Arc<AtomicU64>,
    ran: Arc<AtomicU64>,
}

impl VExec {
    fn idle(&self) -> bool {
        self.ran.load(Ordering::SeqCst) == self.posted.load(Ordering::SeqCst)
    }
}

impl GovExec for VExec {
    fn post(&self, command: GovCommand) {
        self.posted.fetch_add(1, Ordering::SeqCst);
        let ran = Arc::clone(&self.ran);
        self.inner.post(Box::new(move |chain| {
            command(chain);
            ran.fetch_add(1, Ordering::SeqCst);
        }));
    }
}

/// The masternode sync's hooks, as dashd-node's `SyncListener` registers them before
/// governance's.
// Core: dsnotificationinterface.cpp v24.0.2:31-72
struct SyncHooks {
    sync: Arc<MasternodeSync>,
    view: ChainViewHandle,
}

impl SyncHooks {
    fn at(&self, id: BlockId) -> Option<BlockRef> {
        self.view.entry(id).map(|e| BlockRef {
            height: e.height,
            hash: *e.hash.as_bytes(),
        })
    }
}

impl ValidationInterface for SyncHooks {
    fn accepted_block_header(&self, id: BlockId) {
        if let Some(at) = self.at(id) {
            self.sync.accepted_block_header(at);
        }
    }

    fn notify_header_tip(&self, id: BlockId, ibd: bool) {
        self.sync.notify_header_tip(self.at(id), ibd);
    }

    fn initialize_current_block_tip(&self, tip: Option<BlockId>, ibd: bool) {
        if let Some(tip) = tip {
            self.updated_block_tip(tip, None, ibd);
        }
    }

    fn updated_block_tip(&self, new: BlockId, fork: Option<BlockId>, ibd: bool) {
        let (Some(at), false) = (self.at(new), fork == Some(new)) else {
            return;
        };
        let tip = self.view.snapshot().tip;
        let best = BlockRef {
            height: tip.best_header_height,
            hash: *tip.best_header_hash.as_bytes(),
        };
        self.sync.updated_block_tip(Some(best), at, ibd);
    }
}

/// What the replay reads off V's signals, registered last: each tip signal, for the sim's peers;
/// each block's `BlockChecked` state, `submitblock`'s catcher; and each connected block's
/// transactions, for the index.
struct Seen {
    tips: Mutex<Vec<(BlockId, Option<BlockId>, bool)>>,
    checked: Mutex<BTreeMap<BlockHash, BlockState>>,
    index: Arc<FakeIndex>,
}

impl ValidationInterface for Seen {
    fn updated_block_tip(&self, new: BlockId, fork: Option<BlockId>, ibd: bool) {
        self.tips.lock().unwrap_or_else(PoisonError::into_inner).push((new, fork, ibd));
    }

    fn block_checked(&self, block: &Block, state: &BlockState, _: bool) {
        let mut checked = self.checked.lock().unwrap_or_else(PoisonError::into_inner);
        checked.insert(block.hash(), state.clone());
    }

    fn block_connected(&self, block: &Arc<Block>, _: BlockId) -> Cont {
        for tx in block.txs() {
            self.index.put(tx, block.hash());
        }
        Cont::Done
    }
}

/// Where the evo effects reset a masternode's platform ban: the masternode meta store, which
/// nothing here reads.
struct NoBans;

impl PlatformBanApplier for NoBans {
    fn reset(&mut self, _: &ProTxHash, _: i32) -> Result<(), Fatal> {
        Ok(())
    }
}

/// The blocks V starts from, with dashd's answers to each one's `submitblock` and lookup.
pub(super) struct Boot {
    pub(super) blocks: Vec<Vec<u8>>,
    /// None for R10, which has no companion.
    answers: Option<Vec<Value>>,
}

impl Boot {
    /// `name`'s companion bootstrap: S0's frozen blocks, the ChainLock chain for R7's locked
    /// pair.
    fn of(name: &str) -> Boot {
        let gz = |file: &str| {
            let path = PathBuf::from(DIR).join(file);
            let gz = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let mut text = String::new();
            std::io::Read::read_to_string(&mut flate2::read::GzDecoder::new(&gz[..]), &mut text)
                .expect("gzip");
            serde_json::from_str::<Value>(&text).expect("JSON")
        };
        let source = gz("bootstrap.json.gz");
        let blocks = if name.starts_with("R7-") {
            &source["chainlock"]["blocks"]
        } else {
            &source["blocks"]
        };
        let blocks: Vec<Vec<u8>> = blocks
            .as_array()
            .expect("blocks")
            .iter()
            .map(|b| unhex(b.as_str().expect("block hex")))
            .collect();
        let answers = (name != "R10").then(|| {
            let answers = gz(&format!("{name}.bootstrap.json.gz"));
            let answers = answers.as_array().expect("answers").clone();
            assert_eq!(answers.len(), blocks.len(), "{name}: an answer per block");
            answers
        });
        Boot {
            blocks,
            answers,
        }
    }
}

/// What the replay counted beyond S3b's [`Outcome`]; the exceptions by the recording's rows.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct VCounts {
    /// `submitblock` answers equal to dashd's, the bootstrap's included.
    verdicts: usize,
    /// Block lookups (`getblock`) equal to dashd's, the bootstrap's included.
    lookups: usize,
    /// Recorder timing: an `invalidateblock`, `reconsiderblock` or `generate` whose block
    /// announcements dashd sent after the tool's next input, as no quiet wait follows an RPC.
    /// The node's announcements go out in that next segment.
    announced_after_next_input: Vec<usize>,
    /// dashd's sends of a handler this rig does not register, dropped from dashd's side: the
    /// spork's relay. Its effect on V, the spork's value, is applied as the recorded
    /// message arrives.
    other_handler_sends: Vec<usize>,
    /// `check rpc` observations of other subsystems, not compared: `spork show`,
    /// `quorum list`.
    not_governance_rpcs: usize,
    /// R10's bootstrap blocks, for which R10 holds no answers: each must connect, as dashd's
    /// chain at R10's first checkpoint is the bootstrap's.
    bootstrap_unanswered: usize,
    /// `protx info` fields not compared, [`PROTX_UNCOMPARED`], once per record.
    protx_uncompared: usize,
    /// Recorder timing: a generated block whose tip signal and the next block's fell in one of
    /// dashd's `SendMessages` passes, which announced only the later. The node's loop runs once
    /// for the two. The `generate`'s row, once per block.
    announced_with_the_next: Vec<usize>,
    /// `getblocktemplate` observations, not compared: the node answers none (its template
    /// source holds no governance payments). The governance reads of its template run. By row.
    templates_unanswered: Vec<usize>,
}

/// Whether dashd's `cmd` is a send of a handler this rig does not run.
pub(super) fn other_handler(cmd: &str, payload: &[u8]) -> bool {
    cmd == "inv" && dashd_p2p::sim::transcript::only_inv_type(payload, MSG_SPORK)
}

/// V, the rig's own or dashd-node's, and what the replay keeps of it.
pub(super) struct VChain {
    v: ChainHandle,
    host: Host,
    seen: Arc<Seen>,
    params: Arc<dash_params::ChainSetup>,
    /// Every block the recording or its bootstrap carries, by hash.
    blocks: BTreeMap<BlockHash, Arc<Block>>,
    /// The last `submitblock`'s answer.
    answered: Option<Value>,
    /// The blocks the last `generate` connected.
    generated: Vec<BlockHash>,
    /// The generated blocks dashd announced with the next ([`VCounts::announced_with_the_next`]).
    coalesced: BTreeSet<BlockHash>,
    counts: VCounts,
}

/// Whose V the replay drives.
enum Host {
    Own(Box<OwnV>),
    /// dashd-node's process, which a `restart` replaces.
    Node {
        host: Box<dyn NodeHost>,
        parts: Arc<HostParts>,
    },
}

/// The rig's V, with the node's stores in memory and block files on disk, and what only it runs:
/// the node's mempool and the static sporks.
struct OwnV {
    v: ChainHandle,
    join: Option<NodeJoinHandle<VExit>>,
    signals: ValidationSignals,
    _thread: SignalsThread,
    _dir: tempfile::TempDir,
    abort: Arc<RecordingAbort>,
    exec: VExec,
    sporks: Arc<StaticSporks>,
    /// The node's mempool on V, which R10's `tx` deliveries reach.
    pool: Arc<Mempool>,
}

impl OwnV {
    /// Waits until V's queued signals are dispatched and every governance command they posted
    /// has run.
    fn settle(&self) {
        self.signals.sync(&mut Blocking).expect("the signals thread");
        let deadline = Instant::now() + Duration::from_secs(60);
        while !self.exec.idle() {
            assert!(Instant::now() < deadline, "V never ran a governance command");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(self.abort.messages().is_empty(), "V aborted: {:?}", self.abort.messages());
    }

    /// Ends V, which must not have aborted.
    fn stop(&mut self) {
        self.v.shutdown();
        if let Some(join) = self.join.take() {
            drop(join.join());
        }
        self.pool.detach_chain().expect("the admission worker");
        assert!(self.abort.messages().is_empty(), "V aborted: {:?}", self.abort.messages());
    }
}

impl Drop for OwnV {
    fn drop(&mut self) {
        self.v.shutdown();
        if let Some(join) = self.join.take() {
            drop(join.join());
        }
        drop(self.pool.detach_chain());
    }
}

/// R9's node: dashd-node's process, whose V and governance the replay drives and whose clean
/// restart it asks for. dashd-node implements it over `NodeSut`; the rig keeps the sim's side.
pub trait NodeHost {
    /// Starts the first process on a fresh datadir with governance enabled over `parts`, as
    /// [`HostParts`] says, and returns it once its startup ABC and init callback ran.
    fn open(&mut self, parts: Arc<HostParts>) -> HostProcess;

    /// Stops the process cleanly (its governance store written) and starts the next on its
    /// files over the same parts, at the clock's time; returns it as [`Self::open`] does.
    fn restart(&mut self) -> HostProcess;

    /// Waits until V's queued signals are dispatched, every governance command posted before
    /// has run on V and its effects are applied. Panics if the process stopped or latched.
    fn settle(&self);

    /// Stops the process cleanly.
    fn stop(&mut self);
}

/// What each of the host's processes is built over: the sim's network and clock, the replay's
/// transaction index and listeners.
pub struct HostParts {
    /// The node's network core: `GetTime()` and the adjusted time governance reads.
    pub core: Arc<NetCore>,
    /// The processor's shared half, which the governance handler sends through.
    pub shared: Arc<PeerShared>,
    /// SP1's network over the core, for mnsync's notifier and the handler's relays.
    pub net: Arc<CoreSyncNet>,
    /// The sim's mock clock: mnsync's, and the node's mock time.
    pub clock: Arc<dyn Clock>,
    /// `g_txindex`, which the replay fills from V's connected blocks.
    pub index: Arc<dyn TxIndexReader>,
    /// mnsync's listener over a process's mnsync and V's view, registered before governance's.
    #[allow(clippy::type_complexity, reason = "a listener factory")]
    pub before: Box<
        dyn Fn(Arc<MasternodeSync>, ChainViewHandle) -> Arc<dyn ValidationInterface> + Send + Sync,
    >,
    /// The replay's listener, registered after governance's.
    pub after: Arc<dyn ValidationInterface>,
}

impl HostParts {
    /// A process's mnsync over the sim's clock, its notifier over `netfulfilled`, the process's
    /// fulfilled requests.
    pub fn mnsync(&self, netfulfilled: Arc<NetFulfilledRequestManager>) -> Arc<MasternodeSync> {
        let notifier = dashd_masternode::sync_manager::FullSyncNotifier {
            net: Arc::clone(&self.net) as _,
            netfulfilled,
        };
        Arc::new(MasternodeSync::new(Arc::clone(&self.clock), Box::new(notifier)))
    }
}

/// One of the host's processes, as the replay wires it.
pub struct HostProcess {
    pub v: ChainHandle,
    pub handle: Arc<GovernanceHandle<ChainExec>>,
    /// The process's mnsync, built by [`HostParts::mnsync`].
    pub sync: Arc<MasternodeSync>,
    /// The process's fulfilled requests, loaded from its datadir.
    pub fulfilled: Arc<NetFulfilledRequestManager>,
    /// Its `NetGovernance` over [`HostParts::shared`] and [`HostParts::net`].
    pub net: NetGovernance<ChainExec>,
}

impl VChain {
    /// V on `p`'s genesis, governance's listeners on its signals, and `boot`'s blocks submitted,
    /// each answer compared with dashd's.
    #[allow(clippy::too_many_arguments, reason = "the replay's shared parts")]
    pub(super) fn start(
        p: &Arc<dash_params::ChainSetup>,
        clock: Arc<dyn Clock>,
        handle: &Arc<GovernanceHandle<Exec>>,
        exec: VExec,
        sync: &Arc<MasternodeSync>,
        index: &Arc<FakeIndex>,
        boot: Boot,
        script: &Script,
    ) -> VChain {
        let pow = *HeaderParams::from_chain_params(p).expect("header rules").pow();
        let abort = Arc::new(RecordingAbort::default());
        let signals = ValidationSignals::default();
        let thread = signals.start(abort.clone()).expect("the signals thread");
        let latch = GlobalFailStop::new();
        let dir = tempfile::tempdir().expect("a temporary directory");
        let blocks_dir = dir.path().join("blocks");
        std::fs::DirBuilder::new().mode(0o700).create(&blocks_dir).expect("the blocks directory");
        let config = BlockFilesConfig::new(blocks_dir, p.message_start);
        let files = BlockFiles::with_fs(Arc::new(OsFs::new()), config, BlockFilesState::default());
        let files = Arc::new(GuardedFiles::new(files, latch.clone()));
        let path = || AbortPath::new(abort.clone(), latch.clone());
        let block_files = || NodeBlockFiles::new(files.clone(), pow, Arc::new(path()));
        let sporks = Arc::new(StaticSporks::for_network(p, Some(false)));
        let svc = Services {
            clock: clock.clone(),
            sporks: sporks.clone(),
            fail: Arc::new(NoReads(abort.clone())),
        };
        let checks = ChainConfig {
            consistency_checks: false,
            ..ChainConfig::node(p)
        };
        let (chain, idx): (HeaderChain, NodeBlockIndex) =
            load_genesis_block(p.clone(), checks, svc, &mut block_files()).expect("genesis");
        let engine = MemoryEngine::new(&CHAINSTATE_SCHEMA);
        let engine = Arc::new(GuardedEngine::new(engine, latch.clone(), abort.clone()));
        let budget = CoinsBudget::new(1 << 30, 0, Ratio::HALF, 0);
        let coins = CoinsCache::new(engine.clone(), budget, Salt::fixed(7), None);
        let net_info = || node_net_info(p, false).expect("regtest's NetInfo rules");
        let genesis = dashd_chainstate::chain::genesis_block(p).hash();
        let store = Arc::new(RocksEvoStore::new(engine, node_codec(net_info()), genesis));
        let layer = SharedEvoLayer::new(store.resume_layer().expect("a fresh meta row"));
        let rows = Arc::new(RuntimeRows::new(store.clone(), Arc::new(EffectLedger::default())));
        let facade = MembersFacade::new(p, Arc::clone(&rows), latch.clone()).expect("LLMQs");
        let facade = Arc::new(facade);
        let mn_list = MnListSignal::new(signals.clone());
        let appliers = EffectAppliers {
            snapshot: Box::new(StoreSnapshots {
                rows,
            }),
            is_lock: Box::new(UnreachableIsLock),
            mineable: Box::new(FacadeMineable(Arc::clone(&facade))),
            platform_ban: Box::new(NoBans),
            notify: Box::new(mn_list.applier()),
        };
        let hooks = NodeEvo::new(NodeEvoParts {
            store,
            layer,
            appliers,
            params: p.clone(),
            net_info: net_info(),
            ev1: DmlChecks,
            cl: Box::new(AttemptClVerifier::default()),
            blocks: Box::new(FileBlocks(block_files())),
            scheme: dash_consensus::ctx::bls_ctx_for_block(p, 1),
            mn_list,
            bls_checks: Arc::new(dash_evo::llmq::SerialBlsChecks),
        })
        .expect("regtest's payment parameters");
        let options = MemPoolOptions {
            check_ratio: 1,
            ..MemPoolOptions::default()
        };
        let pool = Arc::new(Mempool::with_signals(options, signals.clone()));
        pool.set_clock(clock.clone());
        let dmn: Arc<dyn DmnTip> = Arc::new(exec.inner.clone());
        let backend = GovernanceBackend::new(Arc::clone(handle), dmn);
        let synced = Arc::clone(sync);
        let helper = NodeHelper::with_governance(
            Some(Box::new(backend)),
            Some(Arc::new(move || synced.is_synced())),
        );
        let parts = ConnectorParts {
            coins: Arc::new(Mutex::new(coins)),
            files: files.clone(),
            queue: Arc::new(CheckQueue::serial()),
            caches: Arc::new(ScriptCaches::new(CacheSizes::default()).expect("caches")),
            helper: Box::new(helper),
            abort: path(),
            assume_valid: None,
            digest: None,
            xcheck: false,
        };
        let connector =
            CoreConnector::new(p, parts, hooks, Arc::clone(&facade)).expect("the connector");
        let (v, join) = spawn(VParts {
            params: p.clone(),
            chain,
            idx,
            checks: Box::new(CoreChecks::new(p.clone()).expect("regtest's checks")),
            files: Box::new(block_files()),
            read_ahead: ReadAheadSetup::off(),
            connector: Box::new(connector),
            facade: Some(facade),
            mempool: Box::new(PoolSink::new(Arc::clone(&pool), p.clone())),
            flush: Box::new(NoFlush),
            is: Box::new(NoInstantSend),
            ui: Arc::new(NoUi),
            abort: abort.clone(),
            shutdown: Arc::new(LocalShutdown::default()),
            signals: signals.clone(),
            clock,
            check_block_index: false,
            have_pruned: false,
            stop_at_height: 0,
            reindex: false,
            guard: GuardScope::of(p),
        })
        .expect("V starts");
        assert!(exec.inner.bind(v.clone()), "bound once");
        pool.attach_chain(v.clone()).expect("the admission worker");
        let view = v.view_handle();
        let seen = Arc::new(Seen {
            tips: Mutex::default(),
            checked: Mutex::default(),
            index: Arc::clone(index),
        });
        signals.register(Arc::new(SyncHooks {
            sync: Arc::clone(sync),
            view: view.clone(),
        }));
        signals.register(Arc::new(GovernanceListener::new(
            Arc::clone(handle),
            Arc::new(view.clone()),
            view,
        )));
        signals.register(seen.clone());
        v.activate_best_chain().recv().expect("V").expect("the startup ABC");
        let own = OwnV {
            v: v.clone(),
            join: Some(join),
            signals,
            _thread: thread,
            _dir: dir,
            abort,
            exec,
            sporks,
            pool,
        };
        VChain::booted(v, Host::Own(Box::new(own)), seen, p, boot, script)
    }

    /// dashd-node's first process in `host`, opened over `parts`, and `boot`'s blocks submitted
    /// to its V, each answer compared with dashd's; with the process.
    fn on_node(
        mut host: Box<dyn NodeHost>,
        parts: Arc<HostParts>,
        seen: Arc<Seen>,
        p: &Arc<dash_params::ChainSetup>,
        boot: Boot,
        script: &Script,
    ) -> (VChain, HostProcess) {
        let process = host.open(Arc::clone(&parts));
        let v = process.v.clone();
        let host = Host::Node {
            host,
            parts,
        };
        (VChain::booted(v, host, seen, p, boot, script), process)
    }

    /// `v` with `boot`'s blocks submitted, each answer compared with dashd's, and its signals
    /// settled.
    fn booted(
        v: ChainHandle,
        host: Host,
        seen: Arc<Seen>,
        p: &Arc<dash_params::ChainSetup>,
        boot: Boot,
        script: &Script,
    ) -> VChain {
        let mut chain = VChain {
            v,
            host,
            seen,
            params: p.clone(),
            blocks: BTreeMap::new(),
            answered: None,
            generated: Vec::new(),
            coalesced: script.coalesced.clone(),
            counts: VCounts::default(),
        };
        for line in script.text.lines() {
            if let Some(hex) = line.strip_prefix("vsubmitblock ") {
                chain.know(Arc::new(block(&unhex(hex))));
            }
        }
        for (i, raw) in boot.blocks.iter().enumerate() {
            let b = Arc::new(block(raw));
            chain.know(Arc::clone(&b));
            let got = chain.submit(&b);
            let Some(answer) = boot.answers.as_ref().map(|a| &a[i]) else {
                assert_eq!(got, Value::Null, "the bootstrap's {}", b.hash());
                chain.counts.bootstrap_unanswered += 1;
                continue;
            };
            assert_eq!(got, answer["submitblock"], "the bootstrap's {}", b.hash());
            chain.counts.verdicts += 1;
            chain.check_lookup(&b.hash(), &answer["getblock"]);
        }
        chain.settle();
        chain.seen.tips.lock().unwrap_or_else(PoisonError::into_inner).clear();
        chain
    }

    fn know(&mut self, b: Arc<Block>) {
        self.blocks.insert(b.hash(), b);
    }

    /// V's published view, the DMN-tip list governance reads.
    pub(super) fn view(&self) -> ChainViewHandle {
        self.v.view_handle()
    }

    /// V's tip and best header.
    pub(super) fn tip(&self) -> ChainTip {
        let tip = self.v.view().snap.tip;
        ChainTip {
            height: tip.tip_height,
            hash: tip.tip_hash,
            time: tip.tip_time,
            headers: tip.best_header_height,
        }
    }

    /// Waits until V's queued signals are dispatched and every governance command they posted
    /// has run.
    fn settle(&self) {
        match &self.host {
            Host::Own(own) => own.settle(),
            Host::Node {
                host,
                ..
            } => host.settle(),
        }
    }

    /// The rig's own V's parts.
    fn own(&self) -> &OwnV {
        match &self.host {
            Host::Own(own) => own,
            Host::Node {
                ..
            } => panic!("dashd-node's V: no mempool or static sporks of the rig's"),
        }
    }

    /// `submitblock`'s answer: `duplicate` for a block V holds as valid,
    /// `duplicate-invalid` for one it marked failed, else `ProcessNewBlock` and the block's
    /// `BlockChecked` state.
    // Core: rpc/mining.cpp v24.0.2:1007-1066, :531-548 BIP22ValidationResult
    fn submit(&self, b: &Arc<Block>) -> Value {
        let view = self.v.view_handle();
        if let Some(e) = view.lookup(&b.hash()).and_then(|id| view.entry(id)) {
            if e.status.is_valid(Status::VALID_SCRIPTS) {
                return "duplicate".into();
            }
            if e.status.has(Status::FAILED_MASK) {
                return "duplicate-invalid".into();
            }
        }
        self.seen.checked.lock().unwrap_or_else(PoisonError::into_inner).remove(&b.hash());
        let done = self.v.process_block(Arc::clone(b), true, None, true).recv().expect("V");
        if !done.new_block && done.accepted {
            return "duplicate".into();
        }
        let state =
            self.seen.checked.lock().unwrap_or_else(PoisonError::into_inner).remove(&b.hash());
        match state {
            None => "inconclusive".into(),
            Some(BlockState::Valid) => Value::Null,
            Some(BlockState::Invalid(e)) if e.reject_reason().is_empty() => "rejected".into(),
            Some(BlockState::Invalid(e)) => e.reject_reason().into(),
            Some(BlockState::Error(msg)) => panic!("{}: a system error: {msg}", b.hash()),
        }
    }

    /// `getblock`'s fields that V decides, for the block `hash` (displayed order).
    // Core: rpc/blockchain.cpp v24.0.2:136-166 blockheaderToJSON
    fn lookup(&self, hash: &BlockHash) -> Value {
        let handle = self.v.view_handle();
        let view = self.v.view();
        let id = handle.lookup(hash).expect("V holds the block");
        let e = handle.entry(id).expect("its entry");
        let tip = view.snap.tip;
        let confirmations = if handle.contains(&view, id) {
            i64::from(tip.tip_height) - i64::from(e.height) + 1
        } else {
            -1
        };
        // `HasChainLock(height, hash)` against `bestChainLockBlockIndex`.
        let chainlock = view.snap.cl_floor.is_some_and(|floor| {
            e.height <= floor.height && handle.ancestor(floor.id, e.height).is_some_and(|a| a == id)
        });
        let mut out = json!({
            "hash": hash.to_string(),
            "height": e.height,
            "confirmations": confirmations,
            "chainlock": chainlock,
        });
        if let Some(prev) = e.prev.and_then(|p| handle.entry(p)) {
            out["previousblockhash"] = prev.hash.to_string().into();
        }
        out
    }

    fn check_lookup(&mut self, hash: &BlockHash, want: &Value) {
        let got = self.lookup(hash);
        for (k, v) in got.as_object().expect("an object") {
            assert_eq!(want[k], *v, "getblock {hash}: {k}");
        }
        self.counts.lookups += 1;
    }

    /// `protx info` from V's DMN tip list, each field but [`PROTX_UNCOMPARED`]: what decides
    /// R10's vote invalidations (`votingAddress`, `pubKeyOperator`, the entry's presence) and
    /// the rest of the list's record.
    // Core: rpc/evo.cpp v24.0.2:63-86, :1593-1645; evo/core_write.cpp v24.0.2:150-193
    fn check_protx(&mut self, params: &[&str], want: &Value) {
        assert_eq!(params[0], "info", "protx {params:?}");
        let pro_tx = parse_hash_str(params[1].as_bytes()).expect("a proTxHash");
        let list = DmnTip::dmn_tip_list(&self.own().exec.inner);
        let dmn = list.get(&ProTxHash::from(pro_tx)).expect("the masternode is listed");
        let (cold, hot) = (&dmn.cold, &dmn.hot);
        assert!(!dmn.is_evo() && cold.version < 3, "a regular masternode before ExtAddr");
        let address = |key: &[u8; 20]| Destination::PubKeyHash(*key).encode(&self.params);
        // R10's one record follows the operator change, whose reset left `netInfo` empty.
        // Core: evo/dmnstate.h v24.0.2:127-135
        assert!(dash_dml::netinfo::is_empty(&cold.net_info), "protx info: a reset netInfo");
        let payout = want["state"]["payoutAddress"].as_str().expect("a payout address");
        let payout = decode_destination(payout.as_bytes(), &self.params).expect("an address");
        assert_eq!(payout.script(), cold.script_payout, "protx info: payoutAddress");
        let got = json!({
            "type": "Regular",
            "proTxHash": params[1],
            "collateralHash": hash_hex(dmn.collateral.txid().as_bytes()),
            "collateralIndex": dmn.collateral.index(),
            "operatorReward": f64::from(dmn.operator_reward) / 100.0,
            "state": {
                "version": cold.version,
                "registeredHeight": cold.registered_height,
                "lastPaidHeight": hot.last_paid_height,
                "consecutivePayments": hot.consecutive_payments,
                "PoSePenalty": hot.pose_penalty,
                "PoSeRevivedHeight": hot.pose_revived_height,
                "PoSeBanHeight": hot.banned_height(),
                "revocationReason": cold.revocation_reason,
                "ownerAddress": address(cold.key_id_owner.as_bytes()),
                "votingAddress": address(cold.key_id_voting.as_bytes()),
                "payoutAddress": payout.encode(&self.params),
                "pubKeyOperator": hex_str(&cold.pub_key_operator.to_bytes()),
                "addresses": {},
            },
        });
        let mut want = want.clone();
        for field in PROTX_UNCOMPARED {
            let (at, key) = match field.split_once('.') {
                Some((at, key)) => (&mut want[at], key),
                None => (&mut want, field),
            };
            assert!(at.as_object_mut().and_then(|o| o.remove(key)).is_some(), "{field}");
        }
        want["operatorReward"] = want["operatorReward"].as_f64().expect("a reward").into();
        assert_eq!(got, want, "protx info {}", params[1]);
        self.counts.protx_uncompared += PROTX_UNCOMPARED.len();
    }

    /// Each tip signal V sent since the last call, through the sim's `UpdatedBlockTip`: the
    /// sim's chain stores the blocks it lacks and moves its tip to V's. dashd's handler thread
    /// ran `SendMessages` between two inputs' signals, so the sim's loop runs after each.
    fn announce(&self, sim: &mut Sim) {
        let tips =
            std::mem::take(&mut *self.seen.tips.lock().unwrap_or_else(PoisonError::into_inner));
        let handle = self.v.view_handle();
        for (new, fork, ibd) in tips {
            let hash = |id: BlockId| handle.entry(id).expect("an entry").hash;
            let new = self.sim_store(sim, hash(new));
            let fork =
                fork.map(|f| sim.chain.by_hash(&hash(f)).expect("the sim holds the fork").id);
            sim.chain.set_tip(new);
            let hashes = sim.chain.new_blocks(new, fork);
            sim.shared.updated_block_tip(&sim.core, &hashes, ibd);
            if !hashes.first().is_some_and(|h| self.coalesced.contains(h)) {
                assert!((0..50).any(|_| !sim.pass()), "the loop settles after a tip signal");
            }
        }
    }

    /// The sim's entry for V's block `hash`, storing it and the ancestors the sim lacks.
    fn sim_store(&self, sim: &Sim, hash: BlockHash) -> BlockId {
        let mut path = Vec::new();
        let mut at = hash;
        while sim.chain.by_hash(&at).is_none() || path.is_empty() && at == hash {
            let b = &self.blocks[&at];
            path.push(Arc::clone(b));
            at = b.header().prev_hash();
            if sim.chain.by_hash(&at).is_some() {
                break;
            }
        }
        path.reverse();
        sim.chain.store_blocks(&path);
        sim.chain.by_hash(&hash).expect("stored").id
    }

    /// Ends V, which must not have aborted.
    fn stop(&mut self) {
        match &mut self.host {
            Host::Own(own) => own.stop(),
            Host::Node {
                host,
                ..
            } => host.stop(),
        }
    }
}

/// `protx info` fields R10's governance outcome does not read, by name: the collateral's
/// address and confirmations, which come from the coins view and the transaction index, not
/// the list (`collateralHash` and `collateralIndex` are compared); the masternode meta store's
/// `metaInfo`, which this rig does not run.
const PROTX_UNCOMPARED: [&str; 3] = ["collateralAddress", "confirmations", "metaInfo"];

fn hash(s: &str) -> BlockHash {
    s.parse().expect("a block hash in RPC order")
}

impl Replayer<'_> {
    /// The inputs V takes: a block (`vsubmitblock`, the replay's `submitblock`), `generate`'s
    /// blocks, `invalidateblock` and `reconsiderblock`.
    pub(super) fn v_input(&mut self, f: &[&str]) -> bool {
        let Blocks::V(v) = &mut self.node.blocks else {
            return false;
        };
        match f[0] {
            "vsubmitblock" => {
                let b = Arc::new(block(&unhex(f[1])));
                v.answered = Some(v.submit(&b));
            }
            // dashd mined these from its own template; their bytes come with their resubmission.
            "generate" => {
                v.generated = f[3..].iter().map(|h| hash(h)).collect();
                for h in &v.generated {
                    let b = Arc::clone(v.blocks.get(h).expect("the generated block's bytes"));
                    assert_eq!(v.submit(&b), Value::Null, "{}: generated {h}", self.name);
                }
            }
            "invalidateblock" => {
                v.v.invalidate_block(hash(f[1])).recv().expect("V").expect("a known block");
            }
            "reconsiderblock" => {
                let done = v.v.reconsider_block(hash(f[1]), false).recv().expect("V");
                done.expect("a known block");
            }
            _ => return false,
        }
        v.settle();
        true
    }

    /// `restart`: dashd-node's process stops cleanly and the next starts on its files at the
    /// recording's start time, as dashd restarted with its arguments; the next process's
    /// governance, mnsync, fulfilled requests, tick and jobs are wired as the first's were.
    pub(super) fn v_restart(&mut self, sim: &mut Sim) {
        let name = self.name;
        let start = self.script.text.lines().find_map(|l| l.strip_prefix("mocktime "));
        sim.clock.set_mock_time(start.expect("the start time").parse().expect("seconds"));
        self.id_base = self.scores.keys().max().map_or(0, |id| id + 1);
        let Blocks::V(v) = &mut self.node.blocks else {
            panic!("{name}: restart: the blocks are on V");
        };
        let Host::Node {
            host,
            parts,
        } = &mut v.host
        else {
            panic!("{name}: restart: dashd-node's process");
        };
        let process = host.restart();
        let net = Arc::clone(&parts.net);
        v.v = process.v.clone();
        v.settle();
        v.seen.tips.lock().unwrap_or_else(PoisonError::into_inner).clear();
        let tip: Arc<dyn DmnTip> = Arc::new(v.view());
        let n = &mut self.node;
        let (seam, held) = tick_seam(&process.handle, &process.sync, &tip, &n.tweaks, &n.params);
        let clock = Arc::clone(&sim.clock);
        let (tick, tick_net, sched) =
            wire(&n.late, process.net, seam, held, net, clock, &process.sync, process.fulfilled);
        (n.sync, n.handle, n.tip) = (process.sync, process.handle, tip);
        (n.tick, n.tick_net, n.sched) = (tick, tick_net, sched);
    }

    /// After `mockscheduler`, the maintenance the forward made due has posted its commands to V
    /// without waiting; they run before the next record, so that record reads their effects.
    pub(super) fn v_settle_posted(&self) {
        if let Blocks::V(v) = &self.node.blocks {
            v.settle();
        }
    }

    /// In each quiet wait: a delivered `headers`, `clsig` or `spork` reaches V as it reaches
    /// dashd's chainstate, then V's tip signals reach the sim's peers, unless dashd sent them
    /// after the next input.
    pub(super) fn v_settled(&mut self, sim: &mut Sim, seg: &Segment) {
        let Blocks::V(v) = &mut self.node.blocks else {
            return;
        };
        // dashd-node's process keeps its own mock time, the sim's from here on.
        if matches!(v.host, Host::Node { .. }) {
            v.settle();
        }
        let f: Vec<&str> = seg.input.split(' ').collect();
        if f[0] == ">" && matches!(f[2], "headers" | "clsig" | "spork" | "tx") {
            let payload = unhex(f[3]);
            match f[2] {
                // Core: net_processing.cpp v24.0.2:5340-5352
                "headers" => {
                    let mut r = &payload[..];
                    let n = dash_consensus::primitives::read_compact_size(&mut r, true)
                        .expect("a count");
                    let headers: Vec<Header> = (0..n)
                        .map(|_| {
                            let h = BlockHeader::decode(&mut r).expect("a header");
                            let _txs = dash_consensus::primitives::read_compact_size(&mut r, true);
                            Header::new(h)
                        })
                        .collect();
                    let batch = HeaderBatch {
                        hashes: headers.iter().map(Header::hash).collect(),
                        headers,
                    };
                    let done = v.v.process_headers(batch, None).recv().expect("V");
                    assert!(done.stop.is_none(), "{}: {:?}", self.name, done.stop);
                }
                // The ChainLock handler's intake, which verified this lock in dashd:
                // `ProcessNewChainLock`.
                // Core: chainlock/handler.cpp v24.0.2:138-188
                "clsig" => {
                    let message = ClSig::decode(&mut &payload[..]).expect("a clsig");
                    let sig = ChainLockSig::from_wire(&message, v.v.view().bls).expect("its sig");
                    let intake = v.v.cl_intake().recv().expect("V");
                    assert!(sig.height > intake.best_height, "a new lock");
                    let key = Hash256::from_lendian(sha256d(&payload));
                    let done = v.v.record_chainlock(Arc::new(sig), key, None).recv().expect("V");
                    assert!(done.relay, "{}: the lock is the best", self.name);
                }
                // The spork handler's `ProcessSpork`: SPORK_19 at 0, active from now on.
                "spork" => {
                    let id = i32::from_le_bytes(payload[..4].try_into().expect("an id"));
                    let value = i64::from_le_bytes(payload[4..12].try_into().expect("a value"));
                    assert_eq!((id, value), (SPORK_19, 0), "{}: the ChainLock spork", self.name);
                    v.own().sporks.set(true);
                }
                // `ProcessTransaction` for the peer's `tx`, which dashd admitted
                // (`getrawmempool`).
                // Core: net_processing.cpp v24.0.2:4879, :4959
                "tx" => {
                    let tx = Arc::new(decode_tx_exact(&payload).expect("a tx").into_value());
                    let from = f[1].parse().expect("a peer");
                    let got =
                        v.own().pool.accept_async(tx, Some(from)).recv().expect("the admission");
                    let valid = got.result_type == MempoolAcceptResultType::Valid;
                    assert!(valid, "{}: {got:?}", self.name);
                }
                _ => {}
            }
            v.settle();
        }
        if !seg.announce_later {
            v.announce(sim);
        }
    }

    /// The `check rpc` records V answers.
    pub(super) fn v_check_rpc(&mut self, method: &str, params: &[&str], json: &Value) -> bool {
        let Blocks::V(v) = &mut self.node.blocks else {
            return false;
        };
        let name = self.name;
        let result = || json.get("result").cloned().unwrap_or(Value::Null);
        match method {
            "submitblock" => {
                let got = v.answered.take().expect("the block was submitted");
                assert!(json.get("error").is_none(), "{name}: {json}");
                assert_eq!(
                    got,
                    result(),
                    "{name}: submitblock {}",
                    params[0].get(..160).unwrap_or(params[0])
                );
                v.counts.verdicts += 1;
            }
            "getblock" => v.check_lookup(&hash(params[0]), &json["result"]),
            "invalidateblock" | "reconsiderblock" => assert_eq!(result(), Value::Null),
            "generatetodescriptor" => {
                let want: Vec<BlockHash> = json["result"]
                    .as_array()
                    .expect("hashes")
                    .iter()
                    .map(|h| hash(h.as_str().expect("a hash")))
                    .collect();
                assert_eq!(std::mem::take(&mut v.generated), want, "{name}: generated");
                assert_eq!(v.v.view().snap.tip.tip_hash, *want.last().expect("one"), "{name}");
            }
            "getbestchainlock" => {
                let view = v.v.view();
                let best = view.best_chainlock().expect("a lock");
                let want = &json["result"];
                assert_eq!(best.block_hash.to_string(), want["blockhash"], "{name}: best lock");
                assert_eq!(i64::from(best.height), want["height"], "{name}: best lock height");
            }
            // Core: rpc/mempool.cpp v24.0.2:336-339
            "getrawmempool" => {
                let mut got: Vec<String> = v
                    .own()
                    .pool
                    .info_all()
                    .iter()
                    .map(|i| hash_hex(i.tx.txid().as_bytes()))
                    .collect();
                let mut want: Vec<String> = result()
                    .as_array()
                    .expect("txids")
                    .iter()
                    .map(|t| t.as_str().expect("a txid").to_owned())
                    .collect();
                got.sort();
                want.sort();
                assert_eq!(got, want, "{name}: getrawmempool");
            }
            "protx" => v.check_protx(params, &result()),
            "spork" | "quorum" => v.counts.not_governance_rpcs += 1,
            // The answer is excluded by name, counted by row in `v_script`; the governance reads
            // that build it run.
            "getblocktemplate" => {
                let h = v.tip().height + 1;
                Arc::clone(&self.node.handle).template(Arc::clone(&self.node.tip), h);
            }
            _ => return false,
        }
        true
    }
}

/// S0's script with the blocks for V: `submitblock` becomes `vsubmitblock`, which the sim's
/// replay leaves to the hooks; `generate` carries the hashes its `check rpc` names; and a chain
/// input whose block announcements dashd sent after the next input is marked.
fn v_script(name: &str) -> (Script, VCounts) {
    let raw = fixture(name);
    let mut s = script(&raw);
    let mut counts = VCounts::default();
    let lines: Vec<&str> = s.text.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        if let Some(hex) = line.strip_prefix("submitblock ") {
            out.push(format!("vsubmitblock {hex}"));
        } else if line.starts_with("generate ") {
            let rpc = lines[i + 1..]
                .iter()
                .find_map(|l| l.strip_prefix("check rpc "))
                .expect("generate's observation");
            let json: Value = serde_json::from_str(rpc).expect("JSON");
            assert_eq!(json["method"], "generatetodescriptor");
            let hashes: Vec<&str> = json["result"]
                .as_array()
                .expect("hashes")
                .iter()
                .filter_map(Value::as_str)
                .collect();
            out.push(format!("{line} {}", hashes.join(" ")));
        } else {
            out.push((*line).to_owned());
        }
    }
    // The rows counted are the recording's: each segment's input, and each dropped send.
    let mut rows: Vec<usize> = Vec::new();
    for (row, line) in (1..).zip(raw.lines()) {
        let f: Vec<&str> = line.split(' ').collect();
        match f[0] {
            "check" if line.starts_with("check rpc {\"method\":\"getblocktemplate\"") => {
                counts.templates_unanswered.push(row);
            }
            "" | "#" | "mocktime" | "ibd" | "check" | "log" | "rand" => {}
            "<" => {
                if other_handler(f[2], &unhex(f.get(3).copied().unwrap_or(""))) {
                    counts.other_handler_sends.push(row);
                }
            }
            _ => rows.push(row),
        }
    }
    assert_eq!(rows.len(), s.segments.len(), "{name}: the recording's inputs");
    let recorded: Vec<&str> = raw.lines().collect();
    for (row, seg) in rows.iter().zip(&s.segments) {
        assert_eq!(recorded[row - 1], seg.input, "{name}: row {row}'s input");
    }
    s.text = out.join("\n") + "\n";
    // Each segment's sends: whether a block `inv` opens them, how many, and the blocks
    // announced.
    let mut firsts: Vec<(bool, usize)> = Vec::new();
    let mut invs: Vec<BTreeSet<BlockHash>> = Vec::new();
    for line in s.text.lines() {
        let f: Vec<&str> = line.split(' ').collect();
        match f[0] {
            "" | "#" | "mocktime" | "ibd" | "check" | "log" => {}
            "<" => {
                let block =
                    f[2] == "inv" && unhex(f[3]).get(1..5) == Some(&MSG_BLOCK.to_le_bytes()[..]);
                let last = firsts.last_mut().expect("a segment");
                if last.1 == 0 {
                    last.0 = block;
                }
                last.1 += 1;
                if block {
                    let id: [u8; 32] = unhex(f[3])[5..37].try_into().expect("a hash");
                    invs.last_mut().expect("a segment").insert(BlockHash::from_lendian(id));
                }
            }
            _ => {
                if let Some(seg) = s.segments.get_mut(firsts.len()) {
                    line.clone_into(&mut seg.input);
                }
                firsts.push((false, 0));
                invs.push(BTreeSet::new());
            }
        }
    }
    assert_eq!(firsts.len(), s.segments.len(), "{name}: the segments");
    let chain_input = |seg: &Segment| {
        matches!(
            seg.input.split(' ').next(),
            Some("generate" | "invalidateblock" | "reconsiderblock")
        )
    };
    for i in 0..s.segments.len().saturating_sub(1) {
        if chain_input(&s.segments[i]) && firsts[i].1 == 0 && firsts[i + 1].0 {
            s.segments[i].announce_later = true;
            counts.announced_after_next_input.push(rows[i]);
        }
    }
    for (i, seg) in s.segments.iter().enumerate() {
        let f: Vec<&str> = seg.input.split(' ').collect();
        if f[0] != "generate" {
            continue;
        }
        let sent = &invs[i + usize::from(seg.announce_later)];
        let (last, before) = f[3..].split_last().expect("a generated block");
        assert!(sent.contains(&hash(last)), "{name}: `{}`: the tip announced", seg.input);
        for h in before.iter().map(|h| hash(h)).filter(|h| !sent.contains(h)) {
            s.coalesced.insert(h);
            counts.announced_with_the_next.push(rows[i]);
        }
    }
    (s, counts)
}

/// Replays `name` with its blocks on V: what S3b counts, the script's exceptions, and V's
/// answers.
fn run(name: &str) -> (Outcome, VCounts) {
    let (script, from_script) = v_script(name);
    let (outcome, blocks) = run_on(name, &script, Some(Boot::of(name)));
    (outcome, stopped(blocks, from_script))
}

/// V stopped, and what the replay counted on it besides the script's counts.
fn stopped(blocks: Blocks, from_script: VCounts) -> VCounts {
    let Blocks::V(mut v) = blocks else {
        unreachable!("V's replay");
    };
    v.stop();
    VCounts {
        verdicts: v.counts.verdicts,
        lookups: v.counts.lookups,
        bootstrap_unanswered: v.counts.bootstrap_unanswered,
        protx_uncompared: v.counts.protx_uncompared,
        not_governance_rpcs: v.counts.not_governance_rpcs,
        ..from_script
    }
}

/// Replays `name` against dashd-node's process in `host`, `boot`'s blocks submitted to its V.
fn run_node(name: &str, script: &Script, boot: Boot, host: Box<dyn NodeHost>) -> (Outcome, Blocks) {
    let p = super::chain_params();
    let gov_params = GovParams::from_chain_params(&p).expect("its parameters");
    let (sim, late) = sim_with(&p, &boot.blocks);
    let net = sync_net(&sim);
    let index = Arc::new(FakeIndex::default());
    let seen = Arc::new(Seen {
        tips: Mutex::default(),
        checked: Mutex::default(),
        index: Arc::clone(&index),
    });
    let parts = Arc::new(HostParts {
        core: Arc::clone(&sim.core),
        shared: Arc::clone(&sim.shared),
        net: Arc::clone(&net),
        clock: sim.clock.clone(),
        index,
        before: Box::new(|sync, view| {
            Arc::new(SyncHooks {
                sync,
                view,
            })
        }),
        after: seen.clone(),
    });
    let (v, process) = VChain::on_node(host, parts, seen, &p, boot, script);
    let tip: Arc<dyn DmnTip> = Arc::new(v.view());
    let masternodes = tip.dmn_tip_list().counts().enabled() as usize;
    let tweaks = Tweaks(Arc::new(Mutex::new(ReplayRng::new(
        script.tweaks.iter().map(|t| Draw::BloomTweak(*t)),
    ))));
    let (seam, held) = tick_seam(&process.handle, &process.sync, &tip, &tweaks, &gov_params);
    let clock = Arc::clone(&sim.clock);
    let (tick, tick_net, sched) =
        wire(&late, process.net, seam, held, net, clock.clone(), &process.sync, process.fulfilled);
    // The node's governance reads its own clock and offset (`NodeGovernance`'s environment).
    let env = Arc::new(Env {
        clock,
        sync: Arc::clone(&process.sync),
        offset: 0.into(),
    });
    let node = Node {
        params: gov_params,
        chain: Arc::clone(&sim.chain),
        sync: process.sync,
        env,
        handle: process.handle,
        late,
        tip,
        tick,
        tick_net,
        sched,
        tweaks,
        blocks: Blocks::V(Box::new(v)),
        _scratch: Scratch::new(name),
    };
    play(name, script, sim, node, masternodes)
}

/// R9's row 95: v24.0.0's `InitOnLoad` judging the loaded trigger at a cached height of 0
/// ([`CoreDiffs::pr8097_trigger_kept_at_load`]).
const R9_ROW_95: &str = "log \"2026-11-13T01:59:18Z (mocktime: 1700000000) [gobject] CSuperblock::IsExpired -- Trigger is too far into the future\"";

/// R9's script for V, with dashpay/dash#8097's rewrites
/// ([`CoreDiffs::pr8097_trigger_kept_at_load`]): the trigger the recording's start marked for
/// deletion (row 95's line, once) is expected unmarked in each later observation, and held where
/// dashd had erased it, as at its last observation.
// Core: governance/governance.cpp v24.0.2:100-112; governance/superblock.cpp v24.0.2:366-371
fn r9_script() -> (Script, VCounts) {
    let raw = fixture("R9");
    assert_eq!(raw.lines().nth(94), Some(R9_ROW_95), "R9: row 95");
    let far = raw.lines().filter(|l| l.contains("Trigger is too far into the future")).count();
    assert_eq!(far, 1, "R9: #8097's line, once");
    let (mut s, counts) = v_script("R9");
    let mut kept = 0;
    let mut restarted = false;
    let mut last: Option<Value> = None;
    let mut marked: Option<String> = None;
    let mut out: Vec<String> = Vec::new();
    for line in s.text.lines() {
        let (Some(rest), true) = (line.strip_prefix("check "), restarted) else {
            restarted |= line == "restart";
            if let Some(json) = line.strip_prefix("check gobject ") {
                last = Some(serde_json::from_str(json).expect("JSON"));
            }
            out.push(line.to_owned());
            continue;
        };
        let (kind, text) = rest.split_once(' ').expect("a check");
        let mut json: Value = serde_json::from_str(text).expect("a check's JSON");
        let before = last.as_ref().expect("a checkpoint before the restart");
        let hash = marked.get_or_insert_with(|| {
            let deleted = |j: &Value, h: &str| j["objects"][h]["fCachedDelete"] == true;
            let objects = json["objects"].as_object().expect("the first checkpoint after it");
            let marked: Vec<&String> =
                objects.keys().filter(|h| deleted(&json, h) && !deleted(before, h)).collect();
            assert_eq!(marked.len(), 1, "R9: the start marked one trigger");
            assert_eq!(json["objects"][marked[0]]["ObjectType"], 2, "R9: a trigger");
            marked[0].clone()
        });
        let entry = match (kind, &json["params"]) {
            ("gobject", _) => json["objects"].get_mut(hash.as_str()),
            ("rpc", params) if params[0] == "list" => json["result"].get_mut(hash.as_str()),
            ("rpc", params) if params[0] == "get" && params[1] == hash.as_str() => {
                Some(&mut json["result"])
            }
            _ => {
                out.push(line.to_owned());
                continue;
            }
        };
        match entry {
            Some(e) => {
                assert_eq!(e["fCachedDelete"], true, "R9: `{line:.60}`: marked");
                e["fCachedDelete"] = false.into();
            }
            None => {
                let at = before["objects"][hash.as_str()].clone();
                assert!(at.is_object(), "R9: `{line:.60}`: last held");
                json["objects"][hash.as_str()] = at;
                json["votes"][hash.as_str()] = before["votes"][hash.as_str()].clone();
            }
        }
        kept += 1;
        if kind == "gobject" {
            last = Some(json.clone());
            let label = json["label"].as_str().expect("a label");
            json["label"] = format!("{label} (dashpay/dash#8097: the trigger kept)").into();
        }
        out.push(format!("check {kind} {json}"));
    }
    s.text = out.join("\n") + "\n";
    s.counts.core.pr8097_trigger_kept_at_load = kept;
    (s, counts)
}

/// R9: governance across a clean restart of dashd-node's process (DEC-1927), on S0's v24.0.0
/// recording: the trigger and votes stored at the stop, loaded at the start through `LoadCache`,
/// `InitOnLoad` and the init callback (run, not observable: since dashpay/dash#8097 the load sets
/// the cached height to the tip), then the next superblock at 180 judged as dashd judged it. dashpay/dash#8097 is the one difference, counted; the three `getblocktemplate`
/// observations are not compared, and counted by row.
// Core: init.cpp v24.0.2:414, :2260-2267; governance/governance.cpp v24.0.2:91-112
pub fn r9_restart(host: Box<dyn NodeHost>) {
    let (script, from_script) = r9_script();
    let (outcome, blocks) = run_node("R9", &script, Boot::of("R9"), host);
    let got = (outcome, stopped(blocks, from_script));
    let want = Outcome {
        core: CoreDiffs {
            pr8097_trigger_kept_at_load: 5,
            ..CoreDiffs::default()
        },
        ..outcome_r9()
    };
    let counts = VCounts {
        templates_unanswered: vec![78, 103, 122],
        ..VCounts::default()
    };
    assert_counts("R9", got, want, counts);
}

/// R9's checkpoints and recorder timing.
fn outcome_r9() -> Outcome {
    outcome(5, 1, 2)
}

/// The block answers a recording and its bootstrap hold: `submitblock`s and `getblock`s.
fn answers(name: &str) -> (usize, usize) {
    let boot = Boot::of(name);
    let boot = if boot.answers.is_some() {
        boot.blocks.len()
    } else {
        0
    };
    let text = fixture(name);
    let count = |m: &str| text.lines().filter(|l| l.starts_with(m)).count();
    (
        boot + count("check rpc {\"method\":\"submitblock\""),
        boot + count("check rpc {\"method\":\"getblock\""),
    )
}

fn assert_counts(name: &str, got: (Outcome, VCounts), want: Outcome, mut counts: VCounts) {
    let (verdicts, lookups) = answers(name);
    counts.verdicts = verdicts;
    counts.lookups = lookups;
    assert_eq!(got, (want, counts), "{name}");
}

/// S3b's recorder timing ([`super::Timing`]): `tick` advances whose sync tick fired after the
/// scheduler's, and `bursts` runs of deliveries in one relay flush.
fn outcome(checkpoints: usize, tick: usize, bursts: usize) -> Outcome {
    Outcome {
        timing: super::Timing {
            tick_after_advance_time: tick,
            delivery_bursts: bursts,
        },
        checkpoints,
        ..Outcome::default()
    }
}

/// R6: a funded trigger at 180; dashd's verdicts: the over-budget block and the wrong payee
/// `bad-cb-amount`, the right payees accepted.
// Core: masternode/payments.cpp v24.0.2:313-442; governance/superblock.cpp v24.0.2:578-620
#[test]
fn r6_superblock_verdicts() {
    assert_counts("R6", run("R6"), outcome(5, 1, 2), VCounts::default());
}

/// R6-rank: two triggers at 180, the best by absolute yes count: the block paying the other is
/// `bad-cb-amount`, the best's accepted.
// Core: governance/superblock.cpp v24.0.2:556-576 GetBestSuperblockInternal
#[test]
fn r6_best_superblock_by_absolute_yes() {
    assert_counts("R6-rank", run("R6-rank"), outcome(2, 1, 0), VCounts::default());
}

/// R6-tie: one more yes makes the two triggers tie; the first in the triggers map's order,
/// `uint256`'s byte-wise comparison, wins.
// Core: governance/superblock.cpp v24.0.2:565-573; uint256.h v24.0.2:54 Compare
#[test]
fn r6_tie_goes_to_the_lower_hash() {
    assert_eq!(v_script("R6-tie").0.triggers.len(), 2);
    assert_counts("R6-tie", run("R6-tie"), outcome(2, 1, 0), VCounts::default());
}

/// R7: the same blocks to a node whose masternode sync restarted: only the bounds are checked,
/// so the over-budget block fails and the wrong payee connects; then that block is invalidated
/// and the right payees connect.
// Core: evo/chainhelper.cpp v24.0.2:41-52; masternode/payments.cpp v24.0.2:313-390
#[test]
fn r7_unsynced_max_bounds() {
    let counts = VCounts {
        announced_after_next_input: vec![93],
        ..VCounts::default()
    };
    assert_counts("R7", run("R7"), outcome(6, 0, 2), counts);
}

/// R7-chainlocked: a fully synced node holding the ChainLock of a block paying the wrong
/// superblock accepts it without the superblock check.
// Core: evo/chainhelper.cpp v24.0.2:43-46
#[test]
fn r7_chainlocked_block_skips_the_superblock_check() {
    let counts = VCounts {
        other_handler_sends: vec![23],
        not_governance_rpcs: 2,
        ..VCounts::default()
    };
    assert_counts("R7-chainlocked", run("R7-chainlocked"), outcome(3, 0, 1), counts);
}

/// R7-control: the same block without the lock is `bad-cb-amount`.
#[test]
fn r7_control_without_the_lock() {
    let counts = VCounts {
        other_handler_sends: vec![23],
        not_governance_rpcs: 2,
        ..VCounts::default()
    };
    assert_counts("R7-control", run("R7-control"), outcome(2, 0, 1), counts);
}

/// R8: the superblock executes; `invalidateblock` with nothing to replace it (a null fork) moves
/// the cached height down; a longer alternate branch, `reconsiderblock` and an invalidation
/// bring the superblock back; the shared regtest expiry boundary; the cycle expiry.
// Core: validation.cpp v24.0.2:4049-4051, :4154-4156; governance/superblock.cpp v24.0.2:339-375
#[test]
fn r8_superblock_reorgs() {
    let counts = VCounts {
        announced_after_next_input: vec![84, 92, 106, 114, 201],
        announced_with_the_next: vec![114],
        ..VCounts::default()
    };
    assert_counts("R8", run("R8"), outcome(8, 1, 2), counts);
}

/// R8-future: a deep `invalidateblock` below the trigger's creation, a new branch, and the
/// far-future rule at the next clean.
// Core: governance/superblock.cpp v24.0.2:366-371
#[test]
fn r8_far_future_trigger_after_a_deep_reorg() {
    let counts = VCounts {
        announced_after_next_input: vec![76, 84, 94],
        ..VCounts::default()
    };
    assert_counts("R8-future", run("R8-future"), outcome(4, 1, 2), counts);
}

/// R10: a voting-key change, an operator-key change (which also bans) and a masternode removal,
/// each a transaction dashd admitted from the peer and then mined; the tip hook's
/// `RemoveInvalidVotes` drops the votes each one invalidates. The two key changes show at the next
/// checkpoint; the removal shows only at row 183's endorsement flag, because at row 135 the vote
/// reads already skip a masternode missing from the tip list. Then the next superblock at 200.
// Core: governance/governance.cpp v24.0.2:578-588, :1139-1141, :1184-1233; governance/object.cpp
// v24.0.2:880-895; evo/specialtxman.cpp v24.0.2:556-563, :745-757
#[test]
fn r10_key_changes() {
    let counts = VCounts {
        announced_after_next_input: vec![108, 118, 129, 137],
        bootstrap_unanswered: 179,
        protx_uncompared: 3,
        ..VCounts::default()
    };
    assert_counts("R10", run("R10"), outcome(6, 0, 2), counts);
}
