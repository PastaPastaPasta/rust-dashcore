//! Node-side P2P handler reads.

use dash_consensus::primitives::{P2pCommitment, P2pMnListDiff, P2pQrInfo};
use dash_p2p_core::{
    Addr, AddrV2Entry, AddrV2Msg, BatchedSigShares, BatchedSigSharesMsg, BlockTxn, CFCheckpt,
    CFHeaders, CFilter, ClSig, CmpctBlock, CommandString, DkgComplaint, DkgContribution,
    DkgJustification, DkgPrematureCommitment, DsAccept, DsComplete, DsEntry, DsFinalTx, DsQueue,
    DsSignedInputs, DsStatusUpdate, DsTx, FilterAdd, FilterLoad, GetBlockTxn, GetBlocks,
    GetCFCheckpt, GetCFHeaders, GetCFilters, GetHeaders, GetHeaders2, GetMnListDiff, GetQrInfo,
    GovSync, Headers, Headers2, Inv, Inventory, IsdLock, MerkleBlock, MnAuth, MnListDiff,
    MnListFormat, NotFound, P2pDecodeError, P2pMsg, Ping, PlatformBan, Pong, ProtocolVersion,
    QData, QSendRecSigs, QrInfo, QuorumDataRequest, RecoveredSig, SendCmpct, SendDsq, SendTxRcncl,
    ServiceFlags, SigSesAnnMsg, SigShareMsg, SigSharesInvMsg, Spork, Ssc, TimestampedAddr, Version,
    VersionAddr,
};
use dash_p2p_transport::is_valid_v1;
use dash_pkc::bls::BlsCtx;
use dash_primitives::{
    Block, BlockHash, BlockHeader, Commitment, GovObject, GovVote, MerkleRoot, OutPoint,
    Transaction, TxHash, VoteOutcome, VoteSignal,
};
use dash_types::codec::{BaseCodec, DecodeError, read_bytes};
use dash_types::{CompactSize, Numeric};

/// Per-peer state consulted while decoding a message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PeerCtx {
    version: ProtocolVersion,
    bls: BlsCtx,
}

impl PeerCtx {
    pub const fn new(version: ProtocolVersion, bls: BlsCtx) -> Self {
        Self {
            version,
            bls,
        }
    }
    pub const fn version(&self) -> ProtocolVersion {
        self.version
    }
    pub const fn bls(&self) -> BlsCtx {
        self.bls
    }
}

/// A type read from a handler payload.
pub trait HandlerRead: Sized {
    fn read(data: &mut &[u8], ctx: &PeerCtx) -> Result<Self, ReadFail>;
}

impl<T: HandlerRead> HandlerRead for Box<T> {
    fn read(data: &mut &[u8], ctx: &PeerCtx) -> Result<Self, ReadFail> {
        T::read(data, ctx).map(Box::new)
    }
}

/// Implements a handler read using a type's public wire decoder.
#[macro_export]
macro_rules! default_read {
    ($($ty:ty),+ $(,)?) => { $(
        impl $crate::read::HandlerRead for $ty {
            fn read(data: &mut &[u8], _ctx: &$crate::read::PeerCtx) -> Result<Self, $crate::read::ReadFail> {
                <$ty as dash_types::codec::BaseCodec>::decode(data).map_err(|error| $crate::read::ReadFail::Decode(dash_p2p_core::P2pDecodeError::Consensus(error)))
            }
        }
    )+ };
}

default_read!(
    Ping,
    Pong,
    GovSync,
    GovObject,
    GetMnListDiff,
    SendDsq,
    QSendRecSigs,
    Block,
    Transaction,
    FilterLoad,
    FilterAdd,
    SendCmpct,
    CmpctBlock,
    GetBlockTxn,
    BlockTxn,
    GetCFilters,
    GetCFHeaders,
    GetCFCheckpt,
    MerkleBlock,
    CFilter,
    CFHeaders,
    CFCheckpt,
    SendTxRcncl,
    Ssc,
    Spork,
    RecoveredSig,
    ClSig,
    IsdLock,
    MnAuth,
    PlatformBan,
    QData,
    GetQrInfo,
    DsEntry,
    DsFinalTx,
    DsSignedInputs,
    DsComplete,
    DsStatusUpdate,
    DsQueue,
    DsTx,
);

