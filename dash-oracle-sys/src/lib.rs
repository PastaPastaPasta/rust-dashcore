//! In-process Dash Core consensus oracle.
//!
//! Links `libdash_oracle`, a C ABI over Dash Core's own consensus functions built
//! from the pinned Core releases (`pins.txt`, PLAN DEC-05) by
//! `contrib/node/build-oracle.sh`, and wraps it in a safe API. Every pin is
//! linked into the same binary; pick one with [`Oracle::new`].
//!
//! This crate exists for differential tests and differential fuzzing (A1 §4.4).
//! It must never be a runtime dependency of the node. Everything is behind the
//! `oracle` feature.
//!
//! Byte conventions follow Core: transactions, blocks and headers are network
//! serializations, and 32-byte hashes are in Core's internal byte order (the
//! order `uint256` stores, which is also the order dashcore's hash types store).

#![cfg(feature = "oracle")]

pub mod addrman;
pub mod chain;
pub mod coinjoin;
pub mod coins_ops;
pub mod context;
pub mod ffi;
pub mod gov;
pub mod llmq;
pub mod mnhf;
pub mod mnlist;
pub mod netinfo;
pub mod p2p;
pub mod payments;
pub mod peer_session;
pub mod versionbits;

pub use addrman::{
    AddrEntry, AddrMan, AddrManConfig, AddrManState, AddrOp, Address, Buckets, Cell, Draw,
    LoadOutcome, NetAddr, OpOutput, OpResult, Service,
};
pub use chain::{
    BlockAt, ChainHash, ChainOp, ChainReport, ChainSession, ChainStart, CheckResult, Checked,
    CoreImage, Event, FlushCounts, FlushMode, Persisted, Post, RawReport, Reply, RestartKind,
    RpcError, SaveOutcome, SporkState, Utxo,
};
pub use coinjoin::{
    CjCollateral, CjDstxStructure, CjEntryKind, CjHashes, CjInOuts, CjMessage, CjRule, CjView,
};
pub use coins_ops::{CacheEntry, CoinsOp, CoinsOutcome, CoinsOutput, DbState};
pub use context::{
    BlockContext, BlsDecoded, BlsKind, BlsMode, BlsRecord, Checkpoints, CoinsView, Context,
    ContextCoin, DEPLOYMENT_EVO_SHARES, DEPLOYMENT_V24, DecodedPayload, DmlApply, DmlBlock,
    DmlEffects, DmlStage, MinedCommitment, MnList, QuorumMembers, RelicCall, SequenceLocks,
    ThresholdState, TxInputs, UnlockIndexes,
};
pub use gov::{
    Collateral, Expiry, GovContext, GovExceptionType, GovObjectCheck, GovOp, GovOutput, GovPayment,
    GovVoteCheck, ObjectOutcome, ObjectState, Outpoint, ParsedTrigger, PaymentsScript,
    SuperblockOutcome, Tally, VoteInstance, VoteOutcome,
};
pub use llmq::{
    GetQuorumOutcome, LLMQ_FIXTURE_VERSION, LlmqContext, LlmqFixture, LlmqWindow, LockPass,
    LockStatus, LockVerdict, Member, MembersOutcome, MinedVerdict, PassStatus, Precompute,
    SNAPSHOT_KEY_LEN, ScanCache, ScanDemand, TraceEvent,
};
pub use mnhf::{MnhfConnected, MnhfContext, MnhfStep};
pub use mnlist::{
    AppliedList, CanonicalList, CoreOrder, ListDiff, ListEncoding, ListOp, StateDiff, UniqueHash,
    UniqueProperty,
};
pub use netinfo::{NetInfoAdd, NetInfoKind, NetInfoStatus};
pub use p2p::{P2P_COMMANDS, P2pDecoded, P2pStream, P2pVerdict, QDataRead};
pub use payments::{
    BlockValuePayee, CreditPool, CreditPoolStep, GovVote, PaymentsContext, StepContext,
    SuperblockCheck, Trigger, TxOut,
};
pub use peer_session::{
    BlockRef, Call, ConnType, NodeStateData, PeerData, PeerOutcome, PeerSession, SendMode,
    SessionConfig, SessionGlobals, SessionOutcome, Substitution, TICK_INACTIVITY, TICK_MAINTENANCE,
    Transport,
};
pub use versionbits::{SignalChange, VbAnswer, VbQuery, VbStats};

use std::cell::Cell as ThreadCell;
use std::ffi::{CStr, CString};
use std::fmt;
use std::os::raw::{c_char, c_int};
use std::sync::RwLock;

/// Whether every linked archive is the one `pins.txt` records. `build.rs` refuses
/// any other archive unless `DASH_ORACLE_ALLOW_UNPINNED=1` (development builds), and
/// then this is false. [`Oracle::new`] also refuses to run such a build unless the
/// same variable is set when it runs, so no test can stand for the pinned Core
/// releases by accident.
pub const ARCHIVES_PINNED: bool = !cfg!(dash_oracle_unpinned);

/// A pinned Dash Core release linked into this binary. One today (DEC-82: v24 only); a re-pin
/// (SW-V24) adds its row to `pins.txt`, not a variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Pin {
    /// v24.0.0-rc.4, the parity reference.
    V24_0,
}

impl Pin {
    pub const ALL: [Pin; 1] = [Pin::V24_0];

