//! The rdashd side (O2b §5, O2b-3b): `NodeSut`'s V driven op by op, its results in the step's
//! neutral form ([`Step`]) beside Core's.
//!
//! Only the ops V serves today run (DEC-286): `BLOCK`, `HEADER` and `PROPOSAL` through
//! [`shape`]'s port of Core's handlers (a proposal's checks and its `TestBlockValidity` in V's one
//! `propose_block` command, as under the handler's one `cs_main` hold), `INVALIDATE`,
//! `RECONSIDER` and `PRECIOUS` through V's block RPC commands, `ACTIVATE`, `CL_ENFORCE`, `SPORK`
//! and `SETTIME`. The others are [`Unsupported`] before anything runs, never a made-up reply:
//! `CL_RECORD` waits for LK1a's signature checks, `FLUSH` and `RESTART` for a10's flush counters
//! and images. The directed tests run all three on V outside the runner
//! ([`NodeSide::record_verified_chainlock`], [`NodeSide::flush`], [`NodeSide::restart`]).
//!
//! A step's planes:
//! - **reply**: the shaped result, and a `BlockChecked` list of every `BlockChecked` V emitted
//!   during the op, in order.
//! - **records**: none. CH3a's a7 adds the node's O8 tap; nothing here synthesizes them.
//! - **events**: a listener on V's validation signals records Core's tuples as the shim's
//!   `EventLog` does (`src/test/oracle/oracle_chain.cpp:211-297` at the pin), except
//!   `NotifyMasternodeListChanged`, which V does not emit (no counterpart yet). The shim drains
//!   Core's queue on its own thread at the op's end, so Core lists the synchronous signals as
//!   they fired and then the queued ones (`UpdatedBlockTip`, `BlockConnected`,
//!   `BlockDisconnected`, `ChainStateFlushed`, `NotifyChainLock`) in queue order. The listener
//!   keeps the two apart and joins them the same way, and gives the runner the count of queued
//!   ones ([`NodeStep::facts`]): where Core may also have drained mid-op, [`super::diff`]
//!   would refuse the op if Core's step queued as many and were otherwise the same. No step
//!   here compares the same until a7 and a10 (no records, no flush counters), so today such an
//!   op diverges.
//!
//!   Each op hears its own signals only: whatever V signals after an op's fence and queue sync
//!   and before the next op begins is dropped unchecked. Nothing reaches that window today:
//!   `NodeSut` has no hub listener but this one and no timer, and V's deferred work runs before
//!   the fence. A future listener that posts work back into V, or a timer, would need a check
//!   that nothing was heard between ops (O2b §5.1's quiescence check).
//! - **post**: `down`, `tip`, `best_header`, `utxo` (the shim's `Utxo()` digest, over V's coins
//!   cache and the database below it), `index` (the shim's `Index()` digest), `ibd`, `best_cl`,
//!   `sporks`, `tips` and `deployments`, read as the shim's `FinishReport` reads them: once V is
//!   idle and the signal queue has drained. `flushes`, `flush_delta` and `persisted` stay empty
//!   until a10: V has no flush counters by cause.
//!
//! `NodeSut`'s V connects and disconnects through ND1's `CoreConnector` (CH3a a5's
//! `DisconnectBlock`) and flushes through `NodeFlush` over `NodeBlockFiles`
//! (`dashd-node/src/chainstate.rs`). It checks blocks, writes their undo data and reads it back
//! to disconnect, so a connect-time rejection, a reorg and the `index` digest's undo bits are
//! compared as they are, and the coins each leaves in `utxo`. What it does not do yet:
//! **records** until a7, and the **flush counters** until a10. A step with Core records differs
//! in `records`, any other in `post.flushes`, and the runner names the first other plane that
//! differs, if one does ([`super::diff::GAPS`]).
//!
//! V stopping is reported as it happens: the op's reply says V stopped (also when V stopped in
//! work the op left deferred), the post is down, and every later op but `SETTIME` replies
//! `session-down`, as a down session's.
//!
//! V schedules ChainLock enforcement on every tip change (`ClTrigger::TipScheduled`), where the
//! shim's handler schedules none (O2b D12). Nothing shows it without `CL_RECORD`; `NodeSut` needs
//! a seam for it before LK1a.
//!
//! `NodeSut` has no image yet (a10), so [`NodeSide::restore`] builds a new SUT in a fresh
//! directory and replays the image onto it, until a10's images. Core's restore is a load, which
//! clears its last notified header tip; the replay leaves V's at the image's tip, so an input
//! whose first header is not a new best one hears no `NotifyHeaderTip` on the node where Core's
//! does. A clean restart could give that load now that `NodeSut`'s shutdown flushes store the
//! tip (BL-271); the replay stays until a10's images.

