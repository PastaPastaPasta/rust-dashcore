//! Core's own chainstate on regtest (ABI 22, node-program O2b): a `ChainstateManager` with the
//! Dash managers over in-memory LevelDB environments and real block files, driven op by op as
//! dashd's RPC handlers and startup drive it (`src/test/oracle/oracle.h`, "chain sessions").
//!
//! Core keeps the node in process-wide globals, so one session may exist per process, never
//! alongside a peer session or an LLMQ context. [`ChainSession`] holds its pin's lock exclusively
//! for its lifetime, as [`PeerSession`](crate::PeerSession) does: another thread's call of the
//! pin that takes its lock (every call but `abi_version`, `core_version`, `script_error_string`
//! and the frees of images and address managers) waits until the session is dropped, and a call
//! on the session's own thread is refused ([`ErrorKind::SessionLive`]), the codecs included
//! (oracle.h allows those, but this binding does not tell them apart; build a program's blocks
//! before the session starts). A session stays on the thread that started it (it is not
//! `Send`): the lock and the thread's mark are released there. The
//! session forgets the network: the next chain call selects it again. If a teardown fails, Core's
//! state is unknown and every later call of the pin fails with [`ErrorKind::Poisoned`].
//!
//! Each op returns a [`ChainReport`]: Core's four sections as the shim wrote them ([`RawReport`],
//! the bytes a determinism check compares) and decoded ([`Reply`], StateDigest
//! [`Record`](dash_statedigest::Record)s, [`Event`]s, [`Post`]). Hashes in the decoded sections
//! are in Core's internal byte order, as everywhere in this crate (the JSON holds display hex).

use super::*;

use dash_statedigest::{Lines, Record};
use serde::Deserialize;
use serde::de::{self, Deserializer, SeqAccess, Visitor};
use serde_json::Value;
use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::sync::RwLockWriteGuard;

/// How oracle_last_error() starts after a teardown failed (oracle.h).
const TEARDOWN_FAILED: &str = "teardown failed";

/// `oracle_chain_step`'s FLUSH modes (`ORACLE_CHAIN_FLUSH_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FlushMode {
    Always = 0,
    IfNeeded = 1,
    Periodic = 2,
    /// A PERIODIC flush forced to write the block index only (leg B's index-only mirror).
    IndexOnly = 3,
}

/// `oracle_chain_step`'s RESTART kinds (`ORACLE_CHAIN_RESTART_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum RestartKind {
    /// As dashd's `Shutdown()`: its two flushes and the spork store.
    Clean = 0,
    /// As a SIGKILL: storage holds what earlier writes left.
    Kill = 1,
}

/// One op of a chain session (oracle.h `ORACLE_CHAIN_*`). Hashes are in Core's internal byte
/// order. The shim checks what a frame holds (an unknown value, a label that is not printable
/// ASCII, a mock time out of range are [`ErrorKind::Argument`]); [`ChainOp::encode`] writes it
/// as given.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ChainOp {
    /// `submitblock` with a serialized block.
    Block(Vec<u8>),
    /// `submitheader` with an 80-byte header.
    Header([u8; 80]),
    /// A `getblocktemplate` proposal of a serialized block.
    Proposal(Vec<u8>),
    /// `invalidateblock`.
    Invalidate([u8; 32]),
    /// `reconsiderblock`.
    Reconsider {
        hash: [u8; 32],
        ignore_chainlocks: bool,
    },
    /// `preciousblock`.
    Precious([u8; 32]),
    /// `ActivateBestChain()`.
    Activate,
    /// `ProcessNewChainLock(-1, clsig, ...)`: the `ChainLockSig` (height, block hash, BLS
    /// signature). M3 checks no signature.
    ClRecord {
        height: i32,
        hash: [u8; 32],
        sig: [u8; 96],
    },
    /// `CheckActiveState()`, then `EnforceBestChainLock()`.
    ClEnforce,
    /// `CSporkManager::UpdateSpork()` with the session's spork key.
    Spork {
        id: i32,
        value: i64,
    },
    /// `FlushStateToDisk()`; a non-empty label books the flush under that cause.
    Flush {
        mode: FlushMode,
        label: String,
    },
    /// Teardown, then the load.
    Restart(RestartKind),
    /// `SetMockTime()` (1 ..= 2^33).
    SetTime(i64),
}

