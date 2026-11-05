//! The rdashd side (O2b §5, O2b-3b, CH3a a10): `NodeSut`'s V driven op by op, its results in
//! the step's neutral form ([`Step`]) beside Core's.
//!
//! Only the ops V serves today run (DEC-286): `BLOCK`, `HEADER` and `PROPOSAL` through
//! [`shape`]'s port of Core's handlers (a proposal's checks and its `TestBlockValidity` in V's one
//! `propose_block` command, as under the handler's one `cs_main` hold), `INVALIDATE`,
//! `RECONSIDER` and `PRECIOUS` through V's block RPC commands, `ACTIVATE`, `CL_RECORD`
//! (`NodeSut::cl_record`, every signature taken as valid as the shim's session takes it, O2b
//! D12), `CL_ENFORCE`, `SPORK`, `FLUSH` (but `INDEX_ONLY`), `RESTART` and `SETTIME`. `FLUSH
//! INDEX_ONLY` is [`Unsupported`] before anything runs, never a made-up reply: it has no V
//! counterpart (`FlushSite` takes Core's four modes, and no call forces the index-only write the
//! shim's seam does, O2b D31).
//!
//! A step's planes:
//! - **reply**: the shaped result, and a `BlockChecked` list of every `BlockChecked` V emitted
//!   during the op, in order. `RESTART` and a restore reply as the shim's `LoadReply`: the load's
//!   status and Core's raw message (`NodeSut`'s typed load result).
//! - **records**: `NodeSut`'s records tap (CH3a a10-3): each connect attempt's `C` or `R` with
//!   its `A`, each disconnect's `D`, as Core's patch records them in memory, but the golden keys;
//!   each open's `H` first.
//! - **events**: a listener on V's validation signals records Core's tuples as the shim's
//!   `EventLog` does (`src/test/oracle/oracle_chain.cpp:211-297` at the pin), except
//!   `NotifyMasternodeListChanged`, which V does not emit (no counterpart yet). The shim drains
//!   Core's queue on its own thread at the op's end, so Core lists the synchronous signals as
//!   they fired and then the queued ones (`UpdatedBlockTip`, `BlockConnected`,
//!   `BlockDisconnected`, `ChainStateFlushed`, `NotifyChainLock`) in queue order. The listener
//!   keeps the two apart and joins them the same way, and gives the runner the count of queued
//!   ones ([`NodeStep::facts`]): where Core may also have drained mid-op, [`super::diff`]
//!   would refuse the op if Core's step queued as many and were otherwise the same.
//!
//!   Every V of the session gets its listener as it starts, before its load verifies anything
//!   (`NodeSut`'s per-open listener), as the shim registers its log before `LoadChainstate`: a
//!   `RESTART` hears the old V's teardown, the new V's load and its `ActivateBestChain`, and a
//!   refused load's teardown. What was heard before a V started comes first, in the order heard:
//!   Core drains the old session's queue in its teardown, before the next load. Each op hears
//!   its own signals only: a step ends once V settled
//!   (W's releases and the flushes they leave, `NodeSut::settle`) and the queue drained, so
//!   nothing is left to reach the next op.
//! - **post**: `down`, `tip`, `best_header`, `best_invalid` and `candidates` (from ABI 33, as the
//!   shim writes them), `utxo` (the shim's `Utxo()` digest, over V's coins cache and the database
//!   below it), `index` (the shim's `Index()` digest), `ibd`, `best_cl`, `sporks`, `tips` and
//!   `deployments`, read as the shim's `FinishReport` reads them: once V settled and the signal
//!   queue drained. `persisted` on a `RESTART` and a restore: the storage between the teardown
//!   and the load, read before any load logic ([`persisted`], O2b D37). `flushes` and
//!   `flush_delta` from `NodeSut`'s tally (CH3a a10-2): each job V's flush side completed, by
//!   Core's cause names or a FLUSH op's label, booked once W released it, as the shim books
//!   Core's after its write; `flushes` since the last load, `flush_delta` since the op began,
//!   across a teardown and the reopen.
//!
//! `NodeSut`'s V connects and disconnects through ND1's `CoreConnector` (CH3a a5's
//! `DisconnectBlock`) and flushes through `NodeFlush` over `NodeBlockFiles`
//! (`dashd-node/src/chainstate.rs`), in the `CoreCausesOnly` regime: only Core's flush causes
//! flush (leg A, S0/S1). It checks blocks, writes their undo data and reads it back to
//! disconnect, so a connect-time rejection, a reorg and the `index` digest's undo bits are
//! compared as they are, and the coins each leaves in `utxo`.
//!
//! V stopping is reported as it happens: the op's reply says V stopped (also when V stopped in
//! work the op left deferred), the post is down, and every later op but `SETTIME` and `RESTART`
//! replies `session-down`, as a down session's.
//!
//! V enforces ChainLocks at `CL_ENFORCE` only (`ClSchedule::OnEnforceOnly`), as the shim's
//! handler, which schedules nothing after a lock and hears no tip change (O2b D2, D12).
//!
//! The session's image is `NodeSut`'s (`SutImage`), saved as `CoreSut::start` saves Core's: a
//! clean teardown, the coins and the data directory's files, the reload. A restore is Core's: a
//! kill, the image written back, the load ([`NodeSide::restore`]).

