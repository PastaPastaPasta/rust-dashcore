//! The per-command decode policy (P2a §3.2.2–3.2.5): BS4a's normative pre-read columns for
//! Core's 74 commands, the read point, and the exception boundary that turns a decode error into
//! Core's `ProcessMessages` log line.
//!
//! The processors own each command's post-read outcome and its handler (P2a-core's registry,
//! SPV's router); this table is what both read before and at the read.

use std::any::TypeId;
use std::borrow::Cow;
use std::collections::VecDeque;

use dash_p2p_core::limits::{
    MAX_BLOOM_FILTER_SIZE, MAX_HEADERS_COMPRESSED_RESULT, MAX_HEADERS_UNCOMPRESSED_RESULT,
    MAX_SCRIPT_ELEMENT_SIZE, MAX_SIZE,
};
use dash_p2p_core::{
    CommandString, CompressedHeader, DecodedMsg, FilterAdd, FilterLoad, GetQrInfo, GovSync,
    Headers2, MsgDecode, P2pDecodeError, P2pMsg, ProtoCtx, ProtocolVersion, SendCmpct,
};
use dash_primitives::{BlockHash, BlockHeader, GovVote};
use dash_types::codec::{BaseCodec, DecodeError, EncodeBuf};
use dash_types::{CompactSize, Hashable};

/// When a command may arrive (BS4a §4.0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateClass {
    /// `version`: any time (gate 1 handles a repeat).
    Version,
    /// `verack`, `sendheaders`, `sendheaders2`, `sendcmpct`, `sendaddrv2`, `sendtxrcncl`: before
    /// or after `verack` (gates 3 and 4).
    BeforeOrAfterVerack,
    /// Everything else: only after `verack` (gate 5).
    PostVerack,
}

/// What Core's branch does before its first read (BS4a "Before the read"). Every variant is a
/// condition Core tests first, and what Core does when it holds; the generic gates of P2a §3.2.1
/// (a repeated `version`, a command before `version` or `verack`, the blocks-only disconnect) are
/// not repeated here. Conditions that depend on node state (masternode role, sync state, sporks,
/// wallet) are named; the processor that owns the state evaluates them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreRead {
    /// `LoadingBlocks()` (`-reindex` or import): the message is ignored unread
    /// (`cmpctblock`, `blocktxn`, `headers`, `headers2`, `block`).
    LoadingBlocksIgnore,
    /// `headers`/`headers2`, after the loading-blocks check: clear `m_last_getheaders_timestamp`,
    /// before the count is read, so it happens even when the read then throws.
    HeadersClearGetheadersTimestamp,
    /// `tx`/`dstx` during initial block download: return unread, no penalty.
    IbdReturn,
    /// `filterload`, `filteradd`, `filterclear` without `NODE_BLOOM` for this peer: disconnect.
    NoBloomDisconnect,
    /// `sendtxrcncl` without txreconciliation: log and return, nothing read.
    TxReconciliationOff,
    /// `ssc` once the masternode sync has finished (`IsSynced()`): ignored before the read.
    MnSyncFinished,
    /// Governance messages (`govsync`, `govobj`, `govobjvote`): ignored unread when governance is
    /// disabled or invalid, or the blockchain is not synced.
    GovernanceUnavailable,
    /// `govsync` only, in addition: ignored unread until the full masternode sync has finished.
    GovernanceNotSynced,
    /// `clsig` while ChainLocks are disabled (SPORK_19 off): not read.
    ChainLocksDisabled,
    /// `isdlock` while InstantSend is disabled (SPORK_2 off, or reindexing or importing): not read.
    InstantSendDisabled,
    /// Read only on a masternode (the CoinJoin server, the signing-shares manager); a plain node
    /// ignores the message unread.
    MasternodeOnly,
    /// `qsigshare` only, in addition: read only while SPORK_21 is active.
    Spork21Off,
    /// `mnauth`, `platformban` and the CoinJoin server messages: not read until
    /// `IsBlockchainSynced()`.
    ChainNotSynced,
    /// `dsa`, `dsi`, `dss`: the CoinJoin server's session state is tested before the read and
    /// answers with a status message (`ERR_QUEUE_FULL`, `ERR_SESSION`, `ERR_INVALID_INPUT`), no
    /// penalty.
    CoinJoinServerSession,
    /// `dsq`: read by masternodes and by non-masternode wallet builds not in `-blocksonly`; any
    /// other node ignores it.
    CoinJoinMasternodeOrWallet,
    /// `dssu`, `dsf`, `dsc`: read only by wallet builds, once per matching CoinJoin client session
    /// (client enabled, chain synced, disk space, mixing with this peer as its masternode).
    CoinJoinWalletSessions,
    /// `qgetdata`, `qdata`: a peer that is not a verified masternode (or a `qwatch` connection for
    /// `qgetdata`), or a node without the masternode role, scores +10 before the read.
    QuorumDataPeerCheck,
    /// `qcontrib`, `qcomplaint`, `qjustify`, `qpcommit`. A plain node (neither masternode nor
    /// observer) registers `NetDKGStub`, which scores +10 whatever SPORK_17 says (v24
    /// `init.cpp:2235`, `llmq/net_dkg.cpp:938-944`). With `NetDKG` (a masternode or an observer,
    /// `llmq/net_dkg.cpp:491-604`), in order: SPORK_17 off is ignored; an observer ignores them;
    /// an unverified peer scores +10; a payload under 33 bytes scores +100.
    DkgIntake,
}

/// How Core's handler reads the payload (BS4a "Handler reads").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadPlan {
    /// Nothing is read (the payload is ignored). The processor does not decode such a message, so
    /// it cannot throw: base-sdk's decoder parses `mnlistdiff`, `qrinfo`, `merkleblock`,
    /// `cfilter`, `cfheaders` and `cfcheckpt` for SPV's replies, and refuses what Core ignores.
    /// P2a-core's `dashd-p2p` processor upholds this: it scores or ignores these rows unread.
    Nothing,
    /// The whole message type is read at once.
    Whole,
    /// A count is read and checked against `limit` before any item.
    CountThenItems {
        /// The count limit Core checks first.
        limit: usize,
    },
    /// As [`ReadPlan::CountThenItems`], with a limit that depends on the common version
    /// (`headers2`: `GetHeadersLimit`).
    CountThenItemsByVersion {
        /// The limit below `raised_from`.
        limit: usize,
        /// The limit from `raised_from` on.
        raised_limit: usize,
        /// The first common version with `raised_limit`.
        raised_from: i32,
    },
    /// The handler reads in several steps of its own (e.g. `version`, `pong`, `govsync`).
    Custom,
}

/// What happens when the read throws (BS4a "Decode failure").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThrowOutcome {
    /// Nothing is read, or the read cannot throw (`pong` reads only when 8 bytes remain).
    NeverThrows,
    /// Caught by `ProcessMessages` and logged; the peer stays.
    CaughtByProcessMessages,
    /// `version`: a throw in the optional tail is logged and the peer stays pre-version, but only
    /// after the head checks (`SetServices`, the services check, the obsolete-version check).
    VersionTail,
    /// The handler catches it and scores `score` with its own message `msg` (Core appends
    /// `". peer=%d error=%s"` to the filter, spork and getqrinfo messages).
    Handler {
        /// The misbehaviour score.
        score: i32,
        /// Core's `Misbehaving` reason, without Core's `". peer=%d error=%s"` suffix.
        msg: &'static str,
    },
    /// As [`ThrowOutcome::Handler`] for a throw after the first `head_bytes`; a throw inside
    /// those is not caught by the handler and `ProcessMessages` logs it (`govsync`: the `uint256`
    /// before the bloom filter).
    HandlerAfterHead {
        /// Bytes read outside the handler's catch.
        head_bytes: usize,
        /// The misbehaviour score.
        score: i32,
        /// Core's `Misbehaving` reason.
        msg: &'static str,
    },
    /// The handler scores `score` (the signing shares' `BanNode`) and rethrows, so
    /// `ProcessMessages` logs the exception too.
    HandlerThenCaught {
        /// The misbehaviour score.
        score: i32,
        /// Core's `Misbehaving` reason; empty when Core gives none.
        msg: &'static str,
    },
}

/// What Core does with bytes left after the handler's last read (BS4a "Trailing bytes").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trailing {
    /// Ignored (every command except the two v24 rules below).
    Ignored,
    /// The message must be consumed whole (+100): the four DKG messages (v24
    /// `llmq/net_dkg.cpp:600-603`).
    WholePayload,
    /// A payload longer than this many bytes is refused before the read (+100): `qgetdata`'s
    /// 67-byte request (v24 `llmq/net_quorum.cpp:90-93`).
    PayloadAtMost(usize),
}

/// One row of the policy table.
#[derive(Clone, Copy, Debug)]
pub struct CommandPolicy {
    /// The command.
    pub cmd: CommandString,
    /// When it may arrive.
    pub gate: GateClass,
    /// Effects before the first read.
    pub pre_read: &'static [PreRead],
    /// The read.
    pub read: ReadPlan,
    /// A failed read.
    pub on_throw: ThrowOutcome,
    /// Bytes left after the last read.
    pub trailing: Trailing,
    /// In Core's `NetMessageViolatesBlocksOnly` set (`protocol.cpp:185-212`).
    pub blocks_only_violation: bool,
    /// In Core's `IsQuorumPriorityMessage` set (`net.h:2053-2070`).
    pub quorum_priority: bool,
}

/// What a network-core message function (handshake, ping, addr intake) decided.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// Handled; continue.
    Done,
    /// Ignored with a log line (e.g. a redundant `version` or `verack`).
    Ignored,
    /// `fDisconnect` was set.
    Disconnect,
    /// The read failed where Core's read throws; the processor logs it as Core's
    /// `ProcessMessages` does ([`Throw::log_line`]).
    Throw(Throw),
}

/// Core's `allNetMessageTypes` (`protocol.cpp:98-176`), in Core's order: the 74 types Core
/// counts bytes for and dispatches; anything else is an unknown command.
pub const ALL_NET_MESSAGE_TYPES: [CommandString; 74] = [
    CommandString::from_static("version"),
    CommandString::from_static("verack"),
    CommandString::from_static("addr"),
    CommandString::from_static("addrv2"),
    CommandString::from_static("sendaddrv2"),
    CommandString::from_static("inv"),
    CommandString::from_static("getdata"),
    CommandString::from_static("merkleblock"),
    CommandString::from_static("getblocks"),
    CommandString::from_static("getheaders"),
    CommandString::from_static("tx"),
    CommandString::from_static("headers"),
    CommandString::from_static("block"),
    CommandString::from_static("getaddr"),
    CommandString::from_static("mempool"),
    CommandString::from_static("ping"),
    CommandString::from_static("pong"),
    CommandString::from_static("notfound"),
    CommandString::from_static("filterload"),
    CommandString::from_static("filteradd"),
    CommandString::from_static("filterclear"),
    CommandString::from_static("sendheaders"),
    CommandString::from_static("sendcmpct"),
    CommandString::from_static("cmpctblock"),
    CommandString::from_static("getblocktxn"),
    CommandString::from_static("blocktxn"),
    CommandString::from_static("getcfilters"),
    CommandString::from_static("cfilter"),
    CommandString::from_static("getcfheaders"),
    CommandString::from_static("cfheaders"),
    CommandString::from_static("getcfcheckpt"),
    CommandString::from_static("cfcheckpt"),
    CommandString::from_static("sendtxrcncl"),
    CommandString::from_static("spork"),
    CommandString::from_static("getsporks"),
    CommandString::from_static("senddsq"),
    CommandString::from_static("dsa"),
    CommandString::from_static("dsi"),
    CommandString::from_static("dsf"),
    CommandString::from_static("dss"),
    CommandString::from_static("dsc"),
    CommandString::from_static("dssu"),
    CommandString::from_static("dstx"),
    CommandString::from_static("dsq"),
    CommandString::from_static("ssc"),
    CommandString::from_static("govsync"),
    CommandString::from_static("govobj"),
    CommandString::from_static("govobjvote"),
    CommandString::from_static("getmnlistd"),
    CommandString::from_static("mnlistdiff"),
    CommandString::from_static("qsendrecsigs"),
    CommandString::from_static("qfcommit"),
    CommandString::from_static("qcontrib"),
    CommandString::from_static("qcomplaint"),
    CommandString::from_static("qjustify"),
    CommandString::from_static("qpcommit"),
    CommandString::from_static("qwatch"),
    CommandString::from_static("qsigsesann"),
    CommandString::from_static("qsigsinv"),
    CommandString::from_static("qgetsigs"),
    CommandString::from_static("qbsigs"),
    CommandString::from_static("qsigrec"),
    CommandString::from_static("qsigshare"),
    CommandString::from_static("qgetdata"),
    CommandString::from_static("qdata"),
    CommandString::from_static("clsig"),
    CommandString::from_static("isdlock"),
    CommandString::from_static("mnauth"),
    CommandString::from_static("getheaders2"),
    CommandString::from_static("sendheaders2"),
    CommandString::from_static("headers2"),
    CommandString::from_static("getqrinfo"),
    CommandString::from_static("qrinfo"),
    CommandString::from_static("platformban"),
];