impl ChainOp {
    /// The op's frame: a tag, then the payload in the wire format.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            ChainOp::Block(block) => {
                out.push(ffi::ORACLE_CHAIN_BLOCK);
                out.extend(block);
            }
            ChainOp::Header(header) => {
                out.push(ffi::ORACLE_CHAIN_HEADER);
                out.extend(header);
            }
            ChainOp::Proposal(block) => {
                out.push(ffi::ORACLE_CHAIN_PROPOSAL);
                out.extend(block);
            }
            ChainOp::Invalidate(hash) => {
                out.push(ffi::ORACLE_CHAIN_INVALIDATE);
                out.extend(hash);
            }
            ChainOp::Reconsider {
                hash,
                ignore_chainlocks,
            } => {
                out.push(ffi::ORACLE_CHAIN_RECONSIDER);
                out.extend(hash);
                out.push(u8::from(*ignore_chainlocks));
            }
            ChainOp::Precious(hash) => {
                out.push(ffi::ORACLE_CHAIN_PRECIOUS);
                out.extend(hash);
            }
            ChainOp::Activate => out.push(ffi::ORACLE_CHAIN_ACTIVATE),
            ChainOp::ClRecord {
                height,
                hash,
                sig,
            } => {
                out.push(ffi::ORACLE_CHAIN_CL_RECORD);
                out.extend(height.to_le_bytes());
                out.extend(hash);
                out.extend(sig);
            }
            ChainOp::ClEnforce => out.push(ffi::ORACLE_CHAIN_CL_ENFORCE),
            ChainOp::Spork {
                id,
                value,
            } => {
                out.push(ffi::ORACLE_CHAIN_SPORK);
                out.extend(id.to_le_bytes());
                out.extend(value.to_le_bytes());
            }
            ChainOp::Flush {
                mode,
                label,
            } => {
                out.push(ffi::ORACLE_CHAIN_FLUSH);
                out.push(*mode as u8);
                write_bytes(&mut out, label.as_bytes());
            }
            ChainOp::Restart(kind) => {
                out.push(ffi::ORACLE_CHAIN_RESTART);
                out.push(*kind as u8);
            }
            ChainOp::SetTime(time) => {
                out.push(ffi::ORACLE_CHAIN_SETTIME);
                out.extend(time.to_le_bytes());
            }
        }
        out
    }
}

/// A session's storage (`oracle_chain_image`): every environment, the block files and
/// `sporks.dat`, copied. It owns its copies and outlives the session that saved it. Dropping it
/// frees the copies (a plain delete that touches no Core state, so it runs on any thread, without
/// the pin's lock, and after the pin is poisoned).
pub struct CoreImage {
    pin: Pin,
    handle: NonNull<ffi::OracleChainImage>,
    tainted: bool,
}

// The image is plain data (maps of byte strings) that no Core global points into: freeing it or
// reading it for a restore on another thread is safe.
unsafe impl Send for CoreImage {}
unsafe impl Sync for CoreImage {}

impl Drop for CoreImage {
    fn drop(&mut self) {
        unsafe { (self.pin.api().chain_image_free)(self.handle.as_ptr()) }
    }
}

impl fmt::Debug for CoreImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CoreImage")
            .field("pin", &self.pin)
            .field("tainted", &self.tainted)
            .finish_non_exhaustive()
    }
}

impl CoreImage {
    /// Whether the session that saved it was tainted ([`ChainSession::tainted_since`]): its
    /// state may hold what a tie-break decided, although nothing in a report from it will say
    /// so (Core reports a tie once, on the op that fired it). A prefix image must not be.
    pub fn tainted(&self) -> bool {
        self.tainted
    }
}

/// How [`Oracle::chain_session`] ended.
#[derive(Debug)]
pub enum ChainStart {
    Started(ChainSession),
    /// Core refused the load (no session was made): the load's message, as RESTART's reply
    /// and [`SaveOutcome::ReloadRefused`] give it. (One of Core's refusals, a sporks cache that
    /// does not load, names the session's random data directory; images Core wrote never reach
    /// it.)
    Refused(String),
}

/// What [`ChainSession::save`] gave.
#[derive(Debug)]
pub enum SaveOutcome {
    /// The image, and the session is loaded again.
    Saved(CoreImage),
    /// The image; Core refused the reload, so the session is down. The load's message
    /// (oracle.h: `oracle_last_error()` after `oracle_chain_save`).
    ReloadRefused(CoreImage, String),
    /// The session was down: nothing was saved.
    Down,
}

