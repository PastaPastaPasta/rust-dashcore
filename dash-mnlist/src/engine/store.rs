//! Persistence (SB0 §5.3 `take_dirty`, §9.5; D38): the engine's rows in the host's batch with its
//! locator, and the engine rebuilt from them.
//!
//! Rows name blocks by height and hash, never by [`BlockId`]: a lost buffered suffix may issue an
//! id again (SB0 E7). [`MasternodeListEngine::load`] resolves each through the chain: rows whose
//! locator it does not know are refused, for the host to [`MasternodeListEngine::reset`]; with it
//! known, a row on a block it does not know is dropped, and the next
//! [`MasternodeListEngine::take_dirty`] deletes it.
//!
//! Layouts (each key starts with its table's tag; integers in keys are big-endian, so key order is
//! height order):
//!
//! | Table | Key | Value |
//! |---|---|---|
//! | lists | `L` ‖ height u32 ‖ block hash | flags u8 (1 authenticated, 2 whole) ‖ set hash ‖ [coinbase ‖ base height u32 ‖ base hash ‖ removed ‖ changed] |
//! | lists | `B` ‖ height u32 ‖ block hash | entries (a base, D48) |
//! | lists | `S` ‖ set hash | set (D48) |
//! | quorums | `Q` ‖ commitment hash | first seen u32 ‖ flags u8 (1 authenticated) ‖ state u8 (0 not run, 1 `Verified`, 2 `Unverified`, 3 `Unauthenticated`) ‖ decider ‖ replaced ‖ mined ‖ held to (2: the sets', while retention flags it so, D48; else an optional height) ‖ commitment |
//! | snapshots | `R` ‖ type u8 ‖ cycle hash | 0 ‖ row (Core's), or 1 ‖ count ‖ rows (candidates) |
//! | snapshots | `M` ‖ name | a mark: `best_cl`, `cl_base`, `last_diff`, `retained` (height ‖ hash), `pending_cl` (the `clsig`) |
//!
//! A set is a VARINT count of (type u8 ‖ quorum hash ‖ commitment hash), keyed by its `sha256d`;
//! the coinbase a tag (0 no transactions, 1 the transaction, VARINT-length-prefixed) and the
//! entries a VARINT count of VARINT-length-prefixed wire entries (`nVersion` ‖ `CalcHash`'s
//! pre-image, base-sdk's `SimplifiedMnListEntry`). A whole list is its base's entries less the
//! removed (a VARINT count of proRegTx hashes) and with the changed (entries as a base holds
//! them) added or replacing the base's. An optional mark is 0, or 1 ‖ height u32 ‖ hash; an
//! optional height 0, or 1 ‖ u32. Fixed integers in values are little-endian.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use dash_chain::BlockRef;
use dash_consensus::BlockId;
use dash_consensus::hash::sha256d;
use dash_consensus::primitives::Tx;
use dash_evo::cbtx::{CoinbaseFacts, check_mn_list_root, check_quorums_root};
use dash_evo::llmq::{QuorumSnapshot, VerifiedQuorum};
use dash_evo::sml::{SmlEntry, SmlList, merkle_root_quorums_from_hashes};
use dash_evo::types::ProTxHash;
use dash_kv::codec::{CodecError, read_varint_u64, write_varint};
use dash_kv::{Cf, KvBatch, KvRead};
use dash_p2p_core::{ClSig, SimplifiedMnListEntry};
use dash_primitives::{BlockHash, Commitment, Transaction};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;

use super::{
    Full, HeldList, MasternodeListEngine, Mode, QuorumSet, QuorumState, ROW_CANDIDATES, Record,
    SetMark, newest_holders,
};
use crate::chain::MnChain;
use crate::convert::{commitment_hash, sml_entry};
use crate::engine::cbtx_payload;
use crate::error::{MnLoadError, MnViewError};
use crate::view::{HeldRow, RowKey};

const LIST: u8 = b'L';
/// A list base's entries, which whole list rows are stored as deltas against (D48).
const BASE: u8 = b'B';
/// A quorum set, keyed by its hash: lists holding one set name it once (D48).
const SET: u8 = b'S';
const QUORUM: u8 = b'Q';
const ROW: u8 = b'R';
const MARK: u8 = b'M';

const BEST_CL: &[u8] = b"best_cl";
const CL_BASE: &[u8] = b"cl_base";
const PENDING_CL: &[u8] = b"pending_cl";
const LAST_DIFF: &[u8] = b"last_diff";
const RETAINED: &[u8] = b"retained";

/// A whole list is stored against a base at most this far below it, as Core's evodb keeps a
/// list snapshot every `DISK_SNAPSHOT_PERIOD` blocks (`evo/deterministicmns.h` v24.0.0-rc.3:760;
/// D48).
const BASE_SPAN: u32 = 576;

/// The `held_to` tag of a record not replaced: the newest set holding it, read at load (D48).
const HELD_BY_SETS: u8 = 2;

/// The host's column families for the engine's tables (the SPV store's `mn_lists`, `mn_quorums`
/// and `mn_snapshots`, SB13).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MnCfs {
    pub lists: Cf,
    pub quorums: Cf,
    pub snapshots: Cf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Table {
    Lists,
    Quorums,
    Snapshots,
}

impl Table {
    fn cf(self, cfs: MnCfs) -> Cf {
        match self {
            Table::Lists => cfs.lists,
            Table::Quorums => cfs.quorums,
            Table::Snapshots => cfs.snapshots,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Table::Lists => "mn_lists",
            Table::Quorums => "mn_quorums",
            Table::Snapshots => "mn_snapshots",
        }
    }
}