use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use dash_chain::{ChainReader, HeaderBatch, HeaderStop, Status};
use dash_consensus::coins::tx_out_ser;
use dash_consensus::deployments::info::DeploymentInfo;
use dash_consensus::deployments::{Deployments, NoEhfSignals, VersionBitsCache};
use dash_consensus::primitives::header_bytes;
use dash_consensus::{Block, BlockId, BlockValidationError, Header};
use dash_evo::spork::{SPORK_DEFS, SporkId};
use dash_num::Hash256;
use dash_oracle_sys::{
    BlockAt, Candidate, ChainHash, ChainOp, CheckResult, Checked, Event, FlushCounts, FlushMode,
    Persisted, Pin, Post, Reply, RestartKind, RpcError, SporkState, Utxo,
};
use dash_params::ChainSetup;
use dash_primitives::BlockHash;
use dash_statedigest::Record;
use dash_types::Numeric;
use dashd_chainstate::chain::{
    ActorReply, BlockNotFound, BlockState, ChainHandle, ChainQuery, ClSchedule, FlushStateMode,
    NewBlock, NotTested, ValidationInterface,
};
use dashd_chainstate::coins::{OutPointKey, PackedCoin};
use dashd_chainstate::flush::{CauseCounts, FlushRegime};
use dashd_chainstate::reorg::PoolsOnly;
use dashd_chainstate::store::{CoinsReadError, Stored};
use dashd_node::logging::Logger;
use dashd_node::steps::ListenerFactory;
use dashd_node::sut::{Harness, LoadResult, NodeSut, SutImage, Teardown};
use dashd_node::ui::Ui;
use serde_json::{Value, json};

use super::core::{SPORK_19, Schedule, T_RECENT, image_world};
use super::diff::{NodeFacts, QUEUED};
use super::params;
use super::run::{Refused, Unsupported};
use super::shape::{self, Known, Node, Outcome, Processed, TipRow, block_hash_hex};
use super::step::Step;
use super::world::World;

/// The shim writes `best_invalid` and `candidates` from ABI 33 (OD-L3-15): the node's post
/// carries them as the linked shim's does.
const REPORTS_CANDIDATES: bool = dash_oracle_sys::ffi::ABI_VERSION >= 33;

/// The index status bits the post-state's digest covers (O2b §4.5): validity, data, undo, the
/// failure bits and the ChainLock conflict.
// Core: src/test/oracle/oracle_chain.cpp:108-109 at the pin
pub const INDEX_STATUS_MASK: u32 = Status::VALID_MASK
    | Status::HAVE_DATA
    | Status::HAVE_UNDO
    | Status::FAILED_MASK
    | Status::CONFLICT_CHAINLOCK;

/// The status bits the persisted image's digest covers (O2b D37).
// Core: src/test/oracle/oracle_chain.cpp:111-112 at the pin
const PERSISTED_STATUS_MASK: u32 = Status::VALID_MASK
    | Status::HAVE_DATA
    | Status::HAVE_UNDO
    | Status::FAILED_MASK
    | Status::CONFLICT_CHAINLOCK;

/// The status bits the persisted image lists per failed entry (I23).
// Core: src/test/oracle/oracle_chain.cpp:118 at the pin
const FAILED_STATUS_MASK: u32 = Status::FAILED_MASK | Status::CONFLICT_CHAINLOCK;

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
    /// What the listeners heard before the latest V started: an earlier V's teardown, whose
    /// queue Core drains before the next load (a clean teardown's `FlushBackgroundCallbacks`),
    /// so it comes first, its signals in the order they were heard.
    // Core: src/test/oracle/oracle_chain.cpp `Teardown` at the shim's ABI 33
    earlier: Vec<Event>,
    /// The synchronous signals, as V emitted them.
    sync: Vec<Event>,
    /// The queued signals, in the queue's order.
    queued: Vec<Event>,
    /// Every `BlockChecked`, in order: the reply's list.
    checked: Vec<(BlockHash, BlockState)>,
    /// The signals an earlier V's listener heard after the latest V started, each listed where
    /// it was heard. Core never logs one: its teardown drains the old session's queue and
    /// unregisters the log before the reopen (Opus r2 m-1).
    // Core: src/test/oracle/oracle_chain.cpp:906-910, :1000-1003 at the pin
    late: usize,
}

/// What every V's listener of a session heard, in one log: a step hears the old V's teardown
/// and the new V's load. `generation` counts the Vs started.
#[derive(Default)]
struct Ears {
    heard: Mutex<Heard>,
    generation: AtomicU64,
}

impl Ears {
    fn lock(&self) -> std::sync::MutexGuard<'_, Heard> {
        self.heard.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn take(&self) -> Heard {
        std::mem::take(&mut *self.lock())
    }

    /// A new V starts: what was heard so far, drained, goes before what it will hear. The new
    /// V's listener takes the generation it returns.
    fn new_v(&self) -> u64 {
        let heard = &mut *self.lock();
        heard.earlier.append(&mut heard.sync);
        heard.earlier.append(&mut heard.queued);
        self.generation.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// `submitblock_StateCatcher`'s state: the last `BlockChecked` of `hash`, if one fired.
    // Core: rpc/mining.cpp:989-1005
    fn last_checked(&self, hash: BlockHash) -> Option<BlockState> {
        self.lock().checked.iter().rev().find(|(h, _)| *h == hash).map(|(_, s)| s.clone())
    }
}

/// The shim's `EventLog` on one V's signals: each signal as `[name, args...]`, hashes as Core's
/// hex, a null fork as null, `BlockChecked` as `[name, hash, res, cls, why]`. Like the shim's,
/// it does not hear `InitializeCurrentBlockTip`.
// Core: src/test/oracle/oracle_chain.cpp:211-297 at the pin
struct Listener {
    /// The reader of the V it listens to.
    reader: ChainReader,
    ears: Arc<Ears>,
    /// The [`Ears`] generation its V started at.
    generation: u64,
}

impl Listener {
    /// The log, for one signal: counted late if a later V has started.
    fn hear(&self) -> std::sync::MutexGuard<'_, Heard> {
        let mut heard = self.ears.lock();
        if self.ears.generation.load(Ordering::Relaxed) != self.generation {
            heard.late += 1;
        }
        heard
    }

