//! Core's `NetQuorum` in the node (QM1b-7): the handler step 7c registers, its tip listener on
//! V's signals, and, with `-watchquorums` and no masternode key, the observer's `NetDKG`.
// Core: v24.0.0 src/init.cpp:2152-2172, :2201-2224

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use dash_clock::Clock;
use dash_evo::spork::SporkId;
use dash_evo::types::ProTxHash;
use dash_p2p::NetCore;
use dash_params::ChainSetup;
use dash_primitives::BlockHash;
use dashd_chainstate::chain::{AbortSink, ChainHandle};
use dashd_masternode::active::ActiveMasternode;
use dashd_masternode::active::worker::ListForBlock;
use dashd_masternode::sporks::SporkManager;
use dashd_masternode::sync::MasternodeSync;
use dashd_p2p::NetHandler;
use dashd_quorums::net_quorum::{NetQuorum, NetQuorumParts, QuorumPeers, RecoveryEnv};
use dashd_quorums::observer::ObserverNetDkg;
use dashd_quorums::source::{ChainQuorumData, NoEncryptedContributions};
use dashd_quorums::tip::{NetQuorumTip, QuorumConnectionsCheck, RecoverySteps, Tip, TipWorker};
use dashd_quorums::{RecoveryPool, Role, SyncMode, TipRef, default_worker_count};

use crate::chain::MnStartup;
use crate::init::quorums::{NodeQuorumManager, NodeQuorums, QCacheThread, WeakQuorumDb};

/// The node's `NetQuorum`.
pub type NodeNetQuorum =
    NetQuorum<ChainQuorumData, NoEncryptedContributions, WeakQuorumDb, Arc<QCacheThread>>;

/// What step 7c's quorum options and roles give `NetQuorum`.
pub struct QuorumOptions {
    /// `m_role`'s kind.
    pub role: Role,
    /// `quorums_recovery` (`-llmq-data-recovery`).
    pub recovery: bool,
    /// `sync_map` (`-llmq-qvvec-sync`).
    pub sync: Vec<(dash_params::LlmqType, SyncMode)>,
}

/// `GetListForBlock(block).GetValidMNsList()`'s proTxHashes, from V's lists.
struct NodeValidMns {
    v: ChainHandle,
    lists: dashd_masternode::active::worker::CheckedListForBlock<super::active_mn::NodeLists>,
}

impl dashd_quorums::ValidMnsAt for NodeValidMns {
    // Core: v24.0.0 src/llmq/net_quorum.cpp:691-695 GetQuorumRecoveryStartOffset
    fn valid_pro_tx_hashes(&self, block: &BlockHash) -> Option<Vec<ProTxHash>> {
        let id = self.v.view_handle().lookup(block)?;
        let list = self.lists.list_for_block(id).ok()?;
        Some(list.iter_valid().map(|mn| mn.pro_tx_hash).collect())
    }
}

/// `GetTime()` and `m_mn_sync.IsBlockchainSynced()` for the recovery jobs and the data request
/// cleanup. The time is the network core's clock, the one the message processor gives its
/// handlers (`HandlerCx::now_us`), so `qgetdata`'s registrations and their expiry read one
/// `GetTime()`.
struct NodeEnv {
    clock: Arc<dyn Clock>,
    sync: Arc<MasternodeSync>,
}

impl NodeEnv {
    fn new(clock: Arc<dyn Clock>, sync: &Arc<MasternodeSync>) -> Self {
        Self {
            clock,
            sync: Arc::clone(sync),
        }
    }
}

impl RecoveryEnv for NodeEnv {
    // Core: v24.0.0 src/util/time.cpp GetTime
    fn now(&self) -> i64 {
        self.clock.node_now_micros().div_euclid(1_000_000)
    }

    fn blockchain_synced(&self) -> bool {
        self.sync.is_blockchain_synced()
    }
}

/// The handler and what the node keeps of it.
pub struct QuorumNet {
    /// The handle the node keeps for `Start`, `Schedule` and the tip listener.
    pub net_quorum: NodeNetQuorum,
    /// The registry's handlers in Core's order: `NetQuorum`, then the observer's `NetDKG`.
    pub handlers: Vec<Box<dyn NetHandler>>,
    /// The handler's quorum reads, for the recovery scan: one claim set per node, as
    /// `fQuorumDataRecoveryThreadRunning` is one flag per object.
    pub source: ChainQuorumData,
    /// An active masternode's `NetDKG`, which step 7c gives its intake.
    #[cfg(feature = "dkg-runtime")]
    pub net_dkg: Option<Arc<super::net_dkg::NetDkgSlot>>,
}