/// The engine's write set since the last [`MasternodeListEngine::take_dirty`] or
/// [`MasternodeListEngine::load`] (SB0 §9.5): puts and deletes, pruning included, never a cleared
/// table. The host commits it with [`locator`](Self::locator) in one batch; a failed commit is
/// fatal to the store (SB10's fail-stop), so the engine takes it as written.
#[derive(Debug, Default)]
pub struct MnWrites {
    ops: Vec<(Table, Vec<u8>, Option<Vec<u8>>)>,
    locator: Option<(BlockHash, u32)>,
}

impl MnWrites {
    /// Whether no row changed.
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// How many rows are put or deleted.
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// The block the engine's rows reach: the active tip its last retention ran on, for the host's
    /// `locators` row. `None` before the first retention.
    pub fn locator(&self) -> Option<(BlockHash, u32)> {
        self.locator
    }

    /// Puts and deletes the rows into `batch` under `cfs`.
    pub fn write_into(&self, batch: &mut impl KvBatch, cfs: MnCfs) {
        for (table, key, value) in &self.ops {
            match value {
                Some(v) => batch.put(table.cf(cfs), key, v),
                None => batch.delete(table.cf(cfs), key),
            }
        }
    }

    fn put(&mut self, table: Table, key: Vec<u8>, value: Vec<u8>) {
        self.ops.push((table, key, Some(value)));
    }

    fn delete(&mut self, table: Table, key: Vec<u8>) {
        self.ops.push((table, key, None));
    }

    /// Each row put, with its value, or deleted (`None`), in order.
    #[cfg(test)]
    pub(crate) fn ops(&self) -> impl Iterator<Item = (&[u8], Option<&[u8]>)> {
        self.ops.iter().map(|(_, k, v)| (k.as_slice(), v.as_deref()))
    }
}

/// What the store holds, as the last `take_dirty` or `load` left it: what `take_dirty` compares
/// the engine's state with. A list by its `Arc`s, held here so no address is reused while it
/// compares; `None` for a row the store holds that the engine dropped at load. A rotation row by
/// its stored bytes, a bit per member, not a clone of its `Vec<bool>` (BL-382, D46).
#[derive(Default)]
pub(crate) struct Stored {
    lists: HashMap<(u32, BlockHash), Option<ListMark>>,
    bases: HashMap<(u32, BlockHash), Arc<SmlList>>,
    sets: HashSet<[u8; 32]>,
    records: HashMap<[u8; 32], Vec<u8>>,
    rows: HashMap<RowKey, Vec<u8>>,
    marks: BTreeMap<&'static [u8], Vec<u8>>,
}

struct ListMark {
    authenticated: bool,
    quorums: Arc<QuorumSet>,
    sml: Option<Arc<SmlList>>,
    /// The set row and the base its row names (D48).
    set: [u8; 32],
    base: Option<(u32, BlockHash)>,
}

impl ListMark {
    fn of(l: &HeldList, set: [u8; 32], base: Option<(u32, BlockHash)>) -> ListMark {
        ListMark {
            authenticated: l.authenticated,
            quorums: l.quorums.clone(),
            sml: l.full.as_ref().map(|f| f.sml.clone()),
            set,
            base,
        }
    }

    fn same(&self, l: &HeldList) -> bool {
        self.authenticated == l.authenticated
            && Arc::ptr_eq(&self.quorums, &l.quorums)
            && match (&self.sml, &l.full) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, &b.sml),
                (None, None) => true,
                _ => false,
            }
    }
}

impl MasternodeListEngine {
    /// The rows changed since the last `take_dirty` or [`load`](Self::load), with the locator
    /// (SB0 §5.3, §9.5; D38). The first call on an engine that was not loaded writes everything.
    ///
    /// Taking the writes counts them as stored: the next call writes only what changes after
    /// them. Call it only for a batch that will be committed, never while §9.5's anchor state is
    /// `Candidate`; a host that takes writes and does not commit them (an aborted batch) must
    /// [`load`](Self::load) the engine again before taking more (Opus s3 r1 nit 6): a later batch
    /// may name a set or base row only the aborted one put, and the store then loads as
    /// [`MnLoadError::Corrupt`] (D48).
    pub fn take_dirty(&mut self) -> MnWrites {
        let mut stored = self.stored.take().unwrap_or_default();
        let mut w = MnWrites::default();

        let mut live = HashSet::new();
        let mut changed = Vec::new();
        for l in self.lists.values() {
            let key = (l.height, l.hash);
            live.insert(key);
            if !stored.lists.get(&key).and_then(Option::as_ref).is_some_and(|m| m.same(l)) {
                changed.push(l);
            }
        }
        // In height order, so a batch's bases do not depend on the map's order (D48). A store
        // written block by block may pick other bases than one written at once from the same
        // lists; both load to the same engine.
        changed.sort_unstable_by_key(|l| (l.height, l.hash));
        for l in changed {
            let set = encode_set(&l.quorums);
            let id = sha256d(&set);
            if stored.sets.insert(id) {
                w.put(Table::Lists, set_key(&id), set);
            }
            let base = l.full.as_ref().map(|f| choose_base(&mut stored.bases, &mut w, l, &f.sml));
            let row = encode_list(l, &id, base.map(|key| (key, &*stored.bases[&key])));
            w.put(Table::Lists, list_key(l.height, &l.hash), row);
            stored.lists.insert((l.height, l.hash), Some(ListMark::of(l, id, base)));
        }
        stored.lists.retain(|(height, hash), _| {
            let keep = live.contains(&(*height, *hash));
            if !keep {
                w.delete(Table::Lists, list_key(*height, hash));
            }
            keep
        });
        // A base or set goes in the batch that drops its last list (D48).
        let (named_bases, named_sets): (HashSet<_>, HashSet<_>) =
            stored.lists.values().flatten().map(|m| (m.base, m.set)).unzip();
        stored.bases.retain(|(height, hash), _| {
            let keep = named_bases.contains(&Some((*height, *hash)));
            if !keep {
                w.delete(Table::Lists, base_key(*height, hash));
            }
            keep
        });
        stored.sets.retain(|id| {
            let keep = named_sets.contains(id);
            if !keep {
                w.delete(Table::Lists, set_key(id));
            }
            keep
        });

        for (hash, record) in &self.records {
            let meta = encode_record_meta(record);
            if stored.records.get(hash) == Some(&meta) {
                continue;
            }
            let mut value = meta.clone();
            record.commitment.encode(&mut value);
            w.put(Table::Quorums, quorum_key(hash), value);
            stored.records.insert(*hash, meta);
        }
        stored.records.retain(|hash, _| {
            let keep = self.records.contains_key(hash);
            if !keep {
                w.delete(Table::Quorums, quorum_key(hash));
            }
            keep
        });

        for (key, row) in &self.rows {
            let value = encode_held_row(row);
            if stored.rows.get(key) == Some(&value) {
                continue;
            }
            w.put(Table::Snapshots, row_key(key), value.clone());
            stored.rows.insert(*key, value);
        }
        stored.rows.retain(|key, _| {
            let keep = self.rows.contains_key(key);
            if !keep {
                w.delete(Table::Snapshots, row_key(key));
            }
            keep
        });

        let marks = self.marks();
        for (name, value) in &marks {
            if stored.marks.get(name) != Some(value) {
                w.put(Table::Snapshots, mark_key(name), value.clone());
            }
        }
        for name in stored.marks.keys().filter(|n| !marks.contains_key(*n)) {
            w.delete(Table::Snapshots, mark_key(name));
        }
        stored.marks = marks;

        w.locator = self.retained.map(|t| (t.hash, t.height));
        self.stored = Some(stored);
        w
    }

