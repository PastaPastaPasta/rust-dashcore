//! The 74 commands and who handles them (P2a §3.2.2, §3.2.6, §3.11).
//!
//! [`ROWS`] is Core's `allNetMessageTypes` (`protocol.cpp:99-176`) in Core's order, each with its
//! owner, its M2 status and where its post-read rule comes from. SB6's
//! [`dash_p2p::policy`] holds the normative pre-read columns; [`Row::policy`] joins the two.
//!
//! A command is either handled inline by P2a-core's `ProcessMessage` ([`Route::Inline`]) or goes
//! to the [`Registry`], Core's extension dispatch: each listed command has exactly one
//! [`NetHandler`]. Until its owner registers one, a command is handled by P2a's M2 stub
//! ([`super::stubs::M2Stubs`]), the outcome dashd has in the M2 state. Registering an owner
//! replaces the stub. Registering a command twice, or an inline or unlisted command, panics at
//! startup. `getheaders` and `getheaders2` are the exception: their owner, P2b's
//! [`super::serve::HeadersServer`], is built in (P2b D5).

use std::sync::Arc;

use dash_p2p::policy::{self, CommandPolicy, ReadError, Throw};
use dash_p2p::{CommandString, NetCore, OutMsg, PauseState, PeerHandle, Why};
use dash_p2p_core::{Inventory, MsgDecode, ProtoCtx};
use parking_lot::Mutex;

use super::misbehavior;
use super::peer::PeerState;
use super::peerman::Signals;
use super::seams::{ChainAccess, ChainSnapshot};
use super::serve::HeadersServer;
use super::serve_blocks::BlockServer;
use super::solicit::{self, GetDataResponse};
use super::stubs::M2Stubs;
use super::txrequest::TxRequestTracker;

/// How P2a-core reaches a command's handler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// A branch of P2a-core's `ProcessMessage`.
    Inline,
    /// The [`Registry`]: the owner's handler, or P2a's M2 stub until it registers.
    Registry,
}

/// What rdashd does at M2 compared with dashd in the M2 state (P2a §3.11).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum M2 {
    /// Byte- and score-identical.
    Exact,
    /// An interim difference that the named task closes.
    Deviation {
        /// The owning task's codename.
        closes: &'static str,
    },
}

/// Where a row's post-read rule (the handler's outcome after a successful read) comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostRead {
    /// Re-derived from Core by P2a; the v24 lines.
    Cited(&'static str),
    /// The owner re-derives and cites it when it implements the handler (DEC-45); P2a cites only
    /// the M2-reachable outcome, in the stub.
    OwnerDerives,
}

/// One command (P2a §3.11).
#[derive(Clone, Copy, Debug)]
pub struct Row {
    /// The command.
    pub cmd: CommandString,
    /// Inline or the registry.
    pub route: Route,
    /// The task that owns the full handler.
    pub owner: &'static str,
    /// The M2 status.
    pub m2: M2,
    /// Where the post-read rule comes from.
    pub post_read: PostRead,
}

impl Row {
    /// SB6's normative pre-read row for this command.
    pub fn policy(&self) -> &'static CommandPolicy {
        policy::lookup(&self.cmd).expect("ROWS holds only Core's commands")
    }
}

const fn row(cmd: &'static str, route: Route, owner: &'static str, m2: M2, post: PostRead) -> Row {
    Row {
        cmd: CommandString::from_static(cmd),
        route,
        owner,
        m2,
        post_read: post,
    }
}

const fn inline(cmd: &'static str, cite: &'static str) -> Row {
    row(cmd, Route::Inline, "P2a-core", M2::Exact, PostRead::Cited(cite))
}

/// An inline branch with an interim difference that `closes` removes.
const fn inline_until(cmd: &'static str, cite: &'static str, closes: &'static str) -> Row {
    row(
        cmd,
        Route::Inline,
        "P2a-core",
        M2::Deviation {
            closes,
        },
        PostRead::Cited(cite),
    )
}

