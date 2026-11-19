//! rdashd's `governance.dat`: the manager's [`StoreState`] in a `dashd_masternode::flatdb` file
//! (`magic ‖ message start ‖ body ‖ SHA256d`, as SP1's `mncache.dat`).
//!
//! The body is [`VERSION`], then Core's `-17` fields in Core's order (`governance.h:201-213`), read
//! back through the decoder Core's file uses (`dash_coreimport`'s `decode_store_body_from`) and
//! [`crate::import`]'s conversion. A rate record the conversion repairs is kept and reported in
//! [`GovernanceStore::repaired`].
//!
//! Load outcomes are flatdb's ([`LoadError::recreates`]). A body that does not decode, a version
//! string other than [`VERSION`], or a file over [`MAX_STORE_SIZE`] is `IncorrectFormat`: the store
//! is recreated, with flatdb's log lines. Loading and storing check the size on the file before
//! reading it, and make the same decision: [`store`] removes a file over the cap and writes.

use std::io;
use std::path::Path;

use dash_coreimport::netstate::FormatError;
use dash_coreimport::netstate::governance::{
    GovernanceDat, MAX_CACHE_SIZE, StoreVersion, decode_store_body_from,
};
use dash_dml::MnList;
use dash_governance::manager::StoreState;
use dash_governance::object;
use dash_governance::rate::RateCheckBuffer;
use dash_governance::tally::{ObjectState, OutpointKey};
use dash_primitives::{GovObject, GovVote};
use dash_types::CompactSize;
use dash_types::codec::BaseCodec;
use dashd_masternode::flatdb::{self, FlatObject, LoadError, ReadError};

/// The file in the network's data directory (Core's name).
pub const FILE: &str = "governance.dat";
/// rdashd's magic message, unlike Core's `magicGovernanceCache`, so neither reads the other's file.
pub const MAGIC: &str = "rdashd-governance";
/// The body's version string.
pub const VERSION: &str = "GovernanceStore-1";
/// The largest file loaded: `dash_coreimport`'s cap on Core's `governance.dat`.
pub const MAX_STORE_SIZE: usize = dash_coreimport::netstate::governance::MAX_GOVERNANCE_DAT_SIZE;

/// The store's object.
#[derive(Clone, Debug, Default)]
pub struct GovernanceStore {
    pub state: StoreState,
    /// The masternodes whose rate record [`crate::import::rate_record`] repaired on load.
    pub repaired: Vec<OutpointKey>,
}

impl FlatObject for GovernanceStore {
    const FILE: &'static str = FILE;
    const MAGIC: &'static str = MAGIC;

    // Core: governance/governance.h:201-213 GovernanceStore::Serialize
    fn write(&self, out: &mut Vec<u8>) {
        let s = &self.state;
        push_bytes(out, VERSION.as_bytes());
        push_count(out, s.erased.len());
        for (hash, expiry) in &s.erased {
            out.extend_from_slice(hash);
            out.extend_from_slice(&expiry.to_le_bytes());
        }
        out.extend_from_slice(&MAX_CACHE_SIZE.to_le_bytes());
        push_count(out, s.orphans.len());
        for (parent, (vote, expiration)) in &s.orphans {
            out.extend_from_slice(parent);
            vote.encode(out);
            out.extend_from_slice(&expiration.to_le_bytes());
        }
        push_count(out, s.objects.len());
        for (hash, (obj, state)) in &s.objects {
            out.extend_from_slice(hash);
            write_object(out, obj, state);
        }
        push_count(out, s.last_object.len());
        for ((hash, n), rec) in &s.last_object {
            out.extend_from_slice(hash);
            out.extend_from_slice(&n.to_le_bytes());
            write_buffer(out, &rec.buffer);
            out.push(u8::from(rec.status_ok));
        }
        let default = MnList::default_pre_dip3();
        dash_dml::codec::encode_canonical_list_unvalidated(
            s.last_mn_list.as_ref().unwrap_or(&default),
            out,
        );
    }