    fn hash(&self, id: BlockId) -> Value {
        hash_arg(self.reader.entry(id).expect("a signalled entry is V's").hash)
    }

    fn fork(&self, fork: Option<BlockId>) -> Value {
        fork.map_or(Value::Null, |id| self.hash(id))
    }

    fn add(&self, name: &str, args: Vec<Value>) {
        self.hear().sync.push(event(name, args));
    }

    /// A signal Core queues (`ENQUEUE_AND_LOG_EVENT`).
    // Core: validationinterface.cpp:215-227, :254-278, :309-316
    fn queue(&self, name: &str, args: Vec<Value>) {
        self.hear().queued.push(event(name, args));
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
        let mut heard = self.hear();
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
    ears: &'a Ears,
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
                checked: self.ears.last_checked(hash),
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
            Err(_) => self.stop(),
        }
    }

    /// V stopped before it replied.
    fn stop(&mut self) -> Outcome {
        self.stopped = true;
        rpc_error(V_STOPPED)
    }
}

/// What the post-state reads on V in one class R query (Core reads them under `cs_main`).
struct Indexed {
    tip: BlockAt,
    best_header: BlockAt,
    best_invalid: Option<BlockAt>,
    candidates: Vec<Candidate>,
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

/// The shim's `Persisted()` (O2b D37) over what V's storage held between a teardown and the
/// load: every stored index record (header-only and failed ones included), in Core's key order
/// (`memcmp` of the internal hash bytes), hashed as the hash, `nHeight`, the 80-byte header,
/// `nTx` and `nStatus & PERSISTED_STATUS_MASK`, with Core's integer encodings, in one SHA-256;
/// the records with a failure bit; and the coins' and the evo state's markers, a null
/// `head_blocks` old block as 32 zero bytes, as Core's `uint256()` writes it.
// Core: src/test/oracle/oracle_chain.cpp:596-647 at the pin, chain.h:391-413
pub fn persisted(stored: &Stored) -> Persisted {
    use bitcoin_hashes::{HashEngine, sha256};

    let mut sorted: Vec<_> = stored.index.iter().map(|(h, r)| (chain_hash(*h), r)).collect();
    sorted.sort_unstable_by_key(|a| a.0.0);
    let mut engine = sha256::HashEngine::default();
    let mut failed = Vec::new();
    for (hash, r) in &sorted {
        let status = r.status.bits();
        engine.input(&hash.0);
        engine.input(&i32::try_from(r.height).expect("nHeight is an int").to_le_bytes());
        engine.input(&header_bytes(&r.header));
        engine.input(&r.n_tx.to_le_bytes());
        engine.input(&(status & PERSISTED_STATUS_MASK).to_le_bytes());
        if status & FAILED_STATUS_MASK != 0 {
            failed.push((*hash, status & FAILED_STATUS_MASK));
        }
    }
    let meta = &stored.meta;
    Persisted {
        best_block: meta.tip.map(chain_hash),
        evo_best: meta.evo_tip.map(chain_hash),
        failed,
        head_blocks: meta
            .head_blocks
            .map(|(new, old)| vec![chain_hash(new), old.map_or(ChainHash([0; 32]), chain_hash)]),
        index: ChainHash(sha256::Hash::from_engine(engine).to_byte_array()),
        index_entries: sorted.len() as u64,
    }
}

/// `arith_uint256::GetHex()`: the big-endian work, 64 lowercase hex digits.
fn work_hex(work: &dash_chain::Work) -> String {
    work.to_bendian().iter().map(|b| format!("{b:02x}")).collect()
}

/// The shim's `best_invalid` (`m_best_invalid`) over V's index.
// Core: src/test/oracle/oracle_chain.cpp:767-768 at the shim's ABI 33
fn best_invalid(q: &ChainQuery<'_>, reader: &ChainReader) -> Option<BlockAt> {
    let id = q.idx.best_invalid()?;
    let e = reader.entry(id).expect("the arena holds it");
    Some(block_at(e.height, e.hash))
}

/// The shim's `Candidates()` over V's set, which is Core's `setBlockIndexCandidates` with its
/// comparator (`CandKey`: work, then the later sequence id, then the arena id where Core takes
/// the address), least work first. A run of equal work and sequence id is ordered by the hash's
/// RPC hex, as the shim orders it: neither side's address order is the other's.
// Core: src/test/oracle/oracle_chain.cpp:723-745 at the shim's ABI 33
fn candidates(q: &ChainQuery<'_>, reader: &ChainReader) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = q
        .idx
        .candidates()
        .map(|id| {
            let e = reader.entry(id).expect("a candidate is V's");
            let height = i32::try_from(e.height).expect("a regtest height");
            // Unmasked, unlike the shim's: L3's P7 fails a bit outside the mask (OD-L3-72 n3).
            let status = e.status.bits();
            Candidate(
                height,
                chain_hash(e.hash),
                q.idx.sequence_id(id),
                status,
                work_hex(&e.chain_work),
            )
        })
        .collect();
    for run in out.chunk_by_mut(|a, b| (&a.4, a.2) == (&b.4, b.2)) {
        // The RPC hex orders as the reversed internal bytes.
        run.sort_by_key(|c| {
            let mut rpc = c.1.0;
            rpc.reverse();
            rpc
        });
    }
    out
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
    /// A proposal's: the class of the invalid state V's `TestBlockValidity` reached
    /// ([`shape::proposal_checked`]), in `REJECT_CLASSES`' spelling, for L3's class plane (Core's
    /// proposals signal no `BlockChecked`).
    pub proposal_class: Option<&'static str>,
}

/// Where a side's node log lines go, each as the logger formats it (L3's warn leg reads
/// `UpdateTip`).
pub type LogSink = Arc<dyn Fn(&str) + Send + Sync>;

/// The rdashd side of a `diff_chain` session: one `NodeSut` on the schedule's arguments, with the
/// image's chain built into it and saved; or, for L3, on any argument list, at genesis. Every
/// reopen (`RESTART`, a restore) is the same `NodeSut`'s, which keeps its arguments, its log and
/// the listener factory.
pub struct NodeSide {
    sut: NodeSut,
    params: Arc<ChainSetup>,
    /// What every V's listener heard.
    ears: Arc<Ears>,
    /// The saved image; `None` on a [`with_argv`](NodeSide::with_argv) side.
    image: Option<SutImage>,
    /// V stopped: the session is down until the next `RESTART` or restore.
    stopped: bool,
    /// The running op's proposal class, for its [`NodeStep`].
    proposal_class: Option<&'static str>,
    /// The records the tap held when the running op began ([`NodeFacts::late_records`]).
    late_records: usize,
    /// Records taken as the tap's ahead of what it wrote: a test's planted late record.
    #[cfg(test)]
    planted: Mutex<Vec<Record>>,
    /// Declared last: removed once the node is gone.
    dir: LoadDir,
}

/// The schedules whose D8 check passed in this process.
static CHECKED: Mutex<Vec<(Pin, Schedule)>> = Mutex::new(Vec::new());

/// D8's check for `schedule`, once per process. The oracle refuses `oracle_chainparams_json`
/// while this thread holds a chain session, so it runs before [`CoreSut::start`]
/// ([`super::core::CoreSut::start`]).
fn check_params(pin: Pin, schedule: Schedule, running: &ChainSetup) -> Result<(), String> {
    let mut checked = CHECKED.lock().unwrap_or_else(|p| p.into_inner());
    if !checked.contains(&(pin, schedule)) {
        params::check(pin, schedule, running)?;
        checked.push((pin, schedule));
    }
    Ok(())
}

/// The op kinds V serves (DEC-286): all but `FLUSH INDEX_ONLY` (no V counterpart, O2b D31).
fn served(op: &ChainOp) -> bool {
    match op {
        ChainOp::Flush {
            mode,
            ..
        } => *mode != FlushMode::IndexOnly,
        _ => true,
    }
}

impl NodeSide {
    /// A `NodeSut` on `["-regtest", "-printtoconsole=0", schedule.args()...]` at [`T_RECENT`] in
    /// a subdirectory of `dir`, after D8's check of its parameters; then the image's blocks and
    /// spork 19 on, checked active, and the image saved, as
    /// [`CoreSut::start`](super::core::CoreSut::start) builds Core's. Run it before Core's
    /// session starts on this thread (D8's check needs the oracle free). The world it returns is
    /// the image's.
    pub fn start(pin: Pin, schedule: Schedule, dir: &Path) -> Result<(NodeSide, World), String> {
        let world = image_world();
        let argv: Vec<String> = schedule.args().into_iter().map(str::to_owned).collect();
        let mut side = open(dir, &argv, T_RECENT, None)?;
        if let Some(message) = side.sut.down() {
            return Err(format!("the node's load was refused: {message}"));
        }
        check_params(pin, schedule, side.sut.params())?;
        side.build_image(&world)?;
        Ok((side, world))
    }