const fn exact(cmd: &'static str, owner: &'static str) -> Row {
    row(cmd, Route::Registry, owner, M2::Exact, PostRead::OwnerDerives)
}

const fn deviation(cmd: &'static str, owner: &'static str) -> Row {
    deviation_until(cmd, owner, owner)
}

/// An owner's handler with an interim difference that another task, `closes`, removes.
const fn deviation_until(cmd: &'static str, owner: &'static str, closes: &'static str) -> Row {
    row(
        cmd,
        Route::Registry,
        owner,
        M2::Deviation {
            closes,
        },
        PostRead::OwnerDerives,
    )
}

/// What closes the address-relay rows: the host installing P2a-net's relay (P2a-net D2).
const RELAY: &str = "P2a-net NodeAddressRelay as Seams::addr_relay";

/// What closes the `cmpctblock` row: the host installing P3's block download (P3 S6, D15).
const BLOCK_SYNC: &str = "P3 BlockDownload as Seams::block_sync";

/// Core's 74 commands in `allNetMessageTypes` order (P2a §3.11). The owners are the PLAN's
/// codenames; "Core" marks the four types Core lists and never handles.
pub const ROWS: [Row; 74] = [
    inline("version", "np:3935-4176 (SB6 handshake::on_version)"),
    inline("verack", "np:4187-4259 (SB6 handshake::on_verack)"),
    // Address relay (`RelayAddress`, `MaybeSendAddr`, the inbound `getaddr` reply) is the
    // `AddressRelay` seam (D14): exact once the host installs P2a-net's `NodeAddressRelay` as
    // `Seams::addr_relay`, a deviation under M2's `NoAddressRelay`.
    inline_until("addr", "np:4388-4482 (SB6 addr::on_addr; relay P2a-net)", RELAY),
    inline_until("addrv2", "np:4388-4482 (SB6 addr::on_addr; relay P2a-net)", RELAY),
    inline("sendaddrv2", "np:4292-4305 (SB6 handshake::on_sendaddrv2)"),
    // P5a's tracker requests object announcements (sporks in IBD, everything after it). Exact
    // only after P5 (the transaction announcements and `AlreadyHave`), SP1 (governance waits on
    // mnsync's blockchain sync) and LK1b (`isdlock` completion); the closer names the first
    // (P5a D17).
    inline_until("inv", "np:4507-4611", "P5"),
    // Blocks are served (P4b); the `isdlock`s after a filtered block wait for LK1b's lock store
    // (P4b D9), as `clsig`s wait for LK1a's (P5a D5).
    inline_until("getdata", "np:4613-4634; ProcessGetData np:2935-3111", "LK1b"),
    exact("merkleblock", "Core"),
    // P4b's `serve_blocks::BlockServer`, installed by `Registry::new` (P4b D2), with the filter
    // rows and `getblocktxn`.
    exact("getblocks", "P4"),
    // P2b's `serve::HeadersServer`, installed by `Registry::new` (P2b D5).
    exact("getheaders", "P2b"),
    exact("tx", "P5"),
    inline("headers", "np:5309-5346; ProcessHeadersMessage np:3322-3386"),
    inline("block", "np:5348-5387 (P3 blocks::on_block)"),
    inline_until("getaddr", "np:5389-5424 (inbound reply P2a-net)", RELAY),
    exact("mempool", "P5"),
    inline("ping", "np:5456-5472 (SB6 ping::on_ping)"),
    inline("pong", "np:5474-5527 (SB6 ping::on_pong)"),
    inline("notfound", "np:5744-5765"),
    exact("filterload", "P4"),
    exact("filteradd", "P4"),
    exact("filterclear", "P4"),
    inline("sendheaders", "np:4261-4265"),
    inline("sendcmpct", "np:4273-4288"),
    // Exact once the host installs P3's `BlockDownload` as `Seams::block_sync`; ignored under
    // M2's `NoBlockSync`, which has no in-flight state to reconstruct into.
    inline_until("cmpctblock", "np:5052-5293 (P3 blocks::on_cmpctblock)", BLOCK_SYNC),
    exact("getblocktxn", "P4"),
    inline(
        "blocktxn",
        "np:5295-5307; ProcessCompactBlockTxns np:3734-3816 (P3 blocks::on_blocktxn)",
    ),
    exact("getcfilters", "P6b"),
    exact("cfilter", "Core"),
    exact("getcfheaders", "P6b"),
    exact("cfheaders", "Core"),
    exact("getcfcheckpt", "P6b"),
    exact("cfcheckpt", "Core"),
    inline("sendtxrcncl", "np:4310-4361 (txreconciliation off)"),
    // SP0's handler verifies, scores, stores and serves; P5a's tracker and `RelayInv` complete
    // it (SP0 D4, P5a D13).
    exact("spork", "SP0"),
    exact("getsporks", "SP0"),
    inline("senddsq", "np:4485-4497"),
    exact("dsa", "CJ1"),
    exact("dsi", "CJ1"),
    exact("dsf", "WL6"),
    exact("dss", "CJ1"),
    exact("dsc", "WL6"),
    exact("dssu", "WL6"),
    exact("dstx", "CJ1"),
    exact("dsq", "CJ1"),
    exact("ssc", "SP1"),
    exact("govsync", "GV1b"),
    exact("govobj", "GV1b"),
    exact("govobjvote", "GV1b"),
    deviation("getmnlistd", "P6b"),
    exact("mnlistdiff", "P6b"),
    inline("qsendrecsigs", "np:4500-4505"),
    // Exact at M2 through P4a's P2P-12 gate (nothing is requested); the handler is LQ2's.
    exact("qfcommit", "LQ2"),
    exact("qcontrib", "DK1"),
    exact("qcomplaint", "DK1"),
    exact("qjustify", "DK1"),
    exact("qpcommit", "DK1"),
    exact("qwatch", "DK1"),
    exact("qsigsesann", "SG1b"),
    exact("qsigsinv", "SG1b"),
    exact("qgetsigs", "SG1b"),
    exact("qbsigs", "SG1b"),
    deviation("qsigrec", "SG1a"),
    exact("qsigshare", "SG1b"),
    exact("qgetdata", "QM1b"),
    exact("qdata", "QM1b"),
    // Core's branch is in `PeerManagerImpl` (P5a D3): the spork gate, the read, the hash and
    // the solicitation check are P5a's; the rest is LK1a's `ChainLocks` (D4, D5).
    inline_until(
        "clsig",
        "np:5794-5819; PeerConsumeGetDataResponse np:6925-6957",
        "LK1a ChainLocks as Seams::chainlocks",
    ),
    // Exact while InstantSend is off: off mainnet until a spork turns it on (SP0 stores sporks),
    // never on mainnet, where every spork is hard-coded on (SP0 D6).
    deviation("isdlock", "LK1b"),
    exact("mnauth", "AM1"),
    exact("getheaders2", "P2b"),
    inline("sendheaders2", "np:4267-4271"),
    inline("headers2", "np:5309-5346; ProcessHeadersMessage np:3322-3386"),
    deviation("getqrinfo", "P6b"),
    exact("qrinfo", "P6b"),
    exact("platformban", "AM1"),
];

