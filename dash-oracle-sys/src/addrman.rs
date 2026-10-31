//! Core's address manager (ABI 10): `AddrMan` handles driven by operations, its bucket
//! math and netgroups, and the draw log of the RNG seam (`src/test/oracle/oracle.h`,
//! "address manager"; `src/random.h` `OracleRngHook`).
//!
//! Addresses travel in Core's BIP155 serialization ([`NetAddr`], [`Service`],
//! [`Address`]). Network filters take Core's `Network` enum values ([`NET_IPV4`] ...),
//! which are not the BIP155 network ids [`NetAddr::network`] holds.

use super::*;

use std::ptr::NonNull;

/// `Network::NET_UNROUTABLE` .. `NET_INTERNAL` (`src/netaddress.h`), for operation filters.
pub const NET_UNROUTABLE: u8 = 0;
pub const NET_IPV4: u8 = 1;
pub const NET_IPV6: u8 = 2;
pub const NET_ONION: u8 = 3;
pub const NET_I2P: u8 = 4;
pub const NET_CJDNS: u8 = 5;
pub const NET_INTERNAL: u8 = 6;

/// A `CNetAddr` as BIP155 serializes it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NetAddr {
    /// The BIP155 network id (1 IPv4, 2 IPv6, 4 TorV3, 5 I2P, 6 CJDNS).
    pub network: u8,
    pub bytes: Vec<u8>,
}

impl NetAddr {
    pub fn ipv4(octets: [u8; 4]) -> Self {
        Self {
            network: 1,
            bytes: octets.to_vec(),
        }
    }

    pub fn ipv6(octets: [u8; 16]) -> Self {
        Self {
            network: 2,
            bytes: octets.to_vec(),
        }
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.network);
        write_compact_size(out, self.bytes.len() as u64);
        out.extend(&self.bytes);
    }

    pub(crate) fn decode(r: &mut &[u8]) -> Option<Self> {
        let network = take_u8(r)?;
        let len = read_compact_size(r)?;
        Some(Self {
            network,
            bytes: take(r, usize::try_from(len).ok()?)?.to_vec(),
        })
    }
}

/// A `CService`: an address and a port.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Service {
    pub addr: NetAddr,
    pub port: u16,
}

impl Service {
    pub fn new(addr: NetAddr, port: u16) -> Self {
        Self {
            addr,
            port,
        }
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        self.addr.encode(out);
        out.extend(self.port.to_be_bytes());
    }

    pub(crate) fn decode(r: &mut &[u8]) -> Option<Self> {
        let addr = NetAddr::decode(r)?;
        let port = u16::from_be_bytes(take(r, 2)?.try_into().ok()?);
        Some(Self {
            addr,
            port,
        })
    }
}

/// A `CAddress` as the `addrv2` message carries it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Address {
    pub time: u32,
    pub services: u64,
    pub service: Service,
}

impl Address {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend(self.time.to_le_bytes());
        write_compact_size(out, self.services);
        self.service.encode(out);
    }

    pub(crate) fn decode(r: &mut &[u8]) -> Option<Self> {
        Some(Self {
            time: take_le(r)?,
            services: read_compact_size(r)?,
            service: Service::decode(r)?,
        })
    }
}

/// One record of the draw log (oracle.h "Draw log").
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Draw {
    /// `GetRandBytes` output.
    RandBytes(Vec<u8>),
    /// `GetStrongRandBytes` output.
    StrongRandBytes(Vec<u8>),
    /// A `FastRandomContext` was constructed (`deterministic`: it will not seed itself).
    NewContext {
        ctx: u32,
        deterministic: bool,
    },
    /// Keystream bytes a context produced.
    Keystream {
        ctx: u32,
        bytes: Vec<u8>,
    },
}

