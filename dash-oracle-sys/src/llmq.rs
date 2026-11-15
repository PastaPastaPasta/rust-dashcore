//! Core's quorum managers over a chain the caller seeds (ABI 11; `src/test/oracle/oracle.h`,
//! "LLMQ contexts"): `GetAllQuorumMembers()`, `PreComputeQuorumMembers()`, `ScanQuorums()`,
//! `SelectQuorumForSigning()` and `IsQuorumTypeEnabled()` with the record-only trace Core's
//! hooks reported for each call, and the canonical digest of the `llmq_S` snapshot rows.
//!
//! A context selects its chain in Core's process-wide globals, so one may exist per process,
//! never alongside a peer session. [`LlmqContext`] holds its pin's lock exclusively for its
//! lifetime, as [`PeerSession`] does: another thread's oracle call of the pin waits until the
//! context is freed, and a call on the context's own thread is refused
//! ([`ErrorKind::SessionLive`]).
//!
//! A call that makes Core read state the context does not hold (a masternode list, a work
//! block's coinbase, the snapshot of a block below the first header) or a seeded snapshot
//! shorter than the list Core reads it against fails with [`ErrorKind::Context`] and poisons the
//! context: every later call fails with [`ErrorKind::ContextPoisoned`], as after a stateful call
//! during which Core threw. Drop it and seed a new one.
//!
//! Seeding closes at the first stateful call (members, precompute, scan, select) and reopens at
//! [`LlmqContext::restart`]: Core's caches hold what it computed from the seeded state.

use super::addrman::{take, take_flag, take_hash, take_le, take_u8};
use super::*;

use std::collections::HashMap;
use std::ptr::NonNull;
use std::sync::RwLockWriteGuard;

/// The length of an `llmq_S` row key: the serialized `("llmq_S", snapshot hash)`.
pub const SNAPSHOT_KEY_LEN: usize = 39;

/// What `PreComputeQuorumMembers()` runs as (its `reset_cache` argument).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Precompute {
    /// As Core connects a block (`reset_cache` false).
    Connect,
    /// As Core undoes one (`reset_cache` true).
    Undo,
}

/// `GetAllQuorumMembers()`'s return (oracle.h trace format, MEMBERS).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MembersOutcome {
    /// The type is not enabled at the base's previous block: no members.
    Gate = 0,
    /// The members of the base, cached.
    BaseHit = 1,
    /// A rotated quorum index at or above `signingActiveQuorumCount`: no members.
    IndexOutOfRange = 2,
    /// The cycle's members by quorum index, cached.
    IndexHit = 3,
    /// Computed.
    Computed = 4,
}

/// A block-index `GetQuorum()`'s return (GET_QUORUM).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GetQuorumOutcome {
    NotMined = 0,
    CacheHit = 1,
    Built = 2,
    NoMinedCommitment = 3,
}

/// How `ScanQuorums()` used its cache (SCAN).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScanCache {
    /// The chain overload: no cache.
    Bypass = 0,
    Miss = 1,
    /// A hit that satisfied the request.
    HitSatisfied = 2,
    /// A hit continued by a scan.
    HitContinued = 3,
    /// A continuation that stopped at genesis.
    ContinuationBail = 4,
}

/// How the checks a lock or recovered signature meets before its verification passes ended
/// (`ORACLE_LOCK_*`, ABI 26), in the order Core checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LockStatus {
    /// The passes ran ([`LockVerdict::passes`]).
    Checked = 0,
    /// `isdlock`: more than `InstantSendLock::MAX_INPUTS` inputs.
    Oversized = 1,
    /// `isdlock`: `TriviallyValid()` fails.
    TriviallyInvalid = 2,
    /// `isdlock`: the cycle hash is not in the block index.
    UnknownCycle = 3,
    /// `isdlock`: the cycle block is not at a `dkgInterval` boundary.
    InvalidCycle = 4,
    /// `qsigrec`: the LLMQ type is not configured.
    UnknownType = 5,
    /// `qsigrec`: `IsQuorumActive()` is false.
    InactiveQuorum = 6,
    /// `isdlock`, `qsigrec`: the lazy signature reads invalid.
    MalformedSignature = 7,
}

/// How one verification pass ended (`llmq::VerifyRecSigStatus`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PassStatus {
    Valid = 0,
    Invalid = 1,
    NoQuorum = 2,
}

/// One verification pass: its status and the quorum it used, if it found one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LockPass {
    pub status: PassStatus,
    pub quorum: Option<[u8; 32]>,
}

/// Core's verdict on a `clsig`, `isdlock` or `qsigrec` (oracle.h, ABI 26).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LockVerdict {
    pub status: LockStatus,
    /// The passes that ran, in order: at most one for `clsig` and `qsigrec`, two for an
    /// `isdlock` whose first pass did not verify.
    pub passes: Vec<LockPass>,
    /// `::SerializeHash` of the message as Core read it: its inv hash.
    pub hash: [u8; 32],
}