/// The row of `cmd`, or `None` for a command Core does not list.
pub fn row_of(cmd: &CommandString) -> Option<&'static Row> {
    ROWS.iter().find(|r| r.cmd == *cmd)
}

fn index_of(cmd: &CommandString) -> Option<usize> {
    ROWS.iter().position(|r| r.cmd == *cmd)
}

/// Core's `MisbehavingError` (msg_result.h).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MisbehavingError {
    /// The score.
    pub score: i32,
    /// The message; empty for none.
    pub message: String,
}

/// Core's `MessageProcessingResult` (msg_result.h) less `m_transactions`, which is P5's: what a
/// subsystem outside `net_processing` asks of it, applied by `PostProcessMessage` (P5a D13).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MessageProcessingResult {
    /// `m_error`: score the sending peer.
    pub error: Option<MisbehavingError>,
    /// `m_inventory`: forget each request, then relay it to every peer.
    pub inventory: Vec<Inventory>,
    /// `m_to_erase`: complete the sending peer's request for it.
    pub to_erase: Option<Inventory>,
}

impl MessageProcessingResult {
    /// A result that relays `inv` (Core's `MessageProcessingResult(CInv)`).
    pub fn relay(inv: Inventory) -> Self {
        Self {
            inventory: vec![inv],
            ..Self::default()
        }
    }

