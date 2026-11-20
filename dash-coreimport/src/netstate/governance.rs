//! `governance.dat`: the governance manager's persistent state (`GovernanceStore`,
//! `governance/governance.h`), in the `CFlatDB` envelope with the magic message
//! `magicGovernanceCache`.
//!
//! The object is a version string, then in this order:
//!
//! 1. `mapErasedGovernanceObjects`: `std::map<uint256, int64_t>`, object hash to the time its
//!    erasure expires;
//! 2. **`-16` only:** the historical invalid-vote cache (`CacheMap<uint256, CGovernanceVote>`),
//!    which Core reads and discards: it is decoded and checked here and not returned;
//! 3. `cmmapOrphanVotes`: a `CacheMultiMap<uint256, OrphanVote>`;
//! 4. `mapObjects`: `std::map<uint256, CGovernanceObject>` in the disk form (object, deletion
//!    time, expired flag, the current vote of each masternode, the vote file);
//! 5. `mapLastMasternodeObject`: per masternode collateral, the rate-check buffer of its last
//!    trigger;
//! 6. `lastMNListForVotingKeys`: a whole `CDeterministicMNList`, decoded by `dash-dml`'s
//!    Core-disk codec (`codec::core_disk::decode_list_s3`).
//!
//! Two versions are read ([`StoreVersion`]). `CGovernanceManager-Version-17` is what Dash Core
//! v24.0.0-rc.4 writes (`governance.cpp:31`): it has no field 2, and a governance object's
//! signature is limited to [`MAX_OBJECT_SIGNATURE_SIZE`] bytes (`common.h:37,85`).
//! `CGovernanceManager-Version-16` is what v23 and v24.0.0-rc.3 write (field 2, written empty, and
//! no limit). Core itself reads only the version it writes and leaves the store empty for the
//! other; the importer reads both, so that a datadir from before rc.4 imports in full.
//!
//! Any other version string is not an error: Core leaves the store empty and ignores the rest
//! ([`GovernanceDat::version_matched`]). A repeated key keeps its first entry (`std::map::insert`).
//! The orphan-vote container keeps **every** list item, even one whose `(key, vote)` pair is
//! already indexed, as `CacheMultiMap::RebuildIndex` does.
//!
//! This reader only decodes. Core's post-load work (recomputing each object's cached validity and
//! funding, re-checking vote signatures against the masternode list, `RemoveInvalidVotes`) belongs
//! to the governance manager that consumes it. In particular, [`RateCheckBuffer`] holds raw
//! numbers that Core uses as array indexes without checking them: a loader must call
//! [`RateCheckBuffer::is_consistent`] first.

use std::collections::{BTreeMap, BTreeSet};

use dash_dml::MnList;
use dash_network::Network;

use super::error::{FormatError, FormatResult};
use super::flat::{self, Loaded, Payload};
use super::reader::Reader;

const WHAT: &str = "governance.dat";

/// Larger than any file a mainnet node writes (tens of MiB with every vote of the live objects);
/// `tests/flatfiles_memory.rs` puts the memory at about 0.9 GiB at this size, the masternode list aside.
pub const MAX_GOVERNANCE_DAT_SIZE: usize = 128 << 20;

/// The most bytes the masternode list at the end of the file may take. A mainnet list is a few
/// MiB at most; `dash-dml`'s Core-disk reader has no length budget, so this cap bounds the list's
/// memory (the bounds table in `netstate/mod.rs` has the figures). A larger list makes the file a
/// bad one, which Core's loader answers with a recreate ([`Loaded::discarded`]); Core itself has
/// no such limit, because it only reads what it wrote.
pub const MAX_MN_LIST_SIZE: usize = 8 << 20;

/// `GovernanceStore::SERIALIZATION_VERSION_STRING` at v24.0.0-rc.4 (`governance.cpp:31`): the
/// version Core writes now.
pub const SERIALIZATION_VERSION_STRING: &str = "CGovernanceManager-Version-17";