impl LockVerdict {
    fn from_ffi(v: &ffi::OracleLockVerdict) -> LockVerdict {
        use LockStatus::*;
        let status = match v.status {
            0 => Checked,
            1 => Oversized,
            2 => TriviallyInvalid,
            3 => UnknownCycle,
            4 => InvalidCycle,
            5 => UnknownType,
            6 => InactiveQuorum,
            7 => MalformedSignature,
            other => panic!("the shim's lock status {other}"),
        };
        let passes = (0..2)
            .map_while(|i| {
                let status = match v.pass_status[i] {
                    0 => PassStatus::Valid,
                    1 => PassStatus::Invalid,
                    2 => PassStatus::NoQuorum,
                    3 => return None,
                    other => panic!("the shim's pass status {other}"),
                };
                let quorum = v.pass_quorum[i];
                Some(LockPass {
                    status,
                    quorum: (quorum != [0u8; 32]).then_some(quorum),
                })
            })
            .collect();
        LockVerdict {
            status,
            passes,
            hash: v.hash,
        }
    }

    /// Whether a pass verified the signature.
    pub fn accepted(&self) -> bool {
        self.passes.iter().any(|p| p.status == PassStatus::Valid)
    }
}

/// Core's verdict on a mined commitment's signatures and structure (oracle.h
/// `oracle_llmq_verify_mined`, ABI 26).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MinedVerdict {
    Accepted,
    /// `CheckLLMQCommitment()`'s `Verify(checkSigs=false)` failed (bad-qc-invalid).
    VerifyFailed,
    /// `ProcessBlock()`'s queued signature checks failed (bad-qc-invalid).
    SignaturesFailed,
}

/// One event of a call's trace, in the order Core reported them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TraceEvent {
    /// `StoreSnapshotForBlock()` wrote a row: `key` is `("llmq_S", snapshot hash)` serialized,
    /// `snapshot` the `CQuorumSnapshot`.
    Row {
        key: [u8; SNAPSHOT_KEY_LEN],
        snapshot: Vec<u8>,
    },
    Members {
        llmq_type: u8,
        base: [u8; 32],
        reset: bool,
        outcome: MembersOutcome,
        n: u64,
    },
    GetQuorum {
        llmq_type: u8,
        base: [u8; 32],
        populate_cache: bool,
        chain_overload: bool,
        outcome: GetQuorumOutcome,
    },
    Scan {
        llmq_type: u8,
        /// The start block the scan cache is keyed by.
        store: [u8; 32],
        cache: ScanCache,
        cache_written: bool,
        n: u64,
    },
}

impl TraceEvent {
    /// A trace as the shim writes it: `CompactSize(n) || events`. `None` if malformed.
    pub fn decode_trace(mut bytes: &[u8]) -> Option<Vec<TraceEvent>> {
        let r = &mut bytes;
        let events = take_items(r, Self::decode)?;
        r.is_empty().then_some(events)
    }

    fn decode(r: &mut &[u8]) -> Option<TraceEvent> {
        Some(match take_u8(r)? {
            0x01 => TraceEvent::Row {
                key: take(r, SNAPSHOT_KEY_LEN)?.try_into().ok()?,
                snapshot: take_sized(r)?,
            },
            0x02 => TraceEvent::Members {
                llmq_type: take_u8(r)?,
                base: take_hash(r)?,
                reset: take_flag(r)?,
                outcome: match take_u8(r)? {
                    0 => MembersOutcome::Gate,
                    1 => MembersOutcome::BaseHit,
                    2 => MembersOutcome::IndexOutOfRange,
                    3 => MembersOutcome::IndexHit,
                    4 => MembersOutcome::Computed,
                    _ => return None,
                },
                n: read_compact_size(r)?,
            },
            0x03 => TraceEvent::GetQuorum {
                llmq_type: take_u8(r)?,
                base: take_hash(r)?,
                populate_cache: take_flag(r)?,
                chain_overload: take_flag(r)?,
                outcome: match take_u8(r)? {
                    0 => GetQuorumOutcome::NotMined,
                    1 => GetQuorumOutcome::CacheHit,
                    2 => GetQuorumOutcome::Built,
                    3 => GetQuorumOutcome::NoMinedCommitment,
                    _ => return None,
                },
            },
            0x04 => TraceEvent::Scan {
                llmq_type: take_u8(r)?,
                store: take_hash(r)?,
                cache: match take_u8(r)? {
                    0 => ScanCache::Bypass,
                    1 => ScanCache::Miss,
                    2 => ScanCache::HitSatisfied,
                    3 => ScanCache::HitContinued,
                    4 => ScanCache::ContinuationBail,
                    _ => return None,
                },
                cache_written: take_flag(r)?,
                n: read_compact_size(r)?,
            },
            _ => return None,
        })
    }
}

/// A quorum member: its proTxHash and the `CDeterministicMN` as evodb stores it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub pro_tx_hash: [u8; 32],
    pub dmn: Vec<u8>,
}

