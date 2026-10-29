//! Masternode sync, fulfilled requests and the masternode meta cache (ABI 30; node-program
//! BL-451, DEC-626).
//!
//! [`Oracle::mnsync_step`], [`Oracle::netfulfilled_step`] and [`Oracle::mncache_step`] run op
//! scripts over a fresh `CMasternodeSync`, `CNetFulfilledRequestManager` and
//! `CMasternodeMetaMan`; [`Oracle::flatdb_load`] is Core's own load of `netfulfilled.dat` or
//! `mncache.dat` from given bytes. The formats are in `oracle.h` ("masternode sync").

use super::*;
use crate::addrman::{
    Service, take, take_flag, take_hash, take_le, take_string, take_u8, take_vec,
};
use crate::payments::malformed;

/// A block as `CMasternodeSync` reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyncBlock {
    pub height: i32,
    /// Core's byte order.
    pub hash: [u8; 32],
}

impl SyncBlock {
    fn write(&self, out: &mut Vec<u8>) {
        out.extend(self.height.to_le_bytes());
        out.extend(self.hash);
    }
}

fn write_optional_block(out: &mut Vec<u8>, block: &Option<SyncBlock>) {
    out.push(block.is_some().into());
    if let Some(block) = block {
        block.write(out);
    }
}

/// One op of a sync script (`oracle_mnsync_step`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncOp {
    /// `SetMockTime(t)` (1..2^33); no record.
    Time(i64),
    UpdatedBlockTip {
        tip: Option<SyncBlock>,
        new: SyncBlock,
        initial_download: bool,
    },
    AcceptedBlockHeader(SyncBlock),
    NotifyHeaderTip {
        block: Option<SyncBlock>,
        initial_download: bool,
    },
    Reset {
        force: bool,
        notify: bool,
    },
    SwitchToNextAsset,
    BumpAttempt,
    BumpAssetLastTime,
}

impl SyncOp {
    /// The op's code in the script, and in the [`SyncState`] it produces.
    pub fn code(&self) -> u8 {
        match self {
            Self::Time(_) => 0,
            Self::UpdatedBlockTip {
                ..
            } => 1,
            Self::AcceptedBlockHeader(_) => 2,
            Self::NotifyHeaderTip {
                ..
            } => 3,
            Self::Reset {
                ..
            } => 4,
            Self::SwitchToNextAsset => 5,
            Self::BumpAttempt => 6,
            Self::BumpAssetLastTime => 7,
        }
    }

    fn write(&self, out: &mut Vec<u8>) {
        out.push(self.code());
        match self {
            Self::Time(t) => out.extend(t.to_le_bytes()),
            Self::UpdatedBlockTip {
                tip,
                new,
                initial_download,
            } => {
                write_optional_block(out, tip);
                new.write(out);
                out.push((*initial_download).into());
            }
            Self::AcceptedBlockHeader(block) => block.write(out),
            Self::NotifyHeaderTip {
                block,
                initial_download,
            } => {
                write_optional_block(out, block);
                out.push((*initial_download).into());
            }
            Self::Reset {
                force,
                notify,
            } => out.extend([u8::from(*force), u8::from(*notify)]),
            Self::SwitchToNextAsset | Self::BumpAttempt | Self::BumpAssetLastTime => {}
        }
    }
}

/// `CMasternodeSync`'s state after an op, and the notifier calls the op made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncState {
    /// The op's code (0 for the state before the first op).
    pub code: u8,
    pub asset: i32,
    pub asset_name: String,
    pub sync_status: String,
    pub attempt: i32,
    pub asset_start_time: i64,
    pub last_bump: i64,
    pub reached_best_header: bool,
    pub blockchain_synced: bool,
    pub synced: bool,
    pub resets: u8,
    pub finishes: u8,
}