/// The version string of v23 and v24.0.0-rc.3 (`governance.cpp:31` of those releases).
pub const SERIALIZATION_VERSION_STRING_V16: &str = "CGovernanceManager-Version-16";

/// A version of the file this reader loads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreVersion {
    /// `CGovernanceManager-Version-16`: v23 and v24.0.0-rc.3.
    V16,
    /// `CGovernanceManager-Version-17`: v24.0.0-rc.4.
    V17,
}

impl StoreVersion {
    fn of(string: &[u8]) -> Option<StoreVersion> {
        if string == SERIALIZATION_VERSION_STRING.as_bytes() {
            Some(StoreVersion::V17)
        } else if string == SERIALIZATION_VERSION_STRING_V16.as_bytes() {
            Some(StoreVersion::V16)
        } else {
            None
        }
    }
}

/// Core's `Governance::Object::MAX_SIGNATURE_SIZE` (`governance/common.h:37`): the longest
/// `vchSig` a `-17` file may hold. A `-16` file has no limit.
pub const MAX_OBJECT_SIGNATURE_SIZE: usize = 96;

/// Core's `RATE_BUFFER_SIZE`.
pub const RATE_BUFFER_SIZE: usize = 5;

/// A `COutPoint`; the derived order is `COutPoint::operator<` (hash bytes, then index).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OutPoint {
    /// The transaction hash (internal byte order).
    pub hash: [u8; 32],
    pub n: u32,
}

impl OutPoint {
    fn read(r: &mut Reader<'_>) -> FormatResult<OutPoint> {
        Ok(OutPoint {
            hash: r.hash256()?,
            n: r.u32_le()?,
        })
    }
}

/// A `CGovernanceVote` in the disk form (the signature is an ordinary vector, not limited to the
/// network sizes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernanceVote {
    pub masternode_outpoint: OutPoint,
    pub parent_hash: [u8; 32],
    /// `vote_outcome_enum_t` (`int`): 0 none, 1 yes, 2 no, 3 abstain, 4 unknown; any value is kept.
    pub outcome: i32,
    /// `vote_signal_enum_t` (`int`): 0 none, 1 funding, 2 valid, 3 delete, 4 endorsed, 5 unknown.
    pub signal: i32,
    pub time: i64,
    pub signature: Vec<u8>,
}

/// The smallest a vote can be on disk.
const MIN_VOTE_SIZE: usize = 36 + 32 + 4 + 4 + 8 + 1;

impl GovernanceVote {
    // Core: governance/vote.h CGovernanceVote::SERIALIZE_METHODS
    fn read(r: &mut Reader<'_>) -> FormatResult<GovernanceVote> {
        Ok(GovernanceVote {
            masternode_outpoint: OutPoint::read(r)?,
            parent_hash: r.hash256()?,
            outcome: r.i32_le()?,
            signal: r.i32_le()?,
            time: r.i64_le()?,
            signature: r.byte_vec()?.to_vec(),
        })
    }
}

/// An `OrphanVote`: a vote held until its object arrives, and when it expires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanVote {
    pub vote: GovernanceVote,
    /// Seconds since the epoch (`ChronoFormatter<int64_t>`).
    pub expiration: i64,
}

/// `CGovernanceManager::MAX_CACHE_SIZE`: the capacity `cmmapOrphanVotes` is constructed with.
pub const MAX_CACHE_SIZE: u32 = 1_000_000;

/// The `CacheMultiMap<uint256, OrphanVote>`: its capacity and every list item, newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanVotes {
    /// `nMaxSize`: what the file stored; [`MAX_CACHE_SIZE`] when the file did not get that far
    /// (`Clear()` keeps the capacity, as does a read that fails after it was assigned).
    pub max_size: u32,
    /// The `listItems`: the object hash each vote is waiting for, and the vote.
    pub items: Vec<([u8; 32], OrphanVote)>,
}

impl Default for OrphanVotes {
    fn default() -> Self {
        OrphanVotes {
            max_size: MAX_CACHE_SIZE,
            items: Vec::new(),
        }
    }
}