    /// Every row the engine holds, as a first `take_dirty` writes them (the tags keep the tables'
    /// keys apart), leaving what the store holds as it was.
    #[cfg(test)]
    pub(crate) fn persisted(&mut self) -> BTreeMap<Vec<u8>, Vec<u8>> {
        let stored = self.stored.take();
        let w = self.take_dirty();
        self.stored = stored;
        w.ops.into_iter().map(|(_, k, v)| (k, v.expect("a first write only puts"))).collect()
    }

    /// The engine's marks as stored.
    fn marks(&self) -> BTreeMap<&'static [u8], Vec<u8>> {
        let mut marks = BTreeMap::new();
        if let Some((height, hash)) = self.best_cl {
            marks.insert(BEST_CL, block_mark(height, &hash));
        }
        if let Some((height, hash)) = self.cl_base {
            marks.insert(CL_BASE, block_mark(height, &hash));
        }
        if let Some(cl) = &self.pending_cl {
            let mut v = Vec::new();
            cl.encode(&mut v);
            marks.insert(PENDING_CL, v);
        }
        // The host's base is held whole while it is one (D29); a dropped one is no mark.
        if let Some(l) = self.last_diff.and_then(|id| self.lists.get(&id)) {
            marks.insert(LAST_DIFF, block_mark(l.height, &l.hash));
        }
        if let Some(t) = self.retained {
            marks.insert(RETAINED, block_mark(t.height, &t.hash));
        }
        marks
    }

    /// The engine as `take_dirty`'s rows in `db` left it (SB0 §9.5, D38). The rows' locator, the
    /// tip their last retention ran on, must be on `chain`, else [`MnLoadError::UnknownLocator`]
    /// and the host [`reset`](Self::reset)s them. Each other block a row names is resolved on
    /// `chain`: a list on a block it does not know (a branch it forgot) is dropped, a mark on one
    /// is cleared and a state its decider decided is run again, and the next `take_dirty` writes
    /// the change. Nothing is requested and no step runs: the host then calls
    /// [`tip_changed`](Self::tip_changed), which runs step 5 and retention and lists the requests.
    pub fn load(
        db: &impl KvRead,
        cfs: MnCfs,
        mode: Mode,
        chain: &dyn MnChain,
    ) -> Result<Self, MnLoadError> {
        let mut engine = MasternodeListEngine::new(mode);
        let mut stored = Stored::default();

        for item in db.iter_prefix(cfs.lists, &[BASE]) {
            let (key, value) = item?;
            let key = read_list_key(&key).map_err(corrupt(Table::Lists))?;
            let sml = decode_base(&value).map_err(corrupt(Table::Lists))?;
            stored.bases.insert(key, Arc::new(sml));
        }
        // Each set is decoded once, so the lists holding it share one `Arc`, as D28 holds them.
        let mut sets = HashMap::new();
        for item in db.iter_prefix(cfs.lists, &[SET]) {
            let (key, value) = item?;
            let id = read_hash_key(&key).map_err(corrupt(Table::Lists))?;
            if sha256d(&value) != id {
                return Err(corrupt(Table::Lists)("the key is not the set's hash"));
            }
            let set = decode_set(&value).map_err(corrupt(Table::Lists))?;
            stored.sets.insert(id);
            sets.insert(id, Arc::new(set));
        }
        for item in db.iter_prefix(cfs.lists, &[LIST]) {
            let (key, value) = item?;
            let (height, hash) = read_list_key(&key).map_err(corrupt(Table::Lists))?;
            let (list, mark) = decode_list(height, hash, &value, &sets, &stored.bases)
                .map_err(corrupt(Table::Lists))?;
            // A base's own list shares its entries, as when the base was written, so the base
            // costs no second copy while the engine holds the list (validator s5 n4).
            if let (Some(full), Some(base)) = (&list.full, stored.bases.get_mut(&(height, hash)))
                && full.sml.entries() == base.entries()
            {
                *base = full.sml.clone();
            }
            match resolve(chain, Table::Lists, height, &hash)? {
                Some(id) => {
                    stored.lists.insert((height, hash), Some(mark));
                    engine.insert_list(id, list);
                }
                None => {
                    stored.lists.insert((height, hash), None);
                }
            }
        }

        for item in db.iter_prefix(cfs.quorums, &[QUORUM]) {
            let (key, value) = item?;
            let hash = read_hash_key(&key).map_err(corrupt(Table::Quorums))?;
            let (mut record, meta) = decode_record(&value).map_err(corrupt(Table::Quorums))?;
            if commitment_hash(&record.commitment) != hash {
                return Err(corrupt(Table::Quorums)("the key is not the commitment's hash"));
            }
            if let Some(d) = record.decider {
                record.decider = resolve_mark(chain, d)?;
                // Decided on a block no longer known: step 5 runs again.
                if record.decider.is_none() {
                    record.state = None;
                }
            }
            if let Some(m) = record.mined {
                record.mined = resolve_mark(chain, m)?;
            }
            stored.records.insert(hash, meta);
            engine.records.insert(hash, record);
        }
        // Every set's commitments are rows, each under its own type and quorum hash: else a
        // lookup could read another quorum's row, or a step none at all.
        for set in sets.values() {
            for ((llmq_type, quorum_hash), leaf) in set.iter() {
                let held = engine.records.get(leaf).is_some_and(|r| {
                    r.commitment.llmq_type.to_base() == *llmq_type
                        && r.commitment.quorum_hash.as_bytes() == quorum_hash
                });
                if !held {
                    return Err(corrupt(Table::Lists)("a set names a commitment no row holds"));
                }
            }
        }

        for item in db.iter_prefix(cfs.snapshots, &[ROW]) {
            let (key, value) = item?;
            let row_key = read_row_key(&key).map_err(corrupt(Table::Snapshots))?;
            let row = decode_held_row(&value).map_err(corrupt(Table::Snapshots))?;
            stored.rows.insert(row_key, value.to_vec());
            engine.rows.insert(row_key, row);
        }

        for item in db.iter_prefix(cfs.snapshots, &[MARK]) {
            let (key, value) = item?;
            let name = match &key[1..] {
                BEST_CL => BEST_CL,
                CL_BASE => CL_BASE,
                PENDING_CL => PENDING_CL,
                LAST_DIFF => LAST_DIFF,
                RETAINED => RETAINED,
                _ => return Err(corrupt(Table::Snapshots)("an unknown mark")),
            };
            stored.marks.insert(name, value.clone());
            if name == PENDING_CL {
                engine.pending_cl =
                    Some(decode_exact::<ClSig>(&value).map_err(corrupt(Table::Snapshots))?);
                continue;
            }
            let (height, hash) = read_block_mark(&value).map_err(corrupt(Table::Snapshots))?;
            match name {
                BEST_CL => {
                    // A lock may arrive before its header (up to 8 above the tip, D18): an
                    // unknown block is one the chain has yet to accept, kept as the live engine
                    // keeps it; only a known one at another height is corrupt.
                    resolve(chain, Table::Snapshots, height, &hash)?;
                    engine.best_cl = Some((height, hash));
                }
                CL_BASE => {
                    // Known when retention marked it; a block the chain forgot is no base.
                    if resolve(chain, Table::Snapshots, height, &hash)?.is_some() {
                        engine.cl_base = Some((height, hash));
                    }
                }
                LAST_DIFF => {
                    engine.last_diff = resolve(chain, Table::Snapshots, height, &hash)?
                        .filter(|id| engine.lists.contains_key(id));
                }
                _ => {
                    // The rows reach a block the chain does not know: marks of a branch it lost
                    // may hold on, so the host resets them (SB0 §9.5).
                    let Some(id) = resolve(chain, Table::Snapshots, height, &hash)? else {
                        return Err(MnLoadError::UnknownLocator {
                            height,
                            hash,
                        });
                    };
                    engine.retained = Some(BlockRef {
                        id,
                        hash,
                        height,
                    });
                }
            }
        }

        // A record whose row names the sets holds to the newest authenticated list on the
        // retained tip's chain holding it, as retention left it (D36, D48).
        if let Some(tip) = engine.retained {
            let newest = newest_holders(chain, tip.id, &engine.lists).map_err(MnViewError::from)?;
            for (hash, record) in engine.records.iter_mut() {
                if record.held_by_sets {
                    record.held_to = newest.get(hash).copied();
                }
            }
        }

        // Every retention leaves its tip: rows without one were written after a failed call.
        if engine.retained.is_none() && !(stored.lists.is_empty() && stored.records.is_empty()) {
            return Err(corrupt(Table::Snapshots)("rows without their locator"));
        }
        engine.stored = Some(stored);
        Ok(engine)
    }
}