impl Draw {
    pub(crate) fn decode_log(r: &mut &[u8]) -> Option<Vec<Draw>> {
        let n = read_compact_size(r)?;
        let mut draws = Vec::new();
        for _ in 0..n {
            let kind = take_u8(r)?;
            let ctx: u32 = take_le(r)?;
            let len = usize::try_from(read_compact_size(r)?).ok()?;
            let bytes = take(r, len)?.to_vec();
            draws.push(match kind {
                0 => Draw::RandBytes(bytes),
                1 => Draw::StrongRandBytes(bytes),
                2 | 3 => Draw::NewContext {
                    ctx,
                    deterministic: kind == 2,
                },
                4 => Draw::Keystream {
                    ctx,
                    bytes,
                },
                _ => return None,
            });
        }
        Some(draws)
    }
}

/// An `oracle_addrman_op` operation (oracle.h opcodes). `network` filters are `NET_*`
/// values, `None` for any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AddrOp {
    Add {
        addrs: Vec<Address>,
        source: NetAddr,
        time_penalty: i64,
    },
    Good(Service),
    Attempt {
        addr: Service,
        count_failure: bool,
    },
    Connected(Service),
    SetServices {
        addr: Service,
        services: u64,
    },
    ResolveCollisions,
    Select {
        new_only: bool,
        network: Option<u8>,
    },
    SelectTriedCollision,
    GetAddr {
        max_addresses: u64,
        max_pct: u64,
        network: Option<u8>,
        filtered: bool,
    },
    Size {
        network: Option<u8>,
        in_new: Option<bool>,
    },
    /// The peers.dat `DumpPeerAddresses` writes.
    Dump,
    /// Load a peers.dat as `LoadAddrman` does.
    Load(Vec<u8>),
}

impl AddrOp {
    /// The operation's bytes (opcode and operands).
    pub fn encode(&self) -> Vec<u8> {
        let net = |n: &Option<u8>| n.unwrap_or(0xff);
        let mut out = Vec::new();
        match self {
            AddrOp::Add {
                addrs,
                source,
                time_penalty,
            } => {
                out.push(0);
                write_compact_size(&mut out, addrs.len() as u64);
                addrs.iter().for_each(|a| a.encode(&mut out));
                source.encode(&mut out);
                out.extend(time_penalty.to_le_bytes());
            }
            AddrOp::Good(addr) => {
                out.push(1);
                addr.encode(&mut out);
            }
            AddrOp::Attempt {
                addr,
                count_failure,
            } => {
                out.push(2);
                addr.encode(&mut out);
                out.push(u8::from(*count_failure));
            }
            AddrOp::Connected(addr) => {
                out.push(3);
                addr.encode(&mut out);
            }
            AddrOp::SetServices {
                addr,
                services,
            } => {
                out.push(4);
                addr.encode(&mut out);
                out.extend(services.to_le_bytes());
            }
            AddrOp::ResolveCollisions => out.push(5),
            AddrOp::Select {
                new_only,
                network,
            } => out.extend([6, u8::from(*new_only), net(network)]),
            AddrOp::SelectTriedCollision => out.push(7),
            AddrOp::GetAddr {
                max_addresses,
                max_pct,
                network,
                filtered,
            } => {
                out.push(8);
                out.extend(max_addresses.to_le_bytes());
                out.extend(max_pct.to_le_bytes());
                out.extend([net(network), u8::from(*filtered)]);
            }
            AddrOp::Size {
                network,
                in_new,
            } => out.extend([9, net(network), in_new.map_or(0xff, u8::from)]),
            AddrOp::Dump => out.push(10),
            AddrOp::Load(file) => {
                out.push(11);
                write_compact_size(&mut out, file.len() as u64);
                out.extend(file);
            }
        }
        out
    }
}

/// How `LoadAddrman` treats a peers.dat (`ORACLE_ADDRMAN_LOAD_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadOutcome {
    Ok,
    /// `DbInconsistentError`: silently replaced by an empty addrman.
    Reset,
    /// `InvalidAddrManVersionError`: renamed to .bak, empty addrman.
    Backup,
    /// Any other exception: startup fails.
    Fatal,
}