fn take_sync_state(r: &mut &[u8]) -> Option<SyncState> {
    Some(SyncState {
        code: take_u8(r)?,
        asset: take_le(r)?,
        asset_name: take_string(r)?,
        sync_status: take_string(r)?,
        attempt: take_le(r)?,
        asset_start_time: take_le(r)?,
        last_bump: take_le(r)?,
        reached_best_header: take_flag(r)?,
        blockchain_synced: take_flag(r)?,
        synced: take_flag(r)?,
        resets: take_u8(r)?,
        finishes: take_u8(r)?,
    })
}

/// The states [`Oracle::mnsync_step`] reports: the initial one, then one per op but `Time`.
fn parse_sync_states(mut r: &[u8], ops: &[SyncOp]) -> Option<Vec<SyncState>> {
    let r = &mut r;
    let mut states = vec![take_sync_state(r)?];
    for op in ops.iter().filter(|op| !matches!(op, SyncOp::Time(_))) {
        let state = take_sync_state(r)?;
        if state.code != op.code() {
            return None;
        }
        states.push(state);
    }
    r.is_empty().then_some(states)
}

/// One op of a fulfilled-request script (`oracle_netfulfilled_step`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FulfilledOp {
    Time(i64),
    Add(Service, String),
    /// Reports `HasFulfilledRequest()`.
    Has(Service, String),
    RemoveAll(Service),
    CheckAndRemove,
    DoMaintenance,
}

impl FulfilledOp {
    fn write(&self, out: &mut Vec<u8>) {
        match self {
            Self::Time(t) => {
                out.push(0);
                out.extend(t.to_le_bytes());
            }
            Self::Add(addr, request) => {
                out.push(1);
                addr.encode(out);
                write_bytes(out, request.as_bytes());
            }
            Self::Has(addr, request) => {
                out.push(2);
                addr.encode(out);
                write_bytes(out, request.as_bytes());
            }
            Self::RemoveAll(addr) => {
                out.push(3);
                addr.encode(out);
            }
            Self::CheckAndRemove => out.push(4),
            Self::DoMaintenance => out.push(5),
        }
    }
}

/// What [`Oracle::netfulfilled_step`] reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FulfilledRun {
    /// `HasFulfilledRequest()` of each `Has` op, in order.
    pub has: Vec<bool>,
    /// The store as `netfulfilled.dat` holds it (its body: no magic, no hash).
    pub store: Vec<u8>,
}

/// One op of a meta-cache script (`oracle_mncache_step`). Hashes are in Core's byte order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetaOp {
    /// Reports `CMasternodeMetaInfo` as Core serializes it.
    GetInfo([u8; 32]),
    MixingThreshold([u8; 32], i32),
    AllowMixing([u8; 32]),
    DisallowMixing([u8; 32]),
    ValidForMixing([u8; 32]),
    AddGovernanceVote([u8; 32], [u8; 32]),
    RemoveGovernanceObject([u8; 32]),
    SetLastOutboundAttempt([u8; 32], i64),
    SetLastOutboundSuccess([u8; 32], i64),
    LastOutboundAttempt([u8; 32]),
    LastOutboundSuccess([u8; 32]),
    OutboundFailedTooManyTimes([u8; 32]),
    IsPlatformBanned([u8; 32]),
    ResetPlatformBan([u8; 32], i32),
    /// The inventory hash and a `PlatformBanMessage` in network serialization (a message Core
    /// does not decode whole is an argument error).
    SetPlatformBan([u8; 32], Vec<u8>),
    AlreadyHavePlatformBan([u8; 32]),
    GetPlatformBan([u8; 32]),
    AddUsed([u8; 32]),
    /// At most `i64::MAX`.
    RemoveUsed(u64),
    UsedCount,
    IsUsed([u8; 32]),
}