/// Core's `netMessageTypesViolateBlocksOnly` (`protocol.cpp:185-212`).
const BLOCKS_ONLY_VIOLATIONS: [CommandString; 26] = [
    CommandString::from_static("dsa"),
    CommandString::from_static("dsc"),
    CommandString::from_static("dsf"),
    CommandString::from_static("dsq"),
    CommandString::from_static("dss"),
    CommandString::from_static("dssu"),
    CommandString::from_static("dstx"),
    CommandString::from_static("dsi"),
    CommandString::from_static("getqrinfo"),
    CommandString::from_static("platformban"),
    CommandString::from_static("qbsigs"),
    CommandString::from_static("qcomplaint"),
    CommandString::from_static("qcontrib"),
    CommandString::from_static("qdata"),
    CommandString::from_static("qgetdata"),
    CommandString::from_static("qgetsigs"),
    CommandString::from_static("qjustify"),
    CommandString::from_static("qpcommit"),
    CommandString::from_static("qsendrecsigs"),
    CommandString::from_static("qsigrec"),
    CommandString::from_static("qsigsesann"),
    CommandString::from_static("qsigshare"),
    CommandString::from_static("qsigsinv"),
    CommandString::from_static("qrinfo"),
    CommandString::from_static("qwatch"),
    CommandString::from_static("tx"),
];

/// Core's `IsQuorumPriorityMessage` (`net.h:2053-2070`).
const QUORUM_PRIORITY: [CommandString; 8] = [
    CommandString::from_static("qsigshare"),
    CommandString::from_static("qbsigs"),
    CommandString::from_static("qsigsinv"),
    CommandString::from_static("qgetsigs"),
    CommandString::from_static("qsigsesann"),
    CommandString::from_static("qsigrec"),
    CommandString::from_static("clsig"),
    CommandString::from_static("isdlock"),
];

/// Whether `cmd` is one of Core's 74 message types (`getAllNetMessageTypes`).
pub fn is_known(cmd: &CommandString) -> bool {
    ALL_NET_MESSAGE_TYPES.contains(cmd)
}

/// Core's `IsQuorumPriorityMessage`: the messages `PollMessage` serves first.
pub fn is_quorum_priority(cmd: &CommandString) -> bool {
    QUORUM_PRIORITY.contains(cmd)
}

/// Core's `NetMessageViolatesBlocksOnly`.
pub fn violates_blocks_only(cmd: &CommandString) -> bool {
    BLOCKS_ONLY_VIOLATIONS.contains(cmd)
}

/// Whether `cmd` is in `set` (a `const` form of `slice::contains`, for the table's flags).
const fn in_set(set: &[CommandString], cmd: &CommandString) -> bool {
    let mut i = 0;
    while i < set.len() {
        let (a, b) = (set[i].as_bytes(), cmd.as_bytes());
        let mut j = 0;
        let mut same = true;
        while j < 12 {
            if a[j] != b[j] {
                same = false;
            }
            j += 1;
        }
        if same {
            return true;
        }
        i += 1;
    }
    false
}

/// Builder for the table rows below: a row starts as a post-`verack` command that reads nothing
/// and gets its columns one by one. The two set columns come from Core's lists above.
impl CommandPolicy {
    /// A `PostVerack` row for `name` that reads nothing.
    const fn new(name: &str) -> Self {
        let cmd = CommandString::from_static(name);
        Self {
            cmd,
            gate: GateClass::PostVerack,
            pre_read: &[],
            read: ReadPlan::Nothing,
            on_throw: ThrowOutcome::NeverThrows,
            trailing: Trailing::Ignored,
            blocks_only_violation: in_set(&BLOCKS_ONLY_VIOLATIONS, &cmd),
            quorum_priority: in_set(&QUORUM_PRIORITY, &cmd),
        }
    }

    /// Sets the gate class.
    const fn gate(mut self, gate: GateClass) -> Self {
        self.gate = gate;
        self
    }

    /// Sets the effects before the first read.
    const fn pre(mut self, pre_read: &'static [PreRead]) -> Self {
        self.pre_read = pre_read;
        self
    }

    /// Sets the read; a failed read is then caught by `ProcessMessages` unless
    /// [`CommandPolicy::throws`] says otherwise.
    const fn reads(mut self, read: ReadPlan) -> Self {
        self.read = read;
        self.on_throw = ThrowOutcome::CaughtByProcessMessages;
        self
    }

    /// Sets what a failed read does.
    const fn throws(mut self, on_throw: ThrowOutcome) -> Self {
        self.on_throw = on_throw;
        self
    }

    /// Sets the trailing-byte rule.
    const fn trailing(mut self, trailing: Trailing) -> Self {
        self.trailing = trailing;
        self
    }
}

/// Core's `GetHeadersLimit`: the most headers a `headers` (`compressed` false) or `headers2`
/// message may count for the common `version`. 2000 (`MAX_HEADERS_UNCOMPRESSED_RESULT`), and 8000
/// (`MAX_HEADERS_COMPRESSED_RESULT`) for `headers2` from `INCREASE_MAX_HEADERS2_VERSION` (70235)
/// on. A count over it scores `Misbehaving(20)` before any header is read ([`ReadError::TooMany`]).
// Core: net_processing.cpp:1210-1216, :5323-5327; validation.h:82-83; version.h:50
pub const fn headers_limit(version: ProtocolVersion, compressed: bool) -> usize {
    if version.0 >= ProtocolVersion::INCREASE_MAX_HEADERS2.0 && compressed {
        MAX_HEADERS_COMPRESSED_RESULT
    } else {
        MAX_HEADERS_UNCOMPRESSED_RESULT
    }
}

/// `headers2`'s row: [`headers_limit`] as a table column.
const HEADERS2_READ: ReadPlan = ReadPlan::CountThenItemsByVersion {
    limit: MAX_HEADERS_UNCOMPRESSED_RESULT,
    raised_limit: MAX_HEADERS_COMPRESSED_RESULT,
    raised_from: ProtocolVersion::INCREASE_MAX_HEADERS2.0,
};

/// The `headers`/`headers2` effects before the count is read (v24 `net_processing.cpp:5311-5318`).
const HEADERS_PRE: &[PreRead] =
    &[PreRead::LoadingBlocksIgnore, PreRead::HeadersClearGetheadersTimestamp];

/// The CoinJoin server's reads (`dsa`, `dsi`, `dss`): masternode only, synced, session state.
const COINJOIN_SERVER_PRE: &[PreRead] =
    &[PreRead::MasternodeOnly, PreRead::ChainNotSynced, PreRead::CoinJoinServerSession];

/// The CoinJoin client's reads (`dssu`, `dsf`, `dsc`).
const COINJOIN_CLIENT_PRE: &[PreRead] = &[PreRead::CoinJoinWalletSessions];

/// The four DKG messages (`qcontrib`, `qcomplaint`, `qjustify`, `qpcommit`): intake checks before
/// the framing walk, whose failure scores +100 and which must consume the whole payload.
const DKG_PRE: &[PreRead] = &[PreRead::DkgIntake];

/// The DKG framing walk's catch (v24 `llmq/net_dkg.cpp:600-603`).
const DKG_THROW: ThrowOutcome = ThrowOutcome::Handler {
    score: 100,
    msg: "malformed DKG message",
};

/// The sig-share batches' catch: `BanNode` (+100) and rethrow (v24 `llmq/net_signing.cpp:66-146`).
const SIGSHARE_THROW: ThrowOutcome = ThrowOutcome::HandlerThenCaught {
    score: 100,
    msg: "",
};