    /// The pin's name in `pins.txt`.
    pub fn name(self) -> &'static str {
        match self {
            Pin::V24_0 => "v24.0",
        }
    }

    /// sha256 of the archive `build.rs` linked for this pin (after checking it
    /// against pins.txt), or None in a build that links no archive.
    pub fn archive_sha256(self) -> Option<&'static str> {
        match self {
            Pin::V24_0 => option_env!("DASH_ORACLE_SHA256_V24_0"),
        }
    }

    fn api(self) -> &'static ffi::Api {
        match self {
            Pin::V24_0 => &ffi::v24_0::API,
        }
    }

    /// The lock over the pin's Core globals (oracle.h): calls that select a chain, peer
    /// sessions and chain sessions hold it exclusively, every other call shared.
    fn state(self) -> &'static RwLock<PinState> {
        static V24_0: RwLock<PinState> = RwLock::new(PinState {
            chain: None,
            poisoned: None,
        });
        match self {
            Pin::V24_0 => &V24_0,
        }
    }

    /// What the calling thread holds of [`Pin::state`].
    fn held(self) -> Held {
        HELD.with(|held| held.get()[self as usize])
    }
}

/// The state of a pin's Core globals.
pub(crate) struct PinState {
    /// The chain they are set to.
    chain: Option<Chain>,
    /// Why they can no longer be used: a peer or chain session's teardown failed part-way,
    /// leaving Core's state unknown (oracle.h, oracle_peer_session_free, oracle_chain_free).
    poisoned: Option<String>,
}

impl PinState {
    fn usable(&self) -> Result<()> {
        match &self.poisoned {
            None => Ok(()),
            Some(why) => Err(Error {
                kind: ErrorKind::Poisoned,
                message: format!("{why}; Core's state is unknown"),
            }),
        }
    }
}

/// What a thread holds of a pin's lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Held {
    None,
    Shared,
    Exclusive,
    /// A live peer session's, chain session's or LLMQ context's: the thread may make no other
    /// call of the pin (oracle.h's rule for peer sessions and LLMQ contexts; this crate's for
    /// chain sessions, whose codecs oracle.h allows).
    Session,
}

thread_local! {
    static HELD: ThreadCell<[Held; Pin::ALL.len()]> = const { ThreadCell::new([Held::None; Pin::ALL.len()]) };
}

/// Marks the calling thread as holding `held` of a pin's lock until dropped.
pub(crate) struct Hold {
    pin: Pin,
    previous: Held,
}

impl Hold {
    fn new(pin: Pin, held: Held) -> Self {
        let previous = HELD.with(|cell| {
            let mut all = cell.get();
            let previous = std::mem::replace(&mut all[pin as usize], held);
            cell.set(all);
            previous
        });
        Self {
            pin,
            previous,
        }
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        HELD.with(|cell| {
            let mut all = cell.get();
            all[self.pin as usize] = self.previous;
            cell.set(all);
        });
    }
}

fn session_live(pin: Pin) -> Error {
    Error {
        kind: ErrorKind::SessionLive,
        message: format!(
            "this thread holds a {pin} peer session, chain session or LLMQ context: no other oracle call may run"
        ),
    }
}

impl fmt::Display for Pin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A chain selection: Core's chain name plus extra `-option=value` arguments
/// (e.g. `-devnet=<name>`, `-highsubsidyblocks=<n>`). Besides Core's chain-parameter options,
/// `-allowprivatenet` (ABI 16) sets the flag `CService::IsRoutable()` reads, as a node's startup
/// does; without it the flag is Core's default (off). An LLMQ context registers the
/// chain-parameter options alone, so [`Oracle::llmq_context`] refuses a chain carrying it
/// ([`ErrorKind::Network`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chain {
    pub name: String,
    pub args: Vec<String>,
}

impl Chain {
    pub fn new(name: &str, args: &[&str]) -> Self {
        Self {
            name: name.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
        }
    }
    pub fn main() -> Self {
        Self::new("main", &[])
    }
    pub fn test() -> Self {
        Self::new("test", &[])
    }
    pub fn regtest() -> Self {
        Self::new("regtest", &[])
    }
    pub fn devnet(name: &str) -> Self {
        Self::new("devnet", &[&format!("-devnet={name}")])
    }
}

/// Why the oracle could not produce a verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// Core's deserializer refused the bytes (it threw `std::ios_base::failure`).
    Decode,
    /// Another exception escaped the Core function (`std::bad_alloc` among them): no verdict.
    Exception,
    /// An argument Core would assert on, or a malformed call.
    Argument,
    /// The chain could not be selected.
    Network,
    /// An object decoded but left unread bytes.
    Trailing,
    /// The pin's Core release does not have the function (the v24 pin has them all).
    Unsupported,
    /// The call lacks context Core would read (a short header window, an absent
    /// masternode list, missing quorum members; oracle.h lists each entry point's
    /// cases). Not a Core verdict: supply the missing context.
    Context,
    /// An unexpected status code (including a buffer error after a retry).
    Status(i32),
    /// Refused: the calling thread holds a peer session, chain session or LLMQ context of the
    /// pin, and this crate allows no other call while one lives. (Other threads' calls wait
    /// until it is freed.)
    SessionLive,
    /// Refused: a peer or chain session's teardown failed, so the pin's Core state is unknown.
    /// Every later call of the pin fails this way.
    Poisoned,
    /// Refused: an earlier call on this LLMQ context failed with [`ErrorKind::Context`] (Core
    /// may have cached what it computed without the missing state). Drop the context.
    ContextPoisoned,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    /// The shim's message, e.g. Core's exception text.
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// A rejection's result class, tagged with the Core enum it belongs to, so a
/// transaction class can never be compared with a block class by accident.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResultCode {
    /// `TxValidationResult` as an integer.
    Tx(i32),
    /// `BlockValidationResult` as an integer.
    Block(i32),
    /// The Core function reports only a bool (`CFinalCommitment::Verify()`).
    Bool,
}