use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use dash_chain::{ChainReader, HeaderBatch, HeaderStop, Status};
use dash_consensus::coins::tx_out_ser;
use dash_consensus::deployments::info::DeploymentInfo;
use dash_consensus::deployments::{Deployments, NoEhfSignals, VersionBitsCache};
use dash_consensus::hash::sha256d;
use dash_consensus::{Block, BlockId, BlockValidationError, Header};
use dash_evo::spork::{SPORK_DEFS, SporkId};
use dash_num::Hash256;
use dash_oracle_sys::{
    BlockAt, ChainHash, ChainOp, CheckResult, Checked, Event, FlushCounts, Pin, Post, Reply,
    RestartKind, RpcError, SporkState, Utxo,
};
use dash_params::ChainParams;
use dash_primitives::BlockHash;
use dash_types::Numeric;
use dashd_chainstate::chain::{
    ActorReply, BlockNotFound, BlockState, ChainHandle, ChainLockSig, ChainQuery, FlushStateMode,
    NewBlock, NotTested, ValidationInterface,
};
use dashd_chainstate::coins::{OutPointKey, PackedCoin};
use dashd_chainstate::reorg::PoolsOnly;
use dashd_node::logging::Logger;
use dashd_node::sut::{NodeSut, Teardown};
use dashd_node::ui::Ui;
use serde_json::{Value, json};

use super::core::{SPORK_19, Schedule, T_RECENT, image_world};
use super::diff::NodeFacts;
use super::params;
use super::run::{Refused, Unsupported};
use super::shape::{self, Known, Node, Outcome, Processed, TipRow, block_hash_hex};
use super::step::Step;
use super::world::World;

/// The index status bits the post-state's digest covers (O2b §4.5): validity, data, undo, the
/// failure bits and the ChainLock conflict.
// Core: src/test/oracle/oracle_chain.cpp:108-109 at the pin
const INDEX_STATUS_MASK: u32 = Status::VALID_MASK
    | Status::HAVE_DATA
    | Status::HAVE_UNDO
    | Status::FAILED_MASK
    | Status::CONFLICT_CHAINLOCK;

/// The reply of an op on a down session.
// Core: src/test/oracle/oracle_chain.cpp:961-962 at the pin
const SESSION_DOWN: &str = "session-down";
/// The reply of the op during which V stopped: no handler returned, so there is no Core shape.
pub const V_STOPPED: &str = "V stopped";

/// Core's `uint256` in the internal byte order the oracle's hashes hold.
fn chain_hash(hash: BlockHash) -> ChainHash {
    ChainHash(Hash256::from(hash).to_lendian())
}

fn hash_arg(hash: BlockHash) -> Value {
    block_hash_hex(hash).into()
}

fn event(name: &str, args: Vec<Value>) -> Event {
    Event {
        name: name.to_owned(),
        args,
    }
}

/// What the listener heard since the op began.
#[derive(Default)]
struct Heard {
    /// The synchronous signals, as V emitted them.
    sync: Vec<Event>,
    /// The queued signals, in the queue's order.
    queued: Vec<Event>,
    /// Every `BlockChecked`, in order: the reply's list.
    checked: Vec<(BlockHash, BlockState)>,
}

/// The shim's `EventLog` on V's signals: each signal as `[name, args...]`, hashes as Core's hex,
/// a null fork as null, `BlockChecked` as `[name, hash, res, cls, why]`. Like the shim's, it
/// does not hear `InitializeCurrentBlockTip`.
// Core: src/test/oracle/oracle_chain.cpp:211-297 at the pin
struct Listener {
    reader: ChainReader,
    heard: Mutex<Heard>,
}

impl Listener {
    fn hash(&self, id: BlockId) -> Value {
        hash_arg(self.reader.entry(id).expect("a signalled entry is V's").hash)
    }