impl MetaOp {
    fn write(&self, out: &mut Vec<u8>) {
        use MetaOp::*;
        let (code, hash): (u8, Option<&[u8; 32]>) = match self {
            GetInfo(h) => (1, Some(h)),
            MixingThreshold(h, _) => (2, Some(h)),
            AllowMixing(h) => (3, Some(h)),
            DisallowMixing(h) => (4, Some(h)),
            ValidForMixing(h) => (5, Some(h)),
            AddGovernanceVote(h, _) => (6, Some(h)),
            RemoveGovernanceObject(h) => (7, Some(h)),
            SetLastOutboundAttempt(h, _) => (8, Some(h)),
            SetLastOutboundSuccess(h, _) => (9, Some(h)),
            LastOutboundAttempt(h) => (10, Some(h)),
            LastOutboundSuccess(h) => (11, Some(h)),
            OutboundFailedTooManyTimes(h) => (12, Some(h)),
            IsPlatformBanned(h) => (13, Some(h)),
            ResetPlatformBan(h, _) => (14, Some(h)),
            SetPlatformBan(h, _) => (15, Some(h)),
            AlreadyHavePlatformBan(h) => (16, Some(h)),
            GetPlatformBan(h) => (17, Some(h)),
            AddUsed(h) => (18, Some(h)),
            RemoveUsed(_) => (19, None),
            UsedCount => (20, None),
            IsUsed(h) => (21, Some(h)),
        };
        out.push(code);
        if let Some(hash) = hash {
            out.extend(hash);
        }
        match self {
            MixingThreshold(_, v) | ResetPlatformBan(_, v) => out.extend(v.to_le_bytes()),
            AddGovernanceVote(_, obj) => out.extend(obj),
            SetLastOutboundAttempt(_, t) | SetLastOutboundSuccess(_, t) => {
                out.extend(t.to_le_bytes())
            }
            SetPlatformBan(_, message) => write_bytes(out, message),
            RemoveUsed(count) => out.extend(count.to_le_bytes()),
            _ => {}
        }
    }
}

/// What one [`MetaOp`] reports (ops that report nothing are skipped).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetaOutput {
    /// `CMasternodeMetaInfo` in Core's serialization.
    Info(Vec<u8>),
    Flag(bool),
    Time(i64),
    Count(u64),
    /// `GetPlatformBan()`: the `PlatformBanMessage` in Core's serialization, if held.
    PlatformBan(Option<Vec<u8>>),
}

/// What [`Oracle::mncache_step`] reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetaRun {
    pub outputs: Vec<MetaOutput>,
    /// The store as `mncache.dat` holds it (its body: no magic, no hash).
    pub store: Vec<u8>,
}

/// A `CMasternodeMetaInfo` as Core serializes it is not self-delimiting for the shim's reader,
/// so its length is found by walking its fields.
fn take_meta_info(r: &mut &[u8]) -> Option<Vec<u8>> {
    let start = *r;
    take_hash(r)?; // protx
    take_le::<i64>(r)?; // last dsq
    take_le::<i32>(r)?; // mixing tx count
    let votes = read_compact_size(r)?;
    for _ in 0..votes {
        take_hash(r)?;
        take_le::<i32>(r)?;
    }
    take_le::<i32>(r)?; // outbound attempt count
    take_le::<i64>(r)?; // last outbound attempt
    take_le::<i64>(r)?; // last outbound success
    take_flag(r)?; // platform ban
    take_le::<i32>(r)?; // platform ban updated
    Some(start[..start.len() - r.len()].to_vec())
}

/// A `PlatformBanMessage` (protx, height, quorum hash, 96-byte BLS signature).
fn take_platform_ban(r: &mut &[u8]) -> Option<Vec<u8>> {
    Some(take(r, 32 + 4 + 32 + 96)?.to_vec())
}

fn parse_meta_run(mut r: &[u8], ops: &[MetaOp]) -> Option<MetaRun> {
    use MetaOp::*;
    let r = &mut r;
    let mut outputs = Vec::new();
    for op in ops {
        outputs.push(match op {
            GetInfo(_) => MetaOutput::Info(take_meta_info(r)?),
            MixingThreshold(..)
            | ValidForMixing(_)
            | OutboundFailedTooManyTimes(_)
            | IsPlatformBanned(_)
            | ResetPlatformBan(..)
            | SetPlatformBan(..)
            | AlreadyHavePlatformBan(_)
            | IsUsed(_) => MetaOutput::Flag(take_flag(r)?),
            LastOutboundAttempt(_) | LastOutboundSuccess(_) => MetaOutput::Time(take_le(r)?),
            UsedCount => MetaOutput::Count(take_le(r)?),
            GetPlatformBan(_) => MetaOutput::PlatformBan(if take_flag(r)? {
                Some(take_platform_ban(r)?)
            } else {
                None
            }),
            AllowMixing(_)
            | DisallowMixing(_)
            | AddGovernanceVote(..)
            | RemoveGovernanceObject(_)
            | SetLastOutboundAttempt(..)
            | SetLastOutboundSuccess(..)
            | AddUsed(_)
            | RemoveUsed(_) => continue,
        });
    }
    let store = take_vec(r)?;
    r.is_empty().then_some(MetaRun {
        outputs,
        store,
    })
}