/// Core's `NetQuorum` constructor at step 7c over the node's quorums: the quorum manager's reads
/// through route B, a handle on its database that does not keep it open ([`WeakQuorumDb`]), its
/// `q-cache` thread as the warm executor, its own `workerPool` of `DEFAULT_WORKER_COUNT` workers
/// (started only with a role), the active masternode's operator key, V's lists for the start
/// offset, and the network core. Before DK1's dkgdb the encrypted contributions are none
/// (ENCRYPTED_CONTRIBUTIONS_MISSING). An observer also gets `NetDKG`'s observer handler; with
/// `dkg-runtime` a masternode gets its `NetDKG` over the intake step 7c installs, and without it
/// keeps the stub.
// Core: v24.0.0 src/init.cpp:2201-2224
#[allow(clippy::too_many_arguments)]
pub fn quorum_net(
    params: &Arc<ChainSetup>,
    quorums: &NodeQuorums,
    chain: &ChainHandle,
    core: &Arc<NetCore>,
    sync: &Arc<MasternodeSync>,
    sporks: &Arc<SporkManager>,
    active: Option<Arc<ActiveMasternode>>,
    role: Role,
) -> QuorumNet {
    let source = ChainQuorumData::new(Arc::clone(params), Arc::clone(quorums.chain_quorums()));
    let net_quorum = NetQuorum::new(NetQuorumParts {
        role,
        source: source.clone(),
        contributions: NoEncryptedContributions,
        operator: active.map(|a| a as _),
        valid_mns: Arc::new(NodeValidMns {
            v: chain.clone(),
            lists: super::active_mn::node_lists(chain.clone()),
        }),
        store: quorums.store().downgrade(),
        warm: Arc::clone(quorums.qcache()),
        peers: Arc::clone(core) as Arc<dyn QuorumPeers>,
        env: Arc::new(NodeEnv::new(Arc::clone(core.clock()), sync)),
        pool: RecoveryPool::new(default_worker_count()),
    });
    let mut handlers: Vec<Box<dyn NetHandler>> = vec![Box::new(net_quorum.clone())];
    if role == Role::Observer {
        let sporks = Arc::clone(sporks);
        handlers.push(Box::new(ObserverNetDkg::new(Arc::new(move || {
            sporks.is_active(SporkId::SPORK_17_QUORUM_DKG_ENABLED)
        }))));
    }
    #[cfg(feature = "dkg-runtime")]
    let net_dkg = matches!(role, Role::Masternode { .. }).then(|| {
        let slot = Arc::new(super::net_dkg::NetDkgSlot::default());
        handlers.push(Box::new(super::net_dkg::MnNetDkg(Arc::clone(&slot))));
        slot
    });
    QuorumNet {
        net_quorum,
        handlers,
        source,
        #[cfg(feature = "dkg-runtime")]
        net_dkg,
    }
}