/// Core's 74 message types in `allNetMessageTypes` order. Columns are BS4a's (DDE-SWEEP §4); each
/// row cites the v24.0.0-rc.3 branch. Core's handlers that live outside `net_processing.cpp` are
/// cited by their file. A command with no branch of its own (`merkleblock`, `cfilter`,
/// `cfheaders`, `cfcheckpt`) falls to the extension dispatch (`net_processing.cpp:5769-5825`),
/// which reads nothing for it.
static TABLE: [CommandPolicy; 74] = [
    // Core: net_processing.cpp:3935; a repeated `version` is gate 1, not a pre-read effect.
    CommandPolicy::new("version")
        .gate(GateClass::Version)
        .reads(ReadPlan::Custom)
        .throws(ThrowOutcome::VersionTail),
    // Core: net_processing.cpp:4187
    CommandPolicy::new("verack").gate(GateClass::BeforeOrAfterVerack),
    // Core: net_processing.cpp:4388
    CommandPolicy::new("addr").reads(ReadPlan::Whole),
    // Core: net_processing.cpp:4388
    CommandPolicy::new("addrv2").reads(ReadPlan::Whole),
    // Core: net_processing.cpp:4292
    CommandPolicy::new("sendaddrv2").gate(GateClass::BeforeOrAfterVerack),
    // Core: net_processing.cpp:4507
    CommandPolicy::new("inv").reads(ReadPlan::Whole),
    // Core: net_processing.cpp:4613
    CommandPolicy::new("getdata").reads(ReadPlan::Whole),
    // Core: no branch; extension dispatch, net_processing.cpp:5769
    CommandPolicy::new("merkleblock"),
    // Core: net_processing.cpp:4636
    CommandPolicy::new("getblocks").reads(ReadPlan::Whole),
    // Core: net_processing.cpp:4760 (int version, locator, stop hash)
    CommandPolicy::new("getheaders").reads(ReadPlan::Whole),
    // Core: net_processing.cpp:4861 (IBD return at :4865, before the read)
    CommandPolicy::new("tx").pre(&[PreRead::IbdReturn]).reads(ReadPlan::Whole),
    // Core: net_processing.cpp:5309 (loading-blocks :5311, clear :5318, count :5323)
    CommandPolicy::new("headers").pre(HEADERS_PRE).reads(ReadPlan::CountThenItems {
        limit: MAX_HEADERS_UNCOMPRESSED_RESULT,
    }),
    // Core: net_processing.cpp:5348
    CommandPolicy::new("block").pre(&[PreRead::LoadingBlocksIgnore]).reads(ReadPlan::Whole),
    // Core: net_processing.cpp:5389
    CommandPolicy::new("getaddr"),
    // Core: net_processing.cpp:5426
    CommandPolicy::new("mempool"),
    // Core: net_processing.cpp:5456
    CommandPolicy::new("ping").reads(ReadPlan::Whole),
    // Core: net_processing.cpp:5474; the nonce is read only when 8 bytes remain, so it never throws
    CommandPolicy::new("pong").reads(ReadPlan::Custom).throws(ThrowOutcome::NeverThrows),
    // Core: net_processing.cpp:5744
    CommandPolicy::new("notfound").reads(ReadPlan::Whole),
    // Core: net_processing.cpp:5529 (bloom check :5530, catch :5538)
    CommandPolicy::new("filterload")
        .pre(&[PreRead::NoBloomDisconnect])
        .reads(ReadPlan::Whole)
        .throws(ThrowOutcome::Handler {
            score: 100,
            msg: "misformatted bloom filter",
        }),
    // Core: net_processing.cpp:5560 (bloom check :5561, catch :5572)
    CommandPolicy::new("filteradd")
        .pre(&[PreRead::NoBloomDisconnect])
        .reads(ReadPlan::Whole)
        .throws(ThrowOutcome::Handler {
            score: 100,
            msg: "bad filteradd message",
        }),
    // Core: net_processing.cpp:5592; DDE-SWEEP lists it among the commands that read nothing,
    // with "no NODE_BLOOM: disconnect"; Core tests that before anything else, so it is a pre-read
    CommandPolicy::new("filterclear").pre(&[PreRead::NoBloomDisconnect]),
    // Core: net_processing.cpp:4261
    CommandPolicy::new("sendheaders").gate(GateClass::BeforeOrAfterVerack),
    // Core: net_processing.cpp:4273
    CommandPolicy::new("sendcmpct").gate(GateClass::BeforeOrAfterVerack).reads(ReadPlan::Whole),
    // Core: net_processing.cpp:5052
    CommandPolicy::new("cmpctblock").pre(&[PreRead::LoadingBlocksIgnore]).reads(ReadPlan::Whole),
    // Core: net_processing.cpp:4704
    CommandPolicy::new("getblocktxn").reads(ReadPlan::Whole),
    // Core: net_processing.cpp:5295
    CommandPolicy::new("blocktxn").pre(&[PreRead::LoadingBlocksIgnore]).reads(ReadPlan::Whole),
    // Core: net_processing.cpp:5638 (u8 type, u32 start, hash, before any check)
    CommandPolicy::new("getcfilters").reads(ReadPlan::Whole),
    // Core: no branch; extension dispatch, net_processing.cpp:5769
    CommandPolicy::new("cfilter"),
    // Core: net_processing.cpp:5643
    CommandPolicy::new("getcfheaders").reads(ReadPlan::Whole),
    // Core: no branch; extension dispatch, net_processing.cpp:5769
    CommandPolicy::new("cfheaders"),
    // Core: net_processing.cpp:5648 (u8 type, hash)
    CommandPolicy::new("getcfcheckpt").reads(ReadPlan::Whole),
    // Core: no branch; extension dispatch, net_processing.cpp:5769
    CommandPolicy::new("cfcheckpt"),
    // Core: net_processing.cpp:4310 (the reads, a u32 and a u64, follow the txreconciliation check)
    CommandPolicy::new("sendtxrcncl")
        .gate(GateClass::BeforeOrAfterVerack)
        .pre(&[PreRead::TxReconciliationOff])
        .reads(ReadPlan::Whole),
    // Core: net_processing.cpp:5689 (catch :5693)
    CommandPolicy::new("spork").reads(ReadPlan::Whole).throws(ThrowOutcome::Handler {
        score: 100,
        msg: "malformed spork received",
    }),
    // Core: net_processing.cpp:5713
    CommandPolicy::new("getsporks"),
    // Core: net_processing.cpp:4485
    CommandPolicy::new("senddsq").reads(ReadPlan::Whole),
    // Core: coinjoin/server.cpp:54-56 (synced check :54)
    CommandPolicy::new("dsa").pre(COINJOIN_SERVER_PRE).reads(ReadPlan::Whole),
    // Core: coinjoin/server.cpp:60 (DSVIN)
    CommandPolicy::new("dsi").pre(COINJOIN_SERVER_PRE).reads(ReadPlan::Whole),
    // Core: coinjoin/client.cpp:75, :110 (DSFINALTX)
    CommandPolicy::new("dsf").pre(COINJOIN_CLIENT_PRE).reads(ReadPlan::Whole),
    // Core: coinjoin/server.cpp:62 (DSSIGNFINALTX); above 180 inputs replies ERR_MAXIMUM, no throw
    CommandPolicy::new("dss").pre(COINJOIN_SERVER_PRE).reads(ReadPlan::CountThenItems {
        limit: 180,
    }),
    // Core: coinjoin/client.cpp:75, :124 (DSCOMPLETE)
    CommandPolicy::new("dsc").pre(COINJOIN_CLIENT_PRE).reads(ReadPlan::Whole),
    // Core: coinjoin/client.cpp:75, :105 (DSSTATUSUPDATE)
    CommandPolicy::new("dssu").pre(COINJOIN_CLIENT_PRE).reads(ReadPlan::Whole),
    // Core: net_processing.cpp:4861 (IBD return :4865), dstx read :4874
    CommandPolicy::new("dstx").pre(&[PreRead::IbdReturn]).reads(ReadPlan::Whole),
    // Core: coinjoin/server.cpp:58, coinjoin/walletman.cpp:248 (DSQUEUE)
    CommandPolicy::new("dsq")
        .pre(&[PreRead::CoinJoinMasternodeOrWallet, PreRead::ChainNotSynced])
        .reads(ReadPlan::Whole),
    // Core: node/sync_manager.cpp:333 (SYNCSTATUSCOUNT; synced return :336)
    CommandPolicy::new("ssc").pre(&[PreRead::MnSyncFinished]).reads(ReadPlan::Whole),
    // Core: governance/net_governance.cpp:86 (gates :82-83, :90); the uint256 is read outside the
    // catch, the bloom filter inside it (:95-100)
    CommandPolicy::new("govsync")
        .pre(&[PreRead::GovernanceUnavailable, PreRead::GovernanceNotSynced])
        .reads(ReadPlan::Custom)
        .throws(ThrowOutcome::HandlerAfterHead {
            head_bytes: 32,
            score: 100,
            msg: "misformatted govsync bloom filter",
        }),
    // Core: governance/net_governance.cpp:171 (gates :82-83)
    CommandPolicy::new("govobj").pre(&[PreRead::GovernanceUnavailable]).reads(ReadPlan::Whole),
    // Core: governance/net_governance.cpp:202 (catch :207)
    CommandPolicy::new("govobjvote")
        .pre(&[PreRead::GovernanceUnavailable])
        .reads(ReadPlan::Whole)
        .throws(ThrowOutcome::Handler {
            score: 100,
            msg: "malformed governance vote",
        }),
    // Core: net_processing.cpp:5611
    CommandPolicy::new("getmnlistd").reads(ReadPlan::Whole),
    // Core: net_processing.cpp:5654 (unsolicited, +100, nothing read)
    CommandPolicy::new("mnlistdiff"),
    // Core: net_processing.cpp:4500
    CommandPolicy::new("qsendrecsigs").reads(ReadPlan::Whole),
    // Core: llmq/blockprocessor.cpp:102
    CommandPolicy::new("qfcommit").reads(ReadPlan::Whole),
    // Core: llmq/net_dkg.cpp:492-604
    CommandPolicy::new("qcontrib")
        .pre(DKG_PRE)
        .reads(ReadPlan::Custom)
        .throws(DKG_THROW)
        .trailing(Trailing::WholePayload),
    // Core: llmq/net_dkg.cpp:492-604
    CommandPolicy::new("qcomplaint")
        .pre(DKG_PRE)
        .reads(ReadPlan::Custom)
        .throws(DKG_THROW)
        .trailing(Trailing::WholePayload),
    // Core: llmq/net_dkg.cpp:492-604 (QJUSTIFICATION)
    CommandPolicy::new("qjustify")
        .pre(DKG_PRE)
        .reads(ReadPlan::Custom)
        .throws(DKG_THROW)
        .trailing(Trailing::WholePayload),
    // Core: llmq/net_dkg.cpp:492-604 (QPCOMMITMENT)
    CommandPolicy::new("qpcommit")
        .pre(DKG_PRE)
        .reads(ReadPlan::Custom)
        .throws(DKG_THROW)
        .trailing(Trailing::WholePayload),
    // Core: nothing is read. A plain node's NetDKGStub scores +10 whatever SPORK_17 says
    // (init.cpp:2235, llmq/net_dkg.cpp:938-944). NetDKG (llmq/net_dkg.cpp:491-507): SPORK_17 off
    // ignores it; then an observer scores +10 and a masternode sets `pfrom.qwatch`.
    CommandPolicy::new("qwatch"),
    // Core: llmq/net_signing.cpp:97
    CommandPolicy::new("qsigsesann")
        .pre(&[PreRead::MasternodeOnly])
        .reads(ReadPlan::Custom)
        .throws(SIGSHARE_THROW),
    // Core: llmq/net_signing.cpp:115 (QSIGSHARESINV)
    CommandPolicy::new("qsigsinv")
        .pre(&[PreRead::MasternodeOnly])
        .reads(ReadPlan::Custom)
        .throws(SIGSHARE_THROW),
    // Core: llmq/net_signing.cpp:115 (QGETSIGSHARES)
    CommandPolicy::new("qgetsigs")
        .pre(&[PreRead::MasternodeOnly])
        .reads(ReadPlan::Custom)
        .throws(SIGSHARE_THROW),
    // Core: llmq/net_signing.cpp:133 (QBSIGSHARES)
    CommandPolicy::new("qbsigs")
        .pre(&[PreRead::MasternodeOnly])
        .reads(ReadPlan::Custom)
        .throws(SIGSHARE_THROW),
    // Core: llmq/net_signing.cpp:49; read on every node, before the masternode check at :64
    CommandPolicy::new("qsigrec").reads(ReadPlan::Whole),
    // Core: llmq/net_signing.cpp:66 (QSIGSHARE; SPORK_21 and the masternode check)
    CommandPolicy::new("qsigshare")
        .pre(&[PreRead::MasternodeOnly, PreRead::Spork21Off])
        .reads(ReadPlan::Custom)
        .throws(SIGSHARE_THROW),
    // Core: llmq/net_quorum.cpp:79; the length check is :90 and precedes the read at :94
    CommandPolicy::new("qgetdata")
        .pre(&[PreRead::QuorumDataPeerCheck])
        .reads(ReadPlan::Whole)
        .trailing(Trailing::PayloadAtMost(67)),
    // Core: llmq/net_quorum.cpp:204
    CommandPolicy::new("qdata").pre(&[PreRead::QuorumDataPeerCheck]).reads(ReadPlan::Custom),
    // Core: net_processing.cpp:5794 (ChainLocks check :5795)
    CommandPolicy::new("clsig").pre(&[PreRead::ChainLocksDisabled]).reads(ReadPlan::Whole),
    // Core: instantsend/net_instantsend.cpp:239 (enabled check :243)
    CommandPolicy::new("isdlock").pre(&[PreRead::InstantSendDisabled]).reads(ReadPlan::Whole),
    // Core: evo/mnauth.cpp:57
    CommandPolicy::new("mnauth").pre(&[PreRead::ChainNotSynced]).reads(ReadPlan::Whole),
    // Core: net_processing.cpp:4760
    CommandPolicy::new("getheaders2").reads(ReadPlan::Whole),
    // Core: net_processing.cpp:4267
    CommandPolicy::new("sendheaders2").gate(GateClass::BeforeOrAfterVerack),
    // Core: net_processing.cpp:5309 (count limit :5324 via GetHeadersLimit :1210)
    CommandPolicy::new("headers2").pre(HEADERS_PRE).reads(HEADERS2_READ),
    // Core: net_processing.cpp:5660 (catch :5664, v24 only; v23 logs)
    CommandPolicy::new("getqrinfo").reads(ReadPlan::Whole).throws(ThrowOutcome::Handler {
        score: 100,
        msg: "malformed getqrinfo received",
    }),
    // Core: net_processing.cpp:5739 (unsolicited, +100, nothing read)
    CommandPolicy::new("qrinfo"),
    // Core: net_processing.cpp:3841 (ProcessPlatformBanMessage; synced check :3844)
    CommandPolicy::new("platformban").pre(&[PreRead::ChainNotSynced]).reads(ReadPlan::Whole),
];

/// Core's 74 message types (`allNetMessageTypes`, `protocol.cpp:99-176`) with their policy.
pub fn table() -> &'static [CommandPolicy] {
    &TABLE
}

