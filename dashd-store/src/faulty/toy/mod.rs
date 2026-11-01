//! `ToyChain` (design §6.2): A1 §3.5 in miniature, with errata E1–E22, over ST2's real
//! `BlockFiles` on `FaultyFlatFiles` and `FaultyEngine` chainstate and subscriber databases.
//!
//! - **V** accepts blocks (block data, an index entry with `HAVE_DATA`) and connects them (undo,
//!   `HAVE_UNDO | VALID`, the coins and evo state in memory, immediate effects).
//! - **The flush** (DEC-64; §6.2 steps 0–12): the files barrier, the presumed-effect barrier, B1
//!   (index, file infos, evo, the marker; or the whole flush in one batch), `ChainStateFlushed`,
//!   the subscriber commit step, the prune selection, coin sub-batches, the pre-Bk barrier, Bk,
//!   then the unlink.
//! - **Recovery** (§5.5): `sync_wal` on every engine, prune locks, the block files from the
//!   durable infos, the startup scan, the index, the coins-only blind replay with its pre-final
//!   barrier and Durable final batch, activation, then each subscriber's blind rewind and
//!   catch-up.

pub mod block;
pub mod reference;
pub mod sub;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use crate::faulty::toy::block::NULL;
use crate::faulty::toy::reference::{Gen, ToyRef, coins_digest, evo_fold, sha};
use crate::flatfile::{
    BLOCK_SERIALIZATION_HEADER_SIZE, BLOCKFILE_CHUNK_SIZE, BlockFileInfo, BlockFiles,
    BlockFilesConfig, BlockFilesState, FlatFilePos, FlatFs, FlatRead, MIN_BLOCKS_TO_KEEP,
    UNDO_CHECKSUM_SIZE, UNDOFILE_CHUNK_SIZE,
};
use dash_kv::codec::ValueCodec;
use dash_kv::faulty::FaultyEngine;
use dash_kv::faulty::FaultyFlatFiles;
use dash_kv::faulty::{Abort, FatalCause, Sut, allowed_tips};
use dash_kv::faulty::{
    FileRef, InvariantId, Peek, RefExtractor, Requirement, Scope, Violation, WorldDurableView, hex,
};
use dash_kv::faulty::{FlushCause, Hash, Mark, PruneDue, ScriptCheck, Trace, World};
use dash_kv::{Cf, Commit, KvBatch, KvEngine, KvRead};
use dash_kv::{CfProfile, CfSpec, DbSchema};
use sub::{Sub, SubRecovery};
pub use sub::{SubConfig, SubMode};

/// The chainstate database.
pub static CHAIN_SCHEMA: DbSchema = DbSchema {
    name: "chainstate",
    version: 1,
    cfs: &[
        CfSpec::new("meta", CfProfile::Ordered),
        CfSpec::new("block_index", CfProfile::PointLookup),
        CfSpec::new("block_files", CfProfile::Ordered),
        CfSpec::new("coins", CfProfile::PointLookup),
        CfSpec::new("evo", CfProfile::Ordered),
        CfSpec::new("effects", CfProfile::Ordered),
    ],
};
pub const META: Cf = Cf::new(0);
pub const INDEX: Cf = Cf::new(1);
pub const FILES: Cf = Cf::new(2);
pub const COINS: Cf = Cf::new(3);
pub const EVO: Cf = Cf::new(4);
pub const EFFECTS: Cf = Cf::new(5);

const K_TIP: &[u8] = b"tip";
const K_HEAD: &[u8] = b"head_blocks";
const K_EVO_TIP: &[u8] = b"evo_tip";
const K_LAST_FILE: &[u8] = b"last_file";
const K_PRUNED: &[u8] = b"pruned_files";
/// ST2's (Core's) room left under the target for the next allocations.
const PRUNE_BUFFER: u64 = BLOCKFILE_CHUNK_SIZE as u64 + UNDOFILE_CHUNK_SIZE as u64;
const K_STATE: &[u8] = b"state";
const K_SNAP: &[u8] = b"snap";

pub const STATUS_DATA: u8 = 1;
pub const STATUS_UNDO: u8 = 2;
pub const STATUS_VALID: u8 = 4;
pub const STATUS_FAILED: u8 = 8;

const MAGIC: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];
const BLOCKS_DIR: &str = "/dd/blocks";
/// Core's messages.
const UNABLE_TO_REPLAY: &str =
    "Unable to replay blocks. You will need to rebuild the database using -reindex-chainstate.";
const EVO_INCONSISTENT: &str = "Found EvoDB inconsistency, you must reindex to continue";
const FLUSH_FAILED: &str = "Failed to write to coin database";
const FILES_SYNC_FAILED: &str =
    "Flushing block file to disk failed. This is likely the result of an I/O error.";
/// Core's messages for a failed read: the database, and a block or undo record.
const DB_READ_FAILED: &str = "Error reading from database, shutting down.";
const BLOCK_READ_FAILED: &str = "Failed to read block";
const DB_OPEN_FAILED: &str = "Error opening block database";
/// The chainstate engine's store name.
const CHAIN_STORE: &str = "chainstate";
const DISCONNECT_FAILED: &str = "Failed to disconnect block; see debug.log for details";
/// Core's call paths the toy marks ([`Mark::PathBegin`]): their abort messages differ.
const PATH_LOAD: &str = "LoadChainstate";
const PATH_REPLAY: &str = "ReplayBlocks";
const PATH_DISCONNECT: &str = "DisconnectTip";
const PATH_CONNECT: &str = "ConnectTip";

/// The log line after an effect's batch returns (R13 reads it).
const EFFECT_WRITTEN: &str = "effect written ";
/// The log line of an external hardlink: its name, then the content's hash at link time (R10).
const LINKED: &str = "linked ";
/// Logged when a flush's prune finalizer (Bk or Bp) has returned from its Durable write: the
/// selected files' resets are then entitled (R5 (c)).
const PRUNE_FINALIZED: &str = "prune finalized by flush ";

/// A database row as recovery scans it.
type KeyValue = (Vec<u8>, Vec<u8>);

/// A block index entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub hash: Hash,
    pub height: u32,
    pub prev: Hash,
    pub status: u8,
    pub file: u32,
    pub data_pos: u32,
    pub undo_pos: u32,
}

impl IndexEntry {
    fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(49);
        v.extend_from_slice(&self.height.to_le_bytes());
        v.extend_from_slice(&self.prev);
        v.push(self.status);
        v.extend_from_slice(&self.file.to_le_bytes());
        v.extend_from_slice(&self.data_pos.to_le_bytes());
        v.extend_from_slice(&self.undo_pos.to_le_bytes());
        v
    }

    fn decode(hash: &[u8], v: &[u8]) -> Option<Self> {
        if v.len() != 49 || hash.len() != 32 {
            return None;
        }
        let u = |i: usize| u32::from_le_bytes(v[i..i + 4].try_into().expect("4 bytes"));
        Some(Self {
            hash: hash.try_into().ok()?,
            height: u(0),
            prev: v[4..36].try_into().ok()?,
            status: v[36],
            file: u(37),
            data_pos: u(41),
            undo_pos: u(45),
        })
    }
}

/// The toy's planted bugs (§6.2; † marks Core's own behaviour).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ToyBug {
    /// B1 before the files barrier.
    T1,
    /// Index-only write without the files barrier.
    T2,
    /// The marker written in Bk instead of B1.
    T3,
    /// Evo deltas and `evo_tip` in Bk.
    T4,
    /// † Unlink before Bk.
    T5,
    /// † The barrier syncs only the current pair.
    T6,
    /// Recovery erases the marker in its first batch.
    T7,
    /// Recovery replays every `HAVE_DATA` block above the tip.
    T8,
    /// † Continue after a failed `sync_data`.
    T9,
    /// The subscriber commits its locator at `new` before B1 returns.
    T10,
    /// Bk Buffered (pre-Bk barrier kept), then unlink.
    T11,
    /// `HAVE_UNDO` set before the undo write returns.
    T12,
    /// † Selection before B1, its clears in B1 (A1 rev1).
    T13,
    /// B1 drops the dirty `block_files` record.
    T14,
    /// Selection against V's tip.
    T15,
    /// † The lock from the subscriber's in-memory height.
    T16,
    /// B1 Buffered.
    T17,
    /// No pre-Bk barrier and Bk Buffered.
    T18,
    /// Selection before B1, B1 omits every prune-modified record (DEC-59 literal).
    T19,
    /// B1 before the transition's blocks were connected.
    T20,
    /// An invalid block in the marker path.
    T21,
    /// The subscriber locator without its `sync_wal`.
    T22,
    /// The adopter reads only the header, marks `HAVE_DATA`, syncs, commits.
    T23a,
    /// The adopter commits without syncing.
    T23b,
    /// † Evo committed after Bk.
    T24,
    /// Unlink after an `Applied` failure of Bk.
    T25,
    /// Bk Durable with no pre-Bk barrier (A1 rev1).
    T26,
    /// Recovery's final batch with no barrier.
    T27,
    /// Lock registered only when the subscriber starts, after activation.
    T28,
    /// No barrier before B1 while a presumed effect is pending.
    T29,
    /// The lock at the locator's height, not at the fork.
    T30,
    /// Recovery erases index entries above the recovered tip.
    T31,
    /// The prune decision uses the pre-commit durable lock.
    T32,
    /// † A syncing subscriber's cadence commit is uncapped.
    T33,
    /// Bk Buffered behind the pre-Bk barrier, on two split flushes.
    T34,
    /// A synced subscriber classified by an instantaneous "has processed new" test.
    T35,
    /// The rewind commit is Buffered.
    T36,
    /// rev3's limit rule: no checkpoint at the `D_max` limit.
    T37,
    /// The checkpoint is Buffered and the subscriber resumes before it is covered.
    T38,
    /// rev3's final selection: the automatic selector re-run.
    T39,
    /// No re-arm after a shortfall.
    T40,
    /// The checkpoint without submitting staged data.
    T41,
    /// A backward B1 without the rewind gate (DEC-80 Q1; CH2's X19).
    T42,
    /// S10b's control: the import is not adopted, so no barrier syncs its files.
    NoAdopt,
    /// DEC-80 Q1 (a)'s control: the rewind commit drops the archive rows of `(F, L]`.
    ArchiveDropped,
    /// DEC-80 Q1 (a)'s control: a commit at X writes the state at p (not `commit_state`'s X).
    StateAtProgress,
    /// R10's control: the toy writes into a hardlinked file, bypassing ST2's private copy.
    LinkedWrite,
    /// N8: Bk Buffered behind the pre-Bk barrier (safe on one flush).
    N8,
}

impl ToyBug {
    pub fn name(self) -> String {
        format!("{self:?}")
    }
}

impl FromStr for ToyBug {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        use ToyBug::*;
        const ALL: [ToyBug; 48] = [
            T1,
            T2,
            T3,
            T4,
            T5,
            T6,
            T7,
            T8,
            T9,
            T10,
            T11,
            T12,
            T13,
            T14,
            T15,
            T16,
            T17,
            T18,
            T19,
            T20,
            T21,
            T22,
            T23a,
            T23b,
            T24,
            T25,
            T26,
            T27,
            T28,
            T29,
            T30,
            T31,
            T32,
            T33,
            T34,
            T35,
            T36,
            T37,
            T38,
            T39,
            T40,
            T41,
            T42,
            N8,
            NoAdopt,
            ArchiveDropped,
            StateAtProgress,
            LinkedWrite,
        ];
        ALL.into_iter().find(|b| b.name() == s).ok_or(())
    }
}

/// The toy's configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToyConfig {
    /// Bytes per block (1,024 in the PR gate; 900 in S6).
    pub block_size: u32,
    pub undo_size: u32,
    /// Coin sub-batches per split flush; 0 = one-batch flushes.
    pub sub_batches: u32,
    /// Heights (on the first branch) whose blocks are invalid.
    pub invalid: Vec<u32>,
    /// Heights whose connect writes an independent effect.
    pub effects: Vec<u32>,
    /// The presumed effect: written when `presumed_base` is connected, read by the evo fold of
    /// `presumed_reader`.
    pub presumed_base: Option<u32>,
    pub presumed_reader: Option<u32>,
    pub prune: Option<PruneConfig>,
    /// A mandatory subscriber (`None`: the subscriber-free configuration).
    pub sub: Option<SubConfig>,
    /// The assumevalid height: blocks of the first branch up to it connect with script checks
    /// skipped (R6's provenance).
    pub assumevalid: Option<u32>,
    pub bug: Option<ToyBug>,
}

/// Prune mode: `K` (`MIN_BLOCKS_TO_KEEP`, 8 in the PR gate), the subscribers' lock buffer
/// (`PRUNE_LOCK_BUFFER`, 2), and the automatic target in bytes (`None`: manual only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PruneConfig {
    pub k: u32,
    pub buffer: u32,
    pub target: Option<u64>,
}

impl Default for PruneConfig {
    fn default() -> Self {
        Self {
            k: 8,
            buffer: 2,
            target: None,
        }
    }
}

impl Default for ToyConfig {
    fn default() -> Self {
        Self {
            block_size: 1024,
            undo_size: 240,
            sub_batches: 0,
            invalid: Vec::new(),
            effects: Vec::new(),
            presumed_base: None,
            presumed_reader: None,
            prune: None,
            sub: None,
            assumevalid: None,
            bug: None,
        }
    }
}

fn list(v: &[u32]) -> String {
    if v.is_empty() {
        "-".into()
    } else {
        v.iter().map(u32::to_string).collect::<Vec<_>>().join(",")
    }
}

fn parse_list(s: &str) -> Result<Vec<u32>, ()> {
    if s == "-" {
        return Ok(Vec::new());
    }
    s.split(',').map(|x| x.parse().map_err(|_| ())).collect()
}

fn opt(v: Option<impl fmt::Display>) -> String {
    v.map_or("-".into(), |x| x.to_string())
}

fn parse_opt<T: FromStr>(s: &str) -> Result<Option<T>, ()> {
    if s == "-" {
        Ok(None)
    } else {
        s.parse().map(Some).map_err(|_| ())
    }
}