/// One masternode's vote instance for one signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoteInstance {
    /// `vote_outcome_enum_t`.
    pub outcome: i32,
    /// `last_update`, seconds since the epoch.
    pub last_update: i64,
    pub creation_time: i64,
}

/// A `vote_rec_t`: the vote instances of one masternode, by signal.
pub type VoteRecord = BTreeMap<i32, VoteInstance>;

/// A `CGovernanceObjectVoteFile` after Core's `RebuildIndex`, which its deserializer runs: a vote
/// whose hash (masternode outpoint, parent, signal, outcome and time) repeats an earlier one is
/// dropped, and the count is that of the votes kept.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VoteFile {
    /// `nMemoryVotes` (Core's `GetVoteCount`): `votes.len()`. The stored value is not kept; Core
    /// overwrites it while rebuilding the index.
    pub memory_votes: i32,
    pub votes: Vec<GovernanceVote>,
}

/// A `CGovernanceObject` in the disk form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernanceObject {
    /// `Governance::Object::hashParent`.
    pub parent_hash: [u8; 32],
    pub revision: i32,
    pub time: i64,
    pub collateral_hash: [u8; 32],
    /// `vchData`: the object's data bytes (a proposal's JSON text; the RPC shows them as `DataHex`).
    pub data: Vec<u8>,
    /// `GovernanceObject` (`int`): 0 unknown, 1 proposal, 2 trigger; any value is kept.
    pub object_type: i32,
    pub masternode_outpoint: OutPoint,
    pub signature: Vec<u8>,
    /// `nDeletionTime`.
    pub deletion_time: i64,
    /// `fExpired`.
    pub expired: bool,
    /// `mapCurrentMNVotes`.
    pub current_votes: BTreeMap<OutPoint, VoteRecord>,
    pub vote_file: VoteFile,
}

impl GovernanceObject {
    // Core: governance/common.h Governance::Object::SERIALIZE_METHODS (`LIMITED_VECTOR` on
    // `vchSig` from rc.4, :85) and governance/object.h CGovernanceObject::Unserialize (SER_DISK)
    fn read(r: &mut Reader<'_>, version: StoreVersion) -> FormatResult<GovernanceObject> {
        let parent_hash = r.hash256()?;
        let revision = r.i32_le()?;
        let time = r.i64_le()?;
        let collateral_hash = r.hash256()?;
        let data = r.byte_vec()?.to_vec();
        let object_type = r.i32_le()?;
        let masternode_outpoint = OutPoint::read(r)?;
        let signature = match version {
            StoreVersion::V16 => r.byte_vec()?,
            StoreVersion::V17 => r.limited_byte_vec(MAX_OBJECT_SIGNATURE_SIZE)?,
        }
        .to_vec();
        let deletion_time = r.i64_le()?;
        let expired = r.bool()?;
        let mut current_votes = BTreeMap::new();
        for _ in 0..flat::count(r)? {
            let outpoint = OutPoint::read(r)?;
            let mut record = VoteRecord::new();
            for _ in 0..flat::count(r)? {
                let signal = r.i32_le()?;
                let instance = VoteInstance {
                    outcome: r.i32_le()?,
                    last_update: r.i64_le()?,
                    creation_time: r.i64_le()?,
                };
                record.entry(signal).or_insert(instance);
            }
            current_votes.entry(outpoint).or_insert(record);
        }
        // `nMemoryVotes` as stored: `RebuildIndex` recounts it.
        r.i32_le()?;
        let n = flat::count(r)?;
        let mut votes = Vec::with_capacity(r.capacity_for(n, MIN_VOTE_SIZE));
        // Core: governance/votedb.cpp CGovernanceObjectVoteFile::RebuildIndex keeps the first
        // vote of each hash. The hash covers these fields and not the signature.
        let mut seen = BTreeSet::new();
        for _ in 0..n {
            let vote = GovernanceVote::read(r)?;
            let key =
                (vote.masternode_outpoint, vote.parent_hash, vote.signal, vote.outcome, vote.time);
            if seen.insert(key) {
                votes.push(vote);
            }
        }
        let memory_votes = votes.len() as i32;
        Ok(GovernanceObject {
            parent_hash,
            revision,
            time,
            collateral_hash,
            data,
            object_type,
            masternode_outpoint,
            signature,
            deletion_time,
            expired,
            current_votes,
            vote_file: VoteFile {
                memory_votes,
                votes,
            },
        })
    }
}