/// A `CValidationState` rejection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rejection {
    pub result: ResultCode,
    pub reason: String,
    pub debug_message: String,
    /// Whether Core's reason was longer than the shim's buffer (`reason` is then a prefix).
    pub reason_truncated: bool,
    /// Whether Core's debug message was longer than the shim's buffer.
    pub debug_truncated: bool,
}

/// Core's verdict on a transaction or block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Accept,
    Reject(Rejection),
}

impl Verdict {
    pub fn is_accept(&self) -> bool {
        matches!(self, Verdict::Accept)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedTx {
    /// Bytes read from the input.
    pub consumed: usize,
    pub reencoded: Vec<u8>,
    pub txid: [u8; 32],
    /// `GetInstanceHash()` (the txid on releases without it).
    pub instance_hash: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedBlock {
    pub consumed: usize,
    pub reencoded: Vec<u8>,
    pub hash: [u8; 32],
    /// `BlockMerkleRoot()` over the decoded transactions.
    pub merkle_root: [u8; 32],
    pub mutated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedHeader {
    pub consumed: usize,
    pub reencoded: [u8; 80],
    pub hash: [u8; 32],
}

/// `arith_uint256::SetCompact()` output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Compact {
    pub target: [u8; 32],
    pub negative: bool,
    pub overflow: bool,
}

/// Result of `VerifyScript()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScriptResult {
    pub ok: bool,
    /// `ScriptError` as an integer (see [`Oracle::script_error_string`]).
    pub error: i32,
}

/// The masternode reward era (`MnRewardEra`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RewardEra {
    Classic = 0,
    CreditPool = 1,
    EvoReward = 2,
}

/// Result of `EvalScript()`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvalResult {
    pub ok: bool,
    /// `ScriptError` as an integer.
    pub error: i32,
    /// The stack `EvalScript()` left (also on failure).
    pub stack: Vec<Vec<u8>>,
}

/// A decoded `CScriptNum`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScriptNum {
    /// `GetInt64()`
    pub value: i64,
    /// `getint()` (clamped to `i32`)
    pub getint: i32,
}

/// An output a transaction input spends, for the coins-view entry points.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpentOutput<'a> {
    pub amount: i64,
    pub script_pubkey: &'a [u8],
}

/// `GetLegacySigOpCount()`, `GetP2SHSigOpCount()` and `GetTransactionSigOpCount()`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TxSigops {
    pub legacy: u32,
    pub p2sh: u32,
    pub total: u32,
}

pub(crate) fn write_compact_size(out: &mut Vec<u8>, n: u64) {
    match n {
        0..=0xfc => out.push(n as u8),
        0xfd..=0xffff => {
            out.push(0xfd);
            out.extend((n as u16).to_le_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(0xfe);
            out.extend((n as u32).to_le_bytes());
        }
        _ => {
            out.push(0xff);
            out.extend(n.to_le_bytes());
        }
    }
}

/// A CompactSize-prefixed byte string.
pub(crate) fn write_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    write_compact_size(out, bytes.len() as u64);
    out.extend(bytes);
}

pub(crate) fn read_compact_size(bytes: &mut &[u8]) -> Option<u64> {
    let (&first, rest) = bytes.split_first()?;
    let width = match first {
        0xfd => 2,
        0xfe => 4,
        0xff => 8,
        n => {
            *bytes = rest;
            return Some(n.into());
        }
    };
    let (n, rest) = rest.split_at_checked(width)?;
    *bytes = rest;
    Some(n.iter().rev().fold(0, |acc, &b| (acc << 8) | u64::from(b)))
}

/// A `std::vector<std::vector<unsigned char>>` as Core serializes it.
fn serialize_stack(stack: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    write_compact_size(&mut out, stack.len() as u64);
    for item in stack {
        write_compact_size(&mut out, item.len() as u64);
        out.extend(item);
    }
    out
}

fn deserialize_stack(mut bytes: &[u8]) -> Option<Vec<Vec<u8>>> {
    let n = read_compact_size(&mut bytes)?;
    let mut stack = Vec::new();
    for _ in 0..n {
        let len = usize::try_from(read_compact_size(&mut bytes)?).ok()?;
        let (item, rest) = bytes.split_at_checked(len)?;
        stack.push(item.to_vec());
        bytes = rest;
    }
    bytes.is_empty().then_some(stack)
}

/// A `std::vector<CTxOut>` as Core serializes it.
fn serialize_spent_outputs(spent: &[SpentOutput<'_>]) -> Vec<u8> {
    let mut out = Vec::new();
    write_compact_size(&mut out, spent.len() as u64);
    for o in spent {
        out.extend(o.amount.to_le_bytes());
        write_compact_size(&mut out, o.script_pubkey.len() as u64);
        out.extend(o.script_pubkey);
    }
    out
}

/// The coins-database and undo codecs (`ORACLE_CODEC_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoinCodec {
    /// bytes -> u64 LE (`ReadCompactSize` with range check)
    CompactSizeDecode = 0,
    /// u64 LE -> bytes
    CompactSizeEncode = 1,
    /// bytes -> u64 LE (`VARINT`, default mode)
    VarIntDecode = 2,
    /// u64 LE -> bytes
    VarIntEncode = 3,
    /// u64 LE -> u64 LE
    AmountCompress = 4,
    /// u64 LE -> u64 LE
    AmountDecompress = 5,
    /// raw script -> `ScriptCompression` serialization
    ScriptCompress = 6,
    /// `ScriptCompression` serialization -> raw script
    ScriptDecompress = 7,
    /// u32 LE height | u8 coinbase | `CTxOut` -> `Coin` serialization
    CoinEncode = 8,
    /// `Coin` serialization -> u32 LE height | u8 coinbase | `CTxOut`
    CoinDecode = 9,
    /// `CTxUndo` serialization -> its re-serialization
    TxUndoRoundtrip = 10,
    /// `CBlockUndo` serialization -> its re-serialization
    BlockUndoRoundtrip = 11,
}

