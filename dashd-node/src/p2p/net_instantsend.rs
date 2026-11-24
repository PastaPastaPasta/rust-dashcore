//! LK1b-IN: `NetInstantSend` in the node, the first of Core's `m_handlers`. Step 7c builds it
//! over the load's InstantSend parts ([`IsNetParts`]) once the processor's shared half exists,
//! and registers its `isdlock` handler before `NetQuorum` and `NetDKG`; step 10a starts its
//! `isman` worker, which `InterruptHandlers` interrupts and `StopHandlers` joins.
// Core: v24.0.0 src/init.cpp:2198, :2360, :291-293, :337; src/net_processing.cpp:1690-1729;
// src/instantsend/net_instantsend.cpp:238-301, :425-445

use std::fmt;
use std::sync::Arc;
use std::thread::JoinHandle;

use dash_chain::ChainReader;
use dash_consensus::Tx;
use dash_consensus::primitives::TxHash;
use dash_evo::locks::{QuorumScanView, ScanLookup, isdlock_hash};
use dash_num::Hash256;
use dash_p2p::CommandString;
use dash_p2p::policy::Throw;
use dash_p2p_core::{InvType, Inventory, IsdLock};
use dash_params::{ChainSetup, LlmqType};
use dash_primitives::{BlockHash, QuorumHash};
use dash_types::Numeric as _;
use dash_types::codec::BaseCodec as _;
use dashd_chainstate::chain::ChainHandle;
use dashd_chainstate::store::AbortSink;
use dashd_locks::is::IsLockHash;
use dashd_locks::is::batch::{IsPassView, PassBody};
use dashd_locks::is::manager::IsManager;
use dashd_locks::is::manager::NodeId;
use dashd_locks::is::net::{IsNetError, NetInstantSend};
use dashd_locks::is::process::{IsProcessor, IsProcessorParts, NoNotify, VChain};
use dashd_locks::is::seams::{ChainLockReads, IsPeers, TxLookup};
use dashd_locks::is::verify::{NodeQuorumView, NodeViewError};
use dashd_locks::is::worker::ThreadInterrupt;
use dashd_mempool::is::IsMempool;
use dashd_p2p::processing::registry::CoreHandler;
use dashd_p2p::processing::relay::Related;
use dashd_p2p::processing::seams::IsLockLookup;
use dashd_p2p::{HandlerCx, NetHandler, Shared};

use crate::host::trace_thread;
use crate::instantsend::IsParts;
use crate::signing::shares::StrongRng;

const COMMANDS: [CommandString; 1] = [CommandString::ISDLOCK];

fn inv(hash: &IsLockHash) -> Inventory {
    Inventory::new(InvType::IsdLock, Hash256::from_lendian(*hash))
}

/// What step 7c builds `NetInstantSend` from, besides the processor's shared half.
pub struct IsNetParts {
    pub is: IsParts,
    pub params: Arc<ChainSetup>,
    pub chain: ChainHandle,
    pub chainlocks: Arc<dyn ChainLockReads>,
    pub txs: Arc<dyn TxLookup>,
    /// NetInstantSend's `m_mempool`.
    pub mempool: Option<Arc<dyn IsMempool>>,
    /// `active_ctx`: whether NetInstantSend has an `m_signer`.
    pub masternode: bool,
}