impl MasternodeListEngine {
    /// The write set deleting every row of the engine's tables in `db`: SB0 §9.5's reset of a
    /// subscriber's data, which the host commits with its locator cleared after
    /// [`MnLoadError::UnknownLocator`] or [`MnLoadError::Corrupt`], then starts from
    /// [`new`](Self::new). A new engine's `take_dirty` deletes nothing it did not write, so in
    /// one batch these deletes go before its puts.
    pub fn reset(db: &impl KvRead, cfs: MnCfs) -> Result<MnWrites, MnLoadError> {
        let mut w = MnWrites::default();
        for table in [Table::Lists, Table::Quorums, Table::Snapshots] {
            for item in db.iter_prefix(table.cf(cfs), &[]) {
                w.delete(table, item?.0);
            }
        }
        Ok(w)
    }
}

/// The block `hash` at `height` on `chain`, if it knows it; at another height `table`'s row is
/// corrupt.
fn resolve(
    chain: &dyn MnChain,
    table: Table,
    height: u32,
    hash: &BlockHash,
) -> Result<Option<BlockId>, MnLoadError> {
    let Some(id) = chain.lookup(hash).map_err(MnViewError::from)? else {
        return Ok(None);
    };
    let at = chain.ancestry().header(id).map_err(MnViewError::from)?.height;
    if at != height {
        return Err(corrupt(table)("a block's height differs from the chain's"));
    }
    Ok(Some(id))
}