/// An `llmq-fixture v1` (oracle.h "LLMQ fixture v1"): what [`LlmqContext::load`] seeds, in this
/// order. [`LlmqFixture::encode`] and [`LlmqFixture::decode`] are each other's inverse on
/// Core's canonical encoding, except that decode takes UTF-8 strings only. Core reads any bytes,
/// but its load refuses a chain or options that differ from the context's, and a [`Chain`]'s
/// are UTF-8.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LlmqFixture {
    pub chain: String,
    pub args: Vec<String>,
    /// (height, header)
    pub headers: Vec<(i32, [u8; 80])>,
    /// (block, `CDeterministicMNList` as evodb stores it)
    pub lists: Vec<([u8; 32], Vec<u8>)>,
    /// (block, coinbase transaction, have_data)
    pub coinbases: Vec<([u8; 32], Vec<u8>, bool)>,
    /// (LLMQ type, cycle base, `CQuorumSnapshot`)
    pub snapshots: Vec<(u8, [u8; 32], Vec<u8>)>,
    /// (LLMQ type, `CFinalCommitment`, mined height)
    pub mined: Vec<(u8, Vec<u8>, i32)>,
}

/// The fixture format version [`LlmqFixture`] reads and writes.
pub const LLMQ_FIXTURE_VERSION: u8 = 1;

impl LlmqFixture {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![LLMQ_FIXTURE_VERSION];
        let sized = |out: &mut Vec<u8>, bytes: &[u8]| {
            write_compact_size(out, bytes.len() as u64);
            out.extend(bytes);
        };
        sized(&mut out, self.chain.as_bytes());
        write_compact_size(&mut out, self.args.len() as u64);
        self.args.iter().for_each(|a| sized(&mut out, a.as_bytes()));
        write_compact_size(&mut out, self.headers.len() as u64);
        for (height, header) in &self.headers {
            out.extend(height.to_le_bytes());
            out.extend(header);
        }
        write_compact_size(&mut out, self.lists.len() as u64);
        for (block, list) in &self.lists {
            out.extend(block);
            sized(&mut out, list);
        }
        write_compact_size(&mut out, self.coinbases.len() as u64);
        for (block, tx, have_data) in &self.coinbases {
            out.extend(block);
            sized(&mut out, tx);
            out.push(u8::from(*have_data));
        }
        write_compact_size(&mut out, self.snapshots.len() as u64);
        for (llmq_type, cycle_base, snapshot) in &self.snapshots {
            out.push(*llmq_type);
            out.extend(cycle_base);
            sized(&mut out, snapshot);
        }
        write_compact_size(&mut out, self.mined.len() as u64);
        for (llmq_type, commitment, height) in &self.mined {
            out.push(*llmq_type);
            sized(&mut out, commitment);
            out.extend(height.to_le_bytes());
        }
        out
    }

    /// `None` unless `bytes` is a version-1 fixture in Core's canonical encoding (minimal
    /// CompactSizes up to Core's `MAX_SIZE`, UTF-8 strings, have_data 0 or 1), with nothing
    /// after it.
    pub fn decode(mut bytes: &[u8]) -> Option<LlmqFixture> {
        let r = &mut bytes;
        if take_u8(r)? != LLMQ_FIXTURE_VERSION {
            return None;
        }
        let string = |r: &mut &[u8]| String::from_utf8(take_sized(r)?).ok();
        let fixture = LlmqFixture {
            chain: string(r)?,
            args: take_items(r, string)?,
            headers: take_items(r, |r| Some((take_le(r)?, take(r, 80)?.try_into().ok()?)))?,
            lists: take_items(r, |r| Some((take_hash(r)?, take_sized(r)?)))?,
            coinbases: take_items(r, |r| Some((take_hash(r)?, take_sized(r)?, take_flag(r)?)))?,
            snapshots: take_items(r, |r| Some((take_u8(r)?, take_hash(r)?, take_sized(r)?)))?,
            mined: take_items(r, |r| Some((take_u8(r)?, take_sized(r)?, take_le(r)?)))?,
        };
        r.is_empty().then_some(fixture)
    }
}

/// Core's `llmq::WORK_DIFF_DEPTH` (llmq/utils.h): a quorum's members come from the list (and, at
/// V20, the coinbase) of the block this far below its base, or rotated, its cycle base.
const WORK_DIFF_DEPTH: i64 = 8;
/// Rotated members also read the quarters of the three previous cycles (llmq/utils.cpp,
/// GetQuorumQuarterMembersBySnapshot at H - C, H - 2C and H - 3C).
const ROTATION_CYCLES_READ: i64 = 3;

/// The `Consensus::LLMQParams` of a type that a scan depends on (take them from
/// [`Oracle::chainparams_json`](crate::Oracle::chainparams_json), not from a table).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LlmqWindow {
    pub llmq_type: u8,
    pub use_rotation: bool,
    pub dkg_interval: i32,
    pub dkg_mining_window_start: i32,
    pub dkg_mining_window_end: i32,
    pub signing_active_quorum_count: i32,
    pub keep_old_connections: i32,
}

