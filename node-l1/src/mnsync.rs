//! SP1's masternode sync, fulfilled requests and meta cache against Core's (BL-451, DEC-626):
//! one op script run on dashd-masternode's [`MasternodeSync`], [`NetFulfilledRequestManager`]
//! and [`MasternodeMetaMan`], and on a fresh `CMasternodeSync`, `CNetFulfilledRequestManager`
//! and `CMasternodeMetaMan` (`oracle_mnsync_step`, `oracle_netfulfilled_step`,
//! `oracle_mncache_step`); and one flat file loaded by both (`oracle_flatdb_load`).
//!
//! Compared: the sync state and the notifier's calls after every op; each answer of the other
//! two scripts and the store they leave, in Core's serialization. The flat-file load compares, on
//! the same bytes, `CoreRead`'s verdict class (so the order of the hash, magic and network
//! checks), whether the node starts with the file, the object it then holds (after
//! `CheckAndRemove` for netfulfilled) and the file the manager's store leaves; the last three come
//! from the node's own managers (`load_cache`, then the shutdown's `store`) on the file. rdashd's
//! files carry their own magic messages and version strings (SP1 D3); the comparison reads and
//! writes Core's through dashd-masternode's test-only `core_layout` seam (DEC-626 P2), which runs
//! the production `flatdb` read, recreate, verify and write code with Core's magic and version.
//!
//! Not compared: `ProcessTick` (DEC-626 P4; it needs peers: SP1's transcripts cover it).
//!
//! Each `compare_*` taking bytes is a future libFuzzer target's (BL-451b): its first byte picks
//! the network, the rest the script ([`sync_script`], [`fulfilled_script`], [`meta_script`],
//! [`flat_input`]).

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};

use dash_clock::MockClock;
use dash_evo::types::ProTxHash;
use dash_netaddr::{NetAddr, Network, Service};
use dash_oracle_sys::addrman as oracle_addr;
use dash_oracle_sys::{
    Chain, ErrorKind, FlatFile, FulfilledOp, MetaOp, MetaOutput, Oracle, Pin, ReadResult,
    SyncBlock, SyncOp, SyncState,
};
use dash_p2p_core::PlatformBan;
use dash_params::{ChainOption, ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_types::codec::BaseCodec;
use dashd_masternode::flatdb::core_layout::{core_body, decode_with_magic, encode_with_magic};
use dashd_masternode::flatdb::{self, LoadError};
use dashd_masternode::meta::{MasternodeMetaInfo, MasternodeMetaMan, MasternodeMetaStore};
use dashd_masternode::netfulfilled::{NetFulfilledRequestManager, NetFulfilledStore};
use dashd_masternode::sync::{BlockRef, MasternodeSync, NodeSyncNotifier};

use crate::{Compared, Outcome, check, divergence};

/// The longest script the byte forms make.
pub const MAX_OPS: usize = 64;

/// The networks the first input byte picks from (its low two bits).
pub const NETWORKS: [NetworkKind; 4] =
    [NetworkKind::Mainnet, NetworkKind::Testnet, NetworkKind::Regtest, NetworkKind::Devnet];

const DEVNET_NAME: &str = "bl451";

/// The oracle's chain and the node's parameters for a network selector.
pub fn network(selector: u8) -> &'static (Chain, ChainParams) {
    static PARAMS: [OnceLock<(Chain, ChainParams)>; 4] = [const { OnceLock::new() }; 4];
    let i = usize::from(selector & 3);
    PARAMS[i].get_or_init(|| {
        let kind = NETWORKS[i];
        let (chain, builder) = match kind {
            NetworkKind::Mainnet => (Chain::main(), ChainParamsBuilder::new(kind, Release::V24_0)),
            NetworkKind::Testnet => (Chain::test(), ChainParamsBuilder::new(kind, Release::V24_0)),
            NetworkKind::Regtest => {
                (Chain::new("regtest", &[]), ChainParamsBuilder::new(kind, Release::V24_0))
            }
            NetworkKind::Devnet => (
                Chain::devnet(DEVNET_NAME),
                ChainParamsBuilder::new(kind, Release::V24_0).arg(ChainOption::Devnet, DEVNET_NAME),
            ),
        };
        #[allow(clippy::expect_used)]
        let params = builder.build().expect("V24_0 parameters");
        (chain, params)
    })
}