impl HandlerRead for P2pCommitment {
    fn read(data: &mut &[u8], _: &PeerCtx) -> Result<Self, ReadFail> {
        consensus(Commitment::decode(data)).map(P2pCommitment::new)
    }
}

impl HandlerRead for Commitment {
    fn read(data: &mut &[u8], ctx: &PeerCtx) -> Result<Self, ReadFail> {
        P2pCommitment::read(data, ctx).map(P2pCommitment::into_commitment)
    }
}

impl HandlerRead for P2pMnListDiff {
    fn read(data: &mut &[u8], ctx: &PeerCtx) -> Result<Self, ReadFail> {
        consensus(MnListDiff::decode_as(data, MnListFormat::for_version(ctx.version())))
            .map(P2pMnListDiff::new)
    }
}

impl HandlerRead for MnListDiff {
    fn read(data: &mut &[u8], ctx: &PeerCtx) -> Result<Self, ReadFail> {
        P2pMnListDiff::read(data, ctx).map(P2pMnListDiff::into_diff)
    }
}

impl HandlerRead for P2pQrInfo {
    fn read(data: &mut &[u8], ctx: &PeerCtx) -> Result<Self, ReadFail> {
        consensus(QrInfo::decode_as(data, MnListFormat::for_version(ctx.version())))
            .map(P2pQrInfo::new)
    }
}

impl HandlerRead for QrInfo {
    fn read(data: &mut &[u8], ctx: &PeerCtx) -> Result<Self, ReadFail> {
        P2pQrInfo::read(data, ctx).map(P2pQrInfo::into_info)
    }
}

impl HandlerRead for GovVote {
    fn read(data: &mut &[u8], _: &PeerCtx) -> Result<Self, ReadFail> {
        let vote = GovVote::new(
            consensus(OutPoint::decode(data))?,
            consensus(TxHash::decode(data))?,
            consensus(VoteOutcome::decode(data))?,
            consensus(VoteSignal::decode(data))?,
            consensus(i64::decode(data))?,
            consensus(CompactSize::decode_len(data, 96))
                .and_then(|len| consensus(read_bytes(data, len)).map(|bytes| bytes.to_vec()))?,
        );
        if !matches!(vote.sig().len(), 65 | 96) {
            return Err(ReadFail::Decode(P2pDecodeError::InvalidValue {
                field: "governance vote signature size",
            }));
        }
        Ok(vote)
    }
}

impl HandlerRead for Version {
    fn read(data: &mut &[u8], _: &PeerCtx) -> Result<Self, ReadFail> {
        let original = *data;
        match Self::decode(data) {
            Ok(version) => Ok(version),
            Err(error) => {
                let mut head_data = original;
                let head = Version::new(
                    consensus(ProtocolVersion::decode(&mut head_data))?,
                    consensus(ServiceFlags::decode(&mut head_data))?,
                    consensus(i64::decode(&mut head_data))?,
                    consensus(VersionAddr::decode(&mut head_data))?,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                );
                Err(ReadFail::VersionTail {
                    head: Box::new(head),
                    error,
                })
            }
        }
    }
}

impl HandlerRead for DsAccept {
    fn read(data: &mut &[u8], ctx: &PeerCtx) -> Result<Self, ReadFail> {
        let denom = consensus(i32::decode(data))?;
        let collateral = consensus(Transaction::decode(data))?;
        let flags = if ctx.version() >= ProtocolVersion::COINJOIN_REBALANCE {
            Some(consensus(u8::decode(data))?)
        } else {
            None
        };
        Ok(DsAccept::new(denom, collateral, flags))
    }
}