/// A flat file Core loads (`oracle_flatdb_load`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlatFile {
    NetFulfilled = 0,
    MnCache = 1,
}

/// `CFlatDB::CoreRead()`'s `ReadResult` (flat-database.h).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadResult {
    Ok = 0,
    FileError = 1,
    HashReadError = 2,
    IncorrectHash = 3,
    IncorrectMagicMessage = 4,
    IncorrectMagicNumber = 5,
    IncorrectFormat = 6,
}

impl ReadResult {
    fn from_u8(v: u8) -> Option<Self> {
        use ReadResult::*;
        [
            Ok,
            FileError,
            HashReadError,
            IncorrectHash,
            IncorrectMagicMessage,
            IncorrectMagicNumber,
            IncorrectFormat,
        ]
        .get(usize::from(v))
        .copied()
    }
}

/// What [`Oracle::flatdb_load`] reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlatLoad {
    pub read: ReadResult,
    /// `LoadCache(true)`: whether the node would start with the file.
    pub loaded: bool,
    /// The store the manager then holds (its body).
    pub store: Vec<u8>,
    /// The file after the manager's destructor (which stores when `loaded`).
    pub file_after: Option<Vec<u8>>,
}

fn parse_flat_load(mut r: &[u8]) -> Option<FlatLoad> {
    let r = &mut r;
    let load = FlatLoad {
        read: ReadResult::from_u8(take_u8(r)?)?,
        loaded: take_flag(r)?,
        store: take_vec(r)?,
        file_after: {
            let exists = take_flag(r)?;
            let bytes = take_vec(r)?;
            if !exists && !bytes.is_empty() {
                return None;
            }
            exists.then_some(bytes)
        },
    };
    r.is_empty().then_some(load)
}

impl Oracle {
    /// Run `ops` through a `CMasternodeSync` built at `start_time` on `chain`: the state before
    /// the first op, then after each op but [`SyncOp::Time`].
    pub fn mnsync_step(
        &self,
        chain: &Chain,
        start_time: i64,
        ops: &[SyncOp],
    ) -> Result<Vec<SyncState>> {
        let mut script = Vec::new();
        ops.iter().for_each(|op| op.write(&mut script));
        let record = self.mnsync_step_raw(chain, start_time, &script)?;
        parse_sync_states(&record, ops).ok_or_else(|| malformed("oracle_mnsync_step"))
    }

    /// [`Oracle::mnsync_step`] on an encoded script: the shim's record.
    pub fn mnsync_step_raw(
        &self,
        chain: &Chain,
        start_time: i64,
        script: &[u8],
    ) -> Result<Vec<u8>> {
        self.on_chain(chain, || {
            self.with_buffer(256 + 64 * script.len(), |out, len| unsafe {
                (self.api.mnsync_step)(start_time, script.as_ptr(), script.len(), out, len)
            })
        })
    }