/// The oracle's answer: `Ok(None)` when it refuses the input as outside its domain.
fn oracle_answer<T>(result: dash_oracle_sys::Result<T>) -> Result<Option<T>, crate::Divergence> {
    match result {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.kind == ErrorKind::Argument => Ok(None),
        Err(_) => Err(divergence("oracle error")),
    }
}

/* ---- the input bytes ---- */

/// The input as a stream of bytes; past its end every read is 0.
struct Input<'a>(&'a [u8]);

impl Input<'_> {
    fn u8(&mut self) -> u8 {
        match self.0.split_first() {
            Some((&b, rest)) => {
                self.0 = rest;
                b
            }
            None => 0,
        }
    }

    fn u16(&mut self) -> u16 {
        u16::from_le_bytes([self.u8(), self.u8()])
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// One of `pool`.
    fn pick<T: Copy>(&mut self, pool: &[T]) -> T {
        pool[usize::from(self.u8()) % pool.len()]
    }
}

/// The steps a script's clock takes: around the sync's 6, 30 and 900 s constants and an hour.
const TIME_STEPS: [i64; 12] = [0, 1, 5, 6, 29, 30, 31, 899, 900, 901, 3600, -1];

/// The script's next time: a step from `now`, kept in the oracle's 1..2^33.
fn next_time(input: &mut Input<'_>, now: i64) -> i64 {
    let step = match input.u8() % 4 {
        0 => i64::from(input.u16()),
        _ => input.pick(&TIME_STEPS),
    };
    (now + step).clamp(1, 1 << 33)
}

/// Four hashes, so the script's ops meet.
fn hash(input: &mut Input<'_>) -> [u8; 32] {
    [input.u8() % 4 + 1; 32]
}

/* ---- the masternode sync ---- */

#[derive(Default)]
struct Calls {
    resets: AtomicU8,
    finishes: AtomicU8,
}

struct Counting(Arc<Calls>);

impl NodeSyncNotifier for Counting {
    fn sync_reset(&self) {
        self.0.resets.fetch_add(1, Ordering::SeqCst);
    }

    fn sync_finished(&self) {
        self.0.finishes.fetch_add(1, Ordering::SeqCst);
    }
}

/// An op script for [`compare_mnsync_script`]: the start time, then the ops. The start's top bit
/// picks one in 1..900, within `MASTERNODE_SYNC_RESET_SECONDS` of the sync's initial tip-update
/// time 0.
pub fn sync_script(bytes: &[u8]) -> (i64, Vec<SyncOp>) {
    let mut input = Input(bytes);
    let raw = input.u16();
    let mut now = if raw & 0x8000 != 0 {
        1 + i64::from(raw % 899)
    } else {
        1_000_000 + i64::from(raw)
    };
    let start = now;
    let mut ops = Vec::new();
    let block = |input: &mut Input<'_>| SyncBlock {
        height: i32::from(input.u8()),
        hash: hash(input),
    };
    while !input.is_empty() && ops.len() < MAX_OPS {
        let flags = input.u8();
        ops.push(match input.u8() % 8 {
            0 => {
                now = next_time(&mut input, now);
                SyncOp::Time(now)
            }
            1 => SyncOp::UpdatedBlockTip {
                tip: (flags & 1 == 0).then(|| block(&mut input)),
                new: block(&mut input),
                initial_download: flags & 2 != 0,
            },
            2 => SyncOp::AcceptedBlockHeader(block(&mut input)),
            3 => SyncOp::NotifyHeaderTip {
                block: (flags & 1 == 0).then(|| block(&mut input)),
                initial_download: flags & 2 != 0,
            },
            4 => SyncOp::Reset {
                force: flags & 1 != 0,
                notify: flags & 2 != 0,
            },
            5 => SyncOp::SwitchToNextAsset,
            6 => SyncOp::BumpAttempt,
            _ => SyncOp::BumpAssetLastTime,
        });
    }
    (start, ops)
}

fn block_ref(block: &SyncBlock) -> BlockRef {
    BlockRef {
        height: block.height.unsigned_abs(),
        hash: block.hash,
    }
}

fn sync_state(code: u8, sync: &MasternodeSync, calls: &Calls) -> SyncState {
    SyncState {
        code,
        asset: sync.asset_id(),
        asset_name: sync.asset_name().to_owned(),
        sync_status: sync.sync_status().to_owned(),
        attempt: sync.attempt(),
        asset_start_time: sync.asset_start_time(),
        last_bump: sync.last_bump(),
        reached_best_header: sync.is_reached_best_header(),
        blockchain_synced: sync.is_blockchain_synced(),
        synced: sync.is_synced(),
        resets: calls.resets.swap(0, Ordering::SeqCst),
        finishes: calls.finishes.swap(0, Ordering::SeqCst),
    }
}