    fn fork(&self, fork: Option<BlockId>) -> Value {
        fork.map_or(Value::Null, |id| self.hash(id))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Heard> {
        self.heard.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn add(&self, name: &str, args: Vec<Value>) {
        self.lock().sync.push(event(name, args));
    }

    /// A signal Core queues (`ENQUEUE_AND_LOG_EVENT`).
    // Core: validationinterface.cpp:215-227, :254-278, :309-316
    fn queue(&self, name: &str, args: Vec<Value>) {
        self.lock().queued.push(event(name, args));
    }

    fn take(&self) -> Heard {
        std::mem::take(&mut *self.lock())
    }

    /// `submitblock_StateCatcher`'s state: the last `BlockChecked` of `hash`, if one fired.
    // Core: rpc/mining.cpp:989-1005
    fn last_checked(&self, hash: BlockHash) -> Option<BlockState> {
        self.lock().checked.iter().rev().find(|(h, _)| *h == hash).map(|(_, s)| s.clone())
    }
}

/// `StateFields`: `[res, cls, why]` of a block's state.
// Core: src/test/oracle/oracle_chain.cpp:186-192 at the pin
fn state_args(hash: BlockHash, state: &BlockState) -> Vec<Value> {
    let Checked(_, res, cls, why) = shape::checked(hash, state);
    let res = match res {
        CheckResult::Valid => "valid",
        CheckResult::Invalid => "invalid",
        CheckResult::Error => "error",
    };
    vec![hash_arg(hash), res.into(), cls.into(), why.into()]
}

impl ValidationInterface for Listener {
    fn block_checked(&self, block: &Block, state: &BlockState, _: bool) {
        let hash = block.hash();
        let mut heard = self.lock();
        heard.sync.push(event("BlockChecked", state_args(hash, state)));
        heard.checked.push((hash, state.clone()));
    }

    fn new_pow_valid_block(&self, nb: &NewBlock<'_>) {
        self.add("NewPoWValidBlock", vec![self.hash(nb.id)]);
    }

    fn accepted_block_header(&self, id: BlockId) {
        self.add("AcceptedBlockHeader", vec![self.hash(id)]);
    }

    fn notify_header_tip(&self, id: BlockId, ibd: bool) {
        self.add("NotifyHeaderTip", vec![self.hash(id), ibd.into()]);
    }

    fn synchronous_updated_block_tip(&self, new: BlockId, fork: Option<BlockId>, ibd: bool) {
        let args = vec![self.hash(new), self.fork(fork), ibd.into()];
        self.add("SynchronousUpdatedBlockTip", args);
    }

    fn updated_block_tip(&self, new: BlockId, fork: Option<BlockId>, ibd: bool) {
        self.queue("UpdatedBlockTip", vec![self.hash(new), self.fork(fork), ibd.into()]);
    }

    fn block_connected(&self, _: &Arc<Block>, id: BlockId) {
        self.queue("BlockConnected", vec![self.hash(id)]);
    }

    fn block_disconnected(&self, _: &Arc<Block>, id: BlockId) {
        self.queue("BlockDisconnected", vec![self.hash(id)]);
    }

    fn chain_state_flushed(&self, locator: &[BlockHash]) {
        let tip = locator.first().map_or(Value::Null, |&h| hash_arg(h));
        self.queue("ChainStateFlushed", vec![tip]);
    }

    fn notify_chain_lock(&self, id: BlockId, _: &Arc<dashd_chainstate::chain::ChainLockSig>) {
        self.queue("NotifyChainLock", vec![self.hash(id)]);
    }
}

/// [`shape`]'s handlers over V. A disconnected reply means V stopped: the caller drops whatever
/// the handler made of it.
struct OnV<'a> {
    v: &'a ChainHandle,
    listener: &'a Listener,
    stopped: bool,
}

impl Node for OnV<'_> {
    fn lookup(&mut self, hash: BlockHash) -> Option<Known> {
        let reader = self.v.reader();
        let status = reader.entry(reader.lookup(&hash)?)?.status;
        Some(Known {
            scripts: status.is_valid(Status::VALID_SCRIPTS),
            failed: status.has(Status::FAILED_MASK),
        })
    }

    fn process_block(&mut self, block: Block) -> Processed {
        let hash = block.hash();
        match self.v.process_block(Arc::new(block), true, None, true).recv() {
            Ok(r) => Processed {
                new_block: r.new_block,
                accepted: r.accepted,
                checked: self.listener.last_checked(hash),
            },
            Err(_) => {
                self.stopped = true;
                Processed {
                    new_block: false,
                    accepted: false,
                    checked: None,
                }
            }
        }
    }

    fn process_header(&mut self, header: Header) -> Option<BlockValidationError> {
        let batch = HeaderBatch {
            hashes: vec![header.hash()],
            headers: vec![header],
        };
        match self.v.view_handle().process_headers(batch, None) {
            Ok(r) => match r.stop {
                None => None,
                Some(HeaderStop::Invalid {
                    reject,
                    ..
                }) => Some(reject),
                Some(HeaderStop::BelowAnchor {
                    ..
                }) => unreachable!("the node's chain has no anchor"),
            },
            Err(_) => {
                self.stopped = true;
                None
            }
        }
    }

    fn propose(&mut self, block: Block) -> Result<BlockState, NotTested> {
        match self.v.propose_block(Arc::new(block)).recv() {
            Ok(reply) => reply,
            Err(_) => {
                self.stopped = true;
                Ok(BlockState::Valid)
            }
        }
    }
}

impl OnV<'_> {
    /// `invalidateblock`, `reconsiderblock` or `preciousblock` once V replied
    /// ([`shape::block_rpc`]); no reply means V stopped.
    fn block_rpc(&mut self, reply: ActorReply<Result<(), BlockNotFound>>) -> Outcome {
        match reply.recv() {
            Ok(reply) => shape::block_rpc(reply),
            Err(_) => {
                self.stopped = true;
                rpc_error(V_STOPPED)
            }
        }
    }
}

/// What the post-state reads on V in one class R query (Core reads them under `cs_main`).
struct Indexed {
    tip: BlockAt,
    best_header: BlockAt,
    index: ChainHash,
    tips: Vec<Value>,
    deployments: Value,
}

fn block_at(height: u32, hash: BlockHash) -> BlockAt {
    BlockAt(i32::try_from(height).expect("a regtest height"), chain_hash(hash))
}

/// One index entry as the post-state's digest reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexEntry {
    pub hash: BlockHash,
    pub height: u32,
    /// `nStatus`.
    pub status: u32,
    /// `nChainTx != 0`.
    pub has_txs: bool,
}