/// What an operation returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpResult {
    /// `Add`, `Good`: whether Core added the address / moved it to tried.
    Bool(bool),
    /// `Attempt`, `Connected`, `SetServices`, `ResolveCollisions`.
    Done,
    /// `Select`, `SelectTriedCollision`: the address (all zero when none) and its last try.
    Selected {
        addr: Address,
        last_try: i64,
    },
    /// `GetAddr`.
    Addrs(Vec<Address>),
    /// `Size`.
    Size(u64),
    /// `Dump`: the peers.dat bytes.
    File(Vec<u8>),
    /// `Load`.
    Loaded {
        outcome: LoadOutcome,
        message: String,
    },
}

impl OpResult {
    fn decode(op: &AddrOp, r: &mut &[u8]) -> Option<Self> {
        Some(match op {
            AddrOp::Add {
                ..
            }
            | AddrOp::Good(_) => OpResult::Bool(take_bool(r)?),
            AddrOp::Attempt {
                ..
            }
            | AddrOp::Connected(_)
            | AddrOp::SetServices {
                ..
            }
            | AddrOp::ResolveCollisions => OpResult::Done,
            AddrOp::Select {
                ..
            }
            | AddrOp::SelectTriedCollision => OpResult::Selected {
                addr: Address::decode(r)?,
                last_try: take_le(r)?,
            },
            AddrOp::GetAddr {
                ..
            } => {
                let n = read_compact_size(r)?;
                OpResult::Addrs((0..n).map(|_| Address::decode(r)).collect::<Option<_>>()?)
            }
            AddrOp::Size {
                ..
            } => OpResult::Size(take_le(r)?),
            AddrOp::Dump => OpResult::File(take_vec(r)?),
            AddrOp::Load(_) => {
                let outcome = match take_u8(r)? {
                    0 => LoadOutcome::Ok,
                    1 => LoadOutcome::Reset,
                    2 => LoadOutcome::Backup,
                    3 => LoadOutcome::Fatal,
                    _ => return None,
                };
                OpResult::Loaded {
                    outcome,
                    message: String::from_utf8_lossy(&take_vec(r)?).into_owned(),
                }
            }
        })
    }
}

/// An operation's result and the draws Core's RNG made while running it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpOutput {
    pub result: OpResult,
    pub draws: Vec<Draw>,
}

/// One `mapInfo` entry of [`AddrManState`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddrEntry {
    pub id: i32,
    pub addr: Address,
    pub source: NetAddr,
    pub last_try: i64,
    pub last_count_attempt: i64,
    pub last_success: i64,
    pub attempts: i32,
    pub ref_count: i32,
    pub in_tried: bool,
    pub random_pos: i32,
}

/// An occupied table cell: bucket, position, entry id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub bucket: u16,
    pub position: u8,
    pub id: i32,
}

/// `oracle_addrman_canonical`: an addrman's full state, ids as Core assigned them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddrManState {
    pub key: [u8; 32],
    pub id_count: i32,
    pub n_new: i32,
    pub n_tried: i32,
    pub last_good: i64,
    pub entries: Vec<AddrEntry>,
    pub random: Vec<i32>,
    pub tried: Vec<Cell>,
    pub new: Vec<Cell>,
    pub collisions: Vec<i32>,
    /// (network, new, tried) for networks with a nonzero count.
    pub network_counts: Vec<(u8, u64, u64)>,
    /// Keystream bytes the addrman's `insecure_rand` has produced.
    pub rng_keystream_bytes: u64,
    /// Bits left in its bit buffer, and those bits (zero when none are left).
    pub rng_bitbuf_size: i32,
    pub rng_bitbuf: u64,
}