fn compare_sync_states(core: &SyncState, ours: &SyncState) -> Result<(), crate::Divergence> {
    check(core.asset == ours.asset, "asset")?;
    check(core.asset_name == ours.asset_name, "asset name")?;
    check(core.sync_status == ours.sync_status, "sync status")?;
    check(core.attempt == ours.attempt, "attempt")?;
    check(core.asset_start_time == ours.asset_start_time, "asset start time")?;
    check(core.last_bump == ours.last_bump, "last bump")?;
    check(core.reached_best_header == ours.reached_best_header, "reached best header")?;
    check(core.blockchain_synced == ours.blockchain_synced, "blockchain synced")?;
    check(core.synced == ours.synced, "synced")?;
    check(core.resets == ours.resets, "sync reset calls")?;
    check(core.finishes == ours.finishes, "sync finished calls")
}

/// `ops` on a sync built at `start` on the selected network.
pub fn compare_mnsync_script(selector: u8, start: i64, ops: &[SyncOp], pin: Pin) -> Compared {
    let (chain, _) = network(selector);
    let Some(core) = oracle_answer(Oracle::new(pin).mnsync_step(chain, start, ops))? else {
        return Ok(Outcome::OutOfDomain);
    };
    let clock = Arc::new(MockClock::at(start));
    let calls = Arc::new(Calls::default());
    let sync = MasternodeSync::new(clock.clone(), Box::new(Counting(calls.clone())));
    let mut ours = vec![sync_state(0, &sync, &calls)];
    for op in ops {
        match op {
            SyncOp::Time(t) => {
                clock.set_mocktime(*t);
                continue;
            }
            SyncOp::UpdatedBlockTip {
                tip,
                new,
                initial_download,
            } => sync.updated_block_tip(
                tip.as_ref().map(block_ref),
                block_ref(new),
                *initial_download,
            ),
            SyncOp::AcceptedBlockHeader(block) => sync.accepted_block_header(block_ref(block)),
            SyncOp::NotifyHeaderTip {
                block,
                initial_download,
            } => sync.notify_header_tip(block.as_ref().map(block_ref), *initial_download),
            SyncOp::Reset {
                force,
                notify,
            } => sync.reset(*force, *notify),
            SyncOp::SwitchToNextAsset => sync.switch_to_next_asset(),
            SyncOp::BumpAttempt => sync.bump_attempt(),
            SyncOp::BumpAssetLastTime => sync.bump_asset_last_time("oracle"),
        }
        ours.push(sync_state(op.code(), &sync, &calls));
    }
    check(core.len() == ours.len(), "sync records")?;
    for (core, ours) in core.iter().zip(&ours) {
        compare_sync_states(core, ours)?;
    }
    Ok(Outcome::BothAccept)
}

/// `diff_mnsync` (BL-451b): [`sync_script`] of `bytes[1..]` on the network `bytes[0]` picks.
pub fn compare_mnsync(bytes: &[u8], pin: Pin) -> Compared {
    let Some((&selector, rest)) = bytes.split_first() else {
        return Ok(Outcome::OutOfDomain);
    };
    let (start, ops) = sync_script(rest);
    compare_mnsync_script(selector, start, &ops, pin)
}

/* ---- the fulfilled requests ---- */

/// BIP155's network id for Tor v3: the oracle's peers are ADDRv2 (IPv4's and IPv6's ids are
/// BIP155's and Core's alike).
const BIP155_TORV3: u8 = 4;