impl IsNetParts {
    /// `NetInstantSend` over these parts, reaching peers through `shared`, its handler and its
    /// worker, not started. Its lock processor asks V through `chain`, finds transactions through
    /// `txs`, removes the pool's conflicts from `mempool` and has no signal listeners (S7); on a
    /// masternode `m_signer` is [`IsParts::signer`], which step 7c sets.
    pub fn build(self, shared: &Arc<Shared>) -> (NetIsHandler, IsWorker) {
        let peers: Arc<dyn IsPeers> = Arc::new(NodeIsPeers(Arc::clone(shared)));
        // Core: v24.0.0 src/init.cpp:2198
        let signer = self.masternode.then(|| self.is.signer.seam()).flatten();
        let processor = Arc::new(IsProcessor::new(IsProcessorParts {
            manager: Arc::clone(&self.is.manager),
            txs: self.txs,
            chain: Arc::new(VChain(self.chain.clone())),
            chainlocks: self.chainlocks,
            recsigs: Arc::clone(&self.is.recsigs) as _,
            peers: Arc::clone(&peers),
            signer: signer.clone(),
            mempool: self.mempool,
            notify: Arc::new(NoNotify),
            resolving: Arc::clone(&self.is.resolving),
            is_llmq: self.params.consensus.llmq_type_dip0024_instant_send,
        }));
        let net = Arc::new(NetInstantSend::new(
            Arc::clone(&self.params),
            Arc::clone(&self.is.manager),
            peers,
            Arc::clone(&self.is.recsigs) as _,
            processor,
            signer,
        ));
        let handler = NetIsHandler {
            net: Arc::clone(&net),
            tree: TreeView(self.chain.reader().clone()),
        };
        let worker = IsWorker {
            net,
            passes: NodePasses {
                chain: self.chain,
                params: self.params,
                tip: self.is.tip.clone(),
            },
            interrupt: Arc::default(),
            thread: None,
        };
        (handler, worker)
    }
}

/// `m_peer_manager` as NetInstantSend reaches it, from the message thread and the worker.
pub struct NodeIsPeers(pub Arc<Shared>);

impl IsPeers for NodeIsPeers {
    // Core: v24.0.0 src/net_processing.cpp:6919-6923 PeerMisbehaving
    fn misbehaving(&self, node: NodeId, score: i32) {
        self.0.misbehaving(node, score, "");
    }

    // Core: v24.0.0 src/net_processing.cpp:6930-6935 PeerEraseObjectRequest
    fn erase_object_request(&self, node: NodeId, hash: &IsLockHash) {
        self.0.erase_object_request(node, &inv(hash));
    }

    // Core: v24.0.0 src/net_processing.cpp:6974-6977 PeerForgetObjectRequest
    fn forget_object_request(&self, hash: &IsLockHash) {
        self.0.forget_object_request(&inv(hash));
    }

    // Core: v24.0.0 src/net_processing.cpp:6989-6992 PeerRelayInvFiltered
    fn relay_inv_filtered_tx(&self, hash: &IsLockHash, tx: &Arc<Tx>) {
        self.0.relay_inv_filtered(&inv(hash), Related::Tx(tx));
    }

    // Core: v24.0.0 src/net_processing.cpp:6989-6992 PeerRelayInvFiltered
    fn relay_inv_filtered_txid(&self, hash: &IsLockHash, txid: &TxHash) {
        self.0.relay_inv_filtered(&inv(hash), Related::Hash(*txid));
    }

    // Core: v24.0.0 src/net_processing.cpp:7009-7012 PeerAskPeersForTransaction
    fn ask_peers_for_transaction(&self, txid: &TxHash) {
        self.0.handle().ask_peers_for_transaction(*txid);
    }
}

/// The message checks' view: `ResolveCycleHeight`'s `LookupBlockIndex` over V's block tree,
/// read past V, which is all they read.
// Core: v24.0.0 src/instantsend/net_instantsend.cpp:40-55, :74-88
struct TreeView(ChainReader);

fn unread() -> NodeViewError {
    NodeViewError::Inconsistent("the message checks read no quorum".to_owned())
}

impl QuorumScanView for TreeView {
    type Error = NodeViewError;

    fn tip_height(&self) -> u32 {
        0
    }

    fn scan(&self, _: LlmqType, _: u32) -> Result<ScanLookup, NodeViewError> {
        Err(unread())
    }

    fn scan_at_tip(&self, _: LlmqType, _: usize) -> Result<ScanLookup, NodeViewError> {
        Err(unread())
    }

    fn block_height(&self, hash: &BlockHash) -> Result<Option<u32>, NodeViewError> {
        let chain = |e: dash_chain::ChainError| NodeViewError::Chain(e.to_string());
        let Some(id) = self.0.try_lookup(hash).map_err(chain)? else {
            return Ok(None);
        };
        Ok(self.0.try_entry(id).map_err(chain)?.map(|e| e.height))
    }