/// A live session. Dropping it tears the node down as a SIGKILL would and gives the process's
/// state back (`mem::forget` leaves the pin locked for the rest of the process).
pub struct ChainSession {
    oracle: Oracle,
    handle: NonNull<ffi::OracleChain>,
    // Dropped in this order: the thread's mark first, then the lock.
    _hold: Hold,
    state: RwLockWriteGuard<'static, PinState>,
    /// The reports returned so far.
    reports: u64,
    tainted_since: Option<u64>,
    /// The buffer every report is written into: allocated and zeroed once, not per call (the
    /// bound is megabytes).
    buffer: Vec<u8>,
    // Not Send or Sync, whatever the fields above become: the thread's mark and the lock belong
    // to the thread that started the session.
    _thread: PhantomData<*const ()>,
}

impl fmt::Debug for ChainSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChainSession").field("pin", &self.oracle.pin).finish_non_exhaustive()
    }
}

impl Drop for ChainSession {
    fn drop(&mut self) {
        // Non-OK: the teardown failed (oracle_last_error() starts TEARDOWN_FAILED), and the
        // process's Core state is undefined.
        let status = unsafe { (self.oracle.api.chain_free)(self.handle.as_ptr()) };
        if status != ffi::ORACLE_OK {
            self.state.poisoned = Some(self.oracle.last_error());
        }
    }
}