/// `mark` with its id resolved on `chain`, if it knows its block (a quorum row's mark).
fn resolve_mark(chain: &dyn MnChain, mark: SetMark) -> Result<Option<SetMark>, MnLoadError> {
    Ok(resolve(chain, Table::Quorums, mark.height, &mark.hash)?.map(|id| SetMark {
        id,
        ..mark
    }))
}

fn corrupt<E: std::fmt::Display>(table: Table) -> impl Fn(E) -> MnLoadError {
    move |e| MnLoadError::Corrupt {
        table: table.name(),
        reason: e.to_string(),
    }
}

fn list_key(height: u32, hash: &BlockHash) -> Vec<u8> {
    let mut k = Vec::with_capacity(37);
    k.push(LIST);
    k.extend_from_slice(&height.to_be_bytes());
    k.extend_from_slice(hash.as_bytes());
    k
}

fn base_key(height: u32, hash: &BlockHash) -> Vec<u8> {
    let mut k = list_key(height, hash);
    k[0] = BASE;
    k
}

fn set_key(id: &[u8; 32]) -> Vec<u8> {
    [&[SET][..], id].concat()
}

fn read_list_key(k: &[u8]) -> Result<(u32, BlockHash), CodecError> {
    let mut r = exact(k, 37)?;
    r.skip(1)?;
    Ok((u32::from_be_bytes(r.array()?), BlockHash::from(r.array::<32>()?)))
}

fn quorum_key(hash: &[u8; 32]) -> Vec<u8> {
    let mut k = Vec::with_capacity(33);
    k.push(QUORUM);
    k.extend_from_slice(hash);
    k
}

/// The hash of a set or quorum key (a tag, then the hash).
fn read_hash_key(k: &[u8]) -> Result<[u8; 32], CodecError> {
    let mut r = exact(k, 33)?;
    r.skip(1)?;
    r.array()
}

fn row_key((llmq_type, cycle): &RowKey) -> Vec<u8> {
    let mut k = Vec::with_capacity(34);
    k.push(ROW);
    k.push(*llmq_type);
    k.extend_from_slice(cycle);
    k
}

fn read_row_key(k: &[u8]) -> Result<RowKey, CodecError> {
    let mut r = exact(k, 34)?;
    r.skip(1)?;
    Ok((r.byte()?, r.array()?))
}

fn mark_key(name: &[u8]) -> Vec<u8> {
    let mut k = vec![MARK];
    k.extend_from_slice(name);
    k
}

fn block_mark(height: u32, hash: &BlockHash) -> Vec<u8> {
    let mut v = height.to_le_bytes().to_vec();
    v.extend_from_slice(hash.as_bytes());
    v
}

fn read_block_mark(v: &[u8]) -> Result<(u32, BlockHash), CodecError> {
    exact(v, 36)?.block()
}

/// A list row (D48): its flags, its set row's hash, and a whole list's coinbase and entries as a
/// delta against `base`: the base's key, the proRegTx hashes it holds that the list does not,
/// and the list's entries the base lacks or holds otherwise.
fn encode_list(
    l: &HeldList,
    set: &[u8; 32],
    base: Option<((u32, BlockHash), &SmlList)>,
) -> Vec<u8> {
    let mut v = vec![u8::from(l.authenticated) | (u8::from(l.full.is_some()) << 1)];
    v.extend_from_slice(set);
    if let (Some(full), Some(((height, hash), base))) = (&l.full, base) {
        match &full.coinbase {
            CoinbaseFacts::First(tx) => {
                v.push(1);
                let mut raw = Vec::new();
                tx.raw().encode(&mut raw);
                write_bytes(&mut v, &raw);
            }
            // A held list's coinbase is its diff's, or the empty list's (none).
            _ => v.push(0),
        }
        v.extend_from_slice(&block_mark(height, &hash));
        let (removed, changed) = delta(base, &full.sml);
        write_varint(&mut v, removed.len() as u64);
        for e in removed {
            v.extend_from_slice(e.pro_reg_tx_hash.as_bytes());
        }
        write_varint(&mut v, changed.len() as u64);
        for e in changed {
            write_bytes(&mut v, &entry_wire(e));
        }
    }
    v
}

/// A set row: a VARINT count of (type ‖ quorum hash ‖ leaf), in key order.
fn encode_set(set: &QuorumSet) -> Vec<u8> {
    let mut v = Vec::with_capacity(9 + 65 * set.len());
    write_varint(&mut v, set.len() as u64);
    for ((llmq_type, quorum_hash), leaf) in set {
        v.push(*llmq_type);
        v.extend_from_slice(quorum_hash);
        v.extend_from_slice(leaf);
    }
    v
}

fn decode_set(v: &[u8]) -> Result<QuorumSet, CodecError> {
    let mut r = Reader(v);
    let mut set = QuorumSet::new();
    for _ in 0..r.count()? {
        let key = (r.byte()?, r.array()?);
        // In key order, so a set decodes from one value only (D38).
        if set.last_key_value().is_some_and(|(last, _)| *last >= key) {
            return Err(CodecError::Invalid("a set out of key order"));
        }
        set.insert(key, r.array()?);
    }
    r.end()?;
    Ok(set)
}