impl fmt::Display for ToyConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "block={} undo={} subbatches={} invalid={} effects={} presumed={}:{} prune={} sub={} bug={}",
            self.block_size,
            self.undo_size,
            self.sub_batches,
            list(&self.invalid),
            list(&self.effects),
            opt(self.presumed_base),
            opt(self.presumed_reader),
            self.prune.as_ref().map_or("-".into(), |p| format!(
                "{}:{}:{}",
                p.k,
                p.buffer,
                opt(p.target)
            )),
            opt(self.sub),
            self.bug.map_or("none".into(), ToyBug::name),
        )?;
        if let Some(av) = self.assumevalid {
            write!(f, " av={av}")?;
        }
        Ok(())
    }
}

impl FromStr for ToyConfig {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        let mut c = Self::default();
        for part in s.split_whitespace() {
            let (k, v) = part.split_once('=').ok_or(())?;
            match k {
                "block" => c.block_size = v.parse().map_err(|_| ())?,
                "undo" => c.undo_size = v.parse().map_err(|_| ())?,
                "subbatches" => c.sub_batches = v.parse().map_err(|_| ())?,
                "invalid" => c.invalid = parse_list(v)?,
                "effects" => c.effects = parse_list(v)?,
                "presumed" => {
                    let (b, r) = v.split_once(':').ok_or(())?;
                    c.presumed_base = parse_opt(b)?;
                    c.presumed_reader = parse_opt(r)?;
                }
                "prune" if v == "-" => c.prune = None,
                "prune" => {
                    let mut it = v.split(':');
                    let mut next = || it.next().ok_or(());
                    c.prune = Some(PruneConfig {
                        k: next()?.parse().map_err(|_| ())?,
                        buffer: next()?.parse().map_err(|_| ())?,
                        target: parse_opt(next()?)?,
                    });
                }
                "sub" => c.sub = parse_opt(v)?,
                "av" => c.assumevalid = Some(v.parse().map_err(|_| ())?),
                "bug" => {
                    c.bug = if v == "none" {
                        None
                    } else {
                        Some(v.parse()?)
                    }
                }
                _ => return Err(()),
            }
        }
        Ok(c)
    }
}

/// A toy operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToyOp {
    /// V connects `n` blocks along the current chain (accepting them first if needed).
    Connect(u32),
    /// V accepts `n` blocks ahead without connecting them.
    Accept(u32),
    /// A flush of the chain up to the current tip.
    Flush,
    /// The periodic index-only write.
    IndexWrite,
    /// A reorg: disconnect back to `fork`, then connect `len` blocks on a new branch.
    Reorg {
        fork: u32,
        len: u32,
    },
    /// R12's continuation: connect one block, then flush.
    Continue,
    /// `pruneblockchain <height>`: a manual prune check (a flush only if it finds files).
    Prune(u32),
    /// A manual prune height for the next flush's check, with no flush now.
    PruneRequest(u32),
    /// A validation-only connect of the next accepted block (TestBlockValidity, VerifyDB:
    /// Core's fJustCheck): `Connected { applied: false }`, nothing changes.
    TestValidity,
    /// Sets the automatic prune target and makes a check due, with no flush now (a test knob:
    /// one decision at a chosen height).
    AutoTarget(u64),
    /// A flush of the tip `behind` blocks below V's, with V connecting `during` more blocks
    /// between the prune decision and the selection.
    FlushAt {
        behind: u32,
        during: u32,
    },
    /// The syncing subscriber's thread takes `n` steps toward V's tip.
    Sync(u32),
    /// The syncing subscriber's cadence timer fires (Core's 30-second locator write).
    CadenceTick,
    /// `invalidateblock`: V disconnects back below block `h` and marks it failed.
    Invalidate(u32),
    /// An import hardlinks `blk<n>.dat` from another datadir (S10; R10).
    LinkExternal(u32),
    /// Another process writes the next `n` blocks and their undo, unsynced, and the toy takes
    /// its files over with `adopt_existing_files` (S10b; IM1/CH6).
    Import(u32),
    /// The subscriber is disabled for the rest of the epoch (S11c).
    DisableSubscriber,
}

impl fmt::Display for ToyOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(n) => write!(f, "c{n}"),
            Self::Accept(n) => write!(f, "a{n}"),
            Self::Flush => f.write_str("f"),
            Self::IndexWrite => f.write_str("i"),
            Self::Continue => f.write_str("C"),
            Self::Prune(h) => write!(f, "p{h}"),
            Self::PruneRequest(h) => write!(f, "q{h}"),
            Self::TestValidity => f.write_str("v"),
            Self::AutoTarget(t) => write!(f, "A{t}"),
            Self::Sync(n) => write!(f, "s{n}"),
            Self::CadenceTick => f.write_str("K"),
            Self::Invalidate(h) => write!(f, "x{h}"),
            Self::LinkExternal(n) => write!(f, "L{n}"),
            Self::Import(n) => write!(f, "I{n}"),
            Self::DisableSubscriber => f.write_str("D"),
            Self::FlushAt {
                behind,
                during,
            } => write!(f, "F{behind}:{during}"),
            Self::Reorg {
                fork,
                len,
            } => write!(f, "r{fork}:{len}"),
        }
    }
}

impl FromStr for ToyOp {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        let num = |t: &str| t.parse::<u32>().map_err(|_| ());
        match s.split_at_checked(1).ok_or(())? {
            ("c", n) => Ok(Self::Connect(num(n)?)),
            ("a", n) => Ok(Self::Accept(num(n)?)),
            ("f", "") => Ok(Self::Flush),
            ("i", "") => Ok(Self::IndexWrite),
            ("C", "") => Ok(Self::Continue),
            ("p", h) => Ok(Self::Prune(num(h)?)),
            ("q", h) => Ok(Self::PruneRequest(num(h)?)),
            ("v", "") => Ok(Self::TestValidity),
            ("A", t) => Ok(Self::AutoTarget(t.parse().map_err(|_| ())?)),
            ("s", n) => Ok(Self::Sync(num(n)?)),
            ("K", "") => Ok(Self::CadenceTick),
            ("x", h) => Ok(Self::Invalidate(num(h)?)),
            ("L", n) => Ok(Self::LinkExternal(num(n)?)),
            ("I", n) => Ok(Self::Import(num(n)?)),
            ("D", "") => Ok(Self::DisableSubscriber),
            ("F", rest) => {
                let (behind, during) = rest.split_once(':').ok_or(())?;
                Ok(Self::FlushAt {
                    behind: num(behind)?,
                    during: num(during)?,
                })
            }
            ("r", rest) => {
                let (fork, len) = rest.split_once(':').ok_or(())?;
                Ok(Self::Reorg {
                    fork: num(fork)?,
                    len: num(len)?,
                })
            }
            _ => Err(()),
        }
    }
}

fn height_key(h: u32) -> [u8; 4] {
    h.to_be_bytes()
}

fn hash_of(v: &[u8]) -> Option<Hash> {
    v.try_into().ok()
}

/// The coins of a durable view, by height.
fn read_coins(e: &FaultyEngine) -> Vec<(u32, Hash)> {
    e.iter_prefix(COINS, b"")
        .filter_map(|r| r.ok())
        .filter_map(|(k, v)| {
            Some((u32::from_be_bytes(k.as_slice().try_into().ok()?), hash_of(&v)?))
        })
        .collect()
}

/// The toy chain.
pub struct ToyChain {
    cfg: ToyConfig,
    world: World,
    chain: Arc<FaultyEngine>,
    files: BlockFiles<FaultyFlatFiles>,
    blocks: Gen,
    index: BTreeMap<Hash, IndexEntry>,
    dirty: BTreeSet<Hash>,
    /// The active chain, by height.
    active: Vec<Hash>,
    /// Accepted blocks beyond the active tip on the current chain.
    headers: Vec<Hash>,
    /// The evo state after each active height.
    evo: Vec<Hash>,
    /// The tip of the last completed flush (`old` of the next).
    durable_tip: Option<Hash>,
    next_branch: u8,
    flushes: u64,
    /// A presumed effect written since the last barrier.
    presumed_pending: bool,
    /// R5's E: the recovered index and file infos before recovery wrote anything.
    e_index: Vec<IndexEntry>,
    e_files: BTreeMap<u32, Vec<u8>>,
    /// The blocks the replay connected coins-only.
    replayed: Vec<Hash>,
    /// The booted marker `(new, old)`, and the tip recovery reached before activation (R6).
    booted_head: Option<(Hash, Option<Hash>)>,
    recovered_tip: Option<Hash>,
    failed: bool,
    /// Core's `m_check_for_pruning`.
    prune_check: bool,
    /// A pending `pruneblockchain` height.
    manual_prune: Option<u32>,
    /// T12: an undo record whose HAVE_UNDO is already set.
    deferred_undo: Option<(crate::flatfile::CoreUndoBytes, u32, u32, Hash)>,
    /// The mandatory subscriber and its store.
    sub: Option<Sub>,
    subs: Option<Arc<FaultyEngine>>,
    sub_rec: SubRecovery,
    /// `T_ack`: the tip of the last flush whose B1 returned (or recovery's tip).
    t_ack: Option<Hash>,
    /// After an import the next files barrier reports every file info (the adopted instance
    /// has none dirty).
    imported: bool,
    /// The Core call paths the toy is inside, outermost first.
    paths: Vec<&'static str>,
}

/// Step 0's dry run (§6.2): the files a selection would prune against the flushed tip `new_h`
/// under the lock bound `lock` (`None`: no lock) and `manual` (a `pruneblockchain` height) or the
/// automatic `target`, through ST2's selectors on a scratch copy of the file infos (they are
/// mutating). Returns D and whether D met the target (always true for a manual check).
#[allow(clippy::too_many_arguments)]
pub fn prune_dry_run<F: crate::flatfile::FlatFs>(
    fs: Arc<F>,
    cfg: BlockFilesConfig,
    state: BlockFilesState,
    new_h: u32,
    k: u32,
    lock: Option<u32>,
    manual: Option<u32>,
    target: Option<u64>,
) -> (Vec<u32>, bool) {
    if new_h < k {
        return (Vec::new(), false);
    }
    // ST2 keeps Core's MIN_BLOCKS_TO_KEEP (288); the toy's K is smaller, so the tip is shifted
    // to make `tip - 288` the toy's `new - K`.
    let tip = new_h - k + MIN_BLOCKS_TO_KEEP;
    let mut scratch = BlockFiles::with_fs(fs, cfg, state);
    let bound = lock.unwrap_or(u32::MAX);
    match manual {
        Some(m) => (scratch.find_files_to_prune_manual(m.min(bound), Some(tip)), true),
        None => {
            let target = target.unwrap_or(0);
            let prune_height = bound.min(i32::MAX as u32) as i32;
            let d = scratch.find_files_to_prune(target, 0, Some(tip), prune_height, false);
            let met = !target_unmet(scratch.calculate_current_usage(), Some(target));
            (d, met)
        }
    }
}

/// Step 7's final selection (§6.2): `D ∩ eligible(actual)`, each file of D still before the
/// current file, not yet pruned, and with its last height within the actual bound. The target is
/// not re-applied, so the result is a subset of D.
pub fn final_selection(d: &[u32], infos: &[BlockFileInfo], last_file: u32, bound: u32) -> Vec<u32> {
    d.iter()
        .copied()
        .filter(|&f| {
            f < last_file
                && infos.get(f as usize).is_some_and(|i| i.size > 0 && i.height_last <= bound)
        })
        .collect()
}

/// Whether an automatic target is still unmet at `usage` (ST2's buffer included).
pub fn target_unmet(usage: u64, target: Option<u64>) -> bool {
    usage + PRUNE_BUFFER >= target.unwrap_or(0)
}

impl ToyChain {
    fn bug(&self, b: ToyBug) -> bool {
        self.cfg.bug == Some(b)
    }

    fn abort(&mut self, op: &'static str, message: &str) -> Abort {
        if !self.failed {
            self.failed = true;
            self.world.mark(Mark::StoreFailed {
                op,
            });
            self.world.mark(Mark::Abort {
                message: message.into(),
            });
        }
        Abort::new(message)
    }

    fn files_cfg(cfg: &ToyConfig) -> BlockFilesConfig {
        let mut c = BlockFilesConfig::new(BLOCKS_DIR, MAGIC);
        c.fastprune = true;
        c.prune_mode = cfg.prune.is_some();
        c
    }

    fn tip(&self) -> Option<Hash> {
        self.active.last().copied()
    }

    fn enter(&mut self, path: &'static str) {
        self.world.mark(Mark::PathBegin {
            path,
        });
        self.paths.push(path);
    }

    fn leave(&mut self) {
        self.world.mark(Mark::PathEnd);
        self.paths.pop();
    }