impl LlmqWindow {
    /// The height of the block `ScanQuorums(type, start)` reads commitments up to
    /// (quorumsman.cpp `pindexStore`): the previous cycle's mining-window end before this
    /// cycle's window opens, `start` inside the window, the window's end after it. `None` where
    /// Core bails out below genesis and scans nothing.
    /// Panics if `dkg_interval` is not positive.
    pub fn store_height(&self, start: i32) -> Option<i32> {
        assert!(
            self.dkg_interval > 0,
            "type {}: dkgInterval {}",
            self.llmq_type,
            self.dkg_interval
        );
        let (start, interval) = (i64::from(start), i64::from(self.dkg_interval));
        let cycle = start - start.rem_euclid(interval);
        let mining_end = cycle + i64::from(self.dkg_mining_window_end);
        let store = if start < cycle + i64::from(self.dkg_mining_window_start) {
            (mining_end >= interval).then(|| mining_end - interval)?
        } else {
            start.min(mining_end)
        };
        // At most start; at least 0 for a start of 0 or above.
        Some(i32::try_from(store).expect("at most start"))
    }

    /// The lowest height building the members of the quorum at `base` reads: the list (and, at
    /// V20, the coinbase) of its work block; rotated, the work block of its cycle base (base -
    /// quorum index) and the three previous cycles' snapshots and work-block lists.
    fn members_floor(&self, base: i32) -> i64 {
        let base = i64::from(base);
        if self.use_rotation {
            let interval = i64::from(self.dkg_interval);
            base - base.rem_euclid(interval) - ROTATION_CYCLES_READ * interval - WORK_DIFF_DEPTH
        } else {
            base - WORK_DIFF_DEPTH
        }
    }
}

/// A `ScanQuorums(type, start, count)` a fixture's consumer runs, directly or through
/// SelectQuorumForSigning (count `signingActiveQuorumCount`), with an empty scan cache: on a
/// fresh or restarted context, or through the chain overload (which bypasses the cache). A scan
/// that continues a cached one walks on from the oldest cached quorum, which this does not model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanDemand {
    pub llmq_type: u8,
    pub start_height: i32,
    pub count: u32,
}

impl LlmqFixture {
    /// The base heights of the quorums `ScanQuorums` builds for `demand` from this fixture's
    /// commitments with an empty scan cache, newest first: `max(count, keepOldConnections)` of
    /// them, fewer if the fixture holds fewer (Core returns the first `count`). Core walks its
    /// mined-commitment index from the store block down (blockprocessor.cpp
    /// GetMinedCommitmentsUntilBlock; rotated, GetMinedCommitmentsIndexedUntilBlock: in round
    /// r the r-th newest commitment of each quorum index, in index order, until a round finds
    /// none). A commitment's base height is read from the header whose parent it is.
    pub fn scan_base_heights(
        &self,
        window: &LlmqWindow,
        demand: &ScanDemand,
    ) -> std::result::Result<Vec<i32>, String> {
        Ok(self.scan_walk(window, demand)?.bases)
    }

    fn scan_walk(
        &self,
        window: &LlmqWindow,
        demand: &ScanDemand,
    ) -> std::result::Result<ScanWalk, String> {
        if window.dkg_interval <= 0 {
            return Err(format!("type {}: dkgInterval {}", window.llmq_type, window.dkg_interval));
        }
        if demand.start_height < 0 {
            return Err(format!("a scan from height {}", demand.start_height));
        }
        let mut walk = ScanWalk::default();
        let Some(store) = window.store_height(demand.start_height) else {
            return Ok(walk);
        };
        walk.scanned = true;
        let wanted = (demand.count as usize).max(window.keep_old_connections.max(0) as usize);
        let parent_height: HashMap<&[u8], i32> =
            self.headers.iter().map(|(height, header)| (&header[4..36], height - 1)).collect();
        // (mined height, quorum index, base height) of the type's commitments up to the store.
        let mut mined = Vec::new();
        for (llmq_type, qc, mined_height) in &self.mined {
            if *llmq_type != demand.llmq_type || *mined_height > store {
                continue;
            }
            let fields = (|| {
                let version = u16::from_le_bytes(qc.get(..2)?.try_into().ok()?);
                // The indexed versions (2 and 4) carry quorumIndex after quorumHash.
                let index = (version % 2 == 0)
                    .then(|| Some(i16::from_le_bytes(qc.get(35..37)?.try_into().ok()?)));
                Some((qc.get(3..35)?, index))
            })();
            let (base, index) =
                fields.ok_or_else(|| format!("type {llmq_type}: a malformed commitment"))?;
            // A rotated type's scan reads the indexed keys only; Core writes a non-indexed
            // version under the other key (the version follows rotation's activation).
            let index = match (window.use_rotation, index) {
                (true, None) => continue,
                (_, Some(Some(index))) => index,
                (_, Some(None)) => return Err(format!("type {llmq_type}: a malformed commitment")),
                (false, None) => 0,
            };
            let base_height = *parent_height.get(base).ok_or_else(|| {
                format!(
                    "type {llmq_type} commitment mined at {mined_height}: its base is no fixture \
                     header's parent"
                )
            })?;
            mined.push((*mined_height, index, base_height));
        }
        // The inverse-height keys: newest mined first.
        mined.sort_by_key(|m| std::cmp::Reverse(m.0));
        if !window.use_rotation {
            walk.bases = mined.iter().take(wanted).map(|m| m.2).collect();
            return Ok(walk);
        }
        let by_index: Vec<Vec<i32>> = (0..window.signing_active_quorum_count)
            .map(|q| mined.iter().filter(|m| i32::from(m.1) == q).map(|m| m.2).collect())
            .collect();
        for round in 0.. {
            if walk.bases.len() >= wanted {
                break;
            }
            let need = wanted - walk.bases.len();
            let mut found = Vec::new();
            for index in &by_index {
                match index.get(round) {
                    Some(base) => found.push(*base),
                    // A commitment of this index the fixture lacks would be taken here.
                    None if found.len() < need => walk.index_ran_out = true,
                    None => {}
                }
            }
            if found.is_empty() {
                break;
            }
            walk.bases.extend(found.into_iter().take(need));
        }
        Ok(walk)
    }