/// Eight peers: two IPv4 addresses, one of them on two ports; two IPv6 ones, `::1` sorting before
/// `::ffff:10.0.0.1` in v1 bytes but after every IPv4 address in Core's `(network, bytes)` order;
/// and three Tor v3 ones, two on one port, which the store's v1 serialization re-keys to the same
/// `[::]:9999` (the first stays on reload).
fn peer(input: &mut Input<'_>) -> (Service, oracle_addr::Service) {
    const IPV6_DOC: [u8; 16] = [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
    const LOOPBACK: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
    let (net, bytes, port): (Network, Vec<u8>, u16) = match input.u8() % 8 {
        0 => (Network::Ipv4, vec![10, 0, 0, 1], 9999),
        1 => (Network::Ipv4, vec![10, 0, 0, 1], 19999),
        2 => (Network::Ipv4, vec![192, 168, 1, 7], 9999),
        3 => (Network::Ipv6, IPV6_DOC.to_vec(), 9999),
        4 => (Network::Ipv6, LOOPBACK.to_vec(), 9999),
        5 => (Network::Onion, vec![1; 32], 9999),
        6 => (Network::Onion, vec![2; 32], 9999),
        _ => (Network::Onion, vec![1; 32], 9998),
    };
    #[allow(clippy::expect_used)]
    let ours = Service::new(NetAddr::new(net, bytes.clone()).expect("a valid address"), port);
    let network = match net {
        Network::Ipv4 => oracle_addr::NET_IPV4,
        Network::Ipv6 => oracle_addr::NET_IPV6,
        _ => BIP155_TORV3,
    };
    (
        ours,
        oracle_addr::Service::new(
            oracle_addr::NetAddr {
                network,
                bytes,
            },
            port,
        ),
    )
}

/// Core's request names, and one of the sync manager's.
const REQUESTS: [&str; 4] = ["full-sync", "mempool-sync", "governance-sync", "allow-sync-1000"];

/// An op script for [`compare_netfulfilled_script`] whose times step around the network's
/// `nFulfilledRequestExpireTime` (`expire`).
pub fn fulfilled_script(bytes: &[u8], expire: i64) -> (i64, Vec<(FulfilledOp, Service)>) {
    let mut input = Input(bytes);
    let mut now = 1_000_000 + i64::from(input.u16());
    let start = now;
    let mut ops = Vec::new();
    while !input.is_empty() && ops.len() < MAX_OPS {
        let (ours, core) = peer(&mut input);
        let request = input.pick(&REQUESTS).to_owned();
        let op = match input.u8() % 6 {
            0 => {
                now = match input.u8() % 2 {
                    0 => next_time(&mut input, now),
                    _ => (now + expire + i64::from(input.u8() % 3) - 1).clamp(1, 1 << 33),
                };
                FulfilledOp::Time(now)
            }
            1 => FulfilledOp::Add(core, request),
            2 => FulfilledOp::Has(core, request),
            3 => FulfilledOp::RemoveAll(core),
            4 => FulfilledOp::CheckAndRemove,
            _ => FulfilledOp::DoMaintenance,
        };
        ops.push((op, ours));
    }
    (start, ops)
}

/// `ops` (each with the node's form of its peer) on a manager at `start`.
pub fn compare_netfulfilled_script(
    selector: u8,
    start: i64,
    ops: &[(FulfilledOp, Service)],
    pin: Pin,
) -> Compared {
    let (chain, params) = network(selector);
    let script: Vec<FulfilledOp> = ops.iter().map(|(op, _)| op.clone()).collect();
    let Some(core) = oracle_answer(Oracle::new(pin).netfulfilled_step(chain, start, &script))?
    else {
        return Ok(Outcome::OutOfDomain);
    };
    let clock = Arc::new(MockClock::at(start));
    let manager = NetFulfilledRequestManager::new(
        clock.clone(),
        i64::from(params.fulfilled_request_expire_time),
    );
    let mut has = Vec::new();
    for (op, peer) in ops {
        match op {
            FulfilledOp::Time(t) => clock.set_mocktime(*t),
            FulfilledOp::Add(_, request) => manager.add_fulfilled_request(peer, request),
            FulfilledOp::Has(_, request) => has.push(manager.has_fulfilled_request(peer, request)),
            FulfilledOp::RemoveAll(_) => manager.remove_all_fulfilled_requests(peer),
            FulfilledOp::CheckAndRemove => manager.check_and_remove(),
            FulfilledOp::DoMaintenance => manager.do_maintenance(false),
        }
    }
    check(core.has == has, "has fulfilled request")?;
    check(core.store == core_body(&manager.snapshot()), "netfulfilled store")?;
    Ok(Outcome::BothAccept)
}

/// `diff_netfulfilled` (BL-451b).
pub fn compare_netfulfilled(bytes: &[u8], pin: Pin) -> Compared {
    let Some((&selector, rest)) = bytes.split_first() else {
        return Ok(Outcome::OutOfDomain);
    };
    let expire = i64::from(network(selector).1.fulfilled_request_expire_time);
    let (start, ops) = fulfilled_script(rest, expire);
    compare_netfulfilled_script(selector, start, &ops, pin)
}

/* ---- the meta cache ---- */

/// A `PlatformBanMessage` for `protx` at `height`, signed by the null signature (all zeros, which
/// Core's BLS wrapper reads as the null object: the only signature both sides read from any
/// bytes; the meta cache checks none).
pub fn platform_ban_message(protx: [u8; 32], height: i32, quorum: [u8; 32]) -> Vec<u8> {
    let mut m = Vec::with_capacity(164);
    m.extend(protx);
    m.extend(height.to_le_bytes());
    m.extend(quorum);
    m.extend([0u8; 96]);
    m
}

/// Heights around 0 and the ends of `int`.
const HEIGHTS: [i32; 8] = [0, 1, 2, 100, 101, -1, i32::MAX, i32::MIN];

/// An op script for [`compare_mncache_script`].
pub fn meta_script(bytes: &[u8]) -> Vec<MetaOp> {
    let mut input = Input(bytes);
    let mut ops = Vec::new();
    while !input.is_empty() && ops.len() < MAX_OPS {
        let h = hash(&mut input);
        ops.push(match input.u8() % 21 {
            0 => MetaOp::GetInfo(h),
            1 => MetaOp::MixingThreshold(h, i32::from(input.u8() % 24)),
            2 => MetaOp::AllowMixing(h),
            3 => MetaOp::DisallowMixing(h),
            4 => MetaOp::ValidForMixing(h),
            5 => MetaOp::AddGovernanceVote(h, hash(&mut input)),
            6 => MetaOp::RemoveGovernanceObject(h),
            7 => MetaOp::SetLastOutboundAttempt(h, i64::from(input.u16())),
            8 => MetaOp::SetLastOutboundSuccess(h, i64::from(input.u16())),
            9 => MetaOp::LastOutboundAttempt(h),
            10 => MetaOp::LastOutboundSuccess(h),
            11 => MetaOp::OutboundFailedTooManyTimes(h),
            12 => MetaOp::IsPlatformBanned(h),
            13 => MetaOp::ResetPlatformBan(h, input.pick(&HEIGHTS)),
            14 => {
                let protx = hash(&mut input);
                let height = input.pick(&HEIGHTS);
                MetaOp::SetPlatformBan(h, platform_ban_message(protx, height, hash(&mut input)))
            }
            15 => MetaOp::AlreadyHavePlatformBan(h),
            16 => MetaOp::GetPlatformBan(h),
            17 => MetaOp::AddUsed(h),
            18 => MetaOp::RemoveUsed(u64::from(input.u8() % 6)),
            19 => MetaOp::UsedCount,
            _ => MetaOp::IsUsed(h),
        });
    }
    ops
}

/// `info` as Core serializes it: dashd-masternode's write, cut from a store holding only it.
fn info_bytes(info: &MasternodeMetaInfo) -> Vec<u8> {
    let store = MasternodeMetaStore {
        infos: BTreeMap::from([(info.protx_hash, info.clone())]),
        dsq_count: 0,
        used_masternodes: VecDeque::new(),
    };
    let body = core_body(&store);
    // The version string, the count (1), the info, nDsqCount and an empty used list.
    let head = 1 + "CMasternodeMetaMan-Version-5".len() + 1;
    body.get(head..body.len().saturating_sub(9)).unwrap_or_default().to_vec()
}

fn wire<T: BaseCodec>(value: &T) -> Vec<u8> {
    let mut out = Vec::new();
    value.encode(&mut out);
    out
}

/// `ops` on a fresh meta cache.
pub fn compare_mncache_script(selector: u8, ops: &[MetaOp], pin: Pin) -> Compared {
    let (chain, _) = network(selector);
    let Some(core) = oracle_answer(Oracle::new(pin).mncache_step(chain, ops))? else {
        return Ok(Outcome::OutOfDomain);
    };
    let meta = MasternodeMetaMan::new();
    let p = |h: &[u8; 32]| ProTxHash::from(*h);
    let mut ours = Vec::new();
    for op in ops {
        let out = match op {
            MetaOp::GetInfo(h) => MetaOutput::Info(info_bytes(&meta.info(&p(h)))),
            MetaOp::MixingThreshold(h, n) => {
                MetaOutput::Flag(meta.is_mixing_threshold_exceeded(&p(h), *n))
            }
            MetaOp::AllowMixing(h) => {
                meta.allow_mixing(&p(h));
                continue;
            }
            MetaOp::DisallowMixing(h) => {
                meta.disallow_mixing(&p(h));
                continue;
            }
            MetaOp::ValidForMixing(h) => MetaOutput::Flag(meta.is_valid_for_mixing_txes(&p(h))),
            MetaOp::AddGovernanceVote(h, object) => {
                meta.add_governance_vote(&p(h), *object);
                continue;
            }
            MetaOp::RemoveGovernanceObject(object) => {
                meta.remove_governance_object(object);
                continue;
            }
            MetaOp::SetLastOutboundAttempt(h, t) => {
                meta.set_last_outbound_attempt(&p(h), *t);
                continue;
            }
            MetaOp::SetLastOutboundSuccess(h, t) => {
                meta.set_last_outbound_success(&p(h), *t);
                continue;
            }
            MetaOp::LastOutboundAttempt(h) => MetaOutput::Time(meta.last_outbound_attempt(&p(h))),
            MetaOp::LastOutboundSuccess(h) => MetaOutput::Time(meta.last_outbound_success(&p(h))),
            MetaOp::OutboundFailedTooManyTimes(h) => {
                MetaOutput::Flag(meta.outbound_failed_too_many_times(&p(h)))
            }
            MetaOp::IsPlatformBanned(h) => MetaOutput::Flag(meta.is_platform_banned(&p(h))),
            MetaOp::ResetPlatformBan(h, height) => {
                MetaOutput::Flag(meta.reset_platform_ban(&p(h), *height))
            }
            MetaOp::SetPlatformBan(inv, message) => {
                // AM1 decodes the message; the cache takes its fields.
                let Ok(msg) = PlatformBan::decode(&mut &message[..]) else {
                    return Err(divergence("platform ban decode"));
                };
                let (Some(protx), Some(height)) = (message.get(..32), message.get(32..36)) else {
                    return Err(divergence("platform ban decode"));
                };
                #[allow(clippy::expect_used)]
                let protx: [u8; 32] = protx.try_into().expect("32 bytes");
                #[allow(clippy::expect_used)]
                let height = i32::from_le_bytes(height.try_into().expect("4 bytes"));
                MetaOutput::Flag(meta.set_platform_ban(*inv, &p(&protx), height, msg))
            }
            MetaOp::AlreadyHavePlatformBan(inv) => {
                MetaOutput::Flag(meta.already_have_platform_ban(inv))
            }
            MetaOp::GetPlatformBan(inv) => {
                MetaOutput::PlatformBan(meta.platform_ban(inv).map(|m| wire(&m)))
            }
            MetaOp::AddUsed(h) => {
                meta.add_used_masternode(&p(h));
                continue;
            }
            MetaOp::RemoveUsed(n) => {
                meta.remove_used_masternodes(usize::try_from(*n).unwrap_or(usize::MAX));
                continue;
            }
            MetaOp::UsedCount => MetaOutput::Count(meta.used_masternodes_count() as u64),
            MetaOp::IsUsed(h) => MetaOutput::Flag(meta.is_used_masternode(&p(h))),
        };
        ours.push(out);
    }
    check(core.outputs.len() == ours.len(), "mncache outputs")?;
    for (core, ours) in core.outputs.iter().zip(&ours) {
        let field = match core {
            MetaOutput::Info(_) => "meta info",
            MetaOutput::Flag(_) => "meta flag",
            MetaOutput::Time(_) => "outbound time",
            MetaOutput::Count(_) => "used count",
            MetaOutput::PlatformBan(_) => "platform ban",
        };
        check(core == ours, field)?;
    }
    check(core.store == core_body(&meta.snapshot()), "mncache store")?;
    Ok(Outcome::BothAccept)
}

/// `diff_mncache` (BL-451b).
pub fn compare_mncache(bytes: &[u8], pin: Pin) -> Compared {
    let Some((&selector, rest)) = bytes.split_first() else {
        return Ok(Outcome::OutOfDomain);
    };
    compare_mncache_script(selector, &meta_script(rest), pin)
}

/* ---- the flat files ---- */

/// One load for [`compare_flatdb_load`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlatInput {
    pub selector: u8,
    pub kind: FlatFile,
    pub mocktime: i64,
    /// The file's bytes; `None`: there is none.
    pub file: Option<Vec<u8>>,
}