/// The shim's `Index()`: SHA-256 over every entry, sorted by hash as Core's `uint256` sorts
/// (`memcmp` of the internal bytes): the hash, `nHeight` (i32 LE), `nStatus & INDEX_STATUS_MASK`
/// (u32 LE) and `nChainTx != 0` (one byte). `CHashWriter::GetSHA256` is a single SHA-256.
// Core: src/test/oracle/oracle_chain.cpp:676-689 at the pin, uint256.h:55-59, hash.h:128-132
pub fn index_digest(entries: &[IndexEntry]) -> ChainHash {
    use bitcoin_hashes::{HashEngine, sha256};

    let mut sorted: Vec<(ChainHash, &IndexEntry)> =
        entries.iter().map(|e| (chain_hash(e.hash), e)).collect();
    sorted.sort_unstable_by_key(|a| a.0.0);
    let mut engine = sha256::HashEngine::default();
    for (hash, e) in sorted {
        engine.input(&hash.0);
        engine.input(&i32::try_from(e.height).expect("a regtest height").to_le_bytes());
        engine.input(&(e.status & INDEX_STATUS_MASK).to_le_bytes());
        engine.input(&[u8::from(e.has_txs)]);
    }
    ChainHash(sha256::Hash::from_engine(engine).to_byte_array())
}

/// The shim's `Utxo()`: SHA-256 over coinstats' `TxOutSer` of each coin, in the order given
/// (Core's `COutPoint` order, as [`NodeSut::tip_coins`] reads them), with their number and their
/// total.
// Core: src/test/oracle/oracle_chain.cpp:666-673 at the pin, kernel/coinstats.cpp:51
pub fn utxo_digest(coins: &[(OutPointKey, PackedCoin)]) -> Utxo {
    use bitcoin_hashes::{HashEngine, sha256};

    let mut engine = sha256::HashEngine::default();
    let mut ser = Vec::new();
    let mut amount = 0;
    for (k, coin) in coins {
        let coin = coin.as_ref();
        ser.clear();
        tx_out_ser(&k.outpoint(), &coin, &mut ser);
        engine.input(&ser);
        amount += coin.value();
    }
    Utxo {
        hash: ChainHash(sha256::Hash::from_engine(engine).to_byte_array()),
        txouts: coins.len() as u64,
        amount,
    }
}

/// Every entry of V's index, in id order.
fn index_entries(q: &ChainQuery<'_>, reader: &ChainReader) -> Vec<IndexEntry> {
    let entry = |id| {
        let e = reader.entry(id).expect("the arena holds it");
        IndexEntry {
            hash: e.hash,
            height: e.height,
            status: e.status.bits(),
            has_txs: q.idx.n_chain_tx(id) != 0,
        }
    };
    q.chain.ids().map(entry).collect()
}

/// `getchaintips`' rows over V's index, with `HaveTxsDownloaded()` from V's tables.
fn tip_rows(q: &ChainQuery<'_>, reader: &ChainReader) -> Vec<TipRow> {
    let tips = q.chain.chain_tips_with(&|id| q.idx.have_txs_downloaded(id));
    let entry = |id| reader.entry(id).expect("a tip and its fork point are V's");
    tips.expect("V has a tip")
        .into_iter()
        .map(|t| TipRow {
            height: t.height,
            hash: t.hash,
            bits: entry(t.id).header.bits,
            chain_work: t.chain_work.to_bendian(),
            branchlen: t.branchlen,
            fork_point: entry(t.fork_point).hash,
            status: t.status.as_str(),
        })
        .collect()
}

/// A load's directory, removed with everything in it on drop.
struct LoadDir(PathBuf);

impl Drop for LoadDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An op V ran: its step, and the facts the runner holds against Core's step before it compares.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeStep {
    pub step: Step,
    pub facts: NodeFacts,
}

/// A loaded SUT: the node, the listener on its signals, and its directory.
struct Loaded {
    sut: NodeSut,
    listener: Arc<Listener>,
    /// Declared last: removed once the node is gone.
    dir: LoadDir,
}

/// The rdashd side of a `diff_chain` session: a `NodeSut` on the schedule's arguments, with the
/// image's chain built into it.
pub struct NodeSide {
    schedule: Schedule,
    /// The session's directory: each load is in a subdirectory of its own.
    dir: PathBuf,
    loads: u32,
    params: Arc<ChainParams>,
    loaded: Loaded,
    /// The image's blocks, serialized.
    image: Arc<[Vec<u8>]>,
    /// V stopped: the session is down until the next restore.
    stopped: bool,
}

/// The schedules whose D8 check passed in this process.
static CHECKED: Mutex<Vec<(Pin, Schedule)>> = Mutex::new(Vec::new());