impl AddrManState {
    pub fn decode(mut r: &[u8]) -> Option<Self> {
        let r = &mut r;
        let key = take(r, 32)?.try_into().ok()?;
        let (id_count, n_new, n_tried, last_good) =
            (take_le(r)?, take_le(r)?, take_le(r)?, take_le(r)?);
        let entries = take_list(r, |r| {
            Some(AddrEntry {
                id: take_le(r)?,
                addr: Address::decode(r)?,
                source: NetAddr::decode(r)?,
                last_try: take_le(r)?,
                last_count_attempt: take_le(r)?,
                last_success: take_le(r)?,
                attempts: take_le(r)?,
                ref_count: take_le(r)?,
                in_tried: take_bool(r)?,
                random_pos: take_le(r)?,
            })
        })?;
        let random = take_list(r, take_le)?;
        let cell = |r: &mut &[u8]| {
            Some(Cell {
                bucket: take_le(r)?,
                position: take_u8(r)?,
                id: take_le(r)?,
            })
        };
        let tried = take_list(r, cell)?;
        let new = take_list(r, cell)?;
        let collisions = take_list(r, take_le)?;
        let network_counts = take_list(r, |r| Some((take_u8(r)?, take_le(r)?, take_le(r)?)))?;
        let state = Self {
            key,
            id_count,
            n_new,
            n_tried,
            last_good,
            entries,
            random,
            tried,
            new,
            collisions,
            network_counts,
            rng_keystream_bytes: take_le(r)?,
            rng_bitbuf_size: take_le(r)?,
            rng_bitbuf: take_le(r)?,
        };
        r.is_empty().then_some(state)
    }
}

/// How to construct an [`AddrMan`].
#[derive(Clone, Debug)]
pub struct AddrManConfig {
    /// Core's test mode: `nKey = uint256{1}`, zero-key RNG. Otherwise both come from `rng_seed`.
    pub deterministic: bool,
    pub rng_seed: u64,
    /// The asmap file's bytes (empty: no asmap).
    pub asmap: Vec<u8>,
    /// `-checkaddrman` (0..=1000000).
    pub check_ratio: i32,
    /// The chain whose message start `Dump`/`Load` use.
    pub chain: Chain,
}

impl Default for AddrManConfig {
    /// Core's unit tests' addrman: deterministic, no asmap, no consistency checks, regtest.
    fn default() -> Self {
        Self {
            deterministic: true,
            rng_seed: 0,
            asmap: Vec::new(),
            check_ratio: 0,
            chain: Chain::regtest(),
        }
    }
}

/// A Core `AddrMan` (with its `NetGroupManager`), freed on drop. Not `Sync`: one
/// operation at a time.
pub struct AddrMan {
    oracle: Oracle,
    chain: Chain,
    handle: NonNull<ffi::OracleAddrman>,
}

// The shim's handle owns its Core objects and is used by one thread at a time (&mut self).
unsafe impl Send for AddrMan {}

impl Drop for AddrMan {
    // Unlocked: `oracle_addrman_free` deletes the handle's own objects and touches no Core
    // global, so it may run during a peer session.
    fn drop(&mut self) {
        unsafe { (self.oracle.api.addrman_free)(self.handle.as_ptr()) }
    }
}

impl AddrMan {
    /// Run `op` at mock time `mocktime` and return the raw output (result, then draw log).
    pub fn op_raw(&mut self, op: &[u8], mocktime: i64) -> Result<Vec<u8>> {
        self.op_raw_with_capacity(op, mocktime, 4096)
    }

    /// [`AddrMan::op_raw`] with a first buffer of `capacity` bytes. Output that does not fit is
    /// collected by a second call, which the shim answers without running the operation again.
    pub fn op_raw_with_capacity(
        &mut self,
        op: &[u8],
        mocktime: i64,
        capacity: usize,
    ) -> Result<Vec<u8>> {
        let run = || {
            self.oracle.with_buffer(capacity, |out, len| unsafe {
                (self.oracle.api.addrman_op)(
                    self.handle.as_ptr(),
                    op.as_ptr(),
                    op.len(),
                    mocktime,
                    out,
                    len,
                )
            })
        };
        // Dump and Load read the network's message start.
        if matches!(op.first(), Some(10 | 11)) {
            let chain = self.chain.clone();
            self.oracle.on_chain(&chain, run)
        } else {
            self.oracle.shared(run)
        }
    }