/// `CoreRead`'s result for the node's verdict (`Io` cannot happen on bytes).
fn read_result(verdict: &Result<(), LoadError>) -> Option<ReadResult> {
    Some(match verdict {
        Ok(()) => ReadResult::Ok,
        Err(LoadError::Missing) => ReadResult::FileError,
        Err(LoadError::Short) => ReadResult::HashReadError,
        Err(LoadError::Corrupt) => ReadResult::IncorrectHash,
        Err(LoadError::Magic) => ReadResult::IncorrectMagicMessage,
        Err(LoadError::Network) => ReadResult::IncorrectMagicNumber,
        Err(LoadError::Format(_)) => ReadResult::IncorrectFormat,
        Err(LoadError::Io(_)) => return None,
    })
}

/// A netfulfilled store from the input: up to four peers with up to four requests each.
fn fulfilled_store(input: &mut Input<'_>, now: i64) -> NetFulfilledStore {
    let mut store = NetFulfilledStore::default();
    for _ in 0..input.u8() % 5 {
        let (peer, _) = peer(input);
        let entries = store.requests.entry(peer).or_default();
        for _ in 0..input.u8() % 5 {
            let request = input.pick(&REQUESTS).as_bytes().to_vec();
            entries.insert(request, now + i64::from(input.u8()) - 128);
        }
    }
    store
}