/// D8's check for `schedule`, once per process. The oracle refuses `oracle_chainparams_json`
/// while this thread holds a chain session, so it runs before [`CoreSut::start`]
/// ([`super::core::CoreSut::start`]).
fn check_params(pin: Pin, schedule: Schedule, running: &ChainParams) -> Result<(), String> {
    let mut checked = CHECKED.lock().unwrap_or_else(|p| p.into_inner());
    if !checked.contains(&(pin, schedule)) {
        params::check(pin, schedule, running)?;
        checked.push((pin, schedule));
    }
    Ok(())
}

/// The op kinds V serves (DEC-286).
fn served(op: &ChainOp) -> bool {
    matches!(
        op,
        ChainOp::Block(_)
            | ChainOp::Header(_)
            | ChainOp::Proposal(_)
            | ChainOp::Invalidate(_)
            | ChainOp::Reconsider { .. }
            | ChainOp::Precious(_)
            | ChainOp::Activate
            | ChainOp::ClEnforce
            | ChainOp::Spork { .. }
            | ChainOp::SetTime(_)
    )
}

impl NodeSide {
    /// A `NodeSut` on `["-regtest", "-printtoconsole=0", schedule.args()...]` at [`T_RECENT`] in
    /// a subdirectory of `dir`, after D8's check of its parameters; then the image's blocks and
    /// spork 19 on, checked active, as [`CoreSut::start`](super::core::CoreSut::start) builds
    /// Core's. Run it before Core's session starts on this thread (D8's check needs the oracle
    /// free). The world it returns is the image's.
    pub fn start(pin: Pin, schedule: Schedule, dir: &Path) -> Result<(NodeSide, World), String> {
        let world = image_world();
        let loaded = open(&dir.join("load-0"), schedule, T_RECENT)?;
        check_params(pin, schedule, loaded.sut.params())?;
        let mut side = NodeSide {
            schedule,
            dir: dir.to_owned(),
            loads: 0,
            params: Arc::new(loaded.sut.params().clone()),
            loaded,
            image: world.blocks[1..].iter().map(|b| b.built_bytes()).collect(),
            stopped: false,
        };
        side.build_image()?;
        Ok((side, world))
    }

    /// The input's start: a new SUT at `mocktime` with the image rebuilt (Core restores its
    /// image; `NodeSut` has none until a10's images). The previous load and its
    /// directory are gone once the new one opened; a failed open leaves the previous one.
    pub fn restore(&mut self, mocktime: i64) -> Result<(), String> {
        self.loads += 1;
        let dir = self.dir.join(format!("load-{}", self.loads));
        self.loaded = open(&dir, self.schedule, mocktime)?;
        self.stopped = false;
        self.build_image()
    }

    /// The image's blocks, each connected, then spork 19 on.
    fn build_image(&mut self) -> Result<(), String> {
        let image = Arc::clone(&self.image);
        for (height, bytes) in (1..).zip(image.iter()) {
            let s = self.image_step(&ChainOp::Block(bytes.clone()))?;
            if s.reply.outcome != Ok(Value::Null) {
                return Err(format!("image block {height} not connected: {:?}", s.reply));
            }
        }
        let spork = ChainOp::Spork {
            id: SPORK_19,
            value: 0,
        };
        let s = self.image_step(&spork)?;
        if !s.post.sporks.is_some_and(|s| s.contains(&SporkState(SPORK_19, 0, true))) {
            return Err("spork 19 is not active in the node's image".into());
        }
        Ok(())
    }

    fn image_step(&mut self, op: &ChainOp) -> Result<Step, String> {
        let s = self.step(op).map_err(|u| format!("the image's {}: {u:?}", u.kind))?;
        Ok(s.step)
    }

    /// V, while it serves.
    fn v(&self) -> Option<&ChainHandle> {
        if self.stopped {
            None
        } else {
            self.loaded.sut.chain()
        }
    }

    /// One op, or [`Unsupported`] before anything runs, for an op kind V does not serve. How many
    /// queued signals it delivered comes with the step for the runner to hold against Core's
    /// ([`super::diff::judge`]).
    pub fn step(&mut self, op: &ChainOp) -> Result<NodeStep, Unsupported> {
        if !served(op) {
            return Err(Unsupported::new(op, Refused::Op));
        }
        self.loaded.listener.take();
        let outcome = match op {
            ChainOp::SetTime(t) => {
                // Core: src/test/oracle/oracle_chain.cpp:1032-1035 at the pin (also while down)
                self.loaded.sut.clock().set_mocktime(*t);
                Ok(Value::Null)
            }
            _ if self.v().is_none() => rpc_error(SESSION_DOWN),
            _ => self.run(op),
        };
        let (step, queued) = self.finish(outcome);
        Ok(NodeStep {
            step,
            facts: NodeFacts {
                queued,
            },
        })
    }