/// The policy row of `cmd`, or `None` for a type Core does not list (`"Unknown command"`).
pub fn lookup(cmd: &CommandString) -> Option<&'static CommandPolicy> {
    TABLE.iter().find(|row| row.cmd == *cmd)
}

/// `DataStream::read()` past the end (v24 `streams.h:287`).
const END_OF_DATA: &str = "DataStream::read(): end of data";

/// `typeid(std::ios_base::failure).name()` with libstdc++'s C++11 ABI (the Itanium mangling Core's
/// log line prints).
const IOS_FAILURE_TYPE: &str = "NSt8ios_base7failureB5cxx11E";

/// What libstdc++ appends to an `ios_base::failure`'s message (`io_errc::stream`).
const IOSTREAM_ERROR: &str = ": iostream error";

/// A read that failed where Core's stream read throws (P2a §3.2.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Throw {
    /// Core's `e.what()`: `"DataStream::read(): end of data: iostream error"`, …
    pub what: Cow<'static, str>,
    /// Core's `typeid(e).name()`: `"NSt8ios_base7failureB5cxx11E"` for a stream error.
    pub type_name: &'static str,
}

impl Throw {
    /// A `std::ios_base::failure` thrown with `msg`: libstdc++'s `what()` is
    /// `"<msg>: iostream error"`.
    pub fn ios_failure(msg: &str) -> Self {
        Self {
            what: Cow::Owned(format!("{msg}{IOSTREAM_ERROR}")),
            type_name: IOS_FAILURE_TYPE,
        }
    }

    /// Core's text for a base-sdk decode error.
    ///
    /// The stream errors Core throws map as: running out of bytes is `DataStream::read(): end of
    /// data` (v24 `streams.h:287`), a non-minimal CompactSize is `non-canonical
    /// ReadCompactSize()` and one above `MAX_SIZE` is `ReadCompactSize(): size too large` (v24
    /// `serialize.h:304-319`). A CompactSize over any other bound maps to the end-of-data text.
    /// Mostly that bound is the bytes left (`CompactSize::into_len`), where Core's element reads
    /// run out of bytes too. It is also a `LIMITED_*` bound (`String length limit exceeded`,
    /// `:781`; `Vector length limit exceeded`, `serialize.h:1030`, which [`read`] and [`decode`]
    /// give in Core's words for the vectors [`limited_vector`] lists), which the commands with
    /// such bounds catch in their handler and only score with its text, and a type bound of
    /// base-sdk's own: `getblocktxn`'s index over 65535 (Core: `differential value overflow`,
    /// `blockencodings.h` `DifferenceFormatter`) and `cmpctblock`'s prefilled index over 65535
    /// (Core: `CompactSize exceeds limit of type`, `serialize.h:722`). Those texts differ from
    /// Core's (`ProcessMessages` logs them; the verdict is the same). A `VersionTail` is
    /// classified by its error text, as it is the only error that carries one. Everything else
    /// keeps base-sdk's own text, as a stream error: an error with no Core equivalent (a
    /// frame-level error never reaches a handler as a throw, and [`read`] returns the handler
    /// refusals as [`ReadError`]), and a throw base-sdk words otherwise, such as `cmpctblock`'s
    /// over 65535 transactions (Core: `indexes overflowed 16 bits`, `blockencodings.h:121`).
    // Core: serialize.h:304-319, :722, :781, :1030; streams.h:287; blockencodings.h:30-39, :121
    pub fn from_decode(err: &P2pDecodeError) -> Self {
        let core = match err {
            P2pDecodeError::Consensus(e) => core_text(e),
            P2pDecodeError::VersionTail {
                error,
                ..
            } => core_text_of_display(error),
            _ => None,
        };
        match core {
            Some(msg) => Self::ios_failure(msg),
            None => Self::ios_failure(&err.to_string()),
        }
    }

    /// Core's `ProcessMessages` line: `"%s(%s, %u bytes): Exception '%s' (%s) caught"`, with
    /// `__func__` = `ProcessMessages` and the command passed through `SanitizeString`. Core's
    /// format ends in `"\n"`, which the logger here adds.
    // Core: net_processing.cpp:5941 ProcessMessages
    pub fn log_line(&self, cmd: &CommandString, size: usize) -> String {
        format!(
            "ProcessMessages({}, {} bytes): Exception '{}' ({}) caught",
            sanitize_string(cmd),
            size,
            self.what,
            self.type_name
        )
    }
}

/// Core's text for a base-sdk `DecodeError`, where Core's stream read throws.
fn core_text(err: &DecodeError) -> Option<&'static str> {
    match err {
        DecodeError::Eof {
            ..
        } => Some(END_OF_DATA),
        DecodeError::NonMinimalCompactSize {
            ..
        } => Some("non-canonical ReadCompactSize()"),
        DecodeError::CompactSizeExceedsLimit {
            limit,
            ..
        } => Some(compact_size_text(*limit as u64)),
        DecodeError::BadLen {
            ..
        }
        | DecodeError::DecError(_)
        | DecodeError::InvalidUtf8
        | DecodeError::InvalidValue {
            ..
        }
        | DecodeError::MalleableBls
        | DecodeError::NonCanonicalBool {
            ..
        }
        | DecodeError::TrailingBytes {
            ..
        } => None,
    }
}

/// [`core_text`] for an error that only exists as its `Display` text (`VersionTail`).
fn core_text_of_display(text: &str) -> Option<&'static str> {
    if text.starts_with("unexpected eof") {
        return Some(END_OF_DATA);
    }
    if text.starts_with("non-minimal compact size") {
        return Some("non-canonical ReadCompactSize()");
    }
    text.strip_prefix("compact size value ")
        .and_then(|rest| rest.split_once(" exceeds limit "))
        .and_then(|(_, limit)| limit.parse::<u64>().ok())
        .map(compact_size_text)
}

/// A CompactSize over `limit`: `MAX_SIZE` is Core's `ReadCompactSize` range check, any other
/// bound is the bytes left, which Core's element reads then run out of.
fn compact_size_text(limit: u64) -> &'static str {
    if limit == MAX_SIZE {
        "ReadCompactSize(): size too large"
    } else {
        END_OF_DATA
    }
}

/// Core's `SanitizeString(msg.m_type)` (`SAFE_CHARS_DEFAULT`, `util/strencodings.cpp:16-25`): the
/// type up to its first NUL, keeping only letters, digits and ` .,;-_/:?@()`.
fn sanitize_string(cmd: &CommandString) -> String {
    cmd.as_bytes()
        .iter()
        .take_while(|&&b| b != 0)
        .filter(|&&b| b.is_ascii_alphanumeric() || b" .,;-_/:?@()".contains(&b))
        .map(|&b| char::from(b))
        .collect()
}

/// Where `cmd`'s payload holds a C++ `bool` that the pinned base-sdk reads as 0 or 1 only, while
/// Core reads any byte and takes a non-zero one as `true` (`serialize.h:239`; BSX A-12, kept
/// strict in the fork until BL-82). Only rows the node reads are listed: `qrinfo` and
/// `mnlistdiff` read nothing (D37).
// Core: net_processing.cpp:4274 (sendcmpct: `vRecv >> fAnnounceUsingCMPCTBLOCK`);
// llmq/snapshot.h:111 (CGetQuorumRotationInfo: baseBlockHashes, blockRequestHash, extraShare)
fn core_bool_offset(cmd: &CommandString, payload: &[u8]) -> Option<usize> {
    match cmd.as_str() {
        "sendcmpct" => Some(0),
        "getqrinfo" => {
            let (count, at) = count_at(payload, 0)?;
            at.checked_add(usize::try_from(count).ok()?.checked_mul(32)?)?.checked_add(32)
        }
        _ => None,
    }
}

/// `payload` with the C++ `bool` [`core_bool_offset`] finds rewritten to 1 when it is non-zero,
/// so the decoder reads it as Core does (DEC-178 (2)). Borrowed when there is nothing to rewrite.
pub fn core_bools<'a>(cmd: &CommandString, payload: &'a [u8]) -> Cow<'a, [u8]> {
    match core_bool_offset(cmd, payload) {
        Some(at) if payload.get(at).is_some_and(|&b| b > 1) => {
            let mut owned = payload.to_vec();
            owned[at] = 1;
            Cow::Owned(owned)
        }
        _ => Cow::Borrowed(payload),
    }
}

/// Decodes `cmd`'s payload as its handler reads it (`P2pMsg::decode_with_policy`), and as Core
/// reads it where the pinned base-sdk reads otherwise (DEC-178, D37; [`read_as_core`]). The bytes
/// consumed are the payload's.
pub fn decode(
    cmd: &CommandString,
    payload: &[u8],
    ctx: &ProtoCtx,
) -> Result<DecodedMsg, P2pDecodeError> {
    read_as_core(cmd, payload, |p| {
        P2pMsg::decode_with_policy(cmd, p, ctx).map(|d| (d.msg, d.consumed))
    })
    .map(|(msg, consumed)| DecodedMsg {
        msg,
        consumed,
    })
    .map_err(|f| f.error)
}

/// Reads `T` from the front of `payload` at Core's read point, under the context built now
/// (P2a §3.2.5: never cached across messages), as [`decode`] reads it but without its transport
/// checks (the frame limit and the command), which the transport has made. The types mapped here
/// are the commands [`read_as_core`] reads Core's way; a command added there is added here.
/// `filterload` and `filteradd` read as [`FilterLoad`] and [`FilterAdd`], whose `vData` count
/// [`limited_vector`] checks.
/// `govsync` reads as [`GovSync`] or [`GovSyncRead`], both the whole filter.
/// `read::<Headers2>` is not hash-once: use [`read_headers2`].
///
/// A handler that checks the payload before its read and returns, scoring, gives that refusal
/// and not a [`Throw`]: a `headers` or `headers2` count over [`headers_limit`] is
/// [`ReadError::TooMany`], and a `qgetdata` request
/// ([`QuorumDataRequest`](dash_p2p_core::QuorumDataRequest)) over its 67 bytes is
/// [`ReadError::Oversized`]; [`ReadError::misbehaving`] gives Core's score and reason. On any
/// error `payload` is left as it was; every error ends the message.
// Core: net_processing.cpp:5323-5327 (headers); llmq/net_quorum.cpp:90-93 (qgetdata)
pub fn read<T: MsgDecode + 'static>(payload: &mut &[u8], ctx: &ProtoCtx) -> Result<T, ReadError> {
    let reader = |mut p: &[u8]| {
        let len = p.len();
        T::decode_msg(&mut p, ctx).map(|t| (t, len - p.len()))
    };
    let core_reads = [
        (TypeId::of::<SendCmpct>(), CommandString::SENDCMPCT),
        (TypeId::of::<GetQrInfo>(), CommandString::GETQRINFO),
        (TypeId::of::<GovSync>(), CommandString::GOVSYNC),
        (TypeId::of::<GovSyncRead>(), CommandString::GOVSYNC),
        (TypeId::of::<GovVote>(), CommandString::GOVOBJVOTE),
        (TypeId::of::<Headers2>(), CommandString::HEADERS2),
        (TypeId::of::<FilterLoad>(), CommandString::FILTERLOAD),
        (TypeId::of::<FilterAdd>(), CommandString::FILTERADD),
    ];
    let (msg, consumed) = match core_reads.iter().find(|(t, _)| *t == TypeId::of::<T>()) {
        Some((_, cmd)) => read_as_core(cmd, payload, reader).map_err(|f| f.read_error())?,
        None => reader(payload).map_err(|e| ReadError::from_decode(&e))?,
    };
    *payload = &payload[consumed..];
    Ok(msg)
}