/// A meta store from the input.
fn meta_store(input: &mut Input<'_>) -> MasternodeMetaStore {
    let mut store = MasternodeMetaStore {
        dsq_count: i64::from(input.u16()),
        ..MasternodeMetaStore::default()
    };
    for _ in 0..input.u8() % 4 {
        let protx = ProTxHash::from(hash(input));
        let mut info = MasternodeMetaInfo {
            protx_hash: protx,
            last_dsq: i64::from(input.u8()),
            mixing_tx_count: i32::from(input.u8() % 8),
            outbound_attempt_count: i32::from(input.u8() % 8),
            last_outbound_attempt: i64::from(input.u16()),
            last_outbound_success: i64::from(input.u16()),
            platform_ban: input.u8() & 1 != 0,
            platform_ban_updated: input.pick(&HEIGHTS),
            ..MasternodeMetaInfo::default()
        };
        for _ in 0..input.u8() % 3 {
            info.governance_objects_voted_on.insert(hash(input), i32::from(input.u8() % 4));
        }
        store.infos.insert(protx, info);
    }
    for _ in 0..input.u8() % 4 {
        store.used_masternodes.push_back(ProTxHash::from(hash(input)));
    }
    store
}

/// Re-hash `file` (its last 32 bytes) after an edit.
fn rehash(file: &mut Vec<u8>) {
    let n = file.len().saturating_sub(32);
    file.truncate(n);
    let hash = dash_consensus::hash::sha256d(file);
    file.extend_from_slice(&hash);
}

