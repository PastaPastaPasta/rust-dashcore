//! `mempool.dat`: `DumpMempool`/`LoadMempool` in `kernel/mempool_persist.cpp`.
//!
//! Layout (Core v24.0.0-rc.3, `MEMPOOL_DUMP_VERSION = 1`): `u64 version`, `u64 count`, `count`
//! times (transaction, `i64 time`, `i64 fee delta`), the `mapDeltas` (`std::map<uint256,
//! CAmount>`: compact-size count, then 32-byte txid and `i64` per entry) and the unbroadcast set
//! (compact-size count, then 32-byte txids). v24 has no XOR obfuscation key and no network
//! magic or checksum; a version-2 file (the obfuscated format of later Bitcoin Core releases) is
//! [`FormatError::UnsupportedVersion`].
//!
//! # What Core keeps of a damaged file
//!
//! `LoadMempool` applies the file **as it reads it**: each complete transaction record's fee
//! delta is applied and the transaction offered to the pool before the next record is decoded, the
//! whole `mapDeltas` is applied once it has been read completely, and then the unbroadcast set once
//! it has. A failure ends the load without undoing what was applied. [`load`] returns the same
//! stages: everything before the failure, and the failure ([`Outcome::Stopped`]). [`parse`] is the
//! strict form (all three stages complete, or the error) for validation and format sniffing; bytes
//! after the unbroadcast set are ignored, as Core ignores them ([`MempoolDump::trailing_bytes`]).
//!
//! Containers follow Core's types: `mapDeltas` is a `std::map` (the first value for a repeated
//! txid wins) and the unbroadcast list a `std::set` (repeats collapse). The transaction records
//! stay a list: Core applies every record's delta, also for a txid it has seen, and leaves
//! admission to the pool.

use std::collections::{BTreeMap, BTreeSet};

use super::envelope::check_size;
use super::error::{FormatError, FormatResult};
use super::reader::Reader;
use crate::Hash256;
use crate::tx::{Cursor, read_tx};

const WHAT: &str = "mempool.dat";

/// The only version Core v24 reads and writes.
pub const MEMPOOL_DUMP_VERSION: u64 = 1;

/// Core's default `-maxmempool` is 300 MB of in-memory use; its dump is below that.
pub const MAX_MEMPOOL_DAT_SIZE: usize = 1 << 30;

/// The smallest entry: a 10-byte transaction (version, empty input and output lists, lock time),
/// its time and its fee delta.
const MIN_ENTRY_SIZE: usize = 10 + 8 + 8;

/// A mempool entry as stored: the transaction's bytes, borrowed from the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MempoolTx<'a> {
    pub raw: &'a [u8],
    pub txid: Hash256,
    /// When the transaction entered the pool (`int64_t` seconds).
    pub time: i64,
    /// `nFeeDelta`: the `prioritisetransaction` delta of this entry.
    pub fee_delta: i64,
}

/// A parsed `mempool.dat`, or the part of one that Core would have applied.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MempoolDump<'a> {
    /// The complete transaction records, in file order, repeats included.
    pub txs: Vec<MempoolTx<'a>>,
    /// `mapDeltas`: deltas of transactions that are not in the pool. Empty unless the map was read
    /// completely; the first value for a repeated txid wins.
    pub deltas: BTreeMap<Hash256, i64>,
    /// The transactions that were not yet announced to any peer. Empty unless the set was read
    /// completely.
    pub unbroadcast: BTreeSet<Hash256>,
    /// Bytes after the last structure that was read completely; Core does not look at them.
    pub trailing_bytes: usize,
}

/// The part of the file a failure stopped in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Transactions,
    Deltas,
    Unbroadcast,
}

/// How a load ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Complete,
    /// The file is damaged in `phase`; the dump holds what Core applied before that.
    Stopped {
        phase: Phase,
        error: FormatError,
    },
}

/// What [`load`] returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded<'a> {
    pub dump: MempoolDump<'a>,
    pub outcome: Outcome,
}