    fn read(&mut self, data: &mut &[u8]) -> Result<(), ReadError> {
        if data.len() > MAX_STORE_SIZE {
            return Err(oversize());
        }
        if read_version(data)? != VERSION.as_bytes() {
            return Err(ReadError::Other(format!("not a {VERSION} store")));
        }
        let dat = decode_store_body_from(data, StoreVersion::V17).map_err(read_error)?;
        *self = from_dat(dat)?;
        Ok(())
    }

    fn clear(&mut self) {
        *self = GovernanceStore::default();
    }

    // Core: governance/governance.cpp:1054-1079 GovernanceStore::ToString
    fn summary(&self) -> String {
        let objects = &self.state.objects;
        let count = |t: i32| objects.values().filter(|(_, s)| s.obj_type == t).count();
        let (proposals, triggers) = (count(object::PROPOSAL), count(object::TRIGGER));
        format!(
            "Governance Objects: {} (Proposals: {proposals}, Triggers: {triggers}, Other: {}; Erased: {})",
            objects.len(),
            objects.len() - proposals - triggers,
            self.state.erased.len()
        )
    }
}

fn from_dat(dat: GovernanceDat) -> Result<GovernanceStore, ReadError> {
    let converted = crate::import::convert(dat).map_err(|e| ReadError::Other(e.to_string()))?;
    Ok(GovernanceStore {
        state: converted.state,
        repaired: converted.repaired,
    })
}

/// The file's bytes for `state` on the network with `message_start`.
pub fn encode(state: StoreState, message_start: [u8; 4]) -> Vec<u8> {
    flatdb::encode(&wrap(state), message_start)
}

/// A file's bytes as `CoreRead` reads them, every failure returned; a file over
/// [`MAX_STORE_SIZE`] is `IncorrectFormat` before any check.
pub fn decode(bytes: &[u8], message_start: [u8; 4]) -> Result<GovernanceStore, LoadError> {
    if bytes.len() > MAX_STORE_SIZE {
        return Err(LoadError::Format(oversize()));
    }
    flatdb::decode(bytes, message_start)
}

/// The error of a store over [`MAX_STORE_SIZE`]: `IncorrectFormat`, so the store is recreated.
fn oversize() -> ReadError {
    ReadError::Other(format!("{FILE} is over {MAX_STORE_SIZE} bytes"))
}

/// Whether the file at `path` is over [`MAX_STORE_SIZE`]; if so, logs the recreate as flatdb's
/// `Read` would, after `opening`. Nothing of the file is read.
fn over_cap(path: &Path, opening: &str) -> bool {
    if !std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_STORE_SIZE as u64) {
        return false;
    }
    tracing::info!("{opening}");
    if let Some(line) = LoadError::Format(oversize()).core_read_error() {
        tracing::error!("{line}");
    }
    tracing::info!(
        "ERROR: CFlatDB::Read Error reading {}: Read: Magic is ok but data has invalid format, will try to recreate",
        FILE
    );
    true
}

/// Core's `CFlatDB::Load` of the store at `path`: `Err` stops the node; a missing file, or one
/// that [`LoadError::recreates`], gives the empty store. A file over [`MAX_STORE_SIZE`] is not read
/// and is recreated.
// Core: flat-database.h:158-190 CFlatDB::Read, Load
pub fn load(path: &Path, message_start: [u8; 4]) -> Result<GovernanceStore, LoadError> {
    let mut store = GovernanceStore::default();
    if over_cap(path, &format!("Reading info from {FILE}...")) {
        return Ok(store);
    }
    flatdb::load(path, message_start, &mut store)?;
    Ok(store)
}

/// Core's `CFlatDB::Store`, atomically through flatdb: `Ok(false)` when the file on disk is one
/// that refuses to load, `Err` when the write failed. A file over [`MAX_STORE_SIZE`] gets
/// [`load`]'s decision: it is not read, it is removed, and the store is written.
// Core: flat-database.h:192-205 CFlatDB::Store
pub fn store(state: StoreState, path: &Path, message_start: [u8; 4]) -> io::Result<bool> {
    if over_cap(path, &format!("Verifying {FILE} format...")) {
        std::fs::remove_file(path)?;
    }
    flatdb::store(&wrap(state), path, message_start)
}