/// A load for [`compare_flatdb_load`]: no file, the input's bytes as the file, or a file Core's
/// layout holds for a generated store, edited (a body byte, the body cut short, the magic message,
/// the message start or both, each re-hashed; or a byte without re-hashing).
pub fn flat_input(bytes: &[u8]) -> FlatInput {
    let mut input = Input(bytes);
    let selector = input.u8();
    let mode = input.u8();
    let kind = if mode & 1 == 0 {
        FlatFile::NetFulfilled
    } else {
        FlatFile::MnCache
    };
    let mocktime = 1_000_000 + i64::from(input.u16());
    let message_start = network(selector).1.message_start;
    let file = match (mode >> 1) % 4 {
        0 => None,
        1 => Some(input.0.to_vec()),
        _ => {
            let mut file = match kind {
                FlatFile::NetFulfilled => {
                    encode_with_magic(&fulfilled_store(&mut input, mocktime), message_start)
                }
                FlatFile::MnCache => encode_with_magic(&meta_store(&mut input), message_start),
            };
            let magic_len = 1 + usize::from(file[0]);
            let body = magic_len + 4..file.len() - 32;
            let at = |input: &mut Input<'_>, range: std::ops::Range<usize>| {
                range.start + usize::from(input.u16()) % range.len().max(1)
            };
            match input.u8() % 8 {
                0 => {}
                1 if !body.is_empty() => {
                    let i = at(&mut input, body.clone());
                    file[i] ^= input.u8().max(1);
                    rehash(&mut file);
                }
                2 => {
                    let cut = at(&mut input, body.clone());
                    file.drain(cut..body.end);
                    rehash(&mut file);
                }
                3 => {
                    let i = at(&mut input, 1..magic_len);
                    file[i] ^= 0x20;
                    rehash(&mut file);
                }
                4 => {
                    let i = at(&mut input, magic_len..magic_len + 4);
                    file[i] ^= 1;
                    rehash(&mut file);
                }
                5 => {
                    let i = at(&mut input, 0..file.len());
                    file[i] ^= input.u8().max(1);
                }
                // Both headers wrong: Core reports the magic message, which it checks first.
                6 => {
                    let i = at(&mut input, 1..magic_len);
                    file[i] ^= 0x20;
                    file[magic_len] ^= 1;
                    rehash(&mut file);
                }
                _ => file.truncate(usize::from(input.u8()) % 40),
            }
            Some(file)
        }
    };
    FlatInput {
        selector,
        kind,
        mocktime,
        file,
    }
}