fn consensus<T>(result: Result<T, DecodeError>) -> Result<T, ReadFail> {
    result.map_err(|error| ReadFail::Decode(P2pDecodeError::Consensus(error)))
}

macro_rules! list_read {
    ($ty:ty, $entry:ty, $ctor:path) => {
        impl HandlerRead for $ty {
            fn read(data: &mut &[u8], _: &PeerCtx) -> Result<Self, ReadFail> {
                let entries = consensus(Vec::<$entry>::decode(data))?;
                Ok($ctor(entries))
            }
        }
    };
}

list_read!(Addr, TimestampedAddr, Addr::new);
list_read!(AddrV2Msg, AddrV2Entry, AddrV2Msg::new);
list_read!(Inv, Inventory, Inv::new);
list_read!(dash_p2p_core::GetData, Inventory, dash_p2p_core::GetData::new);
list_read!(NotFound, Inventory, NotFound::new);

macro_rules! locator_read {
    ($ty:ty, $ctor:path) => {
        impl HandlerRead for $ty {
            fn read(data: &mut &[u8], _: &PeerCtx) -> Result<Self, ReadFail> {
                let version = consensus(ProtocolVersion::decode(data))?;
                let locator_hashes = consensus(Vec::<BlockHash>::decode(data))?;
                let hash_stop = consensus(BlockHash::decode(data))?;
                Ok($ctor(version, locator_hashes, hash_stop))
            }
        }
    };
}

locator_read!(GetHeaders, GetHeaders::new);
locator_read!(GetHeaders2, GetHeaders2::new);
locator_read!(GetBlocks, GetBlocks::new);

fn count_value(data: &[u8]) -> Result<u64, ReadFail> {
    let mut peek = data;
    Ok(consensus(CompactSize::decode(&mut peek))?.to_base())
}

fn checked_count(data: &[u8], max: usize, command: &'static str) -> Result<u64, ReadFail> {
    let count = count_value(data)?;
    if count > max as u64 {
        return Err(ReadFail::TooManyEntries {
            command,
            count,
            max: max as u64,
        });
    }
    Ok(count)
}

fn count_fits_remaining(data: &[u8], count: u64) -> Result<(), ReadFail> {
    let mut peek = data;
    let _ = consensus(CompactSize::decode(&mut peek))?;
    if count > peek.len() as u64 {
        return Err(ReadFail::Decode(P2pDecodeError::Consensus(
            DecodeError::CompactSizeExceedsLimit {
                limit: peek.len(),
                value: count,
            },
        )));
    }
    Ok(())
}

fn headers2_wire_error(data: &[u8], count: u64) -> Option<DecodeError> {
    let mut cursor = data;
    macro_rules! decode {
        ($ty:ty) => {
            match <$ty>::decode(&mut cursor) {
                Ok(value) => value,
                Err(error) => return Some(error),
            }
        };
    }
    for _ in 0..count {
        let flags = decode!(u8);
        if flags & 0x07 == 0 {
            decode!(i32);
        }
        if flags & 0x08 != 0 {
            decode!(BlockHash);
        }
        decode!(MerkleRoot);
        if flags & 0x10 != 0 {
            decode!(u32);
        } else {
            decode!(i16);
        }
        if flags & 0x20 != 0 {
            decode!(u32);
        }
        decode!(u32);
    }
    None
}

impl HandlerRead for Headers {
    fn read(data: &mut &[u8], _: &PeerCtx) -> Result<Self, ReadFail> {
        let count = checked_count(data, crate::limits::MAX_HEADERS_UNCOMPRESSED_RESULT, "headers")?;
        count_fits_remaining(data, count)?;
        consensus(CompactSize::decode(data))?;
        let mut headers = Vec::with_capacity(count as usize);
        for _ in 0..count {
            headers.push(BlockHeader::new(
                consensus(i32::decode(data))?,
                consensus(BlockHash::decode(data))?,
                consensus(MerkleRoot::decode(data))?,
                consensus(u32::decode(data))?,
                consensus(u32::decode(data))?,
                consensus(u32::decode(data))?,
            ));
            consensus(CompactSize::decode(data))?;
        }
        Ok(Self::new(headers))
    }
}