    /// Refuse a fixture that cannot answer a demanded scan as the real chain would. A context
    /// holds no commitment whose base is below its first header, so there Core's scan finds
    /// fewer or other quorums without an error. Unless the fixture starts at genesis it must
    /// hold all the quorums the scan builds ([`scan_base_heights`](Self::scan_base_heights)),
    /// and rotated, no quorum index may run out where the walk would take one of its older
    /// commitments. Every built quorum's member reads must stay at or above its first header.
    pub fn check_scan_coverage(
        &self,
        windows: &[LlmqWindow],
        demands: &[ScanDemand],
    ) -> std::result::Result<(), String> {
        let first = self
            .headers
            .iter()
            .map(|(height, _)| *height)
            .min()
            .ok_or("a fixture without headers")?;
        for demand in demands {
            let window = windows
                .iter()
                .find(|w| w.llmq_type == demand.llmq_type)
                .ok_or_else(|| format!("no LLMQ parameters for type {}", demand.llmq_type))?;
            let scan = format!(
                "type {} scan of {} from height {}",
                demand.llmq_type, demand.count, demand.start_height
            );
            let walk = self.scan_walk(window, demand)?;
            if !walk.scanned {
                continue; // Core scans nothing
            }
            let wanted = (demand.count as usize).max(window.keep_old_connections.max(0) as usize);
            if first > 0 && walk.bases.len() < wanted {
                return Err(format!(
                    "{scan} builds {} of {wanted} quorums from the fixture: older ones may be \
                     mined with bases below its first header ({first})",
                    walk.bases.len()
                ));
            }
            if first > 0 && walk.index_ran_out {
                return Err(format!(
                    "{scan}: a quorum index runs out of commitments in the fixture where the scan \
                     would take an older one, which may have its base below the first header \
                     ({first})"
                ));
            }
            for base in &walk.bases {
                let floor = window.members_floor(*base).max(0);
                if floor < i64::from(first) {
                    return Err(format!(
                        "{scan}: the quorum at {base} reads down to height {floor}, below the \
                         fixture's first header ({first})"
                    ));
                }
            }
        }
        Ok(())
    }
}

/// A simulated scan: the quorums it builds and whether a rotated quorum index ran out where the
/// walk would have taken one of its commitments.
#[derive(Default)]
struct ScanWalk {
    scanned: bool,
    bases: Vec<i32>,
    index_ran_out: bool,
}

/// Core's `MAX_SIZE` (serialize.h): the largest CompactSize `ReadCompactSize` accepts.
const MAX_SIZE: u64 = 0x0200_0000;

/// A CompactSize as Core reads it: the shortest encoding only, at most [`MAX_SIZE`].
fn take_canonical_size(r: &mut &[u8]) -> Option<u64> {
    let first = *r.first()?;
    let n = read_compact_size(r)?;
    let minimum = match first {
        0xfd => 0xfd,
        0xfe => 0x1_0000,
        0xff => 0x1_0000_0000,
        _ => 0,
    };
    (n >= minimum && n <= MAX_SIZE).then_some(n)
}

fn take_sized(r: &mut &[u8]) -> Option<Vec<u8>> {
    let len = usize::try_from(take_canonical_size(r)?).ok()?;
    Some(take(r, len)?.to_vec())
}

fn take_items<T>(r: &mut &[u8], mut item: impl FnMut(&mut &[u8]) -> Option<T>) -> Option<Vec<T>> {
    let n = take_canonical_size(r)?;
    (0..n).map(|_| item(r)).collect()
}

/// A live LLMQ context. Dropping it frees Core's managers and the chain selection.
pub struct LlmqContext {
    oracle: Oracle,
    handle: NonNull<ffi::OracleLlmqCtx>,
    // Dropped in this order: the thread's mark first, then the lock.
    _hold: Hold,
    _state: RwLockWriteGuard<'static, PinState>,
}

impl Drop for LlmqContext {
    fn drop(&mut self) {
        unsafe { (self.oracle.api.llmq_ctx_free)(self.handle.as_ptr()) }
    }
}