    fn quorum(
        &self,
        _: LlmqType,
        _: &QuorumHash,
    ) -> Result<Option<dash_evo::locks::QuorumStatus>, NodeViewError> {
        Err(unread())
    }
}

/// A fault inside `ProcessMessage`: Core's `ProcessMessages` catches the thrown store or chain
/// error and logs it, and the peer stays unscored.
// Core: v24.0.0 src/net_processing.cpp:5945-5962 ProcessMessages; src/dbwrapper.h:46
fn thrown<E: fmt::Display>(e: &IsNetError<E>) -> Throw {
    let type_name = match e {
        IsNetError::Store(_) => "15dbwrapper_error",
        IsNetError::View(_) | IsNetError::UnknownLlmqType(_) => "St13runtime_error",
    };
    Throw {
        what: format!("NetInstantSend: {e}").into(),
        type_name,
    }
}

/// NetInstantSend's `ProcessMessage`.
pub struct NetIsHandler {
    net: Arc<NetInstantSend>,
    tree: TreeView,
}

impl NetHandler for NetIsHandler {
    fn commands(&self) -> &'static [CommandString] {
        &COMMANDS
    }

    fn core_handler(&self) -> Option<CoreHandler> {
        Some(CoreHandler::NetInstantSend)
    }

    /// The gate and the read at Core's read point, so a payload that does not decode is the
    /// exception boundary's, then the rest under the read's BLS scheme.
    // Core: v24.0.0 src/instantsend/net_instantsend.cpp:238-289
    fn process_message(
        &mut self,
        cx: &mut HandlerCx<'_>,
        _cmd: &CommandString,
        payload: &[u8],
    ) -> Result<(), Throw> {
        if !self.net.manager().is_instant_send_enabled() {
            return Ok(());
        }
        let _: IsdLock = cx.read(&mut &payload[..])?;
        let read = cx.ctx.bls();
        self.net
            .process_message(&self.tree, cx.id(), payload, read)
            .map(drop)
            .map_err(|e| thrown(&e))
    }

    // Core: v24.0.0 src/net_processing.cpp:2446-2447; src/instantsend/instantsend.cpp:355-364
    fn already_have(&self, inv: &Inventory) -> bool {
        if inv.inv_type() != InvType::IsdLock {
            return false;
        }
        let hash = inv.inv_hash().to_lendian();
        self.net.manager().already_have(&hash).unwrap_or_else(|e| {
            tracing::error!(target: "instantsend", "AlreadyHave(islock={}): {e}", inv.inv_hash());
            false
        })
    }
}

/// The peer manager's reads of `m_isman`: `getdata`'s lock by hash, the locks after a filtered
/// block's matched transactions, the `mempool` reply's lock invs, and a transaction's
/// `AlreadyHave`. A failed read of the IS store is a miss.
// Core: v24.0.0 src/net_processing.cpp:2404-2406, :2862-2864, :3054-3058, :6659-6664;
// src/instantsend/instantsend.cpp:366-426
pub struct NodeIsLocks(pub Arc<IsManager>);

impl NodeIsLocks {
    fn by_txid(&self, txid: &TxHash) -> Option<IsdLock> {
        self.0.get_instant_send_lock_by_txid(txid).unwrap_or_else(|e| {
            tracing::error!(target: "instantsend", "GetInstantSendLockByTxid({txid}): {e}");
            None
        })
    }
}

fn encoded(islock: &IsdLock) -> Vec<u8> {
    let mut out = Vec::new();
    islock.encode(&mut out);
    out
}

impl IsLockLookup for NodeIsLocks {
    fn islock_by_txid(&self, txid: &TxHash) -> Option<Vec<u8>> {
        self.by_txid(txid).as_ref().map(encoded)
    }

    fn islock_hash_by_txid(&self, txid: &TxHash) -> Option<[u8; 32]> {
        self.by_txid(txid).as_ref().map(isdlock_hash)
    }