impl HandlerRead for Headers2 {
    fn read(data: &mut &[u8], ctx: &PeerCtx) -> Result<Self, ReadFail> {
        let count = checked_count(data, Self::max_count(ctx.version()), "headers2")?;
        count_fits_remaining(data, count)?;
        let mut after_count = *data;
        consensus(CompactSize::decode(&mut after_count))?;
        let remaining = after_count;
        if let Some(error) = headers2_wire_error(remaining, count) {
            return Err(ReadFail::Decode(P2pDecodeError::Consensus(error)));
        }
        consensus(Self::decode(data))
    }
}

macro_rules! counted_read {
    ($ty:ty, $max:expr, $command:expr) => {
        impl HandlerRead for $ty {
            fn read(data: &mut &[u8], _: &PeerCtx) -> Result<Self, ReadFail> {
                let count = checked_count(data, $max, $command)?;
                count_fits_remaining(data, count)?;
                consensus(Self::decode(data))
            }
        }
    };
}

counted_read!(SigSesAnnMsg, dash_p2p_core::MAX_SIG_SES_ANNS, "qsigsesann");
counted_read!(SigSharesInvMsg, dash_p2p_core::MAX_SIG_SHARES_INVS, "qsigsinv/qgetsigs");
counted_read!(SigShareMsg, dash_p2p_core::MAX_SIG_SHARES, "qsigshare");

impl HandlerRead for BatchedSigSharesMsg {
    fn read(data: &mut &[u8], _: &PeerCtx) -> Result<Self, ReadFail> {
        let count = checked_count(data, dash_p2p_core::MAX_BATCHED_SIGS, "qbsigs")?;
        count_fits_remaining(data, count)?;
        consensus(CompactSize::decode(data))?;
        let mut batches = Vec::with_capacity(count as usize);
        let mut shares = 0usize;
        for _ in 0..count {
            let batch = consensus(BatchedSigShares::decode(data))?;
            shares += batch.sig_shares().len();
            if shares > dash_p2p_core::MAX_BATCHED_SIGS {
                return Err(ReadFail::TooManyEntries {
                    command: "qbsigs",
                    count: shares as u64,
                    max: dash_p2p_core::MAX_BATCHED_SIGS as u64,
                });
            }
            batches.push(batch);
        }
        Ok(BatchedSigSharesMsg::new(batches))
    }
}

impl HandlerRead for QuorumDataRequest {
    fn read(data: &mut &[u8], _: &PeerCtx) -> Result<Self, ReadFail> {
        if data.len() > crate::limits::MAX_QGETDATA_SIZE {
            return Err(ReadFail::HandlerPayloadTooLarge {
                command: "qgetdata",
                size: data.len(),
                max: crate::limits::MAX_QGETDATA_SIZE,
            });
        }
        consensus(Self::decode(data))
    }
}

macro_rules! dkg_read {
    ($ty:ty, $command:expr) => {
        impl HandlerRead for $ty {
            fn read(data: &mut &[u8], _: &PeerCtx) -> Result<Self, ReadFail> {
                if data.len() > crate::limits::MAX_DKG_PAYLOAD_SIZE {
                    return Err(ReadFail::HandlerPayloadTooLarge {
                        command: $command,
                        size: data.len(),
                        max: crate::limits::MAX_DKG_PAYLOAD_SIZE,
                    });
                }
                let value = consensus(Self::decode(data))?;
                if !data.is_empty() {
                    return Err(ReadFail::Decode(P2pDecodeError::Consensus(
                        DecodeError::TrailingBytes {
                            remaining: data.len(),
                        },
                    )));
                }
                Ok(value)
            }
        }
    };
}