impl Oracle {
    /// A new context on `chain`. Blocks while another thread uses this pin;
    /// [`ErrorKind::SessionLive`] if the calling thread holds a peer session or context.
    pub fn llmq_context(&self, chain: &Chain) -> Result<LlmqContext> {
        match self.pin.held() {
            Held::None => {}
            Held::Session => return Err(session_live(self.pin)),
            held => unreachable!("an LLMQ context start inside another oracle call ({held:?})"),
        }
        let mut state = self.pin.state().write().unwrap_or_else(|e| e.into_inner());
        state.usable()?;
        // The context selects its own chain; the next oracle call selects the network again.
        state.chain = None;
        let hold = Hold::new(self.pin, Held::Session);
        let (name, args) = chain_cstrings(chain)?;
        let argv: Vec<*const c_char> = args.iter().map(|a| a.as_ptr()).collect();
        let mut handle = std::ptr::null_mut();
        self.ok(unsafe {
            (self.api.llmq_ctx_new)(name.as_ptr(), argv.as_ptr(), argv.len(), &mut handle)
        })?;
        Ok(LlmqContext {
            oracle: *self,
            handle: NonNull::new(handle).expect("the shim returned a handle"),
            _hold: hold,
            _state: state,
        })
    }

    /// Core's process-wide `bls::bls_legacy_scheme` (every call that sets it restores it). Reads
    /// one atomic, so it takes no lock and may run while a context or peer session lives.
    pub fn bls_legacy_scheme(&self) -> bool {
        let mut legacy = 0;
        self.ok(unsafe { (self.api.bls_legacy_scheme)(&mut legacy) }).expect("a non-null output");
        legacy != 0
    }

    /// Call `f(out1, len1, out2, len2)`, growing both buffers once if the shim reports the
    /// sizes it needs (a stateful call's second call collects the first one's outputs).
    fn with_buffers2(
        &self,
        capacity: (usize, usize),
        mut f: impl FnMut(*mut u8, *mut usize, *mut u8, *mut usize) -> c_int,
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        let (mut a, mut b) = (vec![0u8; capacity.0], vec![0u8; capacity.1]);
        let (mut a_len, mut b_len) = (a.len(), b.len());
        let mut status = f(a.as_mut_ptr(), &mut a_len, b.as_mut_ptr(), &mut b_len);
        if status == ffi::ORACLE_E_BUFFER {
            a.resize(a_len.max(a.len()), 0);
            b.resize(b_len.max(b.len()), 0);
            (a_len, b_len) = (a.len(), b.len());
            status = f(a.as_mut_ptr(), &mut a_len, b.as_mut_ptr(), &mut b_len);
        }
        self.ok(status)?;
        a.truncate(a_len);
        b.truncate(b_len);
        Ok((a, b))
    }
}

/// Buffer sizes a call starts with when the caller names none.
const DEFAULT_CAPACITY: (usize, usize) = (64 * 1024, 4 * 1024);

fn trace(bytes: &[u8]) -> Vec<TraceEvent> {
    TraceEvent::decode_trace(bytes).expect("the shim's LLMQ trace format")
}

fn flag(b: bool) -> c_int {
    c_int::from(b)
}

impl LlmqContext {
    fn ptr(&self) -> *mut ffi::OracleLlmqCtx {
        self.handle.as_ptr()
    }

    /// The shim's handle, for tests of the raw ABI (e.g. a stateful call's pending output, which
    /// the safe calls always collect). Valid while the context lives; never free it.
    pub fn as_raw(&mut self) -> *mut ffi::OracleLlmqCtx {
        self.ptr()
    }

    /// Add a header at `height`; it becomes the active chain's tip.
    pub fn add_header(&mut self, header: &[u8; 80], height: i32) -> Result<()> {
        self.oracle
            .ok(unsafe { (self.oracle.api.llmq_add_header)(self.ptr(), header.as_ptr(), height) })
    }

    /// Set `block`'s masternode list (evodb's encoding, or empty bytes for the empty list).
    pub fn set_mn_list(&mut self, block: &[u8; 32], list: &[u8]) -> Result<()> {
        self.oracle.ok(unsafe {
            (self.oracle.api.llmq_set_mn_list)(
                self.ptr(),
                block.as_ptr(),
                list.as_ptr(),
                list.len(),
            )
        })
    }

    /// Set `block`'s coinbase transaction; `have_data` false makes the block unreadable.
    pub fn set_coinbase(&mut self, block: &[u8; 32], tx: &[u8], have_data: bool) -> Result<()> {
        self.oracle.ok(unsafe {
            (self.oracle.api.llmq_set_coinbase)(
                self.ptr(),
                block.as_ptr(),
                tx.as_ptr(),
                tx.len(),
                flag(have_data),
            )
        })
    }

    /// Store a `CQuorumSnapshot` for `(llmq_type, cycle_base)` through `StoreSnapshotForBlock()`.
    pub fn set_snapshot(
        &mut self,
        llmq_type: u8,
        cycle_base: &[u8; 32],
        snapshot: &[u8],
    ) -> Result<()> {
        self.oracle.ok(unsafe {
            (self.oracle.api.llmq_set_snapshot)(
                self.ptr(),
                llmq_type,
                cycle_base.as_ptr(),
                snapshot.as_ptr(),
                snapshot.len(),
            )
        })
    }