/// The entries of `base` that `sml` lacks, and those of `sml` that `base` lacks or holds
/// otherwise; both lists are sorted by proRegTx hash.
fn delta<'a>(base: &'a SmlList, sml: &'a SmlList) -> (Vec<&'a SmlEntry>, Vec<&'a SmlEntry>) {
    let (base, sml) = (base.entries(), sml.entries());
    let (mut removed, mut changed) = (Vec::new(), Vec::new());
    let (mut i, mut j) = (0, 0);
    while i < base.len() || j < sml.len() {
        let order = match (base.get(i), sml.get(j)) {
            (Some(x), Some(y)) => x.pro_reg_tx_hash.cmp(&y.pro_reg_tx_hash),
            (Some(_), None) => Ordering::Less,
            (None, _) => Ordering::Greater,
        };
        match order {
            Ordering::Less => {
                removed.push(&base[i]);
                i += 1;
            }
            Ordering::Greater => {
                changed.push(&sml[j]);
                j += 1;
            }
            Ordering::Equal => {
                if base[i] != sml[j] {
                    changed.push(&sml[j]);
                }
                i += 1;
                j += 1;
            }
        }
    }
    (removed, changed)
}

/// The base a whole list's row is written against (D48): the newest stored base at most
/// [`BASE_SPAN`] below it whose delta is at most a quarter of the list, else the list itself,
/// written as a new base.
fn choose_base(
    bases: &mut HashMap<(u32, BlockHash), Arc<SmlList>>,
    w: &mut MnWrites,
    l: &HeldList,
    sml: &Arc<SmlList>,
) -> (u32, BlockHash) {
    let newest = bases
        .iter()
        .filter(|((height, _), _)| *height <= l.height && l.height - height <= BASE_SPAN)
        .max_by_key(|(key, _)| **key);
    if let Some((key, base)) = newest {
        let (removed, changed) = delta(base, sml);
        if (removed.len() + changed.len()) * 4 <= sml.len() {
            return *key;
        }
    }
    w.put(Table::Lists, base_key(l.height, &l.hash), encode_base(sml));
    bases.insert((l.height, l.hash), sml.clone());
    (l.height, l.hash)
}

/// An entry as `mnlistdiff` carries it: its version, then the hashed fields.
fn entry_wire(e: &SmlEntry) -> Vec<u8> {
    let mut wire = e.version.to_le_bytes().to_vec();
    e.write_hashed(&mut wire);
    wire
}

fn encode_base(sml: &SmlList) -> Vec<u8> {
    let mut v = Vec::new();
    write_varint(&mut v, sml.len() as u64);
    for e in sml.entries() {
        write_bytes(&mut v, &entry_wire(e));
    }
    v
}

fn read_entry(r: &mut Reader<'_>) -> Result<SmlEntry, CodecError> {
    let wire = decode_exact::<SimplifiedMnListEntry>(r.bytes()?)
        .map_err(|_| CodecError::Invalid("list entry"))?;
    Ok(sml_entry(&wire))
}

fn decode_base(v: &[u8]) -> Result<SmlList, CodecError> {
    let mut r = Reader(v);
    let n = r.count()?;
    let mut entries: Vec<SmlEntry> = Vec::with_capacity(n.min(1 << 16));
    for _ in 0..n {
        let e = read_entry(&mut r)?;
        if entries.last().is_some_and(|last| last.pro_reg_tx_hash >= e.pro_reg_tx_hash) {
            return Err(CodecError::Invalid("a base out of proRegTx order"));
        }
        entries.push(e);
    }
    r.end()?;
    Ok(SmlList::from_entries(entries))
}

fn decode_list(
    height: u32,
    hash: BlockHash,
    v: &[u8],
    sets: &HashMap<[u8; 32], Arc<QuorumSet>>,
    bases: &HashMap<(u32, BlockHash), Arc<SmlList>>,
) -> Result<(HeldList, ListMark), CodecError> {
    let mut r = Reader(v);
    let flags = r.byte()?;
    if flags > 3 {
        return Err(CodecError::Invalid("list flags"));
    }
    let authenticated = flags & 1 != 0;
    let set = r.array()?;
    let quorums = sets.get(&set).ok_or(CodecError::Invalid("a list's set is not stored"))?;
    let full = if flags & 2 != 0 {
        let coinbase = match r.byte()? {
            0 => CoinbaseFacts::NoTransactions,
            1 => {
                let raw = decode_exact::<Transaction>(r.bytes()?)
                    .map_err(|_| CodecError::Invalid("coinbase"))?;
                CoinbaseFacts::First(Arc::new(Tx::new(raw)))
            }
            _ => return Err(CodecError::Invalid("coinbase tag")),
        };
        let key = r.block()?;
        let base = bases.get(&key).ok_or(CodecError::Invalid("a delta's base is not stored"))?;
        let mut entries: BTreeMap<_, SmlEntry> =
            base.entries().iter().map(|e| (e.pro_reg_tx_hash, e.clone())).collect();
        // Each part in proRegTx order, and no entry both removed and changed, so a delta
        // decodes from one value only (D38).
        let mut removed: Vec<ProTxHash> = Vec::new();
        for _ in 0..r.count()? {
            let hash = ProTxHash::from(r.array::<32>()?);
            if removed.last().is_some_and(|last| *last >= hash) {
                return Err(CodecError::Invalid("a delta's removals out of order"));
            }
            if entries.remove(&hash).is_none() {
                return Err(CodecError::Invalid("a delta removes an entry its base lacks"));
            }
            removed.push(hash);
        }
        let mut last = None;
        for _ in 0..r.count()? {
            let e = read_entry(&mut r)?;
            if last.is_some_and(|last| last >= e.pro_reg_tx_hash) {
                return Err(CodecError::Invalid("a delta's changes out of order"));
            }
            if removed.binary_search(&e.pro_reg_tx_hash).is_ok() {
                return Err(CodecError::Invalid("a delta removes an entry it changes"));
            }
            if entries.get(&e.pro_reg_tx_hash) == Some(&e) {
                return Err(CodecError::Invalid("a delta repeats its base's entry"));
            }
            last = Some(e.pro_reg_tx_hash);
            entries.insert(e.pro_reg_tx_hash, e);
        }
        let mut sml = SmlList::from_entries(entries.into_values());
        check_roots(height, &coinbase, &mut sml, quorums, authenticated)?;
        Some((
            Full {
                sml: Arc::new(sml),
                coinbase,
            },
            key,
        ))
    } else {
        None
    };
    r.end()?;
    let (full, base) = full.unzip();
    let list = HeldList {
        height,
        hash,
        full,
        quorums: quorums.clone(),
        authenticated,
    };
    let mark = ListMark::of(&list, set, base);
    Ok((list, mark))
}