    /// A supported op other than `SETTIME` on a serving V.
    fn run(&mut self, op: &ChainOp) -> Outcome {
        let v = self.loaded.sut.chain().expect("serving");
        let mut on_v = OnV {
            v,
            listener: &self.loaded.listener,
            stopped: false,
        };
        let outcome = match op {
            ChainOp::Block(bytes) => shape::submit_block(bytes, &mut on_v),
            ChainOp::Header(bytes) => shape::submit_header(bytes, &mut on_v),
            ChainOp::Proposal(bytes) => shape::proposal(bytes, &mut on_v),
            // Each handler's lookup, its change and `ActivateBestChain` run in one V command.
            // Core: src/test/oracle/oracle_chain.cpp:978-986 at the pin
            ChainOp::Invalidate(hash) => on_v.block_rpc(v.invalidate_block(BlockHash::from(*hash))),
            ChainOp::Reconsider {
                hash,
                ignore_chainlocks,
            } => on_v.block_rpc(v.reconsider_block(BlockHash::from(*hash), *ignore_chainlocks)),
            ChainOp::Precious(hash) => on_v.block_rpc(v.precious_block(BlockHash::from(*hash))),
            // The shim's reply: `{ok, state}` with `BlockValidationState::ToString()`. V's ABC
            // replies `Ok` with a valid state; its `Err` follows an `AbortNode`, which stops V.
            // Core: src/test/oracle/oracle_chain.cpp:987-991 at the pin
            // Core: consensus/validation.h:105-116
            ChainOp::Activate => match v.activate_best_chain().recv() {
                Ok(Ok(())) => Ok(json!({"ok": true, "state": "Valid"})),
                Ok(Err(_)) | Err(_) => {
                    on_v.stopped = true;
                    rpc_error(V_STOPPED)
                }
            },
            // `CheckActiveState` then `EnforceBestChainLock`: the handler's timer task.
            // Core: src/test/oracle/oracle_chain.cpp:999-1003 at the pin
            ChainOp::ClEnforce => match v.chainlock_timer() {
                Ok(()) => Ok(Value::Null),
                Err(_) => {
                    on_v.stopped = true;
                    rpc_error(V_STOPPED)
                }
            },
            // `UpdateSpork` with the session key: whether it was signed.
            // Core: src/test/oracle/oracle_chain.cpp:1004-1008 at the pin
            ChainOp::Spork {
                id,
                value,
            } => {
                let sporks = self.loaded.sut.sporks().expect("loaded");
                Ok(sporks.update_spork(SporkId(*id), *value).is_some().into())
            }
            _ => unreachable!("dispatched by step"),
        };
        // A handler's reply over a stopped V is not V's: dropped.
        if on_v.stopped {
            self.stopped = true;
            return rpc_error(V_STOPPED);
        }
        outcome
    }

    /// The step as the shim's `FinishReport` makes it: V idle (a class R query runs after its
    /// deferred work), the signal queue drained, then the post read. The events are the
    /// synchronous ones, then the queued ones; the count of queued ones comes with the step. A V
    /// that stopped since the op began makes the reply [`V_STOPPED`], as the post is down.
    // Core: src/test/oracle/oracle_chain.cpp:1043-1049 at the pin
    fn finish(&mut self, mut outcome: Outcome) -> (Step, usize) {
        let serving = self.v().is_some();
        if self.v().is_some_and(|v| v.query(|_| ()).recv().is_err()) {
            self.stopped = true;
        }
        // A test thread carries no no-wait marker. A queue that cannot sync would leave the
        // queued events incomplete: V is taken as stopped, as for a failed fence.
        if self.v().is_some() && !self.loaded.sut.sync_signals() {
            self.stopped = true;
        }
        let post = self.post();
        if serving && self.stopped {
            outcome = rpc_error(V_STOPPED);
        }
        let heard = self.loaded.listener.take();
        let queued = heard.queued.len();
        let checked = heard.checked.iter().map(|(hash, state)| shape::checked(*hash, state));
        let mut events = heard.sync;
        events.extend(heard.queued);
        let step = Step {
            reply: Reply {
                checked: checked.collect(),
                outcome,
            },
            records: Vec::new(),
            events,
            post,
        };
        (step, queued)
    }

    /// The post-state (O2b §4.5), in two class R queries on V with nothing sent between them:
    /// the index's reads, then the coins ([`NodeSut::tip_coins`]). Either failing is V stopped.
    fn post(&mut self) -> Post {
        let mut post = Post {
            down: true,
            flush_delta: FlushCounts::default(),
            tie: Vec::new(),
            persisted: None,
            tip: None,
            best_header: None,
            utxo: None,
            index: None,
            flushes: None,
            ibd: None,
            best_cl: None,
            sporks: None,
            tips: None,
            deployments: None,
        };
        let Some(v) = self.v() else {
            return post;
        };
        let reader = v.reader().clone();
        let params = Arc::clone(&self.params);
        let Ok(read) = v.query(move |q| indexed(q, &reader, &params)).recv() else {
            self.stopped = true;
            return post;
        };
        // The coins are attached while V serves: no answer is V gone.
        let Some(coins) = self.loaded.sut.tip_coins() else {
            self.stopped = true;
            return post;
        };
        let view = v.view();
        post.down = false;
        post.utxo = Some(utxo_digest(&coins.expect("the in-memory coins")));
        post.tip = Some(read.tip);
        post.best_header = Some(Some(read.best_header));
        post.index = Some(read.index);
        // Core: src/test/oracle/oracle_chain.cpp:739 at the pin (the read may latch, as Core's)
        post.ibd = Some(v.view_handle().ibd_service_cache());
        let cl = view.best_chainlock().map(|sig| BlockAt(sig.height, chain_hash(sig.block_hash)));
        post.best_cl = Some(cl);
        post.sporks = Some(self.sporks());
        post.tips = Some(read.tips);
        post.deployments = Some(read.deployments);
        post
    }