dkg_read!(DkgContribution, "qcontrib");
dkg_read!(DkgComplaint, "qcomplaint");
dkg_read!(DkgJustification, "qjustify");
dkg_read!(DkgPrematureCommitment, "qpcommit");

impl HandlerRead for AddrV2Entry {
    fn read(data: &mut &[u8], _: &PeerCtx) -> Result<Self, ReadFail> {
        consensus(Self::decode(data))
    }
}

impl HandlerRead for TimestampedAddr {
    fn read(data: &mut &[u8], _: &PeerCtx) -> Result<Self, ReadFail> {
        consensus(Self::decode(data))
    }
}

/// A decoded message and the bytes consumed by its handler.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Decoded {
    pub msg: Msg,
    pub consumed: usize,
}

/// A known typed message or an unknown command preserved verbatim.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum Msg {
    Known(P2pMsg),
    Unknown {
        command: CommandString,
        payload: Vec<u8>,
    },
}

/// A handler read failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadFail {
    Decode(P2pDecodeError),
    TooManyEntries {
        command: &'static str,
        count: u64,
        max: u64,
    },
    HandlerPayloadTooLarge {
        command: &'static str,
        size: usize,
        max: usize,
    },
    VersionTail {
        head: Box<dash_p2p_core::Version>,
        error: dash_types::codec::DecodeError,
    },
}

impl core::fmt::Display for ReadFail {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Decode(e) => e.fmt(f),
            Self::TooManyEntries {
                command,
                count,
                max,
            } => write!(f, "{command} lists {count} entries, max {max}"),
            Self::HandlerPayloadTooLarge {
                command,
                size,
                max,
            } => write!(f, "{command} payload over its handler's limit: {size} bytes, max {max}"),
            Self::VersionTail {
                error,
                ..
            } => write!(f, "version after addr_recv: {error}"),
        }
    }
}

impl std::error::Error for ReadFail {}