    /// Core's message for a chainstate engine error on the current path: inside
    /// `LoadChainstate` any database error throws and init reports it (DEC-165 (3)).
    fn engine_message(&self, otherwise: &'static str) -> &'static str {
        if self.paths.contains(&PATH_LOAD) {
            DB_OPEN_FAILED
        } else {
            otherwise
        }
    }

    fn write(
        &mut self,
        b: dash_kv::MemoryBatch,
        commit: Commit,
        what: &'static str,
    ) -> Result<(), Abort> {
        let r = self.chain.write(b, commit);
        let message = self.engine_message(FLUSH_FAILED);
        r.map_err(|_| self.abort(what, message))
    }

    fn sync_wal(&mut self) -> Result<(), Abort> {
        let r = self.chain.sync_wal();
        let message = self.engine_message(FLUSH_FAILED);
        r.map_err(|_| self.abort("sync_wal", message))
    }

    // -----------------------------------------------------------------------------------------
    // V: accept and connect.

    /// Stores the next block of the current chain (Core's `AcceptBlock`).
    fn accept_next(&mut self) -> Result<Hash, Abort> {
        let parent = self.headers.last().or(self.active.last()).copied().unwrap_or(NULL);
        let height = (self.active.len() + self.headers.len()) as u32;
        // After recovery the generator knows only activated blocks: a recovered tip's branch is
        // in its data.
        let branch = match self.blocks.get(&parent) {
            Some(b) => b.branch,
            None => match self.index.get(&parent).cloned() {
                Some(e) => {
                    self.enter(PATH_CONNECT);
                    let b = self.recovery_block(&e)?;
                    self.leave();
                    b.and_then(|b| b.get(76).copied()).unwrap_or(0)
                }
                None => 0,
            },
        };
        let branch = if parent == NULL {
            0
        } else {
            branch
        };
        let b = self.blocks.block(&self.cfg, &parent, height, branch);
        let pos = self
            .files
            .save_block(&b.bytes, height, u64::from(height), None)
            .map_err(|_| self.abort("save_block", "Failed to write block"))?;
        self.index.insert(
            b.id,
            IndexEntry {
                hash: b.id,
                height,
                prev: parent,
                status: STATUS_DATA,
                file: pos.file,
                data_pos: pos.pos,
                undo_pos: 0,
            },
        );
        self.dirty.insert(b.id);
        self.headers.push(b.id);
        Ok(b.id)
    }

    /// Connects the first accepted block (Core's `ConnectTip`): undo, status, coins and evo in
    /// memory, immediate effects. Returns `false` for an invalid block (marked failed).
    fn connect_next(&mut self) -> Result<bool, Abort> {
        let id = self.headers.remove(0);
        let b = self.blocks.get(&id).expect("generated").clone();
        let mut e = self.index[&id].clone();
        // T12: the previous block's undo is written only now, after its HAVE_UNDO.
        if let Some((undo, file, height, parent)) = self.deferred_undo.take() {
            self.files
                .write_undo(&undo, file, height, &parent)
                .map_err(|_| self.abort("write_undo", "Failed to write undo data"))?;
        }
        if !b.valid && !self.bug(ToyBug::T21) {
            e.status |= STATUS_FAILED;
            self.index.insert(id, e);
            self.dirty.insert(id);
            self.headers.clear();
            return Ok(false);
        }
        self.world.mark(Mark::ConnectBegin);
        // Core's `WriteUndoDataForBlock` writes undo only for a block without it (a block
        // re-connected after a crash keeps its record).
        if e.status & STATUS_UNDO == 0 && self.bug(ToyBug::T12) {
            let at = self.files.file_info(e.file).map_or(0, |i| i.undo_size);
            e.undo_pos = at + BLOCK_SERIALIZATION_HEADER_SIZE;
            self.deferred_undo = Some((b.undo.clone(), e.file, b.height, b.parent));
        } else if e.status & STATUS_UNDO == 0 {
            let pos = self
                .files
                .write_undo(&b.undo, e.file, b.height, &b.parent)
                .map_err(|_| self.abort("write_undo", "Failed to write undo data"))?;
            e.undo_pos = pos.pos;
        }
        e.status |= STATUS_UNDO | STATUS_VALID;
        self.index.insert(id, e);
        self.dirty.insert(id);
        let prev = self.evo.last().copied().unwrap_or([0u8; 32]);
        let snap = if Some(b.height) == self.cfg.presumed_reader {
            match self.chain.get(EFFECTS, K_SNAP) {
                Ok(v) => v.and_then(|v| hash_of(&v)),
                Err(_) => return Err(self.abort("get", DB_READ_FAILED)),
            }
        } else {
            None
        };
        self.evo.push(evo_fold(&prev, &id, snap.as_ref()));
        self.active.push(id);
        self.world.mark(Mark::Connected {
            block: id,
            scripts: reference::scripts(&self.cfg, &b),
            applied: true,
        });
        self.world.mark(Mark::ConnectEnd);
        // Immediate Buffered effects (A1 §2.10.1).
        if self.cfg.effects.contains(&b.height) {
            let key = format!("i:{}", b.height);
            let mut batch = self.chain.batch();
            batch.put(EFFECTS, key.as_bytes(), &id);
            self.write(batch, Commit::Buffered, "effect")?;
            self.world.mark(Mark::Log {
                line: format!("{EFFECT_WRITTEN}{key}"),
            });
        }
        if Some(b.height) == self.cfg.presumed_base {
            let mut batch = self.chain.batch();
            batch.put(EFFECTS, K_SNAP, &id);
            self.write(batch, Commit::Buffered, "effect")?;
            self.presumed_pending = true;
        }
        self.sub_on_connect(id)?;
        Ok(true)
    }

    fn connect(&mut self, n: u32) -> Result<(), Abort> {
        for _ in 0..n {
            if self.headers.is_empty() {
                self.accept_next()?;
            }
            if !self.connect_next()? {
                break;
            }
            // Core's `FlushStateToDisk(IF_NEEDED)` after each connect: a due prune check.
            self.maybe_flush()?;
        }
        Ok(())
    }

    fn disconnect_to(&mut self, fork: u32) -> Result<(), Abort> {
        self.headers.clear();
        while self.active.len() as u32 > fork + 1 {
            let id = self.active.pop().expect("a block");
            self.evo.pop();
            self.sub_on_disconnect(id, self.active.len() as u32)?;
        }
        Ok(())
    }

    /// `invalidateblock h`: disconnect back to `h − 1`, and mark block `h` failed and its
    /// descendants failed children (one bit in the toy).
    fn invalidate(&mut self, h: u32) -> Result<(), Abort> {
        let failed: Vec<Hash> = self.active.iter().skip(h as usize).copied().collect();
        if failed.is_empty() {
            return Ok(());
        }
        self.disconnect_to(h.saturating_sub(1))?;
        for id in failed {
            self.index.get_mut(&id).expect("an entry").status |= STATUS_FAILED;
            self.dirty.insert(id);
        }
        Ok(())
    }

    fn reorg(&mut self, fork: u32, len: u32) -> Result<(), Abort> {
        self.disconnect_to(fork)?;
        let branch = self.next_branch;
        self.next_branch += 1;
        for _ in 0..len {
            let parent = *self.active.last().expect("genesis");
            let height = self.active.len() as u32;
            let b = self.blocks.block(&self.cfg, &parent, height, branch);
            // Accept along the new branch, then connect.
            self.headers = vec![];
            let pos = self
                .files
                .save_block(&b.bytes, height, u64::from(height), None)
                .map_err(|_| self.abort("save_block", "Failed to write block"))?;
            self.index.insert(
                b.id,
                IndexEntry {
                    hash: b.id,
                    height,
                    prev: parent,
                    status: STATUS_DATA,
                    file: pos.file,
                    data_pos: pos.pos,
                    undo_pos: 0,
                },
            );
            self.dirty.insert(b.id);
            self.headers.push(b.id);
            self.connect_next()?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------------------------
    // The flush.

    /// The path from genesis to `tip` through the index.
    fn path(&self, tip: Option<Hash>) -> Vec<Hash> {
        let mut out = Vec::new();
        let mut cur = tip.unwrap_or(NULL);
        while cur != NULL {
            out.push(cur);
            cur = self.index.get(&cur).map_or(NULL, |e| e.prev);
        }
        out.reverse();
        out
    }

    /// The coin operations of the transition `old → new`: erase the old branch above the fork,
    /// put the new one (blind: nothing is read, A1 §3.5.5).
    fn coin_ops(&self, old: Option<Hash>, new: Option<Hash>) -> Vec<(u32, Option<Hash>)> {
        let (op, np) = (self.path(old), self.path(new));
        let fork = op.iter().zip(&np).take_while(|(a, b)| a == b).count();
        let mut ops: Vec<(u32, Option<Hash>)> =
            (fork..op.len()).rev().map(|h| (h as u32, None)).collect();
        ops.extend((fork..np.len()).map(|h| (h as u32, Some(np[h]))));
        ops
    }

    fn put_coins(batch: &mut dash_kv::MemoryBatch, ops: &[(u32, Option<Hash>)]) {
        for (h, v) in ops {
            match v {
                Some(id) => batch.put(COINS, &height_key(*h), id),
                None => batch.delete(COINS, &height_key(*h)),
            }
        }
    }

    fn put_infos(batch: &mut dash_kv::MemoryBatch, infos: &[(u32, BlockFileInfo)], last_file: u32) {
        for (n, info) in infos {
            batch.put(FILES, &n.to_be_bytes(), &info.to_value_bytes());
        }
        batch.put(META, K_LAST_FILE, &last_file.to_le_bytes());
    }

    fn put_index(&mut self, batch: &mut dash_kv::MemoryBatch) {
        for id in std::mem::take(&mut self.dirty) {
            batch.put(INDEX, &id, &self.index[&id].encode());
        }
    }

    /// Every file's info, numbered.
    fn all_infos(&self) -> Vec<(u32, BlockFileInfo)> {
        self.files.infos().iter().enumerate().map(|(n, i)| (n as u32, *i)).collect()
    }

    fn files_barrier(&mut self) -> Result<crate::flatfile::DirtyFiles, Abort> {
        if self.bug(ToyBug::T6) || self.bug(ToyBug::T9) {
            // † T6: Core's FlushBlockFile syncs only the current pair. T9: a failed sync is
            // ignored. Either way the infos describe every file.
            let r = self.files.flush_block_file(false, false);
            if r.is_err() && !self.bug(ToyBug::T9) {
                return Err(self.abort("sync", FILES_SYNC_FAILED));
            }
            return Ok(crate::flatfile::DirtyFiles {
                infos: self.all_infos(),
                last_file: self.files.last_file(),
            });
        }
        let r = self.files.sync_and_take_dirty_infos();
        let mut d = r.map_err(|_| self.abort("sync", FILES_SYNC_FAILED))?;
        if std::mem::take(&mut self.imported) {
            d.infos = self.all_infos();
        }
        Ok(d)
    }

    /// Step 0's dry run on the toy's current files (see [`prune_dry_run`]).
    fn dry_run(&self, new_h: u32, lock: Option<u32>, manual: Option<u32>) -> (Vec<u32>, bool) {
        let p = self.cfg.prune.as_ref().expect("prune mode");
        let state = files_state(&self.files);
        prune_dry_run(
            self.world.fs(),
            Self::files_cfg(&self.cfg),
            state,
            new_h,
            p.k,
            lock,
            manual,
            p.target,
        )
    }

    /// The subscriber's prune lock at step 0: the predicted post-commit lock (T32: the
    /// pre-commit durable one), with Core's log line.
    fn predicted_lock(&self, new: &Hash) -> Option<u32> {
        let lock = self.sub_lock(new, !self.bug(ToyBug::T32));
        self.sub_bound(new, lock, true)
    }

    /// The actual lock at step 7, after the commit step.
    fn actual_lock(&self, new: &Hash) -> Option<u32> {
        self.sub_bound(new, self.sub_lock(new, false), false)
    }

    /// The tip height a prune decides and selects against: `new_h` (T15: V's tip).
    fn prune_tip_h(&self, new_h: u32) -> u32 {
        if self.bug(ToyBug::T15) {
            (self.active.len() - 1) as u32
        } else {
            new_h
        }
    }

    /// Step 0: the dry run under the predicted lock, its mark (with `cause`, a full-flush cause
    /// other than pruning), and the due check cleared. Returns D, whether it met the target, and
    /// whether the check was automatic.
    fn decide(
        &mut self,
        new_h: u32,
        new_tip: Hash,
        cause: Option<FlushCause>,
    ) -> (Vec<u32>, bool, bool) {
        let manual = self.manual_prune;
        self.sub_mark(false);
        let predicted = self.predicted_lock(&new_tip);
        let dry_h = self.prune_tip_h(new_h);
        let (d, met) = self.dry_run(dry_h, predicted, manual);
        let unlocked = self.dry_run(dry_h, None, manual).0;
        let flush_for_prune = !d.is_empty();
        let mut causes = Vec::new();
        if flush_for_prune {
            causes.push(FlushCause::Prune);
        }
        causes.extend(cause);
        self.world.mark(Mark::PruneDecision {
            due: manual.map_or(PruneDue::Automatic, PruneDue::Manual),
            dry_run: d.clone(),
            met_target: met,
            predicted_lock: predicted,
            flush_for_prune,
            full_flush: causes,
            new: new_tip,
            unlocked,
        });
        // An automatic check is cleared here, and may be re-armed at step 7.
        self.prune_check = false;
        self.manual_prune = None;
        (d, met, manual.is_none())
    }

    /// A flush the workload scripts.
    fn flush(&mut self) -> Result<(), Abort> {
        self.flush_at(0, 0, Some(FlushCause::Scripted))
    }

    /// Whether a prune check is due; if so, decides and flushes when the dry run finds files
    /// (Core's `FlushStateToDisk(IF_NEEDED)` after a connect, and `pruneblockchain`).
    fn maybe_flush(&mut self) -> Result<(), Abort> {
        if self.cfg.prune.is_some() && self.files.take_check_for_pruning() {
            self.prune_check = true;
        }
        // A subscriber at its limit makes a `DMax` flush due.
        let dmax = self.sub.as_ref().is_some_and(|s| s.flush_due);
        if self.prune_check || self.manual_prune.is_some() || dmax {
            return self.flush_at(0, 0, dmax.then_some(FlushCause::DMax));
        }
        Ok(())
    }

    /// The flush (DEC-64; §6.2 steps 0-12) of the tip `behind` blocks below V's, with V
    /// connecting `during` more blocks between the decision and the selection (V ahead).
    /// `cause` is a full-flush cause other than pruning; with none, the flush happens only if
    /// the prune dry run finds files.
    fn flush_at(
        &mut self,
        behind: u32,
        during: u32,
        cause: Option<FlushCause>,
    ) -> Result<(), Abort> {
        if self.active.len() <= behind as usize {
            return Ok(());
        }
        let new_h = (self.active.len() - 1 - behind as usize) as u32;
        let mut new_tip = self.active[new_h as usize];
        // T20: B1 for a block that was accepted but never connected.
        if self.bug(ToyBug::T20)
            && let Some(&h) = self.headers.first()
        {
            new_tip = h;
        }
        let new = Some(new_tip);
        let old = self.durable_tip;
        // Step 0: the prune decision, when a check is due.
        let due = self.cfg.prune.is_some() && (self.prune_check || self.manual_prune.is_some());
        let decision = if due {
            Some(self.decide(new_h, new_tip, cause))
        } else {
            None
        };
        let flush_for_prune = decision.as_ref().is_some_and(|(d, _, _)| !d.is_empty());
        if !flush_for_prune && cause.is_none() {
            return Ok(());
        }
        let selection_early = self.bug(ToyBug::T13) || self.bug(ToyBug::T19);
        // V runs ahead between the decision and the selection.
        for _ in 0..during {
            if self.headers.is_empty() {
                self.accept_next()?;
            }
            self.connect_next()?;
        }
        self.flushes += 1;
        let flush = self.flushes;
        self.world.mark(Mark::FlushBegin {
            flush,
            old,
            new: new_tip,
        });
        let split = self.cfg.sub_batches > 0;
        // T13/T19: the selection before B1 (A1 rev1 and DEC-59's literal text).
        let mut selected = Vec::new();
        if selection_early && let Some((d, met, auto)) = &decision {
            selected = self.select(flush, new_h, d, *met, *auto)?;
        }
        // Step 2: the files barrier (T1 moves it after B1).
        let infos = if self.bug(ToyBug::T1) {
            None
        } else {
            Some(self.files_barrier()?)
        };
        // Only entries whose bytes this barrier covered go into B1.
        let mut b1_entries = std::mem::take(&mut self.dirty);
        if self.bug(ToyBug::T19) {
            // B1 omits every record the selection modified; they wait for Bk.
            let (cleared, kept) =
                b1_entries.into_iter().partition(|h| self.index[h].status & STATUS_DATA == 0);
            self.dirty = cleared;
            b1_entries = kept;
        }
        // Step 3: a pending presumed effect needs a barrier before B1 (T29 skips it).
        if self.presumed_pending && !self.bug(ToyBug::T29) {
            self.sync_wal()?;
        }
        // Step 3b: the rewind gate before a backward B1 (DEC-80 Q1).
        self.sub_rewind_gate(new_tip)?;
        // T10: the subscriber commits at `new` before B1 returns.
        if self.bug(ToyBug::T10) {
            self.sub_commit_step(new_tip)?;
        }
        let coins = self.coin_ops(old, new);
        let evo = self.evo[new_h as usize];
        // Step 4: B1.
        let mut b1 = self.chain.batch();
        for id in &b1_entries {
            b1.put(INDEX, id, &self.index[id].encode());
        }
        if let Some(mut infos) = infos
            && !self.bug(ToyBug::T14)
        {
            if self.bug(ToyBug::T19) {
                infos.infos.retain(|(n, _)| !selected.contains(n));
            }
            Self::put_infos(&mut b1, &infos.infos, self.files.last_file());
        }
        if self.bug(ToyBug::T13) && !selected.is_empty() {
            b1.put(META, K_PRUNED, &[1]);
        }
        let evo_in_b1 =
            !(split && (self.bug(ToyBug::T4) || self.bug(ToyBug::T3) || self.bug(ToyBug::T24)));
        if evo_in_b1 {
            b1.put(EVO, K_STATE, &evo);
            b1.put(META, K_EVO_TIP, &new_tip);
        }
        if split && self.bug(ToyBug::T3) {
            // T3: B1 is a plain index write; the marker, evo and the tip all wait for Bk, so
            // the coin sub-batches land under `tip = old`.
        } else if split {
            let mut head = new_tip.to_vec();
            head.extend_from_slice(&old.unwrap_or(NULL));
            b1.put(META, K_HEAD, &head);
            b1.delete(META, K_TIP);
        } else {
            Self::put_coins(&mut b1, &coins);
            b1.put(META, K_TIP, &new_tip);
            b1.delete(META, K_HEAD);
        }
        let b1_commit = if self.bug(ToyBug::T17) {
            Commit::Buffered
        } else {
            Commit::Durable
        };
        self.write(b1, b1_commit, "B1")?;
        self.presumed_pending = false;
        if self.bug(ToyBug::T1) {
            let infos = self.files_barrier()?;
            let mut b = self.chain.batch();
            Self::put_infos(&mut b, &infos.infos, infos.last_file);
            self.write(b, Commit::Durable, "B1")?;
        }
        // Step 5: ChainStateFlushed.
        self.world.mark(Mark::Durable {
            flush,
        });
        self.t_ack = new;
        // Step 6: synced mandatory subscribers commit.
        if !self.bug(ToyBug::T10) {
            self.sub_commit_step(new_tip)?;
        }
        // Step 7: the prune selection, `D ∩ eligible(actual)`.
        if !selection_early
            && let Some((d, met, auto)) = &decision
            && !d.is_empty()
        {
            selected = self.select(flush, new_h, d, *met, *auto)?;
        }
        if self.bug(ToyBug::T5) && !selected.is_empty() {
            // † Core unlinks before the coins write.
            self.unlink(&selected)?;
        }
        let pruned = !selected.is_empty();
        // Whether this flush's finalizer (Bk or Bp) returned from a Durable write.
        let mut finalized = false;
        let bk_failed = if split {
            // Step 8: coin sub-batches; step 9: the pre-Bk barrier; step 10: Bk.
            let n = self.cfg.sub_batches as usize;
            let per = coins.len().div_ceil(n + 1).max(1);
            let mut chunks: Vec<&[(u32, Option<Hash>)]> = coins.chunks(per).collect();
            let last = if chunks.is_empty() {
                &[][..]
            } else {
                chunks.pop().expect("a chunk")
            };
            for c in chunks {
                let mut b = self.chain.batch();
                Self::put_coins(&mut b, c);
                self.write(b, Commit::Buffered, "coins")?;
            }
            let no_barrier = self.bug(ToyBug::T18) || self.bug(ToyBug::T26);
            if !no_barrier {
                self.sync_wal()?;
            }
            let infos2 = self.files_barrier()?;
            let mut bk = self.chain.batch();
            Self::put_coins(&mut bk, last);
            Self::put_infos(&mut bk, &infos2.infos, infos2.last_file);
            self.put_prune_clears(&mut bk, pruned);
            if !evo_in_b1 && !self.bug(ToyBug::T24) {
                bk.put(EVO, K_STATE, &evo);
                bk.put(META, K_EVO_TIP, &new_tip);
            }
            bk.put(META, K_TIP, &new_tip);
            bk.delete(META, K_HEAD);
            let buffered = self.bug(ToyBug::T18)
                || self.bug(ToyBug::T11)
                || self.bug(ToyBug::T34)
                || self.bug(ToyBug::N8);
            let r = self.chain.write(
                bk,
                if buffered {
                    Commit::Buffered
                } else {
                    Commit::Durable
                },
            );
            finalized = r.is_ok() && !buffered;
            if r.is_ok() && self.bug(ToyBug::T24) {
                // † Core commits EvoDB after the coins.
                let mut b = self.chain.batch();
                b.put(EVO, K_STATE, &evo);
                b.put(META, K_EVO_TIP, &new_tip);
                self.write(b, Commit::Buffered, "evo")?;
            }
            r.is_err()
        } else if pruned {
            // Bp: the clears, the reset infos and `pruned_files`, because B1 set the tip.
            let infos2 = self.files_barrier()?;
            let mut bp = self.chain.batch();
            Self::put_infos(&mut bp, &infos2.infos, infos2.last_file);
            self.put_prune_clears(&mut bp, true);
            let r = self.chain.write(bp, Commit::Durable);
            finalized = r.is_ok();
            r.is_err()
        } else {
            false
        };
        if pruned && finalized {
            self.world.mark(Mark::Log {
                line: format!("{PRUNE_FINALIZED}{flush}"),
            });
        }
        if bk_failed {
            // T25: unlink anyway after a failed Bk.
            if self.bug(ToyBug::T25) && pruned {
                let _ = self.unlink(&selected);
            }
            return Err(self.abort("Bk", FLUSH_FAILED));
        }
        // Step 11: release; the unlink once Bk or Bp has returned.
        if pruned && !self.bug(ToyBug::T5) {
            self.unlink(&selected)?;
        }
        self.durable_tip = new;
        Ok(())
    }

    /// Step 7: prunes `D ∩ eligible(actual)` (each file of D still before the current file and
    /// within the actual bound), clears its index entries in memory, and re-arms an automatic
    /// check that falls short of the target only because of what happened since step 0.
    fn select(
        &mut self,
        flush: u64,
        new_h: u32,
        d: &[u32],
        met: bool,
        automatic: bool,
    ) -> Result<Vec<u32>, Abort> {
        let p = self.cfg.prune.clone().expect("prune mode");
        let tip_h = self.prune_tip_h(new_h);
        let new_tip = self.active[new_h as usize];
        let lock = self.actual_lock(&new_tip);
        let bound = tip_h.saturating_sub(p.k).min(lock.unwrap_or(u32::MAX));
        let last = self.files.last_file();
        let selected: Vec<u32> = if self.bug(ToyBug::T39) && automatic {
            // rev3: re-run the automatic selector under the actual bound.
            let (again, _) = self.dry_run(tip_h, lock, None);
            again
        } else {
            final_selection(d, self.files.infos(), last, bound)
        };
        for &f in &selected {
            self.files.prune_one_block_file(f);
        }
        let cleared: Vec<Hash> = self
            .index
            .values()
            .filter(|e| selected.contains(&e.file) && e.status & (STATUS_DATA | STATUS_UNDO) != 0)
            .map(|e| e.hash)
            .collect();
        for h in cleared {
            let e = self.index.get_mut(&h).expect("an entry");
            e.status &= !(STATUS_DATA | STATUS_UNDO);
            e.data_pos = 0;
            e.undo_pos = 0;
            self.dirty.insert(h);
        }
        let target_unmet =
            automatic && target_unmet(self.files.calculate_current_usage(), p.target);
        let rearmed = met && target_unmet && !self.bug(ToyBug::T40);
        if rearmed {
            self.prune_check = true;
        }
        self.world.mark(Mark::PruneSelected {
            flush,
            files: selected.clone(),
            bound,
            rearmed,
            target_unmet,
            lock,
        });
        Ok(selected)
    }

    /// The prune's index clears and `pruned_files`, into Bk or Bp.
    fn put_prune_clears(&mut self, b: &mut dash_kv::MemoryBatch, pruned: bool) {
        let cleared: Vec<Hash> = self
            .dirty
            .iter()
            .filter(|h| self.index[*h].status & STATUS_DATA == 0)
            .copied()
            .collect();
        for h in cleared {
            self.dirty.remove(&h);
            b.put(INDEX, &h, &self.index[&h].encode());
        }
        if pruned {
            b.put(META, K_PRUNED, &[1]);
        }
    }

    /// Step 11: the unlink, best-effort (a failed remove is logged and left for the scan).
    fn unlink(&mut self, files: &[u32]) -> Result<(), Abort> {
        self.world.mark(Mark::BestEffortBegin);
        let r = self.files.unlink_pruned_files(files);
        self.world.mark(Mark::BestEffortEnd);
        r.map_err(|_| self.abort("unlink", "Failed to sync the blocks directory"))
    }

    /// The periodic index-only write (step 12): the files barrier, then one Durable batch.
    fn index_write(&mut self) -> Result<(), Abort> {
        let infos = if self.bug(ToyBug::T2) {
            let d = std::mem::take(&mut self.dirty);
            self.dirty = d;
            None
        } else {
            Some(self.files_barrier()?)
        };
        let mut b = self.chain.batch();
        self.put_index(&mut b);
        if let Some(infos) = infos {
            Self::put_infos(&mut b, &infos.infos, infos.last_file);
        }
        self.write(b, Commit::Durable, "index")
    }

    // -----------------------------------------------------------------------------------------
    // Recovery.

    fn load_index(&self) -> BTreeMap<Hash, IndexEntry> {
        self.chain
            .iter_prefix(INDEX, b"")
            .filter_map(|r| r.ok())
            .filter_map(|(k, v)| IndexEntry::decode(&k, &v))
            .map(|e| (e.hash, e))
            .collect()
    }

    fn read_block(&self, e: &IndexEntry) -> Result<Option<Vec<u8>>, Unavailable> {
        let mut out = Vec::new();
        let r = self.files.reader().read_block(FlatFilePos::new(e.file, e.data_pos), &mut out);
        block_read(r, out)
    }

    /// A block-file read the node acts on: missing or corrupt data is `None` (Core cannot
    /// replay), an I/O error stops the node (§4.4) with Core's message for the call path
    /// (DEC-162 (4)): durable bytes in an unreadable block are unavailable, never to be taken for
    /// absent (DEC-148).
    fn recovery_read(
        &mut self,
        r: crate::flatfile::FlatResult<()>,
        out: Vec<u8>,
        message: &str,
    ) -> Result<Option<Vec<u8>>, Abort> {
        block_read(r, out).map_err(|Unavailable| self.abort("read", message))
    }

    /// A block read by ConnectTip ("Failed to read block", `validation.cpp:3412`).
    fn recovery_block(&mut self, e: &IndexEntry) -> Result<Option<Vec<u8>>, Abort> {
        self.block_with(e, BLOCK_READ_FAILED)
    }

    /// An undo read by a disconnect (`DisconnectBlock` fails, `validation.cpp:2300`, then
    /// ActivateBestChainStep aborts, `validation.cpp:3621`).
    fn recovery_undo(&mut self, e: &IndexEntry) -> Result<Option<Vec<u8>>, Abort> {
        self.undo_with(e, DISCONNECT_FAILED)
    }

    fn block_with(&mut self, e: &IndexEntry, message: &str) -> Result<Option<Vec<u8>>, Abort> {
        let mut out = Vec::new();
        let r = self.files.reader().read_block(FlatFilePos::new(e.file, e.data_pos), &mut out);
        self.recovery_read(r, out, message)
    }

    fn undo_with(&mut self, e: &IndexEntry, message: &str) -> Result<Option<Vec<u8>>, Abort> {
        let mut out = Vec::new();
        let r =
            self.files.reader().read_undo(FlatFilePos::new(e.file, e.undo_pos), &e.prev, &mut out);
        self.recovery_read(r, out, message)
    }

    /// A database read in recovery: an error stops it (§4.4).
    fn rget(&mut self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>, Abort> {
        let r = self.chain.get(cf, key);
        let message = self.engine_message(DB_READ_FAILED);
        r.map_err(|_| self.abort("read", message))
    }

    fn rscan(&mut self, cf: Cf) -> Result<Vec<KeyValue>, Abort> {
        let r: dash_kv::Result<Vec<_>> = self.chain.iter_prefix(cf, b"").collect();
        let message = self.engine_message(DB_READ_FAILED);
        r.map_err(|_| self.abort("read", message))
    }

    fn read_undo(&self, e: &IndexEntry) -> Result<Option<Vec<u8>>, Unavailable> {
        let mut out = Vec::new();
        let r =
            self.files.reader().read_undo(FlatFilePos::new(e.file, e.undo_pos), &e.prev, &mut out);
        block_read(r, out)
    }

    fn recover(&mut self) -> Result<(), Abort> {
        self.world.mark(Mark::RecoveryBegin);
        // Steps 0–5 are Core's `LoadChainstate` (init's `catch_exceptions`); activation is not.
        self.enter(PATH_LOAD);
        // §5.5: every engine syncs its WAL before recovery's first write.
        self.sync_wal()?;
        // Step 1: the block files from the durable infos.
        let mut infos = Vec::new();
        for (k, v) in self.rscan(FILES)? {
            let n = u32::from_be_bytes(
                k.as_slice().try_into().map_err(|_| Abort::new(UNABLE_TO_REPLAY))?,
            );
            let info = BlockFileInfo::decode_value(&v).map_err(|_| Abort::new(UNABLE_TO_REPLAY))?;
            if infos.len() <= n as usize {
                infos.resize(n as usize + 1, BlockFileInfo::default());
            }
            infos[n as usize] = info;
            self.e_files.insert(n, v);
        }
        let last_file = self
            .rget(META, K_LAST_FILE)?
            .map_or(0, |v| u32::from_le_bytes(v.as_slice().try_into().unwrap_or([0; 4])));
        // T23a/b take the file infos from the files' lengths and adopt the records past them.
        let adopted = if self.bug(ToyBug::T23a) || self.bug(ToyBug::T23b) {
            self.adopt_trailing(&mut infos, last_file)?
        } else {
            Vec::new()
        };
        self.files = BlockFiles::with_fs(
            self.world.fs(),
            Self::files_cfg(&self.cfg),
            BlockFilesState {
                infos,
                last_file,
            },
        );
        // Step 2: the startup scan of already-pruned files (best-effort).
        if self.rget(META, K_PRUNED)?.is_some() {
            self.world.mark(Mark::BestEffortBegin);
            let r = self.files.scan_and_unlink_already_pruned_files(true);
            self.world.mark(Mark::BestEffortEnd);
            r.map_err(|_| self.abort("scan", "Failed to sync the blocks directory"))?;
        }
        // Step 3: the index (R5's E).
        self.index = self
            .rscan(INDEX)?
            .into_iter()
            .filter_map(|(k, v)| IndexEntry::decode(&k, &v))
            .map(|e| (e.hash, e))
            .collect();
        self.e_index = self.index.values().cloned().collect();
        // Blocks the index already knows are skipped, as Core's `LoadExternalBlockFile` does.
        let adopted: Vec<IndexEntry> =
            adopted.into_iter().filter(|e| !self.index.contains_key(&e.hash)).collect();
        if !adopted.is_empty() {
            let mut b = self.chain.batch();
            for e in adopted {
                b.put(INDEX, &e.hash, &e.encode());
                self.index.insert(e.hash, e);
            }
            Self::put_infos(&mut b, &self.all_infos(), self.files.last_file());
            self.write(b, Commit::Durable, "adopt")?;
        }
        let head = self.rget(META, K_HEAD)?;
        let evo_tip = self.rget(META, K_EVO_TIP)?.and_then(|v| hash_of(&v));
        let meta_tip = self.rget(META, K_TIP)?.and_then(|v| hash_of(&v));
        // Step 0: the subscriber's locator (its prune lock) before anything can prune; G is the
        // marker's `new` while a marker is durable, else `meta.tip`.
        let g = head.as_deref().and_then(marker_new).or(meta_tip);
        self.sub_recover_begin(g)?;
        let tip = match head {
            None => {
                let tip = meta_tip;
                if tip != evo_tip {
                    return Err(Abort::new(EVO_INCONSISTENT));
                }
                tip
            }
            Some(head) => {
                // Step 4: the coins-only replay of `old → fork → new`.
                let new = hash_of(&head[..32]).ok_or_else(|| Abort::new(UNABLE_TO_REPLAY))?;
                let old = hash_of(&head[32..]).filter(|o| *o != NULL);
                let ok = self.index.get(&new).is_some_and(|e| e.status & STATUS_DATA != 0)
                    && old.is_none_or(|o| self.index.contains_key(&o))
                    && evo_tip == Some(new);
                if !ok {
                    return Err(Abort::new(UNABLE_TO_REPLAY));
                }
                self.booted_head = Some((new, old));
                self.enter(PATH_REPLAY);
                self.replay(old, new)?;
                self.leave();
                Some(new)
            }
        };
        // Step 5: the tip and the active chain.
        self.recovered_tip = tip;
        let path = self.path(tip);
        if tip.is_some()
            && path.first().is_none_or(|g| self.index.get(g).is_none_or(|e| e.height != 0))
        {
            return Err(Abort::new(UNABLE_TO_REPLAY));
        }
        self.active = path;
        self.evo = Vec::new();
        let state = self.rget(EVO, K_STATE)?.and_then(|v| hash_of(&v));
        if let Some(s) = state {
            // The durable state at the tip, and below it the states EvoDB's undo restores when
            // activation disconnects (I34): the path's fold, with its own snapshot block.
            let snap = self.cfg.presumed_base.and_then(|b| self.active.get(b as usize)).copied();
            let below = self.active.len().saturating_sub(1);
            let mut prev = [0u8; 32];
            for (h, id) in self.active[..below].iter().enumerate() {
                let snap = (Some(h as u32) == self.cfg.presumed_reader).then_some(snap).flatten();
                prev = evo_fold(&prev, id, snap.as_ref());
                self.evo.push(prev);
            }
            self.evo.push(s);
        }
        self.durable_tip = tip;
        if self.bug(ToyBug::T31) {
            let h = tip.and_then(|t| self.index.get(&t)).map_or(0, |e| e.height);
            let above: Vec<Hash> =
                self.index.values().filter(|e| e.height > h).map(|e| e.hash).collect();
            if !above.is_empty() {
                let mut b = self.chain.batch();
                for id in &above {
                    b.delete(INDEX, id);
                    self.index.remove(id);
                }
                self.write(b, Commit::Durable, "index")?;
            }
        }
        self.leave();
        self.world.mark(Mark::RecoveryEnd);
        self.t_ack = tip;
        // Init's prune check (Core's `PruneAndFlush`), under the locks loaded at step 0.
        self.prune_check = self.cfg.prune.is_some();
        // Step 6: activation of every valid `HAVE_DATA` block above the tip.
        self.activate()?;
        // Init's prune check decides now if no activation connect did, still under the locks
        // loaded at step 0.
        self.maybe_flush()?;
        Ok(())
    }

    /// R5 (c): the files a flush of `epoch` (recovery) entitlement-pruned. Each selection must be
    /// eligible (every file's E record ends at or below the bound, the bound at or below `new − K`
    /// and the lock the epoch's durable locator gives) and followed by a completed chainstate
    /// barrier (its Bk or Bp).
    fn entitled_prunes(&self, refm: &ToyRef, trace: &Trace, epoch: u32) -> BTreeSet<u32> {
        use dash_kv::faulty::TraceEntry;
        let mut out = BTreeSet::new();
        let Some(k) = self.cfg.prune.as_ref().map(|p| p.k) else {
            return out;
        };
        let (mut locator, mut new) = (None, None);
        // Eligible selections by flush, entitled once that flush's own finalizer returned.
        let mut candidates: BTreeMap<u64, Vec<u32>> = BTreeMap::new();
        for e in &trace.entries {
            match e {
                TraceEntry::Mark {
                    epoch: ep,
                    mark,
                    ..
                } if *ep == epoch => match mark {
                    Mark::SubscriberState {
                        durable,
                        ..
                    } => locator = *durable,
                    Mark::PruneDecision {
                        new: n,
                        ..
                    } => new = Some(*n),
                    Mark::PruneSelected {
                        flush,
                        files,
                        bound,
                        lock,
                        ..
                    } => {
                        let Some(n) = new else {
                            continue;
                        };
                        let Some(new_h) = refm.height(&n) else {
                            continue;
                        };
                        let want_lock = sub::entitled_lock(&self.cfg, refm, locator, &n);
                        let eligible = *lock == want_lock
                            && *bound <= new_h.saturating_sub(k)
                            && want_lock.is_none_or(|l| *bound <= l)
                            && files.iter().all(|f| {
                                self.e_files
                                    .get(f)
                                    .and_then(|raw| BlockFileInfo::decode_value(raw).ok())
                                    .is_some_and(|i| i.size > 0 && i.height_last <= *bound)
                            });
                        if eligible {
                            candidates.insert(*flush, files.clone());
                        }
                    }
                    Mark::Log {
                        line,
                    } => {
                        let flush = line.strip_prefix(PRUNE_FINALIZED).and_then(|f| f.parse().ok());
                        if let Some(files) = flush.and_then(|f| candidates.remove(&f)) {
                            out.extend(files);
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        out
    }

    /// R13: every effect written before a later completed chainstate barrier, in its epoch or in
    /// a later one with only process kills between, is present (DEC-101 Q4: the combined trace).
    /// R10: the inode behind each external hardlink still has its content at link time.
    fn effects_and_links(&self, trace: &Trace) -> Vec<Violation> {
        use dash_kv::faulty::{CrashKind, OpKind, TraceEntry};
        let mut v = Vec::new();
        // Effects whose batch may still be pending: a ProcessKill keeps them pending (L4), so a
        // barrier of a later epoch covers them too; a PowerLoss may have lost them.
        let mut pending: Vec<String> = Vec::new();
        let mut covered: BTreeSet<String> = BTreeSet::new();
        for e in &trace.entries {
            match e {
                TraceEntry::Mark {
                    mark: Mark::Log {
                        line,
                    },
                    ..
                } => {
                    if let Some(k) = line.strip_prefix(EFFECT_WRITTEN) {
                        pending.push(k.to_string());
                    }
                    if let Some(rest) = line.strip_prefix(LINKED)
                        && let Some((name, want)) = rest.split_once(' ')
                    {
                        let got = self.world.fs().read_external(Path::new(name));
                        let got = got.map(|b| hex(&sha(&[&b])));
                        if got.as_deref() != Some(want) {
                            v.push(Violation::new(
                                InvariantId::R10,
                                format!("the inode behind {name} changed"),
                            ));
                        }
                    }
                }
                TraceEntry::Event {
                    store,
                    op: OpKind::Barrier | OpKind::SyncWal,
                    failed: false,
                    ..
                } if store == "chainstate" => covered.extend(pending.drain(..)),
                TraceEntry::Crash {
                    kind: CrashKind::PowerLoss,
                    ..
                } => pending.clear(),
                _ => {}
            }
        }
        for k in covered {
            if self.chain.get(EFFECTS, k.as_bytes()).ok().flatten().is_none() {
                v.push(Violation::new(
                    InvariantId::R13,
                    format!("effect {k}, committed before a completed barrier, is lost"),
                ));
            }
        }
        v
    }

    /// T23a/b: recovery adopts the records past each file's durable size, reading only each
    /// record's 8-byte header and the block's parent, height and branch (the toy's generator
    /// stands in for decoding the rest), and extends the info to `len()`. T23a then runs
    /// `sync_data`, which carries the unread sectors' cells into the durable image (rule L2);
    /// T23b does not sync.
    fn adopt_trailing(
        &mut self,
        infos: &mut [BlockFileInfo],
        last_file: u32,
    ) -> Result<Vec<IndexEntry>, Abort> {
        let fs = self.world.fs();
        let mut out = Vec::new();
        for n in (0..infos.len() as u32).filter(|n| *n <= last_file) {
            let path = blocks_path("blk", n);
            let len = match fs.len(&path) {
                Ok(l) => l.unwrap_or(0),
                Err(_) => return Err(self.abort("read", BLOCK_READ_FAILED)),
            };
            let info = &mut infos[n as usize];
            let mut at = u64::from(info.size);
            while at + 8 + 41 <= len {
                let mut head = [0u8; 8 + 41];
                if fs.read_exact_at(&path, at, &mut head).is_err() || head[..4] != MAGIC {
                    break;
                }
                let size = u64::from(u32::from_le_bytes(head[4..8].try_into().expect("4")));
                if at + 8 + size > len {
                    break;
                }
                let parent: Hash = head[12..44].try_into().expect("32 bytes");
                let height = u32::from_le_bytes(head[44..48].try_into().expect("4 bytes"));
                let b = self.blocks.block(&self.cfg, &parent, height, head[48]);
                out.push(IndexEntry {
                    hash: b.id,
                    height,
                    prev: parent,
                    status: STATUS_DATA,
                    file: n,
                    data_pos: (at + 8) as u32,
                    undo_pos: 0,
                });
                info.blocks += 1;
                info.height_first = if info.size == 0 {
                    height
                } else {
                    info.height_first.min(height)
                };
                info.height_last = info.height_last.max(height);
                at += 8 + size;
                info.size = at as u32;
            }
            if self.bug(ToyBug::T23a) && fs.sync_data(&path).is_err() {
                return Err(self.abort("sync", BLOCK_READ_FAILED));
            }
        }
        Ok(out)
    }

    /// S10: hardlinks `blk<n>.dat` into another datadir and logs its content's hash (R10).
    fn link_external(&mut self, n: u32) {
        let fs = self.world.fs();
        let name = PathBuf::from(format!("/oracle/blk{n:05}.dat"));
        fs.link_external(&blocks_path("blk", n), &name);
        let bytes = fs.read_external(&name).unwrap_or_default();
        // R10's control: write into the shared inode behind ST2's link-count rule.
        if self.bug(ToyBug::LinkedWrite) {
            let _ = fs.write_all_at(&blocks_path("blk", n), 0, &[0]);
        }
        self.world.mark(Mark::Log {
            line: format!("{LINKED}{} {}", name.display(), hex(&sha(&[&bytes]))),
        });
    }

    /// S10b: another process, one that never syncs, writes the next `n` blocks and their undo;
    /// the toy connects them and takes the files over with `adopt_existing_files`, so its next
    /// files barrier syncs them, and their directory, before an index record references them
    /// (NoAdopt skips that).
    fn import(&mut self, n: u32) -> Result<(), Abort> {
        // Imported blocks skip `connect_next`: the reference treats them as plain connects.
        let first = self.active.len() as u32;
        assert!(
            !(first..first + n)
                .any(|h| self.cfg.invalid.contains(&h) || self.cfg.effects.contains(&h)),
            "an import carries only valid blocks without effects"
        );
        let state = files_state(&self.files);
        let mut foreign = BlockFiles::with_fs(
            Arc::new(NoSync(self.world.fs())),
            Self::files_cfg(&self.cfg),
            state,
        );
        for _ in 0..n {
            let parent = self.tip().expect("genesis");
            let height = self.active.len() as u32;
            let branch = self.blocks.get(&parent).map_or(0, |b| b.branch);
            let b = self.blocks.block(&self.cfg, &parent, height, branch);
            let err = |_| Abort::new("Failed to import block");
            let pos = foreign.save_block(&b.bytes, height, u64::from(height), None).map_err(err)?;
            let undo = foreign.write_undo(&b.undo, pos.file, height, &parent).map_err(err)?;
            self.index.insert(
                b.id,
                IndexEntry {
                    hash: b.id,
                    height,
                    prev: parent,
                    status: STATUS_DATA | STATUS_UNDO | STATUS_VALID,
                    file: pos.file,
                    data_pos: pos.pos,
                    undo_pos: undo.pos,
                },
            );
            self.dirty.insert(b.id);
            let prev = self.evo.last().copied().unwrap_or([0u8; 32]);
            self.evo.push(evo_fold(&prev, &b.id, None));
            self.active.push(b.id);
            self.world.mark(Mark::Connected {
                block: b.id,
                scripts: reference::scripts(&self.cfg, &b),
                applied: true,
            });
            self.sub_on_connect(b.id)?;
        }
        let state = files_state(&foreign);
        self.files = BlockFiles::with_fs(self.world.fs(), Self::files_cfg(&self.cfg), state);
        if !self.bug(ToyBug::NoAdopt) {
            self.files.adopt_existing_files();
        }
        self.imported = true;
        Ok(())
    }

    fn replay(&mut self, old: Option<Hash>, new: Hash) -> Result<(), Abort> {
        let (op, np) = (self.path(old), self.path(Some(new)));
        let fork = op.iter().zip(&np).take_while(|(a, b)| a == b).count();
        let mut ops = Vec::new();
        let entry = |t: &Self, id: &Hash| {
            t.index.get(id).cloned().ok_or_else(|| Abort::new(UNABLE_TO_REPLAY))
        };
        for h in (fork..op.len()).rev() {
            let e = entry(self, &op[h])?;
            // The startup replay stops on any failed read with Core's message for it
            // (`node/chainstate.cpp:252`, DEC-162 (4)).
            if self.undo_with(&e, UNABLE_TO_REPLAY)?.is_none() {
                return Err(Abort::new(UNABLE_TO_REPLAY));
            }
            ops.push((h as u32, None));
        }
        // T8 replays every HAVE_DATA block above the fork instead of the marker's path.
        let np: Vec<Hash> = if self.bug(ToyBug::T8) {
            let mut above: Vec<&IndexEntry> = self
                .index
                .values()
                .filter(|e| e.status & STATUS_DATA != 0 && e.height as usize >= fork)
                .collect();
            above.sort_by_key(|e| e.height);
            above.dedup_by_key(|e| e.height);
            np[..fork].iter().copied().chain(above.into_iter().map(|e| e.hash)).collect()
        } else {
            np
        };
        for (h, id) in np.iter().enumerate().skip(fork) {
            let e = entry(self, id)?;
            let bytes = self
                .block_with(&e, UNABLE_TO_REPLAY)?
                .filter(|b| b.len() >= self.cfg.block_size as usize);
            let Some(bytes) = bytes else {
                return Err(Abort::new(UNABLE_TO_REPLAY));
            };
            // Blind: the coin's value comes from the block read, nothing in `coins` is read.
            let got = crate::faulty::toy::block::block_id(&bytes[..self.cfg.block_size as usize]);
            ops.push((h as u32, Some(got)));
            self.replayed.push(*id);
        }
        if self.bug(ToyBug::T7) {
            let mut b = self.chain.batch();
            b.delete(META, K_HEAD);
            self.write(b, Commit::Buffered, "replay")?;
        }
        let n = self.cfg.sub_batches.max(1) as usize;
        let per = ops.len().div_ceil(n).max(1);
        for c in ops.chunks(per) {
            let mut b = self.chain.batch();
            Self::put_coins(&mut b, c);
            self.write(b, Commit::Buffered, "replay")?;
        }
        if !self.bug(ToyBug::T27) {
            self.sync_wal()?;
        }
        let mut b = self.chain.batch();
        b.put(META, K_TIP, &new);
        b.delete(META, K_HEAD);
        self.write(b, Commit::Durable, "replay")
    }

    /// Recovery's activation (Core's `ActivateBestChain`): reorg to the most-work candidate, a
    /// block whose whole path has data and no failed block (ties keep the active chain, then the
    /// first branch), connecting along its path; an invalid block on the way is marked failed and
    /// the choice is made again.
    fn activate(&mut self) -> Result<(), Abort> {
        loop {
            let usable =
                |e: &IndexEntry| e.status & STATUS_DATA != 0 && e.status & STATUS_FAILED == 0;
            let mut ok: BTreeMap<Hash, bool> = BTreeMap::new();
            let mut entries: Vec<&IndexEntry> = self.index.values().collect();
            entries.sort_by_key(|e| e.height);
            for e in entries {
                let parent_ok = e.height == 0 || ok.get(&e.prev).copied().unwrap_or(false);
                ok.insert(e.hash, usable(e) && parent_ok && (e.height > 0 || e.prev == NULL));
            }
            let on_active = |h: &Hash| self.active.contains(h);
            let target = self
                .index
                .values()
                .filter(|e| ok[&e.hash])
                .max_by_key(|e| {
                    (
                        e.height,
                        on_active(&e.hash),
                        std::cmp::Reverse(self.blocks.get(&e.hash).map_or(u8::MAX, |b| b.branch)),
                    )
                })
                .map(|e| (e.hash, e.height));
            let Some((target, height)) = target else {
                return Ok(());
            };
            if self.active.len() as u32 > height || self.tip() == Some(target) {
                return Ok(());
            }
            let path = self.path(Some(target));
            let shared = sub::common(&self.active, &path);
            // Core's DisconnectBlock reads each block's undo; a missing one stops recovery.
            self.enter(PATH_DISCONNECT);
            for k in shared..self.active.len() {
                let e = self.index.get(&self.active[k]).cloned();
                let e = e.ok_or_else(|| Abort::new(DISCONNECT_FAILED))?;
                if self.recovery_undo(&e)?.is_none() {
                    return Err(Abort::new(DISCONNECT_FAILED));
                }
            }
            if shared < self.active.len() {
                self.disconnect_to(shared.saturating_sub(1) as u32)?;
            }
            self.leave();
            let mut connected = true;
            for &id in &path[shared..] {
                // Re-learn the block from its data (the toy's generator stands in for decoding).
                let e = self.index[&id].clone();
                self.enter(PATH_CONNECT);
                let Some(bytes) = self.recovery_block(&e)? else {
                    return Err(Abort::new(BLOCK_READ_FAILED));
                };
                let branch = bytes.get(76).copied().unwrap_or(0);
                self.blocks.block(&self.cfg, &e.prev, e.height, branch);
                self.headers = vec![id];
                let next = self.connect_next()?;
                self.leave();
                if !next {
                    connected = false;
                    break;
                }
                // ActivateBestChain's `FlushStateToDisk(PERIODIC)`: a due prune check decides.
                self.maybe_flush()?;
            }
            if connected && self.tip() == Some(target) {
                return Ok(());
            }
        }
    }
}

impl Sut for ToyChain {
    type Config = ToyConfig;
    type Op = ToyOp;
    type Ref = ToyRef;
    const NAME: &'static str = "toychain";

    fn open(world: &World, cfg: &ToyConfig) -> Result<Self, Abort> {
        let chain = world.engine(CHAIN_STORE, &CHAIN_SCHEMA);
        let fs = world.fs();
        let mut t = Self {
            cfg: cfg.clone(),
            world: world.clone(),
            files: BlockFiles::with_fs(fs, Self::files_cfg(cfg), BlockFilesState::default()),
            chain,
            blocks: Gen::default(),
            index: BTreeMap::new(),
            dirty: BTreeSet::new(),
            active: Vec::new(),
            headers: Vec::new(),
            evo: Vec::new(),
            durable_tip: None,
            next_branch: 1,
            flushes: 0,
            presumed_pending: false,
            e_index: Vec::new(),
            e_files: BTreeMap::new(),
            replayed: Vec::new(),
            booted_head: None,
            recovered_tip: None,
            failed: false,
            prune_check: false,
            manual_prune: None,
            deferred_undo: None,
            sub: cfg.sub.map(Sub::new),
            subs: cfg.sub.map(|_| world.engine(sub::SUB_STORE, &sub::SUB_SCHEMA)),
            sub_rec: SubRecovery::default(),
            t_ack: None,
            imported: false,
            paths: Vec::new(),
        };
        t.recover()?;
        t.sub_recover()?;
        if t.active.is_empty() {
            // A fresh chain: connect genesis.
            t.connect(1)?;
        }
        Ok(t)
    }

    fn apply(&mut self, op: &ToyOp) -> Result<(), Abort> {
        if self.failed {
            return Err(Abort::new(FLUSH_FAILED));
        }
        match op {
            ToyOp::Connect(n) => self.connect(*n),
            ToyOp::Accept(n) => {
                for _ in 0..*n {
                    self.accept_next()?;
                }
                Ok(())
            }
            ToyOp::Flush => self.flush(),
            ToyOp::IndexWrite => self.index_write(),
            ToyOp::Reorg {
                fork,
                len,
            } => self.reorg(*fork, *len),
            ToyOp::Continue => {
                self.connect(1)?;
                self.flush()
            }
            ToyOp::Prune(h) => {
                self.manual_prune = Some(*h);
                self.maybe_flush()
            }
            ToyOp::PruneRequest(h) => {
                self.manual_prune = Some(*h);
                Ok(())
            }
            ToyOp::TestValidity => {
                if let Some(&block) = self.headers.first() {
                    self.world.mark(Mark::Connected {
                        block,
                        scripts: ScriptCheck::Full,
                        applied: false,
                    });
                }
                Ok(())
            }
            ToyOp::AutoTarget(t) => {
                self.cfg.prune.as_mut().expect("prune mode").target = Some(*t);
                self.prune_check = true;
                Ok(())
            }
            ToyOp::FlushAt {
                behind,
                during,
            } => self.flush_at(*behind, *during, Some(FlushCause::Scripted)),
            ToyOp::Sync(n) => {
                self.sub_steps(*n, false)?;
                self.maybe_flush()
            }
            ToyOp::CadenceTick => self.sub_tick(),
            ToyOp::Invalidate(h) => self.invalidate(*h),
            ToyOp::LinkExternal(n) => {
                self.link_external(*n);
                Ok(())
            }
            ToyOp::Import(n) => self.import(*n),
            ToyOp::DisableSubscriber => self.sub_disable(),
        }
    }

    fn check(&self, _world: &World, refm: &ToyRef, trace: &Trace) -> Result<(), Vec<Violation>> {
        let mut v = Vec::new();
        let get = |k: &[u8]| self.chain.get(META, k).ok().flatten();
        // R2: markers.
        let tip = get(K_TIP).and_then(|x| hash_of(&x));
        if get(K_HEAD).is_some() {
            v.push(Violation::new(InvariantId::R2, "head_blocks present after recovery"));
        }
        let evo_tip = get(K_EVO_TIP).and_then(|x| hash_of(&x));
        if evo_tip != tip {
            v.push(Violation::new(InvariantId::R2, "evo_tip differs from tip"));
        }
        // R3/R4: the recovered tip is allowed, and the state there is the reference's.
        let (t_ack, allowed) = allowed_tips(trace);
        if !allowed.contains(&tip) {
            v.push(Violation::new(
                InvariantId::R3,
                format!("recovered tip {:?} is not allowed", tip.map(|t| refm.height(&t))),
            ));
            if let (Some(a), Some(t)) = (t_ack, tip)
                && refm.is_ancestor(&t, &a)
                && t != a
            {
                v.push(Violation::new(InvariantId::R4, "recovered behind the acknowledged flush"));
            }
            if tip.is_none() && t_ack.is_some() {
                v.push(Violation::new(
                    InvariantId::R4,
                    "recovered nothing after an acknowledged flush",
                ));
            }
        }
        // The reference has no state on a chain with an invalid block.
        if let Some(t) = tip
            && refm.blocks.get(&t).is_some()
            && refm.blocks.path(&t).iter().any(|b| refm.block(b).is_some_and(|b| !b.valid))
        {
            v.push(Violation::new(
                InvariantId::R3,
                format!("recovered tip {:?} is on a chain with an invalid block", refm.height(&t)),
            ));
        }
        let coins = read_coins(&self.chain);
        if coins_digest(coins.iter().map(|(h, id)| (*h, id))) != refm.coins_digest_at(tip.as_ref())
        {
            v.push(Violation::new(
                InvariantId::R3,
                "coins differ from the reference at the recovered tip",
            ));
        }
        let state = self
            .chain
            .get(EVO, K_STATE)
            .ok()
            .flatten()
            .and_then(|x| hash_of(&x))
            .unwrap_or([0; 32]);
        if tip.is_some() && state != refm.state_at(tip.as_ref()) {
            v.push(Violation::new(
                InvariantId::R3,
                "evo state differs from the reference at the recovered tip",
            ));
        }
        // R1: every durable index entry's data and undo read back as the reference's, within the
        // recorded sizes. A file a crash lost after a failed directory sync is entitled (DEC-212).
        let lost =
            |prefix: &str, file: u32| _world.lost_to_failed_dir_sync(&blocks_path(prefix, file));
        let durable = self.load_index();
        let infos: BTreeMap<u32, BlockFileInfo> = self
            .chain
            .iter_prefix(FILES, b"")
            .filter_map(|r| r.ok())
            .filter_map(|(k, x)| {
                Some((
                    u32::from_be_bytes(k.as_slice().try_into().ok()?),
                    BlockFileInfo::decode_value(&x).ok()?,
                ))
            })
            .collect();
        for e in durable.values() {
            let Some(want) = refm.block(&e.hash) else {
                v.push(Violation::new(InvariantId::R1, "an index entry for an unknown block"));
                continue;
            };
            let info = infos.get(&e.file).copied().unwrap_or_default();
            if e.status & STATUS_DATA != 0 {
                let end = u64::from(e.data_pos) + want.bytes.len() as u64;
                if e.data_pos < BLOCK_SERIALIZATION_HEADER_SIZE || end > u64::from(info.size) {
                    v.push(Violation::new(
                        InvariantId::R1,
                        format!("block {} lies outside its file's recorded size", e.height),
                    ));
                }
                // An unreadable block is unavailable, not lost (DEC-148): D1 judges whether its
                // bytes are durable; a read that succeeds must return them.
                match self.read_block(e) {
                    Ok(Some(b)) if b == want.bytes.as_slice() => {}
                    Err(Unavailable) => {}
                    Ok(None) if lost("blk", e.file) => {}
                    _ => v.push(Violation::new(
                        InvariantId::R1,
                        format!("block {} does not read back", e.height),
                    )),
                }
            }
            if e.status & STATUS_UNDO != 0 {
                let end =
                    u64::from(e.undo_pos) + want.undo.len() as u64 + u64::from(UNDO_CHECKSUM_SIZE);
                if e.undo_pos < BLOCK_SERIALIZATION_HEADER_SIZE || end > u64::from(info.undo_size) {
                    v.push(Violation::new(
                        InvariantId::R1,
                        format!("undo {} lies outside its file's recorded size", e.height),
                    ));
                }
                match self.read_undo(e) {
                    Ok(Some(u)) if u == want.undo.as_slice() => {}
                    Err(Unavailable) => {}
                    Ok(None) if lost("rev", e.file) => {}
                    _ => v.push(Violation::new(
                        InvariantId::R1,
                        format!("undo {} does not read back", e.height),
                    )),
                }
            }
        }
        let epoch = _world.epoch();
        // The blocks this epoch connected (applied), with their script-check decisions.
        let connected_now: BTreeMap<Hash, ScriptCheck> = trace
            .marks_with_epoch()
            .filter_map(|(e, m)| match m {
                Mark::Connected {
                    block,
                    scripts,
                    applied: true,
                } if e == epoch => Some((*block, *scripts)),
                _ => None,
            })
            .collect();
        let entitled = self.entitled_prunes(refm, trace, epoch);
        // R5: every block_files and block_index record of E survives recovery with E's value,
        // except entitled changes: growth of files this epoch wrote, undo and validity added by
        // this epoch's connects, the reference's failed marks, and R5 (c)'s prune resets.
        let written = files_written(trace, epoch);
        let top = connected_now.keys().filter_map(|b| refm.height(b)).max();
        for (n, raw) in &self.e_files {
            let Ok(was) = BlockFileInfo::decode_value(raw) else {
                continue;
            };
            let now = infos.get(n).copied();
            let ok = match now {
                Some(now) if now == was => true,
                Some(now) if entitled.contains(n) && now == BlockFileInfo::default() => true,
                Some(now) => {
                    written.contains(n)
                        && now.size >= was.size
                        && now.undo_size >= was.undo_size
                        && now.blocks >= was.blocks
                        && (was.size == 0
                            || (now.height_first == was.height_first
                                && now.time_first == was.time_first))
                        && now.height_last >= was.height_last
                        && now.time_last >= was.time_last
                        && (now.height_last == was.height_last
                            || top.is_some_and(|t| now.height_last <= t))
                }
                None => false,
            };
            if !ok {
                v.push(Violation::new(
                    InvariantId::R5,
                    format!("file record {n} changed by recovery: {now:?}, E had {was:?}"),
                ));
            }
        }
        for e in &self.e_index {
            let Some(now) = durable.get(&e.hash) else {
                v.push(Violation::new(
                    InvariantId::R5,
                    format!("index entry {} lost by recovery", e.height),
                ));
                continue;
            };
            // A block's height, parent and file never change; a prune resets only its data and
            // undo flags and positions (CP3-2).
            let immutable = now.height == e.height && now.prev == e.prev && now.file == e.file;
            let pruned = entitled.contains(&e.file)
                && now.status & (STATUS_DATA | STATUS_UNDO) == 0
                && now.status == e.status & !(STATUS_DATA | STATUS_UNDO)
                && now.data_pos == 0
                && now.undo_pos == 0;
            let connected = connected_now.contains_key(&e.hash);
            let invalid = refm.block(&e.hash).is_some_and(|b| !b.valid);
            let mut allowed_new = 0;
            if connected {
                allowed_new |= STATUS_UNDO | STATUS_VALID;
            }
            if invalid {
                allowed_new |= STATUS_FAILED;
            }
            let undo_ok = now.undo_pos == e.undo_pos
                || (e.status & STATUS_UNDO == 0 && connected && now.status & STATUS_UNDO != 0);
            let kept = now.data_pos == e.data_pos
                && undo_ok
                && now.status & e.status == e.status
                && now.status & !e.status & !allowed_new == 0;
            if !immutable || !(pruned || kept) {
                v.push(Violation::new(
                    InvariantId::R5,
                    format!("index entry {} changed by recovery", e.height),
                ));
            }
        }
        // R6 (a): every block on a flushed transition's connect leg was connected (applied)
        // before its flush began, with the reference's script-check decision; `applied = false`
        // connects (fJustCheck) do not count.
        let mut applied: BTreeMap<Hash, ScriptCheck> = BTreeMap::new();
        for m in trace.marks() {
            match m {
                Mark::Connected {
                    block,
                    scripts,
                    applied: true,
                } => {
                    applied.insert(*block, *scripts);
                }
                Mark::FlushBegin {
                    old,
                    new,
                    ..
                } => {
                    let np = refm.blocks.path(new);
                    let op = old.map(|o| refm.blocks.path(&o)).unwrap_or_default();
                    let fork = sub::common(&np, &op);
                    for b in &np[fork..] {
                        let why = match applied.get(b) {
                            None => "was never connected before its flush",
                            Some(sc) if Some(*sc) != refm.scripts(b) => {
                                "was connected with the wrong script-check decision"
                            }
                            Some(_) => continue,
                        };
                        v.push(Violation::new(
                            InvariantId::R6,
                            format!("flushed block {:?} {why}", refm.height(b)),
                        ));
                        break;
                    }
                }
                _ => {}
            }
        }
        // R6 (b): the replay connected exactly the booted marker's `old → fork → new` path,
        // each of those blocks once applied with the reference's decision.
        let expected: Vec<Hash> = match self.booted_head {
            Some((new, old)) if refm.blocks.get(&new).is_some() => {
                let np = refm.blocks.path(&new);
                let op = old.filter(|o| refm.blocks.get(o).is_some()).map(|o| refm.blocks.path(&o));
                let fork = op.map_or(0, |op| sub::common(&np, &op));
                np[fork..].to_vec()
            }
            _ => Vec::new(),
        };
        if self.replayed != expected {
            v.push(Violation::new(
                InvariantId::R6,
                format!(
                    "the replay connected {} blocks, the marker's path has {}",
                    self.replayed.len(),
                    expected.len()
                ),
            ));
        }
        for id in &self.replayed {
            if applied.get(id).is_none_or(|sc| Some(*sc) != refm.scripts(id)) {
                v.push(Violation::new(
                    InvariantId::R6,
                    "a replayed block was never connected with the reference's decision",
                ));
            }
        }
        // R6 (c): every activation connect (a block of the final chain off the recovered tip's
        // path, a recovery reorg's included) has an applied mark in this epoch with the
        // reference's decision.
        let base: BTreeSet<Hash> = self.path(self.recovered_tip).into_iter().collect();
        for id in self.active.iter().filter(|id| !base.contains(*id)) {
            if connected_now.get(id).is_none_or(|sc| Some(*sc) != refm.scripts(id)) {
                v.push(Violation::new(
                    InvariantId::R6,
                    format!(
                        "activation connect {:?} has no applied mark in recovery",
                        refm.height(id)
                    ),
                ));
            }
        }
        // Equal work is a tie Core keeps its tip through: compare heights.
        let best = refm.best_valid(&self.e_index);
        if best.is_some() {
            let h = |x: Option<Hash>| x.and_then(|t| refm.height(&t));
            let (tip_h, best_h) = (h(self.tip()), h(best));
            if tip_h != best_h {
                v.push(Violation::new(
                    InvariantId::R6,
                    format!(
                        "the final tip is at {tip_h:?}, the best valid tip of E is at {best_h:?}"
                    ),
                ));
            }
        }
        for e in self.index.values() {
            if refm.block(&e.hash).is_some_and(|b| !b.valid) && e.status & STATUS_VALID != 0 {
                v.push(Violation::new(InvariantId::R6, "an invalid block is marked valid"));
            }
        }
        // R9: after the startup scan, no pruned file (a durable info of size 0 before the last
        // file) is left on disk; every durable HAVE_DATA references a present file (R1 reads it).
        if self.chain.get(META, K_PRUNED).ok().flatten().is_some() {
            let last = self.files.last_file();
            for (n, info) in &infos {
                if *n < last && info.size == 0 {
                    for prefix in ["blk", "rev"] {
                        if self.world.fs().len(&blocks_path(prefix, *n)).ok().flatten().is_some() {
                            v.push(Violation::new(
                                InvariantId::R9,
                                format!(
                                    "pruned {prefix}{n:05}.dat is still on disk after the scan"
                                ),
                            ));
                        }
                    }
                }
            }
        }
        v.extend(self.sub_check(_world, refm, trace));
        v.extend(self.effects_and_links(trace));
        let _ = sha;
        if v.is_empty() {
            Ok(())
        } else {
            Err(v)
        }
    }

    /// R2 on the booted stores, before recovery: a marker names the evo tip, or with no marker
    /// the tip is the evo tip.
    fn store_check(world: &World, _: &ToyConfig) -> Vec<Violation> {
        let chain = world.engine(CHAIN_STORE, &CHAIN_SCHEMA);
        // A failed read is the checker's own failure, never an absent marker.
        let read = |k: &[u8]| chain.get(META, k);
        let (Ok(evo_tip), Ok(head), Ok(tip)) = (read(K_EVO_TIP), read(K_HEAD), read(K_TIP)) else {
            return vec![Violation::new(InvariantId::Model, "a store-level check's read failed")];
        };
        let ok = match head {
            Some(head) => evo_tip.as_deref() == head.get(..32),
            None => tip == evo_tip,
        };
        if ok {
            Vec::new()
        } else {
            vec![Violation::new(
                InvariantId::R2,
                "the booted markers disagree: evo is not at the tip or the marker's new",
            )]
        }
    }

    /// FA1 and FA2 (§6.2.1) over the forward run's prune decisions and selections.
    fn forward_check(&self, _world: &World, _refm: &ToyRef, trace: &Trace) -> Vec<Violation> {
        let mut v = Vec::new();
        // The open decision: (D, automatic, met_target).
        let mut decision: Option<(Vec<u32>, bool, bool)> = None;
        let mut expect_flush = false;
        for m in trace.marks() {
            match m {
                Mark::PruneDecision {
                    due,
                    dry_run,
                    met_target,
                    flush_for_prune,
                    full_flush,
                    ..
                } => {
                    if expect_flush {
                        v.push(Violation::new(InvariantId::FA1, "a due full flush did not happen"));
                    }
                    // FA1: the prune cause is exactly a non-empty dry run.
                    if *flush_for_prune == dry_run.is_empty()
                        || full_flush.contains(&FlushCause::Prune) != *flush_for_prune
                    {
                        v.push(Violation::new(InvariantId::FA1, "flush_for_prune is not `D != ∅`"));
                    }
                    let auto = matches!(due, PruneDue::Automatic);
                    // A decision with no cause closes at once; with one, the next flush is it.
                    decision = Some((dry_run.clone(), auto, *met_target));
                    expect_flush = !full_flush.is_empty();
                }
                Mark::FlushBegin {
                    ..
                } => expect_flush = false,
                Mark::PruneSelected {
                    files,
                    rearmed,
                    target_unmet,
                    ..
                } => {
                    let Some((d, auto, met)) = decision.as_ref() else {
                        v.push(Violation::new(InvariantId::FA2, "a selection without a decision"));
                        continue;
                    };
                    // FA2: the selection is a subset of D.
                    if !files.iter().all(|f| d.contains(f)) {
                        v.push(Violation::new(
                            InvariantId::FA2,
                            format!("selection {files:?} is not within D = {d:?}"),
                        ));
                    }
                    // FA2: re-armed iff the target is still unmet while D had met it.
                    if *rearmed != (*auto && *met && *target_unmet) {
                        v.push(Violation::new(InvariantId::FA2, format!("re-armed {rearmed} after a selection that left the target unmet: {target_unmet}")));
                    }
                }
                _ => {}
            }
        }
        if expect_flush {
            v.push(Violation::new(InvariantId::FA1, "a due full flush did not happen"));
        }
        v.extend(sub::forward_subscriber(&self.cfg, _refm, trace));
        v
    }

    fn continue_after_recovery(&mut self) -> Option<ToyOp> {
        Some(ToyOp::Continue)
    }

    fn has_continuation() -> bool {
        true
    }

    fn extractor(cfg: &ToyConfig) -> Option<Arc<dyn RefExtractor>> {
        Some(Arc::new(ToyRefs {
            block: cfg.block_size,
            undo: cfg.undo_size,
        }))
    }

    /// Core's message per call path (DEC-162 (4), DEC-165 (3)), at v24.0.0-rc.3. The innermost
    /// path decides, except that a chainstate engine error anywhere inside `LoadChainstate`
    /// throws out of it, the replay included:
    ///
    /// | path | first fatal error | Core's message |
    /// |---|---|---|
    /// | `ReplayBlocks` | block or undo read | "Unable to replay blocks…" (`node/chainstate.cpp:252`) |
    /// | `LoadChainstate`, `ReplayBlocks` | chainstate engine | "Error opening block database" (`dbwrapper.cpp:261`/`:268` throws, `init.cpp:2033`–`2037` catches) |
    /// | `DisconnectTip` | undo read | "Failed to disconnect block…" (`validation.cpp:2300`, `:3282`, `:3621`) |
    /// | `ConnectTip` | block read | "Failed to read block" (`validation.cpp:3412`) |
    /// | `ConnectTip` | chainstate read | "Error reading from database, shutting down." (`init.cpp:2089`) |
    /// | `BaseIndex::ThreadSync` | block read | "ThreadSync: Failed to read block …" (`index/base.cpp:183`) |
    /// | `CoinStatsIndex::CustomAppend` | undo read | "ThreadSync: Failed to write block …" (`index/coinstatsindex.cpp:129`, `index/base.cpp:190`) |
    /// | `BaseIndex::Rewind` | block or undo read | "ThreadSync: Failed to rewind index …" (`index/coinstatsindex.cpp:287`/`:413`, `index/base.cpp:173`) |
    ///
    /// Anything else falls to [`Sut::sanctioned`].
    fn sanctioned_after(cfg: &ToyConfig, message: &str, cause: &FatalCause) -> bool {
        let chainstate = cause.store.as_deref() == Some(CHAIN_STORE);
        let fs_read = cause.read && cause.fs();
        if cause.within(PATH_LOAD) && chainstate {
            return message == DB_OPEN_FAILED;
        }
        match cause.path() {
            Some(PATH_REPLAY) if fs_read => message == UNABLE_TO_REPLAY,
            Some(PATH_DISCONNECT) if fs_read => message == DISCONNECT_FAILED,
            Some(PATH_CONNECT) if fs_read => message == BLOCK_READ_FAILED,
            Some(PATH_CONNECT) if chainstate && cause.read => message == DB_READ_FAILED,
            Some(p) if fs_read && p != PATH_LOAD => sub::sync_sanctioned(p, message),
            _ => Self::sanctioned(cfg, message),
        }
    }

    /// The messages off those paths: the coins flush (`validation.cpp:3093`), a coins read
    /// (`init.cpp:2089`), the flat-file flush (`node/blockstorage.cpp:600`), and block and undo
    /// writes (`node/blockstorage.cpp:869`, `:784`). Two are the toy's, not Core's on that path:
    /// it reports every non-coins chainstate read (the EFFECTS snapshot) and its index store's
    /// reads with the coins reader's message, and it stops on a failed index commit with the line
    /// Core only logs (`index/base.cpp:232`).
    fn sanctioned(_: &ToyConfig, message: &str) -> bool {
        message == FLUSH_FAILED
            || message == DB_READ_FAILED
            || message.starts_with("Flushing block file")
            || message == "Failed to write block"
            || message == "Failed to write undo data"
            || message == sub::INDEX_FAILED
    }

    fn projection(&self) -> Option<Hash> {
        let coins = read_coins(&self.chain);
        let state = self.chain.get(EVO, K_STATE).ok().flatten().unwrap_or_default();
        Some(sha(&[
            &self.tip().unwrap_or(NULL),
            &coins_digest(coins.iter().map(|(h, id)| (*h, id))),
            &state,
        ]))
    }
}

/// A block-file read that failed with an I/O error other than a short or missing file: the
/// bytes are unavailable (EIO), not absent.
struct Unavailable;

/// A block-file read: `Ok(None)` for missing or corrupt data.
fn block_read(
    r: crate::flatfile::FlatResult<()>,
    out: Vec<u8>,
) -> Result<Option<Vec<u8>>, Unavailable> {
    use crate::flatfile::FlatFileError;
    match r {
        Ok(()) => Ok(Some(out)),
        Err(FlatFileError::Io {
            source,
            ..
        }) if !matches!(source.kind(), io::ErrorKind::UnexpectedEof | io::ErrorKind::NotFound) => {
            Err(Unavailable)
        }
        Err(_) => Ok(None),
    }
}

/// A filesystem view for a process that never syncs (S10b's foreign writer).
struct NoSync(Arc<FaultyFlatFiles>);

impl FlatFs for NoSync {
    fn link_count(&self, p: &Path) -> io::Result<Option<u64>> {
        self.0.link_count(p)
    }
    fn len(&self, p: &Path) -> io::Result<Option<u64>> {
        self.0.len(p)
    }
    fn create(&self, p: &Path) -> io::Result<()> {
        self.0.create(p)
    }
    fn open_read(&self, p: &Path) -> io::Result<Box<dyn FlatRead + '_>> {
        self.0.open_read(p)
    }
    fn read_exact_at(&self, p: &Path, off: u64, buf: &mut [u8]) -> io::Result<()> {
        self.0.read_exact_at(p, off, buf)
    }
    fn write_all_at(&self, p: &Path, off: u64, data: &[u8]) -> io::Result<()> {
        self.0.write_all_at(p, off, data)
    }
    fn allocate(&self, p: &Path, off: u64, len: u64) {
        self.0.allocate(p, off, len)
    }
    fn truncate(&self, p: &Path, len: u64) -> io::Result<()> {
        self.0.truncate(p, len)
    }
    fn sync_data(&self, _: &Path) -> io::Result<()> {
        Ok(())
    }
    fn sync_dir(&self, _: &Path) -> io::Result<()> {
        Ok(())
    }
    fn copy(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.0.copy(from, to)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.0.rename(from, to)
    }
    fn remove(&self, p: &Path) -> io::Result<bool> {
        self.0.remove(p)
    }
    fn create_dir_all(&self, d: &Path) -> io::Result<()> {
        self.0.create_dir_all(d)
    }
    fn available_space(&self, d: &Path) -> io::Result<u64> {
        self.0.available_space(d)
    }
}

/// The toy's dependency declarations (§6.2 "The toy's `RefExtractor`").
struct ToyRefs {
    block: u32,
    undo: u32,
}

fn blocks_path(prefix: &str, file: u32) -> PathBuf {
    PathBuf::from(format!("{BLOCKS_DIR}/{prefix}{file:05}.dat"))
}

/// A copy of a block-file set's infos and current file.
fn files_state<F: FlatFs>(files: &BlockFiles<F>) -> BlockFilesState {
    BlockFilesState {
        infos: files.infos().to_vec(),
        last_file: files.last_file(),
    }
}

/// The marker `[new, old]`'s `new`.
fn marker_new(head: &[u8]) -> Option<Hash> {
    hash_of(&head[..32.min(head.len())])
}

/// The blk and rev file numbers `epoch` wrote.
fn files_written(trace: &Trace, epoch: u32) -> BTreeSet<u32> {
    use dash_kv::faulty::{OpKind, TraceEntry};
    trace
        .entries
        .iter()
        .filter_map(|e| match e {
            TraceEntry::Event {
                id,
                op: OpKind::WriteAt,
                detail,
                ..
            } if id.epoch == epoch => {
                let name = detail.rsplit('/').next()?;
                let digits = name.strip_prefix("blk").or_else(|| name.strip_prefix("rev"))?;
                digits.strip_suffix(".dat")?.parse().ok()
            }
            _ => None,
        })
        .collect()
}

/// The path from genesis to `tip` through durable index entries; a block with no durable entry
/// ends it (and a requirement on it then fails, D1(b)). `None` if an entry is undecided.
fn durable_path(view: &WorldDurableView, tip: Hash) -> Option<Vec<Hash>> {
    let mut p = Vec::new();
    let mut cur = tip;
    while cur != NULL {
        p.push(cur);
        match view.get("chainstate", INDEX, &cur) {
            Peek::Known(Some(v)) => cur = IndexEntry::decode(&cur, &v)?.prev,
            Peek::Known(None) => break,
            Peek::Unknown => return None,
        }
    }
    p.reverse();
    Some(p)
}

/// G, the chainstate's durable tip: the durable marker's `new`, else `meta.tip` (Opus r3 5).
fn durable_tip(view: &WorldDurableView) -> Peek<Option<Hash>> {
    match view.get("chainstate", META, K_HEAD) {
        Peek::Unknown => Peek::Unknown,
        Peek::Known(Some(h)) => Peek::Known(marker_new(&h)),
        Peek::Known(None) => match view.get("chainstate", META, K_TIP) {
            Peek::Unknown => Peek::Unknown,
            Peek::Known(t) => Peek::Known(t.and_then(|t| hash_of(&t))),
        },
    }
}

impl ToyRefs {
    /// The durable locator L requires each block, with undo, on L → `LCA(L, G)` → G, and that
    /// it is not ahead of G (§5.3). Data records require nothing.
    fn subscriber_refs(
        &self,
        cf: Cf,
        key: &[u8],
        value: &[u8],
        view: &WorldDurableView,
    ) -> Vec<Requirement> {
        if cf != sub::LOCATOR || key != sub::K_LOCATOR {
            return Vec::new();
        }
        // A disabled index holds no lock: its locator requires nothing (S11c). While the record
        // is undecided the requirements stand (§5.3's conservative direction).
        if let Peek::Known(Some(_)) = view.get(sub::SUB_STORE, sub::LOCATOR, sub::K_DISABLED) {
            return Vec::new();
        }
        let Some((_, _, l)) = sub::decode_locator(value) else {
            return Vec::new();
        };
        let Some(&tip) = l.last() else {
            return Vec::new();
        };
        let mut out = vec![Requirement::DurableTipAtLeast {
            store: "chainstate",
            hash: tip,
        }];
        let g = match durable_tip(view) {
            Peek::Known(g) => g,
            Peek::Unknown => return out,
        };
        let gp = match g {
            Some(g) => match durable_path(view, g) {
                Some(p) => p,
                None => return out,
            },
            None => Vec::new(),
        };
        let f = sub::common(&l, &gp);
        out.extend(block_reqs(&l[f..], true).chain(block_reqs(&gp[f..], true)));
        out
    }
}

/// A block requirement on each of `hashes`, with undo or on its data only.
fn block_reqs(hashes: &[Hash], undo: bool) -> impl Iterator<Item = Requirement> + '_ {
    hashes.iter().map(move |&hash| Requirement::Block {
        hash,
        undo,
    })
}

impl RefExtractor for ToyRefs {
    fn refs(
        &self,
        db: &str,
        cf: Cf,
        key: &[u8],
        value: &[u8],
        view: &WorldDurableView,
    ) -> Vec<Requirement> {
        if db == sub::SUB_STORE {
            return self.subscriber_refs(cf, key, value, view);
        }
        if db != "chainstate" {
            return Vec::new();
        }
        let mut out = Vec::new();
        if cf == META && key == K_HEAD && value.len() == 64 {
            // The marker `[new, old]` needs its replay path: undo for the disconnects from `old`
            // down to the fork, data for the connects up to `new`.
            let new: Hash = value[..32].try_into().expect("32 bytes");
            let old: Hash = value[32..].try_into().expect("32 bytes");
            if let (Some(np), Some(op)) = (durable_path(view, new), durable_path(view, old)) {
                let fork = sub::common(&np, &op);
                out.extend(block_reqs(&op[fork..], true));
                out.extend(block_reqs(&np[fork..], false));
            }
        }
        if cf == INDEX
            && let Some(e) = IndexEntry::decode(key, value)
        {
            if e.status & STATUS_DATA != 0 && e.data_pos >= 8 {
                let len = 8 + u64::from(self.block);
                out.push(Requirement::Bytes(FileRef::new(
                    blocks_path("blk", e.file),
                    u64::from(e.data_pos - 8),
                    len,
                )));
            }
            if e.status & STATUS_UNDO != 0 && e.undo_pos >= 8 {
                let len = 8 + u64::from(self.undo) + u64::from(UNDO_CHECKSUM_SIZE);
                out.push(Requirement::Bytes(FileRef::new(
                    blocks_path("rev", e.file),
                    u64::from(e.undo_pos - 8),
                    len,
                )));
            }
        }
        out
    }

    /// A block requirement needs a durable index entry with a data position (and with undo,
    /// an undo position too); its bytes are the entry's own requirement.
    fn resolve(&self, req: &Requirement, view: &WorldDurableView) -> Result<Vec<FileRef>, String> {
        match req {
            Requirement::Bytes(r) => Ok(vec![r.clone()]),
            Requirement::DurableTipAtLeast {
                hash,
                ..
            } => {
                let Peek::Known(Some(v)) = view.get(sub::SUB_STORE, sub::LOCATOR, sub::K_LOCATOR)
                else {
                    return Ok(Vec::new());
                };
                let Some((_, _, l)) =
                    sub::decode_locator(&v).filter(|(_, _, l)| l.last() == Some(hash))
                else {
                    return Ok(Vec::new());
                };
                match durable_tip(view) {
                    Peek::Unknown => Ok(Vec::new()),
                    Peek::Known(None) => {
                        Err("the subscriber's locator is ahead of an empty chainstate".into())
                    }
                    Peek::Known(Some(g)) => match view.get("chainstate", INDEX, &g) {
                        Peek::Known(Some(e)) => {
                            let gh = IndexEntry::decode(&g, &e)
                                .ok_or("a corrupt index entry")?
                                .height as usize;
                            if l.len() > gh + 1 && l[gh] == g {
                                Err(format!(
                                    "the subscriber's locator at {} is ahead of the chainstate's durable tip at {gh}",
                                    l.len() - 1
                                ))
                            } else {
                                Ok(Vec::new())
                            }
                        }
                        _ => Ok(Vec::new()),
                    },
                }
            }
            Requirement::Block {
                hash,
                undo,
            } => match view.get("chainstate", INDEX, hash) {
                Peek::Unknown => Ok(Vec::new()),
                Peek::Known(None) => Err("a replay-path block has no durable index entry".into()),
                Peek::Known(Some(v)) => {
                    let e = IndexEntry::decode(hash, &v).ok_or("a corrupt index entry")?;
                    let need = if *undo {
                        STATUS_DATA | STATUS_UNDO
                    } else {
                        STATUS_DATA
                    };
                    if e.status & need != need {
                        return Err(format!(
                            "replay-path block {} has no durable {} position",
                            e.height,
                            if *undo {
                                "undo"
                            } else {
                                "data"
                            }
                        ));
                    }
                    Ok(Vec::new())
                }
            },
        }
    }

    fn finalizes(&self, db: &str, cf: Cf, key: &[u8], value: Option<&[u8]>) -> Option<Vec<Scope>> {
        // A locator commit presumes the subscriber's data batches since its previous one.
        if db == sub::SUB_STORE && cf == sub::LOCATOR && key == sub::K_LOCATOR && value.is_some() {
            return Some(vec![Scope::SincePreviousLocator {
                store: sub::SUB_STORE,
            }]);
        }
        if db != "chainstate" || cf != META {
            return None;
        }
        match (key, value) {
            // Bk, a one-batch B1 and recovery's final batch: `tip = new`. They presume this
            // flush's (or recovery's) coin sub-batches, a pending presumed effect, and the
            // previous chainstate finalizer.
            (K_TIP, Some(_)) => Some(vec![
                Scope::SinceFlushStart,
                Scope::Keys {
                    cf: EFFECTS,
                    range: K_SNAP.to_vec()..b"snaq".to_vec(),
                },
                Scope::PreviousFinalizer,
            ]),
            // B1: the marker (or the one-batch flush) presumes pending effects and the previous
            // chainstate finalizer.
            (K_HEAD, Some(_)) => Some(vec![
                Scope::Keys {
                    cf: EFFECTS,
                    range: K_SNAP.to_vec()..b"snaq".to_vec(),
                },
                Scope::PreviousFinalizer,
            ]),
            _ => None,
        }
    }
}