    fn islock_by_hash(&self, hash: &[u8; 32]) -> Option<Vec<u8>> {
        let islock = self.0.get_instant_send_lock_by_hash(hash).unwrap_or_else(|e| {
            tracing::error!(target: "instantsend", "GetInstantSendLockByHash({}): {e}", Hash256::from(*hash));
            None
        });
        islock.as_ref().map(encoded)
    }

    fn is_waiting_for_tx(&self, txid: &TxHash) -> bool {
        self.0.is_waiting_for_tx(txid)
    }

    fn is_locked(&self, txid: &TxHash) -> bool {
        self.0.is_locked(txid).unwrap_or_else(|e| {
            tracing::error!(target: "instantsend", "IsLocked({txid}): {e}");
            false
        })
    }
}

/// A pass's view: [`NodeQuorumView`] with the InstantSend tip cache, in one `OnV` turn, under
/// V's published BLS scheme.
// Core: v24.0.0 src/instantsend/net_instantsend.cpp:116-160
#[derive(Clone)]
pub struct NodePasses {
    chain: ChainHandle,
    params: Arc<ChainSetup>,
    tip: dashd_locks::is::IsTipCache,
}

impl IsPassView for NodePasses {
    type Error = NodeViewError;

    fn with_pass<T: Send + 'static>(
        &self,
        f: impl PassBody<NodeViewError, T>,
    ) -> Result<T, IsNetError<NodeViewError>> {
        let (params, tip) = (Arc::clone(&self.params), self.tip.clone());
        self.chain
            .on_v(move |o, q| {
                let view =
                    NodeQuorumView::new(o, q, &params).map_err(IsNetError::View)?.with_is_tip(&tip);
                let scheme = view.bls();
                f(&view, scheme)
            })
            .recv()
            .map_err(|_| IsNetError::View(NodeViewError::Chain(V_STOPPED.to_owned())))?
    }
}

const V_STOPPED: &str = "the chainstate has stopped";

/// The `isman` worker: [`NetInstantSend::work_thread_main`] on its own thread.
pub struct IsWorker {
    net: Arc<NetInstantSend>,
    passes: NodePasses,
    interrupt: Arc<ThreadInterrupt>,
    thread: Option<JoinHandle<()>>,
}

impl IsWorker {
    /// `NetInstantSend::Interrupt`, for `InterruptHandlers`.
    // Core: v24.0.0 src/instantsend/net_instantsend.h:65
    pub fn interrupter(&self) -> Arc<dyn Fn() + Send + Sync> {
        let interrupt = Arc::clone(&self.interrupt);
        Arc::new(move || interrupt.interrupt())
    }

    /// `NetInstantSend::Start`: the `isman` thread. A tick's fault ends it and aborts the node
    /// through `abort`, but for V having stopped, whose fault V reported.
    ///
    /// # Errors
    ///
    /// The thread could not be spawned.
    ///
    /// # Panics
    ///
    /// On a second start.
    // Core: v24.0.0 src/instantsend/net_instantsend.cpp:291-299
    pub fn start(&mut self, abort: Arc<dyn AbortSink>) -> std::io::Result<()> {
        assert!(self.thread.is_none(), "isman started twice");
        let (net, interrupt) = (Arc::clone(&self.net), Arc::clone(&self.interrupt));
        let passes = self.passes.clone();
        self.thread = Some(trace_thread("isman", move || {
            let mut rng = StrongRng::default();
            match net.work_thread_main(&passes, &*interrupt, &mut rng) {
                Ok(()) => {}
                Err(IsNetError::View(NodeViewError::Chain(e))) if e == V_STOPPED => {}
                Err(e) => abort.abort_node(&format!("NetInstantSend::WorkThreadMain: {e}"), None),
            }
        })?);
        Ok(())
    }
}

/// `NetInstantSend::Stop`: interrupts `isman`, then joins it.
// Core: v24.0.0 src/instantsend/net_instantsend.cpp:301-311
impl Drop for IsWorker {
    fn drop(&mut self) {
        self.interrupt.interrupt();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