    /// A `NodeSut` on `[network_arg, "-printtoconsole=0", chain_args...]` at `mocktime` in a
    /// subdirectory of `dir`, at genesis: no image, no D8 check (L3's legs run
    /// [`params::check_args`] before any session opens), and the node's log lines to `log`. `Ok`
    /// whether the load succeeded or was refused, which [`down`](Self::down) then says; `Err`
    /// only for arguments `NodeSut` refuses before any load (OD-L3-16).
    ///
    /// # Panics
    ///
    /// On `-dbcrashratio`, whose simulated crash is out of the shim's domain (Opus r1 n-5): the
    /// node's load would report a `failure` the shim never runs.
    pub fn with_argv(
        chain_args: &[String],
        mocktime: i64,
        dir: &Path,
        log: Option<LogSink>,
    ) -> Result<NodeSide, String> {
        let crash =
            |a: &String| a.trim_start_matches('-').split('=').next() == Some("dbcrashratio");
        assert!(!chain_args.iter().any(crash), "-dbcrashratio is out of domain: {chain_args:?}");
        open(dir, chain_args, mocktime, log.as_ref())
    }

    /// Why the side is down: the refused load's message, [`V_STOPPED`] once V stopped, `None`
    /// while V serves.
    pub fn down(&self) -> Option<&str> {
        match self.sut.down() {
            Some(message) => Some(message),
            None if self.stopped => Some(V_STOPPED),
            None => None,
        }
    }

    /// The chain parameters V runs on, for L3's D8 check ([`params::check_args`]).
    pub fn params(&self) -> &ChainSetup {
        &self.params
    }

    /// V's E-evo gate counts (L3, plan §3.6) by key, for its loaded chainstate: `None` when no
    /// counted EV1 checks are registered under the session's directory.
    pub fn evo_gates(&self) -> Option<std::collections::BTreeMap<&'static str, u64>> {
        dashd_chainstate::evo::gates::counts_under(&self.dir.0).map(|c| c.snapshot())
    }

    /// V's coins, the cache laid over the database ([`NodeSut::tip_coins`]), while V serves.
    pub fn tip_coins(&self) -> Option<Result<Vec<(OutPointKey, PackedCoin)>, CoinsReadError>> {
        self.v()?;
        self.sut.tip_coins()
    }