/// A `CRateCheckBuffer`: the timestamps of the last triggers of one masternode.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RateCheckBuffer {
    /// `vecTimestamps` (five entries when Core wrote it).
    pub timestamps: Vec<i64>,
    pub data_start: i32,
    pub data_end: i32,
    pub buffer_empty: bool,
}

impl RateCheckBuffer {
    /// Whether the buffer is what Core's constructor and `AddTimestamp` make: five timestamps and
    /// both positions inside them. Core indexes `timestamps` with the positions unchecked.
    pub fn is_consistent(&self) -> bool {
        let ok = |p: i32| usize::try_from(p).is_ok_and(|p| p < RATE_BUFFER_SIZE);
        self.timestamps.len() == RATE_BUFFER_SIZE && ok(self.data_start) && ok(self.data_end)
    }

    // Core: governance/governance.h CRateCheckBuffer::SERIALIZE_METHODS
    fn read(r: &mut Reader<'_>) -> FormatResult<RateCheckBuffer> {
        let n = flat::count(r)?;
        let mut timestamps = Vec::with_capacity(r.capacity_for(n, 8));
        for _ in 0..n {
            timestamps.push(r.i64_le()?);
        }
        Ok(RateCheckBuffer {
            timestamps,
            data_start: r.i32_le()?,
            data_end: r.i32_le()?,
            buffer_empty: r.bool()?,
        })
    }
}

/// A `last_object_rec`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastObjectRec {
    pub trigger_buffer: RateCheckBuffer,
    pub status_ok: bool,
}

/// A parsed `governance.dat`.
#[derive(Clone)]
pub struct GovernanceDat {
    /// Whether the version string was one this reader loads (`version.is_some()`). When it was
    /// not, the rest is empty.
    pub version_matched: bool,
    /// Which version the file was, when it was one of [`StoreVersion`].
    pub version: Option<StoreVersion>,
    /// `mapErasedGovernanceObjects`.
    pub erased: BTreeMap<[u8; 32], i64>,
    /// `cmmapOrphanVotes`.
    pub orphan_votes: OrphanVotes,
    /// `mapObjects`, by object hash as the file states it. Core does not check the key against
    /// the object's own hash while loading.
    pub objects: BTreeMap<[u8; 32], GovernanceObject>,
    /// `mapLastMasternodeObject`.
    pub last_masternode_object: BTreeMap<OutPoint, LastObjectRec>,
    /// `lastMNListForVotingKeys`; Core's default (pre-DIP3, height -1) when the store is empty
    /// because the version string differed or the file was discarded.
    pub mn_list: MnList,
}

impl PartialEq for GovernanceDat {
    fn eq(&self, other: &Self) -> bool {
        self.version_matched == other.version_matched
            && self.version == other.version
            && self.erased == other.erased
            && self.orphan_votes == other.orphan_votes
            && self.objects == other.objects
            && self.last_masternode_object == other.last_masternode_object
            && self.mn_list.is_equal(&other.mn_list)
    }
}

impl std::fmt::Debug for GovernanceDat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GovernanceDat")
            .field("version_matched", &self.version_matched)
            .field("version", &self.version)
            .field("erased", &self.erased.len())
            .field("orphan_votes", &self.orphan_votes.items.len())
            .field("objects", &self.objects.len())
            .field("last_masternode_object", &self.last_masternode_object.len())
            .field("mn_list", &self.mn_list.len())
            .finish()
    }
}