impl Oracle {
    /// Start a chain session on regtest at `mocktime` (1 ..= 2^33) with Core options `args`
    /// (oracle.h `oracle_chain_new`: `-par=N` with N >= 1 is required; the shim's own
    /// `-oracle-no-size-flush` turns Core's cache-size flush causes off). `image`: the storage
    /// to load, or `None` for genesis.
    ///
    /// Blocks while another thread uses this pin (a peer session, an LLMQ context or a chain
    /// session among them); [`ErrorKind::SessionLive`] if the calling thread holds one.
    /// [`ErrorKind::Argument`] for the options the shim refuses (oracle.h);
    /// [`ErrorKind::Exception`] with Core's message for those Core's own parameter checks refuse
    /// first (`-prune` while `-txindex` or governance is on), or with "teardown failed" if
    /// tearing a failed start down failed (the pin is then poisoned). Only the message tells
    /// such a refusal from a start that threw part-way: the schedules' options are fixed, so
    /// nothing reads it (a typed refusal needs a shim change).
    pub fn chain_session(
        &self,
        args: &[&str],
        mocktime: i64,
        image: Option<&CoreImage>,
    ) -> Result<ChainStart> {
        match self.pin.held() {
            Held::None => {}
            Held::Session => return Err(session_live(self.pin)),
            held => unreachable!("a session start inside another oracle call ({held:?})"),
        }
        if let Some(image) = image {
            self.check_image(image)?;
        }
        let mut state = self.pin.state().write().unwrap_or_else(|e| e.into_inner());
        state.usable()?;
        // The session forgets the network; the next chain call selects it again.
        state.chain = None;
        let hold = Hold::new(self.pin, Held::Session);
        let args = args
            .iter()
            .map(|a| {
                CString::new(*a).map_err(|_| Error {
                    kind: ErrorKind::Argument,
                    message: format!("NUL in argument {a:?}"),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let argv: Vec<*const c_char> = args.iter().map(|a| a.as_ptr()).collect();
        let image_tainted = image.is_some_and(CoreImage::tainted);
        let image = image.map_or(std::ptr::null(), |i| i.handle.as_ptr().cast_const());
        let mut handle = std::ptr::null_mut();
        // Large enough that a refused load's message never needs the retry below (which starts
        // the session again).
        let mut message = vec![0u8; 16 * 1024];
        let mut message_len = message.len();
        let mut new = |message: &mut Vec<u8>, message_len: &mut usize| unsafe {
            (self.api.chain_new)(
                argv.as_ptr(),
                argv.len(),
                mocktime,
                image,
                &mut handle,
                message.as_mut_ptr().cast(),
                message_len,
            )
        };
        let mut status = new(&mut message, &mut message_len);
        if status == ffi::ORACLE_E_BUFFER {
            // A refused load's message did not fit: nothing was made, so start again.
            message.resize(message_len, 0);
            status = new(&mut message, &mut message_len);
        }
        let started = self.status(status).inspect_err(|e| {
            // Tearing a failed start down failed: Core's state is unknown (oracle.h). (A start
            // that threw and was torn down is an Exception too, without the prefix.)
            if e.message.starts_with(TEARDOWN_FAILED) {
                state.poisoned = Some(e.message.clone());
            }
        })?;
        if !started {
            message.truncate(message_len.saturating_sub(1));
            return Ok(ChainStart::Refused(String::from_utf8_lossy(&message).into_owned()));
        }
        Ok(ChainStart::Started(ChainSession {
            oracle: *self,
            handle: NonNull::new(handle).expect("the shim returned a session"),
            _hold: hold,
            state,
            reports: 0,
            // A tainted image's state is tie-decided from the first report.
            tainted_since: image_tainted.then_some(0),
            buffer: Vec::new(),
            _thread: PhantomData,
        }))
    }

    fn check_image(&self, image: &CoreImage) -> Result<()> {
        if image.pin == self.pin {
            return Ok(());
        }
        Err(Error {
            kind: ErrorKind::Argument,
            message: format!("an image of {} given to {}", image.pin, self.pin),
        })
    }
}

impl ChainSession {
    /// Apply `op` and return its report. The op runs once; an [`ErrorKind::Argument`],
    /// [`ErrorKind::Decode`] or [`ErrorKind::Trailing`] frame runs nothing. After an
    /// [`ErrorKind::Exception`] the session is broken (oracle.h: every later call fails so):
    /// drop it.
    pub fn step(&mut self, op: &ChainOp) -> Result<ChainReport> {
        self.step_frame(&op.encode())
    }

    /// [`ChainSession::step`] with a frame as given (for refusal tests and replays).
    pub fn step_frame(&mut self, frame: &[u8]) -> Result<ChainReport> {
        let restart = frame.first() == Some(&ffi::ORACLE_CHAIN_RESTART);
        let (api, handle) = (self.oracle.api, self.handle);
        let report = self.report(restart, |out, len| unsafe {
            (api.chain_step)(handle.as_ptr(), frame.as_ptr(), frame.len(), out, len)
        })?;
        let index = self.counted();
        if report.tainted() {
            self.tainted_since.get_or_insert(index);
        }
        Ok(report)
    }

    /// Since which report (0: the session's first) the session's state may hold what a
    /// tie-break a port cannot reproduce decided (O2b D30), or `None`. Set by the first tainted
    /// report ([`ChainReport::tainted`]) or a start from a tainted image, it stays set through
    /// every later step, restart and save (the state stays tie-decided although the reports
    /// after the one that fired say nothing), and is cleared only by a restore of an untainted
    /// image whose own report is untainted. Nothing from the report it names on is scored.
    pub fn tainted_since(&self) -> Option<u64> {
        self.tainted_since
    }

    /// The index of the report just returned.
    fn counted(&mut self) -> u64 {
        self.reports += 1;
        self.reports - 1
    }

    /// CLEAN teardown, a copy of the storage, and the load again (`oracle_chain_save`).
    pub fn save(&mut self) -> Result<SaveOutcome> {
        let mut image = std::ptr::null_mut();
        let status = unsafe { (self.oracle.api.chain_save)(self.handle.as_ptr(), &mut image) };
        // Owned before the status is read, so that no path leaks an image the shim made.
        let image = NonNull::new(image).map(|handle| CoreImage {
            pin: self.oracle.pin,
            handle,
            tainted: self.tainted_since.is_some(),
        });
        // The refused reload's message, read before any other call can replace it.
        let refused = (status == ffi::ORACLE_REJECT).then(|| self.oracle.last_error());
        let loaded = self.oracle.status(status)?;
        Ok(match (loaded, image) {
            (true, Some(image)) => SaveOutcome::Saved(image),
            (false, Some(image)) => {
                SaveOutcome::ReloadRefused(image, refused.expect("read for ORACLE_REJECT"))
            }
            (false, None) => SaveOutcome::Down,
            (true, None) => unreachable!("oracle_chain_save succeeded without an image"),
        })
    }

    /// KILL teardown (unless down), the storage replaced by copies of `image`'s, then the load:
    /// RESTART's report. Core drops the ties a previous load carried and reports this load's.
    pub fn restore(&mut self, image: &CoreImage) -> Result<ChainReport> {
        self.oracle.check_image(image)?;
        let (api, handle) = (self.oracle.api, self.handle);
        let report = self.report(true, |out, len| unsafe {
            (api.chain_restore)(handle.as_ptr(), image.handle.as_ptr().cast_const(), out, len)
        })?;
        let index = self.counted();
        self.tainted_since = (image.tainted || report.tainted()).then_some(index);
        Ok(report)
    }

    /// A stateful call's decoded report, in a buffer of the session's bound; a report that
    /// outgrew it (ORACLE_E_BUFFER after the call ran) is collected by the same call again
    /// (oracle.h). `restart`: a RESTART or restore, whose POST alone carries `persisted`.
    fn report(
        &mut self,
        restart: bool,
        mut call: impl FnMut(*mut u8, *mut usize) -> c_int,
    ) -> Result<ChainReport> {
        let bound = unsafe { (self.oracle.api.chain_report_bound)(self.handle.as_ptr()) };
        let buffer = &mut self.buffer;
        if buffer.len() < bound {
            buffer.resize(bound, 0);
        }
        let mut len = buffer.len();
        let mut status = call(buffer.as_mut_ptr(), &mut len);
        if status == ffi::ORACLE_E_BUFFER {
            buffer.resize(len, 0);
            status = call(buffer.as_mut_ptr(), &mut len);
        }
        self.oracle.ok(status)?;
        let raw = RawReport::split(&self.buffer[..len]).expect("the shim's report format");
        let report = ChainReport::decode(raw);
        assert_eq!(
            report.post.persisted.is_some(),
            restart,
            "the shim's report format: persisted on a restart or restore only"
        );
        Ok(report)
    }
}

/// A report's four sections as the shim wrote them: canonical JSON, and the StateDigest
/// records as JSON Lines.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RawReport {
    pub reply: Vec<u8>,
    pub records: Vec<u8>,
    pub events: Vec<u8>,
    pub post: Vec<u8>,
}

impl RawReport {
    /// The sections of a report: each a u32 little-endian length, then its bytes.
    pub fn split(mut bytes: &[u8]) -> Option<Self> {
        let mut section = || {
            let (len, rest) = bytes.split_first_chunk::<4>()?;
            let (body, rest) = rest.split_at_checked(u32::from_le_bytes(*len) as usize)?;
            bytes = rest;
            Some(body.to_vec())
        };
        let report = Self {
            reply: section()?,
            records: section()?,
            events: section()?,
            post: section()?,
        };
        bytes.is_empty().then_some(report)
    }

    /// The report with every StateDigest header's `ver` (Core's `FormatFullVersion()`: the
    /// build's provenance, not its behaviour) replaced by `*`, so that reports of two builds of
    /// one Core tree (a development archive and the pinned one) compare equal. Lines that do
    /// not parse are kept as they are (a report from the shim has none).
    pub fn normalized(&self) -> Self {
        let mut records = Vec::with_capacity(self.records.len());
        let mut rest = &self.records[..];
        while !rest.is_empty() {
            let end = rest.iter().position(|&b| b == b'\n').map_or(rest.len(), |i| i + 1);
            let (line, tail) = rest.split_at(end);
            match dash_statedigest::parse_line(line) {
                Ok(Record::Header(mut header)) => {
                    header.ver = b"*".to_vec();
                    Record::Header(header).encode(&mut records);
                }
                _ => records.extend(line),
            }
            rest = tail;
        }
        Self {
            records,
            ..self.clone()
        }
    }
}

/// One op's report, decoded. Every section is decoded strictly: a key the shim does not write,
/// or a value of another shape, fails the decode (the shim's format is this crate's contract).
#[derive(Clone, Debug, PartialEq)]
pub struct ChainReport {
    pub raw: RawReport,
    pub reply: Reply,
    pub records: Vec<Record>,
    pub events: Vec<Event>,
    pub post: Post,
}

impl ChainReport {
    /// Decode the sections; panics on a format this crate does not know (the shim's contract).
    pub fn decode(raw: RawReport) -> Self {
        Self::try_decode(raw).unwrap_or_else(|e| panic!("the shim's report format: {e}"))
    }

    /// Decode the sections.
    pub fn try_decode(raw: RawReport) -> std::result::Result<Self, String> {
        let reply = serde_json::from_slice(&raw.reply).map_err(|e| format!("reply: {e}"))?;
        let records = Lines::new(&raw.records)
            .map(|(line, record)| record.map_err(|e| format!("record line {line}: {e:?}")))
            .collect::<std::result::Result<_, _>>()?;
        let events = serde_json::from_slice(&raw.events).map_err(|e| format!("events: {e}"))?;
        let post: Post = serde_json::from_slice(&raw.post).map_err(|e| format!("post: {e}"))?;
        post.check()?;
        Ok(Self {
            raw,
            reply,
            records,
            events,
            post,
        })
    }

    /// Whether this op fired a tie-break a port cannot reproduce (O2b D30), or carried the ties
    /// of a load that had no report of its own (the session's start, a save's reload). Core
    /// reports each tie once: the reports after this one say nothing although their state is
    /// what the tie decided, so a caller stops at the first tainted report, or reads the
    /// session's latch ([`ChainSession::tainted_since`], [`CoreImage::tainted`]).
    pub fn tainted(&self) -> bool {
        !self.post.tie.is_empty()
    }
}

/// A 32-byte hash, held in Core's internal byte order, written in JSON as display hex.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChainHash(pub [u8; 32]);

impl fmt::Debug for ChainHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().rev().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

impl<'de> Deserialize<'de> for ChainHash {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let hex = String::deserialize(d)?;
        let bytes = hex.as_bytes();
        let nibble = |c: u8| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        };
        if bytes.len() != 64 {
            return Err(de::Error::custom(format!("hash {hex:?}: not 64 hex digits")));
        }
        let mut out = [0u8; 32];
        for (i, pair) in bytes.chunks(2).enumerate() {
            let (Some(hi), Some(lo)) = (nibble(pair[0]), nibble(pair[1])) else {
                return Err(de::Error::custom(format!("hash {hex:?}: not lowercase hex")));
            };
            out[31 - i] = hi << 4 | lo;
        }
        Ok(ChainHash(out))
    }
}

/// `[height, hash]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
pub struct BlockAt(pub i32, pub ChainHash);

/// What `BlockChecked` said of a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckResult {
    Valid,
    Invalid,
    Error,
}

