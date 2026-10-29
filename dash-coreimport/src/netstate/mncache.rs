//! `mncache.dat`: what the node keeps about masternodes besides the deterministic list
//! (`MasternodeMetaStore`, `masternode/meta.h`), in the `CFlatDB` envelope with the
//! magic message `magicMasternodeCache`.
//!
//! The object is the version string `CMasternodeMetaMan-Version-5`, a vector of
//! `CMasternodeMetaInfo`, the CoinJoin queue counter and the vector of recently used masternodes.
//! A different version string is not an error: Core leaves everything as it was (empty at start-up)
//! and ignores the rest ([`MncacheDat::version_matched`]). A repeated protx hash keeps its first
//! entry (`std::map::emplace`).
//!
//! `MasternodeMetaStore::Clear` does not reset the queue counter. When the object fails to
//! deserialize after the counter was read, Core's `CFlatDB` clears the rest and the counter keeps
//! what was read: [`Loaded::value`] does the same.

use std::collections::BTreeMap;

use dash_network::Network;

use super::error::FormatResult;
use super::flat::{self, Loaded, Payload};
use super::reader::Reader;

const WHAT: &str = "mncache.dat";

/// A meta record is 70 bytes or more; thousands of masternodes make a few hundred KiB.
pub const MAX_MNCACHE_DAT_SIZE: usize = 64 << 20;

/// `MasternodeMetaStore::SERIALIZATION_VERSION_STRING`.
pub const SERIALIZATION_VERSION_STRING: &str = "CMasternodeMetaMan-Version-5";

/// One `CMasternodeMetaInfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MasternodeMeta {
    /// The masternode's ProRegTx hash (internal byte order).
    pub protx_hash: [u8; 32],
    /// The `dsq` count of its last queue broadcast.
    pub last_dsq: i64,
    pub mixing_tx_count: i32,
    /// Governance objects it voted on, with Core's count; sorted by hash as `std::map` is.
    pub governance_objects_voted_on: BTreeMap<[u8; 32], i32>,
    pub outbound_attempt_count: i32,
    pub last_outbound_attempt: i64,
    pub last_outbound_success: i64,
    /// Whether it is under a platform ban (DIP-0031).
    pub platform_ban: bool,
    /// The height at which the ban was applied or removed.
    pub platform_ban_updated: i32,
}

/// The smallest a record can be: the hash, `i64`, `i32`, an empty map, `i32`, two `i64`, a
/// `bool` and an `i32`.
const MIN_META_SIZE: usize = 32 + 8 + 4 + 1 + 4 + 8 + 8 + 1 + 4;

impl MasternodeMeta {
    // Core: masternode/meta.h CMasternodeMetaInfo::SERIALIZE_METHODS
    fn read(r: &mut Reader<'_>) -> FormatResult<MasternodeMeta> {
        let protx_hash = r.hash256()?;
        let last_dsq = r.i64_le()?;
        let mixing_tx_count = r.i32_le()?;
        let mut governance_objects_voted_on = BTreeMap::new();
        for _ in 0..flat::count(r)? {
            let hash = r.hash256()?;
            let count = r.i32_le()?;
            governance_objects_voted_on.entry(hash).or_insert(count);
        }
        Ok(MasternodeMeta {
            protx_hash,
            last_dsq,
            mixing_tx_count,
            governance_objects_voted_on,
            outbound_attempt_count: r.i32_le()?,
            last_outbound_attempt: r.i64_le()?,
            last_outbound_success: r.i64_le()?,
            platform_ban: r.bool()?,
            platform_ban_updated: r.i32_le()?,
        })
    }
}

/// A parsed `mncache.dat`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MncacheDat {
    /// Whether the version string was Core's. When it was not, the rest is at its start-up value.
    pub version_matched: bool,
    /// `metaInfos`, keyed by protx hash.
    pub infos: BTreeMap<[u8; 32], MasternodeMeta>,
    /// `nDsqCount`.
    pub dsq_count: i64,
    /// `m_used_masternodes`, oldest first (Core's FIFO); a hash may repeat.
    pub used_masternodes: Vec<[u8; 32]>,
}

impl Payload for MncacheDat {
    const FILE: &'static str = WHAT;
    const MAGIC_MESSAGE: &'static str = "magicMasternodeCache";
    const VERSION_STRINGS: &'static [&'static str] = &[SERIALIZATION_VERSION_STRING];

    // Core: masternode/meta.h MasternodeMetaStore::Unserialize
    fn read(&mut self, r: &mut Reader<'_>) -> FormatResult<()> {
        self.version_matched = r.byte_vec()? == SERIALIZATION_VERSION_STRING.as_bytes();
        if !self.version_matched {
            return Ok(());
        }
        // Core reads the vectors into temporaries and moves them in after the last read; the
        // counter is assigned in place, so it survives a later failure.
        let n = flat::count(r)?;
        let mut metas = Vec::with_capacity(r.capacity_for(n, MIN_META_SIZE));
        for _ in 0..n {
            metas.push(MasternodeMeta::read(r)?);
        }
        self.dsq_count = r.i64_le()?;
        let n = flat::count(r)?;
        let mut used = Vec::with_capacity(r.capacity_for(n, 32));
        for _ in 0..n {
            used.push(r.hash256()?);
        }
        for meta in metas {
            self.infos.entry(meta.protx_hash).or_insert(meta);
        }
        self.used_masternodes = used;
        Ok(())
    }

    // Core: masternode/meta.h MasternodeMetaStore::Clear (the counter is not reset)
    fn clear(&mut self) {
        self.infos.clear();
        self.used_masternodes.clear();
    }
}

/// Parses a `mncache.dat` of `network`.
// Core: flat-database.h CFlatDB<MasternodeMetaStore>::CoreRead
pub fn parse(bytes: &[u8], network: Network) -> FormatResult<Loaded<MncacheDat>> {
    flat::load(bytes, network, MAX_MNCACHE_DAT_SIZE)
}