impl Default for GovernanceDat {
    fn default() -> Self {
        GovernanceDat {
            version_matched: false,
            version: None,
            erased: BTreeMap::new(),
            orphan_votes: OrphanVotes::default(),
            objects: BTreeMap::new(),
            last_masternode_object: BTreeMap::new(),
            mn_list: MnList::default_pre_dip3(),
        }
    }
}

impl Payload for GovernanceDat {
    const FILE: &'static str = WHAT;
    const MAGIC_MESSAGE: &'static str = "magicGovernanceCache";
    const VERSION_STRINGS: &'static [&'static str] =
        &[SERIALIZATION_VERSION_STRING, SERIALIZATION_VERSION_STRING_V16];

    // Core: governance/governance.h GovernanceStore::Unserialize (rc.4 :214-229, rc.3 :221-235)
    fn read(&mut self, r: &mut Reader<'_>) -> FormatResult<()> {
        self.version = StoreVersion::of(r.byte_vec()?);
        self.version_matched = self.version.is_some();
        let Some(version) = self.version else {
            return Ok(());
        };
        for _ in 0..flat::count(r)? {
            let hash = r.hash256()?;
            let expiry = r.i64_le()?;
            self.erased.entry(hash).or_insert(expiry);
        }
        if version == StoreVersion::V16 {
            // The invalid-vote `CacheMap<uint256, CGovernanceVote>`: `nMaxSize`, then the list.
            r.u32_le()?;
            for _ in 0..flat::count(r)? {
                r.hash256()?;
                GovernanceVote::read(r)?;
            }
        }
        // `CacheMultiMap<uint256, OrphanVote>`.
        self.orphan_votes.max_size = r.u32_le()?;
        let n = flat::count(r)?;
        self.orphan_votes.items = Vec::with_capacity(r.capacity_for(n, 32 + MIN_VOTE_SIZE + 8));
        for _ in 0..n {
            let key = r.hash256()?;
            let vote = GovernanceVote::read(r)?;
            let expiration = r.i64_le()?;
            self.orphan_votes.items.push((
                key,
                OrphanVote {
                    vote,
                    expiration,
                },
            ));
        }
        for _ in 0..flat::count(r)? {
            let hash = r.hash256()?;
            let object = GovernanceObject::read(r, version)?;
            self.objects.entry(hash).or_insert(object);
        }
        for _ in 0..flat::count(r)? {
            let outpoint = OutPoint::read(r)?;
            let trigger_buffer = RateCheckBuffer::read(r)?;
            let status_ok = r.bool()?;
            self.last_masternode_object.entry(outpoint).or_insert(LastObjectRec {
                trigger_buffer,
                status_ok,
            });
        }
        let mut rest = r.rest();
        if rest.len() > MAX_MN_LIST_SIZE {
            return Err(FormatError::TooLarge {
                what: "governance.dat masternode list",
                size: rest.len() as u64,
                limit: MAX_MN_LIST_SIZE as u64,
            });
        }
        let before = rest.len();
        let list = dash_dml::codec::core_disk::decode_list_s3(&mut rest).map_err(|e| {
            FormatError::Nested {
                what: WHAT,
                message: format!("lastMNListForVotingKeys: {e}"),
            }
        })?;
        r.take(before - rest.len())?;
        self.mn_list = list;
        Ok(())
    }

    // Core: governance/governance.cpp GovernanceStore::Clear
    fn clear(&mut self) {
        self.objects.clear();
        self.erased.clear();
        // `CacheMultiMap::Clear` keeps `nMaxSize`.
        self.orphan_votes.items.clear();
        self.last_masternode_object.clear();
        self.mn_list = MnList::default_pre_dip3();
    }
}

/// Parses a `governance.dat` of `network`.
// Core: flat-database.h CFlatDB<GovernanceStore>::CoreRead
pub fn parse(bytes: &[u8], network: Network) -> FormatResult<Loaded<GovernanceDat>> {
    flat::load(bytes, network, MAX_GOVERNANCE_DAT_SIZE)
}