/// A `BlockChecked` signal: `[hash, res, cls, why]`, as O8's `R` records spell them (`cls`
/// and `why` are null for a valid block; `cls` is null for an error).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize)]
pub struct Checked(pub ChainHash, pub CheckResult, pub Option<String>, pub Option<String>);

/// An RPC's error, or a direct call's (`code` null: `session-down`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RpcError {
    #[serde(deserialize_with = "Option::deserialize")]
    pub code: Option<i64>,
    pub message: String,
}

/// The REPLY section: the result or the error, and the `BlockChecked` signals the op caused.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub checked: Vec<Checked>,
    pub outcome: std::result::Result<Value, RpcError>,
}

impl<'de> Deserialize<'de> for Reply {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            checked: Vec<Checked>,
            #[serde(default, deserialize_with = "some")]
            result: Option<Value>,
            #[serde(default, deserialize_with = "some")]
            error: Option<RpcError>,
        }
        let wire = Wire::deserialize(d)?;
        let outcome = match (wire.result, wire.error) {
            (Some(result), None) => Ok(result),
            (None, Some(error)) => Err(error),
            _ => return Err(de::Error::custom("a reply holds exactly one of result and error")),
        };
        Ok(Reply {
            checked: wire.checked,
            outcome,
        })
    }
}