    /// V's concise warning (`GetWarnings(false)`), while V serves.
    pub fn warnings(&self) -> Option<String> {
        self.v().map(|_| self.sut.warnings())
    }

    /// The input's start (step 0, O2b-1 I24): the clock at `mocktime`, then the image restored as
    /// the shim's `oracle_chain_restore` restores it (a kill, the image written back, the load);
    /// its step replies as `RESTART`'s, with the persisted image. A
    /// [`with_argv`](Self::with_argv) side has no image to restore; a refused load is an error.
    // Core: src/test/oracle/oracle_chain.cpp:1311-1328 at the pin
    pub fn restore(&mut self, mocktime: i64) -> Result<NodeStep, String> {
        let Some(image) = &self.image else {
            return Err("no image".into());
        };
        self.ears.take();
        self.late_records = self.take_records().len();
        self.sut.begin_op();
        let restored = self.sut.restore(image, mocktime).map_err(|e| format!("restore: {e}"))?;
        self.stopped = false;
        if let Some(message) = self.sut.down() {
            return Err(format!("the node's load was refused: {message}"));
        }
        let persisted = Some(read_persisted(&restored.persisted));
        Ok(self.finish(load_reply(&restored.load), persisted))
    }

    /// The image's blocks, each connected, then spork 19 on, then the save: a clean teardown,
    /// the image, the reload.
    fn build_image(&mut self, world: &World) -> Result<(), String> {
        for b in &world.blocks[1..] {
            let s = self.image_step(&ChainOp::Block(b.built_bytes()))?;
            if s.reply.outcome != Ok(Value::Null) {
                return Err(format!("image block {} not connected: {:?}", b.height, s.reply));
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
        let image = self.sut.save_image().map_err(|e| format!("the node's image save: {e}"))?;
        if let Some(message) = self.sut.down() {
            return Err(format!("the node's image reload was refused: {message}"));
        }
        self.image = Some(image);
        self.settled();
        self.ears.take();
        self.take_records();
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
            self.sut.chain()
        }
    }

    /// V settled and its queue drained (`NodeSut::settle`), unless it serves no more.
    fn settled(&mut self) {
        if self.v().is_some() && self.sut.settle().is_none() {
            self.stopped = true;
        }
    }

    /// One op, or [`Unsupported`] before anything runs, for an op kind V does not serve. How many
    /// queued signals it delivered comes with the step for the runner to hold against Core's
    /// ([`super::diff::judge`]), and so do the records the tap held when it began, which the
    /// runner holds diverged ([`NodeFacts::late_records`]; [`restore`](Self::restore)'s too).
    pub fn step(&mut self, op: &ChainOp) -> Result<NodeStep, Unsupported> {
        if !served(op) {
            return Err(Unsupported::new(op, Refused::Op));
        }
        self.ears.take();
        self.late_records = self.take_records().len();
        self.sut.begin_op();
        self.proposal_class = None;
        let mut persisted = None;
        let outcome = match op {
            ChainOp::SetTime(t) => {
                // Core: src/test/oracle/oracle_chain.cpp:1032-1035 at the pin (also while down)
                self.sut.clock().set_mocktime(*t);
                Ok(Value::Null)
            }
            // A teardown unless down, the persisted image, the load (also while down).
            // Core: src/test/oracle/oracle_chain.cpp:940-953, :1028-1031 at the pin
            ChainOp::Restart(kind) => {
                let how = match kind {
                    RestartKind::Clean => Teardown::Clean,
                    RestartKind::Kill => Teardown::Kill,
                };
                let restarted = self.sut.restart_report(how);
                self.stopped = false;
                persisted = Some(read_persisted(&restarted.persisted));
                load_reply(&restarted.load)
            }
            _ if self.v().is_none() => rpc_error(SESSION_DOWN),
            // Core: src/test/oracle/oracle_chain.cpp:1051-1056 at the pin
            ChainOp::ClRecord {
                height,
                hash,
                sig,
            } => match self.sut.cl_record(*height, BlockHash::from(*hash), *sig) {
                Some(recorded) => Ok(recorded.name().into()),
                None => {
                    self.stopped = true;
                    rpc_error(V_STOPPED)
                }
            },
            _ => self.run(op),
        };
        Ok(self.finish(outcome, persisted))
    }

    /// A supported op other than `SETTIME` on a serving V.
    fn run(&mut self, op: &ChainOp) -> Outcome {
        let v = self.sut.chain().expect("serving");
        let mut on_v = OnV {
            v,
            ears: &self.ears,
            stopped: false,
        };
        let outcome = match op {
            ChainOp::Block(bytes) => shape::submit_block(bytes, &mut on_v),
            ChainOp::Header(bytes) => shape::submit_header(bytes, &mut on_v),
            ChainOp::Proposal(bytes) => {
                let (outcome, invalid) = shape::proposal_checked(bytes, &mut on_v);
                self.proposal_class = invalid.as_ref().map(shape::reject_class);
                outcome
            }
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
                Ok(Err(_)) | Err(_) => on_v.stop(),
            },
            // The shim's reply: `{ok, state}` (V replies only when the flush succeeded, whose
            // state is valid). A label books the op's flushes under it in place of their causes.
            // Core: src/test/oracle/oracle_chain.cpp:1009-1027 at the pin
            ChainOp::Flush {
                mode,
                label,
            } => {
                let label = (!label.is_empty()).then(|| label.as_str().into());
                match v.flush_labelled(flush_state_mode(*mode), label).recv() {
                    Ok(()) => Ok(json!({"ok": true, "state": "Valid"})),
                    Err(_) => on_v.stop(),
                }
            }
            // `CheckActiveState` then `EnforceBestChainLock`: the handler's timer task.
            // Core: src/test/oracle/oracle_chain.cpp:999-1003 at the pin
            ChainOp::ClEnforce => match v.chainlock_timer() {
                Ok(()) => Ok(Value::Null),
                Err(_) => on_v.stop(),
            },
            // `UpdateSpork` with the session key: whether it was signed.
            // Core: src/test/oracle/oracle_chain.cpp:1004-1008 at the pin
            ChainOp::Spork {
                id,
                value,
            } => {
                let sporks = self.sut.sporks().expect("loaded");
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
    /// deferred work), settled (W's releases and the flushes they leave), the signal queue
    /// drained, then the post read, with `persisted` for a load. The events are the synchronous
    /// ones, then the queued ones; the count of queued ones comes with the step. A V that
    /// stopped since the op began makes the reply [`V_STOPPED`], as the post is down.
    // Core: src/test/oracle/oracle_chain.cpp:1043-1049 at the pin
    fn finish(&mut self, mut outcome: Outcome, persisted: Option<Persisted>) -> NodeStep {
        let serving = self.v().is_some();
        if self.v().is_some_and(|v| v.query(|_| ()).recv().is_err()) {
            self.stopped = true;
        }
        // A test thread carries no no-wait marker. A V that cannot settle, or a queue that cannot
        // sync, would leave the queued events incomplete: V is taken as stopped, as for a failed
        // fence.
        self.settled();
        let mut post = self.post();
        post.persisted = persisted;
        if serving && self.stopped {
            outcome = rpc_error(V_STOPPED);
        }
        let heard = self.ears.take();
        let checked = heard.checked.iter().map(|(hash, state)| shape::checked(*hash, state));
        let (teardown, late) = (heard.earlier.len(), heard.late);
        let mut events = heard.earlier;
        events.extend(heard.sync);
        events.extend(heard.queued);
        let queued = events.iter().filter(|e| QUEUED.contains(&e.name.as_str())).count();
        let step = Step {
            reply: Reply {
                checked: checked.collect(),
                outcome,
            },
            records: self.take_records(),
            events,
            post,
        };
        NodeStep {
            step,
            facts: NodeFacts {
                queued,
                teardown,
                late,
                late_records: std::mem::take(&mut self.late_records),
            },
            proposal_class: self.proposal_class.take(),
        }
    }

    /// The post-state (O2b §4.5), in two class R queries on V with nothing sent between them:
    /// the index's reads, then the coins ([`NodeSut::tip_coins`]). Either failing is V stopped.
    /// The flush planes are the tally's ([`NodeSut::tally`]), `flush_delta` while down too.
    // Core: src/test/oracle/oracle_chain.cpp:723-737 at the pin
    fn post(&mut self) -> Post {
        let tally = self.sut.tally();
        let mut post = Post {
            down: true,
            flush_delta: flush_counts(tally.delta()),
            tie: Vec::new(),
            persisted: None,
            tip: None,
            best_header: None,
            best_invalid: None,
            candidates: None,
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
        let Some(coins) = self.sut.tip_coins() else {
            self.stopped = true;
            return post;
        };
        let view = v.view();
        post.down = false;
        post.utxo = Some(utxo_digest(&coins.expect("the in-memory coins")));
        post.tip = Some(read.tip);
        post.best_header = Some(Some(read.best_header));
        if REPORTS_CANDIDATES {
            post.best_invalid = Some(read.best_invalid);
            post.candidates = Some(read.candidates);
        }
        post.index = Some(read.index);
        post.flushes = Some(flush_counts(self.sut.tally().since_open()));
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
        self.sut.register_listener(listener)
    }

    /// V's `m_best_invalid` while V serves, as the post's `best_invalid` (OD-L3-15).
    pub fn best_invalid(&self) -> Option<Option<BlockAt>> {
        self.read(best_invalid)
    }

    /// V's `setBlockIndexCandidates` while V serves, as the post's `candidates` (OD-L3-15).
    pub fn candidates(&self) -> Option<Vec<Candidate>> {
        self.read(candidates)
    }

    /// V's index entries while V serves: what [`index_digest`] hashes, for the tests that hold
    /// it to Core's.
    pub fn index(&self) -> Option<Vec<IndexEntry>> {
        self.read(index_entries)
    }

    /// `read` on V's index in one class R query, while V serves.
    fn read<T: Send + 'static>(&self, read: fn(&ChainQuery<'_>, &ChainReader) -> T) -> Option<T> {
        let v = self.v()?;
        let reader = v.reader().clone();
        v.query(move |q| read(q, &reader)).recv().ok()
    }

    /// The loaded node's blocks directory, for tests that damage a block file where Core's
    /// session has no way to (CH3 §7.6's failure sites).
    pub fn blocks_dir(&self) -> PathBuf {
        self.dir.0.join("regtest").join("blocks")
    }

    /// V's disconnect pools over the session's opens (`NodeSut::pools`, DEC-500): what they
    /// evicted and V's calls on them. Core's session keeps no pool, so the suites read V's alone.
    pub fn pools(&self) -> &PoolsOnly {
        self.sut.pools()
    }

    /// The records the tap wrote since the last take: a line that does not parse is the tap's
    /// bug.
    fn take_records(&self) -> Vec<Record> {
        #[cfg(test)]
        let planted = std::mem::take(&mut *self.planted.lock().expect("unpoisoned"));
        #[cfg(not(test))]
        let planted = Vec::new();
        [planted, self.sut.take_records().expect("the records tap writes O8 lines")].concat()
    }

    /// `(id, value, active)` of every spork in Core's `sporkDefs`, in its order.
    // Core: src/test/oracle/oracle_chain.cpp:706-714 at the pin, spork.h:66-72
    fn sporks(&self) -> Vec<SporkState> {
        let sporks = self.sut.sporks().expect("loaded");
        SPORK_DEFS
            .iter()
            .map(|d| SporkState(d.id.0, sporks.value(d.id), sporks.is_active(d.id)))
            .collect()
    }
}

/// The post-state's index reads, on V.
fn indexed(q: &ChainQuery<'_>, reader: &ChainReader, params: &ChainSetup) -> Indexed {
    let snap = reader.snapshot();
    let facts = &snap.tip;
    Indexed {
        tip: block_at(facts.tip_height, facts.tip_hash),
        best_header: block_at(facts.best_header_height, facts.best_header_hash),
        best_invalid: best_invalid(q, reader),
        candidates: candidates(q, reader),
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
fn deployment_infos(q: &ChainQuery<'_>, params: &ChainSetup, tip: BlockId) -> Vec<DeploymentInfo> {
    let chain = q.chain.complete().expect("V's chain is the node's: no window");
    let cache = VersionBitsCache::new();
    Deployments::new(params, chain, &NoEhfSignals, &cache)
        .deployment_info(tip)
        .expect("NoEhfSignals cannot fault")
}

/// The tally's counts as the post carries them.
fn flush_counts(counts: CauseCounts) -> FlushCounts {
    FlushCounts {
        full: counts.full,
        index_only: counts.index_only,
    }
}

/// A reply that is an error without a code: [`SESSION_DOWN`] or [`V_STOPPED`].
fn rpc_error(message: &str) -> Outcome {
    Err(RpcError {
        code: None,
        message: message.to_owned(),
    })
}

/// The network argument a node gets before `args`: `-regtest`, unless `args` name a network
/// themselves (`-devnet=<name>`, L3's devnet leg, OD-L3-22), with or without a value
/// (`-regtest=1`): Core refuses a second network argument.
pub fn network_arg(args: &[String]) -> Option<&'static str> {
    let named = args.iter().any(|a| {
        ["-regtest", "-testnet", "-devnet"].iter().any(|net| {
            a.strip_prefix(net).is_some_and(|rest| rest.is_empty() || rest.starts_with('='))
        })
    });
    (!named).then_some("-regtest")
}

/// `RESTART`'s and a restore's reply: the shim's `LoadReply`.
// Core: src/test/oracle/oracle_chain.cpp:950-953 at the pin
fn load_reply(load: &LoadResult) -> Outcome {
    Ok(json!({"status": load.status.name(), "message": load.message}))
}

/// The persisted image a load's report carries. The read is of the in-memory coins, through a
/// guard with no fault armed: an error there is the harness's.
fn read_persisted(stored: &Result<Stored, dashd_store::StoreError>) -> Persisted {
    persisted(stored.as_ref().expect("the persisted image reads"))
}

/// A FLUSH op's mode as V's `Flush` command takes it (not `INDEX_ONLY`, which is not served).
fn flush_state_mode(mode: FlushMode) -> FlushStateMode {
    match mode {
        FlushMode::Always => FlushStateMode::Always,
        FlushMode::IfNeeded => FlushStateMode::IfNeeded,
        FlushMode::Periodic => FlushStateMode::Periodic,
        FlushMode::IndexOnly => unreachable!("not served"),
    }
}

/// A side on a `NodeSut` on `[network_arg, "-printtoconsole=0", args...]` at `mocktime` in a
/// private subdirectory of `dir` (removed with the side, or if the open fails), in the
/// `CoreCausesOnly` regime, its log lines to `log`, and every V's listener on one log. What the
/// load signalled is delivered and dropped: the first op hears its own signals only. `Ok` with a
/// refused load too; `Err` only for arguments `NodeSut` refuses before any load. The one open
/// path of [`NodeSide::start`] and [`NodeSide::with_argv`].
fn open(
    dir: &Path,
    args: &[String],
    mocktime: i64,
    log: Option<&LogSink>,
) -> Result<NodeSide, String> {
    let dir = dir.join("node");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    let dir = LoadDir(dir);
    let mut argv: Vec<&str> = network_arg(args).into_iter().collect();
    argv.push("-printtoconsole=0");
    argv.extend(args.iter().map(String::as_str));
    let logger = Arc::new(Logger::new());
    // No file and no console: without `log`, nothing is logged, and nothing buffered for the
    // session's life.
    if let Some(log) = log {
        let log = Arc::clone(log);
        logger.push_back_callback(move |line| log(line));
    }
    if !logger.start_logging() {
        return Err("the node's logger did not start".into());
    }
    let ui = Arc::new(Ui::with_stderr(logger, Box::new(|_| {})));
    let ears = Arc::new(Ears::default());
    let heard = Arc::clone(&ears);
    let listener: Arc<ListenerFactory> = Arc::new(move |reader: &ChainReader| {
        let generation = heard.new_v();
        Arc::new(Listener {
            reader: reader.clone(),
            ears: Arc::clone(&heard),
            generation,
        }) as Arc<dyn ValidationInterface>
    });
    let harness = Harness {
        listener: Some(listener),
        regime: FlushRegime::CoreCausesOnly,
        cl_schedule: ClSchedule::OnEnforceOnly,
        records: true,
    };
    let sut = NodeSut::with_harness(&dir.0, &argv, mocktime, ui, harness)
        .map_err(|e| format!("NodeSut: {e:?}"))?;
    let mut side = NodeSide {
        params: Arc::new(sut.params().clone()),
        sut,
        ears,
        image: None,
        stopped: false,
        proposal_class: None,
        late_records: 0,
        #[cfg(test)]
        planted: Mutex::default(),
        dir,
    };
    side.settled();
    side.ears.take();
    side.take_records();
    Ok(side)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::build::{self, BlockPlan, GENESIS_TIME, REGTEST_BITS, VERSION_BITS_TOP};
    use crate::chain::diff::{Judgement, judge};

    /// OD-L3-72 n5: a network named in `args`, bare or with a value, takes the place of
    /// `-regtest`; anything else gets it.
    #[test]
    fn a_named_network_takes_the_regtest_arguments_place() {
        let arg =
            |args: &[&str]| network_arg(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>());
        for named in [
            &["-regtest"][..],
            &["-regtest=1"],
            &["-testnet"],
            &["-testnet=1"],
            &["-devnet"],
            &["-devnet=l3"],
            &["-par=4", "-devnet=l3"],
        ] {
            assert_eq!(arg(named), None, "{named:?}");
        }
        for unnamed in [&[][..], &["-par=4"], &["-regtestx"], &["-devnetname=x"], &["-testnetwork"]]
        {
            assert_eq!(arg(unnamed), Some("-regtest"), "{unnamed:?}");
        }
    }

    /// Opus r1 m-2: a record the tap wrote before a step or a restore began is not dropped: the
    /// step counts it ([`NodeFacts::late_records`]), and the runner holds the step diverged in
    /// its records. A step that begins on an empty tap counts none.
    #[test]
    fn a_record_written_before_a_step_or_a_restore_is_held_diverged() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let (mut side, _) = NodeSide::start(Pin::V24_0, Schedule::S0, dir.path()).expect("a side");
        let late = || {
            let id = dash_statedigest::Ident {
                h: 1,
                b: dash_statedigest::Hash256::from_wire_bytes([7; 32]),
                org: dash_statedigest::Origin::Tip,
            };
            let d = dash_statedigest::emit::disconnect(id, "ok", 1);
            Record::Outcome(d.expect("a record of the spec's shape"))
        };
        let clean = side.restore(T_RECENT).expect("a restore");
        assert_eq!(clean.facts.late_records, 0);
        let step = side.step(&ChainOp::SetTime(T_RECENT)).expect("served");
        assert_eq!(step.facts.late_records, 0);
        side.planted.lock().expect("unpoisoned").push(late());
        let restored = side.restore(T_RECENT).expect("a restore");
        assert_eq!(restored.facts.late_records, 1, "at a restore");
        side.planted.lock().expect("unpoisoned").push(late());
        let step = side.step(&ChainOp::SetTime(T_RECENT)).expect("served");
        assert_eq!(step.facts.late_records, 1, "at a step");
        let diverged = Judgement::Diverged(crate::Divergence {
            field: "records",
        });
        assert_eq!(judge(&step.step, &step, &[]), diverged);
        assert_eq!(judge(&restored.step, &restored, &[]), diverged);
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| (*a).to_owned()).collect()
    }

    /// Arguments `NodeSut` refuses before any load are `Err`.
    #[test]
    fn with_argv_refuses_arguments_before_a_load() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let e = NodeSide::with_argv(&args(&["-mocktime=1"]), T_RECENT, dir.path(), None)
            .err()
            .expect("refused");
        assert!(e.starts_with("NodeSut: "), "{e}");
    }