    /// A result that scores the sender (Core's `MessageProcessingResult(MisbehavingError)`).
    pub fn misbehaving(score: i32, message: impl Into<String>) -> Self {
        Self {
            error: Some(MisbehavingError {
                score,
                message: message.into(),
            }),
            ..Self::default()
        }
    }

    /// Core's `empty()`.
    pub fn is_empty(&self) -> bool {
        self.error.is_none() && self.inventory.is_empty() && self.to_erase.is_none()
    }
}

/// What a handler reaches: Core's `PeerManagerImpl` members for one message from one peer
/// (P2a §2.4). Reads build their [`ProtoCtx`] when the handler is called, never earlier
/// (P2a §3.2.5).
pub struct HandlerCx<'a> {
    /// The network core.
    pub core: &'a NetCore,
    /// The peer.
    pub peer: &'a PeerHandle,
    /// Its processor state.
    pub state: &'a mut PeerState,
    /// The chain.
    pub chain: &'a dyn ChainAccess,
    /// The message's chain snapshot (R8): every read of the tip or best header for this
    /// message uses it.
    pub snap: Arc<ChainSnapshot>,
    /// The context of this message's reads: the common version and the snapshot's BLS scheme.
    pub ctx: ProtoCtx,
    /// `GetTime<µs>()` when the message was dispatched.
    pub now_us: i64,
    /// The processor's signal state: `recent` (`m_most_recent_block`), which block serving
    /// reads live (P4b D10).
    pub signals: &'a Signals,
    /// `m_object_request`, for [`HandlerCx::consume_getdata_response`].
    pub(crate) requests: Arc<Mutex<TxRequestTracker>>,
    /// The object-request and relay effects queued by the handler, which the processor applies
    /// in order when it returns (P5a D13).
    pub(crate) post: Vec<MessageProcessingResult>,
}

impl Drop for HandlerCx<'_> {
    /// Every queued result is applied before its context goes.
    fn drop(&mut self) {
        debug_assert!(
            self.post.is_empty() || std::thread::panicking(),
            "a handler context dropped unapplied results"
        );
    }
}

impl HandlerCx<'_> {
    /// Reads `T` from the front of `payload` at Core's read point. Only for a row whose read
    /// cannot refuse: `headers`/`headers2` score their count before reading in their own handler
    /// and `qgetdata` its size (SB6's [`ReadError`] refusals), so a refusal here is a misuse (D54).
    pub fn read<T: MsgDecode + 'static>(&self, payload: &mut &[u8]) -> Result<T, Throw> {
        policy::read(payload, &self.ctx).map_err(|e| match e {
            ReadError::Throw(t) => t,
            refusal => unreachable!("HandlerCx::read of a row that refuses: {refusal:?}"),
        })
    }

    /// Core's `Misbehaving(peer, howmuch, msg)`.
    pub fn misbehaving(&mut self, howmuch: i32, msg: &str) {
        misbehavior::misbehaving(self.state.id, &self.state.misbehavior, howmuch, msg);
    }

    /// Core's `PushMessage`.
    pub fn push(&self, msg: OutMsg) -> PauseState {
        self.core.push(self.peer, msg)
    }

    /// Core's `pfrom.fDisconnect = true`.
    pub fn disconnect(&self, why: &'static str) {
        self.core.disconnect(self.peer.id(), Why::Processor(why));
    }

    /// The peer's id.
    pub fn id(&self) -> i64 {
        self.peer.id()
    }

    /// Core's `PostProcessMessage(result, peer)`, applied when the handler returns.
    pub fn post_process(&mut self, result: MessageProcessingResult) {
        if !result.is_empty() {
            self.post.push(result);
        }
    }

    /// Core's `PeerConsumeGetDataResponse(peer, inv)`: whether `inv` answers a request to this
    /// peer, spending the request (the tracker's `ReceivedRequestedResponse`) or, failing that,
    /// the per-peer record's grace.
    // Core: net_processing.cpp:6925-6957
    pub fn consume_getdata_response(&mut self, inv: &Inventory) -> GetDataResponse {
        let requested = self.requests.lock().received_requested_response(self.id(), inv);
        solicit::consume_getdata_response(
            &mut self.state.recent_object_requests,
            requested,
            inv,
            self.now_us,
        )
    }

    /// `m_object_request.ReceivedResponse(peer, inv)` (Core's `PeerConsumeObjectRequest`).
    pub fn received_response(&mut self, inv: Inventory) {
        self.post_process(MessageProcessingResult {
            to_erase: Some(inv),
            ..MessageProcessingResult::default()
        });
    }

    /// `m_object_request.ForgetTxHash(inv)` then `RelayInv(inv)`.
    pub fn relay_inv(&mut self, inv: Inventory) {
        self.post_process(MessageProcessingResult::relay(inv));
    }
}