    /// Run `ops` through a `CNetFulfilledRequestManager` at `start_time` on `chain`.
    pub fn netfulfilled_step(
        &self,
        chain: &Chain,
        start_time: i64,
        ops: &[FulfilledOp],
    ) -> Result<FulfilledRun> {
        let mut script = Vec::new();
        ops.iter().for_each(|op| op.write(&mut script));
        let n_has = ops.iter().filter(|op| matches!(op, FulfilledOp::Has(..))).count();
        self.on_chain(chain, || {
            let record = self.with_buffer(256 + script.len() * 2, |out, len| unsafe {
                (self.api.netfulfilled_step)(start_time, script.as_ptr(), script.len(), out, len)
            })?;
            let r = &mut &record[..];
            let has = (0..n_has).map(|_| take_flag(r)).collect::<Option<Vec<_>>>();
            let store = take_vec(r);
            match (has, store) {
                (Some(has), Some(store)) if r.is_empty() => Ok(FulfilledRun {
                    has,
                    store,
                }),
                _ => Err(malformed("oracle_netfulfilled_step")),
            }
        })
    }

    /// Run `ops` through a `CMasternodeMetaMan` on `chain`.
    pub fn mncache_step(&self, chain: &Chain, ops: &[MetaOp]) -> Result<MetaRun> {
        let mut script = Vec::new();
        ops.iter().for_each(|op| op.write(&mut script));
        self.on_chain(chain, || {
            let record = self.with_buffer(1024 + script.len() * 4, |out, len| unsafe {
                (self.api.mncache_step)(script.as_ptr(), script.len(), out, len)
            })?;
            parse_meta_run(&record, ops).ok_or_else(|| malformed("oracle_mncache_step"))
        })
    }

    /// Core's load of `kind`'s flat file holding `file` (`None`: no file) at `mocktime` on
    /// `chain`, in a private data directory.
    pub fn flatdb_load(
        &self,
        chain: &Chain,
        kind: FlatFile,
        mocktime: i64,
        file: Option<&[u8]>,
    ) -> Result<FlatLoad> {
        let bytes = file.unwrap_or_default();
        self.on_chain(chain, || {
            let record = self.with_buffer(256 + bytes.len() * 2, |out, len| unsafe {
                (self.api.flatdb_load)(
                    kind as c_int,
                    mocktime,
                    bytes.as_ptr(),
                    bytes.len(),
                    c_int::from(file.is_some()),
                    out,
                    len,
                )
            })?;
            parse_flat_load(&record).ok_or_else(|| malformed("oracle_flatdb_load"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_info_walk_stops_at_the_record_end() {
        let mut info = Vec::new();
        info.extend([7u8; 32]);
        info.extend(5i64.to_le_bytes());
        info.extend(2i32.to_le_bytes());
        info.push(1);
        info.extend([9u8; 32]);
        info.extend(3i32.to_le_bytes());
        info.extend(1i32.to_le_bytes());
        info.extend(10i64.to_le_bytes());
        info.extend(11i64.to_le_bytes());
        info.push(1);
        info.extend(42i32.to_le_bytes());
        let mut record = info.clone();
        record.push(0xaa);
        let r = &mut &record[..];
        assert_eq!(take_meta_info(r), Some(info));
        assert_eq!(*r, &[0xaa][..]);
    }

    #[test]
    fn flat_load_refuses_bytes_for_an_absent_file() {
        let mut record = vec![1u8, 0, 0, 0, 1, 0xff];
        assert_eq!(parse_flat_load(&record), None);
        record = vec![1u8, 0, 0, 0, 0];
        assert_eq!(
            parse_flat_load(&record),
            Some(FlatLoad {
                read: ReadResult::FileError,
                loaded: false,
                store: Vec::new(),
                file_after: None,
            })
        );
    }

    #[test]
    fn sync_states_must_follow_the_ops() {
        let state = |code: u8| {
            let mut s = vec![code];
            s.extend(0i32.to_le_bytes());
            s.extend([1, b'a', 1, b'b']);
            s.extend(0i32.to_le_bytes());
            s.extend(1i64.to_le_bytes());
            s.extend(1i64.to_le_bytes());
            s.extend([0, 0, 0, 0, 0]);
            s
        };
        let ops = [SyncOp::Time(5), SyncOp::BumpAttempt];
        let mut record = state(0);
        record.extend(state(6));
        assert_eq!(parse_sync_states(&record, &ops).map(|s| s.len()), Some(2));
        let mut wrong = state(0);
        wrong.extend(state(5));
        assert_eq!(parse_sync_states(&wrong, &ops), None);
    }
}