/// `govsync` as Core reads it: base-sdk's [`GovSync`] (the object hash and the whole filter),
/// with the filter's last three fields copied out of [`GovSync::bloom_filter`] (base-sdk reads
/// them since 8382b76; BSX A-17 is closed). Read it with [`read`], not by offset into the
/// payload (DEC-178).
// Core: governance/net_governance.cpp:93-100; common/bloom.h:50-53, :78 (CBloomFilter's fields
// and their serialization)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GovSyncRead {
    /// The object hash and the filter.
    pub sync: GovSync,
    /// The filter's `nHashFuncs`.
    pub hash_funcs: u32,
    /// The filter's `nTweak`.
    pub tweak: u32,
    /// The filter's `nFlags`.
    pub flags: u8,
}

impl From<GovSync> for GovSyncRead {
    fn from(sync: GovSync) -> Self {
        let f = &sync.bloom_filter;
        let (hash_funcs, tweak, flags) = (f.hash_funcs, f.tweak, f.flags);
        Self {
            sync,
            hash_funcs,
            tweak,
            flags,
        }
    }
}

impl BaseCodec for GovSyncRead {
    fn decode(data: &mut &[u8]) -> Result<Self, DecodeError> {
        GovSync::decode(data).map(Self::from)
    }

    fn encode(&self, buf: &mut impl EncodeBuf) {
        self.sync.encode(buf);
    }
}

impl MsgDecode for GovSyncRead {
    fn decode_msg(data: &mut &[u8], ctx: &ProtoCtx) -> Result<Self, P2pDecodeError> {
        GovSync::decode_msg(data, ctx).map(Self::from)
    }
}

/// Reads a `headers2` payload from the front of `payload` as Core's handler does, in one pass of
/// Core's `Uncompress`, hashing through `hash` exactly where Core does: once per header whose
/// previous block hash is left off (the hash of the header before it), in order, and only for
/// headers read before a failure. The processor pairs each header with its hash there (P2a's
/// hash-once rule, DEC-51a). Headers are read until the count is met or the bytes run out, as
/// Core reads them. A count over [`headers_limit`] for `ctx` reads nothing and is
/// [`ReadError::TooMany`], not a throw ([`ReadError::Oversized`] never comes from here). Nothing
/// carries across messages: the version list is the message's (PQ-3). The headers are the ones
/// [`decode`] gives (D37).
///
/// On success `payload` is advanced past the message. On either error it is left as it was, as
/// [`read`] leaves it (Core's stream has consumed the count and the headers read; both end the
/// message), and the hook calls already made for headers read before a throw stay made.
// Core: net_processing.cpp:5323-5343; primitives/block.cpp:37-55, :95-134 (Uncompress)
pub fn read_headers2(
    payload: &mut &[u8],
    ctx: &ProtoCtx,
    hash: impl FnMut(&BlockHeader) -> BlockHash,
) -> Result<Vec<BlockHeader>, ReadError> {
    let mut data = *payload;
    let count = headers2_count(&mut data, ctx)?;
    let headers = uncompress_headers2(&mut data, count, hash).map_err(ReadError::from)?;
    *payload = data;
    Ok(headers)
}

/// Why [`read`] or [`read_headers2`] gave no value: Core's read throws, or the handler refuses
/// the message before reading it and scores, throwing nothing. Every variant ends the message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadError {
    /// `headers`/`headers2`: the count is over [`headers_limit`]. Core's handler scores
    /// `Misbehaving(20, "headers message size = <count>")` and returns, reading no header.
    // Core: net_processing.cpp:5323-5327
    TooMany {
        /// The count read.
        count: u64,
        /// [`headers_limit`] for the version.
        max: u64,
    },
    /// `qgetdata`: the payload is longer than the request's 67 bytes. Core's handler scores
    /// `PeerMisbehaving(100, "oversized qgetdata")` and returns, before the read.
    // Core: llmq/net_quorum.cpp:90-93
    Oversized {
        /// The payload's size.
        size: usize,
        /// The request's size.
        max: usize,
    },
    /// Core's read throws; the processor logs [`Throw::log_line`], as `ProcessMessages` does.
    Throw(Throw),
}

impl ReadError {
    /// Core's `Misbehaving` score and reason for a handler refusal; `None` for a throw.
    pub fn misbehaving(&self) -> Option<(i32, String)> {
        match self {
            Self::TooMany {
                count,
                ..
            } => Some((20, format!("headers message size = {count}"))),
            Self::Oversized {
                ..
            } => Some((100, "oversized qgetdata".to_owned())),
            Self::Throw(_) => None,
        }
    }

    /// base-sdk's handler refusals for the rows that score them, else Core's throw.
    fn from_decode(err: &P2pDecodeError) -> Self {
        match *err {
            P2pDecodeError::TooManyEntries {
                command: "headers" | "headers2",
                count,
                max,
            } => Self::TooMany {
                count,
                max,
            },
            P2pDecodeError::HandlerPayloadTooLarge {
                command: "qgetdata",
                size,
                max,
            } => Self::Oversized {
                size,
                max,
            },
            _ => Self::Throw(Throw::from_decode(err)),
        }
    }
}

impl From<DecodeError> for ReadError {
    fn from(e: DecodeError) -> Self {
        Self::Throw(Throw::from_decode(&P2pDecodeError::Consensus(e)))
    }
}

/// `headers2`'s count (`ReadCompactSize`), refused over [`headers_limit`] for `ctx` but not for
/// the bytes left: Core reads headers until they run out.
fn headers2_count(data: &mut &[u8], ctx: &ProtoCtx) -> Result<usize, ReadError> {
    let max = headers_limit(ctx.version, true) as u64;
    let count = u64::from(CompactSize::decode_size(data)?);
    if count > max {
        return Err(ReadError::TooMany {
            count,
            max,
        });
    }
    Ok(count as usize)
}

/// A read that failed at the decode boundary: the error [`decode`] returns, and Core's exception
/// text where Core's read throws one that base-sdk's read does not make.
struct Failure {
    error: P2pDecodeError,
    what: Option<&'static str>,
}

impl From<P2pDecodeError> for Failure {
    fn from(error: P2pDecodeError) -> Self {
        Self {
            error,
            what: None,
        }
    }
}

impl Failure {
    /// Core's throw `what`, with `error` standing for it in [`decode`].
    fn core(error: DecodeError, what: &'static str) -> Self {
        Self {
            error: P2pDecodeError::Consensus(error),
            what: Some(what),
        }
    }

    /// Core's exception, or the handler's refusal ([`ReadError::from_decode`]).
    fn read_error(&self) -> ReadError {
        match self.what {
            Some(what) => ReadError::Throw(Throw::ios_failure(what)),
            None => ReadError::from_decode(&self.error),
        }
    }
}

/// Reads `cmd`'s payload with `read` (bytes in; the value and the bytes it read out) as Core
/// reads it where the pinned base-sdk reads otherwise: the bools [`core_bools`] rewrites, the
/// `LIMITED_VECTOR` counts [`limited_vector`] checks, the governance checks [`gov_rest`] adds,
/// and `headers2` headers that only Core's `Uncompress` takes ([`headers2_as_whole`]). Returns
/// the value and the payload bytes Core reads.
fn read_as_core<T>(
    cmd: &CommandString,
    payload: &[u8],
    read: impl Fn(&[u8]) -> Result<(T, usize), P2pDecodeError>,
) -> Result<(T, usize), Failure> {
    limited_vector(cmd, payload)?;
    let (msg, consumed) = match read(&core_bools(cmd, payload)) {
        // Only a header the strict uncompress refuses: the count has passed.
        Err(P2pDecodeError::Consensus(DecodeError::InvalidValue {
            ..
        })) if *cmd == CommandString::HEADERS2 => {
            let (whole, consumed) =
                headers2_as_whole(payload).map_err(P2pDecodeError::Consensus)?;
            (read(&whole)?.0, consumed)
        }
        // base-sdk refuses a vote signature of the wrong size itself, in its own words; Core's
        // throw is the one [`gov_rest`] gives.
        Err(
            e @ P2pDecodeError::InvalidValue {
                ..
            },
        ) if *cmd == CommandString::GOVOBJVOTE => {
            gov_rest(cmd, payload, 0)?;
            return Err(e.into());
        }
        r => r?,
    };
    Ok((msg, gov_rest(cmd, payload, consumed)?))
}

/// The CompactSize at `at` in `payload` and the offset after it; `None` where there is none.
fn count_at(payload: &[u8], at: usize) -> Option<(u64, usize)> {
    let mut rest = payload.get(at..)?;
    let n = CompactSize::decode_size(&mut rest).ok()?;
    Some((u64::from(n), payload.len() - rest.len()))
}

/// Where a `CGovernanceVote`'s signature starts: the outpoint (36 bytes), the parent hash (32),
/// `nVoteOutcome` and `nVoteSignal` (4 each) and `nTime` (8).
const GOV_VOTE_SIG_AT: usize = 84;
/// `BLS_SIG_SIZE`, the vote signature bound on a network read.
const GOV_VOTE_SIG_MAX: u64 = 96;
/// `COMPACT_SIG_SIZE` and `BLS_SIG_SIZE`, the vote signature sizes Core takes.
const GOV_VOTE_SIG_SIZES: [usize; 2] = [65, 96];
/// `CBloomFilter`'s `nHashFuncs`, `nTweak` and `nFlags`, after `vData`.
const BLOOM_TAIL: usize = 9;

/// The `LIMITED_VECTOR` count Core checks before reading a message's vector, a throw base-sdk's
/// read makes in other words (its own count bound, which [`Throw::from_decode`] maps to the
/// end-of-data text): a bloom filter's `vData` in `filterload` and in `govsync` (after `nProp`),
/// `filteradd`'s data and a governance vote's signature. A count over the bound is refused before
/// any element is read, with Core's `Vector length limit exceeded`; [`decode`] returns it as
/// base-sdk's count bound. Every one is caught by its handler (+100).
// Core: common/bloom.h:78 (CBloomFilter); net_processing.cpp:5537, :5571 (filterload, filteradd);
// governance/vote.h:193-203 (network read); governance/net_governance.cpp:93-100;
// serialize.h:1030 (LimitedVectorFormatter)
fn limited_vector(cmd: &CommandString, payload: &[u8]) -> Result<(), Failure> {
    let (at, limit) = match cmd.as_str() {
        "filterload" => (0, MAX_BLOOM_FILTER_SIZE as u64),
        "filteradd" => (0, MAX_SCRIPT_ELEMENT_SIZE as u64),
        "govsync" => (32, MAX_BLOOM_FILTER_SIZE as u64),
        "govobjvote" => (GOV_VOTE_SIG_AT, GOV_VOTE_SIG_MAX),
        _ => return Ok(()),
    };
    match count_at(payload, at) {
        Some((value, _)) if value > limit => Err(Failure::core(
            DecodeError::CompactSizeExceedsLimit {
                limit: usize::try_from(limit).unwrap_or(usize::MAX),
                value,
            },
            "Vector length limit exceeded",
        )),
        _ => Ok(()),
    }
}

/// The rest of Core's governance checks after base-sdk's read of `consumed` bytes, in Core's
/// words: the vote signature's size (base-sdk's `GovVote` refuses it too, in its own words) and
/// the bloom filter's last three fields (base-sdk's `GovSync` reads them since 8382b76; this
/// keeps the throw's text and its order after `vData`). Returns Core's bytes read.
fn gov_rest(cmd: &CommandString, payload: &[u8], consumed: usize) -> Result<usize, Failure> {
    match cmd.as_str() {
        "govobjvote" => {
            // base-sdk has read the signature, so its size is at most 96 here.
            let len = count_at(payload, GOV_VOTE_SIG_AT)
                .map_or(0, |(n, _)| usize::try_from(n).unwrap_or(usize::MAX));
            if GOV_VOTE_SIG_SIZES.contains(&len) {
                Ok(consumed)
            } else {
                Err(Failure::core(
                    DecodeError::BadLen {
                        expected: GOV_VOTE_SIG_SIZES.to_vec(),
                        actual: len,
                    },
                    "bad governance vote signature size",
                ))
            }
        }
        "govsync" => {
            // After the filter's `vData`, whether or not the reader went on into the tail
            // ([`GovSyncRead`] does): the hash (32 bytes), its count and its bytes.
            let vdata_end = count_at(payload, 32).map_or(consumed, |(n, at)| {
                at.saturating_add(usize::try_from(n).unwrap_or(usize::MAX))
            });
            let remaining = payload.len() - vdata_end.min(payload.len());
            if remaining < BLOOM_TAIL {
                return Err(P2pDecodeError::Consensus(DecodeError::Eof {
                    needed: BLOOM_TAIL,
                    remaining,
                })
                .into());
            }
            Ok(vdata_end + BLOOM_TAIL)
        }
        _ => Ok(consumed),
    }
}