/// Core's `m_handlers` in `AddExtraHandler` order (init.cpp:2208-2281; P5a D2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CoreHandler {
    /// `NetInstantSend`.
    NetInstantSend,
    /// `llmq::NetSigning`.
    NetSigning,
    /// `llmq::NetQuorum`.
    NetQuorum,
    /// `llmq::NetDKG`, or `NetDKGStub` without the DKG.
    NetDkg,
    /// `CCoinJoinServer` (masternodes only).
    CoinJoinServer,
    /// `NetGovernance`.
    NetGovernance,
    /// `SyncManager`.
    SyncManager,
}

/// The handlers `ProcessMessage` calls for every known command before the `CLSIG` branch and
/// `m_handlers`, in Core's order (P5a D21).
// Core: net_processing.cpp:5779-5792
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Extension {
    /// `m_cj_walletman->processMessage` (wallets only).
    CoinJoinWallet,
    /// `CMNAuth::ProcessMessage`.
    MnAuth,
    /// `quorum_block_processor->ProcessMessage`.
    QuorumBlockProcessor,
    /// `ProcessPlatformBanMessage`.
    PlatformBan,
}

/// [`CoreHandler`]s in Core's registration order.
pub const CORE_HANDLER_ORDER: [CoreHandler; 7] = [
    CoreHandler::NetInstantSend,
    CoreHandler::NetSigning,
    CoreHandler::NetQuorum,
    CoreHandler::NetDkg,
    CoreHandler::CoinJoinServer,
    CoreHandler::NetGovernance,
    CoreHandler::SyncManager,
];

/// Core's `NetHandler` (`net_processing.h:122-145`): a subsystem that consumes some listed
/// commands. Handlers get the raw payload and decode at Core's read point through
/// [`HandlerCx::read`]; a read that throws returns `Err`, which the exception boundary logs as
/// Core's `ProcessMessages` does (P2a §3.2.4).
pub trait NetHandler: Send {
    /// The commands it consumes.
    fn commands(&self) -> &'static [CommandString];

    /// The Core handler it ports, if it is one of `m_handlers`; [`Registry::register`] keeps
    /// their order. `None` for a handler of an inline Core branch (SP0's `spork`).
    fn core_handler(&self) -> Option<CoreHandler> {
        None
    }

    /// The extension it ports, if it is one: it also gets `clsig`, before Core's `CLSIG` branch
    /// (P5a D21).
    fn extension(&self) -> Option<Extension> {
        None
    }

    /// Core's `ProcessMessage(pfrom, msg_type, vRecv)`.
    fn process_message(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        payload: &[u8],
    ) -> Result<(), Throw>;

    /// Core's `AlreadyHave` for its inventory types.
    fn already_have(&self, inv: &Inventory) -> bool {
        let _ = inv;
        false
    }

