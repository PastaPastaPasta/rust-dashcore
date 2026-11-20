//! The spork store's file: `sporks.dat` in rdashd's own format (A1 §2.9; Core's is import-only,
//! MNO-04, IM1b), read and written with Core's `CFlatDB` outcomes ([`crate::flatdb`]).
//!
//! Layout, every string with a compact-size length:
//! `"rdashd-sporks" ‖ message start (4) ‖ "SporkStore-1" ‖ by-hash sporks ‖ active sporks ‖
//! SHA256d of everything before it`. Each spork list is a compact-size count of sporks in Core's
//! serialization; the active list is in Core's iteration order, and loading reinserts it in that
//! order as Core's `Unserialize` does (which usually reverses it).

use std::io;
use std::path::Path;

use dash_evo::spork::{SporkStore, core_bytes};
use dash_p2p_core::Spork;
use dash_types::codec::{BaseCodec, DecodeError};

pub use crate::flatdb::LoadError;
use crate::flatdb::{
    self, FlatObject, ReadError, capacity, push_compact_size, push_str, read_compact_size,
};

use super::SporkManager;

/// The file's name in the network's data directory (Core's).
pub const FILE_NAME: &str = "sporks.dat";

/// The format version (Core's `SERIALIZATION_VERSION_STRING` role).
const VERSION: &str = "SporkStore-1";

/// `CPubKey::COMPACT_SIGNATURE_SIZE`, the `LIMITED_VECTOR` bound of a spork's signature.
// Core: pubkey.h:41
const COMPACT_SIGNATURE_SIZE: usize = 65;

/// Core's throw for a spork base-sdk refuses: its stream errors in Core's words, anything else
/// in base-sdk's.
fn spork_error(e: DecodeError) -> ReadError {
    match e {
        DecodeError::Eof {
            ..
        } => ReadError::EndOfData,
        DecodeError::NonMinimalCompactSize {
            ..
        } => ReadError::NonCanonical,
        e => ReadError::Other(e.to_string()),
    }
}

/// One `CSporkMessage`. Its signature's length is read as every compact size is (canonical, at
/// most `MAX_SIZE`), then held to [`COMPACT_SIGNATURE_SIZE`], before base-sdk decodes the spork.
// Core: spork.h:113-117; serialize.h:1000-1031 UnserializeVectorWithMaxSize
fn read_spork(data: &mut &[u8]) -> Result<Spork, ReadError> {
    let mut sig = *data;
    // `nSporkID`, `nValue`, `nTimeSigned`.
    flatdb::read_bytes(&mut sig, 4 + 8 + 8)?;
    if read_compact_size(&mut sig)? > COMPACT_SIGNATURE_SIZE {
        return Err(ReadError::VectorLimit);
    }
    Spork::decode(data).map_err(spork_error)
}

fn read_sporks(data: &mut &[u8]) -> Result<Vec<Spork>, ReadError> {
    let n = read_compact_size(data)?;
    let mut sporks = Vec::with_capacity(capacity(n));
    for _ in 0..n {
        sporks.push(read_spork(data)?);
    }
    Ok(sporks)
}

/// The file's object: the store.
#[derive(Default)]
struct SporkFile(SporkStore);

impl FlatObject for SporkFile {
    const FILE: &'static str = FILE_NAME;
    /// Unlike Core's `"magicSporkCache"`, so neither reads the other's file.
    const MAGIC: &'static str = "rdashd-sporks";

    fn write(&self, out: &mut Vec<u8>) {
        push_str(out, VERSION.as_bytes());
        for list in [self.0.all().collect::<Vec<_>>(), self.0.active().iter().collect()] {
            push_compact_size(out, list.len());
            for s in list {
                out.extend_from_slice(&core_bytes(s));
            }
        }
    }

    // Core: spork.h SporkStore::Unserialize
    fn read(&mut self, data: &mut &[u8]) -> Result<(), ReadError> {
        // Another version string leaves the store as it was and reports success.
        if flatdb::read_str(data)? != VERSION.as_bytes() {
            return Ok(());
        }
        let by_hash = read_sporks(data)?;
        let active = read_sporks(data)?;
        self.0 = SporkStore::restore(by_hash, active);
        Ok(())
    }

    fn clear(&mut self) {
        self.0 = SporkStore::new();
    }

    fn summary(&self) -> String {
        format!("Sporks: {}", self.0.active().len())
    }
}

/// The file's bytes for `store` on the network with `message_start`.
pub fn encode(store: &SporkStore, message_start: [u8; 4]) -> Vec<u8> {
    flatdb::encode(&SporkFile(store.clone()), message_start)
}

/// Reads a file's bytes: Core's `CoreRead` checks, in its order.
pub fn decode(bytes: &[u8], message_start: [u8; 4]) -> Result<SporkStore, LoadError> {
    flatdb::decode::<SporkFile>(bytes, message_start).map(|f| f.0)
}

impl SporkManager {
    /// Core's `LoadCache` (init step 7a): reads `path` into the store, then `CheckAndRemove`.
    /// `Err` is Core's `"Failed to load sporks cache from %s"`; a missing or badly formatted file
    /// leaves the store empty. Core stores the file at shutdown only after a load that succeeded.
    // Core: spork.cpp:52-60; flat-database.h:186-190 CFlatDB::Load; init.cpp:1967-1972
    pub fn load(&self, path: &Path, message_start: [u8; 4]) -> Result<(), LoadError> {
        // Core reads into the manager itself: a missing file or another version leaves it as it
        // was, a badly formatted one clears it.
        let mut file =
            SporkFile(std::mem::replace(&mut self.state.write().store, SporkStore::new()));
        let loaded = flatdb::load(path, message_start, &mut file);
        self.state.write().store = file.0;
        loaded?;
        self.check_and_remove();
        Ok(())
    }

    /// Core's `CFlatDB::Store` at shutdown (the `CSporkManager` destructor): verifies the file
    /// on disk first and leaves one Core would refuse to load untouched. Writes a temporary file
    /// and renames it over the old one.
    // Core: spork.cpp:46-50; flat-database.h:36-73 CoreWrite, 192-205 Store
    pub fn store(&self, path: &Path, message_start: [u8; 4]) -> io::Result<bool> {
        let file = SporkFile(self.state.read().store.clone());
        flatdb::store(&file, path, message_start)
    }
}