/// A key that may be absent but never holds null: absent is `None`, a value (null included for a
/// [`Value`], as a `submitblock` that accepts replies null) is `Some`. (Serde's own `Option`
/// would read null as absent.)
fn some<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> std::result::Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

/// A validation signal: `[name, args...]`, in the order Core delivered them.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub name: String,
    pub args: Vec<Value>,
}

impl<'de> Deserialize<'de> for Event {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct EventVisitor;
        impl<'de> Visitor<'de> for EventVisitor {
            type Value = Event;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("[name, args...]")
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Event, A::Error> {
                let name =
                    seq.next_element()?.ok_or_else(|| de::Error::invalid_length(0, &self))?;
                let mut args = Vec::new();
                while let Some(arg) = seq.next_element()? {
                    args.push(arg);
                }
                Ok(Event {
                    name,
                    args,
                })
            }
        }
        d.deserialize_seq(EventVisitor)
    }
}

/// Flushes by cause (Core's `FlushStateMode` names, or a FLUSH op's label).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlushCounts {
    pub full: BTreeMap<String, u64>,
    pub index_only: BTreeMap<String, u64>,
}

/// The UTXO set: coinstats' `TxOutSer` digest over the coins sorted by outpoint, their number
/// and their total.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Utxo {
    pub hash: ChainHash,
    pub txouts: u64,
    pub amount: i64,
}