/// One pinned Core release. Cheap to copy; all pins can be used concurrently.
#[derive(Clone, Copy, Debug)]
pub struct Oracle {
    pin: Pin,
    api: &'static ffi::Api,
}

/// Accept, or the block rejection `state` reports.
pub(crate) fn block_verdict(accepted: bool, state: &ffi::OracleValidation) -> Verdict {
    if accepted {
        Verdict::Accept
    } else {
        Verdict::Reject(rejection(state, ResultCode::Block))
    }
}

/// The rejection a `CValidationState` reports.
pub(crate) fn rejection(
    state: &ffi::OracleValidation,
    code: impl FnOnce(i32) -> ResultCode,
) -> Rejection {
    Rejection {
        result: code(state.result),
        reason: c_string(&state.reject_reason),
        debug_message: c_string(&state.debug_message),
        reason_truncated: state.reject_reason_len as usize >= state.reject_reason.len(),
        debug_truncated: state.debug_message_len as usize >= state.debug_message.len(),
    }
}

/// `chain`'s name and arguments as C strings (for `set_network` and `peer_session_new`).
fn chain_cstrings(chain: &Chain) -> Result<(CString, Vec<CString>)> {
    let c_str = |s: &str| {
        CString::new(s).map_err(|_| Error {
            kind: ErrorKind::Argument,
            message: format!("NUL in chain name or argument {s:?}"),
        })
    };
    Ok((c_str(&chain.name)?, chain.args.iter().map(|a| c_str(a)).collect::<Result<Vec<_>>>()?))
}