    /// Core's `ProcessGetData`: whether it pushed the object.
    fn process_getdata(&mut self, cx: &mut HandlerCx<'_>, inv: &Inventory) -> bool {
        let _ = (cx, inv);
        false
    }

    /// Core's `NetHandler::Interrupt`.
    fn interrupt(&mut self) {}

    /// Core's `NetHandler::Stop`.
    fn stop(&mut self) {}
}

/// Core's extension dispatch: one handler per registry command (P2a §3.2.6).
pub struct Registry {
    /// Index 0 is the M2 stubs; owners follow in registration order (Core's `m_handlers`).
    handlers: Vec<Box<dyn NetHandler>>,
    /// The handler of each row, by [`ROWS`] index; `None` for inline rows.
    owner: [Option<usize>; 74],
    /// The last Core handler registered.
    last_core: Option<CoreHandler>,
    /// The [`Extension`]s' handlers, in Core's order.
    extensions: Vec<(Extension, usize)>,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Registry {
    /// Every registry command handled by P2a's M2 stub, except `getheaders` and `getheaders2`,
    /// which P2b's built-in [`HeadersServer`] serves (P2b D5), and `getblocks`, `getblocktxn`
    /// and the filter commands, which P4b's [`BlockServer`] serves (P4b D2): Core's branches
    /// are part of `PeerManagerImpl` itself, so no host registers them.
    pub fn new() -> Self {
        let mut reg = Self {
            handlers: vec![Box::new(M2Stubs)],
            owner: ROWS.map(|r| (r.route == Route::Registry).then_some(0)),
            last_core: None,
            extensions: Vec::new(),
        };
        reg.register(Box::new(HeadersServer));
        reg.register(Box::new(BlockServer));
        reg
    }

    /// Adds an owner's handler in Core's registration order (ND1). Its commands leave the M2
    /// stub.
    ///
    /// # Panics
    ///
    /// When a command is not one of Core's, is handled inline, or already has an owner; when
    /// the handler is a [`CoreHandler`] Core registers before one already registered; when its
    /// [`Extension`] already has a handler.
    pub fn register(&mut self, handler: Box<dyn NetHandler>) {
        if let Some(h) = handler.core_handler() {
            if let Some(last) = self.last_core.filter(|last| *last >= h) {
                panic!("{h:?} registered after {last:?}: Core registers it first");
            }
            self.last_core = Some(h);
        }
        let id = self.handlers.len();
        if let Some(e) = handler.extension() {
            let at = match self.extensions.binary_search_by_key(&e, |(e, _)| *e) {
                Ok(_) => panic!("{e:?} already has a handler"),
                Err(at) => at,
            };
            self.extensions.insert(at, (e, id));
        }
        for cmd in handler.commands() {
            let i =
                index_of(cmd).unwrap_or_else(|| panic!("{cmd:?} is not one of Core's commands"));
            match self.owner[i] {
                None => panic!("{cmd:?} is handled inline by P2a-core"),
                Some(0) => self.owner[i] = Some(id),
                Some(_) => panic!("{cmd:?} already has a handler"),
            }
        }
        self.handlers.push(handler);
    }

    /// Whether `cmd` is still handled by the M2 stub.
    pub fn is_stubbed(&self, cmd: &CommandString) -> bool {
        index_of(cmd).is_some_and(|i| self.owner[i] == Some(0))
    }

    /// Dispatches a registry command to its handler. `None` when `cmd` is inline or unlisted.
    pub fn dispatch(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        payload: &[u8],
    ) -> Option<Result<(), Throw>> {
        let h = self.owner[index_of(cmd)?]?;
        Some(self.handlers[h].process_message(cx, cmd, payload))
    }

    /// How many [`Extension`]s have a handler.
    pub fn extension_count(&self) -> usize {
        self.extensions.len()
    }

    /// Dispatches `cmd` to the `i`th [`Extension`]'s handler, in Core's order.
    pub fn dispatch_extension(
        &mut self,
        i: usize,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        payload: &[u8],
    ) -> Result<(), Throw> {
        let h = self.extensions[i].1;
        self.handlers[h].process_message(cx, cmd, payload)
    }

    /// Core's `ProcessGetData` loop over `m_handlers`: the first handler that pushes wins.
    pub fn process_getdata(&mut self, cx: &mut HandlerCx<'_>, inv: &Inventory) -> bool {
        self.handlers.iter_mut().skip(1).any(|h| h.process_getdata(cx, inv))
    }

    /// Core's `AlreadyHave` fallthrough to the handlers.
    pub fn already_have(&self, inv: &Inventory) -> bool {
        self.handlers.iter().skip(1).any(|h| h.already_have(inv))
    }

    /// Core's `NetHandler::Interrupt` on every handler.
    pub fn interrupt(&mut self) {
        self.handlers.iter_mut().for_each(|h| h.interrupt());
    }

    /// Core's `NetHandler::Stop` on every handler.
    pub fn stop(&mut self) {
        self.handlers.iter_mut().for_each(|h| h.stop());
    }
}

#[cfg(test)]
mod tests {
    use dash_p2p::policy::{ALL_NET_MESSAGE_TYPES, table};