/// A whole list's coinbase is a CbTx for its own height (step 2's height check; the coinbase's
/// proof is not stored), and its entries, and its set when authenticated, hash to the roots it
/// carries (steps 3–4 over the stored row); a list with no coinbase holds no entry.
fn check_roots(
    height: u32,
    coinbase: &CoinbaseFacts,
    sml: &mut SmlList,
    quorums: &QuorumSet,
    authenticated: bool,
) -> Result<(), CodecError> {
    let tx = match coinbase {
        CoinbaseFacts::First(tx) => tx,
        _ if sml.is_empty() => return Ok(()),
        _ => return Err(CodecError::Invalid("entries without a coinbase")),
    };
    let cbtx = cbtx_payload(tx).map_err(|_| CodecError::Invalid("its coinbase is not a CbTx"))?;
    // Else a list stored under another block's key loads (Opus s3 r1 nit 1).
    if i64::from(cbtx.height) != i64::from(height) {
        return Err(CodecError::Invalid("its CbTx is another height's"));
    }
    let root = sml.merkle_root().map_err(|_| CodecError::Invalid("its entries do not hash"))?;
    check_mn_list_root(&cbtx, &root)
        .map_err(|_| CodecError::Invalid("its entries are not its CbTx's list"))?;
    if authenticated {
        check_quorums_root(&cbtx, || {
            merkle_root_quorums_from_hashes(quorums.values().copied().collect())
        })
        .map_err(|_| CodecError::Invalid("its set is not its CbTx's"))?;
    }
    Ok(())
}

const STATE_NONE: u8 = 0;
const STATE_VERIFIED: u8 = 1;
const STATE_UNVERIFIED: u8 = 2;
const STATE_UNAUTHENTICATED: u8 = 3;

/// A quorum row's value before its commitment: what `take_dirty` compares.
fn encode_record_meta(r: &Record) -> Vec<u8> {
    let mut v = r.first_seen.to_le_bytes().to_vec();
    v.push(u8::from(r.authenticated));
    // A request's state is not decided, and `Skipped` follows the configuration, which may
    // change across a restart: step 5 runs both again after a load.
    v.push(match r.state {
        Some(QuorumState::Verified(_)) => STATE_VERIFIED,
        Some(QuorumState::Unverified) => STATE_UNVERIFIED,
        Some(QuorumState::Unauthenticated) => STATE_UNAUTHENTICATED,
        Some(QuorumState::Pending(_) | QuorumState::Unavailable(_) | QuorumState::Skipped)
        | None => STATE_NONE,
    });
    write_mark(&mut v, r.decider);
    write_height(&mut v, r.replaced);
    write_mark(&mut v, r.mined);
    // Not stored while it is the sets': it follows the tip there, and `load` reads it from the
    // lists it stores (BL-380, D48).
    if r.held_by_sets {
        v.push(HELD_BY_SETS);
    } else {
        write_height(&mut v, r.held_to);
    }
    v
}

/// A quorum row: the record, with every mark's id unresolved, and its meta bytes.
fn decode_record(v: &[u8]) -> Result<(Record, Vec<u8>), CodecError> {
    let mut r = Reader(v);
    let first_seen = r.u32()?;
    let authenticated = match r.byte()? {
        0 => false,
        1 => true,
        _ => return Err(CodecError::Invalid("authenticated flag")),
    };
    let state = r.byte()?;
    let decider = r.mark()?;
    let replaced = r.height()?;
    let mined = r.mark()?;
    let held_by_sets = r.0.first() == Some(&HELD_BY_SETS);
    let held_to = match (held_by_sets, replaced) {
        (true, None) => {
            r.skip(1)?;
            None
        }
        (true, Some(_)) => {
            return Err(CodecError::Invalid("the sets' held_to of a replaced record"));
        }
        (false, _) => r.height()?,
    };
    let meta = v[..v.len() - r.0.len()].to_vec();
    let commitment =
        decode_exact::<Commitment>(r.0).map_err(|_| CodecError::Invalid("commitment"))?;
    let state = match state {
        STATE_NONE => None,
        STATE_VERIFIED => Some(QuorumState::Verified(VerifiedQuorum::reloaded(&commitment))),
        STATE_UNVERIFIED => Some(QuorumState::Unverified),
        STATE_UNAUTHENTICATED => Some(QuorumState::Unauthenticated),
        _ => return Err(CodecError::Invalid("state")),
    };
    let record = Record {
        commitment: Arc::new(commitment),
        first_seen,
        authenticated,
        state,
        decider,
        replaced,
        mined,
        held_to,
        held_by_sets,
    };
    Ok((record, meta))
}

fn write_mark(v: &mut Vec<u8>, mark: Option<SetMark>) {
    match mark {
        None => v.push(0),
        Some(m) => {
            v.push(1);
            v.extend_from_slice(&block_mark(m.height, &m.hash));
        }
    }
}

fn write_height(v: &mut Vec<u8>, height: Option<u32>) {
    match height {
        None => v.push(0),
        Some(h) => {
            v.push(1);
            v.extend_from_slice(&h.to_le_bytes());
        }
    }
}