/// What storage held between a teardown and the load, read before any load logic (O2b D37).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Persisted {
    /// The coins database's best block.
    #[serde(deserialize_with = "Option::deserialize")]
    pub best_block: Option<ChainHash>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub evo_best: Option<ChainHash>,
    /// `[hash, failure bits]` of each stored index record with a failure bit, in key order.
    pub failed: Vec<(ChainHash, u32)>,
    /// The coins database's head blocks (an interrupted flush's marker).
    #[serde(deserialize_with = "Option::deserialize")]
    pub head_blocks: Option<Vec<ChainHash>>,
    /// A digest of every stored index record but its file positions.
    pub index: ChainHash,
    pub index_entries: u64,
}

/// `[spork id, value, active]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
pub struct SporkState(pub i32, pub i64, pub bool);

/// The POST section: the state after the op. A down session's holds `down`, `flush_delta`,
/// `tie` and `persisted` only: the other fields are `None` exactly when `down`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Post {
    pub down: bool,
    /// The flushes this op caused.
    pub flush_delta: FlushCounts,
    /// The tie sites (oracle.h; 1-14 at ABI 22) whose order decided an outcome.
    pub tie: Vec<u32>,
    /// RESTART and restore: the storage's state before the load.
    #[serde(default, deserialize_with = "some")]
    pub persisted: Option<Persisted>,
    #[serde(default, deserialize_with = "some")]
    pub tip: Option<BlockAt>,
    #[serde(default, deserialize_with = "nullable")]
    pub best_header: Option<Option<BlockAt>>,
    #[serde(default, deserialize_with = "some")]
    pub utxo: Option<Utxo>,
    /// A digest of every index entry's hash, height, status and whether it has its chain's
    /// transactions.
    #[serde(default, deserialize_with = "some")]
    pub index: Option<ChainHash>,
    /// The flushes since the last load.
    #[serde(default, deserialize_with = "some")]
    pub flushes: Option<FlushCounts>,
    #[serde(default, deserialize_with = "some")]
    pub ibd: Option<bool>,
    #[serde(default, deserialize_with = "nullable")]
    pub best_cl: Option<Option<BlockAt>>,
    #[serde(default, deserialize_with = "some")]
    pub sporks: Option<Vec<SporkState>>,
    /// `getchaintips`, ordered by height (descending), then hash.
    #[serde(default, deserialize_with = "some")]
    pub tips: Option<Vec<Value>>,
    /// `getdeploymentinfo`.
    #[serde(default, deserialize_with = "some")]
    pub deployments: Option<Value>,
}

/// A key that may hold null: absent is `None`, null is `Some(None)`.
fn nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> std::result::Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