/// `CompressedHeaderBitField` flags: the previous block hash, the timestamp and `nBits` are on
/// the wire.
const H2_PREV_HASH: u8 = 0b0000_1000;
const H2_TIMESTAMP: u8 = 0b0001_0000;
const H2_NBITS: u8 = 0b0010_0000;
/// The `last_unique_versions` bound.
const H2_MAX_VERSIONS: usize = 7;

/// Core's read of a `headers2` payload whose count base-sdk has accepted but one of whose headers
/// its strict uncompress refuses (BSX D-07, b-12): a first header with fields left off keeps them
/// zero (and its version, zero when left off, joins the list), and a version offset past the
/// list leaves the version zero. Returns the payload with every header whole (all fields on the
/// wire, so none decodes `is_prev_hash_derived`) and the bytes Core reads.
// Core: primitives/block.cpp:37-55 (the version list), :95-134 (Uncompress);
// primitives/block.h:166-185 (CompressibleBlockHeader's read); net_processing.cpp:5335-5343
fn headers2_as_whole(payload: &[u8]) -> Result<(Vec<u8>, usize), DecodeError> {
    let mut data = payload;
    // base-sdk has checked the count against the limit for the version.
    let count = usize::try_from(u64::from(CompactSize::decode_size(&mut data)?))
        .expect("a count base-sdk accepted");
    let headers = uncompress_headers2(&mut data, count, |h| h.hash())?;
    let mut whole = Vec::new();
    CompactSize::from(count).encode(&mut whole);
    for h in headers {
        whole.push(H2_PREV_HASH | H2_TIMESTAMP | H2_NBITS);
        h.encode(&mut whole);
    }
    Ok((whole, payload.len() - data.len()))
}