    /// A refused load is a side that is down with the load's message (OD-L3-16): it serves no
    /// op, has no coins or warning, and has no image to restore.
    #[test]
    fn with_argv_keeps_a_refused_load() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let mut side =
            NodeSide::with_argv(&args(&["-sporkkey=nonsense"]), T_RECENT, dir.path(), None)
                .expect("a load");
        assert_eq!(side.down(), Some("Unable to sign spork message, wrong key?"));
        assert!(side.tip_coins().is_none() && side.warnings().is_none());
        let step = side.step(&ChainOp::Invalidate(build::genesis_hash())).expect("served").step;
        assert_eq!(step.reply.outcome, rpc_error(SESSION_DOWN));
        assert!(step.post.down);
        assert_eq!(side.restore(T_RECENT), Err("no image".to_owned()));
    }

    /// A served side starts at genesis on its arguments, logs to its sink, connects a block,
    /// and has no image to restore.
    #[test]
    fn with_argv_serves_from_genesis() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = Arc::clone(&lines);
        let log: LogSink =
            Arc::new(move |line| sink.lock().expect("unpoisoned").push(line.to_owned()));
        let argv = args(&[
            "-dip3params=2000:2000",
            "-testactivationheight=v20@2000",
            "-testactivationheight=mn_rr@2000",
        ]);
        let mut side = NodeSide::with_argv(&argv, T_RECENT, dir.path(), Some(log)).expect("a load");
        assert_eq!(side.down(), None);
        assert!(!lines.lock().expect("unpoisoned").is_empty(), "the load logged");
        assert_eq!(side.tip_coins().map(|c| c.expect("read").len()), Some(0));
        assert!(side.warnings().is_some());
        let block = BlockPlan {
            prev: build::genesis_hash(),
            version: VERSION_BITS_TOP,
            time: GENESIS_TIME + 150,
            bits: REGTEST_BITS,
            txs: vec![build::coinbase(1, build::subsidy(1), 1)],
            merkle_override: None,
        }
        .build();
        let step = side.step(&ChainOp::Block(block.bytes)).expect("served").step;
        assert_eq!(step.reply.outcome, Ok(Value::Null));
        assert_eq!(step.post.tip.map(|t| t.0), Some(1));
        assert_eq!(side.tip_coins().map(|c| c.expect("read").len()), Some(1));
        assert_eq!(side.restore(T_RECENT), Err("no image".to_owned()));
        assert_eq!(side.down(), None, "the load stays");
    }
}