fn encode_held_row(row: &HeldRow) -> Vec<u8> {
    let mut v = Vec::new();
    match row {
        HeldRow::Core(s) => {
            v.push(0);
            encode_snapshot(&mut v, s);
        }
        HeldRow::Candidates(rows) => {
            v.push(1);
            write_varint(&mut v, rows.len() as u64);
            for s in rows {
                encode_snapshot(&mut v, s);
            }
        }
    }
    v
}

fn decode_held_row(v: &[u8]) -> Result<HeldRow, CodecError> {
    let mut r = Reader(v);
    let row = match r.byte()? {
        0 => HeldRow::Core(r.snapshot()?),
        1 => {
            let n = r.count()?;
            if n == 0 || n > ROW_CANDIDATES {
                return Err(CodecError::Invalid("a candidate count"));
            }
            HeldRow::Candidates((0..n).map(|_| r.snapshot()).collect::<Result<_, _>>()?)
        }
        _ => return Err(CodecError::Invalid("row tag")),
    };
    r.end()?;
    Ok(row)
}

fn encode_snapshot(v: &mut Vec<u8>, s: &QuorumSnapshot) {
    write_varint(v, s.active_quorum_members.len() as u64);
    let mut bits = vec![0u8; s.active_quorum_members.len().div_ceil(8)];
    for (i, b) in s.active_quorum_members.iter().enumerate() {
        bits[i / 8] |= u8::from(*b) << (i % 8);
    }
    v.extend_from_slice(&bits);
    v.extend_from_slice(&s.skip_list_mode.to_le_bytes());
    write_varint(v, s.skip_list.len() as u64);
    for x in &s.skip_list {
        v.extend_from_slice(&x.to_le_bytes());
    }
}

fn write_bytes(v: &mut Vec<u8>, bytes: &[u8]) {
    write_varint(v, bytes.len() as u64);
    v.extend_from_slice(bytes);
}

/// `T` decoded from exactly `bytes`.
fn decode_exact<T: BaseCodec>(bytes: &[u8]) -> Result<T, CodecError> {
    let mut cursor = bytes;
    let value = T::decode(&mut cursor).map_err(|_| CodecError::Invalid("base-sdk decode"))?;
    if !cursor.is_empty() {
        return Err(CodecError::TrailingBytes(cursor.len()));
    }
    Ok(value)
}

fn exact(bytes: &[u8], len: usize) -> Result<Reader<'_>, CodecError> {
    if bytes.len() != len {
        return Err(CodecError::Length {
            expected: len,
            got: bytes.len(),
        });
    }
    Ok(Reader(bytes))
}

/// A cursor over a stored value.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], CodecError> {
        if self.0.len() < n {
            return Err(CodecError::UnexpectedEnd);
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn skip(&mut self, n: usize) -> Result<(), CodecError> {
        self.take(n).map(|_| ())
    }

    fn byte(&mut self) -> Result<u8, CodecError> {
        Ok(self.take(1)?[0])
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], CodecError> {
        Ok(self.take(N)?.try_into().expect("N bytes"))
    }

    fn u32(&mut self) -> Result<u32, CodecError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn i32(&mut self) -> Result<i32, CodecError> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    /// A block's height and hash.
    fn block(&mut self) -> Result<(u32, BlockHash), CodecError> {
        Ok((self.u32()?, BlockHash::from(self.array::<32>()?)))
    }

    /// A VARINT count of items, each at least one byte: one beyond the input is corrupt.
    fn count(&mut self) -> Result<usize, CodecError> {
        let n = read_varint_u64(&mut self.0)?;
        usize::try_from(n)
            .ok()
            .filter(|n| *n <= self.0.len())
            .ok_or(CodecError::Invalid("a count beyond the value"))
    }

    fn bytes(&mut self) -> Result<&'a [u8], CodecError> {
        let n = self.count()?;
        self.take(n)
    }

    fn mark(&mut self) -> Result<Option<SetMark>, CodecError> {
        match self.byte()? {
            0 => Ok(None),
            1 => {
                let (height, hash) = self.block()?;
                // The id is resolved on the chain at load.
                Ok(Some(SetMark {
                    id: BlockId(0),
                    height,
                    hash,
                }))
            }
            _ => Err(CodecError::Invalid("mark tag")),
        }
    }

    fn height(&mut self) -> Result<Option<u32>, CodecError> {
        match self.byte()? {
            0 => Ok(None),
            1 => Ok(Some(self.u32()?)),
            _ => Err(CodecError::Invalid("height tag")),
        }
    }

    fn snapshot(&mut self) -> Result<QuorumSnapshot, CodecError> {
        let n = read_varint_u64(&mut self.0)?;
        let n = usize::try_from(n)
            .ok()
            .filter(|n| n.div_ceil(8) <= self.0.len())
            .ok_or(CodecError::Invalid("a bit count beyond the value"))?;
        let bits = self.take(n.div_ceil(8))?;
        // The bits past the last member are 0, so a row decodes from one value only (D38).
        if n % 8 != 0 && bits[n / 8] >> (n % 8) != 0 {
            return Err(CodecError::Invalid("a bit past the last member"));
        }
        let active_quorum_members = (0..n).map(|i| bits[i / 8] >> (i % 8) & 1 == 1).collect();
        let skip_list_mode = self.i32()?;
        let k = self.count()?;
        let skip_list = (0..k).map(|_| self.i32()).collect::<Result<_, _>>()?;
        Ok(QuorumSnapshot {
            active_quorum_members,
            skip_list_mode,
            skip_list,
        })
    }

    fn end(&self) -> Result<(), CodecError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(CodecError::TrailingBytes(self.0.len()))
        }
    }
}