    /// Record a `CFinalCommitment` as mined at `mined_height` on the active chain. Refused
    /// ([`ErrorKind::Argument`]) where Core's `ProcessBlock()` would not store it there (oracle.h
    /// `oracle_llmq_set_mined`: DIP0003, the type enabled, the mining window and cycle base,
    /// `CFinalCommitment::Verify()`, and the signature checks against the members Core computes
    /// from the seeded state). [`ErrorKind::Context`], the context still usable, where those
    /// members read state it does not hold, including a snapshot row a node stores on
    /// connecting a cycle: seed it, or connect the blocks with [`LlmqContext::precompute`] and
    /// [`LlmqContext::restart`], first.
    pub fn set_mined(&mut self, llmq_type: u8, commitment: &[u8], mined_height: i32) -> Result<()> {
        self.oracle.ok(unsafe {
            (self.oracle.api.llmq_set_mined)(
                self.ptr(),
                llmq_type,
                commitment.as_ptr(),
                commitment.len(),
                mined_height,
            )
        })
    }

    /// Seed from a fixture (its chain and args must be the context's).
    pub fn load(&mut self, fixture: &LlmqFixture) -> Result<()> {
        self.load_raw(&fixture.encode())
    }

    /// Seed from fixture bytes.
    pub fn load_raw(&mut self, fixture: &[u8]) -> Result<()> {
        self.oracle.ok(unsafe {
            (self.oracle.api.llmq_ctx_load)(self.ptr(), fixture.as_ptr(), fixture.len())
        })
    }

    /// `IsQuorumTypeEnabled(type, prev, opt_dip24_active, opt_have_dip24_quorums)`.
    pub fn type_enabled(
        &mut self,
        llmq_type: u8,
        prev: Option<&[u8; 32]>,
        opt_dip24_active: Option<bool>,
        opt_have_dip24_quorums: Option<bool>,
    ) -> Result<bool> {
        let opt = |o: Option<bool>| o.map_or(-1, flag);
        let mut enabled = 0;
        self.oracle.ok(unsafe {
            (self.oracle.api.llmq_type_enabled)(
                self.ptr(),
                llmq_type,
                prev.map_or(std::ptr::null(), |p| p.as_ptr()),
                opt(opt_dip24_active),
                opt(opt_have_dip24_quorums),
                &mut enabled,
            )
        })?;
        Ok(enabled != 0)
    }

    /// `GetAllQuorumMembers(type, base, reset_cache)` under the BLS scheme `legacy_bls`.
    pub fn members(
        &mut self,
        llmq_type: u8,
        base: &[u8; 32],
        legacy_bls: bool,
        reset_cache: bool,
    ) -> Result<(Vec<Member>, Vec<TraceEvent>)> {
        let (members, events) =
            self.members_raw(llmq_type, base, legacy_bls, reset_cache, DEFAULT_CAPACITY)?;
        Ok((decode_members(&members).expect("the shim's members format"), trace(&events)))
    }

    /// [`LlmqContext::members`]' bytes (members, trace) with first buffers of `capacity`.
    pub fn members_raw(
        &mut self,
        llmq_type: u8,
        base: &[u8; 32],
        legacy_bls: bool,
        reset_cache: bool,
        capacity: (usize, usize),
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        let ctx = self.ptr();
        self.oracle.with_buffers2(capacity, |m, m_len, t, t_len| unsafe {
            (self.oracle.api.llmq_members)(
                ctx,
                llmq_type,
                base.as_ptr(),
                flag(legacy_bls),
                flag(reset_cache),
                m,
                m_len,
                t,
                t_len,
            )
        })
    }

    /// `PreComputeQuorumMembers(block, reset_cache)`, as Core connects or undoes `block`.
    pub fn precompute(
        &mut self,
        block: &[u8; 32],
        legacy_bls: bool,
        mode: Precompute,
    ) -> Result<Vec<TraceEvent>> {
        let ctx = self.ptr();
        let events = self.oracle.with_buffer(DEFAULT_CAPACITY.1, |t, t_len| unsafe {
            (self.oracle.api.llmq_precompute)(
                ctx,
                block.as_ptr(),
                flag(legacy_bls),
                flag(mode == Precompute::Undo),
                t,
                t_len,
            )
        })?;
        Ok(trace(&events))
    }

    /// `ScanQuorums(type, start, count)` (or the active-chain overload): quorum base hashes,
    /// newest first.
    pub fn scan(
        &mut self,
        llmq_type: u8,
        start: &[u8; 32],
        count: u32,
        chain_overload: bool,
    ) -> Result<(Vec<[u8; 32]>, Vec<TraceEvent>)> {
        let ctx = self.ptr();
        let (bases, events) =
            self.oracle.with_buffers2(DEFAULT_CAPACITY, |b, b_len, t, t_len| unsafe {
                (self.oracle.api.llmq_scan)(
                    ctx,
                    llmq_type,
                    start.as_ptr(),
                    count,
                    flag(chain_overload),
                    b,
                    b_len,
                    t,
                    t_len,
                )
            })?;
        Ok((decode_bases(&bases).expect("the shim's bases format"), trace(&events)))
    }