/// The node's start-up on `load`'s file in Core's layout (`core_layout`'s seams): its own
/// manager's `load_cache(true)` (for netfulfilled, its `CheckAndRemove` at the load's time), then,
/// when that left the manager valid, the store its shutdown runs. Returns whether it starts, the
/// store it then holds, in Core's serialization, and the file left (`None`: there is none).
fn node_start(load: &FlatInput, params: &ChainParams) -> Option<(bool, Vec<u8>, Option<Vec<u8>>)> {
    let dir = tempfile::tempdir().ok()?;
    let ms = params.message_start;
    let path = dir.path().join(match load.kind {
        FlatFile::NetFulfilled => <NetFulfilledStore as flatdb::FlatObject>::FILE,
        FlatFile::MnCache => <MasternodeMetaStore as flatdb::FlatObject>::FILE,
    });
    if let Some(bytes) = &load.file {
        std::fs::write(&path, bytes).ok()?;
    }
    let (loaded, store) = match load.kind {
        FlatFile::NetFulfilled => {
            let manager = NetFulfilledRequestManager::new(
                Arc::new(MockClock::at(load.mocktime)),
                i64::from(params.fulfilled_request_expire_time),
            );
            let loaded = manager.load_cache_core_layout(&path, ms, true);
            // The shutdown stores a valid manager (dashd-node's steps, Core's destructor).
            if manager.is_valid() {
                manager.store_core_layout(&path, ms);
            }
            (loaded, core_body(&manager.snapshot()))
        }
        FlatFile::MnCache => {
            let manager = MasternodeMetaMan::new();
            let loaded = manager.load_cache_core_layout(&path, ms, true);
            // The shutdown stores a valid manager (dashd-node's steps, Core's destructor).
            if manager.is_valid() {
                manager.store_core_layout(&path, ms);
            }
            (loaded, core_body(&manager.snapshot()))
        }
    };
    Some((loaded, store, std::fs::read(&path).ok()))
}

/// One flat-file load by Core and by the node, on the same bytes.
pub fn compare_flatdb_load(load: &FlatInput, pin: Pin) -> Compared {
    let (chain, params) = network(load.selector);
    let file = load.file.as_deref();
    let Some(core) =
        oracle_answer(Oracle::new(pin).flatdb_load(chain, load.kind, load.mocktime, file))?
    else {
        return Ok(Outcome::OutOfDomain);
    };
    // `CoreRead`'s verdict on the bytes, from the decoder the node's load runs.
    let ms = params.message_start;
    let verdict = match (load.kind, file) {
        (_, None) => Err(LoadError::Missing),
        (FlatFile::NetFulfilled, Some(b)) => {
            decode_with_magic::<NetFulfilledStore>(b, ms).map(drop)
        }
        (FlatFile::MnCache, Some(b)) => decode_with_magic::<MasternodeMetaStore>(b, ms).map(drop),
    };
    let Some((loaded, store, after)) = node_start(load, params) else {
        return Err(divergence("the node's start-up could not run"));
    };
    check(read_result(&verdict) == Some(core.read), "read result")?;
    check(loaded == core.loaded, "load verdict")?;
    check(store == core.store, "loaded store")?;
    check(after == core.file_after, "file after store")?;
    Ok(if loaded {
        Outcome::BothAccept
    } else {
        Outcome::BothReject
    })
}

/// `diff_flatdb_load` (BL-451b).
pub fn compare_flatdb(bytes: &[u8], pin: Pin) -> Compared {
    compare_flatdb_load(&flat_input(bytes), pin)
}