/// The version word of a `mempool.dat`, for detection.
pub fn version(bytes: &[u8]) -> FormatResult<u64> {
    Reader::new(bytes, WHAT).u64_le()
}

/// A compact-size count, then that many 32-byte txids.
fn read_txids(r: &mut Reader<'_>) -> FormatResult<BTreeSet<Hash256>> {
    let n = r.compact_size(true)?;
    let mut out = BTreeSet::new();
    for _ in 0..n {
        out.insert(r.array::<32>()?);
    }
    Ok(out)
}

/// A compact-size count, then that many (txid, `i64`) pairs; `std::map::insert` keeps the first.
fn read_deltas(r: &mut Reader<'_>) -> FormatResult<BTreeMap<Hash256, i64>> {
    let n = r.compact_size(true)?;
    let mut out = BTreeMap::new();
    for _ in 0..n {
        let txid = r.array::<32>()?;
        out.entry(txid).or_insert(r.i64_le()?);
    }
    Ok(out)
}

fn read_record<'a>(r: &mut Reader<'a>) -> FormatResult<MempoolTx<'a>> {
    let mut cursor = Cursor::new(r.rest());
    let tx = read_tx(&mut cursor).map_err(|_| r.corrupt("a transaction does not decode"))?;
    let raw = r.take(cursor.position())?;
    Ok(MempoolTx {
        raw,
        txid: tx.txid,
        time: r.i64_le()?,
        fee_delta: r.i64_le()?,
    })
}

/// Reads a `mempool.dat` the way `LoadMempool` does and returns what it applies, also when the
/// file is damaged part-way. A failure before the first record (too large, truncated header,
/// another version) is an `Err`: Core applies nothing then.
///
/// The transactions are decoded (not validated) to find their extent and txid; their bytes stay
/// borrowed from `bytes`.
// Core: kernel/mempool_persist.cpp LoadMempool
pub fn load<'a>(bytes: &'a [u8]) -> FormatResult<Loaded<'a>> {
    check_size(bytes.len(), MAX_MEMPOOL_DAT_SIZE, WHAT)?;
    let mut r = Reader::new(bytes, WHAT);
    let version = r.u64_le()?;
    if version != MEMPOOL_DUMP_VERSION {
        return Err(FormatError::UnsupportedVersion {
            what: WHAT,
            version,
        });
    }
    let count = r.u64_le()?;
    let mut dump = MempoolDump {
        txs: Vec::with_capacity(r.capacity_for(count, MIN_ENTRY_SIZE)),
        ..MempoolDump::default()
    };
    // A structure that fails half-way counts from its start: only complete structures are read.
    let stopped = |dump: MempoolDump<'a>, from: usize, phase, error| Loaded {
        dump: MempoolDump {
            trailing_bytes: bytes.len() - from,
            ..dump
        },
        outcome: Outcome::Stopped {
            phase,
            error,
        },
    };
    for _ in 0..count {
        let from = r.pos();
        match read_record(&mut r) {
            Ok(tx) => dump.txs.push(tx),
            Err(error) => return Ok(stopped(dump, from, Phase::Transactions, error)),
        }
    }
    let from = r.pos();
    match read_deltas(&mut r) {
        Ok(deltas) => dump.deltas = deltas,
        Err(error) => return Ok(stopped(dump, from, Phase::Deltas, error)),
    }
    let from = r.pos();
    match read_txids(&mut r) {
        Ok(unbroadcast) => dump.unbroadcast = unbroadcast,
        Err(error) => return Ok(stopped(dump, from, Phase::Unbroadcast, error)),
    }
    dump.trailing_bytes = r.remaining();
    Ok(Loaded {
        dump,
        outcome: Outcome::Complete,
    })
}

/// Parses a `mempool.dat` strictly: all three stages complete (bytes after them are counted in
/// `trailing_bytes`), or the error that stopped [`load`].
pub fn parse(bytes: &[u8]) -> FormatResult<MempoolDump<'_>> {
    let loaded = load(bytes)?;
    match loaded.outcome {
        Outcome::Complete => Ok(loaded.dump),
        Outcome::Stopped {
            error,
            ..
        } => Err(error),
    }
}