    /// `SelectQuorumForSigning(type, active chain, selection_hash, sign_height, offset)`
    /// (`sign_height` None: the tip): the selected quorum's base hash, if any.
    pub fn select(
        &mut self,
        llmq_type: u8,
        selection_hash: &[u8; 32],
        sign_height: Option<i32>,
        offset: i32,
    ) -> Result<(Option<[u8; 32]>, Vec<TraceEvent>)> {
        let (mut out, mut found) = ([0u8; 32], 0);
        let ctx = self.ptr();
        let events = self.oracle.with_buffer(DEFAULT_CAPACITY.1, |t, t_len| unsafe {
            (self.oracle.api.llmq_select)(
                ctx,
                llmq_type,
                selection_hash.as_ptr(),
                sign_height.unwrap_or(-1),
                offset,
                out.as_mut_ptr(),
                &mut found,
                t,
                t_len,
            )
        })?;
        Ok(((found != 0).then_some(out), trace(&events)))
    }

    /// One lock entry: the payload under `legacy_bls`, the verdict and the trace.
    fn lock_verdict(
        &mut self,
        entry: unsafe extern "C" fn(
            *mut ffi::OracleLlmqCtx,
            *const u8,
            usize,
            c_int,
            *mut ffi::OracleLockVerdict,
            *mut u8,
            *mut usize,
        ) -> c_int,
        payload: &[u8],
        legacy_bls: bool,
    ) -> Result<(LockVerdict, Vec<TraceEvent>)> {
        let mut verdict = ffi::OracleLockVerdict::default();
        let ctx = self.ptr();
        let events = self.oracle.with_buffer(DEFAULT_CAPACITY.1, |t, t_len| unsafe {
            entry(ctx, payload.as_ptr(), payload.len(), flag(legacy_bls), &mut verdict, t, t_len)
        })?;
        Ok((LockVerdict::from_ffi(&verdict), trace(&events)))
    }

    /// `chainlock::VerifyChainLock(params, active chain, qman, clsig)` of a `clsig` payload, on a
    /// node with ChainLocks enabled. The unrequested-CLSIG check (10 points) is peer state, outside
    /// the entry (`oracle.h`).
    pub fn verify_chainlock(
        &mut self,
        clsig: &[u8],
        legacy_bls: bool,
    ) -> Result<(LockVerdict, Vec<TraceEvent>)> {
        self.lock_verdict(self.oracle.api.llmq_verify_chainlock, clsig, legacy_bls)
    }

    /// An `isdlock` payload through `NetInstantSend`'s checks, each pass verifying the one lock.
    pub fn verify_islock(
        &mut self,
        isdlock: &[u8],
        legacy_bls: bool,
    ) -> Result<(LockVerdict, Vec<TraceEvent>)> {
        self.lock_verdict(self.oracle.api.llmq_verify_islock, isdlock, legacy_bls)
    }

    /// A `qsigrec` payload through Core's intake, verifying the one signature.
    pub fn verify_qsigrec(
        &mut self,
        qsigrec: &[u8],
        legacy_bls: bool,
    ) -> Result<(LockVerdict, Vec<TraceEvent>)> {
        self.lock_verdict(self.oracle.api.llmq_verify_qsigrec, qsigrec, legacy_bls)
    }

    /// `CheckLLMQCommitment()`'s `Verify(checkSigs=false)`, then `ProcessBlock()`'s queued
    /// signature checks, of a non-null mined commitment in the mining block's scheme.
    pub fn verify_mined(
        &mut self,
        commitment: &[u8],
        legacy_bls: bool,
    ) -> Result<(MinedVerdict, Vec<TraceEvent>)> {
        let (mut ok, mut stage) = (0, 0);
        let ctx = self.ptr();
        let events = self.oracle.with_buffer(DEFAULT_CAPACITY.1, |t, t_len| unsafe {
            (self.oracle.api.llmq_verify_mined)(
                ctx,
                commitment.as_ptr(),
                commitment.len(),
                flag(legacy_bls),
                &mut ok,
                &mut stage,
                t,
                t_len,
            )
        })?;
        let verdict = match (ok, stage) {
            (1, 0) => MinedVerdict::Accepted,
            (0, 1) => MinedVerdict::VerifyFailed,
            (0, 2) => MinedVerdict::SignaturesFailed,
            other => panic!("the shim's mined verdict {other:?}"),
        };
        Ok((verdict, trace(&events)))
    }

    /// Restart the node over the same evodb (fresh managers and statics).
    pub fn restart(&mut self) -> Result<()> {
        self.oracle.ok(unsafe { (self.oracle.api.llmq_restart)(self.ptr()) })
    }

    /// The canonical digest of evodb's `llmq_S` rows.
    pub fn rows_digest(&mut self) -> Result<[u8; 32]> {
        let mut out = [0u8; 32];
        self.oracle
            .ok(unsafe { (self.oracle.api.llmq_rows_digest)(self.ptr(), out.as_mut_ptr()) })?;
        Ok(out)
    }
}

fn decode_members(mut bytes: &[u8]) -> Option<Vec<Member>> {
    let r = &mut bytes;
    let members = take_items(r, |r| {
        Some(Member {
            pro_tx_hash: take_hash(r)?,
            dmn: take_sized(r)?,
        })
    })?;
    r.is_empty().then_some(members)
}

fn decode_bases(mut bytes: &[u8]) -> Option<Vec<[u8; 32]>> {
    let r = &mut bytes;
    let bases = take_items(r, take_hash)?;
    r.is_empty().then_some(bases)
}