    /// Run `op` at mock time `mocktime`.
    pub fn op(&mut self, op: &AddrOp, mocktime: i64) -> Result<OpOutput> {
        let raw = self.op_raw(&op.encode(), mocktime)?;
        let mut r = raw.as_slice();
        let result = OpResult::decode(op, &mut r).expect("the shim's operation result format");
        let draws =
            Draw::decode_log(&mut r).filter(|_| r.is_empty()).expect("the shim's draw log format");
        Ok(OpOutput {
            result,
            draws,
        })
    }

    /// `oracle_addrman_canonical`'s bytes.
    pub fn canonical_raw(&mut self) -> Result<Vec<u8>> {
        self.oracle.shared(|| {
            self.oracle.with_buffer(64 * 1024, |out, len| unsafe {
                (self.oracle.api.addrman_canonical)(self.handle.as_ptr(), out, len)
            })
        })
    }

    /// The full state.
    pub fn canonical(&mut self) -> Result<AddrManState> {
        Ok(AddrManState::decode(&self.canonical_raw()?).expect("the shim's canonical state format"))
    }
}

/// The peers.dat bytes at the front of a raw [`AddrOp::Dump`] output ([`AddrMan::op_raw`]).
pub fn dump_file(raw: &[u8]) -> Option<Vec<u8>> {
    take_vec(&mut &raw[..])
}

/// `AddrInfo(addr, src)`'s buckets and positions under one key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Buckets {
    pub tried_bucket: u32,
    pub new_bucket: u32,
    pub pos_new: u32,
    pub pos_tried: u32,
}

impl Oracle {
    /// A new Core `AddrMan`; `None` if Core's `SanityCheckASMap` refuses the asmap.
    pub fn addrman_new(&self, config: &AddrManConfig) -> Result<Option<AddrMan>> {
        self.shared(|| {
            let mut handle = std::ptr::null_mut();
            let accepted = self.status(unsafe {
                (self.api.addrman_new)(
                    c_int::from(config.deterministic),
                    config.rng_seed,
                    config.asmap.as_ptr(),
                    config.asmap.len(),
                    config.check_ratio,
                    &mut handle,
                )
            })?;
            Ok(accepted.then(|| AddrMan {
                oracle: *self,
                chain: config.chain.clone(),
                handle: NonNull::new(handle).expect("the shim returned a handle"),
            }))
        })
    }

    /// `AddrInfo(addr, src)`'s buckets under `nkey` (uint256 byte order) and `asmap`;
    /// `None` if `SanityCheckASMap` refuses the asmap.
    pub fn addrman_bucket(
        &self,
        nkey: &[u8; 32],
        addr: &Service,
        src: &NetAddr,
        asmap: &[u8],
    ) -> Result<Option<Buckets>> {
        self.shared(|| {
            let (mut a, mut s) = (Vec::new(), Vec::new());
            addr.encode(&mut a);
            src.encode(&mut s);
            let mut b = Buckets::default();
            let ok = self.status(unsafe {
                (self.api.addrman_bucket)(
                    nkey.as_ptr(),
                    a.as_ptr(),
                    a.len(),
                    s.as_ptr(),
                    s.len(),
                    asmap.as_ptr(),
                    asmap.len(),
                    &mut b.tried_bucket,
                    &mut b.new_bucket,
                    &mut b.pos_new,
                    &mut b.pos_tried,
                )
            })?;
            Ok(ok.then_some(b))
        })
    }

    /// `AddrInfo::GetChance(now)` with `nAttempts = attempts` and the last try
    /// `last_try_delta_s` seconds before now.
    pub fn addrman_chance(&self, last_try_delta_s: i64, attempts: i32) -> Result<f64> {
        self.shared(|| {
            let bits = self.with_out(|bits| unsafe {
                (self.api.addrman_chance)(last_try_delta_s, attempts, bits)
            })?;
            Ok(f64::from_bits(bits))
        })
    }