/// Decode a V1 command using the node's handler policy.
pub fn decode(cmd: &CommandString, payload: &[u8], ctx: &PeerCtx) -> Result<Decoded, ReadFail> {
    if payload.len() > crate::limits::MAX_PROTOCOL_MESSAGE_LENGTH {
        return Err(ReadFail::Decode(P2pDecodeError::PayloadTooLarge {
            command: *cmd,
            size: payload.len(),
            max: crate::limits::MAX_PROTOCOL_MESSAGE_LENGTH,
        }));
    }
    if !is_valid_v1(cmd) {
        return Err(ReadFail::Decode(P2pDecodeError::InvalidCommand {
            command: *cmd,
        }));
    }
    let mut cursor = payload;
    let msg = match *cmd {
        CommandString::VERSION => P2pMsg::Version(Version::read(&mut cursor, ctx)?),
        CommandString::PING => P2pMsg::Ping(Ping::read(&mut cursor, ctx)?),
        CommandString::PONG => P2pMsg::Pong(Pong::read(&mut cursor, ctx)?),
        CommandString::ADDR => P2pMsg::Addr(Addr::read(&mut cursor, ctx)?),
        CommandString::ADDRV2 => P2pMsg::AddrV2(AddrV2Msg::read(&mut cursor, ctx)?),
        CommandString::INV => P2pMsg::Inv(Inv::read(&mut cursor, ctx)?),
        CommandString::GETDATA => P2pMsg::GetData(dash_p2p_core::GetData::read(&mut cursor, ctx)?),
        CommandString::NOTFOUND => P2pMsg::NotFound(NotFound::read(&mut cursor, ctx)?),
        CommandString::GETHEADERS => P2pMsg::GetHeaders(GetHeaders::read(&mut cursor, ctx)?),
        CommandString::HEADERS => P2pMsg::Headers(Headers::read(&mut cursor, ctx)?),
        CommandString::GETHEADERS2 => P2pMsg::GetHeaders2(GetHeaders2::read(&mut cursor, ctx)?),
        CommandString::HEADERS2 => P2pMsg::Headers2(Headers2::read(&mut cursor, ctx)?),
        CommandString::GOVSYNC => P2pMsg::GovSync(GovSync::read(&mut cursor, ctx)?),
        CommandString::GOVOBJ => P2pMsg::GovObj(GovObject::read(&mut cursor, ctx)?),
        CommandString::GOVOBJVOTE => P2pMsg::GovObjVote(GovVote::read(&mut cursor, ctx)?),
        CommandString::GETMNLISTD => P2pMsg::GetMnListDiff(GetMnListDiff::read(&mut cursor, ctx)?),
        CommandString::MNLISTDIFF => P2pMsg::MnListDiff(MnListDiff::read(&mut cursor, ctx)?),
        CommandString::SENDDSQ => P2pMsg::SendDsq(SendDsq::read(&mut cursor, ctx)?),
        CommandString::QSENDRECSIGS => P2pMsg::QSendRecSigs(QSendRecSigs::read(&mut cursor, ctx)?),
        CommandString::BLOCK => P2pMsg::Block(Block::read(&mut cursor, ctx)?),
        CommandString::TX => P2pMsg::Tx(Transaction::read(&mut cursor, ctx)?),
        CommandString::GETBLOCKS => P2pMsg::GetBlocks(GetBlocks::read(&mut cursor, ctx)?),
        CommandString::FILTERLOAD => P2pMsg::FilterLoad(FilterLoad::read(&mut cursor, ctx)?),
        CommandString::FILTERADD => P2pMsg::FilterAdd(FilterAdd::read(&mut cursor, ctx)?),
        CommandString::SENDCMPCT => P2pMsg::SendCmpct(SendCmpct::read(&mut cursor, ctx)?),
        CommandString::CMPCTBLOCK => P2pMsg::CmpctBlock(CmpctBlock::read(&mut cursor, ctx)?),
        CommandString::GETBLOCKTXN => P2pMsg::GetBlockTxn(GetBlockTxn::read(&mut cursor, ctx)?),
        CommandString::BLOCKTXN => P2pMsg::BlockTxn(BlockTxn::read(&mut cursor, ctx)?),
        CommandString::GETCFILTERS => P2pMsg::GetCFilters(GetCFilters::read(&mut cursor, ctx)?),
        CommandString::GETCFHEADERS => P2pMsg::GetCFHeaders(GetCFHeaders::read(&mut cursor, ctx)?),
        CommandString::GETCFCHECKPT => P2pMsg::GetCFCheckpt(GetCFCheckpt::read(&mut cursor, ctx)?),
        CommandString::MERKLEBLOCK => P2pMsg::MerkleBlock(MerkleBlock::read(&mut cursor, ctx)?),
        CommandString::CFILTER => P2pMsg::CFilter(CFilter::read(&mut cursor, ctx)?),
        CommandString::CFHEADERS => P2pMsg::CFHeaders(CFHeaders::read(&mut cursor, ctx)?),
        CommandString::CFCHECKPT => P2pMsg::CFCheckpt(CFCheckpt::read(&mut cursor, ctx)?),
        CommandString::SENDTXRCNCL => P2pMsg::SendTxRcncl(SendTxRcncl::read(&mut cursor, ctx)?),
        CommandString::SSC => P2pMsg::Ssc(Ssc::read(&mut cursor, ctx)?),
        CommandString::SPORK => P2pMsg::Spork(Spork::read(&mut cursor, ctx)?),
        CommandString::QFCOMMIT => P2pMsg::QfCommit(Box::<Commitment>::read(&mut cursor, ctx)?),
        CommandString::QSIGREC => P2pMsg::QSigRec(RecoveredSig::read(&mut cursor, ctx)?),
        CommandString::QGETDATA => P2pMsg::QGetData(QuorumDataRequest::read(&mut cursor, ctx)?),
        CommandString::QDATA => P2pMsg::QData(QData::read(&mut cursor, ctx)?),
        CommandString::CLSIG => P2pMsg::ClSig(ClSig::read(&mut cursor, ctx)?),
        CommandString::ISDLOCK => P2pMsg::IsdLock(IsdLock::read(&mut cursor, ctx)?),
        CommandString::MNAUTH => P2pMsg::MnAuth(MnAuth::read(&mut cursor, ctx)?),
        CommandString::GETQRINFO => P2pMsg::GetQrInfo(GetQrInfo::read(&mut cursor, ctx)?),
        CommandString::QRINFO => P2pMsg::QrInfo(Box::<QrInfo>::read(&mut cursor, ctx)?),
        CommandString::PLATFORMBAN => P2pMsg::PlatformBan(PlatformBan::read(&mut cursor, ctx)?),
        CommandString::QCONTRIB => {
            P2pMsg::QContrib(Box::<DkgContribution>::read(&mut cursor, ctx)?)
        }
        CommandString::QCOMPLAINT => P2pMsg::QComplaint(DkgComplaint::read(&mut cursor, ctx)?),
        CommandString::QJUSTIFY => P2pMsg::QJustify(DkgJustification::read(&mut cursor, ctx)?),
        CommandString::QPCOMMIT => {
            P2pMsg::QpCommit(DkgPrematureCommitment::read(&mut cursor, ctx)?)
        }
        CommandString::QSIGSESANN => P2pMsg::QSigSesAnn(SigSesAnnMsg::read(&mut cursor, ctx)?),
        CommandString::QSIGSINV => P2pMsg::QSigsInv(SigSharesInvMsg::read(&mut cursor, ctx)?),
        CommandString::QGETSIGS => P2pMsg::QGetSigs(SigSharesInvMsg::read(&mut cursor, ctx)?),
        CommandString::QBSIGS => P2pMsg::QbSigs(BatchedSigSharesMsg::read(&mut cursor, ctx)?),
        CommandString::QSIGSHARE => P2pMsg::QSigShare(SigShareMsg::read(&mut cursor, ctx)?),
        CommandString::DSA => P2pMsg::Dsa(DsAccept::read(&mut cursor, ctx)?),
        CommandString::DSI => P2pMsg::Dsi(DsEntry::read(&mut cursor, ctx)?),
        CommandString::DSF => P2pMsg::Dsf(DsFinalTx::read(&mut cursor, ctx)?),
        CommandString::DSS => P2pMsg::Dss(DsSignedInputs::read(&mut cursor, ctx)?),
        CommandString::DSC => P2pMsg::Dsc(DsComplete::read(&mut cursor, ctx)?),
        CommandString::DSSU => P2pMsg::Dssu(DsStatusUpdate::read(&mut cursor, ctx)?),
        CommandString::DSTX => P2pMsg::Dstx(Box::<DsTx>::read(&mut cursor, ctx)?),
        CommandString::DSQ => P2pMsg::Dsq(DsQueue::read(&mut cursor, ctx)?),
        CommandString::VERACK => P2pMsg::Verack,
        CommandString::GETADDR => P2pMsg::GetAddr,
        CommandString::SENDADDRV2 => P2pMsg::SendAddrV2,
        CommandString::SENDHEADERS => P2pMsg::SendHeaders,
        CommandString::SENDHEADERS2 => P2pMsg::SendHeaders2,
        CommandString::FILTERCLEAR => P2pMsg::FilterClear,
        CommandString::MEMPOOL => P2pMsg::Mempool,
        CommandString::QWATCH => P2pMsg::QWatch,
        CommandString::GETSPORKS => P2pMsg::GetSporks,
        _ => {
            return Ok(Decoded {
                msg: Msg::Unknown {
                    command: *cmd,
                    payload: payload.to_vec(),
                },
                consumed: payload.len(),
            });
        }
    };
    let consumed = payload.len() - cursor.len();
    Ok(Decoded {
        msg: Msg::Known(msg),
        consumed,
    })
}