    use super::*;

    #[test]
    fn rows_are_cores_74_commands_in_order() {
        let ours: Vec<_> = ROWS.iter().map(|r| r.cmd).collect();
        assert_eq!(ours, ALL_NET_MESSAGE_TYPES.to_vec());
        assert_eq!(table().len(), 74);
        for r in &ROWS {
            assert_eq!(r.policy().cmd, r.cmd);
        }
    }

    #[test]
    fn inline_rows_are_p2a_cores_and_cited() {
        for r in ROWS.iter().filter(|r| r.route == Route::Inline) {
            assert_eq!(r.owner, "P2a-core", "{:?}", r.cmd);
            assert!(matches!(r.post_read, PostRead::Cited(_)), "{:?}", r.cmd);
        }
        for r in ROWS.iter().filter(|r| r.route == Route::Registry) {
            assert_ne!(r.owner, "P2a-core", "{:?}", r.cmd);
            assert_eq!(r.post_read, PostRead::OwnerDerives, "{:?}", r.cmd);
        }
    }

    #[test]
    fn deviations_name_their_closer() {
        let closers = |route| -> Vec<_> {
            ROWS.iter()
                .filter(|r| r.route == route)
                .filter_map(|r| match r.m2 {
                    M2::Deviation {
                        closes,
                    } => Some((r.cmd.as_str(), closes)),
                    M2::Exact => None,
                })
                .collect()
        };
        // P2a §3.11's twelve and getblocktxn (genesis) until P4 serves, less `block` and
        // `cmpctblock`, inline since P3 S1 and S3; getheaders and getheaders2 are exact since
        // P2b serves them, qfcommit since P4a's P2P-12 gate, getblocktxn and the three filter
        // rows since P4b serves them, `spork` since P5a relays it, and `clsig` inline since
        // P5a. Each closes with its owner's handler.
        let registry = closers(Route::Registry);
        assert_eq!(registry.len(), 4, "{registry:?}");
        for (cmd, c) in &registry {
            let owner = row_of(&CommandString::from_static(cmd)).unwrap().owner;
            assert_eq!(*c, owner, "{cmd}");
        }
        // The inline branches that wait on a seam.
        let inline = closers(Route::Inline);
        assert_eq!(
            inline,
            [
                ("addr", RELAY),
                ("addrv2", RELAY),
                ("inv", "P5"),
                ("getdata", "LK1b"),
                ("getaddr", RELAY),
                ("cmpctblock", BLOCK_SYNC),
                ("clsig", "LK1a ChainLocks as Seams::chainlocks"),
            ]
        );
    }

    struct Owner(&'static [CommandString]);

    impl NetHandler for Owner {
        fn commands(&self) -> &'static [CommandString] {
            self.0
        }

        fn process_message(
            &mut self,
            _cx: &mut HandlerCx<'_>,
            _cmd: &CommandString,
            _payload: &[u8],
        ) -> Result<(), Throw> {
            Ok(())
        }
    }