fn c_string(chars: &[c_char]) -> String {
    let bytes: Vec<u8> = chars.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

impl Oracle {
    /// The oracle of `pin`. Panics if the linked archives are not the pinned ones
    /// (see [`ARCHIVES_PINNED`]) unless `DASH_ORACLE_ALLOW_UNPINNED=1` is set.
    pub fn new(pin: Pin) -> Self {
        assert!(
            ARCHIVES_PINNED || std::env::var("DASH_ORACLE_ALLOW_UNPINNED").is_ok_and(|v| v == "1"),
            "dash-oracle-sys was built with unpinned archives; set DASH_ORACLE_ALLOW_UNPINNED=1 to use them"
        );
        Self {
            pin,
            api: pin.api(),
        }
    }

    /// Every linked pin.
    pub fn all() -> impl Iterator<Item = Oracle> {
        Pin::ALL.into_iter().map(Oracle::new)
    }

    pub fn pin(&self) -> Pin {
        self.pin
    }

    // abi_version, core_version and script_error_string read constants and touch no Core
    // global, so they take no lock and may run during a peer or chain session.

    /// `ORACLE_ABI_VERSION` of the linked shim (must equal [`ffi::ABI_VERSION`]).
    pub fn abi_version(&self) -> i32 {
        unsafe { (self.api.abi_version)() }
    }

    /// Core's `FormatFullVersion()`.
    pub fn core_version(&self) -> String {
        unsafe { CStr::from_ptr((self.api.core_version)()) }.to_string_lossy().into_owned()
    }

    /// Core's `ScriptErrorString()`.
    pub fn script_error_string(&self, err: i32) -> String {
        let mut buf = [0 as c_char; 256];
        unsafe { (self.api.script_error_string)(err, buf.as_mut_ptr(), buf.len()) };
        c_string(&buf)
    }

    /// Map a status to `Ok(true)` (OK), `Ok(false)` (REJECT) or the error.
    fn status(&self, status: c_int) -> Result<bool> {
        let kind = match status {
            ffi::ORACLE_OK => return Ok(true),
            ffi::ORACLE_REJECT => return Ok(false),
            ffi::ORACLE_E_DECODE => ErrorKind::Decode,
            ffi::ORACLE_E_EXCEPTION => ErrorKind::Exception,
            ffi::ORACLE_E_ARG => ErrorKind::Argument,
            ffi::ORACLE_E_NETWORK => ErrorKind::Network,
            ffi::ORACLE_E_TRAILING => ErrorKind::Trailing,
            ffi::ORACLE_E_CONTEXT => ErrorKind::Context,
            ffi::ORACLE_E_UNSUPPORTED => ErrorKind::Unsupported,
            ffi::ORACLE_E_POISONED => ErrorKind::ContextPoisoned,
            other => ErrorKind::Status(other),
        };
        Err(Error {
            kind,
            message: self.last_error(),
        })
    }

    fn ok(&self, status: c_int) -> Result<()> {
        self.status(status).map(|_| ())
    }

    /// Call `f(&mut out)` and return `out` once the shim reports success.
    fn with_out<T: Default>(&self, f: impl FnOnce(&mut T) -> c_int) -> Result<T> {
        let mut out = T::default();
        self.ok(f(&mut out))?;
        Ok(out)
    }

    /// Call `f(&mut state)` and turn the status and `CValidationState` into a verdict
    /// whose result class `code` tags.
    fn verdict(
        &self,
        code: impl FnOnce(i32) -> ResultCode,
        f: impl FnOnce(&mut ffi::OracleValidation) -> c_int,
    ) -> Result<Verdict> {
        let mut state = ffi::OracleValidation::default();
        Ok(if self.status(f(&mut state))? {
            Verdict::Accept
        } else {
            Verdict::Reject(rejection(&state, code))
        })
    }

    /// Call `f(buf, &mut len)`, growing `buf` once if the shim reports the size it needs.
    fn with_buffer(
        &self,
        initial: usize,
        mut f: impl FnMut(*mut u8, *mut usize) -> c_int,
    ) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; initial.max(16)];
        let mut len = buf.len();
        let mut status = f(buf.as_mut_ptr(), &mut len);
        if status == ffi::ORACLE_E_BUFFER {
            buf.resize(len, 0);
            status = f(buf.as_mut_ptr(), &mut len);
        }
        self.ok(status)?;
        buf.truncate(len);
        Ok(buf)
    }

    /// Run `f` holding the pin's lock shared: calls that select no chain run alongside each
    /// other, but never alongside a chain call or a session. Nested in another call, `f`
    /// runs under that call's hold.
    fn shared<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        match self.pin.held() {
            Held::Shared | Held::Exclusive => return f(),
            Held::Session => return Err(session_live(self.pin)),
            Held::None => {}
        }
        let state = self.pin.state().read().unwrap_or_else(|e| e.into_inner());
        state.usable()?;
        let _hold = Hold::new(self.pin, Held::Shared);
        f()
    }

    /// Run `f` with Core's globals set to `chain`, holding the pin's lock exclusively.
    fn on_chain<T>(&self, chain: &Chain, f: impl FnOnce() -> Result<T>) -> Result<T> {
        match self.pin.held() {
            Held::None => {}
            Held::Session => return Err(session_live(self.pin)),
            held => unreachable!("a chain call inside another oracle call ({held:?})"),
        }
        let mut state = self.pin.state().write().unwrap_or_else(|e| e.into_inner());
        state.usable()?;
        let _hold = Hold::new(self.pin, Held::Exclusive);
        if state.chain.as_ref() != Some(chain) {
            state.chain = None;
            let (name, args) = chain_cstrings(chain)?;
            let argv: Vec<*const c_char> = args.iter().map(|a| a.as_ptr()).collect();
            self.ok(unsafe { (self.api.set_network)(name.as_ptr(), argv.as_ptr(), argv.len()) })?;
            state.chain = Some(chain.clone());
        }
        f()
    }

    /// Run `f` holding the pin's lock exclusively without selecting a chain: for calls that set
    /// other process-wide Core state (gArgs) for their duration.
    fn exclusive<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        match self.pin.held() {
            Held::None => {}
            Held::Session => return Err(session_live(self.pin)),
            held => unreachable!("an exclusive call inside another oracle call ({held:?})"),
        }
        let state = self.pin.state().write().unwrap_or_else(|e| e.into_inner());
        state.usable()?;
        let _hold = Hold::new(self.pin, Held::Exclusive);
        f()
    }

    /// This thread's `oracle_last_error()`, whole: a message longer than the first buffer is
    /// read again into one of its full length.
    fn last_error(&self) -> String {
        let mut buf = vec![0 as c_char; 1024];
        let len = unsafe { (self.api.last_error)(buf.as_mut_ptr(), buf.len()) };
        if len >= buf.len() {
            buf.resize(len + 1, 0);
            unsafe { (self.api.last_error)(buf.as_mut_ptr(), buf.len()) };
        }
        c_string(&buf)
    }

    /// Core's chain parameters as JSON.
    pub fn chainparams_json(&self, chain: &Chain) -> Result<String> {
        self.on_chain(chain, || {
            let bytes = self.with_buffer(64 * 1024, |out, len| unsafe {
                (self.api.chainparams_json)(out.cast(), len)
            })?;
            let json = CStr::from_bytes_until_nul(&bytes).expect("NUL-terminated JSON");
            Ok(json.to_string_lossy().into_owned())
        })
    }

    /// Deserialize a transaction (trailing bytes are allowed; see `consumed`).
    pub fn tx_decode(&self, bytes: &[u8]) -> Result<DecodedTx> {
        self.shared(|| {
            let (mut consumed, mut txid, mut instance_hash) = (0, [0u8; 32], [0u8; 32]);
            let reencoded = self.with_buffer(bytes.len() + 64, |out, len| unsafe {
                (self.api.tx_decode)(
                    bytes.as_ptr(),
                    bytes.len(),
                    &mut consumed,
                    out,
                    len,
                    txid.as_mut_ptr(),
                    instance_hash.as_mut_ptr(),
                )
            })?;
            Ok(DecodedTx {
                consumed,
                reencoded,
                txid,
                instance_hash,
            })
        })
    }

    /// Deserialize a block (trailing bytes are allowed; see `consumed`).
    pub fn block_decode(&self, bytes: &[u8]) -> Result<DecodedBlock> {
        self.shared(|| {
            let (mut consumed, mut hash, mut merkle_root, mut mutated) =
                (0, [0u8; 32], [0u8; 32], 0);
            let reencoded = self.with_buffer(bytes.len() + 64, |out, len| unsafe {
                (self.api.block_decode)(
                    bytes.as_ptr(),
                    bytes.len(),
                    &mut consumed,
                    out,
                    len,
                    hash.as_mut_ptr(),
                    merkle_root.as_mut_ptr(),
                    &mut mutated,
                )
            })?;
            Ok(DecodedBlock {
                consumed,
                reencoded,
                hash,
                merkle_root,
                mutated: mutated != 0,
            })
        })
    }

    /// Deserialize a block header (trailing bytes are allowed; see `consumed`).
    pub fn header_decode(&self, bytes: &[u8]) -> Result<DecodedHeader> {
        self.shared(|| {
            let (mut consumed, mut reencoded, mut hash) = (0, [0u8; 80], [0u8; 32]);
            self.ok(unsafe {
                (self.api.header_decode)(
                    bytes.as_ptr(),
                    bytes.len(),
                    &mut consumed,
                    reencoded.as_mut_ptr(),
                    hash.as_mut_ptr(),
                )
            })?;
            Ok(DecodedHeader {
                consumed,
                reencoded,
                hash,
            })
        })
    }

    /// `ComputeMerkleRoot()`: (root, mutated).
    pub fn merkle_root(&self, leaves: &[[u8; 32]]) -> Result<([u8; 32], bool)> {
        self.shared(|| {
            let (mut root, mut mutated) = ([0u8; 32], 0);
            self.ok(unsafe {
                (self.api.merkle_root)(
                    leaves.as_ptr().cast(),
                    leaves.len(),
                    root.as_mut_ptr(),
                    &mut mutated,
                )
            })?;
            Ok((root, mutated != 0))
        })
    }

    /// One coins/undo codec operation: (output, input bytes consumed).
    pub fn coin_codec(&self, op: CoinCodec, input: &[u8]) -> Result<(Vec<u8>, usize)> {
        self.shared(|| {
            let mut consumed = 0;
            let out = self.with_buffer(input.len() * 2 + 64, |out, len| unsafe {
                (self.api.coin_codec)(
                    op as c_int,
                    input.as_ptr(),
                    input.len(),
                    &mut consumed,
                    out,
                    len,
                )
            })?;
            Ok((out, consumed))
        })
    }

    /// `CheckTransaction()`.
    pub fn check_transaction(&self, tx: &[u8]) -> Result<Verdict> {
        self.shared(|| {
            self.verdict(ResultCode::Tx, |state| unsafe {
                (self.api.check_transaction)(tx.as_ptr(), tx.len(), state)
            })
        })
    }

    /// `CheckBlock()` with `chain`'s consensus parameters.
    pub fn check_block(
        &self,
        chain: &Chain,
        block: &[u8],
        check_pow: bool,
        check_merkle_root: bool,
    ) -> Result<Verdict> {
        self.on_chain(chain, || {
            self.verdict(ResultCode::Block, |state| unsafe {
                (self.api.check_block)(
                    block.as_ptr(),
                    block.len(),
                    check_pow.into(),
                    check_merkle_root.into(),
                    state,
                )
            })
        })
    }

    /// `GetLegacySigOpCount()`.
    pub fn legacy_sigops(&self, tx: &[u8]) -> Result<u32> {
        self.shared(|| {
            self.with_out(|count| unsafe { (self.api.legacy_sigops)(tx.as_ptr(), tx.len(), count) })
        })
    }

    /// `VerifyScript()` of input `n_in` of `tx` spending `script_pubkey` worth `amount`.
    pub fn verify_script(
        &self,
        script_pubkey: &[u8],
        tx: &[u8],
        n_in: u32,
        amount: i64,
        flags: u32,
    ) -> Result<ScriptResult> {
        self.shared(|| {
            let mut error = 0;
            let status = unsafe {
                (self.api.verify_script)(
                    script_pubkey.as_ptr(),
                    script_pubkey.len(),
                    tx.as_ptr(),
                    tx.len(),
                    n_in,
                    amount,
                    flags,
                    &mut error,
                )
            };
            Ok(ScriptResult {
                ok: self.status(status)?,
                error,
            })
        })
    }

    /// `SignatureHash()` (`SigVersion::BASE`).
    pub fn signature_hash(
        &self,
        script_code: &[u8],
        tx: &[u8],
        n_in: u32,
        hash_type: i32,
    ) -> Result<[u8; 32]> {
        self.shared(|| {
            self.with_out(|out: &mut [u8; 32]| unsafe {
                (self.api.signature_hash)(
                    script_code.as_ptr(),
                    script_code.len(),
                    tx.as_ptr(),
                    tx.len(),
                    n_in,
                    hash_type,
                    out.as_mut_ptr(),
                )
            })
        })
    }

    /// `GetNextWorkRequired()` for the block after `headers` (consecutive 80-byte
    /// headers starting at `first_height`); only `new_header`'s time is read.
    pub fn next_work_required(
        &self,
        chain: &Chain,
        headers: &[u8],
        first_height: i32,
        new_header: &[u8; 80],
    ) -> Result<u32> {
        assert_eq!(headers.len() % 80, 0, "headers must be 80-byte records");
        self.on_chain(chain, || {
            self.with_out(|bits| unsafe {
                (self.api.next_work_required)(
                    headers.as_ptr(),
                    headers.len() / 80,
                    first_height,
                    new_header.as_ptr(),
                    bits,
                )
            })
        })
    }

    /// `CheckProofOfWork()`.
    pub fn check_proof_of_work(&self, chain: &Chain, hash: &[u8; 32], bits: u32) -> Result<bool> {
        self.on_chain(chain, || {
            self.status(unsafe { (self.api.check_proof_of_work)(hash.as_ptr(), bits) })
        })
    }

    /// `arith_uint256::SetCompact()`.
    pub fn compact_decode(&self, bits: u32) -> Result<Compact> {
        self.shared(|| {
            let (mut target, mut negative, mut overflow) = ([0u8; 32], 0, 0);
            self.ok(unsafe {
                (self.api.compact_decode)(bits, target.as_mut_ptr(), &mut negative, &mut overflow)
            })?;
            Ok(Compact {
                target,
                negative: negative != 0,
                overflow: overflow != 0,
            })
        })
    }

    /// `arith_uint256::GetCompact()`.
    pub fn compact_encode(&self, target: &[u8; 32]) -> Result<u32> {
        self.shared(|| {
            self.with_out(|bits| unsafe { (self.api.compact_encode)(target.as_ptr(), bits) })
        })
    }

    /// `GetBlockProof()` for `bits`.
    pub fn block_proof(&self, bits: u32) -> Result<[u8; 32]> {
        self.shared(|| {
            self.with_out(|proof: &mut [u8; 32]| unsafe {
                (self.api.block_proof)(bits, proof.as_mut_ptr())
            })
        })
    }

    /// `GetBlockSubsidyInner()`.
    pub fn block_subsidy_inner(
        &self,
        chain: &Chain,
        prev_bits: u32,
        prev_height: i32,
        v20: bool,
    ) -> Result<i64> {
        self.on_chain(chain, || {
            self.with_out(|out| unsafe {
                (self.api.block_subsidy_inner)(prev_bits, prev_height, v20.into(), out)
            })
        })
    }

    /// `GetSuperblockSubsidyInner()`.
    pub fn superblock_subsidy_inner(
        &self,
        chain: &Chain,
        prev_bits: u32,
        prev_height: i32,
        v20: bool,
    ) -> Result<i64> {
        self.on_chain(chain, || {
            self.with_out(|out| unsafe {
                (self.api.superblock_subsidy_inner)(prev_bits, prev_height, v20.into(), out)
            })
        })
    }

    /// `GetMasternodePayment()`.
    pub fn mn_payment(
        &self,
        chain: &Chain,
        height: i32,
        block_value: i64,
        era: RewardEra,
    ) -> Result<i64> {
        self.on_chain(chain, || {
            self.with_out(|out| unsafe {
                (self.api.mn_payment)(height, block_value, era as c_int, out)
            })
        })
    }

    /// `PlatformShare()`.
    pub fn platform_share(&self, mn_reward: i64) -> Result<i64> {
        self.shared(|| self.with_out(|out| unsafe { (self.api.platform_share)(mn_reward, out) }))
    }

    /// `EvalScript()` with `BaseSignatureChecker` and `SigVersion::BASE`.
    pub fn eval_script(&self, script: &[u8], stack: &[Vec<u8>], flags: u32) -> Result<EvalResult> {
        self.shared(|| {
            let stack_in = serialize_stack(stack);
            let mut error = 0;
            let mut ok = false;
            let out = self.with_buffer(stack_in.len() + 64, |out, len| {
                let status = unsafe {
                    (self.api.eval_script)(
                        script.as_ptr(),
                        script.len(),
                        stack_in.as_ptr(),
                        stack_in.len(),
                        flags,
                        out,
                        len,
                        &mut error,
                    )
                };
                // ORACLE_REJECT still fills the stack; report it as success to with_buffer.
                ok = status == ffi::ORACLE_OK;
                if status == ffi::ORACLE_REJECT {
                    ffi::ORACLE_OK
                } else {
                    status
                }
            })?;
            let stack = deserialize_stack(&out).expect("the shim returns a serialized stack");
            Ok(EvalResult {
                ok,
                error,
                stack,
            })
        })
    }

    /// `CPubKey(pubkey).Verify(msg, sig)`.
    pub fn pubkey_verify(&self, pubkey: &[u8], msg: &[u8; 32], sig: &[u8]) -> Result<bool> {
        self.shared(|| {
            self.status(unsafe {
                (self.api.pubkey_verify)(
                    pubkey.as_ptr(),
                    pubkey.len(),
                    msg.as_ptr(),
                    sig.as_ptr(),
                    sig.len(),
                )
            })
        })
    }

    /// `CPubKey::CheckLowS(sig)`.
    pub fn check_low_s(&self, sig: &[u8]) -> Result<bool> {
        self.shared(|| self.status(unsafe { (self.api.check_low_s)(sig.as_ptr(), sig.len()) }))
    }

    /// `ecdsa_signature_parse_der_lax()`: the signature in libsecp256k1's compact form,
    /// or None if the parser fails.
    pub fn parse_der_lax(&self, sig: &[u8]) -> Result<Option<[u8; 64]>> {
        self.shared(|| {
            let mut compact = [0u8; 64];
            let parsed = self.status(unsafe {
                (self.api.parse_der_lax)(sig.as_ptr(), sig.len(), compact.as_mut_ptr())
            })?;
            Ok(parsed.then_some(compact))
        })
    }

    /// `CheckSignatureEncoding(sig, flags)`.
    pub fn check_signature_encoding(&self, sig: &[u8], flags: u32) -> Result<ScriptResult> {
        self.shared(|| {
            let mut error = 0;
            let status = unsafe {
                (self.api.check_signature_encoding)(sig.as_ptr(), sig.len(), flags, &mut error)
            };
            Ok(ScriptResult {
                ok: self.status(status)?,
                error,
            })
        })
    }

    /// `CheckMinimalPush(data, opcode)`.
    pub fn check_minimal_push(&self, data: &[u8], opcode: u8) -> Result<bool> {
        self.shared(|| {
            self.status(unsafe {
                (self.api.check_minimal_push)(data.as_ptr(), data.len(), opcode.into())
            })
        })
    }

    /// `CScriptNum(vch, require_minimal, max_size)`, or None where Core throws
    /// `scriptnum_error`.
    pub fn scriptnum_decode(
        &self,
        vch: &[u8],
        require_minimal: bool,
        max_size: usize,
    ) -> Result<Option<ScriptNum>> {
        self.shared(|| {
            let (mut value, mut getint) = (0, 0);
            let decoded = self.status(unsafe {
                (self.api.scriptnum_decode)(
                    vch.as_ptr(),
                    vch.len(),
                    require_minimal.into(),
                    max_size,
                    &mut value,
                    &mut getint,
                )
            })?;
            Ok(decoded.then_some(ScriptNum {
                value,
                getint,
            }))
        })
    }

    /// `CScriptNum::serialize(value)`.
    pub fn scriptnum_encode(&self, value: i64) -> Result<Vec<u8>> {
        self.shared(|| {
            self.with_buffer(16, |out, len| unsafe { (self.api.scriptnum_encode)(value, out, len) })
        })
    }

    /// `CScriptNum::IsMinimallyEncoded(vch, max_size)`.
    pub fn scriptnum_is_minimally_encoded(&self, vch: &[u8], max_size: usize) -> Result<bool> {
        self.shared(|| {
            self.status(unsafe {
                (self.api.scriptnum_is_minimally_encoded)(vch.as_ptr(), vch.len(), max_size)
            })
        })
    }

    /// `CScriptNum::MinimallyEncode(vch)`: the result and whether Core changed it.
    pub fn scriptnum_minimally_encode(&self, vch: &[u8]) -> Result<(Vec<u8>, bool)> {
        self.shared(|| {
            let mut changed = 0;
            let out = self.with_buffer(vch.len(), |out, len| unsafe {
                (self.api.scriptnum_minimally_encode)(
                    vch.as_ptr(),
                    vch.len(),
                    out,
                    len,
                    &mut changed,
                )
            })?;
            Ok((out, changed != 0))
        })
    }

    /// `FindAndDelete(script, pattern)`: the script and the number of matches removed.
    pub fn find_and_delete(&self, script: &[u8], pattern: &[u8]) -> Result<(Vec<u8>, i32)> {
        self.shared(|| {
            let mut count = 0;
            let out = self.with_buffer(script.len(), |out, len| unsafe {
                (self.api.find_and_delete)(
                    script.as_ptr(),
                    script.len(),
                    pattern.as_ptr(),
                    pattern.len(),
                    out,
                    len,
                    &mut count,
                )
            })?;
            Ok((out, count))
        })
    }

    /// `CScript::GetSigOpCount(accurate)`.
    pub fn script_sigops(&self, script: &[u8], accurate: bool) -> Result<u32> {
        self.shared(|| {
            self.with_out(|count| unsafe {
                (self.api.script_sigops)(script.as_ptr(), script.len(), accurate.into(), count)
            })
        })
    }

    /// `spk.GetSigOpCount(script_sig)`.
    pub fn script_sigops_with_sig(&self, script_pubkey: &[u8], script_sig: &[u8]) -> Result<u32> {
        self.shared(|| {
            self.with_out(|count| unsafe {
                (self.api.script_sigops_with_sig)(
                    script_pubkey.as_ptr(),
                    script_pubkey.len(),
                    script_sig.as_ptr(),
                    script_sig.len(),
                    count,
                )
            })
        })
    }

    /// The three transaction-level sigop counts over the outputs `tx` spends (one per
    /// input, in input order).
    pub fn transaction_sigops(
        &self,
        tx: &[u8],
        spent: &[SpentOutput<'_>],
        flags: u32,
    ) -> Result<TxSigops> {
        self.shared(|| {
            let spent = serialize_spent_outputs(spent);
            self.with_out(|c: &mut TxSigops| unsafe {
                (self.api.transaction_sigops)(
                    tx.as_ptr(),
                    tx.len(),
                    spent.as_ptr(),
                    spent.len(),
                    flags,
                    &mut c.legacy,
                    &mut c.p2sh,
                    &mut c.total,
                )
            })
        })
    }

    /// `CheckInputScripts()` over the outputs `tx` spends (one per input, in input order).
    pub fn check_input_scripts(
        &self,
        tx: &[u8],
        spent: &[SpentOutput<'_>],
        flags: u32,
    ) -> Result<Verdict> {
        self.shared(|| {
            let spent = serialize_spent_outputs(spent);
            self.verdict(ResultCode::Tx, |state| unsafe {
                (self.api.check_input_scripts)(
                    tx.as_ptr(),
                    tx.len(),
                    spent.as_ptr(),
                    spent.len(),
                    flags,
                    state,
                )
            })
        })
    }

    /// `ConvertBitsToDouble()`.
    pub fn convert_bits_to_double(&self, bits: u32) -> Result<f64> {
        self.shared(|| self.with_out(|out| unsafe { (self.api.convert_bits_to_double)(bits, out) }))
    }

    /// `SplitAmountByShares()` over share amounts `weights`.
    pub fn split_amount_by_shares(&self, total: i64, weights: &[i64]) -> Result<Vec<i64>> {
        self.shared(|| {
            let mut amounts = vec![0; weights.len()];
            self.ok(unsafe {
                (self.api.split_amount_by_shares)(
                    total,
                    weights.as_ptr(),
                    weights.len(),
                    amounts.as_mut_ptr(),
                )
            })?;
            Ok(amounts)
        })
    }

    /// `CSuperblock::GetPaymentsLimit()` with `chain`'s parameters.
    pub fn superblock_payments_limit(&self, chain: &Chain, height: i32) -> Result<i64> {
        self.on_chain(chain, || {
            self.with_out(|out| unsafe { (self.api.superblock_payments_limit)(height, out) })
        })
    }
}