    /// Another listener on the loaded V's signals, after the side's own, for tests that slow
    /// the queue's thread down (the queue sync in [`NodeSide::step`] must wait for it). The next
    /// restore's load drops it.
    pub fn register_listener(&self, listener: Arc<dyn ValidationInterface>) -> bool {
        self.loaded.sut.register_listener(listener)
    }

    /// V's index entries while V serves: what [`index_digest`] hashes, for the tests that hold
    /// it to Core's.
    pub fn index(&self) -> Option<Vec<IndexEntry>> {
        let v = self.v()?;
        let reader = v.reader().clone();
        v.query(move |q| index_entries(q, &reader)).recv().ok()
    }

    /// The loaded node's blocks directory, for tests that damage a block file where Core's
    /// session has no way to (CH3 §7.6's failure sites).
    pub fn blocks_dir(&self) -> PathBuf {
        self.loaded.dir.0.join("regtest").join("blocks")
    }

    /// V's disconnect pools since this load opened (`NodeSut::pools`, DEC-500): what they evicted
    /// and V's calls on them. Core's session keeps no pool, so the suites read V's alone.
    pub fn pools(&self) -> &PoolsOnly {
        self.loaded.sut.pools()
    }

    /// `CL_RECORD` on V as LK1a will run it once the signature verifies, the signature taken as
    /// valid as the shim's session takes every one (O2b D12): `CheckActiveState` and the best
    /// height (`ClIntake`), the stale check, then `UpdateBestChainlock` (`record_chainlock`),
    /// with Core's reply (`stale`, `not-better` or `accepted`). Its step is finished as
    /// [`step`](Self::step)'s are, V's deferred work fenced in. There is no `seen` check, and V
    /// enforces a lock that moves its floor at once (`ClTrigger::Recorded`, Core's zero-delay
    /// schedule), where the session leaves enforcement to `CL_ENFORCE`: this step and the next
    /// `CL_ENFORCE` hold the reorg's signals that Core's `CL_ENFORCE` step holds alone. Not
    /// served by [`step`](Self::step) until LK1a.
    // Core: src/chainlock/handler.cpp:139-206 at the pin, src/test/oracle/oracle_chain.cpp:993-997
    pub fn record_verified_chainlock(
        &mut self,
        height: i32,
        hash: [u8; 32],
        sig: [u8; 96],
    ) -> Step {
        self.directed(|v| match v.cl_intake().recv() {
            Err(_) => rpc_error(V_STOPPED),
            Ok(intake) if height <= intake.best_height => Ok("stale".into()),
            Ok(_) => {
                // `::SerializeHash(clsig)`: nHeight, blockHash, sig.
                let mut ser = height.to_le_bytes().to_vec();
                ser.extend_from_slice(&hash);
                ser.extend_from_slice(&sig);
                let sig_hash = Hash256::from_lendian(sha256d(&ser));
                let sig = Arc::new(ChainLockSig {
                    height,
                    block_hash: BlockHash::from(hash),
                    sig,
                });
                match v.record_chainlock(sig, sig_hash, None).recv() {
                    Ok(reply) if reply.relay => Ok("accepted".into()),
                    Ok(_) => Ok("not-better".into()),
                    Err(_) => rpc_error(V_STOPPED),
                }
            }
        })
    }

    /// `FLUSH` on V's `Flush` command, with the shim's reply: `{ok, state}` (V replies only when
    /// the flush succeeded, whose state is valid). A directed op for CONS-11's restart cases
    /// (CH3 §4.5), not served by [`step`](Self::step) until a10's flush counters. `INDEX_ONLY`
    /// has no V counterpart: `FlushSite` takes Core's four modes and no call forces the
    /// index-only write the shim's seam does (O2b D31). The op's `label` goes unused: it names
    /// Core's flush records, and V writes none until a7.
    // Core: src/test/oracle/oracle_chain.cpp:1009-1027 at the pin
    pub fn flush(&mut self, mode: FlushStateMode) -> Step {
        self.directed(|v| match v.flush(mode).recv() {
            Ok(()) => Ok(json!({"ok": true, "state": "Valid"})),
            Err(_) => rpc_error(V_STOPPED),
        })
    }

    /// A directed op: `act` on a serving V, its step finished as [`step`](Self::step)'s are.
    fn directed(&mut self, act: impl FnOnce(&ChainHandle) -> Outcome) -> Step {
        self.loaded.listener.take();
        let outcome = self.v().map_or_else(|| rpc_error(SESSION_DOWN), act);
        self.finish(outcome).0
    }