    /// `NetGroupManager(asmap)`'s `GetGroup(addr)` and `GetMappedAS(addr)`; `None` if
    /// `SanityCheckASMap` refuses the asmap. An unroutable address's group reads the
    /// `-allowprivatenet` of the process's latest chain selection: [`Oracle::netgroup_on`] reads
    /// a given chain's.
    pub fn netgroup(&self, addr: &NetAddr, asmap: &[u8]) -> Result<Option<(Vec<u8>, u32)>> {
        self.shared(|| {
            let mut a = Vec::new();
            addr.encode(&mut a);
            let mut asn = 0u32;
            let mut accepted = true;
            let group = self.with_buffer(64, |out, len| {
                let status = unsafe {
                    (self.api.netgroup)(
                        a.as_ptr(),
                        a.len(),
                        asmap.as_ptr(),
                        asmap.len(),
                        out,
                        len,
                        &mut asn,
                    )
                };
                if status == ffi::ORACLE_REJECT {
                    accepted = false;
                    return ffi::ORACLE_OK;
                }
                status
            })?;
            Ok(accepted.then_some((group, asn)))
        })
    }

    /// [`Oracle::netgroup`]'s call, run with `chain` selected, under the exclusive hold every
    /// chain call takes. The group of an unroutable address reads `-allowprivatenet`
    /// (netgroup.cpp:43), which [`Chain`] sets as a node's startup does; plain `netgroup` reads
    /// what the process's latest selection left there, which another thread's chain call can
    /// change between two calls (BL-436).
    pub fn netgroup_on(
        &self,
        chain: &Chain,
        addr: &NetAddr,
        asmap: &[u8],
    ) -> Result<Option<(Vec<u8>, u32)>> {
        self.on_chain(chain, || self.netgroup(addr, asmap))
    }
}

pub(crate) fn take<'a>(r: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
    let (head, rest) = r.split_at_checked(n)?;
    *r = rest;
    Some(head)
}

pub(crate) fn take_u8(r: &mut &[u8]) -> Option<u8> {
    Some(take(r, 1)?[0])
}

/// A little-endian integer.
pub(crate) fn take_le<T: LeInt>(r: &mut &[u8]) -> Option<T> {
    Some(T::from_le(take(r, T::SIZE)?))
}

pub(crate) fn take_bool(r: &mut &[u8]) -> Option<bool> {
    Some(take_u8(r)? != 0)
}

/// A bool as the shim writes it: 0 or 1 only.
pub(crate) fn take_flag(r: &mut &[u8]) -> Option<bool> {
    match take_u8(r)? {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

/// A 0/1 presence byte, then the item when present.
pub(crate) fn take_optional<T>(
    r: &mut &[u8],
    item: impl FnOnce(&mut &[u8]) -> Option<T>,
) -> Option<Option<T>> {
    Some(if take_bool(r)? {
        Some(item(r)?)
    } else {
        None
    })
}

pub(crate) fn take_hash(r: &mut &[u8]) -> Option<[u8; 32]> {
    take(r, 32)?.try_into().ok()
}

pub(crate) fn take_string(r: &mut &[u8]) -> Option<String> {
    String::from_utf8(take_vec(r)?).ok()
}

pub(crate) fn take_vec(r: &mut &[u8]) -> Option<Vec<u8>> {
    let len = usize::try_from(read_compact_size(r)?).ok()?;
    Some(take(r, len)?.to_vec())
}

pub(crate) fn take_list<T>(
    r: &mut &[u8],
    mut item: impl FnMut(&mut &[u8]) -> Option<T>,
) -> Option<Vec<T>> {
    let n = read_compact_size(r)?;
    (0..n).map(|_| item(r)).collect()
}

pub(crate) trait LeInt: Sized {
    const SIZE: usize;
    fn from_le(bytes: &[u8]) -> Self;
}

macro_rules! le_int {
    ($($t:ty),*) => {$(
        impl LeInt for $t {
            const SIZE: usize = size_of::<$t>();
            fn from_le(bytes: &[u8]) -> Self {
                <$t>::from_le_bytes(bytes.try_into().expect("SIZE bytes"))
            }
        }
    )*};
}
le_int!(u16, u32, u64, i32, i64);