    const SPORK: [CommandString; 2] =
        [CommandString::from_static("spork"), CommandString::from_static("getsporks")];

    #[test]
    fn registering_an_owner_replaces_the_stub() {
        let mut reg = Registry::new();
        assert!(reg.is_stubbed(&SPORK[0]));
        reg.register(Box::new(Owner(&SPORK)));
        assert!(!reg.is_stubbed(&SPORK[0]));
        assert!(!reg.is_stubbed(&SPORK[1]));
        assert!(reg.is_stubbed(&CommandString::from_static("govobj")));
    }

    #[test]
    #[should_panic(expected = "already has a handler")]
    fn double_registration_panics() {
        let mut reg = Registry::new();
        reg.register(Box::new(Owner(&SPORK)));
        reg.register(Box::new(Owner(&SPORK[..1])));
    }

    struct Core(CoreHandler, &'static [CommandString]);

    impl NetHandler for Core {
        fn commands(&self) -> &'static [CommandString] {
            self.1
        }

        fn core_handler(&self) -> Option<CoreHandler> {
            Some(self.0)
        }

        fn process_message(
            &mut self,
            _cx: &mut HandlerCx<'_>,
            _cmd: &CommandString,
            _payload: &[u8],
        ) -> Result<(), Throw> {
            Ok(())
        }
    }

    const GOV: [CommandString; 1] = [CommandString::from_static("govobj")];
    const ISLOCK: [CommandString; 1] = [CommandString::from_static("isdlock")];
    const SSC: [CommandString; 1] = [CommandString::from_static("ssc")];

    #[test]
    fn core_handlers_register_in_cores_order() {
        assert!(CORE_HANDLER_ORDER.is_sorted());
        let mut reg = Registry::new();
        reg.register(Box::new(Core(CoreHandler::NetInstantSend, &ISLOCK)));
        // An unnamed handler (SP0's inline branch) may come anywhere.
        reg.register(Box::new(Owner(&SPORK)));
        reg.register(Box::new(Core(CoreHandler::NetGovernance, &GOV)));
        reg.register(Box::new(Core(CoreHandler::SyncManager, &SSC)));
    }

    #[test]
    #[should_panic(expected = "NetInstantSend registered after NetGovernance")]
    fn a_core_handler_out_of_order_panics() {
        let mut reg = Registry::new();
        reg.register(Box::new(Core(CoreHandler::NetGovernance, &GOV)));
        reg.register(Box::new(Core(CoreHandler::NetInstantSend, &ISLOCK)));
    }

    struct Ext(Extension, &'static [CommandString]);

    impl NetHandler for Ext {
        fn commands(&self) -> &'static [CommandString] {
            self.1
        }

        fn extension(&self) -> Option<Extension> {
            Some(self.0)
        }

        fn process_message(
            &mut self,
            _cx: &mut HandlerCx<'_>,
            _cmd: &CommandString,
            _payload: &[u8],
        ) -> Result<(), Throw> {
            Ok(())
        }
    }

    #[test]
    #[should_panic(expected = "MnAuth already has a handler")]
    fn a_second_handler_of_an_extension_panics() {
        const MNAUTH: [CommandString; 1] = [CommandString::from_static("mnauth")];
        let mut reg = Registry::new();
        reg.register(Box::new(Ext(Extension::MnAuth, &MNAUTH)));
        assert_eq!(reg.extension_count(), 1);
        reg.register(Box::new(Ext(Extension::MnAuth, &[])));
    }

    #[test]
    #[should_panic(expected = "handled inline")]
    fn registering_an_inline_command_panics() {
        const PING: [CommandString; 1] = [CommandString::from_static("ping")];
        Registry::new().register(Box::new(Owner(&PING)));
    }

    #[test]
    #[should_panic(expected = "not one of Core's commands")]
    fn registering_an_unlisted_command_panics() {
        const BOGUS: [CommandString; 1] = [CommandString::from_static("bogus")];
        Registry::new().register(Box::new(Owner(&BOGUS)));
    }
}