impl Post {
    fn check(&self) -> std::result::Result<(), String> {
        let live = [
            self.tip.is_some(),
            self.best_header.is_some(),
            self.utxo.is_some(),
            self.index.is_some(),
            self.flushes.is_some(),
            self.ibd.is_some(),
            self.best_cl.is_some(),
            self.sporks.is_some(),
            self.tips.is_some(),
            self.deployments.is_some(),
        ];
        if live.iter().all(|&present| present != self.down) {
            Ok(())
        } else {
            Err(format!("post: down {} with the live fields {live:?}", self.down))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: &str = "00000000000000000000000000000000000000000000000000000000000000ff";

    fn live_post(change: impl FnOnce(&mut serde_json::Map<String, Value>)) -> String {
        let mut post = serde_json::json!({
            "best_cl": null, "best_header": null, "deployments": {}, "down": false,
            "flush_delta": {"full": {}, "index_only": {}}, "flushes": {"full": {}, "index_only": {}},
            "ibd": true, "index": H, "sporks": [[10018, 0, true]], "tie": [], "tip": [0, H],
            "tips": [], "utxo": {"amount": 0, "hash": H, "txouts": 0},
        });
        change(post.as_object_mut().unwrap());
        post.to_string()
    }

    fn post(json: &str) -> std::result::Result<Post, String> {
        let post: Post = serde_json::from_str(json).map_err(|e| e.to_string())?;
        post.check()?;
        Ok(post)
    }

    fn persisted() -> Value {
        serde_json::json!({
            "best_block": H, "evo_best": null, "failed": [[H, 128]], "head_blocks": null,
            "index": H, "index_entries": 1,
        })
    }

    #[test]
    fn a_post_decodes_absent_and_null_apart() {
        let live = post(&live_post(|_| {})).unwrap();
        assert_eq!(live.tip, Some(BlockAt(0, ChainHash(rpc_order()))));
        assert_eq!(
            (live.best_header, live.best_cl, live.persisted),
            (Some(None), Some(None), None)
        );
        let down = r#"{"down":true,"flush_delta":{"full":{},"index_only":{}},"tie":[1]}"#;
        assert_eq!(post(down).unwrap().tip, None);
        // A live field that is null, or a nullable one that is absent, is not the shim's format.
        assert!(post(&live_post(|p| _ = p.insert("tip".into(), Value::Null))).is_err());
        assert!(post(&live_post(|p| _ = p.remove("best_cl"))).is_err());
        assert!(
            post(r#"{"down":true,"flush_delta":{"full":{},"index_only":{}},"tie":[],"tip":null}"#)
                .is_err()
        );
        assert!(post(&live_post(|p| _ = p.insert("persisted".into(), Value::Null))).is_err());
        assert!(post(&live_post(|p| _ = p.insert("extra".into(), Value::Null))).is_err());
        // persisted: its markers are always written, null when absent.
        let restart = post(&live_post(|p| _ = p.insert("persisted".into(), persisted()))).unwrap();
        let p = restart.persisted.unwrap();
        assert_eq!(
            (p.evo_best, p.head_blocks, p.failed),
            (None, None, vec![(ChainHash(rpc_order()), 128)])
        );
        let mut missing = persisted();
        missing.as_object_mut().unwrap().remove("head_blocks");
        assert!(post(&live_post(|p| _ = p.insert("persisted".into(), missing))).is_err());
    }

    /// `H` in Core's internal byte order.
    fn rpc_order() -> [u8; 32] {
        let mut out = [0; 32];
        out[0] = 0xff;
        out
    }

    #[test]
    fn a_reply_holds_exactly_one_outcome() {
        let reply = |json: &str| serde_json::from_str::<Reply>(json);
        assert_eq!(reply(r#"{"checked":[],"result":null}"#).unwrap().outcome, Ok(Value::Null));
        let down =
            reply(r#"{"checked":[],"error":{"code":null,"message":"session-down"}}"#).unwrap();
        assert_eq!(down.outcome.unwrap_err().code, None);
        for bad in [
            r#"{"checked":[],"result":1,"error":null}"#,
            r#"{"checked":[],"error":{"message":"no code"}}"#,
            r#"{"checked":[]}"#,
            r#"{"checked":[],"result":1,"error":{"code":1,"message":"both"}}"#,
            r#"{"result":null}"#,
        ] {
            assert!(reply(bad).is_err(), "{bad}");
        }
        let checked =
            reply(&format!(r#"{{"checked":[["{H}","invalid","consensus","bad"]],"result":"x"}}"#));
        let checked = checked.unwrap().checked;
        assert_eq!(checked[0].1, CheckResult::Invalid);
        assert!(serde_json::from_str::<ChainHash>(&format!("\"{}\"", H.to_uppercase())).is_err());
        assert!(serde_json::from_str::<ChainHash>("\"ff\"").is_err());
    }

    #[test]
    fn a_report_splits_into_four_sections() {
        let mut bytes = Vec::new();
        for section in [&b"{}"[..], b"", b"[]", b"{}"] {
            bytes.extend((section.len() as u32).to_le_bytes());
            bytes.extend(section);
        }
        let raw = RawReport::split(&bytes).unwrap();
        assert_eq!(
            (&raw.reply[..], &raw.records[..], &raw.events[..]),
            (&b"{}"[..], &b""[..], &b"[]"[..])
        );
        assert_eq!(RawReport::split(&bytes[..bytes.len() - 1]), None, "short");
        assert_eq!(RawReport::split(&[bytes.clone(), vec![0]].concat()), None, "trailing");
    }
}