/// `NetQuorum`'s tip listener over `net_quorum`, on its `d-q-tip` worker, with `check` as its
/// `CheckQuorumConnections`: the only caller of the check, once per type per tip at Core's two
/// points. It reads the role at each event: a masternode's proTxHash is known once its `Init`
/// has run. On a masternode it ignores startup's first `InitializeCurrentBlockTip` until
/// [`ArmAfterInit`] arms it; elsewhere it is armed from the start, so the startup broadcast
/// reaches it once either way.
// Core: v24.0.0 src/init.cpp:2452-2466, :2538-2547; src/llmq/net_quorum.cpp:377-404
#[allow(clippy::too_many_arguments)]
pub fn spawn_tip_listener(
    params: &Arc<ChainSetup>,
    quorums: &NodeQuorums,
    net_quorum: &NodeNetQuorum,
    source: ChainQuorumData,
    chain: &ChainHandle,
    sync: &Arc<MasternodeSync>,
    active: Option<Arc<ActiveMasternode>>,
    options: &QuorumOptions,
    check: Arc<dyn QuorumConnectionsCheck>,
    abort: Arc<dyn AbortSink>,
) -> std::io::Result<(Arc<TipWorker>, JoinHandle<()>, Arc<AtomicBool>)> {
    let base = options.role;
    let role = Arc::new(move || match base {
        Role::Masternode {
            watching,
            ..
        } => Role::Masternode {
            pro_tx: active.as_ref().and_then(|a| a.pro_tx_hash()).map(ProTxHash::from),
            watching,
        },
        other => other,
    });
    let synced = {
        let sync = Arc::clone(sync);
        Arc::new(move || sync.is_blockchain_synced())
    };
    let llmqs: Vec<_> = params.consensus.llmq_params().map(|(t, p)| (t, *p)).collect();
    let steps = RecoverySteps {
        net: net_quorum.clone(),
        scan: source,
        cleanup: Arc::clone(&quorums.manager) as Arc<NodeQuorumManager>,
        enabled: options.recovery,
        sync: options.sync.clone(),
        llmqs: params
            .consensus
            .llmq_params()
            .map(|(t, p)| (t, usize::from(p.keep_old_connections)))
            .collect(),
    };
    let tip = NetQuorumTip::new(role, synced, llmqs, steps).with_connections_check(check);
    let reads = chain.view_handle();
    let resolve = Box::new(move |id| {
        reads.entry(id).map(|e| Tip {
            id,
            at: TipRef {
                height: i32::try_from(e.height).unwrap_or(i32::MAX),
                hash: e.hash,
            },
        })
    });
    let armed = Arc::new(AtomicBool::new(!matches!(base, Role::Masternode { .. })));
    let on_panic = Box::new(move |what: String| {
        abort.abort_node(&format!("a validation interface listener panicked: {what}"), None);
    });
    let (worker, handle) = TipWorker::spawn(tip, resolve, Arc::clone(&armed), on_panic)?;
    Ok((worker, handle, armed))
}

/// `NetQuorum::Schedule`: the expired data request cleanup every minute on the node's scheduler.
// Core: v24.0.0 src/init.cpp:2365, src/llmq/net_quorum.cpp:72-75
pub fn schedule(net_quorum: &NodeNetQuorum, scheduler: &crate::scheduler::Scheduler) {
    net_quorum.schedule(|f, every: Duration| scheduler.schedule_every(f, every));
}

/// The active masternode's startup `Init`, then the quorum tip listener hears the deferred
/// broadcast.
pub struct ArmAfterInit {
    pub inner: Box<dyn MnStartup>,
    pub armed: Arc<AtomicBool>,
}

impl MnStartup for ArmAfterInit {
    fn init(&self) {
        self.inner.init();
        self.armed.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use dash_clock::MockClock;
    use dashd_masternode::sync::NodeSyncNotifier;

    use super::*;
    use crate::p2p::MasternodeRole;

    struct Quiet;

    impl NodeSyncNotifier for Quiet {
        fn sync_reset(&self) {}
        fn sync_finished(&self) {}
    }

    /// Addendum 4: the recovery env reads the message processor's clock, mock time included,
    /// so a `qgetdata` registered at `cx.now_us` expires on the same `GetTime()`.
    #[test]
    fn the_recovery_env_reads_the_processors_mockable_clock() {
        let clock = Arc::new(MockClock::at(1_700_000_123));
        let sync = Arc::new(MasternodeSync::new(Arc::clone(&clock) as _, Box::new(Quiet)));
        let env = NodeEnv::new(Arc::clone(&clock) as _, &sync);
        assert_eq!(env.now(), 1_700_000_123);
        clock.set_mocktime(1_700_000_456);
        assert_eq!(env.now(), clock.node_now_micros() / 1_000_000);
        assert!(!env.blockchain_synced());

        let source = include_str!("quorum_net.rs");
        assert!(source.contains("env: Arc::new(NodeEnv::new(Arc::clone(core.clock()), sync)),"));
    }

    /// `m_role` per init.cpp:2156-2172: the active context (with `quorums_watch`), else the
    /// observer context with `-watchquorums`, else none.
    #[test]
    fn the_quorum_role_follows_the_contexts() {
        let none = MasternodeRole::default();
        assert_eq!(none.quorum_role(), Role::None);
        assert!(!none.observer());
        assert_eq!(none.watching(true).quorum_role(), Role::Observer);
        assert!(none.watching(true).observer());
        for watching in [false, true] {
            let mn = none.with_active(true).watching(watching);
            assert!(!mn.observer());
            assert_eq!(
                mn.quorum_role(),
                Role::Masternode {
                    pro_tx: None,
                    watching,
                }
            );
        }
    }
}