fn wrap(state: StoreState) -> GovernanceStore {
    GovernanceStore {
        state,
        repaired: Vec::new(),
    }
}

/// The decoder's error as the `ios_base::failure` Core's stream would throw.
fn read_error(e: FormatError) -> ReadError {
    match e {
        FormatError::Truncated {
            ..
        } => ReadError::EndOfData,
        FormatError::Corrupt {
            reason: "non-canonical compact size",
            ..
        } => ReadError::NonCanonical,
        FormatError::Corrupt {
            reason: "compact size exceeds MAX_SIZE",
            ..
        } => ReadError::SizeTooLarge,
        FormatError::Corrupt {
            reason: "vector length limit exceeded",
            ..
        } => ReadError::VectorLimit,
        e => ReadError::Other(e.to_string()),
    }
}

/// The version string: Core's string read, with `ReadCompactSize`'s errors.
fn read_version<'a>(data: &mut &'a [u8]) -> Result<&'a [u8], ReadError> {
    let n = read_compact_size(data)?;
    let (s, rest) = data.split_at_checked(n).ok_or(ReadError::EndOfData)?;
    *data = rest;
    Ok(s)
}

/// `ReadCompactSize` with its range check.
// Core: serialize.h:292-322 ReadCompactSize
fn read_compact_size(data: &mut &[u8]) -> Result<usize, ReadError> {
    let take = |data: &mut &[u8], n: usize| -> Result<u64, ReadError> {
        let (b, rest) = data.split_at_checked(n).ok_or(ReadError::EndOfData)?;
        *data = rest;
        Ok(b.iter().rev().fold(0, |v, b| (v << 8) | u64::from(*b)))
    };
    let (n, min) = match take(data, 1)? {
        253 => (take(data, 2)?, 253),
        254 => (take(data, 4)?, 0x1_0000),
        255 => (take(data, 8)?, 0x1_0000_0000),
        first => (first, 0),
    };
    if n < min {
        return Err(ReadError::NonCanonical);
    }
    if n > flatdb::MAX_SIZE {
        return Err(ReadError::SizeTooLarge);
    }
    usize::try_from(n).map_err(|_| ReadError::SizeTooLarge)
}

// Core: governance/object.h:295-304 CGovernanceObject::Serialize (SER_DISK)
fn write_object(out: &mut Vec<u8>, obj: &GovObject, state: &ObjectState) {
    obj.encode(out);
    out.extend_from_slice(&state.deletion_time.to_le_bytes());
    out.push(u8::from(state.expired));
    push_count(out, state.current.len());
    for ((hash, n), record) in &state.current {
        out.extend_from_slice(hash);
        out.extend_from_slice(&n.to_le_bytes());
        push_count(out, record.len());
        for (signal, i) in record {
            out.extend_from_slice(&signal.to_le_bytes());
            out.extend_from_slice(&i.outcome.to_le_bytes());
            out.extend_from_slice(&i.last_update.to_le_bytes());
            out.extend_from_slice(&i.creation_time.to_le_bytes());
        }
    }
    let votes: Vec<&GovVote> = state.file.votes().collect();
    out.extend_from_slice(&i32::try_from(votes.len()).unwrap_or(i32::MAX).to_le_bytes());
    push_count(out, votes.len());
    for vote in votes {
        vote.encode(out);
    }
}

// Core: governance/governance.h:150-153 CRateCheckBuffer::SERIALIZE_METHODS
fn write_buffer(out: &mut Vec<u8>, b: &RateCheckBuffer) {
    push_count(out, b.timestamps().len());
    for t in b.timestamps() {
        out.extend_from_slice(&t.to_le_bytes());
    }
    for p in [b.start(), b.end()] {
        out.extend_from_slice(&i32::try_from(p).unwrap_or(i32::MAX).to_le_bytes());
    }
    out.push(u8::from(b.is_empty()));
}

fn push_count(out: &mut Vec<u8>, n: usize) {
    CompactSize::from(n).encode(out);
}

fn push_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    push_count(out, bytes.len());
    out.extend_from_slice(bytes);
}