    /// `RESTART` as `NodeSut` runs it: the teardown `kind` names, then the next load on the same
    /// files and clock, with the shim's reply (`LoadReply`, the load's status and message), and
    /// the teardown's flushes (`NodeSut::restart`). A directed op for CONS-11's restart cases
    /// (CH3 §4.5), not served by [`step`](Self::step) until a10's images. A clean teardown runs
    /// Core's two shutdown flushes (BL-271). The step's signals are not
    /// Core's: the new V's listener registers once the load is done, so it hears at most the end
    /// of the load's `ActivateBestChain`, where Core's session registers before its load and
    /// hears all of it. A refused load leaves the session down, with V's message as an error,
    /// which no Core reply equals (`NodeSut` keeps no load status).
    // Core: src/test/oracle/oracle_chain.cpp:1028-1031, :940-953 at the pin
    pub fn restart(&mut self, kind: RestartKind) -> (Step, u32) {
        let how = match kind {
            RestartKind::Clean => Teardown::Clean,
            RestartKind::Kill => Teardown::Kill,
        };
        let flushes = self.loaded.sut.restart(how);
        let listened = listen(&self.loaded.sut);
        self.stopped = listened.is_err();
        let outcome = match listened {
            Ok(listener) => {
                self.loaded.listener = listener;
                Ok(json!({"status": "success", "message": ""}))
            }
            Err(e) => rpc_error(&e),
        };
        (self.finish(outcome).0, flushes)
    }

    /// `(id, value, active)` of every spork in Core's `sporkDefs`, in its order.
    // Core: src/test/oracle/oracle_chain.cpp:706-714 at the pin, spork.h:66-72
    fn sporks(&self) -> Vec<SporkState> {
        let sporks = self.loaded.sut.sporks().expect("loaded");
        SPORK_DEFS
            .iter()
            .map(|d| SporkState(d.id.0, sporks.value(d.id), sporks.is_active(d.id)))
            .collect()
    }
}

/// The post-state's index reads, on V.
fn indexed(q: &ChainQuery<'_>, reader: &ChainReader, params: &ChainParams) -> Indexed {
    let snap = reader.snapshot();
    let facts = &snap.tip;
    Indexed {
        tip: block_at(facts.tip_height, facts.tip_hash),
        best_header: block_at(facts.best_header_height, facts.best_header_hash),
        index: index_digest(&index_entries(q, reader)),
        tips: shape::chain_tips(&tip_rows(q, reader)),
        deployments: shape::deployment_info(
            facts.tip_hash,
            facts.tip_height,
            &deployment_infos(q, params, facts.tip),
        ),
    }
}

/// `getdeploymentinfo`'s entries at the tip: CS4's report over V's chain, with V's EHF source
/// (none until CH7a) and a fresh versionbits cache (a cache: it changes no answer).
fn deployment_infos(q: &ChainQuery<'_>, params: &ChainParams, tip: BlockId) -> Vec<DeploymentInfo> {
    let chain = q.chain.complete().expect("V's chain is the node's: no window");
    let cache = VersionBitsCache::new();
    Deployments::new(params, chain, &NoEhfSignals, &cache).deployment_info(tip)
}

/// A reply that is an error without a code: [`SESSION_DOWN`] or [`V_STOPPED`].
fn rpc_error(message: &str) -> Outcome {
    Err(RpcError {
        code: None,
        message: message.to_owned(),
    })
}

/// A `NodeSut` at `mocktime` in `dir` (created private, removed if the open fails), with the
/// listener on its signals.
fn open(dir: &Path, schedule: Schedule, mocktime: i64) -> Result<Loaded, String> {
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    let dir = LoadDir(dir.to_owned());
    let mut argv = vec!["-regtest", "-printtoconsole=0"];
    argv.extend(schedule.args());
    let logger = Arc::new(Logger::new());
    // No file, no console and no callback: nothing is logged, and nothing buffered for the
    // load's life.
    if !logger.start_logging() {
        return Err("the node's logger did not start".into());
    }
    let ui = Arc::new(Ui::with_stderr(logger, Box::new(|_| {})));
    let sut = NodeSut::new(&dir.0, &argv, mocktime, ui).map_err(|e| format!("NodeSut: {e:?}"))?;
    let listener = listen(&sut)?;
    Ok(Loaded {
        sut,
        listener,
        dir,
    })
}

/// A new listener on the loaded V's signals, unless the load was refused. What the load queued
/// before it is delivered and dropped: the next op hears its own signals only.
fn listen(sut: &NodeSut) -> Result<Arc<Listener>, String> {
    if let Some(message) = sut.down() {
        return Err(format!("the node's load was refused: {message}"));
    }
    let v = sut.chain().ok_or("the node is loaded without V")?;
    let listener = Arc::new(Listener {
        reader: v.reader().clone(),
        heard: Mutex::default(),
    });
    if !sut.register_listener(Arc::clone(&listener) as Arc<dyn ValidationInterface>) {
        return Err("the node's signals refused the listener".into());
    }
    if !sut.sync_signals() {
        return Err("the node's signals did not sync after the load".into());
    }
    listener.take();
    Ok(listener)
}