/// Core's handler loop over `count` compressed headers: reads each, then `Uncompress`es it
/// against the one before, calling `hash` on that one for a derived previous block hash.
// Core: net_processing.cpp:5335-5343; primitives/block.cpp:37-55, :95-134
fn uncompress_headers2(
    data: &mut &[u8],
    count: usize,
    mut hash: impl FnMut(&BlockHeader) -> BlockHash,
) -> Result<Vec<BlockHeader>, DecodeError> {
    let mut versions: VecDeque<i32> = VecDeque::with_capacity(H2_MAX_VERSIONS + 1);
    // Core: SaveVersionAsMostRecent
    fn save(versions: &mut VecDeque<i32>, v: i32) {
        versions.push_front(v);
        versions.truncate(H2_MAX_VERSIONS);
    }
    let mut headers: Vec<BlockHeader> = Vec::new();
    for _ in 0..count {
        let c = CompressedHeader::decode(data)?;
        let mut h = *c.header();
        match headers.last() {
            None => save(&mut versions, h.version),
            Some(last) => {
                let offset = usize::from(c.version_offset());
                if offset == 0 {
                    save(&mut versions, h.version);
                } else if let Some(v) = versions.remove(offset - 1) {
                    h.version = v;
                    versions.push_front(v);
                }
                if c.flags() & H2_PREV_HASH == 0 {
                    h.prev_hash = hash(last);
                }
                if c.flags() & H2_TIMESTAMP == 0 {
                    // `uint32_t + int16_t`: modulo 2^32.
                    h.time = last.time.wrapping_add(i32::from(c.time_offset()) as u32);
                }
                if c.flags() & H2_NBITS == 0 {
                    h.bits = last.bits;
                }
            }
        }
        headers.push(h);
    }
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use dash_p2p_core::ProtocolVersion;
    use dash_pkc::bls::BlsCtx;

    use super::*;

    fn cmd(name: &str) -> CommandString {
        CommandString::from_static(name)
    }

    fn row(name: &str) -> &'static CommandPolicy {
        lookup(&cmd(name)).unwrap_or_else(|| panic!("no policy row for {name}"))
    }

    /// The throw of a read that must throw.
    fn thrown<T: std::fmt::Debug>(r: Result<T, ReadError>) -> Throw {
        match r {
            Err(ReadError::Throw(t)) => t,
            r => panic!("not a throw: {r:?}"),
        }
    }

    #[test]
    fn table_has_core_74_commands_in_core_order() {
        assert_eq!(table().len(), 74);
        for (row, expected) in table().iter().zip(ALL_NET_MESSAGE_TYPES.iter()) {
            assert_eq!(&row.cmd, expected, "row order differs from allNetMessageTypes");
        }
        assert!(table().iter().all(|r| is_known(&r.cmd)));
    }

    #[test]
    fn set_columns_agree_with_the_set_functions() {
        for r in table() {
            assert_eq!(r.blocks_only_violation, violates_blocks_only(&r.cmd), "{}", r.cmd);
            assert_eq!(r.quorum_priority, is_quorum_priority(&r.cmd), "{}", r.cmd);
        }
        assert_eq!(table().iter().filter(|r| r.blocks_only_violation).count(), 26);
        assert_eq!(table().iter().filter(|r| r.quorum_priority).count(), 8);
    }

    #[test]
    fn gate_classes() {
        let before_or_after =
            ["verack", "sendheaders", "sendheaders2", "sendcmpct", "sendaddrv2", "sendtxrcncl"];
        for r in table() {
            let name = r.cmd.as_str();
            let expected = if name == "version" {
                GateClass::Version
            } else if before_or_after.contains(&name) {
                GateClass::BeforeOrAfterVerack
            } else {
                GateClass::PostVerack
            };
            assert_eq!(r.gate, expected, "{name}");
        }
    }

    #[test]
    fn lookup_finds_known_and_not_unknown() {
        assert_eq!(row("platformban").cmd, cmd("platformban"));
        assert!(lookup(&cmd("nosuchcmd")).is_none());
        assert!(lookup(&cmd("")).is_none());
        assert!(lookup(&cmd("PING")).is_none());
    }

    #[test]
    fn reads_nothing_rows_never_throw() {
        for r in table() {
            let name = r.cmd.as_str();
            if r.read == ReadPlan::Nothing {
                assert_eq!(r.on_throw, ThrowOutcome::NeverThrows, "{name}");
            } else if name == "pong" {
                assert_eq!(r.on_throw, ThrowOutcome::NeverThrows);
            } else {
                assert_ne!(r.on_throw, ThrowOutcome::NeverThrows, "{name}");
            }
        }
        for name in [
            "verack",
            "getaddr",
            "sendaddrv2",
            "sendheaders",
            "sendheaders2",
            "filterclear",
            "mempool",
            "getsporks",
            "qwatch",
            "qrinfo",
            "mnlistdiff",
            "merkleblock",
            "cfilter",
            "cfheaders",
            "cfcheckpt",
        ] {
            assert_eq!(row(name).read, ReadPlan::Nothing, "{name}");
        }
    }

    #[test]
    fn pre_read_columns() {
        for name in ["headers", "headers2"] {
            assert_eq!(
                row(name).pre_read,
                &[PreRead::LoadingBlocksIgnore, PreRead::HeadersClearGetheadersTimestamp]
            );
        }
        for name in ["cmpctblock", "blocktxn", "block"] {
            assert_eq!(row(name).pre_read, &[PreRead::LoadingBlocksIgnore], "{name}");
        }
        assert_eq!(row("tx").pre_read, &[PreRead::IbdReturn]);
        assert_eq!(row("dstx").pre_read, &[PreRead::IbdReturn]);
        assert_eq!(row("sendtxrcncl").pre_read, &[PreRead::TxReconciliationOff]);
        assert_eq!(row("ssc").pre_read, &[PreRead::MnSyncFinished]);
        for name in ["filterload", "filteradd", "filterclear"] {
            assert_eq!(row(name).pre_read, &[PreRead::NoBloomDisconnect], "{name}");
        }
        assert!(row("version").pre_read.is_empty());
        assert!(row("ping").pre_read.is_empty());
    }

    #[test]
    fn read_plans() {
        assert_eq!(
            row("headers").read,
            ReadPlan::CountThenItems {
                limit: 2000
            }
        );
        assert_eq!(
            row("headers2").read,
            ReadPlan::CountThenItemsByVersion {
                limit: 2000,
                raised_limit: 8000,
                raised_from: 70235
            }
        );
        assert_eq!(
            row("dss").read,
            ReadPlan::CountThenItems {
                limit: 180
            }
        );
        assert_eq!(row("version").read, ReadPlan::Custom);
        assert_eq!(row("ping").read, ReadPlan::Whole);
    }

    #[test]
    fn throw_outcomes() {
        assert_eq!(row("version").on_throw, ThrowOutcome::VersionTail);
        assert_eq!(row("inv").on_throw, ThrowOutcome::CaughtByProcessMessages);
        assert_eq!(
            row("filterload").on_throw,
            ThrowOutcome::Handler {
                score: 100,
                msg: "misformatted bloom filter"
            }
        );
        assert_eq!(
            row("spork").on_throw,
            ThrowOutcome::Handler {
                score: 100,
                msg: "malformed spork received"
            }
        );
        assert!(matches!(
            row("govsync").on_throw,
            ThrowOutcome::HandlerAfterHead {
                head_bytes: 32,
                score: 100,
                ..
            }
        ));
        for name in ["qsigsesann", "qsigsinv", "qgetsigs", "qbsigs", "qsigshare"] {
            assert!(
                matches!(
                    row(name).on_throw,
                    ThrowOutcome::HandlerThenCaught {
                        score: 100,
                        ..
                    }
                ),
                "{name}"
            );
        }
        for name in ["qcontrib", "qcomplaint", "qjustify", "qpcommit"] {
            assert_eq!(row(name).on_throw, DKG_THROW, "{name}");
        }
    }

    #[test]
    fn trailing_rules_are_the_two_v24_exceptions() {
        for r in table() {
            let name = r.cmd.as_str();
            let expected = match name {
                "qcontrib" | "qcomplaint" | "qjustify" | "qpcommit" => Trailing::WholePayload,
                "qgetdata" => Trailing::PayloadAtMost(67),
                _ => Trailing::Ignored,
            };
            assert_eq!(r.trailing, expected, "{name}");
        }
    }

    fn from_consensus(err: DecodeError) -> Throw {
        Throw::from_decode(&P2pDecodeError::from(err))
    }

    #[test]
    fn end_of_data_maps_to_core_text() {
        let t = from_consensus(DecodeError::Eof {
            needed: 8,
            remaining: 3,
        });
        assert_eq!(t.what, "DataStream::read(): end of data: iostream error");
        assert_eq!(t.type_name, "NSt8ios_base7failureB5cxx11E");
    }

    #[test]
    fn non_minimal_compact_size_maps_to_non_canonical() {
        let t = from_consensus(DecodeError::NonMinimalCompactSize {
            value: 5,
        });
        assert_eq!(t.what, "non-canonical ReadCompactSize(): iostream error");
    }

    #[test]
    fn compact_size_over_max_size_is_size_too_large() {
        let t = from_consensus(DecodeError::CompactSizeExceedsLimit {
            limit: MAX_SIZE as usize,
            value: MAX_SIZE + 1,
        });
        assert_eq!(t.what, "ReadCompactSize(): size too large: iostream error");
    }

    #[test]
    fn compact_size_over_the_bytes_left_is_end_of_data() {
        let t = from_consensus(DecodeError::CompactSizeExceedsLimit {
            limit: 3,
            value: 4,
        });
        assert_eq!(t.what, "DataStream::read(): end of data: iostream error");
    }

    #[test]
    fn version_tail_is_classified_by_its_error_text() {
        let mut bytes = vec![0u8; 46];
        bytes[..4].copy_from_slice(&70240i32.to_le_bytes());
        let head = Box::new(dash_p2p_core::Version::decode(&mut bytes.as_slice()).unwrap());
        let eof: DecodeError = DecodeError::Eof {
            needed: 4,
            remaining: 0,
        };
        let eof = eof.to_string();
        let t = Throw::from_decode(&P2pDecodeError::VersionTail {
            head: head.clone(),
            error: eof,
        });
        assert_eq!(t.what, "DataStream::read(): end of data: iostream error");
        let big: DecodeError = DecodeError::CompactSizeExceedsLimit {
            limit: MAX_SIZE as usize,
            value: 1 << 40,
        };
        let t = Throw::from_decode(&P2pDecodeError::VersionTail {
            head,
            error: big.to_string(),
        });
        assert_eq!(t.what, "ReadCompactSize(): size too large: iostream error");
    }

    #[test]
    fn errors_without_a_core_text_keep_base_sdk_text() {
        let utf8 = from_consensus(DecodeError::InvalidUtf8);
        assert_eq!(utf8.what, "invalid utf-8 in string: iostream error");
        let flag = from_consensus(DecodeError::NonCanonicalBool {
            value: 2,
        });
        assert_eq!(flag.what, "non-canonical bool: got 2: iostream error");
        let sdk = P2pDecodeError::InvalidValue {
            field: "headers2 flags",
        };
        let t = Throw::from_decode(&sdk);
        assert_eq!(t.what, "invalid value for headers2 flags: iostream error");
        assert_eq!(t.type_name, "NSt8ios_base7failureB5cxx11E");
        let unknown = P2pDecodeError::UnknownShortId {
            id: 7,
        };
        assert_eq!(Throw::from_decode(&unknown).what, "unknown v2 short id: 7: iostream error");
    }

    #[test]
    fn log_line_is_cores_process_messages_line() {
        let t = from_consensus(DecodeError::Eof {
            needed: 8,
            remaining: 3,
        });
        assert_eq!(
            t.log_line(&cmd("ping"), 3),
            "ProcessMessages(ping, 3 bytes): Exception 'DataStream::read(): end of data: \
             iostream error' (NSt8ios_base7failureB5cxx11E) caught"
        );
    }

    #[test]
    fn log_line_sanitizes_the_command() {
        let t = Throw::ios_failure("x");
        let bytes = *b"a\x01b<c>d\x7f\0\0\0\0";
        let line = t.log_line(&CommandString::from_bytes(bytes), 0);
        assert!(line.starts_with("ProcessMessages(abcd, 0 bytes): "), "{line}");
        let safe = CommandString::from_bytes(*b"a b.c,d;e-_/");
        assert!(t.log_line(&safe, 1).starts_with("ProcessMessages(a b.c,d;e-_/, 1 bytes): "));
    }

    /// Core reads a C++ `bool` as any byte (`serialize.h:239`): `sendcmpct`'s announce flag and
    /// `getqrinfo`'s `extraShare` decode for 0, 1, 2 and 255, as `true` for any non-zero byte,
    /// through both entry points, consuming the whole payload (DEC-178 (2)).
    #[test]
    fn core_bools_read_as_core_reads_them() {
        let ctx = ProtoCtx::new(ProtocolVersion(70242), BlsCtx::BASIC);
        for flag in [0u8, 1, 2, 255] {
            let mut sendcmpct = vec![flag];
            sendcmpct.extend(1u64.to_le_bytes());
            let mut cursor = &sendcmpct[..];
            let msg: SendCmpct = read(&mut cursor, &ctx).unwrap();
            assert_eq!(msg.high_bandwidth, flag != 0);
            assert!(cursor.is_empty());
            let d = decode(&cmd("sendcmpct"), &sendcmpct, &ctx).unwrap();
            assert_eq!(d.consumed, sendcmpct.len());

            for hashes in [0u8, 2] {
                let mut getqrinfo = vec![hashes];
                getqrinfo.extend(vec![7u8; 32 * usize::from(hashes) + 32]);
                getqrinfo.push(flag);
                let mut cursor = &getqrinfo[..];
                let msg: GetQrInfo = read(&mut cursor, &ctx).unwrap();
                assert_eq!(msg.extra_share, flag != 0);
                assert_eq!(msg.base_block_hashes.len(), usize::from(hashes));
                assert!(cursor.is_empty());
                let d = decode(&cmd("getqrinfo"), &getqrinfo, &ctx).unwrap();
                assert_eq!(d.consumed, getqrinfo.len());
            }
        }
        // A short payload still throws Core's end-of-data text, and other rows are untouched.
        let mut cursor = &[2u8][..];
        let t = thrown(read::<SendCmpct>(&mut cursor, &ctx));
        assert_eq!(t.what, "DataStream::read(): end of data: iostream error");
        assert!(matches!(core_bools(&cmd("senddsq"), &[2]), Cow::Borrowed(_)));
    }

    fn compact(n: usize) -> Vec<u8> {
        let mut v = Vec::new();
        CompactSize::from(n).encode(&mut v);
        v
    }

    /// govsync reads Core's whole `CBloomFilter`: `vData` up to 36000 bytes (refused before it
    /// is read), then `nHashFuncs`, `nTweak` and `nFlags`.
    #[test]
    fn govsync_reads_the_whole_bloom_filter() {
        let ctx = ProtoCtx::new(ProtocolVersion(70242), BlsCtx::BASIC);
        let govsync = |data: usize, tail: usize| {
            let mut p = vec![0u8; 32];
            p.extend(compact(data));
            p.extend(vec![1u8; data + tail]);
            p
        };
        for (data, tail, ok) in [(0, 9, true), (36_000, 9, true), (36_000, 8, false), (2, 0, false)]
        {
            let p = govsync(data, tail);
            let d = decode(&cmd("govsync"), &p, &ctx);
            assert_eq!(d.as_ref().map(|d| d.consumed).ok(), ok.then_some(p.len()), "{data}+{tail}");
            let mut cursor = &p[..];
            match read::<GovSync>(&mut cursor, &ctx) {
                Ok(m) => assert!(ok && cursor.is_empty() && m.bloom_filter.data.len() == data),
                Err(e) => {
                    let t = thrown::<()>(Err(e));
                    assert!(!ok);
                    assert_eq!(t.what, "DataStream::read(): end of data: iostream error");
                }
            }
            let mut cursor = &p[..];
            match read::<GovSyncRead>(&mut cursor, &ctx) {
                Ok(m) => {
                    assert!(ok && cursor.is_empty() && m.sync.bloom_filter.data.len() == data);
                    assert_eq!((m.hash_funcs, m.tweak, m.flags), (0x0101_0101, 0x0101_0101, 1));
                }
                Err(e) => {
                    let t = thrown::<()>(Err(e));
                    assert!(!ok);
                    assert_eq!(t.what, "DataStream::read(): end of data: iostream error");
                }
            }
        }
        let p = govsync(36_001, 9);
        assert!(decode(&cmd("govsync"), &p, &ctx).is_err());
        let t = thrown(read::<GovSync>(&mut &p[..], &ctx));
        assert_eq!(t.what, "Vector length limit exceeded: iostream error");
        let t = thrown(read::<GovSyncRead>(&mut &p[..], &ctx));
        assert_eq!(t.what, "Vector length limit exceeded: iostream error");
        // The tail is typed in wire order.
        let mut p = govsync(1, 0);
        p.extend(3u32.to_le_bytes());
        p.extend(0xdead_beefu32.to_le_bytes());
        p.push(2);
        let m: GovSyncRead = read(&mut &p[..], &ctx).unwrap();
        assert_eq!(
            (m.sync.bloom_filter.data.clone(), m.hash_funcs, m.tweak, m.flags),
            (vec![1], 3, 0xdead_beef, 2)
        );
        let f = &m.sync.bloom_filter;
        assert_eq!((f.hash_funcs, f.tweak, f.flags), (3, 0xdead_beef, 2));
        assert_eq!(read::<GovSync>(&mut &p[..], &ctx).map(GovSyncRead::from).as_ref(), Ok(&m));
        // The limit is checked on the count, before the bytes it promises.
        let mut short = vec![0u8; 32];
        short.extend(compact(36_001));
        let t = thrown(read::<GovSync>(&mut &short[..], &ctx));
        assert_eq!(t.what, "Vector length limit exceeded: iostream error");
    }

    /// `filterload`'s `vData` (`MAX_BLOOM_FILTER_SIZE`) and `filteradd`'s data
    /// (`MAX_SCRIPT_ELEMENT_SIZE`) are `LIMITED_VECTOR` reads: one byte over throws Core's
    /// `Vector length limit exceeded` on the count, before the bytes it promises, which the
    /// handlers log as `e.what()` (np:5540, :5573). A filter cut short after `vData` runs out of
    /// bytes, as in Core.
    #[test]
    fn filter_vectors_are_limited_as_cores() {
        let ctx = ProtoCtx::new(ProtocolVersion(70242), BlsCtx::BASIC);
        let filterload = |data: usize, tail: usize| {
            let mut p = compact(data);
            p.extend(vec![1u8; data]);
            p.extend(vec![0u8; tail]);
            p
        };
        let limited = "Vector length limit exceeded: iostream error";
        let eof = "DataStream::read(): end of data: iostream error";
        assert!(read::<FilterLoad>(&mut &filterload(36_000, 9)[..], &ctx).is_ok());
        let over = filterload(36_001, 9);
        assert!(decode(&cmd("filterload"), &over, &ctx).is_err());
        assert_eq!(thrown(read::<FilterLoad>(&mut &over[..], &ctx)).what, limited);
        assert_eq!(thrown(read::<FilterLoad>(&mut &compact(36_001)[..], &ctx)).what, limited);
        assert_eq!(thrown(read::<FilterLoad>(&mut &filterload(10, 8)[..], &ctx)).what, eof);

        let filteradd = |data: usize| [compact(data), vec![1u8; data]].concat();
        assert_eq!(read::<FilterAdd>(&mut &filteradd(520)[..], &ctx).unwrap().data.len(), 520);
        assert!(decode(&cmd("filteradd"), &filteradd(521), &ctx).is_err());
        assert_eq!(thrown(read::<FilterAdd>(&mut &filteradd(521)[..], &ctx)).what, limited);
        assert_eq!(thrown(read::<FilterAdd>(&mut &compact(521)[..], &ctx)).what, limited);
        assert_eq!(thrown(read::<FilterAdd>(&mut &filteradd(10)[..9], &ctx)).what, eof);
    }

    /// A vote's signature on a network read: at most 96 bytes, and then 65 or 96.
    #[test]
    fn govobjvote_signature_sizes_are_cores() {
        let ctx = ProtoCtx::new(ProtocolVersion(70242), BlsCtx::BASIC);
        for (len, what) in [
            (65, None),
            (96, None),
            (0, Some("bad governance vote signature size")),
            (64, Some("bad governance vote signature size")),
            (95, Some("bad governance vote signature size")),
            (97, Some("Vector length limit exceeded")),
        ] {
            let mut p = vec![0u8; GOV_VOTE_SIG_AT];
            p.extend(compact(len));
            p.extend(vec![3u8; len]);
            let d = decode(&cmd("govobjvote"), &p, &ctx);
            assert_eq!(d.is_ok(), what.is_none(), "{len}");
            let r = read::<GovVote>(&mut &p[..], &ctx);
            match what {
                None => assert_eq!(r.unwrap().sig.len(), len),
                Some(w) => assert_eq!(thrown(r).what, format!("{w}: iostream error")),
            }
        }
        let mut p = vec![0u8; GOV_VOTE_SIG_AT];
        p.extend(compact(200));
        let t = thrown(read::<GovVote>(&mut &p[..], &ctx));
        assert_eq!(t.what, "Vector length limit exceeded: iostream error", "before the bytes");
    }

    /// headers2 headers only Core's `Uncompress` takes: a first header with fields left off
    /// keeps them zero and saves its version (0) in the list, and a version offset past the list
    /// leaves the version 0; the other fields still come from the previous header.
    #[test]
    fn headers2_reads_as_cores_uncompress() {
        let ctx = ProtoCtx::new(ProtocolVersion(70242), BlsCtx::BASIC);
        let mut p = compact(3);
        // First: version offset 1, every other field left off: merkle root, time offset, nonce.
        p.push(0b0000_0001);
        p.extend([0x11; 32]);
        p.extend(5i16.to_le_bytes());
        p.extend(7u32.to_le_bytes());
        // Second: version offset 1 (the saved 0), time offset -1, bits and prev hash derived.
        p.push(0b0000_0001);
        p.extend([0x22; 32]);
        p.extend((-1i16).to_le_bytes());
        p.extend(8u32.to_le_bytes());
        // Third: version offset 7, past the list of one.
        p.push(0b0000_0111);
        p.extend([0x33; 32]);
        p.extend(3i16.to_le_bytes());
        p.extend(9u32.to_le_bytes());
        assert!(matches!(
            P2pMsg::decode_with_policy(&cmd("headers2"), &p, &ctx),
            Err(P2pDecodeError::Consensus(DecodeError::InvalidValue { .. }))
        ));
        let d = decode(&cmd("headers2"), &p, &ctx).unwrap();
        assert_eq!(d.consumed, p.len());
        let P2pMsg::Headers2(h) = d.msg else {
            panic!("headers2")
        };
        let hs: Vec<BlockHeader> = h.block_headers().copied().collect();
        assert_eq!((hs[0].version, hs[0].time, hs[0].bits, hs[0].nonce), (0, 0, 0, 7));
        assert_eq!(hs[0].prev_hash, BlockHeader::default().prev_hash);
        assert_eq!((hs[1].version, hs[1].time, hs[1].bits), (0, u32::MAX, 0));
        assert_eq!(hs[1].prev_hash, hs[0].hash());
        assert_eq!((hs[2].version, hs[2].time), (0, 2));
        assert_eq!(hs[2].prev_hash, hs[1].hash());
        assert!((0..3).all(|i| !h.is_prev_hash_derived(i)));
        let mut cursor = &p[..];
        let r: Headers2 = read(&mut cursor, &ctx).unwrap();
        assert_eq!(r, h);
        assert!(cursor.is_empty());
        // Cut inside the last header: Core runs out of bytes there too.
        assert!(decode(&cmd("headers2"), &p[..p.len() - 1], &ctx).is_err());
    }

    /// A `headers2` of `n` headers, each after the first in Core's commonest compressed form
    /// (version offset 1, previous hash, time and bits derived), the first whole when `whole`
    /// and otherwise with its fields left off too, which only Core's `Uncompress` takes.
    fn headers2_batch(n: usize, whole: bool) -> Vec<u8> {
        let mut p = compact(n);
        for i in 0..n {
            if i == 0 && whole {
                p.push(H2_PREV_HASH | H2_TIMESTAMP | H2_NBITS);
                p.extend(0x2000_0000i32.to_le_bytes());
                p.extend([0x44; 32]);
            } else {
                p.push(0b0000_0001);
            }
            p.extend([i as u8; 32]);
            if i == 0 && whole {
                p.extend(1_700_000_000u32.to_le_bytes());
                p.extend(0x1d00_ffffu32.to_le_bytes());
            } else {
                p.extend(150i16.to_le_bytes());
            }
            p.extend((i as u32).to_le_bytes());
        }
        p
    }

    /// `read_headers2` hashes once per derived previous hash, through the caller's hook, where
    /// Core's `Uncompress` does: 7999 for a full batch of 8000 on the strict path and on the
    /// lenient one, so a processor that pairs each header with its hash there and hashes only
    /// the last itself makes Core's 8000 (DEC-51a). Its headers are [`decode`]'s.
    #[test]
    fn headers2_hash_once_on_both_paths() {
        let ctx = ProtoCtx::new(ProtocolVersion(70242), BlsCtx::BASIC);
        for whole in [true, false] {
            let p = headers2_batch(8000, whole);
            assert_eq!(P2pMsg::decode_with_policy(&cmd("headers2"), &p, &ctx).is_ok(), whole);
            let mut memo: Vec<(BlockHeader, BlockHash)> = Vec::new();
            let mut cursor = &p[..];
            let headers = read_headers2(&mut cursor, &ctx, |h| {
                let hash = h.hash();
                memo.push((*h, hash));
                hash
            })
            .unwrap();
            assert!(cursor.is_empty());
            assert_eq!(headers.len(), 8000);
            assert_eq!(memo.len(), 7999, "whole first header: {whole}");
            // The hook saw each header but the last, in order, before the next was uncompressed.
            assert!(memo.iter().zip(&headers).all(|((m, _), h)| m == h));
            assert!(headers.windows(2).zip(&memo).all(|(w, (_, hash))| w[1].prev_hash == *hash));
            let P2pMsg::Headers2(d) = decode(&cmd("headers2"), &p, &ctx).unwrap().msg else {
                panic!("headers2")
            };
            assert!(d.block_headers().eq(headers.iter()));
        }
    }

    /// [`read_headers2`] on `p`: its result, its hook calls and the bytes it left.
    fn read_counted(
        p: &[u8],
        ctx: &ProtoCtx,
    ) -> (Result<Vec<BlockHeader>, ReadError>, usize, usize) {
        let mut cursor = p;
        let mut calls = 0;
        let r = read_headers2(&mut cursor, ctx, |h| {
            calls += 1;
            h.hash()
        });
        (r, calls, cursor.len())
    }

    /// Core reads and hashes `headers2` headers until the bytes run out, then throws; a count over
    /// the limit for the version is refused before any header is read or hashed. The cursor
    /// follows `read_headers2`'s contract.
    #[test]
    fn headers2_hashes_until_the_bytes_run_out() {
        let ctx = ProtoCtx::new(ProtocolVersion(70242), BlsCtx::BASIC);
        let throw = |r: Result<Vec<BlockHeader>, ReadError>| match r {
            Err(ReadError::Throw(t)) => t.what.into_owned(),
            r => panic!("{r:?}"),
        };
        const EOF: &str = "DataStream::read(): end of data: iostream error";
        let p = headers2_batch(10, true);
        // The count promises 8000; ten headers follow.
        let mut short = compact(8000);
        short.extend(&p[compact(10).len()..]);
        let (r, calls, left) = read_counted(&short, &ctx);
        assert_eq!((throw(r).as_str(), calls, left), (EOF, 9, short.len()));
        assert!(decode(&cmd("headers2"), &short, &ctx).is_err());
        // An eleventh header cut after its bit field throws before its predecessor is hashed,
        // though the field leaves the previous hash to be derived (version offset 1, nothing on
        // the wire).
        let mut partial = short.clone();
        partial.push(0b0000_0001);
        let (r, calls, left) = read_counted(&partial, &ctx);
        assert_eq!((throw(r).as_str(), calls, left), (EOF, 9, partial.len()));

        let old = ProtoCtx::new(ProtocolVersion(70234), BlsCtx::BASIC);
        let over = headers2_batch(2001, true);
        let (r, calls, left) = read_counted(&over, &old);
        let too_many = Err(ReadError::TooMany {
            count: 2001,
            max: 2000,
        });
        assert_eq!((r, calls, left), (too_many, 0, over.len()));
        let (r, calls, left) = read_counted(&over, &ctx);
        assert_eq!((r.map(|h| h.len()), calls, left), (Ok(2001), 2000, 0));

        // The count's own throws: no count, a non-canonical one, one over `MAX_SIZE`.
        for (p, what) in [
            (&[][..], EOF),
            (&[0xfd, 0x01, 0x00][..], "non-canonical ReadCompactSize(): iostream error"),
            (
                &[0xfe, 0x01, 0x00, 0x00, 0x02][..],
                "ReadCompactSize(): size too large: iostream error",
            ),
        ] {
            let (r, calls, left) = read_counted(p, &ctx);
            assert_eq!((throw(r).as_str(), calls, left), (what, 0, p.len()));
        }
        // An empty message reads no header and only its count; what follows stays.
        let (r, calls, left) = read_counted(&[0, 0xaa], &ctx);
        assert_eq!((r, calls, left), (Ok(Vec::new()), 0, 1));
    }

    /// The version list holds seven entries: after versions 100…106, offset 7 takes 100; after
    /// 100…107 an eighth insertion has evicted 100, and offset 7 takes 101 (Core's
    /// `max_backwards_look_ups`, primitives/block.cpp:49-53; `node-fuzz`'s matrix judges the same
    /// payloads with its own `Uncompress`).
    #[test]
    fn headers2_version_list_holds_seven() {
        let ctx = ProtoCtx::new(ProtocolVersion(70242), BlsCtx::BASIC);
        for (n, oldest) in [(7i32, 100), (8, 101)] {
            let mut p = compact(n as usize + 1);
            for v in 100..100 + n {
                p.push(0b0011_1000);
                p.extend(v.to_le_bytes());
                p.extend([v as u8; 64]);
                p.extend(1_400_000_000u32.to_le_bytes());
                p.extend(0x1d00_ffff_u32.to_le_bytes());
                p.extend((v as u32).to_le_bytes());
            }
            // Offset 7, everything else compressed: the previous hash is derived.
            p.push(7);
            p.extend([0x77; 32]);
            p.extend(0i16.to_le_bytes());
            p.extend(7u32.to_le_bytes());
            let (r, calls, left) = read_counted(&p, &ctx);
            let headers = r.unwrap();
            assert_eq!((headers.last().map(|h| h.version), calls, left), (Some(oldest), 1, 0));
        }
    }

    /// getqrinfo payloads whose bool is not where a short count puts it: wide, non-canonical,
    /// over-limit and over-`MAX_SIZE` counts, and payloads that end before the bool. The
    /// rewrite never panics, and the verdict, bytes consumed and error are base-sdk's on the
    /// same payload with that byte read as Core reads it.
    #[test]
    fn core_bools_leave_other_getqrinfo_verdicts_alone() {
        let ctx = ProtoCtx::new(ProtocolVersion(70242), BlsCtx::BASIC);
        let getqrinfo = cmd("getqrinfo");
        let prefixes: [(&[u8], usize); 9] = [
            (&[0xfd, 0xfd, 0x00], 253),
            (&[0xfd, 0x00, 0x10], 4096),
            (&[0xfd, 0x01, 0x10], 4097),
            (&[0xfd, 0x02, 0x00], 2),
            (&[0xfe, 0x00, 0x10, 0x00, 0x00], 4096),
            (&[0xfe, 0xff, 0xff, 0xff, 0xff], 2),
            (&[0xff, 0x02, 0, 0, 0, 0, 0, 0, 0], 2),
            (&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff], 2),
            (&[0xfd], 0),
        ];
        for (prefix, hashes) in prefixes {
            for flag in [0u8, 1, 2, 255] {
                let mut full = prefix.to_vec();
                full.extend(vec![7u8; 32 * hashes + 32]);
                full.push(flag);
                for len in [full.len(), full.len() - 1, prefix.len()] {
                    let payload = &full[..len];
                    let mut as_core = payload.to_vec();
                    if len == full.len() {
                        as_core[len - 1] = u8::from(flag != 0);
                    }
                    let want = P2pMsg::decode_with_policy(&getqrinfo, &as_core, &ctx)
                        .map(|d| d.consumed)
                        .map_err(|e| e.to_string());
                    let got = decode(&getqrinfo, payload, &ctx)
                        .map(|d| d.consumed)
                        .map_err(|e| e.to_string());
                    assert_eq!(got, want, "{prefix:02x?} flag {flag} len {len}");
                    let mut cursor = payload;
                    let read = read::<GetQrInfo>(&mut cursor, &ctx).map(|m| m.extra_share);
                    assert_eq!(read.is_ok(), want.is_ok(), "{prefix:02x?} flag {flag} len {len}");
                    if let Ok(extra_share) = read {
                        assert_eq!(extra_share, flag != 0);
                        assert!(cursor.is_empty());
                    }
                }
            }
        }
    }
}
